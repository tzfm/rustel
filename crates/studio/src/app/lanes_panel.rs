//! The Lanes panel: the score's lanes with a playhead, docked as a band
//! along the top or the bottom. `e` moves it to the other edge, `-`
//! and `+` size it, and Esc gives the keyboard back to the score and leaves
//! it standing. It comes back at the next launch the way it was left.

use super::sets::step_fixture_band;
use super::*;
use crate::viz_panel::{BAND_DEFAULT_HEIGHT, BAND_MAX_HEIGHT, BAND_MIN_HEIGHT, Dock, Edge};

impl App {
    /// View ▸ Lanes, or a learnt chord: the panel, or hidden again.
    pub(super) fn toggle_lanes_panel(&mut self) {
        if self.lanes_dock.is_some() {
            self.close_panel(PanelKind::Lanes);
            return;
        }
        self.open_lanes_panel(true);
    }

    /// Dock the panel where it was left, with the keyboard or without it.
    /// On a terminal with no room for another band it is docked but not
    /// given the keyboard, and the status line says why nothing appeared.
    pub(super) fn open_lanes_panel(&mut self, focus: bool) {
        let dock = Dock {
            edge: self.prefs.lanes_edge.map_or(Edge::Bottom, Edge::band),
            extent: self
                .prefs
                .lanes_height
                .unwrap_or(BAND_DEFAULT_HEIGHT)
                .clamp(BAND_MIN_HEIGHT, BAND_MAX_HEIGHT),
        };
        self.lanes_dock = Some(dock);
        self.prefs.lanes_docked = Some(true);
        self.save_prefs_soon();
        self.invalidate_maps();
        self.dirty_frame = true;
        let room = self.frame.is_empty()
            || !self
                .layout_with_fixtures(
                    self.mixer_panel.map(MixerPanel::dock),
                    self.log_dock_request(),
                    self.memory_dock_request(),
                    Some(dock),
                )
                .lanes
                .is_empty();
        if !room {
            self.status = format!(
                "lanes: no room for another band on this terminal - make it taller, or {} hides it",
                self.shortcut_or_menu(BindAction::Lanes, "View > Lanes")
            );
            return;
        }
        // Laid at the next draw. The keys stay with it until then.
        self.lanes_unlaid = true;
        if focus {
            self.focus_panel(PanelKind::Lanes);
        }
        self.status = if !focus {
            format!("lanes docked at the {}", dock.edge.name())
        } else if self.ui_settings.zen {
            // Zen has no docked furniture: the band lies over the score,
            // and Esc puts it away.
            "lanes - e top/bottom · -/+ height · Esc closes it".into()
        } else {
            format!(
                "lanes docked at the {} - e top/bottom · -/+ height · Esc back to the score",
                dock.edge.name()
            )
        };
    }

    /// Put the panel away, and remember that it was.
    pub(super) fn close_lanes_panel(&mut self) {
        if self.lanes_dock.take().is_none() {
            return;
        }
        self.lanes_unlaid = false;
        self.prefs.lanes_docked = Some(false);
        self.save_prefs_soon();
        self.invalidate_maps();
        self.settle_focus();
        self.status = "lanes hidden".into();
        self.dirty_frame = true;
    }

    /// The panel's keys while it has the keyboard: `e` for the other band,
    /// `-` and `+` - or `=`, the same key without Shift - for its height,
    /// and Esc to hand the keyboard back to the score. In zen, where it is
    /// a popup over the score, Esc puts it away. Returns whether it took
    /// the key.
    pub(super) fn handle_lanes_key(&mut self, code: KeyCode, primary: bool) -> bool {
        if primary {
            return false;
        }
        match code {
            KeyCode::Esc if self.ui_settings.zen => self.close_lanes_panel(),
            KeyCode::Esc => {
                self.focus = Focus::Editor;
                self.status = format!(
                    "back to the score - the lanes stay docked, and {} hides them",
                    self.shortcut_or_menu(BindAction::Lanes, "View > Lanes")
                );
            }
            KeyCode::Char('e' | 'E') => self.move_lanes_panel(),
            KeyCode::Char('-' | '+' | '=') => self.resize_lanes_panel(code != KeyCode::Char('-')),
            _ => return false,
        }
        self.dirty_frame = true;
        true
    }

    /// `e`: the other band, remembered, as the mixer's and the log's `e`
    /// are.
    fn move_lanes_panel(&mut self) {
        let Some(dock) = self.lanes_dock.as_mut() else {
            return;
        };
        dock.edge = dock.edge.flipped_band();
        let edge = dock.edge;
        self.prefs.lanes_edge = Some(edge);
        self.save_prefs_soon();
        self.invalidate_maps();
        self.lanes_unlaid = true;
        self.status = format!("lanes along the {}", edge.name());
    }

    /// `-` and `+`: a row shorter or taller, remembered, stepped from the
    /// rows it has on screen and no taller than the terminal allows.
    fn resize_lanes_panel(&mut self, grow: bool) {
        let Some(dock) = self.lanes_dock else {
            return;
        };
        let mixer = self.mixer_panel.map(MixerPanel::dock);
        let log = self.log_dock_request();
        let memory = self.memory_dock_request();
        let laid = |extent| {
            self.layout_with_fixtures(mixer, log, memory, Some(Dock { extent, ..dock }))
                .lanes
                .height
        };
        let next = match step_fixture_band(
            "the lanes panel",
            dock.extent,
            grow,
            !self.frame.is_empty(),
            laid,
        ) {
            Ok(next) => next,
            Err(status) => {
                self.status = status;
                return;
            }
        };
        if let Some(dock) = self.lanes_dock.as_mut() {
            dock.extent = next;
        }
        self.prefs.lanes_height = Some(next);
        self.save_prefs_soon();
        self.invalidate_maps();
        self.lanes_unlaid = true;
        self.status = format!("lanes {next} rows tall");
    }

    /// Whether a point is on the panel.
    pub(super) fn lanes_panel_at(&self, x: u16, y: u16) -> bool {
        self.lanes_dock.is_some() && within(self.regions.lanes, x, y)
    }

    /// A press on the panel gives it the keyboard, as a press on the desk
    /// does. Returns whether it took the press.
    pub(super) fn click_lanes_panel(&mut self, x: u16, y: u16) -> bool {
        if !self.lanes_panel_at(x, y) {
            return false;
        }
        self.focus_panel(PanelKind::Lanes);
        self.pointer = Some(Pointer::Panel);
        self.dirty_frame = true;
        true
    }
}
