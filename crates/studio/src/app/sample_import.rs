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
        if imported == 0 && opened == 0 && !refused.is_empty() {
            self.status = format!(
                "nothing to import from {} - audio, a folder of audio, or a score",
                refused.join(", ")
            );
        } else if !refused.is_empty() {
            self.status = format!("{} skipped: not audio or a score", refused.join(", "));
        }
        if !refused.is_empty() {
            self.log.push(
                LogLevel::Warn,
                "drop",
                format!(
                    "not audio, a folder of audio, or a score: {}",
                    refused.join(", ")
                ),
            );
        }
        self.log.push(
            LogLevel::Debug,
            "drop",
            format!(
                "{imported} source(s) imported · {opened} score(s) opened · {} refused",
                refused.len()
            ),
        );
        self.dirty_frame = true;
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
