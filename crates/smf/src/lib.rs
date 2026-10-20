/*
lib.rs - Standard MIDI File reading and writing
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

#![deny(clippy::disallowed_types, clippy::disallowed_methods)]

//! Standard MIDI File reading and writing.
//!
//! The crate is pure: it has no audio, scheduler or device code. The command,
//! Studio and runtime use it for MIDI file import and export.
//!
//! Export writes a Type 1 file. Import accepts format 0 and 1 and emits a
//! Rustel score. Both sides use the General MIDI program table and the drum
//! names the runtime already plays.

mod export;
mod import;
mod meter;
mod score;
mod tables;

pub use export::{Message, MessageKind, Timeline, Track, tempo_micros_per_quarter};
pub use import::{ImportOptions, ImportReport, Imported, LaneReport, import_smf, import_smf_file};
pub use meter::{Meter, SmfOptions};
pub use tables::{
    GM_DRUMS, GM_PROGRAMS, base_sound_name, drum_name_for_note, drum_note_for_name,
    program_for_sound, sound_for_program,
};

/// The default tick resolution for an exported file.
pub const DEFAULT_PPQ: u16 = 480;

/// Grid sizes, in steps per bar, that the importer tries in order.
pub const GRID_STEPS_PER_BAR: [u32; 8] = [4, 8, 12, 16, 24, 32, 48, 64];

/// Why a file could not be read or written.
#[derive(Debug)]
pub enum SmfError {
    Io(std::io::Error),
    Parse(String),
    UnsupportedFormat(u8),
    TimecodeTiming,
    InvalidOptions(String),
    TooLarge { limit: usize, size: usize },
    TooManyEvents { limit: usize },
    TooManyBars { limit: u32 },
    TooManyVoices { limit: usize },
    NoNotes,
    TempoOutOfRange(f64),
    TooManyMessages { limit: usize },
    Write(String),
}

impl std::fmt::Display for SmfError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "cannot access the MIDI file: {error}"),
            Self::Parse(message) => write!(formatter, "invalid MIDI file: {message}"),
            Self::UnsupportedFormat(format) => write!(
                formatter,
                "unsupported MIDI file format {format}; use format 0 or 1"
            ),
            Self::TimecodeTiming => formatter.write_str("SMPTE timecode timing is not supported"),
            Self::InvalidOptions(message) => formatter.write_str(message),
            Self::TooLarge { limit, size } => {
                write!(
                    formatter,
                    "the size is {size} bytes, above the {limit}-byte limit"
                )
            }
            Self::TooManyEvents { limit } => {
                write!(formatter, "the MIDI file has more than {limit} events")
            }
            Self::TooManyBars { limit } => {
                write!(formatter, "the MIDI file is longer than {limit} bars")
            }
            Self::TooManyVoices { limit } => {
                write!(formatter, "a lane needs more than {limit} voices")
            }
            Self::NoNotes => formatter.write_str("the MIDI file has no notes"),
            Self::TempoOutOfRange(bpm) => {
                write!(
                    formatter,
                    "the tempo {bpm} BPM cannot be stored in a MIDI file"
                )
            }
            Self::TooManyMessages { limit } => {
                write!(
                    formatter,
                    "the MIDI file would have more than {limit} messages"
                )
            }
            Self::Write(message) => write!(formatter, "cannot write the MIDI file: {message}"),
        }
    }
}

impl std::error::Error for SmfError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for SmfError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
