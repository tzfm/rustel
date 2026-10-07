/*
rustel-jsruntime - the core utility globals
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! `@strudel/core` exports every helper in `util.mjs`, and the repl's
//! `evalScope(core, …)` puts all of them in a score's scope.
//!
//! The gap stayed invisible because the corpus is scores, and these are what
//! you reach for when you WRITE a function: one published prebake died on
//! `flatten`, another on `clamp`.

use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, Slot};
use rustel_transpiler::TranspileOptions;

/// The single `n` the expression yields, or the error it threw.
fn n_of(expression: &str) -> Result<f64, String> {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.evaluate_score(&format!("n({expression})"), &TranspileOptions::default())
        .map_err(|e| e.to_string())?;
    let haps = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .map_err(|e| e.to_string())?;
    haps.first()
        .and_then(|hap| hap.value.as_object()?.get("n")?.as_f64())
        .ok_or_else(|| "no n".to_owned())
}

fn n(expression: &str) -> f64 {
    n_of(expression).unwrap_or_else(|e| panic!("{expression}: {e}"))
}

#[test]
fn the_list_and_function_helpers_behave_as_upstream_writes_them() {
    assert_eq!(n("flatten([[1,2],[3]]).length"), 3.0);
    assert_eq!(n("clamp(5, 0, 1)"), 1.0);
    // compose reverses, pipe does not: the two must not be the same function.
    assert_eq!(n("compose(x => x+1, x => x*2)(3)"), 8.0);
    assert_eq!(n("pipe(x => x+1, x => x*2)(3)"), 7.0);
    assert_eq!(n("rotate([1,2,3], 1)[0]"), 2.0);
    assert_eq!(n("listRange(2,5).length"), 4.0);
    assert_eq!(n("zipWith((a,b)=>a+b,[1,2],[10,20])[1]"), 22.0);
    assert_eq!(n("splitAt(1,[1,2,3])[1].length"), 2.0);
    // Overlapping and consecutive: [1,2] [2,3] [3,4], not two chunks.
    assert_eq!(n("pairs([1,2,3,4]).length"), 3.0);
    assert_eq!(n("pairs([1,2,3,4])[1][0]"), 2.0);
    assert_eq!(n("removeUndefineds([1,undefined,2]).length"), 2.0);
    assert_eq!(n("averageArray([1,2,3])"), 2.0);
    assert_eq!(n("objectMap({a:1,b:2}, v => v*2).b"), 4.0);
    assert_eq!(n("constant(7, 9)"), 7.0);
}

#[test]
fn the_numeric_helpers_behave_as_upstream_writes_them() {
    // Floor-mod, so a negative wraps rather than staying negative.
    assert_eq!(n("_mod(-1, 12)"), 11.0);
    assert_eq!(n("cycleToSeconds(2, 0.5)"), 4.0);
    assert_eq!(n("getSoundIndex(5, 3)"), 2.0);
    assert_eq!(n("getAccidentalsOffset('##')"), 2.0);
    assert_eq!(n("getEventOffsetMs(1.5, 1.0)"), 500.0);
    // Single quotes: a double-quoted string is mini-notation and would arrive
    // as a Pattern, which is not a numeral.
    assert_eq!(n("parseNumeral('3')"), 3.0);
}

#[test]
fn uniq_works_here_although_upstream_throws() {
    // Upstream's `uniq` calls `seen.hasOwn(item)`. `hasOwn` is a static on
    // `Object`, so upstream throws a TypeError on every non-empty array.
    // No score can depend on that throw, so `uniq` works here.
    assert_eq!(n("uniq([1,1,2,2,3]).length"), 3.0);
    assert_eq!(n("uniqsort([3,1,1,2]).join('-') === '1-2-3' ? 1 : 0"), 1.0);
}

#[test]
fn nan_fallback_returns_the_fallback_instead_of_throwing_over_a_warning() {
    // Upstream calls `logger(...)` here unguarded. Where `logger` is not
    // defined, an unguarded call raises a ReferenceError on the input that
    // the function must absorb. A missing warning must not become a missing
    // fallback.
    assert_eq!(n("nanFallback(NaN, 7)"), 7.0);
    assert_eq!(n("nanFallback(5, 7)"), 5.0);
}

#[test]
fn what_reads_host_state_is_deliberately_absent() {
    // A query must not depend on a wall clock or a keyboard, and the four
    // base64/hash helpers exist to build strudel.cc share links out of
    // `atob`/`btoa`, which this engine does not have and has no use for.
    for name in [
        "getCurrentKeyboardState",
        "getPerformanceTimeSeconds",
        "base64ToUnicode",
        "unicodeToBase64",
        "code2hash",
        "hash2code",
    ] {
        assert_eq!(
            n(&format!("typeof {name} === 'undefined' ? 1 : 0")),
            1.0,
            "{name} should not be defined"
        );
    }
}
