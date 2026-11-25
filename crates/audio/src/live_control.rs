//! Sample-clock transitions for live gain and continuous score controls.
//!
//! The UI commits the exact number immediately. Only the sounding gain,
//! low-pass cutoff, low-pass resonance and direct FM route index move
//! through a short ramp. Note timing and pattern values never do.
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
