/*
mixer_panel.rs - The mixer panel: a desk of strips along the top or the bottom
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! The mixer as a desk, the way a DAW draws one. Each thing that carries
//! sound has a strip: the audio input, each orbit the score names or has
//! sounded lately, and the master. The strips stand side by side across a
//! band at the bottom or the top of the screen. Each strip has a meter in
//! the footer's own colours and scale, a fader knob where it has a fader,
//! and the level or the fader value under it. Beside the strips are the
//! devices: the MIDI ports and the pads, lit while they are active, with
//! the last messages and the pad activity under them. The panel is sticky
//! like the set panel: it stays while the score is edited, Esc returns the
//! keyboard to the editor and leaves the panel open, and F4 hides it.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Widget;

use super::meter::{level_color, scale_decibels, scale_position};
use super::theme::{Theme, mix};
use super::viz_panel::{METER_FLOOR_DB, MixerFacts, MixerStripKind, REDUCTION_FULL_DB};

/// The rows the panel takes until it is resized: enough for a meter to
/// read, bigger than a widget's slot, and still most of the score.
pub const DEFAULT_ROWS: u16 = 16;
pub const MIN_ROWS: u16 = 9;
pub const MAX_ROWS: u16 = 32;

/// A strip's width, gap included: a knob column, a meter, the readout
/// under them, and room for `orbit 12`.
pub const STRIP_WIDTH: u16 = 9;
/// A reduction strip's width. Narrower than the rest because it has less
/// to say: no level, a reading of at most `-12.0`, and a mode name chosen
/// to fit. The desk is where the score's faders live now, and a strip
/// that spends nine columns on four is nine columns they do not get.
pub const REDUCTION_STRIP_WIDTH: u16 = 6;

/// How wide one strip is drawn, by what it has to show.
pub const fn strip_width(kind: MixerStripKind) -> u16 {
    match kind {
        MixerStripKind::Level => STRIP_WIDTH,
        MixerStripKind::Reduction { .. } => REDUCTION_STRIP_WIDTH,
    }
}

/// The widths of a desk's strips, in order - the one thing the drawing
/// and every hit test have to agree about.
pub fn strip_widths(strips: &[super::viz_panel::MixerStrip]) -> Vec<u16> {
    strips.iter().map(|strip| strip_width(strip.kind)).collect()
}
/// The meter's columns within a strip, after the knob column.
const METER_COLUMNS: std::ops::Range<u16> = 1..5;
/// A reduction meter is one column. It has one number to show and no
/// level behind it, so a single bar beside the ceiling's handle reads as
/// a slim gauge and not as a second level meter.
const REDUCTION_METER_COLUMNS: std::ops::Range<u16> = 2..3;

/// The columns a strip's meter fills, by what it has to show.
const fn meter_columns(kind: MixerStripKind) -> std::ops::Range<u16> {
    match kind {
        MixerStripKind::Level => METER_COLUMNS,
        MixerStripKind::Reduction { .. } => REDUCTION_METER_COLUMNS,
    }
}

/// The columns the fader's cap covers: its meter, and one column past it
/// on either side.
///
/// A cap wider than its rail is a cap you can hit. A single glyph in the
/// middle of the track was one cell to aim at with a pointer and nothing
/// at all to see from across a room, and it read as a mark ON the meter
/// rather than as the thing you take hold of.
const fn handle_columns(kind: MixerStripKind) -> std::ops::Range<u16> {
    let meter = meter_columns(kind);
    (meter.start - 1)..(meter.end + 1)
}

/// The held peak and the unity mark sit on this column, across the middle
/// of the track rather than in the gutter beside it.
const fn marker_column(kind: MixerStripKind) -> u16 {
    let meter = meter_columns(kind);
    (meter.start + meter.end) / 2
}

/// Where a `Level` strip's marks sit, for the tests that only ever ask
/// about one.
#[cfg(test)]
const MARKER_COLUMN: u16 = marker_column(MixerStripKind::Level);
/// The narrowest the device block is worth drawing.
const DEVICES_MIN_WIDTH: u16 = 14;

/// One fader of the score, gap included.
///
/// The rail is what is left after the two ends of the range, so widening
/// the fader is how the rail gets longer - and the rail's length is the
/// resolution of the thing: at ten cells a drag moves in tenths, which is
/// too coarse to place a filter by ear. Twice the rail costs a column of
/// faders on a narrow terminal, which is the right trade for a control
/// you are meant to perform with.
pub const FADER_WIDTH: u16 = 32;
/// Rows one fader takes: its name and reading, then its rail between the
/// ends of its range. Two, because all three numbers are worth having -
/// a fader you cannot see the range of is a fader you have to guess at -
/// and a rail squeezed between them on one row leaves no rail.
pub const FADER_HEIGHT: u16 = 2;
/// Columns of the top row given to the name; the reading takes the rest.
const FADER_LABEL: u16 = 12;
/// Columns of the bottom row given to each end of the range.
const FADER_END: u16 = 5;
/// The narrowest run of faders worth drawing at all.
const FADERS_MIN_WIDTH: u16 = FADER_WIDTH;

/// The scale's ceiling: a fader that ends there is the footer's fader
/// and takes the footer's scale; one that climbs past it is linear in
/// dB over its own range.
const SCALE_CEILING_DB: f32 = 6.0;

/// Which edge the desk is along.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MixerEdge {
    Top,
    #[default]
    Bottom,
}

impl MixerEdge {
    pub fn name(self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Bottom => "bottom",
        }
    }

    pub fn flipped(self) -> Self {
        match self {
            Self::Top => Self::Bottom,
            Self::Bottom => Self::Top,
        }
    }

    pub fn from_top(top: bool) -> Self {
        if top { Self::Top } else { Self::Bottom }
    }

    pub fn is_top(self) -> bool {
        self == Self::Top
    }
}

/// The open panel: where it is and how tall.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MixerPanel {
    pub edge: MixerEdge,
    pub rows: u16,
}

impl MixerPanel {
    pub fn new(edge: MixerEdge, rows: Option<u16>) -> Self {
        Self {
            edge,
            rows: rows.unwrap_or(DEFAULT_ROWS).clamp(MIN_ROWS, MAX_ROWS),
        }
    }

    /// A row taller or shorter, within the bounds; whether it changed.
    pub fn resize(&mut self, grow: bool) -> bool {
        let rows = if grow {
            self.rows.saturating_add(1)
        } else {
            self.rows.saturating_sub(1)
        }
        .clamp(MIN_ROWS, MAX_ROWS);
        let changed = rows != self.rows;
        self.rows = rows;
        changed
    }

    /// The room the layout is asked for: a band along the panel's edge.
    pub fn dock(self) -> super::viz_panel::Dock {
        super::viz_panel::Dock {
            edge: match self.edge {
                MixerEdge::Top => super::viz_panel::Edge::Top,
                MixerEdge::Bottom => super::viz_panel::Edge::Bottom,
            },
            extent: self.rows,
        }
    }
}

/// A drag selection over the devices block, held as the rows the drag
/// was drawn on: the log keeps scrolling, so the selection keeps its own
/// copy of those rows rather than re-reading whatever later scrolls under
/// them. The highlight is painted while they are still the rows on
/// screen, and `c` copies them.
#[derive(Clone, Debug)]
pub struct MixerSelection {
    pub lines: Vec<String>,
    pub area: Rect,
    pub selection: super::textblock::TextSelection,
}

/// The panel's rows: the rule along the score's side with the title on
/// it, the desk, and the hint row at the outer edge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MixerParts {
    pub rule: Rect,
    pub desk: Rect,
    pub hint: Rect,
}

pub fn parts(area: Rect, edge: MixerEdge) -> Option<MixerParts> {
    if area.is_empty() || area.height < 5 || area.width < STRIP_WIDTH + 2 {
        return None;
    }
    let inner_x = area.x + 1;
    let inner_width = area.width.saturating_sub(2);
    let (rule_y, hint_y, desk_y) = match edge {
        MixerEdge::Top => (area.bottom() - 1, area.y, area.y + 1),
        MixerEdge::Bottom => (area.y, area.bottom() - 1, area.y + 1),
    };
    Some(MixerParts {
        rule: Rect::new(area.x, rule_y, area.width, 1),
        desk: Rect::new(inner_x, desk_y, inner_width, area.height - 2),
        hint: Rect::new(inner_x, hint_y, inner_width, 1),
    })
}

/// The rows a strip's meter runs over in the desk: the row under the
/// label down to the row above the readout, as (top, bottom). Only a
/// press on these rows takes a fader.
pub fn meter_rows(desk: Rect) -> Option<(u16, u16)> {
    (desk.height >= 4).then(|| (desk.y + 1, desk.bottom() - 2))
}

/// The strip under a column of the desk, by its place from the left.
///
/// Walked rather than divided: the strips are not all one width any more,
/// and a hit test that assumed they were would send a press on the fifth
/// strip to the fourth the moment a narrow one stood before it.
pub fn strip_at(desk: Rect, widths: &[u16], x: u16) -> Option<usize> {
    if x < desk.x || x >= desk.right() {
        return None;
    }
    let mut left = desk.x;
    for (index, width) in widths.iter().enumerate() {
        // A strip the desk was too narrow to draw is not there to press.
        if left + width > desk.right() + 1 {
            return None;
        }
        // The gap column at its right belongs to nobody.
        if x < left + width - 1 {
            return (x >= left).then_some(index);
        }
        if x == left + width - 1 {
            return None;
        }
        left += width;
    }
    None
}

/// Where the strips stop and the faders may begin, reckoned exactly the
/// way `draw_desk` walks them so a hit test and the drawing agree.
pub fn strips_end(desk: Rect, widths: &[u16]) -> u16 {
    if meter_rows(desk).is_none() {
        return desk.x;
    }
    let mut x = desk.x;
    for width in widths {
        if x + width > desk.right() + 1 {
            break;
        }
        x += width;
    }
    x
}

/// The fader block of a drawn desk: what `render` lays out, offered to
/// the pointer so a press lands where the eye says it should.
pub fn fader_desk(desk: Rect, widths: &[u16], faders: usize) -> Option<FaderDesk> {
    FaderDesk::new(desk, strips_end(desk, widths) + 1, faders)
}

/// Where the score's faders go: the block to the right of the strips, and
/// how they are arranged in it.
///
/// Down a column first, then across - a score's sliders are read in source
/// order, and a reader looking for the third one should not have to know
/// how tall the desk happens to be to find it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FaderDesk {
    pub area: Rect,
    /// Faders in one column before the next begins.
    pub rows: u16,
    /// Columns there is room to draw.
    pub columns: u16,
}

impl FaderDesk {
    /// The desk for a run of faders beginning at `from_x`, or none when
    /// there is neither room nor anything to put in it.
    pub fn new(desk: Rect, from_x: u16, faders: usize) -> Option<Self> {
        if faders == 0 || from_x + FADERS_MIN_WIDTH > desk.right() {
            return None;
        }
        let width = desk.right() - from_x;
        let rows = (desk.height / FADER_HEIGHT).max(1);
        let wanted = (faders as u16).div_ceil(rows).max(1);
        let columns = (width / FADER_WIDTH).clamp(1, wanted);
        Some(Self {
            area: Rect::new(from_x, desk.y, columns * FADER_WIDTH, desk.height),
            rows,
            columns,
        })
    }

    /// Where one fader is drawn, or none for a place the desk has no room
    /// to show.
    pub fn cell(&self, at: usize) -> Option<Rect> {
        let at = u16::try_from(at).ok()?;
        let (column, row) = (at / self.rows, at % self.rows);
        (column < self.columns).then(|| {
            Rect::new(
                self.area.x + column * FADER_WIDTH,
                self.area.y + row * FADER_HEIGHT,
                FADER_WIDTH.saturating_sub(1),
                FADER_HEIGHT,
            )
        })
    }

    /// The fader under a point, if the point is on one.
    pub fn at(&self, x: u16, y: u16, faders: usize) -> Option<usize> {
        (0..faders).find(|at| {
            self.cell(*at)
                .is_some_and(|cell| cell.contains(ratatui::layout::Position::new(x, y)))
        })
    }

    /// The rail's own columns, on the fader's second row between the ends
    /// of its range: what a press has to land on to move it, and where the
    /// knob is drawn.
    pub fn rail(cell: Rect) -> Option<Rect> {
        let taken = FADER_END * 2 + 2;
        (cell.width > taken && cell.height >= FADER_HEIGHT)
            .then(|| Rect::new(cell.x + FADER_END + 1, cell.y + 1, cell.width - taken, 1))
    }
}

/// Where a fader stands on its strip, 0 at the floor and 1 at the top:
/// the footer's scale for a fader that ends at the scale's ceiling, a
/// straight run in dB for one that climbs past it.
fn fader_position(db: f32, range: (f32, f32)) -> f32 {
    let (low, high) = range;
    if on_the_meter_scale(range) {
        scale_position(db)
    } else {
        ((db - low) / (high - low).max(1.0)).clamp(0.0, 1.0)
    }
}

/// Whether a strip's fader rides the footer's own -60..+6 scale rather
/// than its declared range.
///
/// The master does, so its handle stands where its level is read. The
/// floor must match as well as the top: the limiter's ceiling tops out at
/// 0, under +6, and on the footer's scale it could reach neither end of
/// its own rail. Only the master's floor matches.
fn on_the_meter_scale((low, high): (f32, f32)) -> bool {
    high <= SCALE_CEILING_DB + 0.01 && low <= METER_FLOOR_DB + 0.01
}

/// The fader a row of the desk asks for, on the strip's own scale, with
/// a detent at unity: a press that lands near 0 dB lands on it.
pub fn fader_db_at(desk: Rect, y: u16, range: (f32, f32)) -> f32 {
    fader_db_within(desk, y, 0.0, range)
}

/// The same, given how far down the row the pointer is.
///
/// A strip is a column, so a row is the whole of its resolution: on a desk
/// ten rows tall a fader has ten stops over its whole range. A terminal
/// that reports pixels knows where in the row the pointer is, so the value
/// comes from there while the picture stays on the cell grid. The score's
/// sliders do the same along their rows.
///
/// `subrow` is a fraction in `[0, 1)`, zero at the row's top edge. A
/// terminal that reports cells passes zero, which selects the row itself.
pub fn fader_db_within(desk: Rect, y: u16, subrow: f64, range: (f32, f32)) -> f32 {
    let Some((top, bottom)) = meter_rows(desk) else {
        return 0.0;
    };
    let clamped = y.clamp(top, bottom);
    let travel = f32::from(bottom - top).max(1.0);
    // Down the row is down the fader, so the fraction comes off the
    // distance from the floor.
    let position = (f32::from(bottom - clamped) - subrow as f32).clamp(0.0, travel) / travel;
    let (low, high) = range;
    let (db, step) = if on_the_meter_scale(range) {
        // A row is a coarser step than the footer's column, so the band
        // is a half row of the range above the knee, or the footer's.
        (scale_decibels(position), (6.0 - -20.0) / (0.6 * travel))
    } else {
        (low + position * (high - low), (high - low) / travel)
    };
    if db.abs() <= (step * 0.5).max(0.75) {
        0.0
    } else {
        db.clamp(low, high)
    }
}

/// A level as the readout says it.
fn db_text(db: f32) -> String {
    if db <= METER_FLOOR_DB {
        "-inf".to_owned()
    } else {
        format!("{db:.0}")
    }
}

/// A fader as the readout says it.
fn gain_text(db: f32) -> String {
    if db <= METER_FLOOR_DB {
        "-inf".to_owned()
    } else if db.abs() < 0.05 {
        "0.0 dB".to_owned()
    } else {
        format!("{db:+.1}")
    }
}

pub struct MixerPanelView<'a> {
    pub keybinds: &'a super::keybinds::Keybinds,
    pub facts: Option<&'a MixerFacts>,
    pub panel: MixerPanel,
    pub theme: &'a Theme,
    pub focused: bool,
    pub area: Rect,
    /// A held drag selection over the devices block, painted while its
    /// rows are still the ones on screen.
    pub selection: Option<&'a MixerSelection>,
}

impl MixerPanelView<'_> {
    fn draw_rule(&self, parts: &MixerParts, buffer: &mut Buffer) {
        let theme = self.theme;
        let rule = Style::default().fg(theme.rule);
        for x in parts.rule.x..parts.rule.right() {
            buffer.set_stringn(x, parts.rule.y, "─", 1, rule);
        }
        let title = " mixer ";
        let style = if self.focused {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.accent)
        };
        buffer.set_stringn(
            parts.rule.x + 2,
            parts.rule.y,
            title,
            usize::from(parts.rule.width.saturating_sub(2)),
            style,
        );
        // Show the score fader first when it has keyboard focus.
        let selected = self.facts.and_then(|facts| {
            facts
                .faders
                .iter()
                .find(|fader| fader.selected)
                .map(|fader| format!("▶ {} {}", fader.label, fader.value))
                .or_else(|| {
                    let strip = facts.strips.iter().find(|strip| strip.selected)?;
                    Some(match (strip.kind, strip.gain_db) {
                        (MixerStripKind::Reduction { bypassed, .. }, Some(ceiling)) => {
                            let bypass = if bypassed { " (bypassed)" } else { "" };
                            format!(
                                "▶ limiter ceiling {ceiling:.1} dB · {}{bypass}",
                                strip.label
                            )
                        }
                        (_, Some(gain)) => {
                            let label = if strip.label == "in" {
                                "input"
                            } else {
                                &strip.label
                            };
                            format!("▶ {label} gain {gain:.1} dB")
                        }
                        (_, None) => {
                            format!("▶ {} meter {} dB", strip.label, db_text(strip.peak_db))
                        }
                    })
                })
        });
        if let Some(selected) = selected {
            let from = parts.rule.x + 11;
            if from < parts.rule.right() {
                buffer.set_stringn(
                    from,
                    parts.rule.y,
                    selected,
                    usize::from(parts.rule.right() - from),
                    style,
                );
            }
        }
    }

    fn draw_hint(&self, parts: &MixerParts, buffer: &mut Buffer) {
        // On the score's faders the arrows mean the other thing: they lie
        // down, so ← → move one and ↑ ↓ pick the next. Say which set is
        // live rather than making a performer find out.
        let on_faders = self
            .facts
            .is_some_and(|facts| facts.faders.iter().any(|fader| fader.selected));
        // The limiter has two keys no other strip answers, so they lead
        // the row while it holds the keys, where a narrow row cannot cut
        // them off.
        let on_limiter = self.facts.is_some_and(|facts| {
            facts.strips.iter().any(|strip| {
                strip.selected && matches!(strip.kind, MixerStripKind::Reduction { .. })
            })
        });
        let hide = self
            .keybinds
            .binding(super::keybinds::BindAction::Mixer)
            .map(|binding| format!("{} hides the mixer · ", binding.hint()))
            .unwrap_or_default();
        let unfocused_hint =
            format!("{hide}a press takes the keys · drag a fader, or the wheel over it");
        let hint = if self.selection.is_some() {
            // A selection in the devices block is the only copyable thing
            // on the desk; while it is held, this is what the keys mean.
            "c copies the selected text · drag the devices block to select again · Esc back"
        } else {
            match (self.focused, on_faders) {
                (true, true) => {
                    "←/→ move · ↑/↓ next fader · l learns a knob for it · Tab strips · Esc score"
                }
                // Remove leads with the other two: a limiter is added from the
                // menu and taken off from here, and a key that is nowhere on
                // the screen is a key nobody has.
                (true, false) if on_limiter => {
                    "c mode · b bypass · ⌫ removes it · ←/→ strip · ↑/↓ ceiling · 0 default · Tab the score's faders · Esc back"
                }
                (true, false) => {
                    "←/→ strip · ↑/↓ fader · Tab the score's faders · 0 unity · e top/bottom · +/- size · Esc back"
                }
                (false, _) => &unfocused_hint,
            }
        };
        buffer.set_stringn(
            parts.hint.x,
            parts.hint.y,
            hint,
            usize::from(parts.hint.width),
            Style::default().fg(self.theme.muted),
        );
    }

    /// The strips, left to right; the column after the last one drawn.
    fn draw_desk(&self, facts: &MixerFacts, desk: Rect, buffer: &mut Buffer) -> u16 {
        let theme = self.theme;
        let Some((meter_top, meter_bottom)) = meter_rows(desk) else {
            return desk.x;
        };
        let rows = meter_bottom - meter_top + 1;
        let travel = f32::from(rows.saturating_sub(1)).max(1.0);
        let row_of = |position: f32| (position * travel).round() as u16;
        let mut x = desk.x;
        for strip in &facts.strips {
            // Its own width, not everyone's: a reduction strip has less to
            // say and takes fewer columns to say it.
            let stride = strip_width(strip.kind);
            if x + stride > desk.right() + 1 {
                break;
            }
            // The label, inverted on the strip under the keys, in the
            // peak's colour on one that has clipped.
            let label_style = if strip.selected {
                Style::default()
                    .fg(theme.selection_text)
                    .bg(theme.selection)
                    .add_modifier(Modifier::BOLD)
            } else if strip.clipping {
                Style::default()
                    .fg(theme.meter.peak)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            };
            let width = usize::from(stride - 1);
            let label: String = strip.label.chars().take(width).collect();
            buffer.set_stringn(x, desk.y, format!("{label:<width$}"), width, label_style);
            // The meter: each row a level on the footer's scale, its
            // background the level's colour where the peak reaches and a
            // tint of it above; the knob, the held peak and the unity
            // mark on the column beside it.
            let level_rows = if strip.peak_db > METER_FLOOR_DB {
                scale_position(strip.peak_db) * travel
            } else {
                -1.0
            };
            let hold_row = strip
                .hold_db
                .filter(|hold| *hold > METER_FLOOR_DB)
                .map(|hold| row_of(scale_position(hold)));
            let fader_row = strip
                .gain_db
                .zip(strip.fader_range)
                .map(|(db, range)| row_of(fader_position(db, range)))
                // A limiter that is off has no ceiling to point at. Its
                // handle rests on the floor of its own rail, where a drag
                // from off starts, so the strip still has a handle to grab.
                .or_else(|| {
                    matches!(strip.kind, MixerStripKind::Reduction { .. }).then(|| row_of(0.0))
                });
            let unity_row = strip.fader_range.map_or_else(
                || row_of(scale_position(0.0)),
                |range| row_of(fader_position(0.0, range)),
            );
            // A reduction strip fills from the top down as the limiter
            // works, on the panel's own ground rather than the level ramp:
            // the opposite direction and a different colour, so it is never
            // mistaken for a level at a glance.
            let reduction_rows = match strip.kind {
                MixerStripKind::Reduction { reduction_db, .. } => {
                    let filled = (reduction_db.max(0.0) / REDUCTION_FULL_DB).clamp(0.0, 1.0);
                    Some((filled * f32::from(rows)).round() as u16)
                }
                MixerStripKind::Level => None,
            };
            for row in 0..rows {
                let y = meter_bottom - row;
                let bar = match reduction_rows {
                    // Row 0 is the bottom, so the fill grows downward by
                    // lighting the rows nearest the top first.
                    Some(filled) => Some(Style::default().bg(if rows - row <= filled {
                        theme.meter.peak
                    } else {
                        // Unlit rows draw the track, so an idle limiter's
                        // handle has a rail to sit on and lines up with
                        // the strips beside it.
                        theme.meter.track
                    })),
                    None => {
                        let db = scale_decibels(f32::from(row) / travel);
                        let colour = level_color(db, theme);
                        // See the note on the footer's bar: a muted strip
                        // meters silence, so its rail drops the tint of the
                        // colour it would have become and reads as switched
                        // off rather than as waiting for signal.
                        let trough = if strip.muted {
                            mix(theme.meter.track, theme.muted, 0.35)
                        } else {
                            mix(theme.meter.track, colour, 0.22)
                        };
                        let lit = f32::from(row) <= level_rows;
                        Some(Style::default().bg(if lit { colour } else { trough }))
                    }
                };
                if let Some(bar) = bar {
                    for column in meter_columns(strip.kind) {
                        if let Some(cell) = buffer.cell_mut((x + column, y)) {
                            cell.set_symbol(" ").set_style(bar);
                        }
                    }
                }
                // The cap, across its rail and one column past it either
                // side, so there is something to take hold of rather than
                // a mark to hit. Drawn as ground rather than a glyph: a
                // block reads as a cap at any size, and a pointer landing
                // anywhere on it has landed on the fader.
                if fader_row == Some(row) {
                    // Muted where the handle is resting rather than
                    // holding: a fader at its floor, and a limiter with no
                    // ceiling at all.
                    let resting = strip.muted
                        || strip.gain_db.is_none_or(|db| db <= METER_FLOOR_DB)
                        || matches!(strip.kind, MixerStripKind::Reduction { bypassed: true, .. });
                    let cap = if resting {
                        theme.muted
                    } else {
                        theme.meter.fader
                    };
                    // Blend the cap half over the ground it crosses. The
                    // row under a handle is the row you read while you
                    // move it, so the level and the track stay visible,
                    // and a cap over a lit meter differs from a cap over
                    // an unlit one.
                    for column in handle_columns(strip.kind) {
                        if let Some(cell) = buffer.cell_mut((x + column, y)) {
                            let behind = cell.style().bg.unwrap_or(theme.surface);
                            let blended =
                                super::theme::mix(behind, cap, super::theme::FADER_HANDLE_OPACITY);
                            cell.set_symbol(" ").set_style(Style::default().bg(blended));
                        }
                    }
                }
                let marker = if fader_row == Some(row) {
                    None
                } else if hold_row == Some(row) {
                    Some(("─", theme.meter.peak))
                } else if row == unity_row && reduction_rows.is_none() {
                    Some(("·", theme.rule))
                } else {
                    None
                };
                // Only a real marker is drawn, and it keeps whatever is
                // behind it: writing a blank here would punch a hole in the
                // bar it now sits on.
                if let Some((symbol, colour)) = marker
                    && let Some(cell) = buffer.cell_mut((x + marker_column(strip.kind), y))
                {
                    let behind = cell.style();
                    cell.set_symbol(symbol).set_style(behind.fg(colour));
                }
            }
            // The readout: the fader on a strip with one, the level on
            // one without, in the fader's colour so it reads as the
            // strip's number.
            let readout = match (strip.kind, strip.gain_db) {
                // The bypass switch, under the fader, because that is the
                // question the bottom of this strip answers. Bypassed, it
                // says so rather than reading `0.0` - a limiter switched
                // out and one switched in but idle both hold back nothing,
                // and they are not the same thing.
                (MixerStripKind::Reduction { bypassed: true, .. }, _) => "byp".to_owned(),
                // In, it reads as what it is taking off, which is the
                // number a person watches; its ceiling is on the handle.
                (MixerStripKind::Reduction { reduction_db, .. }, _) => {
                    if reduction_db >= 0.05 {
                        format!("-{reduction_db:.1}")
                    } else {
                        "0.0".to_owned()
                    }
                }
                (_, Some(db)) => gain_text(db),
                (_, None) => db_text(strip.peak_db),
            };
            // A bypassed limiter's line is a switch, not a reading: shown
            // as a chip so it reads as the thing Enter presses, and so the
            // one strip whose bottom row you can act on looks like it.
            let bypassed = matches!(strip.kind, MixerStripKind::Reduction { bypassed: true, .. });
            let readout_style = if bypassed {
                Style::default()
                    .fg(theme.selection_text)
                    .bg(theme.muted)
                    .add_modifier(Modifier::BOLD)
            } else if strip.gain_db.is_some() {
                Style::default().fg(theme.meter.fader)
            } else {
                Style::default().fg(theme.muted)
            };
            buffer.set_stringn(
                x,
                desk.bottom() - 1,
                format!("{readout:<width$}"),
                width,
                readout_style,
            );
            x += stride;
        }
        x
    }

    /// The devices beside the strips: lit while they speak, and what
    /// they said under the lights.
    /// The evaluated score's sliders, one to a row, down a column then
    /// across. Returns the x the next block may start at.
    fn draw_faders(&self, facts: &MixerFacts, desk: Rect, from_x: u16, buffer: &mut Buffer) -> u16 {
        let theme = self.theme;
        let Some(layout) = FaderDesk::new(desk, from_x + 1, facts.faders.len()) else {
            return from_x;
        };
        for (at, fader) in facts.faders.iter().enumerate() {
            let Some(cell) = layout.cell(at) else {
                // Past what the desk can show. The rest are still there
                // and still driveable; there is simply no room to say so.
                break;
            };
            let label_style = if fader.selected {
                Style::default()
                    .fg(theme.selection_text)
                    .bg(theme.selection)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.foreground)
            };
            // The slot that drives it, where one does: a performer needs to
            // know which knob is this fader's before reaching for it.
            let name = match fader.slot {
                Some(slot) => format!("{} {}", slot + 1, fader.label),
                None => format!("  {}", fader.label),
            };
            buffer.set_stringn(cell.x, cell.y, &name, usize::from(FADER_LABEL), label_style);
            // What it reads now, hard against the right of its own row, so
            // a column of faders lines its numbers up.
            let reading = fader.value.chars().rev().take(8).collect::<Vec<_>>();
            let reading: String = reading.into_iter().rev().collect();
            let value_x = cell
                .right()
                .saturating_sub(reading.chars().count() as u16)
                .max(cell.x + FADER_LABEL);
            buffer.set_stringn(
                value_x,
                cell.y,
                &reading,
                usize::from(cell.right().saturating_sub(value_x)),
                Style::default()
                    .fg(theme.foreground)
                    .add_modifier(Modifier::BOLD),
            );
            if let Some(rail) = FaderDesk::rail(cell) {
                // The ends of its range, one either side of the rail, so
                // what the travel means is on the fader rather than
                // somewhere else.
                let ends = Style::default().fg(theme.muted);
                buffer.set_stringn(cell.x, rail.y, &fader.min, usize::from(FADER_END), ends);
                buffer.set_stringn(
                    rail.right() + 1,
                    rail.y,
                    &fader.max,
                    usize::from(FADER_END),
                    ends,
                );
                let cells = usize::from(rail.width);
                let knob = ((f64::from(fader.notch) * (cells - 1) as f64).round() as usize)
                    .min(cells.saturating_sub(1));
                // The same glyphs as the pill in the score: a thin rail and
                // a full block for the handle, so the two read as one
                // control seen twice rather than two controls.
                let rail_text: String = (0..cells)
                    .map(|position| if position == knob { '█' } else { '─' })
                    .collect();
                let mut style = Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD);
                if fader.selected {
                    style = style.bg(theme.selection);
                    if theme.accent == theme.selection {
                        style = style.fg(theme.selection_text);
                    }
                }
                buffer.set_stringn(rail.x, rail.y, &rail_text, cells, style);
            }
        }
        layout.area.right()
    }

    fn draw_devices(&self, facts: &MixerFacts, desk: Rect, from_x: u16, buffer: &mut Buffer) {
        let theme = self.theme;
        let devices_x = from_x + 1;
        if devices_x + DEVICES_MIN_WIDTH > desk.right() {
            return;
        }
        let width = usize::from(desk.right() - devices_x);
        let rows = devices_lines(facts, desk, theme);
        for (row, (line, style)) in rows.iter().enumerate() {
            buffer.set_stringn(devices_x, desk.y + row as u16, line, width, *style);
        }
        // A held drag is painted while the rows it was made on are still
        // the rows on screen: the log keeps scrolling, and a band that
        // stayed behind would say it selects text that has moved on.
        if let Some(held) = self.selection
            && held.area == Rect::new(devices_x, desk.y, desk.right() - devices_x, desk.height)
            && rows
                .iter()
                .map(|(line, _)| line.as_str())
                .eq(held.lines.iter().map(String::as_str))
        {
            for (row, (line, _)) in rows.iter().enumerate() {
                let chars = line.chars().count();
                if let Some(columns) = held.selection.columns_on(row, chars) {
                    let first = devices_x + columns.start as u16;
                    let last = devices_x + columns.end as u16;
                    for x in first..last.min(desk.right()) {
                        if let Some(cell) = buffer.cell_mut((x, desk.y + row as u16)) {
                            cell.set_style(cell.style().bg(theme.selection));
                        }
                    }
                }
            }
        }
    }
}

/// The rows the devices block draws, in draw order: the port and pad
/// status lines, the pads' doings, then the last pad presses and stick
/// moves, then the last MIDI messages - both newest last, as many as fit.
fn devices_lines(facts: &MixerFacts, desk: Rect, theme: &Theme) -> Vec<(String, Style)> {
    let lit = |on: bool| {
        if on {
            Style::default().fg(theme.ok).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        }
    };
    let midi = super::view::midi_chip_text(facts.midi_ports);
    let pad_glyph = super::terminal::symbol("▣");
    let pads = match facts.pads {
        0 => format!("{pad_glyph} no gamepad"),
        1 => format!("{pad_glyph} 1 gamepad"),
        many => format!("{pad_glyph} {many} gamepads"),
    };
    let mut rows = Vec::new();
    let mut y = desk.y;
    rows.push((midi, lit(facts.midi_active)));
    y += 1;
    if y >= desk.bottom() {
        return rows;
    }
    rows.push((pads, lit(facts.pad_active)));
    y += 1;
    // The pads first - a line each, few - then their own recent presses
    // and stick moves, then the last MIDI messages, newest at the bottom
    // of each, as many as fit.
    for line in &facts.pads_live {
        if y >= desk.bottom() {
            return rows;
        }
        rows.push((line.clone(), Style::default().fg(theme.foreground)));
        y += 1;
    }
    let gamepad_room = usize::from(desk.bottom().saturating_sub(y));
    let gamepad_from = facts.gamepad_recent.len().saturating_sub(gamepad_room);
    let gamepad_shown = &facts.gamepad_recent[gamepad_from..];
    for (age, line) in gamepad_shown.iter().enumerate() {
        if y >= desk.bottom() {
            return rows;
        }
        let newest = age + 1 == gamepad_shown.len();
        rows.push((
            line.clone(),
            Style::default().fg(if newest {
                theme.foreground
            } else {
                theme.muted
            }),
        ));
        y += 1;
    }
    let room = usize::from(desk.bottom().saturating_sub(y));
    let recent = facts.midi_recent.len().saturating_sub(room);
    let shown = &facts.midi_recent[recent..];
    for (age, line) in shown.iter().enumerate() {
        if y >= desk.bottom() {
            return rows;
        }
        let newest = age + 1 == shown.len();
        rows.push((
            line.clone(),
            Style::default().fg(if newest {
                theme.foreground
            } else {
                theme.muted
            }),
        ));
        y += 1;
    }
    rows
}

/// The devices block on the drawn desk: where its rows stand and the
/// rows themselves, for hit tests and copying. `None` when the desk is
/// too narrow to draw the block at all.
pub fn devices_block(
    facts: &MixerFacts,
    desk: Rect,
    widths: &[u16],
    faders: usize,
    theme: &Theme,
) -> Option<(Rect, Vec<String>)> {
    let after = strips_end(desk, widths);
    let from_x = fader_desk(desk, widths, faders).map_or(after, |layout| layout.area.right());
    let devices_x = from_x + 1;
    if devices_x + DEVICES_MIN_WIDTH > desk.right() {
        return None;
    }
    let area = Rect::new(devices_x, desk.y, desk.right() - devices_x, desk.height);
    let lines = devices_lines(facts, desk, theme)
        .into_iter()
        .map(|(line, _)| line)
        .collect();
    Some((area, lines))
}

impl Widget for MixerPanelView<'_> {
    fn render(self, _area: Rect, buffer: &mut Buffer) {
        let Some(parts) = parts(self.area, self.panel.edge) else {
            return;
        };
        let theme = self.theme;
        super::view::clear_surface(
            buffer,
            self.area,
            Style::default().bg(theme.background).fg(theme.foreground),
        );
        self.draw_rule(&parts, buffer);
        self.draw_hint(&parts, buffer);
        let Some(facts) = self.facts else {
            buffer.set_stringn(
                parts.desk.x,
                parts.desk.y,
                "nothing to mix yet",
                usize::from(parts.desk.width),
                Style::default().fg(theme.muted),
            );
            return;
        };
        let after = self.draw_desk(facts, parts.desk, buffer);
        // The score's own faders sit between the strips and the device
        // block: they are what a performer reaches for, and the block
        // beyond them is a status readout that can be the thing to lose
        // when a terminal is narrow.
        let after = self.draw_faders(facts, parts.desk, after, buffer);
        self.draw_devices(facts, parts.desk, after, buffer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viz_panel::{MixerFader, MixerStrip};

    fn row(buffer: &Buffer, area: Rect, y: u16) -> String {
        (area.x..area.right())
            .map(|x| buffer.cell((x, y)).unwrap().symbol().to_owned())
            .collect()
    }

    fn facts() -> MixerFacts {
        MixerFacts {
            strips: vec![
                MixerStrip {
                    label: "in".into(),
                    detail: "Mic".into(),
                    peak_db: -18.0,
                    hold_db: None,
                    gain_db: Some(30.0),
                    fader_range: Some((-60.0, 48.0)),
                    clipping: false,
                    muted: false,
                    selected: true,
                    kind: MixerStripKind::Level,
                },
                MixerStrip {
                    label: "orbit 12".into(),
                    detail: String::new(),
                    peak_db: METER_FLOOR_DB,
                    hold_db: None,
                    gain_db: None,
                    fader_range: None,
                    clipping: false,
                    muted: false,
                    selected: false,
                    kind: MixerStripKind::Level,
                },
                MixerStrip {
                    label: "master".into(),
                    detail: String::new(),
                    peak_db: -2.0,
                    hold_db: Some(-1.0),
                    gain_db: Some(-12.0),
                    fader_range: Some((-60.0, 6.0)),
                    clipping: false,
                    muted: false,
                    selected: false,
                    kind: MixerStripKind::Level,
                },
            ],
            midi_ports: crate::devices::MidiPortCounts {
                outputs: 1,
                inputs: 1,
            },
            midi_active: true,
            midi_recent: vec![
                "MiniLab · note C4 v100 ch1".into(),
                "MiniLab · cc74 63 ch2".into(),
            ],
            pads: 1,
            pad_active: false,
            pads_live: vec!["gamepad(0) Controller · a · x1 +0.72".into()],
            gamepad_recent: vec!["gamepad(0) Controller: a pressed".into()],
            faders: Vec::new(),
        }
    }

    /// The limiter's own keys are on the hint row while it holds the keys,
    /// and nowhere else: `c` and `b` do nothing on any other strip.
    #[test]
    fn the_hint_names_the_limiters_keys_when_it_is_selected() {
        let hint = |facts: &MixerFacts| {
            let area = Rect::new(0, 0, 120, 16);
            let mut buffer = Buffer::empty(area);
            MixerPanelView {
                keybinds: &crate::keybinds::Keybinds::default(),
                facts: Some(facts),
                panel: MixerPanel::new(MixerEdge::Bottom, Some(10)),
                theme: &Theme::built_in_default(),
                focused: true,
                area,
                selection: None,
            }
            .render(area, &mut buffer);
            let parts = parts(area, MixerEdge::Bottom).expect("parts");
            row(&buffer, area, parts.hint.y)
        };

        let mut facts = facts();
        let on_input = hint(&facts);
        assert!(!on_input.contains("b bypass"), "{on_input}");

        facts.strips[0].selected = false;
        facts.strips[1].selected = true;
        facts.strips[1].kind = MixerStripKind::Reduction {
            reduction_db: 3.0,
            bypassed: false,
        };
        let on_limiter = hint(&facts);
        assert!(
            on_limiter.trim_start().starts_with("c mode · b bypass"),
            "{on_limiter}"
        );
    }

    #[test]
    fn the_rule_names_the_selected_control_before_an_arrow_moves_it() {
        let rule = |facts: &MixerFacts, width| {
            let area = Rect::new(0, 0, width, 16);
            let mut buffer = Buffer::empty(area);
            MixerPanelView {
                keybinds: &crate::keybinds::Keybinds::default(),
                facts: Some(facts),
                panel: MixerPanel::new(MixerEdge::Bottom, Some(10)),
                theme: &Theme::built_in_default(),
                focused: true,
                area,
                selection: None,
            }
            .render(area, &mut buffer);
            let parts = parts(area, MixerEdge::Bottom).expect("parts");
            row(&buffer, area, parts.rule.y)
        };

        let mut facts = facts();
        assert!(rule(&facts, 40).contains("▶ input gain 30.0 dB"));

        facts.strips[0].selected = false;
        facts.strips[2].selected = true;
        facts.strips[2].gain_db = Some(-10.0);
        assert!(rule(&facts, 40).contains("▶ master gain -10.0 dB"));

        facts.strips[2].selected = false;
        facts.strips[1].selected = true;
        facts.strips[1].label = "punch".into();
        facts.strips[1].gain_db = Some(-1.0);
        facts.strips[1].kind = MixerStripKind::Reduction {
            reduction_db: 3.0,
            bypassed: false,
        };
        assert!(
            rule(&facts, 40).contains("▶ limiter ceiling -1.0 dB"),
            "{}",
            rule(&facts, 40)
        );
        assert!(rule(&facts, 80).contains("· punch"));
    }

    fn fader(label: &str, notch: f32, selected: bool, slot: Option<usize>) -> MixerFader {
        MixerFader {
            label: label.into(),
            notch,
            value: "800".into(),
            min: "100".into(),
            max: "4000".into(),
            selected,
            slot,
        }
    }

    /// The desk shows each score fader beside the strips: its name, its
    /// reading on its own row, and both ends of its range beside the rail.
    #[test]
    fn the_desk_shows_the_scores_faders_with_their_range() {
        let mut facts = facts();
        facts.faders = vec![
            fader("lpf", 0.25, true, Some(0)),
            fader("room", 0.6, false, Some(1)),
            fader("gain", 0.9, false, None),
        ];
        let area = Rect::new(0, 0, 96, 10);
        let mut buffer = Buffer::empty(area);
        MixerPanelView {
            keybinds: &crate::keybinds::Keybinds::default(),
            facts: Some(&facts),
            panel: MixerPanel::new(MixerEdge::Bottom, Some(10)),
            theme: &Theme::built_in_default(),
            focused: true,
            area,
            selection: None,
        }
        .render(area, &mut buffer);
        let text = (0..area.height)
            .map(|y| row(&buffer, area, y))
            .collect::<Vec<_>>()
            .join("\n");

        // Named, and numbered by the slot that drives it. A fader with no
        // slot keeps its name and says nothing about a knob.
        assert!(text.contains("1 lpf"), "{text}");
        assert!(text.contains("2 room"), "{text}");
        assert!(text.contains("  gain"), "{text}");
        // Reading and range, all three on the fader itself.
        assert!(text.contains("800"), "{text}");
        assert!(text.contains("100"), "{text}");
        assert!(text.contains("4000"), "{text}");
        // The knob moves with the value, and the rail is the score's own
        // glyphs so the two read as one control seen twice.
        let rails: Vec<&str> = text.lines().filter(|line| line.contains('█')).collect();
        assert_eq!(rails.len(), 3, "one rail a fader: {text}");
        let knob_at = |line: &str| line.find('█').unwrap_or(0);
        assert!(
            knob_at(rails[0]) < knob_at(rails[1]) && knob_at(rails[1]) < knob_at(rails[2]),
            "a louder fader stands further along: {rails:?}"
        );

        // It sits between the strips and the devices, and takes room from
        // the devices rather than from the strips.
        let strips = text.find("master").expect("the strips are drawn");
        let desk = text.find("1 lpf").expect("the faders are drawn");
        let devices = text.find("1 out · 1 in").expect("the devices are drawn");
        assert!(strips < desk && desk < devices, "{text}");
    }

    /// Every fader reaches both ends of its own rail. The limiter's ceiling
    /// uses its own range, not the footer's -60..+6 scale.
    #[test]
    fn a_fader_reaches_both_ends_of_its_own_rail() {
        let desk = Rect::new(0, 0, 40, 12);
        let (top, bottom) = meter_rows(desk).expect("meter rows");
        let travel = f32::from(bottom - top);
        for (name, range) in [
            ("limiter", (-24.0f32, 0.0f32)),
            ("master", (METER_FLOOR_DB, 6.0f32)),
            ("input", (-60.0f32, 48.0f32)),
        ] {
            let (low, high) = range;
            // A drag to either end asks for that end.
            assert!(
                (fader_db_within(desk, bottom, 0.0, range) - low).abs() < 0.01,
                "{name}: the bottom row is not its floor"
            );
            assert!(
                (fader_db_within(desk, top, 0.0, range) - high).abs() < 0.01,
                "{name}: the top row is not its ceiling"
            );
            // And the handle for either end is drawn on that end's row.
            assert!(
                (fader_position(low, range) * travel).round() as u16 == 0,
                "{name}: its floor does not sit on the bottom row"
            );
            assert!(
                (fader_position(high, range) * travel).round() as u16 == bottom - top,
                "{name}: its ceiling does not sit on the top row"
            );
        }
    }

    /// A fader resolves finer than the row it is drawn in.
    ///
    /// A strip is a column, so a row was the whole of its resolution: a
    /// desk ten rows tall gave the master's hundred-and-something decibels
    /// of travel ten stops, which is not a fader you can set. A terminal
    /// that reports pixels knows where in the row the pointer is, so the
    /// value comes from there while the picture stays on the cell grid.
    #[test]
    fn a_fader_reads_where_in_the_row_the_pointer_was() {
        let desk = Rect::new(0, 0, 40, 12);
        let (top, bottom) = meter_rows(desk).expect("meter rows");
        let range = (-60.0f32, 6.0f32);
        let row = top + (bottom - top) / 2;

        // Across one row, the value moves the whole of that row's worth.
        let at_top = fader_db_within(desk, row, 0.0, range);
        let mut previous = at_top;
        for step in 1..8 {
            let db = fader_db_within(desk, row, f64::from(step) / 8.0, range);
            assert!(
                db < previous,
                "further down the row is quieter: {db} after {previous}"
            );
            previous = db;
        }
        // And a whole row down lands where the next row's top does, so the
        // fraction and the row are one continuous travel rather than two
        // scales that meet at a seam.
        let next_row = fader_db_within(desk, row + 1, 0.0, range);
        assert!(
            (previous - next_row).abs() < (at_top - next_row).abs(),
            "the bottom of a row is nearer the next row than its own top"
        );

        // A terminal that reports cells passes nought and every press
        // means exactly what it did.
        assert_eq!(
            fader_db_within(desk, row, 0.0, range),
            fader_db_at(desk, row, range)
        );

        // The ends stay the ends: a fraction cannot push past them.
        assert_eq!(
            fader_db_within(desk, bottom, 0.999, range),
            fader_db_within(desk, bottom, 0.0, range),
            "below the floor is still the floor"
        );
        let ceiling = fader_db_within(desk, top, 0.0, range);
        assert!((ceiling - range.1).abs() < 1e-6, "the top row is the top");
    }

    /// A narrow strip beside wide ones: the drawing and the hit test agree
    /// about every column, and the press on the strip after it lands on
    /// that strip rather than on its neighbour.
    #[test]
    fn a_narrow_strip_does_not_shift_the_presses_after_it() {
        let mut facts = facts();
        facts.strips[1].kind = MixerStripKind::Reduction {
            reduction_db: 3.0,
            bypassed: false,
        };
        let widths = strip_widths(&facts.strips);
        assert_eq!(
            widths,
            vec![STRIP_WIDTH, REDUCTION_STRIP_WIDTH, STRIP_WIDTH],
            "the reduction strip is the narrow one"
        );

        let area = Rect::new(0, 0, 96, 10);
        let mut buffer = Buffer::empty(area);
        let view = MixerPanelView {
            keybinds: &crate::keybinds::Keybinds::default(),
            facts: Some(&facts),
            panel: MixerPanel::new(MixerEdge::Bottom, Some(10)),
            theme: &Theme::built_in_default(),
            focused: true,
            area,
            selection: None,
        };
        let parts = parts(area, MixerEdge::Bottom).expect("a desk");
        let drawn = view.draw_desk(&facts, parts.desk, &mut buffer);
        assert_eq!(
            drawn,
            strips_end(parts.desk, &widths),
            "the strips end where the hit test thinks"
        );

        // Every column of every strip answers with that strip, and the gap
        // at its right answers with nobody.
        let mut left = parts.desk.x;
        for (index, width) in widths.iter().enumerate() {
            for column in 0..width - 1 {
                assert_eq!(
                    strip_at(parts.desk, &widths, left + column),
                    Some(index),
                    "column {column} of strip {index}"
                );
            }
            assert_eq!(
                strip_at(parts.desk, &widths, left + width - 1),
                None,
                "the gap after strip {index}"
            );
            left += width;
        }
        // And the third strip really did move left by what the second
        // gave up - the whole point of narrowing it.
        assert_eq!(left, parts.desk.x + STRIP_WIDTH * 2 + REDUCTION_STRIP_WIDTH);

        // All of which agrees the walk with itself. What settles it is the
        // buffer: each strip's own name has to be drawn inside the columns
        // the hit test hands to that strip, or the drawing and the pointer
        // are laying the desk out differently and both halves above would
        // still pass.
        let columns: Vec<Option<usize>> = (parts.desk.x..parts.desk.x + parts.desk.width)
            .map(|x| strip_at(parts.desk, &widths, x))
            .collect();
        let rows: Vec<String> = (parts.desk.y..parts.desk.y + parts.desk.height)
            .map(|y| {
                (parts.desk.x..parts.desk.x + parts.desk.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<Vec<_>>()
                    .concat()
            })
            .collect();
        for (index, strip) in facts.strips.iter().enumerate() {
            // A strip draws as much of its name as its width holds, so
            // "orbit 12" in a nine-column strip is "orbit".
            let width = usize::from(widths[index]);
            let name: String = strip.label.chars().take(width - 1).collect();
            let mut found = false;
            for row in &rows {
                let Some(at) = row.find(&name) else {
                    continue;
                };
                found = true;
                // `find` is a byte offset and a desk is drawn with box
                // characters, so count columns rather than bytes.
                let start = row[..at].chars().count();
                for (column, drawn) in columns
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(name.chars().count())
                {
                    assert_eq!(
                        *drawn,
                        Some(index),
                        "{name:?} is drawn at column {column}, which the pointer gives away"
                    );
                }
            }
            assert!(found, "{name:?} was never drawn:\n{}", rows.join("\n"));
        }
    }

    /// The pointer is offered the block that was drawn: `strips_end` agrees
    /// with the drawing at every width, including widths that cut a strip off.
    #[test]
    fn the_faders_are_offered_to_the_pointer_where_they_are_drawn() {
        let mut facts = facts();
        facts.faders = vec![
            fader("lpf", 0.25, true, Some(0)),
            fader("room", 0.6, false, Some(1)),
        ];
        // A narrow strip among wide ones, and widths that cut a strip off:
        // the two cases a hit test that divides gets wrong.
        facts.strips[1].kind = MixerStripKind::Reduction {
            reduction_db: 3.0,
            bypassed: false,
        };
        let widths = strip_widths(&facts.strips);
        for width in 20..=120u16 {
            let area = Rect::new(0, 0, width, 10);
            let mut buffer = Buffer::empty(area);
            let view = MixerPanelView {
                keybinds: &crate::keybinds::Keybinds::default(),
                facts: Some(&facts),
                panel: MixerPanel::new(MixerEdge::Bottom, Some(10)),
                theme: &Theme::built_in_default(),
                focused: true,
                area,
                selection: None,
            };
            let Some(parts) = parts(area, MixerEdge::Bottom) else {
                continue;
            };
            let drawn = view.draw_desk(&facts, parts.desk, &mut buffer);
            assert_eq!(
                drawn,
                strips_end(parts.desk, &widths),
                "the strips end elsewhere than the hit test thinks, at {width} columns"
            );
            // Every column of every strip the desk had room to draw
            // answers with that strip, at every width - including the
            // ones where a later strip was cut off entirely.
            let mut left = parts.desk.x;
            for (index, stride) in widths.iter().enumerate() {
                if left + stride > parts.desk.right() + 1 {
                    // Cut off: nothing here is pressable, and nothing
                    // beyond it is either.
                    for column in left..parts.desk.right() {
                        assert_eq!(
                            strip_at(parts.desk, &widths, column),
                            None,
                            "a strip the desk could not draw took a press at {width} columns"
                        );
                    }
                    break;
                }
                for column in 0..stride - 1 {
                    assert_eq!(
                        strip_at(parts.desk, &widths, left + column),
                        Some(index),
                        "column {column} of strip {index} at {width} columns"
                    );
                }
                left += stride;
            }
            // And where a fader is drawn, the desk hands the same fader back.
            if let Some(layout) =
                fader_desk(parts.desk, &strip_widths(&facts.strips), facts.faders.len())
            {
                for at in 0..facts.faders.len() {
                    let Some(cell) = layout.cell(at) else {
                        continue;
                    };
                    assert_eq!(
                        layout.at(cell.x, cell.y, facts.faders.len()),
                        Some(at),
                        "fader {at} is not under its own cell at {width} columns"
                    );
                    if let Some(rail) = FaderDesk::rail(cell) {
                        assert!(cell.contains(ratatui::layout::Position::new(rail.x, rail.y)));
                    }
                }
            }
        }
    }

    /// Along the bottom the rule is the top row with the title on it, the
    /// hint the last row, the desk between; along the top, the other way
    /// up. Too short a band is no panel.
    #[test]
    fn the_parts_follow_the_edge() {
        let area = Rect::new(0, 20, 100, 16);
        let bottom = parts(area, MixerEdge::Bottom).expect("parts");
        assert_eq!(bottom.rule, Rect::new(0, 20, 100, 1));
        assert_eq!(bottom.desk, Rect::new(1, 21, 98, 14));
        assert_eq!(bottom.hint, Rect::new(1, 35, 98, 1));
        let top = parts(area, MixerEdge::Top).expect("parts");
        assert_eq!(top.rule.y, 35);
        assert_eq!(top.hint.y, 20);
        assert_eq!(top.desk, Rect::new(1, 21, 98, 14));
        assert!(parts(Rect::new(0, 0, 100, 4), MixerEdge::Bottom).is_none());
        let mut panel = MixerPanel::new(MixerEdge::Bottom, None);
        assert_eq!(panel.rows, DEFAULT_ROWS);
        assert!(panel.resize(true));
        assert_eq!(panel.rows, DEFAULT_ROWS + 1);
        panel.rows = MAX_ROWS;
        assert!(!panel.resize(true), "no taller than the ceiling");
        assert_eq!(MixerPanel::new(MixerEdge::Top, Some(2)).rows, MIN_ROWS);
        assert_eq!(panel.dock().extent, MAX_ROWS);
        assert_eq!(MixerEdge::Bottom.flipped(), MixerEdge::Top);
        assert_eq!(MixerEdge::from_top(true).name(), "top");
    }

    /// Strips stand side by side with the orbit's number on its label, a
    /// meter up each lit to its level, a knob on the strips with a fader
    /// and none on an orbit's, the readout under; the devices beside them,
    /// the pads' doings and the last MIDI messages readable.
    #[test]
    fn the_desk_draws_strips_and_the_devices_beside_them() {
        let facts = facts();
        let theme = Theme::built_in_default();
        let area = Rect::new(0, 0, 90, 12);
        let mut buffer = Buffer::empty(area);
        MixerPanelView {
            keybinds: &crate::keybinds::Keybinds::default(),
            facts: Some(&facts),
            panel: MixerPanel::new(MixerEdge::Bottom, Some(12)),
            theme: &theme,
            focused: true,
            area,
            selection: None,
        }
        .render(area, &mut buffer);
        let parts = parts(area, MixerEdge::Bottom).expect("parts");
        assert!(
            row(&buffer, area, 0).contains("─ mixer ─"),
            "{}",
            row(&buffer, area, 0)
        );
        let labels = row(&buffer, parts.desk, parts.desk.y);
        assert!(labels.starts_with("in      "), "{labels}");
        assert!(labels.contains("orbit 12"), "the whole number: {labels}");
        assert!(labels.contains("master"), "{labels}");
        let readouts = row(&buffer, parts.desk, parts.desk.bottom() - 1);
        assert!(
            readouts.starts_with("+30.0"),
            "the input's fader: {readouts}"
        );
        assert!(
            readouts.contains("-inf"),
            "the silent orbit's level: {readouts}"
        );
        assert!(readouts.contains("-12.0"), "the master's fader: {readouts}");
        let (top, bottom) = meter_rows(parts.desk).expect("meter rows");
        let column = |x: u16| -> String {
            (top..=bottom)
                .map(|y| buffer.cell((x, y)).unwrap().symbol().to_owned())
                .collect()
        };
        // The cap is background, not a glyph, across the rail and one column
        // either side. At half opacity it blends with the ground, so the
        // capped row is the one whose ground differs from every other row's.
        let bg = |x: u16, y: u16| buffer.cell((x, y)).unwrap().style().bg;
        let cap = |x: u16| -> bool {
            let rows: Vec<_> = (top..=bottom).map(|y| bg(x, y)).collect();
            // Exactly one row differs from the column's most common
            // ground, and it is not the cap's flat colour - which is what
            // a solid block would have left.
            rows.iter().any(|found| {
                *found != Some(theme.meter.fader)
                    && rows.iter().filter(|other| *other == found).count() == 1
            })
        };
        let capped = |strip: u16| -> Vec<u16> {
            let left = parts.desk.x + strip * STRIP_WIDTH;
            (0..STRIP_WIDTH)
                .filter(|column| cap(left + column))
                .collect()
        };
        assert_eq!(
            capped(0),
            (handle_columns(MixerStripKind::Level)).collect::<Vec<_>>(),
            "the input's cap covers its rail and one column either side"
        );
        assert!(capped(1).is_empty(), "an orbit has no fader");
        assert_eq!(capped(2), capped(0), "and so does the master's");
        // And the cap lets its ground through rather than replacing it:
        // no cell on it is the flat cap colour.
        let master_left = parts.desk.x + 2 * STRIP_WIDTH;
        assert!(
            (top..=bottom).all(|y| {
                handle_columns(MixerStripKind::Level)
                    .all(|column| bg(master_left + column, y) != Some(theme.meter.fader))
            }),
            "a solid cap would have hidden the meter row it sits on"
        );
        // The held peak is still a mark, on the middle of the track.
        let marker = |strip: u16| column(parts.desk.x + strip * STRIP_WIDTH + MARKER_COLUMN);
        assert!(marker(2).contains('─'), "the master's held peak");
        // The devices beside the strips.
        let devices: Vec<String> = (parts.desk.y..parts.desk.bottom())
            .map(|y| row(&buffer, parts.desk, y))
            .collect();
        assert!(devices[0].contains("⌁ 1 out · 1 in"), "{}", devices[0]);
        assert!(devices[1].contains("▣ 1 gamepad"), "{}", devices[1]);
        assert!(devices[2].contains("gamepad(0) Controller · a · x1 +0.72"));
        assert!(
            devices
                .join("\n")
                .contains("gamepad(0) Controller: a pressed")
        );
        assert!(devices.join("\n").contains("cc74 63 ch2"));
        assert!(
            row(&buffer, area, 11).contains("↑/↓ fader"),
            "{}",
            row(&buffer, area, 11)
        );
        assert_eq!(gain_text(0.0), "0.0 dB");
        assert_eq!(gain_text(-60.0), "-inf");
        assert_eq!(db_text(-12.4), "-12");
    }

    /// Conhost draws `⌁` and `▣` as tofu, so the devices block falls back
    /// to their plain stand-ins instead - see `terminal::symbol`.
    #[test]
    fn the_devices_lines_fall_back_without_the_capability() {
        let _forced = super::super::terminal::ForceSymbolsForTest::set(false);
        let theme = Theme::built_in_default();
        let desk = Rect::new(0, 0, 40, 10);
        let lines = super::devices_lines(&facts(), desk, &theme);
        assert!(lines[0].0.contains("~ 1 out · 1 in"), "{}", lines[0].0);
        assert!(!lines[0].0.contains('⌁'), "{}", lines[0].0);
        assert!(lines[1].0.contains('#'), "{}", lines[1].0);
        assert!(!lines[1].0.contains('▣'), "{}", lines[1].0);
    }

    /// A column maps to its strip and the gap to none. A row maps to a level
    /// on the strip's own scale: the footer's scale for the master, with the
    /// detent at unity, and a straight run to +48 for the input.
    #[test]
    fn a_point_on_the_desk_is_a_strip_and_a_level() {
        let desk = Rect::new(10, 5, 40, 14);
        assert_eq!(strip_at(desk, &[STRIP_WIDTH; 8], 10), Some(0));
        assert_eq!(strip_at(desk, &[STRIP_WIDTH; 8], 17), Some(0));
        assert_eq!(strip_at(desk, &[STRIP_WIDTH; 8], 18), None, "the gap");
        assert_eq!(strip_at(desk, &[STRIP_WIDTH; 8], 19), Some(1));
        assert_eq!(strip_at(desk, &[STRIP_WIDTH; 8], 9), None);
        assert_eq!(strip_at(desk, &[STRIP_WIDTH; 8], 50), None);
        // A desk 40 wide draws four strips (36 columns); the last four
        // columns are nobody's, not a fifth strip's.
        assert_eq!(strip_at(desk, &[STRIP_WIDTH; 8], desk.x + 34), Some(3));
        assert_eq!(
            strip_at(desk, &[STRIP_WIDTH; 8], desk.x + 35),
            None,
            "strip 3's gap"
        );
        assert_eq!(strip_at(desk, &[STRIP_WIDTH; 8], desk.x + 36), None);
        assert_eq!(strip_at(desk, &[STRIP_WIDTH; 8], desk.x + 39), None);
        let (top, bottom) = meter_rows(desk).expect("rows");
        assert_eq!((top, bottom), (6, 17));
        let master = (-60.0, 6.0);
        assert!(
            fader_db_at(desk, bottom, master) <= -60.0,
            "the bottom row is silence"
        );
        assert!(
            fader_db_at(desk, top, master) >= 5.9,
            "the top row is the ceiling"
        );
        assert!(
            fader_db_at(desk, bottom + 5, master) <= -60.0,
            "below the desk is the bottom"
        );
        let unity_row = bottom - (scale_position(0.0) * f32::from(bottom - top)).round() as u16;
        assert_eq!(
            fader_db_at(desk, unity_row, master),
            0.0,
            "the detent at unity"
        );
        let input = (-60.0, 48.0);
        assert_eq!(
            fader_db_at(desk, top, input),
            48.0,
            "the input climbs to its own top"
        );
        assert!(fader_db_at(desk, bottom, input) <= -60.0);
        let unity_row =
            bottom - (fader_position(0.0, input) * f32::from(bottom - top)).round() as u16;
        assert_eq!(
            fader_db_at(desk, unity_row, input),
            0.0,
            "and has a detent at unity too"
        );
        assert!(fader_position(30.0, input) > fader_position(0.0, input));
        assert_eq!(fader_position(0.0, master), scale_position(0.0));
    }

    #[test]
    fn unfocused_footer_tracks_mixer_rebinding_and_unbinding() {
        use crate::keybinds::{BindAction, KeyCombo, Keybinds};
        let draw = |bindings: &Keybinds| {
            let area = Rect::new(0, 0, 120, 16);
            let mut buffer = Buffer::empty(area);
            MixerPanelView {
                keybinds: bindings,
                facts: None,
                panel: MixerPanel::new(MixerEdge::Bottom, Some(10)),
                theme: &Theme::built_in_default(),
                focused: false,
                area,
                selection: None,
            }
            .render(area, &mut buffer);
            row(
                &buffer,
                area,
                parts(area, MixerEdge::Bottom).unwrap().hint.y,
            )
        };
        let mut bindings = Keybinds::default();
        bindings.learn(BindAction::Mixer, KeyCombo::parse("f2"));
        assert!(draw(&bindings).contains("F2 hides"));
        assert!(!draw(&bindings).contains("F4 hides"));
        bindings.unbind(BindAction::Mixer);
        assert!(!draw(&bindings).contains("hides the mixer"));
    }

    #[test]
    fn the_rule_names_the_active_score_fader_while_a_strip_keeps_its_selection() {
        let mut facts = facts();
        facts.faders = vec![
            fader("room", 0.6, false, None),
            fader("lpf", 0.25, true, Some(0)),
        ];
        facts.faders[1].value = "2300".into();
        assert!(facts.strips[0].selected);
        for width in [40, 80] {
            let area = Rect::new(0, 0, width, 16);
            let mut buffer = Buffer::empty(area);
            MixerPanelView {
                keybinds: &crate::keybinds::Keybinds::default(),
                facts: Some(&facts),
                panel: MixerPanel::new(MixerEdge::Bottom, Some(10)),
                theme: &Theme::built_in_default(),
                focused: true,
                area,
                selection: None,
            }
            .render(area, &mut buffer);
            let parts = parts(area, MixerEdge::Bottom).expect("parts");
            let rule = row(&buffer, area, parts.rule.y);
            assert!(rule.contains("▶ lpf 2300"), "{rule}");
            assert!(!rule.contains("input gain"), "{rule}");
            assert!(!rule.contains("room"), "{rule}");
            assert!(
                !rule.contains("dB"),
                "the fader keeps its own units: {rule}"
            );
        }
    }
}
