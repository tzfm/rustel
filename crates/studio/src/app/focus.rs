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
