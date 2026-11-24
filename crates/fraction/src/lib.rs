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
use std::collections::HashMap;
use std::fmt;
use std::num::{NonZeroU16, NonZeroU32, NonZeroU64};
use std::str::FromStr;
use std::sync::{Mutex, OnceLock};

use num_bigint::{BigInt, BigUint, Sign};
use num_traits::{One, ToPrimitive, Zero};

/// High bit of `Fraction`'s packed `NonZeroU64` marks an interned wide
/// `(i128, i128)` entry.
const WIDE_FLAG: u64 = 1u64 << 63;
/// Low 24 bits hold the positive denominator (`1..=16_777_215`), covering all
/// `from_f64` denominators (`<= 10_000_000`) and musical grids inline.
const DEN_BITS: u32 = 24;
const DEN_MASK: u64 = (1u64 << DEN_BITS) - 1;
/// Signed 39-bit numerator bounds (`-2^38 ..= 2^38 - 1`).
const INLINE_NUM_MIN: i64 = -(1i64 << 38);
const INLINE_NUM_MAX: i64 = (1i64 << 38) - 1;

const WIDE_SLAB_SHIFT: usize = 8;
const WIDE_SLAB_LEN: usize = 1 << WIDE_SLAB_SHIFT;
const WIDE_MAX_SLABS: usize = 4096;

struct WideSlab {
    slots: [OnceLock<(i128, i128)>; WIDE_SLAB_LEN],
}

impl WideSlab {
    fn new() -> Box<Self> {
        Box::new(Self {
            slots: [const { OnceLock::new() }; WIDE_SLAB_LEN],
        })
    }
}

struct WideIntern {
    map: HashMap<(i128, i128), usize>,
    next: usize,
}

struct WideStore {
    slabs: [OnceLock<Box<WideSlab>>; WIDE_MAX_SLABS],
    intern: Mutex<WideIntern>,
}

impl WideStore {
    fn new() -> Box<Self> {
        Box::new(Self {
            slabs: [const { OnceLock::new() }; WIDE_MAX_SLABS],
            intern: Mutex::new(WideIntern {
                map: HashMap::new(),
                next: 0,
            }),
        })
    }

    fn try_intern(&self, n: i128, d: i128, max_slabs: usize) -> Option<Fraction> {
        debug_assert!(d > 0);
        let mut guard = self
            .intern
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(&idx) = guard.map.get(&(n, d)) {
            return Some(Fraction(
                NonZeroU64::new(WIDE_FLAG | (idx as u64)).expect("wide flag is non-zero"),
            ));
        }
        let idx = guard.next;
        let slab_idx = idx >> WIDE_SLAB_SHIFT;
        if slab_idx >= max_slabs {
            return None;
        }
        let slab = self.slabs[slab_idx].get_or_init(WideSlab::new);
        let _ = slab.slots[idx & (WIDE_SLAB_LEN - 1)].set((n, d));
        guard.map.insert((n, d), idx);
        guard.next = idx + 1;
        Some(Fraction(
            NonZeroU64::new(WIDE_FLAG | (idx as u64)).expect("wide flag is non-zero"),
        ))
    }
}

static WIDE_STORE: OnceLock<Box<WideStore>> = OnceLock::new();

#[cold]
#[inline(never)]
fn wide_store() -> &'static WideStore {
    WIDE_STORE.get_or_init(WideStore::new)
}

/// Lock-free O(1) lookup for a wide `(i128, i128)` fraction handle.
#[cold]
#[inline(never)]
fn wide_parts(idx: usize) -> (i128, i128) {
    let store = wide_store();
    let slab = store.slabs[idx >> WIDE_SLAB_SHIFT]
        .get()
        .expect("wide fraction slab initialized before handle");
    *slab.slots[idx & (WIDE_SLAB_LEN - 1)]
        .get()
        .expect("wide fraction slot initialized before handle")
}

/// Intern a reduced `(n, d)` pair that does not fit inline (`i39` / `u24`),
/// returning `None` if the wide table is full.
#[cold]
#[inline(never)]
fn try_intern_wide(n: i128, d: i128) -> Option<Fraction> {
    wide_store().try_intern(n, d, WIDE_MAX_SLABS)
}

/// Intern a reduced `(n, d)` pair that does not fit inline (`i39` / `u24`).
#[cold]
#[inline(never)]
fn intern_wide(n: i128, d: i128) -> Fraction {
    try_intern_wide(n, d).expect("rustel-fraction: wide fraction table capacity exceeded")
}

/// An exact rational. Always normalised: `d > 0`, `gcd(|n|, d) == 1`, and
/// zero is canonically `0/1`.
///
/// Compact 8-byte `Copy` layout inside one `NonZeroU64`:
/// - bit 63 == 0: inline 39-bit signed numerator in bits 24..63 and 24-bit
///   positive denominator (`1..=16_777_215`) in bits 0..24.
/// - bit 63 == 1: canonical interned handle into the lock-free wide `(i128, i128)` table.
///
/// Canonical interning keeps derived `PartialEq`, `Eq`, and `Hash` exact.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fraction(NonZeroU64);

const _: () = assert!(std::mem::size_of::<Fraction>() == 8);
const _: () = assert!(std::mem::size_of::<Option<Fraction>>() == 8);

/// Pack a validated inline `(n, d)` pair (`INLINE_NUM_MIN..=INLINE_NUM_MAX`,
/// `1..=DEN_MASK`) into one `NonZeroU64`.
#[inline(always)]
const fn pack_inline(n: i64, d: u64) -> Fraction {
    assert!(
        n >= INLINE_NUM_MIN && n <= INLINE_NUM_MAX && d >= 1 && d <= DEN_MASK,
        "rustel-fraction: inline fraction out of bounds"
    );
    let raw = (((n as u64) << DEN_BITS) & !WIDE_FLAG) | d;
    match NonZeroU64::new(raw) {
        Some(nz) => Fraction(nz),
        None => panic!("rustel-fraction: zero denominator"),
    }
}

#[inline(always)]
const fn decode_inline_n(raw: u64) -> i64 {
    ((raw as i64) << 1) >> (DEN_BITS + 1)
}

#[inline(always)]
fn try_from_reduced_i64(n: i64, d: i64) -> Option<Fraction> {
    debug_assert!(d > 0);
    if (INLINE_NUM_MIN..=INLINE_NUM_MAX).contains(&n) && (d as u64) <= DEN_MASK {
        Some(pack_inline(n, d as u64))
    } else {
        try_intern_wide(i128::from(n), i128::from(d))
    }
}

#[inline(always)]
fn from_reduced_i64(n: i64, d: i64) -> Fraction {
    debug_assert!(d > 0);
    if (INLINE_NUM_MIN..=INLINE_NUM_MAX).contains(&n) && (d as u64) <= DEN_MASK {
        pack_inline(n, d as u64)
    } else {
        intern_wide(i128::from(n), i128::from(d))
    }
}

/// Pack a reduced `(n, d)` pair (`d > 0`) inline when it fits in `(i39, u24)`,
/// or intern it in the wide table otherwise, returning `None` if the wide
/// table is full.
#[inline(always)]
fn try_from_reduced(n: i128, d: i128) -> Option<Fraction> {
    debug_assert!(d > 0);
    if (i128::from(INLINE_NUM_MIN)..=i128::from(INLINE_NUM_MAX)).contains(&n)
        && d <= i128::from(DEN_MASK)
    {
        Some(pack_inline(n as i64, d as u64))
    } else {
        try_intern_wide(n, d)
    }
}

/// Pack a reduced `(n, d)` pair (`d > 0`) inline when it fits in `(i39, u24)`,
/// or intern it in the wide table otherwise.
#[inline(always)]
fn from_reduced(n: i128, d: i128) -> Fraction {
    debug_assert!(d > 0);
    if (i128::from(INLINE_NUM_MIN)..=i128::from(INLINE_NUM_MAX)).contains(&n)
        && d <= i128::from(DEN_MASK)
    {
        pack_inline(n as i64, d as u64)
    } else {
        intern_wide(n, d)
    }
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
    // Terms of a machine word need no wide division.
    if let (Ok(a), Ok(b)) = (u64::try_from(a), u64::try_from(b)) {
        return u128::from(gcd_u64(a, b));
    }
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
    if a == 0 {
        return b;
    }
    if b == 0 {
        return a;
    }
    let shift = (a | b).trailing_zeros();
    a >>= a.trailing_zeros();
    loop {
        b >>= b.trailing_zeros();
        if a > b {
            std::mem::swap(&mut a, &mut b);
        }
        b -= a;
        if b == 0 {
            return a << shift;
        }
    }
}

/// A grid of equally spaced ticks per cycle. Each residue gives the common
/// divisor of its tick and the grid denominator, and the reduced denominator.
#[derive(Debug)]
pub struct TickGrid {
    reductions: Box<[(NonZeroU16, NonZeroU16)]>,
}

impl TickGrid {
    /// Four bytes per residue keep the grid within four KiB.
    pub const MAX_DENOM: i64 = 1024;

    /// A grid with `denom` ticks per cycle, when it fits the table.
    pub fn new(denom: i64) -> Option<Self> {
        if !(1..=Self::MAX_DENOM).contains(&denom) {
            return None;
        }
        let denom = denom as u16;
        let reductions = (0..denom)
            .map(|residue| {
                let gcd = gcd_u64(u64::from(residue), u64::from(denom)) as u16;
                (
                    NonZeroU16::new(gcd).expect("a positive common divisor"),
                    NonZeroU16::new(denom / gcd).expect("a positive denominator"),
                )
            })
            .collect();
        Some(Self { reductions })
    }

    #[inline]
    pub fn denom(&self) -> i64 {
        self.reductions.len() as i64
    }

    /// The signed tick at `time`, when it lies on this grid and fits in a word.
    #[inline]
    pub fn ticks(&self, time: Fraction) -> Option<i64> {
        let n = i64::try_from(time.numer()).ok()?;
        let d = usize::try_from(time.denom()).ok()?;
        if d == self.reductions.len() {
            return Some(n);
        }
        let &(gcd, scale) = self.reductions.get(d)?;
        if usize::from(gcd.get()) != d {
            return None;
        }
        n.checked_mul(i64::from(scale.get()))
    }

    /// The reduced time of a signed tick. The residue of its magnitude
    /// selects the same common divisor for both signs.
    #[inline]
    pub fn time(&self, ticks: i64) -> Fraction {
        let residue = ticks.unsigned_abs() % self.reductions.len() as u64;
        let (gcd, denominator) = self.reductions[residue as usize];
        from_reduced_i64(ticks / i64::from(gcd.get()), i64::from(denominator.get()))
    }
}

/// Reduce a numerator and a positive denominator from the 64-bit path,
/// returning `None` if the result needs a full wide table.
#[inline]
fn try_reduced_small(n: i64, d: i64) -> Option<Fraction> {
    debug_assert!(d > 0);
    if n == 0 {
        return Some(Fraction::ZERO);
    }
    if d == 1 || n == 1 || n == -1 {
        return try_from_reduced_i64(n, d);
    }
    // A power of two takes out its common factor of two with no division.
    if d & (d - 1) == 0 {
        let shift = n.trailing_zeros().min(d.trailing_zeros());
        return try_from_reduced_i64(n >> shift, d >> shift);
    }
    try_reduced_small_gcd(n, d)
}

#[inline(never)]
fn try_reduced_small_gcd(n: i64, d: i64) -> Option<Fraction> {
    let divisor = gcd_u64(n.unsigned_abs(), d as u64) as i64;
    if divisor == 1 {
        return try_from_reduced_i64(n, d);
    }
    if divisor & (divisor - 1) == 0 {
        let shift = divisor.trailing_zeros();
        return try_from_reduced_i64(n >> shift, d >> shift);
    }
    try_from_reduced_i64(n / divisor, d / divisor)
}

/// Reduce a numerator and a positive denominator from the 64-bit path.
#[inline]
fn reduced_small(n: i64, d: i64) -> Fraction {
    try_reduced_small(n, d).expect("rustel-fraction: wide fraction table capacity exceeded")
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

#[inline]
fn checked_normalised(n: i128, d: i128) -> Option<Fraction> {
    if d == 0 {
        return None;
    }
    if n == 0 {
        return Some(Fraction::ZERO);
    }
    let negative = (n < 0) != (d < 0);
    let (numerator, denominator) = (n.unsigned_abs(), d.unsigned_abs());
    // Terms of a machine word reduce with no wide division.
    let (numerator, denominator) = match (u64::try_from(numerator), u64::try_from(denominator)) {
        (Ok(numerator), Ok(denominator)) => {
            if denominator == 1 || numerator == 1 {
                (u128::from(numerator), u128::from(denominator))
            } else if denominator & (denominator - 1) == 0 {
                let shift = numerator.trailing_zeros().min(denominator.trailing_zeros());
                (
                    u128::from(numerator >> shift),
                    u128::from(denominator >> shift),
                )
            } else {
                let divisor = gcd_u64(numerator, denominator);
                if divisor == 1 {
                    (u128::from(numerator), u128::from(denominator))
                } else if divisor & (divisor - 1) == 0 {
                    let shift = divisor.trailing_zeros();
                    (
                        u128::from(numerator >> shift),
                        u128::from(denominator >> shift),
                    )
                } else {
                    (
                        u128::from(numerator / divisor),
                        u128::from(denominator / divisor),
                    )
                }
            }
        }
        _ => {
            let divisor = gcd_u128(numerator, denominator);
            (numerator / divisor, denominator / divisor)
        }
    };
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
    try_from_reduced(numerator, denominator as i128)
}

/// fraction.js's bounded Stern-Brocot search, with consecutive moves batched.
///
/// A scalar walk changes one bound per iteration. A run of `k` lower-bound
/// moves is exactly `(a + k*c)/(b + k*d)`; upper-bound moves are symmetric.
/// The mediants are monotone, so a binary search finds the last move whose
/// f64 comparison has the same result. This preserves the scalar state
/// machine, including equality caused by f64 rounding, while making a run
/// logarithmic rather than linear in the denominator limit.
///
/// Each bound stays between `0/1` and `1/1`, and a move ends at most one
/// step past `limit`, so no term is above `2 * limit`: the terms are
/// machine words, which a float takes with no library call.
fn farey_approximation(p1: f64, limit: i64) -> (i64, i64) {
    farey_approximation_with_runs(p1, limit).0
}

fn farey_approximation_with_runs(p1: f64, limit: i64) -> ((i64, i64), usize) {
    fn last_true(mut low: i64, mut high: i64, predicate: impl Fn(i64) -> bool) -> i64 {
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

    let (mut a, mut b, mut c, mut d) = (0i64, 1i64, 1i64, 1i64);
    // The loop always assigns these before returning: its first iteration
    // takes either the equality branch or a batched run.
    let (mut n, mut den) = (0i64, 1i64);
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
    pub const ZERO: Fraction = pack_inline(0, 1);
    pub const ONE: Fraction = pack_inline(1, 1);

    #[inline(always)]
    fn is_zero(self) -> bool {
        self == Self::ZERO
    }

    #[inline(always)]
    fn inline_parts(self) -> Option<(i64, i64)> {
        let raw = self.0.get();
        ((raw & WIDE_FLAG) == 0).then_some((decode_inline_n(raw), (raw & DEN_MASK) as i64))
    }

    #[inline(always)]
    fn parts(self) -> (i128, i128) {
        let raw = self.0.get();
        if (raw & WIDE_FLAG) == 0 {
            (i128::from(decode_inline_n(raw)), i128::from(raw & DEN_MASK))
        } else {
            wide_parts((raw & !WIDE_FLAG) as usize)
        }
    }

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

    /// The reduced cycle time of a 64-bit signed tick with a power-of-two grid.
    #[inline(always)]
    pub fn dyadic_i64(n: i64, shift: u32) -> Option<Self> {
        if n == 0 {
            return Some(Self::ZERO);
        }
        if shift < DEN_BITS {
            let tz = n.trailing_zeros().min(shift);
            let rn = n >> tz;
            if (INLINE_NUM_MIN..=INLINE_NUM_MAX).contains(&rn) {
                return Some(pack_inline(rn, 1u64 << (shift - tz)));
            }
        }
        Self::dyadic(i128::from(n), shift)
    }

    /// The reduced cycle time of a 64-bit signed tick for a validated `shift < 63`.
    #[inline(always)]
    pub fn dyadic_i64_small(n: i64, shift: u32) -> Self {
        debug_assert!(shift < 63);
        if n == 0 {
            return Self::ZERO;
        }
        let tz = n.trailing_zeros().min(shift);
        let rn = n >> tz;
        let rem = shift - tz;
        if rem < DEN_BITS && (INLINE_NUM_MIN..=INLINE_NUM_MAX).contains(&rn) {
            return pack_inline(rn, 1u64 << rem);
        }
        from_reduced(i128::from(rn), 1i128 << rem)
    }

    /// The reduced cycle time of a 64-bit signed tick after the caller has
    /// verified `shift < DEN_BITS` and `n` lies within the inline `i39` range.
    #[inline(always)]
    pub fn dyadic_i64_in_bounds(n: i64, shift: u32) -> Self {
        debug_assert!(shift < DEN_BITS);
        debug_assert!((INLINE_NUM_MIN..=INLINE_NUM_MAX).contains(&n));
        if n == 0 {
            return Self::ZERO;
        }
        let tz = n.trailing_zeros().min(shift);
        pack_inline(n >> tz, 1u64 << (shift - tz))
    }

    /// Exact division by `2^shift` using bit shifts with no GCD or multiplication.
    #[inline]
    pub fn checked_div_pow2(self, shift: u32) -> Option<Self> {
        if self.is_zero() || shift == 0 {
            return Some(self);
        }
        if let Some((n, d)) = self.inline_parts() {
            let tz = n.trailing_zeros().min(shift);
            let rem = shift - tz;
            let shifted_n = n >> tz;
            if rem == 0 {
                return Some(pack_inline(shifted_n, d as u64));
            }
            if rem < DEN_BITS && ((d as u64) << rem) <= DEN_MASK {
                return Some(pack_inline(shifted_n, (d as u64) << rem));
            }
        }
        self.checked_div_pow2_wide(shift)
    }

    #[cold]
    #[inline(never)]
    fn checked_div_pow2_wide(self, shift: u32) -> Option<Self> {
        let (n, d) = self.parts();
        let tz = n.trailing_zeros().min(shift);
        let rem = shift - tz;
        if rem == 0 {
            return try_from_reduced(n >> tz, d);
        }
        if rem >= 127 || (d as u128).leading_zeros() <= rem {
            return None;
        }
        try_from_reduced(n >> tz, d << rem)
    }

    /// Exact multiplication by `2^shift` using bit shifts with no GCD or multiplication.
    #[inline]
    pub fn checked_mul_pow2(self, shift: u32) -> Option<Self> {
        if self.is_zero() || shift == 0 {
            return Some(self);
        }
        if let Some((n, d)) = self.inline_parts() {
            let tz = (d as u64).trailing_zeros().min(shift);
            let rem = shift - tz;
            let shifted_d = (d as u64) >> tz;
            if rem == 0 {
                return Some(pack_inline(n, shifted_d));
            }
            if rem < 38
                && let Some(shifted_n) = n.checked_shl(rem)
                && (shifted_n >> rem) == n
                && (INLINE_NUM_MIN..=INLINE_NUM_MAX).contains(&shifted_n)
            {
                return Some(pack_inline(shifted_n, shifted_d));
            }
        }
        self.checked_mul_pow2_wide(shift)
    }

    #[cold]
    #[inline(never)]
    fn checked_mul_pow2_wide(self, shift: u32) -> Option<Self> {
        let (n, d) = self.parts();
        let tz = (d as u128).trailing_zeros().min(shift);
        let rem = shift - tz;
        let shifted_d = d >> tz;
        if rem == 0 {
            return try_from_reduced(n, shifted_d);
        }
        let mag = n.unsigned_abs();
        if rem >= 127 || mag.leading_zeros() <= rem {
            return None;
        }
        let shifted = (mag << rem) as i128;
        try_from_reduced(if n < 0 { -shifted } else { shifted }, shifted_d)
    }

    /// The reduced cycle time of a signed tick on a uniform grid.
    #[inline]
    pub fn from_ticks(ticks: i128, ticks_per_cycle: NonZeroU32) -> Self {
        let d = ticks_per_cycle.get();
        if d.is_power_of_two()
            && let Some(time) = Self::dyadic(ticks, d.trailing_zeros())
        {
            return time;
        }
        // Terms of 64 bits reduce without wide division.
        if let Ok(ticks) = i64::try_from(ticks) {
            return reduced_small(ticks, i64::from(d));
        }
        Self::new(ticks, i128::from(d))
    }

    /// `n / 2^shift`, reduced by a shift of the trailing zeros of `n` with
    /// no gcd. `None` for a `shift` above 126, whose denominator has no room.
    #[inline]
    pub fn dyadic(n: i128, shift: u32) -> Option<Self> {
        if shift > 126 {
            return None;
        }
        if n == 0 {
            return Some(Self::ZERO);
        }
        // The one numerator whose magnitude has no room as it is.
        if n == i128::MIN {
            return Self::checked_new(n, 1i128 << shift);
        }
        let drop = n.trailing_zeros().min(shift);
        try_from_reduced(n >> drop, 1i128 << (shift - drop))
    }

    #[inline(always)]
    pub fn int(n: i128) -> Self {
        from_reduced(n, 1)
    }

    /// Both components as `i64` when each is below [`SMALL_LIMIT`].
    #[inline(always)]
    fn small(self) -> Option<(i64, i64)> {
        const SMALL_U64: u64 = SMALL_LIMIT as u64;
        let raw = self.0.get();
        if (raw & WIDE_FLAG) == 0 {
            let n = decode_inline_n(raw);
            let d = (raw & DEN_MASK) as i64;
            let numerator_fits =
                (n.wrapping_add((SMALL_U64 - 1) as i64) as u64) <= (SMALL_U64 * 2 - 2);
            return numerator_fits.then_some((n, d));
        }
        let (n, d) = wide_parts((raw & !WIDE_FLAG) as usize);
        let numerator_fits =
            (n.wrapping_add(SMALL_LIMIT - 1) as u128) <= (SMALL_LIMIT * 2 - 2) as u128;
        (numerator_fits && d < SMALL_LIMIT).then_some((n as i64, d as i64))
    }

    #[inline(always)]
    pub fn numer(&self) -> i128 {
        let raw = self.0.get();
        if (raw & WIDE_FLAG) == 0 {
            i128::from(decode_inline_n(raw))
        } else {
            wide_parts((raw & !WIDE_FLAG) as usize).0
        }
    }

    #[inline(always)]
    pub fn denom(&self) -> i128 {
        let raw = self.0.get();
        if (raw & WIDE_FLAG) == 0 {
            i128::from(raw & DEN_MASK)
        } else {
            wide_parts((raw & !WIDE_FLAG) as usize).1
        }
    }

    /// Exact reduced numerator. Callers sending numerator/denominator pairs
    /// as `f64` must range-check against 2^53 to preserve integer precision.
    #[inline(always)]
    pub fn numerator(&self) -> i128 {
        self.numer()
    }

    /// Exact (positive) denominator in reduced form.
    #[inline(always)]
    pub fn denominator(&self) -> i128 {
        self.denom()
    }

    /// Lossy: for interop at the audio boundary only. Never for cycle maths.
    #[inline(always)]
    pub fn to_f64(&self) -> f64 {
        let raw = self.0.get();
        if (raw & WIDE_FLAG) == 0 {
            decode_inline_n(raw) as f64 / (raw & DEN_MASK) as f64
        } else {
            let (n, d) = wide_parts((raw & !WIDE_FLAG) as usize);
            n as f64 / d as f64
        }
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
                return try_from_reduced(i128::MIN, 1);
            }
            if p1 >= I128_MAX_EXCLUSIVE {
                return None;
            }
            let n = p1 as i128;
            return try_from_reduced(if negative { -n } else { n }, 1);
        }
        if p1 <= 0.0 {
            return Some(Fraction::ZERO);
        }

        let mut z: i128 = 1;
        const N: i64 = 10_000_000;

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
        let n = i128::from(n).checked_mul(z)?;
        checked_normalised(if negative { -n } else { n }, i128::from(den))
    }

    // -- arithmetic --------------------------------------------------------

    /// `self + cycles`, returning `None` if the result cannot fit in `i128`
    /// components. Adding a whole number keeps the fraction reduced, so this
    /// needs no gcd.
    #[inline(always)]
    pub fn checked_add_int(self, cycles: i128) -> Option<Self> {
        let raw = self.0.get();
        if (raw & WIDE_FLAG) == 0
            && let Ok(c) = i64::try_from(cycles)
            && let Some(scaled) = c.checked_mul((raw & DEN_MASK) as i64)
            && let Some(n) = decode_inline_n(raw).checked_add(scaled)
            && (INLINE_NUM_MIN..=INLINE_NUM_MAX).contains(&n)
        {
            return Some(pack_inline(n, raw & DEN_MASK));
        }
        self.checked_add_int_wide(cycles)
    }

    /// `self + cycles` on the inline path, returning `None` if `self` is wide
    /// or the result leaves the inline `i39` numerator range.
    #[inline(always)]
    pub fn checked_add_i64(self, cycles: i64) -> Option<Self> {
        let raw = self.0.get();
        if (raw & WIDE_FLAG) == 0
            && let Some(scaled) = cycles.checked_mul((raw & DEN_MASK) as i64)
            && let Some(n) = decode_inline_n(raw).checked_add(scaled)
            && (INLINE_NUM_MIN..=INLINE_NUM_MAX).contains(&n)
        {
            return Some(pack_inline(n, raw & DEN_MASK));
        }
        None
    }

    /// `self + cycles` on the inline path after the caller has verified that
    /// `self` is inline and `self.n + cycles * self.d` stays in `i39` bounds.
    #[inline(always)]
    pub fn add_i64_in_bounds(self, cycles: i64) -> Self {
        let raw = self.0.get();
        debug_assert!((raw & WIDE_FLAG) == 0);
        debug_assert!(
            cycles
                .checked_mul((raw & DEN_MASK) as i64)
                .and_then(|s| decode_inline_n(raw).checked_add(s))
                .is_some_and(|n| (INLINE_NUM_MIN..=INLINE_NUM_MAX).contains(&n))
        );
        let d = (raw & DEN_MASK) as i64;
        let delta = (cycles.wrapping_mul(d) as u64) << DEN_BITS;
        let next_raw = raw.wrapping_add(delta) & !WIDE_FLAG;
        Fraction(NonZeroU64::new(next_raw).unwrap_or(self.0))
    }

    /// Precomputed packed-numerator delta for advancing an inline fraction by
    /// `cycles` with [`Self::add_inline_step_raw`].
    #[inline(always)]
    pub fn inline_step_raw(self, cycles: i64) -> u64 {
        let raw = self.0.get();
        debug_assert!((raw & WIDE_FLAG) == 0);
        let d = (raw & DEN_MASK) as i64;
        (cycles.wrapping_mul(d) as u64) << DEN_BITS
    }

    /// Advance an inline fraction by a delta returned by [`Self::inline_step_raw`]
    /// after the caller has verified the result stays within the inline `i39`
    /// numerator bounds.
    #[inline(always)]
    pub fn add_inline_step_raw(self, step_raw: u64) -> Self {
        let raw = self.0.get();
        debug_assert!((raw & WIDE_FLAG) == 0);
        let next_raw = raw.wrapping_add(step_raw) & !WIDE_FLAG;
        Fraction(NonZeroU64::new(next_raw).unwrap_or(self.0))
    }

    #[cold]
    #[inline(never)]
    fn checked_add_int_wide(self, cycles: i128) -> Option<Self> {
        let (sn, d) = self.parts();
        let scaled = match cycles.checked_mul(d) {
            Some(scaled) => scaled,
            // The sum can fit when the product does not.
            None => return self.checked_add(try_from_reduced(cycles, 1)?),
        };
        try_from_reduced(sn.checked_add(scaled)?, d)
    }

    /// Exact addition, returning `None` if the reduced result cannot fit in
    /// `i128` components.
    #[inline]
    pub fn checked_add(self, o: Self) -> Option<Self> {
        if let Some(sum) = self.whole_sum(o, false) {
            return Some(sum);
        }
        if let (Some((n, d)), Some((on, od))) = (self.small(), o.small()) {
            // The same denominator leaves smaller terms to reduce.
            if d == od {
                if (d & (d - 1)) == 0 {
                    return Self::dyadic_i64(n + on, d.trailing_zeros());
                }
                return try_reduced_small(n + on, d);
            }
            if ((d & (d - 1)) | (od & (od - 1))) == 0 {
                let sa = d.trailing_zeros();
                let sb = od.trailing_zeros();
                let (sum, shift) = if sa >= sb {
                    (n + (on << (sa - sb)), sa)
                } else {
                    ((n << (sb - sa)) + on, sb)
                };
                return Self::dyadic_i64(sum, shift);
            }
            return try_reduced_small(n * od + on * d, d * od);
        }
        self.checked_add_wide(o)
    }

    #[cold]
    #[inline(never)]
    fn checked_add_wide(self, o: Self) -> Option<Self> {
        let (sn, sd) = self.parts();
        let (on, od) = o.parts();
        let native = || {
            let divisor = gcd_u128(sd as u128, od as u128) as i128;
            let left_scale = od / divisor;
            let right_scale = sd / divisor;
            let numerator = sn
                .checked_mul(left_scale)?
                .checked_add(on.checked_mul(right_scale)?)?;
            let denominator = sd.checked_mul(left_scale)?;
            checked_normalised(numerator, denominator)
        };
        native().or_else(|| {
            fraction_from_bigints(
                BigInt::from(sn) * BigInt::from(od) + BigInt::from(on) * BigInt::from(sd),
                BigInt::from(sd) * BigInt::from(od),
            )
        })
    }

    /// Checked exact subtraction; unlike `self.add(o.neg())`, this can inspect
    /// `i128::MIN` without first negating it.
    #[inline]
    pub fn checked_sub(self, o: Self) -> Option<Self> {
        if let Some(difference) = self.whole_sum(o, true) {
            return Some(difference);
        }
        if let (Some((n, d)), Some((on, od))) = (self.small(), o.small()) {
            if d == od {
                if (d & (d - 1)) == 0 {
                    return Self::dyadic_i64(n - on, d.trailing_zeros());
                }
                return try_reduced_small(n - on, d);
            }
            if ((d & (d - 1)) | (od & (od - 1))) == 0 {
                let sa = d.trailing_zeros();
                let sb = od.trailing_zeros();
                let (diff, shift) = if sa >= sb {
                    (n - (on << (sa - sb)), sa)
                } else {
                    ((n << (sb - sa)) - on, sb)
                };
                return Self::dyadic_i64(diff, shift);
            }
            return try_reduced_small(n * od - on * d, d * od);
        }
        self.checked_sub_wide(o)
    }

    #[cold]
    #[inline(never)]
    fn checked_sub_wide(self, o: Self) -> Option<Self> {
        let (sn, sd) = self.parts();
        let (on, od) = o.parts();
        let native = || {
            let divisor = gcd_u128(sd as u128, od as u128) as i128;
            let left_scale = od / divisor;
            let right_scale = sd / divisor;
            let numerator = sn
                .checked_mul(left_scale)?
                .checked_sub(on.checked_mul(right_scale)?)?;
            let denominator = sd.checked_mul(left_scale)?;
            checked_normalised(numerator, denominator)
        };
        native().or_else(|| {
            fraction_from_bigints(
                BigInt::from(sn) * BigInt::from(od) - BigInt::from(on) * BigInt::from(sd),
                BigInt::from(sd) * BigInt::from(od),
            )
        })
    }

    /// `self + o`, or `self - o` when `subtract`, when one side is a whole
    /// number: `n/d ± k` is `(n ± k·d)/d`, which is already reduced, so it
    /// needs no gcd. `None` when neither side is whole, or when the
    /// numerator does not fit inline, which the general path then decides.
    #[inline]
    fn whole_sum(self, o: Self, subtract: bool) -> Option<Self> {
        let ((sn, sd), (on, od)) = (self.inline_parts()?, o.inline_parts()?);
        let (fn_n, fn_d, whole, whole_first) = if od == 1 {
            (sn, sd, on, false)
        } else if sd == 1 {
            (on, od, sn, true)
        } else {
            return None;
        };
        let scaled = whole.checked_mul(fn_d)?;
        let n = match (subtract, whole_first) {
            (false, _) => fn_n.checked_add(scaled)?,
            (true, false) => fn_n.checked_sub(scaled)?,
            (true, true) => scaled.checked_sub(fn_n)?,
        };
        (INLINE_NUM_MIN..=INLINE_NUM_MAX)
            .contains(&n)
            .then(|| pack_inline(n, fn_d as u64))
    }

    /// Checked multiplication. The `i128` path cancels across the operands
    /// before either product.
    #[inline]
    pub fn checked_mul(self, o: Self) -> Option<Self> {
        if let (Some((n, d)), Some((on, od))) = (self.small(), o.small()) {
            return try_reduced_small(n * on, d * od);
        }
        self.checked_mul_wide(o)
    }

    #[cold]
    #[inline(never)]
    fn checked_mul_wide(self, o: Self) -> Option<Self> {
        let (sn, sd) = self.parts();
        let (on, od) = o.parts();
        let left_cancel = gcd_u128(sn.unsigned_abs(), od as u128);
        let right_cancel = gcd_u128(on.unsigned_abs(), sd as u128);
        let left_n = div_signed_by_u128(sn, left_cancel)?;
        let right_n = div_signed_by_u128(on, right_cancel)?;
        let left_d = sd / i128::try_from(right_cancel).ok()?;
        let right_d = od / i128::try_from(left_cancel).ok()?;
        checked_normalised(left_n.checked_mul(right_n)?, left_d.checked_mul(right_d)?)
    }

    /// Checked division, with cross-cancellation on the `i128` path. `None`
    /// includes division by zero and an exact result outside the native
    /// `i128/i128` representation.
    #[inline]
    pub fn checked_div(self, o: Self) -> Option<Self> {
        if o.is_zero() {
            return None;
        }
        if self.is_zero() {
            return Some(Fraction::ZERO);
        }
        if let (Some((n, d)), Some((on, od))) = (self.small(), o.small()) {
            // The divisor's sign moves to the numerator.
            let (numerator, denominator) = (n * od, d * on);
            return if denominator < 0 {
                try_reduced_small(-numerator, -denominator)
            } else {
                try_reduced_small(numerator, denominator)
            };
        }
        self.checked_div_wide(o)
    }

    #[cold]
    #[inline(never)]
    fn checked_div_wide(self, o: Self) -> Option<Self> {
        let (sn, sd) = self.parts();
        let (on, od) = o.parts();
        let numerator_cancel = gcd_u128(sn.unsigned_abs(), on.unsigned_abs());
        let denominator_cancel = gcd_u128(sd as u128, od as u128);
        let left_n = div_signed_by_u128(sn, numerator_cancel)?;
        let right_n = div_signed_by_u128(on, numerator_cancel)?;
        let left_d = sd / i128::try_from(denominator_cancel).ok()?;
        let right_d = od / i128::try_from(denominator_cancel).ok()?;
        checked_normalised(left_n.checked_mul(right_d)?, left_d.checked_mul(right_n)?)
    }

    /// Checked `lcm(a/b, c/d) = lcm(|a|,|c|) / gcd(b,d)`.
    ///
    /// Returns `None` when the non-negative numerator cannot fit in `i128`.
    pub fn checked_lcm(self, o: Self) -> Option<Self> {
        if self.is_zero() || o.is_zero() {
            return Some(Fraction::ZERO);
        }
        let (sn, sd) = self.parts();
        let (on, od) = o.parts();
        let left = sn.unsigned_abs();
        let right = on.unsigned_abs();
        let divisor = gcd_u128(left, right);
        let numerator = (left / divisor).checked_mul(right)?;
        let numerator = i128::try_from(numerator).ok()?;
        let denominator = gcd_u128(sd as u128, od as u128);
        let denominator = i128::try_from(denominator).ok()?;
        checked_normalised(numerator, denominator)
    }

    /// Checked negation. The sole unrepresentable case is `i128::MIN`.
    #[inline]
    pub fn checked_neg(self) -> Option<Self> {
        if let Some((n, d)) = self.inline_parts()
            && n > INLINE_NUM_MIN
        {
            return Some(pack_inline(-n, d as u64));
        }
        let (sn, sd) = self.parts();
        try_from_reduced(sn.checked_neg()?, sd)
    }

    /// Checked truncated modulo, reducing arbitrary-precision intermediates
    /// before deciding whether the exact result fits this native type.
    pub fn checked_rem(self, o: Self) -> Option<Self> {
        if o.is_zero() {
            return None;
        }
        let (sn, sd) = self.parts();
        let (on, od) = o.parts();
        let native = || {
            let left = sn.checked_mul(od)?;
            let right = on.checked_mul(sd)?;
            let denominator = sd.checked_mul(od)?;
            checked_normalised(left.checked_rem(right)?, denominator)
        };
        native().or_else(|| {
            let left = BigInt::from(sn) * BigInt::from(od);
            let right = BigInt::from(on) * BigInt::from(sd);
            fraction_from_bigints(left % right, BigInt::from(sd) * BigInt::from(od))
        })
    }

    /// Exact overflow-free comparison for guarded construction code.
    pub fn checked_cmp(self, o: Self) -> Option<Ordering> {
        let (sn, sd) = self.parts();
        let (on, od) = o.parts();
        let sign = sn.signum().cmp(&on.signum());
        if sign != Ordering::Equal {
            return Some(sign);
        }
        if sn == 0 {
            return Some(Ordering::Equal);
        }
        let magnitude = cmp_positive(sn.unsigned_abs(), sd as u128, on.unsigned_abs(), od as u128);
        Some(if sn < 0 {
            magnitude.reverse()
        } else {
            magnitude
        })
    }

    #[inline]
    #[track_caller]
    pub fn add(self, o: Self) -> Self {
        ck(self.checked_add(o), "add")
    }

    #[inline]
    #[track_caller]
    pub fn sub(self, o: Self) -> Self {
        ck(self.checked_sub(o), "sub")
    }

    #[inline]
    #[track_caller]
    pub fn mul(self, o: Self) -> Self {
        ck(self.checked_mul(o), "mul")
    }

    #[inline]
    #[track_caller]
    pub fn div(self, o: Self) -> Self {
        assert!(!o.is_zero(), "rustel-fraction: division by zero");
        ck(self.checked_div(o), "div")
    }

    /// Checked because `-i128::MIN` is not representable.
    #[inline]
    #[track_caller]
    pub fn neg(self) -> Self {
        ck(self.checked_neg(), "neg")
    }

    /// Truncated modulo, taking the sign of the dividend - matches fraction.js:
    /// `(-7/3) % (1/2) == -1/3`.
    #[track_caller]
    pub fn rem(self, o: Self) -> Self {
        assert!(!o.is_zero(), "rustel-fraction: modulo by zero");
        ck(self.checked_rem(o), "rem")
    }

    // -- rounding ----------------------------------------------------------

    /// Integer floor toward negative infinity without constructing a `Fraction`.
    #[inline]
    pub fn floor_int(self) -> i128 {
        if let Some((n, d)) = self.inline_parts() {
            if d == 1 {
                return i128::from(n);
            }
            if d & (d - 1) == 0 {
                return i128::from(n >> d.trailing_zeros());
            }
            return i128::from(n.div_euclid(d));
        }
        let (n, d) = self.parts();
        if d == 1 {
            return n;
        }
        if d & (d - 1) == 0 {
            return n >> d.trailing_zeros();
        }
        n.div_euclid(d)
    }

    /// Rounds toward negative infinity: `(-1/4).floor() == -1`, so negative
    /// times map to the start of their containing cycle.
    #[inline]
    pub fn floor(self) -> Self {
        if let Some((n, d)) = self.inline_parts() {
            if d == 1 {
                return self;
            }
            let floored = if d & (d - 1) == 0 {
                n >> d.trailing_zeros()
            } else {
                n.div_euclid(d)
            };
            return pack_inline(floored, 1);
        }
        from_reduced(self.floor_int(), 1)
    }

    #[inline]
    pub fn ceil(self) -> Self {
        if let Some((n, d)) = self.inline_parts() {
            if d == 1 {
                return self;
            }
            let floored = if d & (d - 1) == 0 {
                n >> d.trailing_zeros()
            } else {
                n.div_euclid(d)
            };
            // `floored` is strictly less than `INLINE_NUM_MAX` since `d >= 2`.
            return pack_inline(floored + 1, 1);
        }
        let (_, d) = self.parts();
        if d == 1 {
            return self;
        }
        from_reduced(self.floor_int() + 1, 1)
    }

    /// `(self / o).floor()` as a whole count, with no reduced quotient on
    /// the 64-bit path. `None` as [`Self::checked_div`] gives it.
    #[inline]
    pub fn checked_div_floor(self, o: Self) -> Option<i128> {
        match self.small_quotient(o) {
            Some((n, d)) if d & (d - 1) == 0 => Some(i128::from(n >> d.trailing_zeros())),
            Some((n, d)) => Some(i128::from(n.div_euclid(d))),
            None => Some(self.checked_div(o)?.floor_int()),
        }
    }

    /// `(self / o).ceil()` as a whole count, with no reduced quotient on
    /// the 64-bit path. `None` as [`Self::checked_div`] gives it.
    #[inline]
    pub fn checked_div_ceil(self, o: Self) -> Option<i128> {
        match self.small_quotient(o) {
            // The floor of the negated quotient, negated: no remainder to
            // test, and no overflow for products below 2^62.
            Some((n, d)) if d & (d - 1) == 0 => Some(-i128::from((-n) >> d.trailing_zeros())),
            Some((n, d)) => Some(-i128::from((-n).div_euclid(d))),
            None => {
                let q = self.checked_div(o)?;
                let (n, d) = q.parts();
                if d == 1 {
                    Some(n)
                } else {
                    q.floor_int().checked_add(1)
                }
            }
        }
    }

    /// `self / o` as a numerator and a positive denominator that are not
    /// reduced, for a divisor above zero on the 64-bit path.
    #[inline]
    fn small_quotient(self, o: Self) -> Option<(i64, i64)> {
        let ((n, d), (on, od)) = (self.small()?, o.small()?);
        (on > 0).then_some((n * od, d * on))
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
        if let Some((n, d)) = self.inline_parts() {
            let floored = if d == 1 {
                n
            } else if d & (d - 1) == 0 {
                n >> d.trailing_zeros()
            } else {
                n.div_euclid(d)
            };
            if floored < INLINE_NUM_MAX {
                return pack_inline(floored + 1, 1);
            }
        }
        from_reduced(ck(self.floor_int().checked_add(1), "add"), 1)
    }

    /// Position relative to the start of this time's cycle.
    #[inline]
    pub fn cycle_pos(self) -> Self {
        if let Some((n, d)) = self.inline_parts() {
            if d == 1 {
                return Fraction::ZERO;
            }
            // `n` is coprime to `d`, so `n mod d` is non-zero and coprime to `d`.
            if d & (d - 1) == 0 {
                return pack_inline(n & (d - 1), d as u64);
            }
            return pack_inline(n.rem_euclid(d), d as u64);
        }
        let (n, d) = self.parts();
        if d == 1 {
            return Fraction::ZERO;
        }
        if d & (d - 1) == 0 {
            return from_reduced(n & (d - 1), d);
        }
        from_reduced(n.rem_euclid(d), d)
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
        let (sn, sd) = self.parts();
        let (on, od) = o.parts();
        let n = gcd_i128(sn, on);
        let g = gcd_i128(sd, od);
        let l = ck(sd.checked_mul(od / g), "gcd");
        Self::new(n, l)
    }

    /// `lcm(a/b, c/d) = lcm(|a|,|c|) / gcd(b,d)`
    ///
    /// The result is non-negative, as in fraction.js: `lcm(-1/2, 1/3) == 1/1`.
    #[track_caller]
    pub fn lcm(self, o: Self) -> Self {
        if self.is_zero() || o.is_zero() {
            return Fraction::ZERO;
        }
        let (sn, sd) = self.parts();
        let (on, od) = o.parts();
        // Unsigned for the same reason as `gcd_i128`: `i128::MIN.abs()`
        // overflows, and a numerator of `i128::MIN` is representable.
        let (an, bn) = (sn.unsigned_abs(), on.unsigned_abs());
        let g = gcd_u128(an, bn);
        let n = ck(
            an.checked_mul(bn / g).and_then(|v| i128::try_from(v).ok()),
            "lcm",
        );
        Self::new(n, gcd_i128(sd, od))
    }

    /// Returns `o` if self is zero.
    #[inline]
    pub fn or(self, o: Self) -> Self {
        if self.is_zero() { o } else { self }
    }

    /// Snapshot serialisation. Always `n/d`, even when `d == 1`.
    pub fn show(&self) -> String {
        let (n, d) = self.parts();
        format!("{n}/{d}")
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
    #[inline]
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for Fraction {
    #[inline(always)]
    fn cmp(&self, o: &Self) -> Ordering {
        if self.0 == o.0 {
            return Ordering::Equal;
        }
        let r1 = self.0.get();
        let r2 = o.0.get();
        if ((r1 | r2) & WIDE_FLAG) == 0 {
            let n1 = decode_inline_n(r1);
            let n2 = decode_inline_n(r2);
            let d1 = (r1 & DEN_MASK) as i64;
            let d2 = (r2 & DEN_MASK) as i64;
            if d1 == d2 {
                return n1.cmp(&n2);
            }
            // `|n| <= 2^38` and `d < 2^24`, so `|n * d| < 2^62 < i64::MAX`.
            return (n1 * d2).cmp(&(n2 * d1));
        }
        self.cmp_wide(*o)
    }
}

impl Fraction {
    #[cold]
    #[inline(never)]
    fn cmp_wide(self, o: Self) -> Ordering {
        let (sn, sd) = self.parts();
        let (on, od) = o.parts();
        // Cross-multiply on the `i128` path; both denominators are positive
        // by normalisation. Fall back to the exact continued-fraction compare
        // when either product is outside i128.
        match (sn.checked_mul(od), on.checked_mul(sd)) {
            (Some(left), Some(right)) => left.cmp(&right),
            _ => self
                .checked_cmp(o)
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

    const FRACTION_JS_LIMIT: i64 = 10_000_000;

    /// The walk of fraction.js, one move at a time, in terms as wide as a
    /// fraction holds.
    fn scalar_farey_approximation(p1: f64, limit: i64) -> (i64, i64) {
        let limit = i128::from(limit);
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
        let word = |term: i128| i64::try_from(term).expect("a term of a machine word");
        (word(n), word(den))
    }

    #[test]
    fn batched_farey_walk_matches_the_scalar_state_machine() {
        const LIMIT: i64 = 1_024;
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
            // The limit of fraction.js, where the terms are the largest.
            if index % 8 == 0 {
                assert_eq!(
                    farey_approximation(value, FRACTION_JS_LIMIT),
                    scalar_farey_approximation(value, FRACTION_JS_LIMIT),
                    "sample {index} at the limit of fraction.js: {value:.17}"
                );
            }
        }
    }

    #[test]
    fn near_zero_random_offsets_take_bounded_farey_runs() {
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
            a.numer().checked_mul(b.denom()).is_none()
                || b.numer().checked_mul(a.denom()).is_none(),
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
            (a.numer() as f64 / a.denom() as f64)
                .partial_cmp(&(b.numer() as f64 / b.denom() as f64)),
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

#[cfg(test)]
mod native_fast_path_tests {
    use super::*;

    /// Reduce `n / d` with a plain gcd, as the reference for the fast paths.
    fn reference(n: i128, d: i128) -> Fraction {
        let divisor = gcd_i128(n, d).max(1);
        Fraction::new(n / divisor, d / divisor)
    }

    #[test]
    fn small_sums_differences_and_orders_match_a_plain_reduction() {
        // Powers of two, other denominators, whole numbers, negative terms
        // and terms at the edge of the 64-bit path.
        let denoms = [1, 2, 3, 4, 6, 8, 12, 16, 64, 80, 1 << 30, (1 << 31) - 1];
        let numers = [
            -(1 << 31) + 1,
            -(1 << 31),
            -(1 << 30) - 3,
            -17,
            -8,
            -1,
            0,
            1,
            3,
            5,
            8,
            24,
            1 << 29,
            (1 << 31) - 2,
            (1 << 31) - 1,
            1 << 31,
        ];
        let times: Vec<Fraction> = denoms
            .iter()
            .flat_map(|&d| numers.iter().map(move |&n| Fraction::new(n, d)))
            .collect();
        for &a in &times {
            for &b in &times {
                let (an, ad, bn, bd) = (a.numer(), a.denom(), b.numer(), b.denom());
                assert_eq!(
                    a + b,
                    reference(an * bd + bn * ad, ad * bd),
                    "{a:?} + {b:?}"
                );
                assert_eq!(
                    a - b,
                    reference(an * bd - bn * ad, ad * bd),
                    "{a:?} - {b:?}"
                );
                assert_eq!(a * b, reference(an * bn, ad * bd), "{a:?} * {b:?}");
                assert_eq!(a.cmp(&b), (an * bd).cmp(&(bn * ad)), "{a:?} cmp {b:?}");
            }
        }
    }

    #[test]
    fn signed_ticks_reduce_on_each_grid() {
        for d in [1, 2, 12, 128, 257, 1 << 31, u32::MAX] {
            let grid = NonZeroU32::new(d).unwrap();
            let edges = [
                i128::MIN,
                i128::MIN + 1,
                i128::MAX,
                i128::from(i64::MIN),
                i128::from(i64::MIN) + 1,
                i128::from(i64::MAX),
                i128::from(i64::MAX) + 1,
            ];
            for tick in (-260..=260).chain(edges) {
                assert_eq!(
                    Fraction::from_ticks(tick, grid),
                    Fraction::new(tick, i128::from(d)),
                    "{tick}/{d}"
                );
            }
        }
    }

    #[test]
    fn adding_whole_cycles_agrees_with_exact_addition_at_the_edges() {
        let cases = [
            (Fraction::new(i128::MIN + 1, 2), 1i128 << 126),
            (Fraction::new(3, 7), i128::MAX / 7),
            (Fraction::new(3, 7), i128::MAX / 7 + 1),
            (Fraction::new(-5, 3), i128::from(i64::MIN)),
            (Fraction::new(5, 3), i128::from(i64::MAX)),
            (Fraction::new(1, i128::MAX), 2),
            (Fraction::new(i128::MAX, 1), 1),
            (Fraction::new(i128::MIN + 1, 1), -1),
            (Fraction::new(1, 2), 0),
        ];
        for (fraction, cycles) in cases {
            assert_eq!(
                fraction.checked_add_int(cycles),
                fraction.checked_add(Fraction::int(cycles)),
                "{fraction:?} + {cycles}"
            );
        }
    }

    #[test]
    fn a_whole_side_adds_and_subtracts_as_big_integers_do() {
        let fractions = [
            Fraction::new(3, 7),
            Fraction::new(-5, 3),
            Fraction::new(1, i128::MAX),
            Fraction::new(i128::MAX, 2),
            Fraction::new(i128::MIN + 1, 2),
            Fraction::int(0),
            Fraction::int(5),
            Fraction::int(i128::MAX),
            Fraction::int(i128::MIN),
        ];
        let wholes = [0i128, 1, -1, 7, i128::MAX / 7 + 1, i128::MAX, i128::MIN];
        let big = |f: Fraction| (BigInt::from(f.numer()), BigInt::from(f.denom()));
        for fraction in fractions {
            for whole in wholes.map(Fraction::int) {
                let (n, d) = big(fraction);
                let (k, _) = big(whole);
                for (left, right, got) in [
                    (&n + &k * &d, d.clone(), fraction.checked_add(whole)),
                    (&k * &d + &n, d.clone(), whole.checked_add(fraction)),
                    (&n - &k * &d, d.clone(), fraction.checked_sub(whole)),
                    (&k * &d - &n, d.clone(), whole.checked_sub(fraction)),
                ] {
                    assert_eq!(
                        got,
                        fraction_from_bigints(left, right),
                        "{fraction:?} and {whole:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_whole_counts_around_a_quotient_agree_with_the_reduced_quotient() {
        let terms = [
            Fraction::ZERO,
            Fraction::ONE,
            Fraction::new(-1, 1),
            Fraction::new(13, 96),
            Fraction::new(-13, 96),
            Fraction::new(60, 1),
            Fraction::new(-60, 1),
            Fraction::new(7, 3),
            Fraction::new(-7, 3),
            Fraction::new((1 << 31) - 1, 1),
            Fraction::new(1, (1 << 31) - 1),
            Fraction::new(1 << 31, 3),
            Fraction::new(-5, 1 << 40),
            Fraction::new(i128::MAX, 2),
            Fraction::new(i128::MIN + 1, 1),
        ];
        for left in terms {
            for right in terms {
                let quotient = left.checked_div(right);
                assert_eq!(
                    left.checked_div_floor(right),
                    quotient.map(|quotient| quotient.floor().numer()),
                    "floor of {left:?} / {right:?}"
                );
                assert_eq!(
                    left.checked_div_ceil(right),
                    quotient.map(|quotient| quotient.ceil().numer()),
                    "ceil of {left:?} / {right:?}"
                );
            }
        }
    }

    #[test]
    fn a_64_bit_fraction_converts_as_a_128_bit_one() {
        for (n, d) in [
            (1i64, 3i64),
            (-7, 2),
            (i64::MAX, 3),
            (i64::MIN + 1, 7),
            (9_007_199_254_740_993, 1),
            (1, i64::MAX),
        ] {
            let wide = i128::from(n) as f64 / i128::from(d) as f64;
            assert_eq!(
                Fraction::new(i128::from(n), i128::from(d))
                    .to_f64()
                    .to_bits(),
                wide.to_bits(),
                "{n}/{d}"
            );
        }
    }

    #[test]
    fn a_dyadic_fraction_is_the_reduced_one() {
        for shift in 0..=126u32 {
            for n in [
                -96i128,
                -7,
                -1,
                0,
                1,
                2,
                3,
                8,
                64,
                1 << 40,
                i128::MAX,
                i128::MIN + 1,
            ] {
                let expected = Fraction::checked_new(n, 1i128 << shift);
                assert_eq!(Fraction::dyadic(n, shift), expected, "{n} over 2^{shift}");
            }
        }
        assert_eq!(Fraction::dyadic(1, 127), None);
    }

    #[test]
    fn ticks_have_the_same_reduced_time_as_exact_division() {
        for denom in [1, 8, 11, 15, 96, 176, 257, TickGrid::MAX_DENOM] {
            let grid = TickGrid::new(denom).unwrap();
            assert_eq!(grid.denom(), denom);
            for tick in (-2 * denom..=2 * denom).chain([i64::MIN, i64::MIN + 1, i64::MAX]) {
                let exact = Fraction::new(i128::from(tick), i128::from(denom));
                assert_eq!(grid.time(tick), exact, "tick {tick} of {denom}");
                assert_eq!(grid.ticks(exact), Some(tick));
            }
        }
    }

    #[test]
    fn a_grid_accepts_only_its_representable_ticks() {
        for denom in [-1, 0, TickGrid::MAX_DENOM + 1, i64::MAX] {
            assert!(TickGrid::new(denom).is_none());
        }
        let grid = TickGrid::new(24).unwrap();
        for d in 1..=32 {
            for n in [-13, 0, 7, i128::from(i64::MAX), i128::from(i64::MIN)] {
                let time = Fraction::new(n, d);
                let scaled = time * Fraction::int(24);
                let expected = (scaled.denom() == 1)
                    .then(|| i64::try_from(scaled.numer()).ok())
                    .flatten();
                assert_eq!(grid.ticks(time), expected, "{time:?}");
            }
        }
        assert_eq!(grid.ticks(Fraction::int(i128::MAX)), None);
        assert_eq!(grid.ticks(Fraction::new(1, i128::MAX)), None);
    }

    #[test]
    fn wide_store_returns_none_when_full_without_panicking() {
        let store = WideStore::new();
        for i in 0..WIDE_SLAB_LEN {
            let n = i128::from(INLINE_NUM_MAX) + 1 + i as i128;
            let got = store.try_intern(n, 1, 1);
            assert!(got.is_some(), "slot {i} should fit in one slab");
        }
        // Existing interned pair still resolves via dedup even when full.
        let existing = store.try_intern(i128::from(INLINE_NUM_MAX) + 1, 1, 1);
        assert!(existing.is_some());
        // A new wide value past capacity returns None instead of panicking.
        let overflow =
            store.try_intern(i128::from(INLINE_NUM_MAX) + 1 + WIDE_SLAB_LEN as i128, 1, 1);
        assert!(overflow.is_none());
    }
}
