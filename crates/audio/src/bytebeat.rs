//! A compiled `byteBeatExpression`.
//!
//! Bytebeat expressions are written in JavaScript, with every `Math` member,
//! a few `chyx` helpers and `int` bound as bare names. That cannot be
//! evaluated per sample on the audio thread, so the expression is compiled
//! once on the producer side into a fixed-size postfix program the callback
//! only walks.
//!
//! The subset covers what bytebeat is actually written in: `t`, numeric
//! literals, the full run of JavaScript's arithmetic, bitwise, comparison and
//! logical operators, the conditional, and the `Math`/`chyx` functions and
//! constants. The compiler refuses anything outside it by name. It does not
//! drop the control and play built-in expression 0.
//!
//! Integer semantics follow JavaScript exactly: arithmetic stays in f64 and
//! every bitwise step coerces through `ToInt32` (so `t >> 66` is `t >> 2`,
//! the shift count being taken modulo 32).

use crate::backend::to_int32;

/// Longest program a single expression may compile to.
///
/// Sized against real bytebeat rather than a round number: the documented
/// `t*(t>>15^t>>66)` is nine operations, and the fifteen built-ins top out at
/// twenty-six.
pub const MAX_BYTEBEAT_OPS: usize = 64;

/// Deepest the evaluation stack can go, which is the expression's nesting.
const STACK_DEPTH: usize = 32;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum ByteBeatOp {
    /// Push the sample counter.
    #[default]
    PushT,
    /// Push the constant at the same index.
    PushConst,
    Neg,
    BitNot,
    LogicalNot,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Shl,
    Shr,
    UShr,
    BitAnd,
    BitOr,
    BitXor,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
    Equal,
    NotEqual,
    /// JavaScript's `&&`, which yields an OPERAND and not a boolean.
    LogicalAnd,
    /// JavaScript's `||`, likewise.
    LogicalOr,
    /// `cond ? a : b`, with all three already on the stack.
    Select,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Sqrt,
    Log,
    Log2,
    Exp,
    Floor,
    Ceil,
    Round,
    Trunc,
    Abs,
    Sign,
    Min,
    Max,
    Pow,
    Atan2,
    /// `chyx.sinf` - a sine repeating every 128 steps, not every 2pi.
    SinF,
    CosF,
    TanF,
    /// `chyx.bitC(x, y, z)` - `x & y ? z : 0`.
    BitC,
    /// `chyx.br(x, size)` - reverse the low `size` bits.
    BitReverse,
}

impl ByteBeatOp {
    /// The inverse of `op as u8`, for the flat wire encoding. `None` for a
    /// value no current variant owns, so a malformed record is refused rather
    /// than reinterpreted.
    #[must_use]
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::PushT,
            1 => Self::PushConst,
            2 => Self::Neg,
            3 => Self::BitNot,
            4 => Self::LogicalNot,
            5 => Self::Add,
            6 => Self::Sub,
            7 => Self::Mul,
            8 => Self::Div,
            9 => Self::Rem,
            10 => Self::Shl,
            11 => Self::Shr,
            12 => Self::UShr,
            13 => Self::BitAnd,
            14 => Self::BitOr,
            15 => Self::BitXor,
            16 => Self::Less,
            17 => Self::Greater,
            18 => Self::LessEqual,
            19 => Self::GreaterEqual,
            20 => Self::Equal,
            21 => Self::NotEqual,
            22 => Self::LogicalAnd,
            23 => Self::LogicalOr,
            24 => Self::Select,
            25 => Self::Sin,
            26 => Self::Cos,
            27 => Self::Tan,
            28 => Self::Asin,
            29 => Self::Acos,
            30 => Self::Atan,
            31 => Self::Sqrt,
            32 => Self::Log,
            33 => Self::Log2,
            34 => Self::Exp,
            35 => Self::Floor,
            36 => Self::Ceil,
            37 => Self::Round,
            38 => Self::Trunc,
            39 => Self::Abs,
            40 => Self::Sign,
            41 => Self::Min,
            42 => Self::Max,
            43 => Self::Pow,
            44 => Self::Atan2,
            45 => Self::SinF,
            46 => Self::CosF,
            47 => Self::TanF,
            48 => Self::BitC,
            49 => Self::BitReverse,
            _ => return None,
        })
    }
}

/// One expression, compiled.
///
/// Two parallel arrays rather than an enum carrying its operand: a fieldless
/// enum is one byte, so this is 64 + 512 against the 1024 an
/// `[enum { Push(f64), .. }; 64]` would cost, and the whole thing stays `Copy`
/// so it can ride to the audio thread inside the onset event.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ByteBeatProgram {
    ops: [ByteBeatOp; MAX_BYTEBEAT_OPS],
    constants: [f64; MAX_BYTEBEAT_OPS],
    len: u8,
}

impl Default for ByteBeatProgram {
    fn default() -> Self {
        Self {
            ops: [ByteBeatOp::PushT; MAX_BYTEBEAT_OPS],
            constants: [0.0; MAX_BYTEBEAT_OPS],
            len: 0,
        }
    }
}

impl ByteBeatProgram {
    #[must_use]
    pub fn len(&self) -> usize {
        usize::from(self.len)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The op and constant at `index`, for the flat encoding.
    #[must_use]
    pub fn step(&self, index: usize) -> (ByteBeatOp, f64) {
        (self.ops[index], self.constants[index])
    }

    /// Rebuild from an encoded form. Anything past [`MAX_BYTEBEAT_OPS`] is
    /// refused rather than truncated: half an expression is not the
    /// expression.
    pub fn from_steps(steps: &[(ByteBeatOp, f64)]) -> Result<Self, String> {
        if steps.len() > MAX_BYTEBEAT_OPS {
            return Err(format!(
                "bytebeat program has {} operations, more than the {MAX_BYTEBEAT_OPS} a voice carries",
                steps.len()
            ));
        }
        let mut program = Self::default();
        for (index, (op, constant)) in steps.iter().enumerate() {
            program.ops[index] = *op;
            program.constants[index] = *constant;
        }
        program.len = steps.len() as u8;
        Ok(program)
    }

    /// Evaluate at sample counter `t`.
    ///
    /// Allocation-free and branch-bounded: the callback walks at most
    /// [`MAX_BYTEBEAT_OPS`] steps over a fixed stack. A malformed program
    /// (which the compiler will not produce) yields 0 rather than panicking,
    /// because a voice that cannot be evaluated must still not stop the music.
    #[must_use]
    pub fn eval(&self, t: f64) -> f64 {
        let mut stack = [0.0f64; STACK_DEPTH];
        let mut top: usize = 0;
        macro_rules! pop {
            () => {{
                if top == 0 {
                    return 0.0;
                }
                top -= 1;
                stack[top]
            }};
        }
        macro_rules! push {
            ($value:expr) => {{
                if top == STACK_DEPTH {
                    return 0.0;
                }
                stack[top] = $value;
                top += 1;
            }};
        }
        // JavaScript's truthiness for the numbers this language produces.
        let truthy = |value: f64| value != 0.0 && !value.is_nan();

        for index in 0..self.len() {
            match self.ops[index] {
                ByteBeatOp::PushT => push!(t),
                ByteBeatOp::PushConst => push!(self.constants[index]),
                ByteBeatOp::Neg => {
                    let a = pop!();
                    push!(-a);
                }
                ByteBeatOp::BitNot => {
                    let a = pop!();
                    push!(f64::from(!to_int32(a)));
                }
                ByteBeatOp::LogicalNot => {
                    let a = pop!();
                    push!(f64::from(u8::from(!truthy(a))));
                }
                ByteBeatOp::Add => {
                    let (b, a) = (pop!(), pop!());
                    push!(a + b);
                }
                ByteBeatOp::Sub => {
                    let (b, a) = (pop!(), pop!());
                    push!(a - b);
                }
                ByteBeatOp::Mul => {
                    let (b, a) = (pop!(), pop!());
                    push!(a * b);
                }
                ByteBeatOp::Div => {
                    let (b, a) = (pop!(), pop!());
                    push!(a / b);
                }
                ByteBeatOp::Rem => {
                    let (b, a) = (pop!(), pop!());
                    push!(a % b);
                }
                ByteBeatOp::Shl => {
                    let (b, a) = (pop!(), pop!());
                    push!(f64::from(to_int32(a).wrapping_shl(shift_count(b))));
                }
                ByteBeatOp::Shr => {
                    let (b, a) = (pop!(), pop!());
                    push!(f64::from(to_int32(a).wrapping_shr(shift_count(b))));
                }
                ByteBeatOp::UShr => {
                    let (b, a) = (pop!(), pop!());
                    push!(f64::from((to_int32(a) as u32).wrapping_shr(shift_count(b))));
                }
                ByteBeatOp::BitAnd => {
                    let (b, a) = (pop!(), pop!());
                    push!(f64::from(to_int32(a) & to_int32(b)));
                }
                ByteBeatOp::BitOr => {
                    let (b, a) = (pop!(), pop!());
                    push!(f64::from(to_int32(a) | to_int32(b)));
                }
                ByteBeatOp::BitXor => {
                    let (b, a) = (pop!(), pop!());
                    push!(f64::from(to_int32(a) ^ to_int32(b)));
                }
                ByteBeatOp::Less => {
                    let (b, a) = (pop!(), pop!());
                    push!(f64::from(u8::from(a < b)));
                }
                ByteBeatOp::Greater => {
                    let (b, a) = (pop!(), pop!());
                    push!(f64::from(u8::from(a > b)));
                }
                ByteBeatOp::LessEqual => {
                    let (b, a) = (pop!(), pop!());
                    push!(f64::from(u8::from(a <= b)));
                }
                ByteBeatOp::GreaterEqual => {
                    let (b, a) = (pop!(), pop!());
                    push!(f64::from(u8::from(a >= b)));
                }
                ByteBeatOp::Equal => {
                    let (b, a) = (pop!(), pop!());
                    push!(f64::from(u8::from(a == b)));
                }
                ByteBeatOp::NotEqual => {
                    let (b, a) = (pop!(), pop!());
                    push!(f64::from(u8::from(a != b)));
                }
                // `&&` and `||` yield an OPERAND in JavaScript, not a boolean,
                // and bytebeat leans on that.
                ByteBeatOp::LogicalAnd => {
                    let (b, a) = (pop!(), pop!());
                    push!(if truthy(a) { b } else { a });
                }
                ByteBeatOp::LogicalOr => {
                    let (b, a) = (pop!(), pop!());
                    push!(if truthy(a) { a } else { b });
                }
                ByteBeatOp::Select => {
                    let (b, a, cond) = (pop!(), pop!(), pop!());
                    push!(if truthy(cond) { a } else { b });
                }
                ByteBeatOp::Sin => unary(&mut stack, &mut top, f64::sin),
                ByteBeatOp::Cos => unary(&mut stack, &mut top, f64::cos),
                ByteBeatOp::Tan => unary(&mut stack, &mut top, f64::tan),
                ByteBeatOp::Asin => unary(&mut stack, &mut top, f64::asin),
                ByteBeatOp::Acos => unary(&mut stack, &mut top, f64::acos),
                ByteBeatOp::Atan => unary(&mut stack, &mut top, f64::atan),
                ByteBeatOp::Sqrt => unary(&mut stack, &mut top, f64::sqrt),
                ByteBeatOp::Log => unary(&mut stack, &mut top, f64::ln),
                ByteBeatOp::Log2 => unary(&mut stack, &mut top, f64::log2),
                ByteBeatOp::Exp => unary(&mut stack, &mut top, f64::exp),
                ByteBeatOp::Floor => unary(&mut stack, &mut top, f64::floor),
                ByteBeatOp::Ceil => unary(&mut stack, &mut top, f64::ceil),
                ByteBeatOp::Round => unary(&mut stack, &mut top, js_round),
                ByteBeatOp::Trunc => unary(&mut stack, &mut top, f64::trunc),
                ByteBeatOp::Abs => unary(&mut stack, &mut top, f64::abs),
                ByteBeatOp::Sign => unary(&mut stack, &mut top, js_sign),
                ByteBeatOp::SinF => unary(&mut stack, &mut top, |x| {
                    (x * std::f64::consts::PI / 128.0).sin()
                }),
                ByteBeatOp::CosF => unary(&mut stack, &mut top, |x| {
                    (x * std::f64::consts::PI / 128.0).cos()
                }),
                ByteBeatOp::TanF => unary(&mut stack, &mut top, |x| {
                    (x * std::f64::consts::PI / 128.0).tan()
                }),
                ByteBeatOp::Min => {
                    let (b, a) = (pop!(), pop!());
                    push!(js_min(a, b));
                }
                ByteBeatOp::Max => {
                    let (b, a) = (pop!(), pop!());
                    push!(js_max(a, b));
                }
                ByteBeatOp::Pow => {
                    let (b, a) = (pop!(), pop!());
                    push!(a.powf(b));
                }
                ByteBeatOp::Atan2 => {
                    let (b, a) = (pop!(), pop!());
                    push!(a.atan2(b));
                }
                ByteBeatOp::BitC => {
                    let (z, y, x) = (pop!(), pop!(), pop!());
                    push!(if to_int32(x) & to_int32(y) != 0 {
                        z
                    } else {
                        0.0
                    });
                }
                ByteBeatOp::BitReverse => {
                    let (size, x) = (pop!(), pop!());
                    push!(bit_reverse(x, size));
                }
            }
        }
        if top == 0 { 0.0 } else { stack[top - 1] }
    }
}

/// JavaScript takes the shift count modulo 32.
fn shift_count(value: f64) -> u32 {
    (to_int32(value) as u32) & 31
}

/// `Math.round` rounds halves toward +Infinity, where Rust rounds away from
/// zero - they disagree on every negative half.
fn js_round(value: f64) -> f64 {
    (value + 0.5).floor()
}

fn js_sign(value: f64) -> f64 {
    if value.is_nan() || value == 0.0 {
        value
    } else if value > 0.0 {
        1.0
    } else {
        -1.0
    }
}

/// `Math.min`/`Math.max` propagate NaN, where Rust's `f64::min`/`max` discard
/// it in favour of the other operand.
fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a < b {
        a
    } else {
        b
    }
}

fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a > b {
        a
    } else {
        b
    }
}

/// `chyx.br` - reverse the low `size` bits; a size outside 1..=32 yields 0.
fn bit_reverse(value: f64, size: f64) -> f64 {
    let size = to_int32(size);
    if !(1..=32).contains(&size) {
        return 0.0;
    }
    let value = to_int32(value);
    let mut result: i32 = 0;
    for index in 0..size {
        if value & (1i32.wrapping_shl(index as u32)) != 0 {
            result |= 1i32.wrapping_shl((size - (index + 1)) as u32);
        }
    }
    f64::from(result)
}

fn unary(stack: &mut [f64; STACK_DEPTH], top: &mut usize, f: impl Fn(f64) -> f64) {
    if *top == 0 {
        return;
    }
    stack[*top - 1] = f(stack[*top - 1]);
}

/// Compile `source` into a program, or say what stopped it.
///
/// Runs on the producer side, never in the callback. Errors name the thing
/// they could not take, so an expression using something outside the subset is
/// REFUSED rather than silently replaced by a built-in.
pub fn compile(source: &str) -> Result<ByteBeatProgram, String> {
    let tokens = tokenize(source)?;
    if tokens.is_empty() {
        // A blank expression compiles to a constant 0.
        return ByteBeatProgram::from_steps(&[(ByteBeatOp::PushConst, 0.0)]);
    }
    let mut parser = Parser {
        tokens: &tokens,
        at: 0,
        out: Vec::new(),
    };
    parser.expression(0)?;
    if parser.at != parser.tokens.len() {
        return Err(format!(
            "bytebeat expression has trailing input at {:?}",
            parser.tokens[parser.at]
        ));
    }
    ByteBeatProgram::from_steps(&parser.out)
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(f64),
    Name(String),
    Symbol(&'static str),
}

const SYMBOLS: &[&str] = &[
    ">>>", "===", "!==", "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "+", "-", "*", "/", "%",
    "&", "|", "^", "~", "!", "<", ">", "(", ")", ",", "?", ":",
];

fn tokenize(source: &str) -> Result<Vec<Token>, String> {
    let bytes: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let mut at = 0usize;
    while at < bytes.len() {
        let c = bytes[at];
        if c.is_whitespace() {
            at += 1;
            continue;
        }
        if c.is_ascii_digit() || (c == '.' && bytes.get(at + 1).is_some_and(char::is_ascii_digit)) {
            let start = at;
            if c == '0' && matches!(bytes.get(at + 1), Some('x' | 'X')) {
                at += 2;
                while at < bytes.len() && bytes[at].is_ascii_hexdigit() {
                    at += 1;
                }
                let text: String = bytes[start + 2..at].iter().collect();
                let value = u32::from_str_radix(&text, 16)
                    .map_err(|_| format!("bytebeat literal 0x{text} is not a 32-bit number"))?;
                tokens.push(Token::Number(f64::from(value)));
                continue;
            }
            while at < bytes.len()
                && (bytes[at].is_ascii_digit()
                    || bytes[at] == '.'
                    || bytes[at] == 'e'
                    || bytes[at] == 'E'
                    || ((bytes[at] == '+' || bytes[at] == '-')
                        && matches!(bytes[at - 1], 'e' | 'E')))
            {
                at += 1;
            }
            let text: String = bytes[start..at].iter().collect();
            let value = text
                .parse::<f64>()
                .map_err(|_| format!("bytebeat literal {text} is not a number"))?;
            tokens.push(Token::Number(value));
            continue;
        }
        if c.is_alphabetic() || c == '_' || c == '$' {
            let start = at;
            while at < bytes.len()
                && (bytes[at].is_alphanumeric() || bytes[at] == '_' || bytes[at] == '$')
            {
                at += 1;
            }
            tokens.push(Token::Name(bytes[start..at].iter().collect()));
            continue;
        }
        let rest: String = bytes[at..].iter().collect();
        let Some(symbol) = SYMBOLS.iter().find(|symbol| rest.starts_with(**symbol)) else {
            return Err(format!("bytebeat expression cannot use {c:?}"));
        };
        tokens.push(Token::Symbol(symbol));
        at += symbol.chars().count();
    }
    Ok(tokens)
}

struct Parser<'a> {
    tokens: &'a [Token],
    at: usize,
    out: Vec<(ByteBeatOp, f64)>,
}

/// Binding power and operation for each infix symbol, in JavaScript's order.
fn infix(symbol: &str) -> Option<(u8, ByteBeatOp)> {
    Some(match symbol {
        "||" => (1, ByteBeatOp::LogicalOr),
        "&&" => (2, ByteBeatOp::LogicalAnd),
        "|" => (3, ByteBeatOp::BitOr),
        "^" => (4, ByteBeatOp::BitXor),
        "&" => (5, ByteBeatOp::BitAnd),
        // `===`/`!==` differ from `==`/`!=` only across types, and everything
        // here is a number.
        "==" | "===" => (6, ByteBeatOp::Equal),
        "!=" | "!==" => (6, ByteBeatOp::NotEqual),
        "<" => (7, ByteBeatOp::Less),
        ">" => (7, ByteBeatOp::Greater),
        "<=" => (7, ByteBeatOp::LessEqual),
        ">=" => (7, ByteBeatOp::GreaterEqual),
        "<<" => (8, ByteBeatOp::Shl),
        ">>" => (8, ByteBeatOp::Shr),
        ">>>" => (8, ByteBeatOp::UShr),
        "+" => (9, ByteBeatOp::Add),
        "-" => (9, ByteBeatOp::Sub),
        "*" => (10, ByteBeatOp::Mul),
        "/" => (10, ByteBeatOp::Div),
        "%" => (10, ByteBeatOp::Rem),
        _ => return None,
    })
}

/// `(op, arity)` for every name the subset binds as a function.
fn function(name: &str) -> Option<(ByteBeatOp, usize)> {
    Some(match name {
        "sin" => (ByteBeatOp::Sin, 1),
        "cos" => (ByteBeatOp::Cos, 1),
        "tan" => (ByteBeatOp::Tan, 1),
        "asin" => (ByteBeatOp::Asin, 1),
        "acos" => (ByteBeatOp::Acos, 1),
        "atan" => (ByteBeatOp::Atan, 1),
        "sqrt" => (ByteBeatOp::Sqrt, 1),
        "log" => (ByteBeatOp::Log, 1),
        "log2" => (ByteBeatOp::Log2, 1),
        "exp" => (ByteBeatOp::Exp, 1),
        // `int` is bound to `Math.floor`, not a truncating cast.
        "floor" | "int" => (ByteBeatOp::Floor, 1),
        "ceil" => (ByteBeatOp::Ceil, 1),
        "round" => (ByteBeatOp::Round, 1),
        "trunc" => (ByteBeatOp::Trunc, 1),
        "abs" => (ByteBeatOp::Abs, 1),
        "sign" => (ByteBeatOp::Sign, 1),
        "sinf" => (ByteBeatOp::SinF, 1),
        "cosf" => (ByteBeatOp::CosF, 1),
        "tanf" => (ByteBeatOp::TanF, 1),
        "min" => (ByteBeatOp::Min, 2),
        "max" => (ByteBeatOp::Max, 2),
        "pow" => (ByteBeatOp::Pow, 2),
        "atan2" => (ByteBeatOp::Atan2, 2),
        // `br(x, size = 8)`, so the second argument is optional.
        "br" => (ByteBeatOp::BitReverse, 2),
        "bitC" => (ByteBeatOp::BitC, 3),
        _ => return None,
    })
}

fn constant(name: &str) -> Option<f64> {
    Some(match name {
        "PI" => std::f64::consts::PI,
        "E" => std::f64::consts::E,
        "LN2" => std::f64::consts::LN_2,
        "LN10" => std::f64::consts::LN_10,
        "LOG2E" => std::f64::consts::LOG2_E,
        "LOG10E" => std::f64::consts::LOG10_E,
        "SQRT2" => std::f64::consts::SQRT_2,
        "SQRT1_2" => std::f64::consts::FRAC_1_SQRT_2,
        _ => return None,
    })
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn eat(&mut self, symbol: &str) -> bool {
        if matches!(self.peek(), Some(Token::Symbol(s)) if *s == symbol) {
            self.at += 1;
            return true;
        }
        false
    }

    fn emit(&mut self, op: ByteBeatOp, constant: f64) {
        self.out.push((op, constant));
    }

    /// Precedence climbing, plus the conditional at the bottom.
    fn expression(&mut self, min_power: u8) -> Result<(), String> {
        self.unary()?;
        while let Some(&Token::Symbol(symbol)) = self.peek() {
            if symbol == "?" && min_power == 0 {
                self.at += 1;
                self.expression(0)?;
                if !self.eat(":") {
                    return Err("bytebeat conditional is missing its ':'".to_owned());
                }
                // Right associative, so the else-branch takes the whole tail.
                self.expression(0)?;
                self.emit(ByteBeatOp::Select, 0.0);
                continue;
            }
            let Some((power, op)) = infix(symbol) else {
                break;
            };
            if power < min_power {
                break;
            }
            self.at += 1;
            self.expression(power + 1)?;
            self.emit(op, 0.0);
        }
        Ok(())
    }

    fn unary(&mut self) -> Result<(), String> {
        if let Some(Token::Symbol(symbol)) = self.peek() {
            let op = match *symbol {
                "-" => Some(ByteBeatOp::Neg),
                "~" => Some(ByteBeatOp::BitNot),
                "!" => Some(ByteBeatOp::LogicalNot),
                // Unary plus is a no-op on numbers.
                "+" => Some(ByteBeatOp::PushT),
                _ => None,
            };
            if let Some(op) = op {
                let plus = *symbol == "+";
                self.at += 1;
                self.unary()?;
                if !plus {
                    self.emit(op, 0.0);
                }
                return Ok(());
            }
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<(), String> {
        match self.peek().cloned() {
            Some(Token::Number(value)) => {
                self.at += 1;
                self.emit(ByteBeatOp::PushConst, value);
                Ok(())
            }
            Some(Token::Name(name)) => {
                self.at += 1;
                if self.eat("(") {
                    let Some((op, arity)) = function(&name) else {
                        return Err(format!(
                            "bytebeat expression calls {name}(), which this engine does not provide"
                        ));
                    };
                    let mut given = 0usize;
                    if !self.eat(")") {
                        loop {
                            self.expression(0)?;
                            given += 1;
                            if self.eat(",") {
                                continue;
                            }
                            if self.eat(")") {
                                break;
                            }
                            return Err(format!("bytebeat call to {name}() is missing its ')'"));
                        }
                    }
                    // `br`'s size defaults to 8; nothing else is optional.
                    if given + 1 == arity && op == ByteBeatOp::BitReverse {
                        self.emit(ByteBeatOp::PushConst, 8.0);
                        given += 1;
                    }
                    if given != arity {
                        return Err(format!(
                            "bytebeat call to {name}() takes {arity} arguments, got {given}"
                        ));
                    }
                    self.emit(op, 0.0);
                    return Ok(());
                }
                if name == "t" {
                    self.emit(ByteBeatOp::PushT, 0.0);
                    return Ok(());
                }
                if let Some(value) = constant(&name) {
                    self.emit(ByteBeatOp::PushConst, value);
                    return Ok(());
                }
                Err(format!(
                    "bytebeat expression uses {name}, which this engine does not provide"
                ))
            }
            Some(Token::Symbol("(")) => {
                self.at += 1;
                self.expression(0)?;
                if !self.eat(")") {
                    return Err("bytebeat expression is missing a ')'".to_owned());
                }
                Ok(())
            }
            other => Err(format!("bytebeat expression stops early at {other:?}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::compile;

    fn eval(source: &str, t: f64) -> f64 {
        compile(source).expect("compiles").eval(t)
    }

    /// The low byte is what reaches the speaker, so expectations are checked
    /// through the same `& 255` the render path applies.
    fn byte(source: &str, t: f64) -> u8 {
        crate::backend::bytebeat_byte(eval(source, t))
    }

    #[test]
    fn the_documented_expression_compiles_and_follows_js_integer_semantics() {
        // `t>>66` shifts by 66 % 32 = 2 - the shift count wraps, the value
        // does not saturate. This is the canonical documented example.
        let src = "t*(t>>15^t>>66)";
        for t in [0.0, 1.0, 255.0, 4096.0, 100_000.0, 8_388_607.0] {
            let t32 = t as i64 as i32;
            // The multiply happens in f64; only the bitwise steps coerce.
            // Compare through the byte the speaker hears.
            let expected = crate::backend::bytebeat_byte(t * f64::from((t32 >> 15) ^ (t32 >> 2)));
            assert_eq!(byte(src, t), expected, "at t={t}");
        }
    }

    #[test]
    fn operators_keep_javascripts_meaning_not_rusts() {
        // && yields an operand, not a boolean.
        assert_eq!(eval("5 && 7", 0.0), 7.0);
        assert_eq!(eval("0 && 7", 0.0), 0.0);
        assert_eq!(eval("5 || 7", 0.0), 5.0);
        assert_eq!(eval("0 || 7", 0.0), 7.0);
        // % is a remainder that follows the dividend's sign, as both JS and
        // Rust f64 do; and division does not truncate.
        assert_eq!(eval("-7 % 3", 0.0), -1.0);
        assert_eq!(eval("7 / 2", 0.0), 3.5);
        // >>> is unsigned: -1 >>> 28 is 15.
        assert_eq!(eval("0-1 >>> 28", 0.0), 15.0);
        // ~ coerces through ToInt32.
        assert_eq!(eval("~5.9", 0.0), -6.0);
        // The conditional is right-associative and lazy about nothing - all
        // three arms are numbers here.
        assert_eq!(eval("1 ? 2 : 0 ? 3 : 4", 0.0), 2.0);
        assert_eq!(eval("0 ? 2 : 0 ? 3 : 4", 0.0), 4.0);
        // Precedence: | binds looser than ^, which is looser than &.
        assert_eq!(eval("1 | 2 ^ 2 & 3", 0.0), 1.0);
        // Hex literals.
        assert_eq!(eval("0xFF & 0x0F", 0.0), 15.0);
    }

    #[test]
    fn the_math_and_chyx_names_are_bound_like_the_worklets() {
        assert!((eval("sin(PI/2)", 0.0) - 1.0).abs() < 1e-12);
        // `int` is Math.floor, not a cast toward zero.
        assert_eq!(eval("int(0-1.5)", 0.0), -2.0);
        assert_eq!(eval("min(3, t)", 5.0), 3.0);
        assert_eq!(eval("pow(2, 10)", 0.0), 1024.0);
        // chyx: sinf loops every 128 steps.
        assert!((eval("sinf(64)", 0.0) - 1.0).abs() < 1e-12);
        // bitC(x, y, z) = x & y ? z : 0.
        assert_eq!(eval("bitC(6, 2, 9)", 0.0), 9.0);
        assert_eq!(eval("bitC(4, 2, 9)", 0.0), 0.0);
        // br reverses the low 8 bits by default.
        assert_eq!(eval("br(1)", 0.0), 128.0);
        assert_eq!(eval("br(1, 4)", 0.0), 8.0);
    }

    #[test]
    fn what_the_subset_cannot_take_is_refused_by_name_not_silently_dropped() {
        // The refusal is the point: accepting the control, dropping it, and
        // playing built-in 0 once scored corr -0.000120 on the documented
        // example.
        let err = compile("myFunc(t)").expect_err("unknown call");
        assert!(
            err.contains("myFunc"),
            "the error must name the function: {err}"
        );
        let err = compile("t * q").expect_err("unknown name");
        assert!(err.contains('q'), "the error must name the variable: {err}");
        assert!(
            compile("t +").is_err(),
            "a truncated expression cannot compile"
        );
        assert!(compile("(t").is_err(), "an unclosed paren cannot compile");
        assert!(
            compile("t ? 1").is_err(),
            "a conditional missing ':' cannot compile"
        );
        // A blank expression evaluates to 0.
        assert_eq!(eval("", 123.0), 0.0);
    }

    #[test]
    fn a_program_survives_the_flat_round_trip() {
        let program = compile("t*(t>>15^t>>66)").expect("compiles");
        let steps: Vec<_> = (0..program.len())
            .map(|index| program.step(index))
            .collect();
        let back = super::ByteBeatProgram::from_steps(&steps).expect("rebuilds");
        assert_eq!(program, back);
        for op in 0..=u8::MAX {
            if let Some(decoded) = super::ByteBeatOp::from_u8(op) {
                assert_eq!(decoded as u8, op, "from_u8 must invert the cast");
            }
        }
    }
}
