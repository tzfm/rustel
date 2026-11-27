//! VST3 plugin host.
//!
//! A [`Host`] finds the plugin bundles in the VST3 folders. It loads a
//! plugin when a score or the user asks for the plugin, reads its
//! parameters, and builds the plugin as an orbit insert for the audio
//! engine.
//!
//! One thread, the plugin thread, makes every plugin call except the audio
//! processing. The audio processing runs in the audio callback or on a
//! plugin worker of the engine, one thread at a time. The audio engine owns
//! a running plugin and gives the plugin back to the plugin thread at the
//! end. On macOS the creation and the release of a plugin run on the
//! process's main thread instead, which the plugin thread waits for. See
//! [`install_main_service`] for the contract.
//!
//! ```text
//!   score:   .vst("name", { mix: 0.4 })
//!                  │ names
//!   Host ──────────┴─ numbers ──▶ note ──▶ audio callback ──▶ plugin
//!     └─ plugin thread: load, prepare, end
//!           └─ macOS: the main thread creates and releases the plugin
//! ```

#![forbid(unsafe_op_in_unsafe_fn)]
// The numbers of a VST3 enum are `u32` on Unix and `i32` on Windows, so a
// cast that does nothing on one system is necessary on the other.
#![allow(clippy::unnecessary_cast)]

mod cache;
mod com;
mod host;
mod instance;
mod main_thread;
mod module;
mod preset;
mod scan;
mod watch;
mod worker;

pub use host::{
    FoundPreset, Host, NotQueued, Plugin, PluginInfo, Prepared, RenderFaults, Resolved,
    ScanProgress, Status, WantedCopy, WorkerProgram,
};
pub use main_thread::{install_main_service, needs_the_main_thread, pump_main};
pub use scan::{default_folders, find_bundles};
pub use worker::serve;

/// A piece of work for the plugin thread.
pub(crate) type Job = Box<dyn FnOnce() + Send>;

/// One parameter of a plugin.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamInfo {
    /// The number the plugin gives the parameter.
    pub id: u32,
    /// The title the plugin shows.
    pub name: String,
    /// The name a score writes: the title in lower case with letters and
    /// digits only, or the number when the title is empty or not unique.
    pub key: String,
    /// The group the plugin puts the parameter in, for example
    /// "Osc A/Warp". Empty for a parameter at the top level.
    pub group: String,
    pub units: String,
    /// The value at the start, from 0 to 1.
    pub default: f64,
    /// 0 for a continuous parameter, or the number of steps.
    pub steps: u32,
    /// The value at the start as the plugin prints it.
    pub default_text: String,
    /// What the parameter is to a host, as the plugin marks it.
    pub role: ParamRole,
}

/// What a parameter is to a host, as the plugin marks it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ParamRole {
    /// A parameter of the sound.
    #[default]
    Plain,
    /// The dry/wet mix, as the plugin names it to a host.
    Mix,
    /// The plugin's own bypass.
    Bypass,
    /// The plugin's program change.
    Program,
}

impl ParamInfo {
    /// The start value as a normalized number, followed by the plugin's
    /// display text when different: `1 = 100 %`. Scores use the number.
    pub fn default_shown(&self) -> String {
        let number = format!("{:.3}", self.default);
        let number = number.trim_end_matches('0').trim_end_matches('.');
        let text = format!("{} {}", self.default_text, self.units);
        let text = text.trim();
        // A plugin that prints the number itself needs no second copy.
        let same = |shown: f64| (shown - self.default).abs() < 0.0005;
        if text.is_empty() || text.parse().is_ok_and(same) {
            number.to_owned()
        } else {
            format!("{number} = {text}")
        }
    }
}

/// VST3 controller number for MIDI channel aftertouch (`kAfterTouch`).
pub const MIDI_CC_AFTERTOUCH: u16 = 128;
/// VST3 controller number for MIDI pitch bend (`kPitchBend`).
pub const MIDI_CC_PITCHBEND: u16 = 129;

/// One MIDI controller mapping exposed by a plugin through `IMidiMapping`.
#[derive(Clone, Debug, PartialEq)]
pub struct MidiCcInfo {
    /// VST3 controller number: `0..=127` for MIDI CC, `128` for channel
    /// aftertouch, `129` for pitch bend.
    pub controller: u16,
    /// The VST3 parameter number mapped to this controller.
    pub param_id: u32,
    /// Canonical score key (`modwheel`, `aftertouch`, `pitchbend`, `cc74`, ...).
    pub key: String,
    /// Display label for completion and reference views.
    pub name: String,
    /// Default normalized value (`0.0..=1.0`; `0.5` for pitch bend when the
    /// controller reports no other finite value).
    pub default: f64,
}

/// Resolves a score key to a VST3 MIDI controller number (`0..=127` for MIDI
/// CC, `128` for aftertouch, `129` for pitch bend).
pub fn midi_controller_number(name: &str) -> Option<u16> {
    let key = canonical(name);
    match key.as_str() {
        "modwheel" | "mod" | "modulation" => Some(1),
        "breath" => Some(2),
        "foot" => Some(4),
        "portamentotime" => Some(5),
        "volume" => Some(7),
        "balance" => Some(8),
        "pan" => Some(10),
        "expression" | "expr" => Some(11),
        "sustain" | "hold" | "damper" => Some(64),
        "portamento" => Some(65),
        "sostenuto" => Some(66),
        "soft" => Some(67),
        "legato" => Some(68),
        "resonance" | "timbre" => Some(71),
        "releasetime" => Some(72),
        "attacktime" => Some(73),
        "brightness" => Some(74),
        "aftertouch" | "miditouch" | "pressure" | "channelpressure" => Some(MIDI_CC_AFTERTOUCH),
        "pitchbend" | "midibend" | "bend" => Some(MIDI_CC_PITCHBEND),
        other => other
            .strip_prefix("cc")
            .and_then(|digits| {
                (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| digits.parse::<u16>().ok())
                    .flatten()
            })
            .filter(|cc| *cc <= 127),
    }
}

/// Canonical score key for a VST3 MIDI controller number.
pub fn midi_controller_key(controller: u16) -> String {
    match controller {
        1 => "modwheel".to_owned(),
        2 => "breath".to_owned(),
        11 => "expression".to_owned(),
        64 => "sustain".to_owned(),
        74 => "brightness".to_owned(),
        MIDI_CC_AFTERTOUCH => "aftertouch".to_owned(),
        MIDI_CC_PITCHBEND => "pitchbend".to_owned(),
        cc => format!("cc{cc}"),
    }
}

/// Display name for a VST3 MIDI controller number.
pub fn midi_controller_name(controller: u16) -> String {
    match controller {
        1 => "Mod Wheel (CC 1)".to_owned(),
        2 => "Breath (CC 2)".to_owned(),
        11 => "Expression (CC 11)".to_owned(),
        64 => "Sustain (CC 64)".to_owned(),
        74 => "Brightness (CC 74)".to_owned(),
        MIDI_CC_AFTERTOUCH => "Aftertouch".to_owned(),
        MIDI_CC_PITCHBEND => "Pitch Bend".to_owned(),
        cc => format!("MIDI CC {cc}"),
    }
}

/// The form of a name that the host compares: lower case, letters and
/// digits only. "Pre-Delay" and "predelay" are the same name.
pub fn canonical(name: &str) -> String {
    name.chars()
        .filter(|char| char.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
