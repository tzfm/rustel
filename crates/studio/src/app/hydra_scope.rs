//! Hydra code for completion: whether a position is in a Hydra statement.
//!
//! In a score that calls `initHydra()`, a statement is Hydra code when its
//! first name is a Hydra global such as `osc`, `src`, `o0`, `s0` or
//! `render`. The argument of `H(…)` is pattern code again. A statement whose
//! first name is still being typed, or an empty line, takes the kind of the
//! statement above it, and `await initHydra()` counts as Hydra there.

use std::ops::Range;

use rustel_runtime::lint::{code_only, scan, statement_around};

/// The first name of a statement.
enum Head<'a> {
    /// Written in full: something follows it, or the caret is elsewhere.
    /// `assigned` when an `=` follows it.
    Written { name: &'a str, assigned: bool },
    /// The caret is on the name with nothing after it, or on an empty
    /// statement. The name does not say what the statement is yet.
    Typing,
    /// A `$:` or `name:` label: a pattern lane.
    Label,
}

/// Whether `at` is in Hydra code. `global` says whether a name is a Hydra
/// global, as the reference documents them.
pub(super) fn in_hydra_code(source: &str, at: usize, global: impl Fn(&str) -> bool) -> bool {
    let mut code = code_only(source);
    // The checker reads a score the same way: a mention in a comment or a
    // string does not open Hydra.
    if !code.contains("initHydra") {
        return false;
    }
    let at = at.min(code.len());
    // A caret after the last line break stands on a line of its own.
    code.push(' ');
    let statement = statement_at(&code, at);
    let hydra = match head(&code, statement.clone(), Some(at)) {
        Head::Written { name, assigned } => is_hydra_head(name, assigned, &global),
        Head::Typing => previous_is_hydra(&code, statement.start, &global),
        Head::Label => false,
    };
    hydra
        && !scan::open_parens(&code[..at], at)
            .take_while(|open| *open >= statement.start)
            .any(|open| scan::name_before(&code, open).is_some_and(|name| &code[name] == "H"))
}

/// `initHydra` and `clearHydra` are pattern calls, so their own statements
/// are pattern code. A clock name starts a Hydra statement only when it is
/// assigned: `speed = 2` is Hydra, and `speed(2)` is the pattern control.
fn is_hydra_head(name: &str, assigned: bool, global: &impl Fn(&str) -> bool) -> bool {
    match name {
        "initHydra" | "clearHydra" => false,
        "speed" | "bpm" | "fps" | "time" => assigned,
        _ => global(name),
    }
}

fn previous_is_hydra(code: &str, start: usize, global: &impl Fn(&str) -> bool) -> bool {
    let end = code[..start].trim_end().len();
    if end == 0 {
        return false;
    }
    let previous = statement_at(code, end);
    match head(code, previous, None) {
        Head::Written { name, assigned } => {
            name == "initHydra" || is_hydra_head(name, assigned, global)
        }
        Head::Typing | Head::Label => false,
    }
}

/// The statement around `at`: its lines, cut at a `;` outside brackets.
fn statement_at(code: &str, at: usize) -> Range<usize> {
    let lines = statement_around(code, at);
    let bytes = code.as_bytes();
    let mut depth = 0usize;
    let mut start = lines.start;
    for (index, byte) in bytes.iter().enumerate().take(at).skip(lines.start) {
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b';' if depth == 0 => start = index + 1,
            _ => {}
        }
    }
    let end = (at..lines.end)
        .find(|&index| bytes[index] == b';')
        .unwrap_or(lines.end);
    start..end.max(start)
}

/// The first name of `statement`, past `await` and a `const name =`.
fn head(code: &str, statement: Range<usize>, caret: Option<usize>) -> Head<'_> {
    let end = statement.end;
    let name_at = |at: usize| {
        let at = space_after(code, at, end);
        let name = scan::name_starting_at(code, at);
        name.start..name.end.min(end)
    };
    let mut name = name_at(statement.start);
    if &code[name.clone()] == "await" {
        name = name_at(name.end);
    }
    if matches!(&code[name.clone()], "const" | "let" | "var") {
        let binding = name_at(name.end);
        let equals = space_after(code, binding.end, end);
        if code.as_bytes().get(equals) == Some(&b'=') {
            name = name_at(equals + 1);
        }
    }
    let rest = code[name.end..end.max(name.end)].trim_start();
    if !name.is_empty() && rest.starts_with(':') && !rest.starts_with("::") {
        return Head::Label;
    }
    let typing = caret.is_some_and(|caret| caret >= name.start && caret <= name.end);
    if rest.is_empty() && (typing || name.is_empty()) {
        // `$` is the start of a `$:` lane. No Hydra name starts with it.
        if code[name.clone()].starts_with('$') {
            return Head::Label;
        }
        return Head::Typing;
    }
    Head::Written {
        name: &code[name],
        assigned: rest.starts_with('=') && !rest.starts_with("=="),
    }
}

/// The first byte at or after `at` that is not ASCII whitespace, at most `end`.
fn space_after(code: &str, at: usize, end: usize) -> usize {
    let bytes = code.as_bytes();
    let mut at = at;
    while at < end && bytes[at].is_ascii_whitespace() {
        at += 1;
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hydra's statement heads, as the reference files them.
    fn global(name: &str) -> bool {
        matches!(
            name,
            "osc" | "noise" | "shape" | "src" | "solid" | "o0" | "s0" | "render" | "a" | "speed"
        )
    }

    fn hydra(marked: &str) -> bool {
        let at = marked.find('|').expect("a caret");
        let source = marked.replacen('|', "", 1);
        in_hydra_code(&source, at, global)
    }

    #[test]
    fn a_statement_is_hydra_by_its_first_name_once_hydra_is_open() {
        let open = "await initHydra()\n";
        for (score, expected) in [
            ("osc(10).kai|", true),
            ("osc(10)\n  .kaleid(4)\n  .o|", true),
            ("src(s0).blend(osc(|), 0.5).out()", true),
            ("s0.ini|", true),
            ("speed = |", true),
            ("spe|ed(2)", false),
            ("time == |0", false),
            ("const pic = osc(|)", true),
            ("$: s(\"bd\").lp|", false),
            ("note(\"c e g\").s|", false),
            ("setcpm(|30)", false),
            ("osc(10).out(); $: s(\"b|d\")", false),
            ("$: s(\"bd\"); osc(|)", true),
        ] {
            assert_eq!(hydra(&format!("{open}{score}")), expected, "{score:?}");
        }
        assert!(
            !hydra("osc(10).kai|"),
            "a score without initHydra is pattern code"
        );
        assert!(
            !hydra("// await initHydra()\nosc(10).kai|"),
            "a commented initHydra does not open Hydra"
        );
    }

    #[test]
    fn the_argument_of_h_is_pattern_code() {
        let score = "await initHydra()\nshape(H(\"3 4 |5\"), 0.5).out()";
        assert!(!hydra(score));
        assert!(!hydra("await initHydra()\nshape(H(sine.ra|)).out()"));
        assert!(hydra("await initHydra()\nshape(H(\"3 4\"), |).out()"));
    }

    #[test]
    fn a_name_being_typed_takes_the_kind_of_the_statement_above() {
        for (score, expected) in [
            ("await initHydra()\n|", true),
            ("await initHydra()\nos|", true),
            ("await initHydra()\nosc(10).out()\n\nno|", true),
            ("await initHydra()\n$: s(\"bd\")\nno|", false),
            ("await initHydra()\nosc(10).out()\n$|", false),
            ("se|\nawait initHydra()", false),
            ("await initHydra()\nclearHydra()\nos|", false),
        ] {
            assert_eq!(hydra(score), expected, "{score:?}");
        }
    }

    #[test]
    fn init_hydra_itself_is_pattern_code() {
        assert!(!hydra("await initHy|dra({ feedStrudel: 1 })"));
        assert!(!hydra("await initHydra({ feedStrudel: |1 })"));
    }
}
