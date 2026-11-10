//! The number form both the quick action and the knobs panel edit with:
//! four fields - value, min, max, step - one caret, one selection, and
//! digits only. One definition, because a caret placed from a second one
//! drifts, and a caret one cell off its text is a field you cannot type
//! the end of.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use unicode_width::UnicodeWidthStr;

/// A form row is a marker, then a label, then the value. Both the drawing
/// and the caret are reckoned from these - one definition, because a caret
/// placed from a second one drifts, and a caret one cell off its text is a
/// field you cannot type the end of.
pub(crate) const FIELD_MARKER: usize = 2;
pub(crate) const FIELD_LABEL: usize = 6;

/// The screen column a form field's value starts at. The renderer insets
/// each row by one from the popup's left edge.
pub(crate) fn field_value_x(area: ratatui::layout::Rect) -> u16 {
    area.x + 1 + (FIELD_MARKER + FIELD_LABEL) as u16
}

/// The most a form field will hold. A slider bound is a number, not an
/// essay, and an unbounded paste into a caret-anchored popup is a way to
/// make the popup unreadable.
pub(crate) const FIELD_CHARS: usize = 24;

/// Preserve the actual number: the text and the engine must receive the
/// same value, and a negative zero is written as the zero it is.
pub(crate) fn format_value(value: f64) -> String {
    if value == 0.0 {
        "0".to_owned()
    } else {
        value.to_string()
    }
}

/// A value in the form a score writes a plugin parameter: 3 decimals at
/// most. A rail shows its value this way too, so a step of a third reads
/// `0.333`, not every digit the float holds. A negative that rounds to
/// zero writes the zero it is, as `format_value` does.
pub(crate) fn short_number(value: f64) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    let text = format!("{value:.3}");
    let short = text.trim_end_matches('0').trim_end_matches('.');
    if short == "-0" {
        "0".to_owned()
    } else {
        short.to_owned()
    }
}

/// One number field of the slider form.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Field {
    Value,
    Min,
    Max,
    Step,
}

impl Field {
    pub(crate) const ALL: [Self; 4] = [Self::Value, Self::Min, Self::Max, Self::Step];

    /// Where this field's text sits in the form's `fields`.
    const fn index(self) -> usize {
        self as usize
    }

    fn label(self) -> &'static str {
        match self {
            Self::Value => "value",
            Self::Min => "min",
            Self::Max => "max",
            Self::Step => "step",
        }
    }
}

/// The form the quick action's menu becomes.
#[derive(Clone, Debug)]
pub(crate) struct SliderForm {
    pub(crate) fields: [String; 4],
    /// Where the caret is in the field under the keys, in bytes.
    pub(crate) cursor: usize,
    /// The whole field is selected: the next typed character replaces it,
    /// the way a freshly focused number input behaves. Erase keys edit
    /// from the caret instead; the selection is never drawn.
    pub(crate) selected: bool,
    pub(crate) field: usize,
    /// What the last attempt to write it said, if it could not be written.
    pub(crate) error: Option<String>,
    /// It has been written to the score at least once, so Esc leaves the
    /// fader rather than the number it came from.
    pub(crate) written: bool,
}

impl SliderForm {
    pub(crate) fn text(&self) -> &str {
        &self.fields[self.field.min(3)]
    }

    pub(crate) fn text_mut(&mut self) -> &mut String {
        let field = self.field.min(3);
        &mut self.fields[field]
    }

    pub(crate) fn insert(&mut self, text: &str) {
        if !text
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))
        {
            return;
        }
        if self.selected {
            self.text_mut().clear();
            self.cursor = 0;
            self.selected = false;
        }
        let room = FIELD_CHARS.saturating_sub(self.text().chars().count());
        let text: String = text.chars().take(room).collect();
        let cursor = self.cursor;
        self.text_mut().insert_str(cursor, &text);
        self.cursor += text.len();
    }

    pub(crate) fn erase(&mut self, backwards: bool) {
        // The form draws its caret, never its selection, so an erase key
        // edits from the caret one character at a time. The selection goes
        // with the erase: a field someone has trimmed is no longer the
        // fresh default a typed character replaces.
        self.selected = false;
        if backwards && self.cursor > 0 {
            self.cursor -= 1;
            let cursor = self.cursor;
            self.text_mut().remove(cursor);
        } else if !backwards && self.cursor < self.text().len() {
            let cursor = self.cursor;
            self.text_mut().remove(cursor);
        }
    }

    /// Put new text in a field. The field under the keys is selected whole
    /// again, the way a freshly focused field is: a caret left where it was
    /// can stand past the end of a shorter number, and the next frame
    /// slices the caret's position out of that text.
    pub(crate) fn set_field(&mut self, field: Field, text: String) {
        self.fields[field.index()] = text;
        if self.field == field.index() {
            self.focus(field.index());
        }
    }

    /// Move to another field, with the whole of it selected - Tab out of
    /// a number field and back in should not leave the caret mid-digit.
    pub(crate) fn focus(&mut self, field: usize) {
        self.field = field.min(Field::ALL.len() - 1);
        self.cursor = self.text().len();
        self.selected = true;
    }

    /// The field navigation every form of this kind shares: Tab, Shift+Tab,
    /// ↑ and ↓ move between the fields, wrapping. Returns true when the key
    /// was one of them.
    pub(crate) fn navigate(&mut self, key: &KeyEvent) -> bool {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let forwards = match key.code {
            KeyCode::Tab => !shift,
            KeyCode::BackTab => false,
            KeyCode::Down => true,
            KeyCode::Up => false,
            _ => return false,
        };
        let len = Field::ALL.len();
        let next = (self.field + if forwards { 1 } else { len - 1 }) % len;
        self.focus(next);
        true
    }

    /// The four numbers, if they are four numbers.
    pub(crate) fn parse(&self) -> Result<[f64; 4], String> {
        let mut out = [0.0; 4];
        for (at, field) in Field::ALL.iter().enumerate() {
            let text = self.fields[at].trim();
            match text.parse::<f64>() {
                Ok(number) if number.is_finite() => out[at] = number,
                _ => return Err(format!("{} is not a number", field.label())),
            }
        }
        Ok(out)
    }

    /// The form's rows: each field's marker, label and value. The caller
    /// adds the row that says what Enter does.
    pub(crate) fn rows(&self) -> Vec<String> {
        Field::ALL
            .iter()
            .enumerate()
            .map(|(at, field)| {
                // The marker is padded to its own width rather than
                // assumed to be one cell: a glyph a terminal draws wide
                // would shift the whole row out from under the caret.
                let marker = if at == self.field { "\u{25b8}" } else { " " };
                let pad = FIELD_MARKER.saturating_sub(UnicodeWidthStr::width(marker));
                format!(
                    "{marker}{:pad$}{:<FIELD_LABEL$}{}",
                    "",
                    field.label(),
                    self.fields[at]
                )
            })
            .collect()
    }

    /// A key at the field under the keys: caret moves, erase keys and
    /// digits. Field order, Enter and Esc stay with the caller, whose
    /// surfaces differ in what they do with them. Returns whether the key
    /// belonged to the field.
    pub(crate) fn key(&mut self, key: &KeyEvent) -> bool {
        let primary = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char('a' | 'A') if primary => {
                self.selected = true;
                true
            }
            KeyCode::Left if !alt && !primary => {
                self.cursor = if self.selected {
                    0
                } else {
                    self.cursor.saturating_sub(1)
                };
                self.selected = false;
                true
            }
            KeyCode::Right if !alt && !primary => {
                self.cursor = if self.selected {
                    self.text().len()
                } else {
                    (self.cursor + 1).min(self.text().len())
                };
                self.selected = false;
                true
            }
            KeyCode::Home => {
                self.cursor = 0;
                self.selected = false;
                true
            }
            KeyCode::End => {
                self.cursor = self.text().len();
                self.selected = false;
                true
            }
            KeyCode::Backspace => {
                self.erase(true);
                true
            }
            KeyCode::Delete => {
                self.erase(false);
                true
            }
            KeyCode::Char(character) if !primary && !alt => {
                self.insert(&character.to_string());
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three decimals at most, and no trailing zeros.
    #[test]
    fn a_short_number_keeps_three_decimals() {
        assert_eq!(short_number(1.0 / 3.0), "0.333");
        assert_eq!(short_number(1.0), "1");
        assert_eq!(short_number(0.5), "0.5");
        assert_eq!(short_number(812.6543), "812.654");
        assert_eq!(short_number(-0.0), "0");
        assert_eq!(short_number(-0.0004), "0");
    }
}
