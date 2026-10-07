//! Export one score from the studio: the sheet that asks how long, and the
//! thread that renders it.
//!
//! An export is always one score - the scene in the focused pane, as
//! written - because a bounce of "the set" would mean nothing. It is the
//! same offline render `rustel export` performs, in a session of its own on
//! a thread of its own, so the engine playing the room is never touched.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthStr;

use super::devices::draw_border;
use super::theme::Theme;
use rustel_runtime::{MP3_EXPORT, RenderFormat, Session, SessionConfig};

/// The ceiling a silence-ended export can run to.
const UNTIL_SILENCE_CEILING_SECS: f64 = 600.0;
/// How long the render waits for samples before bouncing without them.
const SAMPLE_WAIT: Duration = Duration::from_secs(60);

fn silence_hold_duration(hold_secs: f64) -> std::io::Result<Duration> {
    if !hold_secs.is_finite() || hold_secs < 0.1 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "silence hold must be a finite positive number",
        ));
    }
    // A hold longer than the render ceiling cannot finish by silence.
    Ok(Duration::from_secs_f64(
        hold_secs.min(UNTIL_SILENCE_CEILING_SECS),
    ))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Length {
    Cycles(f64),
    UntilSilence { floor_db: f64, hold_secs: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExportSettings {
    pub length: Length,
    pub mp3: bool,
    pub limiter_enabled: bool,
    /// Snapshot of the mixer, kept even when bypassed so the export can enable it.
    pub limiter: rustel_audio::RenderLimiter,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            length: Length::Cycles(16.0),
            mp3: false,
            limiter_enabled: false,
            limiter: rustel_audio::RenderLimiter {
                settings: rustel_audio::LimiterSettings {
                    threshold_db: rustel_audio::DEFAULT_THRESHOLD_DB,
                    character: rustel_audio::LimiterCharacter::default(),
                },
                makeup: false,
            },
        }
    }
}

impl ExportSettings {
    pub fn extension(&self) -> &'static str {
        if self.mp3 { "mp3" } else { "wav" }
    }

    pub fn describe_length(&self) -> String {
        match self.length {
            Length::Cycles(cycles) => format!("{} cycles", trim_number(cycles)),
            Length::UntilSilence {
                floor_db,
                hold_secs,
            } => format!(
                "until silence ({} dB for {} s)",
                trim_number(floor_db),
                trim_number(hold_secs)
            ),
        }
    }
}

fn trim_number(value: f64) -> String {
    let text = format!("{value:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// Which line of the sheet the keys go to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Field {
    Length,
    /// Cycles, or the floor in dB when ending on silence.
    First,
    /// The hold, only when ending on silence.
    Second,
    Format,
    Limiter,
    /// The `to` line: the path itself, typed.
    Target,
}

impl Field {
    fn next(self, silence: bool) -> Self {
        match (self, silence) {
            (Self::Length, _) => Self::First,
            (Self::First, true) => Self::Second,
            (Self::First, false) | (Self::Second, _) => Self::Format,
            (Self::Format, _) => Self::Limiter,
            (Self::Limiter, _) => Self::Target,
            (Self::Target, _) => Self::Length,
        }
    }

    fn previous(self, silence: bool) -> Self {
        match (self, silence) {
            (Self::Length, _) => Self::Target,
            (Self::First, _) => Self::Length,
            (Self::Second, _) => Self::First,
            (Self::Format, true) => Self::Second,
            (Self::Format, false) => Self::First,
            (Self::Limiter, _) => Self::Format,
            (Self::Target, _) => Self::Limiter,
        }
    }
}

/// Where each control of the sheet is drawn, so a click lands on what the
/// reader sees. The radio labels are as wide as their text, so these come
/// from the one computation the painter also draws from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SheetLayout {
    pub panel: Rect,
    pub cycles: Rect,
    pub until_silence: Rect,
    /// The cycles, or the floor when ending on silence.
    pub first: Rect,
    /// The hold; empty unless ending on silence.
    pub second: Rect,
    pub wav: Rect,
    pub mp3: Rect,
    pub limiter_off: Rect,
    pub limiter_on: Rect,
    pub target: Rect,
}

/// What a click on the sheet landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Length { silence: bool },
    Number(Field),
    Format { mp3: bool },
    Limiter { enabled: bool },
    Target,
}

/// The strings the sheet shows, built once for the painter and the layout.
struct Texts {
    cycles: String,
    silence: String,
    first: String,
    second: String,
}

fn radio(on: bool, text: &str) -> String {
    format!("{} {text}", if on { "●" } else { "○" })
}

/// The last `room` columns of `text`, with an ellipsis where the front
/// went: on a path the name is the part being typed.
fn tail(text: &str, room: usize) -> String {
    if UnicodeWidthStr::width(text) <= room {
        return text.to_owned();
    }
    let mut kept = String::new();
    let mut width = 0;
    for character in text.chars().rev() {
        let next = width + UnicodeWidthStr::width(character.encode_utf8(&mut [0; 4]) as &str);
        if next + 1 > room {
            break;
        }
        width = next;
        kept.insert(0, character);
    }
    format!("…{kept}")
}

/// The open sheet.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportSheet {
    pub scene_name: String,
    pub settings: ExportSettings,
    pub field: Field,
    /// Digits typed into the number under the caret, until they are taken.
    typed: String,
    pub target: PathBuf,
}

/// What a key did to the sheet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SheetAction {
    Nothing,
    Close,
    Render,
}

impl ExportSheet {
    pub fn open(
        scene_name: String,
        settings: ExportSettings,
        directory: &Path,
        unix_seconds: i64,
    ) -> Self {
        let mut sheet = Self {
            scene_name,
            settings,
            field: Field::Length,
            typed: String::new(),
            target: PathBuf::new(),
        };
        sheet.target = directory.join(sheet.filename(unix_seconds));
        sheet
    }

    fn filename(&self, unix_seconds: i64) -> String {
        let tape = rustel_runtime::session_log::default_session_filename(
            unix_seconds,
            Some(Path::new(&self.scene_name)),
        );
        let base = tape
            .strip_suffix(rustel_runtime::product::SESSION_FILE_SUFFIX)
            .unwrap_or(&tape);
        format!("{base}.{}", self.settings.extension())
    }

    fn silence(&self) -> bool {
        matches!(self.settings.length, Length::UntilSilence { .. })
    }

    /// The format's extension on the target - only where the extension is
    /// the sheet's own (or missing), so a typed `take.final` is left alone.
    fn retarget(&mut self) {
        let extension = self
            .target
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase);
        if matches!(extension.as_deref(), None | Some("wav" | "mp3")) {
            self.target.set_extension(self.settings.extension());
        }
    }

    fn texts(&self) -> Texts {
        let first = self.number_text(Field::First);
        let second = self.number_text(Field::Second);
        let (cycles, silence) = if self.silence() {
            (
                "cycles".to_owned(),
                format!("until silence ({first} dB for {second} s)"),
            )
        } else {
            (format!("{first} cycles"), "until silence".to_owned())
        };
        Texts {
            cycles,
            silence,
            first,
            second,
        }
    }

    /// A click: the caret moves there, and a radio is chosen outright - a
    /// label is unambiguous in a way a whole row is not.
    pub fn click(&mut self, hit: Hit) {
        self.commit_typed();
        match hit {
            Hit::Length { silence } => {
                self.field = Field::Length;
                if silence != self.silence() {
                    self.toggle_choice();
                }
            }
            Hit::Number(field) => self.field = field,
            Hit::Format { mp3 } => {
                self.field = Field::Format;
                let mp3 = mp3 && MP3_EXPORT;
                if self.settings.mp3 != mp3 {
                    self.settings.mp3 = mp3;
                    self.retarget();
                }
            }
            Hit::Target => self.field = Field::Target,
            Hit::Limiter { enabled } => {
                self.field = Field::Limiter;
                self.settings.limiter_enabled = enabled;
            }
        }
    }

    pub fn tab(&mut self, backwards: bool) {
        self.commit_typed();
        self.field = if backwards {
            self.field.previous(self.silence())
        } else {
            self.field.next(self.silence())
        };
    }

    /// ←/→: select the choice in that direction, or nudge a number.
    ///
    /// Choice arrows are deliberately idempotent so a held key reported as
    /// `Press, Repeat…` cannot flicker between modes on iTerm2 or another
    /// enhanced-keyboard terminal. Space retains the explicit toggle gesture.
    pub fn step(&mut self, direction: i8) {
        self.commit_typed();
        match self.field {
            Field::Length => {
                self.settings.length = if direction > 0 {
                    match self.settings.length {
                        Length::Cycles(_) => Length::UntilSilence {
                            floor_db: -60.0,
                            hold_secs: 2.0,
                        },
                        current @ Length::UntilSilence { .. } => current,
                    }
                } else {
                    match self.settings.length {
                        Length::UntilSilence { .. } => Length::Cycles(16.0),
                        current @ Length::Cycles(_) => current,
                    }
                };
            }
            Field::Format => {
                let mp3 = direction > 0 && MP3_EXPORT;
                if self.settings.mp3 != mp3 {
                    self.settings.mp3 = mp3;
                    self.retarget();
                }
            }
            Field::Limiter => self.settings.limiter_enabled = direction > 0,
            Field::First => match &mut self.settings.length {
                Length::Cycles(cycles) => {
                    *cycles = (*cycles
                        + f64::from(direction) * if *cycles >= 16.0 { 4.0 } else { 1.0 })
                    .max(1.0)
                }
                Length::UntilSilence { floor_db, .. } => {
                    *floor_db = (*floor_db + f64::from(direction) * 6.0).clamp(-120.0, 0.0)
                }
            },
            Field::Second => {
                if let Length::UntilSilence { hold_secs, .. } = &mut self.settings.length {
                    *hold_secs = (*hold_secs + f64::from(direction) * 0.5)
                        .clamp(0.1, UNTIL_SILENCE_CEILING_SECS);
                }
            }
            Field::Target => {}
        }
    }

    fn toggle_choice(&mut self) {
        self.commit_typed();
        match self.field {
            Field::Length => {
                self.settings.length = match self.settings.length {
                    Length::Cycles(_) => Length::UntilSilence {
                        floor_db: -60.0,
                        hold_secs: 2.0,
                    },
                    Length::UntilSilence { .. } => Length::Cycles(16.0),
                };
            }
            Field::Format => {
                self.settings.mp3 = !self.settings.mp3 && MP3_EXPORT;
                self.retarget();
            }
            Field::Limiter => self.settings.limiter_enabled = !self.settings.limiter_enabled,
            Field::First | Field::Second | Field::Target => {}
        }
    }

    /// Digits typed on a number line replace it; on the `to` line the keys
    /// edit the path itself.
    pub fn type_char(&mut self, character: char) {
        if self.field == Field::Target {
            if !character.is_control() {
                let mut path = self.target.to_string_lossy().into_owned();
                path.push(character);
                self.target = PathBuf::from(path);
            }
            return;
        }
        if !matches!(self.field, Field::First | Field::Second) {
            return;
        }
        if character.is_ascii_digit()
            || character == '.'
            || (character == '-' && self.typed.is_empty())
        {
            self.typed.push(character);
        }
    }

    pub fn backspace(&mut self) {
        if self.field == Field::Target {
            let mut path = self.target.to_string_lossy().into_owned();
            path.pop();
            self.target = PathBuf::from(path);
            return;
        }
        self.typed.pop();
    }

    fn commit_typed(&mut self) {
        if self.typed.is_empty() {
            return;
        }
        let typed = std::mem::take(&mut self.typed);
        let Ok(value) = typed.parse::<f64>() else {
            return;
        };
        if !value.is_finite() {
            return;
        }
        match (self.field, &mut self.settings.length) {
            (Field::First, Length::Cycles(cycles)) => *cycles = value.max(0.25),
            (Field::First, Length::UntilSilence { floor_db, .. }) => {
                *floor_db = value.clamp(-120.0, 0.0)
            }
            (Field::Second, Length::UntilSilence { hold_secs, .. }) => {
                *hold_secs = value.clamp(0.1, UNTIL_SILENCE_CEILING_SECS)
            }
            _ => {}
        }
    }

    pub fn key(&mut self, code: crossterm::event::KeyCode, shift: bool) -> SheetAction {
        use crossterm::event::KeyCode;
        match code {
            KeyCode::Esc => return SheetAction::Close,
            KeyCode::Enter => {
                self.commit_typed();
                return SheetAction::Render;
            }
            KeyCode::Tab => self.tab(shift),
            KeyCode::BackTab | KeyCode::Up => self.tab(true),
            KeyCode::Down => self.tab(false),
            KeyCode::Left => self.step(-1),
            KeyCode::Right => self.step(1),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Char(' ')
                if matches!(self.field, Field::Length | Field::Format | Field::Limiter) =>
            {
                self.toggle_choice()
            }
            KeyCode::Char(character) => self.type_char(character),
            _ => {}
        }
        SheetAction::Nothing
    }

    /// The settings as they stand, digits included. A typed path without
    /// an extension gets the format's here, so the file is what the sheet
    /// said it would be.
    pub fn settings(&mut self) -> ExportSettings {
        self.commit_typed();
        self.retarget();
        self.settings
    }

    fn number_text(&self, field: Field) -> String {
        if self.field == field && !self.typed.is_empty() {
            return self.typed.clone();
        }
        match (field, self.settings.length) {
            (Field::First, Length::Cycles(cycles)) => trim_number(cycles),
            (Field::First, Length::UntilSilence { floor_db, .. }) => trim_number(floor_db),
            (Field::Second, Length::UntilSilence { hold_secs, .. }) => trim_number(hold_secs),
            _ => String::new(),
        }
    }
}

/// Columns of the little scope beside a render's progress.
pub const SCOPE_COLUMNS: usize = 16;

/// How far a render has got, and what it sounds like, for the header.
struct Progress {
    frames_written: AtomicUsize,
    frames_total: AtomicUsize,
    sample_rate: AtomicUsize,
    /// Peak of each of the last few slices of rendered audio.
    peaks: Mutex<std::collections::VecDeque<f32>>,
}

/// One look at a render in progress.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportGlance {
    pub scene_name: String,
    /// Rendered so far, in seconds of audio.
    pub seconds: f64,
    /// Of the length asked for - `None` when the end is decided by silence.
    pub percent: Option<u8>,
    pub peaks: [f32; SCOPE_COLUMNS],
}

/// Snapshot the host default and any persisted score override for an isolated
/// export. Re-evaluating source text can then override this inherited value.
pub(super) fn session_config(
    default: &SessionConfig,
    host_default: usize,
    score_override: Option<usize>,
) -> SessionConfig {
    default
        .clone()
        .with_max_polyphony(score_override.unwrap_or(host_default))
}

/// The render, on its own thread.
pub struct ExportJob {
    pub scene_name: String,
    pub target: PathBuf,
    pub started: Instant,
    /// Ends by silence, so a percentage would be of a ceiling nobody chose.
    open_ended: bool,
    receiver: Receiver<Result<f64, String>>,
    progress: Arc<Progress>,
    finish: Arc<AtomicBool>,
    finished_early: bool,
}

impl ExportJob {
    /// Render `source` with `settings` into `target`. `config` is the
    /// studio's current session configuration, including the current host
    /// polyphony default. The source may override it with setMaxPolyphony.
    pub fn start(
        scene_name: String,
        source: String,
        mini: bool,
        settings: ExportSettings,
        config: SessionConfig,
        target: PathBuf,
    ) -> std::io::Result<Self> {
        if let Length::UntilSilence { hold_secs, .. } = settings.length {
            silence_hold_duration(hold_secs)?;
        }
        let (sender, receiver) = channel();
        let path = target.clone();
        let progress = Arc::new(Progress {
            frames_written: AtomicUsize::new(0),
            frames_total: AtomicUsize::new(0),
            sample_rate: AtomicUsize::new(config.sample_rate as usize),
            peaks: Mutex::new(std::collections::VecDeque::with_capacity(SCOPE_COLUMNS)),
        });
        let finish = Arc::new(AtomicBool::new(false));
        let worker_progress = Arc::clone(&progress);
        let worker_finish = Arc::clone(&finish);
        thread::Builder::new()
            .name("studio-export".into())
            // A bounce evaluates and queries exactly as the engine does, so it
            // needs the engine's stack. On the platform default the JavaScript
            // runtime's budget is larger than the thread it is measured
            // against, and a refusal it means to report becomes a crash.
            .stack_size(rustel_runtime::QUERY_WORKER_STACK_BYTES)
            .spawn(move || {
                let _ = sender.send(render(
                    &source,
                    mini,
                    settings,
                    config,
                    &path,
                    &worker_progress,
                    &worker_finish,
                ));
            })?;
        Ok(Self {
            scene_name,
            target,
            started: Instant::now(),
            open_ended: matches!(settings.length, Length::UntilSilence { .. }),
            receiver,
            progress,
            finish,
            finished_early: false,
        })
    }

    /// Seconds rendered, or why not. `None` while it is still going.
    pub fn poll(&self) -> Option<Result<f64, String>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                Some(Err("export worker stopped unexpectedly".into()))
            }
        }
    }

    /// End the render now: a tenth of a second of fade, then the file is
    /// closed with everything rendered so far.
    pub fn finish(&mut self) {
        self.finished_early = true;
        self.finish.store(true, Ordering::Relaxed);
    }

    pub fn finished_early(&self) -> bool {
        self.finished_early
    }

    pub fn glance(&self) -> ExportGlance {
        let written = self.progress.frames_written.load(Ordering::Relaxed);
        let total = self.progress.frames_total.load(Ordering::Relaxed);
        let rate = self.progress.sample_rate.load(Ordering::Relaxed).max(1);
        let mut peaks = [0.0f32; SCOPE_COLUMNS];
        if let Ok(recent) = self.progress.peaks.lock() {
            let start = SCOPE_COLUMNS.saturating_sub(recent.len());
            for (slot, peak) in peaks[start..].iter_mut().zip(recent.iter()) {
                *slot = *peak;
            }
        }
        ExportGlance {
            scene_name: self.scene_name.clone(),
            seconds: written as f64 / rate as f64,
            percent: (!self.open_ended && total > 0)
                .then(|| ((written as f64 / total as f64) * 100.0).round().min(100.0) as u8),
            peaks,
        }
    }
}

fn render(
    source: &str,
    mini: bool,
    settings: ExportSettings,
    config: SessionConfig,
    target: &Path,
    progress: &Progress,
    finish: &AtomicBool,
) -> Result<f64, String> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let mut session = Session::with_config(config).map_err(|error| error.to_string())?;
    session.set_export_limiter(settings.limiter_enabled.then_some(settings.limiter));
    // The render does not care what the CLI would print; a library that
    // cannot be reached bounces the synths and says so in the log.
    let _ = session.enable_default_samples();
    if mini {
        session.evaluate_mini(source)
    } else {
        session.evaluate(source)
    }
    .map_err(|error| error.to_string())?;
    let sounds = rustel_runtime::sounds::in_score(source);
    session.prefetch_sounds(&sounds);
    let _ = session.wait_for_sample_loads(SAMPLE_WAIT);
    let duration = match settings.length {
        Length::Cycles(cycles) => cycles / session.config().cps,
        Length::UntilSilence {
            floor_db,
            hold_secs,
        } => {
            let floor = 10f64.powf(floor_db / 20.0) as f32;
            let hold = silence_hold_duration(hold_secs).map_err(|error| error.to_string())?;
            session.stop_export_when_silent(floor, hold);
            UNTIL_SILENCE_CEILING_SECS
        }
    };
    let format = if settings.mp3 {
        RenderFormat::ScalarMp3
    } else {
        RenderFormat::ScalarWav
    };
    let rate = session.config().sample_rate;
    progress.sample_rate.store(rate as usize, Ordering::Relaxed);
    // A slice of the scope is a sixteenth of a second: the header redraws
    // no faster, and a peak per slice is all it shows.
    let slice_frames = (rate as usize / 16).max(1);
    let mut slice_peak = 0.0f32;
    let mut slice_seen = 0usize;
    let mut observer = |tick: rustel_audio::RenderTick<'_>| {
        progress
            .frames_written
            .store(tick.frames_written, Ordering::Relaxed);
        progress
            .frames_total
            .store(tick.frames_total, Ordering::Relaxed);
        slice_peak = tick
            .block
            .iter()
            .fold(slice_peak, |peak, sample| peak.max(sample.abs()));
        slice_seen += tick.block.len() / 2;
        if slice_seen >= slice_frames {
            if let Ok(mut peaks) = progress.peaks.try_lock() {
                if peaks.len() >= SCOPE_COLUMNS {
                    peaks.pop_front();
                }
                peaks.push_back(slice_peak);
            }
            slice_peak = 0.0;
            slice_seen = 0;
        }
    };
    let report = session
        .render_controlled(
            duration,
            target,
            format,
            false,
            Some(&mut observer),
            Some(finish),
        )
        .map_err(|error| error.to_string())?;
    // Written, but not the score: see `RenderReport::failure`.
    if let Some(message) = report.failure() {
        return Err(message);
    }
    Ok(report.duration_secs)
}

pub struct ExportSheetView<'a> {
    pub sheet: &'a ExportSheet,
    pub theme: &'a Theme,
}

impl ExportSheetView<'_> {
    pub fn geometry(available: Rect) -> Option<Rect> {
        let height = 8;
        if available.height < height + 2 || available.width < 44 {
            return None;
        }
        Some(Rect::new(
            available.x + 1,
            available.bottom().saturating_sub(height),
            available.width.saturating_sub(2),
            height,
        ))
    }

    /// Where everything goes, for drawing and for the pointer alike.
    pub fn layout(sheet: &ExportSheet, available: Rect) -> Option<SheetLayout> {
        let panel = Self::geometry(available)?;
        let x = panel.x + 2;
        let right = panel.right().saturating_sub(2);
        let texts = sheet.texts();
        let span = |x: u16, y: u16, text: &str| {
            let width = (UnicodeWidthStr::width(text) as u16).min(right.saturating_sub(x));
            Rect::new(x, y, width, 1)
        };
        let silence = sheet.silence();
        let y = panel.y + 1;
        let cycles = span(x + 9, y, &radio(!silence, &texts.cycles));
        let until_silence = span(cycles.right() + 3, y, &radio(silence, &texts.silence));
        let y = panel.y + 2;
        let (first, second) = if silence {
            (
                span(x + 9, y, &format!("{} dB", texts.first)),
                span(x + 29, y, &format!("{} s", texts.second)),
            )
        } else {
            (span(x + 9, y, &texts.first), Rect::default())
        };
        let y = panel.y + 3;
        let wav = span(x + 9, y, &radio(!sheet.settings.mp3, "wav"));
        let mp3 = if MP3_EXPORT {
            span(wav.right() + 3, y, &radio(sheet.settings.mp3, "mp3"))
        } else {
            Rect::default()
        };
        let y = panel.y + 4;
        let limiter_off = span(x + 9, y, &radio(!sheet.settings.limiter_enabled, "off"));
        let limiter_on = span(
            limiter_off.right() + 3,
            y,
            &radio(sheet.settings.limiter_enabled, "on"),
        );
        let target = Rect::new(x + 9, panel.y + 5, right.saturating_sub(x + 9), 1);
        Some(SheetLayout {
            panel,
            cycles,
            until_silence,
            first,
            second,
            wav,
            mp3,
            limiter_off,
            limiter_on,
            target,
        })
    }

    /// What is under a cell, if anything the sheet answers to.
    pub fn hit(sheet: &ExportSheet, available: Rect, x: u16, y: u16) -> Option<Hit> {
        let layout = Self::layout(sheet, available)?;
        let on = |rect: Rect| rect.width > 0 && y == rect.y && x >= rect.x && x < rect.right();
        if on(layout.cycles) {
            Some(Hit::Length { silence: false })
        } else if on(layout.until_silence) {
            Some(Hit::Length { silence: true })
        } else if on(layout.first) {
            Some(Hit::Number(Field::First))
        } else if on(layout.second) {
            Some(Hit::Number(Field::Second))
        } else if on(layout.wav) {
            Some(Hit::Format { mp3: false })
        } else if on(layout.mp3) {
            Some(Hit::Format { mp3: true })
        } else if on(layout.limiter_off) {
            Some(Hit::Limiter { enabled: false })
        } else if on(layout.limiter_on) {
            Some(Hit::Limiter { enabled: true })
        } else if y == layout.target.y && x >= layout.panel.x && x < layout.panel.right() {
            // The whole row: a short path is still the line to type on.
            Some(Hit::Target)
        } else {
            None
        }
    }
}

impl Widget for ExportSheetView<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let Some(layout) = Self::layout(self.sheet, area) else {
            return;
        };
        let panel = layout.panel;
        let theme = self.theme;
        let sheet = self.sheet;
        super::view::clear_overlay(
            buffer,
            panel,
            Style::default().bg(theme.overlay).fg(theme.foreground),
        );
        draw_border(buffer, panel, theme);
        let title = format!(" export - {} ", sheet.scene_name);
        buffer.set_stringn(
            panel.x + 2,
            panel.y,
            &title,
            usize::from(panel.width.saturating_sub(4)),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        );
        let x = panel.x + 2;
        let width = usize::from(panel.width.saturating_sub(4));
        let label = |buffer: &mut Buffer, y: u16, text: &str| {
            buffer.set_stringn(x, y, text, width, Style::default().fg(theme.muted));
        };
        let paint_radio = |buffer: &mut Buffer, rect: Rect, text: &str, on: bool, active: bool| {
            let mut style = Style::default().fg(if on { theme.foreground } else { theme.muted });
            if active {
                style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
            }
            buffer.set_stringn(
                rect.x,
                rect.y,
                radio(on, text),
                usize::from(rect.width),
                style,
            );
        };
        let value_style = |active: bool| {
            let style = Style::default().fg(theme.foreground);
            if active {
                style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                style
            }
        };
        let texts = sheet.texts();
        let silence = sheet.silence();
        // length
        label(buffer, panel.y + 1, "length ");
        let active = sheet.field == Field::Length;
        paint_radio(buffer, layout.cycles, &texts.cycles, !silence, active);
        paint_radio(
            buffer,
            layout.until_silence,
            &texts.silence,
            silence,
            active,
        );
        // number line(s)
        if silence {
            label(buffer, panel.y + 2, "floor  ");
            buffer.set_stringn(
                layout.first.x,
                layout.first.y,
                format!("{} dB", texts.first),
                usize::from(layout.first.width),
                value_style(sheet.field == Field::First),
            );
            buffer.set_stringn(
                x + 24,
                panel.y + 2,
                "hold ",
                5,
                Style::default().fg(theme.muted),
            );
            buffer.set_stringn(
                layout.second.x,
                layout.second.y,
                format!("{} s", texts.second),
                usize::from(layout.second.width),
                value_style(sheet.field == Field::Second),
            );
        } else {
            label(buffer, panel.y + 2, "cycles ");
            buffer.set_stringn(
                layout.first.x,
                layout.first.y,
                &texts.first,
                usize::from(layout.first.width),
                value_style(sheet.field == Field::First),
            );
        }
        // format
        label(buffer, panel.y + 3, "format ");
        let active = sheet.field == Field::Format;
        paint_radio(buffer, layout.wav, "wav", !sheet.settings.mp3, active);
        if MP3_EXPORT {
            paint_radio(buffer, layout.mp3, "mp3", sheet.settings.mp3, active);
        }
        // master limiter, with the ceiling and character inherited from the mixer
        label(buffer, panel.y + 4, "limiter");
        let active = sheet.field == Field::Limiter;
        paint_radio(
            buffer,
            layout.limiter_off,
            "off",
            !sheet.settings.limiter_enabled,
            active,
        );
        paint_radio(
            buffer,
            layout.limiter_on,
            "on",
            sheet.settings.limiter_enabled,
            active,
        );
        let details_x = layout.limiter_on.right() + 3;
        let limiter = sheet.settings.limiter;
        buffer.set_stringn(
            details_x,
            panel.y + 4,
            format!(
                "{} dB · {}{}",
                trim_number(f64::from(limiter.settings.threshold_db)),
                limiter.settings.character.key(),
                if limiter.makeup { " · makeup" } else { "" }
            ),
            usize::from(panel.right().saturating_sub(2).saturating_sub(details_x)),
            Style::default().fg(theme.muted),
        );
        // target: a text field, with the caret up while it is the one typed into
        label(buffer, panel.y + 5, "to     ");
        let editing = sheet.field == Field::Target;
        let mut path = sheet.target.display().to_string();
        if editing {
            path.push_str(super::terminal::symbol("▏"));
        }
        let room = usize::from(layout.target.width);
        buffer.set_stringn(
            layout.target.x,
            layout.target.y,
            tail(&path, room),
            room,
            value_style(editing),
        );
        buffer.set_stringn(
            x,
            panel.bottom().saturating_sub(2),
            "Tab or click moves · ←/→ or typing changes · Enter renders · Esc closes",
            width,
            Style::default().fg(theme.muted),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    #[test]
    fn the_sheet_moves_between_fields_and_takes_digits_and_arrows() {
        let mut sheet = ExportSheet::open(
            "drums".into(),
            ExportSettings::default(),
            Path::new("/sets/exports"),
            0,
        );
        assert_eq!(
            sheet.target,
            Path::new("/sets/exports/drums-1970-01-01T00-00-00.wav")
        );
        assert_eq!(sheet.field, Field::Length);
        sheet.key(KeyCode::Tab, false);
        assert_eq!(sheet.field, Field::First);
        sheet.key(KeyCode::Char('3'), false);
        sheet.key(KeyCode::Char('2'), false);
        sheet.key(KeyCode::Tab, false);
        assert_eq!(sheet.settings.length, Length::Cycles(32.0));
        assert_eq!(sheet.field, Field::Format, "no hold line for cycles");
        sheet.key(KeyCode::Right, false);
        assert_eq!(sheet.settings.mp3, MP3_EXPORT);
        let extension = if MP3_EXPORT { "mp3" } else { "wav" };
        assert_eq!(
            sheet.target,
            PathBuf::from(format!(
                "/sets/exports/drums-1970-01-01T00-00-00.{extension}"
            ))
        );
        sheet.key(KeyCode::Tab, false);
        assert_eq!(sheet.field, Field::Limiter);
        sheet.key(KeyCode::Tab, false);
        assert_eq!(sheet.field, Field::Target, "the to line is a field too");
        sheet.key(KeyCode::Tab, false);
        assert_eq!(sheet.field, Field::Length);
        sheet.key(KeyCode::Right, false);
        assert_eq!(
            sheet.settings.length,
            Length::UntilSilence {
                floor_db: -60.0,
                hold_secs: 2.0
            }
        );
        sheet.key(KeyCode::Tab, false);
        sheet.key(KeyCode::Left, false);
        sheet.key(KeyCode::Tab, false);
        assert_eq!(sheet.field, Field::Second);
        sheet.key(KeyCode::Right, false);
        assert_eq!(
            sheet.settings(),
            ExportSettings {
                length: Length::UntilSilence {
                    floor_db: -66.0,
                    hold_secs: 2.5
                },
                mp3: MP3_EXPORT,
                ..ExportSettings::default()
            }
        );
        assert_eq!(sheet.key(KeyCode::Enter, false), SheetAction::Render);
        assert_eq!(sheet.key(KeyCode::Esc, false), SheetAction::Close);
        assert_eq!(
            sheet.settings.describe_length(),
            "until silence (-66 dB for 2.5 s)"
        );
    }

    #[test]
    fn oversized_silence_hold_stays_within_the_render_ceiling() {
        let mut sheet = ExportSheet::open(
            "drums".into(),
            ExportSettings::default(),
            Path::new("/sets/exports"),
            0,
        );
        sheet.key(KeyCode::Right, false);
        sheet.key(KeyCode::Tab, false);
        sheet.key(KeyCode::Tab, false);
        assert_eq!(sheet.field, Field::Second);
        for digit in "10000000000000000000000000000000000000000".chars() {
            sheet.key(KeyCode::Char(digit), false);
        }
        sheet.key(KeyCode::Enter, false);
        assert_eq!(
            sheet.settings.length,
            Length::UntilSilence {
                floor_db: -60.0,
                hold_secs: UNTIL_SILENCE_CEILING_SECS,
            }
        );
        assert_eq!(
            silence_hold_duration(f64::MAX).unwrap(),
            Duration::from_secs(600)
        );
    }

    #[test]
    fn invalid_silence_hold_is_rejected_before_starting_a_worker() {
        for hold_secs in [f64::NAN, f64::INFINITY, -1.0] {
            let settings = ExportSettings {
                length: Length::UntilSilence {
                    floor_db: -60.0,
                    hold_secs,
                },
                ..ExportSettings::default()
            };
            let error = ExportJob::start(
                "drums".into(),
                "s('sine')".into(),
                false,
                settings,
                SessionConfig::default(),
                PathBuf::from("/tmp/unused-export.wav"),
            )
            .err()
            .expect("invalid hold rejected before worker launch");
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn an_export_worker_that_exits_without_a_result_reports_failure() {
        let (sender, receiver) = channel();
        drop(sender);
        let job = ExportJob {
            scene_name: "drums".into(),
            target: PathBuf::new(),
            started: Instant::now(),
            open_ended: false,
            receiver,
            progress: Arc::new(Progress {
                frames_written: AtomicUsize::new(0),
                frames_total: AtomicUsize::new(0),
                sample_rate: AtomicUsize::new(48_000),
                peaks: Mutex::new(std::collections::VecDeque::new()),
            }),
            finish: Arc::new(AtomicBool::new(false)),
            finished_early: false,
        };
        assert_eq!(
            job.poll(),
            Some(Err("export worker stopped unexpectedly".into()))
        );
    }

    #[test]
    fn held_arrows_set_binary_choices_without_repeat_flicker() {
        let mut sheet = ExportSheet::open(
            "drums".into(),
            ExportSettings::default(),
            Path::new("/sets/exports"),
            0,
        );

        sheet.key(KeyCode::Right, false);
        let silence = sheet.settings.length;
        sheet.key(KeyCode::Right, false);
        assert_eq!(
            sheet.settings.length, silence,
            "repeat keeps the right choice"
        );
        sheet.key(KeyCode::Left, false);
        let cycles = sheet.settings.length;
        sheet.key(KeyCode::Left, false);
        assert_eq!(
            sheet.settings.length, cycles,
            "repeat keeps the left choice"
        );

        sheet.field = Field::Format;
        sheet.key(KeyCode::Right, false);
        sheet.key(KeyCode::Right, false);
        assert_eq!(
            sheet.settings.mp3, MP3_EXPORT,
            "right consistently selects mp3 where the build encodes it"
        );
        sheet.key(KeyCode::Left, false);
        sheet.key(KeyCode::Left, false);
        assert!(!sheet.settings.mp3, "left consistently selects wav");

        sheet.key(KeyCode::Char(' '), false);
        assert_eq!(sheet.settings.mp3, MP3_EXPORT, "Space remains a toggle");

        sheet.field = Field::Limiter;
        sheet.key(KeyCode::Right, false);
        sheet.key(KeyCode::Right, false);
        assert!(sheet.settings.limiter_enabled);
        sheet.key(KeyCode::Left, false);
        sheet.key(KeyCode::Left, false);
        assert!(!sheet.settings.limiter_enabled);
        sheet.key(KeyCode::Char(' '), false);
        assert!(sheet.settings.limiter_enabled);
    }

    #[test]
    fn the_sheet_draws_its_lines() {
        let theme = Theme::default();
        let mut sheet = ExportSheet::open(
            "drums".into(),
            ExportSettings::default(),
            Path::new("exports"),
            0,
        );
        sheet.key(KeyCode::Tab, false);
        let area = Rect::new(0, 0, 100, 20);
        let mut buffer = Buffer::empty(area);
        ExportSheetView {
            sheet: &sheet,
            theme: &theme,
        }
        .render(area, &mut buffer);
        let text = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("export - drums"), "{text}");
        assert!(text.contains("● 16 cycles"), "{text}");
        assert!(text.contains("○ until silence"), "{text}");
        assert!(text.contains("● wav"), "{text}");
        assert_eq!(text.contains("mp3"), MP3_EXPORT, "{text}");
        assert!(text.contains("limiter  ● off   ○ on"), "{text}");
        assert!(text.contains("drums-1970-01-01T00-00-00.wav"), "{text}");
    }

    #[test]
    fn a_click_chooses_what_it_lands_on() {
        let area = Rect::new(0, 0, 100, 20);
        let mut sheet = ExportSheet::open(
            "drums".into(),
            ExportSettings::default(),
            Path::new("exports"),
            0,
        );
        let layout = ExportSheetView::layout(&sheet, area).expect("room");
        assert!(layout.until_silence.x > layout.cycles.right(), "{layout:?}");
        assert_eq!(layout.second, Rect::default(), "no hold on a cycles export");

        let hit =
            ExportSheetView::hit(&sheet, area, layout.until_silence.x, layout.until_silence.y);
        assert_eq!(hit, Some(Hit::Length { silence: true }));
        sheet.click(hit.expect("hit"));
        assert!(sheet.silence());
        assert_eq!(sheet.field, Field::Length);
        // Choosing what is already chosen changes nothing.
        sheet.click(Hit::Length { silence: true });
        assert!(sheet.silence());

        let layout = ExportSheetView::layout(&sheet, area).expect("room");
        assert!(layout.second.width > 0, "the hold appeared: {layout:?}");
        let hit = ExportSheetView::hit(&sheet, area, layout.second.x, layout.second.y);
        assert_eq!(hit, Some(Hit::Number(Field::Second)));
        if MP3_EXPORT {
            let hit = ExportSheetView::hit(&sheet, area, layout.mp3.x + 1, layout.mp3.y);
            assert_eq!(hit, Some(Hit::Format { mp3: true }));
            sheet.click(Hit::Format { mp3: true });
            assert!(sheet.settings.mp3);
            assert!(sheet.target.to_string_lossy().ends_with(".mp3"));
        } else {
            assert_eq!(
                layout.mp3,
                Rect::default(),
                "no mp3 radio without the encoder"
            );
        }
        assert_eq!(
            ExportSheetView::hit(&sheet, area, layout.panel.x + 3, layout.target.y),
            Some(Hit::Target),
            "the whole to row is the path"
        );
        assert_eq!(
            ExportSheetView::hit(&sheet, area, layout.panel.x + 3, layout.panel.y),
            None,
            "the border is nothing"
        );
    }

    #[test]
    fn the_to_line_is_typed_into_and_keeps_its_own_extension() {
        let mut sheet = ExportSheet::open(
            "drums".into(),
            ExportSettings::default(),
            Path::new("exports"),
            0,
        );
        for _ in 0..4 {
            sheet.key(KeyCode::Tab, false);
        }
        assert_eq!(sheet.field, Field::Target);
        while !sheet.target.as_os_str().is_empty() {
            sheet.key(KeyCode::Backspace, false);
        }
        for character in "/tmp/bounces/take one".chars() {
            sheet.key(KeyCode::Char(character), false);
        }
        assert_eq!(sheet.target, PathBuf::from("/tmp/bounces/take one"));
        // Enter's settings() adds the format where the reader typed none…
        sheet.settings();
        assert_eq!(sheet.target, PathBuf::from("/tmp/bounces/take one.wav"));
        // …and a flip of the format follows it, but never rewrites a
        // deliberate extension.
        sheet.key(KeyCode::Tab, true);
        sheet.key(KeyCode::Tab, true);
        sheet.key(KeyCode::Right, false);
        let extension = if MP3_EXPORT { "mp3" } else { "wav" };
        assert_eq!(
            sheet.target,
            PathBuf::from(format!("/tmp/bounces/take one.{extension}"))
        );
        sheet.target = PathBuf::from("/tmp/bounces/take.final");
        sheet.key(KeyCode::Left, false);
        assert_eq!(sheet.target, PathBuf::from("/tmp/bounces/take.final"));
        // Tab wraps back to the top through the limiter and to lines.
        sheet.key(KeyCode::Tab, false);
        sheet.key(KeyCode::Tab, false);
        sheet.key(KeyCode::Tab, false);
        assert_eq!(sheet.field, Field::Length);
    }

    #[test]
    fn a_long_path_keeps_its_tail_on_screen() {
        assert_eq!(tail("short", 10), "short");
        assert_eq!(tail("/very/long/path/name.wav", 10), "…/name.wav");
    }

    #[test]
    fn the_export_job_limits_wav_and_mp3_and_can_bypass_or_make_up_gain() {
        let dir = tempfile::tempdir().expect("export dir");
        let bounce = |settings: ExportSettings, filename: &str| {
            let target = dir.path().join(filename);
            let job = ExportJob::start(
                "loud".into(),
                "s('sine').gain(8)".into(),
                false,
                settings,
                SessionConfig::default(),
                target.clone(),
            )
            .expect("job");
            let deadline = Instant::now() + std::time::Duration::from_secs(120);
            loop {
                if let Some(result) = job.poll() {
                    assert_eq!(result.expect("render"), 0.5);
                    break;
                }
                assert!(Instant::now() < deadline, "export timed out");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            target
        };
        let mut settings = ExportSettings {
            length: Length::Cycles(0.25),
            limiter_enabled: true,
            limiter: rustel_audio::RenderLimiter {
                settings: rustel_audio::LimiterSettings {
                    threshold_db: -12.0,
                    character: rustel_audio::LimiterCharacter::Warm,
                },
                makeup: false,
            },
            ..ExportSettings::default()
        };
        let limited = bounce(settings, "limited.wav");
        let pcm = rustel_runtime::test_support::read_pcm16_stereo_wav(&limited).expect("WAV");
        let peak = |pcm: &[f32]| pcm.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
        assert!(
            (0.24..0.252).contains(&peak(&pcm)),
            "ceiling: {}",
            peak(&pcm)
        );

        if MP3_EXPORT {
            settings.mp3 = true;
            let mp3 = bounce(settings, "limited.mp3");
            let expected = dir.path().join("expected.mp3");
            rustel_runtime::test_support::encode_mp3(&expected, 48_000, &pcm)
                .expect("encode limited WAV");
            assert_eq!(
                std::fs::read(mp3).unwrap(),
                std::fs::read(expected).unwrap(),
                "MP3 must encode the limited audio"
            );
            settings.mp3 = false;
        }
        settings.limiter.makeup = true;
        let makeup = bounce(settings, "makeup.wav");
        let makeup = rustel_runtime::test_support::read_pcm16_stereo_wav(&makeup).unwrap();
        assert!((0.99..=1.0).contains(&peak(&makeup)));

        settings.limiter_enabled = false;
        let bypassed = bounce(settings, "bypassed.wav");
        let bypassed = rustel_runtime::test_support::read_pcm16_stereo_wav(&bypassed).unwrap();
        assert!(peak(&bypassed) > 0.9);
        assert_ne!(
            makeup, bypassed,
            "limiting with makeup still shapes the signal"
        );
    }

    /// The job's outcome, polled until it arrives or `within` runs out.
    fn await_outcome(job: &ExportJob, within: std::time::Duration) -> Result<f64, String> {
        let deadline = Instant::now() + within;
        loop {
            if let Some(outcome) = job.poll() {
                return outcome;
            }
            assert!(Instant::now() < deadline, "export never finished");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn a_job_renders_one_score_to_a_file() {
        let dir = std::env::temp_dir().join(format!("rustel-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let target = dir.join("bounce.wav");
        let job = ExportJob::start(
            "melody".into(),
            "note(\"c3 e3 g3\").s(\"sine\")".into(),
            false,
            ExportSettings {
                length: Length::Cycles(2.0),
                mp3: false,
                ..ExportSettings::default()
            },
            SessionConfig::default(),
            target.clone(),
        )
        .unwrap();
        let seconds = await_outcome(&job, std::time::Duration::from_secs(120)).expect("rendered");
        assert!(
            (seconds - 4.0).abs() < 0.05,
            "two cycles at 0.5 cps: {seconds}"
        );
        let bytes = std::fs::metadata(&target).unwrap().len();
        assert!(bytes > 44, "{bytes}");
        let glance = job.glance();
        assert_eq!(glance.percent, Some(100));
        assert!((glance.seconds - 4.0).abs() < 0.05, "{glance:?}");
        assert!(
            glance.peaks.iter().any(|peak| *peak > 0.01),
            "the scope heard the sines: {:?}",
            glance.peaks
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A pattern that threw bounces silent where the score is not. The file
    /// is written, but the job reports the CLI's failure - the sheet shows
    /// "export of … failed: …" - rather than a finished export.
    #[test]
    fn a_job_whose_pattern_threw_fails_with_the_clis_words() {
        let dir = tempfile::tempdir().expect("export dir");
        let target = dir.path().join("threw.wav");
        let job = ExportJob::start(
            "threw".into(),
            r#"note("c4 e4").fmap(x => { throw new Error("boom-in-query") })"#.into(),
            false,
            ExportSettings {
                length: Length::Cycles(0.5),
                mp3: false,
                ..ExportSettings::default()
            },
            SessionConfig::default(),
            target.clone(),
        )
        .expect("job");
        let error = await_outcome(&job, std::time::Duration::from_secs(120))
            .expect_err("a silent bounce is not a finished export");
        assert!(
            error
                .starts_with("the pattern threw while querying, so part of the bounce is silent: ")
                && error.contains("boom-in-query"),
            "{error}"
        );
        assert!(target.exists(), "the file is written either way");
    }

    #[test]
    fn max_polyphony_export_jobs_honor_host_defaults_and_score_overrides() {
        fn bounce(voices: usize, inherited: Option<usize>, override_voices: bool) -> Vec<u8> {
            let target = std::env::temp_dir().join(format!(
                "rustel-export-polyphony-{}-{voices}-{inherited:?}-{override_voices}.wav",
                std::process::id()
            ));
            let setter = if override_voices {
                "setMaxPolyphony(4);"
            } else {
                ""
            };
            let job = ExportJob::start(
                "polyphony".into(),
                format!("{setter} stack(note('c3'), note('e3'), note('g3'), note('c4')).s('sine').gain(0.05).attack(0).decay(0).sustain(1)"),
                false,
                ExportSettings { length: Length::Cycles(0.025), mp3: false, ..ExportSettings::default() },
                session_config(&SessionConfig::default(), voices, inherited),
                target.clone(),
            ).expect("start export job");
            let deadline = Instant::now() + std::time::Duration::from_secs(30);
            loop {
                if let Some(result) = job.poll() {
                    result.expect("export WAV");
                    break;
                }
                assert!(Instant::now() < deadline, "export did not finish");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let wav = std::fs::read(&target).expect("WAV");
            std::fs::remove_file(&target).expect("remove test WAV");
            wav
        }
        let limited = bounce(1, None, false);
        let unrestricted = bounce(4, None, false);
        assert_ne!(
            limited, unrestricted,
            "the job must use the passed host budget"
        );
        assert_eq!(
            session_config(&SessionConfig::default(), 192, None).max_polyphony,
            192
        );
        let mut live = Session::new().expect("live session");
        live.evaluate("setMaxPolyphony(1); s('sine')")
            .expect("set persistent override");
        live.evaluate("s('sine')")
            .expect("source no longer contains the setter");
        let inherited = live.max_polyphony_override();
        assert_eq!(inherited, Some(1));
        assert_eq!(
            limited,
            bounce(192, inherited, false),
            "export inherits a prior accepted setting even after its setter is removed"
        );
        assert_eq!(
            unrestricted,
            bounce(192, inherited, true),
            "an explicit exported score overrides inherited module state"
        );
        assert_eq!(
            unrestricted,
            bounce(1, None, true),
            "an explicit score override wins"
        );
    }

    #[test]
    fn finishing_a_job_early_keeps_what_was_rendered() {
        let dir = std::env::temp_dir().join(format!("rustel-export-early-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let target = dir.join("bounce.wav");
        let mut job = ExportJob::start(
            "melody".into(),
            "note(\"c3 e3 g3\").s(\"sine\")".into(),
            false,
            ExportSettings {
                length: Length::Cycles(60.0),
                mp3: false,
                ..ExportSettings::default()
            },
            SessionConfig::default(),
            target.clone(),
        )
        .unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(120);
        // Let it get going, then end it.
        while job.glance().seconds < 0.5 {
            assert!(Instant::now() < deadline, "never started rendering");
            assert!(job.poll().is_none(), "finished before it was asked to");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        job.finish();
        let outcome = loop {
            if let Some(outcome) = job.poll() {
                break outcome;
            }
            assert!(Instant::now() < deadline, "export never finished");
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let seconds = outcome.expect("rendered");
        assert!(job.finished_early());
        assert!((0.5..30.0).contains(&seconds), "{seconds}");
        let bytes = std::fs::metadata(&target).unwrap().len();
        assert_eq!(bytes, 44 + (seconds * 48_000.0).round() as u64 * 4);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
