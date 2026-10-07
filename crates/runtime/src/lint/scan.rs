//! Names, brackets, method chains and function heads in score source.
//! Offsets are the source's own. The name and whitespace helpers read any
//! text; the bracket, chain and function helpers expect text from
//! [`code_only`](super::code_only), where strings, comments and regexes are
//! blank. A name is ASCII letters, digits, `_` and `$`; a JavaScript name may
//! also run on through non-ASCII letters, which [`continues_name`] accepts
//! for the readers that must not split one.
//!
//! Whitespace has two rules here. The plain helpers skip what
//! `u8::is_ascii_whitespace` accepts: space, tab, line feed, form feed and
//! carriage return. The `js_space` helpers skip what `str::trim` skips,
//! Unicode's White_Space, which adds the vertical tab and every non-ASCII
//! space, the no-break space among them. That is JavaScript's whitespace
//! but for U+FEFF, which JavaScript also skips, and U+0085, which it does
//! not. The input-channel check reads a chain as the engine runs it, so it
//! takes the `js_space` rule; every other reader keeps the ASCII one it
//! has always used.

use std::ops::Range;

/// Whether `byte` can be part of a name.
pub fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$')
}

/// Whether `byte` continues a JavaScript name: a name byte, or any byte of
/// a non-ASCII character, which JavaScript may read as a letter. The name
/// helpers here stop before such a character; a reader that must not take
/// the end of a longer name for a whole one checks for it with this.
pub(crate) fn continues_name(byte: u8) -> bool {
    is_name_byte(byte) || !byte.is_ascii()
}

/// The start of the whitespace ending at `at`, or `at` when there is none.
pub fn space_before(text: &str, at: usize) -> usize {
    let bytes = text.as_bytes();
    let mut start = at;
    while start > 0 && bytes[start - 1].is_ascii_whitespace() {
        start -= 1;
    }
    start
}

/// [`space_before`] for JavaScript's whitespace.
pub(crate) fn js_space_before(text: &str, at: usize) -> usize {
    text[..at].trim_end().len()
}

/// The name ending exactly at `end`, or `end..end` when there is none.
pub fn name_ending_at(text: &str, end: usize) -> Range<usize> {
    let bytes = text.as_bytes();
    let mut start = end;
    while start > 0 && is_name_byte(bytes[start - 1]) {
        start -= 1;
    }
    start..end
}

/// The name before `at`, skipping whitespace between them.
pub fn name_before(text: &str, at: usize) -> Option<Range<usize>> {
    let name = name_ending_at(text, space_before(text, at));
    (!name.is_empty()).then_some(name)
}

/// [`name_before`] across JavaScript's whitespace.
pub(crate) fn name_before_js_space(text: &str, at: usize) -> Option<Range<usize>> {
    let name = name_ending_at(text, js_space_before(text, at));
    (!name.is_empty()).then_some(name)
}

/// The name starting exactly at `start`, or an empty range when there is
/// none.
pub fn name_starting_at(text: &str, start: usize) -> Range<usize> {
    let bytes = text.as_bytes();
    let start = start.min(bytes.len());
    let mut end = start;
    while end < bytes.len() && is_name_byte(bytes[end]) {
        end += 1;
    }
    start..end
}

/// The name starting exactly at `start` when it can name a binding: one that
/// does not open with a digit.
pub(crate) fn identifier_at(text: &str, start: usize) -> Option<Range<usize>> {
    let name = name_starting_at(text, start);
    (!name.is_empty() && !text.as_bytes()[name.start].is_ascii_digit()).then_some(name)
}

/// Whether `word` stands at `index` as a whole word, not the start of a
/// longer name.
pub(crate) fn starts_with_word(code: &str, index: usize, word: &str) -> bool {
    let end = index + word.len();
    // `word.len()` is a byte count and the score may hold multi-byte
    // characters, so `end` can land mid-character. `get` answers `None`
    // there, where slicing would panic, and a range that is not whole
    // characters is not the word anyway.
    if code.get(index..end) != Some(word) {
        return false;
    }
    // A non-ASCII letter continues a JavaScript identifier: `functioné` is
    // one name, not the keyword.
    !code
        .as_bytes()
        .get(end)
        .copied()
        .is_some_and(continues_name)
}

/// The `.` before `at`, skipping whitespace, or `None` when anything else
/// stands there.
pub(crate) fn dot_before(text: &str, at: usize) -> Option<usize> {
    let dot = space_before(text, at).checked_sub(1)?;
    (text.as_bytes()[dot] == b'.').then_some(dot)
}

/// [`dot_before`] across JavaScript's whitespace.
pub(crate) fn dot_before_js_space(text: &str, at: usize) -> Option<usize> {
    let dot = js_space_before(text, at).checked_sub(1)?;
    (text.as_bytes()[dot] == b'.').then_some(dot)
}

/// The end of the whitespace starting at `at`, or `at` when there is none.
pub(crate) fn space_after(text: &str, at: usize) -> usize {
    let bytes = text.as_bytes();
    let mut end = at;
    while bytes.get(end).is_some_and(u8::is_ascii_whitespace) {
        end += 1;
    }
    end
}

/// [`space_after`] for JavaScript's whitespace.
pub(crate) fn js_space_after(text: &str, at: usize) -> usize {
    text.get(at..)
        .map_or(at, |rest| at + rest.len() - rest.trim_start().len())
}

/// The call a `.` at `dot` opens: its name, and its `(`. Whitespace may
/// stand on either side of the name. `None` when `dot` holds no `.` or no
/// named call follows it.
pub(crate) fn method_at(code: &str, dot: usize) -> Option<(Range<usize>, usize)> {
    method_across(code, dot, space_after)
}

/// [`method_at`] across JavaScript's whitespace.
pub(crate) fn method_at_js_space(code: &str, dot: usize) -> Option<(Range<usize>, usize)> {
    method_across(code, dot, js_space_after)
}

/// [`method_at`], skipping whitespace with `skip`.
fn method_across(
    code: &str,
    dot: usize,
    skip: fn(&str, usize) -> usize,
) -> Option<(Range<usize>, usize)> {
    let bytes = code.as_bytes();
    if bytes.get(dot) != Some(&b'.') {
        return None;
    }
    let name = name_starting_at(code, skip(code, dot + 1));
    let open = skip(code, name.end);
    (!name.is_empty() && bytes.get(open) == Some(&b'(')).then_some((name, open))
}

/// Each `(` still open at `at`, innermost first. Only round brackets count.
pub fn open_parens(code: &str, at: usize) -> impl Iterator<Item = usize> + '_ {
    let bytes = code.as_bytes();
    let mut depth = 0usize;
    (0..at.min(bytes.len()))
        .rev()
        .filter(move |&index| match bytes[index] {
            b')' => {
                depth += 1;
                false
            }
            b'(' if depth == 0 => true,
            b'(' => {
                depth -= 1;
                false
            }
            _ => false,
        })
}

/// The `(` matching the `)` at `close`.
pub(crate) fn opening_paren(code: &str, close: usize) -> Option<usize> {
    open_parens(code, close).next()
}

/// The index of the `)` closing the `(` at `open`, skipping what strings
/// hold; `None` when the text runs out first. Quotes are read here, so
/// unblanked source works as well as `code_only` text.
pub(crate) fn matching_paren(source: &str, open: usize) -> Option<usize> {
    matching_pair(source, open, b'(', b')')
}

/// [`matching_paren`] for any pair of brackets.
pub(crate) fn matching_pair(source: &str, open: usize, open_ch: u8, close_ch: u8) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut index = open;
    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Some(open_quote) => {
                if byte == b'\\' {
                    // Step over the escaped byte as well as the backslash: a
                    // quote a backslash protects does not close the string,
                    // and a closer inside it does not close the pair.
                    index += 2;
                    continue;
                }
                if byte == open_quote {
                    quote = None;
                }
            }
            None => {
                if byte == b'"' || byte == b'\'' || byte == b'`' {
                    quote = Some(byte);
                } else if byte == open_ch {
                    depth += 1;
                } else if byte == close_ch {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some(index);
                    }
                }
            }
        }
        index += 1;
    }
    None
}

/// One call in a method chain: its name, its `(`, and its `)` when closed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Link {
    pub name: Range<usize>,
    pub open: usize,
    pub close: Option<usize>,
}

impl Link {
    /// The call named `name` whose `(` is at `open`, with the `)` that
    /// closes it, if one does.
    fn opened_at(code: &str, name: Range<usize>, open: usize) -> Self {
        Self {
            name,
            open,
            close: matching_paren(code, open),
        }
    }
}

/// Each `(` whose brackets hold `at`, innermost first, as the call it opens.
/// A `(` with no name before it groups, and its name is empty.
fn parens_around(code: &str, at: usize) -> impl Iterator<Item = Link> + '_ {
    open_parens(code, at).map(move |open| {
        Link::opened_at(code, name_ending_at(code, space_before(code, open)), open)
    })
}

/// The call whose `)` ends the text before `at`, skipping whitespace.
/// `None` when that text ends in anything but a named call.
pub(crate) fn call_before(code: &str, at: usize) -> Option<Link> {
    let close = space_before(code, at).checked_sub(1)?;
    if code.as_bytes()[close] != b')' {
        return None;
    }
    let open = opening_paren(code, close)?;
    let name = name_before(code, open)?;
    Some(Link {
        name,
        open,
        close: Some(close),
    })
}

/// The call before the `.` that precedes the name starting at `start`.
/// `None` at a chain's head or when the receiver is not a call.
pub fn link_before(code: &str, start: usize) -> Option<Link> {
    call_before(code, dot_before(code, start)?)
}

/// The call chained after the `)` at `close`, or `None` when the chain
/// ends there or continues with something other than a call.
pub(crate) fn link_after(code: &str, close: usize) -> Option<Link> {
    let (name, open) = method_at(code, space_after(code, close + 1))?;
    Some(Link::opened_at(code, name, open))
}

/// Each `,` from `from` up to `to` that no bracket opened after `from`
/// encloses.
fn top_level_commas(code: &str, from: usize, to: usize) -> impl Iterator<Item = usize> + '_ {
    let mut depth = 0usize;
    code.bytes()
        .enumerate()
        .take(to)
        .skip(from)
        .filter(move |&(_, byte)| {
            match byte {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth = depth.saturating_sub(1),
                b',' => return depth == 0,
                _ => {}
            }
            false
        })
        .map(|(index, _)| index)
}

/// The start of the top-level argument holding `at` in the bracket group
/// opened at `open`: just past the last `,` before `at` that no other
/// bracket encloses, or just past `open`.
pub(crate) fn argument_start(code: &str, open: usize, at: usize) -> usize {
    top_level_commas(code, open + 1, at)
        .last()
        .map_or(open + 1, |comma| comma + 1)
}

/// A function's head: its parameters, and where its body begins.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FunctionHead {
    /// Each parameter as written, trimmed: `x`, `x = 1`, `{ a }`,
    /// `...rest`. `()` has none.
    pub(crate) params: Vec<Range<usize>>,
    /// Just past the `=>` of an arrow, or past the `)` closing a
    /// `function`'s parameters. The body follows after whitespace.
    pub(crate) end: usize,
    /// Whether the head opens with the keyword `function`, whose body is a
    /// block.
    pub(crate) keyword: bool,
}

impl FunctionHead {
    /// The parameters that are plain names. A destructured, defaulted or
    /// rest parameter is left out.
    pub(crate) fn names<'a>(&'a self, code: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.params
            .iter()
            .filter(|param| identifier_at(code, param.start).as_ref() == Some(*param))
            .map(|param| &code[param.clone()])
    }
}

/// The head of the function that opens at `start`, after whitespace:
/// `x =>`, `(x, i) =>` or `function [name](x)`. `None` when no function
/// opens there.
pub(crate) fn function_head(code: &str, start: usize) -> Option<FunctionHead> {
    let bytes = code.as_bytes();
    let at = space_after(code, start);
    let past_arrow = |from: usize| {
        let arrow = space_after(code, from);
        bytes[arrow..].starts_with(b"=>").then_some(arrow + 2)
    };
    let keyword = starts_with_word(code, at, "function");
    let open = if keyword {
        let name = space_after(code, at + "function".len());
        space_after(
            code,
            identifier_at(code, name).map_or(name, |name| name.end),
        )
    } else if bytes.get(at) == Some(&b'(') {
        at
    } else {
        let name = identifier_at(code, at)?;
        let end = past_arrow(name.end)?;
        return Some(FunctionHead {
            params: vec![name],
            end,
            keyword,
        });
    };
    if bytes.get(open) != Some(&b'(') {
        return None;
    }
    let close = matching_paren(code, open)?;
    let end = if keyword {
        close + 1
    } else {
        past_arrow(close + 1)?
    };
    let mut params = Vec::new();
    let mut from = open + 1;
    if space_after(code, from) < close {
        for comma in top_level_commas(code, from, close).chain(std::iter::once(close)) {
            let param = space_after(code, from);
            params.push(param..space_before(code, comma).max(param));
            from = comma + 1;
        }
    }
    Some(FunctionHead {
        params,
        end,
        keyword,
    })
}

/// Whether `code` contains `=>` or the keyword `function` as a whole word.
pub fn holds_function(code: &str) -> bool {
    code.contains("=>")
        || code.match_indices("function").any(|(at, _)| {
            !code.as_bytes()[..at]
                .last()
                .copied()
                .is_some_and(continues_name)
                && starts_with_word(code, at, "function")
        })
}

/// Each call that applies to the pattern at `at`, in the order it applies:
/// the chain `at` sits in, then the links chained after each call or group
/// enclosing that chain. A pattern heading a chain of its own, as the string
/// does in `"cp".s()`, passes the index its head ends on as `head`; the links
/// chained after that head, and after each group around it, apply just before
/// the call holding it. `at` also sits in the chain of a call it rests right
/// after: one closed earlier on the same line, or one whose `.` it is typing
/// a name after. An enclosing call's own receiver applies only while the
/// chain inside it hangs off a parameter of a function opening the argument
/// that holds the chain, as in `.off(1/8, x => x.s(…))`.
pub(crate) fn applied_links(code: &str, at: usize, head: Option<usize>) -> Vec<Link> {
    let mut own = links_after(code, head);
    let mut around = parens_around(code, at);
    let direct = match resting_call(code, at) {
        Some(call) => call,
        None => loop {
            let Some(paren) = around.next() else {
                return own;
            };
            if !paren.name.is_empty() {
                break paren;
            }
            own.extend(links_after(code, paren.close));
        },
    };
    let mut inherited = Vec::new();
    let mut wrapping = Vec::new();
    let mut inherits = true;
    let mut inner = direct.name.start;
    for paren in around {
        if !paren.name.is_empty() {
            inherits = inherits && hands_on_receiver(code, &paren, inner);
            if inherits {
                inherited.push(links_before(code, paren.name.start));
            }
            inner = paren.name.start;
        }
        wrapping.extend(links_after(code, paren.close));
    }
    let mut applied: Vec<Link> = inherited.into_iter().rev().flatten().collect();
    applied.extend(links_before(code, direct.name.start));
    applied.extend(own);
    let close = direct.close;
    applied.push(direct);
    applied.extend(links_after(code, close));
    applied.extend(wrapping);
    applied
}

/// Whether the receiver of the enclosing `call` reaches the chain holding the
/// link whose name starts at `link`: that chain hangs off a bare name, and
/// the argument of `call` holding the chain opens with a function taking
/// that name as a parameter, as `x` does in `.off(1/8, x => x.s(…))`.
fn hands_on_receiver(code: &str, call: &Link, link: usize) -> bool {
    let head = links_before(code, link)
        .first()
        .map_or(link, |first| first.name.start);
    let Some(receiver) = bare_receiver(code, head) else {
        return false;
    };
    let start = argument_start(code, call.open, receiver.start);
    function_head(code, start).is_some_and(|function| {
        function
            .names(code)
            .any(|name| name == &code[receiver.clone()])
    })
}

/// The name a chain head starting at `head` hangs off, as `x` in `x.s(…)`.
/// `None` for a head with no `.` before it, one hung off a property or
/// anything but a name, or a name running on from a non-ASCII character,
/// which the name scan cannot read whole.
fn bare_receiver(code: &str, head: usize) -> Option<Range<usize>> {
    let name = name_before(code, dot_before(code, head)?)?;
    let whole = !code.as_bytes()[..name.start]
        .last()
        .copied()
        .is_some_and(continues_name);
    (whole && dot_before(code, name.start).is_none()).then_some(name)
}

/// The call `at` rests after: the one before a `.` it is typing a name
/// after, or one ending on the same line.
fn resting_call(code: &str, at: usize) -> Option<Link> {
    let typed = name_ending_at(code, at).start;
    if let Some(link) = link_before(code, typed) {
        return Some(link);
    }
    let call = call_before(code, typed)?;
    let close = call.close?;
    (!code[close..typed].contains('\n')).then_some(call)
}

/// The links chained before the one whose name starts at `start`, head
/// first.
fn links_before(code: &str, start: usize) -> Vec<Link> {
    let mut links = Vec::new();
    let mut from = start;
    while let Some(before) = link_before(code, from) {
        from = before.name.start;
        links.push(before);
    }
    links.reverse();
    links
}

/// The links chained after the `)` at `close`, in order.
fn links_after(code: &str, mut close: Option<usize>) -> Vec<Link> {
    let mut links = Vec::new();
    while let Some(after) = close.and_then(|close| link_after(code, close)) {
        close = after.close;
        links.push(after);
    }
    links
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The offset just past the first `needle` in `text`.
    fn after(text: &str, needle: &str) -> usize {
        text.find(needle).expect(needle) + needle.len()
    }

    /// `name_before` skips spaces and line breaks and accepts `$`; anything else
    /// before `at` is no name.
    #[test]
    fn a_name_is_found_across_whitespace_and_nothing_else() {
        fn name(text: &str, at: usize) -> Option<&str> {
            name_before(text, at).map(|name| &text[name])
        }
        assert_eq!(name("setcpm (", 7), Some("setcpm"));
        assert_eq!(name("s\n  (", 4), Some("s"));
        assert_eq!(name("$my_fn(", 6), Some("$my_fn"));
        assert_eq!(name("a.b(", 3), Some("b"));
        assert_eq!(name("1 + (", 4), None);
        assert_eq!(name("  (", 2), None);
        assert_eq!(name("(", 0), None);
    }

    /// `name_ending_at` skips nothing.
    #[test]
    fn a_name_ending_at_a_place_skips_nothing() {
        assert_eq!(name_ending_at("setcpm(", 6), 0..6);
        assert_eq!(name_ending_at("setcpm (", 7), 7..7);
        assert_eq!(name_ending_at("x.fast", 6), 2..6);
        assert_eq!(name_ending_at("", 0), 0..0);
    }

    /// `name_starting_at` skips nothing and clamps past the end.
    #[test]
    fn a_name_starting_at_a_place_runs_to_its_end() {
        assert_eq!(name_starting_at("lpenv(4)", 2), 2..5);
        assert_eq!(name_starting_at("a .b", 1), 1..1);
        assert_eq!(name_starting_at("ab", 9), 2..2);
    }

    /// `identifier_at` reads a name that does not open with a digit, from
    /// exactly where it is asked.
    #[test]
    fn an_identifier_is_a_name_that_does_not_open_with_a_digit() {
        assert_eq!(identifier_at("$a1(", 0), Some(0..3));
        assert_eq!(identifier_at("x _y", 2), Some(2..4));
        assert_eq!(identifier_at("2e3", 0), None);
        assert_eq!(identifier_at(" x", 0), None);
        assert_eq!(identifier_at("x", 1), None);
    }

    /// `starts_with_word` measures the word in bytes: a multi-byte
    /// character where the word would end answers false instead of
    /// panicking on a slice that is not whole characters.
    #[test]
    fn starts_with_word_is_false_when_the_word_would_end_mid_character() {
        // Byte 8 of the parameter is inside the second `é`: the repro of
        // the panic the value-callback scan used to have.
        assert!(!starts_with_word("aaaaaéé => x", 0, "function"));
        // A character boundary anywhere else changes nothing.
        assert!(!starts_with_word("éfunction", 0, "function"));
        assert!(starts_with_word("function (v) { return v }", 0, "function"));
        // A non-ASCII letter continues the identifier, as in JavaScript.
        assert!(!starts_with_word("functioné", 0, "function"));
        assert!(!starts_with_word("functionx", 0, "function"));
        assert!(!starts_with_word("func", 0, "function"));
    }

    /// A JavaScript name runs on through a name byte or any byte of a non-ASCII
    /// character, and stops at anything else ASCII.
    #[test]
    fn a_name_continues_through_non_ascii_letters() {
        for byte in [
            b'a',
            b'Z',
            b'0',
            b'_',
            b'$',
            "é".as_bytes()[0],
            "é".as_bytes()[1],
        ] {
            assert!(continues_name(byte), "{byte:#x}");
        }
        for &byte in b" .()\"-\n" {
            assert!(!continues_name(byte), "{byte:#x}");
        }
    }

    /// `dot_before` finds a `.` across whitespace, and only a `.`.
    #[test]
    fn a_dot_is_found_across_whitespace() {
        assert_eq!(dot_before("a.b", 2), Some(1));
        assert_eq!(dot_before("a .\n  b", 6), Some(2));
        assert_eq!(dot_before("...b", 3), Some(2));
        assert_eq!(dot_before("a b", 2), None);
        assert_eq!(dot_before("b", 0), None);
    }

    /// `method_at` and `method_at_js_space` read the named call a `.` opens,
    /// spaced or not. Only the second reads through a no-break space.
    #[test]
    fn a_method_is_the_named_call_after_a_dot() {
        type Reader = fn(&str, usize) -> Option<(std::ops::Range<usize>, usize)>;
        fn check(read: Reader) {
            assert_eq!(read("x.fast(2)", 1), Some((2..6, 6)));
            assert_eq!(read("x. fast\n (2)", 1), Some((3..7, 9)));
            for (code, dot) in [
                ("x.fast", 1),
                ("x.(2)", 1),
                ("x.fast.y(2)", 1),
                ("fast(2)", 0),
                ("x.", 1),
                ("x.fast(2)", 9),
            ] {
                assert_eq!(read(code, dot), None, "{code} at {dot}");
            }
        }
        check(method_at_js_space);
        check(method_at);
        assert_eq!(
            method_at_js_space("x.\u{a0}fast\u{b}(2)", 1),
            Some((4..8, 9))
        );
        assert_eq!(method_at("x.\u{a0}fast(2)", 1), None);
    }

    /// Over blanked code, brackets in strings, comments and regexes are ignored,
    /// and open brackets come innermost first.
    #[test]
    fn the_open_brackets_are_the_codes_innermost_first() {
        let source = r#"a(b(")", /* ) */ c(d), /(/, e("(" "#;
        let code = super::super::code_only(source);
        let names = open_parens(&code, code.len())
            .map(|open| name_before(&code, open).map(|name| &source[name]))
            .collect::<Vec<_>>();
        assert_eq!(names, [Some("e"), Some("b"), Some("a")]);
        let at = after(source, "c(");
        let names = open_parens(&code, at)
            .map(|open| name_before(&code, open).map(|name| &source[name]))
            .collect::<Vec<_>>();
        assert_eq!(names, [Some("c"), Some("b"), Some("a")]);
        let code = "f((x";
        assert_eq!(open_parens(code, code.len()).collect::<Vec<_>>(), [2, 1]);
        assert_eq!(opening_paren("f(a(b)c)", 7), Some(1));
        assert_eq!(opening_paren("f(a(b)c)", 5), Some(3));
        assert_eq!(opening_paren("a)b)", 3), None);
    }

    /// `parens_around` reads the brackets holding a place innermost first, a
    /// group with an empty name, noting where each closes.
    #[test]
    fn the_parens_around_a_place_are_read_innermost_first() {
        let code = "a(b, (c(d), e(f x)), g";
        let at = code.find('x').expect("x");
        let calls = parens_around(code, at)
            .map(|call| {
                (
                    &code[call.name],
                    call.close.map(|close| &code[call.open..=close]),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            calls,
            [
                ("e", Some("(f x)")),
                ("", Some("(c(d), e(f x))")),
                ("a", None)
            ]
        );
    }

    /// Chains are followed in both directions across line breaks and blanked
    /// comments.
    #[test]
    fn a_chain_is_walked_call_by_call_both_ways() {
        let source = "s(\"bd\") // a (\n  .fast(2) /* ) */\n  .bank(\"Metal\")";
        let code = super::super::code_only(source);
        let name = |link: &Link| &source[link.name.clone()];

        let fast = link_after(&code, code.find(')').expect("s closes")).expect("fast");
        assert_eq!(name(&fast), "fast");
        let bank = link_after(&code, fast.close.expect("fast closes")).expect("bank");
        assert_eq!(name(&bank), "bank");
        assert_eq!(bank.close, Some(source.len() - 1));
        assert_eq!(link_after(&code, source.len() - 1), None, "the chain's end");

        let before_bank = link_before(&code, bank.name.start).expect("fast before bank");
        assert_eq!(before_bank, fast);
        let head = link_before(&code, fast.name.start).expect("s before fast");
        assert_eq!(name(&head), "s");
        assert_eq!(
            &source[head.open + 1..head.close.expect("closed")],
            "\"bd\""
        );
        assert_eq!(
            link_before(&code, head.name.start),
            None,
            "the chain's head"
        );

        let rest = call_before(&code, source.len()).expect("the last call");
        assert_eq!(rest, bank);
    }

    /// A link only follows a named call. An unclosed call yields a link without
    /// a `close`.
    #[test]
    fn a_link_hung_off_anything_but_a_call_is_none() {
        for source in [
            "x.bank(",
            "s(\"bd\")[0].bank(",
            "\"bd\".bank(",
            "(a, b).bank(",
            "s(\"bd\").x.bank(",
            ".bank(",
            "bank(",
        ] {
            let code = super::super::code_only(source);
            let bank = source.find("bank").expect("bank");
            assert_eq!(link_before(&code, bank), None, "{source}");
        }
        assert_eq!(call_before("x", 1), None);
        assert_eq!(call_before("", 0), None);
        let code = super::super::code_only("s(\"bd\").bank(\"Met");
        let next = link_after(&code, code.find(')').expect("s closes")).expect("bank");
        assert_eq!(next.close, None);
        assert_eq!(link_after("s(x).5", 3), None);
        assert_eq!(link_after("s(x).y.z()", 3), None);
    }

    /// `argument_start` finds the top-level argument holding a position, past
    /// the bracket groups closed before it and inside those still open.
    #[test]
    fn an_argument_starts_after_the_last_top_level_comma() {
        let code = "f(a(1, 2), [b, c], {d: 1, e}, x => g(y, z";
        let open = 1;
        for (needle, expected) in [
            ("a(1", open + 1),
            ("[b", after(code, "2),")),
            ("c]", after(code, "2),")),
            ("e}", after(code, "c],")),
            ("x =>", after(code, "e},")),
            ("z", after(code, "e},")),
        ] {
            let at = code.find(needle).expect(needle);
            assert_eq!(argument_start(code, open, at), expected, "{needle}");
        }
        let at = code.find('z').expect("z");
        assert_eq!(
            argument_start(code, code.find("g(").expect("g") + 1, at),
            after(code, "y,")
        );
    }

    /// `function_head` reads each parameter as written, and where the head
    /// ends: past an arrow's `=>`, or past the `)` of a `function`'s
    /// parameters.
    #[test]
    fn a_functions_head_is_read_where_it_opens() {
        fn head(code: &str) -> Option<(Vec<String>, &str, bool)> {
            function_head(code, 0).map(|head| {
                let params = head
                    .params
                    .iter()
                    .map(|param| code[param.clone()].to_owned())
                    .collect::<Vec<_>>();
                (params, &code[head.end..], head.keyword)
            })
        }
        let owned = |params: &[&str]| params.iter().map(|param| (*param).to_owned()).collect();
        for (code, expected) in [
            (" x => x.s()", Some((&["x"][..], " x.s()", false))),
            ("$p=>1", Some((&["$p"][..], "1", false))),
            ("(x, i) => x", Some((&["x", "i"][..], " x", false))),
            ("( x ,\n y ) =>", Some((&["x", "y"][..], "", false))),
            ("() => 1", Some((&[][..], " 1", false))),
            ("( ) => 1", Some((&[][..], " 1", false))),
            ("(x = 1) => x", Some((&["x = 1"][..], " x", false))),
            ("( , y) => y", Some((&["", "y"][..], " y", false))),
            (
                "({a}, [b], ...d, e) => e",
                Some((&["{a}", "[b]", "...d", "e"][..], " e", false)),
            ),
            (
                "function (x) { return x }",
                Some((&["x"][..], " { return x }", true)),
            ),
            (
                "function named(x, y) {}",
                Some((&["x", "y"][..], " {}", true)),
            ),
            ("function(x) x", Some((&["x"][..], " x", true))),
            ("functional => 1", Some((&["functional"][..], " 1", false))),
            ("x.s()", None),
            ("(x).s()", None),
            ("(x => x)", None),
            ("[x => x]", None),
            ("1 => x", None),
            ("function 1(x) {}", None),
            ("function", None),
            ("(x", None),
            ("x =", None),
        ] {
            assert_eq!(
                head(code),
                expected.map(|(params, rest, keyword)| (owned(params), rest, keyword)),
                "{code}"
            );
        }
    }

    /// `names` keeps only the parameters that are plain names.
    #[test]
    fn a_functions_names_are_its_plain_parameters() {
        let code = "({a}, [b], c = 2, ...d, e, $f, 1g, h) => e";
        let head = function_head(code, 0).expect("an arrow");
        assert_eq!(head.names(code).collect::<Vec<_>>(), ["e", "$f", "h"]);
    }

    /// `holds_function` matches `=>` and the whole keyword, not names containing
    /// it, whether an ASCII or a non-ASCII letter runs them on, or blanked
    /// strings.
    #[test]
    fn a_function_is_an_arrow_or_the_whole_keyword() {
        for (code, expected) in [
            ("x => x.s(y)", true),
            ("function (x) { return x }", true),
            ("0, function(x) { return x }", true),
            ("é function(x) { return x }", true),
            ("myfunction", false),
            ("éfunction", false),
            ("functioné", false),
            ("functions", false),
            ("fn", false),
        ] {
            assert_eq!(holds_function(code), expected, "{code}");
        }
        let code = super::super::code_only(r#""function" + "=>""#);
        assert!(!holds_function(&code));
    }

    /// The plain helpers' whitespace is `u8::is_ascii_whitespace`'s: line
    /// breaks, but neither the vertical tab nor a non-ASCII space.
    #[test]
    fn whitespace_is_ascii_only() {
        assert_eq!(space_before("a \n\t", 4), 1);
        assert_eq!(space_before("a\u{3000}", 4), 4);
        assert_eq!(space_before("a\u{b}", 2), 2);
        assert_eq!(space_after("\n a", 0), 2);
        assert_eq!(space_after("\u{a0}a", 0), 0);
        assert_eq!(space_after("ab", 5), 5);
        assert_eq!(name_before("foo\u{a0}(", 5), None);
        assert_eq!(dot_before("x.\u{b}n", 3), None);
    }

    /// The `js_space` helpers skip what `str::trim` skips, so a vertical tab or
    /// a no-break space between two tokens is whitespace to them, as it is to
    /// JavaScript.
    #[test]
    fn js_space_is_what_trim_skips() {
        assert_eq!(js_space_before("a \u{b}\u{a0}\u{3000}", 8), 1);
        assert_eq!(js_space_before("a\n", 2), 1);
        assert_eq!(js_space_before("a", 1), 1);
        assert_eq!(js_space_after("\u{a0}\u{b}\n a", 0), 5);
        assert_eq!(js_space_after("ab", 5), 5);
        for space in ["", " ", "\n  ", "\u{b}", "\u{a0}", "\u{2028}"] {
            let call = format!("foo{space}(");
            assert_eq!(
                name_before_js_space(&call, call.len() - 1),
                Some(0..3),
                "{call:?}"
            );
            let link = format!("x.{space}n");
            assert_eq!(
                dot_before_js_space(&link, link.len() - 1),
                Some(1),
                "{link:?}"
            );
        }
        assert_eq!(name_before_js_space("1 + (", 4), None);
        assert_eq!(dot_before_js_space("x,\u{a0}n", 4), None);
    }
}
