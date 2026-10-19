//! Regex find over the score, shared by the Vim keys and the find bar.

use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use regex::Regex;

use super::{ByteOffset, Editor, Selection};

const MAX_PATTERN_BYTES: usize = 256;
const BAD_PATTERN: &str = "bad pattern";
const NO_MATCH: &str = "no match";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum FindDirection {
    #[default]
    Forward,
    Backward,
}

impl FindDirection {
    fn opposite(self) -> Self {
        match self {
            Self::Forward => Self::Backward,
            Self::Backward => Self::Forward,
        }
    }

    fn marker(self) -> char {
        match self {
            Self::Forward => '/',
            Self::Backward => '?',
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct FindState {
    open: bool,
    pattern: String,
    direction: FindDirection,
    last_pattern: Option<String>,
    count: usize,
    keep_open: bool,
    /// Where the find bar opened. As-you-type searches start here so the
    /// caret does not walk forward with every keystroke.
    anchor: usize,
    last_match: Option<Range<usize>>,
    message: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FindError {
    BadPattern,
    NoMatch,
}

impl Editor {
    /// Open the one-line find prompt. `keep_open` is the find bar outside
    /// Vim: Enter searches again and the prompt stays. Vim's `/` closes on
    /// Enter and repeats with `n`/`N`.
    pub(crate) fn begin_find(&mut self, direction: FindDirection, keep_open: bool, count: usize) {
        self.find.open = true;
        self.find.direction = direction;
        self.find.count = count;
        self.find.keep_open = keep_open;
        self.find.message = None;
        self.find.last_match = None;
        self.find.pattern.clear();
        self.find.anchor = self.primary_selection().head.0;
    }

    pub(crate) fn find_open(&self) -> bool {
        self.find.open
    }

    pub(crate) fn find_cancel(&mut self) {
        self.find.open = false;
        self.find.count = 0;
        self.find.message = None;
    }

    /// The prompt as the footer draws it, with the reason when the last
    /// Enter could not search.
    pub(crate) fn find_label(&self) -> Option<String> {
        if !self.find.open {
            return None;
        }
        let marker = self.find.direction.marker();
        Some(match self.find.message {
            Some(message) => format!("{marker}{} \u{b7} {message}", self.find.pattern),
            None => format!("{marker}{}", self.find.pattern),
        })
    }

    /// The non-Vim footer label. Vim reports the prompt through
    /// `vim_label`, beside the mode, and keeps its `/` or `?`.
    pub(crate) fn find_footer_label(&self) -> Option<String> {
        if self.vim_enabled() || !self.find.open {
            return None;
        }
        let mut label = if self.find.pattern.is_empty() {
            "FIND".to_owned()
        } else {
            format!("FIND {}", self.find.pattern)
        };
        if let Some(message) = self.find.message {
            label.push_str(&format!(" \u{b7} {message}"));
        }
        Some(label)
    }

    /// A key while the prompt is open. Every key belongs to the prompt.
    pub(crate) fn find_key(&mut self, key: KeyEvent) -> bool {
        if !self.find.open {
            return false;
        }
        if key.kind == KeyEventKind::Release {
            return true;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if control && matches!(key.code, KeyCode::Char('c' | 'C')) {
            self.find_cancel();
            return true;
        }
        if control
            || key.modifiers.intersects(
                KeyModifiers::ALT | KeyModifiers::SUPER | KeyModifiers::HYPER | KeyModifiers::META,
            )
        {
            return true;
        }
        match key.code {
            KeyCode::Esc => self.find_cancel(),
            KeyCode::Enter => self.find_commit(),
            KeyCode::Backspace => self.find_backspace(),
            KeyCode::Char(character) if !character.is_control() => {
                self.find_append(character);
            }
            _ => {}
        }
        true
    }

    pub(crate) fn find_paste(&mut self, text: &str) {
        if !self.find.open {
            return;
        }
        for character in text.chars().filter(|character| !character.is_control()) {
            if self.find.pattern.len() + character.len_utf8() > MAX_PATTERN_BYTES {
                break;
            }
            self.find.pattern.push(character);
        }
        self.find_preview();
    }

    /// `n` and `N`: repeat the last search. `reverse` asks for the other
    /// direction without changing the direction `/` or `?` set.
    pub(crate) fn repeat_find(&mut self, reverse: bool, count: usize) {
        let Some(pattern) = self.find.last_pattern.clone() else {
            self.vim_say("No previous search");
            return;
        };
        let direction = if reverse {
            self.find.direction.opposite()
        } else {
            self.find.direction
        };
        match self.find_apply(&pattern, direction, count.max(1)) {
            Ok(()) => self.find.message = None,
            Err(FindError::NoMatch) => self.vim_say("Pattern not found"),
            Err(FindError::BadPattern) => self.vim_say("Bad pattern"),
        }
    }

    fn find_append(&mut self, character: char) {
        if self.find.pattern.len() + character.len_utf8() > MAX_PATTERN_BYTES {
            return;
        }
        self.find.pattern.push(character);
        self.find_preview();
    }

    /// The find bar searches as the pattern changes. The first match at or
    /// after where the bar opened is selected, and Enter walks on from it.
    fn find_preview(&mut self) {
        if !self.find.keep_open {
            return;
        }
        if self.find.pattern.is_empty() {
            // No query, no match. Put the caret back where the bar opened
            // instead of leaving the last match selected.
            self.find.message = None;
            let anchor = self.find.anchor;
            let _ = self.set_selection(Selection::caret(ByteOffset(anchor)));
            self.find.last_match = None;
            return;
        }
        let pattern = self.find.pattern.clone();
        let anchor = self.find.anchor;
        match self.find_apply_from(&pattern, FindDirection::Forward, 1, anchor, false) {
            Ok(()) => self.find.message = None,
            Err(FindError::BadPattern) => self.find.message = Some(BAD_PATTERN),
            Err(FindError::NoMatch) => self.find.message = Some(NO_MATCH),
        }
    }

    fn find_backspace(&mut self) {
        self.find.pattern.pop();
        self.find.message = None;
        // Vim's command line closes when its pattern empties. The find bar
        // stays open until Esc or a click elsewhere.
        if self.find.pattern.is_empty() && !self.find.keep_open {
            self.find_cancel();
        } else {
            self.find_preview();
        }
    }

    fn find_commit(&mut self) {
        let pattern = if self.find.pattern.is_empty() {
            // Only Vim's empty `/` repeats the last search. The find bar
            // does not drag an earlier Vim pattern into a fresh search.
            if self.vim_enabled() {
                self.find.last_pattern.clone().unwrap_or_default()
            } else {
                String::new()
            }
        } else {
            self.find.pattern.clone()
        };
        if pattern.is_empty() {
            // The find bar waits for a pattern; Vim's command line closes.
            if !self.find.keep_open {
                self.find_cancel();
            }
            return;
        }
        let direction = self.find.direction;
        let count = self.find.count.max(1);
        self.find.pattern = pattern.clone();
        self.find.last_pattern = Some(pattern.clone());
        self.find.count = 0;
        match self.find_apply(&pattern, direction, count) {
            Ok(()) => {
                self.find.message = None;
                if !self.find.keep_open {
                    self.find.open = false;
                }
            }
            Err(FindError::BadPattern) => self.find.message = Some(BAD_PATTERN),
            Err(FindError::NoMatch) => {
                self.find.message = Some(NO_MATCH);
                if !self.find.keep_open {
                    self.find.open = false;
                    self.vim_say("Pattern not found");
                }
            }
        }
    }

    fn find_apply(
        &mut self,
        pattern: &str,
        direction: FindDirection,
        count: usize,
    ) -> Result<(), FindError> {
        let caret = self.primary_selection().head.0;
        let continuing = self
            .find
            .last_match
            .as_ref()
            .filter(|found| found.start == caret);
        let (origin, strict) = match (continuing, direction) {
            (Some(found), FindDirection::Forward) => (found.end, false),
            (Some(found), FindDirection::Backward) => (found.start, false),
            (None, _) => (caret, true),
        };
        self.find_apply_from(pattern, direction, count, origin, strict)
    }

    fn find_apply_from(
        &mut self,
        pattern: &str,
        direction: FindDirection,
        count: usize,
        mut origin: usize,
        mut strict: bool,
    ) -> Result<(), FindError> {
        let regex = Regex::new(pattern).map_err(|_| FindError::BadPattern)?;
        let text = self.document.text();
        let matches: Vec<Range<usize>> = regex
            .find_iter(&text)
            .map(|found| found.start()..found.end())
            .filter(|found| found.start < found.end)
            .collect();
        if matches.is_empty() {
            return Err(FindError::NoMatch);
        }

        let mut index = 0;
        // The Vim count may be as large as the document cap. One full lap of
        // the matches repeats the pattern, so only the remainder matters.
        let steps = (count.max(1) - 1) % matches.len() + 1;
        for _ in 0..steps {
            index = next_index(&matches, origin, direction, strict);
            origin = match direction {
                FindDirection::Forward => matches[index].end,
                FindDirection::Backward => matches[index].start,
            };
            strict = false;
        }

        let start = self.valid_caret_before(ByteOffset(matches[index].start));
        let end = self.valid_caret_after(ByteOffset(matches[index].end));
        if end <= start {
            return Err(FindError::NoMatch);
        }
        let selection = if self.vim_enabled() {
            Selection::caret(start)
        } else {
            Selection::range(end, start)
        };
        self.set_selection(selection)
            .map_err(|_| FindError::NoMatch)?;
        self.find.last_match = Some(start.0..end.0);
        self.reveal_selection();
        Ok(())
    }

    fn valid_caret_before(&self, mut offset: ByteOffset) -> ByteOffset {
        while self.document.validate_caret_offset(offset).is_err() {
            match self.document.previous_grapheme_boundary(offset) {
                Ok(previous) if previous < offset => offset = previous,
                _ => break,
            }
        }
        offset
    }

    fn valid_caret_after(&self, mut offset: ByteOffset) -> ByteOffset {
        while self.document.validate_caret_offset(offset).is_err() {
            match self.document.next_grapheme_boundary(offset) {
                Ok(next) if next > offset => offset = next,
                _ => break,
            }
        }
        offset
    }
}

fn next_index(
    matches: &[Range<usize>],
    origin: usize,
    direction: FindDirection,
    strict: bool,
) -> usize {
    match direction {
        FindDirection::Forward => {
            let index = matches.partition_point(|found| {
                if strict {
                    found.start <= origin
                } else {
                    found.start < origin
                }
            });
            if index == matches.len() { 0 } else { index }
        }
        FindDirection::Backward => {
            let before = matches.partition_point(|found| found.start < origin);
            if before == 0 {
                matches.len() - 1
            } else {
                before - 1
            }
        }
    }
}

#[cfg(test)]
mod tests {
    mod find {
        //! Tests for `editor/find.rs`.

        use crate::editor::{ByteOffset, Editor, FindDirection, Selection};

        fn editor(text: &str) -> Editor {
            Editor::new(text).expect("editor")
        }

        fn jump(editor: &mut Editor, pattern: &str, direction: FindDirection) {
            editor.begin_find(direction, false, 0);
            for character in pattern.chars() {
                editor.find_key(crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Char(character),
                    crossterm::event::KeyModifiers::NONE,
                ));
            }
            editor.find_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            ));
        }

        #[test]
        fn regex_finds_forward_backward_and_wraps() {
            let mut editor = editor("$: s(\"bd\") $: s(\"sd\") $: s(\"bd\")");
            let head = |editor: &Editor| editor.primary_selection().head.0;
            jump(&mut editor, r"\$", FindDirection::Forward);
            assert_eq!(head(&editor), 11, "the match at the caret is skipped");
            jump(&mut editor, r"\$", FindDirection::Forward);
            assert_eq!(head(&editor), 22);
            jump(&mut editor, r"\$", FindDirection::Forward);
            assert_eq!(head(&editor), 0, "the search wraps");
            jump(&mut editor, r"\$", FindDirection::Backward);
            assert_eq!(head(&editor), 22, "backward wraps the other way");
        }

        #[test]
        fn repeats_skip_the_whole_match_and_honour_counts() {
            let mut editor = editor("aaa aa a");
            jump(&mut editor, "a+", FindDirection::Forward);
            assert_eq!(editor.primary_selection().head, ByteOffset(4));
            assert_eq!(
                editor.primary_selection().ordered().end,
                ByteOffset(6),
                "the whole run is one match"
            );
            editor.repeat_find(false, 1);
            assert_eq!(editor.primary_selection().head, ByteOffset(7));
            editor.repeat_find(false, 1);
            assert_eq!(
                editor.primary_selection().head,
                ByteOffset(0),
                "count wraps"
            );
            editor.repeat_find(false, 2);
            assert_eq!(editor.primary_selection().head, ByteOffset(7));
            editor.repeat_find(true, 1);
            assert_eq!(
                editor.primary_selection().head,
                ByteOffset(4),
                "N reverses the search direction"
            );
            editor.repeat_find(false, usize::MAX);
            assert!(
                [0, 4, 7].contains(&editor.primary_selection().head.0),
                "a huge count is reduced to a lap, not run step by step"
            );
        }

        #[test]
        fn backward_search_finds_the_match_around_the_caret() {
            let mut editor = editor("abc xabc");
            editor
                .set_selection(Selection::caret(ByteOffset(6)))
                .expect("caret inside the second match");
            jump(&mut editor, "abc", FindDirection::Backward);
            assert_eq!(
                editor.primary_selection().ordered(),
                ByteOffset(5)..ByteOffset(8),
                "the match containing the caret is the nearest one backward"
            );
        }

        #[test]
        fn clearing_the_find_bar_puts_the_caret_back_where_it_opened() {
            let mut editor = editor("one two one");
            editor
                .set_selection(Selection::caret(ByteOffset(4)))
                .expect("caret after the first word");
            editor.begin_find(FindDirection::Forward, true, 0);
            for character in "one".chars() {
                editor.find_key(crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Char(character),
                    crossterm::event::KeyModifiers::NONE,
                ));
            }
            assert_eq!(
                editor.primary_selection().ordered(),
                ByteOffset(8)..ByteOffset(11),
                "typing previews the next match"
            );
            for _ in 0..3 {
                editor.find_key(crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Backspace,
                    crossterm::event::KeyModifiers::NONE,
                ));
            }
            assert!(editor.find_open());
            assert_eq!(
                editor.primary_selection(),
                Selection::caret(ByteOffset(4)),
                "an empty bar leaves the caret at the anchor, not on the match"
            );
        }

        #[test]
        fn empty_pattern_reuses_the_last_search_and_bad_patterns_stay_open() {
            let mut editor = editor("one two two");
            editor.set_keybindings(crate::editor::EditorKeybindings::Vim);
            jump(&mut editor, "two", FindDirection::Forward);
            jump(&mut editor, "", FindDirection::Forward);
            assert_eq!(
                editor.primary_selection().head,
                ByteOffset(8),
                "an empty pattern repeats the last search"
            );

            editor.begin_find(FindDirection::Forward, true, 0);
            for character in "[".chars() {
                editor.find_key(crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Char(character),
                    crossterm::event::KeyModifiers::NONE,
                ));
            }
            editor.find_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            ));
            assert!(editor.find_open());
            assert!(editor.find_label().unwrap().contains("bad pattern"));
        }

        #[test]
        fn an_empty_document_has_no_match() {
            let mut editor = editor("");
            editor.begin_find(FindDirection::Forward, false, 0);
            for character in "x".chars() {
                editor.find_key(crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Char(character),
                    crossterm::event::KeyModifiers::NONE,
                ));
            }
            editor.find_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            ));
            assert!(!editor.find_open());
            assert_eq!(editor.primary_selection(), Selection::caret(ByteOffset(0)));
        }
    }
}
