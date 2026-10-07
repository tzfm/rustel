/*
gamepad.rs - Polling the gamepads a score reads
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! The device half of `gamepad()`: reading every pad and writing what it
//! sees into [`rustel_core::gamepad`], where the score reads it.
//!
//! On Linux and Windows one thread reads the pads through gilrs. On macOS
//! the system hands an Xbox or PlayStation pad on a cable to its own
//! GameController framework and gives IOKit readers such as gilrs no
//! reports from it, so there the pads are read through the framework -
//! on the main thread, whose run loop the framework delivers through:
//! [`pump`] is called from every loop that owns the main thread.
//!
//! Started by the first `gamepad()` a score calls - a set that never
//! names a pad never opens the device stack - and left running for the
//! rest of the process.

use std::sync::OnceLock;
use std::time::Duration;

use rustel_core::gamepad::{self as pads, MAX_PADS, Notice};

/// Start reading the pads, once. Later calls are free. On macOS there is
/// no thread to start: the main thread reads the framework on every
/// [`pump`], and this only marks the pads as read.
pub fn ensure_polling() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        #[cfg(target_os = "macos")]
        pads::set_polling();
        #[cfg(not(target_os = "macos"))]
        if let Err(error) = std::thread::Builder::new()
            .name("gamepads".to_owned())
            .spawn(hid::poll)
        {
            pads::note_problem(format!("gamepads: could not start polling: {error}"));
        }
    });
}

/// A moment of the main thread for the pads: on macOS the framework
/// finds pads and delivers their state through the main run loop, which
/// neither the studio nor the CLI runs otherwise, so the main thread
/// gives it this call every few milliseconds while the work runs
/// elsewhere. Elsewhere a no-op - gilrs has its own thread.
pub fn pump() {
    #[cfg(target_os = "macos")]
    framework::pump();
}

/// Whether the main thread has to keep calling [`pump`] for the pads to
/// be read at all: on macOS, yes.
pub fn needs_the_main_thread() -> bool {
    cfg!(target_os = "macos")
}

/// The pads plugged in, for `rustel devices`.
pub fn list() -> Result<Vec<(usize, String)>, String> {
    #[cfg(target_os = "macos")]
    {
        framework::list()
    }
    #[cfg(not(target_os = "macos"))]
    {
        hid::list()
    }
}

/// `rustel gamepad-monitor`: what every pad sends, as a score reads it,
/// for `duration`, or until `stop` says so.
///
/// `stop` is the interrupt. Without it the monitor sat out its whole
/// duration whatever anyone pressed: thirty seconds by default, with Ctrl-C
/// doing nothing, on the one command a player runs precisely because
/// something is already not working.
pub fn monitor(
    duration: Duration,
    stop: &dyn Fn() -> bool,
    print: impl FnMut(String),
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        framework::monitor(duration, stop, print)
    }
    #[cfg(not(target_os = "macos"))]
    {
        hid::monitor(duration, stop, print)
    }
}

/// The lines the monitor prints for a pad whose state moved: a button
/// crossing half is pressed or released, a trigger between the two is
/// its value, a stick that moved more than a hair is its position.
#[cfg(any(target_os = "macos", test))]
fn describe_changes(before: &PadSnapshot, after: &PadSnapshot) -> Vec<String> {
    let mut lines = Vec::new();
    for (slot, (was, now)) in before.buttons.iter().zip(&after.buttons).enumerate() {
        if was < &0.5 && now >= &0.5 {
            lines.push(format!("{} pressed", slot_name(slot)));
        } else if was >= &0.5 && now < &0.5 {
            lines.push(format!("{} released", slot_name(slot)));
        } else if (was - now).abs() > 0.02 && (0.01..0.99).contains(now) {
            lines.push(format!("{} {now:.2}", slot_name(slot)));
        }
    }
    for (axis, (was, now)) in before.axes.iter().zip(&after.axes).enumerate() {
        if (was - now).abs() > 0.02 {
            lines.push(format!("{} {now:+.2}", AXIS_NAMES[axis]));
        }
    }
    lines
}

/// A pad's buttons and sticks as the score would read them, for telling
/// one moment from the next.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct PadSnapshot {
    buttons: [f32; rustel_core::gamepad::BUTTONS],
    axes: [f32; rustel_core::gamepad::AXES],
}

#[cfg(target_os = "macos")]
impl PadSnapshot {
    fn of(pad: &pads::Pad) -> Self {
        let mut snapshot = Self::default();
        for (slot, value) in snapshot.buttons.iter_mut().enumerate() {
            *value = pad.button(slot) as f32;
        }
        for (axis, value) in snapshot.axes.iter_mut().enumerate() {
            *value = pad.axis(axis) as f32;
        }
        snapshot
    }
}

/// The name a score reads a button slot by, for the monitor and for the
/// studio's own activity log, which reads a press the same way.
pub fn slot_name(slot: usize) -> &'static str {
    rustel_core::gamepad::BUTTON_NAMES
        .iter()
        .find(|(_, index)| usize::from(*index) == slot)
        .map_or("home", |(names, _)| names[0])
}

/// An axis as a score names it.
pub const AXIS_NAMES: [&str; 4] = ["x1", "y1", "x2", "y2"];

/// How long a pad may be gone and come back under the same number without
/// anyone being told: a cable reseated, a wireless pad dropping a beat.
const REPLUG_GRACE: Duration = Duration::from_secs(1);

/// The numbers a score calls the pads by. A pad keeps its number for the
/// life of the process. When a pad is unplugged, its slot waits for a pad of
/// the same name, so `gamepad(0)` is still the same pad after a reseated
/// cable. A slot that no pad returns to goes to the next new pad. A fifth
/// pad gets no number.
struct Slots<Id> {
    /// The driver's id in each slot; none while the slot's pad is away.
    ids: Vec<Option<Id>>,
    /// When the slot's pad went, until the studio has been told.
    unplugged_at: Vec<Option<std::time::Instant>>,
}

impl<Id: Copy + PartialEq> Slots<Id> {
    const fn new() -> Self {
        Self {
            ids: Vec::new(),
            unplugged_at: Vec::new(),
        }
    }

    fn of(&self, id: Id) -> Option<usize> {
        self.ids.iter().position(|known| *known == Some(id))
    }

    /// Where a pad called `name` goes: its own slot if it is known, the
    /// slot its name left, the first empty slot, or a new one.
    fn place(&mut self, id: Id, name: &str) -> Option<usize> {
        if let Some(slot) = self.of(id) {
            return Some(slot);
        }
        let empty = |slot: &usize| self.ids[*slot].is_none();
        let reclaimed = (0..self.ids.len())
            .filter(empty)
            .find(|&slot| pads::name(slot).as_deref() == Some(name))
            .or_else(|| (0..self.ids.len()).find(empty));
        if let Some(slot) = reclaimed {
            self.ids[slot] = Some(id);
            return Some(slot);
        }
        if self.ids.len() >= MAX_PADS {
            return None;
        }
        self.ids.push(Some(id));
        self.unplugged_at.push(None);
        Some(self.ids.len() - 1)
    }

    fn unplug(&mut self, id: Id) -> Option<usize> {
        let slot = self.of(id)?;
        self.ids[slot] = None;
        self.unplugged_at[slot] = Some(std::time::Instant::now());
        Some(slot)
    }

    /// The slots whose pads have been gone longer than the grace: their
    /// leaving is news now.
    fn overdue(&mut self) -> Vec<usize> {
        let now = std::time::Instant::now();
        let mut gone = Vec::new();
        for (slot, at) in self.unplugged_at.iter_mut().enumerate() {
            if at.is_some_and(|at| now.duration_since(at) >= REPLUG_GRACE) {
                *at = None;
                gone.push(slot);
            }
        }
        gone
    }

    /// The notices a pad called `name` arriving in `slot` owes the studio,
    /// asked before the slot takes the new name: none when the pad that left
    /// it comes back within the grace, otherwise its arrival, after the
    /// displaced pad's leaving if that was never told. Spends the slot's
    /// untold leaving either way.
    fn arrival_notices(&mut self, slot: usize, name: &str) -> Vec<Notice> {
        let arrival = Notice::Connected {
            pad: slot,
            name: name.to_owned(),
        };
        if self.unplugged_at[slot].take().is_none() {
            return vec![arrival];
        }
        if pads::name(slot).as_deref() == Some(name) {
            return Vec::new();
        }
        vec![departure(slot), arrival]
    }
}

/// The notice that the pad last in `slot` has gone, under the name it
/// arrived with.
fn departure(slot: usize) -> Notice {
    Notice::Disconnected {
        pad: slot,
        name: pads::name(slot).unwrap_or_else(|| "gamepad".to_owned()),
    }
}

/// The pads through macOS's GameController framework.
#[cfg(target_os = "macos")]
mod framework {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use objc2::MainThreadMarker;
    use objc2_core_foundation::{CFRunLoop, kCFRunLoopDefaultMode};
    use objc2_game_controller::{GCController, GCDevice, GCExtendedGamepad};

    use super::{PadSnapshot, Slots, departure, describe_changes, pads};
    use rustel_core::gamepad::Notice;

    /// The numbers the pads have, keyed by the framework's controller
    /// object, which lives as long as the connection.
    static SLOTS: Mutex<Slots<usize>> = Mutex::new(Slots::new());

    /// Let the framework deliver: a turn of the main run loop, then every
    /// pad's state into its slot. Off the main thread there is nothing to
    /// pump, and the framework wants its callers there; and until a score
    /// or the studio asks for the pads, the framework is left alone.
    pub fn pump() {
        if MainThreadMarker::new().is_none() || !(pads::polling() || pads::wanted()) {
            return;
        }
        // The terminal is the frontmost application, never rustel, and
        // since macOS 11.3 the framework keeps a background process's
        // presses to itself unless it asks for them. Once.
        static ASKED_FOR_BACKGROUND_EVENTS: std::sync::Once = std::sync::Once::new();
        ASKED_FOR_BACKGROUND_EVENTS.call_once(|| {
            // SAFETY: a class property set on the main thread.
            unsafe { GCController::setShouldMonitorBackgroundEvents(true) };
        });
        // SAFETY: a mode name the framework itself defines; reading it is
        // reading a static.
        let mode = unsafe { kCFRunLoopDefaultMode };
        CFRunLoop::run_in_mode(mode, 0.0, true);
        read_all();
    }

    fn name_of(controller: &GCController) -> String {
        // SAFETY: plain property reads on a live controller.
        let (vendor, category) = unsafe {
            (
                controller.vendorName().map(|name| name.to_string()),
                controller.productCategory().to_string(),
            )
        };
        match vendor {
            Some(vendor) if !vendor.is_empty() && vendor != category => {
                format!("{vendor} ({category})")
            }
            _ => category,
        }
    }

    /// Every controller the framework knows, into the pads.
    fn read_all() {
        // SAFETY: the framework's own list, read on the main thread.
        let controllers = unsafe { GCController::controllers() };
        let mut slots = SLOTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut present = Vec::new();
        for controller in controllers.iter() {
            let id = &*controller as *const GCController as usize;
            present.push(id);
            let slot = match slots.of(id) {
                Some(slot) => slot,
                None => {
                    let name = name_of(&controller);
                    let Some(slot) = slots.place(id, &name) else {
                        continue;
                    };
                    for notice in slots.arrival_notices(slot, &name) {
                        pads::notice(notice);
                    }
                    pads::set_name(slot, Some(name));
                    if let Some(pad) = pads::pad(slot) {
                        pad.set_connected(true);
                    }
                    slot
                }
            };
            if let Some(pad) = pads::pad(slot) {
                read(&controller, pad);
            }
        }
        let gone: Vec<usize> = slots
            .ids
            .iter()
            .flatten()
            .copied()
            .filter(|id| !present.contains(id))
            .collect();
        for id in gone {
            if let Some(pad) = slots.unplug(id).and_then(pads::pad) {
                pad.set_connected(false);
            }
        }
        for slot in slots.overdue() {
            pads::notice(departure(slot));
        }
    }

    /// One controller's buttons and sticks into a pad, in the browser's
    /// standard order. The framework reads up as positive; the page reads
    /// down as positive, and a score written against the page expects the
    /// page's.
    fn read(controller: &GCController, pad: &pads::Pad) {
        // SAFETY: property reads on a live controller's profile.
        unsafe {
            let Some(profile) = controller.extendedGamepad() else {
                return;
            };
            let profile: &GCExtendedGamepad = &profile;
            pad.set_button(0, profile.buttonA().value());
            pad.set_button(1, profile.buttonB().value());
            pad.set_button(2, profile.buttonX().value());
            pad.set_button(3, profile.buttonY().value());
            pad.set_button(4, profile.leftShoulder().value());
            pad.set_button(5, profile.rightShoulder().value());
            pad.set_button(6, profile.leftTrigger().value());
            pad.set_button(7, profile.rightTrigger().value());
            pad.set_button(
                8,
                profile.buttonOptions().map_or(0.0, |button| button.value()),
            );
            pad.set_button(9, profile.buttonMenu().value());
            pad.set_button(
                10,
                profile
                    .leftThumbstickButton()
                    .map_or(0.0, |button| button.value()),
            );
            pad.set_button(
                11,
                profile
                    .rightThumbstickButton()
                    .map_or(0.0, |button| button.value()),
            );
            let dpad = profile.dpad();
            pad.set_button(12, dpad.up().value());
            pad.set_button(13, dpad.down().value());
            pad.set_button(14, dpad.left().value());
            pad.set_button(15, dpad.right().value());
            pad.set_button(
                16,
                profile.buttonHome().map_or(0.0, |button| button.value()),
            );
            let left = profile.leftThumbstick();
            pad.set_axis(0, left.xAxis().value());
            pad.set_axis(1, -left.yAxis().value());
            let right = profile.rightThumbstick();
            pad.set_axis(2, right.xAxis().value());
            pad.set_axis(3, -right.yAxis().value());
        }
    }

    /// Wait up to `at_most` for the main thread's pumping to show a pad:
    /// the framework reports a connected pad a moment after it is first
    /// asked. Ends early once one has shown up and had a moment for a
    /// second.
    fn settle(at_most: Duration) {
        super::ensure_polling();
        let deadline = Instant::now() + at_most;
        let mut seen_at: Option<Instant> = None;
        while Instant::now() < deadline {
            pump();
            std::thread::sleep(Duration::from_millis(10));
            if !pads::connected_pads().is_empty() {
                match seen_at {
                    None => seen_at = Some(Instant::now()),
                    Some(at) if at.elapsed() >= Duration::from_millis(150) => break,
                    Some(_) => {}
                }
            }
        }
    }

    /// The pads plugged in, for `rustel devices`. The main thread is
    /// pumping meanwhile; this only waits for what it finds.
    pub fn list() -> Result<Vec<(usize, String)>, String> {
        settle(Duration::from_millis(800));
        Ok(pads::connected_pads())
    }

    /// `rustel gamepad-monitor` through the framework: the pads present,
    /// then every change as a score reads it, off the state the main
    /// thread keeps.
    pub fn monitor(
        duration: Duration,
        stop: &dyn Fn() -> bool,
        mut print: impl FnMut(String),
    ) -> Result<(), String> {
        settle(Duration::from_millis(800));
        let present = pads::connected_pads();
        // The pads already listed arrived during the wait; their notices
        // are not news.
        let _ = pads::take_notices();
        for (slot, name) in &present {
            print(format!(
                "gamepad({slot}) {name} - read through the GameController framework"
            ));
        }
        if present.is_empty() {
            print("no gamepad plugged in (the framework lists none)".to_owned());
        }
        print(format!(
            "listening for {} seconds - press buttons and move the sticks",
            duration.as_secs()
        ));
        let mut before: Vec<Option<PadSnapshot>> = vec![None; rustel_core::gamepad::MAX_PADS];
        let mut reports = 0usize;
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline && !stop() {
            pump();
            std::thread::sleep(Duration::from_millis(8));
            for notice in pads::take_notices() {
                match notice {
                    Notice::Connected { pad, name } => {
                        print(format!("gamepad({pad}) {name} connected"))
                    }
                    Notice::Disconnected { pad, name } => {
                        print(format!("gamepad({pad}) {name} disconnected"))
                    }
                    Notice::Polling => {}
                    Notice::Problem(problem) => print(problem),
                }
            }
            for (slot, name) in pads::connected_pads() {
                let Some(pad) = pads::pad(slot) else { continue };
                let now = PadSnapshot::of(pad);
                if let Some(was) = before[slot] {
                    for line in describe_changes(&was, &now) {
                        reports += 1;
                        print(format!("gamepad({slot}) {name}: {line}"));
                    }
                }
                before[slot] = Some(now);
            }
        }
        if reports == 0 && !present.is_empty() {
            print(format!(
                "no button or stick moved in {} seconds",
                duration.as_secs()
            ));
        }
        Ok(())
    }
}

/// The pads through gilrs, on Linux and Windows.
#[cfg(not(target_os = "macos"))]
mod hid {
    use std::time::Duration;

    use gilrs::{Axis, Button, EventType, GamepadId, Gilrs};
    use rustel_core::gamepad as pads;

    use super::{AXIS_NAMES, Slots, departure, slot_name};

    /// A HID code as gilrs packs it on macOS: the usage page in the top
    /// half, the usage in the bottom. The Button page numbers its buttons
    /// from one; the Generic Desktop page carries the sticks as X, Y, Z and
    /// Rz. (Linux and Windows codes are small numbers with an empty page, so
    /// they never match here.)
    const HID_PAGE_GENERIC_DESKTOP: u32 = 0x01;
    const HID_PAGE_BUTTON: u32 = 0x09;
    const HID_USAGE_X: u32 = 0x30;
    const HID_USAGE_Y: u32 = 0x31;
    const HID_USAGE_Z: u32 = 0x32;
    const HID_USAGE_RZ: u32 = 0x35;

    /// A button gilrs has no name for, by its raw code: the pad's buttons in
    /// the order it numbers them, which is the order the browser gives an
    /// unmapped pad too - 1 is `a`, 2 `b`, 3 `x`, 4 `y`, and on down the
    /// standard mapping - so a pad no table knows still plays.
    pub(super) fn unmapped_button_slot(raw: u32) -> Option<usize> {
        let (page, usage) = (raw >> 16, raw & 0xffff);
        if page != HID_PAGE_BUTTON || usage == 0 {
            return None;
        }
        let slot = usize::try_from(usage - 1).ok()?;
        (slot < 16).then_some(slot)
    }

    /// An axis gilrs has no name for, by its raw code: X and Y the left
    /// stick, Z and Rz the right, as most pads and the browser have them.
    pub(super) fn unmapped_axis_slot(raw: u32) -> Option<usize> {
        let (page, usage) = (raw >> 16, raw & 0xffff);
        if page != HID_PAGE_GENERIC_DESKTOP {
            return None;
        }
        match usage {
            HID_USAGE_X => Some(0),
            HID_USAGE_Y => Some(1),
            HID_USAGE_Z => Some(2),
            HID_USAGE_RZ => Some(3),
            _ => None,
        }
    }

    /// The browser's standard mapping for gilrs's named buttons, and - on a
    /// pad gilrs has no table for - the raw code's place for a button it
    /// cannot name. A pad with a table keeps its unnamed buttons unnamed: a
    /// touchpad click or a paddle on such a pad is `Unknown` on purpose, and
    /// reading it by number would press some other button in the score.
    fn button_slot_of(button: Button, code: gilrs::ev::Code, raw_fallback: bool) -> Option<usize> {
        button_slot(button).or_else(|| {
            raw_fallback
                .then(|| unmapped_button_slot(code.into_u32()))
                .flatten()
        })
    }

    /// Whether a pad's unnamed buttons and axes may be read by their raw
    /// codes: only when gilrs has no SDL table for it (gilrs reports a pad
    /// without one as mapped by the driver).
    fn reads_raw_codes(gilrs: &Gilrs, id: GamepadId) -> bool {
        gilrs.gamepad(id).mapping_source() != gilrs::MappingSource::SdlMappings
    }

    /// The buttons a score reads: every button [`button_slot`] places except
    /// `b`. gilrs 0.11 keeps `b` in a device's default mapping even when the
    /// device has no such button, so a keyboard reports `b` too.
    pub(super) const READABLE_BUTTONS: [Button; pads::BUTTONS - 1] = [
        Button::South,
        Button::West,
        Button::North,
        Button::LeftTrigger,
        Button::RightTrigger,
        Button::LeftTrigger2,
        Button::RightTrigger2,
        Button::Select,
        Button::Start,
        Button::LeftThumb,
        Button::RightThumb,
        Button::DPadUp,
        Button::DPadDown,
        Button::DPadLeft,
        Button::DPadRight,
        Button::Mode,
    ];

    /// Whether a device gilrs accepted is a real pad. Linux sometimes
    /// hands gilrs a keyboard: udev calls the keyboard's media-key
    /// interface a joystick. That keyboard then takes `gamepad(0)` from
    /// the real pad. A real pad has at least one of [`READABLE_BUTTONS`]
    /// (a, a trigger, the d-pad). A keyboard has none.
    fn is_a_gamepad(gilrs: &Gilrs, id: GamepadId) -> bool {
        READABLE_BUTTONS
            .iter()
            .any(|&button| gilrs.gamepad(id).button_code(button).is_some())
    }

    /// What the monitor says of a device that is no pad, so a player sees
    /// why it takes no number.
    fn not_a_gamepad(name: &str) -> String {
        format!(
            "{name} - accepted as a gamepad, but not one: no button of it a score can read, so it takes no number"
        )
    }

    /// The browser's standard mapping for gilrs's named buttons; a button
    /// gilrs cannot name is nothing to a score.
    pub(super) fn button_slot(button: Button) -> Option<usize> {
        Some(match button {
            Button::South => 0,
            Button::East => 1,
            Button::West => 2,
            Button::North => 3,
            Button::LeftTrigger => 4,
            Button::RightTrigger => 5,
            Button::LeftTrigger2 => 6,
            Button::RightTrigger2 => 7,
            Button::Select => 8,
            Button::Start => 9,
            Button::LeftThumb => 10,
            Button::RightThumb => 11,
            Button::DPadUp => 12,
            Button::DPadDown => 13,
            Button::DPadLeft => 14,
            Button::DPadRight => 15,
            Button::Mode => 16,
            Button::C | Button::Z | Button::Unknown => return None,
        })
    }

    /// A pad arrived: it takes its number, and the studio is told - unless it
    /// is the pad that just left, back within the grace, in which case nobody
    /// need know. A device that is no pad never arrives.
    fn arrived(gilrs: &Gilrs, slots: &mut Slots<GamepadId>, id: GamepadId) {
        if !is_a_gamepad(gilrs, id) {
            return;
        }
        let name = gilrs.gamepad(id).name().to_owned();
        let Some(slot) = slots.place(id, &name) else {
            return;
        };
        let Some(pad) = pads::pad(slot) else {
            return;
        };
        for notice in slots.arrival_notices(slot, &name) {
            pads::notice(notice);
        }
        pads::set_name(slot, Some(name));
        pad.set_connected(true);
    }

    /// The pads plugged in, for `rustel devices`: gilrs on macOS reports a
    /// pad a moment after it opens, so this listens briefly before answering.
    pub fn list() -> Result<Vec<(usize, String)>, String> {
        let mut gilrs = Gilrs::new().map_err(|error| error.to_string())?;
        let deadline = std::time::Instant::now() + Duration::from_millis(400);
        while std::time::Instant::now() < deadline {
            let _ = gilrs.next_event_blocking(Some(Duration::from_millis(50)));
        }
        let pads: Vec<(usize, String)> = gilrs
            .gamepads()
            .filter(|(_, pad)| pad.is_connected())
            .filter(|(id, _)| is_a_gamepad(&gilrs, *id))
            .map(|(id, pad)| (usize::from(id), pad.name().to_owned()))
            .collect();
        Ok(numbered(pads))
    }

    /// The pads, keyed by driver id, under the numbers a score reads them
    /// by: placed in the driver's order through [`Slots::place`], as the
    /// poller places them, so a pad it refuses is left out.
    pub(super) fn numbered(mut pads: Vec<(usize, String)>) -> Vec<(usize, String)> {
        pads.sort();
        let mut slots = Slots::new();
        pads.into_iter()
            .filter_map(|(id, name)| Some((slots.place(id, &name)?, name)))
            .collect()
    }

    /// A raw code as the monitor spells it out: its page and usage, which is
    /// what a bug report about a pad that "does not work" needs to carry.
    pub(super) fn raw_code_text(raw: u32) -> String {
        format!("page {} usage {}", raw >> 16, raw & 0xffff)
    }

    /// Forwards gilrs's `log` lines to the monitor. gilrs reports a device it
    /// rejects, or an element it cannot place, only through `log`, so the
    /// monitor prints those lines with its own.
    struct ForwardGilrsLog(std::sync::Mutex<Option<std::sync::mpsc::Sender<String>>>);

    impl log::Log for ForwardGilrsLog {
        fn enabled(&self, metadata: &log::Metadata) -> bool {
            metadata.target().starts_with("gilrs") && metadata.level() <= log::Level::Debug
        }

        fn log(&self, record: &log::Record) {
            if !self.enabled(record.metadata()) {
                return;
            }
            if let Ok(sender) = self.0.lock()
                && let Some(sender) = sender.as_ref()
            {
                let _ = sender.send(format!(
                    "gilrs {}: {}",
                    record.level().as_str().to_lowercase(),
                    record.args()
                ));
            }
        }

        fn flush(&self) {}
    }

    static GILRS_LOG: ForwardGilrsLog = ForwardGilrsLog(std::sync::Mutex::new(None));

    /// Start forwarding gilrs's log lines; the receiver drains them. The
    /// process may already have a logger, in which case nothing is forwarded.
    fn listen_to_gilrs() -> Option<std::sync::mpsc::Receiver<String>> {
        let (sender, receiver) = std::sync::mpsc::channel();
        if let Ok(mut slot) = GILRS_LOG.0.lock() {
            *slot = Some(sender);
        }
        if log::set_logger(&GILRS_LOG).is_err() {
            return None;
        }
        log::set_max_level(log::LevelFilter::Debug);
        Some(receiver)
    }

    /// `rustel gamepad-monitor`: what every pad sends, as a score reads it,
    /// for `duration`. It first prints the pads that are plugged in, with
    /// their ids and whether gilrs knows them. Then it prints one line per
    /// event, such as `gamepad(0) Controller: a pressed` or
    /// `gamepad(0) Controller: x1 +0.72`. For a button or axis that no score
    /// can read, the line gives the raw code. It also prints gilrs's own log
    /// lines.
    pub fn monitor(
        duration: Duration,
        stop: &dyn Fn() -> bool,
        mut print: impl FnMut(String),
    ) -> Result<(), String> {
        let gilrs_log = listen_to_gilrs();
        let mut gilrs = Gilrs::new().map_err(|error| error.to_string())?;
        let mut slots: Slots<GamepadId> = Slots::new();
        // gilrs on macOS reports a pad a moment after it opens.
        let settle = std::time::Instant::now() + Duration::from_millis(400);
        while std::time::Instant::now() < settle {
            let _ = gilrs.next_event_blocking(Some(Duration::from_millis(50)));
        }
        let mut connected: Vec<GamepadId> = gilrs
            .gamepads()
            .filter(|(_, pad)| pad.is_connected())
            .map(|(id, _)| id)
            .collect();
        connected.sort_by_key(|id| usize::from(*id));
        for id in connected {
            let pad = gilrs.gamepad(id);
            let name = pad.name().to_owned();
            if !is_a_gamepad(&gilrs, id) {
                print(not_a_gamepad(&name));
                continue;
            }
            let Some(slot) = slots.place(id, &name) else {
                continue;
            };
            let known = match pad.mapping_source() {
                gilrs::MappingSource::None => "no mapping known - buttons by their number",
                gilrs::MappingSource::SdlMappings => "a known layout",
                gilrs::MappingSource::Driver => "the driver's layout - buttons by their number",
            };
            let ids = match (pad.vendor_id(), pad.product_id()) {
                (Some(vendor), Some(product)) => {
                    format!(" · vendor {vendor:04x} product {product:04x}")
                }
                _ => String::new(),
            };
            print(format!("gamepad({slot}) {name} - {known}{ids}"));
        }
        if slots.ids.is_empty() {
            print("no gamepad plugged in".to_owned());
        }
        print(format!(
            "listening for {} seconds - press buttons and move the sticks",
            duration.as_secs()
        ));
        let deadline = std::time::Instant::now() + duration;
        let mut reports = 0usize;
        while std::time::Instant::now() < deadline && !stop() {
            if let Some(receiver) = gilrs_log.as_ref() {
                while let Ok(line) = receiver.try_recv() {
                    print(line);
                }
            }
            let Some(event) = gilrs.next_event_blocking(Some(Duration::from_millis(50))) else {
                continue;
            };
            if !matches!(event.event, EventType::Connected | EventType::Disconnected) {
                reports += 1;
            }
            if event.event == EventType::Connected {
                let name = gilrs.gamepad(event.id).name().to_owned();
                if !is_a_gamepad(&gilrs, event.id) {
                    print(not_a_gamepad(&name));
                    continue;
                }
                if let Some(slot) = slots.place(event.id, &name) {
                    slots.unplugged_at[slot] = None;
                    print(format!("gamepad({slot}) {name} connected"));
                }
                continue;
            }
            if event.event == EventType::Disconnected {
                if let Some(slot) = slots.unplug(event.id) {
                    print(format!("gamepad({slot}) disconnected"));
                }
                continue;
            }
            let Some(slot) = slots.of(event.id) else {
                continue;
            };
            let who = format!("gamepad({slot}) {}", gilrs.gamepad(event.id).name());
            let raw = reads_raw_codes(&gilrs, event.id);
            let line = match event.event {
                EventType::ButtonPressed(button, code) => match button_slot_of(button, code, raw) {
                    Some(slot) => format!("{who}: {} pressed", slot_name(slot)),
                    None => format!(
                        "{who}: a button no score can read ({})",
                        raw_code_text(code.into_u32())
                    ),
                },
                EventType::ButtonReleased(button, code) => {
                    match button_slot_of(button, code, raw) {
                        Some(slot) => format!("{who}: {} released", slot_name(slot)),
                        None => continue,
                    }
                }
                EventType::ButtonChanged(button, value, code) => {
                    match button_slot_of(button, code, raw) {
                        Some(slot) if (0.01..0.99).contains(&value) => {
                            format!("{who}: {} {value:.2}", slot_name(slot))
                        }
                        _ => continue,
                    }
                }
                EventType::AxisChanged(axis, value, code) => {
                    // The page's convention: down and right positive.
                    let (name, value) = match axis {
                        Axis::LeftStickX => ("x1", value),
                        Axis::LeftStickY => ("y1", -value),
                        Axis::RightStickX => ("x2", value),
                        Axis::RightStickY => ("y2", -value),
                        Axis::DPadX => ("d-pad x", value),
                        Axis::DPadY => ("d-pad y", value),
                        _ => match raw.then(|| unmapped_axis_slot(code.into_u32())).flatten() {
                            Some(slot) => (AXIS_NAMES[slot], value),
                            None => {
                                print(format!(
                                    "{who}: an axis no score can read ({})",
                                    raw_code_text(code.into_u32())
                                ));
                                continue;
                            }
                        },
                    };
                    format!("{who}: {name} {value:+.2}")
                }
                _ => continue,
            };
            print(line);
        }
        if let Some(receiver) = gilrs_log.as_ref() {
            while let Ok(line) = receiver.try_recv() {
                print(line);
            }
        }
        if reports == 0 && !slots.ids.is_empty() {
            print(format!(
                "no button or stick report arrived in {} seconds: the system delivered none from this pad to this program. A pad the system drives itself - an Xbox One or Series pad over Bluetooth, a PlayStation pad, most USB pads - reports; one that needs a driver the system no longer ships (a wired Xbox 360-style pad) does not.",
                duration.as_secs()
            ));
        }
        Ok(())
    }

    pub(super) fn poll() {
        let mut gilrs = match Gilrs::new() {
            Ok(gilrs) => gilrs,
            Err(error) => {
                pads::note_problem(format!("gamepads: {error}"));
                return;
            }
        };
        pads::set_polling();
        let mut slots: Slots<GamepadId> = Slots::new();
        // Whatever was plugged in before the first ask. (macOS reports a pad a
        // moment later, as a Connected event; both roads lead to `arrived`.)
        let connected: Vec<GamepadId> = gilrs.gamepads().map(|(id, _)| id).collect();
        for id in connected {
            arrived(&gilrs, &mut slots, id);
        }
        loop {
            let event = gilrs.next_event_blocking(Some(Duration::from_millis(50)));
            // A pad gone past the grace is gone for real.
            for slot in slots.overdue() {
                pads::notice(departure(slot));
            }
            let Some(event) = event else {
                continue;
            };
            if event.event == EventType::Connected {
                arrived(&gilrs, &mut slots, event.id);
                continue;
            }
            if event.event == EventType::Disconnected {
                if let Some(pad) = slots.unplug(event.id).and_then(pads::pad) {
                    pad.set_connected(false);
                }
                continue;
            }
            let Some(slot) = slots.of(event.id) else {
                continue;
            };
            let Some(pad) = pads::pad(slot) else {
                continue;
            };
            let raw = reads_raw_codes(&gilrs, event.id);
            match event.event {
                EventType::Connected | EventType::Disconnected => {}
                EventType::ButtonPressed(button, code) => {
                    if let Some(slot) = button_slot_of(button, code, raw) {
                        pad.set_button(slot, 1.0);
                    }
                }
                EventType::ButtonReleased(button, code) => {
                    if let Some(slot) = button_slot_of(button, code, raw) {
                        pad.set_button(slot, 0.0);
                    }
                }
                EventType::ButtonChanged(button, value, code) => {
                    if let Some(slot) = button_slot_of(button, code, raw) {
                        pad.set_button(slot, value);
                    }
                }
                // gilrs reads up as positive; the page reads down as positive,
                // and a score written against the page expects the page's.
                EventType::AxisChanged(axis, value, code) => match axis {
                    Axis::LeftStickX => pad.set_axis(0, value),
                    Axis::LeftStickY => pad.set_axis(1, -value),
                    Axis::RightStickX => pad.set_axis(2, value),
                    Axis::RightStickY => pad.set_axis(3, -value),
                    // A d-pad that arrives as a hat is still four buttons.
                    Axis::DPadX => {
                        pad.set_button(14, f32::from(u8::from(value < -0.5)));
                        pad.set_button(15, f32::from(u8::from(value > 0.5)));
                    }
                    Axis::DPadY => {
                        pad.set_button(12, f32::from(u8::from(value > 0.5)));
                        pad.set_button(13, f32::from(u8::from(value < -0.5)));
                    }
                    // An axis gilrs cannot name, by its raw code, on a pad it
                    // has no table for. The raw report reads down as positive
                    // already.
                    _ => {
                        if raw && let Some(slot) = unmapped_axis_slot(code.into_u32()) {
                            pad.set_axis(slot, value);
                        }
                    }
                },
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A change between two moments reads as a score would say it: a
    /// button crossing half, a trigger between, a stick that moved.
    #[test]
    fn changes_between_two_moments_read_as_a_score_says_them() {
        let mut before = PadSnapshot::default();
        let mut after = PadSnapshot::default();
        after.buttons[0] = 1.0;
        after.buttons[7] = 0.4;
        after.axes[0] = 0.72;
        after.axes[3] = -0.01;
        assert_eq!(
            describe_changes(&before, &after),
            vec!["a pressed", "rt 0.40", "x1 +0.72"]
        );
        before = after;
        after.buttons[0] = 0.0;
        after.buttons[7] = 1.0;
        assert_eq!(
            describe_changes(&before, &after),
            vec!["a released", "rt pressed"]
        );
        assert!(describe_changes(&after, &after).is_empty());
    }

    /// A pad gilrs has no table for still plays: its buttons by the
    /// number it gives them, its sticks by the usages every pad shares;
    /// a code from another page, or past the buttons a score can name,
    /// is nothing.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn a_button_gilrs_cannot_name_plays_by_its_number() {
        use super::hid::{raw_code_text, unmapped_axis_slot, unmapped_button_slot};
        let code = |page: u32, usage: u32| (page << 16) | usage;
        assert_eq!(unmapped_button_slot(code(9, 1)), Some(0), "button 1 is a");
        assert_eq!(unmapped_button_slot(code(9, 4)), Some(3), "button 4 is y");
        assert_eq!(unmapped_button_slot(code(9, 16)), Some(15));
        assert_eq!(
            unmapped_button_slot(code(9, 17)),
            None,
            "past the page's buttons"
        );
        assert_eq!(unmapped_button_slot(code(9, 0)), None);
        assert_eq!(
            unmapped_button_slot(code(1, 1)),
            None,
            "not the button page"
        );
        assert_eq!(
            unmapped_button_slot(0x130),
            None,
            "an evdev code has no page"
        );
        assert_eq!(unmapped_axis_slot(code(1, 0x30)), Some(0), "X is x1");
        assert_eq!(unmapped_axis_slot(code(1, 0x31)), Some(1));
        assert_eq!(unmapped_axis_slot(code(1, 0x32)), Some(2), "Z is x2");
        assert_eq!(unmapped_axis_slot(code(1, 0x35)), Some(3), "Rz is y2");
        assert_eq!(
            unmapped_axis_slot(code(1, 0x39)),
            None,
            "the hat is not a stick"
        );
        assert_eq!(unmapped_axis_slot(code(9, 0x30)), None);
        assert_eq!(slot_name(0), "a");
        assert_eq!(slot_name(16), "home");
        assert_eq!(raw_code_text(code(9, 20)), "page 9 usage 20");
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn the_standard_mapping_numbers_the_buttons_the_way_the_page_does() {
        use super::hid::button_slot;
        use gilrs::Button;
        assert_eq!(button_slot(Button::South), Some(0));
        assert_eq!(button_slot(Button::North), Some(3));
        assert_eq!(button_slot(Button::LeftTrigger2), Some(6));
        assert_eq!(button_slot(Button::Select), Some(8));
        assert_eq!(button_slot(Button::DPadRight), Some(15));
        assert_eq!(button_slot(Button::Unknown), None);
        for (names, index) in rustel_core::gamepad::BUTTON_NAMES {
            assert!(usize::from(*index) < 16, "{names:?} names a mapped button");
        }
    }

    /// The pad check probes every button a score reads except `b`: gilrs 0.11
    /// leaves `b` in every default mapping, keyboard or pad.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn a_pad_is_told_from_a_keyboard_by_the_buttons_a_score_reads() {
        use super::hid::{READABLE_BUTTONS, button_slot};
        use gilrs::Button;
        assert_eq!(READABLE_BUTTONS.len(), rustel_core::gamepad::BUTTONS - 1);
        assert!(!READABLE_BUTTONS.contains(&Button::East));
        for &button in &READABLE_BUTTONS {
            assert!(
                button_slot(button).is_some(),
                "{button:?} is no button a score reads"
            );
        }
        for &unplaced in &[Button::C, Button::Z, Button::Unknown] {
            assert!(!READABLE_BUTTONS.contains(&unplaced));
            assert!(button_slot(unplaced).is_none());
        }
    }

    /// `rustel devices` numbers the pads as a score reads them: in the
    /// driver's order from `gamepad(0)`, with no fifth pad.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn the_listing_never_numbers_a_pad_a_score_cannot_read() {
        use super::hid::numbered;
        let pad = |id: usize, name: &str| (id, name.to_owned());
        assert_eq!(
            numbered(vec![pad(2, "C"), pad(0, "A"), pad(1, "B")]),
            vec![pad(0, "A"), pad(1, "B"), pad(2, "C")],
            "the driver's order, renumbered from gamepad(0)"
        );
        let six: Vec<(usize, String)> = (0..6).map(|id| (id, format!("Pad {id}"))).collect();
        assert_eq!(
            numbered(six),
            vec![
                pad(0, "Pad 0"),
                pad(1, "Pad 1"),
                pad(2, "Pad 2"),
                pad(3, "Pad 3")
            ],
            "a fifth pad is nobody's, as a score's slots say"
        );
    }

    /// A pad keeps its number across a re-plug: its slot waits for its
    /// name, a stranger takes the first empty slot, and a fifth pad is
    /// nobody's.
    #[test]
    fn a_pad_keeps_its_number_across_a_replug() {
        let mut slots: Slots<u32> = Slots::new();
        pads::set_name(0, None);
        pads::set_name(1, None);
        assert_eq!(slots.place(10, "Controller"), Some(0));
        pads::set_name(0, Some("Controller".to_owned()));
        assert_eq!(slots.place(11, "Other Pad"), Some(1));
        pads::set_name(1, Some("Other Pad".to_owned()));
        assert_eq!(
            slots.place(10, "Controller"),
            Some(0),
            "known: its own slot"
        );

        assert_eq!(slots.unplug(10), Some(0));
        assert_eq!(slots.of(10), None);
        // Back under a new driver id, same name: the same number.
        assert_eq!(slots.place(12, "Controller"), Some(0));
        assert!(
            slots.unplugged_at[0].is_some(),
            "back within the grace: not news yet"
        );
        assert!(slots.overdue().is_empty(), "and not overdue");

        // A stranger arriving while slot 0 is empty takes slot 0.
        slots.unplug(12);
        assert_eq!(slots.place(13, "Third Pad"), Some(0));
        pads::set_name(0, Some("Third Pad".to_owned()));
        assert_eq!(slots.place(14, "Fourth"), Some(2));
        assert_eq!(slots.place(15, "Fifth"), Some(3));
        assert_eq!(slots.place(16, "Sixth"), None, "a fifth pad is nobody's");
        pads::set_name(0, None);
        pads::set_name(1, None);
    }

    /// The replug grace silences only the pad that left: the same pad back
    /// within it owes no notice, while a different pad taking the slot owes the
    /// old pad's leaving and then its own arrival. Runs on slot 2, which no other
    /// test in this crate names.
    #[test]
    fn a_stranger_within_the_grace_is_both_notices_and_the_same_pad_silence() {
        let mut slots: Slots<u32> = Slots::new();
        pads::set_name(2, None);
        // Slots 0 and 1, whose names other tests share, stay occupied.
        assert_eq!(slots.place(10, "Warmup"), Some(0));
        assert_eq!(slots.place(11, "Warmup"), Some(1));
        assert_eq!(slots.place(12, "Controller"), Some(2));
        assert_eq!(
            slots.arrival_notices(2, "Controller"),
            vec![Notice::Connected {
                pad: 2,
                name: "Controller".to_owned()
            }],
            "a slot's first pad is news"
        );
        pads::set_name(2, Some("Controller".to_owned()));

        // The same pad, back under a new driver id within the grace.
        assert_eq!(slots.unplug(12), Some(2));
        assert_eq!(slots.place(13, "Controller"), Some(2));
        assert!(
            slots.arrival_notices(2, "Controller").is_empty(),
            "nobody need know"
        );
        assert!(slots.overdue().is_empty(), "and nothing left to tell");

        // A different pad, taking the seat within the grace.
        assert_eq!(slots.unplug(13), Some(2));
        assert_eq!(
            slots.place(14, "Other Pad"),
            Some(2),
            "the first empty slot"
        );
        assert_eq!(
            slots.arrival_notices(2, "Other Pad"),
            vec![
                Notice::Disconnected {
                    pad: 2,
                    name: "Controller".to_owned()
                },
                Notice::Connected {
                    pad: 2,
                    name: "Other Pad".to_owned()
                },
            ],
            "the leaving was never told, and the arrival resets the baseline"
        );
        assert!(
            slots.overdue().is_empty(),
            "the timestamp is spent: the old pad's leaving is not told twice"
        );
        pads::set_name(2, None);
    }
}
