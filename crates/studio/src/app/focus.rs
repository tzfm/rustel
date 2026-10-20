//! Which panel is on top and which one has the keyboard. Covers raising and
//! stacking open panels, finding the front panel that Esc closes, the Shift+F10
//! focus rotation and its landing flash, handing the keyboard back to the score
//! when a panel closes, and dismissing or closing dialogs.

use super::*;

/// ⇧F10's landing flash: long enough for the eye to catch the panel the
/// keyboard just moved to, short enough to read as an accent rather than a
/// blink.
pub(super) const FOCUS_ROTATION_FLASH_DURATION: Duration = Duration::from_millis(300);

/// Advances the ⇧F10 landing flash. Returns `(still lit, just went out)`;
/// the flash keeps which panel it lit, so a renderer or a test can ask which.
pub(super) fn advance_focus_rotation_flash(
    flash: &mut Option<(RotationStop, Instant)>,
    now: Instant,
) -> (bool, bool) {
    match *flash {
        Some((_, until)) if now < until => (true, false),
        Some(_) => {
            *flash = None;
            (false, true)
        }
        None => (false, false),
    }
}

/// Who has the keyboard. A panel being visible does not mean it owns it:
/// the reference stays readable while the score is typed into, and only the
/// panel that is focused gets a look at a key - and even then, only at the
/// keys it has a use for.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum Focus {
    #[default]
    Editor,
    Panel(PanelKind),
    /// A replay's timeline: arrows choose a block.
    Timeline,
}

/// A focusable surface that can receive the brief "here I am" accent.
/// The two visuals docks share a single `Focus::Panel(Viz)` variant, so
/// which one is meant has to travel beside it rather than inside it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RotationStop {
    Editor,
    Timeline,
    Settings,
    Viz(usize),
    Set,
    Mixer,
    Log,
    Memory,
    Reference,
}

impl App {
    fn panel_open(&self, kind: PanelKind) -> bool {
        match kind {
            PanelKind::Reference => self.reference_panel.is_some(),
            PanelKind::Log => self.log_panel.is_some(),
            PanelKind::Jobs => self.jobs_panel.is_some(),
            PanelKind::Export => self.export_sheet.is_some(),
            PanelKind::Theme => self.theme_picker.is_some(),
            PanelKind::ThemeEditor => self.theme_editor.is_some(),
            PanelKind::Settings => self.settings_sheet.is_some(),
            PanelKind::Devices => self.panel.is_some(),
            PanelKind::Set => self.set_panel.is_some() || self.set_prompt.is_some(),
            PanelKind::Viz => self.viz_docks.iter().any(Option::is_some),
            PanelKind::Mixer => self.mixer_panel.is_some(),
            PanelKind::Memory => self.memory_dock.is_some(),
        }
    }

    /// A surface raised: opened, or given the keyboard. The latest raised
    /// paints on top of the rest and answers the pointer and Esc first.
    pub(super) fn raise(&mut self, kind: PanelKind) {
        self.raise_counter += 1;
        self.raised.insert(kind, self.raise_counter);
    }

    fn raised_at(&self, kind: PanelKind) -> u64 {
        self.raised.get(&kind).copied().unwrap_or(0)
    }

    /// The sheets that stack with the reference column.
    const SHEETS: [PanelKind; 6] = [
        PanelKind::Theme,
        PanelKind::Devices,
        PanelKind::Settings,
        PanelKind::Export,
        PanelKind::Log,
        PanelKind::Jobs,
    ];

    /// Whether the reference column was raised after every open sheet, and
    /// so lies over them rather than under them.
    pub(super) fn reference_on_top(&self) -> bool {
        self.reference_panel.is_some()
            && Self::SHEETS
                .iter()
                .filter(|kind| self.panel_open(**kind))
                .all(|kind| self.raised_at(*kind) < self.raised_at(PanelKind::Reference))
    }

    /// Zen draws the breakdown over the score. Otherwise it is docked
    /// furniture and should not be dismissed by Esc from the editor.
    pub(super) fn memory_is_docked(&self) -> bool {
        !self.ui_settings.zen && self.memory_dock.is_some()
    }

    /// The panel an Esc from the score puts away: the one on top - the
    /// prompts and the docks' sheets first, then the reference and the
    /// sheets by when they were raised, the latest first. The set panel is
    /// not among them: it stays until ^B, and only a set prompt over it
    /// goes.
    pub(super) fn front_panel(&self) -> Option<PanelKind> {
        if self.set_prompt.is_some() {
            return Some(PanelKind::Set);
        }
        if self.viz_docks.iter().flatten().any(VizPanel::asking) {
            return Some(PanelKind::Viz);
        }
        let mut open: Vec<PanelKind> = Self::SHEETS
            .into_iter()
            .chain([
                PanelKind::Reference,
                PanelKind::ThemeEditor,
                PanelKind::Memory,
            ])
            .filter(|kind| self.panel_open(*kind))
            // A DOCKED log is furniture, not a sheet: it has its own room
            // on the screen and covers nothing, so Esc from the score has
            // nothing to put away. Esc took it, and a log kept on screen
            // on purpose went every time the score was tidied.
            .filter(|kind| !(*kind == PanelKind::Log && self.log_is_docked()))
            .filter(|kind| !(*kind == PanelKind::Memory && self.memory_is_docked()))
            .collect();
        open.sort_by_key(|kind| std::cmp::Reverse(self.raised_at(*kind)));
        open.first().copied()
    }

    /// ⇧F10 walks the keyboard round the open surfaces a performer cares
    /// to jump between, in the same screen order every time - the score,
    /// visuals 1, visuals 2, the set panel, the mixer, the docked log, and
    /// the reference column - and back to the score. It only ever moves the keyboard: it
    /// must never open or close anything, so a player who has arranged
    /// their panels finds them exactly as they left them.
    pub(super) fn rotate_panel_focus(&mut self) {
        // A modal sheet is the only visible keyboard target. In particular,
        // Settings can cover the mixer and editor completely; walking focus
        // behind it makes the highlighted control and the responding control
        // disagree.
        if self.settings_sheet.is_some() {
            self.focus_panel(PanelKind::Settings);
            self.status = "keyboard on settings".into();
            self.arm_focus_rotation_flash(RotationStop::Settings);
            self.dirty_frame = true;
            return;
        }
        let mut stops = vec![RotationStop::Editor];
        if self.timeline_visible() {
            stops.push(RotationStop::Timeline);
        }
        for index in 0..DOCKS {
            if self.viz_docks[index].is_some()
                && (self.viz_unlaid[index] || !self.regions.viz[index].is_empty())
            {
                stops.push(RotationStop::Viz(index));
            }
        }
        if self.panel_open(PanelKind::Set) && !self.regions.sidebar_hidden {
            stops.push(RotationStop::Set);
        }
        // Open is not the same as on screen: a terminal too short for the
        // desk's band opens the mixer and never draws it, and the walk
        // would hand the keyboard to something invisible.
        if self.mixer_panel.is_some() && !self.regions.mixer.is_empty() {
            stops.push(RotationStop::Mixer);
        }
        if self.log_is_docked() && !self.regions.log.is_empty() {
            stops.push(RotationStop::Log);
        }
        if self.memory_is_docked() && !self.regions.memory.is_empty() {
            stops.push(RotationStop::Memory);
        }
        if self.reference_panel.is_some() {
            stops.push(RotationStop::Reference);
        }
        if stops.len() == 1 {
            self.status = "nothing open to walk the keyboard round - just the score".into();
            self.dirty_frame = true;
            return;
        }
        let current = match self.focus {
            Focus::Editor => Some(RotationStop::Editor),
            Focus::Timeline => Some(RotationStop::Timeline),
            Focus::Panel(PanelKind::Viz) => Some(RotationStop::Viz(self.viz_focus)),
            Focus::Panel(PanelKind::Set) => Some(RotationStop::Set),
            Focus::Panel(PanelKind::Mixer) => Some(RotationStop::Mixer),
            Focus::Panel(PanelKind::Log) => Some(RotationStop::Log),
            Focus::Panel(PanelKind::Memory) => Some(RotationStop::Memory),
            Focus::Panel(PanelKind::Reference) => Some(RotationStop::Reference),
            // A panel outside the rotation - a sheet - has
            // no place in this order; walking from it starts the rotation
            // over at its first stop rather than guessing where it fits.
            _ => None,
        };
        let position =
            current.and_then(|stop| stops.iter().position(|candidate| *candidate == stop));
        let next = stops[position.map_or(0, |position| (position + 1) % stops.len())];
        match next {
            RotationStop::Editor => {
                self.focus = Focus::Editor;
                self.status = "keyboard back on the score".into();
                self.arm_focus_rotation_flash(RotationStop::Editor);
                self.dirty_frame = true;
            }
            RotationStop::Timeline => {
                self.focus_timeline();
                self.arm_focus_rotation_flash(RotationStop::Timeline);
            }
            RotationStop::Settings => {
                self.focus_panel(PanelKind::Settings);
                self.status = "keyboard on settings".into();
                self.arm_focus_rotation_flash(RotationStop::Settings);
            }
            RotationStop::Viz(index) => {
                self.viz_focus = index;
                // `focus_panel` only notices a change by comparing
                // `Focus` values, and both docks share one
                // `Focus::Panel(Viz)` - so hopping from one dock to the
                // other while `Viz` already has the keyboard would
                // otherwise look like no change at all.
                self.focus = Focus::Editor;
                self.focus_panel(PanelKind::Viz);
                self.status = format!("keyboard on visuals {}", index + 1);
                self.arm_focus_rotation_flash(RotationStop::Viz(index));
            }
            RotationStop::Set => {
                self.focus = Focus::Editor;
                self.focus_panel(PanelKind::Set);
                self.status = "keyboard on the set panel".into();
                self.arm_focus_rotation_flash(RotationStop::Set);
            }
            RotationStop::Mixer => {
                self.focus = Focus::Editor;
                self.focus_panel(PanelKind::Mixer);
                self.status = "keyboard on the mixer".into();
                self.arm_focus_rotation_flash(RotationStop::Mixer);
            }
            RotationStop::Log => {
                self.focus = Focus::Editor;
                self.focus_panel(PanelKind::Log);
                self.status = "keyboard on the log".into();
                self.arm_focus_rotation_flash(RotationStop::Log);
            }
            RotationStop::Memory => {
                self.focus = Focus::Editor;
                self.focus_panel(PanelKind::Memory);
                self.status = "keyboard on the memory breakdown - e top/bottom · -/+ height".into();
                self.arm_focus_rotation_flash(RotationStop::Memory);
            }
            RotationStop::Reference => {
                self.focus = Focus::Editor;
                self.focus_panel(PanelKind::Reference);
                self.status = "keyboard on the reference".into();
                self.arm_focus_rotation_flash(RotationStop::Reference);
            }
        }
    }

    /// The room of the panel the rotation just landed on, while the
    /// landing is still lit - and `None` the moment it is not.
    pub(super) fn focus_rotation_flash_room(&self) -> Option<Rect> {
        let (stop, until) = self.focus_rotation_flash?;
        if Instant::now() >= until {
            return None;
        }
        let room = match stop {
            // The score uses a full current-line flash inside the editor.
            // Outlining the pane mostly left one remote rule at the bottom
            // of a large terminal and did not reveal the caret at all.
            RotationStop::Editor => Rect::default(),
            RotationStop::Settings => {
                super::super::settings::SettingsSheetView::geometry(self.settings_sheet_frame())
                    .map(|(sheet, _)| sheet)
                    .unwrap_or_default()
            }
            RotationStop::Timeline => self
                .regions
                .panes
                .get(self.focused)
                .map(|pane| pane.timeline)
                .unwrap_or_default(),
            RotationStop::Viz(index) => self.regions.viz[index],
            RotationStop::Set => self
                .set_prompt
                .as_ref()
                .and_then(|(_, picker)| picker.geometry(self.frame))
                .map(|(sheet, _)| sheet)
                .unwrap_or(self.regions.sidebar),
            RotationStop::Mixer => self.regions.mixer,
            RotationStop::Log => self.regions.log,
            RotationStop::Memory => self.regions.memory,
            RotationStop::Reference => self.regions.reference,
        };
        (!room.is_empty()).then_some(room)
    }

    /// Light a surface's brief location accent. Called from keyboard
    /// navigation alone - `focus_panel` itself never arms this, so a chord or a
    /// click that focuses a panel directly does not flash it too.
    pub(super) fn arm_focus_rotation_flash(&mut self, stop: RotationStop) {
        self.focus_rotation_flash = Some((stop, Instant::now() + FOCUS_ROTATION_FLASH_DURATION));
        self.dirty_frame = true;
    }

    /// Closing a panel hands the keyboard back to the score.
    pub(super) fn settle_focus(&mut self) {
        self.settle_mixer_fader_selection();
        if let Focus::Panel(kind) = self.focus
            && !self.panel_open(kind)
        {
            self.focus = Focus::Editor;
        }
        if self.focus == Focus::Timeline && self.current_replay().is_none() {
            self.focus = Focus::Editor;
        }
        // While the editor's sheet is the visible front it holds the
        // keyboard: everything under it is covered, so keys that "fell
        // through to the score" would be typing into a document nobody can
        // see.
        if self.theme_editor_sheet_visible() && self.focus != Focus::Panel(PanelKind::ThemeEditor) {
            self.focus = Focus::Panel(PanelKind::ThemeEditor);
        }
        #[cfg(feature = "hydra")]
        self.sync_settings_webcam_preview();
    }

    pub(super) fn focus_panel(&mut self, kind: PanelKind) {
        self.raise(kind);
        if self.focus != Focus::Panel(kind) {
            // Which panel had the keyboard is not idle detail: a terminal
            // reports a dropped file as a paste with no coordinates, so
            // what the keyboard was on at that moment is the only record
            // of where a drop was aimed.
            self.log
                .push(LogLevel::Debug, "focus", format!("{} panel", kind.label()));
            // A slider armed in the score must not catch arrows meant for
            // the panel.
            self.armed_slider = None;
            self.focus = Focus::Panel(kind);
            super::vim_keys::focus_changed(self);
            self.dirty_frame = true;
            #[cfg(feature = "hydra")]
            self.sync_settings_webcam_preview();
        }
    }

    /// Put away every dialog but `keep`. A menu row opens one thing, and
    /// the sheet it opens is the one on top: the theme, settings, devices
    /// and export sheets and the set prompts take one another's place.
    /// The set panel, the reference column and the log are not dialogs
    /// and stay, and the keyboard help is an overlay on whatever is open,
    /// so it is neither put away nor puts anything away.
    pub(super) fn dismiss_dialogs(&mut self, keep: Option<PanelKind>) {
        self.dismiss_dialogs_except(keep.as_slice());
    }

    /// Every dialog but the ones named: a prompt that opens over a panel
    /// keeps that panel too, so Esc has somewhere to come back to.
    pub(super) fn dismiss_dialogs_except(&mut self, keep: &[PanelKind]) {
        self.slider_precision = None;
        self.smart_action = None;
        self.replay_edit = None;
        self.editor_mut().find_cancel();
        if !keep.contains(&PanelKind::Set) && self.set_prompt.is_some() {
            self.close_set_prompt();
        }
        // The visuals docks' add list and text sheet are popups of their
        // own.
        if !keep.contains(&PanelKind::Viz) {
            for panel in self.viz_docks.iter_mut().flatten() {
                panel.adding = None;
                panel.prompt = None;
            }
        }
        // A sticky log is docked furniture, like the set panel and the
        // mixer: dismissing the sheets other dialogs displace must not
        // take it with them, or ⇧F9 would be undone by opening a menu.
        if !keep.contains(&PanelKind::Log)
            && self.log_panel.as_ref().is_some_and(|panel| !panel.sticky)
        {
            self.close_panel(PanelKind::Log);
        }
        for kind in [
            PanelKind::Export,
            PanelKind::Theme,
            PanelKind::ThemeEditor,
            PanelKind::Settings,
            PanelKind::Devices,
            PanelKind::Jobs,
        ] {
            if !keep.contains(&kind) && self.panel_open(kind) {
                self.close_panel(kind);
            }
        }
    }

    /// Put one panel away outright, the way its own Esc would from its top
    /// level.
    pub(super) fn close_panel(&mut self, kind: PanelKind) {
        match kind {
            PanelKind::Reference => {
                // Closing the column is the end of what it was previewing,
                // sound and picture alike.
                self.stop_preview();
                self.set_reference_panel(None);
                self.reference_anchor = None;
                self.status = "reference closed".into();
                self.invalidate_maps();
            }
            PanelKind::Log => self.log_panel = None,
            PanelKind::Memory => self.close_memory_dock(),
            PanelKind::Jobs => self.jobs_panel = None,
            PanelKind::Export => {
                self.export_sheet = None;
                self.status = "export closed".into();
            }
            PanelKind::Theme => {
                // Closing the picker keeps the theme, however it is closed -
                // see the Esc arm in `handle_theme_key`.
                if self.theme_picker.is_some() {
                    self.keep_theme();
                }
            }
            PanelKind::ThemeEditor => {
                if let Some(editor) = self.theme_editor.take() {
                    self.close_theme_editor(editor, false);
                    self.status = "theme editor closed - theme put back".into();
                }
            }
            PanelKind::Settings => {
                self.close_settings_sheet();
                self.mapping_learn = None;
            }
            PanelKind::Devices => self.panel = None,
            PanelKind::Set => {
                // `front_panel` names the set only while a prompt is up, and
                // the prompt is the thing in front: putting it away is all
                // that was asked. The panel under it is not a dialog.
                if self.set_prompt.is_some() {
                    self.close_set_prompt();
                } else {
                    self.set_panel = None;
                }
            }
            PanelKind::Viz => {
                let index = self.viz_focus;
                match self.viz_docks[index].as_mut() {
                    Some(panel) if panel.asking() => {
                        panel.adding = None;
                        panel.prompt = None;
                    }
                    _ => self.close_viz_dock(index),
                }
            }
            PanelKind::Mixer => {
                if self.mixer_panel.take().is_some() {
                    self.mixer_unlaid = false;
                    self.mixer_selection = None;
                    self.prefs.mixer_panel = Some(false);
                    self.save_prefs_soon();
                    self.invalidate_maps();
                    self.status = "mixer hidden".into();
                }
            }
        }
        self.settle_focus();
        #[cfg(feature = "hydra")]
        self.sync_settings_webcam_preview();
        self.dirty_frame = true;
    }
}

#[cfg(test)]
mod tests {
    mod focus {
        //! Tests for `app/focus.rs`.

        use crate::app::focus::*;
        use crate::app::tests::support::*;

        /// A menu row opens one sheet and the sheet takes the place of any
        /// other; the set panel is not a sheet and stays through all of it.
        #[test]
        fn opening_a_dialog_puts_the_others_away() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
            assert!(app.set_panel.is_some());
            press(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);
            assert!(app.settings_sheet.is_some(), "^O opens the settings");
            assert!(app.set_panel.is_some(), "the set panel stays");
            press(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
            assert!(app.theme_picker.is_some());
            assert!(app.settings_sheet.is_none());
            app.dispatch_menu_action(crate::menu::MenuAction::OpenSet)
                .expect("menu");
            assert!(app.set_prompt.is_some());
            assert!(app.theme_picker.is_none());
            app.dispatch_menu_action(crate::menu::MenuAction::Devices)
                .expect("menu");
            assert!(app.panel.is_some());
            assert!(app.set_prompt.is_none(), "the prompt is a sheet and goes");
            assert!(app.set_panel.is_some(), "the panel is not");
            press(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);
            assert!(app.settings_sheet.is_some());
            assert!(app.panel.is_none());
        }

        /// Esc from the score puts the front panel away in one press.
        #[test]
        fn escape_from_the_score_closes_the_panel_in_one_press() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.toggle_reference_browse();
            app.focus = Focus::Editor; // stands in for a click on the score
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.reference_panel.is_none());
            assert_eq!(app.focus, Focus::Editor);
        }

        /// While the rotation flash is lit, the rotation hands the room of the
        /// panel it landed on to the chrome. It hands nothing after the flash.
        #[test]
        fn the_rotations_landing_hands_its_room_to_the_chrome() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.open_viz_dock(0, false);
            app.regions.viz[0] = Rect::new(0, 0, 20, 8);
            assert_eq!(app.focus_rotation_flash_room(), None, "nothing lit yet");

            app.rotate_panel_focus();
            assert_eq!(app.focus, Focus::Panel(PanelKind::Viz));
            assert_eq!(
                app.focus_rotation_flash_room(),
                Some(Rect::new(0, 0, 20, 8)),
                "the dock the rotation landed on"
            );

            // Spent, and the room goes with it.
            app.focus_rotation_flash = Some((RotationStop::Viz(0), Instant::now()));
            assert_eq!(app.focus_rotation_flash_room(), None, "the landing is over");

            // A panel with no room on screen has nothing to outline.
            app.regions.viz[0] = Rect::new(0, 0, 0, 0);
            app.focus_rotation_flash = Some((
                RotationStop::Viz(0),
                Instant::now() + Duration::from_secs(9),
            ));
            assert_eq!(app.focus_rotation_flash_room(), None, "no room, no outline");
        }

        /// Esc from the score closes the front-most sheet. A docked log is not
        /// a sheet, so Esc leaves it open.
        #[test]
        fn esc_from_the_score_leaves_a_docked_log_standing() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.toggle_log_panel();
            app.focus = Focus::Editor;
            assert_eq!(
                app.front_panel(),
                Some(PanelKind::Log),
                "a floating log is a sheet, and Esc is for sheets"
            );
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.log_panel.is_none(), "the sheet goes");

            app.toggle_log_sticky();
            app.focus = Focus::Editor;
            assert!(app.log_is_docked());
            assert_eq!(
                app.front_panel(),
                None,
                "furniture is not a sheet, whatever else is on screen"
            );
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.log_panel.is_some(), "the docked log stays put");
        }

        /// The landing accent is for every stop on the walk, not only the
        /// visuals docks: the desk and the docked log get outlined too, each
        /// in the room the layout carved for it. A walk that lit some of its
        /// stops and not others would be worse than one that lit none.
        #[test]
        fn the_landing_accent_covers_the_desk_and_the_docked_log_too() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.regions.mixer = Rect::new(0, 30, 120, 8);
            app.regions.log = Rect::new(0, 22, 120, 8);
            app.regions.sidebar = Rect::new(0, 0, 24, 30);

            for (stop, room) in [
                (RotationStop::Mixer, app.regions.mixer),
                (RotationStop::Log, app.regions.log),
                (RotationStop::Set, app.regions.sidebar),
            ] {
                app.arm_focus_rotation_flash(stop);
                assert_eq!(
                    app.focus_rotation_flash_room(),
                    Some(room),
                    "{stop:?} lights the room the layout gave it"
                );
            }

            // A panel the layout could not find room for is drawn nowhere, so
            // there is nothing to outline - and nothing to walk onto either.
            app.regions.mixer = Rect::default();
            app.arm_focus_rotation_flash(RotationStop::Mixer);
            assert_eq!(app.focus_rotation_flash_room(), None, "no room, no accent");
            app.open_mixer_panel(false);
            app.focus = Focus::Editor;
            app.rotate_panel_focus();
            assert_ne!(
                app.focus,
                Focus::Panel(PanelKind::Mixer),
                "the walk does not stop at a desk with no room on screen"
            );
        }

        /// The settings sheet keeps the keyboard until Esc closes it. A panel
        /// chord pressed under it does not move the focus.
        #[test]
        fn the_settings_sheet_keeps_the_keyboard_until_esc() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            press(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);
            press(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
            let focused = app.focused;
            app.toggle_settings_sheet();
            assert_eq!(app.focus, Focus::Panel(PanelKind::Settings));

            // Every chord that moves the keyboard to another surface.
            for (code, modifiers) in [
                (KeyCode::Char('b'), KeyModifiers::CONTROL),
                (KeyCode::F(4), KeyModifiers::NONE),
                (KeyCode::F(1), KeyModifiers::SHIFT),
                (KeyCode::F(2), KeyModifiers::SHIFT),
                (KeyCode::F(10), KeyModifiers::SHIFT),
            ] {
                press(&mut app, code, modifiers);
                assert_eq!(
                    app.focus,
                    Focus::Panel(PanelKind::Settings),
                    "{code:?} took the keyboard off the sheet"
                );
                assert!(app.settings_sheet.is_some(), "and the sheet is still up");
                assert_eq!(app.focused, focused, "{code:?} hopped to a covered pane");
            }
            assert!(matches!(
                app.focus_rotation_flash,
                Some((RotationStop::Settings, _))
            ));
            assert_eq!(app.status, "keyboard on settings");

            // Esc is the way out, and it hands the keyboard back.
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.settings_sheet.is_none());
            assert_eq!(app.focus, Focus::Editor);
        }

        /// ⇧F10 with one visuals dock open: the score, the dock, and back.
        #[test]
        fn the_rotation_visits_one_open_dock_and_returns_to_the_score() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.open_viz_dock(0, false);
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(180, 40))
                .expect("test terminal");
            terminal
                .draw(|frame| app.paint_into(frame, Instant::now()).expect("frame paints"))
                .expect("draw");
            assert_eq!(app.focus, Focus::Editor);

            app.rotate_panel_focus();
            assert_eq!(app.focus, Focus::Panel(PanelKind::Viz));
            assert_eq!(app.viz_focus, 0);

            app.rotate_panel_focus();
            assert_eq!(app.focus, Focus::Editor);

            app.rotate_panel_focus();
            assert_eq!(app.focus, Focus::Panel(PanelKind::Viz));
            assert_eq!(app.viz_focus, 0, "the rotation is a loop, not a one-shot");
        }

        /// The screen order with everything open: the score, visuals 1,
        /// visuals 2, the set panel, the reference, and back. Sheets are not
        /// rotation stops.
        #[test]
        fn the_rotation_visits_two_docks_and_the_set_panel_in_screen_order() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.open_viz_dock(0, false);
            app.open_viz_dock(1, false);
            app.open_set_panel(false);
            app.toggle_reference_browse();
            app.focus = Focus::Editor;
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(300, 40))
                .expect("test terminal");
            terminal
                .draw(|frame| app.paint_into(frame, Instant::now()).expect("frame paints"))
                .expect("draw");
            assert_eq!(app.focus, Focus::Editor);

            app.rotate_panel_focus();
            assert_eq!(app.focus, Focus::Panel(PanelKind::Viz));
            assert_eq!(app.viz_focus, 0, "visuals 1 before visuals 2");

            app.rotate_panel_focus();
            assert_eq!(app.focus, Focus::Panel(PanelKind::Viz));
            assert_eq!(app.viz_focus, 1);

            app.rotate_panel_focus();
            assert_eq!(app.focus, Focus::Panel(PanelKind::Set));

            app.rotate_panel_focus();
            assert_eq!(app.focus, Focus::Panel(PanelKind::Reference));

            app.rotate_panel_focus();
            assert_eq!(app.focus, Focus::Editor, "the loop closes on the score");
        }

        #[test]
        fn the_rotation_stays_on_an_open_settings_sheet() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.settings_sheet = Some(SettingsSheet::default());
            app.focus = Focus::Editor;

            app.rotate_panel_focus();

            assert_eq!(app.focus, Focus::Panel(PanelKind::Settings));
            assert_eq!(app.status, "keyboard on settings");
            assert!(matches!(
                app.focus_rotation_flash,
                Some((RotationStop::Settings, _))
            ));
            assert_eq!(
                app.focus_rotation_flash_room(),
                crate::settings::SettingsSheetView::geometry(app.settings_sheet_frame())
                    .map(|(sheet, _)| sheet),
                "Settings gets the same landing outline as the other stops"
            );
        }

        /// The rotation only ever moves the keyboard: nothing it walks past is
        /// opened or closed along the way.
        #[test]
        fn the_rotation_never_opens_or_closes_a_panel() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.open_viz_dock(0, false);
            app.open_viz_dock(1, false);
            app.open_set_panel(false);
            let open_before = (
                app.viz_docks[0].is_some(),
                app.viz_docks[1].is_some(),
                app.set_panel.is_some(),
            );
            for _ in 0..8 {
                app.rotate_panel_focus();
            }
            let open_after = (
                app.viz_docks[0].is_some(),
                app.viz_docks[1].is_some(),
                app.set_panel.is_some(),
            );
            assert_eq!(
                open_before, open_after,
                "walking the rotation must not open or close anything"
            );
        }

        /// With nothing open there is nowhere to walk to, and the rotation
        /// says so rather than doing nothing in silence.
        #[test]
        fn the_rotation_says_so_when_nothing_is_open_to_walk_onto() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            assert_eq!(app.focus, Focus::Editor);

            app.rotate_panel_focus();
            assert_eq!(
                app.focus,
                Focus::Editor,
                "the score is the only stop there is"
            );
            assert!(app.status.contains("nothing open"), "{}", app.status);
        }

        /// The landing accent is the rotation's alone: `focus_panel` on its
        /// own - the way a chord or a click reaches a panel - must never light
        /// it, or every ordinary focus change would flash too.
        #[test]
        fn the_rotations_flash_lights_only_from_the_chord_not_from_focus_panel() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.open_viz_dock(0, false);
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(180, 40))
                .expect("test terminal");
            terminal
                .draw(|frame| app.paint_into(frame, Instant::now()).expect("frame paints"))
                .expect("draw");

            app.focus_panel(PanelKind::Viz);
            assert!(
                app.focus_rotation_flash.is_none(),
                "focus_panel alone must not arm the flash"
            );

            app.focus = Focus::Editor;
            app.rotate_panel_focus();
            assert_eq!(app.focus, Focus::Panel(PanelKind::Viz));
            assert!(
                matches!(app.focus_rotation_flash, Some((RotationStop::Viz(0), _))),
                "the rotation must light the panel it landed on"
            );
        }

        /// The rotation's flash is lit up to its deadline, gone from it on, and
        /// asks for one more repaint the instant it goes out so it cannot stick.
        #[test]
        fn the_rotations_flash_decays_and_asks_for_one_more_repaint() {
            let started = Instant::now();
            let mut flash = Some((RotationStop::Set, started + FOCUS_ROTATION_FLASH_DURATION));
            assert_eq!(
                advance_focus_rotation_flash(&mut flash, started),
                (true, false)
            );
            assert_eq!(
                advance_focus_rotation_flash(
                    &mut flash,
                    started + FOCUS_ROTATION_FLASH_DURATION - Duration::from_millis(1)
                ),
                (true, false)
            );
            assert_eq!(
                advance_focus_rotation_flash(&mut flash, started + FOCUS_ROTATION_FLASH_DURATION),
                (false, true),
                "the deadline itself is when it goes out"
            );
            assert!(flash.is_none());
            assert_eq!(
                advance_focus_rotation_flash(&mut flash, started + FOCUS_ROTATION_FLASH_DURATION),
                (false, false),
                "once out it stays out"
            );
        }

        /// The reference and the sheets stack by when they were raised: opened
        /// after the settings, the reference is on top - Esc puts it away
        /// first and the pointer over their overlap is the reference's; a
        /// press on the settings raises the sheet back over it.
        #[test]
        fn the_surface_raised_last_is_on_top() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 140, 40);
            press(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);
            assert!(app.settings_sheet.is_some());
            assert_eq!(app.front_panel(), Some(PanelKind::Settings));
            app.toggle_reference_browse();
            assert!(app.reference_panel.is_some() && app.settings_sheet.is_some());
            assert!(app.reference_on_top(), "raised after the settings");
            assert_eq!(app.front_panel(), Some(PanelKind::Reference));
            app.regions = view::regions(app.frame, false, 1, true, true, None, None);
            let (sheet, _) = crate::app::settings::SettingsSheetView::geometry(app.frame)
                .expect("room for the sheet");
            let overlap = (
                app.regions.reference.x + 2,
                sheet.y.max(app.regions.reference.y) + 1,
            );
            assert!(
                within(app.regions.reference, overlap.0, overlap.1)
                    && within(sheet, overlap.0, overlap.1),
                "the two share cells"
            );
            assert_eq!(
                app.pointer_shape_at(overlap.0, overlap.1),
                app.reference_pointer_shape(overlap.0, overlap.1).unwrap(),
                "the pointer answers for the reference"
            );
            // A press on the sheet where they do not overlap raises the sheet.
            app.focus_panel(PanelKind::Settings);
            assert!(!app.reference_on_top());
            assert_eq!(app.front_panel(), Some(PanelKind::Settings));
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.settings_sheet.is_none(), "Esc took the sheet on top");
            assert!(app.reference_panel.is_some());
        }
    }

    mod theme_stacking {
        //! Floating sheets must not rearrange the docked surfaces beneath them.

        use crate::app::tests::support::*;
        use crate::app::*;
        use crate::viz_panel::Edge;
        use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

        fn paint(app: &mut App, terminal: &mut Terminal<TestBackend>) -> Buffer {
            terminal
                .draw(|frame| app.paint_into(frame, Instant::now()).unwrap())
                .unwrap();
            terminal.backend().buffer().clone()
        }

        fn theme_shortcut(app: &mut App) {
            app.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Char('t'),
                KeyModifiers::CONTROL,
            )))
            .unwrap();
        }

        fn set_bands(app: &mut App, edges: [Edge; 2]) {
            app.theme = Theme::built_in_default();
            app.ui_settings.animation = false;
            app.ui_settings.zen = false;
            app.ui_settings.viz_edges = edges;
            app.ui_settings.apply();
            app.mixer_panel = None;
            app.log_panel = None;
            for (index, edge) in edges.into_iter().enumerate() {
                app.prefs.visuals[index].edge = edge;
                app.prefs.visuals[index].extent = Some(9);
                app.prefs.visuals[index].widgets.clear();
                app.viz_docks[index] = Some(VizPanel {
                    edge,
                    ..VizPanel::default()
                });
            }
            app.toggle_reference_browse();
        }

        #[test]
        fn theme_picker_keeps_both_visual_bands_behind_the_reference() {
            for edges in [
                [Edge::Top, Edge::Top],
                [Edge::Bottom, Edge::Bottom],
                [Edge::Top, Edge::Bottom],
            ] {
                let directory = tempfile::tempdir().unwrap();
                let mut app = app_over(directory.path());
                set_bands(&mut app, edges);
                let mut terminal = Terminal::new(TestBackend::new(140, 48)).unwrap();
                let before = paint(&mut app, &mut terminal);
                let layout = app.regions;
                // Losing focus legitimately changes the shortcut footer. Compare
                // the actual list, using the same geometry as rendering/hit tests,
                // so the regression concerns covered content rather than hints.
                let content = app
                    .reference_panel
                    .as_ref()
                    .unwrap()
                    .geometry(crate::reference::inner_area(layout.reference))
                    .list;
                assert!(app.reference_on_top());

                theme_shortcut(&mut app);
                assert!(app.theme_picker.is_some());
                assert!(!app.reference_on_top(), "the picker is the raised sheet");
                let opened = paint(&mut app, &mut terminal);
                assert_eq!(
                    app.regions, layout,
                    "opening a sheet changes no dock geometry"
                );
                let (picker, _) = app
                    .theme_picker
                    .as_ref()
                    .unwrap()
                    .geometry(app.frame)
                    .unwrap();

                for band in layout.viz {
                    let shared = band.intersection(content);
                    assert!(
                        !shared.is_empty(),
                        "a full-width band passes behind reference"
                    );
                    let mut visible_cells = 0;
                    let mut visible_text = 0;
                    for y in shared.y..shared.bottom() {
                        for x in shared.x..shared.right() {
                            if picker.contains((x, y).into()) {
                                continue;
                            }
                            let original = before.cell((x, y)).unwrap().symbol();
                            visible_cells += 1;
                            visible_text += usize::from(!original.trim().is_empty());
                            assert_eq!(
                                opened.cell((x, y)).unwrap().symbol(),
                                original,
                                "{edges:?}: opening themes repainted reference cell ({x}, {y}) as a visual band"
                            );
                        }
                    }
                    assert!(visible_cells > 0 && visible_text > 0);
                }

                theme_shortcut(&mut app);
                assert!(app.theme_picker.is_none());
                let closed = paint(&mut app, &mut terminal);
                assert_eq!(
                    app.regions, layout,
                    "closing the picker preserves geometry too"
                );
                for band in layout.viz {
                    let shared = band.intersection(content);
                    for y in shared.y..shared.bottom() {
                        for x in shared.x..shared.right() {
                            assert_eq!(
                                closed.cell((x, y)).unwrap().symbol(),
                                before.cell((x, y)).unwrap().symbol(),
                                "{edges:?}: closing themes changed reference cell ({x}, {y})"
                            );
                        }
                    }
                }
            }
        }

        #[test]
        fn reference_above_a_visual_band_keeps_mouse_ownership_under_the_theme_picker() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            set_bands(&mut app, [Edge::Top, Edge::Top]);
            let mut terminal = Terminal::new(TestBackend::new(140, 48)).unwrap();
            paint(&mut app, &mut terminal);
            theme_shortcut(&mut app);
            paint(&mut app, &mut terminal);
            let (picker, _) = app
                .theme_picker
                .as_ref()
                .unwrap()
                .geometry(app.frame)
                .unwrap();
            let shared = app.regions.reference.intersection(app.regions.viz[0]);
            let mut target = None;
            for y in shared.y..shared.bottom() {
                for x in shared.x..shared.right() {
                    if !picker.contains((x, y).into())
                        && app.reference_pointer_shape(x, y) == Some(SHAPE_POINTER)
                        && app.viz_widget_at(0, x, y).is_none()
                    {
                        target = Some((x, y));
                        break;
                    }
                }
            }
            let (x, y) = target.expect("an exposed reference row over the first visual band");
            assert_eq!(app.pointer_shape_at(x, y), SHAPE_POINTER);
            app.handle_terminal_event(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x,
                row: y,
                modifiers: KeyModifiers::NONE,
            }))
            .unwrap();
            assert_eq!(app.focus, Focus::Panel(PanelKind::Reference));
            assert!(
                app.theme_picker.is_some(),
                "clicking reference need not close the picker"
            );
        }

        #[cfg(feature = "hydra")]
        #[test]
        fn theme_picker_does_not_mask_late_shelf_pixels_with_lower_visual_bands() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            set_bands(&mut app, [Edge::Top, Edge::Bottom]);
            let mut terminal = Terminal::new(TestBackend::new(140, 48)).unwrap();
            paint(&mut app, &mut terminal);
            theme_shortcut(&mut app);
            paint(&mut app, &mut terminal);
            let (picker, _) = app
                .theme_picker
                .as_ref()
                .unwrap()
                .geometry(app.frame)
                .unwrap();
            let covered = app.shelf_exclusions(false);
            assert!(
                covered.contains(&picker),
                "the actual raised picker still masks its cells"
            );
            for band in app.regions.viz {
                assert!(
                    !covered.contains(&band),
                    "a lower dock cannot mask a reference picture"
                );
            }
            let area = app.regions.reference;
            let image = crate::graphics::PixelImage {
                area,
                width: u32::from(area.width),
                height: u32::from(area.height),
                rgba: [40, 80, 120, 255].repeat(usize::from(area.width) * usize::from(area.height)),
                depth: 0,
                cells: Some((area.width, area.height)),
            };
            let masked = stage_pixels::mask_shelf(image, &covered)
                .expect("reference remains partly visible");
            for (index, pixel) in masked.rgba.as_chunks::<4>().0.iter().enumerate() {
                let x = area.x + (index % usize::from(area.width)) as u16;
                let y = area.y + (index / usize::from(area.width)) as u16;
                assert_eq!(
                    pixel[3],
                    if picker.contains((x, y).into()) {
                        0
                    } else {
                        255
                    }
                );
            }
        }

        /// A sheet opened over the set panel is on top of it: the pointer
        /// over the sheet's rows is a hand, and a click there is the sheet's,
        /// whether the panel is docked or a sheet itself.
        #[test]
        fn a_sheet_over_the_set_panel_is_on_top() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 100, 30);
            press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
            // Docked at the right, where the theme picker's sheet also goes.
            app.regions = view::regions(
                app.frame,
                false,
                1,
                false,
                true,
                Some(app.set_panel_side()),
                None,
            );
            assert!(!app.regions.sidebar.is_empty());
            press(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
            let picker = app.theme_picker.as_ref().expect("the picker is open");
            let (sheet, rows) = picker.geometry(app.frame).expect("room");
            assert!(
                sheet.right() > app.regions.sidebar.x,
                "the sheet lies over the panel's column"
            );
            let point = (rows.x + 2, rows.y + 1);
            assert_eq!(app.pointer_shape_at(point.0, point.1), SHAPE_POINTER);
            let before = app.theme_picker.as_ref().map(|picker| picker.selected);
            app.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: point.0,
                row: point.1,
                modifiers: KeyModifiers::NONE,
            })
            .expect("click");
            assert_eq!(
                app.focus,
                Focus::Panel(PanelKind::Theme),
                "the sheet took the press"
            );
            assert_ne!(
                app.theme_picker.as_ref().map(|picker| picker.selected),
                before,
                "and chose its row"
            );
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.theme_picker.is_none());
            assert!(app.set_panel.is_some(), "the panel was under it all along");
            // The sheet form of the panel is under a sheet too.
            app.frame = Rect::new(0, 0, 60, 24);
            app.regions = view::regions(
                app.frame,
                false,
                1,
                false,
                true,
                Some(app.set_panel_side()),
                None,
            );
            assert!(app.regions.sidebar.is_empty());
            press(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
            let (_, rows) = app
                .theme_picker
                .as_ref()
                .and_then(|picker| picker.geometry(app.frame))
                .expect("room");
            assert_eq!(app.pointer_shape_at(rows.x + 2, rows.y), SHAPE_POINTER);
            app.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rows.x + 2,
                row: rows.y,
                modifiers: KeyModifiers::NONE,
            })
            .expect("click");
            assert_eq!(app.focus, Focus::Panel(PanelKind::Theme));
        }
    }
}
