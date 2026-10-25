//! The consensus a Link participant holds: which session it stands in,
//! which tempo that session runs at, and how this machine's clock maps
//! onto the session's. Pure arithmetic over wire values, so every rule
//! can be tried without a network.

use super::wire::{MICRO_BEATS_PER_BEAT, StartStop, Timeline, micros_per_beat};

/// Two session clocks less than this far apart are the same time to the
/// ear; the lower session id is the one that yields.
const SESSION_EPSILON_MICROS: i64 = 500_000;

/// The session this machine stands in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Session {
    pub id: [u8; 8],
    pub timeline: Timeline,
    pub start_stop: StartStop,
    /// Ghost time is this machine's host time plus this offset. A session
    /// this machine founded keeps the offset it started with; one it
    /// joined keeps the offset measured against the founder.
    pub host_offset: i64,
}

impl Session {
    /// Found a session of one's own, with the grid already standing where
    /// the score is: `cycle` is where the score's own grid is at `ghost`,
    /// so nothing moves when the session begins.
    pub fn found(id: [u8; 8], bpm: f64, cycle: f64, ghost: i64) -> Self {
        let beat_origin = (cycle * 4.0 * MICRO_BEATS_PER_BEAT as f64) as i64;
        Self {
            id,
            timeline: Timeline {
                micros_per_beat: micros_per_beat(bpm),
                beat_origin,
                time_origin: ghost,
            },
            start_stop: StartStop::default(),
            host_offset: 0,
        }
    }

    /// Change the tempo from this machine, keeping the beat the grid is
    /// on and taking priority over what the session had.
    ///
    /// The beat origin moves to at least one beat past the old one: the
    /// origin is how the session orders competing timelines, so a change
    /// must not arrive looking older than what it replaces.
    pub fn initiate_tempo(&mut self, wanted_bpm: f64, ghost: i64) {
        let micros_per_beat = micros_per_beat(wanted_bpm);
        let beat_now = self.timeline.beat_at(ghost);
        let beat_origin = beat_now.max(
            self.timeline
                .beat_origin
                .saturating_add(MICRO_BEATS_PER_BEAT),
        );
        let time_origin = self.timeline.time_at(beat_now).saturating_add(
            beat_origin
                .saturating_sub(beat_now)
                .saturating_mul(micros_per_beat)
                / MICRO_BEATS_PER_BEAT,
        );
        self.timeline = Timeline {
            micros_per_beat,
            beat_origin,
            time_origin,
        };
    }

    /// Take a timeline heard from the session, when it outranks the one
    /// held. Whether it was taken says if the tempo moved.
    pub fn adopt_timeline(&mut self, timeline: Timeline) -> bool {
        if timeline.micros_per_beat < 1 || timeline.beat_origin <= self.timeline.beat_origin {
            return false;
        }
        self.timeline = timeline;
        true
    }

    /// Take a start and stop heard from the session when it is newer than
    /// the one held.
    pub fn observe_start_stop(&mut self, start_stop: StartStop) -> bool {
        if start_stop.timestamp <= self.start_stop.timestamp {
            return false;
        }
        self.start_stop = start_stop;
        true
    }

    /// Tell the session this machine started or stopped. The timestamp is
    /// this machine's ghost now, which outruns every decision already
    /// heard - unless another participant started at the very same
    /// microsecond, and then the ordering does not matter.
    pub fn set_playing(&mut self, playing: bool, ghost: i64) {
        self.start_stop = StartStop {
            playing,
            beat: self.timeline.beat_at(ghost),
            timestamp: ghost,
        };
    }

    /// Stand in a session another participant founded, on its grid and
    /// its clock.
    pub fn join(&mut self, session: &Session) {
        self.id = session.id;
        self.timeline = session.timeline;
        self.start_stop = session.start_stop;
        self.host_offset = session.host_offset;
    }

    /// Found the session stood in, keeping the grid where it is: the
    /// tempo, the phase and the clock all stay, under this participant's
    /// own id, so the session has a founder to measure against again.
    pub fn refound(&mut self, id: [u8; 8]) {
        self.id = id;
    }

    /// A ghost time for a host time.
    pub fn ghost_at(&self, host: i64) -> i64 {
        host + self.host_offset
    }
}

/// Whether a measured session should replace the one stood in: a session
/// whose clock runs ahead is the one to be on, and of two that agree the
/// lower id wins, so every participant reaches the same answer whatever
/// order the sessions reached them in.
pub fn should_join(
    current_id: [u8; 8],
    current_offset: i64,
    other_id: [u8; 8],
    other_offset: i64,
) -> bool {
    if current_id == other_id {
        return false;
    }
    let ghost_diff = other_offset.saturating_sub(current_offset);
    ghost_diff > SESSION_EPSILON_MICROS
        || (ghost_diff.abs() < SESSION_EPSILON_MICROS && other_id < current_id)
}

/// Where a grid is, in cycles of four beats, at a ghost time.
pub fn cycle_of(timeline: &Timeline, ghost: i64) -> f64 {
    timeline.beat_at(ghost) as f64 / (4.0 * MICRO_BEATS_PER_BEAT as f64)
}

/// The mean of two clock samples. Halves each sample first, so a pair
/// near `i64::MAX` averages exactly as `(a + b) / 2` would, which also
/// rounds toward zero.
fn mean(a: i64, b: i64) -> i64 {
    a / 2 + b / 2 + (a % 2 + b % 2) / 2
}

/// The median of the clock samples a measurement gathered. An empty set
/// has no median, and an even count averages its middle pair.
pub fn median(samples: &mut [i64]) -> Option<i64> {
    if samples.is_empty() {
        return None;
    }
    samples.sort_unstable();
    let middle = samples.len() / 2;
    Some(if samples.len() % 2 == 1 {
        samples[middle]
    } else {
        mean(samples[middle - 1], samples[middle])
    })
}

#[cfg(test)]
mod tests {
    use super::super::wire::bpm;
    use super::*;

    const GHOST: i64 = 10_000_000_000;

    fn session_at_120() -> Session {
        Session::found([1; 8], 120.0, 0.0, GHOST)
    }

    #[test]
    fn refounding_keeps_the_grid_and_the_clock() {
        let mut session = session_at_120();
        session.join(&Session::found([2; 8], 90.0, 1.0, GHOST));
        let (timeline, offset) = (session.timeline, session.host_offset);
        session.refound([3; 8]);
        assert_eq!(session.id, [3; 8]);
        assert_eq!(session.timeline, timeline);
        assert_eq!(session.host_offset, offset);
    }

    #[test]
    fn a_founded_session_starts_where_the_score_is() {
        // The score is mid-bar, three and a half cycles in.
        let session = Session::found([1; 8], 120.0, 3.5, GHOST);
        assert!((cycle_of(&session.timeline, GHOST) - 3.5).abs() < 1e-6);
        assert!((bpm(session.timeline.micros_per_beat) - 120.0).abs() < 0.01);
    }

    #[test]
    fn a_tempo_change_keeps_the_beat_and_takes_priority() {
        let mut session = session_at_120();
        let beat_before = session.timeline.beat_at(GHOST + 1_000_000);
        let origin_before = session.timeline.beat_origin;
        session.initiate_tempo(80.0, GHOST + 1_000_000);
        // Same beat at the moment of the change, on a grid that outranks
        // the one it replaced.
        assert_eq!(session.timeline.beat_at(GHOST + 1_000_000), beat_before);
        assert!(session.timeline.beat_origin > origin_before);
        assert!(session.timeline.beat_origin >= beat_before);
        assert!((bpm(session.timeline.micros_per_beat) - 80.0).abs() < 0.01);
        // An absurd asked tempo is held to the protocol's range.
        session.initiate_tempo(1.0, GHOST + 2_000_000);
        assert!(bpm(session.timeline.micros_per_beat) >= super::super::wire::MIN_BPM);
    }

    #[test]
    fn a_timeline_is_taken_only_when_it_outranks_the_one_held() {
        let mut session = session_at_120();
        let heard = Timeline {
            micros_per_beat: 500_000,
            beat_origin: session.timeline.beat_origin + 1,
            time_origin: session.timeline.time_origin,
        };
        assert!(session.adopt_timeline(heard));
        // The same timeline again, or an older one, changes nothing.
        assert!(!session.adopt_timeline(heard));
        assert!(!session.adopt_timeline(Timeline {
            beat_origin: 0,
            ..heard
        }));
        // A grid without a tempo is not a grid.
        assert!(!session.adopt_timeline(Timeline {
            micros_per_beat: 0,
            beat_origin: heard.beat_origin + 10,
            time_origin: 0,
        }));
    }

    #[test]
    fn the_newer_transport_decision_wins() {
        let mut session = session_at_120();
        assert!(session.observe_start_stop(StartStop {
            playing: true,
            beat: 0,
            timestamp: 500
        }));
        assert!(session.start_stop.playing);
        // Older news is no news, even when it says stopped.
        assert!(!session.observe_start_stop(StartStop {
            playing: false,
            beat: 0,
            timestamp: 400
        }));
        assert!(session.start_stop.playing);
        assert!(session.observe_start_stop(StartStop {
            playing: false,
            beat: 0,
            timestamp: 600
        }));
        assert!(!session.start_stop.playing);
    }

    #[test]
    fn a_session_ahead_in_time_is_joined_and_a_lower_id_breaks_ties() {
        let higher = [9; 8];
        let lower = [5; 8];
        // Two seconds ahead: theirs.
        assert!(should_join(higher, 0, lower, 2_000_000));
        // Two seconds behind: ours stays.
        assert!(!should_join(higher, 0, lower, -2_000_000));
        // Within half a second, the lower id is the session to be on,
        // whichever side is asking.
        assert!(should_join(higher, 0, lower, 100_000));
        assert!(!should_join(lower, 100_000, higher, 0));
        // Exactly on the boundary counts as a tie only below it.
        assert!(!should_join(higher, 0, lower, SESSION_EPSILON_MICROS));
        // A session is never joined with itself.
        assert!(!should_join(higher, 0, higher, 9_000_000));
    }

    #[test]
    fn joining_puts_a_session_on_the_founders_grid_and_clock() {
        let mut ours = session_at_120();
        let mut theirs = Session::found([7; 8], 120.0, 0.0, 5_000_000_000);
        theirs.host_offset = 1_234_567;
        theirs.initiate_tempo(96.0, 5_500_000_000);
        ours.join(&theirs);
        assert_eq!(ours.id, theirs.id);
        assert_eq!(ours.timeline, theirs.timeline);
        assert_eq!(ours.host_offset, 1_234_567);
        assert_eq!(ours.ghost_at(1_000), 1_235_567);
    }

    #[test]
    fn the_median_of_the_samples_is_the_offset() {
        assert_eq!(median(&mut Vec::new()), None);
        assert_eq!(median(&mut [3]), Some(3));
        assert_eq!(median(&mut [1, 9, 4]), Some(4));
        assert_eq!(median(&mut [1, 2, 8, 9]), Some(5));
        // Already sorted, reversed, whatever: the answer is the middle.
        assert_eq!(median(&mut [-40, -50, -30]), Some(-40));
    }

    #[test]
    fn the_median_averages_a_middle_pair_that_would_overflow() {
        let mut high = [i64::MAX - 2, i64::MAX - 2, i64::MAX, i64::MAX];
        assert_eq!(median(&mut high), Some(i64::MAX - 1));
        let mut low = [i64::MIN, i64::MIN, i64::MIN + 2, i64::MIN + 2];
        assert_eq!(median(&mut low), Some(i64::MIN + 1));
    }
}
