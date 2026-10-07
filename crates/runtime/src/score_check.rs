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
    let mut eval_error = session
        .evaluate_no_fallback_cancellable(source, cancellation)
        .err()
        .map(|error| {
            let message = error.to_string();
            let line = extract_reported_line(&message);
            EvalError { message, line }
        });
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
        samples.wait_until_idle_cancellable(Duration::from_secs(5), Some(cancellation));
    }
    // Evaluation queues imports under the session's existing sample grants.
    // The linter judges only the sources the loader has finished publishing.
    let diagnostics = lint::lint(source, false, library);
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
