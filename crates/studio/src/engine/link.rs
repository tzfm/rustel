//! The Link session's side of the engine: turning it on and off,
//! following its tempo and phase, and telling it what this studio plays.

use std::time::Instant;

use rustel_runtime::link::Link;
use rustel_runtime::midi_clock::{self, ClockEstimate};

/// Four beats to the cycle, as the session's bpm turns into scheduler
/// cycles per second.
fn cps_of_bpm(bpm: f64) -> f64 {
    bpm / 240.0
}

impl super::StudioEngine {
    /// Join, lead or leave the Link session. Joining founds a session
    /// standing where the score stands, so nothing moves until the
    /// network says otherwise. The MIDI clock in steps aside: one
    /// outside clock leads at a time.
    pub fn set_link(&mut self, on: bool) -> Result<(), String> {
        if on == self.link.is_some() {
            return Ok(());
        }
        if !on {
            self.link = None;
            self.link_playing_sent = None;
            return Ok(());
        }
        if self.clock_in.is_some() {
            self.set_clock_in(None)?;
        }
        let bpm = midi_clock::bpm(self.session.cps());
        let cycle = self.live.as_ref().map_or(0.0, |live| {
            self.session.cycle_at_time(live.device.clock_seconds())
        });
        let link = Link::start(bpm, cycle).map_err(|error| format!("Link: {error}"))?;
        self.link = Some(link);
        self.link_playing_sent = None;
        // The first turn may follow at once; a session just joined has
        // a grid worth hearing immediately.
        self.link_followed_at = Instant::now() - midi_clock::FOLLOW_INTERVAL;
        Ok(())
    }

    /// The Link session once a turn: follow the grid it runs on, and
    /// keep its transport told what this studio does.
    pub(crate) fn pump_link(&mut self) {
        let Some(status) = self.link.as_ref().map(|link| link.status()) else {
            return;
        };
        let estimate = ClockEstimate {
            cps: cps_of_bpm(status.bpm),
            cycle: status.cycle,
            running: true,
            positioned: true,
        };
        let parked =
            self.live.is_some() && !self.is_stopping() && self.session.transport().is_stopped();
        if parked {
            self.follow_link_stopped(estimate);
        } else {
            self.steer_to(estimate, Instant::now());
        }
        self.publish_link_playing();
    }

    /// Retime the parked score onto the session's grid, at the same
    /// cadence the playing follow bends. `steer_to` declines a stopped
    /// transport, and rightly so; a parked grid can still be moved
    /// without a sound. Whether the interval allowed it.
    fn follow_link_stopped(&mut self, estimate: ClockEstimate) -> bool {
        let now_instant = Instant::now();
        if now_instant.duration_since(self.link_followed_at) < midi_clock::FOLLOW_INTERVAL {
            return false;
        }
        self.link_followed_at = now_instant;
        let Some(live) = self.live.as_ref() else {
            return true;
        };
        let now = live.device.clock_seconds();
        let (steer, _) = midi_clock::steer(
            estimate,
            self.session.cycle_at_time(now),
            self.session.cps(),
        );
        if let midi_clock::Steer::To { cps, cycle } = steer {
            self.session.retime(now, cps, cycle);
        }
        true
    }

    /// Tell the session this studio started or stopped, once per change.
    /// A start the session itself asked for echoes back as the same
    /// decision one timestamp newer, which the protocol already handles.
    fn publish_link_playing(&mut self) {
        let playing = self
            .live
            .as_ref()
            .is_some_and(|live| live.draining.is_none())
            && !self.session.transport().is_stopped();
        if self.link_playing_sent == Some(playing) {
            return;
        }
        self.link_playing_sent = Some(playing);
        if let Some(link) = self.link.as_ref() {
            link.set_playing(playing);
        }
    }

    /// Tell the session the tempo a fresh evaluation settled on, when it
    /// differs from what the session runs. The follow's own bends are
    /// never published from here: they arrive already agreeing.
    pub(crate) fn publish_link_tempo(&self) {
        let Some(link) = self.link.as_ref() else {
            return;
        };
        let ours = midi_clock::bpm(self.session.cps());
        let status = link.status();
        if (status.bpm - ours).abs() > 0.05 {
            link.set_tempo(ours);
        }
    }
}
