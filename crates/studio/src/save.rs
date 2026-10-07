//! Bounded, latest-wins persistence for the full-screen studio.
//!
//! The terminal thread only publishes immutable snapshots and drains tagged
//! results. All filesystem work, including durability barriers, stays on the
//! worker so a slow disk cannot stall input or animation. One worker serves
//! every scene of a set: requests carry their own path, a newer snapshot of
//! the same file replaces the one still waiting, and snapshots of different
//! files queue behind each other in order.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};

use super::editor::Revision;
use rustel_runtime::ui_events::source_revision;

#[derive(Clone, Debug)]
pub(super) struct SaveRequest {
    pub request_id: u64,
    pub path: PathBuf,
    pub editor_revision: Revision,
    pub source_revision: String,
    source: Arc<str>,
}

impl SaveRequest {
    pub fn new(
        request_id: u64,
        path: PathBuf,
        editor_revision: Revision,
        source: Arc<str>,
    ) -> Self {
        let source_revision = source_revision(&source);
        Self {
            request_id,
            path,
            editor_revision,
            source_revision,
            source,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SaveResult {
    pub request_id: u64,
    pub path: PathBuf,
    pub editor_revision: Revision,
    pub source_revision: String,
    pub result: Result<(), String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SaveSubmit {
    /// A snapshot of the same file waiting behind the in-flight write was
    /// superseded. The write already in progress is never cancelled
    /// mid-rename.
    pub replaced_pending: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SaveSubmitError;

impl std::fmt::Display for SaveSubmitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("studio save worker is shutting down")
    }
}

impl std::error::Error for SaveSubmitError {}

#[derive(Default)]
struct SaveState {
    /// At most one waiting snapshot per file, in submission order.
    pending: VecDeque<SaveRequest>,
    /// The write in progress now - the snapshot popped from `pending` but
    /// not yet answered into `completions`. Tracked so an observer can
    /// tell "every submitted snapshot has reached the disk" from "one is
    /// still being written".
    in_flight: bool,
    /// Every completion since the last drain, oldest first.
    completions: Vec<SaveResult>,
    shutting_down: bool,
}

struct Shared {
    state: Mutex<SaveState>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, SaveState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}

/// A single save thread with one in-flight write and one replaceable
/// pending snapshot per file. Memory use therefore stays bounded by the
/// number of scenes under key repeat or a burst of explicit Save commands.
pub(super) struct SaveWorker {
    shared: Arc<Shared>,
    join: Option<JoinHandle<()>>,
}

impl SaveWorker {
    pub fn spawn() -> io::Result<Self> {
        Self::spawn_with_operation(|path, source| {
            atomic_write(path, source).map_err(|error| error.to_string())
        })
    }

    fn spawn_with_operation<F>(operation: F) -> io::Result<Self>
    where
        F: FnMut(&Path, &str) -> Result<(), String> + Send + 'static,
    {
        let shared = Arc::new(Shared {
            state: Mutex::new(SaveState::default()),
            wake: Condvar::new(),
        });
        let worker_shared = Arc::clone(&shared);
        let join = thread::Builder::new()
            .name("studio-save".into())
            .spawn(move || run_worker(&worker_shared, operation))?;
        Ok(Self {
            shared,
            join: Some(join),
        })
    }

    pub fn submit(&self, request: SaveRequest) -> Result<SaveSubmit, SaveSubmitError> {
        let mut state = self.shared.lock();
        if state.shutting_down {
            return Err(SaveSubmitError);
        }
        let replaced_pending = match state
            .pending
            .iter_mut()
            .find(|waiting| waiting.path == request.path)
        {
            Some(waiting) => {
                *waiting = request;
                true
            }
            None => {
                state.pending.push_back(request);
                false
            }
        };
        drop(state);
        self.shared.wake.notify_one();
        Ok(SaveSubmit { replaced_pending })
    }

    /// Whether nothing is waiting and nothing is being written: every
    /// snapshot submitted so far has reached the disk, or failed trying.
    /// A caller that wants the files to read what the screen showed can
    /// wait on this.
    pub fn idle(&self) -> bool {
        let state = self.shared.lock();
        state.pending.is_empty() && !state.in_flight
    }

    /// Every completion since the previous drain, oldest first. Nothing is
    /// collapsed: a later failure must not erase knowledge of the source
    /// that really reached disk, and one scene's result must not hide
    /// another's.
    pub fn take_completions(&self) -> Vec<SaveResult> {
        std::mem::take(&mut self.shared.lock().completions)
    }

    /// Finish the current write and every pending write, then join. This is
    /// intentionally called only after the alternate screen is restored.
    pub fn shutdown(&mut self) {
        {
            let mut state = self.shared.lock();
            state.shutting_down = true;
        }
        self.shared.wake.notify_one();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for SaveWorker {
    fn drop(&mut self) {
        if self.join.is_some() {
            self.shutdown();
        }
    }
}

fn run_worker<F>(shared: &Shared, mut operation: F)
where
    F: FnMut(&Path, &str) -> Result<(), String>,
{
    loop {
        let request = {
            let mut state = shared.lock();
            loop {
                if let Some(request) = state.pending.pop_front() {
                    state.in_flight = true;
                    break Some(request);
                }
                if state.shutting_down {
                    break None;
                }
                state = shared
                    .wake
                    .wait(state)
                    .unwrap_or_else(|error| error.into_inner());
            }
        };
        let Some(request) = request else {
            break;
        };

        let result = operation(&request.path, &request.source);
        let completed = SaveResult {
            request_id: request.request_id,
            path: request.path,
            editor_revision: request.editor_revision,
            source_revision: request.source_revision,
            result,
        };
        let mut state = shared.lock();
        state.in_flight = false;
        state.completions.push(completed);
    }
}

/// Replace a file's contents without ever leaving a half-written one behind.
///
/// The save worker writes scores through this, and so do the studio's own
/// small files - the set's project file and the global prebake - which are
/// written straight from the interface thread because they are tiny and are
/// only written on an explicit gesture.
pub(super) fn atomic_write(path: &Path, source: &str) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let permissions = std::fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions());
    let mut pending = tempfile::NamedTempFile::new_in(parent)?;
    if let Some(permissions) = permissions {
        pending.as_file_mut().set_permissions(permissions)?;
    }
    pending.write_all(source.as_bytes())?;
    pending.flush()?;
    pending.as_file_mut().sync_all()?;
    pending.persist(path).map_err(|error| error.error)?;
    // Opening a directory is not supported by every target. Where it is, run
    // the rename durability barrier on this worker as part of the save.
    if let Ok(directory) = std::fs::File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::sync_channel;

    use super::*;

    fn request(id: u64, revision: u64, source: &str) -> SaveRequest {
        request_for(id, "unused", revision, source)
    }

    fn request_for(id: u64, path: &str, revision: u64, source: &str) -> SaveRequest {
        SaveRequest::new(
            id,
            PathBuf::from(path),
            Revision(revision),
            Arc::<str>::from(source),
        )
    }

    #[test]
    fn atomic_save_replaces_a_score_without_truncating_first() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("score.strudel");
        std::fs::write(&path, "old score").unwrap();

        atomic_write(&path, "new score").unwrap();

        assert_eq!(std::fs::read_to_string(path).unwrap(), "new score");
    }

    #[test]
    fn burst_coalesces_to_the_newest_waiting_snapshot_per_file() {
        let (started_tx, started_rx) = sync_channel(0);
        let (release_tx, release_rx) = sync_channel(0);
        let writes = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&writes);
        let mut first = true;
        let mut worker = SaveWorker::spawn_with_operation(move |path, source| {
            observed
                .lock()
                .unwrap()
                .push((path.to_path_buf(), source.to_owned()));
            if first {
                first = false;
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            }
            Ok(())
        })
        .unwrap();

        assert!(
            !worker
                .submit(request_for(1, "a", 10, "one"))
                .unwrap()
                .replaced_pending
        );
        started_rx.recv().unwrap();
        assert!(
            !worker
                .submit(request_for(2, "a", 20, "two"))
                .unwrap()
                .replaced_pending
        );
        // Another scene's snapshot queues rather than replacing this one.
        assert!(
            !worker
                .submit(request_for(4, "b", 5, "other"))
                .unwrap()
                .replaced_pending
        );
        assert!(
            worker
                .submit(request_for(3, "a", 30, "three"))
                .unwrap()
                .replaced_pending
        );
        release_tx.send(()).unwrap();
        worker.shutdown();

        assert_eq!(
            &*writes.lock().unwrap(),
            &[
                (PathBuf::from("a"), "one".to_owned()),
                (PathBuf::from("a"), "three".to_owned()),
                (PathBuf::from("b"), "other".to_owned()),
            ]
        );
        let completions = worker.take_completions();
        let ids = completions
            .iter()
            .map(|result| result.request_id)
            .collect::<Vec<_>>();
        assert_eq!(ids, [1, 3, 4]);
        let third = &completions[1];
        assert_eq!(third.editor_revision, Revision(30));
        assert_eq!(third.source_revision, source_revision("three"));
        assert_eq!(third.path, PathBuf::from("a"));
        assert_eq!(third.result, Ok(()));
    }

    #[test]
    fn failure_result_keeps_revision_and_exact_source_hash() {
        let mut worker = SaveWorker::spawn_with_operation(|_, _| Err("disk full".into())).unwrap();
        worker.submit(request(7, 42, "exact bytes")).unwrap();
        worker.shutdown();

        let completions = worker.take_completions();
        assert_eq!(completions.len(), 1);
        let result = &completions[0];
        assert_eq!(result.request_id, 7);
        assert_eq!(result.editor_revision, Revision(42));
        assert_eq!(result.source_revision, source_revision("exact bytes"));
        assert_eq!(result.result, Err("disk full".into()));
    }

    #[test]
    fn newer_failure_cannot_hide_the_source_that_last_reached_disk() {
        let (started_tx, started_rx) = sync_channel(0);
        let (release_tx, release_rx) = sync_channel(0);
        let mut first = true;
        let mut worker = SaveWorker::spawn_with_operation(move |_, source| {
            if first {
                first = false;
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            }
            if source == "persisted" {
                Ok(())
            } else {
                Err("newer write failed".into())
            }
        })
        .unwrap();

        worker.submit(request(10, 10, "persisted")).unwrap();
        started_rx.recv().unwrap();
        worker.submit(request(11, 11, "failed")).unwrap();
        release_tx.send(()).unwrap();
        worker.shutdown();

        let completions = worker.take_completions();
        assert_eq!(completions.len(), 2);
        assert_eq!(completions[0].request_id, 10);
        assert_eq!(completions[0].source_revision, source_revision("persisted"));
        assert_eq!(completions[0].result, Ok(()));
        assert_eq!(completions[1].request_id, 11);
        assert_eq!(completions[1].source_revision, source_revision("failed"));
        assert_eq!(completions[1].result, Err("newer write failed".into()));
    }
}
