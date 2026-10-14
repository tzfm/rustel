//! Graph compilation after pattern controls, with retained score ownership.

use rustel_core::{Hap, Value};
use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, Slot};
use rustel_transpiler::TranspileOptions;

fn runtime() -> JsRuntime {
    let runtime = JsRuntime::new().expect("runtime");
    runtime.install_semantic_bindings().expect("bindings");
    runtime
}

fn evaluate(runtime: &JsRuntime, source: &str) -> Vec<Hap> {
    runtime
        .evaluate_score(source, &TranspileOptions::default())
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("query")
}

fn output(value: &Value) -> [f64; 2] {
    let source = value
        .get("workletGraph")
        .and_then(Value::as_str)
        .expect("compiled graph");
    let program = rustel_kabel::Program::from_json(source).expect("program");
    rustel_kabel::Graph::new(&program, 48_000.0)
        .expect("graph")
        .process([0.25, 0.5], 1.0, 0.0)
}

#[test]
fn graph_uses_final_pitch_inputs_and_controls() {
    let runtime = runtime();
    for source in [
        "K(n(sFreq + S(2)).out()).freq(220).octave(1)",
        "freq(220).octave(1).K(n(sFreq + S(2)).out())",
        "freq(220).octave(1).worklet('n(sFreq + pat[0]).out()', 2)",
    ] {
        let haps = evaluate(&runtime, source);
        assert_eq!(haps.len(), 1, "{source}");
        assert_eq!(output(&haps[0].value), [442.0; 2], "{source}");
    }
    let haps = evaluate(&runtime, "note(\"a3 a4\").K(n(sFreq).out())");
    assert_eq!(
        haps.iter()
            .map(|hap| output(&hap.value))
            .collect::<Vec<_>>(),
        vec![[220.0; 2], [440.0; 2]]
    );
    for (controls, expected) in [
        ("{note: ''}", rustel_core::util::midi_to_freq(36.0)),
        ("{note: false}", rustel_core::util::midi_to_freq(36.0)),
        ("{note: 69, freq: '0'}", 0.0),
        ("{note: true, freq: 220}", 220.0),
    ] {
        let haps = evaluate(&runtime, &format!("pure({controls}).K(n(sFreq).out())"));
        assert_eq!(output(&haps[0].value), [expected; 2], "{controls}");
    }
}

#[test]
fn graph_survives_callbacks_effects_and_collection() {
    let runtime = runtime();
    let haps = evaluate(
        &runtime,
        "freq(110).FX(K(audioin().mul(2).out())).K(n(sFreq).out()).sometimesBy(1, p => p)",
    );
    let Value::List(effects) = haps[0].value.get("FX").expect("effects") else {
        panic!("effect list")
    };
    assert_eq!(output(&effects[0]), [0.5; 2]);
    assert_eq!(output(&haps[0].value), [110.0; 2]);
    runtime.run_gc();
    runtime.run_gc();
    let haps = runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("query after collection");
    assert_eq!(output(&haps[0].value), [110.0; 2]);
}

#[test]
fn custom_queries_compile_dynamically_created_sources_and_effects() {
    let runtime = runtime();
    for source in [
        "new Pattern(state => worklet('n(sFreq + pat[0]).out()', 2).query(state)).freq(220).octave(1)",
        "const dynamic = pure(0); dynamic.query = state => worklet('n(sFreq + pat[0]).out()', 2).freq(220).octave(1).query(state); dynamic",
    ] {
        let haps = evaluate(&runtime, source);
        assert_eq!(haps.len(), 1, "{source}");
        assert_eq!(output(&haps[0].value), [442.0; 2], "{source}");
    }
    let haps = evaluate(
        &runtime,
        "new Pattern(state => freq(110).FX(worklet('audioin().mul(sFreq).out()')).query(state)).freq(220).octave(1)",
    );
    assert_eq!(haps.len(), 1);
    let Value::List(effects) = haps[0].value.get("FX").expect("effects") else {
        panic!("effect list")
    };
    assert_eq!(output(&effects[0]), [110.0; 2]);
    runtime.run_gc();
    let again = runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("dynamic query after collection");
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].value, haps[0].value);
}

#[test]
fn value_callbacks_compile_dynamically_created_sources_and_effects() {
    let runtime = runtime();
    let haps = evaluate(
        &runtime,
        "pure(0).fmap(() => ({freq: 110, workletSrc: 'n(sFreq + pat[0]).out()', workletInputs: [2], FX: [{workletSrc: 'audioin().mul(sFreq).out()'}]})).freq(220).octave(1)",
    );
    assert_eq!(haps.len(), 1);
    assert_eq!(output(&haps[0].value), [442.0; 2]);
    let Value::List(effects) = haps[0].value.get("FX").expect("effects") else {
        panic!("effect list")
    };
    assert_eq!(output(&effects[0]), [110.0; 2]);
}

#[test]
fn final_graph_inspection_preserves_ordinary_callback_identity_and_getter_reads() {
    let runtime = runtime();
    let haps = evaluate(
        &runtime,
        r#"
        globalThis.finalGetterReads = 0;
        const event = {
          get gain() { finalGetterReads++; return 0.5; },
          get FX() { finalGetterReads++; return [{gain: 0.25}]; }
        };
        Object.setPrototypeOf(event, {
          get workletSrc() { throw new Error('inherited controls must remain unread'); }
        });
        pure(0).fmap(() => event).fmap(value => {
          if (value !== event) throw new Error('callback value identity changed');
          return value;
        })
        "#,
    );
    assert_eq!(haps.len(), 1);
    assert_eq!(haps[0].value.get("gain").and_then(Value::as_f64), Some(0.5));
    assert_eq!(runtime.get_number("finalGetterReads"), Some(2.0));
    assert!(haps[0].value.get("workletGraph").is_none());
}

#[test]
fn graph_namespace_channels_and_custom_modules_are_isolated() {
    let runtime = runtime();
    let haps = evaluate(
        &runtime,
        "K(() => { const voice = module('voice', x => x.mul(3)); voice(n([1, 2])).out([0, 1]); })",
    );
    assert_eq!(output(&haps[0].value), [3.0, 6.0]);
    let haps = evaluate(&runtime, "K(n(3))");
    assert_eq!(output(&haps[0].value), [0.0; 2]);
}

#[test]
fn graph_sequence_retains_local_scope_with_custom_methods_and_feedback() {
    let runtime = runtime();
    let haps = evaluate(
        &runtime,
        r#"
        K(() => {
          const b = 2;
          Node.prototype.fm = function (modulation) {
            return this.add(modulation).sine().apply2(mul)
              .add(feedback => feedback.delay(0.01).mul(0.25));
          };
          impulse(4).seq(sine(b).range(220, 330))
            .fm(sine(b)).mul(0.1).out();
        }).slow(2)
        "#,
    );
    assert_eq!(haps.len(), 1);
    let source = haps[0]
        .value
        .get("workletGraph")
        .and_then(Value::as_str)
        .expect("compiled graph");
    let program = rustel_kabel::Program::from_json(source).expect("program");
    let mut graph = rustel_kabel::Graph::new(&program, 48_000.0).expect("graph");
    let mut peak = 0.0_f64;
    for frame in 0..4_800 {
        let output = graph.process([0.0; 2], 1.0, f64::from(frame) / 48_000.0);
        assert!(output.into_iter().all(f64::is_finite), "frame {frame}");
        peak = output.into_iter().map(f64::abs).fold(peak, f64::max);
    }
    assert!(peak > 0.01, "graph was silent: peak {peak}");
}

#[test]
fn graph_input_timing_preserves_outer_wholes() {
    let runtime = runtime();
    let haps = evaluate(&runtime, "note(\"c3 e3\").K(n(S(\"1 2 3\")).out())");
    let values: Vec<_> = haps
        .iter()
        .map(|hap| {
            (
                hap.whole.expect("whole").begin,
                hap.whole.unwrap().end,
                output(&hap.value)[0],
            )
        })
        .collect();
    assert_eq!(
        values,
        vec![
            (Fraction::ZERO, Fraction::new(1, 2), 1.0),
            (Fraction::ZERO, Fraction::new(1, 2), 2.0),
            (Fraction::new(1, 2), Fraction::ONE, 2.0),
            (Fraction::new(1, 2), Fraction::ONE, 3.0)
        ]
    );
    let partial = runtime
        .query(Slot::Active, 0, Fraction::new(1, 3), Fraction::new(1, 2))
        .expect("partial query");
    assert_eq!(partial.len(), 1);
    assert_eq!(output(&partial[0].value), [2.0; 2]);
    assert_eq!(partial[0].whole.unwrap().begin, Fraction::ZERO);
    assert_eq!(partial[0].whole.unwrap().end, Fraction::new(1, 2));
    let slow = evaluate(&runtime, "note(\"c3 e3\").K(n(S(\"1 2 3\")).out()).slow(2)");
    assert_eq!(
        slow.iter()
            .map(|hap| output(&hap.value)[0])
            .collect::<Vec<_>>(),
        [1.0, 2.0]
    );
    assert!(
        slow.iter()
            .all(|hap| hap.whole.unwrap().end == Fraction::ONE)
    );
}

#[test]
fn ordinary_callback_effects_do_not_resolve_a_graph_pitch() {
    let runtime = runtime();
    let haps = evaluate(
        &runtime,
        "pure({note: 'not a pitch', FX: [{gain: 0.5}]}).fmap(x => x)",
    );
    assert_eq!(haps.len(), 1);
    assert!(haps[0].value.get("workletGraph").is_none());
}

#[test]
fn graph_helpers_outside_k_report_the_required_context() {
    let runtime = runtime();
    for source in ["S(1)", "audioin()"] {
        let error = runtime
            .evaluate_score(source, &TranspileOptions::default())
            .expect_err("outside graph");
        assert!(error.contains("inside K()"), "{error}");
    }
}
