/// Whether writing the character to a terminal could start an escape
/// sequence, move the cursor or reorder the text around it.
pub fn is_unsafe_terminal_character(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{202A}'
                ..='\u{202E}'
                    | '\u{2066}'..='\u{2069}'
                    | '\u{200E}'
                    | '\u{200F}'
                    | '\u{061C}'
        )
}

/// The visible stand-in for an unsafe character: the Unicode control picture
/// for C0 controls and DEL, `�` for anything else.
pub fn control_picture(character: char) -> char {
    match character {
        '\0'..='\u{001F}' => char::from_u32(0x2400 + u32::from(character)).unwrap_or('�'),
        '\u{007F}' => '␡',
        _ => '�',
    }
}

/// The text with every unsafe character replaced by its visible stand-in,
/// so it cannot drive the terminal it is written to.
pub fn visible(text: &str) -> String {
    text.chars()
        .map(|character| {
            if is_unsafe_terminal_character(character) {
                control_picture(character)
            } else {
                character
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_line_text_shows_every_control() {
        assert_eq!(visible("a\tb\nc\u{7f}\u{9b}\u{202e}"), "a␉b␊c␡��");
    }
}
