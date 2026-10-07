//! A ready-to-display catalogue, refreshed away from the terminal thread.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::{Arc, mpsc};

use rustel_runtime::samples::{SampleLibrary, SoundEntry, SourceState};

#[derive(Default, PartialEq, Eq)]
pub(super) struct Snapshot {
    pub sounds: Vec<SoundEntry>,
    pub sources: HashMap<String, SourceState>,
}

impl Snapshot {
    fn read(library: &SampleLibrary) -> Self {
        Self {
            sounds: library.catalogue(),
            sources: library.browser_source_states(),
        }
    }

    pub fn source_state(&self, spec: &str) -> SourceState {
        self.sources
            .get(spec)
            .or_else(|| {
                serde_json::to_string(spec)
                    .ok()
                    .and_then(|quoted| self.sources.get(&quoted))
            })
            .cloned()
            .unwrap_or(SourceState::Loading)
    }
}

pub(super) struct Catalogue {
    snapshot: RefCell<Arc<Snapshot>>,
    requests: mpsc::SyncSender<()>,
    results: mpsc::Receiver<Snapshot>,
    pending: Cell<bool>,
    invalidated: Cell<bool>,
}

impl Catalogue {
    /// Seed before Studio opens its terminal. Every subsequent read is from
    /// this snapshot, including while a refresh waits on the library.
    pub fn new(library: Option<&Arc<SampleLibrary>>) -> std::io::Result<Self> {
        let initial = library.map_or_else(Snapshot::default, |library| Snapshot::read(library));
        let library = library.map(Arc::downgrade);
        Self::spawn(initial, move || {
            library
                .as_ref()
                .and_then(std::sync::Weak::upgrade)
                .map_or_else(Snapshot::default, |library| Snapshot::read(&library))
        })
    }

    fn spawn(
        initial: Snapshot,
        mut rebuild: impl FnMut() -> Snapshot + Send + 'static,
    ) -> std::io::Result<Self> {
        let (requests, input) = mpsc::sync_channel(1);
        let (output, results) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("studio-catalogue".into())
            .spawn(move || {
                while input.recv().is_ok() {
                    if output.send(rebuild()).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            snapshot: RefCell::new(Arc::new(initial)),
            requests,
            results,
            pending: Cell::new(false),
            invalidated: Cell::new(false),
        })
    }

    pub fn snapshot(&self) -> Arc<Snapshot> {
        Arc::clone(&self.snapshot.borrow())
    }

    /// Coalesce refresh requests: at most one build or result is outstanding.
    pub fn refresh(&self) {
        if !self.pending.get() && self.requests.try_send(()).is_ok() {
            self.pending.set(true);
        }
    }

    /// A source changed while a build was in flight. Publish only a build
    /// started after that change; keep displaying the current snapshot.
    pub fn invalidate(&self) {
        if self.pending.get() {
            self.invalidated.set(true);
        } else {
            self.refresh();
        }
    }

    /// Called each UI tick, independently of which panel is open.
    pub fn poll(&self) -> bool {
        match self.results.try_recv() {
            Ok(next) => {
                self.pending.set(false);
                if self.invalidated.replace(false) {
                    self.refresh();
                    return false;
                }
                let mut held = self.snapshot.borrow_mut();
                if **held == next {
                    return false;
                }
                *held = Arc::new(next);
                true
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.pending.set(false);
                false
            }
            Err(mpsc::TryRecvError::Empty) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_blocked_refresh_keeps_the_catalogue_immediately_available() {
        let (entered, started) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::sync_channel(1);
        let (finished, done) = mpsc::sync_channel(1);
        let mut initial = Snapshot::default();
        initial
            .sources
            .insert("cached".into(), SourceState::Loading);
        let catalogue = Catalogue::spawn(initial, move || {
            entered.send(()).unwrap();
            gate.recv().unwrap();
            finished.send(()).unwrap();
            Snapshot::default()
        })
        .unwrap();
        catalogue.refresh();
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        let cached = catalogue.snapshot();
        for _ in 0..100 {
            catalogue.refresh();
            assert!(!catalogue.poll());
            assert!(Arc::ptr_eq(&cached, &catalogue.snapshot()));
            assert!(catalogue.snapshot().sources.contains_key("cached"));
        }
        release.send(()).unwrap();
        done.recv_timeout(Duration::from_secs(2)).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !catalogue.poll() {
            assert!(
                std::time::Instant::now() < deadline,
                "refresh was not published"
            );
            std::thread::yield_now();
        }
        assert!(catalogue.snapshot().sources.is_empty());
        assert!(
            cached.sources.contains_key("cached"),
            "readers retain the old complete snapshot"
        );
        assert!(started.try_recv().is_err(), "requests were coalesced");
    }

    #[test]
    fn an_invalidated_catalogue_build_cannot_replace_the_visible_snapshot() {
        let (entered, started) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let mut build = 0;
        let catalogue = Catalogue::spawn(Snapshot::default(), move || {
            build += 1;
            entered.send(build).unwrap();
            gate.recv().unwrap();
            let mut next = Snapshot::default();
            next.sources
                .insert(format!("build-{build}"), SourceState::Loading);
            next
        })
        .unwrap();
        let original = catalogue.snapshot();
        catalogue.refresh();
        assert_eq!(started.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
        catalogue.invalidate();
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            assert!(!catalogue.poll(), "the outdated result was published");
            assert!(Arc::ptr_eq(&original, &catalogue.snapshot()));
            if let Ok(build) = started.try_recv() {
                assert_eq!(build, 2);
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "fresh build was not requested"
            );
            std::thread::yield_now();
        }
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !catalogue.poll() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(catalogue.snapshot().sources.contains_key("build-2"));
        assert!(!catalogue.snapshot().sources.contains_key("build-1"));
    }
}
