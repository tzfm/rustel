/*
Scale lookup adapted from Strudel packages/xen/tunejs.js.
Copyright (C) 2022 Strudel contributors

Tune.js and its tuning archive have separate credits and MIT terms in
crates/core/LICENSE-tunejs.

Rust implementation and additions:
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! Tune.js-compatible scale presets and ratio lookup.
//!
//! Scale frequency tables are strudel.cc's TuningList from
//! `packages/xen/tunejs.js`, embedded as gzipped JSON. The `tuning-list`
//! feature holds the table. Without the feature the table is empty and a
//! named scale is unknown.

use once_cell::sync::Lazy;
use serde::Deserialize;
use std::collections::HashMap;

type Table = HashMap<String, Vec<f64>>;

static TUNING_LIST: Lazy<Table> = Lazy::new(load_table);

#[cfg(feature = "tuning-list")]
fn load_table() -> Table {
    use flate2::read::GzDecoder;
    use std::io::Read;

    let compressed = include_bytes!("../data/tuning_list.json.gz");
    let mut decoder = GzDecoder::new(&compressed[..]);
    let mut json = String::new();
    decoder
        .read_to_string(&mut json)
        .expect("decompress embedded TuningList");
    serde_json::from_str(&json).expect("parse embedded TuningList")
}

#[cfg(not(feature = "tuning-list"))]
fn load_table() -> Table {
    Table::new()
}

/// Tune.js engine: load a named scale or raw frequency list, then map steps.
#[derive(Clone, Debug)]
pub struct Tune {
    pub scale: Vec<f64>,
    pub tonic: f64,
}

impl Default for Tune {
    fn default() -> Self {
        Self {
            scale: Vec::new(),
            tonic: 440.0,
        }
    }
}

impl Tune {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tonicize(&mut self, tonic: f64) {
        self.tonic = tonic;
    }

    pub fn is_valid_scale(scale: &ScaleSpec) -> bool {
        match scale {
            ScaleSpec::Name(name) => TUNING_LIST.contains_key(name),
            ScaleSpec::Frequencies(freqs) => is_array_of_numbers(freqs),
        }
    }

    pub fn load_scale(&mut self, scale: &ScaleSpec) -> Result<(), String> {
        let freqs = match scale {
            ScaleSpec::Name(name) => TUNING_LIST
                .get(name)
                .cloned()
                .ok_or_else(|| format!("not a valid tune.js scale name: \"{name}\""))?,
            ScaleSpec::Frequencies(freqs) => {
                if !is_array_of_numbers(freqs) {
                    return Err("tune scale frequency list must contain only numbers".into());
                }
                freqs.clone()
            }
        };
        if freqs.is_empty() {
            return Err("tune scale frequency list must be non-empty".into());
        }
        let root = freqs[0];
        self.scale = freqs[..freqs.len().saturating_sub(1)]
            .iter()
            .map(|freq| freq / root)
            .collect();
        Ok(())
    }

    /// Upstream `Tune.note` / `frequency` with output mode frequency.
    pub fn note(&self, step_in: f64, octave_in: Option<f64>) -> f64 {
        if self.scale.is_empty() {
            return 0.0;
        }
        let len = self.scale.len() as f64;
        let mut octave = (step_in / len).floor();
        if let Some(extra) = octave_in {
            octave += extra;
        }
        let mut scale_degree = step_in % len;
        while scale_degree < 0.0 {
            scale_degree += len;
        }
        // A tiny negative remainder plus `len` can round up to `len`; that
        // degree is past the table and yields NaN, as in Tune.js.
        let Some(ratio) = self.scale.get(scale_degree as usize) else {
            return f64::NAN;
        };
        let mut freq = self.tonic * ratio;
        freq *= 2f64.powf(octave);
        (freq * 100_000_000_000.0).floor() / 100_000_000_000.0
    }

    pub fn scale_names() -> Vec<&'static str> {
        // Materialize once into a leaked sorted list for stable iteration in tests.
        static NAMES: Lazy<Vec<&'static str>> = Lazy::new(|| {
            let mut names: Vec<String> = TUNING_LIST.keys().cloned().collect();
            names.sort();
            names
                .into_iter()
                .map(|name| Box::leak(name.into_boxed_str()) as &'static str)
                .collect()
        });
        NAMES.clone()
    }

    pub fn scale_count() -> usize {
        TUNING_LIST.len()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ScaleSpec {
    Name(String),
    Frequencies(Vec<f64>),
}

impl ScaleSpec {
    pub fn name(name: impl Into<String>) -> Self {
        Self::Name(name.into())
    }
}

fn is_array_of_numbers(freqs: &[f64]) -> bool {
    !freqs.is_empty() && freqs.iter().all(|value| value.is_finite())
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
#[allow(dead_code)]
enum Unused {
    Numbers(Vec<f64>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "tuning-list")]
    #[test]
    fn embeds_the_complete_strudel_tuning_list() {
        assert_eq!(Tune::scale_count(), 3304);
        assert!(Tune::is_valid_scale(&ScaleSpec::name("hexany15")));
        assert!(Tune::is_valid_scale(&ScaleSpec::name("tranh3")));
    }

    #[cfg(feature = "tuning-list")]
    #[test]
    fn hexany15_ratios_match_tunejs_ratio_mode() {
        let mut tune = Tune::new();
        tune.load_scale(&ScaleSpec::name("hexany15")).unwrap();
        tune.tonicize(1.0);
        // freqs/root for degrees 0..n-1
        assert!((tune.note(0.0, None) - 1.0).abs() < 1e-12);
        assert!(tune.note(1.0, None) > 1.0);
        assert!((tune.note(5.0, None) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn accepts_inline_frequency_lists() {
        let mut tune = Tune::new();
        tune.load_scale(&ScaleSpec::Frequencies(vec![
            261.6255653006,
            302.72962012827,
            350.29154279212,
            405.32593044476,
            469.00678383895,
            523.2511306012,
        ]))
        .unwrap();
        tune.tonicize(1.0);
        assert!((tune.note(0.0, None) - 1.0).abs() < 1e-12);
        assert!((tune.note(5.0, None) - 2.0).abs() < 1e-9);
    }

    #[cfg(not(feature = "tuning-list"))]
    #[test]
    fn has_no_named_scales_without_the_tuning_list() {
        let name = ScaleSpec::name("hexany15");
        assert_eq!(Tune::scale_count(), 0);
        assert!(Tune::scale_names().is_empty());
        assert!(!Tune::is_valid_scale(&name));
        assert!(Tune::new().load_scale(&name).is_err());
    }
}
