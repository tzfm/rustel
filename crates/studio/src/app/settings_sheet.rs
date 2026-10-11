//! The Settings sheet (Ctrl+O): opening and closing it, the room it takes above
//! the footer, whether it holds the keyboard, the key handler that applies what
//! its pages change, and what a click or the wheel does on the sheet. Also how
//! settings reach the rest of the studio: adopting the detected terminal at
//! startup, the shortcut profile, pushing UI settings and output latency to the
//! engine, the Keybinds page (learning a chord, building its rows), and keeping
//! the camera open for the Settings webcam preview or a camera theme.

use super::*;

impl App {
    /// Settings is a bottom-anchored sheet, but the Studio footer remains
    /// visible beneath it so actions taken in the sheet can still explain
    /// themselves. Painting and every pointer calculation use this room.
    pub(super) fn settings_sheet_frame(&self) -> Rect {
        view::settings_room(self.frame, self.regions.footer, self.regions.memory)
    }

    /// Whether the settings sheet has the keyboard and means to keep it.
    ///
    /// The sheet is a dialog, not a stop on the focus walk, so the only way
    /// to reach it is to open it. A chord that took the keys from under it
    /// would leave a sheet full of controls that answer nothing, with no
    /// way back. A prompt opened from the sheet is the exception: it is in
    /// front, and Esc comes back to the page.
    pub(super) fn settings_hold_the_keys(&self) -> bool {
        self.settings_sheet.is_some()
            && self.set_prompt.is_none()
            && self.focus == Focus::Panel(PanelKind::Settings)
    }

    /// The selected imported folder on the Samples page, if there is one.
    /// The cache controls and shipped packs have no local source to open.
    pub(super) fn selected_sample_source_folder(&self) -> Option<PathBuf> {
        let sheet = self.settings_sheet?;
        if !sheet.shows_sources() {
            return None;
        }
        let index = sheet
            .selected
            .checked_sub(super::super::settings::SOURCE_CONTROL_COUNT)?;
        if index >= sheet.source_count {
            return None;
        }
        self.prefs
            .sample_sources
            .get(index)
            .and_then(|source| sample_source_folder(&source.spec))
            .map(std::path::Path::to_path_buf)
    }

    /// Option/Alt+O reveals the imported folder selected on Samples.
    pub(super) fn reveal_selected_sample_source_folder(&mut self) -> bool {
        if let Some(folder) = self.selected_sample_source_folder() {
            self.reveal_target(RevealTarget::Folder(folder));
        }
        true
    }

    /// The shortcuts of the dialogs that replace Settings, which still
    /// answer while the sheet holds the keys. `true` when one answered.
    pub(super) fn settings_dialog_chord(
        &mut self,
        key: &KeyEvent,
        alt: bool,
    ) -> Result<bool, RuntimeError> {
        // These dialogs replace Settings, so their effective shortcuts
        // remain available there whether preferred, adapted or learned.
        // Editing chords still belong to the sheet and cannot reach the
        // covered score. Dismissing it also discards any pending reset.
        if self.settings_hold_the_keys()
            && let Some(action) = self.keybinds.action_for(key)
            && matches!(
                action,
                BindAction::ThemePicker
                    | BindAction::Export
                    | BindAction::Devices
                    | BindAction::OpenSet
                    | BindAction::Log
                    | BindAction::Jobs
            )
            && self.dispatch_bind_action(action, alt)?
        {
            return Ok(true);
        }
        Ok(false)
    }

    /// What the terminal can do decides the drawing tier, with the
    /// remembered switches on top; the log says what was found.
    pub(super) fn adopt_terminal(&mut self, features: TerminalFeatures) {
        super::super::graphics::set_cell_pixels(features.cell_pixels);
        self.last_pty_cell_pixels = cell_pixels_now();
        self.features = features;
        // The theme answers for any opacity the reader has not chosen, so a
        // theme's own look is what a fresh install starts with.
        self.ui_settings = self.prefs.ui_settings_for(&self.theme);
        self.apply_shortcut_profile();
        // The command line wins over the kept preference: the worker was
        // started with the flag's size, and what is applied below must
        // agree with it rather than put the kept size (or Automatic) back.
        if let Some(latency) = self.cli_output_latency {
            self.ui_settings.output_latency = latency;
        }
        self.apply_ui_settings();
        // The kept buffer goes to the engine here, not on the first
        // readiness poll: a score reaching the engine in the half second
        // before that would open the output on the automatic size.
        self.apply_output_latency();
        self.log.push(
            LogLevel::Info,
            "terminal",
            format!(
                "{} → drawing {}",
                self.features.summary(),
                super::super::graphics::tier().label()
            ),
        );
        // A warning stays counted in the header until the log is read. The
        // status line alone can be replaced before the first frame.
        if let Some(mismatch) = self.shortcut_profile_mismatch() {
            self.log.push(LogLevel::Warn, "terminal", mismatch.clone());
            self.status = mismatch;
        }
    }

    /// A profile override changes only shortcut conflicts, never the actual
    /// keyboard protocol or rendering capabilities detected from the terminal.
    pub(super) fn apply_shortcut_profile(&mut self) {
        let terminal = super::super::terminal::conflicts::effective_profile(
            &self.features.name,
            self.ui_settings.terminal_profile.as_deref(),
        );
        self.keybinds.set_reach(super::super::keybinds::Reach {
            enhanced: cfg!(windows) || self.features.keyboard,
            terminal,
        });
        self.dirty_frame = true;
    }

    pub(super) fn shortcut_profile_label(&self) -> String {
        match self.ui_settings.terminal_profile.as_deref() {
            Some(name) => super::super::terminal::conflicts::effective_profile(
                &self.features.name,
                Some(name),
            ),
            None if self.features.name.is_empty() => "automatic".into(),
            None => format!("automatic ({})", self.features.name),
        }
    }

    /// What to say when the pinned shortcut profile describes another
    /// terminal than the detected one. `None` when the two agree. A narrow
    /// status line cuts the end, so the detected terminal comes first.
    fn shortcut_profile_mismatch(&self) -> Option<String> {
        let detected = super::super::terminal::conflicts::pinned_over(
            &self.features.name,
            self.ui_settings.terminal_profile.as_deref(),
        )?;
        Some(format!(
            "this terminal is {detected}, but shortcut profile {} is selected - Del on Settings > Keybinds > Terminal returns to automatic; until then Studio does not avoid the keys this terminal takes",
            self.shortcut_profile_label()
        ))
    }

    pub(super) fn apply_ui_settings(&mut self) {
        self.ui_settings.apply();
        // An open list follows the switch at once.
        if self
            .reference
            .set_hidden(self.ui_settings.hidden_categories())
            && let Some(panel) = self.reference_panel.as_mut()
        {
            panel.refresh(&self.reference);
        }
        if let Some(panel) = self.set_panel.as_mut() {
            panel.on_right = self.ui_settings.set_panel_right;
        }
        if let Some(panel) = self.mixer_panel.as_mut() {
            panel.edge = MixerEdge::from_top(self.ui_settings.mixer_top);
        }
        for (index, panel) in self.viz_docks.iter_mut().enumerate() {
            if let Some(panel) = panel {
                panel.edge = self.ui_settings.viz_edges[index];
            }
        }
        for (dock, edge) in self
            .prefs
            .visuals
            .iter_mut()
            .zip(self.ui_settings.viz_edges)
        {
            dock.edge = edge;
        }
        #[cfg(feature = "hydra")]
        if let Some(policy) = &self.hydra_input_policy {
            policy.set_webcam_allowed(self.ui_settings.hydra_webcam);
        }
        #[cfg(feature = "hydra")]
        self.sync_settings_webcam_preview();
        // Automatic follows the probe; Advanced can choose a supported lower tier.
        super::super::graphics::set_tier(self.ui_settings.rendering.resolve(&self.features));
        // Clock ports reach the engine when they change; the engine says
        // in the log if a port cannot be opened.
        if self.applied_clock.0 != self.ui_settings.clock_out {
            self.applied_clock.0 = self.ui_settings.clock_out.clone();
            let _ = self
                .worker
                .try_clock_out(self.ui_settings.clock_out.clone());
        }
        if self.applied_clock.1 != self.ui_settings.clock_in {
            self.applied_clock.1 = self.ui_settings.clock_in.clone();
            let _ = self.worker.try_clock_in(self.ui_settings.clock_in.clone());
        }
        self.configure_piano();
        self.worker
            .master()
            .set_load_mode(self.ui_settings.load_mode);
        // The set's own limiter if it has one, else the studio's.
        self.worker.master().set_limiter(self.live_master_limiter());
        self.worker
            .master()
            .set_limiter_makeup(self.ui_settings.master_limiter_makeup);
        // Process-wide, and set from here rather than handed to the
        // worker: the decoders and the fetcher that enforce it run on
        // loader threads with no engine in reach.
        rustel_audio::set_sample_pcm_ceiling(self.ui_settings.sample_ceiling.bytes());
        self.worker
            .master()
            .set_max_polyphony(self.ui_settings.max_polyphony);
        self.sample_memory_owed = Some((
            self.ui_settings.preview_budget.bytes(),
            self.ui_settings.unused_sample_idle.duration(),
        ));
        self.send_sample_memory();
        self.invalidate_maps();
        self.dirty_frame = true;
    }

    /// Whether the theme the studio paints its own camera for is the
    /// backdrop on screen right now.
    ///
    /// Every clause mirrors the rule a Hydra camera theme already obeys
    /// (`sync_theme_sketch`): the webcam switch is the consent; the theme
    /// must be the one that declares a camera; a draft being typed in the
    /// theme editor is not a theme being chosen; and the theme must either
    /// have been kept or be the one the picker is showing. The one clause
    /// of its own is `hydra_last`: a score's own picture is drawn over the
    /// theme's, so while a score is drawing, the theme's camera would be
    /// opened for something nobody can see.
    #[cfg(feature = "hydra")]
    fn native_camera_theme_visible(&self) -> bool {
        let confirmed = self.theme_camera_confirmed.as_deref() == Some(self.theme.name.as_str());
        let previewing = self.theme_picker.is_some();
        self.ui_settings.hydra_webcam
            && self.theme.native_camera_enabled()
            && self.theme_editor.is_none()
            && self.opacity_before_theme_editor.is_none()
            && self.hydra_last.is_none()
            && (confirmed || previewing)
    }

    /// Whether the settings row wants the camera up, whether or not the
    /// switch has agreed to it yet: Enter on the row opens exactly this,
    /// and it is a request to look, not a request for consent - the switch
    /// still says what a score or a confirmed theme may do with it.
    #[cfg(feature = "hydra")]
    pub(super) fn settings_webcam_preview_visible(&self) -> bool {
        self.help.is_none()
            && self.replay_edit.is_none()
            && self.focus == Focus::Panel(PanelKind::Settings)
            && super::super::settings::SettingsSheetView::geometry(self.settings_sheet_frame())
                .is_some()
            && self
                .settings_sheet
                .is_some_and(SettingsSheet::webcam_preview_open)
    }

    /// Both consumers of the one lease-less camera session: the Settings
    /// thumbnail and a theme the studio paints itself. They are set
    /// separately and the session is cancelled only once neither wants it,
    /// so leaving the settings row while a camera theme is up does not
    /// close the camera the theme is drawing.
    #[cfg(feature = "hydra")]
    pub(super) fn sync_settings_webcam_preview(&self) {
        let requested = self.settings_webcam_preview_visible();
        let theme_picture = self.native_camera_theme_visible();
        if let Some(policy) = &self.hydra_input_policy {
            // The switch is not the only way to consent: an open preview is
            // someone looking at the camera before deciding, and it must
            // open the device for that to mean anything. Leaving the row -
            // or dismissing it with Esc - drops `requested`, and consent
            // falls straight back to whatever the switch alone says.
            policy.set_webcam_allowed(self.ui_settings.hydra_webcam || requested);
            policy.set_settings_preview_requested(requested);
            policy.set_theme_picture_requested(theme_picture);
        }
        if !theme_picture {
            self.theme_visual.clear_camera();
        }
    }

    /// Keep navigation in memory when any route dismisses the settings sheet.
    pub(super) fn close_settings_sheet(&mut self) {
        if let Some(sheet) = self.settings_sheet.take() {
            self.settings_navigation = sheet.remembered_navigation();
        }
    }

    /// Open or close the settings sheet using its current shortcut.
    pub(super) fn toggle_settings_sheet(&mut self) {
        if self.settings_sheet.is_some() && self.focus != Focus::Panel(PanelKind::Settings) {
            self.focus_panel(PanelKind::Settings);
            return;
        }
        if self.settings_sheet.is_none() {
            self.dismiss_dialogs(Some(PanelKind::Settings));
        }
        if self.settings_sheet.is_some() {
            self.close_settings_sheet();
        } else {
            // What the terminal answered stays in the log, written once
            // at startup: a row of ✗ on every opening reads as failures.
            self.status = "settings".into();
            self.settings_sheet = Some(self.settings_navigation.open());
        }
        if self.settings_sheet.is_some() {
            self.measure_sample_cache();
            self.focus_panel(PanelKind::Settings);
        } else {
            self.focus = Focus::Editor;
        }
        #[cfg(feature = "hydra")]
        self.sync_settings_webcam_preview();
        self.dirty_frame = true;
    }

    /// A press on the Settings sheet's tabs and rows: a tab shows its page,
    /// a row is chosen as the arrows would choose it, and a slot or keybind
    /// row is a button. Returns true when one of them took the press; the
    /// rest of the sheet is claimed by `claim_settings_sheet`.
    pub(super) fn click_settings_sheet(&mut self, x: u16, y: u16) -> bool {
        if self.settings_sheet.is_some()
            && let Some(page) = SettingsSheet::tab_at(self.settings_sheet_frame(), x, y)
        {
            self.focus_panel(PanelKind::Settings);
            self.pointer = Some(Pointer::Panel);
            if let Some(sheet) = self.settings_sheet.as_mut() {
                sheet.show_page(page);
            }
            #[cfg(feature = "hydra")]
            self.sync_settings_webcam_preview();
            self.dirty_frame = true;
            return true;
        }
        // A settings control: the click chooses it, as the arrows do.
        // Hydra webcam's live preview stays off until Enter - browsing
        // past the row must not open the camera.
        if let Some(row) = self
            .settings_sheet
            .filter(|sheet| sheet.shows_settings())
            .and_then(|sheet| sheet.row_at_for(self.settings_sheet_frame(), x, y))
        {
            self.focus_panel(PanelKind::Settings);
            self.pointer = Some(Pointer::Panel);
            if let Some(sheet) = self.settings_sheet.as_mut() {
                sheet.select(row);
            }
            #[cfg(feature = "hydra")]
            self.sync_settings_webcam_preview();
            self.dirty_frame = true;
            return true;
        }
        // A slot box is a button: the click picks it, and picking
        // it is what Enter then learns.
        if let Some(slot) = self
            .settings_sheet
            .and_then(|sheet| sheet.slot_at(self.settings_sheet_frame(), x, y))
        {
            self.focus_panel(PanelKind::Settings);
            self.pointer = Some(Pointer::Panel);
            if let Some(sheet) = self.settings_sheet.as_mut() {
                sheet.selected = slot;
            }
            self.status = format!(
                "slot {}: Enter waits for a knob, fader or stick - Space unbinds",
                slot + 1
            );
            self.dirty_frame = true;
            return true;
        }
        // A keybind row is a button: the click arms the learn on
        // it, exactly as Enter does, and a click on the row
        // already armed calls the learn off - the mouse's answer
        // to Esc. While a take is being asked, a click keeps the
        // chord: choosing to take it is Enter's work alone, and
        // mousing into the ask must not take it by accident.
        //
        // The row count is refreshed here as the keyboard path
        // refreshes its own: a mouse-only arrival on the page -
        // tab clicked, never a key pressed - must still find its
        // rows, or the page reads fine and clicks like a wall.
        if let Some(sheet) = self.settings_sheet.as_mut() {
            sheet.keybind_count = super::super::settings::RESET_KEYBINDS_ROW + 1;
        }
        if let Some(row) = self
            .settings_sheet
            .filter(|sheet| sheet.shows_keybinds())
            .and_then(|sheet| sheet.keybind_row_at(self.settings_sheet_frame(), x, y))
        {
            self.focus_panel(PanelKind::Settings);
            self.pointer = Some(Pointer::Panel);
            let action = super::super::settings::keybind_action_at(row);
            if let Some(sheet) = self.settings_sheet.as_mut() {
                if sheet.confirm_reset_keybinds && sheet.selected != row {
                    self.status = "shortcut reset cancelled".into();
                }
                sheet.select(row);
                sheet.hold_scroll = true;
            }
            if row == super::super::settings::TERMINAL_PROFILE_ROW {
                self.keybind_learn = None;
                self.handle_settings_key(KeyCode::Enter, false, false);
            } else if row == super::super::settings::RESET_KEYBINDS_ROW {
                self.keybind_learn = None;
                // A click can request the reset; confirmation remains
                // Enter so a double click cannot erase custom bindings.
                if !self
                    .settings_sheet
                    .is_some_and(|sheet| sheet.confirm_reset_keybinds)
                {
                    self.handle_settings_key(KeyCode::Enter, false, false);
                }
            } else if let Some(action) = action {
                let same_row_armed = self
                    .keybind_learn
                    .is_some_and(|learn| learn.action() == action);
                if same_row_armed {
                    self.keybind_learn = None;
                    self.status = "learn called off".into();
                } else {
                    self.keybind_learn = Some(super::super::keybinds::KeybindLearn::arm(action));
                    self.status = "press the chord - Esc calls it off, and the sheet's own keys stay the sheet's".into();
                }
            }
            self.dirty_frame = true;
            return true;
        }
        // A source row: the click chooses it, as the arrows do.
        // Acting on it - refetch, cache, remove - stays the keys'
        // work, so arriving on a row cannot start a download or
        // drop an import by accident. The counts are refreshed here
        // as the keyboard path refreshes them, for a mouse-only
        // arrival on the page.
        if let Some(sheet) = self.settings_sheet.as_mut() {
            sheet.source_count = self.prefs.sample_sources.len();
            sheet.default_count = self.shipped_sources.len();
        }
        if let Some(row) = self
            .settings_sheet
            .and_then(|sheet| sheet.source_row_at(self.settings_sheet_frame(), x, y))
        {
            self.focus_panel(PanelKind::Settings);
            self.pointer = Some(Pointer::Panel);
            if let Some(sheet) = self.settings_sheet.as_mut() {
                if sheet.selected != row {
                    sheet.confirm_clear_cache = false;
                }
                sheet.selected = row;
                sheet.hold_scroll = true;
            }
            #[cfg(feature = "hydra")]
            self.sync_settings_webcam_preview();
            self.dirty_frame = true;
            return true;
        }
        false
    }

    /// A press anywhere on the open Settings sheet is a claim on it,
    /// clickable row or not: it must not focus the editor and teleport the
    /// caret through the panel. Returns true when the sheet took the press.
    pub(super) fn claim_settings_sheet(&mut self, x: u16, y: u16) -> bool {
        if self.settings_sheet.is_some()
            && super::super::settings::SettingsSheetView::geometry(self.settings_sheet_frame())
                .is_some_and(|(sheet, _)| within(sheet, x, y))
        {
            self.focus_panel(PanelKind::Settings);
            self.pointer = Some(Pointer::Panel);
            return true;
        }
        false
    }

    /// The wheel over Settings is the page's Up/Down key. This keeps every
    /// scrollable page on the keyboard's selection and margin rules.
    /// Whether it took the wheel.
    pub(super) fn scroll_settings_sheet(&mut self, x: u16, y: u16, direction: f32) -> bool {
        if self.settings_sheet.is_some()
            && super::super::settings::SettingsSheetView::geometry(self.settings_sheet_frame())
                .is_some_and(|(sheet, _)| within(sheet, x, y))
        {
            self.focus_panel(PanelKind::Settings);
            let key = if direction > 0.0 {
                KeyCode::Up
            } else {
                KeyCode::Down
            };
            self.handle_settings_key(key, false, false);
            return true;
        }
        false
    }

    /// Returns true when the sheet consumed the key.
    pub(super) fn handle_settings_key(
        &mut self,
        code: KeyCode,
        primary: bool,
        shift: bool,
    ) -> bool {
        let Some(mut sheet) = self.settings_sheet else {
            return false;
        };
        // Retired settings keys must not close the sheet after a rebind.
        let mut modifiers = KeyModifiers::NONE;
        if primary {
            modifiers |= KeyModifiers::CONTROL;
        }
        if shift {
            modifiers |= KeyModifiers::SHIFT;
        }
        let event = KeyEvent::new(code, modifiers);
        let closes = self.keybinds.action_for(&event) == Some(BindAction::Settings);
        if closes {
            self.close_settings_sheet();
            // The keys go back to the score, not to a sheet that is gone.
            self.settle_focus();
            #[cfg(feature = "hydra")]
            self.sync_settings_webcam_preview();
            self.dirty_frame = true;
            return true;
        }
        if primary {
            if sheet.confirm_reset_keybinds {
                sheet.confirm_reset_keybinds = false;
                self.settings_sheet = Some(sheet);
                self.status = "shortcut reset cancelled".into();
                self.dirty_frame = true;
            }
            // Settings is a modal sheet. A chord it does not implement is
            // still the sheet's: letting Ctrl+V, Ctrl+X or Ctrl+Z fall
            // through edits the covered score. Bracketed paste is routed to
            // a picker separately, after that picker has taken focus.
            return true;
        }
        // Esc while learning cancels the learn and keeps the sheet.
        if code == KeyCode::Esc
            && (self.mapping_learn.take().is_some() || self.keybind_learn.take().is_some())
        {
            self.status = "learn cancelled".into();
            self.dirty_frame = true;
            return true;
        }
        if sheet.confirm_reset_keybinds && code != KeyCode::Enter {
            self.status = "shortcut reset cancelled".into();
        }
        // Esc while emptying is armed cancels the ask, not the sheet -
        // same two-step as deleting a theme or a set line.
        if code == KeyCode::Esc && sheet.confirm_clear_cache {
            sheet.confirm_clear_cache = false;
            self.settings_sheet = Some(sheet);
            self.status = "empty cancelled".into();
            self.dirty_frame = true;
            return true;
        }
        // Esc while the camera preview is up and the switch is still off
        // backs out of the picture alone: the switch is exactly where
        // Enter found it, the camera goes with the picture, and the sheet
        // stays open to decide again. Once the switch has already said
        // yes, there is no decision left to lose, and Esc goes back to
        // closing the sheet like every other row.
        #[cfg(feature = "hydra")]
        if code == KeyCode::Esc && sheet.webcam_preview_open() && !self.ui_settings.hydra_webcam {
            sheet.webcam_preview = false;
            self.settings_sheet = Some(sheet);
            self.status = "camera preview closed \u{b7} webcam stays off".into();
            self.sync_settings_webcam_preview();
            self.dirty_frame = true;
            return true;
        }
        let mut settings = self.ui_settings.clone();
        sheet.source_count = self.prefs.sample_sources.len();
        sheet.default_count = self.shipped_sources.len();
        // The learnable rows plus the fixed ones past them (the panels'
        // Alt chords, which a learn can neither take nor move).
        sheet.keybind_count = super::super::settings::RESET_KEYBINDS_ROW + 1;
        match sheet.key_in_frame(
            code,
            &mut settings,
            &self.features,
            self.settings_sheet_frame(),
        ) {
            SettingsAction::Nothing => {}
            // The picture opens the device; the setting is untouched by
            // Enter entirely, so the hint names the keys that DO choose
            // rather than leaving the reader to guess which one commits.
            #[cfg(feature = "hydra")]
            SettingsAction::OpenWebcamPreview => {
                self.status =
                    "\u{2190}/\u{2192} switch the camera on \u{b7} Enter or Esc puts the picture away"
                        .into();
            }
            // A key the sheet has no use for is not the sheet's. On the
            // keybinds page that is the point: a learn armed, the key has
            // already been taken for the chord by `capture_keybind_chord`
            // and never reaches this arm; without a learn it falls through
            // to the score the way an ignored key would.
            SettingsAction::Capture => return false,
            SettingsAction::Ignored => return false,
            SettingsAction::Changed => {
                let polyphony_changed = settings.max_polyphony != self.ui_settings.max_polyphony;
                let terminal_profile_changed =
                    settings.terminal_profile != self.ui_settings.terminal_profile;
                let precache_changed =
                    settings.precache_sources != self.ui_settings.precache_sources;
                let zen_changed = settings.zen != self.ui_settings.zen;
                let visualizers_changed = settings.animation != self.ui_settings.animation;
                let output_latency_changed =
                    settings.output_latency != self.ui_settings.output_latency;
                let syntax_check_changed = settings.syntax_check != self.ui_settings.syntax_check;
                // A set with a limiter of its own keeps it, so the row can
                // change the default and this set go on sounding the same.
                // That is the design, but it is not obvious from the sheet,
                // so it is said out loud rather than looking broken.
                let default_limiter_shadowed = settings.master_limiter_on
                    != self.ui_settings.master_limiter_on
                    && *self.scenes.limiter() != SetLimiter::Defer;
                // Touching one of the three opacity rows makes them the
                // reader's: they then survive a restart and no theme
                // overwrites them again. Detected by value rather than by row,
                // because the sheet reports that something changed and not
                // which thing.
                let opacity_changed = settings.backdrop_opacity
                    != self.ui_settings.backdrop_opacity
                    || settings.interface_opacity != self.ui_settings.interface_opacity
                    || settings.editor_opacity != self.ui_settings.editor_opacity;
                // The second Enter on an open preview, turning the switch on
                // now that the picture has already answered what it looks
                // like. Worth its own line: the generic "settings kept"
                // never mentions a camera, and this is the one row where
                // that would read as the studio staying quiet about it.
                #[cfg(feature = "hydra")]
                let webcam_confirmed = settings.hydra_webcam && !self.ui_settings.hydra_webcam;
                #[cfg(not(feature = "hydra"))]
                let webcam_confirmed = false;
                self.ui_settings = settings;
                if terminal_profile_changed {
                    self.apply_shortcut_profile();
                }
                self.apply_ui_settings();
                if opacity_changed {
                    self.prefs.claim_opacities(&self.ui_settings);
                }
                // Off gives the visualizers' rows back to the code at once;
                // on opens them again - not at the next evaluation.
                if visualizers_changed && let Some(revision) = self.visual_revision {
                    self.install_virtual_rows(revision);
                }
                if zen_changed {
                    self.apply_zen();
                }
                self.settle_menu();
                if output_latency_changed {
                    self.apply_output_latency();
                }
                if syntax_check_changed {
                    self.apply_syntax_check();
                }
                self.prefs.set_ui_settings(&self.ui_settings);
                self.save_prefs_soon();
                // Turning pre-caching on is a request for the files NOW, not
                // at the next restart; off leaves the files already on their
                // way to land, but the row stops following them.
                let mut precache_started = false;
                if precache_changed && self.ui_settings.precache_sources {
                    self.precache_sources();
                    precache_started = true;
                }
                // Zen says its own thing, and it is the more useful line: it
                // is the one that tells you how to get the stage back.
                if terminal_profile_changed {
                    self.status = self.shortcut_profile_mismatch().unwrap_or_else(|| {
                        format!(
                            "shortcut profile: {} - custom bindings kept",
                            self.shortcut_profile_label()
                        )
                    });
                } else if polyphony_changed {
                    self.status =
                        if let Some(voices) = self.worker.master().max_polyphony_override() {
                            format!(
                                "polyphony default: {} · score keeps {voices}",
                                self.ui_settings.max_polyphony
                            )
                        } else {
                            format!("polyphony: {} voices", self.ui_settings.max_polyphony)
                        };
                } else if precache_started {
                    self.status =
                        "fetch imports on - the score's imported sounds cache as they resolve"
                            .into();
                } else if default_limiter_shadowed {
                    self.status = format!(
                        "limiter default kept \u{b7} {} has answered for itself \u{b7} {} changes that",
                        self.scenes.name(),
                        self.shortcut_or_menu(BindAction::Mixer, "View > Mixer")
                    );
                } else if syntax_check_changed {
                    // The marks going away could read as the score having
                    // been fixed, so each mode says what it still does.
                    self.status = match self.ui_settings.syntax_check {
                        SyntaxCheck::Full => {
                            "syntax check full - errors are marked a pause after typing stops"
                        }
                        SyntaxCheck::OnUpdate => {
                            "syntax check on update - errors are marked where an update is refused"
                        }
                        SyntaxCheck::Off => {
                            "syntax check off - nothing is marked; the footer and the log still say why an update is refused"
                        }
                    }
                    .into();
                } else if webcam_confirmed {
                    self.status =
                        "webcam on - the picture stays up while this row is selected".into();
                } else if !zen_changed {
                    self.status = format!(
                        "settings kept - drawing with {}",
                        super::super::graphics::tier().label()
                    );
                }
            }
            SettingsAction::Close => {
                self.close_settings_sheet();
                self.settle_focus();
                #[cfg(feature = "hydra")]
                self.sync_settings_webcam_preview();
                self.dirty_frame = true;
                return true;
            }
            // The sheet hands the keyboard to the editor and closes itself.
            SettingsAction::OpenPrebake(scope) => {
                self.open_prebake(scope);
                #[cfg(feature = "hydra")]
                self.sync_settings_webcam_preview();
                return true;
            }
            // The sheet hands over to a picker for the folder.
            SettingsAction::ChooseSetsFolder => {
                self.settings_sheet = Some(sheet);
                self.open_set_prompt(SetPrompt::SetsFolder);
                #[cfg(feature = "hydra")]
                self.sync_settings_webcam_preview();
                return true;
            }
            SettingsAction::ChooseRecordingsFolder => {
                self.settings_sheet = Some(sheet);
                self.open_set_prompt(SetPrompt::RecordingsFolder);
                #[cfg(feature = "hydra")]
                self.sync_settings_webcam_preview();
                return true;
            }
            SettingsAction::AskClearSampleCache => {
                self.status = "Enter again clears the cache · Esc cancels".into();
            }
            SettingsAction::ClearSampleCache => self.clear_sample_cache(),
            SettingsAction::CacheWholeLibrary => self.cache_whole_library(),
            SettingsAction::RefreshAllSources => self.refresh_all_sample_packs(),
            SettingsAction::CacheDefaultSource(at) => self.cache_default_source(at),
            SettingsAction::CacheImportSource(at) => self.cache_user_source(at),
            SettingsAction::AskResetKeybinds => {
                self.keybind_learn = None;
                self.status = format!(
                    "reset all shortcuts for {} - Enter confirms · Esc cancels",
                    self.shortcut_profile_label()
                );
            }
            SettingsAction::ResetKeybinds => {
                self.keybind_learn = None;
                self.keybinds.reset_all();
                self.prefs.set_keybinds(&self.keybinds);
                self.save_prefs_soon();
                self.status = format!("shortcuts reset - {}", self.shortcut_profile_label());
            }
            SettingsAction::LearnKeybind(action) => {
                self.keybind_learn = Some(super::super::keybinds::KeybindLearn::arm(action));
                self.status =
                    "press the chord - Esc calls it off, and the sheet's own keys stay the sheet's"
                        .into();
            }
            SettingsAction::ClearKeybind(action) => {
                self.keybind_learn = None;
                self.learn_keybind(action, None);
            }
            SettingsAction::LearnSlot(slot) => {
                self.mapping_learn = Some(slot);
                self.status = format!(
                    "slot {}: move the knob, fader or stick that should drive it - Esc cancels",
                    slot + 1
                );
            }
            SettingsAction::StepTakeover(slot, forwards) => {
                let mode = self.ui_settings.takeover[slot].step(forwards);
                self.ui_settings.takeover[slot] = mode;
                self.prefs.set_ui_settings(&self.ui_settings);
                self.save_prefs_soon();
                // The remembered knob position belongs to the old mode, and
                // a scaled turn reckoned from it would be a jump by another
                // name. The next message re-learns where the hand is.
                self.mapping_knob[slot] = None;
                self.status = format!("slot {} \u{00b7} {}", slot + 1, mode.label());
            }
            SettingsAction::ClearSlot(slot) => {
                self.mapping_learn = None;
                if self.ui_settings.mappings[slot].take().is_some() {
                    self.prefs.set_ui_settings(&self.ui_settings);
                    self.save_prefs_soon();
                    self.status = format!("slot {} unbound", slot + 1);
                } else {
                    self.status = format!("slot {} is not bound to anything", slot + 1);
                }
            }
            // The prompt opens over the page, which stays where it is: Esc
            // comes back to the list, and an import lands on it as a row.
            // Closing the sheet first left Esc with nothing to come back to.
            SettingsAction::AddSource => {
                self.settings_sheet = Some(sheet);
                self.open_set_prompt(SetPrompt::AddSampleSource);
                #[cfg(feature = "hydra")]
                self.sync_settings_webcam_preview();
                return true;
            }
            SettingsAction::EditSource(at) => {
                let Some(spec) = self
                    .prefs
                    .sample_sources
                    .get(at)
                    .map(|source| source.spec.clone())
                else {
                    return true;
                };
                self.settings_sheet = Some(sheet);
                self.open_set_prompt(SetPrompt::EditSampleSource(at));
                if let Some((_, picker)) = self.set_prompt.as_mut() {
                    picker.offer(&spec);
                }
                return true;
            }
            SettingsAction::RemoveSource(at) => {
                if at < self.prefs.sample_sources.len() {
                    let gone = self.prefs.sample_sources.remove(at);
                    sheet.selected = at.min(self.prefs.sample_sources.len().saturating_sub(1));
                    self.save_prefs_soon();
                    self.adopt_global_sources();
                    self.status = format!(
                        "{} removed",
                        status_sample_source(&gone.spec, self.ui_settings.show_full_paths)
                    );
                }
            }
            SettingsAction::ToggleSource(at) => {
                if let Some(source) = self.prefs.sample_sources.get_mut(at) {
                    source.enabled = !source.enabled;
                    let (spec, on) = (source.spec.clone(), source.enabled);
                    self.save_prefs_soon();
                    self.adopt_global_sources();
                    self.status = format!(
                        "{} {}",
                        status_sample_source(&spec, self.ui_settings.show_full_paths),
                        if on { "on" } else { "off" }
                    );
                }
            }
            SettingsAction::RenameSourceBank(at) => {
                self.settings_sheet = Some(sheet);
                self.open_bank_rename_from_source(at);
                #[cfg(feature = "hydra")]
                self.sync_settings_webcam_preview();
                return true;
            }
            SettingsAction::RefreshSource(at) => self.refresh_sample_source(at),
        }
        // The keybind learn belongs to the Keybinds page: Tabbed away
        // from it - or closed by any door the sheet has - the learn goes
        // with the page, or the next key pressed days later on another
        // page would be bound in silence. The sheet is held locally here,
        // so the page it is on now is the page the key landed on.
        if self.keybind_learn.is_some() && !sheet.shows_keybinds() {
            self.keybind_learn = None;
            self.status = "learn called off".into();
        }
        // Where the page is scrolled to is kept for the next key to move
        // from, in the frame the sheet is drawn in.
        sheet.settle_scroll(self.settings_sheet_frame());
        self.settings_sheet = Some(sheet);
        #[cfg(feature = "hydra")]
        self.sync_settings_webcam_preview();
        self.dirty_frame = true;
        true
    }

    /// How the open set's own answer about the limiter reads on the
    /// settings sheet, or `None` where the set has not answered and the
    /// studio's switch is the whole story.
    ///
    /// The mode and nothing else. It shares a value column twenty-two
    /// characters wide with the switch itself, and the ceiling is on the
    /// set's own strip where it can be turned - a number here that could
    /// be trimmed to `-24.…` says less than no number at all.
    pub(super) fn set_limiter_label(&self) -> Option<String> {
        match self.scenes.limiter() {
            SetLimiter::Defer => None,
            SetLimiter::None => Some("none".to_owned()),
            SetLimiter::Says { bypassed, settings } => Some(if *bypassed {
                format!("{} byp", settings.character.short())
            } else {
                settings.character.short().to_owned()
            }),
            // Named rather than hidden: a set playing without the limiter
            // it asked for should say why on the row that looks wrong.
            SetLimiter::Unreadable(_) => Some("unreadable".to_owned()),
        }
    }

    /// Frames for the engine's live output: the knob's choice, `None` for
    /// the automatic policy. Retried on the next turn while the queue is
    /// full.
    pub(super) fn apply_output_latency(&mut self) {
        let frames = self.ui_settings.output_latency.frames();
        let frames = (frames != 0).then_some(frames);
        self.output_latency_pending = !self.worker.try_set_output_buffer_frames(frames);
    }

    /// The Keybinds page's learn, ahead of everything else the studio
    /// answers to: the next key press that is not the sheet's own becomes
    /// the chord being learnt. `true` when the learn took the press.
    pub(super) fn capture_keybind_chord(&mut self, terminal_event: &Event) -> bool {
        // The learner is holding a chord for the Keybinds page: the very next
        // key press that is not the sheet's own is the chord. Captured ahead
        // of everything - modals, transport, panels - so a learn can take a
        // key the studio already answers to, which is the only way to move
        // one. A press only: a release or autorepeat of the key the finger
        // is still on is no chord anyone chose. The learn belongs to the
        // sheet, and the sheet keeps Esc and Tab for itself, so the two keys
        // that must never be captured stay the sheet's, exactly as the
        // mapping page's learn does - and the sheet still being open is
        // part of that: closed by the mouse the instant before, the armed
        // learn is the sheet's no longer, and the key goes on to whatever
        // it would have meant, rather than binding in silence.
        if let Event::Key(key) = terminal_event
            && key.kind == KeyEventKind::Press
            && self.keybind_learn.is_some()
            && self.settings_sheet.is_some()
            && !matches!(key.code, KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab)
        {
            // Enter is the take's own key: while the learn is asking -
            // the chord in hand is another action's - Enter confirms the
            // take, exactly as the ask said it would. While the learn is
            // merely armed, Enter is refused, not captured: it is the key
            // that arms and confirms, and a page whose Enter had been
            // learnt away could never be worked again short of hand-
            // editing `studio.json`. The learn stays armed, nothing moves.
            if key.code == KeyCode::Enter {
                match self.keybind_learn {
                    Some(super::super::keybinds::KeybindLearn::Confirm {
                        action,
                        chord,
                        held_by,
                    }) => {
                        self.keybind_learn = None;
                        self.learn_keybind(action, Some(chord));
                        // A panel action that loses a learnt chord goes
                        // back to its Alt letter.
                        let left = match self.keybinds.hint(held_by) {
                            hint if hint.is_empty() => "was unbound".to_owned(),
                            hint => format!("is back on {hint}"),
                        };
                        self.status = format!(
                            "{} is now {} - {} {left}",
                            action.label(),
                            chord.hint(),
                            held_by.label()
                        );
                    }
                    Some(super::super::keybinds::KeybindLearn::Armed(_)) => {
                        self.status = "Enter is reserved - it can't be a shortcut".into();
                        self.dirty_frame = true;
                    }
                    None => {}
                }
                return true;
            }
            let learning = self.keybind_learn.take().expect("learn armed");
            let control = key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER);
            // An uppercase letter is shift's own evidence, the same fold
            // the chord reader and the editor's both make.
            let shift = key.modifiers.contains(KeyModifiers::SHIFT)
                || matches!(key.code, KeyCode::Char(character) if character.is_uppercase());
            let code = match key.code {
                KeyCode::Char(character) => KeyCode::Char(character.to_ascii_lowercase()),
                other => other,
            };
            let chord = super::super::keybinds::KeyCombo {
                code,
                control,
                shift,
            };
            // The chord decides: free is taken outright; another action's
            // becomes an ask the row itself now carries - Enter takes it
            // and the loser goes unbound, Esc keeps things as they were.
            // A press while the ask is up re-plans with the new chord, the
            // way re-pressing in a learn re-captures.
            match learning.plan(&self.keybinds, chord) {
                super::super::keybinds::KeybindCapture::Take { action, chord } => {
                    self.learn_keybind(action, Some(chord));
                }
                super::super::keybinds::KeybindCapture::Ask {
                    action,
                    chord,
                    held_by,
                } => {
                    self.keybind_learn = Some(super::super::keybinds::KeybindLearn::Confirm {
                        action,
                        chord,
                        held_by,
                    });
                    self.status = format!(
                        "{} is already bound to {} - Enter to rebind it here, Esc to keep it",
                        held_by.label(),
                        chord.hint()
                    );
                    self.dirty_frame = true;
                }
                // A key that can never be a shortcut lands here: the
                // sheet's own keys (Esc, Tab and BackTab never reach this
                // arm), Enter's kin, space, the arrows, and bare typing
                // characters. The status names the refused key, so a
                // refusal cannot look like an acceptance. The learn stays
                // armed and nothing moves.
                super::super::keybinds::KeybindCapture::Refused { chord: refused } => {
                    self.keybind_learn = Some(learning);
                    // A bare modifier press is the first half of a chord
                    // still being formed - the finger on ctrl before the
                    // letter - not a key anyone chose, so it is no-op'd
                    // like a release rather than answered with a word.
                    if !matches!(refused.code, KeyCode::Modifier(_)) {
                        self.status = if super::super::keybinds::KeybindLearn::is_a_typing_key(
                            refused,
                        ) {
                            // Most terminals fold ctrl plus a symbol down
                            // to the bare character (ctrl+' arrives as
                            // just '), so the shortcut the learner meant
                            // is not one any terminal can deliver - and
                            // the bare character is one the score is
                            // typed with. Said plainly, so the press
                            // never reads as if it worked.
                            format!(
                                "{} is a typing key - hold ctrl to make it a shortcut",
                                refused.hint()
                            )
                        } else if matches!(refused.code, KeyCode::Null | KeyCode::Char(' '))
                            && refused.control
                        {
                            // Terminals send ctrl+space in different ways,
                            // and some keep it. A binding to the piece
                            // that lands would be a key that cannot fire.
                            format!(
                                "{} cannot be a custom shortcut - where the terminal sends it, it shows the argument values",
                                refused.hint()
                            )
                        } else {
                            format!(
                                "{} is reserved by the studio and can't be a shortcut",
                                refused.hint()
                            )
                        };
                        self.dirty_frame = true;
                    }
                }
            }
            return true;
        }
        false
    }

    /// Bind what was just pressed to the action the Keybinds page is
    /// waiting on, and say so. A chord already on another action moves to
    /// this one rather than meaning two things - one chord, one meaning.
    pub(super) fn learn_keybind(
        &mut self,
        action: super::super::keybinds::BindAction,
        chord: Option<super::super::keybinds::KeyCombo>,
    ) {
        self.keybind_learn = None;
        self.keybinds.learn(action, chord);
        self.prefs.set_keybinds(&self.keybinds);
        self.save_prefs_soon();
        match chord {
            Some(chord) => self.status = format!("{} · {}", action.label(), chord.hint()),
            None => self.status = format!("{} · back to the default", action.label()),
        }
        self.dirty_frame = true;
    }

    /// The shortcut rows, for the settings sheet's Keybinds page: the
    /// action, the chord as every surface spells it, and whether it is
    /// the player's or the studio's own.
    pub(super) fn keybind_rows(&self) -> Vec<super::super::settings::KeybindRow> {
        std::iter::once(super::super::settings::KeybindRow {
            action: "Terminal",
            chord: super::super::settings::terminal_row_label(&self.ui_settings, &self.features),
            ..Default::default()
        })
        .chain(
            super::super::keybinds::BindAction::ALL
                .into_iter()
                .map(|action| {
                    let learning = self
                        .keybind_learn
                        .is_some_and(|learn| learn.action() == action);
                    let (chord, confirm) = match self.keybind_learn {
                        // The row being asked about wears the chord in hand
                        // and says what answering Enter would do.
                        Some(super::super::keybinds::KeybindLearn::Confirm {
                            action: learning,
                            chord,
                            ..
                        }) if learning == action => (chord.hint(), true),
                        _ if learning => (String::new(), false),
                        _ => (self.keybinds.hint(action), false),
                    };
                    super::super::settings::KeybindRow {
                        action: action.label(),
                        chord,
                        // The F-key alias the studio also answers to, shown
                        // beside the chord while the default stands. Once the
                        // action is overridden or unbound the alias is gone
                        // with the default it belonged to, so the row says
                        // nothing of it.
                        also: self
                            .keybinds
                            .advertised_alias(action)
                            .map(|alias| alias.hint())
                            .unwrap_or_default(),
                        learnt: self.keybinds.overridden(action),
                        learning,
                        confirm,
                    }
                }),
        )
        .chain(std::iter::once(super::super::settings::KeybindRow {
            action: "Reset all shortcuts",
            chord: "Enter".into(),
            confirm: self
                .settings_sheet
                .is_some_and(|sheet| sheet.confirm_reset_keybinds),
            ..Default::default()
        }))
        .collect()
    }
}
