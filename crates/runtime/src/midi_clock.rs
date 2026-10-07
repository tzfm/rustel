//! Clock sync over MIDI: ticks out to the hardware on the table, and a
//! follower that keeps the scheduler on a clock coming in.
//!
//! MIDI clock is 24 pulses per beat; with four beats to the cycle
//! that is 96 ticks per cycle. Out, the ticks are scheduled a little ahead
//! on the MIDI sender's own thread, from the scheduler's cycle-to-time
//! mapping, so they carry the engine's timing rather than the interface
//! thread's. In, the ticks are timed as they arrive, the interval smoothed,
//! and the estimate - tempo and cycle position since Start - is what the
//! engine steers the scheduler towards.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustel_midi::MidiMessage;
use rustel_midi::MidiSender;
use rustel_midi::input::{MidiEvent, MidiListener};

use crate::midi_bridge::MAX_OFFSET_SECS;

/// MIDI clock ticks per cycle: 24 per beat, four beats to the cycle.
pub const TICKS_PER_CYCLE: f64 = 96.0;
/// How far ahead ticks are handed to the sender.
const SCHEDULE_AHEAD: Duration = Duration::from_millis(120);
/// The follower stops trusting a clock that has gone quiet this long.
const CLOCK_TIMEOUT: Duration = Duration::from_millis(500);

/// Clock out: ticks to one port, Start and Stop with the transport.
pub struct ClockOut {
    sender: MidiSender,
    port: String,
    running: bool,
    /// Whether a Start has gone out on this port, which is what makes a later
    /// resume a Continue rather than another Start.
    ever_started: bool,
    /// The next tick index to schedule, in ticks since cycle zero.
    next_tick: i64,
    scheduled_until: Instant,
}

impl ClockOut {
    pub fn open(port: &str) -> Result<Self, String> {
        let sender = MidiSender::open(Some(port))?;
        Ok(Self {
            port: sender.port_name().to_owned(),
            sender,
            running: false,
            ever_started: false,
            next_tick: 0,
            scheduled_until: Instant::now(),
        })
    }

    pub fn port(&self) -> &str {
        &self.port
    }

    /// Schedule the ticks due before `now + SCHEDULE_AHEAD`. `cycle_now` is
    /// the scheduler's cycle at `now`, `cps` its rate; when `playing` turns
    /// on a Start goes out and the tick count restarts from cycle zero's
    /// grid, when it turns off a Stop goes out.
    pub fn advance(&mut self, playing: bool, cycle_now: f64, cps: f64, now: Instant) {
        if playing != self.running {
            self.running = playing;
            let message = transport_message(playing, self.ever_started, cycle_now);
            let _ = self.sender.send_at(now, message);
            if playing {
                self.ever_started = true;
                self.next_tick = (cycle_now * TICKS_PER_CYCLE).ceil() as i64;
                self.scheduled_until = now;
            }
        }
        if !playing || !(cps.is_finite() && cps > 0.0) {
            return;
        }
        let horizon = now + SCHEDULE_AHEAD;
        if self.scheduled_until >= horizon {
            return;
        }
        let seconds_per_tick = 1.0 / (cps * TICKS_PER_CYCLE);
        let mut batch = Vec::with_capacity(16);
        loop {
            let tick_cycle = self.next_tick as f64 / TICKS_PER_CYCLE;
            let offset = (tick_cycle - cycle_now) / cps;
            let due = if offset >= 0.0 {
                // Very small positive tempos can exceed Duration's range.
                // Clamping is safe here: such ticks remain beyond the horizon.
                now + Duration::from_secs_f64(offset.clamp(0.0, MAX_OFFSET_SECS))
            } else {
                now
            };
            if due > horizon {
                break;
            }
            batch.push((due, MidiMessage::clock()));
            // Extreme tempos can saturate the initial tick index. Keep the
            // increment bounded too; the batch cap still ends the pass.
            self.next_tick = self.next_tick.saturating_add(1);
            if batch.len() >= 64 {
                break;
            }
        }
        if !batch.is_empty() {
            let _ = self.sender.send_batch_at(&batch);
        }
        // Bound the conversion before limiting the batch span to the horizon.
        let batch_span = (seconds_per_tick * 64.0).clamp(0.0, MAX_OFFSET_SECS);
        self.scheduled_until = horizon.min(now + Duration::from_secs_f64(batch_span));
    }
}

/// The transport message a change of `playing` sends. Picking up from inside
/// the pattern is Continue, so the hardware resumes its own tick count rather
/// than going back to the top; a first start, or one from cycle zero, is
/// Start.
fn transport_message(playing: bool, ever_started: bool, cycle_now: f64) -> MidiMessage {
    if !playing {
        return MidiMessage::stop();
    }
    if ever_started && cycle_now.fract().abs() > 0.0 {
        MidiMessage::cont()
    } else {
        MidiMessage::start()
    }
}

impl Drop for ClockOut {
    fn drop(&mut self) {
        if self.running {
            let _ = self.sender.send_at(Instant::now(), MidiMessage::stop());
        }
    }
}

/// Ticks kept for the tempo estimate: two cycles' worth.
const ESTIMATE_TICKS: usize = 192;
/// Fewer ticks than this and there is no tempo to speak of.
const ESTIMATE_MIN_TICKS: usize = 24;

/// What the follower has heard.
#[derive(Clone, Debug, Default)]
struct Heard {
    running: bool,
    /// A Stop has been heard, and no Start or Continue since. Distinct from
    /// `!running`, which is also the state before anything arrives: joining a
    /// clock that is already going must follow it, while one that was told to
    /// stop must not.
    stopped: bool,
    /// A Start has been heard, so `ticks` counts from a position the clock
    /// itself declared rather than from whenever we began listening.
    positioned: bool,
    /// Ticks since the last Start (or Continue).
    ticks: u64,
    /// When the last ticks arrived. The tempo is the span of this window
    /// over its count - ticks that a sender bunches up cancel out, where
    /// timing each gap would not.
    stamps: std::collections::VecDeque<Instant>,
}

impl Heard {
    fn observe(&mut self, event: MidiEvent, now: Instant) {
        match event {
            MidiEvent::Start => {
                self.running = true;
                self.stopped = false;
                self.positioned = true;
                self.ticks = 0;
            }
            MidiEvent::Continue => {
                self.running = true;
                self.stopped = false;
            }
            MidiEvent::Stop => {
                self.running = false;
                self.stopped = true;
            }
            MidiEvent::Clock => {
                // A long silence is a restart, not a slow tempo.
                if let Some(last) = self.stamps.back()
                    && now.duration_since(*last).as_secs_f64() > 0.25
                {
                    self.stamps.clear();
                }
                if self.stamps.len() >= ESTIMATE_TICKS {
                    self.stamps.pop_front();
                }
                self.stamps.push_back(now);
                if self.running {
                    self.ticks += 1;
                }
            }
            _ => {}
        }
    }

    /// Seconds per tick, over the window.
    fn interval(&self) -> Option<f64> {
        if self.stamps.len() < ESTIMATE_MIN_TICKS {
            return None;
        }
        let first = *self.stamps.front()?;
        let last = *self.stamps.back()?;
        let span = last.duration_since(first).as_secs_f64();
        (span > 0.0).then(|| span / (self.stamps.len() - 1) as f64)
    }

    fn last_tick(&self) -> Option<Instant> {
        self.stamps.back().copied()
    }
}

/// The clock as the engine should follow it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClockEstimate {
    pub cps: f64,
    /// Cycles since the clock's Start, as of the moment asked. Only
    /// meaningful when `positioned`.
    pub cycle: f64,
    /// The clock is going: a Start was heard, or ticks are simply arriving.
    pub running: bool,
    /// A Start was heard, so `cycle` is anchored to a position the clock
    /// declared. Joining a clock that was already running gives a tempo but
    /// no position, and a follower must not jump the score onto a count that
    /// started at an arbitrary moment.
    pub positioned: bool,
}

/// Clock in: a listener on one port, timed as the ticks arrive.
pub struct ClockIn {
    _listener: MidiListener,
    port: String,
    heard: Arc<Mutex<Heard>>,
}

impl ClockIn {
    pub fn open(port: &str) -> Result<Self, String> {
        let heard = Arc::new(Mutex::new(Heard::default()));
        let sink = Arc::clone(&heard);
        let listener =
            MidiListener::open_filtered(Some(port), midir::Ignore::None, move |_, event, _| {
                if matches!(
                    event,
                    MidiEvent::Clock | MidiEvent::Start | MidiEvent::Continue | MidiEvent::Stop
                ) && let Ok(mut heard) = sink.lock()
                {
                    heard.observe(event, Instant::now());
                }
            })?;
        Ok(Self {
            port: listener.port_name().to_owned(),
            _listener: listener,
            heard,
        })
    }

    pub fn port(&self) -> &str {
        &self.port
    }

    /// The clock's tempo and position now, once it has been heard.
    pub fn estimate(&self, now: Instant) -> Option<ClockEstimate> {
        let heard = self.heard.lock().ok()?;
        let interval = heard.interval()?;
        let last = heard.last_tick()?;
        if now.duration_since(last) > CLOCK_TIMEOUT {
            return None;
        }
        let since = now.duration_since(last).as_secs_f64() / interval;
        Some(ClockEstimate {
            cps: 1.0 / (interval * TICKS_PER_CYCLE),
            cycle: (heard.ticks as f64 + since.min(1.0)) / TICKS_PER_CYCLE,
            // Ticks with no Start behind them are still a clock to follow;
            // only an explicit Stop rules one out.
            running: !heard.stopped,
            positioned: heard.positioned,
        })
    }
}

/// Beats per minute of a clock, four beats to the cycle.
pub fn bpm(cps: f64) -> f64 {
    cps * 240.0
}

/// How often the follower steers the scheduler.
pub const FOLLOW_INTERVAL: Duration = Duration::from_millis(200);
/// Phase error beyond which the follower jumps rather than bends the tempo.
const JUMP_CYCLES: f64 = 0.25;
/// How far one step bends the tempo towards the clock.
const MAX_BEND: f64 = 0.03;
/// Tempo and phase both within this and the scheduler is already there.
const LOCK_TEMPO: f64 = 0.0005;
const LOCK_PHASE: f64 = 0.003;
/// A steer that moved this little counts as locked to the interface.
const LOCKED_PHASE: f64 = 0.02;
/// How far the tempo may be off and still count as locked when there is no
/// phase to compare against. Looser than `LOCK_TEMPO` on purpose: a measured
/// MIDI clock jitters by more than 0.05 % from one window to the next, so
/// holding a follower to that would report lock and loss on every pass.
const LOCKED_TEMPO: f64 = 0.02;

/// What the scheduler should do to join an outside clock.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Steer {
    /// Tempo and phase already agree; leave the scheduler alone.
    Hold,
    /// Retime the scheduler onto this tempo and cycle position.
    To { cps: f64, cycle: f64 },
}

/// The signed distance from `actual` to `wanted`, folded into half a cycle:
/// the clock's Start is its cycle zero, and the score's grid need only agree
/// modulo one cycle.
fn phase_error(wanted: f64, actual: f64) -> f64 {
    let mut error = (wanted - actual) % 1.0;
    if error > 0.5 {
        error -= 1.0;
    } else if error < -0.5 {
        error += 1.0;
    }
    error
}

/// Decide how to join `estimate` from where the scheduler is now, and whether
/// that counts as locked. Bend the tempo a little while the phase is close,
/// jump when it is far.
///
/// Pure, and deliberately so: the studio and the live command follow the same
/// clock with the same arithmetic, and neither has to restate the thresholds.
/// Applying the decision needs `Session::retime` and a re-query, which only
/// the caller has.
pub fn steer(estimate: ClockEstimate, cycle_now: f64, cps_now: f64) -> (Steer, bool) {
    // A clock joined mid-run has a tempo but no declared position, so there
    // is no phase to agree with: match its rate and leave the score's own
    // position alone rather than jumping onto a count that began at an
    // arbitrary moment.
    //
    // The deadband here is also what keeps the follow quiet. A measured clock
    // jitters by a fraction of a percent between windows, and retime plus
    // re-query on every one of those would re-trigger onsets that have
    // already sounded - a stutter at the follow interval. Within the band the
    // scheduler is left alone entirely.
    if !estimate.positioned {
        let tempo_off = (estimate.cps / cps_now.max(1e-9) - 1.0).abs();
        if tempo_off < LOCKED_TEMPO {
            return (Steer::Hold, true);
        }
        return (
            Steer::To {
                cps: estimate.cps,
                cycle: cycle_now,
            },
            false,
        );
    }
    let error = phase_error(estimate.cycle, cycle_now);
    let jump = error.abs() > JUMP_CYCLES;
    let cps = if jump {
        estimate.cps
    } else {
        estimate.cps * (1.0 + (error * 0.5).clamp(-MAX_BEND, MAX_BEND))
    };
    let tempo_off = (cps / cps_now.max(1e-9) - 1.0).abs();
    if tempo_off < LOCK_TEMPO && error.abs() < LOCK_PHASE {
        return (Steer::Hold, true);
    }
    let steer = Steer::To {
        cps,
        cycle: if jump { cycle_now + error } else { cycle_now },
    };
    (steer, error.abs() < LOCKED_PHASE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ticks out of a virtual port, back in through the follower: the
    /// whole loop, on this machine's MIDI stack. Skipped where a virtual
    /// port cannot be made. WinMM cannot create virtual MIDI ports, so the
    /// test is not compiled on Windows.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn ticks_sent_come_back_as_the_same_tempo() {
        let Ok(sender) = MidiSender::create_virtual("rustel clock test") else {
            eprintln!("no virtual MIDI port here; skipping");
            return;
        };
        let mut out = ClockOut {
            port: sender.port_name().to_owned(),
            sender,
            running: false,
            ever_started: false,
            next_tick: 0,
            scheduled_until: Instant::now(),
        };
        std::thread::sleep(Duration::from_millis(200));
        let Ok(input) = ClockIn::open("rustel clock test") else {
            eprintln!("cannot listen to the virtual port; skipping");
            return;
        };
        // 0.5 cps = 120 bpm at 96 ticks per cycle, so a tick is due every
        // 20.8 ms. The loop sleeps 5 ms: a 20 ms sleep can cross two tick
        // boundaries in one iteration, and two ticks with one stamp read as a
        // faster clock. Three seconds is about 144 ticks, inside the 192-tick
        // estimate window, so the mean spans the whole run.
        let start = Instant::now();
        loop {
            let now = Instant::now();
            let elapsed = now.duration_since(start).as_secs_f64();
            if elapsed > 3.0 {
                break;
            }
            out.advance(true, elapsed * 0.5, 0.5, now);
            std::thread::sleep(Duration::from_millis(5));
        }
        let estimate = input
            .estimate(Instant::now())
            .expect("the follower heard ticks");
        assert!(estimate.running);
        // Ten percent. The assertion checks that the sender and the follower
        // agree on ticks per cycle: an error there is a factor of four, and a
        // wrong beat division is a factor of two. It is not a wall-clock
        // precision test: `thread::sleep` drives it.
        assert!(
            (bpm(estimate.cps) - 120.0).abs() < 12.0,
            "{}",
            bpm(estimate.cps)
        );
        // 0.5 cps over the three seconds above, give or take a stall.
        assert!(
            estimate.cycle > 1.2 && estimate.cycle < 1.8,
            "{}",
            estimate.cycle
        );
        out.advance(false, 0.0, 0.5, Instant::now());
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !input
                .estimate(Instant::now())
                .map(|e| e.running)
                .unwrap_or(false)
        );
    }

    #[test]
    fn the_follower_reads_tempo_and_position_from_ticks() {
        let mut heard = Heard::default();
        let start = Instant::now();
        heard.observe(MidiEvent::Start, start);
        // 120 bpm: 24 ticks a beat, 500 ms a beat, 20.833 ms a tick.
        let tick = Duration::from_micros(20_833);
        for index in 1..=96 {
            heard.observe(MidiEvent::Clock, start + tick * index);
        }
        assert!(heard.running);
        assert_eq!(heard.ticks, 96);
        let interval = heard.interval().unwrap();
        assert!((interval - 0.020833).abs() < 0.0005, "{interval}");
        let cps = 1.0 / (interval * TICKS_PER_CYCLE);
        assert!((bpm(cps) - 120.0).abs() < 1.5, "{}", bpm(cps));
        heard.observe(MidiEvent::Stop, start + tick * 97);
        assert!(!heard.running);
        // A restart counts from zero again.
        heard.observe(MidiEvent::Start, start + tick * 200);
        assert_eq!(heard.ticks, 0);
    }

    #[test]
    fn a_resume_inside_the_pattern_continues_rather_than_restarts() {
        // The first start on a port is a Start, wherever the pattern is.
        assert_eq!(
            transport_message(true, false, 0.0).as_slice(),
            MidiMessage::start().as_slice()
        );
        assert_eq!(
            transport_message(true, false, 2.5).as_slice(),
            MidiMessage::start().as_slice()
        );
        assert_eq!(
            transport_message(false, true, 2.5).as_slice(),
            MidiMessage::stop().as_slice()
        );
        // Afterwards, coming back at cycle zero starts over, and coming back
        // mid-pattern continues.
        assert_eq!(
            transport_message(true, true, 3.0).as_slice(),
            MidiMessage::start().as_slice()
        );
        assert_eq!(
            transport_message(true, true, 2.5).as_slice(),
            MidiMessage::cont().as_slice()
        );
        // The three are distinct bytes, or the assertions above prove nothing.
        assert_ne!(
            MidiMessage::cont().as_slice(),
            MidiMessage::start().as_slice()
        );
        assert_ne!(
            MidiMessage::stop().as_slice(),
            MidiMessage::start().as_slice()
        );
    }

    /// A clock out over a capture port, so `advance`'s arithmetic can be
    /// driven without hardware: the sender is the real one, only the wire is
    /// recorded. What these tests are after is the arithmetic, not the port.
    fn captured_clock(name: &str) -> (ClockOut, rustel_midi::CapturePort) {
        let capture = rustel_midi::CapturePort::new();
        let sender = MidiSender::with_port(Box::new(capture.clone()), name.to_owned());
        (
            ClockOut {
                port: name.to_owned(),
                sender,
                running: false,
                ever_started: false,
                next_tick: 0,
                scheduled_until: Instant::now(),
            },
            capture,
        )
    }

    /// Wait for the worker to have sent `count` messages, with the same
    /// patience the bridge's tests give an asynchronous sender.
    fn wait_for_messages(capture: &rustel_midi::CapturePort, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while capture.messages().len() < count {
            assert!(Instant::now() < deadline, "MIDI capture timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// The clock ticks among what was captured; everything else is transport.
    fn clock_ticks(capture: &rustel_midi::CapturePort) -> usize {
        capture
            .messages()
            .iter()
            .filter(|(_, bytes)| bytes.as_slice() == MidiMessage::clock().as_slice())
            .count()
    }

    /// A tempo small enough to put the next tick past every `Duration` there
    /// is must schedule nothing rather than panic computing when. The score's
    /// setCps accepts any finite rate above zero, so the clock-out arithmetic
    /// meets 1e-25 (next tick due ~1e23 s out) and 1e-300 as live inputs.
    #[test]
    fn an_absurdly_slow_tempo_schedules_no_ticks_rather_than_panicking() {
        // 5e-324 (the smallest subnormal) overflows the intermediates to
        // infinity, which the clamp must bring back too.
        for cps in [1e-25, 1e-300, f64::from_bits(1)] {
            let (mut out, capture) = captured_clock("absurdly slow");
            // Half a cycle and a hair, so the next grid tick is a hundredth of
            // a cycle out: at these tempos that is far past the horizon - and,
            // before the clamp, past the `u64` seconds a `Duration` holds.
            let now = Instant::now();
            out.advance(true, 0.50001, cps, now);
            // No tick is queued: the counter still names the first grid tick
            // after the Start, and the scheduled span stays in the lookahead.
            assert_eq!(out.next_tick, 49, "no tick is queued at cps = {cps}");
            assert!(out.scheduled_until <= now + SCHEDULE_AHEAD);
            wait_for_messages(&capture, 1);
            assert_eq!(
                clock_ticks(&capture),
                0,
                "no tick is due within the horizon at cps = {cps}"
            );
        }
    }

    /// A tempo so fast that a tick's span underflows to zero. The whole batch
    /// is due at once and the 64-slot cap ends the pass, from cycle zero and
    /// from a cycle past the i64 tick grid.
    #[test]
    fn an_absurdly_fast_tempo_sends_a_bounded_batch() {
        for cycle_now in [0.0, 1e20] {
            let (mut out, capture) = captured_clock("absurdly fast");
            out.advance(true, cycle_now, 1e30, Instant::now());
            // The Start of the transport change plus one full batch of ticks.
            wait_for_messages(&capture, 65);
            assert_eq!(clock_ticks(&capture), 64, "at cycle {cycle_now}");
        }
    }

    #[test]
    fn the_follower_bends_a_close_phase_and_jumps_a_far_one() {
        let at = |cps: f64, cycle: f64| ClockEstimate {
            cps,
            cycle,
            running: true,
            positioned: true,
        };

        // Already there: hold, and call it locked.
        let (decision, locked) = steer(at(0.5, 4.0), 4.0, 0.5);
        assert_eq!(decision, Steer::Hold);
        assert!(locked);

        // A tenth of a cycle off is close enough to bend: the position stays
        // and the tempo leans towards the clock, capped at MAX_BEND.
        let (decision, locked) = steer(at(0.5, 0.1), 0.0, 0.5);
        match decision {
            Steer::To { cps, cycle } => {
                assert_eq!(cycle, 0.0);
                assert!((cps - 0.5 * 1.03).abs() < 1e-9, "{cps}");
            }
            Steer::Hold => panic!("a tenth of a cycle off is not locked"),
        }
        assert!(!locked, "a tenth of a cycle is outside the locked window");

        // Four tenths off is past the jump threshold: take the clock's
        // position outright, at the clock's own tempo.
        let (decision, _) = steer(at(0.5, 0.4), 0.0, 0.5);
        match decision {
            Steer::To { cps, cycle } => {
                assert_eq!(cps, 0.5);
                assert!((cycle - 0.4).abs() < 1e-9, "{cycle}");
            }
            Steer::Hold => panic!("four tenths of a cycle off must move"),
        }

        // The phase folds modulo one cycle, so a clock nine cycles ahead of
        // the score bends exactly like one a tenth ahead.
        let (far, _) = steer(at(0.5, 9.1), 9.0, 0.5);
        let (near, _) = steer(at(0.5, 0.1), 0.0, 0.5);
        match (far, near) {
            (Steer::To { cps: a, .. }, Steer::To { cps: b, .. }) => {
                assert_eq!(a, b, "the same phase error bends the same");
            }
            _ => panic!("both should bend"),
        }
    }

    /// Joining a clock that is already running is the ordinary case: the
    /// master was playing before rustel started listening, so no Start was
    /// ever heard. There is a tempo to follow and no position to agree with.
    #[test]
    fn a_clock_joined_mid_run_is_followed_on_tempo_alone() {
        let mut heard = Heard::default();
        let start = Instant::now();
        // 120 bpm: 24 ticks a beat, 20.833 ms a tick.
        let tick = Duration::from_micros(20_833);
        for index in 1..=48 {
            heard.observe(MidiEvent::Clock, start + tick * index);
        }
        assert!(!heard.positioned, "no Start was heard");
        assert!(heard.interval().is_some(), "the tempo is still measurable");

        let estimate = ClockEstimate {
            cps: 0.5,
            cycle: 3.0,
            running: true,
            positioned: false,
        };
        // The score keeps its own position and takes the clock's tempo
        // outright, rather than bending towards a phase nobody declared.
        let (decision, locked) = steer(estimate, 7.25, 2.0);
        assert_eq!(
            decision,
            Steer::To {
                cps: 0.5,
                cycle: 7.25
            }
        );
        assert!(!locked);
        // Once the tempo agrees it holds, and that counts as locked.
        let (decision, locked) = steer(estimate, 7.25, 0.5);
        assert_eq!(decision, Steer::Hold);
        assert!(locked);
        // So does a tempo a measured clock's jitter away: holding here is what
        // stops every follow from re-querying, and re-triggering onsets that
        // have already sounded.
        let (decision, locked) = steer(estimate, 7.25, 0.505);
        assert_eq!(decision, Steer::Hold);
        assert!(locked);

        // An explicit Stop rules the clock out. Silence before any Start does
        // not, which is what makes joining a running clock work at all.
        heard.observe(MidiEvent::Stop, start + tick * 49);
        assert!(heard.stopped);
        heard.observe(MidiEvent::Start, start + tick * 50);
        assert!(!heard.stopped);
        assert!(heard.positioned);
        assert_eq!(heard.ticks, 0);
    }
}
