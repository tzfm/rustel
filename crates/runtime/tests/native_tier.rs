//! Every score the native tier evaluates plays as the JavaScript tier plays
//! it: the same tempo, and the same haps for whole cycles, many cycles and
//! playback-sized windows.
//!
//! The two tiers also build the same graph, with two exceptions. The
//! JavaScript tier's final rewrite for worklet events is not part of the
//! graph compared, as no native graph makes such an event. And a JavaScript
//! graph that holds a callback applies it at each query, where the native
//! graph holds the transformer's result: those two agree in what they play.

use rustel_fraction::Fraction;
use rustel_runtime::{Session, SessionConfig};
use std::path::{Path, PathBuf};

fn scores_under(folder: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scores_under(&path, out);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "strudel")
        {
            out.push(path);
        }
    }
}

fn corpus() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut scores = Vec::new();
    scores_under(&root.join("tests/e2e/scores"), &mut scores);
    scores_under(&root.join("examples/songs"), &mut scores);
    scores.sort();
    scores
}

/// The spans a comparison queries: one cycle, sixteen, and sixty-four
/// one-eighth-cycle windows in a row.
fn spans() -> Vec<(Fraction, Fraction)> {
    let mut spans = vec![
        (Fraction::ZERO, Fraction::ONE),
        (Fraction::ZERO, Fraction::new(16, 1)),
    ];
    spans.extend((0..64).map(|window| (Fraction::new(window, 8), Fraction::new(window + 1, 8))));
    spans
}

/// What a session plays over each span: each hap, and the widgets it
/// draws in.
fn answers(session: &Session) -> Vec<String> {
    spans()
        .into_iter()
        .map(|(begin, end)| {
            let haps = session.query(begin, end);
            let widgets = haps.as_ref().map(|haps| {
                let widgets = haps.iter().map(rustel_core::Hap::ui_visuals_context);
                widgets.collect::<Vec<_>>()
            });
            format!("{haps:?} {widgets:?}")
        })
        .collect()
}

/// Whether the native tier evaluates `source`. A score that it evaluates
/// builds and plays as the JavaScript tier builds and plays it.
fn plays_as_javascript_plays_it(name: &str, source: &str) -> bool {
    // One session builds both, since a graph carries its session's
    // runtime settings.
    let mut session = Session::with_config(SessionConfig::default()).expect("a session");
    if session.evaluate(source).is_err() || !session.evaluated_natively() {
        return false;
    }
    let built = session.active_pattern_for_test().expect("a native graph");
    let (cps, played) = (session.cps(), answers(&session));
    session.set_native_tier(false);
    session
        .evaluate(source)
        .unwrap_or_else(|error| panic!("{name}: JavaScript refused: {error}"));
    assert!(!session.evaluated_natively());
    let scripted = session.active_pattern_for_test().expect("a script graph");
    let scripted = scripted.without_final_controls();
    assert!(
        built.same_construction(scripted) || !scripted.reachable_callbacks().is_empty(),
        "{name}: the tiers built different graphs"
    );
    assert_eq!(cps, session.cps(), "{name}");
    assert_eq!(played, answers(&session), "{name}");
    true
}

#[test]
fn native_scores_play_as_javascript_plays_them() {
    let native = corpus()
        .into_iter()
        .filter(|path| {
            let source = std::fs::read_to_string(path).expect("read a corpus score");
            plays_as_javascript_plays_it(&path.display().to_string(), &source)
        })
        .count();
    assert!(native >= 840, "only {native} corpus scores ran natively");
}

#[test]
fn a_method_of_numbers_and_patterns_plays_as_javascript_plays_it() {
    let scores = [
        "$: s(\"bd*8\").fast(2).slow(4).ply(2)",
        // A number and a pattern, after a call of one number.
        "$: s(\"bd\").fast(2).euclid(3, \"<8 16>\")",
        "$: s(\"hh*4\").gain(sine.range(0.25, 0.75).slow(2)).fast(2)",
    ];
    for score in scores {
        assert!(plays_as_javascript_plays_it(score, score), "{score}");
    }
}

#[test]
fn an_arrow_of_controls_plays_as_javascript_plays_it() {
    let scores = [
        // Constants in a row, one control two times: the last one stays.
        "$: note(\"c e g\").layer(x => x.s(\"sawtooth\").gain(0.5).lpf(800).gain(0.25))",
        // A pattern between constants, and a second arrow.
        "$: note(\"c e g\").layer(\n  x => x.s(\"square\").lpf(\"<400 800>\").room(0.2),\n  x => x.add(7).gain(0.5).pan(sine))",
        "$: s(\"bd*4\").every(2, x => x.gain(0.5).speed(2))",
    ];
    for score in scores {
        assert!(plays_as_javascript_plays_it(score, score), "{score}");
    }
}

#[test]
fn a_distortion_shortcut_plays_as_javascript_plays_it() {
    let scores = [
        "$: note(\"d1 e1\").s(\"saw\").diode(0.9).lpf(800)",
        "$: s(\"bd*4\").fold(\"<2 4>\", 0.5)",
        "$: s(\"hh*8\").soft(\"3:0.4\").gain(0.8)",
        "$: s(\"bd*2\").every(2, x => x.hard(sine.range(1, 4)))",
    ];
    for score in scores {
        assert!(plays_as_javascript_plays_it(score, score), "{score}");
    }
}

#[test]
fn the_script_engine_starts_at_the_first_score_that_needs_it() {
    let mut session = Session::with_config(SessionConfig::default()).expect("a session");
    let native = "$: note(\"c e g a\").s(\"sawtooth\").every(2, x => x.fast(2))";
    for _ in 0..3 {
        session.evaluate(native).expect("a native score");
        assert!(session.evaluated_natively());
        let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("haps");
        assert_eq!(haps.len(), 8);
    }
    assert!(!session.script_engine_started());
    // A loop is JavaScript alone.
    let script =
        "let lanes = []; for (const name of ['bd', 'hh']) { lanes.push(s(name)) } stack(...lanes)";
    session.evaluate(script).expect("a script score");
    assert!(!session.evaluated_natively());
    assert!(session.script_engine_started());
    // The native tier takes the next native score, in the session that
    // started the engine.
    session.evaluate(native).expect("a native score");
    assert!(session.evaluated_natively());
}

/// A session says what evaluated its score, and the line where a score
/// leaves the native route: where the reader stops, or the call that the
/// native tier does not take.
#[test]
fn a_session_says_what_evaluated_its_score() {
    use rustel_runtime::ScoreEngine;
    let mut session = Session::with_config(SessionConfig::default()).expect("a session");
    assert_eq!(session.score_engine(), None);
    let cases = [
        ("$: s(\"bd\")", ScoreEngine::Native),
        // An index is a form of JavaScript alone.
        (
            "$: s(\"bd\")\n$: s(\"hh\")\n  .gain([0.5][0])",
            ScoreEngine::Script { line: Some(3) },
        ),
        // The reader takes the call, and the native tier has no such method.
        (
            "$: s(\"bd\")\n\n$: note(\"<[c e g]>\")\n  .arp(\"0 1 2\")\n  .s(\"sawtooth\")",
            ScoreEngine::Script { line: Some(4) },
        ),
        ("$: s(\"bd\").gain(0.5)", ScoreEngine::Native),
        ("$: s(\"bd*8\")\n  .fast(2)", ScoreEngine::Native),
        // A method of one input makes a sequence of two arguments.
        (
            "$: s(\"bd*8\")\n  .fast(2, 4)",
            ScoreEngine::Script { line: Some(2) },
        ),
        // A control of a chain takes no array.
        (
            "$: s(\"bd\")\n  .gain(0.5)\n  .pan([0.25])\n  .room(0.2)\n  .lpf(800)",
            ScoreEngine::Script { line: Some(3) },
        ),
    ];
    for (score, engine) in cases {
        session.evaluate(score).expect("the score evaluates");
        assert_eq!(session.score_engine(), Some(engine), "{score}");
    }
}

/// What a session built of the score it evaluated last: its graph, its
/// tempo and what it plays.
fn built(session: &Session) -> (rustel_core::Pattern, f64, Vec<String>) {
    let graph = session.active_pattern_for_test().expect("a graph");
    (graph, session.cps(), answers(session))
}

/// `source` with its last digit changed, as a live coder changes one value.
/// `None` for a text with no digit.
fn edited(source: &str) -> Option<String> {
    let at = source.rfind(|character: char| character.is_ascii_digit())?;
    let digit = source.as_bytes()[at] - b'0';
    let mut text = source.to_owned();
    text.replace_range(at..=at, &((digit + 1) % 10).to_string());
    Some(text)
}

/// Each text of `texts` in turn, in one session, builds what a new session
/// builds of that text alone: a build takes parts of the build before it
/// only where they are what it would build.
fn assert_builds_as_a_new_session(name: &str, texts: &[&str]) {
    let mut session = Session::with_config(SessionConfig::default()).expect("a session");
    for (turn, text) in texts.iter().enumerate() {
        let mut alone = Session::with_config(SessionConfig::default()).expect("a session");
        let (evaluated, expected) = (session.evaluate(text), alone.evaluate(text));
        assert_eq!(
            evaluated.is_ok(),
            expected.is_ok(),
            "{name}: text {turn}: {text}"
        );
        if expected.is_err() {
            continue;
        }
        assert_eq!(
            session.evaluated_natively(),
            alone.evaluated_natively(),
            "{name}: text {turn}: {text}"
        );
        // The script engine numbers what it makes from the first score of
        // a session on, and it keeps no part.
        if !alone.evaluated_natively() {
            continue;
        }
        let ((graph, cps, played), (new_graph, new_cps, new_played)) =
            (built(&session), built(&alone));
        // Each build of an arrow makes a transformer of its own, so two new
        // sessions can build two graphs of one text.
        let same_in_each_session = || {
            let mut other = Session::with_config(SessionConfig::default()).expect("a session");
            other.evaluate(text).expect("the text evaluates");
            new_graph.same_construction(&built(&other).0)
        };
        assert!(
            graph.same_construction(&new_graph) || !same_in_each_session(),
            "{name}: text {turn} built another graph: {text}"
        );
        assert_eq!(cps, new_cps, "{name}: text {turn}: {text}");
        assert_eq!(played, new_played, "{name}: text {turn}: {text}");
    }
}

#[test]
fn a_score_evaluated_again_keeps_its_graph() {
    let score =
        "stack(\n  s(\"bd*4\").gain(0.9),\n  note(\"<c2 eb2>\").s(\"sawtooth\").lpf(400)\n)";
    let mut session = Session::with_config(SessionConfig::default()).expect("a session");
    session.evaluate(score).expect("a native score");
    let first = session.active_pattern_for_test().expect("a graph");
    session.evaluate(score).expect("a native score");
    let again = session.active_pattern_for_test().expect("a graph");
    assert!(first.shares_node(&again));
    // An edit builds another graph.
    session
        .evaluate(&edited(score).expect("a digit"))
        .expect("a native score");
    let changed = session.active_pattern_for_test().expect("a graph");
    assert!(!first.shares_node(&changed));
}

/// A caller can refuse an evaluation, and its effects with it. So each
/// evaluation gives the effects of its score, though its build takes each
/// part of the build before it.
#[test]
fn a_score_evaluated_again_sets_its_tempo_again() {
    // A score that is its tempo alone plays what the tempo call gives, so
    // its graph holds the pattern of that call.
    for source in ["setcpm(30)\n$: s(\"bd*2\").gain(0.5)", "setcpm(30)"] {
        let runtime = rustel_jsruntime::JsRuntime::deferred(None);
        for turn in 0..3 {
            let read = rustel_score::read(source, rustel_transpiler::call_form).expect("a score");
            let (_, effects, _) = runtime
                .evaluate_score_candidate_with_effects_cancellable(
                    source,
                    Some(&read.program),
                    &rustel_transpiler::TranspileOptions::default(),
                    std::time::Duration::from_secs(5),
                    &std::sync::atomic::AtomicBool::new(false),
                )
                .expect("a score");
            assert!(runtime.evaluated_natively());
            assert_eq!(effects.cps, Some(0.5), "{source}: evaluation {turn}");
        }
    }
}

#[test]
fn an_edit_builds_what_a_new_session_builds() {
    let cases: &[(&str, &[&str])] = &[
        (
            "a value of one argument of a stack",
            &[
                "stack(s(\"bd*4\").gain(0.9), s(\"hh*8\").gain(0.4))",
                "stack(s(\"bd*4\").gain(0.9), s(\"hh*8\").gain(0.5))",
                "stack(s(\"bd*4\").gain(0.7), s(\"hh*8\").gain(0.5))",
                "stack(s(\"bd*4\").gain(0.9), s(\"hh*8\").gain(0.4))",
            ],
        ),
        (
            "a binding that a lane reads",
            &[
                "const drums = s(\"bd sd\")\n$: drums.gain(0.5)\n$: drums.fast(2)",
                "const drums = s(\"hh cp\")\n$: drums.gain(0.5)\n$: drums.fast(2)",
                "const drums = s(\"hh cp\")\n$: drums.gain(0.6)\n$: drums.fast(2)",
            ],
        ),
        (
            "a function that another function calls",
            &[
                "function kick() { return s(\"bd*2\") }\nfunction drums() { return stack(kick(), s(\"hh*4\")) }\n$: drums().gain(0.8)\n$: kick().fast(2)",
                "function kick() { return s(\"bd*4\") }\nfunction drums() { return stack(kick(), s(\"hh*4\")) }\n$: drums().gain(0.8)\n$: kick().fast(2)",
                "function kick() { return s(\"bd*4\") }\nfunction drums() { return stack(kick(), s(\"hh*8\")) }\n$: drums().gain(0.8)\n$: kick().fast(2)",
            ],
        ),
        (
            "a function with a parameter, called two times",
            &[
                "function lead(notes) { return stack(note(notes).s(\"sawtooth\"), s(\"hh*2\")) }\n$: lead(\"c e\")\n$: lead(\"g a\").gain(0.5)",
                "function lead(notes) { return stack(note(notes).s(\"sawtooth\"), s(\"hh*2\")) }\n$: lead(\"c f\")\n$: lead(\"g a\").gain(0.5)",
                "function lead(notes) { return stack(note(notes).s(\"triangle\"), s(\"hh*2\")) }\n$: lead(\"c f\")\n$: lead(\"g a\").gain(0.5)",
            ],
        ),
        (
            "a function with a parameter, whose stack holds its arguments",
            &[
                "function lead(notes) { return stack(note(notes).degradeBy(0.5), s(\"hh*2\").degradeBy(0.3)) }\n$: lead(\"c e\")\n$: lead(\"g a\").gain(0.5)",
                "function lead(notes) { return stack(note(notes).degradeBy(0.5), s(\"hh*2\").degradeBy(0.3)) }\n$: lead(\"c f\")\n$: lead(\"g a\").gain(0.5)",
                "function lead(notes) { return stack(note(notes).degradeBy(0.5), s(\"hh*2\").degradeBy(0.3)) }\n$: lead(\"c f\")\n$: lead(\"g b\").gain(0.5)",
            ],
        ),
        (
            "a parameter with the name of a binding",
            &[
                "const part = s(\"bd\")\nfunction twice(part) { return stack(part.fast(2), part) }\n$: twice(s(\"hh\"))\n$: part",
                "const part = s(\"sd\")\nfunction twice(part) { return stack(part.fast(2), part) }\n$: twice(s(\"hh\"))\n$: part",
                "const part = s(\"sd\")\nfunction twice(part) { return stack(part.fast(2), part) }\n$: twice(s(\"cp\"))\n$: part",
            ],
        ),
        (
            "a property of an object that a pick reads",
            &[
                "const parts = { a: s(\"bd*2\"), b: s(\"hh*4\").gain(0.5) }\n$: \"<a b>\".pick(parts)",
                "const parts = { a: s(\"bd*2\"), b: s(\"hh*4\").gain(0.7) }\n$: \"<a b>\".pick(parts)",
                "const parts = { a: s(\"sd*2\"), b: s(\"hh*4\").gain(0.7) }\n$: \"<b a>\".pick(parts)",
            ],
        ),
        (
            "the body of an arrow",
            &[
                "$: note(\"c e g\").layer(x => x.add(12), x => x.s(\"sawtooth\").gain(0.5))",
                "$: note(\"c e g\").layer(x => x.add(19), x => x.s(\"sawtooth\").gain(0.5))",
                "$: note(\"c e g\").layer(x => x.add(19), x => x.s(\"sawtooth\").gain(0.2))",
            ],
        ),
        (
            "a widget before a widget, in a text of the same length",
            &[
                "$: s(\"bd*2\").gain(1)\n$: s(\"hh*4\").scope()",
                "$: s(\"bd*2\").scope()\n$: s(\"hh*4\").scope()",
                "$: s(\"bd*2\").gain(1)\n$: s(\"hh*4\").scope()",
            ],
        ),
        (
            "lanes that change places and marks",
            &[
                "$: s(\"bd*2\")\n$: s(\"hh*4\")",
                "$: s(\"hh*4\")\n$: s(\"bd*2\")",
                "_$: s(\"hh*4\")\n$: s(\"bd*2\")",
                "$: s(\"hh*4\")\n$: s(\"bd*2\")",
            ],
        ),
        (
            "a tempo and a statement above the others",
            &[
                "setcpm(30)\n$: s(\"bd*2\").gain(0.5)",
                "setcpm(60)\n$: s(\"bd*2\").gain(0.5)",
                "setcpm(60)\nconst more = s(\"cp\")\n$: s(\"bd*2\").gain(0.5)\n$: more",
                "setcpm(60)\n$: s(\"bd*2\").gain(0.5)",
            ],
        ),
        (
            "a string that is no mini-notation, in a text of the same length",
            &[
                "// mini-on \n$: s(\"bd*2\").gain(0.5)",
                "// mini-off\n$: s(\"bd*2\").gain(0.5)",
                "// mini-on \n$: s(\"bd*2\").gain(0.5)",
            ],
        ),
        (
            "a score that needs the script engine, between two that do not",
            &[
                "$: s(\"bd*2\").gain(0.5)",
                "$: s(\"bd*2\").gain([0.5][0])",
                "$: s(\"bd*2\").gain(0.5)",
                "$: s(\"bd*2\").gain(0.6)",
            ],
        ),
    ];
    for (name, texts) in cases {
        assert_builds_as_a_new_session(name, texts);
    }
}

#[test]
fn an_edit_of_each_corpus_score_builds_what_a_new_session_builds() {
    let mut edits = 0;
    for path in corpus() {
        let source = std::fs::read_to_string(&path).expect("read a corpus score");
        let Some(changed) = edited(&source) else {
            continue;
        };
        edits += 1;
        let name = path.display().to_string();
        assert_builds_as_a_new_session(&name, &[&source, &changed, &source, &source]);
    }
    assert!(edits > 1000, "only {edits} corpus scores have a digit");
}
