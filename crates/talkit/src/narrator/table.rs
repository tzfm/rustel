//! What each sound aims at: formants, bandwidths, source levels and the
//! color of its noise.
//!
//! The values are fitted to the original engine's plain male voice: the
//! noise colors to its frication spectra, the rest to whole utterances.

use crate::phonetic::Ph;

/// How a sound is produced, which decides how it is built in time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Class {
    Vowel,
    /// A vowel that glides to a second target.
    Diphthong,
    /// Voiced, continuous and vowel-like: liquids, glides, nasals.
    Sonorant,
    /// Noise alone.
    Fricative,
    /// Noise over voicing.
    VoicedFricative,
    /// Closure, then a burst.
    Stop,
    VoicedStop,
    /// Breath through the shape of the next sound.
    Aspirate,
    /// A short break in the voice.
    Glottal,
}

/// The color of a frication noise: up to three resonances (Hz, bandwidth,
/// level) and a level of plain noise, all through a first difference. The
/// levels are in 8-bit steps of the original engine per unit of noise, at
/// its plain male voice.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Hiss {
    pub peaks: [(f32, f32, f32); 3],
    pub flat: f32,
}

impl Hiss {
    pub const NONE: Self = Self {
        peaks: [(0.0, 100.0, 0.0); 3],
        flat: 0.0,
    };
}

/// A hiss from its fitted values. The fit measured noise of variance
/// 1/192 per spectral bin at the engine's -2 dB frication offset, against
/// the synthesizer's uniform noise.
const fn hiss(peaks: [(f32, f32, f32); 3], flat: f32) -> Hiss {
    const UNIT: f32 = 0.156;
    Hiss {
        peaks: [
            (peaks[0].0, peaks[0].1, peaks[0].2 * UNIT),
            (peaks[1].0, peaks[1].1, peaks[1].2 * UNIT),
            (peaks[2].0, peaks[2].1, peaks[2].2 * UNIT),
        ],
        flat: flat * UNIT,
    }
}

const S_HISS: Hiss = hiss(
    [
        (0.0, 725.0, 41.2),
        (3436.0, 606.0, 2.0),
        (4469.0, 507.0, 6.7),
    ],
    0.0,
);
const SH_HISS: Hiss = hiss(
    [
        (1891.0, 207.0, 20.1),
        (2270.0, 174.0, 8.1),
        (4898.0, 799.0, 0.0),
    ],
    0.0,
);
const F_HISS: Hiss = hiss(
    [
        (411.0, 610.0, 16.4),
        (3418.0, 897.0, 5.3),
        (4113.0, 30.0, 0.2),
    ],
    0.0,
);
const TH_HISS: Hiss = hiss(
    [
        (2613.0, 28.0, 0.2),
        (3421.0, 509.0, 2.4),
        (4031.0, 378.0, 0.8),
    ],
    8.1,
);
const KH_HISS: Hiss = hiss(
    [
        (2200.0, 189.0, 14.5),
        (1758.0, 83.0, 5.3),
        (2561.0, 2252.0, 0.0),
    ],
    0.0,
);
const Z_HISS: Hiss = hiss(
    [
        (0.0, 711.0, 37.6),
        (5192.0, 2176.0, 0.0),
        (4454.0, 569.0, 6.4),
    ],
    0.0,
);
const ZH_HISS: Hiss = hiss(
    [
        (2114.0, 181.0, 15.1),
        (1785.0, 227.0, 17.1),
        (3366.0, 2903.0, 0.0),
    ],
    0.0,
);
const V_HISS: Hiss = hiss(
    [
        (0.0, 907.0, 30.9),
        (3559.0, 30.0, 0.1),
        (3448.0, 932.0, 6.3),
    ],
    0.0,
);
const DH_HISS: Hiss = hiss(
    [
        (368.0, 570.0, 18.8),
        (3579.0, 1024.0, 6.8),
        (4112.0, 30.0, 0.3),
    ],
    0.0,
);
const CH_HISS: Hiss = hiss(
    [
        (2119.0, 131.0, 22.3),
        (1771.0, 208.0, 27.6),
        (3124.0, 185.0, 1.8),
    ],
    0.0,
);
const J_HISS: Hiss = hiss(
    [
        (1875.0, 192.0, 42.9),
        (2351.0, 120.0, 12.5),
        (3118.0, 171.0, 2.7),
    ],
    0.0,
);

/// The acoustic target of a sound.
#[derive(Clone, Copy, Debug)]
pub(super) struct Target {
    pub class: Class,
    /// F1 to F3 and their bandwidths, Hz.
    pub f: [f32; 3],
    pub b: [f32; 3],
    /// Where a diphthong ends.
    pub to: [f32; 3],
    /// Voicing amplitude in the source tables' units: the plain male voice's
    /// AA is 9.
    pub av: f32,
    /// Aspiration amplitude, and the share of the frication noise.
    pub ah: f32,
    pub af: f32,
    /// The color of the frication noise.
    pub hiss: Hiss,
}

const fn vowel(f: [f32; 3], b: [f32; 3], av: f32) -> Target {
    Target {
        class: Class::Vowel,
        f,
        b,
        to: f,
        av,
        ah: 0.0,
        af: 0.0,
        hiss: Hiss::NONE,
    }
}

const fn glide(f: [f32; 3], to: [f32; 3], b: [f32; 3], av: f32) -> Target {
    Target {
        class: Class::Diphthong,
        to,
        ..vowel(f, b, av)
    }
}

const fn sonorant(f: [f32; 3], b: [f32; 3], av: f32) -> Target {
    Target {
        class: Class::Sonorant,
        ..vowel(f, b, av)
    }
}

const fn aspirate(f: [f32; 3], b: [f32; 3], av: f32) -> Target {
    Target {
        class: Class::Aspirate,
        ah: 1.2,
        ..vowel(f, b, av)
    }
}

const fn fricative(f: [f32; 3], b: [f32; 3], hiss: Hiss) -> Target {
    Target {
        class: Class::Fricative,
        af: 1.0,
        hiss,
        ..vowel(f, b, 0.0)
    }
}

const fn voiced_fricative(f: [f32; 3], b: [f32; 3], av: f32, hiss: Hiss) -> Target {
    Target {
        class: Class::VoicedFricative,
        av,
        ..fricative(f, b, hiss)
    }
}

const fn stop(f: [f32; 3], b: [f32; 3], av: f32, hiss: Hiss) -> Target {
    Target {
        class: Class::Stop,
        av,
        ..fricative(f, b, hiss)
    }
}

const fn voiced_stop(f: [f32; 3], b: [f32; 3], av: f32, hiss: Hiss) -> Target {
    Target {
        class: Class::VoicedStop,
        af: 0.5,
        ..stop(f, b, av, hiss)
    }
}

/// The target of a sound for the plain male voice.
pub(super) fn target(ph: Ph) -> Target {
    use Ph::*;
    match ph {
        Iy => vowel([301.0, 2256.0, 2847.0], [60.0, 148.0, 293.0], 13.26),
        Ih => vowel([454.0, 1714.0, 2608.0], [20.0, 147.0, 209.0], 11.03),
        Eh => vowel([541.0, 1777.0, 2598.0], [75.0, 75.0, 211.0], 10.19),
        Ae => vowel([620.0, 1573.0, 2498.0], [101.0, 167.0, 473.0], 10.35),
        Aa => vowel([708.0, 1126.0, 2584.0], [142.0, 89.0, 215.0], 10.08),
        Ah => vowel([598.0, 1316.0, 2513.0], [97.0, 92.0, 235.0], 8.65),
        Ao => vowel([633.0, 1004.0, 2677.0], [93.0, 135.0, 135.0], 9.31),
        Uh => vowel([481.0, 1185.0, 2378.0], [91.0, 159.0, 178.0], 11.05),
        Uw => vowel([409.0, 1250.0, 2476.0], [74.0, 204.0, 327.0], 14.23),
        Er => vowel([491.0, 1414.0, 1770.0], [107.0, 137.0, 573.0], 10.77),
        Ax => vowel([527.0, 1294.0, 2491.0], [87.0, 116.0, 600.0], 6.64),
        Ix => vowel([423.0, 1662.0, 2505.0], [65.0, 141.0, 316.0], 6.69),
        Ow => glide(
            [609.0, 1194.0, 2366.0],
            [471.0, 909.0, 2271.0],
            [114.0, 126.0, 176.0],
            10.3,
        ),
        Oh => glide(
            [593.0, 1188.0, 2351.0],
            [470.0, 899.0, 2273.0],
            [119.0, 145.0, 207.0],
            9.69,
        ),
        Ey => glide(
            [569.0, 1823.0, 2648.0],
            [423.0, 2116.0, 2729.0],
            [57.0, 161.0, 333.0],
            12.63,
        ),
        Ay => glide(
            [706.0, 1311.0, 2690.0],
            [367.0, 2106.0, 2711.0],
            [124.0, 162.0, 243.0],
            11.42,
        ),
        Oy => glide(
            [573.0, 1063.0, 2331.0],
            [372.0, 1884.0, 2489.0],
            [79.0, 191.0, 285.0],
            10.14,
        ),
        Aw => glide(
            [638.0, 1338.0, 2592.0],
            [485.0, 994.0, 2347.0],
            [116.0, 99.0, 168.0],
            10.67,
        ),
        Ul => vowel([502.0, 804.0, 2809.0], [98.0, 198.0, 418.0], 7.23),
        Um => vowel([495.0, 1230.0, 2314.0], [222.0, 185.0, 565.0], 7.73),
        Un => vowel([490.0, 1351.0, 2500.0], [191.0, 160.0, 600.0], 7.98),
        Il => vowel([516.0, 1014.0, 2852.0], [65.0, 183.0, 449.0], 7.77),
        Im => vowel([468.0, 1508.0, 2276.0], [170.0, 341.0, 549.0], 7.98),
        In => vowel([411.0, 1656.0, 2558.0], [160.0, 255.0, 600.0], 8.33),
        L => sonorant([347.0, 893.0, 3019.0], [25.0, 92.0, 133.0], 9.39),
        Lx => sonorant([546.0, 951.0, 2900.0], [66.0, 173.0, 175.0], 7.25),
        R => sonorant([524.0, 1198.0, 1854.0], [69.0, 112.0, 160.0], 10.58),
        Rx => sonorant([475.0, 1350.0, 1645.0], [66.0, 110.0, 550.0], 13.9),
        W => sonorant([341.0, 699.0, 2244.0], [43.0, 70.0, 59.0], 11.52),
        Wh => sonorant([430.0, 713.0, 2176.0], [73.0, 68.0, 20.0], 6.63),
        Y => sonorant([285.0, 2278.0, 3060.0], [67.0, 176.0, 338.0], 13.29),
        M => sonorant([323.0, 1211.0, 2210.0], [223.0, 557.0, 248.0], 16.24),
        N => sonorant([240.0, 1638.0, 2258.0], [281.0, 453.0, 235.0], 15.47),
        Nx => sonorant([238.0, 1953.0, 2120.0], [241.0, 515.0, 266.0], 16.18),
        Dx => sonorant([350.0, 1700.0, 2600.0], [100.0, 150.0, 300.0], 3.0),
        S => fricative([415.0, 1664.0, 2548.0], [202.0, 200.0, 302.0], S_HISS),
        Sh => fricative([352.0, 1567.0, 2540.0], [208.0, 229.0, 316.0], SH_HISS),
        F => fricative([431.0, 1148.0, 2437.0], [273.0, 220.0, 389.0], F_HISS),
        Th => fricative([308.0, 1316.0, 2885.0], [264.0, 245.0, 255.0], TH_HISS),
        Kh => fricative([400.0, 1400.0, 2500.0], [200.0, 200.0, 300.0], KH_HISS),
        Z => voiced_fricative([300.0, 1620.0, 2807.0], [83.0, 116.0, 357.0], 6.34, Z_HISS),
        Zh => voiced_fricative(
            [300.0, 1904.0, 2488.0],
            [240.0, 155.0, 164.0],
            6.11,
            ZH_HISS,
        ),
        V => voiced_fricative([287.0, 1170.0, 2011.0], [115.0, 183.0, 423.0], 7.41, V_HISS),
        Dh => voiced_fricative(
            [262.0, 1448.0, 2941.0],
            [166.0, 168.0, 354.0],
            8.73,
            DH_HISS,
        ),
        H => aspirate([505.0, 1568.0, 2468.0], [216.0, 196.0, 302.0], 0.35),
        Hh => aspirate([500.0, 1500.0, 2500.0], [200.0, 200.0, 300.0], 0.0),
        P => stop([297.0, 906.0, 2253.0], [267.0, 189.0, 308.0], 0.16, F_HISS),
        T => stop([256.0, 1642.0, 2679.0], [236.0, 261.0, 341.0], 0.19, S_HISS),
        K => stop([388.0, 2131.0, 2221.0], [243.0, 190.0, 318.0], 0.5, KH_HISS),
        Ch | C => stop(
            [216.0, 1600.0, 2513.0],
            [342.0, 183.0, 260.0],
            0.34,
            CH_HISS,
        ),
        B => voiced_stop([280.0, 911.0, 2194.0], [203.0, 197.0, 344.0], 0.83, F_HISS),
        D => voiced_stop([317.0, 1961.0, 2755.0], [208.0, 198.0, 299.0], 0.28, S_HISS),
        G => voiced_stop(
            [361.0, 2151.0, 2527.0],
            [201.0, 167.0, 213.0],
            1.07,
            KH_HISS,
        ),
        J => Target {
            af: 1.0,
            ..voiced_stop([264.0, 1777.0, 2451.0], [199.0, 91.0, 305.0], 5.0, J_HISS)
        },
        Q | Qx => Target {
            class: Class::Glottal,
            ..vowel([500.0, 1500.0, 2500.0], [100.0, 100.0, 200.0], 0.0)
        },
    }
}

/// How a vocal tract reshapes the targets above: its formant and bandwidth
/// factors, its factor on the hiss frequencies, its frication level, and its
/// voicing level in dB at each of the glottal source anchors.
///
/// The voicing level of the woman and child tracts climbs with the pitch:
/// the woman tract is about 13 dB under the man at 100 Hz and level with
/// him near 320 Hz.
#[derive(Clone, Copy, Debug)]
pub(super) struct Tract {
    pub f: [f32; 3],
    pub b: [f32; 3],
    pub hiss: f32,
    /// The frication level, dB against the man.
    pub af: f32,
    pub av: [f32; 13],
}

impl Tract {
    /// The voicing gain at pitch `hz`, between the two anchors around it.
    pub(super) fn voice_gain(&self, hz: f32) -> f32 {
        let a = &super::glottis::ANCHORS;
        let x = hz.clamp(a[0], a[a.len() - 1]);
        let k = a
            .iter()
            .rposition(|&p| p <= x)
            .unwrap_or(0)
            .min(a.len() - 2);
        let t = (x / a[k]).log2() / (a[k + 1] / a[k]).log2();
        let db = self.av[k] + (self.av[k + 1] - self.av[k]) * t;
        10f32.powf(db / 20.0)
    }
}

/// The tracts of the voice record: 0 man, 1 woman, 2 child, 3 large man.
/// Fitted to the original engine on the voices that use each tract, with
/// the voicing gains measured against the plain male voice.
pub(super) fn tract(n: u8) -> Tract {
    match n {
        1 => Tract {
            f: [1.007, 1.031, 1.135],
            b: [1.145, 1.92, 1.962],
            hiss: 1.132,
            af: -7.5,
            av: [
                -12.6, -13.0, -13.4, -13.8, -13.4, -11.2, -8.2, -4.9, -1.3, 5.3, 3.8, 3.7, 6.9,
            ],
        },
        2 => Tract {
            f: [1.464, 1.414, 1.368],
            b: [2.252, 1.106, 1.313],
            hiss: 1.012,
            af: -9.0,
            av: [
                -12.4, -12.4, -12.7, -13.6, -12.8, -10.0, -8.0, -4.5, 1.5, 3.7, 2.1, 5.2, 3.9,
            ],
        },
        3 => Tract {
            f: [0.89, 0.949, 0.952],
            b: [1.15, 1.093, 0.767],
            hiss: 1.013,
            af: -1.2,
            av: [0.0; 13],
        },
        _ => Tract {
            f: [1.0; 3],
            b: [1.0; 3],
            hiss: 1.0,
            af: 0.0,
            av: [0.0; 13],
        },
    }
}
