//! Vim key policy outside the editor.
//!
//! The list aliases and the modal search boxes live here. The routing code
//! asks this module for one answer. A new text surface implements
//! [`VimTextSurface`] and adds one arm to [`surface_mut`]; the key rules do
//! not change.

use super::*;

/// A panel search box that rests in Vim command mode.
pub(super) trait VimTextSurface {
    fn panel_kind(&self) -> PanelKind;
    fn text_wanted(&self) -> bool;
    fn insert(&self) -> bool;
    fn set_insert(&mut self, insert: bool);
    fn has_preview(&self) -> bool;
}

impl VimTextSurface for ReferencePanel {
    fn panel_kind(&self) -> PanelKind {
        PanelKind::Reference
    }

    fn text_wanted(&self) -> bool {
        self.wants_text()
    }

    fn insert(&self) -> bool {
        self.vim_insert()
    }

    fn set_insert(&mut self, insert: bool) {
        self.set_vim_insert(insert);
    }

    fn has_preview(&self) -> bool {
        !matches!(self.preview(), PanelAction::Nothing)
    }
}

/// The focused surface with a search box. This is the registry: add a new
/// surface here and implement [`VimTextSurface`] for it.
fn surface_mut(app: &mut App) -> Option<&mut dyn VimTextSurface> {
    match app.focus {
        Focus::Panel(PanelKind::Reference) => app
            .reference_panel
            .as_mut()
            .map(|panel| panel as &mut dyn VimTextSurface),
        _ => None,
    }
}

fn surface_ref(app: &App) -> Option<&dyn VimTextSurface> {
    match app.focus {
        Focus::Panel(PanelKind::Reference) => app
            .reference_panel
            .as_ref()
            .map(|panel| panel as &dyn VimTextSurface),
        _ => None,
    }
}

/// Vim list keys are on, and the editor uses Vim keys.
fn enabled(app: &App) -> bool {
    app.ui_settings.vim_list_navigation
        && app.ui_settings.editor_keybindings == crate::editor::EditorKeybindings::Vim
}

/// An anchored completion always types: the search is the word being
/// completed, not a list to walk first.
fn anchored(app: &App) -> bool {
    app.focus == Focus::Panel(PanelKind::Reference) && app.reference_anchor.is_some()
}

/// Whether the panel's text field is typing. `None` when Vim text modes do
/// not apply, so the caller keeps the ordinary answer.
pub(super) fn surface_typing(app: &App, kind: PanelKind) -> Option<bool> {
    if !enabled(app) || anchored(app) {
        return None;
    }
    match kind {
        PanelKind::Reference => {
            surface_ref(app).map(|surface| surface.text_wanted() && surface.insert())
        }
        _ => None,
    }
}

/// A paste into a Vim text surface enters Insert first, so a pasted word
/// lands in the box that is waiting for it.
pub(super) fn enter_insert(app: &mut App) {
    if !enabled(app) || anchored(app) {
        return;
    }
    if let Some(surface) = surface_mut(app)
        && surface.text_wanted()
    {
        surface.set_insert(true);
    }
}

/// What the modal keys decided, applied after the panel borrow ends.
enum TextAction {
    Nothing,
    Message(&'static str),
    Arrow(PanelKind, KeyCode),
}

/// The modal keys of a search box. `i` and `a` enter Insert, Esc returns to
/// Command, and Command keeps j k h l for the list and every other letter
/// out of the query.
pub(super) fn handle_text_key(app: &mut App, key: &KeyEvent) -> Result<bool, RuntimeError> {
    if key.kind == KeyEventKind::Release || !enabled(app) || anchored(app) {
        return Ok(false);
    }
    let alias = app.list_navigation_alias(key);
    let action = {
        let Some(surface) = surface_mut(app) else {
            return Ok(false);
        };
        if !surface.text_wanted() {
            return Ok(false);
        }
        if surface.insert() {
            if key.code == KeyCode::Esc && key.modifiers.is_empty() {
                surface.set_insert(false);
                TextAction::Message("command mode - i or a types")
            } else {
                // Insert takes every key, including the list letters.
                TextAction::Nothing
            }
        } else if !key.modifiers.is_empty() {
            TextAction::Nothing
        } else {
            match key.code {
                KeyCode::Char('i' | 'a') => {
                    surface.set_insert(true);
                    TextAction::Message("insert mode - Esc returns to command")
                }
                // Space auditions the selected row. With nothing to hear,
                // it is text like every other letter, and Command keeps it
                // out.
                KeyCode::Char(' ') if surface.has_preview() => TextAction::Nothing,
                KeyCode::Char(' ') | KeyCode::Backspace | KeyCode::Delete => {
                    TextAction::Message("press i or a to type")
                }
                KeyCode::Char(character) if !character.is_control() => {
                    if let Some(alias) = alias {
                        TextAction::Arrow(surface.panel_kind(), alias.code)
                    } else {
                        TextAction::Message("press i or a to type")
                    }
                }
                _ => TextAction::Nothing,
            }
        }
    };
    match action {
        TextAction::Nothing => Ok(false),
        TextAction::Message(message) => {
            app.status = message.into();
            app.dirty_frame = true;
            Ok(true)
        }
        TextAction::Arrow(kind, code) => app.dispatch_panel_key(kind, code, false, false, false),
    }
}

impl App {
    /// The arrow a plain j, k, h, or l stands in for while Vim list keys
    /// are on.
    pub(super) fn list_navigation_alias(&self, key: &KeyEvent) -> Option<KeyEvent> {
        if !self.ui_settings.vim_list_navigation
            || self.ui_settings.editor_keybindings != crate::editor::EditorKeybindings::Vim
            || key.kind == KeyEventKind::Release
            || !key.modifiers.is_empty()
        {
            return None;
        }
        let code = match key.code {
            KeyCode::Char('j') => KeyCode::Down,
            KeyCode::Char('k') => KeyCode::Up,
            KeyCode::Char('h') => KeyCode::Left,
            KeyCode::Char('l') => KeyCode::Right,
            _ => return None,
        };
        Some(KeyEvent::new_with_kind(code, KeyModifiers::NONE, key.kind))
    }
}

/// The panels whose rows j k h l walk.
pub(super) fn panel_takes_list_navigation(kind: PanelKind) -> bool {
    matches!(
        kind,
        PanelKind::Settings
            | PanelKind::Set
            | PanelKind::Devices
            | PanelKind::Log
            | PanelKind::Jobs
            | PanelKind::Reference
            | PanelKind::Memory
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::support::*;

    fn vim_app() -> (tempfile::TempDir, App) {
        let directory = tempfile::tempdir().expect("temp dir");
        let (mut app, _scene) = plain_app(directory.path(), "// one\n");
        app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
        app.ui_settings.vim_list_navigation = true;
        app.sync_editor_keybindings();
        app.reference_anchor = None;
        let panel = ReferencePanel::browse(&app.reference);
        app.set_reference_panel(Some(panel));
        app.focus_panel(PanelKind::Reference);
        app.reference_panel
            .as_mut()
            .expect("panel")
            .select_tab(Tab::Chords);
        (directory, app)
    }

    fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
        app.handle_terminal_event(Event::Key(KeyEvent::new(code, modifiers)))
            .expect("key handled");
    }

    #[test]
    fn command_keeps_letters_out_and_walks_with_jkhl() {
        let (_directory, mut app) = vim_app();
        let selected = |app: &App| app.reference_panel.as_ref().expect("panel").chord_selected;
        let query = |app: &App| {
            app.reference_panel
                .as_ref()
                .expect("panel")
                .chord_query
                .clone()
        };
        let before = selected(&app);

        press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_ne!(selected(&app), before, "j walks the list");
        assert_eq!(query(&app), "", "j did not type");

        press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(query(&app), "", "a letter waits for Insert");
        assert!(app.status.contains("press i or a"), "{}", app.status);

        press(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(query(&app), "x", "Insert types");
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        let before = selected(&app);
        press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_ne!(selected(&app), before, "Esc returns to Command");
        assert_eq!(query(&app), "x", "Command still does not type");
    }

    #[test]
    fn a_enters_insert_and_the_gate_off_keeps_typing() {
        let (_directory, mut app) = vim_app();
        press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(app.reference_panel.as_ref().expect("panel").vim_insert());
        press(&mut app, KeyCode::Char('m'), KeyModifiers::NONE);
        assert_eq!(
            app.reference_panel.as_ref().expect("panel").chord_query,
            "m"
        );

        app.reference_panel
            .as_mut()
            .expect("panel")
            .chord_query
            .clear();
        app.ui_settings.vim_list_navigation = false;
        press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(
            app.reference_panel.as_ref().expect("panel").chord_query,
            "j",
            "the gate off types again"
        );
    }
}
