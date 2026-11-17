//! The scheduler's mapping between cycles and clock seconds.
//!
//! The map holds one anchor: `cycle` is at `time`, and the clock runs at
//! `cps` cycles per second from there in both directions.

use rustel_fraction::Fraction;

/// One stretch of the timeline at a constant tempo.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Anchor {
    /// Clock seconds of `cycle`.
    time: f64,
    cycle: Fraction,
    /// Cycles per second. Finite and greater than zero.
    cps: f64,
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
}

impl TempoMap {
    /// Cycle zero at time zero, at `cps`.
    pub(crate) fn new(cps: f64) -> Self {
        Self {
            anchor: Anchor {
                time: 0.0,
                cycle: Fraction::ZERO,
                cps,
            },
        }
    }

    /// The tempo of the anchor, in cycles per second.
    pub(crate) fn cps(&self) -> f64 {
        self.anchor.cps
    }

    /// Clock seconds of `cycle`.
    pub(crate) fn time_at_cycle(&self, cycle: Fraction) -> f64 {
        self.anchor.time_at_cycle(cycle)
    }

    /// The cycle position at `time`, in clock seconds.
    pub(crate) fn cycle_at_time(&self, time: f64) -> f64 {
        self.anchor.cycle_at_time(time)
    }

    /// The anchor point: `(time, cycle)`.
    #[cfg(test)]
    pub(crate) fn anchor(&self) -> (f64, Fraction) {
        (self.anchor.time, self.anchor.cycle)
    }

    /// Put `cycle` at `time`. The tempo stays.
    pub(crate) fn reanchor(&mut self, time: f64, cycle: Fraction) {
        self.anchor.time = time;
        self.anchor.cycle = cycle;
    }

    /// Change the tempo. The anchor point stays.
    pub(crate) fn set_cps(&mut self, cps: f64) {
        self.anchor.cps = cps;
    }

    /// Put `cycle` at `time` and run at `cps` from there.
    pub(crate) fn retime(&mut self, time: f64, cycle: Fraction, cps: f64) {
        self.anchor = Anchor { time, cycle, cps };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
