/*
import.rs - Read a Standard MIDI File into a Rustel score
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! Reading a Standard MIDI File into a Rustel score.
//!
//! The importer keeps the first tempo and first meter. Later changes are
//! reported and ignored. Notes pair first in, first out per channel and key,
//! so overlapping notes of the same pitch stay distinct. Channel 10 is drums.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::Path;

use midly::{Format, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};

use crate::SmfError;
use crate::meter::Meter;
use crate::score::{self, RawNote, ScoreInput, ScoreLane};

/// Most warnings kept. Further warnings become one count.
const MAX_WARNINGS: usize = 32;

/// Limits for one import.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportOptions {
    pub max_bytes: usize,
    pub max_events: usize,
    pub max_bars: u32,
    pub max_source_bytes: usize,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            max_bytes: 16 * 1024 * 1024,
            max_events: 1_000_000,
            max_bars: 4096,
            max_source_bytes: 4 * 1024 * 1024,
        }
    }
}

/// A score and what the import made of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Imported {
    pub source: String,
    pub report: ImportReport,
}

/// What the import read.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportReport {
    pub format: u8,
    pub ppq: u16,
    pub tempo_bpm: f64,
    pub meter: Meter,
    pub grid_steps_per_bar: u32,
    pub grid_exact: bool,
    pub bars: u32,
    pub lanes: Vec<LaneReport>,
    pub warnings: Vec<String>,
}

/// One imported lane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaneReport {
    pub label: String,
    pub channel: u8,
    pub program: Option<u8>,
    pub sound: String,
    pub notes: usize,
}

/// A lane while the file is read.
struct RawLane {
    name: Option<String>,
    program: Option<u8>,
    notes: Vec<RawNote>,
}

#[derive(Default)]
struct Warnings {
    items: Vec<String>,
    omitted: usize,
}

impl Warnings {
    fn push(&mut self, message: String) {
        if self.items.len() < MAX_WARNINGS {
            self.items.push(message);
        } else {
            self.omitted += 1;
        }
    }

    fn finish(mut self) -> Vec<String> {
        if self.omitted > 0 {
            self.items
                .push(format!("{} more warning(s) omitted", self.omitted));
        }
        self.items
    }
}

struct Collector<'a> {
    options: &'a ImportOptions,
    format: u8,
    ppq: u16,
    first_tempo: Option<u32>,
    first_meter: Option<Meter>,
    later_tempo: bool,
    later_meter: bool,
    warnings: Warnings,
    lanes: BTreeMap<(usize, u8), RawLane>,
    events: usize,
    unmatched_offs: usize,
    unclosed: usize,
    sysex: usize,
    controllers: usize,
    bends: usize,
    aftertouch: usize,
    ports: usize,
    later_programs: usize,
    drum_programs: usize,
}

impl<'a> Collector<'a> {
    fn new(options: &'a ImportOptions, format: u8, ppq: u16) -> Self {
        Self {
            options,
            format,
            ppq,
            first_tempo: None,
            first_meter: None,
            later_tempo: false,
            later_meter: false,
            warnings: Warnings::default(),
            lanes: BTreeMap::new(),
            events: 0,
            unmatched_offs: 0,
            unclosed: 0,
            sysex: 0,
            controllers: 0,
            bends: 0,
            aftertouch: 0,
            ports: 0,
            later_programs: 0,
            drum_programs: 0,
        }
    }

    fn track(&mut self, track_index: usize, track: &[TrackEvent<'_>]) -> Result<(), SmfError> {
        let mut tick = 0u64;
        let mut name: Option<String> = None;
        let mut active: HashMap<(u8, u8), VecDeque<u64>> = HashMap::new();
        let mut notes: BTreeMap<u8, Vec<RawNote>> = BTreeMap::new();
        let mut programs: BTreeMap<u8, u8> = BTreeMap::new();
        for event in track {
            self.events += 1;
            if self.events > self.options.max_events {
                return Err(SmfError::TooManyEvents {
                    limit: self.options.max_events,
                });
            }
            tick = tick
                .checked_add(u64::from(event.delta.as_int()))
                .ok_or_else(|| SmfError::Parse("tick position overflows 64 bits".to_owned()))?;
            match event.kind {
                TrackEventKind::Meta(meta) => {
                    self.meta(meta, &mut name)?;
                }
                TrackEventKind::Midi { channel, message } => {
                    let channel = channel.as_int() + 1;
                    match message {
                        MidiMessage::NoteOn { key, vel } => {
                            if vel.as_int() == 0 {
                                self.note_off(channel, key.as_int(), tick, &mut active, &mut notes);
                            } else {
                                active
                                    .entry((channel, key.as_int()))
                                    .or_default()
                                    .push_back(tick);
                            }
                        }
                        MidiMessage::NoteOff { key, .. } => {
                            self.note_off(channel, key.as_int(), tick, &mut active, &mut notes);
                        }
                        MidiMessage::ProgramChange { program } => {
                            if channel == 10 {
                                self.drum_programs += 1;
                            } else {
                                let program = program.as_int();
                                match programs.get(&channel) {
                                    None => {
                                        programs.insert(channel, program);
                                    }
                                    Some(first) if *first != program => self.later_programs += 1,
                                    Some(_) => {}
                                }
                            }
                        }
                        MidiMessage::Controller { .. } => self.controllers += 1,
                        MidiMessage::Aftertouch { .. } | MidiMessage::ChannelAftertouch { .. } => {
                            self.aftertouch += 1;
                        }
                        MidiMessage::PitchBend { .. } => self.bends += 1,
                    }
                }
                TrackEventKind::SysEx(_) | TrackEventKind::Escape(_) => self.sysex += 1,
            }
        }
        for ((channel, note), starts) in active.drain() {
            for start in starts {
                notes.entry(channel).or_default().push(RawNote {
                    note,
                    start,
                    end: tick,
                });
                self.unclosed += 1;
            }
        }
        for (channel, lane_notes) in notes {
            let program = programs.get(&channel).copied();
            self.lanes
                .entry((track_index, channel))
                .or_insert_with(|| RawLane {
                    name: name.clone(),
                    program,
                    notes: Vec::new(),
                })
                .notes
                .extend(lane_notes);
        }
        Ok(())
    }

    fn meta(&mut self, meta: MetaMessage<'_>, name: &mut Option<String>) -> Result<(), SmfError> {
        match meta {
            MetaMessage::TrackName(bytes) => {
                if name.is_none() {
                    *name = Some(String::from_utf8_lossy(bytes).into_owned());
                }
            }
            MetaMessage::Tempo(micros) => {
                let micros = micros.as_int();
                if micros == 0 {
                    return Err(SmfError::Parse("the tempo is zero".to_owned()));
                }
                match self.first_tempo {
                    None => self.first_tempo = Some(micros),
                    Some(first) if first != micros => self.later_tempo = true,
                    Some(_) => {}
                }
            }
            MetaMessage::TimeSignature(numerator, power, _, _) => {
                if power > 6 {
                    return Err(SmfError::Parse(format!(
                        "time signature denominator 2^{power} is above 64"
                    )));
                }
                let denominator = 1u8 << power;
                let meter = Meter::new(numerator, denominator)
                    .map_err(|error| SmfError::Parse(error.to_string()))?;
                match self.first_meter {
                    None => self.first_meter = Some(meter),
                    Some(first) if first != meter => self.later_meter = true,
                    Some(_) => {}
                }
            }
            MetaMessage::MidiPort(_) => self.ports += 1,
            _ => {}
        }
        Ok(())
    }

    fn note_off(
        &mut self,
        channel: u8,
        note: u8,
        tick: u64,
        active: &mut HashMap<(u8, u8), VecDeque<u64>>,
        notes: &mut BTreeMap<u8, Vec<RawNote>>,
    ) {
        let Some(queue) = active.get_mut(&(channel, note)) else {
            self.unmatched_offs += 1;
            return;
        };
        let Some(start) = queue.pop_front() else {
            self.unmatched_offs += 1;
            return;
        };
        if queue.is_empty() {
            active.remove(&(channel, note));
        }
        notes.entry(channel).or_default().push(RawNote {
            note,
            start,
            end: tick,
        });
    }

    fn finish(self) -> Result<Imported, SmfError> {
        let tempo_micros = self.first_tempo.unwrap_or(500_000);
        let tempo_bpm = 60_000_000.0 / f64::from(tempo_micros);
        let meter = self.first_meter.unwrap_or(Meter::FOUR_FOUR);
        let mut warnings = self.warnings;
        if self.later_tempo {
            warnings.push("later tempo changes are not imported".to_owned());
        }
        if self.later_meter {
            warnings.push("later meter changes are not imported".to_owned());
        }
        if self.ports > 0 {
            warnings.push("MIDI port metadata is not imported".to_owned());
        }
        if self.controllers > 0 {
            warnings.push(format!(
                "{} controller event(s) are not imported",
                self.controllers
            ));
        }
        if self.bends > 0 {
            warnings.push(format!(
                "{} pitch bend event(s) are not imported",
                self.bends
            ));
        }
        if self.aftertouch > 0 {
            warnings.push(format!(
                "{} aftertouch event(s) are not imported",
                self.aftertouch
            ));
        }
        if self.sysex > 0 {
            warnings.push(format!("{} sysex event(s) are not imported", self.sysex));
        }
        if self.unmatched_offs > 0 {
            warnings.push(format!(
                "{} note-off event(s) without a note-on were ignored",
                self.unmatched_offs
            ));
        }
        if self.unclosed > 0 {
            warnings.push(format!(
                "{} unclosed note(s) end at the track end",
                self.unclosed
            ));
        }
        if self.later_programs > 0 {
            warnings.push("program changes after the first in a lane are not imported".to_owned());
        }
        if self.drum_programs > 0 {
            warnings.push("program changes on channel 10 are not imported".to_owned());
        }
        if self.lanes.values().any(|lane| !lane.notes.is_empty()) {
            warnings.push("note velocity is not imported; notes use the score default".to_owned());
        }
        let labels = lane_labels(&self.lanes);
        let score_lanes: Vec<ScoreLane<'_>> = self
            .lanes
            .iter()
            .filter(|(_, lane)| !lane.notes.is_empty())
            .map(|(key, lane)| ScoreLane {
                label: labels
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| format!("ch{}", key.1)),
                channel: key.1,
                program: lane.program,
                notes: &lane.notes,
            })
            .collect();
        let output = score::emit(&ScoreInput {
            format: self.format,
            ppq: self.ppq,
            tempo_bpm,
            meter,
            lanes: &score_lanes,
            max_bars: self.options.max_bars,
            max_source_bytes: self.options.max_source_bytes,
        })?;
        for warning in output.warnings {
            warnings.push(warning);
        }
        Ok(Imported {
            source: output.source,
            report: ImportReport {
                format: self.format,
                ppq: self.ppq,
                tempo_bpm,
                meter,
                grid_steps_per_bar: output.grid_steps_per_bar,
                grid_exact: output.grid_exact,
                bars: output.bars,
                lanes: output.lanes,
                warnings: warnings.finish(),
            },
        })
    }
}

fn lane_labels(lanes: &BTreeMap<(usize, u8), RawLane>) -> BTreeMap<(usize, u8), String> {
    let mut labels = BTreeMap::new();
    let mut used = BTreeSet::new();
    let mut tracks: BTreeMap<usize, Vec<(usize, u8)>> = BTreeMap::new();
    for key in lanes.keys() {
        tracks.entry(key.0).or_default().push(*key);
    }
    for keys in tracks.values() {
        let multiple = keys.len() > 1;
        for key in keys {
            let channel = key.1;
            let name = lanes[key].name.as_deref();
            let base = match (name, multiple) {
                (Some(name), false) => sanitize_label(name),
                (Some(name), true) => format!("{}_ch{channel}", sanitize_label(name)),
                (None, _) => format!("ch{channel}"),
            };
            let base = if base.is_empty() || base == "$" {
                format!("ch{channel}")
            } else {
                base
            };
            let base = if base.as_bytes()[0].is_ascii_digit() {
                format!("midi_{base}")
            } else {
                base
            };
            let mut label = base.clone();
            let mut suffix = 2;
            while used.contains(&label) {
                label = format!("{base}_{suffix}");
                suffix += 1;
            }
            used.insert(label.clone());
            labels.insert(*key, label);
        }
    }
    labels
}

fn sanitize_label(name: &str) -> String {
    let mut label = String::new();
    for character in name.chars() {
        if character.is_ascii_alphanumeric() || character == '_' || character == '$' {
            label.push(character);
        } else {
            label.push('_');
        }
    }
    label.trim_matches('_').to_owned()
}

/// Read a Standard MIDI File from bytes.
pub fn import_smf(bytes: &[u8], options: &ImportOptions) -> Result<Imported, SmfError> {
    if bytes.len() > options.max_bytes {
        return Err(SmfError::TooLarge {
            limit: options.max_bytes,
            size: bytes.len(),
        });
    }
    if bytes.is_empty() {
        return Err(SmfError::Parse("the file is empty".to_owned()));
    }
    let smf = Smf::parse(bytes).map_err(|error| SmfError::Parse(error.to_string()))?;
    let format = match smf.header.format {
        Format::SingleTrack => 0,
        Format::Parallel => 1,
        Format::Sequential => return Err(SmfError::UnsupportedFormat(2)),
    };
    let ppq = match smf.header.timing {
        Timing::Metrical(ppq) => ppq.as_int(),
        Timing::Timecode(..) => return Err(SmfError::TimecodeTiming),
    };
    if ppq == 0 {
        return Err(SmfError::Parse(
            "the file has zero ticks per quarter".to_owned(),
        ));
    }
    let mut collector = Collector::new(options, format, ppq);
    for (track_index, track) in smf.tracks.iter().enumerate() {
        collector.track(track_index, track)?;
    }
    collector.finish()
}

/// Read a Standard MIDI File from disk.
pub fn import_smf_file(path: &Path, options: &ImportOptions) -> Result<Imported, SmfError> {
    let size = std::fs::metadata(path)?.len();
    let size = usize::try_from(size).unwrap_or(usize::MAX);
    if size > options.max_bytes {
        return Err(SmfError::TooLarge {
            limit: options.max_bytes,
            size,
        });
    }
    let bytes = std::fs::read(path)?;
    import_smf(&bytes, options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::{Message, MessageKind, Timeline, Track, tempo_micros_per_quarter};

    fn file(track: Track) -> Vec<u8> {
        Timeline {
            ppq: 480,
            tempo_micros_per_quarter: tempo_micros_per_quarter(120.0).unwrap(),
            meter: Meter::FOUR_FOUR,
            tracks: vec![track],
        }
        .to_bytes()
        .unwrap()
    }

    fn message(tick: u64, kind: MessageKind) -> Message {
        Message { tick, kind }
    }

    fn later_changes_file() -> Vec<u8> {
        use midly::num::{u4, u7, u15, u24, u28};
        let mut smf = Smf::new(midly::Header::new(
            Format::Parallel,
            Timing::Metrical(u15::from(480)),
        ));
        smf.tracks.push(vec![
            TrackEvent {
                delta: u28::from(0),
                kind: TrackEventKind::Meta(MetaMessage::Tempo(u24::from(500_000))),
            },
            TrackEvent {
                delta: u28::from(0),
                kind: TrackEventKind::Meta(MetaMessage::TimeSignature(4, 2, 24, 8)),
            },
            TrackEvent {
                delta: u28::from(480),
                kind: TrackEventKind::Meta(MetaMessage::Tempo(u24::from(600_000))),
            },
            TrackEvent {
                delta: u28::from(0),
                kind: TrackEventKind::Meta(MetaMessage::TimeSignature(3, 2, 24, 8)),
            },
            TrackEvent {
                delta: u28::from(0),
                kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
            },
        ]);
        smf.tracks.push(vec![
            TrackEvent {
                delta: u28::from(0),
                kind: TrackEventKind::Midi {
                    channel: u4::from(0),
                    message: MidiMessage::NoteOn {
                        key: u7::from(60),
                        vel: u7::from(100),
                    },
                },
            },
            TrackEvent {
                delta: u28::from(480),
                kind: TrackEventKind::Midi {
                    channel: u4::from(0),
                    message: MidiMessage::NoteOff {
                        key: u7::from(60),
                        vel: u7::from(0),
                    },
                },
            },
            TrackEvent {
                delta: u28::from(0),
                kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
            },
        ]);
        let mut bytes = Vec::new();
        smf.write_std(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn an_oversized_file_is_refused() {
        let bytes = file(Track {
            name: "lead".to_owned(),
            channel: 1,
            messages: vec![message(0, MessageKind::NoteOff { note: 0 })],
        });
        let options = ImportOptions {
            max_bytes: 1,
            ..ImportOptions::default()
        };
        assert!(matches!(
            import_smf(&bytes, &options),
            Err(SmfError::TooLarge { limit: 1, .. })
        ));
    }

    #[test]
    fn notes_pair_first_in_first_out_and_zero_velocity_ends_a_note() {
        let bytes = file(Track {
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
                    240,
                    MessageKind::NoteOn {
                        note: 60,
                        velocity: 100,
                    },
                ),
                message(480, MessageKind::NoteOff { note: 60 }),
                message(720, MessageKind::NoteOff { note: 60 }),
            ],
        });
        let imported = import_smf(&bytes, &ImportOptions::default()).unwrap();
        assert_eq!(imported.report.lanes.len(), 1);
        assert_eq!(imported.report.lanes[0].notes, 2);
        assert!(imported.source.contains("lead:"));
    }

    #[test]
    fn the_first_tempo_and_meter_win_and_later_changes_warn() {
        let bytes = later_changes_file();
        let imported = import_smf(&bytes, &ImportOptions::default()).unwrap();
        assert_eq!(imported.report.tempo_bpm, 120.0);
        assert_eq!(imported.report.meter, Meter::FOUR_FOUR);
        assert!(
            imported
                .report
                .warnings
                .iter()
                .any(|warning| warning.contains("later tempo"))
        );
        assert!(
            imported
                .report
                .warnings
                .iter()
                .any(|warning| warning.contains("later meter"))
        );
    }

    #[test]
    fn channel_ten_becomes_drums_and_unknown_drum_notes_drop() {
        let bytes = file(Track {
            name: "drums".to_owned(),
            channel: 10,
            messages: vec![
                message(
                    0,
                    MessageKind::NoteOn {
                        note: 35,
                        velocity: 100,
                    },
                ),
                message(240, MessageKind::NoteOff { note: 35 }),
                message(
                    480,
                    MessageKind::NoteOn {
                        note: 52,
                        velocity: 100,
                    },
                ),
                message(720, MessageKind::NoteOff { note: 52 }),
            ],
        });
        let imported = import_smf(&bytes, &ImportOptions::default()).unwrap();
        assert_eq!(imported.report.lanes[0].channel, 10);
        assert_eq!(imported.report.lanes[0].sound, "drums");
        assert_eq!(imported.report.lanes[0].program, None);
        assert!(imported.source.contains("s(\"bd"));
        assert!(
            imported
                .report
                .warnings
                .iter()
                .any(|warning| warning.contains("dropped"))
        );
    }

    #[test]
    fn labels_are_sanitized_and_unique() {
        let lanes = BTreeMap::from([
            (
                (0, 1),
                RawLane {
                    name: Some("Bass Line!".to_owned()),
                    program: None,
                    notes: Vec::new(),
                },
            ),
            (
                (1, 1),
                RawLane {
                    name: Some("Bass Line!".to_owned()),
                    program: None,
                    notes: Vec::new(),
                },
            ),
            (
                (2, 1),
                RawLane {
                    name: Some("2 fast".to_owned()),
                    program: None,
                    notes: Vec::new(),
                },
            ),
        ]);
        let labels = lane_labels(&lanes);
        assert_eq!(labels[&(0, 1)], "Bass_Line");
        assert_eq!(labels[&(1, 1)], "Bass_Line_2");
        assert_eq!(labels[&(2, 1)], "midi_2_fast");
    }

    #[test]
    fn a_format_two_file_is_refused() {
        let mut bytes = file(Track {
            name: "lead".to_owned(),
            channel: 1,
            messages: vec![message(0, MessageKind::NoteOff { note: 0 })],
        });
        bytes[9] = 2;
        assert!(matches!(
            import_smf(&bytes, &ImportOptions::default()),
            Err(SmfError::UnsupportedFormat(2))
        ));
    }

    #[test]
    fn an_empty_file_and_an_event_overflow_are_refused() {
        assert!(matches!(
            import_smf(&[], &ImportOptions::default()),
            Err(SmfError::Parse(_))
        ));
        let bytes = file(Track {
            name: "lead".to_owned(),
            channel: 1,
            messages: vec![message(0, MessageKind::NoteOff { note: 0 })],
        });
        let options = ImportOptions {
            max_events: 1,
            ..ImportOptions::default()
        };
        assert!(matches!(
            import_smf(&bytes, &options),
            Err(SmfError::TooManyEvents { .. })
        ));
    }
}
