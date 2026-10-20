/*
midi_import.rs - Import a dropped Standard MIDI File as a scene
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

use std::path::Path;

use super::*;

impl App {
    /// Import a dropped Standard MIDI File as a new score scene.
    ///
    /// The MIDI file is never opened as a score and never written to. The new
    /// scene gets a new `.strudel` file in the set's folder.
    pub(super) fn import_dropped_midi(&mut self, path: &Path) -> bool {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        if self.options.mini {
            self.status = "MIDI import needs JavaScript mode; turn mini mode off first".into();
            self.log
                .push(LogLevel::Warn, "midi", "import needs JavaScript mode");
            self.dirty_frame = true;
            return false;
        }
        let options = rustel_smf::ImportOptions {
            max_source_bytes: crate::editor::DEFAULT_MAX_DOCUMENT_BYTES,
            ..rustel_smf::ImportOptions::default()
        };
        let imported = match rustel_smf::import_smf_file(path, &options) {
            Ok(imported) => imported,
            Err(error) => {
                self.log
                    .push(LogLevel::Error, "midi", format!("{name}: {error}"));
                self.status = format!("{name} - could not import; see Log");
                self.dirty_frame = true;
                return false;
            }
        };
        let bars = imported.report.bars;
        let notes: usize = imported.report.lanes.iter().map(|lane| lane.notes).sum();
        match self.scenes.create(&imported.source) {
            Ok(id) => {
                let index = self.scenes.index_of(id).unwrap_or(0);
                if bars > 1 {
                    self.scenes.current_mut().rewind = true;
                }
                self.land_on_current_scene();
                self.persist_manifest();
                self.refresh_set_panel(None);
                self.focus = Focus::Editor;
                let bar_word = if bars == 1 { "bar" } else { "bars" };
                let note_word = if notes == 1 { "note" } else { "notes" };
                self.status = format!(
                    "imported {name} - scene {} ({bars} {bar_word}, {notes} {note_word})",
                    index + 1
                );
                for warning in &imported.report.warnings {
                    self.log.push(LogLevel::Warn, "midi", warning.clone());
                }
                self.dirty_frame = true;
                true
            }
            Err(error) => {
                self.log
                    .push(LogLevel::Error, "midi", format!("{name}: {error}"));
                self.status = format!("{name} - could not create a scene; see Log");
                self.dirty_frame = true;
                false
            }
        }
    }
}
