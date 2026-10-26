//! The knobs panel: a rail for each numeric parameter of the call at the
//! caret, a column at the right edge of the score.
//!
//! The panel reads the score, never the reverse. Every move rewrites the
//! number the score already holds - or the value argument of the
//! `slider(...)` call it holds, keeping its declared bounds - so an update
//! or auto evaluate hears the change. The far left of a rail is the off
//! stop: an optional entry of a `{}` object comes out of the score there,
//! and moves back in when the rail leaves it, drawn in the muted colour
//! while it is off and the accent colour once it is on.

use std::ops::Range;

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Clear, Paragraph, Widget};

use super::editor::Revision;
use super::number_form::{SliderForm, field_value_x, short_number};
use super::scenes::SceneId;
use super::scroll;
use super::theme::Theme;

/// The column's width, cells across.
pub const KNOBS_WIDTH: u16 = 26;
/// The least rows the column draws: a title, a rail, and two hint rows.
pub const KNOBS_HEIGHT: u16 = 6;

/// The rows one knob takes: its name and reading, then its rail.
const KNOB_ROWS: u16 = 2;

/// The least rail a knob is worth dragging: the off stop and a few
/// positions beside it.
const RAIL_MIN: u16 = 8;

/// What a rail moves in the score, and where.
#[derive(Clone, Debug, PartialEq)]
pub enum KnobPlace {
    /// `key:` of the call's options object.
    Entry(String),
    /// The n-th argument of the call, counting from zero.
    Argument(usize),
}

/// One rail of the panel: the app's reading of one parameter, resolved
/// against the score, the plugin and the remembered bounds.
#[derive(Clone, Debug)]
pub struct Knob {
    /// The word above the rail.
    pub label: String,
    pub place: KnobPlace,
    /// The number's bytes and value, when the score holds the parameter.
    /// `None` is the off stop: an optional entry the score does not have.
    pub state: Option<KnobState>,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    /// The value the parameter takes when its rail leaves the off stop.
    pub default: f64,
    /// An entry of a `{}` object can be off. A positional argument cannot:
    /// the call would not parse without it.
    pub optional: bool,
    /// A frequency control on a positive range, as the inline sliders
    /// decide it, so the rail travels the way the ear hears it.
    pub log: bool,
}

impl Knob {
    /// Where the value sits between the bounds, 0..=1, or `None` at the
    /// off stop. The travel itself is applied by the app, which owns it.
    pub fn notch(&self) -> Option<f64> {
        let state = self.state.as_ref()?;
        if self.max <= self.min {
            return Some(0.0);
        }
        let value = state.value.clamp(self.min, self.max);
        let ratio = if self.log && self.min > 0.0 {
            (value.max(self.min).ln() - self.min.ln()) / (self.max.ln() - self.min.ln())
        } else {
            (value - self.min) / (self.max - self.min)
        };
        Some(ratio.clamp(0.0, 1.0))
    }
}

/// The score's own words for a parameter the panel holds a rail to.
#[derive(Clone, Debug)]
pub struct KnobState {
    /// The number's bytes: what a move rewrites.
    pub literal: Range<usize>,
    /// The fader the value expression is: its declared bounds are the
    /// knob's, and a move rewrites the fader's first argument, keeping
    /// the rest of its call.
    pub fader: Option<Fader>,
    pub value: f64,
}

/// A `slider(value, min, max, step)` the score declares at a parameter.
#[derive(Clone, Copy, Debug)]
pub struct Fader {
    pub min: f64,
    pub max: f64,
    pub step: f64,
    /// The transpiler's marking of the control the fader drives: a
    /// frequency control travels on the ear's own scale.
    pub frequency: bool,
    /// The whole `slider(...)` call's bytes, from its name to its `)`.
    pub call: (usize, usize),
}

/// The add list, while `a` holds the panel: every parameter the call could
/// take, searched as it is typed.
#[derive(Clone, Debug)]
pub struct AddList {
    pub query: String,
    pub names: Vec<String>,
    /// The text beside each name, its default mostly.
    pub details: Vec<String>,
    /// The search the list ranks with, as its caller gives it: a plugin
    /// host's word ranking finds a name through its spaces and case.
    pub ranker: fn(&str, &[String]) -> Vec<usize>,
    /// The query's answers, best first. Nothing typed is every name.
    pub ranked: Vec<usize>,
    pub selected: usize,
    pub scroll: usize,
}

impl AddList {
    /// The rows the query leaves. Nothing typed is every name, in the
    /// order the plugin or the reference gives them.
    pub fn rows(&self) -> Vec<usize> {
        if self.query.is_empty() {
            (0..self.names.len()).collect()
        } else {
            self.ranked.clone()
        }
    }

    /// Rank the names by the query and start from the top row.
    pub fn rank(&mut self) {
        self.ranked = (self.ranker)(&self.query, &self.names);
        self.selected = 0;
        self.scroll = 0;
    }

    /// Keep the chosen row in view with the studio's list margin.
    /// Keep the chosen row in view, `shown` rows at a time.
    pub fn follow(&mut self, count: usize, shown: usize) {
        let shown = count.min(shown.max(1));
        self.scroll = scroll::follow(
            self.scroll,
            self.selected,
            shown,
            count,
            scroll::margin(shown),
        );
    }
}

/// One call of one scene, as the panel reads it: where its `(` stands, in
/// the text of `revision`, and its name. Two calls of the same name are
/// two calls, and an edit above a call moves its `(` but not the call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallAt {
    pub scene: SceneId,
    pub open: usize,
    pub revision: Revision,
    pub name: String,
}

/// The value editor, while `e` holds the panel.
#[derive(Clone, Debug)]
pub struct KnobEdit {
    /// Which rail the form belongs to, as the reading holds it.
    pub place: KnobPlace,
    /// The call the form opened on: a caret that went to another call
    /// refuses the write rather than landing in that call.
    pub call: Option<CallAt>,
    /// The score the form opened on, so a score that changed under it
    /// refuses the write rather than landing in the wrong place.
    pub original: String,
    pub(crate) form: SliderForm,
}

/// An entry the hand took out at the far-left stop, with the row it sat
/// at: the rail keeps its place in the column until the caret leaves the
/// call, so a move right puts it back under the hand.
#[derive(Clone, Debug)]
pub struct HeldOff {
    pub at: usize,
    pub knob: Knob,
    /// The entry held a whole `slider(...)` call: putting it back writes
    /// the fader, not the bare number, so the score keeps its own
    /// declaration.
    pub fader: bool,
    /// The call's text before and after the entry came out. While the
    /// call still reads as it was left, the entry goes back exactly as
    /// it was: its place, its comments, its fader.
    pub text: Option<HeldText>,
}

/// What came out of a call with an entry the hand took off, to put back.
#[derive(Clone, Debug)]
pub struct HeldText {
    /// The call's text from its `(` to its `)`, as the entry left it.
    pub after: String,
    /// The pieces that came out, each with where it goes back in `after`.
    pub pieces: Vec<(usize, String)>,
    /// The piece the entry's own code is in.
    pub entry: usize,
    /// The value's number, in that piece.
    pub literal: Range<usize>,
    /// The whole `slider(...)` call in that piece, when the value is one.
    pub fader: Option<Range<usize>>,
}

/// The presets of the plugin call the column reads: a section of rows of
/// its own under a heading, closed until the hand opens it. The names come
/// from the host's read of the preset folders, and the section empties
/// with the call.
#[derive(Clone, Debug)]
pub struct KnobPresets {
    pub names: Vec<String>,
    /// The read of the preset folders the names came from: a new read
    /// brings the names again.
    pub version: u64,
    pub open: bool,
}

/// The rows the presets section holds: its heading, and a row for each
/// preset while the heading is open.
pub fn presets_rows(presets: &KnobPresets) -> usize {
    1 + usize::from(presets.open) * presets.names.len()
}

/// The panel's own state. The rails themselves belong to each frame's
/// reading of the score, which the app computes and hands to the view.
/// What stays here is what the hand is doing.
#[derive(Clone, Debug, Default)]
pub struct KnobsPanel {
    pub selected: usize,
    pub scroll: usize,
    pub adding: Option<AddList>,
    pub editing: Option<KnobEdit>,
    /// The presets of the plugin call the column reads, as a section of
    /// rows under the rails. Closed until the hand opens it.
    pub presets: Option<KnobPresets>,
    /// The entries the hand took out at the far-left stop, held until the
    /// caret leaves the call: the rail stays, and a move right writes the
    /// entry back at the value it left at.
    pub off: Vec<HeldOff>,
    /// The bounds deduced for the call's bare numbers, held while the
    /// hand works the call. A deduction from each new value would widen
    /// the rail under the hand at every move.
    pub ranges: Vec<(KnobPlace, (f64, f64, f64))>,
    /// The rows in the order the hand first saw them: a rail keeps its row
    /// while its entry goes off, comes back or moves in the object.
    pub order: Vec<KnobPlace>,
    /// The order was taken with the plugin's own list in hand. An order
    /// taken while the plugin still loaded gives way, once, to the
    /// plugin's order when its list arrives.
    pub order_by_plugin: bool,
    /// The call the held entries and bounds belong to. Another call, or
    /// another document, empties the hand.
    pub off_call: Option<CallAt>,
    /// `r` was pressed once: the next `r` resets the plugin's rails, any
    /// other key keeps them.
    pub reset_armed: bool,
}

impl KnobsPanel {
    /// Move the selection by whole knobs, wrapping at the ends.
    pub fn move_by(&mut self, delta: isize, count: usize) {
        if count == 0 {
            self.selected = 0;
            return;
        }
        self.selected = ((self.selected as isize + delta).rem_euclid(count as isize)) as usize;
    }

    pub fn clamp(&mut self, count: usize) {
        self.selected = self.selected.min(count.saturating_sub(1));
    }

    /// Keep the selection in view with the studio's list margin.
    pub fn follow(&mut self, count: usize, shown: usize) {
        self.scroll = scroll::follow(
            self.scroll,
            self.selected,
            shown,
            count,
            scroll::margin(shown),
        );
    }
}

/// The column's parts: the title row, the rails' body, the hint row, and
/// the rule on the editor's side. Mirrors the visuals dock's parts, so
/// the two columns read as the same furniture.
#[derive(Clone, Copy, Debug)]
pub struct KnobsParts {
    pub title: Rect,
    pub body: Rect,
    pub hint: Rect,
    pub rule: Rect,
}

/// The add list's width: room for a long parameter name and its start
/// value side by side.
const ADD_WIDTH: u16 = 48;

/// Where the add list stands for a column at `area` with `rows` names to
/// list: right-aligned to the column and reaching left over the score, as
/// tall as the names need, up to the column's height.
pub fn add_sheet(area: Rect, rows: usize) -> Rect {
    let width = ADD_WIDTH.max(area.width).min(area.right());
    let height = (rows as u16).saturating_add(3).min(area.height);
    Rect::new(area.right() - width, area.y, width, height)
}

/// The rows of names the add list shows at once: the sheet less its
/// border and its query row.
pub fn add_shown(sheet: Rect) -> usize {
    usize::from(sheet.height.saturating_sub(3))
}

/// The row of the add list a point is on, counting from the first name.
pub fn add_row_at(sheet: Rect, scroll: usize, x: u16, y: u16) -> Option<usize> {
    let first = sheet.y + 2;
    let inside = x > sheet.x && x + 1 < sheet.right() && y >= first && y + 1 < sheet.bottom();
    inside.then(|| scroll + usize::from(y - first))
}

pub fn parts(area: Rect) -> Option<KnobsParts> {
    if area.is_empty() || area.height < KNOBS_HEIGHT || area.width < KNOBS_WIDTH {
        return None;
    }
    // The rule and a space on the score's side, and a space on the other,
    // so the column's numbers never touch what stands beside it.
    let inner = Rect::new(
        area.x + 2,
        area.y,
        area.width.saturating_sub(3),
        area.height,
    );
    let title = Rect::new(inner.x, inner.y, inner.width, 1);
    // Two hint rows: the keys of the rail under the hand, then the keys
    // of the call itself.
    let hint = Rect::new(inner.x, inner.bottom() - 2, inner.width, 2);
    let body = Rect::new(
        inner.x,
        inner.y + 1,
        inner.width,
        inner.height.saturating_sub(3),
    );
    let rule = Rect::new(area.x, area.y, 1, area.height);
    Some(KnobsParts {
        title,
        body,
        hint,
        rule,
    })
}

/// One rail's rows in the body, by its place in the list: the label row,
/// then the rail row.
pub fn knob_row(body: Rect, index: usize) -> Option<(Rect, Rect)> {
    let top = body.y as usize + index * KNOB_ROWS as usize;
    let bottom = top + KNOB_ROWS as usize;
    if bottom > body.bottom() as usize {
        return None;
    }
    let label = Rect::new(body.x, top as u16, body.width, 1);
    let rail = Rect::new(body.x, top as u16 + 1, body.width, 1);
    Some((label, rail))
}

/// How many rails the body shows.
pub fn shown(body: Rect) -> usize {
    (body.height / KNOB_ROWS) as usize
}

/// The knob a point is on, and whether the point is on its rail rather
/// than its label. The answer is the knob's place in the whole list, so
/// the caller does not have to know what the body is showing.
pub fn knob_at(body: Rect, scroll: usize, x: u16, y: u16) -> Option<(usize, bool)> {
    if !body.contains(ratatui::layout::Position::new(x, y)) {
        return None;
    }
    let row = (y - body.y) as usize;
    let knob = row / KNOB_ROWS as usize + scroll;
    let on_rail = row % KNOB_ROWS as usize == 1;
    Some((knob, on_rail))
}

/// A rail's leftmost cells, the off stop among them.
pub fn rail_position(rail: Rect, x: u16) -> usize {
    (x.saturating_sub(rail.x) as usize).min(rail.width.saturating_sub(1) as usize)
}

/// The cell a value draws its handle on: the off stop at the far left when
/// the knob is optional, then the travel between the bounds.
pub fn rail_cell(rail: Rect, optional: bool, notch: Option<f64>) -> usize {
    let cells = rail.width as usize;
    match (notch, optional) {
        (None, _) => 0,
        (Some(_), true) => 1 + (notch.unwrap_or(0.0) * (cells - 2) as f64).round() as usize,
        (Some(ratio), false) => (ratio * (cells - 1) as f64).round() as usize,
    }
}

/// The presets section as the painter draws it: its heading, the names
/// under it while it is open, and the name the call's object holds.
pub struct PresetsSection<'a> {
    pub open: bool,
    pub names: &'a [String],
    /// The name the call's object holds, when it holds one.
    pub active: Option<&'a str>,
}

/// What the app hands the painter about the column: each frame's reading
/// of the call at the caret, and what the hand is doing to it.
pub struct KnobsData<'a> {
    /// The call's name, when the caret is in one.
    pub name: Option<&'a str>,
    /// The call has a list for `a` to add from.
    pub listed: bool,
    pub knobs: &'a [Knob],
    /// The presets of the plugin call, under the rails.
    pub presets: Option<PresetsSection<'a>>,
    pub panel: &'a KnobsPanel,
}

/// The panel as the painter draws it. The reading is each frame's own. The
/// panel is what the hand is doing to it.
pub struct KnobsView<'a> {
    /// The call's name, when the caret is in one.
    pub name: Option<&'a str>,
    /// The call has a list for `a` to add from: a plugin's parameters, or
    /// the options its reference documents.
    pub listed: bool,
    pub knobs: &'a [Knob],
    /// The presets of the plugin call, under the rails.
    pub presets: Option<PresetsSection<'a>>,
    pub panel: &'a KnobsPanel,
    pub theme: &'a Theme,
    pub focused: bool,
    pub area: Rect,
}

impl Widget for KnobsView<'_> {
    fn render(self, _: Rect, buffer: &mut ratatui::buffer::Buffer) {
        let theme = self.theme;
        let Some(parts) = parts(self.area) else {
            return;
        };
        super::view::clear_overlay(
            buffer,
            self.area,
            Style::default().bg(theme.background).fg(theme.foreground),
        );
        let rule = Style::default().fg(theme.rule);
        for y in parts.rule.y..parts.rule.bottom() {
            buffer.set_stringn(parts.rule.x, y, "│", 1, rule);
        }
        // The call the rails belong to, always: the accent while the
        // column has the keys, muted while the score has them.
        let title = match self.name {
            Some(name) => format!("knobs · {name}"),
            None => "knobs".to_owned(),
        };
        let title_style = if self.focused {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        };
        buffer.set_stringn(
            parts.title.x,
            parts.title.y,
            title,
            usize::from(parts.title.width),
            title_style,
        );
        let body = parts.body;
        if self.knobs.is_empty() {
            // The title names the call already, so a call with no numbers
            // needs only a quiet placeholder.
            let lines: &[&str] = match self.name {
                Some(_) => &["- empty -"],
                None => &["put the caret in a call", "to see its knobs"],
            };
            for (row, line) in lines.iter().enumerate() {
                buffer.set_stringn(
                    body.x,
                    body.y + row as u16,
                    line,
                    usize::from(body.width),
                    Style::default().fg(theme.muted),
                );
            }
        }
        for index in 0..shown(body) {
            let at = self.panel.scroll + index;
            if at >= self.knobs.len() {
                break;
            }
            let Some((label, rail)) = knob_row(body, index) else {
                break;
            };
            let knob = &self.knobs[at];
            let chosen = self.focused && at == self.panel.selected;
            self.draw_label(buffer, label, knob, chosen);
            self.draw_rail(buffer, rail, knob, chosen);
        }
        // The presets of the plugin call, as rows of their own under the
        // rails: a heading, and a name under it while the heading is open.
        if let Some(section) = &self.presets {
            let first = self.knobs.len();
            let names = std::iter::once(None)
                .chain(section.names.iter().filter(|_| section.open).map(Some));
            for (offset, name) in names.enumerate() {
                let at = first + offset;
                let row = at.saturating_sub(self.panel.scroll);
                let Some((label, _)) = knob_row(body, row) else {
                    break;
                };
                let chosen = self.focused && at == self.panel.selected;
                self.draw_preset_row(buffer, label, section, name, chosen);
            }
        }
        if self.focused {
            for (row, line) in self.hint().iter().enumerate() {
                buffer.set_stringn(
                    parts.hint.x,
                    parts.hint.y + row as u16,
                    line,
                    usize::from(parts.hint.width),
                    Style::default().fg(theme.muted),
                );
            }
        }
        if self.focused
            && let Some(adding) = self.panel.adding.as_ref()
        {
            self.draw_add_list(buffer, self.area, adding);
        }
        if self.focused
            && let Some(editing) = self.panel.editing.as_ref()
        {
            self.draw_edit_form(buffer, self.area, editing);
        }
    }
}

impl KnobsView<'_> {
    /// The keys for what is under the hand, short enough for the column:
    /// the rail's own keys, then the call's. The footer is the panel's one
    /// place to say what a key does.
    fn hint(&self) -> [String; 2] {
        let plugin = matches!(self.name, Some("vst" | "vsti"));
        if self.panel.adding.is_some() {
            // Only a plugin remembers a choice for Delete to forget.
            let forget = if plugin { "Delete forgets it" } else { "" };
            return ["type · Enter adds · Esc".to_owned(), forget.to_owned()];
        }
        if self.panel.editing.is_some() {
            return [
                "↑↓/Tab fields · Enter writes · Esc".to_owned(),
                String::new(),
            ];
        }
        let section = self
            .presets
            .as_ref()
            .filter(|section| !section.names.is_empty())
            .and_then(|_| self.panel.selected.checked_sub(self.knobs.len()));
        let rail = match section {
            Some(0) => "→ opens the presets",
            Some(_) => "Enter takes · ← closes",
            None => match self.knobs.get(self.panel.selected) {
                Some(knob) if knob.optional && knob.state.is_none() => "→ on · Enter edit",
                Some(knob) if knob.optional => "⌫ off · Enter edit",
                Some(_) => "←/→ move · Enter edit",
                None => "",
            },
        };
        let call = if plugin {
            "a add · r reset list"
        } else if self.listed {
            "a add · Esc back"
        } else {
            "Esc back to the score"
        };
        [rail.to_owned(), call.to_owned()]
    }

    /// One row of the presets section: the heading, with its marker and
    /// its count, or a preset name under it, with the mark of the preset
    /// the call's object holds.
    fn draw_preset_row(
        &self,
        buffer: &mut ratatui::buffer::Buffer,
        area: Rect,
        section: &PresetsSection<'_>,
        name: Option<&String>,
        chosen: bool,
    ) {
        let theme = self.theme;
        let style = |colour: ratatui::style::Color| {
            let style = Style::default().fg(colour);
            if chosen {
                style.add_modifier(Modifier::BOLD)
            } else {
                style
            }
        };
        let Some(name) = name else {
            let marker = super::terminal::symbol(if section.open { "▾" } else { "▸" });
            buffer.set_stringn(
                area.x,
                area.y,
                format!("{marker} presets"),
                usize::from(area.width),
                style(theme.accent),
            );
            let count = section.names.len().to_string();
            let width = count.chars().count() as u16;
            let x = area.right().saturating_sub(width).max(area.x);
            buffer.set_stringn(
                x,
                area.y,
                &count,
                usize::from(area.right() - x),
                style(theme.muted),
            );
            return;
        };
        // The name the call holds is the preset the score plays: it leads
        // the names, and carries the mark. A studio built without the
        // plugin host lists no presets, so no name is the one held.
        #[cfg(feature = "vst")]
        let active = section.active.is_some_and(|active| {
            rustel_runtime::vst::canonical(active) == rustel_runtime::vst::canonical(name)
        });
        #[cfg(not(feature = "vst"))]
        let active = false;
        let mark = if active { "●" } else { " " };
        let text = format!("{mark} {name}");
        buffer.set_stringn(
            area.x,
            area.y,
            &text,
            usize::from(area.width),
            style(if active {
                theme.accent
            } else {
                theme.foreground
            }),
        );
    }

    fn draw_label(
        &self,
        buffer: &mut ratatui::buffer::Buffer,
        area: Rect,
        knob: &Knob,
        chosen: bool,
    ) {
        let theme = self.theme;
        let on = knob.state.is_some();
        let reading = match &knob.state {
            Some(state) => short_number(state.value),
            None => "off".to_owned(),
        };
        let style = |colour: ratatui::style::Color| {
            let style = Style::default().fg(colour);
            if chosen {
                style.add_modifier(Modifier::BOLD)
            } else {
                style
            }
        };
        buffer.set_stringn(
            area.x,
            area.y,
            &knob.label,
            usize::from(area.width),
            style(if on { theme.foreground } else { theme.muted }),
        );
        let width = reading.chars().count() as u16;
        let x = area.right().saturating_sub(width).max(area.x);
        buffer.set_stringn(
            x,
            area.y,
            &reading,
            usize::from(area.right() - x),
            style(if on { theme.accent } else { theme.muted }),
        );
    }

    fn draw_rail(
        &self,
        buffer: &mut ratatui::buffer::Buffer,
        rail: Rect,
        knob: &Knob,
        chosen: bool,
    ) {
        let theme = self.theme;
        if rail.width < RAIL_MIN {
            return;
        }
        let notch = knob.notch();
        let handle = rail_cell(rail, knob.optional, notch);
        let mut text = String::with_capacity(usize::from(rail.width) * 3);
        for position in 0..rail.width as usize {
            // The off stop is a hollow cell, so a rail that has gone out
            // says so at a glance, the way a dimmed light does.
            if knob.optional && position == 0 {
                text.push(if notch.is_none() { '█' } else { '·' });
            } else {
                text.push(if position == handle { '█' } else { '─' });
            }
        }
        let mut style = Style::default().fg(if notch.is_some() {
            theme.accent
        } else {
            theme.muted
        });
        if chosen {
            style = style.add_modifier(Modifier::BOLD);
        }
        buffer.set_stringn(rail.x, rail.y, &text, usize::from(rail.width), style);
    }

    /// The add list: wider than the column, over the score beside it, and
    /// as tall as the column when the plugin has the rows for it.
    fn draw_add_list(&self, buffer: &mut ratatui::buffer::Buffer, area: Rect, adding: &AddList) {
        let theme = self.theme;
        let rows = adding.rows();
        let sheet = add_sheet(area, rows.len());
        super::graphics::cover_images(sheet);
        Clear.render(sheet, buffer);
        ratatui::widgets::Block::bordered()
            .title(" add a knob ")
            .style(Style::default().bg(theme.overlay).fg(theme.foreground))
            .render(sheet, buffer);
        let inner = Rect::new(
            sheet.x + 1,
            sheet.y + 1,
            sheet.width.saturating_sub(2),
            sheet.height.saturating_sub(2),
        );
        buffer.set_stringn(
            inner.x,
            inner.y,
            if adding.query.is_empty() {
                "type to search"
            } else {
                &adding.query
            },
            usize::from(inner.width),
            Style::default().fg(if adding.query.is_empty() {
                theme.muted
            } else {
                theme.foreground
            }),
        );
        let shown = add_shown(sheet);
        for (row, index) in rows.iter().skip(adding.scroll).take(shown).enumerate() {
            let y = inner.y + 1 + row as u16;
            let chosen = adding.scroll + row == adding.selected;
            let style = if chosen {
                Style::default()
                    .fg(theme.selection_text)
                    .bg(theme.selection)
            } else {
                Style::default().fg(theme.foreground)
            };
            // The name on the left, its start value on the right.
            let name = &adding.names[*index];
            let detail = adding.details.get(*index).map(String::as_str).unwrap_or("");
            let width = usize::from(inner.width);
            let gap = width.saturating_sub(name.chars().count() + detail.chars().count());
            let line = if gap > 0 {
                format!("{name}{}{detail}", " ".repeat(gap))
            } else {
                format!("{name} {detail}")
            };
            buffer.set_stringn(inner.x, y, &line, width, style);
        }
    }

    /// The number form, drawn over the column's own area.
    fn draw_edit_form(&self, buffer: &mut ratatui::buffer::Buffer, area: Rect, editing: &KnobEdit) {
        let theme = self.theme;
        let sheet = edit_sheet(area, &editing.form);
        super::graphics::cover_images(sheet);
        Clear.render(sheet, buffer);
        ratatui::widgets::Block::bordered()
            .title(" edit the knob ")
            .style(Style::default().bg(theme.overlay).fg(theme.foreground))
            .render(sheet, buffer);
        let mut rows = editing.form.rows();
        rows.push(match &editing.form.error {
            Some(problem) => format!("  \u{26a0} {problem}"),
            None => "  Tab · Enter applies · Esc back".to_owned(),
        });
        for (at, row) in rows.iter().enumerate() {
            let y = sheet.y + 1 + at as u16;
            if y + 1 >= sheet.bottom() {
                break;
            }
            let warning = row.contains('\u{26a0}');
            let style = if warning {
                Style::default().fg(theme.error)
            } else {
                Style::default().fg(theme.muted)
            };
            Paragraph::new(row.clone()).style(style).render(
                Rect::new(sheet.x + 1, y, sheet.width.saturating_sub(2), 1),
                buffer,
            );
        }
    }
}

/// Where the edit form's sheet sits, and how tall it is: the form's rows,
/// the row that says what Enter does, and the border.
fn edit_sheet(area: Rect, form: &SliderForm) -> Rect {
    let rows = form.rows().len() as u16 + 3;
    let height = rows.min(area.height);
    Rect::new(area.x + 1, area.y, area.width.saturating_sub(1), height)
}

/// Where the edit form's caret is drawn, while the form is up.
pub fn edit_caret(panel: &KnobsPanel, area: Rect) -> Option<(u16, u16)> {
    let editing = panel.editing.as_ref()?;
    let form = &editing.form;
    let sheet = edit_sheet(area, form);
    if sheet.width < 2 {
        return None;
    }
    let typed = unicode_width::UnicodeWidthStr::width(&form.text()[..form.cursor]) as u16;
    let column = field_value_x(sheet) + typed;
    Some((
        column.min(sheet.right() - 2),
        sheet.y + 1 + form.field as u16,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body() -> Rect {
        Rect::new(10, 5, 24, 12)
    }

    /// A rail's handle cell and a click on that cell have to agree, or a
    /// drag jumps the value on its first move.
    #[test]
    fn the_handle_cell_and_the_click_position_agree() {
        let rail = Rect::new(0, 0, 24, 1);
        for notch in [0.0, 0.25, 0.5, 0.75, 1.0] {
            for optional in [false, true] {
                let cell = rail_cell(rail, optional, Some(notch));
                assert_eq!(rail_position(rail, cell as u16), cell);
                if optional && cell > 0 {
                    assert!((cell as f64 - 1.0) / (rail.width - 2) as f64 - notch <= 0.06);
                } else if !optional {
                    assert!(cell as f64 / (rail.width - 1) as f64 - notch <= 0.05);
                }
            }
        }
        // The off stop is the far left cell, whatever the travel.
        assert_eq!(rail_cell(rail, true, None), 0);
        assert_eq!(rail_cell(rail, false, Some(0.0)), 0);
    }

    #[test]
    fn a_point_finds_its_knob_and_its_rail() {
        let (label, rail) = knob_row(body(), 0).expect("a knob in a tall body");
        assert_eq!(label.y, 5);
        assert_eq!(rail.y, 6);
        assert_eq!(knob_at(body(), 0, 10, 5), Some((0, false)));
        assert_eq!(knob_at(body(), 0, 30, 6), Some((0, true)));
        assert_eq!(
            knob_at(body(), 2, 10, 8),
            Some((3, true)),
            "the scroll counts knobs"
        );
        assert_eq!(knob_at(body(), 0, 9, 6), None, "outside the body");
    }

    #[test]
    fn the_log_notch_spends_its_cells_where_the_ear_hears_them() {
        let knob = |value| Knob {
            label: "lpf".into(),
            place: KnobPlace::Argument(0),
            state: Some(KnobState {
                literal: 0..3,
                fader: None,
                value,
            }),
            min: 100.0,
            max: 16000.0,
            step: 1.0,
            default: value,
            optional: false,
            log: true,
        };
        // Half the rail is the geometric mean, and an octave is the same
        // share of it anywhere in the range, where a linear rail gives an
        // octave at the bottom almost none of the cells.
        let half = knob((100.0f64 * 16000.0).sqrt()).notch().expect("a notch");
        assert!((half - 0.5).abs() < 0.001, "the mean sits at half: {half}");
        let octave = 2.0f64.ln() / 160.0f64.ln();
        for base in [200.0, 2000.0] {
            let low = knob(base).notch().expect("a notch");
            let high = knob(base * 2.0).notch().expect("a notch");
            assert!(
                (high - low - octave).abs() < 0.001,
                "an octave is the same share everywhere: {low} {high}"
            );
        }
    }
}
