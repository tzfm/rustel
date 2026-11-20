use super::repeat::RepeatableAction;
use super::{
    ByteOffset, Edit, EditOrigin, Editor, EditorError, HistoryMoment, Mode, SelectionSet,
    Transaction,
};

impl Editor {
    /// `J` or `gJ` from Normal or Visual. Visual joins its lines and
    /// returns to Normal. `.` joins as many lines again.
    pub(super) fn vim_join(
        &mut self,
        spaces: bool,
        explicit_count: Option<usize>,
        moment: HistoryMoment,
    ) -> Result<(), EditorError> {
        let head = self.vim_head();
        let (first, count) = if let Mode::Visual { anchor, .. } = self.vim.mode {
            let first = self.document.line_of(anchor.min(head))?;
            let last = self.document.line_of(anchor.max(head))?;
            self.vim_escape()?;
            (first, last - first + 1)
        } else {
            (self.document.line_of(head)?, explicit_count.unwrap_or(2))
        };
        if self.vim_join_lines(first, count, spaces, moment)? {
            self.vim_record_change(RepeatableAction::Join { spaces }, Some(count));
        }
        Ok(())
    }

    /// `J` and `gJ`: join `count` lines from `first`, at least two. `J`
    /// drops the leading white space of each joined line and puts one
    /// space between, as Vim does. `gJ` keeps the text as it is. On the
    /// last line nothing changes, and a count past the end stops there.
    pub(super) fn vim_join_lines(
        &mut self,
        first: usize,
        count: usize,
        spaces: bool,
        moment: HistoryMoment,
    ) -> Result<bool, EditorError> {
        let last_line = self.vim_last_line();
        if first >= last_line {
            return Ok(false);
        }
        let last = first.saturating_add(count.max(2) - 1).min(last_line);
        let start = self.document.line_content_range(first).start;
        let end = self.document.line_content_range(last).end;
        let mut joined = self.document.line_content(first);
        let mut caret = joined.len();
        for line in first + 1..=last {
            let content = self.document.line_content(line);
            let text = if spaces {
                content.trim_start_matches([' ', '\t'])
            } else {
                content.as_str()
            };
            caret = joined.len();
            // Vim adds no space after white space, before `)`, or
            // around an empty line.
            if spaces
                && !text.is_empty()
                && !text.starts_with(')')
                && !joined.is_empty()
                && !joined.ends_with([' ', '\t'])
            {
                joined.push(' ');
            }
            joined.push_str(text);
        }
        self.apply_transaction(
            Transaction::new(EditOrigin::Typing)
                .replace(Edit::new(start..end, joined))
                .with_selection(SelectionSet::single(ByteOffset(start.0 + caret))),
            moment,
        )?;
        self.vim_normalize_caret()?;
        Ok(true)
    }
}
