//! Exact rational time.
//!
//! Cycle arithmetic uses reduced `i128/i128` fractions. This port of
//! `fraction.js@5.2.1` preserves its numeric conventions and serialized form:
//!
//! ```text
//! "[ 0/1 → 1/5 | note:c3 ]"
//! ```
//!
//! * `floor()` rounds toward negative infinity, so `sam(-0.25) == -1`.
//! * `show()` always includes the denominator: zero is `0/1`, one is `1/1`.
//! * `gcd(a/b, c/d) = gcd(a,c)/lcm(b,d)`; `lcm(a/b, c/d) = lcm(a,c)/gcd(b,d)`.
//! * Modulo takes the dividend's sign: `(-7/3) % (1/2) = -1/3`.
//!
//! Unlike fraction.js's arbitrary-precision representation, each reduced
//! component must fit in `i128`. Arithmetic checks for overflow: checked
//! methods return `None`, and ordinary arithmetic methods panic.

// The inherent `add`/`sub`/`mul`/`div`/`rem`/`neg` names mirror fraction.js's
// `a.add(b)`. The standard-library traits are also implemented, so `a + b`
// works too.
#![allow(clippy::should_implement_trait)]

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use num_bigint::{BigInt, BigUint, Sign};
use num_traits::{One, ToPrimitive, Zero};

/// An exact rational. Always normalised: `d > 0`, `gcd(|n|, d) == 1`, and
/// zero is canonically `0/1`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fraction {
    n: i128,
    d: i128,
}

/// `i128::MIN.abs()` overflows, so magnitudes are taken unsigned and the
/// result is narrowed once at the end.
#[inline]
fn gcd_i128(a: i128, b: i128) -> i128 {
    let divisor = gcd_u128(a.unsigned_abs(), b.unsigned_abs());
    ck(i128::try_from(divisor).ok(), "gcd")
}

#[inline]
fn gcd_u128(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

/// Components below this magnitude take the 64-bit path.
///
/// A product of two such values is below 2^62, and a sum of two such
/// products fits in `i64`. The 64-bit path therefore needs no overflow check
/// and no 128-bit multiply or divide.
///
/// A cycle time on the 1/1000 query grid stays below the limit for about
/// 2.1 million cycles. A time from `from_f64` has a denominator up to 10^7
/// and passes the limit after about 214 cycles. Such values take the `i128`
/// path.
const SMALL_LIMIT: i128 = 1 << 31;

#[inline]
fn gcd_u64(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

/// Reduce a numerator and a positive denominator from the 64-bit path.
#[inline]
fn reduced_small(n: i64, d: i64) -> Fraction {
    debug_assert!(d > 0);
    if n == 0 {
        return Fraction::ZERO;
    }
    let divisor = gcd_u64(n.unsigned_abs(), d as u64) as i64;
    Fraction {
        n: i128::from(n / divisor),
        d: i128::from(d / divisor),
    }
}

fn gcd_biguint(mut a: BigUint, mut b: BigUint) -> BigUint {
    while b != BigUint::from(0u8) {
        let remainder = &a % &b;
        a = b;
        b = remainder;
    }
    a
}

#[inline]
fn div_signed_by_u128(value: i128, divisor: u128) -> Option<i128> {
    debug_assert_ne!(divisor, 0);
    if divisor <= i128::MAX as u128 {
        return Some(value / divisor as i128);
    }
    (value == i128::MIN && divisor == i128::MIN.unsigned_abs()).then_some(-1)
}

fn checked_normalised(n: i128, d: i128) -> Option<Fraction> {
    if d == 0 {
        return None;
    }
    if n == 0 {
        return Some(Fraction::ZERO);
    }
    let negative = (n < 0) != (d < 0);
    let mut numerator = n.unsigned_abs();
    let mut denominator = d.unsigned_abs();
    let divisor = gcd_u128(numerator, denominator);
    numerator /= divisor;
    denominator /= divisor;
    if denominator > i128::MAX as u128 {
        return None;
    }
    let numerator = if negative {
        if numerator == i128::MIN.unsigned_abs() {
            i128::MIN
        } else {
            i128::try_from(numerator).ok()?.checked_neg()?
        }
    } else {
        i128::try_from(numerator).ok()?
    };
    Some(Fraction {
        n: numerator,
        d: denominator as i128,
    })
}

/// fraction.js's bounded Stern-Brocot search, with consecutive moves batched.
///
/// A scalar walk changes one bound per iteration. A run of `k` lower-bound
/// moves is exactly `(a + k*c)/(b + k*d)`; upper-bound moves are symmetric.
/// The mediants are monotone, so a binary search finds the last move whose
/// f64 comparison has the same result. This preserves the scalar state
/// machine, including equality caused by f64 rounding, while making a run
/// logarithmic rather than linear in the denominator limit.
fn farey_approximation(p1: f64, limit: i128) -> (i128, i128) {
    farey_approximation_with_runs(p1, limit).0
}

fn farey_approximation_with_runs(p1: f64, limit: i128) -> ((i128, i128), usize) {
    fn last_true(mut low: i128, mut high: i128, predicate: impl Fn(i128) -> bool) -> i128 {
        debug_assert!(low >= 1 && low <= high && predicate(low));
        while low < high {
            let middle = low + (high - low + 1) / 2;
            if predicate(middle) {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        low
    }

    let (mut a, mut b, mut c, mut d) = (0i128, 1i128, 1i128, 1i128);
    // The loop always assigns these before returning: its first iteration
    // takes either the equality branch or a batched run.
    let (mut n, mut den) = (0i128, 1i128);
    let mut runs = 0usize;
    while b <= limit && d <= limit {
        runs += 1;
        let mediant = (a + c) as f64 / (b + d) as f64;
        if p1 == mediant {
            if b + d <= limit {
                n = a + c;
                den = b + d;
            } else if d > b {
                n = c;
                den = d;
            } else {
                n = a;
                den = b;
            }
            break;
        }
        if p1 > mediant {
            // Include the first move that crosses the limit. The scalar loop
            // performs it, records the opposite bound, then exits on its next
            // condition check.
            let max_steps = (limit - b) / d + 1;
            let steps = last_true(1, max_steps, |steps| {
                p1 > (a + steps * c) as f64 / (b + steps * d) as f64
            });
            a += steps * c;
            b += steps * d;
        } else {
            let max_steps = (limit - d) / b + 1;
            let steps = last_true(1, max_steps, |steps| {
                p1 < (c + steps * a) as f64 / (d + steps * b) as f64
            });
            c += steps * a;
            d += steps * b;
        }
        if b > limit {
            n = c;
            den = d;
        } else {
            n = a;
            den = b;
        }
    }
    ((n, den), runs)
}

fn signed_magnitude(magnitude: u128, negative: bool) -> Option<i128> {
    if negative {
        if magnitude == i128::MIN.unsigned_abs() {
            Some(i128::MIN)
        } else {
            i128::try_from(magnitude).ok()?.checked_neg()
        }
    } else {
        i128::try_from(magnitude).ok()
    }
}

fn fraction_from_magnitudes(
    mut numerator: u128,
    mut denominator: u128,
    negative: bool,
) -> Option<Fraction> {
    if denominator == 0 {
        return None;
    }
    // Parse into unsigned magnitudes first so an oversized textual pair that
    // reduces into the native range (for example 2^127/2^127) is accepted.
    let divisor = gcd_u128(numerator, denominator);
    numerator /= divisor;
    denominator /= divisor;
    if denominator > i128::MAX as u128 {
        return None;
    }
    checked_normalised(signed_magnitude(numerator, negative)?, denominator as i128)
}

/// Reduce arbitrary-precision intermediates before checking whether the
/// numerator and denominator fit in `i128`.
fn fraction_from_big_magnitudes(
    mut numerator: BigUint,
    mut denominator: BigUint,
    negative: bool,
) -> Option<Fraction> {
    if denominator == BigUint::from(0u8) {
        return None;
    }
    let divisor = gcd_biguint(numerator.clone(), denominator.clone());
    numerator /= &divisor;
    denominator /= divisor;
    fraction_from_magnitudes(numerator.to_u128()?, denominator.to_u128()?, negative)
}

fn fraction_from_bigints(numerator: BigInt, denominator: BigInt) -> Option<Fraction> {
    if denominator.sign() == Sign::NoSign {
        return None;
    }
    let negative = (numerator.sign() == Sign::Minus) != (denominator.sign() == Sign::Minus);
    fraction_from_big_magnitudes(
        numerator.magnitude().clone(),
        denominator.magnitude().clone(),
        negative,
    )
}

/// Compare two positive rational magnitudes without cross multiplication.
fn cmp_positive(mut an: u128, mut ad: u128, mut bn: u128, mut bd: u128) -> Ordering {
    let mut reversed = false;
    loop {
        let aq = an / ad;
        let bq = bn / bd;
        if aq != bq {
            let ordering = aq.cmp(&bq);
            return if reversed {
                ordering.reverse()
            } else {
                ordering
            };
        }
        let ar = an % ad;
        let br = bn % bd;
        match (ar == 0, br == 0) {
            (true, true) => return Ordering::Equal,
            (true, false) => {
                return if reversed {
                    Ordering::Greater
                } else {
                    Ordering::Less
                };
            }
            (false, true) => {
                return if reversed {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
            }
            (false, false) => {
                (an, ad, bn, bd) = (ad, ar, bd, br);
                reversed = !reversed;
            }
        }
    }
}

#[inline]
#[track_caller]
fn ck<T>(v: Option<T>, op: &str) -> T {
    match v {
        Some(v) => v,
        None => panic!(
            "rustel-fraction: i128 overflow in {op}. Upstream fraction.js is \
             arbitrary-precision; this is the documented divergence. If a real \
             pattern reached here, add the big-rational fallback - do not relax \
             the check."
        ),
    }
}

impl Fraction {
    pub const ZERO: Fraction = Fraction { n: 0, d: 1 };
    pub const ONE: Fraction = Fraction { n: 1, d: 1 };

    /// Construct from a numerator and denominator, normalising sign and gcd.
    #[track_caller]
    pub fn new(n: i128, d: i128) -> Self {
        assert!(d != 0, "rustel-fraction: zero denominator");
        ck(checked_normalised(n, d), "normalise")
    }

    /// Constructs a reduced fraction, returning `None` for a zero denominator
    /// or a value whose normal form cannot fit in signed `i128` components.
    /// Use this to validate external input without panicking.
    pub fn checked_new(n: i128, d: i128) -> Option<Self> {
        checked_normalised(n, d)
    }

    #[inline]
    pub const fn int(n: i128) -> Self {
        Fraction { n, d: 1 }
    }

    /// Both components as `i64` when each is below [`SMALL_LIMIT`].
    #[inline]
    fn small(self) -> Option<(i64, i64)> {
        (self.n > -SMALL_LIMIT && self.n < SMALL_LIMIT && self.d < SMALL_LIMIT)
            .then_some((self.n as i64, self.d as i64))
    }

    #[inline]
    pub const fn numer(&self) -> i128 {
        self.n
    }
    #[inline]
    pub const fn denom(&self) -> i128 {
        self.d
    }

    /// Exact reduced numerator. Callers sending numerator/denominator pairs
    /// as `f64` must range-check against 2^53 to preserve integer precision.
    #[inline]
    pub fn numerator(&self) -> i128 {
        self.n
    }

    /// Exact (positive) denominator in reduced form.
    pub fn denominator(&self) -> i128 {
        self.d
    }

    /// Lossy: for interop at the audio boundary only. Never for cycle maths.
    pub fn to_f64(&self) -> f64 {
        self.n as f64 / self.d as f64
    }

    /// Approximate a float using fraction.js@5.2.1's Farey / Stern-Brocot
    /// search, bounded by `N = 10_000_000`. For example, the float `1.0 / 3.0`
    /// becomes exactly `1/3`.
    ///
    /// Mediant comparisons use `f64` to preserve fraction.js's rounding and
    /// equality decisions; rational comparisons could choose a different result.
    #[track_caller]
    pub fn from_f64(value: f64) -> Option<Self> {
        if value.is_nan() || value.is_infinite() {
            return None;
        }
        let negative = value < 0.0;
        let mut p1 = if negative { -value } else { value };

        if p1 % 1.0 == 0.0 {
            // Float-to-integer casts saturate, so check the range before casting.
            // The negative limit is valid as i128::MIN; the positive limit is not.
            const I128_MAX_EXCLUSIVE: f64 = 170_141_183_460_469_231_731_687_303_715_884_105_728.0;
            if negative && p1 == I128_MAX_EXCLUSIVE {
                return Some(Fraction::int(i128::MIN));
            }
            if p1 >= I128_MAX_EXCLUSIVE {
                return None;
            }
            let n = p1 as i128;
            return Some(Fraction::int(if negative { -n } else { n }));
        }
        if p1 <= 0.0 {
            return Some(Fraction::ZERO);
        }

        let mut z: i128 = 1;
        const N: i128 = 10_000_000;

        if p1 >= 1.0 {
            let exponent = (1.0 + p1.log10()).floor();
            let z_f = 10f64.powf(exponent);
            if !z_f.is_finite() || exponent > 30.0 {
                return None;
            }
            z = 10i128.pow(exponent as u32);
            p1 /= z_f;
        }

        let (n, den) = farey_approximation(p1, N);

        if den == 0 {
            return None;
        }
        let n = ck(n.checked_mul(z), "from_f64 scale");
        Some(Fraction::new(if negative { -n } else { n }, den))
    }

    // -- arithmetic --------------------------------------------------------

    /// Exact addition, returning `None` if the reduced result cannot fit in
    /// `i128` components.
    pub fn checked_add(self, o: Self) -> Option<Self> {
        if let (Some((n, d)), Some((on, od))) = (self.small(), o.small()) {
            return Some(reduced_small(n * od + on * d, d * od));
        }
        let native = || {
            let divisor = gcd_u128(self.d as u128, o.d as u128) as i128;
            let left_scale = o.d / divisor;
            let right_scale = self.d / divisor;
            let numerator = self
                .n
                .checked_mul(left_scale)?
                .checked_add(o.n.checked_mul(right_scale)?)?;
            let denominator = self.d.checked_mul(left_scale)?;
            checked_normalised(numerator, denominator)
        };
        native().or_else(|| {
            fraction_from_bigints(
                BigInt::from(self.n) * BigInt::from(o.d) + BigInt::from(o.n) * BigInt::from(self.d),
                BigInt::from(self.d) * BigInt::from(o.d),
            )
        })
    }

    /// Checked exact subtraction; unlike `self.add(o.neg())`, this can inspect
    /// `i128::MIN` without first negating it.
    pub fn checked_sub(self, o: Self) -> Option<Self> {
        if let (Some((n, d)), Some((on, od))) = (self.small(), o.small()) {
            return Some(reduced_small(n * od - on * d, d * od));
        }
        let native = || {
            let divisor = gcd_u128(self.d as u128, o.d as u128) as i128;
            let left_scale = o.d / divisor;
            let right_scale = self.d / divisor;
            let numerator = self
                .n
                .checked_mul(left_scale)?
                .checked_sub(o.n.checked_mul(right_scale)?)?;
            let denominator = self.d.checked_mul(left_scale)?;
            checked_normalised(numerator, denominator)
        };
        native().or_else(|| {
            fraction_from_bigints(
                BigInt::from(self.n) * BigInt::from(o.d) - BigInt::from(o.n) * BigInt::from(self.d),
                BigInt::from(self.d) * BigInt::from(o.d),
            )
        })
    }

    /// Checked multiplication. The `i128` path cancels across the operands
    /// before either product.
    pub fn checked_mul(self, o: Self) -> Option<Self> {
        if let (Some((n, d)), Some((on, od))) = (self.small(), o.small()) {
            return Some(reduced_small(n * on, d * od));
        }
        let left_cancel = gcd_u128(self.n.unsigned_abs(), o.d as u128);
        let right_cancel = gcd_u128(o.n.unsigned_abs(), self.d as u128);
        let left_n = div_signed_by_u128(self.n, left_cancel)?;
        let right_n = div_signed_by_u128(o.n, right_cancel)?;
        let left_d = self.d / i128::try_from(right_cancel).ok()?;
        let right_d = o.d / i128::try_from(left_cancel).ok()?;
        checked_normalised(left_n.checked_mul(right_n)?, left_d.checked_mul(right_d)?)
    }

    /// Checked division, with cross-cancellation on the `i128` path. `None`
    /// includes division by zero and an exact result outside the native
    /// `i128/i128` representation.
    pub fn checked_div(self, o: Self) -> Option<Self> {
        if o.n == 0 {
            return None;
        }
        if self.n == 0 {
            return Some(Fraction::ZERO);
        }
        if let (Some((n, d)), Some((on, od))) = (self.small(), o.small()) {
            // The divisor's sign moves to the numerator.
            let (numerator, denominator) = (n * od, d * on);
            return Some(if denominator < 0 {
                reduced_small(-numerator, -denominator)
            } else {
                reduced_small(numerator, denominator)
            });
        }
        let numerator_cancel = gcd_u128(self.n.unsigned_abs(), o.n.unsigned_abs());
        let denominator_cancel = gcd_u128(self.d as u128, o.d as u128);
        let left_n = div_signed_by_u128(self.n, numerator_cancel)?;
        let right_n = div_signed_by_u128(o.n, numerator_cancel)?;
        let left_d = self.d / i128::try_from(denominator_cancel).ok()?;
        let right_d = o.d / i128::try_from(denominator_cancel).ok()?;
        checked_normalised(left_n.checked_mul(right_d)?, left_d.checked_mul(right_n)?)
    }

    /// Checked `lcm(a/b, c/d) = lcm(|a|,|c|) / gcd(b,d)`.
    ///
    /// Returns `None` when the non-negative numerator cannot fit in `i128`.
    pub fn checked_lcm(self, o: Self) -> Option<Self> {
        if self.n == 0 || o.n == 0 {
            return Some(Fraction::ZERO);
        }
        let left = self.n.unsigned_abs();
        let right = o.n.unsigned_abs();
        let divisor = gcd_u128(left, right);
        let numerator = (left / divisor).checked_mul(right)?;
        let numerator = i128::try_from(numerator).ok()?;
        let denominator = gcd_u128(self.d as u128, o.d as u128);
        let denominator = i128::try_from(denominator).ok()?;
        checked_normalised(numerator, denominator)
    }

    /// Checked negation. The sole unrepresentable case is `i128::MIN`.
    pub fn checked_neg(self) -> Option<Self> {
        Some(Fraction {
            n: self.n.checked_neg()?,
            d: self.d,
        })
    }

    /// Checked truncated modulo, reducing arbitrary-precision intermediates
    /// before deciding whether the exact result fits this native type.
    pub fn checked_rem(self, o: Self) -> Option<Self> {
        if o.n == 0 {
            return None;
        }
        let native = || {
            let left = self.n.checked_mul(o.d)?;
            let right = o.n.checked_mul(self.d)?;
            let denominator = self.d.checked_mul(o.d)?;
            checked_normalised(left.checked_rem(right)?, denominator)
        };
        native().or_else(|| {
            let left = BigInt::from(self.n) * BigInt::from(o.d);
            let right = BigInt::from(o.n) * BigInt::from(self.d);
            fraction_from_bigints(left % right, BigInt::from(self.d) * BigInt::from(o.d))
        })
    }

    /// Exact overflow-free comparison for guarded construction code.
    pub fn checked_cmp(self, o: Self) -> Option<Ordering> {
        let sign = self.n.signum().cmp(&o.n.signum());
        if sign != Ordering::Equal {
            return Some(sign);
        }
        if self.n == 0 {
            return Some(Ordering::Equal);
        }
        let magnitude = cmp_positive(
            self.n.unsigned_abs(),
            self.d as u128,
            o.n.unsigned_abs(),
            o.d as u128,
        );
        Some(if self.n < 0 {
            magnitude.reverse()
        } else {
            magnitude
        })
    }

    #[track_caller]
    pub fn add(self, o: Self) -> Self {
        ck(self.checked_add(o), "add")
    }

    #[track_caller]
    pub fn sub(self, o: Self) -> Self {
        ck(self.checked_sub(o), "sub")
    }

    #[track_caller]
    pub fn mul(self, o: Self) -> Self {
        ck(self.checked_mul(o), "mul")
    }

    #[track_caller]
    pub fn div(self, o: Self) -> Self {
        assert!(o.n != 0, "rustel-fraction: division by zero");
        ck(self.checked_div(o), "div")
    }

    /// Checked because `-i128::MIN` is not representable.
    #[inline]
    #[track_caller]
    pub fn neg(self) -> Self {
        Fraction {
            n: ck(self.n.checked_neg(), "neg"),
            d: self.d,
        }
    }

    /// Truncated modulo, taking the sign of the dividend - matches fraction.js:
    /// `(-7/3) % (1/2) == -1/3`.
    #[track_caller]
    pub fn rem(self, o: Self) -> Self {
        assert!(o.n != 0, "rustel-fraction: modulo by zero");
        ck(self.checked_rem(o), "rem")
    }

    // -- rounding ----------------------------------------------------------

    /// Rounds toward negative infinity: `(-1/4).floor() == -1`, so negative
    /// times map to the start of their containing cycle.
    pub fn floor(self) -> Self {
        if let Some((n, d)) = self.small() {
            return Fraction {
                n: i128::from(n.div_euclid(d)),
                d: 1,
            };
        }
        let q = self.n.div_euclid(self.d);
        Fraction { n: q, d: 1 }
    }

    pub fn ceil(self) -> Self {
        let f = self.floor();
        if f.mul_eq_self(self) {
            f
        } else {
            Fraction { n: f.n + 1, d: 1 }
        }
    }

    #[inline]
    fn mul_eq_self(self, o: Self) -> bool {
        self.n == o.n && self.d == o.d
    }

    // -- cycle vocabulary ---------------------------------------------------

    /// Start of the cycle containing this time.
    #[inline]
    pub fn sam(self) -> Self {
        self.floor()
    }

    /// Start of the next cycle.
    #[inline]
    pub fn next_sam(self) -> Self {
        self.sam().add(Fraction::ONE)
    }

    /// Position relative to the start of this time's cycle.
    #[inline]
    pub fn cycle_pos(self) -> Self {
        self.sub(self.sam())
    }

    #[inline]
    pub fn min(self, o: Self) -> Self {
        if self < o { self } else { o }
    }

    #[inline]
    pub fn max(self, o: Self) -> Self {
        if self > o { self } else { o }
    }

    /// `gcd(a/b, c/d) = gcd(a,c) / lcm(b,d)`
    #[track_caller]
    pub fn gcd(self, o: Self) -> Self {
        let n = gcd_i128(self.n, o.n);
        let g = gcd_i128(self.d, o.d);
        let l = ck(self.d.checked_mul(o.d / g), "gcd");
        Self::new(n, l)
    }

    /// `lcm(a/b, c/d) = lcm(|a|,|c|) / gcd(b,d)`
    ///
    /// The result is non-negative, as in fraction.js: `lcm(-1/2, 1/3) == 1/1`.
    #[track_caller]
    pub fn lcm(self, o: Self) -> Self {
        if self.n == 0 || o.n == 0 {
            return Fraction::ZERO;
        }
        // Unsigned for the same reason as `gcd_i128`: `i128::MIN.abs()`
        // overflows, and a numerator of `i128::MIN` is representable.
        let (an, bn) = (self.n.unsigned_abs(), o.n.unsigned_abs());
        let g = gcd_u128(an, bn);
        let n = ck(
            an.checked_mul(bn / g).and_then(|v| i128::try_from(v).ok()),
            "lcm",
        );
        Self::new(n, gcd_i128(self.d, o.d))
    }

    /// Returns `o` if self is zero.
    #[inline]
    pub fn or(self, o: Self) -> Self {
        if self.n == 0 { o } else { self }
    }

    /// Snapshot serialisation. Always `n/d`, even when `d == 1`.
    pub fn show(&self) -> String {
        format!("{}/{}", self.n, self.d)
    }
}

impl std::ops::Add for Fraction {
    type Output = Fraction;
    fn add(self, o: Self) -> Self {
        Fraction::add(self, o)
    }
}
impl std::ops::Sub for Fraction {
    type Output = Fraction;
    fn sub(self, o: Self) -> Self {
        Fraction::sub(self, o)
    }
}
impl std::ops::Mul for Fraction {
    type Output = Fraction;
    fn mul(self, o: Self) -> Self {
        Fraction::mul(self, o)
    }
}
impl std::ops::Div for Fraction {
    type Output = Fraction;
    fn div(self, o: Self) -> Self {
        Fraction::div(self, o)
    }
}
impl std::ops::Rem for Fraction {
    type Output = Fraction;
    fn rem(self, o: Self) -> Self {
        Fraction::rem(self, o)
    }
}
impl std::ops::Neg for Fraction {
    type Output = Fraction;
    fn neg(self) -> Self {
        Fraction::neg(self)
    }
}

impl PartialOrd for Fraction {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for Fraction {
    fn cmp(&self, o: &Self) -> Ordering {
        if let (Some((n, d)), Some((on, od))) = (self.small(), o.small()) {
            return (n * od).cmp(&(on * d));
        }
        // Cross-multiply on the `i128` path; both denominators are positive
        // by normalisation. Fall back to the exact continued-fraction compare
        // when either product is outside i128.
        match (self.n.checked_mul(o.d), o.n.checked_mul(self.d)) {
            (Some(left), Some(right)) => left.cmp(&right),
            _ => self
                .checked_cmp(*o)
                .expect("checked_cmp is total for normalised fractions"),
        }
    }
}

/// A string is not in the supported `fraction.js` numeric grammar, or its
/// exact value is outside this crate's documented `i128/i128` range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseFractionError;

/// Maximum input length in bytes for [`Fraction::from_str`]. This bounds the
/// work of constructing and reducing arbitrary-precision intermediates.
pub const MAX_FRACTION_SOURCE_BYTES: usize = 1024;

impl fmt::Display for ParseFractionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid or out-of-range fraction")
    }
}

impl std::error::Error for ParseFractionError {}

/// The tokeniser fraction.js applies to a string: `/\d+|./g` after removing
/// underscores. `.` never matches a LineTerminator, so those characters vanish
/// (`"1\n"` is `1`); every other non-digit becomes its own token.
fn tokenise_like_fraction_js(source: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut digits_start = None;
    for (index, ch) in source.char_indices() {
        if ch.is_ascii_digit() {
            digits_start.get_or_insert(index);
            continue;
        }
        if let Some(start) = digits_start.take() {
            tokens.push(&source[start..index]);
        }
        if !matches!(ch, '\n' | '\r' | '\u{2028}' | '\u{2029}') {
            tokens.push(&source[index..index + ch.len_utf8()]);
        }
    }
    if let Some(start) = digits_start {
        tokens.push(&source[start..]);
    }
    tokens
}

/// fraction.js's `assign(token, sign)` is `BigInt(token) * sign`, and `BigInt`
/// trims StrWhiteSpaceChar before reading an empty remainder as zero. A lone
/// whitespace token in an integer slot is therefore zero - `" "`, `"- "` and
/// `". "` are `0/1`, `" .5"` is `1/2` - while any other non-digit token, or a
/// missing one, throws.
fn bigint_like_fraction_js(token: Option<&str>) -> Option<BigInt> {
    let token = token?;
    if token.bytes().all(|byte| byte.is_ascii_digit()) {
        return BigInt::parse_bytes(token.as_bytes(), 10);
    }
    let mut chars = token.chars();
    let whitespace = chars.next().is_some_and(|ch| {
        matches!(
            ch,
            '\t' | '\u{0B}' | '\u{0C}' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
                ..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
        )
    });
    (whitespace && chars.next().is_none()).then(BigInt::zero)
}

impl FromStr for Fraction {
    type Err = ParseFractionError;

    /// Parse the `fraction.js` string forms: integers, decimals, `n/d` or
    /// `n:d`, mixed fractions, and repeating decimals written with
    /// parentheses or apostrophes.
    ///
    /// As in fraction.js, underscores and line terminators vanish, a lone
    /// whitespace token reads as zero (see [`bigint_like_fraction_js`]), and
    /// whitespace beside digits (`" 1/2"`, `"1 "`) is invalid.
    fn from_str(source: &str) -> Result<Self, Self::Err> {
        if source.len() > MAX_FRACTION_SOURCE_BYTES {
            return Err(ParseFractionError);
        }
        let compact = source.replace('_', "");
        let tokens = tokenise_like_fraction_js(&compact);
        if tokens.is_empty() {
            return Err(ParseFractionError);
        }
        let token = |index: usize| tokens.get(index).copied();
        let is = |index: usize, expected: &str| token(index) == Some(expected);
        let assign = |index: usize, sign: &BigInt| {
            bigint_like_fraction_js(token(index))
                .map(|value| value * sign)
                .ok_or(ParseFractionError)
        };
        // Digit counts are in UTF-16 units; every token that reaches here is
        // a digit run or one BMP whitespace character, so chars() agrees.
        let scale = |index: usize| -> Result<BigInt, ParseFractionError> {
            let digits = token(index).map_or(0, |token| token.chars().count());
            let exponent = u32::try_from(digits).map_err(|_| ParseFractionError)?;
            Ok(BigInt::from(10u8).pow(exponent))
        };

        let one = BigInt::one();
        let (mut v, mut w, mut x) = (BigInt::zero(), BigInt::zero(), BigInt::zero());
        let (mut y, mut z) = (one.clone(), one.clone());
        let mut sign = one.clone();
        let mut ndx = 0;
        if is(0, "-") {
            sign = -one.clone();
            ndx += 1;
        } else if is(0, "+") {
            ndx += 1;
        }

        if tokens.len() == ndx + 1 {
            // A simple number, "1234".
            w = assign(ndx, &sign)?;
            ndx += 1;
        } else if is(ndx + 1, ".") || is(ndx, ".") {
            // A decimal, "0.5" or ".5", with optional repeating places.
            if !is(ndx, ".") {
                v = assign(ndx, &sign)?;
                ndx += 1;
            }
            ndx += 1;
            if ndx + 1 == tokens.len()
                || (is(ndx + 1, "(") && is(ndx + 3, ")"))
                || (is(ndx + 1, "'") && is(ndx + 3, "'"))
            {
                w = assign(ndx, &sign)?;
                y = scale(ndx)?;
                ndx += 1;
            }
            if (is(ndx, "(") && is(ndx + 2, ")")) || (is(ndx, "'") && is(ndx + 2, "'")) {
                x = assign(ndx + 1, &sign)?;
                z = scale(ndx + 1)? - &one;
                ndx += 3;
            }
        } else if is(ndx + 1, "/") || is(ndx + 1, ":") {
            // A simple fraction, "123/456" or "123:456".
            w = assign(ndx, &sign)?;
            y = assign(ndx + 2, &one)?;
            ndx += 3;
        } else if is(ndx + 3, "/") && is(ndx + 1, " ") {
            // A mixed fraction, "123 1/2".
            v = assign(ndx, &sign)?;
            w = assign(ndx + 2, &sign)?;
            y = assign(ndx + 4, &one)?;
            ndx += 5;
        }
        if tokens.len() > ndx {
            return Err(ParseFractionError);
        }

        let d = &y * &z;
        let n = x + &d * v + z * w;
        fraction_from_bigints(n, d).ok_or(ParseFractionError)
    }
}

impl fmt::Display for Fraction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.show())
    }
}

impl fmt::Debug for Fraction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fraction({})", self.show())
    }
}

impl From<i128> for Fraction {
    fn from(n: i128) -> Self {
        Fraction::int(n)
    }
}
impl From<i64> for Fraction {
    fn from(n: i64) -> Self {
        Fraction::int(n as i128)
    }
}
impl From<i32> for Fraction {
    fn from(n: i32) -> Self {
        Fraction::int(n as i128)
    }
}

/// Variadic gcd. Empty input is `None`.
pub fn gcd_all(fs: &[Fraction]) -> Option<Fraction> {
    if fs.is_empty() {
        return None;
    }
    Some(fs.iter().fold(Fraction::ONE, |acc, f| acc.gcd(*f)))
}

/// Variadic lcm. Empty input is `None`.
pub fn lcm_all(fs: &[Fraction]) -> Option<Fraction> {
    let (last, rest) = fs.split_last()?;
    Some(rest.iter().fold(*last, |acc, f| acc.lcm(*f)))
}

#[cfg(test)]
mod from_f64_tests {
    use super::{
        Fraction, MAX_FRACTION_SOURCE_BYTES, ParseFractionError, farey_approximation,
        farey_approximation_with_runs, gcd_i128,
    };

    fn scalar_farey_approximation(p1: f64, limit: i128) -> (i128, i128) {
        let (mut a, mut b, mut c, mut d) = (0i128, 1i128, 1i128, 1i128);
        let (mut n, mut den) = (0i128, 1i128);
        while b <= limit && d <= limit {
            let mediant = (a + c) as f64 / (b + d) as f64;
            if p1 == mediant {
                if b + d <= limit {
                    n = a + c;
                    den = b + d;
                } else if d > b {
                    n = c;
                    den = d;
                } else {
                    n = a;
                    den = b;
                }
                break;
            }
            if p1 > mediant {
                a += c;
                b += d;
            } else {
                c += a;
                d += b;
            }
            if b > limit {
                n = c;
                den = d;
            } else {
                n = a;
                den = b;
            }
        }
        (n, den)
    }

    #[test]
    fn batched_farey_walk_matches_the_scalar_state_machine() {
        const LIMIT: i128 = 1_024;
        let edge_cases = [
            f64::MIN_POSITIVE,
            f64::from_bits(1),
            0.000_000_1,
            0.001,
            0.1,
            1.0 / 3.0,
            0.499_999_999_999_999_94,
            0.5,
            0.500_000_000_000_000_1,
            0.999_999_999_999_999_9,
        ];
        for value in edge_cases {
            assert_eq!(
                farey_approximation(value, LIMIT),
                scalar_farey_approximation(value, LIMIT),
                "edge case {value:.17}"
            );
        }

        // Fixed-seed binary64 samples exercise direction changes and rounded
        // equality without making the test probabilistic.
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        for index in 0..4_096 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let value = ((state >> 11) as f64 + 0.5) / ((1_u64 << 53) as f64);
            assert_eq!(
                farey_approximation(value, LIMIT),
                scalar_farey_approximation(value, LIMIT),
                "sample {index}: {value:.17}"
            );
        }
    }

    #[test]
    fn near_zero_random_offsets_take_bounded_farey_runs() {
        const FRACTION_JS_LIMIT: i128 = 10_000_000;
        for value in [
            0.000_000_123_456_789,
            0.000_314_159_265_358_979_3,
            0.000_999_999_999_999_999_8,
        ] {
            let (_, runs) = farey_approximation_with_runs(value, FRACTION_JS_LIMIT);
            assert!(
                runs < 128,
                "{value:.17} took {runs} directional runs; the batched search regressed"
            );
        }
    }

    // Reject out-of-range floats before a saturated factor can trigger excessive
    // allocation. This test checks conversion only.
    #[test]
    fn out_of_range_floats_are_refused_not_saturated() {
        for value in [1e300_f64, -1e300, f64::MAX, 1e39] {
            assert_eq!(
                Fraction::from_f64(value),
                None,
                "{value:e} does not fit i128 and must be refused, not saturated"
            );
        }
    }

    #[test]
    fn in_range_integers_still_convert() {
        assert_eq!(Fraction::from_f64(2.0), Some(Fraction::int(2)));
        assert_eq!(Fraction::from_f64(-3.0), Some(Fraction::int(-3)));
        assert_eq!(Fraction::from_f64(0.0), Some(Fraction::ZERO));
        assert_eq!(
            Fraction::from_f64(1e18),
            Some(Fraction::int(1_000_000_000_000_000_000))
        );
        assert_eq!(
            Fraction::from_f64(i128::MIN as f64),
            Some(Fraction::int(i128::MIN))
        );
        assert_eq!(Fraction::from_f64(-(i128::MIN as f64)), None);
    }

    #[test]
    fn non_integers_take_the_farey_path() {
        // `1/3` exercises binary-to-rational reconstruction.
        assert_eq!(Fraction::from_f64(1.0 / 3.0), Some(Fraction::new(1, 3)));
        assert_eq!(Fraction::from_f64(0.4), Some(Fraction::new(2, 5)));
        assert_eq!(Fraction::from_f64(-1.0 / 3.0), Some(Fraction::new(-1, 3)));
        assert_eq!(Fraction::from_f64(-0.1), Some(Fraction::new(-1, 10)));
    }

    #[test]
    fn nan_and_infinity_are_refused() {
        assert_eq!(Fraction::from_f64(f64::NAN), None);
        assert_eq!(Fraction::from_f64(f64::INFINITY), None);
        assert_eq!(Fraction::from_f64(f64::NEG_INFINITY), None);
    }

    #[test]
    fn checked_arithmetic_cross_cancels_and_never_negates_min() {
        let max = i128::MAX;
        assert_eq!(
            Fraction::new(max, 2).checked_mul(Fraction::new(2, max)),
            Some(Fraction::ONE)
        );
        assert_eq!(
            Fraction::int(i128::MIN).checked_div(Fraction::int(i128::MIN)),
            Some(Fraction::ONE)
        );
        assert_eq!(
            Fraction::ZERO.checked_div(Fraction::int(i128::MIN)),
            Some(Fraction::ZERO)
        );
        assert_eq!(Fraction::int(i128::MIN).checked_neg(), None);
        assert_eq!(
            Fraction::int(i128::MIN).checked_cmp(Fraction::int(max)),
            Some(std::cmp::Ordering::Less)
        );
        assert_eq!(
            Fraction::new(max, 2).checked_add(Fraction::new(-max, 3)),
            Some(Fraction::new(max, 6))
        );
        assert_eq!(
            Fraction::new(max, 2).checked_sub(Fraction::new(max, 3)),
            Some(Fraction::new(max, 6))
        );
    }

    #[test]
    fn ordinary_operators_pre_cancel_representable_results() {
        let max = i128::MAX;
        assert_eq!(
            Fraction::new(1, max).add(Fraction::new(1, max)),
            Fraction::new(2, max)
        );
        assert_eq!(
            Fraction::new(max, 2).mul(Fraction::new(2, max)),
            Fraction::ONE
        );
        assert_eq!(
            Fraction::new(max, 2).div(Fraction::new(max, 2)),
            Fraction::ONE
        );
    }

    #[test]
    fn ordering_is_total_when_cross_products_do_not_fit_i128() {
        let larger = Fraction::new(i128::MAX, i128::MAX - 1);
        let smaller = Fraction::new(i128::MAX - 1, i128::MAX);
        assert!(larger > smaller);
        assert!(smaller < larger);

        let mut values = [larger, Fraction::ZERO, smaller, Fraction::int(i128::MIN)];
        values.sort();
        assert_eq!(
            values,
            [Fraction::int(i128::MIN), Fraction::ZERO, smaller, larger]
        );
    }

    #[test]
    fn display_round_trips_and_fraction_js_string_forms_parse() {
        let values = [
            Fraction::ZERO,
            Fraction::ONE,
            Fraction::new(-3, 4),
            Fraction::int(i128::MIN),
            Fraction::int(i128::MAX),
        ];
        for value in values {
            assert_eq!(value.to_string().parse::<Fraction>(), Ok(value));
        }

        assert_eq!("3/4".parse(), Ok(Fraction::new(3, 4)));
        assert_eq!("+3:4".parse(), Ok(Fraction::new(3, 4)));
        assert_eq!("3 1/2".parse(), Ok(Fraction::new(7, 2)));
        assert_eq!("-.5".parse(), Ok(Fraction::new(-1, 2)));
        assert_eq!("1.2(3)".parse(), Ok(Fraction::new(37, 30)));
        assert_eq!("0.'3'".parse(), Ok(Fraction::new(1, 3)));
        assert_eq!("1_000/2".parse(), Ok(Fraction::int(500)));
        assert_eq!("+".parse(), Ok(Fraction::ZERO));
        assert_eq!("-".parse(), Ok(Fraction::ZERO));
        assert_eq!(
            format!("{}.0", i128::MAX).parse(),
            Ok(Fraction::int(i128::MAX))
        );
        assert_eq!(
            format!("{}.0", i128::MIN).parse(),
            Ok(Fraction::int(i128::MIN))
        );
        assert_eq!(
            "340282366920938463463374607431768211456/\
             340282366920938463463374607431768211456"
                .parse(),
            Ok(Fraction::ONE)
        );
        assert_eq!(
            format!("{} 0/3", i128::MAX).parse(),
            Ok(Fraction::int(i128::MAX))
        );
        assert_eq!(format!("0.{}", "0".repeat(100)).parse(), Ok(Fraction::ZERO));
        assert_eq!(
            format!("{0}/{0}", i128::MIN.unsigned_abs()).parse(),
            Ok(Fraction::ONE)
        );
        assert!(" 3/4".parse::<Fraction>().is_err());
        assert!("3/0".parse::<Fraction>().is_err());
    }

    #[test]
    fn oversized_fraction_text_is_rejected_before_big_integer_work() {
        let oversized = "9".repeat(MAX_FRACTION_SOURCE_BYTES + 1);
        assert_eq!(oversized.parse::<Fraction>(), Err(ParseFractionError));
    }

    #[test]
    fn remainder_gcd_lcm_and_display_contracts_are_pinned() {
        assert_eq!(
            Fraction::new(-7, 3).rem(Fraction::new(1, 2)),
            Fraction::new(-1, 3)
        );
        assert_eq!(
            Fraction::new(2, 3).gcd(Fraction::new(4, 9)),
            Fraction::new(2, 9)
        );
        assert_eq!(
            Fraction::new(2, 3).lcm(Fraction::new(4, 9)),
            Fraction::new(4, 3)
        );
        assert_eq!(Fraction::new(-3, 1).to_string(), "-3/1");

        let max_half = Fraction::new(i128::MAX, 2);
        assert_eq!(max_half.checked_rem(max_half), Some(Fraction::ZERO));
        assert_eq!(max_half.rem(max_half), Fraction::ZERO);
        assert_eq!(
            Fraction::int(i128::MIN).checked_rem(Fraction::int(-1)),
            Some(Fraction::ZERO)
        );
        assert_eq!(
            Fraction::int(i128::MIN).rem(Fraction::int(-1)),
            Fraction::ZERO
        );
    }

    #[test]
    fn checked_arithmetic_refuses_unrepresentable_results() {
        assert_eq!(Fraction::int(i128::MAX).checked_add(Fraction::ONE), None);
        assert_eq!(Fraction::int(i128::MIN).checked_sub(Fraction::ONE), None);
        assert_eq!(Fraction::ONE.checked_div(Fraction::ZERO), None);
    }

    #[test]
    fn checked_lcm_matches_fraction_js_sign_zero_and_fraction_rules() {
        assert_eq!(
            Fraction::new(2, 3).checked_lcm(Fraction::new(3, 4)),
            Some(Fraction::int(6))
        );
        assert_eq!(
            Fraction::new(-1, 2).checked_lcm(Fraction::new(1, 3)),
            Some(Fraction::ONE)
        );
        assert_eq!(
            Fraction::ZERO.checked_lcm(Fraction::int(i128::MIN)),
            Some(Fraction::ZERO)
        );
    }

    #[test]
    fn checked_lcm_refuses_unrepresentable_positive_results_without_panicking() {
        let max = Fraction::int(i128::MAX);
        assert_eq!(max.checked_lcm(max), Some(max));
        assert_eq!(max.checked_lcm(Fraction::int(2)), None);
        assert_eq!(Fraction::int(i128::MIN).checked_lcm(Fraction::ONE), None);
    }

    #[test]
    fn negating_the_extreme_numerator_does_not_wrap_back_to_itself() {
        let extreme = Fraction::new(i128::MIN, 1);
        let payload = std::panic::catch_unwind(|| extreme.neg())
            .expect_err("negating i128::MIN must fail loudly");
        let message = payload
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default();
        assert!(
            message.contains("i128 overflow in neg"),
            "the refusal did not name the overflow: {message}"
        );
    }

    #[test]
    fn magnitude_helpers_survive_the_extreme_numerator() {
        assert_eq!(gcd_i128(i128::MIN, 2), 2);
        assert_eq!(gcd_i128(2, i128::MIN), 2);
        assert_eq!(gcd_i128(0, 0), 0);

        // The one case whose answer genuinely does not fit: gcd is 2^127,
        // one past `i128::MAX`. It must refuse rather than wrap.
        let refused = std::panic::catch_unwind(|| gcd_i128(i128::MIN, i128::MIN))
            .expect_err("a gcd of 2^127 cannot be an i128");
        let message = refused
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default();
        assert!(
            message.contains("i128 overflow in gcd"),
            "the refusal did not name the overflow: {message}"
        );
    }

    #[test]
    fn negation_still_round_trips_for_ordinary_values() {
        for (n, d) in [(3, 4), (-3, 4), (1, 1), (0, 1), (i128::MAX, 1)] {
            let value = Fraction::new(n, d);
            assert_eq!(value.neg().neg(), value);
            assert_eq!(value.neg().add(value), Fraction::ZERO);
        }
    }

    #[test]
    fn extreme_values_normalise_and_subtract_when_the_result_is_representable() {
        let extreme = Fraction::int(i128::MIN);
        assert_eq!(Fraction::new(i128::MIN, i128::MIN), Fraction::ONE);
        assert_eq!(Fraction::new(i128::MIN, 2), Fraction::int(i128::MIN / 2));
        assert_eq!(extreme.sub(extreme), Fraction::ZERO);
        assert_eq!(extreme - extreme, Fraction::ZERO);
    }
}

#[cfg(test)]
mod checked_new_tests {
    use crate::Fraction;

    #[test]
    fn checked_new_agrees_with_new_where_new_is_total() {
        for (n, d) in [
            (1, -6),
            (-3, 9),
            (0, 7),
            (i128::MIN, 1),
            (i128::MAX, -1),
            (170141183460469231731687303715884105727, 2),
        ] {
            assert_eq!(Fraction::checked_new(n, d), Some(Fraction::new(n, d)));
        }
    }

    #[test]
    fn checked_new_refuses_what_new_would_panic_on() {
        assert_eq!(Fraction::checked_new(1, 0), None);
        // A 2^127 denominator an odd numerator cannot shrink below i128::MAX.
        assert_eq!(Fraction::checked_new(1, i128::MIN), None);
        // A 2^127 numerator that must come out POSITIVE has no signed form.
        assert_eq!(Fraction::checked_new(i128::MIN, -3), None);
        // Zero always normalises, whatever the denominator.
        assert_eq!(Fraction::checked_new(0, i128::MIN), Some(Fraction::ZERO));
        // MIN / -1 is +2^127, which no signed i128 holds.
        assert_eq!(Fraction::checked_new(i128::MIN, -1), None);
        // A 2^127 numerator that stays NEGATIVE is exactly i128::MIN.
        assert_eq!(
            Fraction::checked_new(i128::MIN, 1),
            Some(Fraction::int(i128::MIN))
        );
    }
}

#[cfg(test)]
mod cmp_overflow_tests {
    use super::*;

    /// The exact fallback must agree with cross-multiplication wherever
    /// cross-multiplication is available to be right.
    #[test]
    fn the_fallback_agrees_with_cross_multiplication() {
        let values: Vec<(i128, i128)> = vec![
            (0, 1),
            (1, 1),
            (-1, 1),
            (1, 3),
            (2, 6),
            (-2, 3),
            (7, 5),
            (-7, 5),
            (355, 113),
            (22, 7),
            (1, 1_000_000),
            (999_999, 1_000_000),
        ];
        for (an, ad) in &values {
            for (cn, cd) in &values {
                let expected = (an * cd).cmp(&(cn * ad));
                let got = Fraction::new(*an, *ad).cmp(&Fraction::new(*cn, *cd));
                assert_eq!(got, expected, "{an}/{ad} vs {cn}/{cd}");
            }
        }
    }

    /// The case that panicked: cross-multiplication overflows i128, and the
    /// comparison still has to answer.
    #[test]
    fn huge_fractions_compare_instead_of_panicking() {
        let big = i128::MAX / 3;
        let a = Fraction::new(big, big - 1);
        let b = Fraction::new(big - 2, big - 1);
        assert!(
            a.n.checked_mul(b.d).is_none() || b.n.checked_mul(a.d).is_none(),
            "the test inputs must actually stress the multiply"
        );
        assert_eq!(a.cmp(&b), Ordering::Greater);
        assert_eq!(b.cmp(&a), Ordering::Less);
        assert_eq!(a.cmp(&a), Ordering::Equal);
    }

    /// Two fractions that differ by the smallest possible amount must not
    /// compare equal: an approximate fallback would get this wrong.
    #[test]
    fn the_fallback_is_exact_not_approximate() {
        let big = i128::MAX / 5;
        let a = Fraction::new(big, big + 1);
        let b = Fraction::new(big - 1, big + 1);
        assert_eq!(a.cmp(&b), Ordering::Greater);
        // As f64 these are indistinguishable, which is why f64 was not used.
        assert_eq!(
            (a.n as f64 / a.d as f64).partial_cmp(&(b.n as f64 / b.d as f64)),
            Some(Ordering::Equal),
            "if f64 ever separates these the test has lost its point"
        );
    }

    #[test]
    fn negative_numerators_compare_correctly_through_the_fallback() {
        let big = i128::MAX / 3;
        let a = Fraction::new(-big, big - 1);
        let b = Fraction::new(big - 2, big - 1);
        assert_eq!(a.cmp(&b), Ordering::Less);
        assert_eq!(b.cmp(&a), Ordering::Greater);
        let c = Fraction::new(-big, big - 1);
        assert_eq!(a.cmp(&c), Ordering::Equal);
    }
}

#[cfg(test)]
mod parse_tokeniser_tests {
    use super::*;

    /// Each expectation here was taken from fraction.js 5.2.1 itself.
    #[test]
    fn whitespace_tokens_follow_bigint_coercion() {
        for text in [
            " ", "\t", "\u{A0}", "\u{FEFF}", "\u{3000}", "- ", "+ ", "_ ", ". ", "-_",
        ] {
            assert_eq!(text.parse(), Ok(Fraction::ZERO), "{text:?}");
        }
        assert_eq!(" .5".parse(), Ok(Fraction::new(1, 2)));
        assert_eq!("1\n".parse(), Ok(Fraction::ONE));
        assert_eq!("1\r\n/\u{2028}3".parse(), Ok(Fraction::new(1, 3)));
        for text in [
            "", "\n", "  ", " -", " 1", "1 ", " 1/2", "1/ 2", "1  1/2", "\u{85}", "\u{200B}", "١",
            "１", "1e3", "..5", "1.(", "1.2(3", "1/2/3",
        ] {
            assert_eq!(
                text.parse::<Fraction>(),
                Err(ParseFractionError),
                "{text:?}"
            );
        }
        // A whitespace token in a denominator slot reads as zero: division by
        // zero, so the parse is refused.
        for text in ["1/ ", " / ", "1/0", "0/0"] {
            assert_eq!(
                text.parse::<Fraction>(),
                Err(ParseFractionError),
                "{text:?}"
            );
        }
    }
}
