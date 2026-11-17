//! The scheduler's mapping between cycles and clock seconds.
//!
//! The map has an anchor: `cycle` is at `time`, and the clock runs at `cps`
//! cycles per second from there. A pattern can change the tempo at an onset
//! the scheduler queries ahead of the clock. Each such change starts a new
//! stretch of the timeline at its onset, so cycles before it keep their
//! times:
//!
//! ```text
//!   anchor          change 1          change 2
//!     |---- cps a ----|---- cps b ------|---- cps c ---->
//! ```
//!
//! [`TempoMap::advance`] makes a change the anchor once the clock reaches
//! it. Until then the anchor keeps the tempo that plays now.

use rustel_fraction::Fraction;

/// The start of a stretch of the timeline at a constant tempo.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Anchor {
    /// Clock seconds of `cycle`.
    time: f64,
    cycle: Fraction,
    /// Cycles per second. Finite and greater than zero.
    cps: f64,
    /// A pattern set this tempo, not the session.
    from_pattern: bool,
}

impl Anchor {
    fn time_at_cycle(&self, cycle: Fraction) -> f64 {
        self.time + cycle.sub(self.cycle).to_f64() / self.cps
    }

    fn cycle_at_time(&self, time: f64) -> f64 {
        self.cycle.to_f64() + (time - self.time) * self.cps
    }
}

/// Converts between cycle positions and clock seconds.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TempoMap {
    anchor: Anchor,
    /// Tempo changes ahead of the anchor, in cycle order. Each starts at a
    /// later cycle than the one before it. Usually empty.
    ahead: Vec<Anchor>,
}

impl TempoMap {
    /// Cycle zero at time zero, at `cps`.
    pub(crate) fn new(cps: f64) -> Self {
        Self {
            anchor: Anchor {
                time: 0.0,
                cycle: Fraction::ZERO,
                cps,
                from_pattern: false,
            },
            ahead: Vec::new(),
        }
    }

    /// The stretch that holds `cycle`. A cycle before the anchor reads the
    /// anchor's tempo backwards.
    fn at_cycle(&self, cycle: Fraction) -> &Anchor {
        self.ahead
            .iter()
            .rev()
            .find(|change| change.cycle <= cycle)
            .unwrap_or(&self.anchor)
    }

    /// The stretch that holds `time`, in clock seconds.
    fn at_time(&self, time: f64) -> &Anchor {
        self.ahead
            .iter()
            .rev()
            .find(|change| change.time <= time)
            .unwrap_or(&self.anchor)
    }

    /// The tempo of the anchor, in cycles per second: the one that plays
    /// once [`Self::advance`] has caught up with the clock.
    pub(crate) fn cps(&self) -> f64 {
        self.anchor.cps
    }

    /// The tempo at `cycle`.
    pub(crate) fn cps_at_cycle(&self, cycle: Fraction) -> f64 {
        self.at_cycle(cycle).cps
    }

    /// The tempo a pattern set for `cycle`, or `None` at the session tempo.
    pub(crate) fn pattern_cps_at(&self, cycle: Fraction) -> Option<f64> {
        let stretch = self.at_cycle(cycle);
        stretch.from_pattern.then_some(stretch.cps)
    }

    /// Clock seconds of `cycle`.
    pub(crate) fn time_at_cycle(&self, cycle: Fraction) -> f64 {
        self.at_cycle(cycle).time_at_cycle(cycle)
    }

    /// The cycle position at `time`, in clock seconds.
    pub(crate) fn cycle_at_time(&self, time: f64) -> f64 {
        self.at_time(time).cycle_at_time(time)
    }

    /// The anchor point: `(time, cycle)`.
    #[cfg(test)]
    pub(crate) fn anchor(&self) -> (f64, Fraction) {
        (self.anchor.time, self.anchor.cycle)
    }

    /// Put `cycle` at `time`, at the tempo the map has at `cycle`. Changes
    /// after `cycle` keep their cycles and tempos, and their times follow
    /// the new anchor. Changes at or before `cycle` are behind it.
    pub(crate) fn reanchor(&mut self, time: f64, cycle: Fraction) {
        let stretch = *self.at_cycle(cycle);
        self.ahead.retain(|change| change.cycle > cycle);
        self.anchor = Anchor {
            time,
            cycle,
            ..stretch
        };
        let mut before = self.anchor;
        for change in &mut self.ahead {
            change.time = before.time_at_cycle(change.cycle);
            before = *change;
        }
    }

    /// Run at the session tempo `cps` from the anchor point. It replaces
    /// each tempo a pattern set.
    pub(crate) fn set_cps(&mut self, cps: f64) {
        self.anchor.cps = cps;
        self.anchor.from_pattern = false;
        self.ahead.clear();
    }

    /// Put `cycle` at `time` and run at the session tempo `cps` from there.
    /// It replaces each tempo a pattern set.
    pub(crate) fn retime(&mut self, time: f64, cycle: Fraction, cps: f64) {
        self.anchor = Anchor {
            time,
            cycle,
            cps,
            from_pattern: false,
        };
        self.ahead.clear();
    }

    /// A pattern sets the tempo to `cps` at the onset `cycle`. Changes at or
    /// after `cycle` give way to it. Returns whether the tempo changed.
    pub(crate) fn change_at(&mut self, cycle: Fraction, cps: f64) -> bool {
        if self.cps_at_cycle(cycle) == cps {
            return false;
        }
        let change = Anchor {
            time: self.time_at_cycle(cycle),
            cycle,
            cps,
            from_pattern: true,
        };
        self.ahead.retain(|ahead| ahead.cycle < cycle);
        if cycle <= self.anchor.cycle {
            self.anchor = change;
        } else {
            self.ahead.push(change);
        }
        true
    }

    /// Drop the changes at or after `cycle`. The query from `cycle` finds
    /// them again if the pattern still has them.
    pub(crate) fn forget_from(&mut self, cycle: Fraction) {
        self.ahead.retain(|change| change.cycle < cycle);
    }

    /// Make the last change the clock has reached by `now` the anchor.
    pub(crate) fn advance(&mut self, now: f64) {
        let reached = self
            .ahead
            .iter()
            .take_while(|change| change.time <= now)
            .count();
        if let Some(last) = reached.checked_sub(1).map(|last| self.ahead[last]) {
            self.ahead.drain(..reached);
            self.anchor = last;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cycle(n: i128, d: i128) -> Fraction {
        Fraction::new(n, d)
    }

    #[test]
    fn the_map_converts_both_ways_around_its_anchor() {
        let mut map = TempoMap::new(0.5);
        map.reanchor(2.0, Fraction::int(3));
        assert_eq!(map.time_at_cycle(Fraction::int(4)), 4.0);
        assert_eq!(map.time_at_cycle(Fraction::int(2)), 0.0);
        assert_eq!(map.cycle_at_time(6.0), 5.0);
        map.retime(1.0, Fraction::ZERO, 2.0);
        assert_eq!(map.cps(), 2.0);
        assert_eq!(map.cycle_at_time(2.0), 2.0);
    }

    #[test]
    fn a_change_starts_a_stretch_at_its_onset() {
        let mut map = TempoMap::new(1.0);
        assert!(map.change_at(cycle(2, 1), 2.0));
        // Cycles before the change keep their times.
        assert_eq!(map.time_at_cycle(cycle(1, 1)), 1.0);
        assert_eq!(map.time_at_cycle(cycle(2, 1)), 2.0);
        assert_eq!(map.time_at_cycle(cycle(3, 1)), 2.5);
        assert_eq!(map.cycle_at_time(3.0), 4.0);
        assert_eq!(map.pattern_cps_at(cycle(1, 1)), None);
        assert_eq!(map.pattern_cps_at(cycle(5, 2)), Some(2.0));
        // The anchor keeps the tempo that plays now.
        assert_eq!(map.cps(), 1.0);
        assert!(
            !map.change_at(cycle(3, 1), 2.0),
            "the same tempo is no change"
        );
    }

    #[test]
    fn the_clock_makes_a_change_the_anchor() {
        let mut map = TempoMap::new(1.0);
        map.change_at(cycle(1, 1), 0.5);
        map.change_at(cycle(2, 1), 4.0);
        map.advance(0.9);
        assert_eq!(map.cps(), 1.0);
        map.advance(1.0);
        assert_eq!(map.cps(), 0.5);
        map.advance(5.0);
        assert_eq!(map.cps(), 4.0);
        assert_eq!(map.anchor(), (3.0, cycle(2, 1)));
        assert_eq!(map.pattern_cps_at(cycle(2, 1)), Some(4.0));
    }

    #[test]
    fn a_later_change_gives_way_to_an_earlier_one() {
        let mut map = TempoMap::new(1.0);
        map.change_at(cycle(2, 1), 2.0);
        map.change_at(cycle(1, 1), 0.5);
        assert_eq!(map.time_at_cycle(cycle(3, 1)), 5.0);
        map.change_at(cycle(3, 1), 1.0);
        map.forget_from(cycle(3, 1));
        assert_eq!(map.cps_at_cycle(cycle(4, 1)), 0.5);
    }

    #[test]
    fn a_reanchor_keeps_the_changes_after_it() {
        let mut map = TempoMap::new(1.0);
        map.change_at(cycle(2, 1), 2.0);
        map.reanchor(10.0, cycle(1, 1));
        assert_eq!(map.time_at_cycle(cycle(2, 1)), 11.0);
        assert_eq!(map.time_at_cycle(cycle(3, 1)), 11.5);
        // Past the change, the anchor takes its tempo and the change is behind.
        map.reanchor(20.0, cycle(5, 2));
        assert_eq!(map.cps(), 2.0);
        assert_eq!(map.pattern_cps_at(cycle(3, 1)), Some(2.0));
        // A session tempo replaces every tempo a pattern set.
        map.change_at(cycle(4, 1), 1.0);
        map.set_cps(0.5);
        assert_eq!(map.pattern_cps_at(cycle(5, 1)), None);
        assert_eq!(map.cps_at_cycle(cycle(5, 1)), 0.5);
    }
}
