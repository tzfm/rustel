/*
input.rs - MIDI input: listening to a controller
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! Reading MIDI in.
//!
//! Two jobs, sharing one decoder. `rustel midi-monitor` shows a musician what
//! their controller actually sends - which is the only reliable way to learn a
//! knob's CC number, since the panel legend and the wire rarely agree. The same
//! [`MidiListener`] is what a live `midin()` reads from.
//!
//! Nothing here may end a set, and nothing here may block the thread that
//! queries patterns: the driver calls us on its thread, so the callback does
//! the least possible work and hands the value on.

use std::sync::{Arc, Mutex};

use midir::MidiInputConnection;

/// One decoded message.
///
/// Channels are 1-16 here, the way a score writes `midichan`, not the 0-15 the
/// wire uses - a musician reading the monitor should see the number they would
/// type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MidiEvent {
    NoteOn {
        channel: u8,
        note: u8,
        velocity: u8,
    },
    NoteOff {
        channel: u8,
        note: u8,
        velocity: u8,
    },
    ControlChange {
        channel: u8,
        controller: u8,
        value: u8,
    },
    ProgramChange {
        channel: u8,
        program: u8,
    },
    ChannelAftertouch {
        channel: u8,
        pressure: u8,
    },
    PolyAftertouch {
        channel: u8,
        note: u8,
        pressure: u8,
    },
    PitchBend {
        channel: u8,
        value: u16,
    },
    Clock,
    Start,
    Continue,
    Stop,
    ActiveSensing,
}

impl MidiEvent {
    /// The channel this message is on, if it is a channel message.
    pub fn channel(&self) -> Option<u8> {
        match self {
            Self::NoteOn { channel, .. }
            | Self::NoteOff { channel, .. }
            | Self::ControlChange { channel, .. }
            | Self::ProgramChange { channel, .. }
            | Self::ChannelAftertouch { channel, .. }
            | Self::PolyAftertouch { channel, .. }
            | Self::PitchBend { channel, .. } => Some(*channel),
            _ => None,
        }
    }

    /// A short label for grouping in a summary.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NoteOn { .. } => "note-on",
            Self::NoteOff { .. } => "note-off",
            Self::ControlChange { .. } => "cc",
            Self::ProgramChange { .. } => "program",
            Self::ChannelAftertouch { .. } => "aftertouch",
            Self::PolyAftertouch { .. } => "poly-aftertouch",
            Self::PitchBend { .. } => "bend",
            Self::Clock => "clock",
            Self::Start => "start",
            Self::Continue => "continue",
            Self::Stop => "stop",
            Self::ActiveSensing => "active-sensing",
        }
    }
}

/// Decode one message.
///
/// Returns `None` for anything we do not model (sysex, undefined status bytes)
/// rather than guessing - the monitor prints those as raw bytes, which is more
/// honest than naming them wrongly.
pub fn decode(bytes: &[u8]) -> Option<MidiEvent> {
    let status = *bytes.first()?;
    // 1-16 as a score writes it.
    let channel = (status & 0x0f) + 1;
    // Channel data bytes must be present and must actually be data. Masking a
    // status byte into 0..=127 accepts malformed/truncated packets as a valid
    // control or note and can move a live parameter spuriously.
    let at = |i: usize| bytes.get(i).copied().filter(|byte| byte & 0x80 == 0);
    Some(match status & 0xf0 {
        // A note-on at velocity zero IS a note-off on the wire; running status
        // from a keyboard uses it constantly, and showing it as a note-on
        // would make every release look like a stuck note.
        0x90 if at(2)? == 0 => MidiEvent::NoteOff {
            channel,
            note: at(1)?,
            velocity: 0,
        },
        0x90 => MidiEvent::NoteOn {
            channel,
            note: at(1)?,
            velocity: at(2)?,
        },
        0x80 => MidiEvent::NoteOff {
            channel,
            note: at(1)?,
            velocity: at(2)?,
        },
        0xa0 => MidiEvent::PolyAftertouch {
            channel,
            note: at(1)?,
            pressure: at(2)?,
        },
        0xb0 => MidiEvent::ControlChange {
            channel,
            controller: at(1)?,
            value: at(2)?,
        },
        0xc0 => MidiEvent::ProgramChange {
            channel,
            program: at(1)?,
        },
        0xd0 => MidiEvent::ChannelAftertouch {
            channel,
            pressure: at(1)?,
        },
        0xe0 => MidiEvent::PitchBend {
            channel,
            value: u16::from(at(1)?) | (u16::from(at(2)?) << 7),
        },
        _ => match status {
            0xf8 => MidiEvent::Clock,
            0xfa => MidiEvent::Start,
            0xfb => MidiEvent::Continue,
            0xfc => MidiEvent::Stop,
            0xfe => MidiEvent::ActiveSensing,
            _ => return None,
        },
    })
}

/// A MIDI note number as a name, for a display a musician can read.
///
/// Octave numbering matches `note_to_midi`'s: middle C (60) is c4 there, so
/// note 0 is c-1.
pub fn note_name(note: u8) -> String {
    const NAMES: [&str; 12] = [
        "c", "c#", "d", "d#", "e", "f", "f#", "g", "g#", "a", "a#", "b",
    ];
    let octave = i32::from(note) / 12 - 1;
    format!("{}{}", NAMES[usize::from(note % 12)], octave)
}

/// An open input port.
///
/// Holding this alive is what keeps the connection open; dropping it closes the
/// port. The callback runs on the DRIVER's thread, so it must not block.
pub struct MidiListener {
    _connection: MidiInputConnection<()>,
    port_name: String,
}

impl MidiListener {
    /// List the input ports, in the order a numeric selector indexes them.
    pub fn ports() -> Result<Vec<String>, String> {
        let input = crate::open_input("rustel")?;
        Ok(input
            .ports()
            .iter()
            .map(|port| input.port_name(port).unwrap_or_else(|_| "?".into()))
            .collect())
    }

    /// Open an input by name or index, calling `on_message` for every message.
    ///
    /// Matching follows the output side: an index, or a case-insensitive
    /// SUBSTRING of the name, because real port names carry platform noise a
    /// musician should not have to type ("MIDICAKE-ARP    " has trailing
    /// spaces, and `.midi()` cannot even spell a name with a space in it).
    pub fn open<F>(selector: Option<&str>, on_message: F) -> Result<Self, String>
    where
        F: FnMut(u64, MidiEvent, &[u8]) + Send + 'static,
    {
        Self::open_filtered(selector, midir::Ignore::None, on_message)
    }

    /// Open an input, letting the caller choose what the driver filters out.
    ///
    /// `Ignore` covers only sysex, timing and active sensing - never notes or
    /// control changes - so a score-facing listener passing `Ignore::All` loses
    /// nothing it can read, and stops being woken 24 times a beat by a clock it
    /// does not use. The monitor passes `Ignore::None` because showing a
    /// musician everything their device sends is its whole job.
    pub fn open_filtered<F>(
        selector: Option<&str>,
        ignore: midir::Ignore,
        mut on_message: F,
    ) -> Result<Self, String>
    where
        F: FnMut(u64, MidiEvent, &[u8]) + Send + 'static,
    {
        let mut input = crate::open_input("rustel")?;
        input.ignore(ignore);
        let ports = input.ports();
        if ports.is_empty() {
            return Err("no MIDI input ports available".into());
        }
        let names: Vec<String> = ports
            .iter()
            .map(|port| input.port_name(port).unwrap_or_else(|_| "?".into()))
            .collect();

        // Share selector rules with output ports and device authorization.
        let index = crate::resolve_port(selector, &names, "MIDI input")?;
        let port = &ports[index];
        let port_name = names[index].clone();
        let connection = input
            .connect(
                port,
                "rustel-in",
                move |micros, bytes, _| {
                    if let Some(event) = decode(bytes) {
                        on_message(micros, event, bytes);
                    }
                },
                (),
            )
            .map_err(|error| format!("could not open MIDI input \"{port_name}\": {error}"))?;
        Ok(Self {
            _connection: connection,
            port_name,
        })
    }

    pub fn port_name(&self) -> &str {
        &self.port_name
    }
}

/// What a port has been seen to send, accumulated for the monitor's summary.
///
/// This is the mapping aid: a controller's panel legend and the CC numbers it
/// actually emits are routinely different, and the only way to learn a knob is
/// to turn it and watch.
#[derive(Default, Clone)]
pub struct InputSummary {
    pub total: u64,
    pub by_kind: Vec<(&'static str, u64)>,
    pub channels: Vec<u8>,
    /// `(channel, controller, low, high, count)` - the range each knob covered.
    pub controls: Vec<(u8, u8, u8, u8, u64)>,
    pub notes: Vec<(u8, u64)>,
}

impl InputSummary {
    pub fn observe(&mut self, event: MidiEvent) {
        self.total += 1;
        match self.by_kind.iter_mut().find(|(k, _)| *k == event.kind()) {
            Some((_, n)) => *n += 1,
            None => self.by_kind.push((event.kind(), 1)),
        }
        if let Some(channel) = event.channel()
            && !self.channels.contains(&channel)
        {
            self.channels.push(channel);
            self.channels.sort_unstable();
        }
        match event {
            MidiEvent::ControlChange {
                channel,
                controller,
                value,
            } => {
                match self
                    .controls
                    .iter_mut()
                    .find(|(c, n, _, _, _)| *c == channel && *n == controller)
                {
                    Some((_, _, low, high, count)) => {
                        *low = (*low).min(value);
                        *high = (*high).max(value);
                        *count += 1;
                    }
                    None => self.controls.push((channel, controller, value, value, 1)),
                }
                self.controls.sort_unstable();
            }
            MidiEvent::NoteOn { note, .. } => {
                match self.notes.iter_mut().find(|(n, _)| *n == note) {
                    Some((_, count)) => *count += 1,
                    None => self.notes.push((note, 1)),
                }
                self.notes.sort_unstable();
            }
            _ => {}
        }
    }
}

/// A summary shared with the driver thread.
pub type SharedSummary = Arc<Mutex<InputSummary>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_messages_decode_with_one_based_channels() {
        assert_eq!(
            decode(&[0x90, 60, 100]),
            Some(MidiEvent::NoteOn {
                channel: 1,
                note: 60,
                velocity: 100
            })
        );
        // 0x9F is channel index 15, which a score calls channel 16.
        assert_eq!(decode(&[0x9f, 60, 100]).and_then(|e| e.channel()), Some(16));
        assert_eq!(
            decode(&[0xb2, 74, 64]),
            Some(MidiEvent::ControlChange {
                channel: 3,
                controller: 74,
                value: 64
            })
        );
    }

    /// A keyboard releasing a key usually sends note-on velocity 0, not 0x80.
    /// Showing that as a note-on makes every release look like a stuck note.
    #[test]
    fn a_zero_velocity_note_on_is_a_note_off() {
        assert_eq!(
            decode(&[0x90, 60, 0]),
            Some(MidiEvent::NoteOff {
                channel: 1,
                note: 60,
                velocity: 0
            })
        );
    }

    #[test]
    fn pitch_bend_reassembles_lsb_first() {
        // Centre: LSB 0, MSB 64 -> 8192.
        assert_eq!(
            decode(&[0xe0, 0, 64]),
            Some(MidiEvent::PitchBend {
                channel: 1,
                value: 8192
            })
        );
        assert_eq!(
            decode(&[0xe0, 0x7f, 0x7f]),
            Some(MidiEvent::PitchBend {
                channel: 1,
                value: 16383
            })
        );
    }

    #[test]
    fn realtime_bytes_decode_and_unknown_status_is_refused() {
        assert_eq!(decode(&[0xf8]), Some(MidiEvent::Clock));
        assert_eq!(decode(&[0xfa]), Some(MidiEvent::Start));
        // Sysex is deliberately unmodelled.
        assert_eq!(decode(&[0xf0, 1, 2]), None);
        assert_eq!(decode(&[]), None);
    }

    #[test]
    fn truncated_channel_messages_are_refused() {
        for bytes in [
            &[0x90][..],
            &[0x90, 60],
            &[0x80, 60],
            &[0xa0, 60],
            &[0xb0, 74],
            &[0xc0],
            &[0xd0],
            &[0xe0, 0],
        ] {
            assert_eq!(
                decode(bytes),
                None,
                "accepted truncated packet {bytes:02x?}"
            );
        }
    }

    #[test]
    fn status_bytes_are_never_masked_into_channel_data() {
        for bytes in [
            &[0x90, 60, 0xf8][..],
            &[0xb0, 0xf8, 64],
            &[0xb0, 74, 0x90],
            &[0xc0, 0xfa],
            &[0xe0, 0, 0xf8],
        ] {
            assert_eq!(decode(bytes), None, "accepted status as data {bytes:02x?}");
        }
    }

    #[test]
    fn note_names_line_up_with_note_to_midi() {
        // 60 is middle C, which `note_to_midi("c4", _)` also gives.
        assert_eq!(note_name(60), "c4");
        assert_eq!(note_name(48), "c3");
        assert_eq!(note_name(69), "a4");
        assert_eq!(note_name(0), "c-1");
    }

    /// The summary is the mapping aid: turning one knob must show up as one
    /// controller with the range it covered.
    #[test]
    fn the_summary_records_each_knob_and_the_range_it_covered() {
        let mut summary = InputSummary::default();
        for value in [10u8, 80, 3, 127] {
            summary.observe(MidiEvent::ControlChange {
                channel: 1,
                controller: 74,
                value,
            });
        }
        summary.observe(MidiEvent::ControlChange {
            channel: 1,
            controller: 71,
            value: 40,
        });
        assert_eq!(summary.controls.len(), 2);
        let (_, controller, low, high, count) = summary.controls[1];
        assert_eq!((controller, low, high, count), (74, 3, 127, 4));
        assert_eq!(summary.channels, vec![1]);
        assert_eq!(summary.total, 5);
    }
}
