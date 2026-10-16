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
            // A panel reads its own action as an Alt letter: see
            // `panel_spelling`.
            Do::ShowFile
            | Do::RenameFile
            | Do::DeleteSample
            | Do::TrimSample
            | Do::FocusTimeline => Ok(false),
        }
    }

    /// The panel action an Alt letter means where the keyboard is now.
    fn panel_action_at(&self, letter: char) -> Option<BindAction> {
        use PanelKind::{Reference, Set, Settings};
        let score = matches!(self.focus, Focus::Editor | Focus::Timeline);
        let tape = score && self.current_replay().is_some();
        match (letter, self.focus) {
            ('o', Focus::Panel(Reference | Set | Settings)) => Some(BindAction::ShowFile),
            ('r', Focus::Panel(Reference | Set)) => Some(BindAction::RenameFile),
            ('d', Focus::Panel(Reference)) => Some(BindAction::DeleteSample),
            ('t', Focus::Panel(Reference)) => Some(BindAction::TrimSample),
            ('o', _) if tape => Some(BindAction::ShowFile),
            ('r', _) if tape => Some(BindAction::RenameFile),
            ('t', _) if score && self.timeline_visible() => Some(BindAction::FocusTimeline),
            _ => None,
        }
    }

    /// A press as the panels read their own actions, or `None` for a
    /// press that does nothing here.
    ///
    /// Each panel answers to Alt and a letter. A chord learnt for a panel
    /// action arrives as that letter. The letter does nothing while its
    /// action holds a learnt chord, and the learnt chord does nothing
    /// where the action has no panel.
    pub(super) fn panel_spelling(&self, key: &KeyEvent) -> Option<KeyEvent> {
        if let Some(action) = self.keybinds.override_action_for(key)
            && let Some(letter) = action.panel_letter()
        {
            return (self.panel_action_at(letter) == Some(action))
                .then(|| KeyEvent::new(KeyCode::Char(letter), KeyModifiers::ALT));
        }
        if key.modifiers.contains(KeyModifiers::ALT)
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
            && let KeyCode::Char(character) = key.code
            && self
                .panel_action_at(character.to_ascii_lowercase())
                .is_some_and(|action| self.keybinds.overridden(action))
        {
            return None;
        }
        Some(*key)
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

#[cfg(test)]
mod tests {
    mod keybind_cascade {
        //! The chord cascade, end to end: a chord learnt in Settings fires from
        //! wherever its action lives, the key it was moved off means nothing
        //! rather than its old meaning, and an armed learn belongs to the open
        //! sheet and to nothing else.

        use crate::app::tests::support::press;
        use crate::app::*;
        use crate::keybinds::KeyCombo;
        use crate::settings::SettingsSheet;
        use crossterm::event::ModifierKeyCode;

        fn cascade_app(directory: &std::path::Path) -> App {
            let mut app = crate::app::tests::support::app_over(directory);
            app.frame = Rect::new(0, 0, 140, 40);
            app.capabilities = KeyboardCapabilities::enhanced();
            app.focus_pane(0);
            app.focus = Focus::Editor;
            app
        }

        /// Type a word into the score through the real event loop, so every test
        /// can see the caret move or the text change without reaching into the
        /// editor's internals.
        fn typed(app: &mut App, text: &str) {
            for character in text.chars() {
                press(app, KeyCode::Char(character), KeyModifiers::NONE);
            }
        }

        fn source(app: &App) -> String {
            app.editor().source()
        }

        /// An evaluation actually went out: the transport's proof of work.
        fn evaluation_underway(app: &App) -> bool {
            app.pending_evaluation.is_some() || !app.inflight.is_empty()
        }

        fn use_builtin_synth(app: &mut App) {
            // These tests exercise shortcut dispatch. The default test scene uses
            // `bd`, which may be absent from an isolated sample catalogue and is
            // rejected before the transport can queue an evaluation.
            app.scenes.current_mut().editor =
                Editor::new("$: note('c4').s('sine')\n").expect("valid synth score");
        }

        #[test]
        fn ctrl_q_is_reserved_during_capture_and_quits_everywhere_else() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = cascade_app(directory.path());
            app.settings_sheet = Some(SettingsSheet::default());
            app.focus = Focus::Panel(PanelKind::Settings);
            app.keybind_learn = Some(crate::keybinds::KeybindLearn::arm(BindAction::Undo));

            press(&mut app, KeyCode::Char('q'), KeyModifiers::CONTROL);
            assert!(!app.quit_armed && !app.quit, "binding Ctrl+Q tried to quit");
            assert!(
                app.keybind_learn.is_some(),
                "Ctrl+Q was captured as a binding"
            );
            assert!(app.status.contains("reserved for quit"), "{}", app.status);

            app.keybind_learn = None;
            press(&mut app, KeyCode::Char('q'), KeyModifiers::CONTROL);
            assert!(app.quit_armed, "Ctrl+Q was unavailable in Settings");
            press(&mut app, KeyCode::Char('q'), KeyModifiers::CONTROL);
            assert!(app.quit, "Ctrl+Q could not confirm quit from Settings");
        }

        /// A chord learnt onto undo fires in the editor from its new key, F2
        /// here. The key that undo moved off does nothing.
        #[test]
        fn a_learnt_editor_chord_fires_from_its_new_key_and_frees_the_old_one() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = cascade_app(directory.path());
            app.learn_keybind(
                BindAction::Undo,
                Some(KeyCombo::parse("f2").expect("F2 parses")),
            );
            typed(&mut app, "hello");
            assert_eq!(source(&app), "hello$: s(\"bd\")\n");

            press(&mut app, KeyCode::F(2), KeyModifiers::NONE);
            assert_eq!(
                source(&app),
                "$: s(\"bd\")\n",
                "the learnt chord undoes the typing"
            );

            // The freed ^Z is nobody's: it inserts nothing and undoes nothing.
            press(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
            assert_eq!(
                source(&app),
                "$: s(\"bd\")\n",
                "the moved-off key is freed, not still undo"
            );
        }

        /// Undo learnt onto ^F, the reference's default key, moves the whole
        /// chord: ^F undoes from inside the editor.
        #[test]
        fn a_chord_learnt_over_another_action_s_default_fires_whole() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = cascade_app(directory.path());
            app.learn_keybind(
                BindAction::Undo,
                Some(KeyCombo::parse("ctrl+f").expect("^F parses")),
            );
            typed(&mut app, "hi");

            press(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
            assert_eq!(
                source(&app),
                "$: s(\"bd\")\n",
                "undo fires from the reference's key"
            );
        }

        /// Put the caret in an empty argument that takes one of a list of values.
        fn at_a_value_argument(app: &mut App) {
            let marked = "$: s(\"bd\").warpmode(|)";
            let caret = marked.find('|').expect("caret marker");
            app.scenes.current_mut().editor =
                Editor::new(&marked.replace('|', "")).expect("editor");
            app.editor_mut()
                .set_selection(crate::editor::Selection::caret(ByteOffset(caret)))
                .expect("caret");
            app.set_reference_panel(None);
            app.focus = Focus::Editor;
        }

        /// What a chord opened: the argument's values, the function's entry,
        /// another list, or nothing.
        fn opened(app: &App) -> &'static str {
            match app.reference_panel.as_ref() {
                None => "nothing",
                Some(panel) if panel.vocabulary.is_some() => "values",
                Some(panel)
                    if matches!(panel.mode, crate::reference::ReferenceMode::Entry { .. }) =>
                {
                    "docs"
                }
                Some(_) => "list",
            }
        }

        /// In an empty argument, Ctrl+F, F2 and both encodings of Ctrl+Space
        /// list the values. Ctrl+D opens the documentation of the function.
        #[test]
        fn the_values_chords_list_values_and_the_docs_chord_opens_the_entry() {
            let directory = tempfile::tempdir().unwrap();
            for (code, modifiers, expected) in [
                (KeyCode::Char('f'), KeyModifiers::CONTROL, "values"),
                (KeyCode::F(2), KeyModifiers::NONE, "values"),
                (KeyCode::Null, KeyModifiers::NONE, "values"),
                (KeyCode::Char(' '), KeyModifiers::CONTROL, "values"),
                (KeyCode::Char('d'), KeyModifiers::CONTROL, "docs"),
            ] {
                let mut app = cascade_app(directory.path());
                at_a_value_argument(&mut app);
                press(&mut app, code, modifiers);
                assert_eq!(opened(&app), expected, "{code:?}");
            }
        }

        /// A chord learnt for one of the two actions moves that action alone.
        /// The other keeps its own key, and the key moved off does nothing.
        #[test]
        fn the_values_chord_and_the_docs_chord_are_learnt_apart() {
            let directory = tempfile::tempdir().unwrap();
            let f3 = KeyCombo::parse("f3").expect("F3 parses");
            for (learnt, on_f3, on_ctrl_f, on_ctrl_d) in [
                (BindAction::Reference, "values", "nothing", "docs"),
                (BindAction::Docs, "docs", "values", "nothing"),
            ] {
                let mut app = cascade_app(directory.path());
                app.learn_keybind(learnt, Some(f3));
                for (code, modifiers, expected) in [
                    (KeyCode::F(3), KeyModifiers::NONE, on_f3),
                    (KeyCode::Char('f'), KeyModifiers::CONTROL, on_ctrl_f),
                    (KeyCode::Char('d'), KeyModifiers::CONTROL, on_ctrl_d),
                ] {
                    at_a_value_argument(&mut app);
                    let before = source(&app);
                    press(&mut app, code, modifiers);
                    assert_eq!(opened(&app), expected, "{learnt:?}: {code:?}");
                    assert_eq!(source(&app), before, "{learnt:?}: {code:?}");
                }
            }
        }

        /// The docs panel does not replace the settings sheet. A docs chord,
        /// default or learnt, opens nothing while the sheet has the keyboard.
        #[test]
        fn a_docs_chord_opens_nothing_while_the_settings_have_the_keyboard() {
            let directory = tempfile::tempdir().unwrap();
            for learnt in [false, true] {
                let mut app = cascade_app(directory.path());
                at_a_value_argument(&mut app);
                let (code, modifiers) = if learnt {
                    app.learn_keybind(BindAction::Docs, KeyCombo::parse("f3"));
                    (KeyCode::F(3), KeyModifiers::NONE)
                } else {
                    (KeyCode::Char('d'), KeyModifiers::CONTROL)
                };
                app.toggle_settings_sheet();
                assert!(app.settings_hold_the_keys(), "learnt={learnt}");
                press(&mut app, code, modifiers);
                assert_eq!(opened(&app), "nothing", "learnt={learnt}");
                assert!(app.settings_hold_the_keys(), "learnt={learnt}");
            }
        }

        /// Rewind-update moved onto a new chord fires as the transport from
        /// anywhere, and the ^⇧S it was moved off does not fall through to the
        /// legacy reading of ^S, which would make the freed key an update.
        #[test]
        fn a_moved_transport_chord_fires_and_frees_its_old_key() {
            // The learnt chord evaluates, from anywhere the loop reaches.
            let directory = tempfile::tempdir().unwrap();
            let mut app = cascade_app(directory.path());
            app.learn_keybind(
                BindAction::RewindEvaluate,
                Some(KeyCombo::parse("f7").expect("F7 parses")),
            );
            use_builtin_synth(&mut app);
            press(&mut app, KeyCode::F(7), KeyModifiers::NONE);
            assert!(
                evaluation_underway(&app),
                "the learnt chord evaluates, from anywhere the loop reaches: {}",
                app.status
            );
            assert!(!app.next_rewind, "the rewind flag rode along and was spent");

            // The freed ^⇧S re-means nothing: not the moved rewind, and not the
            // legacy reading of ^S either, which would make the freed key a
            // quiet update. A fresh studio over the same table, so the transport
            // proof is this press's own and nobody's left over.
            let directory = tempfile::tempdir().unwrap();
            let mut app = cascade_app(directory.path());
            app.learn_keybind(
                BindAction::RewindEvaluate,
                Some(KeyCombo::parse("f7").expect("F7 parses")),
            );
            press(
                &mut app,
                KeyCode::Char('s'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            );
            assert!(!app.next_rewind, "the freed chord does not rewind");
            assert!(
                !evaluation_underway(&app),
                "and does not quietly evaluate either"
            );
        }

        /// The learn capture is the open sheet's: armed, then the sheet closed by
        /// any door, the next key means what it always meant instead of binding
        /// in silence.
        #[test]
        fn an_armed_learn_dies_with_the_sheet() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = cascade_app(directory.path());
            app.settings_sheet = Some(SettingsSheet::default());
            app.focus = Focus::Panel(PanelKind::Settings);
            app.keybind_learn = Some(crate::keybinds::KeybindLearn::arm(BindAction::Undo));
            // The sheet closes without a key: the mouse's door.
            app.settings_sheet = None;
            app.focus = Focus::Editor;

            press(&mut app, KeyCode::Char('q'), KeyModifiers::CONTROL);
            assert!(
                app.keybinds.binding(BindAction::Undo).unwrap().key() == "ctrl+z",
                "nothing was learnt"
            );
            assert!(
                !app.keybinds.overridden(BindAction::Quit),
                "and ^Q did not become undo, or quit anything"
            );
        }

        /// With a learn armed, the next press is the chord. When another action
        /// holds that chord, the learn asks first: Enter takes it and the other
        /// action goes unbound.
        #[test]
        fn an_armed_learn_asks_before_taking_a_chord_somebody_holds() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = cascade_app(directory.path());
            app.settings_sheet = Some(SettingsSheet::default());
            app.focus = Focus::Panel(PanelKind::Settings);
            app.keybind_learn = Some(crate::keybinds::KeybindLearn::arm(BindAction::Undo));

            // ^R is rename-scene's: the learn asks rather than taking in silence.
            press(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
            assert_eq!(
                app.keybinds.binding(BindAction::Undo).unwrap().key(),
                "ctrl+z",
                "nothing moved while the ask is up"
            );
            assert!(
                matches!(
                    app.keybind_learn,
                    Some(crate::keybinds::KeybindLearn::Confirm {
                        held_by: BindAction::RenameScene,
                        ..
                    })
                ),
                "the learn is asking, and says who would lose the chord"
            );

            // Enter confirms: the chord moves whole, the loser goes unbound.
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(
                app.keybinds.binding(BindAction::Undo).unwrap().key(),
                "ctrl+r",
                "the chord was taken, whatever it used to mean"
            );
            assert!(app.keybinds.overridden(BindAction::Undo));
            assert!(
                app.keybinds.is_unbound(BindAction::RenameScene),
                "rename-scene was displaced, not silently repointed"
            );
            assert!(app.keybind_learn.is_none(), "the learn is over");
        }

        /// The ask is answerable in both directions: Esc keeps what the other
        /// action has and the learn ends having changed nothing.
        #[test]
        fn esc_on_a_take_keeps_the_other_action_s_chord() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = cascade_app(directory.path());
            app.settings_sheet = Some(SettingsSheet::default());
            app.focus = Focus::Panel(PanelKind::Settings);
            app.keybind_learn = Some(crate::keybinds::KeybindLearn::arm(BindAction::Undo));

            press(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert_eq!(
                app.keybinds.binding(BindAction::Undo).unwrap().key(),
                "ctrl+z",
                "undo keeps its own chord"
            );
            assert_eq!(
                app.keybinds.binding(BindAction::RenameScene).unwrap().key(),
                "ctrl+r",
                "rename-scene keeps what it held"
            );
            assert!(app.keybind_learn.is_none());
        }

        /// Enter pressed while the learn is merely ARMED is refused, not
        /// captured: it is the key that arms and confirms, and a page whose
        /// Enter could be learnt away would strand itself.
        #[test]
        fn enter_is_refused_while_a_learn_is_armed() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = cascade_app(directory.path());
            app.settings_sheet = Some(SettingsSheet::default());
            app.focus = Focus::Panel(PanelKind::Settings);
            app.keybind_learn = Some(crate::keybinds::KeybindLearn::arm(BindAction::Undo));

            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(
                app.keybinds.binding(BindAction::Undo).unwrap().key(),
                "ctrl+z",
                "nothing was learnt"
            );
            assert!(
                app.keybind_learn.is_some(),
                "the learn stays armed, no deafer for one refused press"
            );
            // And the sheet still works: Enter on the row re-learns the action.
            assert!(
                app.keybind_rows().iter().any(|row| row.learning),
                "the row still says it is waiting for a chord"
            );
        }

        /// The reserved family is refused at the capture path too, not just in
        /// the model: Esc closes sheets and cancels learns, Tab pages the sheet,
        /// Backspace and Delete unbind rows, Space types spaces, the arrows walk
        /// the page and the score. Armed, none of them becomes a chord; the
        /// learn stays armed and the key keeps its own meaning.
        #[test]
        fn the_reserved_keys_are_never_captured_as_chords() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = cascade_app(directory.path());
            app.settings_sheet = Some(SettingsSheet::default());
            app.focus = Focus::Panel(PanelKind::Settings);

            for (code, name) in [
                (KeyCode::Enter, "enter"),
                (KeyCode::Esc, "esc"),
                (KeyCode::Tab, "tab"),
                (KeyCode::Backspace, "backspace"),
                (KeyCode::Delete, "delete"),
                (KeyCode::Char(' '), "space"),
                (KeyCode::Left, "left"),
                (KeyCode::Right, "right"),
                (KeyCode::Up, "up"),
                (KeyCode::Down, "down"),
            ] {
                app.keybind_learn = Some(crate::keybinds::KeybindLearn::arm(BindAction::Undo));
                press(&mut app, code, KeyModifiers::NONE);
                assert_eq!(
                    app.keybinds.binding(BindAction::Undo).unwrap().key(),
                    "ctrl+z",
                    "{name} was refused and learnt nothing"
                );
                // Esc and Tab close and page the sheet, so they also dismiss
                // the armed learn. Every other key leaves the learn armed.
                if matches!(code, KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab) {
                    assert!(app.keybind_learn.is_none(), "{name} did its own work");
                    // Re-open what Esc closed, for the next pass of the loop.
                    app.settings_sheet = Some(SettingsSheet::default());
                    app.focus = Focus::Panel(PanelKind::Settings);
                } else {
                    assert!(app.keybind_learn.is_some(), "{name} leaves the learn armed");
                }
            }
            // And the sheet survived the whole walk: still open, still on its
            // rows, still working.
            assert!(app.settings_sheet.is_some());
        }

        /// ctrl+space is refused AND SAID: terminals that never send it whole
        /// (gnome-terminal delivers only the modifier's work) would otherwise
        /// bind a key that cannot fire, so the capture keeps the learn armed and
        /// says on the status exactly why the key it saw is not a chord.
        #[test]
        fn ctrl_space_is_refused_with_a_word_not_a_dead_binding() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = cascade_app(directory.path());
            app.settings_sheet = Some(SettingsSheet::default());
            app.focus = Focus::Panel(PanelKind::Settings);
            app.keybind_learn = Some(crate::keybinds::KeybindLearn::arm(BindAction::Undo));

            press(&mut app, KeyCode::Char(' '), KeyModifiers::CONTROL);
            assert_eq!(
                app.keybinds.binding(BindAction::Undo).unwrap().key(),
                "ctrl+z",
                "nothing was learnt"
            );
            assert!(app.keybind_learn.is_some(), "the learn stays armed");
            assert!(
                app.status.contains("cannot be a custom shortcut"),
                "the refusal is said, not silent: {:?}",
                app.status
            );
            assert!(
                !app.keybinds.overridden(BindAction::Undo),
                "and studio.json keeps no dead binding"
            );
        }

        /// Most terminals fold ctrl+' down to a bare ', which is a typing key.
        /// The learn refuses a typing key with a status message and stays
        /// armed, because a binding on ' would fire on every typed apostrophe.
        /// After the learn ends, the letter types into the editor again.
        #[test]
        fn a_typing_key_is_refused_with_a_word_not_bound_in_silence() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = cascade_app(directory.path());
            app.settings_sheet = Some(SettingsSheet::default());
            app.focus = Focus::Panel(PanelKind::Settings);
            app.keybind_learn = Some(crate::keybinds::KeybindLearn::arm(BindAction::Undo));

            // The terminal folds ctrl+' down to a bare ' - the press the app
            // actually sees, indistinguishable from typing an apostrophe.
            press(&mut app, KeyCode::Char('\''), KeyModifiers::NONE);
            assert_eq!(
                app.keybinds.binding(BindAction::Undo).unwrap().key(),
                "ctrl+z",
                "the typing key bound nothing"
            );
            assert!(
                !app.keybinds.overridden(BindAction::Undo),
                "studio.json keeps no binding on a typing key"
            );
            assert!(
                app.status.contains("typing key"),
                "the refusal is said, not silent: {:?}",
                app.status
            );
            assert!(app.keybind_learn.is_some(), "the learn stays armed");
        }

        /// Kitty-protocol terminals report a bare modifier press before the
        /// letter of a chord. The learn ignores it, stays armed and shows no
        /// status message.
        #[test]
        fn a_bare_modifier_press_is_the_first_half_of_a_chord_not_one() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = cascade_app(directory.path());
            app.settings_sheet = Some(SettingsSheet::default());
            app.focus = Focus::Panel(PanelKind::Settings);
            app.keybind_learn = Some(crate::keybinds::KeybindLearn::arm(BindAction::Undo));
            let status_before = app.status.clone();

            press(
                &mut app,
                KeyCode::Modifier(ModifierKeyCode::LeftControl),
                KeyModifiers::NONE,
            );
            assert!(
                app.keybind_learn.is_some(),
                "the learn waits for the letter of the chord"
            );
            assert_eq!(
                app.keybinds.binding(BindAction::Undo).unwrap().key(),
                "ctrl+z",
                "the modifier alone bound nothing"
            );
            assert_eq!(
                app.status, status_before,
                "the first half of a chord says nothing at all"
            );

            // A chord that arrives whole still binds. No action holds
            // Ctrl+Shift+Q, so the learn takes it at once.
            press(
                &mut app,
                KeyCode::Char('Q'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            );
            assert_eq!(
                app.keybinds.binding(BindAction::Undo).unwrap().key(),
                "ctrl+shift+q",
                "the shortcut arrived whole and was taken"
            );
        }

        /// Space does not clear a row: it types spaces and steps lists, and a
        /// reflexive press on a row must not wipe a binding. Delete and
        /// Backspace are the clearing keys.
        #[test]
        fn space_does_not_clear_a_row_delete_and_backspace_do() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = cascade_app(directory.path());
            let undo_at = BindAction::ALL
                .iter()
                .position(|action| *action == BindAction::Undo)
                .expect("undo is a row");
            let mut sheet = SettingsSheet::default();
            sheet.show_page(crate::settings::SettingsPage::Keybinds);
            sheet.selected = crate::settings::KEYBIND_ACTION_START + undo_at;
            app.settings_sheet = Some(sheet);
            app.focus = Focus::Panel(PanelKind::Settings);
            app.learn_keybind(BindAction::Undo, Some(KeyCombo::parse("f2").unwrap()));
            assert_eq!(app.keybinds.binding(BindAction::Undo).unwrap().key(), "f2");

            // Space on the row: the binding stands.
            press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
            assert_eq!(
                app.keybinds.binding(BindAction::Undo).unwrap().key(),
                "f2",
                "space cleared nothing"
            );

            // Delete clears it back to the studio's own chord.
            press(&mut app, KeyCode::Delete, KeyModifiers::NONE);
            assert_eq!(
                app.keybinds.binding(BindAction::Undo).unwrap().key(),
                "ctrl+z",
                "delete puts the studio's own chord back"
            );

            // Backspace does the same work, on a fresh learnt chord.
            app.learn_keybind(BindAction::Undo, Some(KeyCombo::parse("f2").unwrap()));
            press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
            assert_eq!(
                app.keybinds.binding(BindAction::Undo).unwrap().key(),
                "ctrl+z",
                "backspace puts the studio's own chord back too"
            );
        }

        /// A learnt chord pressed with alt held is not the learnt chord. Alt
        /// folds into the primary modifier, so the press falls through and the
        /// default reading of ^S evaluates, the same as with no override learnt.
        #[test]
        fn an_alt_held_learnt_chord_falls_through_like_its_default_does() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = cascade_app(directory.path());
            app.learn_keybind(
                BindAction::Evaluate,
                Some(KeyCombo::parse("ctrl+s").expect("^S parses")),
            );
            use_builtin_synth(&mut app);
            press(
                &mut app,
                KeyCode::Char('s'),
                KeyModifiers::CONTROL | KeyModifiers::ALT,
            );
            assert!(
                evaluation_underway(&app),
                "alt folds into the chord, as it does for every default"
            );

            // And the same press on an untouched studio is the same: the learnt
            // chord did not change what alt+^S means.
            let directory = tempfile::tempdir().unwrap();
            let mut reference = cascade_app(directory.path());
            use_builtin_synth(&mut reference);
            press(
                &mut reference,
                KeyCode::Char('s'),
                KeyModifiers::CONTROL | KeyModifiers::ALT,
            );
            assert!(evaluation_underway(&reference));
        }
    }

    mod shortcut_hint {
        //! User-visible shortcuts and their contextual input routes share one table.

        use crate::app::tests::support::press;
        use crate::app::*;
        use crate::keybinds::{KeyCombo, Reach};

        fn app(directory: &std::path::Path) -> App {
            let mut app = crate::app::tests::support::app_over(directory);
            app.frame = Rect::new(0, 0, 160, 50);
            app.capabilities = KeyboardCapabilities::enhanced();
            app.keybinds.set_reach(Reach {
                enhanced: true,
                terminal: "xterm".into(),
            });
            app.focus_pane(0);
            app.focus = Focus::Editor;
            app
        }

        fn bind(app: &mut App, action: BindAction, number: u8) {
            app.keybinds.learn(
                action,
                Some(KeyCombo {
                    code: KeyCode::F(number),
                    control: true,
                    shift: false,
                }),
            );
        }

        fn custom(app: &mut App, number: u8) {
            press(app, KeyCode::F(number), KeyModifiers::CONTROL);
        }

        #[test]
        fn scene_and_split_status_hints_follow_rebindings_and_unbound_actions() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app(directory.path());
            bind(&mut app, BindAction::NewScene, 1);
            bind(&mut app, BindAction::RenameScene, 2);
            bind(&mut app, BindAction::HopPane, 3);
            bind(&mut app, BindAction::Split, 4);
            app.step_scene(1);
            assert!(app.status.contains("^F1 adds another"), "{}", app.status);
            app.toggle_split();
            for hint in ["^F3 hops panes", "^F2 names it", "^F4 closes the split"] {
                assert!(app.status.contains(hint), "{}", app.status);
            }
            app.close_split();
            app.keybinds.unbind(BindAction::Split);
            app.toggle_split();
            assert!(
                app.status.contains("View > Close the split"),
                "{}",
                app.status
            );
            app.new_scene(false);
            assert!(app.status.contains("^F2 names it"), "{}", app.status);
        }

        #[test]
        fn set_log_and_zen_status_hints_follow_the_current_bindings() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app(directory.path());
            bind(&mut app, BindAction::SetPanel, 1);
            bind(&mut app, BindAction::LogSticky, 2);
            bind(&mut app, BindAction::Zen, 3);
            app.open_set_panel(true);
            assert!(app.status.contains("^F1 hides the panel"), "{}", app.status);
            assert!(app.status.contains("Del deletes"), "{}", app.status);
            assert!(!app.status.contains("^D deletes"), "{}", app.status);
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.status.contains("^F1 hides it"), "{}", app.status);
            app.toggle_log_sticky();
            app.focus_panel(PanelKind::Log);
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.status.contains("^F2 un-docks it"), "{}", app.status);
            app.ui_settings.zen = true;
            app.viz_docks[0] = None;
            app.toggle_viz_dock(0);
            assert!(
                app.status.contains("^F3 restores the stage"),
                "{}",
                app.status
            );
            assert!(!app.status.contains("F11"));
        }

        #[test]
        fn a_rebound_copy_copies_the_log_and_retires_the_old_key() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app(directory.path());
            bind(&mut app, BindAction::Copy, 1);
            app.toggle_log_panel();
            let panel = app.log_panel.as_mut().unwrap();
            panel.editor = Editor::new("this is the selected log").unwrap();
            panel
                .editor
                .dispatch(
                    Command::SelectAll,
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .unwrap();
            let score = app.editor().source();
            app.clipboard.set_text("kept".into()).unwrap();
            press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
            assert_eq!(app.clipboard.get_text().unwrap(), "kept");
            custom(&mut app, 1);
            assert_eq!(
                app.clipboard.get_text().unwrap(),
                "this is the selected log"
            );
            assert_eq!(app.editor().source(), score);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Log));
            app.keybinds.unbind(BindAction::Copy);
            app.clipboard.set_text("unbound".into()).unwrap();
            press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
            assert_eq!(app.clipboard.get_text().unwrap(), "unbound");
        }

        #[test]
        fn a_rebound_copy_uses_the_theme_form_and_code_selection() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app(directory.path());
            bind(&mut app, BindAction::Copy, 1);
            let theme = app.theme.clone();
            app.open_theme_editor(&theme, "test theme".into(), false);
            let json = app.theme_editor.as_ref().unwrap().draft.to_json();
            app.clipboard.set_text("kept".into()).unwrap();
            press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
            assert_eq!(app.clipboard.get_text().unwrap(), "kept");
            custom(&mut app, 1);
            assert_eq!(app.clipboard.get_text().unwrap(), json);
            let theme_editor = app.theme_editor.as_mut().unwrap();
            theme_editor.tab = crate::theme_editor::EditorTab::Code;
            theme_editor.code = Editor::new("selected theme code").unwrap();
            theme_editor
                .code
                .dispatch(
                    Command::SelectAll,
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .unwrap();
            custom(&mut app, 1);
            assert_eq!(app.clipboard.get_text().unwrap(), "selected theme code");
            assert_eq!(app.focus, Focus::Panel(PanelKind::ThemeEditor));
        }

        #[test]
        fn learned_editing_shortcuts_stay_in_the_active_prompt() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app(directory.path());
            bind(&mut app, BindAction::Paste, 1);
            bind(&mut app, BindAction::SelectAll, 2);
            app.open_set_prompt(SetPrompt::RenameSet);
            let score = app.editor().source();
            custom(&mut app, 2);
            assert!(app.set_prompt.as_ref().unwrap().1.selection().is_some());
            app.clipboard.set_text("new name".into()).unwrap();
            custom(&mut app, 1);
            assert_eq!(app.set_prompt.as_ref().unwrap().1.path, "new name");
            assert_eq!(app.editor().source(), score);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Set));
        }

        #[test]
        fn moved_panel_toggles_retire_their_old_close_keys() {
            for (action, panel, code, modifiers) in [
                (
                    BindAction::Log,
                    PanelKind::Log,
                    KeyCode::Char('d'),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                ),
                (
                    BindAction::Devices,
                    PanelKind::Devices,
                    KeyCode::Char('p'),
                    KeyModifiers::CONTROL,
                ),
                (
                    BindAction::Export,
                    PanelKind::Export,
                    KeyCode::Char('x'),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                ),
                (
                    BindAction::ThemePicker,
                    PanelKind::Theme,
                    KeyCode::Char('t'),
                    KeyModifiers::CONTROL,
                ),
            ] {
                let directory = tempfile::tempdir().unwrap();
                let mut app = app(directory.path());
                bind(&mut app, action, 1);
                custom(&mut app, 1);
                assert_eq!(app.focus, Focus::Panel(panel), "{action:?} opens");
                press(&mut app, code, modifiers);
                assert_eq!(
                    app.focus,
                    Focus::Panel(panel),
                    "{action:?} old key closed the panel"
                );
                custom(&mut app, 1);
                assert_ne!(app.focus, Focus::Panel(panel), "{action:?} new key closes");
            }
        }

        #[test]
        fn settings_replacement_dialogs_accept_default_adapted_and_learned_shortcuts() {
            for (enhanced, learned) in [(true, false), (false, false), (false, true)] {
                for action in [
                    BindAction::ThemePicker,
                    BindAction::Export,
                    BindAction::Devices,
                    BindAction::OpenSet,
                    BindAction::Log,
                ] {
                    let directory = tempfile::tempdir().unwrap();
                    let mut app = app(directory.path());
                    app.capabilities = if enhanced {
                        KeyboardCapabilities::enhanced()
                    } else {
                        KeyboardCapabilities::legacy()
                    };
                    app.keybinds.set_reach(Reach {
                        enhanced,
                        terminal: "xterm".into(),
                    });
                    if learned {
                        bind(&mut app, action, 1);
                    }
                    let chord = app.keybinds.binding(action).expect("available shortcut");
                    app.toggle_settings_sheet();
                    assert!(app.settings_hold_the_keys());
                    let modifiers = (if chord.control {
                        KeyModifiers::CONTROL
                    } else {
                        KeyModifiers::NONE
                    }) | (if chord.shift {
                        KeyModifiers::SHIFT
                    } else {
                        KeyModifiers::NONE
                    });
                    press(&mut app, chord.code, modifiers);
                    assert!(
                        app.settings_sheet.is_none(),
                        "{action:?}, enhanced={enhanced}, learned={learned}"
                    );
                    let opened = match action {
                        BindAction::ThemePicker => app.theme_picker.is_some(),
                        BindAction::Export => app.export_sheet.is_some(),
                        BindAction::Devices => app.panel.is_some(),
                        BindAction::OpenSet => {
                            matches!(app.set_prompt, Some((SetPrompt::OpenSet, _)))
                        }
                        BindAction::Log => app.log_panel.is_some(),
                        _ => unreachable!(),
                    };
                    assert!(opened, "{action:?}, enhanced={enhanced}, learned={learned}");
                }
            }
        }

        #[test]
        fn settings_blocks_editing_and_cancels_reset_before_replacement_dialogs() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app(directory.path());
            bind(&mut app, BindAction::Paste, 1);
            let binding = app.keybinds.binding(BindAction::Paste);
            let source = app.editor().source();
            app.clipboard
                .set_text("must not enter the score".into())
                .unwrap();
            app.toggle_settings_sheet();
            let sheet = app.settings_sheet.as_mut().unwrap();
            sheet.show_page(crate::settings::SettingsPage::Keybinds);
            sheet.selected = crate::settings::RESET_KEYBINDS_ROW;
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(app.settings_sheet.unwrap().confirm_reset_keybinds);
            custom(&mut app, 1);
            assert!(!app.settings_sheet.unwrap().confirm_reset_keybinds);
            assert!(app.settings_hold_the_keys());
            assert_eq!(app.editor().source(), source);
            assert_eq!(app.keybinds.binding(BindAction::Paste), binding);
            // The next Enter arms a new confirmation; the swallowed edit never
            // leaves an old confirmation waiting to reset the user's bindings.
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(app.settings_sheet.unwrap().confirm_reset_keybinds);
            assert_eq!(app.keybinds.binding(BindAction::Paste), binding);
            press(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
            assert!(app.theme_picker.is_some());
            assert!(app.settings_sheet.is_none());
            assert_eq!(app.keybinds.binding(BindAction::Paste), binding);
            app.toggle_settings_sheet();
            assert!(!app.settings_sheet.unwrap().confirm_reset_keybinds);
        }
    }
}
