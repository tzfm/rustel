use rustel_core::{QueryLimit, Value};
use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, QueryError, Slot};
use rustel_transpiler::TranspileOptions;

fn runtime() -> JsRuntime {
    let runtime = JsRuntime::new().expect("runtime");
    runtime.install_semantic_bindings().expect("bindings");
    runtime
}

fn evaluate(runtime: &JsRuntime, source: &str) -> Vec<rustel_core::Hap> {
    evaluate_over(runtime, source, Fraction::ONE)
}

/// The haps `source` produces from cycle zero to `cycles`.
fn evaluate_over(runtime: &JsRuntime, source: &str, cycles: Fraction) -> Vec<rustel_core::Hap> {
    runtime
        .evaluate_score(source, &TranspileOptions::default())
        .expect("evaluate");
    runtime
        .query(Slot::Active, 0, Fraction::ZERO, cycles)
        .expect("query")
}

/// Assert that `free` and `method` produce the same non-empty haps over four
/// cycles.
fn assert_plays_like(runtime: &JsRuntime, free: &str, method: &str) {
    let events = |source| {
        evaluate_over(runtime, source, Fraction::from(4))
            .into_iter()
            .map(|hap| (hap.whole, hap.part, hap.value))
            .collect::<Vec<_>>()
    };
    let expected = events(method);
    assert!(!expected.is_empty(), "`{method}` played nothing");
    assert_eq!(
        events(free),
        expected,
        "`{free}` does not play what `{method}` plays"
    );
}

#[test]
fn scalar_registered_methods_reject_function_arguments() {
    let runtime = runtime();
    runtime
        .evaluate_score(
            r#"s("sd:3")
                .seg(8)
                .struct("~ x ~ x")
                .sometimesBy(1, x => x.ply(2, (i, n) => x.vel(1)))"#,
            &TranspileOptions::default(),
        )
        .expect("the callback is invoked while querying");

    let haps = runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("queryArc-compatible user errors become silence");
    assert!(
        haps.is_empty(),
        "the invalid callback produced plausible haps"
    );
    let logs = runtime.take_logs();
    assert!(
        logs.iter()
            .any(|line| line.contains(".ply() does not accept a function")),
        "missing query diagnostic: {logs:?}"
    );
}

#[test]
fn registered_function_validation_preserves_callbacks_and_scalar_sequences() {
    let runtime = runtime();
    let callback = evaluate(
        &runtime,
        r#"s("bd sd").sometimesBy(1, pattern => pattern.rev())"#,
    );
    assert!(!callback.is_empty(), "declared transformer was rejected");

    let sequence = evaluate(&runtime, r#"s("bd").fast(2, 4)"#);
    assert!(!sequence.is_empty(), "valid scalar sequence was rejected");
}

#[test]
fn chained_cps_is_the_cycles_per_second_form_of_cpm() {
    let runtime = runtime();
    let cps = evaluate(&runtime, r#"s("bd*4").cps(1)"#);
    let cpm = evaluate(&runtime, r#"s("bd*4").cpm(60)"#);

    assert!(!cps.is_empty());
    assert_eq!(cps.len(), cpm.len());
    for (from_cps, from_cpm) in cps.iter().zip(&cpm) {
        assert_eq!(from_cps.whole, from_cpm.whole);
        assert_eq!(from_cps.part, from_cpm.part);
        assert_eq!(from_cps.value, from_cpm.value);
    }
}

#[test]
fn chained_cps_overflow_does_not_panic() {
    let runtime = runtime();
    runtime
        .evaluate_score(r#"s("bd").cps(1e38)"#, &TranspileOptions::default())
        .expect("evaluate");

    let result = runtime.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE);
    assert!(
        matches!(
            result,
            Err(QueryError::Limit(QueryLimit::NativeFraction {
                operation: "cps"
            }))
        ),
        "overflowing cps must be refused through the typed channel: {result:?}"
    );
}

#[test]
fn edo_scale_accepts_literal_arrays_and_patterned_definitions() {
    let runtime = runtime();
    let literal = evaluate(
        &runtime,
        r#"n("0 1").edoScale(['C3', 'LLsLLLs', 2, 1]).s("triangle")"#,
    );
    assert!(
        !literal.is_empty(),
        "a structural definition became silence"
    );
    assert!(
        literal.iter().all(|hap| hap.value.get("freq").is_some()),
        "the literal definition was not resolved: {literal:?}"
    );

    let patterned = evaluate(
        &runtime,
        r#"n("0 1").edoScale("G2:<LLsLLL LLLLsL>:3:1").s("triangle")"#,
    );
    assert!(
        !patterned.is_empty(),
        "a patterned definition became silence"
    );
    assert!(
        patterned.iter().all(|hap| hap.value.get("freq").is_some()),
        "the patterned definition was not resolved: {patterned:?}"
    );
}

#[test]
fn native_slice_splice_fit_and_scrub_keep_their_values() {
    let runtime = runtime();
    let haps = evaluate(&runtime, r#"s("bd").splice(4,"0 2")"#);
    assert_eq!(haps.len(), 2);
    assert_eq!(
        haps[0].whole.expect("whole").duration(),
        Fraction::new(1, 2)
    );
    assert_eq!(haps[0].value.get("begin"), Some(&Value::F64(0.0)));
    assert_eq!(haps[0].value.get("end"), Some(&Value::F64(0.25)));
    assert_eq!(haps[0].value.get("speed"), Some(&Value::F64(0.5)));
    assert_eq!(haps[0].value.get("unit"), Some(&Value::Str("c".into())));

    let haps = evaluate(&runtime, r#"s("bd").slice(4,"0 2").fit()"#);
    assert_eq!(haps.len(), 2);
    assert_eq!(haps[1].value.get("speed"), Some(&Value::F64(0.5)));

    let haps = evaluate(&runtime, r#"s("bd").scrub(0)"#);
    assert_eq!(haps[0].value.get("begin"), Some(&Value::F64(0.0)));
    assert_eq!(haps[0].value.get("speed"), Some(&Value::F64(1.0)));
    assert_eq!(haps[0].value.get("clip"), Some(&Value::F64(1.0)));

    // A position that is already a control bag gives `begin` its `value`
    // and keeps the colour, like strudel's `createParam`.
    let haps = evaluate(
        &runtime,
        r#"s("bd").scrub(pick([".3@3 .2".color("teal"), "0.5".color("red")], "0"))"#,
    );
    assert_eq!(haps.len(), 2);
    assert_eq!(haps[0].value.get("begin"), Some(&Value::F64(0.3)));
    assert_eq!(haps[0].value.get("color"), Some(&Value::Str("teal".into())));
    assert_eq!(haps[0].value.get("value"), None);
    assert_eq!(haps[1].value.get("begin"), Some(&Value::F64(0.2)));
}

/// Free `scrub`, `chunkInto` and `chunkinto`, called whole or curried inside
/// a conditional, produce the same haps as their methods.
#[test]
fn free_scrub_and_chunk_into_play_what_their_methods_play() {
    let runtime = runtime();

    assert_plays_like(
        &runtime,
        r#"scrub("0.5", s("bd"))"#,
        r#"s("bd").scrub("0.5")"#,
    );
    assert_plays_like(
        &runtime,
        r#"scrub("0.5:2", s("bd"))"#,
        r#"s("bd").scrub("0.5:2")"#,
    );
    assert_plays_like(
        &runtime,
        r#"s("bd*4").sometimesBy(.5, scrub(pick([".3@3 .2", "0.5"], "<0 1>")))"#,
        r#"s("bd*4").sometimesBy(.5, x => x.scrub(pick([".3@3 .2", "0.5"], "<0 1>")))"#,
    );

    assert_plays_like(
        &runtime,
        r#"chunkInto(4, p => p.rev(), s("bd sd ht lt"))"#,
        r#"s("bd sd ht lt").chunkInto(4, p => p.rev())"#,
    );
    assert_plays_like(
        &runtime,
        r#"s("bd sd ht lt").sometimesBy(.5, chunkInto(4, hurry(2)))"#,
        r#"s("bd sd ht lt").sometimesBy(.5, x => x.chunkInto(4, hurry(2)))"#,
    );
    assert_plays_like(
        &runtime,
        r#"chunkinto(4, hurry(2), s("bd sd ht lt"))"#,
        r#"s("bd sd ht lt").chunkInto(4, hurry(2))"#,
    );
}

#[test]
fn native_callback_combinators_and_collect_keep_their_results() {
    let runtime = runtime();
    let haps = evaluate(
        &runtime,
        r#"s("bd").plyForEach(3, (pattern, index) => pattern.add(n(index)))"#,
    );
    assert_eq!(haps.len(), 3);
    assert_eq!(haps[0].value.get("n"), None);
    assert_eq!(haps[1].value.get("n"), Some(&Value::F64(1.0)));
    assert_eq!(haps[2].value.get("n"), Some(&Value::F64(2.0)));

    let haps = evaluate(
        &runtime,
        r#"stack(note("c"), note("e")).collect().fmap(haps => haps.length)"#,
    );
    assert_eq!(haps.len(), 1);
    assert_eq!(haps[0].value, Value::F64(2.0));

    let haps = evaluate(
        &runtime,
        r#"s("bd").chunkInto(4, pattern => pattern.rev())"#,
    );
    assert_eq!(haps.len(), 4);
    assert_eq!(
        haps[0].whole.expect("whole").duration(),
        Fraction::new(1, 4)
    );
}

#[test]
fn native_partial_application_survives_collection() {
    let runtime = runtime();
    runtime
        .eval("globalThis.savedSlice = slice(4)")
        .expect("partial");
    runtime.run_gc();
    let haps = evaluate(&runtime, r#"savedSlice("0 2", s("bd"))"#);
    assert_eq!(haps.len(), 2);
    assert_eq!(haps[1].value.get("begin"), Some(&Value::F64(0.5)));
}
