use rustel_core::compose::Alignment;
use rustel_core::rng::RngMode;
use rustel_core::{JoinMode, OrderedMap, PickIndexMode, Value, live_node_count};
use rustel_fraction::Fraction;
use rustel_jsruntime::JsRuntime;
use rustel_jsruntime::Slot;
use rustel_transpiler::TranspileOptions;

static SETTINGS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
const THREAD_LOCAL_RUNTIME_CHILD: &str = "RUSTEL_THREAD_LOCAL_RUNTIME_CHILD";

thread_local! {
    static THREAD_LOCAL_RUNTIME: std::cell::RefCell<Option<JsRuntime>> = const {
        std::cell::RefCell::new(None)
    };
}

fn serialize_settings_tests() -> std::sync::MutexGuard<'static, ()> {
    SETTINGS_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn runtime_with_native_surface() -> JsRuntime {
    let runtime = JsRuntime::new().expect("runtime");
    runtime
        .install_semantic_bindings()
        .expect("semantic bindings");
    runtime
        .install_voicings_prebake()
        .expect("voicings bindings");
    runtime
}

#[test]
fn a_thread_local_runtime_drops_after_the_settings_stack() {
    if std::env::var_os(THREAD_LOCAL_RUNTIME_CHILD).is_some() {
        THREAD_LOCAL_RUNTIME.with(|slot| {
            let runtime = runtime_with_native_surface();
            *slot.borrow_mut() = Some(runtime);
        });
        return;
    }

    let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .arg("--exact")
        .arg("a_thread_local_runtime_drops_after_the_settings_stack")
        .arg("--nocapture")
        .env(THREAD_LOCAL_RUNTIME_CHILD, "1")
        .output()
        .expect("run thread-local runtime child");
    assert!(
        output.status.success(),
        "thread-local runtime teardown failed with {}:\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn default_voicing_size(runtime: &JsRuntime) -> usize {
    let mut controls = OrderedMap::new();
    controls.insert("chord".into(), Value::Str("C7".into()));
    runtime.with_runtime_settings(|| {
        rustel_core::voicings::render_voicing(&controls)
            .expect("configured C7 voicing")
            .len()
    })
}

fn current_settings(runtime: &JsRuntime) -> (RngMode, Alignment, usize) {
    (
        runtime.with_runtime_settings(rustel_core::rng::rng_mode),
        runtime.with_runtime_settings(rustel_core::compose::default_alignment),
        default_voicing_size(runtime),
    )
}

fn active_notes(runtime: &JsRuntime) -> Vec<String> {
    runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("query active graph")
        .into_iter()
        .map(|hap| {
            hap.value
                .get("note")
                .and_then(Value::as_str)
                .expect("voicing hap note")
                .to_owned()
        })
        .collect()
}

fn active_string_notes(runtime: &JsRuntime) -> Vec<String> {
    runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("query active graph")
        .into_iter()
        .map(|hap| hap.value.as_str().expect("voicings hap string").to_owned())
        .collect()
}

#[test]
fn deadline_scope_binds_the_runtime_settings() {
    let _serial = serialize_settings_tests();
    let ambient = rustel_core::settings::RuntimeSettings::default();
    ambient.with(|| {
        rustel_core::rng::use_rng(RngMode::Legacy);
        rustel_core::compose::set_default_alignment(Alignment::In);
        rustel_core::voicings::set_default_voicings("lefthand");
    });
    let runtime = JsRuntime::new().expect("runtime");
    runtime.with_runtime_settings(|| {
        rustel_core::rng::use_rng(RngMode::Precise);
        rustel_core::compose::set_default_alignment(Alignment::Out);
        rustel_core::voicings::set_default_voicings("guidetones");
    });

    let observed = ambient.with(|| {
        runtime.with_deadline(std::time::Duration::from_secs(1), || {
            let mut controls = OrderedMap::new();
            controls.insert("chord".into(), Value::Str("C7".into()));
            (
                rustel_core::rng::rng_mode(),
                rustel_core::compose::default_alignment(),
                rustel_core::voicings::render_voicing(&controls)
                    .expect("configured C7 voicing")
                    .len(),
            )
        })
    });
    assert_eq!(observed, (RngMode::Precise, Alignment::Out, 2));
}

#[test]
fn mutable_module_settings_are_isolated_per_runtime() {
    let _serial = serialize_settings_tests();
    let first = runtime_with_native_surface();
    let second = runtime_with_native_surface();

    first
        .evaluate_score(
            "useRNG('precise'); \
             setDefaultJoin('out'); \
             setDefaultVoicings('guidetones'); \
             pure('first')",
            &TranspileOptions::default(),
        )
        .expect("configure first runtime");
    second
        .evaluate_score(
            "useRNG('legacy'); \
             setDefaultJoin('mix'); \
             setDefaultVoicings('lefthand'); \
             pure('second')",
            &TranspileOptions::default(),
        )
        .expect("configure second runtime");

    assert_eq!(
        first.with_runtime_settings(rustel_core::rng::rng_mode),
        RngMode::Precise
    );
    assert_eq!(
        first.with_runtime_settings(rustel_core::compose::default_alignment),
        Alignment::Out
    );
    assert_eq!(default_voicing_size(&first), 2);

    assert_eq!(
        second.with_runtime_settings(rustel_core::rng::rng_mode),
        RngMode::Legacy
    );
    assert_eq!(
        second.with_runtime_settings(rustel_core::compose::default_alignment),
        Alignment::Mix
    );
    assert_eq!(default_voicing_size(&second), 4);

    // Re-enter the first runtime after observing the second. The second
    // runtime's most recent mutations must not have changed its state.
    assert_eq!(default_voicing_size(&first), 2);
    assert_eq!(
        first.with_runtime_settings(rustel_core::rng::rng_mode),
        RngMode::Precise
    );
    first.clear_active();
    second.clear_active();
}

#[test]
fn default_voicing_setters_return_the_original_javascript_argument() {
    let _serial = serialize_settings_tests();
    let runtime = runtime_with_native_surface();
    runtime
        .eval(
            r#"
              let conversions = 0;
              const globalDict = {
                toString() { conversions += 1; return 'guidetones'; }
              };
              const scopeDict = {
                toString() { conversions += 1; return 'lefthand'; }
              };
              globalThis.globalReturnKeptIdentity = String(
                setDefaultVoicings(globalDict) === globalDict
              );
              globalThis.scopeReturnKeptIdentity = String(
                rustelScope.setDefaultVoicings(scopeDict) === scopeDict
              );
              globalThis.defaultVoicingConversions = conversions;
              globalThis.resetVoicingsReturnsUndefined = String(
                resetVoicings() === undefined
              );
              setDefaultVoicings('lefthand');
            "#,
        )
        .expect("call both default-voicing setters");

    assert_eq!(
        runtime.get_string("globalReturnKeptIdentity").as_deref(),
        Some("true")
    );
    assert_eq!(
        runtime.get_string("scopeReturnKeptIdentity").as_deref(),
        Some("true")
    );
    assert_eq!(runtime.get_number("defaultVoicingConversions"), Some(0.0));
    assert_eq!(
        runtime
            .get_string("resetVoicingsReturnsUndefined")
            .as_deref(),
        Some("true")
    );
    assert_eq!(default_voicing_size(&runtime), 4);
}

#[test]
fn scoped_voicing_preserves_registered_and_object_dictionary_defaults() {
    let _serial = serialize_settings_tests();
    let runtime = runtime_with_native_surface();

    runtime
        .evaluate_score(
            "setDefaultVoicings({'7':['0 4 7']}); rustelScope.voicing(pure('C7'))",
            &TranspileOptions::default(),
        )
        .expect("object dictionary score");
    assert_eq!(active_notes(&runtime), ["C4", "E4", "G4"]);

    runtime
        .evaluate_score(
            "registerVoicings('mine', {'7':['0 4 7']}); setDefaultVoicings('mine'); rustelScope.voicing(pure('C7'))",
            &TranspileOptions::default(),
        )
        .expect("registered dictionary score");
    assert_eq!(active_notes(&runtime), ["C4", "E4", "G4"]);

    runtime
        .evaluate_score(
            "setDefaultVoicings('lefthand'); rustelScope.voicing(pure({ chord: 'C7', dictionary: {'7':['0 4 7']} }))",
            &TranspileOptions::default(),
        )
        .expect("explicit dictionary score");
    assert_eq!(active_notes(&runtime), ["C4", "E4", "G4"]);

    runtime
        .evaluate_score(
            "setDefaultVoicings({'7':['0 4 7']}); resetVoicings(); rustelScope.voicing(pure('C7'))",
            &TranspileOptions::default(),
        )
        .expect("reset dictionary score");
    assert_eq!(active_notes(&runtime).len(), 5);
    runtime.clear_active();
}

#[test]
fn object_voicing_default_mutation_is_visible_to_an_existing_graph() {
    let _serial = serialize_settings_tests();
    let runtime = runtime_with_native_surface();
    runtime
        .evaluate_score(
            "globalThis.liveVoicings = {'7':['0 4 7']}; setDefaultVoicings(liveVoicings); pure('C7').voicing()",
            &TranspileOptions::default(),
        )
        .expect("object dictionary score");
    assert_eq!(active_notes(&runtime), ["C4", "E4", "G4"]);

    runtime
        .eval("liveVoicings['7'] = ['0 7']")
        .expect("mutate the retained default object");
    assert_eq!(active_notes(&runtime), ["C4", "G4"]);
    runtime.clear_active();
}

#[test]
fn voicings_last_voicing_is_runtime_local_and_resettable() {
    let _serial = serialize_settings_tests();
    let first = runtime_with_native_surface();
    let second = runtime_with_native_surface();
    let register = "registerVoicings('mine', {'':['1P 3M 5P','3M 5P 8P']}, {range:['C3','C5']}); ";

    first
        .evaluate_score(
            &format!("{register}fastcat(pure('C'), pure('G'), pure('C')).voicings('mine')"),
            &TranspileOptions::default(),
        )
        .expect("voice-leading sequence");
    assert_eq!(
        active_string_notes(&first),
        ["C3", "E3", "G3", "G3", "B3", "D4", "E3", "G3", "C4"]
    );

    second
        .evaluate_score(
            &format!("{register}pure('C').voicings('mine')"),
            &TranspileOptions::default(),
        )
        .expect("independent runtime voicing");
    assert_eq!(active_string_notes(&second), ["C3", "E3", "G3"]);

    first
        .evaluate_score("pure('C').voicings('mine')", &TranspileOptions::default())
        .expect("continue first runtime voice leading");
    assert_eq!(active_string_notes(&first), ["E3", "G3", "C4"]);
    first
        .evaluate_score(
            "resetVoicings(); pure('C').voicings('mine')",
            &TranspileOptions::default(),
        )
        .expect("reset first runtime voice leading");
    assert_eq!(active_string_notes(&first), ["C3", "E3", "G3"]);
    first.clear_active();
    second.clear_active();
}

#[test]
fn scoped_voicing_does_not_trust_mutable_map_prototypes() {
    let _serial = serialize_settings_tests();
    let runtime = runtime_with_native_surface();
    runtime
        .eval(
            r#"
              Map.prototype.has = () => { throw new Error('poisoned has'); };
              Map.prototype.get = () => { throw new Error('poisoned get'); };
              Map.prototype.set = () => { throw new Error('poisoned set'); };
              Map.prototype.delete = () => { throw new Error('poisoned delete'); };
              Map.prototype.keys = () => { throw new Error('poisoned keys'); };
            "#,
        )
        .expect("poison Map prototype");
    runtime
        .evaluate_score(
            "setDefaultVoicings({'7':['0 4 7']}); rustelScope.voicing(pure('C7'))",
            &TranspileOptions::default(),
        )
        .expect("object dictionary with poisoned Map prototype");
    assert_eq!(active_notes(&runtime), ["C4", "E4", "G4"]);
    runtime
        .evaluate_score(
            "setDefaultVoicings('ireal'); rustelScope.voicing(pure('C7'))",
            &TranspileOptions::default(),
        )
        .expect("retire object dictionary with poisoned Map prototype");
    assert_eq!(active_notes(&runtime).len(), 5);
    runtime.clear_active();
}

#[test]
fn registered_voicing_names_cannot_collide_with_object_keys() {
    let _serial = serialize_settings_tests();
    let runtime = runtime_with_native_surface();
    runtime
        .evaluate_score(
            "setDefaultVoicings({'7':['0 4 7']}); rustelScope.voicing(pure('C7'))",
            &TranspileOptions::default(),
        )
        .expect("object dictionary score");
    let retained_object = runtime.snapshot_published_runtime_settings();

    runtime
        .evaluate_score(
            "registerVoicings('\\0rustel-private-voicing-default:0', {'7':['0 7']}); setDefaultVoicings('\\0rustel-private-voicing-default:0'); rustelScope.voicing(pure('C7'))",
            &TranspileOptions::default(),
        )
        .expect("colliding registered dictionary score");
    assert_eq!(active_notes(&runtime), ["C4", "G4"]);
    drop(retained_object);
    runtime.clear_active();
}

#[test]
fn scoped_voicing_setters_take_effect_inside_the_current_query() {
    let _serial = serialize_settings_tests();
    let runtime = runtime_with_native_surface();
    runtime
        .evaluate_score(
            "setDefaultVoicings('guidetones'); rustelScope.voicing(fastcat(pure('C7').fmap(x => { setDefaultVoicings('lefthand'); return x; }), pure('C7')))",
            &TranspileOptions::default(),
        )
        .expect("query-time named default score");
    assert_eq!(
        active_notes(&runtime),
        ["Bb3", "D4", "E4", "A4", "Bb3", "D4", "E4", "A4"]
    );

    runtime
        .evaluate_score(
            "setDefaultVoicings({'7':['0 4 7']}); rustelScope.voicing(fastcat(pure('C7').fmap(x => { resetVoicings(); return x; }), pure('C7')))",
            &TranspileOptions::default(),
        )
        .expect("query-time reset score");
    assert_eq!(
        active_notes(&runtime),
        ["E3", "Bb3", "E4", "G4", "C5", "E3", "Bb3", "E4", "G4", "C5"]
    );
    runtime.clear_active();
}

#[test]
fn nested_scoped_voicing_keeps_an_inner_default_change() {
    let _serial = serialize_settings_tests();
    let runtime = runtime_with_native_surface();
    runtime
        .evaluate_score(
            "setDefaultVoicings('guidetones'); \
             const inner = rustelScope.voicing( \
               pure('C7').fmap(x => { setDefaultVoicings('lefthand'); return x; }) \
             ); \
             const outer = new Pattern(state => { \
               inner.query(state); \
               return [new Hap(state.span, state.span, { chord: 'C7' })]; \
             }); \
             rustelScope.voicing(outer)",
            &TranspileOptions::default(),
        )
        .expect("nested scoped voicing score");
    assert_eq!(
        active_notes(&runtime),
        ["Bb3", "D4", "E4", "A4"],
        "the inner setter was undone before the outer query resumed"
    );
    runtime.clear_active();
}

#[test]
fn rejected_custom_voicing_default_does_not_change_a_last_good_callback() {
    let _serial = serialize_settings_tests();
    let runtime = runtime_with_native_surface();
    runtime
        .evaluate_score(
            "setDefaultVoicings({'7':['0 4 7']}); pure('C7').fmap(x => rustelScope.voicing(pure(x))).innerJoin()",
            &TranspileOptions::default(),
        )
        .expect("last-good custom voicing score");
    assert_eq!(active_notes(&runtime), ["C4", "E4", "G4"]);

    runtime
        .evaluate_score(
            "setDefaultVoicings('lefthand'); throw new Error('reject')",
            &TranspileOptions::default(),
        )
        .expect_err("reject replacement");
    assert_eq!(
        active_notes(&runtime),
        ["C4", "E4", "G4"],
        "the rejected candidate changed the last-good JavaScript voicing default"
    );
    runtime.clear_active();
}

#[test]
fn rejected_registered_voicing_does_not_change_last_good_voicings_graph() {
    let _serial = serialize_settings_tests();
    let runtime = runtime_with_native_surface();
    runtime
        .evaluate_score(
            "registerVoicings('mine', {'7':['0 4 7']}); chord('C7').voicings('mine')",
            &TranspileOptions::default(),
        )
        .expect("last-good registered voicing score");
    let before = active_notes(&runtime);

    runtime
        .evaluate_score(
            "registerVoicings('mine', {'7':['0 3 7']}); throw new Error('reject')",
            &TranspileOptions::default(),
        )
        .expect_err("reject replacement");
    assert_eq!(
        active_notes(&runtime),
        before,
        "the rejected candidate mutated the shared registry read by .voicings(name)"
    );
    runtime.clear_active();
}

#[test]
fn restoring_last_good_restores_voicings_registry_too() {
    let _serial = serialize_settings_tests();
    let runtime = runtime_with_native_surface();
    runtime
        .evaluate_score(
            "registerVoicings('mine', {'7':['0 4 7']}); chord('C7').voicings('mine')",
            &TranspileOptions::default(),
        )
        .expect("last-good registered voicing score");
    let before = active_notes(&runtime);
    runtime.snapshot_active_as_last_good();

    runtime
        .evaluate_score(
            "registerVoicings('mine', {'7':['0 3 7']}); pure('candidate')",
            &TranspileOptions::default(),
        )
        .expect("candidate registration");
    runtime
        .restore_last_good_active()
        .expect("restore last-good graph and registry");
    assert_eq!(
        active_notes(&runtime),
        before,
        "restoring the wrapper left it reading the rejected candidate registry"
    );
    runtime.clear_active();
}

#[test]
fn restoring_first_install_baseline_clears_candidate_state() {
    let _serial = serialize_settings_tests();
    let runtime = runtime_with_native_surface();
    assert!(!runtime.has_active_pattern());
    runtime.snapshot_active_as_last_good();

    runtime
        .evaluate_score(
            "registerVoicings('firstInstallCandidate', {'7':['0 4 7']}); chord('C7').voicings('firstInstallCandidate').gain(sliderWithID('firstInstallGain', .5))",
            &TranspileOptions::default(),
        )
        .expect("candidate evaluation");
    assert!(runtime.has_active_pattern());
    assert_eq!(runtime.slider_value("firstInstallGain").unwrap(), Some(0.5));
    assert!(runtime.slider_binding("firstInstallGain").is_some());
    runtime
        .eval("setVoicingRange('firstInstallCandidate', ['C3', 'C5'])")
        .expect("candidate voicing is selected");

    runtime
        .restore_last_good_active()
        .expect("restore first-install baseline");
    assert!(!runtime.has_active_pattern());
    assert_eq!(runtime.slider_value("firstInstallGain").unwrap(), None);
    assert_eq!(runtime.slider_binding("firstInstallGain"), None);
    assert!(
        runtime
            .eval("setVoicingRange('firstInstallCandidate', ['C3', 'C5'])")
            .is_err(),
        "candidate voicing remained in the selected registry"
    );
}

#[test]
fn discarded_object_voicing_defaults_are_collectable() {
    let _serial = serialize_settings_tests();
    let runtime = runtime_with_native_surface();
    let options = TranspileOptions::default();
    let rejected =
        "setDefaultVoicings({'7':['0 4 7'], blob:'x'.repeat(32768)}); throw new Error('reject')";

    runtime
        .evaluate_score(rejected, &options)
        .expect_err("warm-up rejection");
    runtime
        .evaluate_score("setDefaultVoicings('ireal'); pure('ok')", &options)
        .expect("release warm-up default");
    runtime.run_gc();
    let before = runtime.heap_live();

    for _ in 0..64 {
        runtime
            .evaluate_score(rejected, &options)
            .expect_err("rejected object default");
    }
    runtime
        .evaluate_score("setDefaultVoicings('ireal'); pure('ok')", &options)
        .expect("release discarded defaults");
    runtime.run_gc();
    let growth = runtime.heap_live().saturating_sub(before);
    assert!(
        growth < 512 * 1024,
        "discarded object defaults retained {growth} bytes"
    );
    runtime.clear_active();
}

#[test]
fn rejected_scores_do_not_publish_candidate_module_settings() {
    let _serial = serialize_settings_tests();
    let runtime = runtime_with_native_surface();
    runtime
        .evaluate_score(
            "useRNG('precise'); setDefaultJoin('out'); setDefaultVoicings('guidetones'); pure('last-good')",
            &TranspileOptions::default(),
        )
        .expect("last-good score");
    assert_eq!(
        current_settings(&runtime),
        (RngMode::Precise, Alignment::Out, 2)
    );

    runtime
        .evaluate_score(
            "useRNG('legacy'); setDefaultJoin('mix'); setDefaultVoicings('lefthand'); throw new Error('reject')",
            &TranspileOptions::default(),
        )
        .expect_err("unbounded rejected score");
    assert_eq!(
        current_settings(&runtime),
        (RngMode::Precise, Alignment::Out, 2)
    );

    runtime
        .evaluate_score_cancellable(
            "useRNG('legacy'); setDefaultJoin('restart'); setDefaultVoicings('lefthand'); throw new Error('reject')",
            &TranspileOptions::default(),
            std::time::Duration::from_secs(1),
            &std::sync::atomic::AtomicBool::new(false),
        )
        .expect_err("bounded rejected score");
    assert_eq!(
        current_settings(&runtime),
        (RngMode::Precise, Alignment::Out, 2)
    );
    runtime.clear_active();
}

#[test]
fn session_candidate_settings_remain_detached_until_publication() {
    let _serial = serialize_settings_tests();
    let runtime = runtime_with_native_surface();
    runtime
        .evaluate_score(
            "setDefaultJoin('out'); setDefaultVoicings('guidetones'); pure('last-good')",
            &TranspileOptions::default(),
        )
        .expect("last-good score");

    let (_, _, candidate) = runtime
        .evaluate_score_candidate_with_effects_cancellable(
            "setDefaultJoin('mix'); setDefaultVoicings('lefthand'); chord('C7').voicing()",
            &TranspileOptions::default(),
            std::time::Duration::from_secs(1),
            &std::sync::atomic::AtomicBool::new(false),
        )
        .expect("candidate score");
    assert_eq!(
        runtime.with_runtime_settings(rustel_core::compose::default_alignment),
        Alignment::Out,
        "candidate state was published before Session accepted it"
    );
    assert_eq!(
        candidate.with(rustel_core::compose::default_alignment),
        Alignment::Mix
    );
    assert_eq!(default_voicing_size(&runtime), 2);
    let candidate_pattern = runtime.active_pattern().expect("candidate graph");
    assert_eq!(
        candidate.with(|| {
            candidate_pattern
                .query_arc(Fraction::ZERO, Fraction::ONE)
                .len()
        }),
        4,
        "the candidate graph did not inherit its detached same-runtime state"
    );

    runtime.adopt_runtime_settings(&candidate);
    assert_eq!(
        runtime.with_runtime_settings(rustel_core::compose::default_alignment),
        Alignment::Mix
    );
    assert_eq!(default_voicing_size(&runtime), 4);
    runtime.clear_active();
}

#[test]
fn accepted_candidate_pattern_metadata_follows_later_runtime_settings() {
    let _serial = serialize_settings_tests();
    let runtime = runtime_with_native_surface();
    let (_, _, candidate) = runtime
        .evaluate_score_candidate_with_effects_cancellable(
            "setDefaultVoicings('guidetones'); pure(null).fmap(() => chord('C7').voicing())",
            &TranspileOptions::default(),
            std::time::Duration::from_secs(1),
            &std::sync::atomic::AtomicBool::new(false),
        )
        .expect("candidate score");
    let outer = candidate.with(|| {
        runtime
            .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .expect("query candidate metadata")
    });
    let nested = match &outer[0].value {
        Value::Pattern(pattern) => pattern.pattern().clone(),
        value => panic!("expected nested Pattern, got {value:?}"),
    };
    assert_eq!(
        candidate.with(|| nested.query_arc(Fraction::ZERO, Fraction::ONE).len()),
        2
    );

    runtime.adopt_runtime_settings(&candidate);
    runtime.with_runtime_settings(|| {
        rustel_core::voicings::set_default_voicings("lefthand");
    });
    assert_eq!(
        nested.query_arc(Fraction::ZERO, Fraction::ONE).len(),
        4,
        "accepted metadata stayed pinned to its detached candidate"
    );
    runtime.clear_active();
}

#[test]
fn accepted_candidate_function_metadata_follows_later_runtime_settings() {
    let _serial = serialize_settings_tests();
    let runtime = runtime_with_native_surface();
    let (_, _, candidate) = runtime
        .evaluate_score_candidate_with_effects_cancellable(
            "setDefaultVoicings('guidetones'); pure(null).fmap(() => voicing)",
            &TranspileOptions::default(),
            std::time::Duration::from_secs(1),
            &std::sync::atomic::AtomicBool::new(false),
        )
        .expect("function-metadata candidate");
    let haps = candidate.with(|| {
        runtime
            .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .expect("query function metadata")
    });
    let function = match &haps[0].value {
        Value::Function(function) => function.clone(),
        value => panic!("expected FunctionRef, got {value:?}"),
    };
    let apply = || {
        function
            .apply(rustel_core::pure(Value::Str("C7".into())))
            .query_arc(Fraction::ZERO, Fraction::ONE)
            .len()
    };
    assert_eq!(candidate.with(apply), 2);

    runtime.adopt_runtime_settings(&candidate);
    runtime.with_runtime_settings(|| {
        rustel_core::voicings::set_default_voicings("lefthand");
    });
    assert_eq!(
        apply(),
        4,
        "accepted FunctionRef stayed pinned to its detached candidate"
    );
    runtime.clear_active();
}

#[test]
fn accepted_candidate_pick_lookup_follows_later_runtime_settings() {
    let _serial = serialize_settings_tests();
    let runtime = runtime_with_native_surface();
    let (_, _, candidate) = runtime
        .evaluate_score_candidate_with_effects_cancellable(
            "setDefaultVoicings('guidetones'); pure({a: chord('C7').voicing()}).set({b: 1})",
            &TranspileOptions::default(),
            std::time::Duration::from_secs(1),
            &std::sync::atomic::AtomicBool::new(false),
        )
        .expect("pick-lookup candidate");
    let haps = candidate.with(|| {
        runtime
            .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .expect("query lookup metadata")
    });
    let lookup = haps[0].pick_lookup().expect("PickLookup metadata").clone();
    let selected_len = |lookup| {
        rustel_core::pick(
            rustel_core::pure(Value::Str("a".into())),
            lookup,
            PickIndexMode::Modulo,
            JoinMode::Inner,
        )
        .query_arc(Fraction::ZERO, Fraction::ONE)
        .len()
    };
    assert_eq!(candidate.with(|| selected_len(lookup.clone())), 2);

    runtime.adopt_runtime_settings(&candidate);
    runtime.with_runtime_settings(|| {
        rustel_core::voicings::set_default_voicings("lefthand");
    });
    assert_eq!(
        selected_len(lookup),
        4,
        "accepted PickLookup stayed pinned to its detached candidate"
    );
    runtime.clear_active();
}

#[test]
fn accepted_candidate_function_argument_follows_later_runtime_settings() {
    let _serial = serialize_settings_tests();
    let runtime = runtime_with_native_surface();
    let (_, _, candidate) = runtime
        .evaluate_score_candidate_with_effects_cancellable(
            "setDefaultVoicings('guidetones'); pure('C7').layer(voicing)",
            &TranspileOptions::default(),
            std::time::Duration::from_secs(1),
            &std::sync::atomic::AtomicBool::new(false),
        )
        .expect("function-argument candidate");
    let graph = runtime.active_pattern().expect("candidate graph");
    assert_eq!(
        candidate.with(|| graph.query_arc(Fraction::ZERO, Fraction::ONE).len()),
        2
    );

    runtime.adopt_runtime_settings(&candidate);
    runtime.with_runtime_settings(|| {
        rustel_core::voicings::set_default_voicings("lefthand");
    });
    assert_eq!(
        graph.query_arc(Fraction::ZERO, Fraction::ONE).len(),
        4,
        "accepted function argument stayed pinned to its detached candidate"
    );
    runtime.clear_active();
}

#[test]
fn accepted_candidate_control_graph_follows_later_runtime_settings() {
    let _serial = serialize_settings_tests();
    let runtime = runtime_with_native_surface();
    let (_, _, candidate) = runtime
        .evaluate_score_candidate_with_effects_cancellable(
            "setDefaultVoicings('guidetones'); chord('C7').layer(voicing)",
            &TranspileOptions::default(),
            std::time::Duration::from_secs(1),
            &std::sync::atomic::AtomicBool::new(false),
        )
        .expect("control-graph candidate");
    let graph = runtime.active_pattern().expect("candidate graph");
    assert_eq!(
        candidate.with(|| graph.query_arc(Fraction::ZERO, Fraction::ONE).len()),
        2
    );

    runtime.adopt_runtime_settings(&candidate);
    runtime.with_runtime_settings(|| {
        rustel_core::voicings::set_default_voicings("lefthand");
    });
    assert_eq!(
        graph.query_arc(Fraction::ZERO, Fraction::ONE).len(),
        4,
        "accepted control graph stayed pinned to its detached candidate"
    );
    runtime.clear_active();
}

#[test]
fn accepted_candidate_lookup_argument_follows_later_runtime_settings() {
    let _serial = serialize_settings_tests();
    let runtime = runtime_with_native_surface();
    let (_, _, candidate) = runtime
        .evaluate_score_candidate_with_effects_cancellable(
            "setDefaultVoicings('guidetones'); pure('a').pick(pure({a: chord('C7').voicing()}))",
            &TranspileOptions::default(),
            std::time::Duration::from_secs(1),
            &std::sync::atomic::AtomicBool::new(false),
        )
        .expect("lookup-argument candidate");
    let graph = runtime.active_pattern().expect("candidate graph");
    assert_eq!(
        candidate.with(|| graph.query_arc(Fraction::ZERO, Fraction::ONE).len()),
        2
    );

    runtime.adopt_runtime_settings(&candidate);
    runtime.with_runtime_settings(|| {
        rustel_core::voicings::set_default_voicings("lefthand");
    });
    assert_eq!(
        graph.query_arc(Fraction::ZERO, Fraction::ONE).len(),
        4,
        "accepted lookup argument stayed pinned to its detached candidate"
    );
    runtime.clear_active();
}

#[test]
fn pure_queries_reenter_the_runtime_that_built_the_graph() {
    let _serial = serialize_settings_tests();
    let first = runtime_with_native_surface();
    let second = runtime_with_native_surface();

    first
        .evaluate_score(
            "setDefaultVoicings('guidetones'); chord('C7').voicing()",
            &TranspileOptions::default(),
        )
        .expect("first voicing score");
    second
        .evaluate_score(
            "setDefaultVoicings('lefthand'); chord('C7').voicing()",
            &TranspileOptions::default(),
        )
        .expect("second voicing score");
    let exported_first = first.active_pattern().expect("export first graph");

    assert!(
        !first.active_needs_host(),
        "native voicing should stay pure"
    );
    assert!(
        !second.active_needs_host(),
        "native voicing should stay pure"
    );
    assert_eq!(
        first
            .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .expect("query first graph")
            .len(),
        2
    );
    assert_eq!(
        second
            .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .expect("query second graph")
            .len(),
        4
    );
    assert_eq!(
        exported_first
            .query_arc(Fraction::ZERO, Fraction::ONE)
            .len(),
        2,
        "an exported graph lost the module state of its owning runtime"
    );

    first.clear_active();
    second.clear_active();
}

#[test]
fn accessor_capable_public_entries_bind_the_owning_runtime() {
    let _serial = serialize_settings_tests();
    let ambient = rustel_core::settings::RuntimeSettings::default();
    ambient.with(|| {
        rustel_core::rng::use_rng(RngMode::Legacy);
        rustel_core::compose::set_default_alignment(Alignment::In);
        rustel_core::voicings::set_default_voicings("guidetones");
    });

    let runtime = runtime_with_native_surface();
    runtime
        .eval(
            r#"
              Object.defineProperty(globalThis, 'queryHeld', {
                configurable: true,
                set() { useRNG('precise'); }
              });
              Object.defineProperty(globalThis, '__gc', {
                configurable: true,
                set() { setDefaultJoin('out'); }
              });
              Object.defineProperty(globalThis, '__cellsLive', {
                configurable: true,
                set() { setDefaultVoicings('lefthand'); }
              });
            "#,
        )
        .expect("install accessors");

    ambient.with(|| {
        runtime
            .install_query_binding()
            .expect("query binding through accessor");
        runtime
            .install_gc_binding()
            .expect("gc bindings through accessors");
    });

    assert_eq!(
        runtime.with_runtime_settings(rustel_core::rng::rng_mode),
        RngMode::Precise
    );
    assert_eq!(
        runtime.with_runtime_settings(rustel_core::compose::default_alignment),
        Alignment::Out
    );
    assert_eq!(default_voicing_size(&runtime), 4);
    ambient.with(|| {
        assert_eq!(rustel_core::rng::rng_mode(), RngMode::Legacy);
        assert_eq!(rustel_core::compose::default_alignment(), Alignment::In);
        let mut controls = OrderedMap::new();
        controls.insert("chord".into(), Value::Str("C7".into()));
        assert_eq!(
            rustel_core::voicings::render_voicing(&controls)
                .expect("ambient voicing")
                .len(),
            2
        );
    });
}

#[test]
fn runtime_teardown_does_not_mutate_the_callers_settings() {
    let _serial = serialize_settings_tests();
    let ambient = rustel_core::settings::RuntimeSettings::default();
    ambient.with(|| rustel_core::voicings::set_default_voicings("guidetones"));

    let runtime = runtime_with_native_surface();
    runtime
        .eval(
            r#"
              Object.defineProperty(globalThis, '__rustel_held', {
                configurable: true,
                set() { setDefaultVoicings('lefthand'); }
              });
            "#,
        )
        .expect("install held-slot accessor");
    ambient.with(|| drop(runtime));

    ambient.with(|| {
        let mut controls = OrderedMap::new();
        controls.insert("chord".into(), Value::Str("C7".into()));
        assert_eq!(
            rustel_core::voicings::render_voicing(&controls)
                .expect("ambient voicing after runtime drop")
                .len(),
            2
        );
    });
}

#[test]
fn active_presence_checks_do_not_allocate_graph_nodes() {
    let _serial = serialize_settings_tests();
    let runtime = runtime_with_native_surface();
    runtime
        .evaluate_score("pure('active')", &TranspileOptions::default())
        .expect("active graph");

    let before = live_node_count();
    for _ in 0..1_000 {
        assert!(runtime.has_active_pattern());
    }
    assert_eq!(live_node_count(), before);

    let exported = runtime.active_pattern().expect("export active graph");
    assert_eq!(
        live_node_count(),
        before,
        "exporting runtime settings allocated a graph wrapper"
    );
    drop(exported);
    assert_eq!(live_node_count(), before);
    runtime.clear_active();
    assert!(!runtime.has_active_pattern());
}

#[test]
fn rejected_native_settings_are_detached_from_concurrent_exported_queries() {
    let _serial = serialize_settings_tests();
    let runtime = runtime_with_native_surface();
    runtime
        .evaluate_score(
            "setDefaultVoicings('guidetones'); chord('C7').voicing()",
            &TranspileOptions::default(),
        )
        .expect("last-good graph");
    let exported = runtime.active_pattern().expect("export last-good graph");

    let ambient = rustel_core::settings::RuntimeSettings::default();
    ambient.with(|| rustel_core::voicings::set_default_voicings("lefthand"));
    let candidate = ambient.with(|| runtime.snapshot_ambient_runtime_settings());
    let entered = std::sync::Arc::new(std::sync::Barrier::new(2));
    let resume = std::sync::Arc::new(std::sync::Barrier::new(2));
    let candidate_entered = entered.clone();
    let candidate_resume = resume.clone();
    let candidate_thread = std::thread::spawn(move || {
        candidate.with(|| {
            let mut controls = OrderedMap::new();
            controls.insert("chord".into(), Value::Str("C7".into()));
            assert_eq!(
                rustel_core::voicings::render_voicing(&controls)
                    .expect("candidate voicing")
                    .len(),
                4
            );
            candidate_entered.wait();
            candidate_resume.wait();
        });
        candidate
    });

    entered.wait();
    assert_eq!(
        exported.query_arc(Fraction::ZERO, Fraction::ONE).len(),
        2,
        "a detached candidate changed the concurrently queried last-good graph"
    );
    resume.wait();
    let candidate = candidate_thread.join().expect("candidate thread");
    assert_eq!(
        exported.query_arc(Fraction::ZERO, Fraction::ONE).len(),
        2,
        "rejecting the candidate changed the exported graph"
    );

    runtime.adopt_runtime_settings(&candidate);
    assert_eq!(
        exported.query_arc(Fraction::ZERO, Fraction::ONE).len(),
        4,
        "accepted settings were not published to exported graphs"
    );
    runtime.clear_active();
}
