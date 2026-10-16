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

#[cfg(test)]
mod tests {
    mod sessions {
        //! Tests for sessions.

        use crate::app::evaluation::MAX_EVALUATION_REVISIONS;
        use crate::app::*;
        use crate::editor::Selection;
        use crate::menu::{MenuAction, MenuItemKind};
        use rustel_runtime::session_log::SessionScript;

        fn session_app(directory: &std::path::Path) -> App {
            let mut app = crate::app::tests::support::app_over(directory);
            app.options.recording = Some(RecordingOptions {
                mode: SessionMode::Normal,
                file: None,
            });
            app.frame = Rect::new(0, 0, 140, 40);
            app.capabilities = KeyboardCapabilities::enhanced();
            app
        }

        fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
            app.handle_terminal_event(Event::Key(KeyEvent::new(code, modifiers)))
                .unwrap();
        }

        #[test]
        fn switching_sets_finishes_old_gestures_and_keeps_late_evaluations_off_the_new_tape() {
            let directory = tempfile::tempdir().unwrap();
            let next = tempfile::tempdir().unwrap();
            let mut app = session_app(directory.path());
            app.record_save(SaveStatus::Installed, "s(\"bd\")", None, None);
            let old = app.recording_path().unwrap();
            app.master.set_gain_db(-6.0);
            app.fader_tape_due = Some(Instant::now() + Duration::from_secs(1));
            let request = |request_id| PendingEvaluation {
                request_id,
                scene: app.scenes.current().id,
                editor_revision: Revision(0),
                source: Arc::from("s(\"old\")"),
                launch: Launch::Now,
                rewind: false,
                record: true,
            };
            let pending = request(2);
            app.inflight.insert(1, request(1));
            app.pending_evaluation = Some(pending);
            let set = SceneSet::create_in(next.path(), "new set", "s(\"new\")").unwrap();
            app.switch_set(set, "opened set");
            assert!(app.recorder.is_none(), "the next tape opens lazily");
            assert!(app.fader_tape_due.is_none());
            let old_bytes = std::fs::read_to_string(&old).unwrap();
            let gesture: serde_json::Value =
                serde_json::from_str(old_bytes.lines().last().unwrap()).unwrap();
            assert_eq!(gesture["fader_db"], -6.0, "the old tape gets the gesture");

            // One answer arrives before the new set plays, another afterwards.
            // Neither may create or append to the new set's tape.
            for request_id in [1, 2] {
                if request_id == 2 {
                    app.record_save(SaveStatus::Installed, "s(\"new\")", None, None);
                    app.inflight
                        .insert(2, app.pending_evaluation.take().unwrap());
                }
                app.handle_evaluation(EvaluationOutcome {
                    request_id,
                    editor_revision: 0,
                    result: Ok(crate::StudioInstall {
                        generation: request_id,
                        source_revision: "old".into(),
                        pending_cutover: false,
                        answered_repeat: request_id == 2,
                        held: false,
                    }),
                });
                if request_id == 1 {
                    assert!(app.recorder.is_none());
                }
            }
            let new = app.recording_path().unwrap();
            assert!(new.starts_with(app.scenes.sessions_directory()));
            let script = SessionScript::load(&new).unwrap();
            assert_eq!(script.saves.len(), 1);
            assert_eq!(&*script.saves[0].source, "s(\"new\")");
            assert_eq!(std::fs::read_to_string(&old).unwrap(), old_bytes);
        }

        #[test]
        fn switching_sets_respects_disabled_recording_and_an_explicit_destination() {
            for enabled in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let next = tempfile::tempdir().unwrap();
                let mut app = session_app(directory.path());
                let explicit = directory.path().join("custom.rustel-session");
                if enabled {
                    app.options.recording.as_mut().unwrap().file = Some(explicit.clone());
                } else {
                    app.options.recording = None;
                }
                app.record_save(SaveStatus::Installed, "s(\"bd\")", None, None);
                let old_bytes = enabled.then(|| std::fs::read(&explicit).unwrap());
                let set = SceneSet::create_in(next.path(), "new set", "s(\"new\")").unwrap();
                app.switch_set(set, "opened set");
                app.record_save(SaveStatus::Installed, "s(\"new\")", None, None);
                if let Some(old_bytes) = old_bytes {
                    let new = app.recording_path().unwrap();
                    assert_ne!(new, explicit);
                    assert_eq!(new.parent(), explicit.parent());
                    assert_eq!(std::fs::read(&explicit).unwrap(), old_bytes);
                    assert_eq!(
                        &*SessionScript::load(&new).unwrap().saves[0].source,
                        "s(\"new\")"
                    );
                } else {
                    assert!(app.recorder.is_none());
                }
                assert!(!app.scenes.sessions_directory().exists());
            }
        }

        #[test]
        fn rename_shortcut_preserves_a_live_tape_and_open_replay_edits() {
            for focus in [Focus::Panel(PanelKind::Set), Focus::Editor, Focus::Timeline] {
                let directory = tempfile::tempdir().unwrap();
                let mut app = session_app(directory.path());
                app.record_save(SaveStatus::Installed, "s(\"bd\")", None, None);
                app.recorder.as_mut().unwrap().record_save(
                    2.0,
                    SaveStatus::Installed,
                    "s(\"hh\")",
                    None,
                );
                let old = app.recording_path().unwrap();
                app.options.recording.as_mut().unwrap().file = Some(old.clone());
                let bytes = std::fs::read(&old).unwrap();
                app.open_replay_tab(&old);
                let replay = app.scenes.current().id;
                let tab = app.replays.get_mut(&replay).unwrap();
                tab.set_duration(0, 7.0);
                tab.select(1);
                tab.start(1, Instant::now());
                let started = tab.run.unwrap().started;
                let source = app.editor().source();
                app.open_set_panel(true);
                app.refresh_set_panel(Some(old.clone()));
                app.focus = focus;

                press(&mut app, KeyCode::Char('r'), KeyModifiers::ALT);
                let (prompt, picker) = app.set_prompt.as_mut().expect("rename prompt");
                assert_eq!(*prompt, SetPrompt::RenameSession);
                assert_eq!(picker.path, old.file_stem().unwrap().to_string_lossy());
                picker.set_path("Favorite take");
                press(&mut app, KeyCode::Enter, KeyModifiers::NONE);

                let new = old
                    .with_file_name("Favorite take")
                    .with_extension(old.extension().unwrap());
                assert!(!old.exists());
                assert_eq!(std::fs::read(&new).unwrap(), bytes);
                assert!(app.set_prompt.is_none());
                assert_eq!(app.focus, focus);
                assert_eq!(app.recording_path().as_ref(), Some(&new));
                assert_eq!(
                    app.options.recording.as_ref().unwrap().file.as_ref(),
                    Some(&new)
                );
                assert_eq!(app.scenes.current().path, new);
                assert_eq!(app.scenes.current().name(), "Favorite take");
                assert_eq!(app.editor().source(), source);
                let tab = &app.replays[&replay];
                assert_eq!(tab.path, new);
                assert_eq!(tab.duration_of(0), Some(7.0));
                assert!(tab.dirty);
                assert_eq!(tab.selected, 1);
                assert_eq!(tab.run.unwrap().started, started);
                assert_eq!(
                    app.set_panel
                        .as_ref()
                        .unwrap()
                        .selected_tape()
                        .unwrap()
                        .path,
                    new
                );

                // The existing recorder handle must keep appending at its new name.
                app.record_save(SaveStatus::Installed, "s(\"sd\")", None, None);
                let saved = SessionScript::load(&new).unwrap();
                assert_eq!(saved.saves.len(), 3);
                assert_eq!(&*saved.saves[2].source, "s(\"sd\")");
                assert!(!old.exists());
            }
        }

        #[test]
        fn session_rename_rejects_conflicts_and_paths_and_can_be_cancelled() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = session_app(directory.path());
            app.record_save(SaveStatus::Installed, "s(\"bd\")", None, None);
            let old = app.recording_path().unwrap();
            let bytes = std::fs::read(&old).unwrap();
            let taken = old
                .with_file_name("Taken")
                .with_extension(old.extension().unwrap());
            std::fs::write(&taken, b"keep this tape").unwrap();
            app.open_set_panel(true);
            app.refresh_set_panel(Some(old.clone()));
            press(&mut app, KeyCode::Char('r'), KeyModifiers::ALT);
            for name in ["Taken", "../escape", "", "sub/folder"] {
                app.rename_session_file(name);
                assert!(app.set_prompt.as_ref().unwrap().1.error.is_some());
                assert_eq!(std::fs::read(&old).unwrap(), bytes);
                assert_eq!(std::fs::read(&taken).unwrap(), b"keep this tape");
                assert_eq!(app.recording_path().as_ref(), Some(&old));
            }
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.set_prompt.is_none());
            assert!(app.renaming_session.is_none());
            assert_eq!(app.focus, Focus::Panel(PanelKind::Set));

            // Accepting the offered name is a no-op, including the timestamp prefix.
            press(&mut app, KeyCode::Char('r'), KeyModifiers::ALT);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.recording_path().as_ref(), Some(&old));
            assert_eq!(std::fs::read(&old).unwrap(), bytes);
        }

        #[test]
        fn rotating_a_tape_keeps_old_blocks_and_sends_later_saves_to_the_new_file() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = session_app(directory.path());
            app.record_save(SaveStatus::Installed, "s(\"bd\")", None, None);
            let old = app.recording_path().unwrap();
            let bytes = std::fs::read(&old).unwrap();
            app.open_replay_tab(&old);
            let replay = app.scenes.current().id;
            app.open_set_panel(true);
            app.new_session_at(123);
            let new = app.recording_path().unwrap();
            assert_ne!(old, new);
            assert_eq!(std::fs::read(&old).unwrap(), bytes);
            assert!(
                SessionScript::load_for_editing(&new)
                    .unwrap()
                    .saves
                    .is_empty(),
                "a replay is not copied into a new tape"
            );
            assert_eq!(app.scenes.current().id, replay);
            let panel = app.set_panel.as_ref().unwrap();
            assert!(panel.sessions_open);
            assert_eq!(panel.selected_tape().unwrap().path, new);
            assert!(
                panel
                    .tapes
                    .iter()
                    .any(|tape| tape.path == old && !tape.recording)
            );
            assert!(
                panel
                    .tapes
                    .iter()
                    .any(|tape| tape.path == new && tape.recording)
            );
            app.record_save(SaveStatus::Installed, "s(\"hh\")", None, None);
            assert_eq!(app.replays[&replay].events.len(), 1);
            assert_eq!(std::fs::read(&old).unwrap(), bytes);
            assert_eq!(
                &*SessionScript::load(&new).unwrap().saves[0].source,
                "s(\"hh\")"
            );
        }

        #[test]
        fn delete_on_the_live_tape_explains_how_to_finish_it_first() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = session_app(directory.path());
            app.record_save(SaveStatus::Installed, "s(\"bd\")", None, None);
            let old = app.recording_path().unwrap();
            app.open_set_panel(true);
            app.refresh_set_panel(Some(old.clone()));
            assert!(
                app.set_panel
                    .as_ref()
                    .unwrap()
                    .selected_tape()
                    .unwrap()
                    .recording
            );

            press(&mut app, KeyCode::Delete, KeyModifiers::NONE);
            let panel = app.set_panel.as_ref().unwrap();
            assert!(old.exists());
            assert!(panel.deleting.is_none());
            assert_eq!(
                panel.error.as_deref(),
                Some("recording · press n for a new tape")
            );
            assert!(app.status.contains("n starts a new tape"));

            press(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
            let new = app.recording_path().unwrap();
            assert_ne!(old, new);
            app.refresh_set_panel(Some(old.clone()));
            assert!(
                !app.set_panel
                    .as_ref()
                    .unwrap()
                    .selected_tape()
                    .unwrap()
                    .recording
            );
            press(&mut app, KeyCode::Delete, KeyModifiers::NONE);
            assert!(app.set_panel.as_ref().unwrap().deleting.is_some());
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(!old.exists());
            assert!(new.exists());
        }

        #[test]
        fn rapid_new_sessions_never_reuse_or_truncate_a_timestamp_filename() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = session_app(directory.path());
            let mut tapes = Vec::new();
            for index in 0..4 {
                app.new_session_at(123);
                let path = app.recording_path().unwrap();
                assert!(tapes.iter().all(|(previous, _)| previous != &path));
                app.record_save(
                    SaveStatus::Installed,
                    &format!("s(\"bd*{index}\")"),
                    None,
                    None,
                );
                tapes.push((path.clone(), std::fs::read(path).unwrap()));
            }
            for (path, bytes) in tapes {
                assert_eq!(std::fs::read(&path).unwrap(), bytes);
                assert_eq!(SessionScript::load(&path).unwrap().saves.len(), 1);
            }
        }

        #[test]
        fn rotating_a_live_tape_preserves_unsaved_replay_timing() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = session_app(directory.path());
            app.record_save(SaveStatus::Installed, "s(\"bd\")", None, None);
            app.recorder.as_mut().unwrap().record_save(
                2.0,
                SaveStatus::Installed,
                "s(\"hh\")",
                None,
            );
            let old = app.recording_path().unwrap();
            let bytes = std::fs::read(&old).unwrap();
            app.open_replay_tab(&old);
            let replay = app.scenes.current().id;
            let tab = app.replays.get_mut(&replay).unwrap();
            tab.set_duration(0, 7.0);
            tab.select(1);
            tab.start(1, Instant::now());
            let started = tab.run.unwrap().started;
            app.save_replay_tab(replay);
            assert!(app.status.contains("edit lives in the tab"));
            assert_eq!(std::fs::read(&old).unwrap(), bytes);

            app.new_session_at(123);

            let tab = &app.replays[&replay];
            assert_eq!(tab.duration_of(0), Some(7.0));
            assert!(tab.dirty);
            assert_eq!(tab.selected, 1);
            assert_eq!(tab.run.unwrap().started, started);
            assert_eq!(std::fs::read(&old).unwrap(), bytes);
            assert_ne!(app.recording_path().as_ref(), Some(&old));
            // The old tape can now accept the edit the live recorder protected.
            app.save_replay_tab(replay);
            assert!(!app.replays[&replay].dirty);
            let saved = SessionScript::load(&old).unwrap();
            assert_eq!(saved.saves[1].at - saved.saves[0].at, 7.0);
        }

        #[test]
        fn rejected_evaluations_cannot_evict_the_new_sessions_sounding_snapshot() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = session_app(directory.path());
            let source: Arc<str> = Arc::from("$: s(\"bd\")");
            let installed = source_revision(&source);
            let scene = app.scenes.current().id;
            app.evaluation_revisions.remember(
                installed.clone(),
                Revision(0),
                Arc::clone(&source),
                scene,
                None,
            );
            app.handle_control(StudioControlEvent::Layout(
                visual_layout(&source, 0).unwrap(),
            ));
            app.record_save(SaveStatus::Installed, &source, None, None);
            for index in 0..MAX_EVALUATION_REVISIONS {
                let rejected: Arc<str> = Arc::from(format!("s(\"rejected-{index}\""));
                let request_id = index as u64 + 1;
                let revision = Revision(request_id);
                app.evaluation_revisions.remember(
                    source_revision(&rejected),
                    revision,
                    Arc::clone(&rejected),
                    scene,
                    None,
                );
                app.inflight.insert(
                    request_id,
                    PendingEvaluation {
                        request_id,
                        scene,
                        editor_revision: revision,
                        source: rejected,
                        launch: Launch::Now,
                        rewind: false,
                        record: true,
                    },
                );
                app.handle_evaluation(EvaluationOutcome {
                    request_id,
                    editor_revision: revision.0,
                    result: Err(crate::EngineFailure {
                        kind: "compile".into(),
                        message: "Expected `)`".into(),
                        recoverable: true,
                        playback_stopped: false,
                    }),
                });
            }
            assert!(app.evaluation_revisions.get(&installed).is_none());
            assert_eq!(app.installed_revision.as_deref(), Some(installed.as_str()));
            assert_eq!(app.evaluated_source.as_deref(), Some(source.as_ref()));
            let old = app.recording_path().unwrap();
            app.new_session_at(123);
            let new = app.recording_path().unwrap();
            assert_ne!(new, old);
            let script = SessionScript::load(&new).unwrap();
            assert_eq!(script.saves.len(), 1);
            assert_eq!(&*script.saves[0].source, source.as_ref());
            assert_eq!(script.saves[0].at, 0.0);
            assert_eq!(script.saves[0].via.as_deref(), Some("session"));
            assert!(app.errors.visible().unwrap().contains("Expected `)`"));

            // A retained but unrelated layout cannot stand in for the installed
            // score, and stopping must not seed a tape from an earlier layout.
            app.evaluated_source = Some(Arc::from("s(\"stale\")"));
            assert!(app.sounding_tape_source(false).is_none());
            app.evaluated_source = Some(source);
            app.installed_revision = None;
            assert!(app.sounding_tape_source(false).is_none());
        }

        #[test]
        fn new_session_failure_keeps_the_recorder_and_hides_private_paths() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = session_app(directory.path());
            app.record_save(SaveStatus::Installed, "s(\"bd\")", None, None);
            let old = app.recording_path().unwrap();
            let started = app.recorder_started;
            let blocked = directory.path().join("private-parent-file");
            std::fs::write(&blocked, "not a directory").unwrap();
            app.options.recording.as_mut().unwrap().file =
                Some(blocked.join("session.rustel-session"));
            app.new_session_at(123);
            assert_eq!(app.recording_path().as_ref(), Some(&old));
            assert_eq!(app.recorder_started, started);
            assert!(app.options.recording.is_some());
            assert!(app.status.contains("still recording"));
            assert!(!app.status.contains(&directory.path().display().to_string()));
            assert!(
                app.log
                    .lines()
                    .iter()
                    .any(|line| line.text.contains(&blocked.display().to_string()))
            );
            app.record_save(SaveStatus::Installed, "s(\"hh\")", None, None);
            assert_eq!(SessionScript::load(&old).unwrap().saves.len(), 2);
        }

        #[test]
        fn new_session_snapshots_the_installed_slider_values_without_changing_music_or_editors() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = session_app(directory.path());
            let source = "$: s(\"bd\").gain(slider(0.5, 0, 1, 0.01))\n";
            let layout = visual_layout(source, 0).unwrap();
            let hash = layout.ui_layout.source_revision.clone();
            let scene = app.scenes.current().id;
            app.evaluation_revisions.remember(
                hash.clone(),
                Revision(0),
                Arc::from(source),
                scene,
                None,
            );
            app.installed_revision = Some(hash.clone());
            app.visual.install_layout(layout);
            let slider = &app.visual.layout().unwrap().sliders[0];
            app.sliders = vec![SliderSpan {
                id: slider.id.clone(),
                from: slider.from,
                to: slider.to,
                min: 0.0,
                max: 1.0,
                step: 0.01,
                value: 0.73,
            }];
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(2)))
                .unwrap();
            let editor = app.editor().source();
            let caret = app.editor().primary_selection();
            let viewport = app.editor().viewport();
            app.audible_scene = Some(scene);
            app.playing_pane = Some(0);
            app.next_launch = Launch::Quantised { unit_cycles: 4.0 };
            app.pending_recording = Some(73);
            let launch = app.next_launch;
            app.new_session_at(123);
            let script = SessionScript::load(&app.recording_path().unwrap()).unwrap();
            assert_eq!(script.saves.len(), 1);
            assert_eq!(script.saves[0].at, 0.0);
            assert_eq!(&*script.saves[0].source, source.replace("0.5", "0.73"));
            assert_eq!(app.editor().source(), editor);
            assert_eq!(app.editor().primary_selection(), caret);
            assert_eq!(app.editor().viewport(), viewport);
            assert_eq!(app.installed_revision, Some(hash));
            assert_eq!(app.audible_scene, Some(scene));
            assert_eq!(app.playing_pane, Some(0));
            assert_eq!(app.next_launch, launch);
            assert_eq!(app.pending_recording, Some(73));
            assert!(app.pending_evaluation.is_none() && app.inflight.is_empty());
        }

        #[test]
        fn new_session_respects_no_record_and_exposes_the_file_menu_action() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = session_app(directory.path());
            app.options.recording = None;
            app.open_set_panel(true);
            press(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
            assert!(app.recorder.is_none());
            assert!(app.status.contains("disabled"));
            assert!(!app.sessions_directory().exists());
            let menu_enabled = |app: &App| {
                app.menus()
                    .into_iter()
                    .flat_map(|menu| menu.items)
                    .find(|item| item.kind == MenuItemKind::Action(MenuAction::NewSession))
                    .unwrap()
                    .enabled
            };
            assert!(!menu_enabled(&app));
            app.options.recording = Some(RecordingOptions {
                mode: SessionMode::Normal,
                file: None,
            });
            assert!(menu_enabled(&app));
            app.dispatch_menu_action(MenuAction::NewSession).unwrap();
            assert!(app.recorder.is_some());
        }

        #[test]
        fn session_shortcut_is_plain_n_in_the_focused_panel_and_respects_input_owners() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = session_app(directory.path());
            app.open_set_panel(false);
            app.focus = Focus::Editor;
            press(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
            assert!(app.recorder.is_none(), "editor N types into the score");
            app.focus = Focus::Panel(PanelKind::Set);
            for (code, modifiers, kind) in [
                (KeyCode::Char('n'), KeyModifiers::NONE, KeyEventKind::Repeat),
                (
                    KeyCode::Char('n'),
                    KeyModifiers::NONE,
                    KeyEventKind::Release,
                ),
                (KeyCode::Char('n'), KeyModifiers::ALT, KeyEventKind::Press),
                (KeyCode::Char('N'), KeyModifiers::SHIFT, KeyEventKind::Press),
            ] {
                app.handle_terminal_event(Event::Key(KeyEvent::new_with_kind(
                    code, modifiers, kind,
                )))
                .unwrap();
                assert!(app.recorder.is_none());
                app.focus = Focus::Panel(PanelKind::Set);
            }
            app.help = Some(HelpState::default());
            press(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
            assert!(app.recorder.is_none());
            app.help = None;
            app.begin_rename();
            press(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
            assert!(app.recorder.is_none());
            app.strip_mode = SceneStripMode::Idle;
            app.open_set_prompt(SetPrompt::OpenSet);
            press(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
            assert!(app.recorder.is_none());
            app.set_prompt = None;
            app.focus = Focus::Panel(PanelKind::Set);
            press(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
            assert!(app.recorder.is_some());
        }

        #[test]
        fn lazy_recording_keeps_an_existing_explicit_file_and_chooses_a_unique_tape() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = session_app(directory.path());
            let existing = directory.path().join("idea.rustel-session");
            std::fs::write(&existing, "keep this tape").unwrap();
            app.options.recording.as_mut().unwrap().file = Some(existing.clone());
            app.record_save(SaveStatus::Installed, "s(\"bd\")", None, None);
            assert_ne!(app.recording_path().as_ref(), Some(&existing));
            assert_eq!(std::fs::read_to_string(existing).unwrap(), "keep this tape");
            assert_eq!(
                SessionScript::load(&app.recording_path().unwrap())
                    .unwrap()
                    .saves
                    .len(),
                1
            );
        }
    }
}
