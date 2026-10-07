//! The manifest worker's line: one job is worked on and the rest wait
//! first in, first out, whoever put them there. A producer never waits on
//! the line. It is bounded by jobs and by the text they carry, so a score
//! cannot park unbounded host work behind a slow fetch; past the bound a
//! job is refused.

use std::collections::VecDeque;
use std::sync::mpsc::TrySendError;
use std::sync::{Arc, Condvar, Mutex};

use super::{MAX_MANIFEST_EFFECT_BYTES, MAX_QUEUED_PRELOAD_BYTES, ManifestJob, ManifestWork};

/// Jobs the line holds behind the one being worked on.
pub(super) const MANIFEST_QUEUE_JOBS: usize = 64;

/// Text the waiting jobs may carry between them: two of the largest batches
/// one evaluation may send.
const MANIFEST_QUEUE_BYTES: usize = 2 * (MAX_MANIFEST_EFFECT_BYTES + MAX_QUEUED_PRELOAD_BYTES);

pub(super) struct ManifestQueue {
    line: Mutex<Line>,
    arrived: Condvar,
}

#[derive(Default)]
struct Line {
    /// Each job with the text it carries.
    jobs: VecDeque<(Box<ManifestJob>, usize)>,
    bytes: usize,
    closed: bool,
}

/// The worker's end of the line. Dropping it closes the line and lets go of
/// every job still waiting, so a caller waiting on one hears that the loader
/// stopped.
pub(super) struct ManifestJobs(Arc<ManifestQueue>);

/// A line, and the worker's end of it.
pub(super) fn manifest_queue() -> (Arc<ManifestQueue>, ManifestJobs) {
    let queue = Arc::new(ManifestQueue {
        line: Mutex::new(Line::default()),
        arrived: Condvar::new(),
    });
    (Arc::clone(&queue), ManifestJobs(queue))
}

impl ManifestQueue {
    /// Put `job` at the back of the line. A full or closed line hands it
    /// back.
    pub(super) fn push(&self, job: Box<ManifestJob>) -> Result<(), TrySendError<Box<ManifestJob>>> {
        let bytes = text_bytes(&job.work);
        let mut line = self.line.lock().expect("sample manifest line");
        if line.closed {
            return Err(TrySendError::Disconnected(job));
        }
        if line.jobs.len() >= MANIFEST_QUEUE_JOBS
            || line.bytes.saturating_add(bytes) > MANIFEST_QUEUE_BYTES
        {
            return Err(TrySendError::Full(job));
        }
        line.bytes += bytes;
        line.jobs.push_back((job, bytes));
        self.arrived.notify_one();
        Ok(())
    }

    /// Take no more jobs. The worker still takes the ones already waiting.
    pub(super) fn close(&self) {
        self.line.lock().expect("sample manifest line").closed = true;
        self.arrived.notify_all();
    }

    fn take(line: &mut Line) -> Option<Box<ManifestJob>> {
        let (job, bytes) = line.jobs.pop_front()?;
        line.bytes -= bytes;
        Some(job)
    }
}

impl ManifestJobs {
    /// The next job, waiting for one; `None` once the line is closed and
    /// empty.
    pub(super) fn recv(&self) -> Option<Box<ManifestJob>> {
        let mut line = self.0.line.lock().expect("sample manifest line");
        loop {
            if let Some(job) = ManifestQueue::take(&mut line) {
                return Some(job);
            }
            if line.closed {
                return None;
            }
            line = self.0.arrived.wait(line).expect("sample manifest line");
        }
    }

    /// The jobs waiting now, in the order the worker would take them.
    #[cfg(test)]
    pub(super) fn try_iter(&self) -> impl Iterator<Item = Box<ManifestJob>> + '_ {
        std::iter::from_fn(|| {
            ManifestQueue::take(&mut self.0.line.lock().expect("sample manifest line"))
        })
    }
}

impl Drop for ManifestJobs {
    fn drop(&mut self) {
        let waiting = match self.0.line.lock() {
            Ok(mut line) => {
                line.closed = true;
                line.bytes = 0;
                std::mem::take(&mut line.jobs)
            }
            Err(_) => return,
        };
        drop(waiting);
    }
}

/// The text a job carries: what a score or the player named. The pinned
/// default list is the host's own, and fixed.
fn text_bytes(work: &ManifestWork) -> usize {
    match work {
        ManifestWork::Defaults { .. } => 0,
        ManifestWork::Folders { specs, .. } => specs.iter().map(String::len).sum(),
        ManifestWork::Custom {
            effects, preloads, ..
        } => effects
            .iter()
            .map(|(map, base)| map.len() + base.as_ref().map_or(0, String::len))
            .chain(preloads.iter().map(String::len))
            .sum(),
    }
}
