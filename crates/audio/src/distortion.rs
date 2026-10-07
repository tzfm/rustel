//! Waveshaping distortion: nine named algorithms, the shape amount
//! pre-warped as `expm1(distort)` and output scaled by the clamped
//! `distortvol` post-gain. The transfer function is evaluated exactly
//! once per sample with no oversampling.

/// Algorithm ids in registry order; `distorttype`
/// indexes modulo the table, exactly like `getDistortionAlgorithm`.
pub const DISTORTION_ALGORITHMS: [&str; 9] = [
    "scurve",
    "soft",
    "hard",
    "cubic",
    "diode",
    "asym",
    "fold",
    "sinefold",
    "chebyshev",
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DistortControls {
    /// Raw `distort` control value; the per-sample shape is `expm1(amount)`.
    pub amount: f32,
    /// `distortvol` clamped to a 0.001..=1 post-gain window.
    pub postgain: f32,
    /// Index into [`DISTORTION_ALGORITHMS`] (already reduced modulo 9).
    pub algorithm: u8,
}

impl DistortControls {
    /// The per-sample transfer, `postgain * algorithm(x, expm1(amount))`.
    ///
    /// The result is always finite. Several curves can overflow: `chebyshev`
    /// runs a 63-term recurrence that diverges for `|x| > 1`, and `inf - inf`
    /// there is `NaN`. One non-finite sample that reaches the orbit's
    /// feedback delay stays in the line, because `NaN * feedback` is `NaN`
    /// for any feedback. The orbit then stays silent, even after the score
    /// changes. This function returns silence for that sample instead.
    pub fn apply(&self, x: f32, shape: f32) -> f32 {
        let y = self.transfer(x, shape);
        if y.is_finite() { y } else { 0.0 }
    }

    fn transfer(&self, x: f32, shape: f32) -> f32 {
        let y = match self.algorithm {
            0 => scurve(x, shape),
            1 => soft(x, shape),
            2 => hard(x, shape),
            3 => cubic(x, shape),
            4 => diode(x, shape, false),
            5 => diode(x, shape, true),
            6 => fold(x, shape),
            7 => sinefold(x, shape),
            _ => chebyshev(x, shape),
        };
        self.postgain * y
    }

    /// The largest shape any curve here stays finite at.
    ///
    /// `expm1` overflows f32 above about 88, and every algorithm turns an
    /// infinite shape into `NaN` - `scurve` most directly, as
    /// `(inf * x) / (inf * |x|)`. The reference does this arithmetic in
    /// double, where `expm1(99)` is a perfectly finite `9.9e42` and the
    /// curve saturates into a hard square, so the fault is the narrower
    /// float rather than the value. Computing in `f64` and landing on a
    /// finite ceiling restores that: by `1e30` every curve has long since
    /// saturated, and the headroom left below f32's `3.4e38` is what keeps
    /// `(1 + k) * x` finite for a signal driven well above unity.
    const MAX_SHAPE: f32 = 1e30;

    pub fn shape(&self) -> f32 {
        Self::warp(f64::from(self.amount))
    }

    fn warp(amount: f64) -> f32 {
        let shape = amount.exp_m1();
        if shape > f64::from(Self::MAX_SHAPE) {
            Self::MAX_SHAPE
        } else if shape < f64::from(-Self::MAX_SHAPE) {
            -Self::MAX_SHAPE
        } else {
            shape as f32
        }
    }

    /// The shape for a modulated `distort`. `expm1(distort)` is recomputed
    /// per sample, so a modulator moves the amount and the curve follows -
    /// adding to the shaped value instead would bend a different function.
    pub fn shape_with(&self, added: f32) -> f32 {
        Self::warp(f64::from(self.amount) + f64::from(added))
    }

    /// Correction factor for a modulated `distortvol`, given that [`apply`]
    /// has already multiplied by the unmodulated postgain. The reference clamps
    /// the param to 0.001..=1, so the modulated value is clamped the same way
    /// before the ratio is taken.
    ///
    /// [`apply`]: Self::apply
    pub fn postgain_scale(&self, added: f32) -> f32 {
        let modulated = (self.postgain + added).clamp(0.001, 1.0);
        modulated / self.postgain
    }
}

#[inline]
fn clamp(x: f32, lo: f32, hi: f32) -> f32 {
    x.max(lo).min(hi)
}

/// `__squash`: [0, inf) → [0, 1).
#[inline]
fn squash(x: f32) -> f32 {
    x / (1.0 + x)
}

/// Wrapping remainder with the sign of `m` (double-mod idiom).
#[inline]
fn js_mod(n: f32, m: f32) -> f32 {
    ((n % m) + m) % m
}

#[inline]
fn scurve(x: f32, k: f32) -> f32 {
    ((1.0 + k) * x) / (1.0 + k * x.abs())
}

#[inline]
fn soft(x: f32, k: f32) -> f32 {
    (x * (1.0 + k)).tanh()
}

#[inline]
fn hard(x: f32, k: f32) -> f32 {
    clamp((1.0 + k) * x, -1.0, 1.0)
}

#[inline]
fn fold(x: f32, k: f32) -> f32 {
    let y = (1.0 + 0.5 * k) * x;
    let window = js_mod(y + 1.0, 4.0);
    1.0 - (window - 2.0).abs()
}

#[inline]
fn sinefold(x: f32, k: f32) -> f32 {
    ((std::f32::consts::PI / 2.0) * fold(x, k)).sin()
}

#[inline]
fn cubic(x: f32, k: f32) -> f32 {
    let t = squash(k.ln_1p());
    let cubic = (x - (t / 3.0) * x * x * x) / (1.0 - t / 3.0);
    soft(cubic, k)
}

#[inline]
fn diode(x: f32, k: f32, asym: bool) -> f32 {
    let g = 1.0 + 2.0 * k;
    let t = squash(k.ln_1p());
    let bias = 0.07 * t;
    let pos = soft(x + bias, 2.0 * k);
    let neg = soft(if asym { bias } else { -x + bias }, 2.0 * k);
    let y = pos - neg;
    let sech = 1.0 / (g * bias).cosh();
    let sech2 = sech * sech;
    let denom = ((if asym { 1.0 } else { 2.0 }) * g * sech2).max(1e-8);
    soft(y / denom, k)
}

#[inline]
fn chebyshev(x: f32, k: f32) -> f32 {
    let kl = 10.0 * k.ln_1p();
    let mut tnm1 = 1.0f32;
    let mut tnm2 = x;
    let mut y = 0.0f32;
    for i in 1..64 {
        if i < 2 {
            y += if i == 0 { tnm1 } else { tnm2 };
            continue;
        }
        let tn = 2.0 * x * tnm1 - tnm2;
        tnm2 = tnm1;
        tnm1 = tn;
        if i % 2 == 0 {
            y += ((1.3 * kl) / i as f32).min(2.0) * tn;
        }
    }
    soft(y, kl / 20.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `distort(99)` must give a finite sample. `expm1(99)` overflows f32 to
    /// `inf`, and `scurve(x, inf)` is `(inf * x) / (1 + inf * |x|)`: `NaN` for
    /// every input, silence included. That `NaN` stays in the orbit's
    /// feedback delay, so the orbit stays silent.
    #[test]
    fn no_amount_of_distortion_can_produce_a_sample_that_is_not_a_number() {
        for algorithm in 0..DISTORTION_ALGORITHMS.len() as u8 {
            let controls = DistortControls {
                amount: 0.0,
                postgain: 1.0,
                algorithm,
            };
            let name = DISTORTION_ALGORITHMS[usize::from(algorithm)];
            for amount in [
                0.0, 0.5, 1.0, 8.0, 20.0, 87.0, 88.0, 89.0, 99.0, 500.0, 1e6, -4.0,
            ] {
                let shaped = DistortControls { amount, ..controls };
                let shape = shaped.shape();
                assert!(
                    shape.is_finite(),
                    "{name}: distort({amount}) warped to {shape}"
                );
                // Well past unity too: a voice reaches the shaper after its
                // own gain, and `chebyshev` diverges hardest above one.
                for x in [
                    0.0, 1e-9, 0.25, -0.25, 0.999, 1.0, -1.0, 1.5, -1.5, 8.0, -8.0, 100.0,
                ] {
                    let y = shaped.apply(x, shape);
                    assert!(y.is_finite(), "{name}: distort({amount}) on {x} gave {y}");
                }
            }
        }
    }

    /// The browser computes this in double, where `expm1(99)` is finite and
    /// the curve saturates into a hard square. Matching that is the point of
    /// the ceiling: loud, not silent.
    #[test]
    fn an_enormous_distort_saturates_the_way_the_reference_does() {
        let controls = DistortControls {
            amount: 99.0,
            postgain: 1.0,
            algorithm: 0,
        };
        let shape = controls.shape();
        for (x, expected) in [(0.5f32, 1.0f32), (-0.5, -1.0), (0.01, 1.0)] {
            let y = controls.apply(x, shape);
            assert!(
                (y - expected).abs() < 1e-3,
                "scurve at distort(99) should square up: {x} -> {y}"
            );
        }
        assert_eq!(controls.apply(0.0, shape), 0.0, "silence stays silent");

        // And an ordinary amount is untouched by any of this.
        let gentle = DistortControls {
            amount: 2.0,
            ..controls
        };
        assert!((gentle.shape() - 2.0f32.exp_m1()).abs() < 1e-4);
    }
}
