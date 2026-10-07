/*
combinators.rs - pattern combinator bodies
Includes combinators adapted from Strudel packages/core/pattern.mjs
and packages/tonal/tonal.mjs.
Copyright (C) 2022, 2025 Strudel contributors

Rust implementation and additions:
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! Combinator bodies. Each takes already-resolved arguments; the
//! patternification wrappers come from `native_combinator!`.
//!
//! Every body is generic over [`PatOps`] so one definition serves both the
//! `PurePattern` and `Pattern` instantiations - a concrete `Pattern` version
//! would make the purity-preserving view a hand-maintained copy.

use crate::compose::{Alignment, ComposeOp};
use crate::ops::PatOps;
use crate::util;
use crate::value::Value;
use rustel_fraction::Fraction;
use std::cmp::Ordering;

fn host_memory_refusal<P: PatOps>() -> P {
    P::pat_query_limit(crate::QueryLimit::HostMemory)
}

/// Report arithmetic outside the native fraction range under the caller's
/// operation name. Mark an active query immediately; otherwise carry the
/// refusal on the returned pattern until it is queried.
fn native_fraction_refusal<P: PatOps>(operation: &'static str) -> P {
    P::pat_query_limit(crate::mark_stepwise_refusal(
        crate::QueryLimit::NativeFraction { operation },
    ))
}

/// `pat`'s step count times `factor`, `None` for a step-less `pat`, or the
/// [`native_fraction_refusal`] for `operation` when the product does not fit.
fn scaled_steps<P: PatOps>(
    pat: &P,
    factor: Fraction,
    operation: &'static str,
) -> Result<Option<Fraction>, P> {
    pat.pat_steps()
        .map(|steps| {
            steps
                .checked_mul(factor)
                .ok_or_else(|| native_fraction_refusal(operation))
        })
        .transpose()
}

/// Implements [`PatOps::fast`] and `slow`, and the speed changes in `ply`,
/// `extend`, `replicate` and `pace`, with both time maps checked.
///
/// Even a representable factor can overflow when applied to a late query or
/// composed with another speed change. On overflow, mark `NativeFraction`
/// and substitute zero until the query boundary reports the refusal. Step
/// metadata is preserved.
pub(crate) fn checked_fast<P: PatOps>(pat: &P, factor: Fraction, operation: &'static str) -> P {
    if factor.numer() == 0 {
        // `fast(0)` is silence; there is no arithmetic to check.
        return P::pat_silence();
    }
    let refused = move || {
        crate::mark_stepwise_refusal(crate::QueryLimit::NativeFraction { operation });
        Fraction::ZERO
    };
    pat.with_query_time(move |time| time.checked_mul(factor).unwrap_or_else(refused))
        .with_hap_time(move |time| time.checked_div(factor).unwrap_or_else(refused))
        .set_steps(pat.pat_steps())
}

/// Implements [`PatOps::zoom`] with its query-span and hap-span maps checked.
///
/// Relative to the span's starting cycle, query times map to
/// `t * width + begin` and event times map back to `(t - begin) / width`.
/// Check both maps, the window width, and the scaled step count: fitting
/// bounds alone do not guarantee that these results fit. Refusals name the
/// caller's `operation`, including when used by `take` or `drop`.
pub(crate) fn checked_zoom<P: PatOps>(
    pat: &P,
    begin: Fraction,
    end: Fraction,
    operation: &'static str,
) -> P {
    if begin >= end {
        // An empty slot is silence; there is no arithmetic to check.
        return P::pat_silence();
    }
    // Validate the window and step metadata before building the query maps.
    let Some(width) = end.checked_sub(begin) else {
        return native_fraction_refusal(operation);
    };
    let steps = match scaled_steps(pat, width, operation) {
        Ok(steps) => steps,
        Err(refused) => return refused,
    };
    pat.with_query_span(move |span| {
        checked_cycle_map(span, operation, |t| {
            t.checked_mul(width)?.checked_add(begin)
        })
    })
    .with_hap_span(move |span| {
        checked_cycle_map(span, operation, |t| {
            t.checked_sub(begin)?.checked_div(width)
        })
    })
    .split_queries()
    .set_steps(steps)
}

/// `TimeSpan::with_cycle` with every step checked. On overflow it refuses
/// with `NativeFraction { operation }` inside the query and returns the empty
/// span at zero, as [`checked_fast`] substitutes time zero.
fn checked_cycle_map(
    span: &crate::TimeSpan,
    operation: &'static str,
    within_cycle: impl Fn(Fraction) -> Option<Fraction>,
) -> crate::TimeSpan {
    let sam = span.begin.sam();
    let map = |time: Fraction| {
        let mapped = within_cycle(time.checked_sub(sam)?)?;
        sam.checked_add(mapped)
    };
    match (map(span.begin), map(span.end)) {
        (Some(begin), Some(end)) => crate::TimeSpan::new(begin, end),
        _ => {
            crate::mark_stepwise_refusal(crate::QueryLimit::NativeFraction { operation });
            crate::TimeSpan::new(Fraction::ZERO, Fraction::ZERO)
        }
    }
}

/// Repeats each event `factor` times within its span; steps scale to match.
pub fn ply<P: PatOps>(pat: &P, factor: Fraction) -> P {
    // Step metadata can overflow at construction; event-time scaling must
    // also be checked when the pattern is queried.
    let steps = match scaled_steps(pat, factor, "ply") {
        Ok(steps) => steps,
        Err(refused) => return refused,
    };
    pat.squeeze_bind(move |v| checked_fast(&P::pat_pure(v.clone()), factor, "ply"))
        .set_steps(steps)
}

/// [`ply`] whose `i`th repeat of each event has `function` applied `i`
/// times; steps scale by `factor`.
pub fn ply_with<P: PatOps>(pat: &P, factor: Fraction, function: Option<&FunctionRef>) -> P {
    let count = factor.to_f64().max(0.0).ceil() as u64;
    if let Some(refused) = refuse_oversized_parts("plyWith", count) {
        return refused;
    }
    let steps = match scaled_steps(pat, factor, "plyWith") {
        Ok(steps) => steps,
        Err(refused) => return refused,
    };
    let function = function.cloned();
    pat.squeeze_bind(move |value| {
        let mut parts = Vec::with_capacity(count as usize);
        for index in 0..count {
            let mut part = P::pat_pure(value.clone());
            for _ in 0..index {
                part = call(&part, function.as_ref());
            }
            parts.push(part);
        }
        P::pat_slowcat(parts).fast(factor)
    })
    .set_steps(steps)
}

/// [`ply`] whose repeats after the first are `function` called with the
/// event and the repeat's index; steps scale by `factor`.
pub fn ply_for_each<P: PatOps>(pat: &P, factor: Fraction, function: Option<&FunctionRef>) -> P {
    let count = factor.to_f64().max(0.0).ceil() as u64;
    if let Some(refused) = refuse_oversized_parts("plyForEach", count) {
        return refused;
    }
    let steps = match scaled_steps(pat, factor, "plyForEach") {
        Ok(steps) => steps,
        Err(refused) => return refused,
    };
    let function = function.cloned();
    pat.squeeze_bind(move |value| {
        if count == 0 {
            return P::pat_silence();
        }
        let base = P::pat_pure(value.clone());
        let mut parts = vec![base.clone()];
        if let Some(function) = function.as_ref() {
            parts.extend(P::apply_fn_indexed_batch(
                (1..count)
                    .map(|index| (base.clone(), index as i64))
                    .collect(),
                function,
            ));
        } else {
            parts.extend((1..count).map(|_| base.clone()));
        }
        P::pat_slowcat(parts).fast(factor)
    })
    .set_steps(steps)
}

fn slice_bound(slices: &Value, index: &Value, offset: i64) -> Value {
    if let Value::List(bounds) = slices {
        let index = as_js_number(index) + offset as f64;
        if index.is_finite() && index.fract() == 0.0 && index >= 0.0 {
            return bounds
                .get(index as usize)
                .cloned()
                .unwrap_or(Value::Undefined);
        }
        return Value::Undefined;
    }
    Value::F64((as_js_number(index) + offset as f64) / as_js_number(slices))
}

fn sliced_value(slices: &Value, index: &Value, value: &Value) -> Value {
    let mut controls = crate::OrderedMap::from_entries([
        ("begin".into(), slice_bound(slices, index, 0)),
        ("end".into(), slice_bound(slices, index, 1)),
        ("_slices".into(), slices.clone()),
    ]);
    match crate::materialize_js_value(value) {
        Value::Object(object) => {
            for (name, value) in &object {
                controls.insert(name.clone(), value.clone());
            }
        }
        Value::List(values) => {
            for (index, value) in values.into_iter().enumerate() {
                controls.insert(index.to_string(), value);
            }
        }
        Value::Function(_) | Value::Pattern(_) | Value::JsValue(_) | Value::Haps(_) => {}
        primitive => {
            controls.insert("s".into(), primitive);
        }
    }
    Value::Object(controls)
}

/// Cuts each `pattern` event into `slices` parts, or at the listed bounds, and
/// plays the part each `indices` event names; steps are those of `indices`.
pub fn slice<P: PatOps>(pattern: &P, slices: &P, indices: &P) -> P {
    let pattern = pattern.clone();
    let indices = indices.clone();
    let steps = indices.pat_steps();
    slices
        .inner_bind(move |slices| {
            let slices = slices.to_owned();
            let pattern = pattern.clone();
            indices.outer_bind(move |index| {
                let slices = slices.clone();
                let index = index.to_owned();
                pattern.outer_bind(move |value| P::pat_pure(sliced_value(&slices, &index, value)))
            })
        })
        .set_steps(steps)
}

pub fn scrub<P: PatOps>(pattern: &P, begins: &P) -> P {
    let pattern = pattern.clone();
    begins.outer_bind(move |value| {
        let (begin, multiplier) = match value {
            Value::List(values) => (
                values.first().cloned().unwrap_or(Value::Undefined),
                values.get(1).cloned().unwrap_or(Value::F64(1.0)),
            ),
            value => (value.clone(), Value::F64(1.0)),
        };
        // `begin(...)` is a control, and a control handed an object with
        // an unnamed `value` - `"0.3".color("teal")` is `{ value: 0.3,
        // color: "teal" }` - takes the `value` for its own and keeps the
        // rest of the bag, as strudel's `createParam` does.
        let begin = match begin {
            Value::Object(mut bag) if bag.contains_key("value") => {
                let value = bag.remove("value").unwrap_or(Value::Undefined);
                bag.insert("begin".into(), value);
                Value::Object(bag)
            }
            begin => Value::object([("begin".into(), begin)]),
        };
        let begin = P::pat_pure(begin);
        let speed = P::pat_pure(Value::object([("speed".into(), multiplier)]));
        let clip = P::pat_pure(Value::object([("clip".into(), Value::F64(1.0))]));
        let begun = compose(
            &pattern,
            &begin,
            ComposeOp::Set,
            crate::compose::default_alignment(),
        );
        let sped = compose(
            &begun,
            &speed,
            ComposeOp::Mul,
            crate::compose::default_alignment(),
        );
        compose(
            &sped,
            &clip,
            ComposeOp::Set,
            crate::compose::default_alignment(),
        )
    })
}

/// Squeezes each event into the `[r, 1]` slice of its span.
pub fn press_by<P: PatOps>(pat: &P, r: Fraction) -> P {
    pat.squeeze_bind(move |v| P::pat_pure(v.clone()).compress(r, Fraction::ONE))
}

/// [`press_by`] at 1/2.
pub fn press<P: PatOps>(pat: &P) -> P {
    press_by(pat, Fraction::new(1, 2))
}

/// Samples `pat` at `rate` events per cycle: structure from the pulse
/// pattern, values from `pat` - what turns a continuous signal discrete.
pub fn segment<P: PatOps>(pat: &P, rate: Fraction) -> P {
    let pulse = P::pat_pure(Value::Bool(true)).fast(rate);
    compose(pat, &pulse, ComposeOp::KeepIf, Alignment::Out).set_steps(Some(rate))
}

/// Loops the first `t` of each cycle; negative `t` loops the end. Zero is silence.
pub fn linger<P: PatOps>(pat: &P, t: Fraction) -> P {
    if t == Fraction::ZERO {
        return P::pat_silence();
    }
    if t < Fraction::ZERO {
        return pat.zoom(t.add(Fraction::ONE), Fraction::ONE).slow(t);
    }
    pat.zoom(Fraction::ZERO, t).slow(t)
}

/// Shifts the pattern by `i/times` on cycle `i` (`back` reverses direction).
///
/// The part count truncates: `times = 4` yields offsets `0, 1/4, 2/4, 3/4`,
/// and a fractional `times` below 1 yields no parts, which is silence.
/// Rounding would turn `iter(1/2)` into the identity.
pub fn iter<P: PatOps>(pat: &P, times: Fraction, back: bool) -> P {
    // Truncate through f64, not exact rationals: a decimal infinitesimally
    // below three is exactly 3.0 as an f64 and must yield three parts.
    let count = times.to_f64().trunc();
    if count.is_nan() || count < 1.0 {
        // An empty slowcat queries to nothing.
        return P::pat_slowcat(Vec::new());
    }
    // Bound `count` before it sizes the `Vec`. Without the bound, `iter(1e9)`
    // requests 128 GB. An allocation that large can abort the process, and no
    // panic boundary catches an abort.
    let parts = count.min(u64::MAX as f64) as u64;
    if let Some(refused) = refuse_oversized_parts(if back { "iterBack" } else { "iter" }, parts) {
        return refused;
    }
    let mut pats = Vec::new();
    if pats.try_reserve_exact(parts as usize).is_err() {
        return host_memory_refusal();
    }
    for i in 0..parts {
        let offset = Fraction::int(i128::from(i)).div(times);
        pats.push(if back {
            pat.late(offset)
        } else {
            pat.early(offset)
        });
    }
    P::pat_slowcat(pats)
}

/// Speeds the pattern up AND multiplies sample playback rate. `speed` is
/// multiplied in, not set: an existing `speed` compounds.
pub fn hurry<P: PatOps>(pat: &P, r: f64) -> P {
    let speed = P::pat_pure(Value::object([("speed".to_string(), Value::F64(r))]));
    compose(
        &pat.fast(Fraction::from_f64(r).unwrap_or(Fraction::ONE)),
        &speed,
        ComposeOp::Mul,
        Alignment::In,
    )
}

/// Runs a pattern at an absolute cycles-per-minute rate.
pub fn cpm<P: PatOps>(pat: &P, cpm: Fraction) -> P {
    pat.cpm(cpm)
}

/// Squeezes each event into beat `t` of a `div`-beat cycle.
///
/// Zero `div` is silence, not an error: `compress` refuses a zero-width
/// window at query time anyway. A window outside the native fraction range
/// refuses as `NativeFraction { operation: "beat" }`.
pub fn beat<P: PatOps>(pat: &P, t: Fraction, div: Fraction) -> P {
    if div == Fraction::ZERO {
        return P::pat_silence();
    }
    let window = || {
        let t = t.checked_rem(div)?;
        let b = t.checked_div(div)?;
        let e = t.checked_add(Fraction::ONE)?.checked_div(div)?;
        Some((b, e))
    };
    let Some((b, e)) = window() else {
        return native_fraction_refusal("beat");
    };
    pat.inner_bind(move |x| P::pat_pure(x.clone()).compress(b, e))
}

/// Delays the second half of each of `n` slices by `swing / 2`.
///
/// The offset goes through an inner join over a two-step pattern - a plain
/// `late` would delay everything, not just the second half of each slice.
pub fn swing_by<P: PatOps>(pat: &P, swing: Fraction, n: Fraction) -> P {
    let source_steps = pat.pat_steps();
    let slowed = pat.slow(n);
    let offsets = P::pat_fastcat(vec![
        P::pat_pure(Value::F64(0.0)),
        P::pat_pure(Value::F64(swing.div(Fraction::int(2)).to_f64())),
    ]);
    let shifted = offsets.inner_bind(move |v| {
        let offset = crate::register::value_to_fraction(v).unwrap_or(Fraction::ZERO);
        slowed.late(offset)
    });
    let result = shifted.fast(n);
    if n == Fraction::ZERO {
        result
    } else {
        result.set_steps(source_steps)
    }
}

/// [`swing_by`] at 1/3.
pub fn swing<P: PatOps>(pat: &P, n: Fraction) -> P {
    swing_by(pat, Fraction::new(1, 3), n)
}

/// Alternates forward and reversed cycles. `slowcat_prime`, not `slowcat`:
/// operand cycles are skipped, not stretched across the concatenation.
pub fn palindrome<P: PatOps>(pat: &P) -> P {
    P::pat_slowcat_prime(vec![pat.clone(), pat.rev()])
}

/// Every second cycle, squeezes the pattern into the first half and nudges
/// it right by a quarter. The branch choice stays an inner join over a
/// two-cycle boolean - its whole-selection matters, so the branches are not
/// simply concatenated.
pub fn brak<P: PatOps>(pat: &P) -> P {
    let plain = pat.clone();
    let broken = P::pat_fastcat(vec![pat.clone(), P::pat_silence()]).late(Fraction::new(1, 4));
    let switch = P::pat_slowcat(vec![
        P::pat_pure(Value::Bool(false)),
        P::pat_pure(Value::Bool(true)),
    ]);
    switch.inner_bind(move |v| {
        if v.js_truthy() {
            broken.clone()
        } else {
            plain.clone()
        }
    })
}

/// Loops `cycles` cycles of the pattern starting at `offset`
/// (`keepif` with restart alignment).
pub fn ribbon<P: PatOps>(pat: &P, offset: Fraction, cycles: Fraction) -> P {
    let shifted = pat.early(offset);
    let gate = P::pat_pure(Value::F64(1.0)).slow(cycles);
    compose(&shifted, &gate, ComposeOp::KeepIf, Alignment::Restart)
}

/// Squeezes zoomed `1/n` slices of `pat` in, indexed per event by `ipat`.
/// The slot is `i / n` modulo 1, truncated like fraction.js `mod`, so a
/// negative index's slot starts before the cycle instead of wrapping into it.
/// `npat` and `ipat` arrive as patterns; registration does not fold them.
/// A window outside the native fraction range refuses as
/// `NativeFraction { operation: "bite" }`.
pub fn bite<P: PatOps>(pat: &P, npat: &P, ipat: &P) -> P {
    let pairs = ipat.app_left_with(npat.clone(), |i, n| Value::List(vec![i.clone(), n.clone()]));
    let pat = pat.clone();
    pairs.squeeze_bind(move |v| {
        let Value::List(items) = v else {
            return P::pat_silence();
        };
        let (Some(i), Some(n)) = (
            items.first().and_then(crate::register::value_to_fraction),
            items.get(1).and_then(crate::register::value_to_fraction),
        ) else {
            return P::pat_silence();
        };
        if n == Fraction::ZERO {
            return P::pat_silence();
        }
        let window = || {
            let a = i.checked_div(n)?.checked_rem(Fraction::ONE)?;
            let b = a.checked_add(Fraction::ONE.checked_div(n)?)?;
            Some((a, b))
        };
        let Some((a, b)) = window() else {
            return native_fraction_refusal("bite");
        };
        pat.zoom(a, b)
    })
}

pub use crate::compose::compose;

/// `pat.struct(other)` = `keepif.out`.
pub fn struct_with<P: PatOps>(pat: &P, other: &P) -> P {
    compose(pat, other, ComposeOp::KeepIf, Alignment::Out)
}

/// `pat.mask(other)` = `keepif.in`.
pub fn mask<P: PatOps>(pat: &P, other: &P) -> P {
    compose(pat, other, ComposeOp::KeepIf, Alignment::In)
}

// -- numeric value maps -----------------------------------------------------

/// Maps `f` over values parsed as numerals.
pub fn map_numeral<P: PatOps>(pat: &P, f: impl Fn(f64) -> f64 + Send + Sync + 'static) -> P {
    pat.fmap(move |v| match util::parse_numeral(v) {
        Ok(x) => Value::F64(f(x)),
        // A parse failure aborts the query: `signal_query_error` empties the arc.
        Err(_) => {
            let shown = v.show();
            crate::signal_query_error(move || format!("cannot parse as numeral: \"{shown}\""));
            Value::F64(f64::NAN)
        }
    })
}

/// Unipolar to bipolar. JS number coercion, not numeral parsing: a string
/// coerces at the arithmetic.
pub fn to_bipolar<P: PatOps>(pat: &P) -> P {
    pat.fmap(|v| Value::F64(as_js_number(v) * 2.0 - 1.0))
}

pub fn from_bipolar<P: PatOps>(pat: &P) -> P {
    pat.fmap(|v| Value::F64((as_js_number(v) + 1.0) / 2.0))
}

/// Reduces a list `[a, b, c]` to `a / b / c`; anything else passes through.
pub fn ratio<P: PatOps>(pat: &P) -> P {
    pat.fmap(|v| match v {
        Value::List(items) if !items.is_empty() => {
            let mut acc = as_js_number(&items[0]);
            for n in &items[1..] {
                acc /= as_js_number(n);
            }
            Value::F64(acc)
        }
        other => other.clone(),
    })
}

/// Truthiness negation of each value.
pub fn invert<P: PatOps>(pat: &P) -> P {
    pat.fmap(|v| Value::Bool(!v.js_truthy()))
}

/// Rescales a unipolar pattern to `[min, max]`. Built from the `mul`/`add`
/// composers so control objects are respected.
pub fn range<P: PatOps>(pat: &P, min: f64, max: f64) -> P {
    let scaled = compose(
        pat,
        &P::pat_pure(Value::F64(max - min)),
        ComposeOp::Mul,
        Alignment::In,
    );
    compose(
        &scaled,
        &P::pat_pure(Value::F64(min)),
        ComposeOp::Add,
        Alignment::In,
    )
}

/// Exponential [`range`]: linear in log space.
pub fn rangex<P: PatOps>(pat: &P, min: f64, max: f64) -> P {
    // log/exp via crate::fdlibm - the platform libm differs from V8 by a ULP
    // on some inputs, which shows in printed hap values.
    range(pat, crate::fdlibm::log(min), crate::fdlibm::log(max))
        .fmap(|v| Value::F64(crate::fdlibm::exp(as_js_number(v))))
}

/// [`range`] for a bipolar input.
pub fn range2<P: PatOps>(pat: &P, min: f64, max: f64) -> P {
    range(&from_bipolar(pat), min, max)
}

// -- combinators that take a pattern TRANSFORMER -----------------------------
//
// The transformer arrives inside a [`Value`]; a missing or non-function
// argument is the identity.

use crate::value::FunctionRef;

/// Apply `f` if present, else the identity.
fn call<P: PatOps>(pat: &P, f: Option<&FunctionRef>) -> P {
    match f {
        Some(f) => pat.apply_fn(f),
        None => pat.clone(),
    }
}

/// Applies `f` when `on` - patternified, so the branch is chosen per hap of
/// the boolean.
pub fn when<P: PatOps>(pat: &P, on: bool, f: Option<&FunctionRef>) -> P {
    if on { call(pat, f) } else { pat.clone() }
}

/// Applies `f` to the pattern.
pub fn apply<P: PatOps>(pat: &P, f: Option<&FunctionRef>) -> P {
    call(pat, f)
}

/// Applies `f` `n` times.
pub fn apply_n<P: PatOps>(pat: &P, n: i64, f: Option<&FunctionRef>) -> P {
    // A loop rather than a `Vec`, but the same class of hole: every iteration
    // wraps another node, so the graph is built in full BEFORE the query-time
    // `MAX_PATTERN_DEPTH` check can refuse it. Past a few hundred that check
    // refuses anyway, so a big `n` only ever buys an out-of-memory kill on the
    // way to the same answer.
    if let Some(refused) = refuse_oversized_parts("applyN", n.max(0) as u64) {
        return refused;
    }
    let mut out = pat.clone();
    for _ in 0..n.max(0) {
        out = call(&out, f);
    }
    out
}

/// Applies `f` on the last of every `n` cycles.
pub fn last_of<P: PatOps>(pat: &P, n: i64, f: Option<&FunctionRef>) -> P {
    if n < 1 {
        return P::pat_silence();
    }
    if let Some(refused) = refuse_oversized_parts("lastOf", n as u64) {
        return refused;
    }
    let mut pats = Vec::new();
    if pats.try_reserve_exact(n as usize).is_err() {
        return host_memory_refusal();
    }
    pats.extend((0..n - 1).map(|_| pat.clone()));
    pats.push(call(pat, f));
    P::pat_slowcat_prime(pats)
}

/// Applies `f` on the first of every `n` cycles.
pub fn first_of<P: PatOps>(pat: &P, n: i64, f: Option<&FunctionRef>) -> P {
    if n < 1 {
        return P::pat_silence();
    }
    if let Some(refused) = refuse_oversized_parts("firstOf", n as u64) {
        return refused;
    }
    let mut pats = Vec::new();
    if pats.try_reserve_exact(n as usize).is_err() {
        return host_memory_refusal();
    }
    pats.push(call(pat, f));
    pats.extend((0..n - 1).map(|_| pat.clone()));
    P::pat_slowcat_prime(pats)
}

/// Applies `f` with the pattern slowed by `factor`, then speeds back up.
pub fn inside<P: PatOps>(pat: &P, factor: Fraction, f: Option<&FunctionRef>) -> P {
    call(&pat.slow(factor), f).fast(factor)
}

/// Applies `f` with the pattern sped up by `factor`, then slows back down.
pub fn outside<P: PatOps>(pat: &P, factor: Fraction, f: Option<&FunctionRef>) -> P {
    call(&pat.fast(factor), f).slow(factor)
}

/// Stacks the pattern with a delayed, transformed copy of itself.
pub fn off<P: PatOps>(pat: &P, time: Fraction, f: Option<&FunctionRef>) -> P {
    P::pat_stack(vec![pat.clone(), call(&pat.late(time), f)])
}

/// Stacks the pattern with `f(pat)` for every `f`.
pub fn superimpose<P: PatOps>(pat: &P, fs: &[FunctionRef]) -> P {
    let mut pats = vec![pat.clone()];
    pats.extend(fs.iter().map(|f| pat.apply_fn(f)));
    P::pat_stack(pats)
}

/// Stacks `f(pat)` for every `f`; the untransformed pattern is not included.
pub fn layer<P: PatOps>(pat: &P, fs: &[FunctionRef]) -> P {
    P::pat_stack(fs.iter().map(|f| pat.apply_fn(f)).collect())
}

/// Pans the dry pattern `by/2` left and a transformed copy `by/2` right;
/// steps are the LCM of the two sides. Non-object values are `with_pan`'s
/// business.
pub fn jux_by<P: PatOps>(pat: &P, by: f64, f: Option<&FunctionRef>) -> P {
    let by = by / 2.0;
    let left = pat.fmap(move |v| with_pan(v, -by));
    let right = call(&pat.fmap(move |v| with_pan(v, by)), f);
    let steps = match (left.pat_steps(), right.pat_steps()) {
        (Some(a), Some(b)) => match a.checked_lcm(b) {
            Some(steps) => Some(steps),
            None => return native_fraction_refusal("juxBy"),
        },
        (Some(a), None) => Some(a),
        (None, b) => b,
    };
    P::pat_stack(vec![left, right]).set_steps(steps)
}

/// [`jux_by`] at 1.
pub fn jux<P: PatOps>(pat: &P, f: Option<&FunctionRef>) -> P {
    jux_by(pat, 1.0, f)
}

pub fn jux_flip_by<P: PatOps>(pat: &P, by: f64, f: Option<&FunctionRef>) -> P {
    let pat = pat.clone();
    let f = f.cloned();
    P::pat_slowcat(vec![
        P::pat_pure(Value::F64(by)),
        P::pat_pure(Value::F64(-by)),
    ])
    .inner_bind(move |amount| jux_by(&pat, as_js_number(amount), f.as_ref()))
}

pub fn jux_flip<P: PatOps>(pat: &P, f: Option<&FunctionRef>) -> P {
    jux_flip_by(pat, 1.0, f)
}

fn with_pan(value: &Value, delta: f64) -> Value {
    let materialized = crate::materialize_js_value(value);
    let value = &materialized;
    let mut object = match value {
        Value::Object(map) => map.clone(),
        // A list spreads to indexed string keys, as an array does.
        Value::List(values) => crate::value::OrderedMap::from_entries(
            values
                .iter()
                .enumerate()
                .map(|(index, value)| (index.to_string(), value.clone())),
        ),
        _ => {
            // `'pan' in <primitive>` throws; the query error empties the arc.
            let shown = value.show();
            crate::signal_query_error(move || {
                format!("Cannot use 'in' operator to search for 'pan' in {shown}")
            });
            return value.clone();
        }
    };
    let current = object
        .get("pan")
        .and_then(|v| util::parse_numeral(v).ok())
        .unwrap_or(0.5);
    object.insert("pan".into(), Value::F64(current + delta));
    Value::Object(object)
}

/// Cap on `echoWith` copies. Construction happens before a query installs
/// its hap budget, so the bound is independent - `Infinity` must not saturate
/// the numeric cast into an `i64::MAX` host loop.
pub const MAX_ECHO_COPIES: u64 = 16_384;

/// How many rotated copies `iter`/`chunk` may materialise. Same reasoning
/// and value as [`MAX_ECHO_COPIES`]. strudel.cc refuses this ground too
/// (`RangeError: Invalid array length`), so refusing is compatible behaviour,
/// not a native-only restriction.
pub const MAX_ITER_PARTS: u64 = 16_384;

/// Guard for combinators that materialise one pattern per part from a
/// caller-supplied count. Returns the refusal when `parts` is above
/// [`MAX_ITER_PARTS`], and `None` otherwise.
fn refuse_oversized_parts<P: PatOps>(operation: &'static str, parts: u64) -> Option<P> {
    (parts > MAX_ITER_PARTS).then(|| {
        P::pat_query_limit(crate::QueryLimit::IterParts {
            operation,
            parts,
            limit: MAX_ITER_PARTS,
        })
    })
}

/// Delays `pat` by `time * index` for each copy index below `times`, or
/// returns the refusal: `EchoCopies` past [`MAX_ECHO_COPIES`], `HostMemory`,
/// or `NativeFraction { operation }` when a copy's shift leaves the native
/// fraction range.
fn echo_copies<P: PatOps>(
    pat: &P,
    times: i64,
    time: Fraction,
    operation: &'static str,
) -> Result<Vec<(P, i64)>, P> {
    let copies = times.max(0) as u64;
    if copies > MAX_ECHO_COPIES {
        return Err(P::pat_query_limit(crate::QueryLimit::EchoCopies {
            copies,
            limit: MAX_ECHO_COPIES,
        }));
    }

    let mut delayed = Vec::new();
    if delayed.try_reserve_exact(copies as usize).is_err() {
        return Err(host_memory_refusal());
    }
    for index in 0..times.max(0) {
        let Some(shift) = time.checked_mul(Fraction::int(i128::from(index))) else {
            return Err(native_fraction_refusal(operation));
        };
        delayed.push((pat.late(shift), index));
    }
    Ok(delayed)
}

/// Stacks `times` copies delayed by successive multiples of `time`, each
/// transformed by `f(copy, index)`. Refusals follow [`echo_copies`].
///
/// `apply_fn_indexed_batch` applies every callback before reifying any
/// result; one at a time is observably different when a callback mutates an
/// earlier result or swaps the configured string parser.
pub fn echo_with<P: PatOps>(
    pat: &P,
    times: i64,
    time: Fraction,
    f: Option<&FunctionRef>,
    operation: &'static str,
) -> P {
    let delayed = match echo_copies(pat, times, time, operation) {
        Ok(delayed) => delayed,
        Err(refusal) => return refusal,
    };
    let pats = match f {
        Some(function) => P::apply_fn_indexed_batch(delayed, function),
        None => delayed.into_iter().map(|(pattern, _)| pattern).collect(),
    };
    P::pat_stack(pats)
}

/// Stacks `times` copies delayed by successive multiples of `time`, copy
/// `index` at gain `feedback^index`; the body of `echo` and `stut`. Refusals
/// follow [`echo_copies`].
pub fn echo<P: PatOps>(
    pat: &P,
    times: i64,
    time: Fraction,
    feedback: f64,
    operation: &'static str,
) -> P {
    let delayed = match echo_copies(pat, times, time, operation) {
        Ok(delayed) => delayed,
        Err(refusal) => return refusal,
    };
    let pats = delayed
        .into_iter()
        .map(|(delayed, index)| {
            let gain = P::pat_pure(Value::object([(
                "gain".into(),
                // Upstream uses `Math.pow`; `powi` lowers to target-dependent
                // multiply chains whose rounding differs from it.
                Value::F64(feedback.powf(index as f64)),
            )]));
            compose(
                &delayed,
                &gain,
                ComposeOp::Set,
                crate::compose::default_alignment(),
            )
        })
        .collect();
    P::pat_stack(pats)
}

pub fn within<P: PatOps>(pat: &P, begin: Fraction, end: Fraction, f: Option<&FunctionRef>) -> P {
    let inside = pat.filter_haps(move |hap| {
        let Some(whole) = hap.whole else {
            crate::signal_query_error(|| "within requires discrete haps".into());
            return false;
        };
        let position = whole.begin.cycle_pos();
        position >= begin && position <= end
    });
    let outside = pat.filter_haps(move |hap| {
        let Some(whole) = hap.whole else {
            crate::signal_query_error(|| "within requires discrete haps".into());
            return false;
        };
        let position = whole.begin.cycle_pos();
        position < begin || position > end
    });
    P::pat_stack(vec![call(&inside, f), outside])
}

fn set_control<P: PatOps>(pat: &P, name: &str, value: Value) -> P {
    let control = P::pat_pure(Value::object([(name.to_owned(), value)]));
    compose(
        pat,
        &control,
        ComposeOp::Set,
        crate::compose::default_alignment(),
    )
}

pub fn control<P: PatOps>(pat: &P, value: &Value) -> P {
    let Value::List(values) = crate::materialize_js_value(value) else {
        return P::pat_query_error("control expects an array of [ccn, ccv]");
    };
    let ccn = values.first().cloned().unwrap_or(Value::Undefined);
    let ccv = values.get(1).cloned().unwrap_or(Value::Undefined);
    set_control(&set_control(pat, "ccn", ccn), "ccv", ccv)
}

pub fn sysex<P: PatOps>(pat: &P, value: &Value) -> P {
    let Value::List(values) = crate::materialize_js_value(value) else {
        return P::pat_query_error("sysex expects an array of [id, data]");
    };
    let id = values.first().cloned().unwrap_or(Value::Undefined);
    let data = values.get(1).cloned().unwrap_or(Value::Undefined);
    set_control(&set_control(pat, "sysexid", id), "sysexdata", data)
}

pub fn loop_at_cps<P: PatOps>(pat: &P, factor: Fraction, cps: f64) -> P {
    let speed = if factor == Fraction::ZERO {
        f64::INFINITY
    } else {
        cps / factor.to_f64()
    };
    set_control(
        &set_control(pat, "speed", Value::F64(speed)),
        "unit",
        Value::Str("c".into()),
    )
    .slow(factor)
}

/// Plays `pat` `n` times per cycle, the `k`th time with portion `k` of `n`
/// of each sample; steps scale by `n`.
pub fn striate<P: PatOps>(pat: &P, n: i64) -> P {
    if n < 1 {
        return P::pat_silence();
    }
    if let Some(refused) = refuse_oversized_parts("striate", n as u64) {
        return refused;
    }
    let n_fraction = Fraction::from(n);
    let steps = match scaled_steps(pat, n_fraction, "striate") {
        Ok(steps) => steps,
        Err(refused) => return refused,
    };
    let slices = (0..n)
        .map(|index| {
            P::pat_pure(Value::object([
                ("begin".into(), Value::F64(index as f64 / n as f64)),
                ("end".into(), Value::F64((index + 1) as f64 / n as f64)),
            ]))
        })
        .collect();
    let slices = P::pat_slowcat(slices);
    compose(pat, &slices, ComposeOp::Set, Alignment::In)
        .fast(n_fraction)
        .set_steps(steps)
}

/// Cuts each event into `n` parts that play successive portions of its
/// sample, or of its `begin`/`end` range; steps scale by `n`. An event whose
/// value is a primitive throws, which empties the query at its boundary.
pub fn chop<P: PatOps>(pat: &P, n: i64) -> P {
    if n < 1 {
        return P::pat_silence();
    }
    if let Some(refused) = refuse_oversized_parts("chop", n as u64) {
        return refused;
    }
    let steps = match scaled_steps(pat, Fraction::from(n), "chop") {
        Ok(steps) => steps,
        Err(refused) => return refused,
    };
    pat.squeeze_bind(move |value| {
        let base = match crate::materialize_js_value(value) {
            Value::Object(object) => object,
            // `'begin' in <primitive>` throws; the query error empties the arc.
            primitive @ (Value::Undefined
            | Value::Null
            | Value::Bool(_)
            | Value::F64(_)
            | Value::Str(_)) => {
                let shown = primitive.show();
                crate::signal_query_error(move || {
                    format!("Cannot use 'in' operator to search for 'begin' in {shown}")
                });
                return P::pat_silence();
            }
            _ => crate::OrderedMap::new(),
        };
        let existing = match (base.get("begin"), base.get("end")) {
            (Some(begin), Some(end)) if !begin.is_nullish() && !end.is_nullish() => {
                match (begin.as_f64(), end.as_f64()) {
                    (Some(begin), Some(end)) => Some((begin, end)),
                    _ => None,
                }
            }
            _ => None,
        };
        let mut slices = Vec::with_capacity(n as usize);
        for index in 0..n {
            let mut controls = base.clone();
            let mut begin = index as f64 / n as f64;
            let mut end = (index + 1) as f64 / n as f64;
            if let Some((source_begin, source_end)) = existing {
                let duration = source_end - source_begin;
                begin = source_begin + begin * duration;
                end = source_begin + end * duration;
            }
            controls.insert("begin".into(), Value::F64(begin));
            controls.insert("end".into(), Value::F64(end));
            slices.push(P::pat_pure(Value::Object(controls)));
        }
        P::pat_fastcat(slices)
    })
    .map_haps_native(|hap| {
        let mut hap = hap.clone();
        hap.context.clear();
        Some(hap)
    })
    .set_steps(steps)
}

fn rearrange_with<P: PatOps>(pat: &P, indices: P, n: i64) -> P {
    if n < 1 {
        return P::pat_silence();
    }
    if let Some(refused) = refuse_oversized_parts("rearrange", n as u64) {
        return refused;
    }
    let n_fraction = Fraction::from(n);
    let slices: Vec<P> = (0..n)
        .map(|index| {
            pat.zoom(
                Fraction::from(index).div(n_fraction),
                Fraction::from(index + 1).div(n_fraction),
            )
        })
        .collect();
    indices.inner_bind(move |value| {
        let index = crate::util::parse_numeral(value).unwrap_or(f64::NAN);
        if !index.is_finite() || index < 0.0 || index.fract() != 0.0 {
            return P::pat_query_error("rearrange index is out of range");
        }
        slices
            .get(index as usize)
            .cloned()
            .unwrap_or_else(|| P::pat_query_error("rearrange index is out of range"))
            .repeat_cycles(n_fraction)
            .fast(n_fraction)
    })
}

pub fn shuffle<P: PatOps>(pat: &P, n: i64) -> P {
    if n < 1 {
        return P::pat_silence();
    }
    rearrange_with(pat, P::pat_randrun(n as usize), n)
}

pub fn scramble<P: PatOps>(pat: &P, n: i64) -> P {
    if n < 1 {
        return P::pat_silence();
    }
    rearrange_with(pat, P::pat_irand_segment(n as usize), n)
}

pub fn euclidish<P: PatOps>(pat: &P, pulses: i32, steps: i32, by: Fraction) -> P {
    if pulses < 0 || steps < 1 || pulses > steps {
        return P::pat_query_error("euclidish expects 0 <= pulses <= steps");
    }
    if steps > crate::euclid::MAX_EUCLID_STEPS as i32 {
        return P::pat_query_limit(crate::QueryLimit::EuclidSteps {
            steps: steps as u64,
            limit: crate::euclid::MAX_EUCLID_STEPS as u64,
        });
    }
    let from = match crate::euclid::bjorklund(pulses, steps as usize) {
        Ok(mask) => mask,
        Err(_) => return P::pat_query_error("euclidish mask is invalid"),
    };
    if pulses == 0 {
        return P::pat_silence();
    }
    let from_positions: Vec<usize> = from
        .iter()
        .enumerate()
        .filter_map(|(index, value)| (*value != 0).then_some(index))
        .collect();
    let steps_fraction = Fraction::from(steps);
    let Some(duration) = Fraction::ONE.checked_div(steps_fraction) else {
        return native_fraction_refusal("euclidish");
    };
    let pulses_fraction = Fraction::from(pulses);
    let mut arcs = Vec::with_capacity(from_positions.len());
    for (index, from_index) in from_positions.into_iter().enumerate() {
        let Some((begin, end)) = Fraction::from(from_index as i32)
            .checked_div(steps_fraction)
            .zip(Fraction::from(index as i32).checked_div(pulses_fraction))
            .and_then(|(from, to)| {
                let begin = by.checked_mul(to.checked_sub(from)?)?.checked_add(from)?;
                Some((begin, begin.checked_add(duration)?))
            })
        else {
            return native_fraction_refusal("euclidish");
        };
        arcs.push(P::pat_pure(Value::Bool(true)).compress(begin, end));
    }
    compose(pat, &P::pat_stack(arcs), ComposeOp::KeepIf, Alignment::Out)
}

pub fn as_controls<P: PatOps>(pat: &P, mapping: &Value) -> P {
    let mapping = match crate::materialize_js_value(mapping) {
        Value::List(values) => values,
        value => vec![value],
    };
    pat.fmap(move |value| {
        let values = match crate::materialize_js_value(value) {
            Value::List(values) => values,
            value => vec![value],
        };
        let entries = mapping.iter().zip(values).filter_map(|(name, value)| {
            if matches!(value, Value::Undefined) {
                return None;
            }
            let name = match name {
                Value::Str(name) => name.clone(),
                value => value.show(),
            };
            let canonical = crate::controls::canonical_control_name(&name)
                .unwrap_or(&name)
                .to_owned();
            Some((canonical, value))
        });
        Value::object(entries)
    })
}

pub fn key_down<P: PatOps>(pat: &P) -> P {
    pat.fmap(|_| Value::Bool(false))
}

pub fn when_key<P: PatOps>(pat: &P) -> P {
    pat.clone()
}

pub fn filter_callback<P: PatOps>(pat: &P, function: Option<&FunctionRef>) -> P {
    pat.filter_haps_function(function)
}

pub fn filter_when_callback<P: PatOps>(pat: &P, function: Option<&FunctionRef>) -> P {
    pat.filter_when_function(function)
}

/// Applies `f` to a different `1/n` chunk each cycle.
///
/// The boolean selection is an inner join, which is why the transformed and
/// plain branches interleave per chunk rather than per cycle.
pub fn chunk<P: PatOps>(pat: &P, n: i64, f: Option<&FunctionRef>, back: bool, fast: bool) -> P {
    if n < 1 {
        return P::pat_silence();
    }
    // Refuse before `binary` is sized from `n`. `iter` guards itself too, but
    // this vector is built first, so the abort happened here before `iter` was
    // ever reached.
    let operation = if fast {
        "fastchunk"
    } else if back {
        "chunkBack"
    } else {
        "chunk"
    };
    if let Some(refused) = refuse_oversized_parts(operation, n as u64) {
        return refused;
    }
    let mut binary = Vec::new();
    if binary.try_reserve_exact(n as usize).is_err() {
        return host_memory_refusal();
    }
    binary.push(P::pat_pure(Value::Bool(true)));
    binary.extend((0..n - 1).map(|_| P::pat_pure(Value::Bool(false))));
    let times = Fraction::int(i128::from(n));
    // `iter(…, !back)`: shift the pattern forwards, so time backwards.
    // Dropping the inversion reverses the direction the chunk travels.
    let binary_pat = iter(&P::pat_fastcat(binary), times, !back);
    let operand = if fast {
        pat.clone()
    } else {
        pat.repeat_cycles(times)
    };
    let transformed = call(&operand, f);
    binary_pat.inner_bind(move |v| {
        if v.js_truthy() {
            transformed.clone()
        } else {
            operand.clone()
        }
    })
}

// -- the stepwise (_steps) family --------------------------------------------

/// Silence with zero steps. `silence` has one step, `nothing` zero, and
/// `stepcat` distinguishes them: a zero-width entry is skipped but still
/// counts toward the total.
pub fn nothing<P: PatOps>() -> P {
    P::pat_silence().set_steps(Some(Fraction::ZERO))
}

/// Concatenates patterns weighted by step count. `None` weights take the
/// average of the known ones; all-unknown falls back to `fastcat`.
///
/// A zero-width entry is SKIPPED but still counts toward `total`, so
/// `stepcat(nothing, x)` is not the same as `stepcat(x)`.
pub fn stepcat<P: PatOps>(items: &[(Option<Fraction>, P)]) -> P {
    if items.is_empty() {
        return nothing();
    }
    let mut weights: Vec<Option<Fraction>> = items.iter().map(|(w, _)| *w).collect();
    if weights.iter().any(Option::is_none) {
        let known: Vec<Fraction> = weights.iter().filter_map(|w| *w).collect();
        if known.is_empty() {
            return P::pat_fastcat(items.iter().map(|(_, p)| p.clone()).collect());
        }
        if known.len() == weights.len() {
            return nothing();
        }
        let Some(avg) = stepcat_missing_weight(&known) else {
            return native_fraction_refusal("stepcat");
        };
        for weight in &mut weights {
            if weight.is_none() {
                *weight = Some(avg);
            }
        }
    }
    if items.len() == 1 {
        return items[0].1.set_steps(weights[0]);
    }
    // Representable weights can still overflow their sum or the slot bounds
    // derived from it, so check both the total and each normalized slot.
    let total = weights.iter().try_fold(Fraction::ZERO, |total, weight| {
        total.checked_add(weight.unwrap_or(Fraction::ZERO))
    });
    let Some(total) = total else {
        return native_fraction_refusal("stepcat");
    };
    if total == Fraction::ZERO {
        return nothing();
    }
    let mut begin = Fraction::ZERO;
    let mut pats = Vec::with_capacity(items.len());
    for (weight, (_, pat)) in weights.iter().zip(items.iter()) {
        let time = weight.unwrap_or(Fraction::ZERO);
        if time == Fraction::ZERO {
            continue;
        }
        let bounds = begin.checked_add(time).and_then(|end| {
            let cat_begin = begin.checked_div(total)?;
            let cat_end = end.checked_div(total)?;
            // For a non-empty slot of the unit cycle, `compress` derives its
            // gap factor from `cat_end - cat_begin`, which can overflow even
            // when both bounds fit (1/p and 1/q for large coprime p, q).
            // Checked on exactly that path: out-of-range or inverted bounds
            // stay `compress`'s arithmetic-free silence.
            let slot =
                Fraction::ZERO <= cat_begin && cat_begin < cat_end && cat_end <= Fraction::ONE;
            if slot {
                cat_end.checked_sub(cat_begin)?;
            }
            Some((end, cat_begin, cat_end))
        });
        let Some((end, cat_begin, cat_end)) = bounds else {
            return native_fraction_refusal("stepcat");
        };
        pats.push(pat.compress(cat_begin, cat_end));
        begin = end;
    }
    P::pat_stack(pats).set_steps(Some(total))
}

/// The weight [`stepcat`] gives an entry without one: the mean of the `known`
/// weights. `None` when `known` is empty or when the sum of `known` or its
/// mean does not fit the i128 fraction range.
pub fn stepcat_missing_weight(known: &[Fraction]) -> Option<Fraction> {
    known
        .iter()
        .try_fold(Fraction::ZERO, |sum, weight| sum.checked_add(*weight))
        .and_then(|sum| sum.checked_div(Fraction::int(known.len() as i128)))
}

#[derive(Clone, Copy)]
struct StepwiseSegment {
    zoom_begin: Fraction,
    zoom_end: Fraction,
    weight: Fraction,
    cat_begin: Option<Fraction>,
    cat_factor: Option<Fraction>,
}

struct StepwisePlan {
    segments: Vec<StepwiseSegment>,
    total: Fraction,
}

enum StepwisePlanError {
    Arithmetic,
    DivisionByZero,
    HostMemory,
    Limit(crate::QueryLimit),
}

fn stepwise_error<P: PatOps>(error: StepwisePlanError) -> P {
    match error {
        StepwisePlanError::Arithmetic => native_fraction_refusal("shrink/grow"),
        StepwisePlanError::DivisionByZero => P::pat_query_error("Division by Zero"),
        StepwisePlanError::HostMemory => {
            P::pat_query_limit(crate::mark_stepwise_refusal(crate::QueryLimit::HostMemory))
        }
        StepwisePlanError::Limit(limit) => P::pat_query_limit(limit),
    }
}

fn checked_order(left: Fraction, right: Fraction) -> Result<Ordering, StepwisePlanError> {
    left.checked_cmp(right).ok_or(StepwisePlanError::Arithmetic)
}

/// Plan the exact shrink list used by `shrink`/`grow`, zero-amount bug
/// included, without allocating a single Pattern node.
fn stepwise_plan(
    steps: Fraction,
    amount: Fraction,
    grow: bool,
) -> Result<StepwisePlan, StepwisePlanError> {
    if steps.numer() == 0 {
        return Err(StepwisePlanError::DivisionByZero);
    }
    if steps.numer() < 0 {
        return Ok(StepwisePlan {
            segments: Vec::new(),
            total: Fraction::ZERO,
        });
    }

    // grow is shrink with the amount negated and the list reversed. Select
    // the conceptual sign directly so i128::MIN never has to be negated.
    let from_start = if grow {
        amount.numer() < 0
    } else {
        amount.numer() > 0
    };

    let magnitude_vs_steps = if amount.numer() == 0 {
        None
    } else if amount.numer() > 0 {
        Some(checked_order(amount, steps)?)
    } else {
        let negative_steps = steps.checked_neg().ok_or(StepwisePlanError::Arithmetic)?;
        Some(checked_order(amount, negative_steps)?.reverse())
    };

    let segment = match magnitude_vs_steps {
        Some(Ordering::Greater) => None,
        Some(Ordering::Equal) => Some(Fraction::ONE),
        Some(Ordering::Less) => {
            let magnitude = if amount.numer() < 0 {
                amount.checked_neg().ok_or(StepwisePlanError::Arithmetic)?
            } else {
                amount
            };
            Some(
                magnitude
                    .checked_div(steps)
                    .ok_or(StepwisePlanError::Arithmetic)?,
            )
        }
        None => Some(Fraction::ZERO),
    };

    // Overshoots contain only the original full pattern. Exact equality also
    // includes the terminal zero-width `nothing` entry.
    let count = match magnitude_vs_steps {
        Some(Ordering::Greater) => 1,
        _ => {
            let segment = segment.expect("less/zero amounts have a segment");
            let mut count = 0_u64;
            for index in 0..=crate::MAX_STEPWISE_SEGMENTS {
                let index_fraction = Fraction::int(i128::from(index));
                if checked_order(index_fraction, steps)? != Ordering::Less {
                    break;
                }
                let offset = segment
                    .checked_mul(index_fraction)
                    .ok_or(StepwisePlanError::Arithmetic)?;
                if checked_order(offset, Fraction::ONE)? == Ordering::Greater {
                    break;
                }
                count += 1;
            }
            count
        }
    };

    // This charge is local per invocation outside a query and cumulative when
    // the body is being resolved inside PatternOf*/StepJoin.
    crate::charge_stepwise_segments(count).map_err(StepwisePlanError::Limit)?;

    let mut segments = Vec::new();
    segments
        .try_reserve_exact(count as usize)
        .map_err(|_| StepwisePlanError::HostMemory)?;

    let append = |segments: &mut Vec<StepwiseSegment>, offset: Fraction| {
        let duration = Fraction::ONE
            .checked_sub(offset)
            .ok_or(StepwisePlanError::Arithmetic)?;
        let weight = steps
            .checked_mul(duration)
            .ok_or(StepwisePlanError::Arithmetic)?;
        let (zoom_begin, zoom_end) = if from_start {
            (offset, Fraction::ONE)
        } else {
            (Fraction::ZERO, duration)
        };
        segments.push(StepwiseSegment {
            zoom_begin,
            zoom_end,
            weight,
            cat_begin: None,
            cat_factor: None,
        });
        Ok::<_, StepwisePlanError>(())
    };

    match magnitude_vs_steps {
        Some(Ordering::Greater) => append(&mut segments, Fraction::ZERO)?,
        _ => {
            let segment = segment.expect("less/zero amounts have a segment");
            for index in 0..count {
                let offset = segment
                    .checked_mul(Fraction::int(i128::from(index)))
                    .ok_or(StepwisePlanError::Arithmetic)?;
                append(&mut segments, offset)?;
            }
        }
    }

    if grow {
        segments.reverse();
    }

    let total = segments.iter().try_fold(Fraction::ZERO, |total, segment| {
        total
            .checked_add(segment.weight)
            .ok_or(StepwisePlanError::Arithmetic)
    })?;

    // Precompute stepcat's compression algebra too. Pattern construction below
    // therefore cannot discover an arithmetic error after allocating a partial
    // graph, and it avoids stepcat's metadata LCM over extreme inputs.
    let mut begin = Fraction::ZERO;
    for segment in &mut segments {
        if segment.weight == Fraction::ZERO {
            continue;
        }
        let end = begin
            .checked_add(segment.weight)
            .ok_or(StepwisePlanError::Arithmetic)?;
        let cat_begin = begin
            .checked_div(total)
            .ok_or(StepwisePlanError::Arithmetic)?;
        let cat_end = end
            .checked_div(total)
            .ok_or(StepwisePlanError::Arithmetic)?;
        let width = cat_end
            .checked_sub(cat_begin)
            .ok_or(StepwisePlanError::Arithmetic)?;
        let factor = Fraction::ONE
            .checked_div(width)
            .ok_or(StepwisePlanError::Arithmetic)?;
        segment.cat_begin = Some(cat_begin);
        segment.cat_factor = Some(factor);
        begin = end;
    }

    debug_assert_eq!(segments.len() as u64, count);
    debug_assert!(count <= crate::MAX_STEPWISE_SEGMENTS);
    Ok(StepwisePlan { segments, total })
}

fn build_stepwise<P: PatOps>(pat: &P, amount: Fraction, grow: bool) -> P {
    let Some(steps) = pat.pat_steps() else {
        return nothing();
    };
    let plan = match stepwise_plan(steps, amount, grow) {
        Ok(plan) => plan,
        Err(error) => return stepwise_error(error),
    };
    if plan.segments.is_empty() {
        return nothing();
    }

    let mut entries = Vec::new();
    let mut compressed = Vec::new();
    if entries.try_reserve_exact(plan.segments.len()).is_err() {
        return stepwise_error(StepwisePlanError::HostMemory);
    }
    if compressed.try_reserve_exact(plan.segments.len()).is_err() {
        return stepwise_error(StepwisePlanError::HostMemory);
    }
    crate::note_stepwise_segments_materialised(plan.segments.len() as u64);
    let unstepped = pat.set_steps(None);
    for segment in &plan.segments {
        let entry = if segment.weight == Fraction::ZERO {
            // Generic zoom returns `silence` for an empty range; the shrink
            // list needs `nothing` with zero steps here.
            nothing()
        } else {
            unstepped
                .zoom(segment.zoom_begin, segment.zoom_end)
                .set_steps(Some(segment.weight))
        };
        entries.push(entry);
    }

    if entries.len() == 1 {
        return entries
            .pop()
            .expect("one planned segment")
            .set_steps(Some(plan.total));
    }

    for (entry, segment) in entries.into_iter().zip(&plan.segments) {
        if segment.weight == Fraction::ZERO {
            continue;
        }
        // Validated `compress(begin,end)` expressed directly, so no unchecked
        // Fraction arithmetic is repeated after graph allocation.
        compressed.push(
            entry
                .set_steps(None)
                .fast_gap(segment.cat_factor.expect("nonzero segment factor"))
                .late(segment.cat_begin.expect("nonzero segment begin")),
        );
    }
    P::pat_stack(compressed).set_steps(Some(plan.total))
}

/// Progressively removes `amount` steps from one side, cycle by cycle.
pub fn shrink<P: PatOps>(pat: &P, amount: Fraction) -> P {
    build_stepwise(pat, amount, false)
}

/// Progressively adds steps: [`shrink`] with the amount negated, list reversed.
pub fn grow<P: PatOps>(pat: &P, amount: Fraction) -> P {
    build_stepwise(pat, amount, true)
}

/// Takes the first `amount` steps; negative takes from the end.
pub fn take<P: PatOps>(pat: &P, amount: Fraction) -> P {
    let Some(steps) = pat.pat_steps() else {
        return nothing();
    };
    take_steps(pat, steps, amount, "take")
}

/// [`take`] against a known step count; `operation` names the refusal, so a
/// [`drop`] that overflows here reports as `drop`.
fn take_steps<P: PatOps>(pat: &P, steps: Fraction, amount: Fraction, operation: &'static str) -> P {
    if steps <= Fraction::ZERO || amount == Fraction::ZERO {
        return nothing();
    }

    // Check the step ratio here and the resulting time maps in `checked_zoom`;
    // either can exceed the fraction range even when both inputs fit.
    if amount > Fraction::ZERO {
        if amount >= steps {
            return pat.clone();
        }
        let Some(end) = amount.checked_div(steps) else {
            return native_fraction_refusal(operation);
        };
        return checked_zoom(pat, Fraction::ZERO, end, operation);
    }

    // Compare against the negated step count before negating `amount`: an
    // i128::MIN overshoot then returns without negation. `steps` is positive
    // here, so its own negation is exact.
    if amount <= steps.neg() {
        return pat.clone();
    }
    let begin = amount
        .checked_neg()
        .and_then(|magnitude| magnitude.checked_div(steps))
        .and_then(|fraction| Fraction::ONE.checked_sub(fraction));
    let Some(begin) = begin else {
        return native_fraction_refusal(operation);
    };
    checked_zoom(pat, begin, Fraction::ONE, operation)
}

/// Drops the first `amount` steps (negative drops from the end), via [`take`].
pub fn drop<P: PatOps>(pat: &P, amount: Fraction) -> P {
    let Some(steps) = pat.pat_steps() else {
        return nothing();
    };
    // Guard non-positive steps before the arithmetic; it also avoids an
    // unnecessary checked-i128 overflow for a degenerate receiver.
    if steps <= Fraction::ZERO {
        return nothing();
    }

    // The retained amount can overflow even when the two inputs fit.
    let retained = if amount < Fraction::ZERO {
        steps.checked_add(amount)
    } else {
        amount.checked_sub(steps)
    };
    let Some(retained) = retained else {
        return native_fraction_refusal("drop");
    };
    take_steps(pat, steps, retained, "drop")
}

/// Scale the step count with checked arithmetic, naming the caller's
/// operation if the product exceeds the native fraction range.
fn expand_checked<P: PatOps>(pat: &P, factor: Fraction, operation: &'static str) -> P {
    match scaled_steps(pat, factor, operation) {
        Ok(steps) => pat.set_steps(steps),
        Err(refused) => refused,
    }
}

/// Multiplies the step count; the events don't change.
pub fn expand<P: PatOps>(pat: &P, factor: Fraction) -> P {
    expand_checked(pat, factor, "expand")
}

/// Repeats the cycle `factor` times per cycle, scaling steps to match.
pub fn extend<P: PatOps>(pat: &P, factor: Fraction) -> P {
    expand_checked(&checked_fast(pat, factor, "extend"), factor, "extend")
}

/// Like [`extend`], but repeating whole cycles (`repeat_cycles`) instead of one.
pub fn replicate<P: PatOps>(pat: &P, factor: Fraction) -> P {
    let repeated = checked_fast(&pat.repeat_cycles(factor), factor, "replicate");
    expand_checked(&repeated, factor, "replicate")
}

/// Divides the step count; the events don't change.
pub fn contract<P: PatOps>(pat: &P, factor: Fraction) -> P {
    let Some(steps) = pat.pat_steps() else {
        return pat.clone();
    };
    // A scalar `contract(0)` throws at construction on strudel.cc; a
    // phase-correct exception can't be surfaced here, so return silence
    // rather than panic in Fraction::div.
    if factor == Fraction::ZERO {
        return nothing();
    }
    // A nonzero, representable divisor does not guarantee the quotient fits.
    let Some(contracted) = steps.checked_div(factor) else {
        return native_fraction_refusal("contract");
    };
    pat.set_steps(Some(contracted))
}

/// Speeds the pattern so one cycle carries `target` steps. No step count:
/// identity. Zero steps: `nothing`.
pub fn pace<P: PatOps>(pat: &P, target: Fraction) -> P {
    let Some(steps) = pat.pat_steps() else {
        return pat.clone();
    };
    if steps == Fraction::ZERO {
        return nothing();
    }
    // Check both the rate and its application to query times; a rate that
    // fits can still overflow when the pattern is queried at a later cycle.
    let Some(rate) = target.checked_div(steps) else {
        return native_fraction_refusal("pace");
    };
    checked_fast(pat, rate, "pace").set_steps(Some(target))
}

// -- euclidean rhythms -------------------------------------------------------
//
// The Bjorklund/rotation maths lives in `crate::euclid` (the mini parser's
// `(3,8)` syntax uses it); these are the combinator wrappers.

/// Concatenates patterns with explicit weights; no step metadata, unlike
/// [`stepcat`]. Zero total weight is silence.
pub fn time_cat<P: PatOps>(items: &[(Fraction, P)]) -> P {
    let total = items
        .iter()
        .fold(Fraction::ZERO, |acc, (weight, _)| acc.add(*weight));
    if total == Fraction::ZERO {
        return P::pat_silence();
    }
    let mut begin = Fraction::ZERO;
    let mut pats = Vec::with_capacity(items.len());
    for (weight, pat) in items {
        let end = begin.add(*weight);
        pats.push(pat.compress(begin.div(total), end.div(total)));
        begin = end;
    }
    P::pat_stack(pats)
}

/// The binary euclidean mask; `None` when `steps` is non-positive.
fn euclid_mask(pulses: i32, steps: i32, rotation: i32) -> Option<Vec<bool>> {
    if steps <= 0 {
        return None;
    }
    crate::euclid::euclid_rot(pulses, steps as usize, rotation as isize)
        .ok()
        .map(|mask| mask.into_iter().map(|bit| bit != 0).collect())
}

fn mask_pattern<P: PatOps>(mask: &[bool]) -> P {
    P::pat_fastcat(mask.iter().map(|b| P::pat_pure(Value::Bool(*b))).collect())
}

/// Structures `pat` by the euclidean `(pulses, steps)` mask, rotated.
pub fn euclid_rot<P: PatOps>(pat: &P, pulses: i32, steps: i32, rotation: i32) -> P {
    if steps > crate::euclid::MAX_EUCLID_STEPS as i32 {
        return P::pat_query_limit(crate::QueryLimit::EuclidSteps {
            steps: steps as u64,
            limit: crate::euclid::MAX_EUCLID_STEPS as u64,
        });
    }
    match euclid_mask(pulses, steps, rotation) {
        Some(mask) => struct_with(pat, &mask_pattern::<P>(&mask)),
        None => P::pat_silence(),
    }
}

/// [`euclid_rot`] from a `[pulses, steps?, rotation?]` value; `steps`
/// defaults to `pulses`.
pub fn bjork<P: PatOps>(pat: &P, value: &Value) -> P {
    let items = slots(value, 3);
    let pulses = util::parse_numeral(&items[0]).unwrap_or(f64::NAN) as i32;
    let steps = util::parse_numeral(&items[1])
        .map(|n| n as i32)
        .unwrap_or(pulses);
    let rotation = util::parse_numeral(&items[2])
        .map(|n| n as i32)
        .unwrap_or(0);
    euclid_rot(pat, pulses, steps, rotation)
}

/// Euclidean rhythm with each pulse held until the next: the run of `0`s
/// after a `1` becomes that pulse's width, and anything before the first
/// pulse is dropped.
pub fn euclid_legato<P: PatOps>(pat: &P, pulses: i32, steps: i32, rotation: i32) -> P {
    if pulses < 1 {
        return P::pat_silence();
    }
    if steps > crate::euclid::MAX_EUCLID_STEPS as i32 {
        return P::pat_query_limit(crate::QueryLimit::EuclidSteps {
            steps: steps as u64,
            limit: crate::euclid::MAX_EUCLID_STEPS as u64,
        });
    }
    let Some(mask) = euclid_mask(pulses, steps, 0) else {
        return P::pat_silence();
    };
    let binary: String = mask.iter().map(|b| if *b { '1' } else { '0' }).collect();
    let gapless: Vec<(Fraction, P)> = binary
        .split('1')
        .skip(1)
        .map(|run| {
            (
                Fraction::int(run.len() as i128 + 1),
                P::pat_pure(Value::Bool(true)),
            )
        })
        .collect();
    if gapless.is_empty() {
        return P::pat_silence();
    }
    struct_with(pat, &time_cat(&gapless))
        .late(Fraction::int(i128::from(rotation)).div(Fraction::int(i128::from(steps))))
}

// -- the degrade / sometimes family ------------------------------------------

/// A pure signal, lifted into whichever purity view the body is running under.
///
/// Signals contain no callbacks, so the classification always succeeds; the
/// `expect` is the self-check rather than an assumption.
fn pure_signal<P: PatOps>(pattern: crate::Pattern) -> P {
    P::from_pure_view(
        pattern
            .as_pure_pattern()
            .expect("signals are pure by construction"),
    )
}

/// Keeps only events whose `with_pat` sample exceeds `x` - strictly `>`.
/// Structure comes from `pat` (app-left); the chooser is sampled over each
/// hap's whole.
pub fn degrade_by_with<P: PatOps>(pat: &P, with_pat: &P, x: f64) -> P {
    let gate = with_pat.filter_values(move |v| util::parse_numeral(v).is_ok_and(|n| n > x));
    pat.app_left_with_lookup(gate, |a, _| a.clone(), crate::LookupFlow::Left)
        .set_steps(pat.pat_steps())
}

/// [`degrade_by_with`] over `rand`.
pub fn degrade_by<P: PatOps>(pat: &P, x: f64) -> P {
    degrade_by_with(pat, &pure_signal::<P>(crate::signal::rand()), x)
}

/// [`degrade_by_with`] over `1 - rand`: the exact complement of [`degrade_by`].
pub fn undegrade_by<P: PatOps>(pat: &P, x: f64) -> P {
    let inverted = pure_signal::<P>(crate::signal::rand())
        .fmap(|v| Value::F64(1.0 - util::parse_numeral(v).unwrap_or(f64::NAN)));
    degrade_by_with(pat, &inverted, x)
}

/// Applies `f` to roughly `x` of the events. The halves are complementary:
/// the transformed copy takes exactly the events the plain copy dropped.
/// The scalar carrier and `inner_bind` are required: they split crossing
/// wholes at cycle boundaries and invoke `f` lazily, per queried carrier hap.
pub fn sometimes_by<P: PatOps>(pat: &P, x: f64, f: Option<&FunctionRef>) -> P {
    let pat = pat.clone();
    let f = f.cloned();
    P::pat_pure(Value::F64(x)).inner_bind(move |_| {
        let plain = degrade_by(&pat, x);
        let transformed = call(&undegrade_by(&pat, 1.0 - x), f.as_ref());
        P::pat_stack(vec![plain, transformed])
    })
}

/// [`sometimes_by`], but `segment(1)` on the rand signal makes the coin
/// flip once per CYCLE rather than per hap.
pub fn some_cycles_by<P: PatOps>(pat: &P, x: f64, f: Option<&FunctionRef>) -> P {
    let pat = pat.clone();
    let f = f.cloned();
    P::pat_pure(Value::F64(x)).inner_bind(move |_| {
        let per_cycle = segment(&pure_signal::<P>(crate::signal::rand()), Fraction::ONE);
        let inverted = segment(
            &pure_signal::<P>(crate::signal::rand())
                .fmap(|v| Value::F64(1.0 - util::parse_numeral(v).unwrap_or(f64::NAN))),
            Fraction::ONE,
        );
        P::pat_stack(vec![
            degrade_by_with(&pat, &per_cycle, x),
            call(&degrade_by_with(&pat, &inverted, 1.0 - x), f.as_ref()),
        ])
    })
}

// -- envelope shorthands ----------------------------------------------------
//
// Combinators, not controls. `ds` collides with the `delaysync` control
// alias; the shorthand is declared later and name resolution must keep it
// winning.

/// `pat.set({ … })` with a literal object, as the envelope shorthands use.
fn set_object<P: PatOps>(pat: &P, entries: Vec<(&str, Value)>) -> P {
    let object = Value::object(entries.into_iter().map(|(k, v)| (k.to_string(), v)));
    compose(pat, &P::pat_pure(object), ComposeOp::Set, Alignment::In)
}

/// `.serial(baud, sendcrc, singlecharids, port)` - write every hap to a serial
/// device.
///
/// In the browser the last argument labels a writer (Web Serial's permission
/// dialog picks the device); with no dialog it names the device instead, and
/// "default" means the first port the system reports.
pub fn serial<P: PatOps>(pat: &P, baud: f64, send_crc: bool, short_ids: bool, port: &str) -> P {
    set_object(
        pat,
        vec![
            ("serialport", Value::Str(port.to_string())),
            ("serialbaud", Value::F64(baud)),
            ("serialcrc", Value::F64(if send_crc { 1.0 } else { 0.0 })),
            ("serialshort", Value::F64(if short_ids { 1.0 } else { 0.0 })),
        ],
    )
}
/// `.midi(port)` - mark every hap for a MIDI port.
///
/// Give it an index (`.midi(0)`) or a literal name
/// (`.midi('IAC Driver Bus 1')`). The open path also matches names by
/// case-insensitive substring, so `.midi('IAC')` is enough when unambiguous.
///
/// A name that does not arrive as a literal becomes
/// [`crate::UNREADABLE_MIDI_PORT`], which matches no device, so the MIDI path
/// reports it. A fallback to port 0 would play into the first device without
/// a warning.
///
/// This is not the `midiport` control. A control patternifies its string
/// argument, so `.midiport('IAC Driver Bus 1')` parses as mini-notation and
/// becomes four events whose first is "IAC". A port name is a literal. The
/// `midiport` control still patterns, to move a phrase between devices.
pub fn midi<P: PatOps>(pat: &P, port: &str, options: &Value) -> P {
    let mut pairs = vec![("midiport".to_string(), Value::Str(port.to_string()))];
    // Connection options are fallbacks: carried under one nested key with
    // the same Keep semantics as the port, so per-hap controls always win.
    if let Value::Object(map) = options {
        let carried: Vec<(String, Value)> = [
            "isController",
            "noteOffsetMs",
            "midichannel",
            "velocity",
            "gain",
            "midimap",
            // Accepted for score compatibility; no latency semantics attached.
            "latencyMs",
        ]
        .into_iter()
        .filter_map(|key| {
            map.get(key)
                .filter(|value| !value.is_nullish())
                .map(|value| (key.to_string(), value.clone()))
        })
        .collect();
        if !carried.is_empty() {
            pairs.push(("midiopts".to_string(), Value::object(carried)));
        }
    }
    // Keep, not Set: the `.midi()` argument is only a fallback, and a per-hap
    // `midiport` wins. With Set,
    // `note("c a f e").midiport("<0 1 2 3>").midi()` routes every hap to one
    // device.
    let object = Value::object(pairs);
    compose(pat, &P::pat_pure(object), ComposeOp::Keep, Alignment::In)
}

/// `.osc()` - send every hap to an OSC listener, SuperDirt by default.
///
/// Marks the hap by setting `oscport`, the same way `.midi()` sets `midiport`,
/// so the existing `oscport`/`oschost` controls still override per hap. An
/// explicit `.osc(57121)` targets another port; bare `.osc()` uses SuperDirt's
/// 57120.
pub fn osc<P: PatOps>(pat: &P, port: f64) -> P {
    // Keep, not Set, as in `.midi()`: a per-hap `oscport` wins. With Set,
    // `.oscport(57121).osc()` sends to 57120.
    let object = Value::object([("oscport".to_string(), Value::F64(port))]);
    compose(pat, &P::pat_pure(object), ComposeOp::Keep, Alignment::In)
}

/// The `[a, b, …]` destructuring these four share: a non-array argument is
/// wrapped, and missing positions are `undefined`.
fn slots(value: &Value, n: usize) -> Vec<Value> {
    let items: Vec<Value> = match value {
        Value::List(items) => items.clone(),
        other => vec![other.clone()],
    };
    (0..n)
        .map(|i| items.get(i).cloned().unwrap_or(Value::Undefined))
        .collect()
}

/// Sets attack/decay/sustain/release from a value or list. Absent positions
/// are set as `undefined` anyway, overwriting any existing value.
pub fn adsr<P: PatOps>(pat: &P, value: &Value) -> P {
    let v = slots(value, 4);
    set_object(
        pat,
        vec![
            ("attack", v[0].clone()),
            ("decay", v[1].clone()),
            ("sustain", v[2].clone()),
            ("release", v[3].clone()),
        ],
    )
}

/// Attack/decay; `decay` defaults to `attack`. Applied as two sets in
/// sequence, not one.
pub fn ad<P: PatOps>(pat: &P, value: &Value) -> P {
    let v = slots(value, 2);
    let attack = v[0].clone();
    let decay = if matches!(v[1], Value::Undefined) {
        attack.clone()
    } else {
        v[1].clone()
    };
    let with_attack = set_object(pat, vec![("attack", attack)]);
    set_object(&with_attack, vec![("decay", decay)])
}

/// Decay/sustain; `sustain` defaults to 0.
pub fn ds<P: PatOps>(pat: &P, value: &Value) -> P {
    let v = slots(value, 2);
    let sustain = if matches!(v[1], Value::Undefined) {
        Value::F64(0.0)
    } else {
        v[1].clone()
    };
    set_object(pat, vec![("decay", v[0].clone()), ("sustain", sustain)])
}

/// Attack/release; `release` defaults to `attack`.
pub fn ar<P: PatOps>(pat: &P, value: &Value) -> P {
    let v = slots(value, 2);
    let attack = v[0].clone();
    let release = if matches!(v[1], Value::Undefined) {
        attack.clone()
    } else {
        v[1].clone()
    };
    set_object(pat, vec![("attack", attack), ("release", release)])
}

/// `Number(v)` for the value shapes patterns carry.
pub(crate) fn as_js_number(v: &Value) -> f64 {
    match v {
        Value::F64(x) => *x,
        Value::Bool(b) => f64::from(*b),
        Value::Null => 0.0,
        Value::Str(s) => s.trim().parse::<f64>().unwrap_or(f64::NAN),
        _ => f64::NAN,
    }
}

// ---------------------------------------------------------------------------
// the tonal layer - scale / transpose / scaleTranspose
// ---------------------------------------------------------------------------

/// `Number(value)` with JavaScript's coercion table, for the argument shapes
/// the tonal combinators meet. `None` is NaN.
fn tonal_js_number(value: &Value) -> Option<f64> {
    match value {
        Value::F64(n) => Some(*n),
        Value::Str(s) => crate::tonaljs::js_number(s),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Null => Some(0.0),
        _ => None,
    }
}

/// Tonal combinators need real key access on the hap value; a JS-owned
/// object (produced by user fmaps, `.as(...)`, …) is opaque until
/// materialized. Borrow when already native, clone-with-materialized-value
/// otherwise.
pub(crate) fn materialized_hap(hap: &crate::Hap) -> std::borrow::Cow<'_, crate::Hap> {
    if matches!(hap.value, Value::JsValue(_)) {
        let mut owned = hap.clone();
        owned.value = crate::materialize_js_value(&hap.value);
        std::borrow::Cow::Owned(owned)
    } else {
        std::borrow::Cow::Borrowed(hap)
    }
}

/// Transposes notes by semitones (numeric amount) or a named interval.
pub fn transpose<P: PatOps>(pat: &P, amount: Value) -> P {
    pat.map_pitch_haps_native(move |hap| {
        let hap = &*materialized_hap(hap);
        let note = match hap.value.get("note") {
            Some(v) if !v.is_nullish() => v.clone(),
            Some(_) | None => hap.value.clone(),
        };
        let target: Value = match &note {
            Value::F64(n) => {
                // Numeric note: semitone addition. A non-number,
                // non-string amount sums to NaN.
                let semitones = match &amount {
                    Value::F64(s) => *s,
                    Value::Str(s) => crate::tonaljs::interval_semitones(s).unwrap_or(0) as f64,
                    _ => f64::NAN,
                };
                Value::F64(n + semitones)
            }
            Value::Str(s) if crate::tonaljs::is_score_note(s) => {
                // String note: enharmonic path. A numeric amount becomes an
                // interval via fromSemitones; non-integers, NaN and ±inf
                // name no interval and transpose returns "". So does an
                // integral amount outside i64: `as i64` would saturate into
                // a different interval, and a query error would silence
                // every layer. Strudel never throws here either: it yields
                // "" once tonal prints the number in exponent form (about
                // 1.7e21), and a float-rounded giant octave below that.
                let interval = match tonal_js_number(&amount) {
                    Some(n) => crate::tonaljs::exact_semitones(n)
                        .map_or_else(String::new, crate::tonaljs::interval_from_semitones),
                    None => amount.show(),
                };
                match crate::tonaljs::note_transpose_defaulted(s, &interval) {
                    Some(out) => Value::Str(out),
                    None => {
                        // Names tonal cannot parse (Strudel's 's' sharps
                        // included) abort the query with this TypeError -
                        // bug-compatible on purpose.
                        crate::signal_query_error(|| {
                            "Cannot add property oct, object is not extensible".into()
                        });
                        return None;
                    }
                }
            }
            // Not a note: the hap passes through unchanged (strudel.cc only
            // logs a warning; the pass-through is the observable part).
            _ => return Some(hap.clone()),
        };
        Some(hap.with_value(|value| match value {
            Value::Object(map) => {
                let mut map = map.clone();
                map.insert("note".into(), target.clone());
                Value::Object(map)
            }
            _ => target.clone(),
        }))
    })
}

/// Transposes by scale degrees; requires a preceding `.scale`.
pub fn scale_transpose<P: PatOps>(pat: &P, offset: Value) -> P {
    pat.map_pitch_haps_native(move |hap| {
        let hap = &*materialized_hap(hap);
        let Some(scale) = hap.scale_context().map(str::to_owned) else {
            crate::signal_query_error(|| "can only use scaleTranspose after .scale".into());
            return None;
        };
        let offset_number = tonal_js_number(&offset).unwrap_or(f64::NAN);
        let (note_value, _is_object) = match &hap.value {
            Value::Object(map) => (map.get("note").cloned().unwrap_or(Value::Undefined), true),
            Value::Str(s) => (Value::Str(s.clone()), false),
            _ => {
                crate::signal_query_error(|| "can only use scaleTranspose with notes".into());
                return None;
            }
        };
        // A non-string note is never in any scale; scale_offset raises the
        // same not-in-scale error for it.
        let note_str = match &note_value {
            Value::Str(s) => s.clone(),
            other => other.show(),
        };
        match crate::tonaljs::scale_offset(&scale, offset_number, &note_str) {
            Ok(transposed) => Some(hap.with_value(|value| match value {
                Value::Object(map) => {
                    let mut map = map.clone();
                    map.insert("note".into(), Value::Str(transposed.clone()));
                    Value::Object(map)
                }
                _ => Value::Str(transposed.clone()),
            })),
            Err(error) => {
                crate::signal_query_error(move || error.clone());
                None
            }
        }
    })
}

/// Interprets values against the named scale, per hap: `note ?? n ?? value`
/// picks the input; real notes quantize to the scale, numbers (with optional
/// `#` `b` `s` `f` suffixes as strings) become scale degrees; step-case
/// errors remove the hap; every produced hap is tagged with `context.scale`.
pub fn scale<P: PatOps>(pat: &P, scale_arg: Value) -> P {
    // A JS-owned array (`.scale(pat.withValue(v => [root, mode]))`) arrives
    // as an opaque JsValue; materialize it so the list path below applies.
    let scale_arg = if matches!(scale_arg, Value::JsValue(_)) {
        crate::materialize_js_value(&scale_arg)
    } else {
        scale_arg
    };
    // The mini `:` list syntax arrives as a list: flatten, join with spaces.
    let scale_name: Result<String, ()> = match &scale_arg {
        Value::Str(s) => Ok(s.clone()),
        Value::List(items) => {
            fn flatten(items: &[Value], out: &mut Vec<String>) {
                for item in items {
                    match item {
                        // Depth-1 flattening would do; deeper elements
                        // stringify to the same strings anyway.
                        Value::List(inner) => flatten(inner, out),
                        other => out.push(other.show()),
                    }
                }
            }
            let mut parts = Vec::new();
            flatten(items, &mut parts);
            Ok(parts.join(" "))
        }
        // A non-string scale name throws; the text below is the observable message.
        _ => Err(()),
    };
    let scale_name = match scale_name {
        Ok(name) => name,
        Err(()) => {
            let shown = scale_arg.show();
            return pat.map_pitch_haps_native(move |_| {
                let shown = shown.clone();
                crate::signal_query_error(move || {
                    format!("scaleName.replaceAll is not a function (scale: {shown})")
                });
                None
            });
        }
    };
    let scale_tag: std::sync::Arc<str> = std::sync::Arc::from(scale_name.as_str());
    pat.map_pitch_haps_native(move |hap| {
        let hap = &*materialized_hap(hap);
        // A plain value behaves as `{ n: value }`; note / n / value is
        // picked out and the rest carried along.
        let (note_or_step, others, is_object) = match &hap.value {
            Value::Object(map) => {
                let pick = ["note", "n", "value"]
                    .iter()
                    .filter_map(|key| map.get(key))
                    .find(|v| !v.is_nullish())
                    .cloned();
                let mut rest = crate::value::OrderedMap::new();
                for (key, value) in map.iter() {
                    if !matches!(key, "note" | "n" | "value") {
                        rest.insert(key.to_string(), value.clone());
                    }
                }
                (pick, rest, true)
            }
            // Lists and other object-likes yield no note/n/value - the
            // pass-through path below.
            Value::List(_) | Value::Pattern(_) | Value::JsValue(_) => {
                (None, crate::value::OrderedMap::new(), true)
            }
            // Destructuring null throws.
            Value::Null => {
                crate::signal_query_error(|| {
                    "Cannot destructure property 'note' of 'hVal' as it is null.".into()
                });
                return None;
            }
            plain => (
                (!plain.is_nullish()).then(|| plain.clone()),
                crate::value::OrderedMap::new(),
                false,
            ),
        };
        let Some(note_or_step) = note_or_step else {
            // No usable value: the hap passes through UNTAGGED.
            return Some(hap.clone());
        };

        let is_note = matches!(&note_or_step, Value::Str(s) if crate::tonaljs::is_score_note(s));
        let scale_note: Value = if is_note {
            // Note case: quantize. A bad scale name aborts the query.
            let note = note_or_step.as_str().expect("checked is_note");
            let midi = match crate::util::note_to_midi(note, 3) {
                Ok(midi) => midi,
                Err(error) => {
                    crate::signal_query_error(move || error.clone());
                    return None;
                }
            };
            match crate::tonaljs::nearest_scale_note(&scale_name, midi) {
                Ok(quantized) => Value::Str(quantized),
                Err(error) => {
                    crate::signal_query_error(move || error.clone());
                    return None;
                }
            }
        } else {
            // Step case: errors remove the hap.
            let converted = match &note_or_step {
                Value::Str(s) => crate::tonaljs::convert_step_string(s),
                other => match tonal_js_number(other) {
                    Some(n) => Ok((n, 0)),
                    None => Err(format!(
                        "invalid scale step \"{}\", expected number or integer with optional # b suffixes",
                        other.show()
                    )),
                },
            };
            let Ok((number, accidental_offset)) = converted else {
                return None;
            };
            let anchor = others.get("anchor").filter(|anchor| {
                // Truthiness gates the anchor path.
                match anchor {
                    Value::F64(n) => *n != 0.0 && !n.is_nan(),
                    Value::Str(s) => !s.is_empty(),
                    Value::Bool(b) => *b,
                    Value::Undefined | Value::Null => false,
                    _ => true,
                }
            });
            let stepped: Result<Value, String> = match anchor {
                Some(anchor) => {
                    // Numbers pass through, strings via note_to_midi,
                    // anything else is NaN.
                    let anchor_midi = match anchor {
                        Value::F64(n) => *n,
                        Value::Str(s) => {
                            crate::util::note_to_midi(s, 3).unwrap_or(f64::NAN)
                        }
                        _ => f64::NAN,
                    };
                    crate::tonaljs::step_in_named_scale(number, &scale_name, Some(anchor_midi))
                        .map(Value::F64)
                }
                None => crate::tonaljs::scale_step(number, &scale_name).map(Value::Str),
            };
            let stepped = match stepped {
                Ok(value) => value,
                Err(_) => return None, // an error removes the hap
            };
            if accidental_offset != 0 {
                // Accidental suffixes transpose the result; a numeric
                // scale note (anchor path) yields "".
                let base = match &stepped {
                    Value::Str(s) => s.clone(),
                    _ => String::new(),
                };
                Value::Str(crate::tonaljs::note_transpose(
                    &base,
                    &crate::tonaljs::interval_from_semitones(accidental_offset),
                ))
            } else {
                stepped
            }
        };

        let value = if is_object {
            let mut map = others;
            map.insert("note".into(), scale_note);
            Value::Object(map)
        } else {
            scale_note
        };
        Some(
            hap.with_value(|_| value.clone())
                .with_scale_context(scale_tag.clone()),
        )
    })
}

#[cfg(test)]
mod degrade_tests {
    use super::degrade_by;
    use crate::value::OrderedMap;
    use crate::{Hap, State, TimeSpan, Value, fastcat, pure, silence, stack};
    use rustel_fraction::Fraction;

    type HapSignature = (Option<TimeSpan>, TimeSpan, Value, Vec<(usize, usize)>);

    fn signature(haps: Vec<Hap>) -> Vec<HapSignature> {
        haps.into_iter()
            .map(|hap| (hap.whole, hap.part, hap.value, hap.context))
            .collect()
    }

    #[test]
    fn dedicated_degrade_node_matches_the_generic_graph() {
        let stepped = fastcat(vec![
            pure(Value::Str("bd".into())),
            silence(),
            pure(Value::Str("sd".into())),
            pure(Value::Str("hh".into())),
        ])
        .with_steps(Some(Fraction::int(4)));
        let layered = stack(vec![stepped.clone(), stepped.early(Fraction::new(1, 8))]);
        let patterns = [stepped, layered, crate::signal::sine()];
        let spans = [
            TimeSpan::new(Fraction::int(-2), Fraction::int(-1)),
            TimeSpan::new(Fraction::ZERO, Fraction::ONE),
            TimeSpan::new(Fraction::new(1, 3), Fraction::new(7, 3)),
            TimeSpan::new(Fraction::new(-3, 2), Fraction::new(5, 2)),
        ];

        for (pattern_index, pattern) in patterns.iter().enumerate() {
            let generic = degrade_by(pattern, 0.5);
            let dedicated = pattern.degrade_by_seeded(0.5, 0);
            assert_eq!(generic.steps, dedicated.steps, "pattern {pattern_index}");
            for seed in [0.0, 1.0, -2.5, f64::NAN] {
                let mut controls = OrderedMap::new();
                controls.insert("randSeed".into(), Value::F64(seed));
                for span in spans {
                    let state = State::new(span).set_controls(&controls);
                    assert_eq!(
                        signature(generic.query_state(&state)),
                        signature(dedicated.query_state(&state)),
                        "pattern {pattern_index}, seed {seed:?}, span {span:?}"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod iter_chunk_limit_tests {
    use super::{MAX_ITER_PARTS, apply_n, chunk, first_of, iter, last_of};
    use crate::{QueryLimit, Value, pure};
    use rustel_fraction::Fraction;

    /// The CLI covers the refusal; this covers the BOUNDARY, which the CLI
    /// cannot: one cycle of `chunk` at the limit is ~4.9 MB of haps, enough to
    /// fill the pipe the CLI harness reads from.
    #[test]
    fn the_guard_fires_one_past_the_limit_and_not_at_it() {
        let pat = pure(Value::F64(1.0));
        let refused =
            |limit: Option<QueryLimit>| matches!(limit, Some(QueryLimit::IterParts { .. }));

        for (parts, want_refusal) in [(MAX_ITER_PARTS, false), (MAX_ITER_PARTS + 1, true)] {
            let n = parts as i64;
            for (built, what) in [
                (chunk(&pat, n, None, false, false), "chunk"),
                (chunk(&pat, n, None, true, false), "chunkBack"),
                (chunk(&pat, n, None, false, true), "fastchunk"),
                (iter(&pat, Fraction::int(i128::from(n)), false), "iter"),
                (iter(&pat, Fraction::int(i128::from(n)), true), "iterBack"),
                (first_of(&pat, n, None), "firstOf"),
                (last_of(&pat, n, None), "lastOf"),
                (apply_n(&pat, n, None), "applyN"),
            ] {
                let got = built
                    .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
                    .err();
                assert_eq!(
                    refused(got.clone()),
                    want_refusal,
                    "{what}({parts}) refusal was {got:?}, wanted refusal={want_refusal}"
                );
            }
        }
    }
}

#[cfg(test)]
mod step_combination_overflow_tests {
    use super::{euclidish, jux};
    use crate::value::FunctionRef;
    use crate::{Pattern, QueryLimit, Value, pure};
    use rustel_fraction::Fraction;
    use std::sync::Arc;

    #[test]
    fn jux_refuses_unrepresentable_step_lcm() {
        let pat = pure(Value::Str("bd".into())).with_steps(Some(Fraction::int(i128::MAX)));
        let two_steps = FunctionRef::registered(
            "twoSteps",
            Arc::new(|pat: Pattern| pat.with_steps(Some(Fraction::int(2)))),
            true,
        );
        assert_eq!(
            jux(&pat, Some(&two_steps))
                .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
                .err(),
            Some(QueryLimit::NativeFraction { operation: "juxBy" })
        );
    }

    #[test]
    fn euclidish_refuses_unrepresentable_groove() {
        let by = Fraction::from_f64(1e38).unwrap();
        let pat = pure(Value::Str("hh".into()));
        assert_eq!(
            euclidish(&pat, 15, 17, by)
                .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
                .err(),
            Some(QueryLimit::NativeFraction {
                operation: "euclidish"
            })
        );
    }
}

#[cfg(test)]
mod step_scaling_overflow_tests {
    use super::{contract, drop, expand, extend, pace, ply, replicate, stepcat, take};
    use crate::{Pattern, QueryLimit, Value, fastcat, pure};
    use rustel_fraction::Fraction;

    /// The `s("bd cp")` shape: two events, `_steps` of two.
    fn stepped_pattern() -> Pattern {
        fastcat(vec![
            pure(Value::Str("bd".into())),
            pure(Value::Str("cp".into())),
        ])
    }

    /// `Fraction::from_f64` accepts any magnitude below 2^127 (~1.7e38), so
    /// `1e38` converts cleanly; `2 * 1e38` then overflows `i128`. This factor
    /// used to reach `Fraction::mul` and panic at score-evaluation time,
    /// ending the native session.
    fn overflowing_factor() -> Fraction {
        Fraction::from_f64(1e38).expect("1e38 is inside the conversion bound")
    }

    fn refusal(built: &Pattern) -> Option<QueryLimit> {
        refusal_in_cycle(built, 0)
    }

    /// The refusal from querying the whole of `cycle`.
    fn refusal_in_cycle(built: &Pattern, cycle: i128) -> Option<QueryLimit> {
        built
            .try_query_arc_sorted(Fraction::int(cycle), Fraction::int(cycle + 1))
            .err()
    }

    fn native_fraction(operation: &'static str) -> Option<QueryLimit> {
        Some(QueryLimit::NativeFraction { operation })
    }

    /// `s("bd")`: one event, `_steps` of one, so `1e38` times it is a legal
    /// step count and only query-time arithmetic can overflow.
    fn one_step() -> Pattern {
        pure(Value::Str("bd".into()))
    }

    #[test]
    fn overflowing_factors_refuse_through_the_typed_channel() {
        let pat = stepped_pattern();
        let factor = overflowing_factor();
        for (built, operation) in [
            (expand(&pat, factor), "expand"),
            (extend(&pat, factor), "extend"),
            (replicate(&pat, factor), "replicate"),
            (ply(&pat, factor), "ply"),
        ] {
            assert_eq!(
                refusal(&built),
                Some(QueryLimit::NativeFraction { operation }),
                "{operation}(1e38) must refuse without panicking"
            );
        }
    }

    #[test]
    fn legal_factors_still_scale_the_step_count() {
        let pat = stepped_pattern();
        assert_eq!(expand(&pat, Fraction::int(2)).steps, Some(Fraction::int(4)));
        assert_eq!(expand(&pat, Fraction::new(1, 2)).steps, Some(Fraction::ONE));
        // A step-less receiver still passes through step-less.
        let stepless = pat.clone().with_steps(None);
        assert_eq!(expand(&stepless, Fraction::int(2)).steps, None);
        // extend keeps its events: two per cycle, doubled into one cycle.
        let haps = extend(&pat, Fraction::int(2))
            .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            .expect("extend(2) is legal");
        assert_eq!(haps.len(), 4, "extend(2) lost or duplicated events");
    }

    #[test]
    fn overflowing_contract_factors_refuse_through_the_typed_channel() {
        let pat = stepped_pattern();
        // A huge-denominator factor - reachable from a string argument like
        // "1/170141183460469231731687303715884105727", which `FromStr`
        // accepts - turns the divide into `2 * i128::MAX`.
        assert_eq!(
            refusal(&contract(&pat, Fraction::new(1, i128::MAX))),
            Some(QueryLimit::NativeFraction {
                operation: "contract"
            }),
            "contract with a huge-denominator factor must refuse"
        );
        // The scalar-only chain: expand(1e38) legally lifts a one-step
        // pattern to steps ≈ 1e38, then contract(0.5) doubles it past
        // i128::MAX.
        let huge = expand(&pure(Value::F64(1.0)), overflowing_factor());
        assert_eq!(
            refusal(&contract(&huge, Fraction::new(1, 2))),
            Some(QueryLimit::NativeFraction {
                operation: "contract"
            }),
            "contract(0.5) over steps ≈ 1e38 must refuse"
        );
        // The literal f64 `contract(1e-38)` never reaches the divide: the
        // Farey search in `Fraction::from_f64` is capped at a 1e7
        // denominator, so every double below ~1e-7 rounds to ZERO and the
        // pre-existing zero guard returns `nothing()`. Pinned so the
        // reasoning stays visible.
        let tiny = Fraction::from_f64(1e-38).expect("1e-38 converts");
        assert_eq!(tiny, Fraction::ZERO);
        let haps = contract(&pat, tiny)
            .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            .expect("the zero-factor guard yields nothing(), not a refusal");
        assert!(haps.is_empty());
    }

    #[test]
    fn overflowing_pace_targets_refuse_through_the_typed_channel() {
        // steps = 1/2 (a legal expand), target ≈ 1e38: pace computes
        // target/steps ≈ 2e38, which leaves the i128 representation.
        let half = expand(&pure(Value::F64(1.0)), Fraction::new(1, 2));
        assert_eq!(
            refusal(&pace(&half, overflowing_factor())),
            Some(QueryLimit::NativeFraction { operation: "pace" }),
            "pace(1e38) over half a step must refuse"
        );
    }

    #[test]
    fn legal_contracts_and_paces_still_scale() {
        let pat = stepped_pattern();
        // contract(2) halves a two-step count; contract(0) stays `nothing()`.
        assert_eq!(contract(&pat, Fraction::int(2)).steps, Some(Fraction::ONE));
        let zero = contract(&pat, Fraction::ZERO);
        assert!(
            zero.try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
                .expect("contract(0) is nothing(), not a refusal")
                .is_empty()
        );
        // pace(4) on a two-step pattern doubles the rate: steps 4, four
        // events per cycle.
        let paced = pace(&pat, Fraction::int(4));
        assert_eq!(paced.steps, Some(Fraction::int(4)));
        let haps = paced
            .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            .expect("pace(4) is legal");
        assert_eq!(haps.len(), 4, "pace(4) lost or duplicated events");
    }

    #[test]
    fn pace_refuses_late_cycles_instead_of_panicking_in_fast() {
        // target/steps ≈ 5e37 fits, so construction succeeds; the speed-up's
        // query-time multiply then leaves i128 from cycle 4 on (4 × 5e37).
        let paced = pace(&stepped_pattern(), overflowing_factor());
        assert_eq!(paced.steps, Some(overflowing_factor()));
        for cycle in [4, 1000] {
            assert_eq!(
                refusal_in_cycle(&paced, cycle),
                native_fraction("pace"),
                "pace(1e38) queried at cycle {cycle} must refuse without panicking"
            );
        }
    }

    #[test]
    fn extend_and_replicate_refuse_late_cycles_instead_of_panicking_in_fast() {
        // One step × 1e38 is a legal step count, so the step guard passes and
        // only the speed-up's query-time multiply (2 × 1e38) can overflow.
        let factor = overflowing_factor();
        for (built, operation) in [
            (extend(&one_step(), factor), "extend"),
            (replicate(&one_step(), factor), "replicate"),
        ] {
            assert_eq!(built.steps, Some(factor), "{operation}(1e38) steps");
            assert_eq!(
                refusal_in_cycle(&built, 2),
                native_fraction(operation),
                "{operation}(1e38) queried at cycle 2 must refuse without panicking"
            );
        }
    }

    #[test]
    fn checked_fast_also_refuses_hap_times_outside_i128() {
        // A one-step receiver with a half-cycle event, queried over
        // [0, 1/1e38): the query-time multiply lands exactly on one inner
        // cycle, but mapping the event at 1/2 back divides it into 1/(2e38).
        let factor = overflowing_factor();
        let halves = stepped_pattern().with_steps(Some(Fraction::ONE));
        let built = extend(&halves, factor);
        assert_eq!(built.steps, Some(factor));
        assert_eq!(
            built
                .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE.div(factor))
                .err(),
            native_fraction("extend")
        );
    }

    #[test]
    fn ply_refuses_late_cycles_instead_of_panicking_in_fast() {
        // The step product fits for a one-step receiver; each event's
        // `fast(1e38)` overflows once cycle 2 is queried.
        let plied = ply(&one_step(), overflowing_factor());
        assert_eq!(plied.steps, Some(overflowing_factor()));
        assert_eq!(refusal_in_cycle(&plied, 2), native_fraction("ply"));
    }

    #[test]
    fn legal_speed_ups_are_unchanged_at_late_cycles() {
        let pat = stepped_pattern();
        let late = |built: &Pattern| {
            built
                .try_query_arc_sorted(Fraction::int(1000), Fraction::int(1001))
                .expect("a legal factor queries without a refusal")
                .len()
        };
        assert_eq!(late(&pace(&pat, Fraction::int(4))), 4, "pace(4)");
        assert_eq!(late(&extend(&pat, Fraction::int(2))), 4, "extend(2)");
        assert_eq!(late(&replicate(&pat, Fraction::int(2))), 4, "replicate(2)");
        let plied = ply(&pat, Fraction::int(3));
        assert_eq!(plied.steps, Some(Fraction::int(6)));
        assert_eq!(late(&plied), 6, "ply(3)");
        // fast(0) is still silence, steps included.
        let still = pace(&pat, Fraction::ZERO);
        assert_eq!(still.steps, Some(Fraction::ZERO));
        assert_eq!(late(&still), 0, "pace(0)");
        assert_eq!(ply(&pat, Fraction::ZERO).steps, Some(Fraction::ZERO));
    }

    #[test]
    fn take_and_drop_refuse_quotients_outside_i128() {
        // Steps legally lifted to ≈1e38: a half step is 1/(2e38) of a cycle.
        let huge = expand(&pure(Value::F64(1.0)), overflowing_factor());
        let half = Fraction::new(1, 2);
        // A huge-denominator amount (the `FromStr` spelling
        // "1/170141183460469231731687303715884105727") against two steps.
        let tiny = Fraction::new(1, i128::MAX);
        let two_steps = stepped_pattern();
        for (built, operation, what) in [
            (take(&huge, half), "take", "take(0.5) over 1e38 steps"),
            (
                take(&huge, half.neg()),
                "take",
                "take(-0.5) over 1e38 steps",
            ),
            (drop(&huge, half), "drop", "drop(0.5) over 1e38 steps"),
            (
                drop(&huge, half.neg()),
                "drop",
                "drop(-0.5) over 1e38 steps",
            ),
            (take(&two_steps, tiny), "take", "take(1/MAX) over 2 steps"),
            (
                take(&two_steps, tiny.neg()),
                "take",
                "take(-1/MAX) over 2 steps",
            ),
            (drop(&two_steps, tiny), "drop", "drop(1/MAX) over 2 steps"),
            (
                drop(&two_steps, tiny.neg()),
                "drop",
                "drop(-1/MAX) over 2 steps",
            ),
        ] {
            assert_eq!(refusal(&built), native_fraction(operation), "{what}");
        }
    }

    #[test]
    fn drop_names_itself_when_its_retained_amount_overflows_in_take() {
        let two_steps = stepped_pattern();
        // drop((MAX-1)/MAX) retains -(2^127)/MAX, which fits with the
        // numerator i128::MIN; taking from the end must negate it.
        let negation = Fraction::new(i128::MAX - 1, i128::MAX);
        // drop(3/(2^126+1)) retains -MAX/(2^126+1), which fits; halving its
        // magnitude over two steps needs the denominator 2^127 + 2.
        let quotient = Fraction::new(3, (1 << 126) + 1);
        for amount in [negation, quotient] {
            assert_eq!(
                refusal(&drop(&two_steps, amount)),
                native_fraction("drop"),
                "drop({amount:?})"
            );
        }
    }

    #[test]
    fn legal_takes_and_drops_still_slice() {
        let pat = stepped_pattern();
        let values = |built: Pattern| -> Vec<Value> {
            built
                .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
                .expect("a legal take/drop is not a refusal")
                .into_iter()
                .map(|hap| hap.value)
                .collect()
        };
        let bd = || Value::Str("bd".into());
        let cp = || Value::Str("cp".into());
        assert_eq!(values(take(&pat, Fraction::ONE)), vec![bd()]);
        assert_eq!(values(take(&pat, Fraction::int(-1))), vec![cp()]);
        assert_eq!(values(drop(&pat, Fraction::ONE)), vec![cp()]);
        assert_eq!(values(drop(&pat, Fraction::int(-1))), vec![bd()]);
        assert_eq!(values(take(&pat, Fraction::int(5))), vec![bd(), cp()]);
        assert!(values(drop(&pat, Fraction::int(2))).is_empty());
        // An i128::MIN overshoot still returns the whole pattern unnegated.
        assert_eq!(
            values(take(&pat, Fraction::int(i128::MIN))),
            vec![bd(), cp()]
        );
    }

    #[test]
    fn take_and_drop_refuse_zoom_maps_outside_i128() {
        let factor = overflowing_factor();
        let huge = expand(&one_step(), factor);
        let half = Fraction::new(1, 2);
        let query = |built: &Pattern, begin: Fraction, end: Fraction| {
            built.try_query_arc_sorted(begin, end).err()
        };
        // drop(1) keeps the slot [1/N, 1) of N ≈ 1e38 steps: mapping the
        // query end 1/2 into it needs the denominator 2N.
        assert_eq!(
            query(&drop(&huge, Fraction::ONE), Fraction::ZERO, half),
            native_fraction("drop")
        );
        // take(7) keeps [0, 7/N) (N is not a multiple of 7): the same
        // query-span overflow.
        assert_eq!(
            query(&take(&huge, Fraction::int(7)), Fraction::ZERO, half),
            native_fraction("take")
        );
        // Over cycle 2 the in-cycle map fits, but adding the cycle back
        // (2 + 1/N) does not.
        assert_eq!(
            refusal_in_cycle(&drop(&huge, Fraction::ONE), 2),
            native_fraction("drop")
        );
        // Hap-span map: the whole-cycle query maps into [1/N, 1) exactly, but
        // an event ending at 1/5 maps back through a denominator ≈ 5e38.
        let fifths = fastcat(vec![pure(Value::F64(1.0)); 5]).with_steps(Some(Fraction::ONE));
        let fifths = expand(&fifths, factor);
        assert_eq!(
            refusal(&drop(&fifths, Fraction::ONE)),
            native_fraction("drop")
        );
    }

    #[test]
    fn legal_takes_and_drops_match_plain_zoom_on_fractional_spans() {
        type Signature = Vec<(Option<crate::TimeSpan>, crate::TimeSpan, Value)>;
        let pat = fastcat(vec![
            pure(Value::Str("a".into())),
            pure(Value::Str("b".into())),
            pure(Value::Str("c".into())),
            pure(Value::Str("d".into())),
        ]);
        let signature = |built: &Pattern, begin: Fraction, end: Fraction| -> Signature {
            built
                .try_query_arc_sorted(begin, end)
                .expect("a legal take/drop is not a refusal")
                .into_iter()
                .map(|hap| (hap.whole, hap.part, hap.value))
                .collect()
        };
        let quarter = |n: i128| Fraction::new(n, 4);
        for (built, zoomed, what) in [
            (
                take(&pat, Fraction::ONE),
                pat.zoom(Fraction::ZERO, quarter(1)),
                "take(1)",
            ),
            (
                take(&pat, Fraction::new(-3, 2)),
                pat.zoom(Fraction::new(5, 8), Fraction::ONE),
                "take(-1.5)",
            ),
            (
                drop(&pat, Fraction::ONE),
                pat.zoom(quarter(1), Fraction::ONE),
                "drop(1)",
            ),
            (
                drop(&pat, Fraction::int(-3)),
                pat.zoom(Fraction::ZERO, quarter(1)),
                "drop(-3)",
            ),
        ] {
            assert_eq!(built.steps, zoomed.steps, "{what} steps");
            for (begin, end) in [
                (Fraction::ZERO, Fraction::ONE),
                (Fraction::new(1, 3), Fraction::new(7, 3)),
                (Fraction::new(-5, 2), Fraction::new(-3, 2)),
                (Fraction::int(1000), Fraction::new(4001, 4)),
            ] {
                assert_eq!(
                    signature(&built, begin, end),
                    signature(&zoomed, begin, end),
                    "{what} over [{begin:?}, {end:?})"
                );
            }
        }
    }

    #[test]
    fn stepcat_refuses_totals_and_bounds_outside_i128() {
        let n = overflowing_factor();
        let entry = |weight: Option<Fraction>| (weight, one_step());
        // Two slices of 1e38 steps: the total is 2e38.
        let total = vec![entry(Some(n)), entry(Some(n))];
        // An unweighted entry averages the known weights, whose sum is 2e38.
        let average = vec![entry(Some(n)), entry(Some(n)), entry(None)];
        // The total is 2^63, but the first slot ends at 2^-64 / 2^63 = 2^-127.
        let two_64 = Fraction::int(1 << 64);
        let bound = vec![
            entry(Some(Fraction::ONE.div(two_64))),
            entry(Some(Fraction::int(i128::MAX).div(two_64))),
        ];
        // Weights 1, (p-q)/q and p(q-1)/q total p, so the middle slot runs
        // from 1/p to 1/q: both fit, but `compress`'s width (p-q)/(pq) does
        // not, for coprime p, q with pq just past i128::MAX.
        let q: i128 = 13_043_817_825_332_782_202;
        let p = i128::MAX / q + 1;
        let width = vec![
            entry(Some(Fraction::ONE)),
            entry(Some(Fraction::new(p - q, q))),
            entry(Some(Fraction::new(p * (q - 1), q))),
        ];
        for (items, what) in [
            (total, "total"),
            (average, "average"),
            (bound, "bound"),
            (width, "width"),
        ] {
            assert_eq!(
                refusal(&stepcat(&items)),
                native_fraction("stepcat"),
                "stepcat {what}"
            );
        }
    }

    #[test]
    fn patterned_factors_refuse_through_step_join() {
        // `s("bd").expand("1e38 1e38")`: each slice is a legal 1e38-step
        // expansion, and stepJoin's stepcat totals them past i128::MAX.
        let registry = crate::register::default_registry();
        let expand = registry.get("expand").expect("expand is registered");
        let factors = fastcat(vec![pure(Value::F64(1e38)), pure(Value::F64(1e38))]);
        let built = expand.call(&[factors], one_step());
        assert_eq!(refusal(&built), native_fraction("stepcat"));
        // An ordinary patterned factor still expands through the same path.
        let factors = fastcat(vec![pure(Value::F64(1.0)), pure(Value::F64(2.0))]);
        let haps = expand
            .call(&[factors], stepped_pattern())
            .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            .expect("expand(\"1 2\") is legal");
        assert_eq!(haps.len(), 4, "expand(\"1 2\") lost or duplicated events");
    }
}

#[cfg(test)]
mod tonal_range_tests {
    use super::{scale, transpose};
    use crate::{Pattern, QueryArcOutcome, State, TimeSpan, Value, fastcat, pure};
    use rustel_fraction::Fraction;

    fn note_pair() -> Pattern {
        fastcat(vec![
            pure(Value::Str("c4".into())),
            pure(Value::Str("e5".into())),
        ])
    }

    fn first_cycle() -> State {
        State::new(TimeSpan::new(Fraction::ZERO, Fraction::ONE))
    }

    fn queried_values(pat: &Pattern) -> Vec<Value> {
        let outcome = pat
            .query_arc_outcome(&first_cycle())
            .expect("not a resource refusal");
        match outcome {
            QueryArcOutcome::Haps(haps) => haps.iter().map(|hap| hap.value.clone()).collect(),
            QueryArcOutcome::Thrown(message) => panic!("unexpected query error: {message}"),
        }
    }

    fn empty_notes(count: usize) -> Vec<Value> {
        vec![Value::Str(String::new()); count]
    }

    /// `note("c4 e5").transpose(-1e19)` once panicked: `n as i64` saturated
    /// to i64::MIN, whose `abs` overflowed in `interval_from_semitones`
    /// (debug) or indexed out of bounds through `MIN % 12 == -8` (release).
    /// Strudel never throws for such an amount, so every integral amount
    /// outside i64, -2^63 and 2^63 included, takes the NaN/±inf path: each
    /// hap's note becomes "" and the query carries on.
    #[test]
    fn out_of_range_amounts_yield_empty_notes() {
        for amount in [
            -1e19,
            1e19,
            i64::MIN as f64, // -2^63
            i64::MAX as f64, // rounds up to 2^63
            -1e300,
            1e300,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ] {
            let pat = transpose(&note_pair(), Value::F64(amount));
            assert_eq!(queried_values(&pat), empty_notes(2), "amount {amount}");
        }
    }

    /// The largest amounts that still convert exactly, 2^63 - 1024 either
    /// way, transpose by the exact interval without panicking. Strudel's
    /// float fromSemitones rounds this far out, so these names are the exact
    /// ones, not strudel.cc's.
    #[test]
    fn largest_exact_amounts_transpose() {
        let largest = 2f64.powi(63) - 1024.0;
        let down = transpose(&note_pair(), Value::F64(-largest));
        assert_eq!(
            queried_values(&down),
            vec![
                Value::Str("Ab-768614336404564562".into()),
                Value::Str("C-768614336404564560".into()),
            ]
        );
        let up = transpose(&note_pair(), Value::F64(largest));
        assert_eq!(
            queried_values(&up),
            vec![
                Value::Str("E768614336404564569".into()),
                Value::Str("G#768614336404564570".into()),
            ]
        );
    }

    /// Ordinary amounts are untouched: integral semitones transpose in both
    /// directions, and a non-integer still yields "" notes without an error.
    #[test]
    fn ordinary_amounts_still_transpose() {
        let up = transpose(&note_pair(), Value::F64(7.0));
        assert_eq!(
            queried_values(&up),
            vec![Value::Str("G4".into()), Value::Str("B5".into())]
        );

        let down = transpose(&note_pair(), Value::F64(-12.0));
        assert_eq!(
            queried_values(&down),
            vec![Value::Str("C3".into()), Value::Str("E4".into())]
        );

        let fractional = transpose(&note_pair(), Value::F64(1.5));
        assert_eq!(queried_values(&fractional), empty_notes(2));
    }

    /// `scale` quantizes a note through an unbounded f64 octave, and
    /// `note("c-800000000000000000").scale("C:major")` once panicked the
    /// same way: its octave shift of -9.6e18 semitones saturated to
    /// i64::MIN. An octave shift outside i64 now quantizes to "": Strudel's
    /// result for the NaN (`c-`) and ±inf (400-digit) octaves, and the
    /// nearest non-aborting one for a finite octave Strudel keeps but an
    /// i64 semitone count cannot name.
    #[test]
    fn extreme_octaves_quantize_to_empty_notes() {
        let long_digits = "1".repeat(400);
        for note in [
            "c-800000000000000000".to_string(),
            "c800000000000000000".to_string(),
            format!("c-{long_digits}"),
            format!("c{long_digits}"),
            "c-".to_string(),
        ] {
            let pat = scale(
                &pure(Value::Str(note.clone())),
                Value::Str("C:major".into()),
            );
            assert_eq!(queried_values(&pat), empty_notes(1), "note {note:.24}");
        }
        let ordinary = scale(
            &pure(Value::Str("d#5".into())),
            Value::Str("C:major".into()),
        );
        assert_eq!(queried_values(&ordinary), vec![Value::Str("E5".into())]);
    }
}

#[cfg(test)]
mod tests {
    use super::{bite, chop, expand, pace, ply_for_each, ply_with, slice, striate};
    use crate::register::default_registry;
    use crate::value::{FunctionRef, OrderedMap};
    use crate::{
        Pattern, QueryArcOutcome, QueryLimit, State, TimeSpan, Value, fastcat, pure, slowcat,
    };
    use rustel_fraction::Fraction;

    /// Pins that `echo`, `stut` and `echoWith` refuse an exact string time whose
    /// copy shift leaves the native fraction range as `NativeFraction`, naming the
    /// combinator the score wrote.
    #[test]
    fn an_echo_time_whose_copy_shift_leaves_i128_refuses() {
        let registry = default_registry();
        let receiver = || pure(Value::Str("bd".into()));
        let three = pure(Value::F64(3.0));
        // 2^126: copy two shifts by 2^127, one past `i128::MAX`.
        let time = pure(Value::Str("85070591730234615865843651857942052864".into()));
        let feedback = pure(Value::F64(0.8));
        let cases = [
            (
                "echo",
                vec![three.clone(), time.clone(), feedback.clone()],
                "echo",
            ),
            (
                "stut",
                vec![three.clone(), feedback.clone(), time.clone()],
                "stut",
            ),
            ("echoWith", vec![three.clone(), time.clone()], "echoWith"),
        ];
        for (name, args, operation) in cases {
            let pattern = registry
                .get(name)
                .unwrap_or_else(|| panic!("missing {name}"))
                .call(&args, receiver());
            assert_eq!(
                pattern
                    .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
                    .err(),
                Some(QueryLimit::NativeFraction { operation }),
                "{name} must refuse an echo time whose copy shift leaves i128"
            );
        }
    }

    /// Pins that `bite` does not wrap a negative index: over `<0 1 2 3>`,
    /// `bite(4, "-1 0 1 2")` leaves cycle 0's first quarter silent instead of
    /// replaying slot 3.
    #[test]
    fn bite_with_a_negative_index_zooms_before_the_cycle() {
        let num = |v: i32| pure(Value::F64(f64::from(v)));
        let per_cycle = slowcat((0..4).map(num).collect());
        let indices = fastcat([-1, 0, 1, 2].map(num).to_vec());
        let pat = bite(&per_cycle, &num(4), &indices);
        let parts: Vec<_> = pat
            .query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            .into_iter()
            .map(|hap| (hap.part.begin, hap.part.end, hap.value))
            .collect();
        assert_eq!(
            parts,
            vec![
                (Fraction::new(1, 4), Fraction::new(1, 2), Value::F64(0.0)),
                (Fraction::new(1, 2), Fraction::new(3, 4), Value::F64(0.0)),
                (Fraction::new(3, 4), Fraction::ONE, Value::F64(0.0)),
            ]
        );
    }

    /// An operation name and that combinator applied with a count of four.
    type Fourfold = (&'static str, fn(&Pattern) -> Pattern);

    /// `chop`, `striate`, `plyWith` and `plyForEach` by four.
    fn fourfold_combinators() -> [Fourfold; 4] {
        [
            ("chop", |pat| chop(pat, 4)),
            ("striate", |pat| striate(pat, 4)),
            ("plyWith", |pat| ply_with(pat, Fraction::int(4), None)),
            ("plyForEach", |pat| {
                ply_for_each(pat, Fraction::int(4), None)
            }),
        ]
    }

    /// Pins that `chop`, `striate`, `plyWith` and `plyForEach` scale the
    /// receiver's step count, so `s("bd cp").chop(4).pace(4)` plays four events
    /// per cycle, and that a step-less receiver stays step-less.
    #[test]
    fn fourfold_combinators_scale_the_step_count() {
        let s = |name: &str| pure(Value::object([("s".into(), Value::Str(name.into()))]));
        let bd_cp = || fastcat(vec![s("bd"), s("cp")]);
        for (operation, by_four) in fourfold_combinators() {
            let built = by_four(&bd_cp());
            assert_eq!(built.steps, Some(Fraction::int(8)), "{operation}(4)");
            let paced = pace(&built, Fraction::int(4))
                .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
                .expect("a legal pace is not a refusal");
            assert_eq!(paced.len(), 4, "{operation}(4).pace(4)");
            assert_eq!(
                by_four(&bd_cp().with_steps(None)).steps,
                None,
                "{operation}(4) of a step-less receiver"
            );
        }
    }

    /// Pins that `slice` and `splice` take the step count of their index
    /// pattern, so `slice(4, "0 1").pace(4)` plays four events per cycle.
    #[test]
    fn slice_and_splice_take_the_index_step_count() {
        let sample = pure(Value::Str("bev".into()));
        let four = pure(Value::F64(4.0));
        let indices = fastcat(vec![pure(Value::F64(0.0)), pure(Value::F64(1.0))]);
        let sliced = slice(&sample, &four, &indices);
        for (operation, built) in [("slice", sliced.clone()), ("splice", sliced.splice())] {
            assert_eq!(built.steps, Some(Fraction::int(2)), "{operation}");
            let paced = pace(&built, Fraction::int(4))
                .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
                .expect("a legal pace is not a refusal");
            assert_eq!(paced.len(), 4, "{operation}.pace(4)");
        }
        let stepless = slice(&sample, &four, &indices.with_steps(None));
        assert_eq!(stepless.steps, None, "slice over step-less indices");
        assert_eq!(
            stepless.splice().steps,
            None,
            "splice over step-less indices"
        );
    }

    /// Pins that `chop`, `striate`, `plyWith` and `plyForEach` refuse a step
    /// count that leaves the native fraction range as `NativeFraction`.
    #[test]
    fn a_scaled_step_count_outside_i128_refuses() {
        // `s("bd").expand(1e38)` has a legal step count that four times overflows.
        let factor = Fraction::from_f64(1e38).expect("1e38 converts");
        let huge = expand(&pure(Value::Str("bd".into())), factor);
        for (operation, by_four) in fourfold_combinators() {
            assert_eq!(
                by_four(&huge)
                    .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
                    .err(),
                Some(QueryLimit::NativeFraction { operation }),
                "{operation}(4) over 1e38 steps"
            );
        }
    }

    /// Queries the first cycle of `pat` through the `queryArc` boundary.
    fn first_cycle_outcome(pat: &Pattern) -> QueryArcOutcome {
        pat.query_arc_outcome(&State::new(TimeSpan::new(Fraction::ZERO, Fraction::ONE)))
            .expect("not a resource refusal")
    }

    /// Pins that `chop` over a string, number, boolean, `undefined` or `null` hap
    /// throws strudel.cc's `'begin' in` error at the query boundary, so the cycle
    /// is silent instead of carrying `{begin, end}`-only slices.
    #[test]
    fn chop_over_a_primitive_value_throws_to_silence() {
        for value in [
            Value::Str("bd".into()),
            Value::F64(1.0),
            Value::Bool(true),
            Value::Undefined,
            Value::Null,
        ] {
            let expected = format!(
                "Cannot use 'in' operator to search for 'begin' in {}",
                value.show()
            );
            match first_cycle_outcome(&chop(&pure(value.clone()), 4)) {
                QueryArcOutcome::Thrown(message) => assert_eq!(message, expected, "{value:?}"),
                QueryArcOutcome::Haps(haps) => panic!("chop sliced {value:?}: {haps:?}"),
            }
        }
    }

    /// Pins that `chop` slices a non-primitive hap without throwing: an object
    /// keeps its controls in every slice and a function yields bare
    /// `begin`/`end` slices.
    #[test]
    fn chop_over_an_object_or_function_value_slices_it() {
        let slices = |base: &OrderedMap| -> Vec<Value> {
            [(0.0, 0.5), (0.5, 1.0)]
                .map(|(begin, end)| {
                    let mut controls = base.clone();
                    controls.insert("begin".into(), Value::F64(begin));
                    controls.insert("end".into(), Value::F64(end));
                    Value::Object(controls)
                })
                .to_vec()
        };
        let bd = OrderedMap::from_entries([("s".into(), Value::Str("bd".into()))]);
        let function = Value::Function(FunctionRef::registered(
            "id",
            std::sync::Arc::new(|pat| pat),
            true,
        ));
        for (value, base) in [
            (Value::Object(bd.clone()), bd),
            (function, OrderedMap::new()),
        ] {
            let QueryArcOutcome::Haps(haps) = first_cycle_outcome(&chop(&pure(value.clone()), 2))
            else {
                panic!("chop threw over {value:?}");
            };
            let values: Vec<_> = haps.into_iter().map(|hap| hap.value).collect();
            assert_eq!(values, slices(&base), "{value:?}");
        }
    }
}
