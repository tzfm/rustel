//! Native REPL tempo setters and their atomic score-effect boundary.

use std::sync::atomic::AtomicBool;
use std::time::Duration;

use rustel_jsruntime::{JsRuntime, QueryError, ScoreEffects};
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
fn score_reinjection_repairs_public_slots_without_recreating_canonical_functions() {
    let runtime = runtime();
    evaluate(
        &runtime,
        "globalThis.__canonicalSetCps = setCps; globalThis.__canonicalSetCpm = setCpm; globalThis.__canonicalCps = cps; globalThis.__canonicalScope = rustelScope; globalThis.__canonicalSilence = silence; pure('root')",
    )
    .unwrap();
    runtime
        .eval(
            "Object.defineProperty(globalThis, 'setCps', { value: () => 'replacement', writable: true, enumerable: false, configurable: true }); delete globalThis.setcps; globalThis.cps = () => 'replacement'; Object.defineProperty(__canonicalScope, 'setCpm', { value: () => 'replacement', writable: true, enumerable: false, configurable: true }); delete __canonicalScope.setcpm; delete __canonicalScope.cps; globalThis.silence = 'global-decoy'; __canonicalScope.silence = 'scope-decoy'; Object.defineProperty(__canonicalSetCps, 'name', { value: 'tempoMutated' });",
        )
        .expect("mutate public setter slots");

    let effects = evaluate(
        &runtime,
        "globalThis.__repaired = Number(setCps === __canonicalSetCps && setcps === __canonicalSetCps && cps === __canonicalCps && __canonicalScope.cps === __canonicalCps && __canonicalScope.setCpm === __canonicalSetCpm && __canonicalScope.setcpm === __canonicalSetCpm && setCps.name === 'tempoMutated'); globalThis.__ordinaryAssignmentFlags = Number(!Object.getOwnPropertyDescriptor(globalThis, 'setCps').enumerable && !Object.getOwnPropertyDescriptor(__canonicalScope, 'setCpm').enumerable && Object.getOwnPropertyDescriptor(globalThis, 'setcps').writable && Object.getOwnPropertyDescriptor(globalThis, 'setcps').enumerable && Object.getOwnPropertyDescriptor(globalThis, 'setcps').configurable && Object.getOwnPropertyDescriptor(__canonicalScope, 'setcpm').writable && Object.getOwnPropertyDescriptor(__canonicalScope, 'setcpm').enumerable && Object.getOwnPropertyDescriptor(__canonicalScope, 'setcpm').configurable); globalThis.__closedSilence = Number(cps(1.25) === __canonicalSilence); pure('repaired')",
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
