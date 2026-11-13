//! Every score the native tier evaluates plays as the JavaScript tier plays
//! it: the same graph, the same tempo, and the same haps for whole cycles,
//! many cycles and playback-sized windows. The JavaScript tier's final
//! rewrite for worklet events is not part of the graph compared, as no
//! native graph makes such an event.

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

fn answers(session: &Session) -> Vec<String> {
    spans()
        .into_iter()
        .map(|(begin, end)| format!("{:?}", session.query(begin, end)))
        .collect()
}

#[test]
fn native_scores_play_as_javascript_plays_them() {
    let mut native = 0;
    for path in corpus() {
        let source = std::fs::read_to_string(&path).expect("read a corpus score");
        // One session builds both, since a graph carries its session's
        // runtime settings.
        let mut session = Session::with_config(SessionConfig::default()).expect("a session");
        if session.evaluate(&source).is_err() || !session.evaluated_natively() {
            continue;
        }
        native += 1;
        let built = session.active_pattern_for_test().expect("a native graph");
        let (cps, played) = (session.cps(), answers(&session));
        session.set_native_tier(false);
        session
            .evaluate(&source)
            .unwrap_or_else(|error| panic!("{}: JavaScript refused: {error}", path.display()));
        assert!(!session.evaluated_natively());
        let scripted = session.active_pattern_for_test().expect("a script graph");
        assert!(
            built.same_construction(scripted.without_final_controls()),
            "{}: the tiers built different graphs",
            path.display()
        );
        assert_eq!(cps, session.cps(), "{}", path.display());
        assert_eq!(played, answers(&session), "{}", path.display());
    }
    assert!(native >= 510, "only {native} corpus scores ran natively");
}
