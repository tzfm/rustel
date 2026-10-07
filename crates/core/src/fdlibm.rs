/*
fdlibm.rs - fdlibm-exact sin/cos for signal fidelity
Adapted from Sun fdlibm and V8's src/base/ieee754.cc:
Copyright (C) 1993 by Sun Microsystems, Inc. All rights reserved.
Copyright 2016 the V8 project authors. All rights reserved.
See crates/core/LICENSE-fdlibm-v8 for the original permission terms.

Rust port and additions:
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! `Math.sin`/`Math.cos` exactly as V8's fdlibm computes them.
//!
//! V8 implements `Math.sin`/`Math.cos` with its own port of Sun's fdlibm
//! (`src/base/ieee754.cc`), which differs from the platform libm (and from
//! MUSL's fdlibm descendant) by one ULP on some inputs - e.g.
//! `Math.sin(2π · 25/8)` is `0x3fe6a09e667f3bce` under Node while glibc and
//! the `libm` crate produce `...3bcd`. One ULP through `range(500, 4000)`
//! flips the printed hap value, so continuous signals must use the same
//! reduction and kernels as V8.
//!
//! This is the classic fdlibm structure: `__kernel_sin`, `__kernel_cos`, and
//! `__ieee754_rem_pio2`'s medium-argument path (|x| ≤ 2^19·π/2, covering
//! ~131 000 pattern cycles - beyond any real session). Larger arguments fall
//! back to the platform `sin`/`cos`: the Payne-Hanek reduction is not ported,
//! and the fallback is documented behavior, not silent drift. Bit-exactness
//! against the pinned Node is asserted by `matches_node_sweep_cases` below
//! and was verified over 10⁷ uniformly sampled arguments at porting time.

// The constants below are transcribed digit for digit from fdlibm, each with
// the IEEE-754 bit pattern it must round to in the comment beside it. Several
// carry more decimal digits than an f64 can hold, which is the point: they are
// the reference source's own text, and shortening them to what the type can
// distinguish would silently substitute our arithmetic for the one being
// reproduced. The hex comments are the check that survives.
//
// `INV_PIO2` is 2/π to fdlibm's digits, which is not `f64::consts::FRAC_2_PI`
// to the last bit; substituting the std constant is exactly the one-ULP drift
// this module exists to avoid.
#![allow(clippy::excessive_precision, clippy::approx_constant)]

const HALF: f64 = 5.000_000_000_000_000_00e-01;
const INV_PIO2: f64 = 6.366_197_723_675_813_824_33e-01; /* 0x3FE45F30, 0x6DC9C883 */
const PIO2_1: f64 = 1.570_796_326_734_125_614_17e+00; /* 0x3FF921FB, 0x54400000 */
const PIO2_1T: f64 = 6.077_100_506_506_192_249_32e-11; /* 0x3DD0B461, 0x1A626331 */
const PIO2_2: f64 = 6.077_100_506_303_965_976_60e-11; /* 0x3DD0B461, 0x1A600000 */
const PIO2_2T: f64 = 2.022_266_248_795_950_631_54e-21; /* 0x3BA3198A, 0x2E037073 */
const PIO2_3: f64 = 2.022_266_248_711_166_455_80e-21; /* 0x3BA3198A, 0x2E000000 */
const PIO2_3T: f64 = 8.478_427_660_368_899_569_97e-32; /* 0x397B839A, 0x252049C1 */

const S1: f64 = -1.666_666_666_666_663_243_48e-01; /* 0xBFC55555, 0x55555549 */
const S2: f64 = 8.333_333_333_322_489_461_24e-03; /* 0x3F811111, 0x1110F8A6 */
const S3: f64 = -1.984_126_982_985_794_931_34e-04; /* 0xBF2A01A0, 0x19C161D5 */
const S4: f64 = 2.755_731_370_707_006_767_89e-06; /* 0x3EC71DE3, 0x57B1FE7D */
const S5: f64 = -2.505_076_025_340_686_341_95e-08; /* 0xBE5AE5E6, 0x8A2B9CEB */
const S6: f64 = 1.589_690_995_211_550_102_21e-10; /* 0x3DE5D93A, 0x5ACFD57C */

const C1: f64 = 4.166_666_666_666_660_190_37e-02; /* 0x3FA55555, 0x5555554C */
const C2: f64 = -1.388_888_888_887_410_957_49e-03; /* 0xBF56C16C, 0x16C15177 */
const C3: f64 = 2.480_158_728_947_672_941_78e-05; /* 0x3EFA01A0, 0x19CB1590 */
const C4: f64 = -2.755_731_435_139_066_330_35e-07; /* 0xBE927E4F, 0x809C52AD */
const C5: f64 = 2.087_572_321_298_174_827_90e-09; /* 0x3E21EE9E, 0xBDB4B1C4 */
const C6: f64 = -1.135_964_755_778_819_482_65e-11; /* 0xBDA8FAE9, 0xBE8838D4 */

#[inline]
fn high_word(x: f64) -> u32 {
    (x.to_bits() >> 32) as u32
}

/// `__kernel_sin(x, y, iy)`: sine on |x| ≤ π/4, `x + y` the split argument.
fn kernel_sin(x: f64, y: f64, iy: i32) -> f64 {
    let ix = high_word(x) & 0x7fff_ffff;
    if ix < 0x3e40_0000 {
        // |x| < 2^-27: generate inexact like fdlibm, value is x itself.
        if x as i32 == 0 {
            return x;
        }
    }
    let z = x * x;
    let v = z * x;
    let r = S2 + z * (S3 + z * (S4 + z * (S5 + z * S6)));
    if iy == 0 {
        x + v * (S1 + z * r)
    } else {
        x - ((z * (HALF * y - v * r) - y) - v * S1)
    }
}

/// `__kernel_cos(x, y)`: cosine on |x| ≤ π/4, `x + y` the split argument.
fn kernel_cos(x: f64, y: f64) -> f64 {
    let ix = high_word(x) & 0x7fff_ffff;
    if ix < 0x3e40_0000 {
        // |x| < 2^-27
        if x as i32 == 0 {
            return 1.0;
        }
    }
    let z = x * x;
    let r = z * (C1 + z * (C2 + z * (C3 + z * (C4 + z * (C5 + z * C6)))));
    if ix < 0x3fd3_3333 {
        // |x| < 0.3
        1.0 - (0.5 * z - (z * r - x * y))
    } else {
        let qx = if ix > 0x3fe9_0000 {
            // x > 0.78125
            0.28125
        } else {
            // qx = x/4 exactly via the high word, like fdlibm's __HI(qx).
            f64::from_bits((u64::from(ix - 0x0020_0000)) << 32)
        };
        let hz = 0.5 * z - qx;
        let a = 1.0 - qx;
        a - (hz - (z * r - x * y))
    }
}

/// `__ieee754_rem_pio2` medium path: reduce x to y0+y1 in [-π/4, π/4],
/// returning n with `x ≡ y + n·π/2`. `None` when |x| needs Payne-Hanek.
fn rem_pio2(x: f64) -> Option<(i32, f64, f64)> {
    let hx = high_word(x) as i32;
    let ix = hx & 0x7fff_ffff;
    if ix <= 0x3fe9_21fb {
        // |x| ≤ π/4 - no reduction.
        return Some((0, x, 0.0));
    }
    if ix > 0x4139_21fb {
        // |x| > 2^19·π/2 - Payne-Hanek territory (not ported).
        return None;
    }
    let t = x.abs();
    let n = (t * INV_PIO2 + HALF) as i32;
    let f_n = f64::from(n);
    let mut r = t - f_n * PIO2_1;
    let mut w = f_n * PIO2_1T;
    // fdlibm skips the cancellation checks when n < 32 and the high word of
    // x is NOT that of a near multiple of π/2 (its npio2_hw table). Running
    // the checks unconditionally is bit-identical - the first iteration's
    // y0 is the same expression - just never skips the safety net.
    let j = ix >> 20;
    let mut y0 = r - w;
    let mut i = j - (((high_word(y0) >> 20) & 0x7ff) as i32);
    if i > 16 {
        // 2nd iteration needed, good to 118 bits.
        let t2 = r;
        w = f_n * PIO2_2;
        r = t2 - w;
        w = f_n * PIO2_2T - ((t2 - r) - w);
        y0 = r - w;
        i = j - (((high_word(y0) >> 20) & 0x7ff) as i32);
        if i > 49 {
            // 3rd iteration, 151 bits.
            let t3 = r;
            w = f_n * PIO2_3;
            r = t3 - w;
            w = f_n * PIO2_3T - ((t3 - r) - w);
            y0 = r - w;
        }
    }
    let y1 = (r - y0) - w;
    if hx < 0 {
        Some((-n, -y0, -y1))
    } else {
        Some((n, y0, y1))
    }
}

/// `Math.sin` as V8's fdlibm computes it.
pub fn sin(x: f64) -> f64 {
    let ix = high_word(x) & 0x7fff_ffff;
    if ix <= 0x3fe9_21fb {
        return kernel_sin(x, 0.0, 0);
    }
    if ix >= 0x7ff0_0000 {
        // Inf or NaN.
        return f64::NAN;
    }
    match rem_pio2(x) {
        Some((n, y0, y1)) => match n & 3 {
            0 => kernel_sin(y0, y1, 1),
            1 => kernel_cos(y0, y1),
            2 => -kernel_sin(y0, y1, 1),
            _ => -kernel_cos(y0, y1),
        },
        None => x.sin(),
    }
}

/// `Math.cos` as V8's fdlibm computes it.
pub fn cos(x: f64) -> f64 {
    let ix = high_word(x) & 0x7fff_ffff;
    if ix <= 0x3fe9_21fb {
        return kernel_cos(x, 0.0);
    }
    if ix >= 0x7ff0_0000 {
        return f64::NAN;
    }
    match rem_pio2(x) {
        Some((n, y0, y1)) => match n & 3 {
            0 => kernel_cos(y0, y1),
            1 => -kernel_sin(y0, y1, 1),
            2 => -kernel_cos(y0, y1),
            _ => kernel_sin(y0, y1, 1),
        },
        None => x.cos(),
    }
}

const LN2_HI: f64 = 6.931_471_803_691_238_164_90e-01; /* 0x3FE62E42, 0xFEE00000 */
const LN2_LO: f64 = 1.908_214_929_270_587_700_02e-10; /* 0x3DEA39EF, 0x35793C76 */
const INV_LN2: f64 = 1.442_695_040_888_963_387_00e+00; /* 0x3FF71547, 0x652B82FE */
const EXP_P1: f64 = 1.666_666_666_666_660_190_37e-01; /* 0x3FC55555, 0x5555553E */
const EXP_P2: f64 = -2.777_777_777_701_559_338_42e-03; /* 0xBF66C16C, 0x16BEBD93 */
const EXP_P3: f64 = 6.613_756_321_437_934_361_17e-05; /* 0x3F11566A, 0xAF25DE2C */
const EXP_P4: f64 = -1.653_390_220_546_525_153_90e-06; /* 0xBEBBBD41, 0xC5D26BF1 */
const EXP_P5: f64 = 4.138_136_797_057_238_460_39e-08; /* 0x3E663769, 0x72BEA4D0 */
const O_THRESHOLD: f64 = 7.097_827_128_933_839_730_96e+02;
const U_THRESHOLD: f64 = -7.451_332_191_019_411_084_20e+02;
const TWOM1000: f64 = 9.332_636_185_032_188_789_90e-302;
const HUGE: f64 = 1.0e+300;

const LG1: f64 = 6.666_666_666_666_735_130e-01; /* 0x3FE55555, 0x55555593 */
const LG2: f64 = 3.999_999_999_940_941_908e-01; /* 0x3FD99999, 0x9997FA04 */
const LG3: f64 = 2.857_142_874_366_239_149e-01; /* 0x3FD24924, 0x94229359 */
const LG4: f64 = 2.222_219_843_214_978_396e-01; /* 0x3FCC71C5, 0x1D8E78AF */
const LG5: f64 = 1.818_357_216_161_805_012e-01; /* 0x3FC74664, 0x96CB03DE */
const LG6: f64 = 1.531_383_769_920_937_332e-01; /* 0x3FC39A09, 0xD078C69F */
const LG7: f64 = 1.479_819_860_511_658_591e-01; /* 0x3FC2F112, 0xDF3E5244 */

#[inline]
fn with_high_word(x: f64, hi: u32) -> f64 {
    f64::from_bits((u64::from(hi) << 32) | (x.to_bits() & 0xffff_ffff))
}

/// `Math.exp` as V8's fdlibm `__ieee754_exp` computes it.
pub fn exp(x: f64) -> f64 {
    // V8 special-cases exp(1): fdlibm's polynomial lands 1 ULP above the
    // correctly rounded E, and Math.exp(1) must equal Math.E.
    if x == 1.0 {
        return std::f64::consts::E;
    }
    let hx_signed = high_word(x) as i32;
    let xsb = ((hx_signed >> 31) & 1) as usize;
    let hx = (hx_signed & 0x7fff_ffff) as u32;
    if hx >= 0x4086_2e42 {
        // |x| >= 709.78...
        if hx >= 0x7ff0_0000 {
            let lx = x.to_bits() as u32;
            if ((hx & 0xf_ffff) | lx) != 0 {
                return x + x; // NaN
            }
            return if xsb == 0 { x } else { 0.0 }; // exp(±inf)
        }
        if x > O_THRESHOLD {
            return HUGE * HUGE; // overflow
        }
        if x < U_THRESHOLD {
            return TWOM1000 * TWOM1000; // underflow
        }
    }
    let mut hi = 0.0f64;
    let mut lo = 0.0f64;
    let mut k: i32 = 0;
    let mut x = x;
    if hx > 0x3fd6_2e42 {
        // |x| > 0.5 ln2
        if hx < 0x3ff0_a2b2 {
            // |x| < 1.5 ln2
            hi = x - if xsb == 0 { LN2_HI } else { -LN2_HI };
            lo = if xsb == 0 { LN2_LO } else { -LN2_LO };
            k = 1 - (xsb as i32) - (xsb as i32);
        } else {
            let half = if xsb == 0 { 0.5 } else { -0.5 };
            k = (INV_LN2 * x + half) as i32;
            let t = f64::from(k);
            hi = x - t * LN2_HI;
            lo = t * LN2_LO;
        }
        x = hi - lo;
    } else if hx < 0x3e30_0000 {
        // |x| < 2^-28
        if HUGE + x > 1.0 {
            return 1.0 + x;
        }
    }
    let t = x * x;
    let c = x - t * (EXP_P1 + t * (EXP_P2 + t * (EXP_P3 + t * (EXP_P4 + t * EXP_P5))));
    if k == 0 {
        return 1.0 - ((x * c) / (c - 2.0) - x);
    }
    let y = 1.0 - ((lo - (x * c) / (2.0 - c)) - hi);
    if k >= -1021 {
        let hy = high_word(y);
        with_high_word(y, hy.wrapping_add((k as u32) << 20))
    } else {
        let hy = high_word(y);
        with_high_word(y, hy.wrapping_add(((k + 1000) as u32) << 20)) * TWOM1000
    }
}

/// `Math.log` as V8's fdlibm `__ieee754_log` computes it.
pub fn log(x: f64) -> f64 {
    let mut x = x;
    let mut hx = high_word(x) as i32;
    let lx = x.to_bits() as u32;
    let mut k: i32 = 0;
    if hx < 0x0010_0000 {
        // x < 2^-1022: zero, negative, or subnormal.
        if ((hx & 0x7fff_ffff) as u32 | lx) == 0 {
            return f64::NEG_INFINITY;
        }
        if hx < 0 {
            return f64::NAN;
        }
        k -= 54;
        x *= 1.801_439_850_948_198_4e16; // 2^54
        hx = high_word(x) as i32;
    }
    if hx >= 0x7ff0_0000 {
        return x + x;
    }
    k += (hx >> 20) - 1023;
    hx &= 0x000f_ffff;
    let i = (hx + 0x9_5f64) & 0x10_0000;
    x = with_high_word(x, (hx | (i ^ 0x3ff0_0000)) as u32);
    k += i >> 20;
    let f = x - 1.0;
    if (0x000f_ffff & (2 + hx)) < 3 {
        // |f| < 2^-20
        if f == 0.0 {
            if k == 0 {
                return 0.0;
            }
            let dk = f64::from(k);
            return dk * LN2_HI + dk * LN2_LO;
        }
        let r = f * f * (0.5 - 0.333_333_333_333_333_33 * f);
        if k == 0 {
            return f - r;
        }
        let dk = f64::from(k);
        return dk * LN2_HI - ((r - dk * LN2_LO) - f);
    }
    let s = f / (2.0 + f);
    let dk = f64::from(k);
    let z = s * s;
    let mut i = hx - 0x6_147a;
    let w = z * z;
    let j = 0x6_b851 - hx;
    let t1 = w * (LG2 + w * (LG4 + w * LG6));
    let t2 = z * (LG1 + w * (LG3 + w * (LG5 + w * LG7)));
    i |= j;
    let r = t2 + t1;
    if i > 0 {
        let hfsq = 0.5 * f * f;
        if k == 0 {
            f - (hfsq - s * (hfsq + r))
        } else {
            dk * LN2_HI - ((hfsq - (s * (hfsq + r) + dk * LN2_LO)) - f)
        }
    } else if k == 0 {
        f - s * (f - r)
    } else {
        dk * LN2_HI - ((s * (f - r) - dk * LN2_LO) - f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bits pinned from the pinned Node (`Buffer` dump of `Math.sin`/`cos`).
    /// The first case is the ear-visible one: sine at cycle 25/8 through
    /// `range(500, 4000)` printed one ULP off before the port.
    #[test]
    fn matches_node_sweep_cases() {
        let tau = std::f64::consts::TAU;
        let cases: &[(f64, u64, u64)] = &[
            // (x, Math.sin bits, Math.cos bits) - dumped from the pinned Node.
            (tau * (25.0 / 8.0), 0x3fe6a09e667f3bce, 0x3fe6a09e667f3bcc),
            (tau * (51.0 / 16.0), 0x3fed906bcf328d42, 0x3fd87de2a6aea977),
            (tau * 0.125, 0x3fe6a09e667f3bcc, 0x3fe6a09e667f3bcd),
            (tau * 1000.25, 0x3ff0000000000000, 0x3d60ceadd8228992),
            (19.634954084936208, 0x3fe6a09e667f3bce, 0x3fe6a09e667f3bcc),
        ];
        for &(x, sin_bits, cos_bits) in cases {
            assert_eq!(
                sin(x).to_bits(),
                sin_bits,
                "sin({x}) diverged from the pinned Node"
            );
            assert_eq!(
                cos(x).to_bits(),
                cos_bits,
                "cos({x}) diverged from the pinned Node"
            );
        }
        // exp/log pins, including V8's exp(1) special case and the rangex
        // chain that printed cutoff:1414.2135623730949 instead of ...46.
        assert_eq!(exp(1.0).to_bits(), std::f64::consts::E.to_bits());
        let lo = log(500.0);
        let hi = log(4000.0);
        assert_eq!(
            exp(0.5 * (hi - lo) + lo).to_bits(),
            1414.2135623730946f64.to_bits()
        );
    }
}
