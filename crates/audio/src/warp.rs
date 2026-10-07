//! Wavetable phase warping.
//!
//! The oscillator reads its table at a warped phase rather than the raw one,
//! which is what lets one table make many timbres. Twenty-one modes, kept
//! shape-for-shape rather than tidied, because each is a waveform someone can
//! hear: "simplifying" `PRIMES` into `QUANT` would silently retune a patch.
//!
//! Two rounding helpers do real work here and are NOT interchangeable:
//! `ffloor` truncates TOWARD ZERO, while `frac` floors toward negative
//! infinity. Modes mixing the two would drift apart on negative inputs if
//! both became `floor`.

/// Truncation toward zero, not `floor`.
#[inline]
fn ffloor(x: f32) -> i32 {
    x as i32
}

/// `ffloor(x + 0.5)` - round half up on the truncating floor.
#[inline]
fn fround(x: f32) -> i32 {
    ffloor(x + 0.5)
}

/// `x - ffloor(x)`, so it follows truncation.
#[inline]
fn ffrac(x: f32) -> f32 {
    x - ffloor(x) as f32
}

/// `x - x.floor()` - the true fractional part.
#[inline]
fn frac(x: f32) -> f32 {
    x - x.floor()
}

/// The 32-bit integer hash behind `BROWNIAN`'s value noise.
///
/// Written in i64 with an explicit narrowing to 32 bits at every bitwise
/// step: the sums can exceed 32 bits before each mask, and doing the
/// additions in i32 instead would overflow and change the noise.
fn hash32(seed: i32) -> u32 {
    #[inline]
    fn to_i32(x: i64) -> i32 {
        x as u32 as i32
    }
    let u = i64::from(seed);
    let u = u + 0x7ed5_5d16 + i64::from(to_i32(u).wrapping_shl(12));
    let u = i64::from(to_i32(u) ^ (0xc761_c23c_u32 as i32) ^ ((to_i32(u) as u32 >> 19) as i32));
    let u = u + 0x1656_67b1 + i64::from(to_i32(u).wrapping_shl(5));
    let u = i64::from(to_i32(u + 0xd3a2_646c) ^ to_i32(u).wrapping_shl(9));
    let u = u + 0xfd70_46c5 + i64::from(to_i32(u).wrapping_shl(3));
    let u = to_i32(u) ^ (0xb55a_4f09_u32 as i32) ^ ((to_i32(u) as u32 >> 16) as i32);
    u as u32
}

#[inline]
fn hash01(i: i32) -> f32 {
    ((hash32(i) >> 8) as f32) / 16_777_216.0
}

/// Value noise: hash the integer lattice, lerp between neighbours.
fn noise(x: f32) -> f32 {
    let i = x.floor();
    let f = x - i;
    let i = i as i32;
    let a = hash01(i);
    let b = hash01(i.wrapping_add(1));
    a + (b - a) * f
}

/// Four octaves of value noise, normalised to [-1, 1].
fn brownian(x: f32, octaves: u32) -> f32 {
    let mut amp = 0.5f32;
    let mut sum = 0.0f32;
    let mut norm = 0.0f32;
    let mut freq = 1.0f32;
    for _ in 0..octaves {
        sum += amp * noise(x * freq);
        norm += amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    (sum / norm) * 2.0 - 1.0
}

fn bit_reverse(mut i: i32, n: i32) -> i32 {
    let mut r = 0i32;
    for _ in 0..n {
        r = (r << 1) | (i & 1);
        i = ((i as u32) >> 1) as i32;
    }
    r
}

#[inline]
fn mirror(x: f32) -> f32 {
    1.0 - (2.0 * x - 1.0).abs()
}

/// `_toBits`: a bit-depth that falls from `max` to `min` as the amount rises,
/// and the step count that implies.
fn to_bits(amt: f32, min: f32, max: f32) -> (f32, i32) {
    let b = max + (min - max) * amt;
    (b, fround(2.0f32.powf(b)))
}

#[inline]
fn js_clamp(value: f32, min: f32, max: f32) -> f32 {
    value.max(min).min(max)
}

/// The warp modes. The numbering is part of the control interface, because
/// `warpmode` accepts the index as well as the name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum WarpMode {
    None = 0,
    Asym = 1,
    Mirror = 2,
    BendP = 3,
    BendM = 4,
    BendMp = 5,
    Sync = 6,
    Quant = 7,
    Fold = 8,
    Pwm = 9,
    Orbit = 10,
    Spin = 11,
    Chaos = 12,
    Primes = 13,
    Binary = 14,
    Brownian = 15,
    Reciprocal = 16,
    Wormhole = 17,
    Logistic = 18,
    Sigmoid = 19,
    Fractal = 20,
    Flip = 21,
}

impl WarpMode {
    /// Anything outside the table is `NONE`; an unresolved name falls back
    /// the same way.
    pub fn from_index(index: i32) -> Self {
        use WarpMode::*;
        match index {
            1 => Asym,
            2 => Mirror,
            3 => BendP,
            4 => BendM,
            5 => BendMp,
            6 => Sync,
            7 => Quant,
            8 => Fold,
            9 => Pwm,
            10 => Orbit,
            11 => Spin,
            12 => Chaos,
            13 => Primes,
            14 => Binary,
            15 => Brownian,
            16 => Reciprocal,
            17 => Wormhole,
            18 => Logistic,
            19 => Sigmoid,
            20 => Fractal,
            21 => Flip,
            _ => None,
        }
    }

    /// `Warpmode[name.toUpperCase()]`, with the same fallback to `NONE`.
    pub fn from_name(name: &str) -> Self {
        use WarpMode::*;
        match name.to_ascii_uppercase().as_str() {
            "ASYM" => Asym,
            "MIRROR" => Mirror,
            "BENDP" => BendP,
            "BENDM" => BendM,
            "BENDMP" => BendMp,
            "SYNC" => Sync,
            "QUANT" => Quant,
            "FOLD" => Fold,
            "PWM" => Pwm,
            "ORBIT" => Orbit,
            "SPIN" => Spin,
            "CHAOS" => Chaos,
            "PRIMES" => Primes,
            "BINARY" => Binary,
            "BROWNIAN" => Brownian,
            "RECIPROCAL" => Reciprocal,
            "WORMHOLE" => Wormhole,
            "LOGISTIC" => Logistic,
            "SIGMOID" => Sigmoid,
            "FRACTAL" => Fractal,
            "FLIP" => Flip,
            _ => None,
        }
    }

    /// `FLIP` warps nothing; it inverts the SAMPLE for the first `amt` of the
    /// cycle instead, which the caller has to do because it is not a phase
    /// transform at all.
    #[inline]
    pub fn flips_sample(self, phase: f32, amt: f32) -> bool {
        self == WarpMode::Flip && phase < amt
    }
}

fn is_prime(n: i32) -> bool {
    if n < 2 {
        return false;
    }
    if n % 2 == 0 {
        return n == 2;
    }
    let mut d = 3i64;
    while d * d <= i64::from(n) {
        if i64::from(n) % d == 0 {
            return false;
        }
        d += 2;
    }
    true
}

/// `_warpPhase(phase, amt, mode)`.
pub fn warp_phase(phase: f32, amt: f32, mode: WarpMode) -> f32 {
    use WarpMode::*;
    let tau = std::f32::consts::TAU;
    match mode {
        None => phase,
        Asym => {
            let a = 0.01 + 0.99 * amt;
            // A raw f64 phase just below one can round to one in f32.
            // At full asymmetry the right branch has zero width; use the
            // left-hand limit instead of dividing 0 by 0.
            if phase < a || a == 1.0 {
                (0.5 * phase) / a
            } else {
                0.5 + (0.5 * (phase - a)) / (1.0 - a)
            }
        }
        Mirror => mirror(warp_phase(phase, amt, Asym)),
        BendP => phase.powf(1.0 + 3.0 * amt),
        BendM => phase.powf(1.0 / (1.0 + 3.0 * amt)),
        // The halves are modes 3 and 2, BENDP and MIRROR, and not the BENDM
        // that the name suggests. This is deliberate.
        BendMp => {
            if amt < 0.5 {
                warp_phase(phase, 1.0 - 2.0 * amt, BendP)
            } else {
                warp_phase(phase, 2.0 * amt - 1.0, Mirror)
            }
        }
        Sync => {
            let sync_ratio = 16.0f32.powf(amt * amt);
            (phase * sync_ratio) % 1.0
        }
        Quant => {
            let (_, n) = to_bits(amt, 2.0, 12.0);
            ffloor(phase * n as f32) as f32 / n as f32
        }
        Fold => {
            let k = 1 + fround(7.0 * amt).max(1);
            (ffrac(k as f32 * phase) - 0.5).abs() * 2.0
        }
        Pwm => {
            let w = js_clamp(0.5 + 0.49 * (2.0 * amt - 1.0), 0.0, 1.0);
            if phase < w {
                (phase / w) * 0.5
            } else {
                0.5 + ((phase - w) / (1.0 - w)) * 0.5
            }
        }
        Orbit => {
            let depth = 0.5 * amt;
            frac(phase + depth * (tau * 3.0 * phase).sin())
        }
        Spin => {
            let depth = 0.5 * amt;
            let (_, n) = to_bits(amt, 1.0, 6.0);
            frac(phase + depth * (tau * n as f32 * phase).sin())
        }
        Chaos => {
            let r = 3.7 + 0.3 * amt;
            let logistic = r * phase * (1.0 - phase);
            js_clamp((1.0 - amt) * phase + amt * logistic, 0.0, 1.0)
        }
        Primes => {
            let (_, mut n) = to_bits(amt, 3.0, 12.0);
            while !is_prime(n) {
                n += 1;
            }
            ffloor(phase * n as f32) as f32 / n as f32
        }
        Binary => {
            let (b, _) = to_bits(amt, 3.0, 12.0);
            let b = fround(b);
            let n = 1i32 << b;
            let idx = ffloor(phase * n as f32);
            bit_reverse(idx, b) as f32 / n as f32
        }
        Brownian => {
            let disp = 0.25 * amt * brownian(64.0 * phase, 4);
            frac(phase + disp)
        }
        Reciprocal => {
            let g = 2.0 + 4.0 * amt;
            let num = phase * g;
            let den = phase + (1.0 - phase) * g;
            let y = if den > 1e-12 { num / den } else { 0.0 };
            js_clamp(y, 0.0, 1.0)
        }
        Wormhole => {
            let gap = js_clamp(0.8 * amt, 0.0, 1.0);
            let a = 0.5 * (1.0 - gap);
            let b = 0.5 * (1.0 + gap);
            if phase < a {
                (phase / a) * 0.5
            } else if phase > b {
                0.5 * (1.0 + (phase - b) / (1.0 - b))
            } else {
                0.5
            }
        }
        Logistic => {
            let mut x = phase;
            let r = 3.6 + 0.4 * amt;
            let iters = 1 + fround(2.0 * amt);
            for _ in 0..iters {
                x = r * x * (1.0 - x);
            }
            js_clamp(x, 0.0, 1.0)
        }
        Sigmoid => {
            let k = 1.0 + 10.0 * amt;
            let x = phase - 0.5;
            let y = 1.0 / (1.0 + (-k * x).exp());
            let y0 = 1.0 / (1.0 + (0.5 * k).exp());
            let y1 = 1.0 / (1.0 + (-0.5 * k).exp());
            (y - y0) / (y1 - y0)
        }
        Fractal => {
            let d = 0.5 * (tau * phase).sin() * amt;
            frac(phase + d)
        }
        // Flip leaves the phase alone; see `flips_sample`.
        Flip => phase,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every mode has to keep the phase inside the table. A mode that
    /// returned 1.3 would read past the last frame; one that returned a NaN
    /// would silence the voice.
    #[test]
    fn every_mode_stays_in_range_and_stays_finite() {
        for index in 0..=21 {
            let mode = WarpMode::from_index(index);
            for amt_step in 0..=10 {
                let amt = amt_step as f32 / 10.0;
                for phase_step in 0..100 {
                    let phase = phase_step as f32 / 100.0;
                    let warped = warp_phase(phase, amt, mode);
                    assert!(
                        warped.is_finite(),
                        "{mode:?} at amt {amt} phase {phase} gave {warped}"
                    );
                    assert!(
                        (-0.001..=1.001).contains(&warped),
                        "{mode:?} at amt {amt} phase {phase} left the table: {warped}"
                    );
                }
            }
        }
    }

    /// `FLIP` is the odd one: it is not a phase transform, it inverts the
    /// sample for the first `amt` of the cycle. Treating it as a phase mode
    /// would make it silently do nothing.
    #[test]
    fn flip_inverts_the_early_sample_and_leaves_the_phase_alone() {
        assert_eq!(warp_phase(0.25, 0.5, WarpMode::Flip), 0.25);
        assert!(WarpMode::Flip.flips_sample(0.25, 0.5));
        assert!(!WarpMode::Flip.flips_sample(0.75, 0.5));
        assert!(!WarpMode::Asym.flips_sample(0.25, 0.5));
    }

    /// Names are matched case-insensitively, and anything unknown falls back
    /// to NONE rather than refusing the voice.
    #[test]
    fn names_and_indices_agree() {
        assert_eq!(WarpMode::from_name("asym"), WarpMode::Asym);
        assert_eq!(WarpMode::from_name("ASYM"), WarpMode::Asym);
        assert_eq!(WarpMode::from_name("WoRmHoLe"), WarpMode::Wormhole);
        assert_eq!(WarpMode::from_name("nonsense"), WarpMode::None);
        assert_eq!(WarpMode::from_index(21), WarpMode::Flip);
        assert_eq!(WarpMode::from_index(99), WarpMode::None);
    }

    /// Every mode at three points, pinned from a reference render rather
    /// than worked out by hand (hand-derivation is how you end up pinning
    /// your own mistake). These catch the shapes that are easy to get subtly
    /// wrong: the truncating `ffloor`, the bit-reversal, and the value noise
    /// behind BROWNIAN.
    #[test]
    fn matches_strudel_at_sampled_points() {
        let cases: &[(WarpMode, f32, f32, f32)] = &[
            (WarpMode::None, 0.25, 0.5, 0.25),
            (WarpMode::None, 0.7, 0.3, 0.7),
            (WarpMode::None, 0.1, 0.9, 0.1),
            (WarpMode::Asym, 0.25, 0.5, 0.2475248),
            (WarpMode::Asym, 0.7, 0.3, 0.7835498),
            (WarpMode::Asym, 0.1, 0.9, 0.05549389),
            (WarpMode::Mirror, 0.25, 0.5, 0.4950495),
            (WarpMode::Mirror, 0.7, 0.3, 0.4329004),
            (WarpMode::Mirror, 0.1, 0.9, 0.1109878),
            (WarpMode::BendP, 0.25, 0.5, 0.03125),
            (WarpMode::BendP, 0.7, 0.3, 0.5077925),
            (WarpMode::BendP, 0.1, 0.9, 0.0001995262),
            (WarpMode::BendM, 0.25, 0.5, 0.5743492),
            (WarpMode::BendM, 0.7, 0.3, 0.8288437),
            (WarpMode::BendM, 0.1, 0.9, 0.5366977),
            (WarpMode::BendMp, 0.25, 0.5, 0.7575758),
            (WarpMode::BendMp, 0.7, 0.3, 0.4562635),
            (WarpMode::BendMp, 0.1, 0.9, 0.1246883),
            (WarpMode::Sync, 0.25, 0.5, 0.5),
            (WarpMode::Sync, 0.7, 0.3, 0.8983981),
            (WarpMode::Sync, 0.1, 0.9, 0.9447941),
            (WarpMode::Quant, 0.25, 0.5, 0.25),
            (WarpMode::Quant, 0.7, 0.3, 0.6992188),
            (WarpMode::Quant, 0.1, 0.9, 0.0),
            (WarpMode::Fold, 0.25, 0.5, 0.5),
            (WarpMode::Fold, 0.7, 0.3, 0.8),
            (WarpMode::Fold, 0.1, 0.9, 0.4),
            (WarpMode::Pwm, 0.25, 0.5, 0.25),
            (WarpMode::Pwm, 0.7, 0.3, 0.7844828),
            (WarpMode::Pwm, 0.1, 0.9, 0.05605381),
            (WarpMode::Orbit, 0.25, 0.5, 0.0),
            (WarpMode::Orbit, 0.7, 0.3, 0.7881678),
            (WarpMode::Orbit, 0.1, 0.9, 0.5279754),
            (WarpMode::Spin, 0.25, 0.5, 0.0),
            (WarpMode::Spin, 0.7, 0.3, 0.7881678),
            (WarpMode::Spin, 0.1, 0.9, 0.5279754),
            (WarpMode::Chaos, 0.25, 0.5, 0.4859375),
            (WarpMode::Chaos, 0.7, 0.3, 0.72877),
            (WarpMode::Chaos, 0.1, 0.9, 0.33157),
            (WarpMode::Primes, 0.25, 0.5, 0.2486188),
            (WarpMode::Primes, 0.7, 0.3, 0.6988906),
            (WarpMode::Primes, 0.1, 0.9, 0.05882353),
            (WarpMode::Binary, 0.25, 0.5, 0.0078125),
            (WarpMode::Binary, 0.7, 0.3, 0.4003906),
            (WarpMode::Binary, 0.1, 0.9, 0.5),
            (WarpMode::Brownian, 0.25, 0.5, 0.1927676),
            (WarpMode::Brownian, 0.7, 0.3, 0.7052246),
            (WarpMode::Brownian, 0.1, 0.9, 0.2308453),
            (WarpMode::Reciprocal, 0.25, 0.5, 0.3076923),
            (WarpMode::Reciprocal, 0.7, 0.3, 1.0),
            (WarpMode::Reciprocal, 0.1, 0.9, 0.1089494),
            (WarpMode::Wormhole, 0.25, 0.5, 0.4166667),
            (WarpMode::Wormhole, 0.7, 0.3, 0.6052632),
            (WarpMode::Wormhole, 0.1, 0.9, 0.3571429),
            (WarpMode::Logistic, 0.25, 0.5, 0.7784063),
            (WarpMode::Logistic, 0.7, 0.3, 0.6358468),
            (WarpMode::Logistic, 0.1, 0.9, 0.3297002),
            (WarpMode::Sigmoid, 0.25, 0.5, 0.1491465),
            (WarpMode::Sigmoid, 0.7, 0.3, 0.7494432),
            (WarpMode::Sigmoid, 0.1, 0.9, 0.01144658),
            (WarpMode::Fractal, 0.25, 0.5, 0.5),
            (WarpMode::Fractal, 0.7, 0.3, 0.5573415),
            (WarpMode::Fractal, 0.1, 0.9, 0.3645034),
            (WarpMode::Flip, 0.25, 0.5, 0.25),
            (WarpMode::Flip, 0.7, 0.3, 0.7),
            (WarpMode::Flip, 0.1, 0.9, 0.1),
        ];
        for (mode, phase, amt, expected) in cases {
            let got = warp_phase(*phase, *amt, *mode);
            assert!(
                (got - expected).abs() < 1e-5,
                "{mode:?}({phase}, {amt}) = {got}, the reference says {expected}"
            );
        }
    }
}
