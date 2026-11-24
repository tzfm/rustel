//! The shape of a score that a host evaluates without JavaScript.
//!
//! A program is made only of the shapes below. What a name means is the
//! host's to decide.
//!
//! A program borrows each name and each string that the text of its score
//! holds as it is, so a reader of that text copies none of them.
//!
//! The expressions that another expression holds are in one list of the
//! program: a receiver, the arguments of a call, the elements of an array.
//! A reader so makes no allocation for each call, and a program drops as
//! one list.

use std::borrow::Cow;

/// Where the text of a score has a call or a statement, and what the
/// reader held where that text starts. A host that built the same text at
/// the same place before, under the same reading, can keep what it built.
///
/// Two programs say the same wherever they were written, so no `Written`
/// is part of what makes them equal. A program that was not read from the
/// text of a score has `Written::default()`, which names no text.
#[derive(Clone, Copy, Debug, Default)]
pub struct Written {
    /// The first byte of the text.
    pub start: u32,
    /// How many bytes the text has. Zero for no text.
    pub len: u32,
    /// The number of the first widget with a slot in the text, or
    /// `u32::MAX` for a text with none: each widget of a score has the
    /// number of its place among them.
    pub slots: u32,
    /// Whether a double-quoted string is mini-notation where the text
    /// starts.
    pub mini: bool,
}

impl Written {
    /// The text in `source`, the text of the score. `None` for no text,
    /// and for a text that `source` does not hold.
    pub fn text<'s>(&self, source: &'s str) -> Option<&'s str> {
        let start = usize::try_from(self.start).ok()?;
        let len = usize::try_from(self.len).ok().filter(|len| *len > 0)?;
        source.get(start..start.checked_add(len)?)
    }
}

/// The place of one expression in the [`Held`] list of a program: a
/// receiver, or the object of a member.
#[derive(Clone, Copy, Debug)]
pub struct At(u32);

/// The place of one arrow function in the [`Held`] list of a program.
#[derive(Clone, Copy, Debug)]
pub struct FuncAt(u32);

/// The places of expressions in a row in the [`Held`] list of a program:
/// the arguments of a call, or the elements of an array.
#[derive(Clone, Copy, Debug, Default)]
pub struct Row {
    start: u32,
    len: u32,
}

impl Row {
    /// How many expressions the row has.
    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// The places of object properties in a row in the [`Held`] list of a program.
#[derive(Clone, Copy, Debug, Default)]
pub struct PropRow {
    start: u32,
    len: u32,
}

impl PropRow {
    /// How many properties the row has.
    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Compact 8-byte `'static` string handle inside [`ScoreExpr`] and
/// [`ScoreProperty`]: either a byte span `(start, len)` into [`Held::source`],
/// the built-in lane method `"p"`, or an index into [`Held::strings`] for
/// decoded escape sequences and synthetic strings.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ScoreStr {
    start: u32,
    len: u32,
}

impl ScoreStr {
    pub const EMPTY: Self = Self { start: 0, len: 0 };
    const OWNED_BIT: u32 = 1 << 31;
    pub const LANE_METHOD: Self = Self {
        start: 0,
        len: u32::MAX,
    };

    #[inline(always)]
    pub const fn span(start: u32, len: u32) -> Self {
        Self { start, len }
    }

    #[inline(always)]
    const fn owned(idx: u32) -> Self {
        Self {
            start: idx,
            len: Self::OWNED_BIT,
        }
    }
}

impl std::fmt::Debug for ScoreStr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.len == 0 {
            write!(f, "\"\"")
        } else if self.len == Self::LANE_METHOD.len {
            write!(f, "\"p\"")
        } else if (self.len & Self::OWNED_BIT) != 0 {
            write!(f, "Owned({})", self.start)
        } else {
            write!(f, "Span({}..{})", self.start, self.start + self.len)
        }
    }
}

#[derive(Default)]
struct SpareHeld {
    exprs: Vec<ScoreExpr>,
    functions: Vec<ScoreFunction>,
    properties: Vec<ScoreProperty>,
    strings: Vec<String>,
}

thread_local! {
    static SPARE_HELD: std::cell::RefCell<Option<SpareHeld>> =
        const { std::cell::RefCell::new(None) };
    static SPARE_STATEMENTS: std::cell::RefCell<Option<Vec<ScoreStatement>>> =
        const { std::cell::RefCell::new(None) };
}

/// The expressions and arrow functions that the expressions of a program hold,
/// stored contiguously to avoid per-node heap allocations.
#[derive(Clone, Debug, Default)]
pub struct Held<'s> {
    source: &'s str,
    exprs: Vec<ScoreExpr>,
    functions: Vec<ScoreFunction>,
    properties: Vec<ScoreProperty>,
    strings: Vec<String>,
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        if self.exprs.capacity() == 0
            && self.functions.capacity() == 0
            && self.properties.capacity() == 0
            && self.strings.capacity() == 0
        {
            return;
        }
        if self.exprs.capacity() <= 8192
            && self.functions.capacity() <= 256
            && self.properties.capacity() <= 1024
            && self.strings.capacity() <= 512
        {
            self.exprs.clear();
            self.functions.clear();
            self.properties.clear();
            self.strings.clear();
            let spare = SpareHeld {
                exprs: std::mem::take(&mut self.exprs),
                functions: std::mem::take(&mut self.functions),
                properties: std::mem::take(&mut self.properties),
                strings: std::mem::take(&mut self.strings),
            };
            let _ = SPARE_HELD.try_with(|cell| {
                if let Ok(mut slot) = cell.try_borrow_mut() {
                    *slot = Some(spare);
                }
            });
        }
    }
}

impl<'s> Held<'s> {
    /// A list backed by `source` with room for `count` expressions, reusing
    /// cleared thread-local buffers when available.
    pub fn with_source(source: &'s str, count: usize) -> Self {
        let mut spare = SPARE_HELD
            .try_with(|cell| cell.try_borrow_mut().ok()?.take())
            .ok()
            .flatten()
            .unwrap_or_default();
        spare.exprs.clear();
        if spare.exprs.capacity() < count {
            spare.exprs.reserve(count);
        }
        spare.functions.clear();
        spare.properties.clear();
        spare.strings.clear();
        Self {
            source,
            exprs: spare.exprs,
            functions: spare.functions,
            properties: spare.properties,
            strings: spare.strings,
        }
    }

    /// A list with room for `count` expressions.
    pub fn with_room(count: usize) -> Self {
        Self::with_source("", count)
    }

    /// Hold `expr`. `None` past the count that a place can say.
    #[inline]
    pub fn hold(&mut self, expr: ScoreExpr) -> Option<At> {
        let at = u32::try_from(self.exprs.len()).ok()?;
        self.exprs.push(expr);
        Some(At(at))
    }

    /// Hold `function`. `None` past the count that a place can say.
    #[inline]
    pub fn hold_function(&mut self, function: ScoreFunction) -> Option<FuncAt> {
        let at = u32::try_from(self.functions.len()).ok()?;
        self.functions.push(function);
        Some(FuncAt(at))
    }

    /// Hold `expr` alone as a row, which a call with one argument and an
    /// array with one element need. `None` past the count that a place
    /// can say.
    #[inline]
    pub fn hold_one(&mut self, expr: ScoreExpr) -> Option<Row> {
        let At(at) = self.hold(expr)?;
        Some(Row { start: at, len: 1 })
    }

    /// Hold `exprs` in a row, in their order. `None` past the count that a
    /// place can say.
    #[inline]
    pub fn hold_row(&mut self, exprs: impl IntoIterator<Item = ScoreExpr>) -> Option<Row> {
        let start = self.exprs.len();
        self.exprs.extend(exprs);
        Some(Row {
            start: u32::try_from(start).ok()?,
            len: u32::try_from(self.exprs.len() - start).ok()?,
        })
    }

    /// Hold `properties` in a row, in their order. `None` past the count that a
    /// place can say.
    #[inline]
    pub fn hold_props(
        &mut self,
        properties: impl IntoIterator<Item = ScoreProperty>,
    ) -> Option<PropRow> {
        let start = self.properties.len();
        self.properties.extend(properties);
        Some(PropRow {
            start: u32::try_from(start).ok()?,
            len: u32::try_from(self.properties.len() - start).ok()?,
        })
    }

    /// Encode `s` as a zero-allocation span into [`Self::source`] when it
    /// borrows a subslice of `source`, otherwise store an owned copy.
    #[inline]
    pub fn hold_str(&mut self, s: &str) -> Option<ScoreStr> {
        if s.is_empty() {
            return Some(ScoreStr::EMPTY);
        }
        let base = self.source.as_ptr() as usize;
        let ptr = s.as_ptr() as usize;
        if ptr >= base {
            let offset = ptr - base;
            if let Some(end) = offset.checked_add(s.len())
                && end <= self.source.len()
                && s.len() < (ScoreStr::OWNED_BIT as usize)
                && let Ok(start) = u32::try_from(offset)
            {
                return Some(ScoreStr::span(start, s.len() as u32));
            }
        }
        self.hold_string(s.to_owned())
    }

    /// Store an owned or borrowed string, returning a `Copy` handle.
    #[inline]
    pub fn hold_cow(&mut self, text: Cow<'s, str>) -> Option<ScoreStr> {
        match text {
            Cow::Borrowed(s) => self.hold_str(s),
            Cow::Owned(s) => self.hold_string(s),
        }
    }

    /// Store an owned string, returning a `Copy` handle.
    #[inline]
    pub fn hold_string(&mut self, text: String) -> Option<ScoreStr> {
        if text.is_empty() {
            return Some(ScoreStr::EMPTY);
        }
        let idx = u32::try_from(self.strings.len()).ok()?;
        self.strings.push(text);
        Some(ScoreStr::owned(idx))
    }

    /// Resolve a [`ScoreStr`] handle to its string slice.
    #[inline(always)]
    pub fn text(&self, text: ScoreStr) -> &str {
        if text.len == ScoreStr::LANE_METHOD.len {
            "p"
        } else if (text.len & ScoreStr::OWNED_BIT) != 0 {
            &self.strings[text.start as usize]
        } else {
            let start = text.start as usize;
            &self.source[start..start + text.len as usize]
        }
    }

    /// The expression at `at`.
    #[inline(always)]
    pub fn at(&self, at: At) -> &ScoreExpr {
        &self.exprs[at.0 as usize]
    }

    /// The arrow function at `at`.
    #[inline(always)]
    pub fn function(&self, at: FuncAt) -> &ScoreFunction {
        &self.functions[at.0 as usize]
    }

    /// The expressions of `row`.
    #[inline(always)]
    pub fn row(&self, row: Row) -> &[ScoreExpr] {
        &self.exprs[row.start as usize..][..row.len as usize]
    }

    /// The expressions of `row`, to write.
    #[inline(always)]
    pub fn row_mut(&mut self, row: Row) -> &mut [ScoreExpr] {
        &mut self.exprs[row.start as usize..][..row.len as usize]
    }

    /// The object properties of `row`.
    #[inline(always)]
    pub fn props(&self, row: PropRow) -> &[ScoreProperty] {
        &self.properties[row.start as usize..][..row.len as usize]
    }
}

/// A score made only of bindings, functions and expression statements.
#[derive(Clone, Debug, Default)]
pub struct ScoreProgram<'s> {
    pub statements: Vec<ScoreStatement>,
    /// Each expression that another one holds. See [`Held`].
    pub held: Held<'s>,
}

impl ScoreProgram<'_> {
    pub(crate) fn take_statements(min_cap: usize) -> Vec<ScoreStatement> {
        let mut stmts = SPARE_STATEMENTS
            .try_with(|cell| cell.try_borrow_mut().ok()?.take())
            .ok()
            .flatten()
            .unwrap_or_default();
        stmts.clear();
        if stmts.capacity() < min_cap {
            stmts.reserve(min_cap);
        }
        stmts
    }
}

impl Drop for ScoreProgram<'_> {
    fn drop(&mut self) {
        if self.statements.capacity() > 0 && self.statements.capacity() <= 512 {
            self.statements.clear();
            let stmts = std::mem::take(&mut self.statements);
            let _ = SPARE_STATEMENTS.try_with(|cell| {
                if let Ok(mut slot) = cell.try_borrow_mut() {
                    *slot = Some(stmts);
                }
            });
        }
    }
}

/// Two programs are equal when they say the same, wherever each one holds
/// its expressions.
impl PartialEq for ScoreProgram<'_> {
    fn eq(&self, other: &Self) -> bool {
        let same = Same(&self.held, &other.held);
        self.statements.len() == other.statements.len()
            && self
                .statements
                .iter()
                .zip(&other.statements)
                .all(|(a, b)| same.statements(a, b))
    }
}

/// The comparison of two programs: the held lists of the one and of the
/// other.
struct Same<'a, 's, 't>(&'a Held<'s>, &'a Held<'t>);

impl Same<'_, '_, '_> {
    fn statements(&self, a: &ScoreStatement, b: &ScoreStatement) -> bool {
        use ScoreStatement::{Binding, Expression, Function};
        match (a, b) {
            (
                Binding { name, value, .. },
                Binding {
                    name: other_name,
                    value: other_value,
                    ..
                },
            ) => self.0.text(*name) == self.1.text(*other_name) && self.exprs(value, other_value),
            (Expression(a), Expression(b)) => self.exprs(a, b),
            (
                Function { name, function, .. },
                Function {
                    name: other_name,
                    function: other_function,
                    ..
                },
            ) => {
                self.0.text(*name) == self.1.text(*other_name)
                    && self.functions(function, other_function)
            }
            _ => false,
        }
    }

    fn functions(&self, a: &ScoreFunction, b: &ScoreFunction) -> bool {
        a.parameters.len() == b.parameters.len()
            && a.parameters
                .iter()
                .zip(b.parameters.iter())
                .all(|(x, y)| self.0.text(*x) == self.1.text(*y))
            && a.bindings.len() == b.bindings.len()
            && a.bindings.iter().zip(&b.bindings).all(
                |((name, value), (other_name, other_value))| {
                    self.0.text(*name) == self.1.text(*other_name) && self.exprs(value, other_value)
                },
            )
            && self.exprs(&a.result, &b.result)
    }

    fn rows(&self, a: Row, b: Row) -> bool {
        let (a, b) = (self.0.row(a), self.1.row(b));
        a.len() == b.len() && a.iter().zip(b).all(|(a, b)| self.exprs(a, b))
    }

    fn exprs(&self, a: &ScoreExpr, b: &ScoreExpr) -> bool {
        use ScoreExpr::{Array, Arrow, Call, Member, Method, Mini, Name, Number, Object, String};
        match (a, b) {
            (Number(a), Number(b)) => a == b,
            (String(a), String(b)) | (Name(a), Name(b)) => self.0.text(*a) == self.1.text(*b),
            (
                Mini { source, offset },
                Mini {
                    source: other_source,
                    offset: other_offset,
                },
            ) => offset == other_offset && self.0.text(*source) == self.1.text(*other_source),
            (Object(a), Object(b)) => {
                let (a, b) = (self.0.props(*a), self.1.props(*b));
                a.len() == b.len()
                    && a.iter().zip(b).all(|(a, b)| match (a, b) {
                        (
                            ScoreProperty::Entry { key, value },
                            ScoreProperty::Entry {
                                key: other_key,
                                value: other_value,
                            },
                        ) => {
                            self.0.text(*key) == self.1.text(*other_key)
                                && self.exprs(value, other_value)
                        }
                        (ScoreProperty::Spread(a), ScoreProperty::Spread(b)) => self.exprs(a, b),
                        _ => false,
                    })
            }
            (Array(a), Array(b)) => self.rows(*a, *b),
            (
                Member { object, property },
                Member {
                    object: other_object,
                    property: other_property,
                },
            ) => {
                self.0.text(*property) == self.1.text(*other_property)
                    && self.exprs(self.0.at(*object), self.1.at(*other_object))
            }
            (
                Call {
                    callee, arguments, ..
                },
                Call {
                    callee: other_callee,
                    arguments: other_arguments,
                    ..
                },
            ) => {
                self.0.text(*callee) == self.1.text(*other_callee)
                    && self.rows(*arguments, *other_arguments)
            }
            (
                Method {
                    receiver,
                    method,
                    arguments,
                    ..
                },
                Method {
                    receiver: other_receiver,
                    method: other_method,
                    arguments: other_arguments,
                    ..
                },
            ) => {
                self.0.text(*method) == self.1.text(*other_method)
                    && self.exprs(self.0.at(*receiver), self.1.at(*other_receiver))
                    && self.rows(*arguments, *other_arguments)
            }
            (Arrow(a), Arrow(b)) => self.functions(self.0.function(*a), self.1.function(*b)),
            _ => false,
        }
    }
}

/// One top-level statement.
#[derive(Clone, Debug)]
pub enum ScoreStatement {
    /// `const name = value`, or `let` with no later assignment.
    Binding {
        name: ScoreStr,
        value: ScoreExpr,
        written: Written,
    },
    /// An expression evaluated for its effects and, last, for its value.
    Expression(ScoreExpr),
    /// `function name(...) {...}`, which JavaScript hoists.
    Function {
        name: ScoreStr,
        function: ScoreFunction,
        written: Written,
    },
}

/// Inline-optimized parameter list for [`ScoreFunction`], avoiding a heap
/// `Vec` allocation for zero- and one-parameter arrows (`x => ...`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ScoreParams {
    #[default]
    Empty,
    One(ScoreStr),
    Many(Vec<ScoreStr>),
}

impl ScoreParams {
    #[inline]
    pub fn as_slice(&self) -> &[ScoreStr] {
        match self {
            Self::Empty => &[],
            Self::One(one) => std::slice::from_ref(one),
            Self::Many(many) => many.as_slice(),
        }
    }

    pub fn from_strs(names: &[ScoreStr]) -> Self {
        match names {
            [] => Self::Empty,
            [one] => Self::One(*one),
            many => Self::Many(many.to_vec()),
        }
    }
}

impl std::ops::Deref for ScoreParams {
    type Target = [ScoreStr];
    #[inline]
    fn deref(&self) -> &[ScoreStr] {
        self.as_slice()
    }
}

impl From<Vec<ScoreStr>> for ScoreParams {
    fn from(mut vec: Vec<ScoreStr>) -> Self {
        match vec.len() {
            0 => Self::Empty,
            1 => Self::One(vec.pop().expect("len 1")),
            _ => Self::Many(vec),
        }
    }
}

/// What a function declaration or an arrow holds: plain parameters,
/// bindings, then one result. `(a) => result` has no bindings.
#[derive(Clone, Debug)]
pub struct ScoreFunction {
    pub parameters: ScoreParams,
    pub bindings: Vec<(ScoreStr, ScoreExpr)>,
    pub result: ScoreExpr,
}

/// One part of an object literal.
#[derive(Clone, Copy, Debug)]
pub enum ScoreProperty {
    /// `key: value`, or `key` alone for a name of that spelling.
    Entry { key: ScoreStr, value: ScoreExpr },
    /// `...source`: every property of another object.
    Spread(ScoreExpr),
}

/// One expression of a [`ScoreProgram`].
#[derive(Clone, Copy, Debug)]
pub enum ScoreExpr {
    /// A number literal, or number arithmetic folded as JavaScript does.
    Number(f64),
    /// A string literal's value.
    String(ScoreStr),
    /// A mini-notation literal and its opening byte offset in the score, stored
    /// inline without allocating argument slots in [`Held`].
    Mini { source: ScoreStr, offset: u32 },
    /// A name: a binding of the score or of the host.
    Name(ScoreStr),
    /// `{ key: value, ...source }`: plain keys and spreads, in source order.
    Object(PropRow),
    /// `[first, second]`: an array with no hole and no spread.
    Array(Row),
    /// `object.property`, read and not called.
    Member { object: At, property: ScoreStr },
    /// `callee(...arguments)`.
    Call {
        callee: ScoreStr,
        arguments: Row,
        written: Written,
    },
    /// `receiver.method(...arguments)`.
    Method {
        receiver: At,
        method: ScoreStr,
        arguments: Row,
        written: Written,
    },
    /// `(...parameters) => result`, or the same with a block of bindings
    /// and one `return`.
    Arrow(FuncAt),
}

const _: () = assert!(!std::mem::needs_drop::<ScoreExpr>());
const _: () = assert!(!std::mem::needs_drop::<ScoreProperty>());
#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<ScoreExpr>() <= 40);

#[cfg(test)]
mod tests {
    use crate::{CallForm, read};

    fn program(text: &str) -> crate::ScoreProgram<'_> {
        read(text, |_| CallForm::Written).expect("a score").program
    }

    #[test]
    fn programs_are_equal_when_they_say_the_same() {
        let text = "const a = [s(bd).fast(2), { b: x.y, ...c }]\na.rev(n => n.late(1))";
        // The same words at other places, which hold their expressions at
        // other places.
        let spaced = "const a = [ s(bd)\n  .fast( 2 ), {b: x.y, ...c} ]\n\na.rev(n=>n.late(1))";
        assert_eq!(program(text), program(spaced));
        for other in [
            // An argument, a receiver, a method, a callee.
            "const a = [s(bd).fast(3), { b: x.y, ...c }]\na.rev(n => n.late(1))",
            "const a = [s(bd).fast(2), { b: x.y, ...c }]\nb.rev(n => n.late(1))",
            "const a = [s(bd).slow(2), { b: x.y, ...c }]\na.rev(n => n.late(1))",
            "const a = [n(bd).fast(2), { b: x.y, ...c }]\na.rev(n => n.late(1))",
            // An element, the object of a member, a key, a spread.
            "const a = [s(bd).fast(2)]\na.rev(n => n.late(1))",
            "const a = [s(bd).fast(2), { b: z.y, ...c }]\na.rev(n => n.late(1))",
            "const a = [s(bd).fast(2), { d: x.y, ...c }]\na.rev(n => n.late(1))",
            "const a = [s(bd).fast(2), { b: x.y, ...d }]\na.rev(n => n.late(1))",
            // The body of an arrow, a name, a statement.
            "const a = [s(bd).fast(2), { b: x.y, ...c }]\na.rev(n => n.late(2))",
            "const e = [s(bd).fast(2), { b: x.y, ...c }]\na.rev(n => n.late(1))",
            "const a = [s(bd).fast(2), { b: x.y, ...c }]",
        ] {
            assert_ne!(program(text), program(other), "{other}");
        }
    }
}
