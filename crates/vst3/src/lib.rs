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
mod worker;

pub use host::{
    FoundPreset, Host, Plugin, PluginInfo, Prepared, Resolved, ScanProgress, Status, WantedCopy,
    WorkerProgram,
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

/// The form of a name that the host compares: lower case, letters and
/// digits only. "Pre-Delay" and "predelay" are the same name.
pub fn canonical(name: &str) -> String {
    name.chars()
        .filter(|char| char.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
