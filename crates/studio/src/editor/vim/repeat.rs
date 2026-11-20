use unicode_segmentation::UnicodeSegmentation;

use super::{
    ByteOffset, EditOrigin, Editor, EditorError, EditorKeybindings, HistoryMoment, Mode, Motion,
    Operator, Selection, TextObjectKind,
};

/// One completed Vim change, ready for `.` to replay.
#[derive(Clone, Debug)]
pub(super) struct LastChange {
    pub(super) action: RepeatableAction,
    /// Explicit count supplied to the original command, if any.
    pub(super) count: Option<usize>,
    pub(super) inserted: Vec<InsertStep>,
}

/// Recording and replay phase for Vim `.` repeat.
#[derive(Clone, Debug, Default)]
pub(super) enum RepeatState {
    /// Not recording or replaying.
    #[default]
    Idle,
    /// In Insert mode, collecting edits for the active insert/change command.
    Recording(LastChange),
    /// Executing `.`; inner edits must not overwrite `last_change`.
    Replaying,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum RepeatableAction {
    OperatorMotion {
        operator: Operator,
        motion: Motion,
    },
    OperatorLines {
        operator: Operator,
    },
    OperatorTextObject {
        operator: Operator,
        kind: TextObjectKind,
        character: char,
    },
    NormalEdit(NormalEdit),
    Replace {
        character: char,
    },
    Put {
        before: bool,
    },
    Insert(InsertEntry),
    Visual {
        edit: VisualEdit,
        shape: VisualShape,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NormalEdit {
    DeleteForward,
    DeleteBackward,
    DeleteToEnd,
    ChangeToEnd,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum InsertEntry {
    Before,
    After,
    LineStart,
    LineEnd,
    OpenBelow,
    OpenAbove,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum VisualEdit {
    Delete,
    Change,
    Replace(char),
    Put { before: bool },
}

/// Shape of a Visual selection, so `.` applies over the same span from the
/// current caret position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum VisualShape {
    Linewise {
        lines: usize,
    },
    SingleLine {
        graphemes: usize,
    },
    MultiLine {
        extra_lines: usize,
        end_graphemes: usize,
    },
}

#[derive(Clone, Debug)]
pub(in crate::editor) enum InsertStep {
    Text(String),
    Paste(String),
    Newline,
    Indent,
    Outdent,
    DeleteBackward,
    DeleteForward,
    DeleteWordBackward,
    DeleteWordForward,
}

impl Editor {
    /// Whether the last repeatable Vim change was a `p` or `P` put.
    pub(crate) fn vim_last_change_is_put(&self) -> bool {
        self.vim.last_change.as_ref().is_some_and(|change| {
            matches!(
                change.action,
                RepeatableAction::Put { .. }
                    | RepeatableAction::Visual {
                        edit: VisualEdit::Put { .. },
                        ..
                    }
            )
        })
    }

    pub(super) fn vim_record_change(&mut self, action: RepeatableAction, count: Option<usize>) {
        if matches!(self.vim.repeat_state, RepeatState::Replaying) {
            return;
        }
        let change = LastChange {
            action,
            count,
            inserted: Vec::new(),
        };
        if self.vim.mode == Mode::Insert {
            self.vim.repeat_state = RepeatState::Recording(change);
        } else {
            self.vim.last_change = Some(change);
        }
    }

    pub(super) fn vim_finish_insert_recording(&mut self) {
        if let RepeatState::Recording(change) =
            std::mem::replace(&mut self.vim.repeat_state, RepeatState::Idle)
        {
            self.vim.last_change = Some(change);
        }
    }

    pub(in crate::editor) fn vim_record_insert_step(&mut self, step: InsertStep) {
        if self.vim.keybindings != EditorKeybindings::Vim || self.vim.mode != Mode::Insert {
            return;
        }
        if let RepeatState::Recording(change) = &mut self.vim.repeat_state {
            match (change.inserted.last_mut(), step) {
                (Some(InsertStep::Text(existing)), InsertStep::Text(added)) => {
                    existing.push_str(&added);
                }
                (_, step) => change.inserted.push(step),
            }
        }
    }

    /// Arrow movement during Insert mode breaks the change recording and
    /// starts a fresh insert from the new position, as in Vim.
    pub(in crate::editor) fn vim_break_insert_recording(&mut self) {
        if self.vim.keybindings == EditorKeybindings::Vim
            && self.vim.mode == Mode::Insert
            && matches!(self.vim.repeat_state, RepeatState::Recording(_))
        {
            self.vim.repeat_state = RepeatState::Recording(LastChange {
                action: RepeatableAction::Insert(InsertEntry::Before),
                count: None,
                inserted: Vec::new(),
            });
        }
    }

    pub(super) fn vim_visual_shape(
        &self,
        range: std::ops::Range<ByteOffset>,
        linewise: bool,
    ) -> Result<VisualShape, EditorError> {
        let first_line = self.document.line_of(range.start)?;
        if linewise {
            let last_offset = if range.end > range.start {
                self.document
                    .previous_grapheme_boundary(range.end)?
                    .max(range.start)
            } else {
                range.start
            };
            let last_line = self.document.line_of(last_offset)?;
            return Ok(VisualShape::Linewise {
                lines: last_line.saturating_sub(first_line) + 1,
            });
        }
        let end_line = self.document.line_of(range.end)?;
        if first_line == end_line {
            let graphemes = self.document.slice(range)?.graphemes(true).count().max(1);
            Ok(VisualShape::SingleLine { graphemes })
        } else {
            let end_start = self.document.line_start(end_line);
            let end_graphemes = self
                .document
                .slice(end_start..range.end)?
                .graphemes(true)
                .count();
            Ok(VisualShape::MultiLine {
                extra_lines: end_line - first_line,
                end_graphemes,
            })
        }
    }

    /// Replay the last change for `.`, optionally overriding its count.
    pub(super) fn vim_repeat_last_change(
        &mut self,
        count_override: Option<usize>,
        moment: HistoryMoment,
    ) -> Result<(), EditorError> {
        let Some(change) = self.vim.last_change.take() else {
            return Ok(());
        };
        let previous = std::mem::replace(&mut self.vim.repeat_state, RepeatState::Replaying);
        self.close_history_group();
        let result = self.vim_replay_change(&change, count_override, moment);
        if result.is_err() && self.vim.mode == Mode::Insert {
            let _ = self.vim_escape();
        }
        self.close_history_group();
        self.vim.repeat_state = previous;
        self.vim.last_change = Some(change);
        result
    }

    fn vim_replay_change(
        &mut self,
        change: &LastChange,
        count_override: Option<usize>,
        moment: HistoryMoment,
    ) -> Result<(), EditorError> {
        let effective_count = count_override.or(change.count);
        let count = effective_count.unwrap_or(1).max(1);
        match change.action {
            RepeatableAction::OperatorMotion { operator, motion } => {
                let motion_count = match effective_count {
                    Some(count) => count.max(1),
                    None if matches!(motion, Motion::LastLine) => 0,
                    None => 1,
                };
                let changed =
                    self.vim_execute_operator_motion(operator, motion, motion_count, moment)?;
                if changed && operator == Operator::Change && self.vim.mode == Mode::Insert {
                    self.vim_replay_insert_steps(&change.inserted, moment)?;
                    self.vim_escape()?;
                }
            }
            RepeatableAction::OperatorLines { operator } => {
                let changed = self.vim_execute_operator_lines(operator, count, moment)?;
                if changed && operator == Operator::Change && self.vim.mode == Mode::Insert {
                    self.vim_replay_insert_steps(&change.inserted, moment)?;
                    self.vim_escape()?;
                }
            }
            RepeatableAction::OperatorTextObject {
                operator,
                kind,
                character,
            } => {
                let changed = self
                    .vim_execute_operator_text_object(operator, kind, character, count, moment)?;
                if changed && operator == Operator::Change && self.vim.mode == Mode::Insert {
                    self.vim_replay_insert_steps(&change.inserted, moment)?;
                    self.vim_escape()?;
                }
            }
            RepeatableAction::NormalEdit(edit) => {
                let changed = self.vim_execute_normal_edit(edit, count, None, moment)?;
                if changed && edit == NormalEdit::ChangeToEnd && self.vim.mode == Mode::Insert {
                    self.vim_replay_insert_steps(&change.inserted, moment)?;
                    self.vim_escape()?;
                }
            }
            RepeatableAction::Replace { character } => {
                self.vim_replace_char(count, character, moment)?;
            }
            RepeatableAction::Put { before } => {
                self.vim_put(before, count, moment)?;
            }
            RepeatableAction::Insert(entry) => {
                if matches!(entry, InsertEntry::OpenBelow | InsertEntry::OpenAbove) {
                    let above = entry == InsertEntry::OpenAbove;
                    for _ in 0..count {
                        self.vim_open_line(above, moment)?;
                        self.vim_replay_insert_steps(&change.inserted, moment)?;
                    }
                    self.vim_escape()?;
                } else {
                    self.vim_enter_insert(entry, moment)?;
                    for _ in 0..count {
                        self.vim_replay_insert_steps(&change.inserted, moment)?;
                    }
                    self.vim_escape()?;
                }
            }
            RepeatableAction::Visual { edit, shape } => {
                self.vim_replay_visual(edit, shape, count_override, &change.inserted, moment)?;
            }
        }
        Ok(())
    }

    fn vim_replay_visual(
        &mut self,
        edit: VisualEdit,
        shape: VisualShape,
        count_override: Option<usize>,
        inserted: &[InsertStep],
        moment: HistoryMoment,
    ) -> Result<(), EditorError> {
        let head = self.vim_head();
        let start_line = self.document.line_of(head)?;
        let (range, linewise) = match shape {
            VisualShape::Linewise { lines } => {
                let lines = count_override.unwrap_or(lines).max(1);
                (
                    self.vim_line_range(start_line, start_line.saturating_add(lines - 1)),
                    true,
                )
            }
            VisualShape::SingleLine { graphemes } => {
                let graphemes = count_override.unwrap_or(graphemes).max(1);
                let line_end = self.document.line_content_range(start_line).end;
                let mut end = head;
                for _ in 0..graphemes {
                    if end >= line_end {
                        break;
                    }
                    end = self.document.next_grapheme_boundary(end)?;
                }
                (head..end, false)
            }
            VisualShape::MultiLine {
                extra_lines,
                end_graphemes,
            } => {
                let end_line = self.vim_line_below(start_line, extra_lines);
                let line_start = self.document.line_start(end_line);
                let line_end = self.document.line_content_range(end_line).end;
                let mut end = line_start;
                for _ in 0..end_graphemes {
                    if end >= line_end {
                        break;
                    }
                    end = self.document.next_grapheme_boundary(end)?;
                }
                (head..end.max(head), false)
            }
        };
        let start = range.start;
        match edit {
            VisualEdit::Delete => {
                self.vim_apply_operator(Operator::Delete, range, linewise, false, start, moment)?;
            }
            VisualEdit::Change => {
                let changed = self.vim_apply_operator(
                    Operator::Change,
                    range,
                    linewise,
                    false,
                    start,
                    moment,
                )?;
                if changed && self.vim.mode == Mode::Insert {
                    self.vim_replay_insert_steps(inserted, moment)?;
                    self.vim_escape()?;
                }
            }
            VisualEdit::Replace(character) => {
                self.vim_replace_range(range, character, moment)?;
            }
            VisualEdit::Put { before } => {
                let visual_head = if range.is_empty() {
                    range.start
                } else {
                    self.document
                        .previous_grapheme_boundary(range.end)?
                        .max(range.start)
                };
                self.set_selection(Selection::range(range.start, range.end))?;
                self.vim.mode = Mode::Visual {
                    anchor: range.start,
                    head: visual_head,
                    linewise,
                };
                self.vim_put(before, 1, moment)?;
            }
        }
        Ok(())
    }

    fn vim_replay_insert_steps(
        &mut self,
        steps: &[InsertStep],
        moment: HistoryMoment,
    ) -> Result<(), EditorError> {
        for step in steps {
            match step {
                InsertStep::Text(text) => {
                    self.replace_selections(text, EditOrigin::Typing, moment)?;
                }
                InsertStep::Paste(text) => {
                    self.paste_text(text, moment)?;
                }
                InsertStep::Newline => self.insert_newline(moment)?,
                InsertStep::Indent => self.indent(moment)?,
                InsertStep::Outdent => self.outdent(moment)?,
                InsertStep::DeleteBackward => self.delete(false, false, moment)?,
                InsertStep::DeleteForward => self.delete(true, false, moment)?,
                InsertStep::DeleteWordBackward => self.delete(false, true, moment)?,
                InsertStep::DeleteWordForward => self.delete(true, true, moment)?,
            }
        }
        Ok(())
    }
}
