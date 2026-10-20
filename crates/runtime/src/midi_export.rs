/*
midi_export.rs - Turn scheduled onsets into a Standard MIDI File
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! Building a MIDI file from the onsets a score schedules.
//!
//! The wire planner in `rustel_midi` decides the messages for one hap. This
//! module places them on a tick grid and groups them by port and channel.
//! Note-offs are exact: the live note-off margin exists for device clocks, not
//! for a file.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use rustel_midi::{MidiControls, MidiMessage, note_byte};
use rustel_smf::{
    Message, MessageKind, SmfOptions, Timeline, Track, base_sound_name, drum_note_for_name,
    program_for_sound, tempo_micros_per_quarter,
};

use crate::hap_json::{MidiExportReport, OnsetEventJson, ValueJson};
use crate::midi_onset::{midi_onset_with_port, midi_route, pitch_value};

/// Most skipped names kept in the report.
const MAX_SKIPPED_NAMES: usize = 8;

/// The note a GM sound plays when the score gives none.
const DEFAULT_GM_NOTE: f64 = 48.0;

/// Most bytes in one track name.
const MAX_TRACK_NAME_BYTES: usize = 128;

struct Intent {
    port: Option<String>,
    controls: MidiControls,
    notes: Vec<u8>,
    drum_mapped: bool,
    sound: Option<String>,
    skipped_label: Option<String>,
}

struct TrackState {
    messages: Vec<Message>,
    program: Option<u8>,
}

/// Build the file and its report.
pub(crate) fn timeline(
    onsets: &[OnsetEventJson],
    end_secs: f64,
    cps: f64,
    options: SmfOptions,
) -> Result<(Timeline, MidiExportReport), String> {
    options.validate().map_err(|error| error.to_string())?;
    if !cps.is_finite() || cps <= 0.0 {
        return Err(format!("cps must be a finite positive number, got {cps}"));
    }
    let bpm =
        cps * 240.0 * f64::from(options.meter.numerator) / f64::from(options.meter.denominator);
    let tempo_micros = tempo_micros_per_quarter(bpm).map_err(|error| error.to_string())?;
    let ticks_per_second = f64::from(options.ppq) * 4.0 * cps * f64::from(options.meter.numerator)
        / f64::from(options.meter.denominator);
    if !ticks_per_second.is_finite() || ticks_per_second <= 0.0 {
        return Err("the tempo gives no usable tick rate".to_owned());
    }

    let mut sorted: Vec<&OnsetEventJson> = onsets
        .iter()
        .filter(|onset| onset.target_time < end_secs)
        .collect();
    sorted.sort_by(|left, right| {
        left.target_time
            .partial_cmp(&right.target_time)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.onset_id.cmp(&right.onset_id))
    });

    let mut tracks: BTreeMap<(Option<String>, u8), TrackState> = BTreeMap::new();
    let mut notes = 0usize;
    let mut skipped_unsupported = 0usize;
    let mut skipped_names: Vec<String> = Vec::new();

    for onset in sorted {
        let Some(intent) = intent(onset) else {
            continue;
        };
        let channel = channel_of(&intent);
        let onset_tick = tick_for(onset, 0.0, ticks_per_second);
        let state = tracks
            .entry((intent.port.clone(), channel))
            .or_insert_with(|| TrackState {
                messages: Vec::new(),
                program: None,
            });
        if let Some(program) = desired_program(&intent.controls, intent.sound.as_deref())
            && state.program != Some(program)
        {
            state.program = Some(program);
            state.messages.push(Message {
                tick: onset_tick,
                kind: MessageKind::ProgramChange { program },
            });
        }

        // The non-note controls go out once, so a chord does not repeat them.
        let mut common = intent.controls.clone();
        common.note = None;
        common.prog_num = None;
        common.note_offset_ms = Some(0.0);
        for (offset, message) in rustel_midi::plan(&common, onset.duration_secs) {
            if let Some(kind) = file_message(&message) {
                state.messages.push(Message {
                    tick: tick_for(onset, offset, ticks_per_second),
                    kind,
                });
            }
        }

        if !intent.controls.is_controller {
            for note in &intent.notes {
                let one = MidiControls {
                    note: Some(f64::from(*note)),
                    gain: intent.controls.gain,
                    velocity: intent.controls.velocity,
                    midichan: Some(f64::from(channel)),
                    note_offset_ms: Some(0.0),
                    ..MidiControls::default()
                };
                for (offset, message) in rustel_midi::plan(&one, onset.duration_secs) {
                    if let Some(kind) = file_message(&message) {
                        let mut tick = tick_for(onset, offset, ticks_per_second);
                        if matches!(kind, MessageKind::NoteOff { .. }) && tick <= onset_tick {
                            tick = onset_tick + 1;
                        }
                        state.messages.push(Message { tick, kind });
                    }
                }
                notes += 1;
            }
        }

        if let Some(label) = intent.skipped_label {
            skipped_unsupported += 1;
            if skipped_names.len() < MAX_SKIPPED_NAMES
                && !skipped_names.iter().any(|known| known == &label)
            {
                skipped_names.push(label);
            }
        }
    }

    let tracks: Vec<Track> = tracks
        .into_iter()
        .filter(|(_, state)| !state.messages.is_empty())
        .map(|((port, channel), state)| Track {
            name: track_name(port.as_deref(), channel),
            channel,
            messages: state.messages,
        })
        .collect();
    let message_count: usize = tracks.iter().map(|track| track.messages.len()).sum();
    if message_count == 0 {
        let detail = if skipped_names.is_empty() {
            String::new()
        } else {
            format!(" (no MIDI note for {})", skipped_names.join(", "))
        };
        return Err(format!("the score has no MIDI events{detail}"));
    }
    let report = MidiExportReport {
        ppq: options.ppq,
        tempo_bpm: bpm,
        meter: format!("{}/{}", options.meter.numerator, options.meter.denominator),
        tracks: tracks.len(),
        notes,
        messages: message_count,
        skipped_unsupported,
        skipped_names,
    };
    Ok((
        Timeline {
            ppq: options.ppq,
            tempo_micros_per_quarter: tempo_micros,
            meter: options.meter,
            tracks,
        },
        report,
    ))
}

fn intent(onset: &OnsetEventJson) -> Option<Intent> {
    match &onset.value {
        ValueJson::Number(note) => {
            let notes: Vec<u8> = note_byte(*note).into_iter().collect();
            let skipped_label = notes.is_empty().then(|| note.to_string());
            Some(Intent {
                port: None,
                controls: MidiControls {
                    note: Some(*note),
                    ..MidiControls::default()
                },
                notes,
                drum_mapped: false,
                sound: None,
                skipped_label,
            })
        }
        ValueJson::String(text) => {
            let note = pitch_value(&serde_json::Value::String(text.clone()));
            let notes: Vec<u8> = note.and_then(note_byte).into_iter().collect();
            let skipped_label = notes.is_empty().then(|| truncate(text, 48));
            Some(Intent {
                port: None,
                controls: MidiControls {
                    note,
                    ..MidiControls::default()
                },
                notes,
                drum_mapped: false,
                sound: None,
                skipped_label,
            })
        }
        ValueJson::Raw(serde_json::Value::Object(map)) => {
            let controls = midi_onset_with_port(onset, String::new())?.controls;
            let sound = map
                .get("s")
                .and_then(serde_json::Value::as_str)
                .map(base_sound_name)
                .map(str::to_owned);
            let has_note_key = map.contains_key("note");
            let mut notes = Vec::new();
            let mut drum_mapped = false;
            if let Some(note) = controls.note {
                if let Some(note) = note_byte(note) {
                    notes.push(note);
                }
            } else if let Some(items) = map.get("note").and_then(serde_json::Value::as_array) {
                for item in items {
                    if let Some(note) = pitch_value(item).and_then(note_byte) {
                        notes.push(note);
                    }
                }
            } else if !has_note_key && let Some(sound) = sound.as_deref() {
                if let Some(note) = drum_note_for_name(sound) {
                    notes.push(note);
                    drum_mapped = true;
                } else if program_for_sound(sound).is_some()
                    && let Some(note) = note_byte(DEFAULT_GM_NOTE)
                {
                    notes.push(note);
                }
            }
            notes.sort_unstable();
            notes.dedup();
            let skipped_label =
                if notes.is_empty() && !controls.is_controller && !has_output(&controls) {
                    sound.clone().or_else(|| {
                        let show = onset.value_show.trim();
                        (!show.is_empty()).then(|| truncate(show, 48))
                    })
                } else {
                    None
                };
            Some(Intent {
                port: midi_route(onset).map(|port| port.into_owned()),
                controls,
                notes,
                drum_mapped,
                sound,
                skipped_label,
            })
        }
        ValueJson::Raw(serde_json::Value::Array(items)) => {
            let mut notes: Vec<u8> = items
                .iter()
                .filter_map(|item| pitch_value(item).and_then(note_byte))
                .collect();
            notes.sort_unstable();
            notes.dedup();
            if notes.is_empty() {
                return None;
            }
            Some(Intent {
                port: None,
                controls: MidiControls::default(),
                notes,
                drum_mapped: false,
                sound: None,
                skipped_label: None,
            })
        }
        _ => None,
    }
}

fn channel_of(intent: &Intent) -> u8 {
    intent
        .controls
        .midichan
        .filter(|value| value.is_finite())
        .map(|value| value.round().clamp(1.0, 16.0) as u8)
        .unwrap_or(if intent.drum_mapped { 10 } else { 1 })
}

fn desired_program(controls: &MidiControls, sound: Option<&str>) -> Option<u8> {
    if let Some(program) = controls
        .prog_num
        .filter(|value| value.is_finite() && (0.0..=127.0).contains(value))
    {
        return Some(program.round() as u8);
    }
    sound.and_then(program_for_sound)
}

fn has_output(controls: &MidiControls) -> bool {
    !controls.mapped_ccs.is_empty()
        || (controls.ccn.is_some() && controls.ccv.is_some())
        || controls.midibend.is_some()
        || controls.miditouch.is_some()
        || controls.prog_num.is_some()
}

fn file_message(message: &MidiMessage) -> Option<MessageKind> {
    let bytes = message.as_slice();
    match bytes.first()? & 0xf0 {
        0x80 if bytes.len() >= 3 => Some(MessageKind::NoteOff {
            note: bytes[1] & 0x7f,
        }),
        0x90 if bytes.len() >= 3 => {
            if bytes[2] == 0 {
                Some(MessageKind::NoteOff {
                    note: bytes[1] & 0x7f,
                })
            } else {
                Some(MessageKind::NoteOn {
                    note: bytes[1] & 0x7f,
                    velocity: bytes[2] & 0x7f,
                })
            }
        }
        0xb0 if bytes.len() >= 3 => Some(MessageKind::ControlChange {
            controller: bytes[1] & 0x7f,
            value: bytes[2] & 0x7f,
        }),
        0xc0 if bytes.len() >= 2 => Some(MessageKind::ProgramChange {
            program: bytes[1] & 0x7f,
        }),
        0xd0 if bytes.len() >= 2 => Some(MessageKind::ChannelAftertouch {
            pressure: bytes[1] & 0x7f,
        }),
        0xe0 if bytes.len() >= 3 => Some(MessageKind::PitchBend {
            value: u16::from(bytes[1] & 0x7f) | (u16::from(bytes[2] & 0x7f) << 7),
        }),
        _ => None,
    }
}

fn tick_for(onset: &OnsetEventJson, offset_secs: f64, ticks_per_second: f64) -> u64 {
    let time = onset.target_time + offset_secs;
    if !time.is_finite() || time <= 0.0 {
        0
    } else {
        (time * ticks_per_second).round() as u64
    }
}

fn track_name(port: Option<&str>, channel: u8) -> String {
    let mut name = match port {
        Some(port) => format!("{port} ch {channel}"),
        None => format!("channel {channel}"),
    };
    if name.len() > MAX_TRACK_NAME_BYTES {
        let mut end = MAX_TRACK_NAME_BYTES;
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        name.truncate(end);
    }
    name
}

fn truncate(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustel_smf::Meter;

    fn onset(
        id: u64,
        target_time: f64,
        duration_secs: f64,
        value: serde_json::Value,
    ) -> OnsetEventJson {
        OnsetEventJson {
            live_controls: [0; 3],
            onset_id: id,
            generation: 0,
            whole_begin: "0/1".into(),
            duration_secs,
            target_time,
            value: ValueJson::Raw(value),
            value_show: String::new(),
            ui_visuals: 0,
            log_line: None,
        }
    }

    fn options() -> SmfOptions {
        SmfOptions::default()
    }

    fn exported(onsets: &[OnsetEventJson]) -> (Timeline, MidiExportReport) {
        timeline(onsets, f64::INFINITY, 0.5, options()).unwrap()
    }

    #[test]
    fn a_note_becomes_a_note_on_and_an_exact_note_off() {
        let (timeline, report) = exported(&[onset(
            1,
            0.0,
            0.5,
            serde_json::json!({ "note": 60, "s": "gm_piano" }),
        )]);
        assert_eq!(report.notes, 1);
        assert_eq!(report.tracks, 1);
        assert_eq!(timeline.tracks[0].channel, 1);
        let messages = &timeline.tracks[0].messages;
        assert!(matches!(
            messages[0].kind,
            MessageKind::ProgramChange { program: 0 }
        ));
        assert!(matches!(
            messages[1].kind,
            MessageKind::NoteOn { note: 60, .. }
        ));
        assert!(matches!(
            messages[2].kind,
            MessageKind::NoteOff { note: 60 }
        ));
        assert_eq!(messages[1].tick, 0);
        assert_eq!(messages[2].tick, 480);
    }

    #[test]
    fn bend_and_aftertouch_are_exported() {
        let (timeline, _) = exported(&[onset(
            1,
            0.0,
            0.5,
            serde_json::json!({
                "note": 60,
                "midibend": 0.5,
                "miditouch": 0.5,
            }),
        )]);
        let messages = &timeline.tracks[0].messages;
        assert!(
            messages
                .iter()
                .any(|message| matches!(message.kind, MessageKind::PitchBend { value: 12_287 })),
            "{messages:?}"
        );
        assert!(messages.iter().any(|message| matches!(
            message.kind,
            MessageKind::ChannelAftertouch { pressure: 64 }
        )));
    }

    #[test]
    fn an_explicit_program_outranks_the_sound_name() {
        let (timeline, _) = exported(&[onset(
            1,
            0.0,
            0.5,
            serde_json::json!({
                "note": 60,
                "s": "gm_piano",
                "progNum": 40,
            }),
        )]);
        let programs: Vec<u8> = timeline.tracks[0]
            .messages
            .iter()
            .filter_map(|message| match message.kind {
                MessageKind::ProgramChange { program } => Some(program),
                _ => None,
            })
            .collect();
        assert_eq!(programs, vec![40]);
    }

    #[test]
    fn a_chord_emits_one_common_set_and_one_note_each() {
        let (timeline, report) = exported(&[onset(
            1,
            0.0,
            0.5,
            serde_json::json!({
                "note": [60, 64],
                "s": "gm_piano",
                "ccn": 74,
                "ccv": 0.5,
            }),
        )]);
        assert_eq!(report.notes, 2);
        let messages = &timeline.tracks[0].messages;
        let controls = messages
            .iter()
            .filter(|message| matches!(message.kind, MessageKind::ControlChange { .. }))
            .count();
        assert_eq!(controls, 1);
        let ons = messages
            .iter()
            .filter(|message| matches!(message.kind, MessageKind::NoteOn { .. }))
            .count();
        assert_eq!(ons, 2);
    }

    #[test]
    fn drums_use_channel_ten_and_the_mapped_note() {
        let (timeline, report) = exported(&[onset(1, 0.0, 0.25, serde_json::json!({ "s": "bd" }))]);
        assert_eq!(report.notes, 1);
        assert_eq!(timeline.tracks[0].channel, 10);
        assert!(
            timeline.tracks[0]
                .messages
                .iter()
                .any(|message| { matches!(message.kind, MessageKind::NoteOn { note: 35, .. }) })
        );
    }

    #[test]
    fn a_gm_sound_without_a_note_uses_midi_48() {
        let (timeline, _) = exported(&[onset(1, 0.0, 0.5, serde_json::json!({ "s": "gm_piano" }))]);
        assert!(
            timeline.tracks[0]
                .messages
                .iter()
                .any(|message| { matches!(message.kind, MessageKind::NoteOn { note: 48, .. }) })
        );
    }

    #[test]
    fn a_port_groups_its_own_track() {
        let (timeline, report) = exported(&[
            onset(
                1,
                0.0,
                0.5,
                serde_json::json!({ "note": 60, "midiport": "IAC" }),
            ),
            onset(2, 0.0, 0.5, serde_json::json!({ "note": 64 })),
        ]);
        assert_eq!(report.tracks, 2);
        assert!(timeline.tracks.iter().any(|track| track.name == "IAC ch 1"));
        assert!(
            timeline
                .tracks
                .iter()
                .any(|track| track.name == "channel 1")
        );
    }

    #[test]
    fn transport_messages_are_dropped() {
        let (timeline, report) = exported(&[onset(
            1,
            0.0,
            0.5,
            serde_json::json!({ "note": 60, "midicmd": "start" }),
        )]);
        assert_eq!(report.messages, 2);
        assert!(
            timeline.tracks[0]
                .messages
                .iter()
                .all(|message| !matches!(message.kind, MessageKind::ControlChange { .. }))
        );
    }

    #[test]
    fn an_unmapped_sound_is_named_instead_of_guessed() {
        let error = timeline(
            &[onset(1, 0.0, 0.5, serde_json::json!({ "s": "my_sample" }))],
            f64::INFINITY,
            0.5,
            options(),
        )
        .unwrap_err();
        assert!(error.contains("my_sample"), "{error}");
    }

    #[test]
    fn a_short_note_keeps_one_tick() {
        let (timeline, _) = exported(&[onset(1, 0.0, 0.0, serde_json::json!({ "note": 60 }))]);
        let messages = &timeline.tracks[0].messages;
        assert!(matches!(messages[0].kind, MessageKind::NoteOn { .. }));
        assert!(matches!(messages[1].kind, MessageKind::NoteOff { .. }));
        assert!(messages[1].tick > messages[0].tick);
    }

    #[test]
    fn a_controller_only_hap_has_no_note_and_no_skip() {
        let (timeline, report) = exported(&[onset(
            1,
            0.0,
            0.5,
            serde_json::json!({ "ccn": 7, "ccv": 0.5, "midichan": 2 }),
        )]);
        assert_eq!(report.notes, 0);
        assert_eq!(report.skipped_unsupported, 0);
        assert_eq!(timeline.tracks[0].channel, 2);
        assert!(timeline.tracks[0].messages.iter().any(|message| matches!(
            message.kind,
            MessageKind::ControlChange { controller: 7, .. }
        )));
    }

    #[test]
    fn an_onset_at_the_end_is_left_out() {
        let onsets = [
            onset(1, 0.0, 0.5, serde_json::json!({ "note": 60 })),
            onset(2, 1.0, 0.5, serde_json::json!({ "note": 64 })),
        ];
        let (_, report) = timeline(&onsets, 1.0, 0.5, options()).unwrap();
        assert_eq!(report.notes, 1);
    }

    #[test]
    fn an_empty_score_is_refused() {
        assert!(timeline(&[], f64::INFINITY, 0.5, options()).is_err());
    }

    #[test]
    fn a_render_writes_a_parseable_file_and_fills_the_report() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("take.mid");
        let mut session = crate::Session::new().unwrap();
        session.evaluate("$: note(\"c4\")").unwrap();
        let report = session
            .render(1.0, &path, crate::RenderFormat::Midi)
            .unwrap();
        assert_eq!(report.format, "midi");
        assert_eq!(report.sample_rate, 0);
        assert_eq!(report.channels, 0);
        let midi = report.midi.expect("MIDI report");
        assert!(midi.notes > 0, "{midi:?}");
        let bytes = std::fs::read(&path).unwrap();
        let imported =
            rustel_smf::import_smf(&bytes, &rustel_smf::ImportOptions::default()).unwrap();
        assert!(!imported.report.lanes.is_empty());
    }

    #[test]
    fn the_meter_sets_the_tick_rate() {
        let meter = Meter::new(6, 8).unwrap();
        let (timeline, report) = timeline(
            &[onset(1, 0.0, 1.5, serde_json::json!({ "note": 60 }))],
            f64::INFINITY,
            2.0 / 3.0,
            SmfOptions { ppq: 480, meter },
        )
        .unwrap();
        assert_eq!(timeline.meter, meter);
        assert_eq!(report.tempo_bpm, 120.0);
        assert_eq!(timeline.tracks[0].messages[1].tick, 1440);
    }
}
