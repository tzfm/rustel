//! The bracket under the caret and its partner, as CodeMirror shows them.
//!
//! With the caret just before or just after one of `()`, `[]`, `{}` or
//! `<>` (mini-notation's alternation), both halves light up; a bracket
//! with no partner lights up alone, in the error colour, which is how a
//! missing one is found before the linter says so.

/// One bracket the caret is on, and where its partner is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BracketMatch {
    /// Byte offset of the bracket at the caret.
    pub at: usize,
    /// Byte offset of its partner, when it has one.
    pub partner: Option<usize>,
}

impl BracketMatch {
    /// The ranges to paint, each one byte wide, with whether they matched.
    pub fn ranges(self) -> Vec<(usize, usize, bool)> {
        match self.partner {
            Some(partner) => vec![(self.at, self.at + 1, true), (partner, partner + 1, true)],
            None => vec![(self.at, self.at + 1, false)],
        }
    }
}

const PAIRS: [(u8, u8); 4] = [(b'(', b')'), (b'[', b']'), (b'{', b'}'), (b'<', b'>')];

fn pair_of(byte: u8) -> Option<(u8, u8, bool)> {
    PAIRS.iter().find_map(|&(open, close)| {
        if byte == open {
            Some((open, close, true))
        } else if byte == close {
            Some((open, close, false))
        } else {
            None
        }
    })
}

/// `text` is the document (or a window of it, with `base` its byte offset
/// in the document) and `caret` a byte offset into the document.
///
/// The bracket after the caret wins over the one before it, as in
/// CodeMirror. Quotes are not understood: a bracket inside a mini-notation
/// string is a real bracket, and matching those is most of the point.
pub fn bracket_at(text: &str, base: usize, caret: usize) -> Option<BracketMatch> {
    let bytes = text.as_bytes();
    let local = caret.checked_sub(base)?;
    let candidate = std::iter::once(local)
        .chain(local.checked_sub(1))
        .find(|&index| index < bytes.len() && pair_of(bytes[index]).is_some())?;
    let (open, close, forwards) = pair_of(bytes[candidate])?;
    let mut depth = 0usize;
    let partner = if forwards {
        bytes[candidate..]
            .iter()
            .enumerate()
            .find_map(|(offset, &byte)| {
                if byte == open {
                    depth += 1;
                } else if byte == close {
                    depth -= 1;
                    if depth == 0 {
                        return Some(candidate + offset);
                    }
                }
                None
            })
    } else {
        bytes[..=candidate]
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, &byte)| {
                if byte == close {
                    depth += 1;
                } else if byte == open {
                    depth -= 1;
                    if depth == 0 {
                        return Some(index);
                    }
                }
                None
            })
    };
    Some(BracketMatch {
        at: base + candidate,
        partner: partner.map(|index| base + index),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bracket_at_the_caret_finds_its_partner_either_way() {
        let text = "s(\"bd [hh <sd cp>]\").lpf(800)";
        let open = text.find('[').unwrap();
        let close = text.find(']').unwrap();
        assert_eq!(
            bracket_at(text, 0, open),
            Some(BracketMatch {
                at: open,
                partner: Some(close)
            }),
            "caret before ["
        );
        assert_eq!(
            bracket_at(text, 0, close + 1),
            Some(BracketMatch {
                at: close,
                partner: Some(open)
            }),
            "caret after ]"
        );
        let lt = text.find('<').unwrap();
        assert_eq!(
            bracket_at(text, 0, lt).unwrap().partner,
            Some(text.find('>').unwrap())
        );
        let paren = text.rfind('(').unwrap();
        assert_eq!(
            bracket_at(text, 0, paren).unwrap().partner,
            Some(text.len() - 1)
        );
        assert_eq!(bracket_at(text, 0, 3), None, "on a letter");
    }

    #[test]
    fn an_orphan_bracket_is_reported_without_a_partner_and_windows_offset() {
        let text = "s(\"bd [hh\")";
        let open = text.find('[').unwrap();
        assert_eq!(
            bracket_at(text, 0, open),
            Some(BracketMatch {
                at: open,
                partner: None
            })
        );
        assert_eq!(
            bracket_at(text, 0, open).unwrap().ranges(),
            vec![(open, open + 1, false)]
        );
        // A window starting at byte 100 of a larger document.
        let m = bracket_at("(x)", 100, 100).unwrap();
        assert_eq!((m.at, m.partner), (100, Some(102)));
        assert_eq!(m.ranges(), vec![(100, 101, true), (102, 103, true)]);
        // After the caret wins over before: `)(` with the caret between.
        let m = bracket_at("()()", 0, 2).unwrap();
        assert_eq!((m.at, m.partner), (2, Some(3)));
    }
}
