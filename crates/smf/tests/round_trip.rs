/*
round_trip.rs - Standard MIDI File round trip tests
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

use rustel_smf::{
    ImportOptions, Message, MessageKind, Meter, Timeline, Track, import_smf,
    tempo_micros_per_quarter,
};

fn message(tick: u64, kind: MessageKind) -> Message {
    Message { tick, kind }
}

fn bytes(tracks: Vec<Track>) -> Vec<u8> {
    Timeline {
        ppq: 480,
        tempo_micros_per_quarter: tempo_micros_per_quarter(120.0).unwrap(),
        meter: Meter::FOUR_FOUR,
        tracks,
    }
    .to_bytes()
    .unwrap()
}

#[test]
fn a_written_file_imports_as_the_same_music() {
    let bytes = bytes(vec![Track {
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
            message(
                480,
                MessageKind::NoteOn {
                    note: 62,
                    velocity: 100,
                },
            ),
            message(960, MessageKind::NoteOff { note: 62 }),
        ],
    }]);
    let imported = import_smf(&bytes, &ImportOptions::default()).unwrap();
    assert_eq!(imported.report.tempo_bpm, 120.0);
    assert_eq!(imported.report.meter, Meter::FOUR_FOUR);
    assert_eq!(imported.report.bars, 1);
    assert_eq!(imported.report.lanes.len(), 1);
    assert_eq!(imported.report.lanes[0].label, "lead");
    assert_eq!(imported.report.lanes[0].channel, 1);
    assert_eq!(imported.report.lanes[0].program, None);
    assert_eq!(imported.report.lanes[0].notes, 2);
    assert!(imported.source.contains("lead: note(\"c4 d4"));
}

#[test]
fn programs_and_drums_survive_the_round_trip() {
    let bytes = bytes(vec![
        Track {
            name: "lead".to_owned(),
            channel: 1,
            messages: vec![
                message(0, MessageKind::ProgramChange { program: 40 }),
                message(
                    0,
                    MessageKind::NoteOn {
                        note: 60,
                        velocity: 100,
                    },
                ),
                message(480, MessageKind::NoteOff { note: 60 }),
            ],
        },
        Track {
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
                    240,
                    MessageKind::NoteOn {
                        note: 38,
                        velocity: 100,
                    },
                ),
                message(480, MessageKind::NoteOff { note: 38 }),
            ],
        },
    ]);
    let imported = import_smf(&bytes, &ImportOptions::default()).unwrap();
    assert_eq!(imported.report.lanes.len(), 2);
    let lead = &imported.report.lanes[0];
    assert_eq!(lead.label, "lead");
    assert_eq!(lead.program, Some(40));
    assert_eq!(lead.sound, "gm_violin");
    assert!(imported.source.contains(".s(\"gm_violin\").progNum(40)"));
    let drums = &imported.report.lanes[1];
    assert_eq!(drums.label, "drums");
    assert_eq!(drums.program, None);
    assert_eq!(drums.sound, "drums");
    assert!(imported.source.contains("drums: s(\"bd sd"));
}

#[test]
fn the_import_is_deterministic() {
    let bytes = bytes(vec![Track {
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
    }]);
    let first = import_smf(&bytes, &ImportOptions::default()).unwrap();
    let second = import_smf(&bytes, &ImportOptions::default()).unwrap();
    assert_eq!(first, second);
}
