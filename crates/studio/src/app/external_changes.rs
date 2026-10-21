//! Reloading the open score when another program saves its file.
//!
//! One `metadata` call every half second while the setting is on, and one
//! read only after the file's size or modification time moved and stayed
//! put for a poll. A stat is the cheapest check a score file allows on
//! every system, and an OS change notification would need a thread and a
//! platform backend for a file this small.

use std::path::Path;
use std::time::SystemTime;

use super::*;
use crate::bounded_file::{self, MAX_DOCUMENT_BYTES};

/// How often the open score's file is stat-ed.
const POLL: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileStamp {
    modified: Option<SystemTime>,
    len: u64,
}

/// The file the watch last saw, and whether that sighting has settled.
pub(super) struct ExternalWatch {
    scene: SceneId,
    path: PathBuf,
    stamp: FileStamp,
    /// A stamp moved and is waiting for the next poll to agree. Editors
    /// write in several steps, and reading the first step reads half a
    /// score.
    settling: bool,
}

impl App {
    /// Follow the open score's file when Settings asks for it.
    ///
    /// The current scene alone is watched: the file in front of the reader
    /// is the file whose changes they are looking at. A switch to another
    /// tab baselines that tab's file and reads it once, so an edit made to
    /// a tab while it was in the background lands when it comes forward.
    pub(super) fn poll_external_changes(&mut self, now: Instant) {
        if self.ui_settings.external_changes == ExternalChanges::Off {
            self.external_watch = None;
            return;
        }
        if now.duration_since(self.external_watch_checked_at) < POLL {
            return;
        }
        self.external_watch_checked_at = now;
        let scene = self.scenes.current();
        if !scene.is_score() {
            self.external_watch = None;
            return;
        }
        let path = scene.path.clone();
        let Ok(metadata) = std::fs::metadata(&path) else {
            // A file mid-rename is briefly absent. The next poll finds it.
            self.external_watch = None;
            return;
        };
        let stamp = FileStamp {
            modified: metadata.modified().ok(),
            len: metadata.len(),
        };
        let Some(watch) = self.external_watch.as_mut() else {
            self.external_watch = Some(ExternalWatch {
                scene: scene.id,
                path,
                stamp,
                settling: true,
            });
            return;
        };
        if watch.scene != scene.id || watch.path != path {
            *watch = ExternalWatch {
                scene: scene.id,
                path,
                stamp,
                settling: true,
            };
            return;
        }
        if watch.stamp != stamp {
            watch.stamp = stamp;
            watch.settling = true;
            return;
        }
        if !watch.settling {
            return;
        }
        watch.settling = false;
        let (scene_id, path) = (watch.scene, watch.path.clone());
        self.reload_external_change(scene_id, &path, now);
    }

    /// Read the file and put its text in front of the reader. The watch
    /// mode says whether the text also plays.
    fn reload_external_change(&mut self, scene_id: SceneId, path: &Path, now: Instant) {
        let Ok(source) = bounded_file::read_to_string(path, MAX_DOCUMENT_BYTES) else {
            return;
        };
        let Some(scene) = self.scenes.get_mut(scene_id) else {
            return;
        };
        // A save of our own is not an external change. The file may hold
        // text older than the caret, and adopting it would drop what was
        // typed after the save was queued.
        if scene.latest_save.is_some() || source_revision(&source) == scene.saved_source_revision {
            return;
        }
        if !scene.adopt_source(&source) {
            // The editor already holds the text. The file may still have
            // caught up with it, which clears the unsaved mark.
            self.dirty_frame = true;
            return;
        }
        let name = scene.name();
        self.last_edit_at = now;
        self.lint_pending = true;
        // The pause that was owed belonged to text this reload replaced.
        // Whether the new text plays is the watch mode's answer alone.
        self.auto_evaluate_due = false;
        self.readiness_due = Some((scene_id, now + super::readiness::READINESS_SETTLE));
        self.marks_muted = Some(scene_id);
        self.invalidate_maps();
        self.dirty_frame = true;
        self.set_notice(format!("reloaded {name} from disk"));
        if self.ui_settings.external_changes == ExternalChanges::SyncAndEvaluate {
            self.apply_external_change();
        }
    }
}
