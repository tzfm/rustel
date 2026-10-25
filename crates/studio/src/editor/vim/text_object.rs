use std::ops::Range;

use super::{ByteOffset, Editor, EditorError, HistoryMoment, Mode, Selection};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TextObjectKind {
    Inner,
    Around,
}

impl Editor {
    pub(super) fn vim_finish_text_object(
        &mut self,
        kind: TextObjectKind,
        character: char,
        moment: HistoryMoment,
    ) -> Result<(), EditorError> {
        let count = self
            .vim_take_count()
            .saturating_mul(self.vim.operator.map_or(1, |(_, count)| count));
        let range = self.vim_text_object_range(kind, character, count)?;
        let operator = self.vim.operator.map(|(operator, _)| operator);
        self.vim_cancel_pending();
        let Some(range) = range else {
            self.vim.message = Some("Text object not found");
            return Ok(());
        };
        if let Some(operator) = operator {
            self.vim_apply_operator(operator, range, false, moment)?;
        } else {
            let head = if range.is_empty() {
                range.start
            } else {
                self.document.previous_grapheme_boundary(range.end)?
            };
            self.set_selection(Selection::range(range.start, range.end))?;
            self.vim.mode = Mode::Visual {
                anchor: range.start,
                head,
                linewise: false,
            };
            self.ensure_primary_visible();
        }
        Ok(())
    }

    fn vim_text_object_range(
        &self,
        kind: TextObjectKind,
        character: char,
        count: usize,
    ) -> Result<Option<Range<ByteOffset>>, EditorError> {
        let head = self.vim_head();
        let pair = match character {
            '(' | ')' | 'b' => Some((b'(', b')')),
            '[' | ']' => Some((b'[', b']')),
            '{' | '}' | 'B' => Some((b'{', b'}')),
            '<' | '>' => Some((b'<', b'>')),
            _ => None,
        };
        let range = if let Some((open, close)) = pair {
            let selection = match self.vim.mode {
                Mode::Visual { anchor, head, .. } if anchor != head => {
                    let range = self.primary_selection().ordered();
                    Some(range.start.0..range.end.0)
                }
                _ => None,
            };
            block_range(
                &self.document.text(),
                head.0,
                (open, close),
                kind,
                count,
                selection,
            )
        } else if matches!(character, '"' | '\'' | '`') {
            let line = self.document.line_of(head)?;
            let start = self.document.line_start(line).0;
            quote_range(
                &self.document.line_content(line),
                head.0 - start,
                character as u8,
                kind,
                count,
            )
            .map(|range| start + range.start..start + range.end)
        } else {
            None
        };
        let Some(range) = range else {
            return Ok(None);
        };
        let range = ByteOffset(range.start)..ByteOffset(range.end);
        if self.document.validate_caret_offset(range.start).is_err()
            || self.document.validate_caret_offset(range.end).is_err()
        {
            return Ok(None);
        }
        Ok(Some(range))
    }
}

fn block_range(
    text: &str,
    head: usize,
    (open, close): (u8, u8),
    kind: TextObjectKind,
    mut count: usize,
    selection: Option<Range<usize>>,
) -> Option<Range<usize>> {
    let forward_count = count;
    let mut stack = Vec::new();
    let mut escaped = false;
    let mut next = None::<Range<usize>>;
    let mut enclosing = false;
    let mut quoted_until = None;
    let mut quoted_depth = 0;
    let mut skip_quoted = false;
    for (index, byte) in text.bytes().enumerate() {
        if quoted_until.is_some_and(|end| index > end) {
            stack.truncate(quoted_depth);
            quoted_until = None;
        }
        if quoted_until.is_some() && skip_quoted {
            continue;
        }
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if quoted_until.is_none() && matches!(byte, b'"' | b'\'' | b'`') {
            quoted_until = closing_quote(text.as_bytes(), index);
            quoted_depth = stack.len();
            skip_quoted = quoted_until.is_some_and(|end| head < index || head > end);
        } else if byte == open {
            stack.push(index);
        } else if byte == close
            && stack.len() > quoted_until.map_or(0, |_| quoted_depth)
            && let Some(start) = stack.pop()
        {
            let range = match kind {
                TextObjectKind::Inner => start + 1..index,
                TextObjectKind::Around => start..index + 1,
            };
            if start <= head && head <= index {
                enclosing = true;
                if selection.as_ref().is_some_and(|selection| {
                    range.start > selection.start
                        || range.end < selection.end
                        || range == *selection
                }) {
                    continue;
                }
                count -= 1;
                if count == 0 {
                    return Some(range);
                }
            } else if start > head
                && stack.len() - stack.partition_point(|start| *start <= head) + 1 == forward_count
                && next.as_ref().is_none_or(|next| range.start < next.start)
            {
                next = Some(range);
            }
        }
    }
    (!enclosing).then_some(next).flatten()
}

fn closing_quote(bytes: &[u8], start: usize) -> Option<usize> {
    let mut escaped = false;
    for (index, &byte) in bytes.iter().enumerate().skip(start + 1) {
        if matches!(byte, b'\r' | b'\n') {
            return None;
        }
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == bytes[start] {
            return Some(index);
        }
    }
    None
}

fn quote_range(
    text: &str,
    head: usize,
    quote: u8,
    kind: TextObjectKind,
    count: usize,
) -> Option<Range<usize>> {
    let mut quotes = Vec::new();
    let mut escaped = false;
    for (index, byte) in text.bytes().enumerate() {
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == quote {
            quotes.push(index);
        }
    }
    let next = quotes.partition_point(|index| *index < head);
    let opening = if quotes.get(next) == Some(&head) {
        next - next % 2
    } else {
        next.saturating_sub(1)
    };
    let open = *quotes.get(opening)?;
    let close = *quotes.get(opening + 1)?;
    if close < head {
        return None;
    }
    if kind == TextObjectKind::Inner {
        return Some(if count > 1 {
            open..close + 1
        } else {
            open + 1..close
        });
    }
    let trailing = text.as_bytes()[close + 1..]
        .iter()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count();
    let leading = if trailing == 0 {
        text.as_bytes()[..open]
            .iter()
            .rev()
            .take_while(|byte| matches!(byte, b' ' | b'\t'))
            .count()
    } else {
        0
    };
    Some(open - leading..close + 1 + trailing)
}
