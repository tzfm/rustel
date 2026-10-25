//! Getting sounds and scores into the studio. A file or folder dropped on the
//! window arrives as a pasted path; this file spots that, then imports audio
//! folders as sample sources, opens scores as set tabs and refuses anything
//! else. It also covers adding sample sources by name (then jumping to them on
//! Settings > Samples), renaming imported banks and local sample files, and
//! File > Consolidate samples.

use super::*;

/// The paths in what looks like a drop, or `None` when it is ordinary text.
///
/// A terminal has no drop event: a file dragged onto the window arrives as
/// a bracketed paste of its path, and several files arrive as several
/// paths separated by spaces or newlines. Shells quote a path that has
/// spaces in it, and some terminals escape the spaces instead, so both
/// spellings are read back here.
///
/// Every path has to exist for this to be a drop. That is the whole guard
/// against eating a paste that merely looks path-shaped: text you copied
/// from somewhere names nothing on this disk, and goes into the score as
/// it always did.
pub(super) fn dropped_paths(text: &str) -> Option<Vec<PathBuf>> {
    // A backslash is an escape where the shell says so and a path
    // separator where the filesystem does. macOS Terminal delivers a drop
    // of `/Music/deep house` as `/Music/deep\ house`, so unescaping is
    // what makes that a path at all; Windows delivers `C:\Users\me\packs`
    // unquoted, where the same reading gives `C:Usersmepacks` - not
    // absolute, not there, and so never a drop at all.
    parse_dropped_paths(text, !cfg!(windows))
}

/// The parse itself, with the escaping rule handed in so both readings can
/// be checked from either kind of machine.
pub(super) fn parse_dropped_paths(text: &str, escapes: bool) -> Option<Vec<PathBuf>> {
    /// Longer than any plausible selection of paths. Past this it is a
    /// paste, and splitting it up to ask the filesystem about every word
    /// would be a syscall per word of somebody's score.
    const MAX_DROP_BYTES: usize = 4096;
    /// Likewise: nobody drags this many files at once, and a paste that
    /// happens to start with a real path must not walk the whole of it.
    const MAX_DROPPED_FILES: usize = 64;

    let text = text.trim();
    if text.is_empty() || text.len() > MAX_DROP_BYTES {
        return None;
    }
    let mut paths = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let push = |current: &mut String, paths: &mut Vec<PathBuf>| {
        if !current.is_empty() {
            paths.push(PathBuf::from(std::mem::take(current)));
        }
    };
    for character in text.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        match (character, quote) {
            ('\\', None) if escapes => escaped = true,
            ('\'' | '"', None) => quote = Some(character),
            (c, Some(open)) if c == open => quote = None,
            (c, None) if c.is_whitespace() => push(&mut current, &mut paths),
            (c, _) => current.push(c),
        }
    }
    push(&mut current, &mut paths);
    if paths.is_empty() || paths.len() > MAX_DROPPED_FILES || quote.is_some() || escaped {
        return None;
    }
    // Explorer and some terminals deliver a dropped folder with a trailing
    // separator - `C:\test\` - and a Unix terminal may spell one the same
    // way. The path inside the paste is the folder either way; the
    // separator is spelling, not part of the name. Keeping it would leave
    // a source spec that never matches the same folder added from
    // Settings, so a trailing one comes off here - except from a drive
    // root (`C:\`), where the separator is the name.
    let paths = paths
        .into_iter()
        .map(|path| {
            let text = path.as_os_str().to_string_lossy();
            let mut chars = text.chars();
            let trailing = matches!(chars.next_back(), Some('\\' | '/'));
            // A lone separator (`\`, `/`) is a drive root, whose trailing
            // separator IS the name: leave it.
            let root = path
                .parent()
                .is_none_or(|parent| parent.as_os_str().is_empty());
            if trailing && !root {
                PathBuf::from(text.trim_end_matches(['\\', '/']))
            } else {
                path
            }
        })
        .collect::<Vec<_>>();
    // Absolute, and there. A terminal always delivers a dropped file by its
    // full path; a pasted word that happens to name something in the working
    // directory - `docs`, `src`, `.` - is text and must not be taken as a
    // drop. Short-circuits on the first miss, which for ordinary text is the
    // first word.
    paths
        .iter()
        .all(|path| path.is_absolute() && path.exists())
        .then_some(paths)
}

/// Whether a dropped file is one of this studio's scores.
fn is_score_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case(super::super::scenes::SCENE_EXTENSION)
        })
}

/// Whether a dropped file is a Standard MIDI File.
fn is_midi_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("mid") || extension.eq_ignore_ascii_case("midi")
        })
}

fn normalized_sample_source(spec: &str) -> String {
    let spec = spec.trim();
    let path = std::path::Path::new(spec);
    if path.is_file()
        && rustel_runtime::samples::is_sample_audio(path)
        && let Some(parent) = path.parent()
    {
        parent.display().to_string()
    } else {
        spec.to_owned()
    }
}

impl App {
    /// Keep a rename, or drop it when the name given back is the original.
    pub(super) fn rename_bank(&mut self, to: &str) {
        let Some(from) = self.renaming_bank.take() else {
            return;
        };
        let source = self.renaming_bank_source.take();
        let to = to.trim();
        if to.is_empty() {
            return;
        }
        // Two banks under one name would be one row in the browser with no
        // way to say which is which - and no way to rename either back.
        let current = source
            .as_deref()
            .and_then(|spec| {
                self.worker
                    .library()
                    .map(|library| library.alias_for_import(spec, &from))
            })
            .unwrap_or_else(|| {
                self.prefs
                    .sample_renames
                    .get(&from)
                    .cloned()
                    .unwrap_or_else(|| from.clone())
            });
        let taken_by_rename = self
            .prefs
            .sample_renames
            .iter()
            .any(|(other, target)| *other != from && target == to)
            || self
                .prefs
                .sample_source_renames
                .iter()
                .any(|(other_source, aliases)| {
                    aliases.iter().any(|(other, target)| {
                        (source.as_deref() != Some(other_source.as_str()) || *other != from)
                            && target == to
                    })
                });
        let taken_in_catalogue = to != current
            && to != from
            && self
                .worker
                .library()
                .is_some_and(|library| library.knows(to));
        if taken_by_rename || taken_in_catalogue {
            self.renaming_bank = Some(from);
            self.status = format!("{to} is already a sound's name - choose another");
            self.renaming_bank = None;
            return;
        }
        if to == current {
            self.status = format!("{from} already plays as {to}");
            return;
        }
        if to == from {
            // Back to the name it arrived with: the overlay is removed
            // rather than recorded as a rename to itself.
            if let Some(source) = source.as_ref() {
                if let Some(aliases) = self.prefs.sample_source_renames.get_mut(source) {
                    aliases.remove(&from);
                    if aliases.is_empty() {
                        self.prefs.sample_source_renames.remove(source);
                    }
                }
            } else {
                self.prefs.sample_renames.remove(&from);
            }
            self.status = format!("{from} - back to its own name");
        } else {
            if let Some(source) = source.as_ref() {
                self.prefs
                    .sample_source_renames
                    .entry(source.clone())
                    .or_default()
                    .insert(from.clone(), to.to_owned());
            } else {
                self.prefs
                    .sample_renames
                    .insert(from.clone(), to.to_owned());
            }
            self.status = format!("{from} plays as {to}");
        }
        self.save_prefs_soon();
        if let Some(panel) = self.reference_panel.as_mut() {
            panel.prepare_sound_rename(&current, to);
        }
        // Both imported layers answer to the overlay, so both are taken up
        // again: a set's own folder can hold the renamed bank as easily as
        // an imported one.
        if let Some(library) = self.worker.library() {
            library.set_bank_renames(self.prefs.bank_renames());
            library.set_source_bank_renames(self.prefs.source_bank_renames());
        }
        self.adopt_set_samples();
        self.adopt_global_sources();
    }

    /// Copy the samples this set's scores name into the set's own folder.
    ///
    /// This makes a set that can be handed to someone else. A reference to
    /// a folder on your own disk is the right default: a copy of a library
    /// in every set is not necessary, and a stale copy is worse than a
    /// missing one. But such a set plays only on this machine.
    ///
    /// The copy is a deliberate act, not a policy. Nothing needs a rewrite,
    /// because the set's own folder is already a source and already wins
    /// by name. Copying the files in is the whole operation, and the same
    /// score plays the same sounds afterwards.
    ///
    /// Only what the scores actually name is copied, not the whole of
    /// whatever folder they came from.
    pub(super) fn consolidate_samples(&mut self) {
        let Some(library) = self.worker.library() else {
            return;
        };
        let root = self.scenes.directory().to_path_buf();
        let mut wanted: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for scene in self.scenes.scores() {
            for sound in rustel_runtime::sounds::in_score(&scene.editor.source()) {
                let name = sound
                    .split_once(':')
                    .map_or(sound.as_str(), |(name, _)| name);
                wanted.insert(name.to_owned());
            }
        }
        if wanted.is_empty() {
            self.status = "nothing to copy - this set's scores name no samples".into();
            return;
        }
        let mut copied = 0usize;
        let mut already = 0usize;
        let mut failed: Vec<String> = Vec::new();
        for name in wanted {
            match library.set_folder_holds(&root, &name) {
                // The set already answers this name out of its own folder:
                // copying over it would replace what the player put there.
                true => already += 1,
                false => match library.copy_bank_into(&name, &root) {
                    Ok(0) => {}
                    Ok(files) => copied += files,
                    Err(error) => failed.push(format!("{name}: {error}")),
                },
            }
        }
        if copied > 0 {
            // The folder is a source, so what was just written into it has
            // to be taken up before it can be played from there.
            self.adopt_set_samples();
        }
        self.status = match (copied, already, failed.is_empty()) {
            (0, _, true) if already > 0 => {
                "the set already carries every sample its scores name".into()
            }
            (0, _, true) => "nothing copied - these sounds have no files to copy".into(),
            (files, _, true) => format!(
                "{files} file(s) copied into {} - the set plays on its own now",
                self.scenes.name()
            ),
            (files, _, false) => format!("{files} file(s) copied; {}", failed.join(", ")),
        };
        for message in failed {
            self.log.push(LogLevel::Warn, "samples", message);
        }
        self.dirty_frame = true;
    }

    /// A file or folder dropped on the window.
    ///
    /// What it is decides what happens, because where you dropped it is
    /// not something a terminal can tell us: audio and folders of audio are
    /// imported for every set, and a score opens as a tab. Anything else
    /// says so rather than being pasted into the score as a path, which is
    /// what a drop used to do.
    pub(super) fn accept_drop(&mut self, paths: &[PathBuf]) {
        // A drop is the one gesture with no trace of its own: the terminal
        // reports it as a paste, the panel it lands in may be showing
        // something else entirely, and until now a drop that brought
        // nothing left nothing behind to read. Say what arrived before
        // deciding anything about it.
        self.log.push(
            LogLevel::Info,
            "drop",
            match paths {
                [only] => format!("dropped {}", only.display()),
                many => format!(
                    "dropped {} item(s): {}",
                    many.len(),
                    many.iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            },
        );
        let mut opened = 0usize;
        let mut midi = 0usize;
        let mut presets = 0usize;
        let mut refused: Vec<String> = Vec::new();
        // Collected rather than adopted one at a time: adopting re-walks
        // every source, and a selection of eight files would otherwise walk
        // them all eight times.
        let mut specs: Vec<String> = Vec::new();
        let remember = |spec: String, specs: &mut Vec<String>| {
            if !specs.contains(&spec) {
                specs.push(spec);
            }
        };
        for path in paths {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            if path.is_dir() {
                remember(path.display().to_string(), &mut specs);
            } else if rustel_runtime::samples::is_sample_audio(path) {
                // A file is a convenient way to choose its containing bank.
                // Store the folder as the source so sibling takes are
                // variants of one bank rather than one source per file.
                if let Some(parent) = path.parent() {
                    remember(parent.display().to_string(), &mut specs);
                }
            } else if is_score_path(path) {
                self.open_dropped_score(path);
                opened += 1;
            } else if is_midi_path(path) {
                if self.import_dropped_midi(path) {
                    midi += 1;
                }
            } else if self.take_dropped_preset(path) {
                presets += 1;
            } else {
                refused.push(name);
            }
        }
        let imported = match specs.as_slice() {
            [] => 0,
            [only] => {
                self.add_sample_source(only);
                usize::from(
                    self.prefs
                        .sample_sources
                        .iter()
                        .any(|source| source.spec == *only),
                )
            }
            many => self.add_sample_sources(many),
        };
        if imported == 0 && opened == 0 && midi == 0 && presets == 0 && !refused.is_empty() {
            self.status = format!(
                "nothing to import from {} - audio, a folder of audio, a score, or a MIDI file",
                refused.join(", ")
            );
        } else if !refused.is_empty() {
            self.status = format!(
                "{} skipped: not audio, a score, or a MIDI file",
                refused.join(", ")
            );
        }
        if !refused.is_empty() {
            self.log.push(
                LogLevel::Warn,
                "drop",
                format!(
                    "not audio, a folder of audio, a score, or a MIDI file: {}",
                    refused.join(", ")
                ),
            );
        }
        self.log.push(
            LogLevel::Debug,
            "drop",
            format!(
                "{imported} source(s) imported · {opened} score(s) opened · {midi} MIDI file(s) \
                 imported · {presets} preset(s) added · {} refused",
                refused.len()
            ),
        );
        self.dirty_frame = true;
    }

    /// A `.vstpreset` file dropped on the window goes to the preset folder
    /// of its plugin. False for a file of another kind, and in a build with
    /// no plugin support.
    fn take_dropped_preset(&mut self, path: &std::path::Path) -> bool {
        #[cfg(feature = "vst")]
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("vstpreset"))
        {
            self.add_dropped_preset(path);
            return true;
        }
        let _ = path;
        false
    }

    /// A score dropped on the window opens as a tab of the current set -
    /// the same landing as opening it from the set panel, so a drop and a
    /// click end in the same place.
    pub(super) fn open_dropped_score(&mut self, path: &std::path::Path) {
        let already = self
            .scenes
            .scores()
            .find(|scene| scene.path == path)
            .map(|scene| scene.id)
            .and_then(|id| self.scenes.index_of(id));
        if let Some(index) = already {
            self.select_scene(index);
            self.focus = Focus::Editor;
            self.status = format!(
                "{} is already open",
                status_file_path(path, self.ui_settings.show_full_paths)
            );
            return;
        }
        match self.scenes.open_file(path) {
            Ok(id) => {
                let index = self.scenes.index_of(id).unwrap_or(0);
                self.land_on_current_scene();
                self.persist_manifest();
                self.refresh_set_panel(Some(path.to_path_buf()));
                self.focus = Focus::Editor;
                self.status = format!("scene {} - {}", index + 1, self.scenes.current().name());
            }
            Err(error) => {
                self.log.push(
                    LogLevel::Warn,
                    "file",
                    format!("{}: {error}", path.display()),
                );
                self.status = if self.ui_settings.show_full_paths {
                    format!("{}: {error}", path.display())
                } else {
                    format!(
                        "{} - could not open; see Log",
                        status_file_path(path, false)
                    )
                };
            }
        }
    }

    /// Open Settings ▸ Samples and put the cursor on an imported source.
    fn reveal_imported_source(&mut self, spec: &str) {
        let at = self
            .prefs
            .sample_sources
            .iter()
            .position(|source| source.spec == spec);
        if self.settings_sheet.is_none() {
            self.toggle_settings_sheet();
        }
        if let Some(sheet) = self.settings_sheet.as_mut() {
            sheet.show_page(settings::SettingsPage::Sources);
            if let Some(at) = at {
                sheet.selected = super::super::settings::SOURCE_CONTROL_COUNT + at;
            }
        }
        self.focus_panel(PanelKind::Settings);
        self.dirty_frame = true;
    }

    /// Alias a bank from a user import on Sources (`r`): pick the bank when
    /// the pack has several, then the rename prompt.
    pub(super) fn open_bank_rename_from_source(&mut self, at: usize) {
        let Some(source) = self.prefs.sample_sources.get(at).cloned() else {
            return;
        };
        let Some(library) = self.worker.library() else {
            self.status = "the sample library is not up yet".into();
            return;
        };
        let banks = library.banks_for_import(&source.spec);
        self.renaming_bank_source = Some(source.spec.clone());
        match banks.as_slice() {
            [] => {
                self.renaming_bank_source = None;
                self.status = format!(
                    "{} - no banks to alias yet",
                    status_sample_source(&source.spec, self.ui_settings.show_full_paths)
                );
            }
            [only] => {
                self.renaming_bank = Some(only.clone());
                let offered = library.alias_for_import(&source.spec, only);
                self.open_set_prompt(SetPrompt::RenameBank);
                if let Some((_, picker)) = self.set_prompt.as_mut() {
                    picker.offer(&offered);
                }
            }
            many => {
                let candidates = many
                    .iter()
                    .map(|name| super::super::file_picker::Candidate {
                        label: library.alias_for_import(&source.spec, name),
                        detail: "bank".to_owned(),
                        path: std::path::PathBuf::from(name),
                    })
                    .collect();
                let mut picker = super::super::file_picker::FilePicker::new(
                    "alias a bank",
                    "picks which sound to rename",
                    candidates,
                    std::path::Path::new("."),
                );
                picker.browsable = false;
                picker.label = "bank";
                self.open_set_prompt(SetPrompt::PickBankToRename);
                self.set_prompt = Some((SetPrompt::PickBankToRename, picker));
                self.focus_panel(PanelKind::Set);
                self.status = "alias - pick the bank that clashes, then name it · Esc back".into();
                self.dirty_frame = true;
            }
        }
    }

    /// Keep the sources that are new, and take them all up at once.
    ///
    /// Returns how many were added. One adoption for the lot: adopting
    /// re-walks every source there is, so doing it per file would make a
    /// dropped selection quadratic in the number of files.
    pub(super) fn add_sample_sources(&mut self, specs: &[String]) -> usize {
        let specs: Vec<String> = specs
            .iter()
            .map(|spec| normalized_sample_source(spec))
            .collect();
        let before = self.prefs.sample_sources.clone();
        let mut refresh_existing = false;
        for spec in &specs {
            let spec = spec.trim();
            if spec.is_empty() || self.refuse_set_output_source(spec) {
                continue;
            }
            if self
                .prefs
                .sample_sources
                .iter()
                .any(|source| source.spec == spec)
            {
                // Dropping the same folder again is the refresh gesture.
                // Keep its one source row, but walk it again so folders or
                // files created since the first import appear immediately.
                refresh_existing = true;
                continue;
            }
            self.prefs
                .sample_sources
                .push(super::super::prefs::SampleSourcePref {
                    spec: spec.to_owned(),
                    enabled: true,
                });
        }
        self.prefs.deduplicate_sample_sources();
        let changed = self.prefs.sample_sources != before;
        let added = self
            .prefs
            .sample_sources
            .iter()
            .filter(|source| {
                specs.iter().any(|spec| spec.trim() == source.spec)
                    && !before.iter().any(|old| old == *source)
            })
            .count();
        if !changed {
            if refresh_existing {
                self.adopt_global_sources();
            }
            return 0;
        }
        self.save_prefs_soon();
        self.adopt_global_sources();
        if specs.len() > 1 {
            // The receipt is what arrived, not what was asked for: two
            // rows that both came back missing are not "2 imported".
            use rustel_runtime::samples::GlobalSourceState;
            let mut landed = 0usize;
            let mut trouble: Vec<String> = Vec::new();
            for spec in &specs {
                match self
                    .source_reports
                    .iter()
                    .find(|report| report.spec == spec.trim())
                    .map(|report| &report.state)
                {
                    Some(GlobalSourceState::Ready { .. } | GlobalSourceState::Loading) => {
                        landed += 1;
                    }
                    Some(GlobalSourceState::Missing(why) | GlobalSourceState::Failed(why)) => {
                        trouble.push(status_sample_source_error(
                            spec,
                            why,
                            self.ui_settings.show_full_paths,
                        ));
                    }
                    _ => {}
                }
            }
            if landed > 0
                && let Some(spec) = specs.iter().map(|s| s.trim()).find(|s| !s.is_empty())
            {
                self.reveal_imported_source(spec);
            }
            self.status = match (landed, trouble.is_empty()) {
                (0, false) => trouble.join(" · "),
                (n, true) => format!("{n} source(s) imported"),
                (n, false) => format!("{n} source(s) imported · {}", trouble.join(" · ")),
            };
        }
        added
    }

    /// Refuse a set's own output folder as a sample source, on the status
    /// line and in the log. Returns whether `spec` was refused.
    pub(super) fn refuse_set_output_source(&mut self, spec: &str) -> bool {
        let Some(folder) =
            sample_source_folder(spec).and_then(|folder| self.set_output_folder(folder))
        else {
            return false;
        };
        let reason = super::sets::set_output_reason(&folder);
        self.status = format!(
            "{} not imported - {reason}",
            status_sample_source(spec, self.ui_settings.show_full_paths)
        );
        self.log.push(
            LogLevel::Info,
            "samples",
            format!("{} not imported: {reason}", spec.trim()),
        );
        self.dirty_frame = true;
        true
    }

    /// Add a source the player named, and show what it brought.
    pub(super) fn add_sample_source(&mut self, spec: &str) {
        let spec = normalized_sample_source(spec);
        if self.add_sample_sources(std::slice::from_ref(&spec)) == 0 {
            return;
        }
        let spec = spec.as_str();
        // Landing on Sources IS the confirmation. In a terminal there is
        // no folder to watch fill up, so a source that imported silently
        // has not visibly done anything at all.
        let brought = self
            .source_reports
            .iter()
            .find(|report| report.spec == spec)
            .map(|report| report.state.clone());
        // The status line is one line and is gone by the next thing that
        // happens. The receipt for an import belongs somewhere it can be
        // read afterwards, with the path spelled out in full.
        match &brought {
            Some(rustel_runtime::samples::GlobalSourceState::Ready { banks }) => self.log.push(
                LogLevel::Info,
                "samples",
                format!("imported {spec} - {banks} bank(s)"),
            ),
            Some(rustel_runtime::samples::GlobalSourceState::Loading) => self.log.push(
                LogLevel::Info,
                "samples",
                format!("importing {spec} - fetching its list of sounds"),
            ),
            _ => {}
        }
        if matches!(
            brought,
            Some(
                rustel_runtime::samples::GlobalSourceState::Ready { .. }
                    | rustel_runtime::samples::GlobalSourceState::Loading
            )
        ) {
            self.reveal_imported_source(spec);
        }
        self.show_sample_source_status(spec, "fetching its list of sounds");
    }

    pub(super) fn rename_sample_file(&mut self, stem: &str) {
        let Some((bank, variant, expected)) = self.renaming_sample.clone() else {
            return;
        };
        let Some(library) = self.worker.library() else {
            return;
        };
        match library.rename_local_sample(&bank, variant, &expected, stem) {
            Ok(path) => {
                self.renaming_sample = None;
                self.close_set_prompt();
                let stem = path.file_stem().unwrap_or_default().to_string_lossy();
                if let Some(panel) = self.reference_panel.as_mut() {
                    panel.prepare_sample_rename(
                        &bank,
                        variant,
                        &stem,
                        &format!("file://{}", path.display()),
                    );
                }
                // Cancel any folder walk staged before the rename and publish
                // a new catalogue in the background, keeping the current rows.
                self.adopt_global_sources();
                self.refresh_catalogue();
                self.lint_pending = true;
                self.status =
                    format!("{bank}:{variant} ({stem}) - s(\"{stem}\") also plays this file");
            }
            Err(error) => {
                if let Some((_, picker)) = self.set_prompt.as_mut() {
                    picker.error = Some(error.clone());
                }
                self.status = error;
            }
        }
        self.dirty_frame = true;
    }
}

#[cfg(test)]
mod tests {
    mod sample_import {
        //! Tests for `app/sample_import.rs`.

        use crate::app::sample_import::*;
        use crate::app::tests::support::select_bank_row;

        /// A terminal has no drop event: a dropped file arrives as a paste of
        /// its path. A paste is a drop only when every path in it exists.
        #[test]
        fn a_dropped_path_is_told_apart_from_a_paste() {
            let directory = tempfile::tempdir().expect("temp dir");
            let plain = directory.path().join("kick.wav");
            std::fs::write(&plain, b"x").expect("fixture");
            let spaced = directory.path().join("deep bass.wav");
            std::fs::write(&spaced, b"x").expect("fixture");

            assert_eq!(
                dropped_paths(&plain.display().to_string()),
                Some(vec![plain.clone()])
            );
            // Terminals hand a path with spaces over quoted, or escaped, and
            // both spellings are the same file. The escaped spelling is what
            // macOS Terminal sends, and every sample pack on a real machine is
            // named with spaces in it.
            assert_eq!(
                dropped_paths(&format!("'{}'", spaced.display())),
                Some(vec![spaced.clone()])
            );
            #[cfg(not(windows))]
            assert_eq!(
                dropped_paths(&spaced.display().to_string().replace(' ', "\\ ")),
                Some(vec![spaced.clone()])
            );
            // On Windows a backslash is the path separator, not an escape. The
            // escaping rule is a parameter, so both rules run on every platform.
            // `written` names an existing file with a backslash in its text: one
            // file name on Unix, a nested file on Windows.
            let backslash = if cfg!(windows) {
                let nested = directory.path().join("deep");
                std::fs::create_dir(&nested).expect("fixture folder");
                nested.join("bass.wav")
            } else {
                directory.path().join("deep\\bass.wav")
            };
            std::fs::write(&backslash, b"x").expect("fixture");
            let written = backslash.display().to_string();
            assert_eq!(
                parse_dropped_paths(&written, false),
                Some(vec![backslash.clone()]),
                "a separator survives where nothing escapes"
            );
            assert_eq!(
                parse_dropped_paths(&written, true),
                None,
                "and is eaten where everything does - which was the Windows bug"
            );
            // With escapes on, a trailing backslash is an unfinished escape, so
            // the parse refuses the path.
            assert_eq!(
                parse_dropped_paths(&format!("{}\\", directory.path().display()), true),
                None,
                "a trailing separator read as an unfinished escape"
            );
            if cfg!(windows) {
                let spelled = format!("{}\\", directory.path().display());
                assert_eq!(
                    dropped_paths(&spelled),
                    Some(vec![directory.path().to_path_buf()]),
                    "Explorer's trailing backslash is a folder drop on Windows"
                );
            }
            // Several files at once, the way a multi-selection arrives.
            assert_eq!(
                dropped_paths(&format!("{} '{}'", plain.display(), spaced.display())),
                Some(vec![plain.clone(), spaced.clone()])
            );

            // Ordinary text stays ordinary text, even when it is path-shaped.
            assert_eq!(dropped_paths("s(\"bd sd\")"), None);
            assert_eq!(dropped_paths("/no/such/file/anywhere.wav"), None);
            assert_eq!(dropped_paths(""), None);
            // A word that names something in the working directory is still a
            // word: a terminal delivers a drop by its full path.
            assert_eq!(dropped_paths("docs"), None, "relative, so not a drop");
            assert_eq!(dropped_paths("."), None);
            assert_eq!(dropped_paths("Cargo.toml"), None);
            // One real path and one that is not is not a drop: half-importing a
            // selection would be worse than declining it.
            assert_eq!(
                dropped_paths(&format!("{} /no/such/thing.wav", plain.display())),
                None
            );

            // A pasted score is not split into a thousand words and asked about
            // one syscall at a time.
            let long = format!("{} ", plain.display()).repeat(200);
            assert_eq!(dropped_paths(&long), None, "too long to be a drop");
            let many = std::iter::repeat_n(plain.display().to_string(), 65)
                .collect::<Vec<_>>()
                .join(" ");
            assert_eq!(dropped_paths(&many), None, "too many to be a drop");
        }

        /// What a drop does is decided by what was dropped: a folder and the
        /// sounds inside it are one source, a score opens as a tab, and what
        /// is neither is named in the status rather than swallowed.
        #[test]
        fn a_drop_is_dispatched_on_what_it_is() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            let dropped = tempfile::tempdir().expect("dropped folder");
            let wav = |name: &str| {
                let mut bytes = Vec::new();
                bytes.extend_from_slice(b"RIFF");
                bytes.extend_from_slice(&40u32.to_le_bytes());
                bytes.extend_from_slice(b"WAVEfmt ");
                bytes.extend_from_slice(&16u32.to_le_bytes());
                bytes.extend_from_slice(&1u16.to_le_bytes());
                bytes.extend_from_slice(&1u16.to_le_bytes());
                bytes.extend_from_slice(&48_000u32.to_le_bytes());
                bytes.extend_from_slice(&96_000u32.to_le_bytes());
                bytes.extend_from_slice(&2u16.to_le_bytes());
                bytes.extend_from_slice(&16u16.to_le_bytes());
                bytes.extend_from_slice(b"data");
                bytes.extend_from_slice(&4u32.to_le_bytes());
                bytes.extend_from_slice(&[0, 0, 0, 0]);
                std::fs::write(dropped.path().join(name), bytes).expect("fixture");
            };
            wav("a.wav");
            wav("b.wav");
            let score = dropped.path().join("groove.strudel");
            std::fs::write(&score, "$: s(\"bd\")").expect("fixture");
            let notes = dropped.path().join("notes.txt");
            std::fs::write(&notes, "hello").expect("fixture");
            let scores_before = app.scenes.scores().count();

            app.accept_drop(&[
                dropped.path().to_path_buf(),
                dropped.path().join("a.wav"),
                dropped.path().join("b.wav"),
                score.clone(),
                notes,
            ]);

            // The folder and files inside it are one source. The direct files
            // become variants of the folder bank rather than permanent rows of
            // their own.
            let specs: Vec<&str> = app
                .prefs
                .sample_sources
                .iter()
                .map(|source| source.spec.as_str())
                .collect();
            assert_eq!(specs.len(), 1, "{specs:?}");
            assert!(specs.contains(&dropped.path().display().to_string().as_str()));
            assert_eq!(
                app.scenes.scores().count(),
                scores_before + 1,
                "the score opened as a tab"
            );
            assert!(
                app.status.contains("notes.txt"),
                "what was neither is named: {}",
                app.status
            );

            // The same score dropped again is selected, not opened twice.
            app.accept_drop(std::slice::from_ref(&score));
            assert_eq!(app.scenes.scores().count(), scores_before + 1);
        }

        /// A dropped audio file chooses its containing folder. Its siblings are
        /// variants of one folder-named bank, and the saved source is the folder.
        #[test]
        fn dropping_one_file_imports_its_containing_folder() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            let dropped = tempfile::tempdir().expect("dropped folder");
            let wav = |name: &str| {
                let mut bytes = Vec::new();
                bytes.extend_from_slice(b"RIFF");
                bytes.extend_from_slice(&40u32.to_le_bytes());
                bytes.extend_from_slice(b"WAVEfmt ");
                bytes.extend_from_slice(&16u32.to_le_bytes());
                bytes.extend_from_slice(&1u16.to_le_bytes());
                bytes.extend_from_slice(&1u16.to_le_bytes());
                bytes.extend_from_slice(&48_000u32.to_le_bytes());
                bytes.extend_from_slice(&96_000u32.to_le_bytes());
                bytes.extend_from_slice(&2u16.to_le_bytes());
                bytes.extend_from_slice(&16u16.to_le_bytes());
                bytes.extend_from_slice(b"data");
                bytes.extend_from_slice(&4u32.to_le_bytes());
                bytes.extend_from_slice(&[0, 0, 0, 0]);
                let path = dropped.path().join(name);
                std::fs::write(&path, bytes).expect("fixture");
                path
            };
            let lone = wav("solo.wav");
            wav("sibling.wav");

            app.accept_drop(std::slice::from_ref(&lone));
            assert_eq!(
                app.prefs.sample_sources,
                vec![crate::prefs::SampleSourcePref {
                    spec: dropped.path().display().to_string(),
                    enabled: true,
                }],
                "the dropped file selects its containing folder"
            );

            // A directory, dropped on its own, still brings the whole thing.
            let directory2 = tempfile::tempdir().expect("temp dir");
            let mut app2 = crate::app::tests::support::app_over(directory2.path());
            app2.accept_drop(&[dropped.path().to_path_buf()]);
            assert_eq!(
                app2.prefs.sample_sources,
                vec![crate::prefs::SampleSourcePref {
                    spec: dropped.path().display().to_string(),
                    enabled: true,
                }],
                "a dropped folder is still one source, itself"
            );
        }

        /// Windows Explorer pastes a dropped folder with a trailing separator,
        /// such as `C:\test\`. The drop imports the folder under the same spec
        /// that the Settings page uses, so both agree on what is already
        /// imported.
        #[cfg(windows)]
        #[test]
        fn a_folder_dropped_with_a_trailing_separator_matches_a_settings_add() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            let dropped = tempfile::tempdir().expect("dropped folder");
            std::fs::write(dropped.path().join("snap.wav"), b"not really audio").expect("fixture");

            let explorer_spelling = format!("{}\\", dropped.path().display());
            let paths = dropped_paths(&explorer_spelling)
                .expect("the Explorer spelling of a folder drop parses");
            app.accept_drop(&paths);
            assert_eq!(
                app.prefs.sample_sources,
                vec![crate::prefs::SampleSourcePref {
                    spec: dropped.path().display().to_string(),
                    enabled: true,
                }],
                "the spec is the folder, without the trailing separator"
            );

            // Adding the same folder again through the Settings spelling
            // changes nothing: the two entries are one source.
            app.add_sample_source(&dropped.path().display().to_string());
            assert_eq!(app.prefs.sample_sources.len(), 1, "no duplicate row");
        }

        /// One dropped folder gets the same receipt as one typed: the browser
        /// opens on what arrived and the status says what it brought.
        #[test]
        fn a_single_dropped_folder_is_announced() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            let dropped = tempfile::tempdir().expect("dropped folder");
            std::fs::write(dropped.path().join("snap.wav"), b"not really audio").expect("fixture");
            app.accept_drop(&[dropped.path().to_path_buf()]);
            assert!(
                app.status.contains("imported") || app.status.contains("sound"),
                "a receipt: {}",
                app.status
            );
            assert!(
                app.settings_sheet
                    .is_some_and(|sheet| sheet.shows_sources()),
                "Sources opens on the import"
            );
            assert!(app.reference_panel.is_none(), "the browser stays closed");
        }

        /// Renaming a bank again replaces the rename rather than stacking one
        /// on another; renaming it back to its own name removes it; and a name
        /// another sound already answers to is refused.
        #[test]
        fn renames_replace_and_refuse_collisions() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());

            app.renaming_bank = Some("hats".into());
            app.rename_bank("hats_open");
            assert_eq!(
                app.prefs.sample_renames.get("hats").map(String::as_str),
                Some("hats_open")
            );

            app.renaming_bank = Some("hats".into());
            app.rename_bank("hats_closed");
            assert_eq!(
                app.prefs.sample_renames.get("hats").map(String::as_str),
                Some("hats_closed"),
                "a second rename replaces the first"
            );
            assert_eq!(app.prefs.sample_renames.len(), 1);

            // A name already taken by a rename of another bank is refused.
            app.renaming_bank = Some("kick".into());
            app.rename_bank("hats_closed");
            assert!(
                !app.prefs.sample_renames.contains_key("kick"),
                "two banks under one name would be one row"
            );
            assert!(app.status.contains("already"), "{}", app.status);

            // And a name a sound already answers to is refused too - here one
            // an imported folder brought, since the test's library ships no
            // pinned banks of its own.
            let imported = tempfile::tempdir().expect("imported folder");
            std::fs::write(imported.path().join("snap.wav"), b"x").expect("fixture");
            app.add_sample_sources(&[imported.path().display().to_string()]);
            // Walked off the studio's thread, so the name is known a moment
            // later rather than on the call.
            app.settle_sample_sources();
            assert!(
                app.worker
                    .library()
                    .is_some_and(|library| library.knows("snap")),
                "the folder's sound is known"
            );
            app.renaming_bank = Some("kick".into());
            app.rename_bank("snap");
            assert!(!app.prefs.sample_renames.contains_key("kick"));
            assert!(app.status.contains("already"), "{}", app.status);

            // Back to its own name takes the overlay away.
            app.renaming_bank = Some("hats".into());
            app.rename_bank("hats");
            assert!(app.prefs.sample_renames.is_empty());
        }

        #[test]
        fn renaming_an_imported_bank_twice_keeps_the_browser_on_it() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            let imported = directory.path().join("samples");
            let bank = imported.join("hats");
            std::fs::create_dir_all(&bank).expect("bank folder");
            std::fs::write(bank.join("closed.wav"), b"fixture").expect("sample");
            std::fs::write(bank.join("open.wav"), b"fixture").expect("sample");
            let spec = imported.display().to_string();
            app.add_sample_sources(std::slice::from_ref(&spec));
            app.settle_sample_sources();

            let sounds = app.worker.library().expect("library").catalogue();
            app.reference_panel = Some(ReferencePanel::samples(&app.reference, sounds));
            select_bank_row(app.reference_panel.as_mut().unwrap(), "hats");
            assert!(app.reference_panel.as_mut().unwrap().expand());

            for alias in ["vox1", "vox2"] {
                app.renaming_bank = Some("hats".into());
                app.renaming_bank_source = Some(spec.clone());
                app.rename_bank(alias);
                app.settle_sample_sources();
                assert_eq!(
                    app.reference_panel
                        .as_ref()
                        .and_then(ReferencePanel::selected_bank)
                        .map(|(name, _)| name),
                    Some(alias.to_owned()),
                    "the selected bank follows each alias"
                );
                assert!(
                    app.reference_panel.as_ref().unwrap().expanded.is_some(),
                    "the bank stays expanded"
                );
            }
        }

        #[test]
        fn dropping_an_imported_folder_again_refreshes_its_banks() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            let imported = directory.path().join("samples");
            let first = imported.join("bank1");
            std::fs::create_dir_all(&first).expect("first bank");
            std::fs::write(first.join("a.wav"), b"fixture").expect("first sample");
            let spec = imported.display().to_string();

            assert_eq!(app.add_sample_sources(std::slice::from_ref(&spec)), 1);
            app.settle_sample_sources();
            assert!(app.worker.library().unwrap().knows("bank1"));

            let added = imported.join("bank1 - Copy");
            std::fs::create_dir_all(&added).expect("new bank");
            std::fs::write(added.join("a.wav"), b"fixture").expect("new sample");
            app.status = "quiet".into();
            assert_eq!(app.add_sample_sources(std::slice::from_ref(&spec)), 0);
            app.settle_sample_sources();

            assert_eq!(app.prefs.sample_sources.len(), 1, "the source stays unique");
            assert!(app.worker.library().unwrap().knows("bank1 - Copy"));
            assert!(
                !app.status.contains("already imported"),
                "the repeat is a refresh, not a duplicate warning: {}",
                app.status
            );
        }

        /// Alt+R on the samples browser opens the prompt that `r` opens on
        /// Settings > Samples, for the bank under the cursor. A heading does
        /// nothing, and a pinned bank has no such action. The chord uses the
        /// full press routing, the path that a merged ConPTY `ESC r` arrives
        /// on, so a degraded Alt cannot type the letter into the score.
        #[test]
        fn alt_r_from_the_browser_opens_the_rename_prompt_or_refuses() {
            use rustel_runtime::samples::{SoundCategory, SoundEntry, SoundOrigin};
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            let sounds = vec![
                SoundEntry {
                    name: "bd".into(),
                    variants: 1,
                    variant_names: Vec::new(),
                    origin: SoundOrigin::Global,
                    category: SoundCategory::Drums,
                    location: None,
                    import: None,
                },
                SoundEntry {
                    name: "hh".into(),
                    variants: 1,
                    variant_names: Vec::new(),
                    origin: SoundOrigin::Default,
                    category: SoundCategory::Percussion,
                    location: None,
                    import: None,
                },
            ];
            app.reference_panel = Some(ReferencePanel::samples(&app.reference, sounds));
            app.focus = Focus::Panel(PanelKind::Reference);
            // Two kinds of sound group into a heading each, folded at first.
            assert_eq!(app.reference_panel.as_ref().unwrap().sound_selected, 0);

            // On the heading: nothing happens.
            crate::app::tests::support::press(&mut app, KeyCode::Char('r'), KeyModifiers::ALT);
            assert_eq!(app.renaming_bank, None, "a heading is not a bank");
            assert!(app.set_prompt.is_none());

            // Open "drums" and land on `bd`, an imported bank.
            if let Some(panel) = app.reference_panel.as_mut() {
                panel
                    .open_categories
                    .insert(crate::reference::SoundSection::Kind(SoundCategory::Drums));
                panel.sound_selected = 1;
            }
            crate::app::tests::support::press(&mut app, KeyCode::Char('r'), KeyModifiers::ALT);
            assert_eq!(app.renaming_bank.as_deref(), Some("bd"));
            let (prompt, picker) = app.set_prompt.as_ref().expect("the rename prompt opened");
            assert_eq!(*prompt, SetPrompt::RenameBank);
            assert_eq!(picker.path, "bd", "the bank under the cursor is offered");

            // Back out, then try the pinned bank: the action is unavailable.
            app.set_prompt = None;
            app.renaming_bank = None;
            if let Some(panel) = app.reference_panel.as_mut() {
                panel
                    .open_categories
                    .insert(crate::reference::SoundSection::Kind(
                        SoundCategory::Percussion,
                    ));
                let row = panel
                    .sound_rows()
                    .iter()
                    .position(
                        |row| matches!(row, SoundRow::Bank(index) if panel.sounds[*index].name == "hh"),
                    )
                    .expect("hh is listed");
                panel.sound_selected = row;
            }
            crate::app::tests::support::press(&mut app, KeyCode::Char('r'), KeyModifiers::ALT);
            assert_eq!(app.renaming_bank, None, "a pinned bank cannot be aliased");
            assert!(app.set_prompt.is_none());
        }

        /// The first Alt+T on a local take arms the trim, and a second Alt+T on
        /// the same row cuts the silence. A move off the row, even one that
        /// cannot move the cursor, disarms it.
        #[test]
        fn alt_t_arms_then_trims_and_any_other_key_disarms_it() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());

            let takes = directory.path().join("takes");
            std::fs::create_dir_all(&takes).expect("takes dir");
            let path = takes.join("solo.wav");
            let mut writer = crate::wav::TakeWriter::start(path.clone(), 48_000).expect("writer");
            assert_eq!(writer.push_padded(Vec::new(), 2_000), Ok(true));
            let loud: Vec<f32> = std::iter::repeat_n([0.6_f32, -0.6_f32], 1_000)
                .flatten()
                .collect();
            assert!(writer.push(loud));
            assert_eq!(writer.push_padded(Vec::new(), 2_000), Ok(true));
            let status = writer.finish();
            assert_eq!(status.error, None);

            app.add_sample_sources(&[takes.display().to_string()]);
            app.settle_sample_sources();
            assert!(
                app.worker
                    .library()
                    .is_some_and(|library| library.knows("takes")),
                "the fixture registered in its folder's bank"
            );

            let sounds = app.worker.library().expect("library").catalogue();
            app.reference_panel = Some(ReferencePanel::samples(&app.reference, sounds));
            app.focus = Focus::Panel(PanelKind::Reference);
            select_bank_row(app.reference_panel.as_mut().unwrap(), "takes");
            let row = app.reference_panel.as_ref().unwrap().sound_selected;

            let before = std::fs::read(&path).expect("fixture bytes");

            // Ctrl+Shift+T is deliberately not a second spelling. It must not
            // arm a destructive action that Settings and the footer only call
            // Alt+T.
            app.handle_reference_key_with_modifiers(KeyCode::Char('t'), true, true, false)
                .expect("ctrl+shift+t");
            assert_eq!(
                app.reference_panel.as_ref().unwrap().confirm_trim,
                None,
                "Ctrl+Shift+T must not arm trim"
            );
            assert_eq!(std::fs::read(&path).unwrap(), before, "armed, not cut");

            app.handle_reference_key(KeyCode::Char('t'), false, true)
                .expect("alt+t");
            assert!(
                app.status
                    .contains(crate::keybinds::shortcut_label("Alt+T again trims").as_ref()),
                "{}",
                app.status
            );
            assert_eq!(
                app.reference_panel.as_ref().unwrap().confirm_trim,
                Some(row)
            );

            // Any other key disarms it. The real catalogue can carry other
            // rows, so put the cursor back on `takes` before the next press.
            app.handle_reference_key(KeyCode::Down, false, false)
                .expect("down");
            assert_eq!(app.reference_panel.as_ref().unwrap().confirm_trim, None);
            select_bank_row(app.reference_panel.as_mut().unwrap(), "takes");

            // So this press is a FIRST press again, not the one that cuts.
            app.handle_reference_key(KeyCode::Char('t'), false, true)
                .expect("alt+t");
            assert!(
                app.status
                    .contains(crate::keybinds::shortcut_label("Alt+T again trims").as_ref()),
                "a moved-off arm starts over: {}",
                app.status
            );
            assert_eq!(std::fs::read(&path).unwrap(), before, "still untouched");

            // The second press in a row is the one that cuts.
            app.handle_reference_key(KeyCode::Char('t'), false, true)
                .expect("alt+t again");
            assert!(app.status.contains("trimming"), "{}", app.status);
            assert_eq!(app.reference_panel.as_ref().unwrap().confirm_trim, None);
            let deadline = Instant::now() + Duration::from_secs(5);
            while !app.trim_jobs.is_empty() && Instant::now() < deadline {
                std::thread::yield_now();
                app.poll_trim_jobs();
            }
            assert!(app.trim_jobs.is_empty(), "trim worker did not answer");
            assert!(app.status.contains("trimmed"), "{}", app.status);
            let after = std::fs::read(&path).expect("trimmed bytes");
            assert!(
                after.len() < before.len(),
                "the padded silence either side was cut"
            );
        }

        /// A downloaded pack sample is shared and re-fetchable, not this
        /// player's local file to edit: Alt+T refuses it outright, on both
        /// presses, and the bytes on disk never move.
        #[test]
        fn alt_t_refuses_a_downloaded_pack_sample() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());

            let cache = rustel_runtime::samples::sample_cache_dir().join("alt-t-refusal-fixture");
            std::fs::create_dir_all(&cache).expect("cache subfolder");
            let path = cache.join("kick.wav");
            let mut writer = crate::wav::TakeWriter::start(path.clone(), 48_000).expect("writer");
            let loud: Vec<f32> = std::iter::repeat_n([0.5_f32, -0.5_f32], 200)
                .flatten()
                .collect();
            assert!(writer.push(loud));
            let status = writer.finish();
            assert_eq!(status.error, None);

            app.add_sample_sources(&[cache.display().to_string()]);
            app.settle_sample_sources();
            assert!(
                app.worker
                    .library()
                    .is_some_and(|library| library.knows("alt-t-refusal-fixture")),
                "the fixture registered in its folder's bank"
            );

            let sounds = app.worker.library().expect("library").catalogue();
            app.reference_panel = Some(ReferencePanel::samples(&app.reference, sounds));
            app.focus = Focus::Panel(PanelKind::Reference);
            select_bank_row(
                app.reference_panel.as_mut().unwrap(),
                "alt-t-refusal-fixture",
            );

            let before = std::fs::read(&path).expect("fixture bytes");
            app.handle_reference_key(KeyCode::Char('t'), false, true)
                .expect("alt+t");
            assert!(
                app.status.contains("downloaded pack sample"),
                "{}",
                app.status
            );
            assert_eq!(
                app.reference_panel.as_ref().unwrap().confirm_trim,
                None,
                "a refusal never arms"
            );
            // A second press would be the one that cuts on an armed row; here
            // nothing was ever armed to answer to.
            app.handle_reference_key(KeyCode::Char('t'), false, true)
                .expect("alt+t again");
            assert!(
                app.status.contains("downloaded pack sample"),
                "{}",
                app.status
            );
            let after = std::fs::read(&path).expect("fixture bytes");
            assert_eq!(before, after, "a refused row is never rewritten");

            let _ = std::fs::remove_dir_all(&cache);
        }

        /// A set's `sessions/` and `exports/` hold what the studio writes into
        /// the set, so no door makes either a sample source: a drop, the import
        /// prompt and the edit prompt each refuse, with a status line and an
        /// info line in the log. That holds for another set's folders too, and
        /// for a take dropped from inside one.
        #[test]
        fn a_sets_own_output_folders_are_never_imported() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            let other_set = tempfile::tempdir().expect("another set");
            std::fs::write(other_set.path().join("song.strudel"), "$: s(\"bd\")").expect("score");
            let sessions = app.scenes.sessions_directory();
            let exports = directory
                .path()
                .join(rustel_runtime::product::EXPORT_DIRECTORY_NAME);
            let other_sessions = other_set
                .path()
                .join(rustel_runtime::product::SESSIONS_DIRECTORY_NAME);
            for folder in [&sessions, &exports, &other_sessions] {
                std::fs::create_dir_all(folder).expect("output folder");
                std::fs::write(folder.join("take.wav"), b"fixture").expect("a take");
            }
            let refusals = |app: &App| {
                app.log
                    .lines()
                    .iter()
                    .filter(|line| {
                        line.level == LogLevel::Info && line.text.contains("not imported")
                    })
                    .count()
            };
            let warnings = app.log.unseen();

            app.accept_drop(std::slice::from_ref(&sessions));
            assert!(app.prefs.sample_sources.is_empty(), "the drop was refused");
            assert!(app.status.contains("not imported"), "{}", app.status);
            assert_eq!(refusals(&app), 1);

            app.accept_drop(&[exports.join("take.wav")]);
            assert!(app.prefs.sample_sources.is_empty(), "a take's folder too");
            assert_eq!(refusals(&app), 2);

            app.add_sample_source(&other_sessions.display().to_string());
            assert!(app.prefs.sample_sources.is_empty(), "the import prompt");
            assert!(app.status.contains("not imported"), "{}", app.status);
            assert_eq!(refusals(&app), 3);

            let kit = directory.path().join("kit");
            std::fs::create_dir_all(&kit).expect("kit");
            std::fs::write(kit.join("kick.wav"), b"fixture").expect("sample");
            let kit = kit.display().to_string();
            app.add_sample_source(&kit);
            app.open_set_prompt(SetPrompt::EditSampleSource(0));
            if let Some((_, picker)) = app.set_prompt.as_mut() {
                picker.offer(&sessions.display().to_string());
            }
            assert!(app.handle_set_prompt_key(KeyCode::Enter, false, false));
            assert_eq!(
                app.prefs.sample_sources[0].spec, kit,
                "the edit prompt keeps the source it had"
            );
            assert!(app.status.contains("not imported"), "{}", app.status);
            assert_eq!(refusals(&app), 4);
            assert_eq!(app.log.unseen(), warnings, "a refusal is not a warning");
        }

        fn midi_fixture(directory: &std::path::Path, end: u64) -> std::path::PathBuf {
            let path = directory.join("song.mid");
            let bytes = rustel_smf::Timeline {
                ppq: 480,
                tempo_micros_per_quarter: 500_000,
                meter: rustel_smf::Meter::FOUR_FOUR,
                tracks: vec![rustel_smf::Track {
                    name: "lead".into(),
                    channel: 1,
                    messages: vec![
                        rustel_smf::Message {
                            tick: 0,
                            kind: rustel_smf::MessageKind::NoteOn {
                                note: 60,
                                velocity: 100,
                            },
                        },
                        rustel_smf::Message {
                            tick: end,
                            kind: rustel_smf::MessageKind::NoteOff { note: 60 },
                        },
                    ],
                }],
            }
            .to_bytes()
            .expect("MIDI bytes");
            std::fs::write(&path, bytes).expect("fixture");
            path
        }

        #[test]
        fn dropping_a_midi_file_creates_a_scene_and_leaves_the_file_alone() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            let midi = midi_fixture(directory.path(), 3_840);
            let bytes = std::fs::read(&midi).expect("fixture");
            let before = app.scenes.scores().count();

            app.accept_drop(std::slice::from_ref(&midi));

            assert_eq!(app.scenes.scores().count(), before + 1);
            assert!(app.status.contains("imported song.mid"), "{}", app.status);
            assert_eq!(std::fs::read(&midi).expect("fixture"), bytes);
            let scene = app.scenes.current();
            assert_ne!(scene.path, midi);
            assert!(scene.path.exists());
            assert!(scene.editor.source().contains("note("));
            assert!(scene.editor.source().contains(".slow(2)"));
            assert!(scene.rewind, "a two-bar import starts at its own zero");
        }

        #[test]
        fn a_malformed_midi_drop_creates_nothing() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            let bad = directory.path().join("bad.mid");
            std::fs::write(&bad, b"nope").expect("fixture");
            let before = app.scenes.scores().count();

            app.accept_drop(std::slice::from_ref(&bad));

            assert_eq!(app.scenes.scores().count(), before);
            assert!(app.status.contains("could not import"), "{}", app.status);
            assert!(
                app.log
                    .lines()
                    .iter()
                    .any(|line| line.level == LogLevel::Error)
            );
        }

        #[test]
        fn a_midi_drop_in_mini_mode_is_refused() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.options.mini = true;
            let midi = midi_fixture(directory.path(), 480);
            let before = app.scenes.scores().count();

            app.accept_drop(std::slice::from_ref(&midi));

            assert_eq!(app.scenes.scores().count(), before);
            assert!(app.status.contains("JavaScript mode"), "{}", app.status);
        }
    }
}
