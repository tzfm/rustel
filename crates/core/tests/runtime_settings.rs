use std::sync::{Arc, Barrier};

use rustel_core::compose::{self, Alignment};
use rustel_core::rng::{self, RngMode};
use rustel_core::settings::RuntimeSettings;
use rustel_core::value::{FunctionRef, JsValueRef, PatternValue};
use rustel_core::{
    CallbackHost, Hap, JoinMode, OrderedMap, PickIndexMode, PickLookup, State, TimeSpan,
    TimelineState, Value,
};
use rustel_fraction::Fraction;

fn set_settings(rng_mode: RngMode, join: Alignment, voicings: &str) {
    rng::use_rng(rng_mode);
    compose::set_default_alignment(join);
    rustel_core::voicings::set_default_voicings(voicings);
}
fn settings_tuple() -> (RngMode, Alignment, usize) {
    let mut controls = OrderedMap::new();
    controls.insert("chord".into(), Value::Str("C7".into()));
    (
        rng::rng_mode(),
        compose::default_alignment(),
        rustel_core::voicings::render_voicing(&controls)
            .expect("configured C7 voicing")
            .len(),
    )
}

fn voicing_size_signal() -> rustel_core::Pattern {
    rustel_core::state_signal(|_| Value::F64(settings_tuple().2 as f64))
        .with_steps(Some(Fraction::ONE))
}

#[test]
fn runtime_attachment_preserves_metadata_and_hap_budget() {
    let settings = RuntimeSettings::default();
    let plain = rustel_core::pure(Value::Str("x".into())).with_pure_loc((3, 9));
    let wrapped = plain.clone().with_runtime_settings(settings.clone());
    assert!(wrapped.is_pure());
    assert_eq!(wrapped.pure_loc(), Some((3, 9)));
    assert_eq!(wrapped.as_pure(), plain.as_pure());

    let state = State::new(TimeSpan::new(Fraction::ZERO, Fraction::int(4)));
    for budget in [3, 4, 5] {
        assert_eq!(
            wrapped
                .try_query_state_with_budget(&state, budget)
                .map(|haps| haps.len()),
            plain
                .try_query_state_with_budget(&state, budget)
                .map(|haps| haps.len()),
            "the settings wrapper changed the hap budget boundary"
        );
    }
}

#[test]
fn visual_membership_preserves_pure_source_metadata() {
    let pattern = rustel_core::pure(Value::Str("x".into())).with_pure_loc((3, 9));
    assert_eq!(pattern.with_ui_visual_slot(0).pure_loc(), Some((3, 9)));
}

#[test]
fn timeline_root_inherits_the_receiver_runtime() {
    let time_owner = RuntimeSettings::default();
    time_owner.with(|| rng::use_rng(RngMode::Precise));
    let receiver_owner = RuntimeSettings::default();
    receiver_owner.with(|| rng::use_rng(RngMode::Legacy));
    let ambient = RuntimeSettings::default();
    ambient.with(|| rng::use_rng(RngMode::Precise));

    let time = rustel_core::pure(Value::F64(0.0)).with_runtime_settings(time_owner);
    let receiver =
        rustel_core::pure(Value::Str("receiver".into())).with_runtime_settings(receiver_owner);
    let pattern = rustel_core::timeline(time, receiver, TimelineState::default())
        .fmap(|_| Value::Str(format!("{:?}", rng::rng_mode())));

    ambient.with(|| {
        assert_eq!(
            pattern.query_arc(Fraction::ZERO, Fraction::ONE)[0].value,
            Value::Str("Legacy".into())
        );
    });
}

#[test]
fn concurrent_setters_publish_after_the_bound_operation() {
    let settings = RuntimeSettings::default();
    settings.with(|| set_settings(RngMode::Legacy, Alignment::In, "guidetones"));

    let entered = Arc::new(Barrier::new(2));
    let resume = Arc::new(Barrier::new(2));
    let worker_settings = settings.clone();
    let worker_entered = Arc::clone(&entered);
    let worker_resume = Arc::clone(&resume);
    let worker = std::thread::spawn(move || {
        worker_settings.with(|| {
            let before = settings_tuple();
            worker_entered.wait();
            worker_resume.wait();
            (before, settings_tuple())
        })
    });

    entered.wait();
    settings.with(|| set_settings(RngMode::Precise, Alignment::Out, "lefthand"));
    resume.wait();

    let (before, after) = worker.join().expect("settings reader");
    assert_eq!(before, (RngMode::Legacy, Alignment::In, 2));
    assert_eq!(after, before, "one operation observed two publications");
    settings.with(|| {
        assert_eq!(settings_tuple(), (RngMode::Precise, Alignment::Out, 4));
    });
}

#[test]
fn runtime_scopes_restore_correctly_when_dropped_out_of_order() {
    let ambient = RuntimeSettings::default();
    let outer = RuntimeSettings::default();
    let inner = RuntimeSettings::default();
    ambient.with(|| rng::use_rng(RngMode::Legacy));
    outer.with(|| rng::use_rng(RngMode::Precise));
    inner.with(|| rng::use_rng(RngMode::Legacy));

    let ambient_scope = ambient.bind();
    let outer_scope = outer.bind();
    let inner_scope = inner.bind();
    assert_eq!(rng::rng_mode(), RngMode::Legacy);
    drop(outer_scope);
    assert_eq!(rng::rng_mode(), RngMode::Legacy);
    drop(inner_scope);
    assert_eq!(rng::rng_mode(), RngMode::Legacy);
    drop(ambient_scope);
}

#[test]
fn nested_wrappers_keep_the_snapshot_selected_by_the_outer_query() {
    let settings = RuntimeSettings::default();
    settings.with(|| rng::use_rng(RngMode::Legacy));
    let replacement = RuntimeSettings::default();
    replacement.with(|| rng::use_rng(RngMode::Precise));

    let entered = Arc::new(Barrier::new(2));
    let resume = Arc::new(Barrier::new(2));
    let transform_entered = Arc::clone(&entered);
    let transform_resume = Arc::clone(&resume);
    let pause_once = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let transform_once = Arc::clone(&pause_once);
    let inner = rustel_core::state_signal(|_| Value::Str(format!("{:?}", rng::rng_mode())))
        .with_runtime_settings(settings.clone());
    let exported = inner
        .with_query_time(move |time| {
            if !transform_once.swap(true, std::sync::atomic::Ordering::SeqCst) {
                transform_entered.wait();
                transform_resume.wait();
            }
            time
        })
        .with_runtime_settings(settings.clone());
    let query = std::thread::spawn(move || {
        exported.query_arc(Fraction::ZERO, Fraction::ONE)[0]
            .value
            .show()
    });

    entered.wait();
    settings.replace_with(&replacement);
    resume.wait();
    assert_eq!(query.join().expect("pattern query"), "Legacy");
    settings.with(|| assert_eq!(rng::rng_mode(), RngMode::Precise));
}

#[test]
fn joins_query_resolved_inner_patterns_under_the_owner_snapshot() {
    let owner = RuntimeSettings::default();
    owner.with(|| rustel_core::voicings::set_default_voicings("guidetones"));
    let ambient = RuntimeSettings::default();
    ambient.with(|| rustel_core::voicings::set_default_voicings("lefthand"));

    let wrapped =
        rustel_core::pure_pattern(voicing_size_signal()).with_runtime_settings(owner.clone());
    let joins = [
        wrapped.inner_join(),
        wrapped.step_join(),
        wrapped.poly_join(),
    ];
    ambient.with(|| {
        for joined in joins {
            let haps = joined.query_arc(Fraction::ZERO, Fraction::ONE);
            assert_eq!(haps.len(), 1);
            assert_eq!(haps[0].value, Value::F64(2.0));
        }
    });
}

#[test]
fn pure_metadata_keeps_nested_patterns_and_functions_in_the_owner_runtime() {
    let owner = RuntimeSettings::default();
    owner.with(|| rustel_core::voicings::set_default_voicings("guidetones"));
    let ambient = RuntimeSettings::default();
    ambient.with(|| rustel_core::voicings::set_default_voicings("lefthand"));

    let nested = rustel_core::pure(Value::object([(
        "nested".into(),
        Value::List(vec![Value::Pattern(Box::new(PatternValue::new(
            7,
            voicing_size_signal(),
        )))]),
    )]))
    .with_runtime_settings(owner.clone());
    let Value::Object(nested) = nested.as_pure().expect("pure pattern value") else {
        panic!("pure metadata did not contain its Object value");
    };
    let Some(Value::List(nested)) = nested.get("nested") else {
        panic!("pure metadata did not contain its nested List value");
    };
    let Value::Pattern(nested) = &nested[0] else {
        panic!("pure metadata did not contain its Pattern value");
    };
    ambient.with(|| {
        let haps = nested.pattern().query_arc(Fraction::ZERO, Fraction::ONE);
        assert_eq!(haps[0].value, Value::F64(2.0));
    });

    let function = FunctionRef::native(
        Some("voicingSize"),
        Arc::new(|pattern| {
            let size = settings_tuple().2 as f64;
            pattern.fmap(move |_| Value::F64(size))
        }),
        Arc::new(|pattern| {
            let size = settings_tuple().2 as f64;
            pattern.fmap(move |_| Value::F64(size))
        }),
    );
    let carrier = rustel_core::pure(Value::Function(function)).with_runtime_settings(owner.clone());
    assert!(carrier.is_pure());
    let Value::Function(function) = carrier.as_pure().expect("pure function value") else {
        panic!("pure metadata did not contain its Function value");
    };
    assert!(function.is_native());
    ambient.with(|| {
        let input = rustel_core::pure(Value::Str("x".into()));
        let output = function.apply(input.clone());
        assert!(output.is_pure());
        assert_eq!(
            output.query_arc(Fraction::ZERO, Fraction::ONE)[0].value,
            Value::F64(2.0)
        );
        let output = function
            .apply_pure(input.as_pure_pattern().expect("pure input"))
            .expect("native pure function");
        assert_eq!(
            output.query_arc(Fraction::ZERO, Fraction::ONE)[0].value,
            Value::F64(2.0)
        );
        let output = function.apply_indexed_batch(vec![(input.clone(), 0)]);
        assert_eq!(
            output[0].query_arc(Fraction::ZERO, Fraction::ONE)[0].value,
            Value::F64(2.0)
        );
        let output = function
            .apply_pure_indexed_batch(vec![(input.as_pure_pattern().expect("pure input"), 0)])
            .expect("native pure function batch");
        assert_eq!(
            output[0].query_arc(Fraction::ZERO, Fraction::ONE)[0].value,
            Value::F64(2.0)
        );
    });

    owner.with(|| rustel_core::voicings::set_default_voicings("lefthand"));
    ambient.with(|| {
        assert_eq!(
            nested.pattern().query_arc(Fraction::ZERO, Fraction::ONE)[0].value,
            Value::F64(4.0),
            "extracted metadata stopped following its owning runtime"
        );
        let output = function.apply(rustel_core::pure(Value::Str("x".into())));
        assert_eq!(
            output.query_arc(Fraction::ZERO, Fraction::ONE)[0].value,
            Value::F64(4.0)
        );
    });
}

#[test]
fn explicit_scopes_own_unwrapped_pure_and_lookup_extractors() {
    let owner = RuntimeSettings::default();
    owner.with(|| rustel_core::voicings::set_default_voicings("guidetones"));
    let ambient = RuntimeSettings::default();
    ambient.with(|| rustel_core::voicings::set_default_voicings("lefthand"));
    let function = FunctionRef::native(
        Some("queryVoicingSize"),
        Arc::new(|pattern| pattern.fmap(|_| Value::F64(settings_tuple().2 as f64))),
        Arc::new(|pattern| pattern.fmap(|_| Value::F64(settings_tuple().2 as f64))),
    );
    let carrier = rustel_core::pure(Value::List(vec![
        Value::Pattern(Box::new(PatternValue::new(25, voicing_size_signal()))),
        Value::Function(function),
    ]))
    .with_added_context(vec![(1, 2)]);
    let Value::List(extracted) = owner
        .with(|| carrier.as_pure())
        .expect("unwrapped pure metadata")
    else {
        panic!("pure metadata changed shape");
    };
    let Value::Pattern(nested) = &extracted[0] else {
        panic!("pure metadata lost its Pattern");
    };
    let Value::Function(function) = &extracted[1] else {
        panic!("pure metadata lost its Function");
    };

    let lookup_carrier = rustel_core::pure_pick_lookup(
        Value::F64(0.0),
        PickLookup::Array {
            enumerable_len: 1,
            length: 1,
            entries: vec![(0, voicing_size_signal())],
        },
    );
    let lookup = owner
        .with(|| lookup_carrier.as_pick_lookup())
        .expect("unwrapped lookup metadata");
    let picked = rustel_core::pick(
        rustel_core::pure(Value::F64(0.0)),
        lookup,
        PickIndexMode::Clamp,
        JoinMode::Inner,
    );

    ambient.with(|| {
        assert_eq!(
            nested.pattern().query_arc(Fraction::ZERO, Fraction::ONE)[0].value,
            Value::F64(2.0)
        );
        assert_eq!(
            function
                .apply(rustel_core::pure(Value::Null))
                .query_arc(Fraction::ZERO, Fraction::ONE)[0]
                .value,
            Value::F64(2.0)
        );
        assert_eq!(
            picked.query_arc(Fraction::ZERO, Fraction::ONE)[0].value,
            Value::F64(2.0)
        );
    });
}

#[test]
fn an_owned_function_keeps_its_lexical_runtime_with_a_foreign_receiver() {
    let function_owner = RuntimeSettings::default();
    function_owner.with(|| rustel_core::voicings::set_default_voicings("guidetones"));
    let receiver_owner = RuntimeSettings::default();
    receiver_owner.with(|| rustel_core::voicings::set_default_voicings("lefthand"));
    let ambient = RuntimeSettings::default();
    ambient.with(|| rustel_core::voicings::set_default_voicings("ireal"));

    let function = FunctionRef::native(
        Some("queryVoicingSize"),
        Arc::new(|pattern| pattern.fmap(|_| Value::F64(settings_tuple().2 as f64))),
        Arc::new(|pattern| pattern.fmap(|_| Value::F64(settings_tuple().2 as f64))),
    );
    let unowned_function = function.clone();
    let carrier =
        rustel_core::pure(Value::Function(function)).with_runtime_settings(function_owner.clone());
    let Value::Function(function) = carrier.as_pure().expect("owned function metadata") else {
        panic!("carrier did not contain its Function");
    };
    let receiver = rustel_core::pure(Value::Null).with_runtime_settings(receiver_owner.clone());
    let pure_receiver = receiver.as_pure_pattern().expect("pure receiver");

    let mut outputs = vec![
        function.apply(receiver.clone()),
        function
            .apply_pure(pure_receiver.clone())
            .expect("native pure application")
            .pattern()
            .clone(),
    ];
    outputs.extend(function.apply_indexed_batch(vec![(receiver, 0)]));
    outputs.extend(
        function
            .apply_pure_indexed_batch(vec![(pure_receiver, 0)])
            .expect("native pure indexed application")
            .into_iter()
            .map(|pattern| pattern.pattern().clone()),
    );

    ambient.with(|| {
        for output in outputs {
            assert_eq!(
                output.query_arc(Fraction::ZERO, Fraction::ONE)[0].value,
                Value::F64(2.0),
                "the receiver runtime replaced the function's lexical runtime"
            );
        }
    });

    let receiver = rustel_core::pure(Value::Null).with_runtime_settings(receiver_owner);
    let pure_receiver = receiver.as_pure_pattern().expect("pure receiver");
    let mut outputs = function_owner.with(|| {
        vec![
            unowned_function.apply(receiver.clone()),
            unowned_function
                .apply_pure(pure_receiver.clone())
                .expect("native pure application")
                .pattern()
                .clone(),
        ]
    });
    outputs
        .extend(function_owner.with(|| unowned_function.apply_indexed_batch(vec![(receiver, 0)])));
    outputs.extend(function_owner.with(|| {
        unowned_function
            .apply_pure_indexed_batch(vec![(pure_receiver, 0)])
            .expect("native pure indexed application")
            .into_iter()
            .map(|pattern| pattern.pattern().clone())
    }));
    ambient.with(|| {
        for output in outputs {
            assert_eq!(
                output.query_arc(Fraction::ZERO, Fraction::ONE)[0].value,
                Value::F64(2.0),
                "an explicit lexical scope lost to the receiver runtime"
            );
        }
    });
}

#[test]
fn pick_lookup_metadata_keeps_entry_patterns_in_the_owner_runtime() {
    let owner = RuntimeSettings::default();
    owner.with(|| rustel_core::voicings::set_default_voicings("guidetones"));
    let ambient = RuntimeSettings::default();
    ambient.with(|| rustel_core::voicings::set_default_voicings("lefthand"));
    let lookup = PickLookup::Array {
        enumerable_len: 1,
        length: 1,
        entries: vec![(0, voicing_size_signal())],
    };
    let carrier =
        rustel_core::pure_pick_lookup(Value::F64(0.0), lookup).with_runtime_settings(owner.clone());
    let lookup = carrier.as_pick_lookup().expect("pick lookup metadata");
    let picked = rustel_core::pick(
        rustel_core::pure(Value::F64(0.0)),
        lookup,
        PickIndexMode::Clamp,
        JoinMode::Inner,
    );
    ambient.with(|| {
        assert_eq!(
            picked.query_arc(Fraction::ZERO, Fraction::ONE)[0].value,
            Value::F64(2.0)
        );
    });
    owner.with(|| rustel_core::voicings::set_default_voicings("lefthand"));
    ambient.with(|| {
        assert_eq!(
            picked.query_arc(Fraction::ZERO, Fraction::ONE)[0].value,
            Value::F64(4.0),
            "extracted lookup stopped following its owning runtime"
        );
    });
}

#[test]
fn ordinary_hap_pattern_values_keep_the_owner_runtime_after_extraction() {
    let owner = RuntimeSettings::default();
    owner.with(|| rustel_core::voicings::set_default_voicings("guidetones"));
    let ambient = RuntimeSettings::default();
    ambient.with(|| rustel_core::voicings::set_default_voicings("lefthand"));
    let carrier = rustel_core::pure(Value::Pattern(Box::new(PatternValue::new(
        17,
        voicing_size_signal(),
    ))))
    .with_runtime_settings(owner);

    ambient.with(|| {
        let haps = carrier.query_arc(Fraction::ZERO, Fraction::ONE);
        let Value::Pattern(nested) = &haps[0].value else {
            panic!("carrier did not emit its nested Pattern");
        };
        assert_eq!(
            nested.pattern().query_arc(Fraction::ZERO, Fraction::ONE)[0].value,
            Value::F64(2.0),
            "a Pattern extracted from an ordinary Hap lost its owner runtime"
        );
    });
}

#[test]
fn an_explicit_query_scope_owns_metadata_from_an_unwrapped_root() {
    let owner = RuntimeSettings::default();
    owner.with(|| rustel_core::voicings::set_default_voicings("guidetones"));
    let ambient = RuntimeSettings::default();
    ambient.with(|| rustel_core::voicings::set_default_voicings("lefthand"));
    let carrier = rustel_core::pure(Value::Pattern(Box::new(PatternValue::new(
        20,
        voicing_size_signal(),
    ))));

    let haps = owner.with(|| carrier.query_arc(Fraction::ZERO, Fraction::ONE));
    let Value::Pattern(nested) = &haps[0].value else {
        panic!("carrier did not emit its nested Pattern");
    };
    ambient.with(|| {
        assert_eq!(
            nested.pattern().query_arc(Fraction::ZERO, Fraction::ONE)[0].value,
            Value::F64(2.0),
            "metadata from an unwrapped root lost the explicit query scope"
        );
    });
}

#[test]
fn metadata_returned_by_a_nested_public_query_keeps_its_owner() {
    let owner = RuntimeSettings::default();
    owner.with(|| rustel_core::voicings::set_default_voicings("guidetones"));
    let ambient = RuntimeSettings::default();
    ambient.with(|| rustel_core::voicings::set_default_voicings("lefthand"));
    let nested_query = rustel_core::pure(Value::Pattern(Box::new(PatternValue::new(
        24,
        voicing_size_signal(),
    ))))
    .with_runtime_settings(owner.clone());
    let escaped = Arc::new(std::sync::Mutex::new(None));
    let callback_escaped = Arc::clone(&escaped);
    let callback = rustel_core::state_signal(move |_| {
        let haps = nested_query.query_arc(Fraction::ZERO, Fraction::ONE);
        let Value::Pattern(nested) = &haps[0].value else {
            panic!("nested query did not emit its Pattern");
        };
        *callback_escaped.lock().expect("escaped Pattern slot") = Some(nested.pattern().clone());
        Value::Null
    })
    .with_runtime_settings(owner);

    ambient.with(|| {
        callback.query_arc(Fraction::ZERO, Fraction::ONE);
        let escaped = escaped
            .lock()
            .expect("escaped Pattern slot")
            .clone()
            .expect("nested query did not export its Pattern");
        assert_eq!(
            escaped.query_arc(Fraction::ZERO, Fraction::ONE)[0].value,
            Value::F64(2.0),
            "a nested queryArc result escaped without its owner"
        );
    });
}

#[test]
fn materialized_javascript_containers_keep_the_querying_runtime() {
    struct MaterializeHost {
        nested: rustel_core::Pattern,
        observed_voicing_size: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl CallbackHost for MaterializeHost {
        fn call_value(&self, id: usize, _: &Value) -> Result<Value, String> {
            Err(format!("unexpected value callback {id}"))
        }

        fn call_materialize_value(&self, _: usize) -> Result<Value, String> {
            self.observed_voicing_size
                .store(settings_tuple().2, std::sync::atomic::Ordering::SeqCst);
            Ok(Value::object([(
                "nested".into(),
                Value::Pattern(Box::new(PatternValue::new(22, self.nested.clone()))),
            )]))
        }

        fn call_query(&self, id: usize, _: &State) -> Result<Vec<Hap>, String> {
            Err(format!("unexpected query callback {id}"))
        }
    }

    let owner = RuntimeSettings::default();
    owner.with(|| rustel_core::voicings::set_default_voicings("guidetones"));
    let ambient = RuntimeSettings::default();
    ambient.with(|| rustel_core::voicings::set_default_voicings("lefthand"));
    let observed_voicing_size = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let host = MaterializeHost {
        nested: voicing_size_signal(),
        observed_voicing_size: Arc::clone(&observed_voicing_size),
    };
    let carrier =
        rustel_core::pure(Value::JsValue(JsValueRef::new(23, false))).with_runtime_settings(owner);

    let haps = ambient.with(|| {
        rustel_core::with_callback_host(&host, || carrier.query_arc(Fraction::ZERO, Fraction::ONE))
    });
    assert_eq!(
        observed_voicing_size.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "JavaScript materialization ran after the owner scope unwound"
    );
    let Value::Object(materialized) = &haps[0].value else {
        panic!("JavaScript container did not materialize as an Object");
    };
    let Some(Value::Pattern(nested)) = materialized.get("nested") else {
        panic!("materialized Object did not contain its Pattern");
    };
    ambient.with(|| {
        assert_eq!(
            nested.pattern().query_arc(Fraction::ZERO, Fraction::ONE)[0].value,
            Value::F64(2.0),
            "post-query materialization lost the querying runtime"
        );
    });
}

#[test]
fn dynamic_pattern_values_join_under_the_owner_runtime() {
    let owner = RuntimeSettings::default();
    owner.with(|| rustel_core::voicings::set_default_voicings("guidetones"));
    let ambient = RuntimeSettings::default();
    ambient.with(|| rustel_core::voicings::set_default_voicings("lefthand"));
    let joined = rustel_core::pure(Value::Null)
        .with_runtime_settings(owner)
        .fmap(|_| Value::Pattern(Box::new(PatternValue::new(18, voicing_size_signal()))))
        .inner_join();

    ambient.with(|| {
        assert_eq!(
            joined.query_arc(Fraction::ZERO, Fraction::ONE)[0].value,
            Value::F64(2.0),
            "a query-time Pattern value joined under the ambient runtime"
        );
    });
}

#[test]
fn pick_lookup_attached_to_a_hap_keeps_entry_patterns_in_the_owner_runtime() {
    let owner = RuntimeSettings::default();
    owner.with(|| rustel_core::voicings::set_default_voicings("guidetones"));
    let ambient = RuntimeSettings::default();
    ambient.with(|| rustel_core::voicings::set_default_voicings("lefthand"));
    let carrier = rustel_core::pure_pick_lookup(
        Value::F64(0.0),
        PickLookup::Array {
            enumerable_len: 1,
            length: 1,
            entries: vec![(0, voicing_size_signal())],
        },
    )
    .with_runtime_settings(owner);

    ambient.with(|| {
        let haps = carrier.query_arc(Fraction::ZERO, Fraction::ONE);
        let PickLookup::Array { entries, .. } =
            haps[0].pick_lookup().expect("ordinary Hap lookup metadata")
        else {
            panic!("lookup changed shape");
        };
        assert_eq!(
            entries[0].1.query_arc(Fraction::ZERO, Fraction::ONE)[0].value,
            Value::F64(2.0),
            "a lookup extracted from an ordinary Hap lost its owner runtime"
        );
    });
}

#[test]
fn repeated_runtime_attachment_is_flat_and_queryable() {
    let settings = RuntimeSettings::default();
    let mut pattern = rustel_core::pure(Value::Str("safe".into()));
    for _ in 0..100_000 {
        pattern = pattern.with_runtime_settings(settings.clone());
    }
    assert_eq!(
        pattern.query_arc(Fraction::ZERO, Fraction::ONE)[0].value,
        Value::Str("safe".into())
    );
}

#[test]
fn deeply_nested_metadata_refuses_within_the_bounded_traversal() {
    let settings = RuntimeSettings::default();
    let mut value = Value::Pattern(Box::new(PatternValue::new(19, voicing_size_signal())));
    for _ in 0..=rustel_core::MAX_PATTERN_DEPTH {
        value = Value::List(vec![value]);
    }
    let pattern = rustel_core::pure(value).with_runtime_settings(settings);
    let state = State::new(TimeSpan::new(Fraction::ZERO, Fraction::ONE));
    assert!(matches!(
        pattern.try_query_state_with_budget(&state, 8),
        Err(rustel_core::QueryLimit::GraphDepth { depth, limit })
            if depth == rustel_core::MAX_PATTERN_DEPTH + 1
                && limit == rustel_core::MAX_PATTERN_DEPTH
    ));
}
