//! Transactional score/setup effects and their query-time policy boundary.

use std::cell::Cell;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use rustel_core::{QueryLimit, Value, pure};
use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, QueryError, ScoreEffects, Slot};
use rustel_transpiler::TranspileOptions;

static NOT_CANCELLED: AtomicBool = AtomicBool::new(false);

fn runtime() -> JsRuntime {
    let runtime = JsRuntime::new().expect("runtime");
    runtime
        .install_semantic_bindings()
        .expect("semantic bindings");
    runtime
}

fn evaluate(runtime: &JsRuntime, source: &str) -> Result<ScoreEffects, QueryError> {
    runtime
        .evaluate_score_with_effects_cancellable(
            source,
            &TranspileOptions::default(),
            Duration::from_secs(1),
            &NOT_CANCELLED,
        )
        .map(|(_, effects)| effects)
}

fn assert_policy(error: QueryError, effect: &str) {
    match error {
        QueryError::Policy(message) => assert!(
            message.contains(effect),
            "wrong host-effect policy for {effect}: {message}"
        ),
        other => panic!("host-effect refusal lost its policy channel: {other:?}"),
    }
}

fn active_values(runtime: &JsRuntime) -> Vec<String> {
    runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("active graph remains queryable")
        .into_iter()
        .map(|hap| hap.value.show())
        .collect()
}

#[test]
fn gamepad_polling_is_staged_only_for_an_accepted_score() {
    let runtime = runtime();

    // Outside a score, gamepad() works as on strudel.cc and requests at once.
    runtime.eval("gamepad()").expect("raw gamepad");
    runtime
        .evaluate_prelude_with_effects(
            "globalThis.pad = gamepad();",
            &TranspileOptions::default(),
            Duration::from_secs(1),
        )
        .expect("setup gamepad");

    let thrown = evaluate(&runtime, "gamepad(); throw new Error('reject pad')")
        .expect_err("failed score must discard its gamepad request");
    assert!(matches!(thrown, QueryError::Message(message) if message.contains("reject pad")));
    assert_eq!(
        evaluate(&runtime, "pure('after-reject')").unwrap(),
        ScoreEffects::default()
    );

    let accepted = evaluate(
        &runtime,
        "const pad = gamepad(); pure('accepted').mask(pad.a)",
    )
    .expect("score may request gamepad polling");
    assert!(accepted.gamepad);
    assert_eq!(
        evaluate(&runtime, "pure('next')").unwrap(),
        ScoreEffects::default(),
        "the gamepad request leaked to a later score"
    );

    let queried = evaluate(
        &runtime,
        "pure('x').fmap(() => { gamepad(); return 1; }).queryArc(0, 1); pure('x')",
    )
    .expect("a pattern function may read a pad");
    assert!(
        !queried.gamepad,
        "a query callback is not the score's request"
    );
}

#[test]
fn successful_score_returns_one_ordered_effect_transaction() {
    let runtime = runtime();
    let effects = evaluate(
        &runtime,
        r#"
          samples({ alpha: ['one.wav'] }, 'https://samples.example/kit/');
          preload('alpha:1', ['beta', 'gamma:2']);
          samples({ delta: ['two.wav'] });
          preload('delta');
          setCps(1.25);
          pure('accepted')
        "#,
    )
    .expect("accepted score");

    assert_eq!(effects.cps, Some(1.25));
    assert_eq!(
        effects.samples,
        [
            (
                r#"{"alpha":["one.wav"]}"#.to_string(),
                Some("https://samples.example/kit/".to_string()),
            ),
            (r#"{"delta":["two.wav"]}"#.to_string(), None),
        ]
    );
    assert_eq!(effects.preload, ["alpha:1 beta gamma:2", "delta"]);

    assert_eq!(
        evaluate(&runtime, "pure('next')").expect("next score"),
        ScoreEffects::default(),
        "accepted effects remained available to a later evaluation"
    );
}

#[test]
fn host_effect_collections_are_bounded_before_vector_growth() {
    let runtime = runtime();
    let counted = evaluate(
        &runtime,
        r#"
          let sampleLimitCaught = 0;
          for (let i = 0; i < 65; i++) {
            try { samples({ [String(i)]: ['x.wav'] }); }
            catch (_) { sampleLimitCaught++; }
          }
          let preloadLimitCaught = 0;
          for (let i = 0; i < 257; i++) {
            try { preload('sound-' + i); }
            catch (_) { preloadLimitCaught++; }
          }
          globalThis.sampleLimitCaught = sampleLimitCaught;
          globalThis.preloadLimitCaught = preloadLimitCaught;
          pure('accepted')
        "#,
    )
    .expect("caught host-effect count limits");
    assert_eq!(counted.samples.len(), 64);
    assert_eq!(counted.preload.len(), 256);
    assert_eq!(runtime.get_number("sampleLimitCaught"), Some(1.0));
    assert_eq!(runtime.get_number("preloadLimitCaught"), Some(1.0));

    let bytes = evaluate(
        &runtime,
        r#"
          let sampleBytesCaught = 0;
          let preloadBytesCaught = 0;
          try { samples({ huge: 'x'.repeat(4 * 1024 * 1024) }); }
          catch (_) { sampleBytesCaught++; }
          try { preload('x'.repeat(256 * 1024 + 1)); }
          catch (_) { preloadBytesCaught++; }
          globalThis.sampleBytesCaught = sampleBytesCaught;
          globalThis.preloadBytesCaught = preloadBytesCaught;
          pure('accepted')
        "#,
    )
    .expect("caught host-effect byte limits");
    assert_eq!(bytes, ScoreEffects::default());
    assert_eq!(runtime.get_number("sampleBytesCaught"), Some(1.0));
    assert_eq!(runtime.get_number("preloadBytesCaught"), Some(1.0));
}

#[test]
fn every_failed_score_exit_discards_all_staged_effects() {
    let runtime = runtime();
    runtime
        .eval("globalThis.effectSamples = globalThis.samples; globalThis.effectPreload = globalThis.preload;")
        .expect("save synchronous effect bindings");
    let prefix = r#"
      effectSamples({ leaked: ['leaked.wav'] }, 'https://samples.example/');
      effectPreload('leaked');
      setCps(3);
    "#;

    let thrown = evaluate(
        &runtime,
        &format!("{prefix} throw new Error('effect-boom'); pure('never')"),
    )
    .expect_err("throw after effects");
    assert!(matches!(thrown, QueryError::Message(message) if message.contains("effect-boom")));
    assert_eq!(
        evaluate(&runtime, "pure('after-throw')").unwrap(),
        ScoreEffects::default()
    );

    let pending = evaluate(
        &runtime,
        &format!("{prefix} Promise.resolve().then(() => {{}}); pure('pending')"),
    )
    .expect_err("detached job after effects");
    assert!(matches!(
        pending,
        QueryError::Limit(QueryLimit::JsPendingJobs)
    ));
    assert_eq!(
        evaluate(&runtime, "pure('after-pending')").unwrap(),
        ScoreEffects::default()
    );

    let deadline = runtime
        .evaluate_score_with_effects_cancellable(
            &format!("{prefix} while (true) {{}} pure('deadline')"),
            &TranspileOptions::default(),
            Duration::from_millis(20),
            &NOT_CANCELLED,
        )
        .expect_err("deadline after effects");
    assert!(matches!(
        deadline,
        QueryError::Limit(QueryLimit::JsCpuDeadline { .. })
    ));
    assert_eq!(
        evaluate(&runtime, "pure('after-deadline')").unwrap(),
        ScoreEffects::default()
    );

    let conversion = evaluate(
        &runtime,
        &format!("{prefix} globalThis.__effectNoPattern = 1"),
    )
    .expect_err("score without a pattern");
    assert!(matches!(conversion, QueryError::Message(_)));
    assert_eq!(
        evaluate(&runtime, "pure('after-conversion')").unwrap(),
        ScoreEffects::default()
    );

    let cancelled = AtomicBool::new(true);
    let cancelled = runtime
        .evaluate_score_with_effects_cancellable(
            "samples({ cancelled: ['x.wav'] }); pure('cancelled')",
            &TranspileOptions::default(),
            Duration::from_secs(1),
            &cancelled,
        )
        .expect_err("pre-cancelled score");
    assert!(matches!(
        cancelled,
        QueryError::Limit(QueryLimit::Cancelled)
    ));
    assert_eq!(
        evaluate(&runtime, "pure('after-cancel')").unwrap(),
        ScoreEffects::default()
    );

    let ceiling = runtime.heap_live() + 1024 * 1024;
    runtime
        .set_memory_limit(ceiling)
        .expect("install bounded heap headroom");
    let heap = evaluate(
        &runtime,
        &format!(
            "{prefix} globalThis.__effectHeapHog = new Array(2_000_000).fill(7); pure('heap')"
        ),
    )
    .expect_err("heap refusal after effects");
    assert!(matches!(heap, QueryError::Limit(QueryLimit::HostMemory)));
    assert_eq!(
        evaluate(&runtime, "pure('after-heap')").unwrap(),
        ScoreEffects::default()
    );
}

#[test]
fn setup_effects_commit_only_after_the_whole_setup_succeeds() {
    let runtime = runtime();
    let options = TranspileOptions::default();
    let (_, effects) = runtime
        .evaluate_prelude_with_effects(
            "samples({ setup: ['setup.wav'] }); preload('setup:0'); globalThis.setupKept = 1;",
            &options,
            Duration::from_secs(1),
        )
        .expect("successful setup");
    assert_eq!(effects.cps, None);
    assert_eq!(effects.samples.len(), 1);
    assert_eq!(effects.preload, ["setup:0"]);
    assert_eq!(runtime.get_number("setupKept"), Some(1.0));

    let error = runtime
        .evaluate_prelude_with_effects(
            "samples({ rejected: ['rejected.wav'] }); preload('rejected'); globalThis.setupBeforeThrow = 1; throw new Error('setup-boom');",
            &options,
            Duration::from_secs(1),
        )
        .expect_err("failed setup");
    assert!(matches!(error, QueryError::Message(message) if message.contains("setup-boom")));
    assert_eq!(runtime.get_number("setupBeforeThrow"), Some(1.0));

    let (_, next) = runtime
        .evaluate_prelude_with_effects(
            "globalThis.setupAfterThrow = 1;",
            &options,
            Duration::from_secs(1),
        )
        .expect("setup recovery");
    assert_eq!(next, ScoreEffects::default());

    let policy = runtime
        .evaluate_prelude_with_effects(
            "samples({ discarded: ['discarded.wav'] }); preload('discarded'); try { setCps(2); } catch (_) {}",
            &options,
            Duration::from_secs(1),
        )
        .expect_err("setup tempo refusal");
    assert_policy(policy, "setCps");
    let (_, after_policy) = runtime
        .evaluate_prelude_with_effects(
            "globalThis.setupAfterPolicy = 1;",
            &options,
            Duration::from_secs(1),
        )
        .expect("setup policy recovery");
    assert_eq!(after_policy, ScoreEffects::default());
}

#[test]
fn legacy_setup_apis_refuse_to_discard_successful_effects() {
    let runtime = runtime();
    let options = TranspileOptions::default();

    let sync = runtime
        .evaluate_prelude(
            "globalThis.syncSetupRan = 1; samples({ sync: ['sync.wav'] });",
            &options,
            Duration::from_secs(1),
        )
        .expect_err("legacy synchronous setup discarded a sample effect");
    match sync {
        QueryError::Policy(message) => assert!(
            message.contains("evaluate_prelude_with_effects"),
            "legacy synchronous setup gave no safe replacement API: {message}"
        ),
        other => panic!("legacy synchronous setup used the wrong refusal channel: {other:?}"),
    }
    assert_eq!(runtime.get_number("syncSetupRan"), Some(1.0));

    let cancellable = runtime
        .evaluate_prelude_cancellable(
            "globalThis.cancellableSetupRan = 1; preload('cancellable');",
            &options,
            Duration::from_secs(1),
            &NOT_CANCELLED,
        )
        .expect_err("legacy cancellable setup discarded a preload effect");
    match cancellable {
        QueryError::Policy(message) => assert!(
            message.contains("evaluate_prelude_with_effects_cancellable"),
            "legacy cancellable setup gave no safe replacement API: {message}"
        ),
        other => panic!("legacy cancellable setup used the wrong refusal channel: {other:?}"),
    }
    assert_eq!(runtime.get_number("cancellableSetupRan"), Some(1.0));

    let (_, effects) = runtime
        .evaluate_prelude_with_effects(
            "globalThis.afterLegacySetup = 1;",
            &options,
            Duration::from_secs(1),
        )
        .expect("setup recovered after legacy API refusals");
    assert_eq!(effects, ScoreEffects::default());
}

#[test]
fn raw_and_source_compatible_evaluation_cannot_record_effects() {
    let runtime = runtime();

    let raw = runtime
        .eval("try { samples({ raw: ['x.wav'] }); } catch (_) {}")
        .expect_err("raw sample effect");
    assert!(raw.contains("samples(...)"), "wrong raw policy: {raw}");

    let legacy = runtime
        .evaluate_score_cancellable(
            "try { preload('raw'); } catch (_) {} pure('must-not-publish')",
            &TranspileOptions::default(),
            Duration::from_secs(1),
            &NOT_CANCELLED,
        )
        .expect_err("source-compatible preload effect");
    assert_policy(legacy, "preload(...)");

    assert_eq!(
        evaluate(&runtime, "pure('clean')").expect("recovery"),
        ScoreEffects::default()
    );
}

#[test]
fn query_time_effects_are_structural_for_direct_and_nested_queries() {
    let runtime = runtime();
    runtime
        .eval("globalThis.effectSamples = globalThis.samples; globalThis.effectPreload = globalThis.preload;")
        .expect("save effect bindings for query callbacks");
    let effects = evaluate(
        &runtime,
        r#"
          pure('x').fmap(value => {
            try { effectSamples({ queried: ['queried.wav'] }); } catch (_) {}
            try { effectPreload('queried'); } catch (_) {}
            return value;
          })
        "#,
    )
    .expect("install query callback");
    assert_eq!(effects, ScoreEffects::default());

    let direct = runtime
        .query_cancellable(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            Duration::from_secs(1),
            &NOT_CANCELLED,
        )
        .expect_err("query-time sample effect");
    assert_policy(direct, "samples(...)");

    let pattern = runtime.active_pattern().expect("active query callback");
    let core_refused = Cell::new(false);
    let inherited = runtime
        .with_deadline(Duration::from_secs(1), || {
            runtime.with_active_scope(|| {
                let outcome = pattern.try_query_arc_sorted(Fraction::ZERO, Fraction::ONE);
                core_refused.set(outcome.is_err());
                outcome
            })
        })
        .expect_err("raw-deadline query-time sample effect");
    assert_policy(inherited, "samples(...)");
    assert!(
        core_refused.get(),
        "raw-deadline callback silence reached the host before structural refusal"
    );

    let mut builder = runtime.builder();
    let callback = builder.callback(
        &runtime,
        "(_owner) => value => { try { effectPreload('held'); } catch (_) {} return value; }",
    );
    runtime
        .hold(&builder, pure(Value::Str("held".into())).fmap_js(callback))
        .expect("hold callback-bearing pattern");
    runtime
        .install_query_binding()
        .expect("nested query binding");

    let nested = evaluate(
        &runtime,
        "queryHeld(0, 0, 1); samples({ candidate: ['candidate.wav'] }); pure('candidate')",
    )
    .expect_err("nested query-time preload effect");
    assert_policy(nested, "preload(...)");
    assert_eq!(
        evaluate(&runtime, "pure('after-nested')").expect("nested recovery"),
        ScoreEffects::default(),
        "the rejected candidate's later sample effect escaped its transaction"
    );
}

#[test]
fn engine_query_cannot_record_effects_through_a_native_patterns_query_method() {
    let runtime = runtime();
    runtime
        .eval("globalThis.effectSamples = globalThis.samples;")
        .expect("save sample effect binding");
    let effects = evaluate(
        &runtime,
        r#"
          const inner = pure('inner').fmap(value => {
            try { effectSamples({ nested: ['nested.wav'] }); } catch (_) {}
            return value;
          });
          new Pattern(state => inner.query(state))
        "#,
    )
    .expect("install nested native query");
    assert_eq!(effects, ScoreEffects::default());

    let error = runtime
        .query_cancellable(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            Duration::from_secs(1),
            &NOT_CANCELLED,
        )
        .expect_err("native Pattern.query recorded a sample effect during an engine query");
    assert_policy(error, "samples(...)");
    assert_eq!(
        evaluate(&runtime, "pure('after-native-query')").expect("native-query recovery"),
        ScoreEffects::default(),
        "the refused native query left an effect for a later score"
    );
}

#[test]
fn construction_time_pattern_queries_cannot_stage_effects() {
    let runtime = runtime();
    runtime
        .eval(
            "globalThis.effectSamples = globalThis.samples; globalThis.effectPreload = globalThis.preload;",
        )
        .expect("save effect bindings");

    let native_query_arc = evaluate(
        &runtime,
        r#"
          globalThis.nativeQueryArcRan = 0;
          pure('x').fmap(value => {
            globalThis.nativeQueryArcRan++;
            try { effectSamples({ eager: ['eager.wav'] }); } catch (_) {}
            return value;
          }).queryArc(0, 1);
          pure('accepted')
        "#,
    )
    .expect_err("native queryArc staged construction-time sample work");
    assert_policy(native_query_arc, "samples(...)");
    assert_eq!(runtime.get_number("nativeQueryArcRan"), Some(1.0));

    let native_query = evaluate(
        &runtime,
        r#"
          globalThis.nativeQueryRan = 0;
          const queried = pure('x').fmap(value => {
            globalThis.nativeQueryRan++;
            try { setCps(9); } catch (_) {}
            return value;
          });
          queried.query({ span: { begin: 0, end: 1 }, controls: {} });
          pure('accepted')
        "#,
    )
    .expect_err("native own query staged construction-time tempo work");
    assert_policy(native_query, "setCps");
    assert_eq!(runtime.get_number("nativeQueryRan"), Some(1.0));

    let authored_query = evaluate(
        &runtime,
        r#"
          globalThis.authoredQueryRan = 0;
          new Pattern((_state) => {
            globalThis.authoredQueryRan++;
            try { effectPreload('authored'); } catch (_) {}
            return [];
          }).queryArc(0, 1);
          pure('accepted')
        "#,
    )
    .expect_err("JavaScript-authored queryArc staged construction-time preload work");
    assert_policy(authored_query, "preload(...)");
    assert_eq!(runtime.get_number("authoredQueryRan"), Some(1.0));

    assert_eq!(
        evaluate(&runtime, "pure('after-construction-query')").expect("query recovery"),
        ScoreEffects::default(),
        "a refused construction-time query leaked work to the next score"
    );
}

#[test]
fn query_arc_arms_the_effect_boundary_before_mutable_query_surfaces() {
    let runtime = runtime();
    runtime
        .eval(
            "globalThis.effectSamples = globalThis.samples; globalThis.effectPreload = globalThis.preload;",
        )
        .expect("save effect bindings");

    for (name, effect) in [
        ("samples", "effectSamples({ getterLeak: ['getter.wav'] })"),
        ("preload", "effectPreload('getterLeak')"),
        ("tempo", "setCps(7)"),
    ] {
        let error = evaluate(
            &runtime,
            &format!(
                r#"
                  (() => {{
                    const queried = pure('getter');
                    const originalQuery = queried.query;
                    Object.defineProperty(queried, 'query', {{
                      configurable: true,
                      get() {{
                        try {{ {effect}; }} catch (_) {{}}
                        return originalQuery;
                      }},
                    }});
                    queried.queryArc(0, 1);
                    return pure('must-not-publish');
                  }})()
                "#,
            ),
        )
        .expect_err("query getter effect must refuse the evaluation");
        assert!(
            matches!(error, QueryError::Policy(_)),
            "{name} getter effect lost its policy channel: {error:?}"
        );
        assert_eq!(
            evaluate(&runtime, "pure('clean')").expect("getter-policy recovery"),
            ScoreEffects::default(),
            "{name} getter effect leaked into the next evaluation"
        );
    }

    for (name, constructor, effect) in [
        ("State", "State", "effectPreload('stateLeak')"),
        (
            "TimeSpan",
            "TimeSpan",
            "effectSamples({ spanLeak: ['span.wav'] })",
        ),
    ] {
        let error = evaluate(
            &runtime,
            &format!(
                r#"
                  (() => {{
                    const Original = globalThis.{constructor};
                    globalThis.{constructor} = function (...args) {{
                      try {{ {effect}; }} catch (_) {{}}
                      return new Original(...args);
                    }};
                    try {{
                      pure('constructor').queryArc(0, 1);
                    }} finally {{
                      globalThis.{constructor} = Original;
                    }}
                    return pure('must-not-publish');
                  }})()
                "#,
            ),
        )
        .expect_err("queryArc constructor effect must refuse the evaluation");
        assert!(
            matches!(error, QueryError::Policy(_)),
            "{name} constructor effect lost its policy channel: {error:?}"
        );
        assert_eq!(
            evaluate(&runtime, "pure('clean')").expect("constructor-policy recovery"),
            ScoreEffects::default(),
            "{name} constructor effect leaked into the next evaluation"
        );
    }

    runtime
        .eval("globalThis.logger = () => {};")
        .expect("install ordinary query logger");
    let ordinary = evaluate(
        &runtime,
        r#"
          const result = new Pattern(() => {
            throw new Error('ordinary query failure');
          }).queryArc(0, 1);
          globalThis.ordinaryQueryArcSilence = Number(
            Array.isArray(result) && result.length === 0
          );
          pure('accepted')
        "#,
    )
    .expect("ordinary queryArc throw remains catch-to-silence");
    assert_eq!(ordinary, ScoreEffects::default());
    assert_eq!(runtime.get_number("ordinaryQueryArcSilence"), Some(1.0));
}

#[test]
fn query_arc_preserves_logger_throws_and_cannot_launder_logger_effects() {
    let runtime = runtime();
    evaluate(&runtime, "pure('last-good')").expect("last-good score");

    runtime
        .eval("globalThis.logger = () => { throw new Error('logger-sentinel'); };")
        .expect("install throwing logger");
    let logger = evaluate(
        &runtime,
        r#"
          new Pattern(() => { throw new Error('query-sentinel'); }).queryArc(0, 1);
          pure('must-not-publish')
        "#,
    )
    .expect_err("queryArc must not swallow a logger throw");
    assert!(
        matches!(&logger, QueryError::Message(message) if message.contains("logger-sentinel")),
        "queryArc changed the logger-throw contract: {logger:?}"
    );
    assert_eq!(active_values(&runtime), ["last-good"]);

    runtime
        .eval(
            "globalThis.effectSamples = globalThis.samples; \
             globalThis.logger = () => { \
               try { effectSamples({ loggerLeak: ['logger.wav'] }); } catch (_) {} \
             };",
        )
        .expect("install effectful logger");
    let policy = evaluate(
        &runtime,
        r#"
          new Pattern(() => { throw new Error('query-sentinel'); }).queryArc(0, 1);
          pure('must-not-publish')
        "#,
    )
    .expect_err("a logger must not catch and hide a query-time host effect");
    assert_policy(policy, "samples(...)");
    assert_eq!(active_values(&runtime), ["last-good"]);
    assert_eq!(
        evaluate(&runtime, "pure('recovered')").expect("logger-policy recovery"),
        ScoreEffects::default(),
        "the refused logger effect escaped into a later evaluation"
    );

    for thrown in ["undefined", "null"] {
        let policy = evaluate(
            &runtime,
            &format!(
                r#"
                  new Pattern(() => {{
                    try {{ setCps(9); }} catch (_) {{}}
                    throw {thrown};
                  }}).queryArc(0, 1);
                  pure('must-not-publish')
                "#,
            ),
        )
        .expect_err("a non-object query throw must not hide a tempo refusal");
        assert_policy(policy, "setCps");
    }

    runtime
        .eval(
            "globalThis.logger = () => { \
               try { setCps(9); } catch (_) {} \
               throw new Error('logger-after-policy'); \
             };",
        )
        .expect("install effectful throwing logger");
    let policy = evaluate(
        &runtime,
        "new Pattern(() => { throw new Error('query-sentinel'); }).queryArc(0, 1); pure('must-not-publish')",
    )
    .expect_err("the operation-wide policy latch must outrank a logger throw");
    assert_policy(policy, "setCps");
}

#[test]
fn query_arc_catches_javascript_surface_type_errors() {
    let runtime = runtime();
    let effects = evaluate(
        &runtime,
        r#"
          const OriginalState = State;
          const OriginalTimeSpan = TimeSpan;
          const results = [];
          try {
            globalThis.State = null;
            results.push(pure(1).queryArc(0, 1));
            globalThis.State = OriginalState;
            globalThis.TimeSpan = null;
            results.push(pure(1).queryArc(0, 1));
          } finally {
            globalThis.State = OriginalState;
            globalThis.TimeSpan = OriginalTimeSpan;
          }
          const pattern = pure(1);
          pattern.query = 5;
          results.push(pattern.queryArc(0, 1));
          results.push(Pattern.prototype.queryArc.call(5, 0, 1));
          globalThis.__queryArcTypeErrors = Number(
            results.every(value => Array.isArray(value) && value.length === 0)
          );
          pure('accepted')
        "#,
    )
    .expect("ordinary JavaScript type errors remain catch-to-silence");
    assert_eq!(effects, ScoreEffects::default());
    assert_eq!(runtime.get_number("__queryArcTypeErrors"), Some(1.0));
}

/// A thrown value's own text reaches the report in every shape a score can
/// throw. The report never shows QuickJS's generic `Exception` text or the
/// default `[object Object]`.
#[test]
fn a_thrown_value_reads_its_own_text_in_every_shape() {
    let runtime = runtime();

    let error = evaluate(&runtime, "throw new Error(\"boom\")").expect_err("must throw");
    assert_eq!(error.to_string(), "Error: boom - line 1");

    let error = evaluate(&runtime, "throw new TypeError(\"bad\")").expect_err("must throw");
    assert_eq!(error.to_string(), "TypeError: bad - line 1");

    // The same words in backticks are an ordinary string too, and
    // AggregateError carries its message after the errors iterable.
    let error = evaluate(&runtime, "throw new Error(`backticked`)").expect_err("must throw");
    assert_eq!(error.to_string(), "Error: backticked - line 1");

    let error = evaluate(
        &runtime,
        "throw new AggregateError([new Error(\"x\")], \"many\")",
    )
    .expect_err("must throw");
    assert_eq!(error.to_string(), "AggregateError: many - line 1");

    // QuickJS's own non-standard constructor reads its message first, like
    // the standard family.
    let error = evaluate(&runtime, "throw new InternalError(\"inside\")").expect_err("must throw");
    assert_eq!(error.to_string(), "InternalError: inside - line 1");

    let error = evaluate(&runtime, "throw \"boom\"").expect_err("must throw");
    assert_eq!(error.to_string(), "boom");

    let error = evaluate(&runtime, "throw {message: \"custom\"}").expect_err("must throw");
    assert_eq!(error.to_string(), "custom");

    // A structured pattern has no single text of its own, and an object with
    // no text has none. Neither shows the default `[object Object]`.
    let error = evaluate(&runtime, "throw \"bd sd\"").expect_err("must throw");
    assert_eq!(error.to_string(), "a thrown object value");

    // An object that describes itself keeps its description: a custom
    // `toString` returning a real (single-quoted) string is real text, and
    // so is an array's join.
    let error =
        evaluate(&runtime, "throw {toString(){return 'self-described'}}").expect_err("must throw");
    assert_eq!(error.to_string(), "self-described");

    let error = evaluate(&runtime, "throw [3, 4]").expect_err("must throw");
    assert_eq!(error.to_string(), "3,4");
}

/// An error's message PROPERTY is read honestly: one set to `undefined` is
/// no message at all, one set to an object keeps the object's own text, and
/// a thrown object with a blank message still reports something.
#[test]
fn an_error_message_property_reads_honestly() {
    let runtime = runtime();

    let error = evaluate(
        &runtime,
        "const e = new Error(\"x\"); e.message = undefined; throw e",
    )
    .expect_err("must throw");
    assert_eq!(error.to_string(), "Error - line 1");

    let error = evaluate(
        &runtime,
        "const e = new Error(); e.message = {toString(){return 'boom'}}; throw e",
    )
    .expect_err("must throw");
    assert_eq!(error.to_string(), "Error: boom - line 1");

    let error = evaluate(&runtime, "throw {message: ''}").expect_err("must throw");
    assert_eq!(error.to_string(), "a thrown object value");
}

/// Describing a throw a symbol cannot satisfy never fails the turn or the
/// runtime: a symbol cannot be coerced to a string, so its description dies
/// mid-read, and the report falls back to naming the type while the runtime
/// stays usable for the next score.
#[test]
fn describing_an_undescribable_throw_names_the_type_and_spared_the_runtime() {
    let runtime = runtime();

    let error = evaluate(&runtime, "throw Symbol('x')").expect_err("must throw");
    assert_eq!(error.to_string(), "a thrown symbol value");

    evaluate(&runtime, "globalThis.__afterSymbol = 41 + 1; pure('after')")
        .expect("the runtime is still usable");
    assert_eq!(runtime.get_number("__afterSymbol"), Some(42.0));
}

/// An error the ENGINE raises - not one a score threw - is untouched by any
/// of the above: it never passes through a score's own `Error` constructor
/// or its mini-notation-compiled arguments.
#[test]
fn an_engine_raised_error_is_unchanged() {
    let runtime = runtime();
    let error = evaluate(&runtime, "null()").expect_err("calling null must throw");
    assert_eq!(error.to_string(), "TypeError: not a function - line 1");
}

#[test]
fn evaluation_cannot_open_an_effect_transaction_inside_a_query_scope() {
    let runtime = runtime();
    evaluate(&runtime, "pure('last-good')").expect("last-good score");

    let nested_score = runtime
        .with_active_scope_cancellable(Duration::from_secs(1), &NOT_CANCELLED, || {
            evaluate(
                &runtime,
                "globalThis.nestedScoreRan = 1; \
                 samples({ nestedScore: ['nested.wav'] }); pure('nested')",
            )
        })
        .expect_err("a score evaluation began inside a bounded query scope");
    assert_policy(nested_score, "pattern query");
    assert_eq!(runtime.get_number("nestedScoreRan"), None);
    assert_eq!(active_values(&runtime), ["last-good"]);

    let nested_setup = runtime
        .with_active_scope(|| {
            runtime.evaluate_prelude_with_effects(
                "globalThis.nestedSetupRan = 1; preload('nested');",
                &TranspileOptions::default(),
                Duration::from_secs(1),
            )
        })
        .expect_err("a setup evaluation began inside a legacy query scope");
    assert_policy(nested_setup, "pattern query");
    assert_eq!(runtime.get_number("nestedSetupRan"), None);
    assert_eq!(active_values(&runtime), ["last-good"]);

    let builder = runtime.builder();
    runtime
        .hold(&builder, pure(Value::Str("held".into())))
        .expect("hold benign nested-query pattern");
    runtime
        .install_query_binding()
        .expect("install nested query binding");
    let effects = evaluate(
        &runtime,
        "queryHeld(0, 0, 1); \
         samples({ accepted: ['accepted.wav'] }); \
         preload('accepted'); setCps(1.5); pure('accepted')",
    )
    .expect("a query inside an existing evaluation transaction remains supported");
    assert_eq!(effects.cps, Some(1.5));
    assert_eq!(
        effects.samples,
        [(r#"{"accepted":["accepted.wav"]}"#.to_string(), None)]
    );
    assert_eq!(effects.preload, ["accepted"]);
}

#[test]
fn direct_javascript_query_call_keeps_construction_time_semantics() {
    let runtime = runtime();
    runtime
        .eval("globalThis.effectPreload = globalThis.preload;")
        .expect("save preload binding");
    let effects = evaluate(
        &runtime,
        r#"
          let calls = 0;
          const query = (_state) => {
            calls++;
            effectPreload('direct-authored');
            return [];
          };
          const authored = new Pattern(query);
          authored.query({ span: { begin: 0, end: 1 }, controls: {} });
          globalThis.directAuthoredQueryIdentity = authored.query === query ? 1 : 0;
          globalThis.directAuthoredQueryCalls = calls;
          pure('accepted')
        "#,
    )
    .expect("direct authored function call is part of score construction");

    assert_eq!(effects.preload, ["direct-authored"]);
    assert_eq!(runtime.get_number("directAuthoredQueryIdentity"), Some(1.0));
    assert_eq!(runtime.get_number("directAuthoredQueryCalls"), Some(1.0));
}
