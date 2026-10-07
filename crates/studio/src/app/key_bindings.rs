//! The chord table's side of key handling: looking up which configurable
//! action a key press is bound to (learnt, adapted or retired defaults),
//! running that action through `dispatch_bind_action`, and turning learnt
//! chords into editor commands. It also holds `shortcut_or_menu`, which status
//! messages use to show an action's current key, or its menu path when the
//! action has no key.

use super::*;

impl App {
    /// What this press means to the chord table, as an editor command, for
    /// the chord reader: a learnt chord whose action the editor can carry
    /// dispatches as that command, and everything else resolves to nothing
    /// so the reader's built-in chords stand. The overrides are the change -
    /// the table's defaults are the studio's own chords, which the reader
    /// already knows - so only learnt presses answer here.
    pub(super) fn editor_command_for(&self, key: &crossterm::event::KeyEvent) -> Option<Command> {
        use super::super::keybinds::BindAction as Do;
        let action = self.bind_action_for(key)?;
        match action {
            Do::Undo => Some(Command::Undo),
            Do::Redo => Some(Command::Redo),
            Do::Copy => Some(Command::Copy),
            Do::Cut => Some(Command::Cut),
            Do::Paste => Some(Command::Paste),
            Do::SelectAll => Some(Command::SelectAll),
            Do::ToggleComment => Some(Command::ToggleComment),
            Do::Evaluate => Some(Command::Evaluate),
            Do::Stop => Some(Command::Stop),
            _ => None,
        }
    }

    /// Do what a bound key asked for.
    ///
    /// The chord table's door into the same implementations the chords and
    /// the menu share. `Ok(true)` - the press was claimed, stop routing it;
    /// `Ok(false)` - this table does not answer for the key here, let the
    /// chord table have it. Only the actions the table actually advertises
    /// fire here, and each one says so against the chord it was learnt
    /// with, so an old default that has been moved does not double-fire
    /// behind the legacy checks `route_key_press` goes on to make.
    pub(super) fn dispatch_bind_action(
        &mut self,
        action: super::super::keybinds::BindAction,
        alt: bool,
    ) -> Result<bool, RuntimeError> {
        use super::super::keybinds::BindAction as Do;
        if alt {
            return Ok(false);
        }
        // The chords that would open something beside the sheet and take
        // the keyboard with them, swallowed rather than obeyed while the
        // sheet holds it. Obeyed, they would strand the sheet: it would
        // stay on screen, full of controls that answer nothing, with no
        // way back, because it is a dialog rather than a stop on the
        // walk.
        //
        // A sheet that replaces the settings is a different thing and
        // stays allowed - the theme picker, the export sheet, the devices
        // panel, the log and the jobs list all call `dismiss_dialogs`, so
        // the settings are gone rather than orphaned and one dialog has
        // taken another's place, which is the rule everywhere else in the
        // studio.
        //
        // The transport, the menu and Esc are deliberately absent too:
        // stopping the music must work from anywhere, and Esc is the way
        // out.
        if self.settings_hold_the_keys()
            && matches!(
                action,
                // The reference is not one of the sheets that displaces
                // the settings: `toggle_reference_browse` opens its panel
                // and takes the keyboard without calling `dismiss_dialogs`
                // on any of its paths, so letting it through would leave
                // the settings standing with nothing answering them and
                // no route back. The docs open the same panel.
                Do::Reference
                    | Do::Docs
                    | Do::SetPanel
                    | Do::Mixer
                    | Do::VisualsOne
                    | Do::VisualsTwo
                    | Do::LogSticky
                    | Do::Split
                    | Do::HopPane
            )
        {
            self.status = "the settings have the keyboard - Esc closes them".into();
            self.dirty_frame = true;
            return Ok(true);
        }
        match action {
            Do::PianoMode => {
                self.toggle_piano_mode();
                Ok(true)
            }
            Do::RewindEvaluate => {
                self.next_rewind = true;
                self.dispatch_editor(Command::Evaluate)?;
                Ok(true)
            }
            Do::Evaluate => {
                self.dispatch_editor(Command::Evaluate)?;
                Ok(true)
            }
            Do::Stop => {
                self.dispatch_editor(Command::Stop)?;
                Ok(true)
            }
            Do::MenuBar => {
                // The same decision the F1 press makes: help closes on its
                // own key, and where there is no row for a bar - zen, a
                // hidden menu bar, or a terminal too short to spare it -
                // the chord goes on meaning help.
                if self.help.is_some() || self.menu_row().is_empty() {
                    self.toggle_help();
                } else {
                    self.menu = if self.menu.is_some() {
                        None
                    } else {
                        Some(super::super::menu::MenuState::opened())
                    };
                    self.dirty_frame = true;
                }
                Ok(true)
            }
            Do::Help => {
                self.toggle_help();
                Ok(true)
            }
            Do::Settings => {
                self.toggle_settings_sheet();
                Ok(true)
            }
            Do::Devices => {
                self.toggle_device_panel();
                Ok(true)
            }
            Do::ThemePicker => {
                self.toggle_theme_picker();
                Ok(true)
            }
            Do::Reference => {
                self.toggle_reference_browse();
                Ok(true)
            }
            Do::Docs => {
                self.toggle_reference_at_caret();
                Ok(true)
            }
            Do::SetPanel => {
                self.toggle_set_panel();
                Ok(true)
            }
            Do::Mixer => {
                self.toggle_mixer_panel();
                Ok(true)
            }
            Do::VisualsOne => {
                self.toggle_viz_dock(0);
                Ok(true)
            }
            Do::VisualsTwo => {
                self.toggle_viz_dock(1);
                Ok(true)
            }
            Do::Log => {
                self.toggle_log_panel();
                Ok(true)
            }
            Do::LogSticky => {
                self.toggle_log_sticky();
                Ok(true)
            }
            Do::FocusPanels => {
                self.rotate_panel_focus();
                Ok(true)
            }
            Do::Jobs => {
                self.toggle_jobs_panel();
                Ok(true)
            }
            Do::Memory => {
                self.open_log_from_memory();
                Ok(true)
            }
            Do::Export => {
                self.toggle_export_sheet();
                Ok(true)
            }
            Do::SmartAction => {
                self.open_smart_action();
                Ok(true)
            }
            Do::Split => {
                self.toggle_split();
                Ok(true)
            }
            Do::HopPane => {
                self.hop_pane();
                Ok(true)
            }
            Do::Undo => {
                self.dispatch_editor(Command::Undo)?;
                Ok(true)
            }
            Do::Redo => {
                self.dispatch_editor(Command::Redo)?;
                Ok(true)
            }
            Do::Copy => {
                self.dispatch_editor(Command::Copy)?;
                Ok(true)
            }
            Do::Cut => {
                self.dispatch_editor(Command::Cut)?;
                Ok(true)
            }
            Do::Paste => {
                self.dispatch_editor(Command::Paste)?;
                Ok(true)
            }
            Do::SelectAll => {
                self.dispatch_editor(Command::SelectAll)?;
                Ok(true)
            }
            Do::FirstError => {
                self.jump_to_first_error();
                Ok(true)
            }
            Do::ToggleComment => {
                self.dispatch_editor(Command::ToggleComment)?;
                Ok(true)
            }
            Do::RecordTake => {
                self.toggle_take();
                Ok(true)
            }
            Do::RecordSample => {
                self.toggle_sample();
                Ok(true)
            }
            Do::DuplicateScene => {
                self.new_scene(true);
                Ok(true)
            }
            Do::NewScene => {
                self.new_scene(false);
                Ok(true)
            }
            Do::RenameScene => {
                self.begin_rename();
                Ok(true)
            }
            Do::CloseScene => {
                self.close_scene();
                Ok(true)
            }
            Do::LearnPad => {
                self.begin_learn();
                Ok(true)
            }
            Do::ForgetPad => {
                self.forget_pad();
                Ok(true)
            }
            Do::SceneRewind => {
                self.toggle_scene_rewind();
                Ok(true)
            }
            Do::PreviousScene => {
                self.step_scene(-1);
                Ok(true)
            }
            Do::NextScene => {
                self.step_scene(1);
                Ok(true)
            }
            Do::Wrap => {
                self.toggle_wrap();
                Ok(true)
            }
            Do::Zen => {
                self.toggle_zen();
                Ok(true)
            }
            Do::MasterUp => {
                self.nudge_master_gain_db(VOLUME_KEY_STEP_DB);
                Ok(true)
            }
            Do::MasterDown => {
                self.nudge_master_gain_db(-VOLUME_KEY_STEP_DB);
                Ok(true)
            }
            Do::OpenSet => {
                self.open_set_prompt(SetPrompt::OpenSet);
                Ok(true)
            }
            Do::Quit => {
                self.request_quit();
                Ok(true)
            }
        }
    }

    /// Editing commands belong to whichever text surface owns the keyboard.
    pub(super) fn contextual_editor_binding(action: BindAction) -> bool {
        matches!(
            action,
            BindAction::Undo
                | BindAction::Redo
                | BindAction::Copy
                | BindAction::Cut
                | BindAction::Paste
                | BindAction::SelectAll
                | BindAction::ToggleComment
        )
    }

    /// Retired editing chords must not keep their old meaning inside panels.
    pub(super) fn retired_editor_binding(&self, key: &KeyEvent) -> bool {
        self.keybinds.action_for(key).is_none()
            && self.keybinds.overrules(key)
            && BindAction::ALL
                .into_iter()
                .filter(|action| Self::contextual_editor_binding(*action))
                .any(|action| {
                    std::iter::once(action.default_binding())
                        .chain(action.fallback_bindings())
                        .chain(action.default_alias())
                        .any(|combo| combo.matches(key))
                })
    }

    /// A panel's close alias is the same configurable action as opening it.
    /// Contextual commands such as deleting a set row keep their own keys.
    pub(super) fn retired_panel_toggle(&self, key: &KeyEvent) -> bool {
        let action = match self.focus {
            Focus::Panel(PanelKind::Log) => BindAction::Log,
            Focus::Panel(PanelKind::Devices) => BindAction::Devices,
            Focus::Panel(PanelKind::Export) => BindAction::Export,
            Focus::Panel(PanelKind::Theme) => BindAction::ThemePicker,
            _ => return false,
        };
        self.keybinds.action_for(key).is_none()
            && self.keybinds.overrules(key)
            && std::iter::once(action.default_binding())
                .chain(action.fallback_bindings())
                .chain(action.default_alias())
                .any(|combo| combo.matches(key))
    }

    /// An explicit binding or an automatically substituted terminal default.
    /// Untouched defaults retain their contextual handling in
    /// `route_key_press` (for example, a prompt keeps Ctrl+A). Adapted
    /// chords use the same action dispatcher as learned bindings, so the
    /// advertised fallback actually runs.
    pub(super) fn bind_action_for(
        &self,
        key: &crossterm::event::KeyEvent,
    ) -> Option<super::super::keybinds::BindAction> {
        self.keybinds.adapted_action_for(key)
    }

    /// Whether the key now in hand belongs to the table's own
    /// arrangements: somebody's learnt chord, or the default chord of an
    /// action that has been learnt elsewhere. The capture path asks it
    /// before accepting a chord - a key an action already answers to is
    /// taken for the action being learnt (the chord moves, one chord one
    /// meaning) but never swallowed as a claim on itself - and the legacy
    /// chord checks ask it so a key the table has freed does not answer
    /// to its old meaning behind the table's back.
    pub(super) fn key_is_a_learnt_chord(&self, key: &crossterm::event::KeyEvent) -> bool {
        self.keybinds.overrules(key)
    }

    /// A status message still offers a route when its action is unbound.
    pub(super) fn shortcut_or_menu(&self, action: BindAction, menu: &str) -> String {
        self.keybinds
            .binding(action)
            .map_or_else(|| menu.to_owned(), |key| key.hint())
    }
}
