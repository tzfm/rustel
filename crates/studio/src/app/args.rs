//! The arguments of a call in the score, as text: the members of a
//! bracketed list, the plain form of a property key, and the edit that
//! puts a named parameter into a call's options. A filter's object and
//! a plugin's parameters are both a call's arguments, so no feature
//! gates this file.

use std::ops::Range;

/// The separators of one bracketed argument/object list, ending with its
/// closing bracket. `code_only` keeps punctuation in strings and comments
/// from being mistaken for a separator. A typed key sometimes ends at EOF,
/// before the player closes the list.
pub(super) fn members(code: &str, open: usize, unclosed: bool) -> Option<Vec<Range<usize>>> {
    let mut brackets = vec![*code.as_bytes().get(open)?];
    let mut start = open + 1;
    let mut ranges = Vec::new();
    for (at, byte) in code.bytes().enumerate().skip(start) {
        match byte {
            b'(' | b'[' | b'{' => brackets.push(byte),
            b')' | b']' | b'}' => {
                if !matches!(
                    (brackets.pop()?, byte),
                    (b'(', b')') | (b'[', b']') | (b'{', b'}')
                ) {
                    return None;
                }
                if brackets.is_empty() {
                    ranges.push(start..at);
                    return Some(ranges);
                }
            }
            b',' if brackets.len() == 1 => {
                ranges.push(start..at);
                start = at + 1;
            }
            _ => {}
        }
    }
    unclosed.then(|| {
        ranges.push(start..code.len());
        ranges
    })
}

/// A property key as the score writes it: an identifier, or a quoted
/// property with the quotes off. A leading comment sits before either
/// form; trailing comments are blank in the identifier path.
pub(super) fn plain_key(mut key: &str) -> Option<String> {
    loop {
        key = key.trim_start();
        if let Some(comment) = key.strip_prefix("/*") {
            key = comment.get(comment.find("*/")? + 2..)?;
        } else if let Some(comment) = key.strip_prefix("//") {
            key = comment.get(comment.find('\n')?..)?;
        } else {
            break;
        }
    }
    Some(rustel_runtime::sounds::string_argument(key).map_or_else(
        || rustel_runtime::lint::code_only(key).trim().to_owned(),
        |key| key.into_owned(),
    ))
}

/// Add one parameter to the call's options, or select its existing key for
/// an unchanged replacement. Existing values, nested expressions, comments
/// and trailing commas stay intact.
pub(super) fn parameter_edit(
    source: &str,
    open: usize,
    text: &str,
    typed: Option<Range<usize>>,
) -> Option<(Range<usize>, String)> {
    let code = rustel_runtime::lint::code_only(source);
    let args = members(&code, open, typed.is_some())?;
    let key = &text[..rustel_runtime::lint::code_only(text).find(':')?];
    // The options object, wherever the call keeps it: at any member
    // place, beside or between the call's own arguments. A plugin call
    // names the plugin first, and an empty second member is the comma
    // the player has not filled yet - the object grows there.
    let options = args
        .iter()
        .find(|arg| code[arg.start..arg.end].trim_start().starts_with('{'))
        .cloned()
        .or_else(|| {
            args.get(1)
                .filter(|second| code[second.start..second.end].trim().is_empty())
                .cloned()
        });
    let Some(options) = options else {
        // A call with only its name can gain an object beside it: one
        // literal member, its end before any trailing comment.
        if args.len() != 1 {
            return None;
        }
        let name = args.first()?.clone();
        // A call with nothing in it, `_pianoroll()`, takes the object as
        // its one argument.
        if source[name.clone()].trim().is_empty() {
            return Some((name.start..name.start, format!("{{ {text} }}")));
        }
        let literal = source[name.clone()].trim_start();
        // A string literal, the plugin's name: anything else - `lpf(1001)`,
        // `foo(éé)` - is no name to put an object beside.
        let quote = *literal.as_bytes().first()?;
        if !matches!(quote, b'\'' | b'"' | b'`') {
            return None;
        }
        let mut escaped = false;
        let end = literal.bytes().enumerate().skip(1).find_map(|(at, byte)| {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == quote {
                return Some(at + 1);
            }
            None
        })?;
        let at = name.end - literal.len() + end;
        if !code[at..name.end].trim().is_empty() {
            return None;
        }
        return Some((at..at, format!(", {{ {text} }}")));
    };
    let object = code[options.clone()]
        .bytes()
        .position(|byte| !byte.is_ascii_whitespace());
    let Some(object) = object.map(|at| options.start + at) else {
        return Some((options.start..options.start, format!(" {{ {text} }}")));
    };
    if code.as_bytes()[object] != b'{' {
        return None;
    }
    let entries = members(&code, object, typed.is_some())?;
    let last = entries.last()?;
    if last.end < options.end && !code[last.end + 1..options.end].trim().is_empty() {
        return None;
    }
    let typed_entry = typed.as_ref().and_then(|typed| {
        entries
            .iter()
            .position(|entry| entry.start <= typed.start && typed.end <= entry.end)
    });
    let existing = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            let raw = source[(*entry).clone()].trim();
            let colon = code[(*entry).clone()].find(':');
            let existing = colon.map_or(raw, |at| source[entry.start..entry.start + at].trim());
            plain_key(existing).is_some_and(|existing| Some(existing) == plain_key(key))
        })
        // Prefer a complete member elsewhere over the key being typed.
        .min_by_key(|(index, _)| Some(*index) == typed_entry);
    if let Some((index, entry)) = existing {
        if let Some(typed) = &typed
            && Some(index) == typed_entry
            && !code[entry.clone()].contains(':')
        {
            return Some((typed.clone(), text.to_owned()));
        }
        if typed.as_ref().is_some_and(|typed| !typed.is_empty())
            && let Some(partial) = typed_entry.filter(|partial| *partial != index)
        {
            // The chosen key already has a value elsewhere. Remove
            // the unfinished member and one adjacent comma.
            let range = if partial > 0 {
                entries[partial].start - 1..entries[partial].end
            } else {
                entries[partial].start..entries[partial].end + 1
            };
            return Some((range, String::new()));
        }
        return Some((entry.clone(), source[entry.clone()].to_owned()));
    }
    if let Some(typed) = typed {
        let entry = entries
            .iter()
            .find(|entry| entry.start <= typed.start && typed.end <= entry.end)?;
        let value = code[typed.end..entry.end].trim_start().starts_with(':');
        return Some((typed, if value { key } else { text }.to_owned()));
    }
    // Inserting before an existing member avoids its trailing line comment
    // swallowing the new value and preserves the object's indentation.
    let empty = entries.len() == 1
        && code[last.clone()].trim().is_empty()
        && source[last.clone()].trim().is_empty();
    let rest = &source[object + 1..];
    let leading = &rest[..rest.len() - rest.trim_start().len()];
    let at = object + 1 + leading.len();
    let prefix = if leading.is_empty() { " " } else { "" };
    let suffix = if empty {
        " ".to_owned()
    } else if leading.is_empty() {
        ", ".to_owned()
    } else {
        format!(",{leading}")
    };
    Some((at..at, format!("{prefix}{text}{suffix}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_parameter_creates_or_merges_options_without_replacing_values() {
        for (source, expected) in [
            ("vst('FX')", "vst('FX', { gain: 0.5 })"),
            (
                "vsti('FX' /* a name */)",
                "vsti('FX', { gain: 0.5 } /* a name */)",
            ),
            ("vst('FX',)", "vst('FX', { gain: 0.5 })"),
            ("_pianoroll()", "_pianoroll({ gain: 0.5 })"),
            ("vst('FX', {})", "vst('FX', { gain: 0.5 })"),
            ("vst('FX', { })", "vst('FX', { gain: 0.5 })"),
            ("vst('FX', {wet: .6})", "vst('FX', { gain: 0.5, wet: .6})"),
            (
                "vst('FX', { wet: .6, })",
                "vst('FX', { gain: 0.5, wet: .6, })",
            ),
            (
                "vst('FX', {\n  wet: saw.range(0, 1) // keep\n})",
                "vst('FX', {\n  gain: 0.5,\n  wet: saw.range(0, 1) // keep\n})",
            ),
            (
                "vst('FX', { gain: saw.range(.1, .9) })",
                "vst('FX', { gain: saw.range(.1, .9) })",
            ),
            ("vst('FX', { 'gain': .7 })", "vst('FX', { 'gain': .7 })"),
            ("vst('FX', { \"gain\": .7 })", "vst('FX', { \"gain\": .7 })"),
            ("vst('FX', { gain })", "vst('FX', { gain })"),
            (
                "vst('FX', { /* level */ gain /* value */: .7 })",
                "vst('FX', { /* level */ gain /* value */: .7 })",
            ),
            (
                "vst('FX', { // level\n 'gain': .7 })",
                "vst('FX', { // level\n 'gain': .7 })",
            ),
        ] {
            let open = source.find('(').unwrap();
            let (range, text) = parameter_edit(source, open, "gain: 0.5", None).unwrap();
            let mut result = source.to_owned();
            result.replace_range(range, &text);
            assert_eq!(result, expected, "{source}");
        }
        let source = "vst('FX', { 'gain:left': .7 })";
        let (range, text) = parameter_edit(source, 3, "'gain:left': .5", None).unwrap();
        assert_eq!(&source[range], text);
        for source in [
            "vst('FX', options)",
            "vst('FX', {",
            "vst('FX'",
            "vst('FX', { gain: .7)",
            "vst('FX' + suffix)",
            "vst('FX', {} || other)",
            // No string to put an object beside, and no byte of it to
            // read as a quote.
            "lpf(1001)",
            "foo(éé)",
        ] {
            assert!(
                parameter_edit(source, source.find('(').unwrap(), "gain: 0.5", None).is_none(),
                "{source}"
            );
        }
    }

    #[test]
    fn a_parameter_key_completion_keeps_values_and_removes_duplicate_partial_keys() {
        for (marked, expected) in [
            ("vst('FX', { |ga| })", "vst('FX', { gain: 0.5 })"),
            ("vst('FX', { |ga|", "vst('FX', { gain: 0.5"),
            ("vst('FX', { |gain| })", "vst('FX', { gain: 0.5 })"),
            ("vst('FX', { |ga|: .2 })", "vst('FX', { gain: .2 })"),
            ("vst('FX', { |ga|: .2", "vst('FX', { gain: .2"),
            ("vst('FX', { |ga|: .2 }", "vst('FX', { gain: .2 }"),
            ("vst('FX', { |gain|: .2 })", "vst('FX', { gain: .2 })"),
            ("vst('FX', { |gain|: .2", "vst('FX', { gain: .2"),
            (
                "vst('FX', { |ga|, 'gain': saw.range(0, 1) })",
                "vst('FX', { 'gain': saw.range(0, 1) })",
            ),
            (
                "vst('FX', { |ga|, 'gain': saw.range(0, 1)",
                "vst('FX', { 'gain': saw.range(0, 1)",
            ),
            ("vst('FX', { |gain|, gain: .2 })", "vst('FX', { gain: .2 })"),
            ("vst('FX', { gain: .2, |ga| })", "vst('FX', { gain: .2})"),
            ("vst('FX', { gain: .2, |ga|", "vst('FX', { gain: .2"),
        ] {
            let typed = marked.find('|').unwrap()..marked.rfind('|').unwrap() - 1;
            let source = marked.replace('|', "");
            let (range, text) = parameter_edit(&source, 3, "gain: 0.5", Some(typed)).unwrap();
            let mut result = source.clone();
            result.replace_range(range, &text);
            assert_eq!(result, expected, "{source}");
        }
    }
}
