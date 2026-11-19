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

/// The expressions that the expressions of a program hold, in one list.
#[derive(Clone, Debug, Default)]
pub struct Held<'s>(Vec<ScoreExpr<'s>>);

impl<'s> Held<'s> {
    /// A list with room for `count` expressions.
    pub fn with_room(count: usize) -> Self {
        Self(Vec::with_capacity(count))
    }

    /// Hold `expr`. `None` past the count that a place can say.
    #[inline]
    pub fn hold(&mut self, expr: ScoreExpr<'s>) -> Option<At> {
        let at = u32::try_from(self.0.len()).ok()?;
        self.0.push(expr);
        Some(At(at))
    }

    /// Hold `expr` alone as a row, which a call with one argument and an
    /// array with one element need. `None` past the count that a place
    /// can say.
    #[inline]
    pub fn hold_one(&mut self, expr: ScoreExpr<'s>) -> Option<Row> {
        let At(at) = self.hold(expr)?;
        Some(Row { start: at, len: 1 })
    }

    /// Hold `exprs` in a row, in their order. `None` past the count that a
    /// place can say.
    #[inline]
    pub fn hold_row(&mut self, exprs: impl IntoIterator<Item = ScoreExpr<'s>>) -> Option<Row> {
        let start = self.0.len();
        self.0.extend(exprs);
        Some(Row {
            start: u32::try_from(start).ok()?,
            len: u32::try_from(self.0.len() - start).ok()?,
        })
    }

    /// The expression at `at`.
    pub fn at(&self, at: At) -> &ScoreExpr<'s> {
        &self.0[at.0 as usize]
    }

    /// The expressions of `row`.
    pub fn row(&self, row: Row) -> &[ScoreExpr<'s>] {
        &self.0[row.start as usize..][..row.len as usize]
    }

    /// The expressions of `row`, to write.
    pub fn row_mut(&mut self, row: Row) -> &mut [ScoreExpr<'s>] {
        &mut self.0[row.start as usize..][..row.len as usize]
    }
}

/// A score made only of bindings, functions and expression statements.
#[derive(Clone, Debug, Default)]
pub struct ScoreProgram<'s> {
    pub statements: Vec<ScoreStatement<'s>>,
    /// Each expression that another one holds. See [`Held`].
    pub held: Held<'s>,
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
    fn statements(&self, a: &ScoreStatement<'_>, b: &ScoreStatement<'_>) -> bool {
        use ScoreStatement::{Binding, Expression, Function};
        match (a, b) {
            (
                Binding { name, value, .. },
                Binding {
                    name: other_name,
                    value: other_value,
                    ..
                },
            ) => name == other_name && self.exprs(value, other_value),
            (Expression(a), Expression(b)) => self.exprs(a, b),
            (
                Function { name, function, .. },
                Function {
                    name: other_name,
                    function: other_function,
                    ..
                },
            ) => name == other_name && self.functions(function, other_function),
            _ => false,
        }
    }

    fn functions(&self, a: &ScoreFunction<'_>, b: &ScoreFunction<'_>) -> bool {
        a.parameters == b.parameters
            && a.bindings.len() == b.bindings.len()
            && a.bindings.iter().zip(&b.bindings).all(
                |((name, value), (other_name, other_value))| {
                    name == other_name && self.exprs(value, other_value)
                },
            )
            && self.exprs(&a.result, &b.result)
    }

    fn rows(&self, a: Row, b: Row) -> bool {
        let (a, b) = (self.0.row(a), self.1.row(b));
        a.len() == b.len() && a.iter().zip(b).all(|(a, b)| self.exprs(a, b))
    }

    fn exprs(&self, a: &ScoreExpr<'_>, b: &ScoreExpr<'_>) -> bool {
        use ScoreExpr::{Array, Arrow, Call, Member, Method, Name, Number, Object, String};
        match (a, b) {
            (Number(a), Number(b)) => a == b,
            (String(a), String(b)) | (Name(a), Name(b)) => a == b,
            (Object(a), Object(b)) => {
                a.len() == b.len()
                    && a.iter().zip(b).all(|(a, b)| match (a, b) {
                        (
                            ScoreProperty::Entry { key, value },
                            ScoreProperty::Entry {
                                key: other_key,
                                value: other_value,
                            },
                        ) => key == other_key && self.exprs(value, other_value),
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
                property == other_property
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
            ) => callee == other_callee && self.rows(*arguments, *other_arguments),
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
                method == other_method
                    && self.exprs(self.0.at(*receiver), self.1.at(*other_receiver))
                    && self.rows(*arguments, *other_arguments)
            }
            (Arrow(a), Arrow(b)) => self.functions(a, b),
            _ => false,
        }
    }
}

/// One top-level statement.
#[derive(Clone, Debug)]
pub enum ScoreStatement<'s> {
    /// `const name = value`, or `let` with no later assignment.
    Binding {
        name: Cow<'s, str>,
        value: ScoreExpr<'s>,
        written: Written,
    },
    /// An expression evaluated for its effects and, last, for its value.
    Expression(ScoreExpr<'s>),
    /// `function name(...) {...}`, which JavaScript hoists.
    Function {
        name: Cow<'s, str>,
        function: ScoreFunction<'s>,
        written: Written,
    },
}

/// What a function declaration or an arrow holds: plain parameters,
/// bindings, then one result. `(a) => result` has no bindings.
#[derive(Clone, Debug)]
pub struct ScoreFunction<'s> {
    pub parameters: Vec<Cow<'s, str>>,
    pub bindings: Vec<(Cow<'s, str>, ScoreExpr<'s>)>,
    pub result: ScoreExpr<'s>,
}

/// One part of an object literal.
#[derive(Clone, Debug)]
pub enum ScoreProperty<'s> {
    /// `key: value`, or `key` alone for a name of that spelling.
    Entry {
        key: Cow<'s, str>,
        value: ScoreExpr<'s>,
    },
    /// `...source`: every property of another object.
    Spread(ScoreExpr<'s>),
}

/// One expression of a [`ScoreProgram`].
#[derive(Clone, Debug)]
pub enum ScoreExpr<'s> {
    /// A number literal, or number arithmetic folded as JavaScript does.
    Number(f64),
    /// A string literal's value.
    String(Cow<'s, str>),
    /// A name: a binding of the score or of the host.
    Name(Cow<'s, str>),
    /// `{ key: value, ...source }`: plain keys and spreads, in source order.
    Object(Vec<ScoreProperty<'s>>),
    /// `[first, second]`: an array with no hole and no spread.
    Array(Row),
    /// `object.property`, read and not called.
    Member { object: At, property: Cow<'s, str> },
    /// `callee(...arguments)`.
    Call {
        callee: Cow<'s, str>,
        arguments: Row,
        written: Written,
    },
    /// `receiver.method(...arguments)`.
    Method {
        receiver: At,
        method: Cow<'s, str>,
        arguments: Row,
        written: Written,
    },
    /// `(...parameters) => result`, or the same with a block of bindings
    /// and one `return`.
    Arrow(Box<ScoreFunction<'s>>),
}

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
