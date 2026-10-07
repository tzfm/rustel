//! The log panel (F9). It covers opening and closing the log, docking it to an
//! edge with Shift+F9 and moving the dock with `e`, bringing a docked log back
//! at startup, the frame and extent every log draw, scroll and hit test
//! measures against, the log's own keys (scroll, copy, verbose, Esc), and
//! mouse selection inside it.

use super::super::editor::GridRect;
use super::sets::step_fixture_band;
use super::*;

impl App {
    /// Whether the log counts as a stop on ⇧F10's rotation: docked, not
    /// merely open, so a sheet glanced at and about to close again is not
    /// one more stop to walk past.
    pub(super) fn log_is_docked(&self) -> bool {
        // Zen has no docked furniture: a log left sticky from before zen
        // turned on is drawn as a full-frame sheet (`log_sheet_frame`),
        // not the carved band this name means, so it must not go on
        // answering as docked. A docked answer would keep it out of Esc's
        // reach through `front_panel` and off `handle_log_key`'s closing
        // arm: an overlay covering the score that no key could put away.
        !self.ui_settings.zen && self.log_panel.as_ref().is_some_and(|panel| panel.sticky)
    }

    /// The frame the log anchors against: `self.regions.log` - the room the
    /// layout carved it, outside the mixer, beside the panes - once it is
    /// docked furniture rather than a sheet; otherwise everything short of
    /// the footer, so the sheet bottom-anchors over the score and the
    /// footer - transport, dock, status line - stays visible under it.
    /// Every geometry and hit-test call agrees with the paint through this
    /// one rect.
    pub(super) fn log_sheet_frame(&self) -> Rect {
        // Zen has no docked furniture: a log left sticky from before zen
        // turned on must not go on reading `self.regions.log`, which zen
        // never carves. That rect is empty in zen, so the log would vanish,
        // keys and all, instead of coming up as the popup every other
        // panel already is in zen.
        if !self.ui_settings.zen && self.log_panel.as_ref().is_some_and(|panel| panel.sticky) {
            return self.regions.log;
        }
        Rect {
            height: self.frame.height.saturating_sub(self.regions.footer.height),
            ..self.frame
        }
    }

    /// What the log sheet has to show, which is what decides how tall it
    /// is and where its list starts.
    ///
    /// Hit-testing, scrolling and drawing all have to agree on both, so
    /// they all ask here.
    pub(super) fn log_extent(&self) -> super::super::log::LogExtent {
        let pressure = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.pressure.as_ref());
        match self.log_panel.as_ref() {
            Some(panel) => {
                let mut extent = super::super::log::LogExtent::of(&self.log, panel, pressure);
                // Only when the layout actually found it a room: a sticky
                // log on a terminal too small for another band falls back
                // to the sheet, and its geometry has to fall back with it.
                extent.docked = panel.sticky && !self.regions.log.is_empty();
                extent
            }
            None => super::super::log::LogExtent::default(),
        }
    }

    /// The log's text is brought up to the log and sized to its rows
    /// before the frame is drawn, so drawing it needs nothing mutable.
    pub(super) fn prepare_log_sheet(&mut self) {
        let log_frame = self.log_sheet_frame();
        if let Some(list) = LogPanelView::list_area(log_frame, self.log_extent())
            && let Some(panel) = self.log_panel.as_mut()
        {
            panel.prepare(&self.log, list);
        }
        // The first prepare learns how many visual rows wrapped entries
        // occupy. Recompute once so this very frame gets the matching sheet
        // height instead of following past an older line until the next draw.
        if let Some(list) = LogPanelView::list_area(log_frame, self.log_extent())
            && let Some(panel) = self.log_panel.as_mut()
        {
            panel.prepare(&self.log, list);
        }
    }

    /// ⇧F9: keeps the log on screen as docked furniture - like a visuals
    /// dock or the mixer - instead of a sheet other sheets displace and
    /// `dismiss_dialogs` closes. Opens the log first when it is closed;
    /// toggling it off on an already-open log leaves the log open, as a
    /// sheet again.
    pub(super) fn toggle_log_sticky(&mut self) {
        if self.log_panel.is_none() {
            self.dismiss_dialogs(Some(PanelKind::Log));
            self.log.mark_seen();
            self.log_panel = Some(LogPanel::opened(self.ui_settings.log_verbose));
        }
        let Some(panel) = self.log_panel.as_mut() else {
            return;
        };
        panel.sticky = !panel.sticky;
        // The edge remembered from before, or wherever the panel already
        // sat. A side kept from when the log could be a column is bottom.
        if panel.sticky {
            panel.edge = self.prefs.log_edge.unwrap_or(panel.edge).band();
            panel.height = self.prefs.log_height;
        }
        let sticky = panel.sticky;
        let edge = panel.edge;
        self.prefs.log_sticky = Some(sticky);
        self.save_prefs_soon();
        self.invalidate_maps();
        self.focus_panel(PanelKind::Log);
        self.status = if sticky {
            format!(
                "log docked at the {} - e top/bottom · -/+ height · ↑/↓/PgUp/PgDn/Home/End scroll · Esc leaves the keyboard",
                edge.name()
            )
        } else {
            "log - a sheet again; Esc closes it".into()
        };
        self.dirty_frame = true;
    }

    /// The log comes back the way it was left, alongside the set panel,
    /// the visuals docks and the mixer. Called only when `prefs.log_sticky`
    /// says the docked log survived the last session; a log merely open as
    /// a sheet never did, and still does not.
    pub(super) fn restore_sticky_log(&mut self) {
        if self.prefs.log_sticky != Some(true) {
            return;
        }
        self.log.mark_seen();
        let mut panel = LogPanel::opened(self.ui_settings.log_verbose);
        panel.sticky = true;
        panel.edge = self.prefs.log_edge.unwrap_or(panel.edge).band();
        panel.height = self.prefs.log_height;
        self.log_panel = Some(panel);
    }

    /// `e` on a sticky, focused log: the other band, top or bottom.
    fn move_log_dock(&mut self) {
        let Some(panel) = self.log_panel.as_mut() else {
            return;
        };
        if !panel.sticky {
            return;
        }
        let edge = panel.edge.flipped_band();
        panel.edge = edge;
        self.prefs.log_edge = Some(edge);
        self.save_prefs_soon();
        self.invalidate_maps();
        self.status = format!("log docked at the {}", edge.name());
        self.dirty_frame = true;
    }

    /// Resize a docked log by one visible row, respecting the room left for
    /// the score and other fixtures.
    fn resize_log_dock(&mut self, grow: bool) {
        let frame = self.frame;
        let Some(panel) = self.log_panel.as_ref().filter(|panel| panel.sticky) else {
            return;
        };
        let edge = panel.edge.band();
        let asked = panel.dock_height(frame.height);
        let band = |extent| super::super::viz_panel::Dock { edge, extent };
        let mixer = self.mixer_panel.map(MixerPanel::dock);
        let memory = self.memory_dock_request();
        let laid = |extent| {
            self.layout_with_fixtures(mixer, Some(band(extent)), memory)
                .log
                .height
        };
        let next = match step_fixture_band("the log", asked, grow, !frame.is_empty(), laid) {
            Ok(next) => next,
            Err(status) => {
                self.status = status;
                self.dirty_frame = true;
                return;
            }
        };
        if let Some(panel) = self.log_panel.as_mut() {
            panel.height = Some(next);
        }
        self.prefs.log_height = Some(next);
        self.save_prefs_soon();
        self.invalidate_maps();
        self.status = format!("log {next} rows tall");
        self.dirty_frame = true;
    }

    /// F9: the log sheet, or close it. Opening it is what clears the
    /// header's count.
    pub(super) fn toggle_log_panel(&mut self) {
        if self.log_panel.is_some() && self.focus != Focus::Panel(PanelKind::Log) {
            self.focus_panel(PanelKind::Log);
            return;
        }
        if self.log_panel.is_none() {
            self.dismiss_dialogs(Some(PanelKind::Log));
        }
        let was_sticky = self.log_panel.as_ref().is_some_and(|panel| panel.sticky);
        self.log_panel = match &self.log_panel {
            Some(_) => None,
            None => {
                self.log.mark_seen();
                self.count_for_the_breakdown();
                self.status = match self.log.path() {
                    Some(path) => format!(
                        "log - also in {}",
                        status_file_path(path, self.ui_settings.show_full_paths)
                    ),
                    None => "log".into(),
                };
                Some(LogPanel::opened(self.ui_settings.log_verbose))
            }
        };
        // F9 outright closes the log even while it is docked - the same
        // final word F4 has over the mixer - so a docked log that goes
        // this way must not come back sticky, unasked, on the next launch.
        if self.log_panel.is_none() && was_sticky {
            self.prefs.log_sticky = Some(false);
            self.save_prefs_soon();
        }
        if self.log_panel.is_some() {
            self.focus_panel(PanelKind::Log);
        } else {
            self.focus = Focus::Editor;
        }
        #[cfg(feature = "hydra")]
        self.sync_settings_webcam_preview();
        self.dirty_frame = true;
    }

    /// The header's memory figure opens the shared log and memory panel.
    pub(super) fn open_log_from_memory(&mut self) {
        if self.log_panel.is_none() {
            self.toggle_log_panel();
        } else {
            self.focus_panel(PanelKind::Log);
            self.dirty_frame = true;
        }
    }

    /// Returns true when the log sheet consumed the key.
    pub(super) fn handle_log_key(&mut self, code: KeyCode, primary: bool, shift: bool) -> bool {
        // The room the log actually has, which is the dock when it is
        // stuck to an edge. Measuring the whole frame here scrolled by a
        // sheet's worth of rows while the panel showed a band's worth, and
        // after `e` moved the log to the top it paged a list that was not
        // where the arithmetic thought it was.
        let rows = LogPanelView::rows(self.log_sheet_frame(), self.log_extent());
        let Some(panel) = self.log_panel.as_mut() else {
            return false;
        };
        // A sticky log is docked furniture: Esc and its Ctrl+Shift+D twin
        // hand the keyboard back to the score, the way the mixer's and the
        // visuals docks' Esc do, rather than closing something that is
        // meant to stay on screen. ⇧F9 or F9 is what puts it away.
        //
        // Zen has no docked furniture, though: a log left sticky from
        // before zen turned on is drawn as a popup over the whole frame
        // (`log_sheet_frame`), so Esc closes it outright there, the same
        // as every other zen popup, rather than leaving an overlay
        // standing with no ⇧F9 in sight to un-dock it by.
        let closes =
            code == KeyCode::Esc || (matches!(code, KeyCode::Char('d' | 'D')) && primary && shift);
        if panel.sticky && closes && !self.ui_settings.zen {
            self.focus = Focus::Editor;
            self.status = if let Some(chord) = self.keybinds.binding(BindAction::LogSticky) {
                format!(
                    "back to the score - the log stays docked; {} un-docks it",
                    chord.hint()
                )
            } else {
                format!(
                    "back to the score - the log stays docked; {} hides it",
                    self.shortcut_or_menu(BindAction::Log, "View > Log")
                )
            };
            self.dirty_frame = true;
            return true;
        }
        if panel.sticky && matches!(code, KeyCode::Char('e' | 'E')) && !primary {
            self.move_log_dock();
            return true;
        }
        // Zen draws a sticky log as a sheet, which has no band to size.
        if panel.sticky
            && !self.ui_settings.zen
            && matches!(code, KeyCode::Char('-' | '+' | '='))
            && !primary
        {
            self.resize_log_dock(code != KeyCode::Char('-'));
            return true;
        }
        match code {
            KeyCode::Esc => {
                self.log_panel = None;
                self.settle_focus();
            }
            KeyCode::Char('d' | 'D') if primary && shift => {
                self.log_panel = None;
                self.settle_focus();
            }
            // The selection the mouse pulled through the log goes to the
            // clipboard, like the score's.
            KeyCode::Char('c' | 'C') if primary => {
                let text = panel.editor.selected_text().unwrap_or_default();
                if text.is_empty() {
                    self.status = "nothing selected in the log".to_owned();
                } else {
                    let characters = text.chars().count();
                    self.status = match self.clipboard.set_text(text) {
                        Ok(()) => format!("copied {characters} characters of the log"),
                        Err(error) => format!("could not copy: {error}"),
                    };
                }
            }
            // `v` shows the running commentary as well, or puts it away.
            // Nothing is thrown away either way: the filter is on the
            // view, so what was hidden is still there and `studio.log`
            // holds every line regardless.
            KeyCode::Char('v' | 'V') if !primary => {
                let verbose = panel.toggle_verbose();
                self.ui_settings.log_verbose = verbose;
                self.prefs.log_verbose = Some(verbose);
                self.save_prefs_soon();
                self.status = if verbose {
                    "log: everything, including the running commentary".into()
                } else {
                    "log: what happened - v shows the commentary too".into()
                };
            }
            KeyCode::Up => panel.scroll(1, rows),
            KeyCode::Down => panel.scroll(-1, rows),
            KeyCode::PageUp => panel.scroll(rows as isize, rows),
            KeyCode::PageDown => panel.scroll(-(rows as isize), rows),
            KeyCode::Home => panel.top(rows),
            KeyCode::End => panel.follow(rows),
            _ => return false,
        }
        self.dirty_frame = true;
        true
    }

    /// A press on the log sheet is a claim on it; on its lines it starts a
    /// selection, and the drag that follows pulls it through the text like
    /// any editor. Returns true when the sheet took the press.
    pub(super) fn click_log_panel(&mut self, mouse: MouseEvent, x: u16, y: u16) -> bool {
        if self.log_panel.is_some()
            && super::super::log::LogPanelView::geometry(self.log_sheet_frame(), self.log_extent())
                .is_some_and(|(sheet, _)| within(sheet, x, y))
        {
            self.focus_panel(PanelKind::Log);
            if super::super::log::LogPanelView::list_area(self.log_sheet_frame(), self.log_extent())
                .is_some_and(|list| within(list, x, y))
            {
                self.own_text_selection(TextSurface::Log);
                self.forward_mouse_to_log(mouse);
                self.pointer = Some(Pointer::Log);
            } else {
                self.pointer = Some(Pointer::Panel);
            }
            return true;
        }
        false
    }

    /// The wheel over the log scrolls the log. Whether it took the wheel.
    pub(super) fn scroll_log_panel(&mut self, mouse: MouseEvent, x: u16, y: u16) -> bool {
        if self.log_panel.is_some()
            && super::super::log::LogPanelView::geometry(self.log_sheet_frame(), self.log_extent())
                .is_some_and(|(sheet, _)| within(sheet, x, y))
        {
            self.forward_mouse_to_log(mouse);
            return true;
        }
        false
    }

    /// Route a mouse event into the log's text, with the map rebuilt at the
    /// list's own grid - a press starts a selection, a drag pulls it and
    /// scrolls past the edges, the wheel scrolls - exactly as the score's
    /// editor behaves.
    pub(super) fn forward_mouse_to_log(&mut self, mouse: MouseEvent) {
        // The same room the widget paints into: `click_log_panel` decides a
        // click is the log's using `log_sheet_frame`, so rebuilding the
        // map against the full frame mapped the press to a different row
        // than the one under the pointer.
        let Some(list) = LogPanelView::list_area(self.log_sheet_frame(), self.log_extent()) else {
            return;
        };
        let rows = usize::from(list.height);
        let moment = self.moment();
        let Some(panel) = self.log_panel.as_mut() else {
            return;
        };
        panel.prepare(&self.log, list);
        let grid = GridRect::new(list.x, list.y, list.width, list.height);
        if let Ok(map) = panel.editor.screen_map(grid) {
            let _ = panel.editor.mouse_event(mouse, &map, moment);
        }
        panel.note_view(rows);
        self.dirty_frame = true;
    }
}
