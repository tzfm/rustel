//! Studio settings and the settings sheet.
//!
//! State, navigation and shared geometry live here; `view` draws the sheet.
//! Rendering reads several display switches from process-wide atomics,
//! updated by [`UiSettings::apply`].

mod view;

#[cfg(test)]
use view::{render_keybind_rows, render_keybinds};

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthStr;

use rustel_runtime::{
    CapabilityRegistry, CapabilityReportContext, CapabilityReportV1, CapabilitySafetyFacts,
    EnginePressureSnapshot,
};

use super::keybinds::BindAction;

use super::devices::draw_border;
use super::editor::KeyboardCapabilities;
use super::engine::StudioDeviceInfo;
use super::graphics::{RenderingMode, Tier};
use super::prebake::{PrebakeRow, PrebakeScope};
use super::reference::Category;
use super::terminal::{CaretShape, TerminalFeatures};
use super::theme::Theme;

static ANIMATION: AtomicBool = AtomicBool::new(true);
static HIGHLIGHTS: AtomicBool = AtomicBool::new(true);
static MINIMAP: AtomicBool = AtomicBool::new(false);
/// The set panel docks at the right, beside the reference column, rather
/// than at the left.
static SET_PANEL_RIGHT: AtomicBool = AtomicBool::new(false);
static MASTER_SCOPE: AtomicBool = AtomicBool::new(true);
static BRACKETS: AtomicBool = AtomicBool::new(true);
/// The highlight fade's ladder index; see [`HighlightFade::LADDER`].
static HIGHLIGHT_FADE: AtomicU8 = AtomicU8::new(0);

/// Whether a composed track is taken as one `$: stack(...)` rather than as
/// one `$:` a voice. Under `cfg(test)` it is thread-local, so a test that
/// flips it never changes what a test on another thread reads.
#[cfg(not(test))]
mod examples_format {
    use std::sync::atomic::{AtomicBool, Ordering};

    static STACK: AtomicBool = AtomicBool::new(false);

    pub fn stack() -> bool {
        STACK.load(Ordering::Relaxed)
    }

    pub fn set_stack(stack: bool) {
        STACK.store(stack, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod examples_format {
    use std::cell::Cell;

    thread_local! {
        static STACK: Cell<bool> = const { Cell::new(false) };
    }

    pub fn stack() -> bool {
        STACK.get()
    }

    pub fn set_stack(stack: bool) {
        STACK.set(stack);
    }
}

pub fn animation() -> bool {
    ANIMATION.load(Ordering::Relaxed)
}
pub fn highlights() -> bool {
    HIGHLIGHTS.load(Ordering::Relaxed)
}
pub fn minimap() -> bool {
    MINIMAP.load(Ordering::Relaxed)
}

/// Whether the set panel docks at the right of the editor.
pub fn set_panel_right() -> bool {
    SET_PANEL_RIGHT.load(Ordering::Relaxed)
}

/// Whether the visuals panel docks at the right of the editor.
pub fn master_scope() -> bool {
    MASTER_SCOPE.load(Ordering::Relaxed)
}
pub fn brackets() -> bool {
    BRACKETS.load(Ordering::Relaxed)
}

/// Whether the examples tab hands over one `$: stack(...)` instead of a
/// voice a line.
pub fn examples_stack() -> bool {
    examples_format::stack()
}
/// How long a sounding mark lingers after its event, in seconds; `None`
/// leaves it to the theme.
pub fn highlight_fade() -> Option<f32> {
    HighlightFade::LADDER
        .get(usize::from(HIGHLIGHT_FADE.load(Ordering::Relaxed)))
        .copied()
        .unwrap_or_default()
        .seconds()
}

/// What the audio out latency row reads: the choice, and - when a stream is
/// open - the frames it holds and their milliseconds.
///
/// The frames are the stream's latency frames
/// ([`rustel_audio::AudioOutputFacts::latency_frames`]): the callback size
/// the host reports, or on a host whose fixed period sits below the size
/// asked for (WASAPI in shared mode) the buffer queued ahead of it. The
/// callback size alone would report 480 frames on Windows when the host
/// grants all 2048 chosen frames. A host that really gives a different
/// size than the choice is still reported.
fn output_latency_readout(
    choice: OutputLatency,
    output: Option<&rustel_audio::AudioOutputFacts>,
) -> String {
    let rate = output.map_or(0, |output| output.sample_rate_hz());
    let with_millis = |text: String, frames: u32| {
        if rate == 0 {
            text
        } else {
            let millis = f64::from(frames) * 1000.0 / f64::from(rate);
            format!("{text} \u{b7} {millis:.1} ms")
        }
    };
    match (
        choice.frames(),
        output.map(|output| output.latency_frames()),
    ) {
        (0, None) => choice.label(),
        (0, Some(granted)) => with_millis(format!("automatic \u{b7} {granted} frames"), granted),
        // What was asked for and what the host actually gave, in the
        // fewest words that still say both. It used to read "2048
        // frames \u{b7} asked \u{b7} host gave 1024 \u{b7} 21.3 ms", which is a
        // sentence, and a sentence in a value column pushes the
        // explanation off the right-hand edge of every row on the page.
        (chosen, Some(granted)) if granted != chosen => {
            with_millis(format!("{chosen} asked, {granted} given"), granted)
        }
        (chosen, _) => with_millis(format!("{chosen} frames"), chosen),
    }
}

/// The output buffer size, in frames - the one latency knob every DAW
/// shows: smaller is quicker to the speaker and hungrier on CPU. On macOS
/// and Linux it is the callback size; on Windows the callback stays at the
/// WASAPI device period and this is the buffer queued ahead of it.
///
/// The ladder is what the arrows step through; any size inside the
/// engine's accepted range parses too, so a `96` given with
/// `--buffer-frames` or kept in the preferences file survives without
/// inventing a rung for it. `Automatic` keeps the engine's policy -
/// interactive on real hardware, large on a forwarded sink that underruns
/// smaller.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OutputLatency {
    #[default]
    Automatic,
    Frames32,
    Frames64,
    Frames128,
    Frames256,
    Frames512,
    Frames1024,
    Frames2048,
    /// Any other accepted size, kept exactly as chosen.
    Frames(u32),
}

impl OutputLatency {
    pub const LADDER: [Self; 8] = [
        Self::Automatic,
        Self::Frames32,
        Self::Frames64,
        Self::Frames128,
        Self::Frames256,
        Self::Frames512,
        Self::Frames1024,
        Self::Frames2048,
    ];

    /// The engine's bounds: a size outside them, from the preferences file,
    /// reads as automatic here instead of failing the device open.
    pub const MIN_FRAMES: u32 = rustel_audio::AudioBufferPreference::MIN_FRAMES;
    pub const MAX_FRAMES: u32 = rustel_audio::AudioBufferPreference::MAX_FRAMES;

    pub fn parse(text: &str) -> Self {
        if let Ok(frames) = text.trim().parse::<u32>() {
            return Self::from_frames(frames);
        }
        Self::LADDER
            .iter()
            .copied()
            .find(|latency| latency.key() == text)
            .unwrap_or_default()
    }

    /// The rung for a frame count when the ladder has one, the exact size
    /// otherwise, and `Automatic` for a size the engine would refuse.
    ///
    /// A kept `128` used to read back as `Frames(128)`, which is not
    /// `Frames128`: the arrows could not find it on the ladder and stepped
    /// from the ladder's end instead, so a restart broke the row.
    pub fn from_frames(frames: u32) -> Self {
        if frames == 0 {
            return Self::Automatic;
        }
        if !(Self::MIN_FRAMES..=Self::MAX_FRAMES).contains(&frames) {
            return Self::Automatic;
        }
        Self::LADDER
            .iter()
            .copied()
            .find(|rung| rung.frames() == frames)
            .unwrap_or(Self::Frames(frames))
    }

    /// The name the preferences file keeps: `auto`, a ladder rung's
    /// frames, or the exact number for an off-ladder size.
    pub fn key(self) -> String {
        match self {
            Self::Automatic => "auto".to_owned(),
            Self::Frames32 => "32".to_owned(),
            Self::Frames64 => "64".to_owned(),
            Self::Frames128 => "128".to_owned(),
            Self::Frames256 => "256".to_owned(),
            Self::Frames512 => "512".to_owned(),
            Self::Frames1024 => "1024".to_owned(),
            Self::Frames2048 => "2048".to_owned(),
            Self::Frames(frames) => frames.to_string(),
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::Automatic => "automatic".to_owned(),
            other => format!("{} frames", other.frames()),
        }
    }

    /// The buffer size in frames, or 0 for the automatic policy.
    pub fn frames(self) -> u32 {
        match self {
            Self::Automatic => 0,
            Self::Frames32 => 32,
            Self::Frames64 => 64,
            Self::Frames128 => 128,
            Self::Frames256 => 256,
            Self::Frames512 => 512,
            Self::Frames1024 => 1024,
            Self::Frames2048 => 2048,
            Self::Frames(frames) => frames,
        }
    }

    fn step(self, forwards: bool) -> Self {
        // An off-ladder size (`--buffer-frames 96`, or one kept in the
        // preferences file) steps to the nearest rung in the direction
        // asked. Looking it up on the ladder found nothing, and the arrows
        // jumped to automatic's neighbours from wherever the size was.
        if let Self::Frames(frames) = self {
            let rungs = Self::LADDER[1..].iter().copied();
            let nearest = if forwards {
                rungs.clone().find(|rung| rung.frames() > frames)
            } else {
                rungs.clone().rev().find(|rung| rung.frames() < frames)
            };
            return nearest.unwrap_or(if forwards {
                Self::Automatic
            } else {
                Self::LADDER[Self::LADDER.len() - 1]
            });
        }
        let index = Self::LADDER
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(0);
        let len = Self::LADDER.len();
        let next = if forwards {
            (index + 1) % len
        } else {
            (index + len - 1) % len
        };
        Self::LADDER[next]
    }
}

/// Which evaluation outcomes flash the score.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EvaluationFlashMode {
    /// Every evaluation flashes as it is submitted; failures add two quick
    /// pulses.
    #[default]
    Full,
    /// Only a successful evaluation flashes, once its result is in.
    OnSuccess,
    /// No evaluation flashes.
    Off,
}

impl EvaluationFlashMode {
    const LADDER: [Self; 3] = [Self::Full, Self::OnSuccess, Self::Off];

    pub fn label(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::OnSuccess => "on success",
            Self::Off => "off",
        }
    }

    /// The name the preferences file keeps.
    pub fn key(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::OnSuccess => "on-success",
            Self::Off => "off",
        }
    }

    pub fn parse(text: &str) -> Self {
        Self::LADDER
            .iter()
            .copied()
            .find(|mode| mode.key() == text)
            .unwrap_or_default()
    }

    fn step(self, forwards: bool) -> Self {
        let index = Self::LADDER
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(0);
        let len = Self::LADDER.len();
        Self::LADDER[if forwards {
            (index + 1) % len
        } else {
            (index + len - 1) % len
        }]
    }
}

/// When the editor marks what the syntax checker finds.
///
/// Whatever the mode, an update runs the check itself and refuses a score
/// with an error, and the refusal is said in the footer and written to the
/// log: an error that stops a score playing is never kept quiet.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SyntaxCheck {
    /// Checked a pause after typing stops, and marked where it is.
    #[default]
    Full,
    /// Not checked while typing; an update that is refused marks where,
    /// until the text is edited again.
    OnUpdate,
    /// Never marked in the editor; the footer and the log still say.
    Off,
}

impl SyntaxCheck {
    const LADDER: [Self; 3] = [Self::Full, Self::OnUpdate, Self::Off];

    pub fn label(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::OnUpdate => "on update",
            Self::Off => "off",
        }
    }

    /// The name the preferences file keeps.
    pub fn key(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::OnUpdate => "on-update",
            Self::Off => "off",
        }
    }

    pub fn parse(text: &str) -> Self {
        Self::LADDER
            .iter()
            .copied()
            .find(|mode| mode.key() == text)
            .unwrap_or_default()
    }

    fn step(self, forwards: bool) -> Self {
        let index = Self::LADDER
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(0);
        let len = Self::LADDER.len();
        Self::LADDER[if forwards {
            (index + 1) % len
        } else {
            (index + len - 1) % len
        }]
    }
}

/// How long a sounding mark lingers after its event ends.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum HighlightFade {
    /// Whatever the theme says; the built-in themes let go over 0.3 s.
    #[default]
    Theme,
    Off,
    Short,
    Medium,
    Long,
    Slow,
    Slower,
    Slowest,
}

impl HighlightFade {
    const LADDER: [Self; 8] = [
        Self::Theme,
        Self::Off,
        Self::Short,
        Self::Medium,
        Self::Long,
        // A held mark is how a player reads a dense line back: the rungs
        // above a second are for watching WHAT sounded rather than when,
        // where a 300 ms mark is gone before the eye reaches it.
        Self::Slow,
        Self::Slower,
        Self::Slowest,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Theme => "theme's own",
            Self::Off => "off",
            Self::Short => "150 ms",
            Self::Medium => "300 ms",
            Self::Long => "600 ms",
            Self::Slow => "1 s",
            Self::Slower => "2 s",
            Self::Slowest => "3 s",
        }
    }

    /// The name the preferences file keeps.
    pub fn key(self) -> &'static str {
        match self {
            Self::Theme => "theme",
            Self::Off => "off",
            Self::Short => "150ms",
            Self::Medium => "300ms",
            Self::Long => "600ms",
            Self::Slow => "1s",
            Self::Slower => "2s",
            Self::Slowest => "3s",
        }
    }

    pub fn parse(text: &str) -> Self {
        Self::LADDER
            .iter()
            .copied()
            .find(|fade| fade.key() == text)
            .unwrap_or_default()
    }

    /// Seconds, or `None` for the theme's own.
    pub fn seconds(self) -> Option<f32> {
        match self {
            Self::Theme => None,
            Self::Off => Some(0.0),
            Self::Short => Some(0.15),
            Self::Medium => Some(0.3),
            Self::Long => Some(0.6),
            Self::Slow => Some(1.0),
            Self::Slower => Some(2.0),
            Self::Slowest => Some(3.0),
        }
    }

    fn step(self, forwards: bool) -> Self {
        let index = Self::LADDER
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(0);
        let len = Self::LADDER.len();
        Self::LADDER[if forwards {
            (index + 1) % len
        } else {
            (index + len - 1) % len
        }]
    }

    fn ladder_index(self) -> u8 {
        Self::LADDER
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(0) as u8
    }
}

/// How much process and machine detail the header draws, from nothing to
/// everything the studio can measure about itself.
///
/// The header competes for width with the transport, the scene strip and
/// the set's name, so what it shows by default is deliberately small: the
/// question "is this stuck" only needs rustel's own CPU, its memory, and
/// the frame clock. The questions past that - "is it me or is the machine
/// busy elsewhere", "is the engine keeping up with its deadline" - are for
/// a tuning session rather than a glance mid-set, so they wait for this
/// setting instead of a wider terminal.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MetricDetail {
    /// No process or machine counters in the header at all - a
    /// performance screen that would rather say nothing about the
    /// machine running it than something.
    None,
    /// rustel's own CPU share, resident memory, and the frame clock. This
    /// is enough to tell "stuck" from "working". It says nothing about the
    /// rest of the machine.
    Basic,
    /// Basic, plus the whole machine's CPU beside rustel's own: the
    /// number that answers "is it me or is the laptop busy elsewhere",
    /// sampled on the same rate-limited tick as the process reading so it
    /// costs nothing extra to keep around.
    ///
    /// The default, because the question it answers is the one asked
    /// during a set - a studio that reports only its own share leaves a
    /// player watching a low number while the machine underneath them is
    /// pinned, and nothing on the screen says so. Anyone who wants the
    /// quieter header has a setting; nobody has to find one to learn
    /// their laptop is busy.
    #[default]
    Advanced,
    /// Everything the header can show: Advanced, plus real-time engine
    /// pressure - DSP and scheduler load, active voices, cover time -
    /// the deadline detail a tuning session wants and a glance during a
    /// set does not.
    Full,
}

impl MetricDetail {
    const LADDER: [Self; 4] = [Self::None, Self::Basic, Self::Advanced, Self::Full];

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Basic => "basic",
            Self::Advanced => "advanced",
            Self::Full => "full",
        }
    }

    /// The name the preferences file keeps.
    pub fn key(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Basic => "basic",
            Self::Advanced => "advanced",
            Self::Full => "full",
        }
    }

    pub fn parse(text: &str) -> Self {
        Self::LADDER
            .iter()
            .copied()
            .find(|level| level.key() == text)
            .unwrap_or_default()
    }

    fn step(self, forwards: bool) -> Self {
        let index = Self::LADDER
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(0);
        let len = Self::LADDER.len();
        Self::LADDER[if forwards {
            (index + 1) % len
        } else {
            (index + len - 1) % len
        }]
    }

    /// Whether the header draws rustel's own process counters and the
    /// frame clock at all.
    pub fn shows_process(self) -> bool {
        !matches!(self, Self::None)
    }

    /// Whether the header adds the machine-wide CPU beside rustel's own.
    pub fn shows_machine(self) -> bool {
        matches!(self, Self::Advanced | Self::Full)
    }

    /// Whether the header adds real-time engine pressure.
    pub fn shows_pressure(self) -> bool {
        matches!(self, Self::Full)
    }
}

/// How much decoded PCM the sample browser may keep that the sounding
/// score does not name.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PreviewBudget {
    Small,
    #[default]
    Medium,
    Standard,
    Large,
    Huge,
    Unlimited,
}

impl PreviewBudget {
    const LADDER: [Self; 6] = [
        Self::Small,
        Self::Medium,
        Self::Standard,
        Self::Large,
        Self::Huge,
        Self::Unlimited,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Small => "32 MiB",
            Self::Medium => "64 MiB",
            Self::Standard => "128 MiB",
            Self::Large => "256 MiB",
            Self::Huge => "512 MiB",
            Self::Unlimited => "no cap",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Small => "32mib",
            Self::Medium => "64mib",
            Self::Standard => "128mib",
            Self::Large => "256mib",
            Self::Huge => "512mib",
            Self::Unlimited => "off",
        }
    }

    pub fn parse(text: &str) -> Self {
        Self::LADDER
            .iter()
            .copied()
            .find(|budget| budget.key() == text)
            .unwrap_or_default()
    }

    /// Bytes, or zero for no ceiling.
    pub fn bytes(self) -> usize {
        match self {
            Self::Small => 32 * 1024 * 1024,
            Self::Medium => 64 * 1024 * 1024,
            Self::Standard => 128 * 1024 * 1024,
            Self::Large => 256 * 1024 * 1024,
            Self::Huge => 512 * 1024 * 1024,
            Self::Unlimited => 0,
        }
    }

    fn step(self, forwards: bool) -> Self {
        let index = Self::LADDER
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(1);
        let len = Self::LADDER.len();
        Self::LADDER[if forwards {
            (index + 1) % len
        } else {
            (index + len - 1) % len
        }]
    }
}

/// The most one sound may hold.
///
/// A guard against a file that is not music - a folder holding a disk
/// image, a URL that never ends - rather than a memory policy, so its rungs
/// are generous. What actually bounds resident audio is the preview budget
/// beside it, and what a score names is never dropped.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SampleCeiling {
    Tight,
    Modest,
    #[default]
    Roomy,
    Wide,
    Vast,
}

impl SampleCeiling {
    const LADDER: [Self; 5] = [
        Self::Tight,
        Self::Modest,
        Self::Roomy,
        Self::Wide,
        Self::Vast,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Tight => "64 MiB",
            Self::Modest => "128 MiB",
            Self::Roomy => "256 MiB",
            Self::Wide => "512 MiB",
            Self::Vast => "1 GiB",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Tight => "64mib",
            Self::Modest => "128mib",
            Self::Roomy => "256mib",
            Self::Wide => "512mib",
            Self::Vast => "1gib",
        }
    }

    pub fn parse(text: &str) -> Self {
        Self::LADDER
            .iter()
            .copied()
            .find(|ceiling| ceiling.key() == text)
            .unwrap_or_default()
    }

    pub fn bytes(self) -> usize {
        match self {
            Self::Tight => 64 * 1024 * 1024,
            Self::Modest => 128 * 1024 * 1024,
            Self::Roomy => rustel_audio::DEFAULT_SAMPLE_PCM_BYTES,
            Self::Wide => 512 * 1024 * 1024,
            Self::Vast => rustel_audio::MAX_SAMPLE_PCM_BYTES,
        }
    }

    /// Roughly how long a sound this holds, said in the units a musician
    /// counts in: 48 kHz float stereo, which is what a modern take is.
    pub fn minutes(self) -> f32 {
        const BYTES_A_SECOND: f32 = 48_000.0 * 2.0 * 4.0;
        self.bytes() as f32 / BYTES_A_SECOND / 60.0
    }

    fn step(self, forwards: bool) -> Self {
        let index = Self::LADDER
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(2);
        let len = Self::LADDER.len();
        Self::LADDER[if forwards {
            (index + 1) % len
        } else {
            (index + len - 1) % len
        }]
    }
}

/// How long after the last sample preview unused decoded PCM is dropped.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum UnusedSampleIdle {
    Quick,
    #[default]
    Standard,
    Slow,
    Long,
    Never,
}

impl UnusedSampleIdle {
    const LADDER: [Self; 5] = [
        Self::Quick,
        Self::Standard,
        Self::Slow,
        Self::Long,
        Self::Never,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Quick => "15 s",
            Self::Standard => "30 s",
            Self::Slow => "1 min",
            Self::Long => "5 min",
            Self::Never => "never",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Quick => "15s",
            Self::Standard => "30s",
            Self::Slow => "1min",
            Self::Long => "5min",
            Self::Never => "off",
        }
    }

    pub fn parse(text: &str) -> Self {
        Self::LADDER
            .iter()
            .copied()
            .find(|idle| idle.key() == text)
            .unwrap_or_default()
    }

    /// Idle pause, or zero to never drop by time.
    pub fn duration(self) -> std::time::Duration {
        match self {
            Self::Quick => std::time::Duration::from_secs(15),
            Self::Standard => std::time::Duration::from_secs(30),
            Self::Slow => std::time::Duration::from_secs(60),
            Self::Long => std::time::Duration::from_secs(300),
            Self::Never => std::time::Duration::ZERO,
        }
    }

    fn step(self, forwards: bool) -> Self {
        let index = Self::LADDER
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(1);
        let len = Self::LADDER.len();
        Self::LADDER[if forwards {
            (index + 1) % len
        } else {
            (index + len - 1) % len
        }]
    }
}

/// How often the screen is repainted while something is moving.
///
/// Sixty a second is what a meter riding a sound wants, and on a machine
/// with cores to spare it costs nothing worth counting. It is also, while
/// a set plays, the single biggest thing the studio asks of the
/// processor: the screen is repainted at that rate whether or not
/// anything on it is a visualizer - measured on one machine, the drawing
/// took more of a core than thirty lanes of sounding DSP did. A player
/// who wants those cycles for the sound can have them here.
///
/// Nothing about the audio changes with it. The score is scheduled on the
/// engine's own thread and the mix is rendered in the device's callback,
/// neither of which is paced by this; what slows is how often a meter, a
/// highlight or a scope catches up with them. Typing is not paced by it
/// either: a keystroke paints at once, playing or stopped.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FrameRate {
    Quadruple,
    Double,
    #[default]
    Smooth,
    Half,
    Quarter,
    Thrift,
}

impl FrameRate {
    const LADDER: [Self; 6] = [
        Self::Quadruple,
        Self::Double,
        Self::Smooth,
        Self::Half,
        Self::Quarter,
        Self::Thrift,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Quadruple => "240 fps",
            Self::Double => "120 fps",
            Self::Smooth => "60 fps \u{b7} smooth",
            Self::Half => "30 fps",
            Self::Quarter => "15 fps",
            Self::Thrift => "8 fps \u{b7} thrifty",
        }
    }

    /// The name the preferences file keeps.
    pub fn key(self) -> &'static str {
        match self {
            Self::Quadruple => "240",
            Self::Double => "120",
            Self::Smooth => "60",
            Self::Half => "30",
            Self::Quarter => "15",
            Self::Thrift => "8",
        }
    }

    pub fn parse(text: &str) -> Self {
        Self::LADDER
            .into_iter()
            .find(|rate| rate.key() == text)
            .unwrap_or_default()
    }

    /// How long one frame is allowed to take.
    ///
    /// The two rungs above 60 are asked for rather than promised: a frame
    /// is a screenful of escape codes down a pipe, and past the glass's
    /// own refresh the terminal is the thing that cannot keep up. They
    /// are here because a fast terminal on a fast display can, and the
    /// studio should not be the one saying no.
    pub fn interval(self) -> std::time::Duration {
        match self {
            Self::Quadruple => std::time::Duration::from_micros(4_167),
            Self::Double => std::time::Duration::from_micros(8_333),
            Self::Smooth => std::time::Duration::from_micros(16_667),
            Self::Half => std::time::Duration::from_micros(33_333),
            Self::Quarter => std::time::Duration::from_micros(66_667),
            Self::Thrift => std::time::Duration::from_micros(125_000),
        }
    }

    fn step(self, forwards: bool) -> Self {
        let at = Self::LADDER
            .iter()
            .position(|rate| *rate == self)
            .unwrap_or(0);
        let len = Self::LADDER.len();
        Self::LADDER[if forwards {
            (at + 1) % len
        } else {
            (at + len - 1) % len
        }]
    }
}

/// When a launch - a pad, an update - takes effect.
///
/// Off by default, so a launch sounds at once: a wait before the first
/// sound looks like a missed key press. The line options are one row of
/// the sheet away, and the countdown chip shows the wait when one is on.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Quantise {
    #[default]
    Off,
    /// A quarter cycle.
    Beat,
    Cycle,
    /// Every this many cycles.
    Phrase(u8),
}

impl Quantise {
    pub fn label(self) -> String {
        match self {
            Self::Off => "off · immediate".to_owned(),
            Self::Beat => "beat".to_owned(),
            Self::Cycle => "cycle".to_owned(),
            Self::Phrase(cycles) => format!("{cycles} cycles"),
        }
    }

    pub fn key(self) -> String {
        match self {
            Self::Off => "off".to_owned(),
            Self::Beat => "beat".to_owned(),
            Self::Cycle => "cycle".to_owned(),
            Self::Phrase(cycles) => cycles.to_string(),
        }
    }

    pub fn parse(text: &str) -> Self {
        match text {
            "off" => Self::Off,
            "beat" => Self::Beat,
            // `1` is a cycle by another name - the number a phrase is
            // counted in, with one of them in it.
            "cycle" | "1" => Self::Cycle,
            other => other
                .parse::<u8>()
                .ok()
                .filter(|cycles| *cycles >= 2)
                .map_or_else(Self::default, Self::Phrase),
        }
    }

    /// Cycles per line, `None` for immediate.
    pub fn unit_cycles(self) -> Option<f64> {
        match self {
            Self::Off => None,
            Self::Beat => Some(0.25),
            Self::Cycle => Some(1.0),
            Self::Phrase(cycles) => Some(f64::from(cycles)),
        }
    }

    const LADDER: [Quantise; 6] = [
        Quantise::Off,
        Quantise::Beat,
        Quantise::Cycle,
        Quantise::Phrase(2),
        Quantise::Phrase(4),
        Quantise::Phrase(8),
    ];

    fn step(self, forwards: bool) -> Self {
        let index = Self::LADDER
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(2);
        let len = Self::LADDER.len();
        Self::LADDER[if forwards {
            (index + 1) % len
        } else {
            (index + len - 1) % len
        }]
    }
}

/// How a start or an edit meets sounds that are still loading.
///
/// Wait by default: a first bar that plays its sounds whole is worth the
/// moment a download takes. Async skips a note that misses its time.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LoadMode {
    /// A start waits for the sounds of its first window and begins on
    /// cycle zero; an edit naming a sound not ready keeps the last score
    /// playing until it is.
    #[default]
    Wait,
    /// Starts and edits land at once, and a note whose sound is still
    /// loading is skipped.
    Async,
}

impl LoadMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Wait => "wait",
            Self::Async => "async",
        }
    }

    pub fn key(self) -> &'static str {
        self.label()
    }

    /// Anything unreadable is the default. `strudel-like` also reads as
    /// async.
    pub fn parse(text: &str) -> Self {
        match text {
            "async" | "strudel-like" => Self::Async,
            _ => Self::Wait,
        }
    }

    fn step(self) -> Self {
        match self {
            Self::Wait => Self::Async,
            Self::Async => Self::Wait,
        }
    }
}

/// The next choice on a port row: off, then each port, then off.
///
/// Used by the devices panel's MIDI tab, where the clock ports are chosen
/// now. They lived on this sheet, a page away from the ports they name and
/// from the boxes that decide whether those ports may be opened at all.
pub(super) fn step_port(
    current: &Option<String>,
    ports: &[String],
    forwards: bool,
) -> Option<String> {
    let index = current
        .as_ref()
        .and_then(|name| ports.iter().position(|port| port == name))
        .map(|index| index + 1)
        .unwrap_or(0);
    let len = ports.len() + 1;
    let next = if forwards {
        (index + 1) % len
    } else {
        (index + len - 1) % len
    };
    (next > 0).then(|| ports[next - 1].clone())
}

/// A gamepad axis, as a mapping slot names it: which stick, on whichever
/// pad is plugged in. Held a little it walks the fader that slot drives,
/// held all the way it runs - a stick springs back to the middle, so it
/// pushes rather than pointing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum StickAxis {
    Off,
    /// The left stick, side to side - where a thumb rests on every pad.
    #[default]
    X1,
    Y1,
    X2,
    Y2,
}

impl StickAxis {
    pub const ALL: [Self; 5] = [Self::Off, Self::X1, Self::Y1, Self::X2, Self::Y2];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::X1 => "x1 · left stick ↔",
            Self::Y1 => "y1 · left stick ↕",
            Self::X2 => "x2 · right stick ↔",
            Self::Y2 => "y2 · right stick ↕",
        }
    }

    /// The name the score reads the axis by, and the preferences keep.
    pub fn key(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::X1 => "x1",
            Self::Y1 => "y1",
            Self::X2 => "x2",
            Self::Y2 => "y2",
        }
    }

    pub fn parse(text: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|axis| axis.key() == text.trim())
            .unwrap_or_default()
    }

    /// The axis as the pad numbers it: 0 to 3, or none for off.
    pub fn index(self) -> Option<usize> {
        match self {
            Self::Off => None,
            Self::X1 => Some(0),
            Self::Y1 => Some(1),
            Self::X2 => Some(2),
            Self::Y2 => Some(3),
        }
    }

    pub fn from_index(index: usize) -> Self {
        match index {
            0 => Self::X1,
            1 => Self::Y1,
            2 => Self::X2,
            3 => Self::Y2,
            _ => Self::Off,
        }
    }

    /// Up and down: the page reads down as positive, and a thumb pushed
    /// up should raise a slider, so these run the other way.
    pub fn is_vertical(self) -> bool {
        matches!(self, Self::Y1 | Self::Y2)
    }

    pub fn step(self, forwards: bool) -> Self {
        let at = Self::ALL.iter().position(|axis| *axis == self).unwrap_or(0);
        let len = Self::ALL.len();
        Self::ALL[if forwards {
            (at + 1) % len
        } else {
            (at + len - 1) % len
        }]
    }
}

/// A MIDI control, as a mapping slot names it: a control change number on
/// one channel, or on any. A knob stays where it is put, so it points
/// rather than pushing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SliderCc {
    pub controller: u8,
    pub channel: Option<u8>,
}

impl SliderCc {
    /// The mod wheel, on any channel: the one knob every keyboard has.
    pub const DEFAULT: Self = Self {
        controller: 1,
        channel: None,
    };

    pub fn label(self) -> String {
        match self.channel {
            Some(channel) => format!("cc{} · ch{channel}", self.controller),
            None => format!("cc{} · any channel", self.controller),
        }
    }

    /// The preferences' spelling: `cc74/ch2`, or `cc1` for any channel.
    pub fn key(self) -> String {
        match self.channel {
            Some(channel) => format!("cc{}/ch{channel}", self.controller),
            None => format!("cc{}", self.controller),
        }
    }

    /// `off` is none; anything unreadable is the default.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text == "off" {
            return None;
        }
        let (cc, channel) = text.split_once('/').unwrap_or((text, ""));
        let controller = cc.strip_prefix("cc").and_then(|n| n.parse::<u8>().ok());
        let channel = channel
            .strip_prefix("ch")
            .and_then(|n| n.parse::<u8>().ok());
        Some(controller.map_or(Self::DEFAULT, |controller| Self {
            controller: controller.min(127),
            channel,
        }))
    }

    pub fn matches(self, controller: u8, channel: u8) -> bool {
        self.controller == controller && self.channel.is_none_or(|own| own == channel)
    }

    /// The next control number along, on the same channel, round the end.
    pub fn step(self, forwards: bool) -> Self {
        Self {
            controller: if forwards {
                (self.controller + 1) % 128
            } else {
                (self.controller + 127) % 128
            },
            channel: self.channel,
        }
    }
}

/// What a controller's position means to the fader it drives.
///
/// A knob that is physically at a quarter turn and a fader that is at
/// four thousand disagree the moment a set is opened, and something has
/// to give. Which answer is right depends on the hardware, and the
/// hardware differs per slot - an encoder on one, a sprung fader on the
/// next - so this is a property of the slot, not of the studio.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Takeover {
    /// The fader moves the way the knob turned, not to where it points:
    /// each turn carries the fader the share of its remaining travel that
    /// the turn covered of the knob's, so the two run out together and the
    /// fader lands on the end exactly as the knob reaches it. Never jumps,
    /// and never feels dead: the default, because it is the only one that
    /// is both.
    #[default]
    Scaled,
    /// The knob's position IS the value. Immediate, and jumps on the
    /// first touch after a set is opened.
    Jump,
    /// The message is a nudge, not a position: 64 is still, and each unit
    /// either side is a notch. For an endless encoder set to relative in
    /// the hardware - where it is the only one that works at all.
    Relative,
}

impl Takeover {
    pub const ALL: [Self; 3] = [Self::Scaled, Self::Jump, Self::Relative];

    pub fn label(self) -> &'static str {
        match self {
            Self::Scaled => "scaled - follows the turn, never jumps",
            Self::Jump => "jump - the knob's position is the value",
            Self::Relative => "relative - an endless encoder's notches",
        }
    }

    /// The word a slot box has room for.
    pub fn chip(self) -> &'static str {
        match self {
            Self::Scaled => "scaled",
            Self::Jump => "jump",
            Self::Relative => "relative",
        }
    }

    pub fn key(self) -> &'static str {
        self.chip()
    }

    pub fn parse(text: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|mode| mode.key() == text.trim())
            .unwrap_or_default()
    }

    pub fn step(self, forwards: bool) -> Self {
        let at = Self::ALL.iter().position(|mode| *mode == self).unwrap_or(0);
        let len = Self::ALL.len();
        Self::ALL[if forwards {
            (at + 1) % len
        } else {
            (at + len - 1) % len
        }]
    }
}

/// How many controls a player can assign. Twelve is a bank of knobs on
/// the boxes people actually own, and enough to reach for mid-set without
/// hunting.
pub const MAPPING_SLOTS: usize = 12;

/// What drives one mapping slot.
///
/// There is no factory map, because controllers do not agree: a Maschine's
/// faders are CC 70-77, a Launchkey's are CC 90-93, a nanoKONTROL's are
/// CC 0-7. A slot learns its control instead: press Enter on the slot and
/// move the control.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MappingSource {
    /// A MIDI control change, on one channel or on any.
    Knob(SliderCc),
    /// A gamepad axis, on whichever pad pushes it furthest.
    Stick(StickAxis),
}

impl MappingSource {
    /// What the slot says it is bound to.
    pub fn label(self) -> String {
        match self {
            Self::Knob(knob) => knob.label(),
            Self::Stick(axis) => axis.label().to_owned(),
        }
    }

    /// Short enough for a slot box: `cc74·2`, `x1`.
    pub fn chip(self) -> String {
        match self {
            Self::Knob(knob) => match knob.channel {
                Some(channel) => format!("cc{}\u{00b7}{channel}", knob.controller),
                None => format!("cc{}", knob.controller),
            },
            Self::Stick(axis) => axis.key().to_owned(),
        }
    }

    /// The preferences' spelling: `cc74/ch2`, or `x1`.
    pub fn key(self) -> String {
        match self {
            Self::Knob(knob) => knob.key(),
            Self::Stick(axis) => axis.key().to_owned(),
        }
    }

    /// `off` and anything unreadable is nothing bound. A `cc` spelling is
    /// a knob; an axis name is a stick.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.is_empty() || text == "off" {
            return None;
        }
        if text.starts_with("cc") {
            return SliderCc::parse(text).map(Self::Knob);
        }
        // Strict, unlike the axis row's own reader: an unreadable slot is
        // nothing bound, not silently the left stick driving a fader.
        StickAxis::ALL
            .into_iter()
            .find(|axis| *axis != StickAxis::Off && axis.key() == text)
            .map(Self::Stick)
    }
}

/// The switches, as remembered.
///
/// `PartialEq` and not `Eq`: the limiter's ceiling is a decibel figure,
/// and the sheet only ever asks whether something changed.
#[derive(Clone, Debug, PartialEq)]
pub struct UiSettings {
    /// Shortcut conflict profile. None follows the detected terminal; an
    /// override changes shortcut defaults without claiming extra capabilities.
    pub terminal_profile: Option<String>,
    pub rendering: RenderingMode,
    /// How often the screen is repainted while something moves.
    pub frame_rate: FrameRate,
    /// Show folder paths in file status messages, rather than only filenames.
    pub show_full_paths: bool,
    pub quantise: Quantise,
    pub load_mode: LoadMode,
    /// MIDI clock out, by port; none to send none.
    pub clock_out: Option<String>,
    /// MIDI clock in, by port; none to keep the studio's own clock.
    pub clock_in: Option<String>,
    pub animation: bool,
    pub highlights: bool,
    pub evaluation_flash: EvaluationFlashMode,
    /// A finished recording has its leading and trailing silence cut before
    /// it is saved. On by default: the silence around a take is the count-in
    /// and the wait for the next one, not part of the performance.
    pub trim_recordings: bool,
    /// How long a sounding mark lingers after its event.
    pub highlight_fade: HighlightFade,
    /// How much process and machine detail the header draws.
    pub metric_detail: MetricDetail,
    /// The output buffer size: the one latency knob, in frames, like a
    /// DAW's buffer selector - 32, 128, 256 and the rest.
    pub output_latency: OutputLatency,
    /// Default voice policy, unless the score sets its own limit.
    pub max_polyphony: usize,
    pub piano_sound: super::PianoSound,
    pub piano_volume: u16,
    /// Whether a set STARTS with a brickwall on the output. Off by
    /// default, because it costs its lookahead in latency on everything
    /// that plays, and without it the output is still clipped at full
    /// scale.
    ///
    /// The studio's default, not a set's: it decides whether a set that
    /// has never said anything opens with a limiter on its desk. A set
    /// given one of its own keeps that, and turning this off afterwards
    /// does not take it away.
    ///
    /// One switch, on the first page, because "do I want a limiter" is
    /// the whole of the question most of the time. How it sounds and
    /// where it holds are on Advanced, and on the set's own strip.
    pub master_limiter_on: bool,
    /// How that limiter works when it is switched on.
    ///
    /// Kept whether it is on or off - the same reason as the ceiling
    /// below - and kept as a character rather than an `Option` so the
    /// switch above has exactly one thing to say.
    pub master_limiter_character: rustel_audio::LimiterCharacter,
    /// Where that limiter holds, in dBFS. Kept whether it is on or off, so
    /// turning it off to hear something and on again brings back the
    /// ceiling that was set rather than the default.
    pub master_limiter_ceiling_db: f32,
    /// Bring the ceiling back up to full scale after limiting.
    ///
    /// On, because the alternative is that switching a limiter on makes a
    /// set quieter by exactly the headroom it was given, which reads as
    /// the limiter having broken the volume rather than having changed the
    /// sound. The gain is `1 / ceiling` and nothing else - the limiter has
    /// already promised nothing leaves above the ceiling, so it lands the
    /// ceiling on full scale and cannot pass it.
    pub master_limiter_makeup: bool,
    pub minimap: bool,
    /// Whether overflowing editor panes draw their scrollbars.
    pub show_scrollbars: bool,
    /// The set panel's side: docked at the right, beside the reference
    /// column, rather than at the left.
    pub set_panel_right: bool,
    /// The mixer panel's edge: a band across the top rather than the
    /// bottom.
    pub mixer_top: bool,
    /// The visuals panel's side: the outer edge on the right, or the left.
    /// Where each visuals dock sits: a column down a side, or a band
    /// across the top or the bottom.
    pub viz_edges: [super::viz_panel::Edge; super::viz_panel::DOCKS],
    /// The numbers down the left edge of the score.
    pub line_numbers: bool,
    /// Long lines continue on the next row, at each column's width,
    /// instead of running off the edge. On by default in the terminal.
    pub wrap: bool,
    pub master_scope: bool,
    pub brackets: bool,
    /// Shape requested through DECSCUSR when the active theme does not
    /// override it. The long-standing steady bar remains the default.
    pub caret_shape: CaretShape,
    /// When the editor marks what the checker finds: as it is typed, only
    /// where an update was refused, or never. An update refuses a score with
    /// an error the same way in every mode, and says why in the footer and
    /// the log: the setting is the studio's, not the engine's.
    pub syntax_check: SyntaxCheck,
    /// The log panel shows the studio's running commentary as well as the
    /// events worth interrupting a set for. Off, so the panel opens on
    /// what happened rather than on everything that happened.
    pub log_verbose: bool,
    /// What the examples tab hands over: one `$:` a voice, or the same
    /// voices folded into a single `$: stack(...)`.
    pub examples_stack: bool,
    /// Whether Hydra's backdrop averages the pixels it scales down, or picks
    /// one of them. Costs the same either way; it is purely how it looks.
    pub backdrop_smoothing: bool,
    /// Smooth direct gain/filter slider mouse changes in the audio engine.
    pub slider_smoothing: bool,
    /// Equal distance means equal frequency ratios for direct Hz controls.
    pub frequency_slider_log: bool,
    /// The twelve mapping slots, in order. Slot `n` drives the `n`th
    /// slider of the evaluated score - the `n`th fader on the mixer's
    /// desk - so a mapping outlives the score that was open when it was
    /// made. Nothing is bound until a player learns it.
    pub mappings: [Option<MappingSource>; MAPPING_SLOTS],
    /// What each slot's controller position means to its fader. Per slot,
    /// because the hardware is: an endless encoder on one and a sprung
    /// fader on the next want opposite answers.
    pub takeover: [Takeover; MAPPING_SLOTS],
    /// Explicit permission for score-requested `sN.initCam()` capture. Web
    /// images do not depend on this switch.
    #[cfg(feature = "hydra")]
    pub hydra_webcam: bool,
    /// How strongly score backdrops and native theme effects appear, as a
    /// percentage. The code has to stay readable over either renderer, so it
    /// is a knob rather than a constant. Whole percent, because that is what
    /// the sheet shows and what the preferences file should say.
    pub backdrop_opacity: u8,
    /// How solid everything the interface painted for itself is over the
    /// picture - the header, the footer, the pane separator, the minimap, a
    /// panel, a highlight.
    ///
    /// This value is the opacity of the interface, so it runs the opposite way
    /// to `backdrop_opacity`: a hundred is a solid interface with the picture
    /// strictly behind it, and lower lets the sketch come through the chrome.
    pub interface_opacity: u8,
    /// How solid the code area itself is over the picture. Its own number,
    /// apart from the chrome's: the score is read for minutes at a time
    /// while the header is glanced at, and the two want different amounts
    /// of picture behind them. Runs the interface's way - a hundred is a
    /// solid code area with the picture strictly behind it.
    pub editor_opacity: u8,
    /// The stage and the status bar hidden: the score fills the frame, open
    /// panels float over it and the visuals docks close. Not written to the
    /// preferences file: opening the studio into a stripped-down window
    /// nobody asked for is not a kindness. Independent of
    /// [`Self::show_menu`], [`Self::show_header`] and [`Self::show_footer`]:
    /// those are the player's lasting chrome, and leaving zen restores them.
    pub zen: bool,
    /// The File / Edit menu bar along the top. On by default; remembered.
    pub show_menu: bool,
    /// The rustel PLAYING tempo line. On by default; remembered. Not the
    /// menu bar, and not the scene tabs.
    pub show_header: bool,
    /// The footer's meter, orbits and device chips. On by default; remembered.
    /// Off still keeps the notices and the status line with the caret's
    /// line:column.
    pub show_footer: bool,
    /// Ceiling on decoded PCM the sounding score does not name.
    pub preview_budget: PreviewBudget,
    /// The most one sound may hold before it is refused as not-music.
    pub sample_ceiling: SampleCeiling,
    /// After this long without a sample preview, unused decoded PCM is dropped.
    pub unused_sample_idle: UnusedSampleIdle,
    /// Whether an imported source is fetched into the cache in the
    /// background rather than on the first note that needs it.
    pub precache_sources: bool,
    /// The reference categories the browse list and the suggestions hide.
    pub reference_hidden: Vec<Category>,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            terminal_profile: None,
            rendering: RenderingMode::Automatic,
            frame_rate: FrameRate::default(),
            show_full_paths: false,
            quantise: Quantise::Off,
            load_mode: LoadMode::Wait,
            clock_out: None,
            clock_in: None,
            animation: true,
            highlights: true,
            evaluation_flash: EvaluationFlashMode::Full,
            trim_recordings: true,
            highlight_fade: HighlightFade::Theme,
            metric_detail: MetricDetail::Basic,
            master_limiter_on: false,
            output_latency: OutputLatency::Automatic,
            max_polyphony: rustel_audio::MAX_POLYPHONY,
            piano_sound: super::PianoSound::default(),
            piano_volume: super::piano::DEFAULT_PIANO_VOLUME,
            master_limiter_character: rustel_audio::LimiterCharacter::default(),
            master_limiter_ceiling_db: MASTER_LIMITER_DEFAULT_CEILING_DB,
            master_limiter_makeup: true,
            minimap: false,
            show_scrollbars: true,
            set_panel_right: true,
            mixer_top: false,
            viz_edges: [super::viz_panel::Edge::Right, super::viz_panel::Edge::Top],
            line_numbers: true,
            wrap: true,
            master_scope: true,
            brackets: true,
            caret_shape: CaretShape::default(),
            syntax_check: SyntaxCheck::Full,
            log_verbose: false,
            examples_stack: false,
            backdrop_smoothing: false,
            slider_smoothing: false,
            frequency_slider_log: true,
            mappings: [None; MAPPING_SLOTS],
            takeover: [Takeover::default(); MAPPING_SLOTS],
            #[cfg(feature = "hydra")]
            hydra_webcam: false,
            backdrop_opacity: BACKDROP_OPACITY_DEFAULT,
            interface_opacity: INTERFACE_OPACITY_DEFAULT,
            editor_opacity: EDITOR_OPACITY_DEFAULT,
            zen: false,
            show_menu: true,
            show_header: true,
            show_footer: true,
            preview_budget: PreviewBudget::Medium,
            sample_ceiling: SampleCeiling::default(),
            unused_sample_idle: UnusedSampleIdle::Standard,
            precache_sources: false,
            reference_hidden: Vec::new(),
        }
    }
}

impl UiSettings {
    /// The studio's own limiter, composed from the rows that edit it, or
    /// `None` while the switch is off.
    ///
    /// Three rows rather than one value because the ceiling and the
    /// character have to outlive being switched off: a player who set -6
    /// and then turned the limiter off to hear something wants -6 back,
    /// not the default, and one `Option<LimiterSettings>` has nowhere to
    /// keep it.
    pub fn master_limiter(&self) -> Option<rustel_audio::LimiterSettings> {
        self.master_limiter_on.then(|| self.limiter_default())
    }

    /// The ceiling and character the studio hands out - what a set's own
    /// limiter starts from, whether or not the switch above is on.
    ///
    /// A set being given a limiter from the desk is the player asking for
    /// one now; the switch answers a different question, which is whether
    /// sets come with one already.
    pub fn limiter_default(&self) -> rustel_audio::LimiterSettings {
        rustel_audio::LimiterSettings {
            threshold_db: self.master_limiter_ceiling_db,
            character: self.master_limiter_character,
        }
    }
    /// Whether the reference lists `category`.
    pub fn shows_category(&self, category: Category) -> bool {
        !self.reference_hidden.contains(&category)
    }

    /// The hidden categories, in [`Category::ALL`] order.
    pub fn hidden_categories(&self) -> impl Iterator<Item = Category> + '_ {
        Category::ALL
            .into_iter()
            .filter(|category| !self.shows_category(*category))
    }

    /// Make the switches current; the tier is set by the caller, who knows
    /// the terminal.
    pub fn apply(&self) {
        ANIMATION.store(self.animation, Ordering::Relaxed);
        HIGHLIGHTS.store(self.highlights, Ordering::Relaxed);
        HIGHLIGHT_FADE.store(self.highlight_fade.ladder_index(), Ordering::Relaxed);
        MINIMAP.store(self.minimap, Ordering::Relaxed);
        SET_PANEL_RIGHT.store(self.set_panel_right, Ordering::Relaxed);
        MASTER_SCOPE.store(self.master_scope, Ordering::Relaxed);
        BRACKETS.store(self.brackets, Ordering::Relaxed);
        examples_format::set_stack(self.examples_stack);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Row {
    Rendering,
    /// How often the screen is repainted while something moves.
    FrameRate,
    ShowFullPaths,
    Quantise,
    /// How a start or an edit meets sounds still loading.
    LoadMode,
    Animation,
    Highlights,
    EvaluationFlash,
    /// How long a sounding mark lingers after its event.
    HighlightFade,
    /// How much process and machine detail the header draws.
    MetricDetail,
    /// The output buffer size - the one latency knob, like a DAW's
    /// buffer selector: 32, 128, 256 and the rest of the ladder.
    OutputLatency,
    MaxPolyphony,
    PianoSound,
    PianoVolume,
    /// Whether a set starts with a brickwall on the output. One switch,
    /// on the first page: a set given one of its own on the desk keeps
    /// that instead, and how the studio's own sounds is on Advanced.
    MasterLimiter,
    /// The character that default works with, on Advanced.
    MasterLimiterCharacter,
    /// The ceiling that default holds, in dBFS, on Advanced.
    MasterLimiterCeiling,
    /// Whether that ceiling is brought back up to full scale.
    MasterLimiterMakeup,
    Minimap,
    ShowScrollbars,
    LineNumbers,
    Wrap,
    MasterScope,
    Brackets,
    CaretShape,
    /// When the editor marks the checker's findings: as they are typed,
    /// on an update, or never.
    SyntaxCheck,
    SliderSmoothing,
    FrequencySliderLog,
    #[cfg(feature = "hydra")]
    BackdropSmoothing,
    #[cfg(feature = "hydra")]
    HydraWebcam,
    BackdropStrength,
    InterfaceOpacity,
    EditorOpacity,
    ShowMenu,
    ShowHeader,
    ShowFooter,
    Zen,
    /// The set panel's side of the screen.
    SetPanelSide,
    /// The mixer panel's edge: the bottom, or the top.
    MixerEdge,
    /// The first visuals dock's edge of the screen.
    VizEdgeOne,
    /// The second's.
    VizEdgeTwo,
    /// Where new sets are made; Enter chooses a folder.
    SetsFolder,
    /// Where finished audio takes are written; Enter chooses a folder.
    RecordingsFolder,
    /// Opens the setup every set runs, kept beside the settings.
    GlobalPrebake,
    /// Opens this set's own setup, kept inside its project file.
    LocalPrebake,
    /// A finished recording has its leading and trailing silence cut
    /// before it is saved.
    TrimRecordings,
    PreviewBudget,
    SampleCeiling,
    UnusedSampleIdle,
    PrecacheSources,
    /// Enter downloads every remote pack onto disk - defaults and user URLs.
    CacheDefaults,
    /// Enter refetches every remote pack's list - defaults and user URLs.
    RefreshSources,
    /// Not a setting: how much the sample cache holds, and Enter to empty it.
    SampleCache,
    /// Whether the reference and the suggestions list this category.
    ReferenceCategory(Category),
}

/// Enough visual effect to be present, little enough to read code over.
pub const BACKDROP_OPACITY_DEFAULT: u8 = 55;
/// Mostly solid to the eye, with enough of the sketch coming through the
/// chrome that the picture reads as one wash across the screen rather than
/// a rectangle inside a frame. The surface opacities are on a perceptual
/// scale (see `backdrop_strength`), so fifty here is about a fifth of the
/// picture's light.
pub const INTERFACE_OPACITY_DEFAULT: u8 = 50;
/// A quarter solid, perceptually: with visuals opacity at its default this
/// leaves the code area the picture it has always shown, while the chrome
/// around it stays the calmer surface.
pub const EDITOR_OPACITY_DEFAULT: u8 = 25;
/// How far one press moves either of them. Zero is reachable, because turning
/// the backdrop off without editing the score is a reasonable thing to want
/// mid-set.
const OPACITY_STEP: u8 = 5;

/// The ceiling a limiter turned on starts at, wherever it was turned on
/// from: a decibel of headroom under full scale, which catches the peaks a
/// set overshoots on without audibly working on anything else.
pub const MASTER_LIMITER_DEFAULT_CEILING_DB: f32 = -1.0;

/// The lowest ceiling worth offering, and the floor the desk's strip reads
/// as "off". A limiter working thirty decibels down is not a limiter.
pub const MASTER_LIMITER_FLOOR_DB: f32 = -24.0;

/// `off`, or a ceiling and a character: `-1.0:transparent`.
///
/// Written out even when off, so a file that has been through a studio
/// says so - otherwise "absent" and "the player turned it off" look the
/// same, and a later default could quietly turn it back on.
pub fn write_master_limiter(settings: Option<rustel_audio::LimiterSettings>) -> String {
    settings.map_or_else(
        || "off".to_owned(),
        |settings| format!("{:.1}:{}", settings.threshold_db, settings.character.key()),
    )
}

/// The other direction. Anything that does not parse is off: a limiter is
/// opted into, so a line nobody can read is not an invitation to turn it
/// on behind the player's back.
pub fn parse_master_limiter(text: &str) -> Option<rustel_audio::LimiterSettings> {
    let (ceiling, character) = text.split_once(':')?;
    Some(rustel_audio::LimiterSettings {
        // The same range the desk's strip offers. Without a floor here a
        // hand-edited `-100:warm` reads as on, the sheet and the strip say
        // -100, and the processor runs at the -60 it clamps to - three
        // numbers for one ceiling.
        threshold_db: ceiling
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|db| db.is_finite() && (MASTER_LIMITER_FLOOR_DB..=0.0).contains(db))?,
        character: rustel_audio::LimiterCharacter::parse(character.trim())?,
    })
}

/// How far one press moves the limiter's ceiling. A tenth is the grain
/// the desk's strip and the file both keep, so the two agree about every
/// value the sheet can reach.
const LIMITER_CEILING_STEP_DB: f32 = 0.5;

/// A ceiling from somewhere that could have said anything - a file, a
/// hand edit - brought into the range the sheet and the desk both offer.
/// A number that is not a number at all falls back to the default rather
/// than to an end of the range, either of which would be a choice nobody
/// made.
pub fn clamp_limiter_ceiling(threshold_db: f32) -> f32 {
    if threshold_db.is_finite() {
        threshold_db.clamp(MASTER_LIMITER_FLOOR_DB, 0.0)
    } else {
        MASTER_LIMITER_DEFAULT_CEILING_DB
    }
}

/// Move the ceiling one step up toward full scale or down toward the floor.
/// The step stops at both ends and does not wrap: a wrap from the floor to
/// full scale would remove the limiter's protection in one press.
fn step_limiter_ceiling(threshold_db: f32, forwards: bool) -> f32 {
    let moved = if forwards {
        threshold_db + LIMITER_CEILING_STEP_DB
    } else {
        threshold_db - LIMITER_CEILING_STEP_DB
    };
    // Back through the same tenth the file keeps, so what the sheet shows
    // is what a reopen reads.
    let moved = (moved * 10.0).round() / 10.0;
    moved.clamp(MASTER_LIMITER_FLOOR_DB, 0.0)
}

/// Each character in turn, round the ladder.
///
/// Off is not a stop on it: whether there is a limiter at all is the
/// switch on the first page, and a mode row that could also turn the
/// thing off would be two controls for one state with nothing to say
/// which of them last had the answer.
fn step_limiter_character(
    current: rustel_audio::LimiterCharacter,
    forwards: bool,
) -> rustel_audio::LimiterCharacter {
    let ladder = rustel_audio::LimiterCharacter::ALL;
    let at = ladder
        .iter()
        .position(|candidate| *candidate == current)
        .unwrap_or(0);
    let next = if forwards {
        (at + 1) % ladder.len()
    } else {
        (at + ladder.len() - 1) % ladder.len()
    };
    ladder[next]
}

/// One press of the arrows, on either opacity.
fn step_opacity(opacity: u8, forwards: bool) -> u8 {
    if forwards {
        opacity.saturating_add(OPACITY_STEP)
    } else {
        opacity.saturating_sub(OPACITY_STEP)
    }
    .min(100)
}

/// The first page: what a musician reaches for during a set, most often
/// first - the clock, the controllers, the look of the screen, the
/// editor, the set - and nothing that needs the manual.
const ROWS: &[Row] = &[
    Row::Quantise,
    Row::MasterLimiter,
    Row::ShowMenu,
    Row::ShowHeader,
    Row::ShowFooter,
    Row::Animation,
    Row::MasterScope,
    Row::MetricDetail,
    Row::BackdropStrength,
    Row::InterfaceOpacity,
    Row::EditorOpacity,
    Row::Zen,
    #[cfg(feature = "hydra")]
    Row::HydraWebcam,
    Row::Highlights,
    Row::HighlightFade,
    Row::EvaluationFlash,
    Row::Minimap,
    Row::ShowScrollbars,
    Row::LineNumbers,
    Row::Wrap,
    Row::Brackets,
    Row::CaretShape,
    Row::SyntaxCheck,
    Row::SetsFolder,
    Row::RecordingsFolder,
    Row::GlobalPrebake,
    Row::LocalPrebake,
    Row::TrimRecordings,
    Row::ShowFullPaths,
];

/// The second page: what is set once and left - how a play meets sounds
/// still loading, how the terminal draws, how sliders travel, where the
/// panels dock, what the sample memory and cache may take.
const ADVANCED_ROWS: &[Row] = &[
    Row::LoadMode,
    Row::OutputLatency,
    Row::MaxPolyphony,
    Row::PianoSound,
    Row::PianoVolume,
    Row::MasterLimiterCharacter,
    Row::MasterLimiterCeiling,
    Row::MasterLimiterMakeup,
    Row::Rendering,
    Row::FrameRate,
    #[cfg(feature = "hydra")]
    Row::BackdropSmoothing,
    Row::SliderSmoothing,
    Row::FrequencySliderLog,
    Row::SetPanelSide,
    Row::MixerEdge,
    Row::VizEdgeOne,
    Row::VizEdgeTwo,
    Row::SampleCeiling,
    Row::PreviewBudget,
    Row::UnusedSampleIdle,
];

/// The reference page: which kinds of entry the reference column and the
/// editor's suggestions offer.
const REFERENCE_ROWS: &[Row] = &[
    Row::ReferenceCategory(Category::Osc),
    Row::ReferenceCategory(Category::Serial),
    Row::ReferenceCategory(Category::FmMatrix),
    Row::ReferenceCategory(Category::Bind),
    Row::ReferenceCategory(Category::Internals),
];

/// Cache controls at the top of the Sources page, before imports and
/// shipped packs.
const SOURCES_CONTROLS: &[Row] = &[
    Row::PrecacheSources,
    Row::CacheDefaults,
    Row::RefreshSources,
    Row::SampleCache,
];

/// How many selectable rows precede the imports on Sources.
pub(super) const SOURCE_CONTROL_COUNT: usize = SOURCES_CONTROLS.len();

/// The terminal profile belongs with the bindings it determines.
pub(super) const TERMINAL_PROFILE_ROW: usize = 0;
pub(super) const KEYBIND_ACTION_START: usize = 1;
/// The final keybind row is an action, after every binding.
pub(super) const RESET_KEYBINDS_ROW: usize = KEYBIND_ACTION_START + BindAction::ALL.len();

pub(super) fn keybind_action_at(row: usize) -> Option<BindAction> {
    row.checked_sub(KEYBIND_ACTION_START)
        .and_then(|index| BindAction::ALL.get(index).copied())
}

pub(super) fn normalize_max_polyphony(value: usize) -> usize {
    if value == 0 {
        rustel_audio::MAX_POLYPHONY
    } else {
        value.min(rustel_audio::MAX_CONFIGURABLE_POLYPHONY)
    }
}

fn step_max_polyphony(current: usize, forwards: bool) -> usize {
    const LADDER: [usize; 5] = [32, 64, 128, 192, 256];
    let current = normalize_max_polyphony(current);
    if forwards {
        LADDER
            .into_iter()
            .find(|value| *value > current)
            .unwrap_or(current)
    } else {
        LADDER
            .into_iter()
            .rev()
            .find(|value| *value < current)
            .unwrap_or(current)
    }
}

fn max_polyphony_readout(default: usize, score_override: Option<usize>) -> String {
    if let Some(score) = score_override {
        format!("{default} (score {score})")
    } else if default == rustel_audio::MAX_POLYPHONY {
        format!("{default} (default)")
    } else {
        default.to_string()
    }
}

fn step_terminal_profile(current: Option<&str>, forwards: bool) -> Option<String> {
    let profiles: Vec<_> = super::terminal::conflicts::known().collect();
    let at = current
        .and_then(|current| {
            profiles
                .iter()
                .position(|name| name.eq_ignore_ascii_case(current))
        })
        .map_or(0, |at| at + 1);
    let len = profiles.len() + 1;
    let next = if forwards {
        (at + 1) % len
    } else {
        (at + len - 1) % len
    };
    next.checked_sub(1).map(|at| profiles[at].to_owned())
}

pub(super) fn terminal_profile_label(settings: &UiSettings, features: &TerminalFeatures) -> String {
    settings
        .terminal_profile
        .as_deref()
        .map(|profile| super::terminal::conflicts::effective_profile(&features.name, Some(profile)))
        .unwrap_or_else(|| {
            if features.name.is_empty() {
                "automatic".to_owned()
            } else {
                format!("automatic ({})", features.name)
            }
        })
}

/// The Terminal row's value. A profile pinned over another detected emulator
/// names both, because the pinned list then decides which keys Studio avoids
/// in this terminal.
pub(super) fn terminal_row_label(settings: &UiSettings, features: &TerminalFeatures) -> String {
    let profile = terminal_profile_label(settings, features);
    match super::terminal::conflicts::pinned_over(
        &features.name,
        settings.terminal_profile.as_deref(),
    ) {
        Some(detected) => format!("{profile} (this terminal: {detected})"),
        None => profile,
    }
}

impl Row {
    fn group(self) -> &'static str {
        match self {
            Self::Quantise | Self::MasterLimiter | Self::LoadMode => "Playback",

            // The one latency knob: what the OUTPUT costs is a
            // machine-level choice, set once like a DAW's buffer size.
            Self::OutputLatency | Self::MaxPolyphony => "Audio out",
            Self::PianoSound | Self::PianoVolume => "Computer piano",

            Self::MasterLimiterCharacter
            | Self::MasterLimiterCeiling
            | Self::MasterLimiterMakeup => "Limiter",

            Self::Animation
            | Self::MasterScope
            | Self::MetricDetail
            | Self::BackdropStrength
            | Self::InterfaceOpacity
            | Self::EditorOpacity
            | Self::ShowMenu
            | Self::ShowHeader
            | Self::ShowFooter
            | Self::Zen => "Look",
            #[cfg(feature = "hydra")]
            Self::HydraWebcam => "Look",
            Self::Highlights
            | Self::HighlightFade
            | Self::EvaluationFlash
            | Self::Minimap
            | Self::ShowScrollbars
            | Self::LineNumbers
            | Self::Wrap
            | Self::Brackets
            | Self::CaretShape
            | Self::SyntaxCheck => "Editor",
            Self::SetsFolder
            | Self::RecordingsFolder
            | Self::GlobalPrebake
            | Self::LocalPrebake
            | Self::TrimRecordings
            | Self::ShowFullPaths => "Set",
            Self::Rendering | Self::FrameRate => "Rendering",
            #[cfg(feature = "hydra")]
            Self::BackdropSmoothing => "Rendering",
            Self::SliderSmoothing | Self::FrequencySliderLog => "Sliders",
            Self::SetPanelSide | Self::MixerEdge | Self::VizEdgeOne | Self::VizEdgeTwo => "Panels",
            Self::SampleCeiling | Self::PreviewBudget | Self::UnusedSampleIdle => "Sample memory",
            Self::PrecacheSources
            | Self::CacheDefaults
            | Self::RefreshSources
            | Self::SampleCache => "Sample cache",
            Self::ReferenceCategory(_) => "Listed in the reference",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::ShowFullPaths => "full paths",
            Self::Rendering => "rendering",
            Self::FrameRate => "frame rate",
            Self::Quantise => "launch on",
            Self::LoadMode => "load mode",
            Self::Animation => "visualizers",
            Self::Highlights => "highlights",
            Self::HighlightFade => "highlight fade",
            Self::EvaluationFlash => "flashing on evaluation",
            Self::MetricDetail => "metric detail",
            Self::OutputLatency => "audio out latency",
            Self::MaxPolyphony => "max polyphony",
            Self::PianoSound => "piano sound",
            Self::PianoVolume => "piano volume",
            Self::MasterLimiter => "master limiter",
            Self::MasterLimiterCharacter => "default mode",
            Self::MasterLimiterCeiling => "default ceiling",
            Self::MasterLimiterMakeup => "makeup gain",
            Self::Minimap => "minimap",
            Self::ShowScrollbars => "show scrollbars",
            Self::LineNumbers => "line numbers",
            Self::Wrap => "word wrap",
            Self::MasterScope => "master scope",
            Self::Brackets => "bracket match",
            Self::CaretShape => "caret shape",
            Self::SyntaxCheck => "syntax check",
            Self::SliderSmoothing => "slider smoothing",
            Self::FrequencySliderLog => "frequency travel",

            #[cfg(feature = "hydra")]
            Self::BackdropSmoothing => "hydra smoothing",
            #[cfg(feature = "hydra")]
            Self::HydraWebcam => "Hydra webcam",
            Self::BackdropStrength => "visuals opacity",
            Self::InterfaceOpacity => "ui opacity",
            Self::EditorOpacity => "editor opacity",
            Self::ShowMenu => "menu bar",
            Self::ShowHeader => "header",
            Self::ShowFooter => "footer",
            Self::Zen => "zen mode",
            Self::SetPanelSide => "set panel side",
            Self::MixerEdge => "mixer edge",
            Self::VizEdgeOne => "visuals 1 edge",
            Self::VizEdgeTwo => "visuals 2 edge",
            Self::SetsFolder => "sets folder",
            Self::RecordingsFolder => "recordings folder",
            Self::GlobalPrebake => "global prebake",
            Self::LocalPrebake => "local prebake",
            Self::TrimRecordings => "trim recordings",
            Self::SampleCeiling => "biggest sound",
            Self::PreviewBudget => "preview RAM",
            Self::UnusedSampleIdle => "drop unused",
            Self::PrecacheSources => "fetch imports",
            Self::CacheDefaults => "cache all",
            Self::RefreshSources => "refresh all",
            Self::SampleCache => "clear cache",
            Self::ReferenceCategory(Category::Osc) => "OSC / SuperDirt",
            Self::ReferenceCategory(Category::Serial) => "serial",
            Self::ReferenceCategory(Category::FmMatrix) => "FM routing matrix",
            Self::ReferenceCategory(Category::Bind) => "bind & join",
            Self::ReferenceCategory(Category::Internals) => "pattern internals",
        }
    }

    /// The prebake this row opens, when it opens one. An action row rather
    /// than a switch: there is nothing here to step through.
    fn opens_prebake(self) -> Option<PrebakeScope> {
        match self {
            Self::GlobalPrebake => Some(PrebakeScope::Global),
            Self::LocalPrebake => Some(PrebakeScope::Local),
            _ => None,
        }
    }

    fn explain(self) -> &'static str {
        match self {
            Self::ShowFullPaths => "folder paths in messages, not just filenames",
            Self::Rendering => "how glyphs are drawn \u{b7} Automatic favours speed",
            Self::FrameRate => "repaints a second \u{b7} drawing costs more than sound",
            Self::Quantise => "every play waits for this line · off plays now",
            Self::LoadMode => "wait: plays once its sounds load · async: skips late notes",
            Self::Animation => "pianoroll, scope and friends draw",
            Self::Highlights => "sounding events marked in the score",
            Self::HighlightFade => "how long a sounding mark lingers",
            Self::EvaluationFlash => {
                "full: immediate, double on failure · on success: successes only · off: no flashes"
            }
            Self::MetricDetail => "header counters · advanced adds the machine's CPU",
            Self::OutputLatency => "frames \u{b7} raise it if audio crackles; adds latency",
            Self::MasterLimiter => "a brickwall on new sets \u{b7} the mixer changes this set",
            Self::MasterLimiterCharacter => "how a limiter added from here works",
            Self::MasterLimiterCeiling => "how far under full scale a new one holds",
            Self::MasterLimiterMakeup => "limiting changes the sound, not the loudness",
            Self::Minimap => "the Braille overview at the pane's right edge",
            Self::ShowScrollbars => "scrollbars appear only when the score overflows",
            Self::LineNumbers => "the numbers down the left edge of the score",
            Self::Wrap => "long lines continue on the next row",
            Self::MasterScope => "the small spectrum in the footer",
            Self::Brackets => "the bracket under the caret and its partner",
            Self::CaretShape => "bar, block or underline · steady or blinking",
            Self::SyntaxCheck => "full: as you type · on update: only when refused",
            Self::FrequencySliderLog => "low frequencies get more travel on Hz sliders",
            Self::SliderSmoothing => "ramps mouse changes \u{b7} keys stay immediate",
            #[cfg(feature = "hydra")]
            Self::BackdropSmoothing => "steadies the hydra backdrop's cell tiers",
            #[cfg(feature = "hydra")]
            Self::HydraWebcam => "Enter shows the camera before it is switched on",
            Self::BackdropStrength => "how strongly pictures and effects appear",
            Self::InterfaceOpacity => "how solid the header, footer and panels sit",
            Self::EditorOpacity => "how solid the code area sits over it",
            Self::ShowMenu => "File, Edit and the rest of the top row",
            Self::ShowHeader => "the rustel PLAYING tempo line · not the tabs",
            Self::ShowFooter => "meter, orbits and device chips · the status line stays",
            Self::Zen => "score fills the frame · panels float over it · visuals docks close",
            Self::SetPanelSide => "which edge the set panel docks on",
            Self::MixerEdge => "which edge the mixer docks on",
            Self::VizEdgeOne => "where the first visuals dock sits",
            Self::VizEdgeTwo => "where the second visuals dock sits",
            Self::SetsFolder => "where new sets are made · Enter chooses one",
            Self::RecordingsFolder => "where finished takes are saved · Enter chooses one",
            Self::GlobalPrebake => "setup run before every set · Enter opens it",
            Self::LocalPrebake => "this set's own setup · Enter opens it",
            Self::TrimRecordings => "a take's silence is trimmed before it is saved",
            Self::PianoSound => "instrument played by the F12 keyboard",
            Self::PianoVolume => "F12 volume · 100% is the original level · default 130%",
            Self::MaxPolyphony => {
                "default; scores may override. More voices use more CPU and may cause crackles"
            }
            Self::SampleCeiling => "longer than this is refused as not music",
            Self::PreviewBudget => "decoded samples no live tab names",
            Self::UnusedSampleIdle => "idle time before unused samples are dropped",
            Self::PrecacheSources => "download imported sounds to disk now, not on first play",
            Self::CacheDefaults => "Enter caches every remote pack onto disk",
            Self::RefreshSources => "Enter refetches every remote pack's list",
            Self::SampleCache => "Enter twice empties downloaded samples",
            Self::ReferenceCategory(Category::Osc) => {
                "osc() and controls only SuperDirt reads · tag:osc lists them anyway"
            }
            Self::ReferenceCategory(Category::Serial) => {
                "serial() output · tag:serial lists it anyway"
            }
            Self::ReferenceCategory(Category::FmMatrix) => {
                "fmi12…fmi88: route one operator into another · tag:fm_matrix lists them anyway"
            }
            Self::ReferenceCategory(Category::Bind) => {
                "bind, join, appLeft: for writing pattern functions · tag:bind lists them anyway"
            }
            Self::ReferenceCategory(Category::Internals) => {
                "withHap, splitQueries and other hap plumbing · tag:internals lists them anyway"
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DisplayRow {
    /// A group's title. `first` opens the box; every later one is the rule
    /// that closes the group above and opens this one in the same row,
    /// which is a row per boundary rather than two.
    Header {
        label: &'static str,
        first: bool,
    },
    Control(usize),
    Footer,
    /// A blank row above a group's rule. One row of air is what tells the
    /// eye where a group ends when the rule between two of them is a
    /// single line rather than two facing borders.
    Gap,
}

fn grouped_rows(rows: &[Row]) -> Vec<DisplayRow> {
    let mut display = Vec::new();
    let mut previous = None;
    for (index, row) in rows.iter().enumerate() {
        let group = row.group();
        if previous != Some(group) {
            if previous.is_some() {
                display.push(DisplayRow::Gap);
            }
            display.push(DisplayRow::Header {
                label: group,
                first: previous.is_none(),
            });
            previous = Some(group);
        }
        display.push(DisplayRow::Control(index));
    }
    if previous.is_some() {
        display.push(DisplayRow::Footer);
    }
    display
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SettingsPage {
    #[default]
    Settings,
    Advanced,
    /// The twelve mapping slots: which knob or stick drives which fader.
    Mapping,
    /// The learnt shortcuts: Enter on a row, press the chord.
    Keybinds,
    /// The folders and packs the player imported.
    Sources,
    /// Which kinds of entry the reference and the suggestions offer.
    Reference,
    About,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct SettingsPosition {
    selected: usize,
    first: usize,
    hold_scroll: bool,
}

/// Navigation lasts for this Studio instance, independently of preferences.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct SettingsNavigation {
    page: SettingsPage,
    positions: [SettingsPosition; 7],
}

impl SettingsNavigation {
    pub fn open(self) -> SettingsSheet {
        let position = self.positions[self.page as usize];
        SettingsSheet {
            selected: position.selected,
            first: position.first,
            hold_scroll: position.hold_scroll,
            page: self.page,
            navigation: self,
            ..SettingsSheet::default()
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SettingsSheet {
    pub selected: usize,
    page: SettingsPage,
    navigation: SettingsNavigation,
    /// How many sources the sources page is showing. The sheet does not
    /// own the list - the preferences do - but it has to know how far the
    /// cursor can go, and passing that to every key would be a parameter
    /// on thirty call sites that have no sources in them.
    pub source_count: usize,
    /// How many shipped packs follow the imports on the sources page. The
    /// cursor walks both lists as one; a row past `source_count` is a pack
    /// the studio ships with, which can be cached but not edited.
    pub default_count: usize,
    /// How many keybind rows the keybinds page is showing, for the same
    /// reason as `source_count`.
    pub keybind_count: usize,
    /// The first line of the page drawn, kept from key to key so walking
    /// through the middle of a page leaves it still. The page keeps a margin
    /// of rows around the selection; see [`super::scroll`].
    first: usize,
    /// The selection was last put there by a click. Until a key moves it the
    /// page scrolls only to keep it on screen, not to keep the margin: the
    /// clicked row stays under the pointer.
    pub hold_scroll: bool,
    /// Enter on the sample-cache row asked once: the next Enter empties
    /// it, anything else calls it off - emptying gigabytes is not a
    /// reflexive press.
    pub confirm_clear_cache: bool,
    /// A second Enter on the reset row discards every custom shortcut.
    pub confirm_reset_keybinds: bool,
    /// Enter on Hydra webcam opened the live preview. Scrolling onto the
    /// row alone must not: the camera coming up mid-browse is the nuisance
    /// Enter is here to avoid.
    #[cfg(feature = "hydra")]
    pub webcam_preview: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsAction {
    Nothing,
    Changed,
    Close,
    /// Enter on a prebake row: the sheet hands over to the editor.
    OpenPrebake(PrebakeScope),
    /// Enter on the sets folder row: the sheet hands over to a picker.
    ChooseSetsFolder,
    /// Enter on the recordings folder row: the sheet hands over to a picker.
    ChooseRecordingsFolder,
    /// Enter on the sample cache row: empty it. Armed by a first Enter;
    /// the second Enter is the one that empties.
    ClearSampleCache,
    /// First Enter on the sample cache row: ask again before emptying.
    AskClearSampleCache,
    /// First Enter on the Hydra webcam row: the live picture came up so the
    /// row can be judged by what it shows, but nothing is kept yet.
    #[cfg(feature = "hydra")]
    OpenWebcamPreview,
    /// Enter on the cache-all row: fetch every remote pack onto disk,
    /// defaults and user URLs, skipping what is there already.
    CacheWholeLibrary,
    /// Enter on the refresh-all row: refetch every remote pack's list.
    RefreshAllSources,
    /// `c` on a shipped pack's row: fetch that pack alone onto disk.
    CacheDefaultSource(usize),
    /// `c` on a remote user import: fetch that pack alone onto disk.
    CacheImportSource(usize),
    /// Enter on a mapping slot: wait for a control to move, and make it
    /// this slot's.
    LearnSlot(usize),
    /// Space or Delete on a mapping slot: unbind it.
    ClearSlot(usize),
    /// `t` on a mapping slot: the next way of reading its control.
    StepTakeover(usize, bool),
    /// The sources page: add one, or act on the row the cursor is on.
    AddSource,
    EditSource(usize),
    RemoveSource(usize),
    ToggleSource(usize),
    /// `r` on a user import: alias one of its banks (name clashes).
    RenameSourceBank(usize),
    /// Enter on a source row: fetch what it holds into the cache now.
    RefreshSource(usize),
    /// Enter on a keybind row: wait for a chord, and make it this
    /// action's. The chord itself arrives through the app's learn state.
    LearnKeybind(BindAction),
    /// Delete or Backspace on a keybind row: the studio's own chord
    /// back. Space is not among them - it types spaces, and a reflexive
    /// press on a row must not clear a binding.
    ClearKeybind(BindAction),
    /// Ask before replacing all overrides with this terminal's defaults.
    AskResetKeybinds,
    /// The second Enter on the reset row confirmed replacing all overrides.
    ResetKeybinds,
    /// A key the sheet is not holding while the keybinds page is up: it
    /// goes back to the app, which is listening for the chord a learn is
    /// arming. Only the keybinds page produces it.
    Capture,
    /// The sheet has no use for this key; it belongs to the score.
    Ignored,
}

impl SettingsSheet {
    pub(super) const fn shows_settings(self) -> bool {
        matches!(
            self.page,
            SettingsPage::Settings | SettingsPage::Advanced | SettingsPage::Reference
        )
    }

    /// The mapping page: a grid of slot boxes, so its keys are a grid's.
    pub(super) const fn shows_mapping(self) -> bool {
        matches!(self.page, SettingsPage::Mapping)
    }

    /// The keybinds page: a list of actions, so its keys are a list's.
    pub(super) const fn shows_keybinds(self) -> bool {
        matches!(self.page, SettingsPage::Keybinds)
    }

    /// Arrows walk the grid the way it reads - across, then down - and
    /// Enter learns the slot under the cursor. Space and Delete unbind it.
    /// Esc and Tab stay the sheet's, so the page cannot trap anyone.
    fn mapping_key(&mut self, code: crossterm::event::KeyCode) -> SettingsAction {
        use crossterm::event::KeyCode;
        let columns = usize::from(MAPPING_COLUMNS);
        let at = self.selected.min(MAPPING_SLOTS - 1);
        let step = |at: usize, delta: isize| {
            (at as isize + delta).rem_euclid(MAPPING_SLOTS as isize) as usize
        };
        match code {
            KeyCode::Left => {
                self.selected = step(at, -1);
                SettingsAction::Nothing
            }
            KeyCode::Right => {
                self.selected = step(at, 1);
                SettingsAction::Nothing
            }
            KeyCode::Up => {
                self.selected = step(at, -(columns as isize));
                SettingsAction::Nothing
            }
            KeyCode::Down => {
                self.selected = step(at, columns as isize);
                SettingsAction::Nothing
            }
            KeyCode::Enter => SettingsAction::LearnSlot(at),
            KeyCode::Char(' ') | KeyCode::Delete | KeyCode::Backspace => {
                SettingsAction::ClearSlot(at)
            }
            // What a turn of this slot's control means to its fader. Per
            // slot, because the hardware is: an endless encoder and a
            // sprung fader want opposite answers.
            KeyCode::Char('t' | 'T') => SettingsAction::StepTakeover(at, true),
            KeyCode::Char('r' | 'R') => SettingsAction::StepTakeover(at, false),
            _ => SettingsAction::Ignored,
        }
    }

    /// The sources page: a list you add to, alias and turn off, so its
    /// keys are a list's rather than a switch panel's.
    ///
    /// Tab still pages, and Esc still closes, so the page cannot trap
    /// anyone who wandered onto it.
    fn source_key(
        &mut self,
        code: crossterm::event::KeyCode,
        settings: &mut UiSettings,
    ) -> SettingsAction {
        use crossterm::event::KeyCode;
        // One list to walk: the cache controls, the imports, then the
        // packs the studio ships with. Only the imports are edited.
        let controls = SOURCE_CONTROL_COUNT;
        let imported = self.source_count;
        let len = controls + imported + self.default_count;
        let at = self.selected.min(len.saturating_sub(1));
        let on_control = at < controls;
        let on_import = at >= controls && at < controls + imported;
        let on_pack = at >= controls + imported;
        match code {
            KeyCode::Up if len > 0 => {
                self.confirm_clear_cache = false;
                self.selected = (at + len - 1) % len;
                SettingsAction::Nothing
            }
            KeyCode::Down if len > 0 => {
                self.confirm_clear_cache = false;
                self.selected = (at + 1) % len;
                SettingsAction::Nothing
            }
            // The letters are the ones already on the row's hint, and none
            // of them is a page key: a sources page is not a text field.
            KeyCode::Char('a') | KeyCode::Char('+') => {
                self.confirm_clear_cache = false;
                SettingsAction::AddSource
            }
            KeyCode::Char('c') if on_import => {
                self.confirm_clear_cache = false;
                SettingsAction::CacheImportSource(at - controls)
            }
            KeyCode::Char('e') if on_import => {
                self.confirm_clear_cache = false;
                SettingsAction::EditSource(at - controls)
            }
            KeyCode::Char('c') if on_pack && self.default_count > 0 => {
                self.confirm_clear_cache = false;
                SettingsAction::CacheDefaultSource(at - controls - imported)
            }
            KeyCode::Enter if on_import => {
                self.confirm_clear_cache = false;
                SettingsAction::RefreshSource(at - controls)
            }
            KeyCode::Enter if on_control && SOURCES_CONTROLS[at] == Row::CacheDefaults => {
                self.confirm_clear_cache = false;
                SettingsAction::CacheWholeLibrary
            }
            KeyCode::Enter if on_control && SOURCES_CONTROLS[at] == Row::RefreshSources => {
                self.confirm_clear_cache = false;
                SettingsAction::RefreshAllSources
            }
            KeyCode::Enter if on_control && SOURCES_CONTROLS[at] == Row::SampleCache => {
                if self.confirm_clear_cache {
                    self.confirm_clear_cache = false;
                    SettingsAction::ClearSampleCache
                } else {
                    self.confirm_clear_cache = true;
                    SettingsAction::AskClearSampleCache
                }
            }
            KeyCode::Enter if on_pack || on_control => {
                self.confirm_clear_cache = false;
                SettingsAction::Nothing
            }
            // A switch, so the same keys that change every other one
            // change this: left, right and Space all flip it. The rest
            // of the page is a list, and those arrows walk nothing there.
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
                if on_control && SOURCES_CONTROLS[at] == Row::PrecacheSources =>
            {
                self.confirm_clear_cache = false;
                let before = settings.precache_sources;
                settings.precache_sources = !settings.precache_sources;
                if settings.precache_sources != before {
                    SettingsAction::Changed
                } else {
                    SettingsAction::Nothing
                }
            }
            KeyCode::Char('r') if on_import => {
                self.confirm_clear_cache = false;
                SettingsAction::RenameSourceBank(at - controls)
            }
            // A shipped pack is not the player's to remove, turn off,
            // alias or reorder; the key is taken so it cannot fall through
            // to the score, and nothing happens.
            KeyCode::Char('d' | 'e' | ' ' | 'r' | '[' | ']')
            | KeyCode::Delete
            | KeyCode::Backspace
            | KeyCode::Left
            | KeyCode::Right
                if on_pack =>
            {
                self.confirm_clear_cache = false;
                SettingsAction::Nothing
            }
            KeyCode::Char('d') | KeyCode::Delete | KeyCode::Backspace if on_import => {
                self.confirm_clear_cache = false;
                SettingsAction::RemoveSource(at - controls)
            }
            KeyCode::Char(' ') if on_import => {
                self.confirm_clear_cache = false;
                SettingsAction::ToggleSource(at - controls)
            }
            // Order does not settle name clashes; `r` aliases a bank
            // instead. The move keys do nothing here.
            KeyCode::Char('[') | KeyCode::Char(']') | KeyCode::Left | KeyCode::Right
                if on_import =>
            {
                self.confirm_clear_cache = false;
                SettingsAction::Nothing
            }
            _ => SettingsAction::Ignored,
        }
    }

    /// Whether the page's rows are sources rather than settings: they are
    /// a list the player edits, not a fixed set of switches.
    pub(super) const fn shows_sources(self) -> bool {
        matches!(self.page, SettingsPage::Sources)
    }

    /// The keybinds page is a list of learnable rows. The arrows move the
    /// cursor, Enter arms the learn, and Delete or Backspace restores the
    /// studio's own chord. Space does not clear a binding, because a
    /// reflexive press must not remove one. Every other key returns
    /// `Capture`: while a learn is armed, the chord is the next key the
    /// app sees, so the key must reach the capture path.
    fn keybind_key(
        &mut self,
        code: crossterm::event::KeyCode,
        settings: &mut UiSettings,
    ) -> SettingsAction {
        use crossterm::event::KeyCode;
        let len = self.keybind_count;
        let at = self.selected.min(len.saturating_sub(1));
        // The rows past the learnable table (the panels' Alt chords) are
        // listed to be found, not rebound: they walk like any row, and a
        // learn or a clear has nothing to land on there.
        let action = keybind_action_at(at);
        if code != KeyCode::Enter {
            self.confirm_reset_keybinds = false;
        }
        match code {
            KeyCode::Up if len > 0 => {
                self.selected = (at + len - 1) % len;
                SettingsAction::Nothing
            }
            KeyCode::Down if len > 0 => {
                self.selected = (at + 1) % len;
                SettingsAction::Nothing
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Enter
                if len > 0 && at == TERMINAL_PROFILE_ROW =>
            {
                settings.terminal_profile = step_terminal_profile(
                    settings.terminal_profile.as_deref(),
                    code != KeyCode::Left,
                );
                SettingsAction::Changed
            }
            KeyCode::Delete | KeyCode::Backspace if len > 0 && at == TERMINAL_PROFILE_ROW => {
                if settings.terminal_profile.take().is_some() {
                    SettingsAction::Changed
                } else {
                    SettingsAction::Nothing
                }
            }
            KeyCode::Enter if len > 0 && at == RESET_KEYBINDS_ROW => {
                if self.confirm_reset_keybinds {
                    self.confirm_reset_keybinds = false;
                    SettingsAction::ResetKeybinds
                } else {
                    self.confirm_reset_keybinds = true;
                    SettingsAction::AskResetKeybinds
                }
            }
            KeyCode::Enter if len > 0 => match action {
                Some(action) => SettingsAction::LearnKeybind(action),
                None => SettingsAction::Nothing,
            },
            KeyCode::Delete | KeyCode::Backspace if len > 0 => match action {
                Some(action) => SettingsAction::ClearKeybind(action),
                None => SettingsAction::Nothing,
            },
            _ => SettingsAction::Capture,
        }
    }

    fn rows(self) -> &'static [Row] {
        match self.page {
            SettingsPage::Settings => ROWS,
            SettingsPage::Advanced => ADVANCED_ROWS,
            SettingsPage::Reference => REFERENCE_ROWS,
            SettingsPage::Mapping
            | SettingsPage::Keybinds
            | SettingsPage::Sources
            | SettingsPage::About => &[],
        }
    }

    #[cfg(feature = "hydra")]
    pub(super) fn webcam_row_selected(self) -> bool {
        // The row is only on the main page. The check reads the active
        // page's rows, not every row the sheet knows. The preview is then
        // not requested after a click onto advanced, where the row is not
        // drawn.
        self.shows_settings() && matches!(self.rows().get(self.selected), Some(Row::HydraWebcam))
    }

    /// Live camera thumbnail: Enter on the Hydra webcam row opened it, and
    /// the cursor is still on that row.
    #[cfg(feature = "hydra")]
    pub(super) fn webcam_preview_open(self) -> bool {
        self.webcam_preview && self.webcam_row_selected()
    }

    /// Move the cursor; leaving the webcam row puts its preview away.
    pub(super) fn select(&mut self, at: usize) {
        if self.selected != at {
            self.confirm_reset_keybinds = false;
        }
        self.selected = at;
        #[cfg(feature = "hydra")]
        if !self.webcam_row_selected() {
            self.webcam_preview = false;
        }
    }

    /// Page through the rows that actually fit, including group headings.
    /// The app calls this only after an armed keybinding learn has captured
    /// its key, so PageUp and PageDown can still be learnt as shortcuts.
    pub fn key_in_frame(
        &mut self,
        code: crossterm::event::KeyCode,
        settings: &mut UiSettings,
        features: &TerminalFeatures,
        available: Rect,
    ) -> SettingsAction {
        use crossterm::event::KeyCode;
        let about_arrow =
            self.page == SettingsPage::About && matches!(code, KeyCode::Up | KeyCode::Down);
        if !matches!(
            code,
            KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End
        ) && !about_arrow
        {
            return self.key(code, settings, features);
        }
        self.confirm_clear_cache = false;
        self.confirm_reset_keybinds = false;
        self.hold_scroll = false;
        let rows = self
            .geometry_for(available)
            .map(|(_, rows)| rows)
            .unwrap_or_default();
        let shown = usize::from(rows.height).max(1);
        let forwards = matches!(code, KeyCode::PageDown | KeyCode::Down | KeyCode::End);
        let target = |at: usize, last: usize, step: usize| match code {
            KeyCode::Home => 0,
            KeyCode::End => last,
            _ if forwards => at.saturating_add(step).min(last),
            _ => at.saturating_sub(step).min(last),
        };
        if self.page == SettingsPage::About {
            let last = about_content_height(rows.width).saturating_sub(shown);
            self.first = target(
                self.first.min(last),
                last,
                if about_arrow { 1 } else { shown },
            );
            return SettingsAction::Nothing;
        }
        let choices: Vec<(usize, usize)> = match self.page {
            SettingsPage::Settings | SettingsPage::Advanced | SettingsPage::Reference => {
                grouped_rows(self.rows())
                    .iter()
                    .enumerate()
                    .filter_map(|(line, row)| match row {
                        DisplayRow::Control(index) => Some((line, *index)),
                        _ => None,
                    })
                    .collect()
            }
            SettingsPage::Sources => source_lines(self.source_count, self.default_count)
                .iter()
                .enumerate()
                .filter_map(|(line, row)| match row {
                    SourceLine::Control(index) | SourceLine::Row(index) => Some((line, *index)),
                    _ => None,
                })
                .collect(),
            SettingsPage::Keybinds => (0..self.keybind_count)
                .map(|index| (index, index))
                .collect(),
            SettingsPage::Mapping => {
                let columns = usize::from(MAPPING_COLUMNS);
                let step = (shown / usize::from(SLOT_HEIGHT)).max(1) * columns;
                self.select(target(
                    self.selected.min(MAPPING_SLOTS - 1),
                    MAPPING_SLOTS - 1,
                    step,
                ));
                self.settle_scroll(available);
                return SettingsAction::Nothing;
            }
            SettingsPage::About => unreachable!(),
        };
        if let (Some(first), Some(last)) = (choices.first(), choices.last()) {
            let at = choices
                .iter()
                .find(|(_, index)| *index == self.selected)
                .map_or(first.0, |(line, _)| *line);
            let line = target(at, last.0, shown);
            let choice = if forwards {
                choices.iter().find(|(at, _)| *at >= line).unwrap_or(last)
            } else {
                choices
                    .iter()
                    .rev()
                    .find(|(at, _)| *at <= line)
                    .unwrap_or(first)
            };
            self.select(choice.1);
        } else {
            self.select(0);
        }
        self.settle_scroll(available);
        SettingsAction::Nothing
    }

    pub fn key(
        &mut self,
        code: crossterm::event::KeyCode,
        settings: &mut UiSettings,
        features: &TerminalFeatures,
    ) -> SettingsAction {
        let before = (self.selected, self.page);
        let action = self.apply_key(code, settings, features);
        if (self.selected, self.page) != before {
            self.confirm_reset_keybinds = false;
        }
        // A key that moved the selection within a page walks it the
        // keyboard's way, margin and all. One that changed the selected
        // setting in place leaves a clicked row where the pointer left it.
        if self.page == before.1 && self.selected != before.0 {
            self.hold_scroll = false;
        }
        action
    }

    fn apply_key(
        &mut self,
        code: crossterm::event::KeyCode,
        settings: &mut UiSettings,
        features: &TerminalFeatures,
    ) -> SettingsAction {
        use crossterm::event::KeyCode;
        let change = |settings: &mut UiSettings, row: Row, forwards: bool| {
            // A switch has two states, so every key that changes it flips
            // it. A second press of the same arrow undoes the first.
            let set_switch = |value: &mut bool| {
                *value = !*value;
            };
            match row {
                Row::ShowFullPaths => set_switch(&mut settings.show_full_paths),
                Row::Rendering => settings.rendering = settings.rendering.step(forwards, features),
                // The ladder itself reads fast first, so its "forwards" is
                // a step down. The arrows read the other way: right speeds
                // the screen up, left slows it down.
                Row::FrameRate => settings.frame_rate = settings.frame_rate.step(!forwards),
                Row::Quantise => settings.quantise = settings.quantise.step(forwards),
                Row::LoadMode => settings.load_mode = settings.load_mode.step(),
                Row::Animation => set_switch(&mut settings.animation),
                Row::Highlights => set_switch(&mut settings.highlights),
                Row::EvaluationFlash => {
                    settings.evaluation_flash = settings.evaluation_flash.step(forwards);
                }
                Row::TrimRecordings => set_switch(&mut settings.trim_recordings),
                Row::HighlightFade => {
                    settings.highlight_fade = settings.highlight_fade.step(forwards);
                }
                Row::MetricDetail => {
                    settings.metric_detail = settings.metric_detail.step(forwards);
                }
                Row::OutputLatency => {
                    settings.output_latency = settings.output_latency.step(forwards);
                }
                Row::PianoSound => settings.piano_sound = settings.piano_sound.step(forwards),
                Row::PianoVolume => {
                    settings.piano_volume = if forwards {
                        settings
                            .piano_volume
                            .saturating_add(10)
                            .min(super::piano::MAX_PIANO_VOLUME)
                    } else {
                        settings.piano_volume.saturating_sub(10)
                    }
                }
                Row::MaxPolyphony => {
                    settings.max_polyphony = step_max_polyphony(settings.max_polyphony, forwards)
                }
                Row::MasterLimiter => set_switch(&mut settings.master_limiter_on),
                Row::MasterLimiterCharacter => {
                    settings.master_limiter_character =
                        step_limiter_character(settings.master_limiter_character, forwards);
                }
                Row::MasterLimiterCeiling => {
                    settings.master_limiter_ceiling_db =
                        step_limiter_ceiling(settings.master_limiter_ceiling_db, forwards);
                }
                Row::MasterLimiterMakeup => set_switch(&mut settings.master_limiter_makeup),
                Row::Minimap => set_switch(&mut settings.minimap),
                Row::ShowScrollbars => set_switch(&mut settings.show_scrollbars),
                Row::LineNumbers => set_switch(&mut settings.line_numbers),
                Row::Wrap => set_switch(&mut settings.wrap),
                Row::MasterScope => set_switch(&mut settings.master_scope),
                Row::Brackets => set_switch(&mut settings.brackets),
                Row::CaretShape => {
                    settings.caret_shape = settings.caret_shape.step(forwards);
                }
                Row::SyntaxCheck => settings.syntax_check = settings.syntax_check.step(forwards),
                Row::SliderSmoothing => set_switch(&mut settings.slider_smoothing),
                Row::FrequencySliderLog => set_switch(&mut settings.frequency_slider_log),
                #[cfg(feature = "hydra")]
                Row::BackdropSmoothing => set_switch(&mut settings.backdrop_smoothing),
                #[cfg(feature = "hydra")]
                Row::HydraWebcam => set_switch(&mut settings.hydra_webcam),
                Row::BackdropStrength => {
                    settings.backdrop_opacity = step_opacity(settings.backdrop_opacity, forwards);
                }
                Row::InterfaceOpacity => {
                    settings.interface_opacity = step_opacity(settings.interface_opacity, forwards);
                }
                Row::EditorOpacity => {
                    settings.editor_opacity = step_opacity(settings.editor_opacity, forwards);
                }
                Row::ShowMenu => set_switch(&mut settings.show_menu),
                Row::ShowHeader => set_switch(&mut settings.show_header),
                Row::ShowFooter => set_switch(&mut settings.show_footer),
                Row::Zen => set_switch(&mut settings.zen),
                Row::SetPanelSide => set_switch(&mut settings.set_panel_right),
                Row::MixerEdge => set_switch(&mut settings.mixer_top),
                Row::VizEdgeOne => {
                    settings.viz_edges[0] = settings.viz_edges[0].next(forwards);
                }
                Row::VizEdgeTwo => {
                    settings.viz_edges[1] = settings.viz_edges[1].next(forwards);
                }
                // An action row has nothing to step: Enter opens it.
                Row::GlobalPrebake
                | Row::LocalPrebake
                | Row::SetsFolder
                | Row::RecordingsFolder
                | Row::CacheDefaults
                | Row::RefreshSources
                | Row::SampleCache => {}
                Row::SampleCeiling => {
                    settings.sample_ceiling = settings.sample_ceiling.step(forwards);
                }
                Row::PreviewBudget => {
                    settings.preview_budget = settings.preview_budget.step(forwards);
                }
                Row::UnusedSampleIdle => {
                    settings.unused_sample_idle = settings.unused_sample_idle.step(forwards);
                }
                Row::PrecacheSources => set_switch(&mut settings.precache_sources),
                Row::ReferenceCategory(category) => {
                    if settings.shows_category(category) {
                        settings.reference_hidden.push(category);
                    } else {
                        settings
                            .reference_hidden
                            .retain(|hidden| *hidden != category);
                    }
                }
            }
        };
        // Esc and Tab are the sheet's whatever page is up: a page that
        // swallowed them would be a page you could not leave.
        if self.shows_sources() && !matches!(code, KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab) {
            return self.source_key(code, settings);
        }
        if self.shows_mapping() && !matches!(code, KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab) {
            return self.mapping_key(code);
        }
        // The keybinds page keeps Esc and Tab for the sheet - a page you
        // could not leave is a trap - and hands a learn-armed capture its
        // key as `Capture` rather than deciding here: the app knows what
        // is being learnt, the sheet only knows what page is up.
        if self.shows_keybinds() && !matches!(code, KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab)
        {
            return self.keybind_key(code, settings);
        }
        match code {
            KeyCode::Esc => {
                if self.confirm_reset_keybinds {
                    self.confirm_reset_keybinds = false;
                    return SettingsAction::Nothing;
                }
                // The picture is over the page, not the page: Esc puts it
                // away and leaves the sheet standing, with whatever the
                // arrows chose still chosen. The next Esc closes the sheet
                // the way it always did.
                #[cfg(feature = "hydra")]
                if self.webcam_preview_open() {
                    self.webcam_preview = false;
                    return SettingsAction::Nothing;
                }
                SettingsAction::Close
            }
            KeyCode::Enter => {
                let row = self
                    .rows()
                    .get(self.selected)
                    .filter(|_| self.shows_settings());
                match row.and_then(|row| row.opens_prebake()) {
                    Some(scope) => SettingsAction::OpenPrebake(scope),
                    None if row == Some(&Row::SetsFolder) => SettingsAction::ChooseSetsFolder,
                    None if row == Some(&Row::RecordingsFolder) => {
                        SettingsAction::ChooseRecordingsFolder
                    }
                    None if row == Some(&Row::SampleCache) => SettingsAction::ClearSampleCache,
                    #[cfg(feature = "hydra")]
                    None if row == Some(&Row::HydraWebcam) => {
                        // Enter does not change the choice; the arrows do.
                        // Enter opens the camera preview so the player can
                        // judge the row, and a second Enter closes it.
                        if self.webcam_preview {
                            self.webcam_preview = false;
                            SettingsAction::Nothing
                        } else {
                            self.webcam_preview = true;
                            SettingsAction::OpenWebcamPreview
                        }
                    }
                    None => SettingsAction::Close,
                }
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.confirm_clear_cache = false;
                #[cfg(feature = "hydra")]
                {
                    self.webcam_preview = false;
                }
                const PAGES: [SettingsPage; 7] = [
                    SettingsPage::Settings,
                    SettingsPage::Advanced,
                    SettingsPage::Mapping,
                    SettingsPage::Keybinds,
                    SettingsPage::Sources,
                    SettingsPage::Reference,
                    SettingsPage::About,
                ];
                let at = PAGES
                    .iter()
                    .position(|page| *page == self.page)
                    .unwrap_or(0);
                let len = PAGES.len();
                let page = if matches!(code, KeyCode::Tab) {
                    PAGES[(at + 1) % len]
                } else {
                    PAGES[(at + len - 1) % len]
                };
                self.show_page(page);
                SettingsAction::Nothing
            }
            // The About page reads; it claims nothing but its own paging.
            _ if !self.shows_settings() => SettingsAction::Ignored,
            KeyCode::Up => {
                let len = self.rows().len().max(1);
                self.select((self.selected + len - 1) % len);
                SettingsAction::Nothing
            }
            KeyCode::Down => {
                let len = self.rows().len().max(1);
                self.select((self.selected + 1) % len);
                SettingsAction::Nothing
            }
            KeyCode::Right | KeyCode::Char(' ') | KeyCode::Left
                if self
                    .rows()
                    .get(self.selected)
                    .is_some_and(|row| row.opens_prebake().is_some()) =>
            {
                // Nothing changed, so nothing is remembered and the sheet
                // does not claim it kept a setting.
                SettingsAction::Nothing
            }
            KeyCode::Right | KeyCode::Char(' ') => {
                let Some(&row) = self.rows().get(self.selected) else {
                    return SettingsAction::Nothing;
                };
                let before = settings.clone();
                change(settings, row, true);
                // The preview follows the choice. A camera must not turn on
                // unseen, so the key that turns it on opens the preview. The
                // key that turns it off closes the preview.
                #[cfg(feature = "hydra")]
                if row == Row::HydraWebcam {
                    self.webcam_preview = settings.hydra_webcam;
                }
                if *settings == before {
                    SettingsAction::Nothing
                } else {
                    SettingsAction::Changed
                }
            }
            KeyCode::Left => {
                let Some(&row) = self.rows().get(self.selected) else {
                    return SettingsAction::Nothing;
                };
                let before = settings.clone();
                change(settings, row, false);
                // The preview follows the choice, as it does for Right.
                #[cfg(feature = "hydra")]
                if row == Row::HydraWebcam {
                    self.webcam_preview = settings.hydra_webcam;
                }
                if *settings == before {
                    SettingsAction::Nothing
                } else {
                    SettingsAction::Changed
                }
            }
            _ => SettingsAction::Ignored,
        }
    }

    /// The row under a pointer, when it is over one.
    /// The page a click on the header tabs means, if it lands on one.
    pub fn tab_at(available: Rect, x: u16, y: u16) -> Option<SettingsPage> {
        let (panel, _) = SettingsSheetView::geometry(available)?;
        if y != panel.y {
            return None;
        }
        if x >= panel.x + 2 && x < panel.x + 12 {
            return Some(SettingsPage::Settings);
        }
        if x >= panel.x + 13 && x < panel.x + 23 {
            return Some(SettingsPage::Advanced);
        }
        if x >= panel.x + 24 && x < panel.x + 33 {
            return Some(SettingsPage::Mapping);
        }
        if x >= panel.x + 34 && x < panel.x + 44 {
            return Some(SettingsPage::Keybinds);
        }
        if x >= panel.x + 45 && x < panel.x + 54 {
            return Some(SettingsPage::Sources);
        }
        if x >= panel.x + 56 && x < panel.x + 67 {
            return Some(SettingsPage::Reference);
        }
        if x >= panel.x + 68 && x < panel.x + 75 {
            return Some(SettingsPage::About);
        }
        None
    }

    pub fn show_page(&mut self, page: SettingsPage) {
        if self.page != page {
            self.navigation = self.remembered_navigation();
            let position = self.navigation.positions[page as usize];
            self.selected = position.selected;
            self.first = position.first;
            self.hold_scroll = position.hold_scroll;
            self.confirm_clear_cache = false;
            self.confirm_reset_keybinds = false;
            #[cfg(feature = "hydra")]
            {
                self.webcam_preview = false;
            }
        }
        self.page = page;
    }

    pub(super) fn remembered_navigation(self) -> SettingsNavigation {
        let mut navigation = self.navigation;
        navigation.page = self.page;
        navigation.positions[self.page as usize] = SettingsPosition {
            selected: self.selected,
            first: self.first,
            hold_scroll: self.hold_scroll,
        };
        navigation
    }

    /// The rows a key's move keeps between the selection and an edge, or
    /// none after a click.
    fn scroll_margin(self, shown: usize) -> usize {
        if self.hold_scroll {
            0
        } else {
            super::scroll::margin(shown)
        }
    }

    /// The first row drawn of a page of `total` plain rows, `shown` at a
    /// time: the selection among them, with its margin kept.
    fn first_row(self, shown: usize, total: usize) -> usize {
        super::scroll::follow(
            self.first,
            self.selected,
            shown,
            total,
            self.scroll_margin(shown),
        )
    }

    /// The first display line drawn of a grouped page, `shown` at a time.
    fn first_line(self, shown: usize, display: &[DisplayRow]) -> usize {
        if display.is_empty() {
            return 0;
        }
        let selected_line = display
            .iter()
            .position(|line| *line == DisplayRow::Control(self.selected))
            .unwrap_or(0);
        // The margin counts settings, not lines: a group's title and the
        // blank line above it are not choices, and a margin spent on them
        // would still hide the next group's first settings.
        let margin = self.scroll_margin(shown);
        let is_setting = |line: usize| matches!(display.get(line), Some(DisplayRow::Control(_)));
        let follow = |from: usize| {
            super::scroll::follow_choices(
                from,
                selected_line,
                shown,
                display.len(),
                margin,
                is_setting,
            )
        };
        let first = follow(self.first);
        let header = display[..=selected_line.min(display.len() - 1)]
            .iter()
            .rposition(|line| matches!(line, DisplayRow::Header { .. }))
            .unwrap_or(0);
        // Keep the selected group's label when the page can start at it with
        // the margin still kept both ways - the title then stays put there,
        // since that start is one the margin would not move. Taller groups
        // still scroll every setting into view. Not while a click holds the
        // page: the title coming into view would move the clicked row out
        // from the pointer.
        if !self.hold_scroll && first > header && follow(header) == header {
            header
        } else {
            first
        }
    }

    /// The first line drawn of the sources page, `shown` at a time: the
    /// cursor's choice among the page's rows (cache controls, imports,
    /// shipped packs) with its margin kept, by the same walk the other
    /// pages use. Headers and the gaps between groups are lines, not
    /// choices, and never spend the margin. The walk starts from
    /// `self.first`, not from the selection alone, so the window keeps its
    /// place while the cursor stays clear of the margin.
    fn first_sources(self, shown: usize, imported: usize, shipped: usize) -> usize {
        let lines = source_lines(imported, shipped);
        let selected_line = lines
            .iter()
            .position(|line| match line {
                SourceLine::Control(index) | SourceLine::Row(index) => *index == self.selected,
                _ => false,
            })
            .unwrap_or(0);
        let is_choice = |line: usize| {
            matches!(
                lines.get(line),
                Some(SourceLine::Control(_)) | Some(SourceLine::Row(_))
            )
        };
        super::scroll::follow_choices(
            self.first,
            selected_line,
            shown,
            lines.len(),
            self.scroll_margin(shown),
            is_choice,
        )
    }

    /// Keep where the page is scrolled to, for the next key to move from.
    /// The app calls it after every key the sheet takes, with the frame the
    /// sheet is drawn in.
    pub fn settle_scroll(&mut self, available: Rect) {
        let Some((_, rows)) = self.geometry_for(available) else {
            return;
        };
        let shown = usize::from(rows.height).max(1);
        self.first = match self.page {
            SettingsPage::Settings | SettingsPage::Advanced | SettingsPage::Reference => {
                self.first_line(shown, &grouped_rows(self.rows()))
            }
            SettingsPage::Keybinds => self.first_row(shown, self.keybind_count),
            SettingsPage::Sources => {
                self.first_sources(shown, self.source_count, self.default_count)
            }
            SettingsPage::Mapping => self.mapping_first(rows),
            SettingsPage::About => self
                .first
                .min(about_content_height(rows.width).saturating_sub(shown)),
        };
    }

    fn mapping_first(self, rows: Rect) -> usize {
        let shown = (usize::from(rows.height) / usize::from(SLOT_HEIGHT)).max(1);
        super::scroll::follow(
            self.first,
            self.selected.min(MAPPING_SLOTS - 1) / usize::from(MAPPING_COLUMNS),
            shown,
            MAPPING_SLOTS.div_ceil(usize::from(MAPPING_COLUMNS)),
            0,
        )
    }

    /// The page the sheet is on.
    pub fn page(self) -> SettingsPage {
        self.page
    }

    pub fn row_at_for(self, available: Rect, x: u16, y: u16) -> Option<usize> {
        let (_, rows) = self.geometry_for(available)?;
        if x <= rows.x || x >= rows.right().saturating_sub(1) || y < rows.y || y >= rows.bottom() {
            return None;
        }
        let shown = usize::from(rows.height).max(1);
        let display = grouped_rows(self.rows());
        let first = self.first_line(shown, &display);
        let offset = usize::from(y - rows.y);
        match display.get(first + offset) {
            Some(DisplayRow::Control(index)) => Some(*index),
            _ => None,
        }
    }

    /// Reserve a footer row for the sample drop hint. Reserve a larger
    /// camera thumbnail only while the Hydra webcam preview is open.
    pub fn geometry_for(self, available: Rect) -> Option<(Rect, Rect)> {
        let (panel, rows) = SettingsSheetView::geometry(available)?;
        if self.shows_sources() {
            return Some((
                panel,
                Rect {
                    height: rows.height.saturating_sub(1),
                    ..rows
                },
            ));
        }
        #[cfg(feature = "hydra")]
        if self.webcam_preview_open() {
            let preview_height = (available.width / 10).clamp(4, 16).min(
                panel
                    .height
                    .saturating_sub(SettingsSheetView::MIN_ROWS_SHOWN + 4),
            );
            return Some((
                panel,
                Rect {
                    height: panel.height.saturating_sub(preview_height + 4),
                    ..rows
                },
            ));
        }
        Some((panel, rows))
    }

    /// The mapping slot under a point, while the mapping page is up.
    pub fn slot_at(self, available: Rect, x: u16, y: u16) -> Option<usize> {
        if !self.shows_mapping() {
            return None;
        }
        let (_, rows) = self.geometry_for(available)?;
        SlotGrid::scrolled(rows, self.mapping_first(rows))?.at(x, y)
    }

    /// The keybind row under a point, while the keybinds page is up: the
    /// action whose Enter or click would learn, found the way the page
    /// draws its rows - same geometry, same scroll.
    pub fn keybind_row_at(self, available: Rect, x: u16, y: u16) -> Option<usize> {
        if !self.shows_keybinds() {
            return None;
        }
        let (_, rows) = self.geometry_for(available)?;
        if x < rows.x || x >= rows.right() || y < rows.y || y >= rows.bottom() {
            return None;
        }
        let shown = usize::from(rows.height).max(1);
        let first = self.first_row(shown, self.keybind_count);
        let index = first + usize::from(y - rows.y);
        (index < self.keybind_count).then_some(index)
    }

    /// The source under a point, while the sources page is up: an import
    /// or a shipped pack, by the index the cursor walks, found the way the
    /// page draws its lines - same rule and hint lines, same scroll. The
    /// rule and the "no imports" line are not rows, and answer nothing.
    /// The same window the page draws: settled, never re-pinned to the
    /// selection, or a click would move the list under the pointer.
    pub fn source_row_at(self, available: Rect, x: u16, y: u16) -> Option<usize> {
        if !self.shows_sources() {
            return None;
        }
        let (_, rows) = self.geometry_for(available)?;
        if x < rows.x || x >= rows.right() || y < rows.y || y >= rows.bottom() {
            return None;
        }
        let len = SOURCE_CONTROL_COUNT + self.source_count + self.default_count;
        if len == 0 {
            return None;
        }
        let lines = source_lines(self.source_count, self.default_count);
        let shown = usize::from(rows.height).max(1);
        let first = self.first_sources(shown, self.source_count, self.default_count);
        match lines.get(first + usize::from(y - rows.y)) {
            Some(SourceLine::Control(index)) | Some(SourceLine::Row(index)) => Some(*index),
            _ => None,
        }
    }

    /// The display line at a point, including group borders. For an actual
    /// selectable control use [`Self::row_at_for`], which accounts for scroll.
    pub fn row_at(available: Rect, x: u16, y: u16) -> Option<usize> {
        let (_, rows) = SettingsSheetView::geometry(available)?;
        (x >= rows.x && x < rows.right() && y >= rows.y && y < rows.bottom())
            .then(|| usize::from(y - rows.y))
    }
}

/// Which of the two rows put the files in the line, and so which one
/// follows them down.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrecacheKind {
    /// The switch: the sources the player imported in Settings, fetched
    /// onto disk whenever the studio opens.
    Imports,
    /// Enter on the library row: every sound the studio ships with,
    /// fetched once so nothing ever waits on the network again.
    Library,
}

/// The pre-cache as its row on the samples page follows it: the files it
/// put in the loader's line, how many are still there, and the one in hand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrecacheProgress {
    /// Which row started it, and so which row shows it.
    pub kind: PrecacheKind,
    /// Files the pre-cache queued.
    pub total: usize,
    /// Files still in the line.
    pub left: usize,
    /// The file the loader is fetching or decoding, when it has one.
    pub loading: Option<String>,
}

impl PrecacheProgress {
    /// Every file is in - or failed, which the log says - and the loader
    /// has nothing in hand.
    pub fn done(&self) -> bool {
        self.left == 0 && self.loading.is_none()
    }

    /// The count alone: what is in, out of what was asked for.
    pub fn count(&self) -> String {
        if self.done() {
            format!("{} fetched", self.total)
        } else {
            format!("{}/{}", self.total.saturating_sub(self.left), self.total)
        }
    }

    /// The value column of the switch that started it: the switch, then
    /// the count.
    pub fn value(&self) -> String {
        format!("● on · {}", self.count())
    }

    /// The explanation column while the files come in: a bar, the file
    /// in hand, and how many are still to come. `None` once they are in,
    /// when the row's own explanation is the better one.
    pub fn explain(&self) -> Option<String> {
        if self.done() {
            return None;
        }
        const WIDTH: usize = 10;
        let done = self.total.saturating_sub(self.left);
        let filled = (done * WIDTH)
            .checked_div(self.total)
            .unwrap_or(0)
            .min(WIDTH);
        let percent = (done * 100).checked_div(self.total).unwrap_or(0);
        let bar: String = "█".repeat(filled) + &"░".repeat(WIDTH - filled);
        Some(match &self.loading {
            Some(file) => format!("{bar} {percent}% · syncing {file} · {} to go", self.left),
            None => format!("{bar} {percent}% · {} to go", self.left),
        })
    }
}

pub struct SettingsSheetView<'a> {
    pub sheet: SettingsSheet,
    pub settings: &'a UiSettings,
    pub features: &'a TerminalFeatures,
    pub tier: Tier,
    pub registry: &'a CapabilityRegistry,
    /// The Cargo features the binary was built with, for the About page.
    pub build_features: &'a [&'a str],
    pub device: Option<&'a StudioDeviceInfo>,
    pub pressure: Option<&'a EnginePressureSnapshot>,
    pub max_polyphony_override: Option<usize>,
    /// Live camera acquisition state. Persisted consent stays in
    /// `UiSettings`; this snapshot reports what the asynchronous worker did.
    #[cfg(feature = "hydra")]
    pub hydra_webcam: Option<rustel_runtime::hydra::HydraWebcamStatus>,
    /// The two prebakes as their rows should read, in scope order.
    pub prebakes: [PrebakeRow; 2],
    /// Where new sets are made, as the row should read.
    pub sets_folder: String,
    /// Where finished audio takes are written, as the row should read.
    pub recordings_folder: String,
    /// What the open set says about the limiter, when it says anything.
    ///
    /// The limiter rows are the studio's default. A set with a limiter of
    /// its own does not use them, and the switch row shows that. Without
    /// this text the switch looks broken while the set overrules it.
    pub set_limiter: Option<String>,
    /// What the downloaded samples take on disk, as the row should read -
    /// measured off the frame's turn, not this one.
    pub sample_cache: String,
    /// The pre-cache under way, or the last one, for its row to follow.
    pub precache: Option<PrecacheProgress>,
    /// The imported sources, in precedence order, as their rows read.
    pub sources: Vec<SourceRow>,
    /// The twelve mapping slots, as their boxes read.
    pub mappings: [SlotView; MAPPING_SLOTS],
    /// The shortcut rows, as the keybinds page reads them.
    pub bindings: Vec<KeybindRow>,
    /// Only `super_seen` is read here, for the one place ⌘ is named in the
    /// whole studio: the About page can honestly say Command reaches this
    /// terminal, without any chord ever being advertised in that form.
    pub capabilities: KeyboardCapabilities,
    pub theme: &'a Theme,
}

impl SettingsSheetView<'_> {
    /// The shortest sheet worth drawing: a few rows of switches, the
    /// chrome around them, and the footer hint that says how to work it.
    const MIN_ROWS_SHOWN: u16 = 4;

    /// A sheet across the bottom: the switches, then what the terminal is.
    ///
    /// The sheet takes the height it can get. When the terminal is too
    /// short for the whole list, the list scrolls. Only a terminal too
    /// short for even a few rows gets no sheet.
    pub fn geometry(available: Rect) -> Option<(Rect, Rect)> {
        // Two spare rows past the settings list: the about page's terminal
        // block needs them, and the sheet must be one height for both
        // pages so the mouse claims stay honest.
        let chrome = 3 + Self::explain_rows(available);
        let wanted = grouped_rows(ROWS).len() as u16 + chrome;
        let height = wanted.min(available.height.saturating_sub(2));
        if height < Self::MIN_ROWS_SHOWN + chrome || available.width < 60 {
            return None;
        }
        let area = Rect::new(
            available.x + 1,
            available.bottom().saturating_sub(height),
            available.width.saturating_sub(2),
            height,
        );
        let rows = Rect::new(
            area.x + 2,
            area.y + 1,
            area.width.saturating_sub(4),
            height.saturating_sub(chrome),
        );
        Some((area, rows))
    }

    /// Rows kept under the list to print the selected row's explanation,
    /// wrapped, plus the blank that separates it.
    ///
    /// None of them where the row itself has room for the longest
    /// explanation there is: it is already on the row, in full, and
    /// printing it again underneath spent five rows of the sheet saying
    /// one sentence twice. A narrow terminal trims the copy on the row,
    /// and there the block earns its place.
    ///
    /// Measured over every page of controls, because the sheet is one
    /// height for all of them.
    fn explain_rows(available: Rect) -> u16 {
        let controls = || ROWS.iter().chain(ADVANCED_ROWS).chain(REFERENCE_ROWS);
        let label = controls()
            .map(|row| row.label().chars().count())
            .max()
            .unwrap_or(14)
            .max(14);
        let longest = controls()
            .map(|row| row.explain().chars().count())
            .max()
            .unwrap_or(0);
        // What `render` leaves for the explanation on the row itself: the
        // list's width less the marker, the label column, the value column
        // and the single spaces between them.
        let inline = usize::from(available.width).saturating_sub(6 + 2 + label + 1 + 22 + 1 + 2);
        if inline >= longest {
            return 0;
        }
        // As many rows as the longest one actually wraps to here, and the
        // blank that keeps it off the list. It was four rows whatever the
        // width, which on most terminals meant three blank ones.
        let list_width = usize::from(available.width).saturating_sub(6);
        let wrapped = controls()
            .map(|row| wrap_words(row.explain(), list_width).len())
            .max()
            .unwrap_or(1);
        u16::try_from(wrapped.clamp(1, 4)).unwrap_or(4) + 1
    }
}

/// What one rule row does: open the first group, close one group and open
/// the next in a single row, or close the last.
#[derive(Clone, Copy, Debug)]
enum GroupRule<'a> {
    Opens(&'a str),
    Between(&'a str),
    Closes,
}

impl<'a> GroupRule<'a> {
    fn title(self) -> Option<&'a str> {
        match self {
            Self::Opens(title) | Self::Between(title) => Some(title),
            Self::Closes => None,
        }
    }
}

fn wrap_words(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let extra = usize::from(!line.is_empty()) + word.chars().count();
        if !line.is_empty() && line.chars().count() + extra > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn about_report(view: &SettingsSheetView<'_>) -> CapabilityReportV1 {
    let safety = CapabilitySafetyFacts::with_tripwire(
        view.device.map(|device| device.allocator_tripwire_armed),
    );
    let safety = view.pressure.map_or(safety, |pressure| {
        safety.with_live_report(&pressure.device, Some(pressure.producer_refusals))
    });
    let registry = view
        .device
        .map_or(view.registry, |device| device.registry.as_ref());
    let mut context = CapabilityReportContext::new(registry)
        .with_build_features(view.build_features)
        .with_safety(safety);
    if let Some(device) = view.device {
        context = context.with_audio(&device.audio);
    }
    CapabilityReportV1::capture(context)
}

/// The folders and packs the player imported, in precedence order.
///
/// Not a settings page: these are rows you added, so the keys are a
/// list's. Order is what settles two packs that both call something `bd`,
/// so it is visible - the row nearer the bottom is the one that wins, and
/// `[` / `]` is how you say which.
/// Slot boxes across the page. Three is what fits a box wide enough to
/// hold `cc74 · ch2`, a bar and the fader's name at the narrowest sheet
/// the studio will draw.
pub const MAPPING_COLUMNS: u16 = 3;
/// Each box: a rule with the slot number, what it is bound to, and the
/// fader it drives with its own bar.
const SLOT_HEIGHT: u16 = 3;

/// One mapping slot as its box should read. The sheet owns none of this:
/// what a slot is bound to lives in the settings, what it drives lives in
/// the evaluated score, and whether it just moved lives in the MIDI feed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SlotView {
    /// What it is bound to, short enough for the box: `cc74·2`, `x1`.
    /// Empty when nothing is.
    pub chip: String,
    /// What a turn of it means to the fader: `scaled`, `jump`, `relative`.
    pub takeover: &'static str,
    /// The fader it drives in the evaluated score, when the score has one
    /// that far along.
    pub fader: String,
    /// Where that fader stands, 0 at its floor and 1 at its ceiling.
    pub notch: Option<f32>,
    /// Its control moved just now: the confirmation that a mapping is
    /// live, without having to arm a learn to find out.
    pub live: bool,
    /// Waiting for a control to move.
    pub learning: bool,
}

/// The grid of slot boxes inside the sheet's row area.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SlotGrid {
    area: Rect,
    width: u16,
    first: usize,
}

impl SlotGrid {
    pub fn new(rows: Rect) -> Option<Self> {
        Self::scrolled(rows, 0)
    }

    fn scrolled(rows: Rect, first: usize) -> Option<Self> {
        let width = rows.width / MAPPING_COLUMNS;
        let wanted = SLOT_HEIGHT * (MAPPING_SLOTS as u16).div_ceil(MAPPING_COLUMNS);
        (width >= 14 && rows.height >= SLOT_HEIGHT).then_some(Self {
            area: Rect {
                height: rows.height.min(wanted),
                ..rows
            },
            width,
            first,
        })
    }

    /// Where one slot's box is, or none for a box this sheet is too short
    /// to show.
    pub fn cell(&self, slot: usize) -> Option<Rect> {
        let slot = u16::try_from(slot).ok()?;
        let (row, column) = (slot / MAPPING_COLUMNS, slot % MAPPING_COLUMNS);
        let row = usize::from(row).checked_sub(self.first)?;
        let y = self.area.y + u16::try_from(row).ok()? * SLOT_HEIGHT;
        (y + SLOT_HEIGHT <= self.area.bottom()).then(|| {
            Rect::new(
                self.area.x + column * self.width,
                y,
                self.width.saturating_sub(1),
                SLOT_HEIGHT,
            )
        })
    }

    /// The slot under a point, if the point is on one.
    pub fn at(&self, x: u16, y: u16) -> Option<usize> {
        (0..MAPPING_SLOTS).find(|slot| {
            self.cell(*slot)
                .is_some_and(|cell| cell.contains(ratatui::layout::Position::new(x, y)))
        })
    }
}

/// One action's binding, as its row should read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KeybindRow {
    /// The action's row label.
    pub action: &'static str,
    /// The chord as every surface spells it: `^S`, `F4`. Empty while a
    /// learn waits.
    pub chord: String,
    /// The studio's second chord for the same action - evaluate's `F5`,
    /// stop's `F8` - spelled `also F5` on the row so a shortcut the
    /// studio answers to is never a secret. Empty when there is none,
    /// and empty once the action is overridden or unbound: the alias
    /// belongs to the default and goes where it goes.
    pub also: String,
    /// Whether the chord is one the player set rather than the
    /// studio's own default.
    pub learnt: bool,
    /// Waiting for a chord to be pressed.
    pub learning: bool,
    /// The chord in hand is another action's and the row is asking:
    /// Enter takes it, Esc keeps what the other action has.
    pub confirm: bool,
}

fn keybinds_footer_hint(
    sheet: SettingsSheet,
    settings: &UiSettings,
    features: &TerminalFeatures,
) -> String {
    if sheet.selected == TERMINAL_PROFILE_ROW {
        return "Left/Right: choose · Enter: next · Del: automatic".to_owned();
    }
    if sheet.selected == RESET_KEYBINDS_ROW {
        let profile = terminal_profile_label(settings, features);
        if sheet.confirm_reset_keybinds {
            return format!("Reset all to {profile} defaults? Enter: confirm · Esc: cancel");
        }
        return format!("Enter: reset all to {profile} defaults · Esc: close");
    }
    "Enter: rebind · Del: terminal default · Tab: pages · Esc: close".to_owned()
}

/// A line on the sources page: a group border, air, a cache control, a
/// source row, or the empty-state line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceLine {
    /// A group title: opens the first box, or closes one and opens the next.
    Header { section: SourceSection, first: bool },
    /// Blank row above a later group's rule - the same air main settings uses.
    Gap,
    /// Closes the last group box.
    Footer,
    /// A cache control at the top of the page, by its cursor index.
    Control(usize),
    /// No imports yet: what the user-samples section is for.
    NoImports,
    /// A source row, by its cursor index in the combined list.
    Row(usize),
}

/// Which boxed section a sources-page header names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceSection {
    Cache,
    User,
    Defaults,
}

impl SourceSection {
    fn title(self) -> &'static str {
        match self {
            Self::Cache => "sample cache",
            Self::User => "user samples",
            Self::Defaults => "default samples",
        }
    }
}

/// The page's lines in order: sample-cache controls, user samples (or the
/// line that says there are none), then default samples - each in the same
/// boxed groups the other settings pages use. Each row's index is the one
/// the sheet's cursor walks - so the sheet can find the row under a click
/// by the same reckoning the page draws.
fn source_lines(imported: usize, shipped: usize) -> Vec<SourceLine> {
    let mut lines = Vec::with_capacity(SOURCE_CONTROL_COUNT + imported + shipped + 8);
    lines.push(SourceLine::Header {
        section: SourceSection::Cache,
        first: true,
    });
    lines.extend((0..SOURCE_CONTROL_COUNT).map(SourceLine::Control));
    lines.push(SourceLine::Gap);
    lines.push(SourceLine::Header {
        section: SourceSection::User,
        first: false,
    });
    if imported == 0 {
        lines.push(SourceLine::NoImports);
    }
    lines.extend((SOURCE_CONTROL_COUNT..SOURCE_CONTROL_COUNT + imported).map(SourceLine::Row));
    if shipped > 0 {
        lines.push(SourceLine::Gap);
        lines.push(SourceLine::Header {
            section: SourceSection::Defaults,
            first: false,
        });
        lines.extend(
            (SOURCE_CONTROL_COUNT + imported..SOURCE_CONTROL_COUNT + imported + shipped)
                .map(SourceLine::Row),
        );
    }
    lines.push(SourceLine::Footer);
    lines
}
/// MiB/GiB label for a byte count, matching the sample cache row.
pub fn format_cache_bytes(bytes: u64) -> String {
    if bytes == 0 {
        return "empty".to_owned();
    }
    let mib = bytes as f64 / (1024.0 * 1024.0);
    if mib >= 1024.0 {
        format!("{:.1} GiB", mib / 1024.0)
    } else if mib >= 10.0 {
        format!("{mib:.0} MiB")
    } else {
        format!("{mib:.1} MiB")
    }
}

/// The rest of a remote pack's row: how many sounds it holds, how much
/// of it is on disk, and the bytes those files take.
pub fn source_cache_state(sounds: usize, cached: usize, files: usize, bytes: u64) -> String {
    let mut line = format!("{sounds} sound(s)");
    if files > 0 {
        line.push_str(&format!(" · {cached}/{files} cached"));
    }
    if bytes > 0 {
        line.push_str(&format!(" · {}", format_cache_bytes(bytes)));
    }
    line
}

/// One source on the Sources page, as its row reads: a pack or folder the
/// player imported, or one of the packs the studio ships with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRow {
    pub spec: String,
    /// The folder's own name, told apart from its neighbours where it has
    /// to be. A page of identical truncated home paths is not a list you
    /// can pick from; the whole spec is still shown, on the row you are
    /// standing on, which is the one you are about to act on.
    pub label: String,
    /// What it brought, or why it brought nothing.
    pub state: String,
    /// Off, or not there: the row is kept but says so quietly.
    pub dimmed: bool,
    /// A pack the studio ships with, listed after the imports and drawn
    /// apart from them: it is the library's, not the player's, so it can
    /// be cached onto disk but not removed, turned off or reordered.
    pub shipped: bool,
    /// A folder already on this machine: the row says `(local)` and there
    /// is nothing to download.
    pub local: bool,
    /// Files on disk vs files named, for colouring the `n/m cached`
    /// fraction: full, partial, or none. Absent when there is no remote
    /// cache to count.
    pub cache_fill: Option<(usize, usize)>,
    /// While this pack's files are being cached onto disk: how far along.
    pub cache: Option<SourceCacheProgress>,
}

/// One shipped pack's files on their way onto disk, for its row to follow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceCacheProgress {
    /// Distinct files the pack names.
    pub total: usize,
    /// Files still in the loader's line.
    pub left: usize,
    /// The pack's file the loader has in hand, when it has one.
    pub loading: Option<String>,
}

impl SourceCacheProgress {
    /// Every file is on disk - or failed, which the log says.
    pub fn done(&self) -> bool {
        self.left == 0 && self.loading.is_none()
    }

    /// The state column while the files come in: a bar, the count, and
    /// the file in hand; once they are in, that they are.
    pub fn label(&self) -> String {
        if self.done() {
            return format!("{} file(s) on disk", self.total);
        }
        const WIDTH: usize = 8;
        let done = self.total.saturating_sub(self.left);
        let filled = (done * WIDTH)
            .checked_div(self.total)
            .unwrap_or(0)
            .min(WIDTH);
        let percent = (done * 100).checked_div(self.total).unwrap_or(0);
        let bar: String = "█".repeat(filled) + &"░".repeat(WIDTH - filled);
        match &self.loading {
            Some(file) => format!("{bar} {percent}% · {done}/{} · {file}", self.total),
            None => format!("{bar} {percent}% · {done}/{}", self.total),
        }
    }
}

fn about_content_height(width: u16) -> usize {
    let columns = if width >= 78 { 3 } else { 2 };
    let checklist =
        terminal_checklist(&TerminalFeatures::default(), KeyboardCapabilities::legacy());
    let report = CapabilityReportV1::capture(CapabilityReportContext::new(
        rustel_runtime::capability_registry(),
    ));
    5 + checklist.len().div_ceil(columns) + report.human_lines().len()
}

fn terminal_checklist(
    features: &TerminalFeatures,
    capabilities: KeyboardCapabilities,
) -> [(&'static str, Option<bool>); 6] {
    let measured = |available| (available || !features.name.is_empty()).then_some(available);
    let pixels = features.cell_pixels.is_some_and(|(w, h)| w > 0 && h > 0);
    [
        ("Full colour", measured(features.truecolor)),
        ("Precise sliders", measured(features.pixel_mouse && pixels)),
        ("Smooth redraws", measured(features.sync_output)),
        ("Enhanced shortcuts", measured(capabilities.enhanced)),
        (
            "Pixel graphics",
            measured(RenderingMode::Kitty.supported(features)),
        ),
        (
            "Shortcut profile",
            super::terminal::conflicts::profile_known(&features.name).then_some(true),
        ),
    ]
}

#[cfg(test)]
mod tests {
    #[test]
    fn settings_navigation_remembers_every_page_and_reopens_the_last() {
        let pages = [
            SettingsPage::Settings,
            SettingsPage::Advanced,
            SettingsPage::Mapping,
            SettingsPage::Keybinds,
            SettingsPage::Sources,
            SettingsPage::About,
        ];
        let mut sheet = SettingsSheet::default();
        for (index, page) in pages.into_iter().enumerate() {
            sheet.show_page(page);
            sheet.selected = index + 3;
            sheet.first = index + 1;
            sheet.hold_scroll = index % 2 == 0;
            sheet.confirm_clear_cache = true;
            sheet.confirm_reset_keybinds = true;
            sheet = sheet.remembered_navigation().open();
            assert_eq!(sheet.page, page);
            assert_eq!((sheet.selected, sheet.first), (index + 3, index + 1));
            assert_eq!(sheet.hold_scroll, index % 2 == 0);
            assert!(!sheet.confirm_clear_cache && !sheet.confirm_reset_keybinds);
        }
        for (index, page) in pages.into_iter().enumerate() {
            sheet.show_page(page);
            assert_eq!((sheet.selected, sheet.first), (index + 3, index + 1));
            assert_eq!(sheet.hold_scroll, index % 2 == 0);
        }
        assert_eq!(
            SettingsNavigation::default().open(),
            SettingsSheet::default()
        );
    }

    #[test]
    fn advanced_piano_controls_select_sound_and_bound_volume() {
        let mut settings = UiSettings::default();
        let features = TerminalFeatures::default();
        let mut sheet = SettingsSheet {
            page: SettingsPage::Advanced,
            selected: ADVANCED_ROWS
                .iter()
                .position(|row| matches!(row, Row::PianoSound))
                .unwrap(),
            ..SettingsSheet::default()
        };
        assert_eq!(settings.piano_sound, super::super::PianoSound::Triangle);
        assert_eq!(settings.piano_volume, 130);
        sheet.key(KeyCode::Right, &mut settings, &features);
        assert_eq!(settings.piano_sound, super::super::PianoSound::Square);
        sheet.key(KeyCode::Left, &mut settings, &features);
        assert_eq!(settings.piano_sound, super::super::PianoSound::Triangle);
        sheet.key(KeyCode::Down, &mut settings, &features);
        sheet.key(KeyCode::Right, &mut settings, &features);
        assert_eq!(settings.piano_volume, 140);
        for _ in 0..25 {
            sheet.key(KeyCode::Right, &mut settings, &features);
        }
        assert_eq!(settings.piano_volume, 200);
        for _ in 0..25 {
            sheet.key(KeyCode::Left, &mut settings, &features);
        }
        assert_eq!(settings.piano_volume, 0);
    }

    #[test]
    fn advanced_polyphony_steps_clamp_and_explain_the_cpu_tradeoff() {
        let mut settings = UiSettings::default();
        let features = TerminalFeatures::default();
        let mut sheet = SettingsSheet {
            page: SettingsPage::Advanced,
            selected: ADVANCED_ROWS
                .iter()
                .position(|row| matches!(row, Row::MaxPolyphony))
                .unwrap(),
            ..SettingsSheet::default()
        };
        assert_eq!(settings.max_polyphony, 128);
        for expected in [192, 256, 256] {
            sheet.key(KeyCode::Right, &mut settings, &features);
            assert_eq!(settings.max_polyphony, expected);
        }
        for expected in [192, 128, 64, 32, 32] {
            sheet.key(KeyCode::Left, &mut settings, &features);
            assert_eq!(settings.max_polyphony, expected);
        }
        assert_eq!(step_max_polyphony(96, false), 64);
        assert_eq!(step_max_polyphony(96, true), 128);
        assert_eq!(max_polyphony_readout(128, None), "128 (default)");
        assert_eq!(max_polyphony_readout(256, None), "256");
        assert_eq!(max_polyphony_readout(256, Some(4)), "256 (score 4)");
        assert_eq!(max_polyphony_readout(128, Some(128)), "128 (score 128)");
        for width in [60, 80, 120] {
            let buffer = render_sheet(sheet, &settings, &features, Rect::new(0, 0, width, 24));
            let text = buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(text.contains("max polyphony"), "{text}");
            assert!(text.contains("CPU") && text.contains("crackles"), "{text}");
        }
    }

    /// A kept rung reads back as the rung, so the arrows find it on the
    /// ladder after a restart; an off-ladder size is kept exactly; a size
    /// the engine would refuse falls back to Automatic.
    #[test]
    fn output_latency_round_trips_through_its_key() {
        for rung in OutputLatency::LADDER {
            assert_eq!(OutputLatency::parse(&rung.key()), rung, "{rung:?}");
            assert_eq!(rung.step(true).step(false), rung, "{rung:?}");
        }
        assert_eq!(
            OutputLatency::parse("128").step(true),
            OutputLatency::Frames256
        );
        assert_eq!(OutputLatency::parse("96"), OutputLatency::Frames(96));
        assert_eq!(OutputLatency::parse("96").key(), "96");
        assert_eq!(OutputLatency::parse(" 512 "), OutputLatency::Frames512);
        assert_eq!(OutputLatency::parse("0"), OutputLatency::Automatic);
        assert_eq!(OutputLatency::parse("16"), OutputLatency::Automatic);
        assert_eq!(OutputLatency::parse("65536"), OutputLatency::Automatic);
        assert_eq!(OutputLatency::parse("auto"), OutputLatency::Automatic);
        assert_eq!(OutputLatency::parse("nonsense"), OutputLatency::Automatic);
        assert_eq!(OutputLatency::from_frames(2048), OutputLatency::Frames2048);
        assert_eq!(OutputLatency::parse("31"), OutputLatency::Automatic);
        assert_eq!(OutputLatency::parse("16385"), OutputLatency::Automatic);
        assert_eq!(OutputLatency::parse("16384"), OutputLatency::Frames(16_384));
    }

    /// An off-ladder size - from `--buffer-frames` or the preferences file -
    /// steps to the nearest rung in the direction asked, not to the
    /// ladder's start.
    #[test]
    fn an_off_ladder_output_latency_steps_to_the_nearest_rung() {
        assert_eq!(
            OutputLatency::Frames(96).step(true),
            OutputLatency::Frames128
        );
        assert_eq!(
            OutputLatency::Frames(96).step(false),
            OutputLatency::Frames64
        );
        assert_eq!(
            OutputLatency::Frames(4096).step(false),
            OutputLatency::Frames2048
        );
        assert_eq!(
            OutputLatency::Frames(4096).step(true),
            OutputLatency::Automatic,
            "past the top rung the ladder wraps, as the rungs do"
        );
    }

    /// On Windows the callback stays at the WASAPI period and a larger
    /// size chosen is queued ahead of it: the row reads the buffer the
    /// host grants, not the period. A host that really gives a different
    /// size still says so, and a closed stream reads the choice alone.
    #[test]
    fn the_output_latency_row_reads_the_buffer_the_host_grants() {
        let facts = |requested: u32, reported: Option<u32>, period: Option<u32>| {
            rustel_audio::AudioOutputFacts::new(
                rustel_audio::AudioHost::cpal("host"),
                "device",
                48_000,
                2,
                Some(rustel_audio::AudioSampleFormat::F32),
                requested,
                reported,
            )
            .with_device_period_frames(period)
        };
        let wasapi = facts(2048, Some(480), Some(480));
        assert_eq!(
            output_latency_readout(OutputLatency::Frames2048, Some(&wasapi)),
            "2048 frames \u{b7} 42.7 ms"
        );
        let clamped = facts(2048, Some(1024), None);
        assert_eq!(
            output_latency_readout(OutputLatency::Frames2048, Some(&clamped)),
            "2048 asked, 1024 given \u{b7} 21.3 ms"
        );
        let automatic = facts(480, Some(480), Some(480));
        assert_eq!(
            output_latency_readout(OutputLatency::Automatic, Some(&automatic)),
            "automatic \u{b7} 480 frames \u{b7} 10.0 ms"
        );
        assert_eq!(
            output_latency_readout(OutputLatency::Frames256, None),
            "256 frames"
        );
        assert_eq!(
            output_latency_readout(OutputLatency::Automatic, None),
            "automatic"
        );
    }

    use super::super::prebake::PrebakeVerdict;
    use super::*;
    use crossterm::event::KeyCode;

    /// The biggest-sound ceiling is the player's, within a guard that is
    /// not: a rung may not remove the thing that stops `/dev/zero`.
    #[test]
    fn the_sample_ceiling_is_the_players_between_two_walls() {
        // The ladder only climbs, and every rung is a real ceiling the
        // audio crate will accept.
        let mut seen = 0usize;
        for rung in SampleCeiling::LADDER {
            assert!(rung.bytes() > seen, "the ladder climbs: {}", rung.label());
            seen = rung.bytes();
            assert_eq!(
                rustel_audio::set_sample_pcm_ceiling(rung.bytes()),
                rung.bytes(),
                "{} is inside the guard",
                rung.label()
            );
            assert_eq!(SampleCeiling::parse(rung.key()), rung, "it survives prefs");
        }
        assert_eq!(
            SampleCeiling::default().bytes(),
            rustel_audio::DEFAULT_SAMPLE_PCM_BYTES
        );
        assert_eq!(
            seen,
            rustel_audio::MAX_SAMPLE_PCM_BYTES,
            "the top rung is the wall"
        );

        // Neither wall can be argued with, however the ask arrives.
        assert_eq!(
            rustel_audio::set_sample_pcm_ceiling(usize::MAX),
            rustel_audio::MAX_SAMPLE_PCM_BYTES
        );
        assert_eq!(
            rustel_audio::set_sample_pcm_ceiling(0),
            rustel_audio::MIN_SAMPLE_PCM_BYTES
        );

        // A four-and-a-half-minute stereo take fits the default ceiling and
        // does not fit the tight one.
        let take = 112_805_430;
        assert!(take < SampleCeiling::default().bytes());
        assert!(take > SampleCeiling::Tight.bytes(), "and 64 MiB refused it");
        // 48 kHz float stereo is 384 kB a second, so the default holds
        // about eleven and a half minutes of it.
        assert!(
            (11.0..12.0).contains(&SampleCeiling::default().minutes()),
            "{}",
            SampleCeiling::default().minutes()
        );

        // Put it back, so a later test in this process is not left with
        // whatever this one wanted.
        rustel_audio::set_sample_pcm_ceiling(rustel_audio::DEFAULT_SAMPLE_PCM_BYTES);
    }

    /// The sheet shows the selected row's explanation once: on the row when
    /// it fits, in a block underneath when it does not. One rule row closes
    /// a group and opens the next.
    #[test]
    fn the_sheet_is_no_taller_than_what_it_has_to_say() {
        let settings = UiSettings::default();
        let features = TerminalFeatures::default();
        let controls = grouped_rows(ROWS).len() as u16;

        // Wide enough for the longest explanation to sit on its own row.
        let wide = Rect::new(0, 0, 190, 50);
        assert_eq!(SettingsSheetView::explain_rows(wide), 0);
        let (sheet, _) = SettingsSheetView::geometry(wide).expect("a sheet");
        assert_eq!(sheet.height, controls + 3, "border, hints, border");
        let text = sheet_text(SettingsSheet::default(), &settings, &features, wide);
        assert!(
            text.contains("├─ Look ─"),
            "one rule closes a group and opens the next: {text}"
        );
        assert!(
            !text.contains("└─") || text.matches("└─").count() == 1,
            "and only the last group closes with its own row"
        );
        let explanation = ROWS[0].explain();
        assert_eq!(
            text.matches(explanation).count(),
            1,
            "said once, on the row it belongs to"
        );

        // Narrow enough that the row trims the explanation. The block
        // underneath then takes only the rows the wrap needs.
        let narrow = Rect::new(0, 0, 64, 50);
        let reserved = SettingsSheetView::explain_rows(narrow);
        assert!((1..=5).contains(&reserved), "{reserved}");
        let (sheet, _) = SettingsSheetView::geometry(narrow).expect("a sheet");
        assert_eq!(sheet.height, controls + 3 + reserved);
        let text = sheet_text(SettingsSheet::default(), &settings, &features, narrow);
        assert!(
            text.contains(explanation),
            "the block says in full what the row had to trim: {text}"
        );
    }

    fn sheet_text(
        sheet: SettingsSheet,
        settings: &UiSettings,
        features: &TerminalFeatures,
        area: Rect,
    ) -> String {
        let buffer = render_sheet(sheet, settings, features, area);
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render_sheet(
        sheet: SettingsSheet,
        settings: &UiSettings,
        features: &TerminalFeatures,
        area: Rect,
    ) -> Buffer {
        render_sheet_for_set(sheet, settings, features, area, None)
    }

    fn render_sheet_for_set(
        sheet: SettingsSheet,
        settings: &UiSettings,
        features: &TerminalFeatures,
        area: Rect,
        set_limiter: Option<String>,
    ) -> Buffer {
        let mut buffer = Buffer::empty(area);
        SettingsSheetView {
            sheet,
            settings,
            features,
            tier: settings.rendering.resolve(features),
            registry: rustel_runtime::capability_registry(),
            build_features: &[],
            device: None,
            pressure: None,
            max_polyphony_override: None,
            capabilities: if features.keyboard {
                KeyboardCapabilities::enhanced()
            } else {
                KeyboardCapabilities::legacy()
            },
            #[cfg(feature = "hydra")]
            hydra_webcam: None,
            prebakes: [PrebakeRow::default(); 2],
            sets_folder: String::new(),
            recordings_folder: String::new(),
            set_limiter,
            theme: &Theme::default(),
            sample_cache: String::new(),
            precache: None,
            sources: Vec::new(),
            mappings: Default::default(),
            bindings: Vec::new(),
        }
        .render(area, &mut buffer);
        buffer
    }

    #[test]
    fn terminal_profile_selection_cycles_known_profiles_and_automatic() {
        let profiles: Vec<_> = super::super::terminal::conflicts::known().collect();
        assert!(!profiles.is_empty());
        let mut settings = UiSettings::default();
        let mut sheet = SettingsSheet {
            page: SettingsPage::Keybinds,
            selected: TERMINAL_PROFILE_ROW,
            keybind_count: RESET_KEYBINDS_ROW + 1,
            ..SettingsSheet::default()
        };
        let features = TerminalFeatures {
            name: "detected terminal".into(),
            ..Default::default()
        };
        assert!(settings.terminal_profile.is_none());
        assert_eq!(
            terminal_profile_label(&settings, &features),
            "automatic (detected terminal)"
        );
        for name in &profiles {
            assert_eq!(
                sheet.key(KeyCode::Right, &mut settings, &features),
                SettingsAction::Changed
            );
            assert_eq!(settings.terminal_profile.as_deref(), Some(*name));
        }
        assert_eq!(
            sheet.key(KeyCode::Enter, &mut settings, &features),
            SettingsAction::Changed
        );
        assert!(settings.terminal_profile.is_none());
        sheet.key(KeyCode::Left, &mut settings, &features);
        assert_eq!(
            settings.terminal_profile.as_deref(),
            profiles.last().copied()
        );
        assert_eq!(
            features.name, "detected terminal",
            "a profile cannot change measured capabilities"
        );
        assert_eq!(
            step_terminal_profile(Some(&profiles[0].to_uppercase()), false),
            None
        );
    }

    /// A profile pinned over another detected emulator names both on the
    /// row. The reset prompts keep the profile name alone.
    #[test]
    fn a_profile_pinned_over_another_terminal_names_both_on_the_row() {
        let features = TerminalFeatures {
            name: "kitty 0.35.2".into(),
            ..Default::default()
        };
        let mut settings = UiSettings {
            terminal_profile: Some("Windows Terminal".into()),
            ..Default::default()
        };
        assert_eq!(
            terminal_row_label(&settings, &features),
            "Windows Terminal (this terminal: kitty 0.35.2)"
        );
        assert_eq!(
            terminal_profile_label(&settings, &features),
            "Windows Terminal"
        );
        settings.terminal_profile = Some("kitty".into());
        assert_eq!(terminal_row_label(&settings, &features), "kitty");
        settings.terminal_profile = None;
        assert_eq!(
            terminal_row_label(&settings, &features),
            "automatic (kitty 0.35.2)"
        );
    }

    /// A mismatch label too long for a narrow row stays whole below the list.
    #[test]
    fn a_long_mismatch_label_wraps_below_the_keybind_list() {
        let features = TerminalFeatures {
            name: "GNOME Terminal 3.52.0".into(),
            ..Default::default()
        };
        let settings = UiSettings {
            terminal_profile: Some("Windows Terminal".into()),
            ..Default::default()
        };
        let sheet = SettingsSheet {
            page: SettingsPage::Keybinds,
            keybind_count: RESET_KEYBINDS_ROW + 1,
            ..Default::default()
        };
        let frame = Rect::new(0, 0, 60, 24);
        let (panel, rows) = sheet.geometry_for(frame).unwrap();
        let buffer = render_sheet(sheet, &settings, &features, frame);
        let footer = (rows.bottom()..panel.bottom().saturating_sub(1))
            .map(|y| {
                (rows.x..rows.right())
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join(" ");
        let footer = footer.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            footer.contains("Windows Terminal (this terminal: GNOME Terminal 3.52.0)"),
            "{footer}"
        );
    }

    #[test]
    fn the_terminal_profile_uses_the_full_keybind_row_even_in_a_narrow_panel() {
        let features = TerminalFeatures {
            name: "Windows Console".into(),
            ..Default::default()
        };
        let mut settings = UiSettings::default();
        let mut profiles = vec![None];
        profiles
            .extend(super::super::terminal::conflicts::known().map(|name| Some(name.to_owned())));
        for profile in profiles {
            settings.terminal_profile = profile;
            let value = terminal_profile_label(&settings, &features);
            let bindings = vec![KeybindRow {
                action: "Terminal",
                chord: value.clone(),
                ..Default::default()
            }];
            for width in [60, 80, 120] {
                let frame = Rect::new(0, 0, width, 24);
                let (_, rows) = SettingsSheetView::geometry(frame).unwrap();
                let mut buffer = Buffer::empty(frame);
                render_keybind_rows(
                    &bindings,
                    TERMINAL_PROFILE_ROW,
                    0,
                    rows,
                    &Theme::default(),
                    &mut buffer,
                );
                let line: String = (rows.x..rows.right())
                    .map(|x| buffer[(x, rows.y)].symbol())
                    .collect();
                assert!(
                    line.contains(&format!("Terminal: {value}")),
                    "width={width}: {line}"
                );
                assert!(!line.contains("default"));
            }
        }
    }

    #[test]
    fn a_long_detected_terminal_and_its_controls_wrap_below_the_keybind_list() {
        let features = TerminalFeatures {
            name: "tmux 3.5 via screen via Windows Terminal 1.20".into(),
            ..Default::default()
        };
        let settings = UiSettings::default();
        let sheet = SettingsSheet {
            page: SettingsPage::Keybinds,
            keybind_count: RESET_KEYBINDS_ROW + 1,
            ..Default::default()
        };
        let frame = Rect::new(0, 0, 60, 24);
        let (panel, rows) = sheet.geometry_for(frame).unwrap();
        let buffer = render_sheet(sheet, &settings, &features, frame);
        let footer = (rows.bottom()..panel.bottom().saturating_sub(1))
            .map(|y| {
                (rows.x..rows.right())
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join(" ");
        let footer = footer.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            footer.contains(&terminal_profile_label(&settings, &features)),
            "{footer}"
        );
        assert!(footer.contains("Left/Right: choose"), "{footer}");
        assert!(footer.contains("Del: automatic"), "{footer}");
    }

    #[test]
    fn keybind_learning_explains_keys_that_never_reach_the_app() {
        let features = TerminalFeatures {
            name: "iTerm2".into(),
            ..Default::default()
        };
        let settings = UiSettings::default();
        let theme = Theme::default();
        let sheet = SettingsSheet {
            page: SettingsPage::Keybinds,
            selected: KEYBIND_ACTION_START,
            keybind_count: KEYBIND_ACTION_START + 1,
            ..Default::default()
        };
        for width in [60, 100, 240] {
            let frame = Rect::new(0, 0, width, 24);
            let (panel, rows) = sheet.geometry_for(frame).unwrap();
            let mut buffer = Buffer::empty(frame);
            let view = SettingsSheetView {
                sheet,
                settings: &settings,
                features: &features,
                tier: settings.rendering.resolve(&features),
                registry: rustel_runtime::capability_registry(),
                build_features: &[],
                device: None,
                pressure: None,
                max_polyphony_override: None,
                capabilities: KeyboardCapabilities::legacy(),
                #[cfg(feature = "hydra")]
                hydra_webcam: None,
                prebakes: [PrebakeRow::default(); 2],
                sets_folder: String::new(),
                recordings_folder: String::new(),
                set_limiter: None,
                theme: &theme,
                sample_cache: String::new(),
                precache: None,
                sources: Vec::new(),
                mappings: Default::default(),
                bindings: vec![
                    KeybindRow::default(),
                    KeybindRow {
                        action: "First error",
                        learning: true,
                        ..Default::default()
                    },
                ],
            };
            render_keybinds(&view, panel, rows, &mut buffer);
            let footer = (rows.bottom()..panel.bottom().saturating_sub(1))
                .map(|y| {
                    (rows.x..rows.right())
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join(" ");
            let footer = footer.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(
                footer.contains("Press the shortcut · Esc: cancel"),
                "{footer}"
            );
            assert!(footer.contains("No response? Try another key"), "{footer}");
            assert!(
                footer.contains("check terminal/system shortcuts."),
                "{footer}"
            );
        }
    }

    #[test]
    fn profile_reset_and_binding_relearn_have_separate_row_actions() {
        let features = TerminalFeatures::default();
        let mut settings = UiSettings {
            terminal_profile: Some("kitty".into()),
            ..Default::default()
        };
        let mut sheet = SettingsSheet {
            page: SettingsPage::Keybinds,
            keybind_count: RESET_KEYBINDS_ROW + 1,
            ..Default::default()
        };
        assert_eq!(
            sheet.key(KeyCode::Delete, &mut settings, &features),
            SettingsAction::Changed
        );
        assert!(settings.terminal_profile.is_none());
        assert_eq!(
            sheet.key(KeyCode::Delete, &mut settings, &features),
            SettingsAction::Nothing
        );
        sheet.key(KeyCode::Down, &mut settings, &features);
        assert_eq!(
            sheet.key(KeyCode::Enter, &mut settings, &features),
            SettingsAction::LearnKeybind(BindAction::ALL[0])
        );
        sheet.select(KEYBIND_ACTION_START + BindAction::ALL.len() - 1);
        assert_eq!(
            sheet.key(KeyCode::Delete, &mut settings, &features),
            SettingsAction::ClearKeybind(*BindAction::ALL.last().unwrap())
        );
    }

    #[test]
    fn reset_all_keybinds_requires_two_enters_and_escape_cancels_only_confirmation() {
        let mut sheet = SettingsSheet {
            page: SettingsPage::Keybinds,
            selected: RESET_KEYBINDS_ROW,
            keybind_count: RESET_KEYBINDS_ROW + 1,
            ..SettingsSheet::default()
        };
        let mut settings = UiSettings::default();
        let features = TerminalFeatures::default();
        assert_eq!(
            sheet.key(KeyCode::Enter, &mut settings, &features),
            SettingsAction::AskResetKeybinds
        );
        assert!(sheet.confirm_reset_keybinds);
        assert_eq!(
            sheet.key(KeyCode::Esc, &mut settings, &features),
            SettingsAction::Nothing
        );
        assert!(!sheet.confirm_reset_keybinds);
        assert_eq!(
            sheet.key(KeyCode::Enter, &mut settings, &features),
            SettingsAction::AskResetKeybinds
        );
        assert_eq!(
            sheet.key(KeyCode::Enter, &mut settings, &features),
            SettingsAction::ResetKeybinds
        );
        assert!(!sheet.confirm_reset_keybinds);
        assert_eq!(
            sheet.key(KeyCode::Esc, &mut settings, &features),
            SettingsAction::Close
        );
    }

    #[test]
    fn leaving_reset_keybinds_cancels_confirmation_by_key_click_or_page() {
        let armed = SettingsSheet {
            page: SettingsPage::Keybinds,
            selected: RESET_KEYBINDS_ROW,
            keybind_count: RESET_KEYBINDS_ROW + 1,
            confirm_reset_keybinds: true,
            ..SettingsSheet::default()
        };
        let mut settings = UiSettings::default();
        let features = TerminalFeatures::default();
        for key in [KeyCode::Up, KeyCode::Down, KeyCode::Tab, KeyCode::BackTab] {
            let mut sheet = armed;
            sheet.key(key, &mut settings, &features);
            assert!(!sheet.confirm_reset_keybinds, "{key:?}");
        }
        let mut sheet = armed;
        sheet.select(0);
        assert!(!sheet.confirm_reset_keybinds);
        let mut sheet = armed;
        sheet.show_page(SettingsPage::Advanced);
        assert!(!sheet.confirm_reset_keybinds);
    }

    #[test]
    fn reset_all_footer_names_the_selected_profile() {
        let settings = UiSettings {
            terminal_profile: Some("kitty".into()),
            ..Default::default()
        };
        let features = TerminalFeatures {
            name: "Windows Terminal".into(),
            ..Default::default()
        };
        let mut sheet = SettingsSheet {
            page: SettingsPage::Keybinds,
            selected: RESET_KEYBINDS_ROW,
            keybind_count: RESET_KEYBINDS_ROW + 1,
            ..Default::default()
        };
        assert!(keybinds_footer_hint(sheet, &settings, &features).contains("kitty defaults"));
        sheet.confirm_reset_keybinds = true;
        assert!(keybinds_footer_hint(sheet, &settings, &features).ends_with("Esc: cancel"));
        // The panel rows close the table and rebind like the rest.
        sheet.select(RESET_KEYBINDS_ROW - 1);
        assert!(keybinds_footer_hint(sheet, &settings, &features).contains("rebind"));
    }

    #[test]
    fn keybind_pages_follow_the_visible_height_and_clamp_at_each_end() {
        let mut settings = UiSettings::default();
        let features = TerminalFeatures::default();
        for height in [18, 35] {
            let area = Rect::new(0, 0, 100, height);
            let mut sheet = SettingsSheet {
                page: SettingsPage::Keybinds,
                keybind_count: RESET_KEYBINDS_ROW + 1,
                ..Default::default()
            };
            let (_, rows) = sheet.geometry_for(area).unwrap();
            let shown = usize::from(rows.height);
            assert_eq!(
                sheet.key_in_frame(KeyCode::PageDown, &mut settings, &features, area),
                SettingsAction::Nothing
            );
            assert_eq!(sheet.selected, shown);
            assert!((sheet.first..sheet.first + shown).contains(&sheet.selected));
            sheet.key_in_frame(KeyCode::PageUp, &mut settings, &features, area);
            assert_eq!(sheet.selected, 0);
            sheet.key_in_frame(KeyCode::PageUp, &mut settings, &features, area);
            assert_eq!(sheet.selected, 0, "PageUp cannot wrap to the bottom");
            sheet.key_in_frame(KeyCode::End, &mut settings, &features, area);
            assert_eq!(sheet.selected, RESET_KEYBINDS_ROW);
            sheet.confirm_reset_keybinds = true;
            sheet.hold_scroll = true;
            sheet.key_in_frame(KeyCode::PageDown, &mut settings, &features, area);
            assert_eq!(sheet.selected, RESET_KEYBINDS_ROW);
            assert!(
                !sheet.confirm_reset_keybinds,
                "a page key cancels a pending reset even at the boundary"
            );
            assert!(!sheet.hold_scroll);
            sheet.key_in_frame(KeyCode::Home, &mut settings, &features, area);
            assert_eq!((sheet.selected, sheet.first), (0, 0));
        }
    }

    #[test]
    fn grouped_settings_pages_measure_display_lines_and_skip_headers() {
        let area = Rect::new(0, 0, 100, 18);
        let mut settings = UiSettings::default();
        let features = TerminalFeatures::default();
        for page in [SettingsPage::Settings, SettingsPage::Advanced] {
            let mut sheet = SettingsSheet {
                page,
                ..Default::default()
            };
            let (_, rows) = sheet.geometry_for(area).unwrap();
            let shown = usize::from(rows.height);
            let display = grouped_rows(sheet.rows());
            let first_control = display
                .iter()
                .position(|row| *row == DisplayRow::Control(0))
                .unwrap();
            sheet.key_in_frame(KeyCode::PageDown, &mut settings, &features, area);
            let selected_line = display
                .iter()
                .position(|row| *row == DisplayRow::Control(sheet.selected))
                .unwrap();
            assert!(selected_line >= first_control + shown, "{page:?}");
            assert!(
                display[first_control + shown..selected_line]
                    .iter()
                    .all(|row| !matches!(row, DisplayRow::Control(_)))
            );
            assert!((sheet.first..sheet.first + shown).contains(&selected_line));
            sheet.key_in_frame(KeyCode::End, &mut settings, &features, area);
            assert_eq!(sheet.selected, sheet.rows().len() - 1);
            sheet.key_in_frame(KeyCode::Home, &mut settings, &features, area);
            assert_eq!(sheet.selected, 0);
        }
        assert_eq!(
            settings,
            UiSettings::default(),
            "navigation must not change a setting"
        );
    }

    #[test]
    fn source_pages_cross_group_headings_without_losing_selection() {
        let area = Rect::new(0, 0, 100, 18);
        let mut settings = UiSettings::default();
        let features = TerminalFeatures::default();
        let mut sheet = SettingsSheet {
            page: SettingsPage::Sources,
            source_count: 4,
            default_count: 30,
            confirm_clear_cache: true,
            ..Default::default()
        };
        let shown = usize::from(sheet.geometry_for(area).unwrap().1.height);
        sheet.key_in_frame(KeyCode::PageDown, &mut settings, &features, area);
        assert!(!sheet.confirm_clear_cache);
        let lines = source_lines(sheet.source_count, sheet.default_count);
        let selected_line = lines.iter().position(|row| matches!(row, SourceLine::Row(index) | SourceLine::Control(index) if *index == sheet.selected)).unwrap();
        assert!(selected_line >= shown);
        assert!((sheet.first..sheet.first + shown).contains(&selected_line));
        sheet.key_in_frame(KeyCode::End, &mut settings, &features, area);
        assert_eq!(sheet.selected, SOURCE_CONTROL_COUNT + 4 + 30 - 1);
        sheet.key_in_frame(KeyCode::Home, &mut settings, &features, area);
        assert_eq!(sheet.selected, 0);
        sheet.source_count = 0;
        sheet.default_count = 0;
        sheet.key_in_frame(KeyCode::End, &mut settings, &features, area);
        assert_eq!(sheet.selected, SOURCE_CONTROL_COUNT - 1);
    }

    #[test]
    fn mapping_pages_scroll_boxes_and_mouse_hits_the_visible_slots() {
        let area = Rect::new(0, 0, 100, 18);
        let mut settings = UiSettings::default();
        let features = TerminalFeatures::default();
        let mut sheet = SettingsSheet {
            page: SettingsPage::Mapping,
            ..Default::default()
        };
        let (_, rows) = sheet.geometry_for(area).unwrap();
        assert!(
            usize::from(rows.height)
                < MAPPING_SLOTS / usize::from(MAPPING_COLUMNS) * usize::from(SLOT_HEIGHT)
        );
        sheet.key_in_frame(KeyCode::End, &mut settings, &features, area);
        let grid = SlotGrid::scrolled(rows, sheet.mapping_first(rows)).unwrap();
        let cell = grid
            .cell(MAPPING_SLOTS - 1)
            .expect("the last mapping becomes visible");
        assert_eq!(sheet.slot_at(area, cell.x, cell.y), Some(MAPPING_SLOTS - 1));
        assert!(sheet.first > 0);
        sheet.key_in_frame(KeyCode::PageUp, &mut settings, &features, area);
        assert!(sheet.selected < MAPPING_SLOTS - 1);
        sheet.key_in_frame(KeyCode::Home, &mut settings, &features, area);
        assert_eq!(sheet.first, 0);
        assert_eq!(sheet.selected, 0);
    }

    #[test]
    fn about_pages_reveal_the_report_tail_and_return_to_terminal_details() {
        let area = Rect::new(0, 0, 100, 18);
        let mut settings = UiSettings::default();
        let features = TerminalFeatures::default();
        let mut sheet = SettingsSheet {
            page: SettingsPage::About,
            ..Default::default()
        };
        let text = |sheet| {
            render_sheet(sheet, &settings, &features, area)
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        assert!(text(sheet).contains("Terminal:"));
        sheet.key_in_frame(KeyCode::End, &mut UiSettings::default(), &features, area);
        assert!(
            text(sheet).contains("callback"),
            "the last report line was previously unreachable"
        );
        assert!(!text(sheet).contains("Terminal:"));
        let at_end = sheet.first;
        sheet.key_in_frame(
            KeyCode::PageDown,
            &mut UiSettings::default(),
            &features,
            area,
        );
        assert_eq!(sheet.first, at_end);
        sheet.key_in_frame(KeyCode::PageUp, &mut UiSettings::default(), &features, area);
        assert!(sheet.first < at_end);
        sheet.key_in_frame(KeyCode::Home, &mut UiSettings::default(), &features, area);
        assert!(text(sheet).contains("Terminal:"));
        sheet.key_in_frame(KeyCode::Down, &mut UiSettings::default(), &features, area);
        assert_eq!(sheet.first, 1);
        sheet.key_in_frame(KeyCode::Tab, &mut settings, &features, area);
        assert_eq!(sheet.first, 0, "changing tabs resets the About scroll");
    }

    #[test]
    fn keybind_columns_align_long_labels_and_alternate_chords() {
        let bindings = vec![
            KeybindRow {
                action: "Terminal",
                chord: "automatic".into(),
                ..Default::default()
            },
            KeybindRow {
                action: "Evaluate",
                chord: "^Enter".into(),
                also: "F5".into(),
                ..Default::default()
            },
            KeybindRow {
                action: "An action label longer than its entire column",
                chord: "Shift+F5".into(),
                learnt: true,
                ..Default::default()
            },
            KeybindRow {
                action: "Show sample file",
                chord: "Alt+O".into(),
                ..Default::default()
            },
            KeybindRow {
                action: "Bank alias",
                chord: "Alt+R".into(),
                ..Default::default()
            },
            KeybindRow {
                action: "Trim sample silence",
                chord: "Alt+T".into(),
                ..Default::default()
            },
        ];
        for width in [35, 70, 100] {
            let area = Rect::new(3, 2, width, bindings.len() as u16);
            let mut buffer = Buffer::empty(area);
            render_keybind_rows(&bindings, 0, 0, area, &Theme::default(), &mut buffer);
            let lines: Vec<String> = (area.y..area.bottom())
                .map(|y| {
                    (area.x..area.right())
                        .map(|x| buffer[(x, y)].symbol())
                        .collect()
                })
                .collect();
            let chord = lines[1][..lines[1].find("^Enter").unwrap()].width();
            for (line, row) in lines.iter().zip(&bindings).skip(KEYBIND_ACTION_START) {
                assert_eq!(
                    line[..line.find(&row.chord).unwrap()].width(),
                    chord,
                    "{line}"
                );
            }
            if width >= 70 {
                assert_eq!(
                    lines[1][..lines[1].find("default").unwrap()].width(),
                    lines[2][..lines[2].find("overridden").unwrap()].width()
                );
                assert!(lines[1].contains("also F5"));
            }
        }
    }

    /// The first page asks one question about the limiter; the rest is on
    /// Advanced.
    ///
    /// "Do I want a limiter" is the whole of it most of the time, and it
    /// used to share the page with a ceiling and a makeup switch that
    /// meant nothing until it was answered. What a limiter added from
    /// here sounds like is a thing you set once.
    #[test]
    fn the_first_page_asks_one_thing_about_the_limiter() {
        assert!(ROWS.contains(&Row::MasterLimiter));
        for row in [
            Row::MasterLimiterCharacter,
            Row::MasterLimiterCeiling,
            Row::MasterLimiterMakeup,
        ] {
            assert!(!ROWS.contains(&row), "{row:?} belongs on Advanced");
            assert!(ADVANCED_ROWS.contains(&row), "{row:?} is missing there");
        }

        let features = TerminalFeatures::default();
        let area = Rect::new(0, 0, 120, 40);
        let text = |settings: &UiSettings, set: Option<&str>| {
            render_sheet_for_set(
                SettingsSheet::default(),
                settings,
                &features,
                area,
                set.map(str::to_owned),
            )
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
        };

        let settings = UiSettings {
            master_limiter_ceiling_db: -6.0,
            ..UiSettings::default()
        };
        let off = text(&settings, None);
        assert!(off.contains("master limiter"), "the switch is on page one");
        assert!(
            !off.contains("-6.0 dBFS"),
            "and the ceiling it would hold at is not: {off}"
        );

        // The switch is the studio's default, so the row also shows what
        // the open set uses. Each set answer fits the value column.
        for (set, expected) in [
            ("warm", "\u{25cb} off \u{b7} set warm"),
            ("punch byp", "\u{25cb} off \u{b7} set punch byp"),
            ("none", "\u{25cb} off \u{b7} set none"),
            ("unreadable", "\u{25cb} off \u{b7} set unreadable"),
        ] {
            let overruled = text(&settings, Some(set));
            assert!(
                overruled.contains(expected),
                "{set:?} came out as something else: {overruled}"
            );
        }
    }

    #[test]
    fn grouped_controls_draw_labels_and_skip_borders_when_scrolled() {
        let settings = UiSettings::default();
        let features = TerminalFeatures::default();
        for page in [SettingsPage::Settings, SettingsPage::Advanced] {
            let mut sheet = SettingsSheet {
                page,
                ..SettingsSheet::default()
            };
            for area in [
                Rect::new(0, 0, 80, 24),
                Rect::new(3, 2, 120, 35),
                Rect::new(0, 0, 60, 14),
            ] {
                let (_, rows) = SettingsSheetView::geometry(area).unwrap();
                for selected in 0..sheet.rows().len() {
                    sheet.selected = selected;
                    let buffer = render_sheet(sheet, &settings, &features, area);
                    let y = (rows.y..rows.bottom())
                        .find(|&y| sheet.row_at_for(area, rows.x + 2, y) == Some(selected))
                        .expect("selected control is visible");
                    assert_eq!(buffer.cell((rows.x + 1, y)).unwrap().symbol(), "▸");
                    assert_eq!(
                        sheet.row_at_for(area, rows.x, y),
                        None,
                        "left border is not a control"
                    );
                    assert_eq!(
                        sheet.row_at_for(area, rows.right() - 1, y),
                        None,
                        "right border is not a control"
                    );
                    for y in rows.y..rows.bottom() {
                        if matches!(buffer.cell((rows.x, y)).unwrap().symbol(), "┌" | "└") {
                            assert_eq!(
                                sheet.row_at_for(area, rows.x + 2, y),
                                None,
                                "group borders are not controls"
                            );
                        }
                    }
                }
            }
        }
        // Tall enough for every group of the longer page to be drawn:
        // a sheet that scrolls would hide the last of them.
        let area = Rect::new(0, 0, 120, 60);
        let buffer = render_sheet(SettingsSheet::default(), &settings, &features, area);
        let text = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        for label in ["Playback", "Look", "Editor", "Set"] {
            assert!(text.contains(label), "missing group {label}");
        }
        for label in ["Privacy", "Panels", "Visuals", "Controllers"] {
            assert!(
                !text.contains(label),
                "{label} is not a first-page group any more"
            );
        }
        let advanced = render_sheet(
            SettingsSheet {
                page: SettingsPage::Advanced,
                ..SettingsSheet::default()
            },
            &settings,
            &features,
            area,
        );
        let text = advanced
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        for label in ["Audio out", "Limiter", "Rendering", "Sliders"] {
            assert!(text.contains(label), "missing advanced group {label}");
        }
        // The page is longer than a window: the groups at the far end are
        // checked from the far end, which is what a reader scrolling to
        // them sees.
        let scrolled = render_sheet(
            SettingsSheet {
                page: SettingsPage::Advanced,
                selected: ADVANCED_ROWS.len() - 1,
                ..SettingsSheet::default()
            },
            &settings,
            &features,
            area,
        );
        let tail = scrolled
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        for label in ["Panels", "Sample memory"] {
            assert!(tail.contains(label), "missing advanced group {label}");
        }
        assert!(
            !tail.contains("Sample cache"),
            "the sample cache controls live on Sources now"
        );
    }

    #[test]
    fn advanced_rendering_cycles_only_supported_modes_and_privacy_defaults_off() {
        let mut settings = UiSettings::default();
        assert_eq!(settings.rendering, RenderingMode::Automatic);
        assert!(!settings.show_full_paths);
        let features = TerminalFeatures {
            fine_glyphs: true,
            kitty_graphics: true,
            cell_pixels: Some((10, 20)),
            ..TerminalFeatures::default()
        };
        let mut sheet = SettingsSheet {
            page: SettingsPage::Advanced,
            selected: ADVANCED_ROWS
                .iter()
                .position(|row| *row == Row::Rendering)
                .expect("the rendering row"),
            ..SettingsSheet::default()
        };
        for expected in [
            RenderingMode::Cells,
            RenderingMode::Fine,
            RenderingMode::Kitty,
            RenderingMode::Automatic,
        ] {
            assert_eq!(
                sheet.key(KeyCode::Right, &mut settings, &features),
                SettingsAction::Changed
            );
            assert_eq!(settings.rendering, expected);
        }
        sheet.key(KeyCode::Left, &mut settings, &features);
        assert_eq!(settings.rendering, RenderingMode::Kitty);
        let cell_only = TerminalFeatures {
            sixel: true,
            ..TerminalFeatures::default()
        };
        assert_eq!(settings.rendering.resolve(&cell_only), Tier::Cells);
        sheet.key(KeyCode::Right, &mut settings, &cell_only);
        assert_eq!(settings.rendering, RenderingMode::Cells);
        sheet.key(KeyCode::Right, &mut settings, &cell_only);
        assert_eq!(
            settings.rendering,
            RenderingMode::Automatic,
            "Sixel has no renderer and must not be offered"
        );
        sheet.show_page(SettingsPage::Settings);
        sheet.selected = ROWS
            .iter()
            .position(|row| *row == Row::ShowFullPaths)
            .unwrap();
        assert_eq!(
            sheet.key(KeyCode::Char(' '), &mut settings, &features),
            SettingsAction::Changed
        );
        assert!(settings.show_full_paths);
        sheet.show_page(SettingsPage::Advanced);
        assert_eq!(
            ADVANCED_ROWS[sheet.selected],
            Row::Rendering,
            "returning to Advanced restores the rendering control"
        );
    }

    /// The frame-rate row reads like a speed control: Right and Space ask
    /// for more of it, Left for less, even though the ladder is written
    /// fast first.
    #[test]
    fn the_frame_rate_row_reads_left_slow_and_right_fast() {
        let mut settings = UiSettings::default();
        let features = TerminalFeatures::default();
        let mut sheet = SettingsSheet {
            page: SettingsPage::Advanced,
            selected: ADVANCED_ROWS
                .iter()
                .position(|row| *row == Row::FrameRate)
                .expect("the frame-rate row"),
            ..SettingsSheet::default()
        };

        assert_eq!(
            sheet.key(KeyCode::Right, &mut settings, &features),
            SettingsAction::Changed
        );
        assert_eq!(settings.frame_rate, FrameRate::Double, "right speeds up");
        assert_eq!(
            sheet.key(KeyCode::Char(' '), &mut settings, &features),
            SettingsAction::Changed
        );
        assert_eq!(
            settings.frame_rate,
            FrameRate::Quadruple,
            "Space mirrors right"
        );
        assert_eq!(
            sheet.key(KeyCode::Left, &mut settings, &features),
            SettingsAction::Changed
        );
        assert_eq!(
            settings.frame_rate,
            FrameRate::Double,
            "left slows back down"
        );
    }

    #[test]
    fn about_checklist_requires_pixel_mouse_and_ignores_optional_command_chords() {
        let mut features = TerminalFeatures {
            name: "kitty".into(),
            truecolor: true,
            sync_output: true,
            keyboard: true,
            fine_glyphs: true,
            kitty_graphics: true,
            pixel_mouse: true,
            cell_pixels: Some((10, 20)),
            ..TerminalFeatures::default()
        };
        let capabilities = KeyboardCapabilities::enhanced();
        assert!(!capabilities.super_seen);
        assert!(
            terminal_checklist(&features, capabilities)
                .iter()
                .all(|(_, supported)| *supported == Some(true))
        );
        let settings = UiSettings {
            rendering: RenderingMode::Cells,
            ..UiSettings::default()
        };
        let sheet = SettingsSheet {
            page: SettingsPage::About,
            ..SettingsSheet::default()
        };
        let buffer = render_sheet(sheet, &settings, &features, Rect::new(0, 0, 80, 24));
        let text = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        for label in [
            "Terminal features · complete",
            "✓ Full colour",
            "✓ Precise sliders",
            "✓ Smooth redraws",
            "✓ Enhanced shortcuts",
            "✓ Pixel graphics",
            "✓ Shortcut profile",
            "Rendering: Cells",
        ] {
            assert!(text.contains(label), "{label} missing: {text}");
        }
        for jargon in ["⌘", "sixel", "24-bit", "cell size unknown"] {
            assert!(
                !text.contains(jargon),
                "unexpected protocol jargon {jargon}"
            );
        }
        features.pixel_mouse = false;
        let buffer = render_sheet(sheet, &settings, &features, Rect::new(0, 0, 120, 35));
        let text = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("× Precise sliders"));
        assert!(!text.contains("Terminal features · complete"));
        features.pixel_mouse = true;
        features.cell_pixels = None;
        let checklist = terminal_checklist(&features, capabilities);
        assert_eq!(checklist[1].1, Some(false));
        assert_eq!(checklist[4].1, Some(false));
        assert!(
            terminal_checklist(&TerminalFeatures::default(), KeyboardCapabilities::legacy())
                .iter()
                .all(|(_, supported)| supported.is_none())
        );
    }

    /// A switch has two states, so Left, Right and Space each flip it. A
    /// second press of the same key flips it back.
    #[test]
    fn every_change_key_flips_a_switch_rather_than_setting_it() {
        let mut settings = UiSettings::default();
        let mut sheet = SettingsSheet {
            selected: ROWS
                .iter()
                .position(|row| *row == Row::LineNumbers)
                .expect("a plain switch"),
            ..SettingsSheet::default()
        };
        let press = |sheet: &mut SettingsSheet, settings: &mut UiSettings, code| {
            sheet.key(code, settings, &TerminalFeatures::default());
            settings.line_numbers
        };
        let start = settings.line_numbers;
        for code in [KeyCode::Left, KeyCode::Right, KeyCode::Char(' ')] {
            assert_ne!(
                press(&mut sheet, &mut settings, code),
                start,
                "{code:?} flipped it"
            );
            assert_eq!(
                press(&mut sheet, &mut settings, code),
                start,
                "{code:?} flipped it back"
            );
        }
    }

    #[cfg(feature = "hydra")]
    #[test]
    fn the_webcam_rows_arrows_change_the_choice_and_show_the_camera() {
        let mut settings = UiSettings::default();
        assert!(!settings.hydra_webcam);
        let mut sheet = SettingsSheet {
            selected: ROWS
                .iter()
                .position(|row| *row == Row::HydraWebcam)
                .expect("webcam row"),
            ..SettingsSheet::default()
        };
        // The arrows step the choice as on any other row, and the preview
        // follows: a camera must not turn on unseen.
        for key in [KeyCode::Left, KeyCode::Right, KeyCode::Char(' ')] {
            settings.hydra_webcam = false;
            sheet.webcam_preview = false;
            assert_eq!(
                sheet.key(key, &mut settings, &TerminalFeatures::default()),
                SettingsAction::Changed
            );
            assert!(settings.hydra_webcam, "{key:?} chose the other value");
            assert!(sheet.webcam_preview_open(), "{key:?} showed the camera");

            // And again, from inside the open preview, which is the thing
            // that used to stop answering once the picture was up. Landing
            // back on OFF takes the picture with it: there is nothing left
            // to look at, and a preview of a camera that is off is the
            // device held open to show nobody anything.
            assert_eq!(
                sheet.key(key, &mut settings, &TerminalFeatures::default()),
                SettingsAction::Changed
            );
            assert!(!settings.hydra_webcam, "{key:?} loops back round");
            assert!(
                !sheet.webcam_preview_open(),
                "{key:?} switched it off, so the picture goes too"
            );
        }
        assert_eq!(Row::HydraWebcam.label(), "Hydra webcam");
    }

    /// Enter on the row must never be the key that turns a camera on: the
    /// point of a preview is that the picture comes first and the switch
    /// waits for a second, knowing press.
    #[cfg(feature = "hydra")]
    #[test]
    fn enter_on_the_webcam_row_opens_a_preview_and_leaves_the_switch_off() {
        let mut settings = UiSettings::default();
        let mut sheet = SettingsSheet {
            selected: ROWS
                .iter()
                .position(|row| *row == Row::HydraWebcam)
                .expect("webcam row"),
            ..SettingsSheet::default()
        };
        assert_eq!(
            sheet.key(KeyCode::Enter, &mut settings, &TerminalFeatures::default()),
            SettingsAction::OpenWebcamPreview
        );
        assert!(!settings.hydra_webcam, "browsing here must not enable it");
        assert!(sheet.webcam_preview_open());
    }

    /// Enter is the look, never the yes: a second press puts the picture
    /// away and the choice is left exactly where the arrows left it.
    #[cfg(feature = "hydra")]
    #[test]
    fn a_second_enter_puts_the_picture_away_and_leaves_the_choice_alone() {
        let mut settings = UiSettings::default();
        let mut sheet = SettingsSheet {
            selected: ROWS
                .iter()
                .position(|row| *row == Row::HydraWebcam)
                .expect("webcam row"),
            ..SettingsSheet::default()
        };
        sheet.key(KeyCode::Enter, &mut settings, &TerminalFeatures::default());
        assert!(sheet.webcam_preview_open(), "the first Enter looks");
        assert_eq!(
            sheet.key(KeyCode::Enter, &mut settings, &TerminalFeatures::default()),
            SettingsAction::Nothing
        );
        assert!(
            !settings.hydra_webcam,
            "Enter is the look, not the yes - only the arrows choose"
        );
        assert!(
            !sheet.webcam_preview_open(),
            "the second Enter puts it away"
        );

        // And Esc does the same from an open picture, without closing the
        // sheet underneath it.
        sheet.key(KeyCode::Enter, &mut settings, &TerminalFeatures::default());
        assert!(sheet.webcam_preview_open());
        assert_eq!(
            sheet.key(KeyCode::Esc, &mut settings, &TerminalFeatures::default()),
            SettingsAction::Nothing,
            "Esc takes the picture, not the sheet"
        );
        assert!(!sheet.webcam_preview_open());
        assert_eq!(
            sheet.key(KeyCode::Esc, &mut settings, &TerminalFeatures::default()),
            SettingsAction::Close,
            "and the next Esc closes the sheet as ever"
        );
    }

    /// The syntax check is an editor row on the first page with three
    /// modes, and it ships full: the check is the studio's feedback loop,
    /// and the other two are for whoever finds the marks a nuisance.
    #[test]
    fn syntax_check_is_an_editor_row_of_three_modes_that_ships_full() {
        let mut settings = UiSettings::default();
        assert_eq!(settings.syntax_check, SyntaxCheck::Full);
        assert!(ROWS.contains(&Row::SyntaxCheck));
        assert!(!ADVANCED_ROWS.contains(&Row::SyntaxCheck));
        assert_eq!(Row::SyntaxCheck.group(), "Editor");
        assert_eq!(Row::SyntaxCheck.label(), "syntax check");

        let mut sheet = SettingsSheet {
            selected: ROWS
                .iter()
                .position(|row| *row == Row::SyntaxCheck)
                .expect("syntax check row"),
            ..SettingsSheet::default()
        };
        let features = TerminalFeatures::default();
        assert_eq!(
            sheet.key(KeyCode::Left, &mut settings, &features),
            SettingsAction::Changed
        );
        assert_eq!(settings.syntax_check, SyntaxCheck::Off, "left wraps to off");
        for (key, mode) in [
            (KeyCode::Right, SyntaxCheck::Full),
            (KeyCode::Right, SyntaxCheck::OnUpdate),
            (KeyCode::Char(' '), SyntaxCheck::Off),
        ] {
            assert_eq!(
                sheet.key(key, &mut settings, &features),
                SettingsAction::Changed
            );
            assert_eq!(settings.syntax_check, mode);
        }
        for mode in [SyntaxCheck::Full, SyntaxCheck::OnUpdate, SyntaxCheck::Off] {
            assert_eq!(
                SyntaxCheck::parse(mode.key()),
                mode,
                "{mode:?} survives its key"
            );
        }
        assert_eq!(SyntaxCheck::parse("nonsense"), SyntaxCheck::Full);
    }

    #[test]
    fn evaluation_flash_cycles_all_three_modes_on_the_editor_page() {
        let mut settings = UiSettings::default();
        assert_eq!(settings.evaluation_flash, EvaluationFlashMode::Full);
        assert!(!ADVANCED_ROWS.contains(&Row::EvaluationFlash));
        assert_eq!(Row::EvaluationFlash.group(), "Editor");
        assert_eq!(Row::EvaluationFlash.label(), "flashing on evaluation");

        let mut sheet = SettingsSheet {
            selected: ROWS
                .iter()
                .position(|row| *row == Row::EvaluationFlash)
                .expect("evaluation flash row"),
            ..SettingsSheet::default()
        };
        let features = TerminalFeatures::default();
        for (key, mode) in [
            (KeyCode::Left, EvaluationFlashMode::Off),
            (KeyCode::Right, EvaluationFlashMode::Full),
            (KeyCode::Right, EvaluationFlashMode::OnSuccess),
            (KeyCode::Char(' '), EvaluationFlashMode::Off),
        ] {
            assert_eq!(
                sheet.key(key, &mut settings, &features),
                SettingsAction::Changed
            );
            assert_eq!(settings.evaluation_flash, mode);
            for width in [60, 80, 120] {
                let text = sheet_text(sheet, &settings, &features, Rect::new(0, 0, width, 24));
                let row = text
                    .lines()
                    .find(|line| line.contains("flashing on evaluation"))
                    .expect("selected evaluation flash row is visible");
                assert!(row.contains(mode.label()), "width={width}: {row}");
            }
        }
    }

    #[test]
    fn all_visual_opacities_are_renderer_neutral_and_stay_bounded() {
        let mut settings = UiSettings::default();
        let mut sheet = SettingsSheet {
            selected: ROWS
                .iter()
                .position(|row| *row == Row::BackdropStrength)
                .expect("visuals opacity row"),
            ..SettingsSheet::default()
        };
        let features = TerminalFeatures::default();

        settings.backdrop_opacity = 0;
        assert_eq!(
            sheet.key(KeyCode::Left, &mut settings, &features),
            SettingsAction::Nothing
        );
        assert_eq!(
            sheet.key(KeyCode::Right, &mut settings, &features),
            SettingsAction::Changed
        );
        assert_eq!(settings.backdrop_opacity, OPACITY_STEP);

        settings.backdrop_opacity = 100;
        assert_eq!(
            sheet.key(KeyCode::Right, &mut settings, &features),
            SettingsAction::Nothing
        );
        assert_eq!(settings.backdrop_opacity, 100);
        assert_eq!(Row::BackdropStrength.label(), "visuals opacity");

        sheet.selected = ROWS
            .iter()
            .position(|row| *row == Row::InterfaceOpacity)
            .expect("ui opacity row");
        settings.interface_opacity = 50;
        assert_eq!(
            sheet.key(KeyCode::Left, &mut settings, &features),
            SettingsAction::Changed
        );
        assert_eq!(settings.interface_opacity, 45);
        assert_eq!(Row::InterfaceOpacity.label(), "ui opacity");

        sheet.selected = ROWS
            .iter()
            .position(|row| *row == Row::EditorOpacity)
            .expect("editor opacity row");
        settings.editor_opacity = 50;
        assert_eq!(
            sheet.key(KeyCode::Right, &mut settings, &features),
            SettingsAction::Changed
        );
        assert_eq!(settings.editor_opacity, 55);
        assert_eq!(Row::EditorOpacity.label(), "editor opacity");
    }

    #[test]
    fn chrome_visibility_rows_default_on_and_flip_like_other_switches() {
        let mut settings = UiSettings::default();
        let features = TerminalFeatures::default();
        assert!(settings.show_menu && settings.show_header && settings.show_footer);
        assert!(!settings.zen);
        let shown = |settings: &UiSettings, row| match row {
            Row::ShowMenu => settings.show_menu,
            Row::ShowHeader => settings.show_header,
            Row::ShowFooter => settings.show_footer,
            _ => unreachable!(),
        };
        for row in [Row::ShowMenu, Row::ShowHeader, Row::ShowFooter] {
            assert_eq!(row.group(), "Look");
            let mut sheet = SettingsSheet {
                selected: ROWS
                    .iter()
                    .position(|item| *item == row)
                    .unwrap_or_else(|| panic!("{} row", row.label())),
                ..SettingsSheet::default()
            };
            assert!(shown(&settings, row));
            assert_eq!(
                sheet.key(KeyCode::Char(' '), &mut settings, &features),
                SettingsAction::Changed
            );
            assert!(!shown(&settings, row), "{} did not turn off", row.label());
            assert_eq!(
                sheet.key(KeyCode::Right, &mut settings, &features),
                SettingsAction::Changed
            );
            assert!(
                shown(&settings, row),
                "{} did not turn back on",
                row.label()
            );
        }
        assert_eq!(Row::ShowMenu.label(), "menu bar");
        assert_eq!(Row::ShowHeader.label(), "header");
        assert_eq!(Row::ShowFooter.label(), "footer");
        assert!(!settings.zen, "the chrome rows are not zen");
    }

    #[test]
    fn enter_opens_a_prebake_and_esc_still_closes() {
        let mut sheet = SettingsSheet::default();
        let mut settings = UiSettings::default();
        let features = TerminalFeatures::default();

        sheet.selected = ROWS
            .iter()
            .position(|row| *row == Row::GlobalPrebake)
            .expect("the global prebake row");
        assert_eq!(
            sheet.key(KeyCode::Enter, &mut settings, &features),
            SettingsAction::OpenPrebake(PrebakeScope::Global)
        );
        sheet.selected = ROWS
            .iter()
            .position(|row| *row == Row::LocalPrebake)
            .expect("the local prebake row");
        assert_eq!(
            sheet.key(KeyCode::Enter, &mut settings, &features),
            SettingsAction::OpenPrebake(PrebakeScope::Local)
        );
        assert_eq!(
            sheet.key(KeyCode::Esc, &mut settings, &features),
            SettingsAction::Close
        );

        // On a switch, Enter still closes the sheet as it always did.
        sheet.selected = 0;
        assert_eq!(
            sheet.key(KeyCode::Enter, &mut settings, &features),
            SettingsAction::Close
        );
        // And the About page claims nothing but its own paging.
        sheet.selected = ROWS.len() - 1;
        sheet.show_page(SettingsPage::About);
        assert_eq!(
            sheet.key(KeyCode::Enter, &mut settings, &features),
            SettingsAction::Close
        );
    }

    #[test]
    fn the_arrows_do_nothing_on_a_prebake_row() {
        let mut sheet = SettingsSheet::default();
        let mut settings = UiSettings::default();
        let before = settings.clone();
        let features = TerminalFeatures::default();
        sheet.selected = ROWS
            .iter()
            .position(|row| *row == Row::LocalPrebake)
            .expect("local prebake row");

        for code in [KeyCode::Right, KeyCode::Left, KeyCode::Char(' ')] {
            assert_eq!(
                sheet.key(code, &mut settings, &features),
                SettingsAction::Nothing,
                "a prebake row claimed to change a setting"
            );
        }
        assert_eq!(settings, before);
    }

    #[test]
    fn the_prebake_rows_say_what_is_there_and_whether_it_ran() {
        let settings = UiSettings::default();
        let features = TerminalFeatures::default();
        let theme = Theme::resolve(None).expect("theme");
        let registry = rustel_runtime::capability_registry();
        let area = Rect::new(0, 0, 120, ROWS.len() as u16 + 10);
        let mut buffer = Buffer::empty(area);
        let sheet = SettingsSheet {
            selected: ROWS.len() - 1,
            ..SettingsSheet::default()
        };

        SettingsSheetView {
            sheet,
            settings: &settings,
            features: &features,
            tier: Tier::Cells,
            registry,
            build_features: &[],
            device: None,
            pressure: None,
            max_polyphony_override: None,
            capabilities: KeyboardCapabilities::legacy(),
            prebakes: [
                PrebakeRow {
                    lines: 3,
                    verdict: PrebakeVerdict::Applied,
                    ..PrebakeRow::default()
                },
                PrebakeRow {
                    lines: 7,
                    verdict: PrebakeVerdict::Rejected,
                    dirty: true,
                    ..PrebakeRow::default()
                },
            ],
            sets_folder: String::new(),
            recordings_folder: String::new(),
            set_limiter: None,
            sample_cache: String::new(),
            precache: None,
            sources: Vec::new(),
            mappings: Default::default(),
            bindings: Vec::new(),
            #[cfg(feature = "hydra")]
            hydra_webcam: None,
            theme: &theme,
        }
        .render(area, &mut buffer);

        let text = buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(text.contains("global prebake"), "{text}");
        assert!(text.contains("applied · 3 lines"), "{text}");
        assert!(text.contains("local prebake"), "{text}");
        assert!(text.contains("rejected · 7 lines ●"), "{text}");
        assert!(text.contains("Enter opens it"), "{text}");
        assert!(
            !ROWS.contains(&Row::Rendering),
            "rendering belongs on Advanced: {text}"
        );
    }

    /// The shipped packs are on the Sources page, after the imports and
    /// drawn apart from them. The library download starts there and shows
    /// its progress pack by pack. The Advanced page has no row for it.
    #[test]
    fn the_shipped_packs_are_listed_after_the_imports_and_cached_from_there() {
        assert!(
            !ADVANCED_ROWS
                .iter()
                .any(|row| row.label() == "cache whole library"),
            "the library's download moved to Sources"
        );

        let settings = UiSettings::default();
        let features = TerminalFeatures::default();
        let area = Rect::new(0, 0, 190, 50);
        let mut sheet = SettingsSheet {
            page: SettingsPage::Sources,
            source_count: 2,
            default_count: 2,
            selected: SOURCE_CONTROL_COUNT + 2 + 2 - 1,
            ..SettingsSheet::default()
        };
        let imported = SourceRow {
            spec: "/home/me/kit".to_owned(),
            label: "kit".to_owned(),
            state: "12 sound(s)".to_owned(),
            dimmed: false,
            shipped: false,
            local: true,
            cache_fill: None,
            cache: None,
        };
        let remote = SourceRow {
            spec: "github:me/kit".to_owned(),
            label: "me/kit".to_owned(),
            state: "8 sound(s) · 10/34 cached · 4.2 MiB".to_owned(),
            dimmed: false,
            shipped: false,
            local: false,
            cache_fill: Some((10, 34)),
            cache: None,
        };
        let piano = SourceRow {
            spec: "https://strudel.b-cdn.net/piano.json".to_owned(),
            label: "piano".to_owned(),
            state: "1 sound(s) · 34/34 cached · 12.5 MiB".to_owned(),
            dimmed: false,
            shipped: true,
            local: false,
            cache_fill: Some((34, 34)),
            cache: None,
        };
        let drums = SourceRow {
            spec: "https://strudel.b-cdn.net/tidal-drum-machines.json".to_owned(),
            label: "tidal-drum-machines".to_owned(),
            state: "300 sound(s) · 1000/4000 cached".to_owned(),
            dimmed: false,
            shipped: true,
            local: false,
            cache_fill: Some((1000, 4000)),
            cache: Some(SourceCacheProgress {
                total: 4_000,
                left: 3_000,
                loading: Some("kick.wav".to_owned()),
            }),
        };
        let render =
            |sheet: SettingsSheet, precache: Option<PrecacheProgress>, sources: &[SourceRow]| {
                let mut buffer = Buffer::empty(area);
                SettingsSheetView {
                    sheet,
                    settings: &settings,
                    features: &features,
                    tier: settings.rendering.resolve(&features),
                    registry: rustel_runtime::capability_registry(),
                    build_features: &[],
                    device: None,
                    pressure: None,
                    max_polyphony_override: None,
                    capabilities: KeyboardCapabilities::enhanced(),
                    #[cfg(feature = "hydra")]
                    hydra_webcam: None,
                    prebakes: [PrebakeRow::default(); 2],
                    sets_folder: String::new(),
                    recordings_folder: String::new(),
                    set_limiter: None,
                    theme: &Theme::default(),
                    sample_cache: String::new(),
                    precache,
                    sources: sources.to_vec(),
                    mappings: Default::default(),
                    bindings: Vec::new(),
                }
                .render(area, &mut buffer);
                (0..area.height)
                    .map(|y| {
                        (0..area.width)
                            .map(|x| buffer.cell((x, y)).unwrap().symbol().to_string())
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
            };
        let default_sources = [
            imported.clone(),
            remote.clone(),
            piano.clone(),
            drums.clone(),
        ];

        // At rest: the import first, the rule, then the packs with their
        // glyph - and the rule says how the download is started.
        let lines = render(sheet, None, &default_sources);
        let at = |needle: &str| {
            lines
                .iter()
                .position(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("{needle:?} is on the page:\n{}", lines.join("\n")))
        };
        assert!(at("sample cache") < at("fetch imports"));
        assert!(at("fetch imports") < at("cache all"));
        assert!(at("cache all") < at("refresh all"));
        assert!(at("refresh all") < at("clear cache"));
        assert!(at("clear cache") < at("user samples"));
        assert!(at("user samples") < at("kit (local)"));
        assert!(at("kit (local)") < at("me/kit"));
        assert!(at("me/kit") < at("default samples"));
        assert!(at("default samples") < at("◇ piano"));
        assert!(at("◇ piano") < at("▸ ◇ tidal-drum-machines"));
        assert!(at("▸ ◇ tidal-drum-machines") < at("Drag a sample folder"));
        assert!(
            !lines.iter().any(|line| line.contains("◇ kit")),
            "an import wears no glyph"
        );
        assert!(
            lines[at("kit (local)")].contains("12 sound(s)"),
            "{}",
            lines[at("kit (local)")]
        );
        assert!(
            lines[at("me/kit")].contains("10/34 cached") && lines[at("me/kit")].contains("4.2 MiB"),
            "{}",
            lines[at("me/kit")]
        );
        let mut on_remote = sheet;
        on_remote.selected = SOURCE_CONTROL_COUNT + 1;
        let remote_lines = render(on_remote, None, &default_sources);
        assert!(
            remote_lines
                .iter()
                .any(|line| line.contains("c caches this pack")),
            "a remote user pack can be cached from its row:\n{}",
            remote_lines.join("\n")
        );
        let mut on_local = sheet;
        on_local.selected = SOURCE_CONTROL_COUNT;
        let local_lines = render(on_local, None, &default_sources);
        assert!(
            !local_lines
                .iter()
                .any(|line| line.contains("c caches this pack")),
            "a local folder has nothing to cache:\n{}",
            local_lines.join("\n")
        );
        // The selected pack shows its address, and the one being cached
        // shows its own files coming in.
        assert_eq!(
            at("▸ ◇ tidal-drum-machines"),
            at("https://strudel.b-cdn.net/tidal-drum-machines.json")
        );
        assert!(
            lines[at("◇ tidal-drum-machines")].contains("██░░░░░░ 25% · 1000/4000 · kick.wav"),
            "{}",
            lines[at("◇ tidal-drum-machines")]
        );
        assert!(lines[at("◇ piano")].contains("1 sound(s) · 34/34 cached"));
        assert!(
            !lines.iter().any(|line| line.contains("✓ all cached")),
            "a partial pack keeps cache all quiet:\n{}",
            lines.join("\n")
        );

        let all_cached_sources = [
            imported.clone(),
            SourceRow {
                state: "8 sound(s) · 34/34 cached · 4.2 MiB".to_owned(),
                cache_fill: Some((34, 34)),
                ..remote.clone()
            },
            piano.clone(),
            SourceRow {
                cache_fill: Some((4000, 4000)),
                cache: None,
                state: "4000 sound(s) · 4000/4000 cached".to_owned(),
                ..drums.clone()
            },
        ];
        let all_cached_lines = render(sheet, None, &all_cached_sources);
        assert!(
            all_cached_lines
                .iter()
                .any(|line| line.contains("✓ all cached")),
            "every remote pack full marks cache all:\n{}",
            all_cached_lines.join("\n")
        );

        // While the whole library is being cached, the rule carries the
        // total; the page's hint says what the keys do.
        let lines = render(
            sheet,
            Some(PrecacheProgress {
                kind: PrecacheKind::Library,
                total: 4_034,
                left: 3_000,
                loading: None,
            }),
            &default_sources,
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("default samples · 1034/4034")),
            "{}",
            lines.join("\n")
        );
        assert!(
            lines.iter().any(|line| line.contains("c caches this pack")),
            "{}",
            lines.join("\n")
        );
        assert!(
            !lines.iter().any(|line| line.contains("C caches all")),
            "cache-all is a control, not a pack shortcut:\n{}",
            lines.join("\n")
        );

        // The keys: c on a pack caches it, Enter on cache all caches
        // them all, Enter on a pack does not, and the editing keys have
        // nothing to do on a pack - but they are taken, not passed on.
        let mut changed = settings.clone();
        assert_eq!(
            sheet.key(KeyCode::Enter, &mut changed, &features),
            SettingsAction::Nothing
        );
        assert_eq!(
            sheet.key(KeyCode::Char('c'), &mut changed, &features),
            SettingsAction::CacheDefaultSource(1)
        );
        assert_eq!(
            sheet.key(KeyCode::Char('C'), &mut changed, &features),
            SettingsAction::Ignored
        );
        sheet.selected = 1;
        assert_eq!(
            sheet.key(KeyCode::Enter, &mut changed, &features),
            SettingsAction::CacheWholeLibrary
        );
        sheet.selected = SOURCE_CONTROL_COUNT + 1;
        assert_eq!(
            sheet.key(KeyCode::Char('c'), &mut changed, &features),
            SettingsAction::CacheImportSource(1),
            "c on a remote user pack caches that pack"
        );
        sheet.selected = SOURCE_CONTROL_COUNT;
        assert_eq!(
            sheet.key(KeyCode::Char('c'), &mut changed, &features),
            SettingsAction::CacheImportSource(0),
            "c on a local folder is handed to the app, which no-ops"
        );
        sheet.selected = SOURCE_CONTROL_COUNT + sheet.source_count + sheet.default_count - 1;
        for code in [
            KeyCode::Char('d'),
            KeyCode::Char(' '),
            KeyCode::Char('['),
            KeyCode::Delete,
            KeyCode::Right,
        ] {
            assert_eq!(
                sheet.key(code, &mut changed, &features),
                SettingsAction::Nothing,
                "{code:?} on a shipped pack"
            );
        }
        // Up walks back onto the first import, where the keys are the list's.
        sheet.key(KeyCode::Up, &mut changed, &features);
        sheet.key(KeyCode::Up, &mut changed, &features);
        sheet.key(KeyCode::Up, &mut changed, &features);
        assert_eq!(sheet.selected, SOURCE_CONTROL_COUNT);
        assert_eq!(
            sheet.key(KeyCode::Enter, &mut changed, &features),
            SettingsAction::RefreshSource(0)
        );
        assert_eq!(
            sheet.key(KeyCode::Char('d'), &mut changed, &features),
            SettingsAction::RemoveSource(0)
        );
        sheet.selected = 2;
        assert_eq!(
            sheet.key(KeyCode::Enter, &mut changed, &features),
            SettingsAction::RefreshAllSources
        );
        // And Up from the first row wraps to the last pack.
        sheet.selected = 0;
        sheet.key(KeyCode::Up, &mut changed, &features);
        assert_eq!(
            sheet.selected,
            SOURCE_CONTROL_COUNT + sheet.source_count + sheet.default_count - 1
        );
        assert_eq!(changed, settings, "nothing on this page is a setting");

        assert!(
            !ADVANCED_ROWS.contains(&Row::PrecacheSources)
                && !ADVANCED_ROWS.contains(&Row::CacheDefaults)
                && !ADVANCED_ROWS.contains(&Row::RefreshSources)
                && !ADVANCED_ROWS.contains(&Row::SampleCache),
            "the cache controls moved to Sources"
        );

        // The imports switch never wears the library's download; that
        // progress sits on cache all instead.
        let mut sources = SettingsSheet {
            page: SettingsPage::Sources,
            ..SettingsSheet::default()
        };
        sources.selected = 0;
        let mut buffer = Buffer::empty(area);
        SettingsSheetView {
            sheet: sources,
            settings: &settings,
            features: &features,
            tier: settings.rendering.resolve(&features),
            registry: rustel_runtime::capability_registry(),
            build_features: &[],
            device: None,
            pressure: None,
            max_polyphony_override: None,
            capabilities: KeyboardCapabilities::enhanced(),
            #[cfg(feature = "hydra")]
            hydra_webcam: None,
            prebakes: [PrebakeRow::default(); 2],
            sets_folder: String::new(),
            recordings_folder: String::new(),
            set_limiter: None,
            theme: &Theme::default(),
            sample_cache: String::new(),
            precache: Some(PrecacheProgress {
                kind: PrecacheKind::Library,
                total: 4_000,
                left: 3_000,
                loading: Some("kick.wav".to_owned()),
            }),
            sources: Vec::new(),
            mappings: Default::default(),
            bindings: Vec::new(),
        }
        .render(area, &mut buffer);
        let lines = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let fetch = lines
            .iter()
            .find(|line| line.contains("fetch imports"))
            .expect("fetch imports row");
        assert!(
            fetch.contains("○ off") && !fetch.contains("1000/4000") && !fetch.contains("kick.wav"),
            "fetch imports does not follow the library download: {fetch}"
        );
        let defaults = lines
            .iter()
            .find(|line| line.contains("cache all"))
            .expect("cache all row");
        assert!(
            defaults.contains("1000/4000") && defaults.contains("kick.wav"),
            "cache all follows the library download: {defaults}"
        );
    }

    /// "fetch imports" only ever writes to disk, and the row is where that
    /// has to be read: "cache imported sounds" was taken for a setting that
    /// holds sounds in memory. An 80-column sheet leaves the row 32 columns
    /// of its explanation, so the words that say where the sounds go come
    /// first and are the ones that survive the cut.
    #[test]
    fn fetch_imports_says_it_downloads_to_disk_on_an_eighty_column_sheet() {
        let sheet = SettingsSheet {
            page: SettingsPage::Sources,
            ..SettingsSheet::default()
        };
        let text = sheet_text(
            sheet,
            &UiSettings::default(),
            &TerminalFeatures::default(),
            Rect::new(0, 0, 80, 40),
        );
        let row = text
            .lines()
            .find(|line| line.contains("fetch imports"))
            .expect("the switch is on the page");
        assert!(row.contains("download imported sounds to disk"), "{row}");
    }

    /// fetch imports lives on the samples page, which is otherwise a list,
    /// but the row itself is a switch and answers the same keys as one.
    #[test]
    fn left_and_right_flip_fetch_imports_like_any_other_switch() {
        let mut settings = UiSettings::default();
        assert!(!settings.precache_sources);
        let mut sheet = SettingsSheet {
            page: SettingsPage::Sources,
            ..SettingsSheet::default()
        };
        let features = TerminalFeatures::default();
        for code in [KeyCode::Left, KeyCode::Right, KeyCode::Char(' ')] {
            assert_eq!(
                sheet.key(code, &mut settings, &features),
                SettingsAction::Changed,
                "{code:?} changes fetch imports"
            );
            assert!(settings.precache_sources, "{code:?} turned it on");
            assert_eq!(
                sheet.key(code, &mut settings, &features),
                SettingsAction::Changed
            );
            assert!(!settings.precache_sources, "{code:?} turned it back off");
        }
    }

    #[test]
    fn sample_drop_hint_fits_narrow_sheets_and_stays_outside_the_scrolling_rows() {
        for width in [60, 80, 190] {
            for height in [16, 40] {
                let area = Rect::new(0, 0, width, height);
                let mut sheet = SettingsSheet {
                    page: SettingsPage::Sources,
                    selected: SOURCE_CONTROL_COUNT - 1,
                    ..SettingsSheet::default()
                };
                sheet.settle_scroll(area);
                let (panel, rows) = sheet.geometry_for(area).expect("sheet fits");
                let text = sheet_text(
                    sheet,
                    &UiSettings::default(),
                    &TerminalFeatures::default(),
                    area,
                );
                let lines: Vec<_> = text.lines().collect();
                let hint_y = panel.bottom() - 3;
                assert!(hint_y >= rows.bottom(), "hint overlaps the list: {text}");
                assert!(
                    lines[usize::from(hint_y)]
                        .contains("Drag a sample folder onto this window to import it."),
                    "{width}x{height}: {text}"
                );
                assert!(lines[usize::from(hint_y + 1)].contains("a adds"));
                assert_eq!(sheet.source_row_at(area, rows.x, hint_y), None);
            }
        }
    }

    /// A click on the sources page lands on the row drawn there: the
    /// import, the rule and the packs are found by the same lines the page
    /// draws, so what is under the pointer is what gets selected - and the
    /// rule and the "no imports" line, which are not rows, select nothing.
    #[test]
    fn a_click_on_the_sources_page_finds_the_row_drawn_there() {
        let area = Rect::new(0, 0, 120, 40);
        let sheet = SettingsSheet {
            page: SettingsPage::Sources,
            source_count: 2,
            default_count: 3,
            ..SettingsSheet::default()
        };
        let (_, rows) = sheet.geometry_for(area).expect("a sheet fits");
        let x = rows.x + 3;
        // Boxed groups: cache header + 4 controls + gap + user header +
        // 2 imports + gap + defaults header + 3 packs + footer.
        assert_eq!(sheet.source_row_at(area, x, rows.y), None, "cache header");
        assert_eq!(sheet.source_row_at(area, x, rows.y + 1), Some(0));
        assert_eq!(sheet.source_row_at(area, x, rows.y + 2), Some(1));
        assert_eq!(sheet.source_row_at(area, x, rows.y + 3), Some(2));
        assert_eq!(sheet.source_row_at(area, x, rows.y + 4), Some(3));
        assert_eq!(sheet.source_row_at(area, x, rows.y + 5), None, "gap");
        assert_eq!(
            sheet.source_row_at(area, x, rows.y + 6),
            None,
            "user samples header"
        );
        assert_eq!(sheet.source_row_at(area, x, rows.y + 7), Some(4));
        assert_eq!(sheet.source_row_at(area, x, rows.y + 8), Some(5));
        assert_eq!(sheet.source_row_at(area, x, rows.y + 9), None, "gap");
        assert_eq!(
            sheet.source_row_at(area, x, rows.y + 10),
            None,
            "default samples header"
        );
        assert_eq!(sheet.source_row_at(area, x, rows.y + 11), Some(6));
        assert_eq!(sheet.source_row_at(area, x, rows.y + 13), Some(8));
        assert_eq!(sheet.source_row_at(area, x, rows.y + 14), None, "footer");
        assert_eq!(
            sheet.source_row_at(area, rows.x.saturating_sub(1), rows.y),
            None,
            "beside the list"
        );

        // No imports: empty-state line sits in the user box.
        let none_imported = SettingsSheet {
            source_count: 0,
            ..sheet
        };
        assert_eq!(none_imported.source_row_at(area, x, rows.y), None);
        assert_eq!(none_imported.source_row_at(area, x, rows.y + 1), Some(0));
        assert_eq!(none_imported.source_row_at(area, x, rows.y + 2), Some(1));
        assert_eq!(none_imported.source_row_at(area, x, rows.y + 3), Some(2));
        assert_eq!(none_imported.source_row_at(area, x, rows.y + 4), Some(3));
        assert_eq!(
            none_imported.source_row_at(area, x, rows.y + 5),
            None,
            "gap"
        );
        assert_eq!(
            none_imported.source_row_at(area, x, rows.y + 6),
            None,
            "user samples header"
        );
        assert_eq!(
            none_imported.source_row_at(area, x, rows.y + 7),
            None,
            "no imports yet"
        );
        assert_eq!(
            none_imported.source_row_at(area, x, rows.y + 8),
            None,
            "gap"
        );
        assert_eq!(
            none_imported.source_row_at(area, x, rows.y + 9),
            None,
            "default samples header"
        );
        assert_eq!(none_imported.source_row_at(area, x, rows.y + 10), Some(4));

        // Another page answers nothing, and so does an empty list.
        let advanced = SettingsSheet {
            page: SettingsPage::Advanced,
            ..sheet
        };
        assert_eq!(advanced.source_row_at(area, x, rows.y), None);
        let empty = SettingsSheet {
            source_count: 0,
            default_count: 0,
            ..sheet
        };
        assert_eq!(empty.source_row_at(area, x, rows.y), None, "cache header");
        assert_eq!(empty.source_row_at(area, x, rows.y + 1), Some(0));
        assert_eq!(empty.source_row_at(area, x, rows.y + 2), Some(1));
        assert_eq!(empty.source_row_at(area, x, rows.y + 3), Some(2));
        assert_eq!(empty.source_row_at(area, x, rows.y + 4), Some(3));

        // Scrolled: a list taller than the sheet is found where it is
        // drawn - through the same settled window the page renders, so a
        // click lands on the row the pointer is over and the cursor is
        // among the lines shown, wherever the margin put it.
        let short = Rect::new(0, 0, 120, 12);
        let mut tall = SettingsSheet {
            source_count: 0,
            default_count: 40,
            selected: 42,
            ..sheet
        };
        tall.settle_scroll(short);
        let (_, rows) = tall.geometry_for(short).expect("a sheet fits");
        let last = rows.bottom() - 1;
        let window = tall.first_sources(usize::from(rows.height).max(1), 0, 40);
        let lines = source_lines(0, 40);
        let expected = match lines.get(window + usize::from(last - rows.y)) {
            Some(SourceLine::Row(index)) | Some(SourceLine::Control(index)) => Some(*index),
            _ => None,
        };
        assert_eq!(
            tall.source_row_at(short, rows.x + 3, last),
            expected,
            "the bottom line answers with the row the settled window draws there"
        );
        assert!(
            (rows.y..rows.bottom())
                .filter_map(|y| tall.source_row_at(short, rows.x + 3, y))
                .any(|index| index == 42),
            "the selected pack is among the lines shown"
        );
    }

    /// A pack's row counts its files down, names the one in hand, and
    /// reads as on disk once they are in.
    #[test]
    fn a_shipped_packs_row_follows_its_files() {
        let mut progress = SourceCacheProgress {
            total: 200,
            left: 200,
            loading: None,
        };
        assert_eq!(progress.label(), "░░░░░░░░ 0% · 0/200");
        progress.left = 50;
        progress.loading = Some("snare.wav".to_owned());
        assert_eq!(progress.label(), "██████░░ 75% · 150/200 · snare.wav");
        progress.left = 0;
        assert!(!progress.done(), "a file still in hand is not done");
        progress.loading = None;
        assert!(progress.done());
        assert_eq!(progress.label(), "200 file(s) on disk");
    }

    /// The pre-cache row counts its files down, names the one in hand,
    /// and reads "cached" once they are in.
    #[test]
    fn the_precache_row_follows_the_files() {
        let mut progress = PrecacheProgress {
            kind: PrecacheKind::Imports,
            total: 418,
            left: 418,
            loading: None,
        };
        assert_eq!(progress.value(), "● on · 0/418");
        assert_eq!(
            progress.explain().as_deref(),
            Some("░░░░░░░░░░ 0% · 418 to go")
        );
        progress.left = 247;
        progress.loading = Some("piano_3.wav".to_owned());
        assert_eq!(progress.value(), "● on · 171/418");
        assert_eq!(
            progress.explain().as_deref(),
            Some("████░░░░░░ 40% · syncing piano_3.wav · 247 to go")
        );
        progress.left = 0;
        assert!(!progress.done(), "a file still in hand is not done");
        progress.loading = None;
        assert!(progress.done());
        assert_eq!(progress.value(), "● on · 418 fetched");
        assert_eq!(progress.explain(), None, "the row's own line is back");
    }

    /// A sheet with more rows than it can draw says how many are hidden.
    ///
    /// It scrolled silently: the last row drawn looked exactly like the
    /// last row there was, so a page taller than the terminal hid its end
    /// and nothing on screen said so.
    #[test]
    fn a_scrolled_sheet_says_how_much_is_off_the_screen() {
        let settings = UiSettings::default();
        let features = TerminalFeatures::default();
        let text_at = |height: u16, selected: usize| {
            let area = Rect::new(0, 0, 120, height);
            render_sheet(
                SettingsSheet {
                    page: SettingsPage::Advanced,
                    selected,
                    ..SettingsSheet::default()
                },
                &settings,
                &features,
                area,
            )
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
        };

        // The hint itself says `↑/↓ select`, so the count is read from the
        // words around "more" rather than from an arrow anywhere.
        let counted = |text: &str| -> String {
            // The LAST "more" on the sheet: a help line further up says
            // "gives low frequencies more travel".
            let at = text.rfind("more").expect("a count of hidden rows");
            text[..at]
                .chars()
                .rev()
                .take(14)
                .collect::<String>()
                .chars()
                .rev()
                .collect()
        };

        // Short: the top of a long page, with the rest below it.
        let cramped = text_at(20, 0);
        let tail = counted(&cramped);
        assert!(tail.contains('↓'), "nothing said the page went on: {tail}");
        assert!(
            !tail.contains('↑'),
            "nothing is above the first row: {tail}"
        );

        // At the end of it, the other way round.
        let bottom = text_at(20, ADVANCED_ROWS.len() - 1);
        let tail = counted(&bottom);
        assert!(tail.contains('↑'), "{tail}");

        // More room hides less. The sheet caps its own height, so a taller
        // terminal does not always fit everything - what must hold is that
        // the count follows the room.
        let hidden = |text: &str| -> usize {
            counted(text)
                .split_whitespace()
                .filter_map(|word| word.parse::<usize>().ok())
                .next_back()
                .expect("a number of hidden rows")
        };
        let taller = text_at(40, 0);
        if counted(&taller)
            .chars()
            .any(|glyph| glyph == '↓' || glyph == '↑')
        {
            assert!(
                hidden(&taller) < hidden(&cramped),
                "a taller sheet still hid as much"
            );
        }

        // And a page short enough to fit says nothing at all about
        // scrolling: no counting, no clutter.
        let short_page = {
            let area = Rect::new(0, 0, 120, 40);
            render_sheet(
                SettingsSheet {
                    page: SettingsPage::Mapping,
                    ..SettingsSheet::default()
                },
                &settings,
                &features,
                area,
            )
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
        };
        assert!(!short_page.contains("more"), "{short_page}");
    }

    /// The frame rate is a ladder of four, sixty is the default, and
    /// nothing on it speeds the screen past sixty.
    ///
    /// While a set plays the screen repaints at this rate whether or not
    /// anything on it is a visualizer, and measured on one machine that
    /// drawing took more of a core than thirty lanes of sounding DSP did.
    /// It is the one control that hands those cycles back.
    #[test]
    fn the_frame_rate_ladder_runs_both_ways_from_the_loops_own_sixty() {
        use std::time::Duration;

        assert_eq!(UiSettings::default().frame_rate, FrameRate::Smooth);
        assert_eq!(
            FrameRate::Smooth.interval(),
            Duration::from_micros(16_667),
            "the default is the rate the loop was fixed at"
        );

        // Every step forwards is slower than the one before it, from the
        // top of the ladder to the bottom, and round it comes back.
        let mut rate = FrameRate::Quadruple;
        let mut seen = vec![rate];
        for _ in 1..FrameRate::LADDER.len() {
            let next = rate.step(true);
            assert!(
                next.interval() > rate.interval(),
                "{} is not slower than {}",
                next.label(),
                rate.label()
            );
            rate = next;
            seen.push(rate);
        }
        assert_eq!(rate, FrameRate::Thrift, "the bottom of the ladder");
        assert_eq!(rate.step(true), FrameRate::Quadruple, "round the far end");
        assert_eq!(
            FrameRate::Quadruple.step(false),
            FrameRate::Thrift,
            "and backwards from the top is the slowest"
        );
        assert_eq!(
            FrameRate::Thrift.interval(),
            Duration::from_micros(125_000),
            "eight a second"
        );

        // The two rungs above the default really are faster: the loop's
        // floor was taken off when they were added, and a ladder whose
        // fast end is silently clamped to 60 is a lie told in a menu.
        assert!(
            FrameRate::Double.interval() < FrameRate::Smooth.interval(),
            "120 must outrun 60"
        );
        assert!(
            FrameRate::Quadruple.interval() < FrameRate::Double.interval(),
            "240 must outrun 120"
        );

        // Every rung survives the preferences file, and a line nobody can
        // read is the default rather than a guess at a number.
        assert_eq!(seen.len(), FrameRate::LADDER.len(), "every rung walked");
        for rate in seen {
            assert_eq!(FrameRate::parse(rate.key()), rate, "{}", rate.label());
        }
        assert_eq!(FrameRate::parse("90"), FrameRate::Smooth);
        assert_eq!(FrameRate::parse(""), FrameRate::Smooth);
    }

    /// The master limiter is off by default, and one switch turns it on.
    /// It adds five milliseconds of lookahead, and the output clips at full
    /// scale with or without it, so it must be a choice.
    #[test]
    fn the_master_limiter_is_off_until_it_is_asked_for() {
        assert_eq!(
            UiSettings::default().master_limiter(),
            None,
            "the studio opens with the output undelayed"
        );

        // The mode row is a ring of characters and nothing else: off is
        // the switch's answer, and a mode row that could also turn the
        // limiter off would be two controls for one state.
        let mut character = rustel_audio::LimiterCharacter::default();
        for _ in 0..rustel_audio::LimiterCharacter::ALL.len() {
            character = step_limiter_character(character, true);
        }
        assert_eq!(
            character,
            rustel_audio::LimiterCharacter::default(),
            "round the ladder and home"
        );
        assert_eq!(
            step_limiter_character(rustel_audio::LimiterCharacter::default(), false),
            rustel_audio::LimiterCharacter::ALL
                .last()
                .copied()
                .expect("a ladder with stops on it"),
            "backwards from the first lands on the last"
        );

        // The ceiling and the mode are settings of their own, and they
        // survive being switched off. A player who set -6 warm and then
        // turned the limiter off to hear something gets -6 warm back, not
        // the default.
        let mut settings = UiSettings::default();
        for _ in 0..10 {
            settings.master_limiter_ceiling_db =
                step_limiter_ceiling(settings.master_limiter_ceiling_db, false);
        }
        assert!((settings.master_limiter_ceiling_db + 6.0).abs() < 1e-6);
        settings.master_limiter_character = rustel_audio::LimiterCharacter::Warm;
        settings.master_limiter_on = true;
        let asked = rustel_audio::LimiterSettings {
            threshold_db: -6.0,
            character: rustel_audio::LimiterCharacter::Warm,
        };
        assert_eq!(settings.master_limiter(), Some(asked));
        settings.master_limiter_on = false;
        assert_eq!(settings.master_limiter(), None, "the switch is the switch");
        assert_eq!(
            settings.limiter_default(),
            asked,
            "and what a set added from here starts with is still there"
        );
        settings.master_limiter_on = true;
        assert_eq!(
            settings.master_limiter(),
            Some(asked),
            "off and on again kept the ceiling and the mode"
        );

        // And it stops at both ends rather than wrapping: a press that
        // took the floor round to full scale would be a hand slipping off
        // the range onto no protection at all.
        let mut ceiling = 0.0;
        for _ in 0..200 {
            ceiling = step_limiter_ceiling(ceiling, true);
        }
        assert!((ceiling - 0.0).abs() < 1e-6, "{ceiling}");
        for _ in 0..200 {
            ceiling = step_limiter_ceiling(ceiling, false);
        }
        assert!(
            (ceiling - MASTER_LIMITER_FLOOR_DB).abs() < 1e-6,
            "{ceiling}"
        );
    }

    /// The fade steps round its ladder, keeps its name in the preferences,
    /// and reads back; a name it does not know is the theme's own.
    #[test]
    fn the_highlight_fade_steps_and_keeps_its_name() {
        let mut fade = HighlightFade::default();
        assert_eq!(fade, HighlightFade::Theme);
        assert_eq!(fade.seconds(), None);
        fade = fade.step(false);
        // The far end of the ladder, which grew a 2 s and a 3 s rung: a
        // held mark is how a dense line is read back, and 300 ms is gone
        // before the eye reaches it.
        assert_eq!(fade, HighlightFade::Slowest, "round the far end");
        assert_eq!(fade.seconds(), Some(3.0));
        for expected in HighlightFade::LADDER {
            fade = fade.step(true);
            assert_eq!(fade, expected);
            assert_eq!(HighlightFade::parse(fade.key()), fade);
        }
        assert_eq!(HighlightFade::parse("whenever"), HighlightFade::Theme);
        let settings = UiSettings {
            highlight_fade: HighlightFade::Short,
            ..UiSettings::default()
        };
        settings.apply();
        assert_eq!(highlight_fade(), Some(0.15));
        UiSettings::default().apply();
        assert_eq!(highlight_fade(), None);
    }

    /// The detail level steps its ladder, keeps its name in the
    /// preferences, and reads back; a name it does not know is the
    /// default. Each level shows a strict superset of what the level
    /// below it shows.
    #[test]
    fn the_metric_detail_steps_and_keeps_its_name() {
        let mut detail = MetricDetail::default();
        assert_eq!(
            detail,
            MetricDetail::Advanced,
            "the machine shows by default"
        );
        detail = detail.step(false);
        assert_eq!(detail, MetricDetail::Basic);
        detail = detail.step(false);
        assert_eq!(detail, MetricDetail::None);
        detail = detail.step(false);
        assert_eq!(detail, MetricDetail::Full, "wraps at the near end");
        for expected in MetricDetail::LADDER {
            detail = detail.step(true);
            assert_eq!(detail, expected);
            assert_eq!(MetricDetail::parse(detail.key()), detail);
        }
        assert_eq!(
            MetricDetail::parse("whenever"),
            MetricDetail::default(),
            "a name nobody wrote is the default, whatever the default is"
        );

        assert!(!MetricDetail::None.shows_process());
        assert!(!MetricDetail::None.shows_machine());
        assert!(!MetricDetail::None.shows_pressure());
        assert!(MetricDetail::Basic.shows_process());
        assert!(!MetricDetail::Basic.shows_machine());
        assert!(MetricDetail::Advanced.shows_process());
        assert!(MetricDetail::Advanced.shows_machine());
        assert!(!MetricDetail::Advanced.shows_pressure());
        assert!(MetricDetail::Full.shows_process());
        assert!(MetricDetail::Full.shows_machine());
        assert!(MetricDetail::Full.shows_pressure());
    }

    #[cfg(feature = "hydra")]
    #[test]
    fn narrow_settings_wrap_camera_guidance_and_reserve_a_square_preview() {
        use rustel_runtime::hydra::{HydraWebcamPreview, HydraWebcamState, HydraWebcamStatus};

        let settings = UiSettings {
            hydra_webcam: true,
            ..UiSettings::default()
        };
        let features = TerminalFeatures::default();
        let theme = Theme::built_in_default();
        let selected = ROWS
            .iter()
            .position(|row| *row == Row::HydraWebcam)
            .expect("webcam row");
        let area = Rect::new(0, 0, 60, ROWS.len() as u16 + 10);

        let render = |status| {
            let mut buffer = Buffer::empty(area);
            SettingsSheetView {
                sheet: SettingsSheet {
                    selected,
                    webcam_preview: true,
                    ..SettingsSheet::default()
                },
                settings: &settings,
                features: &features,
                tier: Tier::Cells,
                registry: rustel_runtime::capability_registry(),
                build_features: &[],
                device: None,
                pressure: None,
                max_polyphony_override: None,
                hydra_webcam: Some(status),
                prebakes: [PrebakeRow::default(); 2],
                sets_folder: String::new(),
                recordings_folder: String::new(),
                set_limiter: None,
                sample_cache: String::new(),
                precache: None,
                sources: Vec::new(),
                mappings: Default::default(),
                bindings: Vec::new(),
                capabilities: KeyboardCapabilities::legacy(),
                theme: &theme,
            }
            .render(area, &mut buffer);
            buffer
        };

        let buffer = render(HydraWebcamStatus {
                state: HydraWebcamState::Error,
                requested: true,
                detail: "Camera could not open. Open System Settings > Privacy > Camera, allow access, then retry."
                    .into(),
                preview: None,
            });
        let text = buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        for cue in ["System", "Privacy", "Camera", "retry"] {
            assert!(
                text.contains(cue),
                "missing {cue:?} from wrapped guidance: {text}"
            );
        }
        assert!(
            text.contains("┌──────────┐"),
            "the 60-column panel has a 12×6 placeholder"
        );

        let mut rgb = Vec::new();
        for y in 0..8 {
            for _ in 0..8 {
                rgb.extend_from_slice(if y < 4 { &[255, 0, 0] } else { &[0, 0, 255] });
            }
        }
        let live = render(HydraWebcamStatus {
            state: HydraWebcamState::Ready,
            requested: true,
            detail: "Camera is ready and feeding Hydra.".into(),
            preview: Some(HydraWebcamPreview {
                width: 8,
                height: 8,
                rgb,
            }),
        });
        assert!(
            live.content().iter().any(|cell| cell.symbol() == "▀"),
            "a ready frame is painted as an enlarged half-block preview"
        );
    }

    #[cfg(feature = "hydra")]
    fn render_webcam_fixture(
        sheet: SettingsSheet,
        settings: &UiSettings,
        area: Rect,
        tier: Tier,
    ) -> Buffer {
        let rgb: Vec<_> = (0..64_u8)
            .flat_map(|y| {
                (0..64_u8).flat_map(move |x| {
                    let face = (i32::from(x) - 32).pow(2) + (i32::from(y) - 30).pow(2) < 22 * 22;
                    if face {
                        [220, 170_u8.saturating_add(y), 120]
                    } else {
                        [x * 3, y * 2, 80]
                    }
                })
            })
            .collect();
        let mut buffer = Buffer::empty(area);
        SettingsSheetView {
            sheet,
            settings,
            features: &TerminalFeatures::default(),
            tier,
            registry: rustel_runtime::capability_registry(),
            build_features: &[],
            device: None,
            pressure: None,
            max_polyphony_override: None,
            hydra_webcam: Some(rustel_runtime::hydra::HydraWebcamStatus {
                state: rustel_runtime::hydra::HydraWebcamState::Ready,
                requested: true,
                detail: "Camera is ready and feeding Hydra.".into(),
                preview: Some(rustel_runtime::hydra::HydraWebcamPreview {
                    width: 64,
                    height: 64,
                    rgb,
                }),
            }),
            prebakes: [PrebakeRow::default(); 2],
            sets_folder: String::new(),
            recordings_folder: String::new(),
            set_limiter: None,
            capabilities: KeyboardCapabilities::legacy(),
            sample_cache: String::new(),
            precache: None,
            sources: Vec::new(),
            mappings: Default::default(),
            bindings: Vec::new(),
            theme: &Theme::built_in_default(),
        }
        .render(area, &mut buffer);
        buffer
    }

    #[cfg(feature = "hydra")]
    #[test]
    fn webcam_preview_scales_with_the_panel_and_its_controls_remain_clickable() {
        for (width, height, image_width, image_height) in
            [(80, 24, 16, 8), (120, 35, 24, 12), (60, 14, 8, 4)]
        {
            let area = Rect::new(0, 0, width, height);
            for selected in ROWS
                .iter()
                .enumerate()
                .filter_map(|(index, row)| matches!(row, Row::HydraWebcam).then_some(index))
            {
                let sheet = SettingsSheet {
                    selected,
                    webcam_preview: true,
                    ..Default::default()
                };
                let (panel, rows) = sheet.geometry_for(area).unwrap();
                assert!(rows.height >= SettingsSheetView::MIN_ROWS_SHOWN);
                assert_eq!(panel.bottom() - 2 - (rows.bottom() + 1), image_height);
                // The switch is already on: this fixture is about the
                // geometry and clickability of an established live
                // thumbnail, not the unconfirmed preview's own footer.
                let settings = UiSettings {
                    hydra_webcam: true,
                    ..UiSettings::default()
                };
                let buffer = render_webcam_fixture(sheet, &settings, area, Tier::Cells);
                assert_eq!(
                    buffer
                        .content
                        .iter()
                        .filter(|cell| cell.symbol() == "▀")
                        .count(),
                    usize::from(image_width * image_height)
                );
                let selected_y = (rows.y..rows.bottom())
                    .find(|&y| sheet.row_at_for(area, rows.x + 2, y) == Some(selected))
                    .unwrap();
                assert!(buffer[(rows.x + 1, selected_y)].symbol().contains('▸'));
                assert_eq!(
                    sheet.row_at_for(area, rows.x + 2, rows.bottom() + 1),
                    None,
                    "preview pixels never become controls"
                );
                let footer: String = (rows.x..rows.right())
                    .map(|x| buffer[(x, panel.bottom() - 2)].symbol())
                    .collect();
                assert!(footer.contains("Tab pages"));
            }
        }
        let normal = SettingsSheet::default();
        let area = Rect::new(0, 0, 80, 24);
        assert_eq!(
            normal.geometry_for(area),
            SettingsSheetView::geometry(area),
            "ordinary groups retain all their list rows"
        );
    }

    /// Walking a page taller than the sheet keeps rows in sight past the
    /// selection, so the next setting is on screen before it is chosen; a
    /// click selects without moving the page.
    #[test]
    fn walking_a_long_page_keeps_rows_in_sight_past_the_selection() {
        let short = Rect::new(0, 0, 120, SettingsSheetView::MIN_ROWS_SHOWN + 12);
        let mut settings = UiSettings::default();
        let features = TerminalFeatures::default();
        let mut sheet = SettingsSheet::default();
        let display = grouped_rows(ROWS);
        let (_, rows) = sheet.geometry_for(short).expect("a sheet");
        assert!(
            display.len() > usize::from(rows.height) + 4,
            "the page scrolls"
        );

        for step in 0..ROWS.len() - 1 {
            sheet.key(KeyCode::Down, &mut settings, &features);
            sheet.settle_scroll(short);
            let (_, rows) = sheet.geometry_for(short).expect("a sheet");
            let shown = usize::from(rows.height);
            let margin = crate::scroll::margin(shown);
            let line = display
                .iter()
                .position(|line| *line == DisplayRow::Control(sheet.selected))
                .expect("the selected control is drawn");
            let first = sheet.first_line(shown, &display);
            assert!(
                line >= first && line < first + shown,
                "step {step}: on screen"
            );
            // The next `margin` settings below are in sight, whatever titles
            // and blank lines lie between, as far as half the window reaches.
            let cap = shown.saturating_sub(1) / 2;
            let next: Vec<usize> = (line + 1..display.len())
                .filter(|at| matches!(display[*at], DisplayRow::Control(_)))
                .take(margin)
                .collect();
            for at in next.into_iter().filter(|at| at - line <= cap) {
                assert!(
                    at < first + shown,
                    "step {step}: the setting on line {at} is in sight below line {line} (first {first}, shown {shown})"
                );
            }
        }

        // Back to the top, then a click on the last row drawn: it is
        // selected, and the page stays where it is under the pointer.
        let mut sheet = SettingsSheet::default();
        for _ in 0..8 {
            sheet.key(KeyCode::Down, &mut settings, &features);
            sheet.settle_scroll(short);
        }
        let (_, rows) = sheet.geometry_for(short).expect("a sheet");
        let shown = usize::from(rows.height);
        let first = sheet.first_line(shown, &display);
        let clicked = display[first..first + shown]
            .iter()
            .rev()
            .find_map(|line| match line {
                DisplayRow::Control(index) => Some(*index),
                _ => None,
            })
            .expect("a control on screen");
        sheet.selected = clicked;
        sheet.hold_scroll = true;
        assert_eq!(sheet.first_line(shown, &display), first, "the click holds");
        sheet.key(KeyCode::Down, &mut settings, &features);
        sheet.settle_scroll(short);
        assert!(
            sheet.first_line(shown, &display) > first,
            "the next key brings the margin back"
        );
    }

    /// A click on a row of a group whose title is above the window leaves
    /// the page where it is - the title is not pulled into view under the
    /// pointer - and flipping the clicked switch by key does not scroll it
    /// either. Only a key that moves the selection brings the margin back.
    #[test]
    fn a_click_mid_group_holds_the_page_through_a_flip() {
        let short = Rect::new(0, 0, 120, SettingsSheetView::MIN_ROWS_SHOWN + 10);
        let mut settings = UiSettings::default();
        let features = TerminalFeatures::default();
        let display = grouped_rows(ROWS);
        let mut sheet = SettingsSheet::default();
        // Walk down until the window starts inside a group, below its title.
        let mut mid_group = None;
        for _ in 0..ROWS.len() - 1 {
            sheet.key(KeyCode::Down, &mut settings, &features);
            sheet.settle_scroll(short);
            let (_, rows) = sheet.geometry_for(short).expect("a sheet");
            let shown = usize::from(rows.height);
            let first = sheet.first_line(shown, &display);
            if let Some(DisplayRow::Control(top)) = display.get(first)
                && matches!(
                    display.get(first.saturating_sub(1)),
                    Some(DisplayRow::Control(_))
                )
            {
                mid_group = Some((first, shown, *top));
                break;
            }
        }
        let (first, shown, top) = mid_group.expect("a window that starts mid-group");
        sheet.selected = top;
        sheet.hold_scroll = true;
        sheet.settle_scroll(short);
        assert_eq!(
            sheet.first_line(shown, &display),
            first,
            "the clicked top row stays where it was clicked"
        );
        // Flip a switch on it: Space changes a setting, not the selection.
        sheet.key(KeyCode::Char(' '), &mut settings, &features);
        sheet.settle_scroll(short);
        assert!(sheet.hold_scroll, "a flip in place keeps the hold");
        assert_eq!(sheet.first_line(shown, &display), first);
        // Up moves the selection: the margin comes back.
        sheet.key(KeyCode::Up, &mut settings, &features);
        sheet.settle_scroll(short);
        assert!(!sheet.hold_scroll);
        assert!(
            sheet.first_line(shown, &display) < first,
            "the margin is kept above"
        );
    }

    /// A terminal too short for the whole list still draws the sheet, with
    /// the selected row among the ones it shows.
    #[test]
    fn a_short_terminal_scrolls_the_sheet_instead_of_blanking_it() {
        let short = Rect::new(0, 0, 120, SettingsSheetView::MIN_ROWS_SHOWN + 10);
        let (panel, rows) = SettingsSheetView::geometry(short).expect("a sheet that still draws");
        assert!(panel.height <= short.height.saturating_sub(2));
        assert!(rows.height >= SettingsSheetView::MIN_ROWS_SHOWN);
        assert!(usize::from(rows.height) < ROWS.len(), "nothing to scroll");

        let shown = usize::from(rows.height);
        // The last row is reachable…
        let last = SettingsSheet {
            selected: ROWS.len() - 1,
            ..SettingsSheet::default()
        };
        assert!(last.first_row(shown, ROWS.len()) + shown >= ROWS.len());
        // …and the first one still starts at the top.
        assert_eq!(SettingsSheet::default().first_row(shown, ROWS.len()), 0);

        // Too short for even that, and it says nothing rather than lying.
        assert!(SettingsSheetView::geometry(Rect::new(0, 0, 120, 6)).is_none());
        assert!(SettingsSheetView::geometry(Rect::new(0, 0, 40, 60)).is_none());
    }

    /// A port row steps off, then through each port, then off again, both
    /// ways round. The clock rows on the devices panel choose their port with
    /// exactly this, so it is pinned here rather than through a sheet that no
    /// longer has them.
    #[test]
    fn a_port_row_steps_off_then_each_port_then_off() {
        let ports = vec!["IAC Bus 1".to_owned(), "MK3".to_owned()];
        let mut current = None;
        current = step_port(&current, &ports, true);
        assert_eq!(current.as_deref(), Some("IAC Bus 1"));
        current = step_port(&current, &ports, true);
        assert_eq!(current.as_deref(), Some("MK3"));
        current = step_port(&current, &ports, true);
        assert_eq!(current, None, "round to off");
        current = step_port(&current, &ports, false);
        assert_eq!(current.as_deref(), Some("MK3"), "and backwards from off");
        // A port that is gone steps as if from off, rather than sticking.
        let vanished = Some("unplugged".to_owned());
        assert_eq!(
            step_port(&vanished, &ports, true).as_deref(),
            Some("IAC Bus 1")
        );
        // With nothing to offer, it stays off.
        assert_eq!(step_port(&None, &[], true), None);
    }

    #[test]
    fn the_sheet_walks_rows_and_flips_switches() {
        let mut settings = UiSettings::default();
        // A launch plays now until somebody asks it to wait: a studio
        // that holds the first sound back for a line reads as one that
        // did not hear the key.
        assert_eq!(settings.quantise, Quantise::Off);
        let mut sheet = SettingsSheet::default();
        assert_eq!(
            sheet.key(KeyCode::Right, &mut settings, &TerminalFeatures::default()),
            SettingsAction::Changed
        );
        assert_eq!(settings.quantise, Quantise::Beat, "the next line up");
        sheet.key(KeyCode::Left, &mut settings, &TerminalFeatures::default());
        sheet.key(KeyCode::Left, &mut settings, &TerminalFeatures::default());
        assert_eq!(
            settings.quantise,
            Quantise::Phrase(8),
            "back past off, round to the longest"
        );
        assert_eq!(Quantise::parse("8"), Quantise::Phrase(8));
        assert_eq!(Quantise::parse("1"), Quantise::Cycle);
        // Anything unreadable is the default, which is to play now.
        assert_eq!(Quantise::parse("every other tuesday"), Quantise::Off);
        assert_eq!(Quantise::Beat.unit_cycles(), Some(0.25));
        assert_eq!(Quantise::Off.unit_cycles(), None);
        // The playback group also contains slider smoothing; the next
        // group begins with the visualizers switch.
        sheet.selected = ROWS.iter().position(|row| *row == Row::Animation).unwrap();
        sheet.key(
            KeyCode::Char(' '),
            &mut settings,
            &TerminalFeatures::default(),
        );
        assert!(!settings.animation);
        // From the animation row, walk up past the first row and round.
        let from = sheet.selected;
        for _ in 0..=from {
            sheet.key(KeyCode::Up, &mut settings, &TerminalFeatures::default());
        }
        assert_eq!(sheet.selected, ROWS.len() - 1, "wraps from the top");
        assert_eq!(
            sheet.key(KeyCode::Esc, &mut settings, &TerminalFeatures::default()),
            SettingsAction::Close
        );
        settings.apply();
        assert!(!animation());
        assert!(highlights());
        UiSettings::default().apply();
        assert!(animation());
    }

    #[test]
    fn tab_switches_to_a_read_only_about_page() {
        let mut settings = UiSettings::default();
        let original = settings.clone();
        let mut sheet = SettingsSheet::default();

        assert_eq!(
            sheet.key(KeyCode::Tab, &mut settings, &TerminalFeatures::default()),
            SettingsAction::Nothing
        );
        assert_eq!(sheet.page, SettingsPage::Advanced);
        sheet.key(KeyCode::Tab, &mut settings, &TerminalFeatures::default());
        assert_eq!(sheet.page, SettingsPage::Mapping);
        sheet.key(KeyCode::Tab, &mut settings, &TerminalFeatures::default());
        assert_eq!(sheet.page, SettingsPage::Keybinds);
        sheet.key(KeyCode::Tab, &mut settings, &TerminalFeatures::default());
        assert_eq!(sheet.page, SettingsPage::Sources);
        sheet.key(KeyCode::Tab, &mut settings, &TerminalFeatures::default());
        assert_eq!(sheet.page, SettingsPage::Reference);
        sheet.key(KeyCode::Tab, &mut settings, &TerminalFeatures::default());
        assert_eq!(sheet.page, SettingsPage::About);
        // The About page reads; a key it has no use for is the score's.
        assert_eq!(
            sheet.key(KeyCode::Right, &mut settings, &TerminalFeatures::default()),
            SettingsAction::Ignored
        );
        assert_eq!(settings, original);
        sheet.key(
            KeyCode::BackTab,
            &mut settings,
            &TerminalFeatures::default(),
        );
        assert_eq!(sheet.page, SettingsPage::Reference);
        sheet.key(
            KeyCode::BackTab,
            &mut settings,
            &TerminalFeatures::default(),
        );
        assert_eq!(sheet.page, SettingsPage::Sources);
        sheet.key(
            KeyCode::BackTab,
            &mut settings,
            &TerminalFeatures::default(),
        );
        assert_eq!(sheet.page, SettingsPage::Keybinds);
        sheet.key(
            KeyCode::BackTab,
            &mut settings,
            &TerminalFeatures::default(),
        );
        assert_eq!(sheet.page, SettingsPage::Mapping);
        sheet.key(
            KeyCode::BackTab,
            &mut settings,
            &TerminalFeatures::default(),
        );
        assert_eq!(sheet.page, SettingsPage::Advanced);
        sheet.key(
            KeyCode::BackTab,
            &mut settings,
            &TerminalFeatures::default(),
        );
        assert_eq!(sheet.page, SettingsPage::Settings);
    }

    /// Non-settings tabs start fresh instead of inheriting a row index
    /// from a different page.
    #[test]
    fn a_non_settings_page_starts_on_its_first_row() {
        let mut sheet = SettingsSheet {
            selected: 7,
            ..SettingsSheet::default()
        };
        sheet.show_page(SettingsPage::Sources);
        assert_eq!(sheet.selected, 0);
        sheet.selected = 1;
        sheet.show_page(SettingsPage::Sources);
        assert_eq!(sheet.selected, 1, "the same page keeps its row");
    }

    /// The seven tabs each answer to their own strip of the title row.
    #[test]
    fn every_tab_has_its_own_click_strip() {
        let area = Rect::new(0, 0, 120, 40);
        let (panel, _) = SettingsSheetView::geometry(area).expect("fits");
        let at = |dx: u16| SettingsSheet::tab_at(area, panel.x + dx, panel.y);
        assert_eq!(at(2), Some(SettingsPage::Settings));
        assert_eq!(at(13), Some(SettingsPage::Advanced));
        assert_eq!(at(22), Some(SettingsPage::Advanced));
        assert_eq!(at(24), Some(SettingsPage::Mapping));
        assert_eq!(at(32), Some(SettingsPage::Mapping));
        assert_eq!(at(34), Some(SettingsPage::Keybinds));
        assert_eq!(at(43), Some(SettingsPage::Keybinds));
        assert_eq!(at(45), Some(SettingsPage::Sources));
        assert_eq!(at(53), Some(SettingsPage::Sources));
        assert_eq!(at(56), Some(SettingsPage::Reference));
        assert_eq!(at(66), Some(SettingsPage::Reference));
        assert_eq!(at(68), Some(SettingsPage::About));
        assert_eq!(at(74), Some(SettingsPage::About));
        assert_eq!(at(23), None, "the gap between tabs is nobody's");
        assert_eq!(at(33), None);
        assert_eq!(at(44), None);
        assert_eq!(at(54), None);
        assert_eq!(at(55), None);
        assert_eq!(at(67), None);
    }

    /// Every category starts listed. Each row hides one category.
    #[test]
    fn reference_page_switches_categories() {
        let mut settings = UiSettings::default();
        assert_eq!(settings.hidden_categories().count(), 0);
        let mut sheet = SettingsSheet::default();
        sheet.show_page(SettingsPage::Reference);
        let features = TerminalFeatures::default();
        for (index, category) in Category::ALL.into_iter().enumerate() {
            sheet.select(index);
            assert_eq!(
                sheet.key(KeyCode::Char(' '), &mut settings, &features),
                SettingsAction::Changed
            );
            assert!(!settings.shows_category(category), "{category:?}");
        }
        assert_eq!(
            settings.hidden_categories().collect::<Vec<_>>(),
            Category::ALL
        );
        sheet.key(KeyCode::Left, &mut settings, &features);
        assert!(settings.shows_category(Category::Internals));
    }

    #[test]
    fn advanced_page_steps_preview_budget_and_idle() {
        let mut settings = UiSettings::default();
        let mut sheet = SettingsSheet::default();
        let features = TerminalFeatures::default();
        sheet.show_page(SettingsPage::Advanced);
        sheet.selected = ADVANCED_ROWS
            .iter()
            .position(|row| *row == Row::PreviewBudget)
            .unwrap();
        assert_eq!(settings.preview_budget, PreviewBudget::Medium);
        assert_eq!(settings.preview_budget.bytes(), 64 * 1024 * 1024);
        assert_eq!(
            sheet.key(KeyCode::Right, &mut settings, &features),
            SettingsAction::Changed
        );
        assert_eq!(settings.preview_budget, PreviewBudget::Standard);
        sheet.selected = ADVANCED_ROWS
            .iter()
            .position(|row| *row == Row::UnusedSampleIdle)
            .unwrap();
        assert_eq!(
            sheet.key(KeyCode::Left, &mut settings, &features),
            SettingsAction::Changed
        );
        assert_eq!(settings.unused_sample_idle, UnusedSampleIdle::Quick);
    }

    #[test]
    fn about_renders_cached_capability_audio_and_safety_facts() {
        // Two rows taller than the sheet, which is what `geometry` asks for
        // before it will draw at all.
        let area = Rect::new(0, 0, 120, ROWS.len() as u16 + 10);
        let mut buffer = Buffer::empty(area);
        let settings = UiSettings::default();
        let features = TerminalFeatures::default();
        let theme = Theme::default();
        let device = StudioDeviceInfo {
            stream_id: 7,
            registry: std::sync::Arc::new(rustel_runtime::capability_registry_for_dispatch(
                rustel_audio::DspDispatch::portable(),
            )),
            audio: rustel_runtime::AudioStreamFacts::new(
                rustel_runtime::AudioOutputFacts::new(
                    rustel_runtime::AudioHost::cpal("alsa"),
                    "test output",
                    48_000,
                    2,
                    Some(rustel_runtime::AudioSampleFormat::F32),
                    128,
                    Some(256),
                ),
                None,
            ),
            allocator_tripwire_armed: true,
        };
        let mut pressure = EnginePressureSnapshot::default();
        pressure.device.callback_allocations = 3;
        let mut view = SettingsSheetView {
            prebakes: [PrebakeRow::default(); 2],
            sets_folder: String::new(),
            recordings_folder: String::new(),
            set_limiter: None,
            sample_cache: String::new(),
            precache: None,
            sources: Vec::new(),
            mappings: Default::default(),
            bindings: Vec::new(),
            sheet: SettingsSheet {
                page: SettingsPage::About,
                ..SettingsSheet::default()
            },
            settings: &settings,
            features: &features,
            tier: Tier::Cells,
            registry: rustel_runtime::capability_registry(),
            build_features: &["device-audio", "studio"],
            device: Some(&device),
            pressure: Some(&pressure),
            max_polyphony_override: None,
            capabilities: KeyboardCapabilities::legacy(),
            #[cfg(feature = "hydra")]
            hydra_webcam: None,
            theme: &theme,
        };

        // The live device overrides the configured Auto fallback. Stopping
        // removes only its stream facts and restores that fallback selection.
        assert_eq!(
            about_report(&view).runtime.acceleration_preference,
            "portable"
        );
        view.device = None;
        let fallback = about_report(&view);
        assert_eq!(fallback.runtime.acceleration_preference, "auto");
        assert!(fallback.audio.is_none());
        assert_eq!(fallback.build.features, ["device-audio", "studio"]);
        view.device = Some(&device);
        view.render(area, &mut buffer);
        let (_, rows) = SettingsSheetView::geometry(area).expect("about geometry");
        assert!(
            (rows.y..rows.bottom()).all(|y| (rows.x..rows.right()).all(|x| buffer
                .cell((x, y))
                .is_some_and(|cell| cell.bg == theme.overlay))),
            "About content must keep the opaque settings-sheet background"
        );
        let text = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("portable scalar dsp"));
        assert!(text.contains("policy portable"));
        assert!(
            text.contains("✓ portable scalar dsp"),
            "the cipher markers are spelled out"
        );
        assert!(text.contains("alsa · test output"));
        assert!(text.contains("requested 128 · host 256"));
        assert!(text.contains("alloc 3"));
        assert!(text.contains("features device-audio,studio"));
    }
    #[test]
    fn long_setting_labels_keep_the_same_value_column_when_selected() {
        let area = Rect::new(0, 0, 180, 60);
        let features = TerminalFeatures::default();
        let settings = UiSettings::default();
        let mut sheet = SettingsSheet::default();
        // The longest label lives on the advanced page now.
        sheet.show_page(SettingsPage::Advanced);
        let index = sheet
            .rows()
            .iter()
            .position(|row| *row == Row::SliderSmoothing)
            .unwrap();
        let (_, rows) = SettingsSheetView::geometry(area).unwrap();
        let line = |sheet: SettingsSheet| {
            let buffer = render_sheet(sheet, &settings, &features, area);
            let y = (rows.y..rows.bottom())
                .find(|&y| sheet.row_at_for(area, rows.x + 2, y) == Some(index))
                .unwrap();
            (rows.x + 3..rows.right())
                .map(|x| buffer.cell((x, y)).unwrap().symbol())
                .collect::<String>()
        };
        let unselected = line(sheet);
        sheet.selected = index;
        assert_eq!(
            unselected,
            line(sheet),
            "selection changes styling, never column positions or text"
        );
    }
    /// The mapping page: the grid walks with the arrows the way it reads,
    /// Enter learns the slot under the cursor, Space unbinds it, and the
    /// spellings a slot is saved by round-trip.
    #[test]
    fn the_mapping_slots_walk_learn_and_unbind() {
        use crossterm::event::KeyCode;
        let mut sheet = SettingsSheet::default();
        let mut settings = UiSettings::default();
        let features = TerminalFeatures::default();
        assert_eq!(
            settings.mappings, [None; MAPPING_SLOTS],
            "nothing bound out of the box"
        );
        // Tab reaches the page from the first one, and lands on slot 1.
        sheet.selected = 7;
        sheet.key(KeyCode::Tab, &mut settings, &features);
        sheet.key(KeyCode::Tab, &mut settings, &features);
        assert!(sheet.shows_mapping());
        assert_eq!(sheet.selected, 0);
        // Across, then down, then round: the grid reads the way it looks.
        sheet.key(KeyCode::Right, &mut settings, &features);
        assert_eq!(sheet.selected, 1);
        sheet.key(KeyCode::Down, &mut settings, &features);
        assert_eq!(sheet.selected, 1 + usize::from(MAPPING_COLUMNS));
        sheet.key(KeyCode::Up, &mut settings, &features);
        assert_eq!(sheet.selected, 1);
        sheet.key(KeyCode::Left, &mut settings, &features);
        sheet.key(KeyCode::Left, &mut settings, &features);
        assert_eq!(
            sheet.selected,
            MAPPING_SLOTS - 1,
            "left off the first wraps"
        );
        sheet.selected = 2;
        assert_eq!(
            sheet.key(KeyCode::Enter, &mut settings, &features),
            SettingsAction::LearnSlot(2)
        );
        assert_eq!(
            sheet.key(KeyCode::Char(' '), &mut settings, &features),
            SettingsAction::ClearSlot(2)
        );
        assert_eq!(
            sheet.key(KeyCode::Delete, &mut settings, &features),
            SettingsAction::ClearSlot(2)
        );
        // Esc and Tab stay the sheet's: a page cannot trap anyone.
        assert_eq!(
            sheet.key(KeyCode::Esc, &mut settings, &features),
            SettingsAction::Close
        );

        // A slot's spelling, both ways, for both kinds of control.
        let knob = MappingSource::Knob(SliderCc {
            controller: 74,
            channel: Some(2),
        });
        assert_eq!(knob.key(), "cc74/ch2");
        assert_eq!(knob.chip(), "cc74·2");
        assert_eq!(MappingSource::parse("cc74/ch2"), Some(knob));
        assert_eq!(
            MappingSource::parse("cc90"),
            Some(MappingSource::Knob(SliderCc {
                controller: 90,
                channel: None
            })),
            "a Launchkey's faders are cc90-93; a Maschine's are cc70-77 - \
             which is exactly why nothing is bound by default"
        );
        assert_eq!(
            MappingSource::parse("y2"),
            Some(MappingSource::Stick(StickAxis::Y2))
        );
        assert_eq!(MappingSource::parse("off"), None);
        assert_eq!(MappingSource::parse(""), None);
        assert_eq!(MappingSource::parse("nonsense"), None, "not silently x1");
        assert_eq!(StickAxis::parse("y2"), StickAxis::Y2);
        assert_eq!(StickAxis::Y2.index(), Some(3));
        assert_eq!(StickAxis::from_index(2), StickAxis::X2);
        assert!(StickAxis::Y1.is_vertical());
        let plain = SliderCc {
            controller: 74,
            channel: Some(2),
        };
        assert!(plain.matches(74, 2) && !plain.matches(74, 3));
        assert!(SliderCc::DEFAULT.matches(1, 9), "any channel");
        assert_eq!(plain.label(), "cc74 · ch2");
    }

    /// Every slot has a box, the boxes do not overlap, and the box a click
    /// lands on is the box the eye is on.
    #[test]
    fn every_mapping_slot_has_a_box_the_pointer_can_find() {
        let rows = Rect::new(2, 3, 66, 14);
        let grid = SlotGrid::new(rows).expect("room for the grid");
        let mut seen: Vec<Rect> = Vec::new();
        for slot in 0..MAPPING_SLOTS {
            let cell = grid.cell(slot).expect("a box for every slot");
            assert!(rows.union(cell) == rows, "slot {slot} escaped the page");
            for other in &seen {
                assert!(
                    other.intersection(cell).is_empty(),
                    "slot {slot} overlaps {other:?}"
                );
            }
            assert_eq!(grid.at(cell.x, cell.y), Some(slot));
            assert_eq!(grid.at(cell.right() - 1, cell.bottom() - 1), Some(slot));
            seen.push(cell);
        }
        assert_eq!(grid.at(0, 0), None, "off the page is off the grid");
        // Too narrow for a readable box is no grid at all, rather than a
        // grid of boxes with nothing legible in them.
        assert!(SlotGrid::new(Rect::new(0, 0, 20, 14)).is_none());
    }

    /// A slot box says what drives it, what it drives and where that fader
    /// stands; a slot waiting on a control says so; an unbound one says
    /// that, rather than pretending to a mapping it has not got.
    #[test]
    fn the_mapping_page_draws_what_each_slot_is_doing() {
        let mut sheet = SettingsSheet::default();
        sheet.show_page(SettingsPage::Mapping);
        let settings = UiSettings::default();
        let features = TerminalFeatures::default();
        let theme = Theme::built_in_default();
        let registry = rustel_runtime::capability_registry();
        let mut mappings: [SlotView; MAPPING_SLOTS] = Default::default();
        mappings[0] = SlotView {
            chip: "cc74·2".into(),
            takeover: "scaled",
            fader: "lpf".into(),
            notch: Some(0.5),
            live: true,
            learning: false,
        };
        mappings[1] = SlotView {
            learning: true,
            ..SlotView::default()
        };
        let area = Rect::new(0, 0, 100, 34);
        let mut buffer = Buffer::empty(area);
        SettingsSheetView {
            sheet,
            settings: &settings,
            features: &features,
            tier: Tier::Fine,
            registry,
            build_features: &[],
            device: None,
            pressure: None,
            max_polyphony_override: None,
            #[cfg(feature = "hydra")]
            hydra_webcam: None,
            prebakes: [PrebakeRow::default(), PrebakeRow::default()],
            sets_folder: String::new(),
            recordings_folder: String::new(),
            set_limiter: None,
            sample_cache: String::new(),
            precache: None,
            sources: Vec::new(),
            mappings,
            bindings: Vec::new(),
            capabilities: KeyboardCapabilities::legacy(),
            theme: &theme,
        }
        .render(area, &mut buffer);
        let text = (0..area.height)
            .map(|y| {
                (area.x..area.right())
                    .map(|x| buffer.cell((x, y)).unwrap().symbol().to_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains(" mapping "), "the page has a tab: {text}");
        assert!(text.contains("cc74·2 → lpf"), "{text}");
        assert!(text.contains("move a control…"), "{text}");
        assert!(text.contains("- not assigned"), "{text}");
        assert!(text.contains('█'), "the bound slot draws its fader: {text}");
        assert!(text.contains("Enter learns"), "{text}");
        // A bound slot says how it reads its control, without anyone
        // having to press something to find out.
        assert!(text.contains("1 \u{00b7} scaled"), "{text}");
        assert!(text.contains("t reads it another way"), "{text}");
    }

    /// The load mode sits on Advanced with the playback rows, waits by
    /// default, and either arrow flips it between wait and async.
    #[test]
    fn the_load_mode_row_flips_between_wait_and_async() {
        let mut settings = UiSettings::default();
        assert_eq!(settings.load_mode, LoadMode::Wait);
        assert!(ADVANCED_ROWS.contains(&Row::LoadMode));
        assert!(!ROWS.contains(&Row::LoadMode));
        assert_eq!(Row::LoadMode.group(), "Playback");
        let mut sheet = SettingsSheet {
            page: SettingsPage::Advanced,
            selected: ADVANCED_ROWS
                .iter()
                .position(|row| *row == Row::LoadMode)
                .expect("the row"),
            ..SettingsSheet::default()
        };
        let features = TerminalFeatures::default();
        for (key, expected) in [
            (KeyCode::Right, LoadMode::Async),
            (KeyCode::Right, LoadMode::Wait),
            (KeyCode::Left, LoadMode::Async),
        ] {
            assert_eq!(
                sheet.key(key, &mut settings, &features),
                SettingsAction::Changed
            );
            assert_eq!(settings.load_mode, expected);
        }
        assert_eq!(LoadMode::parse(LoadMode::Async.key()), LoadMode::Async);
        assert_eq!(LoadMode::parse("strudel-like"), LoadMode::Async);
    }
}
