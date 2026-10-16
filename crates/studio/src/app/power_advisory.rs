//! A footer hint when audio struggles while the CPU saves power.
//!
//! A power-saving mode keeps the clock low and changes it often, so the same
//! score needs much more of each callback and some callbacks miss their
//! deadline. The power mode is read only after the trouble lasts, never at
//! startup.

use std::time::{Duration, Instant};

use super::StudioSnapshot;

/// Smoothed DSP load, in basis points, that counts as trouble on its own.
const HIGH_LOAD: u64 = 7_000;
/// How long the trouble lasts before the power mode is read.
const SUSTAIN: Duration = Duration::from_secs(3);
/// How long without trouble clears the hint.
const QUIET: Duration = Duration::from_secs(5);
/// How often a known power mode is read again while the trouble lasts.
const RECHECK: Duration = Duration::from_secs(30);

pub(super) const MESSAGE: &str =
    "Audio struggling · the CPU is saving power · set the power mode to performance";

#[derive(Default)]
pub(super) struct PowerAdvisory {
    /// The deadline-miss counter of the previous snapshot.
    misses: Option<u64>,
    trouble_since: Option<Instant>,
    trouble_at: Option<Instant>,
    checked_at: Option<Instant>,
    visible: bool,
}

impl PowerAdvisory {
    pub(super) fn message(&self) -> Option<&'static str> {
        self.visible.then_some(MESSAGE)
    }

    pub(super) fn reset(&mut self) -> bool {
        let changed = self.visible;
        *self = Self::default();
        changed
    }

    /// Returns whether the footer changed.
    pub(super) fn observe(&mut self, snapshot: &StudioSnapshot, now: Instant) -> bool {
        self.observe_with(snapshot, now, rustel_audio::power_mode::power_saving)
    }

    fn observe_with(
        &mut self,
        snapshot: &StudioSnapshot,
        now: Instant,
        power_saving: impl FnOnce() -> Option<bool>,
    ) -> bool {
        let was_visible = self.visible;
        if !snapshot.playing || snapshot.stopping {
            self.reset();
            return was_visible;
        }
        let Some(load) = snapshot
            .pressure
            .as_ref()
            .map(|pressure| &pressure.device.realtime_load)
            .filter(|load| load.publication != 0)
        else {
            return false;
        };
        // A counter that goes back belongs to a new stream: no miss.
        let missed = self
            .misses
            .replace(load.callbacks_over_100_percent)
            .is_some_and(|before| load.callbacks_over_100_percent > before);
        if missed || load.slow_load_basis_points >= HIGH_LOAD {
            self.trouble_since.get_or_insert(now);
            self.trouble_at = Some(now);
        } else if self
            .trouble_at
            .is_none_or(|at| now.saturating_duration_since(at) >= QUIET)
        {
            self.trouble_since = None;
            self.checked_at = None;
            self.visible = false;
        }
        // The trouble itself spans the sustain time, not just the time since
        // its first sign.
        let lasting = self
            .trouble_since
            .zip(self.trouble_at)
            .is_some_and(|(since, at)| at.saturating_duration_since(since) >= SUSTAIN);
        let due = self
            .checked_at
            .is_none_or(|at| now.saturating_duration_since(at) >= RECHECK);
        // An unknown mode is read again with the next snapshot.
        if lasting
            && due
            && let Some(saving) = power_saving()
        {
            self.visible = saving;
            self.checked_at = Some(now);
        }
        self.visible != was_visible
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustel_audio::{LiveDeviceReport, RealtimeLoadSnapshot};
    use rustel_runtime::EnginePressureSnapshot;

    /// One second of audio: its smoothed load and whether a callback missed.
    #[derive(Clone, Copy)]
    struct Second {
        load: u64,
        missed: bool,
    }

    const HIGH: Second = Second {
        load: 8_000,
        missed: false,
    };
    const MISSING: Second = Second {
        load: 5_500,
        missed: true,
    };
    const CALM: Second = Second {
        load: 3_000,
        missed: false,
    };

    fn snapshot(load: u64, misses: u64) -> StudioSnapshot {
        StudioSnapshot {
            playing: true,
            pressure: Some(EnginePressureSnapshot {
                device: LiveDeviceReport {
                    realtime_load: RealtimeLoadSnapshot {
                        publication: 1,
                        slow_load_basis_points: load,
                        callbacks_over_100_percent: misses,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// Feeds one snapshot a second and counts the power mode reads.
    fn run(seconds: &[Second], saving: Option<bool>) -> (PowerAdvisory, Vec<bool>, usize) {
        let start = Instant::now();
        let mut advisory = PowerAdvisory::default();
        let mut shown = Vec::new();
        let mut reads = 0;
        let mut misses = 100;
        for (at, second) in seconds.iter().enumerate() {
            misses += u64::from(second.missed);
            let now = start + Duration::from_secs(at as u64);
            advisory.observe_with(&snapshot(second.load, misses), now, || {
                reads += 1;
                saving
            });
            shown.push(advisory.message().is_some());
        }
        (advisory, shown, reads)
    }

    #[test]
    fn lasting_high_load_or_recurring_misses_in_power_saving_mode_show_the_hint() {
        let (_, shown, reads) = run(&[HIGH; 6], Some(true));
        assert_eq!(shown, [false, false, false, true, true, true]);
        assert_eq!(reads, 1);
        let misses = [CALM, MISSING, CALM, MISSING, CALM, MISSING];
        let (_, shown, _) = run(&misses, Some(true));
        assert_eq!(shown, [false, false, false, false, false, true]);
    }

    #[test]
    fn a_short_burst_or_a_calm_moderate_load_never_reads_the_power_mode() {
        let calm_moderate = Second {
            load: 6_000,
            missed: false,
        };
        let seconds = [MISSING, MISSING, CALM, CALM, CALM, CALM, CALM, CALM];
        assert_eq!(run(&seconds, Some(true)).2, 0);
        assert_eq!(run(&[calm_moderate; 8], Some(true)).2, 0);
    }

    #[test]
    fn performance_mode_and_an_unknown_mode_stay_quiet() {
        let (_, shown, reads) = run(&[HIGH; 6], Some(false));
        assert!(shown.iter().all(|shown| !shown));
        assert_eq!(reads, 1);
        let (_, shown, reads) = run(&[HIGH; 6], None);
        assert!(shown.iter().all(|shown| !shown));
        assert_eq!(reads, 3, "an unknown mode is read again");
    }

    #[test]
    fn the_hint_clears_after_quiet_seconds_or_when_playback_stops() {
        let mut seconds = vec![HIGH; 4];
        seconds.extend([CALM; 6]);
        let (_, shown, _) = run(&seconds, Some(true));
        assert_eq!(
            shown,
            [
                false, false, false, true, true, true, true, true, false, false
            ]
        );
        let (mut playing, _, _) = run(&[HIGH; 4], Some(true));
        let stopped = StudioSnapshot {
            playing: false,
            ..snapshot(8_000, 0)
        };
        assert!(playing.observe_with(&stopped, Instant::now(), || Some(true)));
        assert!(playing.message().is_none());
    }
}
