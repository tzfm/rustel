use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::Arc;

use ropey::{RopeSlice, iter::Chunks};
use rustel_runtime::terminal_text::{control_picture, is_unsafe_terminal_character};
use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};
use unicode_width::UnicodeWidthStr;

use super::document::{ByteOffset, Document, DocumentError, EditShape, Revision};
use super::selection::map_offset_shapes;

pub const DEFAULT_TAB_WIDTH: u8 = 4;
pub const MAX_VIRTUAL_ROWS: usize = 64;
pub const MAX_VIRTUAL_ROW_HEIGHT: u16 = 32;
pub const MAX_TOTAL_VIRTUAL_HEIGHT: usize = 512;
const MAX_RENDERED_GRAPHEME_BYTES: usize = 256;
const HORIZONTAL_CHECKPOINT_BYTES: usize = 4 * 1024;
const MAX_CACHED_HORIZONTAL_LINES: usize = 128;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GridRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl GridRect {
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn contains(self, point: CellPoint) -> bool {
        point.x >= self.x
            && point.y >= self.y
            && point.x < self.x.saturating_add(self.width)
            && point.y < self.y.saturating_add(self.height)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CellPoint {
    pub x: u16,
    pub y: u16,
}

impl CellPoint {
    pub const fn new(x: u16, y: u16) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Affinity {
    Before,
    After,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VirtualRowSpec {
    pub id: Arc<str>,
    /// The virtual block appears immediately after the logical line containing
    /// this source offset.
    pub after: ByteOffset,
    pub height: u16,
}

impl VirtualRowSpec {
    pub fn new(id: impl Into<Arc<str>>, after: ByteOffset, height: u16) -> Self {
        Self {
            id: id.into(),
            after,
            height,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VirtualRowError {
    StaleRevision {
        expected: Revision,
        actual: Revision,
    },
    TooMany(usize),
    InvalidHeight {
        id: Arc<str>,
        height: u16,
    },
    TooTall(usize),
    InvalidAnchor(DocumentError),
}

impl fmt::Display for VirtualRowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleRevision { expected, actual } => write!(
                formatter,
                "virtual rows target revision {}; document is revision {}",
                expected.0, actual.0
            ),
            Self::TooMany(count) => {
                write!(
                    formatter,
                    "{count} virtual rows exceed maximum {MAX_VIRTUAL_ROWS}"
                )
            }
            Self::InvalidHeight { id, height } => write!(
                formatter,
                "virtual row {id:?} has invalid height {height}; maximum is {MAX_VIRTUAL_ROW_HEIGHT}"
            ),
            Self::TooTall(height) => write!(
                formatter,
                "virtual rows occupy {height} lines; maximum is {MAX_TOTAL_VIRTUAL_HEIGHT}"
            ),
            Self::InvalidAnchor(error) => write!(formatter, "invalid virtual-row anchor: {error}"),
        }
    }
}

impl std::error::Error for VirtualRowError {}

/// Extra columns a grapheme takes on its row: an inline control drawn over
/// the text that needs more room than the text has - the slider's pill -
/// the way the web editor's widget takes its own space. The cluster at `at`
/// is `extra` columns wider, and the rest of the row moves along.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InlineWidth {
    pub at: ByteOffset,
    pub extra: u16,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct VirtualRows {
    revision: Option<Revision>,
    rows: Vec<VirtualRowSpec>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct HorizontalCheckpoint {
    byte: usize,
    column: usize,
}

#[derive(Clone, Debug)]
struct LineLayoutIndex {
    content_start: usize,
    content_bytes: usize,
    tab_width: u8,
    /// The inline widths this index was built with, as the cache counts
    /// them, and the ones on this line by byte within it.
    widths_epoch: u64,
    widths: Vec<(usize, usize)>,
    checkpoints: Vec<HorizontalCheckpoint>,
    scanned: HorizontalCheckpoint,
    complete: bool,
}

impl LineLayoutIndex {
    fn new(
        content_start: usize,
        content_bytes: usize,
        tab_width: u8,
        widths_epoch: u64,
        widths: Vec<(usize, usize)>,
    ) -> Self {
        Self {
            content_start,
            content_bytes,
            tab_width,
            widths_epoch,
            widths,
            checkpoints: vec![HorizontalCheckpoint::default()],
            scanned: HorizontalCheckpoint::default(),
            complete: content_bytes == 0,
        }
    }

    fn matches(
        &self,
        content_start: usize,
        content_bytes: usize,
        tab_width: u8,
        widths_epoch: u64,
    ) -> bool {
        self.content_start == content_start
            && self.content_bytes == content_bytes
            && self.tab_width == tab_width
            && self.widths_epoch == widths_epoch
    }

    /// The extra columns of the cluster at `relative` bytes into the line.
    fn extra_at(&self, relative: usize) -> usize {
        self.widths
            .binary_search_by_key(&relative, |(at, _)| *at)
            .map(|index| self.widths[index].1)
            .unwrap_or(0)
    }

    fn checkpoint_at_or_before(&self, target_column: usize) -> HorizontalCheckpoint {
        let index = self
            .checkpoints
            .partition_point(|checkpoint| checkpoint.column <= target_column)
            .saturating_sub(1);
        self.checkpoints[index]
    }

    fn checkpoint_at_or_before_byte(&self, target_byte: usize) -> HorizontalCheckpoint {
        let index = self
            .checkpoints
            .partition_point(|checkpoint| checkpoint.byte <= target_byte)
            .saturating_sub(1);
        self.checkpoints[index]
    }

    fn retain_prefix(&mut self, content_start: usize, content_bytes: usize, byte: usize) {
        self.content_start = content_start;
        self.content_bytes = content_bytes;
        self.checkpoints
            .retain(|checkpoint| checkpoint.byte <= byte.min(content_bytes));
        if self.checkpoints.is_empty() {
            self.checkpoints.push(HorizontalCheckpoint::default());
        }
        self.scanned = *self
            .checkpoints
            .last()
            .expect("line layout index always has its origin");
        self.complete = self.scanned.byte == content_bytes;
    }
}

/// Sparse, revision-aware display-column checkpoints. A horizontally scrolled
/// score may be redrawn sixty times per second; only the first frame walks the
/// hidden prefix of a long logical line. Later frames seek to the nearest
/// grapheme boundary and traverse a small bounded tail.
#[derive(Clone, Debug, Default)]
pub(crate) struct HorizontalLayoutCache {
    revision: Option<Revision>,
    lines: HashMap<usize, LineLayoutIndex>,
    order: VecDeque<usize>,
    /// The inline widths in force, sorted by offset, and a count of the
    /// times they changed, so a line index built under other widths is
    /// known to be stale.
    widths: Vec<InlineWidth>,
    widths_epoch: u64,
    /// Full document extent for repeated paint and pointer events.
    maximum_width: Option<(Revision, u8, u64, usize)>,
    #[cfg(test)]
    traversed_graphemes: usize,
}

impl HorizontalLayoutCache {
    fn reset_for(&mut self, revision: Revision) {
        self.revision = Some(revision);
        self.lines.clear();
        self.order.clear();
        self.maximum_width = None;
    }

    pub(crate) fn maximum_line_width(&mut self, document: &Document, tab_width: u8) -> usize {
        let tab_width = tab_width.max(1);
        if let Some((revision, tab, epoch, width)) = self.maximum_width
            && revision == document.revision()
            && tab == tab_width
            && epoch == self.widths_epoch
        {
            return width;
        }
        let width = (0..document.line_count())
            .map(|line| {
                display_column_of_offset_cached(
                    document,
                    line,
                    document.line_content_range(line).end,
                    tab_width,
                    self,
                )
            })
            .max()
            .unwrap_or(0);
        self.maximum_width = Some((document.revision(), tab_width, self.widths_epoch, width));
        width
    }

    /// The inline widths the layout lays rows out with. A change throws
    /// the line indices away: their columns were counted without it.
    pub(crate) fn set_inline_widths(&mut self, mut widths: Vec<InlineWidth>) {
        widths.sort_by_key(|width| width.at);
        widths.dedup_by_key(|width| width.at);
        if widths == self.widths {
            return;
        }
        self.widths = widths;
        self.widths_epoch = self.widths_epoch.wrapping_add(1);
        self.lines.clear();
        self.order.clear();
    }

    /// A count of the times the inline widths changed; an index built
    /// under other widths is stale.
    pub(crate) fn widths_epoch(&self) -> u64 {
        self.widths_epoch
    }

    /// The extra columns of the cluster starting at `byte`.
    pub(crate) fn extra_at(&self, byte: usize) -> usize {
        self.widths
            .binary_search_by_key(&byte, |width| width.at.0)
            .map(|index| usize::from(self.widths[index].extra))
            .unwrap_or(0)
    }

    /// The inline widths within one line, by byte within it.
    fn line_widths(&self, content_start: usize, content_bytes: usize) -> Vec<(usize, usize)> {
        self.widths
            .iter()
            .filter(|width| {
                width.at.0 >= content_start && width.at.0 < content_start + content_bytes
            })
            .map(|width| (width.at.0 - content_start, usize::from(width.extra)))
            .collect()
    }

    pub(crate) fn invalidate(&mut self, revision: Revision) {
        self.reset_for(revision);
    }

    /// Preserve checkpoints strictly before the first edit. This matters when
    /// typing at the far right of a very long line: the next frame resumes near
    /// the edit instead of rescanning megabytes that did not change.
    pub(crate) fn map_revision_after_edit(
        &mut self,
        before: &Document,
        after: &Document,
        edits: &[EditShape],
    ) {
        if self.revision != Some(before.revision()) {
            self.reset_for(after.revision());
            return;
        }
        let Some(first) = edits.iter().map(|edit| edit.range.start).min() else {
            self.revision = Some(after.revision());
            return;
        };
        let Ok(line) = before.line_of(first) else {
            self.reset_for(after.revision());
            return;
        };
        let old_start = before.line_start(line).0;
        let prefix_bytes = first.0.saturating_sub(old_start);

        self.lines.retain(|candidate, _| *candidate <= line);
        self.order.retain(|candidate| *candidate <= line);
        if let Some(index) = self.lines.get_mut(&line) {
            let content = after.line_content_range(line);
            index.retain_prefix(
                content.start.0,
                content.end.0.saturating_sub(content.start.0),
                prefix_bytes,
            );
        }
        self.revision = Some(after.revision());
    }

    fn checkpoint_for(
        &mut self,
        document: &Document,
        line: usize,
        target_column: usize,
        tab_width: u8,
    ) -> HorizontalCheckpoint {
        if self.revision != Some(document.revision()) {
            self.reset_for(document.revision());
        }
        let content = document.line_content_range(line);
        let content_bytes = content.end.0.saturating_sub(content.start.0);
        let tab_width = tab_width.max(1);
        let needs_index = self.lines.get(&line).is_none_or(|index| {
            !index.matches(content.start.0, content_bytes, tab_width, self.widths_epoch)
        });
        if needs_index {
            if self.lines.len() >= MAX_CACHED_HORIZONTAL_LINES
                && let Some(oldest) = self.order.pop_front()
            {
                self.lines.remove(&oldest);
            }
            let widths = self.line_widths(content.start.0, content_bytes);
            self.lines.insert(
                line,
                LineLayoutIndex::new(
                    content.start.0,
                    content_bytes,
                    tab_width,
                    self.widths_epoch,
                    widths,
                ),
            );
        }
        self.order.retain(|candidate| *candidate != line);
        self.order.push_back(line);

        let text = document.rope().byte_slice(content.start.0..content.end.0);
        let (checkpoint, traversed) = {
            let index = self
                .lines
                .get_mut(&line)
                .expect("line layout index was inserted above");
            let traversed = extend_line_index(index, text, IndexTarget::Column(target_column));
            let checkpoint = if index.scanned.column == target_column {
                index.scanned
            } else {
                index.checkpoint_at_or_before(target_column)
            };
            (checkpoint, traversed)
        };
        self.note_traversed(traversed);
        checkpoint
    }

    fn checkpoint_for_byte(
        &mut self,
        document: &Document,
        line: usize,
        target_byte: usize,
        tab_width: u8,
    ) -> HorizontalCheckpoint {
        if self.revision != Some(document.revision()) {
            self.reset_for(document.revision());
        }
        let content = document.line_content_range(line);
        let content_bytes = content.end.0.saturating_sub(content.start.0);
        let target_byte = target_byte.min(content_bytes);
        let tab_width = tab_width.max(1);
        let needs_index = self.lines.get(&line).is_none_or(|index| {
            !index.matches(content.start.0, content_bytes, tab_width, self.widths_epoch)
        });
        if needs_index {
            if self.lines.len() >= MAX_CACHED_HORIZONTAL_LINES
                && let Some(oldest) = self.order.pop_front()
            {
                self.lines.remove(&oldest);
            }
            let widths = self.line_widths(content.start.0, content_bytes);
            self.lines.insert(
                line,
                LineLayoutIndex::new(
                    content.start.0,
                    content_bytes,
                    tab_width,
                    self.widths_epoch,
                    widths,
                ),
            );
        }
        self.order.retain(|candidate| *candidate != line);
        self.order.push_back(line);

        let text = document.rope().byte_slice(content.start.0..content.end.0);
        let (checkpoint, traversed) = {
            let index = self
                .lines
                .get_mut(&line)
                .expect("line layout index was inserted above");
            let traversed = extend_line_index(index, text, IndexTarget::Byte(target_byte));
            let checkpoint = if index.scanned.byte == target_byte {
                index.scanned
            } else {
                index.checkpoint_at_or_before_byte(target_byte)
            };
            (checkpoint, traversed)
        };
        self.note_traversed(traversed);
        checkpoint
    }

    fn note_traversed(&mut self, traversed: usize) {
        #[cfg(test)]
        {
            self.traversed_graphemes = self.traversed_graphemes.saturating_add(traversed);
        }
        #[cfg(not(test))]
        let _ = traversed;
    }

    #[cfg(test)]
    fn take_traversed_graphemes(&mut self) -> usize {
        std::mem::take(&mut self.traversed_graphemes)
    }
}

#[derive(Clone, Copy)]
enum IndexTarget {
    Byte(usize),
    Column(usize),
}

impl IndexTarget {
    fn reached(self, checkpoint: HorizontalCheckpoint) -> bool {
        match self {
            Self::Byte(byte) => checkpoint.byte >= byte,
            Self::Column(column) => checkpoint.column >= column,
        }
    }
}

fn extend_line_index(
    index: &mut LineLayoutIndex,
    text: RopeSlice<'_>,
    target: IndexTarget,
) -> usize {
    if index.complete || target.reached(index.scanned) {
        return 0;
    }
    let mut traversed = 0usize;
    for (relative, cluster) in RopeGraphemes::new_at(text, index.scanned.byte) {
        let cluster_bytes = cluster.len_bytes();
        let width = rope_cluster_width(cluster, index.scanned.column, index.tab_width)
            .saturating_add(index.extra_at(relative));
        index.scanned = HorizontalCheckpoint {
            byte: relative.saturating_add(cluster_bytes),
            column: index.scanned.column.saturating_add(width),
        };
        traversed = traversed.saturating_add(1);
        if index.scanned.byte.saturating_sub(
            index
                .checkpoints
                .last()
                .expect("line layout index always has its origin")
                .byte,
        ) >= HORIZONTAL_CHECKPOINT_BYTES
        {
            index.checkpoints.push(index.scanned);
        }
        if target.reached(index.scanned) {
            break;
        }
    }
    if index.scanned.byte >= index.content_bytes {
        index.complete = true;
        if index.checkpoints.last().copied() != Some(index.scanned) {
            index.checkpoints.push(index.scanned);
        }
    }
    traversed
}

impl VirtualRows {
    pub(crate) fn rows(&self) -> &[VirtualRowSpec] {
        &self.rows
    }

    pub(crate) fn clear(&mut self) {
        self.revision = None;
        self.rows.clear();
    }

    pub(crate) fn map_through_steps(&mut self, revision: Revision, steps: &[Vec<EditShape>]) {
        if self.rows.is_empty() {
            self.revision = None;
            return;
        }
        for edits in steps {
            for row in &mut self.rows {
                // A widget at the exact end of a call stays before text typed
                // there (especially a newly inserted newline).
                row.after = map_offset_shapes(row.after, edits, false);
            }
        }
        self.revision = Some(revision);
    }

    pub(crate) fn replace(
        &mut self,
        document: &Document,
        revision: Revision,
        mut rows: Vec<VirtualRowSpec>,
    ) -> Result<(), VirtualRowError> {
        if revision != document.revision() {
            return Err(VirtualRowError::StaleRevision {
                expected: revision,
                actual: document.revision(),
            });
        }
        if rows.len() > MAX_VIRTUAL_ROWS {
            return Err(VirtualRowError::TooMany(rows.len()));
        }
        let mut total = 0usize;
        for row in &rows {
            if row.height == 0 || row.height > MAX_VIRTUAL_ROW_HEIGHT {
                return Err(VirtualRowError::InvalidHeight {
                    id: row.id.clone(),
                    height: row.height,
                });
            }
            document
                .validate_offset(row.after)
                .map_err(VirtualRowError::InvalidAnchor)?;
            total = total.saturating_add(usize::from(row.height));
        }
        if total > MAX_TOTAL_VIRTUAL_HEIGHT {
            return Err(VirtualRowError::TooTall(total));
        }
        rows.sort_by_key(|row| row.after);
        self.revision = Some(revision);
        self.rows = rows;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CellSpan {
    pub bytes: std::ops::Range<ByteOffset>,
    /// Display columns in the unscrolled logical line.
    pub columns: std::ops::Range<usize>,
    /// Absolute terminal cells occupied in this frame.
    pub screen_x: std::ops::Range<u16>,
    /// Sanitized text. Source control bytes are never emitted verbatim.
    pub display: String,
    /// An inline control's room: the cluster took extra columns, and a hit
    /// anywhere on them is the cluster's, not its neighbour's - the whole
    /// of a slider's pill is the slider.
    pub widened: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextRow {
    pub screen_y: u16,
    pub global_row: usize,
    pub line: usize,
    /// The display column at the row's left edge: the scroll, or where
    /// this row of a wrapped line starts.
    pub first_column: usize,
    /// Which row of its line this is; nought unless the line wraps.
    pub segment: usize,
    /// Cells the row is set in from the grid's left edge: the hanging
    /// indent of a wrapped line's later rows, nought otherwise.
    pub hanging: u16,
    pub content: std::ops::Range<ByteOffset>,
    /// Last grapheme boundary traversed while building this horizontal
    /// window. This avoids treating a click after the visible cells as EOF on
    /// a very long clipped line.
    pub visible_end: ByteOffset,
    pub display_columns: usize,
    pub cells: Vec<CellSpan>,
}

impl TextRow {
    pub fn hit_test(&self, target_column: usize) -> (ByteOffset, Affinity) {
        for cell in &self.cells {
            if target_column < cell.columns.start {
                return (cell.bytes.start, Affinity::Before);
            }
            if target_column < cell.columns.end {
                if cell.widened {
                    return (cell.bytes.start, Affinity::Before);
                }
                let width = cell.columns.end.saturating_sub(cell.columns.start).max(1);
                let inside = target_column.saturating_sub(cell.columns.start);
                if inside.saturating_mul(2) < width {
                    return (cell.bytes.start, Affinity::Before);
                }
                return (cell.bytes.end, Affinity::After);
            }
        }
        (self.visible_end, Affinity::After)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InlineVirtualRow {
    pub screen_y: u16,
    pub global_row: usize,
    pub id: Arc<str>,
    pub anchor: ByteOffset,
    pub inner_row: u16,
    pub height: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScreenRow {
    Text(TextRow),
    Virtual(InlineVirtualRow),
}

impl ScreenRow {
    pub fn screen_y(&self) -> u16 {
        match self {
            Self::Text(row) => row.screen_y,
            Self::Virtual(row) => row.screen_y,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Hit {
    Text {
        offset: ByteOffset,
        affinity: Affinity,
        line: usize,
    },
    Virtual {
        id: Arc<str>,
        anchor: ByteOffset,
        inner_row: u16,
    },
}

impl Hit {
    pub fn selection_offset(&self) -> ByteOffset {
        match self {
            Self::Text { offset, .. } => *offset,
            Self::Virtual { anchor, .. } => *anchor,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Viewport {
    pub top_row: usize,
    pub left_column: usize,
    pub tab_width: u8,
    /// Width of the text viewport in terminal cells.  This is used to keep
    /// keyboard-driven caret motion horizontally visible.
    pub page_columns: usize,
    pub page_rows: usize,
    /// Long lines continue on the next row instead of running off the
    /// edge; `left_column` stays at nought.
    pub wrap: bool,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            top_row: 0,
            left_column: 0,
            tab_width: DEFAULT_TAB_WIDTH,
            page_columns: 80,
            page_rows: 20,
            wrap: false,
        }
    }
}

impl Viewport {
    /// The columns a row holds when the text wraps: one fewer than the
    /// page, so the caret at the end of a full row still has a cell.
    pub fn wrap_width(&self) -> Option<usize> {
        self.wrap
            .then(|| self.page_columns.saturating_sub(1).max(1))
    }
}

/// A line longer than this is not wrapped: it takes one row, clipped,
/// so the index never walks a multi-megabyte line.
pub const WRAP_MAX_LINE_BYTES: usize = 16 * 1024;

/// How far in a line's later rows are set: its own indentation and the
/// hanging indent, never more than half the row.
fn hanging_indent(indent: usize, width: usize) -> u16 {
    let hanging = indent.saturating_add(usize::from(HANGING_INDENT));
    u16::try_from(hanging.min(width / 2)).unwrap_or(u16::MAX)
}

/// Where a row of a wrapped line starts: the display column and the byte
/// within the line. The first row of every line starts at nought.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RowStart {
    pub column: usize,
    pub byte: usize,
}

/// Cells a wrapped line's later rows are set in from its own indentation,
/// the way a chain is written by hand: the row under `$: s("bd")` starts
/// under the `s`, four cells in.
const HANGING_INDENT: u16 = 4;

/// Where every line breaks when the text wraps at a width: the rows each
/// line takes, where each row after the first starts, and how far in it
/// is set. A break knows the code: before a `.method(` first, after a
/// comma next, after a space outside a string after that, and inside a
/// string only when the string alone outgrows the row; a word longer
/// than a row breaks inside. Built for one revision, width, tab width,
/// set of inline widths and set of continuation indents; the editor keeps
/// one and rebuilds it when any of them change. The indents are not part
/// of [`WrapIndex::is_current_for`]: setting new ones discards the index.
#[derive(Clone, Debug, Default)]
pub struct WrapIndex {
    revision: Option<Revision>,
    width: usize,
    tab_width: u8,
    widths_epoch: u64,
    /// Rows taken by every line before this one, one entry past the last
    /// line for the total.
    first_row: Vec<usize>,
    /// Every row's start, by `first_row[line] + segment`.
    starts: Vec<RowStart>,
    /// Cells each line's later rows are set in.
    hanging: Vec<u16>,
}

/// A place a row may end: how good a break it is, and where the next row
/// starts if it does.
#[derive(Clone, Copy)]
struct Break {
    start: RowStart,
    /// Before a method's dot, after a comma, after a space, after a space
    /// inside a string: best to worst.
    priority: u8,
}

impl WrapIndex {
    /// `indents` sets the column a line's later rows start at, by line -
    /// a log's message column - in place of the code's hanging indent;
    /// lines past its end keep the hanging indent.
    pub(crate) fn build(
        document: &Document,
        width: usize,
        tab_width: u8,
        cache: &HorizontalLayoutCache,
        indents: &[usize],
    ) -> Self {
        let width = width.max(1);
        let tab_width = tab_width.max(1);
        let line_count = document.line_count();
        let mut first_row = Vec::with_capacity(line_count + 1);
        let mut starts = Vec::with_capacity(line_count);
        let mut hanging = Vec::with_capacity(line_count);
        for line in 0..line_count {
            let continuation_indent = |indent| {
                indents.get(line).map_or_else(
                    || hanging_indent(indent, width),
                    // Keep space for text even in a very narrow dock.
                    |column| {
                        u16::try_from((*column).min(width.saturating_sub(8))).unwrap_or(u16::MAX)
                    },
                )
            };
            first_row.push(starts.len());
            starts.push(RowStart::default());
            let content = document.line_content_range(line);
            let bytes = content.end.0.saturating_sub(content.start.0);
            if bytes > WRAP_MAX_LINE_BYTES {
                hanging.push(0);
                continue;
            }
            let text = document.rope().byte_slice(content.start.0..content.end.0);
            let mut column = 0usize;
            let mut row_start = 0usize;
            let mut row_width = width;
            let mut breaks: Vec<Break> = Vec::new();
            // The dot just read, waiting to see whether a name follows it.
            let mut pending_dot: Option<RowStart> = None;
            let mut quote: Option<char> = None;
            let mut escaped = false;
            let mut indent: Option<usize> = None;
            for (relative, cluster) in RopeGraphemes::new_at(text, 0) {
                let cluster_bytes = cluster.len_bytes();
                let inline_extra = cache.extra_at(content.start.0 + relative);
                let cluster_width =
                    rope_cluster_width(cluster, column, tab_width).saturating_add(inline_extra);
                let blank = cluster.chars().all(char::is_whitespace);
                let first = cluster.chars().next().unwrap_or(' ');
                if indent.is_none() && !blank {
                    indent = Some(column);
                }
                // The dot before a name is the best place to break: the chain
                // continues on the next row, its dot first.
                if let Some(dot) = pending_dot.take()
                    && quote.is_none()
                    && (first.is_alphabetic() || first == '_' || first == '$')
                {
                    breaks.push(Break {
                        start: dot,
                        priority: 3,
                    });
                }
                // A space at the edge hangs past it rather than starting a
                // row of its own; what follows starts the next row.
                if !blank
                    && column > row_start
                    && column.saturating_add(cluster_width) > row_start + row_width
                {
                    let quarter = row_start + row_width / 4;
                    let chosen = breaks
                        .iter()
                        .filter(|candidate| candidate.start.column > quarter)
                        .max_by_key(|candidate| (candidate.priority, candidate.start.column))
                        .or_else(|| {
                            breaks
                                .iter()
                                .filter(|candidate| candidate.start.column > row_start)
                                .max_by_key(|candidate| {
                                    (candidate.priority, candidate.start.column)
                                })
                        })
                        .map(|candidate| candidate.start)
                        .unwrap_or(RowStart {
                            column,
                            byte: relative,
                        });
                    starts.push(chosen);
                    row_start = chosen.column;
                    let hanging = continuation_indent(indent.unwrap_or(0));
                    row_width = width.saturating_sub(usize::from(hanging)).max(1);
                    breaks.retain(|candidate| candidate.start.column > row_start);
                }
                column = column.saturating_add(cluster_width);
                let after = RowStart {
                    column,
                    byte: relative + cluster_bytes,
                };
                if inline_extra > 0 {
                    // Once a control fits, following text may wrap after
                    // it but must not pull it onto a different row. In
                    // particular, a slider's changing numeric literal
                    // cannot move the rail during a pointer gesture.
                    breaks.clear();
                    breaks.push(Break {
                        start: after,
                        priority: 0,
                    });
                } else if blank {
                    breaks.push(Break {
                        start: after,
                        priority: if quote.is_some() { 0 } else { 1 },
                    });
                } else if quote.is_none() && first == ',' {
                    breaks.push(Break {
                        start: after,
                        priority: 2,
                    });
                } else if quote.is_none() && first == '.' {
                    pending_dot = Some(RowStart {
                        column: column - cluster_width,
                        byte: relative,
                    });
                }
                for ch in cluster.chars() {
                    match quote {
                        Some(open) => {
                            if escaped {
                                escaped = false;
                            } else if ch == '\\' {
                                escaped = true;
                            } else if ch == open {
                                quote = None;
                            }
                        }
                        None => {
                            if matches!(ch, '"' | '\'' | '`') {
                                quote = Some(ch);
                            }
                        }
                    }
                }
            }
            hanging.push(continuation_indent(indent.unwrap_or(0)));
        }
        first_row.push(starts.len());
        Self {
            revision: Some(document.revision()),
            width,
            tab_width,
            widths_epoch: cache.widths_epoch(),
            first_row,
            starts,
            hanging,
        }
    }

    /// Cells the rows of `line` after its first are set in.
    pub fn hanging_of(&self, line: usize) -> u16 {
        self.hanging.get(line).copied().unwrap_or(0)
    }

    /// Cells row `segment` of `line` is set in: nought for the first row.
    pub fn hanging_at(&self, line: usize, segment: usize) -> u16 {
        if segment == 0 {
            0
        } else {
            self.hanging_of(line)
        }
    }

    pub(crate) fn is_current_for(
        &self,
        document: &Document,
        width: usize,
        tab_width: u8,
        cache: &HorizontalLayoutCache,
    ) -> bool {
        self.revision == Some(document.revision())
            && self.width == width.max(1)
            && self.tab_width == tab_width.max(1)
            && self.widths_epoch == cache.widths_epoch()
    }

    /// Rows taken by the lines before `line`.
    pub fn rows_before(&self, line: usize) -> usize {
        self.first_row
            .get(line)
            .copied()
            .unwrap_or_else(|| self.total_rows())
    }

    pub fn total_rows(&self) -> usize {
        self.first_row.last().copied().unwrap_or(0)
    }

    pub fn rows_of(&self, line: usize) -> usize {
        self.rows_before(line + 1)
            .saturating_sub(self.rows_before(line))
            .max(1)
    }

    /// Where row `segment` of `line` starts.
    pub fn start_of(&self, line: usize, segment: usize) -> RowStart {
        self.starts
            .get(self.rows_before(line).saturating_add(segment))
            .copied()
            .unwrap_or_default()
    }

    /// The column the row after `segment` starts at, if there is one.
    pub fn end_of(&self, line: usize, segment: usize) -> Option<usize> {
        (segment + 1 < self.rows_of(line)).then(|| self.start_of(line, segment + 1).column)
    }

    /// The line and row a text row index lands in.
    pub fn locate(&self, row: usize) -> (usize, usize) {
        let line = self
            .first_row
            .partition_point(|first| *first <= row)
            .saturating_sub(1);
        (line, row.saturating_sub(self.rows_before(line)))
    }

    /// The row of `line` the caret at display column `column` is drawn
    /// on, as [`ScreenMap::cell_for_offset`] draws it: the column a row
    /// starts at stays on the row before while it fits in the cell
    /// [`Viewport::wrap_width`] leaves past that row's end.
    pub fn segment_of(&self, line: usize, column: usize) -> usize {
        (1..self.rows_of(line))
            .take_while(|segment| {
                let start = self.start_of(line, *segment).column;
                start < column
                    || (start == column && {
                        let previous = self.start_of(line, segment - 1).column;
                        let hanging = usize::from(self.hanging_at(line, segment - 1));
                        column - previous + hanging > self.width
                    })
            })
            .count()
    }
}

#[derive(Clone, Debug)]
pub struct ScreenMap {
    revision: Revision,
    viewport: Viewport,
    area: GridRect,
    total_rows: usize,
    rows: Vec<ScreenRow>,
}

impl ScreenMap {
    pub fn build(
        document: &Document,
        viewport: Viewport,
        virtual_rows: &[VirtualRowSpec],
        area: GridRect,
    ) -> Result<Self, DocumentError> {
        let mut cache = HorizontalLayoutCache::default();
        Self::build_wrapped(document, viewport, virtual_rows, area, &mut cache, None)
    }

    /// The map with the lines wrapped by `wrap`, when there is one; see
    /// [`WrapIndex`].
    pub(crate) fn build_wrapped(
        document: &Document,
        viewport: Viewport,
        virtual_rows: &[VirtualRowSpec],
        area: GridRect,
        horizontal_cache: &mut HorizontalLayoutCache,
        wrap: Option<&WrapIndex>,
    ) -> Result<Self, DocumentError> {
        let resolved = resolve_virtual_rows(document, virtual_rows)?;
        let text_rows = TextRows {
            line_count: document.line_count(),
            wrap,
        };
        let extra_rows = resolved
            .iter()
            .map(|row| usize::from(row.spec.height))
            .sum::<usize>();
        let total_rows = text_rows.total().saturating_add(extra_rows);
        let mut rows = Vec::with_capacity(usize::from(area.height));
        for visible in 0..usize::from(area.height) {
            let global = viewport.top_row.saturating_add(visible);
            if global >= total_rows {
                break;
            }
            let y = area.y.saturating_add(visible as u16);
            match row_at(global, &text_rows, &resolved) {
                ResolvedScreenRow::Text { line, segment } => {
                    let piece = wrap.map(|wrap| WrapPiece {
                        segment,
                        start: wrap.start_of(line, segment),
                        end_column: wrap.end_of(line, segment),
                        hanging: wrap.hanging_at(line, segment),
                    });
                    rows.push(ScreenRow::Text(build_text_row(
                        document,
                        line,
                        global,
                        y,
                        area,
                        viewport,
                        piece,
                        horizontal_cache,
                    )));
                }
                ResolvedScreenRow::Virtual { row, inner_row } => {
                    rows.push(ScreenRow::Virtual(InlineVirtualRow {
                        screen_y: y,
                        global_row: global,
                        id: row.spec.id.clone(),
                        anchor: row.anchor,
                        inner_row,
                        height: row.spec.height,
                    }));
                }
            }
        }
        Ok(Self {
            revision: document.revision(),
            viewport,
            area,
            total_rows,
            rows,
        })
    }

    pub fn revision(&self) -> Revision {
        self.revision
    }

    pub fn viewport(&self) -> Viewport {
        self.viewport
    }

    pub fn area(&self) -> GridRect {
        self.area
    }

    pub fn total_rows(&self) -> usize {
        self.total_rows
    }

    pub fn rows(&self) -> &[ScreenRow] {
        &self.rows
    }

    pub fn is_current_for(&self, document: &Document, viewport: Viewport) -> bool {
        self.revision == document.revision() && self.viewport == viewport
    }

    pub fn hit_test(&self, point: CellPoint) -> Option<Hit> {
        if !self.area.contains(point) {
            return None;
        }
        self.hit_test_inside(point)
    }

    /// A hit for any point inside the grid, the way an editor treats its own
    /// empty space: a press below the last line lands on the last line, so a
    /// selection can start from the blank half of the pane. Outside the grid
    /// it still refuses - a click on the chrome must not teleport the caret.
    pub fn hit_test_within(&self, point: CellPoint) -> Option<Hit> {
        if !self.area.contains(point) {
            return None;
        }
        self.hit_test_inside(point)
            .or_else(|| self.hit_test_clamped(point))
    }

    pub fn hit_test_clamped(&self, point: CellPoint) -> Option<Hit> {
        if self.rows.is_empty() || self.area.width == 0 {
            return None;
        }
        let maximum_x = self
            .area
            .x
            .saturating_add(self.area.width.saturating_sub(1));
        let minimum_y = self.rows.first()?.screen_y();
        let maximum_y = self.rows.last()?.screen_y();
        // Past the text on the y axis the hit takes the row's end (below)
        // or its start (above), not the pointer's column: the empty space
        // under a short score is "after the text", so a drag begun out
        // there and carried into the lines selects as if it had started at
        // the text's edge - the way an editor's blank half behaves.
        let x = if point.y > maximum_y {
            maximum_x
        } else if point.y < minimum_y {
            self.area.x
        } else {
            point.x
        };
        self.hit_test_inside(CellPoint {
            x: x.clamp(self.area.x, maximum_x),
            y: point.y.clamp(minimum_y, maximum_y),
        })
    }

    fn hit_test_inside(&self, point: CellPoint) -> Option<Hit> {
        let row = self.rows.iter().find(|row| row.screen_y() == point.y)?;
        match row {
            ScreenRow::Text(row) => {
                let local = usize::from(
                    point
                        .x
                        .saturating_sub(self.area.x.saturating_add(row.hanging)),
                );
                let column = row.first_column.saturating_add(local);
                let (offset, affinity) = row.hit_test(column);
                Some(Hit::Text {
                    offset,
                    affinity,
                    line: row.line,
                })
            }
            ScreenRow::Virtual(row) => Some(Hit::Virtual {
                id: row.id.clone(),
                anchor: row.anchor,
                inner_row: row.inner_row,
            }),
        }
    }

    pub fn cell_for_offset(&self, offset: ByteOffset) -> Option<CellPoint> {
        for row in &self.rows {
            let ScreenRow::Text(row) = row else {
                continue;
            };
            if offset < row.content.start || offset > row.content.end {
                continue;
            }
            // A wrapped line has several rows; the end of the line is on
            // the last, and any other offset on the row whose cells hold it.
            let column = if offset == row.content.end {
                if row.visible_end != row.content.end {
                    continue;
                }
                row.display_columns
            } else {
                let Some(column) = row.cells.iter().find_map(|cell| {
                    (offset == cell.bytes.start)
                        .then_some(cell.columns.start)
                        .or_else(|| (offset == cell.bytes.end).then_some(cell.columns.end))
                }) else {
                    continue;
                };
                column
            };
            if column < row.first_column {
                continue;
            }
            let local = column - row.first_column;
            if local.saturating_add(usize::from(row.hanging)) >= usize::from(self.area.width) {
                continue;
            }
            return Some(CellPoint::new(
                self.area
                    .x
                    .saturating_add(row.hanging)
                    .saturating_add(local as u16),
                row.screen_y,
            ));
        }
        None
    }
}

#[derive(Clone)]
struct ResolvedVirtualRow<'a> {
    spec: &'a VirtualRowSpec,
    line: usize,
    anchor: ByteOffset,
}

fn resolve_virtual_rows<'a>(
    document: &Document,
    rows: &'a [VirtualRowSpec],
) -> Result<Vec<ResolvedVirtualRow<'a>>, DocumentError> {
    let mut resolved = rows
        .iter()
        .map(|spec| {
            document.validate_offset(spec.after)?;
            let line = document.line_of(spec.after)?;
            Ok(ResolvedVirtualRow {
                spec,
                line,
                anchor: document.line_content_range(line).end,
            })
        })
        .collect::<Result<Vec<_>, DocumentError>>()?;
    resolved.sort_by_key(|row| (row.line, row.spec.after));
    Ok(resolved)
}

enum ResolvedScreenRow<'a> {
    Text {
        line: usize,
        segment: usize,
    },
    Virtual {
        row: &'a ResolvedVirtualRow<'a>,
        inner_row: u16,
    },
}

/// The text rows of the document: one a line, or what the wrap makes.
#[derive(Clone, Copy)]
struct TextRows<'a> {
    line_count: usize,
    wrap: Option<&'a WrapIndex>,
}

impl TextRows<'_> {
    fn total(&self) -> usize {
        self.wrap.map_or(self.line_count, WrapIndex::total_rows)
    }

    /// Rows taken by the lines before `line`.
    fn before(&self, line: usize) -> usize {
        self.wrap.map_or(line, |wrap| wrap.rows_before(line))
    }

    /// The line and row within it a text row index lands in.
    fn locate(&self, row: usize) -> (usize, usize) {
        match self.wrap {
            Some(wrap) => wrap.locate(row),
            None => (row, 0),
        }
    }
}

fn row_at<'a>(
    target: usize,
    text_rows: &TextRows<'_>,
    virtual_rows: &'a [ResolvedVirtualRow<'a>],
) -> ResolvedScreenRow<'a> {
    let mut screen_cursor = 0usize;
    let mut line_cursor = 0usize;
    let mut virtual_cursor = 0usize;
    while virtual_cursor < virtual_rows.len() {
        let anchor_line = virtual_rows[virtual_cursor].line;
        let text_count = text_rows
            .before(anchor_line.saturating_add(1))
            .saturating_sub(text_rows.before(line_cursor));
        if target < screen_cursor.saturating_add(text_count) {
            let (line, segment) =
                text_rows.locate(text_rows.before(line_cursor) + (target - screen_cursor));
            return ResolvedScreenRow::Text { line, segment };
        }
        screen_cursor = screen_cursor.saturating_add(text_count);
        line_cursor = anchor_line.saturating_add(1);
        while virtual_cursor < virtual_rows.len()
            && virtual_rows[virtual_cursor].line == anchor_line
        {
            let row = &virtual_rows[virtual_cursor];
            let height = usize::from(row.spec.height);
            if target < screen_cursor.saturating_add(height) {
                return ResolvedScreenRow::Virtual {
                    row,
                    inner_row: (target - screen_cursor) as u16,
                };
            }
            screen_cursor = screen_cursor.saturating_add(height);
            virtual_cursor += 1;
        }
    }
    let row = text_rows
        .before(line_cursor)
        .saturating_add(target.saturating_sub(screen_cursor))
        .min(text_rows.total().saturating_sub(1));
    let (line, segment) = text_rows.locate(row);
    ResolvedScreenRow::Text {
        line: line.min(text_rows.line_count.saturating_sub(1)),
        segment,
    }
}

/// One row of a wrapped line: which, where it starts, and the column the
/// next row starts at.
#[derive(Clone, Copy)]
struct WrapPiece {
    segment: usize,
    start: RowStart,
    end_column: Option<usize>,
    hanging: u16,
}

#[allow(clippy::too_many_arguments)]
fn build_text_row(
    document: &Document,
    line: usize,
    global_row: usize,
    y: u16,
    area: GridRect,
    viewport: Viewport,
    piece: Option<WrapPiece>,
    horizontal_cache: &mut HorizontalLayoutCache,
) -> TextRow {
    let content = document.line_content_range(line);
    let text = document.rope().byte_slice(content.start.0..content.end.0);
    // A row of a wrapped line starts where the wrap said, and ends where
    // the next row starts; the last row, and an unwrapped one, at the
    // pane's edge.
    let (checkpoint, visible_start, visible_end, segment, hanging) = match piece {
        Some(piece) => (
            HorizontalCheckpoint {
                byte: piece.start.byte,
                column: piece.start.column,
            },
            piece.start.column,
            piece.end_column.unwrap_or(usize::MAX).min(
                piece
                    .start
                    .column
                    .saturating_add(usize::from(area.width.saturating_sub(piece.hanging))),
            ),
            piece.segment,
            piece.hanging,
        ),
        None => (
            horizontal_cache.checkpoint_for(
                document,
                line,
                viewport.left_column,
                viewport.tab_width,
            ),
            viewport.left_column,
            viewport.left_column.saturating_add(usize::from(area.width)),
            0,
            0,
        ),
    };
    // Where the row's cells start on screen: past the hanging indent.
    let left = area.x.saturating_add(hanging);
    let mut column = checkpoint.column;
    let mut cells = Vec::new();
    let mut traversed = checkpoint.byte;
    for (relative, cluster) in RopeGraphemes::new_at(text, checkpoint.byte) {
        if column >= visible_end {
            break;
        }
        horizontal_cache.note_traversed(1);
        let cluster_bytes = cluster.len_bytes();
        let (mut display, mut width) = display_rope_cluster(cluster, column, viewport.tab_width);
        // An inline control's room: the cluster is that much wider, its
        // display padded to fill it.
        let extra = horizontal_cache.extra_at(content.start.0 + relative);
        if extra > 0 {
            width = width.saturating_add(extra);
            display.extend(std::iter::repeat_n(' ', extra));
        }
        let end_column = column.saturating_add(width);
        if end_column > visible_start && column < visible_end {
            let clipped_start = column.max(visible_start);
            let clipped_end = end_column.min(visible_end);
            let screen_start =
                left.saturating_add(clipped_start.saturating_sub(visible_start) as u16);
            let screen_end = left.saturating_add(clipped_end.saturating_sub(visible_start) as u16);
            let clipped = clipped_start != column || clipped_end != end_column;
            cells.push(CellSpan {
                bytes: ByteOffset(content.start.0 + relative)
                    ..ByteOffset(content.start.0 + relative + cluster_bytes),
                columns: column..end_column,
                screen_x: screen_start..screen_end,
                display: if clipped {
                    " ".repeat(clipped_end.saturating_sub(clipped_start))
                } else {
                    display
                },
                widened: extra > 0,
            });
        }
        traversed = relative.saturating_add(cluster_bytes);
        column = end_column;
    }
    TextRow {
        screen_y: y,
        global_row,
        line,
        first_column: visible_start,
        segment,
        hanging,
        visible_end: ByteOffset(content.start.0.saturating_add(traversed)),
        content,
        display_columns: column,
        cells,
    }
}

#[cfg(test)]
pub(crate) fn display_column_of_offset(
    document: &Document,
    line: usize,
    offset: ByteOffset,
    tab_width: u8,
) -> usize {
    let range = document.line_content_range(line);
    let relative_end = offset.0.clamp(range.start.0, range.end.0) - range.start.0;
    let text = document.rope().byte_slice(range.start.0..range.end.0);
    let mut column = 0usize;
    for (relative, cluster) in RopeGraphemes::new(text) {
        if relative >= relative_end {
            break;
        }
        column = column.saturating_add(rope_cluster_width(cluster, column, tab_width));
    }
    column
}

pub(crate) fn display_column_of_offset_cached(
    document: &Document,
    line: usize,
    offset: ByteOffset,
    tab_width: u8,
    cache: &mut HorizontalLayoutCache,
) -> usize {
    let range = document.line_content_range(line);
    let relative_end = offset.0.clamp(range.start.0, range.end.0) - range.start.0;
    let text = document.rope().byte_slice(range.start.0..range.end.0);
    let checkpoint = cache.checkpoint_for_byte(document, line, relative_end, tab_width);
    if checkpoint.byte == relative_end {
        return checkpoint.column;
    }
    let mut column = checkpoint.column;
    for (relative, cluster) in RopeGraphemes::new_at(text, checkpoint.byte) {
        if relative >= relative_end {
            break;
        }
        cache.note_traversed(1);
        column = column
            .saturating_add(rope_cluster_width(cluster, column, tab_width))
            .saturating_add(cache.extra_at(range.start.0 + relative));
    }
    column
}

#[cfg(test)]
pub(crate) fn offset_at_display_column(
    document: &Document,
    line: usize,
    target: usize,
    tab_width: u8,
) -> ByteOffset {
    let range = document.line_content_range(line);
    let text = document.rope().byte_slice(range.start.0..range.end.0);
    let mut column = 0usize;
    for (relative, cluster) in RopeGraphemes::new(text) {
        let cluster_bytes = cluster.len_bytes();
        let width = rope_cluster_width(cluster, column, tab_width);
        if target.saturating_sub(column).saturating_mul(2) < width {
            return ByteOffset(range.start.0 + relative);
        }
        let end = column.saturating_add(width);
        if target < end {
            return ByteOffset(range.start.0 + relative + cluster_bytes);
        }
        column = end;
    }
    range.end
}

pub(crate) fn offset_at_display_column_cached(
    document: &Document,
    line: usize,
    target: usize,
    tab_width: u8,
    cache: &mut HorizontalLayoutCache,
) -> ByteOffset {
    let range = document.line_content_range(line);
    let text = document.rope().byte_slice(range.start.0..range.end.0);
    let checkpoint = cache.checkpoint_for(document, line, target, tab_width);
    let mut column = checkpoint.column;
    for (relative, cluster) in RopeGraphemes::new_at(text, checkpoint.byte) {
        cache.note_traversed(1);
        let cluster_bytes = cluster.len_bytes();
        let width = rope_cluster_width(cluster, column, tab_width)
            .saturating_add(cache.extra_at(range.start.0 + relative));
        if target.saturating_sub(column).saturating_mul(2) < width {
            return ByteOffset(range.start.0 + relative);
        }
        let end = column.saturating_add(width);
        if target < end {
            return ByteOffset(range.start.0 + relative + cluster_bytes);
        }
        column = end;
    }
    range.end
}

/// Grapheme iterator over Rope chunks, adapted from Ropey's official example.
/// It lets viewport layout stop at the right edge without allocating or
/// scanning the rest of a multi-megabyte logical line.
struct RopeGraphemes<'a> {
    text: RopeSlice<'a>,
    chunks: Chunks<'a>,
    chunk: &'a str,
    chunk_start: usize,
    cursor: GraphemeCursor,
}

impl<'a> RopeGraphemes<'a> {
    #[cfg(test)]
    fn new(text: RopeSlice<'a>) -> Self {
        Self::new_at(text, 0)
    }

    fn new_at(text: RopeSlice<'a>, start: usize) -> Self {
        let start = start.min(text.len_bytes());
        let (mut chunks, chunk_start, _, _) = text.chunks_at_byte(start);
        let chunk = chunks.next().unwrap_or("");
        Self {
            text,
            chunks,
            chunk,
            chunk_start,
            cursor: GraphemeCursor::new(start, text.len_bytes(), true),
        }
    }
}

impl<'a> Iterator for RopeGraphemes<'a> {
    type Item = (usize, RopeSlice<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        let start = self.cursor.cur_cursor();
        let end = loop {
            match self.cursor.next_boundary(self.chunk, self.chunk_start) {
                Ok(None) => return None,
                Ok(Some(end)) => break end,
                Err(GraphemeIncomplete::NextChunk) => {
                    self.chunk_start = self.chunk_start.saturating_add(self.chunk.len());
                    self.chunk = self.chunks.next().unwrap_or("");
                }
                Err(GraphemeIncomplete::PreContext(index)) => {
                    let (chunk, chunk_start, _, _) =
                        self.text.chunk_at_byte(index.saturating_sub(1));
                    self.cursor.provide_context(chunk, chunk_start);
                }
                Err(_) => unreachable!("forward grapheme iteration requested a previous chunk"),
            }
        };
        let grapheme = if start < self.chunk_start {
            let start_char = self.text.byte_to_char(start);
            let end_char = self.text.byte_to_char(end);
            self.text.slice(start_char..end_char)
        } else {
            (&self.chunk[start - self.chunk_start..end - self.chunk_start]).into()
        };
        Some((start, grapheme))
    }
}

fn display_rope_cluster(cluster: RopeSlice<'_>, column: usize, tab_width: u8) -> (String, usize) {
    if cluster.len_bytes() > MAX_RENDERED_GRAPHEME_BYTES {
        return ("�".into(), 1);
    }
    let cluster = cluster
        .as_str()
        .map(Cow::Borrowed)
        .unwrap_or_else(|| Cow::Owned(cluster.to_string()));
    display_cluster(cluster.as_ref(), column, tab_width)
}

fn rope_cluster_width(cluster: RopeSlice<'_>, column: usize, tab_width: u8) -> usize {
    if cluster.len_bytes() > MAX_RENDERED_GRAPHEME_BYTES {
        return 1;
    }
    let cluster = cluster
        .as_str()
        .map(Cow::Borrowed)
        .unwrap_or_else(|| Cow::Owned(cluster.to_string()));
    let cluster = cluster.as_ref();
    if cluster == "\t" {
        let tab = usize::from(tab_width.max(1));
        return tab - (column % tab);
    }
    if cluster.chars().any(is_unsafe_terminal_character) {
        return UnicodeWidthStr::width(
            cluster
                .chars()
                .map(control_picture)
                .collect::<String>()
                .as_str(),
        )
        .max(1);
    }
    UnicodeWidthStr::width(cluster).max(1)
}

fn display_cluster(cluster: &str, column: usize, tab_width: u8) -> (String, usize) {
    if cluster.len() > MAX_RENDERED_GRAPHEME_BYTES {
        return ("�".into(), 1);
    }
    if cluster == "\t" {
        let tab = usize::from(tab_width.max(1));
        let width = tab - (column % tab);
        return (" ".repeat(width), width);
    }
    if cluster.chars().any(is_unsafe_terminal_character) {
        let rendered = cluster.chars().map(control_picture).collect::<String>();
        let width = UnicodeWidthStr::width(rendered.as_str()).max(1);
        return (rendered, width);
    }
    let width = UnicodeWidthStr::width(cluster);
    if width == 0 {
        let rendered = format!("◌{cluster}");
        return (rendered, UnicodeWidthStr::width("◌").max(1));
    }
    (cluster.to_owned(), width)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(text: &str) -> Document {
        Document::new(text, 8 * 1024 * 1024).unwrap()
    }

    #[test]
    fn unicode_cells_and_tabs_round_trip() {
        let document = document("a\t界e\u{301}");
        let map = ScreenMap::build(
            &document,
            Viewport::default(),
            &[],
            GridRect::new(0, 0, 20, 1),
        )
        .unwrap();
        let ScreenRow::Text(row) = &map.rows()[0] else {
            panic!("text row")
        };
        assert_eq!(row.display_columns, 7); // a + 3 tab + 2 CJK + 1 grapheme
        assert_eq!(row.hit_test(4).0, ByteOffset(2));
        assert_eq!(row.hit_test(5).0, ByteOffset("a\t界".len()));
        assert_eq!(
            display_column_of_offset(&document, 0, ByteOffset("a\t界".len()), 4),
            6
        );
        assert_eq!(
            offset_at_display_column(&document, 0, 6, 4),
            ByteOffset("a\t界".len())
        );
    }

    #[test]
    fn virtual_rows_change_y_but_never_text_x() {
        let document = document("abcd\nsecond");
        let base = ScreenMap::build(
            &document,
            Viewport::default(),
            &[],
            GridRect::new(3, 2, 20, 10),
        )
        .unwrap();
        let virtuals = [VirtualRowSpec::new("scope", ByteOffset(2), 2)];
        let with = ScreenMap::build(
            &document,
            Viewport::default(),
            &virtuals,
            GridRect::new(3, 2, 20, 10),
        )
        .unwrap();
        assert_eq!(
            base.cell_for_offset(ByteOffset(2)).unwrap().x,
            with.cell_for_offset(ByteOffset(2)).unwrap().x
        );
        assert_eq!(
            base.cell_for_offset(ByteOffset(5)).unwrap().y + 2,
            with.cell_for_offset(ByteOffset(5)).unwrap().y
        );
        assert!(matches!(with.rows()[1], ScreenRow::Virtual(_)));
    }

    #[test]
    fn control_bytes_are_never_emitted_to_the_terminal() {
        let document = document("ok\u{1b}[31m\u{202e}bad");
        let map = ScreenMap::build(
            &document,
            Viewport::default(),
            &[],
            GridRect::new(0, 0, 80, 1),
        )
        .unwrap();
        let ScreenRow::Text(row) = &map.rows()[0] else {
            panic!("text row")
        };
        let rendered = row
            .cells
            .iter()
            .map(|cell| cell.display.as_str())
            .collect::<String>();
        assert!(!rendered.contains('\u{1b}'));
        assert!(!rendered.contains('\u{202e}'));
        assert!(rendered.contains('␛'));
    }

    #[test]
    fn screen_map_only_materialises_visible_rows() {
        let text = (0..50_000).map(|_| "x\n").collect::<String>();
        let document = document(&text);
        let map = ScreenMap::build(
            &document,
            Viewport {
                top_row: 40_000,
                ..Viewport::default()
            },
            &[],
            GridRect::new(0, 0, 80, 24),
        )
        .unwrap();
        assert_eq!(map.rows().len(), 24);
        let ScreenRow::Text(first) = &map.rows()[0] else {
            panic!("text")
        };
        assert_eq!(first.line, 40_000);
    }

    /// Wrapped at ten columns, a line breaks after the last space that
    /// fits, or inside a word longer than a row, and a space at the edge
    /// hangs past it. The rows after the first are set in four cells.
    /// Every row knows its line and its place, the caret lands on the
    /// right row, a click comes back to the same offset, and an inline
    /// widget still follows the whole of its line.
    #[test]
    fn wrapped_rows_break_after_spaces_and_read_back() {
        let document = document("abcdefghij klmnopqrst uvwxyz\nabcdefghijklmnop\nx\n");
        let cache = HorizontalLayoutCache::default();
        let wrap = WrapIndex::build(&document, 10, 4, &cache, &[]);
        assert_eq!(wrap.hanging_of(0), 4);
        // Ten cells, then six a row: "klmnop" breaks inside, "uvwxyz" fits.
        assert_eq!(wrap.rows_of(0), 4);
        assert_eq!(
            wrap.start_of(0, 1),
            RowStart {
                column: 11,
                byte: 11
            }
        );
        assert_eq!(
            wrap.start_of(0, 2),
            RowStart {
                column: 17,
                byte: 17
            }
        );
        assert_eq!(
            wrap.start_of(0, 3),
            RowStart {
                column: 22,
                byte: 22
            }
        );
        assert_eq!(wrap.rows_of(1), 2, "a long word breaks inside");
        assert_eq!(
            wrap.start_of(1, 1),
            RowStart {
                column: 10,
                byte: 10
            }
        );
        assert_eq!(wrap.rows_of(2), 1);
        assert_eq!(
            wrap.total_rows(),
            8,
            "four, two, one, and the empty last line"
        );
        assert_eq!(wrap.locate(5), (1, 1));
        assert_eq!(wrap.segment_of(0, 21), 2, "the hanging space is its row's");
        assert_eq!(
            wrap.segment_of(0, 11),
            1,
            "a caret pushed past the edge starts the next row"
        );
        assert_eq!(
            wrap.segment_of(0, 17),
            1,
            "a boundary caret that fits is the earlier row's"
        );
        assert_eq!(
            wrap.segment_of(0, 22),
            2,
            "the trailing space's row, not the next"
        );

        let viewport = Viewport {
            page_columns: 11,
            wrap: true,
            ..Viewport::default()
        };
        let area = GridRect::new(0, 0, 11, 20);
        let mut cache = HorizontalLayoutCache::default();
        let map = ScreenMap::build_wrapped(
            &document,
            viewport,
            &[VirtualRowSpec {
                id: "scope".into(),
                after: ByteOffset(3),
                height: 2,
            }],
            area,
            &mut cache,
            Some(&wrap),
        )
        .unwrap();
        let kinds: Vec<(usize, usize, bool)> = map
            .rows()
            .iter()
            .map(|row| match row {
                ScreenRow::Text(row) => (row.line, row.segment, true),
                ScreenRow::Virtual(row) => (usize::from(row.inner_row), 0, false),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                (0, 0, true),
                (0, 1, true),
                (0, 2, true),
                (0, 3, true),
                (0, 0, false),
                (1, 0, false),
                (1, 0, true),
                (1, 1, true),
                (2, 0, true),
                (3, 0, true),
            ],
            "the widget hangs after the whole wrapped line"
        );
        let ScreenRow::Text(second) = &map.rows()[1] else {
            panic!("text")
        };
        assert_eq!(second.first_column, 11);
        assert_eq!(second.hanging, 4);
        assert_eq!(second.cells[0].bytes.start, ByteOffset(11));
        assert_eq!(second.cells[0].screen_x, 4..5, "a later row is set in");
        let ScreenRow::Text(first) = &map.rows()[0] else {
            panic!("text")
        };
        assert_eq!(
            first.cells.last().unwrap().screen_x,
            10..11,
            "the space hangs in the caret's cell"
        );
        // The caret: on the row whose cells hold the offset, set in with
        // it, and at the end of the line on the last row.
        assert_eq!(
            map.cell_for_offset(ByteOffset(13)),
            Some(CellPoint::new(6, 1))
        );
        assert_eq!(
            map.cell_for_offset(ByteOffset(28)),
            Some(CellPoint::new(10, 3))
        );
        assert_eq!(
            map.cell_for_offset(ByteOffset(11)),
            Some(CellPoint::new(4, 1))
        );
        assert!(matches!(
            map.hit_test(CellPoint::new(6, 1)),
            Some(Hit::Text {
                offset: ByteOffset(13),
                ..
            })
        ));
        assert!(matches!(
            map.hit_test(CellPoint::new(3, 6)),
            Some(Hit::Text {
                line: 1,
                offset: ByteOffset(32),
                ..
            })
        ));
    }

    /// At every offset of every wrapped line, row boundaries included,
    /// `segment_of` picks the row `cell_for_offset` draws the caret on.
    #[test]
    fn the_row_segment_of_picks_is_the_row_the_caret_is_drawn_on() {
        let document = document("abcdefghij klmnopqrst uvwxyz\nabcdefghijklmnop\nx\n");
        let cache = HorizontalLayoutCache::default();
        let wrap = WrapIndex::build(&document, 10, 4, &cache, &[]);
        let map = ScreenMap::build_wrapped(
            &document,
            Viewport {
                page_columns: 11,
                wrap: true,
                ..Viewport::default()
            },
            &[],
            GridRect::new(0, 0, 11, 20),
            &mut HorizontalLayoutCache::default(),
            Some(&wrap),
        )
        .unwrap();
        for line in 0..document.line_count() {
            let content = document.line_content_range(line);
            for offset in content.start.0..=content.end.0 {
                let offset = ByteOffset(offset);
                let cell = map
                    .cell_for_offset(offset)
                    .unwrap_or_else(|| panic!("offset {offset:?} of line {line} is drawn"));
                let row = map
                    .rows()
                    .iter()
                    .find(|row| row.screen_y() == cell.y)
                    .expect("the drawn row is on the map");
                let ScreenRow::Text(drawn) = row else {
                    panic!("the drawn row is a text row");
                };
                let column = display_column_of_offset(&document, line, offset, 4);
                assert_eq!(
                    (drawn.line, drawn.segment),
                    (line, wrap.segment_of(line, column)),
                    "offset {offset:?} of line {line} moves as it is drawn"
                );
            }
        }
    }

    /// A chain breaks before its dots, never inside a string, and a string
    /// longer than a row breaks at its spaces rather than inside a word.
    #[test]
    fn a_chain_wraps_before_its_dots_and_a_string_only_at_its_spaces() {
        let line = "$: s(\"swpad\").scrub(pick(chops, \"<0@3 [1 2]>\")).n(4).gain(0.8).phaser(.4)._punchcard()";
        let chain = document(&format!(
            "{line}\n    .room(0.5).size(0.9).delay(0.25).delaytime(0.125)\n"
        ));
        let cache = HorizontalLayoutCache::default();
        let wrap = WrapIndex::build(&chain, 52, 4, &cache, &[]);
        assert_eq!(wrap.rows_of(0), 2);
        assert_eq!(
            wrap.start_of(0, 1),
            RowStart {
                column: 47,
                byte: 47
            },
            "the row breaks before .n(, not inside the mini-notation"
        );
        assert_eq!(&line[47..52], ".n(4)");
        // An indented line sets its later rows in under its own indent.
        assert_eq!(wrap.hanging_of(1), 8);
        assert_eq!(wrap.rows_of(1), 2);
        let second = wrap.start_of(1, 1);
        assert_eq!(
            &chain.rope().line(1).to_string()[second.byte..second.byte + 6],
            ".delay"
        );

        let long = document("s(\"bd sd hh cp bd sd hh cp bd sd hh cp\")\n");
        let wrap = WrapIndex::build(&long, 20, 4, &cache, &[]);
        let text = long.rope().line(0).to_string();
        for segment in 1..wrap.rows_of(0) {
            let start = wrap.start_of(0, segment);
            assert_eq!(
                text.as_bytes()[start.byte - 1],
                b' ',
                "row {segment} starts after a space, at byte {}",
                start.byte
            );
        }
        assert_eq!(
            wrap.start_of(0, 1).column,
            21,
            "the space at the edge hangs and the row starts after it"
        );
    }

    /// A drag begun below the text anchors at the last line's end, and one
    /// begun above it at the first line's start. Within a row of text the
    /// pointer's column still decides.
    #[test]
    fn a_drag_begun_below_or_above_the_text_anchors_at_its_edge() {
        let document = document("short");
        let map = ScreenMap::build(
            &document,
            Viewport::default(),
            &[],
            GridRect::new(2, 3, 20, 10),
        )
        .unwrap();
        let Hit::Text { offset, .. } = map
            .hit_test_clamped(CellPoint::new(9, 8))
            .expect("the blank half is hittable")
        else {
            panic!("text hit")
        };
        assert_eq!(
            offset,
            ByteOffset(5),
            "below the text, the anchor is the last line's end"
        );
        let Hit::Text { offset, .. } = map
            .hit_test_clamped(CellPoint::new(9, 1))
            .expect("above the text is hittable")
        else {
            panic!("text hit")
        };
        assert_eq!(
            offset,
            ByteOffset(0),
            "above the text, the anchor is the first line's start"
        );
        // Within a text row the pointer's column still rules.
        let Hit::Text { offset, .. } = map.hit_test(CellPoint::new(4, 3)).unwrap() else {
            panic!("text hit")
        };
        assert_eq!(offset, ByteOffset(2));
    }

    #[test]
    fn a_multi_megabyte_line_is_traversed_only_to_the_viewport_edge() {
        let document = document(&"a".repeat(4 * 1024 * 1024));
        let map = ScreenMap::build(
            &document,
            Viewport::default(),
            &[],
            GridRect::new(0, 0, 80, 1),
        )
        .unwrap();
        let ScreenRow::Text(row) = &map.rows()[0] else {
            panic!("text row")
        };
        assert_eq!(row.cells.len(), 80);
        assert_eq!(row.visible_end, ByteOffset(80));
        assert_eq!(row.content.end, ByteOffset(4 * 1024 * 1024));
    }

    #[test]
    fn horizontal_scroll_width_is_cached_across_pointer_events() {
        // More lines than the checkpoint LRU can hold: the full extent still
        // must not rescan the score on every mouse event or frame.
        let document = document(&format!("{}\n", "a".repeat(1024)).repeat(160));
        let mut cache = HorizontalLayoutCache::default();
        assert_eq!(cache.maximum_line_width(&document, 4), 1024);
        assert!(cache.take_traversed_graphemes() >= 160 * 1024);
        for _ in 0..200 {
            assert_eq!(cache.maximum_line_width(&document, 4), 1024);
        }
        assert_eq!(
            cache.take_traversed_graphemes(),
            0,
            "warm scrolling never remeasures the score"
        );
    }

    #[test]
    fn horizontal_scroll_width_cache_tracks_tabs_widgets_and_edits() {
        let mut document = document("\tabc\nz");
        let mut cache = HorizontalLayoutCache::default();
        assert_eq!(cache.maximum_line_width(&document, 4), 7);
        assert_eq!(cache.maximum_line_width(&document, 8), 11);
        cache.set_inline_widths(vec![InlineWidth {
            at: ByteOffset(1),
            extra: 5,
        }]);
        assert_eq!(cache.maximum_line_width(&document, 8), 16);
        cache.set_inline_widths(vec![]);
        assert_eq!(cache.maximum_line_width(&document, 8), 11);
        let before = document.clone();
        let edit = crate::editor::Edit::new(ByteOffset(0)..ByteOffset(6), "q");
        let shape = EditShape::from(&edit);
        document.apply_edits(&[edit]).unwrap();
        cache.map_revision_after_edit(&before, &document, &[shape]);
        assert_eq!(cache.maximum_line_width(&document, 8), 1);
    }

    #[test]
    fn far_right_multi_megabyte_redraw_uses_sparse_checkpoints() {
        let bytes = 4 * 1024 * 1024;
        let document = document(&"a".repeat(bytes));
        let viewport = Viewport {
            left_column: bytes - 80,
            ..Viewport::default()
        };
        let area = GridRect::new(0, 0, 80, 1);
        let mut cache = HorizontalLayoutCache::default();

        let first =
            ScreenMap::build_wrapped(&document, viewport, &[], area, &mut cache, None).unwrap();
        let first_work = cache.take_traversed_graphemes();
        let ScreenRow::Text(first_row) = &first.rows()[0] else {
            panic!("text row")
        };
        assert_eq!(
            first_row.cells.first().unwrap().bytes.start,
            ByteOffset(bytes - 80)
        );
        assert_eq!(first_row.visible_end, ByteOffset(bytes));
        assert!(first_work >= bytes - 80);

        let second =
            ScreenMap::build_wrapped(&document, viewport, &[], area, &mut cache, None).unwrap();
        let redraw_work = cache.take_traversed_graphemes();
        let ScreenRow::Text(second_row) = &second.rows()[0] else {
            panic!("text row")
        };
        assert_eq!(second_row.cells, first_row.cells);
        assert!(
            redraw_work <= HORIZONTAL_CHECKPOINT_BYTES + usize::from(area.width),
            "redraw traversed {redraw_work} graphemes instead of seeking near the viewport"
        );

        let before_edit = document.clone();
        let mut after_edit = document;
        let edit = crate::editor::Edit::new(ByteOffset(bytes)..ByteOffset(bytes), "b");
        let shape = EditShape::from(&edit);
        after_edit.apply_edits(&[edit]).unwrap();
        cache.map_revision_after_edit(&before_edit, &after_edit, &[shape]);
        let edited_viewport = Viewport {
            left_column: bytes + 1 - 80,
            ..Viewport::default()
        };
        let edited =
            ScreenMap::build_wrapped(&after_edit, edited_viewport, &[], area, &mut cache, None)
                .unwrap();
        let edit_work = cache.take_traversed_graphemes();
        let ScreenRow::Text(edited_row) = &edited.rows()[0] else {
            panic!("text row")
        };
        assert_eq!(edited_row.visible_end, ByteOffset(bytes + 1));
        assert!(
            edit_work <= HORIZONTAL_CHECKPOINT_BYTES + usize::from(area.width),
            "far-right edit invalidated the unchanged line prefix ({edit_work} graphemes)"
        );
    }
}
