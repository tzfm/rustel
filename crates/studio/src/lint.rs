//! The studio's lint thread.
//!
//! The checker itself lives in [`rustel_runtime::lint`], shared with the watched CLI
//! set and `rustel check`. This module runs it off the interface thread: the
//! studio hands over the text once typing has paused, the thread keeps only
//! the newest request so a fast typist never queues work, and the result
//! comes back tagged with the revision it was checked at, so the underline
//! follows the text through later edits like any other mark.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use rustel_runtime::lint::{Diagnostic, LintContext, lint_with};
use rustel_runtime::samples::SampleLibrary;

use super::editor::Revision;
use super::scenes::SceneId;

#[derive(Clone)]
pub struct LintRequest {
    pub scene: SceneId,
    pub revision: Revision,
    pub source: Arc<str>,
    /// The whole buffer is mini-notation (`--mini`).
    pub mini: bool,
    /// The engine's sample library, for sound and bank names.
    pub library: Option<Arc<SampleLibrary>>,
    /// What ran before this text, and whether this is setup or a score.
    /// Shared rather than rebuilt; setup changes and verified input-device
    /// changes replace it, while a request rides every pause in typing.
    pub context: Arc<LintContext>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LintResult {
    pub scene: SceneId,
    pub revision: Revision,
    /// Hardware facts used by the check; a result from an earlier input
    /// device must not replace findings for the current device.
    pub input_channels: Option<usize>,
    pub diagnostics: Vec<Diagnostic>,
}

struct Slot {
    request: Mutex<(Option<LintRequest>, bool)>,
    wake: Condvar,
}

/// The lint thread. One request waits at a time, and a newer one replaces
/// it. Results arrive on a channel the interface drains each turn.
pub struct Linter {
    slot: Arc<Slot>,
    results: Receiver<LintResult>,
    join: Option<JoinHandle<()>>,
}

impl Linter {
    pub fn spawn() -> std::io::Result<Self> {
        let slot = Arc::new(Slot {
            request: Mutex::new((None, false)),
            wake: Condvar::new(),
        });
        let (sender, results) = channel();
        let worker = Arc::clone(&slot);
        let join = thread::Builder::new()
            .name("studio-lint".into())
            // Linting builds a runtime to read the names a score may call.
            .stack_size(rustel_runtime::QUERY_WORKER_STACK_BYTES)
            .spawn(move || run(&worker, sender))?;
        Ok(Self {
            slot,
            results,
            join: Some(join),
        })
    }

    /// Hand the newest text to the thread; anything still waiting is
    /// superseded, so a burst of edits costs one check.
    pub fn submit(&self, request: LintRequest) {
        let mut guard = self.slot.request.lock().unwrap_or_else(|e| e.into_inner());
        guard.0 = Some(request);
        drop(guard);
        self.slot.wake.notify_one();
    }

    pub fn try_recv(&self) -> Option<LintResult> {
        self.results.try_recv().ok()
    }
}

impl Drop for Linter {
    fn drop(&mut self) {
        {
            let mut guard = self.slot.request.lock().unwrap_or_else(|e| e.into_inner());
            guard.1 = true;
        }
        self.slot.wake.notify_one();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn run(slot: &Slot, sender: Sender<LintResult>) {
    loop {
        let request = {
            let mut guard = slot.request.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(request) = guard.0.take() {
                    break Some(request);
                }
                if guard.1 {
                    break None;
                }
                guard = slot.wake.wait(guard).unwrap_or_else(|e| e.into_inner());
            }
        };
        let Some(request) = request else {
            break;
        };
        let diagnostics = lint_with(
            &request.source,
            request.mini,
            request.library.as_deref(),
            &request.context,
        );
        if sender
            .send(LintResult {
                scene: request.scene,
                revision: request.revision,
                input_channels: request.context.input_channels,
                diagnostics,
            })
            .is_err()
        {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_thread_keeps_the_newest_request_and_tags_its_result() {
        let linter = Linter::spawn().unwrap();
        linter.submit(LintRequest {
            scene: SceneId(1),
            revision: Revision(3),
            source: Arc::from("$: s(\"bd\").lpf("),
            mini: false,
            library: None,
            context: Arc::default(),
        });
        linter.submit(LintRequest {
            scene: SceneId(1),
            revision: Revision(4),
            source: Arc::from("$: s(\"bd\")"),
            mini: false,
            library: None,
            context: Arc::new(LintContext {
                input_channels: Some(2),
                ..LintContext::default()
            }),
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut results = Vec::new();
        while std::time::Instant::now() < deadline {
            if let Some(result) = linter.try_recv() {
                results.push(result);
                if results.iter().any(|result| result.revision == Revision(4)) {
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let newest = results
            .iter()
            .find(|result| result.revision == Revision(4))
            .expect("the newest request was checked");
        assert!(newest.diagnostics.is_empty());
        assert_eq!(newest.scene, SceneId(1));
        assert_eq!(newest.input_channels, Some(2));
    }
}
