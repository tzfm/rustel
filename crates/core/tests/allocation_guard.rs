//! A hostile or mistaken factor must not be able to exhaust memory.
//!
//! `TimeSpan::span_cycles` pushes one entry per cycle, so its allocation is
//! driven by the query span, which comes from user input. If
//! `Fraction::from_f64` saturated `1e300 as i128` to `i128::MAX`, a query of
//! `fast(1e300)` could try to materialize about 10^38 spans.
//! Two independent checks prevent that:
//!
//!   1. `Fraction::from_f64` refuses floats that do not fit `i128` exactly,
//!      instead of fabricating `i128::MAX`;
//!   2. `span_cycles` refuses a span wider than `MAX_QUERY_SPAN_CYCLES`,
//!      routed through the query-error channel so the `queryArc` boundary
//!      turns it into an empty result.
//!
//! These tests run the complete query path while keeping the refusal before
//! allocation.

use rustel_core::register::default_registry;
use rustel_core::*;
use rustel_fraction::Fraction;

fn fast_by(factor: f64) -> Vec<Hap> {
    let registry = default_registry();
    let pattern = registry
        .get("fast")
        .expect("fast registered")
        .call(&[pure(Value::F64(factor))], pure(Value::Str("bd".into())));
    pattern.query_arc(Fraction::ZERO, Fraction::ONE)
}

#[test]
fn astronomically_large_factors_yield_nothing_instead_of_exhausting_memory() {
    for factor in [1e300_f64, 1e30, 1e20, 1e12] {
        let haps = fast_by(factor);
        assert!(
            haps.is_empty(),
            "fast({factor:e}) must refuse, not materialise {} haps",
            haps.len()
        );
    }
}

#[test]
fn slow_by_a_denormal_is_equally_refused() {
    // `slow(x)` is `fast(1/x)`, so a tiny factor is the same bomb.
    let registry = default_registry();
    let pattern = registry
        .get("slow")
        .expect("slow registered")
        .call(&[pure(Value::F64(1e-30))], pure(Value::Str("bd".into())));
    assert!(pattern.query_arc(Fraction::ZERO, Fraction::ONE).is_empty());
}

#[test]
fn ordinary_factors_are_unaffected() {
    for (factor, expected) in [(1.0, 1), (2.0, 2), (8.0, 8), (64.0, 64)] {
        assert_eq!(
            fast_by(factor).len(),
            expected,
            "fast({factor}) must still work"
        );
    }
}

#[test]
fn a_wide_but_legitimate_query_still_works() {
    // Well under the bound: the guard must not clip real offline renders.
    let haps = pure(Value::Str("bd".into())).query_arc(Fraction::ZERO, Fraction::int(5_000));
    assert_eq!(haps.len(), 5_000);
}
