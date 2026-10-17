//! The twenty Talk It! voices and the settings each one starts from.
//!
//! Each personality is only a preset of pitch, speed, pitch quality and vocal
//! effort, exactly as the original application treated them: the engine has no
//! per-voice timbre of its own.

use crate::params::{Effort, Quality};

/// A named voice preset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Personality {
    Male,
    Female,
    LargeMale,
    Child,
    GiantMale,
    MellowFemale,
    MellowMale,
    CrispMale,
    TheFly,
    Robotoid,
    Martian,
    Colossus,
    FastFred,
    OldWoman,
    Munchkin,
    Troll,
    Nerd,
    Milktoast,
    Tipsy,
    Choirboy,
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
        Self::Male,
        Self::Female,
        Self::LargeMale,
        Self::Child,
        Self::GiantMale,
        Self::MellowFemale,
        Self::MellowMale,
        Self::CrispMale,
        Self::TheFly,
        Self::Robotoid,
        Self::Martian,
        Self::Colossus,
        Self::FastFred,
        Self::OldWoman,
        Self::Munchkin,
        Self::Troll,
        Self::Nerd,
        Self::Milktoast,
        Self::Tipsy,
        Self::Choirboy,
    ];

    /// The name a score or the command line writes: the label in lowercase
    /// with a hyphen for each space, so the name stays one word in
    /// mini-notation. [`Personality::parse`] reads it back.
    pub fn name(self) -> &'static str {
        match self {
            Self::Male => "male",
            Self::Female => "female",
            Self::LargeMale => "large-male",
            Self::Child => "child",
            Self::GiantMale => "giant-male",
            Self::MellowFemale => "mellow-female",
            Self::MellowMale => "mellow-male",
            Self::CrispMale => "crisp-male",
            Self::TheFly => "the-fly",
            Self::Robotoid => "robotoid",
            Self::Martian => "martian",
            Self::Colossus => "colossus",
            Self::FastFred => "fast-fred",
            Self::OldWoman => "old-woman",
            Self::Munchkin => "munchkin",
            Self::Troll => "troll",
            Self::Nerd => "nerd",
            Self::Milktoast => "milk-toast",
            Self::Tipsy => "tipsy",
            Self::Choirboy => "choir-boy",
        }
    }

    /// The label shown on the original personality button.
    pub fn label(self) -> &'static str {
        match self {
            Self::Male => "Male",
            Self::Female => "Female",
            Self::LargeMale => "Large Male",
            Self::Child => "Child",
            Self::GiantMale => "Giant Male",
            Self::MellowFemale => "Mellow Female",
            Self::MellowMale => "Mellow Male",
            Self::CrispMale => "Crisp Male",
            Self::TheFly => "The Fly",
            Self::Robotoid => "Robotoid",
            Self::Martian => "Martian",
            Self::Colossus => "Colossus",
            Self::FastFred => "Fast Fred",
            Self::OldWoman => "Old Woman",
            Self::Munchkin => "Munchkin",
            Self::Troll => "Troll",
            Self::Nerd => "Nerd",
            Self::Milktoast => "Milk Toast",
            Self::Tipsy => "Tipsy",
            Self::Choirboy => "Choir Boy",
        }
    }

    /// The default settings of this personality, as the original application
    /// set them when its button was pressed.
    pub fn preset(self) -> Preset {
        match self {
            Self::Male => Preset {
                pitch: 100,
                speed: 150,
                quality: Quality::Natural,
                effort: Effort::Normal,
            },
            Self::Female => Preset {
                pitch: 200,
                speed: 150,
                quality: Quality::Natural,
                effort: Effort::Normal,
            },
            Self::LargeMale => Preset {
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
            Self::GiantMale => Preset {
                pitch: 75,
                speed: 140,
                quality: Quality::Natural,
                effort: Effort::Normal,
            },
            Self::MellowFemale => Preset {
                pitch: 190,
                speed: 140,
                quality: Quality::Natural,
                effort: Effort::Breathy,
            },
            Self::MellowMale => Preset {
                pitch: 310,
                speed: 90,
                quality: Quality::Sung,
                effort: Effort::Normal,
            },
            Self::CrispMale => Preset {
                pitch: 200,
                speed: 140,
                quality: Quality::Natural,
                effort: Effort::Normal,
            },
            Self::TheFly => Preset {
                pitch: 480,
                speed: 150,
                quality: Quality::Natural,
                effort: Effort::Normal,
            },
            Self::Robotoid => Preset {
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
            Self::Colossus => Preset {
                pitch: 66,
                speed: 138,
                quality: Quality::Monotone,
                effort: Effort::Normal,
            },
            Self::FastFred => Preset {
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
            Self::Munchkin => Preset {
                pitch: 90,
                speed: 150,
                quality: Quality::Natural,
                effort: Effort::Normal,
            },
            Self::Troll => Preset {
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
            Self::Milktoast => Preset {
                pitch: 120,
                speed: 165,
                quality: Quality::Natural,
                effort: Effort::Normal,
            },
            Self::Tipsy => Preset {
                pitch: 145,
                speed: 115,
                quality: Quality::Sung,
                effort: Effort::Normal,
            },
            Self::Choirboy => Preset {
                pitch: 310,
                speed: 90,
                quality: Quality::Sung,
                effort: Effort::Normal,
            },
        }
    }

    /// Resolve a typed name. Letters alone decide: case, spaces and
    /// punctuation are ignored, so "the-fly", "TheFly" and "the fly" all
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
        assert_eq!(Personality::parse("Robotoid"), Some(Personality::Robotoid));
        assert_eq!(Personality::parse("fast fred"), Some(Personality::FastFred));
        assert_eq!(
            Personality::parse("MILK-TOAST"),
            Some(Personality::Milktoast)
        );
        assert_eq!(Personality::parse("unknown"), None);
        for voice in Personality::ALL {
            assert_eq!(Personality::parse(voice.name()), Some(voice));
            assert!(!voice.name().contains(' '), "{}", voice.name());
        }
    }

    #[test]
    fn presets_match_the_original_application() {
        let p = Personality::Robotoid.preset();
        assert_eq!((p.pitch, p.speed), (90, 150));
        assert_eq!(p.quality, Quality::Monotone);
        assert_eq!(p.effort, Effort::Normal);
        let p = Personality::MellowFemale.preset();
        assert_eq!((p.pitch, p.speed), (190, 140));
        assert_eq!(p.effort, Effort::Breathy);
        let p = Personality::Choirboy.preset();
        assert_eq!((p.pitch, p.speed), (310, 90));
        assert_eq!(p.quality, Quality::Sung);
    }
}
