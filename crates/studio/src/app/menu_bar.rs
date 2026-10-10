//! The top menu bar and the help overlay. The bar's menus are rebuilt here
//! from live studio state, the pointer opens, hovers and chooses in them, and
//! each menu row's action runs through the same command its keyboard chord
//! uses. This file also opens and closes the help overlay and decides which
//! help page to show for whatever currently has the keyboard.

use super::*;

impl App {
    /// What will receive the next non-help key once the overlay closes.
    /// Reading this state must not settle or otherwise alter it: help is a
    /// transparent pause over a potentially half-completed interaction.
    pub(super) fn help_context(&self) -> HelpContext {
        match &self.strip_mode {
            SceneStripMode::Renaming(_) => return HelpContext::SceneRename,
            SceneStripMode::Learning => return HelpContext::SceneLearn,
            SceneStripMode::Idle => {}
        }
        if self.armed_slider.is_some() {
            return HelpContext::Slider;
        }
        match self.focus {
            Focus::Panel(PanelKind::Theme) => HelpContext::ThemePicker,
            Focus::Panel(PanelKind::ThemeEditor) => {
                let Some(editor) = self.theme_editor.as_ref() else {
                    return HelpContext::Editor;
                };
                if editor.saving.is_some() {
                    HelpContext::ThemeEditorSave
                } else if editor.picker.is_some() {
                    HelpContext::ThemeEditorColor
                } else if editor.entry.is_some() {
                    HelpContext::ThemeEditorEntry
                } else {
                    match editor.tab {
                        super::super::theme_editor::EditorTab::Form => HelpContext::ThemeEditorForm,
                        super::super::theme_editor::EditorTab::Code => HelpContext::ThemeEditorCode,
                    }
                }
            }
            Focus::Panel(PanelKind::Reference) => HelpContext::Reference,
            Focus::Panel(PanelKind::Settings) => match self.settings_sheet {
                Some(sheet) if sheet.shows_sources() => HelpContext::SettingsSources,
                #[cfg(feature = "vst")]
                Some(sheet) if sheet.shows_vst() => HelpContext::SettingsVst,
                Some(sheet) if !sheet.shows_settings() => HelpContext::SettingsAbout,
                _ => HelpContext::Settings,
            },
            Focus::Panel(PanelKind::Log) => HelpContext::Log,
            Focus::Panel(PanelKind::Memory) => HelpContext::Memory,
            Focus::Panel(PanelKind::Jobs) => HelpContext::Jobs,
            Focus::Panel(PanelKind::Export) => HelpContext::Export,
            Focus::Panel(PanelKind::Devices) => HelpContext::Devices,
            Focus::Panel(PanelKind::Set) => HelpContext::Set,
            Focus::Panel(PanelKind::Viz) => HelpContext::Viz,
            Focus::Panel(PanelKind::Mixer) => HelpContext::Mixer,
            Focus::Editor => HelpContext::Editor,
            Focus::Timeline => HelpContext::Timeline,
        }
    }

    pub(super) fn toggle_help(&mut self) {
        self.help = self.help.take().is_none().then(HelpState::default);
        #[cfg(feature = "hydra")]
        self.sync_settings_webcam_preview();
        self.dirty_frame = true;
    }

    /// A menu cannot outlive the bar it hangs from.
    ///
    /// View ▸ Zen is a switch, so choosing it does not close the menu - and
    /// `toggle_zen` then takes the bar's row away, as does a resize below
    /// `MENU_MIN_HEIGHT`. Painting already honours that, but the dropdown's
    /// rects are computed from that row: with it empty they collapse onto
    /// `(0, 1)`, so a menu still holding the keyboard would hit-test an
    /// invisible list lying over the top of the editor, and letters would
    /// choose rows nobody could see.
    pub(super) fn settle_menu(&mut self) {
        if self.menu.is_some() && self.menu_row().is_empty() {
            self.menu = None;
            self.dirty_frame = true;
        }
    }

    /// The menu or help chord, as the table resolves it, toggles its
    /// surface and puts the precision slider and the block editor away.
    /// `true` when the press was one of them.
    pub(super) fn menu_or_help_chord(
        &mut self,
        key: &KeyEvent,
        alt: bool,
    ) -> Result<bool, RuntimeError> {
        if !alt
            && let Some(action @ (BindAction::MenuBar | BindAction::Help)) =
                self.keybinds.action_for(key)
        {
            self.slider_precision = None;
            self.replay_edit = None;
            self.dispatch_bind_action(action, false)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// A press while a menu has the keyboard: its mnemonics and arrows
    /// work the menu. `true` when the menu took it.
    pub(super) fn menu_press(&mut self, key: &KeyEvent) -> Result<bool, RuntimeError> {
        if self.menu.is_some() {
            let menus = self.menus();
            let mut state = self.menu.take().expect("menu is some");
            let outcome = state.key_with_area(key, &menus, self.menu_row(), self.frame);
            if matches!(outcome, super::super::menu::MenuKey::Ignored) {
                // A modified key is nobody's mnemonic; let the chord
                // table have it, with the menu still down.
                self.menu = Some(state);
            } else {
                state.follow_scroll(&menus, self.menu_row(), self.frame, true);
                self.menu = Some(state);
                if let super::super::menu::MenuKey::Events(events) = outcome {
                    self.apply_menu_events(events)?;
                }
                self.dirty_frame = true;
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// A press while help is open, which takes it whatever the key.
    /// `true` while help is open.
    pub(super) fn help_press(&mut self, key: &KeyEvent) -> bool {
        // Help is modal only for input, not for application state. Esc
        // closes it; navigation scrolls its lower list; everything else
        // is swallowed so it cannot act on the preserved surface below.
        if let Some(help) = self.help.as_mut() {
            if key.code == KeyCode::Esc {
                self.help = None;
                #[cfg(feature = "hydra")]
                self.sync_settings_webcam_preview();
                self.dirty_frame = true;
            } else {
                let _ = help.scroll_key(key.code, self.frame, self.capabilities);
                self.dirty_frame = true;
            }
            return true;
        }
        false
    }

    /// An open menu's share of what the press ladder never sees: a held
    /// key walks it, and a paste is dropped. `true` when the menu took the
    /// event.
    pub(super) fn menu_event(&mut self, terminal_event: &Event) -> Result<bool, RuntimeError> {
        if self.menu.is_some() {
            match terminal_event {
                Event::Key(key) if key.kind == KeyEventKind::Repeat => {
                    let menus = self.menus();
                    let mut state = self.menu.take().expect("menu is some");
                    let outcome = state.key_with_area(key, &menus, self.menu_row(), self.frame);
                    state.follow_scroll(&menus, self.menu_row(), self.frame, true);
                    self.menu = Some(state);
                    if let super::super::menu::MenuKey::Events(events) = outcome {
                        self.apply_menu_events(events)?;
                    }
                    self.dirty_frame = true;
                    return Ok(true);
                }
                Event::Paste(_) => return Ok(true),
                _ => {}
            }
        }
        Ok(false)
    }

    /// Everything past the press ladder while help is open - held keys,
    /// the wheel, clicks and pastes - belongs to the overlay. `true` while
    /// help is open.
    pub(super) fn help_event(&mut self, terminal_event: &Event) -> bool {
        if let Some(help) = self.help.as_mut() {
            match terminal_event {
                Event::Key(key) if key.kind == KeyEventKind::Repeat => {
                    let _ = help.scroll_key(key.code, self.frame, self.capabilities);
                    self.dirty_frame = true;
                }
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollRight => {
                        help.wheel(true, self.frame, self.capabilities);
                        self.dirty_frame = true;
                    }
                    MouseEventKind::ScrollDown | MouseEventKind::ScrollLeft => {
                        help.wheel(false, self.frame, self.capabilities);
                        self.dirty_frame = true;
                    }
                    _ => {}
                },
                _ => {}
            }
            return true;
        }
        false
    }

    /// The bar's row, computed from the frame rather than read off
    /// `self.regions` - that field is only refreshed inside a draw, so a
    /// gate against it means "no menu" before the first frame and in every
    /// test that never draws.
    pub(super) fn menu_row(&self) -> Rect {
        super::super::view::menu_row(
            self.frame,
            super::super::view::ChromeLayout::from_settings(&self.ui_settings),
        )
    }

    /// The open menu's dropdowns, hung from the row the bar was drawn on.
    pub(super) fn paint_menu_dropdown(
        &self,
        frame: &mut ratatui::Frame<'_>,
        menu_state: Option<&super::super::menu::MenuState>,
        menu_row: Rect,
        menus: &[super::super::menu::Menu],
    ) {
        if let Some(state) = menu_state
            && state.is_dropped()
            && !menu_row.is_empty()
        {
            let screen = frame.area();
            super::super::menu::render_dropdowns(
                frame.buffer_mut(),
                screen,
                menu_row,
                menus,
                state,
                &self.theme,
            );
        }
    }

    /// The bar's menus, rebuilt from live state.
    ///
    /// Never cached: a menu built from a snapshot goes stale the moment
    /// anything else changes what it shows, and a greyed row or a tick that
    /// lies teaches the wrong thing about the studio.
    pub(super) fn menus(&self) -> Vec<super::super::menu::Menu> {
        let editor = self.editor();
        super::super::menu::menus(&super::super::menu::MenuContext {
            capabilities: self.capabilities,
            keybinds: &self.keybinds,
            can_undo: editor.can_undo(),
            can_redo: editor.can_redo(),
            // Mirror the editor's own guard, which joins every non-empty
            // range rather than looking only at the primary caret.
            has_selection: editor
                .selections()
                .ranges()
                .iter()
                .any(|range| !range.is_empty()),
            can_smart_action: self.smart_action_offers(),
            scene_rewinds: self.scenes.current_rewinds(),
            can_add_scene: self.scenes.score_count() < super::super::scenes::MAX_SCENES,
            on_prebake: self.current_prebake().is_some(),
            can_delete_scene: !(self.scenes.current().is_score() && self.scenes.score_count() <= 1),
            can_step_scene: self.scenes.len() > self.panes.len() && self.replay_view.is_none(),
            can_learn_pad: !self.devices.inventory().midi_inputs.is_empty()
                || self.pads.open_count() > 0,
            has_pad: self.scenes.current().pad.is_some(),
            has_last_file: self.last_file.is_some(),
            recording: self.take_chip().is_some(),
            recording_sample: self.sample_take.is_some(),
            recording_tape: self.recorder.is_some(),
            can_record_session: self.options.recording.is_some(),
            set_panel: self.set_panel.is_some(),
            viz_panels: [self.viz_docks[0].is_some(), self.viz_docks[1].is_some()],
            split: self.panes.len() > 1,
            zen: self.ui_settings.zen,
            mixer: self.mixer_panel.is_some(),
            log: self.log_panel.is_some(),
            memory: self.memory_dock.is_some(),
            jobs: self.jobs_panel.is_some(),
            line_numbers: self.ui_settings.line_numbers,
            wrap: self.ui_settings.wrap,
            show_menu: self.ui_settings.show_menu,
            show_header: self.ui_settings.show_header,
            show_footer: self.ui_settings.show_footer,
            at_max_gain: self.master.gain_db() >= super::super::meter::MAX_GAIN_DB,
            at_min_gain: self.master.gain_db() <= super::super::meter::MIN_GAIN_DB,
            set_limiter: self.has_limiter_slot(),
            // Both of these open an overlay that draws nothing at all below
            // its minimum size while still holding the keyboard - a black
            // hole. The row says so instead.
            help_fits: super::super::help::HelpView::geometry(self.frame).is_some(),
            settings_fits: super::super::settings::SettingsSheetView::geometry(
                self.settings_sheet_frame(),
            )
            .is_some(),
            remote_control_available: {
                #[cfg(feature = "remote-control")]
                {
                    self.remote_panel_available()
                }
                #[cfg(not(feature = "remote-control"))]
                {
                    false
                }
            },
        })
    }

    /// The pointer on the menu bar, or anywhere while a menu is down: a
    /// press opens a menu, chooses a row or dismisses it, a move hovers,
    /// and while a menu is down every other event is the menu's too.
    /// Returns true when the menu took the event.
    pub(super) fn menu_mouse(
        &mut self,
        mouse: MouseEvent,
        x: u16,
        y: u16,
    ) -> Result<bool, RuntimeError> {
        if self.menu.is_some() || within(self.menu_row(), x, y) {
            let menus = self.menus();
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    self.slider_precision = None;
                    let mut state = self
                        .menu
                        .take()
                        .unwrap_or_else(super::super::menu::MenuState::opened);
                    let outcome = state.click(&menus, self.menu_row(), self.frame, x, y);
                    state.follow_scroll(&menus, self.menu_row(), self.frame, false);
                    self.menu = Some(state);
                    self.pointer = Some(Pointer::Panel);
                    if let super::super::menu::MenuKey::Events(events) = outcome {
                        self.apply_menu_events(events)?;
                    }
                    self.dirty_frame = true;
                    return Ok(true);
                }
                MouseEventKind::Moved if self.menu.is_some() => {
                    let mut state = self.menu.take().expect("menu is some");
                    let outcome = state.hover(&menus, self.menu_row(), self.frame, x, y);
                    state.follow_scroll(&menus, self.menu_row(), self.frame, false);
                    self.menu = Some(state);
                    if let super::super::menu::MenuKey::Events(events) = outcome {
                        self.apply_menu_events(events)?;
                    }
                    return Ok(true);
                }
                // Drags, releases and the wheel are the menu's while it is
                // down, or they drive the master fader under the dropdown.
                _ if self.menu.is_some() => return Ok(true),
                _ => {}
            }
        }
        Ok(false)
    }

    /// Act on what the widget reported, in order.
    pub(super) fn apply_menu_events(
        &mut self,
        events: Vec<super::super::menu::MenuEvent>,
    ) -> Result<(), RuntimeError> {
        use super::super::menu::MenuEvent as Event;
        // A command runs with the bar already gone. Several of them are
        // written for a keyboard nothing else is holding - a rename takes
        // every subsequent key, and record, export and the master fader all
        // sit after `settle_focus` on the chord path, which must see the
        // surface underneath rather than the menu.
        if events.iter().any(|event| matches!(event, Event::Close(_))) {
            self.menu = None;
            self.settle_focus();
            #[cfg(feature = "hydra")]
            self.sync_settings_webcam_preview();
        }
        for event in events {
            match event {
                Event::Activate(action) => self.dispatch_menu_action(action)?,
                // Nothing in these menus steps a value yet. The variant is
                // carried because the settings rows that do - three opacity
                // steppers and two port cycles - cannot be bolted onto an
                // activate-only widget afterwards.
                Event::Adjust(..) => {}
                // Likewise the preview lifecycle: no list here changes the
                // studio merely by being highlighted. The theme list does,
                // and that is the whole reason the events exist now.
                Event::Highlight(_) | Event::PreviewBegin(_) | Event::PreviewEnd { .. } => {}
                Event::Close(_) => {}
            }
        }
        self.dirty_frame = true;
        Ok(())
    }

    /// Do what a menu row asked for.
    ///
    /// Every arm is the same call the chord already makes, so a command has
    /// one implementation and the menu is a second door to it rather than a
    /// second copy of it.
    pub(super) fn dispatch_menu_action(
        &mut self,
        action: super::super::menu::MenuAction,
    ) -> Result<(), RuntimeError> {
        use super::super::menu::MenuAction as Do;
        match action {
            Do::NewScene => {
                self.new_scene(false);
            }
            Do::DuplicateScene => {
                self.new_scene(true);
            }
            Do::RenameScene => {
                self.begin_rename();
            }
            Do::SetPanel => self.toggle_set_panel(),
            Do::PianoMode => self.toggle_piano_mode(),
            Do::VizPanel => self.toggle_viz_dock(0),
            Do::VizPanelTwo => self.toggle_viz_dock(1),
            Do::Mixer => self.toggle_mixer_panel(),
            Do::SetLimiter => self.set_limiter_slot(!self.has_limiter_slot()),
            Do::NewSet => self.new_set(),
            Do::NewSession => self.new_session(),
            Do::OpenSet => self.open_set_prompt(SetPrompt::OpenSet),
            Do::OpenRecent => self.open_set_prompt(SetPrompt::OpenRecent),
            Do::RenameSet => self.open_set_prompt(SetPrompt::RenameSet),
            Do::ConsolidateSamples => self.consolidate_samples(),
            Do::DeleteScene => self.arm_delete_scene(),
            Do::CloseScene => {
                self.close_scene();
            }
            Do::PreviousScene => {
                self.step_scene(-1);
            }
            Do::NextScene => {
                self.step_scene(1);
            }
            Do::LearnPad => {
                self.begin_learn();
            }
            Do::ForgetPad => self.forget_pad(),
            Do::ShowLastFile => self.reveal_last_file(),
            Do::ShowSet => self.reveal_set_folder(),
            Do::Quit => {
                self.quit_now();
            }
            Do::Undo => self.dispatch_editor(Command::Undo)?,
            Do::Redo => self.dispatch_editor(Command::Redo)?,
            Do::Cut => self.dispatch_editor(Command::Cut)?,
            Do::Copy => self.dispatch_editor(Command::Copy)?,
            Do::Paste => self.dispatch_editor(Command::Paste)?,
            Do::SelectAll => self.dispatch_editor(Command::SelectAll)?,
            Do::ToggleComment => self.dispatch_editor(Command::ToggleComment)?,
            Do::FirstError => self.jump_to_first_error(),
            Do::SmartAction => self.open_smart_action(),
            Do::Update => self.dispatch_editor(Command::Evaluate)?,
            Do::RewindUpdate => {
                self.next_rewind = true;
                self.dispatch_editor(Command::Evaluate)?;
            }
            Do::SceneRewind => self.toggle_scene_rewind(),
            Do::Stop => self.dispatch_editor(Command::Stop)?,
            Do::Record => {
                self.toggle_take();
            }
            Do::RecordSample => self.toggle_sample(),
            Do::Export => {
                if self.export_sheet.is_none() {
                    self.toggle_export_sheet();
                } else {
                    self.focus_panel(PanelKind::Export);
                }
            }
            Do::MasterUp => self.nudge_master_gain_db(VOLUME_KEY_STEP_DB),
            Do::MasterDown => self.nudge_master_gain_db(-VOLUME_KEY_STEP_DB),
            // A menu row OPENS. It does not toggle: picking "Reference" from a
            // menu while the reference is open is an instruction to have the
            // reference, and closing it instead reads as the menu ignoring the
            // click. Toggling belongs to the chord, where pressing the same
            // keys again is the natural way to put a thing away.
            Do::Devices => {
                if self.panel.is_none() {
                    self.toggle_device_panel();
                } else {
                    self.focus_panel(PanelKind::Devices);
                }
            }
            Do::RemoteControl => {
                #[cfg(feature = "remote-control")]
                self.open_remote_panel();
            }
            Do::Reference => {
                if self.reference_panel.is_none() {
                    self.toggle_reference_browse();
                } else {
                    self.focus_panel(PanelKind::Reference);
                }
            }
            Do::Docs => {
                if self.reference_panel.is_none() {
                    self.toggle_reference_at_caret();
                } else {
                    self.focus_panel(PanelKind::Reference);
                }
            }
            Do::Split => self.toggle_split(),
            Do::HopPane => self.hop_pane(),
            Do::SwitchPanel => self.rotate_panel_focus(),
            // The menu row is drawn with a tick, so it turns off as readily
            // as it turns on - the mixer's row beside it always has. F9
            // keeps its own reading, where an open sheet that has not got
            // the keyboard is brought forward rather than shut.
            Do::Log => {
                if self.log_panel.is_some() {
                    self.close_panel(PanelKind::Log);
                    self.settle_focus();
                    self.dirty_frame = true;
                } else {
                    self.toggle_log_panel();
                }
            }
            Do::Jobs => {
                if self.jobs_panel.is_some() {
                    self.close_panel(PanelKind::Jobs);
                    self.settle_focus();
                    self.dirty_frame = true;
                } else {
                    self.toggle_jobs_panel();
                }
            }
            Do::Memory => self.open_log_from_memory(),
            Do::Zen => self.toggle_zen(),
            Do::LineNumbers => self.toggle_line_numbers(),
            Do::Wrap => self.toggle_wrap(),
            Do::ShowMenu => self.toggle_show_menu(),
            Do::ShowHeader => self.toggle_show_header(),
            Do::ShowFooter => self.toggle_show_footer(),
            Do::ThemePicker => {
                // The picker would otherwise come up UNDER the theme
                // editor's sheet, invisible but holding the keyboard.
                if self.theme_editor.is_some() {
                    self.status =
                        "the theme editor is open - s saves, Esc goes back to the themes".into();
                    self.dirty_frame = true;
                    return Ok(());
                }
                if self.theme_picker.is_none() {
                    self.toggle_theme_picker();
                } else {
                    self.focus_panel(PanelKind::Theme);
                }
            }
            Do::Settings => {
                if self.settings_sheet.is_none() {
                    self.toggle_settings_sheet();
                } else {
                    self.focus_panel(PanelKind::Settings);
                }
            }
            Do::KeyboardReference => {
                if self.help.is_none() {
                    self.toggle_help();
                }
            }
            Do::About => {
                if self.settings_sheet.is_none() {
                    self.toggle_settings_sheet();
                }
                if self.settings_sheet.is_some() {
                    self.focus_panel(PanelKind::Settings);
                    if let Some(sheet) = self.settings_sheet.as_mut() {
                        sheet.show_page(super::super::settings::SettingsPage::About);
                    }
                    #[cfg(feature = "hydra")]
                    self.sync_settings_webcam_preview();
                    self.dirty_frame = true;
                }
            }
        }
        Ok(())
    }
}
