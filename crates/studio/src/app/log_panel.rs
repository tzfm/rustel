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

#[cfg(test)]
mod tests {
    mod log_panel {
        //! Tests for `app/log_panel.rs`.

        use crate::app::tests::support::*;
        use crate::app::*;

        /// In zen a sticky log has no docked region. It is a full-frame popup,
        /// `log_is_docked` is false, and Esc closes it.
        #[test]
        fn a_log_left_sticky_from_before_zen_is_a_popup_esc_can_reach() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            app.toggle_log_sticky();
            assert!(app.log_panel.as_ref().is_some_and(|panel| panel.sticky));
            app.focus = Focus::Editor;
            assert!(app.log_is_docked(), "still docked outside zen");

            press(&mut app, KeyCode::F(11), KeyModifiers::NONE);
            assert!(app.ui_settings.zen);
            assert!(
                !app.log_is_docked(),
                "a sticky log in zen must not answer as docked furniture"
            );
            assert_eq!(
                app.log_sheet_frame(),
                app.frame,
                "the log's sheet must cover the frame in zen, not the docked \
             region zen never carves"
            );
            assert_eq!(
                app.front_panel(),
                Some(PanelKind::Log),
                "zen has no docked furniture, so the log is a sheet Esc can reach"
            );
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(
                app.log_panel.is_none(),
                "Esc did not close the sticky log's zen popup"
            );
        }

        /// The same log, focused rather than merely open: Esc while the
        /// keyboard is on it must close it outright in zen too, not just
        /// hand the keyboard back the way it does docked.
        #[test]
        fn esc_closes_a_focused_sticky_log_in_zen_instead_of_merely_unfocusing_it() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            press(&mut app, KeyCode::F(11), KeyModifiers::NONE);
            app.toggle_log_sticky();
            assert!(app.log_panel.is_some());
            assert_eq!(app.focus, Focus::Panel(PanelKind::Log));

            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(
                app.log_panel.is_none(),
                "Esc did not close the focused sticky log in zen"
            );
            assert_eq!(app.focus, Focus::Editor);
        }

        /// ⇧F9 opens the log if it was closed and docks it - furniture like
        /// the set panel and the mixer, not a sheet `dismiss_dialogs` puts
        /// away when some other dialog opens.
        #[test]
        fn a_sticky_log_is_not_closed_by_dismiss_dialogs() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 140, 40);
            assert!(app.log_panel.is_none());

            press(&mut app, KeyCode::F(9), KeyModifiers::SHIFT);
            assert!(app.log_panel.is_some(), "⇧F9 opens the log too");
            assert!(
                app.log_panel.as_ref().unwrap().sticky,
                "and docks it, first press"
            );
            assert_eq!(app.prefs.log_sticky, Some(true));

            app.dismiss_dialogs(None);
            assert!(
                app.log_panel.is_some(),
                "sticky: dismiss_dialogs must leave it be"
            );

            // The control: turned back into a plain sheet, the very same call
            // closes it, the way it always closed a floating log.
            app.log_panel.as_mut().unwrap().sticky = false;
            app.dismiss_dialogs(None);
            assert!(
                app.log_panel.is_none(),
                "not sticky: dismiss_dialogs closes it like any other sheet"
            );
        }

        /// `e` on a sticky log flips its band between bottom and top.
        #[test]
        fn e_flips_the_sticky_log_between_top_and_bottom_and_the_region_moves() {
            use crate::viz_panel::Edge;
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 140, 40);
            let layout = |app: &App| {
                app.layout_with_fixtures(
                    app.mixer_panel.map(MixerPanel::dock),
                    app.log_dock_request(),
                    app.memory_dock_request(),
                )
                .log
            };

            press(&mut app, KeyCode::F(9), KeyModifiers::SHIFT);
            assert!(app.log_panel.as_ref().unwrap().sticky);
            assert_eq!(app.log_panel.as_ref().unwrap().edge, Edge::Bottom);
            let bottom = layout(&app);
            assert!(!bottom.is_empty(), "a sticky log keeps a region");
            assert_eq!(bottom.width, app.frame.width);

            press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
            assert_eq!(app.log_panel.as_ref().unwrap().edge, Edge::Top);
            assert_eq!(app.prefs.log_edge, Some(Edge::Top));
            let top = layout(&app);
            assert!(top.y < bottom.y);
            assert_eq!(top.width, app.frame.width);

            press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
            assert_eq!(app.log_panel.as_ref().unwrap().edge, Edge::Bottom);
            assert_eq!(app.prefs.log_edge, Some(Edge::Bottom));
            assert_eq!(layout(&app), bottom);
        }

        fn laid_log(app: &App) -> Rect {
            app.layout_with_fixtures(
                app.mixer_panel.map(MixerPanel::dock),
                app.log_dock_request(),
                app.memory_dock_request(),
            )
            .log
        }

        #[test]
        fn the_docked_log_takes_a_third_of_the_terminal_by_default() {
            use crate::viz_panel::BAND_DEFAULT_HEIGHT;
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 140, 40);
            press(&mut app, KeyCode::F(9), KeyModifiers::SHIFT);
            assert_eq!(laid_log(&app).height, 13);

            app.frame = Rect::new(0, 0, 140, 25);
            assert_eq!(laid_log(&app).height, BAND_DEFAULT_HEIGHT);
            app.frame = Rect::new(0, 0, 140, 60);
            assert_eq!(laid_log(&app).height, 20);
        }

        #[test]
        fn minus_and_plus_resize_the_docked_log_and_the_height_is_kept() {
            use crate::viz_panel::BAND_MIN_HEIGHT;
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 140, 40);
            press(&mut app, KeyCode::F(9), KeyModifiers::SHIFT);
            assert_eq!(laid_log(&app).height, 13);

            press(&mut app, KeyCode::Char('+'), KeyModifiers::SHIFT);
            assert_eq!(laid_log(&app).height, 14);
            assert_eq!(app.prefs.log_height, Some(14));
            press(&mut app, KeyCode::Char('='), KeyModifiers::NONE);
            assert_eq!(laid_log(&app).height, 15);

            for _ in 0..20 {
                press(&mut app, KeyCode::Char('-'), KeyModifiers::NONE);
            }
            assert_eq!(laid_log(&app).height, BAND_MIN_HEIGHT);
            assert!(app.status.contains("as short as a band goes"));
            press(&mut app, KeyCode::Char('+'), KeyModifiers::SHIFT);
            press(&mut app, KeyCode::Char('+'), KeyModifiers::SHIFT);
            assert_eq!(app.prefs.log_height, Some(BAND_MIN_HEIGHT + 2));

            let prefs = app.prefs.clone();
            let mut next = app_over(directory.path());
            next.prefs = prefs;
            next.frame = app.frame;
            next.restore_panels_from_prefs();
            assert_eq!(
                next.log_panel.as_ref().unwrap().height,
                Some(BAND_MIN_HEIGHT + 2)
            );
            assert_eq!(laid_log(&next).height, BAND_MIN_HEIGHT + 2);
        }

        #[test]
        fn the_docked_log_grows_only_as_far_as_the_terminal_allows() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 140, 24);
            press(&mut app, KeyCode::F(9), KeyModifiers::SHIFT);
            assert_eq!(laid_log(&app).height, 9);
            press(&mut app, KeyCode::Char('+'), KeyModifiers::SHIFT);
            press(&mut app, KeyCode::Char('+'), KeyModifiers::SHIFT);
            assert_eq!(laid_log(&app).height, 11);
            press(&mut app, KeyCode::Char('+'), KeyModifiers::SHIFT);
            assert_eq!(laid_log(&app).height, 11);
            assert!(app.status.contains("as tall as this terminal allows"));

            app.log_panel.as_mut().unwrap().height = Some(20);
            assert_eq!(laid_log(&app).height, 11);
            press(&mut app, KeyCode::Char('-'), KeyModifiers::NONE);
            assert_eq!(laid_log(&app).height, 10);
            assert_eq!(app.prefs.log_height, Some(10));
        }

        #[test]
        fn a_log_kept_docked_at_a_side_comes_back_along_the_bottom() {
            use crate::viz_panel::Edge;
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 140, 40);
            for side in [Edge::Left, Edge::Right] {
                app.log_panel = None;
                app.prefs.log_sticky = Some(true);
                app.prefs.log_edge = Some(side);
                app.restore_panels_from_prefs();
                assert_eq!(app.log_panel.as_ref().unwrap().edge, Edge::Bottom);
                assert_eq!(laid_log(&app).width, app.frame.width);
            }
            app.focus = Focus::Panel(PanelKind::Log);
            press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
            assert_eq!(app.prefs.log_edge, Some(Edge::Top));
        }

        /// The log's own scroll keys - the same ones the panel already
        /// answers to when it is a sheet - reach it only while it holds the
        /// keyboard; with the score focused they edit the score as usual.
        #[test]
        fn the_logs_scroll_keys_reach_it_only_while_it_has_the_keyboard() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 140, 40);
            for index in 0..40 {
                app.log
                    .push(LogLevel::Info, "engine", format!("line {index}"));
            }
            press(&mut app, KeyCode::F(9), KeyModifiers::NONE);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Log));
            let list =
                LogPanelView::list_area(app.log_sheet_frame(), app.log_extent()).expect("list");
            app.log_panel.as_mut().unwrap().prepare(&app.log, list);
            assert!(app.log_panel.as_ref().unwrap().following);

            press(&mut app, KeyCode::Up, KeyModifiers::NONE);
            assert!(
                !app.log_panel.as_ref().unwrap().following,
                "Up scrolled it while it was focused"
            );

            // The control: the very same key, with the score focused instead,
            // must not reach the log at all.
            press(&mut app, KeyCode::End, KeyModifiers::NONE);
            assert!(app.log_panel.as_ref().unwrap().following, "End caught up");
            app.focus = Focus::Editor;
            press(&mut app, KeyCode::Up, KeyModifiers::NONE);
            assert!(
                app.log_panel.as_ref().unwrap().following,
                "Up with the score focused never reached the log"
            );
            assert_eq!(app.focus, Focus::Editor, "and stayed the score's key");
        }

        /// The log's lines are text: a drag through them selects, the wheel
        /// scrolls, and ^C copies the selection.
        #[test]
        fn the_log_is_selectable_and_scrolls_with_the_wheel() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            for index in 0..40 {
                app.log
                    .push(LogLevel::Info, "engine", format!("line {index}"));
            }
            press(&mut app, KeyCode::F(9), KeyModifiers::NONE);
            assert!(app.log_panel.is_some());
            let list = LogPanelView::list_area(app.frame, app.log_extent()).expect("a list");
            app.log_panel.as_mut().unwrap().prepare(&app.log, list);
            assert!(app.log_panel.as_ref().unwrap().following);
            let mouse = |kind: MouseEventKind, column: u16, row: u16| {
                Event::Mouse(MouseEvent {
                    kind,
                    column,
                    row,
                    modifiers: KeyModifiers::NONE,
                })
            };
            // The wheel scrolls the log up, letting go of the newest line.
            app.handle_terminal_event(mouse(MouseEventKind::ScrollUp, list.x + 2, list.y + 1))
                .expect("wheel");
            assert!(
                !app.log_panel.as_ref().unwrap().following,
                "scrolled up, the view no longer follows"
            );
            // A drag along a line selects it, and ^C copies the selection.
            app.handle_terminal_event(mouse(
                MouseEventKind::Down(MouseButton::Left),
                list.x,
                list.y,
            ))
            .expect("press");
            app.handle_terminal_event(mouse(
                MouseEventKind::Drag(MouseButton::Left),
                list.x + 12,
                list.y,
            ))
            .expect("drag");
            app.handle_terminal_event(mouse(
                MouseEventKind::Up(MouseButton::Left),
                list.x + 12,
                list.y,
            ))
            .expect("release");
            let selected = app
                .log_panel
                .as_ref()
                .unwrap()
                .editor
                .selected_text()
                .unwrap();
            assert_eq!(selected.chars().count(), 12, "{selected:?}");
            press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
            assert!(
                app.status.starts_with("copied 12 characters"),
                "{}",
                app.status
            );
        }

        /// ^C after a drag in the log puts the selection in the clipboard, even
        /// when a new log line arrives between the drag and the chord.
        #[test]
        fn copying_the_log_lands_in_the_clipboard_and_survives_new_lines() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            for index in 0..40 {
                app.log
                    .push(LogLevel::Info, "engine", format!("line {index}"));
            }
            press(&mut app, KeyCode::F(9), KeyModifiers::NONE);
            let list = LogPanelView::list_area(app.frame, app.log_extent()).expect("a list");
            let mouse = |kind: MouseEventKind, column: u16, row: u16| {
                Event::Mouse(MouseEvent {
                    kind,
                    column,
                    row,
                    modifiers: KeyModifiers::NONE,
                })
            };
            app.handle_terminal_event(mouse(
                MouseEventKind::Down(MouseButton::Left),
                list.x,
                list.y,
            ))
            .expect("press");
            app.handle_terminal_event(mouse(
                MouseEventKind::Drag(MouseButton::Left),
                list.x + 12,
                list.y,
            ))
            .expect("drag");
            app.handle_terminal_event(mouse(
                MouseEventKind::Up(MouseButton::Left),
                list.x + 12,
                list.y,
            ))
            .expect("release");

            // The very next frame redraws the log - and a line lands in it.
            app.log.push(LogLevel::Info, "engine", "line 40");
            app.log_panel.as_mut().unwrap().prepare(&app.log, list);

            press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
            assert!(
                app.status.starts_with("copied 12 characters"),
                "{}",
                app.status
            );
            let contents = app.clipboard.get_text().expect("the clipboard holds text");
            assert_eq!(
                contents,
                app.log_panel
                    .as_ref()
                    .unwrap()
                    .editor
                    .selected_text()
                    .unwrap(),
                "what ^C said it copied is what the clipboard holds"
            );
            assert_eq!(contents.chars().count(), 12, "{contents:?}");
        }

        /// The log sheet anchors against the frame short of the footer, so it
        /// sits over the score and the footer stays visible under it.
        #[test]
        fn the_log_sheet_leaves_the_footer_showing() {
            let directory = tempfile::tempdir().expect("tempdir");
            let app = app_over(directory.path());
            let frame = Rect::new(0, 0, 140, 40);
            let footer = Rect::new(0, 38, 140, 2);
            let (sheet, _) = crate::log::LogPanelView::geometry(
                Rect {
                    height: frame.height - footer.height,
                    ..frame
                },
                app.log_extent(),
            )
            .expect("the sheet fits");
            assert!(
                sheet.bottom() <= footer.y,
                "the sheet stops above the footer: {sheet:?}"
            );
            let (over_the_footer, _) = crate::log::LogPanelView::geometry(frame, app.log_extent())
                .expect("the sheet fits the whole frame too");
            assert!(
                over_the_footer.bottom() > footer.y,
                "anchoring on the whole frame is what hid the footer: {over_the_footer:?}"
            );
        }

        /// A press anywhere begins a new gesture, and that ends the log's own
        /// drag when its release never came, as it ends the score's. The wheel
        /// extends a live drag, so a stale one made every later scroll over the
        /// log move its selection under a pointer that had not moved.
        #[test]
        fn a_new_press_ends_a_log_drag_whose_release_never_came() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            for index in 0..40 {
                app.log
                    .push(LogLevel::Info, "engine", format!("line {index}"));
            }
            press(&mut app, KeyCode::F(9), KeyModifiers::NONE);
            // A score the linter refused, so the header shows the go chip.
            let scene = app.scenes.current().id;
            app.absorb_lint(crate::lint::LintResult {
                scene,
                revision: app.scenes.current().editor.revision(),
                input_channels: app.lint_input_channels(),
                diagnostics: vec![rustel_runtime::lint::Diagnostic {
                    level: LintLevel::Value,
                    message: "unknown sound \"top\"".into(),
                    from: 0,
                    to: 1,
                }],
            });
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40))
                .expect("test terminal");
            let mut draw = |app: &mut App| {
                terminal
                    .draw(|frame| app.paint_into(frame, Instant::now()).expect("frame paints"))
                    .expect("draw");
            };
            draw(&mut app);
            let list = LogPanelView::list_area(app.frame, app.log_extent()).expect("a list");

            // A drag along a log line whose release never arrives.
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                list.x,
                list.y,
            );
            mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                list.x + 5,
                list.y,
            );
            let selected = app.log_panel.as_ref().unwrap().editor.primary_selection();
            assert!(!selected.is_empty(), "a log drag is live");

            // A press and release on the go chip, which points at the log without
            // holding the pointer, then the wheel over the log.
            let chip = app.go_chip();
            assert!(!chip.is_empty(), "the header shows the go chip");
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                chip.x,
                chip.y,
            );
            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                chip.x,
                chip.y,
            );
            draw(&mut app);
            for _ in 0..3 {
                mouse(&mut app, MouseEventKind::ScrollUp, list.x + 2, list.y + 1);
            }

            assert_eq!(
                app.log_panel.as_ref().unwrap().editor.primary_selection(),
                selected,
                "the wheel scrolled the log without moving its selection"
            );
        }
    }
}
