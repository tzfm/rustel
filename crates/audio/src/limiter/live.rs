//! Sample-counted transitions for the live master limiter.

use super::{Character, DEFAULT_THRESHOLD_DB, Limiter};
use crate::meter::{LimiterSettings, db_to_linear};

fn sanitize_sample(sample: f32) -> f32 {
    if sample.is_finite() {
        sample.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Settings {
    limiter: Option<LimiterSettings>,
    makeup: bool,
}

impl Settings {
    fn new(limiter: Option<LimiterSettings>, makeup: bool) -> Self {
        let limiter = limiter
            .filter(|settings| settings.threshold_db.is_finite())
            .map(|settings| LimiterSettings {
                threshold_db: settings.threshold_db.clamp(-60.0, 0.0),
                ..settings
            });
        Self {
            limiter,
            makeup: limiter.is_some() && makeup,
        }
    }
}

struct Path {
    limiter: Limiter,
    settings: Settings,
    makeup_gain: f32,
}

impl Path {
    fn new(sample_rate: u32) -> Self {
        Self {
            limiter: Limiter::new(sample_rate, DEFAULT_THRESHOLD_DB, Character::default()),
            settings: Settings::new(None, false),
            makeup_gain: 1.0,
        }
    }

    fn configure(&mut self, settings: Settings) {
        self.settings = settings;
        self.makeup_gain = 1.0;
        if let Some(settings) = settings.limiter {
            self.limiter.set_threshold_db(settings.threshold_db);
            if self.limiter.character() == settings.character {
                self.limiter.reset();
            } else {
                self.limiter.set_character(settings.character);
            }
            if self.settings.makeup {
                self.makeup_gain = 1.0 / db_to_linear(settings.threshold_db);
            }
        }
    }

    fn latency_frames(&self) -> usize {
        if self.settings.limiter.is_some() {
            self.limiter.latency_frames()
        } else {
            0
        }
    }

    fn process_frame(&mut self, left: f32, right: f32) -> ([f32; 2], f32) {
        if self.settings.limiter.is_some() {
            let (left, right) = self.limiter.process_frame(left, right);
            (
                [
                    (left * self.makeup_gain).clamp(-1.0, 1.0),
                    (right * self.makeup_gain).clamp(-1.0, 1.0),
                ],
                self.limiter.take_reduction(),
            )
        } else {
            ([sanitize_sample(left), sanitize_sample(right)], 1.0)
        }
    }
}

struct Transition {
    warm_remaining: usize,
    fade_progress: usize,
}

/// Keeps the audible path intact while a second, fixed-size path warms up.
/// Changes during a transition coalesce into the latest requested settings.
pub(crate) struct LiveLimiter {
    paths: [Path; 2],
    active: usize,
    initialized: bool,
    transition: Option<Transition>,
    fade_frames: usize,
}

impl LiveLimiter {
    pub(crate) fn new(sample_rate: u32) -> Self {
        Self {
            paths: [Path::new(sample_rate), Path::new(sample_rate)],
            active: 0,
            initialized: false,
            transition: None,
            fade_frames: (f64::from(sample_rate.max(1)) * 0.010).round().max(1.0) as usize,
        }
    }

    /// Processes interleaved stereo and returns the smallest audible gain.
    /// Steady settings retain the selected mode's original latency and sound.
    pub(crate) fn process_stereo(
        &mut self,
        block: &mut [f32],
        limiter: Option<LimiterSettings>,
        makeup: bool,
    ) -> f32 {
        let target = Settings::new(limiter, makeup);
        let mut reduction = 1.0_f32;
        let (frames, remainder) = block.as_chunks_mut::<2>();
        for frame in frames {
            if !self.initialized {
                self.paths[self.active].configure(target);
                self.initialized = true;
            }
            let incoming = 1 - self.active;
            if self.transition.is_none() && self.paths[self.active].settings != target {
                self.paths[incoming].configure(target);
                self.transition = Some(Transition {
                    warm_remaining: self.paths[incoming].latency_frames(),
                    fade_progress: 0,
                });
            }
            let (mut output, mut gain) = self.paths[self.active].process_frame(frame[0], frame[1]);
            if let Some(transition) = &mut self.transition {
                let (next, next_gain) = self.paths[incoming].process_frame(frame[0], frame[1]);
                if transition.warm_remaining > 0 {
                    transition.warm_remaining -= 1;
                } else {
                    transition.fade_progress += 1;
                    let weight = transition.fade_progress as f32 / self.fade_frames as f32;
                    // A convex blend preserves full-scale safety through mode and bypass changes.
                    for channel in 0..2 {
                        output[channel] = (output[channel] * (1.0 - weight)
                            + next[channel] * weight)
                            .clamp(-1.0, 1.0);
                    }
                    gain = gain.min(next_gain);
                    if transition.fade_progress == self.fade_frames {
                        self.active = incoming;
                        self.transition = None;
                    }
                }
            }
            frame.copy_from_slice(&output);
            reduction = reduction.min(gain);
        }
        for sample in remainder {
            *sample = sanitize_sample(*sample);
        }
        reduction
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: u32 = 48_000;

    fn settings(character: Character, threshold_db: f32) -> Option<LimiterSettings> {
        Some(LimiterSettings {
            threshold_db,
            character,
        })
    }

    fn constant(
        limiter: &mut LiveLimiter,
        settings: Option<LimiterSettings>,
        makeup: bool,
        level: f32,
        frames: usize,
    ) -> Vec<f32> {
        let mut block = vec![level; frames * 2];
        limiter.process_stereo(&mut block, settings, makeup);
        block
    }

    fn assert_smooth(previous: f32, block: &[f32]) {
        let mut previous = previous;
        for frame in block.as_chunks::<2>().0 {
            assert!(
                (frame[0] - previous).abs() < 0.005,
                "sample jumped from {previous} to {}",
                frame[0]
            );
            previous = frame[0];
        }
    }

    #[test]
    fn character_changes_preserve_continuous_audio() {
        for from in Character::ALL {
            for to in Character::ALL {
                for level in [0.25, 0.75] {
                    for makeup in [false, true] {
                        let mut limiter = LiveLimiter::new(SAMPLE_RATE);
                        let before =
                            constant(&mut limiter, settings(from, -6.0), makeup, level, 2048);
                        let after = constant(&mut limiter, settings(to, -6.0), makeup, level, 2048);
                        let expected = *before.last().unwrap();
                        for sample in after {
                            assert!((sample - expected).abs() < 1e-5, "{from:?} to {to:?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn threshold_changes_are_smoothed_with_and_without_makeup() {
        for makeup in [false, true] {
            let mut limiter = LiveLimiter::new(SAMPLE_RATE);
            let initial = constant(
                &mut limiter,
                settings(Character::Transparent, -3.0),
                makeup,
                0.2,
                2048,
            );
            let mut previous = *initial.last().unwrap();
            for threshold_db in [-18.0, -1.0, -60.0, 0.0, -6.0] {
                let block = constant(
                    &mut limiter,
                    settings(Character::Transparent, threshold_db),
                    makeup,
                    0.2,
                    2048,
                );
                assert_smooth(previous, &block);
                previous = *block.last().unwrap();
                let ceiling = db_to_linear(threshold_db);
                let expected = 0.2_f32.min(ceiling) / if makeup { ceiling } else { 1.0 };
                assert!((previous - expected).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn makeup_and_bypass_changes_are_smoothed() {
        let mut limiter = LiveLimiter::new(SAMPLE_RATE);
        let enabled = settings(Character::Warm, -12.0);
        let initial = constant(&mut limiter, enabled, false, 0.2, 2048);
        let mut previous = *initial.last().unwrap();
        for (target, makeup) in [
            (enabled, true),
            (None, true),
            (enabled, true),
            (enabled, false),
        ] {
            let block = constant(&mut limiter, target, makeup, 0.2, 2048);
            assert_smooth(previous, &block);
            previous = *block.last().unwrap();
            let expected = if target.is_some() && makeup {
                0.2 / db_to_linear(-12.0)
            } else {
                0.2
            };
            assert!((previous - expected).abs() < 1e-5);
        }
    }

    #[test]
    fn rapid_changes_settle_on_latest_request_and_its_latency() {
        let mut limiter = LiveLimiter::new(SAMPLE_RATE);
        constant(
            &mut limiter,
            settings(Character::Transparent, -3.0),
            false,
            0.1,
            2048,
        );
        let final_settings = settings(Character::Hard, -6.0);
        for (target, makeup) in [
            (settings(Character::Warm, -9.0), true),
            (settings(Character::Punchy, -18.0), false),
            (settings(Character::Transparent, -24.0), true),
            (None, false),
            (final_settings, false),
        ] {
            constant(&mut limiter, target, makeup, 0.1, 31);
        }
        let settled = constant(&mut limiter, final_settings, false, 0.1, 5000);
        assert!((settled.last().unwrap() - 0.1).abs() < 1e-5);
        constant(&mut limiter, final_settings, false, 0.0, 4096);
        let mut impulse = vec![0.0; 2048];
        impulse[0] = 0.125;
        impulse[1] = 0.125;
        limiter.process_stereo(&mut impulse, final_settings, false);
        let first = impulse
            .as_chunks::<2>()
            .0
            .iter()
            .position(|frame| frame[0] != 0.0);
        assert_eq!(first, Some(Character::Hard.lookahead_frames(SAMPLE_RATE)));
    }

    #[test]
    fn hostile_samples_and_settings_stay_finite_and_within_full_scale() {
        let hostile = [
            f32::MIN,
            f32::MAX,
            f32::INFINITY,
            f32::NAN,
            -4.0,
            4.0,
            0.0,
            1e-38,
        ];
        let mut limiter = LiveLimiter::new(SAMPLE_RATE);
        for threshold in [-120.0, 12.0, -6.0, f32::NAN] {
            for character in Character::ALL {
                for makeup in [false, true] {
                    let mut block: Vec<_> = hostile.into_iter().cycle().take(4096).collect();
                    let reduction =
                        limiter.process_stereo(&mut block, settings(character, threshold), makeup);
                    assert!(reduction.is_finite() && (0.0..=1.0).contains(&reduction));
                    assert!(
                        block
                            .iter()
                            .all(|sample| sample.is_finite() && sample.abs() <= 1.0)
                    );
                }
            }
        }
    }

    #[test]
    fn callback_partitioning_does_not_change_transitions() {
        fn render(chunk_frames: usize) -> (Vec<f32>, f32) {
            let mut limiter = LiveLimiter::new(SAMPLE_RATE);
            let mut output = Vec::new();
            let mut reduction = 1.0_f32;
            let mut cursor = 0;
            for (frames, target, makeup) in [
                (1731, settings(Character::Transparent, -3.0), false),
                (122, settings(Character::Warm, -24.0), true),
                (138, settings(Character::Punchy, -9.0), false),
                (177, None, false),
                (93, settings(Character::Hard, -18.0), true),
                (2207, settings(Character::Warm, -6.0), false),
            ] {
                let mut block: Vec<_> = (cursor..cursor + frames * 2)
                    .map(|sample| (sample as f32 * 0.071).sin() * 0.8)
                    .collect();
                for chunk in block.chunks_mut(chunk_frames * 2) {
                    reduction = reduction.min(limiter.process_stereo(chunk, target, makeup));
                }
                cursor += frames * 2;
                output.extend(block);
            }
            (output, reduction)
        }

        let expected = render(4096);
        for chunk_frames in [1, 17, 256] {
            assert_eq!(render(chunk_frames), expected);
        }
    }

    #[test]
    fn unchanged_settings_match_static_limiter() {
        for character in Character::ALL {
            for makeup in [false, true] {
                let mut limiter = LiveLimiter::new(SAMPLE_RATE);
                let mut reference = Limiter::new(SAMPLE_RATE, -6.0, character);
                let mut actual: Vec<_> = (0..8192)
                    .map(|sample| (sample as f32 * 0.043).sin() * 0.9)
                    .collect();
                let mut expected = actual.clone();
                reference.process_stereo(&mut expected);
                let gain = if makeup {
                    1.0 / db_to_linear(-6.0)
                } else {
                    1.0
                };
                for sample in &mut expected {
                    *sample = (*sample * gain).clamp(-1.0, 1.0);
                }
                let reduction =
                    limiter.process_stereo(&mut actual, settings(character, -6.0), makeup);
                assert_eq!(actual, expected);
                assert_eq!(reduction, reference.take_reduction());
            }
        }
        let mut limiter = LiveLimiter::new(SAMPLE_RATE);
        let mut bypass = [f32::NAN, 0.25, 2.0, -2.0, f32::INFINITY, -0.25];
        assert_eq!(limiter.process_stereo(&mut bypass, None, true), 1.0);
        assert_eq!(bypass, [0.0, 0.25, 1.0, -1.0, 0.0, -0.25]);
    }

    #[test]
    fn incomplete_frames_are_sanitized_without_advancing_the_limiter() {
        let mut limiter = LiveLimiter::new(SAMPLE_RATE);
        let enabled = settings(Character::Hard, -6.0);
        for target in [None, enabled] {
            for input in [f32::NAN, f32::INFINITY, -2.0, 0.25] {
                let mut incomplete = [input];
                assert_eq!(limiter.process_stereo(&mut incomplete, target, true), 1.0);
                assert_eq!(incomplete[0], sanitize_sample(input));
            }
        }
        let mut impulse = vec![0.0; 2048];
        impulse[..2].fill(0.125);
        limiter.process_stereo(&mut impulse, enabled, false);
        assert_eq!(
            impulse
                .as_chunks::<2>()
                .0
                .iter()
                .position(|frame| frame[0] != 0.0),
            Some(Character::Hard.lookahead_frames(SAMPLE_RATE))
        );
    }
}
