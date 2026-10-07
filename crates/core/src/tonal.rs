/*
tonal.rs - Native EDO, xen and pitch arithmetic
Pitch helpers adapted from Strudel packages/xen/xen.mjs.
Copyright (C) 2022 Strudel contributors

Rust implementation and additions:
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

pub const DEFAULT_XEN_BASE: f64 = 220.0;

/// The bounded EDO ratio table that [`crate::xen::edo`] builds.
pub fn edo(name: &str) -> Result<Vec<f64>, String> {
    crate::xen::edo(name)
}

pub fn with_base(scale: &[f64], base: f64) -> Vec<f64> {
    scale.iter().map(|ratio| ratio * base).collect()
}

pub fn xen_offset(scale: &[f64], offset: i64, index: i64) -> Option<f64> {
    let len = i64::try_from(scale.len()).ok()?;
    if len == 0 {
        return None;
    }
    let scale_index = (index + offset).rem_euclid(len) as usize;
    // Preserve strudel.cc's `Math.floor(offset / length)`, including the
    // counter-intuitive negative-offset octave behavior.
    let octave = (offset as f64 / len as f64).floor();
    Some(scale[scale_index] * 2f64.powf(octave))
}

pub fn xen_frequency(scale: &[f64], offset: i64) -> Option<f64> {
    xen_offset(&with_base(scale, DEFAULT_XEN_BASE), offset, 0).map(trim_frequency)
}

pub fn ftranspose(frequency: f64, steps: f64, edo_size: f64) -> f64 {
    trim_frequency(frequency * 2f64.powf(steps / edo_size))
}

pub fn rescale_base(frequency: f64, base: f64, original_base: Option<f64>) -> f64 {
    frequency * base / original_base.unwrap_or(DEFAULT_XEN_BASE)
}

pub fn edo_ratio(division: i64, divisions: i64) -> f64 {
    if division == 0 {
        1.0
    } else {
        2f64.powf(division as f64 / divisions as f64)
    }
}

pub fn midi_to_hz(note: f64, tuning: f64) -> f64 {
    tuning * 2f64.powf((note - 69.0) / 12.0)
}

pub fn hz_to_midi(frequency: f64, tuning: f64) -> f64 {
    12.0 * (frequency / tuning).log2() + 69.0
}

pub(crate) fn trim_frequency(frequency: f64) -> f64 {
    if frequency == 0.0 || !frequency.is_finite() {
        return frequency;
    }
    let magnitude = frequency.abs().log10().floor();
    let factor = 10f64.powf(9.0 - magnitude);
    (frequency * factor).round() / factor
}

#[cfg(test)]
mod edo_tests {
    use super::edo;

    #[test]
    fn edo_refuses_a_table_above_the_shared_division_limit() {
        assert_eq!(edo("65536edo").unwrap().len(), 65_536);
        assert!(edo("65537edo").is_err());
    }

    #[test]
    fn edo_keeps_the_equal_temperament_ratios() {
        let ratios = edo("12edo").unwrap();
        assert_eq!(ratios.len(), 12);
        assert_eq!(ratios[0], 1.0);
        assert!((ratios[7] - 2f64.powf(7.0 / 12.0)).abs() < 1e-12);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xen_edo_and_offset_match_the_strudel_examples() {
        let scale = edo("5edo").unwrap();
        assert_eq!(scale.len(), 5);
        assert_eq!(scale[0], 1.0);
        assert!((scale[2] - 2f64.powf(2.0 / 5.0)).abs() < 1e-12);
        assert_eq!(xen_frequency(&scale, 0), Some(220.0));
        assert_eq!(xen_frequency(&scale, 5), Some(440.0));
        // Upstream applies `parseFloat(freq.toPrecision(10))`.
        assert_eq!(xen_frequency(&scale, -1), Some(191.521_123_9));
    }

    #[test]
    fn validates_edo_names_like_javascript() {
        for invalid in ["edo", "0edo", "05edo", "-5edo", "5EDO", "5.0edo"] {
            assert!(edo(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn pitch_conversions_round_trip() {
        let frequency = midi_to_hz(60.0, 440.0);
        assert!((frequency - 261.625_565_300_598_6).abs() < 1e-10);
        assert!((hz_to_midi(frequency, 440.0) - 60.0).abs() < 1e-12);
    }
}
