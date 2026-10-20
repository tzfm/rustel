//! Vim key policy outside the editor.
//!
//! The list aliases and the modal search boxes live here. The routing code
//! asks this module for one answer. A new text surface implements
//! [`VimTextSurface`] and adds one arm to [`surface_mut`]; the key rules do
//! not change.
//!
//! [`enabled`] is the one switch. It off, no alias, modal key, hint, paste
//! or focus reset in this file applies anywhere.

use super::*;

/// A panel search box that rests in Vim command mode.
pub(super) trait VimTextSurface {
    fn panel_kind(&self) -> PanelKind;
    fn text_wanted(&self) -> bool;
    fn insert(&self) -> bool;
    fn set_insert(&mut self, insert: bool);
    /// Space has a meaning of its own here, so Command leaves it to the
    /// surface. A search that previews a row and a form that toggles a
    /// choice both say yes; a search that only types says no.
    fn space_command(&self) -> bool {
        false
    }
    /// The surface's own anchored completion, if it has one.
    fn anchored(&self) -> bool {
        false
    }
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

    fn space_command(&self) -> bool {
        !matches!(self.preview(), PanelAction::Nothing)
    }
}

impl VimTextSurface for ThemePicker {
    fn panel_kind(&self) -> PanelKind {
        PanelKind::Theme
    }

    fn text_wanted(&self) -> bool {
        true
    }

    fn insert(&self) -> bool {
        self.vim_insert()
    }

    fn set_insert(&mut self, insert: bool) {
        self.set_vim_insert(insert);
    }
}

impl VimTextSurface for ExportSheet {
    fn panel_kind(&self) -> PanelKind {
        PanelKind::Export
    }

    fn text_wanted(&self) -> bool {
        true
    }

    fn insert(&self) -> bool {
        self.vim_insert()
    }

    fn set_insert(&mut self, insert: bool) {
        self.set_vim_insert(insert);
    }

    fn space_command(&self) -> bool {
        self.space_changes_a_choice()
    }
}

/// The focused surface with a search box. Add a surface in one line here;
/// both lookups expand together so they cannot drift apart.
macro_rules! text_surfaces {
    ($($kind:pat => $field:ident),* $(,)?) => {
        fn surface_mut(app: &mut App) -> Option<&mut dyn VimTextSurface> {
            match app.focus {
                $(Focus::Panel($kind) => app.$field.as_mut().map(|panel| panel as &mut dyn VimTextSurface),)*
                _ => None,
            }
        }

        fn surface_ref(app: &App) -> Option<&dyn VimTextSurface> {
            match app.focus {
                $(Focus::Panel($kind) => app.$field.as_ref().map(|panel| panel as &dyn VimTextSurface),)*
                _ => None,
            }
        }
    };
}

text_surfaces! {
    PanelKind::Reference => reference_panel,
    PanelKind::Theme => theme_picker,
    PanelKind::Export => export_sheet,
}

/// Vim list keys are on, and the editor uses Vim keys.
pub(super) fn enabled(app: &App) -> bool {
    app.ui_settings.vim_list_navigation
        && app.ui_settings.editor_keybindings == crate::editor::EditorKeybindings::Vim
}

/// An anchored completion always types: the search is the word being
/// completed, not a list to walk first.
fn anchored(app: &App) -> bool {
    app.focus == Focus::Panel(PanelKind::Reference) && app.reference_anchor.is_some()
}

/// Whether the focused panel's text field is typing. `None` when Vim text
/// modes do not apply, so the caller keeps the ordinary answer.
pub(super) fn surface_typing(app: &App) -> Option<bool> {
    if !enabled(app) {
        return None;
    }
    let surface = surface_ref(app)?;
    if !surface.text_wanted() {
        return Some(false);
    }
    Some(anchored(app) || surface.anchored() || surface.insert())
}

/// A newly focused surface rests in Command mode. One hook serves every
/// registered surface, so focus handling does not grow a match of its own.
pub(super) fn focus_changed(app: &mut App) {
    if !enabled(app) {
        return;
    }
    if let Some(surface) = surface_mut(app) {
        surface.set_insert(false);
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
        && !surface.anchored()
    {
        surface.set_insert(true);
    }
}

/// What the modal keys decided. An arrow is dispatched by the routing code;
/// a message has already been shown.
pub(super) enum VimTextKey {
    Ignored,
    Consumed,
    Arrow(PanelKind, KeyCode),
}

enum TextAction {
    Ignored,
    Message(&'static str),
    Arrow(PanelKind, KeyCode),
}

/// The modal keys of a search box. `i` and `a` enter Insert, Esc returns to
/// Command, and Command keeps j k h l for the list and every other letter
/// out of the query.
pub(super) fn handle_text_key(app: &mut App, key: &KeyEvent) -> VimTextKey {
    if key.kind == KeyEventKind::Release || !enabled(app) {
        return VimTextKey::Ignored;
    }
    let alias = app.list_navigation_alias(key);
    let app_anchored = anchored(app);
    let action = {
        let Some(surface) = surface_mut(app) else {
            return VimTextKey::Ignored;
        };
        if !surface.text_wanted() || app_anchored || surface.anchored() {
            return VimTextKey::Ignored;
        }
        if surface.insert() {
            if key.code == KeyCode::Esc && key.modifiers.is_empty() {
                surface.set_insert(false);
                TextAction::Message("command mode - i or a types")
            } else {
                // Insert takes every key, including the list letters.
                TextAction::Ignored
            }
        } else if key.modifiers.intersects(
            KeyModifiers::CONTROL
                | KeyModifiers::SUPER
                | KeyModifiers::ALT
                | KeyModifiers::HYPER
                | KeyModifiers::META,
        ) {
            TextAction::Ignored
        } else {
            match key.code {
                KeyCode::Char('i' | 'a') => {
                    surface.set_insert(true);
                    TextAction::Message("insert mode - Esc returns to command")
                }
                // Space keeps its surface meaning, such as auditioning
                // the selected row or toggling a choice. Where it has
                // none, it is text and Command keeps it out.
                KeyCode::Char(' ') if surface.space_command() => TextAction::Ignored,
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
                _ => TextAction::Ignored,
            }
        }
    };
    match action {
        TextAction::Ignored => VimTextKey::Ignored,
        TextAction::Message(message) => {
            app.status = message.into();
            app.dirty_frame = true;
            VimTextKey::Consumed
        }
        TextAction::Arrow(kind, code) => VimTextKey::Arrow(kind, code),
    }
}

impl App {
    /// The arrow a plain j, k, h, or l stands in for while Vim list keys
    /// are on.
    pub(super) fn list_navigation_alias(&self, key: &KeyEvent) -> Option<KeyEvent> {
        if !enabled(self) || key.kind == KeyEventKind::Release || !key.modifiers.is_empty() {
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
            | PanelKind::Theme
            | PanelKind::Export
            | PanelKind::Memory
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::support::*;
    use crate::editor::{ByteOffset, Selection};

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

        press(&mut app, KeyCode::Char('X'), KeyModifiers::SHIFT);
        assert_eq!(query(&app), "", "a shifted letter waits for Insert");

        press(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(query(&app), "xj", "Insert types j instead of walking");
        app.reference_panel
            .as_mut()
            .expect("panel")
            .chord_query
            .clear();
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        let before = selected(&app);
        press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_ne!(selected(&app), before, "Esc returns to Command");
        assert_eq!(query(&app), "", "Command still does not type");
    }

    #[test]
    fn the_sample_search_keeps_hjkl_out_of_the_query() {
        let (_directory, mut app) = vim_app();
        app.reference_panel
            .as_mut()
            .expect("panel")
            .select_tab(Tab::Samples);
        app.focus_panel(PanelKind::Reference);
        let query = |app: &App| {
            app.reference_panel
                .as_ref()
                .expect("panel")
                .sound_query
                .clone()
        };
        for key in ['j', 'k', 'h', 'l'] {
            press(&mut app, KeyCode::Char(key), KeyModifiers::NONE);
        }
        assert_eq!(query(&app), "", "hjkl never reach the sample search");
        press(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(query(&app), "x", "Insert types in the sample search");
    }

    #[test]
    fn h_goes_back_from_a_reference_entry() {
        let directory = tempfile::tempdir().expect("temp dir");
        let (mut app, _scene) = plain_app(directory.path(), "// one\n");
        app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
        app.ui_settings.vim_list_navigation = true;
        app.sync_editor_keybindings();
        app.toggle_reference_browse();
        let panel = app.reference_panel.as_mut().expect("panel");
        assert!(panel.expand(), "a result opens its entry");
        assert!(matches!(
            panel.mode,
            crate::reference::ReferenceMode::Entry { .. }
        ));
        let caret = app.editor().primary_selection().head.0;

        press(&mut app, KeyCode::Char('h'), KeyModifiers::NONE);
        assert!(
            matches!(
                app.reference_panel.as_ref().expect("panel").mode,
                crate::reference::ReferenceMode::Browse
            ),
            "h steps out of the entry"
        );
        assert_eq!(
            app.editor().primary_selection().head.0,
            caret,
            "h did not move the editor caret"
        );
    }

    #[test]
    fn the_theme_picker_walks_in_command_and_types_in_insert() {
        let directory = tempfile::tempdir().expect("temp dir");
        let (mut app, _scene) = plain_app(directory.path(), "// one\n");
        app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
        app.ui_settings.vim_list_navigation = true;
        app.sync_editor_keybindings();
        app.theme_picker = Some(ThemePicker::open(&app.theme));
        app.focus_panel(PanelKind::Theme);
        fn picker(app: &App) -> &ThemePicker {
            app.theme_picker.as_ref().expect("picker")
        }
        let before = picker(&app).selected;

        press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_ne!(picker(&app).selected, before, "j walks the theme list");
        assert_eq!(picker(&app).query, "", "j did not type");
        press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(picker(&app).query, "", "a letter waits for Insert");
        assert!(app.status.contains("press i or a"), "{}", app.status);

        press(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
        assert!(picker(&app).vim_insert());
        press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(picker(&app).query, "x", "Insert types the search");
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!picker(&app).vim_insert(), "Esc returns to Command");

        app.theme_picker.as_mut().expect("picker").query.clear();
        let before = picker(&app).selected;
        press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_ne!(picker(&app).selected, before, "Command walks again");
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.theme_picker.is_none(), "a second Esc closes the picker");
    }

    #[test]
    fn the_export_sheet_walks_in_command_and_types_in_insert() {
        use crate::export::{Field, Length};

        let directory = tempfile::tempdir().expect("temp dir");
        let (mut app, _scene) = plain_app(directory.path(), "// one\n");
        app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
        app.ui_settings.vim_list_navigation = true;
        app.sync_editor_keybindings();
        app.toggle_export_sheet();
        fn sheet(app: &App) -> &ExportSheet {
            app.export_sheet.as_ref().expect("sheet")
        }
        assert_eq!(sheet(&app).field, Field::Length);

        press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(sheet(&app).field, Field::First, "j moves down a field");
        press(&mut app, KeyCode::Char('l'), KeyModifiers::NONE);
        assert_eq!(sheet(&app).settings.length, Length::Cycles(20.0));
        press(&mut app, KeyCode::Char('h'), KeyModifiers::NONE);
        assert_eq!(sheet(&app).settings.length, Length::Cycles(16.0));
        press(&mut app, KeyCode::Char('2'), KeyModifiers::NONE);
        assert!(app.status.contains("press i or a"), "{}", app.status);
        press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(app.status.contains("press i or a"), "{}", app.status);

        press(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
        assert!(sheet(&app).vim_insert(), "i enters Insert");
        press(&mut app, KeyCode::Char('3'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('2'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!sheet(&app).vim_insert(), "Esc returns to Command");
        press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(sheet(&app).field, Field::Format, "Tab moves on");
        assert_eq!(
            app.export_sheet.as_mut().expect("sheet").settings().length,
            Length::Cycles(32.0),
            "the typed cycles were taken"
        );
        press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(sheet(&app).field, Field::Limiter);
        let limiter = sheet(&app).settings.limiter_enabled;
        press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
        assert_ne!(
            sheet(&app).settings.limiter_enabled,
            limiter,
            "Space toggles the limiter"
        );

        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(
            app.export_sheet.is_none(),
            "Esc in Command closes the sheet"
        );
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

    #[test]
    fn composed_text_waits_for_insert_in_command_mode() {
        let (_directory, mut app) = vim_app();
        app.handle_terminal_event(Event::KeyWithText {
            key: KeyEvent::new(KeyCode::Char('4'), KeyModifiers::ALT),
            text: "$".into(),
        })
        .expect("composed key");
        app.handle_terminal_event(Event::KeyWithText {
            key: KeyEvent::new(KeyCode::Char('i'), KeyModifiers::ALT),
            text: "İ".into(),
        })
        .expect("composed i");
        let panel = app.reference_panel.as_ref().expect("panel");
        assert_eq!(
            panel.chord_query, "",
            "a composed character is not command-mode text"
        );
        assert!(!panel.vim_insert(), "a composed İ does not enter Insert");
    }

    #[test]
    fn paste_enters_insert_and_lands_in_the_search() {
        let (_directory, mut app) = vim_app();
        app.handle_terminal_event(Event::Paste("maj".into()))
            .expect("paste");
        let panel = app.reference_panel.as_ref().expect("panel");
        assert!(panel.vim_insert(), "a paste enters Insert");
        assert_eq!(panel.chord_query, "maj");
    }

    #[test]
    fn a_held_list_key_keeps_walking() {
        let (_directory, mut app) = vim_app();
        let before = app.reference_panel.as_ref().expect("panel").chord_selected;
        app.handle_terminal_event(Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('j'),
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
        )))
        .expect("repeat");
        assert_ne!(
            app.reference_panel.as_ref().expect("panel").chord_selected,
            before
        );
    }

    #[test]
    fn tabs_and_focus_leave_insert_mode() {
        let (_directory, mut app) = vim_app();
        press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(app.reference_panel.as_ref().expect("panel").vim_insert());
        app.reference_panel
            .as_mut()
            .expect("panel")
            .select_tab(Tab::Scales);
        assert!(
            !app.reference_panel.as_ref().expect("panel").vim_insert(),
            "a tab change returns to Command"
        );
        press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(
            app.reference_panel.as_ref().expect("panel").scale_query,
            "",
            "the reset surface waits for Insert"
        );
        assert!(app.status.contains("press i or a"), "{}", app.status);

        press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(app.reference_panel.as_ref().expect("panel").vim_insert());
        app.focus = Focus::Editor;
        app.focus_panel(PanelKind::Reference);
        assert!(
            !app.reference_panel.as_ref().expect("panel").vim_insert(),
            "a refocus returns to Command"
        );
    }

    #[test]
    fn esc_returns_to_command_then_closes_the_panel() {
        let (_directory, mut app) = vim_app();
        press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        let panel = app.reference_panel.as_ref().expect("panel");
        assert!(!panel.vim_insert(), "Esc returns to Command");
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.reference_panel.is_none(), "a second Esc closes");
    }

    #[test]
    fn space_previews_in_command_mode() {
        let (_directory, mut app) = vim_app();
        press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(app.preview_armed.is_some(), "Space auditions the chord");
        assert_eq!(
            app.reference_panel.as_ref().expect("panel").chord_query,
            "",
            "Space did not type"
        );
    }

    #[test]
    fn command_lets_a_panel_chord_through() {
        let (_directory, mut app) = vim_app();
        press(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
        assert!(app.theme_picker.is_some(), "Ctrl+T still opens the picker");
    }

    #[test]
    fn an_anchored_completion_types_without_insert() {
        let directory = tempfile::tempdir().expect("temp dir");
        let (mut app, _scene) = plain_app(directory.path(), "$: s(\"bd\")\n");
        app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
        app.ui_settings.vim_list_navigation = true;
        app.sync_editor_keybindings();
        app.editor_mut()
            .set_selection(Selection::caret(ByteOffset(7)))
            .expect("caret in the string");
        app.toggle_reference_browse();
        assert!(app.reference_anchor.is_some(), "the completion is anchored");
        let before = app
            .reference_panel
            .as_ref()
            .expect("panel")
            .sound_query
            .clone();
        press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
        let panel = app.reference_panel.as_ref().expect("panel");
        assert!(!panel.vim_insert(), "anchored typing needs no Insert");
        assert_eq!(
            panel.sound_query,
            format!("{before}xi"),
            "the letters typed, i included"
        );
    }
}
