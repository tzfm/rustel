//! Reloading scores when another program saves their files.
//!
//! `sync` follows the score in the active tab. `sync & eval` follows every
//! score in the set, opens the saved file's tab, and evaluates it.
//!
//! One `metadata` call per watched file ten times a second, and one read
//! only after the file's size or modification time moved and stayed put for
//! a poll. A stat is the cheapest check a score file allows on every
//! system, and an OS change notification would need a thread and a platform
//! backend for files this small.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::*;
use crate::bounded_file::{self, MAX_DOCUMENT_BYTES};
use crate::scenes::SCENE_EXTENSION;

/// How often a watched file is stat-ed. Ten checks a second costs
/// microseconds of work, and a read happens only when the stamp moves.
pub(super) const POLL: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileStamp {
    modified: Option<SystemTime>,
    len: u64,
}

struct WatchedFile {
    stamp: FileStamp,
    /// A stamp moved and is waiting for the next poll to agree. Editors
    /// write in several steps, and reading the first step reads half a
    /// score.
    settling: bool,
}

/// The files the watch last saw, and whether the first look is done.
#[derive(Default)]
pub(super) struct ExternalWatch {
    /// The mode these files were gathered for. A switch starts over, so
    /// entering eval mode baselines the whole set instead of playing it.
    mode: ExternalChanges,
    /// The set folder these files were gathered from. Another set starts
    /// over the same way: its scores are a baseline, not saves.
    directory: PathBuf,
    files: HashMap<PathBuf, WatchedFile>,
    /// The first poll after the mode turns on only records what is there.
    /// Otherwise every score in the set would open and play at once.
    started: bool,
}

/// Why one open scene's file did or did not reload.
enum ReloadOutcome {
    /// The file's text was adopted into the editor.
    Adopted,
    /// The file holds what the editor already saved, or could not be read.
    Refused,
    /// A save of the studio's own is still in flight; try again once it
    /// lands.
    SavePending,
}

impl App {
    /// Follow the files Settings asks for.
    ///
    /// `sync` watches the active tab. `sync & eval` also watches every
    /// `.strudel` file in the set folder, so a save to a score that has no
    /// tab yet opens one and plays it, and a save to a tab in the
    /// background brings that tab forward.
    pub(super) fn poll_external_changes(&mut self, now: Instant) {
        let mode = self.ui_settings.external_changes;
        if mode == ExternalChanges::Off {
            self.external_watch = None;
            return;
        }
        if now.duration_since(self.external_watch_checked_at) < POLL {
            return;
        }
        self.external_watch_checked_at = now;

        let paths = self.watched_paths(mode);
        let directory = self.scenes.directory();
        let watch = self.external_watch.get_or_insert_default();
        if watch.mode != mode || watch.directory != directory {
            watch.mode = mode;
            watch.directory = directory.to_path_buf();
            watch.files.clear();
            watch.started = false;
        }
        let started = watch.started;
        watch.files.retain(|path, _| paths.contains(path));
        let mut settled = Vec::new();
        for path in paths {
            let Ok(metadata) = std::fs::metadata(&path) else {
                // A file mid-rename is briefly absent. The next poll finds
                // it.
                watch.files.remove(&path);
                continue;
            };
            let stamp = FileStamp {
                modified: metadata.modified().ok(),
                len: metadata.len(),
            };
            match watch.files.get_mut(&path) {
                None => {
                    // A tab opened in sync mode may have changed while it
                    // was away. The first look at the whole set in eval
                    // mode is only a baseline.
                    let settling = mode == ExternalChanges::Sync || started;
                    watch.files.insert(path, WatchedFile { stamp, settling });
                }
                Some(file) if file.stamp != stamp => {
                    file.stamp = stamp;
                    file.settling = true;
                }
                Some(file) if file.settling => {
                    file.settling = false;
                    settled.push((path, stamp.modified));
                }
                Some(_) => {}
            }
        }
        watch.started = true;
        // Oldest first, so the newest change is the one left in front.
        settled.sort_by_key(|(_, modified)| *modified);
        for (path, _) in settled {
            self.apply_watched_change(&path, now);
        }
    }

    /// Take a file as the watch's baseline, so a write of the studio's own
    /// is not read back as another program's save. A file with a tab has
    /// its saved text to tell. An import, and the file of a tab that
    /// closes after its saves, have no tab to tell it.
    pub(super) fn note_own_write(&mut self, path: &Path) {
        let Some(watch) = self.external_watch.as_mut() else {
            return;
        };
        if let Ok(metadata) = std::fs::metadata(path) {
            let stamp = FileStamp {
                modified: metadata.modified().ok(),
                len: metadata.len(),
            };
            watch.files.insert(
                path.to_path_buf(),
                WatchedFile {
                    stamp,
                    settling: false,
                },
            );
        }
    }

    /// The files this mode follows: one score for `sync`, every score of
    /// the set for `sync & eval`.
    fn watched_paths(&self, mode: ExternalChanges) -> Vec<PathBuf> {
        match mode {
            ExternalChanges::Off => Vec::new(),
            ExternalChanges::Sync => {
                let scene = self.scenes.current();
                if scene.is_score() {
                    vec![scene.path.clone()]
                } else {
                    Vec::new()
                }
            }
            ExternalChanges::SyncAndEvaluate => {
                let mut paths: Vec<PathBuf> = self
                    .scenes
                    .scores()
                    .map(|scene| scene.path.clone())
                    .collect();
                if let Ok(entries) = std::fs::read_dir(self.scenes.directory()) {
                    for entry in entries.filter_map(Result::ok) {
                        let path = entry.path();
                        if path
                            .extension()
                            .is_some_and(|extension| extension == SCENE_EXTENSION)
                            && path.is_file()
                        {
                            paths.push(path);
                        }
                    }
                }
                paths.sort();
                paths.dedup();
                paths
            }
        }
    }

    /// Bring a changed file to the front and take its text. In eval mode
    /// the text then plays, as an update would.
    fn apply_watched_change(&mut self, path: &Path, now: Instant) {
        let mode = self.ui_settings.external_changes;
        let open = self
            .scenes
            .scores()
            .find(|scene| scene.path == path)
            .map(|scene| scene.id);
        let scene_id = match open {
            // Read and adopt before selecting, so a save of our own never
            // steals a tab back from the one the reader is on.
            Some(id) => {
                match self.reload_open_scene(id, path) {
                    ReloadOutcome::Adopted => {}
                    ReloadOutcome::SavePending => {
                        // The stamp was already advanced, so nothing would
                        // trigger a re-read once the save lands. Keep the
                        // file settling: the next polls retry, and an
                        // external edit that landed under the pending save
                        // is still adopted instead of being overwritten by
                        // the next studio save.
                        if let Some(watch) = self.external_watch.as_mut()
                            && let Some(file) = watch.files.get_mut(path)
                        {
                            file.settling = true;
                        }
                        self.dirty_frame = true;
                        return;
                    }
                    ReloadOutcome::Refused => {
                        self.dirty_frame = true;
                        return;
                    }
                }
                if mode == ExternalChanges::SyncAndEvaluate
                    && let Some(index) = self.scenes.index_of(id)
                {
                    self.select_scene(index);
                }
                id
            }
            None => {
                // `sync` follows only the active tab, so a file with no
                // tab arrives here only in eval mode.
                if self.replay_view.is_some() {
                    self.close_replay();
                }
                let id = match self.scenes.open_file(path) {
                    Ok(id) => id,
                    Err(error) => {
                        self.log.push(
                            LogLevel::Warn,
                            "file",
                            format!("{}: {error}", path.display()),
                        );
                        return;
                    }
                };
                self.land_on_current_scene();
                self.persist_manifest();
                id
            }
        };
        let name = self.scenes.current().name();
        self.note_external_change(scene_id, now);
        // The file changed on disk, so its row's fields and saved time did
        // too. Keep the selection the reader has, not the file that moved.
        let keep = self.set_panel_selection();
        self.refresh_set_panel(keep);
        self.set_notice(format!("reloaded {name} from disk"));
        if mode == ExternalChanges::SyncAndEvaluate {
            self.apply_external_change();
        }
    }

    /// Read one open scene's file into the editor.
    fn reload_open_scene(&mut self, scene_id: SceneId, path: &Path) -> ReloadOutcome {
        let Ok(source) = bounded_file::read_to_string(path, MAX_DOCUMENT_BYTES) else {
            return ReloadOutcome::Refused;
        };
        let Some(scene) = self.scenes.get_mut(scene_id) else {
            return ReloadOutcome::Refused;
        };
        // A save of our own is not an external change. The file may hold
        // text older than the caret, and adopting it would drop what was
        // typed after the save was queued.
        if scene.latest_save.is_some() {
            return ReloadOutcome::SavePending;
        }
        if source_revision(&source) == scene.saved_source_revision {
            return ReloadOutcome::Refused;
        }
        scene.adopt_source(&source);
        ReloadOutcome::Adopted
    }

    /// What an edit made outside the studio owes the studio.
    fn note_external_change(&mut self, scene_id: SceneId, now: Instant) {
        self.last_edit_at = now;
        self.lint_pending = true;
        // The pause that was owed belonged to text this reload replaced.
        // Whether the new text plays is the watch mode's answer alone.
        self.auto_evaluate_due = false;
        self.readiness_due = Some((scene_id, now + super::readiness::READINESS_SETTLE));
        self.marks_muted = Some(scene_id);
        self.invalidate_maps();
        // The sounding score's inline painters keep their rows: the edit
        // above moved their anchors with it.
        if Some(scene_id) == self.audible_scene
            && let Some(revision) = self.visual_revision
        {
            self.install_virtual_rows(revision);
        }
        self.dirty_frame = true;
    }
}
