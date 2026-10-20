/*
export.rs - Write a Standard MIDI File
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! Writing a Type 1 Standard MIDI File.
//!
//! Messages are absolute ticks in the timeline and relative deltas on the
//! wire. A stable sort puts note-offs before note-ons at one tick, so a
//! repeated note retriggers instead of cutting itself off.

use midly::num::{u4, u7, u14, u15, u24, u28};
use midly::{
    Format, Header, MetaMessage, MidiMessage as WireMessage, PitchBend, Smf, Timing, TrackEvent,
    TrackEventKind,
};

use crate::SmfError;
use crate::meter::Meter;

/// Ceiling on channel messages in one exported file.
pub const MAX_MESSAGES: usize = 2_000_000;

/// The largest delta a MIDI variable-length integer can hold.
const MAX_DELTA: u64 = 0x0FFF_FFFF;

/// A file to write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Timeline {
    pub ppq: u16,
    pub tempo_micros_per_quarter: u32,
    pub meter: Meter,
    pub tracks: Vec<Track>,
}

/// One track of a file, one channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    pub name: String,
    pub channel: u8,
    pub messages: Vec<Message>,
}

/// One channel message at an absolute tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Message {
    pub tick: u64,
    pub kind: MessageKind,
}

/// The channel messages a file can carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageKind {
    NoteOn { note: u8, velocity: u8 },
    NoteOff { note: u8 },
    ControlChange { controller: u8, value: u8 },
    ProgramChange { program: u8 },
    PitchBend { value: u16 },
    ChannelAftertouch { pressure: u8 },
}

impl MessageKind {
    /// Order at one tick. A note-off first, a note-on last.
    fn priority(self) -> u8 {
        match self {
            Self::NoteOff { .. } => 0,
            Self::ProgramChange { .. } => 1,
            Self::ControlChange { .. } => 2,
            Self::PitchBend { .. } => 3,
            Self::ChannelAftertouch { .. } => 4,
            Self::NoteOn { .. } => 5,
        }
    }

    fn to_wire(self) -> Result<WireMessage, SmfError> {
        Ok(match self {
            Self::NoteOn { note, velocity } => WireMessage::NoteOn {
                key: byte(note, "note")?,
                vel: byte(velocity, "velocity")?,
            },
            Self::NoteOff { note } => WireMessage::NoteOff {
                key: byte(note, "note")?,
                vel: u7::from(0),
            },
            Self::ControlChange { controller, value } => WireMessage::Controller {
                controller: byte(controller, "controller")?,
                value: byte(value, "controller value")?,
            },
            Self::ProgramChange { program } => WireMessage::ProgramChange {
                program: byte(program, "program")?,
            },
            Self::PitchBend { value } => WireMessage::PitchBend {
                bend: PitchBend(bend(value)?),
            },
            Self::ChannelAftertouch { pressure } => WireMessage::ChannelAftertouch {
                vel: byte(pressure, "aftertouch pressure")?,
            },
        })
    }
}

fn byte(value: u8, what: &str) -> Result<u7, SmfError> {
    u7::try_from(value)
        .ok_or_else(|| SmfError::InvalidOptions(format!("{what} must be 0 to 127, got {value}")))
}

fn bend(value: u16) -> Result<u14, SmfError> {
    u14::try_from(value).ok_or_else(|| {
        SmfError::InvalidOptions(format!("pitch bend must be 0 to 16383, got {value}"))
    })
}

/// Convert beats per minute to the microseconds per quarter note a file stores.
pub fn tempo_micros_per_quarter(bpm: f64) -> Result<u32, SmfError> {
    if !bpm.is_finite() || bpm <= 0.0 {
        return Err(SmfError::TempoOutOfRange(bpm));
    }
    let micros = (60_000_000.0 / bpm).round();
    if !(1.0..=f64::from(0x00FF_FFFF)).contains(&micros) {
        return Err(SmfError::TempoOutOfRange(bpm));
    }
    Ok(micros as u32)
}

impl Timeline {
    /// Write the timeline as a Type 1 Standard MIDI File.
    pub fn to_bytes(&self) -> Result<Vec<u8>, SmfError> {
        if self.ppq == 0 {
            return Err(SmfError::InvalidOptions(
                "ppq must be 1 to 32767, got 0".to_owned(),
            ));
        }
        Meter::new(self.meter.numerator, self.meter.denominator)?;
        if self.tempo_micros_per_quarter == 0 || self.tempo_micros_per_quarter > 0x00FF_FFFF {
            return Err(SmfError::InvalidOptions(format!(
                "tempo must be 1 to 16777215 microseconds per quarter, got {}",
                self.tempo_micros_per_quarter
            )));
        }
        let total: usize = self.tracks.iter().map(|track| track.messages.len()).sum();
        if total > MAX_MESSAGES {
            return Err(SmfError::TooManyMessages {
                limit: MAX_MESSAGES,
            });
        }
        let timing = Timing::Metrical(u15::try_from(self.ppq).ok_or_else(|| {
            SmfError::InvalidOptions(format!("ppq must be 1 to 32767, got {}", self.ppq))
        })?);
        let mut smf = Smf::new(Header::new(Format::Parallel, timing));
        smf.tracks.push(self.tempo_track()?);
        for track in &self.tracks {
            smf.tracks.push(track.to_events()?);
        }
        let mut bytes = Vec::new();
        smf.write_std(&mut bytes)
            .map_err(|error| SmfError::Write(error.to_string()))?;
        Ok(bytes)
    }

    fn tempo_track(&self) -> Result<Vec<TrackEvent<'static>>, SmfError> {
        let tempo = u24::try_from(self.tempo_micros_per_quarter).ok_or_else(|| {
            SmfError::InvalidOptions(format!(
                "tempo must be 1 to 16777215 microseconds per quarter, got {}",
                self.tempo_micros_per_quarter
            ))
        })?;
        Ok(vec![
            TrackEvent {
                delta: u28::from(0),
                kind: TrackEventKind::Meta(MetaMessage::TrackName(b"tempo")),
            },
            TrackEvent {
                delta: u28::from(0),
                kind: TrackEventKind::Meta(MetaMessage::Tempo(tempo)),
            },
            TrackEvent {
                delta: u28::from(0),
                kind: TrackEventKind::Meta(MetaMessage::TimeSignature(
                    self.meter.numerator,
                    self.meter.denominator_power(),
                    24,
                    8,
                )),
            },
            TrackEvent {
                delta: u28::from(0),
                kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
            },
        ])
    }
}

impl Track {
    fn to_events(&self) -> Result<Vec<TrackEvent<'_>>, SmfError> {
        if !(1..=16).contains(&self.channel) {
            return Err(SmfError::InvalidOptions(format!(
                "channel must be 1 to 16, got {}",
                self.channel
            )));
        }
        let channel = u4::try_from(self.channel - 1)
            .ok_or_else(|| SmfError::InvalidOptions("channel is outside 1 to 16".to_owned()))?;
        let mut sorted = self.messages.clone();
        sorted.sort_by(|left, right| {
            left.tick
                .cmp(&right.tick)
                .then_with(|| left.kind.priority().cmp(&right.kind.priority()))
        });
        let mut events = Vec::with_capacity(sorted.len() + 2);
        events.push(TrackEvent {
            delta: u28::from(0),
            kind: TrackEventKind::Meta(MetaMessage::TrackName(self.name.as_bytes())),
        });
        let mut previous = 0u64;
        for message in &sorted {
            let mut delta = message.tick.checked_sub(previous).ok_or_else(|| {
                SmfError::InvalidOptions("message ticks must not decrease".to_owned())
            })?;
            previous = message.tick;
            let split = delta.saturating_sub(1) / MAX_DELTA;
            if split > MAX_MESSAGES as u64 {
                return Err(SmfError::TooManyMessages {
                    limit: MAX_MESSAGES,
                });
            }
            for _ in 0..split {
                events.push(TrackEvent {
                    delta: u28::from(MAX_DELTA as u32),
                    kind: TrackEventKind::Meta(MetaMessage::Marker(&[])),
                });
            }
            delta -= split * MAX_DELTA;
            let delta = u28::try_from(delta as u32).ok_or_else(|| {
                SmfError::InvalidOptions("message delta is outside 28 bits".to_owned())
            })?;
            events.push(TrackEvent {
                delta,
                kind: TrackEventKind::Midi {
                    channel,
                    message: message.kind.to_wire()?,
                },
            });
        }
        events.push(TrackEvent {
            delta: u28::from(0),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        });
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use midly::num::u4;

    fn message(tick: u64, kind: MessageKind) -> Message {
        Message { tick, kind }
    }

    fn timeline(track: Track) -> Timeline {
        Timeline {
            ppq: 480,
            tempo_micros_per_quarter: 500_000,
            meter: Meter::FOUR_FOUR,
            tracks: vec![track],
        }
    }

    #[test]
    fn the_header_and_tempo_track_come_first() {
        let timeline = timeline(Track {
            name: "lead".to_owned(),
            channel: 1,
            messages: vec![message(
                0,
                MessageKind::NoteOn {
                    note: 60,
                    velocity: 100,
                },
            )],
        });
        let bytes = timeline.to_bytes().unwrap();
        let smf = Smf::parse(&bytes).unwrap();
        assert_eq!(smf.header.format, Format::Parallel);
        assert_eq!(smf.header.timing, Timing::Metrical(u15::from(480)));
        assert_eq!(smf.tracks.len(), 2);
        assert_eq!(
            smf.tracks[0][1].kind,
            TrackEventKind::Meta(MetaMessage::Tempo(u24::from(500_000)))
        );
        assert_eq!(
            smf.tracks[0][2].kind,
            TrackEventKind::Meta(MetaMessage::TimeSignature(4, 2, 24, 8))
        );
        assert_eq!(
            smf.tracks[1][0].kind,
            TrackEventKind::Meta(MetaMessage::TrackName(b"lead"))
        );
    }

    #[test]
    fn note_offs_lead_at_a_shared_tick() {
        let timeline = timeline(Track {
            name: "lead".to_owned(),
            channel: 1,
            messages: vec![
                message(
                    0,
                    MessageKind::NoteOn {
                        note: 60,
                        velocity: 100,
                    },
                ),
                message(
                    0,
                    MessageKind::ControlChange {
                        controller: 7,
                        value: 100,
                    },
                ),
                message(0, MessageKind::NoteOff { note: 59 }),
                message(0, MessageKind::ProgramChange { program: 40 }),
            ],
        });
        let bytes = timeline.to_bytes().unwrap();
        let smf = Smf::parse(&bytes).unwrap();
        let kinds: Vec<_> = smf.tracks[1][1..].iter().map(|event| event.kind).collect();
        assert!(matches!(
            kinds[0],
            TrackEventKind::Midi {
                message: WireMessage::NoteOff { .. },
                ..
            }
        ));
        assert!(matches!(
            kinds[1],
            TrackEventKind::Midi {
                message: WireMessage::ProgramChange { .. },
                ..
            }
        ));
        assert!(matches!(
            kinds[2],
            TrackEventKind::Midi {
                message: WireMessage::Controller { .. },
                ..
            }
        ));
        assert!(matches!(
            kinds[3],
            TrackEventKind::Midi {
                message: WireMessage::NoteOn { .. },
                ..
            }
        ));
    }

    #[test]
    fn a_long_gap_is_split_into_valid_deltas() {
        let tick = MAX_DELTA + 5;
        let timeline = timeline(Track {
            name: "late".to_owned(),
            channel: 1,
            messages: vec![message(
                tick,
                MessageKind::NoteOn {
                    note: 60,
                    velocity: 100,
                },
            )],
        });
        let bytes = timeline.to_bytes().unwrap();
        let smf = Smf::parse(&bytes).unwrap();
        let absolute: u64 = smf.tracks[1]
            .iter()
            .map(|event| u64::from(event.delta.as_int()))
            .sum();
        assert_eq!(absolute, tick);
        assert!(
            smf.tracks[1]
                .iter()
                .any(|event| matches!(event.kind, TrackEventKind::Meta(MetaMessage::Marker(b""))))
        );
    }

    #[test]
    fn a_gap_that_needs_too_many_split_events_is_refused() {
        let timeline = timeline(Track {
            name: "late".to_owned(),
            channel: 1,
            messages: vec![message(u64::MAX, MessageKind::NoteOff { note: 0 })],
        });
        assert!(matches!(
            timeline.to_bytes(),
            Err(SmfError::TooManyMessages { .. })
        ));
    }

    #[test]
    fn invalid_values_are_refused() {
        let mut invalid = timeline(Track {
            name: "lead".to_owned(),
            channel: 1,
            messages: vec![message(
                0,
                MessageKind::NoteOn {
                    note: 128,
                    velocity: 100,
                },
            )],
        });
        assert!(invalid.to_bytes().is_err());
        invalid.tracks[0].messages[0].kind = MessageKind::PitchBend { value: 16_384 };
        assert!(invalid.to_bytes().is_err());
        invalid.tracks[0].channel = 0;
        assert!(invalid.to_bytes().is_err());
        invalid.tracks[0].channel = 1;
        invalid.ppq = 0;
        assert!(invalid.to_bytes().is_err());
        invalid.ppq = 480;
        invalid.tempo_micros_per_quarter = 0;
        assert!(invalid.to_bytes().is_err());
    }

    #[test]
    fn the_bytes_are_deterministic() {
        let timeline = timeline(Track {
            name: "lead".to_owned(),
            channel: 1,
            messages: vec![
                message(
                    0,
                    MessageKind::NoteOn {
                        note: 60,
                        velocity: 100,
                    },
                ),
                message(480, MessageKind::NoteOff { note: 60 }),
            ],
        });
        assert_eq!(timeline.to_bytes().unwrap(), timeline.to_bytes().unwrap());
    }

    #[test]
    fn tempo_conversion_rounds_and_bounds() {
        assert_eq!(tempo_micros_per_quarter(120.0).unwrap(), 500_000);
        assert_eq!(tempo_micros_per_quarter(60.0).unwrap(), 1_000_000);
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, 1.0e-9] {
            assert!(tempo_micros_per_quarter(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_channel_is_one_based_in_the_timeline() {
        let timeline = timeline(Track {
            name: "lead".to_owned(),
            channel: 16,
            messages: vec![message(0, MessageKind::NoteOff { note: 0 })],
        });
        let bytes = timeline.to_bytes().unwrap();
        let smf = Smf::parse(&bytes).unwrap();
        assert!(matches!(
            smf.tracks[1][1].kind,
            TrackEventKind::Midi {
                channel,
                ..
            } if channel == u4::from(15)
        ));
    }
}
