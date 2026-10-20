//! Clipboard ownership for text fields outside the score, and the reader that
//! gathers a paste a legacy terminal sends as ordinary keys into one chunk
//! before its Enter reaches a prompt.

use super::super::file_picker::single_line_paste;
use super::super::theme_editor::EditorTab;
use super::*;

impl App {
    pub(super) fn accepts_raw_paste_burst(&self) -> bool {
        // The find prompt takes a pasted pattern whole, including any Enter
        // the terminal sends as part of it.
        if self.editor_accepts_raw_paste() && self.editor().find_open() {
            return true;
        }
        // Normal-mode command bursts must reach Vim one key at a time.
        if !self.editor().vim_is_insert()
            && (self.editor_accepts_raw_paste()
                || (self.focus == Focus::Panel(PanelKind::Reference)
                    && self
                        .reference_panel
                        .as_ref()
                        .is_some_and(super::super::reference::ReferencePanel::types_through)))
        {
            return false;
        }
        self.accepts_raw_paste()
    }

    /// Legacy terminals (including Windows key records) can send a paste
    /// as ordinary keys. Collect it before Enter reaches a prompt handler.
    pub(super) fn accepts_raw_paste(&self) -> bool {
        if self.piano.open || self.help.is_some() || self.menu.is_some() {
            return false;
        }
        // Coalesce a Ctrl+V flood wherever a panel might otherwise type
        // it as `n`/`a`/`Enter`. The paste is then kept or ignored; it
        // is never executed as shortcuts.
        if self.editor_accepts_raw_paste() {
            return true;
        }
        matches!(self.focus, Focus::Panel(_) | Focus::Timeline)
            || self.replay_edit.is_some()
            || self.slider_precision.is_some()
            || self.smart_action.is_some()
            || matches!(self.strip_mode, SceneStripMode::Renaming(_))
    }

    /// Whether the focused text field is a *filter* rather than a value:
    /// a box that narrows a list of names the studio already holds.
    ///
    /// It is the one kind of field that yields a real file drop back to
    /// the window. Nothing a filter can match is an absolute path, so a
    /// path landing in one was never typed on purpose - it is a folder
    /// dropped on a window that happened to have the browser focused.
    /// Every other field, including the ones that legitimately take a
    /// path, keeps its paste.
    pub(super) fn focus_filters_names(&self) -> bool {
        match self.focus {
            Focus::Panel(PanelKind::Reference) => self
                .reference_panel
                .as_ref()
                .is_some_and(super::super::reference::ReferencePanel::wants_text),
            Focus::Panel(PanelKind::Theme) => self.theme_picker.is_some(),
            _ => false,
        }
    }

    /// A paste is one edit owned by a real text field, or ignored.
    /// Panels without a field do not keep it, dump it into the score, or
    /// run it as shortcuts.
    pub(super) fn paste_to_prompt(&mut self, text: &str) -> bool {
        if let SceneStripMode::Renaming(draft) = &mut self.strip_mode {
            if self.rename_untouched {
                draft.clear();
                self.rename_untouched = false;
            }
            let room = 40_usize.saturating_sub(draft.chars().count());
            draft.extend(single_line_paste(text).chars().take(room));
            self.dirty_frame = true;
            return true;
        }
        let Focus::Panel(kind) = self.focus else {
            return false;
        };
        let kept = match kind {
            PanelKind::Set => {
                let Some((_, picker)) = &mut self.set_prompt else {
                    return false;
                };
                picker.paste_text(text);
                true
            }
            PanelKind::Viz => return self.paste_viz_prompt(text),
            PanelKind::ThemeEditor => {
                self.paste_to_theme_editor(text);
                self.theme_editor.as_ref().is_some_and(|editor| {
                    editor.saving.is_some()
                        || editor.picker.is_some()
                        || editor.entry.is_some()
                        || editor.tab == EditorTab::Code
                })
            }
            PanelKind::Reference => {
                super::vim_keys::enter_insert(self);
                let Some(panel) = self.reference_panel.as_mut() else {
                    return false;
                };
                if !panel.wants_text() {
                    return false;
                }
                panel.paste_query(&self.reference, &single_line_paste(text));
                true
            }
            PanelKind::Theme => {
                super::vim_keys::enter_insert(self);
                let Some(picker) = self.theme_picker.as_mut() else {
                    return false;
                };
                for character in single_line_paste(text).chars() {
                    picker.push_query(character);
                }
                self.schedule_theme_preview();
                true
            }
            PanelKind::Export => {
                if matches!(
                    self.export_sheet.as_ref().map(|sheet| sheet.field),
                    Some(
                        super::super::export::Field::First
                            | super::super::export::Field::Second
                            | super::super::export::Field::Target
                    )
                ) {
                    super::vim_keys::enter_insert(self);
                }
                let Some(sheet) = self.export_sheet.as_mut() else {
                    return false;
                };
                if !matches!(
                    sheet.field,
                    super::super::export::Field::First
                        | super::super::export::Field::Second
                        | super::super::export::Field::Target
                ) {
                    return false;
                }
                for character in single_line_paste(text).chars() {
                    sheet.type_char(character);
                }
                true
            }
            PanelKind::Devices
            | PanelKind::Log
            | PanelKind::Jobs
            | PanelKind::Settings
            | PanelKind::Mixer
            | PanelKind::Memory => false,
        };
        if kept {
            self.dirty_frame = true;
        }
        kept
    }

    pub(super) fn prompt_copy_shortcut(&mut self, key: &KeyEvent, alt: bool) -> bool {
        if alt {
            return false;
        }
        match self.keybinds.action_for(key) {
            Some(action @ (BindAction::Copy | BindAction::Cut)) => self.prompt_edit_action(action),
            _ => false,
        }
    }

    pub(super) fn focused_file_picker(&self) -> Option<&FilePicker> {
        match self.focus {
            Focus::Panel(PanelKind::Set) => self.set_prompt.as_ref().map(|(_, picker)| picker),
            Focus::Panel(PanelKind::Viz) => self.viz_docks[self.viz_focus]
                .as_ref()
                .and_then(|panel| panel.prompt.as_ref()),
            _ => None,
        }
    }

    pub(super) fn prompt_edit_action(&mut self, action: BindAction) -> bool {
        if action == BindAction::Paste {
            return self.focused_file_picker().is_some() && self.paste_prompt_clipboard();
        }
        if !matches!(
            action,
            BindAction::Copy | BindAction::Cut | BindAction::SelectAll
        ) {
            return false;
        }
        let picker = match self.focus {
            Focus::Panel(PanelKind::Set) => self.set_prompt.as_mut().map(|(_, picker)| picker),
            Focus::Panel(PanelKind::Viz) => self.viz_docks[self.viz_focus]
                .as_mut()
                .and_then(|panel| panel.prompt.as_mut()),
            _ => None,
        };
        let Some(picker) = picker else {
            return false;
        };
        picker.cancel_click();
        if picker.typing && action == BindAction::SelectAll {
            picker.select_all();
            self.dirty_frame = true;
        } else if picker.typing
            && let Some(text) = picker.selected_text()
        {
            match self.clipboard.set_text(text.to_owned()) {
                Ok(()) if action == BindAction::Cut => picker.delete_forward(),
                Ok(()) => {}
                Err(error) => self.status = format!("could not copy: {error}"),
            }
            self.dirty_frame = true;
        }
        true
    }

    /// The paste chord, for the text field that has the keyboard. `true`
    /// when a prompt took the clipboard, or the clipboard could not be read
    /// and the score takes no paste to fall back on.
    pub(super) fn paste_shortcut(&mut self, key: &KeyEvent, alt: bool) -> bool {
        // A paste shortcut belongs to the text field that currently has
        // the keyboard.  Bracketed terminal paste already takes this
        // route in `route_paste`; the internal clipboard shortcut must do the same
        // instead of falling through to the hidden score editor.
        !alt && self.keybinds.action_for(key) == Some(BindAction::Paste)
            && self.paste_prompt_clipboard()
    }

    fn paste_prompt_clipboard(&mut self) -> bool {
        match self.clipboard.get_text() {
            Ok(text) if self.paste_to_prompt(&text) => {
                self.dirty_frame = true;
                return true;
            }
            Ok(_) => {}
            Err(error) if !self.editor_accepts_raw_paste() => {
                self.status = format!("could not paste: {error}");
                self.dirty_frame = true;
                return true;
            }
            Err(_) => {}
        }
        false
    }

    fn paste_to_theme_editor(&mut self, text: &str) {
        let moment = self.moment();
        let Some(editor) = self.theme_editor.as_mut() else {
            return;
        };
        if let Some(saving) = editor.saving.as_mut() {
            saving.name.push_str(&single_line_paste(text));
            saving.note = None;
        } else if let Some(picker) = editor.picker.as_mut() {
            let hex = text.trim();
            if !hex.is_empty()
                && hex
                    .chars()
                    .all(|character| character.is_ascii_hexdigit() || character == '#')
            {
                picker.hex = hex.to_owned();
                if editor.paint() {
                    editor.rebuild_code();
                    let draft = editor.draft.clone();
                    self.apply_theme_draft(&draft);
                }
            }
        } else if let Some(entry) = editor.entry.as_mut() {
            entry.push_str(&single_line_paste(text));
        } else if editor.tab == EditorTab::Code {
            let before = editor.code.revision();
            let _ = editor.code.dispatch(
                Command::PasteText(text.to_owned()),
                moment,
                &mut *self.clipboard,
            );
            if editor.code.revision() != before {
                editor.code_dirty_at = Some(Instant::now());
                editor.code_error = None;
            }
        }
    }
}

pub(super) const MAX_RAW_PASTE_KEY_EVENTS_PER_TURN: usize = DEFAULT_MAX_DOCUMENT_BYTES;

pub(super) const MAX_RAW_PASTE_TERMINAL_EVENTS_PER_TURN: usize = DEFAULT_MAX_DOCUMENT_BYTES * 2;

#[cfg(not(windows))]
pub(super) const RAW_PASTE_IDLE: Duration = Duration::from_millis(1);

#[cfg(windows)]
pub(super) const RAW_PASTE_IDLE: Duration = Duration::from_millis(16);

/// A capped paste can arrive in several Windows input deliveries. Only a
/// full paste idle gap ends it; an empty instantaneous poll does not.
pub(super) fn poll_after_input(
    continuation: &mut bool,
    mut poll: impl FnMut(Duration) -> std::io::Result<bool>,
) -> std::io::Result<bool> {
    let wait = if *continuation {
        RAW_PASTE_IDLE
    } else {
        Duration::ZERO
    };
    let ready = poll(wait)?;
    if !ready {
        *continuation = false;
    }
    Ok(ready)
}

pub(super) fn read_raw_paste_chunk(
    first: Event,
    continuation: &mut bool,
    key_limit: usize,
    terminal_limit: usize,
    mut poll: impl FnMut(Duration) -> std::io::Result<bool>,
    mut read: impl FnMut() -> std::io::Result<Event>,
) -> std::io::Result<Vec<Event>> {
    debug_assert!(is_plain_text_key_event(&first));
    let mut batch = TerminalEventBatch::with_paste_continuation(*continuation);
    batch.push(first);
    let mut text_events = 1;
    let mut exhausted = true;
    for _ in 1..terminal_limit {
        if !poll(RAW_PASTE_IDLE)? {
            exhausted = false;
            break;
        }
        let next = read()?;
        // Character key-ups add nothing to a paste; modifier releases still
        // need routing so terminal-owned pointer overrides can be restored.
        if matches!(&next, Event::Key(key)
            if key.kind == KeyEventKind::Release && !matches!(key.code, KeyCode::Modifier(_)))
        {
            continue;
        }
        let continues = is_plain_text_key_event(&next);
        batch.push(next);
        if !continues {
            exhausted = false;
            break;
        }
        text_events += 1;
        if text_events >= key_limit {
            break;
        }
    }
    let events = batch.finish();
    *continuation = exhausted && events.iter().any(|event| matches!(event, Event::Paste(_)));
    Ok(events)
}

#[cfg(test)]
mod tests {
    mod prompt_paste {
        //! Tests for `app/prompt_paste.rs`.

        use crate::app::prompt_paste::*;
        use crate::app::tests::support::*;
        use crate::editor::{MemoryClipboard, Selection, event_to_input};
        use std::cell::RefCell;

        #[test]
        fn mappings_path_selection_owns_clipboard_shortcuts_and_menu_actions() {
            fn edit_menu(app: &mut App, mnemonic: char) {
                press(app, KeyCode::F(1), KeyModifiers::NONE);
                press(app, KeyCode::Char('e'), KeyModifiers::NONE);
                press(app, KeyCode::Char(mnemonic), KeyModifiers::NONE);
                assert!(app.menu.is_none(), "the menu action ran");
            }

            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let score = app.editor().source();
            let score_selection = Selection::range(ByteOffset(0), ByteOffset(score.len()));
            app.editor_mut().set_selection(score_selection).unwrap();
            app.settings_sheet = Some(SettingsSheet::default());
            app.focus = Focus::Panel(PanelKind::Settings);
            app.open_set_prompt(SetPrompt::VimMappingsPath);
            let path = "maps/λ猫/keys.json";
            app.set_prompt.as_mut().unwrap().1.set_path(path);
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
            terminal
                .draw(|frame| app.paint_into(frame, Instant::now()).unwrap())
                .unwrap();
            let picker = &app.set_prompt.as_ref().unwrap().1;
            let (area, list) = picker.geometry(app.frame).unwrap();
            let start = list.x + picker.label.len() as u16 + 2;
            for (kind, offset) in [
                (MouseEventKind::Down(MouseButton::Left), 5),
                (MouseEventKind::Drag(MouseButton::Left), 8),
                (MouseEventKind::Up(MouseButton::Left), 8),
            ] {
                app.handle_terminal_event(Event::Mouse(MouseEvent {
                    kind,
                    column: start + offset,
                    row: area.y + 1,
                    modifiers: KeyModifiers::NONE,
                }))
                .unwrap();
            }
            for modifiers in [KeyModifiers::CONTROL, KeyModifiers::SUPER] {
                app.clipboard.set_text("kept".into()).unwrap();
                press(&mut app, KeyCode::Char('c'), modifiers);
                assert_eq!(app.clipboard.get_text().unwrap(), "λ猫");
            }
            app.clipboard.set_text("kept".into()).unwrap();
            edit_menu(&mut app, 'o');
            assert_eq!(app.clipboard.get_text().unwrap(), "λ猫");
            edit_menu(&mut app, 'c');
            assert_eq!(app.set_prompt.as_ref().unwrap().1.path, "maps//keys.json");
            let menus = app.menus();
            let edit = menus.iter().find(|menu| menu.title == "Edit").unwrap();
            for label in ["Copy", "Cut"] {
                assert!(
                    !edit
                        .items
                        .iter()
                        .find(|item| item.label == label)
                        .unwrap()
                        .enabled
                );
            }
            edit_menu(&mut app, 'p');
            assert_eq!(app.set_prompt.as_ref().unwrap().1.path, path);
            edit_menu(&mut app, 's');
            edit_menu(&mut app, 'o');
            assert_eq!(app.clipboard.get_text().unwrap(), path);
            assert_eq!(app.editor().source(), score);
            assert_eq!(app.editor().primary_selection(), score_selection);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Set));
            assert!(app.settings_sheet.is_some());
        }

        #[test]
        fn file_picker_clipboard_shortcuts_stay_with_the_focused_field() {
            fn field(app: &App) -> &str {
                match app.focus {
                    Focus::Panel(PanelKind::Set) => &app.set_prompt.as_ref().unwrap().1.path,
                    Focus::Panel(PanelKind::Viz) => {
                        &app.viz_docks[app.viz_focus]
                            .as_ref()
                            .unwrap()
                            .prompt
                            .as_ref()
                            .unwrap()
                            .path
                    }
                    _ => panic!("picker has focus"),
                }
            }

            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let score = app.editor().source();
            app.editor_mut()
                .set_selection(Selection::range(ByteOffset(0), ByteOffset(score.len())))
                .unwrap();
            for (kind, text, copy, cut) in [
                (
                    PanelKind::Set,
                    "keep λ猫",
                    KeyCode::Char('c'),
                    KeyCode::Char('x'),
                ),
                (PanelKind::Viz, "keep\nλ猫", KeyCode::F(2), KeyCode::F(3)),
            ] {
                if kind == PanelKind::Set {
                    app.open_set_prompt(SetPrompt::RenameSet);
                    app.set_prompt.as_mut().unwrap().1.set_path(text);
                } else {
                    app.set_prompt = None;
                    art_prompt(&mut app);
                    app.viz_docks[app.viz_focus]
                        .as_mut()
                        .unwrap()
                        .prompt
                        .as_mut()
                        .unwrap()
                        .set_path(text);
                    for (action, chord) in
                        [(BindAction::Copy, "ctrl+f2"), (BindAction::Cut, "ctrl+f3")]
                    {
                        app.keybinds
                            .learn(action, crate::keybinds::KeyCombo::parse(chord));
                    }
                }
                press(&mut app, KeyCode::End, KeyModifiers::NONE);
                for _ in 0..2 {
                    press(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
                }
                app.clipboard.set_text("kept".into()).unwrap();
                if kind == PanelKind::Viz {
                    for code in [KeyCode::Char('c'), KeyCode::Char('x')] {
                        press(&mut app, code, KeyModifiers::CONTROL);
                    }
                    assert_eq!(app.clipboard.get_text().unwrap(), "kept");
                    assert_eq!(field(&app), text);
                }
                press(&mut app, copy, KeyModifiers::CONTROL);
                assert_eq!(app.clipboard.get_text().unwrap(), "λ猫");
                assert_eq!(field(&app), text);
                press(&mut app, cut, KeyModifiers::CONTROL);
                assert_eq!(app.clipboard.get_text().unwrap(), "λ猫");
                assert_eq!(field(&app), &text[..text.len() - "λ猫".len()]);
                app.clipboard.set_text("kept".into()).unwrap();
                for code in [copy, cut] {
                    press(&mut app, code, KeyModifiers::CONTROL);
                }
                assert_eq!(app.clipboard.get_text().unwrap(), "kept");
                assert_eq!(field(&app), &text[..text.len() - "λ猫".len()]);
                assert_eq!(app.editor().source(), score);
                assert_eq!(app.focus, Focus::Panel(kind));
            }

            app.open_set_prompt(SetPrompt::OpenSet);
            app.set_prompt.as_mut().unwrap().1.offer("λ猫");
            press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
            assert!(app.set_prompt.as_ref().unwrap().1.browsing());
            for code in [KeyCode::F(2), KeyCode::F(3)] {
                press(&mut app, code, KeyModifiers::CONTROL);
            }
            assert_eq!(app.clipboard.get_text().unwrap(), "kept");
            assert_eq!(field(&app), "λ猫");
            assert_eq!(app.editor().source(), score);
        }

        #[test]
        fn settings_source_prompt_pastes_the_internal_clipboard() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let score = app.editor().source();
            let pasted = directory.path().join("my samples").display().to_string();
            app.clipboard
                .set_text(pasted.clone())
                .expect("seed clipboard");
            let mut sheet = SettingsSheet::default();
            sheet.show_page(crate::settings::SettingsPage::Sources);
            app.settings_sheet = Some(sheet);
            app.focus = Focus::Panel(PanelKind::Settings);

            press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Set));
            press(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL);

            assert_eq!(
                app.set_prompt
                    .as_ref()
                    .map(|(_, picker)| picker.path.as_str()),
                Some(pasted.as_str())
            );
            assert_eq!(app.editor().source(), score, "the covered score was edited");
        }

        /// Copying from the score and pasting into a panel search edits the
        /// search that has the keyboard, never the score behind it.
        #[test]
        fn editor_selection_pastes_into_the_focused_reference_search() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.scenes.current_mut().editor =
                crate::editor::Editor::new("wt_digital").expect("editor");
            app.editor_mut()
                .set_selection(Selection::range(ByteOffset(0), ByteOffset(10)))
                .expect("selection");
            press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);

            app.reference_panel = Some(ReferencePanel::browse(&app.reference));
            app.focus_panel(PanelKind::Reference);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Reference));
            press(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL);

            assert_eq!(
                app.reference_panel
                    .as_ref()
                    .map(|panel| panel.query.as_str()),
                Some("wt_digital")
            );
            assert_eq!(app.editor().source(), "wt_digital");
        }

        #[test]
        fn a_large_raw_key_stream_crosses_the_event_cap_as_one_exact_paste() {
            let source = (0..120)
                .map(|line| format!("line {line}\n  nested {line}\n"))
                .collect::<String>();
            let mut queued = source
                .chars()
                .map(|character| {
                    let code = match character {
                        '\n' => KeyCode::Enter,
                        '\t' => KeyCode::Tab,
                        character => KeyCode::Char(character),
                    };
                    Event::Key(crossterm::event::KeyEvent::new(code, KeyModifiers::NONE))
                })
                .collect::<VecDeque<_>>();
            assert!(queued.len() > MAX_EVENTS_PER_TURN);
            let first = queued.pop_front().expect("first key");
            let queued = RefCell::new(queued);
            let events = read_raw_paste_batch(
                first,
                |_| Ok(!queued.borrow().is_empty()),
                || Ok(queued.borrow_mut().pop_front().expect("polled key")),
            )
            .expect("collect key stream");
            assert_eq!(events, vec![Event::Paste(source.clone())]);

            let Some(EditorInput::Command(command)) = event_to_input(
                events.into_iter().next().expect("paste event"),
                KeyboardCapabilities::legacy(),
            ) else {
                panic!("paste command")
            };
            let mut editor = Editor::new("").expect("editor");
            editor
                .dispatch(command, HistoryMoment(0), &mut MemoryClipboard::default())
                .expect("apply paste");
            assert_eq!(editor.source(), source);
            assert_eq!(editor.undo_depth(), 1);
        }

        #[test]
        fn windows_release_records_do_not_split_or_autoindent_a_raw_paste() {
            let source = "register('fill', function (pat) {\n  return new Pattern(function (state) {\n    const lookbothways = 1;\n  });\n});";
            let mut queued = source
                .chars()
                .flat_map(|character| {
                    let code = match character {
                        '\n' => KeyCode::Enter,
                        '\t' => KeyCode::Tab,
                        character => KeyCode::Char(character),
                    };
                    [
                        Event::Key(crossterm::event::KeyEvent::new_with_kind(
                            code,
                            KeyModifiers::NONE,
                            KeyEventKind::Press,
                        )),
                        Event::Key(crossterm::event::KeyEvent::new_with_kind(
                            code,
                            KeyModifiers::NONE,
                            KeyEventKind::Release,
                        )),
                    ]
                })
                .collect::<VecDeque<_>>();
            let first = queued.pop_front().expect("first key");
            let queued = RefCell::new(queued);
            let events = read_raw_paste_batch(
                first,
                |_| Ok(!queued.borrow().is_empty()),
                || Ok(queued.borrow_mut().pop_front().expect("polled event")),
            )
            .expect("collect key stream");
            assert_eq!(events, vec![Event::Paste(source.into())]);

            let Some(EditorInput::Command(command)) = event_to_input(
                events.into_iter().next().expect("paste event"),
                KeyboardCapabilities::legacy(),
            ) else {
                panic!("paste command")
            };
            let mut editor = Editor::new("").expect("editor");
            editor
                .dispatch(command, HistoryMoment(0), &mut MemoryClipboard::default())
                .expect("apply paste");
            assert_eq!(editor.source(), source);
            assert_eq!(editor.undo_depth(), 1);
            editor
                .dispatch(
                    Command::Undo,
                    HistoryMoment(1),
                    &mut MemoryClipboard::default(),
                )
                .expect("undo paste");
            assert_eq!(editor.source(), "");
        }

        #[test]
        fn a_raw_paste_stops_before_the_following_shortcut() {
            let shortcut = Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::CONTROL,
            ));
            let queued = "pattern one\npattern two"
                .chars()
                .skip(1)
                .map(|character| {
                    let code = match character {
                        '\n' => KeyCode::Enter,
                        character => KeyCode::Char(character),
                    };
                    Event::Key(crossterm::event::KeyEvent::new(code, KeyModifiers::NONE))
                })
                .chain(std::iter::once(shortcut.clone()))
                .collect::<VecDeque<_>>();
            let queued = RefCell::new(queued);
            let events = read_raw_paste_batch(
                Event::Key(crossterm::event::KeyEvent::new(
                    KeyCode::Char('p'),
                    KeyModifiers::NONE,
                )),
                |_| Ok(!queued.borrow().is_empty()),
                || Ok(queued.borrow_mut().pop_front().expect("polled event")),
            )
            .expect("collect key stream");

            assert_eq!(
                events,
                vec![Event::Paste("pattern one\npattern two".into()), shortcut]
            );
        }

        #[test]
        fn a_short_delivery_gap_after_the_raw_cap_keeps_pasted_enter_as_text() {
            let mut continuation = true;
            assert!(
                poll_after_input(&mut continuation, |wait| {
                    assert_eq!(
                        wait, RAW_PASTE_IDLE,
                        "allow the rest of the Windows delivery to arrive"
                    );
                    Ok(true)
                })
                .unwrap()
            );
            let events = read_raw_paste_chunk(
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                &mut continuation,
                16,
                32,
                |_| Ok(false),
                || unreachable!(),
            )
            .unwrap();
            assert_eq!(events, [Event::Paste("\n".into())]);
            assert!(!continuation);
            assert!(
                !poll_after_input(&mut continuation, |wait| {
                    assert_eq!(wait, Duration::ZERO);
                    Ok(false)
                })
                .unwrap()
            );
            continuation = true;
            assert!(
                !poll_after_input(&mut continuation, |wait| {
                    assert_eq!(wait, RAW_PASTE_IDLE);
                    Ok(false)
                })
                .unwrap()
            );
            assert!(!continuation, "a real idle gap restores ordinary Enter");
        }

        const CAT: &str = "           /\\_/\\  \n          ( o.o )  \n           > ^ <   \n          /|   |\\  \n         (_|   |_) \n";

        fn art_prompt(app: &mut App) {
            app.frame = Rect::new(0, 0, 140, 40);
            app.prefs.visuals[0].widgets = vec![WidgetSpec::default_art()];
            app.open_viz_dock(0, true);
            app.regions.viz[0] = Rect::new(104, 1, 36, 38);
            app.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )))
            .unwrap();
            assert!(app.viz_docks[0].as_ref().unwrap().prompt.is_some());
        }

        /// Exercise the same owner gate and bounded collector as the live loop,
        /// including Windows release records and a deliberately small raw cap.
        fn raw_windows_paste(app: &mut App, text: &str, key_limit: usize, terminal_limit: usize) {
            raw_windows_paste_with(app, text, key_limit, terminal_limit, |app, event| {
                app.handle_terminal_event(event).unwrap();
            });
        }

        fn raw_windows_paste_with(
            app: &mut App,
            text: &str,
            key_limit: usize,
            terminal_limit: usize,
            mut dispatch: impl FnMut(&mut App, Event),
        ) {
            let queued = RefCell::new(
                text.chars()
                    .flat_map(|character| {
                        let code = match character {
                            '\r' | '\n' => KeyCode::Enter,
                            '\t' => KeyCode::Tab,
                            character => KeyCode::Char(character),
                        };
                        [KeyEventKind::Press, KeyEventKind::Release].map(|kind| {
                            Event::Key(KeyEvent::new_with_kind(code, KeyModifiers::NONE, kind))
                        })
                    })
                    .collect::<VecDeque<_>>(),
            );
            let mut continuation = false;
            while !queued.borrow().is_empty() {
                for _ in 0..MAX_EVENTS_PER_TURN {
                    let Some(first) = queued.borrow_mut().pop_front() else {
                        break;
                    };
                    if app.accepts_raw_paste() && is_plain_text_key_event(&first) {
                        for event in read_raw_paste_chunk(
                            first,
                            &mut continuation,
                            key_limit,
                            terminal_limit,
                            |_| Ok(!queued.borrow().is_empty()),
                            || Ok(queued.borrow_mut().pop_front().unwrap()),
                        )
                        .unwrap()
                        {
                            dispatch(app, event);
                        }
                    } else {
                        if !matches!(&first, Event::Key(key) if key.kind == KeyEventKind::Release) {
                            continuation = false;
                        }
                        dispatch(app, first);
                    }
                }
            }
        }

        #[test]
        fn cat_paste_keeps_every_line_in_the_art_prompt_until_explicit_enter() {
            for raw in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let mut app = app_over(directory.path());
                let before = app.editor().source();
                art_prompt(&mut app);
                assert!(app.accepts_raw_paste());
                assert!(CAT.chars().count() > MAX_EVENTS_PER_TURN);
                if raw {
                    raw_windows_paste(
                        &mut app,
                        CAT,
                        MAX_RAW_PASTE_KEY_EVENTS_PER_TURN,
                        MAX_RAW_PASTE_TERMINAL_EVENTS_PER_TURN,
                    );
                } else {
                    app.handle_terminal_event(Event::Paste(CAT.into())).unwrap();
                }
                assert_eq!(app.focus, Focus::Panel(PanelKind::Viz));
                assert_eq!(
                    app.viz_docks[0]
                        .as_ref()
                        .unwrap()
                        .prompt
                        .as_ref()
                        .unwrap()
                        .path,
                    CAT
                );
                assert_eq!(app.editor().source(), before);
                assert_eq!(
                    app.prefs.visuals[0].widgets[0].text, None,
                    "pasted Enter never commits"
                );
                app.handle_terminal_event(Event::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE,
                )))
                .unwrap();
                assert!(app.viz_docks[0].as_ref().unwrap().prompt.is_none());
                assert_eq!(app.prefs.visuals[0].widgets[0].text.as_deref(), Some(CAT));
                assert_eq!(app.editor().source(), before);
            }
        }

        /// Ctrl+V over the visuals dock, with no art prompt open, used to fire
        /// `a` and Enter as "add a widget". A drawing of ansi art became a
        /// stack of gibberish widgets. The paste is ignored.
        #[test]
        fn a_paste_over_the_visuals_dock_is_ignored() {
            for raw in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let mut app = app_over(directory.path());
                app.frame = Rect::new(0, 0, 140, 40);
                app.prefs.visuals[0].widgets = vec![WidgetSpec::default_art()];
                app.open_viz_dock(0, true);
                app.regions.viz[0] = Rect::new(104, 1, 36, 38);
                app.focus = Focus::Panel(PanelKind::Viz);
                assert!(
                    app.accepts_raw_paste(),
                    "a Ctrl+V flood must coalesce, not type into the dock"
                );
                let before = app.editor().source();
                let widgets = app.prefs.visuals[0].widgets.clone();
                let drawing = "a\na\n/:\\x1b[0m\n( o.o )\n";
                if raw {
                    raw_windows_paste(
                        &mut app,
                        drawing,
                        MAX_RAW_PASTE_KEY_EVENTS_PER_TURN,
                        MAX_RAW_PASTE_TERMINAL_EVENTS_PER_TURN,
                    );
                } else {
                    app.handle_terminal_event(Event::Paste(drawing.into()))
                        .unwrap();
                }
                assert_eq!(
                    app.prefs.visuals[0].widgets, widgets,
                    "no widget was added from the paste"
                );
                assert!(
                    app.viz_docks[0].as_ref().unwrap().prompt.is_none(),
                    "the art prompt did not open"
                );
                assert_eq!(
                    app.viz_docks[0].as_ref().unwrap().adding,
                    None,
                    "add-a-widget did not open"
                );
                assert_eq!(
                    app.editor().source(),
                    before,
                    "the paste is ignored, not dumped into the score"
                );
            }
        }

        /// Ctrl+V over the set panel, with no rename/open prompt, used to fire
        /// `n` and Enter as "new session" / "open this line". The paste is ignored.
        #[test]
        fn a_paste_over_the_set_panel_is_ignored() {
            for raw in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let mut app = app_over(directory.path());
                app.open_set_panel(true);
                assert_eq!(app.focus, Focus::Panel(PanelKind::Set));
                assert!(
                    app.accepts_raw_paste(),
                    "a Ctrl+V flood must coalesce, not type into the set panel"
                );
                let before = app.editor().source();
                let scenes = app.scenes.len();
                let lines = app.set_panel.as_ref().map(|panel| panel.lines.len());
                let drawing = "n\nn\nnew set\n";
                if raw {
                    raw_windows_paste(
                        &mut app,
                        drawing,
                        MAX_RAW_PASTE_KEY_EVENTS_PER_TURN,
                        MAX_RAW_PASTE_TERMINAL_EVENTS_PER_TURN,
                    );
                } else {
                    app.handle_terminal_event(Event::Paste(drawing.into()))
                        .unwrap();
                }
                assert!(app.set_prompt.is_none(), "no set prompt opened");
                assert_eq!(app.scenes.len(), scenes, "no session was created");
                assert_eq!(
                    app.set_panel.as_ref().map(|panel| panel.lines.len()),
                    lines,
                    "the set list was not rewritten by the paste"
                );
                assert_eq!(
                    app.editor().source(),
                    before,
                    "the paste is ignored, not dumped into the score"
                );
            }
        }

        /// ConPTY delivers a dropped folder as one native key record per
        /// character. Over the set panel, the studio joins the records into one
        /// paste and imports the folder; no character runs as a panel shortcut.
        #[test]
        fn a_raw_folder_drop_over_the_set_panel_imports_instead_of_starting_a_session() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.options.recording = Some(RecordingOptions {
                mode: rustel_runtime::session_log::SessionMode::Normal,
                file: None,
            });
            let target = directory.path().join("new sounds");
            std::fs::create_dir(&target).unwrap();
            app.open_set_panel(true);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Set));
            assert!(app.set_prompt.is_none());
            let before = app.editor().source();
            let scenes = app.scenes.len();
            assert!(app.recorder.is_none(), "no tape before the drop");

            // Quoted, the way a terminal hands over a path with a space in it
            // on either platform; a newline on the end, the way some append one.
            let dropped = format!("\"{}\"\n", target.display());
            raw_windows_paste(
                &mut app,
                &dropped,
                MAX_RAW_PASTE_KEY_EVENTS_PER_TURN,
                MAX_RAW_PASTE_TERMINAL_EVENTS_PER_TURN,
            );

            assert!(
                app.recorder.is_none(),
                "the `n` in the path started a session"
            );
            assert_eq!(app.scenes.len(), scenes, "Enter opened a set line");
            assert!(app.set_prompt.is_none());
            assert_eq!(app.prefs.sample_sources.len(), 1, "the folder was imported");
            assert_eq!(
                app.prefs.sample_sources[0].spec,
                target.display().to_string()
            );
            assert_eq!(app.editor().source(), before);
        }

        /// `C:\test` used to miss the eight-key paste threshold, while
        /// `c:\test2` worked. The short burst must reach the drop handler as one
        /// paste instead of executing its characters as panel shortcuts.
        #[test]
        fn short_windows_folder_drops_import_without_opening_unrequested_prompts() {
            for dropped in [r"C:\test", r"c:\test2"] {
                for focus in [
                    Focus::Editor,
                    Focus::Panel(PanelKind::Set),
                    Focus::Panel(PanelKind::Reference),
                    Focus::Panel(PanelKind::Settings),
                    Focus::Panel(PanelKind::Viz),
                ] {
                    let directory = tempfile::tempdir().unwrap();
                    let mut app = app_over(directory.path());
                    let target = directory.path().join("sample folder");
                    std::fs::create_dir(&target).unwrap();
                    app.options.recording = Some(RecordingOptions {
                        mode: rustel_runtime::session_log::SessionMode::Normal,
                        file: None,
                    });
                    match focus {
                        Focus::Panel(PanelKind::Set) => app.open_set_panel(true),
                        Focus::Panel(PanelKind::Reference) => {
                            app.reference_panel =
                                Some(ReferencePanel::samples(&app.reference, Vec::new()));
                        }
                        Focus::Panel(PanelKind::Settings) => {
                            let mut sheet = SettingsSheet::default();
                            sheet.show_page(settings::SettingsPage::Sources);
                            app.settings_sheet = Some(sheet);
                        }
                        Focus::Panel(PanelKind::Viz) => {
                            app.frame = Rect::new(0, 0, 140, 40);
                            app.prefs.visuals[0].widgets = vec![WidgetSpec::default_art()];
                            app.open_viz_dock(0, true);
                        }
                        _ => {}
                    }
                    app.focus = focus;
                    assert!(app.accepts_raw_paste(), "{focus:?}");
                    let before = app.editor().source();
                    let scenes = app.scenes.len();
                    let mut pastes = 0;

                    // Keep the exact seven/eight-character Windows delivery at
                    // the input boundary, then map the resulting paste to an
                    // isolated real folder. The test must never inspect, import,
                    // or create a user's C:\test/C:\test2 directories.
                    raw_windows_paste_with(
                        &mut app,
                        dropped,
                        MAX_RAW_PASTE_KEY_EVENTS_PER_TURN,
                        MAX_RAW_PASTE_TERMINAL_EVENTS_PER_TURN,
                        |app, event| {
                            assert_eq!(event, Event::Paste(dropped.into()), "{focus:?}");
                            pastes += 1;
                            app.handle_terminal_event(Event::Paste(format!(
                                "\"{}\"",
                                target.display()
                            )))
                            .unwrap();
                        },
                    );

                    assert_eq!(pastes, 1);
                    assert_eq!(app.prefs.sample_sources.len(), 1);
                    assert_eq!(
                        app.prefs.sample_sources[0].spec,
                        target.display().to_string()
                    );
                    assert!(app.set_prompt.is_none(), "{focus:?}: no prompt requested");
                    assert!(app.recorder.is_none(), "{focus:?}: no session requested");
                    assert_eq!(app.scenes.len(), scenes);
                    assert_eq!(app.editor().source(), before);
                    if let Some(panel) = &app.reference_panel {
                        assert!(panel.sound_query.is_empty());
                    }
                    if let Some(dock) = &app.viz_docks[0] {
                        assert!(dock.prompt.is_none());
                        assert!(dock.adding.is_none());
                    }
                }
            }
        }

        #[test]
        fn short_windows_paths_stay_in_deliberately_opened_path_prompts() {
            for text in [r"C:\test", r"c:\test2"] {
                for prompt in [SetPrompt::OpenSet, SetPrompt::AddSampleSource] {
                    let directory = tempfile::tempdir().unwrap();
                    let mut app = app_over(directory.path());
                    let before = app.editor().source();
                    app.open_set_prompt(prompt);
                    app.set_prompt.as_mut().unwrap().1.offer("replace this");

                    raw_windows_paste(
                        &mut app,
                        text,
                        MAX_RAW_PASTE_KEY_EVENTS_PER_TURN,
                        MAX_RAW_PASTE_TERMINAL_EVENTS_PER_TURN,
                    );

                    assert_eq!(app.set_prompt.as_ref().unwrap().1.path, text);
                    assert_eq!(app.focus, Focus::Panel(PanelKind::Set));
                    assert!(app.prefs.sample_sources.is_empty());
                    assert_eq!(app.editor().source(), before);
                }
            }
        }

        #[test]
        fn a_trailing_pasted_enter_after_either_raw_cap_never_submits_the_prompt() {
            // 65 chunks cross the ordinary event-turn limit too. The last
            // chunk is just Enter, with its Windows release record beside it.
            let text = format!("{}\n", "x".repeat(16 * (MAX_EVENTS_PER_TURN + 1)));
            for (key_limit, terminal_limit) in [(16, 64), (64, 32)] {
                let directory = tempfile::tempdir().unwrap();
                let mut app = app_over(directory.path());
                let before = app.editor().source();
                art_prompt(&mut app);
                raw_windows_paste(&mut app, &text, key_limit, terminal_limit);
                assert_eq!(
                    app.viz_docks[0]
                        .as_ref()
                        .unwrap()
                        .prompt
                        .as_ref()
                        .unwrap()
                        .path,
                    text
                );
                assert_eq!(app.editor().source(), before);
                assert_eq!(app.focus, Focus::Panel(PanelKind::Viz));
            }
        }

        #[test]
        fn multiline_paste_edits_scene_and_file_names_without_confirming_them() {
            for raw in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let mut app = app_over(directory.path());
                let before = app.editor().source();
                app.begin_rename();
                let text = "next\nscene\n";
                if raw {
                    raw_windows_paste(&mut app, text, 64, 128);
                } else {
                    app.handle_terminal_event(Event::Paste(text.into()))
                        .unwrap();
                }
                assert!(
                    matches!(&app.strip_mode, SceneStripMode::Renaming(name) if name == "next scene ")
                );
                assert_eq!(app.scenes.current().name(), "first");
                assert_eq!(app.editor().source(), before);

                app.strip_mode = SceneStripMode::Idle;
                app.set_prompt = Some((SetPrompt::OpenSet, FilePicker::naming("open", "opens")));
                app.set_prompt.as_mut().unwrap().1.offer("offered name");
                app.focus = Focus::Panel(PanelKind::Set);
                let path = "C:\\Music\\\nnew set\\take.strudel\n";
                if raw {
                    raw_windows_paste(&mut app, path, 64, 128);
                } else {
                    app.handle_terminal_event(Event::Paste(path.into()))
                        .unwrap();
                }
                assert_eq!(
                    app.set_prompt.as_ref().unwrap().1.path,
                    "C:\\Music\\ new set\\take.strudel "
                );
                assert_eq!(app.editor().source(), before);
                assert_eq!(app.focus, Focus::Panel(PanelKind::Set));
            }
        }

        #[test]
        fn real_paths_pasted_into_fields_do_not_import_or_open_them() {
            for raw in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let mut app = app_over(directory.path());
                let target = directory.path().join("pasted folder");
                std::fs::create_dir(&target).unwrap();
                // Quoted drops also preserve native Windows path separators.
                let text = format!("\"{}\"", target.display());
                assert!(dropped_paths(&text).is_some());
                let before = app.editor().source();
                for prompt in [
                    SetPrompt::OpenSet,
                    SetPrompt::SetsFolder,
                    SetPrompt::AddSampleSource,
                ] {
                    app.open_set_prompt(prompt);
                    app.set_prompt.as_mut().unwrap().1.offer("replace this");
                    if raw {
                        raw_windows_paste(&mut app, &text, 4096, 8192);
                    } else {
                        app.handle_terminal_event(Event::Paste(text.clone()))
                            .unwrap();
                    }
                    assert_eq!(app.set_prompt.as_ref().unwrap().1.path, text);
                    assert_eq!(app.focus, Focus::Panel(PanelKind::Set));
                    assert!(app.prefs.sample_sources.is_empty());
                    assert_eq!(app.editor().source(), before);
                }
                app.set_prompt = None;
                art_prompt(&mut app);
                if raw {
                    raw_windows_paste(&mut app, &text, 4096, 8192);
                } else {
                    app.handle_terminal_event(Event::Paste(text.clone()))
                        .unwrap();
                }
                assert_eq!(
                    app.viz_docks[0]
                        .as_ref()
                        .unwrap()
                        .prompt
                        .as_ref()
                        .unwrap()
                        .path,
                    text
                );
                assert!(app.prefs.sample_sources.is_empty());
                assert_eq!(app.editor().source(), before);
            }
        }

        /// The bug this was written for: a folder dropped on the window while
        /// the samples browser held the keyboard was typed into its search box
        /// instead of being imported, one re-rank of the whole catalogue per
        /// character of the path. The browser is where you look to see whether
        /// a drop landed, so it is the panel most likely to be focused when
        /// you drop the next one.
        ///
        /// Also covers the boundary: a path that names nothing is not a drop,
        /// and the box keeps it.
        #[test]
        fn a_folder_drop_lands_while_the_sample_search_box_has_the_keyboard() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let target = directory.path().join("dropped folder");
            std::fs::create_dir(&target).unwrap();
            app.reference_panel = Some(ReferencePanel::samples(&app.reference, Vec::new()));
            app.focus = Focus::Panel(PanelKind::Reference);
            assert!(
                app.accepts_raw_paste(),
                "the search box would otherwise own the paste"
            );
            let before = app.editor().source();

            // A terminal delivers a dropped path with a space quoted on Windows
            // and backslash-escaped on Unix. On Windows the backslash is the
            // path separator, so it cannot escape a space.
            let dropped = if cfg!(windows) {
                format!("'{}'", target.display())
            } else {
                target.display().to_string().replace(' ', "\\ ")
            };
            app.handle_terminal_event(Event::Paste(dropped)).unwrap();

            assert_eq!(app.prefs.sample_sources.len(), 1);
            assert_eq!(
                app.prefs.sample_sources[0].spec,
                target.display().to_string()
            );
            assert_eq!(
                app.reference_panel.as_ref().unwrap().sound_query,
                "",
                "and nothing was typed into the box"
            );
            assert_eq!(app.editor().source(), before);
            // The import opens the Sources page on the row it made, and the
            // focus moves there.
            assert!(matches!(app.focus, Focus::Panel(PanelKind::Settings)));
            assert_eq!(
                app.settings_sheet.as_ref().map(|sheet| sheet.selected),
                Some(settings::SOURCE_CONTROL_COUNT),
                "the sheet selects the import it just made"
            );

            // A path that names nothing is text, but only where a text field
            // holds the keyboard. On the browser, the box keeps a bounded
            // amount of it.
            app.focus_panel(PanelKind::Reference);
            let absent = directory.path().join("no-such-folder");
            app.handle_terminal_event(Event::Paste(absent.display().to_string()))
                .unwrap();
            assert_eq!(app.prefs.sample_sources.len(), 1);
            let dropped = absent.display().to_string();
            let typed = &app.reference_panel.as_ref().unwrap().sound_query;
            assert!(dropped.starts_with(typed.as_str()), "{typed}");
            // How long a temp path is depends on the machine, so the bound is
            // whichever of the two runs out first.
            assert_eq!(
                typed.chars().count(),
                dropped
                    .chars()
                    .count()
                    .min(crate::reference::MAX_QUERY_CHARS)
            );
            assert_eq!(app.editor().source(), before);
        }

        #[test]
        fn a_folder_drop_on_the_sources_page_still_imports_it() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let target = directory.path().join("dropped folder");
            std::fs::create_dir(&target).unwrap();
            app.settings_sheet = Some(SettingsSheet::default());
            app.settings_sheet
                .as_mut()
                .unwrap()
                .show_page(settings::SettingsPage::Sources);
            app.focus = Focus::Panel(PanelKind::Settings);
            assert!(
                app.accepts_raw_paste(),
                "a flood must coalesce so it is not typed as settings keys"
            );
            let before = app.editor().source();
            app.handle_terminal_event(Event::Paste(format!("\"{}\"", target.display())))
                .unwrap();
            assert_eq!(app.prefs.sample_sources.len(), 1);
            assert_eq!(
                app.prefs.sample_sources[0].spec,
                target.display().to_string()
            );
            assert_eq!(app.editor().source(), before);
        }

        /// The global take chord stays the take chord even over the samples
        /// browser, and the browser itself no longer aliases banks: that
        /// moved to the Sources page (`r` on an import row), so no unshifted
        /// ctrl+r bank rename exists to collide with the recording chord.
        #[test]
        fn the_recording_chord_stays_distinct_and_the_browser_does_not_alias() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.reference_panel = Some(ReferencePanel::samples(
                &app.reference,
                vec![rustel_runtime::samples::SoundEntry {
                    name: "my_kick".into(),
                    variants: 1,
                    variant_names: Vec::new(),
                    origin: rustel_runtime::samples::SoundOrigin::Global,
                    category: rustel_runtime::samples::SoundCategory::Drums,
                    location: None,
                    import: None,
                }],
            ));
            app.focus = Focus::Panel(PanelKind::Reference);
            assert_eq!(
                app.reference_panel
                    .as_ref()
                    .unwrap()
                    .selected_bank()
                    .unwrap()
                    .0,
                "my_kick"
            );
            // A pending take makes the global action observable without opening
            // an audio recording or depending on worker queue timing.
            app.pending_recording = Some(42);
            app.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Char('R'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            )))
            .unwrap();
            assert!(app.status.contains("engine busy"), "{}", app.status);
            assert!(app.set_prompt.is_none());
            assert_eq!(app.pending_recording, Some(42));
            // An assigned scene shortcut still works over the browser.
            app.keybinds.learn(
                BindAction::RenameScene,
                crate::keybinds::KeyCombo::parse("ctrl+r"),
            );
            app.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Char('r'),
                KeyModifiers::CONTROL,
            )))
            .unwrap();
            assert!(matches!(app.strip_mode, SceneStripMode::Renaming(_)));
            assert!(app.set_prompt.is_none());
        }

        /// The Sources-side alias: `r` on a user import offers that import's
        /// banks, and the rename prompt takes the picked one. Ctrl+Shift+R -
        /// the recording chord - is still not it, even here.
        #[test]
        fn a_source_row_r_offers_its_banks_and_recording_stays_distinct() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let folder = directory.path().join("my drums");
            std::fs::create_dir(&folder).unwrap();
            std::fs::write(folder.join("kick.wav"), []).unwrap();
            app.add_sample_source(&folder.display().to_string());
            // The walk runs on the worker: keep settling until the receipt.
            for _ in 0..30 {
                if app.status.ends_with("1 sound(s) imported") {
                    break;
                }
                app.settle_sample_sources();
            }

            let mut sheet = SettingsSheet::default();
            sheet.show_page(settings::SettingsPage::Sources);
            app.settings_sheet = Some(sheet);
            app.focus = Focus::Panel(PanelKind::Settings);
            // The keyboard walks the one list: the four cache controls, then
            // the imports, then the packs. `r` on the import row opens the
            // alias prompt for its bank.
            for _ in 0..settings::SOURCE_CONTROL_COUNT {
                app.handle_terminal_event(Event::Key(KeyEvent::new(
                    KeyCode::Down,
                    KeyModifiers::NONE,
                )))
                .unwrap();
            }
            assert!(app.handle_settings_key(KeyCode::Char('r'), false, false));
            assert!(matches!(app.set_prompt, Some((SetPrompt::RenameBank, _))));
            assert_eq!(app.renaming_bank.as_deref(), Some("my drums"));

            // A pending take makes the global action observable without opening
            // an audio recording or depending on worker queue timing.
            app.pending_recording = Some(42);
            app.set_prompt = None;
            app.renaming_bank = None;
            app.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Char('R'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            )))
            .unwrap();
            assert!(app.status.contains("engine busy"), "{}", app.status);
            assert!(app.set_prompt.is_none());
            assert_eq!(app.pending_recording, Some(42));
        }

        #[test]
        fn picker_paste_replaces_unicode_selection_once_and_keeps_the_caret_correct() {
            let mut picker = FilePicker::text("art", "writes");
            picker.offer("old λ text");
            picker.paste_multiline("猫\n  /\\");
            assert_eq!(picker.path, "猫\n  /\\");
            assert_eq!(picker.caret, 6);
            picker.caret_to(1, false);
            picker.caret_to(4, true);
            picker.paste_multiline("é\n");
            assert_eq!(picker.path, "猫é\n/\\");
            assert_eq!(picker.caret, 3);
            assert_eq!(picker.anchor, 3);
        }

        #[test]
        fn theme_save_prompt_owns_a_raw_paste_and_keeps_its_embedded_enter_as_text() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let before = app.editor().source();
            let mut editor = crate::theme_editor::ThemeEditor::open(
                &app.theme,
                &app.theme,
                "draft".into(),
                false,
            );
            editor.saving = Some(crate::theme_editor::SaveSheet {
                name: String::new(),
                keep: false,
                note: None,
                closing: false,
            });
            app.theme_editor = Some(editor);
            app.focus = Focus::Panel(PanelKind::ThemeEditor);
            raw_windows_paste(&mut app, "night\nforest\n", 64, 128);
            assert_eq!(
                app.theme_editor
                    .as_ref()
                    .unwrap()
                    .saving
                    .as_ref()
                    .unwrap()
                    .name,
                "night forest "
            );
            assert_eq!(app.editor().source(), before);
        }

        #[test]
        fn a_paste_in_a_focused_panel_without_a_text_field_cannot_edit_the_score() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let before = app.editor().source();
            app.focus = Focus::Panel(PanelKind::Devices);
            app.panel = Some(DevicePanel::default());
            app.handle_terminal_event(Event::Paste(CAT.into())).unwrap();
            assert_eq!(app.editor().source(), before);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Devices));
        }
    }
}
