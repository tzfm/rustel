/*
euclid.rs - Bjorklund/Euclidean rhythm helpers
Rhythm construction adapted from Strudel packages/core/euclid.mjs.
Copyright (C) 2023 Rohan Drape and Strudel contributors

Rust implementation and additions:
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

use crate::util::rotate;

type Groups = Vec<Vec<u8>>;

/// Largest Euclidean mask the native runtime will materialise.
///
/// Bounds allocations for the Bjorklund work groups and the pattern layer's
/// one structure node per step. Callers that expose resource errors should
/// reject larger masks before entering [`bjorklund`]; its check is the
/// allocation-safety backstop for other callers.
pub const MAX_EUCLID_STEPS: usize = 16_384;

fn zip_concat(left: &[Vec<u8>], right: &[Vec<u8>]) -> Groups {
    left.iter()
        .zip(right)
        .map(|(a, b)| {
            let mut value = a.clone();
            value.extend_from_slice(b);
            value
        })
        .collect()
}

fn recurse_owned(
    mut ons: usize,
    mut offs: usize,
    mut xs: Groups,
    mut ys: Groups,
) -> (Groups, Groups) {
    while ons.min(offs) > 1 {
        if ons > offs {
            let old_xs = xs;
            let (paired_xs, rest_xs) = old_xs.split_at(offs);
            xs = zip_concat(paired_xs, &ys);
            ys = rest_xs.to_vec();
            (ons, offs) = (offs, ons - offs);
        } else {
            let old_ys = ys;
            let (paired_ys, rest_ys) = old_ys.split_at(ons);
            xs = zip_concat(&xs, paired_ys);
            ys = rest_ys.to_vec();
            (ons, offs) = (ons, offs - ons);
        }
    }
    (xs, ys)
}

/// The `bjorklund` euclidean generator, matching strudel.cc exactly.
///
/// Negative pulses invert the resulting binary pattern. Upstream throws a
/// `RangeError` when `abs(pulses) > steps`; Rust reports that explicitly.
pub fn bjorklund(pulses: i32, steps: usize) -> Result<Vec<u8>, String> {
    if steps > MAX_EUCLID_STEPS {
        return Err(format!(
            "bjorklund: steps ({steps}) exceeds native limit ({MAX_EUCLID_STEPS})"
        ));
    }
    let inverted = pulses < 0;
    let ons = pulses.unsigned_abs() as usize;
    if ons > steps {
        return Err(format!(
            "bjorklund: abs(pulses) ({ons}) exceeds steps ({steps})"
        ));
    }
    let offs = steps - ons;
    let ones = vec![vec![1]; ons];
    let zeros = vec![vec![0]; offs];
    let (xs, ys) = recurse_owned(ons, offs, ones, zeros);
    let mut pattern: Vec<u8> = xs
        .into_iter()
        .flatten()
        .chain(ys.into_iter().flatten())
        .collect();
    if inverted {
        for value in &mut pattern {
            *value = 1 - *value;
        }
    }
    Ok(pattern)
}

pub fn euclid_rot(pulses: i32, steps: usize, rotation: isize) -> Result<Vec<u8>, String> {
    let pattern = bjorklund(pulses, steps)?;
    if rotation == 0 {
        Ok(pattern)
    } else {
        Ok(rotate(&pattern, rotation.saturating_neg()))
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_EUCLID_STEPS, bjorklund, euclid_rot};

    #[test]
    fn matches_the_toussaint_cases_from_the_strudel_mini_suite() {
        let cases = [
            (1, 2, "10"),
            (2, 5, "10100"),
            (3, 8, "10010010"),
            (4, 9, "101010100"),
            (5, 8, "10110110"),
            (7, 12, "101101011010"),
        ];
        for (pulses, steps, expected) in cases {
            let got: String = bjorklund(pulses, steps)
                .unwrap()
                .into_iter()
                .map(|value| char::from(b'0' + value))
                .collect();
            assert_eq!(got, expected, "bjorklund({pulses}, {steps})");
        }
    }

    #[test]
    fn supports_inversion_rotation_and_zero() {
        assert_eq!(bjorklund(0, 4).unwrap(), vec![0, 0, 0, 0]);
        assert_eq!(bjorklund(-1, 4).unwrap(), vec![0, 1, 1, 1]);
        // `rotate(b, -rotation)` uses JS Array.slice negative-index rules.
        assert_eq!(euclid_rot(1, 4, 1).unwrap(), vec![0, 1, 0, 0]);
        assert_eq!(euclid_rot(1, 4, 5).unwrap(), vec![1, 0, 0, 0]);
        assert_eq!(euclid_rot(1, 4, -5).unwrap(), vec![1, 0, 0, 0]);
        assert_eq!(euclid_rot(1, 4, isize::MIN).unwrap(), vec![1, 0, 0, 0]);
    }

    #[test]
    fn native_limit_refuses_before_oversized_allocation() {
        assert!(matches!(
            bjorklund(3, MAX_EUCLID_STEPS + 1),
            Err(message) if message.contains("exceeds native limit")
        ));
    }
}
