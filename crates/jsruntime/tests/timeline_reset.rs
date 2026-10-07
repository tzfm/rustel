//! The `timeline` reset is host-owned, not a public global a score can replace.

use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, Slot};
use rustel_transpiler::TranspileOptions;

fn runtime() -> JsRuntime {
    let runtime = JsRuntime::new().expect("runtime");
    runtime
        .install_semantic_bindings()
        .expect("semantic bindings");
    runtime
}

#[test]
fn timeline_reset_is_not_a_public_global() {
    let runtime = runtime();
    runtime
        .eval("globalThis.__kind = typeof globalThis.__rustelResetTimelines")
        .expect("probe");
    assert_eq!(
        runtime.get_string("__kind").as_deref(),
        Some("undefined"),
        "a public reset is a hook a rejected save can replace"
    );
}

#[test]
fn timeline_is_still_registered() {
    let runtime = runtime();
    runtime
        .evaluate_score("s('bd').timeline(1)", &TranspileOptions::default())
        .expect("timeline combinator");
    let haps = runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("query");
    assert!(
        !haps.is_empty(),
        "timeline produced no events after moving its reset off globalThis"
    );
}

#[test]
fn public_timeline_sequences_nested_array_arguments() {
    let runtime = runtime();
    runtime
        .evaluate_score("s('bd').timeline([1, 2], 3)", &TranspileOptions::default())
        .expect("timeline sequence");
    let haps = runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("query nested timeline sequence");
    assert_eq!(haps.len(), 3, "{haps:#?}");
}

#[test]
fn raw_timeline_uses_its_second_argument_as_the_pattern() {
    let runtime = runtime();
    runtime
        .evaluate_score(
            "s('bd')._timeline(0, s('sd'), s('hh'))",
            &TranspileOptions::default(),
        )
        .expect("raw timeline");
    let haps = runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("query raw timeline");
    assert_eq!(haps.len(), 1);
    assert!(haps[0].show().contains("sd"), "{}", haps[0].show());
    assert!(!haps[0].show().contains("bd"), "{}", haps[0].show());
}

#[test]
fn zero_argument_raw_timeline_keeps_its_query_error() {
    let runtime = runtime();
    runtime
        .evaluate_score(
            r#"
              const broken = s('bd')._timeline();
              try {
                broken.query(new State(new TimeSpan(0, 1), {}));
                globalThis.__rawTimelineDirectThrow = 0;
              } catch (error) {
                globalThis.__rawTimelineDirectThrow = Number(error.message.includes('late'));
              }
              broken
            "#,
            &TranspileOptions::default(),
        )
        .expect("raw timeline constructs before querying");
    assert_eq!(runtime.get_number("__rawTimelineDirectThrow"), Some(1.0));
    let haps = runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("queryArc catches the raw callback failure");
    assert!(haps.is_empty());
}

#[test]
fn replacing_a_public_timeline_hook_does_not_freeze_the_next_save() {
    let runtime = runtime();
    runtime
        .evaluate_score(
            r#"globalThis.__rustelResetTimelines = () => { throw new Error('hijacked'); }; s("bd")"#,
            &TranspileOptions::default(),
        )
        .expect("first score");
    runtime
        .evaluate_score(r#"s("sd")"#, &TranspileOptions::default())
        .expect("a writable public reset must not freeze the next save");
    let haps = runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("query");
    assert_eq!(haps.len(), 1);
    assert!(haps[0].show().contains("sd"), "{}", haps[0].show());
}
