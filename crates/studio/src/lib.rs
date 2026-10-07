//! Native terminal studio.
//!
//! The engine and interface share a process but not a hot loop: QuickJS,
//! scheduling and device-ring production stay on the engine worker, while the
//! terminal thread owns editing, input and drawing.

pub mod app;
mod bounded_file;
mod catalogue;
mod config;
pub mod crash;
pub mod devices;
pub mod editor;
pub mod engine;
pub mod examples;
pub mod export;
pub mod file_picker;
pub mod fuzzy;
pub mod graphics;
pub mod help;
#[cfg(feature = "hydra")]
pub mod ideas;
/// Background job chip and list sheet.
pub mod jobs;
/// The one shortcut table: what every chord means, and the Settings page
/// that changes it.
pub mod keybinds;
pub mod lint;
pub mod log;
pub mod memory;
pub mod menu;
pub mod meter;
pub mod minimap;
pub mod mixer_panel;
pub mod pads;
mod performance;
mod piano;
pub mod prebake;
pub mod prefs;
pub mod reference;
#[cfg(feature = "remote-control")]
mod remote;
pub mod replay;
pub mod reveal;
mod save;
pub mod scenes;
pub mod scroll;
pub mod set_panel;
pub mod settings;
pub mod snippets;
pub mod stats;
pub mod syntax;
pub mod terminal;
pub mod textblock;
pub mod theme;
pub mod theme_editor;
mod theme_visuals;
pub mod view;
pub mod visuals;
pub mod viz_panel;
pub mod wav;
pub mod worker;

pub use app::{RecordingOptions, StudioOptions, run};
pub use engine::{
    StudioCapabilities, StudioConfig, StudioDeviceInfo, StudioDiagnostic, StudioDiagnosticLevel,
    StudioEngine, StudioInstall, StudioSnapshot, StudioStop, StudioStopHandle, StudioTick,
    StudioUpdate, StudioUpdateSendResult, try_send_update,
};
pub use piano::PianoSound;
#[cfg(feature = "remote-control")]
pub use remote::{REMOTE_PORT, parse_auth_token, parse_bind};
pub use theme::{Theme, ThemeError};
pub use worker::{
    EngineFailure, EvaluationOutcome, EvaluationSendError, RecordingOutcome, RecordingReply,
    StudioControlEvent, StudioWorker,
};
