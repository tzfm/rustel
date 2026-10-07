use super::document::{ByteOffset, Document, DocumentError, Edit, EditShape};
use super::selection::SelectionSet;

pub const DEFAULT_HISTORY_GROUP_DELAY_MS: u64 = 500;
pub const DEFAULT_HISTORY_MAX_ENTRIES: usize = 10_000;
pub const DEFAULT_HISTORY_MAX_BYTES: usize = 64 * 1024 * 1024;

/// Deterministic UI-clock timestamp.  Tests need not sleep to exercise
/// coalescing, and callers can derive it from their event-loop start instant.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct HistoryMoment(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditOrigin {
    Typing,
    Backspace,
    Delete,
    Paste,
    Cut,
    Newline,
    Indent,
    /// A control such as an inline slider rewriting its own literal. One
    /// drag is many edits and should undo as one.
    Control,
    Programmatic,
}

impl EditOrigin {
    fn may_coalesce(self) -> bool {
        matches!(
            self,
            Self::Typing | Self::Backspace | Self::Delete | Self::Control
        )
    }
}

#[derive(Clone, Copy, Debug)]
pub struct HistoryConfig {
    pub group_delay_ms: u64,
    pub maximum_entries: usize,
    pub maximum_retained_bytes: usize,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            group_delay_ms: DEFAULT_HISTORY_GROUP_DELAY_MS,
            maximum_entries: DEFAULT_HISTORY_MAX_ENTRIES,
            maximum_retained_bytes: DEFAULT_HISTORY_MAX_BYTES,
        }
    }
}

/// Public transaction: all edit ranges are coordinates in the document before
/// the transaction, and must be sorted and non-overlapping.
#[derive(Clone, Debug)]
pub struct Transaction {
    pub edits: Vec<Edit>,
    pub selection_after: Option<SelectionSet>,
    pub origin: EditOrigin,
}

impl Transaction {
    pub fn new(origin: EditOrigin) -> Self {
        Self {
            edits: Vec::new(),
            selection_after: None,
            origin,
        }
    }

    pub fn replace(mut self, edit: Edit) -> Self {
        self.edits.push(edit);
        self
    }

    pub fn with_selection(mut self, selection: SelectionSet) -> Self {
        self.selection_after = Some(selection);
        self
    }
}

#[derive(Clone, Debug)]
pub(crate) struct HistoryEntry {
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    before: SelectionSet,
    after: SelectionSet,
    origin: EditOrigin,
    moment: HistoryMoment,
    retained_bytes: usize,
}

impl HistoryEntry {
    pub(crate) fn apply(
        document: &mut Document,
        transaction: Transaction,
        before: SelectionSet,
        after: SelectionSet,
        moment: HistoryMoment,
    ) -> Result<Self, DocumentError> {
        let edits = transaction.edits;
        let mut delta: i64 = 0;
        let mut inverse_ranges = Vec::with_capacity(edits.len());
        for edit in &edits {
            let mapped_start = add_delta(edit.range.start.0, delta);
            inverse_ranges
                .push(ByteOffset(mapped_start)..ByteOffset(mapped_start + edit.insert.len()));
            delta += edit.insert.len() as i64
                - (edit.range.end.0.saturating_sub(edit.range.start.0)) as i64;
        }
        let removed = document.apply_edits(&edits)?;
        let undo = inverse_ranges
            .into_iter()
            .zip(removed.iter())
            .map(|(range, text)| Edit::new(range, text.clone()))
            .collect::<Vec<_>>();
        let retained_bytes = edits.iter().map(|edit| edit.insert.len()).sum::<usize>()
            + removed.iter().map(String::len).sum::<usize>();
        Ok(Self {
            undo,
            redo: edits,
            before,
            after,
            origin: transaction.origin,
            moment,
            retained_bytes,
        })
    }
}

fn add_delta(value: usize, delta: i64) -> usize {
    if delta >= 0 {
        value.saturating_add(delta as usize)
    } else {
        value.saturating_sub(delta.unsigned_abs() as usize)
    }
}

#[derive(Clone, Debug)]
struct HistoryGroup {
    entries: Vec<HistoryEntry>,
    retained_bytes: usize,
}

impl HistoryGroup {
    fn new(entry: HistoryEntry) -> Self {
        let retained_bytes = entry.retained_bytes;
        Self {
            entries: vec![entry],
            retained_bytes,
        }
    }

    fn first(&self) -> &HistoryEntry {
        &self.entries[0]
    }

    fn last(&self) -> &HistoryEntry {
        self.entries.last().expect("history group is never empty")
    }
}

#[derive(Clone, Debug)]
pub(crate) struct History {
    undo: Vec<HistoryGroup>,
    redo: Vec<HistoryGroup>,
    retained_bytes: usize,
    force_new_group: bool,
    config: HistoryConfig,
}

pub(crate) struct HistoryChange {
    pub selection: SelectionSet,
    pub edit_steps: Vec<Vec<EditShape>>,
}

impl History {
    pub(crate) fn new(config: HistoryConfig) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            retained_bytes: 0,
            force_new_group: true,
            config,
        }
    }

    pub(crate) fn close_group(&mut self) {
        self.force_new_group = true;
    }

    pub(crate) fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub(crate) fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub(crate) fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    pub(crate) fn redo_depth(&self) -> usize {
        self.redo.len()
    }

    /// The text both stacks keep, inserted and removed: what the byte
    /// bound holds them to. The entries' own bookkeeping is not in it.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    pub(crate) fn record(&mut self, entry: HistoryEntry) {
        self.drop_redo();
        let joins = !self.force_new_group
            && entry.origin.may_coalesce()
            && self.undo.last().is_some_and(|group| {
                let previous = group.last();
                previous.origin == entry.origin
                    && entry.moment.0.saturating_sub(previous.moment.0)
                        <= self.config.group_delay_ms
                    && previous.after == entry.before
            });
        self.retained_bytes = self.retained_bytes.saturating_add(entry.retained_bytes);
        if joins {
            let group = self
                .undo
                .last_mut()
                .expect("join requires a previous group");
            group.retained_bytes = group.retained_bytes.saturating_add(entry.retained_bytes);
            group.entries.push(entry);
        } else {
            self.undo.push(HistoryGroup::new(entry));
        }
        self.force_new_group = false;
        self.enforce_bounds();
    }

    pub(crate) fn undo(
        &mut self,
        document: &mut Document,
    ) -> Result<Option<HistoryChange>, DocumentError> {
        let Some(group) = self.undo.last() else {
            return Ok(None);
        };
        let mut candidate = document.clone();
        let edit_steps = group
            .entries
            .iter()
            .rev()
            .map(|entry| entry.undo.iter().map(EditShape::from).collect())
            .collect::<Vec<_>>();
        for entry in group.entries.iter().rev() {
            candidate.apply_edits(&entry.undo)?;
        }
        let selection = group.first().before.clone();
        let group = self.undo.pop().expect("group was inspected above");
        *document = candidate;
        self.redo.push(group);
        self.force_new_group = true;
        Ok(Some(HistoryChange {
            selection,
            edit_steps,
        }))
    }

    pub(crate) fn redo(
        &mut self,
        document: &mut Document,
    ) -> Result<Option<HistoryChange>, DocumentError> {
        let Some(group) = self.redo.last() else {
            return Ok(None);
        };
        let mut candidate = document.clone();
        let edit_steps = group
            .entries
            .iter()
            .map(|entry| entry.redo.iter().map(EditShape::from).collect())
            .collect::<Vec<_>>();
        for entry in &group.entries {
            candidate.apply_edits(&entry.redo)?;
        }
        let selection = group.last().after.clone();
        let group = self.redo.pop().expect("group was inspected above");
        *document = candidate;
        self.undo.push(group);
        self.force_new_group = true;
        Ok(Some(HistoryChange {
            selection,
            edit_steps,
        }))
    }

    fn drop_redo(&mut self) {
        for group in self.redo.drain(..) {
            self.retained_bytes = self.retained_bytes.saturating_sub(group.retained_bytes);
        }
    }

    fn enforce_bounds(&mut self) {
        while self.undo.len() > self.config.maximum_entries
            || (self.retained_bytes > self.config.maximum_retained_bytes && self.undo.len() > 1)
        {
            let removed = self.undo.remove(0);
            self.retained_bytes = self.retained_bytes.saturating_sub(removed.retained_bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::selection::Selection;

    fn selection(offset: usize) -> SelectionSet {
        SelectionSet::from_selection(Selection::caret(ByteOffset(offset)))
    }

    fn entry(
        document: &mut Document,
        at: usize,
        text: &str,
        origin: EditOrigin,
        moment: u64,
    ) -> HistoryEntry {
        HistoryEntry::apply(
            document,
            Transaction::new(origin).replace(Edit::new(ByteOffset(at)..ByteOffset(at), text)),
            selection(at),
            selection(at + text.len()),
            HistoryMoment(moment),
        )
        .unwrap()
    }

    #[test]
    fn adjacent_typing_coalesces_but_a_pause_does_not() {
        let mut document = Document::new("", 1024).unwrap();
        let mut history = History::new(HistoryConfig::default());
        let first = entry(&mut document, 0, "a", EditOrigin::Typing, 0);
        history.record(first);
        let second = entry(&mut document, 1, "b", EditOrigin::Typing, 100);
        history.record(second);
        assert_eq!(history.undo_depth(), 1);
        let third = entry(&mut document, 2, "c", EditOrigin::Typing, 700);
        history.record(third);
        assert_eq!(history.undo_depth(), 2);
        history.undo(&mut document).unwrap();
        assert_eq!(document.text(), "ab");
        history.undo(&mut document).unwrap();
        assert_eq!(document.text(), "");
        history.redo(&mut document).unwrap();
        assert_eq!(document.text(), "ab");
    }

    #[test]
    fn a_new_edit_after_undo_discards_redo() {
        let mut document = Document::new("", 1024).unwrap();
        let mut history = History::new(HistoryConfig::default());
        history.record(entry(&mut document, 0, "a", EditOrigin::Typing, 0));
        history.undo(&mut document).unwrap();
        assert!(history.can_redo());
        history.record(entry(&mut document, 0, "b", EditOrigin::Typing, 10));
        assert!(!history.can_redo());
    }
}
