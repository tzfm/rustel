//! MIDI pads for scenes: a controller's buttons launch scores.
//!
//! Every MIDI input on the machine is listened to. A pad that has been
//! learnt switches to its scene and evaluates it - the two verbs the strip
//! already has, fired together. Learning is the gesture every musician
//! knows from a DAW: pick the scene, say *learn*, hit the pad.
//!
//! The mapping lives in the set's project file, so it travels with the
//! folder and follows a scene through renames.

use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use rustel_midi::input::{MidiEvent, MidiListener};
use serde::{Deserialize, Serialize};

/// One pad: a note on a channel. Stored in the set's project file next to
/// the scene it launches.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Pad {
    pub note: u8,
    pub channel: u8,
}

impl Pad {
    pub fn matches(self, note: u8, channel: u8) -> bool {
        self.note == note && self.channel == channel
    }

    /// A short label for the chip: `c1/10` - note name and channel.
    pub fn label(self) -> String {
        format!(
            "{}/{}",
            rustel_midi::input::note_name(self.note),
            self.channel
        )
    }
}

/// A pad press, as the UI thread sees it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PadPress {
    pub note: u8,
    pub channel: u8,
    pub velocity: u8,
}

/// A knob turned, as the UI thread sees it: the slider's controller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CcTurn {
    pub controller: u8,
    pub channel: u8,
    pub value: u8,
}

/// Every MIDI input on the machine, funnelled into one channel the draw
/// loop drains. The driver's threads only ever push into it.
pub struct PadListener {
    listeners: Vec<MidiListener>,
    ports: Vec<String>,
    presses: Receiver<PadPress>,
    sender: Sender<PadPress>,
    /// The knobs, on their own channel: a slider follows them.
    turns: Receiver<CcTurn>,
    turn_sender: Sender<CcTurn>,
    /// When any message last arrived on any port, in milliseconds since
    /// the UNIX epoch: an activity light, stamped on the driver's thread
    /// with a store and nothing else.
    last_message: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// The last few messages, readable, newest last.
    recent: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<String>>>,
    /// Raised only when a readable line reaches `recent`. The UI compares
    /// this generation on each turn instead of locking and cloning the feed
    /// merely to discover whether a stopped frame needs repainting.
    recent_generation: std::sync::Arc<std::sync::atomic::AtomicU64>,
    observed_recent_generation: u64,
    activity_visible: bool,
    /// The pad that launched last, and when: a physical press reaches the
    /// studio twice when a controller exposes both a hardware port and a
    /// software mirror (Maschine's "Virtual Input"), each carrying the same
    /// NoteOn a few milliseconds apart. Two immediate rewinds back to back
    /// restart the kick mid-decay - the audible "little double". One press
    /// is one launch: an identical pad inside the window is a mirror, not
    /// a musician, and goes no further. Distinct pads, and the same pad
    /// after the window, pass untouched.
    ///
    /// A few slots, not one: two pads hit together arrive as A, B, then
    /// their mirrors A', B' - with one slot B had already replaced A when
    /// A' came, and both mirrors launched.
    last_press: std::sync::Arc<std::sync::Mutex<RecentPresses>>,
    /// HARNESS. Controllers the e2e suite plugs in: counted by
    /// [`Self::open_count`] so a learn can be armed on a machine with no
    /// hardware, and never opened - their messages arrive through
    /// [`Self::harness_message`], the same path a real port's driver
    /// callback takes.
    #[cfg(feature = "harness")]
    harness_ports: Vec<String>,
}

/// The last few pad presses the mirror gate remembers, with when each
/// arrived in milliseconds since the UNIX epoch. Fixed-size: the driver's
/// callback never allocates.
#[derive(Clone, Copy, Debug)]
struct RecentPresses {
    /// `None` until a press fills the slot: an empty slot must never match
    /// (a zeroed one would read as note 0, channel 0, pressed at time 0,
    /// which a monotonic clock that starts at 0 can reach).
    slots: [Option<(Pad, u64)>; RECENT_PRESSES],
    /// The slot the next press overwrites (the oldest).
    next: usize,
}

/// How many pads the mirror gate remembers at once: more than two hands
/// have fingers on a launch row at one instant.
const RECENT_PRESSES: usize = 8;

impl RecentPresses {
    const fn new() -> Self {
        Self {
            slots: [None; RECENT_PRESSES],
            next: 0,
        }
    }

    /// Whether an identical pad arrived inside the window; records this
    /// press either way (a mirror keeps the original's time, a press takes
    /// the oldest slot).
    fn is_mirror(&mut self, pad: Pad, now: u64) -> bool {
        if self.slots.iter().flatten().any(|(last, at)| {
            now.saturating_sub(*at) <= DOUBLE_PRESS_WINDOW_MILLIS
                && last.matches(pad.note, pad.channel)
        }) {
            return true;
        }
        self.slots[self.next] = Some((pad, now));
        self.next = (self.next + 1) % RECENT_PRESSES;
        false
    }
}

/// Milliseconds on a monotonic clock, for the mirror gate. The wall clock
/// steps - NTP, a laptop waking - and a step backwards would hold the gate
/// shut against the last pad for as long as the step, a step forwards let a
/// mirror through; the gate compares a 40 ms window and needs a clock that
/// only moves one way.
fn gate_millis() -> u64 {
    static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let epoch = *EPOCH.get_or_init(std::time::Instant::now);
    u64::try_from(epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// How long an identical (note, channel) press is treated as the same
/// physical press arriving on another port. A fast drummer re-hits a pad
/// on the order of 80-100 ms; mirrors deliver within a few ms of the
/// original. The window must sit well below the human re-hit floor.
const DOUBLE_PRESS_WINDOW_MILLIS: u64 = 40;

/// How many recent messages the mixer can show.
const RECENT_MESSAGES: usize = 24;

/// One message as a musician reads it.
fn describe(port: &str, event: MidiEvent) -> Option<String> {
    let line = match event {
        MidiEvent::NoteOn {
            channel,
            note,
            velocity,
        } if velocity > 0 => format!(
            "{port} · note {} ({note}) v{velocity} ch{channel}",
            rustel_midi::input::note_name(note),
        ),
        MidiEvent::NoteOn { channel, note, .. } | MidiEvent::NoteOff { channel, note, .. } => {
            format!(
                "{port} · off {} ({note}) ch{channel}",
                rustel_midi::input::note_name(note),
            )
        }
        MidiEvent::ControlChange {
            channel,
            controller,
            value,
        } => format!("{port} · cc{controller} {value} ch{channel}"),
        MidiEvent::ProgramChange { channel, program } => {
            format!("{port} · program {program} ch{channel}")
        }
        MidiEvent::PitchBend { channel, .. } => format!("{port} · bend ch{channel}"),
        MidiEvent::ChannelAftertouch { channel, .. }
        | MidiEvent::PolyAftertouch { channel, .. } => {
            format!("{port} · aftertouch ch{channel}")
        }
        MidiEvent::Start => format!("{port} · start"),
        MidiEvent::Stop => format!("{port} · stop"),
        MidiEvent::Continue => format!("{port} · continue"),
        // A clocked device sends twenty-four of these a beat; the light
        // shows them, the list would drown in them.
        MidiEvent::Clock | MidiEvent::ActiveSensing => return None,
        #[allow(unreachable_patterns)]
        _ => return None,
    };
    Some(line)
}

fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

impl Default for PadListener {
    fn default() -> Self {
        let (sender, presses) = channel();
        let (turn_sender, turns) = channel();
        Self {
            listeners: Vec::new(),
            ports: Vec::new(),
            presses,
            sender,
            turns,
            turn_sender,
            last_message: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            recent: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::VecDeque::with_capacity(RECENT_MESSAGES),
            )),
            recent_generation: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            observed_recent_generation: 0,
            activity_visible: false,
            last_press: std::sync::Arc::new(std::sync::Mutex::new(RecentPresses::new())),
            #[cfg(feature = "harness")]
            harness_ports: Vec::new(),
        }
    }
}

/// What one port's driver callback holds: the channels it pushes into and
/// the state it stamps. Every port shares the same handles, so the mirror
/// gate sees presses from all of them.
struct DriverShared {
    presses: Sender<PadPress>,
    turns: Sender<CcTurn>,
    last_message: std::sync::Arc<std::sync::atomic::AtomicU64>,
    recent: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<String>>>,
    recent_generation: std::sync::Arc<std::sync::atomic::AtomicU64>,
    gate: std::sync::Arc<std::sync::Mutex<RecentPresses>>,
}

/// One message from a port's driver thread, exactly as the callback handles
/// it: the activity light, the readable line, a pad press through the
/// mirror gate, a knob turn. The clocks are passed in - the callback gives
/// it the real ones, the tests their own - so the gate's window is tested
/// on the path the driver takes, without sleeping.
fn on_driver_message(
    shared: &DriverShared,
    port: &str,
    event: MidiEvent,
    gate_now: u64,
    wall_now: u64,
) {
    shared
        .last_message
        .store(wall_now, std::sync::atomic::Ordering::Relaxed);
    if let Some(line) = describe(port, event)
        && let Ok(mut recent) = shared.recent.try_lock()
    {
        if recent.len() >= RECENT_MESSAGES {
            recent.pop_front();
        }
        recent.push_back(line);
        shared
            .recent_generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    match event {
        MidiEvent::NoteOn {
            channel,
            note,
            velocity,
        } if velocity > 0 => {
            // The mirror gate: one physical press must be one launch,
            // whatever delivered it. An identical pad inside the window is
            // the same press arriving on another port (a Maschine's Virtual
            // Input beside its hardware port), not a second hit. Checked and
            // recorded under one lock whose critical section is a compare
            // and a store. A poisoned lock admits the press: failing closed
            // to a double beats failing closed to a dead pad.
            let mirror = shared
                .gate
                .lock()
                .is_ok_and(|mut gate| gate.is_mirror(Pad { note, channel }, gate_now));
            if !mirror {
                let _ = shared.presses.send(PadPress {
                    note,
                    channel,
                    velocity,
                });
            }
        }
        MidiEvent::ControlChange {
            channel,
            controller,
            value,
        } => {
            let _ = shared.turns.send(CcTurn {
                controller,
                channel,
                value,
            });
        }
        _ => {}
    }
}

impl PadListener {
    fn driver_shared(&self) -> DriverShared {
        DriverShared {
            presses: self.sender.clone(),
            turns: self.turn_sender.clone(),
            last_message: std::sync::Arc::clone(&self.last_message),
            recent: std::sync::Arc::clone(&self.recent),
            recent_generation: std::sync::Arc::clone(&self.recent_generation),
            gate: std::sync::Arc::clone(&self.last_press),
        }
    }

    /// TEST. A message delivered through the driver callback's own path, at
    /// the gate time `gate_now` (milliseconds on the gate's clock).
    #[cfg(test)]
    pub fn driver_message_for_test(&self, port: &str, event: MidiEvent, gate_now: u64) {
        on_driver_message(&self.driver_shared(), port, event, gate_now, unix_millis());
    }

    /// Open every input port named in `ports`, replacing what was open if
    /// the list changed. Returns the ports that could not be opened.
    pub fn sync(&mut self, ports: &[String]) -> Vec<String> {
        if ports == self.ports.as_slice() {
            return Vec::new();
        }
        self.listeners.clear();
        self.ports = ports.to_vec();
        let mut failed = Vec::new();
        for (index, port) in ports.iter().enumerate() {
            let shared = self.driver_shared();
            let port_name = port.clone();
            let opened = MidiListener::open_filtered(
                Some(&index.to_string()),
                midir::Ignore::All,
                move |_, event, _| {
                    on_driver_message(&shared, &port_name, event, gate_millis(), unix_millis());
                },
            );
            match opened {
                Ok(listener) => self.listeners.push(listener),
                Err(_) => failed.push(port.clone()),
            }
        }
        failed
    }

    pub fn open_count(&self) -> usize {
        self.listeners.len() + self.harness_port_count()
    }

    /// The e2e harness's virtual controllers - none without the feature,
    /// so the product counts exactly the ports it opened.
    #[cfg(feature = "harness")]
    fn harness_port_count(&self) -> usize {
        self.harness_ports.len()
    }

    #[cfg(not(feature = "harness"))]
    fn harness_port_count(&self) -> usize {
        0
    }

    /// Whether any port has spoken in the last `within` milliseconds.
    pub fn active_within(&self, within: u64) -> bool {
        let last = self.last_message.load(std::sync::atomic::Ordering::Relaxed);
        last != 0 && unix_millis().saturating_sub(last) <= within
    }

    /// The last few messages, readable, newest last.
    pub fn recent_events(&self) -> Vec<String> {
        self.recent
            .lock()
            .map(|recent| recent.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Whether the mixer's MIDI feed or activity light changed since the
    /// previous UI turn. Driver callbacks only touch atomics and their
    /// bounded queue; this is the handoff that makes a stopped frame dirty.
    pub fn poll_visual_change(&mut self, within: u64) -> bool {
        let generation = self
            .recent_generation
            .load(std::sync::atomic::Ordering::Relaxed);
        let active = self.active_within(within);
        let changed =
            generation != self.observed_recent_generation || active != self.activity_visible;
        self.observed_recent_generation = generation;
        self.activity_visible = active;
        changed
    }

    /// TEST. A message as the driver would leave it.
    #[cfg(test)]
    pub fn record_for_test(&self, port: &str, event: MidiEvent) {
        if let Some(line) = describe(port, event)
            && let Ok(mut recent) = self.recent.lock()
        {
            if recent.len() >= RECENT_MESSAGES {
                recent.pop_front();
            }
            recent.push_back(line);
            self.recent_generation
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// TEST. Stamp activity as a message would.
    #[cfg(test)]
    pub fn stamp_activity_for_test(&self) {
        self.last_message
            .store(unix_millis(), std::sync::atomic::Ordering::Relaxed);
    }

    pub fn try_recv(&self) -> Result<PadPress, TryRecvError> {
        self.presses.try_recv()
    }

    /// The next knob turned, if one has.
    pub fn try_recv_cc(&self) -> Result<CcTurn, TryRecvError> {
        self.turns.try_recv()
    }

    /// TEST. A knob turned, as the driver would report it.
    #[cfg(test)]
    pub fn turn_for_test(&self, turn: CcTurn) {
        let _ = self.turn_sender.send(turn);
    }

    /// HARNESS. Plug a virtual controller in, as a USB pad appearing at a
    /// port: nothing is opened, but the studio sees a MIDI input it can
    /// learn from. The controller speaks through [`Self::harness_message`].
    #[cfg(feature = "harness")]
    pub fn attach_harness_port(&mut self, port: &str) {
        self.harness_ports.push(port.to_owned());
    }

    /// HARNESS. One message from a harness-attached controller, through
    /// the driver callback's own path - the mirror gate, the readable
    /// feed, the activity light - exactly as a real port's thread would
    /// deliver it.
    #[cfg(feature = "harness")]
    pub fn harness_message(&self, port: &str, event: MidiEvent) {
        on_driver_message(
            &self.driver_shared(),
            port,
            event,
            gate_millis(),
            unix_millis(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note_on(note: u8) -> MidiEvent {
        MidiEvent::NoteOn {
            channel: 10,
            note,
            velocity: 100,
        }
    }

    fn delivered(listener: &PadListener) -> Vec<u8> {
        std::iter::from_fn(|| listener.try_recv().ok())
            .map(|press| press.note)
            .collect()
    }

    /// A Maschine exposes its hardware port AND a software mirror (the app's
    /// "Virtual Input"), so one physical press arrives as two identical
    /// NoteOns milliseconds apart. One press is one launch: the mirror is
    /// dropped inside the window. This is the "pad does a little double" fix,
    /// driven through the driver callback's own path on an injected clock.
    #[test]
    fn a_press_mirrored_on_a_second_port_launches_once() {
        let listener = PadListener::default();
        listener.driver_message_for_test("Maschine", note_on(36), 0);
        listener.driver_message_for_test("Maschine Virtual Input", note_on(36), 5);
        assert_eq!(delivered(&listener), vec![36], "the mirror is dropped");
    }

    /// Past the window the same pad is a musician hitting it again.
    #[test]
    fn the_same_pad_just_past_the_window_launches_twice() {
        let listener = PadListener::default();
        listener.driver_message_for_test("Maschine", note_on(36), 0);
        listener.driver_message_for_test("Maschine", note_on(36), DOUBLE_PRESS_WINDOW_MILLIS + 1);
        assert_eq!(delivered(&listener), vec![36, 36]);
    }

    /// The gate is per pad, not per port-pair: a mirror of one pad must
    /// never swallow a different pad played straight after - two pads hit
    /// together are two launches, and a third pad is a third.
    #[test]
    fn different_pads_in_the_window_all_launch() {
        let listener = PadListener::default();
        for (at, note) in [36, 37, 38, 36].into_iter().enumerate() {
            listener.driver_message_for_test("pads", note_on(note), at as u64);
        }
        assert_eq!(delivered(&listener), vec![36, 37, 38]);
    }

    /// Two pads hit together arrive as A, B and then their mirrors A', B'.
    /// The gate must remember more than the last pad, or B pushes A out
    /// before A' arrives and both mirrors launch - two scenes restarted
    /// twice.
    #[test]
    fn interleaved_mirrors_of_two_pads_are_both_dropped() {
        let listener = PadListener::default();
        for (at, note) in [36, 37, 36, 37].into_iter().enumerate() {
            listener.driver_message_for_test("pads", note_on(note), at as u64);
        }
        assert_eq!(delivered(&listener), vec![36, 37]);
    }

    /// A knob on the driver path reaches the slider channel, and every
    /// message lights the activity light.
    #[test]
    fn a_knob_on_the_driver_path_reaches_the_slider_channel() {
        let listener = PadListener::default();
        listener.driver_message_for_test(
            "knobs",
            MidiEvent::ControlChange {
                channel: 2,
                controller: 74,
                value: 63,
            },
            0,
        );
        assert_eq!(
            listener.try_recv_cc().ok(),
            Some(CcTurn {
                controller: 74,
                channel: 2,
                value: 63,
            })
        );
        assert!(listener.active_within(60_000));
        assert!(listener.try_recv().is_err(), "a knob is not a pad");
    }

    /// The gate's memory is bounded and the oldest slot is the one reused:
    /// more distinct pads than slots still all launch, and a mirror of the
    /// newest is still caught.
    #[test]
    fn the_gate_forgets_the_oldest_pad_first() {
        let listener = PadListener::default();
        let pads = RECENT_PRESSES as u8 + 2;
        for note in 0..pads {
            listener.driver_message_for_test("pads", note_on(36 + note), u64::from(note));
        }
        // The newest pad's mirror is dropped; the oldest (evicted) would be
        // taken for a fresh press, which is the bounded memory's one cost.
        listener.driver_message_for_test("pads", note_on(36 + pads - 1), u64::from(pads));
        assert_eq!(
            delivered(&listener).len(),
            RECENT_PRESSES + 2,
            "every distinct pad launched; the mirror did not"
        );
    }

    /// A ghost note-on (velocity 1-10, as the mixer's `v10` lines show) is
    /// still a press; but a real second hit AFTER the window is a musician,
    /// not a mirror, and must launch.
    #[test]
    fn the_same_pad_after_the_window_launches_again() {
        let listener = PadListener::default();
        let press = PadPress {
            note: 36,
            channel: 10,
            velocity: 100,
        };
        drop(listener);
        // The gate's own clock, injected: no sleep, no wall clock.
        let pad = Pad {
            note: press.note,
            channel: press.channel,
        };
        let mut gate = RecentPresses::new();
        assert!(!gate.is_mirror(pad, 1_000), "the first press launches");
        assert!(
            gate.is_mirror(pad, 1_000 + DOUBLE_PRESS_WINDOW_MILLIS),
            "the same pad at the window's edge is its mirror"
        );
        assert!(
            !gate.is_mirror(pad, 1_000 + DOUBLE_PRESS_WINDOW_MILLIS + 1),
            "a genuine re-hit after the window launches"
        );
    }

    /// An empty gate slot matches nothing: the very first press of note 0 on
    /// channel 0, at the monotonic clock's time zero, still launches.
    #[test]
    fn an_empty_gate_slot_is_never_a_match() {
        let mut gate = RecentPresses::new();
        let zero = Pad {
            note: 0,
            channel: 0,
        };
        assert!(!gate.is_mirror(zero, 0), "a fresh gate has seen no press");
        assert!(gate.is_mirror(zero, 0), "and now it has");
    }

    /// The messages a musician reads: a note with its velocity and channel,
    /// a knob with its value; a clock tick is not a line; the list keeps
    /// only the last few.
    #[test]
    fn recent_messages_read_as_a_musician_reads_them() {
        let listener = PadListener::default();
        listener.record_for_test(
            "MiniLab",
            MidiEvent::NoteOn {
                channel: 1,
                note: 60,
                velocity: 100,
            },
        );
        listener.record_for_test(
            "MiniLab",
            MidiEvent::ControlChange {
                channel: 2,
                controller: 74,
                value: 63,
            },
        );
        listener.record_for_test("MiniLab", MidiEvent::Clock);
        let lines = listener.recent_events();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[0], "MiniLab · note c4 (60) v100 ch1");
        assert_eq!(lines[1], "MiniLab · cc74 63 ch2");
        for _ in 0..40 {
            listener.record_for_test("x", MidiEvent::Start);
        }
        assert_eq!(listener.recent_events().len(), RECENT_MESSAGES);
    }

    /// A message on any port lights the footer for a blink.
    #[test]
    fn a_message_lights_the_listener_for_a_blink() {
        let listener = PadListener::default();
        assert!(!listener.active_within(1_000), "nothing yet");
        listener.stamp_activity_for_test();
        assert!(listener.active_within(1_000));
    }

    #[test]
    fn the_ui_observes_each_new_readable_message_once_while_stopped() {
        let mut listener = PadListener::default();
        assert!(!listener.poll_visual_change(1_000));
        listener.record_for_test("MiniLab", MidiEvent::Start);
        assert!(listener.poll_visual_change(1_000));
        assert!(!listener.poll_visual_change(1_000));

        listener.stamp_activity_for_test();
        assert!(listener.poll_visual_change(1_000));
        assert!(!listener.poll_visual_change(1_000));
    }

    #[test]
    fn a_pad_is_labelled_by_note_name_and_channel() {
        let pad = Pad {
            note: 48,
            channel: 10,
        };
        assert_eq!(pad.label(), "c3/10");
        assert!(pad.matches(48, 10));
        assert!(!pad.matches(48, 1));
    }
}
