//! Native mini-notation parser and pattern compiler.
//!
//! The hand-written recursive-descent parser supports:
//!
//! ```text
//! sequence/group/stack/feet, polymeter, slow alternation, weighted steps,
//! replicate, patterned fast/slow, Euclid, deterministic choice/degrade,
//! nested lists, ranges, rests, source locations, and ^ step metadata.
//! ```
//!
//! Native semantic nodes carry byte spans. Krill's private object graph is
//! more selective: only AtomStub and ElementStub own `location_`; PatternStub
//! and postfix-operation records do not. The raw spans retained below mirror
//! that ownership while [`leaf_locations`] exposes the editor-highlight view.

use rustel_core::register::default_registry;
use rustel_core::{
    Pattern, QueryLimit, Value, choose_cycles, fastcat, pure, query_error_pattern,
    query_limit_pattern, silence, stack,
};
use rustel_fraction::Fraction;

mod krill_unicode_generated;
mod rhythm;

pub use rhythm::Room;
use rhythm::{Planned, Rhythm};

#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone)]
pub enum Ast {
    Atom {
        value: String,
        /// Exact krill AtomStub `location_`, including the whitespace owned by
        /// PEG's `step = ws chars ws` production.
        private_span: Span,
        span: Span,
    },
    Silence {
        span: Span,
    },
    /// A sequence of steps, each with a weight (`@n`) and replication (`!n`).
    Seq {
        items: Vec<Step>,
        steps_source: bool,
        span: Span,
    },
    /// A pattern transformed by the `!n` replicate op - built as
    /// `pat.repeatCycles(n).fast(n)`, fractional `n` included; cloning AST
    /// nodes an integer number of times would make `a!2.5` repeat five times.
    Replicated {
        pat: Box<Ast>,
        /// Modifier operands stay f64 in the AST; Fraction conversion
        /// happens at build time.
        amount: f64,
        span: Span,
    },
    /// `<a b, c d>` - one slow sequence per parallel lane.
    Alt {
        lanes: Vec<AltLane>,
        span: Span,
    },
    /// `a, b` - parallel layers.
    Stack {
        items: Vec<Ast>,
        span: Span,
    },
    /// `{a b, c d e}%n`
    Polymeter {
        items: Vec<Ast>,
        steps_per_cycle: Option<Box<Ast>>,
        span: Span,
    },
    /// `a | b` - deterministic random choice per cycle.
    Choose {
        items: Vec<Ast>,
        seed: u32,
        span: Span,
    },
    /// `a . b c` - foot-separated subsequences.
    Feet {
        items: Vec<Ast>,
        /// krill consumes and exposes a seed for every feet group even though
        /// patternifyAST does not use it. Retain it because `mini2ast` does.
        seed: u32,
        span: Span,
    },
    Fast {
        pat: Box<Ast>,
        factor: Box<Ast>,
        span: Span,
    },
    Slow {
        pat: Box<Ast>,
        factor: Box<Ast>,
        span: Span,
    },
    Euclid {
        pat: Box<Ast>,
        /// Krill retains the complete `slice_with_ops`, including `@`, even
        /// though `patternifyAST` enters only the slice's source and ignores
        /// every postfix option when the slice is a Euclid argument. `Step`
        /// keeps the syntax tree and source paired for `mini2ast`; public
        /// evaluation and `getLeafLocations` both enter only the base source,
        /// through [`euclid_argument_source`].
        pulses: Box<Step>,
        steps: Box<Step>,
        rotation: Option<Box<Step>>,
        span: Span,
    },
    Degrade {
        pat: Box<Ast>,
        /// Bare `?` is `null` in the private operator record (PEG optional
        /// expressions return null) and becomes 0.5 only when patternified.
        amount: Option<f64>,
        seed: u32,
        span: Span,
    },
    Tail {
        pat: Box<Ast>,
        element: Box<Ast>,
        span: Span,
    },
    Range {
        start: Box<Ast>,
        end: Box<Ast>,
        span: Span,
    },
}

#[derive(Debug, Clone)]
pub struct Step {
    pub ast: Ast,
    /// Exact krill ElementStub `location_` for this slice-with-options.
    pub private_span: Span,
    /// `a@3` - relative width within the sequence. Default 1.
    /// Kept as the exact JavaScript Number from krill; checked conversion to
    /// the bounded native Fraction happens only when building the Pattern.
    pub weight: f64,
    /// `a!3` - how many times the step repeats. Default 1.
    pub replicate: usize,
}

#[derive(Debug, Clone)]
pub struct AltLane {
    pub items: Vec<Step>,
    /// A leading `^` belongs to this lane's underlying fastcat and feeds
    /// the parallel `_steps` aggregation.
    pub steps_source: bool,
}

impl Ast {
    pub fn span(&self) -> &Span {
        match self {
            Ast::Atom { span, .. }
            | Ast::Silence { span }
            | Ast::Seq { span, .. }
            | Ast::Replicated { span, .. }
            | Ast::Alt { span, .. }
            | Ast::Stack { span, .. }
            | Ast::Polymeter { span, .. }
            | Ast::Choose { span, .. }
            | Ast::Feet { span, .. }
            | Ast::Fast { span, .. }
            | Ast::Slow { span, .. }
            | Ast::Euclid { span, .. }
            | Ast::Degrade { span, .. }
            | Ast::Tail { span, .. }
            | Ast::Range { span, .. } => span,
        }
    }
}

#[derive(Debug)]
pub struct ParseError {
    pub message: String,
    pub offset: usize,
    /// One-based source line, matching pinned `mini2ast`'s public wrapper.
    pub line: usize,
    pub phase: ErrorPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorPhase {
    Parse,
    Construct,
    /// Native Fraction storage is deliberately bounded to i128. This phase
    /// identifies a resource limit rather than a syntax or construction error.
    NativeLimit,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.phase {
            ErrorPhase::Parse => {
                write!(
                    f,
                    "[mini] parse error at line {}: {}",
                    self.line, self.message
                )
            }
            ErrorPhase::Construct | ErrorPhase::NativeLimit => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for ParseError {}

fn fraction_conversion_error(value: f64) -> String {
    if value.is_infinite() {
        return "The number Infinity cannot be converted to a BigInt because it is not an integer"
            .into();
    }
    // Fraction.js has arbitrary-precision integers; the native Fraction has
    // an intentional i128 ceiling. Do not pretend this native resource bound
    // is a pinned JavaScript parse error.
    format!("number {value} exceeds the native Fraction range")
}

/// How deeply `[`, `<`, `{` and `(` may nest before the parser refuses.
///
/// Counted in atoms, which includes the innermost one: `[[bd]]` is three
/// levels. The bound protects recursive parsing on a 2 MiB thread stack.
pub const MAX_MINI_DEPTH: u32 = 64;

/// Maximum syntax-tree depth, including postfix operators. This also bounds
/// recursive validation, construction, and destruction.
pub const MAX_MINI_AST_DEPTH: u32 = 128;

/// What a parse makes of each construct that it reads. The parser owns the
/// grammar. A builder owns what a construct becomes: the syntax tree, or
/// the plan of a rhythm.
pub(crate) trait Build<'a>: Sized {
    type Node;
    type Step;
    type Lane;

    /// A word, with the span of its element and the span of its leaf.
    fn atom(&mut self, word: &'a str, private_span: Span, span: Span) -> Self::Node;
    /// A slice with its postfix operators, at its weight.
    fn step(&mut self, node: Self::Node, private_span: Span, weight: f64) -> Self::Step;
    fn sequence(
        &mut self,
        steps: impl ExactSizeIterator<Item = Self::Step>,
        steps_source: bool,
        span: Span,
    ) -> Self::Node;
    /// One sequence of an alternation.
    fn lane(
        &mut self,
        steps: impl ExactSizeIterator<Item = Self::Step>,
        steps_source: bool,
    ) -> Self::Lane;
    fn alternation(
        &mut self,
        lanes: impl ExactSizeIterator<Item = Self::Lane>,
        span: Span,
    ) -> Self::Node;
    fn stack(&mut self, items: impl ExactSizeIterator<Item = Self::Node>, span: Span)
    -> Self::Node;
    fn choice(
        &mut self,
        items: impl ExactSizeIterator<Item = Self::Node>,
        seed: u32,
        span: Span,
    ) -> Self::Node;
    fn feet(
        &mut self,
        items: impl ExactSizeIterator<Item = Self::Node>,
        seed: u32,
        span: Span,
    ) -> Self::Node;
    fn polymeter(
        &mut self,
        items: impl ExactSizeIterator<Item = Self::Node>,
        steps_per_cycle: Option<Self::Node>,
        span: Span,
    ) -> Self::Node;
    /// `pattern` under one postfix operator.
    fn postfix(
        &mut self,
        pattern: Self::Node,
        operator: Postfix<Self::Node, Self::Step>,
        span: Span,
    ) -> Self::Node;

    /// Borrow or allocate the parser's open-step stack.
    fn take_steps(&mut self) -> Vec<Slice<Self::Step>> {
        Vec::with_capacity(16)
    }

    /// Return the cleared open-step stack after parsing.
    fn return_steps(&mut self, _steps: Vec<Slice<Self::Step>>) {}

    /// Borrow or allocate the parser's open-group item stack.
    fn take_items(&mut self) -> Vec<Self::Node> {
        Vec::new()
    }

    /// Return the cleared open-group item stack after parsing.
    fn return_items(&mut self, _items: Vec<Self::Node>) {}

    /// Borrow or allocate the parser's open-alternation lane stack.
    fn take_lanes(&mut self) -> Vec<Self::Lane> {
        Vec::new()
    }

    /// Return the cleared open-alternation lane stack after parsing.
    fn return_lanes(&mut self, _lanes: Vec<Self::Lane>) {}
}

/// One postfix operator of a slice, with its arguments.
pub(crate) enum Postfix<N, S> {
    Fast(N),
    Slow(N),
    /// `!`, with the count that the operators of the slice add up to.
    Replicate(f64),
    Euclid {
        pulses: S,
        steps: S,
        rotation: Option<S>,
    },
    Degrade {
        amount: Option<f64>,
        seed: u32,
    },
    Tail(N),
    Range(N),
}

/// The builder of the syntax tree.
struct Tree;

impl<'a> Build<'a> for Tree {
    type Node = Ast;
    type Step = Step;
    type Lane = AltLane;

    fn atom(&mut self, word: &'a str, private_span: Span, span: Span) -> Ast {
        Ast::Atom {
            value: word.to_owned(),
            private_span,
            span,
        }
    }

    fn step(&mut self, ast: Ast, private_span: Span, weight: f64) -> Step {
        Step {
            ast,
            private_span,
            weight,
            replicate: 1,
        }
    }

    fn sequence(
        &mut self,
        steps: impl ExactSizeIterator<Item = Step>,
        steps_source: bool,
        span: Span,
    ) -> Ast {
        Ast::Seq {
            items: steps.collect(),
            steps_source,
            span,
        }
    }

    fn lane(&mut self, steps: impl ExactSizeIterator<Item = Step>, steps_source: bool) -> AltLane {
        AltLane {
            items: steps.collect(),
            steps_source,
        }
    }

    fn alternation(&mut self, lanes: impl ExactSizeIterator<Item = AltLane>, span: Span) -> Ast {
        Ast::Alt {
            lanes: lanes.collect(),
            span,
        }
    }

    fn stack(&mut self, items: impl ExactSizeIterator<Item = Ast>, span: Span) -> Ast {
        Ast::Stack {
            items: items.collect(),
            span,
        }
    }

    fn choice(&mut self, items: impl ExactSizeIterator<Item = Ast>, seed: u32, span: Span) -> Ast {
        Ast::Choose {
            items: items.collect(),
            seed,
            span,
        }
    }

    fn feet(&mut self, items: impl ExactSizeIterator<Item = Ast>, seed: u32, span: Span) -> Ast {
        Ast::Feet {
            items: items.collect(),
            seed,
            span,
        }
    }

    fn polymeter(
        &mut self,
        items: impl ExactSizeIterator<Item = Ast>,
        steps_per_cycle: Option<Ast>,
        span: Span,
    ) -> Ast {
        Ast::Polymeter {
            items: items.collect(),
            steps_per_cycle: steps_per_cycle.map(Box::new),
            span,
        }
    }

    fn postfix(&mut self, pattern: Ast, operator: Postfix<Ast, Step>, span: Span) -> Ast {
        let pat = Box::new(pattern);
        match operator {
            Postfix::Fast(factor) => Ast::Fast {
                pat,
                factor: Box::new(factor),
                span,
            },
            Postfix::Slow(factor) => Ast::Slow {
                pat,
                factor: Box::new(factor),
                span,
            },
            Postfix::Replicate(amount) => Ast::Replicated { pat, amount, span },
            Postfix::Euclid {
                pulses,
                steps,
                rotation,
            } => Ast::Euclid {
                pat,
                pulses: Box::new(pulses),
                steps: Box::new(steps),
                rotation: rotation.map(Box::new),
                span,
            },
            Postfix::Degrade { amount, seed } => Ast::Degrade {
                pat,
                amount,
                seed,
                span,
            },
            Postfix::Tail(element) => Ast::Tail {
                pat,
                element: Box::new(element),
                span,
            },
            Postfix::Range(end) => Ast::Range {
                start: pat,
                end: Box::new(end),
                span,
            },
        }
    }
}

/// A node as the parse made it: how deep its syntax nests, and where its
/// span begins.
struct Parsed<N> {
    node: N,
    depth: u32,
    start: usize,
}

/// A step of a sequence that is open: how deep its syntax nests, and where
/// its element ends.
pub(crate) struct Slice<S> {
    step: S,
    depth: u32,
    end: usize,
}

/// The steps of a sequence that the parse read: the last of the open
/// steps, from `first` on.
struct Run {
    first: usize,
    steps_source: bool,
    span: Span,
    depth: u32,
}

/// What a pair of brackets holds: one sequence, or sequences with one
/// separator between them.
struct Group<N> {
    items: Items<N>,
    /// How deep the deepest of the sequences nests.
    deepest: u32,
    start: usize,
}

enum Items<N> {
    One(N),
    Stack(usize, Span),
    Choice(usize, u32, Span),
    Feet(usize, u32, Span),
}

struct Parser<'a, B: Build<'a>> {
    text: &'a str,
    src: &'a [u8],
    pos: usize,
    seed: u32,
    /// Current nesting depth, checked at the one point all recursion passes
    /// through.
    depth: u32,
    /// PEG.js retains named expectations from optional branches when a later
    /// failure occurs at the same farthest offset. A bare postfix `!`, `@`,
    /// `_`, or `?` therefore contributes `number` to a missing-close error.
    expected_number_at: Option<usize>,
    /// Once PEG selects stack, choose, or feet it commits to that separator.
    /// Later expectations at the same offset retain only that tail grammar.
    aligned_tail_at: Option<(usize, u8)>,
    /// The steps of each sequence that is open, the innermost last. A
    /// sequence takes its steps from here when it ends, in a list of their
    /// count. No parse goes on after an error, so none tidies this list.
    steps: Vec<Slice<B::Step>>,
    /// Open group items (`stack`, `choice`, `feet`), innermost last.
    items: Vec<B::Node>,
    /// Open alternation lanes (`<a, b>`), innermost last.
    lanes: Vec<B::Lane>,
    builder: B,
}

#[derive(Clone, Copy)]
enum PegExpectation {
    SequenceStart,
    SequenceStartAfterMarker,
    SliceStart,
    StepContinuation,
    Whitespace,
    StatementTail,
    TopTail,
    TopStackTail,
    TopChooseTail,
    TopFeetTail,
    SubcycleTail,
    SubcycleStackTail,
    SubcycleChooseTail,
    SubcycleFeetTail,
    SlowSequenceTail,
    PolymeterTail,
    EuclidComma,
    EuclidClose,
    EuclidRotation,
    EuclidRotationClose,
}

impl PegExpectation {
    fn alternatives(self) -> &'static str {
        match self {
            Self::SequenceStart => {
                r##""<", "[", "^", "{", a letter, a number, "-", "#", ".", "^", "_", or whitespace"##
            }
            Self::SequenceStartAfterMarker | Self::SliceStart => {
                r##""<", "[", "{", a letter, a number, "-", "#", ".", "^", "_", or whitespace"##
            }
            Self::StepContinuation => {
                r##"a letter, a number, "-", "#", ".", "^", "_" or whitespace"##
            }
            Self::Whitespace => "whitespace",
            Self::StatementTail => r##""//", end of input, or whitespace"##,
            Self::TopTail => {
                r##""!", "(", "*", ",", ".", "..", "/", ":", "<", "?", "[", "{", "|", ["'], [@_], a letter, a number, "-", "#", ".", "^", "_", or whitespace"##
            }
            Self::TopStackTail => {
                r##""!", "(", "*", ",", "..", "/", ":", "<", "?", "[", "{", ["'], [@_], a letter, a number, "-", "#", ".", "^", "_", or whitespace"##
            }
            Self::TopChooseTail => {
                r##""!", "(", "*", "..", "/", ":", "<", "?", "[", "{", "|", ["'], [@_], a letter, a number, "-", "#", ".", "^", "_", or whitespace"##
            }
            Self::TopFeetTail => {
                r##""!", "(", "*", ".", "..", "/", ":", "<", "?", "[", "{", ["'], [@_], a letter, a number, "-", "#", ".", "^", "_", or whitespace"##
            }
            Self::SubcycleTail => {
                r##""!", "(", "*", ",", ".", "..", "/", ":", "<", "?", "[", "]", "{", "|", [@_], a letter, a number, "-", "#", ".", "^", "_", or whitespace"##
            }
            Self::SubcycleStackTail => {
                r##""!", "(", "*", ",", "..", "/", ":", "<", "?", "[", "]", "{", [@_], a letter, a number, "-", "#", ".", "^", "_", or whitespace"##
            }
            Self::SubcycleChooseTail => {
                r##""!", "(", "*", "..", "/", ":", "<", "?", "[", "]", "{", "|", [@_], a letter, a number, "-", "#", ".", "^", "_", or whitespace"##
            }
            Self::SubcycleFeetTail => {
                r##""!", "(", "*", ".", "..", "/", ":", "<", "?", "[", "]", "{", [@_], a letter, a number, "-", "#", ".", "^", "_", or whitespace"##
            }
            Self::SlowSequenceTail => {
                r##""!", "(", "*", ",", "..", "/", ":", "<", ">", "?", "[", "{", [@_], a letter, a number, "-", "#", ".", "^", "_", or whitespace"##
            }
            Self::PolymeterTail => {
                r##""!", "(", "*", ",", "..", "/", ":", "<", "?", "[", "{", "}", [@_], a letter, a number, "-", "#", ".", "^", "_", or whitespace"##
            }
            Self::EuclidComma => {
                r##""!", "(", "*", ",", "..", "/", ":", "?", [@_], a letter, a number, "-", "#", ".", "^", "_", or whitespace"##
            }
            Self::EuclidClose => {
                r##""!", "(", ")", "*", ",", "..", "/", ":", "<", "?", "[", "{", [@_], a letter, a number, "-", "#", ".", "^", "_", or whitespace"##
            }
            Self::EuclidRotation => {
                r##"")", "<", "[", "{", a letter, a number, "-", "#", ".", "^", "_", or whitespace"##
            }
            Self::EuclidRotationClose => {
                r##""!", "(", ")", "*", "..", "/", ":", "?", [@_], a letter, a number, "-", "#", ".", "^", "_", or whitespace"##
            }
        }
    }
}

impl<'a, B: Build<'a>> Parser<'a, B> {
    fn new(src: &'a str, mut builder: B) -> Self {
        let steps = builder.take_steps();
        let items = builder.take_items();
        let lanes = builder.take_lanes();
        Parser {
            text: src,
            src: src.as_bytes(),
            pos: 0,
            seed: 0,
            depth: 0,
            expected_number_at: None,
            aligned_tail_at: None,
            steps,
            items,
            lanes,
            builder,
        }
    }

    fn ws(&mut self) {
        while self.pos < self.src.len() {
            match self.src[self.pos] {
                b' ' | b'\n' | b'\r' | b'\t' => self.pos += 1,
                0xC2 if self.src.get(self.pos + 1) == Some(&0xA0) => self.pos += 2,
                _ => break,
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn err<T>(&self, msg: &str) -> Result<T, ParseError> {
        Err(ParseError {
            message: msg.to_string(),
            offset: self.pos,
            line: self.line_at(self.pos),
            phase: ErrorPhase::Parse,
        })
    }

    fn ast_depth_error<T>(&self, depth: u32) -> Result<T, ParseError> {
        Err(ParseError {
            message: format!(
                "[mini] pattern syntax nests {depth} levels deep, above the native limit of {MAX_MINI_AST_DEPTH}"
            ),
            offset: self.pos,
            line: self.line_at(self.pos),
            phase: ErrorPhase::NativeLimit,
        })
    }

    /// `depth`, how deep a construct nests, when it is in the native limit.
    fn within(&self, depth: u32) -> Result<u32, ParseError> {
        if depth > MAX_MINI_AST_DEPTH {
            return self.ast_depth_error(depth);
        }
        Ok(depth)
    }

    fn expected<T>(&self, expectation: PegExpectation) -> Result<T, ParseError> {
        let expectation = match (expectation, self.aligned_tail_at) {
            (PegExpectation::TopTail, Some((at, b','))) if at == self.pos => {
                PegExpectation::TopStackTail
            }
            (PegExpectation::TopTail, Some((at, b'|'))) if at == self.pos => {
                PegExpectation::TopChooseTail
            }
            (PegExpectation::TopTail, Some((at, b'.'))) if at == self.pos => {
                PegExpectation::TopFeetTail
            }
            (PegExpectation::SubcycleTail, Some((at, b','))) if at == self.pos => {
                PegExpectation::SubcycleStackTail
            }
            (PegExpectation::SubcycleTail, Some((at, b'|'))) if at == self.pos => {
                PegExpectation::SubcycleChooseTail
            }
            (PegExpectation::SubcycleTail, Some((at, b'.'))) if at == self.pos => {
                PegExpectation::SubcycleFeetTail
            }
            (expectation, _) => expectation,
        };
        let number_expected = self.expected_number_at == Some(self.pos);
        let mut alternatives = if number_expected
            && matches!(expectation, PegExpectation::EuclidComma)
        {
            r##""!", "(", "*", ",", "..", "/", ":", "?", [@_], number, or whitespace"##.to_owned()
        } else {
            expectation.alternatives().to_owned()
        };
        if number_expected && !matches!(expectation, PegExpectation::EuclidComma) {
            alternatives = alternatives.replace(", or whitespace", ", number, or whitespace");
        }
        self.err(&format!(
            "Expected {alternatives} but {} found.",
            self.peg_found()
        ))
    }

    fn peg_found(&self) -> String {
        let character = std::str::from_utf8(&self.src[self.pos..])
            .ok()
            .and_then(|remaining| remaining.chars().next())
            // The public native parser receives Mini's unquoted body. At EOF,
            // pinned krill is looking at the wrapper's closing double quote.
            .unwrap_or('"');
        match character {
            '"' => "\"\\\"\"".into(),
            '\\' => "\"\\\\\"".into(),
            '\n' => "\"\\n\"".into(),
            '\r' => "\"\\r\"".into(),
            '\t' => "\"\\t\"".into(),
            value if value <= '\u{001f}' || value == '\u{007f}' => {
                format!("\"\\x{:02X}\"", value as u32)
            }
            value => format!("\"{value}\""),
        }
    }

    fn line_at(&self, offset: usize) -> usize {
        self.src[..offset.min(self.src.len())]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count()
            + 1
    }

    /// Top level: `a, b` stacks of sequences.
    fn parse_top(&mut self) -> Result<B::Node, ParseError> {
        let group = self.parse_aligned(None, true)?;
        Ok(self.grouped(group).node)
    }

    /// The node of what a pair of brackets holds.
    fn grouped(&mut self, group: Group<B::Node>) -> Parsed<B::Node> {
        let (node, depth) = match group.items {
            Items::One(node) => (node, group.deepest),
            Items::Stack(first, span) => (
                self.builder.stack(self.items.drain(first..), span),
                group.deepest + 1,
            ),
            Items::Choice(first, seed, span) => (
                self.builder.choice(self.items.drain(first..), seed, span),
                group.deepest + 1,
            ),
            Items::Feet(first, seed, span) => (
                self.builder.feet(self.items.drain(first..), seed, span),
                group.deepest + 1,
            ),
        };
        Parsed {
            node,
            depth,
            start: group.start,
        }
    }

    /// Length of the maximal step-character run at `at`.
    ///
    /// ```pegjs
    /// step_char = unicode_letter / [0-9~] / "-" / "#" / "." / "^" / "_"
    /// step = ws chars:step_char+ ws !{ const s = chars.join("");
    ///                                  return (s === ".") || (s === "_") }
    ///        { return new AtomStub(chars.join("")) }
    /// ```
    #[inline]
    fn step_run_len(&self, at: usize) -> usize {
        let src = self.src;
        let len = src.len();
        let mut end = at;
        while end < len {
            let byte = src[end];
            if byte < 128 {
                if !ASCII_STEP_CHARS[usize::from(byte)] {
                    break;
                }
                end += 1;
            } else if let Some(ch_len) = word_char_len(src, end) {
                end += ch_len;
            } else {
                break;
            }
        }
        end - at
    }

    #[inline(always)]
    fn word_char_len(&self, at: usize) -> Option<usize> {
        word_char_len(self.src, at)
    }

    /// Is the `.` at the current position a FEET separator rather than the
    /// start of an atom?
    ///
    /// `step_char+` is greedy and includes `.`, and the rule rejects the run
    /// only when it is exactly `"."`. So `.4` is the atom `0.4` while a lone
    /// `.` falls through to `dot_tail`. Treating every `.` as a separator - the
    /// obvious reading - turns `".4 1"` into an empty foot followed by `4 1`,
    /// which shifts every hap by half a cycle. For example,
    /// `gain(".4!2 1")` must parse `.4` as an atom.
    fn at_feet_dot(&self) -> bool {
        self.peek() == Some(b'.') && self.step_run_len(self.pos) == 1
    }

    fn parse_aligned(
        &mut self,
        closing: Option<u8>,
        allow_choose_and_feet: bool,
    ) -> Result<Group<B::Node>, ParseError> {
        let start = self.pos;
        // The separators and the closing bracket, with no list of them.
        let separators: &[u8] = match allow_choose_and_feet {
            true => b",|.",
            false => b",",
        };
        let mut terminators = [0; 4];
        terminators[..separators.len()].copy_from_slice(separators);
        let count = match closing {
            Some(closing) => {
                terminators[separators.len()] = closing;
                separators.len() + 1
            }
            None => separators.len(),
        };
        let terminators = &terminators[..count];
        let first = self.parse_sequence(terminators)?;
        self.ws();
        // One sequence is the group, with no list.
        let separated = self.peek().filter(|delimiter| {
            closing != Some(*delimiter)
                && (*delimiter == b',' || allow_choose_and_feet && matches!(delimiter, b'|' | b'.'))
                && (*delimiter != b'.' || self.at_feet_dot())
        });
        let Some(delimiter) = separated else {
            return Ok(Group {
                items: Items::One(first.node),
                deepest: first.depth,
                start,
            });
        };
        let mut deepest = first.depth;
        let first_item = self.items.len();
        self.items.push(first.node);
        while self.peek() == Some(delimiter) {
            // Only a BARE `.` separates feet; `.4` is an atom and `..` is the
            // range postfix.
            if delimiter == b'.' && !self.at_feet_dot() {
                break;
            }
            self.pos += 1;
            let next = self.parse_sequence(terminators)?;
            deepest = deepest.max(next.depth);
            self.items.push(next.node);
            self.ws();
        }
        let span = Span {
            start,
            end: self.pos,
        };
        self.aligned_tail_at = Some((self.pos, delimiter));
        let items = match delimiter {
            b',' => Items::Stack(first_item, span),
            b'|' => {
                let seed = self.seed;
                self.seed += 1;
                Items::Choice(first_item, seed, span)
            }
            b'.' => {
                // krill's `dot_tail` allocates a random seed even though the
                // feet compiler does not use it. Later `?`/`|` nodes observe
                // that global seed, so omitting this increment changes music.
                let seed = self.seed;
                self.seed += 1;
                Items::Feet(first_item, seed, span)
            }
            _ => unreachable!(),
        };
        self.within(deepest + 1)?;
        Ok(Group {
            items,
            deepest,
            start,
        })
    }

    fn parse_sequence(&mut self, terminators: &[u8]) -> Result<Parsed<B::Node>, ParseError> {
        let run = self.parse_run(terminators)?;
        let start = run.span.start;
        let steps = self.steps.drain(run.first..).map(|slice| slice.step);
        Ok(Parsed {
            node: self.builder.sequence(steps, run.steps_source, run.span),
            depth: run.depth,
            start,
        })
    }

    /// The steps of one sequence, which stay the last of the open steps.
    fn parse_run(&mut self, terminators: &[u8]) -> Result<Run, ParseError> {
        let start = self.pos;
        self.ws();
        let steps_source = if self.peek() == Some(b'^') {
            self.pos += 1;
            true
        } else {
            false
        };
        // The steps of this sequence are those from here on.
        let first = self.steps.len();
        let mut deepest = 0;
        loop {
            // If the previous slice's PEG production consumed separator
            // whitespace, its ElementStub ends after it. A numeric postfix
            // does not consume following whitespace, so that whitespace
            // instead belongs to the next ElementStub. The retained end tells
            // us which side owns it without changing semantic parser state.
            let element_start = self.steps[first..]
                .last()
                .map_or(self.pos, |slice| slice.end);
            self.ws();
            match self.peek() {
                None => break,
                // A `.` only ends the sequence when it is a bare feet
                // separator; otherwise it starts an atom such as `.4`.
                Some(b'.')
                    if terminators.contains(&b'.')
                        && (!self.at_feet_dot() || self.steps.len() == first) => {}
                Some(c) if terminators.contains(&c) => break,
                _ => {}
            }
            if !self.can_start_slice() {
                break;
            }
            let slice = self.parse_step_at(element_start)?;
            deepest = deepest.max(slice.depth);
            self.steps.push(slice);
        }
        if self.steps.len() == first {
            return self.expected(if steps_source {
                PegExpectation::SequenceStartAfterMarker
            } else {
                PegExpectation::SequenceStart
            });
        }
        Ok(Run {
            first,
            steps_source,
            span: Span {
                start,
                end: self.pos,
            },
            depth: self.within(deepest + 1)?,
        })
    }

    fn can_start_slice(&self) -> bool {
        match self.peek() {
            Some(b'[' | b'<' | b'{') => true,
            // One character of a word starts a slice.
            Some(_) => self.word_char_len(self.pos).is_some(),
            None => false,
        }
    }

    fn parse_required_atom(&mut self) -> Result<Parsed<B::Node>, ParseError> {
        let whitespace_start = self.pos;
        self.ws();
        if !self.can_start_slice() {
            return self.expected(PegExpectation::SliceStart);
        }
        self.pos = whitespace_start;
        self.parse_atom()
    }

    fn parse_required_step(&mut self) -> Result<Slice<B::Step>, ParseError> {
        // op_bjorklund has an explicit `ws` before each slice_with_ops, so
        // those bytes belong to the operator grammar, not ElementStub.
        self.ws();
        let element_start = self.pos;
        if !self.can_start_slice() {
            return self.expected(PegExpectation::SliceStart);
        }
        self.parse_step_at(element_start)
    }

    /// One sequence element plus its postfix operators.
    ///
    /// Faithful to krill.pegjs, which collects postfix operators into an ops
    /// list and applies them **in order**, with two accumulating counters:
    ///
    /// ```text
    /// op_weight    = ("@"/"_") a:number?
    ///   weight = (weight ?? 1) + (a ?? 2) - 1
    /// op_replicate = "!" a:number?
    ///   reps   = (reps ?? 1) + (a ?? 2) - 1
    ///   weight = reps
    ///   // previous replicate ops are FILTERED OUT: only one ever applies
    /// ```
    ///
    /// So `@2@3` is 4 (not 3), `!3!3` is 5 (not 9), and a bare `@`/`!` adds 1.
    /// Because replicate is an op in the list, operators written *after* `!`
    /// apply to the replicated group: `bd!2/3` is `fastcat(bd,bd).slow(3)`.
    fn parse_step_at(&mut self, element_start: usize) -> Result<Slice<B::Step>, ParseError> {
        let base = self.parse_atom_at(element_start)?;
        let mut element_end = score_whitespace_end(self.src, self.pos);
        let mut weight = 1.0;
        let mut reps = 1.0;

        // First postfix op stays inline on the stack (covers 98% of postfix steps
        // with zero heap allocation); additional ops spill into `extra_ops`.
        let mut first_op: Option<(Postfix<_, _>, u32)> = None;
        let mut extra_ops: Vec<(Postfix<_, _>, u32)> = Vec::new();
        let push_op = |first_op: &mut Option<(Postfix<_, _>, u32)>,
                       extra_ops: &mut Vec<(Postfix<_, _>, u32)>,
                       item: (Postfix<_, _>, u32)| {
            if first_op.is_none() {
                *first_op = Some(item);
            } else {
                extra_ops.push(item);
            }
        };

        loop {
            // krill.pegjs prefixes every postfix operator with `ws`:
            //     op_weight    = ws ("@" / "_") a:number?
            //     op_replicate = ws "!"        a:number?
            // so `a ! ! b`, `a _ b _ _` and `a @ b` are all legal - the
            // operator need not be attached to its atom. Look past whitespace,
            // but only consume it when an operator actually follows, or
            // `a b` would swallow the separator.
            let save = self.pos;
            self.ws();
            if !matches!(
                self.peek(),
                Some(b'*' | b'/' | b'@' | b'_' | b'!' | b'(' | b'?' | b':' | b'.')
            ) {
                self.pos = save;
                break;
            }

            match self.peek() {
                Some(b'*') => {
                    self.pos += 1;
                    let factor = self.parse_required_atom()?;
                    element_end = score_whitespace_end(self.src, self.pos);
                    push_op(
                        &mut first_op,
                        &mut extra_ops,
                        (Postfix::Fast(factor.node), factor.depth),
                    );
                }
                Some(b'/') => {
                    self.pos += 1;
                    let factor = self.parse_required_atom()?;
                    element_end = score_whitespace_end(self.src, self.pos);
                    push_op(
                        &mut first_op,
                        &mut extra_ops,
                        (Postfix::Slow(factor.node), factor.depth),
                    );
                }
                Some(b'@') | Some(b'_') => {
                    self.pos += 1;
                    let a = self.optional_number()?.unwrap_or(2.0);
                    weight = weight + a - 1.0;
                    element_end = self.pos;
                }
                Some(b'!') => {
                    self.pos += 1;
                    let a = self.optional_number()?.unwrap_or(2.0);
                    reps = reps + a - 1.0;
                    weight = reps;
                    // The grammar filters out any earlier replicate op and
                    // pushes a fresh one at the END:
                    //   ops = ops.filter(o => o.type_ !== "replicate");
                    //   ops.push({type_: "replicate", amount: reps});
                    // So a later `!` MOVES the replication after everything
                    // written between them: `bd!1/3!2` slows first, then
                    // replicates the slowed pattern. Pushing once at the first
                    // `!` gets this backwards.
                    if matches!(first_op, Some((Postfix::Replicate(_), _))) {
                        first_op = if extra_ops.is_empty() {
                            None
                        } else {
                            Some(extra_ops.remove(0))
                        };
                    }
                    extra_ops.retain(|(op, _)| !matches!(op, Postfix::Replicate(_)));
                    push_op(&mut first_op, &mut extra_ops, (Postfix::Replicate(reps), 0));
                    element_end = self.pos;
                }
                Some(b'(') => {
                    self.pos += 1;
                    let pulses = self.parse_required_step()?;
                    self.ws();
                    if self.peek() != Some(b',') {
                        return self.expected(PegExpectation::EuclidComma);
                    }
                    self.pos += 1;
                    let steps = self.parse_required_step()?;
                    self.ws();
                    // The tail is `ws comma? ws r:slice_with_ops? ws ")"`, so
                    // the comma and the rotation are independent optionals:
                    // `a(3,8 5)` rotates like `a(3,8,5)`. A refusal lists the
                    // comma only while it has not been consumed.
                    let had_comma = self.peek() == Some(b',');
                    if had_comma {
                        self.pos += 1;
                        self.ws();
                    }
                    let rotation = if self.peek() == Some(b')') {
                        None
                    } else if !self.can_start_slice() {
                        return self.expected(if had_comma {
                            PegExpectation::EuclidRotation
                        } else {
                            PegExpectation::EuclidClose
                        });
                    } else {
                        Some(self.parse_step_at(self.pos)?)
                    };
                    self.ws();
                    if self.peek() != Some(b')') {
                        return self.expected(PegExpectation::EuclidRotationClose);
                    }
                    self.pos += 1;
                    element_end = self.pos;
                    let deepest = rotation.as_ref().map_or(0, |rotation| rotation.depth);
                    let deepest = deepest.max(pulses.depth).max(steps.depth);
                    let euclid = Postfix::Euclid {
                        pulses: pulses.step,
                        steps: steps.step,
                        rotation: rotation.map(|rotation| rotation.step),
                    };
                    push_op(&mut first_op, &mut extra_ops, (euclid, deepest));
                }
                Some(b'?') => {
                    self.pos += 1;
                    // PEG's `number` action returns a JavaScript Number here,
                    // not a Fraction. That distinction is observable for
                    // overflowing exponent syntax: `a?1e999` is legal and
                    // degrades everything, while `a@1e999` reaches
                    // Fraction.js during construction and throws.
                    let amount = self.optional_js_number()?;
                    let seed = self.seed;
                    self.seed += 1;
                    element_end = self.pos;
                    push_op(
                        &mut first_op,
                        &mut extra_ops,
                        (Postfix::Degrade { amount, seed }, 0),
                    );
                }
                Some(b':') => {
                    self.pos += 1;
                    let element = self.parse_required_atom()?;
                    element_end = score_whitespace_end(self.src, self.pos);
                    push_op(
                        &mut first_op,
                        &mut extra_ops,
                        (Postfix::Tail(element.node), element.depth),
                    );
                }
                Some(b'.') if self.src.get(self.pos + 1) == Some(&b'.') => {
                    self.pos += 2;
                    let end = self.parse_required_atom()?;
                    element_end = score_whitespace_end(self.src, self.pos);
                    push_op(
                        &mut first_op,
                        &mut extra_ops,
                        (Postfix::Range(end.node), end.depth),
                    );
                }
                _ => break,
            }

            let op_count = usize::from(first_op.is_some()) + extra_ops.len();
            if op_count > MAX_MINI_AST_DEPTH as usize {
                return self.ast_depth_error(op_count as u32);
            }
        }

        let Parsed {
            mut node,
            mut depth,
            start,
        } = base;
        for (op, arguments) in first_op.into_iter().chain(extra_ops) {
            let span = Span {
                start,
                end: self.pos,
            };
            node = self.builder.postfix(node, op, span);
            depth = self.within(depth.max(arguments) + 1)?;
        }
        let private_span = Span {
            start: element_start,
            end: element_end,
        };
        Ok(Slice {
            step: self.builder.step(node, private_span, weight),
            depth,
            end: element_end,
        })
    }

    /// A number if one follows, else `None` (bare `@` / `!`).
    fn optional_number(&mut self) -> Result<Option<f64>, ParseError> {
        if self.starts_number() {
            Ok(Some(self.parse_js_number()?))
        } else {
            self.expected_number_at = Some(self.pos);
            Ok(None)
        }
    }

    /// A JavaScript-number-valued optional modifier, including infinities.
    fn optional_js_number(&mut self) -> Result<Option<f64>, ParseError> {
        if self.starts_number() {
            Ok(Some(self.parse_js_number()?))
        } else {
            self.expected_number_at = Some(self.pos);
            Ok(None)
        }
    }

    fn starts_number(&self) -> bool {
        self.peek().is_some_and(|c| c.is_ascii_digit())
            || (self.peek() == Some(b'-')
                && self.src.get(self.pos + 1).is_some_and(u8::is_ascii_digit))
    }

    fn parse_atom(&mut self) -> Result<Parsed<B::Node>, ParseError> {
        let private_start = self.pos;
        self.parse_atom_at(private_start)
    }

    /// All recursively nested atoms pass through this boundary.
    fn parse_atom_at(&mut self, private_start: usize) -> Result<Parsed<B::Node>, ParseError> {
        self.depth += 1;
        if self.depth > MAX_MINI_DEPTH {
            self.depth -= 1;
            return Err(ParseError {
                message: format!(
                    "[mini] pattern nests deeper than {MAX_MINI_DEPTH} levels; \
                     this is a native stack bound, not a limit of the pinned parser"
                ),
                offset: self.pos,
                line: self.line_at(self.pos),
                phase: ErrorPhase::NativeLimit,
            });
        }
        let parsed = self.parse_atom_at_bounded(private_start);
        self.depth -= 1;
        parsed
    }

    fn parse_atom_at_bounded(
        &mut self,
        private_start: usize,
    ) -> Result<Parsed<B::Node>, ParseError> {
        let whitespace_start = self.pos;
        self.ws();
        let start = self.pos;
        // A slice used directly as a postfix argument (`*`, `/`, `:`, `..`,
        // `%`) owns its leading `ws` in krill. getLeafLocation removes literal
        // spaces from that span but leaves tabs/newlines visible. Normal
        // sequence whitespace has already been consumed by parse_seq, so
        // whitespace_start == start there and adjacent leaves do not overlap.
        let location_start = whitespace_start
            + self.src[whitespace_start..start]
                .iter()
                .filter(|byte| **byte == b' ')
                .count();
        match self.peek() {
            Some(b'[') => {
                self.pos += 1;
                let inner = self.parse_aligned(Some(b']'), true)?;
                if self.peek() != Some(b']') {
                    return self.expected(PegExpectation::SubcycleTail);
                }
                self.pos += 1;
                Ok(self.grouped(inner))
            }
            Some(b'<') => {
                self.pos += 1;
                let first_lane = self.lanes.len();
                // An alternation nests as its deepest lane does.
                let mut depth = 0;
                loop {
                    let run = self.parse_run(b",>")?;
                    depth = depth.max(run.depth);
                    let steps = self.steps.drain(run.first..).map(|slice| slice.step);
                    let lane = self.builder.lane(steps, run.steps_source);
                    self.lanes.push(lane);
                    self.ws();
                    if self.peek() != Some(b',') {
                        break;
                    }
                    self.pos += 1;
                }
                if self.peek() != Some(b'>') {
                    return self.expected(PegExpectation::SlowSequenceTail);
                }
                self.pos += 1;
                let span = Span {
                    start,
                    end: self.pos,
                };
                Ok(Parsed {
                    node: self
                        .builder
                        .alternation(self.lanes.drain(first_lane..), span),
                    depth,
                    start,
                })
            }
            Some(b'{') => {
                self.pos += 1;
                let inner = self.parse_aligned(Some(b'}'), false)?;
                if self.peek() != Some(b'}') {
                    return self.expected(PegExpectation::PolymeterTail);
                }
                self.pos += 1;
                let steps_per_cycle = if self.peek() == Some(b'%') {
                    self.pos += 1;
                    Some(self.parse_atom()?)
                } else {
                    None
                };
                let deepest = steps_per_cycle
                    .as_ref()
                    .map_or(inner.deepest, |steps| inner.deepest.max(steps.depth));
                let span = Span {
                    start,
                    end: self.pos,
                };
                let steps_per_cycle = steps_per_cycle.map(|steps| steps.node);
                let node = match inner.items {
                    Items::One(node) => {
                        self.builder
                            .polymeter(std::iter::once(node), steps_per_cycle, span)
                    }
                    Items::Stack(first, _)
                    | Items::Choice(first, _, _)
                    | Items::Feet(first, _, _) => {
                        self.builder
                            .polymeter(self.items.drain(first..), steps_per_cycle, span)
                    }
                };
                Ok(Parsed {
                    node,
                    depth: self.within(deepest + 1)?,
                    start,
                })
            }
            Some(_) if self.word_char_len(self.pos).is_some() => {
                while let Some(len) = self.word_char_len(self.pos) {
                    self.pos += len;
                }
                let text = self.text;
                let word = &text[start..self.pos];
                if matches!(word, "." | "_") {
                    // `step` consumes its trailing `ws` before the semantic
                    // predicate rejects the two reserved singleton atoms.
                    let word_end = self.pos;
                    self.ws();
                    return self.expected(if self.pos > word_end {
                        PegExpectation::Whitespace
                    } else {
                        PegExpectation::StepContinuation
                    });
                }
                let private_span = Span {
                    start: private_start,
                    end: score_whitespace_end(self.src, self.pos),
                };
                let span = Span {
                    start: location_start,
                    end: self.pos,
                };
                Ok(Parsed {
                    node: self.builder.atom(word, private_span, span),
                    depth: 1,
                    start: location_start,
                })
            }
            _ => self.err("unexpected character"),
        }
    }

    /// Parse krill's PEG `number` production as JavaScript's `parseFloat`.
    /// An incomplete exponent is not consumed (`1e` is number `1` followed by
    /// atom `e`), matching PEG's optional `exp?` rollback.
    fn parse_js_number(&mut self) -> Result<f64, ParseError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        let integer_start = self.pos;
        if self.peek() == Some(b'0') {
            self.pos += 1;
        } else if self.peek().is_some_and(|c| matches!(c, b'1'..=b'9')) {
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if integer_start == self.pos {
            return self.err("expected number");
        }
        if self.peek() == Some(b'.') && self.src.get(self.pos + 1).is_some_and(u8::is_ascii_digit) {
            self.pos += 1;
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            let exponent_start = self.pos;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            let digits_start = self.pos;
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.pos += 1;
            }
            if digits_start == self.pos {
                self.pos = exponent_start;
            }
        }
        let text =
            std::str::from_utf8(&self.src[start..self.pos]).expect("the text comes from a str");
        text.parse::<f64>().map_err(|_| ParseError {
            message: "bad number".into(),
            offset: start,
            line: self.line_at(start),
            phase: ErrorPhase::Parse,
        })
    }
}

/// Whether `c` can be part of a step's word, as krill's `step_char` reads
/// it: a letter, an ASCII digit, `~`, `-`, `#`, `.`, `^` or `_`. So
/// `mlkr-grsl` and `a.b` are each one word.
pub fn is_step_char(c: char) -> bool {
    match u8::try_from(c).ok().filter(u8::is_ascii) {
        Some(byte) => ASCII_STEP_CHARS[usize::from(byte)],
        None => krill_unicode_generated::is_letter(c),
    }
}

/// How many bytes of `src` at `at` are one character of a step's word.
/// `None` when the byte at `at` begins none.
#[inline(always)]
fn word_char_len(src: &[u8], at: usize) -> Option<usize> {
    let byte = *src.get(at)?;
    if byte < 128 {
        return ASCII_STEP_CHARS[usize::from(byte)].then_some(1);
    }
    let value = std::str::from_utf8(&src[at..]).ok()?.chars().next()?;
    is_step_char(value).then(|| value.len_utf8())
}

/// Whether each byte is an ASCII character that can be part of a step's word.
const ASCII_STEP_CHARS: [bool; 256] = {
    let mut steps = [false; 256];
    let mut byte = 0u8;
    while byte < 128 {
        steps[byte as usize] =
            byte.is_ascii_alphanumeric() || matches!(byte, b'~' | b'-' | b'#' | b'.' | b'^' | b'_');
        byte += 1;
    }
    steps
};

pub fn parse(src: &str) -> Result<Ast, ParseError> {
    read(src, Tree).map(|(ast, _)| ast)
}

/// What `builder` makes of `src`, and the builder after the parse.
pub(crate) fn read<'a, B: Build<'a>>(src: &'a str, builder: B) -> Result<(B::Node, B), ParseError> {
    let mut p = Parser::new(src, builder);
    let result = (|| {
        let node = p.parse_top()?;
        p.ws();
        if p.pos < p.src.len() {
            // krill's opening and closing `quote` productions are independent:
            // either quote kind can close the double-quoted wrapper Mini builds.
            // Preserve that observable (if odd) cross-quote failure position.
            if p.peek() == Some(b'\'') {
                p.pos += 1;
                p.ws();
                return p.expected(PegExpectation::StatementTail);
            }
            return p.expected(PegExpectation::TopTail);
        }
        Ok(node)
    })();
    p.steps.clear();
    p.builder.return_steps(p.steps);
    p.items.clear();
    p.builder.return_items(p.items);
    p.lanes.clear();
    p.builder.return_lanes(p.lanes);
    result.map(|node| (node, p.builder))
}

/// Byte ranges of every mini leaf. This is the native equivalent of pinned
/// `getLeafLocations` and is consumed by the Rust transpiler.
pub fn leaf_locations(src: &str, offset: usize) -> Result<Vec<(usize, usize)>, ParseError> {
    let ast = parse(src)?;
    let mut leaves = Vec::new();
    collect_leaf_spans(&ast, &mut leaves);
    Ok(leaves
        .into_iter()
        .map(|span| {
            let (start, end) = leaf_span(src.as_bytes(), &span);
            (start + offset, end + offset)
        })
        .collect())
}

/// One leaf's whitespace-corrected source span.
///
/// An atom's span includes the whitespace consumed after its token; only
/// literal spaces are then trimmed - tabs, newlines and NBSP stay observable
/// in editor highlights, a quirk kept for compatibility. Shared by
/// `leaf_locations` (the transpiler's view) and the build-time hap context,
/// which attaches the SAME span.
#[inline]
fn leaf_span(src: &[u8], span: &Span) -> (usize, usize) {
    let mut at = span.end;
    let mut non_space_ws = 0usize;
    while at < src.len() {
        match src[at] {
            b' ' => at += 1,
            b'\n' | b'\r' | b'\t' => {
                non_space_ws += 1;
                at += 1;
            }
            0xC2 if src.get(at + 1) == Some(&0xA0) => {
                non_space_ws += 2;
                at += 2;
            }
            _ => break,
        }
    }
    (span.start, span.end + non_space_ws)
}

fn score_whitespace_end(src: &[u8], mut at: usize) -> usize {
    while at < src.len() {
        match src[at] {
            b' ' | b'\n' | b'\r' | b'\t' => at += 1,
            0xC2 if src.get(at + 1) == Some(&0xA0) => at += 2,
            _ => break,
        }
    }
    at
}

fn collect_leaf_spans(ast: &Ast, output: &mut Vec<Span>) {
    match ast {
        Ast::Atom { span, .. } | Ast::Silence { span } => output.push(span.clone()),
        // Every pattern child is entered before any postfix argument. The
        // ordering is observable in `leaf_locations("bd*2 [sd cp]")`: bd, sd,
        // cp, then 2. Walking the nested operator AST naively would report
        // bd, 2, sd, cp instead.
        Ast::Seq { items, .. } => {
            for item in items {
                collect_step_source(&item.ast, output);
            }
            for item in items {
                collect_step_options(&item.ast, output);
            }
        }
        Ast::Alt { lanes, .. } => {
            for lane in lanes {
                for item in &lane.items {
                    collect_step_source(&item.ast, output);
                }
                for item in &lane.items {
                    collect_step_options(&item.ast, output);
                }
            }
        }
        Ast::Stack { items, .. } | Ast::Choose { items, .. } | Ast::Feet { items, .. } => {
            for item in items {
                collect_leaf_spans(item, output);
            }
        }
        Ast::Polymeter {
            items,
            steps_per_cycle,
            ..
        } => {
            for item in items {
                collect_leaf_spans(item, output);
            }
            if let Some(steps_per_cycle) = steps_per_cycle {
                collect_leaf_spans(steps_per_cycle, output);
            }
        }
        Ast::Fast { .. }
        | Ast::Slow { .. }
        | Ast::Replicated { .. }
        | Ast::Euclid { .. }
        | Ast::Degrade { .. }
        | Ast::Tail { .. }
        | Ast::Range { .. } => {
            collect_step_source(ast, output);
            collect_step_options(ast, output);
        }
    }
}

/// Enter the source of one element, without entering its postfix arguments.
fn collect_step_source(ast: &Ast, output: &mut Vec<Span>) {
    match ast {
        Ast::Fast { pat, .. }
        | Ast::Slow { pat, .. }
        | Ast::Euclid { pat, .. }
        | Ast::Degrade { pat, .. }
        | Ast::Tail { pat, .. } => collect_step_source(pat, output),
        Ast::Range { start, .. } => collect_step_source(start, output),
        // `!n` duplicates the pattern graph, not the source token. The PEG AST
        // has one source child and one replicate option, so report its leaves
        // once rather than once per repetition.
        Ast::Replicated { pat, .. } => collect_step_source(pat, output),
        other => collect_leaf_spans(other, output),
    }
}

/// Enter postfix arguments in their source order, after sibling sources.
fn collect_step_options(ast: &Ast, output: &mut Vec<Span>) {
    match ast {
        Ast::Fast { pat, factor, .. } | Ast::Slow { pat, factor, .. } => {
            collect_step_options(pat, output);
            collect_leaf_spans(factor, output);
        }
        Ast::Replicated { pat, .. } => collect_step_options(pat, output),
        Ast::Euclid {
            pat,
            pulses,
            steps,
            rotation,
            ..
        } => {
            collect_step_options(pat, output);
            collect_step_source(&pulses.ast, output);
            collect_step_source(&steps.ast, output);
            if let Some(rotation) = rotation {
                collect_step_source(&rotation.ast, output);
            }
        }
        Ast::Degrade { pat, .. } => collect_step_options(pat, output),
        Ast::Tail { pat, element, .. } => {
            collect_step_options(pat, output);
            collect_leaf_spans(element, output);
        }
        Ast::Range { start, end, .. } => {
            collect_step_options(start, output);
            collect_leaf_spans(end, output);
        }
        _ => {}
    }
}

/// A sequence's steps after `!n` replication, each with its weight.
fn expanded_steps(items: &[Step]) -> impl Iterator<Item = (&Step, Fraction)> {
    items
        .iter()
        .flat_map(|step| std::iter::repeat_n((step, sequence_weight(step.weight)), step.replicate))
}

/// Total weight after `!n` replication: a sequence's steps divide one cycle
/// in this proportion, and an `<>` alternation spans this many cycles.
fn total_weight(items: &[Step]) -> Fraction {
    checked_total_weight(items)
        .expect("Mini weight totals must be validated before pattern construction")
}

/// [`total_weight`], or the first step at which the running sum leaves the
/// native Fraction range.
fn checked_total_weight(items: &[Step]) -> Result<Fraction, &Step> {
    // Steps of the weight one count themselves.
    let count = |total: i128, step: &Step| total.checked_add(i128::try_from(step.replicate).ok()?);
    if evenly_weighted(items)
        && let Some(total) = items.iter().try_fold(0, count)
    {
        return Ok(Fraction::int(total));
    }
    expanded_steps(items).try_fold(Fraction::ZERO, |total, (step, weight)| {
        total.checked_add(weight).ok_or(step)
    })
}

/// The `[begin, end)` window of one cycle that `build_seq` compresses each
/// expanded step into, in proportion to its weight; empty when the weights
/// sum to zero. `Err` is the first step whose running sum or window leaves
/// the native Fraction range.
fn sequence_windows(items: &[Step]) -> Result<Vec<(Fraction, Fraction)>, &Step> {
    let total = checked_total_weight(items)?;
    if total == Fraction::ZERO {
        return Ok(Vec::new());
    }
    let mut at = Fraction::ZERO;
    expanded_steps(items)
        .map(|(step, weight)| {
            let begin = at.checked_div(total).ok_or(step)?;
            at = at.checked_add(weight).ok_or(step)?;
            let end = at.checked_div(total).ok_or(step)?;
            Ok((begin, end))
        })
        .collect()
}

fn native_weight(value: f64) -> Fraction {
    Fraction::from_f64(value).expect("Mini weight must be validated before pattern construction")
}

/// `child.options_?.weight || 1` at sequence assembly. Keep the parser's raw
/// zero until here: a later postfix operator still accumulates from zero, while
/// an ultimately-zero `@0` or `!0` retains a one-unit slot. For `!0` the child
/// pattern itself is silent, but the following step must not shift left.
fn sequence_weight(value: f64) -> Fraction {
    native_weight(if value == 0.0 { 1.0 } else { value })
}

/// What a construct builds: a graph, or the plain values of a [`Rhythm`],
/// which become one node.
enum Built {
    Rhythm(Rhythm),
    Graph(Pattern),
}

impl Built {
    fn into_pattern(self) -> Pattern {
        match self {
            Built::Rhythm(rhythm) => rhythm.into_pattern(),
            Built::Graph(pattern) => pattern,
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Set by a test to build one node for each value, as each construct
    /// that is no [`Rhythm`] builds: the graph that a rhythm must answer as.
    static ONE_NODE_FOR_EACH_VALUE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
thread_local! {
    /// Set by a test to build each construct of whole parts level by level:
    /// the pattern that one walk of the construct must write.
    static LEVEL_BY_LEVEL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether one walk may write a construct of whole parts now.
fn reads_whole_parts() -> bool {
    #[cfg(test)]
    if LEVEL_BY_LEVEL.get() {
        return false;
    }
    rhythms([]).is_some()
}

/// The rhythm of each of `parts`, when each is one and the plain values of
/// a construct may share a node.
fn rhythms<'a>(parts: impl IntoIterator<Item = &'a Built>) -> Option<Vec<&'a Rhythm>> {
    #[cfg(test)]
    if ONE_NODE_FOR_EACH_VALUE.get() {
        return None;
    }
    parts
        .into_iter()
        .map(|part| match part {
            Built::Rhythm(rhythm) => Some(rhythm),
            Built::Graph(_) => None,
        })
        .collect()
}

/// `fastcat` of `parts`, each as many times in a row as its count says.
fn concatenated(parts: Vec<(Built, usize)>) -> Built {
    let count: usize = parts.iter().map(|(_, times)| times).sum();
    let allow_rhythms = {
        #[cfg(test)]
        {
            !ONE_NODE_FOR_EACH_VALUE.get()
        }
        #[cfg(not(test))]
        {
            true
        }
    };
    let rhythm = if count < 2 || !allow_rhythms {
        None
    } else if let Some(first_rhythm) = parts.iter().find_map(|(part, times)| {
        (*times > 0).then_some(part).and_then(|p| match p {
            Built::Rhythm(r) => Some(r),
            Built::Graph(_) => None,
        })
    }) && parts.iter().all(|(p, _)| matches!(p, Built::Rhythm(_)))
    {
        if count <= 32 {
            let mut stack = [first_rhythm; 32];
            let mut len = 0usize;
            for (part, times) in &parts {
                if let Built::Rhythm(r) = part {
                    for _ in 0..*times {
                        stack[len] = r;
                        len += 1;
                    }
                }
            }
            Rhythm::concatenated(&stack[..len])
        } else {
            let mut each = Vec::with_capacity(count);
            for (part, times) in &parts {
                if let Built::Rhythm(r) = part {
                    each.extend(std::iter::repeat_n(r, *times));
                }
            }
            Rhythm::concatenated(&each)
        }
    } else {
        None
    };
    match rhythm {
        Some(rhythm) => Built::Rhythm(rhythm),
        None => Built::Graph(fastcat(
            parts
                .into_iter()
                // A handle of the pattern for each time after the first.
                .flat_map(|(part, times)| std::iter::repeat_n(part.into_pattern(), times))
                .collect(),
        )),
    }
}

/// Whether each step of `items` has the weight one: the steps are then
/// equal parts of a cycle, which are in the native range.
fn evenly_weighted(items: &[Step]) -> bool {
    items.iter().all(|step| step.weight == 1.0)
}

/// `stack` of `parts`.
fn stacked(parts: Vec<Built>) -> Built {
    if rhythms(&parts).is_none() {
        return Built::Graph(stack(parts.into_iter().map(Built::into_pattern).collect()));
    }
    let parts = parts
        .into_iter()
        .filter_map(|part| match part {
            Built::Rhythm(rhythm) => Some(rhythm),
            Built::Graph(_) => None,
        })
        .collect();
    match Rhythm::stacked(parts) {
        Ok(rhythm) => Built::Rhythm(rhythm),
        Err(parts) => Built::Graph(stack(parts.into_iter().map(Rhythm::into_pattern).collect())),
    }
}

/// `built` at `rate` times its speed, with the locations of the factor on
/// each hap. `graph` makes the graph of the same speed change. A rate that
/// is `None` or not above zero has no rhythm.
fn at_rate(
    built: Built,
    rate: Option<Fraction>,
    locations: &[(usize, usize)],
    graph: impl FnOnce(Pattern) -> Pattern,
) -> Built {
    let built = match (built, rate) {
        (Built::Rhythm(rhythm), Some(rate)) if rhythms([]).is_some() => match rhythm.faster(rate) {
            Ok(rhythm) => return Built::Rhythm(rhythm.located(locations)),
            Err(rhythm) => Built::Rhythm(rhythm),
        },
        (built, _) => built,
    };
    Built::Graph(if locations.is_empty() {
        graph(built.into_pattern())
    } else {
        graph(built.into_pattern()).with_added_context_slice(locations)
    })
}

/// `built` with the structure of a Euclidean rhythm, when `built` is plain
/// values and the rhythm is a whole count of pulses in a whole count of
/// steps, in one cycle. A rhythm of more pulses than steps, or of steps
/// that each hold to the next pulse, keeps the nodes of its structure.
fn struck(built: &Built, pulses: i32, steps: f64, rotation: i32) -> Option<Rhythm> {
    let rhythm = rhythms([built])?.pop()?;
    let whole = pulses >= 1 && steps.fract() == 0.0 && f64::from(pulses) <= steps;
    if !whole || steps > f64::from(u16::MAX) {
        return None;
    }
    let mask = rustel_core::euclid::euclid_rot(pulses, steps as usize, rotation as isize).ok()?;
    rhythm.struck(&mask)
}

/// Expand `!n` replication, then divide one cycle among the steps in
/// proportion to their `@n` weights: plain `fastcat` when every weight is 1,
/// `timecat` otherwise.
fn build_seq(items: &[Step]) -> Built {
    // Steps of the weight one, the common sequence: plain `fastcat`, where
    // a step with `!n` is built one time.
    if evenly_weighted(items) {
        let mut steps = items.iter().filter(|step| step.replicate > 0);
        let count: usize = steps.clone().map(|step| step.replicate).sum();
        if count == 0 {
            return Built::Rhythm(Rhythm::rest());
        }
        if count == 1 {
            return compile(&steps.next().expect("one step").ast);
        }
        let allow_rhythms = {
            #[cfg(test)]
            {
                !ONE_NODE_FOR_EACH_VALUE.get()
            }
            #[cfg(not(test))]
            {
                true
            }
        };
        if allow_rhythms
            && count <= 256
            && items
                .iter()
                .filter(|s| s.replicate > 0)
                .all(|s| matches!(s.ast, Ast::Atom { .. } | Ast::Silence { .. }))
            && let Some(count_frac) = i128::try_from(count).ok().map(Fraction::int)
        {
            let mut out = Vec::with_capacity(count);
            let mut idx = 0usize;
            let mut ok = true;
            for step in items.iter().filter(|s| s.replicate > 0) {
                match &step.ast {
                    Ast::Silence { .. } => {
                        idx += step.replicate;
                    }
                    Ast::Atom { value, .. } if value == "-" || value == "~" => {
                        idx += step.replicate;
                    }
                    Ast::Atom { value, span, .. } => {
                        let val = atom_value(value);
                        let mut locs = rustel_core::Locations::new();
                        if let Some(l) = current_leaf_span(span) {
                            locs.push(l);
                        }
                        for _ in 0..step.replicate {
                            let (Some(b), Some(e)) = (
                                Fraction::int(idx as i128).checked_div(count_frac),
                                Fraction::int((idx + 1) as i128).checked_div(count_frac),
                            ) else {
                                ok = false;
                                break;
                            };
                            out.push(rustel_core::SequenceStep::new(
                                b,
                                e,
                                val.clone(),
                                locs.clone(),
                            ));
                            idx += 1;
                        }
                        if !ok {
                            break;
                        }
                    }
                    _ => unreachable!(),
                }
            }
            if ok && let Some(rhythm) = Rhythm::from_single_cycle_steps(out, count) {
                return Built::Rhythm(rhythm);
            }
        }
        return concatenated(
            steps
                .map(|step| (compile(&step.ast), step.replicate))
                .collect(),
        );
    }
    let expanded: Vec<(Fraction, &Ast)> = expanded_steps(items)
        .map(|(step, weight)| (weight, &step.ast))
        .collect();
    if expanded.is_empty() {
        return Built::Rhythm(Rhythm::rest());
    }

    // A single step always fills the whole cycle, so its weight is irrelevant
    // and `compress(0, 1)` is the identity - except that `fastGap` carries a
    // `splitQueries`, which would fragment a multi-cycle query. Short-circuit,
    // mirroring `slowcat`'s `if (pats.length == 1) return pats[0]`. It comes
    // before the zero-sum guard: a single step divides nothing, and pinned
    // `stepcat` reifies a singleton before it computes a total, so
    // `bd@1e-10` plays.
    if expanded.len() == 1 {
        return compile(expanded[0].1);
    }

    // A zero weight total is silence: negative weights that cancel
    // (`bd@-1 sd@1`) or weights that round to zero (`bd@1e-10 sd@1e-10`) have
    // no windows to divide the cycle into.
    if total_weight(items) == Fraction::ZERO {
        return Built::Rhythm(Rhythm::rest());
    }

    // All-equal weights: plain fastcat.
    if expanded.iter().all(|(w, _)| *w == Fraction::ONE) {
        return concatenated(expanded.iter().map(|(_, ast)| (compile(ast), 1)).collect());
    }

    // Weighted: `timecat`, which is
    //   stack(pat_i._compressSpan(begin_i/total, end_i/total))
    // and `_compress` is `_fastGap(1/(e-b))._late(b)` - NOT plain `fast`, which
    // would repeat the step instead of leaving a gap.
    let windows = sequence_windows(items)
        .expect("Mini weight windows must be validated before pattern construction");
    // A weight can resolve to zero: the bounded `Fraction::from_f64`
    // rounds a near-zero `@1e-10` to 0/1. Its window is empty, and
    // `compress(b, b)` signals a "Division by zero" query error. Only a
    // `queryArc` boundary drains that signal. No boundary is active at
    // build time, so the error stays pending, and a later raw query or
    // transform program on this thread reads it as its own. Use a silent
    // part instead, as the stepwise and stepcat builders do.
    let parts: Vec<Option<Built>> = expanded
        .iter()
        .map(|(weight, ast)| (*weight != Fraction::ZERO).then(|| compile(ast)))
        .collect();
    let rhythm = rhythms(parts.iter().flatten()).and_then(|_| {
        let parts: Vec<Option<&Rhythm>> = parts
            .iter()
            .map(|part| match part {
                Some(Built::Rhythm(rhythm)) => Some(rhythm),
                _ => None,
            })
            .collect();
        Rhythm::squeezed(&parts, &windows)
    });
    if let Some(rhythm) = rhythm {
        return Built::Rhythm(rhythm);
    }
    Built::Graph(stack(
        parts
            .into_iter()
            .zip(windows)
            .map(|(part, (begin, end))| match part {
                Some(part) => part.into_pattern().compress(begin, end),
                None => silence(),
            })
            .collect(),
    ))
}

thread_local! {
    /// The source being built, for leaf hap context. Scoped by [`BuildSource`]
    /// rather than passed through `build_validated`'s many arms; save/restore
    /// keeps nested builds correct (a callback evaluated during a build may
    /// itself build mini).
    static BUILD_SOURCE: std::cell::RefCell<Option<(std::rc::Rc<str>, usize)>> =
        const { std::cell::RefCell::new(None) };
}

struct BuildSource(Option<(std::rc::Rc<str>, usize)>);

impl BuildSource {
    fn install(src: &str) -> Self {
        Self::install_at(src, 0)
    }

    /// The transpiler emits `m(str, offset)`, so hap context is
    /// DOCUMENT-absolute - editor highlighting matches decoration ids on it.
    fn install_at(src: &str, offset: usize) -> Self {
        BUILD_SOURCE.with(|cell| Self(cell.borrow_mut().replace((std::rc::Rc::from(src), offset))))
    }
}

impl Drop for BuildSource {
    fn drop(&mut self) {
        BUILD_SOURCE.with(|cell| *cell.borrow_mut() = self.0.take());
    }
}

/// The current build's leaf span, whitespace-corrected - `None` outside a
/// sourced build (raw `h()`-style construction stays context-free).
fn current_leaf_span(span: &Span) -> Option<(usize, usize)> {
    BUILD_SOURCE.with(|cell| {
        let source = cell.borrow();
        let (src, offset) = source.as_ref()?;
        Some(leaf_place(src, *offset, span))
    })
}

/// Where the leaf at `span` of `src` is in a score, when `src` begins
/// `offset` bytes after its opening quote: the place in a hap's context.
fn leaf_place(src: &str, offset: usize, span: &Span) -> (usize, usize) {
    let (start, end) = leaf_span(src.as_bytes(), span);
    // Hap context is QUOTED-relative - measured against the quoted source,
    // so the pinned fixture for `'bd sd'` is `{start:1,end:3}`, not
    // `{0,2}`. `leaf_locations` keeps its own separately-pinned unquoted
    // convention; only the hap context carries the +1. The build offset
    // then shifts it document-absolute.
    (start + 1 + offset, end + 1 + offset)
}

/// The values of a `a:b[:c...]` chain whose every side is a sounding atom,
/// in order - or None when a side is a rest, a sequence, or anything else
/// that needs the general combination.
fn tail_atom_chain(pat: &Ast, element: &Ast) -> Option<Vec<Value>> {
    let mut values = match pat {
        Ast::Tail {
            pat: inner,
            element: inner_element,
            ..
        } => tail_atom_chain(inner, inner_element)?,
        Ast::Atom { value, .. } if value != "-" && value != "~" => vec![atom_value(value)],
        _ => return None,
    };
    match element {
        Ast::Atom { value, .. } if value != "-" && value != "~" => values.push(atom_value(value)),
        _ => return None,
    }
    Some(values)
}

/// Leaf spans of an operator argument (the factor of `*4`, the pulses, steps
/// and rotation of a Euclid op, the endpoints of a range), quoted-relative.
/// A patterned argument's located pures merge their spans into each result
/// hap. The eager paths scalarize the argument, so they append these spans
/// explicitly and the hap context is the same either way.
enum ArgSpans {
    Empty,
    One([(usize, usize); 1]),
    Many(Vec<(usize, usize)>),
}

impl ArgSpans {
    #[inline]
    fn as_slice(&self) -> &[(usize, usize)] {
        match self {
            Self::Empty => &[],
            Self::One(one) => one,
            Self::Many(many) => many,
        }
    }
}

fn argument_context_inline(ast: &Ast) -> ArgSpans {
    BUILD_SOURCE.with(|cell| {
        let borrow = cell.borrow();
        let Some((src, offset)) = borrow.as_ref() else {
            return ArgSpans::Empty;
        };
        if let Ast::Atom { span, .. } = ast {
            return ArgSpans::One([leaf_place(src, *offset, span)]);
        }
        let mut leaves = Vec::new();
        collect_leaf_spans(ast, &mut leaves);
        if leaves.is_empty() {
            ArgSpans::Empty
        } else if leaves.len() == 1 {
            ArgSpans::One([leaf_place(src, *offset, &leaves[0])])
        } else {
            ArgSpans::Many(
                leaves
                    .iter()
                    .map(|span| leaf_place(src, *offset, span))
                    .collect(),
            )
        }
    })
}

fn build_validated(ast: &Ast) -> Pattern {
    compile(ast).into_pattern()
}

/// The pattern of `ast`, as plain values where the construct is made of
/// them and as a graph where it is not.
fn compile(ast: &Ast) -> Built {
    match ast {
        // krill's `step_char` includes `-` and `~`, so both are legal inside a
        // token (`-1`, `c-3`). But a token that is ENTIRELY `-` or `~` is a
        // rest: it occupies its slot and produces nothing.
        //
        // `a - b [- c]` yields only a, b, c, with the `-` slots silent rather
        // than atoms literally named "-".
        Ast::Atom { value, .. } if value == "-" || value == "~" => Built::Rhythm(Rhythm::rest()),
        // `pure(value).withLoc(...getLeafLocation(code, ast, offset))` -
        // the same whitespace-corrected span `getLeafLocations` reports,
        // carried as hap context for editor highlighting.
        Ast::Atom { value, span, .. } => {
            Built::Rhythm(Rhythm::atom(atom_value(value), current_leaf_span(span)))
        }
        Ast::Silence { .. } => Built::Rhythm(Rhythm::rest()),
        Ast::Seq { items, .. } => build_seq(items),
        Ast::Replicated { pat, amount, .. } => {
            let amount = native_weight(*amount);
            let built = match compile(pat) {
                Built::Rhythm(rhythm) if rhythms([]).is_some() => match rhythm.replicated(amount) {
                    Ok(rhythm) => return Built::Rhythm(rhythm),
                    Err(rhythm) => Built::Rhythm(rhythm),
                },
                built => built,
            };
            Built::Graph(built.into_pattern().repeat_cycles(amount).fast(amount))
        }
        Ast::Alt { lanes, .. } => {
            // `<a b, c d>` - each comma-separated sequence gets its own slow
            // lane, and the lanes run in parallel. Flattening them changes a
            // polymetric stack into one longer alternation.
            //
            // With weights each lane is NOT plain `slowcat`: `<a b@3>` gives
            // `b` three cycles. Each lane is `timecat(...).slow(total)`, and
            // the lanes are stacked.
            if lanes.is_empty() {
                return Built::Rhythm(Rhythm::rest());
            }
            stacked(
                lanes
                    .iter()
                    .map(|lane| {
                        let total = total_weight(&lane.items);
                        if total == Fraction::ZERO {
                            Built::Rhythm(Rhythm::rest())
                        } else {
                            at_rate(
                                build_seq(&lane.items),
                                Fraction::ONE.checked_div(total),
                                &[],
                                |lane| lane.slow(total),
                            )
                        }
                    })
                    .collect(),
            )
        }
        Ast::Stack { items, .. } => stacked(items.iter().map(compile).collect()),
        Ast::Polymeter {
            items,
            steps_per_cycle,
            ..
        } => {
            if items.is_empty() {
                return Built::Rhythm(Rhythm::rest());
            }
            let default_steps = ast_weight(&items[0]);
            // The steps pattern is always mapped before it reaches `fast`,
            // scalar sources included. `fmap` drops
            // `__pure`, so this must take the patterned/inner-join path: an
            // eager shortcut changes whole/part fragmentation across a query
            // spanning more than one cycle.
            let patterned_steps = steps_per_cycle
                .as_deref()
                .map(build_validated)
                .unwrap_or_else(|| pure(Value::F64(default_steps.to_f64())));
            Built::Graph(stack(
                items
                    .iter()
                    .map(|item| {
                        let weight = ast_weight(item);
                        if weight == Fraction::ZERO {
                            return silence();
                        }
                        let factor = patterned_steps.clone().fmap(move |value| {
                            match fraction_site_value(value) {
                                // A zero polymeter factor must fail at query
                                // time, not silence: encoding it as F64(0)
                                // would take the fast-zero silence path and
                                // let siblings live.
                                Some(steps) if steps == Fraction::ZERO => {
                                    rustel_core::signal_query_error(|| {
                                        "polymeter factor divides time by zero".into()
                                    });
                                    Value::Undefined
                                }
                                Some(steps) => match steps.checked_div(weight) {
                                    Some(factor) => Value::F64(factor.to_f64()),
                                    None => {
                                        rustel_core::mark_stepwise_refusal(
                                            QueryLimit::NativeFraction {
                                                operation: "polymeter",
                                            },
                                        );
                                        Value::Undefined
                                    }
                                },
                                None => {
                                    if let Some(message) = pinned_fraction_error(value) {
                                        rustel_core::signal_query_error(|| message);
                                    }
                                    Value::Undefined
                                }
                            }
                        });
                        apply_patterned_fast(build_validated(item), factor)
                    })
                    .collect(),
            ))
        }
        Ast::Choose { items, seed, .. } => Built::Graph(choose_cycles(
            items.iter().map(build_validated).collect(),
            *seed,
        )),
        Ast::Feet { items, .. } => {
            concatenated(items.iter().map(|item| (compile(item), 1)).collect())
        }
        Ast::Fast { pat, factor, .. } => match static_fraction_site(factor) {
            // The eager path scalarizes the factor; the factor atom's span
            // is appended so every hap still carries it.
            Some(value) => {
                let spans = argument_context_inline(factor);
                at_rate(compile(pat), Some(value), spans.as_slice(), |pattern| {
                    pattern.fast(value)
                })
            }
            None => {
                // Plain values at a rate for each cycle are plain values.
                let (built, rates) = (compile(pat), compile(factor));
                if let Some([rhythm, rates]) = rhythms([&built, &rates]).as_deref()
                    && let Some(rhythm) = rhythm.at_rates(rates, fraction_site_value)
                {
                    return Built::Rhythm(rhythm);
                }
                let factor = rates
                    .into_pattern()
                    .fmap(|value| match fraction_site_value(value) {
                        Some(factor) => Value::F64(factor.to_f64()),
                        None => {
                            if let Some(message) = pinned_fraction_error(value) {
                                rustel_core::signal_query_error(|| message);
                            }
                            Value::Undefined
                        }
                    });
                Built::Graph(apply_patterned_fast(built.into_pattern(), factor))
            }
        },
        Ast::Slow { pat, factor, .. } => {
            if let Some(value) = static_fraction_site(factor) {
                let spans = argument_context_inline(factor);
                return at_rate(
                    compile(pat),
                    Fraction::ONE.checked_div(value),
                    spans.as_slice(),
                    |pattern| pattern.slow(value),
                );
            }
            let reciprocal =
                build_validated(factor).fmap(|value| match fraction_site_value(value) {
                    // `slow(0)` is silence: a zero rate takes the `fast(0)` path.
                    Some(fraction) if fraction == Fraction::ZERO => Value::F64(0.0),
                    Some(fraction) => match Fraction::ONE.checked_div(fraction) {
                        Some(rate) => Value::F64(rate.to_f64()),
                        None => {
                            rustel_core::mark_stepwise_refusal(QueryLimit::NativeFraction {
                                operation: "slow",
                            });
                            Value::Undefined
                        }
                    },
                    None => {
                        if let Some(message) = pinned_fraction_error(value) {
                            rustel_core::signal_query_error(|| message);
                        }
                        Value::Undefined
                    }
                });
            Built::Graph(apply_patterned_fast(build_validated(pat), reciprocal))
        }
        Ast::Euclid {
            pat,
            pulses,
            steps,
            rotation,
            ..
        } => Built::Graph({
            if let (Some(pulses_value), Some(steps_value), true) = (
                static_number_fraction(euclid_argument_source(&pulses.ast)),
                static_number_fraction(euclid_argument_source(&steps.ast)),
                rotation
                    .as_ref()
                    .is_none_or(|rotation| euclid_argument_is_pure_atom(&rotation.ast)),
            ) {
                let rotation_value = rotation
                    .as_deref()
                    .and_then(|rotation| {
                        static_number_fraction(euclid_argument_source(&rotation.ast))
                    })
                    .unwrap_or(Fraction::ZERO);
                let s0 = argument_context_inline(euclid_argument_source(&pulses.ast));
                let s1 = argument_context_inline(euclid_argument_source(&steps.ast));
                let s2 = rotation
                    .as_deref()
                    .map(|r| argument_context_inline(euclid_argument_source(&r.ast)));
                let mut stack_spans = [(0usize, 0usize); 8];
                let mut stack_len = 0usize;
                let mut heap_spans: Option<Vec<(usize, usize)>> = None;
                for slice in [
                    s0.as_slice(),
                    s1.as_slice(),
                    s2.as_ref().map_or(&[][..], ArgSpans::as_slice),
                ] {
                    if let Some(vec) = &mut heap_spans {
                        vec.extend_from_slice(slice);
                    } else if stack_len + slice.len() <= stack_spans.len() {
                        stack_spans[stack_len..stack_len + slice.len()].copy_from_slice(slice);
                        stack_len += slice.len();
                    } else {
                        let mut vec = Vec::with_capacity(stack_len + slice.len());
                        vec.extend_from_slice(&stack_spans[..stack_len]);
                        vec.extend_from_slice(slice);
                        heap_spans = Some(vec);
                    }
                }
                let spans = match &heap_spans {
                    Some(vec) => vec.as_slice(),
                    None => &stack_spans[..stack_len],
                };
                let (pulses, steps, rotation) = (
                    pulses_value.to_f64() as i32,
                    steps_value.to_f64(),
                    rotation_value.to_f64() as i32,
                );
                let built = compile(pat);
                if let Some(rhythm) = struck(&built, pulses, steps, rotation) {
                    return Built::Rhythm(rhythm.located(spans));
                }
                let pat_out = euclid_pattern(built.into_pattern(), pulses, steps, rotation);
                if spans.is_empty() {
                    pat_out
                } else {
                    pat_out.with_added_context_slice(spans)
                }
            } else {
                let mut arguments =
                    build_validated(euclid_argument_source(&pulses.ast)).fmap_collect();
                arguments =
                    arguments.app_left_collect(build_validated(euclid_argument_source(&steps.ast)));
                if let Some(rotation) = rotation {
                    arguments = arguments
                        .app_left_collect(build_validated(euclid_argument_source(&rotation.ast)));
                }
                let base = build_validated(pat);
                arguments
                    .fmap_to_pattern(move |value| {
                        let Value::List(values) = value else {
                            return silence();
                        };
                        let (Some(pulses), Some(steps)) = (
                            values
                                .first()
                                .and_then(rustel_core::register::value_to_fraction),
                            values
                                .get(1)
                                .and_then(rustel_core::register::value_to_fraction),
                        ) else {
                            rustel_core::signal_query_error(|| "Invalid array length".into());
                            return silence();
                        };
                        let (pulses, steps) =
                            match checked_euclid_extents(pulses.to_f64(), steps.to_f64()) {
                                Ok(extents) => extents,
                                Err(EuclidExtentError::InvalidArrayLength) => {
                                    rustel_core::signal_query_error(|| {
                                        "Invalid array length".into()
                                    });
                                    return silence();
                                }
                                Err(EuclidExtentError::NativeLimit { steps }) => {
                                    rustel_core::refuse_euclid_steps(
                                        steps,
                                        MAX_NATIVE_EUCLID_STEPS,
                                    );
                                    return silence();
                                }
                            };
                        let rotation = values
                            .get(2)
                            .and_then(rustel_core::register::value_to_fraction)
                            .unwrap_or(Fraction::ZERO)
                            .to_f64() as i32;
                        euclid_pattern(base.clone(), pulses, steps, rotation)
                    })
                    .inner_join()
            }
        }),
        Ast::Degrade {
            pat, amount, seed, ..
        } => Built::Graph(build_validated(pat).degrade_by_seeded(amount.unwrap_or(0.5), *seed)),
        Ast::Tail { pat, element, span } => {
            // The editor highlights `bd:3` as one band. A chain of plain
            // atoms is built as one value under the span of the whole pair.
            // With one span per side, the `:` between them stays unlit.
            // Anything else (a sequence or a rest on either side) keeps
            // upstream's shape: one span per leaf.
            if let Some(values) = tail_atom_chain(pat, element)
                && let Some(loc) = current_leaf_span(span)
            {
                return Built::Rhythm(Rhythm::atom(Value::list(values), Some(loc)));
            }
            Built::Graph(build_validated(pat).app_left_with(
                build_validated(element),
                |left, right| match left {
                    Value::List(values) => {
                        let mut out = Vec::with_capacity(values.len() + 1);
                        out.extend_from_slice(values);
                        out.push(right.clone());
                        Value::list(out)
                    }
                    value => {
                        Value::list_from_arc(std::sync::Arc::from([value.clone(), right.clone()]))
                    }
                },
            ))
        }
        Ast::Range { start, end, .. } => Built::Graph({
            if let (Some(start_value), Some(end_value)) =
                (static_number_fraction(start), static_number_fraction(end))
            {
                // Range expansion appends the end atom's span before the
                // start atom's span (`6-7,1-2` for `"0 .. 3"`).
                let s0 = argument_context_inline(end);
                let s1 = argument_context_inline(start);
                let mut stack_spans = [(0usize, 0usize); 8];
                let mut stack_len = 0usize;
                let mut heap_spans: Option<Vec<(usize, usize)>> = None;
                for slice in [s0.as_slice(), s1.as_slice()] {
                    if let Some(vec) = &mut heap_spans {
                        vec.extend_from_slice(slice);
                    } else if stack_len + slice.len() <= stack_spans.len() {
                        stack_spans[stack_len..stack_len + slice.len()].copy_from_slice(slice);
                        stack_len += slice.len();
                    } else {
                        let mut vec = Vec::with_capacity(stack_len + slice.len());
                        vec.extend_from_slice(&stack_spans[..stack_len]);
                        vec.extend_from_slice(slice);
                        heap_spans = Some(vec);
                    }
                }
                let spans = match &heap_spans {
                    Some(vec) => vec.as_slice(),
                    None => &stack_spans[..stack_len],
                };
                let pat_out = range_pattern(start_value.to_f64(), end_value.to_f64());
                if spans.is_empty() {
                    pat_out
                } else {
                    pat_out.with_added_context_slice(spans)
                }
            } else {
                build_validated(start)
                    .fmap_collect()
                    .app_left_collect(build_validated(end))
                    .fmap_to_pattern(|value| {
                        let Value::List(values) = value else {
                            return silence();
                        };
                        let numbers: Vec<_> = values
                            .iter()
                            .filter_map(rustel_core::register::value_to_fraction)
                            .collect();
                        if numbers.len() != 2 {
                            return silence();
                        }
                        range_pattern(numbers[0].to_f64(), numbers[1].to_f64())
                    })
                    .inner_join()
            }
        }),
    }
}

fn apply_patterned_fast(pattern: Pattern, factor: Pattern) -> Pattern {
    default_registry()
        .get("fast")
        .expect("native fast registration")
        .call(&[factor], pattern)
}

fn euclid_pattern(pattern: Pattern, pulses: i32, steps: f64, rotation: i32) -> Pattern {
    rustel_core::combinators::euclid_with_steps(&pattern, pulses, steps, rotation)
}

/// How many elements a mini range (`0 .. 7`) may expand to. Bounds native
/// allocation while staying above the compatible ceiling below.
pub const MAX_MINI_RANGE: usize = 131_072;

/// A song that plays on strudel.cc has to play here, so the bound may never
/// drop below the 125,172 elements such a score can build.
const _: () = assert!(MAX_MINI_RANGE > 125_172);

enum RangeExtentError {
    /// The endpoints do not describe a finite count.
    InvalidArrayLength,
    /// Finite, but past what one pattern is allowed to allocate.
    NativeLimit { count: u64 },
}

/// Element count: `trunc(abs(end - start)) + 1`. `trunc(x) + 1 ==
/// trunc(x + 1)` for non-negative `x`, so truncating the span first is exact.
fn checked_range_count(start: f64, end: f64) -> Result<usize, RangeExtentError> {
    let span = (end - start).abs();
    if !span.is_finite() {
        return Err(RangeExtentError::InvalidArrayLength);
    }
    let count = span.trunc() + 1.0;
    if count > MAX_MINI_RANGE as f64 {
        return Err(RangeExtentError::NativeLimit {
            count: count as u64,
        });
    }
    Ok(count as usize)
}

fn range_pattern(start: f64, end: f64) -> Pattern {
    // Static ranges are checked during construction. Patterned endpoints are
    // checked here, before the expansion is allocated.
    let count = match checked_range_count(start, end) {
        Ok(count) => count,
        Err(RangeExtentError::InvalidArrayLength) => {
            return query_error_pattern("Invalid array length");
        }
        Err(RangeExtentError::NativeLimit { count }) => {
            return query_limit_pattern(QueryLimit::MiniRange {
                elements: count,
                limit: MAX_MINI_RANGE as u64,
            });
        }
    };
    if count == 1 {
        return pure(Value::F64(start));
    }
    let direction = if start <= end { 1.0 } else { -1.0 };
    let mut vals = Vec::new();
    if vals.try_reserve_exact(count).is_err() {
        return query_limit_pattern(QueryLimit::HostMemory);
    }
    vals.extend((0..count).map(|index| Value::F64(start + index as f64 * direction)));
    let values: std::sync::Arc<[Value]> = vals.into();
    if let Some(pattern) = rustel_core::sequence_of_equal_values(Fraction::ONE, values, true) {
        return pattern.with_steps(Some(Fraction::int(count as i128)));
    }
    let mut patterns = Vec::new();
    if patterns.try_reserve_exact(count).is_err() {
        return query_limit_pattern(QueryLimit::HostMemory);
    }
    patterns.extend((0..count).map(|index| pure(Value::F64(start + index as f64 * direction))));
    fastcat(patterns)
}

/// An atom that reads as a JS Number becomes one, so mini atoms accept the
/// JavaScript radix spellings, which `f64::from_str` alone would not.
fn atom_value(value: &str) -> Value {
    js_atom_number(value)
        .map(Value::F64)
        .unwrap_or_else(|| Value::Str(value.into()))
}

#[inline]
fn js_atom_number(value: &str) -> Option<f64> {
    let bytes = value.as_bytes();
    let b0 = *bytes.first()?;
    if b0.is_ascii_digit() {
        if bytes.len() == 1 {
            return Some(f64::from(b0 - b'0'));
        }
        if b0 == b'0' {
            match bytes[1] {
                b'x' | b'X' => return js_radix_number(&value[2..], 16),
                b'b' | b'B' => return js_radix_number(&value[2..], 2),
                b'o' | b'O' => return js_radix_number(&value[2..], 8),
                _ => {}
            }
        }
        // Fast exact integer path for up to 15 ASCII digits (< 2^53).
        if bytes.len() <= 15 {
            let mut acc = u64::from(b0 - b'0');
            let mut all_digits = true;
            for &b in &bytes[1..] {
                if b.is_ascii_digit() {
                    acc = acc * 10 + u64::from(b - b'0');
                } else {
                    all_digits = false;
                    break;
                }
            }
            if all_digits {
                return Some(acc as f64);
            }
        }
    } else {
        match b0 {
            b'-' => {
                if bytes.len() == 9 && value == "-Infinity" {
                    return Some(f64::NEG_INFINITY);
                }
                if bytes.len() >= 2 && bytes.len() <= 16 && bytes[1].is_ascii_digit() {
                    let mut acc = u64::from(bytes[1] - b'0');
                    let mut all_digits = true;
                    for &b in &bytes[2..] {
                        if b.is_ascii_digit() {
                            acc = acc * 10 + u64::from(b - b'0');
                        } else {
                            all_digits = false;
                            break;
                        }
                    }
                    if all_digits {
                        return Some(-(acc as f64));
                    }
                }
            }
            b'+' | b'.' => {}
            b'I' => return (value == "Infinity").then_some(f64::INFINITY),
            _ => return None,
        }
    }

    let mut at = usize::from(matches!(b0, b'+' | b'-'));
    let mut digits = 0usize;
    while bytes.get(at).is_some_and(u8::is_ascii_digit) {
        at += 1;
        digits += 1;
    }
    if bytes.get(at) == Some(&b'.') {
        at += 1;
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return None;
    }
    if matches!(bytes.get(at), Some(b'e' | b'E')) {
        at += 1;
        if matches!(bytes.get(at), Some(b'+' | b'-')) {
            at += 1;
        }
        let exponent_start = at;
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
        if at == exponent_start {
            return None;
        }
    }
    (at == bytes.len())
        .then(|| value.parse::<f64>().ok())
        .flatten()
}

/// Convert a JavaScript binary/octal/hex integer literal to binary64 with one
/// final ties-to-even rounding step.
///
/// Accumulating into f64 digit by digit double-rounds long literals. These
/// radices are powers of two, so retaining the leading 53 significand bits and
/// a guard/sticky pair is both exact and allocation-free even for a hostile
/// token. Every byte is still validated after the result is already known.
fn js_radix_number(digits: &str, radix: u32) -> Option<f64> {
    if digits.is_empty() {
        return None;
    }
    let bits_per_digit = match radix {
        2 => 1,
        8 => 3,
        16 => 4,
        _ => return None,
    };
    let mut started = false;
    let mut bit_count = 0usize;
    let mut significand = 0u64;
    let mut guard = false;
    let mut sticky = false;

    for byte in digits.bytes() {
        let digit = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => return None,
        };
        if u32::from(digit) >= radix {
            return None;
        }
        for shift in (0..bits_per_digit).rev() {
            let bit = (digit >> shift) & 1;
            if !started {
                if bit == 0 {
                    continue;
                }
                started = true;
            }
            match bit_count {
                0..=52 => significand = (significand << 1) | u64::from(bit),
                53 => guard = bit != 0,
                _ => sticky |= bit != 0,
            }
            bit_count = bit_count.saturating_add(1);
        }
    }

    if !started {
        return Some(0.0);
    }
    if bit_count <= 53 {
        return Some(significand as f64);
    }

    if guard && (sticky || significand & 1 != 0) {
        significand += 1;
    }
    let mut exponent = bit_count - 1;
    if significand == 1 << 53 {
        significand >>= 1;
        exponent = exponent.saturating_add(1);
    }
    if exponent > 1023 {
        return Some(f64::INFINITY);
    }
    let biased_exponent = (exponent as u64 + 1023) << 52;
    let fraction = significand & ((1 << 52) - 1);
    Some(f64::from_bits(biased_exponent | fraction))
}

fn static_fraction_site(ast: &Ast) -> Option<Fraction> {
    match ast {
        Ast::Atom { value, .. } => fraction_site_value(&atom_value(value)),
        Ast::Seq { items, .. } if items.len() == 1 => static_fraction_site(&items[0].ast),
        _ => None,
    }
}

fn fraction_site_value(value: &Value) -> Option<Fraction> {
    // Fraction's bounded parser already implements numeric separators. Do not
    // build a second, potentially multi-megabyte copy before that bound gets a
    // chance to reject hostile mini-notation text.
    rustel_core::register::value_to_fraction(value)
}

/// Scalar JavaScript Number coercion, used by operators that do arithmetic on
/// the promoted atom directly rather than passing it to Fraction.js.
fn static_number_fraction(ast: &Ast) -> Option<Fraction> {
    match ast {
        Ast::Atom { value, .. } => js_atom_number(value).and_then(Fraction::from_f64),
        _ => None,
    }
}

/// The pinned `applyOptions` implementation calls `enter(argument)` on a
/// Euclid `slice_with_ops`. Entering that element immediately enters its
/// `source_`; it never applies the element's own `options_`. The parser keeps
/// those options as ordinary AST wrappers so locations and the private AST do
/// not lie, but public construction must peel them here.
fn euclid_argument_source(ast: &Ast) -> &Ast {
    match ast {
        Ast::Fast { pat, .. }
        | Ast::Slow { pat, .. }
        | Ast::Replicated { pat, .. }
        | Ast::Euclid { pat, .. }
        | Ast::Degrade { pat, .. }
        | Ast::Tail { pat, .. } => euclid_argument_source(pat),
        Ast::Range { start, .. } => euclid_argument_source(start),
        other => other,
    }
}

fn euclid_argument_is_pure_atom(ast: &Ast) -> bool {
    matches!(
        euclid_argument_source(ast),
        Ast::Atom { value, .. } if !matches!(value.as_str(), "-" | "~")
    )
}

fn pinned_fraction_error(value: &Value) -> Option<String> {
    match value {
        Value::F64(number) if number.is_infinite() => Some(fraction_conversion_error(*number)),
        Value::Str(value)
            if js_atom_number(value).is_none()
                && !value.contains('_')
                && !matches!(&**value, "-" | "~") =>
        {
            Some("Invalid argument".into())
        }
        _ => None,
    }
}

const MAX_NATIVE_EUCLID_STEPS: u64 = rustel_core::euclid::MAX_EUCLID_STEPS as u64;

enum EuclidExtentError {
    InvalidArrayLength,
    NativeLimit { steps: u64 },
}

fn checked_euclid_extents(pulses: f64, steps: f64) -> Result<(i32, f64), EuclidExtentError> {
    if !pulses.is_finite() || !steps.is_finite() || pulses.fract() != 0.0 {
        return Err(EuclidExtentError::InvalidArrayLength);
    }
    if pulses == 0.0 || steps == 0.0 {
        return Ok((0, 0.0));
    }
    let total = (pulses.abs() / steps.abs()).ceil() * steps.abs();
    if total.fract() != 0.0 {
        return Err(EuclidExtentError::InvalidArrayLength);
    }
    if total > MAX_NATIVE_EUCLID_STEPS as f64 {
        return Err(EuclidExtentError::NativeLimit {
            steps: total as u64,
        });
    }
    Ok((pulses as i32, steps))
}

fn ast_weight(ast: &Ast) -> Fraction {
    match ast {
        Ast::Seq { items, .. } => polymeter_sequence_weight(items),
        Ast::Alt { lanes, .. } => lanes
            .first()
            .map_or(Fraction::ONE, |lane| polymeter_sequence_weight(&lane.items)),
        Ast::Replicated { amount, .. } => native_weight(*amount),
        _ => Fraction::ONE,
    }
}

/// Weight exposed to the outer polymeter. An all-zero lane must stay zero -
/// it feeds the caught division-by-zero refusal - and must not inherit the
/// sequence-time `zero || 1` fallback.
fn polymeter_sequence_weight(items: &[Step]) -> Fraction {
    if !items.is_empty() && items.iter().all(|step| step.weight == 0.0) {
        Fraction::ZERO
    } else {
        total_weight(items)
    }
}

fn source_line_at(src: &str, offset: usize) -> usize {
    src.as_bytes()[..offset.min(src.len())]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
        + 1
}

fn validate_direct_factor(src: &str, factor: &Ast) -> Result<(), ParseError> {
    let Ast::Atom { value, span, .. } = factor else {
        return Ok(());
    };
    if matches!(value.as_str(), "-" | "~") || static_fraction_site(factor).is_some() {
        return Ok(());
    }
    let (message, phase) = match js_atom_number(value) {
        Some(number) if number.is_infinite() => {
            (fraction_conversion_error(number), ErrorPhase::Construct)
        }
        Some(number) => (fraction_conversion_error(number), ErrorPhase::NativeLimit),
        None => ("Invalid argument".into(), ErrorPhase::Construct),
    };
    Err(ParseError {
        message,
        offset: span.start,
        line: source_line_at(src, span.start),
        phase,
    })
}

fn validate_euclid_extents(src: &str, pulses: &Ast, steps: &Ast) -> Result<(), ParseError> {
    let (
        Ast::Atom { value: pulse, .. },
        Ast::Atom {
            value: step, span, ..
        },
    ) = (pulses, steps)
    else {
        return Ok(());
    };
    if matches!(pulse.as_str(), "-" | "~") || matches!(step.as_str(), "-" | "~") {
        return Ok(());
    }
    let result = js_atom_number(pulse)
        .zip(js_atom_number(step))
        .ok_or(EuclidExtentError::InvalidArrayLength)
        .and_then(|(pulse, step)| checked_euclid_extents(pulse, step));
    match result {
        Ok(_) => Ok(()),
        Err(EuclidExtentError::InvalidArrayLength) => Err(ParseError {
            message: "Invalid array length".into(),
            offset: span.start,
            line: source_line_at(src, span.start),
            phase: ErrorPhase::Construct,
        }),
        Err(EuclidExtentError::NativeLimit { steps }) => Err(ParseError {
            message: format!(
                "Euclidean rhythm requested {steps} steps, above the native limit of {MAX_NATIVE_EUCLID_STEPS}"
            ),
            offset: span.start,
            line: source_line_at(src, span.start),
            phase: ErrorPhase::NativeLimit,
        }),
    }
}

/// Validate construction-time failures that krill represents in its AST and
/// `patternifyAST` raises only while turning that AST into a Pattern.
fn validate_native_weight(src: &str, value: f64, offset: usize) -> Result<(), ParseError> {
    if Fraction::from_f64(value).is_some() {
        return Ok(());
    }
    Err(ParseError {
        message: fraction_conversion_error(value),
        offset,
        line: source_line_at(src, offset),
        phase: if value.is_finite() {
            ErrorPhase::NativeLimit
        } else {
            ErrorPhase::Construct
        },
    })
}

/// Refuses a sequence whose weights cannot be laid out across one cycle in
/// native Fractions (see [`sequence_windows`]), at the step where that fails.
fn validate_sequence_windows(src: &str, items: &[Step]) -> Result<(), ParseError> {
    sequence_windows(items).map(drop).map_err(|step| {
        let offset = step.ast.span().start;
        ParseError {
            message: "sequence weights exceed the native Fraction range".into(),
            offset,
            line: source_line_at(src, offset),
            phase: ErrorPhase::NativeLimit,
        }
    })
}

fn validate_steps(src: &str, items: &[Step]) -> Result<(), ParseError> {
    // Children are validated before the sequence's weights, matching
    // construction order.
    for step in items {
        validate_construct(src, &step.ast)?;
    }
    if evenly_weighted(items) {
        return Ok(());
    }
    for step in items {
        validate_native_weight(src, step.weight, step.ast.span().start)?;
    }
    validate_sequence_windows(src, items)
}

/// Validate only the JavaScript Number fields that the native builder must
/// convert to its bounded Fraction representation. This keeps the established
/// public parse-then-build path lazy for unrelated query-time resource limits
/// such as Euclid size, while ensuring a private AST retained by `parse` can
/// never make `build` panic on Infinity, an out-of-range finite Number, or
/// sequence weights outside the native Fraction range.
fn validate_build_steps(src: &str, items: &[Step]) -> Result<(), ParseError> {
    for step in items {
        validate_build_weights(src, &step.ast)?;
    }
    if evenly_weighted(items) {
        return Ok(());
    }
    for step in items {
        validate_native_weight(src, step.weight, step.ast.span().start)?;
    }
    validate_sequence_windows(src, items)
}

fn validate_build_weights(src: &str, ast: &Ast) -> Result<(), ParseError> {
    match ast {
        Ast::Atom { .. } | Ast::Silence { .. } => Ok(()),
        Ast::Seq { items, .. } => validate_build_steps(src, items),
        Ast::Replicated { pat, amount, span } => {
            validate_build_weights(src, pat)?;
            validate_native_weight(src, *amount, span.start)
        }
        Ast::Alt { lanes, .. } => lanes
            .iter()
            .try_for_each(|lane| validate_build_steps(src, &lane.items)),
        Ast::Stack { items, .. } | Ast::Choose { items, .. } | Ast::Feet { items, .. } => items
            .iter()
            .try_for_each(|item| validate_build_weights(src, item)),
        Ast::Polymeter {
            items,
            steps_per_cycle,
            ..
        } => {
            items
                .iter()
                .try_for_each(|item| validate_build_weights(src, item))?;
            if let Some(steps_per_cycle) = steps_per_cycle {
                validate_build_weights(src, steps_per_cycle)?;
            }
            Ok(())
        }
        Ast::Fast { pat, factor, .. } | Ast::Slow { pat, factor, .. } => {
            validate_build_weights(src, pat)?;
            validate_build_weights(src, factor)
        }
        Ast::Euclid {
            pat,
            pulses,
            steps,
            rotation,
            ..
        } => {
            validate_build_weights(src, pat)?;
            // patternifyAST deliberately enters only each Euclid argument's
            // base source; postfix weights/options remain private structure.
            validate_build_weights(src, euclid_argument_source(&pulses.ast))?;
            validate_build_weights(src, euclid_argument_source(&steps.ast))?;
            if let Some(rotation) = rotation {
                validate_build_weights(src, euclid_argument_source(&rotation.ast))?;
            }
            Ok(())
        }
        Ast::Degrade { pat, .. } => validate_build_weights(src, pat),
        Ast::Tail { pat, element, .. } => {
            validate_build_weights(src, pat)?;
            validate_build_weights(src, element)
        }
        Ast::Range { start, end, .. } => {
            validate_build_weights(src, start)?;
            validate_build_weights(src, end)
        }
    }
}

fn validate_construct(src: &str, ast: &Ast) -> Result<(), ParseError> {
    match ast {
        Ast::Atom { .. } | Ast::Silence { .. } => Ok(()),
        Ast::Seq { items, .. } => validate_steps(src, items),
        Ast::Replicated { pat, amount, span } => {
            validate_construct(src, pat)?;
            validate_native_weight(src, *amount, span.start)
        }
        Ast::Alt { lanes, .. } => lanes
            .iter()
            .try_for_each(|lane| validate_steps(src, &lane.items)),
        Ast::Stack { items, .. } | Ast::Choose { items, .. } | Ast::Feet { items, .. } => items
            .iter()
            .try_for_each(|item| validate_construct(src, item)),
        Ast::Polymeter {
            items,
            steps_per_cycle,
            ..
        } => {
            items
                .iter()
                .try_for_each(|item| validate_construct(src, item))?;
            if let Some(steps_per_cycle) = steps_per_cycle {
                validate_construct(src, steps_per_cycle)?;
            }
            Ok(())
        }
        Ast::Fast { pat, factor, .. } | Ast::Slow { pat, factor, .. } => {
            // `patternifyAST` enters the receiver before its postfix argument.
            validate_construct(src, pat)?;
            validate_direct_factor(src, factor)?;
            validate_construct(src, factor)
        }
        Ast::Euclid {
            pat,
            pulses,
            steps,
            rotation,
            ..
        } => {
            validate_construct(src, pat)?;
            validate_construct(src, euclid_argument_source(&pulses.ast))?;
            validate_construct(src, euclid_argument_source(&steps.ast))?;
            if let Some(rotation) = rotation {
                validate_construct(src, euclid_argument_source(&rotation.ast))?;
            }
            // `patternifyAST` enters every argument SOURCE before the eager
            // scalar Euclid constructor runs. A nested error in a later source
            // therefore wins over an invalid-array-length error in `pulses`;
            // postfix options on the argument itself remain unentered.
            if euclid_argument_is_pure_atom(&pulses.ast)
                && euclid_argument_is_pure_atom(&steps.ast)
                && rotation
                    .as_ref()
                    .is_none_or(|rotation| euclid_argument_is_pure_atom(&rotation.ast))
            {
                validate_euclid_extents(
                    src,
                    euclid_argument_source(&pulses.ast),
                    euclid_argument_source(&steps.ast),
                )?;
            }
            Ok(())
        }
        Ast::Degrade { pat, .. } => validate_construct(src, pat),
        Ast::Tail { pat, element, .. } => {
            validate_construct(src, pat)?;
            validate_construct(src, element)
        }
        Ast::Range { start, end, .. } => {
            validate_construct(src, start)?;
            validate_construct(src, end)?;
            validate_range_extent(src, start, end)
        }
    }
}

/// A range whose endpoints are both literal numbers has a known length before
/// anything is allocated, so it is refused here rather than at the allocation.
fn validate_range_extent(src: &str, start: &Ast, end: &Ast) -> Result<(), ParseError> {
    let (Some(start_value), Some(end_value)) =
        (static_number_fraction(start), static_number_fraction(end))
    else {
        return Ok(());
    };
    let span = start.span();
    match checked_range_count(start_value.to_f64(), end_value.to_f64()) {
        Ok(_) => Ok(()),
        Err(RangeExtentError::InvalidArrayLength) => Err(ParseError {
            message: "Invalid array length".into(),
            offset: span.start,
            line: source_line_at(src, span.start),
            phase: ErrorPhase::Construct,
        }),
        Err(RangeExtentError::NativeLimit { count }) => Err(ParseError {
            message: format!(
                "range expands to {count} elements, above the native limit of {MAX_MINI_RANGE}"
            ),
            offset: span.start,
            line: source_line_at(src, span.start),
            phase: ErrorPhase::NativeLimit,
        }),
    }
}

/// Construct a native Pattern from an already-parsed Mini AST.
///
/// Krill's private AST can retain JavaScript Numbers that Fraction.js rejects
/// (or that exceed the documented i128 Fraction ceiling), so building
/// remains fallible even after parsing succeeds.
pub fn build(src: &str, ast: &Ast) -> Result<Pattern, ParseError> {
    validate_build_weights(src, ast)?;
    let _scope = BuildSource::install(src);
    Ok(build_validated(ast))
}

/// Parse and build in one step.
pub fn mini(src: &str) -> Result<Pattern, ParseError> {
    mini_at(src, 0)
}

/// `mini` with a document offset, the transpiler-facing `m(str, offset)`:
/// leaf hap context comes out DOCUMENT-absolute (quoted-relative + offset).
/// Editor highlighting matches decoration ids on these absolute spans.
pub fn mini_at(src: &str, offset: usize) -> Result<Pattern, ParseError> {
    mini_in(&mut Room::default(), src, offset)
}

/// Value of a plain word or colon-separated chain (`"bd:1"`, `"c:minor"`)
/// already validated to hold only sounding words.
fn flat_atom_value(word: &str) -> Value {
    if !word.contains(':') {
        return atom_value(word);
    }
    let mut parts: [Value; 4] = [
        Value::Undefined,
        Value::Undefined,
        Value::Undefined,
        Value::Undefined,
    ];
    let mut count = 0usize;
    let mut spilled: Option<Vec<Value>> = None;
    for seg in word.split(':') {
        let val = atom_value(seg);
        if let Some(vec) = &mut spilled {
            vec.push(val);
        } else if count < parts.len() {
            parts[count] = val;
            count += 1;
        } else {
            let mut vec = Vec::with_capacity(8);
            for slot in &mut parts {
                vec.push(std::mem::replace(slot, Value::Undefined));
            }
            vec.push(val);
            spilled = Some(vec);
        }
    }
    let list: std::sync::Arc<[Value]> = match spilled {
        Some(vec) => std::sync::Arc::from(vec),
        None => {
            let [p0, p1, p2, p3] = parts;
            match count {
                2 => std::sync::Arc::from([p0, p1]),
                3 => std::sync::Arc::from([p0, p1, p2]),
                _ => std::sync::Arc::from([p0, p1, p2, p3]),
            }
        }
    };
    Value::List(list)
}

/// Scan an ASCII word (or contiguous `word:word[:word...]` chain) starting at
/// `start` within `bytes[..limit]`, returning its exclusive end byte offset.
/// Callers guarantee `bytes` is valid ASCII (`src.is_ascii()`).
#[inline]
fn flat_word_end(_src: &str, bytes: &[u8], start: usize, limit: usize) -> Option<usize> {
    let mut end = start;
    let mut prev_dot = false;
    while end < limit {
        let b = bytes[end];
        if !ASCII_STEP_CHARS[usize::from(b)] {
            break;
        }
        if b == b'.' {
            if prev_dot {
                return None;
            }
            prev_dot = true;
        } else {
            prev_dot = false;
        }
        end += 1;
    }
    if end == start {
        return None;
    }
    if bytes[start] == b'_' || (end - start == 1 && bytes[start] == b'.') {
        return None;
    }
    if end < limit && bytes[end] == b':' {
        if end - start == 1 && matches!(bytes[start], b'~' | b'-') {
            return None;
        }
        while end < limit && bytes[end] == b':' {
            let w_start = end + 1;
            let mut w_end = w_start;
            let mut seg_prev_dot = false;
            while w_end < limit {
                let b = bytes[w_end];
                if !ASCII_STEP_CHARS[usize::from(b)] {
                    break;
                }
                if b == b'.' {
                    if seg_prev_dot {
                        return None;
                    }
                    seg_prev_dot = true;
                } else {
                    seg_prev_dot = false;
                }
                w_end += 1;
            }
            if w_end == w_start {
                return None;
            }
            if bytes[w_start] == b'^'
                || (w_end - w_start == 1 && matches!(bytes[w_start], b'~' | b'-' | b'.' | b'_'))
            {
                return None;
            }
            end = w_end;
        }
    }
    Some(end)
}

/// Pack a non-empty ASCII word of 1..=4 bytes (all bytes non-zero) into a
/// unique non-zero `u32` key, or `0` for words longer than 4 bytes.
#[inline(always)]
fn word_key4(bytes: &[u8], start: usize, end: usize) -> u32 {
    match end - start {
        1 => u32::from(bytes[start]),
        2 => u32::from(bytes[start]) | (u32::from(bytes[start + 1]) << 8),
        3 => {
            u32::from(bytes[start])
                | (u32::from(bytes[start + 1]) << 8)
                | (u32::from(bytes[start + 2]) << 16)
        }
        4 => u32::from_le_bytes([
            bytes[start],
            bytes[start + 1],
            bytes[start + 2],
            bytes[start + 3],
        ]),
        _ => 0,
    }
}

/// Find or insert `(start, end)` in a `seen_words` table using `word_key4`
/// so 1..=4-byte words compare as a single `u32` without calling `memcmp`.
#[inline(always)]
fn intern_flat_word(
    bytes: &[u8],
    seen_words: &mut [(u16, u16, u32)],
    seen_len: &mut usize,
    start: usize,
    end: usize,
) -> Option<u16> {
    let key = word_key4(bytes, start, end);
    if key != 0 {
        for (idx, &(_, _, k)) in seen_words[..*seen_len].iter().enumerate() {
            if k == key {
                return Some(idx as u16);
            }
        }
    } else {
        let word_bytes = &bytes[start..end];
        let wlen = word_bytes.len();
        let b0 = word_bytes[0];
        for (idx, &(ws, we, k)) in seen_words[..*seen_len].iter().enumerate() {
            if k == 0
                && usize::from(we - ws) == wlen
                && bytes[usize::from(ws)] == b0
                && &bytes[usize::from(ws)..usize::from(we)] == word_bytes
            {
                return Some(idx as u16);
            }
        }
    }
    if *seen_len >= seen_words.len() {
        return None;
    }
    let idx = *seen_len as u16;
    seen_words[*seen_len] = (start as u16, end as u16, key);
    *seen_len += 1;
    Some(idx)
}

/// Single-pass fast path for plain ASCII sequences (`"bd ~ sd ~"`, `"c3 e3 g3"`)
/// and plain equal alternations (`"<c3 e3 g3>"`) of up to 16 words, building
/// `SmallGridStep` directly on a tiny stack frame without `FlatItem`,
/// `alt_words`, `TickStep`, or `is_ascii()`/`contains(b',')` pre-scans.
fn try_plain_flat_into(
    src: &str,
    offset: usize,
    mut into: Option<&mut rustel_core::SequenceBuilder>,
) -> Option<Pattern> {
    if src.len() > usize::from(u16::MAX) {
        return None;
    }
    let bytes = src.as_bytes();
    let mut pos = 0usize;
    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\r' | b'\n') {
        pos += 1;
    }
    if pos >= bytes.len() || bytes[pos] == b'^' {
        return None;
    }
    let mut limit = bytes.len();
    while limit > pos && matches!(bytes[limit - 1], b' ' | b'\t' | b'\r' | b'\n') {
        limit -= 1;
    }
    let mut is_alt = false;
    if bytes[pos] == b'<' {
        if bytes[limit - 1] != b'>' {
            return None;
        }
        is_alt = true;
        pos += 1;
        limit -= 1;
        while pos < limit && matches!(bytes[pos], b' ' | b'\t' | b'\r' | b'\n') {
            pos += 1;
        }
        while limit > pos && matches!(bytes[limit - 1], b' ' | b'\t' | b'\r' | b'\n') {
            limit -= 1;
        }
        if pos >= limit || bytes[pos] == b'^' {
            return None;
        }
    }
    const MAX_PLAIN: usize = 32;
    let mut seen_words: [(u16, u16, u32); MAX_PLAIN] = [(0, 0, 0); MAX_PLAIN];
    let mut seen_len = 0usize;
    let mut raw_steps = [(0u16, 0u16, 0u16, 0u32, 0u16); MAX_PLAIN];
    let mut step_len = 0usize;
    let mut count = 0usize;
    let mut min_begin = u32::MAX;
    let mut max_begin = 0u32;
    let mut max_len = 0u16;
    while pos < limit {
        let start = pos;
        let end = flat_word_end(src, bytes, start, limit)?;
        if end < limit {
            let mut next = end;
            while next < limit && matches!(bytes[next], b' ' | b'\t' | b'\r' | b'\n') {
                next += 1;
            }
            if next == end {
                return None;
            }
            pos = next;
        } else {
            pos = limit;
        }
        if count >= MAX_PLAIN {
            return None;
        }
        let wlen = end - start;
        if !(wlen == 1 && matches!(bytes[start], b'~' | b'-')) {
            let value_idx = intern_flat_word(bytes, &mut seen_words, &mut seen_len, start, end)?;
            let (lb, le) = leaf_place(src, offset, &Span { start, end });
            let loc_begin = u32::try_from(lb).ok()?;
            let loc_len = u16::try_from(le.checked_sub(lb)?).ok()?;
            if loc_len > 0 {
                min_begin = min_begin.min(loc_begin);
                max_begin = max_begin.max(loc_begin);
                max_len = max_len.max(loc_len);
            }
            raw_steps[step_len] = (
                count as u16,
                (count + 1) as u16,
                value_idx,
                loc_begin,
                loc_len,
            );
            step_len += 1;
        }
        count += 1;
    }
    if count == 0 {
        return None;
    }
    if count == 1 {
        if is_alt {
            return None;
        }
        let builder = into.as_deref_mut()?;
        if step_len == 0 {
            return Some(silence());
        }
        let (ws, we, _) = seen_words[0];
        let (_, _, _, loc_begin, loc_len) = raw_steps[0];
        if loc_begin >= (1 << 31) || loc_len >= 255 {
            return None;
        }
        let values: std::sync::Arc<[Value]> =
            rustel_core::arc_single_value(flat_atom_value(&src[usize::from(ws)..usize::from(we)]));
        let ticks = std::num::NonZeroU32::new(1).expect("non-zero");
        return builder
            .push_ticks_plain(
                Fraction::ONE,
                ticks,
                values,
                true,
                &[(0u16, 1u16, 0u16, loc_begin, loc_len)],
            )
            .then(silence);
    }
    let steps_meta = if is_alt {
        Some(Fraction::ONE)
    } else {
        Some(Fraction::int(i128::from(count as u32)))
    };
    if step_len == 0 {
        return Some(if into.is_some() {
            silence()
        } else {
            silence().with_steps(steps_meta)
        });
    }
    let base = if min_begin == u32::MAX { 0 } else { min_begin };
    if base >= (1 << 31)
        || (min_begin != u32::MAX && max_begin - base >= (1 << 22))
        || max_len >= 255
    {
        return None;
    }
    let layer_values: std::sync::Arc<[Value]> = seen_words[..seen_len]
        .iter()
        .map(|&(ws, we, _)| flat_atom_value(&src[usize::from(ws)..usize::from(we)]))
        .collect();
    let (period, ticks) = if is_alt {
        (
            Fraction::int(i128::from(count as u32)),
            std::num::NonZeroU32::new(1).expect("non-zero"),
        )
    } else {
        (
            Fraction::ONE,
            std::num::NonZeroU32::new(count as u32).expect("count >= 2"),
        )
    };
    if let Some(builder) = into {
        return builder
            .push_ticks_plain(period, ticks, layer_values, true, &raw_steps[..step_len])
            .then(silence);
    }
    let pattern = rustel_core::sequence_of_ticks_plain(
        period,
        ticks,
        layer_values,
        true,
        &raw_steps[..step_len],
    )?;
    Some(pattern.with_steps(steps_meta))
}

/// Value and inline source location of a single-word or colon-chained
/// Parsed value of a single-word or colon-separated compound word mini literal,
/// keeping 2-part compounds (`"bd:1"`, `"c:minor"`) inline without allocating
/// an `Arc<[Value]>`.
pub enum WordConstant {
    One(Value),
    Pair(Value, Value),
    Many(std::sync::Arc<[Value]>),
}

/// Extract the value and location of a single-word or colon-separated compound
/// (`"bd:1"`, `"c:minor"`) mini literal without allocating a list for 2-part
/// compounds. `None` for rests, multi-word patterns, or other operators.
pub fn word_constant_parts(
    src: &str,
    offset: usize,
) -> Option<(WordConstant, rustel_core::Locations)> {
    let bytes = src.as_bytes();
    let start = if matches!(bytes.first(), Some(b' ' | b'\n' | b'\r' | b'\t' | 0xC2)) {
        score_whitespace_end(bytes, 0)
    } else {
        0
    };
    // A leading `^` marks the steps of a sequence, which the plans read.
    let first_byte = *bytes.get(start)?;
    if first_byte == b'^' {
        return None;
    }
    let mut end = start;
    let mut prev_dot = false;
    while end < bytes.len() {
        let b = bytes[end];
        if !ASCII_STEP_CHARS[usize::from(b)] {
            if b >= 128 {
                while let Some(len) = word_char_len(bytes, end) {
                    end += len;
                }
                if src[start..end].contains("..") {
                    return None;
                }
            }
            break;
        }
        if b == b'.' {
            if prev_dot {
                return None;
            }
            prev_dot = true;
        } else {
            prev_dot = false;
        }
        end += 1;
    }
    if end == start {
        return None;
    }
    if end - start == 1 && matches!(first_byte, b'-' | b'~' | b'.' | b'_') {
        return None;
    }
    let first = &src[start..end];
    if end == bytes.len() {
        let mut locations = rustel_core::Locations::new();
        locations.push((start + 1 + offset, end + 1 + offset));
        return Some((WordConstant::One(atom_value(first)), locations));
    }
    let mut after = if matches!(bytes[end], b' ' | b'\n' | b'\r' | b'\t' | 0xC2) {
        score_whitespace_end(bytes, end)
    } else {
        end
    };
    if after == bytes.len() {
        let at = leaf_place(src, offset, &Span { start, end });
        let mut locations = rustel_core::Locations::new();
        locations.push(at);
        return Some((WordConstant::One(atom_value(first)), locations));
    }
    if bytes[after] != b':' {
        return None;
    }
    let mut parts: [Value; 4] = [
        Value::Undefined,
        Value::Undefined,
        Value::Undefined,
        Value::Undefined,
    ];
    parts[0] = atom_value(first);
    let mut count = 1usize;
    let mut last_end = end;
    let mut spilled: Option<Vec<Value>> = None;
    while bytes.get(after) == Some(&b':') {
        let w_start = if matches!(
            bytes.get(after + 1),
            Some(b' ' | b'\n' | b'\r' | b'\t' | 0xC2)
        ) {
            score_whitespace_end(bytes, after + 1)
        } else {
            after + 1
        };
        let mut w_end = w_start;
        let mut seg_prev_dot = false;
        while w_end < bytes.len() {
            let b = bytes[w_end];
            if !ASCII_STEP_CHARS[usize::from(b)] {
                if b >= 128 {
                    while let Some(len) = word_char_len(bytes, w_end) {
                        w_end += len;
                    }
                    if src[w_start..w_end].contains("..") {
                        return None;
                    }
                }
                break;
            }
            if b == b'.' {
                if seg_prev_dot {
                    return None;
                }
                seg_prev_dot = true;
            } else {
                seg_prev_dot = false;
            }
            w_end += 1;
        }
        if w_end == w_start {
            return None;
        }
        let wb = bytes[w_start];
        if wb == b'^' || (w_end - w_start == 1 && matches!(wb, b'-' | b'~' | b'.' | b'_')) {
            return None;
        }
        let val = atom_value(&src[w_start..w_end]);
        if let Some(vec) = &mut spilled {
            vec.push(val);
        } else if count < parts.len() {
            parts[count] = val;
            count += 1;
        } else {
            let mut vec = Vec::with_capacity(8);
            for slot in &mut parts {
                vec.push(std::mem::replace(slot, Value::Undefined));
            }
            vec.push(val);
            spilled = Some(vec);
        }
        last_end = w_end;
        after = if matches!(bytes.get(w_end), Some(b' ' | b'\n' | b'\r' | b'\t' | 0xC2)) {
            score_whitespace_end(bytes, w_end)
        } else {
            w_end
        };
    }
    if after != bytes.len() {
        return None;
    }
    let at = if last_end == bytes.len() {
        (start + 1 + offset, last_end + 1 + offset)
    } else {
        leaf_place(
            src,
            offset,
            &Span {
                start,
                end: last_end,
            },
        )
    };
    let mut locations = rustel_core::Locations::new();
    locations.push(at);
    let constant = match spilled {
        Some(vec) => WordConstant::Many(std::sync::Arc::from(vec)),
        None => {
            let [p0, p1, p2, p3] = parts;
            match count {
                2 => WordConstant::Pair(p0, p1),
                3 => WordConstant::Many(std::sync::Arc::from([p0, p1, p2])),
                _ => WordConstant::Many(std::sync::Arc::from([p0, p1, p2, p3])),
            }
        }
    };
    Some((constant, locations))
}

/// Extract the value and location of a single-word or colon-separated compound
/// (`"bd:1"`, `"c:minor"`) mini literal, with zero graph or room allocation.
/// `None` for rests, multi-word patterns, or expressions using other operators.
pub fn word_constant_value(src: &str, offset: usize) -> Option<(Value, rustel_core::Locations)> {
    let (constant, locations) = word_constant_parts(src, offset)?;
    let value = match constant {
        WordConstant::One(v) => v,
        WordConstant::Pair(a, b) => Value::List(std::sync::Arc::from([a, b])),
        WordConstant::Many(list) => Value::List(list),
    };
    Some((value, locations))
}

/// The pattern of a text that is one word with whitespace around it: the
/// constant that the plans of the word make, read with no room and no
/// plan. `None` for any other text, and for the words that are no value:
/// a rest, and the two that the parser refuses.
///
/// Two literals of three in a score are one word. A reader that
/// remembers the graphs of its literals needs not remember this one: it
/// costs less to read than to keep.
pub fn word_constant(src: &str, offset: usize) -> Option<Pattern> {
    let (value, locations) = word_constant_value(src, offset)?;
    Some(rustel_core::pure_locations(value, locations))
}

/// Fast path for flat ASCII whitespace-separated equal sequences
/// (`"bd sd hh cp"`, `"c3 ~ e3 g3"`) and single-lane equal alternations
/// (`"<c3 e3 g3>"`), building `SmallGridStep` grids directly on integer ticks
/// with zero `Parser`, `Plans`, `Fraction`, or `SourceStep` allocations.
fn flat_sequence(src: &str, offset: usize) -> Option<Pattern> {
    flat_sequence_core(src, offset, None)
}

fn flat_sequence_core(
    src: &str,
    offset: usize,
    mut into: Option<&mut rustel_core::SequenceBuilder>,
) -> Option<Pattern> {
    if !src.is_ascii() || src.len() > usize::from(u16::MAX) {
        return None;
    }
    let bytes = src.as_bytes();
    let mut pos = score_whitespace_end(bytes, 0);
    if pos >= bytes.len() || bytes[pos] == b'^' {
        return None;
    }
    let mut limit = bytes.len();
    while limit > pos && matches!(bytes[limit - 1], b' ' | b'\t' | b'\r' | b'\n') {
        limit -= 1;
    }
    #[derive(Clone, Copy)]
    enum RateSuffix {
        None,
        Fast(u8, u16, u16),
        Slow(u8, u16, u16),
        Rep(u8),
    }
    let parse_rate_suffix = |at: usize, end_limit: usize| -> Option<(RateSuffix, usize)> {
        if at >= end_limit {
            return Some((RateSuffix::None, at));
        }
        let op = bytes[at];
        if op != b'*' && op != b'/' {
            return None;
        }
        let digit_start = at + 1;
        let mut digit_end = digit_start;
        let mut val = 0u32;
        while digit_end < end_limit && bytes[digit_end].is_ascii_digit() {
            val = val
                .checked_mul(10)?
                .checked_add(u32::from(bytes[digit_end] - b'0'))?;
            digit_end += 1;
        }
        if digit_end == digit_start || val == 0 {
            return None;
        }
        let next = score_whitespace_end(bytes, digit_end).min(end_limit);
        if next == digit_end && next < end_limit {
            return None;
        }
        let suffix = if op == b'*' {
            if val > 16 {
                return None;
            }
            RateSuffix::Fast(val as u8, digit_start as u16, digit_end as u16)
        } else {
            if val > 64 {
                return None;
            }
            RateSuffix::Slow(val as u8, digit_start as u16, digit_end as u16)
        };
        Some((suffix, next))
    };
    let parse_rep_suffix = |at: usize, end_limit: usize| -> Option<(usize, usize)> {
        if at >= end_limit || bytes[at] != b'!' {
            return None;
        }
        let digit_start = at + 1;
        let mut digit_end = digit_start;
        let mut val = 0usize;
        while digit_end < end_limit && bytes[digit_end].is_ascii_digit() {
            val = val
                .checked_mul(10)?
                .checked_add(usize::from(bytes[digit_end] - b'0'))?;
            digit_end += 1;
        }
        let reps = if digit_end == digit_start { 2 } else { val };
        if !(2..=64).contains(&reps) {
            return None;
        }
        let next = score_whitespace_end(bytes, digit_end).min(end_limit);
        if next == digit_end && next < end_limit {
            return None;
        }
        Some((reps, next))
    };
    let parse_weight_suffix = |at: usize, end_limit: usize| -> Option<(u8, usize)> {
        if at >= end_limit || bytes[at] != b'@' {
            return None;
        }
        let digit_start = at + 1;
        let mut digit_end = digit_start;
        let mut val = 0u16;
        while digit_end < end_limit && bytes[digit_end].is_ascii_digit() {
            val = val
                .checked_mul(10)?
                .checked_add(u16::from(bytes[digit_end] - b'0'))?;
            digit_end += 1;
        }
        let weight = if digit_end == digit_start { 2 } else { val };
        if !(1..=64).contains(&weight) {
            return None;
        }
        let next = score_whitespace_end(bytes, digit_end).min(end_limit);
        if next == digit_end && next < end_limit {
            return None;
        }
        Some((weight as u8, next))
    };
    let mut is_alt = false;
    let mut alt_suffix = RateSuffix::None;
    if bytes[pos] == b'<'
        && let Some(rel_close) = bytes[pos + 1..limit].iter().position(|&b| b == b'>')
    {
        let close = rel_close + pos + 1;
        if let Some((suffix, after)) = parse_rate_suffix(close + 1, limit)
            && after == limit
        {
            is_alt = true;
            alt_suffix = suffix;
            pos = score_whitespace_end(bytes, pos + 1);
            limit = close;
            while limit > pos && matches!(bytes[limit - 1], b' ' | b'\t' | b'\r' | b'\n') {
                limit -= 1;
            }
            if pos >= limit || bytes[pos] == b'^' {
                return None;
            }
        }
    }
    #[derive(Clone, Copy)]
    enum FlatItem {
        Word {
            start: u16,
            end: u16,
            suffix: RateSuffix,
            weight: u8,
        },
        Alt {
            first: u16,
            len: u16,
            suffix: RateSuffix,
        },
        Group {
            first: u16,
            len: u16,
            suffix: RateSuffix,
            multi_rep: bool,
        },
        Chord {
            first: u16,
            len: u16,
            suffix: RateSuffix,
        },
    }
    const EMPTY_ITEM: FlatItem = FlatItem::Word {
        start: 0,
        end: 0,
        suffix: RateSuffix::None,
        weight: 1,
    };
    let mut items_small;
    let mut items_large;
    let items: &mut [FlatItem] = if limit - pos <= 31 {
        items_small = [EMPTY_ITEM; 16];
        &mut items_small
    } else {
        items_large = [EMPTY_ITEM; 64];
        &mut items_large
    };
    let mut alt_small;
    let mut alt_large;
    let has_nested = bytes[pos..limit].iter().any(|&b| matches!(b, b'[' | b'<'));
    let alt_words: &mut [(u16, u16)] = if !has_nested {
        &mut []
    } else if limit - pos <= 31 && !bytes[pos..limit].contains(&b'!') {
        alt_small = [(0u16, 0u16); 16];
        &mut alt_small
    } else {
        alt_large = [(0u16, 0u16); 64];
        &mut alt_large
    };
    let mut alt_words_len = 0usize;
    let mut count = 0usize;
    let mut sounding = 0usize;
    let mut any_fast = false;
    let mut any_slow = false;
    let mut any_rep = false;
    let mut any_weight = false;
    let mut any_inner_alt = false;
    let mut any_group = false;
    while pos < limit {
        if (!is_alt && matches!(bytes[pos], b'<' | b'['))
            || (is_alt && !matches!(alt_suffix, RateSuffix::Slow(..)) && bytes[pos] == b'[')
        {
            let is_group = bytes[pos] == b'[';
            let close_ch = if is_group { b']' } else { b'>' };
            let close = bytes[pos + 1..limit].iter().position(|&b| b == close_ch)? + pos + 1;
            let next_after = score_whitespace_end(bytes, close + 1).min(limit);
            let (suffix, next_after) = if next_after == close + 1 && next_after < limit {
                if is_alt {
                    return None;
                }
                let (sfx, after) = parse_rate_suffix(close + 1, limit)?;
                if is_group && matches!(sfx, RateSuffix::Slow(..)) && (count != 0 || after != limit)
                {
                    return None;
                }
                (sfx, after)
            } else {
                (RateSuffix::None, next_after)
            };
            let mut inner_pos = score_whitespace_end(bytes, pos + 1);
            let mut inner_limit = close;
            while inner_limit > inner_pos
                && matches!(bytes[inner_limit - 1], b' ' | b'\t' | b'\r' | b'\n')
            {
                inner_limit -= 1;
            }
            if inner_pos >= inner_limit || bytes[inner_pos] == b'^' {
                return None;
            }
            let first = alt_words_len;
            if is_group && bytes[inner_pos..inner_limit].contains(&b',') {
                // Comma-separated chord of plain words (`[a2, a3]`, `[c3, e3, g3]`).
                if !matches!(suffix, RateSuffix::None) && (count != 0 || next_after != limit) {
                    return None;
                }
                loop {
                    let s = inner_pos;
                    let e = flat_word_end(src, bytes, s, inner_limit)?;
                    let word = &src[s..e];
                    if alt_words_len >= alt_words.len() {
                        return None;
                    }
                    alt_words[alt_words_len] = (s as u16, e as u16);
                    alt_words_len += 1;
                    if !matches!(word, "~" | "-") {
                        sounding += 1;
                    }
                    let after_word = score_whitespace_end(bytes, e).min(inner_limit);
                    if after_word >= inner_limit {
                        break;
                    }
                    if bytes[after_word] != b',' {
                        return None;
                    }
                    inner_pos = score_whitespace_end(bytes, after_word + 1).min(inner_limit);
                    if inner_pos >= inner_limit || bytes[inner_pos] == b'^' {
                        return None;
                    }
                }
                let chord_len = alt_words_len - first;
                if chord_len < 2 || count >= items.len() {
                    return None;
                }
                match suffix {
                    RateSuffix::None => {}
                    RateSuffix::Fast(..) => any_fast = true,
                    RateSuffix::Slow(..) => any_slow = true,
                    RateSuffix::Rep(..) => any_rep = true,
                }
                items[count] = FlatItem::Chord {
                    first: first as u16,
                    len: chord_len as u16,
                    suffix,
                };
                any_group = true;
                count += 1;
                pos = next_after;
                continue;
            }
            let mut token_count = 0usize;
            let mut has_rep = false;
            while inner_pos < inner_limit {
                let s = inner_pos;
                let e = flat_word_end(src, bytes, s, inner_limit)?;
                let word = &src[s..e];
                let next_inner = score_whitespace_end(bytes, e).min(inner_limit);
                let (reps, next_inner) = if next_inner == e && next_inner < inner_limit {
                    if !is_group || word.contains(':') {
                        return None;
                    }
                    has_rep = true;
                    parse_rep_suffix(e, inner_limit)?
                } else {
                    (1, next_inner)
                };
                if alt_words_len + reps > alt_words.len() {
                    return None;
                }
                for _ in 0..reps {
                    alt_words[alt_words_len] = (s as u16, e as u16);
                    alt_words_len += 1;
                }
                if !matches!(word, "~" | "-") {
                    sounding += reps;
                }
                token_count += 1;
                inner_pos = next_inner;
            }
            let inner_len = alt_words_len - first;
            if inner_len == 0 || count >= items.len() {
                return None;
            }
            let multi_rep = has_rep && token_count > 1;
            if multi_rep && !matches!(suffix, RateSuffix::None) {
                return None;
            }
            match suffix {
                RateSuffix::None => {}
                RateSuffix::Fast(..) => any_fast = true,
                RateSuffix::Slow(..) => any_slow = true,
                RateSuffix::Rep(..) => any_rep = true,
            }
            if is_group {
                items[count] = FlatItem::Group {
                    first: first as u16,
                    len: inner_len as u16,
                    suffix,
                    multi_rep,
                };
                any_group = true;
            } else {
                items[count] = FlatItem::Alt {
                    first: first as u16,
                    len: inner_len as u16,
                    suffix,
                };
                any_inner_alt = true;
            }
            count += 1;
            pos = next_after;
            continue;
        }
        let start = pos;
        let end = flat_word_end(src, bytes, start, limit)?;
        let word = &src[start..end];
        let next_pos = score_whitespace_end(bytes, end).min(limit);
        let (suffix, weight, next_pos) = if next_pos == end && next_pos < limit {
            if bytes[end] == b'!' {
                if is_alt || count > 0 {
                    return None;
                }
                let (reps, next_pos) = parse_rep_suffix(end, limit)?;
                if next_pos != limit {
                    return None;
                }
                (RateSuffix::Rep(reps as u8), 1u8, next_pos)
            } else if bytes[end] == b'@' {
                if word.contains(':') || (is_alt && !matches!(alt_suffix, RateSuffix::None)) {
                    return None;
                }
                let (w, next_pos) = parse_weight_suffix(end, limit)?;
                if w != 1 {
                    any_weight = true;
                }
                (RateSuffix::None, w, next_pos)
            } else {
                if word.contains(':') || (is_alt && !matches!(alt_suffix, RateSuffix::None)) {
                    return None;
                }
                let (sfx, next_pos) = parse_rate_suffix(end, limit)?;
                (sfx, 1u8, next_pos)
            }
        } else {
            (RateSuffix::None, 1u8, next_pos)
        };
        if count >= items.len() {
            return None;
        }
        match suffix {
            RateSuffix::None => {}
            RateSuffix::Fast(..) => any_fast = true,
            RateSuffix::Slow(..) => any_slow = true,
            RateSuffix::Rep(..) => any_rep = true,
        }
        items[count] = FlatItem::Word {
            start: start as u16,
            end: end as u16,
            suffix,
            weight,
        };
        count += 1;
        if !matches!(word, "~" | "-") {
            sounding += 1;
        }
        pos = next_pos;
    }
    let loc_of = |s: u16, e: u16| -> Option<(u32, u16)> {
        let (lb, le) = leaf_place(
            src,
            offset,
            &Span {
                start: usize::from(s),
                end: usize::from(e),
            },
        );
        let lb = u32::try_from(lb).ok()?;
        let ll = u16::try_from(le.checked_sub(lb as usize)?).ok()?;
        (ll < 255).then_some((lb, ll))
    };
    // Single `atom*N`, `atom/N`, `atom!N`, `[a b ...]`, or `[a b ...]*N` outside `<...>`.
    if !is_alt && count == 1 {
        let steps_meta = Some(Fraction::ONE);
        match items[0] {
            FlatItem::Word {
                start,
                end,
                suffix,
                weight,
            } => {
                if weight != 1 || matches!(suffix, RateSuffix::None) {
                    return None;
                }
                let word_steps = match suffix {
                    RateSuffix::Rep(reps) => Some(Fraction::int(i128::from(reps))),
                    _ => steps_meta,
                };
                if sounding == 0 {
                    return Some(if into.is_some() {
                        silence()
                    } else {
                        silence().with_steps(word_steps)
                    });
                }
                let word = &src[usize::from(start)..usize::from(end)];
                if let RateSuffix::Rep(reps) = suffix {
                    let loc_end = if word.contains(':') {
                        limit as u16
                    } else {
                        end
                    };
                    let (loc0_begin, loc0_len) = loc_of(start, loc_end)?;
                    if loc0_begin >= (1 << 31) || loc0_len >= 255 {
                        return None;
                    }
                    let reps_usize = usize::from(reps);
                    if reps_usize > 64 {
                        return None;
                    }
                    let mut raw_small;
                    let mut raw_large;
                    let raw_steps: &mut [(u16, u16, u16, u32, u16)] = if reps_usize <= 16 {
                        raw_small = [(0u16, 0u16, 0u16, 0u32, 0u16); 16];
                        &mut raw_small
                    } else {
                        raw_large = [(0u16, 0u16, 0u16, 0u32, 0u16); 64];
                        &mut raw_large
                    };
                    for (slot, dst) in raw_steps[..reps_usize].iter_mut().enumerate() {
                        *dst = (slot as u16, (slot + 1) as u16, 0, loc0_begin, loc0_len);
                    }
                    let layer_values: std::sync::Arc<[Value]> =
                        rustel_core::arc_single_value(flat_atom_value(word));
                    let ticks = std::num::NonZeroU32::new(u32::from(reps))?;
                    if let Some(builder) = into.as_deref_mut() {
                        return builder
                            .push_ticks(
                                Fraction::ONE,
                                ticks,
                                layer_values,
                                true,
                                &raw_steps[..reps_usize],
                            )
                            .then(silence);
                    }
                    let pattern = rustel_core::sequence_of_ticks(
                        Fraction::ONE,
                        ticks,
                        layer_values,
                        true,
                        &raw_steps[..reps_usize],
                    )?;
                    return Some(pattern.with_steps(word_steps));
                }
                let (loc0_begin, loc0_len) = loc_of(start, end)?;
                let (period, ticks, in_cycles, part_end, rate_s, rate_e) = match suffix {
                    RateSuffix::Fast(rate, rs, re) => (
                        Fraction::checked_new(1, i128::from(rate))?,
                        u32::from(rate),
                        true,
                        1u16,
                        rs,
                        re,
                    ),
                    RateSuffix::Slow(times, rs, re) => (
                        if times == 1 {
                            Fraction::ONE
                        } else {
                            Fraction::int(i128::from(times))
                        },
                        1u32,
                        times == 1,
                        u16::from(times),
                        rs,
                        re,
                    ),
                    RateSuffix::None | RateSuffix::Rep(..) => unreachable!(),
                };
                let (loc1_begin, loc1_len) = loc_of(rate_s, rate_e)?;
                if loc0_len == 0 || loc1_len == 0 {
                    return None;
                }
                let min_b = loc0_begin.min(loc1_begin);
                let max_b = loc0_begin.max(loc1_begin);
                if min_b >= (1 << 31) || max_b - min_b >= (1 << 22) {
                    return None;
                }
                let layer_values: std::sync::Arc<[Value]> =
                    rustel_core::arc_single_value(flat_atom_value(word));
                let step = [rustel_core::TickStep {
                    part_begin: 0,
                    part_end,
                    value: 0,
                    loc0_begin,
                    loc0_len,
                    loc1_begin,
                    loc1_len,
                }];
                let ticks = std::num::NonZeroU32::new(ticks)?;
                if let Some(builder) = into.as_deref_mut() {
                    return builder
                        .push_cut_ticks(period, ticks, layer_values, in_cycles, &step, &[])
                        .then(silence);
                }
                let pattern = rustel_core::sequence_of_cut_ticks(
                    period,
                    ticks,
                    layer_values,
                    in_cycles,
                    &step,
                    &[],
                )?;
                return Some(pattern.with_steps(steps_meta));
            }
            FlatItem::Group {
                first,
                len,
                suffix,
                multi_rep,
            } => {
                if multi_rep || (len < 2 && matches!(suffix, RateSuffix::None)) {
                    return None;
                }
                if sounding == 0 {
                    return Some(if into.is_some() {
                        silence()
                    } else {
                        silence().with_steps(steps_meta)
                    });
                }
                let (period, ticks, step_ticks, loc1_begin, loc1_len) = match suffix {
                    RateSuffix::None => (Fraction::ONE, u32::from(len), 1u32, 0u32, 0u16),
                    RateSuffix::Fast(rate, rs, re) => {
                        let ticks = u32::from(len).checked_mul(u32::from(rate))?;
                        let (b, l) = loc_of(rs, re)?;
                        if l == 0 {
                            return None;
                        }
                        (
                            Fraction::checked_new(1, i128::from(rate))?,
                            ticks,
                            1u32,
                            b,
                            l,
                        )
                    }
                    RateSuffix::Slow(times, rs, re) => {
                        let times_u32 = u32::from(times);
                        let (b, l) = loc_of(rs, re)?;
                        if l == 0 {
                            return None;
                        }
                        let mut ga = u32::from(len);
                        let mut gb = times_u32;
                        while gb != 0 {
                            let r = ga % gb;
                            ga = gb;
                            gb = r;
                        }
                        let step_ticks = times_u32 / ga;
                        let ticks = u32::from(len) / ga;
                        let end_tick = u32::from(len).checked_mul(step_ticks)?;
                        if end_tick > u32::from(u16::MAX) {
                            return None;
                        }
                        let period = if times == 1 {
                            Fraction::ONE
                        } else {
                            Fraction::int(i128::from(times))
                        };
                        (period, ticks, step_ticks, b, l)
                    }
                    RateSuffix::Rep(..) => return None,
                };
                const ZERO_TICK_STEP: rustel_core::TickStep = rustel_core::TickStep {
                    part_begin: 0,
                    part_end: 0,
                    value: 0,
                    loc0_begin: 0,
                    loc0_len: 0,
                    loc1_begin: 0,
                    loc1_len: 0,
                };
                let mut seen_small;
                let mut seen_large;
                let seen_words: &mut [(u16, u16, u32)] = if usize::from(len) <= 16 {
                    seen_small = [(0u16, 0u16, 0u32); 16];
                    &mut seen_small
                } else {
                    seen_large = [(0u16, 0u16, 0u32); 64];
                    &mut seen_large
                };
                let mut seen_len = 0usize;
                let mut raw_small;
                let mut raw_large;
                let raw_steps: &mut [rustel_core::TickStep] = if usize::from(len) <= 16 {
                    raw_small = [ZERO_TICK_STEP; 16];
                    &mut raw_small
                } else {
                    raw_large = [ZERO_TICK_STEP; 64];
                    &mut raw_large
                };
                let mut step_len = 0usize;
                let mut min_begin = u32::MAX;
                let mut max_begin = 0u32;
                let mut in_cycles = true;
                if loc1_len > 0 {
                    min_begin = min_begin.min(loc1_begin);
                    max_begin = max_begin.max(loc1_begin);
                }
                for k in 0..usize::from(len) {
                    let (start, end) = alt_words[usize::from(first) + k];
                    let s = usize::from(start);
                    let e = usize::from(end);
                    if e - s == 1 && matches!(bytes[s], b'~' | b'-') {
                        continue;
                    }
                    let value_idx = intern_flat_word(bytes, seen_words, &mut seen_len, s, e)?;
                    let (loc0_begin, loc0_len) = loc_of(start, end)?;
                    if loc1_len > 0 && loc0_len == 0 {
                        return None;
                    }
                    if loc0_len > 0 {
                        min_begin = min_begin.min(loc0_begin);
                        max_begin = max_begin.max(loc0_begin);
                    }
                    if step_len >= raw_steps.len() {
                        return None;
                    }
                    let pb = (k as u32) * step_ticks;
                    let pe = pb + step_ticks;
                    if step_ticks > 1 && pb / ticks != (pe - 1) / ticks {
                        in_cycles = false;
                    }
                    raw_steps[step_len] = rustel_core::TickStep {
                        part_begin: pb as u16,
                        part_end: pe as u16,
                        value: value_idx,
                        loc0_begin,
                        loc0_len,
                        loc1_begin,
                        loc1_len,
                    };
                    step_len += 1;
                }
                let base = if min_begin == u32::MAX { 0 } else { min_begin };
                if base >= (1 << 31) || (min_begin != u32::MAX && max_begin - base >= (1 << 22)) {
                    return None;
                }
                let layer_values: std::sync::Arc<[Value]> = seen_words[..seen_len]
                    .iter()
                    .map(|&(ws, we, _)| flat_atom_value(&src[usize::from(ws)..usize::from(we)]))
                    .collect();
                let ticks = std::num::NonZeroU32::new(ticks)?;
                if let Some(builder) = into.as_deref_mut() {
                    return builder
                        .push_cut_ticks(
                            period,
                            ticks,
                            layer_values,
                            in_cycles,
                            &raw_steps[..step_len],
                            &[],
                        )
                        .then(silence);
                }
                let pattern = rustel_core::sequence_of_cut_ticks(
                    period,
                    ticks,
                    layer_values,
                    in_cycles,
                    &raw_steps[..step_len],
                    &[],
                )?;
                return Some(pattern.with_steps(steps_meta));
            }
            FlatItem::Chord { first, len, suffix } => {
                if !matches!(suffix, RateSuffix::None) {
                    return None;
                }
                if sounding == 0 {
                    return Some(if into.is_some() {
                        silence()
                    } else {
                        silence().with_steps(steps_meta)
                    });
                }
                let mut seen_small;
                let mut seen_large;
                let seen_words: &mut [(u16, u16, u32)] = if usize::from(len) <= 16 {
                    seen_small = [(0u16, 0u16, 0u32); 16];
                    &mut seen_small
                } else {
                    seen_large = [(0u16, 0u16, 0u32); 64];
                    &mut seen_large
                };
                let mut seen_len = 0usize;
                let mut layer_small;
                let mut layer_large;
                let layer_steps: &mut [(u16, u32, u16)] = if usize::from(len) <= 16 {
                    layer_small = [(0u16, 0u32, 0u16); 16];
                    &mut layer_small
                } else {
                    layer_large = [(0u16, 0u32, 0u16); 64];
                    &mut layer_large
                };
                let mut layer_len = 0usize;
                for k in 0..usize::from(len) {
                    let (start, end) = alt_words[usize::from(first) + k];
                    let s = usize::from(start);
                    let e = usize::from(end);
                    if e - s == 1 && matches!(bytes[s], b'~' | b'-') {
                        continue;
                    }
                    let value_idx = intern_flat_word(bytes, seen_words, &mut seen_len, s, e)?;
                    let (loc0_begin, loc0_len) = loc_of(start, end)?;
                    if loc0_begin >= (1 << 31) || loc0_len >= 255 || layer_len >= layer_steps.len()
                    {
                        return None;
                    }
                    layer_steps[layer_len] = (value_idx, loc0_begin, loc0_len);
                    layer_len += 1;
                }
                let layer_values: std::sync::Arc<[Value]> = seen_words[..seen_len]
                    .iter()
                    .map(|&(ws, we, _)| flat_atom_value(&src[usize::from(ws)..usize::from(we)]))
                    .collect();
                let ticks = std::num::NonZeroU32::new(1).expect("non-zero");
                if let Some(builder) = into.as_deref_mut() {
                    for &(value_idx, loc0_begin, loc0_len) in &layer_steps[..layer_len] {
                        let step = [(0u16, 1u16, value_idx, loc0_begin, loc0_len)];
                        if !builder.push_ticks_plain(
                            Fraction::ONE,
                            ticks,
                            std::sync::Arc::clone(&layer_values),
                            true,
                            &step,
                        ) {
                            return None;
                        }
                    }
                    return Some(silence());
                }
                let mut builder = rustel_core::SequenceBuilder::with_capacity(layer_len);
                for &(value_idx, loc0_begin, loc0_len) in &layer_steps[..layer_len] {
                    let step = [(0u16, 1u16, value_idx, loc0_begin, loc0_len)];
                    if !builder.push_ticks(
                        Fraction::ONE,
                        ticks,
                        std::sync::Arc::clone(&layer_values),
                        true,
                        &step,
                    ) {
                        return None;
                    }
                }
                return Some(builder.finish()?.with_steps(steps_meta));
            }
            FlatItem::Alt { .. } => return None,
        }
    }
    if count < 2 || any_rep {
        return None;
    }
    let total_weight: u32 = if any_weight {
        let mut sum = 0u32;
        for &item in &items[..count] {
            let w = match item {
                FlatItem::Word { weight, .. } => u32::from(weight),
                _ => 1,
            };
            sum = sum.checked_add(w)?;
        }
        if sum == 0 || sum > 256 {
            return None;
        }
        sum
    } else {
        count as u32
    };
    let steps_meta = if is_alt {
        Some(Fraction::ONE)
    } else {
        Some(Fraction::int(i128::from(total_weight)))
    };
    if sounding == 0 {
        return Some(if into.is_some() {
            silence()
        } else {
            silence().with_steps(steps_meta)
        });
    }
    // Plain flat sequence or plain `<a b ...>` (optionally with `@N` weights)
    // and no `*N` / `/N` suffix, inner `<...>`, or `[...]`.
    if !any_fast
        && !any_slow
        && !any_inner_alt
        && !any_group
        && matches!(alt_suffix, RateSuffix::None)
    {
        let mut seen_small;
        let mut seen_large;
        let seen_words: &mut [(u16, u16, u32)] = if sounding <= 16 {
            seen_small = [(0u16, 0u16, 0u32); 16];
            &mut seen_small
        } else {
            seen_large = [(0u16, 0u16, 0u32); 64];
            &mut seen_large
        };
        let mut seen_len = 0usize;
        let mut raw_small;
        let mut raw_large;
        let raw_steps: &mut [(u16, u16, u16, u32, u16)] = if sounding <= 16 {
            raw_small = [(0u16, 0u16, 0u16, 0u32, 0u16); 16];
            &mut raw_small
        } else {
            raw_large = [(0u16, 0u16, 0u16, 0u32, 0u16); 64];
            &mut raw_large
        };
        let mut step_len = 0usize;
        let mut min_begin = u32::MAX;
        let mut max_begin = 0u32;
        let mut max_len = 0u16;
        let mut cursor = 0u32;
        for &item in &items[..count] {
            let FlatItem::Word {
                start, end, weight, ..
            } = item
            else {
                unreachable!()
            };
            let pb = cursor;
            let pe = pb + u32::from(weight);
            cursor = pe;
            let s = usize::from(start);
            let e = usize::from(end);
            if e - s == 1 && matches!(bytes[s], b'~' | b'-') {
                continue;
            }
            let value_idx = intern_flat_word(bytes, seen_words, &mut seen_len, s, e)?;
            let (loc_begin, loc_len) = loc_of(start, end)?;
            if loc_len > 0 {
                min_begin = min_begin.min(loc_begin);
                max_begin = max_begin.max(loc_begin);
                max_len = max_len.max(loc_len);
            }
            raw_steps[step_len] = (pb as u16, pe as u16, value_idx, loc_begin, loc_len);
            step_len += 1;
        }
        let base = if min_begin == u32::MAX { 0 } else { min_begin };
        if base >= (1 << 31)
            || (min_begin != u32::MAX && max_begin - base >= (1 << 22))
            || max_len >= 255
        {
            return None;
        }
        let layer_values: std::sync::Arc<[Value]> = seen_words[..seen_len]
            .iter()
            .map(|&(ws, we, _)| flat_atom_value(&src[usize::from(ws)..usize::from(we)]))
            .collect();
        let (period, ticks) = if is_alt {
            (
                Fraction::int(i128::from(total_weight)),
                std::num::NonZeroU32::new(1).expect("non-zero"),
            )
        } else {
            (
                Fraction::ONE,
                std::num::NonZeroU32::new(total_weight).expect("total_weight >= 2"),
            )
        };
        if any_weight {
            // Weighted sequences (`timecat`) stack one layer per item so even
            // unsorted multi-cycle queries match `Rhythm::squeezed` order.
            if let Some(builder) = into.as_deref_mut() {
                for &(pb, pe, value_idx, loc_begin, loc_len) in &raw_steps[..step_len] {
                    let step_in_cycles = !is_alt || pe - pb <= 1;
                    if !builder.push_ticks_plain(
                        period,
                        ticks,
                        std::sync::Arc::clone(&layer_values),
                        step_in_cycles,
                        &[(pb, pe, value_idx, loc_begin, loc_len)],
                    ) {
                        return None;
                    }
                }
                return Some(silence());
            }
            let mut builder = rustel_core::SequenceBuilder::with_capacity(step_len);
            for &(pb, pe, value_idx, loc_begin, loc_len) in &raw_steps[..step_len] {
                let step_in_cycles = !is_alt || pe - pb <= 1;
                if !builder.push_ticks_plain(
                    period,
                    ticks,
                    std::sync::Arc::clone(&layer_values),
                    step_in_cycles,
                    &[(pb, pe, value_idx, loc_begin, loc_len)],
                ) {
                    return None;
                }
            }
            return Some(builder.finish()?.with_steps(steps_meta));
        }
        if let Some(builder) = into.as_deref_mut() {
            return builder
                .push_ticks_plain(period, ticks, layer_values, true, &raw_steps[..step_len])
                .then(silence);
        }
        let pattern = rustel_core::sequence_of_ticks_plain(
            period,
            ticks,
            layer_values,
            true,
            &raw_steps[..step_len],
        )?;
        return Some(pattern.with_steps(steps_meta));
    }
    if any_weight {
        return None;
    }
    // `<a b ...>*N`, `<a b ...>/N`, or equal sequence with `atom*N`, `atom/N`, `<a b ...>`, or `[a b ...]`.
    if is_alt && any_slow {
        return None;
    }
    let mut seen_words: [(u16, u16, u32); 64] = [(0, 0, 0); 64];
    let mut seen_len = 0usize;
    let mut raw_steps = [rustel_core::TickStep {
        part_begin: 0,
        part_end: 0,
        value: 0,
        loc0_begin: 0,
        loc0_len: 0,
        loc1_begin: 0,
        loc1_len: 0,
    }; 64];
    let mut step_len = 0usize;
    let mut cuts_storage;
    let cuts: &mut [rustel_core::TickCut] = if any_slow {
        cuts_storage = [rustel_core::TickCut {
            index: 0,
            whole_begin: 0,
            whole_end: 0,
        }; 64];
        &mut cuts_storage
    } else {
        &mut []
    };
    let mut cut_len = 0usize;
    let mut min_begin = u32::MAX;
    let mut max_begin = 0u32;
    let (period, ticks, in_cycles) = if is_alt && !matches!(alt_suffix, RateSuffix::None) {
        let mut sub_ticks = 1u32;
        if any_group {
            for &item in &items[..count] {
                if let FlatItem::Group { len, .. } = item {
                    let g = u32::from(len);
                    let mut a = sub_ticks;
                    let mut b = g;
                    while b != 0 {
                        let r = a % b;
                        a = b;
                        b = r;
                    }
                    sub_ticks = (sub_ticks / a).checked_mul(g)?;
                    if sub_ticks > 256 {
                        return None;
                    }
                }
            }
        }
        let (period, ticks, in_cycles, mul, rs, re) = match alt_suffix {
            RateSuffix::Fast(rate, rs, re) => {
                let rate = u32::from(rate);
                let ticks = rate.checked_mul(sub_ticks)?;
                let total_ticks = (count as u32).checked_mul(sub_ticks)?;
                if total_ticks > u32::from(u16::MAX) {
                    return None;
                }
                (
                    Fraction::checked_new(count as i128, i128::from(rate))?,
                    ticks,
                    true,
                    sub_ticks,
                    rs,
                    re,
                )
            }
            RateSuffix::Slow(times, rs, re) => {
                let times_u32 = u32::from(times);
                let p = (count as u32).checked_mul(times_u32)?;
                if p > u32::from(u16::MAX) {
                    return None;
                }
                (
                    Fraction::int(i128::from(p)),
                    1u32,
                    times == 1,
                    times_u32,
                    rs,
                    re,
                )
            }
            RateSuffix::None | RateSuffix::Rep(..) => unreachable!(),
        };
        let (loc1_begin, loc1_len) = loc_of(rs, re)?;
        if loc1_len == 0 {
            return None;
        }
        min_begin = min_begin.min(loc1_begin);
        max_begin = max_begin.max(loc1_begin);
        for (slot, &item) in items[..count].iter().enumerate() {
            let slot_pb = (slot as u32) * mul;
            match item {
                FlatItem::Word { start, end, .. } => {
                    let s = usize::from(start);
                    let e = usize::from(end);
                    if e - s == 1 && matches!(bytes[s], b'~' | b'-') {
                        continue;
                    }
                    let value_idx = intern_flat_word(bytes, &mut seen_words, &mut seen_len, s, e)?;
                    let (loc0_begin, loc0_len) = loc_of(start, end)?;
                    if loc0_len == 0 || step_len >= raw_steps.len() {
                        return None;
                    }
                    min_begin = min_begin.min(loc0_begin);
                    max_begin = max_begin.max(loc0_begin);
                    raw_steps[step_len] = rustel_core::TickStep {
                        part_begin: slot_pb as u16,
                        part_end: (slot_pb + mul) as u16,
                        value: value_idx,
                        loc0_begin,
                        loc0_len,
                        loc1_begin,
                        loc1_len,
                    };
                    step_len += 1;
                }
                FlatItem::Group { first, len, .. } => {
                    let w = mul / u32::from(len);
                    for k in 0..usize::from(len) {
                        let (start, end) = alt_words[usize::from(first) + k];
                        let s = usize::from(start);
                        let e = usize::from(end);
                        if e - s == 1 && matches!(bytes[s], b'~' | b'-') {
                            continue;
                        }
                        let value_idx =
                            intern_flat_word(bytes, &mut seen_words, &mut seen_len, s, e)?;
                        let (loc0_begin, loc0_len) = loc_of(start, end)?;
                        if loc0_len == 0 || step_len >= raw_steps.len() {
                            return None;
                        }
                        min_begin = min_begin.min(loc0_begin);
                        max_begin = max_begin.max(loc0_begin);
                        let pb = slot_pb + (k as u32) * w;
                        raw_steps[step_len] = rustel_core::TickStep {
                            part_begin: pb as u16,
                            part_end: (pb + w) as u16,
                            value: value_idx,
                            loc0_begin,
                            loc0_len,
                            loc1_begin,
                            loc1_len,
                        };
                        step_len += 1;
                    }
                }
                FlatItem::Chord { first, len, .. } => {
                    for k in 0..usize::from(len) {
                        let (start, end) = alt_words[usize::from(first) + k];
                        let s = usize::from(start);
                        let e = usize::from(end);
                        if e - s == 1 && matches!(bytes[s], b'~' | b'-') {
                            continue;
                        }
                        let value_idx =
                            intern_flat_word(bytes, &mut seen_words, &mut seen_len, s, e)?;
                        let (loc0_begin, loc0_len) = loc_of(start, end)?;
                        if loc0_len == 0 || step_len >= raw_steps.len() {
                            return None;
                        }
                        min_begin = min_begin.min(loc0_begin);
                        max_begin = max_begin.max(loc0_begin);
                        raw_steps[step_len] = rustel_core::TickStep {
                            part_begin: slot_pb as u16,
                            part_end: (slot_pb + mul) as u16,
                            value: value_idx,
                            loc0_begin,
                            loc0_len,
                            loc1_begin,
                            loc1_len,
                        };
                        step_len += 1;
                    }
                }
                FlatItem::Alt { .. } => unreachable!(),
            }
        }
        (period, ticks, in_cycles)
    } else {
        if !is_alt && count > 16 {
            return None;
        }
        let gcd_u32 = |mut a: u32, mut b: u32| {
            while b != 0 {
                let r = a % b;
                a = b;
                b = r;
            }
            a
        };
        let mut period_cycles = if is_alt { count as u32 } else { 1u32 };
        let mut sub_ticks = 1u32;
        for &item in &items[..count] {
            match item {
                FlatItem::Word {
                    start, end, suffix, ..
                } => {
                    if matches!(&src[usize::from(start)..usize::from(end)], "~" | "-") {
                        continue;
                    }
                    match suffix {
                        RateSuffix::None => {}
                        RateSuffix::Fast(rate, _, _) => {
                            let rate = u32::from(rate);
                            sub_ticks = (sub_ticks / gcd_u32(sub_ticks, rate)).checked_mul(rate)?;
                            if sub_ticks > 256 {
                                return None;
                            }
                        }
                        RateSuffix::Slow(times, _, _) => {
                            let times = u32::from(times);
                            period_cycles = (period_cycles / gcd_u32(period_cycles, times))
                                .checked_mul(times)?;
                            if period_cycles > 64 {
                                return None;
                            }
                        }
                        RateSuffix::Rep(..) => return None,
                    }
                }
                FlatItem::Alt { len, suffix, .. } => {
                    let p = u32::from(len);
                    match suffix {
                        RateSuffix::None => {
                            period_cycles =
                                (period_cycles / gcd_u32(period_cycles, p)).checked_mul(p)?;
                        }
                        RateSuffix::Slow(times, _, _) => {
                            let pt = p.checked_mul(u32::from(times))?;
                            period_cycles =
                                (period_cycles / gcd_u32(period_cycles, pt)).checked_mul(pt)?;
                        }
                        RateSuffix::Fast(rate, _, _) => {
                            let rate = u32::from(rate);
                            let step_p = p / gcd_u32(p, rate);
                            period_cycles = (period_cycles / gcd_u32(period_cycles, step_p))
                                .checked_mul(step_p)?;
                            sub_ticks = (sub_ticks / gcd_u32(sub_ticks, rate)).checked_mul(rate)?;
                        }
                        RateSuffix::Rep(..) => return None,
                    }
                    if period_cycles > 64 || sub_ticks > 256 {
                        return None;
                    }
                }
                FlatItem::Group { len, suffix, .. } => {
                    let g = match suffix {
                        RateSuffix::None => u32::from(len),
                        RateSuffix::Fast(rate, _, _) => {
                            u32::from(len).checked_mul(u32::from(rate))?
                        }
                        RateSuffix::Slow(..) | RateSuffix::Rep(..) => return None,
                    };
                    sub_ticks = (sub_ticks / gcd_u32(sub_ticks, g)).checked_mul(g)?;
                    if sub_ticks > 256 {
                        return None;
                    }
                }
                FlatItem::Chord { suffix, .. } => {
                    if !matches!(suffix, RateSuffix::None) {
                        return None;
                    }
                }
            }
        }
        let ticks = if is_alt {
            sub_ticks
        } else {
            (count as u32).checked_mul(sub_ticks)?
        };
        let total_ticks = period_cycles.checked_mul(ticks)?;
        if total_ticks > u32::from(u16::MAX) {
            return None;
        }
        let outer_iters = if is_alt { 1u32 } else { period_cycles };
        for t in 0..outer_iters {
            for (slot, &item) in items[..count].iter().enumerate() {
                let slot_start = if is_alt {
                    (slot as u32) * sub_ticks
                } else {
                    (t * (count as u32) + (slot as u32)) * sub_ticks
                };
                match item {
                    FlatItem::Word {
                        start, end, suffix, ..
                    } => {
                        let s = usize::from(start);
                        let e = usize::from(end);
                        if e - s == 1 && matches!(bytes[s], b'~' | b'-') {
                            continue;
                        }
                        let value_idx =
                            intern_flat_word(bytes, &mut seen_words, &mut seen_len, s, e)?;
                        let (loc0_begin, loc0_len) = loc_of(start, end)?;
                        if loc0_len > 0 {
                            min_begin = min_begin.min(loc0_begin);
                            max_begin = max_begin.max(loc0_begin);
                        }
                        match suffix {
                            RateSuffix::None => {
                                if step_len >= raw_steps.len() {
                                    return None;
                                }
                                raw_steps[step_len] = rustel_core::TickStep {
                                    part_begin: slot_start as u16,
                                    part_end: (slot_start + sub_ticks) as u16,
                                    value: value_idx,
                                    loc0_begin,
                                    loc0_len,
                                    loc1_begin: 0,
                                    loc1_len: 0,
                                };
                                step_len += 1;
                            }
                            RateSuffix::Fast(rate, rs, re) => {
                                let rate = u32::from(rate);
                                let (loc1_begin, loc1_len) = loc_of(rs, re)?;
                                if loc0_len == 0 || loc1_len == 0 {
                                    return None;
                                }
                                min_begin = min_begin.min(loc1_begin);
                                max_begin = max_begin.max(loc1_begin);
                                let w = sub_ticks / rate;
                                for r in 0..rate {
                                    if step_len >= raw_steps.len() {
                                        return None;
                                    }
                                    let pb = slot_start + r * w;
                                    raw_steps[step_len] = rustel_core::TickStep {
                                        part_begin: pb as u16,
                                        part_end: (pb + w) as u16,
                                        value: value_idx,
                                        loc0_begin,
                                        loc0_len,
                                        loc1_begin,
                                        loc1_len,
                                    };
                                    step_len += 1;
                                }
                            }
                            RateSuffix::Slow(times, rs, re) => {
                                let times = u32::from(times);
                                let (loc1_begin, loc1_len) = loc_of(rs, re)?;
                                if loc0_len == 0 || loc1_len == 0 || step_len >= raw_steps.len() {
                                    return None;
                                }
                                min_begin = min_begin.min(loc1_begin);
                                max_begin = max_begin.max(loc1_begin);
                                let rem = t % times;
                                if times > 1 {
                                    let wb = i64::from(slot_start)
                                        - i64::from(rem) * i64::from(sub_ticks);
                                    let we = i64::from(slot_start)
                                        + i64::from(times - rem) * i64::from(sub_ticks);
                                    cuts[cut_len] = rustel_core::TickCut {
                                        index: step_len as u16,
                                        whole_begin: u16::try_from(wb).ok()?,
                                        whole_end: u16::try_from(we).ok()?,
                                    };
                                    cut_len += 1;
                                }
                                raw_steps[step_len] = rustel_core::TickStep {
                                    part_begin: slot_start as u16,
                                    part_end: (slot_start + sub_ticks) as u16,
                                    value: value_idx,
                                    loc0_begin,
                                    loc0_len,
                                    loc1_begin,
                                    loc1_len,
                                };
                                step_len += 1;
                            }
                            RateSuffix::Rep(..) => return None,
                        }
                    }
                    FlatItem::Alt { first, len, suffix } => match suffix {
                        RateSuffix::None => {
                            let (start, end) =
                                alt_words[usize::from(first) + (t as usize) % usize::from(len)];
                            let s = usize::from(start);
                            let e = usize::from(end);
                            if e - s == 1 && matches!(bytes[s], b'~' | b'-') {
                                continue;
                            }
                            let value_idx =
                                intern_flat_word(bytes, &mut seen_words, &mut seen_len, s, e)?;
                            let (loc0_begin, loc0_len) = loc_of(start, end)?;
                            if loc0_len > 0 {
                                min_begin = min_begin.min(loc0_begin);
                                max_begin = max_begin.max(loc0_begin);
                            }
                            if step_len >= raw_steps.len() {
                                return None;
                            }
                            raw_steps[step_len] = rustel_core::TickStep {
                                part_begin: slot_start as u16,
                                part_end: (slot_start + sub_ticks) as u16,
                                value: value_idx,
                                loc0_begin,
                                loc0_len,
                                loc1_begin: 0,
                                loc1_len: 0,
                            };
                            step_len += 1;
                        }
                        RateSuffix::Slow(times, rs, re) => {
                            let times = u32::from(times);
                            let inner_cycle = (t / times) as usize;
                            let rem = t % times;
                            let (start, end) =
                                alt_words[usize::from(first) + inner_cycle % usize::from(len)];
                            let s = usize::from(start);
                            let e = usize::from(end);
                            if e - s == 1 && matches!(bytes[s], b'~' | b'-') {
                                continue;
                            }
                            let value_idx =
                                intern_flat_word(bytes, &mut seen_words, &mut seen_len, s, e)?;
                            let (loc0_begin, loc0_len) = loc_of(start, end)?;
                            let (loc1_begin, loc1_len) = loc_of(rs, re)?;
                            if loc0_len == 0 || loc1_len == 0 || step_len >= raw_steps.len() {
                                return None;
                            }
                            min_begin = min_begin.min(loc0_begin).min(loc1_begin);
                            max_begin = max_begin.max(loc0_begin).max(loc1_begin);
                            if times > 1 {
                                let wb =
                                    i64::from(slot_start) - i64::from(rem) * i64::from(sub_ticks);
                                let we = i64::from(slot_start)
                                    + i64::from(times - rem) * i64::from(sub_ticks);
                                cuts[cut_len] = rustel_core::TickCut {
                                    index: step_len as u16,
                                    whole_begin: u16::try_from(wb).ok()?,
                                    whole_end: u16::try_from(we).ok()?,
                                };
                                cut_len += 1;
                            }
                            raw_steps[step_len] = rustel_core::TickStep {
                                part_begin: slot_start as u16,
                                part_end: (slot_start + sub_ticks) as u16,
                                value: value_idx,
                                loc0_begin,
                                loc0_len,
                                loc1_begin,
                                loc1_len,
                            };
                            step_len += 1;
                        }
                        RateSuffix::Fast(rate, rs, re) => {
                            let rate = u32::from(rate);
                            let (loc1_begin, loc1_len) = loc_of(rs, re)?;
                            if loc1_len == 0 {
                                return None;
                            }
                            let w = sub_ticks / rate;
                            for r in 0..rate {
                                let (start, end) = alt_words[usize::from(first)
                                    + ((t * rate + r) as usize) % usize::from(len)];
                                let s = usize::from(start);
                                let e = usize::from(end);
                                if e - s == 1 && matches!(bytes[s], b'~' | b'-') {
                                    continue;
                                }
                                let value_idx =
                                    intern_flat_word(bytes, &mut seen_words, &mut seen_len, s, e)?;
                                let (loc0_begin, loc0_len) = loc_of(start, end)?;
                                if loc0_len == 0 || step_len >= raw_steps.len() {
                                    return None;
                                }
                                min_begin = min_begin.min(loc0_begin).min(loc1_begin);
                                max_begin = max_begin.max(loc0_begin).max(loc1_begin);
                                let pb = slot_start + r * w;
                                raw_steps[step_len] = rustel_core::TickStep {
                                    part_begin: pb as u16,
                                    part_end: (pb + w) as u16,
                                    value: value_idx,
                                    loc0_begin,
                                    loc0_len,
                                    loc1_begin,
                                    loc1_len,
                                };
                                step_len += 1;
                            }
                        }
                        RateSuffix::Rep(..) => return None,
                    },
                    FlatItem::Group {
                        first, len, suffix, ..
                    } => {
                        let (rate, loc1_begin, loc1_len) = match suffix {
                            RateSuffix::None => (1u32, 0u32, 0u16),
                            RateSuffix::Fast(rate, rs, re) => {
                                let (b, l) = loc_of(rs, re)?;
                                if l == 0 {
                                    return None;
                                }
                                (u32::from(rate), b, l)
                            }
                            RateSuffix::Slow(..) | RateSuffix::Rep(..) => return None,
                        };
                        let w = sub_ticks / (u32::from(len) * rate);
                        for r in 0..rate {
                            for k in 0..usize::from(len) {
                                let (start, end) = alt_words[usize::from(first) + k];
                                let s = usize::from(start);
                                let e = usize::from(end);
                                if e - s == 1 && matches!(bytes[s], b'~' | b'-') {
                                    continue;
                                }
                                let value_idx =
                                    intern_flat_word(bytes, &mut seen_words, &mut seen_len, s, e)?;
                                let (loc0_begin, loc0_len) = loc_of(start, end)?;
                                if loc1_len > 0 {
                                    if loc0_len == 0 {
                                        return None;
                                    }
                                    min_begin = min_begin.min(loc0_begin).min(loc1_begin);
                                    max_begin = max_begin.max(loc0_begin).max(loc1_begin);
                                } else if loc0_len > 0 {
                                    min_begin = min_begin.min(loc0_begin);
                                    max_begin = max_begin.max(loc0_begin);
                                }
                                if step_len >= raw_steps.len() {
                                    return None;
                                }
                                let pb = slot_start + (r * u32::from(len) + (k as u32)) * w;
                                raw_steps[step_len] = rustel_core::TickStep {
                                    part_begin: pb as u16,
                                    part_end: (pb + w) as u16,
                                    value: value_idx,
                                    loc0_begin,
                                    loc0_len,
                                    loc1_begin,
                                    loc1_len,
                                };
                                step_len += 1;
                            }
                        }
                    }
                    FlatItem::Chord { first, len, .. } => {
                        for k in 0..usize::from(len) {
                            let (start, end) = alt_words[usize::from(first) + k];
                            let s = usize::from(start);
                            let e = usize::from(end);
                            if e - s == 1 && matches!(bytes[s], b'~' | b'-') {
                                continue;
                            }
                            let value_idx =
                                intern_flat_word(bytes, &mut seen_words, &mut seen_len, s, e)?;
                            let (loc0_begin, loc0_len) = loc_of(start, end)?;
                            if loc0_len > 0 {
                                min_begin = min_begin.min(loc0_begin);
                                max_begin = max_begin.max(loc0_begin);
                            }
                            if step_len >= raw_steps.len() {
                                return None;
                            }
                            raw_steps[step_len] = rustel_core::TickStep {
                                part_begin: slot_start as u16,
                                part_end: (slot_start + sub_ticks) as u16,
                                value: value_idx,
                                loc0_begin,
                                loc0_len,
                                loc1_begin: 0,
                                loc1_len: 0,
                            };
                            step_len += 1;
                        }
                    }
                }
            }
        }
        let period = if period_cycles == 1 {
            Fraction::ONE
        } else {
            Fraction::int(i128::from(period_cycles))
        };
        (period, ticks, true)
    };
    let base = if min_begin == u32::MAX { 0 } else { min_begin };
    if base >= (1 << 31) || (min_begin != u32::MAX && max_begin - base >= (1 << 22)) {
        return None;
    }
    let layer_values: std::sync::Arc<[Value]> = seen_words[..seen_len]
        .iter()
        .map(|&(s, e, _)| flat_atom_value(&src[usize::from(s)..usize::from(e)]))
        .collect();
    let ticks = std::num::NonZeroU32::new(ticks)?;
    if let Some(builder) = into {
        return builder
            .push_cut_ticks(
                period,
                ticks,
                layer_values,
                in_cycles,
                &raw_steps[..step_len],
                &cuts[..cut_len],
            )
            .then(silence);
    }
    let pattern = rustel_core::sequence_of_cut_ticks(
        period,
        ticks,
        layer_values,
        in_cycles,
        &raw_steps[..step_len],
        &cuts[..cut_len],
    )?;
    Some(pattern.with_steps(steps_meta))
}

/// Fast path for a single plain ASCII Euclidean rhythm (`"sd(3,8)"`, `"hh(7,16,2)"`)
/// directly on stack-allocated `SourceStep`s, skipping `Room`, `Parser`, and `Plans`.
fn flat_euclid(src: &str, offset: usize) -> Option<Pattern> {
    flat_euclid_core(src, offset, None)
}

fn flat_euclid_core(
    src: &str,
    offset: usize,
    into: Option<&mut rustel_core::SequenceBuilder>,
) -> Option<Pattern> {
    if !src.is_ascii() {
        return None;
    }
    let bytes = src.as_bytes();
    let start = score_whitespace_end(bytes, 0);
    if start >= bytes.len() || bytes[start] == b'^' {
        return None;
    }
    let mut limit = bytes.len();
    while limit > start && matches!(bytes[limit - 1], b' ' | b'\t' | b'\r' | b'\n') {
        limit -= 1;
    }
    let word_end = flat_word_end(src, bytes, start, limit)?;
    let word = &src[start..word_end];
    let open = score_whitespace_end(bytes, word_end).min(limit);
    if open >= limit || bytes[open] != b'(' {
        return None;
    }
    // Reject tabs/newlines inside the expression so span trimming stays trivial.
    if bytes[start..limit]
        .iter()
        .any(|&b| matches!(b, b'\t' | b'\r' | b'\n'))
    {
        return None;
    }
    let parse_u32 = |at: usize| -> Option<(u32, usize, usize)> {
        let s = score_whitespace_end(bytes, at).min(limit);
        let mut e = s;
        let mut val = 0u32;
        while e < limit && bytes[e].is_ascii_digit() {
            val = val
                .checked_mul(10)?
                .checked_add(u32::from(bytes[e] - b'0'))?;
            e += 1;
        }
        if e == s {
            return None;
        }
        Some((val, s, e))
    };
    let (pulses, p_start, p_end) = parse_u32(open + 1)?;
    let comma = score_whitespace_end(bytes, p_end).min(limit);
    if comma >= limit || bytes[comma] != b',' {
        return None;
    }
    let (steps, s_start, s_end) = parse_u32(comma + 1)?;
    if pulses < 1 || steps < pulses || steps > 64 {
        return None;
    }
    let mut after = score_whitespace_end(bytes, s_end).min(limit);
    let (rot, rot_span) = if after < limit && bytes[after] == b')' {
        (0i32, None)
    } else {
        if after < limit && bytes[after] == b',' {
            after = score_whitespace_end(bytes, after + 1).min(limit);
        }
        let r_start = after;
        let neg = after < limit && bytes[after] == b'-';
        if neg {
            after += 1;
        }
        let mut r_end = after;
        let mut r_val = 0i32;
        while r_end < limit && bytes[r_end].is_ascii_digit() {
            r_val = r_val
                .checked_mul(10)?
                .checked_add(i32::from(bytes[r_end] - b'0'))?;
            r_end += 1;
        }
        if r_end == after || r_val > 64 {
            return None;
        }
        after = score_whitespace_end(bytes, r_end).min(limit);
        if after >= limit || bytes[after] != b')' {
            return None;
        }
        (if neg { -r_val } else { r_val }, Some((r_start, r_end)))
    };
    if after + 1 != limit {
        return None;
    }
    let steps_meta = Some(Fraction::ONE);
    if matches!(word, "~" | "-") {
        return Some(if into.is_some() {
            silence()
        } else {
            silence().with_steps(steps_meta)
        });
    }
    let mask = rustel_core::euclid::euclid_rot(pulses as i32, steps as usize, rot as isize).ok()?;
    let mut locs = rustel_core::Locations::new();
    locs.push(leaf_place(
        src,
        offset,
        &Span {
            start,
            end: if word.contains(':') { limit } else { word_end },
        },
    ));
    locs.push(leaf_place(
        src,
        offset,
        &Span {
            start: p_start,
            end: p_end,
        },
    ));
    locs.push(leaf_place(
        src,
        offset,
        &Span {
            start: s_start,
            end: s_end,
        },
    ));
    if let Some((rs, re)) = rot_span {
        locs.push(leaf_place(src, offset, &Span { start: rs, end: re }));
    }
    let mut source_steps: [rustel_core::SourceStep; 64] =
        std::array::from_fn(|_| rustel_core::SourceStep {
            begin: Fraction::ZERO,
            end: Fraction::ZERO,
            value: 0,
            locations: rustel_core::Locations::new(),
        });
    let mut len = 0usize;
    let denom = i128::from(steps);
    for (i, &set) in mask.iter().enumerate() {
        if set == 0 {
            continue;
        }
        let begin = Fraction::new(i as i128, denom);
        let end = Fraction::new((i + 1) as i128, denom);
        source_steps[len] = rustel_core::SourceStep {
            begin,
            end,
            value: 0,
            locations: locs.clone(),
        };
        len += 1;
    }
    let layer_values: std::sync::Arc<[Value]> =
        rustel_core::arc_single_value(flat_atom_value(word));
    if let Some(builder) = into {
        builder.push_indexed_slice(Fraction::ONE, layer_values, true, &mut source_steps[..len]);
        return Some(silence());
    }
    let pattern = rustel_core::sequence_of_indexed_slice(
        Fraction::ONE,
        layer_values,
        true,
        &mut source_steps[..len],
    )?;
    Some(pattern.with_steps(steps_meta))
}

/// Fast path for plain ASCII integer ranges (`a .. b`, e.g. `"0 .. 63"`),
/// avoiding `Room`/`Parser`/`Ast` while preserving exact range spans and steps.
fn flat_range(src: &str, offset: usize) -> Option<Pattern> {
    if !src.is_ascii() {
        return None;
    }
    let bytes = src.as_bytes();
    let start = score_whitespace_end(bytes, 0);
    if start >= bytes.len() || bytes[start] == b'^' {
        return None;
    }
    let mut limit = bytes.len();
    while limit > start && matches!(bytes[limit - 1], b' ' | b'\t' | b'\r' | b'\n') {
        limit -= 1;
    }
    let scan_int = |mut pos: usize| -> Option<usize> {
        if pos < limit && bytes[pos] == b'-' {
            pos += 1;
        }
        let digits_start = pos;
        while pos < limit && bytes[pos].is_ascii_digit() {
            pos += 1;
        }
        (pos > digits_start).then_some(pos)
    };
    let a_end = scan_int(start)?;
    let dots = score_whitespace_end(bytes, a_end).min(limit);
    if dots == a_end || dots + 2 > limit || bytes[dots] != b'.' || bytes[dots + 1] != b'.' {
        return None;
    }
    if dots + 2 < limit && bytes[dots + 2] == b'.' {
        return None;
    }
    let b_start = score_whitespace_end(bytes, dots + 2).min(limit);
    let b_end = scan_int(b_start)?;
    if b_end != limit {
        return None;
    }
    let start_f = js_atom_number(&src[start..a_end])
        .and_then(Fraction::from_f64)?
        .to_f64();
    let end_f = js_atom_number(&src[b_start..b_end])
        .and_then(Fraction::from_f64)?
        .to_f64();
    checked_range_count(start_f, end_f).ok()?;
    let spans = [
        leaf_place(
            src,
            offset,
            &Span {
                start: b_start,
                end: b_end,
            },
        ),
        leaf_place(src, offset, &Span { start, end: a_end }),
    ];
    Some(
        range_pattern(start_f, end_f)
            .with_added_context_slice(&spans)
            .with_steps(Some(Fraction::ONE)),
    )
}

const CLS_FORBIDDEN: u8 = 1 << 0;
const CLS_COMMA: u8 = 1 << 1;
const CLS_PAREN: u8 = 1 << 2;
const CLS_BRACKET: u8 = 1 << 3;
const CLS_MODIFIER: u8 = 1 << 4;
const CLS_DOT: u8 = 1 << 5;
const CLS_PLUS: u8 = 1 << 6;

const MINI_CLASS: [u8; 256] = {
    let mut table = [0u8; 256];
    let mut i = 0usize;
    while i < 256 {
        table[i] = if i >= 128 {
            CLS_FORBIDDEN
        } else {
            match i as u8 {
                b'{' | b'}' | b'|' | b'?' | b'$' | b'%' | b'"' | b'\'' | b'\\' | b';' => {
                    CLS_FORBIDDEN
                }
                b',' => CLS_COMMA,
                b'(' | b')' => CLS_PAREN,
                b'[' | b']' => CLS_BRACKET,
                b'*' | b'/' | b'!' | b'@' => CLS_MODIFIER,
                b'.' => CLS_DOT,
                b'+' => CLS_PLUS,
                _ => 0,
            }
        };
        i += 1;
    }
    table
};

#[inline]
fn classify_mini_bytes(bytes: &[u8]) -> u8 {
    let mut mask = 0u8;
    for &b in bytes {
        mask |= MINI_CLASS[usize::from(b)];
    }
    mask
}

/// Fast path for top-level comma-stacked parallel lanes (`"seq1, seq2, ..."`),
/// emitting all lanes directly into one shared [`rustel_core::SequenceBuilder`]
/// with zero intermediate `Pattern` or `stack_slice` allocations.
///
/// Returns `None` when there is no top-level `,` (so callers may run
/// `flat_sequence` / `flat_euclid`), `Some(Some(pattern))` on success, or
/// `Some(None)` when a top-level `,` exists but a lane requires the full AST
/// (so callers skip `flat_sequence` / `flat_euclid` / `flat_range`).
fn try_flat_stack(src: &str, offset: usize) -> Option<Option<Pattern>> {
    let bytes = src.as_bytes();
    let mut ranges: [(usize, usize); 8] = [(0, 0); 8];
    let mut lane_mask: [u8; 8] = [0; 8];
    let mut lane_count = 0usize;
    let mut depth = 0i32;
    let mut start = 0usize;
    let mut has_top_comma = false;
    let mut unsupported = false;
    let mut cur_mask = 0u8;
    for (i, &b) in bytes.iter().enumerate() {
        if b >= 128 {
            return None;
        }
        cur_mask |= MINI_CLASS[usize::from(b)];
        match b {
            b'{' | b'}' | b'|' | b'?' | b'^' => {
                unsupported = true;
                if b == b'{' {
                    depth += 1;
                } else if b == b'}' {
                    depth -= 1;
                    if depth < 0 {
                        return if has_top_comma { Some(None) } else { None };
                    }
                }
            }
            b'(' => {
                depth += 1;
                if depth > 1 {
                    unsupported = true;
                }
            }
            b'[' | b'<' => {
                depth += 1;
                if depth > 2 {
                    unsupported = true;
                }
            }
            b')' | b']' | b'>' => {
                depth -= 1;
                if depth < 0 {
                    return if has_top_comma { Some(None) } else { None };
                }
            }
            b',' if depth == 0 => {
                has_top_comma = true;
                if lane_count + 1 >= ranges.len() {
                    unsupported = true;
                } else {
                    ranges[lane_count] = (start, i);
                    lane_mask[lane_count] = cur_mask & !CLS_COMMA;
                    lane_count += 1;
                    start = i + 1;
                    cur_mask = 0;
                }
            }
            _ => {}
        }
    }
    if !has_top_comma {
        return None;
    }
    if depth != 0 || unsupported {
        return Some(None);
    }
    ranges[lane_count] = (start, bytes.len());
    lane_mask[lane_count] = cur_mask;
    lane_count += 1;

    // Trim lane endpoints with O(1) edge scans and verify paren-terminated euclid lanes.
    let mut trimmed: [(usize, usize); 8] = [(0, 0); 8];
    for idx in 0..lane_count {
        let (raw_start, raw_end) = ranges[idx];
        let mut ts = raw_start;
        while ts < raw_end && matches!(bytes[ts], b' ' | b'\t' | b'\r' | b'\n') {
            ts += 1;
        }
        let mut te = raw_end;
        while te > ts && matches!(bytes[te - 1], b' ' | b'\t' | b'\r' | b'\n') {
            if bytes[te - 1] != b' ' {
                return Some(None);
            }
            te -= 1;
        }
        if ts >= te || te - ts > usize::from(u16::MAX) {
            return Some(None);
        }
        if (lane_mask[idx] & CLS_PAREN) != 0 && bytes[te - 1] != b')' {
            return Some(None);
        }
        trimmed[idx] = (ts, te);
    }

    let mut builder = rustel_core::SequenceBuilder::with_capacity(lane_count);
    for idx in 0..lane_count {
        let (ts, te) = trimmed[idx];
        let lane_src = &src[ts..te];
        let lane_offset = offset + ts;
        if te - ts == 1 && matches!(bytes[ts], b'~' | b'-') {
            continue;
        }
        let lmask = lane_mask[idx];
        if (lmask & CLS_PAREN) != 0 {
            if flat_euclid_core(lane_src, lane_offset, Some(&mut builder)).is_none() {
                return Some(None);
            }
            continue;
        }
        if (lmask & (CLS_COMMA | CLS_BRACKET | CLS_MODIFIER | CLS_PLUS)) == 0
            && try_plain_flat_into(lane_src, lane_offset, Some(&mut builder)).is_some()
        {
            continue;
        }
        if flat_sequence_core(lane_src, lane_offset, Some(&mut builder)).is_none() {
            return Some(None);
        }
    }
    // Without any `^` explicit step source, `Ast::Stack` always reports `_steps = 1`.
    Some(Some(
        builder
            .finish()
            .unwrap_or_else(silence)
            .with_steps(Some(Fraction::ONE)),
    ))
}

/// [`mini_in`] for callers that have already checked [`word_constant`] /
/// [`word_constant_parts`], avoiding a redundant single-word scan.
pub fn mini_non_word_in(room: &mut Room, src: &str, offset: usize) -> Result<Pattern, ParseError> {
    if reads_whole_parts() {
        let mask = classify_mini_bytes(src.as_bytes());
        if (mask & CLS_FORBIDDEN) == 0 {
            if (mask & (CLS_COMMA | CLS_PAREN | CLS_BRACKET | CLS_MODIFIER | CLS_PLUS)) == 0
                && let Some(pattern) = try_plain_flat_into(src, offset, None)
            {
                return Ok(pattern);
            }
            if (mask & CLS_COMMA) != 0 {
                if (mask & CLS_PLUS) == 0 {
                    if let Some(stacked) = try_flat_stack(src, offset) {
                        if let Some(pattern) = stacked {
                            return Ok(pattern);
                        }
                    } else if (mask & CLS_PAREN) == 0 {
                        if let Some(pattern) = flat_sequence(src, offset) {
                            return Ok(pattern);
                        }
                    } else if let Some(pattern) = flat_euclid(src, offset) {
                        return Ok(pattern);
                    }
                }
            } else {
                if (mask & (CLS_PAREN | CLS_PLUS)) == 0
                    && let Some(pattern) = flat_sequence(src, offset)
                {
                    return Ok(pattern);
                }
                if (mask & CLS_DOT) != 0
                    && (mask & (CLS_PAREN | CLS_BRACKET | CLS_MODIFIER | CLS_PLUS)) == 0
                    && let Some(pattern) = flat_range(src, offset)
                {
                    return Ok(pattern);
                }
            }
        }
        if let Some(pattern) = Planned::read(src, offset, room)?.and_then(Planned::pattern) {
            return Ok(pattern);
        }
    }
    let ast = parse(src)?;
    validate_construct(src, &ast)?;
    let _scope = BuildSource::install_at(src, offset);
    let pattern = build_validated(&ast);
    let metadata = step_metadata(&ast).ok_or_else(|| ParseError {
        message: "pattern _steps exceed the native Fraction range".into(),
        offset: ast.span().start,
        line: source_line_at(src, ast.span().start),
        phase: ErrorPhase::NativeLimit,
    })?;
    Ok(pattern.with_steps(Some(metadata.steps)))
}

/// [`mini_at`] in `room`. A reader of many texts keeps one room, and a
/// read then takes no new room for its plans.
pub fn mini_in(room: &mut Room, src: &str, offset: usize) -> Result<Pattern, ParseError> {
    // A construct of whole parts writes each value one time at its place,
    // from one parse with no syntax tree. One word takes no room.
    if reads_whole_parts()
        && let Some(pattern) = word_constant(src, offset)
    {
        return Ok(pattern);
    }
    mini_non_word_in(room, src, offset)
}

#[derive(Clone, Copy)]
struct StepMetadata {
    steps: Fraction,
    sourced: bool,
}

/// The `_steps` a pattern reports, or `None` when a weight sum or step product
/// leaves the native Fraction range.
fn step_metadata(ast: &Ast) -> Option<StepMetadata> {
    match ast {
        Ast::Seq {
            items,
            steps_source,
            ..
        } => sequence_metadata(items, *steps_source),
        // repeatCycles and fast both preserve the child's `_steps`. The
        // replicate amount is also the containing Step's weight; the parent
        // sequence applies it once when it computes its own weight sum.
        Ast::Replicated { pat, .. } => step_metadata(pat),
        // Each lane is first built as its own fastcat, so a lane
        // contributes its weight sum multiplied by the LCM of any sourced
        // children; only then are the lanes LCM'd together. Flattening
        // here loses the lane weight sum.
        Ast::Alt { lanes, .. } => sourced_metadata(
            lanes
                .iter()
                .map(|lane| sequence_metadata(&lane.items, lane.steps_source)),
        ),
        Ast::Stack { items, .. } | Ast::Choose { items, .. } | Ast::Polymeter { items, .. } => {
            aggregate_parallel_metadata(items)
        }
        Ast::Feet { items, .. } => {
            // fastcat reports the foot count as `_steps` and discards the
            // children's; only the sourced marker propagates to the parent.
            let children = items
                .iter()
                .map(step_metadata)
                .collect::<Option<Vec<_>>>()?;
            Some(StepMetadata {
                steps: Fraction::int(items.len() as i128),
                sourced: children.iter().any(|child| child.sourced),
            })
        }
        Ast::Euclid {
            pat,
            pulses,
            steps,
            rotation,
            ..
        } => {
            let receiver = step_metadata(pat)?;
            let eager = static_number_fraction(euclid_argument_source(&pulses.ast)).is_some()
                && static_number_fraction(euclid_argument_source(&steps.ast)).is_some()
                && rotation
                    .as_ref()
                    .is_none_or(|rotation| euclid_argument_is_pure_atom(&rotation.ast));
            Some(StepMetadata {
                // The dynamic register path has no step count. An ordinary
                // mask counts all steps across its cycles. Legato has one.
                steps: if eager {
                    let pulses = static_number_fraction(euclid_argument_source(&pulses.ast))
                        .expect("eager Euclid pulses");
                    let steps = static_number_fraction(euclid_argument_source(&steps.ast))
                        .expect("eager Euclid steps");
                    if pulses == Fraction::ZERO || steps <= Fraction::ZERO {
                        Fraction::ONE
                    } else {
                        let cycles = (pulses.to_f64().abs() / steps.to_f64()).ceil() as i128;
                        steps.checked_mul(Fraction::int(cycles))?
                    }
                } else {
                    Fraction::ZERO
                },
                // applyOptions restores only the receiver's private marker.
                sourced: receiver.sourced,
            })
        }
        Ast::Fast { pat, .. }
        | Ast::Slow { pat, .. }
        | Ast::Degrade { pat, .. }
        | Ast::Tail { pat, .. } => step_metadata(pat),
        Ast::Range { start, .. } => step_metadata(start),
        Ast::Atom { .. } | Ast::Silence { .. } => Some(StepMetadata {
            steps: Fraction::ONE,
            sourced: false,
        }),
    }
}

fn sequence_metadata(items: &[Step], explicitly_sourced: bool) -> Option<StepMetadata> {
    let own = checked_total_weight(items).ok()?;
    let nested = sourced_metadata(items.iter().map(|item| step_metadata(&item.ast)))?;
    Some(StepMetadata {
        steps: own.checked_mul(nested.steps)?,
        sourced: explicitly_sourced || nested.sourced,
    })
}

fn aggregate_parallel_metadata(items: &[Ast]) -> Option<StepMetadata> {
    sourced_metadata(items.iter().map(step_metadata))
}

/// The common multiple of the step counts of the sourced `children`, and
/// whether one is sourced. `None` for a child with no metadata, and for a
/// multiple outside the native Fraction range.
fn sourced_metadata(children: impl Iterator<Item = Option<StepMetadata>>) -> Option<StepMetadata> {
    let none = StepMetadata {
        steps: Fraction::ONE,
        sourced: false,
    };
    children.into_iter().try_fold(none, |all, child| {
        let child = child?;
        Some(match child.sourced {
            true => StepMetadata {
                steps: all.steps.checked_lcm(child.steps)?,
                sourced: true,
            },
            false => all,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::{
        ErrorPhase, EuclidExtentError, MAX_MINI_AST_DEPTH, MAX_MINI_DEPTH, MAX_NATIVE_EUCLID_STEPS,
        build, checked_euclid_extents, is_step_char, leaf_locations, mini, parse,
    };
    use rustel_core::compose::{Alignment, ComposeOp, compose};
    use rustel_core::{Pattern, QueryLimit, Value};
    use rustel_fraction::Fraction;

    /// [`is_step_char`] is the parser's own word rule: a run of step
    /// characters is one leaf, and the separators and operators around a word
    /// are no part of it.
    #[test]
    fn a_run_of_step_characters_is_one_word() {
        for word in [
            "mlkr-grsl",
            "tr-808",
            "a.b",
            "c#3",
            "x^y",
            "snare_2",
            "bd~",
            "é2",
        ] {
            assert!(word.chars().all(is_step_char), "{word}");
            assert_eq!(
                leaf_locations(word, 0).expect(word),
                [(0, word.len())],
                "{word}"
            );
        }
        for separator in [
            ' ', '\n', ',', ':', '*', '/', '!', '@', '?', '<', '[', '{', '(', '|', '%', '"', '\'',
        ] {
            assert!(!is_step_char(separator), "{separator:?}");
        }
    }

    #[test]
    fn patterned_slow_zero_is_silent_without_losing_other_factors() {
        let shape = |haps: Vec<rustel_core::Hap>| {
            haps.into_iter()
                .map(|hap| (hap.whole, hap.part, hap.value))
                .collect::<Vec<_>>()
        };
        for (source, silent_factor) in [("bd/[0 2]", "bd/[~ 2]"), ("bd/<0 2>", "bd/<~ 2>")] {
            let haps = mini(source)
                .expect("patterned slow parses")
                .query_arc_sorted(Fraction::ZERO, Fraction::from(4));
            let expected = mini(silent_factor)
                .expect("silent factor parses")
                .query_arc_sorted(Fraction::ZERO, Fraction::from(4));

            assert!(!expected.is_empty(), "the nonzero factor must still play");
            assert_eq!(shape(haps), shape(expected), "{source}");
        }
    }

    /// A patterned slow factor whose reciprocal leaves the native Fraction range
    /// refuses the query as `slow`, as the scalar form does.
    #[test]
    fn a_patterned_slow_factor_without_a_reciprocal_refuses_the_query() {
        let pattern = mini("bd/[-170141183460469231731687303715884105728 2]").expect("parses");
        let result = pattern.try_query_arc_sorted(Fraction::ZERO, Fraction::ONE);
        assert!(
            matches!(
                result,
                Err(QueryLimit::NativeFraction { operation: "slow" })
            ),
            "{result:?}"
        );
    }

    #[test]
    fn a_pathologically_nested_pattern_is_refused_rather_than_aborting() {
        let Err(error) = mini(&"[".repeat(200_000)) else {
            panic!("200k open brackets must be refused");
        };
        assert_eq!(
            error.phase,
            ErrorPhase::NativeLimit,
            "the refusal must be typed as a native limit: {error}"
        );
        assert!(
            error.to_string().contains("nests deeper than"),
            "the refusal did not name the depth bound: {error}"
        );
    }

    /// The limit has to sit above anything real and below where the stack
    /// gives out. Depth `MAX_MINI_DEPTH` parses; one more is refused.
    #[test]
    fn the_depth_bound_admits_everything_it_claims_to() {
        // One bracket short of the bound, because the `bd` inside them is
        // itself the last level.
        let depth = MAX_MINI_DEPTH as usize - 1;
        let accepted = format!("{}bd{}", "[".repeat(depth), "]".repeat(depth));
        let pattern = mini(&accepted).expect("the accepted boundary must parse");
        assert_eq!(
            pattern
                .query_arc_sorted(Fraction::ZERO, Fraction::ONE)
                .len(),
            1,
            "the accepted boundary must remain queryable"
        );

        let refused = format!("{}bd{}", "[".repeat(depth + 1), "]".repeat(depth + 1));
        let Err(error) = mini(&refused) else {
            panic!("one past the boundary must be refused");
        };
        assert_eq!(error.phase, ErrorPhase::NativeLimit);

        // Ordinary music is nowhere near the bound.
        assert!(mini("[bd [sd [hh [cp]]]] <a b>").is_ok());
    }

    #[test]
    fn postfix_operators_cannot_bypass_the_ast_depth_bound() {
        let source = format!("a{}", "?".repeat(200_000));
        let Err(error) = mini(&source) else {
            panic!("an oversized postfix chain must be refused");
        };
        assert_eq!(error.phase, ErrorPhase::NativeLimit);
        assert!(error.to_string().contains("pattern syntax nests"));

        let accepted = format!("a{}", "?".repeat(MAX_MINI_AST_DEPTH as usize - 2));
        assert!(mini(&accepted).is_ok());
        let refused = format!("a{}", "?".repeat(MAX_MINI_AST_DEPTH as usize - 1));
        let Err(error) = mini(&refused) else {
            panic!("one level past the AST limit must be refused");
        };
        assert_eq!(error.phase, ErrorPhase::NativeLimit);
    }

    /// Each construct is one level of syntax around what it holds, and the
    /// deepest of what it holds counts: a text at the limit is read, and a
    /// text one level past it is refused.
    #[test]
    fn each_construct_is_one_level_of_syntax() {
        // A text around levels of `?`, and the levels of the text with
        // none.
        type Text = fn(&str) -> String;
        let texts: [(Text, usize); 14] = [
            (|deep| format!("a{deep}"), 2),
            (|deep| format!("a{deep}*2"), 3),
            (|deep| format!("a{deep}/2"), 3),
            (|deep| format!("a{deep}:b"), 3),
            (|deep| format!("a{deep}..2"), 3),
            (|deep| format!("a{deep}!2"), 3),
            (|deep| format!("a{deep}(3,8)"), 3),
            (|deep| format!("a(b{deep},8)"), 3),
            (|deep| format!("a*[b{deep}]"), 4),
            (|deep| format!("[a{deep}, b]"), 4),
            (|deep| format!("[a, b{deep}]"), 4),
            (|deep| format!("<a{deep} b>"), 3),
            (|deep| format!("<a{deep}, b>"), 3),
            (|deep| format!("{{a}}%[b{deep}]"), 4),
        ];
        for (text, levels) in texts {
            let most = MAX_MINI_AST_DEPTH as usize - levels;
            let read = text(&"?".repeat(most));
            assert!(parse(&read).is_ok(), "{}", text("?"));
            let Err(error) = parse(&text(&"?".repeat(most + 1))) else {
                panic!("{} is read one level past the limit", text("?"));
            };
            assert!(
                error.to_string().contains("pattern syntax nests"),
                "{error}"
            );
        }
    }

    #[test]
    fn a_range_past_the_native_limit_is_refused_rather_than_allocated() {
        let Err(error) = mini("0 .. 5000000000") else {
            panic!("a five-billion element range must be refused");
        };
        assert_eq!(
            error.phase,
            ErrorPhase::NativeLimit,
            "the refusal must be typed as a native limit: {error}"
        );
        assert!(
            error.to_string().contains("above the native limit"),
            "the refusal did not name the bound: {error}"
        );
    }

    /// The bound has to sit above every range strudel.cc can build, or a song
    /// that plays there would not play here. V8 takes 125,172 spread
    /// arguments before throwing.
    #[test]
    fn every_range_strudel_can_build_still_builds() {
        for (source, expected) in [("0 .. 3", 4), ("3 .. 0", 4), ("0 .. 100000", 100_001)] {
            let pattern = mini(source).expect("strudel.cc builds this range");
            assert_eq!(
                pattern
                    .query_arc_sorted(Fraction::ZERO, Fraction::ONE)
                    .len(),
                expected,
                "{source} did not expand to {expected} elements"
            );
        }
    }

    #[test]
    fn a_patterned_range_past_the_limit_is_a_typed_refusal() {
        let pattern = mini("0 .. <5000000000 3>").expect("the source itself is valid");
        assert!(matches!(
            pattern.try_query_arc_sorted(Fraction::ZERO, Fraction::ONE),
            Err(QueryLimit::MiniRange {
                elements: 5_000_000_001,
                limit,
            })
            if limit == super::MAX_MINI_RANGE as u64
        ));
        assert_eq!(
            pattern
                .query_arc_sorted(Fraction::ONE, Fraction::int(2))
                .len(),
            4,
            "the next cycle must still play"
        );
    }

    #[test]
    fn euclid_native_limit_is_exact_and_survivable() {
        assert!(checked_euclid_extents(3.0, MAX_NATIVE_EUCLID_STEPS as f64).is_ok());
        assert!(matches!(
            checked_euclid_extents(3.0, (MAX_NATIVE_EUCLID_STEPS + 1) as f64),
            Err(EuclidExtentError::NativeLimit { steps: 16_385 })
        ));
        assert!(checked_euclid_extents(16_384.0, -4.0).is_ok());
        for (pulses, steps) in [(16_385.0, 4.0), (-16_385.0, -4.0)] {
            assert!(matches!(
                checked_euclid_extents(pulses, steps),
                Err(EuclidExtentError::NativeLimit { steps: 16_388 })
            ));
        }
        assert!(matches!(
            checked_euclid_extents(3.0, -16_385.0),
            Err(EuclidExtentError::NativeLimit { steps: 16_385 })
        ));

        let pattern = mini("a(3,16384)").expect("the accepted boundary must construct");
        assert_eq!(
            pattern
                .query_arc_sorted(Fraction::ZERO, Fraction::ONE)
                .len(),
            3,
            "the accepted boundary must remain queryable"
        );
    }

    #[test]
    fn euclid_static_and_patterned_arguments_share_extended_timing() {
        let timing = |source: &str| {
            mini(source)
                .expect("euclid notation")
                .query_arc_sorted(Fraction::ZERO, Fraction::int(2))
                .into_iter()
                .map(|hap| {
                    let whole = hap.whole.expect("euclid event");
                    (whole.begin, whole.end)
                })
                .collect::<Vec<_>>()
        };
        let expected = [0, 2, 3, 5, 6]
            .into_iter()
            .map(|start| (Fraction::new(start, 4), Fraction::new(start + 1, 4)))
            .collect::<Vec<_>>();
        for source in ["x(5,4)", "x(<5>,<4>)"] {
            assert_eq!(timing(source), expected, "{source}");
        }
        let expected = [0, 2, 3, 3, 5, 6]
            .into_iter()
            .zip([2, 3, 5, 5, 6, 8])
            .map(|(start, end)| (Fraction::new(start, 4), Fraction::new(end, 4)))
            .collect::<Vec<_>>();
        for source in ["x(5,-4)", "x(<5>,<-4>)"] {
            assert_eq!(timing(source), expected, "{source}");
        }
        let expected = [(0, 2), (2, 4), (2, 4), (4, 6)]
            .map(|(begin, end)| (Fraction::new(begin, 3), Fraction::new(end, 3)));
        for source in ["x(3,1.5)", "x(<3>,<1.5>)"] {
            assert_eq!(timing(source), expected, "{source}");
        }
    }

    /// A Euclid rotation may follow the steps after whitespace alone and plays
    /// like the comma spelling; exactly one rotation fits, and refusals keep
    /// krill's expectation sets.
    #[test]
    fn euclid_rotation_without_a_comma_matches_the_comma_spelling() {
        let first_cycle = |source: &str| {
            mini(source)
                .unwrap_or_else(|error| panic!("{source} did not parse: {error:?}"))
                .query_arc_sorted(Fraction::ZERO, Fraction::ONE)
                .into_iter()
                .map(|hap| (hap.part.begin, hap.part.end, hap.value))
                .collect::<Vec<_>>()
        };
        let a = || rustel_core::Value::Str("a".into());

        assert_eq!(
            first_cycle("a(3,8 5)"),
            vec![
                (Fraction::ZERO, Fraction::new(1, 8), a()),
                (Fraction::new(3, 8), Fraction::new(1, 2), a()),
                (Fraction::new(5, 8), Fraction::new(3, 4), a()),
            ]
        );
        for (commaless, comma) in [
            ("a(3,8 5)", "a(3,8,5)"),
            ("a(3, 8 5)", "a(3, 8, 5)"),
            ("a(3,8  5 )", "a(3,8 , 5)"),
            ("a(3,8 <1 2>)", "a(3,8,<1 2>)"),
            ("a(3,8)", "a(3,8,)"),
        ] {
            assert_eq!(first_cycle(commaless), first_cycle(comma), "{commaless}");
        }

        for (source, message) in [
            (
                "a(3,8%2)",
                r##"Expected "!", "(", ")", "*", ",", "..", "/", ":", "<", "?", "[", "{", [@_], a letter, a number, "-", "#", ".", "^", "_", or whitespace but "%" found."##,
            ),
            (
                "a(3,8,%2)",
                r##"Expected ")", "<", "[", "{", a letter, a number, "-", "#", ".", "^", "_", or whitespace but "%" found."##,
            ),
            (
                "a(3,8 5,6)",
                r##"Expected "!", "(", ")", "*", "..", "/", ":", "?", [@_], a letter, a number, "-", "#", ".", "^", "_", or whitespace but "," found."##,
            ),
        ] {
            let Err(error) = mini(source) else {
                panic!("{source} must not parse");
            };
            assert_eq!(error.message, message, "{source}");
        }
        assert!(
            mini("a(3,8 5 6)").is_err(),
            "a second rotation must not parse"
        );
    }

    /// Weights that sum to zero are silence, not a division by zero. A zero
    /// `@` weight reads as 1, so the guard is for negative weights and for
    /// weights that round to zero.
    #[test]
    fn weights_summing_to_zero_are_silent_rather_than_a_panic() {
        for source in ["bd@-1 sd@1", "bd@-2 sd@1 hh@1", "[bd@-1 sd@1]"] {
            let pattern =
                mini(source).unwrap_or_else(|error| panic!("{source} did not parse: {error:?}"));
            let haps = pattern.query_arc(Fraction::ZERO, Fraction::ONE);
            assert!(haps.is_empty(), "{source} produced {haps:?}");
        }
    }

    /// Pins that sequence weights whose sum, step windows or `_steps` product
    /// leave the native Fraction range are refused as native limits by `mini`, and
    /// the sum and window cases by `build` too, while a single in-range weight
    /// still builds.
    #[test]
    fn weight_totals_and_steps_products_past_the_fraction_limit_are_refused() {
        for source in [
            // Each weight fits; their sum does not.
            "bd@9e37 sd@9e37",
            // The same sum as an `<>` lane's span.
            "<a@9e37 b@9e37>",
            // The sum fits; the first step's window end, 1/(2e38 + 2), does not.
            "a@0.5 b@0.5 c@1e38",
            // A `_steps` product: 2e37, doubled at each of four enclosing levels.
            "[^[^[^[^[^bd@1e37 sd@1e37] cp] cp] cp] cp]",
        ] {
            let Err(error) = mini(source) else {
                panic!("{source} must be refused");
            };
            assert_eq!(
                error.phase,
                ErrorPhase::NativeLimit,
                "{source}: the refusal must be typed as a native limit: {error}"
            );
            assert!(
                error.to_string().contains("native Fraction range"),
                "{source}: the refusal did not name the bound: {error}"
            );
        }

        let pattern = mini("bd@9e37").expect("a single in-range weight must build");
        assert_eq!(
            pattern
                .query_arc_sorted(Fraction::ZERO, Fraction::ONE)
                .len(),
            1,
            "the accepted boundary must remain queryable"
        );

        for source in ["bd@9e37 sd@9e37", "a@0.5 b@0.5 c@1e38"] {
            let ast = parse(source).expect("the source itself parses");
            let Err(error) = build(source, &ast) else {
                panic!("build must refuse {source}");
            };
            assert_eq!(error.phase, ErrorPhase::NativeLimit, "{source}: {error}");
        }
    }

    /// Pins that a polymeter whose step count over a lane's weight total leaves
    /// the native Fraction range refuses the query as a native limit.
    #[test]
    fn a_polymeter_factor_past_the_fraction_limit_refuses_the_query() {
        let pattern = mini("{a@1e38 b, c@0.5}").expect("each lane's weights fit");
        let result = pattern.try_query_arc_sorted(Fraction::ZERO, Fraction::ONE);
        assert!(
            matches!(
                result,
                Err(QueryLimit::NativeFraction {
                    operation: "polymeter"
                })
            ),
            "{result:?}"
        );
    }

    /// A weight that rounds to 0/1 (`1e-10`) must not leave a pending query
    /// error at build time. `@n` and `!n` both go through `sequence_weight`.
    #[test]
    fn a_tiny_weight_builds_without_arming_a_query_error() {
        for source in ["bd@1e-10 sd", "bd!1e-10 sd"] {
            // Test threads are reused; start from a known-clean thread-local.
            let _ = rustel_core::take_query_error();
            mini(source).unwrap_or_else(|error| panic!("{source} did not parse: {error:?}"));
            assert_eq!(
                rustel_core::take_query_error(),
                None,
                "building {source} left a pending query error"
            );
        }
    }

    /// The zero-weight step contributes silence and the survivor takes the
    /// whole cycle - the same slot arithmetic a raw zero-weight step gets
    /// from `stepcat`, not a shifted or dropped one.
    #[test]
    fn a_tiny_weight_step_is_silent_and_the_rest_spans_the_cycle() {
        for source in ["bd@1e-10 sd", "bd!1e-10 sd"] {
            let haps = mini(source)
                .expect("parses")
                .query_arc(Fraction::ZERO, Fraction::ONE);
            assert_eq!(haps.len(), 1, "{source} produced {haps:?}");
            assert_eq!(haps[0].value, rustel_core::Value::Str("sd".into()));
            assert_eq!(haps[0].part.begin, Fraction::ZERO, "{source}: {haps:?}");
            assert_eq!(haps[0].part.end, Fraction::ONE, "{source}: {haps:?}");
        }
    }

    /// When EVERY weight of several steps rounds to zero the total is zero,
    /// and `build_seq` answers with silence before any window division, as
    /// upstream `stepcat` skips every zero-time entry: no panic, no haps, no
    /// pending query error.
    #[test]
    fn a_sequence_of_only_tiny_weights_is_silent_rather_than_poisoned() {
        for source in ["bd@1e-10 sd@1e-10", "bd!1e-10 sd!1e-10"] {
            let _ = rustel_core::take_query_error();
            let pattern =
                mini(source).unwrap_or_else(|error| panic!("{source} did not parse: {error:?}"));
            assert_eq!(
                rustel_core::take_query_error(),
                None,
                "building {source} left a pending query error"
            );
            let haps = pattern.query_arc(Fraction::ZERO, Fraction::ONE);
            assert!(haps.is_empty(), "{source} produced {haps:?}");
            assert_eq!(
                rustel_core::take_query_error(),
                None,
                "querying {source} left a pending query error"
            );
        }
    }

    /// A single step fills the cycle whatever its weight: pinned `stepcat`
    /// reifies a singleton before it computes a total, so a weight that
    /// rounds to zero still plays, alone or as a bracketed step.
    #[test]
    fn a_single_tiny_weight_step_still_plays_the_whole_slot() {
        let _ = rustel_core::take_query_error();
        let haps = mini("bd@1e-10")
            .expect("parses")
            .query_arc(Fraction::ZERO, Fraction::ONE);
        assert_eq!(haps.len(), 1, "{haps:?}");
        assert_eq!(haps[0].value, rustel_core::Value::Str("bd".into()));
        assert_eq!(haps[0].part.begin, Fraction::ZERO);
        assert_eq!(haps[0].part.end, Fraction::ONE);

        let haps = mini("[bd@1e-10] sd")
            .expect("parses")
            .query_arc(Fraction::ZERO, Fraction::ONE);
        assert_eq!(haps.len(), 2, "{haps:?}");
        assert_eq!(haps[0].value, rustel_core::Value::Str("bd".into()));
        assert_eq!(haps[0].part.end, Fraction::new(1, 2), "{haps:?}");
        assert_eq!(
            rustel_core::take_query_error(),
            None,
            "a single tiny-weight step left a pending query error"
        );
    }

    /// `bd:3` reads as one word and highlights as one band: a chain of
    /// plain atoms carries one span over the whole token. A sequence on
    /// the base keeps one span per leaf, as upstream has it.
    #[test]
    fn a_sound_with_an_index_highlights_as_one_word() {
        let haps = mini("bd:3 gm_acoustic_guitar_steel:1")
            .expect("parses")
            .query_arc(Fraction::ZERO, Fraction::ONE);
        assert_eq!(haps.len(), 2, "{haps:?}");
        for (hap, token) in haps.iter().zip(["bd:3", "gm_acoustic_guitar_steel:1"]) {
            assert_eq!(hap.context.len(), 1, "{:?}", hap.context);
            let (begin, end) = hap.context.first().expect("one location");
            assert_eq!(end - begin, token.len(), "{token}");
        }
        assert_eq!(
            haps[0].value,
            rustel_core::Value::list(vec![
                rustel_core::Value::Str("bd".into()),
                rustel_core::Value::F64(3.0)
            ])
        );

        let haps = mini("a:b:c")
            .expect("parses")
            .query_arc(Fraction::ZERO, Fraction::ONE);
        assert_eq!(haps[0].context.len(), 1, "{:?}", haps[0].context);
        let (begin, end) = haps[0].context.first().expect("one location");
        assert_eq!(end - begin, "a:b:c".len());
        assert_eq!(
            haps[0].value,
            rustel_core::Value::list(vec![
                rustel_core::Value::Str("a".into()),
                rustel_core::Value::Str("b".into()),
                rustel_core::Value::Str("c".into())
            ])
        );

        let haps = mini("[bd sd]:3")
            .expect("parses")
            .query_arc(Fraction::ZERO, Fraction::ONE);
        assert_eq!(haps.len(), 2);
        assert!(
            haps.iter().all(|hap| hap.context.len() == 2),
            "a sequence keeps one span per leaf: {haps:?}"
        );
    }

    /// A final zero weight becomes one only when the sequence is assembled.
    /// Keeping it raw while parsing matters to chained modifiers, and `!0`
    /// silences its child while retaining the child's slot.
    #[test]
    fn a_zero_at_weight_reads_as_one_but_zero_replicas_still_delete() {
        let _ = rustel_core::take_query_error();
        let haps = mini("bd@0 sd")
            .expect("parses")
            .query_arc(Fraction::ZERO, Fraction::ONE);
        assert_eq!(haps.len(), 2, "{haps:?}");
        assert_eq!(
            haps[0].part.end,
            Fraction::new(1, 2),
            "bd must keep a full slot"
        );
        assert_eq!(
            rustel_core::take_query_error(),
            None,
            "a raw zero weight left a pending query error"
        );

        let haps = mini("bd@0 sd@0")
            .expect("parses")
            .query_arc(Fraction::ZERO, Fraction::ONE);
        assert_eq!(haps.len(), 2, "both zero weights read as one: {haps:?}");

        let haps = mini("bd!0 sd")
            .expect("parses")
            .query_arc(Fraction::ZERO, Fraction::ONE);
        assert_eq!(haps.len(), 1, "zero replicas must still delete the step");
        assert_eq!(haps[0].part.begin, Fraction::new(1, 2));
        assert_eq!(haps[0].part.end, Fraction::ONE);

        let haps = mini("bd@0@2 sd")
            .expect("parses")
            .query_arc(Fraction::ZERO, Fraction::ONE);
        assert_eq!(haps.len(), 2, "the second @ must accumulate from raw zero");
        assert_eq!(haps[0].part.end, Fraction::new(1, 2));

        let haps = mini("bd!0@2 sd")
            .expect("parses")
            .query_arc(Fraction::ZERO, Fraction::ONE);
        assert_eq!(haps.len(), 1, "the replicated child remains silent");
        assert_eq!(haps[0].part.begin, Fraction::new(1, 2));

        let haps = mini("bd!0!2 sd")
            .expect("parses")
            .query_arc(Fraction::ZERO, Fraction::ONE);
        assert_eq!(haps.len(), 2, "a later ! restores one replica");
        assert_eq!(haps[0].part.end, Fraction::new(1, 2));
    }

    #[test]
    fn all_falsy_lane_weights_keep_strudels_polymeter_refusal() {
        for source in ["{bd@0, sd}", "{bd!0, sd}"] {
            let haps = mini(source)
                .unwrap_or_else(|error| panic!("{source} did not parse: {error:?}"))
                .query_arc(Fraction::ZERO, Fraction::ONE);
            assert!(haps.is_empty(), "{source} produced {haps:?}");
        }

        // The same zero inside a larger sequence still keeps an ordinary
        // half-slot: the sibling makes this an unweighted two-step sequence.
        let haps = mini("{bd!0 sd, cp}")
            .expect("parses")
            .query_arc(Fraction::ZERO, Fraction::ONE);
        assert!(
            haps.iter()
                .any(|hap| hap.value == rustel_core::Value::Str("sd".into())
                    && hap.part.begin == Fraction::new(1, 2)),
            "the silent replicated step lost its half-slot: {haps:?}"
        );
    }

    /// Build `source` at `offset` with one node for each value, the graph
    /// that a rhythm stands for.
    fn one_node_for_each_value(source: &str, offset: usize) -> rustel_core::Pattern {
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) {
                super::ONE_NODE_FOR_EACH_VALUE.set(false);
            }
        }
        super::ONE_NODE_FOR_EACH_VALUE.set(true);
        let _restore = Restore;
        super::mini_at(source, offset).expect(source)
    }

    /// Sources of plain values at a constant speed, and sources with a step
    /// that is another construct, which keeps its own nodes.
    const RHYTHMS: &[&str] = &[
        "a b c",
        "^a b c",
        "a [^b c d] e",
        "a:b",
        "<a b c d>/4",
        "<a@2 b c>/3, d*2",
        "[a b]/2",
        "a/2 b",
        "<a b>/1.5",
        "bd:3 sd:1:0.5 hh",
        "<a:b [c:d e]>*2",
        "[a]:b c",
        "a:[b] c",
        "~:a b:~",
        "<^a b c> d, e f",
        "a ~ b -",
        "a [b c] d",
        "[a b] [c [d e]] f",
        "[a,b] c",
        "[a, b, ~] - [c,d]",
        "a!3 b",
        "a b!2 [c d]!2",
        "[a@2] b",
        "0 1 [2 3] -4.5",
        "<[a b]!3 [c d e]> f",
        "<[a [b,c]] [d e f]>",
        "a b, c d e",
        "bd*4, ~ sd ~ sd, hh hh hh hh",
        "a b@2 c",
        "a@3 b",
        "a@3 ~ b@2.5",
        "[a b@3 c] d",
        "<a b> c d",
        "<a b>",
        "<a b c>*2",
        "a*<1 2 4>",
        "[a b]*<2 3> c",
        "<a b c>*<2 1>",
        "a*<1 2>, b*<4 8 2>",
        "a*<2 1.5>",
        "a*<2 [3 4]>",
        "[a,b]*<1 2>",
        "[a/2 b]*<1 2>",
        "a/2*<1 2>",
        "<a b>/2*<2 1>",
        "a*<1@1.5 2@0.5>",
        "a*[[1 ~ 2 3]/3]",
        "a*<2 0>",
        "a*<2@2 3>",
        "<a b>/2",
        "<a@2 b c>/3",
        "<a b, c d e>",
        "<a [b c]> <d e f>",
        "a <b <c d>>",
        "a*2 b c",
        "a*4",
        "[a b]*3",
        "[a b c]/2",
        "[a b]*1.5",
        "[a b]/1.5 c",
        "a*3 [b*2 c]",
        "[a b@3]*2 c",
        "[a, b c]*2",
        "a/2 b",
        "<a b>!2 c",
        "<a@3 b> c",
        "a . b c . d",
        "a:b c",
        "a:b*2 c:1",
        "a*0 b",
        "a*-1 b",
        "a/0 b",
        "<a b>*0",
        "a",
        "[a]",
        "<a>",
        "<a:b>",
        "a*1",
        "a/1",
        "<<a>>/1",
        "[a, b]",
        "a@0 b c",
        "a@-1 b@2 c",
        "<a@0 b>",
        "~ ~",
        "~",
        "[~ ~] a",
        "<~ a>",
        "a(3,8) b",
        "a(3,8)",
        "a(3,8,2)",
        "a(5,8,-3) b",
        "[a,b](3,8) c",
        "<a b>(3,8)",
        "<a b c>(2,5) d",
        "a(3,8)*2",
        "a(3,8)/2 b",
        "[a b](2,4)",
        "[a b](2,8)",
        "a(8,8)",
        "a(1,1)",
        "a!3 b",
        "a!2",
        "a!1 b",
        "<a b!3>",
        "[a b]!2 c",
        "<a b>!2 c",
        "<a b c>!3",
        "[a, b]!3 c",
        "a/2!2 b",
        "[a!2 b]!2",
        "a!3(3,8)",
        "a!2*3 b",
        // Layers that are not in the order of time when they repeat, and a
        // count that is not whole or not above zero, keep their nodes.
        "[a b, c]!2 d",
        "a!0 b",
        "a!-2 b",
        // A structure that cuts a value, or holds no whole count of pulses
        // in one cycle, keeps its nodes.
        "[a b c](3,8)",
        "a(9,8)",
        "a(0,8) b",
        "a(-3,8) b",
        "a(3,-8) b",
        // A step that is another construct keeps its own nodes.
        "a? b c",
        "[a b, c d e] f",
        "{a b c}%4 d",
        "a*<2 3> b",
        "a!2.5 b",
        "a|b c",
        "0 .. 3 a",
    ];

    /// Sources of whole parts, written by a fixed walk of a small grammar:
    /// atoms, words with `:` between them, rests, sequences, stacks,
    /// alternations, whole speeds, slow counts and repeats, three levels
    /// deep at most.
    fn sources_of_whole_parts(count: usize) -> Vec<String> {
        fn next(state: &mut u64, below: u64) -> u64 {
            *state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (*state >> 33) % below
        }
        fn sequence(state: &mut u64, depth: u32) -> String {
            let steps = 1 + next(state, 4);
            let steps: Vec<String> = (0..steps).map(|_| step(state, depth)).collect();
            steps.join(" ")
        }
        fn step(state: &mut u64, depth: u32) -> String {
            match next(state, if depth == 0 { 3 } else { 10 }) {
                0 => "~".to_owned(),
                1 | 2 => ["a", "b", "c3", "1", "0.5"][next(state, 5) as usize].to_owned(),
                3 => format!("[{}]", sequence(state, depth - 1)),
                4 => format!("<{}>", sequence(state, depth - 1)),
                5 => format!(
                    "[{}, {}]",
                    sequence(state, depth - 1),
                    sequence(state, depth - 1)
                ),
                6 => format!(
                    "<{}, {}>",
                    sequence(state, depth - 1),
                    sequence(state, depth - 1)
                ),
                7 if next(state, 3) == 0 => {
                    format!("{}:{}", step(state, depth - 1), step(state, 0))
                }
                7 if next(state, 2) == 0 => {
                    format!("{}/{}", step(state, depth - 1), 1 + next(state, 3))
                }
                7 => format!("{}*{}", step(state, depth - 1), 1 + next(state, 3)),
                8 => format!("{}@{}", step(state, depth - 1), 1 + next(state, 3)),
                _ => format!("{}!{}", step(state, depth - 1), 1 + next(state, 3)),
            }
        }
        let mut state = 0x2545_f491_4f6c_dd1d;
        (0..count).map(|_| sequence(&mut state, 3)).collect()
    }

    /// Spans that a query asks: whole cycles, parts of cycles, spans
    /// before cycle zero, and spans of no width.
    fn spans() -> [(Fraction, Fraction); 14] {
        let third = |n: i128| Fraction::new(n, 3);
        [
            (Fraction::ZERO, Fraction::ONE),
            (Fraction::ZERO, Fraction::from(3)),
            (Fraction::ZERO, Fraction::from(13)),
            (third(1), third(5)),
            (third(-7), third(22)),
            (Fraction::new(7, 8), Fraction::new(9, 8)),
            (Fraction::new(-3, 2), Fraction::new(-1, 4)),
            (Fraction::new(5, 16), Fraction::new(7, 16)),
            (Fraction::new(41, 8), Fraction::new(21, 4)),
            // A span of no width asks for what sounds at its one time.
            (Fraction::ZERO, Fraction::ZERO),
            (Fraction::new(1, 2), Fraction::new(1, 2)),
            (third(4), third(4)),
            (Fraction::from(2), Fraction::from(2)),
            (Fraction::new(-5, 8), Fraction::new(-5, 8)),
        ]
    }

    /// The plans of a text of whole parts answer each query as the
    /// constructs of the text do when they build the pattern level by
    /// level: the same haps in the same order, with the same times,
    /// values and locations, and the same step count. A query that walks
    /// the pattern reads the plans where it asks, and a table and a fold
    /// list each step: each answers the same. Each text reads its plans
    /// in the room of the text before.
    /// A text that is one word with whitespace around it is the constant
    /// that the plans of the word make: the same haps at the same places,
    /// with the same step count. The word is read with no room, so it is
    /// placed as the parse places it, spaces trimmed and tabs kept.
    #[test]
    fn a_text_of_one_word_answers_as_the_plans_read_it() {
        let mut room = super::Room::default();
        for (source, offset) in [
            ("bd", 7),
            (" sawtooth ", 0),
            ("bd\t", 3),
            (" \tbd\n", 11),
            ("  bd  ", 0),
            ("0x1A", 0),
            ("3.5", 0),
            ("-7", 0),
            ("c#3", 5),
            ("a.b", 0),
            ("é2", 0),
            ("gm_piano", 2),
            ("bd:3", 0),
            ("c:minor", 4),
        ] {
            let planned = super::Planned::read(source, offset, &mut room)
                .unwrap_or_else(|error| panic!("{source}: {error}"))
                .expect("one word is a construct of whole parts");
            let planned = planned.pattern().expect("the plans of one word");
            let word = super::word_constant(source, offset).expect(source);
            assert_eq!(word.steps(), planned.steps(), "the steps of {source:?}");
            assert_eq!(word.as_pure(), planned.as_pure(), "the value of {source:?}");
            for (begin, end) in spans() {
                assert_eq!(
                    format!("{:?}", word.query_arc(begin, end)),
                    format!("{:?}", planned.query_arc(begin, end)),
                    "{source:?} over {begin:?}..{end:?}"
                );
            }
        }
        // A rest, the two words that the parser refuses, a marked step, a
        // broken tail, and a text of two words are no constant of one word.
        for source in [
            "-", "~", ".", "_", "^bd", "bd sd", "bd:~", "bd:", "bd*2", "",
        ] {
            assert!(super::word_constant(source, 0).is_none(), "{source:?}");
        }
        let haps = |source: &str| {
            mini(source)
                .unwrap_or_else(|error| panic!("{source:?}: {error}"))
                .query_arc_sorted(Fraction::ZERO, Fraction::ONE)
                .len()
        };
        assert_eq!(haps("~"), 0, "a rest is silence");
        assert!(mini(".").is_err(), "a lone dot is refused");
        assert_eq!(haps("^bd"), 1, "a marked step of one word still plays");
    }

    #[test]
    fn the_plans_of_a_text_answer_as_its_constructs_level_by_level() {
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) {
                super::LEVEL_BY_LEVEL.set(false);
            }
        }
        let generated = sources_of_whole_parts(8000);
        let sources = RHYTHMS
            .iter()
            .copied()
            .chain(generated.iter().map(String::as_str));
        let mut room = super::Room::default();
        // The texts with a layer that reads the plans, and those that
        // list each step.
        let (mut reading, mut listed) = (0, 0);
        for source in sources {
            let Ok(Some(planned)) = super::Planned::read(source, 7, &mut room) else {
                continue;
            };
            let reads = planned.reads_plans();
            let Some(asked) = planned.pattern() else {
                continue;
            };
            match reads {
                true => reading += 1,
                false => listed += 1,
            }
            super::LEVEL_BY_LEVEL.set(true);
            let _restore = Restore;
            let level = super::mini_at(source, 7).expect("the plans read the source");
            assert_eq!(asked.steps(), level.steps(), "the steps of {source}");
            assert_eq!(asked.as_pure(), level.as_pure(), "the constant of {source}");
            // A walked handle has no table: each query walks the pattern.
            let walked = asked.mark_walked();
            for (begin, end) in spans() {
                assert_eq!(
                    format!("{:?}", walked.query_arc(begin, end)),
                    format!("{:?}", level.query_arc(begin, end)),
                    "a walk of {source} over {begin:?}..{end:?}"
                );
            }
            for (begin, end) in spans() {
                assert_eq!(
                    format!("{:?}", asked.try_query_arc_sorted(begin, end).err()),
                    format!("{:?}", level.try_query_arc_sorted(begin, end).err()),
                    "the refusal of {source} over {begin:?}..{end:?}"
                );
                assert_eq!(
                    format!("{:?}", asked.query_arc(begin, end)),
                    format!("{:?}", level.query_arc(begin, end)),
                    "{source} over {begin:?}..{end:?}"
                );
            }
            let twice = Fraction::int(2);
            let (begin, end) = spans()[2];
            assert_eq!(
                format!("{:?}", asked.fast(twice).query_arc(begin, end)),
                format!("{:?}", level.fast(twice).query_arc(begin, end)),
                "{source} at twice the speed"
            );
            // A constant cuts each step that is longer than a cycle.
            let one = rustel_core::pure(Value::F64(1.0));
            let under = |pattern: &Pattern| compose(pattern, &one, ComposeOp::Set, Alignment::In);
            assert_eq!(
                format!("{:?}", under(&asked).query_arc(begin, end)),
                format!("{:?}", under(&level).query_arc(begin, end)),
                "{source} under a constant"
            );
        }
        assert!(reading > 200, "{reading} texts read their plans");
        assert!(listed > 500, "{listed} texts list their steps");
    }

    #[test]
    fn plain_values_answer_as_one_node_for_each_value() {
        for source in RHYTHMS {
            let one_node = super::mini_at(source, 7).expect(source);
            let each_value = one_node_for_each_value(source, 7);
            assert_eq!(
                one_node.steps(),
                each_value.steps(),
                "the steps of {source}"
            );
            // A function that reads its argument takes a `pure` another way.
            assert_eq!(
                one_node.as_pure(),
                each_value.as_pure(),
                "the constant of {source}"
            );
            for (begin, end) in spans() {
                assert_eq!(
                    format!("{:?}", one_node.try_query_arc_sorted(begin, end).err()),
                    format!("{:?}", each_value.try_query_arc_sorted(begin, end).err()),
                    "the refusal of {source} over {begin:?}..{end:?}"
                );
                assert_eq!(
                    format!("{:?}", one_node.query_arc(begin, end)),
                    format!("{:?}", each_value.query_arc(begin, end)),
                    "{source} over {begin:?}..{end:?}"
                );
            }
        }
    }

    /// The step count that each construct reports inside another construct,
    /// which a stack and a polymeter of them read.
    #[test]
    fn plain_values_report_the_steps_of_one_node_for_each_value() {
        for source in RHYTHMS {
            for outer in ["{{{}}}%4", "{{{}, x y z}}", "[{}, x y]", "{}?"] {
                let source = outer.replace("{}", source);
                let Ok(one_node) = super::mini_at(&source, 0) else {
                    continue;
                };
                assert_eq!(
                    one_node.steps(),
                    one_node_for_each_value(&source, 0).steps(),
                    "the steps of {source}"
                );
            }
        }
    }

    /// Plain values at a whole rate for each cycle are one node: one layer
    /// of steps that each lie in a cycle, and one rate a cycle.
    #[test]
    fn plain_values_at_a_rate_for_each_cycle_are_one_node() {
        for (source, plain) in [
            ("a*<1 2 4>", true),
            ("bd*<1 2 4 8>, hh*<4 8 16 32>", true),
            ("[a b]*<2 3> c", true),
            ("<a b c>*<2 1>", true),
            // A rate that is no whole number, and two rates in a cycle.
            ("a*<2 1.5>", false),
            ("a*<2 [3 4]>", false),
            ("a*<2@2 3>", false),
            ("a*<1@1.5 2@0.5>", false),
            ("a*[[1 ~ 2 3]/3]", false),
            // A step that its cycle cut lies in the cycle.
            ("[a/2 b]*<1 2>", true),
            // Two layers, and a step that crosses its cycle.
            ("[a,b]*<1 2>", false),
            ("a/2*<1 2>", false),
            ("<a b>/2*<2 1>", false),
        ] {
            let built = super::mini_at(source, 0).expect(source);
            assert_eq!(built.is_sequence(), plain, "{source}");
        }
    }

    #[test]
    fn plain_values_at_a_constant_speed_are_one_node() {
        for (source, plain) in [
            ("a [b c] ~ [d,e]", true),
            ("<[a b]!3 [c d e]>", true),
            ("a b@2 c", true),
            ("a*2 b c", true),
            ("a(3,8)", true),
            ("a(3,8,2) [b,c](5,8)", true),
            // A step that crosses its cycle is cut where the cycle ends.
            ("a/2 b", true),
            ("a <b c>/2 d", true),
            ("bd*2, hh*4, <a b>/2", true),
            // An atom is a `pure` on both sides, and one value a cycle
            // is one node on both sides.
            ("a", false),
            ("<a>", false),
            ("a? b", false),
        ] {
            let built = super::mini_at(source, 0).expect(source);
            assert_eq!(
                !built.same_construction(&one_node_for_each_value(source, 0)),
                plain,
                "{source}"
            );
        }
    }

    #[test]
    fn flat_stack_and_weight_fast_paths_match_full_ast() {
        for source in [
            "bd ~ sd ~, hh*8",
            "bd, hh*4, ~ sd",
            "<c3 e3>, <g3 b3>",
            "[bd sd], hh(3,8)",
            "~, bd*2",
            "~, ~",
            "c3@2 e3 g3",
            "c3@ e3@3 ~@2",
            "<intro@2 verse@4 chorus@4>",
            "<a@ b@3 ~@2>",
            "bd:4!4",
            "perc:2(7,12,3)",
            "<[~ D:g4:.6!3] [D:g4 A:d4]>",
            "bd:4@2 sd",
            "c3@2 e3, g3 b3@3",
        ] {
            let fast = super::mini_at(source, 100).expect(source);
            let slow = {
                let ast = parse(source).expect(source);
                let _scope = super::BuildSource::install_at(source, 100);
                let pat = super::build_validated(&ast);
                let meta = super::step_metadata(&ast).expect(source);
                pat.with_steps(Some(meta.steps))
            };
            assert_eq!(fast.steps(), slow.steps(), "steps mismatch on {source}");
            let haps_fast = fast.query_arc_sorted(Fraction::ZERO, Fraction::from(8));
            let haps_slow = slow.query_arc_sorted(Fraction::ZERO, Fraction::from(8));
            assert_eq!(haps_fast.len(), haps_slow.len(), "len mismatch on {source}");
            for (i, (a, b)) in haps_fast.iter().zip(&haps_slow).enumerate() {
                assert_eq!(a.whole, b.whole, "whole[{i}] on {source}");
                assert_eq!(a.part, b.part, "part[{i}] on {source}");
                assert_eq!(a.value, b.value, "value[{i}] on {source}");
                assert_eq!(
                    a.context.iter().collect::<Vec<_>>(),
                    b.context.iter().collect::<Vec<_>>(),
                    "context[{i}] on {source}"
                );
            }
        }
    }

    #[test]
    fn rest_huge_replication_and_rate_do_not_exhaust_fractions() {
        for source in ["~!1e18", "~*1e18", "~!1000000", "~*1000000"] {
            let pat = super::mini_at(source, 0).expect(source);
            let haps = pat.query_arc_sorted(Fraction::ZERO, Fraction::ONE);
            assert!(haps.is_empty(), "expected silence for {source}");
        }
    }

    #[test]
    fn euclid_plan_localizes_each_atom_id_once() {
        let source = "x, <y(126,128) z w v>";
        let fast = super::mini_at(source, 0).expect(source);
        let slow = {
            let ast = parse(source).expect(source);
            let _scope = super::BuildSource::install_at(source, 0);
            let pat = super::build_validated(&ast);
            let meta = super::step_metadata(&ast).expect(source);
            pat.with_steps(Some(meta.steps))
        };
        let haps_fast = fast.query_arc_sorted(Fraction::ZERO, Fraction::from(4));
        let haps_slow = slow.query_arc_sorted(Fraction::ZERO, Fraction::from(4));
        assert_eq!(haps_fast.len(), haps_slow.len());
        let y_count = haps_fast
            .iter()
            .filter(|h| h.value == Value::Str("y".into()))
            .count();
        assert_eq!(y_count, 126, "all 126 euclid pulses must play y, not x");
        for (i, (a, b)) in haps_fast.iter().zip(&haps_slow).enumerate() {
            assert_eq!(a.whole, b.whole, "whole[{i}]");
            assert_eq!(a.part, b.part, "part[{i}]");
            assert_eq!(a.value, b.value, "value[{i}]");
            assert_eq!(
                a.context.iter().collect::<Vec<_>>(),
                b.context.iter().collect::<Vec<_>>(),
                "context[{i}]"
            );
        }
    }

    #[test]
    fn flat_paths_match_full_ast_for_underscore_weights_ranges_and_whitespace() {
        for source in [
            "bd _3 sd",
            "bd _ sd",
            "bd _3 sd, hh _2 cp",
            "1..3",
            "1.. 3",
            "1 .. 3",
            "-2 .. 2",
            "bd\nsd",
            "bd\tsd",
            "bd\r\nsd",
            "<bd\nsd>",
            "bd\n, sd",
            "bd sd,\nhh\ncp",
        ] {
            let fast = super::mini_at(source, 0).expect(source);
            let slow = {
                let ast = parse(source).expect(source);
                let _scope = super::BuildSource::install_at(source, 0);
                let pat = super::build_validated(&ast);
                let meta = super::step_metadata(&ast).expect(source);
                pat.with_steps(Some(meta.steps))
            };
            assert_eq!(fast.steps(), slow.steps(), "steps mismatch on {source:?}");
            let haps_fast = fast.query_arc_sorted(Fraction::ZERO, Fraction::from(4));
            let haps_slow = slow.query_arc_sorted(Fraction::ZERO, Fraction::from(4));
            assert_eq!(
                haps_fast.len(),
                haps_slow.len(),
                "len mismatch on {source:?}"
            );
            for (i, (a, b)) in haps_fast.iter().zip(&haps_slow).enumerate() {
                assert_eq!(a.whole, b.whole, "whole[{i}] on {source:?}");
                assert_eq!(a.part, b.part, "part[{i}] on {source:?}");
                assert_eq!(a.value, b.value, "value[{i}] on {source:?}");
                assert_eq!(
                    a.context.iter().collect::<Vec<_>>(),
                    b.context.iter().collect::<Vec<_>>(),
                    "context[{i}] on {source:?}"
                );
            }
        }
        // `+1 .. 3` is a parse error in the grammar and must not be accepted by `flat_range`.
        assert!(parse("+1 .. 3").is_err());
        assert!(super::mini_at("+1 .. 3", 0).is_err());
        // Verify exact multi-line highlight span for `"bd\nsd"`.
        let nl = super::mini_at("bd\nsd", 0).expect("bd\\nsd");
        let haps = nl.query_arc_sorted(Fraction::ZERO, Fraction::ONE);
        assert_eq!(haps[0].context.iter().collect::<Vec<_>>(), vec![(1, 4)]);
    }
}
