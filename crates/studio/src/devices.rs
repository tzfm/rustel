//! The device dock: what the studio is playing through, and what MIDI ports
//! this machine offers.
//!
//! Enumerating devices talks to CoreAudio, ALSA or WASAPI, any of which can
//! take milliseconds and occasionally much longer when a USB interface is
//! waking up. That work therefore happens on a throwaway thread and arrives
//! through a channel; the draw loop only ever reads the last result it was
//! given.

use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthStr;

use super::theme::Theme;

/// How long a listing is trusted while the picker is open.
pub const ACTIVE_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
/// How long it is trusted while only the dock's summary is on screen.
///
/// Enumerating audio and MIDI hosts is not free, but a minute is not "a few
/// seconds of the fact": someone who plugs a controller in and plays a bar
/// has already decided the studio did not notice. Five gives the same answer
/// with or without the picker open, and the probe runs off the frame thread.
pub const IDLE_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
/// Longest device name kept. Some CoreAudio aggregate names are enormous and
/// a status dock is not the place to discover that.
const MAX_NAME_BYTES: usize = 96;
/// Upper bound on ports listed per family, so a virtual-port storm cannot
/// turn the panel into an unbounded allocation.
const MAX_PORTS: usize = 64;
/// The audio input row that is not an input: choosing it closes whichever
/// one is open. It leads the list rather than trailing it the way `silent`
/// trails the outputs, because the window shown always ends at the cursor
/// and the cursor arrives at 0: past twelve inputs a last row would be off
/// the bottom on arrival, and the picker binds no Home or Page keys to get
/// there. A machine with that many interfaces is the one most likely to
/// want the switch. Never a selector - the engine is told there is no
/// input with `None`, and this string must not reach `open_input`.
pub const NO_INPUT_NAME: &str = "none";

/// Which family of ports a panel row belongs to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceKind {
    /// Audio outputs. Selecting one moves playback onto it.
    AudioOutput,
    /// Audio inputs. Selecting one is what `s("in")` plays.
    AudioInput,
    /// Every MIDI port, one row a device, with a box each way that decides
    /// whether this machine may open it, and the clock ports under them.
    ///
    /// One tab rather than an `in` tab and an `out` tab: a controller that
    /// both sends and receives is one thing on the desk, and splitting it
    /// across two tabs made a player hunt for the same device twice.
    Midi,
    /// The pads plugged in, numbered the way a score numbers them.
    /// Selecting one copies `gamepad(n)`.
    Gamepad,
}

impl DeviceKind {
    pub const ALL: [Self; 4] = [
        Self::AudioOutput,
        Self::AudioInput,
        Self::Midi,
        Self::Gamepad,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::AudioOutput => "audio out",
            Self::AudioInput => "audio in",
            Self::Midi => "midi",
            Self::Gamepad => "gamepads",
        }
    }

    /// What a person most likely wants on their clipboard: the bare port
    /// name, ready to drop between the quotes of whichever call the score
    /// is already making. Wrapping it in `.midi("…")` only got in the way
    /// when the call was already typed.
    pub fn snippet(self, name: &str) -> String {
        name.to_owned()
    }
}

/// One selectable port.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceEntry {
    pub name: String,
    /// What the backend calls it - `coreaudio:00-00-5E-00-53-01:output`.
    ///
    /// The engine reports the device it opened by this, and the list
    /// shows the friendly name, so the row that is playing can only be
    /// found by keeping both. Empty for rows with no device behind them:
    /// `silent`, `none`, a MIDI port, a pad.
    pub id: String,
    /// Extra detail shown after the name, such as `48000 Hz · 2ch`.
    pub detail: String,
    /// True for the host's default output.
    pub is_default: bool,
}

impl DeviceEntry {
    /// Whether this row is the device the engine named.
    ///
    /// By id first, because that is what the engine reports; by name for
    /// the rows that have no id, and for a host that gives a device no
    /// handle of its own.
    pub fn is_named(&self, current: Option<&str>) -> bool {
        let Some(current) = current else {
            return false;
        };
        (!self.id.is_empty() && self.id == current) || self.name == current
    }
}

/// Which MIDI ports this machine may open, in each direction.
///
/// A port is gated rather than merely hidden: a disabled destination is not
/// opened at all, and a score naming it is told so. If every port were open
/// to any score, a set could play into a device the player did not choose.
///
/// Held by name because that is what a score writes: `.midi("loopMIDI
/// Port")`. The panel merges the two directions into one row per name, but
/// the sets stay separate because a device can be wanted one way and not
/// the other: a controller you play from and never send to.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MidiEnablement {
    outputs: DirectionGate,
    inputs: DirectionGate,
}

/// One direction's ports: what the host reports, and which of them are on.
///
/// Both halves, because a score does not name a port: it names a selector.
/// `.midi("Maschine")` is one distinctive word, resolved against the host's
/// list by the first case-insensitive hit. Deciding whether that is allowed
/// means resolving it the same way the opener will, and then asking about
/// the name it landed on. Holding only the enabled names would resolve
/// "Maschine" against a list the disabled ports had been removed from, and
/// answer about a different device than the one that would actually open.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct DirectionGate {
    /// The host's own list, in the host's own order - the list the opener
    /// will index into.
    present: Vec<String>,
    enabled: std::collections::BTreeSet<String>,
}

impl MidiEnablement {
    /// What the preferences kept, with the absent case filled in from the
    /// ports actually present.
    ///
    /// Absent is a studio that predates this switch, and the honest reading
    /// of it is "everything was allowed", because everything WAS: there was
    /// no gate. Taking absent as "nothing allowed" would open the upgrade
    /// with every set's MIDI silently dead and no hint that a checkbox now
    /// stands between the score and the port.
    pub fn from_prefs(
        outputs: Option<&[String]>,
        inputs: Option<&[String]>,
        present: &DeviceInventory,
    ) -> Self {
        let resolve = |kept: Option<&[String]>, ports: &[DeviceEntry]| DirectionGate {
            present: ports.iter().map(|port| port.name.clone()).collect(),
            enabled: kept
                .map(|kept| kept.iter().cloned().collect())
                .unwrap_or_else(|| ports.iter().map(|port| port.name.clone()).collect()),
        };
        Self {
            outputs: resolve(outputs, &present.midi_outputs),
            inputs: resolve(inputs, &present.midi_inputs),
        }
    }

    /// Take the host's current port lists, keeping every tick.
    ///
    /// A device unplugged and plugged back in comes back with its box still
    /// ticked, because the list of what is enabled is kept by name and never
    /// pruned to what happens to be present. Pruning would forget a synth
    /// the moment its cable was pulled, and turn it off for the next time it
    /// was connected.
    pub fn refresh_present(&mut self, present: &DeviceInventory) {
        self.outputs.present = present
            .midi_outputs
            .iter()
            .map(|port| port.name.clone())
            .collect();
        self.inputs.present = present
            .midi_inputs
            .iter()
            .map(|port| port.name.clone())
            .collect();
    }

    /// Whether a port is switched on, by its exact name. The panel's rows
    /// ask this, because a row is a name.
    pub fn is_enabled(&self, direction: MidiDirection, port: &str) -> bool {
        self.set(direction).enabled.contains(port)
    }

    /// Whether a SCORE may have the port its selector names.
    ///
    /// Resolved by [`rustel_midi::port_for_selector`], as the opener resolves
    /// it, then asked about the name it landed on. A selector that matches
    /// nothing is allowed through: the opener's own "not found, available:
    /// ..." is a better answer than a refusal about a port that does not
    /// exist, and hiding it behind this gate would turn a typo into a mystery.
    pub fn allows(&self, direction: MidiDirection, selector: &str) -> bool {
        let gate = self.set(direction);
        match rustel_midi::port_for_selector(selector, &gate.present) {
            Some(index) => gate
                .present
                .get(index)
                .is_none_or(|name| gate.enabled.contains(name)),
            None => true,
        }
    }

    /// The port a selector lands on, for saying which one was refused.
    pub fn port_named(&self, direction: MidiDirection, selector: &str) -> Option<&str> {
        let gate = self.set(direction);
        rustel_midi::port_for_selector(selector, &gate.present)
            .and_then(|index| gate.present.get(index))
            .map(String::as_str)
    }

    /// Turn one direction of one port on or off, and say what it became.
    pub fn toggle(&mut self, direction: MidiDirection, port: &str) -> bool {
        let gate = match direction {
            MidiDirection::Out => &mut self.outputs,
            MidiDirection::In => &mut self.inputs,
        };
        if gate.enabled.remove(port) {
            return false;
        }
        gate.enabled.insert(port.to_owned());
        true
    }

    fn set(&self, direction: MidiDirection) -> &DirectionGate {
        match direction {
            MidiDirection::Out => &self.outputs,
            MidiDirection::In => &self.inputs,
        }
    }

    /// The enabled names, for the preferences to keep verbatim.
    pub fn kept(&self, direction: MidiDirection) -> Vec<String> {
        self.set(direction).enabled.iter().cloned().collect()
    }
}

/// The rows under the MIDI ports: clock in, then clock out.
pub const MIDI_CLOCK_ROWS: usize = 2;

/// What a row of the MIDI tab is, by its index in the list.
///
/// The ports come first and the clock rows after, so a device plugged in
/// pushes the clock rows down rather than moving a port out from under the
/// selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MidiPanelRow {
    Port(usize),
    ClockIn,
    ClockOut,
}

impl MidiPanelRow {
    pub fn at(index: usize, ports: usize) -> Option<Self> {
        match index.checked_sub(ports) {
            None => Some(Self::Port(index)),
            Some(0) => Some(Self::ClockIn),
            Some(1) => Some(Self::ClockOut),
            Some(_) => None,
        }
    }
}

/// Which way MIDI is flowing for a given row or port.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MidiDirection {
    /// The studio sends: a score's `.midi("...")`.
    Out,
    /// The studio listens: a controller a score reads.
    In,
}

impl MidiDirection {
    pub const BOTH: [Self; 2] = [Self::In, Self::Out];

    pub fn label(self) -> &'static str {
        match self {
            Self::In => "in",
            Self::Out => "out",
        }
    }
}

/// One line of the merged MIDI list: a port name, and what it can do.
///
/// A device that both sends and receives is one row with two boxes rather
/// than two rows in two tabs, because it is one thing on the desk. Where a
/// name exists in only one direction the other reads as a dash: nothing to
/// tick, as opposed to something switched off.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MidiRow {
    pub name: String,
    pub has_in: bool,
    pub has_out: bool,
    pub in_enabled: bool,
    pub out_enabled: bool,
}

impl MidiRow {
    /// Whether this row offers anything in that direction at all.
    pub fn has(&self, direction: MidiDirection) -> bool {
        match direction {
            MidiDirection::In => self.has_in,
            MidiDirection::Out => self.has_out,
        }
    }

    pub fn enabled(&self, direction: MidiDirection) -> bool {
        match direction {
            MidiDirection::In => self.in_enabled,
            MidiDirection::Out => self.out_enabled,
        }
    }

    /// The box, or the dash where there is no port to tick.
    pub fn box_for(&self, direction: MidiDirection) -> &'static str {
        if !self.has(direction) {
            return " - ";
        }
        if self.enabled(direction) {
            "[x]"
        } else {
            "[ ]"
        }
    }
}

/// Ports each way, counted apart: MIDI learn listens only to inputs.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MidiPortCounts {
    pub outputs: usize,
    pub inputs: usize,
}

impl MidiPortCounts {
    /// Whether there is no port either way.
    pub fn is_empty(self) -> bool {
        self.outputs == 0 && self.inputs == 0
    }
}

/// A snapshot of everything the dock knows.
#[derive(Clone, Debug, Default)]
pub struct DeviceInventory {
    pub audio_outputs: Vec<DeviceEntry>,
    pub audio_inputs: Vec<DeviceEntry>,
    pub midi_outputs: Vec<DeviceEntry>,
    pub midi_inputs: Vec<DeviceEntry>,
    /// The pads plugged in right now, refreshed from the poller's state
    /// rather than probed: they come and go while the dock is open.
    pub gamepads: Vec<DeviceEntry>,
    /// Why a family is empty, when it is empty because something failed.
    pub problem: Option<String>,
}

impl DeviceInventory {
    pub fn entries(&self, kind: DeviceKind) -> &[DeviceEntry] {
        match kind {
            DeviceKind::AudioOutput => &self.audio_outputs,
            DeviceKind::AudioInput => &self.audio_inputs,
            // The MIDI tab lists merged rows, not entries: see `midi_rows`.
            DeviceKind::Midi => &[],
            DeviceKind::Gamepad => &self.gamepads,
        }
    }

    /// Every MIDI port one way, ticked or not.
    ///
    /// Not gated, on purpose: the completion list and the linter describe
    /// what the host HAS, and a port that exists but is switched off is still
    /// a name a score can be written against - it is refused when it plays,
    /// with a message saying where to switch it on, not hidden while typing.
    pub fn midi_ports(&self, direction: MidiDirection) -> &[DeviceEntry] {
        match direction {
            MidiDirection::Out => &self.midi_outputs,
            MidiDirection::In => &self.midi_inputs,
        }
    }

    /// How many rows a tab lists. The MIDI tab lists its merged ports and
    /// then the two clock rows.
    ///
    /// Asked by everything that sizes, scrolls or hit-tests the list, so the
    /// clock rows are counted in one place and every one of those agrees on
    /// where a row is.
    pub fn row_count(&self, kind: DeviceKind) -> usize {
        match kind {
            DeviceKind::Midi => self.midi_device_count() + MIDI_CLOCK_ROWS,
            _ => self.entries(kind).len(),
        }
    }

    /// How many devices there are: distinct port names, either way.
    ///
    /// Not [`Self::midi_port_counts`], which counts every port each way: a
    /// device that both sends and receives is two ports and one row, and the
    /// list is sized by rows.
    ///
    /// Needs no enablement: which boxes are ticked never changes how many
    /// rows exist.
    pub fn midi_device_count(&self) -> usize {
        let mut names: Vec<&str> = Vec::new();
        for port in self.midi_outputs.iter().chain(&self.midi_inputs) {
            if !names.contains(&port.name.as_str()) {
                names.push(&port.name);
            }
        }
        names.len()
    }

    /// The MIDI ports as one list: a row per name, whichever way it goes.
    ///
    /// Destinations lead, then any source whose name has not already been
    /// seen. Most hardware reports the same name both ways and collapses to
    /// one row; a keyboard that only sends, or a synth that only receives,
    /// keeps its row and shows a dash where it has no port.
    ///
    /// Order follows the host's own, not the alphabet: the list a player has
    /// already learned the shape of is the one the host gave, and re-sorting
    /// it here would move rows under a hand that had stopped reading them.
    pub fn midi_rows(&self, enabled: &MidiEnablement) -> Vec<MidiRow> {
        let mut rows: Vec<MidiRow> = Vec::new();
        let mut push = |name: &str, direction: MidiDirection| {
            if let Some(row) = rows.iter_mut().find(|row| row.name == name) {
                match direction {
                    MidiDirection::In => row.has_in = true,
                    MidiDirection::Out => row.has_out = true,
                }
                return;
            }
            rows.push(MidiRow {
                name: name.to_owned(),
                has_in: direction == MidiDirection::In,
                has_out: direction == MidiDirection::Out,
                in_enabled: false,
                out_enabled: false,
            });
        };
        for port in &self.midi_outputs {
            push(&port.name, MidiDirection::Out);
        }
        for port in &self.midi_inputs {
            push(&port.name, MidiDirection::In);
        }
        for row in &mut rows {
            row.in_enabled = row.has_in && enabled.is_enabled(MidiDirection::In, &row.name);
            row.out_enabled = row.has_out && enabled.is_enabled(MidiDirection::Out, &row.name);
        }
        rows
    }

    /// The pads as the poller sees them now: one row a pad, named the way
    /// a score names it, with the pad's own name and whether it is moving.
    pub fn refresh_gamepads(&mut self) {
        self.gamepads = gamepad_entries();
    }

    /// The ports each way, as the footer chip and the mixer desk show them.
    pub fn midi_port_counts(&self) -> MidiPortCounts {
        MidiPortCounts {
            outputs: self.midi_outputs.len(),
            inputs: self.midi_inputs.len(),
        }
    }

    /// The inventory of a host with nothing attached and nothing failing:
    /// the output that makes no sound but keeps the clock, the meters and
    /// the visualizers going; at the head of the inputs, the one that is
    /// not an input (an interface unplugged since it was chosen leaves no
    /// row of its own to press Enter on, so without it there was no way
    /// back to silence at all); and no port of any other kind anywhere.
    ///
    /// The harness stands in for the host with exactly this, so a runner's
    /// hardware - or its absent MIDI backend - never reaches a test's rows.
    pub fn no_hardware() -> Self {
        Self {
            audio_outputs: vec![DeviceEntry {
                name: rustel_audio::SILENT_OUTPUT_NAME.to_owned(),
                id: String::new(),
                detail: "no sound · clock and visuals only".to_owned(),
                is_default: false,
            }],
            audio_inputs: vec![DeviceEntry {
                name: NO_INPUT_NAME.to_owned(),
                id: String::new(),
                detail: "no input · s(\"in\") is silence".to_owned(),
                is_default: false,
            }],
            midi_outputs: Vec::new(),
            midi_inputs: Vec::new(),
            gamepads: Vec::new(),
            problem: None,
        }
    }

    /// Enumerate every family. Blocking; run this off the UI thread.
    ///
    /// Builds on [`Self::no_hardware`]: those two rows belong on every
    /// listing whatever the host says, and a host that cannot be read at
    /// all is a problem to name, not rows to drop.
    pub fn probe() -> Self {
        let mut inventory = Self::from_host(rustel_audio::audio_device_inventory(), probe_midi());
        // The pads too: a poll replaces the whole inventory, so one probed
        // without them wipes the rows a keypress had just filled, and the tab
        // reads "no pad plugged in" beside a footer chip that counts one.
        inventory.refresh_gamepads();
        inventory
    }

    /// The listing a host's answers make, laid over [`Self::no_hardware`].
    fn from_host(
        audio: Result<
            (
                Vec<rustel_audio::AudioDeviceInfo>,
                Vec<rustel_audio::AudioDeviceInfo>,
            ),
            rustel_audio::DevicePlaybackError,
        >,
        (midi_outputs, midi_inputs, midi_problem): (
            Vec<DeviceEntry>,
            Vec<DeviceEntry>,
            Option<String>,
        ),
    ) -> Self {
        let mut inventory = Self::no_hardware();
        let mut problems = Vec::new();
        match audio {
            Ok((outputs, inputs)) => {
                // Real outputs lead the list, ahead of the silent one; real
                // inputs follow the not-an-input row it is a way back from.
                inventory
                    .audio_outputs
                    .splice(0..0, outputs.into_iter().take(MAX_PORTS).map(audio_entry));
                inventory
                    .audio_inputs
                    .extend(inputs.into_iter().take(MAX_PORTS).map(audio_entry));
            }
            Err(error) => problems.push(format!("audio devices: {error}")),
        }
        inventory.midi_outputs = midi_outputs;
        inventory.midi_inputs = midi_inputs;
        problems.extend(midi_problem);
        if !problems.is_empty() {
            inventory.problem = Some(problems.join("; "));
        }
        inventory
    }
}

/// One host-read audio device as the panel lists it.
fn audio_entry(device: rustel_audio::AudioDeviceInfo) -> DeviceEntry {
    DeviceEntry {
        name: truncate(&device.name),
        id: device.id,
        detail: match (device.sample_rate, device.channels) {
            (Some(rate), Some(channels)) => format!("{rate} Hz · {channels}ch"),
            (Some(rate), None) => format!("{rate} Hz"),
            _ => String::new(),
        },
        is_default: device.is_default,
    }
}

fn probe_midi() -> (Vec<DeviceEntry>, Vec<DeviceEntry>, Option<String>) {
    let plain = |names: Vec<String>| {
        names
            .into_iter()
            .take(MAX_PORTS)
            .map(|name| DeviceEntry {
                name: truncate(&name),
                id: String::new(),
                detail: String::new(),
                is_default: false,
            })
            .collect::<Vec<_>>()
    };
    let mut problem = None;
    let outputs = match rustel_midi::MidiSender::ports() {
        Ok(names) => plain(names),
        Err(error) => {
            problem = Some(format!("midi outputs: {error}"));
            Vec::new()
        }
    };
    let inputs = match rustel_midi::MidiSender::input_ports() {
        Ok(names) => plain(names),
        Err(error) => {
            problem
                .get_or_insert_with(String::new)
                .push_str(&format!("midi inputs: {error}"));
            Vec::new()
        }
    };
    (outputs, inputs, problem)
}

/// One dock row a connected pad.
fn gamepad_entries() -> Vec<DeviceEntry> {
    rustel_core::gamepad::connected_pads()
        .into_iter()
        .map(|(slot, name)| {
            let activity =
                match rustel_core::gamepad::pad(slot).and_then(|pad| pad.idle_for_millis()) {
                    Some(idle) if idle < 300 => "● moving".to_owned(),
                    Some(idle) => format!("idle {}s", idle / 1_000),
                    None => "connected".to_owned(),
                };
            DeviceEntry {
                name: format!("gamepad({slot})"),
                id: String::new(),
                detail: format!("{} · {activity}", truncate(&name)),
                is_default: slot == 0,
            }
        })
        .collect()
}

fn truncate(name: &str) -> String {
    if name.len() <= MAX_NAME_BYTES {
        return name.to_owned();
    }
    let mut cut = MAX_NAME_BYTES;
    while cut > 0 && !name.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &name[..cut])
}

/// Background enumeration with at most one probe in flight.
#[derive(Debug, Default)]
pub struct DeviceScanner {
    inventory: DeviceInventory,
    pending: Option<Receiver<DeviceInventory>>,
    /// When the most recent probe was started. The refresh cadence is
    /// measured from here so opening the picker can shorten it without
    /// rescheduling anything.
    last_requested: Option<Instant>,
    scanned: bool,
    /// Set by [`Self::pin`]: the host is never probed again.
    pinned: bool,
}

impl DeviceScanner {
    /// The pads change under the dock without a probe: refresh their rows.
    pub fn refresh_gamepads(&mut self) {
        self.inventory.refresh_gamepads();
    }

    pub fn inventory(&self) -> &DeviceInventory {
        &self.inventory
    }

    /// True once a probe has completed at least once.
    pub fn has_scanned(&self) -> bool {
        self.scanned
    }

    /// Stand in for the host with one inventory, permanently. No probe is
    /// started or adopted afterwards, so the machine a test runs on - its
    /// ports, or its absent MIDI backend - cannot reach a test's rows. The
    /// harness path; the stand-in counts as the completed probe.
    pub fn pin(&mut self, inventory: DeviceInventory) {
        self.inventory = inventory;
        self.pending = None;
        self.last_requested = None;
        self.scanned = true;
        self.pinned = true;
    }

    /// Start a probe if one is due and none is running.
    pub fn request(&mut self, now: Instant) {
        if self.pinned || self.pending.is_some() {
            return;
        }
        self.last_requested = Some(now);
        let (sender, receiver) = channel();
        // Detached: nothing waits for this thread, and a probe that never
        // returns leaves the studio with its previous listing rather than a
        // frozen frame.
        if std::thread::Builder::new()
            .name("studio-devices".into())
            .spawn(move || {
                let _ = sender.send(DeviceInventory::probe());
            })
            .is_ok()
        {
            self.pending = Some(receiver);
        }
    }

    /// Refresh on the cadence the interface is asking for and collect any
    /// finished probe. Returns true when the listing changed.
    pub fn poll(&mut self, now: Instant, watching: bool) -> bool {
        if self.pinned {
            return false;
        }
        let interval = if watching {
            ACTIVE_REFRESH_INTERVAL
        } else {
            IDLE_REFRESH_INTERVAL
        };
        let due = self
            .last_requested
            .is_none_or(|last| now.saturating_duration_since(last) >= interval);
        if due {
            self.request(now);
        }
        let Some(receiver) = self.pending.as_ref() else {
            return false;
        };
        match receiver.try_recv() {
            Ok(inventory) => {
                self.inventory = inventory;
                self.pending = None;
                self.scanned = true;
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.pending = None;
                false
            }
        }
    }

    /// Ask for a fresh listing now, regardless of the cadence.
    pub fn refresh_now(&mut self, now: Instant) {
        self.last_requested = None;
        self.request(now);
    }

    /// A listing without a machine behind it, so a test can choose a device
    /// that is not plugged into the machine running the test.
    #[cfg(test)]
    pub(super) fn install_for_tests(&mut self, inventory: DeviceInventory) {
        self.inventory = inventory;
        self.scanned = true;
    }
}

/// Which port list the panel is showing, and where the cursor is in it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DevicePanel {
    pub kind: DeviceKind,
    pub selected: usize,
    /// The column the picker was opened from, when a chip opened it, so
    /// the popover appears next to that chip. `None` is the keyboard's,
    /// which has no place on screen to be near.
    pub anchor_x: Option<u16>,
    /// The first entry the list draws, kept from key to key so walking the
    /// middle of a long list leaves it still; see [`super::scroll`].
    pub first: usize,
    /// The selection was last put there by a click: the list keeps no
    /// margin until a key moves it, so the clicked row stays put.
    pub hold_scroll: bool,
    /// On the MIDI tab, which of the two boxes Space toggles. Kept as the
    /// selection moves, so ticking every destination down a column is a run
    /// of Down and Space rather than a hunt.
    pub midi_column: MidiDirection,
}

impl Default for DevicePanel {
    fn default() -> Self {
        Self {
            kind: DeviceKind::AudioOutput,
            selected: 0,
            anchor_x: None,
            first: 0,
            hold_scroll: false,
            midi_column: MidiDirection::Out,
        }
    }
}

impl DevicePanel {
    pub fn next_tab(&mut self) {
        let index = DeviceKind::ALL
            .iter()
            .position(|kind| *kind == self.kind)
            .unwrap_or(0);
        self.kind = DeviceKind::ALL[(index + 1) % DeviceKind::ALL.len()];
        self.selected = 0;
        self.first = 0;
    }

    pub fn previous_tab(&mut self) {
        let index = DeviceKind::ALL
            .iter()
            .position(|kind| *kind == self.kind)
            .unwrap_or(0);
        self.kind = DeviceKind::ALL[(index + DeviceKind::ALL.len() - 1) % DeviceKind::ALL.len()];
        self.selected = 0;
        self.first = 0;
    }

    pub fn move_selection(&mut self, delta: isize, len: usize) {
        if len == 0 {
            self.selected = 0;
            return;
        }
        let current = self.selected.min(len - 1) as isize;
        self.selected = (current + delta).clamp(0, len as isize - 1) as usize;
        self.hold_scroll = false;
    }

    /// Keep where the list is scrolled to, for the next key to move from,
    /// in the frame the panel is drawn in.
    pub fn settle_scroll(&mut self, available: Rect, entries: usize) {
        if let Some(geometry) = DevicePanelView::geometry(available, *self, entries.max(1)) {
            self.first = geometry.first;
        }
    }
}

/// Where the panel is drawn and which row each device occupies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PanelGeometry {
    pub area: Rect,
    pub tabs: Rect,
    pub list: Rect,
    /// The row naming the MIDI tab's two box columns; zero-height elsewhere.
    pub header: Rect,
    /// Index of the first listed entry, once the list is scrolled.
    pub first: usize,
}

impl PanelGeometry {
    /// Entry index under a pointer, if it is over the list.
    pub fn entry_at(self, x: u16, y: u16) -> Option<usize> {
        (x >= self.list.x && x < self.list.right() && y >= self.list.y && y < self.list.bottom())
            .then(|| self.first + usize::from(y - self.list.y))
    }

    /// The MIDI box under a pointer: which row, and which way.
    ///
    /// A click lands on the box and one column either side of it, because a
    /// hit target exactly three cells wide is one a trackpad misses half the
    /// time.
    pub fn midi_box_at(self, x: u16, y: u16) -> Option<(usize, MidiDirection)> {
        let index = self.entry_at(x, y)?;
        MidiDirection::BOTH.into_iter().find_map(|direction| {
            let start = midi_box_x(self.list, direction);
            (x + 1 >= start && x <= start + MIDI_BOX_WIDTH).then_some((index, direction))
        })
    }

    /// Tab under a pointer, if it is over the tab row.
    pub fn tab_at(self, x: u16, y: u16) -> Option<DeviceKind> {
        if y != self.tabs.y || x < self.tabs.x {
            return None;
        }
        let mut cursor = self.tabs.x;
        for kind in DeviceKind::ALL {
            let width = tab_width(kind);
            if x >= cursor && x < cursor + width {
                return Some(kind);
            }
            cursor += width;
        }
        None
    }
}

/// The width of a box: `[x]`.
const MIDI_BOX_WIDTH: u16 = 3;
/// The space between the two boxes.
const MIDI_BOX_GAP: u16 = 2;

/// Where a direction's box sits on a MIDI row: the right-hand columns, with
/// `out` last, so the boxes line up under their header however long a name.
fn midi_box_x(list: Rect, direction: MidiDirection) -> u16 {
    let out = list.right().saturating_sub(MIDI_BOX_WIDTH);
    match direction {
        MidiDirection::Out => out,
        MidiDirection::In => out.saturating_sub(MIDI_BOX_WIDTH + MIDI_BOX_GAP),
    }
}

/// Rows a tab reserves above its list: the MIDI tab names its two columns.
const fn header_rows(kind: DeviceKind) -> u16 {
    match kind {
        DeviceKind::Midi => 1,
        _ => 0,
    }
}

fn tab_width(kind: DeviceKind) -> u16 {
    UnicodeWidthStr::width(kind.label()) as u16 + 3
}

/// The floating device picker.
pub struct DevicePanelView<'a> {
    pub panel: DevicePanel,
    pub inventory: &'a DeviceInventory,
    pub theme: &'a Theme,
    /// Name of the output the engine is currently using.
    pub current_output: Option<&'a str>,
    /// Name of the input `s("in")` is playing, once one is open.
    pub current_input: Option<&'a str>,
    /// Whether an input is chosen at all, open yet or not. `none` is the
    /// active row exactly when this is false.
    pub input_chosen: bool,
    pub scanning: bool,
    /// What the open stream costs, end to end. Built by the app, because
    /// only it knows the limiter's character and the input lag being paid;
    /// the panel only has to draw it.
    pub latency: Option<LatencyReport>,
    /// The MIDI tab's rows, merged, with their boxes already resolved.
    pub midi_rows: &'a [MidiRow],
    /// The port MIDI clock is followed from, or `None` for the studio's own.
    pub clock_in: Option<&'a str>,
    /// The port MIDI clock is sent to, or `None` to send none.
    pub clock_out: Option<&'a str>,
}

/// Rows a tab reserves for what the open stream costs. Only the audio
/// tabs have a latency to report; a MIDI port has no buffer of ours.
const fn latency_rows(kind: DeviceKind) -> u16 {
    match kind {
        DeviceKind::AudioOutput | DeviceKind::AudioInput => 2,
        _ => 0,
    }
}

/// The delay between an action and hearing it, in the terms it is made of.
///
/// It has three parts: the output buffer, the input lag, which sizes
/// itself to the driver, and the master limiter. The panel where an output
/// is chosen shows them together.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LatencyReport {
    /// Frames of output the stream holds ahead of the speaker
    /// ([`rustel_audio::AudioOutputFacts::latency_frames`]): the callback
    /// size the driver reports, or - on a host whose fixed period sits
    /// below the buffer asked for, WASAPI in shared mode - that buffer,
    /// which is what a listener actually waits behind.
    pub output_frames: u32,
    /// Whether the driver said what it settled on. When it will not,
    /// `output_frames` is what was asked for - a guess, and the row says so.
    pub output_frames_reported: bool,
    pub sample_rate: u32,
    /// The master limiter's runway, in milliseconds; zero when it is off.
    /// What it actually takes at this rate, which above about 128 kHz is
    /// less than the character's nominal figure - the ring runs out first.
    pub limiter_millis: f32,
    /// The input lag really being paid, in frames, when an input is open:
    /// where the reader sits behind the writer, floored by the block it is
    /// filling. There is no setting to report instead: the lag is always
    /// automatic, sized from the driver's largest delivery.
    pub input_frames: Option<u64>,
}

impl LatencyReport {
    /// The report for an open output, the limiter's runway at its rate, and
    /// the input lag being paid when an input is open.
    ///
    /// The input reader never sits nearer its writer than the block being
    /// filled from it: the callback, which on WASAPI is the device period
    /// and not the larger buffer queued ahead of it.
    pub fn for_output(
        output: &rustel_audio::AudioOutputFacts,
        limiter_millis: f32,
        input_lag_frames: Option<u64>,
    ) -> Self {
        let callback = u64::from(
            output
                .reported_buffer_frames()
                .unwrap_or_else(|| output.requested_buffer_frames()),
        );
        Self {
            output_frames: output.latency_frames(),
            output_frames_reported: output.reported_buffer_frames().is_some(),
            sample_rate: output.sample_rate_hz(),
            limiter_millis,
            input_frames: input_lag_frames.map(|lag| lag.max(callback)),
        }
    }

    fn millis(&self, frames: f64) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            frames * 1000.0 / f64::from(self.sample_rate)
        }
    }

    /// What leaves the machine: the device's own buffer, plus whatever the
    /// limiter is holding back to look ahead with.
    pub fn output_millis(&self) -> f64 {
        self.millis(f64::from(self.output_frames)) + f64::from(self.limiter_millis)
    }

    /// Microphone to speaker, when an input is open.
    pub fn round_trip_millis(&self) -> Option<f64> {
        let input = self.input_frames?;
        Some(self.output_millis() + self.millis(input as f64))
    }

    /// The lines the panel draws. Two when an input is open: the round
    /// trip is a different question from the output's own delay, and a
    /// player chasing one is not chasing the other.
    pub fn lines(&self) -> (String, Option<String>) {
        let frames = self.output_frames;
        // A driver that will not say what it settled on gets a tilde: the
        // number is then what we asked for, not what we have.
        let sure = if self.output_frames_reported { "" } else { "~" };
        let limiter = if self.limiter_millis > 0.0 {
            format!(" + {:.1} limiter", self.limiter_millis)
        } else {
            String::new()
        };
        let out = format!(
            "out {sure}{:.1} ms \u{00b7} {frames} frames{limiter}",
            self.output_millis()
        );
        let round = self.round_trip_millis().map(|millis| {
            format!(
                "in \u{2192} out {sure}{millis:.1} ms \u{00b7} + {} input frames",
                self.input_frames.unwrap_or(0)
            )
        });
        (out, round)
    }
}

impl DevicePanelView<'_> {
    /// Fit the panel into `available`: over whatever opened it when a chip
    /// did, and in the bottom-right corner when the keyboard did.
    pub fn geometry(available: Rect, panel: DevicePanel, entries: usize) -> Option<PanelGeometry> {
        let width = available.width.saturating_sub(4).clamp(34, 64);
        let rows = u16::try_from(entries).unwrap_or(u16::MAX).clamp(1, 12);
        // Border, tab row, list rows, hint row - and, on the audio tabs,
        // the two the latency takes. Reserved from the KIND rather than
        // from whether a report happens to exist, so the panel does not
        // change height the moment a device opens and the hit test never
        // disagrees with the drawing about where a row is.
        let header_height = header_rows(panel.kind);
        let height = rows + 5 + latency_rows(panel.kind) + header_height;
        if available.width < width + 2 || available.height < height {
            return None;
        }
        // The list's own column is `area.x + 2`, so the anchor comes back
        // two to the left and the names line up under the chip that named
        // them. Clamped into the frame at both ends.
        let right_edge = available.right().saturating_sub(width + 1);
        let x = match panel.anchor_x {
            Some(anchor) => anchor.saturating_sub(2).clamp(available.x, right_edge),
            None => right_edge,
        };
        let area = Rect::new(x, available.bottom().saturating_sub(height), width, height);
        let tabs = Rect::new(area.x + 2, area.y + 2, area.width.saturating_sub(4), 1);
        let header = Rect::new(
            area.x + 2,
            area.y + 3,
            area.width.saturating_sub(4),
            header_height,
        );
        let list = Rect::new(
            area.x + 2,
            area.y + 3 + header_height,
            area.width.saturating_sub(4),
            rows,
        );
        let shown = usize::from(rows);
        let margin = if panel.hold_scroll {
            0
        } else {
            super::scroll::margin(shown)
        };
        let first = super::scroll::follow(panel.first, panel.selected, shown, entries, margin);
        Some(PanelGeometry {
            area,
            tabs,
            list,
            header,
            first,
        })
    }
}

impl DevicePanelView<'_> {
    /// The MIDI tab: a row a device with a box each way, then the clock rows.
    ///
    /// A ticked box is drawn in the colour the other tabs use for the device
    /// that is playing, because it answers the same question: this one is
    /// live. A dash is a direction the device does not have, drawn muted so
    /// it never reads as a box somebody forgot to tick.
    fn render_midi(&self, buffer: &mut Buffer, geometry: PanelGeometry) {
        let muted = Style::default().fg(self.theme.muted);
        let ports = self.midi_rows.len();
        if geometry.header.height > 0 {
            if ports == 0 {
                // Nothing to tick, but the clock rows are still worth having
                // on screen, so the reason goes where the column names would.
                let message = self
                    .inventory
                    .problem
                    .as_deref()
                    .unwrap_or("no MIDI ports found");
                buffer.set_stringn(
                    geometry.header.x,
                    geometry.header.y,
                    message,
                    usize::from(geometry.header.width),
                    muted.add_modifier(Modifier::ITALIC),
                );
            } else {
                for direction in MidiDirection::BOTH {
                    let label = direction.label();
                    let width = UnicodeWidthStr::width(label) as u16;
                    let x = midi_box_x(geometry.list, direction)
                        + MIDI_BOX_WIDTH.saturating_sub(width) / 2;
                    buffer.set_stringn(x, geometry.header.y, label, usize::from(width), muted);
                }
            }
        }
        // Names stop short of the boxes rather than running under them.
        let name_width = midi_box_x(geometry.list, MidiDirection::In)
            .saturating_sub(geometry.list.x)
            .saturating_sub(MIDI_BOX_GAP);
        for row in 0..geometry.list.height {
            let index = geometry.first + usize::from(row);
            let Some(kind) = MidiPanelRow::at(index, ports) else {
                break;
            };
            let y = geometry.list.y + row;
            let selected = index == self.panel.selected;
            if selected {
                buffer.set_style(
                    Rect::new(geometry.list.x, y, geometry.list.width, 1),
                    Style::default().bg(self.theme.selection),
                );
            }
            let text = if selected {
                self.theme.selection_text
            } else {
                self.theme.foreground
            };
            match kind {
                MidiPanelRow::Port(port) => {
                    let Some(device) = self.midi_rows.get(port) else {
                        break;
                    };
                    buffer.set_stringn(
                        geometry.list.x,
                        y,
                        format!("  {}", device.name),
                        usize::from(name_width),
                        Style::default().fg(text),
                    );
                    for direction in MidiDirection::BOTH {
                        let colour = if !device.has(direction) {
                            self.theme.muted
                        } else if device.enabled(direction) {
                            self.theme.ok
                        } else {
                            text
                        };
                        let mut style = Style::default().fg(colour);
                        // The box Space would toggle, on the row it would
                        // toggle: without it a player ticking down a column
                        // cannot see which of the two they are about to flip.
                        if selected && direction == self.panel.midi_column && device.has(direction)
                        {
                            style = style.add_modifier(Modifier::REVERSED);
                        }
                        buffer.set_stringn(
                            midi_box_x(geometry.list, direction),
                            y,
                            device.box_for(direction),
                            usize::from(MIDI_BOX_WIDTH),
                            style,
                        );
                    }
                }
                MidiPanelRow::ClockIn | MidiPanelRow::ClockOut => {
                    let (label, port) = if kind == MidiPanelRow::ClockIn {
                        ("clock in ", self.clock_in)
                    } else {
                        ("clock out", self.clock_out)
                    };
                    let colour = if port.is_some() { self.theme.ok } else { text };
                    buffer.set_stringn(
                        geometry.list.x,
                        y,
                        format!("  {label}  < {} >", port.unwrap_or("none")),
                        usize::from(geometry.list.width),
                        Style::default().fg(colour),
                    );
                }
            }
        }
    }
}

impl Widget for DevicePanelView<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let entries = self.inventory.entries(self.panel.kind);
        // The MIDI tab is sized from the rows it was handed rather than from
        // the inventory, because those rows are the ONE answer to "which
        // ports are there": the app decides it once, pinning included, and
        // the drawing, the keys and the clicks all count the same list.
        let rows = if self.panel.kind == DeviceKind::Midi {
            self.midi_rows.len() + MIDI_CLOCK_ROWS
        } else {
            self.inventory.row_count(self.panel.kind)
        };
        let Some(geometry) = DevicePanelView::geometry(area, self.panel, rows.max(1)) else {
            return;
        };
        let panel = geometry.area;
        super::view::clear_overlay(
            buffer,
            panel,
            Style::default()
                .bg(self.theme.overlay)
                .fg(self.theme.foreground),
        );
        draw_border(buffer, panel, self.theme);

        let title = if self.scanning {
            " devices - scanning… "
        } else {
            " devices "
        };
        buffer.set_stringn(
            panel.x + 2,
            panel.y,
            title,
            usize::from(panel.width.saturating_sub(4)),
            Style::default()
                .fg(self.theme.accent)
                .add_modifier(Modifier::BOLD),
        );

        let mut cursor = geometry.tabs.x;
        for kind in DeviceKind::ALL {
            let selected = kind == self.panel.kind;
            // Accent and bold are the whole marking, the same one the theme
            // editor's tabs use. No underline, so this tab bar matches the
            // others.
            let style = if selected {
                Style::default()
                    .fg(self.theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(self.theme.muted)
            };
            let label = format!(" {} ", kind.label());
            buffer.set_stringn(
                cursor,
                geometry.tabs.y,
                &label,
                usize::from(geometry.tabs.right().saturating_sub(cursor)),
                style,
            );
            cursor += tab_width(kind);
        }

        if self.panel.kind == DeviceKind::Midi {
            self.render_midi(buffer, geometry);
        } else {
            if entries.is_empty() {
                let message = match self.panel.kind {
                    DeviceKind::Gamepad => rustel_core::gamepad::problem()
                        .unwrap_or("no pad plugged in - they are watched from launch"),
                    _ => self
                        .inventory
                        .problem
                        .as_deref()
                        .unwrap_or("no ports found"),
                };
                buffer.set_stringn(
                    geometry.list.x,
                    geometry.list.y,
                    message,
                    usize::from(geometry.list.width),
                    Style::default()
                        .fg(self.theme.muted)
                        .add_modifier(Modifier::ITALIC),
                );
            }

            for row in 0..geometry.list.height {
                let Some(entry) = entries.get(geometry.first + usize::from(row)) else {
                    break;
                };
                let y = geometry.list.y + row;
                let selected = geometry.first + usize::from(row) == self.panel.selected;
                let active = match self.panel.kind {
                    DeviceKind::AudioOutput => entry.is_named(self.current_output),
                    // Nothing chosen is what `none` means, and a choice that
                    // has not opened yet - an interface still waking, or gone
                    // from the machine - is still a choice: the mark stays off
                    // `none` until the input is really let go.
                    DeviceKind::AudioInput if entry.name == NO_INPUT_NAME => !self.input_chosen,
                    DeviceKind::AudioInput => entry.is_named(self.current_input),
                    // The MIDI tabs copy a port name for the score to use rather
                    // than moving playback onto it, so nothing there is active;
                    // a pad is named by its number, so nothing is chosen there.
                    DeviceKind::Midi | DeviceKind::Gamepad => false,
                };
                if selected {
                    buffer.set_style(
                        Rect::new(geometry.list.x, y, geometry.list.width, 1),
                        Style::default().bg(self.theme.selection),
                    );
                }
                let marker = if active {
                    super::terminal::symbol("▸")
                } else if entry.is_default {
                    "·"
                } else {
                    " "
                };
                let name_style = Style::default().fg(if active {
                    self.theme.ok
                } else if selected {
                    self.theme.selection_text
                } else {
                    self.theme.foreground
                });
                // The detail keeps its own right-aligned column; the name is
                // ellipsized two cells before it.
                let detail_width = UnicodeWidthStr::width(entry.detail.as_str());
                let list_width = usize::from(geometry.list.width);
                let show_detail = !entry.detail.is_empty() && detail_width + 4 < list_width;
                let name_width = if show_detail {
                    list_width - detail_width - 2
                } else {
                    list_width
                };
                let name =
                    super::view::ellipsize(&format!("{marker} {}", entry.name), name_width as u16);
                buffer.set_stringn(geometry.list.x, y, name, name_width, name_style);
                if show_detail {
                    buffer.set_stringn(
                        geometry.list.right() - detail_width as u16,
                        y,
                        &entry.detail,
                        detail_width,
                        Style::default().fg(self.theme.muted),
                    );
                }
            }
        }

        // What the open stream costs, under the list and above the hint:
        // it belongs to the stream rather than to any one row, and it is
        // the question this panel is opened to answer.
        let reserved = latency_rows(self.panel.kind);
        if let Some(latency) = self.latency.filter(|_| reserved > 0) {
            let (out, round) = latency.lines();
            let width = usize::from(panel.width.saturating_sub(4));
            let style = Style::default().fg(self.theme.muted);
            // In the rows the geometry already set aside, just above the
            // hint: the list keeps every row it was given.
            let top = panel.bottom().saturating_sub(2 + reserved);
            for (offset, line) in [Some(out), round].into_iter().flatten().enumerate() {
                buffer.set_stringn(panel.x + 2, top + offset as u16, line, width, style);
            }
        }

        let hint = match self.panel.kind {
            DeviceKind::AudioOutput => "Enter plays through it · Tab switches · Esc closes",
            DeviceKind::AudioInput => "Enter makes it s(\"in\") · none turns it off · Esc closes",
            DeviceKind::Gamepad => {
                "click copies gamepad(n) and stays · Enter copies and closes · Esc closes"
            }
            DeviceKind::Midi => "Space ticks · ←/→ column or clock · Enter copies · Tab next",
        };
        buffer.set_stringn(
            panel.x + 2,
            panel.bottom().saturating_sub(2),
            hint,
            usize::from(panel.width.saturating_sub(4)),
            Style::default().fg(self.theme.muted),
        );
    }
}

pub(super) fn draw_border(buffer: &mut Buffer, area: Rect, theme: &Theme) {
    if area.width < 2 || area.height < 2 {
        return;
    }
    // A panel sits over whatever was drawn there; every cell of it is
    // blanked first, or the stage shows through around its text.
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = buffer.cell_mut((x, y)) {
                cell.set_symbol(" ")
                    .set_style(Style::default().bg(theme.overlay).fg(theme.foreground));
            }
        }
    }
    let style = Style::default().fg(theme.rule).bg(theme.overlay);
    let right = area.right() - 1;
    let bottom = area.bottom() - 1;
    for x in area.x..area.right() {
        if let Some(cell) = buffer.cell_mut((x, area.y)) {
            cell.set_symbol("─").set_style(style);
        }
        if let Some(cell) = buffer.cell_mut((x, bottom)) {
            cell.set_symbol("─").set_style(style);
        }
    }
    for y in area.y..area.bottom() {
        if let Some(cell) = buffer.cell_mut((area.x, y)) {
            cell.set_symbol("│").set_style(style);
        }
        if let Some(cell) = buffer.cell_mut((right, y)) {
            cell.set_symbol("│").set_style(style);
        }
    }
    for (x, y, symbol) in [
        (area.x, area.y, "╭"),
        (right, area.y, "╮"),
        (area.x, bottom, "╰"),
        (right, bottom, "╯"),
    ] {
        if let Some(cell) = buffer.cell_mut((x, y)) {
            cell.set_symbol(symbol).set_style(style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pads' rows come from the poller's state, not a probe: one a
    /// pad, named the way a score names it, saying whether it is moving.
    /// The row the engine is playing wears the mark, whatever the
    /// backend calls the device.
    ///
    /// The list shows the name a musician chose - `Bose QC45` - and the
    /// engine reports the handle it opened - `coreaudio:00-00-5E-...:output`.
    /// Compared as strings those never match, so the mark never moved off
    /// whatever had it: changing device left it on the old row.
    #[test]
    fn the_mark_follows_the_device_the_engine_opened_by_its_handle() {
        let entry = |name: &str, id: &str| DeviceEntry {
            name: name.to_owned(),
            id: id.to_owned(),
            detail: String::new(),
            is_default: false,
        };
        let bose = entry("Bose QC45", "coreaudio:00-00-5E-00-53-01:output");
        assert!(
            bose.is_named(Some("coreaudio:00-00-5E-00-53-01:output")),
            "the handle the engine reports"
        );
        assert!(
            bose.is_named(Some("Bose QC45")),
            "and the name it was chosen by"
        );
        assert!(!bose.is_named(Some("coreaudio:BuiltInSpeakerDevice")));
        assert!(!bose.is_named(None), "nothing open, nothing marked");

        // A row with no device behind it - `silent`, `none`, a MIDI port -
        // carries no handle, and must not match every empty report.
        let silent = entry("silent", "");
        assert!(silent.is_named(Some("silent")));
        assert!(!silent.is_named(Some("")), "an empty handle is not a match");
    }

    #[test]
    fn the_gamepads_tab_lists_the_pads_the_poller_sees() {
        let pad = rustel_core::gamepad::pad(3).expect("pad 3");
        rustel_core::gamepad::set_name(3, Some("Test Pad".to_owned()));
        pad.set_connected(true);
        let mut inventory = DeviceInventory::default();
        inventory.refresh_gamepads();
        let row = inventory
            .entries(DeviceKind::Gamepad)
            .iter()
            .find(|entry| entry.name == "gamepad(3)")
            .expect("the pad has a row");
        assert!(row.detail.starts_with("Test Pad · "), "{}", row.detail);
        assert!(!row.is_default, "only gamepad(0) is the default");
        assert_eq!(DeviceKind::Gamepad.snippet(&row.name), "gamepad(3)");
        pad.set_button(0, 1.0);
        inventory.refresh_gamepads();
        let row = inventory
            .entries(DeviceKind::Gamepad)
            .iter()
            .find(|entry| entry.name == "gamepad(3)")
            .expect("still there");
        assert!(row.detail.ends_with("● moving"), "{}", row.detail);
        pad.set_connected(false);
        inventory.refresh_gamepads();
        assert!(
            !inventory
                .entries(DeviceKind::Gamepad)
                .iter()
                .any(|entry| entry.name == "gamepad(3)"),
            "gone with the pad"
        );
        rustel_core::gamepad::set_name(3, None);
        assert_eq!(DeviceKind::ALL.last(), Some(&DeviceKind::Gamepad));
        assert_eq!(DeviceKind::Gamepad.label(), "gamepads");
    }

    fn inventory() -> DeviceInventory {
        DeviceInventory {
            audio_inputs: Vec::new(),
            audio_outputs: vec![
                DeviceEntry {
                    name: "MacBook Pro Speakers".into(),
                    id: String::new(),
                    detail: "48000 Hz · 2ch".into(),
                    is_default: true,
                },
                DeviceEntry {
                    name: "Scarlett 2i2".into(),
                    id: String::new(),
                    detail: "44100 Hz · 2ch".into(),
                    is_default: false,
                },
            ],
            midi_outputs: vec![DeviceEntry {
                name: "IAC Driver Bus 1".into(),
                id: String::new(),
                detail: String::new(),
                is_default: false,
            }],
            midi_inputs: Vec::new(),
            gamepads: Vec::new(),
            problem: None,
        }
    }

    #[test]
    fn a_long_device_name_does_not_collide_with_its_sample_rate() {
        let mut inventory = inventory();
        inventory.audio_outputs[0].name = "Headphones (G522 LIGHTSPEED - Wireless Mode)".into();
        let area = Rect::new(0, 0, 90, 30);
        let panel = DevicePanel::default();
        let mut buffer = Buffer::empty(area);
        DevicePanelView {
            midi_rows: &[],
            clock_in: None,
            clock_out: None,
            latency: None,
            panel,
            inventory: &inventory,
            theme: &Theme::built_in_default(),
            current_output: None,
            current_input: None,
            input_chosen: false,
            scanning: false,
        }
        .render(area, &mut buffer);
        let list = DevicePanelView::geometry(area, panel, 2).unwrap().list;
        let row = |offset| {
            (list.x..list.right())
                .map(|x| buffer[(x, list.y + offset)].symbol().to_owned())
                .collect::<String>()
        };
        let long = row(0);
        assert!(long.contains("Headphones (G522 LIGHTSPEED"), "{long}");
        assert!(long.contains("…  48000 Hz · 2ch"), "{long}");
        let short = row(1);
        assert!(short.contains("Scarlett 2i2"), "{short}");
        assert!(short.contains("44100 Hz · 2ch"), "{short}");
        assert!(!short.contains('…'), "{short}");
    }

    /// Every probe leads the inputs with the row that is not an input, so
    /// letting one go is a row to press rather than a keystroke to know -
    /// and so an interface unplugged since it was chosen still has a way
    /// back to silence, however many interfaces are plugged in.
    #[test]
    fn the_inputs_are_led_by_the_row_that_turns_them_off() {
        let mic = rustel_audio::AudioDeviceInfo {
            id: "hw:1,0".into(),
            name: "USB mic".into(),
            is_default: true,
            sample_rate: Some(48_000),
            channels: Some(1),
        };
        let inventory =
            DeviceInventory::from_host(Ok((Vec::new(), vec![mic])), (Vec::new(), Vec::new(), None));
        assert_eq!(inventory.entries(DeviceKind::AudioInput)[1].name, "USB mic");
        let first = inventory
            .entries(DeviceKind::AudioInput)
            .first()
            .expect("the none row is always there");
        assert_eq!(first.name, NO_INPUT_NAME);
        assert!(!first.is_default, "it is nobody's default");
        assert!(
            !inventory
                .entries(DeviceKind::AudioOutput)
                .iter()
                .any(|entry| entry.name == NO_INPUT_NAME),
            "the outputs have `silent` instead, and only that"
        );
        assert_eq!(
            inventory
                .entries(DeviceKind::AudioInput)
                .iter()
                .filter(|entry| entry.name == NO_INPUT_NAME)
                .count(),
            1,
            "one row, however many times a machine is probed"
        );
    }

    /// The mark follows the choice, not the open device: `none` wears it
    /// only while nothing is chosen, so an input still waking up does not
    /// leave two rows claiming to be the one.
    #[test]
    fn the_none_row_is_marked_while_no_input_is_chosen() {
        let inventory = DeviceInventory {
            audio_inputs: vec![
                DeviceEntry {
                    name: NO_INPUT_NAME.into(),
                    id: String::new(),
                    detail: "no input · s(\"in\") is silence".into(),
                    is_default: false,
                },
                DeviceEntry {
                    name: "Scarlett 2i2".into(),
                    id: String::new(),
                    detail: "44100 Hz · 2ch".into(),
                    is_default: true,
                },
            ],
            ..inventory()
        };
        let area = Rect::new(0, 0, 100, 30);
        let panel = DevicePanel {
            kind: DeviceKind::AudioInput,
            ..DevicePanel::default()
        };
        let marked = |current_input, input_chosen| {
            let mut buffer = Buffer::empty(area);
            DevicePanelView {
                midi_rows: &[],
                clock_in: None,
                clock_out: None,
                latency: None,
                panel,
                inventory: &inventory,
                theme: &Theme::built_in_default(),
                current_output: None,
                current_input,
                input_chosen,
                scanning: false,
            }
            .render(area, &mut buffer);
            let geometry = DevicePanelView::geometry(area, panel, 2).expect("it fits");
            (0..2)
                .filter(|row| {
                    buffer[(geometry.list.x, geometry.list.y + *row as u16)].symbol() == "▸"
                })
                .collect::<Vec<usize>>()
        };
        assert_eq!(marked(None, false), vec![0], "nothing chosen: `none` is it");
        assert_eq!(
            marked(None, true),
            Vec::<usize>::new(),
            "chosen but not open yet: neither row claims it"
        );
        assert_eq!(
            marked(Some("Scarlett 2i2"), true),
            vec![1],
            "open: the interface wears the mark"
        );
    }

    /// Conhost draws `▸` as tofu, so the active row's mark falls back to
    /// its plain stand-in instead - never the fancy glyph, which is what
    /// a terminal without the capability cannot show.
    #[test]
    fn the_active_row_mark_falls_back_without_the_capability() {
        let _forced = super::super::terminal::ForceSymbolsForTest::set(false);
        let inventory = DeviceInventory {
            audio_inputs: vec![DeviceEntry {
                name: "Scarlett 2i2".into(),
                id: String::new(),
                detail: "44100 Hz \u{b7} 2ch".into(),
                is_default: true,
            }],
            ..inventory()
        };
        let area = Rect::new(0, 0, 100, 30);
        let panel = DevicePanel {
            kind: DeviceKind::AudioInput,
            ..DevicePanel::default()
        };
        let mut buffer = Buffer::empty(area);
        DevicePanelView {
            midi_rows: &[],
            clock_in: None,
            clock_out: None,
            latency: None,
            panel,
            inventory: &inventory,
            theme: &Theme::built_in_default(),
            current_output: None,
            current_input: Some("Scarlett 2i2"),
            input_chosen: true,
            scanning: false,
        }
        .render(area, &mut buffer);
        let geometry = DevicePanelView::geometry(area, panel, 1).expect("it fits");
        assert_eq!(
            buffer[(geometry.list.x, geometry.list.y)].symbol(),
            ">",
            "the stand-in draws instead of the tofu conhost would show"
        );
    }

    #[test]
    fn midi_rows_copy_the_bare_port_name() {
        assert_eq!(
            DeviceKind::Midi.snippet("IAC Driver Bus 1"),
            "IAC Driver Bus 1"
        );
        assert_eq!(DeviceKind::Midi.snippet("LPK25"), "LPK25");
        // An audio output is chosen here, not named in the score.
        assert_eq!(
            DeviceKind::AudioOutput.snippet("Scarlett 2i2"),
            "Scarlett 2i2"
        );
    }

    #[test]
    fn tabs_cycle_in_both_directions_and_reset_the_cursor() {
        let mut panel = DevicePanel {
            selected: 3,
            anchor_x: None,
            ..DevicePanel::default()
        };
        panel.next_tab();
        assert_eq!(panel.kind, DeviceKind::AudioInput);
        assert_eq!(panel.selected, 0);
        panel.next_tab();
        assert_eq!(panel.kind, DeviceKind::Midi, "one MIDI tab, both ways");
        panel.next_tab();
        assert_eq!(panel.kind, DeviceKind::Gamepad, "the pads are the last tab");
        panel.next_tab();
        assert_eq!(panel.kind, DeviceKind::AudioOutput);
        panel.previous_tab();
        assert_eq!(panel.kind, DeviceKind::Gamepad);
        panel.previous_tab();
        assert_eq!(panel.kind, DeviceKind::Midi);
    }

    #[test]
    fn the_cursor_stays_inside_the_list() {
        let mut panel = DevicePanel::default();
        panel.move_selection(5, 2);
        assert_eq!(panel.selected, 1);
        panel.move_selection(-5, 2);
        assert_eq!(panel.selected, 0);
        panel.move_selection(1, 0);
        assert_eq!(panel.selected, 0);
    }

    #[test]
    fn pointer_positions_resolve_to_rows_and_tabs() {
        let available = Rect::new(0, 0, 100, 30);
        let panel = DevicePanel::default();
        let geometry = DevicePanelView::geometry(available, panel, 2).expect("panel fits");
        assert_eq!(geometry.entry_at(geometry.list.x, geometry.list.y), Some(0));
        assert_eq!(
            geometry.entry_at(geometry.list.x, geometry.list.y + 1),
            Some(1)
        );
        assert_eq!(geometry.entry_at(geometry.list.x, geometry.area.y), None);
        assert_eq!(
            geometry.tab_at(geometry.tabs.x, geometry.tabs.y),
            Some(DeviceKind::AudioOutput)
        );
        assert_eq!(
            geometry.tab_at(
                geometry.tabs.x + tab_width(DeviceKind::AudioOutput),
                geometry.tabs.y
            ),
            Some(DeviceKind::AudioInput)
        );
    }

    #[test]
    fn a_terminal_with_no_room_gets_no_panel() {
        assert!(
            DevicePanelView::geometry(Rect::new(0, 0, 30, 30), DevicePanel::default(), 2).is_none()
        );
        assert!(
            DevicePanelView::geometry(Rect::new(0, 0, 100, 5), DevicePanel::default(), 2).is_none()
        );
    }

    #[test]
    fn a_long_selection_scrolls_the_visible_window() {
        let available = Rect::new(0, 0, 100, 30);
        let mut panel = DevicePanel {
            selected: 20,
            ..DevicePanel::default()
        };
        let geometry = DevicePanelView::geometry(available, panel, 24).expect("panel fits");
        assert_eq!(geometry.list.height, 12);
        // Two entries stay in sight below the selection.
        assert_eq!(geometry.first, 11);
        assert_eq!(
            geometry.entry_at(geometry.list.x, geometry.list.bottom() - 3),
            Some(20)
        );
        // A click holds the list: no margin, only the selection kept inside.
        panel.hold_scroll = true;
        let held = DevicePanelView::geometry(available, panel, 24).expect("panel fits");
        assert_eq!(held.first, 9);
        // Walking by keys from the top keeps the list still until the margin.
        let mut panel = DevicePanel::default();
        for _ in 0..9 {
            panel.move_selection(1, 24);
            panel.settle_scroll(available, 24);
        }
        assert_eq!((panel.selected, panel.first), (9, 0));
        panel.move_selection(1, 24);
        panel.settle_scroll(available, 24);
        assert_eq!((panel.selected, panel.first), (10, 1));
    }

    #[test]
    fn the_panel_renders_the_active_output_and_every_family() {
        let inventory = inventory();
        assert_eq!(
            inventory.midi_port_counts(),
            MidiPortCounts {
                outputs: 1,
                inputs: 0
            }
        );
        let area = Rect::new(0, 0, 100, 30);
        let mut buffer = Buffer::empty(area);
        DevicePanelView {
            midi_rows: &[],
            clock_in: None,
            clock_out: None,
            latency: None,
            panel: DevicePanel::default(),
            inventory: &inventory,
            theme: &Theme::built_in_default(),
            current_output: Some("Scarlett 2i2"),
            current_input: None,
            input_chosen: false,
            scanning: false,
        }
        .render(area, &mut buffer);
        let text = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("Scarlett 2i2"), "{text}");
        assert!(text.contains("▸"), "the active output is marked");
        // One MIDI tab, not one each way.
        assert!(text.contains(" midi "), "{text}");
        assert!(
            !text.contains("midi out") && !text.contains("midi in "),
            "{text}"
        );
    }

    /// The MIDI tab draws one row a device with a box each way, a dash where
    /// a device has no port that way, the column names over the boxes, and
    /// the two clock rows under the ports.
    #[test]
    fn the_midi_tab_draws_a_box_each_way_and_the_clock_rows() {
        let entry = |name: &str| DeviceEntry {
            name: name.to_owned(),
            id: String::new(),
            detail: String::new(),
            is_default: false,
        };
        let inventory = DeviceInventory {
            midi_outputs: vec![entry("Maschine"), entry("Synth")],
            midi_inputs: vec![entry("Maschine")],
            ..DeviceInventory::default()
        };
        let mut enabled = MidiEnablement::from_prefs(Some(&[]), Some(&[]), &inventory);
        enabled.toggle(MidiDirection::Out, "Maschine");
        let rows = inventory.midi_rows(&enabled);
        let panel = DevicePanel {
            kind: DeviceKind::Midi,
            ..DevicePanel::default()
        };
        let area = Rect::new(0, 0, 100, 30);
        let mut buffer = Buffer::empty(area);
        DevicePanelView {
            midi_rows: &rows,
            clock_in: None,
            clock_out: Some("Maschine"),
            latency: None,
            panel,
            inventory: &inventory,
            theme: &Theme::built_in_default(),
            current_output: None,
            current_input: None,
            input_chosen: false,
            scanning: false,
        }
        .render(area, &mut buffer);
        let line = |needle: &str| -> String {
            (0..area.height)
                .map(|y| {
                    (0..area.width)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                })
                .find(|row| row.contains(needle))
                .unwrap_or_else(|| panic!("no line contains {needle:?}"))
        };

        let maschine = line("Maschine  ");
        assert!(
            maschine.contains("[ ]") && maschine.contains("[x]"),
            "{maschine}"
        );
        assert!(
            maschine.find("[ ]") < maschine.find("[x]"),
            "in comes before out: {maschine}"
        );
        let synth = line("Synth");
        assert!(
            synth.contains(" - "),
            "no port in: a dash, not a box: {synth}"
        );
        assert!(synth.contains("[ ]"), "{synth}");

        let header = line(" in ");
        assert!(header.contains("out"), "the columns are named: {header}");

        assert!(line("clock in").contains("< none >"));
        assert!(line("clock out").contains("< Maschine >"));
    }

    #[test]
    fn very_long_device_names_are_truncated_on_a_character_boundary() {
        let name = format!("{}é", "x".repeat(MAX_NAME_BYTES));
        let short = truncate(&name);
        assert!(short.len() <= MAX_NAME_BYTES + 3);
        assert!(short.ends_with('…'));
        assert!(std::str::from_utf8(short.as_bytes()).is_ok());
    }
}

#[cfg(test)]
mod latency_tests {
    use super::LatencyReport;

    /// The delay between doing something and hearing it, said in the terms
    /// it is made of.
    ///
    /// Three numbers lived in three places and appeared in none: the
    /// device's buffer, the master limiter's runway, and the input lag.
    /// Adding a limiter made the total worse without anything on screen
    /// changing, which is the state this exists to end.
    #[test]
    fn the_report_adds_up_and_says_what_it_is_unsure_of() {
        // 256 frames at 48 kHz is 5.33 ms; a transparent limiter adds 5.
        let report = LatencyReport {
            output_frames: 256,
            output_frames_reported: true,
            sample_rate: 48_000,
            limiter_millis: 5.0,
            input_frames: None,
        };
        assert!((report.output_millis() - 10.33).abs() < 0.01);
        assert_eq!(report.round_trip_millis(), None, "no input, no round trip");
        let (out, round) = report.lines();
        assert!(
            out.contains("10.3 ms") && out.contains("256 frames"),
            "{out}"
        );
        assert!(out.contains("5.0 limiter"), "{out}");
        assert!(round.is_none());

        // An input open adds its own lag, and the round trip is its own
        // line: chasing one is not chasing the other.
        let with_input = LatencyReport {
            input_frames: Some(1024),
            ..report
        };
        assert!((with_input.round_trip_millis().expect("open") - 31.66).abs() < 0.01);
        let (_, round) = with_input.lines();
        let round = round.expect("a round trip");
        assert!(
            round.contains("31.7 ms") && round.contains("1024"),
            "{round}"
        );

        // The limiter off costs nothing and is not mentioned.
        let off = LatencyReport {
            limiter_millis: 0.0,
            ..report
        };
        assert!((off.output_millis() - 5.33).abs() < 0.01);
        assert!(!off.lines().0.contains("limiter"), "{}", off.lines().0);

        // A driver that will not say what it settled on is marked: the
        // number is then what was asked for, not what is being had.
        let unsure = LatencyReport {
            output_frames_reported: false,
            ..report
        };
        assert!(unsure.lines().0.contains("~"), "{}", unsure.lines().0);
        assert!(
            !report.lines().0.contains('~'),
            "a known buffer is not a guess"
        );

        // A device that reports no rate divides by nothing rather than
        // panicking or claiming an infinity of milliseconds.
        let silent = LatencyReport {
            sample_rate: 0,
            ..report
        };
        assert_eq!(silent.output_millis(), 5.0, "only the limiter is left");
    }

    /// On WASAPI in shared mode a larger buffer asked for is queued ahead
    /// of the fixed device period. The report counts that buffer; the
    /// input lag still floors at the callback size.
    #[test]
    fn a_buffer_queued_ahead_of_a_fixed_period_is_the_latency_reported() {
        let output = rustel_audio::AudioOutputFacts::new(
            rustel_audio::AudioHost::cpal("WASAPI"),
            "Speakers",
            48_000,
            2,
            Some(rustel_audio::AudioSampleFormat::F32),
            2048,
            Some(480),
        )
        .with_device_period_frames(Some(480));
        let report = LatencyReport::for_output(&output, 0.0, Some(100));
        assert_eq!(report.output_frames, 2048);
        assert!(report.output_frames_reported);
        let (out, round) = report.lines();
        assert!(
            out.contains("42.7 ms") && out.contains("2048 frames"),
            "{out}"
        );
        assert_eq!(report.input_frames, Some(480), "floored at the callback");
        assert!(
            round
                .expect("an input is open")
                .contains("480 input frames")
        );

        // A host with no fixed period reports its callback, whatever was
        // asked; one that says nothing reads as the request, marked.
        let clamped = rustel_audio::AudioOutputFacts::new(
            rustel_audio::AudioHost::cpal("coreaudio"),
            "Built-in",
            48_000,
            2,
            Some(rustel_audio::AudioSampleFormat::F32),
            2048,
            Some(1024),
        );
        assert_eq!(
            LatencyReport::for_output(&clamped, 0.0, None).output_frames,
            1024
        );
        let silent = rustel_audio::AudioOutputFacts::new(
            rustel_audio::AudioHost::Silent,
            "silent",
            48_000,
            2,
            None,
            256,
            None,
        );
        let unsure = LatencyReport::for_output(&silent, 0.0, None);
        assert_eq!(unsure.output_frames, 256);
        assert!(unsure.lines().0.contains('~'), "{}", unsure.lines().0);
    }
}

#[cfg(test)]
mod latency_draw_tests {
    use super::*;
    use ratatui::layout::Rect;

    /// The panel draws the latency report, above its hint and below the
    /// list.
    #[test]
    fn the_devices_panel_shows_what_the_stream_costs() {
        let inventory = DeviceInventory {
            audio_outputs: vec![DeviceEntry {
                name: "BuiltInSpeakerDevice".to_owned(),
                id: String::new(),
                detail: "48000 Hz · 2ch".to_owned(),
                is_default: true,
            }],
            ..DeviceInventory::default()
        };
        let theme = Theme::built_in_default();
        let area = Rect::new(0, 0, 90, 30);
        let drawn = |latency: Option<LatencyReport>| {
            let mut buffer = Buffer::empty(area);
            DevicePanelView {
                midi_rows: &[],
                clock_in: None,
                clock_out: None,
                latency,
                panel: DevicePanel::default(),
                inventory: &inventory,
                theme: &theme,
                current_output: Some("BuiltInSpeakerDevice"),
                current_input: None,
                input_chosen: false,
                scanning: false,
            }
            .render(area, &mut buffer);
            (0..area.height)
                .map(|y| {
                    (area.x..area.right())
                        .map(|x| buffer.cell((x, y)).unwrap().symbol().to_owned())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        // Without one - a panel opened before the engine has a device -
        // nothing is claimed.
        let quiet = drawn(None);
        assert!(!quiet.contains(" ms"), "{quiet}");
        assert!(
            quiet.contains("BuiltInSpeakerDevice"),
            "the list still draws"
        );

        let text = drawn(Some(LatencyReport {
            output_frames: 256,
            output_frames_reported: true,
            sample_rate: 48_000,
            limiter_millis: 5.0,
            input_frames: Some(1024),
        }));
        assert!(text.contains("out 10.3 ms"), "{text}");
        assert!(text.contains("5.0 limiter"), "{text}");
        assert!(text.contains("31.7 ms"), "the round trip too: {text}");
        // The row it belongs to is still there, and so is the hint.
        assert!(text.contains("48000 Hz"), "{text}");
        assert!(text.contains("Enter plays through it"), "{text}");
    }
}

#[cfg(test)]
mod midi_enablement_tests {
    use super::*;

    fn entry(name: &str) -> DeviceEntry {
        DeviceEntry {
            name: name.to_owned(),
            id: String::new(),
            detail: String::new(),
            is_default: false,
        }
    }

    fn inventory(outputs: &[&str], inputs: &[&str]) -> DeviceInventory {
        DeviceInventory {
            midi_outputs: outputs.iter().copied().map(entry).collect(),
            midi_inputs: inputs.iter().copied().map(entry).collect(),
            ..DeviceInventory::default()
        }
    }

    /// A studio that predates the switch had no gate, so everything it can
    /// see was allowed. Reading its silence as "nothing allowed" would open
    /// the upgrade with every set's MIDI dead and a checkbox nobody knows to
    /// look for standing between the score and the port.
    #[test]
    fn a_studio_that_never_said_keeps_the_ports_it_already_had() {
        let present = inventory(&["loopMIDI", "Synth"], &["Keys"]);
        let enabled = MidiEnablement::from_prefs(None, None, &present);
        assert!(enabled.is_enabled(MidiDirection::Out, "loopMIDI"));
        assert!(enabled.is_enabled(MidiDirection::Out, "Synth"));
        assert!(enabled.is_enabled(MidiDirection::In, "Keys"));
        // Only what was there. Something plugged in afterwards is off until
        // it is ticked, which is the half of "off by default" worth having.
        assert!(!enabled.is_enabled(MidiDirection::Out, "Arrived Later"));
    }

    /// Turning everything off is a decision, and has to survive a restart.
    /// An empty list and no list at all cannot mean the same thing.
    #[test]
    fn everything_off_is_kept_apart_from_never_having_said() {
        let present = inventory(&["loopMIDI"], &["Keys"]);
        let silent = MidiEnablement::from_prefs(Some(&[]), Some(&[]), &present);
        assert!(!silent.is_enabled(MidiDirection::Out, "loopMIDI"));
        assert!(!silent.is_enabled(MidiDirection::In, "Keys"));
    }

    /// A device can be wanted one way and not the other: a controller you
    /// play FROM and never send TO.
    #[test]
    fn the_two_directions_are_switched_apart() {
        let present = inventory(&["Maschine"], &["Maschine"]);
        let mut enabled = MidiEnablement::from_prefs(Some(&[]), Some(&[]), &present);
        assert!(enabled.toggle(MidiDirection::In, "Maschine"));
        assert!(enabled.is_enabled(MidiDirection::In, "Maschine"));
        assert!(!enabled.is_enabled(MidiDirection::Out, "Maschine"));
        assert_eq!(enabled.kept(MidiDirection::In), vec!["Maschine".to_owned()]);
        assert!(enabled.kept(MidiDirection::Out).is_empty());
        // And off again.
        assert!(!enabled.toggle(MidiDirection::In, "Maschine"));
        assert!(!enabled.is_enabled(MidiDirection::In, "Maschine"));
    }

    /// A score names a selector, not a port. Gating it against exact names
    /// would refuse `.midi("Maschine")` outright, because no port is called
    /// just "Maschine" - the gate has to land where the opener lands.
    #[test]
    fn a_selector_is_judged_by_the_port_it_opens() {
        let present = inventory(
            &["Microsoft GS Wavetable Synth", "Maschine MK3 EXT MIDI"],
            &[],
        );
        let mut enabled = MidiEnablement::from_prefs(Some(&[]), Some(&[]), &present);
        enabled.toggle(MidiDirection::Out, "Maschine MK3 EXT MIDI");

        assert!(enabled.allows(MidiDirection::Out, "Maschine"), "one word");
        assert!(enabled.allows(MidiDirection::Out, "maschine"), "any case");
        assert!(enabled.allows(MidiDirection::Out, "1"), "by index");
        assert!(!enabled.allows(MidiDirection::Out, "Wavetable"), "off");
        assert!(!enabled.allows(MidiDirection::Out, "0"), "off, by index");
        assert_eq!(
            enabled.port_named(MidiDirection::Out, "wavetable"),
            Some("Microsoft GS Wavetable Synth"),
            "a refusal names the row to go and tick"
        );
        // A selector that matches nothing is not this gate's to refuse: the
        // opener's own "not found, available: ..." is the better answer.
        assert!(enabled.allows(MidiDirection::Out, "Nonexistent"));
        // An empty one matches nothing either, rather than landing on the
        // switched-off port 0 as it used to.
        assert!(enabled.allows(MidiDirection::Out, ""));
        assert_eq!(enabled.port_named(MidiDirection::Out, ""), None);
    }

    /// One device is one row, however many directions it reports, because
    /// it is one thing on the desk. A name with only one direction keeps its
    /// row and shows a dash where there is no port - nothing to tick, as
    /// opposed to something switched off.
    #[test]
    fn a_device_that_goes_both_ways_is_one_row_with_two_boxes() {
        let present = inventory(&["Maschine", "Synth"], &["Maschine", "Keys"]);
        let mut enabled = MidiEnablement::from_prefs(Some(&[]), Some(&[]), &present);
        enabled.toggle(MidiDirection::Out, "Maschine");

        let rows = present.midi_rows(&enabled);
        let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, ["Maschine", "Synth", "Keys"], "hosts order, merged");

        let maschine = &rows[0];
        assert!(maschine.has_in && maschine.has_out);
        assert_eq!(maschine.box_for(MidiDirection::Out), "[x]");
        assert_eq!(maschine.box_for(MidiDirection::In), "[ ]");

        let synth = &rows[1];
        assert!(!synth.has_in);
        assert_eq!(synth.box_for(MidiDirection::In), " - ", "no port to tick");
        assert_eq!(synth.box_for(MidiDirection::Out), "[ ]");

        let keys = &rows[2];
        assert!(keys.has_in && !keys.has_out);
        assert_eq!(keys.box_for(MidiDirection::Out), " - ");
    }
}
