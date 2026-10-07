//! The visuals docks' keys, clicks and widgets: what the app does with
//! the columns and bands that `crate::viz_panel` draws. Two docks, each
//! at an edge of its own; one of them has the keyboard at a time.

use super::super::file_picker::FilePicker;
use super::super::meter;
use super::super::viz_panel::{ART_MAX_BYTES, DOCKS, Dock, art_text_error};
use super::*;

impl App {
    /// The mix's level on the meter's own scale, 0..1, for the widgets
    /// that breathe with it.
    pub(super) fn mix_level(&self) -> f32 {
        if !self.is_playing() {
            return 0.0;
        }
        meter::scale_position(self.master.peak_db()).clamp(0.0, 1.0)
    }

    /// Whether there is sound for the pictures to be of.
    ///
    /// A stop keeps `playing` true until the last voices have rung out, so
    /// the widgets stay for the tail and go when it does - which is what
    /// the ear expects of a picture of the mix.
    pub(super) fn viz_motion(&self) -> super::super::viz_panel::Motion {
        use super::super::viz_panel::Motion;
        if self.is_playing() {
            Motion::Live
        } else {
            Motion::Off
        }
    }

    /// Advance the visuals' own clock, and answer where it now stands.
    ///
    /// It counts sound rather than time: stopped, it stays where it was,
    /// so an animation driven by it stops with the music instead of
    /// running on over silence.
    pub(super) fn count_sound(&self, now: Instant, motion: super::super::viz_panel::Motion) -> f32 {
        let since = now.saturating_duration_since(self.sound_counted_at.get());
        self.sound_counted_at.set(now);
        if motion == super::super::viz_panel::Motion::Live {
            self.sound_seconds
                .set(self.sound_seconds.get() + since.as_secs_f32());
        }
        self.sound_seconds.get()
    }

    /// Whether any open dock has a widget that moves on its own.
    ///
    /// Only while there is sound: the clock those widgets animate on stops
    /// with the music, so a stopped set redraws the same frame forever and
    /// asking for it is wasted work.
    pub(super) fn viz_animated(&self) -> bool {
        self.viz_motion() == super::super::viz_panel::Motion::Live
            && self
                .viz_docks
                .iter()
                .zip(&self.prefs.visuals)
                .any(|(dock, prefs)| {
                    dock.is_some() && prefs.widgets.iter().any(WidgetSpec::animated)
                })
    }

    /// The room each open dock asks the layout for: a column's width, or
    /// a band's rows for its widgets across the whole screen.
    pub(super) fn dock_requests(&self) -> [Option<Dock>; DOCKS] {
        std::array::from_fn(|index| {
            let panel = self.viz_docks[index].as_ref()?;
            Some(Dock {
                edge: panel.edge,
                extent: self.dock_extent(index),
            })
        })
    }

    /// [`Self::dock_extent`] for the view, which asks before it draws.
    pub(super) fn dock_extent_for_view(&self, index: usize) -> u16 {
        self.dock_extent(index)
    }

    fn dock_extent(&self, index: usize) -> u16 {
        self.prefs.visuals[index].extent()
    }

    /// The dock whose room a point is in.
    pub(super) fn viz_dock_at(&self, x: u16, y: u16) -> Option<usize> {
        (0..DOCKS)
            .find(|&index| self.viz_docks[index].is_some() && within(self.regions.viz[index], x, y))
    }

    /// The dock with the keyboard, when one is open.
    fn focused_dock(&self) -> Option<usize> {
        self.viz_docks[self.viz_focus]
            .is_some()
            .then_some(self.viz_focus)
    }

    /// View > Visuals 1 and 2: a dock of widgets, until the same again.
    /// Opening it gives it the keyboard; Esc gives it back.
    pub(super) fn toggle_viz_dock(&mut self, index: usize) {
        if self.viz_docks[index].is_some() {
            self.close_viz_dock(index);
            self.status = format!("visuals {} hidden", index + 1);
            return;
        }
        if self.ui_settings.zen {
            // A visuals dock is docked furniture by nature - a column or
            // a band that takes a slice of the stage for good - and zen's
            // whole point is a stage nothing is sliced from. Every other
            // panel comes up as a popup instead; this one has no popup
            // form to come up as, so it stays refused rather than opening
            // invisible and keeping the keyboard.
            self.status = format!(
                "zen has no docked furniture - visuals {} stays off · {} restores the stage",
                index + 1,
                self.shortcut_or_menu(BindAction::Zen, "View > Zen mode")
            );
            self.dirty_frame = true;
            return;
        }
        self.open_viz_dock(index, true);
        // Its region is not computed until the next draw; until then the
        // dock keeps the keyboard rather than falling through to the score.
        self.viz_unlaid[index] = true;
        self.prefs.visuals[index].open = true;
        self.save_prefs_soon();
    }

    pub(super) fn close_viz_dock(&mut self, index: usize) {
        if self.viz_docks[index].take().is_none() {
            return;
        }
        self.viz_unlaid[index] = false;
        // The keyboard goes to the other dock if it is open.
        if self.viz_focus == index
            && let Some(other) = (0..DOCKS).find(|&other| self.viz_docks[other].is_some())
        {
            self.viz_focus = other;
        }
        self.settle_focus();
        self.invalidate_maps();
        self.prefs.visuals[index].open = false;
        self.save_prefs_soon();
        self.dirty_frame = true;
    }

    /// A dock's sheets are its own. When the keyboard moves to another
    /// dock the first one's popups go: the view draws the first dock that
    /// has a sheet up, so leaving them shows one the keys no longer drive,
    /// and Enter then adds the widget the driven dock had chosen.
    fn clear_other_viz_sheets(&mut self, index: usize) {
        for (other, dock) in self.viz_docks.iter_mut().enumerate() {
            if other == index {
                continue;
            }
            if let Some(panel) = dock.as_mut() {
                panel.adding = None;
                panel.prompt = None;
            }
        }
    }

    /// Show a dock, with the keyboard or without it. An empty dock gets
    /// the set's name in art, so there is something to look at.
    pub(super) fn open_viz_dock(&mut self, index: usize, focus: bool) {
        self.viz_docks[index] = Some(VizPanel {
            edge: self.prefs.visuals[index].edge,
            ..VizPanel::default()
        });
        self.ensure_default_widget(index);
        self.invalidate_maps();
        if focus {
            self.viz_focus = index;
            self.clear_other_viz_sheets(index);
            self.focus_panel(PanelKind::Viz);
        }
        self.status = format!(
            "visuals {number} - ↑/↓ choose a widget · ←/→ its style · Space its colour · a adds · e moves it · View {arrow} Visuals {number} hides",
            number = index + 1,
            arrow = crate::terminal::symbol("▸"),
        );
        self.dirty_frame = true;
    }

    /// The widgets are the studio's, kept in the preferences, the same in
    /// every set and every launch. Without any, a set file that kept
    /// widgets from before hands them to the first dock, and failing that
    /// the dock gets the art of the set's name.
    pub(super) fn ensure_default_widget(&mut self, index: usize) {
        if !self.prefs.visuals[index].widgets.is_empty() {
            return;
        }
        let carried = if index == 0 {
            self.scenes.take_carried_widgets()
        } else {
            Vec::new()
        };
        self.prefs.visuals[index].widgets = if carried.is_empty() {
            vec![WidgetSpec::default_art()]
        } else {
            carried
        };
        self.save_prefs_soon();
    }

    /// Give a dock new widgets and remember them.
    fn set_widgets(&mut self, index: usize, widgets: Vec<WidgetSpec>) {
        self.prefs.visuals[index].widgets = widgets;
        self.save_prefs_soon();
    }

    /// `e` on a dock: the next edge round - left, right, top, bottom -
    /// remembered with the setting.
    fn move_viz_dock(&mut self) {
        let Some(index) = self.focused_dock() else {
            return;
        };
        let edge = self.prefs.visuals[index].edge.next(true);
        self.prefs.visuals[index].edge = edge;
        self.ui_settings.viz_edges[index] = edge;
        if let Some(panel) = self.viz_docks[index].as_mut() {
            panel.edge = edge;
            panel.scroll = 0;
        }
        self.save_prefs_soon();
        self.invalidate_maps();
        self.status = format!("visuals {} at the {}", index + 1, edge.name());
    }

    /// `+`/`-` on a dock: bigger or smaller by a step, remembered.
    fn resize_viz_dock(&mut self, grow: bool) {
        let Some(index) = self.focused_dock() else {
            return;
        };
        if !self.prefs.visuals[index].resize(grow) {
            self.status = format!(
                "visuals {} is as {} as it goes",
                index + 1,
                if grow { "big" } else { "small" }
            );
            return;
        }
        self.save_prefs_soon();
        self.invalidate_maps();
        let dock = &self.prefs.visuals[index];
        self.status = format!(
            "visuals {} · {} {}",
            index + 1,
            dock.extent(),
            if dock.edge.is_column() {
                "cells"
            } else {
                "rows"
            }
        );
    }

    /// Shift with an arrow: the chosen widget swaps places with its
    /// neighbour, the selection going with it.
    fn viz_move_widget(&mut self, forwards: bool) {
        let Some(index) = self.focused_dock() else {
            return;
        };
        let Some(chosen) = self.viz_docks[index].as_ref().map(|panel| panel.selected) else {
            return;
        };
        let count = self.prefs.visuals[index].widgets.len();
        let Some(other) = (if forwards {
            chosen.checked_add(1).filter(|other| *other < count)
        } else {
            chosen.checked_sub(1)
        }) else {
            return;
        };
        let mut widgets = self.prefs.visuals[index].widgets.clone();
        widgets.swap(chosen, other);
        self.set_widgets(index, widgets);
        if let Some(panel) = self.viz_docks[index].as_mut() {
            panel.selected = other;
            // The style memory is kept by position, so it has to travel
            // with its widget: left behind, the two swap their held traces
            // and a sweep draws a radar hand it never measured.
            let mut memory = panel.memory.borrow_mut();
            if chosen < memory.len() && other < memory.len() {
                memory.swap(chosen, other);
            }
        }
        self.status = format!("moved to {}", other + 1);
    }

    /// ↑/↓ on a widget: the next kind along, in its place.
    fn viz_step_kind(&mut self, forwards: bool) {
        let Some(index) = self.focused_dock() else {
            return;
        };
        let Some(chosen) = self.viz_docks[index].as_ref().map(|panel| panel.selected) else {
            return;
        };
        let mut widgets = self.prefs.visuals[index].widgets.clone();
        let Some(spec) = widgets.get_mut(chosen) else {
            return;
        };
        spec.step_kind(forwards);
        self.status = spec.title();
        self.set_widgets(index, widgets);
    }

    /// Enter on the art: a sheet for its text, the set's name offered.
    fn open_art_text_prompt(&mut self) {
        let Some(index) = self.focused_dock() else {
            return;
        };
        let set_name = self.scenes.name();
        let Some(chosen) = self.viz_docks[index].as_ref().map(|panel| panel.selected) else {
            return;
        };
        let Some(spec) = self.prefs.visuals[index].widgets.get(chosen) else {
            return;
        };
        if spec.kind != WidgetKind::Art {
            self.status = format!("the {} has no text; the art does", spec.kind.name());
            return;
        }
        let mut picker = FilePicker::text("the art's text", "writes it");
        picker.offer(spec.art_text(&set_name));
        picker.error = art_text_error(&picker.path).map(str::to_owned);
        self.dismiss_dialogs(Some(PanelKind::Viz));
        if let Some(panel) = self.viz_docks[index].as_mut() {
            panel.prompt = Some(picker);
        }
        self.status =
            "paste multiline art or type a banner · Enter writes it · empty for the set's name · Esc back".into();
    }

    /// A paste belongs to the art prompt as one edit, including its newlines.
    pub(super) fn paste_viz_prompt(&mut self, text: &str) -> bool {
        if self.focus != Focus::Panel(PanelKind::Viz) {
            return false;
        }
        let Some(index) = self.focused_dock() else {
            return false;
        };
        let Some(picker) = self.viz_docks[index]
            .as_mut()
            .and_then(|panel| panel.prompt.as_mut())
        else {
            return false;
        };
        let selection = picker.selection().unwrap_or(picker.caret..picker.caret);
        let byte_at = |index| {
            picker
                .path
                .char_indices()
                .nth(index)
                .map_or(picker.path.len(), |(at, _)| at)
        };
        let (from, to) = (byte_at(selection.start), byte_at(selection.end));
        let new_len = picker
            .path
            .len()
            .saturating_sub(to - from)
            .saturating_add(text.len());
        let error = if new_len > ART_MAX_BYTES {
            Some("artwork limit: 64 KiB; shorten the text before pasting")
        } else {
            let mut proposed = String::with_capacity(new_len);
            proposed.push_str(&picker.path[..from]);
            proposed.push_str(text);
            proposed.push_str(&picker.path[to..]);
            art_text_error(&proposed)
        };
        if let Some(error) = error {
            picker.error = Some(error.to_owned());
            self.status = error.to_owned();
        } else {
            picker.paste_multiline(text);
        }
        self.dirty_frame = true;
        true
    }

    fn align_viz_art(&mut self) -> bool {
        let Some(index) = self.focused_dock() else {
            return false;
        };
        let Some(panel) = self.viz_docks[index].as_ref() else {
            return false;
        };
        let mut widgets = self.prefs.visuals[index].widgets.clone();
        let Some(spec) = widgets
            .get_mut(panel.selected)
            .filter(|spec| spec.kind == WidgetKind::Art)
        else {
            return false;
        };
        spec.alignment = spec.alignment.next();
        self.status = spec.title_at(panel.edge);
        self.set_widgets(index, widgets);
        true
    }

    /// The typed text goes to the chosen art widget; empty, or the set's
    /// own name as offered, means the set's name - whatever set is open.
    fn take_art_text(&mut self, index: usize, text: &str) {
        let Some(chosen) = self.viz_docks[index].as_ref().map(|panel| panel.selected) else {
            return;
        };
        let set_name = self.scenes.name();
        let mut widgets = self.prefs.visuals[index].widgets.clone();
        if let Some(spec) = widgets.get_mut(chosen) {
            spec.set_text(if !text.contains('\n') && text.trim() == set_name {
                ""
            } else {
                text
            });
            self.status = if spec.text.is_some() {
                format!("the art writes {:?}", spec.art_text(""))
            } else {
                "the art writes the set's name".to_owned()
            };
        }
        self.set_widgets(index, widgets);
    }

    /// The focused dock's keys. Returns true when the dock consumed the key.
    pub(super) fn handle_viz_key(&mut self, code: KeyCode, primary: bool, shift: bool) -> bool {
        let Some(index) = self.focused_dock() else {
            return false;
        };
        let count = self.prefs.visuals[index].widgets.len();
        let Some(panel) = self.viz_docks[index].as_mut() else {
            return false;
        };
        // The art's text being typed: the sheet owns the keyboard.
        if let Some(picker) = panel.prompt.as_mut() {
            if code == KeyCode::Enter && !primary {
                if let Some(error) = art_text_error(&picker.path) {
                    picker.error = Some(error.to_owned());
                    self.status = error.to_owned();
                    self.dirty_frame = true;
                    return true;
                }
                // Artwork is text, not a filesystem path: keep indentation,
                // trailing newlines and literal tildes exactly as entered.
                let text = picker.path.clone();
                panel.prompt = None;
                self.take_art_text(index, &text);
                self.dirty_frame = true;
                return true;
            }
            match picker_key(picker, code, primary, shift, self.frame) {
                PickerKey::Ignored => return !primary,
                PickerKey::Handled => {}
                PickerKey::Back => {
                    panel.prompt = None;
                    self.status = "the art keeps its text".into();
                }
                PickerKey::Chose(path) => {
                    panel.prompt = None;
                    let text = path.to_string_lossy().into_owned();
                    self.take_art_text(index, &text);
                }
            }
            self.dirty_frame = true;
            return true;
        }
        // A kind being chosen to add.
        if let Some(chosen) = panel.adding {
            let kinds = WidgetKind::ALL.len();
            match code {
                KeyCode::Left | KeyCode::Up => panel.adding = Some((chosen + kinds - 1) % kinds),
                KeyCode::Right | KeyCode::Down => panel.adding = Some((chosen + 1) % kinds),
                KeyCode::Enter if !primary => {
                    panel.adding = None;
                    self.viz_add(WidgetKind::ALL[chosen]);
                }
                KeyCode::Esc => {
                    panel.adding = None;
                    self.status = "add cancelled".into();
                }
                _ => return false,
            }
            self.dirty_frame = true;
            return true;
        }
        // The mixer's keys: the arrows drive the selected strip's fader,
        // Enter walks the strips, 0 puts one back. Shift keeps the dock's
        // own meaning, so a mixer can still be moved.
        if self.focused_widget_kind() == Some(WidgetKind::Mixer) && !shift {
            match code {
                KeyCode::Left => {
                    self.nudge_mixer(-FADER_KEY_STEP_DB);
                    return true;
                }
                KeyCode::Right => {
                    self.nudge_mixer(FADER_KEY_STEP_DB);
                    return true;
                }
                KeyCode::Enter if !primary => {
                    self.mixer_next_strip();
                    return true;
                }
                KeyCode::Char('0') if !primary => {
                    self.reset_mixer_strip();
                    return true;
                }
                _ => {}
            }
        }
        let Some(panel) = self.viz_docks[index].as_mut() else {
            return false;
        };
        match code {
            KeyCode::Esc => {
                panel.error = None;
                self.focus = Focus::Editor;
                self.status = format!(
                    "back to the score - the dock stays; View {} Visuals {} hides it",
                    crate::terminal::symbol("▸"),
                    index + 1
                );
            }
            KeyCode::Tab => panel.move_by(1, count),
            KeyCode::BackTab => panel.move_by(-1, count),
            KeyCode::Home => panel.selected = 0,
            KeyCode::End => panel.selected = count.saturating_sub(1),
            KeyCode::Up | KeyCode::Left if shift => self.viz_move_widget(false),
            KeyCode::Down | KeyCode::Right if shift => self.viz_move_widget(true),
            KeyCode::Up => self.viz_step_kind(false),
            KeyCode::Down => self.viz_step_kind(true),
            KeyCode::Left => self.viz_step(false, true),
            KeyCode::Right => self.viz_step(true, true),
            KeyCode::Char(' ') if !primary => self.viz_step(true, false),
            KeyCode::Char('j' | 'J') if !primary => {
                if !self.align_viz_art() {
                    return false;
                }
            }
            KeyCode::Enter if !primary => self.open_art_text_prompt(),
            KeyCode::Char('+' | '=') if !primary => {
                self.resize_viz_dock(true);
                self.viz_unlaid[index] = true;
            }
            KeyCode::Char('-' | '_') if !primary => {
                self.resize_viz_dock(false);
                self.viz_unlaid[index] = true;
            }
            KeyCode::Char('a' | 'A') if !primary => {
                panel.adding = Some(0);
                panel.error = None;
                self.dismiss_dialogs(Some(PanelKind::Viz));
                self.status = "add a widget - ↑/↓ choose its kind, Enter adds it, Esc back".into();
            }
            KeyCode::Char('e' | 'E') if !primary => {
                self.move_viz_dock();
                self.viz_unlaid[index] = true;
            }
            KeyCode::Delete | KeyCode::Backspace if !primary => {
                let chosen = panel.selected;
                self.viz_remove(chosen);
            }
            KeyCode::PageUp => self.viz_scroll(-1),
            KeyCode::PageDown => self.viz_scroll(1),
            _ => return false,
        }
        self.dirty_frame = true;
        true
    }

    /// Scroll the focused column a page at a time.
    fn viz_scroll(&mut self, pages: i32) {
        let Some(index) = self.focused_dock() else {
            return;
        };
        let name = self.scenes.name();
        let area = self.regions.viz[index];
        let widgets = &self.prefs.visuals[index].widgets;
        let Some(panel) = self.viz_docks[index].as_mut() else {
            return;
        };
        let Some(parts) = VizPanel::parts(area, panel.edge) else {
            return;
        };
        let rows = i32::from(parts.body.height.max(1));
        panel.scroll_by(pages * rows, widgets, &name, parts.body);
    }

    /// ←/→ and Space on the chosen widget: its style and its colour.
    fn viz_step(&mut self, forwards: bool, style: bool) {
        let Some(index) = self.focused_dock() else {
            return;
        };
        let Some(chosen) = self.viz_docks[index].as_ref().map(|panel| panel.selected) else {
            return;
        };
        let mut widgets = self.prefs.visuals[index].widgets.clone();
        let Some(spec) = widgets.get_mut(chosen) else {
            return;
        };
        let changed = if style {
            spec.step_style(forwards)
        } else {
            spec.step_colour(forwards)
        };
        if !changed {
            self.status = format!("the {} has no style to step", spec.kind.name());
            return;
        }
        self.status = spec.title();
        self.set_widgets(index, widgets);
    }

    /// Enter on a kind: the widget joins the focused dock after the
    /// chosen one.
    pub(super) fn viz_add(&mut self, kind: WidgetKind) {
        let Some(index) = self.focused_dock() else {
            return;
        };
        let at = self.viz_docks[index]
            .as_ref()
            .map_or(0, |panel| panel.selected.saturating_add(1))
            .min(self.prefs.visuals[index].widgets.len());
        let mut widgets = self.prefs.visuals[index].widgets.clone();
        widgets.insert(at, WidgetSpec::new(kind));
        self.set_widgets(index, widgets);
        if let Some(panel) = self.viz_docks[index].as_mut() {
            panel.selected = at;
        }
        self.status = format!("added {}", kind.name());
    }

    /// Delete on a widget: it leaves the focused dock.
    fn viz_remove(&mut self, chosen: usize) {
        let Some(index) = self.focused_dock() else {
            return;
        };
        let mut widgets = self.prefs.visuals[index].widgets.clone();
        if chosen >= widgets.len() {
            return;
        }
        let gone = widgets.remove(chosen);
        let count = widgets.len();
        self.set_widgets(index, widgets);
        if let Some(panel) = self.viz_docks[index].as_mut() {
            panel.clamp(count);
            // As when they swap: the memory is kept by position, so the
            // removed widget's entry goes with it or every widget after it
            // inherits its neighbour's held trace.
            let mut memory = panel.memory.borrow_mut();
            if chosen < memory.len() {
                memory.remove(chosen);
            }
        }
        self.status = format!("removed {}", gone.kind.name());
    }

    /// The widget under a point of a dock.
    pub(super) fn viz_widget_at(&self, index: usize, x: u16, y: u16) -> Option<usize> {
        let panel = self.viz_docks[index].as_ref()?;
        let parts = VizPanel::parts(self.regions.viz[index], panel.edge)?;
        panel.widget_at(
            &self.prefs.visuals[index].widgets,
            &self.scenes.name(),
            parts.body,
            x,
            y,
        )
    }

    /// A click on a dock's text sheet keeps the keyboard on it. Returns
    /// true when the sheet took the press.
    pub(super) fn click_viz_prompt(&mut self, x: u16, y: u16, shift: bool) -> bool {
        let frame = self.frame;
        let Some(index) = (0..DOCKS).find(|&index| {
            self.viz_docks[index]
                .as_ref()
                .is_some_and(|panel| panel.prompt.is_some())
        }) else {
            return false;
        };
        let Some(picker) = self.viz_docks[index]
            .as_mut()
            .and_then(|panel| panel.prompt.as_mut())
        else {
            return false;
        };
        let held = if let Some(at) = picker.field_at(frame, x, y) {
            picker.caret_to(at, shift);
            Pointer::PromptField
        } else if picker.contains(frame, x, y) {
            Pointer::Panel
        } else {
            return false;
        };
        self.viz_focus = index;
        self.focus_panel(PanelKind::Viz);
        self.pointer = Some(held);
        self.dirty_frame = true;
        true
    }

    /// A click on a dock's add sheet chooses a kind, and a second click
    /// on the chosen one adds it. Returns true when the sheet took the
    /// press.
    pub(super) fn click_viz_add_sheet(&mut self, x: u16, y: u16) -> bool {
        let frame = self.frame;
        let Some(index) = (0..DOCKS).find(|&index| {
            self.viz_docks[index]
                .as_ref()
                .is_some_and(|panel| panel.adding.is_some())
        }) else {
            return false;
        };
        let Some(panel) = self.viz_docks[index].as_mut() else {
            return false;
        };
        if let Some(row) = panel.add_row_at(frame, x, y) {
            let again = panel.adding == Some(row);
            panel.adding = Some(row);
            self.viz_focus = index;
            self.focus_panel(PanelKind::Viz);
            self.pointer = Some(Pointer::Panel);
            self.dirty_frame = true;
            if again {
                if let Some(panel) = self.viz_docks[index].as_mut() {
                    panel.adding = None;
                }
                self.viz_add(WidgetKind::ALL[row]);
            }
            return true;
        }
        if panel.add_sheet_contains(frame, x, y) {
            self.viz_focus = index;
            self.focus_panel(PanelKind::Viz);
            self.pointer = Some(Pointer::Panel);
            return true;
        }
        false
    }

    /// A click on a dock chooses the widget under it and gives the dock
    /// the keyboard. On the mixer, a press on a strip's row selects that
    /// strip. Returns true when it took the press.
    pub(super) fn click_viz_panel(&mut self, x: u16, y: u16) -> bool {
        let Some(index) = self.viz_dock_at(x, y) else {
            return false;
        };
        let chosen = self.viz_widget_at(index, x, y);
        if let Some(panel) = self.viz_docks[index].as_mut() {
            if let Some(widget) = chosen {
                panel.selected = widget;
            }
            panel.error = None;
        }
        self.viz_focus = index;
        self.clear_other_viz_sheets(index);
        self.focus_panel(PanelKind::Viz);
        self.pointer = Some(Pointer::Panel);
        if let Some(target) = self.mixer_strip_at(index, x, y) {
            self.mixer.selected = target;
        }
        self.dirty_frame = true;
        true
    }

    /// The screen rows and columns a widget of a dock draws in - its
    /// slot without the header row - or none while it is off screen.
    pub(super) fn viz_widget_rect(&self, index: usize, widget: usize) -> Option<Rect> {
        let panel = self.viz_docks[index].as_ref()?;
        let parts = VizPanel::parts(self.regions.viz[index], panel.edge)?;
        let widgets = &self.prefs.visuals[index].widgets;
        let slot = if panel.edge.is_column() {
            let (top, height) =
                *VizPanel::stack_in(widgets, &self.scenes.name(), parts.body).get(widget)?;
            let top = i32::from(top) - i32::from(panel.scroll);
            let bottom = top + i32::from(height);
            let visible_top = top.max(0);
            let visible_bottom = bottom.min(i32::from(parts.body.height));
            if visible_bottom <= visible_top {
                return None;
            }
            Rect::new(
                parts.body.x,
                parts.body.y + visible_top as u16,
                parts.body.width,
                (visible_bottom - visible_top) as u16,
            )
        } else {
            *VizPanel::band_slots(widgets.len(), parts.body).get(widget)?
        };
        (slot.height > 1).then(|| Rect::new(slot.x, slot.y + 1, slot.width, slot.height - 1))
    }

    /// The strip under a point of a dock's mixer widget: a row each, in
    /// the order the desk has them.
    pub(super) fn mixer_strip_at(&self, index: usize, x: u16, y: u16) -> Option<MixerTarget> {
        let widget = self.viz_widget_at(index, x, y)?;
        if self.prefs.visuals[index].widgets.get(widget)?.kind != WidgetKind::Mixer {
            return None;
        }
        let area = self.viz_widget_rect(index, widget)?;
        if !within(area, x, y) {
            return None;
        }
        self.mixer_targets().get(usize::from(y - area.y)).copied()
    }

    /// The fader under a point of a dock's mixer, if the strip there has
    /// one: an orbit's level is the score's, so its row takes no wheel.
    pub(super) fn mixer_fader_at(&self, index: usize, x: u16, y: u16) -> Option<MixerTarget> {
        let target = self.mixer_strip_at(index, x, y)?;
        target.has_fader().then_some(target)
    }

    /// The wheel over a strip with a fader nudges it, half a decibel a
    /// notch, the master's way.
    pub(super) fn scroll_mixer_fader(&mut self, x: u16, y: u16, direction: f32) -> bool {
        let Some(index) = self.viz_dock_at(x, y) else {
            return false;
        };
        let Some(target) = self.mixer_fader_at(index, x, y) else {
            return false;
        };
        self.mixer.selected = target;
        self.nudge_mixer(direction * VOLUME_SCROLL_STEP_DB);
        true
    }

    /// The wheel over a column scrolls it.
    pub(super) fn scroll_viz_panel(&mut self, x: u16, y: u16, direction: f32) -> bool {
        let Some(index) = self.viz_dock_at(x, y) else {
            return false;
        };
        let name = self.scenes.name();
        let area = self.regions.viz[index];
        let widgets = &self.prefs.visuals[index].widgets;
        let Some(panel) = self.viz_docks[index].as_mut() else {
            return false;
        };
        let Some(parts) = VizPanel::parts(area, panel.edge) else {
            return false;
        };
        panel.scroll_by(
            if direction > 0.0 { -3 } else { 3 },
            widgets,
            &name,
            parts.body,
        );
        self.dirty_frame = true;
        true
    }

    /// The kind of widget under the keys in the focused dock.
    pub(super) fn focused_widget_kind(&self) -> Option<WidgetKind> {
        let index = self.viz_focus;
        let selected = self.viz_docks[index].as_ref()?.selected;
        self.prefs.visuals[index]
            .widgets
            .get(selected)
            .map(|widget| widget.kind)
    }
}
