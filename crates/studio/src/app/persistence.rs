//! Getting the studio's state onto disk: handing dirty scenes to the background
//! save worker, applying its results back to each scene (dirty flag, status
//! line, save errors), and waiting for or shutting down that worker before a
//! set switch or quit. It also holds the debounced writes of the preferences
//! and the set manifest (with the current pane layout), so a held key or a drag
//! doesn't write the file on every step.

use super::*;

/// How long the preferences wait before they are written.
///
/// Long enough that a held arrow on an opacity row, or a walk down the theme
/// list, is one write rather than twenty - each of which is a directory
/// creation, a temporary file, a rename and an fsync sitting between a
/// keypress and the next frame.
pub(super) const PREFS_DEBOUNCE: Duration = Duration::from_millis(600);
/// Avoid hammering an unavailable config directory while keeping an owed
/// preference write alive until access returns.
const PREFS_RETRY_DELAY: Duration = Duration::from_secs(5);

impl App {
    /// Ask for the preferences to be written, once the reader stops.
    ///
    /// The write is a directory creation, a temporary file, a rename and an
    /// fsync. The debounce keeps it out of the path between a keypress and
    /// the next frame, on every step of an opacity row and every close of
    /// the theme picker.
    pub(super) fn save_prefs_soon(&mut self) {
        self.prefs_pending = true;
        self.prefs_changed_at = Instant::now();
        self.prefs_retry_at = None;
        self.dirty_frame = true;
    }

    /// The set file is owed a write. Same debounce as the preferences,
    /// and for the same reason: a drag is one gesture however many
    /// reports it arrives in.
    pub(super) fn save_manifest_soon(&mut self) {
        self.manifest_pending = true;
        self.manifest_changed_at = Instant::now();
        self.dirty_frame = true;
    }

    pub(super) fn flush_manifest(&mut self) {
        if !self.manifest_pending {
            return;
        }
        self.manifest_pending = false;
        self.persist_manifest();
    }

    /// Say once where a preferences file that did not read was kept.
    pub(super) fn note_kept_prefs(&mut self) {
        let Some(kept) = crate::prefs::take_kept_aside() else {
            return;
        };
        let name = kept
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.status =
            format!("the settings file did not read - kept as {name}; defaults are loaded");
        self.log
            .push(LogLevel::Warn, "settings", self.status.clone());
        self.dirty_frame = true;
    }

    /// Write the preferences now if a write is owed. Called when the
    /// debounce expires, and on the way out so nothing is lost by quitting
    /// promptly. A failed write stays owed and is tried again after
    /// `PREFS_RETRY_DELAY`.
    pub(super) fn flush_prefs(&mut self) {
        if !self.prefs_pending {
            return;
        }
        match self.prefs.save() {
            Ok(_) => {
                self.prefs_pending = false;
                self.prefs_retry_at = None;
                self.clear_error(ErrorOwner::Preferences);
            }
            Err(error) => {
                self.prefs_retry_at = Some(Instant::now() + PREFS_RETRY_DELAY);
                // Raised once until a write succeeds: the text can name a
                // fresh temporary file on every attempt, and raising it again
                // would log each retry and take the footer from newer errors.
                if self.errors.get(ErrorOwner::Preferences).is_none() {
                    self.set_error(
                        ErrorOwner::Preferences,
                        format!("preferences not saved: {error}"),
                    );
                }
            }
        }
    }

    pub(super) fn persist_manifest(&mut self) {
        // The layout goes in on the way past, so every write carries the
        // panes as they are now rather than as they were when something
        // last thought to tell the set about them.
        self.remember_panes();
        // Any write pays the debt, however it was asked for. Without this
        // a debounced write outlives the set that asked for it: `switch_set`
        // writes the old set here and then replaces `self.scenes`, so the
        // pending flush lands on the new set's file - a write nobody asked
        // for, on a set that may not have earned a file at all.
        self.manifest_pending = false;
        match self.scenes.persist_manifest() {
            Ok(_) => self.clear_error(ErrorOwner::Save),
            Err(error) => self.set_error(
                ErrorOwner::Save,
                format!(
                    "could not write {}: {error}",
                    self.scenes.manifest_path().display()
                ),
            ),
        }
    }

    /// Hand one scene's text to the save worker.
    pub(super) fn write_scene(&mut self, scene_id: SceneId, revision: Revision, source: Arc<str>) {
        let Some(scene) = self.scenes.get_mut(scene_id) else {
            return;
        };
        // A prebake tab has no file of its own; its text goes where its
        // scope keeps it, through `persist_prebake`.
        if !scene.is_score() {
            debug_assert!(false, "a prebake tab was handed to the save worker");
            return;
        }
        let request_id = self.next_save_request_id;
        self.next_save_request_id = self.next_save_request_id.saturating_add(1);
        let path = scene.path.clone();
        let request = SaveRequest::new(request_id, path.clone(), revision, source);
        let pending = PendingSave {
            request_id,
            source_revision: request.source_revision.clone(),
        };
        match self.save_worker.submit(request) {
            Ok(_) => scene.latest_save = Some(pending),
            Err(error) => self.set_error(
                ErrorOwner::Save,
                format!("could not queue a write of {}: {error}", path.display()),
            ),
        }
    }

    /// Write every scene whose text is newer than its file. Quitting and
    /// closing do this rather than asking: the file is the buffer.
    pub(super) fn flush_dirty_scenes(&mut self) {
        let dirty = self
            .scenes
            .scenes()
            .iter()
            .filter(|scene| scene.dirty)
            .map(|scene| {
                (
                    scene.id,
                    scene.prebake(),
                    scene.editor.revision(),
                    Arc::<str>::from(scene.editor.source()),
                )
            })
            .collect::<Vec<_>>();
        for (id, prebake, revision, source) in dirty {
            match prebake {
                // Kept where its scope keeps it, and checked on the way out
                // so a broken setup says so now rather than at the next
                // studio. It is not run: leaving is not an update.
                Some(scope) => {
                    self.persist_prebake(scope, &source);
                    self.check_stored_prebake(scope);
                }
                None => self.write_scene(id, revision, source),
            }
        }
    }

    /// Wait until the save worker has written everything it was given, so
    /// the files on disk read what the screen showed. A set switch asks
    /// for this before the old set is put down: the set is the file, and
    /// a set left must be left whole, not left to a worker that may not
    /// have been scheduled yet. Bounded: a disk that stalls longer than
    /// the wait is answered by the save completions, not by an endless
    /// switch.
    pub(super) fn wait_for_saves(&mut self) {
        for _ in 0..2_000 {
            self.drain_save();
            if self.save_worker.idle() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Write each dirty score and wait for the writes. Returns the name of
    /// a score that is still not on disk: its text is only in its tab, and
    /// the save error says why.
    pub(super) fn score_not_on_disk(&mut self) -> Option<String> {
        self.flush_dirty_scenes();
        self.wait_for_saves();
        self.scenes
            .scenes()
            .iter()
            .find(|scene| scene.unsaved())
            .map(|scene| scene.name())
    }

    pub(super) fn drain_save(&mut self) {
        let _ = self.drain_save_completions();
    }

    /// Route every completed write to its scene. Returns the newest error.
    pub(super) fn drain_save_completions(&mut self) -> Option<String> {
        let mut latest_error = None;
        for result in self.save_worker.take_completions() {
            if let Err(error) = &result.result {
                latest_error = Some(error.clone());
            }
            self.handle_save_result(result);
        }
        latest_error
    }

    pub(super) fn handle_save_result(&mut self, result: SaveResult) {
        let SaveResult {
            request_id,
            path,
            editor_revision,
            source_revision,
            result,
        } = result;
        // A scene renamed while its save was in flight is found by the
        // request rather than the path it no longer has.
        let scene_id = self.scenes.by_path(&path).or_else(|| {
            self.scenes
                .scenes()
                .iter()
                .find(|scene| {
                    scene
                        .latest_save
                        .as_ref()
                        .is_some_and(|pending| pending.request_id == request_id)
                })
                .map(|scene| scene.id)
        });
        let Some(scene_id) = scene_id else {
            return;
        };
        let Some(scene) = self.scenes.get_mut(scene_id) else {
            return;
        };
        let is_latest = scene
            .latest_save
            .as_ref()
            .is_some_and(|pending| pending.request_id == request_id);
        if is_latest {
            scene.latest_save = None;
        }
        let still_pending = scene.latest_save.is_some();
        match result {
            Ok(()) => {
                // A rename can race an in-flight write: the worker persisted
                // to a path this scene no longer owns. Do not clear dirty or
                // claim the current file is saved - finish_rename queues a
                // follow-up write to the new path.
                if scene.path != path {
                    scene.refresh_dirty();
                    self.status = format!(
                        "a save finished at {}, but the scene now lives at {}",
                        status_file_path(&path, self.ui_settings.show_full_paths),
                        status_file_path(&scene.path, self.ui_settings.show_full_paths)
                    );
                    self.dirty_frame = true;
                    return;
                }
                scene.saved_source_revision = source_revision;
                scene.refresh_dirty();
                self.clear_error(ErrorOwner::Save);
                self.status = if still_pending {
                    format!(
                        "saved editor revision {}; a newer save is still pending…",
                        editor_revision.0
                    )
                } else {
                    format!(
                        "saved {}",
                        status_file_path(&path, self.ui_settings.show_full_paths)
                    )
                };
            }
            Err(error) => {
                scene.refresh_dirty();
                self.status = if still_pending {
                    "save failed; a newer edit is still queued…".into()
                } else {
                    "save failed - the editor buffer is unchanged".into()
                };
                self.set_error(
                    ErrorOwner::Save,
                    format!("could not save {}: {error}", path.display()),
                );
            }
        }
        self.dirty_frame = true;
    }

    pub(super) fn shutdown_save_worker(&mut self) -> Option<RuntimeError> {
        self.save_worker.shutdown();
        let shutdown_error = self
            .drain_save_completions()
            .map(|error| RuntimeError::Message(format!("could not save during shutdown: {error}")));
        if let Some(scene) = self
            .scenes
            .scenes()
            .iter()
            .find(|scene| scene.latest_save.is_some())
        {
            return Some(RuntimeError::Message(format!(
                "save worker stopped before {} was persisted",
                scene.path.display()
            )));
        }
        shutdown_error
    }
}
