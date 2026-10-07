//! A recorded set opened to replay: the tape's saves along the top of the
//! editor, left to right on the clock they were made, one of them in the
//! editor to read, edit and play.
//!
//! A block is one save that installed. Choosing one puts its code in the
//! editor; ^S plays what the editor holds and starts the run from there -
//! when the block's recorded time is up the next one loads and plays, and
//! so on to the end, with a countdown on the block that is sounding. The
//! run stops with ^G, or when the tape runs out. The model here knows the
//! tape, the blocks, which one is chosen, the run and the scroll; the app
//! owns the editor, the engine and the clock that ticks the run along.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Widget;

use super::editor::{DEFAULT_MAX_DOCUMENT_BYTES, Document};
use super::minimap::{Minimap, MinimapView};
use super::theme::Theme;
use rustel_runtime::session_log::{STOP_SOURCE, SaveStatus, SessionMode, SessionScript};

/// Rows the timeline takes off the top of the editor: a header, three rows
/// of thumbnail, the ruler, and a scrollbar.
pub const TIMELINE_HEIGHT: u16 = 6;
/// A block's width in cells, gap included.
pub const BLOCK_WIDTH: u16 = 12;
const BLOCK_GAP: u16 = 1;
const THUMBNAIL_ROWS: u16 = 3;

/// Take the timeline's rows off the top of a pane's editor: the strip and
/// what is left for the text. `None` when the pane is too short for both,
/// and the tab is then just an editor.
pub fn split_timeline(editor: Rect) -> Option<(Rect, Rect)> {
    let shortcuts = shortcut_height(editor.width);
    if editor.height < TIMELINE_HEIGHT + shortcuts + 3 || editor.width < BLOCK_WIDTH {
        return None;
    }
    let strip = Rect::new(editor.x, editor.y, editor.width, TIMELINE_HEIGHT);
    let rest = Rect::new(
        editor.x,
        editor.y + TIMELINE_HEIGHT,
        editor.width,
        editor.height - TIMELINE_HEIGHT - shortcuts,
    );
    Some((strip, rest))
}

/// Keep one horizontal shortcut bar below the editor, wrapping to two
/// rows in a narrow pane. Both the hit map and rendering reserve it.
fn shortcut_height(width: u16) -> u16 {
    if width < 100 { 2 } else { 1 }
}

pub fn shortcut_area(editor: Rect) -> Option<Rect> {
    split_timeline(editor)?;
    let height = shortcut_height(editor.width);
    Some(Rect::new(
        editor.x,
        editor.bottom() - height,
        editor.width,
        height,
    ))
}

pub struct TimelineShortcuts<'a> {
    pub focused: bool,
    pub theme: &'a Theme,
}

impl Widget for TimelineShortcuts<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        super::view::clear_surface(
            buffer,
            area,
            Style::default().bg(self.theme.surface).fg(self.theme.muted),
        );
        let segments = if self.focused {
            [
                "Esc editor",
                "←/→ blocks",
                "Enter play",
                "T duration",
                "Del delete",
                "Home/End ends",
                "Alt+R rename",
                "Alt+O reveal",
            ]
        } else {
            [
                "Alt+T timeline",
                "Alt+←/→ blocks",
                "Alt+R rename",
                "Alt+O reveal",
                "",
                "",
                "",
                "",
            ]
        };
        let (mut x, mut y) = (area.x, area.y);
        for segment in segments.into_iter().filter(|text| !text.is_empty()) {
            let segment = super::keybinds::shortcut_label(segment);
            let segment = super::terminal::safe_text(&segment);
            let width = unicode_width::UnicodeWidthStr::width(segment.as_ref()) as u16;
            let gap = if x > area.x { 3 } else { 0 };
            if x + gap + width > area.right() && x > area.x {
                x = area.x;
                y += 1;
            }
            if y >= area.bottom() {
                break;
            }
            if x > area.x {
                buffer.set_stringn(
                    x,
                    y,
                    " · ",
                    usize::from(area.right() - x),
                    Style::default().fg(self.theme.rule),
                );
                x = (x + 3).min(area.right());
            }
            buffer.set_stringn(
                x,
                y,
                segment.as_ref(),
                usize::from(area.right() - x),
                Style::default().fg(self.theme.muted),
            );
            x = (x + width).min(area.right());
        }
    }
}

/// One save of the tape, as a block.
#[derive(Clone, Debug)]
pub struct ReplayEvent {
    /// Seconds into the tape.
    pub at: f64,
    /// The code: the tape's, or what the editor made of it.
    pub source: Arc<str>,
    /// What made the save when no keystroke did: `slider`, `stop`, `replay`.
    pub via: Option<String>,
    /// The editor changed it since the tape was read.
    pub edited: bool,
    document: Document,
    minimap: Minimap,
}

impl ReplayEvent {
    fn new(at: f64, source: Arc<str>, via: Option<String>) -> Result<Self, String> {
        let document = Document::new(&source, DEFAULT_MAX_DOCUMENT_BYTES)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            at,
            source,
            via,
            edited: false,
            document,
            minimap: Minimap::default(),
        })
    }
}

/// A save the tape keeps but the timeline does not show: a stop, or what
/// an earlier replay's run made. Neither is a scene the artist wrote. It
/// rides with the block it came after, so an edit to that block's length
/// moves it too, and never past the next block.
#[derive(Clone, Debug)]
pub struct Aside {
    /// The block it came after; nought for one before the first.
    pub after: usize,
    /// Seconds after that block's start.
    pub offset: f64,
    pub save: rustel_runtime::session_log::SessionSave,
}

/// The run: which block is sounding, and when the next is due.
#[derive(Clone, Copy, Debug)]
pub struct ReplayRun {
    pub event: usize,
    pub started: Instant,
    /// When the next block or explicit tape endpoint is due.
    /// `None` on an indefinite last block.
    pub next_due: Option<Instant>,
}

/// The shortest a block can be made: a boundary dragged past the one
/// before it stops here rather than crossing it.
pub const MIN_BLOCK_SECONDS: f64 = 0.1;

#[derive(Clone, Debug)]
pub struct ReplayTab {
    /// The tape.
    pub path: PathBuf,
    /// When the tape was started, as its header says.
    pub recorded: Option<String>,
    /// What the tape kept, as its header says.
    pub keeps: String,
    pub baseline_cps: Option<f64>,
    /// The saves that sounded, as blocks.
    pub events: Vec<ReplayEvent>,
    /// The saves kept off the timeline, written back with the rest.
    pub aside: Vec<Aside>,
    /// A trailing recorded stop bounds the final block. Legacy tapes with
    /// no such stop remain indefinite; no duration is guessed for them.
    end_at: Option<f64>,
    /// Something changed since the tape was read or written.
    pub dirty: bool,
    /// The block in the editor.
    pub selected: usize,
    /// The first block on screen.
    pub scroll: usize,
    pub run: Option<ReplayRun>,
}

impl ReplayTab {
    /// Read a tape. Only the saves that installed are blocks: a rejected
    /// save never sounded, and there is nothing to replay of it. A stop,
    /// and what an earlier replay made, stay on the tape but off the
    /// timeline: a run plays each block until the next one the artist
    /// wrote.
    pub fn open(path: &Path) -> Result<Self, String> {
        let script = SessionScript::load_for_editing(path)?;
        let (events, mut aside) = events_of(&script)?;
        let end_at = take_endpoint(&events, &mut aside);
        Ok(Self {
            path: path.to_path_buf(),
            recorded: script.recorded,
            keeps: script.keeps,
            baseline_cps: script.baseline_cps,
            events,
            aside,
            end_at,
            dirty: false,
            selected: 0,
            scroll: 0,
            run: None,
        })
    }

    /// Whether the tape can take an edit back: a normal tape can, a debug
    /// tape cannot - writing it back would drop the diagnostics it exists
    /// to keep.
    pub fn writable(&self) -> bool {
        self.keeps == SessionMode::Normal.keeps()
    }

    /// The tape as it reads now, edits and all: the blocks, and the saves
    /// kept aside put back after the blocks they followed.
    fn script(&self) -> SessionScript {
        let mut saves: Vec<(usize, rustel_runtime::session_log::SessionSave)> = self
            .events
            .iter()
            .enumerate()
            .map(|(index, event)| {
                (
                    index,
                    rustel_runtime::session_log::SessionSave {
                        at: event.at,
                        status: SaveStatus::Installed.as_str().to_owned(),
                        source: event.source.clone(),
                        error: None,
                        via: event.via.clone(),
                    },
                )
            })
            .collect();
        for aside in &self.aside {
            let Some(block) = self.events.get(aside.after) else {
                continue;
            };
            let limit = self
                .events
                .get(aside.after + 1)
                .map(|next| next.at)
                .or(self.end_at)
                .map(|end| end - block.at);
            let offset = limit
                .map_or(aside.offset, |limit| aside.offset.min(limit))
                .max(0.0);
            let mut save = aside.save.clone();
            save.at = block.at + offset;
            saves.push((aside.after, save));
        }
        if let Some(at) = self.end_at.filter(|_| !self.events.is_empty()) {
            // A stop is already understood by readers of this format and
            // remains hidden on the timeline. It also makes the edited
            // endpoint audible when the tape is played outside Studio.
            saves.push((
                self.events.len() - 1,
                rustel_runtime::session_log::SessionSave {
                    at,
                    status: SaveStatus::Installed.as_str().into(),
                    source: STOP_SOURCE.into(),
                    error: None,
                    via: Some("stop".into()),
                },
            ));
        }
        // Stable within each block. An old aside clamped to the following
        // block's start must still precede that block, or it acquires the
        // wrong owner (and can look like a final stop) when reopened.
        saves.sort_by(|(owner_a, a), (owner_b, b)| {
            a.at.partial_cmp(&b.at)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| owner_a.cmp(owner_b))
        });
        SessionScript {
            keeps: self.keeps.clone(),
            recorded: self.recorded.clone(),
            baseline_cps: self.baseline_cps,
            saves: saves.into_iter().map(|(_, save)| save).collect(),
            logs: 0,
        }
    }

    /// Write the tape back with the edits: the blocks' code and times.
    pub fn save(&mut self) -> Result<(), String> {
        if !self.writable() {
            return Err(format!(
                "{} is a debug tape; its diagnostics would be lost",
                self.path.display()
            ));
        }
        self.script().write(&self.path)?;
        self.dirty = false;
        for event in &mut self.events {
            event.edited = false;
        }
        Ok(())
    }

    /// How long a block plays: until the next block or an explicit final
    /// stop. A legacy last block without a stop remains indefinite.
    pub fn duration_of(&self, index: usize) -> Option<f64> {
        let this = self.events.get(index)?.at;
        let next = self
            .events
            .get(index + 1)
            .map(|next| next.at)
            .or(self.end_at)?;
        Some(next - this)
    }

    /// Give a block a new length. Every block after it moves by the
    /// difference, so the rest of the tape keeps its own timing.
    pub fn set_duration(&mut self, index: usize, seconds: f64) {
        if seconds.is_finite() {
            let _ = self.set_duration_exact(index, seconds.max(MIN_BLOCK_SECONDS));
        }
    }

    /// Set exact seconds without the pointer drag's minimum-size clamp.
    /// A caller can edit a clone and save it before replacing the live tab.
    pub fn set_duration_exact(&mut self, index: usize, seconds: f64) -> Result<bool, String> {
        if !self.writable() {
            return Err("a debug tape keeps its original timing and diagnostics".into());
        }
        if !seconds.is_finite() || seconds <= 0.0 || seconds > 86_400.0 {
            return Err("duration must be greater than zero and at most 86400 seconds".into());
        }
        let start = self
            .events
            .get(index)
            .ok_or("there is no block to resize")?
            .at;
        let current = self.duration_of(index);
        if current == Some(seconds) {
            return Ok(false);
        }
        let delta = current.map_or(0.0, |current| seconds - current);
        let new_end = if index + 1 == self.events.len() {
            Some(start + seconds)
        } else {
            self.end_at.map(|end| end + delta)
        };
        if !start.is_finite()
            || new_end.is_some_and(|end| !end.is_finite())
            || self
                .events
                .iter()
                .skip(index + 1)
                .any(|event| !(event.at + delta).is_finite())
        {
            return Err("the tape contains a timestamp that cannot be resized".into());
        }
        for event in self.events.iter_mut().skip(index + 1) {
            event.at += delta;
        }
        self.end_at = new_end;
        if let Some(run) = self.run {
            let next_due = self.next_due_after(run.event, run.started);
            self.run = Some(ReplayRun { next_due, ..run });
        }
        self.dirty = true;
        Ok(true)
    }

    /// Remove one block and its attached hidden saves. Later blocks move
    /// earlier by its duration; all surviving block lengths remain intact.
    pub fn delete_event(&mut self, index: usize) -> Result<bool, String> {
        if !self.writable() {
            return Err("a debug tape keeps its original blocks and diagnostics".into());
        }
        let Some(removed) = self.events.get(index) else {
            return Ok(false);
        };
        let start = removed.at;
        let duration = self.duration_of(index).unwrap_or(0.0);
        if !start.is_finite() || !duration.is_finite() || duration < 0.0 {
            return Err("the tape contains a timestamp that cannot be deleted".into());
        }
        let was_last = index + 1 == self.events.len();
        self.events.remove(index);
        for event in self.events.iter_mut().skip(index) {
            event.at -= duration;
        }
        self.aside.retain(|aside| aside.after != index);
        for aside in &mut self.aside {
            if aside.after > index {
                aside.after -= 1;
            }
        }
        self.end_at = if self.events.is_empty() {
            self.aside.clear();
            None
        } else if was_last {
            Some(start)
        } else {
            self.end_at.map(|end| end - duration)
        };
        self.selected = self
            .selected
            .saturating_sub(usize::from(self.selected > index))
            .min(self.events.len().saturating_sub(1));
        self.scroll = self
            .scroll
            .saturating_sub(usize::from(self.scroll > index))
            .min(self.events.len().saturating_sub(1));
        self.run = None;
        self.dirty = true;
        Ok(true)
    }

    /// Read the tape again - it is still being written - keeping the
    /// selection, the edits and the run by position.
    pub fn reload(&mut self) -> Result<(), String> {
        let script = SessionScript::load_for_editing(&self.path)?;
        let (mut events, mut aside) = events_of(&script)?;
        let end_at = take_endpoint(&events, &mut aside);
        for (index, event) in events.iter_mut().enumerate() {
            if let Some(old) = self.events.get(index)
                && old.edited
            {
                event.source = old.source.clone();
                event.edited = true;
                event.document = old.document.clone();
                event.minimap = old.minimap.clone();
            }
        }
        self.events = events;
        self.aside = aside;
        self.end_at = end_at;
        self.selected = self.selected.min(self.events.len().saturating_sub(1));
        self.scroll = self.scroll.min(self.events.len().saturating_sub(1));
        if let Some(run) = self.run {
            self.run = self.events.get(run.event).map(|_| ReplayRun {
                next_due: self.next_due_after(run.event, run.started),
                ..run
            });
        }
        Ok(())
    }

    /// When the first block was recorded. A tape's times are counted from
    /// the moment the recorder opened, which is when the studio did - so a
    /// set whose first evaluate came a quarter of an hour in starts at
    /// 15:00 and nowhere near zero. The timeline reads from the first
    /// block instead; the tape on disk keeps its own clock.
    pub fn origin(&self) -> f64 {
        self.events.first().map(|event| event.at).unwrap_or(0.0)
    }

    /// A block's place on the timeline: seconds from the first block.
    pub fn since_origin(&self, index: usize) -> f64 {
        self.events
            .get(index)
            .map(|event| event.at - self.origin())
            .unwrap_or(0.0)
    }

    /// The tape's length: its last block, from the first.
    pub fn duration(&self) -> f64 {
        self.end_at
            .or_else(|| self.events.last().map(|event| event.at))
            .map(|end| end - self.origin())
            .unwrap_or(0.0)
    }

    pub fn selected_source(&self) -> &str {
        self.events
            .get(self.selected)
            .map(|event| &*event.source)
            .unwrap_or("")
    }

    /// The editor's text becomes the block's: an edit the run will play.
    pub fn set_source(&mut self, index: usize, text: &str) -> Result<(), String> {
        if self.events.is_empty() && index == 0 {
            let mut event = ReplayEvent::new(0.0, text.into(), None)?;
            event.edited = true;
            self.aside.clear();
            self.end_at = None;
            self.events.push(event);
            self.dirty = true;
            return Ok(());
        }
        let Some(event) = self.events.get_mut(index) else {
            return Ok(());
        };
        if &*event.source == text {
            return Ok(());
        }
        event.source = text.into();
        event.edited = true;
        event.document =
            Document::new(text, DEFAULT_MAX_DOCUMENT_BYTES).map_err(|error| error.to_string())?;
        event.minimap = Minimap::default();
        self.dirty = true;
        Ok(())
    }

    /// Choose a block. Returns true when it changed.
    pub fn select(&mut self, index: usize) -> bool {
        if index >= self.events.len() || index == self.selected {
            return false;
        }
        self.selected = index;
        true
    }

    /// Start the run at `from`, now.
    pub fn start(&mut self, from: usize, now: Instant) {
        if self.events.is_empty() {
            self.run = None;
            return;
        }
        let from = from.min(self.events.len().saturating_sub(1));
        self.selected = from;
        self.run = Some(ReplayRun {
            event: from,
            started: now,
            next_due: self.next_due_after(from, now),
        });
    }

    pub fn stop(&mut self) {
        self.run = None;
    }

    fn next_due_after(&self, index: usize, from: Instant) -> Option<Instant> {
        let gap = self.duration_of(index)?;
        // A tape is meant to be handed to someone else, so a time in it may
        // be anything at all. `from_secs_f64` panics on a value that is not
        // finite or is past `u64::MAX` seconds, and a day is far beyond any
        // gap between two blocks a set actually made.
        let gap = if gap.is_finite() {
            gap.clamp(0.0, 86_400.0)
        } else {
            0.0
        };
        Some(from + Duration::from_secs_f64(gap))
    }

    /// Whether the next block is due at `now`.
    pub fn due(&self, now: Instant) -> bool {
        self.run
            .and_then(|run| run.next_due)
            .is_some_and(|due| now >= due)
    }

    /// Move the run to the next block and answer with its index - or end
    /// the run on the last block. The selection follows only while it is
    /// on the block that was sounding: a block chosen to be read stays
    /// chosen, and the run plays on beside it.
    pub fn advance(&mut self, now: Instant) -> Option<usize> {
        let run = self.run?;
        let next = run.event + 1;
        if next >= self.events.len() {
            self.run = None;
            return None;
        }
        if self.selected == run.event {
            self.selected = next;
        }
        self.run = Some(ReplayRun {
            event: next,
            started: now,
            next_due: self.next_due_after(next, now),
        });
        Some(next)
    }

    /// Seconds until the next block, while a run is on and there is one.
    pub fn countdown(&self, now: Instant) -> Option<f64> {
        let due = self.run?.next_due?;
        Some(due.saturating_duration_since(now).as_secs_f64())
    }

    /// The block the run is sounding, if any.
    pub fn playing(&self) -> Option<usize> {
        self.run.map(|run| run.event)
    }

    /// How many blocks fit across `width` cells.
    pub fn blocks_across(width: u16) -> usize {
        usize::from(width / BLOCK_WIDTH).max(1)
    }

    /// Scroll so the selection is on screen.
    pub fn ensure_visible(&mut self, width: u16) {
        let across = Self::blocks_across(width);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + across {
            self.scroll = self.selected + 1 - across;
        }
        self.scroll = self.scroll.min(self.events.len().saturating_sub(across));
    }

    /// Scroll to a point along the bar: nought is the first block, one the
    /// last screenful - a click or a drag on the scrollbar.
    pub fn scroll_to(&mut self, fraction: f64, width: u16) {
        let across = Self::blocks_across(width);
        let most = self.events.len().saturating_sub(across);
        self.scroll = ((fraction.clamp(0.0, 1.0) * most as f64).round() as usize).min(most);
    }

    /// The row of the strip the scrollbar is drawn on.
    pub fn scrollbar_row(area: Rect) -> u16 {
        area.y + TIMELINE_HEIGHT - 1
    }

    /// The row of the strip the blocks' times are drawn on: dragging a
    /// time there moves the boundary it marks.
    pub fn ruler_row(area: Rect) -> u16 {
        area.y + 1 + THUMBNAIL_ROWS
    }

    /// The scrollbar's track, thumb start and thumb length for a strip
    /// `width` wide, when the tape is longer than the strip: the track
    /// stops short of the total written at the row's right end.
    pub fn bar_geometry(&self, width: u16) -> Option<(u16, u16, u16)> {
        let across = Self::blocks_across(width);
        let count = self.events.len();
        if count <= across {
            return None;
        }
        let total_width = self.total_text().chars().count() as u16;
        let track = if width > total_width + 1 {
            width - total_width - 1
        } else {
            width
        };
        let thumb_len = (usize::from(track) * across / count).max(1) as u16;
        let thumb_x = (usize::from(track - thumb_len) * self.scroll / (count - across)) as u16;
        Some((track, thumb_x, thumb_len))
    }

    /// The total at the bar's right end: the tape's length and its saves.
    pub fn total_text(&self) -> String {
        format!("{} · {} saves", clock(self.duration()), self.events.len())
    }

    /// Scroll so the thumb starts `thumb_x` cells along the track: what a
    /// press on the bar or a drag of the thumb asks for.
    pub fn scroll_thumb_to(&mut self, thumb_x: u16, width: u16) {
        let Some((track, _, thumb_len)) = self.bar_geometry(width) else {
            return;
        };
        let across = Self::blocks_across(width);
        let most = self.events.len().saturating_sub(across);
        let span = usize::from(track.saturating_sub(thumb_len)).max(1);
        let along = usize::from(thumb_x.min(track.saturating_sub(thumb_len)));
        self.scroll = ((along * most + span / 2) / span).min(most);
    }

    pub fn scroll_by(&mut self, delta: isize, width: u16) {
        let across = Self::blocks_across(width);
        let most = self.events.len().saturating_sub(across);
        self.scroll = (self.scroll as isize + delta).clamp(0, most as isize) as usize;
    }

    /// The block under a pointer in the timeline `area`, if any.
    pub fn block_at(&self, area: Rect, x: u16, y: u16) -> Option<usize> {
        if !area.contains((x, y).into())
            || y >= Self::scrollbar_row(area)
            || (x - area.x) % BLOCK_WIDTH >= BLOCK_WIDTH - BLOCK_GAP
        {
            return None;
        }
        let column = usize::from((x - area.x) / BLOCK_WIDTH);
        if column >= Self::blocks_across(area.width) {
            return None;
        }
        let index = self.scroll + column;
        (index < self.events.len()).then_some(index)
    }

    /// The rect a block draws in, when it is on screen.
    fn block_rect(&self, area: Rect, index: usize) -> Option<Rect> {
        let column = index.checked_sub(self.scroll)?;
        let x = area
            .x
            .checked_add(u16::try_from(column).ok()? * BLOCK_WIDTH)?;
        if x + BLOCK_WIDTH > area.right() {
            return None;
        }
        Some(Rect::new(x, area.y, BLOCK_WIDTH - BLOCK_GAP, area.height))
    }

    /// Thumbnails for the blocks on screen, at the size they draw at.
    pub fn sync_thumbnails(&mut self, area: Rect) {
        let across = Self::blocks_across(area.width);
        let thumb = Rect::new(0, 0, BLOCK_WIDTH - BLOCK_GAP, THUMBNAIL_ROWS);
        for event in self.events.iter_mut().skip(self.scroll).take(across) {
            event.minimap.sync(&event.document, thumb);
        }
    }
}

/// A trailing stop explicitly ends the tape. Keep it outside ordinary
/// attached saves so resizing/deleting the last block can preserve it.
fn take_endpoint(events: &[ReplayEvent], aside: &mut Vec<Aside>) -> Option<f64> {
    let last = events.last()?;
    let first = aside
        .iter()
        .rposition(|save| {
            save.after + 1 != events.len() || save.save.via.as_deref() != Some("stop")
        })
        .map_or(0, |index| index + 1);
    // Repeated Stop presses do not lengthen the block that already stopped.
    // Remove the redundant tail together, so extending its endpoint later
    // cannot leave an earlier hidden stop that cuts off external playback.
    let end = aside[first..]
        .iter()
        .map(|stop| stop.save.at)
        .filter(|at| at.is_finite() && *at > last.at)
        .min_by(f64::total_cmp);
    aside.truncate(first);
    end
}

/// The blocks, and the saves kept aside: a stop, or a save an earlier
/// replay's run made, each riding with the block it came after. Blocks of
/// one recorded body share its text and its document.
fn events_of(script: &SessionScript) -> Result<(Vec<ReplayEvent>, Vec<Aside>), String> {
    let mut events: Vec<ReplayEvent> = Vec::new();
    let mut aside = Vec::new();
    // The first block of each body, by the address its saves share.
    let mut firsts: HashMap<*const u8, usize> = HashMap::new();
    for save in script.saves.iter().filter(|save| save.installed()) {
        if matches!(save.via.as_deref(), Some("stop" | "replay")) {
            let after = events.len().saturating_sub(1);
            let start = events.last().map_or(save.at, |event| event.at);
            aside.push(Aside {
                after,
                offset: save.at - start,
                save: save.clone(),
            });
        } else if let Some(&first) = firsts.get(&save.source.as_ptr()) {
            events.push(ReplayEvent {
                at: save.at,
                via: save.via.clone(),
                ..events[first].clone()
            });
        } else {
            firsts.insert(save.source.as_ptr(), events.len());
            events.push(ReplayEvent::new(
                save.at,
                Arc::clone(&save.source),
                save.via.clone(),
            )?);
        }
    }
    Ok((events, aside))
}

/// `m:ss`, or `h:mm:ss` past an hour.
pub fn clock(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    let (hours, minutes, seconds) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// A countdown: tenths under ten seconds, whole seconds above.
pub fn countdown_text(seconds: f64) -> String {
    if seconds < 10.0 {
        format!("{seconds:.1}s")
    } else {
        format!("{}s", seconds.round() as u64)
    }
}

/// The timeline over a replay tab's editor.
pub struct TimelineView<'a> {
    pub tab: &'a ReplayTab,
    pub theme: &'a Theme,
    pub now: Instant,
    /// The tab's editor has the caret.
    pub focused: bool,
}

impl Widget for TimelineView<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.height < TIMELINE_HEIGHT || area.width < BLOCK_WIDTH {
            return;
        }
        let theme = self.theme;
        let colour = theme.replay_colour();
        super::view::clear_surface(
            buffer,
            area,
            Style::default().bg(theme.surface).fg(theme.muted),
        );
        let tab = self.tab;
        if tab.events.is_empty() {
            buffer.set_stringn(
                area.x,
                area.y + 1,
                "Empty tape · type code and evaluate to add a block",
                usize::from(area.width),
                Style::default().fg(theme.muted),
            );
        }
        let across = ReplayTab::blocks_across(area.width);
        let playing = tab.playing();
        let origin = tab.origin();
        for index in tab.scroll..(tab.scroll + across).min(tab.events.len()) {
            let Some(rect) = tab.block_rect(area, index) else {
                break;
            };
            let event = &tab.events[index];
            let selected = index == tab.selected;
            let sounding = playing == Some(index);
            // The header row: the block's number, and what it is doing.
            let mut head = format!("{}", index + 1);
            if selected && let Some(length) = tab.duration_of(index) {
                head.push_str(&format!(" {}", countdown_text(length)));
            }
            if event.edited {
                head.push_str(" ●");
            }
            if sounding {
                head = match tab.countdown(self.now) {
                    Some(left) => format!("▶{} {}", index + 1, countdown_text(left)),
                    None => format!("▶{} last", index + 1),
                };
            }
            let head_style = if sounding {
                Style::default().fg(theme.ok).add_modifier(Modifier::BOLD)
            } else if selected {
                Style::default().fg(colour).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.foreground)
            };
            buffer.set_stringn(rect.x, rect.y, &head, usize::from(rect.width), head_style);
            // The thumbnail, lit for the selection.
            let thumb = Rect::new(rect.x, rect.y + 1, rect.width, THUMBNAIL_ROWS);
            MinimapView {
                minimap: &event.minimap,
                theme,
                viewport: (usize::MAX, usize::MAX),
            }
            .render(thumb, buffer);
            if selected {
                let wash = if self.focused {
                    theme.selection
                } else {
                    theme.overlay
                };
                buffer.set_style(thumb, Style::default().bg(wash));
            }
            if sounding {
                buffer.set_style(
                    Rect::new(rect.x, rect.y, rect.width, 1),
                    Style::default().bg(theme.surface),
                );
            }
            // The ruler row: when this block came.
            buffer.set_stringn(
                rect.x,
                rect.y + 1 + THUMBNAIL_ROWS,
                clock(event.at - origin),
                usize::from(rect.width),
                Style::default().fg(if selected { colour } else { theme.muted }),
            );
        }
        // The bottom row: the scrollbar when the tape is longer than the
        // strip, and the total at its right end, off the ruler so the
        // blocks' times are never written over.
        let bar_y = area.y + TIMELINE_HEIGHT - 1;
        let total = tab.total_text();
        let total_width = total.chars().count() as u16;
        let track = match tab.bar_geometry(area.width) {
            Some((track, thumb_x, thumb_len)) => {
                let line: String = (0..track)
                    .map(|x| {
                        if x >= thumb_x && x < thumb_x + thumb_len {
                            '━'
                        } else {
                            '─'
                        }
                    })
                    .collect();
                buffer.set_stringn(
                    area.x,
                    bar_y,
                    &line,
                    usize::from(track),
                    Style::default().fg(theme.muted),
                );
                buffer.set_style(
                    Rect::new(area.x + thumb_x, bar_y, thumb_len, 1),
                    Style::default().fg(colour),
                );
                track
            }
            None => {
                let track = if area.width > total_width + 1 {
                    area.width - total_width - 1
                } else {
                    area.width
                };
                let line: String = "─".repeat(usize::from(track));
                buffer.set_stringn(
                    area.x,
                    bar_y,
                    &line,
                    usize::from(track),
                    Style::default().fg(theme.rule),
                );
                track
            }
        };
        if area.width > total_width + 1 {
            buffer.set_stringn(
                area.x + track + 1,
                bar_y,
                &total,
                usize::from(total_width),
                Style::default().fg(colour),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_shortcuts_stay_below_the_editor_and_wrap_in_narrow_panes() {
        let theme = Theme::built_in_default();
        for width in [40, 70, 120] {
            let pane = Rect::new(3, 5, width, 30);
            let (strip, editor) = split_timeline(pane).unwrap();
            let footer = shortcut_area(pane).unwrap();
            assert_eq!(strip.bottom(), editor.y);
            assert_eq!(editor.bottom(), footer.y);
            assert_eq!(footer.bottom(), pane.bottom());
            assert_eq!(footer.height, if width < 100 { 2 } else { 1 });
            for focused in [false, true] {
                let mut buffer = Buffer::empty(pane);
                TimelineShortcuts {
                    focused,
                    theme: &theme,
                }
                .render(footer, &mut buffer);
                let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
                if focused {
                    assert!(text.contains("Esc editor"), "{text}");
                    assert!(text.contains("←/→ blocks"), "{text}");
                    assert!(text.contains("Enter play"), "{text}");
                    if width >= 70 {
                        assert!(text.contains("T duration"), "{text}");
                        assert!(text.contains("Del delete"), "{text}");
                        assert!(text.contains("Home/End ends"), "{text}");
                    }
                } else {
                    assert!(
                        text.contains(crate::keybinds::shortcut_label("Alt+T timeline").as_ref()),
                        "{text}"
                    );
                    assert!(
                        text.contains(crate::keybinds::shortcut_label("Alt+←/→ blocks").as_ref()),
                        "{text}"
                    );
                }
                for y in editor.y..editor.bottom() {
                    assert!(
                        (editor.x..editor.right())
                            .all(|x| buffer.cell((x, y)).unwrap().symbol() == " ")
                    );
                }
            }
        }
        let too_short = Rect::new(0, 0, 70, TIMELINE_HEIGHT + 3);
        assert!(split_timeline(too_short).is_none());
        assert!(shortcut_area(too_short).is_none());
    }

    #[test]
    fn extending_a_last_block_replaces_all_redundant_trailing_stops() {
        let directory = tempfile::tempdir().unwrap();
        let path = tape(
            directory.path(),
            &[
                (0.0, "a", None),
                (2.0, "silence", Some("stop")),
                (5.0, "silence", Some("stop")),
            ],
        );
        let mut tab = ReplayTab::open(&path).unwrap();
        assert_eq!(tab.duration_of(0), Some(2.0));
        assert!(tab.aside.is_empty());
        tab.set_duration_exact(0, 10.0).unwrap();
        tab.save().unwrap();
        let script = SessionScript::load(&path).unwrap();
        let stops: Vec<_> = script
            .saves
            .iter()
            .filter(|save| save.via.as_deref() == Some("stop"))
            .collect();
        assert_eq!(stops.len(), 1);
        assert_eq!(stops[0].at, 10.0);
        assert_eq!(ReplayTab::open(&path).unwrap().duration_of(0), Some(10.0));
    }

    #[test]
    fn populating_a_stop_only_tape_does_not_inherit_its_old_silence() {
        let directory = tempfile::tempdir().unwrap();
        let path = tape(directory.path(), &[(2.0, "silence", Some("stop"))]);
        let mut tab = ReplayTab::open(&path).unwrap();
        assert!(tab.events.is_empty());
        tab.set_source(0, "new block").unwrap();
        assert!(tab.aside.is_empty());
        tab.save().unwrap();
        let script = SessionScript::load(&path).unwrap();
        assert_eq!(script.saves.len(), 1);
        assert_eq!(&*script.saves[0].source, "new block");
        assert_eq!(ReplayTab::open(&path).unwrap().duration_of(0), None);
    }

    #[test]
    fn a_clamped_aside_stays_with_its_own_block_at_a_shared_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let path = tape(
            directory.path(),
            &[
                (0.0, "a", None),
                (1.0, "silence", Some("stop")),
                (2.0, "b", None),
            ],
        );
        let mut tab = ReplayTab::open(&path).unwrap();
        tab.set_duration_exact(0, 0.25).unwrap();
        tab.save().unwrap();
        let again = ReplayTab::open(&path).unwrap();
        assert_eq!(again.duration_of(1), None);
        assert_eq!(again.aside.len(), 1);
        assert_eq!(again.aside[0].after, 0);
        assert_eq!(again.aside[0].offset, 0.25);
        let script = SessionScript::load(&path).unwrap();
        assert_eq!(script.saves[1].via.as_deref(), Some("stop"));
        assert_eq!(&*script.saves[2].source, "b");
    }

    #[test]
    fn exact_durations_preserve_neighbors_and_a_finite_last_block_after_reopening() {
        let directory = tempfile::tempdir().unwrap();
        let path = tape(
            directory.path(),
            &[
                (10.0, "a", None),
                (12.0, "b", None),
                (15.0, "c", None),
                (19.0, "silence", Some("stop")),
            ],
        );
        let mut tab = ReplayTab::open(&path).unwrap();
        assert_eq!(tab.duration_of(2), Some(4.0));
        assert_eq!(tab.duration(), 9.0);
        assert!(tab.set_duration_exact(1, 0.03125).unwrap());
        assert_eq!(tab.duration_of(0), Some(2.0));
        assert_eq!(tab.duration_of(1), Some(0.03125));
        assert_eq!(tab.duration_of(2), Some(4.0));
        assert_eq!(tab.events[2].at, 12.03125);
        assert!(tab.set_duration_exact(2, 0.0625).unwrap());
        tab.save().unwrap();
        let mut again = ReplayTab::open(&path).unwrap();
        assert_eq!(again.duration_of(1), Some(0.03125));
        assert_eq!(again.duration_of(2), Some(0.0625));
        let now = Instant::now();
        again.start(2, now);
        assert!(!again.due(now + Duration::from_millis(62)));
        assert!(again.due(now + Duration::from_micros(62_500)));
        assert_eq!(again.advance(now + Duration::from_micros(62_500)), None);
        assert!(again.run.is_none());
        let script = SessionScript::load(&path).unwrap();
        assert_eq!(script.saves.last().unwrap().via.as_deref(), Some("stop"));
        assert_eq!(script.saves.last().unwrap().at, 12.09375);
    }

    #[test]
    fn deleting_first_middle_or_last_preserves_all_surviving_lengths() {
        for removed in 0..3 {
            let directory = tempfile::tempdir().unwrap();
            let path = tape(
                directory.path(),
                &[
                    (10.0, "a", None),
                    (12.0, "b", None),
                    (15.0, "c", None),
                    (19.0, "silence", Some("stop")),
                ],
            );
            let mut tab = ReplayTab::open(&path).unwrap();
            tab.selected = 2;
            tab.scroll = 2;
            tab.start(2, Instant::now());
            assert!(tab.delete_event(removed).unwrap());
            assert_eq!(tab.origin(), 10.0);
            assert_eq!(tab.selected, 1);
            assert_eq!(tab.scroll, 1);
            assert!(tab.run.is_none());
            let wanted: Vec<_> = [2.0, 3.0, 4.0]
                .into_iter()
                .enumerate()
                .filter_map(|(index, duration)| (index != removed).then_some(duration))
                .collect();
            assert_eq!(tab.duration_of(0), Some(wanted[0]));
            assert_eq!(tab.duration_of(1), Some(wanted[1]));
            assert_eq!(tab.events[1].at, 10.0 + wanted[0]);
            tab.save().unwrap();
            let again = ReplayTab::open(&path).unwrap();
            assert_eq!(again.events.len(), 2);
            assert_eq!(again.duration_of(0), Some(wanted[0]));
            assert_eq!(again.duration_of(1), Some(wanted[1]));
        }
    }

    #[test]
    fn removing_an_indefinite_last_block_retains_its_predecessors_duration() {
        let directory = tempfile::tempdir().unwrap();
        let path = tape(
            directory.path(),
            &[(10.0, "a", None), (12.0, "b", None), (15.0, "c", None)],
        );
        let mut tab = ReplayTab::open(&path).unwrap();
        assert_eq!(
            tab.duration_of(2),
            None,
            "old tapes never acquire a guessed ending"
        );
        tab.start(2, Instant::now());
        assert_eq!(tab.run.unwrap().next_due, None);
        tab.delete_event(2).unwrap();
        assert_eq!(tab.duration_of(1), Some(3.0));
        tab.save().unwrap();
        let again = ReplayTab::open(&path).unwrap();
        assert_eq!(again.duration_of(1), Some(3.0));
        assert_eq!(again.duration(), 5.0);
    }

    #[test]
    fn deleting_a_block_removes_its_asides_and_keeps_other_asides_attached() {
        let directory = tempfile::tempdir().unwrap();
        let path = tape(
            directory.path(),
            &[
                (0.0, "a", None),
                (1.0, "silence", Some("stop")),
                (1.5, "played a", Some("replay")),
                (2.0, "b", None),
                (3.0, "silence", Some("stop")),
                (5.0, "c", None),
                (9.0, "silence", Some("stop")),
            ],
        );
        let mut tab = ReplayTab::open(&path).unwrap();
        assert_eq!(tab.aside.len(), 3);
        tab.delete_event(1).unwrap();
        assert_eq!(tab.aside.len(), 2);
        assert!(tab.aside.iter().all(|aside| aside.after == 0));
        tab.save().unwrap();
        let again = ReplayTab::open(&path).unwrap();
        assert_eq!(again.duration_of(0), Some(2.0));
        assert_eq!(again.duration_of(1), Some(4.0));
        assert_eq!(again.aside.len(), 2);
        assert_eq!(again.aside[0].offset, 1.0);
        assert_eq!(again.aside[1].offset, 1.5);
    }

    #[test]
    fn deleting_the_sole_block_leaves_an_editable_empty_tape() {
        let directory = tempfile::tempdir().unwrap();
        let path = tape(
            directory.path(),
            &[(5.0, "a", None), (7.0, "silence", Some("stop"))],
        );
        let mut tab = ReplayTab::open(&path).unwrap();
        assert!(tab.delete_event(0).unwrap());
        assert!(tab.events.is_empty());
        assert!(tab.aside.is_empty());
        assert_eq!(tab.selected_source(), "");
        assert_eq!((tab.origin(), tab.duration()), (0.0, 0.0));
        assert_eq!((tab.selected, tab.scroll), (0, 0));
        tab.start(0, Instant::now());
        assert!(tab.run.is_none());
        tab.save().unwrap();
        assert!(
            SessionScript::load(&path).is_err(),
            "CLI playback still requires a save"
        );
        assert!(
            SessionScript::load_for_editing(&path)
                .unwrap()
                .saves
                .is_empty()
        );
        let mut again = ReplayTab::open(&path).unwrap();
        assert!(again.events.is_empty());
        let area = Rect::new(0, 0, 80, TIMELINE_HEIGHT);
        let mut buffer = Buffer::empty(area);
        TimelineView {
            tab: &again,
            theme: &Theme::default(),
            now: Instant::now(),
            focused: true,
        }
        .render(area, &mut buffer);
        let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("Empty tape"));
        again.set_source(0, "$: s(\"bd\")").unwrap();
        assert_eq!(again.events.len(), 1);
        again.set_duration_exact(0, 0.0625).unwrap();
        again.save().unwrap();
        let populated = ReplayTab::open(&path).unwrap();
        assert_eq!(populated.selected_source(), "$: s(\"bd\")");
        assert_eq!(populated.duration_of(0), Some(0.0625));
    }

    #[test]
    fn invalid_or_debug_timing_edits_leave_the_candidate_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let path = tape(directory.path(), &[(0.0, "a", None), (2.0, "b", None)]);
        let original = ReplayTab::open(&path).unwrap();
        let mut candidate = original.clone();
        for seconds in [f64::NAN, f64::INFINITY, -1.0, 0.0, 86_400.1] {
            assert!(candidate.set_duration_exact(0, seconds).is_err());
        }
        assert!(!candidate.dirty);
        assert_eq!(candidate.events[1].at, 2.0);
        candidate.keeps = "all-saves-and-diagnostics".into();
        assert!(candidate.delete_event(0).is_err());
        assert!(candidate.set_duration_exact(0, 1.0).is_err());
        assert_eq!(candidate.events.len(), 2);
        assert!(!original.dirty);
        assert_eq!(original.duration_of(0), Some(2.0));
    }

    /// The bar leaves the total its room at the right end, and the thumb
    /// The timeline counts from the first block, not from the moment the
    /// recorder opened: a set whose first evaluate came a quarter of an
    /// hour in still reads 0:00 under its first block.
    #[test]
    fn the_timeline_counts_from_the_first_block() {
        let directory = tempfile::tempdir().unwrap();
        let mut tab = ReplayTab::open(&tape(
            directory.path(),
            &[
                (905.0, "$: s(\"bd\")", None),
                (935.0, "$: s(\"sd\")", None),
                (1000.0, "$: s(\"hh\")", None),
            ],
        ))
        .unwrap();
        assert_eq!(tab.origin(), 905.0);
        assert_eq!(tab.since_origin(1), 30.0);
        assert_eq!(tab.duration(), 95.0);
        assert_eq!(tab.total_text(), "1:35 · 3 saves");
        let theme = super::super::theme::Theme::built_in_default();
        let area = Rect::new(0, 0, 40, TIMELINE_HEIGHT);
        tab.sync_thumbnails(area);
        let mut buffer = Buffer::empty(area);
        TimelineView {
            tab: &tab,
            theme: &theme,
            now: Instant::now(),
            focused: false,
        }
        .render(area, &mut buffer);
        let ruler = (0..area.width)
            .filter_map(|x| buffer.cell((x, 1 + THUMBNAIL_ROWS)))
            .map(|cell| cell.symbol().to_owned())
            .collect::<String>();
        assert!(ruler.contains("0:00"), "{ruler}");
        assert!(ruler.contains("0:30"), "{ruler}");
        assert!(!ruler.contains("15:"), "no clock time: {ruler}");
    }

    /// walks the rest: its far end is the last screenful.
    #[test]
    fn the_bar_leaves_room_for_the_total_and_the_thumb_scrolls_it() {
        let directory = tempfile::tempdir().unwrap();
        let saves: Vec<(f64, String, Option<&str>)> = (0..20)
            .map(|index| (index as f64, format!("$: s(\"bd:{index}\")"), None))
            .collect();
        let borrowed: Vec<(f64, &str, Option<&str>)> = saves
            .iter()
            .map(|(at, source, via)| (*at, source.as_str(), *via))
            .collect();
        let mut tab = ReplayTab::open(&tape(directory.path(), &borrowed)).unwrap();
        // Sixty cells: five blocks across, "0:19 · 20 saves" at the end.
        let (track, thumb_x, thumb_len) = tab.bar_geometry(60).expect("longer than the strip");
        assert_eq!(track, 60 - 15 - 1);
        assert_eq!(thumb_x, 0);
        assert_eq!(thumb_len, 44 * 5 / 20);
        tab.scroll_thumb_to(track - thumb_len, 60);
        assert_eq!(tab.scroll, 15, "the far end is the last screenful");
        tab.scroll_thumb_to(0, 60);
        assert_eq!(tab.scroll, 0);
        let mut few = ReplayTab::open(&tape(directory.path(), &borrowed[..3])).unwrap();
        assert!(few.bar_geometry(60).is_none(), "three blocks need no bar");
        few.scroll_thumb_to(10, 60);
        assert_eq!(few.scroll, 0);
    }

    /// Blocks of one recorded body share its text and its document, so a
    /// tape of references to one score opens in the memory of that score.
    #[test]
    fn blocks_of_one_body_share_its_text_and_document() {
        let directory = tempfile::tempdir().unwrap();
        let path = tape(directory.path(), &[(0.0, "$: s(\"bd\")", None)]);
        let mut text = std::fs::read_to_string(&path).unwrap();
        for at in 1..64 {
            text.push_str(&format!(
                "{{\"kind\":\"save\",\"ref\":0,\"status\":\"installed\",\"t\":{at}}}\n"
            ));
        }
        std::fs::write(&path, text).unwrap();
        let tab = ReplayTab::open(&path).unwrap();
        assert_eq!(tab.events.len(), 64);
        let chunk = |event: &ReplayEvent| event.document.rope().chunks().next().map(str::as_ptr);
        for event in &tab.events {
            assert_eq!(event.source.as_ptr(), tab.events[0].source.as_ptr());
            assert_eq!(chunk(event), chunk(&tab.events[0]));
        }
    }

    fn tape(directory: &Path, saves: &[(f64, &str, Option<&str>)]) -> PathBuf {
        use std::io::Write;
        let path = directory.join("session-2026-09-05T12-09-48.rustel-session");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(
            file,
            r#"{{"keeps":"installed-saves","recorded":"2026-09-05T12:09:48Z","version":2}}"#
        )
        .unwrap();
        for (at, source, via) in saves {
            let encoded = rustel_runtime::session_log::encode_base64(source.as_bytes());
            let via = via
                .map(|via| format!(r#","via":"{via}""#))
                .unwrap_or_default();
            writeln!(
                file,
                r#"{{"kind":"save","source":"{encoded}","status":"installed","t":{at}{via}}}"#
            )
            .unwrap();
        }
        path
    }

    /// A tape opens on its saves that sounded; a stop is kept but is no
    /// block. Choosing a block puts its code up; the run walks the blocks
    /// on the tape's own clock, counting down to each next one, ends on
    /// the last, and moves the selection only while the selection is on
    /// the sounding block.
    #[test]
    fn a_tape_replays_on_its_own_clock() {
        let directory = tempfile::tempdir().unwrap();
        let path = tape(
            directory.path(),
            &[
                (0.0, "$: s(\"bd\")", None),
                (4.0, "$: s(\"sd\")", None),
                (4.5, "silence", Some("stop")),
                (10.0, "$: s(\"hh\")", None),
            ],
        );
        let mut tab = ReplayTab::open(&path).unwrap();
        assert_eq!(tab.recorded.as_deref(), Some("2026-09-05T12:09:48Z"));
        assert_eq!(tab.events.len(), 3, "a stop is not a block");
        assert_eq!(tab.aside.len(), 1, "but it is kept");
        assert_eq!((tab.aside[0].after, tab.aside[0].offset), (1, 0.5));
        assert_eq!(tab.duration(), 10.0);
        assert!(tab.select(1));
        assert_eq!(tab.selected_source(), "$: s(\"sd\")");
        let t0 = Instant::now();
        tab.start(1, t0);
        assert_eq!(tab.playing(), Some(1));
        assert!(
            (tab.countdown(t0).unwrap() - 6.0).abs() < 1e-6,
            "a block plays to the next one, stop or no stop"
        );
        assert!(!tab.due(t0 + Duration::from_millis(5900)));
        assert!(tab.due(t0 + Duration::from_millis(6000)));
        let t1 = t0 + Duration::from_millis(6000);
        assert_eq!(tab.advance(t1), Some(2));
        assert_eq!(tab.selected, 2, "the run moves the selection with it");
        assert_eq!(tab.countdown(t1), None, "nothing after the last block");
        assert_eq!(tab.advance(t1), None, "the run ends on the last block");
        assert_eq!(tab.playing(), None);
        assert_eq!(tab.selected, 2);
        // A block chosen to be read stays chosen while the run goes on.
        tab.start(0, t0);
        assert!(tab.select(2));
        assert_eq!(tab.advance(t0 + Duration::from_millis(4000)), Some(1));
        assert_eq!(tab.selected, 2, "the reader's block, not the run's");
        assert_eq!(tab.playing(), Some(1));
        tab.stop();
        // An edit is the block's now, and survives a reload; the run does not
        // read the tape, so nothing else changes.
        tab.set_source(2, "$: s(\"cp\")").unwrap();
        assert!(tab.events[2].edited);
        assert!(tab.dirty);
        tab.reload().unwrap();
        assert_eq!(&*tab.events[2].source, "$: s(\"cp\")");
        assert!(tab.events[2].edited);
        assert!(!tab.events[0].edited);
        assert_eq!(tab.aside.len(), 1);
        // A block's length moves everything after it; the shortest is held.
        assert_eq!(tab.duration_of(0), Some(4.0));
        tab.set_duration(0, 6.0);
        assert_eq!(
            tab.events.iter().map(|e| e.at).collect::<Vec<_>>(),
            [0.0, 6.0, 12.0]
        );
        tab.set_duration(1, 0.0);
        assert!(
            (tab.events[2].at - 6.1).abs() < 1e-9,
            "held at the shortest"
        );
        assert_eq!(tab.duration_of(2), None, "the last block has no length");
        // Written back, the tape reads as edited - code, times and marks -
        // and the stop is still on it, after the block it followed and
        // never past the next.
        assert!(tab.writable());
        tab.save().unwrap();
        assert!(!tab.dirty);
        let again = ReplayTab::open(&path).unwrap();
        assert_eq!(again.events.len(), 3);
        assert_eq!(&*again.events[2].source, "$: s(\"cp\")");
        assert!((again.events[1].at - 6.0).abs() < 1e-9);
        assert_eq!(again.aside.len(), 1, "the stop kept its place on the tape");
        assert_eq!(again.aside[0].save.via.as_deref(), Some("stop"));
        assert!(
            (again.aside[0].save.at - 6.1).abs() < 1e-9,
            "held within its block"
        );
        assert_eq!(again.recorded.as_deref(), Some("2026-09-05T12:09:48Z"));
    }

    /// Blocks are a fixed width; the strip scrolls to keep the selection on
    /// screen and a click lands on the block under it.
    #[test]
    fn the_strip_scrolls_and_clicks_land_on_blocks() {
        let directory = tempfile::tempdir().unwrap();
        let saves: Vec<(f64, String, Option<&str>)> = (0..20)
            .map(|index| (index as f64 * 2.0, format!("$: s(\"bd:{index}\")"), None))
            .collect();
        let borrowed: Vec<(f64, &str, Option<&str>)> = saves
            .iter()
            .map(|(at, source, via)| (*at, source.as_str(), *via))
            .collect();
        let path = tape(directory.path(), &borrowed);
        let mut tab = ReplayTab::open(&path).unwrap();
        let area = Rect::new(0, 3, 58, TIMELINE_HEIGHT);
        assert_eq!(ReplayTab::blocks_across(area.width), 4);
        assert_eq!(tab.block_at(area, 13, 4), Some(1));
        assert_eq!(tab.block_at(area, 57, 4), None, "past the last whole block");
        tab.select(7);
        tab.ensure_visible(area.width);
        assert_eq!(tab.scroll, 4);
        assert_eq!(tab.block_at(area, 0, 4), Some(4));
        tab.scroll_by(100, area.width);
        assert_eq!(tab.scroll, 16, "never past the last screenful");
        tab.scroll_by(-100, area.width);
        assert_eq!(tab.scroll, 0);
        tab.scroll_to(0.5, area.width);
        assert_eq!(
            tab.scroll, 8,
            "halfway along the bar is halfway through the tape"
        );
        tab.scroll_to(1.5, area.width);
        assert_eq!(tab.scroll, 16);
        assert_eq!(ReplayTab::scrollbar_row(area), 8);
        tab.scroll_to(0.0, area.width);
        assert_eq!(clock(754.0), "12:34");
        assert_eq!(clock(3601.0), "1:00:01");
        assert_eq!(countdown_text(3.25), "3.2s");
        assert_eq!(countdown_text(42.6), "43s");

        // Drawing paints every block on screen and the total at the right.
        tab.sync_thumbnails(area);
        let theme = Theme::built_in_default();
        let mut buffer = Buffer::empty(Rect::new(0, 0, 58, 12));
        TimelineView {
            tab: &tab,
            theme: &theme,
            now: Instant::now(),
            focused: true,
        }
        .render(area, &mut buffer);
        let row = |y: u16| -> String {
            (0..58)
                .map(|x| buffer.cell((x, y)).unwrap().symbol().to_owned())
                .collect()
        };
        assert!(row(3).contains('1') && row(3).contains('4'), "{:?}", row(3));
        assert!(
            !row(7).contains("saves"),
            "the ruler keeps its times: {:?}",
            row(7)
        );
        assert!(row(8).contains("0:38 · 20 saves"), "{:?}", row(8));
        assert!(row(8).contains('━'), "a scrollbar: {:?}", row(8));
    }
}
