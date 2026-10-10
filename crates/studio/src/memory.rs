//! Where the header's memory goes: the right column of the log panel.
//!
//! The header says how much memory the studio has - one number, the figure
//! the platform's own monitor shows. This splits it into the parts the
//! studio can count exactly and cheaply: the decoded sounds, the way the
//! engine's memory policy holds them; the script engine's heap; the audio
//! output while one is open; the visuals while Hydra has a renderer open;
//! and the interface's own buffers. What none of those covers is one
//! remainder, so the breakdown always adds up to the figure it explains and
//! never claims to know more than it does.
//!
//! The plugins of the plugin host follow the remainder, one row each. A
//! plugin in the Studio process has no figure of its own, and a plugin
//! process is outside the figure, so the plugin rows add nothing to the sum.
//!
//! Clicking the header's figures opens the log with this breakdown to its
//! left. The two share a border and resize together. On a short log, the
//! total and principal parts stay visible before the details.

use std::time::Duration;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    widgets::Widget,
};
use unicode_width::UnicodeWidthStr;

use super::engine::{AudioMemory, SampleMemory};
use super::stats::format_bytes;
use super::theme::Theme;
use super::viz_panel::{BAND_MAX_HEIGHT, BAND_MIN_HEIGHT, Dock, Edge};

/// What a value reads while nobody has said it yet: before the engine's
/// first snapshot, or on a platform that does not report the figure.
const UNKNOWN: &str = "-";

/// Everything the breakdown reads, gathered on the UI thread while it is
/// open.
///
/// The engine's parts come from its last snapshot, so they are as old as
/// that - a tenth of a second, or the last one before a busy stretch. The
/// header's figure is sampled at most twice a second. The two are never
/// read at the same instant, which is why the remainder can come out
/// below zero and [`rows`] says so rather than printing it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MemoryFigures {
    /// The header's figure: the footprint, or the resident set where the
    /// platform reports no footprint.
    pub total: Option<u64>,
    /// The resident set, which also counts pages mapped from files and, on
    /// macOS, freed pages the system has not yet taken back.
    pub resident: Option<u64>,
    /// The decoded sounds and the limits holding them; `None` until the
    /// engine's first snapshot.
    pub samples: Option<SampleMemory>,
    /// The script engine's heap; `None` until the engine's first snapshot.
    pub script_heap: Option<usize>,
    /// What the audio side holds, only while an output or input is open.
    pub audio: Option<AudioMemory>,
    /// What Hydra holds, only while it has a renderer open or a camera or
    /// image frame waiting; never in a build without it.
    pub visuals: Option<VisualsMemory>,
    /// The terminal's size in cells: the size of each frame buffer.
    pub screen: (u16, u16),
    /// The two frame buffers the terminal keeps, a `Cell` for every
    /// character cell in each.
    pub screen_bytes: usize,
    /// Pictures the interface holds: Hydra's last frames, and the images a
    /// graphics terminal has been sent and may be asked to redraw.
    pub pictures: usize,
    /// The spectrograms' column histories, one per audio tap.
    pub spectrograms: usize,
    /// Tabs open, each holding its text and its undo.
    pub tabs_open: usize,
    /// Scores in the set's folder with no tab: a count, because a closed
    /// file holds nothing.
    pub tabs_closed: usize,
    /// The open tabs' text and the text their undo keeps. The undo's own
    /// bookkeeping is not counted, so this reads low.
    pub tab_bytes: usize,
    /// The transport is running, which keeps freed memory from going back
    /// to the system until it stops, where the allocator is asked for it.
    pub playing: bool,
    /// Freed memory goes back only when the engine asks the allocator for
    /// it on its idle turns: glibc's Linux and macOS. Windows' heap hands a
    /// large block back as it is freed, and there is nothing to ask.
    pub returned_when_stopped: bool,
    /// What the sample cache takes on disk, once measured.
    pub sample_cache: Option<u64>,
    /// Whether imported sounds are fetched to disk ahead of their first
    /// play: a disk setting, shown beside the disk it fills.
    pub fetch_imports: bool,
    /// The plugins of the plugin host: each plugin in its load, loaded, or
    /// failed. Empty with no host, and in a build with no plugins.
    pub plugins: Vec<PluginMemory>,
}

impl MemoryFigures {
    /// The interface's parts together.
    fn interface(&self) -> u64 {
        [
            self.screen_bytes,
            self.pictures,
            self.spectrograms,
            self.tab_bytes,
        ]
        .into_iter()
        .map(|bytes| bytes as u64)
        .sum()
    }
}

/// One of Hydra's renderers, as the breakdown lists it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VisualRenderer {
    /// The picture it draws: the score's, the theme's, the shelf's preview.
    pub picture: &'static str,
    /// The size it renders at, supersampling included, which is what its
    /// textures are sized by.
    pub size: (u32, u32),
    /// What of it counts toward the figure.
    pub bytes: u64,
}

/// What the visuals hold: Hydra's renderers - one per picture being drawn,
/// kept a few seconds after it stops - and the camera and image frames
/// decoded for them.
///
/// The renderers' share is counted from the textures and buffers each one
/// asked the GPU API for, which is why the breakdown marks it `≈`: the
/// graphics driver's own memory - the device, compiled shaders, its
/// allocator's padding - is in no API, and stays in the remainder.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VisualsMemory {
    /// The open renderers, in the order the breakdown lists them.
    pub renderers: Vec<VisualRenderer>,
    /// Camera and image frames decoded and waiting for the GPU.
    pub inputs: u64,
    /// Textures a GPU's driver holds outside the process's figure - a
    /// discrete card's, and an integrated one's on Linux and Windows,
    /// whose buffers neither the anonymous and shared RSS nor the private
    /// working set counts. So not part of the figure: said beside it,
    /// never added to it. On a software rasteriser or Apple silicon the
    /// same textures are the process's, and are in the renderers' bytes.
    pub gpu: u64,
}

impl VisualsMemory {
    /// What Hydra reports, or `None` while it holds nothing, so an idle
    /// build with visuals shows no line for them.
    #[cfg(feature = "hydra")]
    pub fn from_hydra(memory: rustel_hydra::HydraMemory) -> Option<Self> {
        (!memory.is_empty()).then(|| Self {
            renderers: memory
                .renderers()
                .map(|(picture, footprint)| VisualRenderer {
                    picture,
                    size: (footprint.width, footprint.height),
                    bytes: footprint.process_bytes() as u64,
                })
                .collect(),
            inputs: memory.inputs as u64,
            gpu: memory.gpu_bytes() as u64,
        })
    }

    /// The part of the figure they come to.
    fn total(&self) -> u64 {
        self.renderers
            .iter()
            .map(|renderer| renderer.bytes)
            .sum::<u64>()
            + self.inputs
    }
}

/// One plugin of the plugin host, as a row of the breakdown.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PluginMemory {
    /// The name, then the kind, the state, the running copies and the time
    /// of the load, as far as the host knows them.
    pub label: String,
    /// The memory of the process of the plugin, in bytes, with each
    /// process the plugin started. 0 for a plugin whose process is the
    /// figure of an earlier row.
    pub process: Option<u64>,
    /// The host loaded the plugin. With no process of its own, the plugin
    /// is in the Studio process and its memory is part of the figure.
    pub loaded: bool,
}

/// Which part a detail row belongs to, which decides when it goes on a
/// short terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Detail {
    Sounds,
    Audio,
    Visuals,
    Interface,
    Plugins,
}

/// What a row is, which decides how it is drawn and when it is dropped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RowKind {
    /// The header's figure.
    Title,
    /// A part of the figure. The parts add up to it.
    Part,
    /// What a part is made of, indented under it; the details add up to
    /// their part.
    Detail(Detail),
    /// The parts came to more than the figure: said, muted, and never as
    /// a negative remainder.
    Excess,
    /// A muted line saying what the remainder holds.
    Hint,
    /// A line about memory outside the figure: the resident set, the disk.
    Outside,
}

/// One line of the breakdown: a label, and the figure it stands for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Row {
    pub kind: RowKind,
    pub label: String,
    /// What the value column reads; empty for a line that is all label.
    pub value: String,
    /// The bytes behind the value, when there are any.
    pub bytes: Option<u64>,
}

impl Row {
    fn figure(kind: RowKind, label: impl Into<String>, bytes: u64) -> Self {
        Self {
            kind,
            label: label.into(),
            value: format_bytes(bytes),
            bytes: Some(bytes),
        }
    }

    fn unknown(kind: RowKind, label: impl Into<String>) -> Self {
        Self {
            kind,
            label: label.into(),
            value: UNKNOWN.to_owned(),
            bytes: None,
        }
    }

    fn text(kind: RowKind, label: impl Into<String>) -> Self {
        Self {
            kind,
            label: label.into(),
            value: String::new(),
            bytes: None,
        }
    }
}

/// A pause as the settings sheet spells one: `30 s`, `5 min`.
fn pause(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds >= 60 && seconds.is_multiple_of(60) {
        format!("{} min", seconds / 60)
    } else {
        format!("{seconds} s")
    }
}

/// The breakdown's lines, in order: the header's figure, the parts that add up
/// to it with what each is made of, the remainder and what it holds, then
/// the memory outside the figure.
pub fn rows(figures: &MemoryFigures) -> Vec<Row> {
    use RowKind::{Detail as Of, Excess, Hint, Outside, Part, Title};
    let mut rows = vec![match figures.total {
        Some(total) => Row::figure(Title, "memory", total),
        None => Row::unknown(Title, "memory"),
    }];
    // What the parts come to, for the remainder. A part not known yet
    // counts nothing, so the remainder is what is known not to be it.
    let mut counted: u64 = 0;
    match figures.samples {
        Some(samples) => {
            let sounds = (samples.live_bytes + samples.preview_bytes) as u64;
            counted += sounds;
            rows.push(Row::figure(Part, "sounds", sounds));
            rows.push(Row::figure(
                Of(Detail::Sounds),
                "kept · no byte cap · playing, panes, pads, setup, launch",
                samples.live_bytes.saturating_sub(samples.recent_bytes) as u64,
            ));
            rows.push(Row::figure(
                Of(Detail::Sounds),
                format!(
                    "recent tabs · up to {}",
                    format_bytes(samples.recent_limit_bytes as u64)
                ),
                samples.recent_bytes as u64,
            ));
            // The limits are the engine's, not the settings': what holds
            // is what the breakdown says holds.
            let cap = match samples.preview_budget_bytes {
                0 => "no cap".to_owned(),
                budget => format!("cap {}", format_bytes(budget as u64)),
            };
            let idle = if samples.unused_idle.is_zero() {
                "never dropped".to_owned()
            } else {
                format!("drop after {}", pause(samples.unused_idle))
            };
            rows.push(Row::figure(
                Of(Detail::Sounds),
                format!("previews · {cap} · {idle}"),
                samples.preview_bytes as u64,
            ));
        }
        None => rows.push(Row::unknown(Part, "sounds")),
    }
    match figures.script_heap {
        Some(heap) => {
            counted += heap as u64;
            rows.push(Row::figure(Part, "script engine", heap as u64));
        }
        None => rows.push(Row::unknown(Part, "script engine")),
    }
    // Only while something is open: the audio side is nothing otherwise.
    // Its parts are what has been written, not what was set aside, so the
    // row says it is an estimate.
    if let Some(audio) = figures.audio {
        let total = audio.total() as u64;
        counted += total;
        let mut row = Row::figure(Part, "audio output", total);
        row.value = format!("≈ {}", row.value);
        rows.push(row);
        for (label, bytes) in [
            ("event ring", audio.event_ring),
            ("input", audio.input),
            ("record", audio.record),
            ("reverbs", audio.reverbs),
            ("backend at open", audio.backend),
            ("other", audio.other),
        ] {
            if bytes > 0 {
                rows.push(Row::figure(Of(Detail::Audio), label, bytes as u64));
            }
        }
    }
    // Only while Hydra holds something: a renderer is kept a few seconds
    // after its picture goes, then the line goes with it. Counted from
    // what was asked of the GPU API, so an estimate like the audio's.
    if let Some(visuals) = &figures.visuals {
        let total = visuals.total();
        counted += total;
        let mut row = Row::figure(Part, "visuals", total);
        row.value = format!("≈ {}", row.value);
        rows.push(row);
        for renderer in &visuals.renderers {
            let (width, height) = renderer.size;
            rows.push(Row::figure(
                Of(Detail::Visuals),
                format!("{} renderer · {width}×{height}", renderer.picture),
                renderer.bytes,
            ));
        }
        if visuals.inputs > 0 {
            rows.push(Row::figure(
                Of(Detail::Visuals),
                "camera and image inputs",
                visuals.inputs,
            ));
        }
        if visuals.gpu > 0 {
            rows.push(Row::text(
                Hint,
                format!(
                    "+ {} of textures in GPU memory, outside mem",
                    format_bytes(visuals.gpu)
                ),
            ));
        }
        // A camera frame waiting with no renderer open has no driver
        // behind it to speak of.
        if !visuals.renderers.is_empty() {
            rows.push(Row::text(
                Hint,
                "graphics driver's own memory is not itemised",
            ));
        }
    }
    let interface = figures.interface();
    counted += interface;
    rows.push(Row::figure(Part, "interface", interface));
    let (width, height) = figures.screen;
    for (label, bytes) in [
        (format!("screen {width}×{height}"), figures.screen_bytes),
        ("pictures".to_owned(), figures.pictures),
        ("spectrograms".to_owned(), figures.spectrograms),
        (
            format!(
                "tabs · {} open, {} closed · text + undo",
                figures.tabs_open, figures.tabs_closed
            ),
            figures.tab_bytes,
        ),
    ] {
        rows.push(Row::figure(Of(Detail::Interface), label, bytes as u64));
    }
    match figures.total {
        Some(total) if counted <= total => {
            rows.push(Row::figure(Part, "not itemised", total - counted));
            rows.push(Row::text(
                Hint,
                "allocator-held freed memory, stacks, tables, code data",
            ));
            // Where freed memory is handed back on the engine's idle turns
            // only, a sound let go mid-set leaves the figure where it was.
            if figures.playing && figures.returned_when_stopped {
                rows.push(Row::text(Hint, "freed memory returns when stopped"));
            }
        }
        // The parts and the figure are read at different moments, and the
        // system can page part of what the parts count out: a remainder
        // below zero is a reading, not a fact, and is said as one.
        Some(total) => rows.push(Row::figure(
            Excess,
            "items exceed the figure by",
            counted - total,
        )),
        None => rows.push(Row::unknown(Part, "not itemised")),
    }
    // Only while the plugin host has a plugin loaded, loading or failed.
    // The heading is no part of the figure and counts nothing: a plugin in
    // the Studio process has its memory in the parts above, and a plugin
    // process is outside the figure.
    if !figures.plugins.is_empty() {
        let loaded = figures.plugins.iter().filter(|plugin| plugin.loaded);
        let mut heading = Row::text(Part, "plugins");
        heading.value = format!("{} loaded", loaded.count());
        rows.push(heading);
        for plugin in &figures.plugins {
            let mut row = match plugin.process {
                Some(bytes) if bytes > 0 => Row::figure(Of(Detail::Plugins), &plugin.label, bytes),
                _ => Row::text(Of(Detail::Plugins), &plugin.label),
            };
            if plugin.process == Some(0) {
                row.value = "same process".to_owned();
            } else if plugin.process.is_none() && plugin.loaded {
                row.value = "in process".to_owned();
            }
            rows.push(row);
        }
    }
    let resident = match figures.resident {
        Some(resident) => format!(
            "RSS {} · {} beyond mem",
            format_bytes(resident),
            format_bytes(resident.saturating_sub(figures.total.unwrap_or(resident)))
        ),
        None => format!("RSS {UNKNOWN}"),
    };
    rows.push(Row::text(Outside, resident));
    rows.push(Row::text(
        Outside,
        format!(
            "on disk · sample cache {} · fetch imports {}",
            figures
                .sample_cache
                .map_or_else(|| UNKNOWN.to_owned(), format_bytes),
            if figures.fetch_imports { "on" } else { "off" }
        ),
    ));
    rows
}

/// When a row goes on a short terminal, earliest first: the hints, then
/// what is outside the figure, then the interface's, the audio's, the
/// visuals' and the plugins' details, then the sounds'. `None` stays
/// whatever the height - the figure, its parts and the remainder, which are
/// the breakdown's whole point.
fn drop_rank(kind: RowKind) -> Option<u8> {
    match kind {
        RowKind::Hint => Some(0),
        RowKind::Outside => Some(1),
        RowKind::Detail(Detail::Interface | Detail::Audio | Detail::Visuals | Detail::Plugins) => {
            Some(2)
        }
        RowKind::Detail(Detail::Sounds) => Some(3),
        RowKind::Title | RowKind::Part | RowKind::Excess => None,
    }
}

/// The rows that fit in `room` lines. A group goes whole or not at all:
/// half a part's details would no longer add up to it.
pub fn fit(mut rows: Vec<Row>, room: usize) -> Vec<Row> {
    for rank in 0..=3 {
        if rows.len() <= room {
            break;
        }
        rows.retain(|row| drop_rank(row.kind) != Some(rank));
    }
    rows
}

/// Keep the figures that explain the total when the log is only a few rows
/// tall. Details return in their normal order as the log grows.
pub fn sidecar_rows(figures: &MemoryFigures, room: usize) -> Vec<Row> {
    let all = rows(figures);
    if all.len() <= room {
        return all;
    }
    let mut ranked: Vec<_> = all.into_iter().enumerate().collect();
    ranked.sort_by_key(|(index, row)| {
        let priority = match row.kind {
            RowKind::Title => 0,
            RowKind::Excess => 3,
            RowKind::Part => match row.label.as_str() {
                "sounds" => 1,
                "script engine" => 2,
                "not itemised" => 3,
                "interface" => 4,
                _ => 5,
            },
            // A plugin row stays with its heading, which has no figure to
            // stand for the rows.
            RowKind::Detail(Detail::Plugins) => 5,
            RowKind::Outside => 6,
            RowKind::Detail(_) => 7,
            RowKind::Hint => 8,
        };
        (priority, *index)
    });
    ranked.truncate(room);
    ranked.sort_by_key(|(index, _)| *index);
    ranked.into_iter().map(|(_, row)| row).collect()
}

/// Memory's compact column inside the log border. The log owns the border
/// and the only close action; this column shrinks to totals on short docks.
pub struct MemorySidecarView<'a> {
    pub figures: &'a MemoryFigures,
    pub theme: &'a Theme,
}

impl Widget for MemorySidecarView<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        for (line, row) in sidecar_rows(self.figures, area.height.into())
            .iter()
            .enumerate()
        {
            let y = area.y + line as u16;
            let (label_style, value_style) = match row.kind {
                RowKind::Title => (
                    Style::default()
                        .fg(self.theme.accent)
                        .add_modifier(Modifier::BOLD),
                    Style::default()
                        .fg(self.theme.foreground)
                        .add_modifier(Modifier::BOLD),
                ),
                RowKind::Part => (
                    Style::default().fg(self.theme.foreground),
                    Style::default().fg(self.theme.foreground),
                ),
                _ => (
                    Style::default().fg(self.theme.muted),
                    Style::default().fg(self.theme.muted),
                ),
            };
            let value_width = UnicodeWidthStr::width(row.value.as_str()) as u16;
            let value_x = area.right().saturating_sub(value_width).max(area.x);
            if !row.value.is_empty() {
                buffer.set_stringn(
                    value_x,
                    y,
                    &row.value,
                    usize::from(area.right() - value_x),
                    value_style,
                );
            }
            let label_width = if row.value.is_empty() {
                area.width
            } else {
                value_x.saturating_sub(area.x + 1)
            };
            let label = super::view::ellipsize(&row.label, label_width);
            buffer.set_stringn(area.x, y, &label, usize::from(label_width), label_style);
        }
    }
}

/// The breakdown while it is open: the band it is docked as.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryDock {
    /// The top or the bottom. A side is read as the bottom, as the log's
    /// is: the breakdown is rows of a label and a figure, read across.
    pub edge: Edge,
    /// The rows `-` and `+` made it; unset, [`default_height`] for the
    /// terminal's width.
    pub height: Option<u16>,
}

impl MemoryDock {
    /// The rows it asks for on a terminal `width` cells wide, within the
    /// bounds every band keeps.
    pub fn height_on(&self, width: u16) -> u16 {
        self.height.map_or_else(
            || default_height(width),
            |rows| rows.clamp(BAND_MIN_HEIGHT, BAND_MAX_HEIGHT),
        )
    }

    /// The room the layout is asked for on a terminal the size of `frame`.
    pub fn dock(&self, frame: Rect) -> Dock {
        Dock {
            edge: self.edge.band(),
            extent: self.height_on(frame.width),
        }
    }
}

/// The narrowest a column of the breakdown is laid at: a detail's label
/// and its figure side by side without cutting either.
const COLUMN_MIN_WIDTH: u16 = 50;
/// Cells between two columns.
const COLUMN_GAP: u16 = 3;
/// The most columns side by side. Three hold the whole breakdown at a
/// band's everyday height; a fourth would only spread the same rows
/// thinner.
const MAX_COLUMNS: usize = 3;

/// Where the rows go inside the band: a cell of border and a cell of
/// margin in from each side, and the border's row top and bottom.
fn inner(area: Rect) -> Rect {
    Rect::new(
        area.x.saturating_add(2),
        area.y.saturating_add(1),
        area.width.saturating_sub(4),
        area.height.saturating_sub(2),
    )
}

/// How many columns side by side a band whose inside is `width` cells
/// lays its rows in: as many as fit at [`COLUMN_MIN_WIDTH`], at least one
/// and at most [`MAX_COLUMNS`]. A band is short and wide, and one column
/// down it would be cut by `fit` long before a column across was full.
fn columns_for(width: u16) -> usize {
    usize::from((width + COLUMN_GAP) / (COLUMN_MIN_WIDTH + COLUMN_GAP)).clamp(1, MAX_COLUMNS)
}

/// The rows' groups, as ranges of `rows`: a part with its details and the
/// hints under it, the header's figure, a line outside the figure. A
/// column break falls between groups, never inside one - details away
/// from their part would no longer be read as adding up to it.
fn groups(rows: &[Row]) -> Vec<std::ops::Range<usize>> {
    let mut groups: Vec<std::ops::Range<usize>> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        match (row.kind, groups.last_mut()) {
            (RowKind::Detail(_) | RowKind::Hint, Some(group)) => group.end = index + 1,
            _ => groups.push(index..index + 1),
        }
    }
    groups
}

/// The rows split, in order and between groups, into at most `columns`
/// columns, so the tallest is as short as it can be; of two splits as
/// tall, the one in fewer columns.
fn balance(rows: Vec<Row>, columns: usize) -> Vec<Vec<Row>> {
    let groups = groups(&rows);
    let sizes: Vec<usize> = groups.iter().map(ExactSizeIterator::len).collect();
    // Every way of cutting the groups into `columns` runs or fewer, as the
    // indices each run ends at. There are at most a dozen groups and three
    // columns, so trying them all is cheaper than being clever.
    let mut best: Option<(usize, Vec<usize>)> = None;
    let mut consider = |ends: Vec<usize>| {
        let mut start = 0;
        let tallest = ends
            .iter()
            .map(|&end| {
                let height = sizes[start..end].iter().sum::<usize>();
                start = end;
                height
            })
            .max()
            .unwrap_or(0);
        let better = best
            .as_ref()
            .is_none_or(|(height, cut)| (tallest, ends.len()) < (*height, cut.len()));
        if better {
            best = Some((tallest, ends));
        }
    };
    let count = sizes.len();
    consider(vec![count]);
    if columns >= 2 {
        for first in 1..count {
            consider(vec![first, count]);
            if columns >= 3 {
                for second in first + 1..count {
                    consider(vec![first, second, count]);
                }
            }
        }
    }
    let ends = best.map_or_else(|| vec![count], |(_, ends)| ends);
    let mut rows = rows.into_iter();
    let mut start = 0;
    ends.into_iter()
        .map(|end| {
            let take = sizes[start..end].iter().sum::<usize>();
            start = end;
            rows.by_ref().take(take).collect()
        })
        .collect()
}

/// The rows laid in `columns` columns side by side, none taller than
/// `room`. When even the most even split is too tall, the details go the
/// way [`fit`] drops them in one column - the hints, then the lines
/// outside the figure, then the interface's, the audio's and the visuals'
/// details, then the sounds' - until it is not; the figure and its parts
/// always stay, cut at the band's foot only if the band is shorter than
/// they are.
fn lay_out(mut rows: Vec<Row>, room: usize, columns: usize) -> Vec<Vec<Row>> {
    if columns <= 1 {
        let mut column = fit(rows, room);
        column.truncate(room);
        return vec![column];
    }
    let mut rank = 0;
    loop {
        let laid = balance(rows.clone(), columns);
        if rank > 3 || laid.iter().all(|column| column.len() <= room) {
            return laid
                .into_iter()
                .map(|mut column| {
                    column.truncate(room);
                    column
                })
                .collect();
        }
        rows.retain(|row| drop_rank(row.kind) != Some(rank));
        rank += 1;
    }
}

/// The rows the dock takes until it is resized, on a terminal `width`
/// cells wide: what the breakdown needs in the columns that width allows,
/// with an output open and its usual three parts, and its border - within
/// a band's bounds. Worked out from the width alone, never from the live
/// figures, so a band does not grow and push the score up the moment an
/// output opens and adds its lines.
pub fn default_height(width: u16) -> u16 {
    let everyday = MemoryFigures {
        total: Some(1 << 30),
        resident: Some(1 << 30),
        samples: Some(SampleMemory::default()),
        script_heap: Some(0),
        audio: Some(AudioMemory {
            event_ring: 1,
            backend: 1,
            other: 1,
            ..AudioMemory::default()
        }),
        ..MemoryFigures::default()
    };
    let columns = columns_for(width.saturating_sub(4));
    let tallest = balance(rows(&everyday), columns)
        .iter()
        .map(Vec::len)
        .max()
        .unwrap_or(0);
    u16::try_from(tallest + 2)
        .unwrap_or(u16::MAX)
        .clamp(BAND_MIN_HEIGHT, BAND_MAX_HEIGHT)
}

/// Whether a point is on the close mark at the right end of the band's
/// top row, or the cell either side of it: a target a hand can hit.
pub fn close_at(area: Rect, x: u16, y: u16) -> bool {
    let mark = area.right().saturating_sub(3);
    !area.is_empty() && y == area.y && x.saturating_add(1) >= mark && x <= mark + 1
}

/// The docked breakdown: a bordered band like the docked log's, its title
/// and close mark on the top row and its rows in columns, the values of
/// each column in one column of their own so the parts can be read against
/// the figure they add up to.
pub struct MemoryView<'a> {
    pub figures: &'a MemoryFigures,
    pub theme: &'a Theme,
    /// It has the keyboard, so the bottom row says what its keys do.
    pub focused: bool,
    /// Esc puts it away rather than hand the keyboard back: zen, where it
    /// is a band laid over the score rather than docked beside it, and
    /// where the status line that opened it already says "Esc closes it".
    /// The hint must not tell the eye one thing and the key do another.
    pub closes_on_esc: bool,
}

impl Widget for MemoryView<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.width < 8 || area.height < 3 {
            return;
        }
        let theme = self.theme;
        super::view::clear_overlay(
            buffer,
            area,
            Style::default().bg(theme.overlay).fg(theme.foreground),
        );
        super::devices::draw_border(buffer, area, theme);
        let muted = Style::default().fg(theme.muted);
        let plain = Style::default().fg(theme.foreground);
        // The title as the log's is set, and the close mark at the other
        // end of the same row, clear of the corner.
        buffer.set_stringn(
            area.x + 2,
            area.y,
            " memory breakdown ",
            usize::from(area.width.saturating_sub(7)),
            muted.add_modifier(Modifier::BOLD),
        );
        buffer.set_stringn(area.right().saturating_sub(4), area.y, " × ", 3, muted);
        if self.focused {
            let hint = if self.closes_on_esc {
                " e top/bottom · -/+ height · Esc closes "
            } else {
                " e top/bottom · -/+ height · Esc to the score "
            };
            let width = UnicodeWidthStr::width(hint) as u16;
            if area.width >= width + 4 {
                buffer.set_stringn(
                    area.x + 2,
                    area.bottom() - 1,
                    hint,
                    usize::from(width),
                    muted,
                );
            }
        }
        let inner = inner(area);
        if inner.is_empty() {
            return;
        }
        let columns = columns_for(inner.width);
        let count = columns as u16;
        let column_width = inner.width.saturating_sub(COLUMN_GAP * (count - 1)) / count;
        let room = usize::from(inner.height);
        let laid = lay_out(rows(self.figures), room, columns);
        for (index, column) in laid.iter().enumerate() {
            let x = inner.x + (column_width + COLUMN_GAP) * index as u16;
            // The last column takes the cell the division left over, so
            // its figures end where the band's inside does.
            let values_end = if index + 1 == columns {
                inner.right()
            } else {
                x + column_width
            };
            for (line, row) in column.iter().enumerate() {
                let y = inner.y + line as u16;
                let (indent, label_style, value_style) = match row.kind {
                    RowKind::Title => (
                        0,
                        Style::default()
                            .fg(theme.accent)
                            .add_modifier(Modifier::BOLD),
                        plain.add_modifier(Modifier::BOLD),
                    ),
                    RowKind::Part => (0, plain, plain),
                    RowKind::Detail(_) => (2, muted, muted),
                    RowKind::Excess | RowKind::Outside => (0, muted, muted),
                    RowKind::Hint => (2, muted, muted),
                };
                let label_x = x + indent;
                // A line that is all label has the whole column; one with
                // a figure stops a cell short of it.
                let label_end = if row.value.is_empty() {
                    values_end
                } else {
                    let value_width = UnicodeWidthStr::width(row.value.as_str()) as u16;
                    let value_x = values_end.saturating_sub(value_width).max(label_x);
                    buffer.set_stringn(
                        value_x,
                        y,
                        &row.value,
                        usize::from(values_end.saturating_sub(value_x)),
                        value_style,
                    );
                    value_x.saturating_sub(1)
                };
                let label = super::view::ellipsize(&row.label, label_end.saturating_sub(label_x));
                buffer.set_stringn(
                    label_x,
                    y,
                    &label,
                    usize::from(label_end.saturating_sub(label_x)),
                    label_style,
                );
            }
        }
    }
}

// Where the header drew its process counters this frame, packed x/y/width
// like the jobs chip, so the pointer can ask without replicating the
// header's flowing layout. Zero means none are on screen.
//
// A running studio paints on one thread, so a plain static is that
// thread's own. Under `cfg(test)` it is literally thread-local, the way
// the graphics state is, so tests painting headers of different widths in
// parallel cannot move each other's chip.
#[cfg(not(test))]
static MEMORY_CHIP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(test)]
thread_local! {
    static MEMORY_CHIP: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(not(test))]
fn store_chip(packed: u64) {
    MEMORY_CHIP.store(packed, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(not(test))]
fn load_chip() -> u64 {
    MEMORY_CHIP.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
fn store_chip(packed: u64) {
    MEMORY_CHIP.with(|chip| chip.set(packed));
}

#[cfg(test)]
fn load_chip() -> u64 {
    MEMORY_CHIP.with(std::cell::Cell::get)
}

pub(super) fn set_memory_chip(rect: Option<(u16, u16, u16)>) {
    store_chip(rect.map_or(0, |(x, y, width)| {
        (u64::from(x) << 32) | (u64::from(y) << 16) | u64::from(width)
    }));
}

/// Where the header's counters are on screen, when they are.
pub fn memory_chip() -> Option<Rect> {
    let packed = load_chip();
    (packed != 0).then(|| {
        Rect::new(
            (packed >> 32) as u16,
            (packed >> 16) as u16,
            packed as u16,
            1,
        )
    })
}

/// Whether a pointer position is on the header's counters: the whole
/// `cpu … · mem … · fps` run, a bigger target than `mem` alone.
pub fn memory_chip_at(x: u16, y: u16) -> bool {
    memory_chip().is_some_and(|chip| chip.contains((x, y).into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: usize = 1024 * 1024;

    /// A studio holding a little of everything, in round figures.
    fn figures() -> MemoryFigures {
        MemoryFigures {
            total: Some(100 * MIB as u64),
            resident: Some(130 * MIB as u64),
            samples: Some(SampleMemory {
                live_bytes: 16 * MIB,
                recent_bytes: 6 * MIB,
                preview_bytes: 4 * MIB,
                preview_budget_bytes: 64 * MIB,
                unused_idle: Duration::from_secs(30),
                recent_limit_bytes: 256 * MIB,
            }),
            script_heap: Some(5 * MIB),
            audio: Some(AudioMemory {
                event_ring: 8 * MIB,
                backend: 17 * MIB,
                other: MIB,
                ..AudioMemory::default()
            }),
            visuals: None,
            screen: (120, 40),
            screen_bytes: MIB,
            pictures: 0,
            spectrograms: MIB,
            tabs_open: 2,
            tabs_closed: 3,
            tab_bytes: 2 * MIB,
            playing: true,
            returned_when_stopped: true,
            sample_cache: Some(300 * MIB as u64),
            fetch_imports: false,
            plugins: Vec::new(),
        }
    }

    fn row<'a>(rows: &'a [Row], label: &str) -> &'a Row {
        rows.iter()
            .find(|row| row.label.starts_with(label))
            .unwrap_or_else(|| panic!("no {label} row in {rows:#?}"))
    }

    #[test]
    fn short_log_keeps_memory_total_and_main_parts_first() {
        let figures = figures();
        assert_eq!(
            sidecar_rows(&figures, 2)
                .iter()
                .map(|row| row.label.as_str())
                .collect::<Vec<_>>(),
            ["memory", "sounds"]
        );
        assert_eq!(
            sidecar_rows(&figures, 4)
                .iter()
                .map(|row| row.label.as_str())
                .collect::<Vec<_>>(),
            ["memory", "sounds", "script engine", "not itemised"]
        );
        assert_eq!(sidecar_rows(&figures, 0).len(), 0);
    }

    /// The parts add up to the header's figure exactly, the remainder
    /// being defined as what they leave, and each part's details add up
    /// to it: the breakdown explains the number the header shows, not
    /// another.
    #[test]
    fn rows_add_up_to_the_header_figure() {
        let rows = rows(&figures());

        let parts: u64 = rows
            .iter()
            .filter(|row| row.kind == RowKind::Part)
            .map(|row| row.bytes.expect("every part is known"))
            .sum();
        assert_eq!(parts, 100 * MIB as u64);
        assert_eq!(row(&rows, "memory").value, "100MB");
        // 100 − 20 sounds − 5 script − 26 audio − 4 interface.
        assert_eq!(row(&rows, "not itemised").value, "45.0MB");
        for (part, detail) in [
            ("sounds", Detail::Sounds),
            ("audio output", Detail::Audio),
            ("interface", Detail::Interface),
        ] {
            let details: u64 = rows
                .iter()
                .filter(|row| row.kind == RowKind::Detail(detail))
                .filter_map(|row| row.bytes)
                .sum();
            assert_eq!(Some(details), row(&rows, part).bytes, "{part}");
        }
        assert_eq!(row(&rows, "kept").value, "10.0MB", "kept less recent");
        assert_eq!(row(&rows, "audio output").value, "≈ 26.0MB");
        assert!(
            !rows.iter().any(|row| row.label == "input"),
            "an input nobody opened is not a line: {rows:#?}"
        );
        assert_eq!(row(&rows, "RSS").label, "RSS 130MB · 30.0MB beyond mem");
        assert_eq!(
            row(&rows, "on disk").label,
            "on disk · sample cache 300MB · fetch imports off"
        );
        assert_eq!(
            rows.iter()
                .filter(|row| row.kind == RowKind::Hint)
                .map(|row| row.label.as_str())
                .collect::<Vec<_>>(),
            [
                "allocator-held freed memory, stacks, tables, code data",
                "freed memory returns when stopped"
            ]
        );
    }

    /// Kept samples have no total byte cap. Piano mode keeps no samples.
    #[test]
    fn the_kept_line_names_its_policy_and_sample_owners() {
        let rows = rows(&figures());

        assert_eq!(
            row(&rows, "kept").label,
            "kept · no byte cap · playing, panes, pads, setup, launch"
        );
    }

    /// Hydra's share is a part of its own, after the audio output: each
    /// open renderer and the waiting inputs under it, adding up to it,
    /// and the remainder shrinks by exactly as much. It says it is an
    /// estimate, and that the driver's own memory is left in the
    /// remainder.
    #[test]
    fn the_visuals_are_a_part_and_the_remainder_is_what_they_leave() {
        let visuals = VisualsMemory {
            renderers: vec![
                VisualRenderer {
                    picture: "theme",
                    size: (1280, 720),
                    bytes: 30 * MIB as u64,
                },
                VisualRenderer {
                    picture: "shelf preview",
                    size: (1280, 1280),
                    bytes: 8 * MIB as u64,
                },
            ],
            inputs: 2 * MIB as u64,
            gpu: 0,
        };
        let rows = rows(&MemoryFigures {
            visuals: Some(visuals),
            ..figures()
        });

        let parts: Vec<&str> = rows
            .iter()
            .filter(|row| row.kind == RowKind::Part)
            .map(|row| row.label.as_str())
            .collect();
        assert_eq!(
            parts,
            [
                "sounds",
                "script engine",
                "audio output",
                "visuals",
                "interface",
                "not itemised"
            ]
        );
        let sum: u64 = rows
            .iter()
            .filter(|row| row.kind == RowKind::Part)
            .filter_map(|row| row.bytes)
            .sum();
        assert_eq!(sum, 100 * MIB as u64, "still exactly the figure");
        assert_eq!(row(&rows, "visuals").value, "≈ 40.0MB");
        // 45 not itemised without them, less their 40.
        assert_eq!(row(&rows, "not itemised").value, "5.00MB");
        let details: u64 = rows
            .iter()
            .filter(|row| row.kind == RowKind::Detail(Detail::Visuals))
            .filter_map(|row| row.bytes)
            .sum();
        assert_eq!(Some(details), row(&rows, "visuals").bytes);
        assert_eq!(
            row(&rows, "theme renderer").label,
            "theme renderer · 1280×720"
        );
        assert_eq!(row(&rows, "shelf preview").value, "8.00MB");
        assert_eq!(row(&rows, "camera and image").value, "2.00MB");
        let hints: Vec<&str> = rows
            .iter()
            .filter(|row| row.kind == RowKind::Hint)
            .map(|row| row.label.as_str())
            .collect();
        assert_eq!(hints[0], "graphics driver's own memory is not itemised");
        assert!(
            !hints.iter().any(|hint| hint.contains("GPU memory")),
            "nothing is held outside the figure on this adapter: {hints:?}"
        );

        // A short band gives up the visuals' details with the interface's
        // and the audio's, before the sounds'.
        let kinds: Vec<RowKind> = fit(rows.clone(), 11)
            .into_iter()
            .map(|row| row.kind)
            .collect();
        assert!(
            !kinds.contains(&RowKind::Detail(Detail::Visuals)),
            "{kinds:?}"
        );
        assert!(
            kinds.contains(&RowKind::Detail(Detail::Sounds)),
            "{kinds:?}"
        );
        assert!(kinds.contains(&RowKind::Part));

        let without = super::rows(&figures());
        assert!(
            !without.iter().any(|row| row.label == "visuals"),
            "no line while Hydra holds nothing, or is not built in"
        );
    }

    /// A GPU's driver keeps the textures outside the process's figure - a
    /// discrete card's and, on Linux and Windows, an integrated one's: they
    /// are said beside the visuals and never added to the figure, which
    /// only the readback buffers count toward.
    #[test]
    fn textures_in_gpu_memory_are_said_but_not_added() {
        let rows = rows(&MemoryFigures {
            visuals: Some(VisualsMemory {
                renderers: vec![VisualRenderer {
                    picture: "score",
                    size: (1280, 720),
                    bytes: 4 * MIB as u64,
                }],
                inputs: 0,
                gpu: 36 * MIB as u64,
            }),
            ..figures()
        });

        assert_eq!(row(&rows, "visuals").value, "≈ 4.00MB");
        assert_eq!(row(&rows, "not itemised").value, "41.0MB");
        assert!(
            !rows.iter().any(|row| row.label.starts_with("camera")),
            "no inputs, no line"
        );
        let gpu = row(&rows, "+ 36.0MB");
        assert_eq!(gpu.label, "+ 36.0MB of textures in GPU memory, outside mem");
        assert_eq!((gpu.kind, gpu.bytes), (RowKind::Hint, None));
    }

    /// What Hydra reports becomes the breakdown's lines: a software
    /// adapter's written textures are the process's, a GPU's are memory
    /// beside it at the size allocated, and a host holding nothing is no
    /// line.
    #[cfg(feature = "hydra")]
    #[test]
    fn hydra_readings_become_the_visuals_lines() {
        use rustel_hydra::native::NativeFootprint;
        let footprint = |in_process| NativeFootprint {
            width: 1280,
            height: 720,
            textures: 10 * MIB,
            resident: 5 * MIB,
            readback: MIB,
            textures_in_process: in_process,
        };
        assert_eq!(
            VisualsMemory::from_hydra(rustel_hydra::HydraMemory::default()),
            None
        );
        let visuals = VisualsMemory::from_hydra(rustel_hydra::HydraMemory {
            score: Some(footprint(false)),
            theme: Some(footprint(true)),
            inputs: 3,
            ..rustel_hydra::HydraMemory::default()
        })
        .expect("something is held");
        assert_eq!(
            visuals.renderers,
            [
                VisualRenderer {
                    picture: "score",
                    size: (1280, 720),
                    bytes: MIB as u64,
                },
                VisualRenderer {
                    picture: "theme",
                    size: (1280, 720),
                    bytes: 6 * MIB as u64,
                },
            ]
        );
        assert_eq!((visuals.inputs, visuals.gpu), (3, 10 * MIB as u64));
        assert_eq!(visuals.total(), 7 * MIB as u64 + 3);
    }

    /// Where the heap hands freed memory back as it goes, Stop gives back
    /// nothing more, and the breakdown does not say it will.
    #[test]
    fn no_stop_hint_where_freed_memory_returns_at_once() {
        let rows = rows(&MemoryFigures {
            returned_when_stopped: false,
            ..figures()
        });

        assert_eq!(
            rows.iter()
                .filter(|row| row.kind == RowKind::Hint)
                .map(|row| row.label.as_str())
                .collect::<Vec<_>>(),
            ["allocator-held freed memory, stacks, tables, code data"]
        );
    }

    /// The parts and the figure are read at different moments, and swap
    /// or compression can take pages the parts still count. The breakdown
    /// says the parts come to more than the figure; it never prints a
    /// negative remainder.
    #[test]
    fn over_itemised_says_so_rather_than_going_negative() {
        let rows = rows(&MemoryFigures {
            total: Some(30 * MIB as u64),
            ..figures()
        });

        assert!(!rows.iter().any(|row| row.label == "not itemised"));
        let excess = row(&rows, "items exceed the figure by");
        assert_eq!(excess.kind, RowKind::Excess);
        // 20 + 5 + 26 + 4 = 55 against 30.
        assert_eq!(excess.value, "25.0MB");
        assert!(!rows.iter().any(|row| row.value.starts_with('-')));
        assert_eq!(row(&rows, "RSS").label, "RSS 130MB · 100MB beyond mem");
    }

    /// Previews are shown against the limits the engine applies, in the
    /// settings' own words when there is none.
    #[test]
    fn preview_limit_reads_no_cap_and_never() {
        let limited = rows(&figures());
        assert_eq!(
            row(&limited, "previews").label,
            "previews · cap 64.0MB · drop after 30 s"
        );
        assert_eq!(
            row(&limited, "recent tabs").label,
            "recent tabs · up to 256MB"
        );

        let mut unlimited = figures();
        let samples = unlimited.samples.as_mut().unwrap();
        samples.preview_budget_bytes = 0;
        samples.unused_idle = Duration::ZERO;
        let unlimited = rows(&unlimited);
        assert_eq!(
            row(&unlimited, "previews").label,
            "previews · no cap · never dropped"
        );

        let mut slow = figures();
        slow.samples.as_mut().unwrap().unused_idle = Duration::from_secs(300);
        assert_eq!(
            row(&rows(&slow), "previews").label,
            "previews · cap 64.0MB · drop after 5 min"
        );
    }

    /// Before the engine's first snapshot its parts read as dashes and
    /// the remainder counts only what is known: the figure less the
    /// interface, with no audio line because nothing reported one.
    #[test]
    fn missing_snapshot_shows_dashes() {
        let rows = rows(&MemoryFigures {
            samples: None,
            script_heap: None,
            audio: None,
            ..figures()
        });

        assert_eq!(row(&rows, "sounds").value, "-");
        assert_eq!(row(&rows, "script engine").value, "-");
        assert!(
            !rows
                .iter()
                .any(|row| row.kind == RowKind::Detail(Detail::Sounds))
        );
        assert!(!rows.iter().any(|row| row.label == "audio output"));
        assert_eq!(row(&rows, "not itemised").value, "96.0MB");

        let unknown = super::rows(&MemoryFigures {
            total: None,
            resident: None,
            ..figures()
        });
        assert_eq!(row(&unknown, "memory").value, "-");
        assert_eq!(row(&unknown, "not itemised").value, "-");
        assert_eq!(row(&unknown, "RSS").label, "RSS -");
    }

    /// A short terminal loses the explanations before the figures: the
    /// hints, then the lines outside the figure, then the interface's and
    /// the audio's details, then the sounds'. The parts always stay.
    #[test]
    fn short_height_drops_detail_rows_first() {
        let all = rows(&figures());
        let hints = all.iter().filter(|row| row.kind == RowKind::Hint).count();
        let kinds = |room| {
            fit(all.clone(), room)
                .into_iter()
                .map(|row| row.kind)
                .collect::<Vec<_>>()
        };

        assert_eq!(kinds(all.len()).len(), all.len(), "all of it fits");
        let without_hints = kinds(all.len() - 1);
        assert_eq!(without_hints.len(), all.len() - hints);
        assert!(!without_hints.contains(&RowKind::Hint));
        assert!(without_hints.contains(&RowKind::Outside));

        let parts_and_sounds = kinds(10);
        assert!(!parts_and_sounds.contains(&RowKind::Outside));
        assert!(!parts_and_sounds.contains(&RowKind::Detail(Detail::Interface)));
        assert!(!parts_and_sounds.contains(&RowKind::Detail(Detail::Audio)));
        assert!(parts_and_sounds.contains(&RowKind::Detail(Detail::Sounds)));

        let bare = kinds(6);
        assert_eq!(
            bare,
            [
                RowKind::Title,
                RowKind::Part,
                RowKind::Part,
                RowKind::Part,
                RowKind::Part,
                RowKind::Part
            ],
            "the figure, sounds, script, audio, interface and the remainder"
        );
    }

    /// The chip the header records is the one the pointer finds, and a
    /// frame without counters leaves nothing armed.
    #[test]
    fn the_chip_answers_where_it_was_recorded() {
        set_memory_chip(Some((60, 1, 35)));
        assert_eq!(memory_chip(), Some(Rect::new(60, 1, 35, 1)));
        assert!(memory_chip_at(60, 1) && memory_chip_at(94, 1));
        assert!(!memory_chip_at(59, 1) && !memory_chip_at(95, 1) && !memory_chip_at(70, 2));
        set_memory_chip(None);
        assert_eq!(memory_chip(), None);
        assert!(!memory_chip_at(60, 1));
    }

    /// A band's inside lays its rows in as many columns as fit at fifty
    /// cells: one on a narrow terminal, two on the usual one, three on a
    /// wide one, and never more.
    #[test]
    fn a_wider_band_lays_more_columns_side_by_side() {
        assert_eq!(columns_for(40), 1);
        assert_eq!(columns_for(102), 1);
        assert_eq!(columns_for(103), 2, "two of fifty and the gap");
        assert_eq!(columns_for(116), 2);
        assert_eq!(columns_for(156), 3);
        assert_eq!(columns_for(400), 3);
    }

    /// The columns split between groups, so a part keeps its details, and
    /// the tallest column is as short as it can be.
    #[test]
    fn columns_split_between_parts_as_evenly_as_they_can() {
        let laid = balance(rows(&figures()), 2);
        let labels = |column: &[Row]| {
            column
                .iter()
                .map(|row| row.label.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(laid.len(), 2);
        let (left, right) = (labels(&laid[0]), labels(&laid[1]));
        assert_eq!(left[0], "memory");
        assert!(
            left.iter().any(|label| label == "script engine"),
            "{left:?}"
        );
        assert_eq!(left.last().unwrap(), "other", "the audio's last detail");
        assert_eq!(right[0], "interface");
        assert!(right.last().unwrap().starts_with("on disk"), "{right:?}");
        for column in &laid {
            assert!(
                !matches!(column[0].kind, RowKind::Detail(_) | RowKind::Hint),
                "no column starts with another's detail: {column:#?}"
            );
        }
        // 1 + 4 + 1 + 4 on the left, 5 + 3 + 2 on the right.
        assert_eq!((laid[0].len(), laid[1].len()), (10, 10));

        let one = balance(rows(&figures()), 1);
        assert_eq!(one.len(), 1, "one column is all of it");
        assert_eq!(one[0].len(), rows(&figures()).len());
    }

    /// A band shorter than its columns loses the explanations first, the
    /// way a short popup did - hints, then the lines outside the figure,
    /// then the details - and the parts always stay.
    #[test]
    fn a_short_band_drops_details_before_parts() {
        let all = rows(&figures());
        let kinds = |laid: &[Vec<Row>]| {
            laid.iter()
                .flatten()
                .map(|row| row.kind)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            kinds(&lay_out(all.clone(), 10, 2)).len(),
            all.len(),
            "it fits"
        );
        let short = kinds(&lay_out(all.clone(), 8, 2));
        assert!(!short.contains(&RowKind::Hint), "{short:?}");
        assert!(short.contains(&RowKind::Detail(Detail::Sounds)));
        let bare = lay_out(all.clone(), 3, 2);
        assert!(bare.iter().all(|column| column.len() <= 3), "{bare:#?}");
        assert!(
            kinds(&bare)
                .iter()
                .all(|kind| matches!(kind, RowKind::Title | RowKind::Part)),
            "{bare:#?}"
        );
        // One column is `fit`, cut at the band's foot.
        assert_eq!(lay_out(all.clone(), 6, 1)[0], fit(all, 6));
    }

    /// Until it is resized the dock is as tall as the breakdown needs in
    /// the columns the terminal's width allows, with an output open,
    /// border and all; a resize holds within a band's bounds.
    #[test]
    fn the_default_height_is_what_the_columns_need() {
        assert_eq!(default_height(120), 12, "two columns of ten");
        assert_eq!(default_height(200), 11, "three columns of nine");
        assert_eq!(default_height(80), 21, "one column of nineteen");
        let mut dock = MemoryDock {
            edge: Edge::Bottom,
            height: None,
        };
        assert_eq!(dock.height_on(120), 12);
        dock.height = Some(99);
        assert_eq!(dock.height_on(120), BAND_MAX_HEIGHT);
        dock.edge = Edge::Left;
        assert_eq!(
            dock.dock(Rect::new(0, 0, 120, 40)).edge,
            Edge::Bottom,
            "never a column"
        );
    }

    /// Drawn in its band: the title and the close mark on the top row,
    /// the rows in two columns, each column's values standing in one
    /// column at its right, and what its keys do on the bottom row while
    /// it has them - Esc closing it where, in zen, it does.
    #[test]
    fn the_band_draws_its_columns_under_a_title_and_a_close_mark() {
        let theme = Theme::built_in_default();
        let figures = figures();
        let area = Rect::new(0, 0, 120, 12);
        let draw_with = |focused: bool, closes_on_esc: bool| {
            let mut buffer = Buffer::empty(area);
            MemoryView {
                figures: &figures,
                theme: &theme,
                focused,
                closes_on_esc,
            }
            .render(area, &mut buffer);
            (0..area.height)
                .map(|y| {
                    (0..area.width)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
        };
        let draw = |focused: bool| draw_with(focused, false);
        let lines = draw(false);
        assert!(lines[0].contains(" memory breakdown "), "{}", lines[0]);
        assert!(lines[0].ends_with(" × ╮"), "{}", lines[0]);
        assert!(close_at(area, area.right() - 3, 0));
        assert!(!close_at(area, area.right() - 3, 1));
        assert!(!close_at(area, 4, 0));
        let title = &lines[1];
        assert!(title.starts_with("│ memory"), "{title}");
        assert!(title.contains("100MB   interface"), "{title}");
        assert!(title.ends_with("4.00MB │"), "{title}");
        let all = lines.join("\n");
        assert!(all.contains("allocator-held freed memory"), "{all}");
        assert!(all.contains("fetch imports off"), "{all}");
        assert!(!lines[11].contains("Esc"), "no keys while it has none");
        assert!(draw(true)[11].contains("e top/bottom · -/+ height · Esc to the score"));
        // In zen Esc puts it away, and the hint says that instead; `e` and
        // `-`/`+` still answer there.
        let zen = &draw_with(true, true)[11];
        assert!(
            zen.contains("e top/bottom · -/+ height · Esc closes"),
            "{zen}"
        );
        assert!(!zen.contains("to the score"), "{zen}");
    }
}
