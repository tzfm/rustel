//! Ratatui composition for the studio's source-first screen.
//!
//! Every colour here comes from the active [`Theme`]; nothing is hard-coded,
//! so a community theme file changes the whole surface without touching this
//! module.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::Instant;

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
use rustel_runtime::{EnginePressureLevel, EnginePressureSnapshot, ProcessStats, product};

mod layout;

pub use layout::{
    ChromeLayout, PaneRegion, SetSidebar, Side, StudioRegions, menu_row, regions, regions_with,
    regions_with_footer,
};

/// The master dock at the bottom right: meter, fader and readouts.
const DOCK_WIDTH: u16 = 40;
/// A narrower dock for terminals without room for the full one.
const DOCK_WIDTH_NARROW: u16 = 28;
/// The master scope beside the dock: what it takes when the row can spare
/// it, and the least it is worth drawing in. Between the two it takes what
/// is free; under the floor the row keeps the cells for the status and the
/// orbit chips instead.
const SCOPE_WIDTH: u16 = 18;
const SCOPE_MIN_WIDTH: u16 = 8;
/// The narrowest footer that gets the full master dock rather than the
/// narrow one.
const WIDE_FOOTER_WIDTH: u16 = 110;
/// Cells the status line is promised before the scope and the orbit chips
/// take theirs, and the least it is ever promised on a narrow row. It
/// keeps whatever they leave on top of this.
const STATUS_RESERVED_WIDTH: u16 = 28;
const STATUS_LEAST_WIDTH: u16 = 20;

/// The text grid of a pane: `area` less the gutter, which `numbered`
/// gives the line numbers or takes away.
pub fn source_grid(editor: &Editor, area: Rect, numbered: bool) -> GridRect {
    let gutter = gutter_width(editor, numbered);
    GridRect::new(
        area.x.saturating_add(gutter),
        area.y,
        area.width.saturating_sub(gutter),
        area.height,
    )
}

/// What the gutter keeps without line numbers: text should not touch the
/// pane's edge.
const MARGIN_WIDTH: u16 = 1;

pub(crate) fn gutter_width(editor: &Editor, numbered: bool) -> u16 {
    if !numbered {
        return MARGIN_WIDTH;
    }
    let digits = editor.document().line_count().max(1).ilog10() as u16 + 1;
    digits.saturating_add(2).clamp(4, 8)
}

/// Where the header drew its warning badge this frame, packed x/y/width, so
/// the pointer can ask without replicating the header's flowing layout.
/// Zero means no badge is on screen.
static WARNING_BADGE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn set_warning_badge(rect: Option<(u16, u16, u16)>) {
    let packed = rect.map_or(0, |(x, y, width)| {
        (u64::from(x) << 32) | (u64::from(y) << 16) | u64::from(width)
    });
    WARNING_BADGE.store(packed, std::sync::atomic::Ordering::Relaxed);
}

/// Whether a pointer position is on the header's warning badge - the icon
/// and its count, not the bar around them.
pub fn warning_badge_at(x: u16, y: u16) -> bool {
    let packed = WARNING_BADGE.load(std::sync::atomic::Ordering::Relaxed);
    if packed == 0 {
        return false;
    }
    let badge_x = (packed >> 32) as u16;
    let badge_y = (packed >> 16) as u16;
    let width = packed as u16;
    y == badge_y && x >= badge_x && x < badge_x.saturating_add(width)
}

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
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TransportWord {
    pub evaluating: bool,
    pub stopping: bool,
    pub playing: bool,
}

impl TransportWord {
    fn text(self) -> &'static str {
        if self.evaluating {
            "EVALUATING"
        } else if self.stopping {
            "STOPPING"
        } else if self.playing {
            "PLAYING"
        } else {
            "STOPPED"
        }
    }
}

/// Where the header draws its readiness chip - `✓ ready`, `✗ 5 problems`.
///
/// Computed the way the header renders it, so a click lands on the words a
/// reader is looking at. Empty when the chip has nothing to say.
pub fn header_go_chip(area: Rect, word: TransportWord, go: GoState) -> Rect {
    let name_width = UnicodeWidthStr::width(format!(" ● {} ", product::NAME).as_str())
        .min(usize::from(area.width)) as u16;
    let state_x = area.x.saturating_add(name_width);
    let chip_x =
        state_x.saturating_add(UnicodeWidthStr::width(format!("{} ", word.text()).as_str()) as u16);
    let width = UnicodeWidthStr::width(go.text().as_str()) as u16;
    Rect::new(
        chip_x,
        area.y,
        width.min(area.right().saturating_sub(chip_x)),
        u16::from(!area.is_empty()),
    )
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
            Self::Loading { .. } => theme.warn,
            Self::Failed { .. } | Self::Problems { .. } => theme.error,
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

/// The colour the loading line's underline takes: the accent, moved until
/// it reads against the header.
pub fn loading_line_colour(theme: &Theme) -> Color {
    super::theme::legible_against_floor(theme.accent, theme.surface, 3.0)
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
    pub evaluating: bool,
    pub dirty: bool,
    pub status: &'a str,
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
    /// The evaluation acknowledgement is drawn over this pane.
    pub flash: bool,
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
    pub reference: Option<(&'a Reference, &'a ReferencePanel)>,
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
                    keybinds: chrome.keybinds,
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
        if pane.flash {
            frame.render_widget(EvaluationFlash { map: pane.map }, editor_area);
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
        let Some((reference, panel)) = view.reference else {
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
                pulse: Some(super::reference::SamplesPulse {
                    // The preview's own tap, not the mix: the meter shows
                    // what the browser is sounding.
                    peak_db: super::reference::audition_peak_db(chrome.audition_audio),
                    preview_gain: view.preview_gain,
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
        if focused {
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
        frame.render_widget(ExportSheetView { sheet, theme }, area);
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
            },
            area,
        );
    }
    if view.reference_on_top {
        draw_reference(frame, &mut cursor);
    }
    if let Some(picker) = view.set_prompt {
        frame.render_widget(super::file_picker::FilePickerView { picker, theme }, area);
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
    if let Some(picker) = view
        .viz
        .iter()
        .flatten()
        .find_map(|viz| viz.panel.prompt.as_ref())
    {
        frame.render_widget(super::file_picker::FilePickerView { picker, theme }, area);
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

/// Whether a glyph is one of the drawing rasters' - a block, a sextant, a
/// Braille cell - as opposed to text.
fn is_raster_glyph(symbol: &str) -> bool {
    symbol.chars().next().is_some_and(|glyph| {
        ('\u{2580}'..='\u{259f}').contains(&glyph)
            || ('\u{1fb00}'..='\u{1fbff}').contains(&glyph)
            || ('\u{2800}'..='\u{28ff}').contains(&glyph)
    })
}

/// Lay a painter's picture under the score: the global stage. `stage` is
/// the painter's own render of `area`; every cell it drew - a raster glyph,
/// or a background of its own - tints the frame cell's background by
/// `strength`, so the text sits on the picture and the picture sits over
/// Hydra. Chrome keeps its colour (by rect, like the backdrop), and so do
/// `untouched` cells: a sounding event's highlight is the top layer. The
/// painter's words follow [`stage_words`].
pub(crate) fn paint_stage(
    buffer: &mut Buffer,
    stage: &Buffer,
    area: Rect,
    background: Color,
    chrome: &[Rect],
    untouched: &std::collections::HashSet<(u16, u16)>,
    strength: f32,
) {
    if strength <= 0.0 {
        return;
    }
    for y in area.y..area.bottom() {
        // Only the score's own ground takes the stage: a chip or a
        // highlight painted its own colour and keeps it.
        let ground: Vec<bool> = (area.x..area.right())
            .map(|x| {
                let at = (x, y);
                !untouched.contains(&at)
                    && !chrome.iter().any(|panel| panel.contains(at.into()))
                    && buffer
                        .cell(at)
                        .is_some_and(|cell| cell.bg == background || cell.bg == Color::Reset)
            })
            .collect();
        let words = stage_words(stage, area, y, &blank_ground(buffer, area, y, &ground));
        for x in area.x..area.right() {
            let at = (x, y);
            let index = usize::from(x - area.x);
            let Some(painted) = stage.cell(at) else {
                continue;
            };
            let glyph = painted.symbol();
            let drawn = is_raster_glyph(glyph);
            let written = !drawn && glyph != " " && words[index];
            let colour = if painted.bg != background && painted.bg != Color::Reset {
                painted.bg
            } else if drawn || written {
                painted.fg
            } else {
                continue;
            };
            if !ground[index] {
                continue;
            }
            let Some(cell) = buffer.cell_mut(at) else {
                continue;
            };
            cell.set_bg(super::theme::mix(background, colour, strength));
            // Brighter than the wash, or a word would be a smudge the
            // shape of a word.
            if written {
                cell.set_char(glyph.chars().next().unwrap_or(' '));
                cell.set_fg(super::theme::mix(
                    background,
                    colour,
                    (strength * 2.5).clamp(0.45, 0.9),
                ));
            }
        }
    }
}

/// The cells of `ground` on row `y` that hold nothing of the score: a blank
/// symbol that no wide character to its left covers.
fn blank_ground(buffer: &Buffer, area: Rect, y: u16, ground: &[bool]) -> Vec<bool> {
    let mut covered: usize = 0;
    (area.x..area.right())
        .zip(ground)
        .map(|(x, &open)| {
            let symbol = buffer.cell((x, y)).map_or(" ", |cell| cell.symbol());
            let blank = open && covered == 0 && symbol == " ";
            covered = usize::max(
                covered.saturating_sub(1),
                UnicodeWidthStr::width(symbol).saturating_sub(1),
            );
            blank
        })
        .collect()
}

/// The columns of `area` on row `y` that take the painter's words. A word
/// is a run of the painter's text, single spaces inside it included; it is
/// written only where `blank` holds for every cell under it and for the
/// cell on either side (the pane's edge counts as blank), and is left out
/// whole otherwise, so it never mixes with the score's own text.
fn stage_words(stage: &Buffer, area: Rect, y: u16, blank: &[bool]) -> Vec<bool> {
    let free = |x: u16| blank[usize::from(x - area.x)];
    let symbol = |x: u16| stage.cell((x, y)).map_or(" ", |cell| cell.symbol());
    let text = |x: u16| {
        let glyph = symbol(x);
        glyph != " " && !is_raster_glyph(glyph)
    };
    let right = area.right();
    let mut words = vec![false; usize::from(area.width)];
    let mut x = area.x;
    while x < right {
        if !text(x) {
            x += 1;
            continue;
        }
        let start = x;
        let mut end = x;
        loop {
            if end < right && text(end) {
                let width = UnicodeWidthStr::width(symbol(end)).max(1) as u16;
                end = end.saturating_add(width).min(right);
            } else if end + 1 < right && symbol(end) == " " && text(end + 1) {
                end += 1;
            } else {
                break;
            }
        }
        let fits = (start == area.x || free(start - 1))
            && (start..end).all(&free)
            && (end >= right || free(end));
        if fits {
            words[usize::from(start - area.x)..usize::from(end - area.x)].fill(true);
        }
        x = end;
    }
    words
}

/// The countdown chip fitted to `room` cells. A plain guillemet, not a
/// clock emoji: the studio is a terminal, and a colour glyph in the header
/// reads as a sticker on it. The number is the point of the chip, so a
/// long scene name gives way before the number does.
pub(crate) fn launch_chip(scene: &str, cycles: f64, room: usize) -> String {
    let tail = format!(" in {cycles:.1}");
    let full = format!("» {scene}{tail}");
    if UnicodeWidthStr::width(full.as_str()) <= room {
        return full;
    }
    let name_room = room.saturating_sub(UnicodeWidthStr::width(tail.as_str()) + 3);
    if name_room >= 2 {
        let short: String = scene.chars().take(name_room).collect();
        return format!("» {short}…{tail}");
    }
    format!("»{tail}")
}

fn render_launch_corner(buffer: &mut Buffer, area: Rect, scene: &str, cycles: f64, theme: &Theme) {
    if area.height == 0 || area.width < 12 {
        return;
    }
    let text = format!(
        " {} ",
        launch_chip(scene, cycles, usize::from(area.width.saturating_sub(4)))
    );
    let width = UnicodeWidthStr::width(text.as_str()) as u16;
    let x = area.right().saturating_sub(width);
    buffer.set_stringn(
        x,
        area.y,
        &text,
        usize::from(width),
        Style::default()
            .fg(theme.warn)
            .bg(theme.background)
            .add_modifier(Modifier::BOLD),
    );
}

/// The name row over a pane in split view: which scene it shows, whether
/// it is the sounding one, and whether the caret is here.
struct PaneTitle<'a> {
    pub keybinds: &'a super::keybinds::Keybinds,
    name: &'a str,
    dirty: bool,
    playing: bool,
    focused: bool,
    prebake: bool,
    replay: bool,
    theme: &'a Theme,
}

impl Widget for PaneTitle<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let theme = self.theme;
        clear_surface(
            buffer,
            area,
            Style::default().bg(theme.surface).fg(theme.muted),
        );
        let text = format!(
            " {}{}{} ",
            if self.prebake {
                format!("{PREBAKE_GLYPH} ")
            } else if self.replay {
                format!("{REPLAY_GLYPH} ")
            } else if self.playing {
                "▶ ".to_owned()
            } else {
                String::new()
            },
            self.name,
            if self.dirty { " ●" } else { "" }
        );
        let style = if self.focused {
            Style::default()
                .fg(theme.background)
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else if self.playing {
            Style::default().fg(theme.ok)
        } else {
            Style::default().fg(theme.foreground)
        };
        let title_width = UnicodeWidthStr::width(text.as_str());
        buffer.set_stringn(area.x, area.y, text, usize::from(area.width), style);
        // Titles focus their pane, and that is all they do: no click
        // plays a scene. A pad does, and so does an update.
        if self.focused {
            return;
        }
        let Some(binding) = self.keybinds.binding(super::keybinds::BindAction::HopPane) else {
            return;
        };
        let switch = format!("{} switches", binding.hint());
        let hints: &[&str] = &[&switch];
        if let Some((hint, width)) = hints.iter().find_map(|&hint| {
            let width = UnicodeWidthStr::width(hint);
            (usize::from(area.width) >= title_width + width + 2).then_some((hint, width as u16))
        }) {
            buffer.set_stringn(
                area.right().saturating_sub(width + 1),
                area.y,
                hint,
                usize::from(width),
                Style::default().fg(theme.muted),
            );
        }
    }
}

/// First and last screen row the source pane is showing, inline rows included.
fn visible_rows(map: &ScreenMap) -> (usize, usize) {
    let first = map.viewport().top_row;
    (
        first,
        first.saturating_add(map.rows().len().saturating_sub(1)),
    )
}

pub(super) struct Backdrop<'a> {
    pub(super) theme: &'a Theme,
}

/// One RGBA visual frame, as a terminal that has no graphics protocol can
/// show it: the colour of each cell.
///
/// Each cell is one pixel, so the picture has the resolution of the
/// grid, for example 80 by 24. This is the only version that works in
/// every terminal, including the ones on Windows.
pub struct VisualBackdrop {
    pub width: u16,
    pub height: u16,
    pub rgba: Vec<u8>,
    /// How strongly it shows through the score's own background, 0..=1. The
    /// code has to stay readable, so the caller caps this by what `editor
    /// opacity` leaves (`backdrop_strength`).
    pub strength: f32,
    /// How strongly it shows through everything the interface painted for
    /// itself - the header, the footer, the minimap, a panel, a highlight.
    /// Zero leaves the interface solid, which is the picture behind it rather
    /// than through it. The code area has `strength`, the chrome has this:
    /// two surfaces, each capped by its own opacity setting.
    pub interface: f32,
}

/// Compatibility name for callers that consumed Hydra frames before the
/// compositor also became TachyonFX's image delivery path.
#[cfg(feature = "hydra")]
pub type HydraBackdrop = VisualBackdrop;

/// The editor's selected cells, and the wash the theme affords them.
///
/// The selection is the one editor decoration whose ground the picture may
/// run through at its own strength - decided by the theme's contrast budget
/// rather than the flat interface number - and colour cannot identify it at
/// composite time (four surfaces paint `theme.selection`, and two shipped
/// themes reuse the editor's own background), so the cells are named
/// outright.
pub struct SelectionWash<'a> {
    pub cells: &'a std::collections::HashSet<(u16, u16)>,
    /// 0..=1: how much of the picture the selection takes.
    pub strength: f32,
    /// Cells the picture never touches: a sounding event's highlight is
    /// the top layer, drawn over everything, and reads as a mark only
    /// while nothing washes through it.
    pub untouched: &'a std::collections::HashSet<(u16, u16)>,
}

impl VisualBackdrop {
    /// Paint through the original single-exclusion API.
    pub fn paint(
        &self,
        buffer: &mut Buffer,
        area: Rect,
        under: Color,
        keep: Option<Rect>,
        chrome: &[Rect],
        selection: Option<&SelectionWash>,
    ) {
        match keep {
            Some(keep) => self.paint_excluding(
                buffer,
                area,
                under,
                std::slice::from_ref(&keep),
                chrome,
                selection,
            ),
            None => self.paint_excluding(buffer, area, under, &[], chrome, selection),
        }
    }

    /// Colour the finished frame with the picture, leaving `keep` untouched.
    ///
    /// Runs after everything else has drawn, so every cell already holds the
    /// colour it wanted. A cell showing nothing but the theme's own background
    /// takes the picture at [`Self::strength`]; a cell that painted something
    /// for itself - the header, the minimap, a panel, a highlight - takes it
    /// over that colour at [`Self::interface`], keeping its own identity while
    /// the picture washes through it.
    ///
    /// `keep` spares a rect from the picture entirely; `chrome` declares one
    /// interface by rect, for panels whose colours a theme makes
    /// indistinguishable from the score's.
    pub fn paint_excluding(
        &self,
        buffer: &mut Buffer,
        area: Rect,
        under: Color,
        keep: &[Rect],
        chrome: &[Rect],
        selection: Option<&SelectionWash>,
    ) {
        for row in 0..area.height {
            for column in 0..area.width {
                let at = (area.x + column, area.y + row);
                if keep.iter().any(|panel| panel.contains(at.into())) {
                    continue;
                }
                let Some(cell) = buffer.cell_mut(at) else {
                    continue;
                };
                if selection.is_some_and(|wash| wash.untouched.contains(&at)) {
                    continue;
                }
                let (ground, strength) =
                    if let Some(wash) = selection.filter(|wash| wash.cells.contains(&at)) {
                        (cell.bg, wash.strength)
                    } else if chrome.iter().any(|panel| panel.contains(at.into())) {
                        // A rect declared interface takes the interface wash even
                        // where its colours are indistinguishable from the
                        // score's - mono paints its panels with the editor's own
                        // background, and reset-coloured themes paint nothing.
                        // The header, footer, strip and sheets are all here,
                        // which is what keeps a reset-coloured theme's chrome
                        // from pulsing at the picture's full strength.
                        let ground = if cell.bg == Color::Reset {
                            under
                        } else {
                            cell.bg
                        };
                        (ground, self.interface)
                    } else {
                        self.ground_of(cell.bg, under)
                    };
                if strength <= 0.0 {
                    continue;
                }
                cell.set_bg(self.cell(column, row, area, ground, strength));
            }
        }
    }

    /// What a cell is composited over, and how much of the picture it takes.
    ///
    /// The score's own background is the picture's to fill. Anything else on
    /// screen was painted deliberately, so the picture goes over that colour
    /// rather than replacing it, and how far is the reader's setting.
    fn ground_of(&self, background: Color, under: Color) -> (Color, f32) {
        if background == under || background == Color::Reset {
            (under, self.strength)
        } else {
            (background, self.interface)
        }
    }

    /// The picture as an image for a terminal that draws real pixels, with
    /// the same rule the cell path follows.
    ///
    /// Two things are decided here rather than left to the terminal.
    ///
    /// The colour is composited against `under` and sent opaque. A graphics
    /// protocol does carry alpha, but a translucent image is resolved in the
    /// terminal's own colour space against whatever the terminal decides is
    /// beneath it, and no two agree; doing it here is how both delivery paths
    /// arrive at the same colour on every terminal.
    ///
    /// `frame` is the finished frame the image will be laid over, and lets the
    /// image follow the cell path exactly: every pixel is composited over the
    /// colour of the cell it lands in, at that cell's own strength. An image
    /// drawn under the text is still drawn OVER the cell backgrounds, so
    /// without this the picture covers the interface and leaves only the
    /// glyphs, which is a wall of colour rather than a backdrop.
    ///
    /// A cell taking none of the picture is cut out of the image rather than
    /// painted in its own colour. That keeps the guarantee exact: the terminal
    /// draws nothing there, so its idea of that colour never has to match
    /// ours.
    pub fn composited(
        &self,
        under: Color,
        frame: Option<(&Buffer, Rect)>,
        chrome: &[Rect],
        selection: Option<&SelectionWash>,
    ) -> Vec<u8> {
        self.composited_excluding(under, frame, &[], chrome, selection)
    }

    /// The image delivery path with more than one protected overlay.
    pub fn composited_excluding(
        &self,
        under: Color,
        frame: Option<(&Buffer, Rect)>,
        keep: &[Rect],
        chrome: &[Rect],
        selection: Option<&SelectionWash>,
    ) -> Vec<u8> {
        let default = super::graphics::rgb(under);
        let mut out = Vec::with_capacity(self.rgba.len());
        let paint = |out: &mut Vec<u8>, pixel: &[u8], ground: (u8, u8, u8), strength: f32| {
            if strength <= 0.0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
                return;
            }
            let (red, green, blue) = over(pixel, ground, strength);
            out.extend_from_slice(&[red, green, blue, 255]);
        };
        // No frame to follow, or nothing to follow it on: one strength
        // over the theme's own colour, everywhere.
        let Some((buffer, area)) =
            frame.filter(|(_, area)| !area.is_empty() && self.width != 0 && self.height != 0)
        else {
            for pixel in self.rgba.as_chunks::<4>().0 {
                paint(&mut out, pixel, default, self.strength);
            }
            return out;
        };
        // Every cell under the picture, resolved once.
        //
        // The rule a cell falls under - kept clear, washed by a selection,
        // chrome, or plain score background - is a property of the cell, and
        // deciding it costs two hash lookups and two rect scans. A frame can
        // have a million and a half pixels for a few thousand cells, so the
        // pixel loop below only reads a cell's answer out of this table.
        let width = usize::from(area.width);
        let mut table: Vec<((u8, u8, u8), f32)> =
            Vec::with_capacity(width * usize::from(area.height));
        for row in 0..area.height {
            for column in 0..area.width {
                let at = (area.x + column, area.y + row);
                let Some(background) = buffer.cell(at).map(|cell| cell.bg) else {
                    table.push((default, self.strength));
                    continue;
                };
                let (ground, strength) = if keep.iter().any(|rect| rect.contains(at.into()))
                    || selection.is_some_and(|wash| wash.untouched.contains(&at))
                {
                    (background, 0.0)
                } else if let Some(wash) = selection.filter(|wash| wash.cells.contains(&at)) {
                    (background, wash.strength)
                } else if chrome.iter().any(|panel| panel.contains(at.into())) {
                    let ground = if background == Color::Reset {
                        under
                    } else {
                        background
                    };
                    (ground, self.interface)
                } else {
                    self.ground_of(background, under)
                };
                table.push((super::graphics::rgb(ground), strength));
            }
        }
        // Which cell a pixel row and a pixel column land in, once each
        // rather than two divisions per pixel.
        let stride = self.width as usize * 4;
        let row_count = self.rgba.len().div_ceil(stride.max(1));
        let row_of: Vec<usize> = (0..row_count)
            .map(|y| {
                ((y as u64 * u64::from(area.height)) / u64::from(self.height)) as usize * width
            })
            .map(|base| base.min(table.len().saturating_sub(width)))
            .collect();
        let column_of: Vec<usize> = (0..self.width as usize)
            .map(|x| {
                (((x as u64 * u64::from(area.width)) / u64::from(self.width)) as usize)
                    .min(width.saturating_sub(1))
            })
            .collect();
        for (y, line) in self.rgba.chunks(stride).enumerate() {
            let base = row_of[y];
            for (x, pixel) in line.as_chunks::<4>().0.iter().enumerate() {
                let (ground, strength) = table[base + column_of[x]];
                paint(&mut out, pixel, ground, strength);
            }
        }
        out
    }

    /// The colour under one cell, dimmed toward the theme's own background so
    /// the text on top of it keeps its contrast.
    fn cell(&self, column: u16, row: u16, area: Rect, under: Color, strength: f32) -> Color {
        if self.width == 0 || self.height == 0 || area.width == 0 || area.height == 0 {
            return under;
        }
        // Nearest-neighbour, and deliberately so: this is already the coarsest
        // possible raster and averaging would only turn it grey.
        let x = u32::from(column) * u32::from(self.width) / u32::from(area.width);
        let y = u32::from(row) * u32::from(self.height) / u32::from(area.height);
        let at = ((y.min(u32::from(self.height) - 1) * u32::from(self.width))
            + x.min(u32::from(self.width) - 1)) as usize
            * 4;
        let Some(pixel) = self.rgba.get(at..at + 4) else {
            return under;
        };
        let (red, green, blue) = over(pixel, super::graphics::rgb(under), strength);
        Color::Rgb(red, green, blue)
    }
}

/// One pixel of the picture over `under`, at `strength`.
///
/// Hydra's transforms write alpha and write it premultiplied - `luma` returns
/// `vec4(rgb * a, a)`, `mask` and `layer` the same - which is the shape a
/// WebGL canvas hands to the page it is drawn on. So the composite is the one
/// a browser would do, `picture + under * (1 - a)`, and a sketch that thins
/// itself out shows the terminal through the gaps the way it does upstream.
///
/// `strength` scales how much of the picture is let through. A sketch that
/// writes no alpha at all - most of them - reduces to a plain mix.
fn over(pixel: &[u8], under: (u8, u8, u8), strength: f32) -> (u8, u8, u8) {
    let coverage = (f32::from(pixel[3]) / 255.0) * strength;
    let mix = |picture: u8, under: u8| -> u8 {
        (f32::from(picture) * strength + f32::from(under) * (1.0 - coverage)).clamp(0.0, 255.0)
            as u8
    };
    (
        mix(pixel[0], under.0),
        mix(pixel[1], under.1),
        mix(pixel[2], under.2),
    )
}

impl Widget for Backdrop<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        buffer.set_style(
            area,
            Style::default()
                .bg(self.theme.background)
                .fg(self.theme.foreground),
        );
    }
}

/// A geometry-neutral acknowledgement that the score was evaluated. Only
/// visible, non-empty source lines flash: the terminal visualizers and the
/// unused editor background keep moving without a full-pane white blast.
struct EvaluationFlash<'a> {
    map: &'a ScreenMap,
}

/// A subtle, full-width cue for the caret's logical line, including wrapped
/// segments. Preserve syntax, selections and bracket marks so the caret wins.
struct CaretLineFlash<'a> {
    map: &'a ScreenMap,
    line: usize,
    theme: &'a Theme,
}

/// A short Braille-dot ring that converges on the caret. Each glyph resolves
/// the curve within a terminal cell without hiding its background. Measure
/// that background and move the theme accent only as far as visibility needs.
struct CaretLocatorRing<'a> {
    center: CellPoint,
    progress: f32,
    theme: &'a Theme,
}

impl Widget for CaretLocatorRing<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        const BRAILLE_DOTS: [[u32; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];
        if !area.contains((self.center.x, self.center.y).into()) {
            return;
        }
        let radius = self.progress.clamp(0.0, 1.0) * 3.2;
        let reach_x = (radius * 2.0 + 1.0).ceil() as i32;
        let reach_y = (radius + 1.0).ceil() as i32;
        let center_x = i32::from(self.center.x);
        let center_y = i32::from(self.center.y);
        for dy in -reach_y..=reach_y {
            for dx in -reach_x..=reach_x {
                let mut dots = 0;
                for (dot_x, column) in BRAILLE_DOTS.iter().enumerate() {
                    for (dot_y, bit) in column.iter().enumerate() {
                        let x = (dx as f32 + (dot_x as f32 + 0.5) / 2.0 - 0.5) / 2.0;
                        let y = dy as f32 + (dot_y as f32 + 0.5) / 4.0 - 0.5;
                        let distance = (x.powi(2) + y.powi(2)).sqrt();
                        if (distance - radius).abs() <= 0.19 {
                            dots |= *bit;
                        }
                    }
                }
                if dots == 0 {
                    continue;
                }
                let (x, y) = (center_x + dx, center_y + dy);
                if x < i32::from(area.x)
                    || x >= i32::from(area.right())
                    || y < i32::from(area.y)
                    || y >= i32::from(area.bottom())
                {
                    continue;
                }
                if let Some(cell) = buffer.cell_mut((x as u16, y as u16)) {
                    let ground = if cell.bg == Color::Reset {
                        self.theme.background
                    } else {
                        cell.bg
                    };
                    let ring = legible_against_floor(self.theme.accent, ground, 5.5);
                    let glyph = char::from_u32(0x2800 + dots).expect("Braille dots are valid");
                    cell.set_symbol(&glyph.to_string())
                        .set_style(Style::default().fg(ring).remove_modifier(Modifier::all()));
                }
            }
        }
    }
}

/// Painted by the app after its picture, dropdown and help passes so the
/// locator stays in front of every layer that can cover the editor.
pub(super) fn render_caret_locator(
    buffer: &mut Buffer,
    area: Rect,
    center: CellPoint,
    progress: f32,
    theme: &Theme,
) {
    CaretLocatorRing {
        center,
        progress,
        theme,
    }
    .render(area, buffer);
}

impl Widget for CaretLineFlash<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let fill = self.theme.caret_line_fill();
        for row in self.map.rows() {
            let ScreenRow::Text(row) = row else {
                continue;
            };
            if row.line == self.line && area.contains((area.x, row.screen_y).into()) {
                for x in area.x..area.right() {
                    if let Some(cell) = buffer.cell_mut((x, row.screen_y))
                        && (cell.bg == self.theme.background || cell.bg == Color::Reset)
                    {
                        if let Some(fill) = fill {
                            cell.set_bg(fill);
                        } else {
                            // Terminal-owned backgrounds have no measurable RGB;
                            // a little weight still locates the line without inversion.
                            cell.set_style(Style::default().add_modifier(Modifier::BOLD));
                        }
                    }
                }
            }
        }
    }
}

impl Widget for EvaluationFlash<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        for row in self.map.rows() {
            let ScreenRow::Text(row) = row else {
                continue;
            };
            let Some(last_cell) = row.cells.last() else {
                continue;
            };
            if row.screen_y < area.y || row.screen_y >= area.bottom() {
                continue;
            }
            let end = last_cell.screen_x.end.min(area.right());
            if end <= area.x {
                continue;
            }
            buffer.set_style(
                Rect::new(area.x, row.screen_y, end - area.x, 1),
                Style::default().add_modifier(Modifier::REVERSED),
            );
        }
    }
}

/// The right edge of a pane without a minimap: a rule the height of the
/// pane, with a quiet handle where the page sits in the text. A press or
/// a drag on it scrolls, as on the minimap.
struct Scrollbar<'a> {
    editor: &'a Editor,
    theme: &'a Theme,
}

struct HorizontalScrollbar<'a> {
    extent: (usize, usize, usize),
    theme: &'a Theme,
}

pub(crate) fn horizontal_scrollbar_thumb(
    extent: (usize, usize, usize),
    width: u16,
) -> (usize, usize) {
    let (left, page, total) = extent;
    let width = usize::from(width);
    if width == 0 || total == 0 {
        return (0, width);
    }
    let rounded_ratio = |value: usize, numerator: usize, denominator: usize| {
        value
            .saturating_mul(numerator)
            .saturating_add(denominator / 2)
            / denominator
    };
    let furthest = total.saturating_sub(page);
    // A usable handle on long scores, with travel even for tiny overflow.
    let maximum_length = if furthest > 0 && width > 1 {
        width - 1
    } else {
        width
    };
    let length = rounded_ratio(width, page, total).clamp(3.min(maximum_length), maximum_length);
    let travel = width.saturating_sub(length);
    let start = if furthest == 0 {
        0
    } else {
        rounded_ratio(travel, left.min(furthest), furthest).min(travel)
    };
    (start, length)
}

/// Inverse of the painted thumb geometry, with exact endpoints.
pub(crate) fn horizontal_scrollbar_left(
    extent: (usize, usize, usize),
    width: u16,
    thumb: usize,
) -> usize {
    let (_, length) = horizontal_scrollbar_thumb(extent, width);
    let travel = usize::from(width).saturating_sub(length);
    let maximum = extent.2.saturating_sub(extent.1);
    if travel == 0 {
        return 0;
    }
    thumb
        .min(travel)
        .saturating_mul(maximum)
        .saturating_add(travel / 2)
        / travel
}

impl Widget for HorizontalScrollbar<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let width = usize::from(area.width);
        if width == 0 {
            return;
        }
        let (start, length) = horizontal_scrollbar_thumb(self.extent, area.width);
        for column in 0..width {
            let on_handle = column >= start && column < start + length;
            if let Some(cell) = buffer.cell_mut((area.x + column as u16, area.y)) {
                cell.set_symbol("─").set_fg(if on_handle {
                    self.theme.muted
                } else {
                    self.theme.rule
                });
            }
        }
    }
}

impl Scrollbar<'_> {
    fn needed(editor: &Editor) -> bool {
        let (_, page, total) = editor.scroll_extent();
        total > page
    }

    /// The handle's first row and height on a bar `height` tall.
    pub fn handle(editor: &Editor, height: u16) -> (u16, u16) {
        let (top, page, total) = editor.scroll_extent();
        let height = usize::from(height);
        if height == 0 || total <= page {
            return (0, height as u16);
        }
        let length = (height * page / total).clamp(1, height);
        let furthest = total.saturating_sub(1).max(1);
        let start = (height - length) * top.min(furthest) / furthest;
        (start as u16, length as u16)
    }
}

impl Widget for Scrollbar<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let (start, length) = Self::handle(self.editor, area.height);
        for row in 0..area.height {
            let on_handle = row >= start && row < start + length;
            if let Some(cell) = buffer.cell_mut((area.x, area.y + row)) {
                cell.set_symbol(if on_handle { "┃" } else { "│" })
                    .set_fg(if on_handle {
                        self.theme.muted
                    } else {
                        self.theme.rule
                    });
            }
        }
    }
}

struct Header<'a, 'b> {
    chrome: &'a StudioChrome<'b>,
}

impl Widget for Header<'_, '_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let theme = self.chrome.theme;
        clear_surface(
            buffer,
            area,
            Style::default().bg(theme.surface).fg(theme.foreground),
        );
        let word = TransportWord {
            evaluating: self.chrome.evaluating,
            stopping: self.chrome.stopping,
            playing: self.chrome.playing,
        };
        let state = word.text();
        let state_color = if self.chrome.evaluating || self.chrome.stopping {
            theme.warn
        } else if self.chrome.playing {
            theme.ok
        } else {
            theme.muted
        };
        // The dot beats: lit on every beat, most on the downbeat, fading
        // between - the studio's own pulse, or the clock it follows.
        let (dot, dot_color) = beat_pulse(self.chrome.playing, self.chrome.cycle, theme);
        let name = format!(" {dot} {} ", product::NAME);
        let name_width = UnicodeWidthStr::width(name.as_str()).min(usize::from(area.width)) as u16;
        buffer.set_stringn(
            area.x,
            area.y,
            &name,
            usize::from(area.width),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        );
        buffer.set_stringn(
            area.x + 1,
            area.y,
            dot,
            1,
            Style::default().fg(dot_color).add_modifier(Modifier::BOLD),
        );
        let state_x = area.x.saturating_add(name_width);
        buffer.set_stringn(
            state_x,
            area.y,
            format!("{state} "),
            usize::from(area.right().saturating_sub(state_x)),
            Style::default()
                .fg(state_color)
                .add_modifier(Modifier::BOLD),
        );

        // Ready-to-go sits right after the transport word: the one thing to
        // glance at before pressing update.
        let go = self.chrome.go.text();
        // The same reckoning the pointer uses, so a click lands on the
        // words rather than beside them.
        let chip = header_go_chip(area, word, self.chrome.go);
        let go_x = chip.x;
        let go_width = UnicodeWidthStr::width(go.as_str()) as u16;
        if go_width > 0 {
            buffer.set_stringn(
                go_x,
                area.y,
                &go,
                usize::from(area.right().saturating_sub(go_x)),
                Style::default()
                    .fg(self.chrome.go.color(theme))
                    .add_modifier(Modifier::BOLD),
            );
        }
        // A take in progress is the one thing a performer must never lose
        // track of: red, next to the state, with its clock.
        let mut after_go = go_x.saturating_add(go_width + u16::from(go_width > 0) * 3);
        #[cfg(feature = "hydra")]
        if let Some(state) = self.chrome.webcam {
            let (text, colour) = match state {
                rustel_runtime::hydra::HydraWebcamState::Blocked => ("CAM OFF", theme.warn),
                rustel_runtime::hydra::HydraWebcamState::Allowed => ("CAM", theme.muted),
                rustel_runtime::hydra::HydraWebcamState::Requested => ("CAM WAIT", theme.warn),
                rustel_runtime::hydra::HydraWebcamState::Opening => ("CAM OPEN", theme.warn),
                rustel_runtime::hydra::HydraWebcamState::Ready => ("CAM ●", theme.ok),
                rustel_runtime::hydra::HydraWebcamState::Error => ("CAM !", theme.error),
            };
            buffer.set_stringn(
                after_go,
                area.y,
                text,
                usize::from(area.right().saturating_sub(after_go)),
                Style::default().fg(colour).add_modifier(Modifier::BOLD),
            );
            after_go = after_go.saturating_add(UnicodeWidthStr::width(text) as u16 + 3);
        }
        if let Some(recording) = self.chrome.recording {
            let text = recording.text();
            buffer.set_stringn(
                after_go,
                area.y,
                &text,
                usize::from(area.right().saturating_sub(after_go)),
                Style::default()
                    .fg(theme.error)
                    .add_modifier(Modifier::BOLD),
            );
            after_go = after_go.saturating_add(UnicodeWidthStr::width(text.as_str()) as u16 + 3);
        } else if let Some(notice) = self.chrome.take_notice {
            let text = format!("○ {notice}");
            buffer.set_stringn(
                after_go,
                area.y,
                &text,
                usize::from(area.right().saturating_sub(after_go)),
                Style::default().fg(theme.muted),
            );
            after_go = after_go.saturating_add(UnicodeWidthStr::width(text.as_str()) as u16 + 3);
        }
        if let Some((bpm, locked)) = self.chrome.clock_in {
            let text = format!("⇄ {bpm:.1}");
            buffer.set_stringn(
                after_go,
                area.y,
                &text,
                usize::from(area.right().saturating_sub(after_go)),
                Style::default()
                    .fg(if locked { theme.ok } else { theme.warn })
                    .add_modifier(Modifier::BOLD),
            );
            after_go = after_go.saturating_add(UnicodeWidthStr::width(text.as_str()) as u16 + 3);
        } else if self.chrome.clock_out {
            buffer.set_stringn(
                after_go,
                area.y,
                "⇄ out",
                usize::from(area.right().saturating_sub(after_go)),
                Style::default().fg(theme.muted),
            );
            after_go = after_go.saturating_add(5 + 3);
        }
        if let Some((scene, cycles)) = self.chrome.launch
            && !self.chrome.stopping
        {
            let room = usize::from(area.right().saturating_sub(after_go));
            let text = launch_chip(scene, cycles, room);
            buffer.set_stringn(
                after_go,
                area.y,
                &text,
                usize::from(area.right().saturating_sub(after_go)),
                Style::default().fg(theme.warn).add_modifier(Modifier::BOLD),
            );
            after_go = after_go.saturating_add(UnicodeWidthStr::width(text.as_str()) as u16 + 3);
        }
        if let Some(glance) = self.chrome.exporting {
            // Progress and a scope, as a render in a DAW shows: how far, and
            // whether there is still anything to hear.
            let total = glance.seconds.max(0.0) as u64;
            let how_far = match glance.percent {
                Some(percent) => format!("{percent}%"),
                None => format!("{}:{:02}", total / 60, total % 60),
            };
            let scope = glance
                .peaks
                .iter()
                .map(|peak| {
                    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
                    let level = peak.clamp(0.0, 1.0).sqrt();
                    BARS[((level * 7.0).round() as usize).min(7)]
                })
                .collect::<String>();
            let text = format!("⇣ {} {how_far} {scope}", glance.scene_name);
            buffer.set_stringn(
                after_go,
                area.y,
                &text,
                usize::from(area.right().saturating_sub(after_go)),
                Style::default().fg(theme.muted),
            );
            after_go = after_go.saturating_add(UnicodeWidthStr::width(text.as_str()) as u16 + 3);
        }
        if let Some(jobs) = self.chrome.jobs {
            let width = UnicodeWidthStr::width(jobs) as u16;
            super::jobs::set_jobs_chip(Some((after_go, area.y, width)));
            buffer.set_stringn(
                after_go,
                area.y,
                jobs,
                usize::from(area.right().saturating_sub(after_go)),
                Style::default().fg(theme.accent),
            );
            after_go = after_go.saturating_add(width + 3);
        } else {
            super::jobs::set_jobs_chip(None);
        }
        // Something went on under the scene: a small count, until the log
        // is opened. Small on purpose, and a button - clicking it opens the
        // log, which is also what makes it go away.
        if self.chrome.unseen_warnings > 0 {
            let text = format!("⚠ {}", self.chrome.unseen_warnings);
            let width = UnicodeWidthStr::width(text.as_str()) as u16;
            set_warning_badge(Some((after_go, area.y, width)));
            buffer.set_stringn(
                after_go,
                area.y,
                &text,
                usize::from(area.right().saturating_sub(after_go)),
                Style::default().fg(theme.warn),
            );
            after_go = after_go.saturating_add(width + 3);
        } else {
            set_warning_badge(None);
        }
        // The right-hand block names the scene; the loading label stops
        // short of its short name.
        let dirty = if self.chrome.dirty {
            format!(" {}", super::terminal::symbol("●"))
        } else {
            String::new()
        };
        // A prebake tab has no score file, so the header says what it is
        // rather than naming a path that is not the music.
        let short = match self.chrome.prebake {
            Some(scope) => format!("{PREBAKE_GLYPH} {}{dirty}", scope.tab_name()),
            None if self.chrome.replay => format!(
                "{REPLAY_GLYPH} {}{dirty}",
                super::scenes::replay_name(self.chrome.path)
            ),
            None => format!("{}{dirty}", super::scenes::score_name(self.chrome.path)),
        };
        // A load that has lasted says what it waits for where the tempo
        // sits, as one more chip in the row.
        if let Some(label) = self
            .chrome
            .loading
            .as_ref()
            .and_then(|line| line.label.as_deref())
        {
            let short_width = UnicodeWidthStr::width(short.as_str()) as u16;
            let room = area
                .right()
                .saturating_sub(after_go)
                .saturating_sub(short_width + 3);
            let text = ellipsize(label, room);
            buffer.set_stringn(
                after_go,
                area.y,
                &text,
                usize::from(room),
                Style::default().fg(theme.accent),
            );
            after_go = after_go.saturating_add(UnicodeWidthStr::width(text.as_str()) as u16);
        } else if let Some(cps) = self.chrome.cps {
            // Tempo sits next to that, where a musician looks for it,
            // rather than among the process counters.
            let tempo = format!(
                "{:.1} bpm · {cps:.2} cps{}",
                beats_per_minute(cps),
                self.chrome
                    .cycle
                    // The clock can sit a hair before zero while stopped
                    // (the onset lead); a negative cycle is a nonsense to
                    // read, so the header floors it.
                    .map(|cycle| format!(" · cycle {:.2}", cycle.max(0.0)))
                    .unwrap_or_default()
            );
            let tempo_x = after_go;
            let room = usize::from(area.right().saturating_sub(tempo_x)) / 2;
            buffer.set_stringn(
                tempo_x,
                area.y,
                &tempo,
                room,
                Style::default().fg(theme.muted),
            );
            after_go =
                tempo_x.saturating_add(UnicodeWidthStr::width(tempo.as_str()).min(room) as u16);
        }

        // The right-hand block - counters, then the scene - takes what the
        // transport left, never a cell of it. The scene is named the way
        // the strip names it, under its set: `opening night ▸ live`. The
        // path is not here - a set's folder is nested deep and its name
        // says everything the row has room to say.
        let left_end = after_go.saturating_add(2);
        let room = usize::from(area.right().saturating_sub(left_end + 1));
        // The set's name goes before the scene while there is room for
        // both; the scene wins when there is not.
        let set = self.chrome.set_name;
        let candidates = [
            if set.is_empty() {
                short.clone()
            } else {
                format!("{set} \u{25b8} {short}")
            },
            short.clone(),
        ];
        let mut file = candidates
            .iter()
            .find(|candidate| UnicodeWidthStr::width(candidate.as_str()) <= room)
            .cloned()
            .unwrap_or(short);
        while UnicodeWidthStr::width(file.as_str()) > room && !file.is_empty() {
            file.remove(0);
        }
        let file_width = UnicodeWidthStr::width(file.as_str()) as u16;
        let file_x = area.right().saturating_sub(file_width + 1);
        buffer.set_stringn(
            file_x,
            area.y,
            &file,
            usize::from(file_width),
            Style::default().fg(if self.chrome.dirty {
                theme.warn
            } else if self.chrome.prebake.is_some() {
                theme.mini
            } else if self.chrome.replay {
                theme.replay_colour()
            } else {
                theme.muted
            }),
        );

        // Engine pressure has priority over process-wide counters: this is
        // the deadline information that says whether playback can keep up.
        // It only appears at Full - the deadline detail a tuning session
        // wants, not a glance mid-set.
        let detail = self.chrome.metric_detail;
        let mut counters_right = file_x;
        if detail.shows_pressure()
            && let Some(pressure) = self.chrome.pressure
        {
            let engine = engine_pressure_text(pressure);
            let engine_width = UnicodeWidthStr::width(engine.as_str()) as u16;
            if area.width >= 100 && counters_right > left_end + engine_width + 3 {
                let engine_x = counters_right.saturating_sub(engine_width + 3);
                buffer.set_stringn(
                    engine_x,
                    area.y,
                    engine,
                    usize::from(engine_width),
                    Style::default().fg(engine_pressure_color(pressure.level, theme)),
                );
                counters_right = engine_x;
            }
        }

        // Process counters remain useful secondary context when there is
        // room, and are skipped entirely at None - a performance screen
        // that would rather say nothing about the machine than something.
        // The whole run is the memory breakdown's button: cleared first,
        // so a frame that draws no counters leaves no button behind them.
        super::memory::set_memory_chip(None);
        if detail.shows_process() {
            let stats = stats_text(self.chrome, detail);
            let stats_width = UnicodeWidthStr::width(stats.as_str()) as u16;
            if area.width >= 100 && counters_right > left_end + stats_width + 3 {
                let stats_x = counters_right.saturating_sub(stats_width + 3);
                super::memory::set_memory_chip(Some((stats_x, area.y, stats_width)));
                buffer.set_stringn(
                    stats_x,
                    area.y,
                    stats,
                    usize::from(stats_width),
                    Style::default().fg(theme.muted),
                );
            }
        }
        if let Some(line) = &self.chrome.loading {
            draw_loading_line(buffer, area, line, theme);
        }
    }
}

/// The loading line over the header row as drawn: an underline in the
/// accent from the left edge to `line.reach` (two cells at least), or a
/// faint accent tint where the terminal cannot colour an underline. The
/// text and its colours stay.
fn draw_loading_line(buffer: &mut Buffer, area: Rect, line: &LoadingLine, theme: &Theme) {
    // A stub from the start, so a load of one file shows before it lands.
    let reach = ((f64::from(area.width) * line.reach.clamp(0.0, 1.0)).round() as u16).max(2);
    let underline = loading_line_colour(theme);
    let tint = super::theme::mix(theme.surface, theme.accent, 0.2);
    for x in area.x..area.x.saturating_add(reach.min(area.width)) {
        let Some(cell) = buffer.cell_mut((x, area.y)) else {
            continue;
        };
        if line.underline {
            cell.modifier.insert(Modifier::UNDERLINED);
            cell.underline_color = underline;
        } else if tint != theme.surface {
            cell.bg = tint;
        } else {
            // A palette theme has no tint to blend: a plain underline.
            cell.modifier.insert(Modifier::UNDERLINED);
        }
    }
}

/// One orbit on the footer: its number, a four-cell meter, its output
/// pair - `3 ▮▮▮▯ 5/6`. The pair is the click target when there is more
/// than one.
fn render_orbit_chip(
    buffer: &mut Buffer,
    rect: Rect,
    level: &OrbitLevel,
    output_pairs: u16,
    theme: &Theme,
) {
    if rect.is_empty() {
        return;
    }
    let db = if level.peak > 0.0 {
        20.0 * level.peak.log10()
    } else {
        -120.0
    };
    let lit = if db >= -3.0 {
        4
    } else if db >= -12.0 {
        3
    } else if db >= -24.0 {
        2
    } else if db >= -48.0 {
        1
    } else {
        0
    };
    let color = if db >= -1.0 {
        theme.meter.peak
    } else if db >= -6.0 {
        theme.meter.high
    } else if db >= -18.0 {
        theme.meter.mid
    } else {
        theme.meter.low
    };
    buffer.set_stringn(
        rect.x,
        rect.y,
        format!("{}", level.orbit),
        2,
        Style::default().fg(theme.accent),
    );
    for cell in 0..4u16 {
        let (symbol, fg) = if cell < lit {
            (crate::terminal::symbol("▮"), color)
        } else {
            ("▯", theme.rule)
        };
        buffer.set_stringn(
            rect.x + 2 + cell,
            rect.y,
            symbol,
            1,
            Style::default().fg(fg),
        );
    }
    let pair = u16::from(level.pair);
    let label = format!("{}/{}", pair * 2 + 1, pair * 2 + 2);
    let style = if output_pairs > 1 {
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::UNDERLINED)
    } else {
        Style::default().fg(theme.muted)
    };
    buffer.set_stringn(
        rect.x + 7,
        rect.y,
        &label,
        usize::from(rect.width.saturating_sub(7)),
        style,
    );
}

/// The beat as a dot: four beats to the cycle, `●` on the beat fading to
/// `◉`, the downbeat in the ok colour and the others in the accent, all of
/// it muted while nothing plays.
pub fn beat_pulse(playing: bool, cycle: Option<f64>, theme: &Theme) -> (&'static str, Color) {
    let Some(cycle) = cycle.filter(|_| playing) else {
        return (super::terminal::symbol("◉"), theme.muted);
    };
    let beat = cycle * 4.0;
    let phase = beat.rem_euclid(1.0);
    let downbeat = beat.rem_euclid(4.0) < 1.0;
    let brightness = (1.0 - phase).powi(3) as f32;
    let color = if downbeat { theme.ok } else { theme.accent };
    let symbol = if phase < 0.2 { "●" } else { "◉" };
    (
        super::terminal::symbol(symbol),
        mix(theme.muted, color, brightness),
    )
}

fn stats_text(chrome: &StudioChrome<'_>, detail: settings::MetricDetail) -> String {
    let cpu = chrome
        .stats
        .cpu_percent
        .map(format_cpu_percent)
        .unwrap_or_else(|| "-".to_owned());
    let memory = chrome
        .stats
        .memory_bytes()
        .map(format_bytes)
        .unwrap_or_else(|| "-".to_owned());
    // `sys` sits right beside `cpu` rather than after `mem`, so the two
    // numbers a player actually wants to compare - mine against the whole
    // machine's - read next to each other.
    let machine = if detail.shows_machine() {
        let machine = chrome
            .stats
            .machine_cpu_percent
            .map(format_cpu_percent)
            .unwrap_or_else(|| "-".to_owned());
        format!(" · sys {machine}")
    } else {
        String::new()
    };
    format!(
        "cpu {cpu}{machine} · mem {memory} · {:.0}fps {:.1}ms",
        chrome.fps, chrome.render_ms
    )
}

fn engine_pressure_text(pressure: &EnginePressureSnapshot) -> String {
    let percent = |basis_points: u64| basis_points.saturating_add(50) / 100;
    format!(
        "DSP {}%  sched {}%  voices {}/{}  cover {}ms",
        percent(pressure.dsp_load_basis_points()),
        percent(pressure.scheduler_load_basis_points()),
        pressure.active_voices(),
        pressure.semantic_voice_capacity(),
        pressure.cover_millis(),
    )
}

fn engine_pressure_color(level: EnginePressureLevel, theme: &Theme) -> Color {
    match level {
        EnginePressureLevel::Normal => theme.ok,
        EnginePressureLevel::Caution => theme.accent,
        EnginePressureLevel::Warning => theme.warn,
        EnginePressureLevel::Error => theme.error,
    }
}

/// Regions of the footer that respond to a click.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FooterHits {
    /// The output-device chip.
    pub device_chip: Rect,
    /// The MIDI-port chip.
    pub midi_chip: Rect,
    /// The master dock (meter, fader, readouts).
    pub dock: Rect,
    /// The draggable level bar inside the dock.
    pub meter: Rect,
    /// The master scope, when there is room for one.
    pub scope: Rect,
    /// The status line, whose path is clickable when it names a file.
    pub status: Rect,
    /// The orbit chips on the status row, lowest orbit first.
    pub orbits: [(u8, Rect); MAX_ORBIT_CHIPS],
    pub orbit_count: usize,
}

/// The remote-control icon sits at the left of the status line, with a
/// blank cell before the status message. Paint and hit testing use the
/// icon's two cells; short status lines keep their room for a useful message.
pub fn remote_indicator_rect(status: Rect, enabled: bool) -> Rect {
    const WIDTH: u16 = 2; // "<>"
    if !enabled || status.height == 0 || status.width < 20 {
        return Rect::default();
    }
    Rect::new(status.x, status.y, WIDTH, 1)
}

/// The most orbit chips the footer shows.
pub const MAX_ORBIT_CHIPS: usize = 8;
/// One chip: `3 ▮▮▮▯ 5/6`.
const ORBIT_CHIP_WIDTH: u16 = 12;

/// Keep status, devices and master controls below notice and retained-note rows.
/// Tiny degraded layouts preserve the controls and give errors priority.
/// `show_footer` off leaves the status line alone under the notices.
pub(crate) fn footer_content_area(
    area: Rect,
    notice_rows: u16,
    has_notes: bool,
    show_footer: bool,
) -> Rect {
    let content_rows = 1 + u16::from(show_footer);
    let extra =
        (notice_rows.min(2) + u16::from(has_notes)).min(area.height.saturating_sub(content_rows));
    Rect::new(area.x, area.y + extra, area.width, area.height - extra)
}

/// The status line alone: an error and the caret's line:column, with no
/// meter, orbits or device chips. What the footer setting leaves when it
/// is off.
pub fn status_footer_hits(area: Rect) -> FooterHits {
    if area.is_empty() {
        return FooterHits::default();
    }
    FooterHits {
        status: Rect::new(
            area.x.saturating_add(1),
            area.y,
            area.width.saturating_sub(2),
            1.min(area.height),
        ),
        ..FooterHits::default()
    }
}

/// Where the footer draws its chips and its dock, computed the same way it
/// renders them. The dock sits at the bottom right, like the master section
/// of a desk, with the scope to its left.
pub fn footer_hits(
    area: Rect,
    chrome_device: Option<&str>,
    chrome_input: Option<&str>,
    midi_ports: MidiPortCounts,
    pads: usize,
    orbits: &[OrbitLevel],
) -> FooterHits {
    if area.is_empty() {
        return FooterHits::default();
    }
    let dock_width = if area.width >= WIDE_FOOTER_WIDTH {
        DOCK_WIDTH
    } else {
        DOCK_WIDTH_NARROW
    }
    .min(area.width.saturating_sub(24));
    let dock = Rect::new(
        area.right().saturating_sub(dock_width + 1),
        area.y,
        dock_width,
        area.height.min(2),
    );
    // The scope takes what is free between the status line's own third and
    // the dock, up to its full width: a terminal too narrow for all of it
    // still gets a smaller one, and only a row with nothing to spare goes
    // without. (A hundred and ten columns used to be the price of any
    // scope at all, which is a common terminal width to be just under.)
    // The status keeps a third of the row, but no more than a sentence's
    // worth: past that the cells are better spent on the scope and the
    // orbit chips, and the status still takes everything they leave.
    let least_status = (area.width / 3).clamp(STATUS_LEAST_WIDTH, STATUS_RESERVED_WIDTH);
    // The cell between the status and the scope is the gap they need to
    // read as two things rather than one run of text.
    let free = dock.x.saturating_sub(area.x + 2 + least_status);
    let scope_width = if area.height >= 2 && settings::master_scope() && free >= SCOPE_MIN_WIDTH {
        SCOPE_WIDTH.min(free)
    } else {
        0
    };
    let scope = Rect::new(
        dock.x.saturating_sub(scope_width + 1),
        area.y,
        scope_width,
        area.height.min(2),
    );
    let chip_y = area.y + u16::from(area.height >= 2);
    let device = device_chip_text(chrome_device, chrome_input);
    let midi = devices_chip_text(midi_ports, pads);
    let device_width = UnicodeWidthStr::width(device.as_str()) as u16;
    let midi_width = UnicodeWidthStr::width(midi.as_str()) as u16;
    let x = area.x + 1;
    let mut status_right = if scope.is_empty() { dock.x } else { scope.x }.saturating_sub(1);
    // Orbit chips sit at the right of the status row, the highest orbits
    // kept when there is no room for all; the status text takes what is
    // left, never less than a third of the row.
    let mut orbit_hits = [(0u8, Rect::default()); MAX_ORBIT_CHIPS];
    let mut orbit_count = 0;
    let min_status = least_status;
    let room = status_right.saturating_sub(x + min_status) / (ORBIT_CHIP_WIDTH + 1);
    let shown = orbits.len().min(MAX_ORBIT_CHIPS).min(usize::from(room));
    if shown > 0 {
        let first = orbits.len() - shown;
        let mut chip_x = status_right.saturating_sub(shown as u16 * (ORBIT_CHIP_WIDTH + 1));
        for level in &orbits[first..] {
            orbit_hits[orbit_count] = (level.orbit, Rect::new(chip_x, area.y, ORBIT_CHIP_WIDTH, 1));
            orbit_count += 1;
            chip_x += ORBIT_CHIP_WIDTH + 1;
        }
        status_right = status_right.saturating_sub(shown as u16 * (ORBIT_CHIP_WIDTH + 1) + 1);
    }
    let device_chip = Rect::new(x, chip_y, device_width.min(area.width), 1);
    let midi_chip = Rect::new(
        (x + device_width + 2).min(area.right()),
        chip_y,
        midi_width.min(area.width),
        1,
    );
    // A chip keeps its cells whenever the row has room for it, and loses the
    // rect along with the paint when it does not - one source of truth, so a
    // click can never land on a chip that was not drawn.
    let fits = |chip: Rect| chip.right() <= status_right;
    FooterHits {
        orbits: orbit_hits,
        orbit_count,
        status: Rect::new(x, area.y, status_right.saturating_sub(x), 1),
        device_chip: if area.height >= 2 && fits(device_chip) {
            device_chip
        } else {
            Rect::default()
        },
        midi_chip: if area.height >= 2 && fits(midi_chip) {
            midi_chip
        } else {
            Rect::default()
        },
        dock,
        meter: MasterDock::geometry(dock).unwrap_or_default(),
        scope,
    }
}

fn device_chip_text(device: Option<&str>, input: Option<&str>) -> String {
    match (device, input) {
        (Some(device), Some(input)) => format!("♪ {device} · ♩ {input}"),
        (Some(device), None) => format!("♪ {device}"),
        (None, Some(input)) => format!("♪ no output · ♩ {input}"),
        (None, None) => "♪ no output".to_owned(),
    }
}

/// How many cells of the device label the current input peak lights. Full
/// scale reaches the end of the label; the 250 ms activity light keeps one
/// cell on between snapshots so a quiet microphone does not strobe.
fn input_chip_lit_width(peak_db: f32, width: u16) -> u16 {
    let full_scale = scale_position(0.0);
    let fraction = (scale_position(peak_db) / full_scale).clamp(0.0, 1.0);
    ((f32::from(width) * fraction).ceil() as u16)
        .max(1)
        .min(width)
}

/// `⌁ 1 out · 0 in`, or `⌁ no midi`: the footer chip and the mixer desk
/// word the ports alike.
pub(crate) fn midi_chip_text(ports: MidiPortCounts) -> String {
    let symbol = crate::terminal::symbol("⌁");
    if ports.is_empty() {
        format!("{symbol} no midi")
    } else {
        format!("{symbol} {} out · {} in", ports.outputs, ports.inputs)
    }
}

/// The MIDI chip with the pads beside it: `⌁ 1 out · 0 in · ▣ 1 pad`.
fn devices_chip_text(ports: MidiPortCounts, pads: usize) -> String {
    match pads {
        0 => midi_chip_text(ports),
        1 => format!(
            "{} · {} 1 pad",
            midi_chip_text(ports),
            crate::terminal::symbol("▣")
        ),
        many => format!(
            "{} · {} {many} pads",
            midi_chip_text(ports),
            crate::terminal::symbol("▣")
        ),
    }
}

/// How many chips are off each end of the strip, and where the ones on it
/// land. Rendering and hit-testing both read this, so a chip is clickable
/// exactly where it is drawn.
pub struct SceneStripLayout {
    pub hits: Vec<Rect>,
    /// Chips before the first one drawn, and after the last.
    pub before: usize,
    pub after: usize,
}

/// Columns kept at each end for the `‹2` and `3›` counters.
const STRIP_MARKER: u16 = 3;

/// One dim separator cell between adjacent chips.
const STRIP_SEPARATOR: u16 = 1;

/// Where each chip of the strip lands, computed the same way it is drawn.
/// A chip is `▶1 name ♪c1/10 ●` with a space of padding either side.
///
/// Sixteen scenes need over two hundred columns, so on any ordinary
/// terminal the strip cannot show them all. It scrolls rather than
/// clipping, and what it scrolls to is the chip you are on: the whole
/// point of the strip is to say which scene is lit, and a lit chip that
/// has fallen off the end says nothing.
///
/// The window is derived from the chips themselves rather than kept as
/// state, so there is nothing to invalidate when a scene is added, closed
/// or renamed - and a rename that grows a chip cannot leave the strip
/// scrolled somewhere stale.
pub fn scene_strip_layout(area: Rect, chips: &[SceneChip]) -> SceneStripLayout {
    let empty = || SceneStripLayout {
        hits: vec![Rect::default(); chips.len()],
        before: 0,
        after: 0,
    };
    if area.is_empty() || chips.is_empty() {
        return empty();
    }
    let widths: Vec<u16> = chips
        .iter()
        .enumerate()
        .map(|(index, chip)| UnicodeWidthStr::width(chip_text(index, chip, None).as_str()) as u16)
        .collect();
    let current = chips.iter().position(|chip| chip.current).unwrap_or(0);

    // Seat as many as fit from `first`, and say whether `current` is among
    // them. Padding precedes the first chip; separators sit between chips.
    let seat = |first: usize, left: u16, right: u16| {
        let mut hits = vec![Rect::default(); chips.len()];
        let mut x = area.x.saturating_add(1).saturating_add(left);
        let mut last = first;
        for (index, width) in widths.iter().enumerate().skip(first) {
            if x.saturating_add(*width) > area.right().saturating_sub(right) {
                break;
            }
            hits[index] = Rect::new(x, area.y, *width, 1);
            x = x.saturating_add(*width).saturating_add(STRIP_SEPARATOR);
            last = index;
        }
        (hits, last)
    };

    // Everything at once, if it fits.
    let (hits, last) = seat(0, 0, 0);
    if last + 1 == chips.len() && !hits[chips.len() - 1].is_empty() {
        return SceneStripLayout {
            hits,
            before: 0,
            after: 0,
        };
    }

    // Otherwise scroll the least that keeps the current chip on screen,
    // so the strip only moves when it has to and the chips before the
    // current one stay put as long as they can.
    let mut first = 0usize;
    loop {
        let left = if first == 0 { 0 } else { STRIP_MARKER };
        let (hits, last) = seat(first, left, STRIP_MARKER);
        let seated_all = last + 1 == chips.len() && !hits[chips.len() - 1].is_empty();
        if !hits[current].is_empty() || first >= current {
            let right = if seated_all { 0 } else { STRIP_MARKER };
            // Re-seat without reserving the right marker when nothing is
            // after the last chip, so the final chip is not dropped to
            // make room for a counter of zero.
            let (hits, last) = if seated_all {
                (hits, last)
            } else {
                seat(first, left, right)
            };
            return SceneStripLayout {
                hits,
                before: first,
                after: chips.len().saturating_sub(last + 1),
            };
        }
        first += 1;
    }
}

/// The chip rectangles alone, for callers that only hit-test.
pub fn scene_strip_hits(area: Rect, chips: &[SceneChip]) -> Vec<Rect> {
    scene_strip_layout(area, chips).hits
}

fn chip_text(index: usize, chip: &SceneChip, renaming: Option<&str>) -> String {
    // A prebake wears its own mark and no number: it is not one of the
    // sixteen, and there is no key that plays it.
    let (marker, number) = match chip.prebake {
        Some(_) => (PREBAKE_GLYPH.to_string(), String::new()),
        None if chip.replay => (REPLAY_GLYPH.to_string(), String::new()),
        None => {
            let marker = if chip.armed.is_some() {
                "»"
            } else if chip.playing {
                "▶"
            } else {
                " "
            };
            (marker.to_owned(), (index + 1).to_string())
        }
    };
    let name = match renaming {
        Some(draft) => format!("{draft}{}", super::terminal::symbol("▏")),
        None => chip.name.clone(),
    };
    let pad = chip
        .pad
        .as_deref()
        .map(|pad| format!(" ♪{pad}"))
        .unwrap_or_default();
    let dirty = if chip.dirty {
        format!(" {}", super::terminal::symbol("●"))
    } else {
        String::new()
    };
    let errors = if chip.errors { " ✗" } else { "" };
    let rewind = if chip.rewind {
        format!(" {}", super::terminal::symbol("⟲"))
    } else {
        String::new()
    };
    let armed = chip
        .armed
        .map(|cycles| format!(" in {cycles:.1}"))
        .unwrap_or_default();
    format!("{marker}{number} {name}{rewind}{pad}{dirty}{errors}{armed} ")
}

/// The scene strip: one chip per scene, the current one lit, the sounding
/// one marked, and the chords that drive it at the right.
struct SceneStrip<'a> {
    pub keybinds: &'a super::keybinds::Keybinds,
    chips: &'a [SceneChip],
    mode: &'a SceneStripMode,
    split: bool,
    capabilities: KeyboardCapabilities,
    theme: &'a Theme,
}

impl Widget for SceneStrip<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let theme = self.theme;
        clear_surface(
            buffer,
            area,
            Style::default().bg(theme.surface).fg(theme.muted),
        );
        // A split gives the strip a second row: a dim rule that ends the
        // tabs, so the pane titles under it read as titles rather than as
        // more tabs. The chips keep the first row, and every hit test is
        // measured against it.
        if area.height > 1 {
            let rule = "\u{2500}".repeat(usize::from(area.width));
            buffer.set_stringn(
                area.x,
                area.bottom() - 1,
                rule,
                usize::from(area.width),
                Style::default().fg(theme.rule).bg(theme.surface),
            );
        }
        let area = Rect::new(area.x, area.y, area.width, 1);
        let SceneStripLayout {
            hits,
            before,
            after,
        } = scene_strip_layout(area, self.chips);
        // What is off each end, said rather than left to be discovered.
        // The counters sit in the muted colour so they read as chrome
        // beside the chips rather than as another scene.
        let counter = Style::default().fg(theme.muted).bg(theme.surface);
        if before > 0 {
            buffer.set_stringn(area.x, area.y, format!("‹{before}"), 3, counter);
        }
        if after > 0 {
            let text = format!("{after}›");
            let at = area
                .right()
                .saturating_sub(UnicodeWidthStr::width(text.as_str()) as u16);
            buffer.set_stringn(at, area.y, text, 3, counter);
        }
        let mut preceding_chip = false;
        for (index, (chip, hit)) in self.chips.iter().zip(&hits).enumerate() {
            if hit.is_empty() {
                continue;
            }
            if preceding_chip {
                buffer.set_string(hit.x - STRIP_SEPARATOR, hit.y, "│", counter);
            }
            preceding_chip = true;
            let renaming = match self.mode {
                SceneStripMode::Renaming(draft) if chip.current => Some(draft.as_str()),
                _ => None,
            };
            let text = chip_text(index, chip, renaming);
            // Setup reads in its own colour wherever it appears, so a
            // glance at the strip never mistakes it for a scene.
            let style = match (chip.current, chip.prebake.is_some(), chip.replay) {
                (true, true, _) => Style::default()
                    .fg(theme.background)
                    .bg(theme.mini)
                    .add_modifier(Modifier::BOLD),
                (true, false, true) => Style::default()
                    .fg(theme.background)
                    .bg(theme.replay_colour())
                    .add_modifier(Modifier::BOLD),
                (true, false, false) => Style::default()
                    .fg(theme.background)
                    .bg(theme.accent)
                    .add_modifier(Modifier::BOLD),
                (false, true, _) => Style::default().fg(theme.mini),
                (false, false, true) => Style::default().fg(theme.replay_colour()),
                (false, false, false) if chip.playing => Style::default().fg(theme.ok),
                // Quieter than the text under it. A tab nobody is on is
                // somewhere to go, not something to read. The strip sits
                // one row above a pane's own title, which names the scene
                // you are on, so the muted colour keeps the two rows apart.
                (false, false, false) => Style::default().fg(theme.muted),
            };
            // A rename draft may be wider than the chip the hits were
            // measured for; it simply runs on to the right.
            let width = UnicodeWidthStr::width(text.as_str()) as u16;
            buffer.set_stringn(
                hit.x,
                hit.y,
                text,
                usize::from(area.right().saturating_sub(hit.x).min(width)),
                style,
            );
        }
        let used = hits
            .iter()
            .filter(|hit| !hit.is_empty())
            .map(|hit| hit.right())
            .max()
            .unwrap_or(area.x);

        // The hint shrinks before it disappears: the full form on a wide
        // terminal, the chords alone on a narrower one.
        use super::keybinds::BindAction;
        let previous_scene = menu::scene_shortcut_hint(
            self.keybinds,
            self.capabilities,
            BindAction::PreviousScene,
            self.split,
        );
        let next_scene = menu::scene_shortcut_hint(
            self.keybinds,
            self.capabilities,
            BindAction::NextScene,
            self.split,
        );
        let update = self.keybinds.hint(BindAction::Evaluate);
        let close = self.keybinds.hint(BindAction::CloseScene);
        let settings = self.keybinds.hint(BindAction::Settings);
        let hints: Vec<String> = match self.mode {
            SceneStripMode::Renaming(_) => vec!["Enter renames · Esc cancels".to_owned()],
            SceneStripMode::Learning => {
                vec!["hit a pad on your controller · Esc cancels".to_owned()]
            }
            // On a prebake tab the scene chords are the wrong advice:
            // there is nothing here to rename, launch or learn.
            SceneStripMode::Idle
                if self
                    .chips
                    .iter()
                    .any(|chip| chip.current && chip.prebake.is_some()) =>
            {
                let switch = [previous_scene.as_str(), next_scene.as_str()]
                    .into_iter()
                    .filter(|hint| !hint.is_empty())
                    .collect::<Vec<_>>()
                    .join("/");
                let keys = [
                    switch.as_str(),
                    update.as_str(),
                    close.as_str(),
                    settings.as_str(),
                ];
                let row = |labels: [&str; 4]| {
                    keys.into_iter()
                        .zip(labels)
                        .filter(|(key, _)| !key.is_empty())
                        .map(|(key, label)| {
                            if label.is_empty() {
                                key.to_owned()
                            } else {
                                format!("{key} {label}")
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" · ")
                };
                [
                    row(["switch", "applies the prebake", "closes it", "reopens it"]),
                    row(["", "applies", "closes", "reopens"]),
                    row(["", "", "", ""]),
                ]
                .into_iter()
                .filter(|hint| !hint.is_empty())
                .collect()
            }
            // Nothing at rest: the Scene menu holds every scene chord, with
            // its accelerator beside it. What stays here is what the menu
            // cannot say: the modes above, where the keyboard means
            // something it does not otherwise mean.
            SceneStripMode::Idle => Vec::new(),
        };
        let Some(hint) = hints.into_iter().find(|hint| {
            let width = UnicodeWidthStr::width(hint.as_str()) as u16;
            area.right().saturating_sub(width + 1) > used + 2
        }) else {
            return;
        };
        let hint_width = UnicodeWidthStr::width(hint.as_str()) as u16;
        let hint_x = area.right().saturating_sub(hint_width + 1);
        {
            buffer.set_stringn(
                hint_x,
                area.y,
                hint,
                usize::from(hint_width),
                Style::default().fg(match self.mode {
                    SceneStripMode::Idle => theme.muted,
                    _ => theme.warn,
                }),
            );
        }
    }
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

struct Footer<'a, 'b> {
    chrome: &'a StudioChrome<'b>,
}

pub(super) fn render_piano_notes(buffer: &mut Buffer, area: Rect, notes: &str, theme: &Theme) {
    if area.is_empty() {
        return;
    }
    let area = Rect { height: 1, ..area };
    let style = Style::default().bg(theme.surface).fg(theme.foreground);
    clear_surface(buffer, area, style);
    buffer.set_stringn(
        area.x,
        area.y,
        ellipsize(notes, area.width),
        usize::from(area.width),
        style,
    );
}

/// A compact mode indicator usable in the ordinary footer or a spare Zen row.
/// The caller owns its room; the label fades while note names stay steady.
pub(super) fn render_piano_indicator(
    buffer: &mut Buffer,
    area: Rect,
    status: &str,
    theme: &Theme,
    pulse: f32,
) {
    if area.is_empty() {
        return;
    }
    let area = Rect { height: 1, ..area };
    clear_surface(
        buffer,
        area,
        Style::default().bg(theme.surface).fg(theme.foreground),
    );
    let status = super::terminal::safe_text(status);
    buffer.set_stringn(
        area.x,
        area.y,
        status,
        usize::from(area.width),
        Style::default().fg(theme.foreground),
    );
    // Terminal-owned palette colours cannot be interpolated reliably. Keep
    // those steady instead of turning the smooth fade into a hard blink.
    let background = if super::theme::true_rgb(theme.surface).is_some()
        && super::theme::true_rgb(theme.accent).is_some()
    {
        mix(theme.surface, theme.accent, pulse)
    } else {
        theme.accent
    };
    let style = Style::default()
        .fg(theme.surface)
        .bg(background)
        .add_modifier(Modifier::BOLD);
    buffer.set_stringn(area.x, area.y, "PIANO", usize::from(area.width), style);
}

impl Widget for Footer<'_, '_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let theme = self.chrome.theme;
        clear_surface(
            buffer,
            area,
            Style::default().bg(theme.surface).fg(theme.muted),
        );
        let warning = self.chrome.audio_warning.map(|message| {
            if UnicodeWidthStr::width(message) > usize::from(area.width.saturating_sub(2)) {
                "Try raising latency: Settings → Advanced"
            } else {
                message
            }
        });
        let content = footer_content_area(
            area,
            u16::from(self.chrome.error.is_some()) + u16::from(warning.is_some()),
            self.chrome.piano_notes.is_some(),
            self.chrome.show_footer,
        );
        // Allocate spare rows to errors, retained piano notes, then audio advice.
        let mut notice_y = area.y;
        if notice_y < content.y
            && let Some(message) = self.chrome.error
        {
            let message = super::terminal::safe_text(message);
            buffer.set_stringn(
                area.x.saturating_add(1),
                notice_y,
                ellipsize(&message, area.width.saturating_sub(2)),
                usize::from(area.width.saturating_sub(2)),
                Style::default().fg(theme.error),
            );
            notice_y += 1;
        }
        if notice_y < content.y
            && let Some(notes) = self.chrome.piano_notes
        {
            render_piano_notes(
                buffer,
                Rect::new(
                    area.x.saturating_add(1),
                    notice_y,
                    area.width.saturating_sub(2),
                    1,
                ),
                notes,
                theme,
            );
            notice_y += 1;
        }
        // A retained query error must not hide sustained-audio advice. Give
        // both full-width rows; in a cramped terminal errors and piano notes
        // take priority over the optional hint.
        if notice_y < content.y
            && let Some(message) = warning
        {
            let message = super::terminal::safe_text(message);
            buffer.set_stringn(
                area.x.saturating_add(1),
                notice_y,
                ellipsize(&message, area.width.saturating_sub(2)),
                usize::from(area.width.saturating_sub(2)),
                Style::default().fg(theme.warn),
            );
        }
        let hits = if self.chrome.show_footer {
            footer_hits(
                content,
                self.chrome.device,
                self.chrome.input,
                self.chrome.midi_ports,
                self.chrome.pads,
                self.chrome.orbits,
            )
        } else {
            status_footer_hits(content)
        };
        // Everything textual stops short of the orbit chips, the scope and
        // the dock.
        let remote_indicator = remote_indicator_rect(hits.status, self.chrome.remote_control);
        let right_edge = hits.status.right();
        let message_left = if remote_indicator.is_empty() {
            content.x.saturating_add(1)
        } else {
            remote_indicator.right().saturating_add(1)
        };
        for (index, (orbit, rect)) in hits.orbits.iter().enumerate() {
            if index >= hits.orbit_count {
                break;
            }
            let Some(level) = self
                .chrome
                .orbits
                .iter()
                .find(|level| level.orbit == *orbit)
            else {
                continue;
            };
            render_orbit_chip(buffer, *rect, level, self.chrome.output_pairs, theme);
        }

        // Piano mode must remain visible while typing plays notes. Errors keep
        // their dedicated row; a terminal too short for that still shows them.
        let inline_notice = content.y == area.y;
        let piano = self
            .chrome
            .piano
            .filter(|_| !inline_notice || self.chrome.error.is_none());
        let (message, color) = match (
            inline_notice,
            self.chrome.error,
            piano,
            warning,
            self.chrome.lint,
        ) {
            (true, Some(error), _, _, _) => (error.to_owned(), theme.error),
            (_, _, Some(_), _, _) => (self.chrome.status.to_owned(), theme.foreground),
            (true, _, _, Some(warning), _) => (warning.to_owned(), theme.warn),
            (_, _, _, _, Some(lint)) => (format!("✗ {lint}"), theme.warn),
            _ => (self.chrome.status.to_owned(), theme.foreground),
        };
        // The caret's line and column, a vim-style ruler (`2,4`), shown the
        // way vim's own ruler always is. The gutter can be turned off and
        // the column is shown nowhere else, so it stays put right of the
        // message with one cell of gap before it; the message gives way
        // instead, cut to fit with a trailing ellipsis, and only when the
        // status area cannot even hold the ruler, its gap and an ellipsis
        // does the ruler step aside and the message take the whole row.
        // Whatever the sentence, and wherever in the studio it was
        // written: the status line is read more often than anything else
        // on the screen, and threading the glyph table through every
        // status string in the place would miss one.
        let message = super::terminal::safe_text(&message).into_owned();
        let mut message = message;
        let mut message_right = right_edge;
        if self.chrome.piano.is_none()
            && let Some((line, column)) = self.chrome.caret
        {
            let ruler = format!("{line},{column}");
            let ruler_width = UnicodeWidthStr::width(ruler.as_str()) as u16;
            if right_edge.saturating_sub(message_left) >= ruler_width + 2 {
                let ruler_x = right_edge.saturating_sub(ruler_width);
                buffer.set_stringn(
                    ruler_x,
                    content.y,
                    ruler,
                    usize::from(ruler_width),
                    Style::default().fg(theme.muted),
                );
                message_right = ruler_x.saturating_sub(1);
                let room = message_right.saturating_sub(message_left);
                message = ellipsize(&message, room);
            }
        }
        if let Some(pulse) = piano {
            render_piano_indicator(
                buffer,
                Rect::new(
                    message_left,
                    content.y,
                    message_right.saturating_sub(message_left),
                    1,
                ),
                &message,
                theme,
                pulse,
            );
        } else {
            buffer.set_stringn(
                message_left,
                content.y,
                message,
                usize::from(message_right.saturating_sub(message_left)),
                Style::default().fg(color),
            );
        }
        if !remote_indicator.is_empty() {
            let style = if self.chrome.remote_receiving {
                Style::default().fg(theme.ok).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.muted)
            };
            buffer.set_stringn(
                remote_indicator.x,
                remote_indicator.y,
                "<>",
                usize::from(remote_indicator.width),
                style,
            );
        }

        // Stopped, the widget draws its own centre line: the meter's
        // neighbour is an instrument at rest rather than a hole.
        if !hits.scope.is_empty() {
            // The footer's scope is the docks' own widget in its line
            // style and the theme's colours: the mix as a trace, the same
            // picture at a glance that a dock shows in full.
            super::viz_panel::ScopeView {
                audio: self.chrome.audio,
                motion: self.chrome.motion,
                style: super::viz_panel::ScopeStyle::Line,
                look: super::viz_panel::Look {
                    theme,
                    colour: super::viz_panel::Colouring::Theme,
                    seconds: 0.0,
                    level: 0.0,
                },
            }
            .render(hits.scope, buffer);
        }
        MasterDock {
            keybinds: self.chrome.keybinds,
            state: self.chrome.master,
            theme,
            now: self.chrome.now,
            playing: self.chrome.playing,
            limiter: self.chrome.master_limiter,
        }
        .render(hits.dock, buffer);

        if content.height < 2 {
            return;
        }
        // A chip with no rect was not drawn, and cannot be clicked.
        // The chips are the lights: a device that is doing something right
        // now - signal on the input, a message on a MIDI port, a thumb on a
        // pad - is drawn lit, so what is connected can be seen working
        // before a score asks it anything.
        if !hits.device_chip.is_empty() {
            buffer.set_stringn(
                hits.device_chip.x,
                hits.device_chip.y,
                device_chip_text(self.chrome.device, self.chrome.input),
                usize::from(hits.device_chip.width),
                Style::default().fg(theme.accent),
            );
            // `set_stringn` leaves the continuation cell of a wide glyph
            // unstyled; give the whole chip its idle colour first.
            buffer.set_style(hits.device_chip, Style::default().fg(theme.accent));
            if self.chrome.input_active && self.chrome.input.is_some() {
                let lit_width =
                    input_chip_lit_width(self.chrome.input_peak_db, hits.device_chip.width);
                let lit_color = legible_against_floor(theme.ok, theme.surface, 3.0);
                for x in hits.device_chip.x..hits.device_chip.x + lit_width {
                    if let Some(cell) = buffer.cell_mut((x, hits.device_chip.y)) {
                        cell.set_fg(lit_color)
                            .set_style(Style::default().add_modifier(Modifier::BOLD));
                    }
                }
            }
        }
        if !hits.midi_chip.is_empty() {
            let style = if self.chrome.midi_active || self.chrome.pad_active {
                Style::default().fg(theme.ok).add_modifier(Modifier::BOLD)
            } else if !self.chrome.midi_ports.is_empty() || self.chrome.pads > 0 {
                Style::default().fg(theme.accent)
            } else {
                Style::default().fg(theme.muted)
            };
            buffer.set_stringn(
                hits.midi_chip.x,
                hits.midi_chip.y,
                devices_chip_text(self.chrome.midi_ports, self.chrome.pads),
                usize::from(hits.midi_chip.width),
                style,
            );
        }
    }
}
/// Draw the toast: centred near the top, over everything.
///
/// Deliberately not the status line. A confirmation competes with whatever the
/// status was already saying, and the thing being confirmed usually made the
/// panel disappear - so it goes where the eye already is.
///
/// `top` is the first row the chrome above it does not own, so the toast
/// does not land on the menu bar or the header.
fn render_toast(buffer: &mut Buffer, area: Rect, top: u16, text: &str, theme: &Theme) {
    let width = (UnicodeWidthStr::width(text) as u16).saturating_add(4);
    if area.width < width || area.height < 3 {
        return;
    }
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = top.min(area.bottom().saturating_sub(1));
    let box_area = Rect::new(x, y, width, 1);
    clear_surface(
        buffer,
        box_area,
        Style::default().bg(theme.accent).fg(theme.background),
    );
    buffer.set_stringn(
        x + 2,
        y,
        text,
        usize::from(width.saturating_sub(3)),
        Style::default()
            .bg(theme.accent)
            .fg(theme.background)
            .add_modifier(Modifier::BOLD),
    );
}

struct EditorPane<'a> {
    editor: &'a Editor,
    map: &'a ScreenMap,
    line_numbers: bool,
    visual: &'a VisualState,
    decorations: Decorations<'a>,
    theme: &'a Theme,
    area: Rect,
}

impl Widget for EditorPane<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let theme = self.theme;
        buffer.set_style(
            area,
            Style::default().bg(theme.background).fg(theme.foreground),
        );
        let gutter = gutter_width(self.editor, self.line_numbers);
        let current_line = self
            .editor
            .document()
            .line_of(self.editor.primary_selection().head)
            .unwrap_or(0);
        // Inline widgets describe the last audible generation and remain in
        // place while its source is edited.
        let visuals = self
            .visual
            .layout()
            .map(|layout| {
                layout
                    .visuals
                    .iter()
                    .map(|visual| {
                        (
                            visual.id.as_str(),
                            (visual.kind.as_str(), visual.slot, visual.options.as_str()),
                        )
                    })
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let decorations = DecorationIndex::new(self.map, self.editor, self.decorations);

        // Keep the full virtual-widget geometry even when its top or bottom
        // is outside the viewport. Rendering at the clipped height would
        // rescale a scope or piano roll while scrolling and make it jump.
        let mut virtual_groups = BTreeMap::<&str, (u16, u16, u16, u16)>::new();
        for row in self.map.rows() {
            match row {
                ScreenRow::Text(row) => render_text_row(
                    buffer,
                    row,
                    self.area.x,
                    self.area.width,
                    gutter,
                    current_line,
                    &decorations,
                    self.decorations.sliders,
                    theme,
                    row_tokens(self.editor.document(), row),
                ),
                ScreenRow::Virtual(row) => {
                    let entry = virtual_groups.entry(row.id.as_ref()).or_insert((
                        row.screen_y,
                        0,
                        row.inner_row,
                        row.height,
                    ));
                    entry.1 = entry.1.saturating_add(1);
                    entry.2 = entry.2.min(row.inner_row);
                    if gutter > MARGIN_WIDTH
                        && let Some(cell) =
                            buffer.cell_mut((self.area.x + gutter - 1, row.screen_y))
                    {
                        cell.set_symbol("┊").set_fg(theme.rule);
                    }
                }
            }
        }

        for (id, (y, height, offset, full_height)) in virtual_groups {
            let visual_area = Rect::new(
                self.area.x.saturating_add(gutter),
                y,
                self.area.width.saturating_sub(gutter),
                height,
            );
            if let Some((kind, slot, options)) = visuals.get(id).copied() {
                render_clipped_visual(
                    kind,
                    slot,
                    options,
                    self.visual,
                    theme,
                    theme.background,
                    visual_area,
                    offset,
                    full_height,
                    buffer,
                );
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct VisibleCell {
    key: (u16, u16),
    from: usize,
    to: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct CellDecorations {
    mini: bool,
    /// Which of the row's slider chips this cell belongs to.
    slider: Option<u8>,
    error: bool,
    selected: bool,
    /// The sounding event's colour and how strongly it still marks the cell.
    active: Option<(Color, f32)>,
    /// A bracket at the caret: `Some(true)` matched, `Some(false)` orphan.
    bracket: Option<bool>,
    /// A tempo call an outside clock overrides.
    overridden: bool,
}

/// Per-frame decoration lookup. Building it assigns each visible cell at
/// most once per decoration layer, so thousands of source ranges do not turn
/// into thousands of comparisons for every terminal cell.
struct DecorationIndex {
    caret_shape: super::terminal::CaretShape,
    by_screen_cell: HashMap<(u16, u16), CellDecorations>,
}

impl DecorationIndex {
    fn new(map: &ScreenMap, editor: &Editor, decorations: Decorations<'_>) -> Self {
        let mut cells = map
            .rows()
            .iter()
            .filter_map(|row| match row {
                ScreenRow::Text(row) => Some(row),
                ScreenRow::Virtual(_) => None,
            })
            .flat_map(|row| {
                row.cells.iter().map(|cell| VisibleCell {
                    key: (cell.screen_x.start, row.screen_y),
                    from: cell.bytes.start.0,
                    to: cell.bytes.end.0,
                })
            })
            .collect::<Vec<_>>();
        cells.sort_unstable_by_key(|cell| (cell.from, cell.to, cell.key.1, cell.key.0));
        debug_assert!(cells.windows(2).all(|pair| pair[0].to <= pair[1].from));

        let mut mini_cells = vec![None; cells.len()];
        let _ = assign_first_overlapping(
            &cells,
            decorations.mini.iter().map(|&(from, to)| (from, to, ())),
            &mut mini_cells,
        );

        let mut slider_cells = vec![None; cells.len()];
        let _ = assign_first_overlapping(
            &cells,
            decorations
                .sliders
                .iter()
                .enumerate()
                .map(|(index, chip)| (chip.from, chip.to, index.min(u8::MAX as usize) as u8)),
            &mut slider_cells,
        );

        let mut error_cells = vec![None; cells.len()];
        let _ = assign_first_overlapping(
            &cells,
            decorations.errors.iter().map(|&(from, to)| (from, to, ())),
            &mut error_cells,
        );

        let mut overridden_cells = vec![None; cells.len()];
        let _ = assign_first_overlapping(
            &cells,
            decorations
                .overridden
                .iter()
                .map(|&(from, to)| (from, to, ())),
            &mut overridden_cells,
        );

        let mut bracket_cells = vec![None; cells.len()];
        let _ = assign_first_overlapping(
            &cells,
            decorations
                .brackets
                .iter()
                .map(|&(from, to, matched)| (from, to, matched)),
            &mut bracket_cells,
        );

        let mut selected_cells = vec![None; cells.len()];
        let selections = editor.selections().ranges().iter().filter_map(|selection| {
            if selection.is_empty() {
                None
            } else {
                let selected = selection.ordered();
                Some((selected.start.0, selected.end.0, ()))
            }
        });
        let _ = assign_first_overlapping(&cells, selections, &mut selected_cells);

        // SourceMark order is observable: the old renderer used `find`, so
        // the first overlapping mark supplied the foreground color.
        let mut active_cells = vec![None; cells.len()];
        let _ = assign_first_overlapping(
            &cells,
            decorations
                .active
                .iter()
                .map(|mark| (mark.from, mark.to, (mark.color, mark.strength))),
            &mut active_cells,
        );

        let mut by_screen_cell = HashMap::with_capacity(cells.len());
        for (index, cell) in cells.into_iter().enumerate() {
            by_screen_cell.insert(
                cell.key,
                CellDecorations {
                    mini: mini_cells[index].is_some(),
                    slider: slider_cells[index],
                    error: error_cells[index].is_some(),
                    selected: selected_cells[index].is_some(),
                    active: active_cells[index],
                    bracket: bracket_cells[index],
                    overridden: overridden_cells[index].is_some(),
                },
            );
        }
        Self {
            by_screen_cell,
            caret_shape: decorations.caret_shape,
        }
    }

    fn get(&self, screen_x: u16, screen_y: u16) -> CellDecorations {
        self.by_screen_cell
            .get(&(screen_x, screen_y))
            .copied()
            .unwrap_or_default()
    }
}

/// Assign the first input range overlapping each visible cell. The
/// disjoint-set skips cells that already have their precedence winner, which
/// bounds the inner loop by the number of visible cells rather than by
/// `cells * ranges`. The returned count makes that invariant testable without
/// timing-sensitive benchmarks.
fn assign_first_overlapping<T: Copy>(
    cells: &[VisibleCell],
    ranges: impl IntoIterator<Item = (usize, usize, T)>,
    assigned: &mut [Option<T>],
) -> usize {
    debug_assert_eq!(cells.len(), assigned.len());
    let mut unassigned = NextUnassigned::new(cells.len());
    let mut assignment_count = 0;
    for (from, to, value) in ranges {
        if from >= to {
            continue;
        }
        let first = cells.partition_point(|cell| cell.to <= from);
        let limit = cells.partition_point(|cell| cell.from < to);
        let mut index = unassigned.find(first);
        while index < limit {
            assigned[index] = Some(value);
            assignment_count += 1;
            index = unassigned.claim(index);
        }
    }
    assignment_count
}

struct NextUnassigned {
    parent: Vec<usize>,
}

impl NextUnassigned {
    fn new(len: usize) -> Self {
        Self {
            parent: (0..=len).collect(),
        }
    }

    fn find(&mut self, index: usize) -> usize {
        let mut root = index;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        let mut cursor = index;
        while self.parent[cursor] != cursor {
            let next = self.parent[cursor];
            self.parent[cursor] = root;
            cursor = next;
        }
        root
    }

    fn claim(&mut self, index: usize) -> usize {
        let next = self.find(index + 1);
        self.parent[index] = next;
        next
    }
}

/// Draw a visualization at its full height and blit the visible slice.
///
/// An inline widget scrolling past the top of the pane must not be redrawn
/// at the clipped height: a piano roll would rescale its lanes and a scope
/// its amplitude, so the whole picture would jump as the caret moved.
#[allow(clippy::too_many_arguments)]
fn render_clipped_visual(
    kind: &str,
    slot: Option<u8>,
    options: &str,
    visual: &VisualState,
    theme: &Theme,
    background: Color,
    visible_area: Rect,
    row_offset: u16,
    full_height: u16,
    destination: &mut Buffer,
) {
    if visible_area.is_empty() || full_height == 0 {
        return;
    }
    let full_area = Rect::new(0, 0, visible_area.width, full_height);
    let mut layer = Buffer::empty(full_area);
    layer.set_style(
        full_area,
        Style::default().bg(background).fg(theme.foreground),
    );
    let parsed = VisualOptions::parse(options);
    if !settings::animation() {
        layer.set_string(
            full_area.x,
            full_area.y,
            format!("_{kind}() - animation is off in settings"),
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::ITALIC),
        );
        blit_rows(&layer, row_offset, visible_area, destination);
        return;
    }
    // On the pixel tier a visualizer paints no cells at all: it hands the
    // frame an image, and that image carries the rectangle it was drawn
    // in - which here is the scratch layer's, at the origin. Blitting the
    // cells home would leave the picture at the top-left corner of the
    // screen, at its unclipped height, over whatever is there. So the
    // images are captured with the layer and moved home with it, the way
    // the visualizer dock already moves its own.
    let ((), images) = crate::graphics::capture_images(|| {
        super::visuals::render(
            VisualRequest {
                kind,
                slot,
                options: &parsed,
                state: visual,
                theme,
                background,
                inline: true,
            },
            full_area,
            &mut layer,
        );
    });
    blit_rows(&layer, row_offset, visible_area, destination);
    let source = Rect::new(0, row_offset, visible_area.width, visible_area.height);
    for image in images {
        if let Some(image) = image.crop_and_move(source, (visible_area.x, visible_area.y)) {
            crate::graphics::push_image(image);
        }
    }
}

fn blit_rows(source: &Buffer, source_y: u16, destination_area: Rect, destination: &mut Buffer) {
    for row in 0..destination_area.height {
        let from_y = source_y.saturating_add(row);
        for column in 0..destination_area.width {
            let Some(cell) = source.cell((source.area.x.saturating_add(column), from_y)) else {
                continue;
            };
            if let Some(target) = destination.cell_mut((
                destination_area.x.saturating_add(column),
                destination_area.y.saturating_add(row),
            )) {
                *target = cell.clone();
            }
        }
    }
}

/// The row's tokens, resolved against the whole line it is part of.
///
/// Two different things need context beyond the row, and they need different
/// amounts of it. A string or comment only needs what came before, which is
/// what [`Lexer::primed`] carries. A word needs both sides: a row can begin or
/// end in the middle of one, and whether `stack` is a call depends on a `(`
/// that may not be on this row at all. Classifying only the row's own cells
/// reads `const` wrapped after `co` as the non-word `nst`, and leaves `sta`
/// plain while `ck` on the next row goes bold.
///
/// So the line is classified whole - the prefix, the row, and the remainder -
/// and the row's own run of tokens is taken back out of the middle.
fn row_tokens(document: &Document, row: &TextRow) -> Vec<Token> {
    let own = |document: &Document| {
        syntax::classify_from(
            lexer_for_row(document, row),
            row.cells.iter().map(|cell| cell.display.as_str()),
        )
    };
    let (Some(first), Some(last)) = (row.cells.first(), row.cells.last()) else {
        return Vec::new();
    };
    let Ok(line) = document.line_of(first.bytes.start) else {
        return own(document);
    };
    // The line's own bytes, terminator excluded - `line_start(line + 1)`
    // would not do, because it clamps to the last line and so returns this
    // line's own start on the final one, leaving no suffix at all and the
    // last call on the last line uncoloured.
    let content = document.line_content_range(line);
    let start = content.start;
    let end = content.end.max(last.bytes.end);
    let (Ok(prefix), Ok(suffix)) = (
        document.slice(start..first.bytes.start),
        document.slice(last.bytes.end..end),
    ) else {
        return own(document);
    };
    // Characters, not graphemes, for the two ends: they are only counted and
    // read for word shape and a following `(`, both of which are ASCII, while
    // the row's own cells stay the clusters the renderer actually drew.
    let before: Vec<String> = prefix.chars().map(|c| c.to_string()).collect();
    let after: Vec<String> = suffix.chars().map(|c| c.to_string()).collect();
    let tokens = syntax::classify(
        before
            .iter()
            .map(String::as_str)
            .chain(row.cells.iter().map(|cell| cell.display.as_str()))
            .chain(after.iter().map(String::as_str)),
    );
    let taken: Vec<Token> = tokens
        .into_iter()
        .skip(before.len())
        .take(row.cells.len())
        .collect();
    // A slice that did not come out whole falls back rather than painting the
    // row from somebody else's offsets.
    if taken.len() == row.cells.len() {
        taken
    } else {
        own(document)
    }
}

/// The lexical state `row` starts in: its line read up to the row's first
/// cell, and not a character further.
///
/// A screen row is not a line. Scrolled sideways, or wrapped, a row begins
/// somewhere in the middle of one, and a lexer started there has no idea
/// whether it stands inside a string or a comment. Read from `bd").seg(4)`,
/// the quote that closes `s("bd")` opens a string instead, and the rest of
/// the row - and every row below it - is painted as one.
///
/// The prefix is bounded by the line, so this is proportional to what is on
/// screen the way the rest of the draw loop is.
fn lexer_for_row(document: &Document, row: &TextRow) -> Lexer {
    // `content` is the whole line; `cells` is the window of it this row
    // draws. The first cell is where the row actually starts, which is what
    // the prefix has to reach.
    let Some(first) = row.cells.first() else {
        return Lexer::default();
    };
    let Ok(line) = document.line_of(first.bytes.start) else {
        return Lexer::default();
    };
    let start = document.line_start(line);
    if start >= first.bytes.start {
        return Lexer::default();
    }
    match document.slice(start..first.bytes.start) {
        Ok(prefix) => Lexer::primed(&prefix),
        Err(_) => Lexer::default(),
    }
}

#[allow(clippy::too_many_arguments)]
fn render_text_row(
    buffer: &mut Buffer,
    row: &TextRow,
    pane_x: u16,
    pane_width: u16,
    gutter: u16,
    current_line: usize,
    decorations: &DecorationIndex,
    chips: &[SliderChip],
    theme: &Theme,
    // The row's finished tokens, resolved against its whole line - see
    // `row_tokens`. A row is not a line, and can work out neither its string
    // state nor its word boundaries for itself.
    lexical: Vec<Token>,
) {
    let is_current = row.line == current_line;
    // A full-width band across the caret's line is a strong decoration for a
    // weak fact - you can see where the caret is - and over Hydra it is a bar
    // laid across the picture. Themes that want one say so; none of the
    // shipped ones do.
    if is_current && let Some(colour) = theme.current_line {
        buffer.set_style(
            Rect::new(pane_x, row.screen_y, pane_width, 1),
            Style::default().bg(colour),
        );
    }
    // A wrapped line is numbered once, on its first row; the rows it
    // continues on keep the gutter blank, the way every editor shows it.
    // A gutter no wider than the margin carries no number.
    if gutter > MARGIN_WIDTH {
        let number = if row.segment == 0 {
            format!("{:>width$} ", row.line + 1, width = usize::from(gutter - 1))
        } else {
            " ".repeat(usize::from(gutter))
        };
        buffer.set_stringn(
            pane_x,
            row.screen_y,
            number,
            usize::from(gutter),
            Style::default().fg(if is_current {
                theme.accent
            } else {
                theme.muted
            }),
        );
    }

    for (index, cell) in row.cells.iter().enumerate() {
        let token = lexical.get(index).copied().unwrap_or_default();
        let mut style = Style::default().fg(token.color(theme));
        let decoration = decorations.get(cell.screen_x.start, row.screen_y);
        if decoration.mini {
            style = style.fg(theme.mini);
            if let Some(fill) = theme.mini_fill {
                style = style.bg(fill);
            }
        }
        if decoration.slider.is_some() {
            // A live control reads as a chip: the accent on a faint tint of
            // itself, so it is obviously something to drag rather than type.
            // Where the pill fits on the row, the control itself is drawn
            // over this after the text pass.
            style = style
                .fg(theme.accent)
                .bg(mix(theme.background, theme.accent, 0.25))
                .add_modifier(Modifier::BOLD);
        }
        if decoration.overridden {
            // `setcpm` while an outside clock owns the tempo: shown, struck
            // through, so nobody wonders why it did nothing.
            style = style.fg(theme.muted).add_modifier(Modifier::CROSSED_OUT);
        }
        if let Some(matched) = decoration.bracket {
            style = theme.bracket_style(style, decorations.caret_shape, matched);
        }
        if let Some((color, strength)) = decoration.active {
            // How a sounding event marks its text is a theme decision. The
            // default tints its background and keeps the text readable.
            // A mark letting go eases back to the plain style.
            let marked = theme.event_mark.apply(style, color, theme.background);
            style = if strength >= 1.0 {
                marked
            } else {
                super::theme::fade_mark(style, marked, strength, theme.foreground, theme.background)
            };
        }
        if decoration.error {
            // The linter's underline, in the error colour, over whatever
            // else the cell carries: a problem is never hidden by a mark.
            //
            // It runs after the sounding mark, so the mark cannot paint
            // over it. A mark says "this is sounding now" and comes back
            // every cycle; an error says "this will not run" and stays
            // until it is fixed, so the error is the one that must stay
            // visible.
            style = style
                .underline_color(theme.error)
                .add_modifier(Modifier::UNDERLINED | Modifier::BOLD);
        }
        if decoration.selected {
            // After the error, and last of all: `selection_text` is picked
            // to be readable on `selection`, and a foreground set by
            // anything else on top of that background is a colour nobody
            // chose the pair for. What is selected is what the hand is
            // holding, and it has to stay legible while it is held.
            style = style.bg(theme.selection).fg(theme.selection_text);
        }
        buffer.set_stringn(
            cell.screen_x.start,
            row.screen_y,
            &cell.display,
            usize::from(cell.screen_x.end.saturating_sub(cell.screen_x.start)),
            style,
        );
    }

    // The live controls. A chip whose whole cover sits on this row is drawn
    // as a control: a track with the knob where the value sits, covering the
    // cover's text completely. A clipped or wrapped cover keeps its text
    // under the chip tint instead.
    let mut runs: Vec<(u8, u16, u16, usize, usize)> = Vec::new();
    for cell in &row.cells {
        let Some(index) = decorations.get(cell.screen_x.start, row.screen_y).slider else {
            continue;
        };
        match runs.last_mut() {
            Some((chip, _, x1, _, byte_to)) if *chip == index && *x1 == cell.screen_x.start => {
                *x1 = cell.screen_x.end;
                *byte_to = cell.bytes.end.0;
            }
            _ => runs.push((
                index,
                cell.screen_x.start,
                cell.screen_x.end,
                cell.bytes.start.0,
                cell.bytes.end.0,
            )),
        }
    }
    for (index, x0, x1, byte_from, byte_to) in runs {
        let Some(chip) = chips.get(usize::from(index)) else {
            continue;
        };
        if byte_from != chip.from
            || byte_to != chip.to
            || slider_cover_bounds(row, chip.from, chip.to) != Some((x0, x1))
        {
            continue;
        }
        let background = if is_current {
            theme.current_line.unwrap_or(theme.background)
        } else {
            theme.background
        };
        // The drawn control is atomic: a source selection touching its
        // cover selects the complete rail, including the expanded cells.
        let selected = row.cells.iter().any(|cell| {
            cell.bytes.start.0 >= chip.from
                && cell.bytes.end.0 <= chip.to
                && decorations.get(cell.screen_x.start, row.screen_y).selected
        });
        draw_slider_control(
            buffer,
            Rect::new(x0, row.screen_y, x1 - x0, 1),
            chip,
            theme,
            background,
            selected,
        );
    }
}

/// The complete visible cover, shared by drawing and pointer input. A
/// clipped or wrapped cover stays text instead of becoming a shorter rail.
pub(super) fn slider_cover_bounds(row: &TextRow, from: usize, to: usize) -> Option<(u16, u16)> {
    if from < row.cells.first()?.bytes.start.0 || to > row.cells.last()?.bytes.end.0 {
        return None;
    }
    let mut cells = row
        .cells
        .iter()
        .filter(|cell| cell.bytes.start.0 >= from && cell.bytes.end.0 <= to);
    let first = cells.next()?;
    let mut last = first;
    for cell in std::iter::once(first).chain(cells) {
        if usize::from(cell.screen_x.end.saturating_sub(cell.screen_x.start)) != cell.columns.len()
        {
            return None;
        }
        last = cell;
    }
    (first.bytes.start.0 == from && last.bytes.end.0 == to)
        .then_some((first.screen_x.start, last.screen_x.end))
}

/// Track cells within the complete cover, shared by drawing and input.
pub fn slider_track_span(width: usize) -> Option<std::ops::Range<usize>> {
    // Every cell of the pill is track: `slider(` is seven cells, and a
    // knob wants all the room it can get.
    if width < 3 {
        return None;
    }
    Some(0..width)
}

pub(super) fn draw_slider_control(
    buffer: &mut Buffer,
    area: Rect,
    chip: &SliderChip,
    theme: &Theme,
    background: Color,
    selected: bool,
) {
    let width = usize::from(area.width);
    if width == 0 {
        return;
    }
    let mut text = String::with_capacity(width * 3);
    match slider_track_span(width) {
        Some(track) => {
            for _ in 0..track.start {
                text.push(' ');
            }
            let cells = track.len();
            let knob = ((chip.notch * (cells - 1) as f64).round() as usize).min(cells - 1);
            for position in 0..cells {
                // A full block for the handle: it fills its cell exactly,
                // where a dot glyph sat off-baseline in many fonts.
                text.push(if position == knob { '█' } else { '─' });
            }
            while text.chars().count() < width {
                text.push(' ');
            }
        }
        None => {
            // Too narrow for any track at all: a plain tinted cover.
            while text.chars().count() < width {
                text.push(' ');
            }
        }
    }
    // Without a fill, later visuals can change the background. The rail
    // keeps its glyphs and accent colour.
    let mut style = Style::default()
        .fg(theme.accent)
        .bg(theme.slider_fill.unwrap_or(background))
        .add_modifier(Modifier::BOLD);
    // Focus changes the ground, leaving the accent handle and continuous
    // thin rail identical to the control the pointer first pressed.
    if chip.armed || selected {
        style = style.bg(theme.selection);
        // Mono deliberately uses its accent as the selection ground.
        // Keep the theme's contrasting pair when those colours coincide.
        if theme.accent == theme.selection {
            style = style.fg(theme.selection_text);
        }
    }
    // set_stringn patches existing cells. Source underlines, reverse,
    // dimming and bracket fills must not survive into parts of the rail.
    clear_surface(buffer, area, style);
    buffer.set_stringn(area.x, area.y, &text, width, style);
}

#[cfg(test)]
mod loading_line_tests {
    //! The loading line preserves text, remains readable in each theme, and clears
    //! obsolete underlines when progress shrinks or the line disappears.

    use super::*;

    fn header_row(width: u16, theme: &Theme) -> Buffer {
        let mut buffer = Buffer::empty(Rect::new(0, 0, width, 1));
        for x in 0..width {
            buffer[(x, 0)].set_symbol("x").set_bg(theme.surface);
        }
        buffer
    }

    fn underlined(buffer: &Buffer, colour: Color) -> u16 {
        (0..buffer.area.width)
            .filter(|x| {
                let cell = &buffer[(*x, 0)];
                cell.modifier.contains(Modifier::UNDERLINED) && cell.underline_color == colour
            })
            .count() as u16
    }

    /// The loading line has at least 3:1 contrast against an RGB header surface.
    #[test]
    fn the_line_reads_against_the_header_in_every_theme() {
        for name in Theme::built_in_names() {
            let theme = Theme::built_in(name).unwrap_or_else(|| panic!("{name} is missing"));
            let colour = loading_line_colour(&theme);
            if let (Color::Rgb(..), Color::Rgb(..)) = (colour, theme.surface) {
                let ratio = super::super::theme::contrast_ratio(colour, theme.surface);
                assert!(ratio >= 3.0, "{name}: {ratio}");
            }
        }
    }

    /// The underline grows with progress, starting at two cells. The fallback
    /// tints the same fraction of the row. Both preserve the header text.
    #[test]
    fn the_line_reaches_as_far_as_the_load_and_falls_back_to_a_tint() {
        let theme = Theme::default();
        let colour = loading_line_colour(&theme);
        let area = Rect::new(0, 0, 40, 1);
        let mut reaches = Vec::new();
        for reach in [0.0, 0.25, 0.75, 1.0] {
            let mut buffer = header_row(40, &theme);
            draw_loading_line(
                &mut buffer,
                area,
                &LoadingLine {
                    reach,
                    label: None,
                    underline: true,
                },
                &theme,
            );
            assert_eq!(buffer[(0, 0)].symbol(), "x", "the text stays");
            reaches.push(underlined(&buffer, colour));
        }
        assert_eq!(reaches, [2, 10, 30, 40]);

        let mut buffer = header_row(40, &theme);
        draw_loading_line(
            &mut buffer,
            area,
            &LoadingLine {
                reach: 0.5,
                label: None,
                underline: false,
            },
            &theme,
        );
        assert_eq!(underlined(&buffer, colour), 0);
        let tint = super::super::theme::mix(theme.surface, theme.accent, 0.2);
        let tinted = (0..40).filter(|x| buffer[(*x, 0)].bg == tint).count();
        assert_eq!(tinted, 20);
    }

    /// What a backend wrote, kept where the test can read it.
    #[derive(Clone, Default)]
    struct Sent(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

    impl std::io::Write for Sent {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Which cells of a one-row screen show an underline after `sent` is
    /// played into it: cursor moves, SGR 4 / 24 / 0, and one-column glyphs.
    fn underlines_on_screen(sent: &str, screen: &mut [bool]) {
        let mut underline = false;
        let mut column = 0usize;
        let mut chars = sent.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch != '\u{1b}' {
                if let Some(cell) = screen.get_mut(column) {
                    *cell = underline;
                }
                column += 1;
                continue;
            }
            if chars.next() != Some('[') {
                continue;
            }
            let mut body = String::new();
            let end = loop {
                match chars.next() {
                    Some(c) if c.is_ascii_digit() || c == ';' || c == ':' || c == '?' => {
                        body.push(c)
                    }
                    other => break other,
                }
            };
            match end {
                Some('H') => {
                    let col = body.split(';').nth(1).and_then(|c| c.parse::<usize>().ok());
                    column = col.unwrap_or(1).saturating_sub(1);
                }
                Some('m') => {
                    let mut params = body.split(';');
                    while let Some(param) = params.next() {
                        match param {
                            "" | "0" | "24" => underline = false,
                            "4" => underline = true,
                            // Colours carry their own arguments.
                            "38" | "48" | "58" => {
                                let skip = if params.next() == Some("5") { 1 } else { 3 };
                                for _ in 0..skip {
                                    params.next();
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Redrawing a shorter or absent line removes the previous underline.
    #[test]
    fn the_terminal_keeps_no_underline_where_the_line_no_longer_reaches() {
        use ratatui::backend::Backend;

        let theme = Theme::default();
        let area = Rect::new(0, 0, 20, 1);
        let drawn = |reach: Option<f64>| {
            let mut buffer = header_row(20, &theme);
            if let Some(reach) = reach {
                draw_loading_line(
                    &mut buffer,
                    area,
                    &LoadingLine {
                        reach,
                        label: None,
                        underline: true,
                    },
                    &theme,
                );
            }
            buffer
        };
        for (from, to) in [(1.0, Some(0.5)), (0.5, None)] {
            let before = drawn(Some(from));
            let after = drawn(to);
            let sent = Sent::default();
            let mut backend = super::super::terminal::StudioBackend::new(sent.clone());
            backend
                .draw(Buffer::empty(area).diff(&before).into_iter())
                .expect("the first frame");
            backend
                .draw(before.diff(&after).into_iter())
                .expect("the diff is drawn");
            let mut screen = [false; 20];
            underlines_on_screen(&String::from_utf8_lossy(&sent.0.borrow()), &mut screen);
            let expected: Vec<bool> = (0..20)
                .map(|x| after[(x, 0)].modifier.contains(Modifier::UNDERLINED))
                .collect();
            assert_eq!(screen.to_vec(), expected, "{from} to {to:?}");
        }
    }
}
#[cfg(test)]
mod tests {
    use super::layout::PANE_MIN_WIDTH_BESIDE_REFERENCE;
    use super::*;
    use crate::editor::ByteOffset;

    /// An inline visualizer is drawn at its full height on a scratch layer
    /// and the visible slice copied home. On the pixel tier it paints no
    /// cells at all - it hands the frame a picture - so the picture has to
    /// be carried home with the cells. Left behind, it was placed at the
    /// scratch layer's own origin: the top-left corner of the screen, at
    /// full height, over the menu bar, and the widget's rows showed nothing.
    /// An inline painter with nothing to draw says so in its own band: the view
    /// renders it as inline, unlike a stage painter, which stays blank.
    #[test]
    fn an_inline_visualizer_with_nothing_to_draw_says_so_in_its_band() {
        let theme = Theme::resolve(None).expect("theme");
        let visual = VisualState::default();
        for (kind, note) in [
            ("scope", "waiting for audio"),
            ("pianoroll", "waiting for the first beat"),
        ] {
            let mut buffer = Buffer::empty(Rect::new(0, 0, 40, 3));
            render_clipped_visual(
                kind,
                None,
                "",
                &visual,
                &theme,
                theme.background,
                Rect::new(0, 0, 40, 3),
                0,
                3,
                &mut buffer,
            );
            let text: String = (0..3)
                .flat_map(|y| (0..40).map(move |x| (x, y)))
                .map(|at| buffer.cell(at).expect("cell").symbol().to_string())
                .collect();
            assert!(text.contains(note), "_{kind}() shows {note:?}: {text:?}");
        }
    }

    #[test]
    fn an_inline_visualizer_puts_its_picture_where_the_widget_is() {
        let _renderer = crate::graphics::HeldRenderer::pixels((2, 3));
        let theme = Theme::resolve(None).expect("theme");
        // A spiral draws once there is a clock to draw it against.
        let source = "$: s(\"bd\")._spiral()";
        let envelope = rustel_runtime::ui_events::visual_layout(source, 1).expect("layout");
        let revision = envelope.ui_layout.source_revision.clone();
        let mut visual = VisualState::default();
        visual.install_layout(envelope);
        visual.start();
        let batch = rustel_runtime::ui_events::UiEventBatch::new(
            10.0,
            0.0,
            1.0,
            1,
            revision,
            Vec::new(),
            0,
        )
        .expect("batch");
        assert!(visual.install_batch(batch));
        let mut buffer = Buffer::empty(Rect::new(0, 0, 80, 24));
        // The widget is twelve rows tall and scrolled four rows past the
        // top of the pane, so eight of them show, well down the screen.
        let visible = Rect::new(9, 7, 40, 8);
        render_clipped_visual(
            "spiral",
            None,
            "",
            &visual,
            &theme,
            theme.background,
            visible,
            4,
            12,
            &mut buffer,
        );
        let images = crate::graphics::take_images();
        assert_eq!(images.len(), 1, "the visualizer painted one picture");
        let image = &images[0];
        assert_eq!(image.area, visible, "placed on the widget's own rows");
        assert_eq!(
            image.cells,
            Some((visible.width, visible.height)),
            "and scaled into exactly those cells"
        );
        assert_eq!(
            image.height,
            u32::from(visible.height) * 3,
            "cropped to the rows in view, not the widget's full height"
        );
    }

    fn midi_ports(outputs: usize, inputs: usize) -> MidiPortCounts {
        MidiPortCounts { outputs, inputs }
    }

    /// The pads ride the MIDI chip, so the footer says what is plugged in
    /// before a score asks for any of it.
    #[test]
    fn the_devices_chip_counts_the_pads_beside_the_midi_ports() {
        assert_eq!(devices_chip_text(midi_ports(0, 0), 0), "⌁ no midi");
        assert_eq!(
            devices_chip_text(midi_ports(1, 0), 1),
            "⌁ 1 out · 0 in · ▣ 1 pad"
        );
        assert_eq!(devices_chip_text(midi_ports(0, 1), 0), "⌁ 0 out · 1 in");
        assert_eq!(
            devices_chip_text(midi_ports(2, 1), 3),
            "⌁ 2 out · 1 in · ▣ 3 pads"
        );
        let with = footer_hits(
            Rect::new(0, 0, 160, 2),
            Some("out"),
            None,
            midi_ports(1, 0),
            2,
            &[],
        );
        let without = footer_hits(
            Rect::new(0, 0, 160, 2),
            Some("out"),
            None,
            midi_ports(1, 0),
            0,
            &[],
        );
        assert!(
            with.midi_chip.width > without.midi_chip.width,
            "the chip grows to fit the pads"
        );
    }
    use crate::devices::DeviceEntry;
    use crate::editor::VirtualRowSpec;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// The stage tints the score's ground with what a painter drew - a
    /// raster glyph's colour or a background of its own - and leaves chrome,
    /// highlights and cells with their own colour alone.
    #[test]
    fn the_stage_tints_only_the_ground_under_what_the_painter_drew() {
        let area = Rect::new(0, 0, 5, 1);
        let ground = Color::Rgb(10, 10, 20);
        let mut frame = Buffer::empty(area);
        frame.set_style(area, Style::default().bg(ground));
        frame.set_string(1, 0, "x", Style::default().fg(Color::White));
        // A chip with its own background at x = 4.
        frame.set_string(4, 0, " ", Style::default().bg(Color::Rgb(90, 90, 90)));

        let mut stage = Buffer::empty(area);
        stage.set_string(0, 0, "▀", Style::default().fg(Color::Rgb(200, 0, 0)));
        stage.set_string(1, 0, "▄", Style::default().fg(Color::Rgb(0, 200, 0)));
        stage.set_string(3, 0, "█", Style::default().fg(Color::Rgb(0, 0, 200)));
        stage.set_string(4, 0, "█", Style::default().fg(Color::Rgb(0, 0, 200)));

        let untouched = std::collections::HashSet::from([(3_u16, 0_u16)]);
        paint_stage(&mut frame, &stage, area, ground, &[], &untouched, 0.5);

        let bg = |x: u16| frame.cell((x, 0)).expect("cell").bg;
        assert_eq!(
            bg(0),
            super::super::theme::mix(ground, Color::Rgb(200, 0, 0), 0.5)
        );
        // Text keeps its glyph and takes the tint under it.
        assert_eq!(frame.cell((1, 0)).expect("cell").symbol(), "x");
        assert_eq!(
            bg(1),
            super::super::theme::mix(ground, Color::Rgb(0, 200, 0), 0.5)
        );
        assert_eq!(bg(2), ground, "ground the painter left bare stays bare");
        assert_eq!(bg(3), ground, "a highlight is the top layer");
        assert_eq!(bg(4), Color::Rgb(90, 90, 90), "a chip keeps its own colour");

        // Chrome is spared by rect, whatever its colour.
        let mut frame = Buffer::empty(area);
        frame.set_style(area, Style::default().bg(ground));
        paint_stage(
            &mut frame,
            &stage,
            area,
            ground,
            &[Rect::new(0, 0, 1, 1)],
            &untouched,
            0.5,
        );
        assert_eq!(frame.cell((0, 0)).expect("cell").bg, ground);
    }

    /// A painter's word on the stage is written whole where the score is blank
    /// under it and on either side of it, and not at all elsewhere: none of its
    /// letters or its wash lands among the score's text.
    #[test]
    fn a_stage_word_is_written_whole_in_blank_ground_or_not_at_all() {
        let area = Rect::new(0, 0, 32, 4);
        let ground = Color::Rgb(10, 10, 20);
        let mut frame = Buffer::empty(area);
        frame.set_style(area, Style::default().bg(ground));
        let code = Style::default().fg(Color::White);
        frame.set_string(0, 0, "// await samples('github:x')", code);
        frame.set_string(0, 2, "ab   cd", code);
        // The drum covers cells 3 and 4; cell 4 holds a blank symbol.
        frame.set_string(0, 3, "// 🥁", code);
        let score = frame.clone();

        let blue = Color::Rgb(0, 0, 200);
        let word = Style::default().fg(blue);
        let mut stage = Buffer::empty(area);
        stage.set_string(0, 0, "waiting for audio", word);
        stage.set_string(2, 1, "▸ bd", word);
        // Exactly the width of the gap, so it would touch the code on both sides.
        stage.set_string(2, 2, "xyz", word);
        // One cell, with a blank on either side.
        stage.set_string(20, 2, "q", word);
        // Against the drum's right half, then clear of it.
        stage.set_string(5, 3, "q", word);
        stage.set_string(8, 3, "r", word);

        paint_stage(
            &mut frame,
            &stage,
            area,
            ground,
            &[],
            &std::collections::HashSet::new(),
            0.5,
        );
        let row = |buffer: &Buffer, y: u16| -> String {
            (area.x..area.right())
                .map(|x| buffer.cell((x, y)).expect("cell").symbol())
                .collect()
        };
        let bg = |x: u16, y: u16| frame.cell((x, y)).expect("cell").bg;
        assert_eq!(
            row(&frame, 0),
            row(&score, 0),
            "no letter lands between the code's words"
        );
        assert!(
            (area.x..area.right()).all(|x| bg(x, 0) == ground),
            "a word left out leaves no wash"
        );
        assert_eq!(
            row(&frame, 1).trim_end(),
            "  ▸ bd",
            "a blank row takes it whole"
        );
        assert_eq!(bg(4, 1), super::super::theme::mix(ground, blue, 0.5));
        assert_eq!(
            row(&frame, 2).trim_end(),
            "ab   cd             q",
            "a word flush against the code is left out; a spaced one stays"
        );
        assert_eq!(bg(3, 2), ground);
        assert_eq!(
            row(&frame, 3).trim_end(),
            "// 🥁    r",
            "a wide character's second cell is the score's, not a margin"
        );
    }

    fn theme() -> Theme {
        Theme::built_in_default()
    }

    static DEFAULT_KEYBINDS: std::sync::OnceLock<super::super::keybinds::Keybinds> =
        std::sync::OnceLock::new();

    fn chrome<'a>(theme: &'a Theme, master: &'a MasterState, path: &'a Path) -> StudioChrome<'a> {
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
            evaluating: false,
            dirty: false,
            status: "ready",
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

    #[test]
    fn a_rewind_scene_chip_uses_a_conhost_safe_marker() {
        let mut chip = strip_chip("score", true);
        chip.rewind = true;
        chip.dirty = true;
        let fancy = {
            let _symbols = super::super::terminal::ForceSymbolsForTest::set(true);
            chip_text(0, &chip, None)
        };
        let plain = {
            let _symbols = super::super::terminal::ForceSymbolsForTest::set(false);
            chip_text(0, &chip, None)
        };
        assert!(fancy.contains(" ⟲"));
        assert!(fancy.contains(" ●"));
        assert!(plain.contains(" <"));
        assert!(plain.contains(" ·"));
        assert!(!plain.contains('⟲'));
        assert!(!plain.contains('●'));
        assert_eq!(
            unicode_width::UnicodeWidthStr::width(fancy.as_str()),
            unicode_width::UnicodeWidthStr::width(plain.as_str())
        );
    }

    fn strip_chip(name: &str, current: bool) -> SceneChip {
        SceneChip {
            name: name.to_owned(),
            current,
            playing: false,
            dirty: false,
            rewind: false,
            pad: None,
            errors: false,
            armed: None,
            prebake: None,
            replay: false,
        }
    }

    /// Sixteen scenes need over two hundred columns, so the strip scrolls,
    /// and it always keeps the current chip on screen.
    #[test]
    fn the_scene_strip_keeps_the_current_chip_on_screen() {
        let area = Rect::new(0, 0, 80, 1);
        for current in 0..16usize {
            let chips: Vec<SceneChip> = (0..16)
                .map(|index| strip_chip(&format!("scene {index}"), index == current))
                .collect();
            let layout = scene_strip_layout(area, &chips);
            assert!(
                !layout.hits[current].is_empty(),
                "chip {current} of 16 fell off the strip"
            );
            assert!(
                layout.hits[current].right() <= area.right(),
                "chip {current} runs past the edge"
            );
            // Everything drawn is inside the area and in order.
            let drawn: Vec<usize> = (0..chips.len())
                .filter(|index| !layout.hits[*index].is_empty())
                .collect();
            assert!(!drawn.is_empty());
            assert!(
                drawn.windows(2).all(|pair| pair[1] == pair[0] + 1),
                "the drawn chips are a run, not a scatter: {drawn:?}"
            );
            assert_eq!(layout.before, drawn[0], "the left counter counts the rest");
            assert_eq!(
                layout.after,
                chips.len() - 1 - drawn[drawn.len() - 1],
                "the right counter counts the rest"
            );
        }
    }

    /// A strip that fits says nothing at its edges and starts at the first
    /// chip, so the counters never appear when there is nothing behind
    /// them.
    #[test]
    fn a_strip_that_fits_scrolls_nowhere() {
        let area = Rect::new(0, 0, 80, 1);
        let chips = vec![strip_chip("a", true), strip_chip("b", false)];
        let layout = scene_strip_layout(area, &chips);
        assert_eq!((layout.before, layout.after), (0, 0));
        assert!(layout.hits.iter().all(|hit| !hit.is_empty()));
        // And hit-testing lands on the chip that was drawn.
        for (index, hit) in layout.hits.iter().enumerate() {
            let found = scene_strip_hits(area, &chips);
            assert_eq!(found[index], *hit, "hit-testing reads the same layout");
        }
    }

    fn text_of(buffer: &Buffer) -> String {
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

    #[test]
    fn a_pane_without_a_minimap_has_a_scrollbar_whose_handle_tracks_the_page() {
        let area = Rect::new(0, 0, 100, 30);
        let layout = regions(area, false, 1, false, false, None, None);
        let pane = layout.panes[0];
        assert_eq!(pane.minimap.width, 0, "no minimap");
        assert_eq!(
            pane.scrollbar,
            Rect::new(99, pane.editor.y, 1, pane.editor.height)
        );
        assert_eq!(pane.editor.right(), 99);
        assert!(
            layout.pane_at(99, pane.editor.y + 1).is_some(),
            "the bar is the pane's"
        );

        let text = (0..100).map(|n| format!("{n}\n")).collect::<String>();
        let mut editor = Editor::new(&text).expect("editor");
        editor.set_view_size(80, 20);
        assert!(Scrollbar::needed(&editor), "overflow needs the bar");
        assert_eq!(
            Scrollbar::handle(&editor, 20),
            (0, 20 * 20 / 101),
            "a fifth of the text, at the top"
        );
        let mut viewport = editor.viewport();
        viewport.top_row = 100;
        editor.set_viewport(viewport);
        let (start, length) = Scrollbar::handle(&editor, 20);
        assert_eq!(start + length, 20, "the last row, at the bottom");
        let short = Editor::new("one\ntwo\n").expect("editor");
        assert!(
            !Scrollbar::needed(&short),
            "content that fits does not show a bar"
        );
        assert_eq!(
            Scrollbar::handle(&short, 20),
            (0, 20),
            "all of a short text: a full bar"
        );
    }

    #[test]
    fn horizontal_scrollbar_stays_grabbable_and_reaches_both_ends() {
        for width in [2, 3, 4, 23, 40, 80] {
            for (page, total) in [(79, 80), (20, 200), (80, 1_000_000)] {
                let maximum = total - page;
                let (start, length) = horizontal_scrollbar_thumb((0, page, total), width);
                assert_eq!(start, 0);
                assert!(length >= usize::from(width.saturating_sub(1).min(3)));
                assert!(
                    length < usize::from(width),
                    "even tiny overflow has thumb travel"
                );
                let travel = usize::from(width) - length;
                let mut previous = 0;
                for cell in 0..=travel {
                    let left = horizontal_scrollbar_left((0, page, total), width, cell);
                    assert!(left >= previous && left <= maximum);
                    previous = left;
                }
                assert_eq!(previous, maximum);
                let (end, size) = horizontal_scrollbar_thumb((maximum, page, total), width);
                assert_eq!(end + size, usize::from(width));
            }
        }
    }

    #[test]
    fn horizontal_scrollbar_thumb_uses_the_full_track() {
        assert_eq!(horizontal_scrollbar_thumb((0, 20, 200), 40), (0, 4));

        let (start, length) = horizontal_scrollbar_thumb((180, 20, 200), 40);
        assert_eq!(start + length, 40, "the final thumb touches the right edge");

        let (start, length) = horizontal_scrollbar_thumb((65, 17, 82), 23);
        assert_eq!(
            start + length,
            23,
            "non-divisible sizes also reach the edge"
        );
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
        };
        let shown = ChromeLayout::shown(false);
        for (notices, piano) in [(0, false), (1, false), (2, false), (0, true), (2, true)] {
            let rows = notices + u16::from(piano);
            assert_eq!(footer(hidden, notices, piano).height, 1 + rows);
            assert_eq!(footer(shown, notices, piano).height, 2 + rows);
            assert_eq!(footer(hidden, notices, piano).bottom(), area.bottom());
        }
    }

    #[test]
    fn a_status_only_footer_has_no_meter_orbits_or_device_chips() {
        let area = Rect::new(0, 20, 150, 1);
        let hits = status_footer_hits(area);
        assert!(hits.dock.is_empty());
        assert!(hits.scope.is_empty());
        assert!(hits.device_chip.is_empty());
        assert!(hits.midi_chip.is_empty());
        assert_eq!(hits.orbit_count, 0);
        assert_eq!(hits.status.y, area.y);
        assert_eq!(
            hits.status.right(),
            area.right() - 1,
            "the status keeps the footer's one-cell right margin"
        );
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

    /// A prebake chip says what it is, wears no number, and takes its own
    /// colour - and the strip still draws chips where it hit-tests them.
    #[test]
    fn a_prebake_chip_is_marked_numberless_and_in_its_own_colour() {
        let theme = theme();
        let chips = vec![
            SceneChip {
                replay: false,
                prebake: None,
                name: "intro".into(),
                current: false,
                playing: true,
                dirty: false,
                rewind: false,
                pad: None,
                errors: false,
                armed: None,
            },
            SceneChip {
                replay: false,
                prebake: Some(PrebakeScope::Global),
                name: PrebakeScope::Global.tab_name(),
                current: false,
                playing: false,
                dirty: true,
                rewind: false,
                pad: None,
                errors: false,
                armed: None,
            },
            SceneChip {
                replay: false,
                prebake: Some(PrebakeScope::Local),
                name: PrebakeScope::Local.tab_name(),
                current: true,
                playing: false,
                dirty: false,
                rewind: false,
                pad: None,
                errors: false,
                armed: None,
            },
        ];
        let area = Rect::new(0, 1, 120, 1);
        let mut buffer = Buffer::empty(area);
        SceneStrip {
            keybinds: &crate::keybinds::Keybinds::default(),
            chips: &chips,
            mode: &SceneStripMode::Idle,
            split: false,
            capabilities: KeyboardCapabilities::enhanced(),
            theme: &theme,
        }
        .render(area, &mut buffer);

        let hits = scene_strip_hits(area, &chips);
        let read = |rect: Rect| {
            (rect.x..rect.right())
                .filter_map(|x| buffer.cell((x, rect.y)))
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        assert_eq!(read(hits[0]).trim(), "▶1 intro");
        assert_eq!(read(hits[1]).trim(), "⚙ prebake (global) ●");
        assert_eq!(read(hits[2]).trim(), "⚙ prebake (local)");
        assert!(!hits[1].intersects(hits[2]), "the chips overlap");
        assert_eq!(
            buffer.cell((hits[1].x, 1)).unwrap().fg,
            theme.mini,
            "a prebake chip is not in its own colour"
        );
        assert_eq!(buffer.cell((hits[2].x, 1)).unwrap().bg, theme.mini);

        let text = text_of(&buffer);
        assert!(text.contains("^Enter applies"), "{text}");
    }

    /// The header names a prebake tab rather than passing off the file it
    /// is kept in as the music.
    #[test]
    fn the_header_names_a_prebake_tab() {
        let theme = theme();
        let path = std::path::Path::new("/x/config/prebake.strudel");
        let area = Rect::new(0, 0, 120, 1);

        let master = MasterState::new(Instant::now());
        let mut buffer = Buffer::empty(area);
        let mut tab = chrome(&theme, &master, path);
        tab.prebake = Some(PrebakeScope::Global);
        Header { chrome: &tab }.render(area, &mut buffer);
        let text = text_of(&buffer);
        assert!(text.contains("⚙ prebake (global)"), "{text}");
        assert!(
            !text.contains("/x/config"),
            "not the file it is kept in: {text}"
        );

        // A score reads as its own name.
        let mut buffer = Buffer::empty(area);
        let score = chrome(&theme, &master, std::path::Path::new("/x/song.strudel"));
        Header { chrome: &score }.render(area, &mut buffer);
        let text = text_of(&buffer);
        assert!(text.contains("song"), "{text}");
        assert!(!text.contains("prebake") && !text.contains("/x/"), "{text}");
    }

    #[test]
    fn the_scene_strip_draws_chips_where_it_hit_tests_them() {
        let theme = theme();
        let chips = vec![
            SceneChip {
                replay: false,
                prebake: None,
                name: "intro".into(),
                current: false,
                playing: true,
                dirty: false,
                rewind: false,
                pad: Some("c1/10".into()),
                errors: false,
                armed: None,
            },
            SceneChip {
                replay: false,
                prebake: None,
                name: "drop".into(),
                current: true,
                playing: false,
                dirty: true,
                rewind: false,
                pad: None,
                errors: true,
                armed: None,
            },
        ];
        let area = Rect::new(0, 1, 120, 1);
        let mut buffer = Buffer::empty(area);
        SceneStrip {
            keybinds: &crate::keybinds::Keybinds::default(),
            chips: &chips,
            mode: &SceneStripMode::Idle,
            split: false,
            capabilities: KeyboardCapabilities::enhanced(),
            theme: &theme,
        }
        .render(area, &mut buffer);
        let hits = scene_strip_hits(area, &chips);
        let read = |rect: Rect| {
            (rect.x..rect.right())
                .filter_map(|x| buffer.cell((x, rect.y)))
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        assert_eq!(read(hits[0]).trim(), "▶1 intro ♪c1/10");
        assert_eq!(read(hits[1]).trim(), "2 drop ● ✗");
        assert!(!hits[0].intersects(hits[1]));
        assert_eq!(buffer.cell((hits[1].x, 1)).unwrap().bg, theme.accent);
        // At rest the strip advertises nothing: those chords live in the
        // Scene menu, each with its accelerator beside it.
        let text = text_of(&buffer);
        assert!(!text.contains("^N new"), "{text}");
        assert!(!text.contains("^[/^]"), "{text}");

        let mut renaming = Buffer::empty(area);
        SceneStrip {
            keybinds: &crate::keybinds::Keybinds::default(),
            chips: &chips,
            mode: &SceneStripMode::Renaming("dr".into()),
            split: false,
            capabilities: KeyboardCapabilities::enhanced(),
            theme: &theme,
        }
        .render(area, &mut renaming);
        let text = text_of(&renaming);
        assert!(text.contains("2 dr▏"), "{text}");
        assert!(text.contains("Enter renames"), "{text}");
    }
    /// The scope keeps its place on a narrow row: it takes what is free
    /// between the status's share and the dock, shrinking rather than
    /// vanishing, and the orbit chips take what it leaves. A hundred and
    /// ten columns used to be the price of any scope at all.
    #[test]
    fn the_footer_keeps_a_scope_on_a_narrow_row() {
        let orbits: Vec<crate::engine::OrbitLevel> = (0..3)
            .map(|orbit| crate::engine::OrbitLevel {
                orbit,
                peak: 0.5,
                pair: 0,
            })
            .collect();
        let hits = |width: u16| {
            footer_hits(
                Rect::new(0, 0, width, 2),
                Some("out"),
                None,
                midi_ports(1, 0),
                0,
                &orbits,
            )
        };
        let narrow = hits(100);
        assert_eq!(
            narrow.dock.width, DOCK_WIDTH_NARROW,
            "a narrow row still narrows the dock"
        );
        assert_eq!(narrow.scope.width, SCOPE_WIDTH, "and keeps its scope");
        assert_eq!(narrow.scope.right(), narrow.dock.x.saturating_sub(1));
        assert!(narrow.orbit_count > 0, "with room left for a chip");
        // Wider, the dock grows and the chips multiply; the scope is the
        // same width throughout.
        let wide = hits(150);
        assert_eq!(wide.dock.width, DOCK_WIDTH);
        assert_eq!(wide.scope.width, SCOPE_WIDTH);
        assert!(wide.orbit_count > narrow.orbit_count);
        // Narrower still, the scope shrinks before it goes.
        let small = hits(64);
        assert!(
            (SCOPE_MIN_WIDTH..SCOPE_WIDTH).contains(&small.scope.width),
            "shrunk, not gone: {}",
            small.scope.width
        );
        assert_eq!(small.orbit_count, 0, "the chips give way first");
        // And a row with nothing to spare has none.
        assert!(hits(48).scope.is_empty());
        // The status keeps its share whatever else is drawn.
        for width in [48u16, 64, 100, 150] {
            let hits = hits(width);
            assert!(
                hits.status.width >= STATUS_LEAST_WIDTH.min(width),
                "the status kept {} of {width}",
                hits.status.width
            );
        }
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
    fn first_overlapping_range_preserves_style_precedence() {
        let cells = vec![
            VisibleCell {
                key: (0, 0),
                from: 0,
                to: 1,
            },
            VisibleCell {
                key: (1, 0),
                from: 1,
                to: 2,
            },
            // One displayed grapheme can cover several source bytes and
            // overlap ranges that begin inside it.
            VisibleCell {
                key: (2, 0),
                from: 2,
                to: 6,
            },
            VisibleCell {
                key: (3, 0),
                from: 10,
                to: 11,
            },
        ];
        let mut assigned = vec![None; cells.len()];

        let count =
            assign_first_overlapping(&cells, [(3, 4, 0_u8), (0, 6, 1), (1, 11, 2)], &mut assigned);

        assert_eq!(count, cells.len());
        assert_eq!(assigned, [Some(1), Some(1), Some(0), Some(2)]);
    }

    #[test]
    fn dense_ranges_assign_each_cell_only_once() {
        let cells = (0..512)
            .map(|index| VisibleCell {
                key: (index as u16, 0),
                from: index * 2,
                to: index * 2 + 1,
            })
            .collect::<Vec<_>>();
        let ranges = (0..rustel_runtime::ui_events::MAX_UI_LAYOUT_MINI_LOCATIONS)
            .map(|index| (0, cells.len() * 2, index));
        let mut assigned = vec![None; cells.len()];

        let count = assign_first_overlapping(&cells, ranges, &mut assigned);

        assert_eq!(count, cells.len());
        assert!(assigned.iter().all(|winner| *winner == Some(0)));
    }

    #[test]
    fn clipped_inline_layers_keep_their_full_height_geometry() {
        let mut source = Buffer::empty(Rect::new(0, 0, 3, 4));
        for (row, label) in ["AAA", "BBB", "CCC", "DDD"].into_iter().enumerate() {
            source.set_string(0, row as u16, label, Style::default());
        }
        let mut destination = Buffer::empty(Rect::new(0, 0, 8, 4));

        blit_rows(&source, 1, Rect::new(2, 1, 3, 2), &mut destination);

        let row = |y| {
            (2..5)
                .filter_map(|x| destination.cell((x, y)))
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        assert_eq!(row(1), "BBB");
        assert_eq!(row(2), "CCC");
    }

    #[test]
    fn evaluation_flash_changes_style_without_changing_any_cell() {
        let mut editor = Editor::new("$: note(60)\n\n.scope()\n").unwrap();
        let area = Rect::new(0, 0, 24, 4);
        let grid = source_grid(&editor, area, true);
        editor.set_view_size(usize::from(grid.width), usize::from(grid.height));
        let map = editor.screen_map(grid).unwrap();
        let mut buffer = Buffer::empty(area);
        buffer.set_string(4, 0, "$: note(60)", Style::default());
        buffer.set_string(4, 2, ".scope()", Style::default());
        let symbols = buffer
            .content
            .iter()
            .map(|cell| cell.symbol().to_owned())
            .collect::<Vec<_>>();

        EvaluationFlash { map: &map }.render(area, &mut buffer);

        assert_eq!(
            buffer
                .content
                .iter()
                .map(|cell| cell.symbol().to_owned())
                .collect::<Vec<_>>(),
            symbols
        );
        assert!(
            buffer
                .cell((0, 0))
                .is_some_and(|cell| cell.modifier.contains(Modifier::REVERSED))
        );
        assert!(
            buffer
                .cell((15, 0))
                .is_some_and(|cell| !cell.modifier.contains(Modifier::REVERSED)),
            "cells after the visible source stay untouched"
        );
        assert!(
            buffer
                .cell((14, 0))
                .is_some_and(|cell| cell.modifier.contains(Modifier::REVERSED)),
            "the final visible source cell flashes"
        );
        assert!(
            buffer
                .cell((0, 1))
                .is_some_and(|cell| !cell.modifier.contains(Modifier::REVERSED)),
            "empty source rows stay untouched"
        );
        assert!(
            buffer
                .cell((0, 2))
                .is_some_and(|cell| cell.modifier.contains(Modifier::REVERSED)),
            "every visible non-empty source row flashes"
        );
    }

    #[test]
    fn caret_locator_flashes_the_full_logical_line_including_wrapped_rows() {
        use super::super::editor::{ByteOffset, Selection};

        let source = "first\nabcdefghijklmnopqrstuvwxyz\nlast";
        let mut editor = Editor::new(source).unwrap();
        editor.set_wrap(true);
        editor
            .set_selection(Selection::caret(ByteOffset(source.find('a').unwrap())))
            .unwrap();
        let area = Rect::new(0, 0, 12, 6);
        let grid = source_grid(&editor, area, false);
        editor.set_view_size(usize::from(grid.width), usize::from(grid.height));
        let map = editor.screen_map(grid).unwrap();
        let mut buffer = Buffer::empty(area);

        let theme = Theme::built_in_default();
        let fill = theme.caret_line_fill().unwrap();
        buffer.set_style(area, Style::default().fg(theme.syntax.punctuation));
        let caret_row = map
            .rows()
            .iter()
            .find_map(|row| match row {
                ScreenRow::Text(row) if row.line == 1 => Some(row.screen_y),
                _ => None,
            })
            .unwrap();
        let marked = (area.x + 1, caret_row);
        buffer[marked]
            .set_bg(Color::Blue)
            .set_style(Style::default().add_modifier(Modifier::UNDERLINED));

        CaretLineFlash {
            map: &map,
            line: 1,
            theme: &theme,
        }
        .render(area, &mut buffer);

        let mut wrapped_rows = 0;
        for row in map.rows() {
            let ScreenRow::Text(row) = row else {
                continue;
            };
            if row.line == 1 {
                wrapped_rows += 1;
            }
            for x in area.x..area.right() {
                let cell = &buffer[(x, row.screen_y)];
                assert_eq!(cell.fg, theme.syntax.punctuation);
                assert!(!cell.modifier.contains(Modifier::REVERSED));
                if (x, row.screen_y) == marked {
                    assert_eq!(cell.bg, Color::Blue, "preserve bracket/selection fill");
                    assert!(cell.modifier.contains(Modifier::UNDERLINED));
                } else {
                    assert_eq!(cell.bg, if row.line == 1 { fill } else { Color::Reset });
                }
            }
        }
        assert!(wrapped_rows > 1, "the fixture must exercise wrapping");
    }

    #[test]
    fn caret_locator_ring_contracts_and_stays_visible_on_light_and_dark_cells() {
        let area = Rect::new(0, 0, 25, 13);
        let center = CellPoint::new(12, 6);
        for ground in [Color::Rgb(12, 16, 25), Color::Rgb(245, 244, 239)] {
            let mut theme = Theme::built_in_default();
            theme.background = ground;
            theme.accent = Color::Rgb(120, 120, 120);
            let mut buffer = Buffer::empty(area);
            buffer.set_style(area, Style::default().bg(ground));
            buffer[(18, 6)]
                .set_symbol("X")
                .set_bg(Color::Rgb(100, 60, 90));

            CaretLocatorRing {
                center,
                progress: 1.0,
                theme: &theme,
            }
            .render(area, &mut buffer);
            assert_eq!(buffer[(12, 6)].symbol(), " ", "ring starts clear of caret");
            assert_eq!(buffer[(18, 6)].bg, Color::Rgb(100, 60, 90));
            assert!(
                super::super::theme::contrast_ratio(buffer[(18, 6)].fg, Color::Rgb(100, 60, 90))
                    >= 5.5,
                "ring clears a differently colored selection cell without filling it"
            );
            assert!(
                ('\u{2801}'..='\u{28ff}')
                    .contains(&buffer[(18, 6)].symbol().chars().next().unwrap_or(' ')),
                "Braille ring covers a later layer"
            );

            CaretLocatorRing {
                center,
                progress: 0.0,
                theme: &theme,
            }
            .render(area, &mut buffer);
            assert!(
                super::super::theme::contrast_ratio(buffer[(12, 6)].fg, ground) >= 5.5,
                "ring reaches the caret"
            );
            assert_eq!(buffer[(12, 6)].bg, ground, "ring leaves the fill intact");
            assert!(
                ('\u{2801}'..='\u{28ff}')
                    .contains(&buffer[(12, 6)].symbol().chars().next().unwrap_or(' ')),
                "Braille ring reaches the caret"
            );
        }
    }

    /// However a row is clipped out of its line, it is coloured as that line:
    /// every visible cell has the colour it has when the whole line is on
    /// screen. See `row_tokens`.
    #[test]
    fn a_row_is_coloured_as_its_line_however_it_is_clipped() {
        let source = "const x = stack(s(\"bd\").fast(2))";
        // The line classified whole, which is the answer every clipping of it
        // has to agree with. ASCII, so a byte offset is a cluster index.
        let clusters: Vec<String> = source.chars().map(|c| c.to_string()).collect();
        let whole = syntax::classify(clusters.iter().map(String::as_str));

        let mut checked = 0usize;
        let mut saw_split_word = false;
        for width in [12u16, 18, 26, 60] {
            let mut editor = Editor::new(source).unwrap();
            // Scroll the line sideways a column at a time. That is what puts
            // a row's first cell in the middle of a word.
            for caret in 0..source.chars().count() {
                let area = Rect::new(0, 0, width, 4);
                let grid = source_grid(&editor, area, true);
                editor.set_view_size(usize::from(grid.width), usize::from(grid.height));
                let mut viewport = editor.viewport();
                viewport.left_column = caret;
                editor.set_viewport(viewport);
                let Ok(map) = editor.screen_map(grid) else {
                    continue;
                };
                for screen_row in map.rows() {
                    let ScreenRow::Text(row) = screen_row else {
                        continue;
                    };
                    let Some(first) = row.cells.first() else {
                        continue;
                    };
                    // A row starting inside a word is the case that used to
                    // lose its colour; note when the sweep reaches one.
                    if first.bytes.start.0 > 0
                        && source.as_bytes()[first.bytes.start.0 - 1].is_ascii_alphanumeric()
                        && source.as_bytes()[first.bytes.start.0].is_ascii_alphanumeric()
                    {
                        saw_split_word = true;
                    }
                    for (cell, token) in row.cells.iter().zip(row_tokens(editor.document(), row)) {
                        let at = cell.bytes.start.0;
                        assert_eq!(
                            token,
                            whole[at],
                            "at {width} columns with the caret at {caret}, byte {at} \
                             ({:?}) is {token:?} but the whole line says {:?}",
                            &source[at..(at + 1).min(source.len())],
                            whole[at]
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert!(
            checked > 500,
            "the sweep barely looked at anything: {checked}"
        );
        assert!(
            saw_split_word,
            "no row ever began inside a word, so the regression could not have been seen"
        );
    }

    /// Render one source row through the decoration pipeline and return the
    /// style landing on the cell for byte `offset`.
    fn styled_cell(
        theme: &Theme,
        source: &str,
        decorations: Decorations<'_>,
        offset: usize,
    ) -> ratatui::buffer::Cell {
        let mut editor = Editor::new(source).unwrap();
        let area = Rect::new(0, 0, 40, 3);
        let grid = source_grid(&editor, area, true);
        editor.set_view_size(usize::from(grid.width), usize::from(grid.height));
        let map = editor.screen_map(grid).unwrap();
        let mut buffer = Buffer::empty(area);
        let index = DecorationIndex::new(&map, &editor, decorations);
        let ScreenRow::Text(row) = &map.rows()[0] else {
            panic!("expected a text row");
        };
        render_text_row(
            &mut buffer,
            row,
            area.x,
            area.width,
            4,
            usize::MAX,
            &index,
            decorations.sliders,
            theme,
            row_tokens(editor.document(), row),
        );
        let cell = map
            .cell_for_offset(crate::editor::ByteOffset(offset))
            .expect("cell for offset");
        buffer
            .cell((cell.x, cell.y))
            .expect("rendered cell")
            .clone()
    }

    /// A row scrolled past the start of its line is coloured as that line.
    ///
    /// The pane is narrower than the score and the caret is at the end, so
    /// the row drawn begins in the middle of the line. A lexer started
    /// fresh there reads the closing quote of the string as an opening
    /// quote and paints every cell to the right edge as one string.
    #[test]
    fn a_row_scrolled_past_its_lines_opening_quote_keeps_its_colours() {
        let theme = theme();
        // Long enough that a narrow pane with the caret at the end starts
        // its row inside the string, so the row holds the quote that closes
        // it.
        let source = r#"$: s("bd sd hh oh bd sd hh oh").lpf(500)"#;
        let mut editor = Editor::new(source).unwrap();
        editor
            .set_selection(crate::editor::Selection::caret(crate::editor::ByteOffset(
                source.len(),
            )))
            .expect("caret at the end of the line");
        let area = Rect::new(0, 0, 20, 1);
        let grid = source_grid(&editor, area, true);
        editor.set_view_size(usize::from(grid.width), usize::from(grid.height));
        let map = editor.screen_map(grid).unwrap();
        let ScreenRow::Text(row) = &map.rows()[0] else {
            panic!("expected a text row");
        };
        let opening_quote = source.find('"').expect("the opening quote");
        let closing_quote = source.rfind('"').expect("the closing quote");
        let starts_at = row.cells.first().expect("a visible cell").bytes.start.0;
        assert!(
            starts_at > opening_quote && starts_at < closing_quote,
            "the row has to begin INSIDE the string for this to test anything: \
             starts at {starts_at}, quotes at {opening_quote} and {closing_quote}"
        );
        let mut buffer = Buffer::empty(area);
        let index = DecorationIndex::new(&map, &editor, Decorations::default());
        render_text_row(
            &mut buffer,
            row,
            area.x,
            area.width,
            4,
            usize::MAX,
            &index,
            &[],
            &theme,
            row_tokens(editor.document(), row),
        );
        // `500` is a number wherever the row happens to begin, and the `)`
        // beside it is punctuation - neither is inside a string.
        for (offset, wanted, what) in [
            (
                source.find("500").expect("the cutoff"),
                theme.syntax.number,
                "500",
            ),
            (
                source.len() - 1,
                theme.syntax.punctuation,
                "the closing paren",
            ),
        ] {
            let cell = map
                .cell_for_offset(crate::editor::ByteOffset(offset))
                .expect("cell for offset");
            let drawn = buffer.cell((cell.x, cell.y)).expect("rendered cell").fg;
            assert_eq!(
                drawn, wanted,
                "{what} was painted {drawn:?}, not {wanted:?} - the row read the closing quote as an opening one"
            );
        }
    }

    #[test]
    fn a_fading_mark_eases_back_to_the_plain_style() {
        let theme = theme();
        let base = Style::default()
            .fg(Color::Rgb(0, 0, 0))
            .bg(Color::Rgb(0, 0, 0));
        let marked = base
            .fg(Color::Rgb(200, 100, 0))
            .bg(Color::Rgb(100, 100, 100))
            .underline_color(Color::Rgb(200, 100, 0))
            .add_modifier(Modifier::BOLD);
        let half = crate::theme::fade_mark(base, marked, 0.5, theme.foreground, theme.background);
        assert_eq!(half.fg, Some(Color::Rgb(100, 50, 0)), "halfway in colour");
        assert_eq!(half.bg, Some(Color::Rgb(50, 50, 50)));
        assert!(
            half.add_modifier.contains(Modifier::BOLD),
            "the mark's weight while over half"
        );
        assert_eq!(half.underline_color, Some(Color::Rgb(200, 100, 0)));
        let faint = crate::theme::fade_mark(base, marked, 0.2, theme.foreground, theme.background);
        assert!(!faint.add_modifier.contains(Modifier::BOLD));
        assert_eq!(faint.underline_color, None);
        assert_eq!(faint.fg, Some(Color::Rgb(40, 20, 0)));
    }

    #[test]
    fn the_default_theme_tints_a_sounding_event_without_changing_syntax() {
        let theme = theme();
        let plain = styled_cell(&theme, "$: s(\"bd\")\n", Decorations::default(), 0);
        let marks = [SourceMark {
            from: 0,
            to: 4,
            color: Color::Rgb(255, 202, 40),
            onset_id: 1,
            strength: 1.0,
        }];
        let cell = styled_cell(
            &theme,
            "$: s(\"bd\")\n",
            Decorations {
                active: &marks,
                ..Decorations::default()
            },
            0,
        );
        assert_eq!(cell.fg, plain.fg);
        assert!(cell.modifier.contains(Modifier::BOLD));
        assert!(!cell.modifier.contains(Modifier::UNDERLINED));
        assert_eq!(cell.underline_color, Color::Reset);
        assert_ne!(cell.bg, Color::Reset, "the event should tint the cell");
        assert_ne!(
            cell.bg, marks[0].color,
            "the tint must not be a solid block"
        );
    }

    #[test]
    fn light_themes_render_visible_readable_sounding_marks() {
        for name in [
            "rustel-light",
            "solarized-light",
            "catppuccin-latte",
            "github-light",
            "emacs",
            "unicorn",
        ] {
            let theme = Theme::built_in(name).expect("bundled light theme");
            let marks = [SourceMark {
                from: 6,
                to: 8,
                color: theme.event,
                onset_id: 1,
                strength: 1.0,
            }];
            let cell = styled_cell(
                &theme,
                "$: s(\"bd\")\n",
                Decorations {
                    active: &marks,
                    ..Decorations::default()
                },
                6,
            );
            assert!(
                crate::theme::contrast_ratio(cell.bg, theme.background) >= 1.28,
                "{name}: mark disappeared"
            );
            assert!(
                crate::theme::contrast_ratio(cell.fg, cell.bg) >= 4.48,
                "{name}: marked string became unreadable"
            );
        }
    }

    #[test]
    fn mini_notation_is_tinted_rather_than_filled_by_default() {
        let theme = theme();
        assert_eq!(theme.mini_fill, None);
        let painted = styled_cell(
            &theme,
            "$: s(\"bd\")\n",
            Decorations {
                mini: &[(6, 8)],
                ..Decorations::default()
            },
            6,
        );
        assert_eq!(painted.fg, theme.mini);
        assert_eq!(painted.bg, Color::Reset);
    }

    #[test]
    fn a_lint_finding_is_underlined_in_the_error_colour() {
        let theme = theme();
        let source = "$: s(\"bd [hh\")\n";
        let from = source.find('[').unwrap();
        let painted = styled_cell(
            &theme,
            source,
            Decorations {
                mini: &[(from - 3, from + 3)],
                errors: &[(from, from + 1)],
                ..Decorations::default()
            },
            from,
        );
        assert_eq!(painted.fg, theme.mini);
        assert!(painted.modifier.contains(Modifier::UNDERLINED));
        assert_eq!(painted.underline_color, theme.error);
    }

    #[test]
    fn automatic_brackets_complement_all_six_caret_shapes() {
        use super::super::terminal::CaretShape;
        let theme = theme();
        let source = "s(\"bd\")";
        for caret_shape in CaretShape::ALL {
            let block = matches!(
                caret_shape,
                CaretShape::SteadyUnderline | CaretShape::BlinkingUnderline
            );
            for matched in [true, false] {
                let painted = styled_cell(
                    &theme,
                    source,
                    Decorations {
                        brackets: &[(1, 2, matched)],
                        caret_shape,
                        ..Decorations::default()
                    },
                    1,
                );
                assert_eq!(
                    painted.modifier.contains(Modifier::UNDERLINED),
                    !block,
                    "{caret_shape:?}"
                );
                if block {
                    assert_ne!(painted.bg, theme.background);
                    assert_ne!(painted.fg, theme.background);
                    let contrast =
                        super::super::theme::contrast_ratio(painted.bg, theme.background);
                    assert!(
                        (1.4..1.55).contains(&contrast),
                        "{caret_shape:?}: {contrast}"
                    );
                } else {
                    assert_eq!(
                        painted.underline_color,
                        if matched {
                            theme.bracket()
                        } else {
                            theme.error
                        }
                    );
                }
            }
        }
    }

    #[test]
    fn matched_brackets_use_the_themes_underlined_highlight() {
        let mut theme = theme();
        theme.bracket_mark = super::super::theme::BracketMark::Underline;
        let source = "$: s(\"bd\")\n";
        let from = source.find('(').unwrap();
        let painted = styled_cell(
            &theme,
            source,
            Decorations {
                brackets: &[(from, from + 1, true)],
                ..Decorations::default()
            },
            from,
        );
        assert_eq!(painted.fg, theme.syntax.punctuation);
        assert_eq!(painted.underline_color, theme.bracket());
        assert!(painted.modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn the_footer_shows_a_lint_finding_when_there_is_no_error() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let area = Rect::new(0, 0, 150, 2);
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.lint = Some("line 2: [mini] parse error");
        let mut buffer = Buffer::empty(area);
        Footer { chrome: &chrome }.render(area, &mut buffer);
        let text = text_of(&buffer);
        assert!(text.contains("✗ line 2: [mini] parse error"), "{text}");
        assert!(!text.contains("ready"), "the status yields to the finding");
        assert_eq!(buffer.cell((1, 0)).unwrap().fg, theme.warn);
    }

    /// The header names the scene under its set, the way the strip names
    /// it and never as a path; when the row runs short the set goes and
    /// the scene's name is what remains.
    #[test]
    fn the_header_names_the_scene_under_its_set() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(
            &theme,
            &master,
            Path::new("/Users/EXAMPLE/.rustel/sets/opening night/live.strudel"),
        );
        chrome.set_name = "opening night";
        let render = |width: u16| {
            let area = Rect::new(0, 0, width, 1);
            let mut buffer = Buffer::empty(area);
            Header { chrome: &chrome }.render(area, &mut buffer);
            text_of(&buffer)
        };
        let wide = render(200);
        assert!(wide.contains("opening night \u{25b8} live"), "{wide}");
        assert!(
            !wide.contains(".rustel") && !wide.contains(".strudel"),
            "no path, no extension: {wide}"
        );
        let mut width = 200;
        while render(width).contains("opening night \u{25b8}") {
            width -= 1;
        }
        let shortest = render(width);
        assert!(
            shortest.contains("live"),
            "the scene's name is what remains: {shortest}"
        );
    }

    /// The control is upstream's: a pill over `slider(` alone, the numbers
    /// after it left as the text they are - and the pill takes room on its
    /// row to be twice the keyword, the text after it moving along.
    #[test]
    fn a_closed_slider_is_drawn_as_a_control_over_its_call() {
        let theme = theme();
        let source = "$: s(\"bd\").gain(slider(.25, 0, 1))\n";
        let call = source.find("slider(").unwrap();
        let end = call + "slider(".len();
        let chip = SliderChip {
            from: call,
            to: end,
            notch: 0.25,
            armed: false,
        };

        let mut editor = Editor::new(source).unwrap();
        editor.set_inline_widths(vec![crate::editor::InlineWidth {
            at: crate::editor::ByteOffset(end - 1),
            extra: 7,
        }]);
        let area = Rect::new(0, 0, 60, 3);
        let grid = source_grid(&editor, area, true);
        editor.set_view_size(usize::from(grid.width), usize::from(grid.height));
        let map = editor.screen_map(grid).unwrap();
        let mut buffer = Buffer::empty(area);
        let decorations = Decorations {
            sliders: std::slice::from_ref(&chip),
            ..Decorations::default()
        };
        let index = DecorationIndex::new(&map, &editor, decorations);
        let ScreenRow::Text(row) = &map.rows()[0] else {
            panic!("expected a text row");
        };
        render_text_row(
            &mut buffer,
            row,
            area.x,
            area.width,
            4,
            usize::MAX,
            &index,
            decorations.sliders,
            &theme,
            row_tokens(editor.document(), row),
        );

        let start = map
            .cell_for_offset(crate::editor::ByteOffset(call))
            .expect("cell for the call");
        let after = map
            .cell_for_offset(crate::editor::ByteOffset(end))
            .expect("cell for the value");
        assert_eq!(after.y, start.y);
        let width = after.x - start.x;
        assert_eq!(width, 14, "the pill is twice the keyword");
        // A press anywhere on the pill lands on the control, not on the
        // numbers after it.
        for column in start.x..after.x {
            let (offset, _) = row.hit_test(usize::from(column - grid.x));
            assert!(
                (call..end).contains(&offset.0),
                "column {column} hits byte {} outside the pill",
                offset.0
            );
        }
        let drawn: String = (0..width)
            .map(|column| {
                buffer
                    .cell((start.x + column, start.y))
                    .unwrap()
                    .symbol()
                    .to_owned()
            })
            .collect();
        assert!(drawn.contains('█'), "the knob is drawn: {drawn:?}");
        assert!(drawn.contains('─'), "the track is drawn: {drawn:?}");
        assert!(
            !drawn.contains("slider"),
            "nothing of the keyword shows through: {drawn:?}"
        );
        // The knob sits a quarter of the way along the track.
        let track: Vec<char> = drawn.chars().filter(|c| *c == '█' || *c == '─').collect();
        let knob = track.iter().position(|c| *c == '█').unwrap();
        assert!(
            (knob as f64) < (track.len() as f64) * 0.5,
            "a 0.25 knob sits left of centre: {knob} of {}",
            track.len()
        );
        assert_eq!(track.len(), 14, "the whole pill is track");
        let after: String = (0..10)
            .map(|column| {
                buffer
                    .cell((start.x + width + column, start.y))
                    .unwrap()
                    .symbol()
                    .to_owned()
            })
            .collect();
        assert_eq!(after, ".25, 0, 1)", "the numbers stay text after the pill");
    }

    /// The selection ground shows which slider has the arrows. Its accent
    /// and glyphs stay the same while the pointer is interacting with it.
    #[test]
    fn an_armed_slider_is_drawn_selected() {
        let theme = theme();
        let source = "$: s(\"bd\").gain(slider(.25, 0, 1))\n";
        let call = source.find("slider(").unwrap();
        let end = call + "slider(".len();
        let render = |armed: bool| {
            let chip = SliderChip {
                from: call,
                to: end,
                notch: 0.25,
                armed,
            };
            let mut editor = Editor::new(source).unwrap();
            editor.set_inline_widths(vec![crate::editor::InlineWidth {
                at: crate::editor::ByteOffset(end - 1),
                extra: 7,
            }]);
            let area = Rect::new(0, 0, 60, 3);
            let grid = source_grid(&editor, area, true);
            editor.set_view_size(usize::from(grid.width), usize::from(grid.height));
            let map = editor.screen_map(grid).unwrap();
            let mut buffer = Buffer::empty(area);
            let decorations = Decorations {
                sliders: std::slice::from_ref(&chip),
                ..Decorations::default()
            };
            let index = DecorationIndex::new(&map, &editor, decorations);
            let ScreenRow::Text(row) = &map.rows()[0] else {
                panic!("expected a text row");
            };
            render_text_row(
                &mut buffer,
                row,
                area.x,
                area.width,
                4,
                usize::MAX,
                &index,
                decorations.sliders,
                &theme,
                row_tokens(editor.document(), row),
            );
            let start = map
                .cell_for_offset(crate::editor::ByteOffset(call))
                .expect("cell for the call");
            (buffer, start.x)
        };
        let (plain, x) = render(false);
        let (armed, _) = render(true);
        let pill = x..x + 14;
        assert!(
            pill.clone()
                .all(|x| armed.cell((x, 0)).unwrap().bg == theme.selection),
            "the armed pill wears the selection's ground"
        );
        assert!(
            pill.clone()
                .any(|x| plain.cell((x, 0)).unwrap().bg != theme.selection),
            "the plain pill does not"
        );
        for x in pill.clone() {
            let before = plain.cell((x, 0)).unwrap();
            let after = armed.cell((x, 0)).unwrap();
            assert_eq!(after.symbol(), before.symbol(), "focus keeps the same rail");
            assert_eq!(before.fg, theme.accent);
            assert_eq!(after.fg, theme.accent);
        }
        assert_eq!(
            pill.clone()
                .filter(|&x| armed.cell((x, 0)).unwrap().symbol() == "█")
                .count(),
            1,
            "one knob either way"
        );
    }

    #[test]
    fn slider_cells_replace_source_styles_and_emit_one_continuous_rail() {
        use ratatui::backend::{Backend, CrosstermBackend};

        let source = "slider(.25, 0, 1)";
        let end = "slider(".len();
        let mut editor = Editor::new(source).unwrap();
        editor.set_inline_widths(vec![crate::editor::InlineWidth {
            at: crate::editor::ByteOffset(end - 1),
            extra: 13,
        }]);
        let area = Rect::new(0, 0, 40, 1);
        let grid = crate::editor::GridRect::new(0, 0, 40, 1);
        editor.set_view_size(40, 1);
        let map = editor.screen_map(grid).unwrap();
        let ScreenRow::Text(row) = &map.rows()[0] else {
            panic!("text row")
        };
        let (start, right) = slider_cover_bounds(row, 0, end).unwrap();
        assert_eq!(right - start, 20);
        let rail = format!("{}█{}", "─".repeat(5), "─".repeat(14));

        for armed in [false, true] {
            for filled in [false, true] {
                for current in [false, true] {
                    let mut theme = theme();
                    theme.slider_fill = filled.then_some(Color::Rgb(10, 20, 30));
                    theme.current_line = Some(Color::Rgb(30, 20, 10));
                    theme.event_mark = crate::theme::MarkStyle::Invert;
                    let chip = SliderChip {
                        from: 0,
                        to: end,
                        notch: 0.25,
                        armed,
                    };
                    let marks = [SourceMark {
                        from: 2,
                        to: 4,
                        color: Color::Red,
                        onset_id: 1,
                        strength: 1.0,
                    }];
                    let errors = [(0, 1), (end, end + 1)];
                    let overridden = [(4, 5)];
                    let brackets = [(end - 1, end, true)];
                    let decorations = Decorations {
                        sliders: std::slice::from_ref(&chip),
                        active: &marks,
                        errors: &errors,
                        overridden: &overridden,
                        brackets: &brackets,
                        ..Decorations::default()
                    };
                    let index = DecorationIndex::new(&map, &editor, decorations);
                    let mut buffer = Buffer::empty(area);
                    // Simulate differently styled text and background cells
                    // under a control, including the expanded '(' cells.
                    for x in start..right {
                        buffer.cell_mut((x, 0)).unwrap().set_style(
                            Style::default()
                                .fg(Color::Yellow)
                                .bg(if x % 2 == 0 { Color::Blue } else { Color::Red })
                                .underline_color(Color::Green)
                                .add_modifier(if x % 2 == 0 {
                                    Modifier::DIM | Modifier::REVERSED
                                } else {
                                    Modifier::ITALIC | Modifier::UNDERLINED
                                }),
                        );
                    }
                    render_text_row(
                        &mut buffer,
                        row,
                        0,
                        40,
                        0,
                        if current { 0 } else { usize::MAX },
                        &index,
                        decorations.sliders,
                        &theme,
                        row_tokens(editor.document(), row),
                    );
                    let background = if armed {
                        theme.selection
                    } else {
                        theme.slider_fill.unwrap_or(if current {
                            theme.current_line.unwrap()
                        } else {
                            theme.background
                        })
                    };
                    for x in start..right {
                        let cell = buffer.cell((x, 0)).unwrap();
                        assert_eq!(cell.fg, theme.accent, "column {x}");
                        assert_eq!(cell.bg, background, "column {x}");
                        assert_eq!(cell.modifier, Modifier::BOLD, "column {x}");
                        assert_eq!(cell.underline_color, Color::Reset, "column {x}");
                    }
                    let literal = buffer.cell((right, 0)).unwrap();
                    assert_eq!(literal.symbol(), ".");
                    assert!(literal.modifier.contains(Modifier::UNDERLINED));
                    assert_eq!(
                        literal.fg, theme.syntax.punctuation,
                        "the diagnostic retains the value's syntax colour"
                    );

                    // Exercise the same backend as Studio: no inherited
                    // per-cell SGR change can split the emitted rail.
                    let mut output = Vec::new();
                    {
                        let mut backend = CrosstermBackend::new(&mut output);
                        backend
                            .draw((start..right).map(|x| (x, 0, buffer.cell((x, 0)).unwrap())))
                            .unwrap();
                    }
                    let output = std::str::from_utf8(&output).unwrap();
                    assert!(output.contains(&rail), "{output:?}");
                    assert!(!output.contains('━'));
                }
            }
        }
    }

    #[test]
    fn an_armed_mono_slider_uses_the_themes_contrasting_selection_pair() {
        let theme = Theme::built_in("mono").expect("the bundled monochrome theme");
        assert_eq!(theme.accent, theme.selection);
        let area = Rect::new(0, 0, 20, 1);
        let mut plain = Buffer::empty(area);
        let mut armed = Buffer::empty(area);
        let mut chip = SliderChip {
            from: 0,
            to: 7,
            notch: 0.5,
            armed: false,
        };
        draw_slider_control(&mut plain, area, &chip, &theme, theme.background, false);
        chip.armed = true;
        draw_slider_control(&mut armed, area, &chip, &theme, theme.background, false);
        for x in 0..20 {
            let before = plain.cell((x, 0)).unwrap();
            let after = armed.cell((x, 0)).unwrap();
            assert_eq!(before.fg, theme.accent);
            assert_eq!(after.fg, theme.selection_text);
            assert_eq!(after.bg, theme.selection);
            assert_ne!(after.fg, after.bg, "the track and handle remain visible");
            assert_eq!(after.symbol(), before.symbol(), "focus keeps the same rail");
            assert_eq!(after.modifier, Modifier::BOLD);
        }
    }

    #[test]
    fn a_source_selection_selects_the_whole_slider_cover_without_selecting_it_from_the_value() {
        use crate::editor::{ByteOffset, GridRect, Selection};

        let source = "pre(slider(.25, 0, 1));";
        let call = source.find("slider(").unwrap();
        let end = call + "slider(".len();
        let area = Rect::new(0, 0, 60, 1);
        let chip = SliderChip {
            from: call,
            to: end,
            notch: 0.25,
            armed: false,
        };
        for theme in [theme(), Theme::built_in("mono").unwrap()] {
            // Full and partial cover selections, a selection starting at
            // the value, and an ordinary caret outside the slider call.
            for (anchor, selected_cover) in [
                (Some(call - 1), true),
                (Some(call + 3), true),
                (Some(end), false),
                (None, false),
            ] {
                let mut editor = Editor::new(source).unwrap();
                editor.set_inline_widths(vec![crate::editor::InlineWidth {
                    at: ByteOffset(end - 1),
                    extra: 13,
                }]);
                editor.set_view_size(60, 1);
                editor
                    .set_selection(match anchor {
                        Some(anchor) => {
                            Selection::range(ByteOffset(anchor), ByteOffset(source.len()))
                        }
                        None => Selection::caret(ByteOffset(source.len())),
                    })
                    .unwrap();
                let map = editor.screen_map(GridRect::new(0, 0, 60, 1)).unwrap();
                let ScreenRow::Text(row) = &map.rows()[0] else {
                    panic!("text row")
                };
                let (start, right) = slider_cover_bounds(row, call, end).unwrap();
                assert_eq!(right - start, 20);
                let errors = [(end, end + 3)];
                let decorations = Decorations {
                    sliders: std::slice::from_ref(&chip),
                    errors: &errors,
                    ..Decorations::default()
                };
                let index = DecorationIndex::new(&map, &editor, decorations);
                let mut buffer = Buffer::empty(area);
                render_text_row(
                    &mut buffer,
                    row,
                    0,
                    60,
                    0,
                    usize::MAX,
                    &index,
                    decorations.sliders,
                    &theme,
                    row_tokens(editor.document(), row),
                );
                for x in start..right {
                    let cell = buffer.cell((x, 0)).unwrap();
                    assert_eq!(
                        cell.bg,
                        if selected_cover {
                            theme.selection
                        } else {
                            theme.background
                        }
                    );
                    assert_eq!(
                        cell.fg,
                        if selected_cover && theme.accent == theme.selection {
                            theme.selection_text
                        } else {
                            theme.accent
                        }
                    );
                    assert_eq!(cell.modifier, Modifier::BOLD);
                }
                let literal = buffer.cell((right, 0)).unwrap();
                assert_eq!(literal.symbol(), ".");
                assert!(literal.modifier.contains(Modifier::UNDERLINED));
                assert_eq!(literal.underline_color, theme.error);
                assert_eq!(
                    literal.fg,
                    if anchor.is_some() {
                        theme.selection_text
                    } else {
                        theme.syntax.punctuation
                    }
                );
                if anchor.is_some() {
                    assert_eq!(literal.bg, theme.selection);
                }
                assert!(!chip.armed, "text selection does not arm the control");
            }
        }
    }

    #[test]
    fn a_theme_can_ask_for_a_filled_event_marking_instead() {
        let mut theme = theme();
        theme.event_mark = crate::theme::MarkStyle::Fill;
        let marks = [SourceMark {
            from: 0,
            to: 4,
            color: Color::Rgb(255, 202, 40),
            onset_id: 1,
            strength: 1.0,
        }];
        let cell = styled_cell(
            &theme,
            "$: s(\"bd\")\n",
            Decorations {
                active: &marks,
                ..Decorations::default()
            },
            0,
        );
        assert_eq!(cell.bg, Color::Rgb(255, 202, 40));
    }

    /// At every width from 30 to 200 columns a chip's hit rect is empty
    /// exactly when none of it was painted, and a painted chip stops short
    /// of the status text's end.
    #[test]
    fn a_chip_that_lost_the_squeeze_keeps_no_clickable_rect() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.device = Some("Scarlett 2i2");
        chrome.midi_ports = midi_ports(2, 1);
        let mut squeezed = 0;
        for width in 30..=200u16 {
            let area = Rect::new(0, 20, width, 2);
            let mut buffer = Buffer::empty(Rect::new(0, 0, width, 22));
            Footer { chrome: &chrome }.render(area, &mut buffer);
            let hits = footer_hits(area, Some("Scarlett 2i2"), None, midi_ports(2, 1), 0, &[]);
            let rows = (area.y..area.bottom())
                .map(|y| {
                    (area.x..area.right())
                        .filter_map(|x| buffer.cell((x, y)))
                        .map(|cell| cell.symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>();
            let painted = |text: &str| rows.iter().any(|row| row.contains(text));
            for (rect, text) in [(hits.device_chip, "Scarlett"), (hits.midi_chip, "2 out")] {
                if rect.is_empty() {
                    assert!(!painted(text), "width {width}: {text} painted with no rect");
                    squeezed += 1;
                } else {
                    assert!(
                        painted(text),
                        "width {width}: {text} has a rect but no paint"
                    );
                    assert!(
                        rect.right() <= hits.status.right(),
                        "width {width}: {text} chip runs past the status text into the dock"
                    );
                }
            }
        }
        assert!(squeezed > 0, "no width squeezed a chip out");
    }

    #[test]
    fn piano_footer_pulses_only_its_label_and_preserves_errors_and_controls() {
        UiSettings::default().apply();
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.status = "PIANO  C4 + E4 + G4";
        chrome.lint = Some("an unrelated lint finding");
        chrome.caret = Some((42, 17));
        chrome.error = Some("evaluation failed");
        chrome.device = Some("Speakers");
        chrome.midi_ports = midi_ports(1, 1);
        let orbits = [OrbitLevel {
            orbit: 1,
            peak: 0.4,
            pair: 0,
        }];
        chrome.orbits = &orbits;
        let area = Rect::new(0, 20, 150, 3);
        let frame = Rect::new(0, 0, 150, 23);
        let content = footer_content_area(area, 1, false, true);
        let hits = footer_hits(content, chrome.device, None, chrome.midi_ports, 0, &orbits);
        let mut normal = Buffer::empty(frame);
        Footer { chrome: &chrome }.render(area, &mut normal);

        let mut phases = Vec::new();
        for pulse in [0.0, 0.5, 1.0] {
            chrome.piano = Some(pulse);
            let mut buffer = Buffer::empty(frame);
            buffer.set_string(1, area.y - 1, "score stays visible", Style::default());
            Footer { chrome: &chrome }.render(area, &mut buffer);
            let row = |y| {
                (area.x..area.right())
                    .map(|x| buffer.cell((x, y)).unwrap().symbol())
                    .collect::<String>()
            };
            let status = row(content.y);
            assert!(status.contains("PIANO  C4 + E4 + G4"), "{status}");
            assert!(!status.contains("lint finding"));
            assert!(
                !status.contains("42,17"),
                "piano notes get the ruler's room"
            );
            assert!(row(area.y).contains("evaluation failed"));
            assert!(row(area.y - 1).contains("score stays visible"));
            let label = buffer.cell((content.x + 1, content.y)).unwrap();
            assert!(label.modifier.contains(Modifier::BOLD));
            assert_eq!(label.fg, theme.surface);
            assert_eq!(label.bg, mix(theme.surface, theme.accent, pulse));
            for rect in [
                hits.dock,
                hits.scope,
                hits.device_chip,
                hits.midi_chip,
                hits.orbits[0].1,
            ] {
                for y in rect.y..rect.bottom() {
                    for x in rect.x..rect.right() {
                        assert_eq!(
                            buffer.cell((x, y)),
                            normal.cell((x, y)),
                            "control at {x},{y}"
                        );
                    }
                }
            }
            phases.push(buffer);
        }
        // The chord stays readable and stationary throughout the software pulse.
        for x in content.x + 6..hits.status.right() {
            assert_eq!(
                phases[0].cell((x, content.y)),
                phases[1].cell((x, content.y))
            );
            assert_eq!(
                phases[0].cell((x, content.y)),
                phases[2].cell((x, content.y))
            );
        }
    }

    #[test]
    fn piano_footer_keeps_its_label_when_narrow_and_errors_when_too_short() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.status = "PIANO  C4 + E4 + G4";
        chrome.lint = Some("lint must not hide piano mode");
        chrome.caret = Some((999, 999));
        for width in 0..80 {
            for height in 0..=3 {
                let area = Rect::new(3, 1, width, height);
                let mut buffer = Buffer::empty(Rect::new(0, 0, area.right(), area.bottom()));
                chrome.piano = Some(f32::from(width % 2));
                Footer { chrome: &chrome }.render(area, &mut buffer);
                let hits = footer_hits(area, None, None, midi_ports(0, 0), 0, &[]);
                for (offset, character) in "PIANO"
                    .chars()
                    .take(usize::from(hits.status.width))
                    .enumerate()
                {
                    assert_eq!(
                        buffer
                            .cell((hits.status.x + offset as u16, hits.status.y))
                            .unwrap()
                            .symbol(),
                        character.to_string(),
                        "{width}x{height} keeps the mode label"
                    );
                }
            }
        }
        chrome.error = Some("evaluation failed");
        let area = Rect::new(0, 0, 80, 1);
        let mut buffer = Buffer::empty(area);
        Footer { chrome: &chrome }.render(area, &mut buffer);
        let text = text_of(&buffer);
        assert!(text.contains("evaluation failed"), "{text}");
        assert!(
            !text.contains("PIANO"),
            "the only row must retain its error"
        );
    }

    #[test]
    fn retained_piano_footer_fits_short_terminals_and_prioritizes_errors() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.piano_notes = Some("Notes  C4 + E4 + G4 · [60,64,67]");
        for error in [None, Some("evaluation failed")] {
            chrome.error = error;
            for height in 1..=12 {
                let area = Rect::new(0, 0, 80, height);
                let regions = regions_with_footer(
                    area,
                    ChromeLayout::shown(false),
                    2,
                    false,
                    false,
                    None,
                    [None, None],
                    None,
                    None,
                    None,
                    u16::from(error.is_some()),
                    true,
                );
                assert!(regions.footer.bottom() <= area.bottom());
                let mut buffer = Buffer::empty(area);
                Footer { chrome: &chrome }.render(regions.footer, &mut buffer);
                let text = text_of(&buffer);
                if error.is_some() {
                    assert!(
                        text.contains("evaluation failed"),
                        "height {height}: {text}"
                    );
                }
                if regions.footer.height >= 3 + u16::from(error.is_some()) {
                    assert!(
                        text.contains("Notes  C4 + E4 + G4"),
                        "height {height}: {text}"
                    );
                }
            }
        }
    }

    #[test]
    fn audio_warning_has_a_full_width_row_and_keeps_footer_controls() {
        UiSettings::default().apply();
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.status = "ready";
        chrome.audio_warning =
            Some("Audio struggling · try raising audio out latency in Settings → Advanced");
        chrome.device = Some("Speakers");
        for width in [80, 100] {
            let area = Rect::new(0, 0, width, 3);
            let content = footer_content_area(area, 1, false, true);
            let hits = footer_hits(content, chrome.device, None, midi_ports(0, 0), 0, &[]);
            let mut buffer = Buffer::empty(area);
            Footer { chrome: &chrome }.render(area, &mut buffer);
            let row = |y| {
                (area.x..area.right())
                    .map(|x| buffer.cell((x, y)).unwrap().symbol())
                    .collect::<String>()
            };
            assert!(row(0).contains(chrome.audio_warning.unwrap()));
            assert_eq!(buffer.cell((1, 0)).unwrap().fg, theme.warn);
            assert!(row(1).contains("ready"));
            assert!(row(2).contains("Speakers"));
            assert_eq!(hits.status.y, 1);
            assert_eq!(hits.device_chip.y, 2);
            assert!(hits.dock.y > area.y);
        }

        let area = Rect::new(0, 0, 50, 3);
        let mut buffer = Buffer::empty(area);
        Footer { chrome: &chrome }.render(area, &mut buffer);
        assert!(text_of(&buffer).contains("Try raising latency: Settings → Advanced"));
    }

    #[test]
    fn audio_warning_remains_actionable_below_a_query_error() {
        UiSettings::default().apply();
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.error = Some("the query exceeded its 249 ms deadline and was refused");
        chrome.status = "the producer fell behind the audio clock; resumed at the present";
        chrome.audio_warning =
            Some("Audio struggling · try raising audio out latency in Settings → Advanced");
        chrome.device = Some("Speakers");
        for width in [50, 100] {
            let regions = regions_with_footer(
                Rect::new(0, 0, width, 30),
                ChromeLayout::shown(false),
                1,
                false,
                false,
                None,
                [None, None],
                None,
                None,
                None,
                2,
                false,
            );
            assert_eq!(regions.footer.height, 4);
            let area = Rect::new(0, 0, width, regions.footer.height);
            let content = footer_content_area(area, 2, false, true);
            let hits = footer_hits(content, chrome.device, None, midi_ports(0, 0), 0, &[]);
            let mut buffer = Buffer::empty(area);
            Footer { chrome: &chrome }.render(area, &mut buffer);
            let row = |y| {
                (area.x..area.right())
                    .map(|x| buffer.cell((x, y)).unwrap().symbol())
                    .collect::<String>()
            };
            assert!(row(0).contains("the query exceeded its 249 ms deadline"));
            assert_eq!(buffer.cell((1, 0)).unwrap().fg, theme.error);
            assert!(row(1).contains("raising"), "warning row: {}", row(1));
            assert!(
                row(1).contains("Settings → Advanced"),
                "warning row: {}",
                row(1)
            );
            assert_eq!(buffer.cell((1, 1)).unwrap().fg, theme.warn);
            assert!(row(2).contains("the producer"));
            assert!(row(3).contains("Speakers"));
            assert_eq!(hits.status.y, 2);
            assert_eq!(hits.device_chip.y, 3);
            assert_eq!(hits.dock.y, 2);
        }
    }

    #[test]
    fn audio_warning_preserves_errors_piano_and_retained_notes() {
        UiSettings::default().apply();
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.status = "PIANO  C4 + E4 + G4";
        chrome.piano = Some(0.5);
        chrome.piano_notes = Some("Notes  C4 + E4 + G4");
        chrome.device = Some("Speakers");
        chrome.audio_warning =
            Some("Audio struggling · try raising audio out latency in Settings → Advanced");
        let row = |buffer: &Buffer, y| {
            (0..100)
                .map(|x| buffer.cell((x, y)).unwrap().symbol())
                .collect::<String>()
        };
        let area = Rect::new(0, 0, 100, 4);
        let mut buffer = Buffer::empty(area);
        Footer { chrome: &chrome }.render(area, &mut buffer);
        let text = text_of(&buffer);
        assert!(text.contains(chrome.audio_warning.unwrap()));
        assert!(text.contains("Notes  C4 + E4 + G4"));
        assert!(text.contains("PIANO  C4 + E4 + G4"));
        assert!(row(&buffer, 0).contains("Notes  C4 + E4 + G4"));
        assert!(row(&buffer, 1).contains(chrome.audio_warning.unwrap()));
        assert!(row(&buffer, 2).contains("PIANO"));
        assert!(row(&buffer, 3).contains("Speakers"));

        let short_area = Rect::new(0, 0, 100, 3);
        let mut buffer = Buffer::empty(short_area);
        Footer { chrome: &chrome }.render(short_area, &mut buffer);
        assert!(row(&buffer, 0).contains("Notes  C4 + E4 + G4"));
        assert!(row(&buffer, 1).contains("PIANO"));
        assert!(row(&buffer, 2).contains("Speakers"));
        assert!(!text_of(&buffer).contains("Audio struggling"));

        chrome.error = Some("evaluation failed");
        let full_area = Rect::new(0, 0, 100, 5);
        let mut buffer = Buffer::empty(full_area);
        Footer { chrome: &chrome }.render(full_area, &mut buffer);
        let text = text_of(&buffer);
        assert!(text.contains("evaluation failed"));
        assert!(text.contains(chrome.audio_warning.unwrap()));
        assert!(text.contains("Notes  C4 + E4 + G4"));
        assert!(text.contains("PIANO  C4 + E4 + G4"));
        assert!(row(&buffer, 0).contains("evaluation failed"));
        assert!(row(&buffer, 1).contains("Notes  C4 + E4 + G4"));
        assert!(row(&buffer, 2).contains(chrome.audio_warning.unwrap()));
        assert!(row(&buffer, 3).contains("PIANO"));
        assert!(row(&buffer, 4).contains("Speakers"));

        let mut buffer = Buffer::empty(area);
        Footer { chrome: &chrome }.render(area, &mut buffer);
        let text = text_of(&buffer);
        assert!(text.contains("evaluation failed"));
        assert!(!text.contains("Audio struggling"));
        assert!(text.contains("Notes  C4 + E4 + G4"));
        assert!(text.contains("PIANO  C4 + E4 + G4"));

        for height in [1, 2] {
            let area = Rect::new(0, 0, 100, height);
            let mut buffer = Buffer::empty(area);
            Footer { chrome: &chrome }.render(area, &mut buffer);
            assert!(text_of(&buffer).contains("evaluation failed"));
            chrome.error = None;
            let mut buffer = Buffer::empty(area);
            Footer { chrome: &chrome }.render(area, &mut buffer);
            assert!(text_of(&buffer).contains("PIANO"));
            chrome.error = Some("evaluation failed");
        }
    }

    /// With no row to spare above the controls, the hint takes the status line
    /// in the warning colour when no error or piano claims it, short where the
    /// full sentence would not fit.
    #[test]
    fn audio_warning_takes_the_status_line_when_no_row_is_free() {
        UiSettings::default().apply();
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.status = "ready";
        chrome.audio_warning =
            Some("Audio struggling · try raising audio out latency in Settings → Advanced");
        for (width, hint) in [(100, "Audio struggling"), (50, "Try raising latency")] {
            for height in [1, 2] {
                let area = Rect::new(0, 0, width, height);
                let mut buffer = Buffer::empty(area);
                Footer { chrome: &chrome }.render(area, &mut buffer);
                let row = (0..width)
                    .map(|x| buffer.cell((x, 0)).unwrap().symbol())
                    .collect::<String>();
                assert!(row.contains(hint), "{width}x{height}: {row}");
                assert_eq!(buffer.cell((1, 0)).unwrap().fg, theme.warn);
                assert!(!text_of(&buffer).contains("ready"), "{width}x{height}");
            }
        }
    }

    #[test]
    fn an_error_gets_a_footer_row_of_its_own() {
        UiSettings::default().apply();
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.status = "ready";
        chrome.error = Some("evaluation · javascript: ReferenceError: ssss is not defined");
        chrome.device = Some("Speakers");

        let area = Rect::new(0, 20, 150, 3);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 150, 23));
        Footer { chrome: &chrome }.render(area, &mut buffer);
        let row = |y| {
            (area.x..area.right())
                .filter_map(|x| buffer.cell((x, y)))
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        let error = row(area.y);
        let status = row(area.y + 1);
        let devices = row(area.y + 2);
        assert!(error.contains("ReferenceError"), "error row: {error:?}");
        assert!(
            !error.contains("ready"),
            "status leaked onto error row: {error:?}"
        );
        assert!(
            !error.contains("Speakers"),
            "devices leaked onto error row: {error:?}"
        );
        assert!(
            status.contains("ready"),
            "ordinary status stays below: {status:?}"
        );
        assert!(
            devices.contains("Speakers"),
            "devices keep their row: {devices:?}"
        );

        let layout_area = Rect::new(0, 0, 150, 30);
        let plain = regions_with(
            layout_area,
            false,
            1,
            false,
            false,
            None,
            [None, None],
            None,
            None,
        );
        let errored = regions_with_footer(
            layout_area,
            ChromeLayout::shown(false),
            1,
            false,
            false,
            None,
            [None, None],
            None,
            None,
            None,
            1,
            false,
        );
        assert_eq!(errored.footer.height, plain.footer.height + 1);
        assert_eq!(errored.footer.bottom(), plain.footer.bottom());
    }

    /// The footer shows the caret as a vim-style ruler (`line,col`) right of
    /// the status message. A long message is cut with an ellipsis, and a
    /// status area too narrow for the ruler gives the row to the message.
    #[test]
    fn the_footer_reads_the_caret_as_a_vim_style_ruler() {
        UiSettings::default().apply();
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.caret = Some((2, 4));
        let row = |chrome: &StudioChrome, area: Rect| {
            let mut buffer = Buffer::empty(Rect::new(0, 0, area.right(), area.bottom()));
            Footer { chrome }.render(area, &mut buffer);
            (area.x..area.right())
                .filter_map(|x| buffer.cell((x, area.y)))
                .map(|cell| cell.symbol())
                .collect::<String>()
        };

        // A short message is unchanged, with the ruler to its right.
        let area = Rect::new(0, 20, 150, 2);
        chrome.status = "ready";
        let wide = row(&chrome, area);
        let ruler_at = wide.find("2,4").expect("the ruler is on the status row");
        let ready_at = wide.find("ready").expect("the message keeps the row");
        assert!(
            ruler_at > ready_at,
            "the ruler sits after the message: {wide}"
        );

        // A message too long for the room left of the ruler is shortened
        // to fit, ending with an ellipsis; the ruler still reads.
        let long = "compiling the whole score after the sample cache moved, \
                     which will take a little while";
        chrome.status = long;
        let crowded = row(&chrome, area);
        assert!(crowded.contains("2,4"), "the ruler still reads: {crowded}");
        assert!(
            !crowded.contains(long),
            "the long message is cut down to fit: {crowded}"
        );
        assert!(
            crowded.contains('…'),
            "the cut message ends with an ellipsis: {crowded}"
        );

        // A status area too narrow for the ruler, its gap and even an
        // ellipsis gives the row back to the message instead.
        chrome.status = "ready";
        let narrow = row(&chrome, Rect::new(0, 20, 7, 2));
        assert!(!narrow.contains("2,4"), "no room for the ruler: {narrow}");
    }

    #[test]
    fn remote_indicator_uses_its_visible_footer_hit_area() {
        UiSettings::default().apply();
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.show_footer = false;
        chrome.remote_control = true;
        chrome.caret = Some((2, 4));
        let area = Rect::new(0, 20, 80, 1);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 80, 21));
        Footer { chrome: &chrome }.render(area, &mut buffer);

        let hit = remote_indicator_rect(status_footer_hits(area).status, true);
        assert_eq!(hit, Rect::new(1, 20, 2, 1));
        let text = |rect: Rect| {
            (rect.x..rect.right())
                .map(|x| buffer.cell((x, rect.y)).expect("cell").symbol())
                .collect::<String>()
        };
        assert_eq!(text(hit), "<>");
        assert_eq!(text(Rect::new(hit.x, hit.y, 3, 1)), "<> ");
        assert!(text(area).starts_with(" <> ready"));
        assert!(!text(area).contains("remote"));
        assert!(text(area).contains("ready"));
        assert!(text(area).contains("2,4"));
        assert!(remote_indicator_rect(area, false).is_empty());
        assert!(remote_indicator_rect(Rect::new(0, 20, 19, 1), true).is_empty());

        let idle = buffer.cell((hit.x, hit.y)).expect("idle indicator").style();
        assert_eq!(idle.fg, Some(theme.muted));
        chrome.remote_receiving = true;
        Footer { chrome: &chrome }.render(area, &mut buffer);
        for (offset, symbol) in ["<", ">"].into_iter().enumerate() {
            let active = buffer
                .cell((hit.x + offset as u16, hit.y))
                .expect("active indicator");
            assert_eq!(active.symbol(), symbol);
            assert_eq!(active.style().fg, Some(theme.ok));
            assert!(active.style().add_modifier.contains(Modifier::BOLD));
        }
        chrome.remote_receiving = false;
        Footer { chrome: &chrome }.render(area, &mut buffer);
        for (offset, symbol) in ["<", ">"].into_iter().enumerate() {
            let restored = buffer
                .cell((hit.x + offset as u16, hit.y))
                .expect("idle indicator");
            assert_eq!(restored.symbol(), symbol);
            assert_eq!(restored.style(), idle);
        }

        chrome.remote_control = false;
        Footer { chrome: &chrome }.render(area, &mut buffer);
        assert!(!text_of(&buffer).contains("<>"));

        // The icon stays at the left while orbit chips remain on the right.
        chrome.show_footer = true;
        chrome.remote_control = true;
        let orbits = [OrbitLevel {
            orbit: 1,
            peak: 0.4,
            pair: 0,
        }];
        chrome.orbits = &orbits;
        let area = Rect::new(0, 20, 150, 2);
        let hits = footer_hits(
            area,
            chrome.device,
            chrome.input,
            chrome.midi_ports,
            0,
            &orbits,
        );
        assert_eq!(hits.orbit_count, 1);
        let hit = remote_indicator_rect(hits.status, true);
        let orbit = hits.orbits[0].1;
        let mut buffer = Buffer::empty(area);
        Footer { chrome: &chrome }.render(area, &mut buffer);
        assert_eq!(hit.x, hits.status.x);
        assert_eq!(buffer.cell((hit.right(), hit.y)).unwrap().symbol(), " ");
        assert!(orbit.x >= hits.status.right());
        assert_eq!(buffer.cell((orbit.x, orbit.y)).unwrap().symbol(), "1");
    }

    #[test]
    fn footer_chips_are_hit_tested_where_they_are_drawn() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let area = Rect::new(0, 20, 150, 2);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 150, 22));
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.device = Some("Scarlett 2i2");
        chrome.midi_ports = midi_ports(2, 1);
        Footer { chrome: &chrome }.render(area, &mut buffer);

        let hits = footer_hits(area, Some("Scarlett 2i2"), None, midi_ports(2, 1), 0, &[]);
        let read = |rect: Rect| {
            (rect.x..rect.right())
                .filter_map(|x| buffer.cell((x, rect.y)))
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        assert!(read(hits.device_chip).contains("Scarlett 2i2"));
        assert!(read(hits.midi_chip).contains("2 out · 1 in"));
        assert!(!hits.device_chip.intersects(hits.midi_chip));
        // The dock's fader and readouts are drawn where the hits say. The
        // cap is ground rather than a glyph, so it is one cell of the
        // meter row whose background nothing else on that row shares.
        let bg = |x: u16, y: u16| buffer.cell((x, y)).unwrap().style().bg;
        let row = hits.meter.y;
        assert_eq!(
            (hits.meter.x..hits.meter.right())
                .filter(|x| {
                    let here = bg(*x, row);
                    (hits.meter.x..hits.meter.right())
                        .filter(|other| bg(*other, row) == here)
                        .count()
                        == 1
                })
                .count(),
            1,
            "the fader's cap sits on the meter the hits name"
        );
        let text = text_of(&buffer);
        assert!(text.contains("MASTER"), "{text}");
        assert!(text.contains("0.0dB"), "{text}");
    }

    #[test]
    fn footer_input_level_fills_device_characters_from_left_to_right() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let area = Rect::new(0, 20, 150, 2);
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.device = Some("Speakers");
        chrome.input = Some("Mic界");
        chrome.midi_ports = midi_ports(2, 0);
        chrome.input_active = true;
        let hits = footer_hits(area, chrome.device, chrome.input, midi_ports(2, 0), 0, &[]);
        let lit_color = legible_against_floor(theme.ok, theme.surface, 3.0);
        assert_ne!(lit_color, theme.accent);
        assert_eq!(
            usize::from(hits.device_chip.width),
            UnicodeWidthStr::width(device_chip_text(chrome.device, chrome.input).as_str()),
        );

        let mut previous = 0;
        for peak_db in [-60.0, -30.0, -10.0, 0.0] {
            chrome.input_peak_db = peak_db;
            let mut buffer = Buffer::empty(Rect::new(0, 0, 150, 22));
            Footer { chrome: &chrome }.render(area, &mut buffer);
            let colors = (hits.device_chip.x..hits.device_chip.right())
                .map(|x| buffer.cell((x, hits.device_chip.y)).unwrap().fg)
                .collect::<Vec<_>>();
            let lit = colors
                .iter()
                .take_while(|color| **color == lit_color)
                .count();
            assert!(
                lit > previous,
                "{peak_db} dB should fill farther than the prior level"
            );
            assert!(colors[..lit].iter().all(|color| *color == lit_color));
            assert!(
                colors[lit..].iter().all(|color| *color == theme.accent),
                "{peak_db} dB, lit={lit}, colors={colors:?}, accent={:?}",
                theme.accent
            );
            assert_eq!(
                buffer
                    .cell((hits.midi_chip.x, hits.midi_chip.y))
                    .unwrap()
                    .fg,
                theme.accent,
                "MIDI keeps its own activity color",
            );
            let text = (hits.device_chip.x..hits.device_chip.right())
                .map(|x| buffer.cell((x, hits.device_chip.y)).unwrap().symbol())
                .collect::<String>();
            assert!(text.contains("Speakers · ♩ Mic界"), "{text}");
            previous = lit;
        }
        assert_eq!(previous, usize::from(hits.device_chip.width));

        chrome.input_active = false;
        chrome.input_peak_db = 0.0;
        let mut buffer = Buffer::empty(Rect::new(0, 0, 150, 22));
        Footer { chrome: &chrome }.render(area, &mut buffer);
        assert!(
            (hits.device_chip.x..hits.device_chip.right()).all(|x| buffer
                .cell((x, hits.device_chip.y))
                .unwrap()
                .fg
                == theme.accent),
            "an idle microphone leaves the label unfilled",
        );

        chrome.input = None;
        chrome.input_active = true;
        let disconnected = footer_hits(area, chrome.device, None, midi_ports(2, 0), 0, &[]);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 150, 22));
        Footer { chrome: &chrome }.render(area, &mut buffer);
        assert!(
            (disconnected.device_chip.x..disconnected.device_chip.right()).all(|x| buffer
                .cell((x, disconnected.device_chip.y))
                .unwrap()
                .fg
                == theme.accent),
            "a removed microphone cannot leave a stale fill",
        );
    }

    #[test]
    fn the_header_says_whether_the_scene_is_good_to_go() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let area = Rect::new(0, 0, 140, 1);
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.cps = Some(0.5);
        for (go, expected, color) in [
            (GoState::Ready, "✓ ready", theme.ok),
            (
                GoState::Loading { ready: 3, total: 7 },
                "◐ loading 3/7",
                theme.warn,
            ),
            (GoState::Problems { count: 2 }, "✗ 2 problems", theme.error),
            (
                GoState::Failed { failed: 1 },
                "⚠ 1 sound failed",
                theme.error,
            ),
        ] {
            chrome.go = go;
            let mut buffer = Buffer::empty(area);
            Header { chrome: &chrome }.render(area, &mut buffer);
            let text = text_of(&buffer);
            assert!(text.contains(expected), "{text}");
            assert!(text.contains("120.0 bpm"), "tempo still follows: {text}");
            let at = text.find(expected).unwrap();
            let cell = text[..at].chars().count() as u16;
            assert_eq!(buffer.cell((cell, 0)).unwrap().fg, color);
        }
    }

    #[cfg(feature = "hydra")]
    #[test]
    fn the_header_keeps_camera_acquisition_visible_outside_settings() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let area = Rect::new(0, 0, 100, 1);
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        for (state, expected) in [
            (rustel_runtime::hydra::HydraWebcamState::Blocked, "CAM OFF"),
            (rustel_runtime::hydra::HydraWebcamState::Opening, "CAM OPEN"),
            (rustel_runtime::hydra::HydraWebcamState::Ready, "CAM ●"),
            (rustel_runtime::hydra::HydraWebcamState::Error, "CAM !"),
        ] {
            chrome.webcam = Some(state);
            let mut buffer = Buffer::empty(area);
            Header { chrome: &chrome }.render(area, &mut buffer);
            assert!(text_of(&buffer).contains(expected), "{}", text_of(&buffer));
        }
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

    /// The countdown is the one header chip the eye waits on; it is drawn
    /// in the warning colour, keeps its number under any width, and goes
    /// out with a stop.
    #[test]
    fn the_header_counts_a_launch_down_and_keeps_its_number() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.cps = Some(0.5);
        chrome.launch = Some(("intro", 3.2));
        let area = Rect::new(0, 0, 140, 1);
        let mut buffer = Buffer::empty(area);
        Header { chrome: &chrome }.render(area, &mut buffer);
        let text = text_of(&buffer);
        assert!(text.contains("» intro in 3.2"), "{text}");
        let at = text.find("» intro").unwrap();
        let cell = text[..at].chars().count() as u16;
        assert_eq!(buffer.cell((cell, 0)).unwrap().fg, theme.warn);

        chrome.stopping = true;
        let mut buffer = Buffer::empty(area);
        Header { chrome: &chrome }.render(area, &mut buffer);
        assert!(
            !text_of(&buffer).contains("» intro"),
            "a stop ends the countdown"
        );

        assert_eq!(launch_chip("intro", 3.2, 40), "» intro in 3.2");
        assert_eq!(
            launch_chip("ambient-intro-take-two", 3.2, 16),
            "» ambien… in 3.2",
            "the name gives way, the number stays"
        );
        assert_eq!(launch_chip("intro", 0.4, 7), "» in 0.4");
    }

    #[test]
    fn the_header_prioritizes_engine_pressure_with_exact_values_and_severity() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let area = Rect::new(0, 0, 160, 1);
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        let mut pressure = EnginePressureSnapshot {
            level: EnginePressureLevel::Warning,
            ..EnginePressureSnapshot::default()
        };
        pressure.device.realtime_load.slow_load_basis_points = 2_400;
        pressure.device.realtime_pressure.active_voices = 18;
        pressure.producer.slow_load_basis_points = 1_100;
        pressure.producer.cover_end_nanos = 420_000_000;
        chrome.pressure = Some(&pressure);
        // Pressure only draws at Full - the deadline detail a tuning
        // session wants, not what a glance mid-set gets by default.
        chrome.metric_detail = settings::MetricDetail::Full;
        chrome.stats = ProcessStats {
            cpu_percent: Some(99.0),
            machine_cpu_percent: Some(80.0),
            resident_bytes: Some(3 << 30),
            footprint_bytes: None,
        };

        let mut buffer = Buffer::empty(area);
        Header { chrome: &chrome }.render(area, &mut buffer);
        let text = text_of(&buffer);
        let expected = "DSP 24%  sched 11%  voices 18/128  cover 420ms";
        assert!(text.contains(expected), "{text}");
        let at = text.find(expected).unwrap();
        let cell = text[..at].chars().count() as u16;
        assert_eq!(buffer.cell((cell, 0)).unwrap().fg, theme.warn);
    }

    #[test]
    fn the_header_shows_a_take_in_red_with_its_clock_and_size() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let area = Rect::new(0, 0, 160, 1);
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.cps = Some(0.5);
        chrome.go = GoState::Ready;
        chrome.recording = Some(RecordingChip {
            seconds: 754.0,
            bytes: 140 << 20,
            dropped_seconds: 0.0,
            sample: false,
        });
        let mut buffer = Buffer::empty(area);
        Header { chrome: &chrome }.render(area, &mut buffer);
        let text = text_of(&buffer);
        assert!(text.contains("✓ ready"), "{text}");
        assert!(text.contains("● REC 12:34 · 140 MB"), "{text}");
        assert!(text.contains("120.0 bpm"), "{text}");
        let at = text.find("● REC").unwrap();
        let cell = text[..at].chars().count() as u16;
        assert_eq!(buffer.cell((cell, 0)).unwrap().fg, theme.error);
        assert_eq!(
            RecordingChip {
                seconds: 3725.0,
                bytes: 3 << 30,
                dropped_seconds: 0.4,
                sample: false,
            }
            .text(),
            "● REC 1:02:05 · 3.0 GB · dropped 0.4s"
        );
        assert_eq!(
            RecordingChip {
                seconds: 4.0,
                bytes: 0,
                dropped_seconds: 0.0,
                sample: true,
            }
            .text(),
            "● REC SAMPLE 0:04",
            "a sample says so, in the same red"
        );
        // A take that ended by itself is said in the same place, muted -
        // never in the error colour on a screen an audience may see.
        chrome.recording = None;
        chrome.take_notice = Some("take ended · disk full");
        chrome.unseen_warnings = 3;
        let mut buffer = Buffer::empty(area);
        Header { chrome: &chrome }.render(area, &mut buffer);
        let text = text_of(&buffer);
        assert!(text.contains("○ take ended · disk full"), "{text}");
        assert!(text.contains("⚠ 3"), "{text}");
        chrome.unseen_warnings = 0;
        let mut peaks = [0.0f32; super::super::export::SCOPE_COLUMNS];
        peaks[15] = 1.0;
        peaks[14] = 0.25;
        let glance = ExportGlance {
            scene_name: "drums".into(),
            seconds: 42.0,
            percent: Some(42),
            peaks,
        };
        chrome.exporting = Some(&glance);
        let mut buffer = Buffer::empty(area);
        Header { chrome: &chrome }.render(area, &mut buffer);
        let text = text_of(&buffer);
        assert!(text.contains("⇣ drums 42% ▁▁▁▁▁▁▁▁▁▁▁▁▁▁▅█"), "{text}");
        let at = text.find("○ take").unwrap();
        let cell = text[..at].chars().count() as u16;
        assert_eq!(buffer.cell((cell, 0)).unwrap().fg, theme.muted);
    }

    #[test]
    fn orbit_chips_take_the_right_of_the_status_row_and_yield_to_a_narrow_footer() {
        let orbits = [
            OrbitLevel {
                orbit: 0,
                peak: 0.9,
                pair: 0,
            },
            OrbitLevel {
                orbit: 1,
                peak: 0.1,
                pair: 1,
            },
            OrbitLevel {
                orbit: 5,
                peak: 0.0,
                pair: 0,
            },
        ];
        let area = Rect::new(0, 20, 160, 2);
        let hits = footer_hits(area, Some("Scarlett"), None, midi_ports(0, 0), 0, &orbits);
        assert_eq!(hits.orbit_count, 3);
        let plain = footer_hits(area, Some("Scarlett"), None, midi_ports(0, 0), 0, &[]);
        assert!(
            hits.status.width < plain.status.width,
            "the status yields room"
        );
        assert!(hits.orbits[2].1.right() <= plain.status.right());
        assert_eq!(hits.orbits[0].0, 0);
        assert_eq!(hits.orbits[2].0, 5);
        // Too narrow for any chip: the status keeps its third.
        let narrow = footer_hits(
            Rect::new(0, 20, 60, 2),
            None,
            None,
            midi_ports(0, 0),
            0,
            &orbits,
        );
        assert_eq!(narrow.orbit_count, 0);
        // Rendered: number, meter, pair.
        let theme = theme();
        let mut buffer = Buffer::empty(area);
        render_orbit_chip(&mut buffer, hits.orbits[0].1, &orbits[0], 2, &theme);
        render_orbit_chip(&mut buffer, hits.orbits[1].1, &orbits[1], 2, &theme);
        let text = text_of(&buffer);
        assert!(text.contains("0 ▮▮▮▮ 1/2"), "{text}");
        assert!(text.contains("1 ▮▮▯▯ 3/4"), "{text}");
    }

    #[test]
    fn the_dot_beats_with_the_cycle() {
        let theme = theme();
        assert_eq!(beat_pulse(false, Some(2.0), &theme), ("◉", theme.muted));
        let (on_downbeat, color) = beat_pulse(true, Some(2.0), &theme);
        assert_eq!(on_downbeat, "●");
        assert_eq!(color, theme.ok);
        let (on_beat, color) = beat_pulse(true, Some(2.25), &theme);
        assert_eq!(on_beat, "●");
        assert_eq!(color, theme.accent);
        let (between, faded) = beat_pulse(true, Some(2.6), &theme);
        assert_eq!(between, "◉");
        assert_ne!(faded, theme.accent, "faded towards the muted colour");
    }

    #[test]
    fn the_dot_uses_safe_frames_when_symbols_are_unavailable() {
        let _symbols = crate::terminal::ForceSymbolsForTest::set(false);
        let theme = theme();
        assert_eq!(beat_pulse(false, Some(2.0), &theme).0, "o");
        assert_eq!(beat_pulse(true, Some(2.0), &theme).0, "\u{b7}");
        assert_eq!(beat_pulse(true, Some(2.6), &theme).0, "o");
    }

    #[test]
    fn the_header_reports_tempo_and_process_counters() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.stats = ProcessStats {
            cpu_percent: Some(12.0),
            machine_cpu_percent: Some(40.0),
            resident_bytes: Some(84 * 1024 * 1024),
            footprint_bytes: None,
        };
        chrome.cps = Some(0.5);
        chrome.cycle = Some(4.0);

        let header_area = Rect::new(0, 0, 140, 1);
        let mut header_buffer = Buffer::empty(header_area);
        Header { chrome: &chrome }.render(header_area, &mut header_buffer);
        let header_text = text_of(&header_buffer);
        assert!(header_text.contains("120.0 bpm"), "{header_text}");
        assert!(header_text.contains("0.50 cps"), "{header_text}");
        assert!(header_text.contains("cpu 12%"), "{header_text}");
        assert!(header_text.contains("mem 84.0MB"), "{header_text}");
        assert!(header_text.contains("live"), "{header_text}");
        // Advanced is the default, so the machine's own share stands
        // beside rustel's: a player watching a low number while the
        // laptop underneath them is pinned has nothing on the screen
        // telling them so.
        assert!(header_text.contains("sys "), "{header_text}");
    }

    /// The header's memory is the footprint, which is what the process
    /// owns, and not its resident set. Where there is no footprint, the
    /// resident set stands in.
    #[test]
    fn the_header_shows_the_memory_footprint_over_the_resident_set() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        let area = Rect::new(0, 0, 140, 1);
        let header = |chrome: &StudioChrome<'_>| {
            let mut buffer = Buffer::empty(area);
            Header { chrome }.render(area, &mut buffer);
            text_of(&buffer)
        };

        chrome.stats = ProcessStats {
            cpu_percent: Some(12.0),
            machine_cpu_percent: None,
            resident_bytes: Some(300 * 1024 * 1024),
            footprint_bytes: Some(120 * 1024 * 1024),
        };
        let text = header(&chrome);
        assert!(text.contains("mem 120MB"), "{text}");
        assert!(!text.contains("300MB"), "{text}");

        chrome.stats.footprint_bytes = None;
        let text = header(&chrome);
        assert!(text.contains("mem 300MB"), "{text}");
    }

    /// The four levels add detail in strict order: nothing, rustel's own
    /// figures, the machine's beside them, then the engine's deadline
    /// pressure too. Each level shows all that the level below it shows.
    #[test]
    fn the_metric_detail_setting_shows_a_ladder_of_header_counters() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.stats = ProcessStats {
            cpu_percent: Some(12.0),
            machine_cpu_percent: Some(55.0),
            resident_bytes: Some(84 * 1024 * 1024),
            footprint_bytes: None,
        };
        let mut pressure = EnginePressureSnapshot {
            level: EnginePressureLevel::Warning,
            ..EnginePressureSnapshot::default()
        };
        pressure.device.realtime_load.slow_load_basis_points = 2_400;
        chrome.pressure = Some(&pressure);
        let area = Rect::new(0, 0, 160, 1);

        chrome.metric_detail = settings::MetricDetail::None;
        let mut buffer = Buffer::empty(area);
        Header { chrome: &chrome }.render(area, &mut buffer);
        let text = text_of(&buffer);
        assert!(!text.contains("cpu 12%"), "{text}");
        assert!(!text.contains("mem 84"), "{text}");
        assert!(!text.contains("sys 55%"), "{text}");
        assert!(!text.contains("DSP"), "{text}");

        chrome.metric_detail = settings::MetricDetail::Basic;
        let mut buffer = Buffer::empty(area);
        Header { chrome: &chrome }.render(area, &mut buffer);
        let text = text_of(&buffer);
        assert!(text.contains("cpu 12%"), "{text}");
        assert!(text.contains("mem 84"), "{text}");
        assert!(!text.contains("sys 55%"), "{text}");
        assert!(!text.contains("DSP"), "{text}");

        chrome.metric_detail = settings::MetricDetail::Advanced;
        let mut buffer = Buffer::empty(area);
        Header { chrome: &chrome }.render(area, &mut buffer);
        let text = text_of(&buffer);
        assert!(text.contains("cpu 12%"), "{text}");
        assert!(text.contains("sys 55%"), "{text}");
        assert!(!text.contains("DSP"), "{text}");

        chrome.metric_detail = settings::MetricDetail::Full;
        let mut buffer = Buffer::empty(area);
        Header { chrome: &chrome }.render(area, &mut buffer);
        let text = text_of(&buffer);
        assert!(text.contains("cpu 12%"), "{text}");
        assert!(text.contains("sys 55%"), "{text}");
        assert!(text.contains("DSP 24%"), "{text}");
    }

    /// The counters are the memory breakdown's button, and the rect the header
    /// records is exactly the run it drew: `cpu … · mem … · fps`, whole.
    #[test]
    fn memory_chip_is_recorded_where_counters_paint() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.metric_detail = settings::MetricDetail::Basic;
        chrome.stats.resident_bytes = Some(84 * 1024 * 1024);
        let area = Rect::new(0, 1, 160, 1);
        let mut buffer = Buffer::empty(area);
        super::super::memory::set_memory_chip(None);

        Header { chrome: &chrome }.render(area, &mut buffer);

        let chip = super::super::memory::memory_chip().expect("the counters drew");
        assert_eq!(chip.y, 1);
        let drawn = (chip.x..chip.right())
            .map(|x| buffer[(x, 1)].symbol())
            .collect::<String>();
        assert!(drawn.starts_with("cpu "), "{drawn}");
        assert!(drawn.contains("mem 84.0MB"), "{drawn}");
        assert!(drawn.ends_with("ms"), "the whole run and no more: {drawn}");
        assert!(super::super::memory::memory_chip_at(chip.x, 1));
        assert!(!super::super::memory::memory_chip_at(chip.right(), 1));
    }

    /// A header that draws no counters leaves no button behind: at metric
    /// detail "none", and on a header too narrow for them. (Zen, with no
    /// header at all, is the app's frame to paint; its test is there.)
    #[test]
    fn memory_chip_is_cleared_at_metric_detail_none_and_narrow() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        let wide = Rect::new(0, 1, 160, 1);
        let chip_after = |chrome: &StudioChrome<'_>, area: Rect| {
            super::super::memory::set_memory_chip(Some((1, 1, 30)));
            Header { chrome }.render(area, &mut Buffer::empty(area));
            super::super::memory::memory_chip()
        };

        chrome.metric_detail = settings::MetricDetail::Basic;
        assert!(chip_after(&chrome, wide).is_some());
        chrome.metric_detail = settings::MetricDetail::None;
        assert_eq!(chip_after(&chrome, wide), None);
        chrome.metric_detail = settings::MetricDetail::Basic;
        assert_eq!(chip_after(&chrome, Rect::new(0, 1, 99, 1)), None);
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
                        prebake_rows: [PrebakeRow::default(); 2],
                        sets_folder: String::new(),
                        recordings_folder: String::new(),
                        set_limiter: None,
                        sample_cache: String::new(),
                        precache: None,
                        sources: Vec::new(),
                        mappings: Default::default(),
                        keybind_rows: Vec::new(),
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
                            flash: true,
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
                        prebake_rows: [PrebakeRow::default(); 2],
                        sets_folder: String::new(),
                        recordings_folder: String::new(),
                        set_limiter: None,
                        sample_cache: String::new(),
                        precache: None,
                        sources: Vec::new(),
                        mappings: Default::default(),
                        keybind_rows: Vec::new(),
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
                            flash: false,
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
                        prebake_rows: [PrebakeRow::default(); 2],
                        sets_folder: String::new(),
                        recordings_folder: String::new(),
                        set_limiter: None,
                        sample_cache: String::new(),
                        precache: None,
                        sources: Vec::new(),
                        mappings: Default::default(),
                        keybind_rows: Vec::new(),
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
                            flash: false,
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
                        prebake_rows: [PrebakeRow::default(); 2],
                        sets_folder: String::new(),
                        recordings_folder: String::new(),
                        set_limiter: None,
                        sample_cache: String::new(),
                        precache: None,
                        sources: Vec::new(),
                        mappings: Default::default(),
                        keybind_rows: Vec::new(),
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
                                flash: false,
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
                                flash: false,
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
                        reference: Some((&reference, &panel)),
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
        assert!(titles.contains("F10 switches"), "{titles}");
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

    #[test]
    fn minimap_viewport_uses_wrapped_and_inline_screen_rows() {
        let mut editor = Editor::new(&"abcdefgh".repeat(20)).expect("editor");
        editor.set_view_size(9, 4);
        editor.set_wrap(true);
        editor.set_wrap_indents(vec![0]);
        let mut viewport = editor.viewport();
        viewport.top_row = 4;
        editor.set_viewport(viewport);
        let map = editor.screen_map(GridRect::new(0, 0, 9, 4)).expect("map");
        assert_eq!(visible_rows(&map), (4, 7));

        editor.set_wrap(false);
        editor
            .set_virtual_rows(
                editor.revision(),
                vec![VirtualRowSpec::new("plot", ByteOffset(0), 8)],
            )
            .expect("inline block");
        let mut viewport = editor.viewport();
        viewport.top_row = 4;
        editor.set_viewport(viewport);
        let map = editor.screen_map(GridRect::new(0, 0, 9, 4)).expect("map");
        assert!(
            map.rows()
                .iter()
                .all(|row| matches!(row, ScreenRow::Virtual(_)))
        );
        assert_eq!(visible_rows(&map), (4, 7));
    }
}
#[cfg(test)]
mod visual_backdrop_tests {
    use super::*;

    /// A picture at a given strength, with the interface left solid - the
    /// case most of these tests are about.
    fn picture(pixels: &[[u8; 3]], width: u16, height: u16, strength: f32) -> VisualBackdrop {
        let mut rgba = Vec::with_capacity(pixels.len() * 4);
        for pixel in pixels {
            rgba.extend_from_slice(pixel);
            rgba.push(255);
        }
        VisualBackdrop {
            width,
            height,
            rgba,
            strength,
            interface: 0.0,
        }
    }

    /// Every cell takes its colour from the picture, scaled to the grid.
    #[test]
    fn the_picture_becomes_the_colour_of_every_cell() {
        let theme = Theme::built_in_default();
        // Two pixels: red on the left, green on the right.
        let hydra = picture(&[[255, 0, 0], [0, 255, 0]], 2, 1, 1.0);
        let area = Rect::new(0, 0, 4, 2);
        let mut buffer = Buffer::empty(area);
        Backdrop { theme: &theme }.render(area, &mut buffer);
        hydra.paint_excluding(&mut buffer, area, theme.background, &[], &[], None);

        for row in 0..2 {
            assert_eq!(
                buffer.cell((0, row)).unwrap().bg,
                Color::Rgb(255, 0, 0),
                "the left half is the left pixel"
            );
            assert_eq!(
                buffer.cell((3, row)).unwrap().bg,
                Color::Rgb(0, 255, 0),
                "the right half is the right pixel"
            );
        }
    }

    /// At half strength the picture is mixed into the theme's own background,
    /// which is what keeps the code on top of it readable.
    #[test]
    fn strength_mixes_the_picture_into_the_theme() {
        let theme = Theme::built_in_default();
        let (base_r, _, _) = super::super::graphics::rgb(theme.background);
        let hydra = picture(&[[255, 255, 255]], 1, 1, 0.5);
        let area = Rect::new(0, 0, 1, 1);
        let mut buffer = Buffer::empty(area);
        Backdrop { theme: &theme }.render(area, &mut buffer);
        hydra.paint_excluding(&mut buffer, area, theme.background, &[], &[], None);
        let Color::Rgb(red, _, _) = buffer.cell((0, 0)).unwrap().bg else {
            panic!("a mixed background is an explicit colour");
        };
        let expected = (f32::from(base_r) * 0.5 + 255.0 * 0.5) as u8;
        assert_eq!(red, expected);
    }

    /// A selected cell takes the picture over its own colour, at the wash
    /// the caller says - on both delivery paths identically.
    #[test]
    fn a_named_selection_cell_takes_the_picture_at_its_own_wash() {
        let theme = Theme::built_in_default();
        let area = Rect::new(0, 0, 2, 1);
        let hydra = VisualBackdrop {
            width: 2,
            height: 1,
            rgba: vec![255; 8],
            strength: 1.0,
            interface: 0.0,
        };
        let selection_colour = Color::Rgb(40, 0, 80);
        let mut cells = std::collections::HashSet::new();
        cells.insert((1u16, 0u16));
        let untouched = std::collections::HashSet::new();
        let wash = SelectionWash {
            cells: &cells,
            strength: 0.5,
            untouched: &untouched,
        };

        let mut buffer = Buffer::empty(area);
        Backdrop { theme: &theme }.render(area, &mut buffer);
        buffer.cell_mut((1, 0)).unwrap().set_bg(selection_colour);
        let image = hydra.composited_excluding(
            theme.background,
            Some((&buffer, area)),
            &[],
            &[],
            Some(&wash),
        );
        hydra.paint_excluding(&mut buffer, area, theme.background, &[], &[], Some(&wash));

        // The named cell: half the picture over its own colour, 40 + 215/2.
        let expected = Color::Rgb(147, 127, 167);
        assert_eq!(buffer.cell((1, 0)).unwrap().bg, expected);
        assert_eq!(&image[4..8], &[147, 127, 167, 255]);
        // An unnamed interface cell would have been cut out entirely at
        // interface 0 - the wash names its cells, colour never does.
        assert_eq!(buffer.cell((0, 0)).unwrap().bg, Color::Rgb(255, 255, 255));
    }

    /// The theme decides how much a selection can take, and it is the
    /// theme's contrast that decides it.
    #[test]
    fn the_selection_wash_budget_follows_the_themes_contrast() {
        use crate::theme::selection_wash;
        let generous = Theme::built_in_default();
        let budget = selection_wash(&generous);
        assert!((0.05..=1.0).contains(&budget), "a real budget: {budget}");

        // A theme whose selection barely clears its text affords less than
        // one with headroom.
        let mut tight = generous.clone();
        tight.selection = Color::Rgb(90, 90, 90);
        tight.selection_text = Color::Rgb(190, 190, 190);
        assert!(
            selection_wash(&tight) < budget,
            "{} < {budget}",
            selection_wash(&tight)
        );

        // Colours the theme hands to the terminal cannot be measured, and
        // take the conservative fixed budget rather than a made-up one.
        let mut reset = generous.clone();
        reset.selection = Color::Reset;
        assert_eq!(selection_wash(&reset), 0.2);
    }

    /// Hydra's `luma`, `mask` and `layer` transforms produce premultiplied alpha.
    /// Both delivery paths preserve transparency so the theme shows through.
    #[test]
    fn the_pictures_own_alpha_lets_the_theme_through() {
        let theme = Theme::built_in_default();
        let under = super::super::graphics::rgb(theme.background);
        // Two pixels, both premultiplied: one solid white, one fully absent.
        // An absent pixel carries no colour of its own - that is what
        // premultiplied means - so the cell must come out as the theme's.
        let hydra = VisualBackdrop {
            width: 2,
            height: 1,
            rgba: vec![255, 255, 255, 255, 0, 0, 0, 0],
            strength: 1.0,
            interface: 0.0,
        };
        let area = Rect::new(0, 0, 2, 1);
        let mut buffer = Buffer::empty(area);
        Backdrop { theme: &theme }.render(area, &mut buffer);
        hydra.paint_excluding(&mut buffer, area, theme.background, &[], &[], None);
        assert_eq!(buffer.cell((0, 0)).unwrap().bg, Color::Rgb(255, 255, 255));
        assert_eq!(
            buffer.cell((1, 0)).unwrap().bg,
            Color::Rgb(under.0, under.1, under.2),
            "a transparent pixel is the theme, not black"
        );
    }

    /// Cell backgrounds and terminal images use the same compositing rules.
    /// Both delivery paths must produce the same colour.
    #[test]
    fn both_delivery_paths_composite_to_the_same_colour() {
        let theme = Theme::built_in_default();
        for alpha in [0_u8, 64, 128, 255] {
            for opacity in [0.05_f32, 0.55, 1.0] {
                // Premultiplied, so a half-covered pixel carries half its
                // colour, exactly as hydra's own shaders emit it.
                let level = (200.0 * f32::from(alpha) / 255.0) as u8;
                let hydra = VisualBackdrop {
                    width: 1,
                    height: 1,
                    rgba: vec![level, level, level, alpha],
                    strength: opacity,
                    interface: 0.0,
                };
                let area = Rect::new(0, 0, 1, 1);
                let mut buffer = Buffer::empty(area);
                Backdrop { theme: &theme }.render(area, &mut buffer);
                hydra.paint_excluding(&mut buffer, area, theme.background, &[], &[], None);
                let cells = buffer.cell((0, 0)).unwrap().bg;

                let image = hydra.composited_excluding(theme.background, None, &[], &[], None);
                assert_eq!(image.len(), 4, "one pixel in, one pixel out");
                assert_eq!(image[3], 255, "the image goes out opaque");
                assert_eq!(
                    cells,
                    Color::Rgb(image[0], image[1], image[2]),
                    "alpha {alpha} at opacity {opacity}"
                );
            }
        }
        // At zero the picture is not drawn at all rather than drawn as the
        // background, which is what turning the setting down to 0% has to
        // mean. The two paths still agree: one leaves the cell alone, the
        // other leaves a hole for it to show through.
        let hydra = VisualBackdrop {
            width: 1,
            height: 1,
            rgba: vec![255, 255, 255, 255],
            strength: 0.0,
            interface: 0.0,
        };
        let area = Rect::new(0, 0, 1, 1);
        let mut buffer = Buffer::empty(area);
        Backdrop { theme: &theme }.render(area, &mut buffer);
        hydra.paint_excluding(&mut buffer, area, theme.background, &[], &[], None);
        assert_eq!(
            buffer.cell((0, 0)).unwrap().bg,
            theme.background,
            "the cell is left as the theme painted it"
        );
        assert_eq!(
            hydra.composited_excluding(theme.background, None, &[], &[], None),
            vec![0, 0, 0, 0]
        );
    }

    /// A reset-coloured chrome inside a declared rect takes the interface
    /// wash, not the picture's full strength, so reset-coloured themes
    /// do not pulse with the sketch across the whole frame.
    #[test]
    fn a_reset_chrome_rect_takes_the_interface_wash_not_the_full_picture() {
        let theme = Theme::built_in_default();
        let area = Rect::new(0, 0, 2, 1);
        let hydra = VisualBackdrop {
            width: 2,
            height: 1,
            rgba: vec![255; 8],
            strength: 1.0,
            interface: 0.0,
        };
        let mut buffer = Buffer::empty(area);
        // Both cells use `reset`; only the left one is declared chrome.
        for x in 0..2 {
            buffer.cell_mut((x, 0)).unwrap().set_bg(Color::Reset);
        }
        let header = Rect::new(0, 0, 1, 1);
        hydra.paint_excluding(&mut buffer, area, theme.background, &[], &[header], None);
        assert_eq!(
            buffer.cell((0, 0)).unwrap().bg,
            Color::Reset,
            "chrome at a solid interface keeps its colour"
        );
        assert_ne!(
            buffer.cell((1, 0)).unwrap().bg,
            Color::Reset,
            "the ground outside the rect still takes the picture"
        );
    }

    /// With the interface solid, it is cut out of the image rather than
    /// painted over.
    ///
    /// A terminal draws an image under the glyphs but still over the cell
    /// backgrounds, so an image covering the grid hides every panel, border
    /// and highlight and leaves the glyphs floating on it. Cutting those cells
    /// out is what gives the picture back its place behind the interface
    /// rather than in front of it.
    #[test]
    fn a_solid_interface_is_cut_out_of_the_image() {
        let theme = Theme::built_in_default();
        // Two cells wide, one deep, two picture pixels per cell.
        let area = Rect::new(0, 0, 2, 1);
        let hydra = VisualBackdrop {
            width: 4,
            height: 1,
            rgba: vec![255; 16],
            strength: 1.0,
            interface: 0.0,
        };
        let mut buffer = Buffer::empty(area);
        Backdrop { theme: &theme }.render(area, &mut buffer);
        // The right-hand cell claims a colour, the way a highlight does.
        buffer.cell_mut((1, 0)).unwrap().set_bg(Color::Rgb(9, 9, 9));

        let image =
            hydra.composited_excluding(theme.background, Some((&buffer, area)), &[], &[], None);
        assert_eq!(image.len(), 16, "four pixels out");
        assert_eq!(
            &image[0..8],
            &[255, 255, 255, 255, 255, 255, 255, 255],
            "the cell that kept the theme takes the picture"
        );
        assert_eq!(
            &image[8..16],
            &[0, 0, 0, 0, 0, 0, 0, 0],
            "the cell that chose a colour is a hole, so its colour survives"
        );

        // With no frame to consult, nothing is cut out.
        let whole = hydra.composited_excluding(theme.background, None, &[], &[], None);
        assert!(whole.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 255));
    }

    #[test]
    fn a_kept_preview_is_untouched_in_cell_and_pixel_delivery() {
        let theme = Theme::built_in_default();
        let area = Rect::new(0, 0, 2, 1);
        let keep = Rect::new(1, 0, 1, 1);
        let picture = VisualBackdrop {
            width: 2,
            height: 1,
            rgba: vec![255; 8],
            strength: 1.0,
            interface: 1.0,
        };
        let mut buffer = Buffer::empty(area);
        Backdrop { theme: &theme }.render(area, &mut buffer);
        let before = buffer.cell((1, 0)).unwrap().clone();
        let image = picture.composited_excluding(
            theme.background,
            Some((&buffer, area)),
            &[keep],
            &[],
            None,
        );

        picture.paint_excluding(&mut buffer, area, theme.background, &[keep], &[], None);

        assert_eq!(buffer.cell((1, 0)), Some(&before));
        assert_eq!(&image[4..8], &[0, 0, 0, 0]);
        assert_eq!(&image[0..4], &[255, 255, 255, 255]);
    }

    /// A translucent interface blends the picture over each cell's own colour.
    /// Cell and image delivery must produce the same result.
    #[test]
    fn a_translucent_interface_takes_the_picture_over_its_own_colour() {
        let theme = Theme::built_in_default();
        let chrome = Color::Rgb(40, 0, 80);
        let area = Rect::new(0, 0, 2, 1);
        // Two cells, two picture pixels each, the picture solid white.
        let hydra = VisualBackdrop {
            width: 4,
            height: 1,
            rgba: vec![255; 16],
            strength: 1.0,
            interface: 0.5,
        };
        let mut buffer = Buffer::empty(area);
        Backdrop { theme: &theme }.render(area, &mut buffer);
        buffer.cell_mut((1, 0)).unwrap().set_bg(chrome);

        let image =
            hydra.composited_excluding(theme.background, Some((&buffer, area)), &[], &[], None);
        hydra.paint_excluding(&mut buffer, area, theme.background, &[], &[], None);

        // The score's own background takes all of the picture.
        assert_eq!(buffer.cell((0, 0)).unwrap().bg, Color::Rgb(255, 255, 255));
        assert_eq!(&image[0..4], &[255, 255, 255, 255]);

        // The interface cell takes half of it, over its own colour rather
        // than over the theme's - 40 + (255 - 40) / 2 in the red channel.
        let expected = Color::Rgb(147, 127, 167);
        assert_eq!(buffer.cell((1, 0)).unwrap().bg, expected);
        assert_eq!(
            &image[8..12],
            &[147, 127, 167, 255],
            "and the image says the same thing"
        );
    }

    /// Explicit cell backgrounds remain unchanged. Active-line and event
    /// highlights must not change colour as the picture animates.
    #[test]
    fn a_cell_that_chose_its_colour_keeps_it() {
        let theme = Theme::built_in_default();
        let hydra = picture(&[[255, 0, 0]], 1, 1, 1.0);
        let area = Rect::new(0, 0, 2, 1);
        let mut buffer = Buffer::empty(area);
        Backdrop { theme: &theme }.render(area, &mut buffer);
        // Something drew a highlight over the second cell.
        buffer.cell_mut((1, 0)).unwrap().set_bg(Color::Rgb(0, 0, 0));
        hydra.paint_excluding(&mut buffer, area, theme.background, &[], &[], None);

        assert_eq!(
            buffer.cell((0, 0)).unwrap().bg,
            Color::Rgb(255, 0, 0),
            "plain background is the picture"
        );
        // The highlight keeps its own colour exactly. The picture moves
        // every frame, so a highlight that took part of it would never
        // settle.
        assert_eq!(
            buffer.cell((1, 0)).unwrap().bg,
            Color::Rgb(0, 0, 0),
            "a chosen background survives the picture untouched"
        );
    }

    /// Without a picture, the backdrop leaves the theme unchanged.
    #[test]
    fn without_a_picture_the_backdrop_is_the_theme() {
        let theme = Theme::built_in_default();
        let area = Rect::new(0, 0, 3, 1);
        let mut buffer = Buffer::empty(area);
        Backdrop { theme: &theme }.render(area, &mut buffer);
        assert_eq!(buffer.cell((1, 0)).unwrap().bg, theme.background);
    }

    #[test]
    fn pane_title_advertises_only_the_active_switch_binding() {
        use crate::keybinds::{BindAction, KeyCombo, Keybinds};
        let theme = Theme::built_in_default();
        let draw = |keybinds: &Keybinds| {
            let area = Rect::new(0, 0, 70, 1);
            let mut buffer = Buffer::empty(area);
            PaneTitle {
                keybinds,
                name: "verse",
                dirty: false,
                playing: false,
                focused: false,
                prebake: false,
                replay: false,
                theme: &theme,
            }
            .render(area, &mut buffer);
            buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        let mut bindings = Keybinds::default();
        bindings.learn(BindAction::HopPane, KeyCombo::parse("f2"));
        assert!(draw(&bindings).contains("F2 switches"));
        assert!(!draw(&bindings).contains("F10"));
        bindings.unbind(BindAction::HopPane);
        assert!(!draw(&bindings).contains("switches"));
    }

    #[test]
    fn prebake_strip_uses_active_update_close_and_settings_bindings() {
        use crate::keybinds::{BindAction, KeyCombo, Keybinds};
        let theme = Theme::built_in_default();
        let chips = [SceneChip {
            replay: false,
            prebake: Some(PrebakeScope::Local),
            name: "prebake".into(),
            current: true,
            playing: false,
            dirty: false,
            rewind: false,
            pad: None,
            errors: false,
            armed: None,
        }];
        let mut bindings = Keybinds::default();
        for (action, chord) in [
            (BindAction::Evaluate, "f2"),
            (BindAction::CloseScene, "f3"),
            (BindAction::Settings, "f4"),
        ] {
            bindings.learn(action, KeyCombo::parse(chord));
        }
        let area = Rect::new(0, 0, 200, 1);
        let mut buffer = Buffer::empty(area);
        SceneStrip {
            keybinds: &bindings,
            chips: &chips,
            mode: &SceneStripMode::Idle,
            split: false,
            capabilities: KeyboardCapabilities::enhanced(),
            theme: &theme,
        }
        .render(area, &mut buffer);
        let text = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("F2 applies"), "{text}");
        assert!(text.contains("F3 closes"), "{text}");
        assert!(text.contains("F4 reopens"), "{text}");
        for retired in ["^S", "^W", "^B"] {
            assert!(!text.contains(retired), "{text}");
        }

        for action in [
            BindAction::PreviousScene,
            BindAction::NextScene,
            BindAction::Evaluate,
            BindAction::CloseScene,
            BindAction::Settings,
        ] {
            bindings.unbind(action);
        }
        let mut buffer = Buffer::empty(area);
        SceneStrip {
            keybinds: &bindings,
            chips: &chips,
            mode: &SceneStripMode::Idle,
            split: false,
            capabilities: KeyboardCapabilities::enhanced(),
            theme: &theme,
        }
        .render(area, &mut buffer);
        let text = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        for absent in ["switch", "applies", "closes", "reopens", "/", " · "] {
            assert!(
                !text.contains(absent),
                "unbound action advertised in {text}"
            );
        }
    }
}

#[cfg(test)]
mod scene_separator_tests {
    use super::*;

    #[test]
    fn scene_markers_are_separated_without_moving_clicks_into_the_gap() {
        let chips: Vec<_> = ["intro", "breakdown"]
            .into_iter()
            .enumerate()
            .map(|(index, name)| SceneChip {
                name: name.into(),
                current: index == 1,
                playing: index == 0,
                dirty: true,
                errors: true,
                rewind: false,
                pad: None,
                armed: None,
                prebake: None,
                replay: false,
            })
            .collect();
        let theme = Theme::built_in_default();
        let area = Rect::new(3, 2, 80, 1);
        let mut buffer = Buffer::empty(area);
        SceneStrip {
            keybinds: &crate::keybinds::Keybinds::default(),
            chips: &chips,
            mode: &SceneStripMode::Idle,
            split: false,
            capabilities: KeyboardCapabilities::enhanced(),
            theme: &theme,
        }
        .render(area, &mut buffer);
        let hits = scene_strip_hits(area, &chips);
        let separator = (hits[0].right(), area.y);
        assert_eq!(hits[1].x, separator.0 + 1);
        let cell = buffer.cell(separator).expect("separator cell");
        assert_eq!(cell.symbol(), "│");
        assert_eq!(cell.fg, theme.muted);
        assert!(!hits.iter().any(|hit| hit.contains(separator.into())));
        for (index, hit) in hits.iter().enumerate() {
            for x in hit.x..hit.right() {
                assert_eq!(
                    hits.iter()
                        .position(|rect| rect.contains((x, area.y).into())),
                    Some(index)
                );
            }
        }
    }
}
