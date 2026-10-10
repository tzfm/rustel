//! Text selection and scrolling in entry and snippet bodies.

use super::*;

impl ReferencePanel {
    /// The selectable text block under the pointer, if any: the open
    /// entry's body, or the selected snippet's code rows.
    pub fn text_block_at(
        &self,
        reference: &Reference,
        inner: Rect,
        x: u16,
        y: u16,
    ) -> Option<(SelectionTarget, Rect, Vec<String>, usize)> {
        let hit = |area: Rect| area.contains(ratatui::layout::Position::new(x, y));
        match self.tab {
            // Chords and scales are rows to press, not text to drag out.
            Tab::Chords | Tab::Scales => None,
            #[cfg(feature = "vst")]
            Tab::Vst => None,
            Tab::Reference => {
                let ReferenceMode::Entry { index, scroll, .. } = self.mode else {
                    return None;
                };
                let area = entry_body_area(inner);
                // The hint line the body stops one above is the block's
                // empty half, not chrome: a press there begins the
                // selection at the end of the text, the way a drag out
                // of a pane's blank space does in an editor or browser.
                let claimed = if area.contains(ratatui::layout::Position::new(x, y)) {
                    area
                } else if y == area.bottom() && x >= inner.x && x < inner.right() {
                    Rect::new(area.x, area.y, area.width, area.height + 1)
                } else {
                    return None;
                };
                if !hit(claimed) {
                    return None;
                }
                let entry = reference.entry(index)?;
                let mut lines: Vec<String> = entry_body(entry, usize::from(inner.width))
                    .into_iter()
                    .map(|line| line.text)
                    .collect();
                let max_scroll = lines.len().saturating_sub(usize::from(area.height)) as u16;
                let first = usize::from(scroll.min(max_scroll));
                // The window's text ends at the last visible line: rows
                // past it - the claimed hint row among them - are empty
                // half, and an anchor there is the text's edge, never a
                // phantom line's column.
                lines.truncate(first + usize::from(area.height));
                Some((
                    SelectionTarget::Entry {
                        index,
                        width: inner.width,
                    },
                    claimed,
                    lines,
                    first,
                ))
            }
            #[cfg(feature = "hydra")]
            Tab::Examples | Tab::Generator => {
                let rows = self.snippet_layout(inner).code_rows();
                if !hit(rows) {
                    return None;
                }
                let lines =
                    self.snippet_code_lines(usize::from(rows.height), usize::from(rows.width));
                if lines.is_empty() {
                    return None;
                }
                Some((
                    SelectionTarget::Snippet {
                        row: self.snippet_selected,

                        width: rows.width,
                    },
                    rows,
                    lines,
                    0,
                ))
            }
            Tab::Samples => None,
        }
    }

    /// The block a held selection refers to, when its tag still describes
    /// what is on screen; a mismatch is a selection that no longer exists.
    pub fn selection_block(
        &self,
        reference: &Reference,
        inner: Rect,
    ) -> Option<(Rect, Vec<String>, usize)> {
        let held = self.selection.as_ref()?;
        match held.target {
            SelectionTarget::Entry { index, width } => {
                if width != inner.width || self.tab != Tab::Reference {
                    return None;
                }
                let ReferenceMode::Entry {
                    index: open,
                    scroll,
                    ..
                } = self.mode
                else {
                    return None;
                };
                if open != index {
                    return None;
                }
                let entry = reference.entry(index)?;
                let area = entry_body_area(inner);
                let mut lines: Vec<String> = entry_body(entry, usize::from(width))
                    .into_iter()
                    .map(|line| line.text)
                    .collect();
                let max_scroll = lines.len().saturating_sub(usize::from(area.height)) as u16;
                let first = usize::from(scroll.min(max_scroll));
                // Same window as `text_block_at`: the text ends at the
                // last visible line, so a drag carried below the body -
                // onto the hint row or past the pane - anchors at its
                // edge rather than at an offscreen line's column.
                lines.truncate(first + usize::from(area.height));
                Some((area, lines, first))
            }
            #[cfg(feature = "hydra")]
            SelectionTarget::Snippet { row, width } => {
                let rows = self.snippet_layout(inner).code_rows();
                if !self.tab.is_snippets() || self.snippet_selected != row || width != rows.width {
                    return None;
                }
                let lines =
                    self.snippet_code_lines(usize::from(rows.height), usize::from(rows.width));
                (!lines.is_empty()).then_some((rows, lines, 0))
            }
        }
    }

    /// What a copy takes: the dragged text, when the drag still describes
    /// the screen and holds something.
    pub fn live_selection_text(&self, reference: &Reference, inner: Rect) -> Option<String> {
        let held = self.selection.as_ref()?;
        if held.selection.is_empty() {
            return None;
        }
        let (_, lines, _) = self.selection_block(reference, inner)?;
        let text = held.selection.text(&lines);
        #[cfg(feature = "hydra")]
        let text = self.snippet_source_text(&held.selection, inner, text);
        (!text.trim().is_empty()).then_some(text)
    }

    /// The shelf's drag read back as the bytes its rows were drawn from.
    ///
    /// A drag is held in the rows it is drawn in, and stays so - the
    /// highlight, the hit test and the selection all keep their rows. What
    /// moves is only the string handed to the clipboard, because a drawn row
    /// is not the source: a continuation row carries an indent that is
    /// display alone, a seam leaves its space behind on the row before it,
    /// and a chain too wide for any seam is cut on a character boundary,
    /// which lands inside a string as readily as outside one. Copying the
    /// drawing so hands back an unterminated literal, or a mini-notation
    /// with a newline and two spaces in the middle of it.
    #[cfg(feature = "hydra")]
    fn snippet_source_text(
        &self,
        selection: &super::super::textblock::TextSelection,
        inner: Rect,
        drawn: String,
    ) -> String {
        if !self.tab.is_snippets() {
            return drawn;
        }
        let Some(code) = self.selected_snippet_code() else {
            return drawn;
        };
        let rows = self.snippet_layout(inner).code_rows();
        let spans = self.snippet_code_rows(usize::from(rows.height), usize::from(rows.width));
        let (from, to) = selection.ordered();
        let mut out = String::new();
        let mut row = from.line;
        while row <= to.line {
            let Some((index, first)) = spans.get(row) else {
                break;
            };
            let index = *index;
            let Some(source) = code.lines().nth(index) else {
                break;
            };
            // The rest of this logical line's rows, so far as the drag
            // reaches them. A line the room cut short ends at the last row
            // drawn, which is all a drag over it can mean: reaching past it
            // would copy text that was never on screen.
            let mut last_row = row;
            while let Some((next_index, _)) = spans.get(last_row + 1) {
                if *next_index != index || last_row + 1 > to.line {
                    break;
                }
                last_row += 1;
            }
            let Some((_, last)) = spans.get(last_row) else {
                break;
            };
            // One slice across the run rather than the rows joined: the
            // space a seam trims belongs to neither row, and only the source
            // still has it.
            let start = if row == from.line {
                Self::byte_at(source, first, from.column)
            } else {
                first.from
            };
            let end = if last_row == to.line {
                Self::byte_at(source, last, to.column)
            } else {
                last.to
            };
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&source[start.min(end)..end.max(start)]);
            row = last_row + 1;
        }
        if out.is_empty() {
            return drawn;
        }
        out
    }

    /// The byte of `source` that a drawn column of this row points at: past
    /// the indent, which is display and not in the line at all, and then
    /// that many characters into the bytes the row shows. A column past the
    /// row's end is the row's end.
    #[cfg(feature = "hydra")]
    fn byte_at(source: &str, row: &WrappedRow, column: usize) -> usize {
        let piece = &source[row.from..row.to];
        let wanted = column.saturating_sub(row.indent);
        let mut bytes = 0;
        for (taken, (offset, character)) in piece.char_indices().enumerate() {
            if taken >= wanted {
                break;
            }
            bytes = offset + character.len_utf8();
        }
        row.from + bytes
    }

    /// The code rows the snippet block draws: wrapped the way the renderer
    /// wraps them, and cut off where it stops drawing.
    ///
    /// One row here is one row on screen, which is what the selection
    /// model needs. A chain too long for the column is drawn as several
    /// rows, and handing back logical lines instead made a drag address
    /// the wrong text: the second screen row of a wrapped chain was read
    /// as the second line of the snippet, so the highlight landed a row
    /// below the pointer and `c` copied another line altogether. The same
    /// mismatch cut the list by logical lines while the renderer stops by
    /// rows, which let a selection reach text that was never drawn.
    #[cfg(feature = "hydra")]
    pub fn snippet_code_lines(&self, room: usize, width: usize) -> Vec<String> {
        self.snippet_code_rows(room, width)
            .into_iter()
            .map(|(_, row)| row.text)
            .collect()
    }

    /// The same rows, with the logical line each is part of and the bytes of
    /// that line it shows. The highlight wants the rows and the copy wants
    /// the bytes, and both have to agree on where the renderer stopped.
    #[cfg(feature = "hydra")]
    pub(super) fn snippet_code_rows(&self, room: usize, width: usize) -> Vec<(usize, WrappedRow)> {
        let rows = self.all_snippet_code_rows(width);
        let first = self
            .snippet_code_scroll
            .get()
            .min(rows.len().saturating_sub(room));
        self.snippet_code_scroll.set(first);
        rows.into_iter().skip(first).take(room).collect()
    }

    #[cfg(feature = "hydra")]
    pub(super) fn all_snippet_code_rows(&self, width: usize) -> Vec<(usize, WrappedRow)> {
        let Some(code) = self.selected_snippet_code() else {
            return Vec::new();
        };
        code.lines()
            .enumerate()
            .flat_map(|(index, line)| {
                wrap_code_spans(line, width)
                    .into_iter()
                    .map(move |row| (index, row))
            })
            .collect()
    }

    #[cfg(feature = "hydra")]
    pub fn scroll_snippet_code(&mut self, inner: Rect, delta: isize) {
        let rows = self.snippet_layout(inner).code_rows();
        let last = self
            .all_snippet_code_rows(usize::from(rows.width))
            .len()
            .saturating_sub(usize::from(rows.height));
        self.snippet_code_scroll.set(
            self.snippet_code_scroll
                .get()
                .saturating_add_signed(delta)
                .min(last),
        );
        self.selection = None;
    }

    /// The same thumb geometry is used for drawing and dragging.
    #[cfg(feature = "hydra")]
    pub fn snippet_scrollbar(&self, inner: Rect) -> Option<(Rect, u16, u16, usize)> {
        if !self.tab.is_snippets() {
            return None;
        }
        let rows = self.snippet_layout(inner).code_rows();
        if rows.height < 2 {
            return None;
        }
        let total = self.all_snippet_code_rows(usize::from(rows.width)).len();
        let last = total.saturating_sub(usize::from(rows.height));
        if last == 0 {
            return None;
        }
        let length = (usize::from(rows.height).pow(2) / total).max(1) as u16;
        let travel = rows.height - length;
        let first = self.snippet_code_scroll.get().min(last);
        let top = ((first * usize::from(travel) + last / 2) / last) as u16;
        Some((
            Rect::new(rows.right(), rows.y, 1, rows.height),
            top,
            length,
            last,
        ))
    }
}
