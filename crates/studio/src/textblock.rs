//! Character selection for read-only pane text.
//!
//! The reference entry and the snippet shelf draw generated lines, not a
//! [`Document`](super::editor::Document): the text is rebuilt every frame and
//! never edited, so the editor's byte-offset selection machinery has nothing
//! to hold on to. This is the small model that fits what they are - a list of
//! lines, a rect they occupy, and positions counted in characters, because
//! the panes draw per character and a char-indexed hit-test is the exact
//! inverse of that drawing.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use unicode_width::UnicodeWidthStr;

use super::editor::HistoryMoment;

/// A character position in generated pane text: a line, and a char index
/// into it.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TextPoint {
    pub line: usize,
    pub column: usize,
}

/// A dragged range: where the press landed and where the pointer is. Either
/// order - `ordered()` sorts them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextSelection {
    pub anchor: TextPoint,
    pub head: TextPoint,
}

impl TextSelection {
    pub fn caret(at: TextPoint) -> Self {
        Self {
            anchor: at,
            head: at,
        }
    }

    pub fn ordered(&self) -> (TextPoint, TextPoint) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// The selected char columns on one line, or `None` when the line is
    /// outside the span. Interior lines are selected whole.
    pub fn columns_on(&self, line: usize, line_chars: usize) -> Option<std::ops::Range<usize>> {
        let (from, to) = self.ordered();
        if line < from.line || line > to.line {
            return None;
        }
        let start = if line == from.line { from.column } else { 0 };
        let end = if line == to.line {
            to.column
        } else {
            line_chars
        };
        (start < end).then_some(start.min(line_chars)..end.min(line_chars))
    }

    /// The selected text, read out of the same lines the pane draws.
    pub fn text(&self, lines: &[String]) -> String {
        let (from, to) = self.ordered();
        let mut out = String::new();
        for line in from.line..=to.line {
            let Some(text) = lines.get(line) else {
                break;
            };
            let chars = text.chars().count();
            if let Some(columns) = self.columns_on(line, chars) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.extend(text.chars().skip(columns.start).take(columns.len()));
            } else if line > from.line && line < to.line {
                // A blank interior line is part of what was dragged over.
                if !out.is_empty() {
                    out.push('\n');
                }
            }
        }
        out
    }
}

/// Whether a hit outside the block is rejected or pulled back into it. A
/// press is `Inside`; the drag that follows is `Extend`, so leaving the rect
/// extends to its edge the way the editor's own drag does.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Clamp {
    Inside,
    Extend,
}

/// The selectable text on screen: the lines a pane drew, the rows they
/// occupy, and which line sits on the first of them.
pub struct TextBlock<'a> {
    pub lines: &'a [String],
    pub area: Rect,
    pub first_line: usize,
}

impl TextBlock<'_> {
    /// The character under the pointer.
    ///
    /// A press past a line's end lands at its end; `x` past the right edge
    /// takes the end of the LOGICAL line - deliberately including what the
    /// column truncated away, because a truncated copy of an example is a
    /// trap.
    ///
    /// The rows beside the text - blank padding under a short block, or
    /// outside the rect on a drag - are the pane's empty half, not text: a
    /// hit there anchors at the text's own edge, the end of the last drawn
    /// line below or the start of the first drawn line above, the way an
    /// editor treats the space under a short document. A drag begun below
    /// the text and carried up into it therefore reads as if it had begun
    /// at the text's right end, not at whatever column the last line
    /// happens to have under the pointer. Blank lines WITHIN the text are
    /// lines like any other, and a press on one still lands on them.
    pub fn point_at(&self, x: u16, y: u16, clamp: Clamp) -> Option<TextPoint> {
        if self.area.is_empty() || self.lines.is_empty() {
            return None;
        }
        // `Extend` keeps where the pointer REALLY is: a drag below the
        // block is below the text even after the clamp pulls it back to
        // the bottom row.
        let (raw_y, y) = match clamp {
            Clamp::Inside => {
                if y < self.area.y || y >= self.area.bottom() {
                    return None;
                }
                (y, y)
            }
            Clamp::Extend => (y, y.clamp(self.area.y, self.area.bottom() - 1)),
        };
        let last = self.lines.len().saturating_sub(1);
        let line = (self.first_line + usize::from(y - self.area.y))
            .min(self.first_line + last)
            .min(last);
        let Some(first_drawn) = self.lines.iter().position(|line| !line.is_empty()) else {
            // Every line blank: there is no text edge to anchor on.
            return Some(TextPoint { line, column: 0 });
        };
        let last_drawn = self
            .lines
            .iter()
            .rposition(|line| !line.is_empty())
            .unwrap_or(last);
        // The screen row a drawn line would occupy. A drawn line scrolled
        // past the window's far edge sits at the window's edge: the whole
        // window is then on the void side of it.
        let row_of = |at: usize| {
            self.area
                .y
                .saturating_add(at.saturating_sub(self.first_line) as u16)
        };
        if raw_y > row_of(last_drawn) {
            return Some(TextPoint {
                line: last_drawn,
                column: self.lines[last_drawn].chars().count(),
            });
        }
        if raw_y < row_of(first_drawn) {
            return Some(TextPoint {
                line: first_drawn,
                column: 0,
            });
        }
        let text = &self.lines[line];
        let column = if x < self.area.x {
            0
        } else if x >= self.area.right() {
            text.chars().count()
        } else {
            column_at_cell(text, usize::from(x - self.area.x))
        };
        Some(TextPoint { line, column })
    }
}

/// The char index whose cell span contains `cell` - the inverse of the walk
/// the panes draw with, width for width.
pub fn column_at_cell(line: &str, cell: usize) -> usize {
    let mut column = 0;
    let mut position = 0;
    for character in line.chars() {
        let width = UnicodeWidthStr::width(character.encode_utf8(&mut [0; 4]) as &str).max(1);
        if cell < position + width {
            return column;
        }
        position += width;
        column += 1;
    }
    column
}

/// The cell span a char range occupies on its line - `column_at_cell`'s
/// inverse, so the band and the hit-test cannot drift.
pub fn cell_span(line: &str, columns: std::ops::Range<usize>) -> std::ops::Range<u16> {
    let mut start = 0usize;
    let mut end = 0usize;
    let mut position = 0usize;
    for (index, character) in line.chars().enumerate() {
        let width = UnicodeWidthStr::width(character.encode_utf8(&mut [0; 4]) as &str).max(1);
        if index == columns.start {
            start = position;
        }
        position += width;
        if index + 1 == columns.end {
            end = position;
        }
    }
    if columns.start >= line.chars().count() {
        start = position;
    }
    if columns.end >= line.chars().count() {
        end = position;
    }
    (start.min(u16::MAX as usize) as u16)..(end.min(u16::MAX as usize) as u16)
}

/// Paint the selected span of one visible row, background only, so the
/// syntax colours already drawn keep their say.
pub fn paint_band(
    buffer: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    line: &str,
    columns: std::ops::Range<usize>,
    colour: Color,
) {
    let span = cell_span(line, columns);
    let from = x.saturating_add(span.start);
    let to = x.saturating_add(span.end).min(x.saturating_add(width));
    if from >= to {
        return;
    }
    buffer.set_style(
        Rect::new(from, y, to - from, 1),
        Style::default().bg(colour),
    );
}

/// Double- and triple-click detection for the panes, with the editor's own
/// rule: within half a second and a cell of the last press.
#[derive(Clone, Copy, Debug, Default)]
pub struct ClickCounter {
    last: Option<(u16, u16, HistoryMoment, u8)>,
}

/// What a repeated press selects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Granularity {
    Character,
    Word,
    Line,
}

const MULTI_CLICK_WINDOW_MS: u64 = 500;

impl ClickCounter {
    pub fn press(&mut self, x: u16, y: u16, moment: HistoryMoment) -> Granularity {
        let count = match self.last {
            Some((last_x, last_y, last_moment, count))
                if moment.0.saturating_sub(last_moment.0) <= MULTI_CLICK_WINDOW_MS
                    && last_x.abs_diff(x) <= 1
                    && last_y.abs_diff(y) <= 1 =>
            {
                count % 3 + 1
            }
            _ => 1,
        };
        self.last = Some((x, y, moment, count));
        match count {
            2 => Granularity::Word,
            3 => Granularity::Line,
            _ => Granularity::Character,
        }
    }
}

/// The word around a char column: alphanumerics and `_` hold together,
/// whitespace holds together, anything else stands alone - the editor's
/// classes, without needing a `Document` to ask.
pub fn word_range(line: &str, column: usize) -> std::ops::Range<usize> {
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return 0..0;
    }
    let at = column.min(chars.len() - 1);
    let class = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            0u8
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    };
    let kind = class(chars[at]);
    let mut start = at;
    while start > 0 && class(chars[start - 1]) == kind {
        start -= 1;
    }
    let mut end = at + 1;
    while end < chars.len() && class(chars[end]) == kind {
        end += 1;
    }
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(lines: &[String]) -> TextBlock<'_> {
        TextBlock {
            lines,
            area: Rect::new(2, 5, 20, 3),
            first_line: 0,
        }
    }

    fn strings(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| (*line).to_owned()).collect()
    }

    #[test]
    fn a_cell_column_maps_to_the_character_drawn_there() {
        // ASCII: one cell, one char, both ways.
        assert_eq!(column_at_cell("osc(20)", 0), 0);
        assert_eq!(column_at_cell("osc(20)", 4), 4);
        assert_eq!(cell_span("osc(20)", 1..4), 1..4);

        // A double-width character occupies two cells; either lands on it,
        // and its span is two wide.
        let wide = "a界b";
        assert_eq!(column_at_cell(wide, 1), 1);
        assert_eq!(column_at_cell(wide, 2), 1);
        assert_eq!(column_at_cell(wide, 3), 2);
        assert_eq!(cell_span(wide, 1..2), 1..3);
        assert_eq!(cell_span(wide, 2..3), 3..4);
    }

    #[test]
    fn a_press_past_the_end_of_a_line_lands_at_its_end() {
        let lines = strings(&["short", "a longer line here"]);
        let block = block(&lines);
        let point = block.point_at(2 + 15, 5, Clamp::Inside).unwrap();
        assert_eq!(point, TextPoint { line: 0, column: 5 });
    }

    #[test]
    fn a_press_below_the_last_line_lands_on_the_last_line() {
        let lines = strings(&["one", "two"]);
        let block = block(&lines);
        let point = block.point_at(4, 7, Clamp::Inside).unwrap();
        assert_eq!(point.line, 1);
        // And a drag above the block pulls back to the first.
        let point = block.point_at(4, 0, Clamp::Extend).unwrap();
        assert_eq!(point.line, 0);
        assert_eq!(block.point_at(4, 0, Clamp::Inside), None);
    }

    #[test]
    fn a_drag_begun_below_the_text_anchors_at_its_right_end() {
        // A drag that starts on the blank rows under a short entry reads
        // as if it started at the end of the last drawn line.
        let lines = strings(&["first line of text", "", "short"]);
        let block = block(&lines);
        // The block is three rows tall (5, 6, 7): text on 5 and 7, a
        // drawn gap on 6, and no room below - `Extend` from y=8 clamps
        // back onto the last row and must still read as BELOW the text.
        let below = block.point_at(9, 8, Clamp::Extend).unwrap();
        assert_eq!(
            below,
            TextPoint { line: 2, column: 5 },
            "the anchor is the end of the last drawn line"
        );
        // A press on the drawn gap between two lines of text is on the
        // gap: blank interior lines are lines like any other.
        let gap = block.point_at(3, 6, Clamp::Inside).unwrap();
        assert_eq!(gap, TextPoint { line: 1, column: 0 });
        // A drag above the text anchors at its left edge, symmetrically.
        let above = block.point_at(9, 0, Clamp::Extend).unwrap();
        assert_eq!(above, TextPoint { line: 0, column: 0 });
        // The text reads end-of-text backward into the block.
        let selection = TextSelection {
            anchor: below,
            head: TextPoint {
                line: 0,
                column: 14,
            },
        };
        assert_eq!(selection.text(&lines), "text\n\nshort");
    }

    #[test]
    fn a_press_on_the_last_drawn_line_keeps_its_column() {
        // The blank-half rule is for the void BESIDE the text, not for the
        // text's own rows: the last drawn line still hit-tests by column.
        let lines = strings(&["one two", "three"]);
        let block = block(&lines);
        let point = block.point_at(2 + 2, 6, Clamp::Inside).unwrap();
        assert_eq!(point, TextPoint { line: 1, column: 2 });
    }

    #[test]
    fn a_selection_dragged_upward_reads_the_same_as_one_dragged_down() {
        let lines = strings(&["osc(20)", ".rotate(0.1)", ".out()"]);
        let down = TextSelection {
            anchor: TextPoint { line: 0, column: 4 },
            head: TextPoint { line: 2, column: 4 },
        };
        let up = TextSelection {
            anchor: down.head,
            head: down.anchor,
        };
        assert_eq!(down.text(&lines), up.text(&lines));
        assert_eq!(down.text(&lines), "20)\n.rotate(0.1)\n.out");
    }

    #[test]
    fn a_drag_past_the_right_edge_takes_the_rest_of_the_line() {
        // The column truncates a wide example; the copy must not.
        let lines = strings(&["a line far wider than the twenty cells shown"]);
        let block = block(&lines);
        let point = block.point_at(2 + 25, 5, Clamp::Extend).unwrap();
        assert_eq!(point.column, lines[0].chars().count());
        let selection = TextSelection {
            anchor: TextPoint { line: 0, column: 0 },
            head: point,
        };
        assert_eq!(selection.text(&lines), lines[0]);
    }

    #[test]
    fn double_and_triple_clicks_widen_and_a_late_press_starts_over() {
        let mut clicks = ClickCounter::default();
        assert_eq!(clicks.press(4, 4, HistoryMoment(0)), Granularity::Character);
        assert_eq!(clicks.press(4, 4, HistoryMoment(200)), Granularity::Word);
        assert_eq!(clicks.press(5, 4, HistoryMoment(400)), Granularity::Line);
        assert_eq!(
            clicks.press(5, 4, HistoryMoment(2_000)),
            Granularity::Character
        );

        assert_eq!(word_range("osc(20).luma()", 1), 0..3);
        assert_eq!(word_range("osc(20).luma()", 3), 3..4);
        assert_eq!(word_range("  spaced", 1), 0..2);
    }
}
