//! Speech sound inventory and the acoustic target each sound carries.
//!
//! One `Phoneme` is one identifiable sound of English or Spanish speech. Its
//! [`Frame`] holds the formant targets, source amplitudes and nominal duration
//! the synthesizer works from. The values follow the classic male reference
//! measurements used by cascade formant synthesizers.

/// A speech sound, or the inter-word gap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phoneme {
    // Vowels.
    /// father
    Aa,
    /// cat
    Ae,
    /// but
    Ah,
    /// dog
    Ao,
    /// cow
    Aw,
    /// bite
    Ay,
    /// about
    Ax,
    /// bet
    Eh,
    /// bird
    Er,
    /// bait
    Ey,
    /// bit
    Ih,
    /// beet
    Iy,
    /// boat
    Ow,
    /// boy
    Oy,
    /// book
    Uh,
    /// boot
    Uw,
    // Consonants.
    B,
    D,
    /// voiced th, as in the
    Dh,
    F,
    G,
    /// aspiration, as in hat
    Hh,
    K,
    L,
    M,
    N,
    /// ring
    Ng,
    P,
    R,
    S,
    /// ship
    Sh,
    T,
    /// unvoiced th, as in thin
    Th,
    V,
    /// Spanish j and g before e or i
    X,
    W,
    Y,
    Z,
    /// measure
    Zh,
    /// Spanish rolled rr
    Rr,
    /// Inter-word gap.
    Pause,
}

impl Phoneme {
    /// True for sounds that carry a syllable: the prosody plan anchors one
    /// pitch target to each of them.
    pub fn is_nucleus(self) -> bool {
        matches!(
            self,
            Phoneme::Aa
                | Phoneme::Ae
                | Phoneme::Ah
                | Phoneme::Ao
                | Phoneme::Aw
                | Phoneme::Ay
                | Phoneme::Ax
                | Phoneme::Eh
                | Phoneme::Er
                | Phoneme::Ey
                | Phoneme::Ih
                | Phoneme::Iy
                | Phoneme::Ow
                | Phoneme::Oy
                | Phoneme::Uh
                | Phoneme::Uw
        )
    }

    /// The second half of a diphthong, which the utterance builder appends as
    /// its own span so the glide glides through the same interpolation path
    /// every other transition uses.
    pub fn diphthong_tail(self) -> Option<Self> {
        match self {
            Phoneme::Ey => Some(Phoneme::Ih),
            Phoneme::Ay => Some(Phoneme::Ih),
            Phoneme::Ow => Some(Phoneme::Uw),
            Phoneme::Aw => Some(Phoneme::Uw),
            Phoneme::Oy => Some(Phoneme::Iy),
            _ => None,
        }
    }

    /// True for the six stop consonants, which expand into a closure followed
    /// by a burst.
    pub fn is_stop(self) -> bool {
        matches!(
            self,
            Phoneme::B | Phoneme::D | Phoneme::G | Phoneme::P | Phoneme::T | Phoneme::K
        )
    }

    /// True when the sound is voiced, so whisper mode has a source to replace.
    pub fn is_voiced(self) -> bool {
        !matches!(
            self,
            Phoneme::P
                | Phoneme::T
                | Phoneme::K
                | Phoneme::F
                | Phoneme::Th
                | Phoneme::S
                | Phoneme::Sh
                | Phoneme::X
                | Phoneme::Hh
                | Phoneme::Pause
        )
    }
}

/// One acoustic target: what the synthesizer aims at while a sound lasts.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    /// First three cascade formant centers, in Hz.
    pub f1: f32,
    pub f2: f32,
    pub f3: f32,
    /// Voicing amplitude, 0 to 1. A stop closure carries a small value as its
    /// voice bar.
    pub voice: f32,
    /// Frication noise center in Hz, 0 when the sound has no frication.
    pub fric_hz: f32,
    /// Frication noise amplitude, 0 to 1.
    pub fric_amp: f32,
    /// Nasal side-branch amount, 0 to 1.
    pub nasal: f32,
    /// Nominal duration in ms at speed 150. A stop's value is its closure;
    /// the burst adds a fixed 12 ms.
    pub ms: u32,
}

const fn voiced(f1: f32, f2: f32, f3: f32, ms: u32) -> Frame {
    Frame {
        f1,
        f2,
        f3,
        voice: 1.0,
        fric_hz: 0.0,
        fric_amp: 0.0,
        nasal: 0.0,
        ms,
    }
}

const fn nasal(f1: f32, f2: f32, f3: f32, ms: u32) -> Frame {
    Frame {
        nasal: 1.0,
        ..voiced(f1, f2, f3, ms)
    }
}

const fn stop(
    f1: f32,
    f2: f32,
    f3: f32,
    voice: f32,
    burst_hz: f32,
    burst_amp: f32,
    ms: u32,
) -> Frame {
    Frame {
        voice,
        fric_hz: burst_hz,
        fric_amp: burst_amp,
        ..voiced(f1, f2, f3, ms)
    }
}

const fn fricative(fric_hz: f32, fric_amp: f32, ms: u32) -> Frame {
    Frame {
        fric_hz,
        fric_amp,
        ..voiced(400.0, 1400.0, 2500.0, ms)
    }
}

/// A voiced fricative runs both branches at reduced voicing.
const fn voiced_fric(fric_hz: f32, fric_amp: f32, ms: u32) -> Frame {
    Frame {
        voice: 0.5,
        ..fricative(fric_hz, fric_amp, ms)
    }
}

/// The acoustic target of a sound.
pub fn frame(ph: Phoneme) -> Frame {
    match ph {
        Phoneme::Aa => voiced(710.0, 1100.0, 2540.0, 125),
        Phoneme::Ae => voiced(690.0, 1660.0, 2490.0, 125),
        Phoneme::Ah => voiced(620.0, 1220.0, 2550.0, 90),
        Phoneme::Ao => voiced(590.0, 880.0, 2540.0, 120),
        Phoneme::Aw => voiced(710.0, 1100.0, 2540.0, 165),
        Phoneme::Ay => voiced(710.0, 1100.0, 2540.0, 175),
        Phoneme::Ax => voiced(500.0, 1500.0, 2500.0, 65),
        Phoneme::Eh => voiced(550.0, 1770.0, 2490.0, 105),
        Phoneme::Er => voiced(490.0, 1350.0, 1690.0, 110),
        Phoneme::Ey => voiced(550.0, 1770.0, 2490.0, 160),
        Phoneme::Ih => voiced(400.0, 1920.0, 2560.0, 100),
        Phoneme::Iy => voiced(280.0, 2250.0, 2890.0, 130),
        Phoneme::Ow => voiced(590.0, 880.0, 2540.0, 160),
        Phoneme::Oy => voiced(590.0, 880.0, 2540.0, 175),
        Phoneme::Uh => voiced(450.0, 1030.0, 2380.0, 95),
        Phoneme::Uw => voiced(310.0, 870.0, 2250.0, 125),
        Phoneme::B => stop(350.0, 900.0, 2200.0, 0.2, 750.0, 0.25, 55),
        Phoneme::D => stop(350.0, 1700.0, 2600.0, 0.2, 3800.0, 0.3, 50),
        Phoneme::G => stop(350.0, 1600.0, 2400.0, 0.2, 2200.0, 0.3, 55),
        Phoneme::P => stop(350.0, 900.0, 2200.0, 0.0, 750.0, 0.35, 60),
        Phoneme::T => stop(350.0, 1700.0, 2600.0, 0.0, 3800.0, 0.45, 55),
        Phoneme::K => stop(350.0, 1600.0, 2400.0, 0.0, 2200.0, 0.45, 60),
        Phoneme::M => nasal(250.0, 1100.0, 2150.0, 70),
        Phoneme::N => nasal(250.0, 1600.0, 2600.0, 68),
        Phoneme::Ng => nasal(250.0, 2100.0, 2900.0, 72),
        Phoneme::L => voiced(380.0, 1100.0, 2800.0, 75),
        Phoneme::R => voiced(350.0, 1000.0, 1600.0, 75),
        Phoneme::Rr => voiced(350.0, 1000.0, 1600.0, 85),
        Phoneme::W => voiced(300.0, 610.0, 2150.0, 60),
        Phoneme::Y => voiced(270.0, 2300.0, 3000.0, 55),
        Phoneme::F => fricative(1600.0, 0.35, 95),
        Phoneme::V => voiced_fric(1600.0, 0.3, 75),
        Phoneme::Th => fricative(5700.0, 0.18, 90),
        Phoneme::Dh => voiced_fric(5700.0, 0.15, 60),
        Phoneme::S => fricative(5800.0, 0.55, 100),
        Phoneme::Z => voiced_fric(5800.0, 0.45, 85),
        Phoneme::Sh => fricative(2600.0, 0.6, 105),
        Phoneme::Zh => voiced_fric(2600.0, 0.5, 85),
        // Aspiration: weak broadband noise the neighboring vowels color.
        Phoneme::Hh => fricative(1500.0, 0.12, 65),
        Phoneme::X => fricative(2200.0, 0.45, 100),
        Phoneme::Pause => Frame {
            f1: 500.0,
            f2: 1500.0,
            f3: 2500.0,
            voice: 0.0,
            fric_hz: 0.0,
            fric_amp: 0.0,
            nasal: 0.0,
            ms: 40,
        },
    }
}
