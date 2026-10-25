/*
midi.rs - Standard MIDI File import and export through the command
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

use std::path::Path;

use super::*;
use rustel_smf::{ImportOptions, Meter, import_smf};

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn score(directory: &Path, name: &str) -> PathBuf {
    let path = directory.join(name);
    std::fs::write(&path, "$: note(\"c4 e4 g4 b4\").s(\"gm_piano\")").unwrap();
    path
}

fn export(score: &Path, output: &Path) {
    let result = run(&[
        "export",
        score.to_str().unwrap(),
        "--cycles",
        "2",
        "--format",
        "midi",
        "-o",
        output.to_str().unwrap(),
    ]);
    assert!(result.status.success(), "{}", text(&result.stderr));
}

fn imported(bytes: &[u8]) -> rustel_smf::ImportReport {
    import_smf(bytes, &ImportOptions::default()).unwrap().report
}

#[test]
fn export_writes_a_type_1_file_and_infers_the_extension() {
    let directory = tempfile::tempdir().unwrap();
    let score = score(directory.path(), "take.strudel");
    let midi = directory.path().join("take.mid");
    export(&score, &midi);
    let report = imported(&std::fs::read(&midi).unwrap());
    assert_eq!(report.format, 1);
    assert_eq!(report.lanes.len(), 1);
    assert_eq!(report.lanes[0].notes, 8);

    let inferred = directory.path().join("inferred.mid");
    let result = run(&[
        "export",
        score.to_str().unwrap(),
        "--cycles",
        "2",
        "-o",
        inferred.to_str().unwrap(),
    ]);
    assert!(result.status.success(), "{}", text(&result.stderr));
    assert!(std::fs::read(&inferred).unwrap().starts_with(b"MThd"));
}

#[test]
fn export_honors_the_ppq_and_meter() {
    let directory = tempfile::tempdir().unwrap();
    let score = score(directory.path(), "take.strudel");
    let midi = directory.path().join("take.mid");
    let result = run(&[
        "export",
        score.to_str().unwrap(),
        "--cycles",
        "1",
        "--format",
        "midi",
        "--ppq",
        "96",
        "--meter",
        "3/4",
        "-o",
        midi.to_str().unwrap(),
    ]);
    assert!(result.status.success(), "{}", text(&result.stderr));
    let report = imported(&std::fs::read(&midi).unwrap());
    assert_eq!(report.ppq, 96);
    assert_eq!(report.meter, Meter::new(3, 4).unwrap());
}

#[test]
fn midi_flags_are_only_for_midi_output() {
    let directory = tempfile::tempdir().unwrap();
    let score = score(directory.path(), "take.strudel");
    let wav = directory.path().join("take.wav");
    let result = run(&[
        "export",
        score.to_str().unwrap(),
        "--format",
        "wav",
        "--ppq",
        "480",
        "-o",
        wav.to_str().unwrap(),
    ]);
    assert!(!result.status.success());
    assert!(text(&result.stderr).contains("only for MIDI"));

    let midi = directory.path().join("take.mid");
    let result = run(&[
        "export",
        score.to_str().unwrap(),
        "--format",
        "midi",
        "--until-silence",
        "-o",
        midi.to_str().unwrap(),
    ]);
    assert!(!result.status.success());
    assert!(text(&result.stderr).contains("--until-silence"));
}

#[test]
fn import_midi_writes_a_score_and_respects_force() {
    let directory = tempfile::tempdir().unwrap();
    let score = score(directory.path(), "score.strudel");
    let midi = directory.path().join("song.mid");
    export(&score, &midi);

    let default_output = directory.path().join("song.strudel");
    let result = run(&["import-midi", midi.to_str().unwrap()]);
    assert!(result.status.success(), "{}", text(&result.stderr));
    let source = std::fs::read_to_string(&default_output).unwrap();
    assert!(source.contains("setcpm("));
    assert!(source.contains("note("));

    let result = run(&["import-midi", midi.to_str().unwrap()]);
    assert!(!result.status.success());
    assert!(text(&result.stderr).contains("exists"));

    let result = run(&["import-midi", midi.to_str().unwrap(), "--force"]);
    assert!(result.status.success(), "{}", text(&result.stderr));

    let result = run(&[
        "import-midi",
        midi.to_str().unwrap(),
        "-o",
        midi.to_str().unwrap(),
    ]);
    assert!(!result.status.success());
    assert!(text(&result.stderr).contains("replace the input"));
}

#[test]
fn import_midi_json_is_one_object() {
    let directory = tempfile::tempdir().unwrap();
    let score = score(directory.path(), "score.strudel");
    let midi = directory.path().join("song.mid");
    export(&score, &midi);
    let output = directory.path().join("song.strudel");
    let result = run(&[
        "import-midi",
        midi.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--json",
    ]);
    assert!(result.status.success(), "{}", text(&result.stderr));
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert!(
        value["lanes"]
            .as_array()
            .is_some_and(|lanes| !lanes.is_empty())
    );
    assert!(value["warnings"].as_array().is_some());
}

#[test]
fn import_midi_rejects_a_malformed_file() {
    let directory = tempfile::tempdir().unwrap();
    let bad = directory.path().join("bad.mid");
    std::fs::write(&bad, b"not a midi file").unwrap();
    let output = directory.path().join("bad.strudel");
    let result = run(&[
        "import-midi",
        bad.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
    ]);
    assert!(!result.status.success());
    assert!(text(&result.stderr).contains("invalid MIDI file"));
    assert!(!output.exists());
}

#[test]
fn export_import_export_keeps_the_notes() {
    let directory = tempfile::tempdir().unwrap();
    let score = score(directory.path(), "score.strudel");
    let first = directory.path().join("first.mid");
    export(&score, &first);
    let midi = directory.path().join("song.mid");
    std::fs::copy(&first, &midi).unwrap();
    let source = directory.path().join("song.strudel");
    let result = run(&[
        "import-midi",
        midi.to_str().unwrap(),
        "-o",
        source.to_str().unwrap(),
    ]);
    assert!(result.status.success(), "{}", text(&result.stderr));
    let second = directory.path().join("second.mid");
    let result = run(&[
        "export",
        source.to_str().unwrap(),
        "--cycles",
        "2",
        "--format",
        "midi",
        "-o",
        second.to_str().unwrap(),
    ]);
    assert!(result.status.success(), "{}", text(&result.stderr));
    let first = imported(&std::fs::read(&first).unwrap());
    let second = imported(&std::fs::read(&second).unwrap());
    assert_eq!(first.lanes.len(), second.lanes.len());
    assert_eq!(first.lanes[0].notes, second.lanes[0].notes);
}
