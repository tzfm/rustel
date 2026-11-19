//! The value of a JavaScript string literal.

use std::borrow::Cow;

/// The value of the next `count` hexadecimal digits of `chars`, which it then
/// leaves behind. `None`, with `chars` as it was, for anything else.
pub fn take_hex(
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
    count: usize,
) -> Option<u32> {
    let mut probe = chars.clone();
    let mut value = 0u32;
    for _ in 0..count {
        let digit = probe.next()?.1.to_digit(16)?;
        value = value * 16 + digit;
    }
    *chars = probe;
    Some(value)
}

/// Read the `{...}` form of a `\u` escape, positioned just after the `u`.
fn take_braced_hex(chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>) -> Option<u32> {
    let mut probe = chars.clone();
    if probe.next()?.1 != '{' {
        return None;
    }
    let mut value = 0u32;
    let mut digits = 0;
    loop {
        match probe.next().map(|(_, ch)| ch) {
            Some('}') if digits > 0 => break,
            Some(ch) => {
                value = value.checked_mul(16)?.checked_add(ch.to_digit(16)?)?;
                digits += 1;
                if digits > 6 {
                    return None;
                }
            }
            None => return None,
        }
    }
    *chars = probe;
    Some(value)
}

fn is_high_surrogate(value: u32) -> bool {
    (0xD800..=0xDBFF).contains(&value)
}

fn is_low_surrogate(value: u32) -> bool {
    (0xDC00..=0xDFFF).contains(&value)
}

/// Why a string literal has no value.
#[derive(Debug, PartialEq, Eq)]
pub struct DecodeError {
    pub message: String,
    /// The byte of the literal's body where the fault starts.
    pub offset: usize,
}

/// The value of a JavaScript string literal with the body `raw`. A template
/// keeps a line break, as `allow_line_breaks` says. A body with no escape
/// is its own value, and is not copied.
pub fn decode(raw: &str, allow_line_breaks: bool) -> Result<Cow<'_, str>, DecodeError> {
    // A carriage return becomes a line feed, and a line feed is an error
    // where no line break is allowed.
    let as_written = |byte: u8| match byte {
        b'\\' | b'\r' => false,
        b'\n' => allow_line_breaks,
        _ => true,
    };
    if raw.bytes().all(as_written) {
        return Ok(Cow::Borrowed(raw));
    }
    let mut output = String::new();
    let mut chars = raw.char_indices().peekable();
    while let Some((offset, ch)) = chars.next() {
        if ch != '\\' {
            if matches!(ch, '\n' | '\r') {
                // A raw line break is a syntax error inside '' or "", but a
                // template literal keeps it - and real scores rely on that to
                // lay a long phrase out over several lines.
                if !allow_line_breaks {
                    return Err(DecodeError {
                        message: "unescaped line break in mini string".into(),
                        offset,
                    });
                }
                // JavaScript normalises CRLF and lone CR to LF in template
                // literal values. Mini treats any of them as whitespace, but
                // matching the spec keeps offsets and values predictable.
                if ch == '\r' {
                    if chars.peek().map(|(_, next)| *next) == Some('\n') {
                        chars.next();
                    }
                    output.push('\n');
                } else {
                    output.push(ch);
                }
                continue;
            }
            output.push(ch);
            continue;
        }
        match chars.next().map(|(_, ch)| ch) {
            Some('n') => output.push('\n'),
            Some('r') => output.push('\r'),
            Some('t') => output.push('\t'),
            Some('b') => output.push('\u{0008}'),
            Some('f') => output.push('\u{000c}'),
            Some('v') => output.push('\u{000b}'),
            // `\0` is NUL only when no decimal digit follows.
            Some('0') if !chars.peek().is_some_and(|(_, ch)| ch.is_ascii_digit()) => {
                output.push('\0');
            }
            Some('0'..='9') => {
                return Err(DecodeError {
                    message: "legacy numeric escape is not allowed in mini string".into(),
                    offset,
                });
            }
            // A backslash before a line terminator is a line continuation: the
            // break is removed and contributes nothing.
            Some('\n') | Some('\u{2028}') | Some('\u{2029}') => {}
            Some('\r') => {
                if chars.peek().is_some_and(|(_, ch)| *ch == '\n') {
                    chars.next();
                }
            }
            Some('x') => {
                let value = take_hex(&mut chars, 2).ok_or_else(|| DecodeError {
                    message: "invalid hexadecimal escape in mini string".into(),
                    offset,
                })?;
                output.push(char::from_u32(value).expect("two hex digits form a scalar value"));
            }
            Some('u') => decode_unicode_escape(&mut chars, &mut output)
                .map_err(|message| DecodeError { message, offset })?,
            Some(ch) => output.push(ch),
            None => {
                return Err(DecodeError {
                    message: "unterminated escape in mini string".into(),
                    offset,
                });
            }
        }
    }
    Ok(Cow::Owned(output))
}

/// Decode one `\u` escape, positioned just after the `u`.
///
/// Surrogate pairs become one scalar value. A lone surrogate is refused
/// because it has no UTF-8 representation for the native mini parser.
fn decode_unicode_escape(
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
    output: &mut String,
) -> Result<(), String> {
    let value = take_braced_hex(chars)
        .or_else(|| take_hex(chars, 4))
        .ok_or_else(|| "invalid Unicode escape in mini string".to_string())?;
    if let Some(decoded) = char::from_u32(value) {
        output.push(decoded);
        return Ok(());
    }
    if is_high_surrogate(value) {
        let mut probe = chars.clone();
        if probe.next().map(|(_, ch)| ch) == Some('\\')
            && probe.next().map(|(_, ch)| ch) == Some('u')
            && let Some(low) = take_hex(&mut probe, 4).filter(|low| is_low_surrogate(*low))
            && let Some(decoded) =
                char::from_u32(0x10000 + ((value - 0xD800) << 10) + (low - 0xDC00))
        {
            output.push(decoded);
            *chars = probe;
            return Ok(());
        }
    }
    if is_high_surrogate(value) || is_low_surrogate(value) {
        Err("lone UTF-16 surrogate in mini string".into())
    } else {
        Err("Unicode escape is outside the valid code-point range".into())
    }
}

#[cfg(test)]
mod tests {
    use super::decode;

    #[test]
    fn string_escapes_decode_the_way_javascript_reads_them() {
        for (escaped, decoded) in [
            (r"bdd x", "bdd x"),
            (r"bd\x64 x", "bdd x"),
            (r"bd\u0064 x", "bdd x"),
            // JavaScript strings are UTF-16, so an astral character is written
            // as a surrogate PAIR and only means anything combined.
            (r"a\ud83d\ude00b", "a\u{1F600}b"),
            (r"a\u{1F600}b", "a\u{1F600}b"),
            (r"a\vb", "a\u{000b}b"),
            (r"a\0b", "a\0b"),
            (r"a\nb", "a\nb"),
            (r"a\tb", "a\tb"),
            // An unrecognised escape really does just drop the backslash.
            (r"a\qb", "aqb"),
        ] {
            assert_eq!(
                decode(escaped, false).expect("valid JavaScript escape"),
                decoded,
                "{escaped:?} did not decode as JavaScript reads it"
            );
        }

        // A backslash before a line terminator is a continuation: it and the
        // break contribute nothing.
        assert_eq!(decode("a\\\nb", false).unwrap(), "ab");

        // A raw line break is a syntax error in '' and "", but a template
        // literal keeps it, and real scores lay long phrases out over several
        // lines.
        assert!(decode("c3 e3\ng3 b3", false).is_err());
        assert_eq!(decode("c3 e3\ng3 b3", true).unwrap(), "c3 e3\ng3 b3");
        // JavaScript normalises CRLF and lone CR to LF in a template literal.
        assert_eq!(decode("a\r\nb", true).unwrap(), "a\nb");
        assert_eq!(decode("a\rb", true).unwrap(), "a\nb");
        assert_eq!(decode("a\\\r\nb", false).unwrap(), "ab");
    }
}
