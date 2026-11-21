//! Answering "is this a valid score": the static findings from
//! [`lint::lint`], plus whatever a dry run of the engine itself turns up.
//! `check` reports both together; `query` refuses the lint findings that
//! need no sample library before its own fallback-free evaluation (through
//! [`Session::evaluate_no_fallback_cancellable`]), so a mistake is not
//! listed as an empty pattern.
//!
//! The lint module is deliberately static - it never evaluates the score,
//! so it cannot see a name that resolves to nothing until the engine runs,
//! or a score that throws outright. strudel.cc has no equivalent recovery:
//! a JavaScript failure there is a `ReferenceError` in the console, not a
//! quiet reinterpretation as mini-notation. Both commands refuse that
//! recovery so both agree with it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rustel_fraction::Fraction;

use crate::lint::{self, Diagnostic, Level};
use crate::samples::SampleLibrary;
use crate::session::{RuntimeError, Session};
use rustel_voice::SampleLookup;

/// Where an evaluation failure happened, when the underlying message says.
///
/// The host writes a `" - line N[, column C]"` suffix onto a JavaScript
/// error once it can attribute one (see `describe_caught_js_value` in
/// rustel-jsruntime); a mini-notation or transpiler failure may carry no
/// position at all, in which case this is `None` rather than a guess.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvalError {
    pub message: String,
    pub line: Option<usize>,
}

/// The lint findings plus the outcome of a dry evaluation, if one ran.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScoreCheck {
    pub diagnostics: Vec<Diagnostic>,
    pub eval_error: Option<EvalError>,
}

impl ScoreCheck {
    /// Whether the score is clean enough to run: no error-level lint
    /// finding, and no evaluation failure. An informational note never
    /// fails a check, matching `lint::lint`'s own rule.
    pub fn is_ok(&self) -> bool {
        self.eval_error.is_none()
            && !self
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.level != Level::Note)
    }
}

/// Evaluate `source` once against `session`, with the mini-notation
/// compatibility fallback disabled, then lint it.
///
/// When sample checking is enabled, local imports have up to five seconds
/// to finish before linting. Pending manifests still defer sound-name checks.
/// Cancellation interrupts that wait and makes the check unsuccessful.
///
/// `session` is spent by this call: it installs whatever `source` evaluates
/// to, with that evaluation's `samples()` registrations and diagnostics,
/// exactly as an ordinary one-shot evaluation would.
pub fn check_score(
    source: &str,
    library: Option<&SampleLibrary>,
    session: &mut Session,
    cancellation: &AtomicBool,
) -> ScoreCheck {
    check_score_with(
        source,
        library,
        session,
        cancellation,
        &lint::LintContext::default(),
    )
}

/// [`check_score`] after a setup: `context` holds the names the setup
/// defined, which the session has already evaluated.
pub fn check_score_with(
    source: &str,
    library: Option<&SampleLibrary>,
    session: &mut Session,
    cancellation: &AtomicBool,
    context: &lint::LintContext,
) -> ScoreCheck {
    let mut eval_error = session
        .evaluate_no_fallback_cancellable(source, cancellation)
        .err()
        .map(|error| {
            let message = error.to_string();
            let line = extract_reported_line(&message);
            EvalError { message, line }
        });
    // A score can evaluate and still refuse when it is queried: a chunk
    // count or a Euclid step past its limit, or a KabelSalat graph that will
    // not build. Query its first cycle, as the first update would, so the
    // check reports what query and export would.
    if eval_error.is_none() && !cancellation.load(Ordering::Relaxed) {
        let thrown = match session.query_report(Fraction::ZERO, Fraction::ONE) {
            Ok(report) => report
                .query_threw
                .map(|message| format!("the pattern threw while querying: {message}")),
            Err(error) => Some(error.to_string()),
        };
        // A stop that lands during the query is the check's cancellation,
        // reported as such below, not a fault of the score.
        if let Some(message) = thrown.filter(|_| !cancellation.load(Ordering::Relaxed)) {
            let line = extract_reported_line(&message);
            eval_error = Some(EvalError { message, line });
        }
    }
    if eval_error.is_none()
        && library.is_some()
        && !cancellation.load(Ordering::Relaxed)
        && crate::sounds::samples_imports(source).iter().any(|import| {
            import
                .spec
                .as_deref()
                .is_some_and(|spec| spec.starts_with("local:"))
        })
        && let Some(samples) = session.sample_library()
    {
        samples.wait_until_idle_cancellable(Duration::from_secs(5), || {
            cancellation.load(Ordering::Relaxed)
        });
    }
    // Evaluation queues imports under the session's existing sample grants.
    // The linter judges only the sources the loader has finished publishing.
    let mut diagnostics = lint::lint_with(source, false, library, context);
    if eval_error.is_none() && library.is_some() && !cancellation.load(Ordering::Relaxed) {
        diagnostics.extend(notes_outside_their_fonts(session, cancellation));
    }
    if eval_error.is_none() && cancellation.load(Ordering::Relaxed) {
        eval_error = Some(EvalError {
            message: RuntimeError::Cancelled.to_string(),
            line: None,
        });
    }
    ScoreCheck {
        diagnostics,
        eval_error,
    }
}

/// The notes of the first cycle that a `gm_*` soundfont has no zone for:
/// they play as silence, as on strudel.cc, and the check says so with the
/// words a render uses. The fonts the cycle names are fetched first, for
/// at most a few seconds; a font that does not come in is not judged.
fn notes_outside_their_fonts(session: &mut Session, cancellation: &AtomicBool) -> Vec<Diagnostic> {
    let Ok(report) = session.query_report(Fraction::ZERO, Fraction::ONE) else {
        return Vec::new();
    };
    let Some(samples) = session.sample_library() else {
        return Vec::new();
    };
    let fonts: Vec<_> = report
        .haps
        .iter()
        .filter(|hap| hap.has_onset)
        .filter_map(|hap| match &hap.value {
            crate::hap_json::ValueJson::Raw(serde_json::Value::Object(object)) => {
                let sound = object.get("s")?.as_str()?;
                let font = sound.starts_with("gm_") && !object.contains_key("bank");
                font.then(|| FontNote {
                    value: object.clone(),
                    sound: sound.to_owned(),
                    at: hap.context.first().copied(),
                })
            }
            _ => None,
        })
        .collect();
    if fonts.is_empty() {
        return Vec::new();
    }
    // Asking is what starts the fetch.
    for note in &fonts {
        let _ = rustel_voice::sample_failure(&note.value, samples.as_ref());
    }
    samples.wait_until_idle_cancellable(Duration::from_secs(5), || {
        cancellation.load(Ordering::Relaxed)
    });
    font_notes_missing(fonts, samples.as_ref())
}

/// A note a `gm_*` soundfont plays: the hap's value, its sound, and where
/// the score wrote it.
struct FontNote {
    value: serde_json::Map<String, serde_json::Value>,
    sound: String,
    at: Option<(usize, usize)>,
}

/// The findings for the font notes of a cycle against `samples`: one per
/// message, so a note heard many times is said once.
fn font_notes_missing(fonts: Vec<FontNote>, samples: &dyn SampleLookup) -> Vec<Diagnostic> {
    let mut said = Vec::new();
    let mut diagnostics = Vec::new();
    for FontNote { value, sound, at } in fonts {
        let n = value
            .get("n")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0);
        // A font that came in has a range; one that failed to come in is
        // the network's, not the score's.
        if samples.note_range(&sound, n).is_none() {
            continue;
        }
        let Some(message) = rustel_voice::sample_failure(&value, samples) else {
            continue;
        };
        if said.contains(&message) {
            continue;
        }
        let (from, to) = at.unwrap_or((0, 0));
        diagnostics.push(Diagnostic {
            level: Level::Value,
            message: message.clone(),
            from,
            to,
        });
        said.push(message);
    }
    diagnostics
}

/// Pull the line number back out of a `" - line N"` or `" - line N, column
/// C"` suffix, the shape the JS host writes. A message without that exact
/// marker reports no line.
fn extract_reported_line(message: &str) -> Option<usize> {
    let (_, after) = message.rsplit_once(" - line ")?;
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A font that plays midi 48 to 84, found nowhere else.
    struct Clarinet;

    impl SampleLookup for Clarinet {
        fn resolve(&self, _s: &str, _n: f64, midi: f64) -> rustel_voice::SampleResolution {
            if (48.0..=84.0).contains(&midi) {
                rustel_voice::SampleResolution::Loading
            } else {
                rustel_voice::SampleResolution::Failed
            }
        }

        fn note_range(&self, s: &str, _n: f64) -> Option<(f64, f64)> {
            (s == "gm_clarinet").then_some((48.0, 84.0))
        }
    }

    fn font_note(sound: &str, note: &str) -> FontNote {
        let serde_json::Value::Object(value) = serde_json::json!({ "s": sound, "note": note })
        else {
            unreachable!("an object")
        };
        FontNote {
            value,
            sound: sound.to_owned(),
            at: Some((3, 9)),
        }
    }

    /// A note a loaded font has no zone for is a finding, said once
    /// however often it plays; a note in its range, and a font that never
    /// came in, are not.
    #[test]
    fn a_note_outside_its_font_is_a_finding() {
        let found = font_notes_missing(
            vec![
                font_note("gm_clarinet", "c2"),
                font_note("gm_clarinet", "c2"),
                font_note("gm_clarinet", "c3"),
                font_note("gm_recorder", "c2"),
            ],
            &Clarinet,
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].level, Level::Value);
        assert_eq!((found[0].from, found[0].to), (3, 9));
        assert!(
            found[0]
                .message
                .contains("has no note at midi 36: it plays midi 48 to 84"),
            "{}",
            found[0].message
        );
    }

    #[test]
    fn extracts_a_line_with_no_column() {
        assert_eq!(
            extract_reported_line("ReferenceError: gibberish is not defined - line 1"),
            Some(1)
        );
    }

    #[test]
    fn extracts_a_line_ahead_of_a_column() {
        assert_eq!(
            extract_reported_line("SyntaxError: unexpected token - line 3, column 5"),
            Some(3)
        );
    }

    #[test]
    fn reports_no_line_when_the_message_carries_none() {
        assert_eq!(extract_reported_line("javascript: boom"), None);
    }
}
