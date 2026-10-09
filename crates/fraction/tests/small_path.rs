//! The 64-bit path of `Fraction` against exact big-integer arithmetic.
//!
//! Components below 2^31 take the 64-bit path and larger ones take the
//! `i128` path. Each path must give the reduced result of the exact
//! operation, on both sides of the limit.

use std::cmp::Ordering;

use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive, Zero};
use rustel_fraction::Fraction;

/// Mirrors the private `SMALL_LIMIT` of the crate.
const LIMIT: i128 = 1 << 31;

/// xorshift64*: the run repeats exactly and needs no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A component of one size class: tiny, musical, around the limit,
    /// anywhere below the limit, or far above. Every magnitude stays below
    /// 2^62, so each exact result fits in `i128`.
    fn component(&mut self) -> i128 {
        let magnitude = match self.next() % 5 {
            0 => (self.next() % 17) as i128,
            1 => (self.next() % 4096) as i128,
            2 => LIMIT - 3 + (self.next() % 7) as i128,
            3 => (self.next() % (1 << 31)) as i128,
            _ => (self.next() >> 2) as i128,
        };
        if self.next().is_multiple_of(2) {
            magnitude
        } else {
            -magnitude
        }
    }

    fn fraction(&mut self) -> Fraction {
        loop {
            let denominator = self.component();
            if denominator != 0 {
                return Fraction::new(self.component(), denominator);
            }
        }
    }
}

fn gcd(mut a: BigInt, mut b: BigInt) -> BigInt {
    while !b.is_zero() {
        let remainder = &a % &b;
        a = b;
        b = remainder;
    }
    a.abs()
}

/// The reduced form of `n / d` with a positive denominator.
fn reduced(n: BigInt, d: BigInt) -> (i128, i128) {
    let divisor = gcd(n.clone(), d.clone());
    let (n, d) = if d.is_negative() {
        (-n / &divisor, -d / &divisor)
    } else {
        (n / &divisor, d / &divisor)
    };
    (
        n.to_i128().expect("the numerator fits"),
        d.to_i128().expect("the denominator fits"),
    )
}

fn parts(value: Fraction) -> (i128, i128) {
    (value.numer(), value.denom())
}

fn big(value: Fraction) -> (BigInt, BigInt) {
    (BigInt::from(value.numer()), BigInt::from(value.denom()))
}

fn is_small(value: Fraction) -> bool {
    value.numer().abs() < LIMIT && value.denom() < LIMIT
}

fn assert_exact(a: Fraction, b: Fraction) {
    let (an, ad) = big(a);
    let (bn, bd) = big(b);
    assert_eq!(
        a.checked_add(b).map(parts),
        Some(reduced(&an * &bd + &bn * &ad, &ad * &bd)),
        "{a:?} + {b:?}"
    );
    assert_eq!(
        a.checked_sub(b).map(parts),
        Some(reduced(&an * &bd - &bn * &ad, &ad * &bd)),
        "{a:?} - {b:?}"
    );
    assert_eq!(
        a.checked_mul(b).map(parts),
        Some(reduced(&an * &bn, &ad * &bd)),
        "{a:?} * {b:?}"
    );
    let quotient = (!bn.is_zero()).then(|| reduced(&an * &bd, &ad * &bn));
    assert_eq!(a.checked_div(b).map(parts), quotient, "{a:?} / {b:?}");
    assert_eq!(a.cmp(&b), (&an * &bd).cmp(&(&bn * &ad)), "{a:?} cmp {b:?}");

    // Truncation rounds toward zero, so a negative value with a remainder
    // sits one above its floor.
    let truncated = &an / &ad;
    let floor = if an.is_negative() && !(&an % &ad).is_zero() {
        truncated - 1
    } else {
        truncated
    };
    assert_eq!(
        parts(a.floor()),
        (floor.to_i128().expect("the floor fits"), 1),
        "floor of {a:?}"
    );
}

#[test]
fn every_operation_matches_exact_arithmetic_on_both_paths() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut small_pairs = 0_u32;
    for _ in 0..100_000 {
        let (a, b) = (rng.fraction(), rng.fraction());
        if is_small(a) && is_small(b) {
            small_pairs += 1;
        }
        assert_exact(a, b);
    }
    // A run with few pairs on the 64-bit path proves nothing about the path.
    assert!(
        small_pairs > 10_000,
        "only {small_pairs} pairs took the 64-bit path"
    );
}

#[test]
fn values_at_the_limit_match_exact_arithmetic() {
    let edges = [
        0,
        1,
        -1,
        LIMIT - 1,
        -(LIMIT - 1),
        LIMIT,
        -LIMIT,
        LIMIT + 1,
        -(LIMIT + 1),
    ];
    let denominators = [1, 2, 3, LIMIT - 1, LIMIT, LIMIT + 1];
    let values: Vec<Fraction> = edges
        .iter()
        .flat_map(|&n| denominators.iter().map(move |&d| Fraction::new(n, d)))
        .collect();
    for &a in &values {
        for &b in &values {
            assert_exact(a, b);
        }
    }
}

#[test]
fn the_largest_small_values_stay_exact_and_ordered() {
    let below = Fraction::new(LIMIT - 2, LIMIT - 1);
    let above = Fraction::new(LIMIT - 1, LIMIT - 2);
    assert_eq!(below.cmp(&above), Ordering::Less);
    assert_eq!(above.cmp(&below), Ordering::Greater);
    assert_eq!(below.cmp(&below), Ordering::Equal);

    // The sum of two such values is the largest numerator the 64-bit path
    // forms: 2^63 - 3 * 2^32 + 4.
    assert_exact(above, above);
    assert_exact(below, above);
    assert_exact(above, Fraction::new(-(LIMIT - 1), LIMIT - 2));
}
