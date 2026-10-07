/*
rustel-ext - undefined_aeon extension
Copyright (C) 2026 Rustel contributors

The embedded inspire recipe is by tzfm, under the alias undefined_aeon.

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! The explicit native extension checked against the JavaScript recipe that
//! introduced it. This is semantic parity, not source-pattern recognition.

use rustel_core::Hap;
use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, Slot};
use rustel_transpiler::TranspileOptions;

const REFERENCE: &str = r#"
register('inspireReference', (_scale, density, octaves, seed, bars, x) => {
  return x.n(rand.range(0,pure(12).mul(octaves)))
    .scale(_scale)
    .sometimesBy(pure(1).sub(density), x => x.mask(rand.round()))
    .early(rand2.range(-0.001, 0.001))
    .fill()
    .rib(seed, bars)
})
"#;

fn fresh_runtime() -> JsRuntime {
    let runtime = JsRuntime::new().expect("runtime");
    runtime.install_semantic_bindings().expect("bindings");
    runtime
}

fn runtime(source: &str) -> JsRuntime {
    let runtime = fresh_runtime();
    runtime
        .evaluate_score(source, &TranspileOptions::default())
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    runtime
}

fn assert_musical_contract(expected: &[Hap], actual: &[Hap], label: &str) {
    assert_eq!(actual.len(), expected.len(), "{label}: hap count");
    for (index, (expected, actual)) in expected.iter().zip(actual).enumerate() {
        assert_eq!(actual.whole, expected.whole, "{label}: whole {index}");
        assert_eq!(actual.part, expected.part, "{label}: part {index}");
        assert_eq!(actual.value, expected.value, "{label}: value {index}");
        assert_eq!(
            actual.scale_context(),
            expected.scale_context(),
            "{label}: scale context {index}"
        );
        assert_eq!(
            actual.edo_size_context().map(f64::to_bits),
            expected.edo_size_context().map(f64::to_bits),
            "{label}: EDO context {index}"
        );
        assert_eq!(
            actual.tags_context(),
            expected.tags_context(),
            "{label}: tags {index}"
        );
        assert_eq!(
            actual.log_line(),
            expected.log_line(),
            "{label}: log context {index}"
        );
    }
}

#[test]
fn undefined_aeons_native_inspire_matches_the_authored_javascript_recipe() {
    let cases = [
        (r#"s("piano").seg(8)"#, r#""<ab:major>", 0.4, 2, 10, 4"#),
        (
            r#"s("piano").seg(16).gain("0.2 0.4")"#,
            r#""<d:minor f:dorian>", 0.2, 1, 60, 4"#,
        ),
        (
            r#"s("piano").seg(4)"#,
            r#""<ab:major>", "<0.2 0.8>", 2, 10, 4"#,
        ),
    ];
    let spans = [
        (Fraction::ZERO, Fraction::ONE),
        (Fraction::new(1, 3), Fraction::new(7, 3)),
        (Fraction::int(-2), Fraction::int(2)),
    ];

    for (case_index, (receiver, args)) in cases.iter().enumerate() {
        let reference = runtime(&format!(
            "{REFERENCE}\n$: {receiver}.inspireReference({args})"
        ));
        let native = runtime(&format!("$: {receiver}.inspire({args})"));
        assert!(
            native
                .active_pattern()
                .expect("active undefined_aeon inspire pattern")
                .is_pure(),
            "case {case_index}: the explicit native extension stays native"
        );

        for (begin, end) in spans {
            let expected = reference
                .query(Slot::Active, 0, begin, end)
                .expect("reference query");
            let actual = native
                .query(Slot::Active, 0, begin, end)
                .expect("native query");
            assert_musical_contract(
                &expected,
                &actual,
                &format!("case {case_index}, span {begin}..{end}"),
            );
        }
    }
}

#[test]
fn a_score_can_override_inspire_like_every_other_extension() {
    let runtime = runtime(
        r#"
        register('inspire', (_scale, _density, _octaves, _seed, _bars, x) => x.s("sd"))
        $: s("bd").inspire("c:major", 1, 1, 0, 1)
        "#,
    );
    let haps = runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("query");
    assert_eq!(haps.len(), 1);
    assert_eq!(
        haps[0].value.get("s").and_then(|value| value.as_str()),
        Some("sd")
    );
}
