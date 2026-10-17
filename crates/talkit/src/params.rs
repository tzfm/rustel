//! Everything one utterance is spoken with, and how `key=value` settings
//! fill it in. The command line and a score name the settings the same way,
//! so this module is the one place that reads them.

use std::ops::RangeInclusive;

use crate::lang::Language;
use crate::personality::{Personality, Timbre};

/// Pitch contour style. The numbers are the `SVSetF0Style` values the
/// original engine used for the three buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Quality {
    Natural,
    Monotone,
    Sung,
}

impl Quality {
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "natural" => Some(Self::Natural),
            "monotone" => Some(Self::Monotone),
            "sung" => Some(Self::Sung),
            _ => None,
        }
    }
}

/// Vocal effort, the `SVSetVoicingMode` setting of the original engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Effort {
    Normal,
    Breathy,
    Whispered,
}

impl Effort {
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "normal" => Some(Self::Normal),
            "breathy" => Some(Self::Breathy),
            "whispered" => Some(Self::Whispered),
            _ => None,
        }
    }
}

/// The closed pitch and speed ranges of the original application.
pub const PITCH_MIN: f32 = 50.0;
pub const PITCH_MAX: f32 = 500.0;
pub const SPEED_MIN: u32 = 50;
pub const SPEED_MAX: u32 = 400;

/// The base pitches a musical note can ask for, in Hz: C1 to C7. A note
/// reaches past the pitch setting's range, so a melody keeps its octave.
pub const NOTE_PITCH: RangeInclusive<f32> = 32.70..=2093.0;

/// One utterance's settings. Pitch is the base F0 in Hz, the pitch the voice
/// starts on, and speed is a percentage of the nominal 150. The timbre comes
/// with the personality and no single setting changes it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    pub pitch: f32,
    pub speed: u32,
    pub quality: Quality,
    pub effort: Effort,
    pub language: Language,
    pub timbre: Timbre,
}

impl Default for Params {
    fn default() -> Self {
        let p = Personality::Man.preset();
        Self {
            pitch: p.pitch as f32,
            speed: p.speed,
            quality: p.quality,
            effort: p.effort,
            language: Language::English,
            timbre: Personality::Man.timbre(),
        }
    }
}

/// One setting of an utterance, by its long name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    Personality,
    Pitch,
    Speed,
    Quality,
    Effort,
    Language,
}

impl Setting {
    /// The setting `key` names, in any case: the long name or a short form.
    pub fn parse(key: &str) -> Option<Self> {
        match key.to_ascii_lowercase().as_str() {
            "personality" | "voice" | "v" => Some(Self::Personality),
            "pitch" | "p" => Some(Self::Pitch),
            "speed" | "rate" | "s" => Some(Self::Speed),
            "quality" | "q" => Some(Self::Quality),
            "effort" | "e" => Some(Self::Effort),
            "language" | "lang" | "l" => Some(Self::Language),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Personality => "personality",
            Self::Pitch => "pitch",
            Self::Speed => "speed",
            Self::Quality => "quality",
            Self::Effort => "effort",
            Self::Language => "language",
        }
    }

    /// The values the setting takes, as an error message says them.
    fn takes(self) -> String {
        match self {
            Self::Personality => {
                let names: Vec<&str> = Personality::ALL.iter().map(|p| p.name()).collect();
                format!("one of {}", names.join(", "))
            }
            Self::Pitch => format!("{PITCH_MIN} to {PITCH_MAX} (Hz)"),
            Self::Speed => format!("a whole number from {SPEED_MIN} to {SPEED_MAX}"),
            Self::Quality => "natural, monotone or sung".into(),
            Self::Effort => "normal, breathy or whispered".into(),
            Self::Language => "english or spanish".into(),
        }
    }
}

/// Why a setting was refused, in a form the command line and the score can
/// both print as is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParamError {
    UnknownKey(String),
    BadValue { setting: Setting, value: String },
}

impl std::fmt::Display for ParamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownKey(key) => write!(
                f,
                "`{key}` is not a setting; the settings are personality, pitch, speed, quality, \
                 effort and language"
            ),
            Self::BadValue { setting, value } => {
                write!(
                    f,
                    "{} takes {}, not `{value}`",
                    setting.name(),
                    setting.takes()
                )
            }
        }
    }
}

impl Params {
    /// Apply one `key=value` pair. Pairs apply in the order given, see
    /// [`Params::set`].
    pub fn apply(&mut self, key: &str, value: &str) -> Result<(), ParamError> {
        let setting = Setting::parse(key).ok_or_else(|| ParamError::UnknownKey(key.into()))?;
        self.set(setting, value)
    }

    /// Set one setting from its written value. A personality applies its
    /// whole preset, exactly as pressing the original button did, so the
    /// single settings after it override it.
    pub fn set(&mut self, setting: Setting, value: &str) -> Result<(), ParamError> {
        let bad = || ParamError::BadValue {
            setting,
            value: value.into(),
        };
        match setting {
            Setting::Personality => {
                let personality = Personality::parse(value).ok_or_else(bad)?;
                let preset = personality.preset();
                self.pitch = preset.pitch as f32;
                self.speed = preset.speed;
                self.quality = preset.quality;
                self.effort = preset.effort;
                self.timbre = personality.timbre();
            }
            Setting::Pitch => {
                self.pitch = value
                    .trim()
                    .parse()
                    .ok()
                    .filter(|hz| (PITCH_MIN..=PITCH_MAX).contains(hz))
                    .ok_or_else(bad)?;
            }
            Setting::Speed => {
                self.speed = value
                    .trim()
                    .parse()
                    .ok()
                    .filter(|speed| (SPEED_MIN..=SPEED_MAX).contains(speed))
                    .ok_or_else(bad)?;
            }
            Setting::Quality => self.quality = Quality::parse(value).ok_or_else(bad)?,
            Setting::Effort => self.effort = Effort::parse(value).ok_or_else(bad)?,
            Setting::Language => self.language = Language::parse(value).ok_or_else(bad)?,
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> Result<Params, ParamError> {
        let mut p = Params::default();
        for (k, v) in pairs {
            p.apply(k, v)?;
        }
        Ok(p)
    }

    #[test]
    fn personality_applies_its_whole_preset() {
        let p = params(&[("personality", "singingboy")]).unwrap();
        assert_eq!((p.pitch, p.speed), (310.0, 90));
        assert_eq!(p.quality, Quality::Sung);
    }

    #[test]
    fn later_pairs_override_a_preset() {
        let p = params(&[
            ("personality", "little-robot"),
            ("pitch", "250"),
            ("quality", "sung"),
        ])
        .unwrap();
        assert_eq!(p.pitch, 250.0);
        assert_eq!(p.quality, Quality::Sung);
        assert_eq!(p.speed, 150);
    }

    #[test]
    fn ranges_refuse_out_of_bounds_values() {
        assert!(params(&[("pitch", "49")]).is_err());
        assert!(params(&[("pitch", "501")]).is_err());
        assert!(params(&[("pitch", "NaN")]).is_err());
        assert!(params(&[("speed", "401")]).is_err());
        assert!(params(&[("speed", "150.5")]).is_err());
        assert!(params(&[("speed", "300")]).is_ok());
        assert_eq!(params(&[("pitch", "130.81")]).unwrap().pitch, 130.81);
    }

    #[test]
    fn unknown_keys_and_bad_values_name_what_they_take() {
        let unknown = params(&[("timbre", "warm")]).unwrap_err();
        assert_eq!(unknown, ParamError::UnknownKey("timbre".into()));
        assert!(unknown.to_string().contains("pitch, speed"), "{unknown}");
        let effort = params(&[("effort", "loud")]).unwrap_err().to_string();
        assert_eq!(
            effort,
            "effort takes normal, breathy or whispered, not `loud`"
        );
        let voice = params(&[("v", "daft")]).unwrap_err().to_string();
        assert!(voice.contains("hyper-female"), "{voice}");
        assert!(voice.contains("singing-boy"), "{voice}");
    }

    #[test]
    fn single_letter_keys_reach_the_same_settings() {
        let p = params(&[
            ("v", "little-robot"),
            ("p", "250"),
            ("s", "90"),
            ("q", "sung"),
            ("e", "whispered"),
            ("l", "spanish"),
        ])
        .unwrap();
        assert_eq!(p.pitch, 250.0);
        assert_eq!(p.speed, 90);
        assert_eq!(p.quality, Quality::Sung);
        assert_eq!(p.effort, Effort::Whispered);
        assert_eq!(p.language, Language::Spanish);
    }
}
