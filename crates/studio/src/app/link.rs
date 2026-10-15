//! The Link session's side of the app: the checkbox that joins it, and
//! the transport that follows what the session plays.

use super::*;

impl App {
    /// Join or leave the Link session. Never kept between runs: joining
    /// puts this studio on a grid other machines run, so it is a thing
    /// done on purpose, each time.
    pub(crate) fn set_link(&mut self, on: bool) {
        if on == self.link_enabled {
            return;
        }
        if self.worker.try_link(on) {
            self.link_enabled = on;
            self.link_seen_playing = None;
            self.status = if on {
                "link session joined - tempo and transport follow it"
            } else {
                "link session left"
            }
            .into();
        }
    }

    /// The Link session once a turn: a session that starts playing
    /// starts this studio too, and one that stops stops it. What this
    /// studio plays is already told to the session by the engine, so
    /// the echoes of our own decisions arrive agreeing with us and
    /// nothing happens.
    pub(super) fn pump_link(&mut self) {
        let Some(link) = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.clock.link)
        else {
            self.link_seen_playing = None;
            return;
        };
        let seen = self.link_seen_playing.replace(link.playing);
        if seen == Some(link.playing) {
            return;
        }
        if link.playing && !self.is_playing() {
            let _ = self.dispatch_editor(Command::Evaluate);
        } else if seen == Some(true) && !link.playing && self.is_playing() && !self.is_stopping() {
            // A stop is the session going quiet after it was heard
            // playing. The turns before our own start echoes back must
            // not read as that.
            let _ = self.dispatch_editor(Command::Stop);
        }
    }
}
