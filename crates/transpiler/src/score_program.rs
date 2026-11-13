//! The plain shape of a transpiled score, for a host that evaluates it
//! without JavaScript.
//!
//! Recognition is fail-closed: a program that is not made only of the shapes
//! below stays with the JavaScript evaluator, which owns every other
//! construct and every error. The program read is the transpiler's own
//! output, so mini-notation calls, their offsets and lane calls are read as
//! the transpiler wrote them. What a name means is the host's to decide.

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, BinaryOperator, BindingPattern, CallExpression, Expression, Statement, UnaryOperator,
    VariableDeclarationKind,
};
use oxc_parser::Parser;
use oxc_span::SourceType;

/// A score made only of bindings and expression statements.
#[derive(Clone, Debug, PartialEq)]
pub struct ScoreProgram {
    pub statements: Vec<ScoreStatement>,
}

/// One top-level statement.
#[derive(Clone, Debug, PartialEq)]
pub enum ScoreStatement {
    /// `const name = value`, or `let` with no later assignment.
    Binding { name: Box<str>, value: ScoreExpr },
    /// An expression evaluated for its effects and, last, for its value.
    Expression(ScoreExpr),
}

/// One expression of a [`ScoreProgram`].
#[derive(Clone, Debug, PartialEq)]
pub enum ScoreExpr {
    /// A number literal, or number arithmetic folded as JavaScript does.
    Number(f64),
    /// A string literal's value.
    String(Box<str>),
    /// A name: a binding of the score or of the host.
    Name(Box<str>),
    /// `callee(...arguments)`.
    Call {
        callee: Box<str>,
        arguments: Vec<ScoreExpr>,
    },
    /// `receiver.method(...arguments)`.
    Method {
        receiver: Box<ScoreExpr>,
        method: Box<str>,
        arguments: Vec<ScoreExpr>,
    },
}

/// The [`ScoreProgram`] of transpiled `code`, or `None` when any part of it
/// is another shape.
pub fn score_program(code: &str) -> Option<ScoreProgram> {
    if crate::check_nesting(code).is_err() {
        return None;
    }
    crate::on_parse_stack(|| score_program_here(code)).flatten()
}

fn score_program_here(code: &str) -> Option<ScoreProgram> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, code, SourceType::script()).parse();
    if parsed.panicked || !parsed.diagnostics.is_empty() || !parsed.program.directives.is_empty() {
        return None;
    }
    let mut statements = Vec::with_capacity(parsed.program.body.len());
    for statement in &parsed.program.body {
        statements.push(match statement {
            Statement::ExpressionStatement(statement) => {
                ScoreStatement::Expression(expression(&statement.expression)?)
            }
            Statement::VariableDeclaration(declaration)
                if matches!(
                    declaration.kind,
                    VariableDeclarationKind::Const | VariableDeclarationKind::Let
                ) && !declaration.declare =>
            {
                let [declarator] = declaration.declarations.as_slice() else {
                    return None;
                };
                let BindingPattern::BindingIdentifier(name) = &declarator.id else {
                    return None;
                };
                if declarator.definite || declarator.type_annotation.is_some() {
                    return None;
                }
                ScoreStatement::Binding {
                    name: name.name.as_str().into(),
                    value: expression(declarator.init.as_ref()?)?,
                }
            }
            _ => return None,
        });
    }
    Some(ScoreProgram { statements })
}

fn expression(node: &Expression<'_>) -> Option<ScoreExpr> {
    match node.without_parentheses() {
        Expression::NumericLiteral(number) => Some(ScoreExpr::Number(number.value)),
        Expression::StringLiteral(string) if !string.lone_surrogates => {
            Some(ScoreExpr::String(string.value.as_str().into()))
        }
        Expression::Identifier(name) => Some(ScoreExpr::Name(name.name.as_str().into())),
        Expression::UnaryExpression(unary) if unary.operator == UnaryOperator::UnaryNegation => {
            let ScoreExpr::Number(value) = expression(&unary.argument)? else {
                return None;
            };
            Some(ScoreExpr::Number(-value))
        }
        Expression::BinaryExpression(binary) => {
            let (ScoreExpr::Number(left), ScoreExpr::Number(right)) =
                (expression(&binary.left)?, expression(&binary.right)?)
            else {
                return None;
            };
            Some(ScoreExpr::Number(match binary.operator {
                BinaryOperator::Addition => left + right,
                BinaryOperator::Subtraction => left - right,
                BinaryOperator::Multiplication => left * right,
                BinaryOperator::Division => left / right,
                _ => return None,
            }))
        }
        Expression::CallExpression(call) => call_expression(call),
        _ => None,
    }
}

fn call_expression(call: &CallExpression<'_>) -> Option<ScoreExpr> {
    if call.optional || call.type_arguments.is_some() {
        return None;
    }
    let arguments = call
        .arguments
        .iter()
        .map(|argument| match argument {
            Argument::SpreadElement(_) => None,
            argument => expression(argument.to_expression()),
        })
        .collect::<Option<Vec<_>>>()?;
    match call.callee.without_parentheses() {
        Expression::Identifier(callee) => Some(ScoreExpr::Call {
            callee: callee.name.as_str().into(),
            arguments,
        }),
        Expression::StaticMemberExpression(member) if !member.optional => Some(ScoreExpr::Method {
            receiver: Box::new(expression(&member.object)?),
            method: member.property.name.as_str().into(),
            arguments,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{ScoreExpr, ScoreProgram, ScoreStatement, score_program};

    fn call(callee: &str, arguments: Vec<ScoreExpr>) -> ScoreExpr {
        ScoreExpr::Call {
            callee: callee.into(),
            arguments,
        }
    }

    fn method(receiver: ScoreExpr, method: &str, arguments: Vec<ScoreExpr>) -> ScoreExpr {
        ScoreExpr::Method {
            receiver: Box::new(receiver),
            method: method.into(),
            arguments,
        }
    }

    #[test]
    fn a_transpiled_score_reads_as_bindings_and_expressions() {
        let program = score_program(
            "setcpm(120 / 4);\nconst drums = s(m('bd sd', 30)).gain(0.8);\ndrums.room(-0.3).p('$', 0);",
        );
        let mini = call(
            "m",
            vec![ScoreExpr::String("bd sd".into()), ScoreExpr::Number(30.0)],
        );
        assert_eq!(
            program,
            Some(ScoreProgram {
                statements: vec![
                    ScoreStatement::Expression(call("setcpm", vec![ScoreExpr::Number(30.0)])),
                    ScoreStatement::Binding {
                        name: "drums".into(),
                        value: method(call("s", vec![mini]), "gain", vec![ScoreExpr::Number(0.8)]),
                    },
                    ScoreStatement::Expression(method(
                        method(
                            ScoreExpr::Name("drums".into()),
                            "room",
                            vec![ScoreExpr::Number(-0.3)]
                        ),
                        "p",
                        vec![ScoreExpr::String("$".into()), ScoreExpr::Number(0.0)],
                    )),
                ],
            })
        );
    }

    #[test]
    fn any_other_shape_is_left_to_javascript() {
        for code in [
            "s(m('bd', 2)).fast(x => x)",
            "function f() { return 1 }",
            "let n = 1; n = 2;",
            "const [a] = list;",
            "s(...parts)",
            "s(m('bd', 2))?.fast(2)",
            "s(m('bd', 2))['fast'](2)",
            "'a' + 1",
            "`a${b}`",
            "x = 1",
            "if (a) b()",
            "'\\uD800'",
            "1n",
        ] {
            assert_eq!(score_program(code), None, "{code}");
        }
    }
}
