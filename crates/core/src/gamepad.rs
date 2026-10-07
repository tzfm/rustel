/*
gamepad.rs - Live gamepad state, shared between the poller and the queries
Native state and polling interface:
Copyright (C) 2026 Rustel contributors

Button names and sequence behavior follow @strudel/gamepad by Yuta Nakayama.

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! What a score reads off a gamepad: which buttons are down, which have
//! been toggled, where the sticks are, and the last few presses for a
//! button sequence.
//!
//! The pads are polled by whichever host has a device stack - the
//! runtime's `gamepad` feature - and read on the thread that queries
//! patterns, so everything here is an atomic: the poller never blocks a
//! query and a query never blocks the poller. A build with no poller
//! reads every button up and every stick centred, the way a page with no
//! gamepad plugged in does.
//!
//! Buttons and sticks are numbered the way the browser's standard mapping
//! numbers them, so a score written against strudel.cc reads the same
//! pad here: `a b x y lb rb lt rt back start l3 r3 up down left right`,
//! then the home button; sticks left X, left Y, right X, right Y, with
//! down and right positive.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// How many pads a score can name: `gamepad(0)` to `gamepad(3)`.
pub const MAX_PADS: usize = 4;
/// The standard mapping's sixteen buttons and the home button.
pub const BUTTONS: usize = 17;
/// Two sticks, two axes each.
pub const AXES: usize = 4;
/// How many presses a sequence can look back over.
const HISTORY: usize = 16;
/// How recent the last press of a sequence has to be for the sequence to
/// count as played - the same two seconds as strudel.cc.
pub const SEQUENCE_WINDOW_MILLIS: u64 = 2_000;

/// Every name a score may call a button, and the button it names.
pub const BUTTON_NAMES: &[(&[&str], u8)] = &[
    (&["a"], 0),
    (&["b"], 1),
    (&["x"], 2),
    (&["y"], 3),
    (&["lb"], 4),
    (&["rb"], 5),
    (&["lt"], 6),
    (&["rt"], 7),
    (&["back"], 8),
    (&["start"], 9),
    (&["l3", "ls"], 10),
    (&["r3", "rs"], 11),
    (&["up", "u"], 12),
    (&["down", "d"], 13),
    (&["left", "l"], 14),
    (&["right", "r"], 15),
];

/// The button a name means, in any case: `A` and `a` are one button.
pub fn button_index(name: &str) -> Option<u8> {
    let name = name.to_ascii_lowercase();
    BUTTON_NAMES
        .iter()
        .find(|(names, _)| names.contains(&name.as_str()))
        .map(|(_, index)| *index)
}

/// One pad's live state.
pub struct Pad {
    /// Each button's value, 0 to 1, as f32 bits: a trigger is analog.
    buttons: [AtomicU32; BUTTONS],
    /// One bit a button, flipped on every press.
    toggles: AtomicU32,
    /// Each axis, -1 to 1, as f32 bits.
    axes: [AtomicU32; AXES],
    /// The last presses, newest last: the button in the top byte, the
    /// millisecond it landed in the rest.
    history: [AtomicU64; HISTORY],
    /// Presses ever seen; the next one goes at `presses % HISTORY`.
    presses: AtomicUsize,
    connected: AtomicBool,
    /// The millisecond anything last moved on it, for an activity light.
    activity: AtomicU64,
}

static PADS: [Pad; MAX_PADS] = [const { Pad::new() }; MAX_PADS];

/// The pad a score numbered `index`, if there can be one.
pub fn pad(index: usize) -> Option<&'static Pad> {
    PADS.get(index)
}

/// Milliseconds since the first thing here asked the time.
fn now_millis() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let elapsed = EPOCH.get_or_init(Instant::now).elapsed();
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

impl Pad {
    const fn new() -> Self {
        Self {
            buttons: [const { AtomicU32::new(0) }; BUTTONS],
            toggles: AtomicU32::new(0),
            axes: [const { AtomicU32::new(0) }; AXES],
            history: [const { AtomicU64::new(0) }; HISTORY],
            presses: AtomicUsize::new(0),
            connected: AtomicBool::new(false),
            activity: AtomicU64::new(0),
        }
    }

    /// Something moved: a light can show it.
    fn touched(&self) {
        self.activity.store(now_millis().max(1), Ordering::Relaxed);
    }

    /// How long since anything moved on this pad, or None if nothing has.
    pub fn idle_for_millis(&self) -> Option<u64> {
        match self.activity.load(Ordering::Relaxed) {
            0 => None,
            at => Some(now_millis().saturating_sub(at)),
        }
    }

    /// POLLER THREAD. A button's new value; a rise through a half is a
    /// press, which flips its toggle and goes into the history.
    pub fn set_button(&self, button: usize, value: f32) {
        let Some(slot) = self.buttons.get(button) else {
            return;
        };
        let was = f32::from_bits(slot.swap(value.to_bits(), Ordering::Relaxed));
        if was != value {
            self.touched();
        }
        if was < 0.5 && value >= 0.5 {
            self.toggles.fetch_xor(1 << button, Ordering::Relaxed);
            self.record_press(button, now_millis());
        }
    }

    fn record_press(&self, button: usize, millis: u64) {
        let count = self.presses.load(Ordering::Acquire);
        let entry = ((button as u64) << 56) | (millis & ((1 << 56) - 1));
        self.history[count % HISTORY].store(entry, Ordering::Relaxed);
        self.presses.store(count + 1, Ordering::Release);
    }

    /// POLLER THREAD. An axis's new position, -1 to 1, down and right
    /// positive.
    pub fn set_axis(&self, axis: usize, value: f32) {
        if let Some(slot) = self.axes.get(axis) {
            let value = value.clamp(-1.0, 1.0);
            let was = f32::from_bits(slot.swap(value.to_bits(), Ordering::Relaxed));
            // A stick at rest jitters by a hair; that is not activity.
            if (was - value).abs() > 0.02 {
                self.touched();
            }
        }
    }

    /// POLLER THREAD. A pad arriving or going; one that goes lets go of
    /// every button and centres every stick, so a score does not play on
    /// a button that can no longer be released.
    pub fn set_connected(&self, connected: bool) {
        self.connected.store(connected, Ordering::Relaxed);
        if !connected {
            for slot in &self.buttons {
                slot.store(0, Ordering::Relaxed);
            }
            for slot in &self.axes {
                slot.store(0, Ordering::Relaxed);
            }
        }
    }

    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// QUERY THREAD. The button's value, 0 to 1.
    pub fn button(&self, button: usize) -> f64 {
        self.buttons.get(button).map_or(0.0, |slot| {
            f64::from(f32::from_bits(slot.load(Ordering::Relaxed)))
        })
    }

    /// QUERY THREAD. Whether the button has been pressed an odd number
    /// of times: 1 or 0.
    pub fn toggle(&self, button: usize) -> f64 {
        if button >= BUTTONS {
            return 0.0;
        }
        f64::from((self.toggles.load(Ordering::Relaxed) >> button) & 1)
    }

    /// QUERY THREAD. The axis, -1 to 1.
    pub fn axis(&self, axis: usize) -> f64 {
        self.axes.get(axis).map_or(0.0, |slot| {
            f64::from(f32::from_bits(slot.load(Ordering::Relaxed)))
        })
    }

    /// QUERY THREAD. 1 while the last presses were exactly `buttons`, in
    /// order, and the last of them landed within the window; 0 otherwise.
    pub fn sequence(&self, buttons: &[u8]) -> f64 {
        self.sequence_at(buttons, now_millis())
    }

    fn sequence_at(&self, buttons: &[u8], now: u64) -> f64 {
        if buttons.is_empty() || buttons.len() > HISTORY {
            return 0.0;
        }
        let count = self.presses.load(Ordering::Acquire);
        if count < buttons.len() {
            return 0.0;
        }
        let first = count - buttons.len();
        for (offset, wanted) in buttons.iter().enumerate() {
            let entry = self.history[(first + offset) % HISTORY].load(Ordering::Relaxed);
            if (entry >> 56) as u8 != *wanted {
                return 0.0;
            }
        }
        let last = self.history[(count - 1) % HISTORY].load(Ordering::Relaxed);
        let landed = last & ((1 << 56) - 1);
        if now.saturating_sub(landed) <= SEQUENCE_WINDOW_MILLIS {
            1.0
        } else {
            0.0
        }
    }

    #[cfg(test)]
    fn reset(&self) {
        self.set_connected(false);
        self.toggles.store(0, Ordering::Relaxed);
        self.presses.store(0, Ordering::Release);
        self.activity.store(0, Ordering::Relaxed);
    }
}

static WANTED: AtomicBool = AtomicBool::new(false);
static STARTER: OnceLock<fn()> = OnceLock::new();
static PROBLEM: OnceLock<String> = OnceLock::new();
static POLLING: AtomicBool = AtomicBool::new(false);
static NAMES: Mutex<[Option<String>; MAX_PADS]> = Mutex::new([None, None, None, None]);
static NOTICES: Mutex<VecDeque<Notice>> = Mutex::new(VecDeque::new());

/// Something a player should hear about: the poller starting, a pad
/// arriving or going, or why none will. Left by the poller, taken by the
/// studio for its log and status line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Notice {
    /// The poller is up: from here on pads are watched.
    Polling,
    Connected {
        pad: usize,
        name: String,
    },
    Disconnected {
        pad: usize,
        name: String,
    },
    /// No pad will ever be read, and why.
    Problem(String),
}

/// POLLER THREAD. Leave a notice for the studio.
pub fn notice(notice: Notice) {
    let mut notices = NOTICES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // A studio that never takes them must not grow a queue forever.
    if notices.len() >= 64 {
        notices.pop_front();
    }
    notices.push_back(notice);
}

/// STUDIO THREAD. Everything left since the last take.
pub fn take_notices() -> Vec<Notice> {
    NOTICES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .drain(..)
        .collect()
}

/// How many pad lines the mixer's feed remembers - the same span MIDI's
/// own recent list keeps, so the two panels read as one convention.
const RECENT_ACTIVITY: usize = 24;
static RECENT: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

/// STUDIO THREAD. Leave a line for the mixer's feed: newest last, bounded.
/// Unlike a [`Notice`] this is read, not drained - the mixer redraws every
/// frame and wants the same lines still there until a new one pushes the
/// oldest out, the way MIDI's own recent list works.
pub fn note_activity(line: String) {
    let mut recent = RECENT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if recent.len() >= RECENT_ACTIVITY {
        recent.pop_front();
    }
    recent.push_back(line);
}

/// The last few pad lines, readable, newest last.
pub fn recent_activity() -> Vec<String> {
    RECENT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .cloned()
        .collect()
}

/// POLLER THREAD. A pad arrived under this name; a pad that went leaves
/// its name until another takes the slot, so the notice can say who went.
pub fn set_name(pad: usize, name: Option<String>) {
    if let Some(slot) = NAMES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get_mut(pad)
    {
        *slot = name;
    }
}

/// The name the pad arrived under, if one has.
pub fn name(pad: usize) -> Option<String> {
    NAMES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(pad)
        .cloned()
        .flatten()
}

/// The pads plugged in right now, numbered the way a score numbers them.
pub fn connected_pads() -> Vec<(usize, String)> {
    (0..MAX_PADS)
        .filter(|&index| PADS[index].connected())
        .map(|index| (index, name(index).unwrap_or_else(|| "gamepad".to_owned())))
        .collect()
}

/// POLLER THREAD. Say the poller is up.
pub fn set_polling() {
    POLLING.store(true, Ordering::Release);
    notice(Notice::Polling);
}

/// Whether a poller is reading the pads.
pub fn polling() -> bool {
    POLLING.load(Ordering::Acquire)
}

/// Code has asked for a pad: start polling, if a host has said how. A score
/// asks once it is accepted. The host's starter is expected to be a no-op
/// after the first time. A build with no host to poll reports that once, so
/// the user can see why no pad responds.
pub fn request() {
    let first = !WANTED.swap(true, Ordering::AcqRel);
    match STARTER.get() {
        Some(start) => start(),
        None if first => note_problem(
            "this build reads no gamepads: every button reads up and every stick centred"
                .to_owned(),
        ),
        None => {}
    }
}

/// Whether any score has asked for a pad yet.
pub fn wanted() -> bool {
    WANTED.load(Ordering::Acquire)
}

/// The host says how polling starts. If a score asked before the host
/// got here, it starts now.
pub fn set_starter(start: fn()) {
    if STARTER.set(start).is_ok() && wanted() {
        start();
    }
}

/// Records that the host cannot poll (no device stack, no permission). Only
/// the first problem is kept and sent as a notice, for a status line to show.
pub fn note_problem(problem: String) {
    if PROBLEM.set(problem.clone()).is_ok() {
        notice(Notice::Problem(problem));
    }
}

pub fn problem() -> Option<&'static str> {
    PROBLEM.get().map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests share the static pads, so each takes a pad of its own and
    /// resets it; the runtime's semantic test takes pad 0.
    fn fresh(index: usize) -> &'static Pad {
        let pad = pad(index).expect("a pad");
        pad.reset();
        pad
    }

    #[test]
    fn a_press_is_a_rise_through_a_half_and_flips_the_toggle() {
        let pad = fresh(1);
        assert_eq!(pad.button(0), 0.0);
        assert_eq!(pad.toggle(0), 0.0);
        pad.set_button(0, 1.0);
        assert_eq!(pad.button(0), 1.0);
        assert_eq!(pad.toggle(0), 1.0);
        pad.set_button(0, 1.0);
        assert_eq!(pad.toggle(0), 1.0, "held is not pressed again");
        pad.set_button(0, 0.0);
        assert_eq!(pad.toggle(0), 1.0, "a release does not flip");
        pad.set_button(0, 0.3);
        pad.set_button(0, 0.7);
        assert_eq!(
            pad.toggle(0),
            0.0,
            "a trigger squeezed past a half is a press"
        );
        assert_eq!(pad.button(0), 0.7f32 as f64, "and reads its analog value");
        assert_eq!(
            pad.button(BUTTONS),
            0.0,
            "a button that is not there reads up"
        );
        assert_eq!(pad.toggle(BUTTONS), 0.0);
    }

    #[test]
    fn activity_is_stamped_by_a_change_and_not_by_jitter() {
        let pad = fresh(0);
        assert_eq!(pad.idle_for_millis(), None, "nothing has moved yet");
        pad.set_axis(0, 0.01);
        assert_eq!(
            pad.idle_for_millis(),
            None,
            "a hair of jitter is not a move"
        );
        pad.set_axis(0, 0.5);
        assert!(pad.idle_for_millis().is_some_and(|idle| idle < 1_000));
        pad.set_button(2, 1.0);
        assert!(pad.idle_for_millis().is_some_and(|idle| idle < 1_000));
    }

    #[test]
    fn sticks_read_where_they_were_put_and_centre_when_the_pad_goes() {
        let pad = fresh(2);
        pad.set_axis(0, -0.5);
        pad.set_axis(3, 2.0);
        assert_eq!(pad.axis(0), -0.5);
        assert_eq!(pad.axis(3), 1.0, "clamped to the stick's throw");
        assert_eq!(pad.axis(AXES), 0.0);
        pad.set_button(4, 1.0);
        pad.set_connected(false);
        assert_eq!(pad.axis(0), 0.0);
        assert_eq!(pad.button(4), 0.0, "an unplugged pad holds nothing down");
        assert_eq!(pad.toggle(4), 1.0, "but what was toggled stays toggled");
    }

    #[test]
    fn a_sequence_is_the_last_presses_in_order_within_the_window() {
        let pad = fresh(3);
        let combo = [13u8, 15, 0];
        assert_eq!(pad.sequence_at(&combo, 0), 0.0, "nothing pressed yet");
        for (at, button) in [(100, 13usize), (200, 15), (300, 0)] {
            pad.record_press(button, at);
        }
        assert_eq!(pad.sequence_at(&combo, 300), 1.0);
        assert_eq!(pad.sequence_at(&combo, 300 + SEQUENCE_WINDOW_MILLIS), 1.0);
        assert_eq!(
            pad.sequence_at(&combo, 301 + SEQUENCE_WINDOW_MILLIS),
            0.0,
            "too long ago"
        );
        assert_eq!(
            pad.sequence_at(&[15, 0], 300),
            1.0,
            "a tail of it is a sequence too"
        );
        assert_eq!(pad.sequence_at(&[0, 15], 300), 0.0, "order matters");
        pad.record_press(1, 400);
        assert_eq!(
            pad.sequence_at(&combo, 400),
            0.0,
            "a press after it ends it"
        );
        // The history is a ring: a long run of presses still matches its tail.
        for at in 0..(HISTORY as u64 * 2) {
            pad.record_press(2, 1_000 + at);
        }
        pad.record_press(13, 5_000);
        pad.record_press(15, 5_001);
        pad.record_press(0, 5_002);
        assert_eq!(pad.sequence_at(&combo, 5_002), 1.0);
        assert_eq!(pad.sequence_at(&[], 5_002), 0.0);
        assert_eq!(
            pad.sequence_at(&[2; HISTORY + 1], 5_002),
            0.0,
            "longer than the ring"
        );
    }

    /// Notices queue up for the studio and are taken once; names ride
    /// beside the pads so a notice can say who came or went.
    #[test]
    fn notices_are_left_for_the_studio_and_taken_once() {
        let _ = take_notices();
        set_name(3, Some("Test Pad".to_owned()));
        notice(Notice::Connected {
            pad: 3,
            name: name(3).expect("named"),
        });
        assert_eq!(
            take_notices(),
            vec![Notice::Connected {
                pad: 3,
                name: "Test Pad".to_owned()
            }]
        );
        assert!(take_notices().is_empty(), "taken once");
        set_name(3, None);
        assert_eq!(name(3), None);
        assert_eq!(name(MAX_PADS), None);
        // A queue nobody takes stays bounded.
        for index in 0..200 {
            notice(Notice::Disconnected {
                pad: 0,
                name: index.to_string(),
            });
        }
        assert_eq!(take_notices().len(), 64);
    }

    /// The mixer's feed keeps only the last few lines, newest last, and
    /// reading it does not empty it - the mixer redraws every frame and
    /// wants the same lines still there next time, unlike a [`Notice`].
    #[test]
    fn recent_activity_is_read_not_drained_and_stays_bounded() {
        for index in 0..40 {
            note_activity(format!("gamepad(0) Test Pad: line {index}"));
        }
        let first_read = recent_activity();
        assert_eq!(first_read.len(), RECENT_ACTIVITY, "{first_read:?}");
        assert_eq!(
            first_read.last(),
            Some(&"gamepad(0) Test Pad: line 39".to_owned()),
            "newest last"
        );
        assert_eq!(
            first_read.first(),
            Some(&format!(
                "gamepad(0) Test Pad: line {}",
                40 - RECENT_ACTIVITY
            )),
            "the oldest lines fell off the front"
        );
        assert_eq!(recent_activity(), first_read, "read, not drained");
    }

    #[test]
    fn every_button_name_is_one_button_in_any_case() {
        assert_eq!(button_index("a"), Some(0));
        assert_eq!(button_index("A"), Some(0));
        assert_eq!(button_index("LB"), Some(4));
        assert_eq!(button_index("ls"), Some(10));
        assert_eq!(button_index("L3"), Some(10));
        assert_eq!(button_index("u"), Some(12));
        assert_eq!(button_index("Right"), Some(15));
        assert_eq!(button_index("home"), None);
        assert!(pad(MAX_PADS).is_none());
    }
}
