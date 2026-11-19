//! The reader of a score text.
//!
//! One pass reads the text into the [`ScoreProgram`] that the JavaScript
//! route reads from the same text after its rewrites. A double-quoted
//! string and a template are mini-notation calls, a labeled statement is a
//! lane call, and a widget call holds its slot. The reader stops at the
//! first form it does not know.
//!
//! The JavaScript route finds the labeled statements with a scan of lines
//! and brackets, and then reads the rewritten text as JavaScript. For the
//! texts the reader takes, the scan and JavaScript end each statement at
//! the same place: the reader takes a line end inside a statement, outside
//! every bracket, only where the scan also reads on.

use crate::string::decode;
use crate::{
    Held, LaneStatement, Row, ScoreExpr, ScoreFunction, ScoreProgram, ScoreProperty,
    ScoreStatement, Written,
};
use std::borrow::Cow;

/// The deepest an expression of a program nests, so a host walks a program,
/// and drops it, on any stack.
pub const MAX_NESTING: usize = 512;

/// The longest text the reader takes, in bytes.
const MAX_SOURCE_BYTES: usize = 64 * 1024;

/// The longest text fits the `u32` of each place and count that a
/// `Written` holds, so the casts of `written` need no test.
const _: () = assert!(MAX_SOURCE_BYTES <= u32::MAX as usize);

/// How deep the reader calls itself for one statement, on the stack of its
/// caller: one level for each bracket, arrow and sign around a token.
const MAX_LEVELS: usize = 96;

/// The callee of a mini-notation literal.
const MINI_CALLEE: &str = "m";

/// The method that collects a lane.
const LANE_METHOD: &str = "p";

/// What the JavaScript route does with a call of a name, where it finds the
/// call in the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallForm {
    /// Nothing: the call is read as written.
    Written,
    /// A method call of the name is a widget. The route writes the id of
    /// the widget before the arguments and, with `slot`, the number of the
    /// widget among those with a slot.
    Widget { slot: bool },
    /// A call of the name can be one widget for the page. The route then
    /// finds no widget in its arguments.
    Page,
    /// Another form, which the reader leaves to the route.
    Other,
}

/// A score as [`read`] reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct Read<'s> {
    /// The program. The number of each lane call is the place of its lane
    /// among the labeled statements, until [`Self::number_lanes`] writes
    /// the numbers of a host.
    pub program: ScoreProgram<'s>,
    /// The lanes of the text, in the order of the text: each labeled
    /// statement with code, or the last statement of a text with no label.
    pub lanes: Vec<LaneStatement>,
    /// The statement of each labeled lane, in the order of the text.
    labeled: Vec<usize>,
}

impl Read<'_> {
    /// Write `numbers` as the lane numbers of the labeled statements, in the
    /// order of the text. A list with another count writes nothing: the
    /// lane calls keep their places, and the answer is `false`.
    pub fn number_lanes(&mut self, numbers: &[u64]) -> bool {
        if numbers.len() != self.labeled.len() {
            return false;
        }
        for (statement, number) in self.labeled.iter().zip(numbers) {
            if let ScoreStatement::Expression(ScoreExpr::Method { arguments, .. }) =
                &self.program.statements[*statement]
                && let Some(place) = self.program.held.row_mut(*arguments).last_mut()
            {
                // A lane number is far below the first integer that a
                // float cannot hold.
                *place = ScoreExpr::Number(*number as f64);
            }
        }
        true
    }
}

/// The program and the lanes of `source`, or `None` for a text with a form
/// the reader does not know.
///
/// `form_of` says what the JavaScript route does with a call of a name.
///
/// The program is the one that route reads, but for the id of each widget
/// call. The id names the place of the widget in the text for an editor,
/// and a host that evaluates the program reads the slot alone, so the
/// reader writes an empty id.
pub fn read(source: &str, form_of: impl Fn(&str) -> CallForm) -> Option<Read<'_>> {
    Reader::of(source, form_of)?.program()
}

/// The line of `source`, counted from 1, where [`read`] stops: the reader
/// knows no form for what that line holds, or for the end of the line
/// before it. `None` for a text that [`read`] reads.
///
/// A host shows the line to say why a score needs JavaScript.
pub fn left_at(source: &str, form_of: impl Fn(&str) -> CallForm) -> Option<usize> {
    let Some(mut reader) = Reader::of(source, form_of) else {
        return Some(1);
    };
    if reader.program().is_some() {
        return None;
    }
    let at = reader.at.min(source.len());
    Some(reader.line_of(at.max(reader.counted)))
}

/// What lies between two tokens: a line end outside a comment, a block
/// comment after the last such line end, and a line end inside a block
/// comment. JavaScript ends a statement at the last one, and the scan of
/// the JavaScript route does not.
#[derive(Clone, Copy, Default)]
struct Gap {
    line: bool,
    comment: bool,
    comment_line: bool,
    /// The token to come is the first of its statement: a line end before
    /// it is between statements, and no statement ends there. The gap
    /// that follows a token clears it.
    fresh: bool,
}

/// Where the reader is, to go back to.
#[derive(Clone, Copy)]
struct Mark {
    at: usize,
    last: usize,
    gap: Gap,
    depth: usize,
    mini: bool,
}

/// What an expression is in the text.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Form {
    /// A name, a call, a member, an array, a mini-notation literal or
    /// anything in parentheses: a method call written after it is a call
    /// on all of it.
    Chain,
    /// A string literal alone, which is a directive as a whole statement.
    String,
    /// A number, a sum, a negation or an arrow.
    Other,
}

/// An expression, how deep it nests and what it is in the text.
struct Parsed<'s> {
    expr: ScoreExpr<'s>,
    height: usize,
    form: Form,
}

impl<'s> Parsed<'s> {
    fn chain(expr: ScoreExpr<'s>, height: usize) -> Self {
        Self {
            expr,
            height,
            form: Form::Chain,
        }
    }
}

/// The bindings, the result and the height of the body of a function.
type Body<'s> = (Vec<(Cow<'s, str>, ScoreExpr<'s>)>, ScoreExpr<'s>, usize);

struct Reader<'s, F> {
    source: &'s str,
    bytes: &'s [u8],
    form_of: F,
    /// The start of the next token.
    at: usize,
    /// The end of the last token.
    last: usize,
    /// What lies between the last token and the next.
    gap: Gap,
    /// How many brackets are open around the next token.
    depth: usize,
    /// How deep the reader calls itself.
    levels: usize,
    /// Whether a double-quoted string or a template is mini-notation: a
    /// `mini-off` comment turns it off, and `mini-on` turns it on again.
    mini: bool,
    /// How many widgets with a slot the reader is past.
    widgets: usize,
    /// How many calls that can be a widget for the page are open around
    /// the next token.
    pages: usize,
    /// The line of the byte at `counted`, from 1.
    line: usize,
    counted: usize,
    /// The expressions that the expressions read so far hold.
    held: Held<'s>,
    /// The arguments and the elements of the lists that are open, the last
    /// list last: a list joins `held` as a row when it closes.
    listed: Vec<ScoreExpr<'s>>,
}

impl<'s, F: Fn(&str) -> CallForm> Reader<'s, F> {
    /// A reader at the start of `source`. `None` for a text that the
    /// reader takes no part of: one past the size it reads, or with a line
    /// separator, which ends a line for JavaScript and not for this reader.
    fn of(source: &'s str, form_of: F) -> Option<Self> {
        if source.len() > MAX_SOURCE_BYTES
            || (!source.is_ascii() && source.contains(['\u{2028}', '\u{2029}']))
        {
            return None;
        }
        Some(Self {
            source,
            bytes: source.as_bytes(),
            form_of,
            at: 0,
            last: 0,
            gap: Gap::default(),
            depth: 0,
            levels: 0,
            mini: true,
            widgets: 0,
            pages: 0,
            line: 1,
            counted: 0,
            // A score of calls holds an expression for each seven bytes,
            // and the list takes its room in one piece.
            held: Held::with_room(source.len() / 7 + 8),
            // The lists of the score nest a few calls deep, and take their
            // room here once rather than as each one closes.
            listed: Vec::with_capacity(8),
        })
    }

    fn program(&mut self) -> Option<Read<'s>> {
        // Most scores hold a handful of statements: the lists take their
        // room in one piece, and a growing list copies its room each time.
        // Scores with no label hold no lane number either.
        let mut statements = Vec::with_capacity(8);
        let mut lanes = Vec::with_capacity(8);
        let mut labeled = Vec::new();
        // The names the score declares, each one time.
        let mut declared: Vec<&'s str> = Vec::new();
        // The code of the last statement with no label.
        let mut last_code = None;
        self.skip()?;
        while self.at < self.bytes.len() {
            self.gap.fresh = true;
            let start = self.at;
            // The word the statement starts with, which its label and its
            // own reading share: one scan reads both.
            let word = self.word();
            if let Some(label) = self.label(word)? {
                let first = self.peek();
                // A label with no code at the end of the text is none of
                // the program, as the route drops it: no statement, no
                // lane. A zero byte stands for the end of the text here
                // and nowhere else.
                if first == 0 && self.at == self.bytes.len() {
                    continue;
                }
                // A label with no code before a semicolon or a block is no
                // lane: the route leaves such a label to JavaScript.
                if matches!(first, b';' | b'{') {
                    return None;
                }
                let code = self.at;
                let body = self.expression()?;
                if body.form == Form::Other || body.height >= MAX_NESTING {
                    return None;
                }
                lanes.push(LaneStatement {
                    label: label.to_owned(),
                    body: LaneStatement::body_of(&self.source[code..self.last]),
                    line: self.line_of(start),
                });
                labeled.push(statements.len());
                statements.push(ScoreStatement::Expression(ScoreExpr::Method {
                    receiver: self.held.hold(body.expr)?,
                    method: LANE_METHOD.into(),
                    arguments: self.held.hold_row([
                        ScoreExpr::String(label.into()),
                        ScoreExpr::Number((lanes.len() - 1) as f64),
                    ])?,
                    // The score writes no such call.
                    written: Written::default(),
                }));
                self.end_statement(false)?;
                continue;
            }
            let mut ends_at_its_block = false;
            let statement = match word {
                Some("const" | "let") => {
                    // The record of the binding is of the binding alone:
                    // what the reader holds before the value it reads.
                    let reading = self.reading();
                    let ((name, value), height) = self.binding()?;
                    if height > MAX_NESTING || declared.contains(&name) {
                        return None;
                    }
                    declared.push(name);
                    ScoreStatement::Binding {
                        name: name.into(),
                        value,
                        written: self.written(start, reading),
                    }
                }
                Some("function") => {
                    let reading = self.reading();
                    self.take(8)?;
                    let name = self.bound_name()?;
                    // The JavaScript route finds a call in a name and a `(`.
                    let rewritten =
                        matches!((self.form_of)(name), CallForm::Page | CallForm::Other);
                    if declared.contains(&name) || rewritten {
                        return None;
                    }
                    declared.push(name);
                    let parameters = self.parameters()?;
                    let (bindings, result, height) = self.block(&parameters)?;
                    // The scan reads a token on the line of the `}` as one
                    // statement with the function.
                    let ended = self.at == self.bytes.len() || self.gap.line;
                    if height > MAX_NESTING || !ended || self.peek() == b';' {
                        return None;
                    }
                    ends_at_its_block = true;
                    ScoreStatement::Function {
                        name: name.into(),
                        function: ScoreFunction {
                            parameters: borrowed(&parameters),
                            bindings,
                            result,
                        },
                        written: self.written(start, reading),
                    }
                }
                _ => {
                    if self.peek() == b'{' {
                        return None;
                    }
                    let value = self.expression()?;
                    if value.form == Form::String || value.height > MAX_NESTING {
                        return None;
                    }
                    ScoreStatement::Expression(value.expr)
                }
            };
            last_code = Some((start, self.last));
            if !ends_at_its_block {
                self.end_statement(false)?;
            }
            statements.push(statement);
        }
        if lanes.is_empty() {
            lanes.push(match last_code {
                Some((start, end)) => LaneStatement {
                    label: String::new(),
                    body: LaneStatement::body_of(&self.source[start..end]),
                    line: self.line_of(start),
                },
                None => LaneStatement {
                    label: String::new(),
                    body: String::new(),
                    line: 1,
                },
            });
        }
        Some(Read {
            program: ScoreProgram {
                statements,
                held: std::mem::take(&mut self.held),
            },
            lanes,
            labeled,
        })
    }

    /// The label that the next statement starts with, as the scan of the
    /// JavaScript route finds one: a run of identifier bytes, then a colon.
    /// The reader is then at the code after the colon. `word`, when the
    /// statement starts with one, is that run: a number or a form the
    /// reader knows no word for still has its own scan.
    fn label(&mut self, word: Option<&'s str>) -> Option<Option<&'s str>> {
        let run = match word {
            Some(word) => word.len(),
            None => {
                let bytes = &self.bytes[self.at..];
                let mut run = 0;
                while run < bytes.len() && CONTINUES_AN_IDENTIFIER[usize::from(bytes[run])] {
                    run += 1;
                }
                run
            }
        };
        if run == 0 {
            return Some(None);
        }
        let bytes = &self.bytes[self.at..];
        match bytes.get(run) {
            // The label with its colon right after it, which most labels
            // are written with, reads as one token. A second colon is no
            // label: the scan reads `a::b` as no label.
            Some(&b':') if bytes.get(run + 1) != Some(&b':') => {
                // The scan starts a statement at a block comment on the
                // line of the label, and then finds no label.
                if self.gap.comment {
                    return None;
                }
                let name = &self.source[self.at..self.at + run];
                self.take(run + 1)?;
                self.gap.fresh = true;
                Some(Some(name))
            }
            // A gap can sit between the label and its colon, and the
            // reader reads past it to say which it is.
            Some(b' ' | b'\t' | 0x0C | b'\n' | b'\r' | b'/') => {
                let mark = self.mark();
                let name = &self.source[self.at..self.at + run];
                self.take(run)?;
                let labeled = self.peek() == b':' && self.bytes.get(self.at + 1) != Some(&b':');
                if !labeled {
                    self.reset(mark);
                    return Some(None);
                }
                // The scan starts a statement at a block comment on the
                // line of the label, and then finds no label.
                if mark.gap.comment {
                    return None;
                }
                self.gap.fresh = true;
                self.take(1)?;
                self.gap.fresh = true;
                Some(Some(name))
            }
            // No colon can follow the run without a gap: no label, and no
            // token to take and put back.
            _ => Some(None),
        }
    }

    /// `const name = value` or `let name = value`: the name, the value and
    /// the height of the value.
    fn binding(&mut self) -> Option<((&'s str, ScoreExpr<'s>), usize)> {
        self.take(self.word()?.len())?;
        let name = self.bound_name()?;
        if self.peek() != b'=' || matches!(self.bytes.get(self.at + 1), Some(b'=' | b'>')) {
            return None;
        }
        self.take(1)?;
        let value = self.expression()?;
        Some(((name, value.expr), value.height))
    }

    /// The end of a statement: a semicolon, a line end before a token that
    /// starts a statement, the end of the text, or the `}` of a block.
    fn end_statement(&mut self, block: bool) -> Option<()> {
        match self.peek() {
            0 if self.at == self.bytes.len() => Some(()),
            b';' => self.take(1),
            b'}' if block => Some(()),
            first => {
                // JavaScript reads on at `in` and `instanceof`.
                let starts_statement = (first.is_ascii_alphanumeric()
                    || matches!(first, b'_' | b'$' | b'\'' | b'"'))
                    && !self.reads_on_at_in_or_instanceof();
                (self.gap.line && starts_statement).then_some(())
            }
        }
    }

    /// Whether the reader is at the word `in` or `instanceof`, which
    /// JavaScript reads on into the statement before it. The two words
    /// say so with no scan of the word around them.
    fn reads_on_at_in_or_instanceof(&self) -> bool {
        let bytes = &self.bytes[self.at..];
        let length = match bytes.first() {
            Some(b'i') if bytes.get(1) == Some(&b'n') => {
                if bytes.get(2) == Some(&b's') && bytes.starts_with(b"instanceof") {
                    10
                } else {
                    2
                }
            }
            _ => return false,
        };
        !bytes
            .get(length)
            .is_some_and(|byte| is_identifier_byte(*byte))
    }

    /// An expression: an arrow, or a sum of products.
    fn expression(&mut self) -> Option<Parsed<'s>> {
        if self.levels >= MAX_LEVELS {
            return None;
        }
        match self.peek() {
            // The rule each fast path keeps: a fast path gives up where
            // the general path gives up. A string or a number is many
            // of the values of a score, and reads with no pass through
            // the arrow and the sum.
            b'\'' | b'"' | b'`' | b'0'..=b'9' | b'.' => {
                let start = self.at;
                let reading = self.reading();
                let mut value = match self.bytes[start] {
                    b'\'' | b'"' | b'`' => self.string()?,
                    _ => self.number()?,
                };
                self.levels += 1;
                self.chained(&mut value, start, reading, false)?;
                // What the chain left can be the left operand of a sum.
                if matches!(self.peek(), b'+' | b'-' | b'*' | b'/') {
                    self.summed(0, &mut value)?;
                }
                self.levels -= 1;
                Some(value)
            }
            first if STARTS_AN_IDENTIFIER[usize::from(first)] => {
                // The rule each fast path keeps: a fast path gives up
                // where the general path gives up. A word before a `(`
                // is a call: no arrow and no sum can stand between the
                // two.
                let word = self.word()?;
                let mut after = self.at + word.len();
                while matches!(
                    self.bytes.get(after),
                    Some(b' ' | b'\t' | 0x0C | b'\n' | b'\r')
                ) {
                    after += 1;
                }
                if self.bytes.get(after) != Some(&b'(') || is_reserved(word) {
                    return self.sum_or_arrow();
                }
                self.levels += 1;
                let start = self.at;
                let reading = self.reading();
                // The `(` of the call, right after the name, joins the
                // token: a `(` never ends a statement.
                let paren = after == self.at + word.len();
                self.take(word.len() + usize::from(paren))?;
                let mut value = Parsed::chain(ScoreExpr::Name(word.into()), 1);
                self.chained(&mut value, start, reading, paren)?;
                // What the chain left can be the left operand of a sum.
                if matches!(self.peek(), b'+' | b'-' | b'*' | b'/') {
                    self.summed(0, &mut value)?;
                }
                self.levels -= 1;
                Some(value)
            }
            _ => self.sum_or_arrow(),
        }
    }

    /// The general form of an expression behind [`Self::expression`]:
    /// an arrow, or a sum.
    fn sum_or_arrow(&mut self) -> Option<Parsed<'s>> {
        self.levels += 1;
        // Most expressions are no arrow, and need none of its levels.
        let parsed = if self.peek() != b'(' && !self.names_an_arrow() {
            self.sum(0)
        } else {
            match self.arrow() {
                Some(Some(arrow)) => Some(arrow),
                Some(None) => self.sum(0),
                None => None,
            }
        };
        self.levels -= 1;
        parsed
    }

    /// `name => body` or `(names) => body`, or `Some(None)` for another
    /// form, with the reader where it was. The reader is at a `(` or at
    /// a word that names an arrow: [`Self::sum_or_arrow`] says so.
    fn arrow(&mut self) -> Option<Option<Parsed<'s>>> {
        let mark = self.mark();
        let parameters = match self.peek() {
            b'(' => self.parameters(),
            _ => self.bound_name().map(|name| vec![name]),
        };
        // JavaScript takes no line end before the `=>`.
        let arrow =
            self.bytes[self.at..].starts_with(b"=>") && !self.gap.line && !self.gap.comment_line;
        let Some(parameters) = parameters.filter(|_| arrow) else {
            self.reset(mark);
            return Some(None);
        };
        self.take(2)?;
        let (bindings, result, height) = match self.peek() {
            b'{' => self.block(&parameters)?,
            _ => {
                let result = self.expression()?;
                (Vec::new(), result.expr, result.height)
            }
        };
        Some(Some(Parsed {
            expr: ScoreExpr::Arrow(Box::new(ScoreFunction {
                parameters: borrowed(&parameters),
                bindings,
                result,
            })),
            height: height + 1,
            form: Form::Other,
        }))
    }

    /// Whether the reader is at a word with `=>` after it and only spaces
    /// between them: the one parameter of an arrow.
    fn names_an_arrow(&self) -> bool {
        self.word().is_some_and(|word| {
            self.bytes[self.at + word.len()..]
                .trim_ascii_start()
                .starts_with(b"=>")
        })
    }

    /// `(first, second)`: names, each one time.
    fn parameters(&mut self) -> Option<Vec<&'s str>> {
        self.open(b'(')?;
        let mut parameters = Vec::new();
        while self.peek() != b')' {
            let name = self.bound_name()?;
            if parameters.contains(&name) {
                return None;
            }
            parameters.push(name);
            if self.peek() != b',' {
                break;
            }
            self.take(1)?;
        }
        self.close(b')')?;
        Some(parameters)
    }

    /// `{ const name = value }` and one `return`: the bindings, the result
    /// and the height of the deepest of them. Each name of the block and
    /// of `parameters` is declared one time.
    fn block(&mut self, parameters: &[&'s str]) -> Option<Body<'s>> {
        self.open(b'{')?;
        let mut names = parameters.to_vec();
        let mut bindings = Vec::new();
        let mut height = 0;
        loop {
            match self.word()? {
                "const" | "let" => {
                    let ((name, value), deep) = self.binding()?;
                    if names.contains(&name) {
                        return None;
                    }
                    names.push(name);
                    bindings.push((name.into(), value));
                    height = height.max(deep);
                    self.end_statement(true)?;
                }
                "return" => {
                    self.take(6)?;
                    // JavaScript returns nothing at a line end.
                    if self.gap.line || self.gap.comment_line {
                        return None;
                    }
                    let result = self.expression()?;
                    if self.peek() == b';' {
                        self.take(1)?;
                    }
                    self.close(b'}')?;
                    return Some((bindings, result.expr, height.max(result.height)));
                }
                _ => return None,
            }
        }
    }

    /// Operands joined by `+ - * /`, each at `floor` or tighter. Each
    /// operand of an operator is a number, and the result is their value.
    fn sum(&mut self, floor: u8) -> Option<Parsed<'s>> {
        // A sign before an operand needs its own reading, and most operands
        // have none, so they go straight to their value.
        let mut left = match self.peek() {
            b'-' if !matches!(self.bytes.get(self.at + 1), Some(b'-' | b'=')) => self.signed()?,
            _ => self.operand()?,
        };
        self.summed(floor, &mut left)?;
        Some(left)
    }

    /// The operators of [`Self::sum`] after its left operand, which they
    /// fold in the place it was built in.
    fn summed(&mut self, floor: u8, left: &mut Parsed<'s>) -> Option<()> {
        loop {
            let operator = self.peek();
            let precedence = match operator {
                b'+' | b'-' => 1,
                b'*' | b'/' => 2,
                _ => return Some(()),
            };
            // `++`, `--`, `**` and an assignment are other operators.
            let next = self.bytes.get(self.at + 1);
            if next == Some(&operator) || next == Some(&b'=') {
                return None;
            }
            if precedence < floor {
                return Some(());
            }
            self.take(1)?;
            let right = self.sum(precedence + 1)?;
            let (ScoreExpr::Number(a), ScoreExpr::Number(b)) = (&left.expr, &right.expr) else {
                return None;
            };
            let value = match operator {
                b'+' => a + b,
                b'-' => a - b,
                b'*' => a * b,
                _ => a / b,
            };
            left.expr = ScoreExpr::Number(value);
            left.height = left.height.max(right.height) + 1;
            left.form = Form::Other;
        }
    }

    /// An operand, or `-` and a number.
    fn signed(&mut self) -> Option<Parsed<'s>> {
        let negates =
            self.peek() == b'-' && !matches!(self.bytes.get(self.at + 1), Some(b'-' | b'='));
        if !negates {
            return self.operand();
        }
        if self.levels >= MAX_LEVELS {
            return None;
        }
        self.take(1)?;
        self.levels += 1;
        let negated = self.signed();
        self.levels -= 1;
        let negated = negated?;
        let ScoreExpr::Number(value) = negated.expr else {
            return None;
        };
        Some(Parsed {
            expr: ScoreExpr::Number(-value),
            height: negated.height + 1,
            form: Form::Other,
        })
    }

    /// A value and the members and calls written after it.
    fn operand(&mut self) -> Option<Parsed<'s>> {
        let start = self.at;
        let reading = self.reading();
        let mut value = self.value()?;
        self.chained(&mut value, start, reading, false)?;
        Some(value)
    }

    /// The members and calls written after `value`, of a text that starts
    /// at `start` where the reader held `reading`. The place `value` holds
    /// is its own: the reader builds each link of the chain in it, with no
    /// copy of the link before. `paren` says the `(` of the first call is
    /// taken with the token before it.
    fn chained(
        &mut self,
        value: &mut Parsed<'s>,
        start: usize,
        reading: (bool, usize),
        mut paren: bool,
    ) -> Option<()> {
        loop {
            if paren {
                self.call(value, start, reading, true)?;
                paren = false;
                continue;
            }
            match self.peek() {
                // A dot before a digit starts a number.
                b'.' if !self.bytes.get(self.at + 1).is_some_and(u8::is_ascii_digit) => {
                    let (property, after_its_dot) = self.dotted()?;
                    if self.peek() == b'(' {
                        let (arguments, height) = match (self.form_of)(property) {
                            CallForm::Written | CallForm::Page => self.arguments(false)?,
                            CallForm::Widget { slot } => {
                                // The JavaScript route finds a widget call
                                // as a dot, the name, spaces and a `(`.
                                let spaced = self.bytes[self.last..self.at]
                                    .iter()
                                    .all(u8::is_ascii_whitespace);
                                if !(after_its_dot && spaced) || self.pages > 0 {
                                    return None;
                                }
                                (self.widget(slot)?, 1)
                            }
                            CallForm::Other => return None,
                        };
                        // The value read before leaves its place for the
                        // list of the program.
                        let receiver = self
                            .held
                            .hold(std::mem::replace(&mut value.expr, ScoreExpr::Number(0.0)))?;
                        *value = Parsed::chain(
                            ScoreExpr::Method {
                                receiver,
                                method: property.into(),
                                arguments,
                                written: self.written(start, reading),
                            },
                            value.height.max(height) + 1,
                        );
                    } else {
                        let object = self
                            .held
                            .hold(std::mem::replace(&mut value.expr, ScoreExpr::Number(0.0)))?;
                        *value = Parsed::chain(
                            ScoreExpr::Member {
                                object,
                                property: property.into(),
                            },
                            value.height + 1,
                        );
                    }
                }
                b'(' => self.call(value, start, reading, false)?,
                _ => return Some(()),
            }
        }
    }

    /// The call of `value`: the `(` is before the reader, or an earlier
    /// token took it when `paren` says so.
    fn call(
        &mut self,
        value: &mut Parsed<'s>,
        start: usize,
        reading: (bool, usize),
        paren: bool,
    ) -> Option<()> {
        let inner = std::mem::replace(&mut value.expr, ScoreExpr::Number(0.0));
        let form = match &inner {
            ScoreExpr::Name(callee) => (self.form_of)(callee),
            _ => CallForm::Written,
        };
        // Only a call for the page opens one: the counter is no work of
        // the calls a score is made of.
        let arguments = if form == CallForm::Page {
            self.pages += 1;
            let arguments = self.arguments(paren);
            self.pages -= 1;
            arguments
        } else {
            self.arguments(paren)
        };
        let (arguments, height) = arguments?;
        let written = self.written(start, reading);
        match inner {
            ScoreExpr::Name(callee) if form != CallForm::Other => {
                value.expr = ScoreExpr::Call {
                    callee,
                    arguments,
                    written,
                };
                value.height = height + 1;
            }
            // A member in parentheses, called.
            ScoreExpr::Member { object, property } => {
                value.expr = ScoreExpr::Method {
                    receiver: object,
                    method: property,
                    arguments,
                    written,
                };
                value.height = (value.height - 1).max(height) + 1;
            }
            _ => return None,
        }
        value.form = Form::Chain;
        Some(())
    }

    /// The property of the member that the reader is at, past its dot,
    /// and whether it stands right after the dot. A gap of spaces
    /// between the dot and the name lets the two take as one token. The
    /// `(` of the call takes as its own token: the reader gives a call
    /// it does not read up at the `(`, and says the line of that `(`.
    fn dotted(&mut self) -> Option<(&'s str, bool)> {
        let dot = self.at;
        let mut name = dot + 1;
        let mut line = false;
        while let Some(byte) = self.bytes.get(name) {
            match byte {
                b' ' | b'\t' | 0x0C => name += 1,
                b'\n' | b'\r' => {
                    line = true;
                    name += 1;
                }
                _ => break,
            }
        }
        // A comment between the dot and the name, and a line end where
        // the byte before the dot can end a statement, read as before:
        // the name takes as its own token there.
        let first = self.bytes.get(name).copied();
        let one_take = first.is_some_and(|byte| STARTS_AN_IDENTIFIER[usize::from(byte)])
            && (!line || !matches!(self.bytes[dot - 1], b'.' | b'0'..=b'9'));
        if one_take {
            let bytes = &self.bytes[name..];
            let mut length = 1;
            while length < bytes.len() && CONTINUES_AN_IDENTIFIER[usize::from(bytes[length])] {
                length += 1;
            }
            // A name that reads on at a character outside ASCII or at
            // an escape goes to the slow path, which takes the dot and
            // its gap first and stops at the name: the line the reader
            // leaves is the line of the name, as before.
            if !bytes
                .get(length)
                .is_some_and(|byte| !byte.is_ascii() || *byte == b'\\')
            {
                self.take(name + length - dot)?;
                return Some((&self.source[name..name + length], name == dot + 1));
            }
        }
        self.take(1)?;
        let property = self.word()?;
        let after_its_dot = self.last == self.at;
        self.take(property.len())?;
        Some((property, after_its_dot))
    }

    /// What the reader holds where a text starts, for [`Self::written`]:
    /// whether a string is mini-notation, and how many widgets with a slot
    /// the reader is past.
    fn reading(&self) -> (bool, usize) {
        (self.mini, self.widgets)
    }

    /// Where the text is that starts at `start`, where the reader held
    /// `reading`, and ends with the last token.
    fn written(&self, start: usize, (mini, widgets): (bool, usize)) -> Written {
        Written {
            start: start as u32,
            len: (self.last - start) as u32,
            slots: match self.widgets == widgets {
                true => u32::MAX,
                false => widgets as u32,
            },
            mini,
        }
    }

    /// The arguments of a widget call with none of its own: an id and,
    /// with `slot`, the number of the widget among those with a slot. The
    /// JavaScript route gives no number past the first 256.
    fn widget(&mut self, slot: bool) -> Option<Row> {
        let (written, _) = self.arguments(false)?;
        if !written.is_empty() {
            return None;
        }
        let id = ScoreExpr::String("".into());
        let number = match slot {
            true => {
                self.widgets += 1;
                u8::try_from(self.widgets - 1).ok()
            }
            false => None,
        };
        match number {
            Some(number) => self
                .held
                .hold_row([id, ScoreExpr::Number(f64::from(number))]),
            None => self.held.hold_row([id]),
        }
    }

    /// `(first, second)`: the arguments of a call and the height of the
    /// deepest. The reader is at the `(`, which an earlier token can have
    /// taken already.
    #[inline]
    fn arguments(&mut self, paren: bool) -> Option<(Row, usize)> {
        if !paren {
            self.take(1)?;
        }
        self.depth += 1;
        let listed = self.list(b')')?;
        self.close(b')')?;
        Some(listed)
    }

    /// Expressions with a comma between them, up to `close`: their row
    /// and the height of the deepest.
    fn list(&mut self, close: u8) -> Option<(Row, usize)> {
        if self.peek() == close {
            return Some((self.held.hold_row([])?, 0));
        }
        // Lists of one and two expressions, which most calls hold, join
        // `held` with no pass through `listed`.
        let first = self.expression()?;
        if self.peek() != b',' {
            return Some((self.held.hold_one(first.expr)?, first.height));
        }
        self.take(1)?;
        // A trailing comma ends the list.
        if self.peek() == close {
            return Some((self.held.hold_one(first.expr)?, first.height));
        }
        let second = self.expression()?;
        let mut height = first.height.max(second.height);
        if self.peek() != b',' {
            return Some((self.held.hold_row([first.expr, second.expr])?, height));
        }
        let from = self.listed.len();
        self.listed.push(first.expr);
        self.listed.push(second.expr);
        loop {
            self.take(1)?;
            // A trailing comma ends the list.
            if self.peek() == close {
                break;
            }
            let expression = self.expression()?;
            height = height.max(expression.height);
            self.listed.push(expression.expr);
            if self.peek() != b',' {
                break;
            }
        }
        let row = self.held.hold_row(self.listed.drain(from..))?;
        Some((row, height))
    }

    /// A name, a number, a string, an array, an object, or an expression
    /// in parentheses.
    fn value(&mut self) -> Option<Parsed<'s>> {
        match self.peek() {
            b'(' => {
                self.open(b'(')?;
                let inner = self.expression()?;
                self.close(b')')?;
                Some(Parsed::chain(inner.expr, inner.height))
            }
            b'[' => {
                self.open(b'[')?;
                let (elements, height) = self.list(b']')?;
                self.close(b']')?;
                Some(Parsed::chain(ScoreExpr::Array(elements), height + 1))
            }
            b'{' => self.object(),
            b'\'' | b'"' | b'`' => self.string(),
            b'0'..=b'9' | b'.' => self.number(),
            _ => {
                let name = self.word().filter(|name| !is_reserved(name))?;
                self.take(name.len())?;
                Some(Parsed::chain(ScoreExpr::Name(name.into()), 1))
            }
        }
    }

    /// `{ key: value, name, ...source }`.
    fn object(&mut self) -> Option<Parsed<'s>> {
        self.open(b'{')?;
        let mut properties = Vec::new();
        let mut height = 0;
        while self.peek() != b'}' {
            if self.bytes[self.at..].starts_with(b"...") {
                self.take(3)?;
                let source = self.expression()?;
                height = height.max(source.height);
                properties.push(ScoreProperty::Spread(source.expr));
            } else {
                let (key, name): (Cow<'s, str>, Option<&'s str>) = match self.peek() {
                    // A double-quoted key is mini-notation, and no key.
                    b'\'' => match self.string()?.expr {
                        ScoreExpr::String(key) => (key, None),
                        _ => return None,
                    },
                    _ => {
                        let name = self.word()?;
                        self.take(name.len())?;
                        (name.into(), Some(name))
                    }
                };
                // `__proto__` sets the prototype and no property.
                if &*key == "__proto__" {
                    return None;
                }
                let value = if self.peek() == b':' {
                    self.take(1)?;
                    self.expression()?
                } else {
                    // `name` alone holds the value of that name.
                    let name = name.filter(|name| !is_reserved(name))?;
                    Parsed::chain(ScoreExpr::Name(name.into()), 1)
                };
                height = height.max(value.height);
                properties.push(ScoreProperty::Entry {
                    key,
                    value: value.expr,
                });
            }
            if self.peek() != b',' {
                break;
            }
            self.take(1)?;
        }
        self.close(b'}')?;
        Some(Parsed::chain(ScoreExpr::Object(properties), height + 1))
    }

    /// A string literal: mini-notation in double quotes and in a template,
    /// a string in single quotes.
    #[inline]
    fn string(&mut self) -> Option<Parsed<'s>> {
        let open = self.at;
        let quote = self.bytes[open];
        // A template reads on at a line feed and ends at a `${`. Any other
        // string refuses a line end.
        let third = if quote == b'`' { b'$' } else { b'\n' };
        // Whether the body needs the escape reader: it has a backslash, or
        // a line end, which a template normalises and other strings
        // refuse. Any other body is its own value, with no second scan.
        let mut coded = false;
        let mut close = open + 1;
        loop {
            let (at, byte) = first_of(&self.bytes[close..], quote, b'\\', b'\r', third)?;
            close += at;
            match byte {
                b'\\' => {
                    coded = true;
                    // The byte after a backslash is no end and no line
                    // end, and a backslash at the end of the text ends
                    // the scan there: the string has no close.
                    close = (close + 2).min(self.bytes.len());
                }
                byte if byte == quote => break,
                // A template with a substitution is code.
                b'$' if self.bytes.get(close + 1) == Some(&b'{') => return None,
                b'\r' | b'\n' => {
                    coded = true;
                    close += 1;
                }
                // A `$` of a template with no `{` after it.
                _ => close += 1,
            }
        }
        let raw = self.source.get(open + 1..close)?;
        let mini = match quote {
            b'\'' => false,
            b'"' => self.mini,
            // A template after a name is a call of that name.
            _ if self.mini && !self.follows_a_name(open) => true,
            _ => return None,
        };
        let value = if coded {
            decode(raw, quote == b'`').ok()?
        } else {
            Cow::Borrowed(raw)
        };
        self.take(close + 1 - open)?;
        Some(if mini {
            Parsed::chain(
                ScoreExpr::Call {
                    callee: MINI_CALLEE.into(),
                    // The place of the literal is far below the first
                    // integer that a float cannot hold.
                    arguments: self
                        .held
                        .hold_row([ScoreExpr::String(value), ScoreExpr::Number(open as f64)])?,
                    // The call holds its place, so a host has no use for
                    // the text of the literal.
                    written: Written::default(),
                },
                2,
            )
        } else {
            Parsed {
                expr: ScoreExpr::String(value),
                height: 1,
                form: Form::String,
            }
        })
    }

    /// Whether the last character before `at` and the spaces before it is
    /// one of a name, as a tag of a template is.
    fn follows_a_name(&self, at: usize) -> bool {
        self.bytes[..at]
            .trim_ascii_end()
            .last()
            .is_some_and(|byte| is_identifier_byte(*byte) || !byte.is_ascii())
    }

    /// A decimal number: `12`, `0.5`, `.5`, `5.`, `1e3`.
    #[inline]
    fn number(&mut self) -> Option<Parsed<'s>> {
        let bytes = self.bytes;
        let start = self.at;
        let (whole, value) = fold_digits(bytes, start, 0);
        // A zero before another digit starts an octal number.
        if whole > 1 && bytes[start] == b'0' {
            return None;
        }
        let mut end = start + whole;
        let mut fraction = 0;
        let mut value = value;
        if bytes.get(end) == Some(&b'.') {
            let (count, folded) = fold_digits(bytes, end + 1, value);
            fraction = count;
            value = folded;
            end += 1 + fraction;
        }
        if whole + fraction == 0 {
            return None;
        }
        let mut plain = true;
        if matches!(bytes.get(end), Some(b'e' | b'E')) {
            let sign = usize::from(matches!(bytes.get(end + 1), Some(b'+' | b'-')));
            let (exponent, _) = fold_digits(bytes, end + 1 + sign, 0);
            if exponent == 0 {
                return None;
            }
            end += 1 + sign + exponent;
            plain = false;
        }
        // `1n`, `0x1f`, `1_000` and `3px` are other numbers, or none.
        if bytes
            .get(end)
            .is_some_and(|byte| is_identifier_byte(*byte) || !byte.is_ascii())
        {
            return None;
        }
        // A run of at most fifteen digits is exact as a `u64` and one
        // division, the value the parser of the text gives, with the
        // fraction folded into the same scan that counts it.
        let value = if plain && whole + fraction <= 15 {
            value as f64 / POW10[fraction]
        } else {
            self.source[start..end].parse().ok()?
        };
        self.take(end - start)?;
        Some(Parsed {
            expr: ScoreExpr::Number(value),
            height: 1,
            form: Form::Other,
        })
    }

    /// The name that the reader is at, which it then takes: a name a score
    /// can bind.
    fn bound_name(&mut self) -> Option<&'s str> {
        let name = self
            .word()
            .filter(|name| !is_reserved(name) && !matches!(*name, "eval" | "arguments"))?;
        self.take(name.len())?;
        Some(name)
    }

    /// The word that the reader is at: a name or a keyword in ASCII.
    fn word(&self) -> Option<&'s str> {
        let first = self.peek();
        if !STARTS_AN_IDENTIFIER[usize::from(first)] {
            return None;
        }
        let bytes = &self.bytes[self.at..];
        let mut length = 1;
        for (more, &byte) in bytes.iter().enumerate().skip(1) {
            if !CONTINUES_AN_IDENTIFIER[usize::from(byte)] {
                break;
            }
            length = more + 1;
        }
        // A name reads on at a character outside ASCII and at an escape.
        if bytes
            .get(length)
            .is_some_and(|byte| !byte.is_ascii() || *byte == b'\\')
        {
            return None;
        }
        Some(&self.source[self.at..self.at + length])
    }

    /// The byte at `at`, or zero past the end of the text.
    #[inline]
    fn peek(&self) -> u8 {
        self.bytes.get(self.at).copied().unwrap_or(0)
    }

    /// Take the opening `bracket`.
    #[inline]
    fn open(&mut self, bracket: u8) -> Option<()> {
        if self.peek() != bracket {
            return None;
        }
        self.take(1)?;
        self.depth += 1;
        Some(())
    }

    /// Take the closing `bracket`.
    #[inline]
    fn close(&mut self, bracket: u8) -> Option<()> {
        if self.peek() != bracket {
            return None;
        }
        self.take(1)?;
        self.depth -= 1;
        Some(())
    }

    /// Take the next `width` bytes as a token of the statement in progress,
    /// and go to the token after it.
    #[inline]
    fn take(&mut self, width: usize) -> Option<()> {
        // The scan ends a statement at a line end outside every bracket,
        // unless the line before ends in the middle of an expression or
        // the next line starts with a member, a call or an index. The line
        // test comes first: most tokens have no line end before them.
        if self.gap.line && !self.gap.fresh && self.depth == 0 && !self.scan_reads_on() {
            return None;
        }
        self.at += width;
        self.last = self.at;
        self.skip()
    }

    /// Whether the scan of the JavaScript route reads the next token as
    /// part of the statement of the last one, with a line end between them.
    fn scan_reads_on(&self) -> bool {
        let before = |back: usize| self.last.checked_sub(back).map(|at| self.bytes[at]);
        let last_reads_on = match before(1) {
            Some(b'.') => !before(2).is_some_and(|byte| byte.is_ascii_digit() || byte == b'.'),
            Some(sign @ (b'+' | b'-')) => before(2) != Some(sign),
            Some(
                b',' | b'*' | b'/' | b'%' | b'&' | b'|' | b'^' | b'?' | b':' | b'=' | b'<' | b'>',
            ) => true,
            _ => false,
        };
        last_reads_on
            || match self.peek() {
                b'.' => self
                    .bytes
                    .get(self.at + 1)
                    .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$')),
                b'(' | b'[' => true,
                _ => false,
            }
    }

    /// Go to the next token: past spaces, line ends and comments.
    #[inline]
    fn skip(&mut self) -> Option<()> {
        // Most tokens have the next right after them, or past one space.
        match self.peek() {
            b' ' => match self.bytes.get(self.at + 1) {
                Some(b' ' | b'\t' | 0x0C | b'\n' | b'\r' | b'/') => self.skip_gap(),
                _ => {
                    self.at += 1;
                    self.gap = Gap::default();
                    Some(())
                }
            },
            b'\t' | 0x0C | b'\n' | b'\r' | b'/' => self.skip_gap(),
            _ => {
                self.gap = Gap::default();
                Some(())
            }
        }
    }

    /// [`Self::skip`] where a space, a line end or a `/` is next.
    fn skip_gap(&mut self) -> Option<()> {
        let bytes = self.bytes;
        let mut gap = Gap::default();
        let mut at = self.at;
        while let Some(byte) = bytes.get(at) {
            match *byte {
                // A run of spaces is most gaps, and ends in a tight loop.
                b' ' => {
                    at += 1;
                    while bytes.get(at) == Some(&b' ') {
                        at += 1;
                    }
                }
                b'\t' | 0x0C => at += 1,
                b'\n' | b'\r' => {
                    // A line end clears a comment before it.
                    gap.line = true;
                    gap.comment = false;
                    at += 1;
                }
                b'/' => match bytes.get(at + 1) {
                    Some(b'/') => {
                        let text = &self.source[at + 2..];
                        let end = text.find(['\n', '\r']).unwrap_or(text.len());
                        self.directive(&text[..end]);
                        at += 2 + end;
                    }
                    Some(b'*') => {
                        let text = &self.source[at + 2..];
                        let Some(end) = text.find("*/") else {
                            // The reader stops where the text does, as it
                            // did through the gap it read so far.
                            self.at = at;
                            return None;
                        };
                        self.directive(&text[..end]);
                        gap.comment = true;
                        if text[..end].contains(['\n', '\r']) {
                            gap.comment_line = true;
                        }
                        at += 2 + end + 2;
                    }
                    _ => break,
                },
                _ => break,
            }
        }
        self.at = at;
        self.gap = gap;
        Some(())
    }

    /// Read a comment that turns mini-notation off or on.
    fn directive(&mut self, comment: &str) {
        let comment = comment.trim_start();
        if comment.starts_with("mini-off") {
            self.mini = false;
        } else if comment.starts_with("mini-on") {
            self.mini = true;
        }
    }

    fn mark(&self) -> Mark {
        Mark {
            at: self.at,
            last: self.last,
            gap: self.gap,
            depth: self.depth,
            mini: self.mini,
        }
    }

    fn reset(&mut self, mark: Mark) {
        self.at = mark.at;
        self.last = mark.last;
        self.gap = mark.gap;
        self.depth = mark.depth;
        self.mini = mark.mini;
    }

    /// The line of the byte at `at`, from 1. Each call asks for a byte at
    /// or after the byte of the call before.
    fn line_of(&mut self, at: usize) -> usize {
        self.line += self.bytes[self.counted..at]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count();
        self.counted = at;
        self.line
    }
}

/// `names`, each as the text of the score holds it.
fn borrowed<'s>(names: &[&'s str]) -> Vec<Cow<'s, str>> {
    names.iter().map(|name| Cow::Borrowed(*name)).collect()
}

/// The digits from `from` in `bytes`: how many, and `value` folded left
/// through them. The fold stops where a `u64` would overflow: a number
/// with that many digits takes the parser of the text, which the count
/// alone decides, so the count reads on.
fn fold_digits(bytes: &[u8], from: usize, value: u64) -> (usize, u64) {
    let mut count = 0;
    let mut folded = value;
    while let Some(byte) = bytes.get(from + count).filter(|byte| byte.is_ascii_digit()) {
        let digit = u64::from(byte - b'0');
        if let Some(next) = folded
            .checked_mul(10)
            .and_then(|folded| folded.checked_add(digit))
        {
            folded = next;
        }
        count += 1;
    }
    (count, folded)
}

/// The bytes of a word of eight that equal `needle`, one high bit for
/// each. Only the lowest set bit is exact: a higher one can be set
/// where no byte equals `needle`. One test reads the whole word at
/// once. The one scan of eight bytes that the reader and the lane
/// share.
#[inline]
pub(crate) fn hits(word: u64, needle: u8) -> u64 {
    let spread = word ^ (u64::from(needle) * 0x0101_0101_0101_0101);
    spread.wrapping_sub(0x0101_0101_0101_0101) & !spread & 0x8080_8080_8080_8080
}

/// The first place in `bytes` of one of four bytes, and the byte there.
/// A scan in words of eight bytes reads them all at once, which a string
/// body of any length needs less than a test for each byte. `None` where
/// `bytes` holds none of the four.
fn first_of(bytes: &[u8], first: u8, second: u8, third: u8, fourth: u8) -> Option<(usize, u8)> {
    let (chunks, rest) = bytes.as_chunks::<8>();
    let mut at = 0;
    for chunk in chunks {
        let word = u64::from_le_bytes(*chunk);
        let found = hits(word, first) | hits(word, second) | hits(word, third) | hits(word, fourth);
        if found != 0 {
            let at = at + usize::try_from(found.trailing_zeros()).expect("a byte place") / 8;
            return Some((at, bytes[at]));
        }
        at += 8;
    }
    rest.iter()
        .position(|byte| *byte == first || *byte == second || *byte == third || *byte == fourth)
        .map(|place| (at + place, bytes[at + place]))
}

/// Whether each byte can continue an ASCII JavaScript identifier.
const CONTINUES_AN_IDENTIFIER: [bool; 256] = {
    let mut continues = [false; 256];
    let mut byte = 0u8;
    loop {
        continues[byte as usize] = byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$');
        if byte == u8::MAX {
            break;
        }
        byte += 1;
    }
    continues
};

/// Whether each byte can start an ASCII JavaScript identifier.
const STARTS_AN_IDENTIFIER: [bool; 256] = {
    let mut starts = [false; 256];
    let mut byte = 0u8;
    loop {
        starts[byte as usize] = byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$');
        if byte == u8::MAX {
            break;
        }
        byte += 1;
    }
    starts
};

/// Ten to each power a plain number of the reader can hold, exact as a
/// `f64` and as text.
const POW10: [f64; 16] = [
    1.0,
    10.0,
    100.0,
    1_000.0,
    10_000.0,
    100_000.0,
    1_000_000.0,
    10_000_000.0,
    100_000_000.0,
    1_000_000_000.0,
    1e10,
    1e11,
    1e12,
    1e13,
    1e14,
    1e15,
];

/// Whether `byte` can continue an ASCII JavaScript identifier.
fn is_identifier_byte(byte: u8) -> bool {
    CONTINUES_AN_IDENTIFIER[usize::from(byte)]
}

/// One list of the words JavaScript keeps for itself, in a module: the
/// table of lengths and the match of `is_reserved` both derive from the
/// same words, written one time here.
macro_rules! reserved_words {
    ($($word:literal),* $(,)?) => {
        /// The words, as one list.
        const RESERVED_WORDS: &[&str] = &[$($word),*];

        /// For each first byte of a word, the lengths of the reserved
        /// words that start with it, one bit for each length: most words
        /// of a score are no keyword, and the bits say so with no
        /// comparison of text.
        const RESERVED_LENGTHS: [u64; 256] = {
            let mut lengths = [0u64; 256];
            let mut at = 0;
            while at < RESERVED_WORDS.len() {
                let word = RESERVED_WORDS[at];
                if let Some(first) = word.as_bytes().first() {
                    lengths[*first as usize] |= 1u64 << word.len();
                }
                at += 1;
            }
            lengths
        };

        /// Whether JavaScript keeps `word` for itself, in a module.
        /// `async` is a name, and the reader leaves each text that uses
        /// it to JavaScript.
        fn is_reserved(word: &str) -> bool {
            word.as_bytes().first().is_some_and(|first| {
                // The min keeps the shift inside a `u64`: no word is that long.
                RESERVED_LENGTHS[usize::from(*first)] & (1u64 << word.len().min(63)) != 0
            }) && matches!(word, $($word)|*)
        }
    };
}

reserved_words!(
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "interface",
    "let",
    "new",
    "null",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
);

#[cfg(test)]
mod tests {
    use super::{CallForm, left_at, read};
    use crate::{LaneStatement, ScoreExpr, ScoreStatement};

    /// Each call as written.
    fn written(_: &str) -> CallForm {
        CallForm::Written
    }

    /// The arguments of the lane call that statement `index` of `read` is,
    /// as they print.
    fn lane_call(read: &super::Read<'_>, index: usize) -> String {
        match &read.program.statements[index] {
            ScoreStatement::Expression(ScoreExpr::Method {
                method, arguments, ..
            }) if &**method == "p" => format!("{:?}", read.program.held.row(*arguments)),
            other => panic!("{other:?} is no lane call"),
        }
    }

    #[test]
    fn a_labeled_statement_reads_as_a_lane_call_and_a_lane() {
        let read =
            read("// drums\n$: s(\"bd\")\r\n  .fast(2) // twice\n", written).expect("a score");
        assert_eq!(
            read.lanes,
            [LaneStatement {
                label: "$".into(),
                body: "s(\"bd\")\n  .fast(2)".into(),
                line: 2,
            }]
        );
        assert_eq!(lane_call(&read, 0), r#"[String("$"), Number(0.0)]"#);
    }

    #[test]
    fn a_text_with_no_label_is_one_lane_of_its_last_statement() {
        let read = read("setcpm(30)\n\nnote(\"c e\") ;\n", written).expect("a score");
        assert_eq!(
            read.lanes,
            [LaneStatement {
                label: String::new(),
                body: "note(\"c e\")".into(),
                line: 3,
            }]
        );
    }

    #[test]
    fn a_host_writes_its_lane_numbers_over_the_places() {
        let mut read = read("a: s(\"bd\")\nb: s(\"hh\")", written).expect("a score");
        // A list with another count writes nothing.
        assert!(!read.number_lanes(&[7]));
        assert_eq!(lane_call(&read, 1), r#"[String("b"), Number(1.0)]"#);
        assert!(read.number_lanes(&[7, 9]));
        assert_eq!(lane_call(&read, 0), r#"[String("a"), Number(7.0)]"#);
        assert_eq!(lane_call(&read, 1), r#"[String("b"), Number(9.0)]"#);
    }

    #[test]
    fn a_call_that_the_javascript_route_rewrites_is_left_to_it() {
        let other = |name: &str| match name {
            "slider" => CallForm::Other,
            _ => CallForm::Written,
        };
        assert!(read("slider(1)", written).is_some());
        assert!(read("slider(1)", other).is_none());
        assert!(read("function slider(a) { return a }", other).is_none());
    }

    #[test]
    fn a_widget_call_holds_an_empty_id_and_its_slot() {
        fn form(name: &str) -> CallForm {
            match name {
                "scope" => CallForm::Widget { slot: true },
                "markcss" => CallForm::Widget { slot: false },
                "all" => CallForm::Page,
                _ => CallForm::Written,
            }
        }
        fn arguments(text: &str) -> String {
            let read = read(text, form).expect("a score");
            let Some(ScoreStatement::Expression(ScoreExpr::Method { arguments, .. })) =
                read.program.statements.last()
            else {
                panic!("{text} is no method call");
            };
            format!("{:?}", read.program.held.row(*arguments))
        }
        assert_eq!(
            arguments("s(\"bd\").scope().markcss().scope( )"),
            r#"[String(""), Number(1.0)]"#
        );
        assert_eq!(arguments("s(\"bd\").markcss()"), r#"[String("")]"#);
        // The JavaScript route finds no widget here, or writes other
        // arguments.
        for text in [
            "s(\"bd\"). scope()",
            "s(\"bd\").scope /* c */ ()",
            "s(\"bd\").scope({ a: 1 })",
            "all(s(\"bd\").scope())",
        ] {
            assert!(read(text, form).is_none(), "{text}");
        }
    }

    #[test]
    fn the_line_of_a_form_of_javascript_alone_is_said() {
        for (text, line) in [
            ("$: s(\"bd\")", None),
            ("for (;;) {}", Some(1)),
            (
                "$: s(\"bd\")\n$: s(\"hh\")\n  .gain(x ? 1 : 2)\n$: s(\"cp\")",
                Some(3),
            ),
            ("const a = s(\"bd\")\n\nconst b = a[0]", Some(3)),
        ] {
            assert_eq!(left_at(text, written), line, "{text}");
        }
    }

    #[test]
    fn a_string_that_ends_with_a_backslash_is_refused() {
        assert!(read("s(\"bd\\", written).is_none());
    }

    #[test]
    fn a_label_with_no_code_at_the_end_of_the_text_is_no_statement() {
        let alone = read("$:", written).expect("a score");
        assert!(alone.program.statements.is_empty());
        assert_eq!(
            alone.lanes,
            [LaneStatement {
                label: String::new(),
                body: String::new(),
                line: 1,
            }]
        );
        let with_code = read("$: s(\"bd\")\n$:", written).expect("a score");
        assert_eq!(
            with_code.lanes,
            [LaneStatement {
                label: "$".into(),
                body: "s(\"bd\")".into(),
                line: 1,
            }]
        );
        assert_eq!(with_code.program.statements.len(), 1);
    }

    #[test]
    fn a_binding_records_the_slots_and_mini_of_its_own_text() {
        let scope = |name: &str| match name {
            "scope" => CallForm::Widget { slot: true },
            _ => CallForm::Written,
        };
        let with_slot = read("const a = s(\"hh\").scope()\n$: a", scope).expect("a score");
        let ScoreStatement::Binding { written, .. } = &with_slot.program.statements[0] else {
            panic!("no binding");
        };
        assert_eq!(written.slots, 0);
        let off = read("const a = /* mini-off */ s(\"hh\")\na", scope).expect("a score");
        let ScoreStatement::Binding { written, .. } = &off.program.statements[0] else {
            panic!("no binding");
        };
        assert!(written.mini);
    }

    #[test]
    fn a_number_of_many_digits_reads_as_the_text_parses() {
        let read = read("s(\"bd\").gain(3.14159265358979323846)", written).expect("a score");
        let ScoreStatement::Expression(ScoreExpr::Method { arguments, .. }) =
            &read.program.statements[0]
        else {
            panic!("no method call");
        };
        let ScoreExpr::Number(value) = &read.program.held.row(*arguments)[0] else {
            panic!("no number");
        };
        assert_eq!(*value, "3.14159265358979323846".parse::<f64>().unwrap());
    }

    #[test]
    fn the_line_of_a_sum_of_no_numbers_is_after_its_right_operand() {
        assert_eq!(left_at("s(\"bd\") +\n1", written), Some(2));
    }

    #[test]
    fn the_line_of_a_sum_of_no_numbers_is_after_its_right_operand_from_a_string_or_number() {
        // The fast paths for a string and for a number read the sum that
        // follows the value, as the one for a name does.
        assert_eq!(left_at("\"bd\" +\n1", written), Some(2));
        assert_eq!(left_at("1 +\nx", written), Some(2));
    }

    #[test]
    fn the_line_of_a_property_that_reads_on_is_of_the_name() {
        // The name reads on at the `é` and at the backslash, so the slow
        // path takes the dot and its gap first: the reader stops at the
        // name, on the line after the dot.
        assert_eq!(left_at("a.\nb\u{e9}", written), Some(2));
        assert_eq!(left_at("s(\"bd\").\nfastA\u{e9}()", written), Some(2));
    }

    #[test]
    fn the_line_of_a_call_the_reader_leaves_is_of_its_paren() {
        let form = |name: &str| match name {
            "K" => CallForm::Other,
            "scope" => CallForm::Widget { slot: true },
            "all" => CallForm::Page,
            _ => CallForm::Written,
        };
        // A method the route rewrites, and a widget in a page: the reader
        // gives both up at their `(`, before the gap after it.
        assert_eq!(left_at("s(\"saw\")\n  .K(\n    saw(1)\n  )", form), Some(2));
        assert_eq!(
            left_at("all(\n  s(\"bd\").scope(\n    1\n  )\n)", form),
            Some(2)
        );
    }

    #[test]
    fn a_form_of_javascript_alone_is_left_to_javascript() {
        for text in [
            "for (;;) {}",
            "a = 1",
            "x ? y : z",
            "s(`${x}`)",
            "new X()",
            "a[0]",
            "s(\"bd\"",
        ] {
            assert!(read(text, written).is_none(), "{text}");
        }
    }
}
