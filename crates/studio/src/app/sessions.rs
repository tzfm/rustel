//! Starting and rotating the set's replay tape without changing its music.

use super::*;

fn timestamp_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0)
}

/// Keep the suffix and never replace another tape, including one created
/// between checking the name and committing it. The recorder's open file
/// remains attached to these same bytes throughout the rename.
fn rename_tape(old: &std::path::Path, stem: &str) -> Result<PathBuf, String> {
    let stem = stem.trim();
    if stem.is_empty()
        || stem.len() > 120
        || !stem
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
    {
        return Err("use letters, numbers, spaces, _ or -; leave out the extension".into());
    }
    let extension = old.extension().ok_or("session has no extension")?;
    let mut new = old.with_file_name(stem);
    new.set_extension(extension);
    if new == old {
        return Ok(new);
    }
    let metadata =
        std::fs::symlink_metadata(old).map_err(|error| format!("cannot read session: {error}"))?;
    if !metadata.file_type().is_file() {
        return Err("only regular session files can be renamed".into());
    }
    std::fs::hard_link(old, &new).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            "a session with that name already exists".into()
        } else {
            format!("cannot rename session: {error}")
        }
    })?;
    if let Err(error) = std::fs::remove_file(old) {
        let _ = std::fs::remove_file(&new);
        return Err(format!("cannot rename session: {error}"));
    }
    Ok(new)
}

impl App {
    fn create_tape(&self, path: PathBuf) -> Result<SessionRecorder, String> {
        let options = self
            .options
            .recording
            .as_ref()
            .ok_or("recording is disabled")?;
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        }
        let baseline_cps = (self.options.session.cps != SessionConfig::default().cps)
            .then_some(self.options.session.cps);
        SessionRecorder::create_unique(path, options.mode, baseline_cps)
    }

    pub(super) fn new_session(&mut self) {
        self.new_session_at(timestamp_seconds());
    }

    pub(super) fn new_session_at(&mut self, seconds: i64) {
        if self.options.recording.is_none() {
            self.status = "session recording is disabled for this run".into();
            self.dirty_frame = true;
            return;
        }
        let path = self
            .sessions_directory()
            .join(default_session_filename(seconds, None));
        let prepared = self.create_tape(path).and_then(|mut recorder| {
            // The sounding revision may differ from every open editor. Its
            // current slider targets belong in the first block too, so this
            // tape does not depend on a block in the previous tape.
            let seeded = (|| {
                if let Some(source) = self.sounding_tape_source(false) {
                    recorder.try_record_save_via(
                        0.0,
                        SaveStatus::Installed,
                        &source,
                        None,
                        Some("session"),
                    )?;
                }
                recorder.try_record_control(
                    0.0,
                    serde_json::json!({ "fader_db": f64::from(self.master.gain_db()) }),
                )
            })();
            if let Err(error) = seeded {
                let path = recorder.path().to_path_buf();
                drop(recorder);
                let _ = std::fs::remove_file(path);
                return Err(error);
            }
            Ok(recorder)
        });
        let recorder = match prepared {
            Ok(recorder) => recorder,
            Err(error) => {
                self.log.push(LogLevel::Error, "session", error);
                self.status = if self.recorder.is_some() {
                    "new session failed - the current tape is still recording; see the log"
                } else {
                    "new session failed - see the log"
                }
                .into();
                self.dirty_frame = true;
                return;
            }
        };
        // Only a completely prepared tape can replace the old recorder.
        // Finish pending gestures on the old tape before handing it off.
        self.flush_tape_gestures();
        let path = recorder.path().to_path_buf();
        self.recorder = Some(recorder);
        self.recorder_started = Instant::now();
        // Open tabs keep their edits when a recorder leaves their tape.
        // Reloading here would replace timing that could not yet be saved
        // while that tape was still being recorded.
        self.refresh_set_panel(Some(path.clone()));
        self.status = format!(
            "new session - {}",
            status_file_path(&path, self.ui_settings.show_full_paths),
        );
        self.remember_status_file(path);
        self.dirty_frame = true;
    }

    fn flush_tape_gestures(&mut self) {
        if self.slider_tape_due.take().is_some() {
            self.record_sounding_sliders();
        }
        if self.fader_tape_due.take().is_some() {
            self.record_control(
                serde_json::json!({ "fader_db": f64::from(self.master.gain_db()) }),
            );
        }
    }

    /// Finish this set's tape. The next set opens its own on evaluation.
    pub(super) fn finish_set_recording(&mut self) {
        self.flush_tape_gestures();
        self.recorder = None;
        // An evaluation already queued for the old set may answer after
        // the switch. It can still play, but must not open or append to
        // the new set's tape.
        for sent in self
            .pending_evaluation
            .iter_mut()
            .chain(self.inflight.values_mut())
        {
            sent.record = false;
        }
    }

    /// The installed source, including live slider targets. Reading a tape
    /// or auditioning a snippet never becomes a fresh recording of itself.
    pub(super) fn sounding_tape_source(&self, require_slider_change: bool) -> Option<String> {
        #[cfg(feature = "hydra")]
        if self.snippet_preview.is_some() {
            return None;
        }
        if self.replay_view.is_some()
            || self
                .audible_scene
                .and_then(|id| self.scenes.get(id))
                .is_some_and(|scene| scene.is_replay())
        {
            return None;
        }
        let installed = self.installed_revision.as_deref()?;
        let source = self
            .evaluation_revisions
            .get(installed)
            .map(|entry| entry.source)
            .or_else(|| {
                // Failed evaluations can evict the still-sounding score
                // from the bounded request cache. The installed layout's
                // source remains available independently of those requests.
                self.evaluated_source
                    .as_ref()
                    .filter(|source| source_revision(source) == installed)
                    .cloned()
            })?;
        let mut edits = self
            .visual
            .layout()
            .into_iter()
            .flat_map(|layout| &layout.sliders)
            .filter_map(|slider| {
                let current = self.sliders.iter().find(|span| span.id == slider.id)?;
                (current.value != slider.value)
                    .then(|| (slider.from, slider.to, slider::format_value(current.value)))
            })
            .collect::<Vec<_>>();
        if require_slider_change && edits.is_empty() {
            return None;
        }
        edits.sort_by_key(|(from, _, _)| std::cmp::Reverse(*from));
        let mut text = source.to_string();
        for (from, to, literal) in edits {
            if from <= to
                && to <= text.len()
                && text.is_char_boundary(from)
                && text.is_char_boundary(to)
            {
                text.replace_range(from..to, &literal);
            }
        }
        Some(text)
    }

    /// Lazily open the first tape, and preserve existing files even when a
    /// run starts twice in the same second or names an existing tape.
    pub(super) fn record_save(
        &mut self,
        status: SaveStatus,
        source: &str,
        error: Option<&str>,
        via: Option<&str>,
    ) {
        if self.recorder.is_none() {
            let Some(options) = self.options.recording.as_ref() else {
                return;
            };
            let path = options.file.clone().unwrap_or_else(|| {
                self.sessions_directory()
                    .join(default_session_filename(timestamp_seconds(), None))
            });
            match self.create_tape(path) {
                Ok(recorder) => {
                    self.recorder = Some(recorder);
                    self.recorder_started = Instant::now();
                    self.refresh_set_panel(None);
                }
                Err(error) => {
                    self.options.recording = None;
                    self.log.push(LogLevel::Error, "session", error);
                    self.set_error(
                        ErrorOwner::Save,
                        "not recording this set - see the log".into(),
                    );
                    return;
                }
            }
        }
        let at = self.recorder_started.elapsed().as_secs_f64();
        let Some(recorder) = self.recorder.as_mut() else {
            return;
        };
        recorder.record_save_via(at, status, source, error, via);
        let path = recorder.path().to_path_buf();
        for tab in self.replays.values_mut().filter(|tab| tab.path == path) {
            let _ = tab.reload();
        }
    }

    /// Sliders and the fader move continuously; the tape gets the value
    /// they settle on.
    pub(super) fn settle_tape_gestures(&mut self, now: Instant) {
        if self.slider_tape_due.is_some_and(|due| now >= due) {
            self.slider_tape_due = None;
            self.record_sounding_sliders();
        }
        if self.fader_tape_due.is_some_and(|due| now >= due) {
            self.fader_tape_due = None;
            let db = self.master.gain_db();
            self.record_control(
                serde_json::json!({ "fader_db": (f64::from(db) * 10.0).round() / 10.0 }),
            );
        }
    }

    /// The score as it sounds with the sliders where they are now: the
    /// evaluated text with each slider's literal replaced, so the tape
    /// replays the drag as an update to the same score.
    fn record_sounding_sliders(&mut self) {
        if self.recorder.is_none() {
            return;
        }
        let Some(text) = self.sounding_tape_source(true) else {
            return;
        };
        let at = self.recorder_started.elapsed().as_secs_f64();
        if let Some(recorder) = self.recorder.as_mut() {
            recorder.record_save_via(at, SaveStatus::Installed, &text, None, Some("slider"));
        }
    }

    /// A gesture line on the tape, if a tape is open. Nothing opens one.
    pub(super) fn record_control(&mut self, fields: serde_json::Value) {
        let at = self.recorder_started.elapsed().as_secs_f64();
        if let Some(recorder) = self.recorder.as_mut() {
            recorder.record_control(at, fields);
        }
    }

    /// Where the set's tapes and takes go: the set's own sessions folder,
    /// or the folder of a tape named on the command line.
    pub(super) fn sessions_directory(&self) -> PathBuf {
        self.options
            .recording
            .as_ref()
            .and_then(|options| options.file.as_ref())
            .and_then(|file| file.parent())
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| self.scenes.sessions_directory())
    }

    /// The tape being written right now, if one is.
    pub(super) fn recording_path(&self) -> Option<PathBuf> {
        self.recorder
            .as_ref()
            .map(|recorder| recorder.path().to_path_buf())
    }

    /// The same file actions as Samples, on the selected tape or open replay.
    pub(super) fn handle_session_file_key(&mut self, code: KeyCode) -> bool {
        if !matches!(code, KeyCode::Char('r' | 'R' | 'o' | 'O')) {
            return false;
        }
        let path = if self.focus == Focus::Panel(PanelKind::Set) {
            self.set_panel
                .as_ref()
                .filter(|panel| panel.deleting.is_none())
                .and_then(|panel| panel.selected_tape())
                .map(|tape| tape.path.clone())
        } else {
            self.current_replay()
                .and_then(|id| self.replays.get(&id))
                .map(|tab| tab.path.clone())
        };
        let Some(path) = path else {
            return false;
        };
        if matches!(code, KeyCode::Char('o' | 'O')) {
            self.reveal_target(super::super::reveal::RevealTarget::File(path));
        } else {
            let stem = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            self.open_set_prompt(SetPrompt::RenameSession);
            self.renaming_session = Some(path);
            if let Some((_, picker)) = self.set_prompt.as_mut() {
                picker.offer(&stem);
            }
        }
        true
    }

    pub(super) fn rename_session_file(&mut self, stem: &str) {
        let Some(old) = self.renaming_session.clone() else {
            return;
        };
        let result = rename_tape(&old, stem);
        let new = match result {
            Ok(path) => path,
            Err(error) => {
                if let Some((_, picker)) = self.set_prompt.as_mut() {
                    picker.error = Some(error);
                }
                self.dirty_frame = true;
                return;
            }
        };
        if let Some(recorder) = self.recorder.as_mut().filter(|r| r.path() == old) {
            recorder.relocate(new.clone());
        }
        if let Some(options) = self.options.recording.as_mut()
            && options.file.as_ref() == Some(&old)
        {
            options.file = Some(new.clone());
        }
        // Only the address changes. Reloading would discard unsaved timing
        // or text, and replacing the tab would interrupt a running replay.
        for block in self
            .evaluation_revisions
            .entries
            .values_mut()
            .filter_map(|entry| entry.replay_block.as_mut())
            .chain(self.visual_replay_block.iter_mut())
        {
            if block.path == old {
                block.path = new.clone();
            }
        }
        for tab in self.replays.values_mut().filter(|tab| tab.path == old) {
            tab.path = new.clone();
        }
        for scene in self.scenes.scenes_mut() {
            if scene.is_replay() && scene.path == old {
                scene.path = new.clone();
            }
        }
        self.close_set_prompt();
        self.refresh_set_panel(Some(new.clone()));
        self.status = format!(
            "session renamed - {}",
            status_file_path(&new, self.ui_settings.show_full_paths),
        );
        self.remember_status_file(new);
        self.dirty_frame = true;
    }
}

/// A slider or fader that stops moving for this long is written to the
/// tape, once, at its final value.
pub(super) const TAPE_GESTURE_SETTLE: Duration = Duration::from_millis(300);
