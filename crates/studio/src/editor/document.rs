use std::fmt;
use std::ops::Range;

use ropey::{Rope, RopeSlice};
use unicode_segmentation::UnicodeSegmentation;

/// A UTF-8 byte offset in the exact editor document.
///
/// The runtime's layout and onset protocol also uses UTF-8 byte offsets.  The
/// editor therefore keeps bytes as its public coordinate system and converts
/// to Ropey's character indices only at the mutation boundary.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ByteOffset(pub usize);

impl ByteOffset {
    pub const ZERO: Self = Self(0);

    pub const fn get(self) -> usize {
        self.0
    }
}

impl From<usize> for ByteOffset {
    fn from(value: usize) -> Self {
        Self(value)
    }
}

/// Monotonically increasing identity of the current text.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Revision(pub u64);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DocumentError {
    OffsetOutOfBounds { offset: usize, bytes: usize },
    NotUtf8Boundary(usize),
    NotCaretBoundary(usize),
    InvertedRange { start: usize, end: usize },
    OverlappingEdits,
    DocumentTooLarge { attempted: usize, maximum: usize },
}

impl fmt::Display for DocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OffsetOutOfBounds { offset, bytes } => {
                write!(
                    formatter,
                    "byte offset {offset} exceeds document length {bytes}"
                )
            }
            Self::NotUtf8Boundary(offset) => {
                write!(formatter, "byte offset {offset} is not a UTF-8 boundary")
            }
            Self::NotCaretBoundary(offset) => write!(
                formatter,
                "byte offset {offset} would split a grapheme cluster or line ending"
            ),
            Self::InvertedRange { start, end } => {
                write!(formatter, "document range is inverted ({start} > {end})")
            }
            Self::OverlappingEdits => write!(formatter, "transaction edits overlap"),
            Self::DocumentTooLarge { attempted, maximum } => write!(
                formatter,
                "edit would grow document to {attempted} bytes; maximum is {maximum}"
            ),
        }
    }
}

impl std::error::Error for DocumentError {}

/// One replacement expressed in coordinates of the document before the
/// transaction.  A transaction's edits must be sorted and non-overlapping.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Edit {
    pub range: Range<ByteOffset>,
    pub insert: String,
}

/// Offset-only form of an edit. History uses this to keep decorations mapped
/// through undo/redo without cloning potentially huge inserted text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EditShape {
    pub range: Range<ByteOffset>,
    pub inserted_bytes: usize,
}

impl From<&Edit> for EditShape {
    fn from(edit: &Edit) -> Self {
        Self {
            range: edit.range.clone(),
            inserted_bytes: edit.insert.len(),
        }
    }
}

impl Edit {
    pub fn new(range: Range<ByteOffset>, insert: impl Into<String>) -> Self {
        Self {
            range,
            insert: insert.into(),
        }
    }
}

/// Rope-backed, revisioned UTF-8 document.
#[derive(Clone, Debug)]
pub struct Document {
    rope: Rope,
    revision: Revision,
    maximum_bytes: usize,
}

impl Document {
    pub fn new(text: &str, maximum_bytes: usize) -> Result<Self, DocumentError> {
        if text.len() > maximum_bytes {
            return Err(DocumentError::DocumentTooLarge {
                attempted: text.len(),
                maximum: maximum_bytes,
            });
        }
        Ok(Self {
            rope: Rope::from_str(text),
            revision: Revision(0),
            maximum_bytes,
        })
    }

    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    pub fn snapshot(&self) -> Rope {
        self.rope.clone()
    }

    pub fn revision(&self) -> Revision {
        self.revision
    }

    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    pub fn is_empty(&self) -> bool {
        self.len_bytes() == 0
    }

    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    pub fn maximum_bytes(&self) -> usize {
        self.maximum_bytes
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    pub fn validate_offset(&self, offset: ByteOffset) -> Result<usize, DocumentError> {
        let bytes = self.len_bytes();
        if offset.0 > bytes {
            return Err(DocumentError::OffsetOutOfBounds {
                offset: offset.0,
                bytes,
            });
        }
        let char_index = self.rope.byte_to_char(offset.0);
        if self.rope.char_to_byte(char_index) != offset.0 {
            return Err(DocumentError::NotUtf8Boundary(offset.0));
        }
        Ok(char_index)
    }

    pub fn validate_range(&self, range: &Range<ByteOffset>) -> Result<Range<usize>, DocumentError> {
        if range.start > range.end {
            return Err(DocumentError::InvertedRange {
                start: range.start.0,
                end: range.end.0,
            });
        }
        Ok(self.validate_offset(range.start)?..self.validate_offset(range.end)?)
    }

    /// Validate a position a user-visible caret may occupy. Unlike a general
    /// source span boundary, this may not split an extended grapheme or CRLF.
    pub fn validate_caret_offset(&self, offset: ByteOffset) -> Result<(), DocumentError> {
        self.validate_offset(offset)?;
        if offset == ByteOffset::ZERO || offset.0 == self.len_bytes() {
            return Ok(());
        }
        let line = self.line_of(offset)?;
        let range = self.line_content_range(line);
        if offset == range.start || offset == range.end {
            return Ok(());
        }
        if offset < range.start || offset > range.end {
            return Err(DocumentError::NotCaretBoundary(offset.0));
        }
        let relative = offset.0 - range.start.0;
        if self
            .line_content(line)
            .grapheme_indices(true)
            .any(|(index, _)| index == relative)
        {
            Ok(())
        } else {
            Err(DocumentError::NotCaretBoundary(offset.0))
        }
    }

    pub fn slice(&self, range: Range<ByteOffset>) -> Result<String, DocumentError> {
        self.validate_range(&range)?;
        Ok(self.rope.byte_slice(range.start.0..range.end.0).to_string())
    }

    pub fn line_of(&self, offset: ByteOffset) -> Result<usize, DocumentError> {
        self.validate_offset(offset)?;
        if self.len_bytes() == 0 {
            return Ok(0);
        }
        Ok(self.rope.byte_to_line(offset.0))
    }

    pub fn line_start(&self, line: usize) -> ByteOffset {
        ByteOffset(
            self.rope
                .line_to_byte(line.min(self.line_count().saturating_sub(1))),
        )
    }

    /// Byte range excluding the line terminator.  CRLF is excluded as one
    /// indivisible terminator, as are Ropey's supported Unicode line breaks.
    pub fn line_content_range(&self, line: usize) -> Range<ByteOffset> {
        let line = line.min(self.line_count().saturating_sub(1));
        let start = self.rope.line_to_byte(line);
        let raw = self.rope.line(line);
        let content_len = raw.len_bytes().saturating_sub(rope_line_ending_len(raw));
        ByteOffset(start)..ByteOffset(start + content_len)
    }

    pub fn line_content(&self, line: usize) -> String {
        let range = self.line_content_range(line);
        self.rope.byte_slice(range.start.0..range.end.0).to_string()
    }

    pub fn line_ending(&self, line: usize) -> String {
        let line = line.min(self.line_count().saturating_sub(1));
        let raw = self.rope.line(line);
        let ending = rope_line_ending_len(raw);
        raw.byte_slice(raw.len_bytes().saturating_sub(ending)..raw.len_bytes())
            .to_string()
    }

    pub fn first_non_whitespace(&self, line: usize) -> ByteOffset {
        let range = self.line_content_range(line);
        let text = self.line_content(line);
        let relative = text
            .char_indices()
            .find_map(|(index, character)| (!character.is_whitespace()).then_some(index))
            .unwrap_or(0);
        ByteOffset(range.start.0 + relative)
    }

    pub fn previous_grapheme_boundary(
        &self,
        offset: ByteOffset,
    ) -> Result<ByteOffset, DocumentError> {
        self.validate_offset(offset)?;
        if offset == ByteOffset::ZERO {
            return Ok(offset);
        }
        let line = self.line_of(offset)?;
        let range = self.line_content_range(line);
        if offset <= range.start {
            if line == 0 {
                return Ok(ByteOffset::ZERO);
            }
            return Ok(self.line_content_range(line - 1).end);
        }
        let text = self.line_content(line);
        let relative = offset.0.min(range.end.0).saturating_sub(range.start.0);
        let previous = text
            .grapheme_indices(true)
            .map(|(index, _)| index)
            .take_while(|index| *index < relative)
            .last()
            .unwrap_or(0);
        Ok(ByteOffset(range.start.0 + previous))
    }

    pub fn next_grapheme_boundary(&self, offset: ByteOffset) -> Result<ByteOffset, DocumentError> {
        self.validate_offset(offset)?;
        if offset.0 == self.len_bytes() {
            return Ok(offset);
        }
        let line = self.line_of(offset)?;
        let range = self.line_content_range(line);
        if offset < range.end {
            let text = self.line_content(line);
            let relative = offset.0.saturating_sub(range.start.0);
            let next = text
                .grapheme_indices(true)
                .map(|(index, _)| index)
                .find(|index| *index > relative)
                .unwrap_or(text.len());
            return Ok(ByteOffset(range.start.0 + next));
        }
        if line + 1 < self.line_count() {
            return Ok(self.line_start(line + 1));
        }
        Ok(ByteOffset(self.len_bytes()))
    }

    pub fn previous_group_boundary(
        &self,
        mut offset: ByteOffset,
    ) -> Result<ByteOffset, DocumentError> {
        self.validate_offset(offset)?;
        if offset == ByteOffset::ZERO {
            return Ok(offset);
        }
        // Whitespace belongs to the gap before the preceding group.
        while offset > ByteOffset::ZERO {
            let previous = self.previous_grapheme_boundary(offset)?;
            if self.cluster_class(previous, offset)? != ClusterClass::Space {
                break;
            }
            offset = previous;
        }
        if offset == ByteOffset::ZERO {
            return Ok(offset);
        }
        let previous = self.previous_grapheme_boundary(offset)?;
        let class = self.cluster_class(previous, offset)?;
        offset = previous;
        while offset > ByteOffset::ZERO {
            let candidate = self.previous_grapheme_boundary(offset)?;
            if self.cluster_class(candidate, offset)? != class {
                break;
            }
            offset = candidate;
        }
        Ok(offset)
    }

    pub fn next_group_boundary(&self, mut offset: ByteOffset) -> Result<ByteOffset, DocumentError> {
        self.validate_offset(offset)?;
        let end = ByteOffset(self.len_bytes());
        if offset == end {
            return Ok(offset);
        }
        let next = self.next_grapheme_boundary(offset)?;
        let class = self.cluster_class(offset, next)?;
        offset = next;
        while offset < end {
            let candidate = self.next_grapheme_boundary(offset)?;
            if self.cluster_class(offset, candidate)? != class {
                break;
            }
            offset = candidate;
        }
        // Windows/CodeMirror-style Ctrl-Right lands at the beginning of the
        // next group by consuming the whitespace following the current one.
        if class != ClusterClass::Space {
            while offset < end {
                let candidate = self.next_grapheme_boundary(offset)?;
                if self.cluster_class(offset, candidate)? != ClusterClass::Space {
                    break;
                }
                offset = candidate;
            }
        }
        Ok(offset)
    }

    pub fn word_range(&self, offset: ByteOffset) -> Result<Range<ByteOffset>, DocumentError> {
        self.validate_offset(offset)?;
        let end = ByteOffset(self.len_bytes());
        if end == ByteOffset::ZERO {
            return Ok(ByteOffset::ZERO..ByteOffset::ZERO);
        }
        let seed_start = if offset == end {
            self.previous_grapheme_boundary(offset)?
        } else {
            offset
        };
        let seed_end = self.next_grapheme_boundary(seed_start)?;
        let class = self.cluster_class(seed_start, seed_end)?;
        let mut start = seed_start;
        while start > ByteOffset::ZERO {
            let candidate = self.previous_grapheme_boundary(start)?;
            if self.cluster_class(candidate, start)? != class {
                break;
            }
            start = candidate;
        }
        let mut finish = seed_end;
        while finish < end {
            let candidate = self.next_grapheme_boundary(finish)?;
            if self.cluster_class(finish, candidate)? != class {
                break;
            }
            finish = candidate;
        }
        Ok(start..finish)
    }

    fn cluster_class(
        &self,
        start: ByteOffset,
        end: ByteOffset,
    ) -> Result<ClusterClass, DocumentError> {
        let cluster = self.slice(start..end)?;
        if cluster.chars().all(char::is_whitespace) {
            Ok(ClusterClass::Space)
        } else if cluster
            .chars()
            .any(|character| character == '_' || character.is_alphanumeric())
        {
            Ok(ClusterClass::Word)
        } else {
            Ok(ClusterClass::Punctuation)
        }
    }

    /// Apply sorted, non-overlapping pre-document edits as one revision.
    /// Returns the removed text paired with each edit in ascending order.
    pub(crate) fn apply_edits(&mut self, edits: &[Edit]) -> Result<Vec<String>, DocumentError> {
        validate_edits(self, edits)?;
        let removed = edits
            .iter()
            .map(|edit| self.slice(edit.range.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        let removed_bytes = edits
            .iter()
            .map(|edit| edit.range.end.0 - edit.range.start.0)
            .sum::<usize>();
        let inserted_bytes = edits.iter().map(|edit| edit.insert.len()).sum::<usize>();
        let attempted = self
            .len_bytes()
            .checked_sub(removed_bytes)
            .and_then(|bytes| bytes.checked_add(inserted_bytes))
            .unwrap_or(usize::MAX);
        if attempted > self.maximum_bytes {
            return Err(DocumentError::DocumentTooLarge {
                attempted,
                maximum: self.maximum_bytes,
            });
        }
        for edit in edits.iter().rev() {
            let chars = self.validate_range(&edit.range)?;
            self.rope.remove(chars.clone());
            self.rope.insert(chars.start, &edit.insert);
        }
        if !edits.is_empty() {
            self.revision.0 = self.revision.0.wrapping_add(1);
        }
        Ok(removed)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClusterClass {
    Space,
    Word,
    Punctuation,
}

pub(crate) fn validate_edits(document: &Document, edits: &[Edit]) -> Result<(), DocumentError> {
    let mut previous_end = ByteOffset::ZERO;
    for (index, edit) in edits.iter().enumerate() {
        document.validate_range(&edit.range)?;
        if index > 0 && edit.range.start < previous_end {
            return Err(DocumentError::OverlappingEdits);
        }
        previous_end = edit.range.end;
    }
    Ok(())
}

fn rope_line_ending_len(line: RopeSlice<'_>) -> usize {
    let character_count = line.len_chars();
    if character_count == 0 {
        return 0;
    }
    match line.char(character_count - 1) {
        '\n' if character_count >= 2 && line.char(character_count - 2) == '\r' => 2,
        character
            if matches!(
                character,
                '\n' | '\r' | '\u{000B}' | '\u{000C}' | '\u{0085}' | '\u{2028}' | '\u{2029}'
            ) =>
        {
            character.len_utf8()
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(text: &str) -> Document {
        Document::new(text, 1024 * 1024).unwrap()
    }

    #[test]
    fn byte_offsets_reject_the_middle_of_utf8() {
        let document = document("a😀z");
        assert_eq!(
            document.validate_offset(ByteOffset(2)),
            Err(DocumentError::NotUtf8Boundary(2))
        );
        assert!(document.validate_offset(ByteOffset(5)).is_ok());
    }

    #[test]
    fn grapheme_motion_keeps_combining_and_zwj_sequences_whole() {
        let text = "e\u{301}👨‍👩‍👧‍👦x";
        let document = document(text);
        let first = document.next_grapheme_boundary(ByteOffset::ZERO).unwrap();
        assert_eq!(first.0, "e\u{301}".len());
        let second = document.next_grapheme_boundary(first).unwrap();
        assert_eq!(second.0, "e\u{301}👨‍👩‍👧‍👦".len());
        assert_eq!(document.previous_grapheme_boundary(second).unwrap(), first);
    }

    #[test]
    fn crlf_is_one_navigation_step() {
        let document = document("ab\r\ncd");
        assert_eq!(
            document.next_grapheme_boundary(ByteOffset(2)).unwrap(),
            ByteOffset(4)
        );
        assert_eq!(
            document.previous_grapheme_boundary(ByteOffset(4)).unwrap(),
            ByteOffset(2)
        );
        assert_eq!(document.line_content_range(0), ByteOffset(0)..ByteOffset(2));
    }

    #[test]
    fn carets_cannot_split_a_grapheme_or_crlf() {
        let document = document("e\u{301}\r\nx");
        assert_eq!(
            document.validate_caret_offset(ByteOffset(1)),
            Err(DocumentError::NotCaretBoundary(1))
        );
        let carriage_return_end = "e\u{301}\r".len();
        assert_eq!(
            document.validate_caret_offset(ByteOffset(carriage_return_end)),
            Err(DocumentError::NotCaretBoundary(carriage_return_end))
        );
        assert!(
            document
                .validate_caret_offset(ByteOffset("e\u{301}".len()))
                .is_ok()
        );
        assert!(
            document
                .validate_caret_offset(ByteOffset("e\u{301}\r\n".len()))
                .is_ok()
        );
    }

    #[test]
    fn edits_are_atomic_and_bounded() {
        let mut document = Document::new("abcdef", 8).unwrap();
        let removed = document
            .apply_edits(&[
                Edit::new(ByteOffset(1)..ByteOffset(2), "XX"),
                Edit::new(ByteOffset(4)..ByteOffset(6), ""),
            ])
            .unwrap();
        assert_eq!(removed, ["b", "ef"]);
        assert_eq!(document.text(), "aXXcd");
        assert_eq!(document.revision(), Revision(1));
        let before = document.text();
        assert!(matches!(
            document.apply_edits(&[Edit::new(ByteOffset(0)..ByteOffset(0), "1234")]),
            Err(DocumentError::DocumentTooLarge { .. })
        ));
        assert_eq!(document.text(), before);
    }

    #[test]
    fn group_motion_uses_unicode_words_and_punctuation() {
        let document = document("alpha βeta  .. next");
        assert_eq!(
            document.next_group_boundary(ByteOffset(0)).unwrap(),
            ByteOffset(6)
        );
        let next = ByteOffset("alpha βeta  .. ".len());
        assert_eq!(
            document.previous_group_boundary(next).unwrap(),
            ByteOffset("alpha βeta  ".len())
        );
    }
}
