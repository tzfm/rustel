/*
midi_import.rs - Convert a Standard MIDI File into a Rustel score
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

use std::path::Path;

use super::*;
use rustel_smf::{ImportOptions, SmfError, import_smf_file};

pub(super) fn import_midi(
    input: PathBuf,
    output: Option<PathBuf>,
    force: bool,
    json: bool,
) -> Result<(), RuntimeError> {
    let output = output.unwrap_or_else(|| input.with_extension("strudel"));
    if same_path(&input, &output) {
        return Err(RuntimeError::Message(
            "the output would replace the input; choose another file".into(),
        ));
    }
    if output.exists() && !force {
        return Err(RuntimeError::Message(format!(
            "{} exists; pass --force to replace it",
            output.display()
        )));
    }
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(RuntimeError::Io)?;
    }
    let imported = import_smf_file(&input, &ImportOptions::default()).map_err(import_error)?;
    rustel_runtime::replace_file_bytes(&output, ".midi-import-", imported.source.as_bytes())
        .map_err(RuntimeError::Io)?;

    let report = &imported.report;
    if json {
        let lanes: Vec<serde_json::Value> = report
            .lanes
            .iter()
            .map(|lane| {
                serde_json::json!({
                    "label": lane.label,
                    "channel": lane.channel,
                    "program": lane.program,
                    "sound": lane.sound,
                    "notes": lane.notes,
                })
            })
            .collect();
        let object = serde_json::json!({
            "input": input.display().to_string(),
            "output": output.display().to_string(),
            "format": report.format,
            "ppq": report.ppq,
            "tempo_bpm": report.tempo_bpm,
            "meter": format!("{}/{}", report.meter.numerator, report.meter.denominator),
            "bars": report.bars,
            "grid_steps_per_bar": report.grid_steps_per_bar,
            "grid_exact": report.grid_exact,
            "lanes": lanes,
            "warnings": &report.warnings,
        });
        outln!(
            "{}",
            serde_json::to_string_pretty(&object)
                .map_err(|error| RuntimeError::Message(error.to_string()))?
        );
    } else {
        let on = style::stdout_on();
        let notes: usize = report.lanes.iter().map(|lane| lane.notes).sum();
        let path = output.display().to_string();
        outln!(
            "{} {} - {} bar{}, {} lane{}, {} note{}, {} bpm, {}, grid {} steps/bar{}",
            style::green(on, "wrote"),
            style::bold(on, &path),
            report.bars,
            if report.bars == 1 { "" } else { "s" },
            report.lanes.len(),
            if report.lanes.len() == 1 { "" } else { "s" },
            notes,
            if notes == 1 { "" } else { "s" },
            format_tempo(report.tempo_bpm),
            format!("{}/{}", report.meter.numerator, report.meter.denominator),
            report.grid_steps_per_bar,
            if report.grid_exact {
                ""
            } else {
                " (quantized)"
            },
        );
        for warning in &report.warnings {
            eprintln!("warning: {warning}");
        }
    }
    Ok(())
}

fn import_error(error: SmfError) -> RuntimeError {
    match error {
        SmfError::Io(error) => RuntimeError::Io(error),
        error @ (SmfError::TooLarge { .. }
        | SmfError::TooManyEvents { .. }
        | SmfError::TooManyBars { .. }
        | SmfError::TooManyVoices { .. }
        | SmfError::TooManyMessages { .. }) => RuntimeError::ResourceLimit(error.to_string()),
        other => RuntimeError::Message(other.to_string()),
    }
}

fn same_path(input: &Path, output: &Path) -> bool {
    match (input.canonicalize(), output.canonicalize()) {
        (Ok(input), Ok(output)) => input == output,
        _ => false,
    }
}
