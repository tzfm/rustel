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
    /// The prompt, while it is open.
    prompt: Option<FindPrompt>,
    /// The pattern Enter last searched for, and its direction. `n`
    /// follows that direction, whatever a prompt closed since then asked
    /// for.
    last_search: Option<(String, FindDirection)>,
    /// The caret the last match left, and that match. A search from that
    /// caret walks on past the whole match.
    last_match: Option<(usize, Range<usize>)>,
}

/// The one-line find prompt.
#[derive(Clone, Debug)]
struct FindPrompt {
    kind: PromptKind,
    pattern: String,
    direction: FindDirection,
    /// Where the prompt opened. As-you-type searches start here so the
    /// caret does not walk forward with every keystroke.
    anchor: usize,
    /// Why the last search from the prompt found nothing or could not run.
    message: Option<&'static str>,
}

#[derive(Clone, Copy, Debug)]
enum PromptKind {
    /// The find bar outside Vim. It searches as the pattern changes, and
    /// Enter searches again and keeps it open.
    Bar,
    /// Vim's `/` or `?`. Enter searches `count` matches on and closes it,
    /// unless the pattern is bad.
    VimSearch { count: usize },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FindError {
    BadPattern,
    NoMatch,
}

impl Editor {
    /// Open the one-line find prompt. `keep_open` is the find bar outside
    /// Vim: Enter searches again and the prompt stays. Vim's `/` closes on
    /// Enter and repeats with `n`/`N`. Its Enter goes `count` matches on.
    /// The find bar has no count.
    pub(crate) fn begin_find(&mut self, direction: FindDirection, keep_open: bool, count: usize) {
        // Vim's `n` walks on from the last match whatever prompt opens and
        // closes in between. The find bar starts every search afresh.
        if keep_open {
            self.find.last_match = None;
        }
        self.find.prompt = Some(FindPrompt {
            kind: if keep_open {
                PromptKind::Bar
            } else {
                PromptKind::VimSearch { count }
            },
            pattern: String::new(),
            direction,
            anchor: self.primary_selection().head.0,
            message: None,
        });
    }

    pub(crate) fn find_open(&self) -> bool {
        self.find.prompt.is_some()
    }

    /// Where the open prompt opened, after `edits`.
    pub(super) fn find_anchor_through(&self, edits: &[super::Edit]) -> Option<usize> {
        let prompt = self.find.prompt.as_ref()?;
        Some(super::map_offset(ByteOffset(prompt.anchor), edits, true).0)
    }

    pub(super) fn find_set_anchor(&mut self, anchor: Option<usize>) {
        if let (Some(prompt), Some(anchor)) = (self.find.prompt.as_mut(), anchor) {
            prompt.anchor = anchor;
        }
    }

    pub(crate) fn find_cancel(&mut self) {
        self.find.prompt = None;
    }

    /// The prompt as the footer draws it, with the reason when the last
    /// Enter could not search.
    pub(crate) fn find_label(&self) -> Option<String> {
        let prompt = self.find.prompt.as_ref()?;
        let marker = prompt.direction.marker();
        Some(match prompt.message {
            Some(message) => format!("{marker}{} \u{b7} {message}", prompt.pattern),
            None => format!("{marker}{}", prompt.pattern),
        })
    }

    /// The non-Vim footer label. Vim reports the prompt through
    /// `vim_label`, beside the mode, and keeps its `/` or `?`.
    pub(crate) fn find_footer_label(&self) -> Option<String> {
        if self.uses_modal_keys() {
            return None;
        }
        let prompt = self.find.prompt.as_ref()?;
        let mut label = if prompt.pattern.is_empty() {
            "FIND".to_owned()
        } else {
            format!("FIND {}", prompt.pattern)
        };
        if let Some(message) = prompt.message {
            label.push_str(&format!(" \u{b7} {message}"));
        }
        Some(label)
    }

    /// A key while the prompt is open. Every key belongs to the prompt.
    pub(crate) fn find_key(&mut self, key: KeyEvent) -> bool {
        if !self.find_open() {
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
        let Some(prompt) = self.find.prompt.as_mut() else {
            return;
        };
        for character in text.chars().filter(|character| !character.is_control()) {
            if prompt.pattern.len() + character.len_utf8() > MAX_PATTERN_BYTES {
                break;
            }
            prompt.pattern.push(character);
        }
        self.find_preview();
    }

    /// `n` and `N`: repeat the last search. `reverse` asks for the other
    /// direction without changing the direction `/` or `?` set.
    pub(crate) fn repeat_find(&mut self, reverse: bool, count: usize) {
        let Some((pattern, direction)) = self.find.last_search.clone() else {
            self.vim_say("No previous search");
            return;
        };
        let direction = if reverse {
            direction.opposite()
        } else {
            direction
        };
        match self.find_apply(&pattern, direction, count.max(1)) {
            Ok(()) => {}
            Err(FindError::NoMatch) => self.vim_say("Pattern not found"),
            Err(FindError::BadPattern) => self.vim_say("Bad pattern"),
        }
    }

    fn find_append(&mut self, character: char) {
        let Some(prompt) = self.find.prompt.as_mut() else {
            return;
        };
        if prompt.pattern.len() + character.len_utf8() > MAX_PATTERN_BYTES {
            return;
        }
        prompt.pattern.push(character);
        self.find_preview();
    }

    /// The find bar searches as the pattern changes. The first match at or
    /// after where the bar opened is selected, and Enter walks on from it.
    fn find_preview(&mut self) {
        let Some(FindPrompt {
            kind: PromptKind::Bar,
            pattern,
            anchor,
            ..
        }) = &self.find.prompt
        else {
            return;
        };
        let (pattern, anchor) = (pattern.clone(), *anchor);
        let message = if pattern.is_empty() {
            // No query, no match. Put the caret back where the bar opened
            // instead of leaving the last match selected.
            let _ = self.set_selection(Selection::caret(ByteOffset(anchor)));
            self.find.last_match = None;
            None
        } else {
            match self.find_apply_from(&pattern, FindDirection::Forward, 1, anchor, false) {
                Ok(()) => None,
                Err(FindError::BadPattern) => Some(BAD_PATTERN),
                Err(FindError::NoMatch) => Some(NO_MATCH),
            }
        };
        if let Some(prompt) = self.find.prompt.as_mut() {
            prompt.message = message;
        }
    }

    fn find_backspace(&mut self) {
        let Some(prompt) = self.find.prompt.as_mut() else {
            return;
        };
        prompt.pattern.pop();
        prompt.message = None;
        // Vim's command line closes when its pattern empties. The find bar
        // stays open until Esc or a click elsewhere.
        if prompt.pattern.is_empty() && matches!(prompt.kind, PromptKind::VimSearch { .. }) {
            self.find_cancel();
        } else {
            self.find_preview();
        }
    }

    /// Enter. The find bar stays open. Vim's prompt closes, except on a
    /// bad pattern, where it stays open and keeps its count.
    fn find_commit(&mut self) {
        let Some(mut prompt) = self.find.prompt.take() else {
            return;
        };
        let pattern = if prompt.pattern.is_empty() {
            // Only Vim's empty `/` repeats the last search. The find bar
            // does not drag an earlier Vim pattern into a fresh search.
            if self.uses_modal_keys() {
                self.find
                    .last_search
                    .as_ref()
                    .map(|(pattern, _)| pattern.clone())
                    .unwrap_or_default()
            } else {
                String::new()
            }
        } else {
            prompt.pattern.clone()
        };
        if pattern.is_empty() {
            // The find bar waits for a pattern. Vim's command line closes.
            if matches!(prompt.kind, PromptKind::Bar) {
                self.find.prompt = Some(prompt);
            }
            return;
        }
        let count = match prompt.kind {
            PromptKind::Bar => 1,
            PromptKind::VimSearch { count } => count.max(1),
        };
        // A new Vim pattern searches from the caret, not on from a match of
        // the old one.
        if matches!(prompt.kind, PromptKind::VimSearch { .. })
            && self
                .find
                .last_search
                .as_ref()
                .is_none_or(|(last, _)| *last != pattern)
        {
            self.find.last_match = None;
        }
        prompt.pattern = pattern.clone();
        self.find.last_search = Some((pattern.clone(), prompt.direction));
        match (
            self.find_apply(&pattern, prompt.direction, count),
            prompt.kind,
        ) {
            (Ok(()), PromptKind::Bar) => {
                prompt.message = None;
                self.find.prompt = Some(prompt);
            }
            (Err(FindError::BadPattern), _) => {
                prompt.message = Some(BAD_PATTERN);
                self.find.prompt = Some(prompt);
            }
            (Err(FindError::NoMatch), PromptKind::Bar) => {
                prompt.message = Some(NO_MATCH);
                self.find.prompt = Some(prompt);
            }
            (Ok(()), PromptKind::VimSearch { .. }) => {}
            (Err(FindError::NoMatch), PromptKind::VimSearch { .. }) => {
                self.vim_say("Pattern not found");
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
            .filter(|(left, _)| *left == caret)
            .map(|(_, found)| found);
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
        let selection = if self.uses_modal_keys() {
            Selection::caret(start)
        } else {
            Selection::range(end, start)
        };
        self.set_selection(selection)
            .map_err(|_| FindError::NoMatch)?;
        // Vim's Normal keeps the caret on a character, so a match that
        // starts on a line break leaves it on the one before.
        self.normalize_after_input()
            .map_err(|_| FindError::NoMatch)?;
        self.find.last_match = Some((self.primary_selection().head.0, start.0..end.0));
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

        /// A Vim match that starts on a line break leaves the caret on the
        /// last character of its line, where Normal keeps it, and `n` walks
        /// on from there. `n` once found the same match again and again.
        #[test]
        fn n_walks_on_from_a_match_on_a_line_break() {
            let mut editor = editor("ab\ncd ef");
            editor.set_keybindings(crate::editor::EditorKeybindings::Vim);
            let head = |editor: &Editor| editor.primary_selection().head.0;
            jump(&mut editor, r"\s", FindDirection::Forward);
            assert_eq!(head(&editor), 1, "the caret rests on the b");
            let n = crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('n'),
                crossterm::event::KeyModifiers::NONE,
            );
            editor
                .vim_key(n, crate::editor::HistoryMoment(0), None)
                .expect("n");
            assert_eq!(head(&editor), 5, "n finds the space");
            editor
                .vim_key(n, crate::editor::HistoryMoment(0), None)
                .expect("n");
            assert_eq!(head(&editor), 1, "and wraps to the line break");

            // A `/` closed with Esc in between leaves the walk where it was.
            editor.begin_find(FindDirection::Forward, false, 0);
            editor.find_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Esc,
                crossterm::event::KeyModifiers::NONE,
            ));
            editor
                .vim_key(n, crate::editor::HistoryMoment(0), None)
                .expect("n");
            assert_eq!(head(&editor), 5, "n walks on after a cancelled prompt");
        }

        /// A `?` closed with Esc never searched, so `n` keeps the direction
        /// of the search that ran. The cancelled prompt once turned it.
        #[test]
        fn a_cancelled_search_keeps_the_direction_of_n() {
            let mut editor = editor("foo foo foo");
            editor.set_keybindings(crate::editor::EditorKeybindings::Vim);
            jump(&mut editor, "foo", FindDirection::Forward);
            assert_eq!(editor.primary_selection().head, ByteOffset(4));
            editor.begin_find(FindDirection::Backward, false, 0);
            editor.find_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Esc,
                crossterm::event::KeyModifiers::NONE,
            ));
            editor.repeat_find(false, 1);
            assert_eq!(
                editor.primary_selection().head,
                ByteOffset(8),
                "n still searches forward"
            );
        }

        /// The count before `/` waits through a bad pattern, so the
        /// corrected search lands on the match it asked for, not on the
        /// first.
        #[test]
        fn a_count_survives_a_bad_pattern() {
            use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
            let mut editor = editor("x a a a a");
            editor.set_keybindings(crate::editor::EditorKeybindings::Vim);
            let press = |editor: &mut Editor, code| {
                editor
                    .vim_key(
                        KeyEvent::new(code, KeyModifiers::NONE),
                        crate::editor::HistoryMoment(0),
                        None,
                    )
                    .expect("key");
            };
            for character in ['3', '/', 'a', '('] {
                press(&mut editor, KeyCode::Char(character));
            }
            press(&mut editor, KeyCode::Enter);
            assert!(editor.find_open(), "a bad pattern keeps the prompt open");
            press(&mut editor, KeyCode::Backspace);
            press(&mut editor, KeyCode::Enter);
            assert!(!editor.find_open());
            assert_eq!(
                editor.primary_selection().head,
                ByteOffset(6),
                "the third a"
            );
        }
    }
}
