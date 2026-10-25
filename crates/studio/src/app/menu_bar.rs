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
                Some(sheet) if sheet.shows_permissions() => HelpContext::SettingsPermissions,
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
            Focus::Panel(PanelKind::Knobs) => HelpContext::Knobs,
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
            // Vim list keys own j k h l in the menus while the mode is
            // on, so h moves left instead of opening Help. Other letters
            // keep their mnemonics.
            let outcome = if let Some(alias) = self.list_navigation_alias(key) {
                state.key_with_area(&alias, &menus, self.menu_row(), self.frame)
            } else {
                state.key_with_area(key, &menus, self.menu_row(), self.frame)
            };
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
        let alias = self.list_navigation_alias(key);
        if let Some(help) = self.help.as_mut() {
            if key.code == KeyCode::Esc {
                self.help = None;
                #[cfg(feature = "hydra")]
                self.sync_settings_webcam_preview();
                self.dirty_frame = true;
            } else {
                if !help.scroll_key(key.code, self.frame, self.capabilities)
                    && let Some(alias) = alias
                {
                    let _ = help.scroll_key(alias.code, self.frame, self.capabilities);
                }
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
                    // A held letter is the command its press named, or a
                    // list key. It must not become some other row's
                    // mnemonic after the level changes under it.
                    let outcome = if matches!(key.code, KeyCode::Char(_)) {
                        if state.repeats_mnemonic(key) {
                            super::super::menu::MenuKey::Consumed
                        } else {
                            self.list_navigation_alias(key).map_or(
                                super::super::menu::MenuKey::Consumed,
                                |alias| {
                                    state.key_with_area(&alias, &menus, self.menu_row(), self.frame)
                                },
                            )
                        }
                    } else {
                        state.key_with_area(key, &menus, self.menu_row(), self.frame)
                    };
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
        let alias = match terminal_event {
            Event::Key(key) if key.kind == KeyEventKind::Repeat => self.list_navigation_alias(key),
            _ => None,
        };
        if let Some(help) = self.help.as_mut() {
            match terminal_event {
                Event::Key(key) if key.kind == KeyEventKind::Repeat => {
                    if !help.scroll_key(key.code, self.frame, self.capabilities)
                        && let Some(alias) = alias
                    {
                        let _ = help.scroll_key(alias.code, self.frame, self.capabilities);
                    }
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
            has_selection: self
                .focused_file_picker()
                .map(|picker| picker.typing && picker.selection().is_some())
                .unwrap_or_else(|| {
                    editor
                        .selections()
                        .ranges()
                        .iter()
                        .any(|range| !range.is_empty())
                }),
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
            visualizers: self.ui_settings.animation,
            split: self.panes.len() > 1,
            zen: self.ui_settings.zen,
            mixer: self.mixer_panel.is_some(),
            knobs: self.knobs_panel.is_some(),
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
            Do::Visualizers => self.toggle_visualizers(),
            Do::Mixer => self.toggle_mixer_panel(),
            Do::Knobs => self.toggle_knobs_panel(),
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
            Do::Cut => {
                self.dispatch_bind_action(BindAction::Cut, false)?;
            }
            Do::Copy => {
                self.dispatch_bind_action(BindAction::Copy, false)?;
            }
            Do::Paste => {
                self.dispatch_bind_action(BindAction::Paste, false)?;
            }
            Do::SelectAll => {
                self.dispatch_bind_action(BindAction::SelectAll, false)?;
            }
            Do::ToggleComment => self.dispatch_editor(Command::ToggleComment)?,
            Do::Find => {
                self.dispatch_bind_action(BindAction::Find, false)?;
            }
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
            Do::GlobalPrebake => self.open_prebake(PrebakeScope::Global),
            Do::LocalPrebake => self.open_prebake(PrebakeScope::Local),
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

#[cfg(test)]
mod tests {
    mod menu_bar {
        //! Tests for `app/menu_bar.rs`.

        use crate::app::tests::support::*;
        use crate::app::*;

        #[test]
        fn f1_help_preserves_the_focused_surface_and_swallows_covered_input() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.toggle_reference_browse();
            let panel_before = app.reference_panel.clone();
            let score_before = app.editor().source();

            press(&mut app, KeyCode::F(1), KeyModifiers::NONE);
            assert_eq!(app.help, Some(HelpState::default()));
            assert_eq!(app.focus, Focus::Panel(PanelKind::Reference));
            press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
            app.handle_terminal_event(Event::Paste("hidden paste".into()))
                .expect("paste swallowed");
            app.handle_terminal_event(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }))
            .expect("click swallowed");
            assert_eq!(app.editor().source(), score_before);
            assert_eq!(app.reference_panel, panel_before);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Reference));

            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.help.is_none());
            assert_eq!(app.reference_panel, panel_before);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Reference));
        }

        /// A menu cannot go on holding the keyboard once its bar has no row.
        ///
        /// View ▸ Zen is a switch, so it does not close the menu, and zen then
        /// takes the row away. The dropdown's rects are computed from that row,
        /// so an unsettled menu would hit-test an invisible list lying over the
        /// top of the editor: a click near the top would choose a View row that
        /// was never drawn, and F11 would be swallowed rather than restoring the
        /// stage.
        #[test]
        fn a_menu_does_not_outlive_the_bar_it_hangs_from() {
            let directory = tempfile::tempdir().expect("temp dir");

            // Zen from inside the menu.
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 100, 30);
            press(&mut app, KeyCode::F(1), KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('v'), KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('z'), KeyModifiers::NONE);
            assert!(app.ui_settings.zen, "View ▸ Zen did not turn zen on");
            assert!(app.menu_row().is_empty(), "zen still draws a menu row");
            // The next event must find no menu holding the keyboard.
            press(&mut app, KeyCode::F(11), KeyModifiers::NONE);
            assert!(
                !app.ui_settings.zen,
                "F11 was swallowed by a menu with no bar"
            );
            // The chord a Mac terminal delivers when F11 never arrives.
            press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
            assert!(app.ui_settings.zen, "^K did not turn zen on");
            assert!(app.status.starts_with("zen mode"), "{}", app.status);
            press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
            assert!(!app.ui_settings.zen, "^K did not restore the stage");

            // And a resize below the bar's minimum height does the same.
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 100, 30);
            press(&mut app, KeyCode::F(1), KeyModifiers::NONE);
            assert!(app.menu.is_some());
            app.frame = Rect::new(0, 0, 100, 4);
            assert!(
                app.menu_row().is_empty(),
                "a four-row frame drew a menu row"
            );
            press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
            assert!(
                app.menu.is_none(),
                "the menu survived the bar it hangs from"
            );
        }

        /// The menu bar, header and footer switches are the player's lasting
        /// chrome: zen hides them for the session, and leaving zen restores
        /// whatever they said, not the factory defaults.
        #[test]
        fn zen_does_not_rewrite_the_chrome_visibility_preferences() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 100, 30);
            app.ui_settings.show_menu = false;
            app.ui_settings.show_header = false;
            app.ui_settings.show_footer = false;
            assert!(app.menu_row().is_empty(), "the hidden bar still drew a row");

            press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
            assert!(app.ui_settings.zen);
            assert!(!app.ui_settings.show_menu);
            assert!(!app.ui_settings.show_header);
            assert!(!app.ui_settings.show_footer);
            press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
            assert!(!app.ui_settings.zen);
            assert!(!app.ui_settings.show_menu);
            assert!(!app.ui_settings.show_header);
            assert!(!app.ui_settings.show_footer);
            assert!(
                app.menu_row().is_empty(),
                "leaving zen brought the bar back against the preference"
            );

            press(&mut app, KeyCode::F(1), KeyModifiers::NONE);
            assert!(app.menu.is_none(), "F1 opened a bar that is hidden");
            assert!(app.help.is_some(), "F1 did not go on meaning help");
        }

        /// The chrome switches lead the Options menu, ticked while their rows are
        /// drawn; each one flips and remembers its own preference.
        #[test]
        fn options_menu_toggles_remember_the_chrome() {
            use crate::menu::{MenuAction, MenuItemKind};
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 100, 30);
            let options = |app: &App| {
                app.menus()
                    .into_iter()
                    .find(|menu| menu.title == "Options")
                    .expect("Options")
                    .items
            };
            let labels: Vec<String> = options(&app).into_iter().map(|item| item.label).collect();
            assert_eq!(
                labels
                    .iter()
                    .take(3)
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                ["Menu bar", "Header", "Footer"],
                "the chrome switches lead the Options menu: {labels:?}"
            );
            let ticked = |app: &App, id| {
                options(app)
                    .into_iter()
                    .find(|item| item.id() == Some(id))
                    .map(|item| matches!(item.kind, MenuItemKind::Toggle { on: true, .. }))
                    .unwrap_or_else(|| panic!("{id:?} is not on the Options menu"))
            };
            let switches = [
                MenuAction::ShowMenu,
                MenuAction::ShowHeader,
                MenuAction::ShowFooter,
            ];
            for id in switches {
                assert!(ticked(&app, id), "{id:?} starts ticked");
                app.dispatch_menu_action(id).expect("switch");
                assert!(!ticked(&app, id), "{id:?} did not turn off");
            }
            assert!(app.menu_row().is_empty());
            let remembered = app.prefs.ui_settings();
            assert!(!remembered.show_menu && !remembered.show_header && !remembered.show_footer);
        }

        /// With the bar hidden the Settings chord is the way back to the switch,
        /// so the status names the chord the table holds, or the preferences file
        /// when Settings has none.
        #[test]
        fn hiding_the_menu_bar_names_the_settings_chord_the_table_holds() {
            use crate::keybinds::KeyCombo;
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 100, 30);
            app.toggle_show_menu();
            assert!(
                app.status.contains("^O opens settings"),
                "the shipped chord: {}",
                app.status
            );
            app.toggle_show_menu();
            assert_eq!(app.status, "menu bar shown");

            // ^O learnt for zen leaves Settings with no chord.
            app.keybinds
                .learn(BindAction::Zen, Some(KeyCombo::parse("ctrl+o").unwrap()));
            assert!(app.keybinds.binding(BindAction::Settings).is_none());
            app.toggle_show_menu();
            assert!(!app.status.contains("^O"), "{}", app.status);
            assert!(app.status.contains("studio.json"), "{}", app.status);
            app.toggle_show_menu();

            app.keybinds.learn(
                BindAction::Settings,
                Some(KeyCombo::parse("ctrl+shift+f9").unwrap()),
            );
            app.toggle_show_menu();
            let chord = app.keybinds.hint(BindAction::Settings);
            assert!(!chord.is_empty());
            assert!(
                app.status.contains(&format!("{chord} opens settings")),
                "the learnt chord: {}",
                app.status
            );
        }

        /// F1 focuses the bar with nothing dropped, so pressing it twice is
        /// cheap, and the bar keeps the precedence help used to hold: ahead of a
        /// rename, behind the transport.
        #[test]
        fn f1_opens_the_menu_bar_and_it_outranks_everything_but_the_transport() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 100, 30);
            app.begin_rename();
            let rename_before = app.strip_mode.clone();

            press(&mut app, KeyCode::F(1), KeyModifiers::NONE);
            let menu = app.menu.as_ref().expect("F1 did not open the bar");
            assert!(!menu.is_dropped(), "F1 alone should drop nothing");
            assert!(app.help.is_none(), "F1 opened help rather than the bar");

            // The mnemonics are unmodified letters, and a rename swallows every
            // unmodified letter - so the bar has to be asked first or choosing
            // "New scene" types "n" into the scene name.
            press(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
            assert_eq!(
                app.strip_mode, rename_before,
                "the rename ate the menu's mnemonic"
            );
            assert!(
                app.menu
                    .as_ref()
                    .is_some_and(crate::menu::MenuState::is_dropped),
                "s did not drop the Scene menu"
            );

            // The transport is claimed before anything on screen, menu included.
            press(&mut app, KeyCode::F(8), KeyModifiers::NONE);
            assert!(app.menu.is_some(), "stop dismissed the menu");

            press(&mut app, KeyCode::F(1), KeyModifiers::NONE);
            assert!(app.menu.is_none(), "F1 does not close the bar again");
            assert_eq!(app.strip_mode, rename_before);
        }

        /// Where there is no bar to open, F1 goes on meaning help: a key that
        /// does nothing at all is worse than one that does the old thing.
        #[test]
        fn f1_still_opens_help_where_there_is_no_room_for_a_bar() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());

            // Too short for the row.
            app.frame = Rect::new(0, 0, 100, 5);
            press(&mut app, KeyCode::F(1), KeyModifiers::NONE);
            assert!(app.menu.is_none(), "a five-row terminal grew a menu bar");
            assert!(app.help.is_some(), "F1 did nothing at all");
            press(&mut app, KeyCode::F(1), KeyModifiers::NONE);

            // Zen is exactly the source: no chrome, so no bar.
            app.frame = Rect::new(0, 0, 100, 30);
            app.ui_settings.zen = true;
            press(&mut app, KeyCode::F(1), KeyModifiers::NONE);
            assert!(app.menu.is_none(), "zen grew a menu bar");
            assert!(app.help.is_some(), "F1 did nothing at all in zen");
        }

        /// With a dropdown down, a letter that is not a mnemonic must never
        /// reach the score underneath it.
        #[test]
        fn an_open_menu_swallows_letters_instead_of_typing_them() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 100, 30);
            let source = app.editor().source();

            press(&mut app, KeyCode::F(1), KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
            // None of these is a Scene mnemonic, so all four must be eaten.
            for letter in ['j', 'k', 'v', 'z'] {
                press(&mut app, KeyCode::Char(letter), KeyModifiers::NONE);
            }
            assert_eq!(
                app.editor().source(),
                source,
                "an open menu let letters through into the score"
            );
        }

        #[test]
        fn help_opens_ahead_of_rename_and_transport_stays_ahead_of_help() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.begin_rename();
            let rename_before = app.strip_mode.clone();

            press(&mut app, KeyCode::F(1), KeyModifiers::NONE);
            assert!(app.help.is_some(), "rename did not swallow F1");
            assert_eq!(app.help_context(), HelpContext::SceneRename);
            press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
            assert_eq!(app.strip_mode, rename_before, "help swallowed rename input");

            press(&mut app, KeyCode::F(8), KeyModifiers::NONE);
            assert!(app.help.is_some(), "the transport does not dismiss help");
            assert_eq!(app.strip_mode, rename_before, "stop did not reach rename");

            press(&mut app, KeyCode::F(1), KeyModifiers::NONE);
            assert!(app.help.is_none(), "F1 toggles help closed");
            assert_eq!(app.strip_mode, rename_before);
        }

        #[test]
        fn help_scrolls_with_keys_repeats_and_the_mouse_wheel() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 80, 18);
            // On a frame this size F1 opens the menu bar, so help comes up the
            // way the Help menu opens it. This test is about the scrolling.
            app.toggle_help();

            press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            let after_down = app.help.expect("help").scroll;
            assert_eq!(after_down, 1);
            app.handle_terminal_event(Event::Key(KeyEvent::new_with_kind(
                KeyCode::PageDown,
                KeyModifiers::NONE,
                KeyEventKind::Repeat,
            )))
            .expect("repeat handled");
            let after_repeat = app.help.expect("help").scroll;
            assert!(after_repeat > after_down);
            app.handle_terminal_event(Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 1,
                row: 1,
                modifiers: KeyModifiers::NONE,
            }))
            .expect("wheel handled");
            assert!(app.help.expect("help").scroll >= after_repeat);
            press(&mut app, KeyCode::Home, KeyModifiers::NONE);
            assert_eq!(app.help.expect("help").scroll, 0);
            press(&mut app, KeyCode::End, KeyModifiers::NONE);
            assert!(app.help.expect("help").scroll > 0);
        }

        #[test]
        fn theme_editor_help_tracks_nested_prompts_without_changing_them() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let theme = app.theme.clone();
            app.open_theme_editor(&theme, "mine".into(), false);
            assert_eq!(app.help_context(), HelpContext::ThemeEditorForm);

            app.theme_editor.as_mut().expect("editor").saving =
                Some(crate::theme_editor::SaveSheet {
                    name: "half-typed".into(),
                    keep: true,
                    note: None,
                    closing: false,
                });
            assert_eq!(app.help_context(), HelpContext::ThemeEditorSave);
            press(&mut app, KeyCode::F(1), KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('z'), KeyModifiers::NONE);
            assert_eq!(
                app.theme_editor
                    .as_ref()
                    .and_then(|editor| editor.saving.as_ref())
                    .map(|saving| (saving.name.as_str(), saving.keep)),
                Some(("half-typed", true))
            );
        }

        #[test]
        fn help_context_distinguishes_every_studio_surface() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            assert_eq!(app.help_context(), HelpContext::Editor);

            for (panel, context) in [
                (PanelKind::Theme, HelpContext::ThemePicker),
                (PanelKind::Reference, HelpContext::Reference),
                (PanelKind::Settings, HelpContext::Settings),
                (PanelKind::Log, HelpContext::Log),
                (PanelKind::Export, HelpContext::Export),
                (PanelKind::Devices, HelpContext::Devices),
                (PanelKind::Set, HelpContext::Set),
                (PanelKind::Viz, HelpContext::Viz),
            ] {
                app.focus = Focus::Panel(panel);
                assert_eq!(app.help_context(), context, "{panel:?}");
            }

            app.strip_mode = SceneStripMode::Learning;
            assert_eq!(app.help_context(), HelpContext::SceneLearn);
            app.strip_mode = SceneStripMode::Idle;
            app.focus = Focus::Panel(PanelKind::Reference);
            app.armed_slider = Some((app.scenes.current().id, 0));
            assert_eq!(
                app.help_context(),
                HelpContext::Slider,
                "an armed slider takes arrows before the focused surface"
            );
        }

        /// A menu row opens a panel; it never closes one.
        ///
        /// Picking "Reference" from a menu while the reference is open is an
        /// instruction to have the reference. Closing it reads as the menu
        /// ignoring the click. The chord keeps its toggle, where pressing the same
        /// keys again is how a thing is put away.
        #[test]
        fn a_menu_row_opens_a_panel_and_never_closes_it() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);

            // Twice through the menu leaves it open both times.
            for _ in 0..2 {
                press(&mut app, KeyCode::F(1), KeyModifiers::NONE);
                press(&mut app, KeyCode::Char('v'), KeyModifiers::NONE);
                press(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);
                assert!(
                    app.reference_panel.is_some(),
                    "a menu row closed the panel it names"
                );
            }

            // The chord still toggles.
            press(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
            assert!(app.reference_panel.is_none(), "the chord stopped toggling");
        }
    }
}
