//! Ratatui composition for the studio's source-first screen.
//!
//! Every colour here comes from the active [`Theme`]; nothing is hard-coded,
//! so a community theme file changes the whole surface without touching this
//! module.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::devices::{DeviceInventory, DevicePanel, DevicePanelView, MidiPortCounts};
use super::editor::{
    Document, Editor, GridRect, KeyboardCapabilities, ScreenMap, ScreenRow, TextRow,
};
use super::engine::{OrbitLevel, StudioDeviceInfo};
use super::export::{ExportGlance, ExportSheet, ExportSheetView};
use super::log::{LogPanel, LogPanelView, StudioLog};
use super::menu;
use super::meter::{MasterDock, MasterState, scale_position};
use super::minimap::{Minimap, MinimapView};
use super::prebake::{PREBAKE_GLYPH, PrebakeRow, PrebakeScope};

/// The mark a replay tab wears on its chip and title: a tape, played.
pub const REPLAY_GLYPH: &str = "▷";
use super::editor::CellPoint;
use super::reference::{Reference, ReferencePanel, ReferenceView};
use super::settings::{self, SettingsSheet, SettingsSheetView, UiSettings};
use super::stats::{beats_per_minute, format_bytes, format_cpu_percent};
use super::syntax;
use super::syntax::{Lexer, Token};
use super::terminal::TerminalFeatures;
use super::theme::{
    Theme, ThemePicker, ThemePickerView, VisualOptions, legible_against_floor, mix,
};
use super::visuals::{SourceMark, VisualRequest, VisualState};
use rustel_runtime::ui_analysis::UiAudioAnalysisFrame;
use rustel_runtime::{
    EnginePressureCause, EnginePressureLevel, EnginePressureSnapshot, ProcessStats, product,
};

mod backdrop;
mod beat_line;
mod footer;
mod header;
mod layout;
mod scene_strip;
mod source;

use backdrop::*;
use footer::*;
use header::*;
use scene_strip::*;
use source::*;

pub(crate) use backdrop::Backdrop;
#[cfg(feature = "hydra")]
pub use backdrop::HydraBackdrop;
pub use backdrop::SelectionWash;
pub use backdrop::VisualBackdrop;
pub(crate) use backdrop::paint_stage;
pub(crate) use backdrop::render_caret_locator;
pub use footer::FooterHits;
pub use footer::MAX_ORBIT_CHIPS;
pub(crate) use footer::footer_content_area;
pub use footer::footer_hits;
pub(crate) use footer::midi_chip_text;
pub use footer::remote_indicator_rect;
pub(crate) use footer::render_piano_indicator;
pub(crate) use footer::render_piano_notes;
pub use footer::status_footer_hits;
pub use header::beat_pulse;
pub use header::busy_pulse;
pub use header::header_go_chip;
pub(crate) use header::launch_chip;
pub use header::loading_line_colour;
pub use header::warning_badge_at;
pub use scene_strip::SceneStripLayout;
pub use scene_strip::scene_strip_hits;
pub use scene_strip::scene_strip_layout;
#[cfg(feature = "hydra")]
pub(crate) use source::draw_slider_control;
pub(crate) use source::gutter_width;
pub(crate) use source::horizontal_scrollbar_left;
pub(crate) use source::horizontal_scrollbar_thumb;
pub(crate) use source::slider_cover_bounds;
pub use source::slider_track_span;
pub use source::source_grid;

pub use layout::{
    ChromeLayout, PaneRegion, SetSidebar, Side, StudioRegions, menu_row, regions, regions_with,
    regions_with_footer,
};

/// A panel that can hold the keyboard. In paint order, back to front -
/// the sheets and the reference among them by when they were last
/// raised, which the app keeps.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PanelKind {
    Reference,
    Log,
    Jobs,
    Export,
    Theme,
    ThemeEditor,
    Settings,
    Devices,
    Set,
    /// The visuals panel.
    Viz,
    /// The mixer panel, a band along the top or the bottom.
    Mixer,
    /// The memory breakdown, a band along the top or the bottom.
    Memory,
    /// The knobs column, a rail for each numeric parameter of the call
    /// at the caret.
    Knobs,
}

impl PanelKind {
    /// The panel's name as the log writes it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Reference => "reference",
            Self::Log => "log",
            Self::Jobs => "jobs",
            Self::Export => "export",
            Self::Theme => "theme",
            Self::ThemeEditor => "theme editor",
            Self::Settings => "settings",
            Self::Devices => "devices",
            Self::Set => "set",
            Self::Viz => "visuals",
            Self::Mixer => "mixer",
            Self::Memory => "memory",
            Self::Knobs => "knobs",
        }
    }
}

/// A live slider control, as the text renderer draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct SliderChip {
    /// The cover the pill is drawn over: `slider(` alone, keyword to the
    /// opening bracket. The numbers after it stay text.
    pub from: usize,
    pub to: usize,
    /// Where the value sits between its bounds, 0..=1.
    pub notch: f64,
    /// On the arrow keys: drawn selected, so the eye knows which control
    /// the next ←/→ will move.
    pub armed: bool,
}

/// Source decorations for one frame, already mapped into current document
/// coordinates by the application.
#[derive(Clone, Copy, Debug, Default)]
pub struct Decorations<'a> {
    /// User caret shape, before any explicit theme override.
    pub caret_shape: super::terminal::CaretShape,
    /// Ranges of events sounding right now.
    pub active: &'a [SourceMark],
    /// Mini-notation spans of the evaluated generation.
    pub mini: &'a [(usize, usize)],
    /// Calls that are live slider controls.
    pub sliders: &'a [SliderChip],
    /// Ranges the linter found a problem at.
    pub errors: &'a [(usize, usize)],
    /// The bracket at the caret and its partner, `true` when they matched;
    /// a lone `false` is an orphan.
    pub brackets: &'a [(usize, usize, bool)],
    /// Tempo calls the score makes that an outside clock overrides.
    pub overridden: &'a [(usize, usize)],
}

/// Whether the scene being edited would play perfectly on the next update:
/// the three levels of ready, folded into one word in the header.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum GoState {
    /// Not checked yet.
    #[default]
    Unchecked,
    /// Syntax and values are fine and every sound is loaded.
    Ready,
    /// Syntax and values are fine; sounds are still arriving.
    Loading { ready: usize, total: usize },
    /// Syntax and values are fine; some sounds failed to load.
    Failed { failed: usize },
    /// The linter found problems; the update would be refused.
    Problems { count: usize },
}

/// What the header's transport word says, which is what the readiness
/// chip after it is placed against.
///
/// Only what the transport is doing: an evaluation is a job, not a
/// transport state - the set is still playing or stopped while it runs -
/// so the busy signal is the logo dot breathing amber, never the word.
/// The word is also drawn in a field as wide as the widest of them, so
/// the chip - and the take, the clocks and the scene name after it -
/// stays put while the word changes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TransportWord {
    pub stopping: bool,
    pub playing: bool,
}

impl TransportWord {
    /// The width of `STOPPING`, the widest word. Kept honest by a test.
    const FIELD: usize = 8;

    fn text(self) -> String {
        let word = if self.stopping {
            "STOPPING"
        } else if self.playing {
            "PLAYING"
        } else {
            "STOPPED"
        };
        format!("{word:<field$}", field = Self::FIELD)
    }
}

impl GoState {
    fn text(self) -> String {
        match self {
            Self::Unchecked => String::new(),
            Self::Ready => "✓ ready".to_owned(),
            Self::Loading { ready, total } => format!("◐ loading {ready}/{total}"),
            Self::Failed { failed: 1 } => "⚠ 1 sound failed".to_owned(),
            Self::Failed { failed } => format!("⚠ {failed} sounds failed"),
            Self::Problems { count: 1 } => "✗ 1 problem".to_owned(),
            Self::Problems { count } => format!("✗ {count} problems"),
        }
    }

    fn color(self, theme: &Theme) -> Color {
        match self {
            Self::Unchecked => theme.muted,
            Self::Ready => theme.ok,
            // A failed sound leaves the set playing without it: warning
            // weight, not the red of a refused update.
            Self::Loading { .. } | Self::Failed { .. } => theme.warn,
            Self::Problems { .. } => theme.error,
        }
    }
}
/// The header's loading line: a thin line along the row's bottom edge
/// that grows as what the score needs comes in, and, once a load has
/// lasted, its words in the tempo's place.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadingLine {
    /// How far the line reaches across the row, 0 to 1.
    pub reach: f64,
    /// `waiting for sounds · 12/40 · 30% · mlkr-grsl`, once shown.
    pub label: Option<String>,
    /// The terminal colours an underline; otherwise the reach is a faint
    /// tint behind the row.
    pub underline: bool,
}

/// A take in progress, for the header.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RecordingChip {
    pub seconds: f64,
    pub bytes: u64,
    pub dropped_seconds: f64,
    /// A sample from the input rather than a take of the mix.
    pub sample: bool,
}

impl RecordingChip {
    fn text(self) -> String {
        let total = self.seconds.max(0.0) as u64;
        let clock = if total >= 3600 {
            format!(
                "{}:{:02}:{:02}",
                total / 3600,
                (total / 60) % 60,
                total % 60
            )
        } else {
            format!("{}:{:02}", total / 60, total % 60)
        };
        let size = if self.bytes >= 1 << 30 {
            format!("{:.1} GB", self.bytes as f64 / (1u64 << 30) as f64)
        } else {
            format!("{} MB", self.bytes >> 20)
        };
        // A sample is short and its size is not the point: the clock is.
        let mut text = if self.sample {
            format!("● REC SAMPLE {clock}")
        } else {
            format!("● REC {clock} · {size}")
        };
        if self.dropped_seconds > 0.0 {
            text.push_str(&format!(" · dropped {:.1}s", self.dropped_seconds));
        }
        text
    }
}

/// One chip of the scene strip.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneChip {
    pub name: String,
    /// On screen right now.
    pub current: bool,
    /// The pane that shows this scene, if one does. The tab wears that
    /// pane's colour, and a click on a tab in the other pane moves the caret
    /// there.
    pub pane: Option<usize>,
    /// The generation the engine is playing came from this scene.
    pub playing: bool,
    pub dirty: bool,
    /// Label of the MIDI pad that launches it, if one is learnt.
    pub pad: Option<String>,
    /// The linter found a problem in it since its text last changed.
    pub errors: bool,
    /// Armed to launch on a cycle line: cycles to go.
    pub armed: Option<f64>,
    /// Setup rather than music, and which of the two.
    pub prebake: Option<PrebakeScope>,
    /// A tape opened to replay.
    pub replay: bool,
    /// Played from its own cycle zero rather than joining the running
    /// one. The chip says so because it changes what every way of
    /// playing this scene does: a pad, ^S, a click.
    pub rewind: bool,
}

/// What the strip is doing besides showing chips.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum SceneStripMode {
    #[default]
    Idle,
    /// The current chip is being renamed; the text so far.
    Renaming(String),
    /// Waiting for a pad press to bind to the current scene.
    Learning,
}

pub struct StudioChrome<'a> {
    pub caret_visible: bool,
    pub keybinds: &'a super::keybinds::Keybinds,
    /// Configured fallback; an opened device supplies its own selection.
    pub registry: &'a rustel_runtime::CapabilityRegistry,
    /// The Cargo features the binary was built with.
    pub build_features: &'a [&'a str],
    /// A score's file, or the file a prebake tab is kept in.
    pub path: &'a Path,
    /// The set's name, shown beside the file: the header is the one place
    /// it is always in view.
    pub set_name: &'a str,
    /// The tab on screen is setup rather than music.
    pub prebake: Option<PrebakeScope>,
    /// The tab on screen is a tape opened to replay.
    pub replay: bool,
    pub theme: &'a Theme,
    pub playing: bool,
    /// What the pictures may do: follow the mix, hold the last of it, or
    /// stay empty because nothing has sounded yet.
    pub motion: super::viz_panel::Motion,
    /// Stop requested; the tail is still ringing out.
    pub stopping: bool,
    /// A job is running, and this is how long ago it began: the logo dot
    /// breathes amber for it. `None` when the studio is idle.
    pub evaluating_for: Option<Duration>,
    pub dirty: bool,
    pub status: &'a str,
    /// Editor mode or pending command, kept visible beside diagnostics.
    pub vim_label: Option<&'a str>,
    /// Vim text modes own the list keys and the text fields.
    pub vim_text_modes: bool,
    /// The find bar outside Vim: `FIND /pattern`.
    pub find_label: Option<&'a str>,
    /// A remote-control listener is active; the status line shows a
    /// clickable `<>` icon.
    pub remote_control: bool,
    /// Brief activity light after accepting an authenticated remote command.
    pub remote_receiving: bool,
    /// Active keyboard piano mode and its software pulse intensity. Its compact
    /// status takes the footer; only the PIANO label pulses, never the notes.
    pub piano: Option<f32>,
    /// Last auditioned chord, retained briefly after returning to editing.
    pub piano_notes: Option<&'a str>,
    pub error: Option<&'a str>,
    /// A sustained audio timing hint, below any error and retained piano notes.
    pub audio_warning: Option<&'a str>,
    pub capabilities: KeyboardCapabilities,
    /// The five menus, rebuilt from live state every frame.
    pub menus: &'a [menu::Menu],
    /// Set while the bar has the keyboard.
    pub menu: Option<&'a menu::MenuState>,
    pub fps: f64,
    pub render_ms: f64,
    /// The caret's line and column, 1-based - a vim-style ruler reading.
    /// The gutter can be turned off, and the column it never shows.
    pub caret: Option<(usize, usize)>,
    pub cycle: Option<f64>,
    pub cps: Option<f64>,
    pub zen: bool,
    /// The File / Edit menu bar. Ignored in zen, which has no bar anyway.
    pub show_menu: bool,
    /// The rustel PLAYING tempo line. Ignored in zen.
    pub show_header: bool,
    /// How the beat line under the header moves, when the layout gave it
    /// a row at all. Ignored without a header.
    pub beat_line: settings::BeatLineStyle,
    /// The footer's meter, orbits and device chips. Off still keeps the
    /// notices and the status line. Ignored in zen, which has no footer.
    pub show_footer: bool,
    pub evaluation_flash: bool,
    /// The room of the panel ⇧F10 has just walked the keyboard onto, while
    /// the landing is still lit. Only the rotation sets it: a panel that
    /// takes the keyboard by its own chord, or by a click, was aimed at and
    /// needs no help being found.
    pub focus_flash: Option<Rect>,
    pub stats: ProcessStats,
    /// How much of `stats`, and of `pressure` below, the header actually
    /// draws.
    pub metric_detail: settings::MetricDetail,
    /// Real-time engine pressure, separate from process-wide CPU and memory.
    pub pressure: Option<&'a EnginePressureSnapshot>,
    pub max_polyphony_override: Option<usize>,
    pub master: &'a MasterState,
    /// The master limiter's character, or `None` while it is off - for the
    /// footer, which otherwise says nothing about it unless it is working.
    pub master_limiter: Option<&'static str>,
    /// The latest post-mix analysis, for the master scope.
    pub audio: Option<&'a UiAudioAnalysisFrame>,
    /// The audition tap's analysis: the sample preview alone, for the
    /// samples browser's own scope.
    pub audition_audio: Option<&'a UiAudioAnalysisFrame>,
    /// What the linter found in the focused scene, if anything.
    pub lint: Option<&'a str>,
    /// Whether the focused scene is good to go.
    pub go: GoState,
    /// A camera declaration that deserves a persistent privacy indicator.
    #[cfg(feature = "hydra")]
    pub webcam: Option<rustel_runtime::hydra::HydraWebcamState>,
    /// A take being written.
    pub recording: Option<RecordingChip>,
    /// A take that just ended on its own, said quietly where the red chip
    /// was: an audience may be looking at this screen.
    pub take_notice: Option<&'a str>,
    /// A short-lived confirmation of something the reader just did, shown over
    /// the frame and gone shortly after. The status line says what the studio
    /// is; this says what just happened, which is a different question and
    /// wants a different place.
    pub toast: Option<&'a str>,
    /// Warnings in the log nobody has looked at.
    pub unseen_warnings: usize,
    /// A render in progress.
    pub exporting: Option<&'a ExportGlance>,
    /// Background jobs chip: downloads, exports, clears - click opens the
    /// jobs list. Every background job shares this one place.
    pub jobs: Option<&'a str>,
    /// A launch waiting for its cycle line: the scene, cycles to go.
    pub launch: Option<(&'a str, f64)>,
    /// Sounds the playing or the waiting score needs, still loading.
    pub loading: Option<LoadingLine>,
    /// An outside clock: `Some(bpm, locked)` when following one, `None`
    /// when the studio keeps its own; `clock_out` when ticks go out.
    pub clock_in: Option<(f64, bool)>,
    pub clock_out: bool,
    /// The orbits that have sounded lately, for the footer strip.
    pub orbits: &'a [OrbitLevel],
    /// Stereo pairs the output has; more than one makes the strip routable.
    pub output_pairs: u16,
    pub now: Instant,
    /// Output device the engine is playing through.
    pub device: Option<&'a str>,
    /// Input device `s("in")` is playing, once one is open.
    pub input: Option<&'a str>,
    /// Whether an input is chosen at all, opened yet or not.
    pub input_chosen: bool,
    pub device_info: Option<&'a StudioDeviceInfo>,
    pub midi_ports: MidiPortCounts,
    /// The pads plugged in, and the lights: something moved on a pad, a
    /// MIDI port spoke, the audio input carried signal - each within the
    /// last blink.
    pub pads: usize,
    pub pad_active: bool,
    pub midi_active: bool,
    pub input_active: bool,
    /// Current post-fader input peak, for the device chip's character fill.
    pub input_peak_db: f32,
}

/// One moment of the evaluation flash's run: the animation in force, how
/// far through the run, and how bright.
#[derive(Clone, Copy)]
pub struct EvaluationPulse {
    pub animation: settings::EvalAnimation,
    /// 0 to 1 across the whole run.
    pub progress: f32,
    /// 0 to 1, the envelope at that progress.
    pub intensity: f32,
}

/// What one cell of a flashing line does.
#[derive(Clone, Copy, Debug, PartialEq)]
enum FlashLook {
    /// The code shows through untouched.
    Off,
    /// Blended toward the theme's opposite grounds by a share.
    Wash(f32),
    /// Swapped around, as the flash has always read.
    Reverse,
    /// The letter gives way to a glyph standing in for it: the glitch's
    /// broken-signal blocks.
    Glyph(&'static str),
}

/// How wide the swipe's soft edge is, as a share of the line.
const SWIPE_TRAIL: f32 = 0.3;

impl EvaluationPulse {
    /// The look of one cell of a line of code, `line` for the glitch's
    /// per-line scramble and `x` from the row's own start.
    fn look(self, line: usize, x: u16, width: u16) -> FlashLook {
        match self.animation {
            settings::EvalAnimation::Normal => FlashLook::Reverse,
            settings::EvalAnimation::FadeFast
            | settings::EvalAnimation::FadeMedium
            | settings::EvalAnimation::FadeSlow => FlashLook::Wash(self.intensity),
            settings::EvalAnimation::Swipe => {
                // The code comes back a column at a time: a soft edge
                // sweeps left to right, the letters standing revealed
                // behind it while the rest of the line still carries the
                // flash, and the whole thing fades as it goes.
                let across = f32::from(x) / f32::from(width.max(1));
                let edge = self.progress * (1.0 + SWIPE_TRAIL) - SWIPE_TRAIL;
                let band = ((across - edge) / SWIPE_TRAIL).clamp(0.0, 1.0);
                let level = self.intensity * band;
                if level > 0.95 {
                    FlashLook::Reverse
                } else if level > 0.02 {
                    FlashLook::Wash(level)
                } else {
                    FlashLook::Off
                }
            }
            settings::EvalAnimation::Zap => {
                if self.intensity > 0.55 {
                    FlashLook::Reverse
                } else {
                    FlashLook::Wash(self.intensity)
                }
            }
            settings::EvalAnimation::Steps => {
                // Four chunky rungs, the way old hardware stepped: whole,
                // three quarters, a half, a quarter, gone.
                let rung = (self.intensity * 4.0).floor() / 4.0;
                if rung >= 1.0 {
                    FlashLook::Reverse
                } else {
                    FlashLook::Wash(rung)
                }
            }
            settings::EvalAnimation::Glitch => {
                if self.intensity <= 0.0 {
                    return FlashLook::Off;
                }
                // A broken signal, flickering fast: each step strikes some
                // lines and spares the rest, and a struck line shatters
                // into bands of blocks and flips where its letters were.
                // The damage holds still within a step, so a moment reads
                // as one corrupted frame, then jumps to the next.
                let step = (self.progress * 24.0) as u32;
                let struck =
                    (glitch_scramble(line, step, 999) % 100) < (45.0 * self.intensity) as u32;
                let scramble = glitch_scramble(line, step, x / 3);
                let roll = scramble % 100;
                let heat = if struck {
                    self.intensity
                } else {
                    self.intensity * 0.3
                };
                if roll < (20.0 * heat) as u32 {
                    FlashLook::Reverse
                } else if roll < (70.0 * heat) as u32 {
                    const BROKEN: [&str; 6] = ["█", "▓", "▒", "░", "▀", "▄"];
                    FlashLook::Glyph(BROKEN[((scramble >> 8) % 6) as usize])
                } else {
                    FlashLook::Off
                }
            }
            settings::EvalAnimation::Wave => {
                let center = f32::from(width) * self.progress;
                let distance = (f32::from(x) - center).abs();
                let band = (1.0 - (distance / 6.0).min(1.0)).powi(2);
                let level = self.intensity * band;
                if band > 0.8 && self.intensity > 0.8 {
                    FlashLook::Reverse
                } else if level > 0.03 {
                    FlashLook::Wash(level)
                } else {
                    FlashLook::Off
                }
            }
        }
    }
}

/// A stable scramble for the glitch, so a moment's static is the same on
/// every redraw of it.
fn glitch_scramble(line: usize, step: u32, block: u16) -> u32 {
    let mix = (line as u32).wrapping_mul(0x9E37_79B1)
        ^ step.wrapping_mul(0x85EB_CA6B)
        ^ u32::from(block).wrapping_mul(0xC2B2_AE35);
    mix.rotate_left(13) ^ mix.rotate_right(7)
}

/// One editor pane as drawn this frame.
pub struct PaneView<'a> {
    pub editor: &'a Editor,
    pub map: &'a ScreenMap,
    pub minimap: &'a Minimap,
    pub decorations: Decorations<'a>,
    /// Scene name for the title row.
    pub name: String,
    pub dirty: bool,
    /// This pane shows the scene the engine is playing.
    pub playing: bool,
    /// The caret lives here.
    pub focused: bool,
    /// The evaluation acknowledgement is drawn over this pane, and where
    /// its animation stands.
    pub flash: Option<EvaluationPulse>,
    /// Shift+F3 lights the caret's logical line and contracts a ring toward
    /// its destination. 1.0 is the beginning of the brief flash.
    pub locate_flash: Option<f32>,
    /// The replay's timeline has the keyboard: the caret is not drawn,
    /// the chosen block is lit.
    pub timeline_focus: bool,
    /// The line numbers down the left edge.
    pub line_numbers: bool,
    /// This pane shows a prebake rather than a scene.
    pub prebake: Option<PrebakeScope>,
    /// This pane shows a tape opened to replay: its timeline goes over the
    /// text.
    pub replay: Option<&'a super::replay::ReplayTab>,
}

/// Where a snippet preview stands: the row it was taken from, the word for
/// what it is waiting for - `None` once it is sounding, the strip saying
/// where - and the playhead fraction through its bar.
#[cfg(feature = "hydra")]
#[derive(Clone, Debug)]
pub struct SnippetPreviewStatus {
    pub row: Option<super::reference::SnippetLine>,
    pub note: Option<(String, bool)>,
    pub progress: Option<f32>,
}

/// Everything one frame needs beyond the chrome.
pub struct StudioView<'a> {
    /// One or two panes, left to right.
    pub panes: Vec<PaneView<'a>>,
    /// Whether overflowing editor panes draw their scrollbars.
    pub show_scrollbars: bool,
    pub visual: &'a VisualState,
    pub devices: &'a DeviceInventory,
    /// True until the first device probe has come back.
    pub scanning: bool,
    /// The device picker, when it is open.
    pub panel: Option<DevicePanel>,
    /// The set's scenes, in strip order.
    pub scenes: &'a [SceneChip],
    /// The two prebakes, for the settings sheet's rows.
    pub prebake_rows: [PrebakeRow; 2],
    /// Where new sets are made, for the settings sheet's row.
    pub sets_folder: String,
    /// Where finished audio takes are written, for the settings sheet's row.
    pub recordings_folder: String,
    /// What the open set says about the limiter, when it says anything -
    /// the sheet's two limiter rows are the studio's default, and a set
    /// with its own is not using them.
    pub set_limiter: Option<String>,
    /// What the sample cache takes on disk, as the Advanced row reads it.
    pub sample_cache: String,
    /// The pre-cache under way, or the last one, for its Advanced row.
    pub precache: Option<super::settings::PrecacheProgress>,
    /// The imported sample sources, as the Sources page reads them.
    pub sources: Vec<super::settings::SourceRow>,
    /// The twelve mapping slots, as the Mapping page reads them.
    pub mappings: [super::settings::SlotView; super::settings::MAPPING_SLOTS],
    /// The shortcut rows, for the Keybinds page.
    pub keybind_rows: Vec<super::settings::KeybindRow>,
    /// The origins of the permissions page.
    pub permission_rows: Vec<super::settings::PermissionRow>,
    /// The plugin folders, for the vst page. Empty while another page shows.
    #[cfg(feature = "vst")]
    pub vst_page: super::settings::VstPage,
    /// What the open stream costs end to end, for the devices panel.
    pub latency: Option<super::devices::LatencyReport>,
    /// The MIDI tab's rows, merged and with their boxes resolved. Empty while
    /// the panel is closed, since nothing draws them then.
    pub midi_rows: Vec<super::devices::MidiRow>,
    /// The clock ports, for the MIDI tab's two clock rows.
    pub clock_in: Option<&'a str>,
    pub clock_out: Option<&'a str>,
    /// Whether the Link session was asked for, and what it last said,
    /// for the Link tab.
    pub link_enabled: bool,
    pub link: Option<rustel_runtime::link::LinkStatus>,
    /// Files the sample loader still has in its line, for the browser's
    /// caching countdown.
    pub caching_samples: usize,
    /// The sounding preview's shape and how far into it the sound has got.
    pub preview_shape: Option<(&'a [u8], f32)>,
    /// Imported sources still being read: folders being walked, packs
    /// being fetched.
    pub importing_sources: usize,
    /// The library still has manifests on the way.
    pub library_loading: bool,
    pub strip_mode: &'a SceneStripMode,
    /// The reference column, when it is open.
    /// The open column, and whether an anchored completion is typing into
    /// it without Insert.
    pub reference: Option<(&'a Reference, &'a ReferencePanel, bool)>,
    /// Which panel holds the keyboard, if any. It decides where the one lit
    /// caret goes and which footer says the way back.
    pub focused_panel: Option<PanelKind>,
    /// The contextual help sheet is orthogonal to panel focus, but while it
    /// covers the studio the underlying caret must not shine through it.
    pub help_open: bool,
    /// A previewed sound still fetching its sample, for the samples tab.
    pub audition_loading: Option<String>,
    /// How loud a sample preview plays, for the samples tab's pulse row.
    pub preview_gain: f32,
    /// Whether moving through the browser's lists of things to hear plays
    /// each row, for the pulse row to say.
    pub auto_preview: bool,
    /// Whether a preview is sounding: a chord's piano recolours its whole
    /// voicing at once, a run its steps, for as long as they sound.
    pub sounding_note: Option<usize>,
    /// Whether the studio has a Hydra frame for the example's background
    /// preview. The code panel shows a loading label until one arrives.
    pub snippet_picture: bool,
    /// Why the selected snippet cannot be drawn, when it cannot.
    pub snippet_refused: Option<String>,
    /// A snippet playing under the score, and where it is sounding: the
    /// shelf lights up the way the score does.
    #[cfg(feature = "hydra")]
    pub snippet_playing: Option<(&'a str, &'a [super::visuals::SourceMark])>,
    /// Where the snippet preview stands, when one is playing.
    #[cfg(feature = "hydra")]
    pub snippet_preview_status: Option<SnippetPreviewStatus>,
    /// The log sheet, when it is open.
    pub log: Option<(&'a StudioLog, &'a LogPanel)>,
    /// Figures drawn in the left column of the log.
    pub log_memory: Option<super::memory::MemoryFigures>,
    /// Background jobs list, when open.
    pub jobs: Option<(&'a super::jobs::JobsPanel, &'a [super::jobs::BackgroundJob])>,
    /// What the docked memory breakdown shows and the room it asks the
    /// layout for, while it is open.
    pub memory: Option<(super::memory::MemoryFigures, super::viz_panel::Dock)>,
    /// The export sheet, when it is open.
    pub export: Option<&'a ExportSheet>,
    /// The theme picker, when it is open.
    pub theme_picker: Option<&'a ThemePicker>,
    /// The set panel, when it is open.
    pub set_panel: Option<&'a super::set_panel::SetPanel>,
    /// A set being named or chosen from the menu, when one is.
    pub set_prompt: Option<&'a super::file_picker::FilePicker>,
    /// The visuals panel, when it is open.
    /// The visuals docks that are open, and which of them has the keyboard.
    pub viz: [Option<VizView<'a>>; super::viz_panel::DOCKS],
    pub viz_focus: usize,
    /// The mixer's picture, assembled when a dock's widget or the mixer
    /// panel shows it.
    pub mixer: Option<super::viz_panel::MixerFacts>,
    /// The mixer panel, while it is open.
    pub mixer_panel: Option<super::mixer_panel::MixerPanel>,
    /// The knobs column, while it is open: each frame's reading of the
    /// call at the caret, and what the hand is doing to it.
    pub knobs: Option<super::knobs_panel::KnobsData<'a>>,
    /// A drag selection over the mixer's devices block, painted while its
    /// rows are still the ones on screen.
    pub mixer_selection: Option<&'a super::mixer_panel::MixerSelection>,
    /// The reference column was raised after every open sheet, so it
    /// paints over them; otherwise the sheets paint over it.
    pub reference_on_top: bool,
    /// Under a camera theme in the picker: what the reader needs to know.
    pub theme_camera: Option<super::theme::CameraNote>,
    /// Live Hydra camera state for the Settings row.
    #[cfg(feature = "hydra")]
    pub hydra_webcam: Option<rustel_runtime::hydra::HydraWebcamStatus>,
    /// The settings sheet, when it is open, with what it shows.
    pub settings: Option<(SettingsSheet, &'a UiSettings, &'a TerminalFeatures)>,
}

/// What a visuals dock draws from: its state, its widgets, the set's
/// name, the mix's level, and the room it asks the layout for.
pub struct VizView<'a> {
    pub panel: &'a super::viz_panel::VizPanel,
    pub widgets: &'a [super::viz_panel::WidgetSpec],
    pub set_name: &'a str,
    /// The mix's level, 0..1.
    pub level: f32,
    /// How long the studio has had sound, in seconds: the clock the
    /// widgets animate on, which stops while the set does.
    pub seconds: f32,
    /// Cells across for a column, rows for a band.
    pub extent: u16,
}

impl VizView<'_> {
    fn dock(&self) -> super::viz_panel::Dock {
        super::viz_panel::Dock {
            edge: self.panel.edge,
            extent: self.extent,
        }
    }
}

/// The room the settings sheet anchors against: the frame short of the
/// footer, where a set's status explains what the sheet just did, and
/// clear of the docked memory breakdown. The sheet is where preview RAM,
/// the idle drop and the sample ceiling are set, and the breakdown is how
/// their effect is watched; a sheet standing over it would hide the very
/// line a change moves.
///
/// The sheet takes the larger span the band leaves. The band's first row
/// does not show which edge it is on: a band along the top starts under
/// the menu, the header and the strip, not at the frame's first row.
/// Where neither span can hold the sheet (a tall band in zen, or one
/// stacked with the desk and the log), the sheet covers the band: a sheet
/// that holds the keyboard must be visible. The paint, the pointer and
/// the menu's "does it fit" all read this one room, so they agree.
pub(crate) fn settings_room(area: Rect, footer: Rect, memory: Rect) -> Rect {
    let room = Rect {
        height: area.height.saturating_sub(footer.height),
        ..area
    };
    if memory.is_empty() {
        return room;
    }
    let above = memory.y.saturating_sub(room.y).min(room.height);
    let under = memory.bottom().clamp(room.y, room.bottom());
    let below = room.bottom().saturating_sub(under);
    let clear = if below >= above {
        Rect {
            y: under,
            height: below,
            ..room
        }
    } else {
        Rect {
            height: above,
            ..room
        }
    };
    if SettingsSheetView::geometry(clear).is_some() {
        clear
    } else {
        room
    }
}

/// Where the log actually draws.
///
/// Docked, it is the room the layout carved, beside the other fixtures.
/// The app's geometry and hit-testing follow `regions.log`, so the paint
/// uses the same room and does not cover the score, the desk or the docks.
///
/// Undocked, the sheet bottom-anchors against what it is given, and the
/// footer is where a set's health and status live - so it gets the frame
/// short of the footer and sits over the score rather than the transport.
pub(crate) fn log_room(panel: &LogPanel, docked: Rect, footer: Rect, area: Rect) -> Rect {
    if panel.sticky && !docked.is_empty() {
        return docked;
    }
    Rect {
        height: area.height.saturating_sub(footer.height),
        ..area
    }
}

/// The room a sticky log's dock asks the layout for on a terminal the
/// size of `frame`: a band along the top or the bottom, as tall as `-`
/// and `+` made it or [`super::log::default_dock_height`] until they have.
///
/// Never a column. A log is lines of prose read left to right, and down a
/// thirty-six-cell side every one of them wrapped two or three times; an
/// edge kept from when it could be a column is read as the bottom.
///
/// `pub(crate)` rather than private: the app computes its own hit-test
/// regions from `regions_with` outside a draw, and must ask for the same
/// room a real frame would or a click would test against a layout the
/// screen never showed.
pub(crate) fn log_dock(panel: &LogPanel, frame: Rect) -> Option<super::viz_panel::Dock> {
    panel.sticky.then(|| super::viz_panel::Dock {
        edge: panel.edge.band(),
        extent: panel.dock_height(frame.height),
    })
}

/// Draw the accent round a panel's edge, changing no cell's symbol.
///
/// Only the colour: the landing is a glance, and a border that swapped its
/// characters would shift the eye to the shape rather than the place.
fn outline_landing(buffer: &mut Buffer, room: Rect, theme: &Theme) {
    if room.is_empty() {
        return;
    }
    for x in room.x..room.right() {
        for y in [room.y, room.bottom().saturating_sub(1)] {
            if let Some(cell) = buffer.cell_mut((x, y)) {
                cell.set_fg(theme.accent);
            }
        }
    }
    for y in room.y..room.bottom() {
        for x in [room.x, room.right().saturating_sub(1)] {
            if let Some(cell) = buffer.cell_mut((x, y)) {
                cell.set_fg(theme.accent);
            }
        }
    }
}

pub fn render(frame: &mut Frame<'_>, view: StudioView<'_>, chrome: StudioChrome<'_>) {
    let area = frame.area();
    let theme = chrome.theme;
    let toast = chrome.toast;
    frame.render_widget(Backdrop { theme }, area);
    let layout = regions_with_footer(
        area,
        ChromeLayout {
            zen: chrome.zen,
            menu: chrome.show_menu,
            header: chrome.show_header,
            beat_line: chrome.beat_line != settings::BeatLineStyle::Off,
            footer: chrome.show_footer,
        },
        view.panes.len(),
        view.reference.is_some(),
        settings::minimap(),
        view.set_panel.map(|panel| SetSidebar {
            side: if settings::set_panel_right() {
                Side::Right
            } else {
                Side::Left
            },
            width: panel.width,
        }),
        [
            view.viz[0].as_ref().map(VizView::dock),
            view.viz[1].as_ref().map(VizView::dock),
        ],
        view.mixer_panel.map(super::mixer_panel::MixerPanel::dock),
        view.log.and_then(|(_, panel)| log_dock(panel, area)),
        view.memory.as_ref().map(|(_, dock)| *dock),
        view.knobs.is_some(),
        u16::from(chrome.error.is_some()) + u16::from(chrome.audio_warning.is_some()),
        chrome.piano_notes.is_some(),
    );
    if !layout.menu.is_empty() {
        menu::render_bar(
            frame.buffer_mut(),
            layout.menu,
            chrome.menus,
            chrome.menu,
            theme,
        );
    }
    if !layout.header.is_empty() {
        frame.render_widget(Header { chrome: &chrome }, layout.header);
        if layout.header.height > 1 {
            frame.render_widget(
                beat_line::BeatLine { chrome: &chrome },
                Rect::new(layout.header.x, layout.header.y + 1, layout.header.width, 1),
            );
        }
    } else {
        // No header, no badge: zen mode and a three-row terminal must not
        // keep last frame's hit rect armed over whatever sits there now.
        set_warning_badge(None);
        super::jobs::set_jobs_chip(None);
        super::memory::set_memory_chip(None);
    }
    if !layout.scenes.is_empty() {
        frame.render_widget(
            SceneStrip {
                keybinds: chrome.keybinds,
                chips: view.scenes,
                mode: view.strip_mode,
                split: view.panes.len() > 1,
                capabilities: chrome.capabilities,
                theme,
            },
            layout.scenes,
        );
    }
    let mut cursor = None;
    for (index, pane) in view.panes.iter().enumerate().take(layout.pane_count) {
        let region = layout.panes[index];
        if !region.title.is_empty() {
            frame.render_widget(
                PaneTitle {
                    index,
                    name: &pane.name,
                    dirty: pane.dirty,
                    playing: pane.playing,
                    focused: pane.focused,
                    prebake: pane.prebake.is_some(),
                    replay: pane.replay.is_some(),
                    theme,
                },
                region.title,
            );
        }
        // A tape's timeline sits over its text: the same split the app made
        // for the map, so the rows agree.
        let mut editor_area = region.editor;
        if let Some(tab) = pane.replay
            && let Some((strip, rest)) = super::replay::split_timeline(region.editor)
        {
            frame.render_widget(
                super::replay::TimelineView {
                    tab,
                    theme,
                    now: std::time::Instant::now(),
                    focused: pane.timeline_focus,
                },
                strip,
            );
            if let Some(shortcuts) = super::replay::shortcut_area(region.editor) {
                frame.render_widget(
                    super::replay::TimelineShortcuts {
                        focused: pane.timeline_focus,
                        theme,
                        keybinds: chrome.keybinds,
                    },
                    shortcuts,
                );
            }
            editor_area = rest;
        }
        // Recreate the app's reservation after the replay strip has taken
        // its rows. The renderer builds its own base layout, so the app's
        // stored hit rectangle is not present in `layout`; deriving it from
        // the same editor extent keeps paint, map and mouse on one row.
        let horizontal_scrollbar = if view.show_scrollbars
            && pane.editor.horizontal_scroll_extent().is_some()
            && editor_area.height > 1
        {
            let bar = Rect::new(
                editor_area.x,
                editor_area.bottom() - 1,
                editor_area.width,
                1,
            );
            editor_area.height -= 1;
            bar
        } else {
            Rect::default()
        };
        frame.render_widget(
            EditorPane {
                editor: pane.editor,
                map: pane.map,
                line_numbers: pane.line_numbers,
                visual: view.visual,
                decorations: pane.decorations,
                theme,
                area: editor_area,
            },
            editor_area,
        );
        if pane.locate_flash.is_some() {
            let line = pane
                .editor
                .document()
                .line_of(pane.editor.primary_selection().head)
                .unwrap_or(0);
            frame.render_widget(
                CaretLineFlash {
                    map: pane.map,
                    line,
                    theme,
                },
                editor_area,
            );
        }
        if let Some(extent) = pane.editor.horizontal_scroll_extent()
            && !horizontal_scrollbar.is_empty()
        {
            frame.render_widget(HorizontalScrollbar { extent, theme }, horizontal_scrollbar);
        }
        if let Some(pulse) = pane.flash {
            frame.render_widget(
                EvaluationFlash {
                    map: pane.map,
                    theme,
                    pulse,
                },
                editor_area,
            );
        }
        if !region.minimap.is_empty() {
            frame.render_widget(
                MinimapView {
                    minimap: pane.minimap,
                    theme,
                    viewport: visible_rows(pane.map),
                },
                region.minimap,
            );
        }
        if view.show_scrollbars && Scrollbar::needed(pane.editor) && !region.scrollbar.is_empty() {
            frame.render_widget(
                Scrollbar {
                    editor: pane.editor,
                    theme,
                },
                region.scrollbar,
            );
        }
        if pane.focused && !pane.timeline_focus {
            cursor = pane
                .map
                .cell_for_offset(pane.editor.primary_selection().head);
        }
    }
    // Exactly one caret is ever lit. A focused panel takes it - into its
    // search box when it has one, nowhere when it does not - and the score
    // keeps it the rest of the time.
    if view.focused_panel.is_some() {
        cursor = None;
    }
    if !layout.footer.is_empty() {
        frame.render_widget(Footer { chrome: &chrome }, layout.footer);
    }
    // The set panel and the docks sit on the stage with the panes; the
    // reference column and the sheets stack over them in the order they
    // were raised - the one raised last on top - and the menu, the theme
    // editor and the help paint over those in turn.
    let draw_reference = |frame: &mut Frame<'_>, cursor: &mut Option<CellPoint>| {
        let Some((reference, panel, anchored)) = view.reference else {
            return;
        };
        if layout.reference.is_empty() {
            return;
        }
        let focused = view.focused_panel == Some(PanelKind::Reference);
        frame.render_widget(
            ReferenceView {
                keybinds: chrome.keybinds,
                reference,
                panel,
                theme,
                focused,
                vim: chrome.vim_text_modes,
                anchored,
                pulse: Some(super::reference::SamplesPulse {
                    // The preview's own tap, not the mix: the meter shows
                    // what the browser is sounding.
                    peak_db: super::reference::audition_peak_db(chrome.audition_audio),
                    preview_gain: view.preview_gain,
                    auto_preview: view.auto_preview,
                    shape: view.preview_shape,
                }),
                sounding_note: view.sounding_note,
                picture: view.snippet_picture,
                refused: view.snippet_refused.clone(),
                #[cfg(feature = "hydra")]
                playing: view.snippet_playing,
                #[cfg(feature = "hydra")]
                preview_note: view.snippet_preview_status.clone().and_then(|status| {
                    status
                        .note
                        .map(|(word, sounding)| (status.row, word, sounding))
                }),
                #[cfg(feature = "hydra")]
                preview_row: view.snippet_preview_status.clone().map(|status| status.row),
                #[cfg(feature = "hydra")]
                preview_progress: view
                    .snippet_preview_status
                    .as_ref()
                    .and_then(|status| status.progress),
                loading: view.audition_loading.clone(),
                caching: view.caching_samples,
                importing: view.importing_sources,
                library_loading: view.library_loading,
            },
            layout.reference,
        );
        // Command rests with no caret; typing states keep it up.
        if focused && (!chrome.vim_text_modes || panel.vim_insert() || anchored) {
            *cursor = panel
                .search_cursor(super::reference::inner_area(layout.reference))
                .map(|(x, y)| CellPoint::new(x, y));
        }
        // A caret under the overlaid reference would be drawn on top of it.
        if layout.reference_overlays
            && let Some(position) = *cursor
            && layout.reference.contains((position.x, position.y).into())
            && view.focused_panel.is_none()
        {
            *cursor = None;
        }
    };
    // The desk along its edge, a fixture like the set panel's column -
    // under the set panel, so the set sheet a narrow screen makes of it
    // stays on top, where the pointer already finds it first.
    if let Some(panel) = view.mixer_panel
        && !layout.mixer.is_empty()
    {
        frame.render_widget(
            super::mixer_panel::MixerPanelView {
                keybinds: chrome.keybinds,
                facts: view.mixer.as_ref(),
                panel,
                theme,
                focused: view.focused_panel == Some(PanelKind::Mixer),
                area: layout.mixer,
                selection: view.mixer_selection,
            },
            area,
        );
    }
    // The memory breakdown in the room the layout carved it, a fixture
    // like the desk and painted with it: under the set panel, whose sheet
    // a narrow screen or zen lays over the band's rows and which the
    // pointer finds first, and under the reference - which only zen, where
    // both are laid over the score, lets the two meet - and every sheet.
    // In zen, where it lies over the score, Esc puts it away, and its keys
    // hint says so.
    if let Some((figures, _)) = view.memory.as_ref()
        && !layout.memory.is_empty()
    {
        frame.render_widget(
            super::memory::MemoryView {
                figures,
                theme,
                focused: view.focused_panel == Some(PanelKind::Memory),
                closes_on_esc: chrome.zen,
            },
            layout.memory,
        );
    }
    if let Some(panel) = view.set_panel
        && !layout.sidebar_hidden
    {
        frame.render_widget(
            super::set_panel::SetPanelView {
                panel,
                theme,
                sidebar: layout.sidebar,
                on_right: layout.sidebar_side == Side::Right,
                focused: view.focused_panel == Some(PanelKind::Set),
                keybinds: chrome.keybinds,
            },
            area,
        );
    }
    // The docks stand with the set panel, under every sheet.
    for (index, viz) in view.viz.iter().enumerate() {
        let Some(viz) = viz else { continue };
        if layout.viz[index].is_empty() {
            continue;
        }
        frame.render_widget(
            super::viz_panel::VizDockView {
                panel: viz.panel,
                index,
                widgets: viz.widgets,
                set_name: viz.set_name,
                state: view.visual,
                theme,
                motion: chrome.motion,
                level: viz.level,
                seconds: viz.seconds,
                area: layout.viz[index],
                focused: view.focused_panel == Some(PanelKind::Viz) && view.viz_focus == index,
                mixer: view.mixer.as_ref(),
            },
            area,
        );
    }
    // The knobs column, a fixture at the score's right edge, painted with
    // the other fixtures and under every sheet.
    if let Some(knobs) = view.knobs
        && !layout.knobs.is_empty()
    {
        frame.render_widget(
            super::knobs_panel::KnobsView {
                name: knobs.name,
                listed: knobs.listed,
                knobs: knobs.knobs,
                panel: knobs.panel,
                theme,
                focused: view.focused_panel == Some(PanelKind::Knobs),
                area: layout.knobs,
            },
            area,
        );
    }
    // The reference always covers docked visuals. Raising another sheet
    // only changes its position relative to sheets; it must not expose the
    // ends of full-width visual bands behind the reference column.
    if !view.reference_on_top {
        draw_reference(frame, &mut cursor);
    }
    if let Some((log, panel)) = view.log {
        // Docked, the log gets the room the layout carved for it, beside
        // the other fixtures. Undocked, the sheet gets the frame short of
        // the footer, so it sits over the score instead of over the
        // transport and the dock. See `log_room`.
        let sheet = log_room(panel, layout.log, layout.footer, area);
        frame.render_widget(
            LogPanelView {
                keybinds: chrome.keybinds,
                panel,
                log,
                theme,
                pressure: chrome.pressure,
                device: chrome.device_info,
                memory: view.log_memory.as_ref(),
                docked: panel.sticky && !layout.log.is_empty(),
            },
            sheet,
        );
    }
    if let Some((panel, jobs)) = view.jobs {
        frame.render_widget(
            super::jobs::JobsPanelView {
                panel,
                jobs,
                theme,
                focused: view.focused_panel == Some(PanelKind::Jobs),
            },
            area,
        );
    }
    if let Some(sheet) = view.export {
        frame.render_widget(
            ExportSheetView {
                sheet,
                theme,
                vim: chrome.vim_text_modes,
            },
            area,
        );
    }
    if let Some((sheet, settings, features)) = view.settings {
        let settings_room = settings_room(area, layout.footer, layout.memory);
        frame.render_widget(
            SettingsSheetView {
                sheet,
                settings,
                features,
                tier: crate::graphics::tier(),
                registry: chrome.registry,
                build_features: chrome.build_features,
                device: chrome.device_info,
                pressure: chrome.pressure,
                max_polyphony_override: chrome.max_polyphony_override,
                #[cfg(feature = "hydra")]
                hydra_webcam: view.hydra_webcam,
                prebakes: view.prebake_rows,
                sets_folder: view.sets_folder.clone(),
                recordings_folder: view.recordings_folder.clone(),
                set_limiter: view.set_limiter.clone(),
                sample_cache: view.sample_cache.clone(),
                precache: view.precache.clone(),
                sources: view.sources,
                mappings: view.mappings,
                bindings: view.keybind_rows,
                permissions: view.permission_rows,
                #[cfg(feature = "vst")]
                vst: view.vst_page,
                capabilities: chrome.capabilities,
                theme,
            },
            settings_room,
        );
    }
    if let Some(panel) = view.panel {
        frame.render_widget(
            DevicePanelView {
                latency: view.latency,
                midi_rows: &view.midi_rows,
                clock_in: view.clock_in,
                clock_out: view.clock_out,
                link_enabled: view.link_enabled,
                link: view.link,
                panel,
                inventory: view.devices,
                theme,
                current_output: chrome.device,
                current_input: chrome.input,
                input_chosen: chrome.input_chosen,
                scanning: view.scanning,
            },
            area,
        );
    }
    if let Some(picker) = view.theme_picker {
        frame.render_widget(
            ThemePickerView {
                picker,
                theme,
                camera: view.theme_camera,
                vim: chrome.vim_text_modes,
            },
            area,
        );
    }
    if view.reference_on_top {
        draw_reference(frame, &mut cursor);
    }
    let copy_hint = chrome
        .keybinds
        .binding(super::keybinds::BindAction::Copy)
        .map(|key| key.hint());
    let prompt_active = !view.help_open && chrome.menu.is_none();
    if let Some(picker) = view.set_prompt {
        frame.render_widget(
            super::file_picker::FilePickerView {
                picker,
                theme,
                copy_hint: copy_hint
                    .as_deref()
                    .filter(|_| prompt_active && view.focused_panel == Some(PanelKind::Set)),
            },
            area,
        );
    }
    if let Some(viz) = view
        .viz
        .iter()
        .flatten()
        .find(|viz| viz.panel.adding.is_some())
    {
        frame.render_widget(
            super::viz_panel::VizAddView {
                panel: viz.panel,
                theme,
            },
            area,
        );
    }
    if let Some((index, picker)) = view
        .viz
        .iter()
        .enumerate()
        .find_map(|(index, viz)| Some((index, viz.as_ref()?.panel.prompt.as_ref()?)))
    {
        frame.render_widget(
            super::file_picker::FilePickerView {
                picker,
                theme,
                copy_hint: copy_hint.as_deref().filter(|_| {
                    prompt_active
                        && view.focused_panel == Some(PanelKind::Viz)
                        && view.viz_focus == index
                }),
            },
            area,
        );
    }
    // Error navigation and panel rotation share this one restrained landing
    // accent. The terminal keeps drawing its ordinary caret inside the score.
    if let Some(room) = chrome.focus_flash {
        outline_landing(frame.buffer_mut(), room.intersection(area), theme);
    }
    if !view.help_open
        && chrome.caret_visible
        && let Some(cursor) = cursor
    {
        // A caret under a dropped menu would shine through it: the terminal
        // paints its cursor over whatever cells the frame left there, and
        // the app paints the dropdowns only after this returns. The
        // reference overlay hides the caret the same way; a caret clear of
        // every dropdown still draws. The rects are taken from the same
        // function the dropdowns are drawn with, so the two cannot drift.
        // Zen lays the memory breakdown over the score, and a caret under
        // it is covered the same way.
        let covered = chrome.menu.is_some_and(|state| {
            !layout.menu.is_empty()
                && menu::dropdown_rects(chrome.menus, state, layout.menu, area)
                    .iter()
                    .any(|rect| rect.contains((cursor.x, cursor.y).into()))
        }) || layout.memory.contains((cursor.x, cursor.y).into());
        if !covered {
            frame.set_cursor_position((cursor.x, cursor.y));
        }
    }
    // With no header to carry it - zen, or the header setting off - a
    // launch counts down in the top-right corner of the rightmost pane's
    // source, below the menu bar and the scene strip.
    if layout.header.is_empty()
        && !chrome.stopping
        && let Some((scene, cycles)) = chrome.launch
    {
        let score = layout.panes[layout.pane_count.saturating_sub(1)].editor;
        render_launch_corner(frame.buffer_mut(), score, scene, cycles, theme);
    }
    // Last, over everything: a confirmation is worth nothing behind a panel.
    if let Some(text) = toast {
        // Under whatever chrome is above it: the menu bar, then the header.
        let top = layout
            .header
            .bottom()
            .max(layout.menu.bottom())
            .max(area.y + 1);
        render_toast(frame.buffer_mut(), area, top, text, theme);
    }
}

/// Paint an opaque UI surface over both text and previously queued images.
/// The surface starts clean: a plain `set_style` only patches a cell, so an
/// underline, bold or reverse modifier that the score drew underneath
/// would stay in the panel drawn over it.
/// Scratch canvases use `clear_surface` until their images have screen positions.
pub(crate) fn clear_overlay(buffer: &mut Buffer, area: Rect, style: Style) {
    crate::graphics::cover_images(area);
    clear_surface(buffer, area, style);
}

pub(crate) fn clear_surface(buffer: &mut Buffer, area: Rect, style: Style) {
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = buffer.cell_mut((x, y)) {
                cell.reset();
            }
        }
    }
    buffer.set_style(area, style);
}

/// Shortens `text` to `max_width` cells, ending with a single-cell ellipsis
/// when it does not already fit. Cuts on display width, never on byte or
/// char count, so a wide character is never split in two; a string that
/// already fits comes back unchanged.
pub(super) fn ellipsize(text: &str, max_width: u16) -> String {
    if UnicodeWidthStr::width(text) <= usize::from(max_width) {
        return text.to_owned();
    }
    if max_width == 0 {
        return String::new();
    }
    let budget = usize::from(max_width) - 1;
    let mut kept = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let glyph_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + glyph_width > budget {
            break;
        }
        kept.push(ch);
        width += glyph_width;
    }
    kept.push('…');
    kept
}

#[cfg(test)]
mod tests {
    use super::layout::PANE_MIN_WIDTH_BESIDE_REFERENCE;
    use super::*;
    use crate::devices::DeviceEntry;
    use crate::editor::VirtualRowSpec;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    pub(in crate::view) fn midi_ports(outputs: usize, inputs: usize) -> MidiPortCounts {
        MidiPortCounts { outputs, inputs }
    }

    pub(in crate::view) fn theme() -> Theme {
        Theme::built_in_default()
    }

    /// The flash at its plainest: lit whole, no animation, the look the
    /// reverse modifier has always made.
    fn lit_flash() -> Option<EvaluationPulse> {
        Some(EvaluationPulse {
            animation: settings::EvalAnimation::Normal,
            progress: 0.0,
            intensity: 1.0,
        })
    }

    /// Every look shapes the same pulse data differently: the plain one
    /// swaps the whole run, the fades wash by brightness, zap hits and
    /// washes, steps dims in quarters, the wave's band travels, and the
    /// glitch shatters lines into fast bands of blocks and flips.
    #[test]
    fn the_flash_looks_read_the_same_pulse_differently() {
        use settings::EvalAnimation as Look;
        let look = |animation, progress, intensity, x, width| {
            EvaluationPulse {
                animation,
                progress,
                intensity,
            }
            .look(3, x, width)
        };

        assert_eq!(look(Look::Normal, 0.0, 1.0, 5, 40), FlashLook::Reverse);
        assert_eq!(
            look(Look::FadeMedium, 0.5, 0.5, 5, 40),
            FlashLook::Wash(0.5)
        );
        assert_eq!(
            look(Look::FadeSlow, 0.99, 0.01, 5, 40),
            FlashLook::Wash(0.01)
        );
        assert_eq!(
            look(Look::FadeFast, 0.99, 0.0, 5, 40),
            FlashLook::Wash(0.0),
            "a spent fade washes nothing, the same as leaving the cell"
        );

        assert_eq!(look(Look::Zap, 0.2, 1.0, 5, 40), FlashLook::Reverse);
        assert_eq!(look(Look::Zap, 0.8, 0.4, 5, 40), FlashLook::Wash(0.4));

        assert_eq!(look(Look::Steps, 0.5, 1.0, 5, 40), FlashLook::Reverse);
        assert_eq!(look(Look::Steps, 0.9, 0.5, 5, 40), FlashLook::Wash(0.5));
        assert_eq!(look(Look::Steps, 0.99, 0.3, 5, 40), FlashLook::Wash(0.25));

        // The band crosses left to right, and only where it stands.
        match look(Look::Wave, 0.1, 1.0, 0, 40) {
            FlashLook::Wash(level) => assert!((level - 0.111).abs() < 0.001, "{level}"),
            other => panic!("the band's tail washes, got {other:?}"),
        }
        assert_eq!(
            look(Look::Wave, 0.1, 1.0, 39, 40),
            FlashLook::Off,
            "ahead of the band is untouched"
        );
        assert_eq!(look(Look::Wave, 0.9, 1.0, 39, 40), FlashLook::Wash(0.25));

        // One moment of the glitch mixes its looks across the lines: some
        // cells flipped, some shattered into blocks, some left as they
        // are, and the damage lands on some lines and spares others.
        let glitching = |line: usize, x: u16| {
            EvaluationPulse {
                animation: Look::Glitch,
                progress: 0.5,
                intensity: 0.8,
            }
            .look(line, x, 40)
        };
        let scattered: Vec<FlashLook> = (0..6)
            .flat_map(|line| (0..40).map(move |x| glitching(line, x)))
            .collect();
        assert!(scattered.contains(&FlashLook::Reverse), "{scattered:?}");
        assert!(
            scattered
                .iter()
                .any(|look| matches!(look, FlashLook::Glyph(_))),
            "{scattered:?}"
        );
        assert!(scattered.contains(&FlashLook::Off), "{scattered:?}");
        assert_eq!(
            look(Look::Glitch, 0.5, 0.0, 5, 40),
            FlashLook::Off,
            "a spent glitch is nothing at all"
        );
        // And the damage holds still within one step of the run.
        let a = look(Look::Glitch, 0.5, 0.8, 17, 40);
        let b = look(Look::Glitch, 0.52, 0.8, 17, 40);
        assert_eq!(a, b, "the same step breaks the same way");

        // The swipe reveals from the left: the start of the line is
        // itself again while the rest still carries the flash, and the
        // edge travels until the whole line is back.
        assert_eq!(
            look(Look::Swipe, 0.5, 1.0, 0, 40),
            FlashLook::Off,
            "the leading columns are revealed first"
        );
        assert_eq!(
            look(Look::Swipe, 0.5, 1.0, 39, 40),
            FlashLook::Reverse,
            "the tail still carries the whole flash"
        );
        assert!(
            matches!(look(Look::Swipe, 0.5, 1.0, 22, 40), FlashLook::Wash(_)),
            "the soft edge washes between them"
        );
        assert_eq!(
            look(Look::Swipe, 0.0, 1.0, 0, 40),
            FlashLook::Reverse,
            "the run opens with the line whole"
        );
        assert_eq!(
            look(Look::Swipe, 1.0, 1.0, 39, 40),
            FlashLook::Off,
            "and closes with all of it revealed"
        );
    }

    static DEFAULT_KEYBINDS: std::sync::OnceLock<super::super::keybinds::Keybinds> =
        std::sync::OnceLock::new();

    pub(in crate::view) fn chrome<'a>(
        theme: &'a Theme,
        master: &'a MasterState,
        path: &'a Path,
    ) -> StudioChrome<'a> {
        StudioChrome {
            caret_visible: true,
            keybinds: DEFAULT_KEYBINDS.get_or_init(Default::default),
            registry: rustel_runtime::capability_registry(),
            build_features: &[],
            replay: false,
            menus: &[],
            menu: None,
            prebake: None,
            path,
            set_name: "",
            theme,
            motion: crate::viz_panel::Motion::Off,
            playing: false,
            stopping: false,
            evaluating_for: None,
            dirty: false,
            status: "ready",
            vim_label: None,
            vim_text_modes: false,
            find_label: None,
            remote_control: false,
            remote_receiving: false,
            piano: None,
            piano_notes: None,
            error: None,
            audio_warning: None,
            capabilities: KeyboardCapabilities::enhanced(),
            fps: 60.0,
            render_ms: 1.0,
            caret: None,
            cycle: None,
            cps: None,
            zen: false,
            show_menu: true,
            show_header: true,
            beat_line: settings::BeatLineStyle::Off,
            show_footer: true,
            evaluation_flash: false,
            focus_flash: None,
            stats: ProcessStats::default(),
            metric_detail: settings::MetricDetail::default(),
            pressure: None,
            max_polyphony_override: None,
            master,
            master_limiter: None,
            audio: None,
            audition_audio: None,
            lint: None,
            go: GoState::default(),
            #[cfg(feature = "hydra")]
            webcam: None,
            recording: None,
            take_notice: None,
            unseen_warnings: 0,
            exporting: None,
            jobs: None,
            launch: None,
            loading: None,
            clock_in: None,
            clock_out: false,
            orbits: &[],
            output_pairs: 1,
            now: Instant::now(),
            device: None,
            input: None,
            input_chosen: false,
            device_info: None,
            midi_ports: MidiPortCounts::default(),
            pads: 0,
            pad_active: false,
            midi_active: false,
            input_active: false,
            input_peak_db: -60.0,
            toast: None,
        }
    }

    pub(in crate::view) fn text_of(buffer: &Buffer) -> String {
        buffer.content.iter().map(|cell| cell.symbol()).collect()
    }

    #[test]
    fn the_body_is_source_and_minimap_between_header_and_footer() {
        let area = Rect::new(3, 4, 120, 30);
        let layout = regions(area, false, 1, false, true, None, None);
        let pane = layout.panes[0];
        assert_eq!(layout.pane_count, 1);
        // The menu bar takes the top row and everything below it shifts.
        assert_eq!(layout.menu, Rect::new(3, 4, 120, 1));
        assert_eq!(layout.header, Rect::new(3, 5, 120, 1));
        // One pane, one row: the rule under the chips is a split's.
        assert_eq!(layout.scenes, Rect::new(3, 6, 120, 1));
        assert_eq!(layout.footer, Rect::new(3, 32, 120, 2));
        assert!(pane.title.is_empty(), "a single pane needs no title row");
        assert_eq!(pane.editor.x, area.x);
        assert_eq!(pane.editor.y, layout.scenes.bottom());
        assert_eq!(pane.editor.bottom(), layout.footer.y);
        assert_eq!(pane.editor.width + pane.minimap.width, area.width);
        assert_eq!(pane.minimap.right(), area.right());
        assert!(layout.reference.is_empty());
    }

    /// Without the minimap a pane keeps a one-cell scrollbar at its right
    /// edge, and its handle sits where the page is in the text.
    /// The visuals panel takes the outer edge, the set panel sits inside
    /// it on the same side and across from it otherwise, and the visuals
    /// panel is the first to yield when the panes would be squeezed.
    /// The mixer's band is carved first, outermost, along its edge: a
    /// visuals band across the same edge sits inside it, the set panel
    /// and the reference stop at it, and a terminal too short for it
    /// gives the desk no room rather than the score none.
    #[test]
    fn the_mixer_band_is_outermost_and_the_reference_stops_at_it() {
        use super::super::viz_panel::{Dock, Edge};
        let area = Rect::new(0, 0, 140, 50);
        let mixer = Some(Dock {
            edge: Edge::Bottom,
            extent: 16,
        });
        let layout = regions_with(
            area,
            false,
            1,
            true,
            false,
            Some(Side::Right),
            [
                None,
                Some(Dock {
                    edge: Edge::Bottom,
                    extent: 8,
                }),
            ],
            mixer,
            None,
        );
        // The footer takes the last two rows; the desk the sixteen above.
        assert_eq!(layout.mixer, Rect::new(0, 32, 140, 16));
        assert_eq!(
            layout.viz[1],
            Rect::new(0, 24, 140, 8),
            "the visuals band inside it"
        );
        assert_eq!(
            layout.sidebar.bottom(),
            24,
            "the set panel stops at the bands"
        );
        assert_eq!(
            layout.reference.bottom(),
            32,
            "the reference stops at the desk"
        );
        assert_eq!(layout.panes[0].editor.bottom(), 24);
        let top = regions_with(
            area,
            false,
            1,
            false,
            false,
            None,
            [None, None],
            Some(Dock {
                edge: Edge::Top,
                extent: 12,
            }),
            None,
        );
        assert_eq!(top.mixer, Rect::new(0, 3, 140, 12));
        assert_eq!(top.panes[0].editor.y, 15);
        let short = regions_with(
            Rect::new(0, 0, 100, 12),
            false,
            1,
            false,
            false,
            None,
            [None, None],
            mixer,
            None,
        );
        assert!(short.mixer.is_empty(), "no room: the score keeps its rows");
        let without = regions_with(
            Rect::new(0, 0, 100, 12),
            false,
            1,
            false,
            false,
            None,
            [None, None],
            None,
            None,
        );
        assert_eq!(short.panes[0].editor, without.panes[0].editor);
    }

    /// The docked log draws in the room the layout carved, not over the
    /// screen.
    #[test]
    fn a_docked_log_draws_in_its_carved_room_not_over_the_screen() {
        let area = Rect::new(0, 0, 120, 40);
        let footer = Rect::new(0, 38, 120, 2);
        let carved = Rect::new(0, 29, 120, 9);
        let mut panel = LogPanel::opened(false);

        // A sheet takes the frame short of the footer, whatever was carved.
        assert!(!panel.sticky);
        let sheet = log_room(&panel, carved, footer, area);
        assert_eq!(sheet, Rect::new(0, 0, 120, 38), "a sheet is a sheet");

        // Docked, it takes its room and leaves the rest of the screen be.
        panel.sticky = true;
        let docked = log_room(&panel, carved, footer, area);
        assert_eq!(docked, carved, "the layout said where it goes");
        assert!(
            docked.height < area.height / 2,
            "and it is a band, not the screen: {docked:?}"
        );

        // Sticky but with nothing carved - a terminal too short for another
        // band - falls back rather than drawing into an empty rect.
        let squeezed = log_room(&panel, Rect::default(), footer, area);
        assert_eq!(squeezed, Rect::new(0, 0, 120, 38), "no room, so a sheet");
    }

    /// A sounding mark never hides a syntax error: a note that plays inside
    /// a broken call does not repaint the foreground that the linter set.
    #[test]
    fn a_sounding_mark_never_paints_over_a_syntax_error() {
        use super::super::visuals::SourceMark;
        let mut editor = Editor::new(
            "$: chord(\"Db2\")
",
        )
        .unwrap();
        let area = Rect::new(0, 0, 30, 2);
        let grid = source_grid(&editor, area, true);
        editor.set_view_size(usize::from(grid.width), usize::from(grid.height));
        let map = editor.screen_map(grid).expect("a map");
        let theme = Theme::default();
        // The call's own span, sounding and broken at the same time.
        let span = (3usize, 8usize);
        let marks = [SourceMark {
            from: span.0,
            to: span.1,
            color: theme.event,
            onset_id: 1,
            strength: 1.0,
        }];
        let errors = [span];

        let paint = |active: &[SourceMark], errors: &[(usize, usize)]| {
            let mut buffer = Buffer::empty(area);
            EditorPane {
                editor: &editor,
                map: &map,
                line_numbers: false,
                visual: &VisualState::default(),
                decorations: Decorations {
                    active,
                    mini: &[],
                    sliders: &[],
                    errors,
                    brackets: &[],
                    overridden: &[],
                    ..Decorations::default()
                },
                theme: &theme,
                area,
            }
            .render(area, &mut buffer);
            (0..area.width)
                .filter_map(|x| buffer.cell((x, 0)).map(|cell| cell.style()))
                .collect::<Vec<_>>()
        };

        let broken = paint(&[], &errors);
        assert!(
            broken.iter().any(|style| {
                style.underline_color == Some(theme.error)
                    && style.add_modifier.contains(Modifier::UNDERLINED)
            }),
            "the linter marks the call at all: {broken:?}"
        );
        let both = paint(&marks, &errors);
        assert!(
            both.iter().any(|style| {
                style.underline_color == Some(theme.error)
                    && style.add_modifier.contains(Modifier::UNDERLINED)
            }),
            "and the sounding mark does not take that away: {both:?}"
        );
    }

    /// Every fixture at once: two visuals docks, the set panel's column,
    /// the mixer's desk and a sticky log, all on screen together and none
    /// of them over another.
    #[test]
    fn all_five_fixtures_dock_at_once_without_overlapping() {
        use super::super::viz_panel::{Dock, Edge};
        let area = Rect::new(0, 0, 200, 60);
        let regions = regions_with(
            area,
            false,
            1,
            true,
            true,
            Some(Side::Left),
            [
                Some(Dock {
                    edge: Edge::Top,
                    extent: 6,
                }),
                Some(Dock {
                    edge: Edge::Right,
                    extent: 24,
                }),
            ],
            Some(Dock {
                edge: Edge::Bottom,
                extent: 12,
            }),
            Some(Dock {
                edge: Edge::Bottom,
                extent: 8,
            }),
        );
        let rooms = [
            ("visuals 1", regions.viz[0]),
            ("visuals 2", regions.viz[1]),
            ("set panel", regions.sidebar),
            ("mixer", regions.mixer),
            ("log", regions.log),
            ("the score", regions.panes[0].editor),
        ];
        for (name, room) in rooms {
            assert!(!room.is_empty(), "{name} got no room: {rooms:?}");
        }
        for (index, (name, room)) in rooms.iter().enumerate() {
            for (other_name, other) in rooms.iter().skip(index + 1) {
                assert!(
                    room.intersection(*other).is_empty(),
                    "{name} draws over {other_name}: {room:?} and {other:?}"
                );
            }
        }
    }

    /// A sticky log carves at the mixer's own outermost tier, so the two
    /// fixtures stack rather than draw over one another wherever the log's
    /// own edge is asked to move to.
    #[test]
    fn a_sticky_logs_dock_stacks_outside_the_mixer_and_moves_with_its_edge() {
        use super::super::viz_panel::{BAND_DEFAULT_HEIGHT, Dock, Edge};
        let area = Rect::new(0, 0, 140, 50);
        let mixer = Some(Dock {
            edge: Edge::Bottom,
            extent: 16,
        });
        let log = Some(Dock {
            edge: Edge::Bottom,
            extent: BAND_DEFAULT_HEIGHT,
        });
        let bottomed = regions_with(area, false, 1, false, false, None, [None, None], mixer, log);
        assert!(!bottomed.mixer.is_empty());
        assert!(!bottomed.log.is_empty());
        assert_eq!(
            bottomed.mixer.bottom(),
            bottomed.footer.y,
            "the desk keeps the very bottom"
        );
        assert_eq!(
            bottomed.log.bottom(),
            bottomed.mixer.y,
            "the log stacks directly above it, not over it"
        );
        // `e` moves the log to another edge; the desk does not move with it.
        let topped = regions_with(
            area,
            false,
            1,
            false,
            false,
            None,
            [None, None],
            mixer,
            Some(Dock {
                edge: Edge::Top,
                extent: BAND_DEFAULT_HEIGHT,
            }),
        );
        assert_eq!(
            topped.log.y,
            topped.scenes.bottom(),
            "moved to the top, the log sits right under the strip"
        );
        assert_eq!(topped.mixer.bottom(), topped.footer.y, "the desk stays put");
        assert!(
            topped.log.bottom() <= topped.mixer.y,
            "still not over the desk"
        );
        assert_ne!(bottomed.log, topped.log, "the region actually moved");
    }

    /// The log is only ever a band. A left or right edge lays it along the
    /// bottom. A band that would take the panes' least rows shrinks instead
    /// of being dropped. Under a band's least height there is no band and
    /// the log is a sheet.
    #[test]
    fn a_sticky_log_is_a_band_that_shrinks_to_leave_the_panes_their_least() {
        use super::super::viz_panel::{Dock, Edge};
        let area = Rect::new(0, 0, 140, 40);
        let log = |edge, extent| Some(Dock { edge, extent });
        for side in [Edge::Left, Edge::Right] {
            let laid = regions_with(
                area,
                false,
                1,
                false,
                false,
                None,
                [None, None],
                None,
                log(side, 13),
            );
            assert_eq!(
                laid.log,
                Rect::new(0, laid.footer.y - 13, 140, 13),
                "{side:?} is read as the bottom"
            );
        }

        // Forty rows less the chrome and a sixteen-row desk leave nineteen,
        // and the panes keep eight of them.
        let desk = Some(Dock {
            edge: Edge::Bottom,
            extent: 16,
        });
        let beside = regions_with(
            area,
            false,
            1,
            false,
            false,
            None,
            [None, None],
            desk,
            log(Edge::Bottom, 13),
        );
        assert_eq!(beside.log.height, 11, "shrunk, not dropped");
        assert_eq!(beside.log.bottom(), beside.mixer.y, "inside the desk");
        assert_eq!(
            beside.panes[0].editor.height, 8,
            "the panes keep their least"
        );

        let cramped = regions_with(
            Rect::new(0, 0, 140, 17),
            false,
            1,
            false,
            false,
            None,
            [None, None],
            None,
            log(Edge::Bottom, 13),
        );
        assert!(cramped.log.is_empty(), "no band under a band's least");
    }

    /// The memory breakdown carves at the fixtures' tier, last of the
    /// three: on the edge the desk and the log share it stacks inside
    /// both, nearest the score, and the room running short it is the one
    /// that shrinks. Across the top it sits under the strip. In zen, where
    /// nothing is docked, it is a band over the score, stacked inside the
    /// desk's own zen band on a shared edge.
    #[test]
    fn the_memory_breakdown_stacks_innermost_of_the_fixtures() {
        use super::super::viz_panel::{Dock, Edge};
        let band = |edge, extent| Some(Dock { edge, extent });
        let area = Rect::new(0, 0, 160, 60);
        let lay = |area, zen, memory| {
            regions_with_footer(
                area,
                ChromeLayout::shown(zen),
                1,
                true,
                false,
                None,
                [None, None],
                band(Edge::Bottom, 16),
                band(Edge::Bottom, 13),
                memory,
                false,
                0,
                false,
            )
        };

        let stacked = lay(area, false, band(Edge::Bottom, 11));
        assert_eq!(stacked.mixer.bottom(), stacked.footer.y);
        assert_eq!(stacked.log.bottom(), stacked.mixer.y);
        assert_eq!(stacked.memory, Rect::new(0, stacked.log.y - 11, 160, 11));
        assert_eq!(
            stacked.reference.bottom(),
            stacked.memory.y,
            "the reference stops at it"
        );

        let topped = lay(area, false, band(Edge::Top, 11));
        assert_eq!(topped.memory.y, topped.scenes.bottom());
        assert_eq!(topped.panes[0].editor.y, topped.memory.bottom());
        assert_eq!(topped.log, stacked.log, "the log does not move with it");

        // Forty rows: thirty-five of body, the desk's sixteen and the
        // log's eleven - shrunk from thirteen - leave the panes their
        // eight, and no band's least for the breakdown.
        let short = lay(Rect::new(0, 0, 160, 40), false, band(Edge::Bottom, 11));
        assert!(short.memory.is_empty(), "the first to give up");
        assert_eq!(short.log.height, 11);
        let roomier = lay(Rect::new(0, 0, 160, 48), false, band(Edge::Bottom, 11));
        assert_eq!(roomier.log.height, 13, "the log keeps its rows");
        assert_eq!(roomier.memory.height, 6, "while the breakdown shrinks");

        let zen = lay(area, true, band(Edge::Bottom, 11));
        assert_eq!(zen.mixer.bottom(), 60);
        assert_eq!(zen.memory, Rect::new(0, zen.mixer.y - 11, 160, 11));
        assert_eq!(zen.panes[0].editor.height, 60, "laid over the score");
    }

    #[test]
    fn the_visuals_panel_stacks_outside_the_set_panel() {
        use super::super::set_panel::SIDEBAR_WIDTH;
        use super::super::viz_panel::VIZ_WIDTH;
        let both = regions(
            Rect::new(0, 0, 140, 40),
            false,
            1,
            false,
            true,
            Some(Side::Right),
            Some(Side::Right),
        );
        assert_eq!(both.viz[0], Rect::new(140 - VIZ_WIDTH, 3, VIZ_WIDTH, 35));
        assert_eq!(
            both.sidebar.right(),
            both.viz[0].x,
            "the set panel inside the visuals"
        );
        assert_eq!(both.sidebar.width, SIDEBAR_WIDTH);
        assert_eq!(both.panes[0].editor.x, 0);
        let apart = regions(
            Rect::new(0, 0, 140, 40),
            false,
            1,
            false,
            true,
            Some(Side::Left),
            Some(Side::Right),
        );
        assert_eq!(apart.sidebar.x, 0);
        assert_eq!(apart.viz[0].right(), 140);
        assert_eq!(apart.panes[0].editor.x, SIDEBAR_WIDTH);
        let left = regions(
            Rect::new(0, 0, 140, 40),
            false,
            1,
            false,
            true,
            Some(Side::Left),
            Some(Side::Left),
        );
        assert_eq!(left.viz[0].x, 0);
        assert_eq!(left.sidebar.x, VIZ_WIDTH);
        assert_eq!(left.viz_edge[0], super::super::viz_panel::Edge::Left);
        // Ninety columns hold the set panel and a pane, not the visuals.
        let tight = regions(
            Rect::new(0, 0, 90, 40),
            false,
            1,
            false,
            true,
            Some(Side::Right),
            Some(Side::Right),
        );
        assert!(tight.viz[0].is_empty(), "the ornament yields first");
        assert!(!tight.sidebar.is_empty());
        // Alone, the visuals panel needs only its own width beside a pane.
        let alone = regions(
            Rect::new(0, 0, 90, 40),
            false,
            1,
            false,
            true,
            None,
            Some(Side::Right),
        );
        assert_eq!(alone.viz[0].width, VIZ_WIDTH);
        assert_eq!(
            alone.panes[0].editor.width + alone.panes[0].minimap.width,
            90 - VIZ_WIDTH
        );
        // A band runs the whole width across the top, over the columns,
        // and the second dock sits inside the first when both are columns
        // on one side; a band the body cannot spare yields.
        use super::super::viz_panel::{Dock, Edge};
        let banded = regions_with(
            Rect::new(0, 0, 140, 40),
            false,
            1,
            false,
            true,
            Some(Side::Left),
            [
                Some(Dock {
                    edge: Edge::Right,
                    extent: VIZ_WIDTH,
                }),
                Some(Dock {
                    edge: Edge::Top,
                    extent: 10,
                }),
            ],
            None,
            None,
        );
        assert_eq!(banded.viz[1], Rect::new(0, 3, 140, 10));
        assert_eq!(banded.viz[0], Rect::new(140 - VIZ_WIDTH, 13, VIZ_WIDTH, 25));
        assert_eq!(banded.sidebar.y, 13);
        assert_eq!(banded.viz_edge, [Edge::Right, Edge::Top]);
        let stacked = regions_with(
            Rect::new(0, 0, 160, 40),
            false,
            1,
            false,
            true,
            None,
            [
                Some(Dock {
                    edge: Edge::Right,
                    extent: VIZ_WIDTH,
                }),
                Some(Dock {
                    edge: Edge::Right,
                    extent: VIZ_WIDTH,
                }),
            ],
            None,
            None,
        );
        assert_eq!(stacked.viz[0].x, 160 - VIZ_WIDTH);
        assert_eq!(
            stacked.viz[1].right(),
            stacked.viz[0].x,
            "the second inside the first"
        );
        let short = regions_with(
            Rect::new(0, 0, 140, 14),
            false,
            1,
            false,
            true,
            None,
            [
                None,
                Some(Dock {
                    edge: Edge::Bottom,
                    extent: 10,
                }),
            ],
            None,
            None,
        );
        assert!(
            short.viz[1].is_empty(),
            "no body left under a band that tall"
        );
        // The reference keeps the stage's full height over a band across
        // the bottom, rather than being cut short by it.
        let over = regions_with(
            Rect::new(0, 0, 140, 40),
            false,
            1,
            true,
            true,
            None,
            [
                None,
                Some(Dock {
                    edge: Edge::Bottom,
                    extent: 9,
                }),
            ],
            None,
            None,
        );
        assert_eq!(over.viz[1].height, 9);
        assert_eq!(
            over.reference.bottom(),
            over.viz[1].bottom(),
            "down to the footer"
        );
        assert_eq!(
            over.panes[0].editor.bottom(),
            over.viz[1].y,
            "the pane stops at the band"
        );
    }

    /// The set panel docks at the left when the body can spare it beside
    /// the panes, and takes nothing when it cannot: the app shows the
    /// sheet form then. The reference column is measured against what
    /// the panel left.
    #[test]
    fn the_set_panel_docks_at_the_left_when_there_is_room() {
        let docked = regions(
            Rect::new(0, 0, 120, 40),
            false,
            1,
            false,
            true,
            Some(Side::Left),
            None,
        );
        assert_eq!(
            docked.sidebar,
            Rect::new(0, 3, super::super::set_panel::SIDEBAR_WIDTH, 35)
        );
        assert_eq!(
            docked.panes[0].editor.x,
            super::super::set_panel::SIDEBAR_WIDTH
        );
        assert_eq!(
            docked.panes[0].editor.width + docked.panes[0].minimap.width,
            120 - super::super::set_panel::SIDEBAR_WIDTH
        );
        let closed = regions(Rect::new(0, 0, 120, 40), false, 1, false, true, None, None);
        assert!(closed.sidebar.is_empty());
        assert_eq!(closed.panes[0].editor.x, 0);
        // Two panes need twice the room; sixty columns cannot spare it.
        let narrow = regions(
            Rect::new(0, 0, 60, 40),
            false,
            1,
            false,
            true,
            Some(Side::Left),
            None,
        );
        assert!(
            narrow.sidebar.is_empty(),
            "too narrow: the sheet form instead"
        );
        let split = regions(
            Rect::new(0, 0, 100, 40),
            false,
            2,
            false,
            true,
            Some(Side::Left),
            None,
        );
        assert!(split.sidebar.is_empty());
        let wide_split = regions(
            Rect::new(0, 0, 140, 40),
            false,
            2,
            false,
            true,
            Some(Side::Left),
            None,
        );
        assert!(!wide_split.sidebar.is_empty());
        assert_eq!(
            wide_split.panes[0].title.x,
            super::super::set_panel::SIDEBAR_WIDTH
        );
        // Beside the reference column the panel keeps its place.
        let both = regions(
            Rect::new(0, 0, 160, 40),
            false,
            1,
            true,
            true,
            Some(Side::Left),
            None,
        );
        assert!(!both.sidebar.is_empty());
        assert!(both.reference.x > both.sidebar.right());
        // At the right, the panel holds the edge and the reference column
        // sits inside it, taking its room from the pane.
        let right = regions(
            Rect::new(0, 0, 160, 40),
            false,
            1,
            true,
            true,
            Some(Side::Right),
            None,
        );
        assert_eq!(right.sidebar_side, Side::Right);
        assert_eq!(
            right.sidebar,
            Rect::new(
                160 - super::super::set_panel::SIDEBAR_WIDTH,
                3,
                super::super::set_panel::SIDEBAR_WIDTH,
                35
            )
        );
        assert_eq!(right.panes[0].editor.x, 0);
        assert_eq!(
            right.reference.right(),
            right.sidebar.x,
            "the reference stops at the panel"
        );
        assert!(right.panes[0].editor.width < both.panes[0].editor.width + 1);
        // Too narrow for both beside the pane, the set temporarily yields.
        let squeezed = regions(
            Rect::new(0, 0, 100, 40),
            false,
            1,
            true,
            true,
            Some(Side::Right),
            None,
        );
        assert!(squeezed.sidebar.is_empty());
        assert!(squeezed.sidebar_hidden);
        assert!(!squeezed.reference_overlays);
        assert_eq!(squeezed.reference.right(), 100);
    }

    #[test]
    fn reference_reclaims_secondary_columns_before_covering_the_editor() {
        use super::super::viz_panel::{Dock, Edge};
        let sidebar = Some(SetSidebar {
            side: Side::Right,
            width: 48,
        });
        let column = Some(Dock {
            edge: Edge::Right,
            extent: 32,
        });
        let layout = |width, reference, docks, log| {
            regions_with_footer(
                Rect::new(0, 0, width, 40),
                ChromeLayout::shown(false),
                2,
                reference,
                false,
                sidebar,
                docks,
                None,
                log,
                None,
                false,
                0,
                false,
            )
        };
        let wide = layout(300, true, [column, None], None);
        assert!(!wide.viz[0].is_empty());
        assert!(!wide.sidebar.is_empty());
        let medium = layout(220, true, [column, None], None);
        assert!(medium.viz[0].is_empty());
        assert!(!medium.sidebar.is_empty());
        let narrow = layout(180, true, [column, None], None);
        assert!(narrow.viz[0].is_empty());
        assert!(narrow.sidebar_hidden);
        for regions in [&wide, &medium, &narrow] {
            assert!(!regions.reference_overlays);
            for pane in &regions.panes[..regions.pane_count] {
                assert!(pane.editor.width >= 49);
                assert!(pane.editor.right() <= regions.reference.x);
            }
        }
        let closed = layout(180, false, [column, None], None);
        assert!(!closed.viz[0].is_empty());
        assert!(!closed.sidebar_hidden);
        assert!(!closed.sidebar.is_empty());
        let band = Some(Dock {
            edge: Edge::Top,
            extent: 6,
        });
        let log = Some(Dock {
            edge: Edge::Bottom,
            extent: 8,
        });
        let with_bands = layout(180, true, [band, None], log);
        assert!(
            !with_bands.viz[0].is_empty(),
            "horizontal bands keep their space"
        );
        assert_eq!(
            with_bands.log.height, 8,
            "the docked log is only ever a band, and keeps its rows too"
        );
        assert!(!with_bands.reference_overlays);
    }

    /// The rule under the tabs keeps a pane's title from reading as a
    /// second row of tabs. A band docked across the top already separates
    /// the two, so the layout leaves the rule out.
    #[test]
    fn a_band_across_the_top_already_ends_the_tabs() {
        use super::super::viz_panel::{Dock, Edge};
        let area = Rect::new(0, 0, 160, 40);
        let plain = regions_with(area, false, 2, false, false, None, [None, None], None, None);
        assert_eq!(plain.scenes.height, 2, "a split alone gets the rule");

        let top_band = Dock {
            edge: Edge::Top,
            extent: 12,
        };
        let with_mixer = regions_with(
            area,
            false,
            2,
            false,
            false,
            None,
            [None, None],
            Some(top_band),
            None,
        );
        assert_eq!(
            with_mixer.scenes.height, 1,
            "the desk's own edge ends the tabs"
        );
        assert_eq!(
            with_mixer.mixer.y,
            with_mixer.scenes.bottom(),
            "and it starts directly under them"
        );

        let with_viz = regions_with(
            area,
            false,
            2,
            false,
            false,
            None,
            [Some(top_band), None],
            None,
            None,
        );
        assert_eq!(with_viz.scenes.height, 1, "so does a visuals band");

        let with_log = regions_with(
            area,
            false,
            2,
            false,
            false,
            None,
            [None, None],
            None,
            Some(top_band),
        );
        assert_eq!(with_log.scenes.height, 1, "and so does a sticky log's");

        // A band along the BOTTOM is not between the two, so the rule
        // stays: the panes still start directly under the tabs.
        let below = regions_with(
            area,
            false,
            2,
            false,
            false,
            None,
            [None, None],
            Some(Dock {
                edge: Edge::Bottom,
                extent: 12,
            }),
            None,
        );
        assert_eq!(below.scenes.height, 2, "a band underneath changes nothing");

        // A band asked for on a terminal with no room for it is not laid,
        // and the rule it would have replaced is not taken away.
        let cramped = regions_with(
            Rect::new(0, 0, 160, 13),
            false,
            2,
            false,
            false,
            None,
            [None, None],
            Some(Dock {
                edge: Edge::Top,
                extent: 12,
            }),
            None,
        );
        assert!(cramped.mixer.is_empty(), "no room for the desk");
        assert_eq!(
            cramped.scenes.height, 2,
            "so the rule that ends the tabs stays"
        );
    }

    #[test]
    fn two_panes_share_the_body_and_get_title_rows() {
        let area = Rect::new(0, 0, 160, 40);
        let layout = regions(area, false, 2, false, true, None, None);
        assert_eq!(layout.pane_count, 2);
        // A split gives the strip a rule under it, and the panes start
        // below that: with one pane there is nothing to separate.
        assert_eq!(layout.scenes.height, 2, "the strip and its rule");
        assert_eq!(
            regions(area, false, 1, false, true, None, None)
                .scenes
                .height,
            1,
            "one pane keeps the row for the source"
        );
        let [left, right] = layout.panes;
        assert_eq!(layout.menu, Rect::new(0, 0, 160, 1));
        assert_eq!(left.title, Rect::new(0, 4, 80, 1));
        assert_eq!(right.title, Rect::new(80, 4, 80, 1));
        assert_eq!(left.editor.y, 5);
        assert_eq!(
            left.editor.width + left.minimap.width + left.scrollbar.width,
            80
        );
        assert_eq!(
            right.editor.width + right.minimap.width + right.scrollbar.width,
            80
        );
        assert_eq!(
            right.scrollbar.right(),
            area.right(),
            "the bar keeps the edge without a minimap"
        );
        assert_eq!(layout.pane_at(10, 10), Some(0));
        assert_eq!(layout.pane_at(100, 10), Some(1));
        assert_eq!(layout.pane_at(100, 0), None, "the header is no pane");
    }

    #[test]
    fn the_reference_takes_a_column_when_there_is_room_and_overlays_otherwise() {
        let wide = regions(Rect::new(0, 0, 200, 40), false, 2, true, true, None, None);
        assert!(!wide.reference.is_empty());
        assert!(!wide.reference_overlays);
        assert_eq!(wide.reference.right(), 200);
        assert_eq!(wide.panes[1].minimap.right(), wide.reference.x);
        assert!(wide.panes[0].editor.width >= PANE_MIN_WIDTH_BESIDE_REFERENCE);

        let narrow = regions(Rect::new(0, 0, 100, 40), false, 2, true, true, None, None);
        assert!(narrow.reference_overlays);
        assert_eq!(
            narrow.panes[1].minimap.right(),
            100,
            "panes keep their width"
        );
        assert!(narrow.reference.intersects(narrow.panes[1].editor));

        let single = regions(Rect::new(0, 0, 100, 40), false, 1, true, true, None, None);
        assert!(!single.reference_overlays);
        assert_eq!(single.panes[0].minimap.right(), single.reference.x);
    }

    #[test]
    fn the_minimap_only_appears_when_the_source_pane_is_wide_enough() {
        let wide = regions(Rect::new(0, 0, 160, 40), false, 1, false, true, None, None).panes[0];
        assert!(wide.minimap.width >= 6);
        assert_eq!(wide.minimap.x, wide.editor.right());

        let narrow = regions(Rect::new(0, 0, 60, 20), false, 1, false, true, None, None).panes[0];
        assert!(narrow.minimap.is_empty());
        assert_eq!(narrow.editor.width, 60);
    }

    #[test]
    fn wide_layout_math_cannot_overflow_u16() {
        let area = Rect::new(0, 0, u16::MAX, 30);
        let pane = regions(area, false, 1, false, true, None, None).panes[0];
        assert_eq!(pane.editor.width + pane.minimap.width, u16::MAX);
        let split = regions(area, false, 2, true, true, None, None);
        assert_eq!(
            split.panes[0].editor.width
                + split.panes[0].minimap.width
                + split.panes[1].editor.width
                + split.panes[1].minimap.width
                + split.reference.width,
            u16::MAX
        );
    }

    #[test]
    fn zen_is_exactly_the_editor_area_with_no_chrome_at_all() {
        let area = Rect::new(2, 3, 160, 40);
        let layout = regions(area, true, 1, false, true, None, None);
        assert!(layout.menu.is_empty(), "zen has no menu bar either");
        assert_eq!(layout.pane_count, 1);
        assert_eq!(layout.panes[0].editor, area);
        assert!(layout.reference.is_empty());
        assert!(layout.header.is_empty());
        assert!(layout.scenes.is_empty());
        assert!(layout.footer.is_empty());
        assert!(layout.panes[0].minimap.is_empty());
        assert!(layout.sidebar.is_empty());
        assert!(layout.mixer.is_empty());
        assert!(layout.viz.iter().all(|rect| rect.is_empty()));
    }

    #[test]
    fn zen_opens_the_reference_as_an_overlay_without_shrinking_the_editor() {
        let area = Rect::new(2, 3, 160, 40);
        let layout = regions(area, true, 1, true, true, None, None);

        assert_eq!(layout.panes[0].editor, area);
        assert_eq!(layout.reference, Rect::new(102, 3, 60, 40));
        assert!(layout.reference_overlays);
        assert!(layout.header.is_empty());
        assert!(layout.footer.is_empty());
    }

    /// ^E's split is not chrome, so zen keeps it: two panes share the
    /// width exactly as they do outside zen, and the strip that would
    /// name them stays gone.
    #[test]
    fn zen_with_a_split_still_shows_two_panes_side_by_side() {
        let area = Rect::new(0, 0, 160, 40);
        let layout = regions(area, true, 2, true, true, None, None);
        assert_eq!(layout.pane_count, 2);
        assert!(layout.scenes.is_empty(), "the tabs stay hidden in zen");
        assert!(layout.menu.is_empty());
        assert!(layout.header.is_empty());
        assert!(layout.footer.is_empty());
        assert_eq!(layout.panes[0].editor.x, area.x);
        assert_eq!(layout.panes[0].editor.y, area.y);
        assert_eq!(layout.panes[0].editor.height, area.height);
        assert_eq!(layout.panes[1].editor.x, layout.panes[0].editor.right());
        assert_eq!(layout.panes[1].editor.right(), area.right());
        assert_eq!(
            layout.panes[0].editor.width + layout.panes[1].editor.width,
            area.width
        );
        assert!(layout.panes[0].minimap.is_empty(), "no minimap in zen");
        assert!(layout.panes[0].title.is_empty(), "no per-pane title either");
    }

    /// The mixer has no sheet form of its own, only a band. Zen lays that
    /// band over the whole frame instead of carving it out of the body, so
    /// the pane behind it never shrinks.
    #[test]
    fn zen_pops_the_mixer_up_over_the_editor_instead_of_docking_it() {
        use super::super::viz_panel::{Dock, Edge};
        let area = Rect::new(0, 0, 140, 40);
        let mixer = Some(Dock {
            edge: Edge::Bottom,
            extent: 16,
        });
        let layout = regions_with(area, true, 1, false, false, None, [None, None], mixer, None);
        assert_eq!(
            layout.panes[0].editor, area,
            "the editor keeps the whole frame under the desk"
        );
        assert_eq!(
            layout.mixer,
            Rect::new(0, 24, 140, 16),
            "the desk still lands where its own edge says"
        );
        // The set panel's sidebar is left empty on purpose: an empty
        // sidebar is what tells `SetPanel` to draw its sheet form.
        assert!(layout.sidebar.is_empty());
        // The visuals docks are refused in zen outright - see
        // `App::toggle_viz_dock` - so zen never even asks for their room.
        let top = regions_with(
            area,
            true,
            1,
            false,
            false,
            None,
            [None, None],
            Some(Dock {
                edge: Edge::Top,
                extent: 12,
            }),
            None,
        );
        assert_eq!(top.mixer, Rect::new(0, 0, 140, 12));
        assert_eq!(top.panes[0].editor, area);
    }

    /// The menu bar, the PLAYING header and the meter/orbits footer are
    /// independent of zen: hiding one does not hide the others, and zen still
    /// hides all of them.
    #[test]
    fn chrome_preferences_hide_the_menu_header_and_footer_independently_of_zen() {
        let area = Rect::new(0, 0, 160, 40);
        let lay = |chrome: ChromeLayout| {
            regions_with_footer(
                area,
                chrome,
                1,
                false,
                false,
                None,
                [None, None],
                None,
                None,
                None,
                false,
                0,
                false,
            )
        };
        let full = lay(ChromeLayout::shown(false));
        assert_eq!(full.menu.height, 1);
        assert_eq!(full.header.height, 1);
        assert_eq!(full.footer.height, 2);
        assert_eq!(full.scenes.height, 1, "the tabs are not the header");

        let no_menu = lay(ChromeLayout {
            zen: false,
            menu: false,
            header: true,
            footer: true,
            beat_line: false,
        });
        assert!(no_menu.menu.is_empty());
        assert_eq!(no_menu.header.height, 1);
        assert_eq!(
            no_menu.header.y, area.y,
            "the header moves up into the bar's row"
        );
        assert_eq!(no_menu.footer.height, 2);
        assert_eq!(no_menu.scenes.height, 1);

        let no_header = lay(ChromeLayout {
            zen: false,
            menu: true,
            header: false,
            footer: true,
            beat_line: false,
        });
        assert_eq!(no_header.menu.height, 1);
        assert!(no_header.header.is_empty());
        assert_eq!(no_header.scenes.y, no_header.menu.bottom());
        assert_eq!(no_header.footer.height, 2);

        let no_footer = lay(ChromeLayout {
            zen: false,
            menu: true,
            header: true,
            footer: false,
            beat_line: false,
        });
        assert_eq!(no_footer.menu.height, 1);
        assert_eq!(no_footer.header.height, 1);
        assert_eq!(
            no_footer.footer.height, 1,
            "the status line stays when the meter row is hidden"
        );
        assert_eq!(no_footer.footer.bottom(), area.bottom());

        let zen_over_hidden = lay(ChromeLayout {
            zen: true,
            menu: false,
            header: false,
            footer: false,
            beat_line: false,
        });
        assert!(zen_over_hidden.menu.is_empty());
        assert!(zen_over_hidden.header.is_empty());
        assert!(zen_over_hidden.footer.is_empty());
        assert_eq!(zen_over_hidden.panes[0].editor, area);
    }

    /// Hiding the footer takes only the meter and chip row: notices and
    /// retained piano notes keep their rows above the status line.
    #[test]
    fn a_hidden_footer_keeps_the_notice_rows_above_the_status_line() {
        let area = Rect::new(0, 0, 160, 40);
        let footer = |chrome: ChromeLayout, notices, piano| {
            regions_with_footer(
                area,
                chrome,
                1,
                false,
                false,
                None,
                [None, None],
                None,
                None,
                None,
                false,
                notices,
                piano,
            )
            .footer
        };
        let hidden = ChromeLayout {
            zen: false,
            menu: true,
            header: true,
            footer: false,
            beat_line: false,
        };
        let shown = ChromeLayout::shown(false);
        for (notices, piano) in [(0, false), (1, false), (2, false), (0, true), (2, true)] {
            let rows = notices + u16::from(piano);
            assert_eq!(footer(hidden, notices, piano).height, 1 + rows);
            assert_eq!(footer(shown, notices, piano).height, 2 + rows);
            assert_eq!(footer(hidden, notices, piano).bottom(), area.bottom());
        }
    }

    /// An error does not hide the audio advice, and retained piano notes
    /// keep their row, with the footer hidden as with it shown.
    #[test]
    fn a_hidden_footer_still_shows_every_notice_and_the_caret_without_the_meter() {
        UiSettings::default().apply();
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.show_footer = false;
        chrome.status = "ready";
        chrome.error = Some("evaluation failed");
        chrome.audio_warning = Some("audio dropouts: raise latency");
        chrome.piano_notes = Some("Notes  C4 + E4 · [60,64]");
        chrome.caret = Some((3, 8));
        chrome.device = Some("Speakers");
        let area = Rect::new(0, 0, 120, 4);
        let mut buffer = Buffer::empty(area);
        Footer { chrome: &chrome }.render(area, &mut buffer);
        let row = |y| {
            (0..area.width)
                .map(|x| buffer.cell((x, y)).unwrap().symbol())
                .collect::<String>()
        };
        assert!(row(0).contains("evaluation failed"), "{}", row(0));
        assert!(row(1).contains("Notes  C4 + E4 · [60,64]"), "{}", row(1));
        assert!(row(2).contains("audio dropouts"), "{}", row(2));
        assert!(row(3).contains("ready"), "{}", row(3));
        assert!(row(3).trim_end().ends_with("3,8"), "{}", row(3));
        let text = text_of(&buffer);
        assert!(!text.contains("MASTER"), "{text}");
        assert!(!text.contains("Speakers"), "{text}");
    }

    #[test]
    fn the_dock_sits_at_the_bottom_right_with_the_scope_beside_it() {
        let area = Rect::new(0, 20, 150, 2);
        let hits = footer_hits(area, Some("Speakers"), None, midi_ports(0, 0), 0, &[]);
        assert_eq!(hits.dock.right(), area.right() - 1);
        assert_eq!(hits.dock.height, 2);
        assert_eq!(hits.dock.width, DOCK_WIDTH);
        assert_eq!(hits.scope.right() + 1, hits.dock.x);
        assert_eq!(hits.scope.width, SCOPE_WIDTH);
        assert!(!hits.meter.is_empty());
        assert!(hits.meter.y == area.y);
        assert!(!hits.meter.intersects(hits.device_chip));

        // A narrow footer narrows the dock and keeps the meter; the scope
        // stays, taking what is free (see the narrow-row test below).
        let narrow = footer_hits(
            Rect::new(0, 20, 90, 2),
            None,
            None,
            midi_ports(0, 0),
            0,
            &[],
        );
        assert_eq!(narrow.dock.width, DOCK_WIDTH_NARROW);
        assert!(!narrow.scope.is_empty());
        assert!(!narrow.meter.is_empty());

        // A one-row footer gets a one-row dock.
        let short = footer_hits(
            Rect::new(0, 20, 150, 1),
            None,
            None,
            midi_ports(0, 0),
            0,
            &[],
        );
        assert_eq!(short.dock.height, 1);
        assert!(short.scope.is_empty());
    }

    #[test]
    fn complete_frame_renders_unicode_cursor_and_source_only_flash() {
        let source = "// 演奏\n$: s(\"bd\")._pianoroll().scope()\n";
        let mut editor = Editor::new(source).unwrap();
        let size = Rect::new(0, 0, 140, 30);
        let ui_layout = rustel_runtime::ui_events::visual_layout(source, 0).unwrap();
        let mut visual = VisualState::default();
        visual.install_layout(ui_layout.clone());
        let layout = regions(size, false, 1, false, true, None, None);
        let grid = source_grid(&editor, layout.panes[0].editor, true);
        editor.set_view_size(usize::from(grid.width), usize::from(grid.height));
        let inline = ui_layout
            .ui_layout
            .visuals
            .iter()
            .find(|visual| visual.inline)
            .unwrap();
        editor
            .set_virtual_rows(
                editor.revision(),
                vec![VirtualRowSpec::new(
                    inline.id.clone(),
                    crate::editor::ByteOffset(inline.to),
                    2,
                )],
            )
            .unwrap();
        let map = editor.screen_map(grid).unwrap();
        let virtual_y = map
            .rows()
            .iter()
            .find_map(|row| match row {
                ScreenRow::Virtual(row) => Some(row.screen_y),
                ScreenRow::Text(_) => None,
            })
            .unwrap();
        let expected_cursor = map
            .cell_for_offset(editor.primary_selection().head)
            .unwrap();

        let theme = theme();
        let master = MasterState::new(Instant::now());
        let minimap = Minimap::default();
        let devices = DeviceInventory {
            audio_outputs: vec![DeviceEntry {
                name: "Speakers".into(),
                id: String::new(),
                detail: String::new(),
                is_default: true,
            }],
            ..DeviceInventory::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(size.width, size.height)).unwrap();
        terminal
            .draw(|frame| {
                let mut chrome = chrome(&theme, &master, Path::new("演奏.strudel"));
                chrome.evaluation_flash = true;
                render(
                    frame,
                    StudioView {
                        show_scrollbars: true,
                        midi_rows: Vec::new(),
                        clock_in: None,
                        clock_out: None,
                        link_enabled: false,
                        link: None,
                        prebake_rows: [PrebakeRow::default(); 2],
                        sets_folder: String::new(),
                        recordings_folder: String::new(),
                        set_limiter: None,
                        sample_cache: String::new(),
                        precache: None,
                        sources: Vec::new(),
                        mappings: Default::default(),
                        keybind_rows: Vec::new(),
                        permission_rows: Vec::new(),
                        #[cfg(feature = "vst")]
                        vst_page: Default::default(),
                        latency: None,
                        caching_samples: 0,
                        importing_sources: 0,
                        library_loading: false,
                        preview_shape: None,
                        focused_panel: None,
                        help_open: false,
                        preview_gain: 1.0,
                        auto_preview: false,
                        sounding_note: None,
                        audition_loading: None,
                        snippet_picture: false,
                        snippet_refused: None,
                        #[cfg(feature = "hydra")]
                        snippet_playing: None,
                        #[cfg(feature = "hydra")]
                        snippet_preview_status: None,
                        panes: vec![PaneView {
                            replay: None,
                            prebake: None,
                            editor: &editor,
                            map: &map,
                            minimap: &minimap,
                            decorations: Decorations::default(),
                            name: "演奏".into(),
                            dirty: false,
                            playing: true,
                            focused: true,
                            flash: lit_flash(),
                            locate_flash: None,
                            timeline_focus: false,
                            line_numbers: true,
                        }],
                        visual: &visual,
                        devices: &devices,
                        scanning: false,
                        panel: None,
                        scenes: &[],
                        strip_mode: &SceneStripMode::Idle,
                        reference: None,
                        log: None,
                        jobs: None,
                        memory: None,
                        log_memory: None,
                        export: None,
                        theme_picker: None,
                        set_panel: None,
                        set_prompt: None,
                        viz: [None, None],
                        viz_focus: 0,
                        mixer: None,
                        mixer_panel: None,
                        knobs: None,
                        mixer_selection: None,
                        reference_on_top: false,
                        theme_camera: None,
                        #[cfg(feature = "hydra")]
                        hydra_webcam: None,
                        settings: None,
                    },
                    chrome,
                );
            })
            .unwrap();

        let cursor = terminal.backend().cursor_position();
        assert_eq!((cursor.x, cursor.y), (expected_cursor.x, expected_cursor.y));
        let buffer = terminal.backend().buffer();
        let caret = buffer
            .cell((expected_cursor.x, expected_cursor.y))
            .expect("the editor caret cell");
        assert_ne!(caret.bg, mix(theme.background, theme.accent, 0.45));
        assert!(
            caret.modifier.contains(Modifier::REVERSED),
            "the native caret does not rewrite the source cell beneath it"
        );
        assert!(buffer.content.iter().any(|cell| cell.symbol() == "演"));
        let editor_area = layout.panes[0].editor;
        assert!(
            buffer
                .cell((editor_area.x, editor_area.y))
                .is_some_and(|cell| cell.modifier.contains(Modifier::REVERSED))
        );
        assert!(
            buffer
                .cell((editor_area.right().saturating_sub(1), editor_area.y))
                .is_some_and(|cell| !cell.modifier.contains(Modifier::REVERSED)),
            "the flash ends with the source instead of filling its row"
        );
        assert!(
            buffer
                .cell((grid.x, virtual_y))
                .is_some_and(|cell| !cell.modifier.contains(Modifier::REVERSED)),
            "inline visualization rows keep rendering through the flash"
        );
        // The master dock and its readouts are part of every wide frame.
        let text = text_of(buffer);
        assert!(text.contains("MASTER"), "the master dock is missing");
        assert!(text.contains("0.0dB"));

        // The help overlay is not a focused panel, so this flag is the one
        // thing that tells the compositor to put the underlying caret out.
        terminal
            .draw(|frame| {
                render(
                    frame,
                    StudioView {
                        show_scrollbars: true,
                        midi_rows: Vec::new(),
                        clock_in: None,
                        clock_out: None,
                        link_enabled: false,
                        link: None,
                        prebake_rows: [PrebakeRow::default(); 2],
                        sets_folder: String::new(),
                        recordings_folder: String::new(),
                        set_limiter: None,
                        sample_cache: String::new(),
                        precache: None,
                        sources: Vec::new(),
                        mappings: Default::default(),
                        keybind_rows: Vec::new(),
                        permission_rows: Vec::new(),
                        #[cfg(feature = "vst")]
                        vst_page: Default::default(),
                        latency: None,
                        caching_samples: 0,
                        importing_sources: 0,
                        library_loading: false,
                        preview_shape: None,
                        focused_panel: None,
                        help_open: true,
                        preview_gain: 1.0,
                        auto_preview: false,
                        sounding_note: None,
                        audition_loading: None,
                        snippet_picture: false,
                        snippet_refused: None,
                        #[cfg(feature = "hydra")]
                        snippet_playing: None,
                        #[cfg(feature = "hydra")]
                        snippet_preview_status: None,
                        panes: vec![PaneView {
                            replay: None,
                            prebake: None,
                            editor: &editor,
                            map: &map,
                            minimap: &minimap,
                            decorations: Decorations::default(),
                            name: "演奏".into(),
                            dirty: false,
                            playing: true,
                            focused: true,
                            flash: None,
                            locate_flash: None,
                            timeline_focus: false,
                            line_numbers: true,
                        }],
                        visual: &visual,
                        devices: &devices,
                        scanning: false,
                        panel: None,
                        scenes: &[],
                        strip_mode: &SceneStripMode::Idle,
                        reference: None,
                        log: None,
                        jobs: None,
                        memory: None,
                        log_memory: None,
                        export: None,
                        theme_picker: None,
                        set_panel: None,
                        set_prompt: None,
                        viz: [None, None],
                        viz_focus: 0,
                        mixer: None,
                        mixer_panel: None,
                        knobs: None,
                        mixer_selection: None,
                        reference_on_top: false,
                        theme_camera: None,
                        #[cfg(feature = "hydra")]
                        hydra_webcam: None,
                        settings: None,
                    },
                    chrome(&theme, &master, Path::new("演奏.strudel")),
                );
            })
            .unwrap();
        assert!(!terminal.backend().cursor_visible());
    }

    #[test]
    fn an_open_device_panel_is_drawn_over_the_frame() {
        let source = "$: s(\"bd\")\n";
        let mut editor = Editor::new(source).unwrap();
        let size = Rect::new(0, 0, 140, 30);
        let layout = regions(size, false, 1, false, true, None, None);
        let grid = source_grid(&editor, layout.panes[0].editor, true);
        editor.set_view_size(usize::from(grid.width), usize::from(grid.height));
        let map = editor.screen_map(grid).unwrap();
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let minimap = Minimap::default();
        let visual = VisualState::default();
        let devices = DeviceInventory {
            audio_outputs: vec![DeviceEntry {
                name: "Scarlett 2i2".into(),
                id: String::new(),
                detail: "48000 Hz · 2ch".into(),
                is_default: false,
            }],
            ..DeviceInventory::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(size.width, size.height)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    StudioView {
                        show_scrollbars: true,
                        midi_rows: Vec::new(),
                        clock_in: None,
                        clock_out: None,
                        link_enabled: false,
                        link: None,
                        prebake_rows: [PrebakeRow::default(); 2],
                        sets_folder: String::new(),
                        recordings_folder: String::new(),
                        set_limiter: None,
                        sample_cache: String::new(),
                        precache: None,
                        sources: Vec::new(),
                        mappings: Default::default(),
                        keybind_rows: Vec::new(),
                        permission_rows: Vec::new(),
                        #[cfg(feature = "vst")]
                        vst_page: Default::default(),
                        latency: None,
                        caching_samples: 0,
                        importing_sources: 0,
                        library_loading: false,
                        preview_shape: None,
                        focused_panel: None,
                        help_open: false,
                        preview_gain: 1.0,
                        auto_preview: false,
                        sounding_note: None,
                        audition_loading: None,
                        snippet_picture: false,
                        snippet_refused: None,
                        #[cfg(feature = "hydra")]
                        snippet_playing: None,
                        #[cfg(feature = "hydra")]
                        snippet_preview_status: None,
                        panes: vec![PaneView {
                            replay: None,
                            prebake: None,
                            editor: &editor,
                            map: &map,
                            minimap: &minimap,
                            decorations: Decorations::default(),
                            name: "live".into(),
                            dirty: false,
                            playing: false,
                            focused: true,
                            flash: None,
                            locate_flash: None,
                            timeline_focus: false,
                            line_numbers: true,
                        }],
                        visual: &visual,
                        devices: &devices,
                        scanning: false,
                        panel: Some(DevicePanel::default()),
                        scenes: &[],
                        strip_mode: &SceneStripMode::Idle,
                        reference: None,
                        log: None,
                        jobs: None,
                        memory: None,
                        log_memory: None,
                        export: None,
                        theme_picker: None,
                        set_panel: None,
                        set_prompt: None,
                        viz: [None, None],
                        viz_focus: 0,
                        mixer: None,
                        mixer_panel: None,
                        knobs: None,
                        mixer_selection: None,
                        reference_on_top: false,
                        theme_camera: None,
                        #[cfg(feature = "hydra")]
                        hydra_webcam: None,
                        settings: None,
                    },
                    chrome(&theme, &master, Path::new("live.strudel")),
                );
            })
            .unwrap();
        let text = text_of(terminal.backend().buffer());
        assert!(text.contains("Scarlett 2i2"), "{text}");
        assert!(text.contains("audio out"));
    }

    #[test]
    fn a_split_frame_titles_both_panes_and_draws_the_reference() {
        use crate::reference::{Reference, ReferencePanel};
        let size = Rect::new(0, 0, 200, 40);
        let layout = regions(size, false, 2, true, true, None, None);
        let mut left = Editor::new("$: s(\"bd\")\n").unwrap();
        let mut right = Editor::new("$: s(\"hh*4\")\n").unwrap();
        let left_grid = source_grid(&left, layout.panes[0].editor, true);
        let right_grid = source_grid(&right, layout.panes[1].editor, true);
        left.set_view_size(usize::from(left_grid.width), usize::from(left_grid.height));
        right.set_view_size(
            usize::from(right_grid.width),
            usize::from(right_grid.height),
        );
        let left_map = left.screen_map(left_grid).unwrap();
        let right_map = right.screen_map(right_grid).unwrap();
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let minimap = Minimap::default();
        let visual = VisualState::default();
        let devices = DeviceInventory::default();
        let reference = Reference::load_all();
        let panel = ReferencePanel::open(&reference, reference.lookup("lpf").unwrap());
        let mut terminal = Terminal::new(TestBackend::new(size.width, size.height)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    StudioView {
                        show_scrollbars: true,
                        midi_rows: Vec::new(),
                        clock_in: None,
                        clock_out: None,
                        link_enabled: false,
                        link: None,
                        prebake_rows: [PrebakeRow::default(); 2],
                        sets_folder: String::new(),
                        recordings_folder: String::new(),
                        set_limiter: None,
                        sample_cache: String::new(),
                        precache: None,
                        sources: Vec::new(),
                        mappings: Default::default(),
                        keybind_rows: Vec::new(),
                        permission_rows: Vec::new(),
                        #[cfg(feature = "vst")]
                        vst_page: Default::default(),
                        latency: None,
                        caching_samples: 0,
                        importing_sources: 0,
                        library_loading: false,
                        preview_shape: None,
                        focused_panel: None,
                        help_open: false,
                        preview_gain: 1.0,
                        auto_preview: false,
                        sounding_note: None,
                        audition_loading: None,
                        snippet_picture: false,
                        snippet_refused: None,
                        #[cfg(feature = "hydra")]
                        snippet_playing: None,
                        #[cfg(feature = "hydra")]
                        snippet_preview_status: None,
                        panes: vec![
                            PaneView {
                                replay: None,
                                prebake: None,
                                editor: &left,
                                map: &left_map,
                                minimap: &minimap,
                                decorations: Decorations::default(),
                                name: "intro".into(),
                                dirty: false,
                                playing: true,
                                focused: false,
                                flash: None,
                                locate_flash: None,
                                timeline_focus: false,
                                line_numbers: true,
                            },
                            PaneView {
                                replay: None,
                                prebake: None,
                                editor: &right,
                                map: &right_map,
                                minimap: &minimap,
                                decorations: Decorations::default(),
                                name: "drop".into(),
                                dirty: true,
                                playing: false,
                                focused: true,
                                flash: None,
                                locate_flash: None,
                                timeline_focus: false,
                                line_numbers: true,
                            },
                        ],
                        visual: &visual,
                        devices: &devices,
                        scanning: false,
                        panel: None,
                        scenes: &[],
                        strip_mode: &SceneStripMode::Idle,
                        reference: Some((&reference, &panel, false)),
                        log: None,
                        jobs: None,
                        memory: None,
                        log_memory: None,
                        export: None,
                        theme_picker: None,
                        set_panel: None,
                        set_prompt: None,
                        viz: [None, None],
                        viz_focus: 0,
                        mixer: None,
                        mixer_panel: None,
                        knobs: None,
                        mixer_selection: None,
                        reference_on_top: false,
                        theme_camera: None,
                        #[cfg(feature = "hydra")]
                        hydra_webcam: None,
                        settings: None,
                    },
                    chrome(&theme, &master, Path::new("drop.strudel")),
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let row = |y: u16| {
            (0..size.width)
                .filter_map(|x| buffer.cell((x, y)))
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        let titles = row(layout.panes[0].title.y);
        assert!(titles.contains("▶ intro"), "{titles}");
        assert!(titles.contains("drop ●"), "{titles}");
        assert!(
            !titles.contains("plays"),
            "a title does not play its scene: {titles}"
        );
        let text = (0..size.height).map(row).collect::<Vec<_>>().join("\n");
        assert!(text.contains("reference"), "{text}");
        assert!(text.contains("lpf(frequency)"), "{text}");
        assert!(text.contains("hh*4"), "the right pane's source is drawn");
        let cursor = terminal.backend().cursor_position();
        assert!(
            layout.panes[1].editor.contains((cursor.x, cursor.y).into()),
            "the caret sits in the focused pane"
        );
    }
    /// A surface drawn over the score forgets what the score drew there:
    /// an underline under a mini-notation word or a reversed caret cell
    /// used to survive a plain `set_style` into the settings sheet as
    /// stray coloured fragments and black boxes.
    #[test]
    fn a_surface_starts_clean_of_what_was_under_it() {
        let area = Rect::new(0, 0, 4, 1);
        let mut buffer = Buffer::empty(area);
        buffer.set_string(
            0,
            0,
            "hh o",
            Style::default()
                .fg(Color::Rgb(1, 2, 3))
                .add_modifier(Modifier::UNDERLINED | Modifier::BOLD),
        );
        buffer
            .cell_mut((3, 0))
            .unwrap()
            .set_style(Style::default().add_modifier(Modifier::REVERSED));
        clear_surface(
            &mut buffer,
            area,
            Style::default()
                .bg(Color::Rgb(9, 9, 9))
                .fg(Color::Rgb(7, 7, 7)),
        );
        for x in 0..4 {
            let cell = buffer.cell((x, 0)).unwrap();
            assert_eq!(cell.symbol(), " ", "the old glyph is gone");
            assert_eq!(cell.bg, Color::Rgb(9, 9, 9));
            assert_eq!(cell.fg, Color::Rgb(7, 7, 7));
            assert!(
                cell.modifier.is_empty(),
                "no modifier survives: {:?}",
                cell.modifier
            );
        }
    }
}
