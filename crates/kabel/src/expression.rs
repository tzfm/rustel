//! Bounded arithmetic expression parser for the code strings of native
//! `raw` and `bytebeat` nodes.
//!
//! KabelSalat embeds those code strings as JavaScript in the compiled graph
//! function. This module parses the arithmetic subset of that language and
//! emits [`Instruction`]s for native sample processing, so raw expressions
//! need no script engine. Accepted syntax: number and Boolean literals, the identifiers
//! `t`, `$input` and `time`, the `Math` members behind [`Unary`] and
//! [`Binary`], the JavaScript operators from `||` down to `**`, the
//! conditional operator, parentheses and comments.
//!
//! Source text is untrusted score data, so parsing is bounded: at most
//! [`MAX_SOURCE_BYTES`] bytes of source, [`MAX_NESTING`] nested
//! subexpressions and [`MAX_INSTRUCTIONS`] instructions per expression. Two
//! rules follow JavaScript strict mode: legacy octal literals (`017`) are
//! rejected, and a unary operand of `**` needs parentheses.

use std::f64::consts::{E, PI};

/// Largest accepted expression source, in bytes.
const MAX_SOURCE_BYTES: usize = 65_536;

/// Largest number of instructions one expression may need.
const MAX_INSTRUCTIONS: usize = 256;

/// Largest number of nested subexpressions. The parser recurses once per
/// nesting level, so this also bounds its stack use.
const MAX_NESTING: usize = 64;

/// Precedence of the conditional operator, the loosest operator.
const CONDITIONAL_PREC: u8 = 1;

/// One unary operation. `Log` is the natural logarithm, as `Math.log`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Unary {
    Plus,
    Neg,
    Not,
    BitNot,
    Abs,
    Floor,
    Ceil,
    Round,
    Sign,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Log,
    Exp,
    Sqrt,
}

/// One binary operation. `Shr` is the signed `>>`, `UnsignedShr` the `>>>`
/// fill with zeroes. `And` and `Or` are the logical `&&` and `||`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Binary {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    UnsignedShr,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Equal,
    NotEqual,
    StrictEqual,
    StrictNotEqual,
    And,
    Or,
    Min,
    Max,
    Atan2,
}

/// One instruction of a parsed expression, in postfix evaluation order.
///
/// Operand fields index earlier instructions of the same list. The last
/// instruction is the root and computes the value of the whole expression.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Instruction {
    Constant(f64),
    Boolean(bool),
    /// The value of the node input, written `t` or `$input` in the source.
    Input,
    /// The elapsed time in seconds, written `time` in the source.
    Time,
    /// Applies the operation to the instruction the index points at.
    Unary(Unary, u16),
    /// Combines the instructions the two indices point at, in source order.
    Binary(Binary, u16, u16),
    /// Takes the consequent or the alternative, depending on the truth of
    /// the condition, mirroring `condition ? consequent : alternative`.
    Select(u16, u16, u16),
}

/// Parses one bounded arithmetic expression into postfix instructions.
///
/// Errors name the first defect and its byte offset in the source.
pub(super) fn parse(source: &str) -> Result<Vec<Instruction>, String> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "expression source exceeds {} bytes",
            MAX_SOURCE_BYTES
        ));
    }
    let mut parser = Parser::new(source);
    parser.expr(0, 0)?;
    if let Some(word) = parser.peek()? {
        return Err(format!(
            "unexpected trailing token '{}' at byte {}",
            word.text, word.start
        ));
    }
    debug_assert_eq!(parser.stack.len(), 1);
    Ok(parser.code)
}

/// Returns the operation, precedence and right-associativity of an infix
/// token. Precedence follows the JavaScript grammar, tightest last:
/// `?:` (handled separately), `||`, `&&`, `|`, `^`, `&`, equality,
/// relational, shifts, additive, multiplicative, `**`.
fn infix(token: Token<'_>) -> Option<(Binary, u8, bool)> {
    use Token::*;
    Some(match token {
        BarBar => (Binary::Or, 2, false),
        AmpAmp => (Binary::And, 3, false),
        Bar => (Binary::BitOr, 4, false),
        Caret => (Binary::BitXor, 5, false),
        Amp => (Binary::BitAnd, 6, false),
        EqEq => (Binary::Equal, 7, false),
        NotEq => (Binary::NotEqual, 7, false),
        EqEqEq => (Binary::StrictEqual, 7, false),
        NotEqEq => (Binary::StrictNotEqual, 7, false),
        Lt => (Binary::Less, 8, false),
        LtEq => (Binary::LessEqual, 8, false),
        Gt => (Binary::Greater, 8, false),
        GtEq => (Binary::GreaterEqual, 8, false),
        Shl => (Binary::Shl, 9, false),
        Shr => (Binary::Shr, 9, false),
        UShr => (Binary::UnsignedShr, 9, false),
        Plus => (Binary::Add, 10, false),
        Minus => (Binary::Sub, 10, false),
        Star => (Binary::Mul, 11, false),
        Slash => (Binary::Div, 11, false),
        Percent => (Binary::Mod, 11, false),
        StarStar => (Binary::Pow, 12, true),
        _ => return None,
    })
}

/// Maps a `Math` function name to its unary operation.
fn math_unary(name: &str) -> Option<Unary> {
    Some(match name {
        "abs" => Unary::Abs,
        "floor" => Unary::Floor,
        "ceil" => Unary::Ceil,
        "round" => Unary::Round,
        "sign" => Unary::Sign,
        "sin" => Unary::Sin,
        "cos" => Unary::Cos,
        "tan" => Unary::Tan,
        "asin" => Unary::Asin,
        "acos" => Unary::Acos,
        "atan" => Unary::Atan,
        "log" => Unary::Log,
        "exp" => Unary::Exp,
        "sqrt" => Unary::Sqrt,
        _ => return None,
    })
}

/// Maps a `Math` function name to its binary operation.
fn math_binary(name: &str) -> Option<Binary> {
    Some(match name {
        "min" => Binary::Min,
        "max" => Binary::Max,
        "pow" => Binary::Pow,
        "atan2" => Binary::Atan2,
        _ => return None,
    })
}

#[derive(Clone, Copy, PartialEq)]
enum Token<'a> {
    Number(f64),
    Ident(&'a str),
    LParen,
    RParen,
    Comma,
    Colon,
    Dot,
    QMark,
    Plus,
    Minus,
    Star,
    StarStar,
    Slash,
    Percent,
    Lt,
    Gt,
    LtEq,
    GtEq,
    Shl,
    Shr,
    UShr,
    EqEq,
    EqEqEq,
    NotEq,
    NotEqEq,
    Amp,
    AmpAmp,
    Bar,
    BarBar,
    Caret,
    Tilde,
    Bang,
}

/// One lexed token with its source text and byte range.
#[derive(Clone, Copy)]
struct Word<'a> {
    text: &'a str,
    token: Token<'a>,
    start: usize,
    end: usize,
}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
    lookahead: Option<Word<'a>>,
    code: Vec<Instruction>,
    stack: Vec<u16>,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str) -> Self {
        Parser {
            src: source.as_bytes(),
            pos: 0,
            lookahead: None,
            code: Vec::new(),
            stack: Vec::new(),
        }
    }

    /// Parses one expression whose infix operators all have at least
    /// `min_prec` precedence, leaving its result index on the stack.
    /// `depth` is the nesting level. Subexpressions recurse with `depth + 1`.
    fn expr(&mut self, min_prec: u8, depth: usize) -> Result<(), String> {
        if depth > MAX_NESTING {
            return Err(format!("expression nesting exceeds {} levels", MAX_NESTING));
        }
        // JavaScript rejects a prefixed unary expression as the left operand
        // of `**`, so remember how the left operand started.
        let prefixed = matches!(
            self.peek()?,
            Some(Word {
                token: Token::Plus | Token::Minus | Token::Bang | Token::Tilde,
                ..
            })
        );
        self.unary(depth)?;
        let mut prefixed = prefixed;
        while let Some(word) = self.peek()? {
            if word.token == Token::QMark {
                if min_prec > CONDITIONAL_PREC {
                    break;
                }
                self.advance();
                self.expr(CONDITIONAL_PREC, depth + 1)?;
                self.expect(Token::Colon, "':' of the conditional operator")?;
                self.expr(CONDITIONAL_PREC, depth + 1)?;
                let alternative = self.pop();
                let consequent = self.pop();
                let condition = self.pop();
                self.emit(Instruction::Select(condition, consequent, alternative))?;
                prefixed = false;
                continue;
            }
            let Some((op, prec, right_assoc)) = infix(word.token) else {
                break;
            };
            if prec < min_prec {
                break;
            }
            if op == Binary::Pow && prefixed {
                return Err(format!(
                    "a unary operand of '**' needs parentheses at byte {}",
                    word.start
                ));
            }
            self.advance();
            let next_prec = if right_assoc { prec } else { prec + 1 };
            self.expr(next_prec, depth + 1)?;
            let rhs = self.pop();
            let lhs = self.pop();
            self.emit(Instruction::Binary(op, lhs, rhs))?;
            prefixed = false;
        }
        Ok(())
    }

    /// Parses one unary expression: any number of prefix operators applied
    /// to a primary expression.
    fn unary(&mut self, depth: usize) -> Result<(), String> {
        if depth > MAX_NESTING {
            return Err(format!("expression nesting exceeds {} levels", MAX_NESTING));
        }
        let Some(word) = self.peek()? else {
            return Err("expected an expression but found end of source".to_owned());
        };
        let op = match word.token {
            Token::Plus => Unary::Plus,
            Token::Minus => Unary::Neg,
            Token::Bang => Unary::Not,
            Token::Tilde => Unary::BitNot,
            _ => return self.primary(depth),
        };
        let (start, end, token) = (word.start, word.end, word.token);
        self.advance();
        // `++` and `--` are update operators in JavaScript. Without the
        // check they would silently parse as two signs.
        if let Some(next) = self.peek()?
            && matches!(token, Token::Plus | Token::Minus)
            && next.token == token
            && next.start == end
        {
            let text = if token == Token::Plus { "++" } else { "--" };
            return Err(format!("unsupported operator '{}' at byte {}", text, start));
        }
        self.unary(depth + 1)?;
        let operand = self.pop();
        self.emit(Instruction::Unary(op, operand))?;
        Ok(())
    }

    /// Parses one primary expression: a literal, an identifier, a `Math`
    /// member or a parenthesized expression.
    fn primary(&mut self, depth: usize) -> Result<(), String> {
        let Some(word) = self.peek()? else {
            return Err("expected an expression but found end of source".to_owned());
        };
        match word.token {
            Token::Number(value) => {
                self.advance();
                self.emit(Instruction::Constant(value))?;
            }
            Token::Ident(name) => match name {
                "true" | "false" => {
                    self.advance();
                    self.emit(Instruction::Boolean(name == "true"))?;
                }
                "t" | "$input" => {
                    self.advance();
                    self.emit(Instruction::Input)?;
                }
                "time" => {
                    self.advance();
                    self.emit(Instruction::Time)?;
                }
                "NaN" | "Infinity" => {
                    return Err(format!(
                        "non-finite constant '{}' at byte {}",
                        name, word.start
                    ));
                }
                "Math" => {
                    self.advance();
                    self.math(depth)?;
                }
                _ => {
                    return Err(format!(
                        "unknown identifier '{}' at byte {}",
                        name, word.start
                    ));
                }
            },
            Token::LParen => {
                self.advance();
                self.expr(0, depth + 1)?;
                self.expect(Token::RParen, "')'")?;
            }
            _ => {
                return Err(format!(
                    "unexpected token '{}' at byte {}",
                    word.text, word.start
                ));
            }
        }
        Ok(())
    }

    /// Parses the `.member` after a `Math` identifier: a constant or a
    /// function call with fixed arity.
    fn math(&mut self, depth: usize) -> Result<(), String> {
        self.expect(Token::Dot, "'.' after 'Math'")?;
        let Some(word) = self.peek()? else {
            return Err("expected a Math member but found end of source".to_owned());
        };
        let Token::Ident(name) = word.token else {
            return Err(format!(
                "expected a Math member but found '{}' at byte {}",
                word.text, word.start
            ));
        };
        if let Some(op) = math_unary(name) {
            self.advance();
            self.expect(Token::LParen, "'('")?;
            self.expr(0, depth + 1)?;
            self.trailing_comma()?;
            self.expect(Token::RParen, "')'")?;
            let operand = self.pop();
            self.emit(Instruction::Unary(op, operand))?;
            return Ok(());
        }
        if let Some(op) = math_binary(name) {
            self.advance();
            self.expect(Token::LParen, "'('")?;
            self.expr(0, depth + 1)?;
            let wanted = format!("',' between the arguments of Math.{}", name);
            self.expect(Token::Comma, &wanted)?;
            self.expr(0, depth + 1)?;
            self.trailing_comma()?;
            self.expect(Token::RParen, "')'")?;
            let rhs = self.pop();
            let lhs = self.pop();
            self.emit(Instruction::Binary(op, lhs, rhs))?;
            return Ok(());
        }
        match name {
            "PI" => {
                self.advance();
                self.emit(Instruction::Constant(PI))?;
            }
            "E" => {
                self.advance();
                self.emit(Instruction::Constant(E))?;
            }
            _ => {
                return Err(format!(
                    "unknown Math member '{}' at byte {}",
                    name, word.start
                ));
            }
        }
        Ok(())
    }

    /// Consumes one comma that directly precedes the closing parenthesis,
    /// which JavaScript call syntax allows.
    fn trailing_comma(&mut self) -> Result<(), String> {
        if matches!(
            self.peek()?,
            Some(Word {
                token: Token::Comma,
                ..
            })
        ) {
            self.advance();
        }
        Ok(())
    }

    /// Consumes the next token when it is `want`, or reports `wanted`.
    fn expect(&mut self, want: Token<'a>, wanted: &str) -> Result<(), String> {
        match self.peek()? {
            Some(word) if word.token == want => {
                self.advance();
                Ok(())
            }
            Some(word) => Err(format!(
                "expected {} but found '{}' at byte {}",
                wanted, word.text, word.start
            )),
            None => Err(format!("expected {} but found end of source", wanted)),
        }
    }

    fn peek(&mut self) -> Result<Option<Word<'a>>, String> {
        if self.lookahead.is_none() {
            self.lookahead = self.lex()?;
        }
        Ok(self.lookahead)
    }

    fn advance(&mut self) {
        self.lookahead = None;
    }

    fn pop(&mut self) -> u16 {
        self.stack
            .pop()
            .expect("a parsed subexpression leaves its index on the stack")
    }

    fn emit(&mut self, instruction: Instruction) -> Result<u16, String> {
        if self.code.len() >= MAX_INSTRUCTIONS {
            return Err(format!(
                "expression needs more than {} instructions",
                MAX_INSTRUCTIONS
            ));
        }
        self.code.push(instruction);
        let index = (self.code.len() - 1) as u16;
        self.stack.push(index);
        Ok(index)
    }

    /// Lexes the next token, skipping whitespace and comments. The byte
    /// cursor stops after the token.
    fn lex(&mut self) -> Result<Option<Word<'a>>, String> {
        loop {
            let Some(&b) = self.src.get(self.pos) else {
                return Ok(None);
            };
            let terminator_len = line_terminator_len(&self.src[self.pos..]);
            if terminator_len != 0 {
                self.pos += terminator_len;
                continue;
            }
            match b {
                b' ' | b'\t' | b'\x0b' | b'\x0c' => self.pos += 1,
                b'/' if self.src.get(self.pos + 1) == Some(&b'/') => {
                    self.pos += 2;
                    while self.pos < self.src.len()
                        && line_terminator_len(&self.src[self.pos..]) == 0
                    {
                        self.pos += 1;
                    }
                }
                b'/' if self.src.get(self.pos + 1) == Some(&b'*') => {
                    let start = self.pos;
                    self.pos += 2;
                    loop {
                        match self.src.get(self.pos) {
                            None => {
                                return Err(format!("unterminated comment at byte {}", start));
                            }
                            Some(b'*') if self.src.get(self.pos + 1) == Some(&b'/') => {
                                self.pos += 2;
                                break;
                            }
                            Some(_) => self.pos += 1,
                        }
                    }
                }
                _ => break,
            }
        }
        let start = self.pos;
        let token = self.lex_token()?;
        let end = self.pos;
        let text = std::str::from_utf8(&self.src[start..end]).unwrap_or("");
        Ok(Some(Word {
            text,
            token,
            start,
            end,
        }))
    }

    fn lex_token(&mut self) -> Result<Token<'a>, String> {
        let b = self.src[self.pos];
        let two = self.src.get(self.pos + 1).copied();
        let three = self.src.get(self.pos + 2).copied();
        let unsupported =
            |pos: usize, text: &str| format!("unsupported operator '{}' at byte {}", text, pos);
        let token = match b {
            b'0'..=b'9' => self.lex_number()?,
            b'.' if two.is_some_and(|c| c.is_ascii_digit()) => self.lex_number()?,
            b'A'..=b'Z' | b'a'..=b'z' | b'_' | b'$' => self.lex_ident(),
            b'(' => {
                self.pos += 1;
                Token::LParen
            }
            b')' => {
                self.pos += 1;
                Token::RParen
            }
            b',' => {
                self.pos += 1;
                Token::Comma
            }
            b':' => {
                self.pos += 1;
                Token::Colon
            }
            b'.' => {
                self.pos += 1;
                Token::Dot
            }
            b'?' => {
                if two == Some(b'?') {
                    return Err(unsupported(self.pos, "??"));
                }
                if two == Some(b'.') {
                    return Err(unsupported(self.pos, "?."));
                }
                self.pos += 1;
                Token::QMark
            }
            b'+' => {
                if two == Some(b'=') {
                    return Err(unsupported(self.pos, "+="));
                }
                self.pos += 1;
                Token::Plus
            }
            b'-' => {
                if two == Some(b'=') {
                    return Err(unsupported(self.pos, "-="));
                }
                self.pos += 1;
                Token::Minus
            }
            b'*' => {
                if two == Some(b'*') {
                    if three == Some(b'=') {
                        return Err(unsupported(self.pos, "**="));
                    }
                    self.pos += 2;
                    Token::StarStar
                } else {
                    if two == Some(b'=') {
                        return Err(unsupported(self.pos, "*="));
                    }
                    self.pos += 1;
                    Token::Star
                }
            }
            b'/' => {
                if two == Some(b'=') {
                    return Err(unsupported(self.pos, "/="));
                }
                self.pos += 1;
                Token::Slash
            }
            b'%' => {
                if two == Some(b'=') {
                    return Err(unsupported(self.pos, "%="));
                }
                self.pos += 1;
                Token::Percent
            }
            b'<' => {
                if two == Some(b'<') {
                    if three == Some(b'=') {
                        return Err(unsupported(self.pos, "<<="));
                    }
                    self.pos += 2;
                    Token::Shl
                } else if two == Some(b'=') {
                    self.pos += 2;
                    Token::LtEq
                } else {
                    self.pos += 1;
                    Token::Lt
                }
            }
            b'>' => {
                if two == Some(b'>') {
                    if three == Some(b'>') {
                        if self.src.get(self.pos + 3) == Some(&b'=') {
                            return Err(unsupported(self.pos, ">>>="));
                        }
                        self.pos += 3;
                        Token::UShr
                    } else {
                        if three == Some(b'=') {
                            return Err(unsupported(self.pos, ">>="));
                        }
                        self.pos += 2;
                        Token::Shr
                    }
                } else if two == Some(b'=') {
                    self.pos += 2;
                    Token::GtEq
                } else {
                    self.pos += 1;
                    Token::Gt
                }
            }
            b'=' => {
                if two == Some(b'=') {
                    if three == Some(b'=') {
                        self.pos += 3;
                        Token::EqEqEq
                    } else {
                        self.pos += 2;
                        Token::EqEq
                    }
                } else if two == Some(b'>') {
                    return Err(unsupported(self.pos, "=>"));
                } else {
                    return Err(unsupported(self.pos, "="));
                }
            }
            b'!' => {
                if two == Some(b'=') {
                    if three == Some(b'=') {
                        self.pos += 3;
                        Token::NotEqEq
                    } else {
                        self.pos += 2;
                        Token::NotEq
                    }
                } else {
                    self.pos += 1;
                    Token::Bang
                }
            }
            b'&' => {
                if two == Some(b'&') {
                    if three == Some(b'=') {
                        return Err(unsupported(self.pos, "&&="));
                    }
                    self.pos += 2;
                    Token::AmpAmp
                } else {
                    if two == Some(b'=') {
                        return Err(unsupported(self.pos, "&="));
                    }
                    self.pos += 1;
                    Token::Amp
                }
            }
            b'|' => {
                if two == Some(b'|') {
                    if three == Some(b'=') {
                        return Err(unsupported(self.pos, "||="));
                    }
                    self.pos += 2;
                    Token::BarBar
                } else {
                    if two == Some(b'=') {
                        return Err(unsupported(self.pos, "|="));
                    }
                    self.pos += 1;
                    Token::Bar
                }
            }
            b'^' => {
                if two == Some(b'=') {
                    return Err(unsupported(self.pos, "^="));
                }
                self.pos += 1;
                Token::Caret
            }
            b'~' => {
                self.pos += 1;
                Token::Tilde
            }
            _ => {
                return Err(format!("unexpected character at byte {}", self.pos));
            }
        };
        Ok(token)
    }

    /// Lexes a number literal: decimal with fraction and exponent, or a
    /// `0x`, `0b` or `0o` radix prefix.
    fn lex_number(&mut self) -> Result<Token<'a>, String> {
        let start = self.pos;
        if self.src[self.pos] == b'0'
            && let Some(&prefix) = self.src.get(self.pos + 1)
        {
            let radix = match prefix {
                b'x' | b'X' => Some(16u32),
                b'b' | b'B' => Some(2u32),
                b'o' | b'O' => Some(8u32),
                b'0'..=b'9' => {
                    return Err(format!(
                        "unsupported legacy octal literal at byte {}; write 0o for octal",
                        start
                    ));
                }
                _ => None,
            };
            if let Some(radix) = radix {
                self.pos += 2;
                let digits = self.pos;
                while matches!(self.src.get(self.pos), Some(&c) if (c as char).is_digit(radix)) {
                    self.pos += 1;
                }
                if self.pos == digits {
                    return Err(format!("malformed number literal at byte {}", start));
                }
                let mut value = 0.0;
                for &c in &self.src[digits..self.pos] {
                    let digit = (c as char).to_digit(radix).unwrap_or(0) as f64;
                    value = value * radix as f64 + digit;
                }
                if !value.is_finite() {
                    return Err(format!("non-finite number literal at byte {start}"));
                }
                return Ok(Token::Number(value));
            }
        }
        while matches!(self.src.get(self.pos), Some(c) if c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.src.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            while matches!(self.src.get(self.pos), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.src.get(self.pos), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.src.get(self.pos), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !matches!(self.src.get(self.pos), Some(c) if c.is_ascii_digit()) {
                return Err(format!("malformed number literal at byte {}", start));
            }
            while matches!(self.src.get(self.pos), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        let text = std::str::from_utf8(&self.src[start..self.pos]).unwrap_or("");
        let value: f64 = text
            .parse()
            .map_err(|_| format!("malformed number literal at byte {}", start))?;
        if !value.is_finite() {
            return Err(format!(
                "non-finite number literal '{}' at byte {}",
                text, start
            ));
        }
        Ok(Token::Number(value))
    }

    fn lex_ident(&mut self) -> Token<'a> {
        let start = self.pos;
        while matches!(self.src.get(self.pos), Some(c)
            if c.is_ascii_alphanumeric() || *c == b'_' || *c == b'$')
        {
            self.pos += 1;
        }
        Token::Ident(std::str::from_utf8(&self.src[start..self.pos]).unwrap_or(""))
    }
}

fn line_terminator_len(source: &[u8]) -> usize {
    match source {
        [b'\r' | b'\n', ..] => 1,
        [0xe2, 0x80, 0xa8 | 0xa9, ..] => 3,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(source: &str) -> String {
        parse(source).expect_err("source should not parse")
    }

    #[test]
    fn parses_number_literal_forms() {
        let cases = [
            ("2", 2.0),
            ("2.5", 2.5),
            (".5", 0.5),
            ("5.", 5.0),
            ("1e2", 100.0),
            ("1.5e-2", 0.015),
            ("1E+2", 100.0),
            ("5.e3", 5000.0),
            ("0x1f", 31.0),
            ("0Xff", 255.0),
            ("0b101", 5.0),
            ("0o17", 15.0),
        ];
        for (source, value) in cases {
            assert_eq!(
                parse(source).unwrap_or_else(|e| panic!("{source}: {e}")),
                vec![Instruction::Constant(value)],
                "{source}"
            );
        }
    }

    #[test]
    fn rejects_malformed_number_literals() {
        for source in ["0x", "0b", "0o", "1e", "1e+", "017", "08", "1e999", "-"] {
            assert!(!parse(source).is_ok(), "{source} should not parse");
        }
        assert!(error("017").contains("octal"));
        assert!(error("1e999").contains("non-finite"));
        for (prefix, digit, count) in [("0x", "f", 300), ("0b", "1", 1100), ("0o", "7", 400)] {
            assert!(error(&format!("{prefix}{}", digit.repeat(count))).contains("non-finite"));
        }
    }

    #[test]
    fn maps_identifiers_to_inputs() {
        assert_eq!(parse("t").unwrap(), vec![Instruction::Input]);
        assert_eq!(parse("$input").unwrap(), vec![Instruction::Input]);
        assert_eq!(parse("time").unwrap(), vec![Instruction::Time]);
    }

    #[test]
    fn rejects_unknown_and_nonfinite_identifiers() {
        assert!(error("foo").contains("unknown identifier 'foo'"));
        assert!(error("Math").contains("expected '.'")); // Math alone misses its member
        assert!(error("NaN").contains("non-finite constant 'NaN'"));
        assert!(error("Infinity").contains("non-finite"));
    }

    #[test]
    fn parses_math_constants() {
        assert_eq!(parse("Math.PI").unwrap(), vec![Instruction::Constant(PI)]);
        assert_eq!(parse("Math.E").unwrap(), vec![Instruction::Constant(E)]);
        assert!(error("Math.PII").contains("unknown Math member"));
        assert!(error("Math(1)").contains("expected '.'"));
    }

    #[test]
    fn parses_math_calls() {
        assert_eq!(
            parse("Math.sin(t)").unwrap(),
            vec![Instruction::Input, Instruction::Unary(Unary::Sin, 0)]
        );
        assert_eq!(
            parse("Math.min(t, 2)").unwrap(),
            vec![
                Instruction::Input,
                Instruction::Constant(2.0),
                Instruction::Binary(Binary::Min, 0, 1)
            ]
        );
        // A trailing comma before the closing parenthesis is legal.
        assert_eq!(
            parse("Math.min(1, 2,)").unwrap(),
            vec![
                Instruction::Constant(1.0),
                Instruction::Constant(2.0),
                Instruction::Binary(Binary::Min, 0, 1)
            ]
        );
        assert_eq!(
            parse("Math.atan2(1, 2)").unwrap().last(),
            Some(&Instruction::Binary(Binary::Atan2, 0, 1))
        );
    }

    #[test]
    fn checks_math_call_arity() {
        assert!(error("Math.abs()").contains("unexpected token ')'"));
        assert!(error("Math.abs(1, 2)").contains("expected ')'"));
        assert!(error("Math.min(1)").contains("expected ','"));
        assert!(error("Math.min(1, 2, 3)").contains("expected ')'"));
    }

    #[test]
    fn parses_binary_operators_in_source_order() {
        let cases = [
            ("1+2", Binary::Add),
            ("1-2", Binary::Sub),
            ("1*2", Binary::Mul),
            ("1/2", Binary::Div),
            ("1%2", Binary::Mod),
            ("1**2", Binary::Pow),
            ("1&2", Binary::BitAnd),
            ("1|2", Binary::BitOr),
            ("1^2", Binary::BitXor),
            ("1<<2", Binary::Shl),
            ("1>>2", Binary::Shr),
            ("1>>>2", Binary::UnsignedShr),
            ("1<2", Binary::Less),
            ("1<=2", Binary::LessEqual),
            ("1>2", Binary::Greater),
            ("1>=2", Binary::GreaterEqual),
            ("1==2", Binary::Equal),
            ("1===2", Binary::StrictEqual),
            ("1!=2", Binary::NotEqual),
            ("1!==2", Binary::StrictNotEqual),
            ("1&&2", Binary::And),
            ("1||2", Binary::Or),
        ];
        for (source, op) in cases {
            let code = parse(source).unwrap_or_else(|e| panic!("{source}: {e}"));
            assert_eq!(code.len(), 3, "{source}");
            assert_eq!(code[2], Instruction::Binary(op, 0, 1), "{source}");
        }
    }

    #[test]
    fn orders_operators_by_precedence_and_associativity() {
        assert_eq!(
            parse("1+2*3").unwrap(),
            vec![
                Instruction::Constant(1.0),
                Instruction::Constant(2.0),
                Instruction::Constant(3.0),
                Instruction::Binary(Binary::Mul, 1, 2),
                Instruction::Binary(Binary::Add, 0, 3)
            ]
        );
        // Additive operators are left-associative, `**` is not.
        assert_eq!(
            parse("16/4/2").unwrap(),
            vec![
                Instruction::Constant(16.0),
                Instruction::Constant(4.0),
                Instruction::Binary(Binary::Div, 0, 1),
                Instruction::Constant(2.0),
                Instruction::Binary(Binary::Div, 2, 3)
            ]
        );
        assert_eq!(
            parse("2**3**2").unwrap(),
            vec![
                Instruction::Constant(2.0),
                Instruction::Constant(3.0),
                Instruction::Constant(2.0),
                Instruction::Binary(Binary::Pow, 1, 2),
                Instruction::Binary(Binary::Pow, 0, 3)
            ]
        );
        // Shifts bind looser than additive operators, tighter than `&`.
        assert_eq!(
            parse("1+2<<3").unwrap(),
            vec![
                Instruction::Constant(1.0),
                Instruction::Constant(2.0),
                Instruction::Binary(Binary::Add, 0, 1),
                Instruction::Constant(3.0),
                Instruction::Binary(Binary::Shl, 2, 3)
            ]
        );
        assert_eq!(
            parse("1|2^3&4").unwrap(),
            vec![
                Instruction::Constant(1.0),
                Instruction::Constant(2.0),
                Instruction::Constant(3.0),
                Instruction::Constant(4.0),
                Instruction::Binary(Binary::BitAnd, 2, 3),
                Instruction::Binary(Binary::BitXor, 1, 4),
                Instruction::Binary(Binary::BitOr, 0, 5)
            ]
        );
    }

    #[test]
    fn grouping_overrides_precedence() {
        assert_eq!(
            parse("(1+2)*3").unwrap(),
            vec![
                Instruction::Constant(1.0),
                Instruction::Constant(2.0),
                Instruction::Binary(Binary::Add, 0, 1),
                Instruction::Constant(3.0),
                Instruction::Binary(Binary::Mul, 2, 3)
            ]
        );
        assert_eq!(parse("((1))").unwrap(), vec![Instruction::Constant(1.0)]);
    }

    #[test]
    fn parses_unary_operators() {
        let cases = [
            ("+t", Unary::Plus),
            ("-t", Unary::Neg),
            ("!t", Unary::Not),
            ("~t", Unary::BitNot),
        ];
        for (source, op) in cases {
            assert_eq!(
                parse(source).unwrap_or_else(|e| panic!("{source}: {e}")),
                vec![Instruction::Input, Instruction::Unary(op, 0)],
                "{source}"
            );
        }
        assert_eq!(
            parse("- -1").unwrap(),
            vec![
                Instruction::Constant(1.0),
                Instruction::Unary(Unary::Neg, 0),
                Instruction::Unary(Unary::Neg, 1)
            ]
        );
        assert_eq!(
            parse("-~1").unwrap(),
            vec![
                Instruction::Constant(1.0),
                Instruction::Unary(Unary::BitNot, 0),
                Instruction::Unary(Unary::Neg, 1)
            ]
        );
    }

    #[test]
    fn rejects_increment_operators() {
        assert!(error("--1").contains("unsupported operator '--'"));
        assert!(error("++1").contains("unsupported operator '++'"));
    }

    #[test]
    fn requires_parens_around_unary_pow_operands() {
        assert!(error("-2**2").contains("parentheses"));
        assert_eq!(
            parse("(-2)**2").unwrap(),
            vec![
                Instruction::Constant(2.0),
                Instruction::Unary(Unary::Neg, 0),
                Instruction::Constant(2.0),
                Instruction::Binary(Binary::Pow, 1, 2)
            ]
        );
        assert_eq!(
            parse("2**-2").unwrap(),
            vec![
                Instruction::Constant(2.0),
                Instruction::Constant(2.0),
                Instruction::Unary(Unary::Neg, 1),
                Instruction::Binary(Binary::Pow, 0, 2)
            ]
        );
    }

    #[test]
    fn parses_conditional_right_associatively() {
        assert_eq!(
            parse("t?1:2").unwrap(),
            vec![
                Instruction::Input,
                Instruction::Constant(1.0),
                Instruction::Constant(2.0),
                Instruction::Select(0, 1, 2)
            ]
        );
        assert_eq!(
            parse("1?2:3?4:5").unwrap(),
            vec![
                Instruction::Constant(1.0),
                Instruction::Constant(2.0),
                Instruction::Constant(3.0),
                Instruction::Constant(4.0),
                Instruction::Constant(5.0),
                Instruction::Select(2, 3, 4),
                Instruction::Select(0, 1, 5)
            ]
        );
        // The conditional is the loosest operator.
        assert_eq!(
            parse("1+2?3:4").unwrap().last(),
            Some(&Instruction::Select(2, 3, 4))
        );
    }

    #[test]
    fn skips_comments() {
        assert_eq!(
            parse("1 // trailing note\n").unwrap(),
            vec![Instruction::Constant(1.0)]
        );
        for terminator in ["\n", "\r", "\r\n", "\u{2028}", "\u{2029}"] {
            assert_eq!(
                parse(&format!("1 // note{terminator}+ 2")).unwrap(),
                vec![
                    Instruction::Constant(1.0),
                    Instruction::Constant(2.0),
                    Instruction::Binary(Binary::Add, 0, 1)
                ]
            );
        }
        assert_eq!(
            parse("1 /* inner */ + 2").unwrap(),
            vec![
                Instruction::Constant(1.0),
                Instruction::Constant(2.0),
                Instruction::Binary(Binary::Add, 0, 1)
            ]
        );
        assert!(error("/* unterminated").contains("unterminated comment"));
        assert!(error("// only a comment").contains("expected an expression"));
    }

    #[test]
    fn rejects_incomplete_expressions() {
        for source in ["", " ", "2 +", "(1", "1?2", "()", "2**"] {
            assert!(!parse(source).is_ok(), "{source} should not parse");
        }
        assert!(error("(1").contains("expected ')'"));
        assert!(error("1?2").contains("expected ':'"));
    }

    #[test]
    fn rejects_trailing_tokens_and_unsupported_syntax() {
        assert!(error("1 2").contains("trailing token"));
        assert!(error("1)").contains("trailing token"));
        assert!(error("1,2").contains("trailing token"));
        assert!(error("t=1").contains("unsupported operator '='"));
        assert!(error("t+=1").contains("unsupported operator '+='"));
        assert!(error("t??1").contains("unsupported operator '??'"));
        assert!(error(";").contains("unexpected character"));
        assert!(error("'x'").contains("unexpected character"));
    }

    #[test]
    fn bounds_nesting_at_sixty_four() {
        let ok = format!("{}1{}", "(".repeat(64), ")".repeat(64));
        assert_eq!(parse(&ok).unwrap(), vec![Instruction::Constant(1.0)]);
        let deep = format!("{}1{}", "(".repeat(65), ")".repeat(65));
        assert!(error(&deep).contains("nesting"));
    }

    #[test]
    fn bounds_instructions_at_two_hundred_fifty_six() {
        let ok = format!("1{}", "+1".repeat(127));
        assert_eq!(parse(&ok).unwrap().len(), 255);
        let over = format!("1{}", "+1".repeat(128));
        assert!(error(&over).contains("instructions"));
    }

    #[test]
    fn bounds_source_size() {
        let ok = format!("{}(1)", " ".repeat(65533));
        assert_eq!(parse(&ok).unwrap(), vec![Instruction::Constant(1.0)]);
        let big = " ".repeat(MAX_SOURCE_BYTES + 1);
        assert!(parse(&big).unwrap_err().contains("bytes"));
    }

    #[test]
    fn parses_upstream_node_examples() {
        // The bytebeat example from the kabelsalat library.
        let bytebeat = "// Fractalized Past\n(t>>10^t>>11)%5*((t>>14&3^t>>15&1)+1)*t%99+((3+(t>>14&3)-(t>>16&1))/3*t%99&64)";
        let code = parse(bytebeat).unwrap();
        assert_eq!(code.first(), Some(&Instruction::Input));
        assert!(matches!(
            code.last(),
            Some(Instruction::Binary(Binary::Add, _, _))
        ));
        // The raw example from the kabelsalat library.
        let raw = "(time*110%1*2-1)*$input";
        let code = parse(raw).unwrap();
        assert!(matches!(
            code.last(),
            Some(Instruction::Binary(Binary::Mul, _, _))
        ));
        assert!(code.contains(&Instruction::Time));
        assert!(code.contains(&Instruction::Input));
    }
}
