/*
lib.rs - Rust ES2022 parser/printer and Strudel source transforms
Source transforms follow Strudel packages/transpiler/.
Copyright (C) 2022 Strudel contributors

Rust implementation and additions:
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, ArrayExpression, ArrayExpressionElement, AssignmentExpression, AwaitExpression,
    BindingPattern, CallExpression, ExportAllDeclaration, ExportDeclaration,
    ExportDefaultDeclaration, ExportFromDeclaration, ExportNamedDeclaration, Expression,
    ForOfStatement, IfStatement, ImportDeclaration, ImportExpression, ImportMeta,
    JSXAttributeValue, JSXText, NewExpression, ObjectExpression, ObjectPropertyKind, Program,
    RegExpLiteral, Statement, TemplateLiteral, VariableDeclarationKind,
};
use oxc_ast_visit::{Visit, VisitMut, walk, walk_mut};
use oxc_codegen::{Codegen, CodegenOptions};
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::{GetSpan, SourceType};
use oxc_syntax::xml_entities::XML_ENTITIES;
use std::path::Path;

mod callback_ir;
mod frequency_sliders;
pub use callback_ir::{
    MAX_CALLBACK_IR_SOURCE_BYTES, PatternTransformCandidate, PatternTransformFallbackReason,
    lower_pattern_transform_callback,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseDiagnostic {
    pub message: String,
    /// Native UTF-8 byte offset of the primary syntax failure when available.
    pub offset: Option<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParseOutput {
    pub code: String,
    pub diagnostics: Vec<ParseDiagnostic>,
    pub panicked: bool,
    /// Static names published by score-level `register(...)` calls.
    pub registrations: RegistrationHints,
    /// Printed position → source position, from the printer's own map.
    pub line_map: LineMap,
}

/// Printed-text positions mapped back to the text the printer was given.
/// Every rewrite before the printer preserves line structure, so a source
/// line here is the score's line. An error report needs that line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineMap {
    /// `(printed_line, printed_column, source_line, source_column)`,
    /// zero-based, sorted by printed position.
    tokens: Vec<(u32, u32, u32, u32)>,
}

impl LineMap {
    /// The source position for a printed one (both zero-based): the last
    /// mapping at or before it, the way source-map remapping works.
    pub fn original_position(&self, line: u32, column: u32) -> Option<(u32, u32)> {
        let position = self
            .tokens
            .partition_point(|&(l, c, ..)| (l, c) <= (line, column));
        let &(l, _, src_line, src_col) = self.tokens.get(position.checked_sub(1)?)?;
        // A lookup on a line the map never wrote is a miss, not a guess
        // from some earlier line.
        (l == line).then_some((src_line, src_col))
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }
}

/// Facts about score-level `register(...)` calls which a host can know without
/// executing the score. Dynamic names deliberately remain a flag: callers
/// must not reject a later method merely because its runtime name could not be
/// proven statically.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RegistrationHints {
    pub names: Vec<String>,
    pub has_dynamic_names: bool,
    /// A bare final `register(...)` evaluates to its curried function (or, for
    /// an array of names, an object), not a Pattern. Live editors use this to
    /// distinguish a valid definition-only buffer from an accidental scalar
    /// score.
    pub final_expression_is_registration: bool,
}

/// Maximum count of syntax bytes accepted before parsing.
///
/// Raw counting includes strings and comments, so lexical mistakes cannot
/// lower the bound on bracket, member-chain, or operator depth. Together with
/// `PARSE_STACK_BYTES`, this limits recursive parsing while allowing large,
/// shallow setup files.
pub const MAX_STRUCTURAL_BYTES: usize = 64 * 1024;

/// The bytes that buy a level of recursion, counted without tokenizing.
fn structural_bytes(source: &str) -> usize {
    let punctuation = source
        .bytes()
        .filter(|byte| {
            matches!(
                byte,
                b'(' | b'['
                    | b'{'
                    | b'.'
                    | b'!'
                    | b'~'
                    | b'+'
                    | b'-'
                    | b'?'
                    | b':'
                    | b'='
                    | b'>'
                    | b'<'
                    | b'*'
                    | b'/'
                    | b'%'
                    | b'&'
                    | b'|'
                    | b'^'
            )
        })
        .count();
    // These word operators can also recurse without spending punctuation.
    // Count raw occurrences: a mistaken lexical classification must not let
    // one of them hide from this backstop.
    punctuation
        + word_operators()
            .iter()
            .map(|(word, _)| source.matches(*word).count())
            .sum::<usize>()
        // A keyword spelled with an escape, `\u0076oid`, matches no word
        // above but parses as one, and every such spelling contains `\u`.
        + source.matches("\\u").count()
}

/// The stack given to the thread this crate parses on.
///
/// Measured at under 3.3 KB of stack per nesting level, so the deepest source
/// `MAX_STRUCTURAL_BYTES` can express -- 65_536 levels -- fits with room to
/// spare.
/// `the_deepest_source_the_size_limit_admits_survives` parses exactly that
/// worst case, so the pairing is checked rather than reasoned about.
///
/// The stack is address space, not memory: pages are committed only as the
/// recursion touches them, and an ordinary score touches a few dozen.
const PARSE_STACK_BYTES: usize = 256 * 1024 * 1024;

/// The deepest bracket nesting, method chain, or operator chain this crate will
/// hand to a parser.
///
/// These traversals are recursive and cannot report failure: oxc descends
/// through brackets and operator expressions, and this crate's visitors
/// descend once per link in an `a().b().c()` chain. Exhausting the stack
/// aborts the process rather than raising an error, which during a live set is
/// the one failure that cannot be recovered from.
///
/// Unlike the two constants above, this limit is a courtesy: it names what is
/// wrong with the score instead of only how big it is. Safety does not rest on
/// it, because it rests on a tokenizer that can be defeated.
///
/// 512 matches the pattern graph's own nesting limit and is far beyond any
/// real score: across the vendored songs the deepest nests 5 brackets and the
/// longest chains 18 calls.
///
/// Statement nesting shares the limit, measured on the parsed program by
/// `StatementNesting` before it is printed.
///
/// The guards run in this order:
///
/// ```text
/// source
///   |  structural_bytes > MAX_STRUCTURAL_BYTES  -> refuse (size limit)
///   |  measure_nesting  > MAX_SOURCE_NESTING    -> refuse (lexical scan)
///   v
/// parse on a thread with PARSE_STACK_BYTES of stack
///   |  StatementNesting > MAX_SOURCE_NESTING    -> refuse (parsed tree)
///   v
/// print
/// ```
pub const MAX_SOURCE_NESTING: usize = 512;

/// The deepest bracket nesting, member chain, and operator chain in a source file.
struct SourceNesting {
    brackets: usize,
    chain: usize,
    operators: usize,
}

/// One level of the lexical scan.
enum Frame {
    /// An open `(`, `[`, `{`, or a template's `${`. Carries the chain length
    /// and the operator count measured at this level, so `f(a.b().c())`
    /// scores the inner chain on its own rather than adding it to the
    /// enclosing expression.
    Bracket { chain: usize, operators: usize },
    /// The text of a template literal, where brackets are not code.
    Template,
}

/// Whether a `/` here opens a regular expression rather than dividing.
///
/// This is the standard heuristic -- a regex may not follow a value -- and it
/// is genuinely ambiguous after `}`, which ends either a block (regex) or an
/// object literal (division). Either way the cost is bounded: reading a regex
/// as division counts the brackets inside it, and reading a division as a
/// regex skips to the next `/`. The first over-counts, the second under-counts,
/// and `MAX_STRUCTURAL_BYTES` covers the under-count.
fn regex_can_start_here(bytes: &[u8], slash: usize) -> bool {
    let mut index = slash;
    while index > 0 {
        let previous = bytes[index - 1];
        if previous.is_ascii_whitespace() {
            index -= 1;
            continue;
        }
        // A value before the slash means division.
        if previous.is_ascii_alphanumeric()
            || matches!(previous, b')' | b']' | b'_' | b'$' | b'\'' | b'"' | b'`')
        {
            // ... unless the word is a keyword an expression may follow.
            let end = index;
            let mut start = end;
            while start > 0
                && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_')
            {
                start -= 1;
            }
            return matches!(
                &bytes[start..end],
                b"return"
                    | b"typeof"
                    | b"instanceof"
                    | b"in"
                    | b"of"
                    | b"new"
                    | b"delete"
                    | b"void"
                    | b"case"
                    | b"do"
                    | b"else"
                    | b"yield"
                    | b"await"
            );
        }
        return true;
    }
    true
}

/// Exact byte ranges occupied by regular-expression literals in parseable
/// JavaScript. Source transforms must use these rather than guessing from the
/// byte before `/`: the same slash is a regex after an `if (…)` control head,
/// but division after a call, a postfix increment, or an intervening comment.
#[derive(Default)]
struct RegexLiteralRanges(Vec<(usize, usize)>);

#[cfg(test)]
std::thread_local! {
    static REGEX_LITERAL_PARSE_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl RegexLiteralRanges {
    fn parse(source: &str) -> Self {
        if !has_code_slash(source) {
            return Self::default();
        }
        #[cfg(test)]
        REGEX_LITERAL_PARSE_COUNT.with(|count| count.set(count.get() + 1));
        let allocator = Allocator::default();
        let parsed = Parser::new(&allocator, source, SourceType::mjs()).parse();
        let mut collector = RegexLiteralCollector::default();
        collector.visit_program(&parsed.program);
        collector.ranges.sort_unstable_by_key(|range| range.0);
        Self(collector.ranges)
    }

    fn parse_if_present(source: &str, regexes_are_present: bool) -> Self {
        if regexes_are_present {
            Self::parse(source)
        } else {
            Self::default()
        }
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn end_at(&self, start: usize) -> Option<usize> {
        self.0
            .binary_search_by_key(&start, |range| range.0)
            .ok()
            .map(|index| self.0[index].1)
    }

    /// The opening `/` of a regex literal that ends at `end` or covers the
    /// character just before it (flags after the closer sit inside the span).
    fn start_at_end(&self, end: usize) -> Option<usize> {
        self.0.iter().find_map(|&(start, stop)| {
            (stop == end || (start < end && end <= stop)).then_some(start)
        })
    }
}

#[derive(Default)]
struct RegexLiteralCollector {
    ranges: Vec<(usize, usize)>,
}

impl<'a> Visit<'a> for RegexLiteralCollector {
    fn visit_reg_exp_literal(&mut self, literal: &RegExpLiteral<'a>) {
        self.ranges
            .push((literal.span.start as usize, literal.span.end as usize));
    }
}

/// Double-quoted arguments that are JavaScript data rather than mini
/// notation. Keep this list deliberately tiny and structural: globally
/// exempting URLs (or every argument to a method named `initImage`) would
/// silently change ordinary score semantics.
#[derive(Default)]
struct OrdinaryStringLiteralRanges(Vec<(usize, usize)>);

impl OrdinaryStringLiteralRanges {
    fn parse(source: &str) -> Self {
        // Most scores name no device, load no image, and construct no
        // built-in error. Skip the parse unless one of the exempted names
        // is even present as text.
        if !PORT_NAMED_CALLS
            .iter()
            .chain(ERROR_MESSAGE_CALLS.iter())
            .map(|(name, _)| *name)
            .chain(std::iter::once("initImage"))
            .any(|name| source.contains(name))
        {
            return Self::default();
        }
        let allocator = Allocator::default();
        let parsed = Parser::new(&allocator, source, SourceType::mjs()).parse();
        let mut collector = HydraImageStringCollector::default();
        collector.visit_program(&parsed.program);
        collector.ranges.sort_unstable_by_key(|range| range.0);
        Self(collector.ranges)
    }

    fn contains(&self, start: usize) -> bool {
        self.0.binary_search_by_key(&start, |range| range.0).is_ok()
    }
}

/// Calls whose string argument names a device, a bank or a file and not a
/// rhythm, with the position of that argument.
///
/// Such a name is not mini-notation. Compiled as mini-notation,
/// `.midi("Pilote IAC Bus 1")` reaches the call as four words: `Pilote`,
/// `IAC`, `Bus`, `1`. The transpiler leaves the string unchanged, so a
/// name means the same in single and double quotes.
const PORT_NAMED_CALLS: [(&str, usize); 8] = [
    ("midi", 0),
    ("midin", 0),
    ("midikeys", 0),
    ("midimap", 0),
    // baud, sendcrc, singlecharids, THEN the port.
    ("serial", 3),
    // A bank to fetch and the base it hangs off: a URL and a path, both
    // of which mini-notation would cut into words at every space.
    ("samples", 0),
    ("samples", 1),
    ("loadSoundfont", 0),
];

/// The built-in `Error` family, and the argument position that carries a
/// message for a person rather than a rhythm for the engine.
///
/// The native constructor converts that argument with `ToString` and keeps
/// only the result, so later code, including the engine's exception
/// reporting, cannot recover the original. A compiled mini-notation Pattern
/// converts to `[object Object]`, and `throw new Error("boom")` would report
/// `Error: [object Object]`. The transpiler leaves the message unchanged,
/// as it does for a device name.
///
/// `AggregateError` takes its message second, after the errors iterable.
/// Every other constructor here takes it first. QuickJS's `InternalError`
/// is in the list too.
const ERROR_MESSAGE_CALLS: [(&str, usize); 9] = [
    ("Error", 0),
    ("TypeError", 0),
    ("RangeError", 0),
    ("SyntaxError", 0),
    ("ReferenceError", 0),
    ("EvalError", 0),
    ("URIError", 0),
    ("AggregateError", 1),
    ("InternalError", 0),
];

#[derive(Default)]
struct HydraImageStringCollector {
    ranges: Vec<(usize, usize)>,
}

impl HydraImageStringCollector {
    /// Exempt `name`'s string argument at each position either table names.
    fn exempt_named_argument<'a>(&mut self, name: &str, arguments: &[Argument<'a>]) {
        for (_, at) in PORT_NAMED_CALLS
            .iter()
            .chain(ERROR_MESSAGE_CALLS.iter())
            .filter(|(candidate, _)| *candidate == name)
        {
            match arguments.get(*at) {
                Some(Argument::StringLiteral(literal)) => {
                    let span = literal.span();
                    self.ranges.push((span.start as usize, span.end as usize));
                }
                // The same words in backticks: a template with nothing
                // interpolated is an ordinary string too, and the rewriter
                // would otherwise compile it just the same.
                Some(Argument::TemplateLiteral(template)) if template.expressions.is_empty() => {
                    let span = template.span();
                    self.ranges.push((span.start as usize, span.end as usize));
                }
                _ => {}
            }
        }
    }
}

impl<'a> Visit<'a> for HydraImageStringCollector {
    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        let direct_hydra_source = call.callee.as_member_expression().is_some_and(|member| {
            member.static_property_name() == Some("initImage")
                && ["s0", "s1", "s2", "s3"]
                    .iter()
                    .any(|source| member.object().is_specific_id(source))
        });
        if direct_hydra_source
            && let Some(Argument::StringLiteral(literal)) = call.arguments.first()
        {
            let span = literal.span();
            self.ranges.push((span.start as usize, span.end as usize));
        }
        // A device name, wherever it is written: `.midi('IAC')` on a
        // pattern, `await midikeys('KeyStep')` on its own. `Error("boom")`,
        // called plainly rather than with `new`, constructs the same way.
        let called = call
            .callee
            .as_member_expression()
            .and_then(|member| member.static_property_name())
            .or_else(|| {
                call.callee
                    .get_identifier_reference()
                    .map(|id| id.name.as_str())
            });
        if let Some(called) = called {
            self.exempt_named_argument(called, &call.arguments);
        }
        walk::walk_call_expression(self, call);
    }

    fn visit_new_expression(&mut self, new_expr: &NewExpression<'a>) {
        let called = new_expr
            .callee
            .as_member_expression()
            .and_then(|member| member.static_property_name())
            .or_else(|| {
                new_expr
                    .callee
                    .get_identifier_reference()
                    .map(|id| id.name.as_str())
            });
        if let Some(called) = called {
            self.exempt_named_argument(called, &new_expr.arguments);
        }
        walk::walk_new_expression(self, new_expr);
    }
}

/// Whether source contains a slash that is not inside trivia or a quoted
/// literal. Most scores have none, so they avoid the extra parser pass used to
/// obtain exact regex spans. This is deliberately conservative: division also
/// returns true and is then distinguished exactly by the parser.
fn has_code_slash(source: &str) -> bool {
    let bytes = source.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if matches!(bytes[index], b'\'' | b'"' | b'`') {
            index = scan_quoted(bytes, index, bytes[index]);
            continue;
        }
        if bytes[index..].starts_with(b"//") || bytes[index..].starts_with(b"/*") {
            index = scan_comment(source, index);
            continue;
        }
        if bytes[index] == b'/' {
            return true;
        }
        index += source[index..]
            .chars()
            .next()
            .expect("index remains on a UTF-8 boundary")
            .len_utf8();
    }
    false
}

/// The next byte that is neither whitespace nor a comment, from `index` on.
fn next_significant(bytes: &[u8], index: usize) -> Option<u8> {
    bytes.get(skip_trivia(bytes, index)).copied()
}

/// Index of the first byte at or after `index` that is neither whitespace,
/// a line terminator, nor a comment; `bytes.len()` when none is left.
fn skip_trivia(bytes: &[u8], mut index: usize) -> usize {
    loop {
        while index < bytes.len() {
            let width = if bytes[index].is_ascii_whitespace() {
                1
            } else {
                line_terminator_width_at(bytes, index)
            };
            if width == 0 {
                break;
            }
            index += width;
        }
        match comment_end_at(bytes, index) {
            Some(after) => index = after,
            None => return index,
        }
    }
}

/// Keyword operators from the parser's token kinds. The flag is true for a
/// binary operator, which can continue an expression from the previous line.
fn word_operators() -> &'static [(&'static str, bool)] {
    static WORDS: std::sync::LazyLock<Vec<(&'static str, bool)>> = std::sync::LazyLock::new(|| {
        oxc_parser::Kind::VARIANTS
            .iter()
            .copied()
            .filter(|kind| {
                kind.is_any_keyword()
                    && (kind.is_unary_operator()
                        || kind.is_binary_operator()
                        || matches!(
                            kind,
                            oxc_parser::Kind::Await
                                | oxc_parser::Kind::Yield
                                | oxc_parser::Kind::New
                        ))
            })
            .map(|kind| (kind.to_str(), kind.is_binary_operator()))
            .collect()
    });
    &WORDS
}

/// Return the width and binary status of a word operator without mistaking part of
/// an identifier for a token. Strings, comments, and regexes are skipped by
/// the caller before it asks this question.
fn word_operator_at(bytes: &[u8], index: usize) -> Option<(usize, bool)> {
    fn identifier_byte(byte: u8) -> bool {
        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$') || byte >= 0x80
    }

    if index > 0 && identifier_byte(bytes[index - 1]) {
        return None;
    }
    word_operators().iter().find_map(|(word, binary)| {
        let end = index + word.len();
        (bytes.get(index..end) == Some(word.as_bytes())
            && bytes.get(end).is_none_or(|&byte| !identifier_byte(byte)))
        .then_some((word.len(), *binary))
    })
}

/// Measure nesting without parsing.
///
/// Strings, template literals, comments and regular expressions are skipped,
/// so mini-notation -- which is almost entirely brackets -- never counts
/// toward the limit. Code inside `${...}` does count, because it is parsed.
fn measure_nesting(source: &str) -> SourceNesting {
    let bytes = source.as_bytes();
    let mut index = 0;
    let mut stack: Vec<Frame> = Vec::new();
    let mut brackets = 0;
    // The chain measured outside any bracket, where there is no frame to hold it.
    let mut root_chain = 0;
    let mut chain = 0;
    let mut root_operators = 0;
    let mut operators = 0;
    // The last byte that was neither whitespace nor a comment, so a chain
    // broken across lines by a trailing `.` is still one chain.
    let mut previous_significant = 0u8;
    let mut previous_was_word_operator = false;
    // The last `(from, to)` with `skip_trivia(from) == to`. Each newline in
    // one run of blank lines and comments has the same `to`, so the run is
    // scanned one time and the scan stays linear.
    let mut trivia_run = (0usize, 0usize);

    // UTF-8 continuation bytes are all >= 0x80, so matching on ASCII
    // delimiters byte-wise can never split a multi-byte character.
    while index < bytes.len() {
        let byte = bytes[index];

        if matches!(stack.last(), Some(Frame::Template)) {
            match byte {
                b'\\' => index += 1,
                b'`' => {
                    stack.pop();
                }
                b'$' if bytes.get(index + 1) == Some(&b'{') => {
                    stack.push(Frame::Bracket {
                        chain: 0,
                        operators: 0,
                    });
                    brackets = brackets.max(stack.len());
                    index += 1;
                }
                _ => {}
            }
            index += 1;
            continue;
        }

        // The chain counter of the level being scanned.
        macro_rules! current_chain {
            () => {
                match stack.last_mut() {
                    Some(Frame::Bracket { chain, .. }) => chain,
                    _ => &mut root_chain,
                }
            };
        }
        macro_rules! current_operators {
            () => {
                match stack.last_mut() {
                    Some(Frame::Bracket { operators, .. }) => operators,
                    _ => &mut root_operators,
                }
            };
        }

        let mut word_operator_here = false;
        match byte {
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index = line_terminator_bytes(bytes, index + 2);
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index < bytes.len()
                    && !(bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/'))
                {
                    index += 1;
                }
                index += 2;
                continue;
            }
            b'/' if regex_can_start_here(bytes, index) => {
                // Skip the literal. A `/` inside a character class does not end
                // it, which is exactly the case that fooled a scan without this
                // arm: `/[//]/` read as a line comment hid the rest of the line.
                index += 1;
                let mut in_class = false;
                while index < bytes.len() {
                    if is_line_terminator_at(bytes, index) {
                        break;
                    }
                    match bytes[index] {
                        b'\\' => index += 1,
                        b'[' => in_class = true,
                        b']' => in_class = false,
                        b'/' if !in_class => break,
                        _ => {}
                    }
                    index += 1;
                }
            }
            b'\'' | b'"' => {
                index += 1;
                while index < bytes.len() && bytes[index] != byte {
                    // A backslash escapes the next byte, including the quote
                    // that would otherwise end the string.
                    index += if bytes[index] == b'\\' { 2 } else { 1 };
                }
            }
            b'`' => stack.push(Frame::Template),
            b'(' | b'[' | b'{' => {
                stack.push(Frame::Bracket {
                    chain: 0,
                    operators: 0,
                });
                brackets = brackets.max(stack.len());
            }
            b')' | b']' | b'}' => {
                // Only a bracket can be on top here: template text is handled
                // above, so a `}` closing a `${` pops back into the template
                // frame that already sits underneath it.
                stack.pop();
            }
            b'.' => {
                // A decimal point is not a member access.
                let digits = index > 0
                    && bytes[index - 1].is_ascii_digit()
                    && bytes.get(index + 1).is_some_and(u8::is_ascii_digit);
                if !digits {
                    let slot = current_chain!();
                    *slot += 1;
                    chain = chain.max(*slot);
                }
            }
            b'!' | b'~' | b'+' | b'-' | b'?' | b':' | b'=' | b'>' | b'<' | b'*' | b'/' | b'%'
            | b'&' | b'|' | b'^' => {
                let slot = current_operators!();
                *slot += 1;
                operators = operators.max(*slot);
            }
            b';' | b',' => {
                *current_chain!() = 0;
                *current_operators!() = 0;
            }
            // The source is written one call per line, so a newline ends the
            // expression only when the next line does not continue it -- and
            // the continuation may be hidden behind a comment, or announced by
            // a trailing `.` on the line just ended.
            b'\n' => {
                let start = index + 1;
                if !(trivia_run.0..=trivia_run.1).contains(&start) {
                    trivia_run = (start, skip_trivia(bytes, start));
                }
                let after_trivia = trivia_run.1;
                let next = bytes.get(after_trivia).copied();
                if previous_significant != b'.' && next != Some(b'.') {
                    *current_chain!() = 0;
                }
                // An operator at the end of this line, or a binary operator
                // at the start of the next line, continues the expression.
                // Otherwise the newline starts a new operator count.
                if !previous_was_word_operator
                    && !word_operator_at(bytes, after_trivia).is_some_and(|(_, binary)| binary)
                    && !matches!(
                        previous_significant,
                        b'!' | b'~'
                            | b'+'
                            | b'-'
                            | b'?'
                            | b':'
                            | b'='
                            | b'>'
                            | b'<'
                            | b'*'
                            | b'/'
                            | b'%'
                            | b'&'
                            | b'|'
                            | b'^'
                    )
                    && !matches!(
                        next,
                        Some(
                            b'+' | b'-'
                                | b'?'
                                | b':'
                                | b'='
                                | b'>'
                                | b'<'
                                | b'*'
                                | b'/'
                                | b'%'
                                | b'&'
                                | b'|'
                                | b'^'
                        )
                    )
                {
                    *current_operators!() = 0;
                }
            }
            _ => {
                if byte.is_ascii_alphabetic()
                    && let Some((width, _)) = word_operator_at(bytes, index)
                {
                    let slot = current_operators!();
                    *slot += 1;
                    operators = operators.max(*slot);
                    index += width - 1;
                    word_operator_here = true;
                }
            }
        }

        // An arm above may have run the index to the end -- an unterminated
        // string or regex does exactly that -- so this cannot index blindly.
        if let Some(&last) = bytes.get(index)
            && !last.is_ascii_whitespace()
        {
            previous_significant = last;
            previous_was_word_operator = word_operator_here;
        }
        index += 1;
    }

    SourceNesting {
        brackets,
        chain,
        operators,
    }
}

/// Refuse source too large or too deeply nested to parse safely.
///
/// The message names what was measured, so a live coder can see which part of
/// the score to unpick rather than only that something was too big.
pub fn check_nesting(source: &str) -> Result<(), ParseDiagnostic> {
    let structural = structural_bytes(source);
    if structural > MAX_STRUCTURAL_BYTES {
        return Err(ParseDiagnostic {
            message: format!(
                "source spends {structural} characters on brackets, dots, and operators, past the {MAX_STRUCTURAL_BYTES} that can be parsed within the stack reserved for it"
            ),
            offset: None,
        });
    }
    let nesting = measure_nesting(source);
    let message = if nesting.brackets > MAX_SOURCE_NESTING {
        format!(
            "source nests brackets {} levels deep, past the {MAX_SOURCE_NESTING} the parser can traverse without overflowing the stack",
            nesting.brackets
        )
    } else if nesting.chain > MAX_SOURCE_NESTING {
        format!(
            "source chains {} calls in one expression, past the {MAX_SOURCE_NESTING} the syntax rewriter can traverse without overflowing the stack",
            nesting.chain
        )
    } else if nesting.operators > MAX_SOURCE_NESTING {
        format!(
            "source chains {} operators in one expression, past the {MAX_SOURCE_NESTING} the parser can traverse without overflowing the stack",
            nesting.operators
        )
    } else {
        return Ok(());
    };
    Err(ParseDiagnostic {
        message,
        offset: None,
    })
}

/// Run `work` on a thread with `PARSE_STACK_BYTES` of stack.
///
/// Every parse in this crate goes through here, except `callback_ir`. That
/// module parses on the caller's stack and bounds its input with
/// `MAX_CALLBACK_IR_SOURCE_BYTES`. The lexical nesting limit stops deep
/// ordinary expressions; the raw size limit also bounds recursion when the
/// lexical scan is defeated. This stack covers the bounded depth.
fn on_parse_stack<T: Send>(work: impl FnOnce() -> T + Send) -> Option<T> {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(PARSE_STACK_BYTES)
            .spawn_scoped(scope, work)
            .ok()
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
            })
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranspileOptions {
    pub wrap_async: bool,
    pub add_return: bool,
    pub emit_mini_locations: bool,
    pub block_offset: usize,
    pub registered_languages: Vec<String>,
    pub widget_methods: Vec<String>,
    pub emit_widgets: bool,
    pub id: Option<String>,
    /// Whether ECMAScript module syntax is accepted by the output parser.
    /// Native score execution disables this because its realm has no module
    /// capability; standalone transpiler callers retain ordinary ES2022
    /// module support by default.
    pub allow_module_syntax: bool,
}

impl Default for TranspileOptions {
    fn default() -> Self {
        Self {
            wrap_async: false,
            add_return: true,
            emit_mini_locations: true,
            block_offset: 0,
            registered_languages: Vec::new(),
            widget_methods: Vec::new(),
            emit_widgets: true,
            id: None,
            allow_module_syntax: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WidgetRecord {
    pub from: usize,
    pub to: usize,
    /// The span a host draws a control over: the whole call for a slider
    /// (`from..to` is only its value), the whole `all(…)` call for an
    /// `all(<painter>)` record, and the receiver chain through the painter
    /// call's closing paren for another visual. A bracket in a comment
    /// inside a bracketed receiver can move the start.
    pub call_from: usize,
    pub call_to: usize,
    pub index: usize,
    pub widget_type: String,
    pub id: String,
    pub visual_slot: Option<u8>,
    pub value: Option<String>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    /// Original bytes between the call parentheses, with only surrounding
    /// whitespace removed. Visual hosts use this as display/configuration
    /// source; it is never evaluated by the transpiler.
    pub options: Option<String>,
    /// The name of the call that takes this slider as its first argument:
    /// `lpf` for `.lpf(slider(800, 100, 4000))`. A host uses it to label
    /// the fader. `None` for a slider that is not the first argument of a
    /// call, and for every widget that is not a slider.
    pub label: Option<String>,
    /// Native hosts defer this capability to the UI while retaining the
    /// complete stable identity and source configuration.
    pub deferred_rendering: bool,
    /// A direct literal slider argument to a frequency control. Native
    /// hosts use this travel hint; it does not alter generated JavaScript.
    pub frequency: bool,
}

/// Pattern methods whose call sites form the native visual layout.
///
/// Both page-level and underscore-prefixed inline spellings are retained.
/// `markcss` is a source-bound highlight directive rather than a canvas, but
/// rides the same layout record so an editor does not have to rediscover its
/// call site by parsing JavaScript independently.
pub const VISUAL_WIDGET_METHODS: &[&str] = &[
    "punchcard",
    "_punchcard",
    "pianoroll",
    "_pianoroll",
    "wordfall",
    "_wordfall",
    "spiral",
    "_spiral",
    "scope",
    "_scope",
    "tscope",
    "_tscope",
    "pitchwheel",
    "_pitchwheel",
    "spectrum",
    "_spectrum",
    "markcss",
];

/// Synthetic widget kind used for the `all(pianoroll)` spelling. It is
/// layout metadata only and is never rewritten into a method invocation.
pub const ALL_VISUAL_WIDGET_PREFIX: &str = "all:";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TranspileOutput {
    pub output: String,
    pub mini_locations: Vec<(usize, usize)>,
    pub diagnostics: Vec<ParseDiagnostic>,
    pub widgets: Vec<WidgetRecord>,
    pub registrations: RegistrationHints,
    /// Positions in `output` mapped back to the score's own lines.
    pub line_map: LineMap,
    /// Each KabelSalat call outside another call's graph, in source order;
    /// their graphs do not overlap. Empty when the source is refused.
    pub kabelsalat_calls: Vec<KabelsalatSpan>,
}

/// Where a KabelSalat call sits in the score, offset by `block_offset`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KabelsalatSpan {
    /// The `K`.
    pub name: std::ops::Range<usize>,
    /// The graph, or the whole argument list when the call collapses to
    /// `K()`: the text `rewrite_kabelsalat` turns into the worklet template
    /// and the patterns lifted from it.
    pub stringified: std::ops::Range<usize>,
}

/// Parse an ES2022 script and print its normalized JavaScript representation.
///
/// Oxc provides complete modern JavaScript syntax, Unicode identifiers,
/// comments, ASI, precedence and source spans. Parse diagnostics are never
/// discarded; callers decide whether recovery is acceptable.
pub fn parse_and_print(source: &str) -> ParseOutput {
    parse_and_print_as(source, SourceType::mjs(), true)
}

pub fn parse_and_print_path(source: &str, path: impl AsRef<Path>) -> ParseOutput {
    match SourceType::from_path(path) {
        Ok(source_type) => parse_and_print_as(source, source_type, true),
        Err(error) => ParseOutput {
            diagnostics: vec![ParseDiagnostic {
                message: error.to_string(),
                offset: None,
            }],
            ..ParseOutput::default()
        },
    }
}

fn parse_and_print_as(
    source: &str,
    source_type: SourceType,
    allow_module_syntax: bool,
) -> ParseOutput {
    // Before the allocator, before the parser: the recursion this refuses
    // would abort the process rather than return a diagnostic.
    if let Err(diagnostic) = check_nesting(source) {
        return ParseOutput {
            diagnostics: vec![diagnostic],
            ..ParseOutput::default()
        };
    }
    let Some(output) =
        on_parse_stack(|| parse_and_print_here(source, source_type, allow_module_syntax))
    else {
        return ParseOutput {
            diagnostics: vec![ParseDiagnostic {
                message: "could not reserve a stack to parse on".into(),
                offset: None,
            }],
            ..ParseOutput::default()
        };
    };
    output
}

/// The deepest nesting of statements and of array and object literals in a
/// parsed program.
///
/// The printer indents each nested block, and wraps a nested `if` that has
/// no `else` in a block of its own, so its output grows with the square of
/// the depth: 65_530 nested `if(1)` would print 4 GB. An `else if` prints
/// flat and adds no level. The printer also indents an array or object
/// literal that it breaks across lines. The lexical scan cannot see braceless
/// nesting, and it skips brackets after a `/` that it reads as a regular
/// expression, so this is measured on the tree.
#[derive(Default)]
struct StatementNesting {
    depth: usize,
    deepest: usize,
}

impl<'a> Visit<'a> for StatementNesting {
    fn visit_statement(&mut self, statement: &Statement<'a>) {
        self.depth += 1;
        self.deepest = self.deepest.max(self.depth);
        walk::walk_statement(self, statement);
        self.depth -= 1;
    }

    fn visit_if_statement(&mut self, statement: &IfStatement<'a>) {
        self.visit_expression(&statement.test);
        self.visit_statement(&statement.consequent);
        match &statement.alternate {
            Some(Statement::IfStatement(chained)) => self.visit_if_statement(chained),
            Some(alternate) => self.visit_statement(alternate),
            None => {}
        }
    }

    fn visit_array_expression(&mut self, array: &ArrayExpression<'a>) {
        self.depth += 1;
        self.deepest = self.deepest.max(self.depth);
        walk::walk_array_expression(self, array);
        self.depth -= 1;
    }

    fn visit_object_expression(&mut self, object: &ObjectExpression<'a>) {
        self.depth += 1;
        self.deepest = self.deepest.max(self.depth);
        walk::walk_object_expression(self, object);
        self.depth -= 1;
    }
}

/// The body of `parse_and_print_as`, always called on a parse stack.
fn parse_and_print_here(
    source: &str,
    source_type: SourceType,
    allow_module_syntax: bool,
) -> ParseOutput {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, source_type).parse();
    let mut nesting = StatementNesting::default();
    nesting.visit_program(&parsed.program);
    if nesting.deepest > MAX_SOURCE_NESTING {
        return ParseOutput {
            diagnostics: vec![ParseDiagnostic {
                message: format!(
                    "source nests statements or literals {} levels deep, past the {MAX_SOURCE_NESTING} the printer can indent without its output outgrowing memory",
                    nesting.deepest
                ),
                offset: None,
            }],
            ..ParseOutput::default()
        };
    }
    let registrations = registration_hints_from_program(&parsed.program);
    let mut diagnostics = parsed
        .diagnostics
        .iter()
        .map(|error| ParseDiagnostic {
            message: error.to_string(),
            offset: error
                .labels
                .iter()
                .find(|label| label.primary())
                .or_else(|| error.labels.first())
                .map(|label| label.offset() as usize),
        })
        .collect::<Vec<_>>();
    let mut program = parsed.program;
    let normalized_jsx_values = if source_type.is_jsx() {
        normalize_jsx_values_with_sites(&allocator, &mut program)
    } else {
        Vec::new()
    };
    if diagnostics.is_empty() && !parsed.panicked {
        diagnostics.extend(
            SemanticBuilder::new_compiler()
                .build(&program)
                .diagnostics
                .iter()
                .map(|error| ParseDiagnostic {
                    message: error.to_string(),
                    offset: error
                        .labels
                        .iter()
                        .find(|label| label.primary())
                        .or_else(|| error.labels.first())
                        .map(|label| label.offset() as usize),
                }),
        );
        if !allow_module_syntax {
            let mut validator = ModuleSyntaxValidator::default();
            validator.visit_program(&program);
            diagnostics.extend(validator.diagnostics);
        }
    }
    if !normalized_jsx_values.is_empty() {
        encode_jsx_values_for_codegen(&allocator, &mut program, &normalized_jsx_values);
    }
    let printed = Codegen::new()
        .with_options(CodegenOptions {
            single_quote: true,
            // Asking for a map is what makes the printer keep one; the
            // path itself is never written anywhere.
            source_map_path: Some(std::path::PathBuf::from("score")),
            ..CodegenOptions::default()
        })
        .build(&program);
    let line_map = LineMap {
        tokens: printed
            .map
            .as_ref()
            .map(|map| {
                map.get_tokens()
                    .map(|token| {
                        (
                            token.get_dst_line(),
                            token.get_dst_col(),
                            token.get_src_line(),
                            token.get_src_col(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default(),
    };
    let code = printed.code.trim_end().to_string();
    ParseOutput {
        code,
        diagnostics,
        panicked: parsed.panicked,
        registrations,
        line_map,
    }
}

/// Collect registration facts from parseable JavaScript. This is public for
/// hosts handling an evaluation error, where no [`TranspileOutput`] is
/// returned to carry the same facts.
pub fn registration_hints(source: &str) -> RegistrationHints {
    parse_and_print(source).registrations
}

/// Reads of names not bound in JavaScript's lexical scopes. Hosts can compare
/// these against their globals without evaluating user code. Property names,
/// labels, declarations and the safe `typeof missing` probe are not reads that
/// require a global to exist.
pub fn unresolved_reads(source: &str) -> Vec<(String, std::ops::Range<usize>)> {
    if check_nesting(source).is_err() {
        return Vec::new();
    }
    on_parse_stack(|| {
        let allocator = Allocator::default();
        let parsed = Parser::new(&allocator, source, SourceType::mjs()).parse();
        if parsed.panicked || !parsed.diagnostics.is_empty() {
            return Vec::new();
        }
        let built = SemanticBuilder::new()
            .with_build_nodes(true)
            .build(&parsed.program);
        let semantic = built.semantic;
        let scoping = semantic.scoping();
        let mut reads = Vec::new();
        for (name, references) in scoping.root_unresolved_references() {
            for &id in references {
                let reference = scoping.get_reference(id);
                if !reference.is_read() {
                    continue;
                }
                // Oxc leaves the implicit function-local `arguments` object
                // unresolved. Arrow functions inherit it from an enclosing
                // ordinary function; a top-level arrow has no such binding.
                if name == "arguments"
                    && scoping
                        .scope_ancestors(semantic.nodes().get_node(reference.node_id()).scope_id())
                        .any(|scope| {
                            let flags = scoping.scope_flags(scope);
                            flags.is_function() && !flags.is_arrow()
                        })
                {
                    continue;
                }
                if let Some(oxc_ast::AstKind::UnaryExpression(expression)) = semantic
                    .nodes()
                    .ancestor_kinds(reference.node_id())
                    .find(|kind| !matches!(kind, oxc_ast::AstKind::ParenthesizedExpression(_)))
                    && expression.operator == oxc_syntax::operator::UnaryOperator::Typeof
                {
                    continue;
                }
                let span = semantic.reference_span(reference);
                reads.push((name.to_string(), span.start as usize..span.end as usize));
            }
        }
        reads.sort_by_key(|(_, span)| span.start);
        reads
    })
    .unwrap_or_default()
}

/// A top-level declaration in a setup file.
///
/// Setup runs in its own function scope, so this name is gone the moment the
/// file finishes: a later score cannot see it. Reported so a checker can say
/// so rather than letting the score fail on a name that looks defined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopLevelDeclaration {
    pub name: String,
    /// `const`, `let`, `var`, `function` or `class`.
    pub kind: &'static str,
    /// Byte range of the name in the source that was read.
    pub from: usize,
    pub to: usize,
}

/// What a setup file leaves behind for the scores that follow it, read from
/// the text alone.
///
/// Only what survives one persistent realm counts: assignments to
/// `globalThis`/`window`, methods hung on `Pattern.prototype`, and
/// `register(...)` names. A declaration is not one of them, which is exactly
/// why they are reported separately.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SetupDefinitions {
    /// Names a later score can call.
    pub names: Vec<String>,
    /// A definition whose name is not in the text - `globalThis[key] = …`,
    /// `Object.assign(globalThis, helpers)`, a computed `register(...)`. A
    /// checker that cannot read the name must not refuse any later call as
    /// unknown.
    pub has_dynamic_names: bool,
    /// Direct children of the program only.
    pub top_level_declarations: Vec<TopLevelDeclaration>,
}

/// Read what a setup file defines, without executing it.
///
/// This parses the original text, not the rewritten text. A caller reports
/// positions in the file that the user sees, and the rewrites move them.
/// Text that does not parse yields nothing: the caller reports the parse
/// error itself, and a setup that does not parse does not run.
pub fn setup_definitions(source: &str) -> SetupDefinitions {
    if check_nesting(source).is_err() {
        return SetupDefinitions::default();
    }
    on_parse_stack(|| {
        let allocator = Allocator::default();
        let parsed = Parser::new(&allocator, source, SourceType::mjs()).parse();
        if parsed.panicked {
            return SetupDefinitions::default();
        }
        let mut collector = SetupCollector::default();
        collector.visit_program(&parsed.program);
        let mut names = collector.names;
        // A registered name is reachable exactly like a global one, so the
        // two lists are one list to a caller.
        names.extend(collector.registrations.names);
        SetupDefinitions {
            names,
            has_dynamic_names: collector.has_dynamic_names
                || collector.registrations.has_dynamic_names,
            top_level_declarations: top_level_declarations(&parsed.program),
        }
    })
    .unwrap_or_default()
}

fn top_level_declarations(program: &Program<'_>) -> Vec<TopLevelDeclaration> {
    let mut declarations = Vec::new();
    let mut push = |name: &str, kind: &'static str, span: oxc_span::Span| {
        declarations.push(TopLevelDeclaration {
            name: name.to_owned(),
            kind,
            from: span.start as usize,
            to: span.end as usize,
        });
    };
    for statement in &program.body {
        match statement {
            Statement::VariableDeclaration(declaration) => {
                let kind = match declaration.kind {
                    VariableDeclarationKind::Var => "var",
                    VariableDeclarationKind::Let => "let",
                    VariableDeclarationKind::Const => "const",
                    // `using` bindings are scoped the same way; naming them
                    // by their keyword keeps the message honest.
                    VariableDeclarationKind::Using => "using",
                    VariableDeclarationKind::AwaitUsing => "await using",
                };
                for declarator in &declaration.declarations {
                    // A destructured binding is skipped: there is no single
                    // name to offer, and the advice would be the same.
                    if let BindingPattern::BindingIdentifier(identifier) = &declarator.id {
                        push(identifier.name.as_str(), kind, identifier.span);
                    }
                }
            }
            Statement::FunctionDeclaration(function) => {
                if let Some(identifier) = &function.id {
                    push(identifier.name.as_str(), "function", identifier.span);
                }
            }
            Statement::ClassDeclaration(class) => {
                if let Some(identifier) = &class.id {
                    push(identifier.name.as_str(), "class", identifier.span);
                }
            }
            _ => {}
        }
    }
    declarations
}

/// Whether an expression names the shared global object under either name.
fn is_shared_global(expression: &Expression<'_>) -> bool {
    expression.is_specific_id("globalThis") || expression.is_specific_id("window")
}

#[derive(Default)]
struct SetupCollector {
    names: Vec<String>,
    has_dynamic_names: bool,
    registrations: RegistrationCollector,
}

impl SetupCollector {
    fn push(&mut self, name: &str) {
        self.names.push(name.to_owned());
    }

    /// The keys of an object literal being merged onto a global. Anything
    /// spread or computed is a name this cannot read.
    fn collect_assign_target(&mut self, argument: Option<&Argument<'_>>) {
        let Some(Argument::ObjectExpression(object)) = argument else {
            self.has_dynamic_names = true;
            return;
        };
        for property in &object.properties {
            match property {
                ObjectPropertyKind::ObjectProperty(property) if !property.computed => {
                    match property.key.static_name() {
                        Some(name) => self.push(&name),
                        None => self.has_dynamic_names = true,
                    }
                }
                _ => self.has_dynamic_names = true,
            }
        }
    }
}

impl<'a> Visit<'a> for SetupCollector {
    fn visit_assignment_expression(&mut self, assignment: &AssignmentExpression<'a>) {
        if let Some(member) = assignment.left.as_member_expression() {
            let object = member.object();
            if is_shared_global(object) || object.is_specific_member_access("Pattern", "prototype")
            {
                match member.static_property_name() {
                    Some(name) => self.push(name),
                    None => self.has_dynamic_names = true,
                }
            }
        }
        walk::walk_assignment_expression(self, assignment);
    }

    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        if is_public_registration_call(call) {
            self.registrations.collect_argument(call.arguments.first());
        } else if call.callee.is_specific_member_access("Object", "assign")
            && call
                .arguments
                .first()
                .and_then(Argument::as_expression)
                .is_some_and(is_shared_global)
        {
            self.collect_assign_target(call.arguments.get(1));
        } else if call
            .callee
            .is_specific_member_access("Object", "defineProperty")
        {
            let target = call.arguments.first().and_then(Argument::as_expression);
            let onto_global = target.is_some_and(|expression| {
                is_shared_global(expression)
                    || expression.is_specific_member_access("Pattern", "prototype")
            });
            if onto_global {
                match call.arguments.get(1).and_then(Argument::as_expression) {
                    Some(Expression::StringLiteral(literal)) => {
                        let name = literal.value.to_string();
                        self.push(&name);
                    }
                    _ => self.has_dynamic_names = true,
                }
            }
        }
        // Always walk on: a setup may define its globals inside a
        // `queueMicrotask` or an `await`ed helper, and those count.
        walk::walk_call_expression(self, call);
    }
}

fn is_public_registration_call(call: &CallExpression<'_>) -> bool {
    call.callee.is_specific_id("register")
        || call
            .callee
            .is_specific_member_access("rustelScope", "register")
        || call
            .callee
            .is_specific_member_access("globalThis", "register")
}

fn static_template_value<'a, 'b>(template: &'b TemplateLiteral<'a>) -> Option<&'b str> {
    if !template.expressions.is_empty() || template.quasis.len() != 1 {
        return None;
    }
    template.quasis[0].value.cooked.map(|value| value.as_str())
}

#[derive(Default)]
struct RegistrationCollector {
    names: Vec<String>,
    has_dynamic_names: bool,
}

impl RegistrationCollector {
    fn push(&mut self, name: &str) {
        // Keep this linear in source size. Hosts already place these names in
        // a set; deduplicating here with a scan would make a setup file full
        // of registrations quadratic before it ever reaches QuickJS.
        self.names.push(name.to_owned());
    }

    fn collect_argument(&mut self, argument: Option<&Argument<'_>>) {
        match argument {
            Some(Argument::StringLiteral(literal)) => self.push(literal.value.as_str()),
            Some(Argument::TemplateLiteral(template)) => {
                if let Some(name) = static_template_value(template) {
                    self.push(name);
                } else {
                    self.has_dynamic_names = true;
                }
            }
            Some(Argument::ArrayExpression(array)) => {
                for element in &array.elements {
                    match element {
                        ArrayExpressionElement::StringLiteral(literal) => {
                            self.push(literal.value.as_str());
                        }
                        ArrayExpressionElement::TemplateLiteral(template) => {
                            if let Some(name) = static_template_value(template) {
                                self.push(name);
                            } else {
                                self.has_dynamic_names = true;
                            }
                        }
                        _ => self.has_dynamic_names = true,
                    }
                }
            }
            _ => self.has_dynamic_names = true,
        }
    }
}

impl<'a> Visit<'a> for RegistrationCollector {
    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        if is_public_registration_call(call) {
            self.collect_argument(call.arguments.first());
        }
        walk::walk_call_expression(self, call);
    }
}

/// Whether a parsed program really awaits.
#[derive(Default)]
struct AwaitFinder {
    found: bool,
}

impl<'a> Visit<'a> for AwaitFinder {
    fn visit_await_expression(&mut self, expression: &AwaitExpression<'a>) {
        self.found = true;
        walk::walk_await_expression(self, expression);
    }

    /// `for await (const x of xs)` is a `ForOfStatement` carrying a flag, not
    /// an `AwaitExpression`, so looking only for the expression missed it and
    /// a valid score failed to compile in the synchronous wrapper.
    fn visit_for_of_statement(&mut self, statement: &ForOfStatement<'a>) {
        self.found = self.found || statement.r#await;
        walk::walk_for_of_statement(self, statement);
    }
}

/// Whether the source contains an `await`, as the parser reads it.
///
/// The host uses this to choose an evaluation wrapper: only a score that
/// awaits needs the asynchronous one, and only that one pumps the job queue,
/// which is the refusal that keeps evaluation bounded.
///
/// A byte scan is not sufficient. A scan that skips comments and quoted
/// literals answers true for `/await/`, `{ await: 1 }` and `x.await`, and
/// each gives the job-pumping path to a score that does not await. It
/// answers false for `` s(`${await p}`) ``: a template literal is quoted but
/// its `${...}` is code, so that valid score fails to compile.
///
/// The answer errs only toward true: an `await` inside a nested
/// `async function` answers true. The cost is the asynchronous wrapper for
/// a score that runs with either wrapper.
pub fn awaits_in_code(source: &str) -> bool {
    if !source.contains("await") {
        // Cheap rejection first: the parse is only worth doing for the few
        // scores where the word appears at all.
        return false;
    }
    if check_nesting(source).is_err() {
        // Unparseable input cannot be judged; assume it awaits so a score that
        // does is never sent to a wrapper that cannot compile it.
        return true;
    }
    on_parse_stack(|| {
        let allocator = Allocator::default();
        let parsed = Parser::new(&allocator, source, SourceType::mjs()).parse();
        if parsed.panicked {
            return true;
        }
        let mut finder = AwaitFinder::default();
        finder.visit_program(&parsed.program);
        finder.found
    })
    .unwrap_or(true)
}

fn registration_hints_from_program(program: &Program<'_>) -> RegistrationHints {
    let mut collector = RegistrationCollector::default();
    collector.visit_program(program);
    let final_expression_is_registration = program.body.last().is_some_and(|statement| {
        let Statement::ExpressionStatement(statement) = statement else {
            return false;
        };
        let Expression::CallExpression(call) = &statement.expression else {
            return false;
        };
        is_public_registration_call(call)
    });
    RegistrationHints {
        names: collector.names,
        has_dynamic_names: collector.has_dynamic_names,
        final_expression_is_registration,
    }
}

#[derive(Default)]
struct ModuleSyntaxValidator {
    diagnostics: Vec<ParseDiagnostic>,
}

impl ModuleSyntaxValidator {
    fn reject(&mut self, offset: u32, message: &'static str) {
        self.diagnostics.push(ParseDiagnostic {
            message: message.to_owned(),
            offset: Some(offset as usize),
        });
    }
}

impl<'a> Visit<'a> for ModuleSyntaxValidator {
    fn visit_import_expression(&mut self, expression: &ImportExpression<'a>) {
        self.reject(
            expression.span.start,
            "dynamic import is disabled in native score code",
        );
    }

    fn visit_import_declaration(&mut self, declaration: &ImportDeclaration<'a>) {
        self.reject(
            declaration.span.start,
            "module declarations are disabled in native score code",
        );
    }

    fn visit_import_meta(&mut self, meta: &ImportMeta) {
        self.reject(
            meta.span.start,
            "import.meta is disabled in native score code",
        );
    }

    fn visit_export_declaration(&mut self, declaration: &ExportDeclaration<'a>) {
        self.reject(
            declaration.span.start,
            "module declarations are disabled in native score code",
        );
    }

    fn visit_export_named_declaration(&mut self, declaration: &ExportNamedDeclaration<'a>) {
        self.reject(
            declaration.span.start,
            "module declarations are disabled in native score code",
        );
    }

    fn visit_export_from_declaration(&mut self, declaration: &ExportFromDeclaration<'a>) {
        self.reject(
            declaration.span.start,
            "module declarations are disabled in native score code",
        );
    }

    fn visit_export_default_declaration(&mut self, declaration: &ExportDefaultDeclaration<'a>) {
        self.reject(
            declaration.span.start,
            "module declarations are disabled in native score code",
        );
    }

    fn visit_export_all_declaration(&mut self, declaration: &ExportAllDeclaration<'a>) {
        self.reject(
            declaration.span.start,
            "module declarations are disabled in native score code",
        );
    }
}

/// Normalize representable JSX text and quoted-attribute entity values to
/// acorn-jsx semantics while retaining each node's raw source and span.
///
/// Oxc 0.144.0 exposes encoded JSX entities as both `raw` and `value`.
/// acorn-jsx 5.3.2 decodes `value` while retaining the encoded `raw`. This also
/// applies acorn-jsx's JSXText-only raw CRLF normalization.
///
/// This public-but-hidden entry point exists only for integration-test AST
/// projection. It does not prepare the mutated tree for Oxc codegen; the
/// production parse/print path immediately follows normalization with the
/// private, site-scoped encoder above.
#[doc(hidden)]
pub fn normalize_jsx_entities<'a>(allocator: &'a Allocator, program: &mut Program<'a>) {
    let _ = normalize_jsx_values_with_sites(allocator, program);
}

fn normalize_jsx_values_with_sites<'a>(
    allocator: &'a Allocator,
    program: &mut Program<'a>,
) -> Vec<NormalizedJsxValue> {
    let mut normalizer = JsxValueNormalizer {
        allocator,
        normalized: Vec::new(),
    };
    normalizer.visit_program(program);
    normalizer.normalized.sort_unstable();
    normalizer.normalized.dedup();
    normalizer.normalized
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct NormalizedJsxValue {
    start: u32,
    end: u32,
    attribute: bool,
}

struct JsxValueNormalizer<'a> {
    allocator: &'a Allocator,
    normalized: Vec<NormalizedJsxValue>,
}

impl<'a> JsxValueNormalizer<'a> {
    fn decoded_attribute(&self, value: &str) -> Option<&'a str> {
        decode_acorn_jsx_entities(value).map(|decoded| self.allocator.alloc_str(&decoded))
    }

    fn decoded_text(&self, value: &str) -> Option<&'a str> {
        decode_acorn_jsx_text(value).map(|decoded| self.allocator.alloc_str(&decoded))
    }
}

impl<'a> VisitMut<'a> for JsxValueNormalizer<'a> {
    fn visit_jsx_text(&mut self, text: &mut JSXText<'a>) {
        if let Some(decoded) = self.decoded_text(text.value.as_str()) {
            text.value = decoded.into();
            self.normalized.push(NormalizedJsxValue {
                start: text.span.start,
                end: text.span.end,
                attribute: false,
            });
        }
        walk_mut::walk_jsx_text(self, text);
    }

    fn visit_jsx_attribute_value(&mut self, value: &mut JSXAttributeValue<'a>) {
        if let JSXAttributeValue::StringLiteral(literal) = value
            && let Some(decoded) = self.decoded_attribute(literal.value.as_str())
        {
            literal.value = decoded.into();
            self.normalized.push(NormalizedJsxValue {
                start: literal.span.start,
                end: literal.span.end,
                attribute: true,
            });
        }
        walk_mut::walk_jsx_attribute_value(self, value);
    }
}

/// Oxc's JSX printer writes `value` verbatim rather than consulting `raw` or
/// escaping syntax delimiters. Re-encode only characters that would otherwise
/// change tokenization; reparsing the printed source then reconstructs the
/// normalized Acorn value without double-decoding a literal ampersand.
fn encode_jsx_values_for_codegen<'a>(
    allocator: &'a Allocator,
    program: &mut Program<'a>,
    normalized: &[NormalizedJsxValue],
) {
    JsxCodegenEncoder {
        allocator,
        normalized,
    }
    .visit_program(program);
}

struct JsxCodegenEncoder<'a, 'sites> {
    allocator: &'a Allocator,
    normalized: &'sites [NormalizedJsxValue],
}

impl<'a> JsxCodegenEncoder<'a, '_> {
    fn encoded(&self, value: &str, start: u32, end: u32, attribute: bool) -> Option<&'a str> {
        let site = NormalizedJsxValue {
            start,
            end,
            attribute,
        };
        if self.normalized.binary_search(&site).is_err() {
            return None;
        }
        let mut output = String::with_capacity(value.len());
        let mut changed = false;
        for character in value.chars() {
            let entity = match character {
                '&' => Some("&amp;"),
                '<' => Some("&lt;"),
                '>' => Some("&gt;"),
                '{' => Some("&#123;"),
                '}' => Some("&#125;"),
                '"' if attribute => Some("&quot;"),
                '\'' if attribute => Some("&apos;"),
                '\r' => Some("&#13;"),
                _ => None,
            };
            if let Some(entity) = entity {
                output.push_str(entity);
                changed = true;
            } else {
                output.push(character);
            }
        }
        changed.then(|| self.allocator.alloc_str(&output) as &'a str)
    }
}

impl<'a> VisitMut<'a> for JsxCodegenEncoder<'a, '_> {
    fn visit_jsx_text(&mut self, text: &mut JSXText<'a>) {
        if let Some(encoded) =
            self.encoded(text.value.as_str(), text.span.start, text.span.end, false)
        {
            text.value = encoded.into();
        }
        walk_mut::walk_jsx_text(self, text);
    }

    fn visit_jsx_attribute_value(&mut self, value: &mut JSXAttributeValue<'a>) {
        if let JSXAttributeValue::StringLiteral(literal) = value
            && let Some(encoded) = self.encoded(
                literal.value.as_str(),
                literal.span.start,
                literal.span.end,
                true,
            )
        {
            literal.value = encoded.into();
        }
        walk_mut::walk_jsx_attribute_value(self, value);
    }
}

/// Decode the Unicode-scalar entity values accepted by acorn-jsx 5.3.2.
///
/// In particular, its lexer examines at most ten UTF-16 code units including
/// the semicolon and uses `String.fromCharCode` for numeric entities. The
/// latter intentionally wraps `&#x1F600;` to U+F600 rather than producing the
/// Unicode scalar U+1F600. The pinned 253-name table is shared with Oxc, but
/// the scan limit and numeric rules still have to be applied here. A lone
/// UTF-16 surrogate has no Rust `str` representation and remains encoded.
fn decode_acorn_jsx_entities(value: &str) -> Option<String> {
    decode_acorn_jsx_value(value, false)
}

/// acorn-jsx normalizes a raw CRLF pair in JSX text to one LF while leaving
/// CR/LF code units produced by entities untouched. Quoted attribute strings
/// use a different lexer path and therefore do not use this normalization.
fn decode_acorn_jsx_text(value: &str) -> Option<String> {
    decode_acorn_jsx_value(value, true)
}

fn decode_acorn_jsx_value(value: &str, normalize_raw_crlf: bool) -> Option<String> {
    let bytes = value.as_bytes();
    let mut output = Vec::<u16>::with_capacity(value.len());
    let mut cursor = 0usize;
    let mut changed = false;

    while let Some(relative_amp) = value[cursor..].find('&') {
        let amp = cursor + relative_amp;
        extend_raw_jsx_units(
            &mut output,
            &value[cursor..amp],
            normalize_raw_crlf,
            &mut changed,
        );
        let word_start = amp + 1;
        let mut scan = word_start;
        let mut units = 0usize;
        let mut semicolon = None;
        while scan < bytes.len() && units < 10 {
            let byte = bytes[scan];
            if !byte.is_ascii() {
                break;
            }
            scan += 1;
            units += 1;
            if byte == b';' {
                semicolon = Some(scan - 1);
                break;
            }
        }

        let decoded = semicolon.and_then(|end| decode_acorn_jsx_entity(&value[word_start..end]));
        if let (Some(end), Some(decoded)) = (semicolon, decoded) {
            match decoded {
                DecodedJsxEntity::CodePoint(code_point) if code_point <= u32::from(u16::MAX) => {
                    output.push(code_point as u16);
                }
                DecodedJsxEntity::CodePoint(code_point) => {
                    let character = char::from_u32(code_point)?;
                    let mut encoded = [0u16; 2];
                    output.extend(character.encode_utf16(&mut encoded).iter().copied());
                }
                DecodedJsxEntity::Text(text) => output.extend(text.encode_utf16()),
            }
            cursor = end + 1;
            changed = true;
        } else {
            // acorn-jsx resets to immediately after `&` when decoding fails;
            // the remaining source is then lexed normally.
            output.push(b'&'.into());
            cursor = word_start;
        }
    }
    extend_raw_jsx_units(
        &mut output,
        &value[cursor..],
        normalize_raw_crlf,
        &mut changed,
    );
    changed.then(|| String::from_utf16(&output).ok()).flatten()
}

fn extend_raw_jsx_units(
    output: &mut Vec<u16>,
    source: &str,
    normalize_raw_crlf: bool,
    changed: &mut bool,
) {
    if !normalize_raw_crlf || !source.contains("\r\n") {
        output.extend(source.encode_utf16());
        return;
    }

    let mut remainder = source;
    while let Some(crlf) = remainder.find("\r\n") {
        output.extend(remainder[..crlf].encode_utf16());
        output.push(u16::from(b'\n'));
        remainder = &remainder[crlf + 2..];
        *changed = true;
    }
    output.extend(remainder.encode_utf16());
}

enum DecodedJsxEntity {
    CodePoint(u32),
    Text(&'static str),
}

fn decode_acorn_jsx_entity(word: &str) -> Option<DecodedJsxEntity> {
    if let Some(digits) = word.strip_prefix("#x") {
        return decode_acorn_numeric_entity(digits, 16);
    }
    if let Some(digits) = word.strip_prefix('#') {
        return decode_acorn_numeric_entity(digits, 10);
    }

    match word {
        // acorn-jsx indexes its ordinary object without an own-property guard;
        // these three Object.prototype names fit inside its ten-unit scan.
        "toString" => Some(DecodedJsxEntity::Text(
            "function toString() { [native code] }",
        )),
        "valueOf" => Some(DecodedJsxEntity::Text(
            "function valueOf() { [native code] }",
        )),
        "__proto__" => Some(DecodedJsxEntity::Text("[object Object]")),
        _ => XML_ENTITIES
            .get(word)
            .map(|character| DecodedJsxEntity::CodePoint(u32::from(*character))),
    }
}

fn decode_acorn_numeric_entity(digits: &str, radix: u32) -> Option<DecodedJsxEntity> {
    if digits.is_empty()
        || !digits.bytes().all(|byte| match radix {
            16 => byte.is_ascii_hexdigit(),
            _ => byte.is_ascii_digit(),
        })
    {
        return None;
    }
    Some(DecodedJsxEntity::CodePoint(u32::from(
        u64::from_str_radix(digits, radix).ok()? as u16,
    )))
}

/// Native source transforms implemented before QuickJS evaluation.
///
/// This performs the pinned mini-literal and bare-`samples` transforms, then
/// round-trips through the Rust ES parser/printer. A tagged template with no
/// `${...}` substitution whose tag is a registered language becomes a call to
/// that handler. Every other tagged template stays as written.
pub fn transpile(source: &str, options: &TranspileOptions) -> TranspileOutput {
    // `collect_widgets` and the rewriters below each parse, so the nesting
    // check has to precede them rather than sit in `parse_and_print_as`.
    let refuse = |message: String| TranspileOutput {
        diagnostics: vec![ParseDiagnostic {
            message,
            offset: None,
        }],
        ..TranspileOutput::default()
    };
    if let Err(diagnostic) = check_nesting(source) {
        return refuse(diagnostic.message);
    }
    let Some(output) = on_parse_stack(|| transpile_here(source, options)) else {
        return refuse("could not reserve a stack to parse on".into());
    };
    output
}

/// The body of `transpile`, always called on a parse stack.
fn transpile_here(source: &str, options: &TranspileOptions) -> TranspileOutput {
    let regexes = RegexLiteralRanges::parse(source);
    let ordinary_strings = OrdinaryStringLiteralRanges::parse(source);
    // These transforms may move or consume a regex, but none can create one.
    // Reuse a proven-empty result so ordinary division does not trigger an
    // extra full parse at every stage; real regexes refresh their moved spans.
    let regexes_are_present = !regexes.is_empty();
    let kabelsalat = kabelsalat_calls(source, &regexes);
    let (mut widgets, sites): (Vec<_>, Vec<_>) =
        collect_widgets(source, options, &regexes, &kabelsalat)
            .into_iter()
            .unzip();
    frequency_sliders::annotate(source, &mut widgets, options.block_offset);
    let mut visual_slot = 0usize;
    for widget in &mut widgets {
        if !widget.widget_type.starts_with(ALL_VISUAL_WIDGET_PREFIX)
            && widget.widget_type != "markcss"
            && VISUAL_WIDGET_METHODS.contains(&widget.widget_type.as_str())
        {
            widget.visual_slot = u8::try_from(visual_slot).ok();
            visual_slot = visual_slot.saturating_add(1);
        }
    }
    let MiniRewrite {
        output: mini,
        locations: mini_locations,
        mut diagnostics,
        map: mini_map,
    } = rewrite_mini_literals(source, options, &regexes, &ordinary_strings);
    // The widget splice runs while its sites are one map from the source,
    // before `rewrite_kabelsalat` moves them.
    let widget_source = rewrite_widget_calls(&mini, &mini_map, &widgets, &sites);
    let kabel = rewrite_kabelsalat(&widget_source, regexes_are_present);
    let labels = rewrite_labels(&kabel, regexes_are_present);
    let samples = rewrite_bare_samples(&labels);
    let mut parsed = parse_and_print_as(&samples, SourceType::mjs(), options.allow_module_syntax);
    // The parser read the rewritten text; the score is what the reader sees.
    for diagnostic in &mut parsed.diagnostics {
        if let Some(offset) = diagnostic.offset {
            diagnostic.offset = Some(rewritten_to_source_offset(source, &samples, offset));
        }
    }
    let mut output = normalize_leading_decimals(&parsed.code, regexes_are_present);
    diagnostics.extend(parsed.diagnostics);
    if diagnostics.is_empty() && !parsed.panicked && options.add_return {
        output = add_final_return(&output);
    }
    if options.wrap_async {
        output = format!("(async ()=>{{{output}}})()");
    }
    TranspileOutput {
        output,
        mini_locations,
        diagnostics,
        line_map: parsed.line_map,
        registrations: parsed.registrations,
        widgets: if options.emit_widgets {
            widgets
        } else {
            Vec::new()
        },
        kabelsalat_calls: kabelsalat
            .iter()
            .map(|call| {
                let offset = |range: std::ops::Range<usize>| {
                    range.start + options.block_offset..range.end + options.block_offset
                };
                KabelsalatSpan {
                    name: offset(call.name..call.name + 1),
                    stringified: offset(call.stringified()),
                }
            })
            .collect(),
    }
}

/// A parse error is found in the rewritten text but belongs to the score.
///
/// Every rewrite keeps the line structure, so the line is the score's own.
/// Within the line the rewrites change widths: a mini-notation string
/// becomes an `m(...)` call, a label loses its `name:`, a lane gains a
/// `.p(...)`. An unchanged offset would point to the right of the fault by
/// the width of every rewrite before it. The column therefore goes through
/// the longest common subsequence of the two lines. A position on shared
/// text maps to that text. A position inside a rewrite maps to the next
/// shared character after it.
fn rewritten_to_source_offset(source: &str, rewritten: &str, offset: usize) -> usize {
    let mut offset = offset.min(rewritten.len());
    while !rewritten.is_char_boundary(offset) {
        offset -= 1;
    }
    let line_index = rewritten[..offset].matches('\n').count();
    let line_start = rewritten[..offset].rfind('\n').map_or(0, |at| at + 1);
    let rewritten_line = rewritten[line_start..]
        .split('\n')
        .next()
        .unwrap_or_default();
    let Some(source_line) = source.split('\n').nth(line_index) else {
        return source.len();
    };
    let source_line_start = source
        .split('\n')
        .take(line_index)
        .map(|line| line.len() + 1)
        .sum::<usize>();
    let column = align_column(source_line, rewritten_line, offset - line_start);
    (source_line_start + column).min(source.len())
}

/// The byte column on `source_line` that the byte column `column` of
/// `rewritten_line` corresponds to.
fn align_column(source_line: &str, rewritten_line: &str, column: usize) -> usize {
    let a: Vec<char> = source_line.chars().collect();
    let b: Vec<char> = rewritten_line.chars().collect();
    let wanted = rewritten_line[..column.min(rewritten_line.len())]
        .chars()
        .count();
    let byte_at = |index: usize| {
        source_line
            .char_indices()
            .nth(index)
            .map_or(source_line.len(), |(byte, _)| byte)
    };
    if wanted >= b.len() {
        return source_line.len();
    }
    let (n, m) = (a.len(), b.len());
    if n * m > 250_000 {
        // Too long a line to align in full: what both lines start and end
        // with is still the same text.
        let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
        if wanted <= prefix {
            return byte_at(wanted);
        }
        let suffix = a
            .iter()
            .rev()
            .zip(b.iter().rev())
            .take_while(|(x, y)| x == y)
            .count()
            .min(n - prefix)
            .min(m - prefix);
        if wanted >= m - suffix {
            return byte_at(n - (m - wanted));
        }
        return byte_at(prefix);
    }
    // Longest common subsequence, then a walk along it.
    let mut table = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if a[i] == b[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            if j >= wanted {
                return byte_at(i);
            }
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    byte_at(i)
}

fn normalize_leading_decimals(source: &str, regexes_are_present: bool) -> String {
    let bytes = source.as_bytes();
    let regexes = RegexLiteralRanges::parse_if_present(source, regexes_are_present);
    let mut output = String::with_capacity(source.len() + 4);
    let mut index = 0usize;
    while index < bytes.len() {
        if matches!(bytes[index], b'\'' | b'"' | b'`') {
            let end = scan_quoted(bytes, index, bytes[index]);
            output.push_str(&source[index..end]);
            index = end;
            continue;
        }
        if bytes[index..].starts_with(b"//") || bytes[index..].starts_with(b"/*") {
            let end = scan_comment(source, index);
            output.push_str(&source[index..end]);
            index = end;
            continue;
        }
        if let Some(end) = regexes.end_at(index) {
            output.push_str(&source[index..end]);
            index = end;
            continue;
        }
        if bytes[index] == b'.'
            && bytes.get(index + 1).is_some_and(u8::is_ascii_digit)
            && !bytes
                .get(index.wrapping_sub(1))
                .is_some_and(|byte| is_identifier_byte(byte) || matches!(byte, b')' | b']'))
        {
            output.push('0');
        }
        let ch = source[index..]
            .chars()
            .next()
            .expect("index remains on a UTF-8 boundary");
        output.push(ch);
        index += ch.len_utf8();
    }
    output
}

/// Maps source positions in the text `rewrite_mini_literals` copied to their
/// positions in its output.
struct RewriteMap {
    /// `(source offset, output offset)` at the start of each copied stretch,
    /// ascending in both and starting at `(0, 0)`.
    copied: Vec<(usize, usize)>,
}

impl RewriteMap {
    fn new() -> Self {
        Self {
            copied: vec![(0, 0)],
        }
    }

    /// Starts a copied stretch after a replaced literal.
    fn resume(&mut self, source: usize, output: usize) {
        self.copied.push((source, output));
    }

    /// The output position of `source`, which must lie in copied text or on
    /// the first byte of a replaced literal. A position strictly inside a
    /// replaced literal has no counterpart, and the result is meaningless.
    fn to_rewritten(&self, source: usize) -> usize {
        let stretch = self.copied.partition_point(|&(start, _)| start <= source) - 1;
        let (start, output) = self.copied[stretch];
        output + (source - start)
    }
}

/// The result of `rewrite_mini_literals`.
struct MiniRewrite {
    /// The source with each mini-notation literal and registered-language
    /// template rewritten as a call.
    output: String,
    /// Score spans of the mini-notation leaves.
    locations: Vec<(usize, usize)>,
    /// Literals that failed to decode; each is copied as written.
    diagnostics: Vec<ParseDiagnostic>,
    /// Positions of the copied source text in `output`.
    map: RewriteMap,
}

fn rewrite_mini_literals(
    source: &str,
    options: &TranspileOptions,
    regexes: &RegexLiteralRanges,
    ordinary_strings: &OrdinaryStringLiteralRanges,
) -> MiniRewrite {
    let bytes = source.as_bytes();
    let mut output = String::with_capacity(source.len() + 32);
    let mut locations = Vec::new();
    let mut diagnostics = Vec::new();
    let mut map = RewriteMap::new();
    let mut index = 0;
    let mut mini_enabled = true;
    let mut last_comment_end = 0usize;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"//") || bytes[index..].starts_with(b"/*") {
            let block = bytes[index..].starts_with(b"/*");
            let end = if block {
                source[index + 2..]
                    .find("*/")
                    .map(|offset| index + 2 + offset + 2)
                    .unwrap_or(source.len())
            } else {
                line_terminator_bytes(bytes, index + 2)
            };
            let comment = &source[index..end];
            // A comment toggles mini notation only when its trimmed text
            // starts with a directive, as upstream's `findMiniDisableRanges`
            // reads it; prose that mentions one changes nothing.
            let directive = comment[2..].trim();
            if directive.starts_with("mini-off") {
                mini_enabled = false;
            } else if directive.starts_with("mini-on") {
                mini_enabled = true;
            }
            output.push_str(comment);
            index = end;
            last_comment_end = end;
            continue;
        }
        if let Some(end) = regexes.end_at(index) {
            output.push_str(&source[index..end]);
            index = end;
            continue;
        }
        let quote = bytes[index];
        if quote == b'\'' || quote == b'"' || quote == b'`' {
            let (end, closed) = scan_quoted_with_status(bytes, index, quote);
            if !closed {
                // Leave incomplete source for the parser to diagnose. At EOF,
                // subtracting one byte may land inside a UTF-8 character.
                output.push_str(&source[index..end]);
                break;
            }
            let raw = &source[index + 1..end - 1];
            let tag = (quote == b'`')
                .then(|| preceding_identifier_bounded(source, index, last_comment_end))
                .flatten();
            let tagged = tag.is_some();
            let interpolated = quote == b'`' && has_unescaped_template_interpolation(raw);
            // Rewrite only registered language tags. Other tags keep their
            // JavaScript meaning, including errors for undefined names.
            let language = tag.as_deref().is_some_and(|name| {
                options
                    .registered_languages
                    .iter()
                    .any(|registered| registered == name)
            });
            if language && !interpolated {
                // The tag and any whitespace before the template are
                // already copied; the template becomes the argument list.
                output.push('(');
                output.push_str(&quote_single(raw));
                output.push_str(", ");
                if options.emit_mini_locations {
                    output.push_str(&(index + 1 + options.block_offset).to_string());
                } else {
                    // Tagged-language handlers share the host's -1
                    // no-location contract with ordinary mini notation.
                    output.push_str("-1");
                }
                restore_line_breaks(&mut output, raw);
                output.push(')');
                map.resume(end, output.len());
            } else if mini_enabled
                && !ordinary_strings.contains(index)
                && (quote == b'"' || (quote == b'`' && !tagged && !interpolated))
            {
                let value = match decode_js_string(raw, quote == b'`') {
                    Ok(value) => value,
                    Err(error) => {
                        diagnostics.push(ParseDiagnostic {
                            message: error.message,
                            offset: Some(index + 1 + error.offset),
                        });
                        output.push_str(&source[index..end]);
                        index = end;
                        continue;
                    }
                };
                if options.emit_mini_locations
                    && let Ok(mut spans) =
                        rustel_mini::leaf_locations(&value, index + 1 + options.block_offset)
                {
                    locations.append(&mut spans);
                }
                output.push_str("m(");
                output.push_str(&quote_single(&value));
                output.push_str(", ");
                if options.emit_mini_locations {
                    output.push_str(&(index + options.block_offset).to_string());
                } else {
                    // Setup/prebake code does not belong to the active editor
                    // document. The host treats -1 as "parse without source
                    // locations" so its offsets cannot impersonate score
                    // ranges later when a helper returns this pattern.
                    output.push_str("-1");
                }
                restore_line_breaks(&mut output, raw);
                output.push(')');
                map.resume(end, output.len());
            } else {
                output.push_str(&source[index..end]);
            }
            index = end;
            continue;
        }
        let ch = source[index..].chars().next().unwrap();
        output.push(ch);
        index += ch.len_utf8();
    }
    MiniRewrite {
        output,
        locations,
        diagnostics,
        map,
    }
}

/// Appends the CR and LF line breaks written in `raw`, the body of a literal
/// replaced by a call whose single-quoted string escapes them.
///
/// Emitted inside the call's argument list, where no line break ends a
/// statement, they keep the rewritten text on the score's lines, which
/// [`rewritten_to_source_offset`] and the printer's [`LineMap`] count on. An
/// escaped `\n` is not a line break and adds none.
fn restore_line_breaks(output: &mut String, raw: &str) {
    output.extend(raw.chars().filter(|ch| matches!(ch, '\r' | '\n')));
}

/// A KabelSalat call, `K(…)` or `x.K(…)`.
struct KabelsalatCall<'a> {
    /// Position of the `K`.
    name: usize,
    open: usize,
    close: usize,
    /// The first top-level argument, as written.
    first: &'a str,
}

impl KabelsalatCall<'_> {
    /// The first argument, which `rewrite_kabelsalat` turns into the worklet
    /// template and its patterns, or `None` when it is empty and the call
    /// collapses to `K()`.
    fn graph(&self) -> Option<&str> {
        Some(self.first).filter(|graph| !graph.trim().is_empty())
    }

    /// The graph, or the whole argument list when the call collapses to
    /// `K()`: the text `rewrite_kabelsalat` turns into the worklet template
    /// and the patterns lifted from it. Arguments after a graph stay code.
    fn stringified(&self) -> std::ops::Range<usize> {
        match self.graph() {
            Some(graph) => self.open + 1..self.open + 1 + graph.len(),
            None => self.open + 1..self.close,
        }
    }
}

#[cfg(test)]
std::thread_local! {
    /// The bytes of source handed to `kabelsalat_calls`.
    static KABELSALAT_LISTED_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Each KabelSalat call in `source` outside another one's `stringified`
/// span, in source order.
fn kabelsalat_calls<'a>(source: &'a str, regexes: &RegexLiteralRanges) -> Vec<KabelsalatCall<'a>> {
    #[cfg(test)]
    KABELSALAT_LISTED_BYTES.with(|listed| listed.set(listed.get() + source.len()));
    let bytes = source.as_bytes();
    let mut calls = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if matches!(bytes[index], b'\'' | b'"' | b'`') {
            index = scan_quoted(bytes, index, bytes[index]);
            continue;
        }
        if bytes[index..].starts_with(b"//") || bytes[index..].starts_with(b"/*") {
            index = scan_comment(source, index);
            continue;
        }
        if let Some(end) = regexes.end_at(index) {
            index = end;
            continue;
        }
        if bytes[index] != b'K'
            || bytes.get(index + 1).is_some_and(is_identifier_byte)
            || bytes
                .get(index.wrapping_sub(1))
                .is_some_and(is_identifier_byte)
        {
            index += 1;
            continue;
        }
        let mut open = index + 1;
        while bytes.get(open).is_some_and(u8::is_ascii_whitespace) {
            open += 1;
        }
        if bytes.get(open) != Some(&b'(') {
            index += 1;
            continue;
        }
        let Some(close) = matching_delimiter(bytes, open, b'(', b')', regexes) else {
            break;
        };
        let call = KabelsalatCall {
            name: index,
            open,
            close,
            first: split_top_level(&source[open + 1..close], ',', regexes, open + 1)
                .first()
                .copied()
                .unwrap_or_default(),
        };
        // A `K(` in the arguments after the graph is a call of its own.
        index = call.stringified().end;
        calls.push(call);
    }
    calls
}

/// `source` with each KabelSalat call outside another's graph rewritten to
/// `worklet('<template>', <patterns>…)`, or to `K()` when its graph is empty.
///
/// The `m(…)` and `S(…)` patterns in a graph are lifted out as code, and the
/// rest of the graph is reprinted and quoted as the template. A `K(` inside
/// a graph or a lifted pattern is left as written; the runtime refuses `K`
/// as it refuses `worklet`. KabelSalat calls in the later arguments are
/// rewritten from the same listing, so the output stays linear in the source.
fn rewrite_kabelsalat(source: &str, regexes_are_present: bool) -> String {
    let regexes = RegexLiteralRanges::parse_if_present(source, regexes_are_present);
    let mut output = String::with_capacity(source.len() + 32);
    let mut cursor = 0usize;
    for call in kabelsalat_calls(source, &regexes) {
        debug_assert!(cursor <= call.name, "KabelSalat calls out of source order");
        // Text up to the `K` is kept, including any receiver.
        output.push_str(&source[cursor..call.name]);
        let Some(graph) = call.graph() else {
            output.push_str("K()");
            cursor = call.close + 1;
            continue;
        };
        let (template, patterns) = extract_pattern_placeholders(graph.trim(), regexes_are_present);
        let template = if patterns.is_empty() {
            normalize_kabel_expression(&template)
        } else {
            template
        };
        output.push_str("worklet(");
        output.push_str(&quote_single(&template));
        for pattern in patterns {
            output.push_str(", ");
            output.push_str(&pattern);
        }
        // The later arguments and the closing paren are copied as written;
        // a KabelSalat call among them comes next in the list.
        cursor = call.stringified().end;
    }
    output.push_str(&source[cursor..]);
    output
}

fn extract_pattern_placeholders(
    expression: &str,
    regexes_are_present: bool,
) -> (String, Vec<String>) {
    let bytes = expression.as_bytes();
    let regexes = RegexLiteralRanges::parse_if_present(expression, regexes_are_present);
    let mut template = String::with_capacity(expression.len());
    let mut patterns = Vec::new();
    let mut cursor = 0usize;
    let mut index = 0usize;
    while index < bytes.len() {
        if matches!(bytes[index], b'\'' | b'"' | b'`') {
            index = scan_quoted(bytes, index, bytes[index]);
            continue;
        }
        if bytes[index..].starts_with(b"//") || bytes[index..].starts_with(b"/*") {
            index = scan_comment(expression, index);
            continue;
        }
        if let Some(end) = regexes.end_at(index) {
            index = end;
            continue;
        }
        let name = if bytes[index..].starts_with(b"m(") {
            Some(("m", index + 1))
        } else if bytes[index..].starts_with(b"S(")
            && !bytes
                .get(index.wrapping_sub(1))
                .is_some_and(is_identifier_byte)
        {
            Some(("S", index + 1))
        } else {
            None
        };
        let Some((name, open)) = name else {
            index += 1;
            continue;
        };
        let Some(close) = matching_delimiter(bytes, open, b'(', b')', &regexes) else {
            break;
        };
        template.push_str(&expression[cursor..index]);
        template.push_str(&format!("pat[{}]", patterns.len()));
        let pattern = if name == "S" {
            split_top_level(&expression[open + 1..close], ',', &regexes, open + 1)
                .first()
                .map(|arg| arg.trim().to_string())
                .unwrap_or_default()
        } else {
            expression[index..=close].to_string()
        };
        patterns.push(pattern);
        cursor = close + 1;
        index = cursor;
    }
    template.push_str(&expression[cursor..]);
    (normalize_kabel_expression(&template), patterns)
}

fn normalize_kabel_expression(expression: &str) -> String {
    let trimmed = expression.trim();
    let should_call =
        (trimmed.starts_with("() =>") || trimmed.starts_with("()=>")) && trimmed.contains('{');
    let candidate = if should_call {
        format!("({trimmed})()")
    } else {
        trimmed.to_string()
    };
    let parsed = parse_and_print(&candidate);
    let mut normalized = parsed
        .code
        .strip_suffix(';')
        .unwrap_or(&parsed.code)
        .to_string();
    if should_call {
        normalized = normalized.replace('\t', "    ").replace(";\n}", "\n}");
    }
    normalized
}

fn matching_delimiter(
    bytes: &[u8],
    open: usize,
    left: u8,
    right: u8,
    regexes: &RegexLiteralRanges,
) -> Option<usize> {
    let mut depth = 0usize;
    let mut index = open;
    while index < bytes.len() {
        if matches!(bytes[index], b'\'' | b'"' | b'`') {
            index = scan_quoted(bytes, index, bytes[index]);
            continue;
        }
        if bytes[index..].starts_with(b"//") {
            index = line_terminator_bytes(bytes, index + 2);
            continue;
        }
        if bytes[index..].starts_with(b"/*") {
            index = bytes[index + 2..]
                .windows(2)
                .position(|window| window == b"*/")
                .map_or(bytes.len(), |offset| index + offset + 4);
            continue;
        }
        if let Some(end) = regexes.end_at(index) {
            index = end;
            continue;
        }
        if bytes[index] == left {
            depth += 1;
        } else if bytes[index] == right {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
        }
        index += 1;
    }
    None
}

fn split_top_level<'a>(
    source: &'a str,
    delimiter: char,
    regexes: &RegexLiteralRanges,
    source_offset: usize,
) -> Vec<&'a str> {
    let bytes = source.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut depth = 0i32;
    let mut index = 0usize;
    while index < bytes.len() {
        if matches!(bytes[index], b'\'' | b'"' | b'`') {
            index = scan_quoted(bytes, index, bytes[index]);
            continue;
        }
        if bytes[index..].starts_with(b"//") || bytes[index..].starts_with(b"/*") {
            index = scan_comment(source, index);
            continue;
        }
        if let Some(end) = regexes.end_at(source_offset + index) {
            index = end - source_offset;
            continue;
        }
        match bytes[index] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            byte if char::from(byte) == delimiter && depth == 0 => {
                parts.push(&source[start..index]);
                start = index + 1;
            }
            _ => {}
        }
        index += 1;
    }
    if start < source.len() || !source.is_empty() {
        parts.push(&source[start..]);
    }
    parts
}

/// The byte offset of the first JavaScript line terminator at or after
/// `start`, or `bytes.len()`.
///
/// JavaScript terminates a `//` comment on CR, LF and the U+2028/U+2029 line
/// separators, so the scanners must too: code after a separator inside what a
/// naive scan calls a comment is live source.
fn line_terminator_bytes(bytes: &[u8], start: usize) -> usize {
    let mut index = start;
    while index < bytes.len() {
        if is_line_terminator_at(bytes, index) {
            return index;
        }
        index += 1;
    }
    bytes.len()
}

fn is_line_terminator_at(bytes: &[u8], index: usize) -> bool {
    line_terminator_width_at(bytes, index) != 0
}

fn line_terminator_width_at(bytes: &[u8], index: usize) -> usize {
    match bytes.get(index) {
        Some(b'\r') if bytes.get(index + 1) == Some(&b'\n') => 2,
        Some(b'\r' | b'\n') => 1,
        _ if bytes[index..].starts_with(b"\xe2\x80\xa8")
            || bytes[index..].starts_with(b"\xe2\x80\xa9") =>
        {
            3
        }
        _ => 0,
    }
}

/// Whether `byte` can continue an ASCII JavaScript identifier.
fn is_identifier_byte(byte: &u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$')
}

fn scan_comment(source: &str, index: usize) -> usize {
    if source.as_bytes()[index..].starts_with(b"/*") {
        source[index + 2..]
            .find("*/")
            .map(|offset| index + offset + 4)
            .unwrap_or(source.len())
    } else {
        // `scan_comment` is only called with ASCII `/` starts, which are
        // single-byte, so the char boundary is already valid.
        line_terminator_bytes(source.as_bytes(), index + 2)
    }
}

/// How many of the `earlier` records have `widget_type`: the index the next
/// one of that type gets.
fn count_of_type(earlier: &[(WidgetRecord, CallSite)], widget_type: &str) -> usize {
    earlier
        .iter()
        .filter(|(record, _)| record.widget_type == widget_type)
        .count()
}

/// A slider's id: the score span of its value.
fn slider_widget_id(value: std::ops::Range<usize>) -> String {
    format!("{}:{}", value.start, value.end)
}

/// The record of a visual widget whose ranges both cover `span` in the
/// source. Its index counts the `earlier` records of the same type, and its id
/// spells the type's `:` as `_`.
fn visual_record(
    earlier: &[(WidgetRecord, CallSite)],
    options: &TranspileOptions,
    widget_type: String,
    span: std::ops::Range<usize>,
    painter_options: &str,
) -> WidgetRecord {
    let from = span.start + options.block_offset;
    let to = span.end + options.block_offset;
    let index = count_of_type(earlier, &widget_type);
    let prefix = options.id.as_deref().unwrap_or("");
    let stem = widget_type.replace(':', "_");
    WidgetRecord {
        from,
        to,
        call_from: from,
        call_to: to,
        index,
        id: format!("{prefix}_widget_{stem}_{index}_{from}-{to}"),
        widget_type,
        visual_slot: None,
        value: None,
        min: None,
        max: None,
        step: None,
        options: Some(painter_options.to_owned()),
        deferred_rendering: true,
        frequency: false,
        label: None,
    }
}

/// How `rewrite_widget_calls` splices a recorded call. Positions are source
/// offsets without `block_offset`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CallSite {
    /// `slider(…)`: insert `WithID` at `name_end` and the id after `open`.
    Slider { name_end: usize, open: usize },
    /// `.painter(…)`: insert the id, and the visual slot when the record has
    /// one, after `open`.
    Method { open: usize },
    /// `all(<painter>)`: layout metadata; the call is not rewritten.
    AsWritten,
}

/// The widget calls in `source` with their splice sites, in source order.
///
/// Calls inside a recorded call's arguments or inside a KabelSalat graph are
/// not recorded, so each site follows the closing paren of the previously
/// recorded call. `kabelsalat` is `kabelsalat_calls` of the same source.
fn collect_widgets(
    source: &str,
    options: &TranspileOptions,
    regexes: &RegexLiteralRanges,
    kabelsalat: &[KabelsalatCall],
) -> Vec<(WidgetRecord, CallSite)> {
    let bytes = source.as_bytes();
    let mut widgets: Vec<(WidgetRecord, CallSite)> = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if matches!(bytes[index], b'\'' | b'"' | b'`') {
            index = scan_quoted(bytes, index, bytes[index]);
            continue;
        }
        if bytes[index..].starts_with(b"//") || bytes[index..].starts_with(b"/*") {
            index = scan_comment(source, index);
            continue;
        }
        if let Some(end) = regexes.end_at(index) {
            index = end;
            continue;
        }
        if !(bytes[index].is_ascii_alphabetic() || matches!(bytes[index], b'_' | b'$')) {
            index += 1;
            continue;
        }
        let name_start = index;
        while bytes.get(index).is_some_and(is_identifier_byte) {
            index += 1;
        }
        let name_end = index;
        let name = &source[name_start..name_end];
        let mut open = name_end;
        while bytes.get(open).is_some_and(u8::is_ascii_whitespace) {
            open += 1;
        }
        if bytes.get(open) != Some(&b'(') {
            continue;
        }
        let Some(close) = matching_delimiter(bytes, open, b'(', b')', regexes) else {
            break;
        };
        // Widgets inside a KabelSalat graph are not recorded: the call fails
        // when the score runs, so no control there could drive it. The
        // arguments after the graph are scanned, even when `K` is also a
        // widget method.
        if let Ok(call) = kabelsalat.binary_search_by_key(&name_start, |call| call.name) {
            index = kabelsalat[call].stringified().end;
            continue;
        }
        let member = bytes.get(name_start.wrapping_sub(1)) == Some(&b'.');
        // `all(pianoroll)` or `all(pianoroll({ labels: 1 }))` names a
        // page-level painter other than `markcss`; the painter's own call
        // carries the options.
        let all_painter = if name == "all" && !member {
            let args = split_top_level(&source[open + 1..close], ',', regexes, open + 1);
            let painter = args.first().map(|argument| argument.trim()).unwrap_or("");
            let (painter, painter_options) = match painter.find('(') {
                Some(open_at) if painter.ends_with(')') => (
                    painter[..open_at].trim_end(),
                    painter[open_at + 1..painter.len() - 1].trim(),
                ),
                _ => (painter, ""),
            };
            let recognized = !painter.is_empty()
                && painter != "markcss"
                && painter.bytes().all(|byte| is_identifier_byte(&byte))
                && !painter.starts_with('_')
                && options
                    .widget_methods
                    .iter()
                    .any(|method| method == painter);
            recognized.then_some((painter, painter_options))
        } else {
            None
        };
        let recorded = if let Some((painter, painter_options)) = all_painter {
            let record = visual_record(
                &widgets,
                options,
                format!("{ALL_VISUAL_WIDGET_PREFIX}{painter}"),
                name_start..close + 1,
                painter_options,
            );
            Some((record, CallSite::AsWritten))
        } else if name == "slider" && !member {
            let args_source = &source[open + 1..close];
            let args = split_top_level(args_source, ',', regexes, open + 1);
            let leading = args_source.len() - args_source.trim_start().len();
            let value_from = open + 1 + leading + options.block_offset;
            let value_to = value_from + args.first().map_or(0, |value| value.trim().len());
            let id = slider_widget_id(value_from..value_to);
            // Missing bounds default to the 0..1 range. An explicit bound
            // that is not a finite numeric literal must remain `None`, though:
            // treating `slider(.5, low, high)` as 0..1 would advertise a UI
            // control whose range does not describe the score. `inf` and
            // `NaN` parse as floats but are not finite numeric literals.
            let numeric_bound = |index: usize, default: f64| match args.get(index) {
                Some(value) => value
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|bound| bound.is_finite()),
                None => Some(default),
            };
            let record = WidgetRecord {
                from: value_from,
                to: value_to,
                call_from: name_start + options.block_offset,
                call_to: close + 1 + options.block_offset,
                index: count_of_type(&widgets, "slider"),
                widget_type: "slider".into(),
                id,
                visual_slot: None,
                value: args.first().map(|value| value.trim().to_string()),
                min: numeric_bound(1, 0.0),
                max: numeric_bound(2, 1.0),
                step: args
                    .get(3)
                    .and_then(|value| value.trim().parse::<f64>().ok())
                    .filter(|step| step.is_finite()),
                options: None,
                deferred_rendering: true,
                frequency: false,
                label: None,
            };
            Some((record, CallSite::Slider { name_end, open }))
        } else if member && options.widget_methods.iter().any(|method| method == name) {
            let call_start = find_member_expression_start(bytes, name_start - 1, regexes);
            let record = visual_record(
                &widgets,
                options,
                name.into(),
                call_start..close + 1,
                source[open + 1..close].trim(),
            );
            Some((record, CallSite::Method { open }))
        } else {
            None
        };
        // Skip a recorded call's arguments. Descend into any other call,
        // whose arguments may hold widgets (`stack(a._scope(), b._scope())`).
        index = match recorded {
            Some(widget) => {
                widgets.push(widget);
                close + 1
            }
            None => open + 1,
        };
    }
    widgets
}

/// The unescaped opening quote matching the closing quote at `end`.
fn quote_open_backward(bytes: &[u8], end: usize) -> Option<usize> {
    let quote = *bytes.get(end)?;
    if !matches!(quote, b'\'' | b'"' | b'`') {
        return None;
    }
    let mut i = end;
    while i > 0 {
        i -= 1;
        if bytes[i] == quote {
            let mut backslashes = 0;
            let mut j = i;
            while j > 0 && bytes[j - 1] == b'\\' {
                backslashes += 1;
                j -= 1;
            }
            if backslashes % 2 == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// The opening bracket matching the closing bracket at `end`, skipping
/// quoted strings so brackets inside literals do not confuse the depth.
fn bracket_open_backward(bytes: &[u8], end: usize) -> Option<usize> {
    let close = *bytes.get(end)?;
    let open = match close {
        b')' => b'(',
        b']' => b'[',
        b'}' => b'{',
        _ => return None,
    };
    let mut depth = 0usize;
    let mut i = end;
    loop {
        match bytes.get(i) {
            Some(&c) if c == close => depth += 1,
            Some(&c) if c == open => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(i);
                }
            }
            Some(b'\'' | b'"' | b'`') => {
                let quote_open = quote_open_backward(bytes, i)?;
                i = quote_open;
            }
            None => return None,
            _ => {}
        }
        if i == 0 {
            break;
        }
        i -= 1;
    }
    None
}

/// The `/` that opened a `/* … */` whose closer is at `slash_of_close`.
fn block_comment_open_backward(bytes: &[u8], slash_of_close: usize) -> Option<usize> {
    let mut i = slash_of_close.saturating_sub(1);
    while i > 0 {
        i -= 1;
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
            return Some(i);
        }
    }
    None
}

/// A `//` comment on the line that ends at `newline`, or `None` when that
/// line has no line-comment (quoted `//` is not a comment).
fn line_comment_start_on_line_ending_at(bytes: &[u8], newline: usize) -> Option<usize> {
    let line_start = bytes[..newline]
        .iter()
        .rposition(|&byte| byte == b'\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    let mut i = line_start;
    while i < newline {
        match bytes[i] {
            b'\'' | b'"' | b'`' => {
                i = scan_quoted(bytes, i, bytes[i]);
                if i > newline {
                    return None;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => return Some(i),
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < newline && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                if i + 1 >= newline {
                    return None;
                }
                i += 2;
            }
            _ => i += 1,
        }
    }
    None
}

/// Where the receiver expression of the member call beginning at `dot`
/// starts. The receiver may contain quoted strings with spaces and
/// separators, nested call arguments, comments, regex literals, and
/// `member\n.member` chain continuations; the scan stops at actual
/// enclosing expression boundaries (top-level separators, operators,
/// unmatched openers) rather than at the first whitespace it meets.
fn find_member_expression_start(bytes: &[u8], dot: usize, regexes: &RegexLiteralRanges) -> usize {
    let mut start = dot;
    while start > 0 {
        let byte = bytes[start - 1];
        match byte {
            // A closing quote: the whole literal is part of the receiver,
            // spaces and separators inside it included.
            b'\'' | b'"' | b'`' => match quote_open_backward(bytes, start - 1) {
                Some(open) if open < start => start = open,
                _ => break,
            },
            // A closed bracket group is part of the receiver, whatever its
            // contents are.
            b')' | b']' | b'}' => match bracket_open_backward(bytes, start - 1) {
                Some(open) if open < start => start = open,
                _ => break,
            },
            b'/' => {
                if let Some(open) = regexes.start_at_end(start).filter(|open| *open < start) {
                    start = open;
                } else if start >= 2 && bytes[start - 2] == b'*' {
                    match block_comment_open_backward(bytes, start - 1) {
                        Some(open) if open < start => start = open,
                        _ => break,
                    }
                } else {
                    break;
                }
            }
            // Unmatched openers and top-level separators/operators end the
            // receiver: what precedes them is a sibling expression.
            b'(' | b'[' | b'{' | b',' | b';' | b'=' | b'+' | b'-' | b'*' | b'%' | b'<' | b'>'
            | b'&' | b'|' | b'?' | b':' | b'!' => break,
            b' ' | b'\t' | b'\r' => break,
            b'\n' => {
                // A line break inside the receiver is only the legal
                // `member\n.member` continuation when the next significant
                // character, towards the dot, is the continuation dot.
                if next_significant(bytes, start) == Some(b'.') {
                    start -= 1;
                    if let Some(comment) = line_comment_start_on_line_ending_at(bytes, start) {
                        start = comment;
                    }
                } else {
                    break;
                }
            }
            _ => start -= 1,
        }
    }
    start
}

/// `rewritten`, the output of `rewrite_mini_literals`, with each recorded
/// widget call spliced at its site, which `map` carries into it.
///
/// Each id lands on the call its record was made for, and unrecorded calls
/// are left alone. Every splice is an insertion, so the score's text and line
/// breaks survive. The sites arrive in source order.
fn rewrite_widget_calls(
    rewritten: &str,
    map: &RewriteMap,
    records: &[WidgetRecord],
    sites: &[CallSite],
) -> String {
    debug_assert_eq!(records.len(), sites.len());
    let mut output = String::with_capacity(rewritten.len() + records.len() * 24);
    let mut cursor = 0usize;
    let mut insert = |at: usize, text: &str| {
        debug_assert!(cursor <= at, "widget sites out of source order");
        output.push_str(&rewritten[cursor..at]);
        output.push_str(text);
        cursor = at;
    };
    for (record, site) in records.iter().zip(sites) {
        let open = match *site {
            CallSite::AsWritten => continue,
            CallSite::Slider { name_end, open } => {
                insert(map.to_rewritten(name_end), "WithID");
                open
            }
            CallSite::Method { open } => open,
        };
        let at = map.to_rewritten(open + 1);
        let mut arguments = quote_single(&record.id);
        if let Some(slot) = record.visual_slot {
            arguments.push_str(", ");
            arguments.push_str(&slot.to_string());
        }
        if rewritten.as_bytes().get(at) != Some(&b')') {
            arguments.push_str(", ");
        }
        insert(at, &arguments);
    }
    output.push_str(&rewritten[cursor..]);
    output
}

/// The identifier directly before `at` (a template-literal tag candidate),
/// with its `\u` escapes decoded, reading no further back than `floor` - the
/// caller passes the end of the last comment so a trailing `// word` is never
/// mistaken for a tag.
fn preceding_identifier_bounded(source: &str, at: usize, floor: usize) -> Option<String> {
    let before = source[..at].trim_end();
    let mut end = before.len();
    let mut reversed = Vec::new();
    while end > floor {
        let Some((start, ch)) = identifier_char_before(before, end) else {
            break;
        };
        if start < floor || !oxc_syntax::identifier::is_identifier_part(ch) {
            break;
        }
        reversed.push(ch);
        end = start;
    }
    (!reversed.is_empty()).then(|| reversed.into_iter().rev().collect())
}

/// The character that ends at byte `end` of `source` and its start, reading a
/// `\uXXXX` or `\u{X…}` identifier escape as the character it names; `None`
/// when such an escape names no character.
fn identifier_char_before(source: &str, end: usize) -> Option<(usize, char)> {
    let bytes = source.as_bytes();
    if end >= 6
        && &bytes[end - 6..end - 4] == b"\\u"
        && let Some(value) = take_hex(&mut source[end - 4..end].char_indices().peekable(), 4)
    {
        return char::from_u32(value).map(|ch| (end - 6, ch));
    }
    if bytes.get(end.checked_sub(1)?) == Some(&b'}') {
        for digits_len in 1..=6 {
            let Some(start) = end.checked_sub(digits_len + 4) else {
                break;
            };
            if &bytes[start..start + 3] == b"\\u{"
                && let Some(value) = take_hex(
                    &mut source[start + 3..end - 1].char_indices().peekable(),
                    digits_len,
                )
            {
                return char::from_u32(value).map(|ch| (start, ch));
            }
        }
    }
    source[..end].char_indices().next_back()
}

/// Whether the template body `raw` opens a `${` substitution; a backslash
/// escapes the byte after it, so `\${` opens none and `\\${` opens one.
fn has_unescaped_template_interpolation(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            index += 2;
        } else if bytes[index..].starts_with(b"${") {
            return true;
        } else {
            index += 1;
        }
    }
    false
}

/// Index just past the literal that `quote` opens at `start`, or `bytes.len()`
/// when it never closes. A template's `${...}` substitutions are code, skipped
/// by [`skip_interpolation`].
fn scan_quoted(bytes: &[u8], start: usize, quote: u8) -> usize {
    scan_quoted_with_status(bytes, start, quote).0
}

fn scan_quoted_with_status(bytes: &[u8], start: usize, quote: u8) -> (usize, bool) {
    let mut index = start + 1;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            index = (index + 2).min(bytes.len());
        } else if bytes[index] == quote {
            return (index + 1, true);
        } else if quote == b'`' && bytes[index..].starts_with(b"${") {
            // A substitution is code, and a backtick inside it opens a
            // nested template rather than ending this one.
            index = skip_interpolation(bytes, index + 2);
        } else {
            index += 1;
        }
    }
    (bytes.len(), false)
}

/// Index just past the `}` closing the template substitution whose code starts
/// at `start`, or `bytes.len()` when it never closes. Braces, strings, comments
/// and nested templates in the code are skipped whole; regex literals are not
/// recognized.
fn skip_interpolation(bytes: &[u8], start: usize) -> usize {
    let mut depth = 1usize;
    let mut index = start;
    while index < bytes.len() {
        if matches!(bytes[index], b'\'' | b'"' | b'`') {
            index = scan_quoted(bytes, index, bytes[index]);
            continue;
        }
        if let Some(after) = comment_end_at(bytes, index) {
            index = after;
            continue;
        }
        if bytes[index] == b'{' {
            depth += 1;
        } else if bytes[index] == b'}' {
            depth -= 1;
            if depth == 0 {
                return index + 1;
            }
        }
        index += 1;
    }
    bytes.len()
}

/// Read `count` hex digits, or nothing if any of them is not hex.
fn take_hex(
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

/// Decode JavaScript string escapes for a mini-notation literal.
#[derive(Debug, PartialEq, Eq)]
struct JsStringDecodeError {
    message: String,
    offset: usize,
}

fn decode_js_string(raw: &str, allow_line_breaks: bool) -> Result<String, JsStringDecodeError> {
    let mut output = String::new();
    let mut chars = raw.char_indices().peekable();
    while let Some((offset, ch)) = chars.next() {
        if ch != '\\' {
            if matches!(ch, '\n' | '\r') {
                // A raw line break is a syntax error inside '' or "", but a
                // template literal keeps it - and real scores rely on that to
                // lay a long phrase out over several lines.
                if !allow_line_breaks {
                    return Err(JsStringDecodeError {
                        message: "unescaped line break in mini string".into(),
                        offset,
                    });
                }
                // JavaScript normalises CRLF and lone CR to LF in template
                // literal values; mini treats any of them as whitespace, but
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
                return Err(JsStringDecodeError {
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
                let value = take_hex(&mut chars, 2).ok_or_else(|| JsStringDecodeError {
                    message: "invalid hexadecimal escape in mini string".into(),
                    offset,
                })?;
                output.push(char::from_u32(value).expect("two hex digits form a scalar value"));
            }
            Some('u') => decode_unicode_escape(&mut chars, &mut output)
                .map_err(|message| JsStringDecodeError { message, offset })?,
            Some(ch) => output.push(ch),
            None => {
                return Err(JsStringDecodeError {
                    message: "unterminated escape in mini string".into(),
                    offset,
                });
            }
        }
    }
    Ok(output)
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

fn quote_single(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t");
    format!("'{escaped}'")
}

/// Start offsets of the calls upstream awaits: each call whose callee is the
/// identifier `samples`, unless it is already the operand of an `await`.
#[derive(Default)]
struct SamplesFetchCalls {
    starts: Vec<usize>,
}

impl<'a> Visit<'a> for SamplesFetchCalls {
    fn visit_await_expression(&mut self, expression: &AwaitExpression<'a>) {
        match expression.argument.without_parentheses() {
            Expression::CallExpression(call) if call.callee.is_specific_id("samples") => {
                walk::walk_call_expression(self, call);
            }
            _ => walk::walk_await_expression(self, expression),
        }
    }

    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        if call.callee.is_specific_id("samples") {
            self.starts.push(call.span.start as usize);
        }
        walk::walk_call_expression(self, call);
    }
}

/// `source` with `await ` spliced before each [`SamplesFetchCalls`] call.
/// A member call, a constructor, or a declaration named `samples` is not a
/// fetch call and is left as written.
fn rewrite_bare_samples(source: &str) -> String {
    if !source.contains("samples") {
        return source.to_owned();
    }
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::mjs()).parse();
    let mut calls = SamplesFetchCalls::default();
    calls.visit_program(&parsed.program);
    calls.starts.sort_unstable();
    let mut output = String::with_capacity(source.len() + calls.starts.len() * "await ".len());
    let mut copied = 0;
    for start in calls.starts {
        output.push_str(&source[copied..start]);
        output.push_str("await ");
        copied = start;
    }
    output.push_str(&source[copied..]);
    output
}

/// Whether an expression cannot have ended here: its last significant
/// character is a member-access `.`, a binary operator, or a comma, all of
/// which need something after them. A `.` that ends a number (`1.`) and a
/// postfix `++`/`--` do end an expression, and are left to the semicolon
/// rules.
fn ends_mid_expression(chain: &str) -> bool {
    let mut tail = chain.trim_end().chars().rev();
    let Some(last) = tail.next() else {
        return false;
    };
    let before = tail.next();
    match last {
        '.' => !before.is_some_and(|c| c.is_ascii_digit() || c == '.'),
        '+' | '-' => before != Some(last),
        ',' | '*' | '/' | '%' | '&' | '|' | '^' | '?' | ':' | '=' | '<' | '>' => true,
        _ => false,
    }
}

/// Rewrite labeled statements (`kick: s("bd")`, `$: s("bd*4")`) to `.p('kick')`.
///
/// Every labeled statement is rewritten even when non-label statements appear
/// before it, as in `setcpm(140/4)` followed by `$: s("bd*4")`.
fn rewrite_labels(source: &str, regexes_are_present: bool) -> String {
    let bytes = source.as_bytes();
    let regexes = RegexLiteralRanges::parse_if_present(source, regexes_are_present);
    let mut output = String::with_capacity(source.len() + 16);
    let mut cursor = 0usize;
    let mut rewrote = false;
    let mut index = 0usize;

    while index < bytes.len() {
        while index < bytes.len() {
            let width = if bytes[index].is_ascii_whitespace() {
                1
            } else {
                line_terminator_width_at(bytes, index)
            };
            if width == 0 {
                break;
            }
            index += width;
        }
        if index >= bytes.len() {
            break;
        }
        let statement_start = index;

        // A label is `IDENT:` at the start of a top-level statement, with any
        // whitespace or comments before the colon (`kick : s("bd")`). Anything
        // else is copied through and skipped, rather than ending the scan.
        let mut name_end = statement_start;
        while bytes.get(name_end).is_some_and(is_identifier_byte) {
            name_end += 1;
        }
        let label_colon = (name_end > statement_start)
            .then(|| skip_trivia(bytes, name_end))
            // `::` is not a label, and `a ? b : c` never reaches here because
            // the `?` breaks the identifier run.
            .filter(|&at| bytes.get(at) == Some(&b':') && bytes.get(at + 1) != Some(&b':'));

        // The labeled body may start on a following line (`voice:` alone,
        // chain indented below) and is still one labeled statement.
        let body_start = label_colon.map_or(statement_start, |at| skip_trivia(bytes, at + 1));
        let mut end = body_start;
        let mut depth = 0i32;
        while end < bytes.len() {
            if matches!(bytes[end], b'\'' | b'"' | b'`') {
                end = scan_quoted(bytes, end, bytes[end]);
                continue;
            }
            if let Some(after) = comment_end_at(bytes, end) {
                end = after;
                continue;
            }
            if let Some(regex_end) = regexes.end_at(end) {
                end = regex_end;
                continue;
            }
            let terminator_width = line_terminator_width_at(bytes, end);
            if depth == 0 && terminator_width != 0 {
                // A line that ends mid-expression - on the `.` of a method
                // chain, an operator, a comma - is not the end of the
                // statement: JavaScript itself reads on, and so must the
                // label, or `$: s("x").decay(.08).` with `gain(.4)` below
                // it registers a lane that ends in a dot.
                let (chain, _) =
                    split_trailing_trivia(&source[body_start..end], regexes_are_present);
                if ends_mid_expression(chain) {
                    end += terminator_width;
                    continue;
                }
                // A labeled statement continues across line terminators while
                // the next significant token is a method-chain `.` (blank
                // lines and comments included). Otherwise a multi-line
                // `$bass:` lane would register only its first line and discard
                // the chained controls below it.
                let probe = skip_trivia(bytes, end + terminator_width);
                if bytes.get(probe) == Some(&b'.')
                    && bytes
                        .get(probe + 1)
                        .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_' || *c == b'$')
                {
                    end = probe;
                    continue;
                }
                // JS never inserts a semicolon before `(` or `[`: a line
                // ending mid-chain (`.layer` alone) then `(args…)` on the
                // next line is ONE statement (sarahonaworm's lane).
                if matches!(bytes.get(probe), Some(b'(') | Some(b'[')) {
                    end = probe;
                    continue;
                }
                break;
            }
            match bytes[end] {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                b';' if depth == 0 => break,
                _ => {}
            }
            end += 1;
        }

        if let Some(colon_at) = label_colon {
            output.push_str(&source[cursor..statement_start]);
            let name = &source[statement_start..name_end];
            // The gaps the scanner swallowed keep their line breaks: those
            // around the colon go out before the chain, the one after it goes
            // out after the statement, so an error reported by line lands on
            // the score's own line.
            output.push_str(&source[name_end..colon_at]);
            output.push_str(&source[colon_at + 1..body_start]);
            let body_raw = &source[body_start..end];
            let body = body_raw.trim_end();
            // A trailing line comment is part of the scanned span; `.p()`
            // appended after it would be commented out and the lane lost.
            let (chain, trivia) = split_trailing_trivia(body, regexes_are_present);
            output.push_str(chain.trim_end());
            output.push_str(".p(");
            output.push_str(&quote_single(name));
            output.push(')');
            if !trivia.is_empty() {
                output.push(' ');
                output.push_str(trivia);
            }
            if bytes.get(end) == Some(&b';') {
                output.push(';');
                end += 1;
            }
            output.push_str(&body_raw[body.len()..]);
            cursor = end;
            rewrote = true;
        } else if bytes.get(end) == Some(&b';') {
            end += 1;
        }
        index = if end > statement_start {
            end
        } else {
            statement_start + 1
        };
    }

    if !rewrote {
        source.to_string()
    } else {
        output.push_str(&source[cursor..]);
        output
    }
}

/// Append `return` to the final top-level expression statement. Public so a
/// host that evaluated script-mode (where the completion value needs no
/// `return`) can rebuild the async-wrapper form when a score turns out to
/// contain top-level `await`.
pub fn add_final_return(code: &str) -> String {
    add_final_return_on_parse_stack(code, false)
}

fn add_final_return_on_parse_stack(code: &str, empty_is_undefined: bool) -> String {
    // Callers pass transpiled output, which is already bounded, but this is
    // public: rather than parse source no one checked, fall back to the same
    // unchanged-code path an unparseable input takes.
    if check_nesting(code).is_err() {
        return code.to_string();
    }
    on_parse_stack(|| add_final_return_here(code, empty_is_undefined))
        .unwrap_or_else(|| code.to_string())
}

fn add_final_return_here(code: &str, empty_is_undefined: bool) -> String {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, code, SourceType::mjs()).parse();
    if parsed.panicked || !parsed.diagnostics.is_empty() {
        return code.to_string();
    }
    if empty_is_undefined && parsed.program.body.is_empty() {
        return code.to_string();
    }

    let Some(last) = parsed.program.body.last() else {
        if parsed.program.directives.is_empty() {
            return format!("return silence;{code}");
        }
        return format!("{code}\nreturn silence;");
    };
    let Statement::ExpressionStatement(statement) = last else {
        return format!("{code}\nreturn silence;");
    };

    let start = statement.expression.span().start as usize;
    let mut output = String::with_capacity(code.len() + "return ".len());
    output.push_str(&code[..start]);
    output.push_str("return ");
    output.push_str(&code[start..]);
    output
}

/// `add_final_return` for the synchronous score wrapper.
///
/// It differs in one case: a program with no statements (empty, or only
/// comments) stays unchanged, so the wrapper returns `undefined` and not
/// `silence`.
///
/// The runtime uses `undefined` to tell "this score named no pattern" from
/// "this score produced a value that is not a pattern". `rustel play` can
/// then refuse a score that is fully commented out, and does not open an
/// audio device for silence. In every other case this follows upstream,
/// which answers a final non-expression statement with `silence`: a score
/// that ends on a declaration names no pattern, and strudel.cc plays
/// nothing for it.
pub fn add_final_return_or_undefined(code: &str) -> String {
    add_final_return_on_parse_stack(code, true)
}

/// Index just past a comment starting at `index`, or `None` if none starts
/// there.
///
/// Distinct from `scan_comment`, which assumes the caller has already
/// established that a comment begins at `index`; the scanners below need to
/// ASK, because they walk arbitrary code.
fn comment_end_at(bytes: &[u8], index: usize) -> Option<usize> {
    if bytes.get(index) != Some(&b'/') {
        return None;
    }
    match bytes.get(index + 1) {
        Some(b'/') => Some(line_terminator_bytes(bytes, index + 2)),
        Some(b'*') => {
            let mut i = index + 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            Some((i + 2).min(bytes.len()))
        }
        _ => None,
    }
}

/// Split `code` into (statements, trailing trivia), where trivia is the run of
/// comments and whitespace after the last code-bearing byte.
///
fn split_trailing_trivia(code: &str, regexes_are_present: bool) -> (&str, &str) {
    let bytes = code.as_bytes();
    let regexes = RegexLiteralRanges::parse_if_present(code, regexes_are_present);
    let mut index = 0usize;
    let mut end_of_code = 0usize;
    while index < bytes.len() {
        if matches!(bytes[index], b'\'' | b'"' | b'`') {
            index = scan_quoted(bytes, index, bytes[index]);
            end_of_code = index;
            continue;
        }
        if let Some(after) = comment_end_at(bytes, index) {
            index = after;
            continue;
        }
        if let Some(end) = regexes.end_at(index) {
            index = end;
            end_of_code = index;
            continue;
        }
        if !bytes[index].is_ascii_whitespace() {
            end_of_code = index + 1;
        }
        index += 1;
    }
    code.split_at(end_of_code)
}

pub fn validate_es2022(source: &str) -> Result<(), Vec<ParseDiagnostic>> {
    let result = parse_and_print(source);
    if result.diagnostics.is_empty() && !result.panicked {
        Ok(())
    } else {
        Err(result.diagnostics)
    }
}

pub fn validate_path(source: &str, path: impl AsRef<Path>) -> Result<(), Vec<ParseDiagnostic>> {
    let result = parse_and_print_path(source, path);
    if result.diagnostics.is_empty() && !result.panicked {
        Ok(())
    } else {
        Err(result.diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_definitions_read_what_a_score_could_call() {
        let definitions = setup_definitions(
            "globalThis.a = 1\n\
         window.b = () => 2\n\
         Pattern.prototype.c = function () {}\n\
         globalThis['d'] = 3\n\
         register('e', (pat) => pat)\n\
         rustelScope.register(['f', 'g'], (pat) => pat)\n\
         queueMicrotask(() => { globalThis.h = 1 })\n\
         Object.assign(window, { i: 1, 'j': 2 })\n\
         Object.defineProperty(globalThis, 'k', { value: 1 })\n",
        );
        let mut names = definitions.names.clone();
        names.sort();
        assert_eq!(
            names,
            ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k"],
            "every name a later score could call"
        );
        assert!(!definitions.has_dynamic_names);
        assert!(definitions.top_level_declarations.is_empty());
    }

    #[test]
    fn setup_definitions_flag_the_names_they_cannot_read() {
        for source in [
            "globalThis[name] = 1",
            "Object.assign(globalThis, helpers)",
            "Object.assign(window, { ...more })",
            "Object.assign(globalThis, { [key]: 1 })",
            "Object.defineProperty(window, key, {})",
            "register(dynamicName, (pat) => pat)",
        ] {
            assert!(
                setup_definitions(source).has_dynamic_names,
                "a name this cannot read was not flagged: {source}"
            );
        }
        // Something else's namespace is not the shared one.
        for source in [
            "plugin.assign(globalThis, {})",
            "other.prototype.x = 1",
            "local[key] = 1",
        ] {
            let definitions = setup_definitions(source);
            assert!(
                !definitions.has_dynamic_names && definitions.names.is_empty(),
                "an unrelated assignment was read as setup: {source}"
            );
        }
    }

    #[test]
    fn setup_definitions_list_top_level_declarations_with_their_spans() {
        let source = "const a = 1, b = 2\nlet c\nvar d\nfunction e() {}\nclass F {}\nif (a) { const inner = 1 }\nconst [x] = [1]\n";
        let definitions = setup_definitions(source);
        let listed = definitions
            .top_level_declarations
            .iter()
            .map(|declaration| (declaration.name.as_str(), declaration.kind))
            .collect::<Vec<_>>();
        assert_eq!(
            listed,
            [
                ("a", "const"),
                ("b", "const"),
                ("c", "let"),
                ("d", "var"),
                ("e", "function"),
                ("F", "class"),
            ],
            "only the program's own direct children, and only named ones"
        );
        for declaration in &definitions.top_level_declarations {
            assert_eq!(
                &source[declaration.from..declaration.to],
                declaration.name,
                "a declaration's span does not cover its name"
            );
        }
    }

    #[test]
    fn setup_definitions_survive_text_that_does_not_parse() {
        assert_eq!(
            setup_definitions("globalThis.a = ("),
            SetupDefinitions::default()
        );
        let deep = "(".repeat(MAX_STRUCTURAL_BYTES + 1);
        assert_eq!(setup_definitions(&deep), SetupDefinitions::default());
        assert_eq!(setup_definitions(""), SetupDefinitions::default());
    }

    /// Device names remain literal strings in either quote style. Treating a
    /// double-quoted port name as mini-notation splits names that contain spaces.
    #[test]
    fn a_name_argument_is_left_alone_in_either_quote() {
        let named = [
            r#"$: note("c4").midi("Pilote IAC Bus 1")"#,
            r#"const kb = await midikeys("Arturia KeyStep 32")"#,
            r#"const cc = await midin("Bass Station II")"#,
            r#"$: s("bd").midimap("my map")"#,
            r#"await samples("github:tidalcycles/dirt-samples")"#,
            r#"await samples({ bd: ["bd.wav"] }, "https://example.test/audio/")"#,
            r#"$: s("bd").serial(38400, 0, 0, "/dev/tty.usbmodem 1")"#,
        ];
        for source in named {
            let output = transpile(
                source,
                &TranspileOptions {
                    add_return: false,
                    ..TranspileOptions::default()
                },
            );
            assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
            // The rhythm is still mini-notation; the name beside it is not.
            assert!(
                !output.output.contains("m('Pilote IAC Bus 1'")
                    && !output.output.contains("m('Arturia KeyStep 32'")
                    && !output.output.contains("m('Bass Station II'")
                    && !output.output.contains("m('my map'")
                    && !output
                        .output
                        .contains("m('github:tidalcycles/dirt-samples'")
                    && !output.output.contains("m('https://example.test/audio/'")
                    && !output.output.contains("m('/dev/tty.usbmodem 1'"),
                "a name was parsed as a rhythm: {}",
                output.output
            );
        }

        // And the change is narrow: a string anywhere else is still a
        // pattern, including one that happens to sit in another argument
        // of the same call.
        let output = transpile(
            r#"$: note("c4 e4").midi('IAC', { velocity: "0.5 1" })"#,
            &TranspileOptions {
                add_return: false,
                ..TranspileOptions::default()
            },
        );
        assert!(output.output.contains("m('c4 e4'"), "{}", output.output);
        assert!(output.output.contains("m('0.5 1'"), "{}", output.output);
    }

    #[test]
    fn registration_hints_follow_public_register_names_and_completion() {
        let output = transpile(
            r#"
              register('in\u0073pire', (pat) => pat)
              rustelScope.register(['bloom', 'shine'], (pat) => pat)
              globalThis.register(dynamicName, (pat) => pat)
              $: pure(1).inspire()
            "#,
            &TranspileOptions {
                add_return: false,
                ..TranspileOptions::default()
            },
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(output.registrations.names, ["inspire", "bloom", "shine"]);
        assert!(output.registrations.has_dynamic_names);
        assert!(!output.registrations.final_expression_is_registration);

        let final_registration =
            registration_hints("setCpm(120 / 4)\nregister(['left', 'right'], (pat) => pat)");
        assert_eq!(final_registration.names, ["left", "right"]);
        assert!(final_registration.final_expression_is_registration);

        let unrelated = registration_hints("plugin.register('notThePatternRegistry', callback)");
        assert!(unrelated.names.is_empty());
        assert!(!unrelated.has_dynamic_names);
        assert!(!unrelated.final_expression_is_registration);
    }

    /// Deep nesting used to abort the process on a stack overflow rather than
    /// return a diagnostic, which during a live set stops the music.
    #[test]
    fn nesting_past_the_limit_is_refused_rather_than_overflowing_the_stack() {
        // Deep, but inside the size limit, so this is the nesting refusal and
        // not the size one.
        let deep = format!("const x = {}1{};", "(".repeat(20_000), ")".repeat(20_000));
        assert!(deep.len() <= MAX_STRUCTURAL_BYTES);
        let error = check_nesting(&deep).expect_err("deep brackets were accepted");
        assert!(error.message.contains("20000 levels deep"), "{error:?}");

        let chained = format!("note(\"c\"){}", ".fast(1)".repeat(8_000));
        assert!(chained.len() <= MAX_STRUCTURAL_BYTES);
        let error = check_nesting(&chained).expect_err("a long chain was accepted");
        assert!(error.message.contains("8000 calls"), "{error:?}");

        // The refusal has to happen through the ordinary entry points, since
        // those are what a score reaches.
        assert!(
            !transpile(&deep, &TranspileOptions::default())
                .diagnostics
                .is_empty()
        );
        assert!(!parse_and_print(&chained).diagnostics.is_empty());
    }

    #[test]
    fn operator_chains_are_refused_before_they_reach_the_parser() {
        let unary = format!("{}x", "!".repeat(600));
        let multiline_unary = format!("{}x", "!\n".repeat(600));
        let ternary = format!("{}0", "1?0:".repeat(600));
        let arrows = format!("{}x", "x=>".repeat(600));
        let word_operators = format!("{}x", "void ".repeat(600));
        let multiline_words = format!("{}x", "void\n".repeat(600));
        let leading_binary = format!("x{}", "\n/* more */ + y".repeat(600));
        for source in [
            &unary,
            &multiline_unary,
            &ternary,
            &arrows,
            &word_operators,
            &multiline_words,
            &leading_binary,
        ] {
            let error = check_nesting(source).expect_err("operator chain was accepted");
            assert!(error.message.contains("operators"), "{error:?}");
            assert!(!parse_and_print(source).diagnostics.is_empty());
            assert!(
                !transpile(source, &TranspileOptions::default())
                    .diagnostics
                    .is_empty()
            );
        }

        // Many separate expressions remain valid; the diagnostic measures one
        // chain rather than summing every operator in a file.
        assert!(check_nesting(&"x = 1;\n".repeat(600)).is_ok());
        assert!(check_nesting(&"void x;\n".repeat(600)).is_ok());
        assert!(check_nesting(&"unavoidable;\n".repeat(600)).is_ok());
        assert!(check_nesting(&format!("{}x", "!".repeat(MAX_SOURCE_NESTING))).is_ok());
    }

    #[test]
    fn raw_operator_budget_catches_chains_the_lexical_scan_misses() {
        let too_many_bangs = "!".repeat(MAX_STRUCTURAL_BYTES + 1);
        let error = check_nesting(&too_many_bangs).expect_err("raw operator budget was bypassed");
        assert!(error.message.contains("brackets, dots, and operators"));

        // Word operators spend no punctuation, but also recurse in the parser.
        let too_many_voids = "void ".repeat(MAX_STRUCTURAL_BYTES + 1);
        assert!(check_nesting(&too_many_voids).is_err());

        // So does a keyword spelled with an escape.
        let too_many_escaped = "\\u0076oid ".repeat(MAX_STRUCTURAL_BYTES + 1);
        let error =
            check_nesting(&too_many_escaped).expect_err("escaped keywords were not counted");
        assert!(error.message.contains("brackets, dots, and operators"));
    }

    /// The printer wraps a nested `if` without `else` in an indented block, so its
    /// output grows with the square of the depth. Braceless nesting spends no
    /// bracket, so only the parsed tree shows it.
    #[test]
    fn braceless_nesting_past_the_limit_is_refused_before_printing() {
        let refused = |source: &str| {
            transpile(source, &TranspileOptions::default())
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("nests statements"))
        };
        // `x` sits one level inside the `if`s around it.
        let at_limit = format!("{}x", "if(1)".repeat(MAX_SOURCE_NESTING - 1));
        assert!(
            !refused(&at_limit),
            "{MAX_SOURCE_NESTING} levels were refused"
        );
        let past_limit = format!("{}x", "if(1)".repeat(MAX_SOURCE_NESTING));
        assert!(
            refused(&past_limit),
            "{} levels were accepted",
            MAX_SOURCE_NESTING + 1
        );
        assert!(!parse_and_print(&past_limit).diagnostics.is_empty());

        // The deepest the size limit admits is refused without being printed.
        let deepest = format!("{}x", "if(1)".repeat(65_530));
        let started = std::time::Instant::now();
        let output = transpile(&deepest, &TranspileOptions::default());
        let elapsed = started.elapsed();
        assert!(
            output.output.is_empty(),
            "{} bytes printed",
            output.output.len()
        );
        assert_eq!(output.diagnostics.len(), 1, "{:?}", output.diagnostics);
        assert!(
            output.diagnostics[0]
                .message
                .contains("nests statements or literals 65531 levels deep"),
            "{:?}",
            output.diagnostics
        );
        assert!(elapsed.as_secs_f64() < 1.0, "refusal took {elapsed:?}");
    }

    /// The bracket scan reads the `/` after `}` as a regular expression and
    /// skips the rest of the line, so only the parsed tree shows this nesting.
    #[test]
    fn literal_nesting_hidden_from_the_bracket_scan_is_refused_before_printing() {
        let nested = |open: &str, close: &str, depth: usize| {
            format!(
                "let x = {{}} / {}1{}",
                open.repeat(depth),
                close.repeat(depth)
            )
        };
        for (open, close) in [("[1,1,", "]"), ("{a:1,b:", "}")] {
            let shallow = transpile(&nested(open, close, 8), &TranspileOptions::default());
            assert!(shallow.diagnostics.is_empty(), "{:?}", shallow.diagnostics);
            let deep = transpile(
                &nested(open, close, MAX_SOURCE_NESTING + 1),
                &TranspileOptions::default(),
            );
            assert!(
                deep.output.is_empty(),
                "{} bytes printed",
                deep.output.len()
            );
            assert!(
                deep.diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.message.contains("nests statements or literals")),
                "{:?}",
                deep.diagnostics
            );
        }
    }

    #[test]
    fn ordinary_if_else_chains_still_transpile() {
        let nested = "function pick(a) {\n\
                  \x20 if (a > 3) {\n\
                  \x20   if (a > 5) return 'high'\n\
                  \x20   else if (a > 4) return 'upper'\n\
                  \x20   else return 'mid'\n\
                  \x20 } else if (a > 1) {\n\
                  \x20   for (let i = 0; i < a; i++) if (i % 2) a--\n\
                  \x20   return 'low'\n\
                  \x20 } else {\n\
                  \x20   return 'none'\n\
                  \x20 }\n\
                  }\n\
                  note(pick(2))";
        let output = transpile(nested, &TranspileOptions::default());
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert!(
            output.output.contains("else if (a > 4) return 'upper'"),
            "{}",
            output.output
        );

        // An `else if` ladder prints flat, so its length is not depth.
        let ladder = format!(
            "let n\nif (a == 0) n = 0\n{}",
            (1..2_000)
                .map(|rung| format!("else if (a == {rung}) n = {rung}\n"))
                .collect::<String>()
        );
        let output = transpile(&ladder, &TranspileOptions::default());
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert!(output.output.contains("else if (a == 1999) n = 1999"));
    }

    #[test]
    fn binary_word_operators_spend_the_expression_and_raw_budgets() {
        for source in [
            "item in first in second",
            "item instanceof First instanceof Second",
            "item\n/* continuation */ in first\n in second",
            "item\n instanceof First\n instanceof Second",
        ] {
            assert_eq!(measure_nesting(source).operators, 2, "{source}");
            assert!(structural_bytes(source) >= 2, "{source}");
        }
        assert_eq!(measure_nesting("instanceofName; inside").operators, 0);
    }

    /// Half-typed source reaches this on every keystroke under `--watch`, and
    /// an unterminated string or regex runs the scan to the end of the input.
    /// Reading one byte past that end panicked, which is the same crash this
    /// guard exists to prevent.
    #[test]
    fn unterminated_source_does_not_run_off_the_end() {
        for source in [
            "note(\"c",
            "note('c",
            "note(`c",
            "x = /abc",
            "x = /[abc",
            "/* unclosed",
            "note(\"c\\",
            "x = `${",
        ] {
            // Only that it returns rather than panicking; either answer is fine.
            let _ = check_nesting(source);
        }
    }

    #[test]
    fn unterminated_mini_string_ending_in_utf8_returns_a_diagnostic() {
        for source in ["s(\"é", "s(\"🎵", "s(\"é\\\"", "s(`é"] {
            let result = transpile(source, &TranspileOptions::default());
            assert!(
                !result.diagnostics.is_empty(),
                "an unfinished string was accepted: {source}"
            );
        }
    }

    /// A regular expression is neither a comment nor a string, and reading it
    /// as one hides the code that follows: `/[//]/` looks like a line comment,
    /// `/[/*]/` like a block comment that never closes, and `/[']/` like the
    /// start of a string. Each hid a bracket bomb from an earlier scan.
    #[test]
    fn a_regex_does_not_hide_the_code_after_it() {
        let bomb = format!("{}1{}", "(".repeat(20_000), ")".repeat(20_000));
        for prefix in ["const re = /[//]/;", "const re = /[/*]/;", "x = /[']/;"] {
            let source = format!("{prefix}const x = {bomb}");
            let error =
                check_nesting(&source).expect_err("a regex hid the nesting after it: {prefix}");
            assert!(
                error.message.contains("20000 levels deep"),
                "{prefix}: {error:?}"
            );
        }
    }

    /// Division is not a regex. Counting it as one would skip to the next `/`
    /// and hide whatever lies between.
    #[test]
    fn division_is_not_read_as_a_regex() {
        let bomb = format!("{}1{}", "(".repeat(20_000), ")".repeat(20_000));
        let source = format!("const half = total / 2;\nconst x = {bomb}");
        let error = check_nesting(&source).expect_err("division hid the nesting after it");
        assert!(error.message.contains("20000 levels deep"), "{error:?}");
    }

    #[test]
    fn every_javascript_line_terminator_ends_nesting_comments() {
        let bomb = format!("{}1{}", "(".repeat(20_000), ")".repeat(20_000));
        for separator in ["\r", "\n", "\u{2028}", "\u{2029}"] {
            let source = format!("// comment{separator}const x = {bomb}");
            let error = check_nesting(&source).expect_err("a line comment hid live source");
            assert!(
                error.message.contains("20000 levels deep"),
                "line terminator {separator:?}: {error:?}"
            );

            let trivia = format!("// comment{separator}.fast(2)");
            assert_eq!(
                next_significant(trivia.as_bytes(), 0),
                Some(b'.'),
                "next_significant did not cross {separator:?}"
            );
        }
    }

    /// A chain continues across a line even when a comment sits between the
    /// links, or when the dot ends the line before rather than starting the
    /// one after. Both broke continuation detection and hid a long chain.
    #[test]
    fn a_chain_continues_across_comments_and_trailing_dots() {
        for chain in [
            format!("note(\"c\"){}", "\n//x\n.fast(1)".repeat(4_000)),
            format!("note(\"c\"){}", "\n/*x*/\n.fast(1)".repeat(4_000)),
            format!("note(\"c\").{}fast(1)", "\nfast(1).".repeat(4_000)),
        ] {
            let error = check_nesting(&chain).expect_err("a broken-up chain was accepted");
            assert!(
                error.message.contains("calls in one expression"),
                "{error:?}"
            );
        }
    }

    /// A long run of blank and comment lines is scanned one time. A scan for
    /// each line makes this source take minutes.
    #[test]
    fn a_long_run_of_blank_and_comment_lines_keeps_the_chain_and_stays_linear() {
        let run = "\n// muted lane\n".repeat(100_000);
        let long = measure_nesting(&format!("note(\"c\")\n  .fast(1){run}  .slow(1)\n"));
        let short = measure_nesting("note(\"c\")\n  .fast(1)\n  .slow(1)\n");
        assert_eq!(long.chain, short.chain);
        assert_eq!(long.operators, short.operators);
    }

    /// The size limit and the parse stack are one guarantee in two halves: the
    /// limit bounds how deep source can nest, and the stack bounds what that
    /// depth costs. This parses the very worst case the limit admits -- every
    /// byte an opening bracket -- so the pairing is measured, not assumed.
    ///
    /// It is the backstop for the scan above being defeated, which a
    /// hand-written JavaScript tokenizer eventually will be.
    #[test]
    fn the_deepest_source_the_size_limit_admits_survives() {
        // Every construct where one byte buys one level, plus a mix, since a
        // parser may spend more stack alternating between them than repeating
        // one. `${` costs two bytes a level and so cannot go as deep.
        let mixed: String =
            "([{".repeat(MAX_STRUCTURAL_BYTES / 3 + 1)[..MAX_STRUCTURAL_BYTES].into();
        for worst in [
            "(".repeat(MAX_STRUCTURAL_BYTES),
            "[".repeat(MAX_STRUCTURAL_BYTES),
            "{".repeat(MAX_STRUCTURAL_BYTES),
            "`".repeat(MAX_STRUCTURAL_BYTES),
            "!".repeat(MAX_STRUCTURAL_BYTES),
            mixed,
            format!("x = {}", "${".repeat((MAX_STRUCTURAL_BYTES - 4) / 2)),
            // A member chain recurses in this crate's visitor rather than in
            // the parser, and spends one structural byte per link.
            format!("x{}", ".a".repeat(MAX_STRUCTURAL_BYTES)),
        ] {
            let opener = worst.chars().next().unwrap();
            assert!(structural_bytes(&worst) <= MAX_STRUCTURAL_BYTES);
            let parsed = on_parse_stack(|| {
                let allocator = Allocator::default();
                Parser::new(&allocator, &worst, SourceType::mjs())
                    .parse()
                    .diagnostics
                    .len()
            });
            // Unbalanced, so it must not parse cleanly -- but it must not take
            // the process down either.
            assert!(parsed.is_some(), "{opener} worst case did not return");

            // The parser is only half of it: the rewriters below walk the AST
            // recursively, and a chain overflows there rather than in the
            // parse. Both run on the reserved stack.
            let _ = transpile(&worst, &TranspileOptions::default());
        }
    }

    #[test]
    fn source_past_the_structural_limit_is_refused() {
        let oversized = "(".repeat(MAX_STRUCTURAL_BYTES + 1);
        let error = check_nesting(&oversized).expect_err("oversized source was accepted");
        assert!(
            error.message.contains("brackets, dots, and operators"),
            "{error:?}"
        );

        // Length is not the measure: a large file that spends nothing on
        // structure is fine, which is what lets a 4 MB prebake through.
        let mut roomy = " ".repeat(4 * 1024 * 1024);
        roomy.push_str("note(\"c\")");
        assert!(
            check_nesting(&roomy).is_ok(),
            "a 4 MB setup file was refused"
        );

        // The busiest vendored song spends 263.
        assert!(check_nesting(&"n(\"0\").s(\"bd\")\n".repeat(1_000)).is_ok());
    }

    /// A source past the structural size limit is refused with no KabelSalat
    /// calls listed, even with a code `/` in it.
    #[test]
    fn an_oversized_source_with_a_division_is_refused() {
        let oversized = format!("K(x);1/2;{}", "[".repeat(MAX_STRUCTURAL_BYTES + 1));
        let output = transpile(&oversized, &TranspileOptions::default());
        assert_eq!(output.diagnostics.len(), 1, "{:?}", output.diagnostics);
        assert_eq!(
            output.diagnostics[0].message,
            check_nesting(&oversized)
                .expect_err("over the size limit")
                .message
        );
        assert!(output.kabelsalat_calls.is_empty());
    }

    /// Every bracket in these lives inside a string or comment, so none of them
    /// is code the parser will recurse through.
    #[test]
    fn brackets_that_are_not_code_do_not_count_toward_the_limit() {
        let mini = format!("note(\"{}\")", "[bd sd]".repeat(2_000));
        assert!(check_nesting(&mini).is_ok(), "mini-notation was refused");

        let commented = format!("// {}\nnote(\"c\")", "(".repeat(2_000));
        assert!(check_nesting(&commented).is_ok(), "a comment was refused");

        let block = format!("/* {} */ note(\"c\")", "[".repeat(2_000));
        assert!(check_nesting(&block).is_ok(), "a block comment was refused");

        let escaped = format!("const s = \"{}\"; note(\"c\")", "\\\"(".repeat(1_000));
        assert!(
            check_nesting(&escaped).is_ok(),
            "an escaped quote was refused"
        );

        let template = format!("const s = `{}`; note(\"c\")", "(".repeat(2_000));
        assert!(
            check_nesting(&template).is_ok(),
            "a template literal was refused"
        );
    }

    /// Code inside `${...}` is still code, and still recursed through.
    #[test]
    fn template_interpolation_counts_because_it_is_code() {
        let interpolated = format!(
            "const s = `${{{}1{}}}`; note(\"c\")",
            "(".repeat(2_000),
            ")".repeat(2_000)
        );
        let error = check_nesting(&interpolated).expect_err("template code was accepted");
        assert!(error.message.contains("levels deep"), "{error:?}");
    }

    /// The source is written one call per line; that is one chain, not many.
    /// Separate statements are not, or a long score would be refused for the
    /// sum of its lines.
    #[test]
    fn a_chain_is_measured_per_expression_not_per_file() {
        let continued = format!("note(\"c\")\n{}", "  .fast(1)\n".repeat(600));
        let error = check_nesting(&continued).expect_err("a 600-call chain was accepted");
        assert!(error.message.contains("600 calls"), "{error:?}");

        let statements = "note(\"c\").fast(1).gain(0.5)\n".repeat(600);
        assert!(
            check_nesting(&statements).is_ok(),
            "separate statements were summed into one chain"
        );

        let arguments = format!("stack({})", "note(\"c\").fast(1), ".repeat(600));
        assert!(
            check_nesting(&arguments).is_ok(),
            "sibling arguments were summed into one chain"
        );
    }

    #[test]
    fn parses_es2022_features_used_by_strudel() {
        let cases = [
            "await samples('x');",
            "const f = async (x = 1) => ({ ...x, y: x?.y ?? 2 });",
            "class C { #x = 1; static { this.y = 2 } m() { return this.#x } }",
            "const n = 1_000n; const r = /(?<x>a)/du;",
            "label: note(`c${3}`).gain(.5)",
        ];
        for source in cases {
            validate_es2022(source).unwrap_or_else(|errors| {
                panic!("{source:?}: {errors:?}");
            });
        }
    }

    #[test]
    fn production_jsx_path_decodes_values_and_prints_reparseable_entities() {
        let source = "const x = <a title=\"&quot;&apos;&amp;nbsp;\">&lt;&gt;&#123;&#125;&amp;nbsp;&#xD83D;&#xDE00;</a>;";
        let parsed = parse_and_print_path(source, "fixture.jsx");
        assert!(parsed.diagnostics.is_empty(), "{:#?}", parsed.diagnostics);
        assert_eq!(
            parsed.code,
            "const x = <a title=\"&quot;&apos;&amp;nbsp;\">&lt;&gt;&#123;&#125;&amp;nbsp;\u{1f600}</a>;"
        );
        let reparsed = parse_and_print_path(&parsed.code, "fixture.jsx");
        assert!(
            reparsed.diagnostics.is_empty(),
            "{:#?}",
            reparsed.diagnostics
        );
        assert_eq!(reparsed.code, parsed.code, "JSX printing is not idempotent");

        let crlf = parse_and_print_path("const x = <a>&#13;&#10;</a>;", "fixture.jsx");
        assert!(crlf.diagnostics.is_empty(), "{:#?}", crlf.diagnostics);
        assert_eq!(crlf.code, "const x = <a>&#13;\n</a>;");

        let raw_crlf = parse_and_print_path("const x = <a>\r\n&nbsp;</a>;", "fixture.jsx");
        assert!(
            raw_crlf.diagnostics.is_empty(),
            "{:#?}",
            raw_crlf.diagnostics
        );
        assert_eq!(raw_crlf.code, "const x = <a>\n\u{a0}</a>;");
        let raw_crlf_only = parse_and_print_path("const x = <a>\r\n</a>;", "fixture.jsx");
        assert!(
            raw_crlf_only.diagnostics.is_empty(),
            "{:#?}",
            raw_crlf_only.diagnostics
        );
        assert_eq!(raw_crlf_only.code, "const x = <a>\n</a>;");

        let lone = parse_and_print_path("const x = <a>&#55296;</a>;", "fixture.jsx");
        assert!(lone.diagnostics.is_empty(), "{:#?}", lone.diagnostics);
        assert_eq!(lone.code, "const x = <a>&#55296;</a>;");

        let mixed = parse_and_print_path(
            "const x = <><a>&nbsp;</a><a>&#55296;</a></>;",
            "fixture.jsx",
        );
        assert!(mixed.diagnostics.is_empty(), "{:#?}", mixed.diagnostics);
        assert_eq!(
            mixed.code, "const x = <><a>\u{a0}</a><a>&#55296;</a></>;",
            "an unrepresentable node must not be re-encoded because a sibling normalized"
        );
    }

    #[test]
    fn jsx_entity_decoder_pins_acorn_scan_and_from_char_code_rules() {
        assert_eq!(
            decode_acorn_jsx_entities("&nbsp;&#160;&#xA0;"),
            Some("\u{a0}\u{a0}\u{a0}".into())
        );
        assert_eq!(
            decode_acorn_jsx_entities("&#x1F600;&#128512;"),
            Some("\u{f600}\u{f600}".into())
        );
        assert_eq!(
            decode_acorn_jsx_entities("&#00000160;&#000000160;"),
            Some("\u{a0}&#000000160;".into())
        );
        assert_eq!(decode_acorn_jsx_entities("&bogus; &broken"), None);
        assert_eq!(
            decode_acorn_jsx_entities("&toString;&valueOf;&__proto__;"),
            Some(
                "function toString() { [native code] }function valueOf() { [native code] }[object Object]"
                    .into()
            )
        );
        assert_eq!(decode_acorn_jsx_entities("&#55296;"), None);
        assert_eq!(decode_acorn_jsx_text("\r\n&nbsp;"), Some("\n\u{a0}".into()));
        assert_eq!(decode_acorn_jsx_text("\r\n"), Some("\n".into()));
        assert_eq!(decode_acorn_jsx_text("\r"), None);
        assert_eq!(
            decode_acorn_jsx_entities("&#55357;&#56832;&#xD83D;&#xDE00;"),
            Some("\u{1f600}\u{1f600}".into())
        );
    }

    #[test]
    fn jsx_codegen_site_lookup_is_sorted_and_handles_many_nodes() {
        let children = "<a>&lt;</a>".repeat(512);
        let source = format!("const x = <>{children}</>;");
        let allocator = Allocator::default();
        let parsed = Parser::new(&allocator, &source, SourceType::jsx()).parse();
        assert!(parsed.diagnostics.is_empty(), "{:#?}", parsed.diagnostics);
        let mut program = parsed.program;
        let sites = normalize_jsx_values_with_sites(&allocator, &mut program);
        assert_eq!(sites.len(), 512);
        assert!(sites.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(sites.binary_search(&sites[0]).is_ok());
        assert!(sites.binary_search(&sites[sites.len() - 1]).is_ok());

        let printed = parse_and_print_path(&source, "fixture.jsx");
        assert!(printed.diagnostics.is_empty(), "{:#?}", printed.diagnostics);
        assert_eq!(printed.code.matches("&lt;").count(), 512);
        let reparsed = parse_and_print_path(&printed.code, "fixture.jsx");
        assert!(
            reparsed.diagnostics.is_empty(),
            "{:#?}",
            reparsed.diagnostics
        );
    }

    /// `add_return` injects `return ` before the final statement. Trailing
    /// comments are not statements, and treating them as one is silently
    /// destructive: `return // …;` is bare `return;` under ASI, and a block
    /// comment gets the keyword spliced into its middle.
    #[test]
    fn add_return_steps_over_trailing_comments() {
        let options = TranspileOptions {
            add_return: true,
            ..TranspileOptions::default()
        };
        for (source, expected) in [
            (
                "note(\"c\").add(1)\n// trailing",
                "return note(m('c', 5)).add(1);\n// trailing",
            ),
            (
                "s(\"bd\")\n/* block ; comment */",
                "return s(m('bd', 2));\n/* block ; comment */",
            ),
            // A `;` inside a comment is not a statement terminator.
            ("s(\"bd\") // note; here", "return s(m('bd', 2));"),
            ("s(\"bd sd\").fast(2)", "return s(m('bd sd', 2)).fast(2);"),
        ] {
            assert_eq!(
                transpile(source, &options).output,
                expected,
                "add_return mishandled {source:?}"
            );
        }
    }

    /// A source that is ONLY a comment has no final statement at all.
    #[test]
    fn add_return_on_comment_only_source() {
        let options = TranspileOptions {
            add_return: true,
            ..TranspileOptions::default()
        };
        let out = transpile("// nothing here", &options).output;
        assert!(
            out.starts_with("return silence;"),
            "expected a silence fallback, got {out:?}"
        );
    }

    #[test]
    fn the_mini_and_sample_transforms_all_transpile() {
        let options = TranspileOptions {
            add_return: false,
            ..TranspileOptions::default()
        };
        assert_eq!(transpile("\"c3\"", &options).output, "m('c3', 0);");
        assert_eq!(
            transpile("stack(\"c3\",\"bd sd\")", &options).output,
            "stack(m('c3', 6), m('bd sd', 11));"
        );
        assert_eq!(transpile("`c3`", &options).output, "m('c3', 0);");
        assert_eq!(transpile("xxx`c3`", &options).output, "xxx`c3`;");
        assert_eq!(
            transpile("samples('xxx');", &options).output,
            "await samples('xxx');"
        );
        assert_eq!(
            transpile("await samples('xxx');", &options).output,
            "await samples('xxx');"
        );
    }

    /// A member call, a constructor, or a function or method named `samples`
    /// still parses and gains no `await`, while a plain call of the identifier
    /// `samples` is awaited once wherever it sits.
    #[test]
    fn await_is_injected_only_into_the_samples_fetch_call() {
        let options = TranspileOptions {
            add_return: false,
            ..TranspileOptions::default()
        };
        for source in [
            "const p = bank.samples('bd')",
            "const p = bank ?. samples('bd')",
            "const p = bank./* bank */samples('bd')",
            "const p = new samples('bd')",
            "function samples(bank) { return bank }",
            "const f = function samples(x) { return x }",
            "function* samples(bank) { yield bank }",
            "const bank = { samples(name) { return name } }",
            "class Bank { static samples(name) { return name } }",
        ] {
            let output = transpile(source, &options);
            assert!(
                output.diagnostics.is_empty(),
                "{source:?} must still parse: {:?}",
                output.diagnostics
            );
            assert!(
                !output.output.contains("await"),
                "{source:?} must not gain an await: {}",
                output.output
            );
        }
        for (source, expected) in [
            (
                "stack(...samples('xxx'))",
                "stack(...await samples('xxx'));",
            ),
            (
                "const b = new Bank(samples('xxx'))",
                "const b = new Bank(await samples('xxx'));",
            ),
            ("s(`${samples('x')}`)", "s(`${await samples('x')}`);"),
            ("await (samples('xxx'))", "await samples('xxx');"),
        ] {
            assert_eq!(transpile(source, &options).output, expected, "{source:?}");
        }
    }

    #[test]
    fn a_direct_hydra_image_url_is_a_javascript_string_not_mini_notation() {
        let options = TranspileOptions {
            add_return: false,
            ..TranspileOptions::default()
        };
        let result = transpile(
            "s0.initImage(\"https://i.imgur.com/zFttbWq.jpg\")\ns(\"bd sd\")",
            &options,
        );
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        assert_eq!(
            result.output,
            "s0.initImage('https://i.imgur.com/zFttbWq.jpg');\ns(m('bd sd', 50));"
        );
        assert!(
            !result.output.contains("initImage(m("),
            "the URL must reach native Hydra as a real JS string: {}",
            result.output
        );

        // The exemption is the first argument of a direct s0..s3 call only.
        assert_eq!(
            transpile("other.initImage(\"bd\")", &options).output,
            "other.initImage(m('bd', 16));"
        );
        assert_eq!(
            transpile("s4.initImage(\"bd\")", &options).output,
            "s4.initImage(m('bd', 13));"
        );
    }

    #[test]
    fn disabled_mini_locations_use_the_hosts_no_location_sentinel() {
        let options = TranspileOptions {
            add_return: false,
            emit_mini_locations: false,
            registered_languages: vec!["mondo".into()],
            ..TranspileOptions::default()
        };
        let output = transpile(r#"globalThis.helper = () => note("c4 e4")"#, &options);
        assert_eq!(
            output.output,
            "globalThis.helper = () => note(m('c4 e4', -1));"
        );
        assert!(output.mini_locations.is_empty());
        // A registered language keeps the sentinel; `tidal` is not one
        // and is left exactly as written.
        assert_eq!(
            transpile("tidal`c4 e4`; mondo`bd sd`", &options).output,
            "tidal`c4 e4`;\nmondo('bd sd', -1);"
        );
    }

    /// Pins that only a comment whose trimmed text starts with `mini-off` or
    /// `mini-on` toggles mini notation; one that mentions a directive mid-text
    /// changes nothing.
    #[test]
    fn mini_off_toggles_only_on_a_comment_that_starts_with_it() {
        let options = TranspileOptions {
            add_return: false,
            ..TranspileOptions::default()
        };
        assert_eq!(
            transpile("// turning mini-off for this bit\ns(\"bd sd\")", &options).output,
            "// turning mini-off for this bit\ns(m('bd sd', 35));"
        );
        assert_eq!(
            transpile(
                "/* mini-off */\ns(\"bd sd\")\n// not mini-on yet\ns(\"hh\")\n//mini-on\ns(\"bd sd\")",
                &options
            )
            .output,
            "/* mini-off */\ns('bd sd');\n// not mini-on yet\ns('hh');\n//mini-on\ns(m('bd sd', 65));"
        );
    }

    /// Rust callers need UTF-8 byte offsets to slice source strings, while the
    /// JavaScript parser reports UTF-16 code-unit offsets. This checks Unicode
    /// conversion together with the containing block's byte offset.
    #[test]
    fn mini_locations_are_absolute_utf8_byte_offsets() {
        let source = "const 音 = \"é 音\"";
        let result = transpile(
            source,
            &TranspileOptions {
                add_return: false,
                block_offset: 7,
                ..TranspileOptions::default()
            },
        );

        assert_eq!(result.output, "const 音 = m('é 音', 19);");
        assert_eq!(result.mini_locations, vec![(20, 22), (23, 26)]);
        for &(from, to) in &result.mini_locations {
            assert!(source.is_char_boundary(from - 7));
            assert!(source.is_char_boundary(to - 7));
        }
        assert_eq!(&source[13..15], "é");
        assert_eq!(&source[16..19], "音");
    }

    #[test]
    fn return_and_async_options_match_runtime_contract() {
        assert_eq!(
            transpile("note(\"c3\")", &TranspileOptions::default()).output,
            "return note(m('c3', 5));"
        );
        let wrapped = transpile(
            "note(\"c3\")",
            &TranspileOptions {
                wrap_async: true,
                ..TranspileOptions::default()
            },
        );
        assert_eq!(wrapped.output, "(async ()=>{return note(m('c3', 5));})()");
    }

    /// A registered language becomes a call; a tag nothing registered is
    /// left alone.
    ///
    /// `tidal` used to be rewritten by name, which meant a score using it
    /// transpiled cleanly and then died on a function no engine here has.
    /// Leaving it as written is not support either, but it does not
    /// pretend to be: the error names the tag rather than a call the
    /// reader never wrote. Both alternative notations are listed as
    /// unsupported in `docs/compatibility.md`.
    #[test]
    fn transforms_labels_registered_languages_and_leaves_the_rest() {
        let simple = TranspileOptions {
            add_return: false,
            registered_languages: vec!["mondo".into()],
            ..TranspileOptions::default()
        };
        assert_eq!(
            transpile("$: note(\"c3\")", &simple).output,
            "note(m('c3', 8)).p('$');"
        );
        assert_eq!(transpile("mondo`a b`", &simple).output, "mondo('a b', 6);");
        // A space or line break between tag and template keeps the tag whole.
        for source in ["mondo `a b`", "mondo\n`a b`"] {
            assert_eq!(transpile(source, &simple).output, "mondo('a b', 7);");
        }
        // Not registered, not rewritten - and not turned into mini
        // notation either, because it is still a tagged template.
        assert_eq!(
            transpile("tidal`note \"c3\"`", &simple).output,
            "tidal`note \"c3\"`;"
        );
        assert_eq!(transpile("1`a b`", &simple).output, "1`a b`;");
        let none = TranspileOptions {
            add_return: false,
            ..TranspileOptions::default()
        };
        assert_eq!(transpile("mondo`a b`", &none).output, "mondo`a b`;");
    }

    #[test]
    fn unicode_escaped_identifier_tags_keep_their_registered_language() {
        let options = TranspileOptions {
            add_return: false,
            emit_mini_locations: false,
            registered_languages: vec!["mondo".into()],
            ..TranspileOptions::default()
        };
        for source in [
            r"\u006dondo`a b`",
            r"mon\u0064o`a b`",
            r"\u{6d}ondo`a b`",
            r"mond\u{6f}`a b`",
        ] {
            let result = transpile(source, &options);
            assert!(
                result.diagnostics.is_empty(),
                "{source}: {:?}",
                result.diagnostics
            );
            assert_eq!(result.output, "mondo('a b', -1);", "{source}");
        }
        assert_eq!(transpile(r"fo\u{6f}`a b`", &options).output, "foo`a b`;");
        let located = TranspileOptions {
            emit_mini_locations: true,
            ..options
        };
        assert_eq!(
            transpile(r"\u{6d}ondo`a b`", &located).output,
            "mondo('a b', 11);"
        );
    }

    #[test]
    fn escaped_template_dollar_brace_is_static_mini_text() {
        let options = TranspileOptions {
            add_return: false,
            ..TranspileOptions::default()
        };
        let result = transpile(r#"s(`bd \${snare}`)"#, &options);
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        assert_eq!(result.output, "s(m('bd ${snare}', 2));");

        let language = TranspileOptions {
            add_return: false,
            registered_languages: vec!["mondo".into()],
            ..TranspileOptions::default()
        };
        let result = transpile(r#"mondo`a \${b}`"#, &language);
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        // Registered language handlers receive the raw notation spelling.
        assert_eq!(result.output, r"mondo('a \\${b}', 6);");

        // Escaping the backslash makes `${` a real substitution.
        let interpolated = transpile(r#"s(`bd \\${snare}`)"#, &options);
        assert!(
            interpolated.diagnostics.is_empty(),
            "{:?}",
            interpolated.diagnostics
        );
        assert_eq!(interpolated.output, r"s(`bd \\${snare}`);");

        // One real substitution keeps the template interpolated.
        let mixed = transpile(r#"s(`\${a} ${b}`)"#, &options);
        assert!(mixed.diagnostics.is_empty(), "{:?}", mixed.diagnostics);
        assert_eq!(mixed.output, r"s(`\${a} ${b}`);");
    }

    /// A template's `${...}` substitution is code: a backtick, brace or comment
    /// inside it does not end the template, and an escaped `\${` opens none.
    #[test]
    fn a_template_scan_skips_the_code_of_its_substitutions() {
        for template in [
            "`${`bd sd!`}`",
            "`a ${`b ${`c`}`} d`",
            "`${ '`' + \"`\" }`",
            "`${ {a: 1}.a }`",
            "`${ /* ` } */ 1 }`",
            "`${ 1 // ` }\n}`",
            "`\\${`",
        ] {
            assert_eq!(
                scan_quoted(template.as_bytes(), 0, b'`'),
                template.len(),
                "{template:?}"
            );
        }
        assert_eq!(
            scan_quoted(b"`${`a`", 0, b'`'),
            6,
            "an open template runs to EOF"
        );
    }

    /// A template nested in a substitution is copied with its outer template, so
    /// the score stays valid JavaScript and a phrase after it keeps its offset.
    #[test]
    fn a_template_nested_in_a_substitution_is_copied_as_written() {
        let options = TranspileOptions {
            add_return: false,
            ..TranspileOptions::default()
        };
        let result = transpile("note(`${`bd sd!`}`)\ns(\"bd\")", &options);
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        assert_eq!(result.output, "note(`${`bd sd!`}`);\ns(m('bd', 22));");
    }

    /// A line ending on the dot of a chain, an operator or a comma is not
    /// the end of the lane - as on strudel.cc, where `.decay(.08).` with
    /// `gain(.4)` on the next line is one pattern.
    #[test]
    fn a_labeled_statement_reads_on_past_a_line_that_ends_mid_expression() {
        let options = TranspileOptions {
            add_return: false,
            ..TranspileOptions::default()
        };
        for (source, lanes, expect) in [
            (
                "$: s(\"white!8\").decay(.08).\ngain(.4)",
                1,
                ".decay(0.08).gain(0.4).p('$')",
            ),
            (
                "$: s(\"white!8\").decay(.08).\n  gain(.4)\n$: s(\"bd\")",
                2,
                ".decay(0.08).gain(0.4).p('$');",
            ),
            ("$: stack(s(\"bd\"),\n  s(\"hh\"))", 1, ")).p('$')"),
            ("$: s(\"bd\").gain(1 +\n  .2)", 1, "0.2).p('$')"),
            ("$: x =>\n  s(\"bd\")", 1, ".p('$')"),
            // A dot inside a trailing comment is prose: the lane ends there
            // and `foo()` is its own statement.
            ("$: s(\"bd\") // then.\nfoo()", 1, "foo()"),
        ] {
            let result = transpile(source, &options);
            assert!(
                result.diagnostics.is_empty(),
                "{source:?}: {:?}",
                result.diagnostics
            );
            assert_eq!(
                result.output.matches(".p('$')").count(),
                lanes,
                "{source:?} -> {}",
                result.output
            );
            assert!(
                result.output.contains(expect),
                "{source:?} -> {}",
                result.output
            );
            assert!(
                !result.output.contains(".).p(") && !result.output.contains(".\n"),
                "{source:?} registered a lane ending in a dot: {}",
                result.output
            );
        }
    }

    /// The parser reads the rewritten text, where every mini-notation
    /// string has grown into its `m(…)` call; the mark has to land on the
    /// score's own text, not that many cells to the right of it.
    #[test]
    fn a_parse_error_is_reported_where_it_sits_in_the_score() {
        let options = TranspileOptions {
            add_return: false,
            ..TranspileOptions::default()
        };
        let open_call = "$: s(\"breaks/2\").fit()\n  .scrub(irand(16).div(\n  .orbit(2)\n\n$: s(\"white!8\").decay(.08)";
        let stray = "s(\"bd\").n(\"1 2\") )";
        let unclosed = "$: s(\"bd\") foo(";
        let laid_over = "s(`bd\nsd`)\nconst broken = ) 2";
        for (source, at) in [
            (open_call, open_call.find(".orbit").expect("the dot")),
            // The statement should have ended where the space is.
            (
                stray,
                stray.rfind(' ').expect("the gap before the stray paren"),
            ),
            // The lane runs to the end past the open call; the fault is the
            // missing break before `foo`.
            (unclosed, unclosed.find(" foo").expect("the gap")),
            // A phrase laid over several lines leaves the fault on its own line.
            (laid_over, laid_over.rfind(')').expect("the stray paren")),
        ] {
            let result = transpile(source, &options);
            let offsets = result
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.offset)
                .collect::<Vec<_>>();
            assert!(
                offsets.contains(&Some(at)),
                "{source:?}: expected a finding at {at}, got {:?}",
                result.diagnostics
            );
        }
        assert_eq!(align_column("abc", "abc", 1), 1);
        assert_eq!(align_column("s(\"bd\").x", "s(m('bd', 2)).x", 14), 8);
        assert_eq!(
            align_column("$: s(\"bd\")", " s(m('bd', 5)).p('$')", 21),
            10
        );
    }

    /// Pins that a mini phrase or registered-language template laid over several
    /// lines keeps its line breaks, as written, inside its call, and that an
    /// escaped `\n` adds none. The line map then reaches the lines below, while
    /// the printed output, a lane's end and a widget splice match the phrase
    /// written on one line.
    #[test]
    fn a_multi_line_mini_phrase_keeps_the_scores_line_breaks() {
        let options = TranspileOptions {
            add_return: false,
            registered_languages: vec!["mondo".into()],
            ..TranspileOptions::default()
        };
        for (source, expect) in [
            ("s(`bd\nsd`)", "s(m('bd\\nsd', 2\n))"),
            ("$: `bd\nsd`", "$: m('bd\\nsd', 3\n)"),
            ("mondo`bd\nsd`", "mondo('bd\\nsd', 6\n)"),
            ("s(`bd\r\nsd`)", "s(m('bd\\nsd', 2\r\n))"),
            ("s(`bd\rsd`)", "s(m('bd\\nsd', 2\r))"),
            // An escaped `\n` is not a line break.
            ("s(\"bd\\nsd\")", "s(m('bd\\nsd', 2))"),
        ] {
            let rewrite = rewrite_mini_literals(
                source,
                &options,
                &RegexLiteralRanges::parse(source),
                &OrdinaryStringLiteralRanges::parse(source),
            );
            assert_eq!(rewrite.output, expect, "{source:?}");
            assert!(
                rewrite.diagnostics.is_empty(),
                "{source:?}: {:?}",
                rewrite.diagnostics
            );
        }
        let printed = transpile("s(`bd\nsd`).rev()\nfoo()", &options);
        assert_eq!(printed.output, "s(m('bd\\nsd', 2)).rev();\nfoo();");
        assert_eq!(printed.line_map.original_position(1, 0), Some((2, 0)));
        // A lane ends where it would for the phrase written on one line.
        for (laid_over, one_line) in [
            ("$: `a\nb` + y", "$: `a b` + y"),
            ("$: x ? `a\nb` : `c`", "$: x ? `a b` : `c`"),
        ] {
            assert_eq!(
                transpile(laid_over, &options).output.replace("\\n", " "),
                transpile(one_line, &options).output
            );
        }
        assert_eq!(
            transpile_for_layout("s(`bd\nsd`).pianoroll()").output,
            "s(m('bd\\nsd', 2)).pianoroll('layout_widget_pianoroll_0_0-22', 0);"
        );
    }

    #[test]
    fn every_javascript_line_terminator_ends_a_labeled_statement() {
        let options = TranspileOptions {
            add_return: false,
            ..TranspileOptions::default()
        };
        for separator in ["\r", "\n", "\r\n", "\u{2028}", "\u{2029}"] {
            let source = format!("$: s(\"bd\"){separator}foo()");
            let result = transpile(&source, &options);
            assert!(
                result.diagnostics.is_empty(),
                "line terminator {separator:?}: {:?}",
                result.diagnostics
            );
            assert_eq!(
                result.output, "s(m('bd', 5)).p('$');\nfoo();",
                "line terminator {separator:?} joined the next statement to the label"
            );
        }
    }

    /// Pins that whitespace or a comment between a label and its colon still
    /// makes a `.p(name)` lane, as it does in JavaScript and on strudel.cc.
    #[test]
    fn a_label_with_trivia_before_the_colon_still_rewrites() {
        let options = TranspileOptions {
            add_return: false,
            ..TranspileOptions::default()
        };
        assert_eq!(
            transpile("kick : s(\"bd\")", &options).output,
            "s(m('bd', 9)).p('kick');"
        );
        assert_eq!(
            transpile("kick : s(\"bd\")", &TranspileOptions::default()).output,
            "return s(m('bd', 9)).p('kick');"
        );
        assert_eq!(
            transpile("kick\n: s(\"bd\")", &options).output,
            "s(m('bd', 9)).p('kick');"
        );
        assert_eq!(
            transpile("kick /* lane */ : s(\"bd\")", &options).output,
            "/* lane */ s(m('bd', 20)).p('kick');"
        );
        // The gap keeps its lines, so a parse error still lands on its own text.
        let source = "kick\n: s(\"bd\") foo(";
        let result = transpile(source, &options);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.offset == source.find(" foo")),
            "{source:?}: {:?}",
            result.diagnostics
        );
    }

    #[test]
    fn ports_pinned_kabelsalat_transform_cases() {
        let simple = TranspileOptions {
            add_return: false,
            ..TranspileOptions::default()
        };
        assert_eq!(
            transpile("K(\"bd sd\")", &simple).output,
            "worklet('pat[0]', m('bd sd', 2));"
        );
        assert_eq!(transpile("K(1+2)", &simple).output, "worklet('1 + 2');");
        assert_eq!(
            transpile("K(S(\"bd\".fast(4)))", &simple).output,
            "worklet('pat[0]', m('bd', 4).fast(4));"
        );
        assert_eq!(
            transpile("foo.K(osc(2), .5)", &simple).output,
            "foo.worklet('osc(2)', 0.5);"
        );
    }

    /// A chain of KabelSalat calls, each in the previous one's later
    /// arguments, is rewritten from one listing of the source.
    #[test]
    fn a_chain_of_kabelsalat_calls_is_listed_once() {
        let depth = 500;
        let literal = format!("'{}'", "x".repeat(1 << 16));
        let source = format!("{}{literal}{}", "K(a, ".repeat(depth), ")".repeat(depth));
        KABELSALAT_LISTED_BYTES.with(|listed| listed.set(0));
        let output = rewrite_kabelsalat(&source, false);
        assert_eq!(
            output,
            format!(
                "{}{literal}{}",
                "worklet('a', ".repeat(depth),
                ")".repeat(depth)
            )
        );
        assert_eq!(
            KABELSALAT_LISTED_BYTES.with(std::cell::Cell::get),
            source.len()
        );
    }

    /// A chain of KabelSalat calls, each in the previous one's lifted `S(…)`
    /// pattern, is listed once and left as code below the outer call.
    #[test]
    fn a_chain_of_kabelsalat_calls_in_lifted_patterns_is_listed_once() {
        let depth = 250;
        let literal = format!("'{}'", "x".repeat(1 << 16));
        let source = format!("{}{literal}{}", "K(S(".repeat(depth), "))".repeat(depth));
        KABELSALAT_LISTED_BYTES.with(|listed| listed.set(0));
        let output = rewrite_kabelsalat(&source, false);
        assert_eq!(
            output,
            format!("worklet('pat[0]', {})", &source[4..source.len() - 2])
        );
        assert_eq!(
            KABELSALAT_LISTED_BYTES.with(std::cell::Cell::get),
            source.len()
        );
    }

    /// A KabelSalat call nested in a graph is stringified as written, so the
    /// output stays linear in the source at every depth.
    #[test]
    fn a_kabelsalat_call_in_a_graph_is_stringified_as_written() {
        assert_eq!(
            transpile_for_layout("K(K('x'))").output,
            r"worklet('K(\'x\')');"
        );
        for depth in (5..=40).step_by(5) {
            let source = format!("{}'x'{}", "K(".repeat(depth), ")".repeat(depth));
            let output = transpile_for_layout(&source).output;
            assert!(
                output.len() < 4 * source.len(),
                "depth {depth}: {} bytes of output for a {}-byte source",
                output.len(),
                source.len()
            );
        }
    }

    /// A KabelSalat call in a pattern lifted out of a graph is left as code,
    /// for the runtime to refuse.
    #[test]
    fn a_kabelsalat_call_in_a_lifted_pattern_is_left_as_code() {
        assert_eq!(
            transpile_for_layout("K(S(K(x)))").output,
            "worklet('pat[0]', K(x));"
        );
    }

    #[test]
    fn a_kabelsalat_call_in_another_ones_later_arguments_is_one_of_its_own() {
        // Pins that the nested call is rewritten and listed like the outer
        // one, and that the slider in its graph gets no record.
        let source = "K(a, K(slider(0.3)));";
        let result = transpile_for_layout(source);
        assert_eq!(result.output, "worklet('a', worklet('slider(.3)'));");
        assert_eq!(
            result
                .kabelsalat_calls
                .iter()
                .map(|call| (call.name.clone(), call.stringified.clone()))
                .collect::<Vec<_>>(),
            [(0..1, 2..3), (5..6, 7..18)]
        );
        assert!(result.widgets.is_empty(), "{:?}", result.widgets);
    }

    #[test]
    fn kabelsalat_calls_on_a_receiver_keep_the_receiver() {
        // The second call's receiver is the rewritten first call.
        assert_eq!(
            transpile_for_layout("K(x).K(y)").output,
            "worklet('x').worklet('y');"
        );
        // An empty graph collapses to `K()` on the same receiver.
        assert_eq!(
            transpile_for_layout("s(\"bd\").K()").output,
            "s(m('bd', 2)).K();"
        );
    }

    #[test]
    fn widget_semantics_emit_stable_deferred_records() {
        let options = TranspileOptions {
            add_return: false,
            widget_methods: vec!["scope".into()],
            id: Some("repl".into()),
            ..TranspileOptions::default()
        };
        let result = transpile("s(\"bd\").scope(); slider(.5, 0, 2, .1)", &options);
        assert_eq!(result.widgets.len(), 2);
        assert!(
            result
                .widgets
                .iter()
                .all(|widget| widget.deferred_rendering)
        );
        assert_eq!(result.widgets[0].widget_type, "scope");
        assert_eq!(result.widgets[0].id, "repl_widget_scope_0_0-15");
        assert_eq!(result.widgets[0].options.as_deref(), Some(""));
        assert_eq!(result.widgets[1].id, "24:26");
        assert_eq!(result.widgets[1].options, None);
        assert_eq!(
            result.output,
            "s(m('bd', 2)).scope('repl_widget_scope_0_0-15', 0);\nsliderWithID('24:26', 0.5, 0, 2, 0.1);"
        );
    }

    #[test]
    fn regex_literals_are_opaque_to_source_rewriters_and_widget_discovery() {
        let source = r#"const matched = /slider(.5)/.test('sliderx5');
const looksVisual = /_scope()/.test('_scope');
const transformNames = /K(S("bd")) samples() \.5/;
slider(.25);
p._scope({ test: /[),]/ });"#;
        let result = transpile(
            source,
            &TranspileOptions {
                add_return: false,
                widget_methods: vec!["_scope".into()],
                ..TranspileOptions::default()
            },
        );

        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        assert_eq!(
            result
                .widgets
                .iter()
                .map(|widget| widget.widget_type.as_str())
                .collect::<Vec<_>>(),
            vec!["slider", "_scope"]
        );
        assert!(
            result
                .widgets
                .iter()
                .all(|widget| !source[widget.from..widget.to].starts_with('/')),
            "a regex literal was advertised as a widget: {:?}",
            result.widgets
        );
        assert_eq!(result.output.matches("sliderWithID").count(), 1);
        assert!(result.output.contains("/slider(.5)/.test('sliderx5')"));
        assert!(result.output.contains("/_scope()/.test('_scope')"));
        assert!(
            result.output.contains(r#"/K(S("bd")) samples() \.5/"#),
            "a non-widget source rewriter changed regex contents: {}",
            result.output
        );
    }

    #[test]
    fn division_only_scores_reuse_the_empty_regex_result_downstream() {
        REGEX_LITERAL_PARSE_COUNT.with(|count| count.set(0));
        let division = transpile_here(
            "setcpm(140/4)",
            &TranspileOptions {
                add_return: false,
                ..TranspileOptions::default()
            },
        );
        assert!(
            division.diagnostics.is_empty(),
            "{:?}",
            division.diagnostics
        );
        assert_eq!(
            REGEX_LITERAL_PARSE_COUNT.with(std::cell::Cell::get),
            1,
            "division was reparsed as a possible regex in downstream transforms"
        );

        REGEX_LITERAL_PARSE_COUNT.with(|count| count.set(0));
        let regex = transpile_here(
            "if (ok) /slider(.5)/.test(x)",
            &TranspileOptions {
                add_return: false,
                ..TranspileOptions::default()
            },
        );
        assert!(regex.diagnostics.is_empty(), "{:?}", regex.diagnostics);
        assert!(
            REGEX_LITERAL_PARSE_COUNT.with(std::cell::Cell::get) > 1,
            "real regex spans were not refreshed after offsets could change"
        );
    }

    #[test]
    fn division_expressions_do_not_hide_real_slider_calls() {
        let result = transpile(
            "const a = total / slider(.5); const b = value() / slider(.25)",
            &TranspileOptions {
                add_return: false,
                ..TranspileOptions::default()
            },
        );

        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        assert_eq!(
            result
                .widgets
                .iter()
                .filter(|widget| widget.widget_type == "slider")
                .count(),
            2
        );
        assert_eq!(result.output.matches("sliderWithID").count(), 2);
    }

    #[test]
    fn control_head_regexes_stay_opaque_across_every_source_rewriter() {
        let source = r#"if (ok) /slider(.5) _scope() samples() K(S("bd")) \.5/.test(x);
slider(.25);
p._scope();"#;
        let result = transpile(
            source,
            &TranspileOptions {
                add_return: false,
                widget_methods: vec!["_scope".into()],
                ..TranspileOptions::default()
            },
        );

        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        assert_eq!(
            result
                .widgets
                .iter()
                .map(|widget| widget.widget_type.as_str())
                .collect::<Vec<_>>(),
            vec!["slider", "_scope"]
        );
        assert_eq!(result.output.matches("sliderWithID").count(), 1);
        assert!(
            result
                .output
                .contains(r#"/slider(.5) _scope() samples() K(S("bd")) \.5/.test(x)"#),
            "a source rewriter changed the control-head regex: {}",
            result.output
        );
    }

    #[test]
    fn comment_and_postfix_division_do_not_hide_real_slider_calls() {
        let source = "const a = x /* keep */ / slider(.5); const b = x++ / slider(.25)";
        let result = transpile(
            source,
            &TranspileOptions {
                add_return: false,
                ..TranspileOptions::default()
            },
        );

        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        assert_eq!(
            result
                .widgets
                .iter()
                .filter(|widget| widget.widget_type == "slider")
                .count(),
            2
        );
        assert_eq!(result.output.matches("sliderWithID").count(), 2);
    }

    /// A member call named `slider` is its receiver's method: it records no
    /// widget and keeps its name, while bare `slider` calls beside it and inside
    /// its arguments are still recorded and rewritten with their own ids.
    #[test]
    fn member_calls_named_slider_belong_to_their_receiver() {
        let options = TranspileOptions {
            add_return: false,
            ..TranspileOptions::default()
        };

        let result = transpile(
            "const p = { slider: (x) => x }; p.slider(0.5); slider(0.7)",
            &options,
        );
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        assert_eq!(result.widgets.len(), 1, "{:?}", result.widgets);
        assert_eq!(result.widgets[0].value.as_deref(), Some("0.7"));
        assert!(result.output.contains("p.slider(0.5)"), "{}", result.output);
        assert_eq!(result.output.matches("sliderWithID").count(), 1);
        assert!(
            result
                .output
                .contains(&format!("sliderWithID('{}', 0.7)", result.widgets[0].id)),
            "{}",
            result.output
        );

        let nested = transpile("p.slider(slider(0.2))", &options);
        assert!(nested.diagnostics.is_empty(), "{:?}", nested.diagnostics);
        assert_eq!(nested.widgets.len(), 1, "{:?}", nested.widgets);
        assert_eq!(nested.widgets[0].value.as_deref(), Some("0.2"));
        assert!(
            nested.output.contains(&format!(
                "p.slider(sliderWithID('{}', 0.2))",
                nested.widgets[0].id
            )),
            "{}",
            nested.output
        );
    }

    #[test]
    fn slider_widget_defaults_only_bounds_that_are_absent() {
        let options = TranspileOptions {
            add_return: false,
            ..TranspileOptions::default()
        };

        let omitted = transpile("slider(.5)", &options);
        let omitted = omitted
            .widgets
            .iter()
            .find(|widget| widget.widget_type == "slider")
            .expect("omitted-bound slider widget");
        assert_eq!((omitted.min, omitted.max), (Some(0.0), Some(1.0)));

        let source = "const low = 0, high = 2; slider(.5, low, high, .1)";
        let dynamic = transpile(source, &options);
        let widget = dynamic
            .widgets
            .iter()
            .find(|widget| widget.widget_type == "slider")
            .expect("dynamic-bound slider widget");
        assert_eq!((widget.min, widget.max), (None, None));
        let rewritten = format!("sliderWithID('{}', 0.5, low, high, 0.1)", widget.id);
        assert!(
            dynamic.output.contains(&rewritten),
            "dynamic bounds disappeared from executable output: {}",
            dynamic.output
        );
    }

    #[test]
    fn non_finite_slider_bounds_and_steps_are_not_numeric_literals() {
        let options = TranspileOptions {
            add_return: false,
            ..TranspileOptions::default()
        };

        for source in [
            "slider(.5, NaN, Infinity, -Infinity)",
            "slider(.5, inf, -inf)",
            "slider(.5, 0, 1, NaN)",
        ] {
            let result = transpile(source, &options);
            let widget = result
                .widgets
                .iter()
                .find(|widget| widget.widget_type == "slider")
                .unwrap_or_else(|| panic!("missing slider widget for {source}"));
            assert!(
                widget.min.map(f64::is_finite).unwrap_or(true)
                    && widget.max.map(f64::is_finite).unwrap_or(true)
                    && widget.step.map(f64::is_finite).unwrap_or(true),
                "{source} advertised a non-finite bound: {widget:?}"
            );
        }
    }

    #[test]
    fn an_unterminated_quote_at_the_end_of_the_source_is_transpiled_not_panicked() {
        for source in ["note('", "'", "`", "note(\"abc"] {
            let result =
                std::panic::catch_unwind(|| transpile(source, &TranspileOptions::default()));
            assert!(result.is_ok(), "transpiling {source:?} panicked");
        }
    }

    #[test]
    fn unicode_line_separators_terminate_line_comments_for_the_scanners() {
        let options = TranspileOptions {
            add_return: false,
            widget_methods: vec!["_scope".into()],
            ..TranspileOptions::default()
        };

        // The code after the U+2028 separator is live source per JavaScript;
        // the scanner must not swallow it as part of the comment.
        let result = transpile("stack(// hidden\u{2028}p._scope())", &options);
        assert_eq!(result.widgets.len(), 1, "{:?}", result.widgets);
        assert!(
            result.output.contains("hidden"),
            "separator-terminated comment was mis-scanned: {}",
            result.output
        );

        // The same terminator inside a block argument must not hide the
        // closing delimiter from `matching_delimiter`.
        let result = transpile("stack(// one\u{2029}, still comment\np._scope())", &options);
        assert_eq!(
            result.widgets.len(),
            1,
            "a U+2029 inside a line comment broke delimiter matching"
        );
    }

    #[test]
    fn carriage_returns_terminate_line_comments_for_the_scanners() {
        let options = TranspileOptions {
            add_return: false,
            widget_methods: vec!["_scope".into()],
            ..TranspileOptions::default()
        };

        let source = "// slider(.1)\rslider(.5); // p._scope()\rp._scope()";
        let result = transpile(source, &options);
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        assert_eq!(
            result
                .widgets
                .iter()
                .map(|widget| widget.widget_type.as_str())
                .collect::<Vec<_>>(),
            vec!["slider", "_scope"],
            "a bare CR failed to end a line comment: {:?}",
            result.widgets
        );
        assert_eq!(result.output.matches("sliderWithID").count(), 1);
    }

    /// Transpiles with every visual method and the `layout` id prefix.
    fn transpile_for_layout(source: &str) -> TranspileOutput {
        transpile(
            source,
            &TranspileOptions {
                add_return: false,
                widget_methods: VISUAL_WIDGET_METHODS
                    .iter()
                    .map(|method| (*method).to_owned())
                    .collect(),
                id: Some("layout".into()),
                ..TranspileOptions::default()
            },
        )
    }

    /// The id of the slider whose value is the first `value` in `source`.
    fn slider_id(source: &str, value: &str) -> String {
        let from = source.find(value).expect("the slider's value");
        slider_widget_id(from..from + value.len())
    }

    /// The id of the only widget of `widget_type`.
    fn widget_id(result: &TranspileOutput, widget_type: &str) -> String {
        let mut found = result
            .widgets
            .iter()
            .filter(|widget| widget.widget_type == widget_type);
        let widget = found.next().expect("a widget of that type");
        assert!(found.next().is_none(), "{:?}", result.widgets);
        widget.id.clone()
    }

    #[test]
    fn visual_widget_catalog_covers_global_inline_alias_and_style_calls() {
        let source = VISUAL_WIDGET_METHODS
            .iter()
            .map(|method| format!("p.{method}({{ test: true }})"))
            .collect::<Vec<_>>()
            .join(";\n");
        let result = transpile_for_layout(&source);

        assert_eq!(result.widgets.len(), VISUAL_WIDGET_METHODS.len());
        assert_eq!(
            result
                .widgets
                .iter()
                .map(|widget| widget.widget_type.as_str())
                .collect::<Vec<_>>(),
            VISUAL_WIDGET_METHODS
        );
        assert!(
            result
                .widgets
                .iter()
                .all(|widget| widget.options.as_deref() == Some("{ test: true }"))
        );
        let ids = result
            .widgets
            .iter()
            .map(|widget| widget.id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids.len(), VISUAL_WIDGET_METHODS.len());
    }

    #[test]
    fn visual_options_are_raw_and_nested_visual_calls_are_not_skipped() {
        let source = r#"stack(
  note("c")._pianoroll({ labels: 1, text: ")" }),
  note("d").scope({ trigger: (() => 0)(), /* a ) in a comment */ color: "cyan" })
).punchcard()
.markcss('text-decoration:underline')"#;
        let result = transpile_for_layout(source);

        assert_eq!(
            result
                .widgets
                .iter()
                .map(|widget| (widget.widget_type.as_str(), widget.options.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("_pianoroll", Some("{ labels: 1, text: \")\" }")),
                (
                    "scope",
                    Some("{ trigger: (() => 0)(), /* a ) in a comment */ color: \"cyan\" }")
                ),
                ("punchcard", Some("")),
                ("markcss", Some("'text-decoration:underline'")),
            ]
        );
        for widget in &result.widgets {
            assert!(widget.from < widget.to);
            assert!(widget.to <= source.len());
            assert!(source[widget.from..widget.to].contains(&widget.widget_type));
        }
        assert_eq!(result.widgets[0].visual_slot, Some(0));
        assert_eq!(result.widgets[1].visual_slot, Some(1));
        assert_eq!(result.widgets[2].visual_slot, Some(2));
        assert_eq!(result.widgets[3].visual_slot, None);
        // Each call carries its own id and, for painters, its slot.
        let id = |index: usize| &result.widgets[index].id;
        assert_eq!(
            result.output,
            format!(
                "stack(note(m('c', 14))._pianoroll('{}', 0, {{\n\tlabels: 1,\n\ttext: m(')', 49)\n}}), \
             note(m('d', 64)).scope('{}', 1, {{\n\ttrigger: (() => 0)(),\n\t\
             /* a ) in a comment */ color: m('cyan', 129)\n}})).punchcard('{}', 2)\
             .markcss('{}', 'text-decoration:underline');",
                id(0),
                id(1),
                id(2),
                id(3),
            )
        );
    }

    #[test]
    fn widget_receiver_ranges_survive_quoted_spaces_and_chain_continuations() {
        let quoted = "s(\"anvil\").note(\"a2 c3\").seg(8)._pitchwheel()";
        let result = transpile(
            quoted,
            &TranspileOptions {
                add_return: false,
                widget_methods: vec!["_pitchwheel".to_owned()],
                ..TranspileOptions::default()
            },
        );
        assert_eq!(result.widgets.len(), 1, "{:?}", result.widgets);
        let widget = &result.widgets[0];
        // The receiver is the whole chain, including the quoted space in
        // `"a2 c3"` and the nested arguments.
        assert_eq!(
            &quoted[widget.from..widget.to],
            "s(\"anvil\").note(\"a2 c3\").seg(8)._pitchwheel()"
        );

        let multiline = "s(\"anvil\")\n.note(\"a2 c3\")\n._pitchwheel()";
        let result = transpile(
            multiline,
            &TranspileOptions {
                add_return: false,
                widget_methods: vec!["_pitchwheel".to_owned()],
                ..TranspileOptions::default()
            },
        );
        assert_eq!(result.widgets.len(), 1, "{:?}", result.widgets);
        let widget = &result.widgets[0];
        assert_eq!(
            &multiline[widget.from..widget.to],
            "s(\"anvil\")\n.note(\"a2 c3\")\n._pitchwheel()"
        );
    }

    #[test]
    fn widget_receiver_ranges_skip_comments_and_stop_at_siblings() {
        let options = TranspileOptions {
            add_return: false,
            widget_methods: vec!["_pitchwheel".to_owned()],
            ..TranspileOptions::default()
        };
        let commented = "s(\"anvil\")\n// keep going\n._pitchwheel()";
        let result = transpile(commented, &options);
        assert_eq!(result.widgets.len(), 1, "{:?}", result.widgets);
        assert_eq!(
            &commented[result.widgets[0].from..result.widgets[0].to],
            commented
        );

        let block = "s(\"anvil\")/* x */._pitchwheel()";
        let result = transpile(block, &options);
        assert_eq!(result.widgets.len(), 1, "{:?}", result.widgets);
        assert_eq!(&block[result.widgets[0].from..result.widgets[0].to], block);

        let sibling = "foo(), bar._pitchwheel()";
        let result = transpile(sibling, &options);
        assert_eq!(result.widgets.len(), 1, "{:?}", result.widgets);
        assert_eq!(
            &sibling[result.widgets[0].from..result.widgets[0].to],
            "bar._pitchwheel()"
        );

        let added = "foo() + bar._pitchwheel()";
        let result = transpile(added, &options);
        assert_eq!(result.widgets.len(), 1, "{:?}", result.widgets);
        assert_eq!(
            &added[result.widgets[0].from..result.widgets[0].to],
            "bar._pitchwheel()"
        );

        let regex = "s(\"anvil\").replace(/a b/, \"x\")._pitchwheel()";
        let result = transpile(regex, &options);
        assert_eq!(result.widgets.len(), 1, "{:?}", result.widgets);
        assert_eq!(&regex[result.widgets[0].from..result.widgets[0].to], regex);
    }

    #[test]
    fn nested_visual_calls_receive_stable_receiver_slots() {
        let source = r#"stack(
  s("bd hh sd hh")._spiral(),
  note("[d2!2 f3 g3]*4")._punchcard()._scope()
)"#;
        let result = transpile_for_layout(source);

        assert_eq!(
            result
                .widgets
                .iter()
                .map(|widget| (widget.widget_type.as_str(), widget.visual_slot))
                .collect::<Vec<_>>(),
            vec![
                ("_spiral", Some(0)),
                ("_punchcard", Some(1)),
                ("_scope", Some(2))
            ]
        );
        assert!(result.output.contains("._spiral('"));
        assert!(result.output.contains("', 0)"));
        assert!(result.output.contains("._punchcard('"));
        assert!(result.output.contains("', 1)"));
        assert!(result.output.contains("._scope('"));
        assert!(result.output.contains("', 2)"));
    }

    #[test]
    fn visual_discovery_ignores_strings_comments_and_non_member_calls() {
        let source = r#"const text = "x._scope()";
// p._spiral()
/* p.pianoroll() */
scope();
p.notScope();
p._scope({ text: "/* not a comment */" });"#;
        let result = transpile_for_layout(source);

        assert_eq!(result.widgets.len(), 1);
        assert_eq!(result.widgets[0].widget_type, "_scope");
        assert_eq!(
            result.widgets[0].options.as_deref(),
            Some("{ text: \"/* not a comment */\" }")
        );
    }

    #[test]
    fn all_with_a_painter_call_carries_its_options() {
        let source = "$: s(\"bd\")\nall(pianoroll({ labels: 1, fold: 0 }))";
        let result = transpile_for_layout(source);
        let widget = result
            .widgets
            .iter()
            .find(|widget| widget.widget_type == "all:pianoroll")
            .expect("the all(pianoroll(..)) record");
        assert_eq!(widget.options.as_deref(), Some("{ labels: 1, fold: 0 }"));
        assert_eq!(
            &source[widget.from..widget.to],
            "all(pianoroll({ labels: 1, fold: 0 }))"
        );
        // Left as a call for the runtime's page-level `pianoroll` to answer
        // (pretty-printed, never rewritten into a method).
        assert!(
            result.output.contains("all(pianoroll({") && result.output.contains("labels: 1"),
            "{}",
            result.output
        );
    }

    #[test]
    fn visual_discovery_includes_all_transform_without_rewriting_it() {
        let source = "all(pianoroll); p.pianoroll(); all(notAVisual)";
        let result = transpile_for_layout(source);

        assert_eq!(result.widgets.len(), 2);
        assert_eq!(result.widgets[0].widget_type, "all:pianoroll");
        assert_eq!(result.widgets[0].visual_slot, None);
        assert_eq!(result.widgets[0].from, 0);
        assert_eq!(result.widgets[0].to, "all(pianoroll)".len());
        assert_eq!(result.widgets[1].widget_type, "pianoroll");
        assert_eq!(result.widgets[1].visual_slot, Some(0));
        assert!(result.output.contains("all(pianoroll)"));
        assert!(
            result
                .output
                .contains(".pianoroll('layout_widget_pianoroll_0_")
        );
        assert!(result.output.contains("all(notAVisual)"));
    }

    #[test]
    fn a_painter_call_inside_all_never_consumes_a_later_widget_id() {
        let source = "all(pianoroll({ labels: 1 }));\ns(\"bd\").pianoroll();";
        let result = transpile_for_layout(source);

        // The member call carries its id; the all() call is left as written.
        assert_eq!(
            result.output,
            "all(pianoroll({ labels: 1 }));\n\
         s(m('bd', 33)).pianoroll('layout_widget_pianoroll_0_31-50', 0);"
        );
    }

    #[test]
    fn a_slider_inside_an_all_painter_body_keeps_its_own_text() {
        let source = "slider(0.5);\nall(pianoroll({ speed: slider(0.2) }));\nslider(0.7);";
        let result = transpile_for_layout(source);

        // The slider in the all() body has no record and is left as written;
        // the sliders around it keep their own ids.
        assert_eq!(
            result.output,
            format!(
                "sliderWithID('{}', 0.5);\nall(pianoroll({{ speed: slider(0.2) }}));\n\
             sliderWithID('{}', 0.7);",
                slider_id(source, "0.5"),
                slider_id(source, "0.7"),
            )
        );
        assert_eq!(
            result
                .widgets
                .iter()
                .map(|widget| widget.widget_type.as_str())
                .collect::<Vec<_>>(),
            ["slider", "all:pianoroll", "slider"]
        );
    }

    #[test]
    fn a_slider_nested_in_a_member_visual_body_never_steals_the_next_sliders_id() {
        let source = "s(\"bd\").pianoroll({ speed: slider(0.2) });\nslider(0.7);";
        let result = transpile_for_layout(source);

        // The nested slider has no record and is left as written; the member
        // call and the later slider keep their own ids.
        assert_eq!(
            result.output,
            format!(
                "s(m('bd', 2)).pianoroll('{}', 0, {{ speed: slider(0.2) }});\n\
             sliderWithID('{}', 0.7);",
                widget_id(&result, "pianoroll"),
                slider_id(source, "0.7"),
            )
        );
        assert_eq!(result.widgets.len(), 2, "{:?}", result.widgets);
    }

    #[test]
    fn a_slider_in_a_kabelsalat_graph_is_not_advertised_as_a_control() {
        let source = "K(seq(slider(0.5)));\nslider(0.9);";
        let result = transpile_for_layout(source);

        // A slider in a K graph becomes worklet source, so it gets no record
        // and no control.
        assert_eq!(
            result.output,
            format!(
                "worklet('seq(slider(.5))');\nsliderWithID('{}', 0.9);",
                slider_id(source, "0.9")
            )
        );
        assert_eq!(result.widgets.len(), 1, "{:?}", result.widgets);
    }

    #[test]
    fn the_arguments_beside_a_kabelsalat_graph_stay_live_widgets() {
        // Arguments after a K graph stay code, so a slider there is recorded.
        let source = "K(seq(slider(0.5)), slider(0.3));";
        let result = transpile_for_layout(source);
        assert_eq!(
            result.output,
            format!(
                "worklet('seq(slider(.5))', sliderWithID('{}', 0.3));",
                slider_id(source, "0.3")
            )
        );
        assert_eq!(result.widgets.len(), 1, "{:?}", result.widgets);
    }

    #[test]
    fn a_bare_page_level_painter_call_never_consumes_a_member_records_id() {
        let source = "pianoroll({ labels: 1 });\ns(\"bd\").pianoroll();";
        let result = transpile_for_layout(source);

        // The bare painter call has no record and keeps its options; the
        // member call carries its id.
        assert_eq!(
            result.output,
            format!(
                "pianoroll({{ labels: 1 }});\ns(m('bd', 28)).pianoroll('{}', 0);",
                widget_id(&result, "pianoroll")
            )
        );
    }

    #[test]
    fn a_slider_with_space_before_its_paren_keeps_its_id_and_its_lines() {
        let source = "slider (0.5);\nslider\n(0.7);";
        let result = transpile_for_layout(source);

        assert_eq!(result.widgets.len(), 2, "{:?}", result.widgets);
        assert_eq!(
            result.output,
            format!(
                "sliderWithID('{}', 0.5);\nsliderWithID('{}', 0.7);",
                slider_id(source, "0.5"),
                slider_id(source, "0.7"),
            )
        );

        // The line break between the name and its paren survives, so a later
        // error is reported on its own line.
        let source = "slider\n(0.7);\n)";
        let result = transpile_for_layout(source);
        let offset = result.diagnostics[0].offset.expect("an offset");
        assert_eq!(
            source[..offset].matches('\n').count(),
            2,
            "{:?}",
            result.diagnostics
        );
    }

    #[test]
    fn nested_same_name_painter_calls_each_carry_their_own_id_and_slot() {
        let source = "stack(s(\"bd\").pianoroll()).pianoroll();";
        let result = transpile_for_layout(source);

        assert_eq!(
            result.output,
            "stack(s(m('bd', 8)).pianoroll('layout_widget_pianoroll_0_6-25', 0))\
         .pianoroll('layout_widget_pianoroll_1_0-38', 1);"
        );
    }

    #[test]
    fn a_paren_in_a_comment_before_a_painter_call_never_costs_it_its_id() {
        // A bracket in a comment inside the receiver leaves each id on its
        // own call.
        let source = "(s(\"bd\") /* ( // */).pianoroll();\nslider(0.5);";
        let result = transpile_for_layout(source);

        assert_eq!(
            result.output,
            format!(
                "s(m('bd', 3)).pianoroll('{}', 0);\nsliderWithID('{}', 0.5);",
                widget_id(&result, "pianoroll"),
                slider_id(source, "0.5"),
            )
        );
    }

    #[test]
    fn native_score_mode_rejects_module_capabilities_by_ast_node() {
        let score = TranspileOptions {
            add_return: false,
            allow_module_syntax: false,
            ..TranspileOptions::default()
        };
        for (source, expected) in [
            ("import value from './module.js';", "module declarations"),
            (
                "export { value } from './module.js';",
                "module declarations",
            ),
            ("const value = import('./module.js');", "dynamic import"),
            ("const value = import.meta.url;", "import.meta"),
        ] {
            let output = transpile(source, &score);
            assert!(
                output
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.message.contains(expected)),
                "{source:?} was accepted: {output:?}"
            );
        }

        for source in [
            "const text = 'import(\\\"file:///tmp/nope\\\")';",
            "// import './module.js'\nconst importName = 1;",
            "const object = { import: 1 }; object.import;",
        ] {
            let output = transpile(source, &score);
            assert!(
                output.diagnostics.is_empty(),
                "non-module source was rejected: {source:?}: {:?}",
                output.diagnostics
            );
        }

        assert!(
            transpile(
                "export const value = 1;",
                &TranspileOptions {
                    add_return: false,
                    ..TranspileOptions::default()
                }
            )
            .diagnostics
            .is_empty(),
            "standalone transpiler callers retain module support"
        );
    }

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
                decode_js_string(escaped, false).expect("valid JavaScript escape"),
                decoded,
                "{escaped:?} did not decode as JavaScript reads it"
            );
        }

        // A backslash before a line terminator is a continuation: it and the
        // break contribute nothing.
        assert_eq!(decode_js_string("a\\\nb", false).unwrap(), "ab");

        // A raw line break is a syntax error in '' and "", but a template
        // literal keeps it, and real scores lay long phrases out over several
        // lines.
        assert!(decode_js_string("c3 e3\ng3 b3", false).is_err());
        assert_eq!(
            decode_js_string("c3 e3\ng3 b3", true).unwrap(),
            "c3 e3\ng3 b3"
        );
        // JavaScript normalises CRLF and lone CR to LF in a template literal.
        assert_eq!(decode_js_string("a\r\nb", true).unwrap(), "a\nb");
        assert_eq!(decode_js_string("a\rb", true).unwrap(), "a\nb");
        assert_eq!(decode_js_string("a\\\r\nb", false).unwrap(), "ab");
    }

    #[test]
    fn malformed_or_unrepresentable_mini_escapes_are_diagnostics() {
        for source in [
            r#"s("a\x6Zb")"#,
            r#"s("a\u12ZZb")"#,
            r#"s("a\u{}b")"#,
            r#"s("a\u{110000}b")"#,
            r#"s("a\ud800b")"#,
            r#"s("a\01b")"#,
        ] {
            let result = transpile(source, &TranspileOptions::default());
            assert!(
                !result.diagnostics.is_empty(),
                "{source:?} must not be silently repaired"
            );
            assert!(
                !result.output.contains("m("),
                "{source:?} was rewritten despite its invalid escape"
            );
        }
    }

    #[test]
    fn awaits_in_code_asks_the_parser_not_the_bytes() {
        // True only where a score really awaits.
        for yes in [
            "await 0",
            "await samples('x')",
            "s(`${await Promise.resolve(\"bd\")}`)",
            "const x = (await p) + 1",
            "async function f() { await p }",
            // A `for await` is a ForOfStatement with a flag, not an
            // AwaitExpression.
            "for await (const x of xs) { f(x) }",
            // The production spelling: the transpiler inserts this itself.
            &transpile("samples('x')", &TranspileOptions::default()).output,
        ] {
            assert!(awaits_in_code(yes), "should await: {yes}");
        }
        // Every one of these contains the word and none of them awaits. A
        // byte scanner answered true for the last three, which bought the
        // job-pumping evaluation path for a score that never awaits.
        for no in [
            "s(\"bd\")",
            "// await the drop",
            "/* await */",
            "s(\"await\")",
            "const awaiting = 1",
            "/await/.test(\"x\")",
            "const o = { await: 1 }",
            "o.await",
            "`plain await text`",
            "for (const x of xs) { f(x) }",
        ] {
            assert!(!awaits_in_code(no), "should not await: {no}");
        }
    }

    #[test]
    fn awaits_in_code_assumes_the_worst_of_what_it_cannot_parse() {
        // Unparseable input cannot be judged, and guessing "no" would send a
        // score that does await to a wrapper that cannot compile it.
        assert!(awaits_in_code("await ((((("));
        // But nonsense without the word is still not an await.
        assert!(!awaits_in_code("((((("));
    }

    #[test]
    fn public_reparses_accept_a_bounded_deep_operator_chain() {
        let source = format!("{}await 0", "!".repeat(MAX_SOURCE_NESTING - 1));
        assert!(awaits_in_code(&source));
        assert_eq!(add_final_return(&source), format!("return {source}"));
        assert_eq!(
            add_final_return_or_undefined(&source),
            format!("return {source}")
        );
        assert_eq!(add_final_return_or_undefined("// no score"), "// no score");
    }

    #[test]
    fn a_trailing_statement_does_not_produce_invalid_javascript() {
        for source in [
            r#"s("bd"); if (true) { s("sd") }"#,
            r#"try { s("bd") } catch (error) {}"#,
            r#"for (const x of [1]) { s("bd") }"#,
            r#"while (false) { s("bd") }"#,
            r#"do { s("bd") } while (false)"#,
            r#"switch (1) { default: s("bd") }"#,
            r#"{ s("bd") }"#,
            r#"const x = 1; if (x) { s("bd") }"#,
            r#"async function score() { return s("bd") }"#,
            r#"label: s("bd")"#,
        ] {
            let with_return = add_final_return(source);
            assert!(
                validate_es2022(&format!("(function(){{\n{with_return}\n}})")).is_ok(),
                "{source:?} produced invalid JavaScript: {with_return:?}"
            );
            assert!(
                with_return.contains("return silence;"),
                "{source:?} should fall back to silence: {with_return:?}"
            );
        }
    }

    #[test]
    fn an_expression_that_starts_like_a_keyword_still_returns() {
        for source in [
            r#"iffy(1)"#,
            r#"forward("bd")"#,
            r#"classic.s("bd")"#,
            r#"letter"#,
            r#"s("bd")"#,
            r#"const x = s("bd"); x"#,
        ] {
            let with_return = add_final_return(source);
            assert!(
                !with_return.contains("return silence;"),
                "{source:?} was wrongly treated as a statement: {with_return:?}"
            );
            assert!(
                validate_es2022(&format!("(function(){{\n{with_return}\n}})")).is_ok(),
                "{source:?} produced invalid JavaScript: {with_return:?}"
            );
        }
    }
}
