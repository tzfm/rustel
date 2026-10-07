//! A code minimap and scrollbar for the right edge of the source pane.
//!
//! The pane minimap reduces the editor's screen rows, including wrapping
//! and inline layout, and shares their coordinates with scrolling. The
//! reduction is cached until text, layout, or minimap dimensions change.
//! Braille dots and pixel rows compress tall documents to fit the column;
//! above [`MAX_MINIMAP_LINES`] rows the column is a proportional
//! scrollbar.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Widget;

use super::editor::{Document, Editor, EditorError, OverviewLayout, Revision, ScreenRow, TextRow};
use super::syntax::{self, Token};
use super::theme::{Theme, mix};
use super::visuals::BRAILLE_BITS;

/// The most source rows a minimap summarises.
pub const MAX_MINIMAP_LINES: usize = 20_000;
/// The most characters of a line the pixel picture keeps.
const PICTURE_COLUMNS: usize = 160;
/// Braille dots per cell, across and down.
const DOTS_ACROSS: usize = 2;
const DOTS_DOWN: usize = 4;

/// One cell: which of its eight dots are lit, and the most distinctive
/// token class among the text they stand for.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Cell {
    bits: u8,
    token: Token,
}

/// The picture the pixel tier draws: every line's characters as tokens.
#[derive(Clone, Debug, Default)]
struct Picture {
    cell: (u16, u16),
    /// Pixel rows one reduced source row takes, gap included.
    px_per_line: usize,
    lines: Vec<Vec<Option<Token>>>,
}

/// Cached reduction of a document into minimap rows.
#[derive(Clone, Debug, Default)]
pub struct Minimap {
    revision: Option<Revision>,
    layout: Option<OverviewLayout>,
    cell_pixels: Option<(u16, u16)>,
    width: u16,
    height: u16,
    tier: Option<super::graphics::Tier>,
    /// Source rows folded into each Braille dot row or pixel picture line.
    lines_per_dot_row: usize,
    line_count: usize,
    rows: Vec<Vec<Cell>>,
    picture: Option<Picture>,
}

impl Minimap {
    /// Whether this document is small enough to summarise.
    pub fn supports(document: &Document) -> bool {
        document.line_count() <= MAX_MINIMAP_LINES
    }

    /// A document thumbnail with one source row per logical line. Pane
    /// minimaps use [`Self::sync_editor`] to follow the editor's layout.
    pub fn sync(&mut self, document: &Document, area: Rect) {
        if !self.prepare(document.revision(), None, document.line_count(), area) {
            return;
        }
        for line in 0..self.line_count {
            self.add_row(line, &picture_line(&document.line_content(line)));
        }
    }

    /// Rebuild from the pane's display rows when text or layout changes.
    /// Scrolling only moves the viewport band; it does not rebuild the map.
    pub fn sync_editor(&mut self, editor: &Editor, area: Rect) -> Result<(), EditorError> {
        if !self.prepare(
            editor.document().revision(),
            Some(editor.overview_layout()),
            editor.visual_row_count(),
            area,
        ) {
            return Ok(());
        }
        let columns = self
            .picture
            .as_ref()
            .map_or(usize::from(area.width) * DOTS_ACROSS, |picture| {
                usize::from(area.width) * usize::from(picture.cell.0)
            });
        let source_columns = editor.viewport().page_columns.max(columns);
        let mut tokens = LineTokens::default();
        let result = editor.visit_overview_rows(|row| {
            if let ScreenRow::Text(row) = row {
                let reduced = tokens.reduce(editor.document(), row, source_columns, columns);
                self.add_row(row.global_row, &reduced);
            }
        });
        if result.is_err() {
            // A partial reduction must be retried on the next frame.
            self.revision = None;
        }
        result
    }

    fn prepare(
        &mut self,
        revision: Revision,
        layout: Option<OverviewLayout>,
        line_count: usize,
        area: Rect,
    ) -> bool {
        let tier = super::graphics::tier();
        let cell_pixels = super::graphics::cell_pixels();
        if self.revision == Some(revision)
            && self.layout == layout
            && self.width == area.width
            && self.height == area.height
            && self.tier == Some(tier)
            && self.cell_pixels == cell_pixels
        {
            return false;
        }
        self.revision = Some(revision);
        self.layout = layout;
        self.width = area.width;
        self.height = area.height;
        self.tier = Some(tier);
        self.cell_pixels = cell_pixels;
        self.rows.clear();
        self.picture = None;
        self.line_count = line_count.max(1);
        self.lines_per_dot_row = 1;
        if area.is_empty() || self.line_count > MAX_MINIMAP_LINES {
            return false;
        }
        if tier == super::graphics::Tier::Pixels
            && let Some(cell) = cell_pixels
        {
            let height_px = usize::from(area.height) * usize::from(cell.1);
            let px_per_line = (height_px / self.line_count).clamp(1, 3);
            let slots_per_row = (usize::from(cell.1) / px_per_line).max(1);
            let available = usize::from(area.height) * slots_per_row;
            self.lines_per_dot_row = self.line_count.div_ceil(available).max(1);
            self.picture = Some(Picture {
                cell,
                px_per_line,
                lines: vec![Vec::new(); self.line_count.div_ceil(self.lines_per_dot_row)],
            });
        } else {
            let available = usize::from(area.height) * DOTS_DOWN;
            self.lines_per_dot_row = self.line_count.div_ceil(available).max(1);
            let dot_rows = self.line_count.div_ceil(self.lines_per_dot_row);
            self.rows =
                vec![vec![Cell::default(); usize::from(area.width)]; dot_rows.div_ceil(DOTS_DOWN)];
        }
        true
    }

    fn add_row(&mut self, row: usize, tokens: &[Option<Token>]) {
        let reduced = row / self.lines_per_dot_row;
        if let Some(picture) = &mut self.picture {
            let line = &mut picture.lines[reduced];
            line.resize(line.len().max(tokens.len()), None);
            for (target, token) in line.iter_mut().zip(tokens) {
                merge_token(target, *token);
            }
        } else {
            summarise_tokens(
                tokens,
                reduced % DOTS_DOWN,
                &mut self.rows[reduced / DOTS_DOWN],
            );
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.picture.is_none()
    }

    /// Source rows covered by one terminal row of the map.
    pub fn lines_per_row(&self) -> usize {
        if let Some(picture) = &self.picture {
            return (usize::from(picture.cell.1) / picture.px_per_line.max(1)).max(1)
                * self.lines_per_dot_row;
        }
        self.lines_per_dot_row.max(1) * DOTS_DOWN
    }

    pub fn line_count(&self) -> usize {
        self.line_count
    }

    /// First source row summarised by the row at `y` of `area`.
    /// For pane minimaps this is an editor screen row, including inline rows.
    pub fn line_at(&self, area: Rect, y: u16) -> usize {
        if area.height == 0 {
            return 0;
        }
        let row = usize::from(y.saturating_sub(area.y));
        if self.is_empty() {
            // Proportional scrollbar: the whole document maps onto the bar.
            return (row * self.line_count / usize::from(area.height).max(1))
                .min(self.line_count.saturating_sub(1));
        }
        (row * self.lines_per_row()).min(self.line_count.saturating_sub(1))
    }

    /// Rows of `area` that summarise lines `first..=last`.
    fn rows_covering(&self, area: Rect, first: usize, last: usize) -> std::ops::Range<u16> {
        if self.is_empty() {
            let height = usize::from(area.height);
            let lines = self.line_count.max(1);
            let start = (first * height / lines).min(height.saturating_sub(1));
            let end = ((last + 1) * height)
                .div_ceil(lines)
                .clamp(start + 1, height);
            return start as u16..end as u16;
        }
        let per_row = self.lines_per_row();
        let start = (first / per_row).min(usize::from(area.height));
        let end = (last / per_row + 1).min(usize::from(area.height));
        start as u16..end.max(start) as u16
    }
}

/// Accumulate a reduced source row into Braille cells. Several rows may
/// share a dot; a non-text token takes precedence over plain text.
fn summarise_tokens(tokens: &[Option<Token>], dot_row: usize, cells: &mut [Cell]) {
    for (dot, token) in tokens.iter().enumerate().take(cells.len() * DOTS_ACROSS) {
        let Some(token) = token else { continue };
        let cell = &mut cells[dot / DOTS_ACROSS];
        cell.bits |= BRAILLE_BITS[dot % DOTS_ACROSS][dot_row];
        if cell.token == Token::Text {
            cell.token = *token;
        }
    }
}

fn merge_token(target: &mut Option<Token>, token: Option<Token>) {
    if token.is_some() && matches!(target, None | Some(Token::Text)) {
        *target = token;
    }
}

/// Classify each logical line once, then place its visible graphemes using
/// the editor's cell spans. Tabs, wide glyphs, indentation and inline widths
/// occupy the same proportions as in the pane.
#[derive(Default)]
struct LineTokens {
    line: Option<usize>,
    tokens: Vec<(usize, Token)>,
}

impl LineTokens {
    fn reduce(
        &mut self,
        document: &Document,
        row: &TextRow,
        source_columns: usize,
        columns: usize,
    ) -> Vec<Option<Token>> {
        if self.line != Some(row.line) {
            self.line = Some(row.line);
            let content = document.line_content(row.line);
            let clusters: Vec<String> = content.chars().map(|c| c.to_string()).collect();
            let classes = syntax::classify(clusters.iter().map(String::as_str));
            let start = document.line_content_range(row.line).start.get();
            self.tokens = content
                .char_indices()
                .zip(classes)
                .map(|((offset, _), token)| (start + offset, token))
                .collect();
        }
        let mut reduced = vec![None; columns];
        for cell in &row.cells {
            if !cell.widened && cell.display.chars().all(char::is_whitespace) {
                continue;
            }
            let token = self
                .tokens
                .binary_search_by_key(&cell.bytes.start.get(), |(offset, _)| *offset)
                .ok()
                .map(|index| self.tokens[index].1)
                .unwrap_or_default();
            let first = usize::from(cell.screen_x.start) * columns / source_columns;
            let end = (usize::from(cell.screen_x.end) * columns)
                .div_ceil(source_columns)
                .min(columns);
            for target in &mut reduced[first.min(columns)..end] {
                merge_token(target, Some(token));
            }
        }
        reduced
    }
}

/// One line of the picture: a token per character, `None` for space.
fn picture_line(line: &str) -> Vec<Option<Token>> {
    let clusters: Vec<String> = line.chars().map(|c| c.to_string()).collect();
    let classes = syntax::classify(clusters.iter().map(String::as_str));
    line.chars()
        .zip(classes)
        .take(PICTURE_COLUMNS)
        .map(|(character, token)| (!character.is_whitespace()).then_some(token))
        .collect()
}

/// The minimap column, or a proportional scrollbar when the document is too
/// large to summarise.
pub struct MinimapView<'a> {
    pub minimap: &'a Minimap,
    pub theme: &'a Theme,
    /// First and last source row currently visible; pane maps use screen rows.
    pub viewport: (usize, usize),
}

impl Widget for MinimapView<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        super::view::clear_surface(buffer, area, Style::default().bg(self.theme.minimap));
        let (first_visible, last_visible) = self.viewport;
        let band = self
            .minimap
            .rows_covering(area, first_visible, last_visible);

        if self.minimap.is_empty() {
            render_scrollbar(buffer, area, self.theme, band);
            return;
        }

        if let Some(picture) = &self.minimap.picture {
            self.render_picture(picture, area, buffer, &band);
            return;
        }

        for row in 0..area.height {
            let Some(cells) = self.minimap.rows.get(usize::from(row)) else {
                break;
            };
            let y = area.y + row;
            let in_viewport = band.contains(&row);
            if in_viewport {
                buffer.set_style(
                    Rect::new(area.x, y, area.width, 1),
                    Style::default().bg(self.theme.minimap_viewport),
                );
            }
            for (column, cell) in cells.iter().enumerate().take(usize::from(area.width)) {
                if cell.bits == 0 {
                    continue;
                }
                let symbol = char::from_u32(0x2800 + u32::from(cell.bits)).unwrap_or(' ');
                let color = shade(cell.token.color(self.theme), self.theme, in_viewport);
                if let Some(target) = buffer.cell_mut((area.x + column as u16, y)) {
                    target.set_char(symbol).set_fg(color);
                }
            }
        }
    }
}

impl MinimapView<'_> {
    /// The pixel tier's map: the code as a picture, the viewport as a
    /// lighter band behind it - what a graphical editor shows.
    fn render_picture(
        &self,
        picture: &Picture,
        area: Rect,
        buffer: &mut Buffer,
        band: &std::ops::Range<u16>,
    ) {
        let mut canvas = super::visuals::Canvas::with_raster(
            area,
            super::visuals::Raster::Pixels {
                cell_width: picture.cell.0,
                cell_height: picture.cell.1,
            },
        );
        let (width, height) = (canvas.width(), canvas.height());
        let cell_height = usize::from(picture.cell.1).max(1);
        let lines_per_row = (cell_height / picture.px_per_line.max(1)).max(1);
        for row in 0..usize::from(area.height) {
            let in_viewport = band.contains(&(row as u16));
            if in_viewport {
                for y in row * cell_height..((row + 1) * cell_height).min(height) {
                    for x in 0..width {
                        canvas.set(x, y, self.theme.minimap_viewport);
                    }
                }
            }
            for slot in 0..lines_per_row {
                let line = row * lines_per_row + slot;
                let Some(tokens) = picture.lines.get(line) else {
                    break;
                };
                let y0 = row * cell_height + slot * picture.px_per_line;
                let rows_lit = picture.px_per_line.saturating_sub(1).max(1);
                for (x, token) in tokens.iter().enumerate().take(width) {
                    let Some(token) = token else {
                        continue;
                    };
                    let color = shade(token.color(self.theme), self.theme, in_viewport);
                    for y in y0..(y0 + rows_lit).min(height) {
                        canvas.set(x, y, color);
                    }
                }
            }
        }
        canvas.paint(buffer, self.theme.minimap);
    }
}

/// Dim the part of the map outside the viewport so the visible window reads
/// as the focused region rather than as a colour change.
fn shade(color: Color, theme: &Theme, in_viewport: bool) -> Color {
    if in_viewport {
        color
    } else {
        mix(color, theme.minimap, 0.45)
    }
}

fn render_scrollbar(buffer: &mut Buffer, area: Rect, theme: &Theme, band: std::ops::Range<u16>) {
    for row in 0..area.height {
        let inside = band.contains(&row);
        let x = area.x + area.width / 2;
        if let Some(cell) = buffer.cell_mut((x, area.y + row)) {
            cell.set_symbol(if inside { "┃" } else { "│" })
                .set_fg(if inside { theme.accent } else { theme.rule });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(text: &str) -> Document {
        Document::new(text, 1 << 20).expect("document")
    }

    #[test]
    fn a_short_document_folds_four_lines_into_each_row() {
        let document = document("aaa\nbbb\n\nccc\nddd\n");
        let mut minimap = Minimap::default();
        minimap.sync(&document, Rect::new(0, 0, 8, 20));
        assert_eq!(minimap.lines_per_row(), 4);
        assert_eq!(minimap.rows.len(), 2);
        assert_eq!(minimap.line_at(Rect::new(0, 0, 8, 20), 1), 4);
        // The blank third line leaves its dot row dark; the others are lit.
        let first = &minimap.rows[0][0];
        assert_eq!(first.bits & BRAILLE_BITS[0][2], 0, "the blank line is dark");
        assert_ne!(first.bits & BRAILLE_BITS[0][0], 0);
        assert_ne!(first.bits & BRAILLE_BITS[0][3], 0);
    }

    #[test]
    fn a_long_document_compresses_several_lines_into_each_dot() {
        let text = (0..800)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let document = document(&text);
        let mut minimap = Minimap::default();
        let area = Rect::new(0, 0, 10, 20);
        minimap.sync(&document, area);
        assert_eq!(minimap.lines_per_row(), 40);
        assert_eq!(minimap.rows.len(), 20);
        assert_eq!(minimap.line_at(area, 10), 400);
        // Clicking past the end still resolves to a real line.
        assert!(minimap.line_at(area, 200) < 800);
    }

    #[test]
    fn resyncing_after_an_edit_rebuilds_and_an_idle_frame_does_not() {
        let mut document = document("one\n");
        let mut minimap = Minimap::default();
        let area = Rect::new(0, 0, 8, 4);
        minimap.sync(&document, area);
        let first = minimap.rows.clone();
        minimap.sync(&document, area);
        assert_eq!(minimap.rows, first, "an unchanged document is not rebuilt");

        document
            .apply_edits(&[super::super::editor::Edit::new(
                super::super::editor::ByteOffset(0)..super::super::editor::ByteOffset(0),
                "// a comment\n",
            )])
            .expect("edit");
        minimap.sync(&document, area);
        assert_ne!(minimap.rows, first);
    }

    #[test]
    fn density_and_class_survive_into_the_summary() {
        let mut cells = vec![Cell::default(); 8];
        summarise_tokens(&picture_line("    s(\"bd\")"), 0, &mut cells);
        assert_eq!(cells[0].bits, 0, "leading whitespace stays dark");
        assert_eq!(
            cells[1].bits, 0,
            "one dot per column: columns 2 and 3 are spaces too"
        );
        assert_ne!(cells[2].bits, 0, "the `s` at column 4");
        // The quoted sound name is summarised as a string, not plain text.
        assert!(cells.iter().any(|cell| cell.token == Token::String));
    }

    #[test]
    fn the_viewport_band_is_highlighted_and_the_rest_is_dimmed() {
        let text = (0..160)
            .map(|index| format!("s(\"bd\") // {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let document = document(&text);
        let theme = Theme::built_in_default();
        let area = Rect::new(0, 0, 8, 20);
        let mut minimap = Minimap::default();
        minimap.sync(&document, area);
        let mut buffer = Buffer::empty(area);
        MinimapView {
            minimap: &minimap,
            theme: &theme,
            viewport: (0, 12),
        }
        .render(area, &mut buffer);

        // 160 lines over 20 rows is eight lines a row: lines 0..=12 light
        // the first two rows and nothing further down.
        let background = |y: u16| buffer.cell((0, y)).map(|cell| cell.bg);
        assert_eq!(background(0), Some(theme.minimap_viewport));
        assert_eq!(background(1), Some(theme.minimap_viewport));
        assert_eq!(background(2), Some(theme.minimap));
        assert_eq!(background(10), Some(theme.minimap));
        let glyph = buffer
            .cell((0, 0))
            .unwrap()
            .symbol()
            .chars()
            .next()
            .unwrap();
        assert!(('\u{2800}'..='\u{28ff}').contains(&glyph), "{glyph:?}");
    }

    #[test]
    fn an_oversized_document_falls_back_to_a_scrollbar() {
        let text = "x\n".repeat(MAX_MINIMAP_LINES + 10);
        let document = document(&text);
        let mut minimap = Minimap::default();
        let area = Rect::new(0, 0, 6, 20);
        minimap.sync(&document, area);
        assert!(minimap.is_empty());

        let theme = Theme::built_in_default();
        let mut buffer = Buffer::empty(area);
        MinimapView {
            minimap: &minimap,
            theme: &theme,
            viewport: (0, 19),
        }
        .render(area, &mut buffer);
        let column = (0..area.height)
            .filter_map(|y| buffer.cell((area.x + area.width / 2, y)))
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(column.starts_with('┃'), "{column}");
        assert!(column.contains('│'), "{column}");
        // The bar still maps back onto document lines.
        assert!(minimap.line_at(area, 10) > 9_000);
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use crate::editor::{ByteOffset, GridRect, InlineWidth, VirtualRowSpec};

    fn wrapped(source: &str, columns: usize, rows: usize) -> Editor {
        let mut editor = Editor::new(source).expect("editor");
        editor.set_view_size(columns, rows);
        editor.set_wrap(true);
        editor.set_wrap_indents(vec![0]);
        editor
    }

    #[test]
    fn wrapped_rows_share_the_minimap_density_band_and_pointer_coordinates() {
        let mut editor = wrapped(&"abcdefgh".repeat(8), 9, 4);
        let area = Rect::new(20, 3, 5, 10);
        let mut minimap = Minimap::default();
        minimap.sync_editor(&editor, area).expect("minimap");
        assert_eq!(editor.document().line_count(), 1);
        assert_eq!(minimap.line_count(), 8);
        assert_eq!(minimap.rows.len(), 2);
        assert!(minimap.rows[1].iter().any(|cell| cell.bits != 0));
        assert_eq!(minimap.line_at(area, area.y + 1), 4);
        assert_eq!(minimap.rows_covering(area, 4, 7), 1..2);

        let mut viewport = editor.viewport();
        viewport.top_row = 4;
        editor.set_viewport(viewport);
        let map = editor.screen_map(GridRect::new(0, 0, 9, 4)).expect("map");
        assert!(
            matches!(&map.rows()[0], ScreenRow::Text(row) if row.line == 0 && row.segment == 4)
        );
        let layout = minimap.layout.clone();
        let density = minimap.rows.clone();
        minimap
            .sync_editor(&editor, area)
            .expect("scrolled minimap");
        assert_eq!(
            minimap.layout, layout,
            "scrolling leaves the cache key unchanged"
        );
        assert_eq!(minimap.rows, density);

        let theme = Theme::built_in_default();
        let mut buffer = Buffer::empty(area);
        MinimapView {
            minimap: &minimap,
            theme: &theme,
            viewport: (4, 7),
        }
        .render(area, &mut buffer);
        assert_eq!(buffer[(area.x, area.y)].bg, theme.minimap);
        assert_eq!(buffer[(area.x, area.y + 1)].bg, theme.minimap_viewport);
    }

    #[test]
    fn the_summary_uses_the_whole_pane_width_and_display_columns() {
        let mut editor = Editor::new("\t界                  x").expect("editor");
        editor.set_view_size(25, 4);
        let mut minimap = Minimap::default();
        let area = Rect::new(0, 0, 5, 4);
        minimap.sync_editor(&editor, area).expect("minimap");
        assert_eq!(
            minimap.rows[0][0].bits & BRAILLE_BITS[0][0],
            0,
            "leading tab is blank"
        );
        assert_ne!(
            minimap.rows[0][4].bits, 0,
            "text at the right edge is retained"
        );
        assert!(
            minimap.rows[0][1..4].iter().any(|cell| cell.bits == 0),
            "interior spaces remain blank"
        );
    }

    #[test]
    fn layout_changes_rebuild_without_a_document_edit() {
        let mut editor = wrapped(&"abcdefgh".repeat(8), 9, 4);
        let revision = editor.revision();
        let area = Rect::new(0, 0, 5, 10);
        let mut minimap = Minimap::default();
        minimap.sync_editor(&editor, area).expect("minimap");
        assert_eq!(minimap.line_count(), 8);
        editor.set_view_size(17, 4);
        minimap.sync_editor(&editor, area).expect("resized");
        assert_eq!(minimap.line_count(), 4);
        editor.set_wrap(false);
        minimap.sync_editor(&editor, area).expect("unwrapped");
        assert_eq!(minimap.line_count(), 1);
        editor.set_wrap(true);
        editor.set_inline_widths(vec![InlineWidth {
            at: ByteOffset(0),
            extra: 16,
        }]);
        minimap.sync_editor(&editor, area).expect("inline width");
        assert!(minimap.line_count() > 4);
        let text_rows = minimap.line_count();
        editor
            .set_virtual_rows(
                revision,
                vec![VirtualRowSpec::new("plot", ByteOffset(0), 4)],
            )
            .expect("inline block");
        minimap.sync_editor(&editor, area).expect("inline rows");
        assert_eq!(minimap.line_count(), text_rows + 4);
        assert_eq!(editor.revision(), revision);

        editor.clear_virtual_rows();
        editor.set_inline_widths(Vec::new());
        editor.set_wrap_indents(vec![6]);
        minimap
            .sync_editor(&editor, area)
            .expect("continuation indentation");
        assert!(minimap.line_count() > 4);
        assert_eq!(minimap.line_count(), editor.scroll_extent().2);
    }

    #[test]
    fn inline_blocks_keep_later_text_at_its_screen_row() {
        let mut editor = wrapped("abc\ndef", 9, 4);
        editor
            .set_virtual_rows(
                editor.revision(),
                vec![VirtualRowSpec::new("plot", ByteOffset(0), 3)],
            )
            .expect("inline block");
        let mut minimap = Minimap::default();
        minimap
            .sync_editor(&editor, Rect::new(0, 0, 5, 10))
            .expect("minimap");
        assert_eq!(minimap.line_count(), 5);
        for bit in BRAILLE_BITS[0].iter().skip(1) {
            assert_eq!(minimap.rows[0][0].bits & bit, 0);
        }
        assert_ne!(minimap.rows[1][0].bits & BRAILLE_BITS[0][0], 0);
    }

    #[test]
    fn pixel_density_band_and_pointer_use_the_same_compression() {
        let _renderer = crate::graphics::HeldRenderer::pixels((8, 16));
        let mut minimap = Minimap::default();
        let area = Rect::new(2, 5, 5, 4);
        let editor = wrapped(&"abcdefgh".repeat(8), 9, 4);
        minimap.sync_editor(&editor, area).expect("short map");
        assert_eq!(minimap.line_at(area, area.y + 1), 5);
        assert_eq!(minimap.rows_covering(area, 5, 7), 1..2);

        let editor = wrapped(&"abcdefgh".repeat(160), 9, 4);
        let mut minimap = Minimap::default();
        minimap.sync_editor(&editor, area).expect("tall map");
        assert_eq!(minimap.line_count(), 160);
        assert_eq!(minimap.lines_per_row(), 48);
        assert_eq!(minimap.line_at(area, area.y + 2), 96);
        assert_eq!(minimap.rows_covering(area, 96, 143), 2..3);
        assert_eq!(minimap.picture.as_ref().expect("picture").lines.len(), 54);
        assert!(
            minimap
                .picture
                .as_ref()
                .unwrap()
                .lines
                .last()
                .unwrap()
                .iter()
                .any(Option::is_some)
        );
        let theme = Theme::built_in_default();
        let mut buffer = Buffer::empty(area);
        MinimapView {
            minimap: &minimap,
            theme: &theme,
            viewport: (96, 143),
        }
        .render(area, &mut buffer);
        let images = crate::graphics::take_images();
        assert_eq!(images.len(), 1);
        assert_eq!((images[0].width, images[0].height), (40, 64));
        assert_eq!(
            images[0].rgba[53 * 40 * 4 + 3],
            255,
            "the final compressed source row is painted below the viewport band"
        );
        assert_eq!(images[0].rgba[54 * 40 * 4 + 3], 0);
    }

    #[test]
    fn too_many_wrapped_rows_use_a_proportional_scrollbar() {
        let source = format!("{}\n", "abcdefgh".repeat(100)).repeat(300);
        let mut editor = Editor::new(&source).expect("editor");
        editor.set_view_size(9, 4);
        editor.set_wrap(true);
        let mut minimap = Minimap::default();
        let area = Rect::new(0, 0, 5, 10);
        minimap.sync_editor(&editor, area).expect("minimap");
        assert!(minimap.is_empty());
        assert!(minimap.line_count() > MAX_MINIMAP_LINES);
        assert!(editor.document().line_count() < MAX_MINIMAP_LINES);
        assert_eq!(minimap.line_at(area, 5), minimap.line_count() / 2);
        assert_eq!(minimap.line_at(area, 100), minimap.line_count() - 1);
    }
}
