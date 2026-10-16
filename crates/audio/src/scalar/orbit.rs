use super::*;

/// Stereo pan, STEREO input branch (the mono branch is
/// `channel_gains`): x ≤ 0 folds right into left, x > 0 folds left into
/// right, equal-power.
pub(super) fn stereo_pan(l: f32, r: f32, x: f32) -> (f32, f32) {
    use std::f32::consts::FRAC_PI_2;
    if x <= 0.0 {
        let a = (x + 1.0) * FRAC_PI_2;
        (l + r * a.cos(), r * a.sin())
    } else {
        let a = x * FRAC_PI_2;
        (l * a.cos(), r + l * a.sin())
    }
}

impl DuckSegments {
    fn value_at(&self, frame: u64) -> f32 {
        if !self.active {
            return 1.0;
        }
        if frame >= self.end_frame {
            return 1.0;
        }
        if frame <= self.capture_frame {
            return self.from;
        }
        // exponentialRampToValueAtTime: v0 * (v1/v0)^((t-t0)/(t1-t0)),
        // endpoints clamped positive by the ducked-value clamp.
        let (t0, v0, t1, v1) = if frame <= self.mid_frame {
            (self.capture_frame, self.from, self.mid_frame, self.ducked)
        } else {
            (self.mid_frame, self.ducked, self.end_frame, 1.0)
        };
        if t1 <= t0 {
            return v1;
        }
        let progress = (frame - t0) as f32 / (t1 - t0) as f32;
        v0 * (v1 / v0).powf(progress)
    }
}

impl OrbitDuck {
    pub(super) fn value_at(&self, frame: u64) -> f32 {
        if self.current.active && frame <= self.current.capture_frame {
            // Before the new hold point the OLD curve is still in charge.
            return self.previous.value_at(frame);
        }
        self.current.value_at(frame)
    }

    /// `duck(t, onset, attack, depth)` - schedule a dip at `t` (frames).
    pub(super) fn trigger(
        &mut self,
        t: u64,
        now: u64,
        sample_rate: u32,
        duck: crate::backend::DuckTarget,
    ) {
        // The dip is armed ~10 ms before t and holds the CURRENT value as
        // the first ramp's start point.
        // When the trigger arrives later than the hold point, still anchor
        // at NOW (cancel+hold at now, t0 = max(t, now)) so the curve stays
        // continuous. Anchoring in already-rendered
        // frames instead stepped the gain onto the middle of the ramp and
        // clicked on every live sidechain trigger.
        let capture = t
            .saturating_sub((0.01 * sample_rate as f64) as u64)
            .max(now);
        let from = self.value_at(capture).max(0.0001);
        let attack = duck.attack_secs.max(0.002);
        // clamp(1 - sqrt(depth), 0.01, current) with the inverted-range
        // rule: min(max(x, lo), hi), so hi (the current value) wins when
        // below lo.
        let ducked = (1.0 - duck.depth.max(0.0).sqrt()).max(0.01).min(from);
        let sr = sample_rate as f64;
        let t0 = t.max(capture);
        self.previous = self.current;
        self.current = DuckSegments {
            active: true,
            capture_frame: capture,
            from,
            ducked,
            mid_frame: t0 + (f64::from(duck.onset_secs.max(0.0)) * sr) as u64,
            end_frame: 0, // filled below
        };
        self.current.end_frame = self.current.mid_frame + (f64::from(attack) * sr) as u64;
    }
}

impl Default for OrbitGains {
    fn default() -> Self {
        Self([1.0; MAX_ORBITS])
    }
}

impl ScalarBackend {
    pub fn take_orbit_peaks(&mut self) -> [f32; MAX_ORBITS] {
        std::mem::replace(&mut self.orbit_peaks, [0.0; MAX_ORBITS])
    }

    /// Route each orbit to an output pair; pair 0 is the main mix.
    pub fn set_orbit_pairs(&mut self, pairs: [u8; MAX_ORBITS]) {
        self.orbit_pairs = pairs;
    }

    pub fn orbit_pairs(&self) -> [u8; MAX_ORBITS] {
        self.orbit_pairs
    }

    /// Whether any orbit is kept apart from the main pair.
    pub fn routing_active(&self) -> bool {
        self.orbit_pairs.iter().any(|pair| *pair != 0)
    }

    /// The stereo this orbit produced in the last block, interleaved, when
    /// it is routed away from the main pair; empty otherwise.
    pub fn orbit_mix(&self, orbit: usize, frames: usize) -> &[f32] {
        if orbit >= MAX_ORBITS || self.orbit_pairs[orbit] == 0 || self.orbit_mix.is_empty() {
            return &[];
        }
        let from = orbit * ORBIT_MIX_FRAMES * 2;
        &self.orbit_mix[from..from + frames.min(ORBIT_MIX_FRAMES) * 2]
    }

    /// Scale the stereo each routed orbit produced in the last block, one
    /// gain for each frame. The stop ramp fades a routed orbit with the main
    /// pair.
    pub fn scale_orbit_mix(&mut self, frames: usize, gain: impl Fn(usize) -> f32) {
        if self.orbit_mix.is_empty() {
            return;
        }
        let frames = frames.min(ORBIT_MIX_FRAMES);
        for orbit in 0..MAX_ORBITS {
            if self.orbit_pairs[orbit] == 0 {
                continue;
            }
            let from = orbit * ORBIT_MIX_FRAMES * 2;
            for (frame, [left, right]) in self.orbit_mix[from..from + frames * 2]
                .as_chunks_mut::<2>()
                .0
                .iter_mut()
                .enumerate()
            {
                let gain = gain(frame);
                *left *= gain;
                *right *= gain;
            }
        }
    }

    /// Silence each routed orbit for a block that renders nothing.
    pub fn clear_orbit_mix(&mut self, frames: usize) {
        if self.orbit_mix.is_empty() {
            return;
        }
        let span = frames.min(ORBIT_MIX_FRAMES) * 2;
        for orbit in 0..MAX_ORBITS {
            if self.orbit_pairs[orbit] != 0 {
                let from = orbit * ORBIT_MIX_FRAMES * 2;
                self.orbit_mix[from..from + span].fill(0.0);
            }
        }
    }

    pub fn set_ui_visual_capture_mask(&mut self, mask: u64) {
        self.ui_visual_capture_mask = mask;
    }

    pub fn set_ui_visual_capture_generation_floor(&mut self, generation: u64) {
        self.ui_visual_capture_generation_floor = generation;
    }

    /// Receiver-isolated post-voice, pre-orbit stereo audio.
    ///
    /// Shared orbit delay, reverb, ducking, DJF and orbit gain are excluded:
    /// attributing those mixed buses to one receiver would reintroduce sibling
    /// leakage.
    pub fn ui_visual_mix(&self, slot: usize, frames: usize) -> &[f32] {
        let Some(block) = self.ui_visual_mix.get(slot) else {
            return &[];
        };
        &block[..frames.min(UI_VISUAL_MIX_FRAMES) * 2]
    }

    /// Preallocate the complete voice/pending budget off the real-time thread.
    /// Install (or replace) a decoded sample in the bank. Offline surface:
    /// the live path delivers through the device's SPSC sample channel and
    /// installs between blocks.
    /// Sidechain scheduling, shared by both intake paths: queue the trigger
    /// for its ARM time (10 ms before the onset), never arm immediately.
    pub(super) fn arm_duck<K>(&mut self, event: &OnsetEvent<K>) {
        self.arm_duck_at(event.onset_frame, event.controls.duck);
    }

    /// Arm a sidechain dip the moment its event is first seen - at SCHEDULE
    /// time, while the 10 ms pre-hold point is comfortably in the future.
    /// The duck applies even when the event itself is muted (speed 0) or
    /// its voice is later refused.
    pub fn arm_duck_at(&mut self, onset_frame: u64, duck: Option<crate::backend::DuckControls>) {
        if let Some(duck) = duck {
            if duck.is_empty() {
                return;
            }
            if self.pending_ducks.len() == self.pending_ducks.capacity() {
                // Full queue: dropping one duck dip beats allocating on the
                // audio thread. Capacity matches the voice budget.
                return;
            }
            let hold = (0.01 * f64::from(self.sample_rate.max(1))) as u64;
            self.pending_ducks.push(PendingDuck {
                arm_frame: onset_frame.saturating_sub(hold),
                onset_frame,
                controls: duck,
            });
        }
    }

    /// The mixer's orbit faders, linear; 1 is unity. Read once a block by
    /// the callback from the device's shared state.
    pub fn set_orbit_gains(&mut self, gains: &[f32; MAX_ORBITS]) {
        self.orbit_gains = OrbitGains(*gains);
    }

    /// `channels` - which OUTPUT channel each source channel is wired to.
    ///
    /// The control counts outputs from 1 (interface convention), and source
    /// channel `i` is wired to destination `channels[i]`. So a source
    /// channel with no entry is DROPPED, an output nobody names stays
    /// SILENT, two entries naming one output SUM there, and the order
    /// matters - `"2:1"` swaps the pair. The index wraps on the render's
    /// channel count, which is why `channels(3)` is `channels(1)` here and
    /// `"3:4"` is `"1:2"`.
    ///
    /// Without the routing, `channels(2)` plays from both outputs and is
    /// twice as loud in mono: +6.012 dB against Chromium, which leaves its
    /// left output silent.
    ///
    /// Applied at the ORBIT mix, deliberately: the orbit bus itself is
    /// connected to the named outputs, so every voice in the orbit - and
    /// its reverb and delay sends - travels together. Which list wins is
    /// decided at orbit creation; see `orbit_channels`.
    #[inline]
    pub(super) fn route_channels(channels: Option<[u8; 2]>, left: f32, right: f32) -> (f32, f32) {
        let Some(destinations) = channels else {
            return (left, right);
        };
        let mut out = [0.0f32; 2];
        for (source, destination) in [left, right].into_iter().zip(destinations) {
            if destination == 0 {
                continue;
            }
            let slot = usize::from(destination - 1) % out.len();
            out[slot] += source;
        }
        (out[0], out[1])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    mod ui_visual_mix_tests {
        use super::*;
        use crate::backend::{AudioBackend, OnsetEvent};

        fn render(first_frequency: f32) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
            let mut backend = ScalarBackend::prepared(48_000, 16).expect("backend");
            backend.set_ui_visual_capture_mask(0b11);
            backend.note(OnsetEvent::new(0, first_frequency, 0.2, 0.1).with_ui_visuals(0b01));
            backend.note(OnsetEvent::new(0, 440.0, 0.4, 0.1).with_ui_visuals(0b10));
            let mut output = vec![0.0; UI_VISUAL_MIX_FRAMES * 2];
            backend.process_block(&mut output, UI_VISUAL_MIX_FRAMES);
            (
                output,
                backend.ui_visual_mix(0, UI_VISUAL_MIX_FRAMES).to_vec(),
                backend.ui_visual_mix(1, UI_VISUAL_MIX_FRAMES).to_vec(),
            )
        }

        #[test]
        fn visual_audio_contains_only_voices_tagged_for_that_receiver() {
            let (output, first, second) = render(220.0);
            assert!(first.iter().any(|sample| sample.abs() > 1e-5));
            assert!(second.iter().any(|sample| sample.abs() > 1e-5));
            for ((mixed, first), second) in output.iter().zip(&first).zip(&second) {
                assert!((mixed - first - second).abs() < 1e-6);
            }

            let (_, _, second_after_sibling_change) = render(880.0);
            assert_eq!(second, second_after_sibling_change);
        }
    }

    mod channel_route_tests {
        /// Every expectation here was read off Chromium's own left/right rms for
        /// `s("bd").bank("tr909")`, whose channels are 0.092521 and 0.092374.
        #[test]
        fn channels_wires_each_source_channel_to_the_output_it_names() {
            let route = |channels| super::ScalarBackend::route_channels(channels, 1.0, 2.0);

            // One destination drops the second source. Repeated destinations sum.
            for (channels, expected) in [
                (None, (1.0, 2.0)),
                (Some([1, 2]), (1.0, 2.0)),
                (Some([2, 1]), (2.0, 1.0)),
                (Some([1, 0]), (1.0, 0.0)),
                (Some([2, 0]), (0.0, 1.0)),
                (Some([1, 1]), (3.0, 0.0)),
                (Some([2, 2]), (0.0, 3.0)),
                (Some([3, 0]), (1.0, 0.0)),
                (Some([4, 0]), (0.0, 1.0)),
                (Some([3, 4]), (1.0, 2.0)),
            ] {
                assert_eq!(route(channels), expected, "{channels:?}");
            }
        }
    }
}
