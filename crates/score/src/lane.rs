//! The lanes of a score text.

use crate::reader::hits;

/// Normalized body text of a lane read directly from a score source: either a
/// zero-copy byte range of the score source when already normalized, or an
/// owned normalized string when line endings or trailing blanks changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaneBodySource {
    Slice(u32, u32),
    Owned(String),
}

impl LaneBodySource {
    /// Build from `source[start..end]`, storing byte offsets without allocating
    /// when the slice is already in canonical lane-body form.
    pub fn from_range(source: &str, start: usize, end: usize) -> Self {
        let code = &source[start..end];
        if LaneStatement::is_a_body(code)
            && let (Ok(s), Ok(e)) = (u32::try_from(start), u32::try_from(end))
        {
            Self::Slice(s, e)
        } else {
            Self::Owned(LaneStatement::body_of(code))
        }
    }

    #[inline]
    pub fn as_str<'a>(&'a self, source: &'a str) -> &'a str {
        match self {
            Self::Slice(start, end) => &source[*start as usize..*end as usize],
            Self::Owned(owned) => owned.as_str(),
        }
    }
}

/// A lane read from a score source without allocating strings for verbatim
/// labels or normalized bodies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadLane {
    pub label: (u32, u32),
    pub body: LaneBodySource,
    pub line: usize,
}

impl ReadLane {
    #[inline]
    pub fn label_str<'a>(&self, source: &'a str) -> &'a str {
        &source[self.label.0 as usize..self.label.1 as usize]
    }

    #[inline]
    pub fn body_str<'a>(&'a self, source: &'a str) -> &'a str {
        self.body.as_str(source)
    }

    pub fn to_statement(&self, source: &str) -> LaneStatement {
        LaneStatement {
            label: self.label_str(source).to_owned(),
            body: self.body_str(source).to_owned(),
            line: self.line,
        }
    }
}

/// Inline-optimized list of [`ReadLane`]s avoiding a heap `Vec` for empty and
/// single-lane scores.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ReadLanes {
    #[default]
    Empty,
    One(ReadLane),
    Many(Vec<ReadLane>),
}

impl ReadLanes {
    #[inline]
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    #[inline]
    pub fn len(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::One(_) => 1,
            Self::Many(lanes) => lanes.len(),
        }
    }

    #[inline]
    pub fn as_slice(&self) -> &[ReadLane] {
        match self {
            Self::Empty => &[],
            Self::One(lane) => std::slice::from_ref(lane),
            Self::Many(lanes) => lanes.as_slice(),
        }
    }

    pub fn push(&mut self, lane: ReadLane) {
        match std::mem::take(self) {
            Self::Empty => *self = Self::One(lane),
            Self::One(first) => *self = Self::Many(vec![first, lane]),
            Self::Many(mut lanes) => {
                lanes.push(lane);
                *self = Self::Many(lanes);
            }
        }
    }

    pub fn to_statements(&self, source: &str) -> Vec<LaneStatement> {
        self.as_slice()
            .iter()
            .map(|lane| lane.to_statement(source))
            .collect()
    }
}

/// A lane of a score, as a reader of the text lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaneStatement {
    /// The label as written: `$`, `_$`, `lead`, `Slead`. Empty for a score
    /// with no label.
    pub label: String,
    /// The code of the statement after its label, with no comment before or
    /// after it. Each line end is `\n` and no line has whitespace at its
    /// end, so a score saved with different line ends has the same body.
    pub body: String,
    /// The line of the label, counted from 1. With no label, the line where
    /// the body starts.
    pub line: usize,
}

impl LaneStatement {
    /// The body of a lane whose statement is `code`: `code` with `\n` for
    /// each line end and no whitespace at the end of a line.
    pub fn body_of(code: &str) -> String {
        const LINE_ENDS: [char; 4] = ['\n', '\r', '\u{2028}', '\u{2029}'];
        if Self::is_a_body(code) {
            return code.to_owned();
        }
        let mut body = String::with_capacity(code.len());
        let mut rest = code;
        while let Some(end) = rest.find(LINE_ENDS) {
            body.push_str(rest[..end].trim_end());
            body.push('\n');
            let after = &rest[end..];
            // `\r\n` is one line end.
            let width = match after.strip_prefix("\r\n") {
                Some(_) => 2,
                None => after.chars().next().map_or(0, char::len_utf8),
            };
            rest = &after[width..];
        }
        body.push_str(rest.trim_end());
        body
    }

    /// Whether `code` is the body that [`Self::body_of`] makes of it, which
    /// one pass over its bytes says: text in ASCII, with `\n` for each line
    /// end and no whitespace at the end of a line.
    fn is_a_body(code: &str) -> bool {
        let blank = |byte: u8| matches!(byte, b' ' | b'\t' | 0x0B | 0x0C);
        // Words of eight bytes read most of a body at once: each byte
        // outside ASCII, each `\r`, and each `\n`, whose byte before it
        // alone needs its own look.
        let bytes = code.as_bytes();
        let high = 0x8080_8080_8080_8080u64;
        let (chunks, rest) = bytes.as_chunks::<8>();
        let mut before = 0u8;
        let mut at = 0;
        for chunk in chunks {
            let word = u64::from_le_bytes(*chunk);
            if word & high != 0 || hits(word, b'\r') != 0 {
                return false;
            }
            let mut feeds = hits(word, b'\n');
            while feeds != 0 {
                let place = usize::try_from(feeds.trailing_zeros()).expect("a byte place") / 8;
                // A `\n` at the start of the word reads the byte before
                // the word, which the word before it ended in.
                let ends_a_blank = match place {
                    0 => blank(before),
                    _ => blank(bytes[at + place - 1]),
                };
                if ends_a_blank {
                    return false;
                }
                feeds &= feeds - 1;
            }
            before = bytes[at + 7];
            at += 8;
        }
        for &byte in rest {
            let ends = byte == b'\n';
            if !byte.is_ascii() || byte == b'\r' || (ends && blank(before)) {
                return false;
            }
            before = byte;
        }
        !blank(before)
    }
}

#[cfg(test)]
mod tests {
    use super::LaneStatement;

    #[test]
    fn a_body_has_one_form_for_each_line_end_and_no_space_at_the_end_of_a_line() {
        for (code, body) in [
            ("", ""),
            ("s(\"bd\")", "s(\"bd\")"),
            ("s(\"bd\")  \n  .fast(2)\t", "s(\"bd\")\n  .fast(2)"),
            ("a\r\nb\rc\u{2028}d\u{2029}e", "a\nb\nc\nd\ne"),
            ("a \r\n\r\n b\n", "a\n\n b\n"),
            ("a\n\r\nb", "a\n\nb"),
            ("a\n\n b\n", "a\n\n b\n"),
            ("a \nb", "a\nb"),
            ("a\nb\t", "a\nb"),
            ("a\u{b}\nb\u{c}", "a\nb"),
            ("a\u{a0}\nb\u{3000}", "a\nb"),
        ] {
            assert_eq!(LaneStatement::body_of(code), body, "{code:?}");
        }
    }
}
