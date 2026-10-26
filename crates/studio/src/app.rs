//! Interactive application loop joining the editor, renderer and engine actor.
//!
//! This file holds [`App`] itself, what building it takes, and [`run`], with
//! the few helpers every feature shares: the editor at the caret, the toast,
//! showing a file on the desktop, and what a crash report carries. What the
//! App does lives in one file per feature under `app/`, each an `impl App`
//! block of its own:
//!
//! | On screen | File |
//! |---|---|
//! | the mixer desk, master level and limiter | `mixer.rs` |
//! | the reference column (Ctrl+D) | `reference_panel.rs` |
//! | completion lists at the caret (Ctrl+Space, Tab) | `completion.rs`, `string_roles.rs`, `bank_context.rs` |
//! | hearing a chord, scale, sound or snippet | `previews.rs` |
//! | the Settings sheet | `settings_sheet.rs` |
//! | its Permissions page: origins and orchestras | `permissions.rs` |
//! | the theme picker, editor and save sheet | `theme_sheets.rs` |
//! | the export sheet | `export_sheet.rs` |
//! | the top menu and help | `menu_bar.rs` |
//! | the log and jobs sheets | `log_panel.rs`, `jobs_panel.rs` |
//! | the footer device picker, orbit routing, volume | `device_picker.rs` |
//! | pads, MIDI knobs, gamepad sticks | `controllers.rs` |
//! | the recording chip, takes, silence trim | `recording.rs` |
//! | the scene chips | `scene_strip.rs` |
//! | sets and the set panel (^B) | `sets.rs` |
//! | tapes, the replay tab and timeline | `replay_tabs.rs`, `replay_edit.rs`, `replay_pointer.rs`, `sessions.rs` |
//! | inline sliders in the score | `slider.rs`, `slider_precision.rs` |
//! | the knobs column beside the score | `knobs.rs` |
//! | the visuals docks | `viz_panel.rs` |
//! | Hydra visuals behind the score | `hydra_backdrop.rs`, `stage_pixels.rs` |
//! | split panes, scrollbars, minimap, zen, wrap | `panes.rs` |
//! | the header's go state, the footer status, lint | `readiness.rs` |
//! | the header's loading line and its words | `loading_line.rs` |
//! | evaluation flashes and their outcome | `evaluation_flash.rs` |
//! | the sample library and its sources | `sample_sources.rs` |
//! | files dropped on the studio, bank rename | `sample_import.rs` |
//! | the prebake tabs: global and per-set setup | `prebakes.rs` |
//! | ^J, the quick action at the caret | `smart_action.rs` |
//! | the computer piano | `piano_mode.rs` |
//! | jumping to the first error | `error_navigation.rs` |
//! | pasting into prompts | `prompt_paste.rs` |
//! | the remote-control sheet and its footer indicator | `remote_panel.rs`, `remote_edit.rs` |
//! | the two vst tabs, and the plugin rows of the memory breakdown | `vst.rs` |
//!
//! And the machinery every feature goes through:
//!
//! | What | File |
//! |---|---|
//! | the run loop and frame pacing | `event_loop.rs` |
//! | remote keypresses and screen snapshots | `event_loop.rs`; the TCP listener is `remote.rs`, beside `app.rs` |
//! | routing keys, and what each chord does | `keyboard.rs`, `key_bindings.rs` |
//! | routing the mouse, and the cursor's shape | `mouse.rs`, `pointer_shape.rs` |
//! | which panel is on top and has the keyboard | `focus.rs` |
//! | the list keys every panel shares | `list_keys.rs` |
//! | building and painting a frame | `render.rs` |
//! | what an edit sets off | `editing.rs` |
//! | evaluating the score, auto evaluate, and the engine's events | `evaluation.rs` |
//! | reloading the open score when another program saves it | `external_changes.rs` |
//! | saving scenes, preferences and the manifest | `persistence.rs` |
//! | quitting, play state, the errors shown | `transport.rs` |
//!
//! A method another file calls is `pub(super)`: visible inside `app`, and
//! nowhere else. Each feature keeps its tests beside its code. Shared test
//! fixtures stay in this file. `app/harness.rs` drives a real App for the
//! end-to-end suite.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::crash;
use super::devices::{
    DeviceKind, DevicePanel, DevicePanelView, DeviceScanner, MidiDirection, MidiPanelRow,
    MidiPortCounts, NO_INPUT_NAME,
};
use super::editor::ScreenMap;
use super::editor::{
    ByteOffset, Clipboard, Command, DEFAULT_MAX_DOCUMENT_BYTES, Edit, EditOrigin, Editor,
    EditorEffect, EditorInput, HistoryMoment, KeyboardCapabilities, MemoryClipboard,
    PlatformClipboard, Revision, TerminalEventBatch, Transaction, VirtualRowSpec,
    composed_text_event, event_to_input_with_binds, is_plain_text_key_event, key_to_command,
};
use super::engine::{DiagnosticAlert, Launch, LiveMaterial, StudioDiagnostic};
use super::engine::{StudioConfig, StudioDeviceInfo, StudioDiagnosticLevel, StudioSnapshot};
use super::export::{
    ExportJob, ExportSamples, ExportScore, ExportSettings, ExportSheet, SheetAction,
};
use super::file_picker::{Candidate, FilePicker, PickerChoice};
use super::help::{HelpContext, HelpState, HelpView};
use super::jobs::{BackgroundJob, JobsPanel, JobsPanelView};
use super::keybinds::BindAction;
use super::lint::{LintRequest, LintResult, Linter};
use super::log::{Level as LogLevel, LogPanel, LogPanelView, StudioLog};
use super::meter::{MasterDock, MasterState};
use super::mixer_panel::{MixerEdge, MixerPanel, MixerSelection};
use super::pads::{Pad, PadListener, PadPress};
use super::performance::{
    Performance, Publication, StudioPerformanceSnapshot, StudioRuntimeSnapshot,
};
use super::prebake::{self, PrebakeRow, PrebakeScope, PrebakeVerdict};
use super::prefs::StudioPrefs;
use super::reference::{
    PanelAction, Reference, ReferencePanel, SoundRow, Tab, engine_knows, inner_area,
};
use super::replay::ReplayTab;
use super::reveal::{Reveal, RevealError, RevealTarget, bank_location, reveal};
use super::save::{SaveRequest, SaveResult, SaveWorker};
use super::scenes::{PendingSave, SceneError, SceneId, SceneSet, SetLimiter};
use super::set_panel::{SetLine, SetPanel};
use super::settings::{
    self, ExternalChanges, OutputLatency, SettingsAction, SettingsNavigation, SettingsSheet,
    SyntaxCheck, UiSettings,
};
use super::terminal::TerminalSession;
use super::terminal::{TerminalFeatures, cell_pixels_now};
use super::theme::ThemePicker;
use super::theme::{TachyonMode, Theme, backdrop_strength};
use super::theme_visuals::{CharacterVisualSurfaces, ThemeVisualEngine, ThemeVisualSurfaces};
use super::view::{
    self, Decorations, GoState, PaneView, PanelKind, RecordingChip, SceneChip, SceneStripMode,
    StudioChrome, StudioRegions, StudioView,
};
use super::visuals::{SourceMark, VisualState};
use super::viz_panel::{
    DOCKS, METER_FLOOR_DB, MixerFacts, MixerStrip, MixerStripKind, VizPanel, WidgetKind, WidgetSpec,
};
use super::wav::take_filename;
use super::worker::{
    EvaluationOutcome, EvaluationSendError, PrebakeOutcome, RecordingOutcome, RecordingReply,
    SampleRecordingOutcome, SampleReply, StudioControlEvent, StudioWorker,
};
use rustel_audio::SampleId;
use rustel_runtime::lint::{Level as LintLevel, LintContext, LintMode, lint_with, rejection_of};
use rustel_runtime::product::EXPORT_DIRECTORY_NAME;
use rustel_runtime::samples::SoundReadiness;
use rustel_runtime::session_log::{
    SaveStatus, SessionMode, SessionRecorder, default_session_filename,
};
use rustel_runtime::ui_events::{UiLayout, UiVisual, source_revision, visual_layout};
use rustel_runtime::{ProcessMonitor, ProcessStats, RuntimeError, SessionConfig};

mod args;
mod audio_advisory;
mod bank_context;
mod completion;
mod controllers;
mod device_picker;
mod editing;
mod error_navigation;
mod evaluation;
mod evaluation_flash;
mod event_loop;
mod export_sheet;
mod external_changes;
mod find;
mod focus;
mod hydra_backdrop;
mod jobs_panel;
mod key_bindings;
mod keyboard;
mod knobs;
mod link;
mod list_keys;
mod loading_line;
mod log_panel;
mod memory_panel;
mod menu_bar;
mod mixer;
mod mouse;
mod panes;
mod permissions;
mod persistence;
mod piano_mode;
mod pointer_shape;
mod power_advisory;
mod prebakes;
mod previews;
mod prompt_paste;
mod readiness;
mod recording;
mod reference_panel;
#[cfg(feature = "remote-control")]
mod remote_edit;
#[cfg(feature = "remote-control")]
mod remote_panel;
mod render;
mod replay_edit;
mod replay_pointer;
mod replay_tabs;
mod sample_delete;
mod sample_import;
mod sample_sources;
mod scene_strip;
mod sessions;
mod sets;
mod settings_sheet;
mod slider;
mod slider_precision;
mod smart_action;
mod stage_pixels;
mod string_roles;
mod theme_sheets;
mod transport;
mod vim;
mod viz_panel;
#[cfg(feature = "vst")]
mod vst;

use bank_context::bank_machines_ranked;
use completion::{Landing, ReferenceAnchor};
use controllers::StickRun;
use device_picker::{ACTIVITY_LIGHT_MILLIS, INPUT_LIGHT_PEAK};
use editing::edits_document;
use evaluation::{PendingEvaluation, ReplayBlock, RevisionCache, virtual_rows_for};
use evaluation_flash::{EvaluationFeedback, EvaluationFlash, outcome_success};
use event_loop::MAX_EVENTS_PER_TURN;
use focus::{Focus, RotationStop, advance_focus_rotation_flash};
use keyboard::transport_key;
use list_keys::{PickerKey, page_selection, picker_key};
use mixer::{
    FADER_KEY_STEP_DB, MixerState, MixerTarget, VOLUME_KEY_STEP_DB, VOLUME_SCROLL_STEP_DB,
    orbits_in_source,
};
use mouse::{Pointer, TextSurface, widen_selection};
use panes::{HorizontalScrollbarGrab, Pane, reserve_horizontal_scrollbar};
use persistence::PREFS_DEBOUNCE;
use pointer_shape::{SHAPE_DEFAULT, SHAPE_GRABBING, SHAPE_POINTER, SHAPE_TEXT};
use prebakes::PrebakeRequest;
#[cfg(feature = "hydra")]
use previews::SnippetPreview;
use previews::{PREVIEW_STOP_WINDOW, nudge_preview_gain};
use prompt_paste::{
    MAX_RAW_PASTE_KEY_EVENTS_PER_TURN, MAX_RAW_PASTE_TERMINAL_EVENTS_PER_TURN, poll_after_input,
    read_raw_paste_chunk,
};
use readiness::{
    LINT_DEBOUNCE, LintKnowledge, READINESS_POLL, READINESS_SETTLE, Readiness, lint_alert,
    lint_has_problems,
};
use recording::{
    SampleTake, TAKE_NOTICE_DURATION, TrimJob, TrimTarget, format_take_length, trim_candidate,
};
use reference_panel::{
    CATALOGUE_LOADING_REFRESH, CATALOGUE_REFRESH, browser_imports, with_input_channels,
};
use replay_tabs::SetStash;
use sample_import::dropped_paths;
use sample_sources::{
    ShippedSource, SourceMeasurement, SourceRefresh, UserSource, sample_source_folder,
    status_sample_source, status_sample_source_error,
};
use sessions::TAPE_GESTURE_SETTLE;
use sets::{SetPrompt, SetSwitch};
use slider::{LiveChip, LiveSliderSet, SliderSpan, SliderTrack, slider_stepped};
use theme_sheets::THEME_APPLY_SETTLE;
use transport::{AppErrors, ErrorOwner};

const DEFAULT_SCORE: &str = r#"// Type normally. Use the Play menu to update or stop.
$: s("[bd <hh oh>]*2").bank("tr909").dec(.4)._pianoroll()
"#;
const DEFAULT_MINI_SCORE: &str = "bd hh sd hh";

/// How long a confirmation stays up.
///
/// Long enough to read without looking for it, short enough that it is gone
/// before it becomes part of the furniture.
pub(crate) const TOAST_DURATION: Duration = Duration::from_millis(1400);

/// How long a snippet row has to stay selected before its sounds are asked
/// for. Asking decodes them, so rows an arrow only passes over stay cold.
#[cfg(feature = "hydra")]
const SNIPPET_WARM_DWELL: Duration = Duration::from_millis(300);

/// Identifies a tab's text without reading it: its edit and saved revisions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LiveTextMark {
    edits: Revision,
    saved: u64,
}

/// FNV-1a over the saved revision text. The mark holds this hash instead of
/// a copy of the text.
fn stable_hash(text: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

impl LiveTextMark {
    fn of(scene: &super::scenes::Scene) -> Self {
        Self {
            edits: scene.editor.revision(),
            saved: stable_hash(&scene.saved_source_revision),
        }
    }
}

/// Where and how the studio records its tape.
#[derive(Clone, Debug)]
pub struct RecordingOptions {
    pub mode: SessionMode,
    /// An explicit tape path; otherwise one is written into the set's
    /// own sessions folder.
    pub file: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct StudioOptions {
    /// A set folder, or a score to open with its folder as the set. None
    /// opens the set open last time, or makes a new one.
    pub path: Option<PathBuf>,
    pub mini: bool,
    pub session: SessionConfig,
    /// Built-in theme name, user theme name, or a path to a theme file.
    /// `None` selects the default.
    pub theme: Option<String>,
    /// The audio output to open first: a device name, or `silent` for
    /// none. `None` follows the host default.
    pub output: Option<String>,
    /// The output buffer asked for, in frames: the callback size on macOS
    /// and Linux, the buffer queued ahead of the device period on Windows.
    /// `None` follows the kept "audio out latency" preference, or the
    /// automatic policy when nothing was kept.
    pub output_buffer_frames: Option<u32>,
    /// The audio input to open for `s("in")`: a device name, or a distinctive
    /// substring of one. `None` opens no input, so `s("in")` is silence until
    /// the device panel picks one.
    pub input: Option<String>,
    /// Join the Link session at start. Not kept between runs: joining is
    /// a thing done on purpose, each time.
    pub link: bool,
    /// Optional process-level cancellation observed by the UI loop. CLI
    /// frontends use this to turn SIGINT/SIGTERM into graceful restoration.
    pub cancellation: Option<Arc<AtomicBool>>,
    /// Session recording; `None` records nothing.
    pub recording: Option<RecordingOptions>,
    /// The Cargo features the binary was built with, for the About page.
    pub build_features: &'static [&'static str],
    /// Allow a background release check and print its notice after quitting.
    /// Interactive CLI sessions enable this. Embedders skip it by default.
    pub check_updates: bool,
    /// Emit bounded frame and input-to-paint measurements on stderr.
    #[doc(hidden)]
    pub performance_events: bool,
    /// Start remote control on this address and keep it across toggles.
    /// `None` starts disabled; the menu defaults to 127.0.0.1:9247.
    #[cfg(feature = "remote-control")]
    pub remote_control: Option<std::net::SocketAddr>,
    /// Reuse this token across toggles. `None` generates a fresh token and
    /// private file on each enable; disabling removes both.
    #[cfg(feature = "remote-control")]
    pub auth_token: Option<String>,
    /// Load the default sample library in the background at start. The CLI
    /// keeps the default (`true`); a harness that needs no library at all
    /// turns it off, so opening a studio never starts the load. (The studio
    /// e2e harness keeps it on - registering local banks needs the loaded
    /// library, and nothing a hermetic score names depends on the pinned
    /// manifests' background fetch.)
    #[doc(hidden)]
    pub default_samples: bool,
    /// Keep copies in an in-memory clipboard rather than the platform one,
    /// which a headless runner may not have and whose error text differs per
    /// system. The product never sets this.
    #[doc(hidden)]
    pub memory_clipboard: bool,
}

impl Default for StudioOptions {
    fn default() -> Self {
        Self::new(String::new())
    }
}

impl StudioOptions {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: Some(path.into()),
            mini: false,
            session: SessionConfig::default(),
            theme: None,
            output: None,
            output_buffer_frames: None,
            input: None,
            link: false,
            cancellation: None,
            recording: None,
            build_features: &[],
            check_updates: false,
            performance_events: false,
            #[cfg(feature = "remote-control")]
            remote_control: None,
            #[cfg(feature = "remote-control")]
            auth_token: None,
            default_samples: true,
            memory_clipboard: false,
        }
    }
}

/// Open the studio. After a panic, open it again if the performer asks.
///
/// A restart builds a new engine, a new terminal, and a new set read from
/// disk. It does not resume the studio that panicked: the same state would
/// probably panic again.
pub fn run(options: StudioOptions) -> Result<(), RuntimeError> {
    // A panic's frames are the whole crash report, so the studio wants
    // backtraces by default. An explicit `RUST_BACKTRACE` - including an
    // empty value and `0` - is the user's choice and is never overridden.
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        // SAFETY: this is the studio entry point, before `watch_for_panics`
        // or the event loop spawn any other threads that would race on the
        // environment. An explicit value - including empty or `0` - is left
        // alone by the `is_none` check above. The value is an ASCII constant.
        unsafe {
            std::env::set_var("RUST_BACKTRACE", "1");
        }
    }
    crash::watch_for_panics();
    // On macOS each plugin is created and released on the process's main
    // thread. Running there, this registers the service before the engine
    // worker starts, and the event loop pumps the thread every turn; when
    // the command line runs the studio on its worker thread, it has
    // registered already and pumps the main thread itself, so this call
    // does nothing.
    #[cfg(feature = "vst")]
    rustel_runtime::vst::install_main_service();
    let update_check = options.check_updates.then(|| {
        let (sender, receiver) = std::sync::mpsc::channel();
        rustel_runtime::updates::check_for_updates(move |message| {
            let _ = sender.send(message);
        });
        receiver
    });
    let result = loop {
        match run_once(options.clone()) {
            Err(error) => break Err(error),
            Ok(Session::Ended(result)) => break result,
            Ok(Session::Crashed { file }) => {
                let log = rustel_runtime::session_log::default_session_dir()
                    .join(super::log::LOG_FILE_NAME);
                let answer = crash::report_and_ask(
                    crash::take_report().as_ref(),
                    options
                        .path
                        .as_deref()
                        .unwrap_or_else(|| std::path::Path::new(".")),
                    log.exists().then_some(log.as_path()),
                    file.as_deref(),
                    &mut rustel_runtime::stdio::out().lock(),
                    &mut std::io::stdin().lock(),
                );
                if answer == crash::AfterCrash::Quit {
                    break Err(RuntimeError::Message(
                        "the studio stopped on a fault of its own".into(),
                    ));
                }
            }
        }
    };
    if let Some(receiver) = update_check
        && let Ok(message) = receiver.try_recv()
    {
        // The terminal can be gone here. `eprintln!` panics on a failed write.
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), "{message}");
    }
    result
}

/// How one studio ended.
enum Session {
    /// It closed, well or badly, but on its own terms.
    Ended(Result<(), RuntimeError>),
    /// It panicked. The terminal is already back, and the scores it had
    /// open are written where they can be sent on.
    Crashed { file: Option<PathBuf> },
}

/// The set a launch opens: the path named on the command line; else the
/// set open last time, when its folder is still there; else a new set in
/// the sets folder, so a first run starts with something to play.
fn open_startup_set(
    path: Option<&std::path::Path>,
    prefs: &StudioPrefs,
    starter: &str,
) -> Result<SceneSet, SceneError> {
    if let Some(path) = path {
        return SceneSet::open(path, starter);
    }
    if let Some(last) = prefs
        .last_set
        .as_deref()
        .map(std::path::Path::new)
        .filter(|last| last.is_dir())
        && let Ok(set) = SceneSet::open(last, starter)
    {
        return Ok(set);
    }
    let root = prefs.sets_directory().unwrap_or_else(|| PathBuf::from("."));
    SceneSet::create_in(&root, &today(), starter)
}

/// Today, the way a new set is named: `2026-09-06`.
fn today() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0);
    rustel_runtime::session_log::dashed_timestamp(seconds)
        .chars()
        .take(10)
        .collect()
}

/// Performance status messages hide parent folders unless explicitly enabled.
fn status_file_path(path: &std::path::Path, full: bool) -> String {
    if full {
        path.display().to_string()
    } else {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into())
    }
}

/// Enter the alternate screen and run until the user quits. Playback begins
/// only after the first evaluate command, mirroring the REPL's explicit Play
/// gesture while still allowing the score to be edited silently first.
#[allow(
    clippy::print_stderr,
    reason = "the theme fallback is reported before the screen opens"
)]
fn run_once(options: StudioOptions) -> Result<Session, RuntimeError> {
    let starter = if options.mini {
        DEFAULT_MINI_SCORE
    } else {
        DEFAULT_SCORE
    };
    let prefs = StudioPrefs::load();
    let scenes =
        open_startup_set(options.path.as_deref(), &prefs, starter).map_err(
            |error| match error {
                SceneError::Io(error) => RuntimeError::Io(error),
                SceneError::TooLarge { path, bytes } => RuntimeError::ResourceLimit(format!(
                    "studio score {} is {bytes} bytes; maximum is {}",
                    path.display(),
                    super::editor::DEFAULT_MAX_DOCUMENT_BYTES
                )),
                other => RuntimeError::Message(other.to_string()),
            },
        )?;
    // A named theme that cannot be loaded is a mistake worth reporting before
    // the alternate screen swallows the message.
    // The flag, then $RUSTEL_THEME, then what the picker remembered last time;
    // a remembered theme that has since gone missing is not worth refusing
    // to open the set over.
    let remembered = options
        .theme
        .is_none()
        .then(|| {
            std::env::var("RUSTEL_THEME")
                .ok()
                .filter(|value| !value.is_empty())
        })
        .flatten()
        .is_none()
        .then(|| StudioPrefs::load().theme)
        .flatten();
    // A remembered selector is resolved through the confined path: it comes
    // from `studio.json`, which is synced and hand-edited, so a path in it is
    // held to the theme directory. A flag or `$RUSTEL_THEME` was typed by the
    // person at the keyboard and is taken as written.
    let resolved = match (options.theme.as_deref(), remembered.as_deref()) {
        (Some(chosen), _) => Theme::resolve(Some(chosen)),
        (None, Some(remembered)) => Theme::resolve_remembered(remembered),
        (None, None) => Theme::resolve(None),
    };
    let theme = match resolved {
        Ok(theme) => theme,
        Err(error) if remembered.is_some() && options.theme.is_none() => {
            eprintln!(
                "{}",
                serde_json::json!({ "theme": { "remembered": remembered, "message": error.to_string() } })
            );
            Theme::resolve(None).map_err(|error| RuntimeError::Message(error.to_string()))?
        }
        Err(error) => return Err(RuntimeError::Message(error.to_string())),
    };
    // The window feed has two ends on two threads: the engine worker drives
    // what is drawn, and the terminal thread publishes the frames it just
    // drew. The sink is taken here, before the bridge moves to the worker.
    #[cfg(feature = "hydra")]
    let hydra = rustel_runtime::hydra::HydraBridge::new();
    #[cfg(feature = "hydra")]
    let hydra_input_policy = hydra.input_policy();
    #[cfg(feature = "hydra")]
    let hydra_frames = hydra.tui_sink();
    #[cfg(feature = "hydra")]
    let hydra_backdrop = hydra.frames();
    #[cfg(feature = "hydra")]
    let hydra_shelf = hydra.preview_frames();
    #[cfg(feature = "hydra")]
    let hydra_theme = hydra.theme_frames();
    let prefs = StudioPrefs::load();
    let sample_settings = prefs.ui_settings();
    // Before the loaders exist: the ceiling is what they will refuse a file
    // against, and a set that opens with a long sample in it must not have
    // it declined on the way in.
    rustel_audio::set_sample_pcm_ceiling(sample_settings.sample_ceiling.bytes());
    let worker = StudioWorker::spawn(
        StudioConfig {
            session: options
                .session
                .clone()
                .with_max_polyphony(sample_settings.max_polyphony),
            output: options.output.clone(),
            // The command line wins over a kept preference; a kept one
            // shapes the very first stream. `apply_output_latency` still
            // runs at startup so a live change and this agree.
            output_buffer_frames: options.output_buffer_frames.or_else(|| {
                (sample_settings.output_latency.frames() != 0)
                    .then_some(sample_settings.output_latency.frames())
            }),
            default_samples: options.default_samples,
            // What the command line named wins; otherwise the input the
            // studio was last listening through, so a set that listens opens
            // listening.
            input: options.input.clone().or_else(|| prefs.audio_input.clone()),
            preview_budget_bytes: sample_settings.preview_budget.bytes(),
            unused_sample_idle: sample_settings.unused_sample_idle.duration(),
            import_origins: prefs.import_origins.clone(),
            // `csound_allowed` stays at its default: on, because an
            // installed Csound library is the studio's opt-in. The
            // permissions page takes it back for a run, and the one-shot
            // commands keep `--allow-csound`.
            ..StudioConfig::default()
        },
        #[cfg(feature = "hydra")]
        hydra,
    )
    .map_err(|failure| RuntimeError::Message(failure.message))?;
    let mut app = App::new(options, theme, scenes, worker)?;
    // The Link session is asked for before the first turn, so the grid
    // is there to be followed from the first evaluation.
    if app.options.link {
        app.set_link(true);
    }
    // The panels come back the way they were left.
    app.restore_panels_from_prefs();
    // Said after the panels are back rather than before: opening one puts
    // its own hint in the status line, and a set file kept aside is the
    // thing worth reading when the studio starts.
    app.note_kept_manifest();
    app.note_kept_prefs();
    #[cfg(feature = "hydra")]
    {
        app.hydra_frames = Some(hydra_frames);
        app.hydra_backdrop = Some(hydra_backdrop);
        app.hydra_shelf = Some(hydra_shelf);
        app.hydra_theme = Some(hydra_theme);
        app.hydra_input_policy = Some(hydra_input_policy);
        app.sync_theme_sketch();
    }
    #[cfg(feature = "remote-control")]
    if let Some(address) = app.options.remote_control {
        let remote =
            crate::remote::listen(address, app.options.auth_token.as_deref()).map_err(|error| {
                RuntimeError::Io(std::io::Error::new(
                    error.kind(),
                    format!("remote control on {address}: {error}"),
                ))
            })?;
        app.remote = Some(Box::new(remote));
    }
    let mut terminal = TerminalSession::enter()?;
    // A plugin prints when it wants. The interface has its own copy of the
    // terminal stream, and the standard output of the process goes to the
    // null device.
    rustel_runtime::stdio::keep_clear();
    // Normal sessions move stderr into the Studio log. The hidden measurement
    // stream keeps stderr separate so its controller can consume the protocol.
    if !app.performance.enabled() && app.log.capture_stderr() {
        app.log.push(
            LogLevel::Info,
            "studio",
            "stderr is written here while the studio is open",
        );
    }
    // Catch every panic from here, so the studio can report it on a
    // terminal the performer can read. The catch starts at the terminal,
    // not at the event loop, because opening a set runs its prebakes and
    // a panic there must also be reported.
    //
    // The unwind-safety assertion is valid because the code after the
    // catch only shuts the studio down. It never plays through this App
    // again: a restart builds a new one from disk.
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        app.adopt_terminal(terminal.features().clone());
        // The setups the set opens on. Read here rather than in `App::new`
        // so building a studio in a test never reaches into the config
        // folder of whoever is running it.
        app.global_prebake = prebake::load_global_from(prebake::global_read_path().as_deref());
        app.queue_startup_prebakes();
        app.capabilities = if terminal.precise_modified_keys() {
            KeyboardCapabilities::enhanced()
        } else {
            KeyboardCapabilities::legacy()
        };
        app.piano.key_releases = terminal.reports_key_releases();
        app.emit_performance_snapshot(Publication::Started);
        app.event_loop(&mut terminal)
    }));
    // Give the user's shell back before disk/device drains or worker joins.
    drop(terminal);
    // A returned RuntimeError belongs to the shell now, so standard error
    // must leave the log first; the marker separates a completed session
    // from an interrupted one in the file. Written before shutdown so a
    // hang or panic on the way down cannot leave crash and clean looking
    // the same.
    app.log.restore_stderr();
    app.emit_performance_snapshot(Publication::Final);
    let crashed = caught.is_err();
    if crashed {
        app.log
            .session_end_marker(LogLevel::Error, "session ended - crash");
    } else {
        app.log.session_end();
    }
    app.worker.shutdown();
    let save_shutdown_error = app.shutdown_save_worker();
    // The plugins end on the process's main thread on macOS: the engine
    // worker put their copies back for the plugin thread. `pump_main`
    // serves their end when this thread is that main thread; otherwise the
    // program that owns the main thread, which still pumps while this
    // thread runs, serves the steps while this thread waits.
    #[cfg(feature = "vst")]
    {
        let ending = std::thread::spawn(rustel_runtime::vst::shutdown);
        while !ending.is_finished() {
            rustel_runtime::vst::pump_main();
            std::thread::sleep(std::time::Duration::from_millis(4));
        }
        ending
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    }
    let Ok(result) = caught else {
        // The scores are what reproduces the fault, and this is the last
        // moment they exist: the App goes out of scope with them.
        let file = crash::write_crash_file(
            crash::peek_report().as_ref(),
            app.scenes.directory(),
            &app.open_sources(),
            &app.what_happened_first(),
            app.recorded_tape().as_ref(),
            &rustel_runtime::session_log::default_session_dir(),
        )
        .ok();
        return Ok(Session::Crashed { file });
    };
    Ok(Session::Ended(match result {
        Err(error) => Err(error),
        Ok(()) => save_shutdown_error.map_or(Ok(()), Err),
    }))
}

fn within(rect: Rect, x: u16, y: u16) -> bool {
    !rect.is_empty() && x >= rect.x && x < rect.right() && y >= rect.y && y < rect.bottom()
}

#[cfg(feature = "harness")]
#[path = "app/harness.rs"]
pub mod harness;

/// Mapping slots a player can assign. Twelve is a bank of knobs on the
/// controllers people actually own, and enough faders to reach for
/// mid-performance without hunting. The settings own the count, because
/// they own the arrays a slot indexes into; a second twelve here would
/// index them past their end the day either one moved.
pub(super) use super::settings::MAPPING_SLOTS;

fn load_variants_ahead(
    sources: &[Arc<str>],
    every_variant: bool,
    library: &rustel_runtime::samples::SampleLibrary,
) {
    if sources.is_empty() {
        return;
    }
    let ahead: Vec<(String, rustel_runtime::sounds::Variants)> = sources
        .iter()
        .flat_map(|source| rustel_runtime::sounds::to_load_ahead(source, every_variant))
        .collect();
    library.load_variants_ahead(
        ahead
            .iter()
            .map(|(name, variants)| (name.as_str(), variants)),
    );
}

struct App {
    options: StudioOptions,
    #[cfg(feature = "remote-control")]
    // Boxed to keep rarely used remote-control state out of `App`. While
    // remote control is off, this is one pointer and allocates nothing.
    remote: Option<Box<crate::remote::Remote>>,
    #[cfg(feature = "remote-control")]
    remote_panel_open: bool,
    #[cfg(feature = "remote-control")]
    remote_token_revealed: bool,
    #[cfg(feature = "remote-control")]
    remote_panel_selected: usize,
    #[cfg(feature = "remote-control")]
    remote_edit: Option<Box<remote_edit::RemoteEdit>>,
    /// Configured kernel choices while no audio device is open.
    registry: rustel_runtime::CapabilityRegistry,
    theme: Theme,
    theme_visual: ThemeVisualEngine,
    scenes: SceneSet,
    /// `Box<dyn Clipboard>` so a harness can keep copies in memory; the
    /// product always constructs a `PlatformClipboard` here.
    clipboard: Box<dyn Clipboard>,
    worker: StudioWorker,
    save_worker: SaveWorker,
    visual: VisualState,
    /// One or two panes, left to right. The focused one holds the caret and
    /// is always the set's current scene.
    panes: Vec<Pane>,
    /// The set's panes as they were when a tape took the studio: a replay
    /// has the strip and the panes to itself, and closing it puts the set
    /// back the way it was. `None` in the set view.
    replay_view: Option<SetStash>,
    focused: usize,
    /// The pane the last update was sent from: where a launch lands.
    playing_pane: Option<usize>,
    reference: Reference,
    reference_panel: Option<ReferencePanel>,
    #[cfg(feature = "hydra")]
    generator_session: super::ideas::Generator,
    catalogue_refreshed_at: Instant,
    /// When the vst tab of the reference column last read the plugin list.
    #[cfg(feature = "vst")]
    plugins_polled_at: Instant,
    /// The plugin host read its folders at the last turn.
    #[cfg(feature = "vst")]
    plugins_scanning: bool,
    /// The plugins in their load at the last turn: see `rustel_runtime::vst::loads`.
    #[cfg(feature = "vst")]
    plugin_loads: Vec<String>,
    /// Where the plugin scan was at the last turn, while the scan runs.
    #[cfg(feature = "vst")]
    plugin_scan: Option<rustel_runtime::vst::ScanProgress>,
    /// The reads of the preset folders at the last turn: the watch reads
    /// them again when a file changed, and the open plugin takes the
    /// presets of that read.
    #[cfg(feature = "vst")]
    plugin_presets_version: u64,
    /// The text of the settings vst page's plugin filter. The sheet holds
    /// only its state, for its rows and its keys.
    #[cfg(feature = "vst")]
    vst_filter: settings::VstFilter,
    /// The plugins the filter kept, by their place in the full scanned
    /// list. Identity in length while no filter holds.
    #[cfg(feature = "vst")]
    vst_shown: Vec<usize>,
    /// The plugin rows of the memory breakdown, as last measured.
    #[cfg(feature = "vst")]
    plugin_memory: Vec<super::memory::PluginMemory>,
    /// The presets of the settings vst page's open plugin row, as the host
    /// last read them: see [`SettingsSheet::vst_open`].
    #[cfg(feature = "vst")]
    settings_vst_open_presets: Vec<String>,
    linter: Linter,
    /// The newest finding per scene, in the coordinates of the revision
    /// it was checked at.
    lint: HashMap<SceneId, LintResult>,
    /// The text changed since it was last handed to the linter.
    lint_pending: bool,
    /// An edit paused and auto evaluate is owed a look at the text, once
    /// the typing pause elapses.
    auto_evaluate_due: bool,
    error_jump: Option<(SceneId, Revision)>,
    /// What the checker could judge beyond the text when it was last
    /// looked at; see [`App::lint_knowledge`].
    lint_knowledge: LintKnowledge,
    /// A preferences write waiting for the reader to stop turning things.
    prefs_pending: bool,
    /// Next attempt after a failed preferences write; a new change can retry sooner.
    prefs_retry_at: Option<Instant>,
    /// A set-file write owed, and when it was asked for. Dragging the
    /// limiter's ceiling is a gesture, not an edit: writing the file per
    /// pointer report would put a hundred writes through a folder that
    /// may be on a sync client.
    manifest_pending: bool,
    /// What the sample cache held when the settings sheet last asked. A
    /// directory walk, so it is measured on opening rather than on a frame.
    sample_cache_bytes: Option<u64>,
    /// Why the open set's folder last failed to read for samples. An
    /// update that fails the same way is not said again.
    set_samples_error: Option<String>,
    /// How the sources the player imported stand, for the browser and the
    /// sources page to show. Rebuilt whenever the list changes.
    source_reports: Vec<rustel_runtime::samples::GlobalSourceReport>,
    /// The bank a rename prompt is about, under the name it arrived with -
    /// renaming it again has to replace that entry, not stack on it.
    renaming_bank: Option<String>,
    renaming_bank_source: Option<String>,
    renaming_sample: Option<(String, usize, PathBuf)>,
    renaming_session: Option<PathBuf>,
    /// A pre-cache asked for before the packs' lists were in. Paid when the
    /// loader has no manifest left in flight, on the readiness poll.
    precache_owed: bool,
    /// The pre-cache under way, or the last one, as its row follows it -
    /// the imports switch on Advanced, or the rule over the shipped packs
    /// on Sources: files queued, files left, the one in hand.
    precache: Option<super::settings::PrecacheProgress>,
    /// The packs the studio ships with, as the Sources page lists them
    /// under the imports: what each holds, and - while it is being cached
    /// onto disk - how far its files are. Counts arrive from a background
    /// walk on the readiness poll.
    shipped_sources: Vec<ShippedSource>,
    /// User imports as the Sources page follows them: local vs remote,
    /// and - for a URL pack - how much of it is on disk.
    user_sources: Vec<UserSource>,
    /// Source counts being measured off the UI thread, and requests for the next walk.
    source_measure: Option<SourceMeasurement>,
    source_refresh: SourceRefresh,
    /// The cache walk, running on its own thread; the number arrives here.
    cache_measure: Option<std::sync::mpsc::Receiver<u64>>,
    /// Next time a downloading cache may remeasure its on-disk size.
    cache_measure_due: Option<Instant>,
    /// Emptying the sample cache, off the UI thread - a multi-gigabyte
    /// folder walk must not freeze the sheet on Enter.
    cache_clear: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
    /// Silence scans and WAV rewrites can cover minutes of audio. They run
    /// away from the event/render thread so stopping a take or confirming a
    /// manual trim never freezes the desk.
    trim_jobs: Vec<TrimJob>,
    /// Files the sample loader still had in its line when the readiness
    /// poll last looked. Read on a frame, so it is sampled on the poll
    /// rather than measured under the painter.
    caching_samples: usize,
    /// The library still has manifests on the way, sampled on the same
    /// half-second as everything else that asks the loader how it is
    /// doing. Reading the counter itself takes a lock, and the browser's
    /// count row is not worth one every frame.
    library_loading: bool,
    prefs_changed_at: Instant,
    /// The limiter mode `c` landed on and when it is applied, while the key
    /// may still be walking the ladder.
    limiter_mode_due: Option<(rustel_audio::LimiterCharacter, Instant)>,
    manifest_changed_at: Instant,
    last_edit_at: Instant,
    /// The open score's file as the external watch last saw it, and when
    /// it last looked. See `external_changes.rs`.
    external_watch: Option<external_changes::ExternalWatch>,
    external_watch_checked_at: Instant,
    /// The word the reference column was opened on, so the name chosen
    /// there replaces it rather than landing beside it.
    reference_anchor: Option<ReferenceAnchor>,
    /// A slider moved; once it settles, the sounding score goes on the tape.
    slider_tape_due: Option<Instant>,
    /// The fader moved; once it settles, its value goes on the tape.
    fader_tape_due: Option<Instant>,
    /// The last reported error for the active take.
    reported_take_error: Option<String>,
    pending_recording: Option<u64>,
    active_recording_id: Option<u64>,
    /// The sample being recorded from the input: when it started, for the
    /// header's clock, and where it goes.
    sample_take: Option<SampleTake>,
    /// A sample start or finish the engine has not answered yet.
    pending_sample: Option<u64>,
    last_finished_take: Option<RecordingOutcome>,
    /// A take ended by itself; said quietly in the header until this.
    take_notice: Option<(String, Instant)>,
    /// What went on under the scene.
    log: StudioLog,
    log_panel: Option<LogPanel>,
    /// Background jobs list (downloads, exports, …), while open.
    jobs_panel: Option<JobsPanel>,
    /// Docked memory breakdown and its pending layout state.
    memory_dock: Option<super::memory::MemoryDock>,
    memory_unlaid: bool,
    /// Scores in the set folder with no open tab.
    memory_closed_files: usize,
    /// The set panel, while it is open.
    set_panel: Option<SetPanel>,
    /// A set being chosen or renamed from the menu, while it is.
    set_prompt: Option<(SetPrompt, FilePicker)>,
    /// The surface that opened the prompt, retained through nested pickers.
    set_prompt_return_focus: Option<Focus>,
    /// The visuals docks that are open, and which of them has the keyboard.
    viz_docks: [Option<VizPanel>; DOCKS],
    viz_focus: usize,
    /// Docks opened or resized since the last draw. Only a paint refreshes
    /// `regions`, and the event loop drains keys faster than it draws. A
    /// key can arrive while the dock's region is still the empty rectangle
    /// from before the dock existed. Dispatch on that region would pass the
    /// key to the score while the screen shows the focus on the dock. A
    /// dock flagged here keeps the keyboard until a draw sets its region.
    viz_unlaid: [bool; DOCKS],
    /// The mixer desk's region is judged at draw time like the docks':
    /// from its opening (or a resize) until the next draw, the desk keeps
    /// the keyboard while the status claims it - keys do not fall through
    /// to the score behind an empty region.
    mixer_unlaid: bool,
    /// The mixer panel - the desk along the top or the bottom - while it
    /// is open. Sticky like the set panel: Esc leaves it standing.
    mixer_panel: Option<MixerPanel>,
    /// The knobs column - a rail for each numeric parameter of the call
    /// at the caret - while it is open. Sticky like the mixer: Esc leaves
    /// it standing.
    knobs_panel: Option<super::knobs_panel::KnobsPanel>,
    /// Opened since the last draw, like `mixer_unlaid`: until a draw lays
    /// the column, its keys must not fall through to the score.
    knobs_unlaid: bool,
    /// The call at the caret as the panel last read it, kept until the
    /// scene, the text or the caret changes. The parse is the heavy part,
    /// with the faders the transpiler's own reading, so it is done once
    /// per change, not once per frame.
    knobs_parse: Option<knobs::KnobsParse>,
    /// The rows the painter draws this frame, assembled from the parse.
    knobs_rows: Option<knobs::KnobsRows>,
    /// A drag selection over the mixer's devices block - the MIDI log and
    /// the pads - kept with the rows it was drawn on, for `c` to copy.
    mixer_selection: Option<MixerSelection>,
    /// Pixels a backdrop is asked for, tuned to what this terminal can
    /// actually carry in a frame. See [`App::watch_pixel_frame`].
    pixel_budget: u64,
    /// Whether the log has already said the picture was made smaller, so
    /// it is said once and not on every step down.
    pixel_budget_said: bool,
    /// The mapping slot the settings sheet is waiting to learn.
    mapping_learn: Option<usize>,
    /// The keybind learn the settings sheet is holding. While one is
    /// armed, the next key event that is not the sheet's own is the
    /// chord - exactly how the mapping learn reads its knob - and when a
    /// chord is another action's, the learn asks first instead of taking
    /// it in silence.
    keybind_learn: Option<super::keybinds::KeybindLearn>,
    /// The learnt keyboard shortcuts. Built from the preferences once and
    /// re-learnt from there, so every surface asks the same table.
    keybinds: super::keybinds::Keybinds,
    vim_config: crate::vim_config::VimConfig,
    /// Plain keys waiting for an insert mapping in a Vim text surface.
    vim_insert_pending: String,
    /// This key came from remote control. A machine sends normal keys, not
    /// Vim modes, so the score reads it with its usual editing.
    remote_key_event: bool,
    /// The value each control change last said and when it said it, keyed
    /// by number and channel, so a bound row reads the crossings of the
    /// line at 64 rather than every message: a knob swept across fires
    /// once each way, and a button that toggles between 127 and 0 - the
    /// Maschine's transport in MIDI mode - fires on both presses, its
    /// second held apart from a hand's release by the length of the
    /// first.
    midi_cc_last: HashMap<(u8, u8), (u8, Instant)>,
    /// When each note was last pressed, keyed by note and channel, so a
    /// button that latches can be told from one that springs back: a note
    /// that ends long after it began was not released, it was pressed
    /// again - the Maschine's transport in MIDI mode, whose second press
    /// is a note-off. An ending with no stamp belongs to a note that began
    /// before the studio did, and reads as that button's next press too.
    note_pressed_at: HashMap<(u8, u8), Instant>,
    /// When each slot's control last moved, so a bound slot can say it is
    /// live without a player having to arm a learn to find out.
    mapping_lit: [Option<Instant>; MAPPING_SLOTS],
    /// Each stick slot's run on the fader it is pushing, between frames.
    stick_run: [Option<StickRun>; MAPPING_SLOTS],
    /// Where each slot's knob was when it last spoke, so a turn can be
    /// read as a movement rather than only as a position.
    mapping_knob: [Option<f64>; MAPPING_SLOTS],
    /// When the stick last pushed, for the run's time step.
    stick_ticked_at: Instant,
    /// A stick is held over: the loop keeps frames coming.
    stick_pushing: bool,
    /// When each surface was last raised - opened or given the keyboard -
    /// so the reference and the sheets stack in that order, the latest on
    /// top, for painting, for the pointer and for Esc alike.
    raised: std::collections::HashMap<PanelKind, u64>,
    raise_counter: u64,
    /// The tapes open as replay tabs, by the tab's scene.
    replays: HashMap<SceneId, ReplayTab>,
    /// The export sheet, when it is open.
    export_sheet: Option<ExportSheet>,
    /// The last choices, so the second export is Enter.
    export_settings: ExportSettings,
    /// A render in progress.
    export_job: Option<ExportJob>,
    /// The last file the studio made - an export, a take - for ⌘⇧O and a
    /// click on the status line.
    last_file: Option<PathBuf>,
    /// Exact status message associated with the file-manager action.
    last_file_status: Option<String>,
    /// The latest sample recorded from the input this session, as the
    /// score names it: `recordings:2`. `None` once the latest one lands
    /// where no bank plays it. ^J offers to paste it.
    last_recorded_sample: Option<String>,
    /// The desktop is being asked to show something.
    reveal_pending: Option<Reveal>,
    /// The theme picker, when it is open.
    theme_editor: Option<super::theme_editor::ThemeEditor>,
    /// The last OSC 22 pointer shape asked of the terminal.
    pointer_shape_sent: Option<&'static str>,
    /// A terminal modifier override can change the visible pointer without
    /// updating its OSC 22 cache. Force a distinct shape before restoring ours.
    pointer_shape_reset: bool,
    /// Defer modifier recovery while either physical Shift key owns selection.
    pointer_shift_held: [bool; 2],
    /// Where the pointer was last seen, so a screen that changes under a
    /// still pointer - a sheet opened from the keyboard - still gets the
    /// right shape.
    pointer_at: Option<(u16, u16)>,
    /// The interface opacity from before the theme editor opened, so a
    /// draft's live-applied opacity can be put back on Esc.
    opacity_before_theme_editor: Option<u8>,
    /// The last sketch text run through the parser, and its verdict - so
    /// the per-pass sketch pump costs a string compare, not a parse.
    #[cfg(feature = "hydra")]
    theme_sketch_checked: Option<(String, Option<String>)>,
    /// The camera-backed theme explicitly kept by Enter/save or selected at
    /// startup. A theme-editor draft never acquires on the strength of this;
    /// a picker preview acquires on the webcam setting instead, which is
    /// where the consent for it lives.
    #[cfg(feature = "hydra")]
    theme_camera_confirmed: Option<String>,
    theme_picker: Option<ThemePicker>,
    /// What the studio remembers between sessions.
    prefs: StudioPrefs,
    /// The global prebake's text, as stored. Loaded by `run` rather than by
    /// `App::new`, so building a studio for a test never reads the config
    /// folder of whoever is running it.
    global_prebake: String,
    /// Where that text is kept; `None` when this machine has no config
    /// folder, which is reported when a save is attempted rather than at
    /// startup.
    global_prebake_path: Option<PathBuf>,
    /// Setups waiting to be sent, in the order they must run.
    prebake_queue: VecDeque<PrebakeRequest>,
    /// The setup the engine is running now.
    prebake_inflight: Option<PrebakeRequest>,
    /// Hash of the text last applied and last refused, per scope, so a row
    /// can say what became of what is stored rather than of what once ran.
    prebake_applied: [Option<String>; 2],
    prebake_rejected: [Option<String>; 2],
    /// What each scope's last successful setup defined, for the checker.
    /// Only a setup that ran counts: what a refused one managed before it
    /// threw is on the heap, but which names those are cannot be known.
    applied_setup: [Arc<rustel_transpiler::SetupDefinitions>; 2],
    /// The rename prompt still shows the old name: the first key replaces it.
    rename_untouched: bool,
    /// What this terminal can do, found out when it was entered.
    features: TerminalFeatures,
    /// The switches, as remembered and as shown.
    ui_settings: UiSettings,
    /// The clock ports the engine was last told about.
    applied_clock: (Option<String>, Option<String>),
    /// The preview RAM policy still owed to the engine.
    sample_memory_owed: Option<(usize, Duration)>,
    /// Which MIDI ports may be opened, either way.
    ///
    /// `None` until the first device scan completes. A studio with no kept
    /// choice enables every port that is present, and before the scan no
    /// port is known, so every port would be off. While this is `None` the
    /// engine allows every port, so a set started right after launch is
    /// not silenced before the scan returns.
    midi_enabled: Option<super::devices::MidiEnablement>,
    /// What the engine was last told, so it is told again only on a change.
    applied_midi_enabled: Option<super::devices::MidiEnablement>,
    /// How the next evaluate takes effect: a launch sets it, an edit is
    /// always now.
    next_launch: Launch,
    /// The next thing played starts from its own cycle zero: ^⇧S, or the
    /// setting that says every score moved to does. Consumed by the
    /// evaluation it belongs to, the way `next_launch` is.
    next_rewind: bool,
    /// The revision pinned in the audible editor's edit trail, so it can be
    /// unpinned when the next evaluation replaces it.
    visual_pin: Option<(SceneId, Revision)>,
    /// The scene whose launch is waiting for its cycle line.
    armed_scene: Option<SceneId>,
    /// The request that armed it: only ITS outcome ends the arming. The
    /// worker answers a superseded launch with a cancel for the OLD request
    /// after the new one is already armed here, and that answer must not
    /// wipe the new arming.
    armed_request: Option<u64>,
    /// The text that is armed, so pressing update again on it is a no-op
    /// rather than a re-arm that can slip the launch to the next line.
    armed_revision: Option<String>,
    /// The armed scene's name, kept past the fire: the reload runs a
    /// head-room early, but the line is still ahead and the countdown
    /// keeps its name until then.
    landing_name: Option<String>,
    settings_sheet: Option<SettingsSheet>,
    settings_navigation: SettingsNavigation,
    /// A source a take registered for itself: when it settles it still
    /// belongs in the log, but its receipt must not replace the status
    /// line that named the take.
    quiet_settled_source: Option<String>,
    /// Where each scene's sounds stand, from the last clean check.
    readiness: HashMap<SceneId, Readiness>,
    readiness_polled_at: Instant,
    /// The scene last typed into, and when its sounds are next asked for.
    readiness_due: Option<(SceneId, Instant)>,
    /// The scene being typed in, while the typing has not paused long
    /// enough for its caret line to show its marks.
    marks_muted: Option<SceneId>,
    capabilities: KeyboardCapabilities,
    piano: piano_mode::PianoMode,
    /// The revision of the audible scene the installed layout was evaluated
    /// at, and the text of that revision.
    visual_revision: Option<Revision>,
    evaluated_source: Option<Arc<str>>,
    visual_replay_block: Option<ReplayBlock>,
    /// The scene the engine's layout came from. Decorations belong to it.
    audible_scene: Option<SceneId>,
    evaluation_revisions: RevisionCache,
    /// Hash of the source currently installed and sounding, if any.
    ///
    /// Update is held down as often as it is pressed, and each repeat used to
    /// tear down a generation and build a byte-identical one. The cutovers
    /// stack up faster than the music absorbs them, and it is audible.
    installed_revision: Option<String>,
    inflight: HashMap<u64, PendingEvaluation>,
    /// The text the footer's update message is about: the scene and the
    /// revision the engine refused. Once that text has changed and the
    /// checker finds it fine, the message comes down without waiting for
    /// the next update to say the same.
    update_failure: Option<(SceneId, Revision)>,
    /// The origins whose script imports the engine refused in this studio,
    /// newest last. The permissions page offers them, so nobody types an origin.
    blocked_imports: Vec<String>,
    /// Whether native orchestras run in this studio. The switch starts on -
    /// an installed library is the opt-in - and the permissions page flips
    /// it for the run. The studio keeps neither state, so the next start is
    /// on again.
    csound_allowed: bool,
    /// A score asked for an orchestra and the engine refused it, so the
    /// permissions page offers the switch.
    csound_refused: bool,
    /// The permissions page flipped the switch and the engine does not
    /// have it yet.
    csound_owed: bool,
    /// The permissions page changed and the engine does not have the list yet.
    import_origins_owed: bool,
    /// A permit unblocked a refused update and the studio owes its re-run.
    /// It waits for the engine to hold the permission, so the re-run cannot
    /// be refused by the state it is running to use.
    refused_update_owed: bool,
    /// Latest refused revision for each scene with an unresolved log alert.
    evaluation_alerts: HashMap<SceneId, Revision>,
    /// A diagnostic copied into the ordinary status line by the navigation
    /// shortcut. Its exact text is kept so a newer lint result can retire
    /// only that message without erasing unrelated status written since.
    lint_status: Option<(SceneId, Revision, String)>,
    pending_evaluation: Option<PendingEvaluation>,
    /// The newest slider move the engine has not accepted yet.
    pending_slider: Option<(String, f64, bool)>,
    next_request_id: u64,
    snapshot: Option<StudioSnapshot>,
    /// Whether the Link session was asked for. Kept nowhere: each run
    /// starts out of the session.
    link_enabled: bool,
    /// What the Link session's transport last said, once a stand was
    /// taken on it. `None` when no session is joined.
    link_seen_playing: Option<bool>,
    audio_advisory: audio_advisory::AudioAdvisory,
    power_advisory: power_advisory::PowerAdvisory,
    /// The output the last play went through, or the one picked in the
    /// device panel since. A stopped studio has no stream open, and this is
    /// what the footer names then: the device the next evaluation opens.
    resting_output: Option<String>,
    /// When the input last carried signal, so its chip stays lit for the
    /// same quarter second every other device light does.
    input_lit_at: Option<Instant>,
    status: String,
    status_notice: Option<(String, Instant)>,
    errors: AppErrors,
    next_save_request_id: u64,
    quit: bool,
    started: Instant,
    dirty_frame: bool,
    /// The screen is stale because the player did something - a key, a
    /// click, a resize - as opposed to a turn's animation or the engine's
    /// visuals. Typed text must appear at once, so this one is never held
    /// for the frame interval; the animated kind is.
    input_dirty: bool,
    next_frame: Instant,
    caret_activity: Instant,
    caret_visible: bool,
    /// When the work the header's busy dot breathes for began; `None`
    /// while the studio is idle. Held apart from `is_evaluating()` so
    /// the dot is timed by when it started, not by how the work ends.
    evaluating_since: Option<Instant>,
    evaluation_flash: Option<EvaluationFlash>,
    evaluation_feedback: Option<EvaluationFeedback>,
    /// Which panel `rotate_panel_focus` last landed the keyboard on, and
    /// until when it keeps its brief accent - `None` the instant that
    /// passes. Only the rotation arms this: a chord or a click that focuses
    /// a panel directly already shows you where it went, and lighting up for
    /// those too would just be noise.
    focus_rotation_flash: Option<(RotationStop, Instant)>,
    performance: Performance,
    /// Where each drawn frame goes when a sketch asked for `feedStrudel`.
    /// `None` in a build without the visuals window, and while none is open.
    #[cfg(feature = "hydra")]
    hydra_frames: Option<rustel_hydra::HydraTuiSink>,
    /// Hydra's own frames, when a sketch is drawing for the terminal rather
    /// than for a window of its own.
    #[cfg(feature = "hydra")]
    hydra_backdrop: Option<rustel_hydra::HydraFrames>,
    /// The shelf's frames. A second Hydra draws these, which is what lets a
    /// snippet be previewed without taking the screen off the score.
    #[cfg(feature = "hydra")]
    hydra_shelf: Option<rustel_hydra::HydraFrames>,
    /// The theme's own picture, when the theme carries a sketch.
    #[cfg(feature = "hydra")]
    hydra_theme: Option<rustel_hydra::HydraFrames>,
    /// Direct privacy control for score-requested webcam input. This bypasses
    /// the worker command queue so revocation cannot be dropped under load.
    #[cfg(feature = "hydra")]
    hydra_input_policy: Option<rustel_runtime::hydra::HydraInputPolicy>,
    /// What was last sent as the theme's sketch, so a redraw is not a
    /// re-send.
    #[cfg(feature = "hydra")]
    theme_sketch_sent: Option<(Option<String>, bool)>,
    /// The selected picker row, waiting for arrows to stop repeating.
    theme_apply_due: Option<Instant>,
    /// The newest theme frame, or the last one still.
    #[cfg(feature = "hydra")]
    hydra_theme_last: Option<super::view::VisualBackdrop>,
    /// The last frame Hydra sent. Kept because the terminal redraws on its own
    /// schedule and a frame that has not been replaced is still the picture.
    #[cfg(feature = "hydra")]
    hydra_last: Option<super::view::VisualBackdrop>,
    /// The last frame the shelf's Hydra sent, kept for the same reason.
    #[cfg(feature = "hydra")]
    hydra_shelf_last: Option<super::view::VisualBackdrop>,
    /// The snippet the shelf last asked to see, so the same one is not sent to
    /// the engine on every keystroke.
    #[cfg(feature = "hydra")]
    hydra_preview_sent: Option<String>,
    /// The frame sizes last asked for, so a resize says it once.
    #[cfg(feature = "hydra")]
    hydra_frame_size_sent: Option<rustel_runtime::hydra::HydraFrameRequest>,
    /// A short-lived confirmation of something the reader just did, and when
    /// it stops being shown.
    toast: Option<(String, Instant)>,
    engine_connected: bool,
    master: MasterState,
    devices: DeviceScanner,
    panel: Option<DevicePanel>,
    strip_mode: SceneStripMode,
    pads: PadListener,
    /// When the turn that last drained the pads began. The pad-arrival
    /// deadline counts from here (see [`event_loop::pad_wait_cap`]), not
    /// from the drain a few steps into the turn. The turn starts where the
    /// loop's previous wait ended, which on Windows's default timer is a
    /// clock tick. A deadline 15 ms after that tick falls before the next
    /// tick. A deadline counted from a drain one millisecond later would
    /// round up to the tick after.
    pads_drained_at: Instant,
    /// The launch-pad set last accepted by the worker, so the per-turn
    /// sync sends only on change. `None` until the first send lands.
    launch_pads_sent: Option<Vec<(u8, u8)>>,
    /// Tabs by most recent visit, and material last accepted by the engine.
    scene_visits: Vec<SceneId>,
    tabs_closed: u64,
    live_material_sent: Option<Vec<(SceneId, LiveTextMark, bool)>>,
    tabs_closed_sent: u64,
    plugins_shown_sent: Vec<String>,
    setups_select_variants_sent: bool,
    sent_setup_selected_variants: bool,
    /// The mixer widget's faders and which strip the keys drive.
    mixer: MixerState,
    /// The chosen output buffer has not reached the engine yet: a size
    /// kept from last time is applied to the engine that starts with
    /// this one rather than only when the row is next touched.
    output_latency_pending: bool,
    /// `--buffer-frames` from the command line, which wins over the kept
    /// preference for this run: the sheet's row shows it and the engine
    /// is given it, instead of the kept size (or Automatic) overwriting
    /// it the moment the terminal was adopted.
    cli_output_latency: Option<OutputLatency>,
    /// The input's meter, with the master's ballistics: the held peak
    /// falls the same way, so the two strips read alike.
    input_meter: MasterState,
    /// The orbits the score names, read off the text as it is typed (on
    /// the lint's debounce), so a strip is there before the score is
    /// evaluated and goes when the line does.
    orbits_in_code: Vec<u8>,
    recorder: Option<SessionRecorder>,
    recorder_started: Instant,
    process: ProcessMonitor,
    /// When set, the frame's observe-the-host inputs - the process CPU and
    /// memory readout, the harness's own render timing, and the host MIDI
    /// inventory - are pinned to fixed values instead of read, so a rendered
    /// frame is byte-identical across machines.
    ///
    /// The field exists only under `harness`. A release build has no such
    /// switch, so the product cannot show pinned CPU, memory or MIDI values.
    #[cfg(feature = "harness")]
    pin_frame_inputs: bool,
    /// Geometry of the last painted frame, used to route pointer input.
    frame: Rect,
    regions: StudioRegions,
    pointer: Option<Pointer>,
    /// How far into its cell the pointer was at the last report, 0..1. A
    /// terminal that reports pixels sets it; one that reports cells leaves
    /// it at 0.
    pointer_subcell: f64,
    /// The same fraction down a cell, for the controls that stand up.
    ///
    /// A slider lies along its row and needs only the horizontal fraction.
    /// The desk's faders are columns: without this fraction a fader resolves
    /// to a whole cell, about sixteen steps for the master's whole travel
    /// against the slider's hundreds. The fader is still drawn on the cell
    /// grid, but its value comes from the pointer's position inside the cell.
    pointer_subrow: f64,
    /// Last pty scale, which can differ from the terminal's direct answer.
    /// An unchanged pty guess must not overwrite that authoritative size.
    last_pty_cell_pixels: Option<(u16, u16)>,
    /// Who has the keyboard; see [`Focus`].
    focus: Focus,
    /// Global contextual help. This deliberately does not participate in
    /// [`Focus`] or [`PanelKind`], so closing it reveals precisely the same
    /// editor, panel, nested prompt, selection, and armed control.
    help: Option<HelpState>,
    /// A quit waiting for its second press. `^Q` asks, the next `^Q` goes,
    /// and anything else calls it off.
    quit_armed: bool,
    /// Where a set switch was going while the set panel asks about a score
    /// that could not be saved.
    pending_switch: Option<SetSwitch>,
    /// Set while the menu bar has the keyboard.
    ///
    /// Its own field rather than a `Focus`/`PanelKind` variant, exactly as
    /// `help` is: `settle_focus` forces focus back onto the theme editor
    /// whenever that sheet is up, and would take the keyboard off the menu
    /// on the very next key.
    menu: Option<super::menu::MenuState>,
    /// Double- and triple-click detection for the reference column's text.
    clicks: super::textblock::ClickCounter,
    /// A slider the arrows adjust: set by any gesture on a control, let go
    /// by Esc, an edit, or a press anywhere else.
    armed_slider: Option<(SceneId, usize)>,
    slider_precision: Option<slider_precision::PrecisionSlider>,
    /// The quick action's menu, open at the caret.
    smart_action: Option<smart_action::SmartAction>,
    replay_edit: Option<replay_edit::ReplayEdit>,
    /// How loud a sample preview plays, 1.0 being unity. It goes past unity
    /// on purpose: a browser next to a playing set needs headroom to be
    /// heard through the music.
    preview_gain: f32,
    /// Which of the score's faders on the mixer's desk the keys drive,
    /// and whether the keys are on that block at all rather than on the
    /// strips to its left. Tab crosses between them.
    mixer_fader: usize,
    mixer_on_faders: bool,
    /// The last preview and when it started: Space on its row stops it
    /// while it may still be sounding; choosing a sound or leaving the
    /// browser always does.
    preview_armed: Option<(String, Instant)>,
    /// When the preview's picture expires. A chord recolours its voicing
    /// and a run lights its steps only for as long as they sound; the
    /// studio repaints once more when that ends, so the keys go out
    /// instead of staying lit. (The sound itself is Space's to stop.)
    preview_lit_until: Option<Instant>,
    /// The sounding preview's shape and length, for the browser to draw it
    /// against. Taken once when the preview starts and again on the poll
    /// while it is still `None`: a sound heard for the first time is being
    /// decoded at the moment it is asked for.
    preview_shape: Option<(std::sync::Arc<[u8]>, f64)>,
    /// How long the studio has had sound, in seconds, and when that was
    /// last counted.
    ///
    /// The visuals animate on this rather than on the wall clock, so a
    /// stopped set holds its pictures where they were: a widget moving
    /// over silence reads as playback.
    sound_seconds: std::cell::Cell<f32>,
    sound_counted_at: std::cell::Cell<Instant>,
    /// The snippet playing under the score, if one is: a second player,
    /// installed alongside the set rather than in place of it.
    #[cfg(feature = "hydra")]
    snippet_preview: Option<SnippetPreview>,
    #[cfg(feature = "hydra")]
    generator_preview_pending: bool,
    #[cfg(feature = "hydra")]
    generator_preview_epoch: u64,
    #[cfg(feature = "hydra")]
    /// Last generator edit, retained across loading/ready transitions so
    /// rapid installations cannot flash the progress strip during a hold.
    generator_feedback_since: Option<Instant>,
    #[cfg(feature = "hydra")]
    generator_last_preview: Option<Instant>,
    /// Whether anything has been heard from the preview yet: a mark of its
    /// own coming back. Until then it is still on its way, and the shelf
    /// says so rather than looking broken.
    #[cfg(feature = "hydra")]
    snippet_heard: bool,
    /// When the engine's current loading cue first arrived.
    loading_since: Option<Instant>,
    /// Whether the header's loading line, and its words, were due at the
    /// last look: a change repaints.
    loading_due: (bool, bool),
    /// The snippet whose sounds have been asked for, so a row read twice
    /// is not asked for twice.
    #[cfg(feature = "hydra")]
    warmed_snippet: Option<String>,
    /// The snippet under the cursor and when it became selected.
    #[cfg(feature = "hydra")]
    snippet_under_cursor: Option<(String, Instant)>,
    /// A stop has been asked for and the engine has not said so yet.
    ///
    /// The worker stops on an atomic; the snapshot that reports it arrives
    /// up to a tenth of a second later, and until it does `is_playing`
    /// still says yes. Anything deciding "is the set playing" in that
    /// window has to consult this as well, or it acts on a set that is
    /// already on its way out.
    stop_requested: bool,
    /// Source marks of the audible generation, mapped into the coordinates of
    /// the revision on screen.
    marks: Vec<SourceMark>,
    /// The same, for a snippet playing under the score: ranges into the
    /// snippet as the shelf shows it, so the preview lights up where the
    /// reader is looking.
    #[cfg(feature = "hydra")]
    snippet_marks: Vec<SourceMark>,
    mini_spans: Vec<(usize, usize)>,
    /// Slider literals of the audible generation, in current coordinates.
    sliders: Vec<SliderSpan>,
    /// The revision `sliders` is expressed in.
    sliders_revision: Option<Revision>,
    /// What each scene's current text holds, control-wise; see
    /// [`LiveSliderSet`].
    live_sliders: HashMap<SceneId, LiveSliderSet>,
}

impl App {
    fn new(
        options: StudioOptions,
        theme: Theme,
        mut scenes: SceneSet,
        worker: StudioWorker,
    ) -> Result<Self, RuntimeError> {
        let now = Instant::now();
        let (clipboard, clipboard_status): (Box<dyn Clipboard>, &str) = if options.memory_clipboard
        {
            (
                Box::new(MemoryClipboard::default()),
                "ready - terminal paste and the internal clipboard are available",
            )
        } else {
            let platform = PlatformClipboard::new();
            let status = if platform.system_available() {
                "ready - evaluate the score to start audio"
            } else {
                "ready - terminal paste and the internal clipboard are available"
            };
            (Box::new(platform), status)
        };
        // The scene on screen gets its inline widgets placed before anything
        // has played, so a score that asks for a piano roll opens with room
        // for one.
        let mut visual = VisualState::default();
        let mut visual_revision = None;
        let mut evaluated_source = None;
        let mut audible_scene = None;
        let mut evaluation_revisions = RevisionCache::default();
        {
            let scene_id = scenes.current().id;
            let scene = scenes.current_mut();
            let source = scene.editor.source();
            let revision = scene.editor.revision();
            if let Ok(layout) = visual_layout(&source, 0) {
                let hash = layout.ui_layout.source_revision.clone();
                if let Ok(rows) = virtual_rows_for(&layout.ui_layout, Some, |_| true) {
                    let _ = scene.editor.set_virtual_rows(revision, rows);
                    visual_revision = Some(revision);
                    audible_scene = Some(scene_id);
                }
                visual.install_layout(layout);
                let source: Arc<str> = Arc::from(source.as_str());
                evaluated_source = Some(Arc::clone(&source));
                evaluation_revisions.remember(hash, revision, source, scene_id, None);
            }
        }
        let save_worker = SaveWorker::spawn().map_err(RuntimeError::Io)?;
        let first_scene = scenes.current().id;
        let performance_events = options.performance_events;
        // The studio's own log is one file for every set, beside the
        // preferences, never inside a set's folder.
        let log = StudioLog::open(Some(&rustel_runtime::session_log::default_session_dir()));
        #[cfg(feature = "hydra")]
        let theme_camera_confirmed = theme.camera_enabled().then(|| theme.name.clone());
        let prefs = StudioPrefs::load();
        let mixer = MixerState::new(prefs.input_gain_tenths_db);
        // The switch starts on, as the engine's does: an installed library
        // is the opt-in, and the permissions page owns the switch from here.
        let csound_at_start = true;
        let mut app = Self {
            output_latency_pending: true,
            cli_output_latency: options.output_buffer_frames.map(OutputLatency::from_frames),
            global_prebake: String::new(),
            global_prebake_path: prebake::global_path(),
            prebake_queue: VecDeque::new(),
            prebake_inflight: None,
            prebake_applied: [None, None],
            prebake_rejected: [None, None],
            applied_setup: [Arc::default(), Arc::default()],
            registry: rustel_runtime::capability_registry_for_dispatch(
                options.session.dsp_dispatch,
            ),
            options,
            #[cfg(feature = "remote-control")]
            remote: None,
            #[cfg(feature = "remote-control")]
            remote_panel_open: false,
            #[cfg(feature = "remote-control")]
            remote_token_revealed: false,
            #[cfg(feature = "remote-control")]
            remote_panel_selected: 0,
            #[cfg(feature = "remote-control")]
            remote_edit: None,
            theme,
            theme_visual: ThemeVisualEngine::default(),
            scenes,
            clipboard,
            worker,
            save_worker,
            visual,
            panes: vec![Pane {
                scene: first_scene,
                last_map: None,
            }],
            replay_view: None,
            focused: 0,
            playing_pane: None,
            reference: Reference::load(engine_knows()),
            reference_panel: None,
            #[cfg(feature = "hydra")]
            generator_session: super::ideas::Generator::default(),
            catalogue_refreshed_at: now,
            #[cfg(feature = "vst")]
            plugins_polled_at: now,
            #[cfg(feature = "vst")]
            plugins_scanning: false,
            #[cfg(feature = "vst")]
            plugin_loads: Vec::new(),
            #[cfg(feature = "vst")]
            settings_vst_open_presets: Vec::new(),
            #[cfg(feature = "vst")]
            plugin_scan: None,
            #[cfg(feature = "vst")]
            plugin_presets_version: 0,
            #[cfg(feature = "vst")]
            vst_filter: settings::VstFilter::default(),
            #[cfg(feature = "vst")]
            vst_shown: Vec::new(),
            #[cfg(feature = "vst")]
            plugin_memory: Vec::new(),
            linter: Linter::spawn().map_err(RuntimeError::Io)?,
            lint: HashMap::new(),
            // Check what was opened before anything is typed.
            lint_pending: true,
            auto_evaluate_due: false,
            error_jump: None,
            lint_knowledge: LintKnowledge::default(),
            prefs_pending: false,
            prefs_retry_at: None,
            manifest_pending: false,
            sample_cache_bytes: None,
            set_samples_error: None,
            source_reports: Vec::new(),
            renaming_bank: None,
            renaming_bank_source: None,
            renaming_sample: None,
            renaming_session: None,
            precache_owed: false,
            precache: None,
            shipped_sources: rustel_runtime::samples::SampleLibrary::default_sources()
                .iter()
                .map(|source| ShippedSource {
                    source: source.clone(),
                    sounds: 0,
                    files: 0,
                    cached_files: 0,
                    cache_bytes: 0,
                    counts_ready: false,
                    seen_files: HashSet::new(),
                    cache: None,
                })
                .collect(),
            user_sources: Vec::new(),
            source_measure: None,
            source_refresh: SourceRefresh::default(),
            cache_measure: None,
            cache_measure_due: None,
            cache_clear: None,
            trim_jobs: Vec::new(),
            caching_samples: 0,
            library_loading: false,
            prefs_changed_at: now,
            limiter_mode_due: None,
            manifest_changed_at: now,
            last_edit_at: now - LINT_DEBOUNCE,
            external_watch: None,
            external_watch_checked_at: now,
            reference_anchor: None,
            slider_tape_due: None,
            fader_tape_due: None,
            reported_take_error: None,
            pending_recording: None,
            sample_take: None,
            pending_sample: None,
            active_recording_id: None,
            last_finished_take: None,
            take_notice: None,
            toast: None,
            log,
            log_panel: None,
            jobs_panel: None,
            memory_dock: None,
            memory_unlaid: false,
            memory_closed_files: 0,
            set_panel: None,
            set_prompt: None,
            set_prompt_return_focus: None,
            viz_docks: [None, None],
            viz_focus: 0,
            viz_unlaid: [false; DOCKS],
            mixer_unlaid: false,
            mixer_panel: None,
            knobs_panel: None,
            knobs_unlaid: false,
            knobs_parse: None,
            knobs_rows: None,
            mixer_selection: None,
            pixel_budget: super::graphics::PIXEL_CEILING,
            pixel_budget_said: false,
            mapping_learn: None,
            keybind_learn: None,
            keybinds: prefs.keybinds(),
            vim_config: crate::vim_config::VimConfig::default(),
            vim_insert_pending: String::new(),
            remote_key_event: false,
            midi_cc_last: HashMap::new(),
            note_pressed_at: HashMap::new(),
            mapping_lit: [None; MAPPING_SLOTS],
            stick_run: [None; MAPPING_SLOTS],
            mapping_knob: [None; MAPPING_SLOTS],
            stick_ticked_at: now,
            stick_pushing: false,
            raised: std::collections::HashMap::new(),
            raise_counter: 0,
            replays: HashMap::new(),
            export_sheet: None,
            export_settings: ExportSettings::default(),
            export_job: None,
            last_file: None,
            last_file_status: None,
            last_recorded_sample: None,
            reveal_pending: None,
            theme_editor: None,
            pointer_shape_sent: None,
            pointer_shape_reset: false,
            pointer_shift_held: [false; 2],
            pointer_at: None,
            opacity_before_theme_editor: None,
            #[cfg(feature = "hydra")]
            theme_sketch_checked: None,
            #[cfg(feature = "hydra")]
            theme_camera_confirmed,
            theme_picker: None,
            prefs,
            rename_untouched: false,
            features: TerminalFeatures::default(),
            ui_settings: UiSettings::default(),
            applied_clock: (None, None),
            sample_memory_owed: None,
            midi_enabled: None,
            applied_midi_enabled: None,
            settings_sheet: None,
            settings_navigation: SettingsNavigation::default(),
            quiet_settled_source: None,
            next_launch: Launch::Now,
            next_rewind: false,
            visual_pin: None,
            armed_scene: None,
            armed_request: None,
            armed_revision: None,
            landing_name: None,
            readiness: HashMap::new(),
            readiness_polled_at: now,
            readiness_due: None,
            marks_muted: None,
            capabilities: KeyboardCapabilities::legacy(),
            piano: piano_mode::PianoMode::default(),
            visual_revision,
            visual_replay_block: None,
            evaluated_source,
            audible_scene,
            evaluation_revisions,
            installed_revision: None,
            inflight: HashMap::new(),
            update_failure: None,
            blocked_imports: Vec::new(),
            csound_allowed: csound_at_start,
            csound_refused: false,
            csound_owed: false,
            import_origins_owed: false,
            refused_update_owed: false,
            evaluation_alerts: HashMap::new(),
            lint_status: None,
            pending_evaluation: None,
            pending_slider: None,
            next_request_id: 1,
            snapshot: None,
            link_enabled: false,
            link_seen_playing: None,
            audio_advisory: audio_advisory::AudioAdvisory::default(),
            power_advisory: power_advisory::PowerAdvisory::default(),
            resting_output: None,
            input_lit_at: None,
            status: clipboard_status.into(),
            status_notice: None,
            errors: AppErrors::default(),
            next_save_request_id: 1,
            quit: false,
            started: now,
            dirty_frame: true,
            input_dirty: false,
            next_frame: now,
            caret_activity: now,
            caret_visible: true,
            evaluating_since: None,
            evaluation_flash: None,
            evaluation_feedback: None,
            focus_rotation_flash: None,
            performance: Performance::new(now, performance_events),
            #[cfg(feature = "hydra")]
            hydra_frames: None,
            #[cfg(feature = "hydra")]
            hydra_backdrop: None,
            #[cfg(feature = "hydra")]
            hydra_shelf: None,
            #[cfg(feature = "hydra")]
            hydra_theme: None,
            #[cfg(feature = "hydra")]
            hydra_input_policy: None,
            #[cfg(feature = "hydra")]
            theme_sketch_sent: None,
            theme_apply_due: None,
            #[cfg(feature = "hydra")]
            hydra_theme_last: None,
            #[cfg(feature = "hydra")]
            hydra_last: None,
            #[cfg(feature = "hydra")]
            hydra_shelf_last: None,
            #[cfg(feature = "hydra")]
            hydra_preview_sent: None,
            #[cfg(feature = "hydra")]
            hydra_frame_size_sent: None,
            engine_connected: true,
            master: MasterState::new(now),
            // No probe yet: the first `pump_turn` starts one, after the
            // studio has taken stderr, and a test App pins it first.
            devices: DeviceScanner::default(),
            panel: None,
            strip_mode: SceneStripMode::Idle,
            pads: PadListener::default(),
            pads_drained_at: now,
            launch_pads_sent: None,
            scene_visits: Vec::new(),
            tabs_closed: 0,
            live_material_sent: None,
            tabs_closed_sent: 0,
            plugins_shown_sent: Vec::new(),
            setups_select_variants_sent: false,
            sent_setup_selected_variants: false,
            mixer,
            input_meter: MasterState::new(now),
            orbits_in_code: Vec::new(),
            recorder: None,
            recorder_started: now,
            process: ProcessMonitor::new(now),
            #[cfg(feature = "harness")]
            pin_frame_inputs: false,
            frame: Rect::default(),
            regions: StudioRegions::default(),
            pointer: None,
            pointer_subcell: 0.0,
            pointer_subrow: 0.0,
            last_pty_cell_pixels: None,
            focus: Focus::Editor,
            help: None,
            quit_armed: false,
            pending_switch: None,
            menu: None,
            clicks: super::textblock::ClickCounter::default(),
            armed_slider: None,
            slider_precision: None,
            smart_action: None,
            replay_edit: None,
            preview_gain: 1.0,
            mixer_fader: 0,
            mixer_on_faders: false,
            preview_armed: None,
            preview_lit_until: None,
            preview_shape: None,
            sound_seconds: std::cell::Cell::new(0.0),
            sound_counted_at: std::cell::Cell::new(Instant::now()),
            #[cfg(feature = "hydra")]
            snippet_preview: None,
            #[cfg(feature = "hydra")]
            generator_preview_pending: false,
            #[cfg(feature = "hydra")]
            generator_preview_epoch: 0,
            #[cfg(feature = "hydra")]
            generator_feedback_since: None,
            #[cfg(feature = "hydra")]
            generator_last_preview: None,
            #[cfg(feature = "hydra")]
            snippet_heard: false,
            loading_since: None,
            loading_due: (false, false),
            #[cfg(feature = "hydra")]
            warmed_snippet: None,
            #[cfg(feature = "hydra")]
            snippet_under_cursor: None,
            stop_requested: false,
            marks: Vec::new(),
            #[cfg(feature = "hydra")]
            snippet_marks: Vec::new(),
            mini_spans: Vec::new(),
            sliders: Vec::new(),
            sliders_revision: None,
            live_sliders: HashMap::new(),
        };
        // The reference leaves out what the settings hide from the first
        // frame, before a terminal is adopted.
        app.reference
            .set_hidden(app.ui_settings.hidden_categories());
        // The set open now is the one a bare launch opens next time, and
        // the newest of the recent ones.
        app.remember_current_set();
        // The set's own folder is a sample source from the first frame. The
        // constructor adopts it, not the run loop: a launch that reaches this
        // point has a set and a library, and every other path that changes
        // the set adopts the folder where the set changes.
        // The renames overlay every imported layer, so they are set before
        // the first adoption. The set's own folder uses them too: adopting
        // it first would show its renamed banks under their old names until
        // the set is opened again.
        if let Some(library) = app.worker.library() {
            library.set_bank_renames(app.prefs.bank_renames());
            library.set_source_bank_renames(app.prefs.source_bank_renames());
        }
        app.adopt_set_samples();
        // The player's own imported sources, which every set sees. Before
        // the first frame for the same reason the set's folder is: a sound
        // that is only there after you open Settings once is a sound the
        // browser was wrong about.
        app.forget_sources_in_gone_sets();
        app.adopt_global_sources();
        app.refresh_catalogue();
        #[cfg(feature = "vst")]
        {
            app.adopt_vst_folders();
            app.adopt_preset_folders();
        }
        // Laid out the way the set was left: the same scores in the same
        // panes, with the caret in the pane that had it.
        app.restore_panes();
        // And at the levels it was left at, rather than at unity.
        app.restore_set_levels();
        app.rebuild_slider_spans();
        Ok(app)
    }

    fn editor(&self) -> &Editor {
        &self.scenes.current().editor
    }

    /// The caret's line and column, 1-based, the way vim's ruler reads
    /// them. The gutter can be turned off in the UI, and the column it
    /// never shows - a caret past a wide line is otherwise a guess.
    fn caret_position(&self) -> Option<(usize, usize)> {
        let editor = self.editor();
        let head = editor.primary_selection().head;
        let document = editor.document();
        let line = document.line_of(head).ok()?;
        let column = document
            .slice(document.line_start(line)..head)
            .map(|text| text.chars().count())
            .unwrap_or(0);
        Some((line + 1, column + 1))
    }

    fn editor_mut(&mut self) -> &mut Editor {
        &mut self.scenes.current_mut().editor
    }

    fn editor_accepts_raw_paste(&self) -> bool {
        #[cfg(feature = "remote-control")]
        if self.remote_panel_open {
            return false;
        }
        // A paste batches into the document exactly when the document
        // receives the typed text. An open panel does not prevent this. A
        // focused panel does, because the paste belongs to its search box.
        !self.piano.open
            && self.help.is_none()
            && self.slider_precision.is_none()
            && self.replay_edit.is_none()
            && self.smart_action.is_none()
            // While a dropdown is open, its mnemonics are unmodified letters.
            // A quick run of them looks like a paste, and batching would
            // insert them into the document as text instead of choosing a
            // menu item.
            && self.menu.is_none()
            && matches!(self.strip_mode, SceneStripMode::Idle)
            && self.focus == Focus::Editor
    }

    fn audible_editor(&self) -> Option<&Editor> {
        self.audible_scene
            .and_then(|id| self.scenes.get(id))
            .map(|scene| &scene.editor)
    }

    /// Say that something just happened, briefly and where the eye is.
    ///
    /// Not the status line: a confirmation competes there with whatever the
    /// studio was already saying, and what was confirmed has usually just made
    /// a panel disappear.
    fn toast(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), Instant::now()));
        self.dirty_frame = true;
    }

    fn remember_status_file(&mut self, path: PathBuf) {
        self.last_file = Some(path);
        self.last_file_status = Some(self.status.clone());
    }

    /// ⌘⇧O / Ctrl+Shift+O, or a click on the status line: show the last
    /// file the studio made in the desktop's file manager.
    fn reveal_last_file(&mut self) {
        match self.last_file.clone() {
            Some(path) => self.reveal_target(RevealTarget::File(path)),
            None => {
                self.status = "nothing exported or recorded yet".into();
                self.dirty_frame = true;
            }
        }
    }

    /// Ask the desktop; without one, the clipboard gets the address instead.
    /// An address the reveal refuses gets neither, and the status says why.
    fn reveal_target(&mut self, target: RevealTarget) {
        let started = reveal(target.clone());
        self.report_reveal(target, started);
    }

    /// What the studio says, and does, once a reveal has started or not.
    fn report_reveal(&mut self, target: RevealTarget, started: Result<Reveal, RevealError>) {
        match started {
            Ok(pending) => {
                self.status = format!("opening {}…", self.reveal_status_name(&target));
                self.reveal_pending = Some(pending);
            }
            Err(RevealError::Refused(why)) => self.reveal_refused(&why),
            Err(RevealError::NoDesktop(error)) => {
                self.reveal_fallback(&target, &error.to_string());
            }
        }
        self.dirty_frame = true;
    }

    /// An address that is not a web page's is not the desktop's to open,
    /// and it does not go to the clipboard either, where the next paste
    /// would hand it on: the status line says what is wrong with it.
    fn reveal_refused(&mut self, why: &str) {
        self.status = format!("not opened: {why}");
        self.log.push(LogLevel::Warn, "reveal", why.to_owned());
    }

    fn reveal_status_name(&self, target: &RevealTarget) -> String {
        match target {
            RevealTarget::File(path) | RevealTarget::Folder(path) => {
                status_file_path(path, self.ui_settings.show_full_paths)
            }
            RevealTarget::Url(url) => url.clone(),
        }
    }

    fn reveal_fallback(&mut self, target: &RevealTarget, why: &str) {
        let text = target.text();
        let copied = self.clipboard.set_text(text.clone()).is_ok();
        let name = self.reveal_status_name(target);
        self.status = if copied {
            format!("no desktop to open it here - copied {name} to the clipboard")
        } else {
            format!("no desktop to open it here - {name}")
        };
        self.log
            .push(LogLevel::Warn, "reveal", format!("{text}: {why}"));
        self.dirty_frame = true;
    }

    fn poll_reveal(&mut self) {
        let Some(pending) = self.reveal_pending.as_ref() else {
            return;
        };
        let Some(outcome) = pending.poll() else {
            return;
        };
        let pending = self.reveal_pending.take().expect("checked");
        match outcome {
            Ok(()) => {
                self.status = format!("opened {}", self.reveal_status_name(&pending.target));
                self.dirty_frame = true;
            }
            Err(why) => self.reveal_fallback(&pending.target, &why),
        }
    }

    /// The panels come back the way they were left: the set panel, the
    /// visualisation docks and the mixer desk reopen keyless, exactly as
    /// the previous studio left them. The CLI startup and the test
    /// harness share this so the two cannot drift apart.
    pub(super) fn restore_panels_from_prefs(&mut self) {
        if self.prefs.set_panel == Some(true) {
            self.open_set_panel(false);
        }
        for index in 0..crate::viz_panel::DOCKS {
            if self.prefs.visuals[index].open {
                self.open_viz_dock(index, false);
            }
        }
        if self.prefs.mixer_panel == Some(true) {
            self.open_mixer_panel(false);
        }
        if self.prefs.knobs_panel == Some(true) {
            self.open_knobs_panel(false);
        }
        self.restore_sticky_log();
    }

    /// Every text the studio had open, for a crash report: the scores as
    /// they stood - including edits never updated, which is often exactly
    /// what broke - and the setups that ran before them.
    fn open_sources(&self) -> Vec<crash::CrashSource> {
        let mut sources = Vec::new();
        for scope in PrebakeScope::ALL {
            let text = self.stored_prebake(scope);
            if !prebake::is_blank(text) {
                sources.push(crash::CrashSource {
                    name: scope.tab_name(),
                    text: text.to_owned(),
                    audible: false,
                });
            }
        }
        // The same rule the strip's chips use: the layout's scene is only
        // the sounding one while something is actually sounding.
        let playing = self.is_playing().then_some(self.audible_scene).flatten();
        for scene in self.scenes.scenes() {
            if scene.prebake().is_some() {
                // Already listed above, from the store rather than the tab.
                continue;
            }
            sources.push(crash::CrashSource {
                name: scene.name(),
                text: scene.editor.source(),
                audible: playing == Some(scene.id),
            });
        }
        sources
    }

    /// The run so far, as the log saw it: refusals, updates, devices, the
    /// setups that applied. The log keeps a ring of these in memory, so a
    /// crash report can carry the story without reading anything back.
    fn what_happened_first(&self) -> Vec<crash::CrashMoment> {
        self.log
            .lines()
            .iter()
            .map(|line| crash::CrashMoment {
                at: line.at,
                level: line.level.label(),
                kind: line.kind.clone(),
                text: line.text.clone(),
            })
            .collect()
    }

    /// The tape this set is recording, when it is recording one. Every
    /// evaluate is on it, so it replays the whole way here.
    fn recorded_tape(&self) -> Option<crash::CrashTape> {
        self.recorder.as_ref().map(|recorder| crash::CrashTape {
            path: recorder.path().to_path_buf(),
            saves: recorder.saves(),
        })
    }

    pub(super) fn layout_with_fixtures(
        &self,
        mixer: Option<super::viz_panel::Dock>,
        log: Option<super::viz_panel::Dock>,
        memory: Option<super::viz_panel::Dock>,
    ) -> StudioRegions {
        view::regions_with_footer(
            self.frame,
            view::ChromeLayout::from_settings(&self.ui_settings),
            self.panes.len(),
            self.reference_panel.is_some(),
            self.ui_settings.minimap,
            self.set_sidebar(),
            self.dock_requests(),
            mixer,
            log,
            memory,
            self.knobs_panel.is_some(),
            self.footer_notice_rows(),
            self.has_piano_notes(),
        )
    }

    pub(super) fn log_dock_request(&self) -> Option<super::viz_panel::Dock> {
        self.log_panel
            .as_ref()
            .and_then(|panel| view::log_dock(panel, self.frame))
    }

    pub(super) fn memory_dock_request(&self) -> Option<super::viz_panel::Dock> {
        self.memory_dock.map(|dock| dock.dock(self.frame))
    }

    fn send_sample_memory(&mut self) {
        let Some((budget_bytes, idle)) = self.sample_memory_owed else {
            return;
        };
        if self.pending_evaluation.is_some() || !self.prebake_queue.is_empty() {
            return;
        }
        if self.worker.try_set_sample_memory(budget_bytes, idle) {
            self.sample_memory_owed = None;
        }
    }

    fn note_scene_visit(&mut self, scene: SceneId) {
        self.scene_visits.retain(|visited| *visited != scene);
        self.scene_visits.insert(0, scene);
    }

    fn forget_scene(&mut self, id: SceneId) {
        self.scene_visits.retain(|visited| *visited != id);
        self.lint.remove(&id);
        self.live_sliders.remove(&id);
        self.readiness.remove(&id);
        self.tabs_closed += 1;
    }

    fn live_tabs(&self) -> Vec<(SceneId, bool)> {
        let pinned = |scene: &super::scenes::Scene| {
            self.panes.iter().any(|pane| pane.scene == scene.id)
                || scene.pad.is_some()
                || self.audible_scene == Some(scene.id)
                || scene.prebake().is_some()
        };
        let scores = self
            .scenes
            .scenes()
            .iter()
            .filter(|scene| !scene.is_replay());
        let mut tabs: Vec<(SceneId, bool)> = scores
            .clone()
            .filter(|scene| pinned(scene))
            .map(|scene| (scene.id, true))
            .collect();
        let mut rest: Vec<SceneId> = scores
            .filter(|scene| !pinned(scene))
            .map(|scene| scene.id)
            .collect();
        // Visited most recently first; never visited this session last, in
        // tab order.
        rest.sort_by_key(|scene| {
            self.scene_visits
                .iter()
                .position(|visited| visited == scene)
                .unwrap_or(usize::MAX)
        });
        tabs.extend(rest.into_iter().map(|scene| (scene, false)));
        tabs
    }

    fn sync_live_material(&mut self, now: Instant) {
        let tabs = self.live_tabs();
        let key: Vec<(SceneId, LiveTextMark, bool)> = tabs
            .iter()
            .filter_map(|(id, pinned)| {
                let scene = self.scenes.get(*id)?;
                Some((*id, LiveTextMark::of(scene), *pinned))
            })
            .collect();
        #[cfg(feature = "vst")]
        let plugins = self.plugins_shown();
        #[cfg(not(feature = "vst"))]
        let plugins = Vec::new();
        if (self.live_material_sent.as_ref() == Some(&key)
            && self.tabs_closed_sent == self.tabs_closed
            && self.plugins_shown_sent == plugins)
            || now.duration_since(self.last_edit_at) < LINT_DEBOUNCE
        {
            return;
        }
        // The engine's queue is two deep and shared with evaluations: a
        // press on a pad must never wait a turn behind this.
        if self.pending_evaluation.is_some() || !self.prebake_queue.is_empty() {
            return;
        }
        // Loading ahead needs the library; until it is there, nothing is
        // sent, so the first set sent is also the first loaded.
        let Some(library) = self.worker.library() else {
            return;
        };
        let text = |id: SceneId| -> Option<Arc<str>> {
            Some(Arc::from(self.scenes.get(id)?.editor.source()))
        };
        let setups_select_variants = self.setups_select_variants();
        let material = LiveMaterial {
            pinned: key
                .iter()
                .filter(|(_, _, pinned)| *pinned)
                .filter_map(|(id, _, _)| text(*id))
                .collect(),
            recent: key
                .iter()
                .filter(|(_, _, pinned)| !*pinned)
                .filter_map(|(id, _, _)| text(*id))
                .collect(),
            tabs_closed: self.tabs_closed,
            setups_select_variants,
            plugins: plugins.clone(),
        };
        if !self.worker.try_set_live_material(material.clone()) {
            // The queue is full: the next turn asks again.
            return;
        }
        // Load ahead what is always kept and has changed since it was last
        // sent: a tab newly behind a pad or in a pane, or edited - or every
        // one of them, when a setup has just started picking variants.
        let before = self.live_material_sent.take().unwrap_or_default();
        let widened = setups_select_variants && !self.setups_select_variants_sent;
        let mut ahead = Vec::new();
        for ((id, mark, _), source) in key
            .iter()
            .filter(|(_, _, pinned)| *pinned)
            .zip(material.pinned.iter())
        {
            if widened || !before.contains(&(*id, *mark, true)) {
                // A tab never opened this evening has had its imports
                // looked up by nobody; without them its names are unknown.
                self.look_up_imports(*id);
                ahead.push(Arc::clone(source));
            }
        }
        load_variants_ahead(&ahead, setups_select_variants, &library);
        self.live_material_sent = Some(key);
        self.tabs_closed_sent = self.tabs_closed;
        self.plugins_shown_sent = plugins;
        self.setups_select_variants_sent = setups_select_variants;
    }

    fn setups_select_variants(&self) -> bool {
        self.sent_setup_selected_variants
            || self
                .scenes
                .scenes()
                .iter()
                .filter(|scene| scene.prebake().is_some())
                .any(|scene| {
                    rustel_runtime::sounds::variant_selection(&scene.editor.source()).is_some()
                })
            || PrebakeScope::ALL.iter().any(|scope| {
                rustel_runtime::sounds::variant_selection(self.stored_prebake(*scope)).is_some()
            })
    }
}

#[cfg(test)]
mod tests {
    mod status_path {
        //! Tests for status path.

        use super::super::*;

        #[test]
        fn successful_save_hides_parent_folders_unless_enabled() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = super::support::app_over(directory.path());
            let path = app.scenes.current().path.clone();
            assert!(!app.ui_settings.show_full_paths);
            for full in [false, true] {
                app.ui_settings.show_full_paths = full;
                app.handle_save_result(SaveResult {
                    request_id: 42,
                    path: path.clone(),
                    editor_revision: app.editor().revision(),
                    source_revision: source_revision(&app.editor().source()),
                    result: Ok(true),
                });
                let expected = if full {
                    format!("saved {}", path.display())
                } else {
                    "saved first.strudel".to_owned()
                };
                assert_eq!(app.status, expected);
                let deadline = app.status_notice.as_ref().expect("save notice").1;
                app.expire_notice(deadline - Duration::from_millis(1));
                assert_eq!(app.status, expected);
                app.expire_notice(deadline);
                assert!(app.status.is_empty());
            }
        }

        #[test]
        fn shortened_export_status_keeps_the_actual_reveal_target() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = super::support::app_over(directory.path());
            let target = directory.path().join("take.wav");
            app.status = format!("exported scene - {}", status_file_path(&target, false));
            app.remember_status_file(target.clone());
            assert!(app.status_names_last_file());
            assert_eq!(app.last_file.as_ref(), Some(&target));
            let reveal = RevealTarget::File(target.clone());
            assert_eq!(app.reveal_status_name(&reveal), "take.wav");
            assert_eq!(
                reveal.text(),
                target.display().to_string(),
                "clipboard and file-manager actions keep the real address"
            );
            app.status = "saved another-take.wav".into();
            assert!(
                !app.status_names_last_file(),
                "a filename substring cannot activate an old file link"
            );
            app.ui_settings.show_full_paths = true;
            assert_eq!(
                app.reveal_status_name(&reveal),
                target.display().to_string()
            );
        }

        #[test]
        fn sample_source_labels_preserve_remote_names_and_hide_local_parent_folders() {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("drums");
            let plain = path.display().to_string();
            for spec in [&plain, &format!("local:{plain}")] {
                assert_eq!(status_sample_source(spec, false), "drums");
                assert_eq!(status_sample_source(spec, true), *spec);
                let reason = format!("cannot read {}: permission denied", path.display());
                assert!(!status_sample_source_error(spec, &reason, false).contains(&plain));
                assert!(status_sample_source_error(spec, &reason, true).contains(&plain));
            }
            for spec in [
                "https://example.test/samples/strudel.json",
                "github:artist/drums",
                "shabda:birds",
            ] {
                assert_eq!(status_sample_source(spec, false), spec);
                assert_eq!(status_sample_source(spec, true), spec);
            }
        }

        #[test]
        fn sample_import_refresh_toggle_remove_and_drop_statuses_follow_path_privacy() {
            for full in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let mut app = super::support::app_over(directory.path());
                app.ui_settings.show_full_paths = full;
                app.ui_settings.precache_sources = false;
                let folder = directory.path().join("my drums");
                std::fs::create_dir(&folder).unwrap();
                // Folder catalogue discovery only needs a supported audio filename.
                std::fs::write(folder.join("kick.wav"), []).unwrap();
                let spec = folder.display().to_string();
                let label = status_sample_source(&spec, full);
                app.add_sample_source(&spec);
                // The folder is walked off the studio's thread, so the receipt
                // usually arrives with the poll rather than with the keypress. A
                // folder this small can be walked before `adopt_global_sources`
                // returns its reports, though, and then the keypress already holds
                // the receipt. Both are honest; a status naming neither is not.
                assert!(
                    [
                        format!("{label} - fetching its list of sounds"),
                        format!("{label} - 1 sound(s) imported"),
                    ]
                    .contains(&app.status),
                    "the import said {:?}, which names neither the walk nor its receipt",
                    app.status
                );
                // The walk is queued behind whatever the library is still loading,
                // so one settle is a wish under load: keep settling until the
                // receipt lands (it always does; the walk only waits its turn).
                for _ in 0..30 {
                    if app.status == format!("{label} - 1 sound(s) imported") {
                        break;
                    }
                    app.settle_sample_sources();
                }
                assert_eq!(app.status, format!("{label} - 1 sound(s) imported"));
                assert_eq!(app.prefs.sample_sources[0].spec, spec);
                app.add_sample_source(&spec);
                for _ in 0..30 {
                    if app.status == format!("{label} - 1 sound(s) imported") {
                        break;
                    }
                    app.settle_sample_sources();
                }
                assert_eq!(
                    app.status,
                    format!("{label} - 1 sound(s) imported"),
                    "adding the same source is its quick refresh gesture"
                );
                app.refresh_sample_source(0);
                assert!(
                    [
                        format!("{label} - fetching again"),
                        format!("{label} - 1 sound(s) imported"),
                    ]
                    .contains(&app.status),
                    "refresh reports its pending scan or completed receipt: {:?}",
                    app.status
                );
                app.settle_sample_sources();
                assert_eq!(app.status, format!("{label} - 1 sound(s) imported"));

                let mut sheet = SettingsSheet::default();
                sheet.show_page(settings::SettingsPage::Sources);
                // The page's cache controls sit above the imports now; selecting
                // the row this import owns is what puts Space and `d` on it.
                sheet.selected = settings::SOURCE_CONTROL_COUNT;
                app.settings_sheet = Some(sheet);
                app.focus = Focus::Panel(PanelKind::Settings);
                assert!(app.handle_settings_key(KeyCode::Char(' '), false, false));
                assert_eq!(app.status, format!("{label} off"));
                app.refresh_sample_source(0);
                assert_eq!(app.status, format!("{label} is off - Space turns it on"));
                assert!(app.handle_settings_key(KeyCode::Char('d'), false, false));
                assert_eq!(app.status, format!("{label} removed"));
                assert!(app.prefs.sample_sources.is_empty());

                let path = app.scenes.current().path.clone();
                app.open_dropped_score(&path);
                assert_eq!(
                    app.status,
                    format!("{} is already open", status_file_path(&path, full))
                );
                let missing = directory.path().join("missing");
                app.add_sample_source(&missing.display().to_string());
                assert_eq!(
                    app.status.contains(&directory.path().display().to_string()),
                    full
                );
            }
        }

        /// Fetch imports caches the imported sounds the score names, not every
        /// file of every import. A local folder is already here, so turning the
        /// switch on queues nothing and does not put a count on the row.
        #[test]
        fn fetch_imports_caches_the_score_not_the_whole_import() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = super::support::app_over(directory.path());
            app.ui_settings.precache_sources = false;
            let folder = directory.path().join("my drums");
            std::fs::create_dir(&folder).unwrap();
            std::fs::write(folder.join("kick.wav"), []).unwrap();
            std::fs::write(folder.join("snare.wav"), []).unwrap();
            app.add_sample_source(&folder.display().to_string());
            app.settle_sample_sources();
            app.precache_sources();
            assert_eq!(
                app.precache, None,
                "fetch imports does not put a count on its row"
            );
            app.ui_settings.precache_sources = true;
            app.precache_sources();
            assert_eq!(app.precache, None);
        }

        /// A minimal WAV: a header and two silent frames, enough for a folder scan
        /// to take it for a sample.
        fn tiny_wav() -> Vec<u8> {
            let data: [u8; 4] = [0; 4];
            let mut bytes = Vec::new();
            bytes.extend_from_slice(b"RIFF");
            bytes.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
            bytes.extend_from_slice(b"WAVEfmt ");
            bytes.extend_from_slice(&16u32.to_le_bytes());
            bytes.extend_from_slice(&1u16.to_le_bytes());
            bytes.extend_from_slice(&1u16.to_le_bytes());
            bytes.extend_from_slice(&8000u32.to_le_bytes());
            bytes.extend_from_slice(&16000u32.to_le_bytes());
            bytes.extend_from_slice(&2u16.to_le_bytes());
            bytes.extend_from_slice(&16u16.to_le_bytes());
            bytes.extend_from_slice(b"data");
            bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
            bytes.extend_from_slice(&data);
            bytes
        }

        /// Showing a sample from the browser goes to that sample's own file, a
        /// space in its folder's name and all - not to its bank's first file, and
        /// not to an address the desktop cannot open.
        #[test]
        fn showing_a_browser_sample_finds_its_own_file() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = super::support::app_over(directory.path());
            let samples = directory.path().join("sample root");
            let kicks = samples.join("deep kicks");
            std::fs::create_dir_all(&kicks).unwrap();
            for name in ["a.wav", "b.wav"] {
                std::fs::write(kicks.join(name), tiny_wav()).unwrap();
            }
            let library = rustel_runtime::samples::SampleLibrary::empty();
            library
                .adopt_set_folder(&samples)
                .expect("adopt the folder");
            app.worker.set_library(Arc::new(library));

            let wanted = kicks.join("b.wav").canonicalize().unwrap();
            match app.sound_reveal_target("deep kicks", Some(1), "file:///listed/first.wav") {
                Ok(RevealTarget::File(path)) => {
                    assert_eq!(
                        path.canonicalize().unwrap(),
                        wanted,
                        "the sample's own file"
                    );
                }
                other => panic!("expected the sample's file, got {other:?}"),
            }
            match app.sound_reveal_target("deep kicks", None, "file:///listed/first.wav") {
                Ok(RevealTarget::File(path)) => assert_eq!(
                    path.canonicalize().unwrap(),
                    kicks.join("a.wav").canonicalize().unwrap(),
                    "a bank row shows the bank's first file"
                ),
                other => panic!("expected the bank's file, got {other:?}"),
            }
            // A name the library has no file for keeps the address it was listed
            // with, and says so when that is nowhere either.
            assert_eq!(
                app.sound_reveal_target("gm_piano", None, "file:///nowhere/at/all.wav"),
                Err("nowhere to open for file:///nowhere/at/all.wav".to_owned())
            );
        }

        /// A reveal of an address that the studio refuses to open reports the
        /// reason, not a missing desktop. The clipboard keeps its content, so the
        /// next paste does not carry that address.
        #[test]
        fn a_refused_reveal_says_why_and_leaves_the_clipboard_alone() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = super::support::app_over(directory.path());
            app.clipboard.set_text("what was there".into()).unwrap();

            app.reveal_target(RevealTarget::Url("javascript:alert(1)".into()));
            assert!(app.status.starts_with("not opened: "), "{}", app.status);
            assert!(
                app.status.contains("a javascript: address"),
                "{}",
                app.status
            );
            assert!(!app.status.contains("no desktop"), "{}", app.status);
            assert!(app.reveal_pending.is_none(), "nothing was started");
            assert_eq!(app.clipboard.get_text().unwrap(), "what was there");
        }

        /// With no desktop to ask - the launcher is missing, as over SSH - the
        /// address goes to the clipboard instead, and the status says so.
        #[test]
        fn a_reveal_with_no_desktop_copies_the_address_instead() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = super::support::app_over(directory.path());
            let target = RevealTarget::Url("https://example.com/kits/909/".into());

            app.report_reveal(
                target,
                Err(RevealError::NoDesktop(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "no xdg-open here",
                ))),
            );
            assert_eq!(
                app.status,
                "no desktop to open it here - copied https://example.com/kits/909/ to the clipboard"
            );
            assert_eq!(
                app.clipboard.get_text().unwrap(),
                "https://example.com/kits/909/"
            );
            assert!(app.reveal_pending.is_none());
        }
    }

    pub(in crate::app) mod support {
        //! Fixtures shared by the App's tests: building an App over a folder,
        //! settling it, pressing keys and typing text.

        use super::super::*;
        use crate::editor::{GridRect, MemoryClipboard, Selection};
        use rustel_runtime::samples::{SoundCategory, SoundEntry, SoundOrigin};

        pub(in crate::app) const SOURCE: &str = "$: s(\"bd\").gain(slider(0.5, 0, 1, 0.0001))\n";

        pub(in crate::app) fn slider_app(directory: &std::path::Path) -> (App, SceneId, usize) {
            slider_app_with_source(directory, SOURCE)
        }

        /// An app over a score with no slider in it yet - what `^J` starts from.
        pub(in crate::app) fn plain_app(
            directory: &std::path::Path,
            source: &str,
        ) -> (App, SceneId) {
            let (app, scene, _) = build_app(directory, source, None);
            (app, scene)
        }

        pub(in crate::app) fn slider_app_with_source(
            directory: &std::path::Path,
            source: &str,
        ) -> (App, SceneId, usize) {
            let (app, scene, call) = build_app(directory, source, Some("slider("));
            (app, scene, call.expect("slider call"))
        }

        pub(in crate::app) fn build_app(
            directory: &std::path::Path,
            source: &str,
            marker: Option<&str>,
        ) -> (App, SceneId, Option<usize>) {
            let mut app = app_over(directory);
            app.editor_mut()
                .dispatch(
                    Command::PasteText(source.into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed the score");
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(0)))
                .expect("keep the start visible");
            let scene = app.scenes.current().id;
            app.live_sliders.insert(
                scene,
                LiveSliderSet {
                    revision: app.editor().revision(),
                    sliders: rustel_runtime::ui_events::literal_sliders(&app.editor().source()),
                },
            );
            app.features.pixel_mouse = false;
            app.frame = Rect::new(0, 0, 120, 30);
            app.regions = view::regions(app.frame, false, 1, false, false, None, None);
            (app, scene, marker.and_then(|marker| source.find(marker)))
        }

        pub(in crate::app) fn map_on(
            app: &mut App,
            scene: SceneId,
            grid: GridRect,
            wrap: bool,
            left: usize,
        ) {
            let widths = app.slider_pill_widths(scene);
            let editor = &mut app.scenes.get_mut(scene).expect("slider scene").editor;
            editor
                .set_selection(Selection::caret(ByteOffset(0)))
                .expect("keep the first row visible");
            editor.set_inline_widths(widths);
            editor.set_wrap(wrap);
            editor.set_view_size(usize::from(grid.width), usize::from(grid.height));
            let mut viewport = editor.viewport();
            viewport.top_row = 0;
            viewport.left_column = left;
            editor.set_viewport(viewport);
            let map = editor.screen_map(grid).expect("screen map");
            app.panes
                .iter_mut()
                .find(|pane| pane.scene == scene)
                .expect("slider pane")
                .last_map = Some(map);
        }

        pub(in crate::app) fn lay_out(
            app: &mut App,
            scene: SceneId,
            call: usize,
        ) -> (u16, u16, u16) {
            let pane = app
                .panes
                .iter()
                .position(|pane| pane.scene == scene)
                .expect("slider pane");
            let area = app.regions.panes[pane].editor;
            let grid = view::source_grid(
                &app.scenes.get(scene).expect("slider scene").editor,
                area,
                true,
            );
            map_on(app, scene, grid, false, 0);
            let chip = app.live_chip_at(scene, call).expect("live slider");
            let (x, width) = app.slider_pill_track(scene, &chip).expect("visible track");
            let row = app.panes[pane]
                .last_map
                .as_ref()
                .unwrap()
                .cell_for_offset(ByteOffset(call))
                .expect("visible call")
                .y;
            (x, width, row)
        }

        /// Use the same conversion and routing order as the terminal event loop.
        pub(in crate::app) fn mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
            let mut event = Event::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            });
            app.pointer_to_cells(&mut event);
            app.handle_terminal_event(event).expect("mouse handled");
        }

        pub(in crate::app) fn value(app: &App, scene: SceneId, call: usize) -> f64 {
            let chip = app
                .live_chip_at(scene, call)
                .expect("slider survives its value edits")
                .span;
            let source = app.scenes.get(scene).expect("slider scene").editor.source();
            let literal = source[chip.from..chip.to]
                .parse::<f64>()
                .expect("the current value is a numeric literal");
            assert_eq!(
                literal, chip.value,
                "source and live value agree after every pointer report"
            );
            chip.value
        }

        pub(in crate::app) fn assert_value(app: &App, scene: SceneId, call: usize, expected: f64) {
            let actual = value(app, scene, call);
            assert!(
                (actual - expected).abs() <= 0.000_050_001,
                "expected {expected} to the slider's step, got {actual}"
            );
        }

        pub(in crate::app) fn precision_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
            app.handle_terminal_event(Event::Key(crossterm::event::KeyEvent::new(code, modifiers)))
                .expect("precision key handled");
        }

        /// Open every heading and land the samples-tab cursor on the bank
        /// named `name`, wherever the real catalogue - synths and all - files
        /// it: a fixture asking for one bank by name should not have to know
        /// how many others the library happens to carry alongside it.
        pub(in crate::app) fn select_bank_row(panel: &mut ReferencePanel, name: &str) {
            for (section, _) in panel.visible_categories() {
                panel.open_categories.insert(section);
            }
            let row = panel
                .sound_rows()
                .iter()
                .position(
                    |row| matches!(row, SoundRow::Bank(index) if panel.sounds[*index].name == name),
                )
                .unwrap_or_else(|| panic!("{name} is not listed"));
            panel.sound_selected = row;
        }

        /// A tape in the sessions folder, for the panel to find.
        pub(in crate::app) fn write_tape(
            directory: &std::path::Path,
            name: &str,
            saves: &[(f64, &str)],
        ) -> PathBuf {
            use std::io::Write;
            let path = directory.join(format!("session-{name}.rustel-session"));
            let mut file = std::fs::File::create(&path).expect("tape");
            writeln!(
                file,
                r#"{{"keeps":"installed-saves","recorded":"2026-09-05T12:09:48Z","version":2}}"#
            )
            .unwrap();
            for (at, source) in saves {
                let encoded = rustel_runtime::session_log::encode_base64(source.as_bytes());
                writeln!(
                    file,
                    r#"{{"kind":"save","source":"{encoded}","status":"installed","t":{at}}}"#
                )
                .unwrap();
            }
            path
        }

        pub(in crate::app) fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
            app.handle_terminal_event(Event::Key(crossterm::event::KeyEvent::new(code, modifiers)))
                .expect("key handled");
        }

        pub(in crate::app) fn bank(name: &str, import: &str) -> SoundEntry {
            SoundEntry {
                name: name.into(),
                variants: 1,
                variant_names: Vec::new(),
                origin: SoundOrigin::Global,
                category: SoundCategory::Mine,
                location: None,
                import: Some(import.into()),
            }
        }

        /// A studio over a temp folder holding one scene, with its global
        /// prebake pointed at that folder so a test never reads, or writes,
        /// the config folder of whoever is running it.
        pub(in crate::app) fn app_over(directory: &std::path::Path) -> App {
            // Before anything reads a preference or opens a log: a test must not
            // touch the real one. See `config::isolate_for_tests`.
            super::super::super::config::isolate_for_tests();
            std::fs::write(directory.join("first.strudel"), "$: s(\"bd\")\n").expect("scene file");
            let scenes = SceneSet::open(directory, "first").expect("scenes");
            let options = StudioOptions {
                path: Some(directory.to_path_buf()),
                mini: false,
                session: SessionConfig::default(),
                theme: None,
                output: Some("silent".to_owned()),
                input: None,
                cancellation: None,
                recording: None,
                performance_events: false,
                // Use a memory clipboard. Another process can hold the platform
                // clipboard on a runner, and the copy tests then fail.
                memory_clipboard: true,
                ..StudioOptions::default()
            };
            let theme = Theme::resolve(None).expect("theme");
            let worker = StudioWorker::spawn(
                StudioConfig {
                    session: options.session.clone(),
                    output: options.output.clone(),
                    default_samples: options.default_samples,
                    // No runner's microphone either: with no input chosen, ^H
                    // would open the host's default input. Refused instead, as a
                    // machine with no input refuses it.
                    open_default_input: false,
                    ..Default::default()
                },
                #[cfg(feature = "hydra")]
                rustel_runtime::hydra::HydraBridge::new(),
            )
            .expect("worker");
            let mut app = App::new(options, theme, scenes, worker).expect("app");
            // The host with nothing attached, as the harness pins it: no test
            // App enumerates the runner's audio or MIDI hardware.
            app.devices
                .pin(crate::devices::DeviceInventory::no_hardware());
            // Nor its plugins: with no standard plugin folder, a vst tab
            // lists and loads no plugin of the machine.
            #[cfg(feature = "vst")]
            rustel_runtime::vst::pin_standard_folders(Vec::new());
            // A folder of its own, as the real one has: the config folder is
            // never the set's folder, and a prebake.strudel among the scores
            // would be read as one.
            app.global_prebake_path = Some(directory.join("config").join("prebake.strudel"));
            app
        }

        /// Drain what the engine has answered so far, without waiting forever
        /// on an engine that answered nothing.
        pub(in crate::app) fn settle(app: &mut App) {
            for _ in 0..200 {
                app.flush_pending_prebake();
                app.flush_pending_evaluation();
                let mut had = false;
                while let Ok(event) = app.worker.try_recv_control() {
                    app.handle_control(event);
                    had = true;
                }
                if !had && app.prebake_inflight.is_none() && app.prebake_queue.is_empty() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }

        pub(in crate::app) fn type_text(app: &mut App, text: &str) {
            for character in text.chars() {
                press(app, KeyCode::Char(character), KeyModifiers::NONE);
            }
        }

        /// A snapshot describing a studio that is playing, or stopping.
        pub(in crate::app) fn sounding(
            playing: bool,
            stopping: bool,
        ) -> crate::engine::StudioSnapshot {
            crate::engine::StudioSnapshot {
                playing,
                stopping,
                ..Default::default()
            }
        }

        pub(in crate::app) fn write(directory: &std::path::Path, name: &str) -> std::path::PathBuf {
            let path = directory.join(name);
            std::fs::write(&path, "$: s(\"bd\")").expect("score");
            path
        }

        pub(in crate::app) fn read_raw_paste_batch(
            first: Event,
            poll: impl FnMut(Duration) -> std::io::Result<bool>,
            read: impl FnMut() -> std::io::Result<Event>,
        ) -> std::io::Result<Vec<Event>> {
            read_raw_paste_chunk(
                first,
                &mut false,
                MAX_RAW_PASTE_KEY_EVENTS_PER_TURN,
                MAX_RAW_PASTE_TERMINAL_EVENTS_PER_TURN,
                poll,
                read,
            )
        }

        impl App {
            /// Keys the open reference consumes. Returns true when it took the key.
            pub(in crate::app) fn handle_reference_key(
                &mut self,
                code: KeyCode,
                primary: bool,
                alt: bool,
            ) -> Result<bool, RuntimeError> {
                self.handle_reference_key_with_modifiers(code, primary, false, alt)
            }

            // Wait for completed import rows, not an idle worker: on a cold cache,
            // folder walks can remain queued behind default-manifest downloads.
            pub(in crate::app) fn settle_sample_sources(&mut self) {
                const CEILING: std::time::Duration = std::time::Duration::from_secs(60);
                let started = std::time::Instant::now();
                loop {
                    if let Some(library) = self.worker.library() {
                        library.wait_until_idle(CEILING.saturating_sub(started.elapsed()));
                    }
                    self.poll_readiness();
                    let walking = self.source_reports.iter().any(|report| {
                        report.state == rustel_runtime::samples::GlobalSourceState::Loading
                    });
                    if !walking || started.elapsed() >= CEILING {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
            }

            /// TEST. A settings action as the sheet would hand it over.
            pub(in crate::app) fn dispatch_settings_action_for_test(
                &mut self,
                action: SettingsAction,
            ) {
                match action {
                    SettingsAction::LearnSlot(slot) => self.mapping_learn = Some(slot),
                    SettingsAction::ClearSlot(slot) => self.ui_settings.mappings[slot] = None,
                    _ => {}
                }
            }

            /// The edge a dock sits at, for the tests that lay the screen out.
            pub(in crate::app) fn viz_dock_edge(
                &self,
                index: usize,
            ) -> super::super::super::viz_panel::Edge {
                self.prefs.visuals[index].edge
            }
        }
    }

    mod live_material {
        //! Tests for live tab retention, sample memory, and scene cleanup.

        use super::super::*;
        use super::support::*;

        /// A setup sent to the engine this session that picks variants keeps
        /// every tab loaded ahead with every variant once its text no longer
        /// says so: what it defined is still in the heap, and the engine keeps
        /// every variant from then on, so a tab pinned afterwards must find
        /// them all loaded.
        #[test]
        fn a_setup_sent_this_session_picks_variants_once_edited_without_them() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            assert!(!app.setups_select_variants());
            app.global_prebake = "register('shuffle', p => p.n(irand(4)))".to_owned();
            app.queue_prebakes(&[PrebakeScope::Global], false);
            app.global_prebake = "register('shuffle', p => p.fast(2))".to_owned();
            assert!(
                app.setups_select_variants(),
                "the helper it sent is still defined"
            );
            settle(&mut app);
            assert!(app.setups_select_variants());
        }

        /// Which tabs the engine keeps whole, and which by their last visit: a
        /// tab in a pane or behind a pad is always kept, and the rest follow
        /// the order they were visited, the most recent first.
        #[test]
        fn live_tabs_keep_the_pinned_ones_and_order_the_rest_by_visit() {
            let directory = tempfile::tempdir().expect("tempdir");
            let mut app = app_over(directory.path());
            let first = app.scenes.current().id;
            app.new_scene(false);
            let second = app.scenes.current().id;
            app.new_scene(false);
            let third = app.scenes.current().id;
            app.new_scene(false);
            let fourth = app.scenes.current().id;
            app.scenes.get_mut(second).expect("second").pad = Some(crate::pads::Pad {
                note: 36,
                channel: 10,
            });
            app.audible_scene = None;
            for scene in [first, fourth, second, third] {
                let index = app.scenes.index_of(scene).expect("a tab");
                assert!(app.select_scene(index));
            }

            assert_eq!(
                app.live_tabs(),
                [
                    (second, true),
                    (third, true),
                    (fourth, false),
                    (first, false)
                ],
                "the pad's tab and the open one are kept; the fourth was visited after the first"
            );

            app.audible_scene = Some(first);
            assert!(
                app.live_tabs().contains(&(first, true)),
                "the tab last heard is kept too"
            );
        }

        /// The engine hears about the live tabs once typing pauses, not per
        /// keystroke, and every tab sent carries whether it is always kept.
        #[test]
        fn the_engine_hears_the_live_tabs_once_typing_pauses() {
            let directory = tempfile::tempdir().expect("tempdir");
            let mut app = app_over(directory.path());
            let now = Instant::now();
            app.last_edit_at = now;
            app.sync_live_material(now);
            assert_eq!(app.live_material_sent, None, "not mid-keystroke");

            app.sync_live_material(now + LINT_DEBOUNCE);
            let current = app.scenes.current();
            assert_eq!(
                app.live_material_sent,
                Some(vec![(current.id, LiveTextMark::of(current), true)]),
                "the tab being written is always kept"
            );
        }

        /// A kept tab is loaded ahead at exactly the takes it can play: each
        /// one its `n` writes and the one its name spells, and not the rest of
        /// the bank. Asked at its first take alone, a pad stepping through
        /// takes found the others missing on its first press.
        #[test]
        fn a_kept_tab_is_loaded_ahead_at_the_takes_it_plays() {
            let directory = tempfile::tempdir().expect("tempdir");
            let mut app = app_over(directory.path());
            settle(&mut app);
            let library = Arc::new(rustel_runtime::samples::SampleLibrary::empty_without_loading());
            library
                .register_trusted_custom(
                    r#"{"_base":"https://samples.invalid/","takes":["0.wav","1.wav","2.wav","3.wav"]}"#,
                    None,
                )
                .expect("bank map");
            app.worker.set_library(Arc::clone(&library));
            app.scenes.current_mut().editor =
                Editor::new("$: s(\"takes:3\").n(\"<0 1>\")").expect("editor");
            assert_eq!(app.live_material_sent, None, "nothing sent yet");

            app.sync_live_material(Instant::now() + LINT_DEBOUNCE);

            assert!(app.live_material_sent.is_some(), "the tabs were sent");
            let asked: Vec<bool> = (0..4)
                .map(|take| library.asked_for_test(&format!("https://samples.invalid/{take}.wav")))
                .collect();
            assert_eq!(asked, [true, true, false, true]);
        }

        /// Something of every kind the studio keeps per tab, kept for `id`.
        fn remember_something_of(app: &mut App, id: SceneId) {
            let revision = app.scenes.get(id).expect("a tab").editor.revision();
            app.note_scene_visit(id);
            app.lint.insert(
                id,
                LintResult {
                    scene: id,
                    revision,
                    input_channels: None,
                    diagnostics: Vec::new(),
                },
            );
            app.live_sliders.insert(
                id,
                LiveSliderSet {
                    revision,
                    sliders: Vec::new(),
                },
            );
            app.readiness.insert(id, Readiness::default());
        }

        fn holds_anything_of(app: &App, id: SceneId) -> bool {
            app.scene_visits.contains(&id)
                || app.lint.contains_key(&id)
                || app.live_sliders.contains_key(&id)
                || app.readiness.contains_key(&id)
        }

        /// A closed tab holds nothing: its lint, its sliders, its readiness
        /// and its place among the visits go with it, and the count of tabs
        /// let go moves, which is what has the engine sweep its sounds.
        #[test]
        fn closing_a_scene_forgets_it() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.new_scene(false);
            let closed = app.scenes.current().id;
            remember_something_of(&mut app, closed);
            let before = app.tabs_closed;

            app.close_scene();

            assert!(app.scenes.get(closed).is_none(), "closed: {}", app.status);
            assert!(!holds_anything_of(&app, closed));
            assert_eq!(app.tabs_closed, before + 1);
            assert!(!app.status.contains("pad"), "{}", app.status);
        }

        /// Put a folder where the score file is, so each write to it fails.
        fn refuse_writes_to(path: &std::path::Path) {
            let _ = std::fs::remove_file(path);
            std::fs::create_dir(path).expect("a folder in the place of the file");
        }

        /// A score that cannot be written stays open: its text is nowhere else.
        #[test]
        fn a_scene_that_cannot_be_saved_stays_open() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.new_scene(false);
            let unsaved = app.scenes.current().id;
            type_text(&mut app, "hh");
            let path = app.scenes.current().path.clone();
            refuse_writes_to(&path);
            let before = app.tabs_closed;

            app.close_scene();

            let scene = app.scenes.get(unsaved).expect("the tab stays open");
            assert!(scene.editor.source().contains("hh"));
            assert_eq!(app.tabs_closed, before);
            assert!(app.status.contains("is not closed"), "{}", app.status);

            // When the write can succeed again, the same key closes the tab.
            std::fs::remove_dir(&path).expect("the folder goes");
            app.close_scene();
            assert!(app.scenes.get(unsaved).is_none(), "closed: {}", app.status);
            assert!(
                std::fs::read_to_string(&path)
                    .expect("score file")
                    .contains("hh")
            );
        }

        /// A set is not put down while one of its scores cannot be written.
        /// The set panel asks: Enter retries, Del leaves without the score,
        /// Esc keeps the set.
        #[test]
        fn a_set_with_a_score_that_cannot_be_saved_asks_before_it_goes() {
            let press = |app: &mut App, code| {
                app.handle_terminal_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
                    .unwrap();
            };
            let open = |app: &mut App, folder: &std::path::Path| {
                let set = SceneSet::open(folder, "// starter").expect("set");
                app.switch_set(set, "opened set");
            };
            let asking = |app: &App| {
                app.set_panel
                    .as_ref()
                    .is_some_and(|panel| panel.unsaved.is_some())
            };
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let unsaved = app.scenes.current().id;
            type_text(&mut app, "hh");
            let path = app.scenes.current().path.clone();
            refuse_writes_to(&path);
            let elsewhere = tempfile::tempdir().expect("second set");
            let other = write(elsewhere.path(), "other.strudel");

            open(&mut app, elsewhere.path());
            let scene = app.scenes.get(unsaved).expect("the old set stays open");
            assert!(scene.editor.source().contains("hh"));
            assert!(asking(&app), "{}", app.status);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Set));
            press(&mut app, KeyCode::Esc);
            assert!(!asking(&app) && app.scenes.get(unsaved).is_some());

            // Enter writes again. It fails, so the panel asks again. Once the
            // write succeeds, the set goes with the score on disk.
            open(&mut app, elsewhere.path());
            press(&mut app, KeyCode::Enter);
            assert!(asking(&app), "{}", app.status);
            std::fs::remove_dir(&path).expect("the folder goes");
            press(&mut app, KeyCode::Enter);
            assert_eq!(app.scenes.directory(), elsewhere.path());
            assert!(std::fs::read_to_string(&path).unwrap().contains("hh"));

            // Del leaves without the score that could not be written.
            type_text(&mut app, "kk");
            refuse_writes_to(&other);
            open(&mut app, directory.path());
            press(&mut app, KeyCode::Delete);
            assert_eq!(app.scenes.directory(), directory.path());
            assert!(other.is_dir(), "the edit was left behind");
        }

        /// The set file keeps a pad for open tabs only, so a tab closed with
        /// one comes back without it; the close says so rather than leaving it
        /// to be found on the next press.
        #[test]
        fn closing_a_scene_with_a_pad_says_the_pad_goes() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.new_scene(false);
            assert!(app.scenes.learn_pad(Pad {
                note: 36,
                channel: 10,
            }));

            app.close_scene();

            assert!(
                app.status.ends_with("· its pad is forgotten"),
                "{}",
                app.status
            );
        }

        /// A score deleted from the set panel while open leaves the strip the
        /// way a closed one does.
        #[test]
        fn deleting_an_open_file_forgets_it() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.new_scene(false);
            let deleted = app.scenes.current().id;
            let path = app.scenes.current().path.clone();
            remember_something_of(&mut app, deleted);
            let before = app.tabs_closed;

            app.arm_delete_scene();
            app.confirm_delete_set_line();

            assert!(!path.exists(), "deleted: {}", app.status);
            assert!(!holds_anything_of(&app, deleted));
            assert_eq!(app.tabs_closed, before + 1);
        }

        /// A set put down takes every tab with it, and what was kept for each
        /// goes whole - as one move of the count, not one per tab.
        #[test]
        fn switching_sets_forgets_every_tab_of_the_old_one() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let first = app.scenes.current().id;
            app.new_scene(false);
            let second = app.scenes.current().id;
            for id in [first, second] {
                remember_something_of(&mut app, id);
            }
            let before = app.tabs_closed;

            let elsewhere = tempfile::tempdir().expect("second set");
            std::fs::write(elsewhere.path().join("first.strudel"), "$: s(\"bd\")\n")
                .expect("scene");
            let set = SceneSet::open(elsewhere.path(), "// starter").expect("set");
            app.switch_set(set, "opened set");

            for id in [first, second] {
                assert!(!holds_anything_of(&app, id), "{id:?}");
            }
            assert_eq!(app.tabs_closed, before + 1);
            assert_eq!(
                app.scene_visits,
                [app.scenes.current().id],
                "the new set's tab is where the studio landed"
            );
        }

        /// A tab opened again from the set panel is a new tab, and ranks among
        /// the recent ones by that visit - not as never visited, behind tabs
        /// the performer has not looked at since.
        #[test]
        fn a_reopened_tab_ranks_as_most_recent() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let first = app.scenes.current().id;
            app.new_scene(false);
            let second = app.scenes.current().id;
            let path = app.scenes.current().path.clone();
            app.new_scene(false);
            let third = app.scenes.current().id;
            app.audible_scene = None;
            for scene in [first, third, second] {
                let index = app.scenes.index_of(scene).expect("a tab");
                assert!(app.select_scene(index));
            }
            app.close_scene();
            assert!(app.scenes.get(second).is_none(), "closed: {}", app.status);

            app.open_dropped_score(&path);
            let reopened = app.scenes.current().id;
            assert_ne!(reopened, second, "a reopened file is a new tab");
            let index = app.scenes.index_of(first).expect("a tab");
            assert!(app.select_scene(index));

            assert_eq!(
                app.live_tabs(),
                [(first, true), (reopened, false), (third, false)],
                "the reopened tab was visited after the third"
            );
        }

        /// A tape's tab is not live material, so closing one leaves the tabs
        /// the engine was sent as they were. The count of tabs let go is sent
        /// on its own, or what only the tape played would wait out a sweep
        /// that had already run.
        #[test]
        fn closing_a_tape_is_news_to_the_engine() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.audible_scene = Some(app.scenes.current().id);
            let tapes = directory.path().join("sessions");
            std::fs::create_dir_all(&tapes).expect("sessions");
            let tape = write_tape(&tapes, "2026-09-05T12-09-48", &[(0.0, "$: s(\"bd\")")]);
            app.open_replay_tab(&tape);
            assert!(app.scenes.current().is_replay(), "{}", app.status);
            let later = Instant::now() + LINT_DEBOUNCE;
            app.sync_live_material(later);
            let sent = app.live_material_sent.clone().expect("the tabs were sent");

            app.close_scene();
            assert!(!app.scenes.current().is_replay(), "{}", app.status);
            app.sync_live_material(later);

            assert_eq!(app.live_material_sent, Some(sent), "the same tabs");
            assert_eq!(
                app.tabs_closed_sent, app.tabs_closed,
                "and the tape leaving went with them"
            );
        }

        /// The engine's queue is two deep and shared with evaluations, so it
        /// can refuse the preview RAM policy. The policy stays owed and goes
        /// on a later turn: dropped, the engine kept holding sounds to the old
        /// ceiling until the settings changed again.
        #[test]
        fn a_refused_sample_memory_policy_is_sent_again() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            // A worker shut down refuses every command, as a full queue does.
            app.worker.shutdown();
            app.apply_ui_settings();
            let policy = (
                app.ui_settings.preview_budget.bytes(),
                app.ui_settings.unused_sample_idle.duration(),
            );
            assert_eq!(app.sample_memory_owed, Some(policy));
            app.send_sample_memory();
            assert_eq!(app.sample_memory_owed, Some(policy), "refused again");

            app.worker = StudioWorker::spawn(
                StudioConfig {
                    output: Some("silent".to_owned()),
                    ..Default::default()
                },
                #[cfg(feature = "hydra")]
                rustel_runtime::hydra::HydraBridge::new(),
            )
            .expect("worker");
            // Nothing changes in the settings: an ordinary turn sends it. The
            // turn's other commands may fill the two slots for a moment first.
            let deadline = Instant::now() + Duration::from_secs(5);
            while app.sample_memory_owed.is_some() && Instant::now() < deadline {
                app.pump_turn();
                std::thread::sleep(Duration::from_millis(1));
            }

            assert_eq!(
                app.sample_memory_owed, None,
                "taken on a later turn once there is room"
            );
        }
    }
}
