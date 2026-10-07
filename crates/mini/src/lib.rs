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

    fn depth(&self) -> u32 {
        fn child_depth<'a>(children: impl Iterator<Item = &'a Ast>) -> u32 {
            children.map(Ast::depth).max().unwrap_or(0)
        }

        let nested = match self {
            Ast::Atom { .. } | Ast::Silence { .. } => return 1,
            Ast::Seq { items, .. } => child_depth(items.iter().map(|step| &step.ast)),
            Ast::Replicated { pat, .. } | Ast::Degrade { pat, .. } => pat.depth(),
            Ast::Alt { lanes, .. } => child_depth(
                lanes
                    .iter()
                    .flat_map(|lane| lane.items.iter().map(|step| &step.ast)),
            ),
            Ast::Stack { items, .. } | Ast::Choose { items, .. } | Ast::Feet { items, .. } => {
                child_depth(items.iter())
            }
            Ast::Polymeter {
                items,
                steps_per_cycle,
                ..
            } => child_depth(
                items
                    .iter()
                    .chain(steps_per_cycle.iter().map(|item| item.as_ref())),
            ),
            Ast::Fast { pat, factor, .. } | Ast::Slow { pat, factor, .. } => {
                pat.depth().max(factor.depth())
            }
            Ast::Euclid {
                pat,
                pulses,
                steps,
                rotation,
                ..
            } => pat
                .depth()
                .max(pulses.ast.depth())
                .max(steps.ast.depth())
                .max(
                    rotation
                        .iter()
                        .map(|step| step.ast.depth())
                        .max()
                        .unwrap_or(0),
                ),
            Ast::Tail { pat, element, .. } => pat.depth().max(element.depth()),
            Ast::Range { start, end, .. } => start.depth().max(end.depth()),
        };
        nested.saturating_add(1)
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

struct Parser<'a> {
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

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Parser {
            src: src.as_bytes(),
            pos: 0,
            seed: 0,
            depth: 0,
            expected_number_at: None,
            aligned_tail_at: None,
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

    fn ensure_ast_depth(&self, ast: Ast) -> Result<Ast, ParseError> {
        let depth = ast.depth();
        if depth > MAX_MINI_AST_DEPTH {
            return self.ast_depth_error(depth);
        }
        Ok(ast)
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
    fn parse_top(&mut self) -> Result<Ast, ParseError> {
        self.parse_aligned(None, true)
    }

    /// Length of the maximal step-character run at `at`.
    ///
    /// ```pegjs
    /// step_char = unicode_letter / [0-9~] / "-" / "#" / "." / "^" / "_"
    /// step = ws chars:step_char+ ws !{ const s = chars.join("");
    ///                                  return (s === ".") || (s === "_") }
    ///        { return new AtomStub(chars.join("")) }
    /// ```
    fn step_run_len(&self, at: usize) -> usize {
        let mut end = at;
        while let Some(len) = self.word_char_len(end) {
            end += len;
        }
        end - at
    }

    fn word_char_len(&self, at: usize) -> Option<usize> {
        let byte = *self.src.get(at)?;
        if byte.is_ascii() {
            return is_step_char(char::from(byte)).then_some(1);
        }
        let value = std::str::from_utf8(&self.src[at..]).ok()?.chars().next()?;
        is_step_char(value).then(|| value.len_utf8())
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
    ) -> Result<Ast, ParseError> {
        let start = self.pos;
        let mut terminators = vec![b','];
        if allow_choose_and_feet {
            terminators.extend_from_slice(b"|.");
        }
        if let Some(closing) = closing {
            terminators.push(closing);
        }
        let mut items = vec![self.parse_seq(&terminators)?];
        self.ws();
        let Some(delimiter) = self.peek() else {
            return Ok(items.pop().unwrap());
        };
        if closing == Some(delimiter)
            || delimiter != b',' && (!allow_choose_and_feet || !matches!(delimiter, b'|' | b'.'))
            || (delimiter == b'.' && !self.at_feet_dot())
        {
            return Ok(items.pop().unwrap());
        }
        while self.peek() == Some(delimiter) {
            // Only a BARE `.` separates feet; `.4` is an atom and `..` is the
            // range postfix.
            if delimiter == b'.' && !self.at_feet_dot() {
                break;
            }
            self.pos += 1;
            items.push(self.parse_seq(&terminators)?);
            self.ws();
        }
        let span = Span {
            start,
            end: self.pos,
        };
        self.aligned_tail_at = Some((self.pos, delimiter));
        let ast = match delimiter {
            b',' => Ast::Stack { items, span },
            b'|' => {
                let seed = self.seed;
                self.seed += 1;
                Ast::Choose { items, seed, span }
            }
            b'.' => {
                // krill's `dot_tail` allocates a random seed even though the
                // feet compiler does not use it. Later `?`/`|` nodes observe
                // that global seed, so omitting this increment changes music.
                let seed = self.seed;
                self.seed += 1;
                Ast::Feet { items, seed, span }
            }
            _ => unreachable!(),
        };
        self.ensure_ast_depth(ast)
    }

    fn parse_seq(&mut self, terminators: &[u8]) -> Result<Ast, ParseError> {
        let start = self.pos;
        self.ws();
        let steps_source = if self.peek() == Some(b'^') {
            self.pos += 1;
            true
        } else {
            false
        };
        let mut items = Vec::new();
        loop {
            // If the previous slice's PEG production consumed separator
            // whitespace, its ElementStub ends after it. A numeric postfix
            // does not consume following whitespace, so that whitespace
            // instead belongs to the next ElementStub. The retained end tells
            // us which side owns it without changing semantic parser state.
            let element_start = items
                .last()
                .map_or(self.pos, |step: &Step| step.private_span.end);
            self.ws();
            match self.peek() {
                None => break,
                // A `.` only ends the sequence when it is a bare feet
                // separator; otherwise it starts an atom such as `.4`.
                Some(b'.')
                    if terminators.contains(&b'.') && (!self.at_feet_dot() || items.is_empty()) => {
                }
                Some(c) if terminators.contains(&c) => break,
                _ => {}
            }
            if !self.can_start_slice() {
                break;
            }
            items.push(self.parse_step_at(element_start)?);
        }
        if items.is_empty() {
            return self.expected(if steps_source {
                PegExpectation::SequenceStartAfterMarker
            } else {
                PegExpectation::SequenceStart
            });
        }
        self.ensure_ast_depth(Ast::Seq {
            items,
            steps_source,
            span: Span {
                start,
                end: self.pos,
            },
        })
    }

    fn can_start_slice(&self) -> bool {
        match self.peek() {
            Some(b'[' | b'<' | b'{') => true,
            Some(_) => {
                let len = self.step_run_len(self.pos);
                len > 0
            }
            None => false,
        }
    }

    fn parse_required_atom(&mut self) -> Result<Ast, ParseError> {
        let whitespace_start = self.pos;
        self.ws();
        if !self.can_start_slice() {
            return self.expected(PegExpectation::SliceStart);
        }
        self.pos = whitespace_start;
        self.parse_atom()
    }

    fn parse_required_step(&mut self) -> Result<Step, ParseError> {
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
    fn parse_step_at(&mut self, element_start: usize) -> Result<Step, ParseError> {
        let base = self.parse_atom_at(element_start)?;
        let mut element_end = score_whitespace_end(self.src, self.pos);
        let mut weight = 1.0;
        let mut reps = 1.0;

        // Ops in source order. `Replicate` is a placeholder: it materialises
        // once, with the final accumulated count.
        enum Op {
            Fast(Ast),
            Slow(Ast),
            Replicate,
            Euclid {
                pulses: Box<Step>,
                steps: Box<Step>,
                rotation: Option<Box<Step>>,
            },
            Degrade {
                amount: Option<f64>,
                seed: u32,
            },
            Tail(Ast),
            Range(Ast),
        }
        let mut ops: Vec<Op> = Vec::new();

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
                    ops.push(Op::Fast(factor));
                }
                Some(b'/') => {
                    self.pos += 1;
                    let factor = self.parse_required_atom()?;
                    element_end = score_whitespace_end(self.src, self.pos);
                    ops.push(Op::Slow(factor));
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
                    ops.retain(|o| !matches!(o, Op::Replicate));
                    ops.push(Op::Replicate);
                    element_end = self.pos;
                }
                Some(b'(') => {
                    self.pos += 1;
                    let pulses = Box::new(self.parse_required_step()?);
                    self.ws();
                    if self.peek() != Some(b',') {
                        return self.expected(PegExpectation::EuclidComma);
                    }
                    self.pos += 1;
                    let steps = Box::new(self.parse_required_step()?);
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
                        Some(Box::new(self.parse_step_at(self.pos)?))
                    };
                    self.ws();
                    if self.peek() != Some(b')') {
                        return self.expected(PegExpectation::EuclidRotationClose);
                    }
                    self.pos += 1;
                    element_end = self.pos;
                    ops.push(Op::Euclid {
                        pulses,
                        steps,
                        rotation,
                    });
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
                    ops.push(Op::Degrade { amount, seed });
                }
                Some(b':') => {
                    self.pos += 1;
                    let element = self.parse_required_atom()?;
                    element_end = score_whitespace_end(self.src, self.pos);
                    ops.push(Op::Tail(element));
                }
                Some(b'.') if self.src.get(self.pos + 1) == Some(&b'.') => {
                    self.pos += 2;
                    let end = self.parse_required_atom()?;
                    element_end = score_whitespace_end(self.src, self.pos);
                    ops.push(Op::Range(end));
                }
                _ => break,
            }

            if ops.len() > MAX_MINI_AST_DEPTH as usize {
                return self.ast_depth_error(ops.len() as u32);
            }
        }

        let start = base.span().start;
        let mut ast = base;
        for op in ops {
            ast = match op {
                Op::Fast(f) => Ast::Fast {
                    pat: Box::new(ast),
                    factor: Box::new(f),
                    span: Span {
                        start,
                        end: self.pos,
                    },
                },
                Op::Slow(f) => Ast::Slow {
                    pat: Box::new(ast),
                    factor: Box::new(f),
                    span: Span {
                        start,
                        end: self.pos,
                    },
                },
                Op::Replicate => Ast::Replicated {
                    pat: Box::new(ast),
                    amount: reps,
                    span: Span {
                        start,
                        end: self.pos,
                    },
                },
                Op::Euclid {
                    pulses,
                    steps,
                    rotation,
                } => Ast::Euclid {
                    pat: Box::new(ast),
                    pulses,
                    steps,
                    rotation,
                    span: Span {
                        start,
                        end: self.pos,
                    },
                },
                Op::Degrade { amount, seed } => Ast::Degrade {
                    pat: Box::new(ast),
                    amount,
                    seed,
                    span: Span {
                        start,
                        end: self.pos,
                    },
                },
                Op::Tail(element) => Ast::Tail {
                    pat: Box::new(ast),
                    element: Box::new(element),
                    span: Span {
                        start,
                        end: self.pos,
                    },
                },
                Op::Range(end) => Ast::Range {
                    start: Box::new(ast),
                    end: Box::new(end),
                    span: Span {
                        start,
                        end: self.pos,
                    },
                },
            };
            ast = self.ensure_ast_depth(ast)?;
        }

        Ok(Step {
            ast,
            private_span: Span {
                start: element_start,
                end: element_end,
            },
            weight,
            replicate: 1,
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

    fn parse_atom(&mut self) -> Result<Ast, ParseError> {
        let private_start = self.pos;
        self.parse_atom_at(private_start)
    }

    /// All recursively nested atoms pass through this boundary.
    fn parse_atom_at(&mut self, private_start: usize) -> Result<Ast, ParseError> {
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
        let parsed = self
            .parse_atom_at_bounded(private_start)
            .and_then(|ast| self.ensure_ast_depth(ast));
        self.depth -= 1;
        parsed
    }

    fn parse_atom_at_bounded(&mut self, private_start: usize) -> Result<Ast, ParseError> {
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
                Ok(inner)
            }
            Some(b'<') => {
                self.pos += 1;
                let mut lanes = vec![self.parse_seq(b",>")?];
                self.ws();
                while self.peek() == Some(b',') {
                    self.pos += 1;
                    lanes.push(self.parse_seq(b",>")?);
                    self.ws();
                }
                if self.peek() != Some(b'>') {
                    return self.expected(PegExpectation::SlowSequenceTail);
                }
                self.pos += 1;
                let lanes = lanes
                    .into_iter()
                    .map(|lane| match lane {
                        Ast::Seq {
                            items,
                            steps_source,
                            ..
                        } => AltLane {
                            items,
                            steps_source,
                        },
                        _ => unreachable!("parse_seq always constructs a sequence"),
                    })
                    .collect();
                Ok(Ast::Alt {
                    lanes,
                    span: Span {
                        start,
                        end: self.pos,
                    },
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
                    Some(Box::new(self.parse_atom()?))
                } else {
                    None
                };
                let items = match inner {
                    Ast::Stack { items, .. } => items,
                    other => vec![other],
                };
                Ok(Ast::Polymeter {
                    items,
                    steps_per_cycle,
                    span: Span {
                        start,
                        end: self.pos,
                    },
                })
            }
            Some(_) if self.word_char_len(self.pos).is_some() => {
                while let Some(len) = self.word_char_len(self.pos) {
                    self.pos += len;
                }
                let value = std::str::from_utf8(&self.src[start..self.pos])
                    .unwrap()
                    .to_string();
                if matches!(value.as_str(), "." | "_") {
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
                Ok(Ast::Atom {
                    value,
                    private_span: Span {
                        start: private_start,
                        end: score_whitespace_end(self.src, self.pos),
                    },
                    span: Span {
                        start: location_start,
                        end: self.pos,
                    },
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
        let text = std::str::from_utf8(&self.src[start..self.pos]).unwrap();
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
    if c.is_ascii() {
        c.is_ascii_alphanumeric() || matches!(c, '~' | '-' | '#' | '.' | '^' | '_')
    } else {
        krill_unicode_generated::is_letter(c)
    }
}

pub fn parse(src: &str) -> Result<Ast, ParseError> {
    let mut p = Parser::new(src);
    let ast = p.parse_top()?;
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
    Ok(ast)
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
fn leaf_span(src: &[u8], span: &Span) -> (usize, usize) {
    let trailing_end = score_whitespace_end(src, span.end);
    let spaces = src[span.end..trailing_end]
        .iter()
        .filter(|byte| **byte == b' ')
        .count();
    (span.start, trailing_end.saturating_sub(spaces))
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

/// Expand `!n` replication, then divide one cycle among the steps in
/// proportion to their `@n` weights: plain `fastcat` when every weight is 1,
/// `timecat` otherwise.
fn build_seq(items: &[Step]) -> Pattern {
    let expanded: Vec<(Fraction, &Ast)> = expanded_steps(items)
        .map(|(step, weight)| (weight, &step.ast))
        .collect();
    if expanded.is_empty() {
        return silence();
    }

    // A single step always fills the whole cycle, so its weight is irrelevant
    // and `compress(0, 1)` is the identity - except that `fastGap` carries a
    // `splitQueries`, which would fragment a multi-cycle query. Short-circuit,
    // mirroring `slowcat`'s `if (pats.length == 1) return pats[0]`. It comes
    // before the zero-sum guard: a single step divides nothing, and pinned
    // `stepcat` reifies a singleton before it computes a total, so
    // `bd@1e-10` plays.
    if expanded.len() == 1 {
        return build_validated(expanded[0].1);
    }

    // A zero weight total is silence: negative weights that cancel
    // (`bd@-1 sd@1`) or weights that round to zero (`bd@1e-10 sd@1e-10`) have
    // no windows to divide the cycle into.
    if total_weight(items) == Fraction::ZERO {
        return silence();
    }

    // All-equal weights: plain fastcat.
    if expanded.iter().all(|(w, _)| *w == Fraction::ONE) {
        return fastcat(
            expanded
                .iter()
                .map(|(_, ast)| build_validated(ast))
                .collect(),
        );
    }

    // Weighted: `timecat`, which is
    //   stack(pat_i._compressSpan(begin_i/total, end_i/total))
    // and `_compress` is `_fastGap(1/(e-b))._late(b)` - NOT plain `fast`, which
    // would repeat the step instead of leaving a gap.
    let windows = sequence_windows(items)
        .expect("Mini weight windows must be validated before pattern construction");
    let mut parts = Vec::new();
    for ((w, a), (begin, end)) in expanded.iter().zip(windows) {
        // A weight can resolve to zero: the bounded `Fraction::from_f64`
        // rounds a near-zero `@1e-10` to 0/1. Its window is empty, and
        // `compress(b, b)` signals a "Division by zero" query error. Only a
        // `queryArc` boundary drains that signal. No boundary is active at
        // build time, so the error stays pending, and a later raw query or
        // transform program on this thread reads it as its own. Use a silent
        // part instead, as the stepwise and stepcat builders do.
        if *w == Fraction::ZERO {
            parts.push(silence());
            continue;
        }
        parts.push(build_validated(a).compress(begin, end));
    }
    stack(parts)
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
        cell.borrow().as_ref().map(|(src, offset)| {
            let (start, end) = leaf_span(src.as_bytes(), span);
            // Hap context is QUOTED-relative - measured against the quoted
            // source, so the pinned fixture for `'bd sd'` is
            // `{start:1,end:3}`, not `{0,2}`. `leaf_locations` keeps its own
            // separately-pinned unquoted convention; only the hap context
            // carries the +1. The build offset then shifts it
            // document-absolute.
            (start + 1 + offset, end + 1 + offset)
        })
    })
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
fn argument_context(ast: &Ast) -> Vec<(usize, usize)> {
    BUILD_SOURCE.with(|cell| {
        let Some((src, offset)) = cell.borrow().as_ref().cloned() else {
            return Vec::new();
        };
        let mut leaves = Vec::new();
        collect_leaf_spans(ast, &mut leaves);
        leaves
            .into_iter()
            .map(|span| {
                let (start, end) = leaf_span(src.as_bytes(), &span);
                (start + 1 + offset, end + 1 + offset)
            })
            .collect()
    })
}

fn with_argument_context(pattern: Pattern, spans: Vec<(usize, usize)>) -> Pattern {
    if spans.is_empty() {
        pattern
    } else {
        pattern.with_added_context(spans)
    }
}

fn build_validated(ast: &Ast) -> Pattern {
    match ast {
        // krill's `step_char` includes `-` and `~`, so both are legal inside a
        // token (`-1`, `c-3`). But a token that is ENTIRELY `-` or `~` is a
        // rest: it occupies its slot and produces nothing.
        //
        // `a - b [- c]` yields only a, b, c, with the `-` slots silent rather
        // than atoms literally named "-".
        Ast::Atom { value, .. } if value == "-" || value == "~" => silence(),
        Ast::Atom { value, span, .. } => match current_leaf_span(span) {
            // `pure(value).withLoc(...getLeafLocation(code, ast, offset))` -
            // the same whitespace-corrected span `getLeafLocations` reports,
            // carried as hap context for editor highlighting.
            Some(loc) => pure(atom_value(value)).with_added_context(vec![loc]),
            None => pure(atom_value(value)),
        },
        Ast::Silence { .. } => silence(),
        Ast::Seq { items, .. } => build_seq(items),
        Ast::Replicated { pat, amount, .. } => {
            let amount = native_weight(*amount);
            build_validated(pat).repeat_cycles(amount).fast(amount)
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
                return silence();
            }
            stack(
                lanes
                    .iter()
                    .map(|lane| {
                        let total = total_weight(&lane.items);
                        if total == Fraction::ZERO {
                            silence()
                        } else {
                            build_seq(&lane.items).slow(total)
                        }
                    })
                    .collect(),
            )
        }
        Ast::Stack { items, .. } => stack(items.iter().map(build_validated).collect()),
        Ast::Polymeter {
            items,
            steps_per_cycle,
            ..
        } => {
            if items.is_empty() {
                return silence();
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
            stack(
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
            )
        }
        Ast::Choose { items, seed, .. } => {
            choose_cycles(items.iter().map(build_validated).collect(), *seed)
        }
        Ast::Feet { items, .. } => fastcat(items.iter().map(build_validated).collect()),
        Ast::Fast { pat, factor, .. } => match static_fraction_site(factor) {
            // The eager path scalarizes the factor; the factor atom's span
            // is appended so every hap still carries it.
            Some(value) => {
                with_argument_context(build_validated(pat).fast(value), argument_context(factor))
            }
            None => {
                let factor =
                    build_validated(factor).fmap(|value| match fraction_site_value(value) {
                        Some(factor) => Value::F64(factor.to_f64()),
                        None => {
                            if let Some(message) = pinned_fraction_error(value) {
                                rustel_core::signal_query_error(|| message);
                            }
                            Value::Undefined
                        }
                    });
                apply_patterned_fast(build_validated(pat), factor)
            }
        },
        Ast::Slow { pat, factor, .. } => {
            if let Some(value) = static_fraction_site(factor) {
                return with_argument_context(
                    build_validated(pat).slow(value),
                    argument_context(factor),
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
            apply_patterned_fast(build_validated(pat), reciprocal)
        }
        Ast::Euclid {
            pat,
            pulses,
            steps,
            rotation,
            ..
        } => {
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
                let mut spans = argument_context(euclid_argument_source(&pulses.ast));
                spans.extend(argument_context(euclid_argument_source(&steps.ast)));
                if let Some(rotation) = rotation.as_deref() {
                    spans.extend(argument_context(euclid_argument_source(&rotation.ast)));
                }
                with_argument_context(
                    euclid_pattern(
                        build_validated(pat),
                        pulses_value.to_f64() as i32,
                        steps_value.to_f64() as usize,
                        rotation_value.to_f64() as isize,
                    ),
                    spans,
                )
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
                            .to_f64() as isize;
                        euclid_pattern(base.clone(), pulses, steps, rotation)
                    })
                    .inner_join()
            }
        }
        Ast::Degrade {
            pat, amount, seed, ..
        } => build_validated(pat).degrade_by_seeded(amount.unwrap_or(0.5), *seed),
        Ast::Tail { pat, element, span } => {
            // The editor highlights `bd:3` as one band. A chain of plain
            // atoms is built as one value under the span of the whole pair.
            // With one span per side, the `:` between them stays unlit.
            // Anything else (a sequence or a rest on either side) keeps
            // upstream's shape: one span per leaf.
            if let Some(values) = tail_atom_chain(pat, element)
                && let Some(loc) = current_leaf_span(span)
            {
                return pure(Value::List(values)).with_added_context(vec![loc]);
            }
            build_validated(pat).app_left_with(build_validated(element), |left, right| match left {
                Value::List(values) => {
                    let mut values = values.clone();
                    values.push(right.clone());
                    Value::List(values)
                }
                value => Value::List(vec![value.clone(), right.clone()]),
            })
        }
        Ast::Range { start, end, .. } => {
            if let (Some(start_value), Some(end_value)) =
                (static_number_fraction(start), static_number_fraction(end))
            {
                // Range expansion appends the end atom's span before the
                // start atom's span (`6-7,1-2` for `"0 .. 3"`).
                let mut spans = argument_context(end);
                spans.extend(argument_context(start));
                with_argument_context(
                    range_pattern(start_value.to_f64(), end_value.to_f64()),
                    spans,
                )
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
        }
    }
}

fn apply_patterned_fast(pattern: Pattern, factor: Pattern) -> Pattern {
    default_registry()
        .get("fast")
        .expect("native fast registration")
        .call(&[factor], pattern)
}

fn euclid_pattern(pattern: Pattern, pulses: i32, steps: usize, rotation: isize) -> Pattern {
    if steps > rustel_core::euclid::MAX_EUCLID_STEPS {
        return rustel_core::query_limit_pattern(rustel_core::QueryLimit::EuclidSteps {
            steps: steps as u64,
            limit: MAX_NATIVE_EUCLID_STEPS,
        });
    }
    match rustel_core::euclid::euclid_rot(pulses, steps, rotation) {
        Ok(mask) => {
            let structure = fastcat(
                mask.into_iter()
                    .map(|value| pure(Value::Bool(value != 0)))
                    .collect(),
            );
            pattern.struct_with(structure)
        }
        Err(_) => silence(),
    }
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
    let direction = if start <= end { 1.0 } else { -1.0 };
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
        .unwrap_or_else(|| Value::Str(value.to_owned()))
}

fn js_atom_number(value: &str) -> Option<f64> {
    match value {
        "Infinity" => return Some(f64::INFINITY),
        "-Infinity" => return Some(f64::NEG_INFINITY),
        _ => {}
    }
    for (prefix, radix) in [
        ("0x", 16u32),
        ("0X", 16),
        ("0b", 2),
        ("0B", 2),
        ("0o", 8),
        ("0O", 8),
    ] {
        if let Some(digits) = value.strip_prefix(prefix) {
            return js_radix_number(digits, radix);
        }
    }

    let bytes = value.as_bytes();
    let mut at = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
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
                && !matches!(value.as_str(), "-" | "~") =>
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

fn checked_euclid_extents(pulses: f64, steps: f64) -> Result<(i32, usize), EuclidExtentError> {
    if !pulses.is_finite()
        || !steps.is_finite()
        || pulses.fract() != 0.0
        || steps.fract() != 0.0
        || steps < 0.0
    {
        return Err(EuclidExtentError::InvalidArrayLength);
    }
    let ons = pulses.abs();
    let offs = steps - ons;
    if offs < 0.0 || ons > f64::from(u32::MAX) || offs > f64::from(u32::MAX) {
        return Err(EuclidExtentError::InvalidArrayLength);
    }
    if steps > MAX_NATIVE_EUCLID_STEPS as f64 {
        return Err(EuclidExtentError::NativeLimit {
            steps: steps as u64,
        });
    }
    Ok((pulses as i32, steps as usize))
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
        Ast::Alt { lanes, .. } => {
            let children: Vec<_> = lanes
                .iter()
                // Each lane is first built as its own fastcat, so a lane
                // contributes its weight sum multiplied by the LCM of any
                // sourced children; only then are the lanes LCM'd together.
                // Flattening here loses the lane weight sum.
                .map(|lane| sequence_metadata(&lane.items, lane.steps_source))
                .collect::<Option<Vec<_>>>()?
                .into_iter()
                .filter(|metadata| metadata.sourced)
                .collect();
            Some(StepMetadata {
                steps: if children.is_empty() {
                    Fraction::ONE
                } else {
                    children
                        .iter()
                        .try_fold(Fraction::ONE, |steps, child| steps.checked_lcm(child.steps))?
                },
                sourced: !children.is_empty(),
            })
        }
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
                // Eager `euclid`/`euclidRot` takes its structure from the
                // Euclid mask, whose fastcat has `steps` steps. The dynamic
                // register path inner-joins and has undefined `_steps`;
                // Fraction(undefined) becomes zero when a sourced receiver is
                // consumed by its parent sequence.
                steps: if eager {
                    let steps = static_number_fraction(euclid_argument_source(&steps.ast))
                        .expect("eager Euclid steps");
                    if steps == Fraction::ZERO {
                        // The empty mask IS silence, and silence carries one
                        // step.
                        Fraction::ONE
                    } else {
                        steps
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
    let children: Vec<_> = items
        .iter()
        .map(|item| step_metadata(&item.ast))
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .filter(|metadata| metadata.sourced)
        .collect();
    let nested = children
        .iter()
        .try_fold(Fraction::ONE, |steps, child| steps.checked_lcm(child.steps))?;
    Some(StepMetadata {
        steps: own.checked_mul(nested)?,
        sourced: explicitly_sourced || !children.is_empty(),
    })
}

fn aggregate_parallel_metadata(items: &[Ast]) -> Option<StepMetadata> {
    let sourced: Vec<_> = items
        .iter()
        .map(step_metadata)
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .filter(|metadata| metadata.sourced)
        .collect();
    Some(StepMetadata {
        steps: sourced
            .iter()
            .try_fold(Fraction::ONE, |steps, child| steps.checked_lcm(child.steps))?,
        sourced: !sourced.is_empty(),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        ErrorPhase, EuclidExtentError, MAX_MINI_AST_DEPTH, MAX_MINI_DEPTH, MAX_NATIVE_EUCLID_STEPS,
        build, checked_euclid_extents, is_step_char, leaf_locations, mini, parse,
    };
    use rustel_core::QueryLimit;
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

        let pattern = mini("a(3,16384)").expect("the accepted boundary must construct");
        assert_eq!(
            pattern
                .query_arc_sorted(Fraction::ZERO, Fraction::ONE)
                .len(),
            3,
            "the accepted boundary must remain queryable"
        );
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
            assert_eq!(hap.context[0].1 - hap.context[0].0, token.len(), "{token}");
        }
        assert_eq!(
            haps[0].value,
            rustel_core::Value::List(vec![
                rustel_core::Value::Str("bd".into()),
                rustel_core::Value::F64(3.0)
            ])
        );

        let haps = mini("a:b:c")
            .expect("parses")
            .query_arc(Fraction::ZERO, Fraction::ONE);
        assert_eq!(haps[0].context.len(), 1, "{:?}", haps[0].context);
        assert_eq!(haps[0].context[0].1 - haps[0].context[0].0, "a:b:c".len());
        assert_eq!(
            haps[0].value,
            rustel_core::Value::List(vec![
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
}
