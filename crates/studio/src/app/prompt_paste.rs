//! Paste ownership for text fields outside the score, and the reader that
//! gathers a paste a legacy terminal sends as ordinary keys into one chunk
//! before its Enter reaches a prompt.

use super::super::file_picker::single_line_paste;
use super::super::theme_editor::EditorTab;
use super::*;

impl App {
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

    /// The paste chord, for the text field that has the keyboard. `true`
    /// when a prompt took the clipboard, or the clipboard could not be read
    /// and the score takes no paste to fall back on.
    pub(super) fn paste_shortcut(&mut self, key: &KeyEvent, alt: bool) -> bool {
        // A paste shortcut belongs to the text field that currently has
        // the keyboard.  Bracketed terminal paste already takes this
        // route in `route_paste`; the internal clipboard shortcut must do the same
        // instead of falling through to the hidden score editor.
        if !alt && self.keybinds.action_for(key) == Some(BindAction::Paste) {
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
