//! The find prompt: opening it, the keys it owns, and the footer status.

use super::*;
use crate::editor::FindDirection;

impl App {
    /// Open the find prompt for the focused score. The chord and Edit > Find
    /// both arrive here. The editor says whether its profile keeps the bar
    /// open, so a modal profile closes it on Enter and repeats with `n`.
    pub(super) fn open_find(&mut self) {
        self.focus_editor_for_locator();
        if let Err(error) = self.editor_mut().leave_key_mode_for_find() {
            self.set_error(ErrorOwner::Editor, error.to_string());
        }
        let keeps_open = self.editor().find_bar_keeps_open();
        self.editor_mut()
            .begin_find(FindDirection::Forward, keeps_open, 0);
        self.status = if keeps_open {
            "find - type a regular expression \u{b7} Enter next \u{b7} Esc closes".into()
        } else {
            "find - type a regular expression \u{b7} Enter searches \u{b7} Esc cancels".into()
        };
        self.dirty_frame = true;
    }

    /// Keys and pastes while find has the keyboard. Offered before Vim
    /// mappings and every default editor command, so a typed pattern never
    /// becomes a motion or an edit.
    pub(super) fn route_find_event(
        &mut self,
        terminal_event: &Event,
    ) -> Result<bool, RuntimeError> {
        if !self.editor_accepts_raw_paste() || !self.editor().find_open() {
            return Ok(false);
        }
        // Quit and the transport are global: they work from every other
        // prompt, so the find bar must not swallow them.
        if let Event::Key(key) = terminal_event
            && self.find_global_chord(key)
        {
            return Ok(false);
        }
        match terminal_event {
            Event::Key(key) => {
                if key.kind != KeyEventKind::Release {
                    let was_open = self.editor().find_open();
                    self.editor_mut().find_key(*key);
                    self.invalidate_maps();
                    if was_open && !self.editor().find_open() {
                        self.status = "find closed".into();
                    }
                    self.dirty_frame = true;
                }
            }
            Event::Paste(text) => {
                self.editor_mut().find_paste(text);
                self.invalidate_maps();
                self.dirty_frame = true;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Whether this key belongs to a surface that stays global while find
    /// is open: quit and the transport.
    pub(super) fn find_global_chord(&self, key: &KeyEvent) -> bool {
        key.kind == KeyEventKind::Press
            && matches!(
                self.keybinds.action_for(key),
                Some(crate::keybinds::BindAction::Quit)
                    | Some(crate::keybinds::BindAction::Evaluate)
                    | Some(crate::keybinds::BindAction::Stop)
                    | Some(crate::keybinds::BindAction::RewindEvaluate)
            )
    }
}

#[cfg(test)]
mod tests {
    mod find {
        //! Tests for `app/find.rs`.

        use crate::app::tests::support::*;
        use crate::app::*;
        use crate::editor::{ByteOffset, Selection};

        fn chord(app: &mut App) {
            press(
                app,
                KeyCode::Char('y'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            );
        }

        #[test]
        fn the_find_chord_walks_matches_keeps_the_prompt_and_esc_closes() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "one two one\n");
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(4)))
                .expect("caret inside the second word");
            let head = |app: &App| app.editor().primary_selection().head.0;

            chord(&mut app);
            assert!(app.editor().find_open());
            press(&mut app, KeyCode::Char('o'), KeyModifiers::NONE);
            assert_eq!(head(&app), 6, "the bar jumps as you type");
            press(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
            assert_eq!(head(&app), 8);
            press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
            assert_eq!(
                app.editor().find_footer_label().as_deref(),
                Some("FIND one"),
                "the find bar has no Vim slash"
            );
            assert_eq!(
                app.editor().primary_selection().ordered(),
                ByteOffset(8)..ByteOffset(11),
                "the match is selected while typing"
            );
            assert!(app.editor().find_open(), "the find bar stays open");

            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(head(&app), 0, "Enter wraps to the first match");
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(head(&app), 8, "and walks forward again");

            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(!app.editor().find_open());

            chord(&mut app);
            assert_eq!(
                app.editor().find_footer_label().as_deref(),
                Some("FIND"),
                "a new find bar starts empty; the Vim search does not stick"
            );
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(app.editor().find_open(), "an empty find bar stays open");
            assert_eq!(head(&app), 8, "and the caret does not move");
            for character in "one".chars() {
                press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
            }
            assert_eq!(head(&app), 8, "the match at the anchor is selected");
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(head(&app), 0, "Enter walks on from it");
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);

            chord(&mut app);
            press(&mut app, KeyCode::Char('['), KeyModifiers::NONE);
            assert!(
                app.editor()
                    .find_footer_label()
                    .is_some_and(|label| label.contains("bad pattern")),
                "the error is shown while typing"
            );
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(app.editor().find_open(), "a bad pattern is not a crash");
            press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
            assert!(!app.editor().find_open(), "Ctrl+C closes the find bar");
        }

        #[test]
        fn vim_edit_find_closes_on_enter_and_n_walks_matches() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "one two one three one\n");
            app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
            app.sync_editor_keybindings();
            let head = |app: &App| app.editor().primary_selection().head.0;

            chord(&mut app);
            assert!(app.editor().find_open());
            for character in "one".chars() {
                press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
            }
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(!app.editor().find_open(), "Enter searches once and closes");
            let first = head(&app);
            press(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
            assert_ne!(head(&app), first, "n walks forward");
            press(&mut app, KeyCode::Char('N'), KeyModifiers::NONE);
            assert_eq!(head(&app), first, "N walks back");
        }

        /// A click on Edit > Find opens the bar on its press. Its release
        /// is the end of that click, so the typing after it goes to the
        /// bar, in either profile. The release once closed the bar again,
        /// and the pattern went into the score as text or as Vim commands.
        #[test]
        fn the_release_of_the_find_click_leaves_the_bar_open() {
            use crossterm::event::{MouseButton, MouseEventKind};
            for vim in [false, true] {
                let directory = tempfile::tempdir().expect("temp dir");
                let (mut app, _scene) = plain_app(directory.path(), "one two one three one\n");
                if vim {
                    app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
                    app.sync_editor_keybindings();
                }
                let before = app.editor().source();
                app.dispatch_menu_action(crate::menu::MenuAction::Find)
                    .expect("the menu row");
                mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 8, 1);
                assert!(
                    app.editor().find_open(),
                    "the release closed the bar (vim: {vim})"
                );
                type_text(&mut app, "three");
                press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
                assert_eq!(
                    app.editor().source(),
                    before,
                    "the pattern went into the score"
                );
                assert_eq!(
                    app.editor().primary_selection().head.0,
                    12,
                    "Enter found the match (vim: {vim})"
                );
            }
        }

        #[test]
        fn backspace_keeps_the_non_vim_find_bar_open() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "one\n");
            chord(&mut app);
            for character in "one".chars() {
                press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
            }
            for _ in 0..3 {
                press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
            }
            assert!(
                app.editor().find_open(),
                "an empty pattern keeps the find bar open"
            );
            assert_eq!(app.editor().find_footer_label().as_deref(), Some("FIND"));
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(!app.editor().find_open());
        }

        #[test]
        fn global_quit_still_reaches_the_studio_from_the_find_bar() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "// \n");
            chord(&mut app);
            press(&mut app, KeyCode::Char('q'), KeyModifiers::CONTROL);
            press(&mut app, KeyCode::Char('q'), KeyModifiers::CONTROL);
            assert!(app.quit, "Ctrl+Q still quits from the find bar");
        }

        #[test]
        fn a_paste_lands_in_the_find_bar_and_not_in_the_score() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "one two one\n");
            let score = app.editor().source();
            chord(&mut app);
            app.handle_terminal_event(Event::Paste("tw".into()))
                .expect("paste handled");
            assert_eq!(app.editor().find_footer_label().as_deref(), Some("FIND tw"));
            assert_eq!(
                app.editor().primary_selection().ordered(),
                ByteOffset(4)..ByteOffset(6),
                "the pasted pattern is searched at once"
            );
            assert_eq!(app.editor().source(), score);
        }

        #[test]
        fn composed_text_types_in_the_find_bar() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "one two one\n");
            chord(&mut app);
            app.handle_terminal_event(Event::KeyWithText {
                key: KeyEvent::new(KeyCode::Char('4'), KeyModifiers::ALT),
                text: "t".into(),
            })
            .expect("composed text");
            assert_eq!(app.editor().find_footer_label().as_deref(), Some("FIND t"));
        }

        /// The wheel only moves the view. A find prompt stays open under it
        /// in either profile, and the typing after it goes on into the
        /// pattern. A wheel step once closed the prompt.
        #[test]
        fn a_wheel_step_keeps_the_find_prompt_open() {
            use crossterm::event::MouseEventKind;
            for vim in [false, true] {
                let directory = tempfile::tempdir().expect("temp dir");
                let (mut app, _scene) = plain_app(directory.path(), "foo foobar\n");
                let score = app.editor().source();
                if vim {
                    app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
                    app.sync_editor_keybindings();
                    type_text(&mut app, "/");
                } else {
                    chord(&mut app);
                }
                type_text(&mut app, "foo");
                let editor = app.regions.panes[0].editor;
                mouse(
                    &mut app,
                    MouseEventKind::ScrollDown,
                    editor.x + 2,
                    editor.y + 1,
                );
                assert!(
                    app.editor().find_open(),
                    "the wheel closed the prompt (vim: {vim})"
                );
                type_text(&mut app, "bar");
                press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
                assert_eq!(app.editor().source(), score, "vim: {vim}");
                assert_eq!(
                    app.editor().primary_selection().head.0,
                    4,
                    "Enter found foobar (vim: {vim})"
                );
            }
        }

        /// Focus leaving the window is no input. The find bar stays open
        /// through it, and a paste after it lands in the pattern. Focus
        /// loss once closed the bar, so the paste went into the score.
        #[test]
        fn focus_loss_keeps_the_find_bar_open() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "one two one\n");
            let score = app.editor().source();
            chord(&mut app);
            app.handle_terminal_event(Event::FocusLost)
                .expect("focus lost");
            app.handle_terminal_event(Event::FocusGained)
                .expect("focus gained");
            assert!(app.editor().find_open(), "focus loss closed the bar");
            app.handle_terminal_event(Event::Paste("tw".into()))
                .expect("paste handled");
            assert_eq!(app.editor().find_footer_label().as_deref(), Some("FIND tw"));
            assert_eq!(app.editor().source(), score);
        }

        /// Find opened from Vim's Insert leaves Insert, as `/` searches from
        /// Normal. Enter closes the prompt and `n` walks the matches. The
        /// chord once kept Insert, so `n` typed a letter into the score.
        #[test]
        fn vim_find_from_insert_lands_in_normal() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "one two one three one\n");
            app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
            app.sync_editor_keybindings();
            let score = app.editor().source();
            press(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
            chord(&mut app);
            type_text(&mut app, "one");
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(app.editor().vim_is_normal(), "Enter lands in Normal");
            let first = app.editor().primary_selection().head.0;
            press(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
            assert_eq!(app.editor().source(), score, "n typed into the score");
            assert_ne!(
                app.editor().primary_selection().head.0,
                first,
                "n walks forward"
            );
        }
    }
}
