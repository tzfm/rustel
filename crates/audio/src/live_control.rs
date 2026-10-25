//! Sample-clock transitions for live gain and continuous score controls.
//!
//! The UI commits the exact number immediately. Only the sounding gain,
//! low-pass cutoff and low-pass resonance move through a short ramp. Note
//! timing and pattern values never do.
//!
//! ```text
//! slider move --> LiveControlUpdate --> control ring --> audio callback
//!                                                          |
//!                 new voice: starts at the target <--------+
//!                 sounding voice: Ramp (5 ms exact, 35 ms smoothed)
//! ```

/// Glide of a smoothed slider move, in seconds.
pub const SLIDER_SMOOTHING_SECS: f32 = 0.035;

/// Transition of an exact update, in seconds. A sounding voice never steps:
/// a step in gain, cutoff or resonance is a click, and the steps of one drag
/// are heard as distortion.
pub const SLIDER_DECLICK_SECS: f32 = 0.005;

/// A plain-data message on the device's single-producer control ring.
#[derive(Clone, Copy, Debug)]
pub struct LiveControlUpdate {
    pub binding: u64,
    pub value: f32,
    pub smooth: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Ramp {
    pub binding: u64,
    value: f32,
    target: f32,
    step: f32,
    remaining: u32,
}

impl Ramp {
    pub fn new(binding: u64, value: f32) -> Self {
        Self {
            binding,
            value,
            target: value,
            ..Self::default()
        }
    }

    /// Continue a matching live fade when a note retriggers.
    pub fn continue_from(&mut self, previous: Self) {
        if self.binding != 0
            && self.binding == previous.binding
            && self.target == previous.target
            && previous.remaining != 0
        {
            *self = previous;
        }
    }

    /// Starts at the value of `previous` and moves to the target of this
    /// ramp in [`SLIDER_DECLICK_SECS`]: a change at a note start is then no
    /// step. With the target of `previous`, this ramp goes on as `previous`.
    pub fn declick_from(&mut self, previous: Self, sample_rate: u32) {
        if previous.target == self.target {
            (self.value, self.step, self.remaining) =
                (previous.value, previous.step, previous.remaining);
            return;
        }
        self.value = previous.value;
        self.remaining = (SLIDER_DECLICK_SECS * sample_rate as f32).round().max(1.0) as u32;
        let count = self.remaining as f32;
        self.step = self.target / count - self.value / count;
    }

    /// True while the value moves to the target.
    pub fn is_moving(&self) -> bool {
        self.remaining != 0
    }

    pub fn retarget(&mut self, update: LiveControlUpdate, sample_rate: u32) {
        if self.binding == 0 || self.binding != update.binding {
            return;
        }
        let secs = if update.smooth {
            SLIDER_SMOOTHING_SECS
        } else {
            SLIDER_DECLICK_SECS
        };
        self.set_target(update.value, (secs * sample_rate as f32).round() as u32);
    }

    pub fn target(&self) -> f32 {
        self.target
    }

    pub fn set_target(&mut self, value: f32, frames: u32) {
        self.target = value;
        self.remaining = frames.max(1);
        let count = self.remaining as f32;
        self.step = self.target / count - self.value / count;
    }

    #[inline]
    pub fn next(&mut self) -> f32 {
        if self.remaining != 0 {
            self.remaining -= 1;
            if self.remaining == 0 {
                self.value = self.target;
            } else {
                self.value = if self.step >= 0.0 {
                    (self.value + self.step).min(self.target)
                } else {
                    (self.value + self.step).max(self.target)
                };
            }
        }
        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramp_hits_exact_target_and_retargets_from_current_value() {
        let mut ramp = Ramp::new(7, 0.0);
        ramp.retarget(
            LiveControlUpdate {
                binding: 7,
                value: 1.0,
                smooth: true,
            },
            1000,
        );
        for _ in 0..10 {
            ramp.next();
        }
        let current = ramp.value;
        ramp.retarget(
            LiveControlUpdate {
                binding: 7,
                value: 0.5,
                smooth: true,
            },
            1000,
        );
        assert_eq!(ramp.value, current);
        for (binding, value) in [(0, 0.5), (8, 0.5), (7, 0.75)] {
            let mut next = Ramp::new(binding, value);
            next.continue_from(ramp);
            assert_eq!(next.next(), value);
        }
        for _ in 0..35 {
            ramp.next();
        }
        assert_eq!(ramp.next(), 0.5);
    }

    #[test]
    fn a_change_at_a_note_start_moves_from_the_value_before_and_ends_exact() {
        // 5 ms at 1000 Hz is five frames.
        let mut next = Ramp::new(0, 0.25);
        next.declick_from(Ramp::new(0, 1.0), 1000);
        assert!(next.is_moving());
        let values: Vec<f32> = (0..6).map(|_| next.next()).collect();
        assert!(values.windows(2).take(4).all(|pair| pair[1] < pair[0]));
        assert!(values[0] < 1.0 && values[0] > 0.25, "{values:?}");
        assert_eq!(values[4..], [0.25, 0.25]);
        assert!(!next.is_moving());

        // The same target goes on as the ramp before, in the middle of a
        // move too.
        let mut moving = Ramp::new(0, 0.5);
        moving.declick_from(Ramp::new(0, 0.0), 1000);
        moving.next();
        let mut same = Ramp::new(0, 0.5);
        same.declick_from(moving, 1000);
        assert_eq!(
            (same.value, same.remaining),
            (moving.value, moving.remaining)
        );
        let mut settled = Ramp::new(0, 0.5);
        settled.declick_from(Ramp::new(0, 0.5), 1000);
        assert!(!settled.is_moving());
        assert_eq!(settled.next(), 0.5);
    }

    #[test]
    fn exact_update_replaces_the_glide_with_a_short_ramp_and_other_bindings_do_nothing() {
        let mut ramp = Ramp::new(7, 0.0);
        ramp.retarget(
            LiveControlUpdate {
                binding: 7,
                value: 1.0,
                smooth: true,
            },
            1000,
        );
        ramp.next();
        ramp.retarget(
            LiveControlUpdate {
                binding: 8,
                value: 9.0,
                smooth: false,
            },
            1000,
        );
        assert!(ramp.next() < 1.0);
        let before = ramp.value;
        ramp.retarget(
            LiveControlUpdate {
                binding: 7,
                value: 0.125,
                smooth: false,
            },
            1000,
        );
        // 5 ms at 1000 Hz is five frames. The first frame does not step.
        let first = ramp.next();
        assert!(first > before && first < 0.125, "{first}");
        for _ in 0..3 {
            ramp.next();
        }
        assert_eq!(ramp.next(), 0.125);
        assert_eq!(ramp.remaining, 0);
    }
}
