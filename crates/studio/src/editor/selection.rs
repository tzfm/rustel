use std::ops::Range;

use super::document::{ByteOffset, Document, DocumentError, Edit, EditShape};

/// An anchored selection. `anchor` remains fixed while Shift or a mouse drag
/// moves `head`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Selection {
    pub anchor: ByteOffset,
    pub head: ByteOffset,
    /// Desired terminal cell column for repeated vertical movement.
    pub goal_column: Option<usize>,
}

impl Selection {
    pub const fn caret(offset: ByteOffset) -> Self {
        Self {
            anchor: offset,
            head: offset,
            goal_column: None,
        }
    }

    pub const fn range(anchor: ByteOffset, head: ByteOffset) -> Self {
        Self {
            anchor,
            head,
            goal_column: None,
        }
    }

    pub fn ordered(self) -> Range<ByteOffset> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    pub fn is_empty(self) -> bool {
        self.anchor == self.head
    }

    pub fn with_head(self, head: ByteOffset, goal_column: Option<usize>) -> Self {
        Self {
            anchor: self.anchor,
            head,
            goal_column,
        }
    }
}

/// Sorted, non-overlapping editor selections.  PR1 presents one primary
/// selection, but edit transactions already operate on every range so adding
/// Alt-click carets does not require changing the document/history format.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectionSet {
    ranges: Vec<Selection>,
    primary: usize,
}

impl SelectionSet {
    pub fn single(offset: ByteOffset) -> Self {
        Self {
            ranges: vec![Selection::caret(offset)],
            primary: 0,
        }
    }

    pub fn from_selection(selection: Selection) -> Self {
        Self {
            ranges: vec![selection],
            primary: 0,
        }
    }

    pub fn new(mut ranges: Vec<Selection>, primary: usize) -> Self {
        if ranges.is_empty() {
            return Self::single(ByteOffset::ZERO);
        }
        let primary_selection = ranges[primary.min(ranges.len() - 1)];
        ranges.sort_by_key(|selection| (selection.ordered().start, selection.ordered().end));
        let mut merged: Vec<Selection> = Vec::with_capacity(ranges.len());
        for selection in ranges {
            if let Some(previous) = merged.last_mut()
                && (selection.ordered().start < previous.ordered().end
                    || (selection.is_empty()
                        && previous.is_empty()
                        && selection.head == previous.head))
            {
                let start = previous.ordered().start.min(selection.ordered().start);
                let end = previous.ordered().end.max(selection.ordered().end);
                *previous = Selection::range(start, end);
            } else {
                merged.push(selection);
            }
        }
        let primary = merged
            .iter()
            .position(|selection| {
                let ordered = selection.ordered();
                let requested = primary_selection.ordered();
                ordered.start <= requested.start && ordered.end >= requested.end
            })
            .unwrap_or(0);
        Self {
            ranges: merged,
            primary,
        }
    }

    pub fn primary(&self) -> Selection {
        self.ranges[self.primary]
    }

    pub fn ranges(&self) -> &[Selection] {
        &self.ranges
    }

    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    pub fn set_single(&mut self, selection: Selection) {
        self.ranges.clear();
        self.ranges.push(selection);
        self.primary = 0;
    }

    pub fn validate(&self, document: &Document) -> Result<(), DocumentError> {
        for selection in &self.ranges {
            document.validate_caret_offset(selection.anchor)?;
            document.validate_caret_offset(selection.head)?;
        }
        Ok(())
    }

    pub(crate) fn map_through(&self, edits: &[Edit]) -> Self {
        let ranges = self
            .ranges
            .iter()
            .map(|selection| {
                if selection.is_empty() {
                    return Selection::caret(map_offset(selection.head, edits, true));
                }
                let forward = selection.anchor <= selection.head;
                Selection {
                    anchor: map_offset(selection.anchor, edits, !forward),
                    head: map_offset(selection.head, edits, forward),
                    goal_column: selection.goal_column,
                }
            })
            .collect();
        Self::new(ranges, self.primary)
    }
}

/// Map a pre-transaction position into the resulting document.
pub(crate) fn map_offset(offset: ByteOffset, edits: &[Edit], associate_after: bool) -> ByteOffset {
    let mut delta: i64 = 0;
    for edit in edits {
        if offset < edit.range.start {
            break;
        }
        let mapped_start = add_delta(edit.range.start.0, delta);
        if offset <= edit.range.end {
            return ByteOffset(if associate_after {
                mapped_start + edit.insert.len()
            } else {
                mapped_start
            });
        }
        delta +=
            edit.insert.len() as i64 - (edit.range.end.0.saturating_sub(edit.range.start.0)) as i64;
    }
    ByteOffset(add_delta(offset.0, delta))
}

pub(crate) fn map_offset_shapes(
    offset: ByteOffset,
    edits: &[EditShape],
    associate_after: bool,
) -> ByteOffset {
    let mut delta: i64 = 0;
    for edit in edits {
        if offset < edit.range.start {
            break;
        }
        let mapped_start = add_delta(edit.range.start.0, delta);
        if offset <= edit.range.end {
            return ByteOffset(if associate_after {
                mapped_start + edit.inserted_bytes
            } else {
                mapped_start
            });
        }
        delta += edit.inserted_bytes as i64
            - (edit.range.end.0.saturating_sub(edit.range.start.0)) as i64;
    }
    ByteOffset(add_delta(offset.0, delta))
}

fn add_delta(value: usize, delta: i64) -> usize {
    if delta >= 0 {
        value.saturating_add(delta as usize)
    } else {
        value.saturating_sub(delta.unsigned_abs() as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping_honours_endpoint_affinity() {
        let edits = [Edit::new(ByteOffset(2)..ByteOffset(4), "XYZ")];
        assert_eq!(map_offset(ByteOffset(2), &edits, false), ByteOffset(2));
        assert_eq!(map_offset(ByteOffset(2), &edits, true), ByteOffset(5));
        assert_eq!(map_offset(ByteOffset(6), &edits, true), ByteOffset(7));
    }

    #[test]
    fn selection_set_sorts_and_merges_overlap() {
        let set = SelectionSet::new(
            vec![
                Selection::range(ByteOffset(8), ByteOffset(4)),
                Selection::range(ByteOffset(2), ByteOffset(6)),
            ],
            1,
        );
        assert_eq!(
            set.ranges(),
            &[Selection::range(ByteOffset(2), ByteOffset(8))]
        );
    }

    #[test]
    fn carets_track_insertions_without_becoming_selections() {
        let set = SelectionSet::new(
            vec![
                Selection::caret(ByteOffset(2)),
                Selection::caret(ByteOffset(2)),
            ],
            0,
        );
        assert_eq!(set.len(), 1);
        let mapped = set.map_through(&[Edit::new(ByteOffset(2)..ByteOffset(2), "x")]);
        assert_eq!(mapped.primary(), Selection::caret(ByteOffset(3)));
    }
}
