/*
rng.rs - Exact legacy and precise Strudel random number generators
Random number algorithms adapted from Strudel packages/core/signal.mjs.
Copyright (C) 2024 Strudel contributors

Rust implementation and additions:
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

const SCALE: f64 = 536_870_912.0; // 2^29
const U32_SCALE: f64 = 4_294_967_296.0; // 2^32

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RngMode {
    #[default]
    Legacy,
    Precise,
}

pub fn use_rng(mode: RngMode) {
    crate::settings::set_rng_mode(match mode {
        RngMode::Legacy => 0,
        RngMode::Precise => 1,
    });
}

pub fn rng_mode() -> RngMode {
    match crate::settings::rng_mode() {
        0 => RngMode::Legacy,
        _ => RngMode::Precise,
    }
}

fn murmur_hash_finalizer(mut value: u32) -> u32 {
    value ^= value >> 16;
    value = value.wrapping_mul(0x85eb_ca6b);
    value ^= value >> 13;
    value = value.wrapping_mul(0xc2b2_ae35);
    value ^= value >> 16;
    value
}

fn time_key(time: f64) -> i64 {
    (time * SCALE).floor() as i64
}

fn decorrelate(time: i64, index: u32, seed: i32) -> u32 {
    let low_bits = time as u32;
    let high_bits = time.div_euclid(1i64 << 32) as u32;
    let mut key = low_bits ^ (high_bits ^ 0x85eb_ca6b).wrapping_mul(0xc2b2_ae35);
    key ^= (index ^ 0x7f4a_7c15).wrapping_mul(0x9e37_79b9);
    key ^= ((seed as u32) ^ 0x1656_67b1).wrapping_mul(0x27d4_eb2d);
    key
}

fn precise_at(time: i64, index: u32, seed: i32) -> f64 {
    f64::from(murmur_hash_finalizer(decorrelate(time, index, seed))) / U32_SCALE
}

pub fn precise_rands_at_time(time: f64, count: usize, seed: i32) -> Vec<f64> {
    let time = time_key(time);
    (0..count)
        .map(|index| precise_at(time, index as u32, seed))
        .collect()
}

fn xorwise(value: i32) -> i32 {
    let a = value.wrapping_shl(13) ^ value;
    let b = (a >> 17) ^ a;
    b.wrapping_shl(5) ^ b
}

fn legacy_time_seed(time: f64) -> i32 {
    let divided = time / 300.0;
    let fraction = divided - divided.trunc();
    xorwise((fraction * SCALE).trunc() as i32)
}

fn legacy_seed_rand(seed: i32) -> f64 {
    f64::from(seed % 536_870_912) / SCALE
}

pub fn legacy_rands_at_time(time: f64, count: usize) -> Vec<f64> {
    let mut seed = legacy_time_seed(time);
    if count == 1 {
        return vec![legacy_seed_rand(seed).abs()];
    }
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(legacy_seed_rand(seed));
        seed = xorwise(seed);
    }
    out
}

/// strudel.cc `getRandsAtTime(t, n, seed)`. Legacy mode adds the raw seed to
/// the time (`__timeToRands(t + seed, n)`), so fractional seeds and seeds
/// beyond int32 shift the stream as on strudel.cc. Precise mode hashes the
/// seed independently. It reads the seed through `Math.imul`, which applies
/// ToInt32 (the wrap in `compose::to_int32`), not a saturating cast.
pub fn rands_at_time(time: f64, count: usize, seed: f64) -> Vec<f64> {
    match rng_mode() {
        RngMode::Legacy => legacy_rands_at_time(time + seed, count),
        RngMode::Precise => precise_rands_at_time(time, count, crate::compose::to_int32(seed)),
    }
}

pub fn rand_at_time(time: f64, seed: f64) -> f64 {
    rands_at_time(time, 1, seed)[0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_repeats_every_300_cycles() {
        // Avoid claiming exact periodicity for arbitrary floats: strudel.cc's
        // `frac((time + 300) / 300)` exposes IEEE-754 rounding differences.
        for time in [0.0, 150.0] {
            assert_eq!(
                legacy_rands_at_time(time, 8),
                legacy_rands_at_time(time + 300.0, 8)
            );
        }
    }

    #[test]
    fn precise_seed_and_index_are_decorrelated() {
        let a = precise_rands_at_time(1.25, 4, 0);
        let b = precise_rands_at_time(1.25, 4, 1);
        assert_ne!(a, b);
        assert!(a.iter().all(|value| (0.0..1.0).contains(value)));
    }

    #[test]
    fn matches_pinned_node_vectors() {
        assert_eq!(
            legacy_rands_at_time(1.25, 4),
            vec![
                -0.7287282031029463,
                0.9341024849563837,
                -0.5098182689398527,
                -0.5608645845204592,
            ]
        );
        assert_eq!(
            precise_rands_at_time(-10.25, 4, 2),
            vec![
                0.5351264134515077,
                0.5197606100700796,
                0.1719492357224226,
                0.2793034662026912,
            ]
        );
    }

    #[test]
    fn mode_switch_is_observable_and_reversible() {
        crate::settings::RuntimeSettings::default().with(|| {
            use_rng(RngMode::Legacy);
            let legacy = rand_at_time(1.0, 2.0);
            use_rng(RngMode::Precise);
            let precise = rand_at_time(1.0, 2.0);
            assert_ne!(legacy, precise);
            use_rng(RngMode::Legacy);
        });
    }
}
