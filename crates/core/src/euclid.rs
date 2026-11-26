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

/// Largest Euclidean mask the native runtime will materialise.
///
/// Bounds allocations for the Bjorklund work groups and the pattern layer's
/// one structure node per step. Callers that expose resource errors should
/// reject larger masks before entering [`bjorklund`]; its check is the
/// allocation-safety backstop for other callers.
pub const MAX_EUCLID_STEPS: usize = 16_384;

#[derive(Clone, Copy)]
struct Op {
    more_ons: bool,
    repeat: u16,
}

/// Emit one Bjorklund group at `level` directly into `out`.
///
/// At level 0, `is_x` is `[1]` and `!is_x` is `[0]` (flipped when `invert_bit`
/// is 1). Each step `d` combines `x_{d+1} = x_d ++ y_d^{repeat}` and sets
/// `y_{d+1}` to `x_d` when `more_ons` is true or `y_d` otherwise.
fn emit_group(level: usize, is_x: bool, ops: &[Op], invert_bit: u8, out: &mut Vec<u8>) {
    if level == 0 {
        out.push(u8::from(is_x) ^ invert_bit);
        return;
    }
    let prev = level - 1;
    let op = ops[prev];
    if is_x {
        emit_group(prev, true, ops, invert_bit, out);
        for _ in 0..op.repeat {
            emit_group(prev, false, ops, invert_bit, out);
        }
    } else {
        emit_group(prev, op.more_ons, ops, invert_bit, out);
    }
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
    let invert_bit = u8::from(pulses < 0);
    let mut ons = pulses.unsigned_abs() as usize;
    if ons > steps {
        return Err(format!(
            "bjorklund: abs(pulses) ({ons}) exceeds steps ({steps})"
        ));
    }
    let mut offs = steps - ons;
    // Collapsing consecutive `offs -= ons` runs reduces depth to standard
    // Euclidean division steps, bounded by <= 2 * log_phi(MAX_EUCLID_STEPS) <= 44.
    let mut ops = [Op {
        more_ons: false,
        repeat: 0,
    }; 64];
    let mut depth = 0usize;
    while ons.min(offs) > 1 {
        if ons > offs {
            ops[depth] = Op {
                more_ons: true,
                repeat: 1,
            };
            (ons, offs) = (offs, ons - offs);
        } else {
            let k = offs / ons;
            ops[depth] = Op {
                more_ons: false,
                repeat: k as u16,
            };
            offs %= ons;
        }
        depth += 1;
    }
    let mut pattern = Vec::with_capacity(steps);
    for _ in 0..ons {
        emit_group(depth, true, &ops[..depth], invert_bit, &mut pattern);
    }
    for _ in 0..offs {
        emit_group(depth, false, &ops[..depth], invert_bit, &mut pattern);
    }
    Ok(pattern)
}

fn emit_group_slice(
    level: usize,
    is_x: bool,
    ops: &[Op],
    invert_bit: u8,
    out: &mut [u8],
    len: &mut usize,
) {
    if level == 0 {
        out[*len] = u8::from(is_x) ^ invert_bit;
        *len += 1;
        return;
    }
    let prev = level - 1;
    let op = ops[prev];
    if is_x {
        emit_group_slice(prev, true, ops, invert_bit, out, len);
        for _ in 0..op.repeat {
            emit_group_slice(prev, false, ops, invert_bit, out, len);
        }
    } else {
        emit_group_slice(prev, op.more_ons, ops, invert_bit, out, len);
    }
}

/// Write `euclid_rot(pulses, steps, rotation)` directly into `out[..steps]`
/// without heap allocation when `steps <= out.len()`.
pub fn euclid_rot_into(
    pulses: i32,
    steps: usize,
    rotation: isize,
    out: &mut [u8],
) -> Result<usize, String> {
    if steps > MAX_EUCLID_STEPS || steps > out.len() {
        return Err(format!(
            "bjorklund: steps ({steps}) exceeds buffer limit ({})",
            MAX_EUCLID_STEPS.min(out.len())
        ));
    }
    let invert_bit = u8::from(pulses < 0);
    let mut ons = pulses.unsigned_abs() as usize;
    if ons > steps {
        return Err(format!(
            "bjorklund: abs(pulses) ({ons}) exceeds steps ({steps})"
        ));
    }
    let mut offs = steps - ons;
    let mut ops = [Op {
        more_ons: false,
        repeat: 0,
    }; 64];
    let mut depth = 0usize;
    while ons.min(offs) > 1 {
        if ons > offs {
            ops[depth] = Op {
                more_ons: true,
                repeat: 1,
            };
            (ons, offs) = (offs, ons - offs);
        } else {
            let k = offs / ons;
            ops[depth] = Op {
                more_ons: false,
                repeat: k as u16,
            };
            offs %= ons;
        }
        depth += 1;
    }
    let mut len = 0usize;
    for _ in 0..ons {
        emit_group_slice(depth, true, &ops[..depth], invert_bit, out, &mut len);
    }
    for _ in 0..offs {
        emit_group_slice(depth, false, &ops[..depth], invert_bit, out, &mut len);
    }
    let slice = &mut out[..len];
    if rotation != 0 && !slice.is_empty() {
        let n = rotation.saturating_neg();
        let len_isize = isize::try_from(slice.len()).unwrap_or(isize::MAX);
        let at = if n < 0 {
            len_isize.saturating_add(n).max(0)
        } else {
            n.min(len_isize)
        } as usize;
        slice.rotate_left(at);
    }
    Ok(len)
}

pub fn euclid_rot(pulses: i32, steps: usize, rotation: isize) -> Result<Vec<u8>, String> {
    let mut pattern = bjorklund(pulses, steps)?;
    if rotation != 0 && !pattern.is_empty() {
        let n = rotation.saturating_neg();
        let len = isize::try_from(pattern.len()).unwrap_or(isize::MAX);
        let at = if n < 0 {
            len.saturating_add(n).max(0)
        } else {
            n.min(len)
        } as usize;
        pattern.rotate_left(at);
    }
    Ok(pattern)
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
