use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

use super::super::{display_column_of_offset_cached, offset_at_display_column_cached};
use super::{
    ByteOffset, Editor, EditorError, HistoryMoment, Mode, Motion, Operator, line_ending_len,
};

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
            let target = self.vim_motion_target(motion, count, true)?.0;
            // j on the last line and k on the first fail, and the
            // operator waiting for them does nothing, as in Vim.
            if matches!(motion, Motion::Up | Motion::Down)
                && self.document.line_of(target)? == self.document.line_of(head)?
            {
                self.vim_cancel_pending();
                self.vim.motion_blocked = true;
                return Ok(());
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
                    // An inclusive end takes the character it rests on,
                    // never a line break. e rests on a line end only at
                    // the end of the text.
                    let last = head.max(target);
                    let line_end = self
                        .document
                        .line_content_range(self.document.line_of(last)?)
                        .end;
                    self.document.next_grapheme_boundary(last)?.min(line_end)
                } else {
                    head.max(target)
                };
                head.min(target)..end
            };
            self.vim_apply_operator(operator, range, linewise, head.min(target), moment)
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

    /// Ctrl+D and Ctrl+U. A count scrolls that many lines, as Vim does.
    /// Without one, half the window moves, caret and viewport together.
    /// Ctrl+D and Ctrl+U. A count scrolls that many lines, as Vim does.
    /// Without one, half the window moves, caret and viewport together.
    pub(super) fn vim_half_page(
        &mut self,
        down: bool,
        count: Option<usize>,
    ) -> Result<(), EditorError> {
        let before = self.vim_head();
        match count {
            None => {
                let motion = if down {
                    super::super::Motion::HalfPageDown
                } else {
                    super::super::Motion::HalfPageUp
                };
                if let Mode::Visual {
                    anchor, linewise, ..
                } = self.vim.mode
                {
                    // The half page moves the Visual head, not the edge of
                    // the highlighted band. The goal column stays for j and k.
                    let mut selection = self.primary_selection();
                    selection.anchor = before;
                    selection.head = before;
                    self.selections.set_single(selection);
                    self.move_selection(motion, false)?;
                    let moved = self.primary_selection();
                    let head = self.vim_clamp_normal(moved.head)?;
                    self.vim_select_visual(anchor, head, linewise)?;
                    let mut selection = self.primary_selection();
                    selection.goal_column = moved.goal_column;
                    self.selections.set_single(selection);
                } else {
                    self.move_selection(motion, false)?;
                    self.vim_normalize_caret()?;
                }
            }
            Some(count) => {
                self.vim_move(if down { Motion::Down } else { Motion::Up }, count)?;
                self.viewport.top_row = if down {
                    self.viewport
                        .top_row
                        .saturating_add(count)
                        .min(self.maximum_top_row())
                } else {
                    self.viewport.top_row.saturating_sub(count)
                };
                self.ensure_primary_visible();
            }
        }
        self.vim.motion_blocked |= self.vim_head() == before;
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
                    self.vim_line_below(line, count_nonzero)
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
            // On a line of only blanks, ^ rests on the last one, as in Vim.
            Motion::FirstText => self.vim_clamp_normal(self.document.indent_end(line))?,
            Motion::LineEnd => {
                self.document
                    .line_content_range(self.vim_line_below(line, count_nonzero - 1))
                    .end
            }
            Motion::FirstLine | Motion::LastLine => {
                let target_line = if matches!(motion, Motion::LastLine) && count == 0 {
                    self.vim_last_line()
                } else {
                    count_nonzero.saturating_sub(1).min(self.vim_last_line())
                };
                self.document.indent_end(target_line)
            }
            Motion::WordNext | Motion::WordPrevious | Motion::WordEnd | Motion::WordChange => {
                self.vim_word_target(head, motion, count_nonzero, operator)
            }
        };
        // $ keeps j and k at the end of every line after it, as Vim's goal
        // column past the end does.
        let goal = matches!(motion, Motion::LineEnd).then_some(usize::MAX);
        Ok((
            if operator {
                target
            } else {
                self.vim_clamp_normal(target)?
            },
            goal,
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

    /// Where `w`, `b`, `e` or the `cw` form of `e` takes the caret. An
    /// operator's `w` stops its last word at the end of its line.
    fn vim_word_target(
        &self,
        head: ByteOffset,
        motion: Motion,
        count: usize,
        operator: bool,
    ) -> ByteOffset {
        let text = self.document.text();
        let mut walk = WordWalk::new(&text, head.0);
        match motion {
            Motion::WordPrevious => walk.back_word(count),
            Motion::WordEnd => walk.end_word(count, false),
            Motion::WordChange => walk.end_word(count, true),
            _ => walk.forward_word(count, operator),
        }
        ByteOffset(
            walk.clusters
                .get(walk.index)
                .map_or(text.len(), |(offset, ..)| *offset),
        )
    }

    /// The last line Vim knows. The empty line after a final line break is
    /// drawn, but no Vim motion or operator lands on it.
    pub(super) fn vim_last_line(&self) -> usize {
        let last = self.document.line_count() - 1;
        if last > 0 && self.document.line_content_range(last).is_empty() {
            last - 1
        } else {
            last
        }
    }

    /// The line `count` lines below `line`, held to the last line Vim
    /// knows. It never climbs, even from the drawn line past the end.
    fn vim_line_below(&self, line: usize, count: usize) -> usize {
        line.saturating_add(count)
            .min(self.vim_last_line().max(line))
    }

    pub(super) fn vim_line_range(&self, first: usize, last: usize) -> Range<ByteOffset> {
        // The drawn line past the end is not a Vim line, so a command made
        // from it takes nothing, never the line above.
        if first > self.vim_last_line() {
            let start = self.document.line_start(first);
            return start..start;
        }
        let last = last.min(self.vim_last_line());
        let end = if last + 1 < self.document.line_count() {
            self.document.line_start(last + 1)
        } else {
            ByteOffset(self.document.len_bytes())
        };
        self.document.line_start(first.min(last))..end
    }
}

/// The text as Vim's word motions walk it, one grapheme at a time. A line
/// break stands where its line ends, as the NUL after each line does in
/// Vim, and the break after the last line ends the text.
struct WordWalk {
    /// Each grapheme's offset and word class, and whether it breaks a line.
    clusters: Vec<(usize, u8, bool)>,
    /// Where the last line ends. Nothing lies past it.
    end: usize,
    /// Where the last line starts.
    last_line: usize,
    index: usize,
}

impl WordWalk {
    fn new(text: &str, head: usize) -> Self {
        let clusters = text
            .grapheme_indices(true)
            .map(|(offset, cluster)| (offset, word_class(cluster), line_ending_len(cluster) > 0))
            .collect::<Vec<_>>();
        let end = clusters.len() - usize::from(clusters.last().is_some_and(|cluster| cluster.2));
        let last_line = clusters[..end]
            .iter()
            .rposition(|cluster| cluster.2)
            .map_or(0, |index| index + 1);
        let index = clusters
            .partition_point(|(offset, ..)| *offset < head)
            .min(end);
        Self {
            clusters,
            end,
            last_line,
            index,
        }
    }

    fn at_line_end(&self) -> bool {
        self.index >= self.end || self.clusters[self.index].2
    }

    fn on_empty_line(&self) -> bool {
        self.at_line_end() && (self.index == 0 || self.clusters[self.index - 1].2)
    }

    /// The word class here. A line end is blank.
    fn class(&self) -> u8 {
        if self.at_line_end() {
            0
        } else {
            self.clusters[self.index].1
        }
    }

    /// One grapheme on, as Vim's `inc`. `None` at the end of the text,
    /// otherwise whether the step left a line or reached its end.
    fn forward(&mut self) -> Option<bool> {
        if self.index >= self.end {
            return None;
        }
        let left = self.at_line_end();
        self.index += 1;
        Some(left || self.at_line_end())
    }

    /// One grapheme back, as Vim's `dec`. False at the start of the text.
    fn back(&mut self) -> bool {
        let moved = self.index > 0;
        self.index = self.index.saturating_sub(1);
        moved
    }

    /// One grapheme on inside `w`. False where the motion ends: at the end
    /// of the text, or on reaching a line end when `eol` holds.
    fn step(&mut self, eol: bool) -> bool {
        matches!(self.forward(), Some(crossed) if !(crossed && eol))
    }

    /// Past every grapheme of `class`. False at the end of the text.
    fn skip(&mut self, class: u8) -> bool {
        while self.class() == class {
            if self.forward().is_none() {
                return false;
            }
        }
        true
    }

    /// Vim's `w`, which stops on an empty line. With `eol`, the last word
    /// ends at its line's end, as it does for an operator.
    fn forward_word(&mut self, count: usize, eol: bool) {
        for step in 0..count {
            let eol = eol && step + 1 == count;
            let class = self.class();
            let on_last_line = self.index >= self.last_line;
            if !self.step(eol || on_last_line) {
                return;
            }
            if class != 0 {
                while self.class() == class {
                    if !self.step(eol) {
                        return;
                    }
                }
            }
            while self.class() == 0 && !self.on_empty_line() {
                if !self.step(eol) {
                    return;
                }
            }
        }
    }

    /// Vim's `b`, which stops on an empty line.
    fn back_word(&mut self, count: usize) {
        for _ in 0..count {
            if !self.back() {
                return;
            }
            while self.class() == 0 && !self.on_empty_line() {
                if !self.back() {
                    return;
                }
            }
            if self.on_empty_line() {
                continue;
            }
            let class = self.class();
            while self.class() == class {
                if !self.back() {
                    return;
                }
            }
            self.forward();
        }
    }

    /// Vim's `e`. With `stop`, a first word that already ends here counts
    /// as reached, as it does for `cw`. Later words never do.
    fn end_word(&mut self, count: usize, mut stop: bool) {
        for _ in 0..count {
            let class = self.class();
            if self.forward().is_none() {
                return;
            }
            if self.class() == class && class != 0 {
                if !self.skip(class) {
                    return;
                }
            } else if !stop || class == 0 {
                if !self.skip(0) {
                    return;
                }
                let word = self.class();
                if !self.skip(word) {
                    return;
                }
            }
            self.back();
            stop = false;
        }
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
