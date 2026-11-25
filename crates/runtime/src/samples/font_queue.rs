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
