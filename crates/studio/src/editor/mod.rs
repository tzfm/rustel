//! Modeless terminal editor core.
//!
//! The document, history and screen map are independent of the renderer.  A
//! Ratatui pane can draw [`ScreenMap::rows`] and route Crossterm input through
//! [`input::event_to_input`] without giving presentation code ownership of the
//! score or its UTF-8 source coordinates.

pub mod brackets;
mod clipboard;
mod document;
mod history;
mod input;
mod layout;
mod selection;

use std::cell::RefCell;
use std::collections::{BTreeSet, VecDeque};
use std::fmt;

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ropey::Rope;

pub use clipboard::{Clipboard, ClipboardError, MemoryClipboard, PlatformClipboard};
pub use document::{ByteOffset, Document, DocumentError, Edit, Revision};
pub use history::{
    DEFAULT_HISTORY_GROUP_DELAY_MS, DEFAULT_HISTORY_MAX_BYTES, DEFAULT_HISTORY_MAX_ENTRIES,
    EditOrigin, HistoryConfig, HistoryMoment, Transaction,
};
pub use input::{
    EditorInput, KeyboardCapabilities, PRIMARY_MODIFIER, click_extends, event_to_input,
    event_to_input_with_binds, is_stray_control, key_to_command,
};
pub(crate) use input::{TerminalEventBatch, is_plain_text_key_event};
pub use layout::{
    Affinity, CellPoint, CellSpan, GridRect, Hit, InlineVirtualRow, InlineWidth, RowStart,
    ScreenMap, ScreenRow, TextRow, Viewport, VirtualRowError, VirtualRowSpec, WRAP_MAX_LINE_BYTES,
    WrapIndex,
};
pub use selection::{Selection, SelectionSet};

use document::EditShape;
use history::{History, HistoryEntry};
use layout::{
    HorizontalLayoutCache, VirtualRows, display_column_of_offset_cached,
    offset_at_display_column_cached,
};
use selection::map_offset_shapes;

pub const DEFAULT_MAX_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
/// Line-comment marker. Scores are JavaScript, so this is not configurable.
const COMMENT_MARKER: &str = "//";
const COMMENT_PREFIX: &str = "// ";
/// Edit groups retained for mapping evaluated source ranges forward. Past
/// this a highlight from a very old revision is simply dropped, which is what
/// the renderer already does for an unknown generation.
pub const DEFAULT_WHEEL_ROWS: usize = 3;
const MULTI_CLICK_WINDOW_MS: u64 = 500;

/// Inputs that change the document's row layout, independent of scrolling.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OverviewLayout {
    viewport: Viewport,
    widths_epoch: u64,
    virtual_rows: Vec<VirtualRowSpec>,
    wrap_indents: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct EditorConfig {
    pub maximum_document_bytes: usize,
    pub history: HistoryConfig,
    pub tab_width: u8,
    pub wheel_rows: usize,
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            maximum_document_bytes: DEFAULT_MAX_DOCUMENT_BYTES,
            history: HistoryConfig::default(),
            tab_width: layout::DEFAULT_TAB_WIDTH,
            wheel_rows: DEFAULT_WHEEL_ROWS,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    GroupLeft,
    GroupRight,
    LineStart,
    LineEnd,
    DocumentStart,
    DocumentEnd,
    PageUp,
    PageDown,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    InsertText(String),
    /// Replace the bytes `from..to` with `text`, the caret ending after it.
    ReplaceRange {
        from: ByteOffset,
        to: ByteOffset,
        text: String,
    },
    /// Bracketed paste payload. It never goes through per-character handling.
    PasteText(String),
    Newline,
    Indent,
    Outdent,
    DeleteBackward,
    DeleteForward,
    DeleteWordBackward,
    DeleteWordForward,
    Move {
        motion: Motion,
        extend: bool,
    },
    SelectAll,
    /// Comment or uncomment every line the selections touch (`Ctrl+/`).
    ToggleComment,
    Copy,
    Cut,
    Paste,
    Undo,
    Redo,
    Evaluate,
    Stop,
}

#[derive(Clone, Debug)]
pub enum EditorEffect {
    Evaluate { revision: Revision, source: Rope },
    Stop,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EditorError {
    Document(DocumentError),
    Clipboard(ClipboardError),
    VirtualRows(VirtualRowError),
    StaleScreenMap {
        map_revision: Revision,
        document_revision: Revision,
        map_viewport: Viewport,
        editor_viewport: Viewport,
    },
}

impl fmt::Display for EditorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Document(error) => error.fmt(formatter),
            Self::Clipboard(error) => error.fmt(formatter),
            Self::VirtualRows(error) => error.fmt(formatter),
            Self::StaleScreenMap {
                map_revision,
                document_revision,
                map_viewport,
                editor_viewport,
            } => write!(
                formatter,
                "mouse map is stale (revision {}, viewport {:?}); editor is at revision {}, viewport {:?}",
                map_revision.0, map_viewport, document_revision.0, editor_viewport
            ),
        }
    }
}

impl std::error::Error for EditorError {}

impl From<DocumentError> for EditorError {
    fn from(value: DocumentError) -> Self {
        Self::Document(value)
    }
}

impl From<ClipboardError> for EditorError {
    fn from(value: ClipboardError) -> Self {
        Self::Clipboard(value)
    }
}

impl From<VirtualRowError> for EditorError {
    fn from(value: VirtualRowError) -> Self {
        Self::VirtualRows(value)
    }
}

/// Contiguous record of the edits applied since each retained revision.
///
/// Entries are kept in application order and are contiguous: each one's
/// `after` is the next one's `before`. That is what makes replaying a suffix
/// of the trail equivalent to replaying every edit since a given revision.
#[derive(Clone, Debug, Default)]
struct EditTrail {
    entries: VecDeque<TrailEntry>,
    /// Revisions a reader still maps from - the evaluated score's, whose
    /// coordinates every layout anchor and sounding-event mark arrives in.
    /// The trail never merges a boundary away while it is pinned.
    pinned: Vec<Revision>,
}

/// Past this many entries the trail folds its oldest neighbours together
/// (an entry can hold several steps) rather than forgetting the oldest;
/// only a pinned boundary is kept apart, so mapping from the evaluated
/// revision survives a held slider key indefinitely.
const MAX_EDIT_TRAIL_ENTRIES: usize = 256;
/// The hard ceiling on retained steps, beyond which the oldest really are
/// forgotten: a boundary pinned that long ago has been mapped from for
/// ten minutes of continuous editing.
const MAX_EDIT_TRAIL_STEPS_TOTAL: usize = 16_384;

#[derive(Clone, Debug)]
struct TrailEntry {
    before: Revision,
    after: Revision,
    steps: Vec<Vec<EditShape>>,
}

impl EditTrail {
    fn record(&mut self, before: Revision, after: Revision, steps: &[Vec<EditShape>]) {
        if after == before || steps.iter().all(Vec::is_empty) {
            return;
        }
        self.entries.push_back(TrailEntry {
            before,
            after,
            steps: steps.to_vec(),
        });
        while self.entries.len() > MAX_EDIT_TRAIL_ENTRIES {
            // Fold the oldest pair whose shared boundary nobody maps from.
            // The steps stay in order, so replaying the folded entry is
            // replaying both; only the boundary between them is gone.
            let foldable = (0..self.entries.len().saturating_sub(1))
                .find(|&index| !self.pinned.contains(&self.entries[index].after));
            match foldable {
                Some(index) => {
                    let next = self.entries.remove(index + 1).expect("pair");
                    let entry = &mut self.entries[index];
                    entry.after = next.after;
                    entry.steps.extend(next.steps);
                }
                None => break,
            }
        }
        let mut total: usize = self.entries.iter().map(|entry| entry.steps.len()).sum();
        while total > MAX_EDIT_TRAIL_STEPS_TOTAL {
            let Some(oldest) = self.entries.pop_front() else {
                break;
            };
            total -= oldest.steps.len();
        }
    }

    fn pin(&mut self, revision: Revision) {
        if !self.pinned.contains(&revision) {
            self.pinned.push(revision);
        }
    }

    fn unpin(&mut self, revision: Revision) {
        self.pinned.retain(|pinned| *pinned != revision);
    }

    /// Index of the first retained entry applied after `revision`, or the
    /// entry count when `revision` is already the newest recorded state.
    /// `None` when that revision has aged out of the trail.
    fn suffix_since(&self, revision: Revision) -> Option<usize> {
        if self
            .entries
            .back()
            .is_some_and(|entry| entry.after == revision)
        {
            return Some(self.entries.len());
        }
        self.entries
            .iter()
            .position(|entry| entry.before == revision)
    }

    /// Edit steps from `index` onward, oldest first.
    fn steps_from(&self, index: usize) -> impl Iterator<Item = &Vec<EditShape>> {
        self.entries
            .range(index.min(self.entries.len())..)
            .flat_map(|entry| entry.steps.iter())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DragGranularity {
    Grapheme,
    Word,
    Line,
}

#[derive(Clone, Debug)]
struct DragState {
    anchor: Selection,
    granularity: DragGranularity,
    last_point: CellPoint,
}

#[derive(Clone, Copy, Debug)]
struct LastClick {
    point: CellPoint,
    moment: HistoryMoment,
    count: u8,
}

/// Canonical editor state. There is no mode switch: printable input always
/// inserts, pointer input always places/extends a caret, and commands operate
/// on the current selection exactly as in a graphical code editor.
#[derive(Clone, Debug)]
pub struct Editor {
    document: Document,
    selections: SelectionSet,
    history: History,
    viewport: Viewport,
    virtual_rows: VirtualRows,
    horizontal_layout: RefCell<HorizontalLayoutCache>,
    /// Where the lines break when the text wraps, for the viewport's
    /// width; rebuilt when the text, the width or the inline widths change.
    wrap_index: RefCell<WrapIndex>,
    /// Optional continuation columns for read-only, columnar text such as logs.
    wrap_indents: Vec<usize>,
    drag: Option<DragState>,
    last_click: Option<LastClick>,
    edits_since: EditTrail,
    config: EditorConfig,
}

impl Editor {
    pub fn new(text: &str) -> Result<Self, EditorError> {
        Self::with_config(text, EditorConfig::default())
    }

    pub fn with_config(text: &str, config: EditorConfig) -> Result<Self, EditorError> {
        let document = Document::new(text, config.maximum_document_bytes)?;
        let viewport = Viewport {
            tab_width: config.tab_width.max(1),
            ..Viewport::default()
        };
        Ok(Self {
            document,
            selections: SelectionSet::single(ByteOffset::ZERO),
            history: History::new(config.history),
            viewport,
            virtual_rows: VirtualRows::default(),
            horizontal_layout: RefCell::new(HorizontalLayoutCache::default()),
            wrap_index: RefCell::new(WrapIndex::default()),
            wrap_indents: Vec::new(),
            drag: None,
            last_click: None,
            edits_since: EditTrail::default(),
            config,
        })
    }

    pub fn document(&self) -> &Document {
        &self.document
    }

    pub fn source(&self) -> String {
        self.document.text()
    }

    pub fn revision(&self) -> Revision {
        self.document.revision()
    }

    pub fn selections(&self) -> &SelectionSet {
        &self.selections
    }

    pub fn primary_selection(&self) -> Selection {
        self.selections.primary()
    }

    pub fn set_selection(&mut self, selection: Selection) -> Result<(), EditorError> {
        self.document.validate_caret_offset(selection.anchor)?;
        self.document.validate_caret_offset(selection.head)?;
        self.selections.set_single(selection);
        self.history.close_group();
        self.drag = None;
        Ok(())
    }

    /// Reveal a programmatically placed caret, respecting wrapping, inline
    /// widgets, and both scroll axes without changing the document.
    pub fn reveal_selection(&mut self) {
        self.ensure_primary_visible();
    }

    pub fn viewport(&self) -> Viewport {
        self.viewport
    }

    /// Left edge, visible width and full width for a horizontal scrollbar.
    /// Wrapped text has no horizontal extent, and a document that fits needs
    /// no rail.
    pub fn horizontal_scroll_extent(&self) -> Option<(usize, usize, usize)> {
        self.horizontal_scroll_extent_for(self.viewport.page_columns)
    }

    pub(crate) fn horizontal_scroll_extent_for(
        &self,
        columns: usize,
    ) -> Option<(usize, usize, usize)> {
        if self.viewport.wrap {
            return None;
        }
        // Count the same graphemes, tabs and inline widgets as text layout,
        // including the cell occupied by the end-of-line caret.
        let total = self
            .horizontal_layout
            .borrow_mut()
            .maximum_line_width(&self.document, self.viewport.tab_width)
            .saturating_add(1);
        let page = columns.max(1);
        (total > page).then_some((self.viewport.left_column, page, total))
    }

    pub fn set_viewport(&mut self, mut viewport: Viewport) {
        viewport.tab_width = viewport.tab_width.max(1);
        viewport.page_columns = viewport.page_columns.max(1);
        viewport.page_rows = viewport.page_rows.max(1);
        self.viewport = viewport;
        self.clamp_viewport();
    }

    /// Long lines continue on the next row, or run off the edge and
    /// scroll sideways. Turning wrap on brings the scroll back to the left.
    pub fn set_wrap(&mut self, wrap: bool) {
        if self.viewport.wrap == wrap {
            return;
        }
        self.viewport.wrap = wrap;
        if wrap {
            self.viewport.left_column = 0;
        }
        self.clamp_viewport();
        self.ensure_primary_visible();
    }

    /// Override continuation indentation by logical line, without changing
    /// source bytes or selection coordinates. Unspecified lines use code indent.
    pub(super) fn set_wrap_indents(&mut self, indents: Vec<usize>) {
        if self.wrap_indents != indents {
            self.wrap_indents = indents;
            *self.wrap_index.get_mut() = WrapIndex::default();
            self.clamp_viewport();
        }
    }

    /// The wrap index for the viewport's width, brought up to date.
    fn wrap_index(&self) -> Option<std::cell::Ref<'_, WrapIndex>> {
        let width = self.viewport.wrap_width()?;
        {
            let cache = self.horizontal_layout.borrow();
            let mut index = self.wrap_index.borrow_mut();
            if !index.is_current_for(&self.document, width, self.viewport.tab_width, &cache) {
                *index = WrapIndex::build(
                    &self.document,
                    width,
                    self.viewport.tab_width,
                    &cache,
                    &self.wrap_indents,
                );
            }
        }
        Some(self.wrap_index.borrow())
    }

    pub fn set_page_rows(&mut self, rows: usize) {
        self.viewport.page_rows = rows.max(1);
        self.clamp_viewport();
    }

    /// Update the visible text-grid dimensions after layout or resize.
    pub fn set_view_size(&mut self, columns: usize, rows: usize) {
        let columns = columns.max(1);
        let rows = rows.max(1);
        if self.viewport.page_columns == columns && self.viewport.page_rows == rows {
            return;
        }
        let grew_horizontally = columns > self.viewport.page_columns;
        self.viewport.page_columns = columns;
        self.viewport.page_rows = rows;
        self.clamp_viewport();
        let primary_column = self.ensure_primary_visible();
        if grew_horizontally {
            self.restore_horizontal_context(primary_column);
        }
    }

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    pub fn undo_depth(&self) -> usize {
        self.history.undo_depth()
    }

    pub fn redo_depth(&self) -> usize {
        self.history.redo_depth()
    }

    /// What the undo and redo keep, as text: a count the memory breakdown
    /// can read on every frame, where [`Self::source`] would build the whole
    /// document to measure it.
    pub fn undo_bytes(&self) -> usize {
        self.history.retained_bytes()
    }

    pub fn close_history_group(&mut self) {
        self.history.close_group();
    }

    pub fn set_virtual_rows(
        &mut self,
        revision: Revision,
        rows: Vec<VirtualRowSpec>,
    ) -> Result<(), EditorError> {
        self.virtual_rows.replace(&self.document, revision, rows)?;
        self.clamp_viewport();
        Ok(())
    }

    pub fn clear_virtual_rows(&mut self) {
        self.virtual_rows.clear();
        self.clamp_viewport();
    }

    /// The inline controls' room on their rows: each named cluster is that
    /// many columns wider, and the rest of its row moves along. Set with
    /// the current text's offsets before a map is built; the caller keeps
    /// them current.
    pub fn set_inline_widths(&mut self, widths: Vec<InlineWidth>) {
        self.horizontal_layout
            .borrow_mut()
            .set_inline_widths(widths);
        // A pill appearing or going changes where the wrapped lines break,
        // so the row count moves with it - and this is the one invalidation
        // that reaches the wrap index without going through a call that
        // clamps. Left alone the top row can sit past the end of the text
        // and the pane draws as bare background, with no caret and no
        // numbers, until a keystroke happens to clamp it.
        self.clamp_viewport();
    }

    pub fn virtual_rows(&self) -> &[VirtualRowSpec] {
        self.virtual_rows.rows()
    }

    pub fn screen_map(&self, area: GridRect) -> Result<ScreenMap, EditorError> {
        let wrap = self.wrap_index();
        Ok(ScreenMap::build_wrapped(
            &self.document,
            self.viewport,
            self.virtual_rows.rows(),
            area,
            &mut self.horizontal_layout.borrow_mut(),
            wrap.as_deref(),
        )?)
    }

    pub(crate) fn overview_layout(&self) -> OverviewLayout {
        OverviewLayout {
            viewport: Viewport {
                top_row: 0,
                left_column: 0,
                page_rows: 0,
                ..self.viewport
            },
            widths_epoch: self.horizontal_layout.borrow().widths_epoch(),
            virtual_rows: self.virtual_rows.rows().to_vec(),
            wrap_indents: self.wrap_indents.clone(),
        }
    }

    /// Visit the same text and inline rows the pane lays out, from the top
    /// and left edge. Bounded batches keep an overview from retaining a
    /// document's worth of grapheme cells at once.
    pub(crate) fn visit_overview_rows(
        &self,
        mut visit: impl FnMut(&ScreenRow),
    ) -> Result<(), EditorError> {
        const BATCH_ROWS: usize = 128;
        let total = self.total_rows();
        let wrap = self.wrap_index();
        let width = self.viewport.page_columns.min(usize::from(u16::MAX)) as u16;
        let mut cache = self.horizontal_layout.borrow_mut();
        for top_row in (0..total).step_by(BATCH_ROWS) {
            let height = (total - top_row).min(BATCH_ROWS) as u16;
            let viewport = Viewport {
                top_row,
                left_column: 0,
                ..self.viewport
            };
            let map = ScreenMap::build_wrapped(
                &self.document,
                viewport,
                self.virtual_rows.rows(),
                GridRect::new(0, 0, width, height),
                &mut cache,
                wrap.as_deref(),
            )?;
            for row in map.rows() {
                visit(row);
            }
        }
        Ok(())
    }

    pub fn selected_text(&self) -> Result<String, EditorError> {
        let mut parts = Vec::new();
        for selection in self.selections.ranges() {
            if !selection.is_empty() {
                parts.push(self.document.slice(selection.ordered())?);
            }
        }
        Ok(parts.join("\n"))
    }

    /// Follow a source range recorded at `revision` into current coordinates.
    ///
    /// This is what lets a sounding event keep highlighting its own text while
    /// the score is being edited, exactly as the web editor does: the mark
    /// belongs to the evaluated generation, and typing around it moves it
    /// rather than erasing it. `None` means the mark can no longer be placed -
    /// its text was deleted, or the revision is older than the retained trail.
    pub fn map_range_since(
        &self,
        revision: Revision,
        range: std::ops::Range<usize>,
    ) -> Option<std::ops::Range<usize>> {
        if range.start >= range.end {
            return None;
        }
        let mut start = ByteOffset(range.start);
        let mut end = ByteOffset(range.end);
        if revision != self.document.revision() {
            let index = self.edits_since.suffix_since(revision)?;
            for shapes in self.edits_since.steps_from(index) {
                // Text typed at either edge of a mark stays outside it, so a
                // highlight does not creep over characters it never covered.
                start = map_offset_shapes(start, shapes, true);
                end = map_offset_shapes(end, shapes, false);
            }
        }
        let limit = self.document.len_bytes();
        let start = start.0.min(limit);
        let end = end.0.min(limit);
        (start < end).then_some(start..end)
    }

    /// Keep `revision` reachable by [`Self::map_offset_since`] however many
    /// edits follow: the evaluated score's revision, which every layout
    /// anchor and sounding-event mark is expressed in.
    pub fn pin_revision(&mut self, revision: Revision) {
        self.edits_since.pin(revision);
    }

    pub fn unpin_revision(&mut self, revision: Revision) {
        self.edits_since.unpin(revision);
    }

    /// Follow a single source offset from `revision` into current
    /// coordinates. Used to keep an inline visualizer anchored to its own
    /// call while the score around it is edited.
    pub fn map_offset_since(&self, revision: Revision, offset: usize) -> Option<usize> {
        let limit = self.document.len_bytes();
        if revision == self.document.revision() {
            return Some(offset.min(limit));
        }
        let index = self.edits_since.suffix_since(revision)?;
        let mut at = ByteOffset(offset);
        for shapes in self.edits_since.steps_from(index) {
            at = map_offset_shapes(at, shapes, false);
        }
        Some(at.0.min(limit))
    }

    pub fn dispatch(
        &mut self,
        command: Command,
        moment: HistoryMoment,
        clipboard: &mut dyn Clipboard,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        match command {
            Command::InsertText(text) => {
                self.replace_selections(&text, EditOrigin::Typing, moment)?;
            }
            Command::ReplaceRange { from, to, text } => {
                if from <= to && to.0 <= self.document.len_bytes() {
                    self.history.close_group();
                    self.selections.set_single(Selection::range(from, to));
                    self.replace_selections(&text, EditOrigin::Typing, moment)?;
                }
            }
            Command::PasteText(text) => {
                self.paste_text(&text, moment)?;
            }
            Command::Newline => self.insert_newline(moment)?,
            Command::Indent => self.indent(moment)?,
            Command::Outdent => self.outdent(moment)?,
            Command::DeleteBackward => self.delete(false, false, moment)?,
            Command::DeleteForward => self.delete(true, false, moment)?,
            Command::DeleteWordBackward => self.delete(false, true, moment)?,
            Command::DeleteWordForward => self.delete(true, true, moment)?,
            Command::Move { motion, extend } => self.move_selection(motion, extend)?,
            Command::SelectAll => {
                self.history.close_group();
                self.selections.set_single(Selection::range(
                    ByteOffset::ZERO,
                    ByteOffset(self.document.len_bytes()),
                ));
            }
            Command::ToggleComment => self.toggle_comment(moment)?,
            Command::Copy => {
                self.history.close_group();
                let selected = self.selected_text()?;
                if !selected.is_empty() {
                    clipboard.set_text(selected)?;
                }
            }
            Command::Cut => {
                self.history.close_group();
                let selected = self.selected_text()?;
                if !selected.is_empty() {
                    // Never destroy text when an injected clipboard refuses it.
                    clipboard.set_text(selected)?;
                    self.replace_selections("", EditOrigin::Cut, moment)?;
                }
            }
            Command::Paste => {
                let text = clipboard.get_text()?;
                self.paste_text(&text, moment)?;
            }
            Command::Undo => {
                self.undo()?;
            }
            Command::Redo => {
                self.redo()?;
            }
            Command::Evaluate => {
                self.history.close_group();
                return Ok(vec![EditorEffect::Evaluate {
                    revision: self.document.revision(),
                    source: self.document.snapshot(),
                }]);
            }
            Command::Stop => {
                self.history.close_group();
                return Ok(vec![EditorEffect::Stop]);
            }
        }
        Ok(Vec::new())
    }

    pub fn apply_transaction(
        &mut self,
        mut transaction: Transaction,
        moment: HistoryMoment,
    ) -> Result<(), EditorError> {
        transaction
            .edits
            .sort_by_key(|edit| (edit.range.start, edit.range.end));
        transaction
            .edits
            .retain(|edit| !edit.insert.is_empty() || edit.range.start != edit.range.end);
        self.selections.validate(&self.document)?;
        if transaction.edits.is_empty() {
            if let Some(selection) = transaction.selection_after {
                selection.validate(&self.document)?;
                self.selections = selection;
            }
            return Ok(());
        }
        let before = self.selections.clone();
        let after = transaction
            .selection_after
            .clone()
            .unwrap_or_else(|| before.map_through(&transaction.edits));
        // Rope clones share storage. Applying to a candidate gives a public
        // malformed transaction a true all-or-nothing boundary.
        let mut candidate = self.document.clone();
        let edit_shapes = transaction
            .edits
            .iter()
            .map(document::EditShape::from)
            .collect::<Vec<_>>();
        let entry =
            HistoryEntry::apply(&mut candidate, transaction, before, after.clone(), moment)?;
        after.validate(&candidate)?;
        let mut virtual_rows = self.virtual_rows.clone();
        virtual_rows.map_through_steps(candidate.revision(), std::slice::from_ref(&edit_shapes));
        self.horizontal_layout.borrow_mut().map_revision_after_edit(
            &self.document,
            &candidate,
            &edit_shapes,
        );
        let before_revision = self.document.revision();
        self.document = candidate;
        self.edits_since.record(
            before_revision,
            self.document.revision(),
            std::slice::from_ref(&edit_shapes),
        );
        self.selections = after;
        self.history.record(entry);
        self.virtual_rows = virtual_rows;
        self.drag = None;
        self.ensure_primary_visible();
        Ok(())
    }

    pub fn undo(&mut self) -> Result<bool, EditorError> {
        let before = self.document.revision();
        let Some(change) = self.history.undo(&mut self.document)? else {
            return Ok(false);
        };
        self.edits_since
            .record(before, self.document.revision(), &change.edit_steps);
        self.selections = change.selection;
        self.virtual_rows
            .map_through_steps(self.document.revision(), &change.edit_steps);
        self.horizontal_layout
            .borrow_mut()
            .invalidate(self.document.revision());
        self.drag = None;
        self.ensure_primary_visible();
        Ok(true)
    }

    pub fn redo(&mut self) -> Result<bool, EditorError> {
        let before = self.document.revision();
        let Some(change) = self.history.redo(&mut self.document)? else {
            return Ok(false);
        };
        self.edits_since
            .record(before, self.document.revision(), &change.edit_steps);
        self.selections = change.selection;
        self.virtual_rows
            .map_through_steps(self.document.revision(), &change.edit_steps);
        self.horizontal_layout
            .borrow_mut()
            .invalidate(self.document.revision());
        self.drag = None;
        self.ensure_primary_visible();
        Ok(true)
    }

    fn replace_selections(
        &mut self,
        text: &str,
        origin: EditOrigin,
        moment: HistoryMoment,
    ) -> Result<(), EditorError> {
        let replacements = self
            .selections
            .ranges()
            .iter()
            .map(|selection| (selection.ordered(), text.to_owned()))
            .collect::<Vec<_>>();
        self.replace_ranges(replacements, origin, moment)
    }

    fn paste_text(&mut self, text: &str, moment: HistoryMoment) -> Result<(), EditorError> {
        self.history.close_group();
        let line_ending = self.preferred_line_ending();
        let text = normalize_paste_line_endings(text, &line_ending);
        self.replace_selections(&text, EditOrigin::Paste, moment)
    }

    fn preferred_line_ending(&self) -> String {
        let primary_line = self
            .document
            .line_of(self.selections.primary().head)
            .unwrap_or(0);
        let current = self.document.line_ending(primary_line);
        if !current.is_empty() {
            return current;
        }
        if primary_line > 0 {
            let previous = self.document.line_ending(primary_line - 1);
            if !previous.is_empty() {
                return previous;
            }
        }
        let first = self.document.line_ending(0);
        if !first.is_empty() {
            return first;
        }
        "\n".to_owned()
    }

    fn replace_ranges(
        &mut self,
        replacements: Vec<(std::ops::Range<ByteOffset>, String)>,
        origin: EditOrigin,
        moment: HistoryMoment,
    ) -> Result<(), EditorError> {
        if replacements.is_empty() {
            return Ok(());
        }
        let mut edits = replacements
            .into_iter()
            .map(|(range, insert)| Edit::new(range, insert))
            .collect::<Vec<_>>();
        edits.retain(|edit| !edit.insert.is_empty() || edit.range.start != edit.range.end);
        if edits.is_empty() {
            return Ok(());
        }
        edits.sort_by_key(|edit| (edit.range.start, edit.range.end));
        let mut delta: i64 = 0;
        let mut cursors = Vec::with_capacity(edits.len());
        for edit in &edits {
            let start = add_delta(edit.range.start.0, delta);
            cursors.push(Selection::caret(ByteOffset(start + edit.insert.len())));
            delta += edit.insert.len() as i64
                - (edit.range.end.0.saturating_sub(edit.range.start.0)) as i64;
        }
        let after = SelectionSet::new(cursors, self.selections.len().saturating_sub(1));
        self.apply_transaction(
            Transaction {
                edits,
                selection_after: Some(after),
                origin,
            },
            moment,
        )
    }

    /// Enter. A caret on a line that holds only indentation removes the
    /// indentation and adds no line, so a second Enter puts the caret at
    /// column zero. A selection with text is not a caret: it, and a caret
    /// that it touches, are replaced by a line ending and the indentation
    /// left of the caret. Every other caret is handled the same way.
    fn insert_newline(&mut self, moment: HistoryMoment) -> Result<(), EditorError> {
        let mut replacements: Vec<(std::ops::Range<ByteOffset>, String)> = Vec::new();
        for selection in self.selections.ranges() {
            let line = self.document.line_of(selection.head)?;
            let content = self.document.line_content_range(line);
            if selection.is_empty() && self.caret_clears_indentation(line, &content) {
                // Carets on one line are adjacent in the set and clear it once.
                if replacements.last().map(|(range, _)| range) != Some(&content) {
                    replacements.push((content, String::new()));
                }
                continue;
            }
            let prefix_end = selection.head.0.clamp(content.start.0, content.end.0);
            let prefix = self.document.slice(content.start..ByteOffset(prefix_end))?;
            let indentation = prefix
                .chars()
                .take_while(|character| matches!(character, ' ' | '\t'))
                .collect::<String>();
            let ending = self.document.line_ending(line);
            let ending = if !ending.is_empty() {
                ending
            } else if line > 0 {
                let previous = self.document.line_ending(line - 1);
                if previous.is_empty() {
                    "\n".to_owned()
                } else {
                    previous
                }
            } else {
                "\n".to_owned()
            };
            replacements.push((selection.ordered(), format!("{ending}{indentation}")));
        }
        self.replace_ranges(replacements, EditOrigin::Newline, moment)
    }

    /// Whether Enter at a caret on `line` removes its indentation: the line
    /// holds spaces and tabs only, and no selection with text touches it.
    /// The edit for such a selection would overlap or meet the removal.
    fn caret_clears_indentation(&self, line: usize, content: &std::ops::Range<ByteOffset>) -> bool {
        let text = self.document.line_content(line);
        !text.is_empty()
            && text.bytes().all(|byte| matches!(byte, b' ' | b'\t'))
            && !self.selections.ranges().iter().any(|other| {
                let other = other.ordered();
                other.start < other.end && other.start <= content.end && other.end >= content.start
            })
    }

    fn indent(&mut self, moment: HistoryMoment) -> Result<(), EditorError> {
        if self.selections.len() == 1 && self.selections.primary().is_empty() {
            let selection = self.selections.primary();
            let line = self.document.line_of(selection.head)?;
            let column = display_column_of_offset_cached(
                &self.document,
                line,
                selection.head,
                self.viewport.tab_width,
                &mut self.horizontal_layout.borrow_mut(),
            );
            let tab = usize::from(self.viewport.tab_width.max(1));
            let spaces = tab - (column % tab);
            return self.replace_selections(&" ".repeat(spaces), EditOrigin::Indent, moment);
        }
        let lines = self.selected_lines()?;
        let edits = lines
            .into_iter()
            .map(|line| {
                (
                    self.document.line_start(line)..self.document.line_start(line),
                    " ".repeat(usize::from(self.viewport.tab_width)),
                )
            })
            .collect::<Vec<_>>();
        let raw_edits = edits
            .iter()
            .map(|(range, insert)| Edit::new(range.clone(), insert.clone()))
            .collect::<Vec<_>>();
        let after = self.selections.map_through(&raw_edits);
        self.apply_transaction(
            Transaction {
                edits: raw_edits,
                selection_after: Some(after),
                origin: EditOrigin::Indent,
            },
            moment,
        )
    }

    fn outdent(&mut self, moment: HistoryMoment) -> Result<(), EditorError> {
        let lines = self.selected_lines()?;
        let tab_width = usize::from(self.viewport.tab_width.max(1));
        let mut edits = Vec::new();
        for line in lines {
            let content = self.document.line_content(line);
            let remove = if content.starts_with('\t') {
                1
            } else {
                content
                    .bytes()
                    .take(tab_width)
                    .take_while(|byte| *byte == b' ')
                    .count()
            };
            if remove > 0 {
                let start = self.document.line_start(line);
                edits.push(Edit::new(
                    start..ByteOffset(start.0 + remove),
                    String::new(),
                ));
            }
        }
        if edits.is_empty() {
            return Ok(());
        }
        let after = self.selections.map_through(&edits);
        self.apply_transaction(
            Transaction {
                edits,
                selection_after: Some(after),
                origin: EditOrigin::Indent,
            },
            moment,
        )
    }

    /// Line comments over every line the selections touch.
    ///
    /// The gesture is a toggle: if every non-blank line in the range is
    /// already commented the markers come off, otherwise they go on at the
    /// shallowest indentation in the range so a block stays aligned.
    fn toggle_comment(&mut self, moment: HistoryMoment) -> Result<(), EditorError> {
        let lines = self.selected_lines()?;
        let mut content = Vec::with_capacity(lines.len());
        for line in lines {
            let start = self.document.line_start(line);
            let text = self.document.line_content(line);
            let indent = text
                .bytes()
                .take_while(|byte| matches!(byte, b' ' | b'\t'))
                .count();
            content.push((start, text, indent));
        }
        let substantive = content
            .iter()
            .filter(|(_, text, indent)| *indent < text.len())
            .collect::<Vec<_>>();
        if substantive.is_empty() {
            return Ok(());
        }
        let all_commented = substantive
            .iter()
            .all(|(_, text, indent)| text[*indent..].starts_with(COMMENT_MARKER));

        let mut edits = Vec::new();
        if all_commented {
            for (start, text, indent) in substantive {
                let rest = &text[*indent..];
                // Remove the marker and the single space a commenting pass
                // adds, but never a second space the author typed.
                let removed = if rest.starts_with(COMMENT_PREFIX) {
                    COMMENT_PREFIX.len()
                } else {
                    COMMENT_MARKER.len()
                };
                let from = ByteOffset(start.0 + indent);
                edits.push(Edit::new(from..ByteOffset(from.0 + removed), String::new()));
            }
        } else {
            let column = substantive
                .iter()
                .map(|(_, _, indent)| *indent)
                .min()
                .unwrap_or(0);
            for (start, _, _) in substantive {
                let at = ByteOffset(start.0 + column);
                edits.push(Edit::new(at..at, COMMENT_PREFIX));
            }
        }
        if edits.is_empty() {
            return Ok(());
        }
        edits.sort_by_key(|edit| (edit.range.start, edit.range.end));
        let after = self.selections.map_through(&edits);
        self.apply_transaction(
            Transaction {
                edits,
                selection_after: Some(after),
                origin: EditOrigin::Indent,
            },
            moment,
        )
    }

    fn selected_lines(&self) -> Result<Vec<usize>, EditorError> {
        let mut lines = BTreeSet::new();
        for selection in self.selections.ranges() {
            let ordered = selection.ordered();
            let start_line = self.document.line_of(ordered.start)?;
            let mut end_line = self.document.line_of(ordered.end)?;
            if ordered.end > ordered.start
                && end_line > start_line
                && ordered.end == self.document.line_start(end_line)
            {
                end_line -= 1;
            }
            lines.extend(start_line..=end_line);
        }
        Ok(lines.into_iter().collect())
    }

    fn delete(
        &mut self,
        forward: bool,
        by_group: bool,
        moment: HistoryMoment,
    ) -> Result<(), EditorError> {
        let mut replacements = Vec::new();
        for selection in self.selections.ranges() {
            let range = if !selection.is_empty() {
                selection.ordered()
            } else if forward {
                let end = if by_group {
                    self.document.next_group_boundary(selection.head)?
                } else {
                    self.document.next_grapheme_boundary(selection.head)?
                };
                selection.head..end
            } else {
                let start = if by_group {
                    self.document.previous_group_boundary(selection.head)?
                } else {
                    self.document.previous_grapheme_boundary(selection.head)?
                };
                start..selection.head
            };
            replacements.push((range, String::new()));
        }
        let origin = if forward {
            EditOrigin::Delete
        } else {
            EditOrigin::Backspace
        };
        self.replace_ranges(replacements, origin, moment)
    }

    fn move_selection(&mut self, motion: Motion, extend: bool) -> Result<(), EditorError> {
        self.history.close_group();
        self.drag = None;
        let selection = self.selections.primary();
        if !extend && !selection.is_empty() {
            match motion {
                Motion::Left | Motion::GroupLeft | Motion::LineStart | Motion::DocumentStart => {
                    self.selections
                        .set_single(Selection::caret(selection.ordered().start));
                    self.ensure_primary_visible();
                    return Ok(());
                }
                Motion::Right | Motion::GroupRight | Motion::LineEnd | Motion::DocumentEnd => {
                    self.selections
                        .set_single(Selection::caret(selection.ordered().end));
                    self.ensure_primary_visible();
                    return Ok(());
                }
                _ => {}
            }
        }
        let (head, goal) = self.motion_target(selection, motion)?;
        let moved = if extend {
            Selection {
                anchor: selection.anchor,
                head,
                goal_column: goal,
            }
        } else {
            Selection {
                anchor: head,
                head,
                goal_column: goal,
            }
        };
        self.selections.set_single(moved);
        // Paging moves the viewport as well as the caret. Merely revealing
        // the destination scrolls by just one row when starting at the top.
        match motion {
            Motion::PageUp => {
                self.viewport.top_row = self
                    .viewport
                    .top_row
                    .saturating_sub(self.viewport.page_rows);
            }
            Motion::PageDown => {
                self.viewport.top_row = self
                    .viewport
                    .top_row
                    .saturating_add(self.viewport.page_rows)
                    .min(self.maximum_top_row());
            }
            _ => {}
        }
        self.ensure_primary_visible();
        Ok(())
    }

    fn motion_target(
        &self,
        selection: Selection,
        motion: Motion,
    ) -> Result<(ByteOffset, Option<usize>), EditorError> {
        let head = selection.head;
        let result = match motion {
            Motion::Left => (self.document.previous_grapheme_boundary(head)?, None),
            Motion::Right => (self.document.next_grapheme_boundary(head)?, None),
            Motion::GroupLeft => (self.document.previous_group_boundary(head)?, None),
            Motion::GroupRight => (self.document.next_group_boundary(head)?, None),
            Motion::DocumentStart => (ByteOffset::ZERO, None),
            Motion::DocumentEnd => (ByteOffset(self.document.len_bytes()), None),
            Motion::LineStart => {
                let line = self.document.line_of(head)?;
                let line_start = self.document.line_start(line);
                let first_text = self.document.first_non_whitespace(line);
                (
                    (if head == first_text {
                        line_start
                    } else {
                        first_text
                    }),
                    None,
                )
            }
            Motion::LineEnd => {
                let line = self.document.line_of(head)?;
                (self.document.line_content_range(line).end, None)
            }
            Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown => {
                let line = self.document.line_of(head)?;
                let distance = match motion {
                    Motion::PageUp | Motion::PageDown => self.viewport.page_rows.max(1),
                    _ => 1,
                };
                let upwards = matches!(motion, Motion::Up | Motion::PageUp);
                let paged_target =
                    if matches!(motion, Motion::PageUp | Motion::PageDown) {
                        let column = display_column_of_offset_cached(
                            &self.document,
                            line,
                            head,
                            self.viewport.tab_width,
                            &mut self.horizontal_layout.borrow_mut(),
                        );
                        let segment = self
                            .wrap_index()
                            .map_or(0, |wrap| wrap.segment_of(line, column));
                        let row = self.global_row_for_line(line).saturating_add(segment);
                        Some(self.text_position_at_screen_row(
                            self.page_motion_row(row, upwards),
                            upwards,
                        ))
                    } else {
                        None
                    };
                if let Some(wrap) = self.wrap_index() {
                    // Wrapped, the caret moves by row and straight down the
                    // screen: the goal is a screen column, kept from row to
                    // row, so a row set in under its line starts under a
                    // caret that was left of its indent.
                    let column = display_column_of_offset_cached(
                        &self.document,
                        line,
                        head,
                        self.viewport.tab_width,
                        &mut self.horizontal_layout.borrow_mut(),
                    );
                    let segment = wrap.segment_of(line, column);
                    let goal = selection.goal_column.unwrap_or(
                        column
                            .saturating_sub(wrap.start_of(line, segment).column)
                            .saturating_add(usize::from(wrap.hanging_at(line, segment))),
                    );
                    let row = wrap.rows_before(line).saturating_add(segment);
                    let target_row = if upwards {
                        row.saturating_sub(distance)
                    } else {
                        row.saturating_add(distance)
                            .min(wrap.total_rows().saturating_sub(1))
                    };
                    let (target_line, target_segment) =
                        paged_target.unwrap_or_else(|| wrap.locate(target_row));
                    // A row that draws no caret position of its own is
                    // passed in the direction of travel; a line's first
                    // and last rows always draw one.
                    let mut earlier = (0..=target_segment).rev();
                    let mut later = target_segment..wrap.rows_of(target_line);
                    let passed: &mut dyn Iterator<Item = usize> =
                        if upwards { &mut earlier } else { &mut later };
                    let target = passed
                        .map(|segment| self.wrapped_row_position(&wrap, target_line, segment, goal))
                        .find_map(Result::transpose)
                        .transpose()?
                        .unwrap_or(head);
                    return Ok((target, Some(goal)));
                }
                let goal = match selection.goal_column {
                    Some(goal) => goal,
                    None => display_column_of_offset_cached(
                        &self.document,
                        line,
                        head,
                        self.viewport.tab_width,
                        &mut self.horizontal_layout.borrow_mut(),
                    ),
                };
                let target_line = if let Some((target_line, _)) = paged_target {
                    target_line
                } else if upwards {
                    line.saturating_sub(distance)
                } else {
                    line.saturating_add(distance)
                        .min(self.document.line_count().saturating_sub(1))
                };
                (
                    offset_at_display_column_cached(
                        &self.document,
                        target_line,
                        goal,
                        self.viewport.tab_width,
                        &mut self.horizontal_layout.borrow_mut(),
                    ),
                    Some(goal),
                )
            }
        };
        Ok(result)
    }

    /// The caret position drawn on row `segment` of wrapped `line` nearest
    /// screen column `goal`, or `None` when the row draws none of its own,
    /// as a slider pill alone on a row does.
    fn wrapped_row_position(
        &self,
        wrap: &WrapIndex,
        line: usize,
        segment: usize,
        goal: usize,
    ) -> Result<Option<ByteOffset>, EditorError> {
        let drawn_on = |offset| {
            let column = display_column_of_offset_cached(
                &self.document,
                line,
                offset,
                self.viewport.tab_width,
                &mut self.horizontal_layout.borrow_mut(),
            );
            wrap.segment_of(line, column)
        };
        // Not past the row's last caret position: the next row's start
        // only while it is drawn on this row.
        let end = wrap.end_of(line, segment).map_or(usize::MAX, |next| {
            if wrap.segment_of(line, next) == segment {
                next
            } else {
                next.saturating_sub(1)
            }
        });
        let within = goal.saturating_sub(usize::from(wrap.hanging_at(line, segment)));
        let column = wrap
            .start_of(line, segment)
            .column
            .saturating_add(within)
            .min(end);
        let target = offset_at_display_column_cached(
            &self.document,
            line,
            column,
            self.viewport.tab_width,
            &mut self.horizontal_layout.borrow_mut(),
        );
        let drawn = drawn_on(target);
        if drawn == segment {
            return Ok(Some(target));
        }
        // The row's start drawn on the row above, or a wide cluster's end
        // drawn on the row below: the grapheme boundary toward this row is
        // the nearest position it draws, if it draws any.
        let inward = if drawn < segment {
            self.document.next_grapheme_boundary(target)?
        } else {
            self.document.previous_grapheme_boundary(target)?
        };
        Ok((drawn_on(inward) == segment).then_some(inward))
    }

    fn page_motion_row(&self, caret_row: usize, upwards: bool) -> usize {
        let page = self.viewport.page_rows.max(1);
        // A wheel-scrolled view may no longer contain the caret. Page from
        // the visible edge in that case rather than jumping back to it.
        let row = caret_row.clamp(
            self.viewport.top_row,
            self.viewport.top_row.saturating_add(page.saturating_sub(1)),
        );
        if upwards {
            row.saturating_sub(page)
        } else {
            row.saturating_add(page)
                .min(self.total_rows().saturating_sub(1))
        }
    }

    /// Resolve a rendered row to editable text. Widget rows occupy page
    /// space but cannot hold the caret, so pass them in the travel direction.
    fn text_position_at_screen_row(&self, row: usize, upwards: bool) -> (usize, usize) {
        let mut low = 0;
        let mut high = self.document.line_count();
        while low + 1 < high {
            let middle = low + (high - low) / 2;
            if self.global_row_for_line(middle) <= row {
                low = middle;
            } else {
                high = middle;
            }
        }
        let segment = row.saturating_sub(self.global_row_for_line(low));
        let text_rows = self.wrap_index().map_or(1, |wrap| wrap.rows_of(low));
        if segment < text_rows {
            (low, segment)
        } else if !upwards && low + 1 < self.document.line_count() {
            (low + 1, 0)
        } else {
            (low, text_rows.saturating_sub(1))
        }
    }

    fn ensure_primary_visible(&mut self) -> usize {
        let head = self.selections.primary().head;
        let line = self.document.line_of(head).unwrap_or(0);
        let column = display_column_of_offset_cached(
            &self.document,
            line,
            head,
            self.viewport.tab_width,
            &mut self.horizontal_layout.borrow_mut(),
        );
        // Wrapped, the caret's row is one of its line's, and nothing
        // scrolls sideways.
        let segment = self
            .wrap_index()
            .map_or(0, |wrap| wrap.segment_of(line, column));
        let global = self.global_row_for_line(line).saturating_add(segment);
        let page = self.viewport.page_rows.max(1);
        if global < self.viewport.top_row {
            self.viewport.top_row = global;
        } else if global >= self.viewport.top_row.saturating_add(page) {
            self.viewport.top_row = global.saturating_add(1).saturating_sub(page);
        }
        if self.viewport.wrap {
            self.viewport.left_column = 0;
            return column;
        }
        if column < self.viewport.left_column {
            self.viewport.left_column = column;
        } else {
            let width = self.viewport.page_columns.max(1);
            if column >= self.viewport.left_column.saturating_add(width) {
                self.viewport.left_column = column.saturating_add(1).saturating_sub(width);
            }
        }
        column
    }

    fn restore_horizontal_context(&mut self, primary_column: usize) {
        if self.viewport.wrap {
            self.viewport.left_column = 0;
            return;
        }
        let earliest = primary_column
            .saturating_add(1)
            .saturating_sub(self.viewport.page_columns.max(1));
        self.viewport.left_column = self.viewport.left_column.min(earliest);
    }

    /// Where the page sits in the text, in rows: the first row on screen,
    /// the rows a page holds, and the rows the text has, inline widgets
    /// included. What a scrollbar draws.
    pub fn scroll_extent(&self) -> (usize, usize, usize) {
        (
            self.viewport.top_row,
            self.viewport.page_rows,
            self.total_rows(),
        )
    }

    /// Screen row (inline widgets included) of a document line.
    pub fn screen_row_of_line(&self, line: usize) -> usize {
        self.global_row_for_line(line)
    }

    fn global_row_for_line(&self, line: usize) -> usize {
        let inserted = self
            .virtual_rows
            .rows()
            .iter()
            .filter_map(|row| {
                self.document
                    .line_of(row.after)
                    .ok()
                    .filter(|anchor| *anchor < line)
                    .map(|_| usize::from(row.height))
            })
            .sum::<usize>();
        let text_rows = self
            .wrap_index()
            .map_or(line, |wrap| wrap.rows_before(line));
        text_rows.saturating_add(inserted)
    }

    /// Number of screen rows the document occupies, inline widgets included.
    fn total_rows(&self) -> usize {
        let text_rows = self
            .wrap_index()
            .map_or(self.document.line_count(), |wrap| wrap.total_rows());
        text_rows.saturating_add(
            self.virtual_rows
                .rows()
                .iter()
                .map(|row| usize::from(row.height))
                .sum::<usize>(),
        )
    }

    /// Number of rows occupied in the current viewport, including wrapped
    /// continuations and inline widgets.
    pub(crate) fn visual_row_count(&self) -> usize {
        self.total_rows()
    }

    /// The furthest the viewport may scroll. The last row is allowed to
    /// travel all the way to the top of the pane, as in every graphical code
    /// editor: pinning the end of the score to the bottom edge makes the last
    /// lines feel boxed in while they are being written.
    fn maximum_top_row(&self) -> usize {
        self.total_rows().saturating_sub(1)
    }

    fn clamp_viewport(&mut self) {
        self.viewport.top_row = self.viewport.top_row.min(self.maximum_top_row());
        if !self.viewport.wrap && self.viewport.left_column > 0 {
            let maximum = self
                .horizontal_scroll_extent()
                .map_or(0, |(_, page, total)| total.saturating_sub(page));
            self.viewport.left_column = self.viewport.left_column.min(maximum);
        }
    }

    /// Forget a mouse drag without extending it. A drag ends on its own
    /// release; this is for one whose release never came (let go outside
    /// the window, or taken by another surface on its way) once a new
    /// press has begun a gesture of its own.
    pub fn cancel_drag(&mut self) {
        self.drag = None;
    }

    /// Route one Crossterm mouse event through the exact screen map used to
    /// draw the preceding frame.
    pub fn mouse_event(
        &mut self,
        event: MouseEvent,
        map: &ScreenMap,
        moment: HistoryMoment,
    ) -> Result<bool, EditorError> {
        if !map.is_current_for(&self.document, self.viewport) {
            return Err(EditorError::StaleScreenMap {
                map_revision: map.revision(),
                document_revision: self.document.revision(),
                map_viewport: map.viewport(),
                editor_viewport: self.viewport,
            });
        }
        let point = CellPoint::new(event.column, event.row);
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // A fresh click belongs to the editor whenever it lands in
                // the source grid - the empty rows under a short score
                // included, where it lands on the last line the way any
                // editor's blank half behaves. A click on the stage or
                // chrome still must not teleport the caret.
                let Some(hit) = map.hit_test_within(point) else {
                    return Ok(false);
                };
                self.history.close_group();
                let offset = hit.selection_offset();
                // Shift+click moves the head and keeps the anchor, so the
                // selection grows, shrinks or inverts. It is never part of
                // a double click: a second one near the first must not snap
                // the head to a word on release.
                let (selection, granularity) = if click_extends(event.modifiers) {
                    self.last_click = None;
                    (
                        Selection::range(self.selections.primary().anchor, offset),
                        DragGranularity::Grapheme,
                    )
                } else {
                    let granularity = match self.click_count(point, moment) {
                        1 => DragGranularity::Grapheme,
                        2 => DragGranularity::Word,
                        _ => DragGranularity::Line,
                    };
                    (self.selection_at(offset, granularity)?, granularity)
                };
                self.selections.set_single(selection);
                self.drag = Some(DragState {
                    anchor: selection,
                    granularity,
                    last_point: point,
                });
                Ok(true)
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.drag.is_none() {
                    return Ok(false);
                }
                // A drag held past the grid's edge scrolls the viewport
                // after the selection, the way every editor does: each
                // drag event past the edge pulls one step further.
                let area = map.area();
                let mut scrolled = false;
                if point.y < area.y && self.viewport.top_row > 0 {
                    self.viewport.top_row = self.viewport.top_row.saturating_sub(1);
                    scrolled = true;
                } else if point.y >= area.y.saturating_add(area.height)
                    && self.viewport.top_row < self.maximum_top_row()
                {
                    self.viewport.top_row += 1;
                    scrolled = true;
                }
                if self.viewport.wrap {
                    // Nothing to scroll sideways to.
                } else if point.x < area.x && self.viewport.left_column > 0 {
                    self.viewport.left_column = self.viewport.left_column.saturating_sub(4);
                    scrolled = true;
                } else if point.x >= area.x.saturating_add(area.width) {
                    self.viewport.left_column = self.viewport.left_column.saturating_add(4);
                    scrolled = true;
                }
                if scrolled {
                    self.continue_drag_after_scroll(point, area)?;
                } else {
                    self.extend_drag(point, map)?;
                }
                Ok(true)
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if self.drag.is_some() {
                    self.extend_drag(point, map)?;
                    self.drag = None;
                    return Ok(true);
                }
                Ok(false)
            }
            MouseEventKind::ScrollUp => {
                self.viewport.top_row =
                    self.viewport.top_row.saturating_sub(self.config.wheel_rows);
                self.continue_drag_after_scroll(point, map.area())?;
                Ok(true)
            }
            MouseEventKind::ScrollDown => {
                self.viewport.top_row = self
                    .viewport
                    .top_row
                    .saturating_add(self.config.wheel_rows)
                    .min(self.maximum_top_row());
                self.continue_drag_after_scroll(point, map.area())?;
                Ok(true)
            }
            MouseEventKind::ScrollLeft => {
                if self.viewport.wrap {
                    return Ok(false);
                }
                self.viewport.left_column = self.viewport.left_column.saturating_sub(4);
                Ok(true)
            }
            MouseEventKind::ScrollRight => {
                if self.viewport.wrap {
                    return Ok(false);
                }
                self.viewport.left_column = self.viewport.left_column.saturating_add(4);
                Ok(true)
            }
            MouseEventKind::Moved
            | MouseEventKind::Down(_)
            | MouseEventKind::Up(_)
            | MouseEventKind::Drag(_) => Ok(false),
        }
    }

    fn click_count(&mut self, point: CellPoint, moment: HistoryMoment) -> u8 {
        let count = self
            .last_click
            .filter(|last| {
                moment.0.saturating_sub(last.moment.0) <= MULTI_CLICK_WINDOW_MS
                    && last.point.x.abs_diff(point.x) <= 1
                    && last.point.y.abs_diff(point.y) <= 1
            })
            .map(|last| (last.count % 3) + 1)
            .unwrap_or(1);
        self.last_click = Some(LastClick {
            point,
            moment,
            count,
        });
        count
    }

    fn selection_at(
        &self,
        offset: ByteOffset,
        granularity: DragGranularity,
    ) -> Result<Selection, EditorError> {
        Ok(match granularity {
            DragGranularity::Grapheme => Selection::caret(offset),
            DragGranularity::Word => {
                let word = self.document.word_range(offset)?;
                Selection::range(word.start, word.end)
            }
            DragGranularity::Line => {
                let line = self.document.line_of(offset)?;
                let start = self.document.line_start(line);
                let end = if line + 1 < self.document.line_count() {
                    self.document.line_start(line + 1)
                } else {
                    ByteOffset(self.document.len_bytes())
                };
                Selection::range(start, end)
            }
        })
    }

    fn extend_drag(&mut self, point: CellPoint, map: &ScreenMap) -> Result<(), EditorError> {
        let Some(hit) = map.hit_test_clamped(point) else {
            return Ok(());
        };
        let offset = hit.selection_offset();
        let (anchor, granularity) = self
            .drag
            .as_ref()
            .map(|drag| (drag.anchor, drag.granularity))
            .expect("checked by caller");
        self.drag.as_mut().expect("checked by caller").last_point = point;
        let selection = match granularity {
            DragGranularity::Grapheme => Selection::range(anchor.anchor, offset),
            DragGranularity::Word => {
                let word = self.document.word_range(offset)?;
                if offset >= anchor.ordered().start {
                    Selection::range(anchor.ordered().start, word.end)
                } else {
                    Selection::range(anchor.ordered().end, word.start)
                }
            }
            DragGranularity::Line => {
                let line_selection = self.selection_at(offset, DragGranularity::Line)?;
                if offset >= anchor.ordered().start {
                    Selection::range(anchor.ordered().start, line_selection.ordered().end)
                } else {
                    Selection::range(anchor.ordered().end, line_selection.ordered().start)
                }
            }
        };
        self.selections.set_single(selection);
        Ok(())
    }

    fn continue_drag_after_scroll(
        &mut self,
        event_point: CellPoint,
        area: GridRect,
    ) -> Result<(), EditorError> {
        let Some(last_point) = self.drag.as_ref().map(|drag| drag.last_point) else {
            return Ok(());
        };
        let refreshed = self.screen_map(area)?;
        // A wheel event's coordinates are usually the pointer coordinates. If
        // a backend reports zero, retain the last drag location.
        let point = if event_point == CellPoint::default() {
            last_point
        } else {
            event_point
        };
        self.extend_drag(point, &refreshed)
    }
}

fn add_delta(value: usize, delta: i64) -> usize {
    if delta >= 0 {
        value.saturating_add(delta as usize)
    } else {
        value.saturating_sub(delta.unsigned_abs() as usize)
    }
}

fn normalize_paste_line_endings(text: &str, line_ending: &str) -> String {
    if !text.contains('\r') && (line_ending == "\n" || !text.contains('\n')) {
        return text.to_owned();
    }
    let mut normalized = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\r' => {
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                normalized.push_str(line_ending);
            }
            '\n' => normalized.push_str(line_ending),
            _ => normalized.push(character),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn dispatch(editor: &mut Editor, command: Command, at: u64) {
        editor
            .dispatch(command, HistoryMoment(at), &mut MemoryClipboard::default())
            .unwrap();
    }

    /// Up and Down from a caret drawn at the end of a wrapped row move it
    /// one displayed row, to the target row's caret position nearest its
    /// screen column.
    #[test]
    fn wrapped_vertical_motion_from_a_row_boundary_moves_the_drawn_row() {
        let mut editor = Editor::new("abcdefghij klmnopqrst uvwxyz\nsecond\n").unwrap();
        editor.set_view_size(11, 6);
        editor.set_wrap(true);
        let area = GridRect::new(0, 0, 11, 6);
        let drawn = |editor: &Editor| {
            editor
                .screen_map(area)
                .unwrap()
                .cell_for_offset(editor.primary_selection().head)
        };
        let up = Command::Move {
            motion: Motion::Up,
            extend: false,
        };
        let down = Command::Move {
            motion: Motion::Down,
            extend: false,
        };
        // Row two's start, drawn in the cell past row one's last character.
        editor
            .set_selection(Selection::caret(ByteOffset(17)))
            .unwrap();
        assert_eq!(drawn(&editor), Some(CellPoint::new(10, 1)));
        dispatch(&mut editor, up, 0);
        assert_eq!(drawn(&editor), Some(CellPoint::new(10, 0)), "up one row");
        dispatch(&mut editor, down.clone(), 1);
        assert_eq!(
            drawn(&editor),
            Some(CellPoint::new(10, 1)),
            "down one row, back onto the boundary"
        );
        dispatch(&mut editor, down.clone(), 2);
        assert_eq!(
            drawn(&editor),
            Some(CellPoint::new(9, 2)),
            "down one row, to its end: row three's start, after the space"
        );
        assert_eq!(editor.primary_selection().head, ByteOffset(22));
        dispatch(&mut editor, down, 3);
        assert_eq!(
            drawn(&editor),
            Some(CellPoint::new(10, 3)),
            "down one row, not two"
        );
    }

    /// Up and Down pass a wrapped row that draws no caret position of its
    /// own, a slider pill alone on it, and keep the goal column on the row
    /// they reach.
    #[test]
    fn wrapped_vertical_motion_passes_a_row_without_a_caret_position() {
        let mut editor = Editor::new("abcdefgh(0123456789").unwrap();
        editor.set_inline_widths(vec![InlineWidth {
            at: ByteOffset(8),
            extra: 13,
        }]);
        editor.set_view_size(16, 10);
        editor.set_wrap(true);
        let area = GridRect::new(0, 0, 16, 10);
        let drawn = |editor: &Editor| {
            editor
                .screen_map(area)
                .unwrap()
                .cell_for_offset(editor.primary_selection().head)
        };
        let up = Command::Move {
            motion: Motion::Up,
            extend: false,
        };
        let down = Command::Move {
            motion: Motion::Down,
            extend: false,
        };
        // The pill's start is drawn at the end of row zero and its end at the
        // start of row two.
        editor
            .set_selection(Selection::caret(ByteOffset(8)))
            .unwrap();
        assert_eq!(drawn(&editor), Some(CellPoint::new(8, 0)));
        editor
            .set_selection(Selection::caret(ByteOffset(9)))
            .unwrap();
        assert_eq!(drawn(&editor), Some(CellPoint::new(4, 2)));

        dispatch(&mut editor, up.clone(), 0);
        assert_eq!(
            drawn(&editor),
            Some(CellPoint::new(4, 0)),
            "up past the pill"
        );
        dispatch(&mut editor, down.clone(), 1);
        assert_eq!(drawn(&editor), Some(CellPoint::new(4, 2)), "down past it");
        dispatch(&mut editor, up.clone(), 2);
        assert_eq!(drawn(&editor), Some(CellPoint::new(4, 0)), "and up again");

        // A goal right of the pill's middle, which rounds onto row two.
        editor
            .set_selection(Selection::caret(ByteOffset(18)))
            .unwrap();
        assert_eq!(drawn(&editor), Some(CellPoint::new(13, 2)));
        dispatch(&mut editor, up, 3);
        assert_eq!(
            drawn(&editor),
            Some(CellPoint::new(8, 0)),
            "up to row zero's end"
        );
        dispatch(&mut editor, down, 4);
        assert_eq!(
            drawn(&editor),
            Some(CellPoint::new(13, 2)),
            "down to the goal"
        );
    }

    #[test]
    fn typing_selection_paste_and_history_are_normal_editor_transactions() {
        let mut editor = Editor::new("hello world").unwrap();
        editor
            .set_selection(Selection::range(ByteOffset(6), ByteOffset(11)))
            .unwrap();
        dispatch(&mut editor, Command::InsertText("there".into()), 0);
        dispatch(&mut editor, Command::InsertText("!".into()), 100);
        assert_eq!(editor.source(), "hello there!");
        assert_eq!(editor.undo_depth(), 1);
        dispatch(&mut editor, Command::Undo, 200);
        assert_eq!(editor.source(), "hello world");
        assert_eq!(
            editor.primary_selection(),
            Selection::range(ByteOffset(6), ByteOffset(11))
        );
        dispatch(&mut editor, Command::Redo, 300);
        assert_eq!(editor.source(), "hello there!");

        dispatch(&mut editor, Command::PasteText("\n🥁".into()), 400);
        assert_eq!(editor.undo_depth(), 2);
        dispatch(&mut editor, Command::Undo, 500);
        assert_eq!(editor.source(), "hello there!");
    }

    #[test]
    fn paste_preserves_indentation_and_normalizes_line_endings_once() {
        let mut editor = Editor::new("head\r\n").unwrap();
        editor
            .set_selection(Selection::caret(ByteOffset("head\r\n".len())))
            .unwrap();
        dispatch(
            &mut editor,
            Command::PasteText("first\r\n  second\rthird\n    fourth".into()),
            0,
        );
        assert_eq!(
            editor.source(),
            "head\r\nfirst\r\n  second\r\nthird\r\n    fourth"
        );
        assert_eq!(editor.undo_depth(), 1);
        dispatch(&mut editor, Command::Undo, 1);
        assert_eq!(editor.source(), "head\r\n");
    }

    #[test]
    fn a_manually_pressed_newline_still_autoindents() {
        let mut editor = Editor::new("    pattern").unwrap();
        editor
            .set_selection(Selection::caret(ByteOffset("    pattern".len())))
            .unwrap();
        dispatch(&mut editor, Command::Newline, 0);
        assert_eq!(editor.source(), "    pattern\n    ");
    }

    /// The second Enter leaves the chain: it removes the indentation the
    /// first one added, and adds no line.
    #[test]
    fn enter_on_a_line_of_only_indentation_clears_it_and_adds_no_line() {
        let chain = "$: s(\"piano\")\n    .lpf(1000)";
        let mut editor = Editor::new(chain).unwrap();
        editor
            .set_selection(Selection::caret(ByteOffset(chain.len())))
            .unwrap();
        dispatch(&mut editor, Command::Newline, 0);
        assert_eq!(editor.source(), format!("{chain}\n    "));

        dispatch(&mut editor, Command::Newline, 1);
        assert_eq!(editor.source(), format!("{chain}\n"));
        assert_eq!(
            editor.selections().ranges(),
            &[Selection::caret(ByteOffset(chain.len() + 1))]
        );

        // The line is empty now, so the third Enter adds a line.
        dispatch(&mut editor, Command::Newline, 2);
        assert_eq!(editor.source(), format!("{chain}\n\n"));
        assert_eq!(
            editor.primary_selection(),
            Selection::caret(ByteOffset(chain.len() + 2))
        );
    }

    /// The caret can be anywhere in the indentation, which can hold tabs.
    #[test]
    fn enter_clears_a_line_of_indentation_from_any_caret_in_it() {
        for caret in 2..=5 {
            let mut editor = Editor::new("a\n \t \nb").unwrap();
            editor
                .set_selection(Selection::caret(ByteOffset(caret)))
                .unwrap();
            dispatch(&mut editor, Command::Newline, 0);
            assert_eq!(editor.source(), "a\n\nb", "caret at {caret}");
            assert_eq!(
                editor.primary_selection(),
                Selection::caret(ByteOffset(2)),
                "caret at {caret}"
            );
        }
    }

    #[test]
    fn clearing_a_line_of_indentation_keeps_its_crlf_ending() {
        let mut editor = Editor::new("a\r\n    \r\nb").unwrap();
        editor
            .set_selection(Selection::caret(ByteOffset(7)))
            .unwrap();
        dispatch(&mut editor, Command::Newline, 0);
        assert_eq!(editor.source(), "a\r\n\r\nb");
        assert_eq!(editor.primary_selection(), Selection::caret(ByteOffset(3)));
    }

    #[test]
    fn one_undo_brings_cleared_indentation_back_and_redo_clears_it_again() {
        let mut editor = Editor::new("    a").unwrap();
        editor
            .set_selection(Selection::caret(ByteOffset(5)))
            .unwrap();
        dispatch(&mut editor, Command::Newline, 0);
        dispatch(&mut editor, Command::Newline, 1);
        assert_eq!(editor.source(), "    a\n");
        assert_eq!(editor.undo_depth(), 2);

        dispatch(&mut editor, Command::Undo, 2);
        assert_eq!(editor.source(), "    a\n    ");
        assert_eq!(editor.primary_selection(), Selection::caret(ByteOffset(10)));

        dispatch(&mut editor, Command::Redo, 3);
        assert_eq!(editor.source(), "    a\n");
        assert_eq!(editor.primary_selection(), Selection::caret(ByteOffset(6)));
    }

    /// Each caret follows its own line. Two carets on one line of
    /// indentation clear it once and become one caret.
    #[test]
    fn enter_with_several_carets_treats_each_caret_by_its_own_line() {
        let mut editor = Editor::new("a\n    \nb\n  ").unwrap();
        editor.selections = SelectionSet::new(
            [1, 4, 6, 11]
                .map(|offset| Selection::caret(ByteOffset(offset)))
                .to_vec(),
            0,
        );
        dispatch(&mut editor, Command::Newline, 0);
        assert_eq!(editor.source(), "a\n\n\nb\n");
        assert_eq!(
            editor.selections().ranges(),
            &[2, 3, 6].map(|offset| Selection::caret(ByteOffset(offset)))
        );

        dispatch(&mut editor, Command::Undo, 1);
        assert_eq!(editor.source(), "a\n    \nb\n  ");
    }

    /// Enter replaces a selection that holds text, also on a line of
    /// indentation. A caret on a line that such a selection touches does
    /// not clear it, because the two edits would overlap.
    #[test]
    fn enter_replaces_a_selection_on_a_line_of_indentation_as_before() {
        let mut editor = Editor::new("a\n    ").unwrap();
        editor
            .set_selection(Selection::range(ByteOffset(3), ByteOffset(5)))
            .unwrap();
        dispatch(&mut editor, Command::Newline, 0);
        assert_eq!(editor.source(), "a\n \n    ");

        let mut editor = Editor::new("a\n    ").unwrap();
        editor.selections = SelectionSet::new(
            vec![
                Selection::range(ByteOffset(3), ByteOffset(5)),
                Selection::caret(ByteOffset(6)),
            ],
            0,
        );
        dispatch(&mut editor, Command::Newline, 0);
        assert_eq!(editor.source(), "a\n \n    \n    ");
    }

    /// A selection that ends where the indentation starts, or starts where
    /// it ends, also keeps the caret on that line from clearing it.
    #[test]
    fn enter_keeps_a_line_of_indentation_when_a_selection_meets_it() {
        let mut editor = Editor::new("a\n    ").unwrap();
        editor.selections = SelectionSet::new(
            vec![
                Selection::range(ByteOffset(0), ByteOffset(2)),
                Selection::caret(ByteOffset(6)),
            ],
            0,
        );
        dispatch(&mut editor, Command::Newline, 0);
        assert_eq!(editor.source(), "\n    \n    ");

        let mut editor = Editor::new("a\n    \nb").unwrap();
        editor.selections = SelectionSet::new(
            vec![
                Selection::caret(ByteOffset(4)),
                Selection::range(ByteOffset(6), ByteOffset(7)),
            ],
            0,
        );
        dispatch(&mut editor, Command::Newline, 0);
        assert_eq!(editor.source(), "a\n  \n    \nb");
    }

    #[test]
    fn movement_breaks_coalescing_and_unicode_delete_is_grapheme_safe() {
        let mut editor = Editor::new("").unwrap();
        dispatch(&mut editor, Command::InsertText("e\u{301}".into()), 0);
        dispatch(
            &mut editor,
            Command::Move {
                motion: Motion::Left,
                extend: false,
            },
            10,
        );
        dispatch(
            &mut editor,
            Command::Move {
                motion: Motion::Right,
                extend: false,
            },
            20,
        );
        dispatch(&mut editor, Command::InsertText("x".into()), 30);
        assert_eq!(editor.undo_depth(), 2);
        dispatch(&mut editor, Command::DeleteBackward, 40);
        dispatch(&mut editor, Command::DeleteBackward, 50);
        assert_eq!(editor.source(), "");
        dispatch(&mut editor, Command::Undo, 60);
        assert_eq!(editor.source(), "e\u{301}x");
    }

    #[test]
    fn boundary_deletes_are_true_no_ops() {
        let mut editor = Editor::new("").unwrap();
        dispatch(&mut editor, Command::DeleteBackward, 0);
        dispatch(&mut editor, Command::DeleteForward, 1);
        assert_eq!(editor.revision(), Revision(0));
        assert_eq!(editor.undo_depth(), 0);
    }

    #[test]
    fn multi_edit_transactions_are_atomic_and_round_trip_history() {
        let mut editor = Editor::new("abcdef").unwrap();
        editor
            .apply_transaction(
                Transaction::new(EditOrigin::Programmatic)
                    .replace(Edit::new(ByteOffset(1)..ByteOffset(2), "XX"))
                    .replace(Edit::new(ByteOffset(4)..ByteOffset(6), "")),
                HistoryMoment(0),
            )
            .unwrap();
        assert_eq!(editor.source(), "aXXcd");
        assert!(editor.undo().unwrap());
        assert_eq!(editor.source(), "abcdef");
        assert!(editor.redo().unwrap());
        assert_eq!(editor.source(), "aXXcd");

        let before = (editor.source(), editor.revision(), editor.undo_depth());
        let invalid = Transaction::new(EditOrigin::Programmatic)
            .replace(Edit::new(ByteOffset(0)..ByteOffset(2), "x"))
            .replace(Edit::new(ByteOffset(1)..ByteOffset(3), "y"));
        assert!(matches!(
            editor.apply_transaction(invalid, HistoryMoment(1)),
            Err(EditorError::Document(DocumentError::OverlappingEdits))
        ));
        assert_eq!(
            (editor.source(), editor.revision(), editor.undo_depth()),
            before
        );
    }

    #[test]
    fn newline_preserves_crlf_and_keyboard_motion_scrolls_horizontally() {
        let mut crlf = Editor::new("a\r\n").unwrap();
        crlf.set_selection(Selection::caret(ByteOffset(3))).unwrap();
        dispatch(&mut crlf, Command::Newline, 0);
        assert_eq!(crlf.source(), "a\r\n\r\n");

        let mut editor = Editor::new("0123456789").unwrap();
        editor.set_view_size(4, 2);
        dispatch(
            &mut editor,
            Command::Move {
                motion: Motion::DocumentEnd,
                extend: false,
            },
            0,
        );
        assert_eq!(editor.viewport().left_column, 7);
        let map = editor.screen_map(GridRect::new(0, 0, 4, 1)).unwrap();
        assert_eq!(
            map.cell_for_offset(editor.primary_selection().head),
            Some(CellPoint::new(3, 0))
        );
    }

    #[test]
    fn horizontal_scroll_extent_exists_only_for_unwrapped_overflow() {
        let mut editor = Editor::new("short\n0123456789abcdef").expect("editor");
        editor.set_viewport(Viewport {
            page_columns: 8,
            wrap: false,
            ..editor.viewport()
        });
        assert_eq!(editor.horizontal_scroll_extent(), Some((0, 8, 17)));
        editor.set_wrap(true);
        assert_eq!(editor.horizontal_scroll_extent(), None);
    }

    #[test]
    fn horizontal_scroll_extent_matches_tabs_graphemes_widgets_and_the_end_caret() {
        let source = "\t👩‍💻e\u{301}界xy";
        let mut editor = Editor::new(source).unwrap();
        editor.set_view_size(7, 2);
        editor.set_inline_widths(vec![InlineWidth {
            at: ByteOffset(source.find('x').unwrap()),
            extra: 6,
        }]);
        dispatch(
            &mut editor,
            Command::Move {
                motion: Motion::DocumentEnd,
                extend: false,
            },
            0,
        );
        let (left, page, total) = editor.horizontal_scroll_extent().unwrap();
        assert_eq!(total, 18, "display cells, inline width and the caret cell");
        assert_eq!(left, total - page);
        let map = editor.screen_map(GridRect::new(0, 0, 7, 2)).unwrap();
        assert_eq!(
            map.cell_for_offset(editor.primary_selection().head),
            Some(CellPoint::new(6, 0))
        );

        editor.set_inline_widths(vec![]);
        assert_eq!(editor.horizontal_scroll_extent().unwrap().2, 12);
        assert_eq!(
            editor.viewport().left_column,
            5,
            "shrinking content clamps the viewport to the new end"
        );
        editor.set_view_size(32, 2);
        assert_eq!(editor.horizontal_scroll_extent(), None);
        assert_eq!(editor.viewport().left_column, 0);
        editor.set_view_size(7, 2);
        editor.set_wrap(true);
        assert_eq!(editor.horizontal_scroll_extent(), None);
        editor.set_wrap(false);
        dispatch(&mut editor, Command::SelectAll, 1);
        dispatch(&mut editor, Command::InsertText("x".into()), 2);
        assert_eq!(
            editor.horizontal_scroll_extent(),
            None,
            "deleting the long line removes overflow"
        );
        dispatch(&mut editor, Command::Undo, 3);
        assert_eq!(
            editor.horizontal_scroll_extent().unwrap().2,
            12,
            "undo updates the extent"
        );
    }

    #[test]
    fn widening_the_view_restores_horizontal_context_without_touching_editor_state() {
        let source = "\t界0123456789";
        let selection = Selection::range(ByteOffset(0), ByteOffset(source.len()));
        let mut editor = Editor::new(source).unwrap();
        editor.set_selection(selection).unwrap();
        editor.set_view_size(4, 2);
        assert_eq!(editor.viewport().left_column, 13);

        let revision = editor.revision();
        editor.set_view_size(10, 2);
        assert_eq!(editor.viewport().left_column, 7);
        let map = editor.screen_map(GridRect::new(0, 0, 10, 1)).unwrap();
        assert_eq!(
            map.cell_for_offset(editor.primary_selection().head),
            Some(CellPoint::new(9, 0))
        );

        editor.set_view_size(17, 2);
        assert_eq!(editor.viewport().left_column, 0);
        assert_eq!(editor.source(), source);
        assert_eq!(editor.revision(), revision);
        assert_eq!(editor.primary_selection(), selection);
        let map = editor.screen_map(GridRect::new(0, 0, 17, 1)).unwrap();
        assert_eq!(
            map.cell_for_offset(ByteOffset(0)),
            Some(CellPoint::new(0, 0))
        );
    }

    /// Wrapped, Down moves to the next row of the same line, straight down
    /// the screen: the rows set in under the line start under a caret
    /// left of their indent. The viewport follows by row.
    #[test]
    fn wrapped_the_caret_moves_by_row_and_the_page_follows() {
        let mut editor = Editor::new("abcdefghij klmnopqrst uvwxyz\nsecond\n").unwrap();
        // Eleven columns: ten a row when wrapped, plus the caret's cell;
        // the rows after the first are set in four cells and hold six.
        editor.set_view_size(11, 4);
        editor.set_wrap(true);
        editor
            .set_selection(Selection::caret(ByteOffset(2)))
            .unwrap();
        let down = Command::Move {
            motion: Motion::Down,
            extend: false,
        };
        dispatch(&mut editor, down.clone(), 0);
        assert_eq!(
            editor.primary_selection().head,
            ByteOffset(11),
            "row two starts under the caret"
        );
        dispatch(&mut editor, down.clone(), 1);
        assert_eq!(
            editor.primary_selection().head,
            ByteOffset(18),
            "row three, one grapheme past the boundary row two ends with"
        );
        dispatch(&mut editor, down.clone(), 2);
        assert_eq!(
            editor.primary_selection().head,
            ByteOffset(23),
            "row four, likewise"
        );
        assert_eq!(editor.viewport().top_row, 0, "four rows fit the page");
        dispatch(&mut editor, down, 3);
        assert_eq!(
            editor.primary_selection().head,
            ByteOffset(31),
            "the next line, column two again"
        );
        assert_eq!(
            editor.viewport().top_row,
            1,
            "the fifth row scrolls the page by one"
        );
        let up = Command::Move {
            motion: Motion::Up,
            extend: false,
        };
        for step in 0..4 {
            dispatch(&mut editor, up.clone(), 4 + step);
        }
        assert_eq!(
            editor.primary_selection().head,
            ByteOffset(2),
            "back up the rows"
        );
        assert_eq!(editor.viewport().left_column, 0, "nothing scrolls sideways");
    }

    #[test]
    fn paging_moves_a_full_view_and_preserves_the_caret_screen_position() {
        let mut editor = Editor::new(&"abcdefgh\n".repeat(40)).unwrap();
        editor.set_view_size(20, 5);
        editor
            .set_selection(Selection::caret(ByteOffset(12)))
            .unwrap();
        let revision = editor.revision();
        for (motion, expected_top, expected_line) in [
            (Motion::PageDown, 5, 6),
            (Motion::PageDown, 10, 11),
            (Motion::PageUp, 5, 6),
            (Motion::PageUp, 0, 1),
        ] {
            dispatch(
                &mut editor,
                Command::Move {
                    motion,
                    extend: false,
                },
                0,
            );
            assert_eq!(editor.viewport().top_row, expected_top);
            assert_eq!(
                editor.primary_selection().head,
                ByteOffset(expected_line * 9 + 3)
            );
        }
        assert_eq!(editor.revision(), revision);
        assert_eq!(editor.undo_depth(), 0);
    }

    #[test]
    fn wrapped_paging_counts_visual_rows_and_shift_keeps_the_selection_anchor() {
        let mut editor = Editor::new(&"abcdefghij klmnopqrst uvwxyz\n".repeat(10)).unwrap();
        editor.set_view_size(11, 4);
        editor.set_wrap(true);
        editor
            .set_selection(Selection::caret(ByteOffset(2)))
            .unwrap();
        dispatch(
            &mut editor,
            Command::Move {
                motion: Motion::PageDown,
                extend: true,
            },
            0,
        );
        assert_eq!(editor.viewport().top_row, 4);
        assert_eq!(editor.primary_selection().anchor, ByteOffset(2));
        assert_eq!(editor.primary_selection().head, ByteOffset(31));
        dispatch(
            &mut editor,
            Command::Move {
                motion: Motion::PageUp,
                extend: true,
            },
            1,
        );
        assert_eq!(editor.viewport().top_row, 0);
        assert_eq!(editor.primary_selection().head, ByteOffset(2));
        assert!(editor.primary_selection().is_empty());
    }

    #[test]
    fn paging_counts_inline_widgets_as_screen_space() {
        for wrap in [false, true] {
            let mut editor = Editor::new(&"abc\n".repeat(30)).unwrap();
            editor.set_view_size(20, 5);
            editor.set_wrap(wrap);
            editor
                .set_virtual_rows(
                    editor.revision(),
                    vec![VirtualRowSpec::new("scope", ByteOffset(1), 3)],
                )
                .unwrap();
            dispatch(
                &mut editor,
                Command::Move {
                    motion: Motion::PageDown,
                    extend: false,
                },
                0,
            );
            assert_eq!(editor.viewport().top_row, 5);
            assert_eq!(
                editor.primary_selection().head,
                ByteOffset(8),
                "two text rows and three widget rows"
            );
            dispatch(
                &mut editor,
                Command::Move {
                    motion: Motion::PageUp,
                    extend: false,
                },
                1,
            );
            assert_eq!(editor.viewport().top_row, 0);
            assert_eq!(editor.primary_selection().head, ByteOffset(0));
        }
    }

    #[test]
    fn paging_passes_widgets_that_cannot_hold_a_caret_and_clamps_at_document_edges() {
        let mut editor = Editor::new("abc\ndef\nghi").unwrap();
        editor.set_view_size(20, 4);
        editor
            .set_virtual_rows(
                editor.revision(),
                vec![VirtualRowSpec::new("scope", ByteOffset(1), 8)],
            )
            .unwrap();
        dispatch(
            &mut editor,
            Command::Move {
                motion: Motion::PageDown,
                extend: false,
            },
            0,
        );
        assert_eq!(editor.primary_selection().head, ByteOffset(4));
        dispatch(
            &mut editor,
            Command::Move {
                motion: Motion::PageUp,
                extend: false,
            },
            1,
        );
        assert_eq!(editor.primary_selection().head, ByteOffset(0));
        for motion in [Motion::PageDown, Motion::PageUp] {
            for _ in 0..5 {
                dispatch(
                    &mut editor,
                    Command::Move {
                        motion,
                        extend: false,
                    },
                    2,
                );
            }
            assert_eq!(
                editor.primary_selection().head,
                ByteOffset(if motion == Motion::PageDown { 8 } else { 0 })
            );
        }
    }

    #[test]
    fn paging_after_mouse_scrolling_uses_the_visible_page() {
        let mut editor = Editor::new(&"abc\n".repeat(50)).unwrap();
        editor.set_view_size(20, 5);
        editor.set_viewport(Viewport {
            top_row: 20,
            ..editor.viewport()
        });
        dispatch(
            &mut editor,
            Command::Move {
                motion: Motion::PageDown,
                extend: false,
            },
            0,
        );
        assert_eq!(editor.viewport().top_row, 25);
        assert_eq!(editor.primary_selection().head, ByteOffset(100));
    }

    #[test]
    fn vertical_motion_retains_goal_display_column() {
        let mut editor = Editor::new("abcd\nx\nabcdef").unwrap();
        editor
            .set_selection(Selection::caret(ByteOffset(4)))
            .unwrap();
        dispatch(
            &mut editor,
            Command::Move {
                motion: Motion::Down,
                extend: false,
            },
            0,
        );
        assert_eq!(editor.primary_selection().head, ByteOffset(6));
        dispatch(
            &mut editor,
            Command::Move {
                motion: Motion::Down,
                extend: false,
            },
            1,
        );
        assert_eq!(editor.primary_selection().head, ByteOffset(11));
    }

    #[test]
    fn copy_cut_and_clipboard_refusal_do_not_lose_text() {
        #[derive(Default)]
        struct RefusingClipboard;
        impl Clipboard for RefusingClipboard {
            fn get_text(&mut self) -> Result<String, ClipboardError> {
                Err(ClipboardError("refused".into()))
            }
            fn set_text(&mut self, _text: String) -> Result<(), ClipboardError> {
                Err(ClipboardError("refused".into()))
            }
        }

        let mut editor = Editor::new("abc").unwrap();
        editor
            .set_selection(Selection::range(ByteOffset(0), ByteOffset(3)))
            .unwrap();
        assert!(
            editor
                .dispatch(Command::Cut, HistoryMoment(0), &mut RefusingClipboard)
                .is_err()
        );
        assert_eq!(editor.source(), "abc");

        let mut clipboard = MemoryClipboard::default();
        editor
            .dispatch(Command::Cut, HistoryMoment(1), &mut clipboard)
            .unwrap();
        assert_eq!(clipboard.contents(), Some("abc"));
        assert_eq!(editor.source(), "");
    }

    #[test]
    fn evaluate_does_not_erase_undo() {
        let mut editor = Editor::new("").unwrap();
        dispatch(&mut editor, Command::InsertText("abc".into()), 0);
        let depth = editor.undo_depth();
        let effects = editor
            .dispatch(
                Command::Evaluate,
                HistoryMoment(10),
                &mut MemoryClipboard::default(),
            )
            .unwrap();
        assert!(matches!(
            effects.as_slice(),
            [EditorEffect::Evaluate { .. }]
        ));
        assert_eq!(editor.undo_depth(), depth);
        dispatch(&mut editor, Command::Undo, 30);
        assert_eq!(editor.source(), "");
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn shift_click_extends_reduces_and_inverts_the_selection_around_its_anchor() {
        let mut editor = Editor::new("one two three four").unwrap();
        let map = editor.screen_map(GridRect::new(0, 0, 20, 2)).unwrap();
        let mut moment = 0;
        let mut press = |editor: &mut Editor, column, modifiers| {
            for kind in [
                MouseEventKind::Down(MouseButton::Left),
                MouseEventKind::Up(MouseButton::Left),
            ] {
                moment += 10;
                editor
                    .mouse_event(
                        MouseEvent {
                            modifiers,
                            ..mouse(kind, column, 0)
                        },
                        &map,
                        HistoryMoment(moment),
                    )
                    .unwrap();
            }
        };
        press(&mut editor, 5, KeyModifiers::NONE);
        // kitty keeps Shift+click, so Alt and Ctrl extend as Shift does.
        for (column, modifiers) in [
            (13, KeyModifiers::SHIFT),
            (9, KeyModifiers::ALT),
            (1, KeyModifiers::CONTROL),
            (16, KeyModifiers::SHIFT),
        ] {
            press(&mut editor, column, modifiers);
            assert_eq!(
                editor.primary_selection(),
                Selection::range(ByteOffset(5), ByteOffset(column.into()))
            );
        }
        // A second Shift+click on the same cell is not a double click.
        press(&mut editor, 16, KeyModifiers::SHIFT);
        assert_eq!(
            editor.primary_selection(),
            Selection::range(ByteOffset(5), ByteOffset(16))
        );
        // The next plain click is not a double click either.
        press(&mut editor, 16, KeyModifiers::NONE);
        assert_eq!(editor.primary_selection(), Selection::caret(ByteOffset(16)));
    }

    #[test]
    fn mouse_click_drag_double_triple_and_wheel_drag_use_screen_map() {
        let mut editor = Editor::new("one two\nthree four\nfive six\nseven").unwrap();
        editor.set_page_rows(2);
        let area = GridRect::new(0, 0, 20, 2);
        let map = editor.screen_map(area).unwrap();
        editor
            .mouse_event(
                mouse(MouseEventKind::Down(MouseButton::Left), 1, 0),
                &map,
                HistoryMoment(0),
            )
            .unwrap();
        editor
            .mouse_event(
                mouse(MouseEventKind::Up(MouseButton::Left), 1, 0),
                &map,
                HistoryMoment(1),
            )
            .unwrap();
        assert_eq!(editor.primary_selection().head, ByteOffset(1));

        // Second click selects a Unicode word; third selects the complete line.
        editor
            .mouse_event(
                mouse(MouseEventKind::Down(MouseButton::Left), 1, 0),
                &map,
                HistoryMoment(100),
            )
            .unwrap();
        assert_eq!(
            editor.primary_selection().ordered(),
            ByteOffset(0)..ByteOffset(3)
        );
        editor
            .mouse_event(
                mouse(MouseEventKind::Down(MouseButton::Left), 1, 0),
                &map,
                HistoryMoment(200),
            )
            .unwrap();
        assert_eq!(
            editor.primary_selection().ordered(),
            ByteOffset(0)..ByteOffset(8)
        );

        // Begin a fresh character drag, then wheel while the button remains held.
        editor.last_click = None;
        editor
            .mouse_event(
                mouse(MouseEventKind::Down(MouseButton::Left), 0, 0),
                &map,
                HistoryMoment(1_000),
            )
            .unwrap();
        editor
            .mouse_event(
                mouse(MouseEventKind::Drag(MouseButton::Left), 5, 1),
                &map,
                HistoryMoment(1_010),
            )
            .unwrap();
        let before_scroll = editor.primary_selection().ordered().end;
        editor
            .mouse_event(
                mouse(MouseEventKind::ScrollDown, 5, 1),
                &map,
                HistoryMoment(1_020),
            )
            .unwrap();
        assert!(editor.viewport.top_row > 0);
        assert!(editor.primary_selection().ordered().end > before_scroll);
    }

    #[test]
    fn the_viewport_can_scroll_until_the_last_row_reaches_the_top() {
        let mut editor = Editor::new("one\ntwo\nthree\nfour\nfive").unwrap();
        editor.set_page_rows(3);
        let area = GridRect::new(0, 0, 20, 3);
        for _ in 0..10 {
            let map = editor.screen_map(area).unwrap();
            editor
                .mouse_event(
                    mouse(MouseEventKind::ScrollDown, 0, 0),
                    &map,
                    HistoryMoment(0),
                )
                .unwrap();
        }
        assert_eq!(
            editor.viewport().top_row,
            4,
            "the last line scrolls up to the top of the pane"
        );
        let map = editor.screen_map(area).unwrap();
        assert_eq!(map.rows().len(), 1);

        // Keyboard motion never scrolls past what the caret needs.
        editor.set_viewport(Viewport {
            top_row: 99,
            ..editor.viewport()
        });
        assert_eq!(editor.viewport().top_row, 4);
    }

    #[test]
    fn copying_across_an_inline_visual_never_includes_the_widget() {
        let mut editor = Editor::new("$: s(\"bd\")._scope()\n$: s(\"hh\")").unwrap();
        let revision = editor.revision();
        editor
            .set_virtual_rows(
                revision,
                vec![VirtualRowSpec::new("scope", ByteOffset(5), 6)],
            )
            .unwrap();
        // Drag from the first line, over the widget rows, into the second.
        let area = GridRect::new(0, 0, 40, 10);
        let map = editor.screen_map(area).unwrap();
        editor
            .mouse_event(
                mouse(MouseEventKind::Down(MouseButton::Left), 0, 0),
                &map,
                HistoryMoment(0),
            )
            .unwrap();
        editor
            .mouse_event(
                mouse(MouseEventKind::Drag(MouseButton::Left), 3, 4),
                &map,
                HistoryMoment(10),
            )
            .unwrap();
        editor
            .mouse_event(
                mouse(MouseEventKind::Drag(MouseButton::Left), 3, 7),
                &map,
                HistoryMoment(20),
            )
            .unwrap();
        let mut clipboard = MemoryClipboard::default();
        editor
            .dispatch(Command::Copy, HistoryMoment(30), &mut clipboard)
            .unwrap();
        assert_eq!(clipboard.contents(), Some("$: s(\"bd\")._scope()\n$: "));
    }

    #[test]
    fn virtual_rows_are_revision_gated_and_stay_mapped_through_history() {
        let mut editor = Editor::new("a\nb").unwrap();
        let revision = editor.revision();
        editor
            .set_virtual_rows(
                revision,
                vec![VirtualRowSpec::new("scope", ByteOffset(1), 2)],
            )
            .unwrap();
        assert_eq!(editor.virtual_rows().len(), 1);
        dispatch(&mut editor, Command::InsertText("x".into()), 0);
        assert_eq!(editor.virtual_rows()[0].after, ByteOffset(2));
        dispatch(&mut editor, Command::Undo, 1);
        assert_eq!(editor.virtual_rows()[0].after, ByteOffset(1));
        dispatch(&mut editor, Command::Redo, 2);
        assert_eq!(editor.virtual_rows()[0].after, ByteOffset(2));
        assert!(matches!(
            editor.set_virtual_rows(
                revision,
                vec![VirtualRowSpec::new("stale", ByteOffset(1), 1)]
            ),
            Err(EditorError::VirtualRows(
                VirtualRowError::StaleRevision { .. }
            ))
        ));
    }

    #[test]
    fn stale_mouse_maps_are_rejected() {
        let mut editor = Editor::new("abc").unwrap();
        let map = editor.screen_map(GridRect::new(0, 0, 10, 2)).unwrap();
        dispatch(&mut editor, Command::InsertText("x".into()), 0);
        assert!(matches!(
            editor.mouse_event(
                mouse(MouseEventKind::Down(MouseButton::Left), 0, 0),
                &map,
                HistoryMoment(1)
            ),
            Err(EditorError::StaleScreenMap { .. })
        ));
    }

    #[test]
    fn a_press_in_the_empty_space_below_the_text_starts_a_selection_there() {
        // The blank space below the text is still the editor. A press there
        // lands on the last line, and the drag that follows selects.
        let mut editor = Editor::new("abc\ndef").unwrap();
        let map = editor.screen_map(GridRect::new(0, 0, 20, 10)).unwrap();

        let changed = editor
            .mouse_event(
                mouse(MouseEventKind::Down(MouseButton::Left), 15, 8),
                &map,
                HistoryMoment(1),
            )
            .unwrap();
        assert!(changed, "the press belongs to the editor");
        assert_eq!(editor.primary_selection().head, ByteOffset(7));

        // Drag up into the text: the selection runs from the end back to it.
        editor
            .mouse_event(
                mouse(MouseEventKind::Drag(MouseButton::Left), 1, 0),
                &map,
                HistoryMoment(1),
            )
            .unwrap();
        let range = editor.primary_selection().ordered();
        assert_eq!((range.start, range.end), (ByteOffset(1), ByteOffset(7)));
    }

    #[test]
    fn a_new_click_outside_the_source_grid_does_not_move_the_caret() {
        let mut editor = Editor::new("abc\ndef").unwrap();
        editor
            .set_selection(Selection::caret(ByteOffset(2)))
            .unwrap();
        let map = editor.screen_map(GridRect::new(4, 2, 20, 2)).unwrap();

        let changed = editor
            .mouse_event(
                mouse(MouseEventKind::Down(MouseButton::Left), 30, 10),
                &map,
                HistoryMoment(1),
            )
            .unwrap();

        assert!(!changed);
        assert_eq!(editor.primary_selection(), Selection::caret(ByteOffset(2)));
    }

    #[test]
    fn toggling_comments_covers_every_touched_line_at_the_shallowest_indent() {
        let mut editor = Editor::new("$: s(\"bd\")\n  .fast(2)\n\n.gain(.5)\n").unwrap();
        editor
            .set_selection(Selection::range(ByteOffset(0), ByteOffset(22)))
            .unwrap();
        dispatch(&mut editor, Command::ToggleComment, 0);
        assert_eq!(
            editor.source(),
            "// $: s(\"bd\")\n//   .fast(2)\n\n.gain(.5)\n",
            "both lines are commented at column zero, the blank line is left alone"
        );

        dispatch(&mut editor, Command::ToggleComment, 100);
        assert_eq!(editor.source(), "$: s(\"bd\")\n  .fast(2)\n\n.gain(.5)\n");
    }

    #[test]
    fn a_partly_commented_selection_comments_the_rest_rather_than_uncommenting() {
        let mut editor = Editor::new("// one\ntwo\n").unwrap();
        editor
            .set_selection(Selection::range(ByteOffset(0), ByteOffset(10)))
            .unwrap();
        dispatch(&mut editor, Command::ToggleComment, 0);
        assert_eq!(editor.source(), "// // one\n// two\n");
    }

    #[test]
    fn commenting_one_line_needs_no_selection_and_keeps_its_indentation() {
        let mut editor = Editor::new("    .fast(2)\n").unwrap();
        editor
            .set_selection(Selection::caret(ByteOffset(6)))
            .unwrap();
        dispatch(&mut editor, Command::ToggleComment, 0);
        assert_eq!(editor.source(), "    // .fast(2)\n");
        dispatch(&mut editor, Command::ToggleComment, 100);
        assert_eq!(editor.source(), "    .fast(2)\n");
    }

    #[test]
    fn toggling_a_blank_document_changes_nothing() {
        let mut editor = Editor::new("\n\n").unwrap();
        let before = editor.revision();
        dispatch(&mut editor, Command::ToggleComment, 0);
        assert_eq!(editor.revision(), before);
    }

    #[test]
    fn a_source_range_follows_the_edits_made_after_its_revision() {
        let mut editor = Editor::new("$: s(\"bd\")").unwrap();
        let evaluated = editor.revision();
        // The mark covers `bd`.
        assert_eq!(editor.map_range_since(evaluated, 6..8), Some(6..8));

        // Typing before it shifts the mark; typing after it does not.
        editor
            .set_selection(Selection::caret(ByteOffset(0)))
            .unwrap();
        dispatch(&mut editor, Command::InsertText("// ".into()), 0);
        assert_eq!(editor.map_range_since(evaluated, 6..8), Some(9..11));

        editor
            .set_selection(Selection::caret(ByteOffset(editor.source().len())))
            .unwrap();
        dispatch(&mut editor, Command::InsertText(".fast(2)".into()), 100);
        assert_eq!(editor.map_range_since(evaluated, 6..8), Some(9..11));
    }

    #[test]
    fn typing_at_either_edge_of_a_mark_never_extends_it() {
        let mut editor = Editor::new("s(\"bd\")").unwrap();
        let evaluated = editor.revision();
        editor
            .set_selection(Selection::caret(ByteOffset(3)))
            .unwrap();
        dispatch(&mut editor, Command::InsertText("x".into()), 0);
        // `bd` began at byte 3; the inserted character stays outside it.
        assert_eq!(editor.map_range_since(evaluated, 3..5), Some(4..6));

        editor
            .set_selection(Selection::caret(ByteOffset(6)))
            .unwrap();
        dispatch(&mut editor, Command::InsertText("y".into()), 100);
        assert_eq!(editor.map_range_since(evaluated, 3..5), Some(4..6));
    }

    #[test]
    fn deleting_the_text_a_mark_covers_retires_the_mark() {
        let mut editor = Editor::new("$: s(\"bd\")").unwrap();
        let evaluated = editor.revision();
        editor
            .set_selection(Selection::range(ByteOffset(6), ByteOffset(8)))
            .unwrap();
        dispatch(&mut editor, Command::DeleteForward, 0);
        assert_eq!(editor.map_range_since(evaluated, 6..8), None);
    }

    #[test]
    fn undo_and_redo_are_recorded_in_the_trail_too() {
        let mut editor = Editor::new("s(\"bd\")").unwrap();
        let evaluated = editor.revision();
        editor
            .set_selection(Selection::caret(ByteOffset(0)))
            .unwrap();
        dispatch(&mut editor, Command::InsertText("// ".into()), 0);
        assert_eq!(editor.map_range_since(evaluated, 2..4), Some(5..7));

        dispatch(&mut editor, Command::Undo, 100);
        assert_eq!(
            editor.map_range_since(evaluated, 2..4),
            Some(2..4),
            "undoing the shift puts the mark back where it started"
        );
        dispatch(&mut editor, Command::Redo, 200);
        assert_eq!(editor.map_range_since(evaluated, 2..4), Some(5..7));
    }

    /// A held slider key is an edit per repeat. The evaluated revision is
    /// pinned, so the widgets and marks expressed in its coordinates keep
    /// their anchors through thousands of them; the trail folds the rest.
    #[test]
    fn a_pinned_revision_keeps_mapping_through_a_long_trail() {
        let mut editor = Editor::new("abc").unwrap();
        let evaluated = editor.revision();
        editor.pin_revision(evaluated);
        editor
            .set_selection(Selection::caret(ByteOffset(0)))
            .unwrap();
        for step in 0..MAX_EDIT_TRAIL_ENTRIES * 4 {
            editor.close_history_group();
            dispatch(
                &mut editor,
                Command::InsertText("x".into()),
                step as u64 * 1_000,
            );
        }
        let inserted = MAX_EDIT_TRAIL_ENTRIES * 4;
        assert_eq!(
            editor.map_range_since(evaluated, 0..3),
            Some(inserted..inserted + 3),
            "the pinned revision still maps, exactly"
        );
        assert_eq!(editor.map_offset_since(evaluated, 3), Some(inserted + 3));
    }

    /// The trail folds rather than forgets: the oldest boundary survives
    /// any number of edits, an unpinned boundary in the middle is folded
    /// away once the trail is full, and the newest always maps.
    #[test]
    fn an_unpinned_middle_revision_folds_away_but_the_ends_map() {
        let mut editor = Editor::new("abc").unwrap();
        let ancient = editor.revision();
        editor
            .set_selection(Selection::caret(ByteOffset(0)))
            .unwrap();
        let mut middle = None;
        let total = MAX_EDIT_TRAIL_ENTRIES * 2;
        for step in 0..total {
            editor.close_history_group();
            dispatch(
                &mut editor,
                Command::InsertText("x".into()),
                step as u64 * 1_000,
            );
            if step == 10 {
                middle = Some(editor.revision());
            }
        }
        assert_eq!(
            editor.map_range_since(ancient, 0..3),
            Some(total..total + 3),
            "the oldest boundary is kept by folding"
        );
        assert_eq!(
            editor.map_range_since(middle.expect("middle"), 0..3),
            None,
            "an unpinned boundary in the middle is folded away"
        );
        let recent = editor.revision();
        assert_eq!(editor.map_range_since(recent, 0..3), Some(0..3));
    }

    #[test]
    fn an_inline_widget_anchor_follows_its_call_through_edits() {
        let mut editor = Editor::new("s(\"bd\")._scope()").unwrap();
        let evaluated = editor.revision();
        let anchor = editor.source().len();
        editor
            .set_selection(Selection::caret(ByteOffset(0)))
            .unwrap();
        dispatch(&mut editor, Command::InsertText("$: ".into()), 0);
        assert_eq!(editor.map_offset_since(evaluated, anchor), Some(anchor + 3));
    }
}
