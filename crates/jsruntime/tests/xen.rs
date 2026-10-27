use rustel_core::{Hap, Value};
use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, Slot};
use rustel_transpiler::TranspileOptions;

fn runtime(source: &str) -> JsRuntime {
    let runtime = JsRuntime::new().expect("the runtime builds");
    runtime
        .install_semantic_bindings()
        .expect("the bindings install");
    runtime
        .evaluate_score(source, &TranspileOptions::default())
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    runtime
}

fn query(runtime: &JsRuntime, begin: Fraction, end: Fraction) -> Vec<Hap> {
    runtime
        .query(Slot::Active, 0, begin, end)
        .expect("the query succeeds")
}

fn frequencies(haps: &[Hap]) -> Vec<f64> {
    haps.iter()
        .map(|hap| {
            hap.value
                .get("freq")
                .and_then(Value::as_f64)
                .expect("every hap carries a numeric freq")
        })
        .collect()
}

#[test]
fn custom_ratio_forms_wrap_octaves_and_remain_host_free() {
    for source in [
        "i(\"-1 0 1 2 3\").xen([1, '3:2'])",
        "xen(['1:1', [3, 2]], i(\"-1 0 1 2 3\"))",
        "xen([pure(1), pure('3:2')])(i(\"-1 0 1 2 3\"))",
        "i(\"-1 0 1 2 3\").xen([\"1:1\", \"3:2\"])",
    ] {
        let pattern = {
            let runtime = runtime(source);
            let pattern = runtime.active_pattern().unwrap();
            assert!(pattern.is_pure(), "{source}");
            assert!(pattern.reachable_callbacks().is_empty(), "{source}");
            pattern
        };
        assert_eq!(
            frequencies(&pattern.query_arc(Fraction::ZERO, Fraction::ONE)),
            [165.0, 220.0, 330.0, 440.0, 660.0],
            "{source}"
        );
    }
}

#[test]
fn selected_ratio_voices_keep_source_spans_across_query_slices() {
    let steady = runtime("i(1).gain(0.25).xen([\"1:1\", \"[1:1 3:2]\"])");
    let haps = query(&steady, Fraction::ZERO, Fraction::ONE);
    assert_eq!(frequencies(&haps), [220.0, 330.0]);
    for hap in &haps {
        let whole = hap.whole.unwrap();
        assert_eq!((whole.begin, whole.end), (Fraction::ZERO, Fraction::ONE));
        assert_eq!(hap.value.get("gain").and_then(Value::as_f64), Some(0.25));
    }
    let quarter = Fraction::new(1, 4);
    let three_quarters = Fraction::new(3, 4);
    for hap in &haps {
        assert_eq!(
            (hap.part.begin, hap.part.end),
            (Fraction::ZERO, Fraction::ONE)
        );
    }
    let slice = query(&steady, quarter, three_quarters);
    assert_eq!(frequencies(&slice), [220.0, 330.0]);
    for hap in &slice {
        assert_eq!((hap.part.begin, hap.part.end), (quarter, three_quarters));
    }
    assert_eq!(slice[0].whole, haps[0].whole);
    assert_eq!(slice[1].whole, haps[1].whole);

    let split = runtime("i(1).gain(\"0.25 0.5\").xen([\"1:1\", \"[1:1 3:2]\"])");
    let half = Fraction::new(1, 2);
    for (begin, end) in [(Fraction::ZERO, Fraction::ONE), (quarter, three_quarters)] {
        let haps = query(&split, begin, end);
        assert_eq!(frequencies(&haps), [220.0, 330.0, 220.0, 330.0]);
        for (index, hap) in haps.iter().enumerate() {
            assert_eq!(hap.whole, slice[0].whole);
            let (expected_begin, expected_end, gain) = if index < 2 {
                (begin, half, 0.25)
            } else {
                (half, end, 0.5)
            };
            assert_eq!(
                (hap.part.begin, hap.part.end),
                (expected_begin, expected_end)
            );
            assert_eq!(hap.value.get("gain").and_then(Value::as_f64), Some(gain));
        }
    }
    let first_quarter = query(&split, Fraction::ZERO, quarter);
    assert_eq!(frequencies(&first_quarter), [220.0]);
    assert_eq!(first_quarter[0].part.end, quarter);

    for (degree, expected) in [
        (0, vec![220.0, 220.0]),
        (1, vec![330.0, 293.3333333, 293.3333333]),
    ] {
        let runtime = runtime(&format!(
            "i({degree}).xen([\"[1:1 5:4]\", \"[3:2 4:3 7:4]\"])"
        ));
        let haps = query(&runtime, Fraction::ZERO, Fraction::ONE);
        assert_eq!(frequencies(&haps), expected);
        for hap in haps {
            assert_eq!(hap.whole, slice[0].whole);
            assert_eq!(
                (hap.part.begin, hap.part.end),
                (Fraction::ZERO, Fraction::ONE)
            );
        }
    }
}

#[test]
fn patterned_scales_handle_rests_and_chords() {
    for (source, expected) in [
        ("i(0).xen([\"[1:1 ~]\", 1.5])", vec![220.0]),
        (
            "i(\"0,1\").xen([\"[1:1 5:4]\", 1.5])",
            vec![220.0, 275.0, 330.0],
        ),
        ("i(0).xen([silence, 1.5])", vec![]),
    ] {
        let runtime = runtime(source);
        let mut actual = frequencies(&query(&runtime, Fraction::ZERO, Fraction::ONE));
        actual.sort_by(f64::total_cmp);
        assert_eq!(actual, expected, "{source}");
    }
    for source in [
        "i(1).xen([silence, \"3:2\"])",
        "i(0.5).xen([\"1:1\", \"3:2\"])",
        "i('c3').xen([\"1:1\", \"3:2\"])",
    ] {
        let runtime = runtime(source);
        assert!(
            query(&runtime, Fraction::ZERO, Fraction::ONE).is_empty(),
            "{source}"
        );
        assert!(runtime.take_query_throw().is_some(), "{source}");
    }
}

#[test]
fn ratio_callbacks_and_curried_arrays_keep_their_owners_after_collection() {
    for source in [
        "i(0).xen([pure(1).fmap(x => x + 0.5)])",
        "i(0).xen([pure([3, 2])])",
        "i(0).apply(xen([pure(1).fmap(x => x + 0.5)]))",
        "(() => { const tune = xen(['ratio']); setStringParser(() => pure(1).fmap(x => x + 0.5)); return i(0).apply(tune); })()",
    ] {
        let runtime = runtime(source);
        assert!(!runtime.active_pattern().unwrap().is_pure(), "{source}");
        runtime.run_gc();
        runtime.run_gc();
        assert_eq!(
            frequencies(&query(&runtime, Fraction::ZERO, Fraction::ONE)),
            [330.0],
            "{source}"
        );
        assert_eq!(runtime.query_depth(), 0);
        assert_eq!(rustel_jsruntime::bridge_frame_depth(), 0);
    }

    for (source, reads) in [
        ("i(0).xen([pure(first), pure(second)])", 0.0),
        (
            "i(0).xen([pure([1, 1]), fastcat(pure(second), pure(first))])",
            1.0,
        ),
    ] {
        let runtime = runtime(&format!(
            "(() => {{
                globalThis.ratioReads = 0;
                const first = [1, 1];
                const second = [3, 2];
                Object.defineProperty(first, 0, {{ get() {{ throw new Error('ratio getter'); }} }});
                Object.defineProperty(second, 0, {{ get() {{ globalThis.ratioReads++; return 3; }} }});
                return {source};
            }})()"
        ));
        assert!(query(&runtime, Fraction::ZERO, Fraction::ONE).is_empty());
        assert!(
            runtime
                .take_query_throw()
                .is_some_and(|message| message.contains("ratio getter")),
            "{source}"
        );
        assert_eq!(runtime.get_number("ratioReads"), Some(reads), "{source}");
    }
}

#[test]
fn custom_ratio_arrays_reject_unbounded_sparse_lengths() {
    let runtime = runtime(
        "(() => { try { i(0).xen(new Array(2 ** 31)); } catch (error) { globalThis.xenError = error.name; } return silence; })()",
    );
    assert_eq!(
        runtime.get_string("xenError").as_deref(),
        Some("RangeError")
    );
}

#[test]
fn selected_random_ratios_receive_the_query_seed() {
    let direct = runtime("rand.add(1).segment(4).mul(220).freq().seed(42)");
    let tuned = runtime("i(0).xen([rand.add(1).segment(4)]).seed(42)");
    let expected = frequencies(&query(&direct, Fraction::ZERO, Fraction::ONE));
    let actual = frequencies(&query(&tuned, Fraction::ZERO, Fraction::ONE));
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert!((actual - expected).abs() < 1e-6);
    }
}

#[test]
fn numeric_scale_keeps_one_voice_for_notes_longer_than_a_cycle() {
    for source in ["i(0).slow(2).xen([1, 1.5])", "xen([1, 1.5], i(0).slow(2))"] {
        let runtime = runtime(source);
        let haps = query(&runtime, Fraction::ZERO, Fraction::from(2));
        assert_eq!(frequencies(&haps), [220.0], "{source}");
        assert_eq!(haps[0].part.end, Fraction::from(2));
        assert_eq!(haps[0].whole.unwrap().end, Fraction::from(2));
    }
}
