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
    let mut first_cycle = None;
    if eval_error.is_none() && !cancellation.load(Ordering::Relaxed) {
        let thrown = match session.query_report(Fraction::ZERO, Fraction::ONE) {
            Ok(report) => {
                let thrown = report
                    .query_threw
                    .clone()
                    .map(|message| format!("the pattern threw while querying: {message}"));
                first_cycle = Some(report);
                thrown
            }
            Err(error) => Some(error.to_string()),
        };
        // A patterned argument can refuse only in a later cycle, as
        // `euclid(3, "<8 1000000>")` does in its second.
        let thrown = thrown.or_else(|| later_cycle_refusal(session, cancellation));
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
    if eval_error.is_none()
        && library.is_some()
        && !cancellation.load(Ordering::Relaxed)
        && let Some(report) = &first_cycle
    {
        diagnostics.extend(notes_outside_their_fonts(
            report,
            session,
            source,
            cancellation,
        ));
    }
    if eval_error.is_none()
        && !cancellation.load(Ordering::Relaxed)
        && let Some(report) = &first_cycle
    {
        diagnostics.extend(talkit_settings(report, source));
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

/// How many cycles a check queries for a refusal. A pattern with a longer
/// period can still refuse past them.
const CHECKED_CYCLES: i64 = 8;

/// What the first query after cycle zero that fails says, with its cycle.
fn later_cycle_refusal(session: &mut Session, cancellation: &AtomicBool) -> Option<String> {
    for cycle in 1..CHECKED_CYCLES {
        if cancellation.load(Ordering::Relaxed) {
            return None;
        }
        let begin = Fraction::from(cycle as i32);
        let end = Fraction::from(cycle as i32 + 1);
        let thrown = match session.query_report(begin, end) {
            Ok(report) => report
                .query_threw
                .map(|message| format!("the pattern threw while querying: {message}")),
            Err(error) => Some(error.to_string()),
        };
        if let Some(message) = thrown {
            return Some(format!("cycle {cycle}: {message}"));
        }
    }
    None
}

/// The notes of the first cycle that a soundfont has no zone for: they play
/// as silence, as on strudel.cc, and the check says so with the words a
/// render uses. Each note is resolved as a voice resolves it, so the sound
/// is the one that plays. The fonts the cycle names are fetched first, for
/// at most a few seconds; a font that does not come in, or a `samples()`
/// still on its way that may name the sound, is not judged.
fn notes_outside_their_fonts(
    report: &crate::hap_json::QueryReport,
    session: &Session,
    source: &str,
    cancellation: &AtomicBool,
) -> Vec<Diagnostic> {
    let Some(samples) = session.sample_library() else {
        return Vec::new();
    };
    let cps = session.config().cps;
    let onsets: Vec<_> = report
        .haps
        .iter()
        .filter(|hap| hap.has_onset)
        .filter_map(|hap| match &hap.value {
            crate::hap_json::ValueJson::Raw(value @ serde_json::Value::Object(_)) => {
                Some((value, hap.context.first().copied()))
            }
            _ => None,
        })
        .collect();
    let resolve = |value| {
        rustel_voice::resolve_voice_with_samples_detailed(
            value,
            0,
            0.25,
            0.0,
            0.0,
            48_000,
            cps,
            samples.as_ref(),
        )
    };
    // Asking is what starts the fetch.
    let loading = onsets.iter().fold(false, |loading, (value, _)| {
        matches!(
            resolve(value),
            Err(rustel_voice::VoiceError::SampleLoading(_))
        ) || loading
    });
    if loading {
        samples.wait_until_idle_cancellable(Duration::from_secs(5), || {
            cancellation.load(Ordering::Relaxed)
        });
    }
    if samples.manifests_pending() > 0 {
        return Vec::new();
    }
    let mut said = std::collections::HashSet::new();
    let mut diagnostics = Vec::new();
    for (value, at) in onsets {
        let Err(rustel_voice::VoiceError::NoteOutsideFont(message)) = resolve(value) else {
            continue;
        };
        if !said.insert(message.clone()) {
            continue;
        }
        let (from, to) = at.unwrap_or((0, 0));
        diagnostics.push(Diagnostic {
            level: Level::Value,
            message,
            from: char_floor(source, from),
            to: char_floor(source, to),
        });
    }
    diagnostics
}

/// The most `talkit` findings a check reports: a patterned value that is
/// out of range is one mistake, with a different number on each note.
const MAX_TALKIT_FINDINGS: usize = 8;

/// The `talkit` notes of the first cycle whose text or settings a render
/// refuses, in the words the render uses, one time each. A note sent to
/// MIDI, OSC or serial speaks nothing here, so it is not judged. A phrase in
/// single quotes has no place of its own: the finding marks the first
/// `talkit(` of the score.
fn talkit_settings(report: &crate::hap_json::QueryReport, source: &str) -> Vec<Diagnostic> {
    let mut said = std::collections::HashSet::new();
    let mut diagnostics = Vec::new();
    let first_call = source.find("talkit(").map(|at| (at, at + "talkit".len()));
    for hap in report.haps.iter().filter(|hap| hap.has_onset) {
        let crate::hap_json::ValueJson::Raw(value) = &hap.value else {
            continue;
        };
        if ["midiport", "oscport", "serialport"]
            .iter()
            .any(|key| value.get(*key).is_some_and(|port| !port.is_null()))
        {
            continue;
        }
        let Some(message) = rustel_voice::talkit_refusal(value) else {
            continue;
        };
        if !said.insert(message.clone()) {
            continue;
        }
        let (from, to) = hap
            .context
            .first()
            .copied()
            .or(first_call)
            .unwrap_or((0, 0));
        diagnostics.push(Diagnostic {
            level: Level::Value,
            message,
            from: char_floor(source, from),
            to: char_floor(source, to),
        });
        if diagnostics.len() == MAX_TALKIT_FINDINGS {
            break;
        }
    }
    diagnostics
}

/// `at`, moved back to the start of the character it falls in, and no
/// further than the end of `source`.
fn char_floor(source: &str, at: usize) -> usize {
    let mut at = at.min(source.len());
    while !source.is_char_boundary(at) {
        at -= 1;
    }
    at
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

    /// A position inside a character moves back to its start, so a
    /// finding never cuts the score's text in two.
    #[test]
    fn a_finding_starts_on_a_character() {
        let source = "// 🎵\nnote(\"c2\")";
        assert_eq!(char_floor(source, 4), 3);
        assert_eq!(char_floor(source, 3), 3);
        assert_eq!(char_floor(source, 999), source.len());
    }

    #[test]
    fn extracts_a_line_ahead_of_a_column() {
        assert_eq!(
            extract_reported_line("SyntaxError: unexpected token - line 3, column 5"),
            Some(3)
        );
    }
}
