//! A Talk It! style speech synthesizer in native Rust.
//!
//! The original Microsoft Talk It! spoke through the closed SoftVoice engine
//! and offered twenty voice presets over pitch, speed, pitch quality and
//! vocal effort, in English and Spanish. This crate keeps that parameter
//! model and rebuilds the engine from measurements of its output: text
//! becomes the engine's phonetic notation by language rules, the notation
//! becomes a timeline of formant targets, and a cascade formant synthesizer
//! speaks the timeline in the engine's 8-bit steps at 11.025 kHz.
//!
//! The public surface is small on purpose:
//!
//! - [`params::Params`] holds one utterance's settings, including the twenty
//!   [`personality::Personality`] presets.
//! - [`render::render`] turns text and settings into samples, and
//!   [`render::render_phonetic`] does the same for the notation.
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

mod narrator;
pub mod phonetic;

/// The sample rate speech is synthesized at, in Hz.
pub const RATE: u32 = 11_025;
