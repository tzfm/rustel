/*
rustel-runtime - log() and logValues()
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! `.log()` prints when a note sounds, not when it is queried.
//!
//! Upstream attaches the callback to `hap.context.onTrigger` and calls it
//! from the browser's scheduler. This engine formats the text when the hap is
//! produced and prints it when the onset fires, the moment upstream logs at.
//! A re-queried span therefore cannot print twice.

use rustel_fraction::Fraction;
use rustel_runtime::Session;

/// The log lines a score's first `cycles` cycles produce, in order.
///
/// Scheduled in short steps because one transfer may not reach further than
/// the horizon; the point of the test is the ONSETS, not the window size.
fn lines(source: &str, cycles: i64) -> Vec<String> {
    let mut session = Session::new().expect("session");
    session.evaluate(source).expect("evaluate");
    scheduled(source, cycles).0
}

/// The log lines AND the onset count they came from. The scheduler runs ahead
/// of the window it is asked for, so an exact count is the wrong assertion:
/// the property that matters is one line per onset, no more and no fewer.
fn scheduled(source: &str, cycles: i64) -> (Vec<String>, usize) {
    let mut session = Session::new().expect("session");
    session.evaluate(source).expect("evaluate");
    let seconds = cycles as f64 / session.cps();
    let (mut collected, mut onsets) = (Vec::new(), 0usize);
    let mut at = 0.0f64;
    while at < seconds {
        let through = (at + 0.25).min(seconds);
        for onset in session.schedule_through(at, through).expect("schedule") {
            onsets += 1;
            if let Some(line) = onset.log_line {
                collected.push(line);
            }
        }
        at = through;
    }
    (collected, onsets)
}

#[test]
fn log_prints_one_line_for_each_note_that_sounds() {
    // Exactly one line per onset. A re-queried span cannot print twice
    // because the printing is driven by the onset, not by the query.
    let (lines, onsets) = scheduled(r#"$: s("bd*2").log()"#, 2);
    assert_eq!(lines.len(), onsets, "{lines:?}");
    assert!(onsets >= 4, "expected at least two cycles: {onsets}");
    assert!(lines[0].starts_with("[hap] "), "{lines:?}");
    assert!(lines[0].contains("s:bd"), "{lines:?}");
}

#[test]
fn log_values_shows_the_controls_rather_than_the_span() {
    let lines = lines(r#"$: s("bd sd").gain("0.25 0.5").n("2 1").logValues()"#, 1);
    assert_eq!(lines[0], "[hap] s:bd gain:0.25 n:2");
    assert_eq!(lines[1], "[hap] s:sd gain:0.5 n:1");
}

#[test]
fn a_formatter_of_your_own_is_used() {
    // Single quotes: a double-quoted string inside the callback is
    // mini-notation and would arrive as a Pattern, printing [object Object].
    let (lines, onsets) = scheduled(r#"$: s("bd*2").log(h => 'v:' + h.value.s)"#, 1);
    assert_eq!(lines.len(), onsets);
    assert!(lines.iter().all(|line| line == "v:bd"), "{lines:?}");
}

#[test]
fn logging_leaves_the_value_exactly_as_it_was() {
    // The reason the text rides the hap's CONTEXT and not its value: a value
    // is what a score means. `"0 1"` carries a bare number, and adding a key
    // to it would make the pattern mean something else.
    let mut with = Session::new().expect("session");
    with.evaluate(r#"$: note("0 1".log())"#).expect("evaluate");
    let mut without = Session::new().expect("session");
    without.evaluate(r#"$: note("0 1")"#).expect("evaluate");
    let show = |session: &mut Session| {
        session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query")
            .iter()
            .map(|hap| hap.value.show())
            .collect::<Vec<_>>()
    };
    assert_eq!(show(&mut with), show(&mut without));
}

/// What a score writes with `logger`/`console.log` while it EVALUATES,
/// drained the way the offline render path drains it.
fn written(source: &str) -> Vec<String> {
    let mut session = Session::new().expect("session");
    // Queue diagnostics whatever the process default, the same path the
    // studio drains into its log pane.
    session.set_direct_diagnostic_logging(false);
    session.evaluate(source).expect("evaluate");
    session.play(1.0).expect("play");
    session
        .take_diagnostics()
        .into_iter()
        .filter(|d| d.kind == "log")
        .map(|d| d.message)
        .collect()
}

#[test]
fn console_log_reaches_the_terminal() {
    // The shim was already routing to `logger`, inside a `try {} catch (_) {}`
    // that swallowed the ReferenceError when nothing answered to that name.
    // So `console.log` in a score was silent rather than broken, which is
    // harder to notice.
    assert_eq!(
        written(r#"console.log('hello'); $: s("bd")"#),
        vec!["hello"]
    );
    assert_eq!(
        written(r#"console.log('a', 'b', 1); $: s("bd")"#),
        vec!["a b 1"]
    );
}

#[test]
fn a_type_is_kept_in_front_of_the_message() {
    assert_eq!(
        written(r#"console.warn('careful'); $: s("bd")"#),
        vec!["[warn] careful"]
    );
    assert_eq!(
        written(r#"console.error('bad'); $: s("bd")"#),
        vec!["[error] bad"]
    );
    assert_eq!(
        written(r#"logger('typed', 'warning'); $: s("bd")"#),
        vec!["[warning] typed"]
    );
}

#[test]
fn nan_fallback_can_finally_say_what_it_did() {
    // The call returns the fallback either way. This checks the warning.
    let written = written(r#"nanFallback(NaN, 7); $: s("bd")"#);
    assert_eq!(written.len(), 1, "{written:?}");
    assert!(written[0].contains("not a number"), "{written:?}");
}
