use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

use super::super::{display_column_of_offset_cached, offset_at_display_column_cached};
use super::{ByteOffset, Editor, EditorError, HistoryMoment, Mode, Motion, Operator};

impl Editor {
    pub(super) fn vim_finish_motion(
        &mut self,
        motion: Motion,
        moment: HistoryMoment,
    ) -> Result<(), EditorError> {
        let specified = std::mem::take(&mut self.vim.count);
        let count = if matches!(motion, Motion::LastLine) {
            specified
        } else {
            specified.max(1)
        };
        if let Some((operator, before)) = self.vim.operator.take() {
            let count = if count == 0 && before == 1 {
                0
            } else {
                count.max(1).saturating_mul(before)
            };
            let head = self.vim_head();
            let motion = if operator == Operator::Change
                && matches!(motion, Motion::WordNext)
                && !self.vim_grapheme_is_space(head)?
            {
                Motion::WordChange
            } else {
                motion
            };
            let mut target = self.vim_motion_target(motion, count, true)?.0;
            if matches!(motion, Motion::WordNext)
                && count == 1
                && !self.vim_grapheme_is_space(head)?
                && self.document.line_of(target)? > self.document.line_of(head)?
            {
                target = self
                    .document
                    .line_content_range(self.document.line_of(head)?)
                    .end;
            }
            let linewise = matches!(
                motion,
                Motion::Up | Motion::Down | Motion::FirstLine | Motion::LastLine
            );
            let range = if linewise {
                self.vim_line_range(
                    self.document.line_of(head.min(target))?,
                    self.document.line_of(head.max(target))?,
                )
            } else {
                let end = if matches!(motion, Motion::WordEnd | Motion::WordChange) {
                    self.document.next_grapheme_boundary(head.max(target))?
                } else {
                    head.max(target)
                };
                head.min(target)..end
            };
            self.vim_apply_operator(operator, range, linewise, moment)
        } else {
            self.vim_move(motion, count)
        }
    }

    pub(super) fn vim_move(&mut self, motion: Motion, count: usize) -> Result<(), EditorError> {
        let head = self.vim_head();
        let (target, goal) = self.vim_motion_target(motion, count, false)?;
        if let Mode::Visual {
            anchor, linewise, ..
        } = self.vim.mode
        {
            self.vim_select_visual(anchor, target, linewise)?;
        } else {
            self.vim_place_caret(target)?;
            self.vim_normalize_caret()?;
        }
        self.vim.motion_blocked |= matches!(
            motion,
            Motion::Left | Motion::Right | Motion::Up | Motion::Down
        ) && self.vim_head() == head;
        let mut selection = self.primary_selection();
        selection.goal_column = goal;
        self.selections.set_single(selection);
        Ok(())
    }

    pub(super) fn vim_motion_target(
        &self,
        motion: Motion,
        count: usize,
        operator: bool,
    ) -> Result<(ByteOffset, Option<usize>), EditorError> {
        let head = self.vim_head();
        let line = self.document.line_of(head)?;
        let range = self.document.line_content_range(line);
        let count_nonzero = count.max(1);
        let target = match motion {
            Motion::Left | Motion::Right => {
                let text = self.document.line_content(line);
                let relative = head.0.saturating_sub(range.start.0);
                let offset = if matches!(motion, Motion::Left) {
                    text.grapheme_indices(true)
                        .rev()
                        .filter(|(index, _)| *index < relative)
                        .nth(count_nonzero - 1)
                        .map_or(0, |(index, _)| index)
                } else {
                    text.grapheme_indices(true)
                        .filter(|(index, _)| *index > relative)
                        .nth(count_nonzero - 1)
                        .map_or(text.len(), |(index, _)| index)
                };
                ByteOffset(range.start.0 + offset)
            }
            Motion::Up | Motion::Down => {
                let goal = self.primary_selection().goal_column.unwrap_or_else(|| {
                    display_column_of_offset_cached(
                        &self.document,
                        line,
                        head,
                        self.viewport.tab_width,
                        &mut self.horizontal_layout.borrow_mut(),
                    )
                });
                let target_line = if matches!(motion, Motion::Up) {
                    line.saturating_sub(count_nonzero)
                } else {
                    line.saturating_add(count_nonzero)
                        .min(self.document.line_count() - 1)
                };
                let offset = offset_at_display_column_cached(
                    &self.document,
                    target_line,
                    goal,
                    self.viewport.tab_width,
                    &mut self.horizontal_layout.borrow_mut(),
                );
                return Ok((self.vim_clamp_normal(offset)?, Some(goal)));
            }
            Motion::LineStart => range.start,
            Motion::FirstText => self.document.first_non_whitespace(line),
            Motion::LineEnd => {
                self.document
                    .line_content_range(line.saturating_add(count_nonzero - 1))
                    .end
            }
            Motion::FirstLine | Motion::LastLine => {
                let target_line = if matches!(motion, Motion::LastLine) && count == 0 {
                    self.document.line_count() - 1
                } else {
                    count_nonzero
                        .saturating_sub(1)
                        .min(self.document.line_count() - 1)
                };
                self.document.first_non_whitespace(target_line)
            }
            Motion::WordNext | Motion::WordPrevious | Motion::WordEnd | Motion::WordChange => {
                self.vim_word_target(head, motion, count_nonzero)?
            }
        };
        Ok((
            if operator {
                target
            } else {
                self.vim_clamp_normal(target)?
            },
            None,
        ))
    }

    fn vim_clamp_normal(&self, head: ByteOffset) -> Result<ByteOffset, EditorError> {
        let range = self
            .document
            .line_content_range(self.document.line_of(head)?);
        if head >= range.end && range.end > range.start {
            Ok(self.document.previous_grapheme_boundary(range.end)?)
        } else {
            Ok(head)
        }
    }

    fn vim_grapheme_is_space(&self, head: ByteOffset) -> Result<bool, EditorError> {
        let next = self.document.next_grapheme_boundary(head)?;
        Ok(self
            .document
            .slice(head..next)?
            .chars()
            .all(char::is_whitespace))
    }

    fn vim_word_target(
        &self,
        head: ByteOffset,
        motion: Motion,
        count: usize,
    ) -> Result<ByteOffset, EditorError> {
        let text = self.document.text();
        let clusters = text
            .grapheme_indices(true)
            .map(|(offset, cluster)| (offset, word_class(cluster)))
            .collect::<Vec<_>>();
        if clusters.is_empty() {
            return Ok(ByteOffset::ZERO);
        }
        let mut index = clusters.partition_point(|(offset, _)| *offset < head.0);
        for step in 0..count.min(clusters.len()) {
            let before = index;
            match motion {
                Motion::WordPrevious => {
                    index = index.saturating_sub(1);
                    while index > 0 && clusters[index].1 == 0 {
                        index -= 1;
                    }
                    while index > 0 && clusters[index - 1].1 == clusters[index].1 {
                        index -= 1;
                    }
                }
                Motion::WordEnd | Motion::WordChange => {
                    if step > 0 || matches!(motion, Motion::WordEnd) {
                        index = index.saturating_add(1).min(clusters.len());
                    }
                    while index < clusters.len() && clusters[index].1 == 0 {
                        index += 1;
                    }
                    while index + 1 < clusters.len() && clusters[index + 1].1 == clusters[index].1 {
                        index += 1;
                    }
                }
                _ => {
                    if index < clusters.len() {
                        let class = clusters[index].1;
                        while index < clusters.len() && clusters[index].1 == class {
                            index += 1;
                        }
                        while index < clusters.len() && clusters[index].1 == 0 {
                            index += 1;
                        }
                    }
                }
            }
            if index == before || index == clusters.len() {
                break;
            }
        }
        Ok(ByteOffset(
            clusters
                .get(index)
                .map_or(text.len(), |(offset, _)| *offset),
        ))
    }

    pub(super) fn vim_line_range(&self, first: usize, last: usize) -> Range<ByteOffset> {
        let last = last.min(self.document.line_count() - 1);
        let end = if last + 1 < self.document.line_count() {
            self.document.line_start(last + 1)
        } else {
            ByteOffset(self.document.len_bytes())
        };
        self.document.line_start(first)..end
    }
}

pub(super) fn motion_for(character: char) -> Option<Motion> {
    match character {
        'h' => Some(Motion::Left),
        'l' | ' ' => Some(Motion::Right),
        'j' => Some(Motion::Down),
        'k' => Some(Motion::Up),
        '0' => Some(Motion::LineStart),
        '^' => Some(Motion::FirstText),
        '$' => Some(Motion::LineEnd),
        'w' => Some(Motion::WordNext),
        'b' => Some(Motion::WordPrevious),
        'e' => Some(Motion::WordEnd),
        'G' => Some(Motion::LastLine),
        _ => None,
    }
}

fn word_class(cluster: &str) -> u8 {
    if cluster.chars().all(char::is_whitespace) {
        0
    } else if cluster
        .chars()
        .any(|character| character == '_' || character.is_alphanumeric())
    {
        1
    } else {
        2
    }
}
