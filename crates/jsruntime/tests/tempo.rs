//! Native REPL tempo setters and their atomic score-effect boundary.

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

fn assert_policy(error: QueryError, needle: &str) {
    match error {
        QueryError::Policy(message) => {
            assert!(message.contains(needle), "wrong policy message: {message}");
            assert!(
                message.contains("--cps"),
                "tempo policy did not name the CLI alternative: {message}"
            );
        }
        other => panic!("tempo refusal lost its policy channel: {other:?}"),
    }
}

#[test]
fn repl_setters_pin_alias_identity_reflection_order_and_silence_return() {
    let runtime = runtime();
    let effects = evaluate(
        &runtime,
        r#"
          globalThis.__tempoAliases = Number(
            setCps === setcps && setCpm === setcpm && setCps !== setCpm
            && setCps === rustelScope.setCps
            && setcps === rustelScope.setcps
            && setCpm === rustelScope.setCpm
            && setcpm === rustelScope.setcpm
          );
          globalThis.__tempoNames = [setCps.name, setcps.name, setCpm.name, setcpm.name].join('|');
          globalThis.__tempoLengths = [setCps.length, setcps.length, setCpm.length, setcpm.length].join('|');
          globalThis.__tempoOrder = Object.keys(rustelScope)
            .filter(name => ['setCps', 'setcps', 'setCpm', 'setcpm'].includes(name))
            .join('|');
          globalThis.__tempoDescriptors = Number(
            ['setCps', 'setcps', 'setCpm', 'setcpm'].every(name => {
              const global = Object.getOwnPropertyDescriptor(globalThis, name);
              const scoped = Object.getOwnPropertyDescriptor(rustelScope, name);
              return global.writable && global.enumerable && global.configurable
                && scoped.writable && scoped.enumerable && scoped.configurable;
            })
          );
          globalThis.__tempoArrow = Number(
            setCps.prototype === undefined && setCpm.prototype === undefined
            && (() => { try { new setCps(1); return false; } catch (error) {
              return error instanceof TypeError;
            } })()
            && (() => { try { new setCpm(60); return false; } catch (error) {
              return error instanceof TypeError;
            } })()
          );
          globalThis.__savedSetCps = setCps;
          globalThis.__savedSetCpm = setCpm;
          globalThis.__savedScopeSetCps = rustelScope.setCps;
          globalThis.__tempoSilence = Number(setCps(1) === silence);
          pure('shape')
        "#,
    )
    .expect("shape score");
    assert_eq!(effects.cps, Some(1.0));
    assert_eq!(runtime.get_number("__tempoAliases"), Some(1.0));
    assert_eq!(
        runtime.get_string("__tempoNames").as_deref(),
        Some("setCps|setCps|setCpm|setCpm")
    );
    assert_eq!(
        runtime.get_string("__tempoLengths").as_deref(),
        Some("1|1|1|1")
    );
    assert_eq!(
        runtime.get_string("__tempoOrder").as_deref(),
        Some("setCps|setcps|setCpm|setcpm")
    );
    assert_eq!(runtime.get_number("__tempoDescriptors"), Some(1.0));
    assert_eq!(runtime.get_number("__tempoArrow"), Some(1.0));
    assert_eq!(runtime.get_number("__tempoSilence"), Some(1.0));

    let effects = evaluate(
        &runtime,
        "globalThis.__tempoStable = Number(setCps === __savedSetCps && setCpm === __savedSetCpm && rustelScope.setCps === __savedScopeSetCps); globalThis.__tempoCpmSilence = Number(setcpm(120) === silence); pure('stable')",
    )
    .expect("second score");
    assert_eq!(effects.cps, Some(2.0));
    assert_eq!(runtime.get_number("__tempoStable"), Some(1.0));
    assert_eq!(runtime.get_number("__tempoCpmSilence"), Some(1.0));
}

#[test]
fn score_reinjection_repairs_public_slots_without_recreating_canonical_functions() {
    let runtime = runtime();
    evaluate(
        &runtime,
        "globalThis.__canonicalSetCps = setCps; globalThis.__canonicalSetCpm = setCpm; globalThis.__canonicalScope = rustelScope; globalThis.__canonicalSilence = silence; pure('root')",
    )
    .unwrap();
    runtime
        .eval(
            "Object.defineProperty(globalThis, 'setCps', { value: () => 'replacement', writable: true, enumerable: false, configurable: true }); delete globalThis.setcps; globalThis.cps = () => 'replacement'; Object.defineProperty(__canonicalScope, 'setCpm', { value: () => 'replacement', writable: true, enumerable: false, configurable: true }); delete __canonicalScope.setcpm; delete __canonicalScope.cps; globalThis.silence = 'global-decoy'; __canonicalScope.silence = 'scope-decoy'; Object.defineProperty(__canonicalSetCps, 'name', { value: 'tempoMutated' });",
        )
        .expect("mutate public setter slots");

    let effects = evaluate(
        &runtime,
        "globalThis.__repaired = Number(setCps === __canonicalSetCps && setcps === __canonicalSetCps && cps === __canonicalSetCps && __canonicalScope.cps === __canonicalSetCps && __canonicalScope.setCpm === __canonicalSetCpm && __canonicalScope.setcpm === __canonicalSetCpm && setCps.name === 'tempoMutated'); globalThis.__ordinaryAssignmentFlags = Number(!Object.getOwnPropertyDescriptor(globalThis, 'setCps').enumerable && !Object.getOwnPropertyDescriptor(__canonicalScope, 'setCpm').enumerable && Object.getOwnPropertyDescriptor(globalThis, 'setcps').writable && Object.getOwnPropertyDescriptor(globalThis, 'setcps').enumerable && Object.getOwnPropertyDescriptor(globalThis, 'setcps').configurable && Object.getOwnPropertyDescriptor(__canonicalScope, 'setcpm').writable && Object.getOwnPropertyDescriptor(__canonicalScope, 'setcpm').enumerable && Object.getOwnPropertyDescriptor(__canonicalScope, 'setcpm').configurable); globalThis.__closedSilence = Number(cps(1.25) === __canonicalSilence); pure('repaired')",
    )
    .expect("reinject score");
    assert_eq!(effects.cps, Some(1.25));
    assert_eq!(runtime.get_number("__repaired"), Some(1.0));
    assert_eq!(runtime.get_number("__ordinaryAssignmentFlags"), Some(1.0));
    assert_eq!(runtime.get_number("__closedSilence"), Some(1.0));

    evaluate(
        &runtime,
        "delete globalThis.setcps; delete __canonicalScope.setcpm; globalThis.__deletedInTurn = Number(!Object.hasOwn(globalThis, 'setcps') && !Object.hasOwn(__canonicalScope, 'setcpm')); pure('deleted')",
    )
    .expect("delete during score");
    assert_eq!(runtime.get_number("__deletedInTurn"), Some(1.0));
    runtime
        .eval(
            "globalThis.__stillDeleted = Number(!Object.hasOwn(globalThis, 'setcps') && !Object.hasOwn(__canonicalScope, 'setcpm'));",
        )
        .expect("inspect between score turns");
    assert_eq!(runtime.get_number("__stillDeleted"), Some(1.0));

    evaluate(
        &runtime,
        "globalThis.__restoredNextTurn = Number(setcps === __canonicalSetCps && __canonicalScope.setcpm === __canonicalSetCpm); pure('restored')",
    )
    .expect("restore next score");
    assert_eq!(runtime.get_number("__restoredNextTurn"), Some(1.0));

    runtime
        .eval("globalThis.rustelScope = { replaced: true };")
        .expect("replace public scope slot");
    evaluate(
        &runtime,
        "globalThis.__scopeNotRestored = Number(rustelScope.replaced === true && __canonicalScope.setCps === __canonicalSetCps); setCps(1); pure('scope')",
    )
    .expect("canonical scope reinjection without global scope repair");
    assert_eq!(runtime.get_number("__scopeNotRestored"), Some(1.0));
}

#[test]
fn irreversible_public_setter_descriptors_do_not_brick_the_next_save() {
    let runtime = runtime();
    let first = evaluate(
        &runtime,
        r#"
          globalThis.__tempoPoisonSetterHits = 0;
          Object.defineProperty(globalThis, 'setCps', {
            value: null,
            writable: false,
            enumerable: true,
            configurable: false,
          });
          Object.defineProperty(rustelScope, 'setCps', {
            value: null,
            writable: false,
            enumerable: true,
            configurable: false,
          });
          const poisonedAccessor = {
            get() { return null; },
            set(_) {
              globalThis.__tempoPoisonSetterHits++;
              throw new Error('public tempo setter was invoked');
            },
            enumerable: true,
            configurable: false,
          };
          Object.defineProperty(globalThis, 'setcps', poisonedAccessor);
          Object.defineProperty(rustelScope, 'setcps', poisonedAccessor);
          pure('poisoned')
        "#,
    )
    .expect("the poisoning save itself is otherwise valid");
    assert_eq!(first, ScoreEffects::default());

    let second = evaluate(
        &runtime,
        "globalThis.__tempoRecoveryRan = 1; pure('recovered')",
    )
    .expect("irreversible public descriptors must not brick the next save");
    assert_eq!(second, ScoreEffects::default());
    assert_eq!(runtime.get_number("__tempoRecoveryRan"), Some(1.0));
    assert_eq!(
        runtime.get_number("__tempoPoisonSetterHits"),
        Some(0.0),
        "host reinjection invoked a score-installed accessor"
    );
}

#[test]
fn successful_effects_use_unpure_js_cpm_coercion_and_last_call_wins() {
    let runtime = runtime();
    let effects = evaluate(
        &runtime,
        "setCps(pure(.75)); setcps(1); setCpm('120'); pure('last')",
    )
    .expect("valid setters");
    assert_eq!(effects.cps, Some(2.0));

    let effects = evaluate(&runtime, "setCps({ _Pattern: true }); pure('default')")
        .expect("patternish missing __pure uses Cyclist default");
    assert_eq!(effects.cps, Some(0.5));

    let omitted = evaluate(&runtime, "setCps(); pure('unreachable')")
        .expect_err("omitted argument reads _Pattern from undefined on strudel.cc");
    assert!(
        matches!(omitted, QueryError::Message(ref message) if message.contains("_Pattern")),
        "omitted setCps stopped being an ordinary strudel.cc-shaped throw: {omitted:?}"
    );

    let null = evaluate(&runtime, "setCps(null); pure('unreachable')")
        .expect_err("null reads _Pattern before native tempo validation");
    assert!(
        matches!(null, QueryError::Message(ref message) if message.contains("of null") && message.contains("_Pattern")),
        "null setCps lost its JavaScript property-access error: {null:?}"
    );

    let invalid_cpm = evaluate(&runtime, "setCpm({ _Pattern: true }); pure('unreachable')")
        .expect_err("undefined divided by 60 is not a valid native tempo");
    assert_policy(invalid_cpm, "finite tempo");

    let effects = evaluate(
        &runtime,
        "const OriginalObject = Object; try { globalThis.Object = () => ({ _Pattern: true, __pure: 0.001 }); setCps(0.5); } finally { globalThis.Object = OriginalObject; } pure('boxed')",
    )
    .expect("primitive property access must not consult globalThis.Object");
    assert_eq!(effects.cps, Some(0.5));
}

#[test]
fn query_scope_policy_precedes_tempo_value_validation() {
    let runtime = runtime();
    let error = evaluate(
        &runtime,
        r#"
          new Pattern(() => {
            try { setCps('invalid'); } catch (_) {}
            return [];
          }).queryArc(0, 1);
          pure('must-not-publish')
        "#,
    )
    .expect_err("query-time tempo changes must use the scope refusal");
    assert_policy(error, "Session-owned");
}

#[test]
fn invalid_or_out_of_scope_tempo_is_structural_and_never_laundered() {
    let runtime = runtime();
    evaluate(&runtime, "pure('baseline')").unwrap();

    for source in [
        "setCps(0); pure('zero')",
        "setCps(-1); pure('negative')",
        "setCps(NaN); pure('nan')",
        "setCps(Infinity); pure('infinity')",
        "setCps('2'); pure('string')",
    ] {
        let error = evaluate(&runtime, source).expect_err("invalid tempo must refuse");
        assert_policy(error, "finite tempo");
        assert_eq!(
            evaluate(&runtime, "pure('clean')").unwrap(),
            ScoreEffects::default(),
            "invalid effect leaked into the next score"
        );
    }

    let legacy = runtime
        .evaluate_score_cancellable(
            "try { setcps(1); } catch (_) {} pure('caught')",
            &TranspileOptions::default(),
            Duration::from_secs(1),
            &NOT_CANCELLED,
        )
        .expect_err("source-compatible bounded evaluator has no effect owner");
    assert_policy(legacy, "Session-owned");

    let raw = runtime
        .eval("try { setCps(1); } catch (_) {}")
        .expect_err("raw eval must consume a caught policy refusal");
    assert!(
        raw.contains("--cps"),
        "raw refusal was not actionable: {raw}"
    );

    let prebake = runtime
        .evaluate_prelude_cancellable(
            "try { setCpm(120); } catch (_) {}",
            &TranspileOptions::default(),
            Duration::from_secs(1),
            &NOT_CANCELLED,
        )
        .expect_err("prebake must consume a caught policy refusal");
    assert_policy(prebake, "Session-owned");
}

#[test]
fn nested_query_held_cannot_clear_a_raw_eval_policy_owner() {
    let runtime = runtime();
    let builder = runtime.builder();
    runtime
        .hold(&builder, pure(Value::Str("held".into())))
        .expect("hold clean pattern");
    runtime
        .install_query_binding()
        .expect("install nested query binding");

    let error = runtime
        .eval("try { setCps(1); } catch (_) {} globalThis.__nestedTempoQuery = queryHeld(0, 0, 1);")
        .expect_err("clean nested query must not launder the raw policy latch");
    assert!(
        error.contains("--cps"),
        "nested raw-eval refusal was not actionable: {error}"
    );
    assert_eq!(
        runtime.get_string("__nestedTempoQuery").as_deref(),
        Some("held"),
        "the clean nested query itself stopped running"
    );

    runtime
        .eval("globalThis.__afterNestedTempoPolicy = 1;")
        .expect("raw policy owner cleaned up for the next eval");
    assert_eq!(runtime.get_number("__afterNestedTempoPolicy"), Some(1.0));
}

#[test]
fn policy_first_raised_inside_query_held_survives_raw_and_legacy_owners() {
    let runtime = runtime();
    runtime
        .evaluate_score("pure('baseline')", &TranspileOptions::default())
        .expect("install baseline active score");
    let mut builder = runtime.builder();
    let callback = builder.callback(
        &runtime,
        "(_owner) => value => { try { setCps(1); } catch (_) {} return value; }",
    );
    let held = pure(Value::Str("held".into())).fmap_js(callback);
    runtime
        .hold(&builder, held)
        .expect("hold callback-bearing pattern");
    runtime
        .install_query_binding()
        .expect("install nested query binding");

    let raw = runtime
        .eval("globalThis.__callbackTempoQuery = queryHeld(0, 0, 1);")
        .expect_err("nested callback policy must escape its raw owner");
    assert!(
        raw.contains("--cps"),
        "raw nested-callback policy was not actionable: {raw}"
    );
    assert_eq!(
        runtime.get_string("__callbackTempoQuery").as_deref(),
        Some(""),
        "queryHeld stopped preserving its established error fallback"
    );

    let legacy = runtime
        .evaluate_score(
            "queryHeld(0, 0, 1); pure('must-not-publish')",
            &TranspileOptions::default(),
        )
        .expect_err("nested callback policy must escape legacy score evaluation");
    assert!(
        legacy.contains("--cps"),
        "legacy nested-callback policy was not actionable: {legacy}"
    );
    let active = runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("baseline remains queryable after refused legacy score");
    assert_eq!(active[0].value.show(), "baseline");

    runtime
        .eval("globalThis.__afterCallbackTempoPolicy = 1;")
        .expect("policy owner recovered after nested callback refusal");
    assert_eq!(runtime.get_number("__afterCallbackTempoPolicy"), Some(1.0));
}

#[test]
fn query_time_setter_is_policy_not_plausible_silence() {
    let runtime = runtime();
    let effects = evaluate(
        &runtime,
        "pure('x').fmap(x => { try { setcps(1); } catch (_) {} return x; })",
    )
    .expect("install query-time callback");
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
        .expect_err("direct query must not accept callback silence");
    assert_policy(direct, "Session-owned");

    let pattern = runtime.active_pattern().expect("active pattern");
    let scheduled = runtime
        .with_active_scope_cancellable(Duration::from_secs(1), &NOT_CANCELLED, || {
            pattern.try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
        })
        .expect_err("scheduler-style query must stop before accepting silence");
    assert_policy(scheduled, "Session-owned");
}

#[test]
fn failed_cancelled_deadline_pending_and_heap_turns_discard_staged_effects() {
    let runtime = runtime();

    let thrown = evaluate(
        &runtime,
        "setCps(2); (() => { throw new Error('tempo-boom'); })(); pure('never')",
    )
    .expect_err("throw after setter");
    assert!(matches!(thrown, QueryError::Message(message) if message.contains("tempo-boom")));
    assert_eq!(
        evaluate(&runtime, "pure('after-throw')").unwrap(),
        ScoreEffects::default()
    );

    let cancelled = AtomicBool::new(true);
    let cancelled_error = runtime
        .evaluate_score_with_effects_cancellable(
            "setCps(3); pure('cancelled')",
            &TranspileOptions::default(),
            Duration::from_secs(1),
            &cancelled,
        )
        .expect_err("pre-cancelled score");
    assert!(matches!(
        cancelled_error,
        QueryError::Limit(QueryLimit::Cancelled)
    ));
    assert_eq!(
        evaluate(&runtime, "pure('after-cancel')").unwrap(),
        ScoreEffects::default()
    );

    let deadline = runtime
        .evaluate_score_with_effects_cancellable(
            "setCps(4); while (true) {} pure('deadline')",
            &TranspileOptions::default(),
            Duration::from_millis(20),
            &NOT_CANCELLED,
        )
        .expect_err("deadline after setter");
    assert!(matches!(
        deadline,
        QueryError::Limit(QueryLimit::JsCpuDeadline { .. })
    ));
    assert_eq!(
        evaluate(&runtime, "pure('after-deadline')").unwrap(),
        ScoreEffects::default()
    );

    let pending = evaluate(
        &runtime,
        "setCps(5); Promise.resolve().then(() => {}); pure('pending')",
    )
    .expect_err("pending job after setter");
    assert!(matches!(
        pending,
        QueryError::Limit(QueryLimit::JsPendingJobs)
    ));
    assert_eq!(
        evaluate(&runtime, "pure('after-pending')").unwrap(),
        ScoreEffects::default()
    );

    let ceiling = runtime.heap_live() + 1024 * 1024;
    runtime
        .set_memory_limit(ceiling)
        .expect("install bounded heap headroom");
    let heap = evaluate(
        &runtime,
        "setCps(6); globalThis.__tempoHeapHog = new Array(2_000_000).fill(7); pure('heap')",
    )
    .expect_err("heap refusal after setter");
    assert!(matches!(heap, QueryError::Limit(QueryLimit::HostMemory)));
    assert_eq!(
        evaluate(&runtime, "pure('after-heap')").unwrap(),
        ScoreEffects::default()
    );
}
