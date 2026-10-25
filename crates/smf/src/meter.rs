/*
meter.rs - Time signature and tick resolution
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! Time signature and tick resolution for MIDI files.

use crate::SmfError;

/// A time signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Meter {
    pub numerator: u8,
    pub denominator: u8,
}

impl Meter {
    pub const FOUR_FOUR: Self = Self {
        numerator: 4,
        denominator: 4,
    };

    /// Validate a meter.
    pub fn new(numerator: u8, denominator: u8) -> Result<Self, SmfError> {
        if !(1..=64).contains(&numerator) {
            return Err(SmfError::InvalidOptions(format!(
                "meter numerator must be 1 to 64, got {numerator}"
            )));
        }
        if denominator == 0 || !denominator.is_power_of_two() || denominator > 64 {
            return Err(SmfError::InvalidOptions(format!(
                "meter denominator must be a power of two from 1 to 64, got {denominator}"
            )));
        }
        Ok(Self {
            numerator,
            denominator,
        })
    }

    /// The denominator as a power of two, the form a MIDI file stores.
    pub fn denominator_power(self) -> u8 {
        self.denominator.trailing_zeros() as u8
    }

    /// Ticks in one bar at `ppq` ticks per quarter note.
    pub fn ticks_per_bar(self, ppq: u16) -> f64 {
        f64::from(ppq) * 4.0 * f64::from(self.numerator) / f64::from(self.denominator)
    }
}

/// Tick resolution and meter for an exported file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SmfOptions {
    pub ppq: u16,
    pub meter: Meter,
}

impl Default for SmfOptions {
    fn default() -> Self {
        Self {
            ppq: crate::DEFAULT_PPQ,
            meter: Meter::FOUR_FOUR,
        }
    }
}

impl SmfOptions {
    /// Validate both fields.
    pub fn validate(self) -> Result<(), SmfError> {
        if !(1..=32767).contains(&self.ppq) {
            return Err(SmfError::InvalidOptions(format!(
                "ppq must be 1 to 32767, got {}",
                self.ppq
            )));
        }
        Meter::new(self.meter.numerator, self.meter.denominator)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_ticks_follow_the_meter() {
        assert_eq!(Meter::FOUR_FOUR.ticks_per_bar(480), 1920.0);
        assert_eq!(Meter::new(3, 4).unwrap().ticks_per_bar(480), 1440.0);
        assert_eq!(Meter::new(6, 8).unwrap().ticks_per_bar(480), 1440.0);
    }

    #[test]
    fn denominators_are_powers_of_two_up_to_64() {
        assert_eq!(Meter::new(4, 4).unwrap().denominator_power(), 2);
        assert_eq!(Meter::new(6, 8).unwrap().denominator_power(), 3);
        assert!(Meter::new(4, 3).is_err());
        assert!(Meter::new(4, 128).is_err());
        assert!(Meter::new(0, 4).is_err());
        assert!(Meter::new(65, 4).is_err());
    }

    #[test]
    fn options_refuse_a_zero_resolution_and_an_invalid_meter() {
        assert!(SmfOptions::default().validate().is_ok());
        assert!(
            SmfOptions {
                ppq: 0,
                ..SmfOptions::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            SmfOptions {
                ppq: 32_768,
                ..SmfOptions::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            SmfOptions {
                meter: Meter {
                    numerator: 4,
                    denominator: 3,
                },
                ..SmfOptions::default()
            }
            .validate()
            .is_err()
        );
    }
}
