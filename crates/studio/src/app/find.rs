//! The find prompt: opening it, the keys it owns, and the footer status.

use super::*;
use crate::editor::FindDirection;

impl App {
    /// Open the find bar for the focused score. The chord and Edit > Find
    /// both arrive here.
    pub(super) fn open_find(&mut self) {
        self.focus_editor_for_locator();
        self.editor_mut().vim_interrupt();
        self.editor_mut()
            .begin_find(FindDirection::Forward, true, 0);
        self.status = "find - type a regular expression \u{b7} Enter next \u{b7} Esc closes".into();
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
    }
}
