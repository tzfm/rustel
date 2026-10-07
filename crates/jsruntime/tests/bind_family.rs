/*
rustel-jsruntime - the bind family
Rust test harness:
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! `bind`, `outerBind`, `innerBind`, `squeezeBind`, `polyBind`.
//!
//! Upstream declares five (`packages/core/pattern.mjs:271-401`). Three were
//! missing here, which stayed invisible because the corpus is scores and
//! these are what you reach for when you WRITE a function: `outerBind` alone
//! carries `vstruct`, `cue`, `oncue` and `chrd` in one published prebake.
//!
//! What separates them is the WHOLE, not the part, so every test here reads
//! wholes. A test on parts passes for all five and proves nothing.

use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, Slot};
use rustel_transpiler::TranspileOptions;

fn wholes(source: &str) -> Vec<String> {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.evaluate_score(source, &TranspileOptions::default())
        .unwrap_or_else(|e| panic!("{source}: {e}"));
    rt.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("query")
        .iter()
        .map(|hap| match &hap.whole {
            Some(whole) => format!("{}..{}", whole.begin, whole.end),
            None => "analog".to_owned(),
        })
        .collect()
}

#[test]
fn bind_intersects_the_wholes() {
    // `bindWhole((a, b) => a.intersection_e(b))` - pattern.mjs:271.
    assert_eq!(
        wholes(r#""0 1".bind(v => n(v).fast(3))"#),
        vec!["0/1..1/3", "1/3..1/2", "1/2..2/3", "2/3..1/1"]
    );
}

#[test]
fn inner_bind_takes_the_wholes_from_the_inner_pattern() {
    // `bindWhole((_, b) => b)` - pattern.mjs:298.
    assert_eq!(
        wholes(r#""0 1".innerBind(v => n(v).fast(3))"#),
        vec!["0/1..1/3", "1/3..2/3", "1/3..2/3", "2/3..1/1"]
    );
}

#[test]
fn outer_bind_takes_the_wholes_from_the_outer_pattern() {
    // `bindWhole((a) => a)` - pattern.mjs:287. The outer structure survives,
    // which is what makes it the one a struct-shaped helper reaches for.
    assert_eq!(
        wholes(r#""0 1".outerBind(v => n(v).fast(3))"#),
        vec!["0/1..1/2", "0/1..1/2", "1/2..1/1", "1/2..1/1"]
    );
}

#[test]
fn every_bind_upstream_declares_is_callable() {
    // The gap that hid: `squeezeJoin` and `outerJoin` were both present, so
    // the machinery was there and only the wrappers were missing.
    for name in ["bind", "outerBind", "innerBind", "squeezeBind", "polyBind"] {
        let source = format!("n(1).{name}(v => n(v))");
        assert!(
            !wholes(&source).is_empty(),
            "{name} produced no haps at all"
        );
    }
}

#[test]
fn a_struct_shaped_helper_written_with_outer_bind_works() {
    // Reduced from a real-world nested-binding score, the reason this was noticed.
    // The struct string sets the rhythm; its values ride along as velocity.
    let source = r#"
        register('vstructOwn', (ipat, pat) =>
          ipat.outerBind(vel => pat.keepif.out(Math.ceil(vel)).velocity(vel)), false)
        $: n("0").s("piano").vstructOwn("1 0.5 ~ 0.7")
    "#;
    assert_eq!(wholes(source), vec!["0/1..1/4", "1/4..1/2", "3/4..1/1"]);
}

#[test]
fn free_bind_wrappers_return_overridden_method_values_verbatim() {
    let runtime = JsRuntime::new().expect("runtime");
    runtime
        .install_semantic_bindings()
        .expect("semantic bindings");
    runtime
        .eval(
            r#"
              const marker = {};
              globalThis.__freeBindReturn = Number(
                polyBind(() => {}, { polyBind() { return 42; } }) === 42
                && stepBind(() => {}, { stepBind() { return marker; } }) === marker
              );
            "#,
        )
        .expect("free bind wrappers");
    assert_eq!(runtime.get_number("__freeBindReturn"), Some(1.0));
}
