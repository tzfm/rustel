//! A Talk It! style speech synthesizer in native Rust.
//!
//! The original Microsoft Talk It! spoke through the closed SoftVoice engine
//! and offered twenty voice presets over pitch, speed, pitch quality and
//! vocal effort, in English and Spanish. This crate keeps that parameter
//! model and replaces the engine: text becomes phonemes by language rules,
//! the phonemes become a timeline of formant targets, and a cascade formant
//! synthesizer speaks the timeline.
//!
//! The public surface is small on purpose:
//!
//! - [`params::Params`] holds one utterance's settings, including the twenty
//!   [`personality::Personality`] presets.
//! - [`render::render`] turns text and settings into samples.
//! - [`wav::write_wav`] and [`playback::play`] deliver them.
//!
//! ```no_run
//! use rustel_talkit::{params::Params, render, wav};
//!
//! let samples = render::render("hello world", &Params::default());
//! wav::write_wav("hello.wav", &samples, rustel_talkit::RATE).unwrap();
//! ```

pub mod lang;
pub mod params;
pub mod personality;
#[cfg(feature = "play")]
pub mod playback;
pub mod render;
pub mod wav;

mod phoneme;
mod prosody;
mod synth;
mod utterance;

/// The sample rate speech is synthesized at, in Hz.
pub const RATE: u32 = 22_050;
