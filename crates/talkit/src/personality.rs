//! The twenty Talk It! voices and the settings each one starts from.
//!
//! A personality is a preset of pitch, speed, pitch quality and vocal effort,
//! as the original application's buttons set them, and a [`Timbre`]: the
//! original engine took the personality as a voice of its own as well.

use crate::params::{Effort, Quality};

/// The sound of a voice apart from its settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timbre {
    /// The factors on the first three formants, the shape of the vocal
    /// tract: all above 1 for a short tract, as a child's. F4 and F5 follow
    /// the third.
    pub formants: [f32; 3],
    /// The factor on every vowel's length. A sung voice holds its vowels.
    pub vowels: f32,
    /// The brightness of the glottal source, from -1 to 1. Below 0 the
    /// source is softened, above 0 it buzzes: at 1 each closure snaps and
    /// the voice reaches up to its third formant at nearly full strength, a
    /// machine voice.
    pub tone: f32,
}

impl Timbre {
    /// The adult voice the formant tables describe.
    pub const PLAIN: Self = timbre([1.0, 1.0, 1.0], 1.0, PLAIN_TONE);
}

const fn timbre(formants: [f32; 3], vowels: f32, tone: f32) -> Timbre {
    Timbre {
        formants,
        vowels,
        tone,
    }
}

/// The plain voice's tone.
const PLAIN_TONE: f32 = 0.35;

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

    /// The voice's own sound, measured from the original engine's output:
    /// each voice speaking the same vowels at the same steady pitch, against
    /// the plain male voice.
    pub fn timbre(self) -> Timbre {
        match self {
            Self::Man => Timbre::PLAIN,
            Self::Woman => timbre([1.24, 1.11, 1.16], 1.02, 0.25),
            Self::HyperFemale => timbre([1.24, 1.11, 1.16], 1.12, 0.20),
            Self::Child => timbre([1.69, 1.43, 1.43], 0.99, 0.55),
            Self::StrongMan => timbre([0.87, 0.91, 0.92], 1.01, 0.15),
            Self::Mellow => timbre([1.26, 1.11, 1.19], 1.21, 0.10),
            Self::SingingGirl => timbre([1.39, 1.29, 1.31], 2.46, 0.50),
            Self::StrongWoman => timbre([1.20, 1.08, 1.12], 1.16, 0.10),
            Self::Fly => timbre([1.86, 1.84, 1.57], 0.44, 0.25),
            Self::LittleRobot => timbre([1.45, 1.03, 1.00], 0.96, 1.0),
            Self::Martian => timbre([1.12, 1.00, 0.99], 1.04, 0.0),
            Self::BigRobot => timbre([1.05, 0.99, 1.01], 1.14, 1.0),
            Self::HyperMale => timbre([1.0, 1.0, 1.0], 1.0, 0.15),
            Self::OldWoman => timbre([1.28, 1.12, 1.16], 0.98, 0.40),
            Self::LittleMan => timbre([1.48, 1.38, 1.37], 1.00, 0.40),
            Self::ImaginaryMan => timbre([1.08, 1.10, 1.15], 1.64, 0.50),
            Self::Nerd => timbre([1.39, 1.00, 1.00], 0.79, 0.65),
            Self::Whiner => timbre([0.89, 1.20, 1.22], 1.00, 0.75),
            Self::Wobbly => timbre([1.0, 1.0, 1.0], 2.44, 0.30),
            Self::SingingBoy => timbre([1.16, 1.04, 1.12], 2.49, -0.85),
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
