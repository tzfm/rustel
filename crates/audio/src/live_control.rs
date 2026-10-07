//! Sample-clock transitions for directly bound continuous score controls.
//!
//! The UI commits the exact number immediately. Only the sounding gain/cutoff
//! moves through a short ramp; note timing and pattern values never do.
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
/// a step in gain or cutoff is a click, and the steps of one drag are heard
/// as distortion.
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

    pub fn retarget(&mut self, update: LiveControlUpdate, sample_rate: u32) {
        if self.binding == 0 || self.binding != update.binding {
            return;
        }
        self.target = update.value;
        let secs = if update.smooth {
            SLIDER_SMOOTHING_SECS
        } else {
            SLIDER_DECLICK_SECS
        };
        self.remaining = (secs * sample_rate as f32).round().max(1.0) as u32;
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
        for _ in 0..35 {
            ramp.next();
        }
        assert_eq!(ramp.next(), 0.5);
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
