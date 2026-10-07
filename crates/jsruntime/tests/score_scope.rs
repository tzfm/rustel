/*
rustel-jsruntime - re-evaluating a score that declares things
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! Playing a score, stopping, and playing it again is the most ordinary thing
//! a live coder does, and `const main = stack(…)` is an ordinary way to write
//! one. A score is evaluated inside a FUNCTION so its declarations belong to
//! that evaluation; without one they land in the global lexical environment
//! and the second play fails with `SyntaxError: redeclaration`. A block is
//! not enough: `var` hoists out of a block.

use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, Slot};
use rustel_transpiler::TranspileOptions;

pub(crate) fn runtime() -> JsRuntime {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt
}

/// Evaluate the same source three times, returning the first failure.
pub(crate) fn replay(source: &str) -> Result<(), String> {
    let rt = runtime();
    for pass in 1..=3 {
        rt.evaluate_score(source, &TranspileOptions::default())
            .map_err(|error| format!("pass {pass}: {error}"))?;
    }
    Ok(())
}

#[test]
fn a_score_that_declares_a_binding_can_be_played_again() {
    // A script may redeclare `var` and `function`, so they prove nothing
    // here. They can break the next score instead; see
    // `a_var_in_one_score_does_not_reach_the_next`.
    for keyword in ["const", "let"] {
        let source = format!(
            r#"{keyword} main = stack(s("bd*4"), note("c3 e3"))
               $: main"#
        );
        replay(&source).unwrap_or_else(|error| panic!("{keyword}: {error}"));
    }
    replay("class Shape { }\n$: s(\"bd\")").expect("class");
}

#[test]
fn a_var_in_one_score_does_not_reach_the_next() {
    // `var` hoists out of a block to the global var scope. A score that
    // names a `var` after a control must not replace that control for the
    // next score.
    let rt = runtime();
    let options = TranspileOptions::default();
    rt.evaluate_score("var gain = 0.5\n$: s(\"bd*4\")", &options)
        .expect("a score may name whatever it likes");
    rt.evaluate_score("$: s(\"bd\").set(gain(0.3))", &options)
        .expect("the next score still has the real `gain`");
}

#[test]
fn a_function_declared_by_one_score_does_not_reach_the_next() {
    let rt = runtime();
    let options = TranspileOptions::default();
    rt.evaluate_score("function gain() { return 1 }\n$: s(\"bd*4\")", &options)
        .expect("a score may declare whatever it likes");
    rt.evaluate_score("$: s(\"bd\").set(gain(0.3))", &options)
        .expect("the next score still has the real `gain`");
}

#[test]
fn the_same_binding_name_may_be_reused_by_the_next_score() {
    // Two different scores, each declaring `main`, in one session: the second
    // must not collide with the first.
    let rt = runtime();
    rt.evaluate_score(
        "const main = s(\"bd\")\n$: main",
        &TranspileOptions::default(),
    )
    .expect("first score");
    rt.evaluate_score(
        "const main = s(\"hh\")\n$: main",
        &TranspileOptions::default(),
    )
    .expect("second score with the same binding name");
}

#[test]
fn a_lane_survives_whatever_the_score_writes_after_it() {
    // A `$:` lane is recorded when it is evaluated, so it plays whatever the
    // score writes afterwards. This is the shape essentially every score uses,
    // and it is why the return policy below costs real scores nothing.
    for tail in [
        "const x = 1",
        "class K { }",
        "if (1) { }",
        "// just a comment",
    ] {
        let rt = runtime();
        let source = format!("$: s(\"bd*4\")\n{tail}");
        rt.evaluate_score(&source, &TranspileOptions::default())
            .unwrap_or_else(|error| panic!("{tail}: {error}"));
        let haps = rt
            .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .expect("query");
        assert_eq!(haps.len(), 4, "lane lost after `{tail}`");
    }
}

#[test]
fn a_bare_score_ending_on_a_declaration_plays_nothing_as_upstream_does() {
    // Without a lane, the score's value IS its last statement, and upstream
    // answers a final non-expression statement with `silence`
    // (`packages/core/evaluate.mjs`). A score ending on a declaration named no
    // pattern, so nothing plays. Copied rather than improved: it is not a
    // crash and not a note silenced behind the author's back.
    let rt = runtime();
    rt.evaluate_score("s(\"bd*4\")\nconst tail = 1", &TranspileOptions::default())
        .expect("it evaluates");
    let haps = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("query");
    assert!(haps.is_empty(), "expected silence, got {haps:?}");
}

#[test]
fn one_score_cannot_break_the_next_by_touching_call() {
    // The wrapper invokes plainly rather than through `Function.prototype.call`
    // so a score that assigns that name cannot stop every later evaluation
    // before its body starts.
    let rt = runtime();
    let options = TranspileOptions::default();
    rt.evaluate_score("Function.prototype.call = null\n$: s(\"bd\")", &options)
        .expect("a hostile score still evaluates");
    rt.evaluate_score("$: s(\"hh*2\")", &options)
        .expect("the next score is unaffected");
}

#[test]
fn a_score_that_names_no_pattern_still_says_so() {
    // The wrapper must return `undefined` for a program with no final
    // expression. That is how the runtime tells "this score named no pattern"
    // from "this score produced a value that is not a pattern"; synthesising
    // `silence` makes a commented-out score look playable and loses the
    // message. It is why the wrapper cannot use `add_final_return`.
    let rt = runtime();
    let error = rt
        .evaluate_score("// $: s(\"bd\")", &TranspileOptions::default())
        .expect_err("a score that is entirely commented out names no pattern");
    assert!(
        error.to_string().contains("undefined"),
        "the undefined sentinel must survive the wrapper: {error}"
    );
}

#[test]
fn a_score_that_awaits_can_be_played_again_too() {
    // The awaiting path was already wrapped, which is why this bug only ever
    // showed on scores WITHOUT `samples(…)`. Pinned so the two paths cannot
    // drift apart again.
    replay("const main = s(\"bd\")\nawait Promise.resolve(1)\n$: main").expect("await path");
}

/// The `await` path, which the transpiler enters for a bare `samples(...)`.
///
/// This is the half that already worked, and the reason the scoping bug was
/// invisible for so long: a score that loads samples replayed fine while the
/// same score without them did not. These pin the two paths together so they
/// cannot drift apart again.
mod await_path {
    use super::{replay, runtime};
    use rustel_fraction::Fraction;
    use rustel_jsruntime::Slot;
    use rustel_transpiler::TranspileOptions;

    /// Every hap of the first cycle, as `control=value` pairs.
    fn haps(source: &str) -> Vec<String> {
        let rt = runtime();
        rt.evaluate_score(source, &TranspileOptions::default())
            .unwrap_or_else(|error| panic!("{source}: {error}"));
        rt.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .expect("query")
            .iter()
            .map(|hap| hap.value.show())
            .collect()
    }

    #[test]
    fn a_bare_samples_call_is_what_puts_a_score_on_the_async_path() {
        // `samples(...)` is the only
        // thing that puts one there without the author typing it.
        let transpiled = rustel_transpiler::transpile(
            "samples('github:x/y')\n$: s(\"bd\")",
            &TranspileOptions::default(),
        )
        .output;
        assert!(
            transpiled.contains("await samples("),
            "the transpiler no longer awaits a bare samples(): {transpiled}"
        );
    }

    #[test]
    fn both_paths_produce_the_same_haps_for_the_same_score() {
        // The bug was that these two disagreed. A trailing `await 0` is the
        // smallest thing that moves a score between them.
        for score in [
            "const main = s(\"bd*4\")\n$: main",
            "$: s(\"bd hh\").gain(0.5)",
            "var v = 2\n$: s(\"bd*4\").fast(v)",
            "$: s(\"bd*4\")\nconst tail = 1",
            // No lane, so the returned value is the whole result. A `$:` in
            // every case would hide a difference between the two paths.
            "s(\"bd*4\")",
            "s(\"bd*4\")\nconst tail = 1",
            "s(\"bd*4\")\nif (1) { }",
            "s(\"bd*4\")\nclass K { }",
        ] {
            let synchronous = haps(score);
            // `await 0` is prepended. Appended, it would be the score's last
            // expression, and the score would return 0 and not the pattern.
            let asynchronous = haps(&format!("await 0\n{score}"));
            assert_eq!(
                synchronous, asynchronous,
                "the two evaluation paths disagree on `{score}`"
            );
        }
    }

    #[test]
    fn the_async_path_scopes_declarations_too() {
        replay("const main = s(\"bd\")\nawait 0\n$: main").expect("const");
        let rt = runtime();
        let options = TranspileOptions::default();
        rt.evaluate_score("var gain = 0.5\nawait 0\n$: s(\"bd*4\")", &options)
            .expect("first score");
        rt.evaluate_score("$: s(\"bd\").set(gain(0.3))", &options)
            .expect("the next score still has the real `gain`");
    }

    #[test]
    fn a_commented_out_score_names_no_pattern_whatever_the_comment_says() {
        // A comment mentioning `await` used to send an otherwise-empty score
        // down the other path. Both wrappers
        // must answer an empty body the same way, or a score that is entirely
        // commented out becomes playable because of a word in the comment.
        for source in [
            "// $: s(\"bd\")",
            "// TODO: await samples later",
            "/* await */",
        ] {
            let rt = runtime();
            let error = rt
                .evaluate_score(source, &TranspileOptions::default())
                .expect_err("a commented-out score names no pattern");
            assert!(
                error.to_string().contains("undefined"),
                "`{source}` did not report the undefined sentinel: {error}"
            );
        }
    }

    #[test]
    fn a_score_whose_value_is_an_already_resolved_promise_plays() {
        // `async function` unwraps a returned thenable and a plain function
        // does not, so this played on one path and errored on the other.
        // Upstream evaluates every score in an async arrow, so it plays there.
        for score in [
            "Promise.resolve(s(\"bd*4\"))",
            "await 0\nPromise.resolve(s(\"bd*4\"))",
        ] {
            assert_eq!(haps(score).len(), 4, "`{score}` did not play");
        }
    }

    #[test]
    fn a_rejected_promise_reports_the_scores_own_error_on_both_paths() {
        // Rethrown rather than left to fail conversion, so a reader is told
        // what the score did instead of "could not convert a promise".
        for score in [
            "Promise.reject(new Error(\"nope\"))",
            "await 0\nPromise.reject(new Error(\"nope\"))",
        ] {
            let rt = runtime();
            let error = rt
                .evaluate_score(score, &TranspileOptions::default())
                .expect_err("a rejected promise is not a pattern")
                .to_string();
            assert!(
                !error.contains("converting"),
                "`{score}` reported a conversion failure: {error}"
            );
        }
    }

    #[test]
    fn a_for_await_loop_reaches_the_path_that_can_run_it() {
        // `for await` is a ForOfStatement carrying a flag, not an
        // AwaitExpression, so a visitor looking only for the expression sent
        // this to the wrapper that cannot compile it.
        let source = "const xs = { async *[Symbol.asyncIterator]() { yield \"bd\" } }\n\
                      let last = \"hh\"\n\
                      for await (const x of xs) { last = x }\n\
                      s(last)";
        assert_eq!(haps(source).len(), 1, "a for-await score must run");
    }

    #[test]
    fn a_promise_that_never_settles_is_still_refused() {
        // Only an already-settled promise is read. Waiting for a pending one
        // would mean running jobs, and a score that queues deferred work is
        // refused so evaluation stays bounded.
        let rt = runtime();
        rt.evaluate_score("new Promise(() => {})", &TranspileOptions::default())
            .expect_err("a pending promise is not a pattern");
    }

    #[test]
    fn the_word_await_as_text_does_not_change_how_a_score_is_evaluated() {
        // The dispatch asks the parser. A score that queues a microtask
        // is refused so evaluation stays bounded, and no amount of writing
        // the word somewhere inert may buy it the job-pumping path.
        //
        // Deliberately NOT compared through haps: every one of these has a
        // lane, and a recorded lane makes the returned value irrelevant, so a
        // hap comparison passes whichever path runs. The refusal is the
        // observable that differs.
        let queues = "Promise.resolve().then(() => {});\n";
        for inert in [
            "",
            "// await\n",
            "/* await */\n",
            "const awaiting = 1\n",
            "$: s(\"await\")\n",
            // The classes a byte scanner got wrong: a regex, a property name,
            // and a member access all contain the word and none awaits.
            "/await/.test(\"x\")\n",
            "const o = { await: 1 }\n",
            "const o = {}; o.await\n",
        ] {
            let rt = runtime();
            let source = format!("{inert}{queues}$: s(\"bd\")");
            // The BOUNDED entry point: the refusal lives there, not in the
            // raw evaluator, because it is a property of a Session's budget.
            rt.evaluate_score_cancellable(
                &source,
                &TranspileOptions::default(),
                std::time::Duration::from_secs(1),
                &std::sync::atomic::AtomicBool::new(false),
            )
            .expect_err(&format!(
                "a queued job must be refused; inert text: {inert:?}"
            ));
        }
        // A real `await` reaches the async path. That includes one inside a
        // template interpolation, which is code inside a quoted literal.
        assert_eq!(haps("await 0\n$: s(\"bd*4\")").len(), 4);
        assert_eq!(haps("$: s(`${await Promise.resolve(\"bd\")}`)").len(), 1);
    }
}
