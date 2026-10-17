//! The twenty Talk It! voices and the settings each one starts from.
//!
//! A personality is a preset of pitch, speed, pitch quality and vocal effort,
//! as the original application's buttons set them, and a [`Timbre`]: the
//! rest of the original engine's voice record for that button.

use crate::params::{Effort, Quality};

/// The sound of a voice apart from its four settings, field for field as the
/// original engine's voice record holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Timbre {
    /// The vowel and consonant tables the voice speaks with: 0 man,
    /// 1 woman, 2 child, 3 large man.
    pub tract: u8,
    /// The glottal source, 1 to 8.
    pub glottis: u8,
    /// The slope of the source: 2 and above is flat, each step below it
    /// darkens the voice.
    pub tilt: i8,
    /// How far natural intonation moves, in percent.
    pub range: i16,
    /// How unsteady the pitch is.
    pub perturb: i16,
    /// The height of natural accents, in percent.
    pub accent: [i16; 2],
    /// The length of the vowels, in percent.
    pub vowels: i16,
    /// Level offsets of voicing, aspiration and frication, in dB.
    pub av: i8,
    pub ah: i8,
    pub af: i8,
    /// F1 to F3 and their bandwidths, in percent of the tables.
    pub formants: [i16; 3],
    pub bandwidths: [i16; 3],
    /// The fixed fourth and fifth resonances: Hz and bandwidth.
    pub upper: [(i16, i16); 2],
}

impl Timbre {
    /// The voice the tables describe as they are: the Man button.
    pub const PLAIN: Self = Personality::Man.timbre();
}

/// One voice record in the order the original engine stores it, after
/// pitch, speed, effort and pitch quality.
#[allow(clippy::too_many_arguments)]
const fn record(
    tract: u8,
    glottis: u8,
    tilt: i8,
    range: i16,
    perturb: i16,
    accent: i16,
    vowels: i16,
    [av, ah, af]: [i8; 3],
    formants: [i16; 3],
    bandwidths: [i16; 3],
    upper: [i16; 4],
) -> Timbre {
    Timbre {
        tract,
        glottis,
        tilt,
        range,
        perturb,
        accent: [accent, accent],
        vowels,
        av,
        ah,
        af,
        formants,
        bandwidths,
        upper: [(upper[0], upper[1]), (upper[2], upper[3])],
    }
}

const SAME: [i16; 3] = [100, 100, 100];
/// A named voice preset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Personality {
    Man,
    Woman,
    HyperFemale,
    Child,
    StrongMan,
    Mellow,
    SingingGirl,
    StrongWoman,
    Fly,
    LittleRobot,
    Martian,
    BigRobot,
    HyperMale,
    OldWoman,
    LittleMan,
    ImaginaryMan,
    Nerd,
    Whiner,
    Wobbly,
    SingingBoy,
}

/// The preset a personality selects: base pitch in Hz, speaking speed in
/// percent of the nominal 150, and the two mode settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preset {
    pub pitch: u32,
    pub speed: u32,
    pub quality: Quality,
    pub effort: Effort,
}

impl Personality {
    /// Every personality in the original button order.
    pub const ALL: [Self; 20] = [
        Self::Man,
        Self::Woman,
        Self::HyperFemale,
        Self::Child,
        Self::StrongMan,
        Self::Mellow,
        Self::SingingGirl,
        Self::StrongWoman,
        Self::Fly,
        Self::LittleRobot,
        Self::Martian,
        Self::BigRobot,
        Self::HyperMale,
        Self::OldWoman,
        Self::LittleMan,
        Self::ImaginaryMan,
        Self::Nerd,
        Self::Whiner,
        Self::Wobbly,
        Self::SingingBoy,
    ];

    /// The name a score or the command line writes: the label in lowercase
    /// with a hyphen for each space, so the name stays one word in
    /// mini-notation. [`Personality::parse`] reads it back.
    pub fn name(self) -> &'static str {
        match self {
            Self::Man => "man",
            Self::Woman => "woman",
            Self::HyperFemale => "hyper-female",
            Self::Child => "child",
            Self::StrongMan => "strong-man",
            Self::Mellow => "mellow",
            Self::SingingGirl => "singing-girl",
            Self::StrongWoman => "strong-woman",
            Self::Fly => "fly",
            Self::LittleRobot => "little-robot",
            Self::Martian => "martian",
            Self::BigRobot => "big-robot",
            Self::HyperMale => "hyper-male",
            Self::OldWoman => "old-woman",
            Self::LittleMan => "little-man",
            Self::ImaginaryMan => "imaginary-man",
            Self::Nerd => "nerd",
            Self::Whiner => "whiner",
            Self::Wobbly => "wobbly",
            Self::SingingBoy => "singing-boy",
        }
    }

    /// The label shown on the original personality button.
    pub fn label(self) -> &'static str {
        match self {
            Self::Man => "Man",
            Self::Woman => "Woman",
            Self::HyperFemale => "Hyper Female",
            Self::Child => "Child",
            Self::StrongMan => "Strong Man",
            Self::Mellow => "Mellow",
            Self::SingingGirl => "Singing Girl",
            Self::StrongWoman => "Strong Woman",
            Self::Fly => "Fly",
            Self::LittleRobot => "Little Robot",
            Self::Martian => "Martian",
            Self::BigRobot => "Big Robot",
            Self::HyperMale => "Hyper Male",
            Self::OldWoman => "Old Woman",
            Self::LittleMan => "Little Man",
            Self::ImaginaryMan => "Imaginary Man",
            Self::Nerd => "Nerd",
            Self::Whiner => "Whiner",
            Self::Wobbly => "Wobbly",
            Self::SingingBoy => "Singing Boy",
        }
    }

    /// The default settings of this personality, as the original application
    /// set them when its button was pressed.
    pub fn preset(self) -> Preset {
        match self {
            Self::Man => Preset {
                pitch: 100,
                speed: 150,
                quality: Quality::Natural,
                effort: Effort::Normal,
            },
            Self::Woman => Preset {
                pitch: 200,
                speed: 150,
                quality: Quality::Natural,
                effort: Effort::Normal,
            },
            Self::HyperFemale => Preset {
                pitch: 190,
                speed: 250,
                quality: Quality::Natural,
                effort: Effort::Normal,
            },
            Self::Child => Preset {
                pitch: 350,
                speed: 130,
                quality: Quality::Natural,
                effort: Effort::Normal,
            },
            Self::StrongMan => Preset {
                pitch: 75,
                speed: 140,
                quality: Quality::Natural,
                effort: Effort::Normal,
            },
            Self::Mellow => Preset {
                pitch: 190,
                speed: 140,
                quality: Quality::Natural,
                effort: Effort::Breathy,
            },
            Self::SingingGirl => Preset {
                pitch: 310,
                speed: 90,
                quality: Quality::Sung,
                effort: Effort::Normal,
            },
            Self::StrongWoman => Preset {
                pitch: 200,
                speed: 140,
                quality: Quality::Natural,
                effort: Effort::Normal,
            },
            Self::Fly => Preset {
                pitch: 480,
                speed: 150,
                quality: Quality::Natural,
                effort: Effort::Normal,
            },
            Self::LittleRobot => Preset {
                pitch: 90,
                speed: 150,
                quality: Quality::Monotone,
                effort: Effort::Normal,
            },
            Self::Martian => Preset {
                pitch: 80,
                speed: 150,
                quality: Quality::Monotone,
                effort: Effort::Normal,
            },
            Self::BigRobot => Preset {
                pitch: 66,
                speed: 138,
                quality: Quality::Monotone,
                effort: Effort::Normal,
            },
            Self::HyperMale => Preset {
                pitch: 135,
                speed: 300,
                quality: Quality::Natural,
                effort: Effort::Normal,
            },
            Self::OldWoman => Preset {
                pitch: 270,
                speed: 115,
                quality: Quality::Natural,
                effort: Effort::Normal,
            },
            Self::LittleMan => Preset {
                pitch: 90,
                speed: 150,
                quality: Quality::Natural,
                effort: Effort::Normal,
            },
            Self::ImaginaryMan => Preset {
                pitch: 110,
                speed: 200,
                quality: Quality::Natural,
                effort: Effort::Normal,
            },
            Self::Nerd => Preset {
                pitch: 140,
                speed: 155,
                quality: Quality::Natural,
                effort: Effort::Normal,
            },
            Self::Whiner => Preset {
                pitch: 120,
                speed: 165,
                quality: Quality::Natural,
                effort: Effort::Normal,
            },
            Self::Wobbly => Preset {
                pitch: 145,
                speed: 115,
                quality: Quality::Sung,
                effort: Effort::Normal,
            },
            Self::SingingBoy => Preset {
                pitch: 310,
                speed: 90,
                quality: Quality::Sung,
                effort: Effort::Normal,
            },
        }
    }

    /// The voice's own sound: the original engine's record for this
    /// button, read through its public interface.
    pub const fn timbre(self) -> Timbre {
        match self {
            Self::Man => record(
                0,
                1,
                2,
                125,
                0,
                120,
                100,
                [-8, 0, -2],
                SAME,
                SAME,
                [3600, 400, 4300, 150],
            ),
            Self::Woman => record(
                1,
                2,
                -2,
                100,
                0,
                120,
                100,
                [0, 0, 0],
                SAME,
                SAME,
                [4200, 1200, 4200, 1200],
            ),
            Self::HyperFemale => record(
                1,
                2,
                -2,
                350,
                0,
                120,
                110,
                [0, 0, 0],
                SAME,
                SAME,
                [4200, 1200, 4200, 1200],
            ),
            Self::Child => record(
                2,
                3,
                0,
                150,
                0,
                80,
                100,
                [-9, 0, 0],
                SAME,
                SAME,
                [2500, 2000, 4500, 2000],
            ),
            Self::StrongMan => record(
                3,
                1,
                4,
                150,
                10,
                80,
                100,
                [-3, 0, 0],
                [97, 95, 95],
                SAME,
                [3300, 350, 3850, 300],
            ),
            Self::Mellow => record(
                1,
                2,
                0,
                125,
                0,
                75,
                120,
                [0, 0, 0],
                SAME,
                SAME,
                [4200, 1200, 4200, 1200],
            ),
            Self::SingingGirl => record(
                0,
                5,
                -3,
                100,
                15,
                125,
                250,
                [-7, -10, 0],
                [130, 130, 130],
                [135, 140, 160],
                [4200, 1000, 4900, 1000],
            ),
            Self::StrongWoman => record(
                1,
                2,
                -4,
                100,
                0,
                120,
                110,
                [3, 0, 0],
                [100, 95, 95],
                SAME,
                [4000, 1200, 4200, 1200],
            ),
            Self::Fly => record(
                2,
                4,
                -4,
                180,
                120,
                70,
                140,
                [-11, 0, 0],
                [120, 120, 120],
                [110, 110, 100],
                [2500, 2000, 4500, 2000],
            ),
            Self::LittleRobot => record(
                0,
                6,
                -1,
                100,
                0,
                80,
                100,
                [-16, 0, 0],
                SAME,
                SAME,
                [3300, 450, 3850, 400],
            ),
            Self::Martian => record(
                0,
                7,
                0,
                100,
                30,
                80,
                100,
                [-8, 0, 0],
                SAME,
                SAME,
                [3600, 400, 4300, 150],
            ),
            Self::BigRobot => record(
                0,
                8,
                3,
                100,
                0,
                80,
                110,
                [-12, 0, 0],
                SAME,
                SAME,
                [3500, 2000, 4500, 2000],
            ),
            Self::HyperMale => record(
                0,
                1,
                0,
                65,
                0,
                80,
                100,
                [-4, 0, 0],
                SAME,
                SAME,
                [3600, 400, 4300, 150],
            ),
            Self::OldWoman => record(
                1,
                2,
                1,
                160,
                90,
                65,
                100,
                [-6, 0, 0],
                SAME,
                SAME,
                [4200, 1200, 4200, 1200],
            ),
            Self::LittleMan => record(
                2,
                1,
                -2,
                180,
                15,
                80,
                100,
                [-1, -4, -3],
                [100, 95, 95],
                [120, 130, 140],
                [4000, 2000, 4500, 2000],
            ),
            Self::ImaginaryMan => record(
                1,
                1,
                6,
                160,
                0,
                80,
                165,
                [5, 0, 0],
                SAME,
                SAME,
                [4200, 1200, 4200, 1200],
            ),
            Self::Nerd => record(
                0,
                1,
                2,
                200,
                20,
                80,
                80,
                [-5, 0, 0],
                [135, 100, 100],
                [150, 115, 100],
                [3600, 400, 4300, 300],
            ),
            Self::Whiner => record(
                0,
                1,
                -2,
                160,
                0,
                80,
                100,
                [-2, 0, 0],
                [90, 120, 120],
                [100, 130, 200],
                [3600, 400, 4300, 400],
            ),
            Self::Wobbly => record(
                0,
                1,
                2,
                100,
                15,
                50,
                250,
                [-4, 0, 0],
                SAME,
                SAME,
                [3600, 400, 4300, 150],
            ),
            Self::SingingBoy => record(
                0,
                3,
                -1,
                100,
                15,
                125,
                250,
                [-7, -10, 0],
                [105, 105, 105],
                [135, 115, 115],
                [4100, 600, 4900, 1000],
            ),
        }
    }

    /// Resolve a typed name. Letters alone decide: case, spaces and
    /// punctuation are ignored, so "little-robot", "LittleRobot" and "little robot" all
    /// resolve.
    pub fn parse(name: &str) -> Option<Self> {
        let wanted: String = name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_lowercase())
            .collect();
        Self::ALL.into_iter().find(|p| {
            p.label()
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .map(|c| c.to_ascii_lowercase())
                .eq(wanted.chars())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_labels_with_case_and_separators() {
        assert_eq!(
            Personality::parse("LittleRobot"),
            Some(Personality::LittleRobot)
        );
        assert_eq!(
            Personality::parse("hyper male"),
            Some(Personality::HyperMale)
        );
        assert_eq!(
            Personality::parse("IMAGINARY-MAN"),
            Some(Personality::ImaginaryMan)
        );
        assert_eq!(Personality::parse("unknown"), None);
        for voice in Personality::ALL {
            assert_eq!(Personality::parse(voice.name()), Some(voice));
            assert!(!voice.name().contains(' '), "{}", voice.name());
        }
    }

    #[test]
    fn presets_match_the_original_application() {
        let p = Personality::LittleRobot.preset();
        assert_eq!((p.pitch, p.speed), (90, 150));
        assert_eq!(p.quality, Quality::Monotone);
        assert_eq!(p.effort, Effort::Normal);
        let p = Personality::Mellow.preset();
        assert_eq!((p.pitch, p.speed), (190, 140));
        assert_eq!(p.effort, Effort::Breathy);
        let p = Personality::SingingBoy.preset();
        assert_eq!((p.pitch, p.speed), (310, 90));
        assert_eq!(p.quality, Quality::Sung);
    }
}
