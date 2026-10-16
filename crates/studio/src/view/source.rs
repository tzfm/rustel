use super::*;

/// The text grid of a pane: `area` less the gutter, which `numbered`
/// gives the line numbers or takes away.
pub fn source_grid(editor: &Editor, area: Rect, numbered: bool) -> GridRect {
    let gutter = gutter_width(editor, numbered);
    GridRect::new(
        area.x.saturating_add(gutter),
        area.y,
        area.width.saturating_sub(gutter),
        area.height,
    )
}

/// What the gutter keeps without line numbers: text should not touch the
/// pane's edge.
const MARGIN_WIDTH: u16 = 1;

pub(crate) fn gutter_width(editor: &Editor, numbered: bool) -> u16 {
    if !numbered {
        return MARGIN_WIDTH;
    }
    let digits = editor.document().line_count().max(1).ilog10() as u16 + 1;
    digits.saturating_add(2).clamp(4, 8)
}

/// The name row over a pane in split view: which scene it shows, whether
/// it is the sounding one, and whether the caret is here.
pub(super) struct PaneTitle<'a> {
    pub keybinds: &'a crate::keybinds::Keybinds,
    pub(super) name: &'a str,
    pub(super) dirty: bool,
    pub(super) playing: bool,
    pub(super) focused: bool,
    pub(super) prebake: bool,
    pub(super) replay: bool,
    pub(super) theme: &'a Theme,
}

impl Widget for PaneTitle<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let theme = self.theme;
        clear_surface(
            buffer,
            area,
            Style::default().bg(theme.surface).fg(theme.muted),
        );
        let text = format!(
            " {}{}{} ",
            if self.prebake {
                format!("{PREBAKE_GLYPH} ")
            } else if self.replay {
                format!("{REPLAY_GLYPH} ")
            } else if self.playing {
                "▶ ".to_owned()
            } else {
                String::new()
            },
            self.name,
            if self.dirty { " ●" } else { "" }
        );
        let style = if self.focused {
            Style::default()
                .fg(theme.background)
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else if self.playing {
            Style::default().fg(theme.ok)
        } else {
            Style::default().fg(theme.foreground)
        };
        let title_width = UnicodeWidthStr::width(text.as_str());
        buffer.set_stringn(area.x, area.y, text, usize::from(area.width), style);
        // Titles focus their pane, and that is all they do: no click
        // plays a scene. A pad does, and so does an update.
        if self.focused {
            return;
        }
        let Some(binding) = self.keybinds.binding(crate::keybinds::BindAction::HopPane) else {
            return;
        };
        let switch = format!("{} switches", binding.hint());
        let hints: &[&str] = &[&switch];
        if let Some((hint, width)) = hints.iter().find_map(|&hint| {
            let width = UnicodeWidthStr::width(hint);
            (usize::from(area.width) >= title_width + width + 2).then_some((hint, width as u16))
        }) {
            buffer.set_stringn(
                area.right().saturating_sub(width + 1),
                area.y,
                hint,
                usize::from(width),
                Style::default().fg(theme.muted),
            );
        }
    }
}

/// First and last screen row the source pane is showing, inline rows included.
pub(super) fn visible_rows(map: &ScreenMap) -> (usize, usize) {
    let first = map.viewport().top_row;
    (
        first,
        first.saturating_add(map.rows().len().saturating_sub(1)),
    )
}

/// The right edge of a pane without a minimap: a rule the height of the
/// pane, with a quiet handle where the page sits in the text. A press or
/// a drag on it scrolls, as on the minimap.
pub(super) struct Scrollbar<'a> {
    pub(super) editor: &'a Editor,
    pub(super) theme: &'a Theme,
}

pub(super) struct HorizontalScrollbar<'a> {
    pub(super) extent: (usize, usize, usize),
    pub(super) theme: &'a Theme,
}

pub(crate) fn horizontal_scrollbar_thumb(
    extent: (usize, usize, usize),
    width: u16,
) -> (usize, usize) {
    let (left, page, total) = extent;
    let width = usize::from(width);
    if width == 0 || total == 0 {
        return (0, width);
    }
    let rounded_ratio = |value: usize, numerator: usize, denominator: usize| {
        value
            .saturating_mul(numerator)
            .saturating_add(denominator / 2)
            / denominator
    };
    let furthest = total.saturating_sub(page);
    // A usable handle on long scores, with travel even for tiny overflow.
    let maximum_length = if furthest > 0 && width > 1 {
        width - 1
    } else {
        width
    };
    let length = rounded_ratio(width, page, total).clamp(3.min(maximum_length), maximum_length);
    let travel = width.saturating_sub(length);
    let start = if furthest == 0 {
        0
    } else {
        rounded_ratio(travel, left.min(furthest), furthest).min(travel)
    };
    (start, length)
}

/// Inverse of the painted thumb geometry, with exact endpoints.
pub(crate) fn horizontal_scrollbar_left(
    extent: (usize, usize, usize),
    width: u16,
    thumb: usize,
) -> usize {
    let (_, length) = horizontal_scrollbar_thumb(extent, width);
    let travel = usize::from(width).saturating_sub(length);
    let maximum = extent.2.saturating_sub(extent.1);
    if travel == 0 {
        return 0;
    }
    thumb
        .min(travel)
        .saturating_mul(maximum)
        .saturating_add(travel / 2)
        / travel
}

impl Widget for HorizontalScrollbar<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let width = usize::from(area.width);
        if width == 0 {
            return;
        }
        let (start, length) = horizontal_scrollbar_thumb(self.extent, area.width);
        for column in 0..width {
            let on_handle = column >= start && column < start + length;
            if let Some(cell) = buffer.cell_mut((area.x + column as u16, area.y)) {
                cell.set_symbol("─").set_fg(if on_handle {
                    self.theme.muted
                } else {
                    self.theme.rule
                });
            }
        }
    }
}

impl Scrollbar<'_> {
    pub(super) fn needed(editor: &Editor) -> bool {
        let (_, page, total) = editor.scroll_extent();
        total > page
    }

    /// The handle's first row and height on a bar `height` tall.
    pub fn handle(editor: &Editor, height: u16) -> (u16, u16) {
        let (top, page, total) = editor.scroll_extent();
        let height = usize::from(height);
        if height == 0 || total <= page {
            return (0, height as u16);
        }
        let length = (height * page / total).clamp(1, height);
        let furthest = total.saturating_sub(1).max(1);
        let start = (height - length) * top.min(furthest) / furthest;
        (start as u16, length as u16)
    }
}

impl Widget for Scrollbar<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let (start, length) = Self::handle(self.editor, area.height);
        for row in 0..area.height {
            let on_handle = row >= start && row < start + length;
            if let Some(cell) = buffer.cell_mut((area.x, area.y + row)) {
                cell.set_symbol(if on_handle { "┃" } else { "│" })
                    .set_fg(if on_handle {
                        self.theme.muted
                    } else {
                        self.theme.rule
                    });
            }
        }
    }
}

pub(super) struct EditorPane<'a> {
    pub(super) editor: &'a Editor,
    pub(super) map: &'a ScreenMap,
    pub(super) line_numbers: bool,
    pub(super) visual: &'a VisualState,
    pub(super) decorations: Decorations<'a>,
    pub(super) theme: &'a Theme,
    pub(super) area: Rect,
}

impl Widget for EditorPane<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let theme = self.theme;
        buffer.set_style(
            area,
            Style::default().bg(theme.background).fg(theme.foreground),
        );
        let gutter = gutter_width(self.editor, self.line_numbers);
        let current_line = self
            .editor
            .document()
            .line_of(self.editor.primary_selection().head)
            .unwrap_or(0);
        // Inline widgets describe the last audible generation and remain in
        // place while its source is edited.
        let visuals = self
            .visual
            .layout()
            .map(|layout| {
                layout
                    .visuals
                    .iter()
                    .map(|visual| {
                        (
                            visual.id.as_str(),
                            (visual.kind.as_str(), visual.slot, visual.options.as_str()),
                        )
                    })
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let decorations = DecorationIndex::new(self.map, self.editor, self.decorations);

        // Keep the full virtual-widget geometry even when its top or bottom
        // is outside the viewport. Rendering at the clipped height would
        // rescale a scope or piano roll while scrolling and make it jump.
        let mut virtual_groups = BTreeMap::<&str, (u16, u16, u16, u16)>::new();
        for row in self.map.rows() {
            match row {
                ScreenRow::Text(row) => render_text_row(
                    buffer,
                    row,
                    self.area.x,
                    self.area.width,
                    gutter,
                    current_line,
                    &decorations,
                    self.decorations.sliders,
                    theme,
                    row_tokens(self.editor.document(), row),
                ),
                ScreenRow::Virtual(row) => {
                    let entry = virtual_groups.entry(row.id.as_ref()).or_insert((
                        row.screen_y,
                        0,
                        row.inner_row,
                        row.height,
                    ));
                    entry.1 = entry.1.saturating_add(1);
                    entry.2 = entry.2.min(row.inner_row);
                    if gutter > MARGIN_WIDTH
                        && let Some(cell) =
                            buffer.cell_mut((self.area.x + gutter - 1, row.screen_y))
                    {
                        cell.set_symbol("┊").set_fg(theme.rule);
                    }
                }
            }
        }

        for (id, (y, height, offset, full_height)) in virtual_groups {
            let visual_area = Rect::new(
                self.area.x.saturating_add(gutter),
                y,
                self.area.width.saturating_sub(gutter),
                height,
            );
            if let Some((kind, slot, options)) = visuals.get(id).copied() {
                render_clipped_visual(
                    kind,
                    slot,
                    options,
                    self.visual,
                    theme,
                    theme.background,
                    visual_area,
                    offset,
                    full_height,
                    buffer,
                );
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct VisibleCell {
    key: (u16, u16),
    from: usize,
    to: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct CellDecorations {
    mini: bool,
    /// Which of the row's slider chips this cell belongs to.
    slider: Option<u8>,
    error: bool,
    selected: bool,
    /// The sounding event's colour and how strongly it still marks the cell.
    active: Option<(Color, f32)>,
    /// A bracket at the caret: `Some(true)` matched, `Some(false)` orphan.
    bracket: Option<bool>,
    /// A tempo call an outside clock overrides.
    overridden: bool,
}

/// Per-frame decoration lookup. Building it assigns each visible cell at
/// most once per decoration layer, so thousands of source ranges do not turn
/// into thousands of comparisons for every terminal cell.
struct DecorationIndex {
    caret_shape: crate::terminal::CaretShape,
    by_screen_cell: HashMap<(u16, u16), CellDecorations>,
}

impl DecorationIndex {
    fn new(map: &ScreenMap, editor: &Editor, decorations: Decorations<'_>) -> Self {
        let mut cells = map
            .rows()
            .iter()
            .filter_map(|row| match row {
                ScreenRow::Text(row) => Some(row),
                ScreenRow::Virtual(_) => None,
            })
            .flat_map(|row| {
                row.cells.iter().map(|cell| VisibleCell {
                    key: (cell.screen_x.start, row.screen_y),
                    from: cell.bytes.start.0,
                    to: cell.bytes.end.0,
                })
            })
            .collect::<Vec<_>>();
        cells.sort_unstable_by_key(|cell| (cell.from, cell.to, cell.key.1, cell.key.0));
        debug_assert!(cells.windows(2).all(|pair| pair[0].to <= pair[1].from));

        let mut mini_cells = vec![None; cells.len()];
        let _ = assign_first_overlapping(
            &cells,
            decorations.mini.iter().map(|&(from, to)| (from, to, ())),
            &mut mini_cells,
        );

        let mut slider_cells = vec![None; cells.len()];
        let _ = assign_first_overlapping(
            &cells,
            decorations
                .sliders
                .iter()
                .enumerate()
                .map(|(index, chip)| (chip.from, chip.to, index.min(u8::MAX as usize) as u8)),
            &mut slider_cells,
        );

        let mut error_cells = vec![None; cells.len()];
        let _ = assign_first_overlapping(
            &cells,
            decorations.errors.iter().map(|&(from, to)| (from, to, ())),
            &mut error_cells,
        );

        let mut overridden_cells = vec![None; cells.len()];
        let _ = assign_first_overlapping(
            &cells,
            decorations
                .overridden
                .iter()
                .map(|&(from, to)| (from, to, ())),
            &mut overridden_cells,
        );

        let mut bracket_cells = vec![None; cells.len()];
        let _ = assign_first_overlapping(
            &cells,
            decorations
                .brackets
                .iter()
                .map(|&(from, to, matched)| (from, to, matched)),
            &mut bracket_cells,
        );

        let mut selected_cells = vec![None; cells.len()];
        let selections = editor.selections().ranges().iter().filter_map(|selection| {
            if selection.is_empty() {
                None
            } else {
                let selected = selection.ordered();
                Some((selected.start.0, selected.end.0, ()))
            }
        });
        let _ = assign_first_overlapping(&cells, selections, &mut selected_cells);

        // SourceMark order is observable: the old renderer used `find`, so
        // the first overlapping mark supplied the foreground color.
        let mut active_cells = vec![None; cells.len()];
        let _ = assign_first_overlapping(
            &cells,
            decorations
                .active
                .iter()
                .map(|mark| (mark.from, mark.to, (mark.color, mark.strength))),
            &mut active_cells,
        );

        let mut by_screen_cell = HashMap::with_capacity(cells.len());
        for (index, cell) in cells.into_iter().enumerate() {
            by_screen_cell.insert(
                cell.key,
                CellDecorations {
                    mini: mini_cells[index].is_some(),
                    slider: slider_cells[index],
                    error: error_cells[index].is_some(),
                    selected: selected_cells[index].is_some(),
                    active: active_cells[index],
                    bracket: bracket_cells[index],
                    overridden: overridden_cells[index].is_some(),
                },
            );
        }
        Self {
            by_screen_cell,
            caret_shape: decorations.caret_shape,
        }
    }

    fn get(&self, screen_x: u16, screen_y: u16) -> CellDecorations {
        self.by_screen_cell
            .get(&(screen_x, screen_y))
            .copied()
            .unwrap_or_default()
    }
}

/// Assign the first input range overlapping each visible cell. The
/// disjoint-set skips cells that already have their precedence winner, which
/// bounds the inner loop by the number of visible cells rather than by
/// `cells * ranges`. The returned count makes that invariant testable without
/// timing-sensitive benchmarks.
fn assign_first_overlapping<T: Copy>(
    cells: &[VisibleCell],
    ranges: impl IntoIterator<Item = (usize, usize, T)>,
    assigned: &mut [Option<T>],
) -> usize {
    debug_assert_eq!(cells.len(), assigned.len());
    let mut unassigned = NextUnassigned::new(cells.len());
    let mut assignment_count = 0;
    for (from, to, value) in ranges {
        if from >= to {
            continue;
        }
        let first = cells.partition_point(|cell| cell.to <= from);
        let limit = cells.partition_point(|cell| cell.from < to);
        let mut index = unassigned.find(first);
        while index < limit {
            assigned[index] = Some(value);
            assignment_count += 1;
            index = unassigned.claim(index);
        }
    }
    assignment_count
}

struct NextUnassigned {
    parent: Vec<usize>,
}

impl NextUnassigned {
    fn new(len: usize) -> Self {
        Self {
            parent: (0..=len).collect(),
        }
    }

    fn find(&mut self, index: usize) -> usize {
        let mut root = index;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        let mut cursor = index;
        while self.parent[cursor] != cursor {
            let next = self.parent[cursor];
            self.parent[cursor] = root;
            cursor = next;
        }
        root
    }

    fn claim(&mut self, index: usize) -> usize {
        let next = self.find(index + 1);
        self.parent[index] = next;
        next
    }
}

/// Draw a visualization at its full height and blit the visible slice.
///
/// An inline widget scrolling past the top of the pane must not be redrawn
/// at the clipped height: a piano roll would rescale its lanes and a scope
/// its amplitude, so the whole picture would jump as the caret moved.
#[allow(clippy::too_many_arguments)]
fn render_clipped_visual(
    kind: &str,
    slot: Option<u8>,
    options: &str,
    visual: &VisualState,
    theme: &Theme,
    background: Color,
    visible_area: Rect,
    row_offset: u16,
    full_height: u16,
    destination: &mut Buffer,
) {
    if visible_area.is_empty() || full_height == 0 {
        return;
    }
    let full_area = Rect::new(0, 0, visible_area.width, full_height);
    let mut layer = Buffer::empty(full_area);
    layer.set_style(
        full_area,
        Style::default().bg(background).fg(theme.foreground),
    );
    let parsed = VisualOptions::parse(options);
    if !settings::animation() {
        layer.set_string(
            full_area.x,
            full_area.y,
            format!("_{kind}() - animation is off in settings"),
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::ITALIC),
        );
        blit_rows(&layer, row_offset, visible_area, destination);
        return;
    }
    // On the pixel tier a visualizer paints no cells at all: it hands the
    // frame an image, and that image carries the rectangle it was drawn
    // in - which here is the scratch layer's, at the origin. Blitting the
    // cells home would leave the picture at the top-left corner of the
    // screen, at its unclipped height, over whatever is there. So the
    // images are captured with the layer and moved home with it, the way
    // the visualizer dock already moves its own.
    let ((), images) = crate::graphics::capture_images(|| {
        crate::visuals::render(
            VisualRequest {
                kind,
                slot,
                options: &parsed,
                state: visual,
                theme,
                background,
                inline: true,
            },
            full_area,
            &mut layer,
        );
    });
    blit_rows(&layer, row_offset, visible_area, destination);
    let source = Rect::new(0, row_offset, visible_area.width, visible_area.height);
    for image in images {
        if let Some(image) = image.crop_and_move(source, (visible_area.x, visible_area.y)) {
            crate::graphics::push_image(image);
        }
    }
}

fn blit_rows(source: &Buffer, source_y: u16, destination_area: Rect, destination: &mut Buffer) {
    for row in 0..destination_area.height {
        let from_y = source_y.saturating_add(row);
        for column in 0..destination_area.width {
            let Some(cell) = source.cell((source.area.x.saturating_add(column), from_y)) else {
                continue;
            };
            if let Some(target) = destination.cell_mut((
                destination_area.x.saturating_add(column),
                destination_area.y.saturating_add(row),
            )) {
                *target = cell.clone();
            }
        }
    }
}

/// The row's tokens, resolved against the whole line it is part of.
///
/// Two different things need context beyond the row, and they need different
/// amounts of it. A string or comment only needs what came before, which is
/// what [`Lexer::primed`] carries. A word needs both sides: a row can begin or
/// end in the middle of one, and whether `stack` is a call depends on a `(`
/// that may not be on this row at all. Classifying only the row's own cells
/// reads `const` wrapped after `co` as the non-word `nst`, and leaves `sta`
/// plain while `ck` on the next row goes bold.
///
/// So the line is classified whole - the prefix, the row, and the remainder -
/// and the row's own run of tokens is taken back out of the middle.
fn row_tokens(document: &Document, row: &TextRow) -> Vec<Token> {
    let own = |document: &Document| {
        syntax::classify_from(
            lexer_for_row(document, row),
            row.cells.iter().map(|cell| cell.display.as_str()),
        )
    };
    let (Some(first), Some(last)) = (row.cells.first(), row.cells.last()) else {
        return Vec::new();
    };
    let Ok(line) = document.line_of(first.bytes.start) else {
        return own(document);
    };
    // The line's own bytes, terminator excluded - `line_start(line + 1)`
    // would not do, because it clamps to the last line and so returns this
    // line's own start on the final one, leaving no suffix at all and the
    // last call on the last line uncoloured.
    let content = document.line_content_range(line);
    let start = content.start;
    let end = content.end.max(last.bytes.end);
    let (Ok(prefix), Ok(suffix)) = (
        document.slice(start..first.bytes.start),
        document.slice(last.bytes.end..end),
    ) else {
        return own(document);
    };
    // Characters, not graphemes, for the two ends: they are only counted and
    // read for word shape and a following `(`, both of which are ASCII, while
    // the row's own cells stay the clusters the renderer actually drew.
    let before: Vec<String> = prefix.chars().map(|c| c.to_string()).collect();
    let after: Vec<String> = suffix.chars().map(|c| c.to_string()).collect();
    let tokens = syntax::classify(
        before
            .iter()
            .map(String::as_str)
            .chain(row.cells.iter().map(|cell| cell.display.as_str()))
            .chain(after.iter().map(String::as_str)),
    );
    let taken: Vec<Token> = tokens
        .into_iter()
        .skip(before.len())
        .take(row.cells.len())
        .collect();
    // A slice that did not come out whole falls back rather than painting the
    // row from somebody else's offsets.
    if taken.len() == row.cells.len() {
        taken
    } else {
        own(document)
    }
}

/// The lexical state `row` starts in: its line read up to the row's first
/// cell, and not a character further.
///
/// A screen row is not a line. Scrolled sideways, or wrapped, a row begins
/// somewhere in the middle of one, and a lexer started there has no idea
/// whether it stands inside a string or a comment. Read from `bd").seg(4)`,
/// the quote that closes `s("bd")` opens a string instead, and the rest of
/// the row - and every row below it - is painted as one.
///
/// The prefix is bounded by the line, so this is proportional to what is on
/// screen the way the rest of the draw loop is.
fn lexer_for_row(document: &Document, row: &TextRow) -> Lexer {
    // `content` is the whole line; `cells` is the window of it this row
    // draws. The first cell is where the row actually starts, which is what
    // the prefix has to reach.
    let Some(first) = row.cells.first() else {
        return Lexer::default();
    };
    let Ok(line) = document.line_of(first.bytes.start) else {
        return Lexer::default();
    };
    let start = document.line_start(line);
    if start >= first.bytes.start {
        return Lexer::default();
    }
    match document.slice(start..first.bytes.start) {
        Ok(prefix) => Lexer::primed(&prefix),
        Err(_) => Lexer::default(),
    }
}

#[allow(clippy::too_many_arguments)]
fn render_text_row(
    buffer: &mut Buffer,
    row: &TextRow,
    pane_x: u16,
    pane_width: u16,
    gutter: u16,
    current_line: usize,
    decorations: &DecorationIndex,
    chips: &[SliderChip],
    theme: &Theme,
    // The row's finished tokens, resolved against its whole line - see
    // `row_tokens`. A row is not a line, and can work out neither its string
    // state nor its word boundaries for itself.
    lexical: Vec<Token>,
) {
    let is_current = row.line == current_line;
    // A full-width band across the caret's line is a strong decoration for a
    // weak fact - you can see where the caret is - and over Hydra it is a bar
    // laid across the picture. Themes that want one say so; none of the
    // shipped ones do.
    if is_current && let Some(colour) = theme.current_line {
        buffer.set_style(
            Rect::new(pane_x, row.screen_y, pane_width, 1),
            Style::default().bg(colour),
        );
    }
    // A wrapped line is numbered once, on its first row; the rows it
    // continues on keep the gutter blank, the way every editor shows it.
    // A gutter no wider than the margin carries no number.
    if gutter > MARGIN_WIDTH {
        let number = if row.segment == 0 {
            format!("{:>width$} ", row.line + 1, width = usize::from(gutter - 1))
        } else {
            " ".repeat(usize::from(gutter))
        };
        buffer.set_stringn(
            pane_x,
            row.screen_y,
            number,
            usize::from(gutter),
            Style::default().fg(if is_current {
                theme.accent
            } else {
                theme.muted
            }),
        );
    }

    for (index, cell) in row.cells.iter().enumerate() {
        let token = lexical.get(index).copied().unwrap_or_default();
        let mut style = Style::default().fg(token.color(theme));
        let decoration = decorations.get(cell.screen_x.start, row.screen_y);
        if decoration.mini {
            style = style.fg(theme.mini);
            if let Some(fill) = theme.mini_fill {
                style = style.bg(fill);
            }
        }
        if decoration.slider.is_some() {
            // A live control reads as a chip: the accent on a faint tint of
            // itself, so it is obviously something to drag rather than type.
            // Where the pill fits on the row, the control itself is drawn
            // over this after the text pass.
            style = style
                .fg(theme.accent)
                .bg(mix(theme.background, theme.accent, 0.25))
                .add_modifier(Modifier::BOLD);
        }
        if decoration.overridden {
            // `setcpm` while an outside clock owns the tempo: shown, struck
            // through, so nobody wonders why it did nothing.
            style = style.fg(theme.muted).add_modifier(Modifier::CROSSED_OUT);
        }
        if let Some(matched) = decoration.bracket {
            style = theme.bracket_style(style, decorations.caret_shape, matched);
        }
        if let Some((color, strength)) = decoration.active {
            // How a sounding event marks its text is a theme decision. The
            // default tints its background and keeps the text readable.
            // A mark letting go eases back to the plain style.
            let marked = theme.event_mark.apply(style, color, theme.background);
            style = if strength >= 1.0 {
                marked
            } else {
                crate::theme::fade_mark(style, marked, strength, theme.foreground, theme.background)
            };
        }
        if decoration.error {
            // The linter's underline, in the error colour, over whatever
            // else the cell carries: a problem is never hidden by a mark.
            //
            // It runs after the sounding mark, so the mark cannot paint
            // over it. A mark says "this is sounding now" and comes back
            // every cycle; an error says "this will not run" and stays
            // until it is fixed, so the error is the one that must stay
            // visible.
            style = style
                .underline_color(theme.error)
                .add_modifier(Modifier::UNDERLINED | Modifier::BOLD);
        }
        if decoration.selected {
            // After the error, and last of all: `selection_text` is picked
            // to be readable on `selection`, and a foreground set by
            // anything else on top of that background is a colour nobody
            // chose the pair for. What is selected is what the hand is
            // holding, and it has to stay legible while it is held.
            style = style.bg(theme.selection).fg(theme.selection_text);
        }
        buffer.set_stringn(
            cell.screen_x.start,
            row.screen_y,
            &cell.display,
            usize::from(cell.screen_x.end.saturating_sub(cell.screen_x.start)),
            style,
        );
    }

    // The live controls. A chip whose whole cover sits on this row is drawn
    // as a control: a track with the knob where the value sits, covering the
    // cover's text completely. A clipped or wrapped cover keeps its text
    // under the chip tint instead.
    let mut runs: Vec<(u8, u16, u16, usize, usize)> = Vec::new();
    for cell in &row.cells {
        let Some(index) = decorations.get(cell.screen_x.start, row.screen_y).slider else {
            continue;
        };
        match runs.last_mut() {
            Some((chip, _, x1, _, byte_to)) if *chip == index && *x1 == cell.screen_x.start => {
                *x1 = cell.screen_x.end;
                *byte_to = cell.bytes.end.0;
            }
            _ => runs.push((
                index,
                cell.screen_x.start,
                cell.screen_x.end,
                cell.bytes.start.0,
                cell.bytes.end.0,
            )),
        }
    }
    for (index, x0, x1, byte_from, byte_to) in runs {
        let Some(chip) = chips.get(usize::from(index)) else {
            continue;
        };
        if byte_from != chip.from
            || byte_to != chip.to
            || slider_cover_bounds(row, chip.from, chip.to) != Some((x0, x1))
        {
            continue;
        }
        let background = if is_current {
            theme.current_line.unwrap_or(theme.background)
        } else {
            theme.background
        };
        // The drawn control is atomic: a source selection touching its
        // cover selects the complete rail, including the expanded cells.
        let selected = row.cells.iter().any(|cell| {
            cell.bytes.start.0 >= chip.from
                && cell.bytes.end.0 <= chip.to
                && decorations.get(cell.screen_x.start, row.screen_y).selected
        });
        draw_slider_control(
            buffer,
            Rect::new(x0, row.screen_y, x1 - x0, 1),
            chip,
            theme,
            background,
            selected,
        );
    }
}

/// The complete visible cover, shared by drawing and pointer input. A
/// clipped or wrapped cover stays text instead of becoming a shorter rail.
pub(crate) fn slider_cover_bounds(row: &TextRow, from: usize, to: usize) -> Option<(u16, u16)> {
    if from < row.cells.first()?.bytes.start.0 || to > row.cells.last()?.bytes.end.0 {
        return None;
    }
    let mut cells = row
        .cells
        .iter()
        .filter(|cell| cell.bytes.start.0 >= from && cell.bytes.end.0 <= to);
    let first = cells.next()?;
    let mut last = first;
    for cell in std::iter::once(first).chain(cells) {
        if usize::from(cell.screen_x.end.saturating_sub(cell.screen_x.start)) != cell.columns.len()
        {
            return None;
        }
        last = cell;
    }
    (first.bytes.start.0 == from && last.bytes.end.0 == to)
        .then_some((first.screen_x.start, last.screen_x.end))
}

/// Track cells within the complete cover, shared by drawing and input.
pub fn slider_track_span(width: usize) -> Option<std::ops::Range<usize>> {
    // Every cell of the pill is track: `slider(` is seven cells, and a
    // knob wants all the room it can get.
    if width < 3 {
        return None;
    }
    Some(0..width)
}

pub(crate) fn draw_slider_control(
    buffer: &mut Buffer,
    area: Rect,
    chip: &SliderChip,
    theme: &Theme,
    background: Color,
    selected: bool,
) {
    let width = usize::from(area.width);
    if width == 0 {
        return;
    }
    let mut text = String::with_capacity(width * 3);
    match slider_track_span(width) {
        Some(track) => {
            for _ in 0..track.start {
                text.push(' ');
            }
            let cells = track.len();
            let knob = ((chip.notch * (cells - 1) as f64).round() as usize).min(cells - 1);
            for position in 0..cells {
                // A full block for the handle: it fills its cell exactly,
                // where a dot glyph sat off-baseline in many fonts.
                text.push(if position == knob { '█' } else { '─' });
            }
            while text.chars().count() < width {
                text.push(' ');
            }
        }
        None => {
            // Too narrow for any track at all: a plain tinted cover.
            while text.chars().count() < width {
                text.push(' ');
            }
        }
    }
    // Without a fill, later visuals can change the background. The rail
    // keeps its glyphs and accent colour.
    let mut style = Style::default()
        .fg(theme.accent)
        .bg(theme.slider_fill.unwrap_or(background))
        .add_modifier(Modifier::BOLD);
    // Focus changes the ground, leaving the accent handle and continuous
    // thin rail identical to the control the pointer first pressed.
    if chip.armed || selected {
        style = style.bg(theme.selection);
        // Mono deliberately uses its accent as the selection ground.
        // Keep the theme's contrasting pair when those colours coincide.
        if theme.accent == theme.selection {
            style = style.fg(theme.selection_text);
        }
    }
    // set_stringn patches existing cells. Source underlines, reverse,
    // dimming and bracket fills must not survive into parts of the rail.
    clear_surface(buffer, area, style);
    buffer.set_stringn(area.x, area.y, &text, width, style);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{ByteOffset, VirtualRowSpec};
    use crate::view::tests::*;

    /// An inline visualizer is drawn at its full height on a scratch layer
    /// and the visible slice copied home. On the pixel tier it paints no
    /// cells at all - it hands the frame a picture - so the picture has to
    /// be carried home with the cells. Left behind, it was placed at the
    /// scratch layer's own origin: the top-left corner of the screen, at
    /// full height, over the menu bar, and the widget's rows showed nothing.
    /// An inline painter with nothing to draw says so in its own band: the view
    /// renders it as inline, unlike a stage painter, which stays blank.
    #[test]
    fn an_inline_visualizer_with_nothing_to_draw_says_so_in_its_band() {
        let theme = Theme::resolve(None).expect("theme");
        let visual = VisualState::default();
        for (kind, note) in [
            ("scope", "waiting for audio"),
            ("pianoroll", "waiting for the first beat"),
        ] {
            let mut buffer = Buffer::empty(Rect::new(0, 0, 40, 3));
            render_clipped_visual(
                kind,
                None,
                "",
                &visual,
                &theme,
                theme.background,
                Rect::new(0, 0, 40, 3),
                0,
                3,
                &mut buffer,
            );
            let text: String = (0..3)
                .flat_map(|y| (0..40).map(move |x| (x, y)))
                .map(|at| buffer.cell(at).expect("cell").symbol().to_string())
                .collect();
            assert!(text.contains(note), "_{kind}() shows {note:?}: {text:?}");
        }
    }

    #[test]
    fn an_inline_visualizer_puts_its_picture_where_the_widget_is() {
        let _renderer = crate::graphics::HeldRenderer::pixels((2, 3));
        let theme = Theme::resolve(None).expect("theme");
        // A spiral draws once there is a clock to draw it against.
        let source = "$: s(\"bd\")._spiral()";
        let envelope = rustel_runtime::ui_events::visual_layout(source, 1).expect("layout");
        let revision = envelope.ui_layout.source_revision.clone();
        let mut visual = VisualState::default();
        visual.install_layout(envelope);
        visual.start();
        let batch = rustel_runtime::ui_events::UiEventBatch::new(
            10.0,
            0.0,
            1.0,
            1,
            revision,
            Vec::new(),
            0,
        )
        .expect("batch");
        assert!(visual.install_batch(batch));
        let mut buffer = Buffer::empty(Rect::new(0, 0, 80, 24));
        // The widget is twelve rows tall and scrolled four rows past the
        // top of the pane, so eight of them show, well down the screen.
        let visible = Rect::new(9, 7, 40, 8);
        render_clipped_visual(
            "spiral",
            None,
            "",
            &visual,
            &theme,
            theme.background,
            visible,
            4,
            12,
            &mut buffer,
        );
        let images = crate::graphics::take_images();
        assert_eq!(images.len(), 1, "the visualizer painted one picture");
        let image = &images[0];
        assert_eq!(image.area, visible, "placed on the widget's own rows");
        assert_eq!(
            image.cells,
            Some((visible.width, visible.height)),
            "and scaled into exactly those cells"
        );
        assert_eq!(
            image.height,
            u32::from(visible.height) * 3,
            "cropped to the rows in view, not the widget's full height"
        );
    }

    #[test]
    fn a_pane_without_a_minimap_has_a_scrollbar_whose_handle_tracks_the_page() {
        let area = Rect::new(0, 0, 100, 30);
        let layout = regions(area, false, 1, false, false, None, None);
        let pane = layout.panes[0];
        assert_eq!(pane.minimap.width, 0, "no minimap");
        assert_eq!(
            pane.scrollbar,
            Rect::new(99, pane.editor.y, 1, pane.editor.height)
        );
        assert_eq!(pane.editor.right(), 99);
        assert!(
            layout.pane_at(99, pane.editor.y + 1).is_some(),
            "the bar is the pane's"
        );

        let text = (0..100).map(|n| format!("{n}\n")).collect::<String>();
        let mut editor = Editor::new(&text).expect("editor");
        editor.set_view_size(80, 20);
        assert!(Scrollbar::needed(&editor), "overflow needs the bar");
        assert_eq!(
            Scrollbar::handle(&editor, 20),
            (0, 20 * 20 / 101),
            "a fifth of the text, at the top"
        );
        let mut viewport = editor.viewport();
        viewport.top_row = 100;
        editor.set_viewport(viewport);
        let (start, length) = Scrollbar::handle(&editor, 20);
        assert_eq!(start + length, 20, "the last row, at the bottom");
        let short = Editor::new("one\ntwo\n").expect("editor");
        assert!(
            !Scrollbar::needed(&short),
            "content that fits does not show a bar"
        );
        assert_eq!(
            Scrollbar::handle(&short, 20),
            (0, 20),
            "all of a short text: a full bar"
        );
    }

    #[test]
    fn horizontal_scrollbar_stays_grabbable_and_reaches_both_ends() {
        for width in [2, 3, 4, 23, 40, 80] {
            for (page, total) in [(79, 80), (20, 200), (80, 1_000_000)] {
                let maximum = total - page;
                let (start, length) = horizontal_scrollbar_thumb((0, page, total), width);
                assert_eq!(start, 0);
                assert!(length >= usize::from(width.saturating_sub(1).min(3)));
                assert!(
                    length < usize::from(width),
                    "even tiny overflow has thumb travel"
                );
                let travel = usize::from(width) - length;
                let mut previous = 0;
                for cell in 0..=travel {
                    let left = horizontal_scrollbar_left((0, page, total), width, cell);
                    assert!(left >= previous && left <= maximum);
                    previous = left;
                }
                assert_eq!(previous, maximum);
                let (end, size) = horizontal_scrollbar_thumb((maximum, page, total), width);
                assert_eq!(end + size, usize::from(width));
            }
        }
    }

    #[test]
    fn horizontal_scrollbar_thumb_uses_the_full_track() {
        assert_eq!(horizontal_scrollbar_thumb((0, 20, 200), 40), (0, 4));

        let (start, length) = horizontal_scrollbar_thumb((180, 20, 200), 40);
        assert_eq!(start + length, 40, "the final thumb touches the right edge");

        let (start, length) = horizontal_scrollbar_thumb((65, 17, 82), 23);
        assert_eq!(
            start + length,
            23,
            "non-divisible sizes also reach the edge"
        );
    }

    #[test]
    fn first_overlapping_range_preserves_style_precedence() {
        let cells = vec![
            VisibleCell {
                key: (0, 0),
                from: 0,
                to: 1,
            },
            VisibleCell {
                key: (1, 0),
                from: 1,
                to: 2,
            },
            // One displayed grapheme can cover several source bytes and
            // overlap ranges that begin inside it.
            VisibleCell {
                key: (2, 0),
                from: 2,
                to: 6,
            },
            VisibleCell {
                key: (3, 0),
                from: 10,
                to: 11,
            },
        ];
        let mut assigned = vec![None; cells.len()];

        let count =
            assign_first_overlapping(&cells, [(3, 4, 0_u8), (0, 6, 1), (1, 11, 2)], &mut assigned);

        assert_eq!(count, cells.len());
        assert_eq!(assigned, [Some(1), Some(1), Some(0), Some(2)]);
    }

    #[test]
    fn dense_ranges_assign_each_cell_only_once() {
        let cells = (0..512)
            .map(|index| VisibleCell {
                key: (index as u16, 0),
                from: index * 2,
                to: index * 2 + 1,
            })
            .collect::<Vec<_>>();
        let ranges = (0..rustel_runtime::ui_events::MAX_UI_LAYOUT_MINI_LOCATIONS)
            .map(|index| (0, cells.len() * 2, index));
        let mut assigned = vec![None; cells.len()];

        let count = assign_first_overlapping(&cells, ranges, &mut assigned);

        assert_eq!(count, cells.len());
        assert!(assigned.iter().all(|winner| *winner == Some(0)));
    }

    #[test]
    fn clipped_inline_layers_keep_their_full_height_geometry() {
        let mut source = Buffer::empty(Rect::new(0, 0, 3, 4));
        for (row, label) in ["AAA", "BBB", "CCC", "DDD"].into_iter().enumerate() {
            source.set_string(0, row as u16, label, Style::default());
        }
        let mut destination = Buffer::empty(Rect::new(0, 0, 8, 4));

        blit_rows(&source, 1, Rect::new(2, 1, 3, 2), &mut destination);

        let row = |y| {
            (2..5)
                .filter_map(|x| destination.cell((x, y)))
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        assert_eq!(row(1), "BBB");
        assert_eq!(row(2), "CCC");
    }

    /// However a row is clipped out of its line, it is coloured as that line:
    /// every visible cell has the colour it has when the whole line is on
    /// screen. See `row_tokens`.
    #[test]
    fn a_row_is_coloured_as_its_line_however_it_is_clipped() {
        let source = "const x = stack(s(\"bd\").fast(2))";
        // The line classified whole, which is the answer every clipping of it
        // has to agree with. ASCII, so a byte offset is a cluster index.
        let clusters: Vec<String> = source.chars().map(|c| c.to_string()).collect();
        let whole = syntax::classify(clusters.iter().map(String::as_str));

        let mut checked = 0usize;
        let mut saw_split_word = false;
        for width in [12u16, 18, 26, 60] {
            let mut editor = Editor::new(source).unwrap();
            // Scroll the line sideways a column at a time. That is what puts
            // a row's first cell in the middle of a word.
            for caret in 0..source.chars().count() {
                let area = Rect::new(0, 0, width, 4);
                let grid = source_grid(&editor, area, true);
                editor.set_view_size(usize::from(grid.width), usize::from(grid.height));
                let mut viewport = editor.viewport();
                viewport.left_column = caret;
                editor.set_viewport(viewport);
                let Ok(map) = editor.screen_map(grid) else {
                    continue;
                };
                for screen_row in map.rows() {
                    let ScreenRow::Text(row) = screen_row else {
                        continue;
                    };
                    let Some(first) = row.cells.first() else {
                        continue;
                    };
                    // A row starting inside a word is the case that used to
                    // lose its colour; note when the sweep reaches one.
                    if first.bytes.start.0 > 0
                        && source.as_bytes()[first.bytes.start.0 - 1].is_ascii_alphanumeric()
                        && source.as_bytes()[first.bytes.start.0].is_ascii_alphanumeric()
                    {
                        saw_split_word = true;
                    }
                    for (cell, token) in row.cells.iter().zip(row_tokens(editor.document(), row)) {
                        let at = cell.bytes.start.0;
                        assert_eq!(
                            token,
                            whole[at],
                            "at {width} columns with the caret at {caret}, byte {at} \
                             ({:?}) is {token:?} but the whole line says {:?}",
                            &source[at..(at + 1).min(source.len())],
                            whole[at]
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert!(
            checked > 500,
            "the sweep barely looked at anything: {checked}"
        );
        assert!(
            saw_split_word,
            "no row ever began inside a word, so the regression could not have been seen"
        );
    }

    /// Render one source row through the decoration pipeline and return the
    /// style landing on the cell for byte `offset`.
    fn styled_cell(
        theme: &Theme,
        source: &str,
        decorations: Decorations<'_>,
        offset: usize,
    ) -> ratatui::buffer::Cell {
        let mut editor = Editor::new(source).unwrap();
        let area = Rect::new(0, 0, 40, 3);
        let grid = source_grid(&editor, area, true);
        editor.set_view_size(usize::from(grid.width), usize::from(grid.height));
        let map = editor.screen_map(grid).unwrap();
        let mut buffer = Buffer::empty(area);
        let index = DecorationIndex::new(&map, &editor, decorations);
        let ScreenRow::Text(row) = &map.rows()[0] else {
            panic!("expected a text row");
        };
        render_text_row(
            &mut buffer,
            row,
            area.x,
            area.width,
            4,
            usize::MAX,
            &index,
            decorations.sliders,
            theme,
            row_tokens(editor.document(), row),
        );
        let cell = map
            .cell_for_offset(crate::editor::ByteOffset(offset))
            .expect("cell for offset");
        buffer
            .cell((cell.x, cell.y))
            .expect("rendered cell")
            .clone()
    }

    /// A row scrolled past the start of its line is coloured as that line.
    ///
    /// The pane is narrower than the score and the caret is at the end, so
    /// the row drawn begins in the middle of the line. A lexer started
    /// fresh there reads the closing quote of the string as an opening
    /// quote and paints every cell to the right edge as one string.
    #[test]
    fn a_row_scrolled_past_its_lines_opening_quote_keeps_its_colours() {
        let theme = theme();
        // Long enough that a narrow pane with the caret at the end starts
        // its row inside the string, so the row holds the quote that closes
        // it.
        let source = r#"$: s("bd sd hh oh bd sd hh oh").lpf(500)"#;
        let mut editor = Editor::new(source).unwrap();
        editor
            .set_selection(crate::editor::Selection::caret(crate::editor::ByteOffset(
                source.len(),
            )))
            .expect("caret at the end of the line");
        let area = Rect::new(0, 0, 20, 1);
        let grid = source_grid(&editor, area, true);
        editor.set_view_size(usize::from(grid.width), usize::from(grid.height));
        let map = editor.screen_map(grid).unwrap();
        let ScreenRow::Text(row) = &map.rows()[0] else {
            panic!("expected a text row");
        };
        let opening_quote = source.find('"').expect("the opening quote");
        let closing_quote = source.rfind('"').expect("the closing quote");
        let starts_at = row.cells.first().expect("a visible cell").bytes.start.0;
        assert!(
            starts_at > opening_quote && starts_at < closing_quote,
            "the row has to begin INSIDE the string for this to test anything: \
             starts at {starts_at}, quotes at {opening_quote} and {closing_quote}"
        );
        let mut buffer = Buffer::empty(area);
        let index = DecorationIndex::new(&map, &editor, Decorations::default());
        render_text_row(
            &mut buffer,
            row,
            area.x,
            area.width,
            4,
            usize::MAX,
            &index,
            &[],
            &theme,
            row_tokens(editor.document(), row),
        );
        // `500` is a number wherever the row happens to begin, and the `)`
        // beside it is punctuation - neither is inside a string.
        for (offset, wanted, what) in [
            (
                source.find("500").expect("the cutoff"),
                theme.syntax.number,
                "500",
            ),
            (
                source.len() - 1,
                theme.syntax.punctuation,
                "the closing paren",
            ),
        ] {
            let cell = map
                .cell_for_offset(crate::editor::ByteOffset(offset))
                .expect("cell for offset");
            let drawn = buffer.cell((cell.x, cell.y)).expect("rendered cell").fg;
            assert_eq!(
                drawn, wanted,
                "{what} was painted {drawn:?}, not {wanted:?} - the row read the closing quote as an opening one"
            );
        }
    }

    #[test]
    fn a_fading_mark_eases_back_to_the_plain_style() {
        let theme = theme();
        let base = Style::default()
            .fg(Color::Rgb(0, 0, 0))
            .bg(Color::Rgb(0, 0, 0));
        let marked = base
            .fg(Color::Rgb(200, 100, 0))
            .bg(Color::Rgb(100, 100, 100))
            .underline_color(Color::Rgb(200, 100, 0))
            .add_modifier(Modifier::BOLD);
        let half = crate::theme::fade_mark(base, marked, 0.5, theme.foreground, theme.background);
        assert_eq!(half.fg, Some(Color::Rgb(100, 50, 0)), "halfway in colour");
        assert_eq!(half.bg, Some(Color::Rgb(50, 50, 50)));
        assert!(
            half.add_modifier.contains(Modifier::BOLD),
            "the mark's weight while over half"
        );
        assert_eq!(half.underline_color, Some(Color::Rgb(200, 100, 0)));
        let faint = crate::theme::fade_mark(base, marked, 0.2, theme.foreground, theme.background);
        assert!(!faint.add_modifier.contains(Modifier::BOLD));
        assert_eq!(faint.underline_color, None);
        assert_eq!(faint.fg, Some(Color::Rgb(40, 20, 0)));
    }

    #[test]
    fn the_default_theme_tints_a_sounding_event_without_changing_syntax() {
        let theme = theme();
        let plain = styled_cell(&theme, "$: s(\"bd\")\n", Decorations::default(), 0);
        let marks = [SourceMark {
            from: 0,
            to: 4,
            color: Color::Rgb(255, 202, 40),
            onset_id: 1,
            strength: 1.0,
        }];
        let cell = styled_cell(
            &theme,
            "$: s(\"bd\")\n",
            Decorations {
                active: &marks,
                ..Decorations::default()
            },
            0,
        );
        assert_eq!(cell.fg, plain.fg);
        assert!(cell.modifier.contains(Modifier::BOLD));
        assert!(!cell.modifier.contains(Modifier::UNDERLINED));
        assert_eq!(cell.underline_color, Color::Reset);
        assert_ne!(cell.bg, Color::Reset, "the event should tint the cell");
        assert_ne!(
            cell.bg, marks[0].color,
            "the tint must not be a solid block"
        );
    }

    #[test]
    fn light_themes_render_visible_readable_sounding_marks() {
        for name in [
            "rustel-light",
            "solarized-light",
            "catppuccin-latte",
            "github-light",
            "emacs",
            "unicorn",
        ] {
            let theme = Theme::built_in(name).expect("bundled light theme");
            let marks = [SourceMark {
                from: 6,
                to: 8,
                color: theme.event,
                onset_id: 1,
                strength: 1.0,
            }];
            let cell = styled_cell(
                &theme,
                "$: s(\"bd\")\n",
                Decorations {
                    active: &marks,
                    ..Decorations::default()
                },
                6,
            );
            assert!(
                crate::theme::contrast_ratio(cell.bg, theme.background) >= 1.28,
                "{name}: mark disappeared"
            );
            assert!(
                crate::theme::contrast_ratio(cell.fg, cell.bg) >= 4.48,
                "{name}: marked string became unreadable"
            );
        }
    }

    #[test]
    fn mini_notation_is_tinted_rather_than_filled_by_default() {
        let theme = theme();
        assert_eq!(theme.mini_fill, None);
        let painted = styled_cell(
            &theme,
            "$: s(\"bd\")\n",
            Decorations {
                mini: &[(6, 8)],
                ..Decorations::default()
            },
            6,
        );
        assert_eq!(painted.fg, theme.mini);
        assert_eq!(painted.bg, Color::Reset);
    }

    #[test]
    fn a_lint_finding_is_underlined_in_the_error_colour() {
        let theme = theme();
        let source = "$: s(\"bd [hh\")\n";
        let from = source.find('[').unwrap();
        let painted = styled_cell(
            &theme,
            source,
            Decorations {
                mini: &[(from - 3, from + 3)],
                errors: &[(from, from + 1)],
                ..Decorations::default()
            },
            from,
        );
        assert_eq!(painted.fg, theme.mini);
        assert!(painted.modifier.contains(Modifier::UNDERLINED));
        assert_eq!(painted.underline_color, theme.error);
    }

    #[test]
    fn automatic_brackets_complement_all_six_caret_shapes() {
        use crate::terminal::CaretShape;
        let theme = theme();
        let source = "s(\"bd\")";
        for caret_shape in CaretShape::ALL {
            let block = matches!(
                caret_shape,
                CaretShape::SteadyUnderline | CaretShape::BlinkingUnderline
            );
            for matched in [true, false] {
                let painted = styled_cell(
                    &theme,
                    source,
                    Decorations {
                        brackets: &[(1, 2, matched)],
                        caret_shape,
                        ..Decorations::default()
                    },
                    1,
                );
                assert_eq!(
                    painted.modifier.contains(Modifier::UNDERLINED),
                    !block,
                    "{caret_shape:?}"
                );
                if block {
                    assert_ne!(painted.bg, theme.background);
                    assert_ne!(painted.fg, theme.background);
                    let contrast = crate::theme::contrast_ratio(painted.bg, theme.background);
                    assert!(
                        (1.4..1.55).contains(&contrast),
                        "{caret_shape:?}: {contrast}"
                    );
                } else {
                    assert_eq!(
                        painted.underline_color,
                        if matched {
                            theme.bracket()
                        } else {
                            theme.error
                        }
                    );
                }
            }
        }
    }

    #[test]
    fn matched_brackets_use_the_themes_underlined_highlight() {
        let mut theme = theme();
        theme.bracket_mark = crate::theme::BracketMark::Underline;
        let source = "$: s(\"bd\")\n";
        let from = source.find('(').unwrap();
        let painted = styled_cell(
            &theme,
            source,
            Decorations {
                brackets: &[(from, from + 1, true)],
                ..Decorations::default()
            },
            from,
        );
        assert_eq!(painted.fg, theme.syntax.punctuation);
        assert_eq!(painted.underline_color, theme.bracket());
        assert!(painted.modifier.contains(Modifier::UNDERLINED));
    }

    /// The control is upstream's: a pill over `slider(` alone, the numbers
    /// after it left as the text they are - and the pill takes room on its
    /// row to be twice the keyword, the text after it moving along.
    #[test]
    fn a_closed_slider_is_drawn_as_a_control_over_its_call() {
        let theme = theme();
        let source = "$: s(\"bd\").gain(slider(.25, 0, 1))\n";
        let call = source.find("slider(").unwrap();
        let end = call + "slider(".len();
        let chip = SliderChip {
            from: call,
            to: end,
            notch: 0.25,
            armed: false,
        };

        let mut editor = Editor::new(source).unwrap();
        editor.set_inline_widths(vec![crate::editor::InlineWidth {
            at: crate::editor::ByteOffset(end - 1),
            extra: 7,
        }]);
        let area = Rect::new(0, 0, 60, 3);
        let grid = source_grid(&editor, area, true);
        editor.set_view_size(usize::from(grid.width), usize::from(grid.height));
        let map = editor.screen_map(grid).unwrap();
        let mut buffer = Buffer::empty(area);
        let decorations = Decorations {
            sliders: std::slice::from_ref(&chip),
            ..Decorations::default()
        };
        let index = DecorationIndex::new(&map, &editor, decorations);
        let ScreenRow::Text(row) = &map.rows()[0] else {
            panic!("expected a text row");
        };
        render_text_row(
            &mut buffer,
            row,
            area.x,
            area.width,
            4,
            usize::MAX,
            &index,
            decorations.sliders,
            &theme,
            row_tokens(editor.document(), row),
        );

        let start = map
            .cell_for_offset(crate::editor::ByteOffset(call))
            .expect("cell for the call");
        let after = map
            .cell_for_offset(crate::editor::ByteOffset(end))
            .expect("cell for the value");
        assert_eq!(after.y, start.y);
        let width = after.x - start.x;
        assert_eq!(width, 14, "the pill is twice the keyword");
        // A press anywhere on the pill lands on the control, not on the
        // numbers after it.
        for column in start.x..after.x {
            let (offset, _) = row.hit_test(usize::from(column - grid.x));
            assert!(
                (call..end).contains(&offset.0),
                "column {column} hits byte {} outside the pill",
                offset.0
            );
        }
        let drawn: String = (0..width)
            .map(|column| {
                buffer
                    .cell((start.x + column, start.y))
                    .unwrap()
                    .symbol()
                    .to_owned()
            })
            .collect();
        assert!(drawn.contains('█'), "the knob is drawn: {drawn:?}");
        assert!(drawn.contains('─'), "the track is drawn: {drawn:?}");
        assert!(
            !drawn.contains("slider"),
            "nothing of the keyword shows through: {drawn:?}"
        );
        // The knob sits a quarter of the way along the track.
        let track: Vec<char> = drawn.chars().filter(|c| *c == '█' || *c == '─').collect();
        let knob = track.iter().position(|c| *c == '█').unwrap();
        assert!(
            (knob as f64) < (track.len() as f64) * 0.5,
            "a 0.25 knob sits left of centre: {knob} of {}",
            track.len()
        );
        assert_eq!(track.len(), 14, "the whole pill is track");
        let after: String = (0..10)
            .map(|column| {
                buffer
                    .cell((start.x + width + column, start.y))
                    .unwrap()
                    .symbol()
                    .to_owned()
            })
            .collect();
        assert_eq!(after, ".25, 0, 1)", "the numbers stay text after the pill");
    }

    /// The selection ground shows which slider has the arrows. Its accent
    /// and glyphs stay the same while the pointer is interacting with it.
    #[test]
    fn an_armed_slider_is_drawn_selected() {
        let theme = theme();
        let source = "$: s(\"bd\").gain(slider(.25, 0, 1))\n";
        let call = source.find("slider(").unwrap();
        let end = call + "slider(".len();
        let render = |armed: bool| {
            let chip = SliderChip {
                from: call,
                to: end,
                notch: 0.25,
                armed,
            };
            let mut editor = Editor::new(source).unwrap();
            editor.set_inline_widths(vec![crate::editor::InlineWidth {
                at: crate::editor::ByteOffset(end - 1),
                extra: 7,
            }]);
            let area = Rect::new(0, 0, 60, 3);
            let grid = source_grid(&editor, area, true);
            editor.set_view_size(usize::from(grid.width), usize::from(grid.height));
            let map = editor.screen_map(grid).unwrap();
            let mut buffer = Buffer::empty(area);
            let decorations = Decorations {
                sliders: std::slice::from_ref(&chip),
                ..Decorations::default()
            };
            let index = DecorationIndex::new(&map, &editor, decorations);
            let ScreenRow::Text(row) = &map.rows()[0] else {
                panic!("expected a text row");
            };
            render_text_row(
                &mut buffer,
                row,
                area.x,
                area.width,
                4,
                usize::MAX,
                &index,
                decorations.sliders,
                &theme,
                row_tokens(editor.document(), row),
            );
            let start = map
                .cell_for_offset(crate::editor::ByteOffset(call))
                .expect("cell for the call");
            (buffer, start.x)
        };
        let (plain, x) = render(false);
        let (armed, _) = render(true);
        let pill = x..x + 14;
        assert!(
            pill.clone()
                .all(|x| armed.cell((x, 0)).unwrap().bg == theme.selection),
            "the armed pill wears the selection's ground"
        );
        assert!(
            pill.clone()
                .any(|x| plain.cell((x, 0)).unwrap().bg != theme.selection),
            "the plain pill does not"
        );
        for x in pill.clone() {
            let before = plain.cell((x, 0)).unwrap();
            let after = armed.cell((x, 0)).unwrap();
            assert_eq!(after.symbol(), before.symbol(), "focus keeps the same rail");
            assert_eq!(before.fg, theme.accent);
            assert_eq!(after.fg, theme.accent);
        }
        assert_eq!(
            pill.clone()
                .filter(|&x| armed.cell((x, 0)).unwrap().symbol() == "█")
                .count(),
            1,
            "one knob either way"
        );
    }

    #[test]
    fn slider_cells_replace_source_styles_and_emit_one_continuous_rail() {
        use ratatui::backend::{Backend, CrosstermBackend};

        let source = "slider(.25, 0, 1)";
        let end = "slider(".len();
        let mut editor = Editor::new(source).unwrap();
        editor.set_inline_widths(vec![crate::editor::InlineWidth {
            at: crate::editor::ByteOffset(end - 1),
            extra: 13,
        }]);
        let area = Rect::new(0, 0, 40, 1);
        let grid = crate::editor::GridRect::new(0, 0, 40, 1);
        editor.set_view_size(40, 1);
        let map = editor.screen_map(grid).unwrap();
        let ScreenRow::Text(row) = &map.rows()[0] else {
            panic!("text row")
        };
        let (start, right) = slider_cover_bounds(row, 0, end).unwrap();
        assert_eq!(right - start, 20);
        let rail = format!("{}█{}", "─".repeat(5), "─".repeat(14));

        for armed in [false, true] {
            for filled in [false, true] {
                for current in [false, true] {
                    let mut theme = theme();
                    theme.slider_fill = filled.then_some(Color::Rgb(10, 20, 30));
                    theme.current_line = Some(Color::Rgb(30, 20, 10));
                    theme.event_mark = crate::theme::MarkStyle::Invert;
                    let chip = SliderChip {
                        from: 0,
                        to: end,
                        notch: 0.25,
                        armed,
                    };
                    let marks = [SourceMark {
                        from: 2,
                        to: 4,
                        color: Color::Red,
                        onset_id: 1,
                        strength: 1.0,
                    }];
                    let errors = [(0, 1), (end, end + 1)];
                    let overridden = [(4, 5)];
                    let brackets = [(end - 1, end, true)];
                    let decorations = Decorations {
                        sliders: std::slice::from_ref(&chip),
                        active: &marks,
                        errors: &errors,
                        overridden: &overridden,
                        brackets: &brackets,
                        ..Decorations::default()
                    };
                    let index = DecorationIndex::new(&map, &editor, decorations);
                    let mut buffer = Buffer::empty(area);
                    // Simulate differently styled text and background cells
                    // under a control, including the expanded '(' cells.
                    for x in start..right {
                        buffer.cell_mut((x, 0)).unwrap().set_style(
                            Style::default()
                                .fg(Color::Yellow)
                                .bg(if x % 2 == 0 { Color::Blue } else { Color::Red })
                                .underline_color(Color::Green)
                                .add_modifier(if x % 2 == 0 {
                                    Modifier::DIM | Modifier::REVERSED
                                } else {
                                    Modifier::ITALIC | Modifier::UNDERLINED
                                }),
                        );
                    }
                    render_text_row(
                        &mut buffer,
                        row,
                        0,
                        40,
                        0,
                        if current { 0 } else { usize::MAX },
                        &index,
                        decorations.sliders,
                        &theme,
                        row_tokens(editor.document(), row),
                    );
                    let background = if armed {
                        theme.selection
                    } else {
                        theme.slider_fill.unwrap_or(if current {
                            theme.current_line.unwrap()
                        } else {
                            theme.background
                        })
                    };
                    for x in start..right {
                        let cell = buffer.cell((x, 0)).unwrap();
                        assert_eq!(cell.fg, theme.accent, "column {x}");
                        assert_eq!(cell.bg, background, "column {x}");
                        assert_eq!(cell.modifier, Modifier::BOLD, "column {x}");
                        assert_eq!(cell.underline_color, Color::Reset, "column {x}");
                    }
                    let literal = buffer.cell((right, 0)).unwrap();
                    assert_eq!(literal.symbol(), ".");
                    assert!(literal.modifier.contains(Modifier::UNDERLINED));
                    assert_eq!(
                        literal.fg, theme.syntax.punctuation,
                        "the diagnostic retains the value's syntax colour"
                    );

                    // Exercise the same backend as Studio: no inherited
                    // per-cell SGR change can split the emitted rail.
                    let mut output = Vec::new();
                    {
                        let mut backend = CrosstermBackend::new(&mut output);
                        backend
                            .draw((start..right).map(|x| (x, 0, buffer.cell((x, 0)).unwrap())))
                            .unwrap();
                    }
                    let output = std::str::from_utf8(&output).unwrap();
                    assert!(output.contains(&rail), "{output:?}");
                    assert!(!output.contains('━'));
                }
            }
        }
    }

    #[test]
    fn an_armed_mono_slider_uses_the_themes_contrasting_selection_pair() {
        let theme = Theme::built_in("mono").expect("the bundled monochrome theme");
        assert_eq!(theme.accent, theme.selection);
        let area = Rect::new(0, 0, 20, 1);
        let mut plain = Buffer::empty(area);
        let mut armed = Buffer::empty(area);
        let mut chip = SliderChip {
            from: 0,
            to: 7,
            notch: 0.5,
            armed: false,
        };
        draw_slider_control(&mut plain, area, &chip, &theme, theme.background, false);
        chip.armed = true;
        draw_slider_control(&mut armed, area, &chip, &theme, theme.background, false);
        for x in 0..20 {
            let before = plain.cell((x, 0)).unwrap();
            let after = armed.cell((x, 0)).unwrap();
            assert_eq!(before.fg, theme.accent);
            assert_eq!(after.fg, theme.selection_text);
            assert_eq!(after.bg, theme.selection);
            assert_ne!(after.fg, after.bg, "the track and handle remain visible");
            assert_eq!(after.symbol(), before.symbol(), "focus keeps the same rail");
            assert_eq!(after.modifier, Modifier::BOLD);
        }
    }

    #[test]
    fn a_source_selection_selects_the_whole_slider_cover_without_selecting_it_from_the_value() {
        use crate::editor::{ByteOffset, GridRect, Selection};

        let source = "pre(slider(.25, 0, 1));";
        let call = source.find("slider(").unwrap();
        let end = call + "slider(".len();
        let area = Rect::new(0, 0, 60, 1);
        let chip = SliderChip {
            from: call,
            to: end,
            notch: 0.25,
            armed: false,
        };
        for theme in [theme(), Theme::built_in("mono").unwrap()] {
            // Full and partial cover selections, a selection starting at
            // the value, and an ordinary caret outside the slider call.
            for (anchor, selected_cover) in [
                (Some(call - 1), true),
                (Some(call + 3), true),
                (Some(end), false),
                (None, false),
            ] {
                let mut editor = Editor::new(source).unwrap();
                editor.set_inline_widths(vec![crate::editor::InlineWidth {
                    at: ByteOffset(end - 1),
                    extra: 13,
                }]);
                editor.set_view_size(60, 1);
                editor
                    .set_selection(match anchor {
                        Some(anchor) => {
                            Selection::range(ByteOffset(anchor), ByteOffset(source.len()))
                        }
                        None => Selection::caret(ByteOffset(source.len())),
                    })
                    .unwrap();
                let map = editor.screen_map(GridRect::new(0, 0, 60, 1)).unwrap();
                let ScreenRow::Text(row) = &map.rows()[0] else {
                    panic!("text row")
                };
                let (start, right) = slider_cover_bounds(row, call, end).unwrap();
                assert_eq!(right - start, 20);
                let errors = [(end, end + 3)];
                let decorations = Decorations {
                    sliders: std::slice::from_ref(&chip),
                    errors: &errors,
                    ..Decorations::default()
                };
                let index = DecorationIndex::new(&map, &editor, decorations);
                let mut buffer = Buffer::empty(area);
                render_text_row(
                    &mut buffer,
                    row,
                    0,
                    60,
                    0,
                    usize::MAX,
                    &index,
                    decorations.sliders,
                    &theme,
                    row_tokens(editor.document(), row),
                );
                for x in start..right {
                    let cell = buffer.cell((x, 0)).unwrap();
                    assert_eq!(
                        cell.bg,
                        if selected_cover {
                            theme.selection
                        } else {
                            theme.background
                        }
                    );
                    assert_eq!(
                        cell.fg,
                        if selected_cover && theme.accent == theme.selection {
                            theme.selection_text
                        } else {
                            theme.accent
                        }
                    );
                    assert_eq!(cell.modifier, Modifier::BOLD);
                }
                let literal = buffer.cell((right, 0)).unwrap();
                assert_eq!(literal.symbol(), ".");
                assert!(literal.modifier.contains(Modifier::UNDERLINED));
                assert_eq!(literal.underline_color, theme.error);
                assert_eq!(
                    literal.fg,
                    if anchor.is_some() {
                        theme.selection_text
                    } else {
                        theme.syntax.punctuation
                    }
                );
                if anchor.is_some() {
                    assert_eq!(literal.bg, theme.selection);
                }
                assert!(!chip.armed, "text selection does not arm the control");
            }
        }
    }

    #[test]
    fn a_theme_can_ask_for_a_filled_event_marking_instead() {
        let mut theme = theme();
        theme.event_mark = crate::theme::MarkStyle::Fill;
        let marks = [SourceMark {
            from: 0,
            to: 4,
            color: Color::Rgb(255, 202, 40),
            onset_id: 1,
            strength: 1.0,
        }];
        let cell = styled_cell(
            &theme,
            "$: s(\"bd\")\n",
            Decorations {
                active: &marks,
                ..Decorations::default()
            },
            0,
        );
        assert_eq!(cell.bg, Color::Rgb(255, 202, 40));
    }

    #[test]
    fn minimap_viewport_uses_wrapped_and_inline_screen_rows() {
        let mut editor = Editor::new(&"abcdefgh".repeat(20)).expect("editor");
        editor.set_view_size(9, 4);
        editor.set_wrap(true);
        editor.set_wrap_indents(vec![0]);
        let mut viewport = editor.viewport();
        viewport.top_row = 4;
        editor.set_viewport(viewport);
        let map = editor.screen_map(GridRect::new(0, 0, 9, 4)).expect("map");
        assert_eq!(visible_rows(&map), (4, 7));

        editor.set_wrap(false);
        editor
            .set_virtual_rows(
                editor.revision(),
                vec![VirtualRowSpec::new("plot", ByteOffset(0), 8)],
            )
            .expect("inline block");
        let mut viewport = editor.viewport();
        viewport.top_row = 4;
        editor.set_viewport(viewport);
        let map = editor.screen_map(GridRect::new(0, 0, 9, 4)).expect("map");
        assert!(
            map.rows()
                .iter()
                .all(|row| matches!(row, ScreenRow::Virtual(_)))
        );
        assert_eq!(visible_rows(&map), (4, 7));
    }
    #[test]
    fn pane_title_advertises_only_the_active_switch_binding() {
        use crate::keybinds::{BindAction, KeyCombo, Keybinds};
        let theme = Theme::built_in_default();
        let draw = |keybinds: &Keybinds| {
            let area = Rect::new(0, 0, 70, 1);
            let mut buffer = Buffer::empty(area);
            PaneTitle {
                keybinds,
                name: "verse",
                dirty: false,
                playing: false,
                focused: false,
                prebake: false,
                replay: false,
                theme: &theme,
            }
            .render(area, &mut buffer);
            buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        let mut bindings = Keybinds::default();
        bindings.learn(BindAction::HopPane, KeyCombo::parse("f2"));
        assert!(draw(&bindings).contains("F2 switches"));
        assert!(!draw(&bindings).contains("F10"));
        bindings.unbind(BindAction::HopPane);
        assert!(!draw(&bindings).contains("switches"));
    }
}
