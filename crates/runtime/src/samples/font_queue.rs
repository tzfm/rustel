//! Soundfont jobs use the same priority policy as ordinary sample loads.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};

use super::LoadPriority;

pub(super) struct FontQueue {
    state: Mutex<State>,
    wake: Condvar,
}

#[derive(Default)]
struct State {
    now: VecDeque<Arc<str>>,
    bets: VecDeque<Arc<str>>,
    closed: bool,
}

impl FontQueue {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
        })
    }

    pub(super) fn push(&self, font: Arc<str>, priority: LoadPriority) {
        let mut state = self.state.lock().expect("soundfont queue");
        if state.closed {
            return;
        }
        match priority {
            LoadPriority::Now => state.now.push_back(font),
            LoadPriority::Bet => state.bets.push_back(font),
        }
        self.wake.notify_one();
    }

    pub(super) fn promote(&self, font: &str) {
        let mut state = self.state.lock().expect("soundfont queue");
        if let Some(at) = state.bets.iter().position(|queued| &**queued == font)
            && let Some(font) = state.bets.remove(at)
        {
            state.now.push_back(font);
            self.wake.notify_one();
        }
    }

    fn take(state: &mut State) -> Option<Arc<str>> {
        state.now.pop_front().or_else(|| state.bets.pop_back())
    }

    pub(super) fn pop(&self) -> Option<Arc<str>> {
        let mut state = self.state.lock().expect("soundfont queue");
        loop {
            if let Some(font) = Self::take(&mut state) {
                return Some(font);
            }
            if state.closed {
                return None;
            }
            state = self.wake.wait(state).expect("soundfont queue");
        }
    }

    #[cfg(test)]
    pub(super) fn try_pop(&self) -> Option<Arc<str>> {
        Self::take(&mut self.state.lock().expect("soundfont queue"))
    }

    pub(super) fn close(&self) {
        let mut state = self.state.lock().expect("soundfont queue");
        state.closed = true;
        self.wake.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soundfont_priority_promotes_actual_requests_ahead_of_old_bets() {
        let queue = FontQueue::new();
        for (name, priority) in [
            ("old", LoadPriority::Bet),
            ("older", LoadPriority::Bet),
            ("playing", LoadPriority::Now),
            ("new", LoadPriority::Bet),
        ] {
            queue.push(Arc::from(name), priority);
        }
        queue.promote("old");
        queue.promote("old");
        let order: Vec<_> = std::iter::from_fn(|| queue.try_pop())
            .map(|font| font.to_string())
            .collect();
        assert_eq!(order, ["playing", "old", "new", "older"]);
        queue.close();
        assert!(queue.pop().is_none());
        queue.push(Arc::from("after close"), LoadPriority::Now);
        assert!(queue.try_pop().is_none());
    }

    #[test]
    fn closing_soundfont_queue_releases_an_idle_worker() {
        let queue = FontQueue::new();
        let worker_queue = Arc::clone(&queue);
        let (finished, completion) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            finished.send(worker_queue.pop()).unwrap();
        });
        queue.close();
        assert!(
            completion
                .recv_timeout(std::time::Duration::from_secs(1))
                .expect("closed queue must wake the worker")
                .is_none()
        );
        worker.join().unwrap();
    }
}
