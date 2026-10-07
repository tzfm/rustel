/*
signal.rs - continuous control signals
Signal functions adapted from Strudel packages/core/signal.mjs.
Copyright (C) 2024 Strudel contributors

Rust implementation and additions:
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

use crate::host_value::{HostValue, Pointer};
use crate::rng::rand_at_time;
use crate::{Pattern, Value, signal};

pub fn saw() -> Pattern {
    signal(|time, _| Value::F64(time.to_f64().rem_euclid(1.0)))
}

pub fn saw2() -> Pattern {
    signal(|time, _| Value::F64(time.to_f64().rem_euclid(1.0) * 2.0 - 1.0))
}

pub fn isaw() -> Pattern {
    signal(|time, _| Value::F64(1.0 - time.to_f64().rem_euclid(1.0)))
}

pub fn isaw2() -> Pattern {
    signal(|time, _| Value::F64((1.0 - time.to_f64().rem_euclid(1.0)) * 2.0 - 1.0))
}

// sine/cosine go through crate::fdlibm rather than the platform libm, so
// hap values are bit-identical to strudel.cc's.
pub fn sine2() -> Pattern {
    signal(|time, _| Value::F64(crate::fdlibm::sin(std::f64::consts::TAU * time.to_f64())))
}

pub fn sine() -> Pattern {
    signal(|time, _| {
        Value::F64((crate::fdlibm::sin(std::f64::consts::TAU * time.to_f64()) + 1.0) / 2.0)
    })
}

pub fn cosine2() -> Pattern {
    signal(|time, _| Value::F64(crate::fdlibm::cos(std::f64::consts::TAU * time.to_f64())))
}

pub fn cosine() -> Pattern {
    signal(|time, _| {
        Value::F64((crate::fdlibm::cos(std::f64::consts::TAU * time.to_f64()) + 1.0) / 2.0)
    })
}

pub fn square() -> Pattern {
    signal(|time, _| Value::F64((time.to_f64() * 2.0).rem_euclid(2.0).floor()))
}

pub fn square2() -> Pattern {
    signal(|time, _| Value::F64((time.to_f64() * 2.0).rem_euclid(2.0).floor() * 2.0 - 1.0))
}

/// Flipped squares: high first.
pub fn isquare() -> Pattern {
    signal(|time, _| Value::F64(1.0 - (time.to_f64() * 2.0).rem_euclid(2.0).floor()))
}

pub fn isquare2() -> Pattern {
    signal(|time, _| Value::F64((1.0 - (time.to_f64() * 2.0).rem_euclid(2.0).floor()) * 2.0 - 1.0))
}

pub fn time() -> Pattern {
    signal(|time, _| Value::F64(time.to_f64()))
}

pub fn rand() -> Pattern {
    signal(|time, controls| {
        let seed = controls
            .get("randSeed")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        Value::F64(rand_at_time(time.to_f64(), seed))
    })
}

/// Triangle: a saw up then a saw down, each half a cycle.
pub fn tri() -> Pattern {
    crate::fastcat(vec![saw(), isaw()])
}

pub fn tri2() -> Pattern {
    crate::fastcat(vec![saw2(), isaw2()])
}

/// A triangle that starts high.
pub fn itri() -> Pattern {
    crate::fastcat(vec![isaw(), saw()])
}

pub fn itri2() -> Pattern {
    crate::fastcat(vec![isaw2(), saw2()])
}

/// The mouse position when the host gives no [`Pointer`]: the constant 0,
/// the same constant a headless strudel.cc run settles on. A score that
/// reaches for the mouse is not an error, it is a constant.
pub fn mouse_position() -> Pattern {
    signal(|_, _| Value::F64(0.0))
}

/// Bipolar rand.
pub fn rand2() -> Pattern {
    crate::combinators::to_bipolar(&rand())
}

/// `irand(i)`: random integers in `0..i`. The multiply runs through
/// [`crate::compose::ComposeOp::Mul`], so string/boolean operands coerce
/// with script semantics.
pub fn irand_value(i: Value) -> Pattern {
    rand().fmap(
        move |x| match crate::compose::ComposeOp::Mul.apply_scalar(x, &i) {
            Value::F64(product) => Value::F64(product.trunc()),
            other => other,
        },
    )
}

/// `irand` with a patterned bound.
pub fn irand(ipat: &Pattern) -> Pattern {
    ipat.fmap_to_pattern(|i| irand_value(i.clone()))
        .inner_join()
}

/// A per-cycle random permutation of `0..n` (shuffle's index stream). The
/// sort must be ascending and STABLE, and the cycle index uses Fraction
/// math so segment boundaries cannot drift through f64.
pub fn randrun(n: usize) -> Pattern {
    use rustel_fraction::Fraction;
    let sig = signal(move |time, controls| {
        if n == 0 {
            return Value::Undefined;
        }
        let seed = controls
            .get("randSeed")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        // Without adding 0.5, the first cycle is always 0,1,2,3,...
        let at = time.to_f64().floor() + 0.5;
        let rands = crate::rng::rands_at_time(at, n, seed);
        let mut nums: Vec<usize> = (0..n).collect();
        nums.sort_by(|&a, &b| {
            rands[a]
                .partial_cmp(&rands[b])
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let index = (time.cycle_pos() * Fraction::from(n as i64)).floor();
        let index = index.to_f64().rem_euclid(n as f64) as usize;
        Value::F64(nums[index] as f64)
    });
    crate::combinators::segment(&sig, Fraction::from(n as i64))
}

/// A list of `n` random values, sampled together at each moment.
///
/// Unlike every other random signal, `randL` ignores `randSeed` - kept that
/// way on purpose: a seeded score must get the same list strudel.cc gives.
/// `partials(randL(8))` is the use: one value per harmonic, one instant.
/// A count past [`crate::MAX_STEPWISE_ENTRIES`] is refused as
/// `StepwiseExpansion` before any list is built.
pub fn rand_list_value(count: Value) -> Pattern {
    let count = count.as_f64().unwrap_or(0.0);
    if !count.is_finite() || count < 1.0 {
        return signal(|_, _| Value::List(Vec::new()));
    }
    let count = count as u64;
    if count > crate::MAX_STEPWISE_ENTRIES {
        return crate::query_limit_pattern(crate::mark_stepwise_refusal(
            crate::QueryLimit::StepwiseExpansion {
                operation: "randL",
                minimum_entries: count,
                limit: crate::MAX_STEPWISE_ENTRIES,
            },
        ));
    }
    let count = count as usize;
    signal(move |time, _| {
        let rands = crate::rng::rands_at_time(time.to_f64(), count, 0.0);
        Value::List(
            rands
                .into_iter()
                .map(|value| Value::F64(value.abs()))
                .collect(),
        )
    })
}

/// `randL(nPat)` for a patterned length.
pub fn rand_list(npat: &Pattern) -> Pattern {
    npat.fmap_to_pattern(|n| rand_list_value(n.clone()))
        .inner_join()
}

/// `brandBy(p)`: a weighted coin. Boolean-valued (`x < p`), not 0/1.
pub fn brand_by_value(p: Value) -> Pattern {
    rand().fmap(move |x| crate::compose::ComposeOp::Lt.apply_scalar(x, &p))
}

/// `brandBy` with a patterned probability.
pub fn brand_by(ppat: &Pattern) -> Pattern {
    ppat.fmap_to_pattern(|p| brand_by_value(p.clone()))
        .inner_join()
}

/// A fair coin.
pub fn brand() -> Pattern {
    brand_by_value(Value::F64(0.5))
}

/// Smoothed value noise: smootherstep between random values at whole-cycle
/// times.
pub fn perlin() -> Pattern {
    signal(|time, controls| {
        let seed = controls
            .get("randSeed")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let t = time.to_f64();
        let ta = t.floor();
        let ra = rand_at_time(ta, seed);
        let rb = rand_at_time(ta + 1.0, seed);
        let x = t - ta;
        let smoother = 6.0 * x.powi(5) - 15.0 * x.powi(4) + 10.0 * x.powi(3);
        Value::F64(ra + smoother * (rb - ra))
    })
}

/// `berlin`: perlin's shape with the smoothing taken out.
///
/// The ridge climbs linearly from one random floor to that floor plus the
/// NEXT random value, then drops - sawteeth of random height rather than
/// perlin's smoothed hills. Halving keeps a sum of two 0..1 values in 0..1.
pub fn berlin() -> Pattern {
    signal(|time, controls| {
        let seed = controls
            .get("randSeed")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let t = time.to_f64();
        let previous = t.floor();
        let next = previous + 1.0;
        let bottom = rand_at_time(previous, seed);
        let height = rand_at_time(next, seed);
        let top = bottom + height;
        // The ridge span is always exactly one cycle.
        let percent = t - previous;
        Value::F64((bottom + percent * (top - bottom)) / 2.0)
    })
}

/// Every signal the score language exposes as a bare global, by name.
///
/// Installed as globals in the QuickJS host: a signal is a *value*, not a
/// function, so `note(saw.range(0, 7))` is valid. The mouse names read
/// `pointer` when there is one, and [`mouse_position`] when there is not.
pub fn signals(pointer: Option<&Pointer>) -> Vec<(&'static str, Pattern)> {
    let mouse = |axis: Option<&HostValue>| axis.map_or_else(mouse_position, HostValue::signal);
    let (x, y) = (pointer.map(|p| &p.x), pointer.map(|p| &p.y));
    vec![
        ("saw", saw()),
        ("saw2", saw2()),
        ("isaw", isaw()),
        ("isaw2", isaw2()),
        ("sine", sine()),
        ("sine2", sine2()),
        ("cosine", cosine()),
        ("cosine2", cosine2()),
        ("square", square()),
        ("square2", square2()),
        ("isquare", isquare()),
        ("isquare2", isquare2()),
        ("tri", tri()),
        ("tri2", tri2()),
        ("itri", itri()),
        ("itri2", itri2()),
        ("time", time()),
        // Both spellings of each axis are exposed.
        ("mousex", mouse(x)),
        ("mouseX", mouse(x)),
        ("mousey", mouse(y)),
        ("mouseY", mouse(y)),
        ("rand", rand()),
        ("rand2", rand2()),
        // `brand` is a bare value like the waves, not a function.
        ("brand", brand()),
        ("perlin", perlin()),
        ("berlin", berlin()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustel_fraction::Fraction;

    /// Pinned samples of `perlin.segment(4)` from strudel.cc (legacy RNG).
    #[test]
    fn perlin_matches_the_strudel_samples() {
        let expected = [
            0.0,
            0.05378073193423916,
            0.25977108255028725,
            0.46576143316633534,
        ];
        for (i, want) in expected.iter().enumerate() {
            // Signals sample at the query-span BEGIN (rand.segment proves
            // it: integer-begin haps reproduce rand at integers).
            let begin = Fraction::new(i as i128, 4);
            let end = Fraction::new(i as i128 + 1, 4);
            let got = match perlin().query_arc(begin, end)[0].value {
                Value::F64(v) => v,
                ref other => panic!("non-number perlin value {other:?}"),
            };
            assert!(
                (got - want).abs() < 1e-12,
                "perlin sample {i}: {got} vs {want}"
            );
        }
    }

    #[test]
    fn base_signals_match_the_strudel_samples() {
        let quarter = Fraction::new(1, 4);
        assert_eq!(
            saw().query_arc(quarter, Fraction::new(1, 2))[0].value,
            Value::F64(0.25)
        );
        assert_eq!(
            saw2().query_arc(quarter, Fraction::new(1, 2))[0].value,
            Value::F64(-0.5)
        );
        assert_eq!(
            isaw().query_arc(quarter, Fraction::new(1, 2))[0].value,
            Value::F64(0.75)
        );
    }
}
