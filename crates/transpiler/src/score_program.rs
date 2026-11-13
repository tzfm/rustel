//! The plain shape of a transpiled score, for a host that evaluates it
//! without JavaScript.
//!
//! Recognition is fail-closed: a program that is not made only of the shapes
//! below stays with the JavaScript evaluator, which owns every other
//! construct and every error. The program read is the one the transpiler
//! prints, so mini-notation calls, their offsets and lane calls are read as
//! the transpiler wrote them. What a name means is the host's to decide.
//!
//! No expression nests deeper than [`crate::MAX_SOURCE_NESTING`], so a host
//! walks a program, and drops it, on any stack.

use oxc_ast::ast::{
    Argument, ArrowFunctionBody, ArrowFunctionExpression, BinaryOperator, BindingPattern,
    CallExpression, Expression, FormalParameters, Function, FunctionBody, ObjectExpression,
    ObjectPropertyKind, Program, PropertyKey, PropertyKind, Statement, UnaryOperator,
    VariableDeclaration, VariableDeclarationKind,
};

/// A score made only of bindings, functions and expression statements.
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
    /// `function name(...) {...}`, which JavaScript hoists.
    Function {
        name: Box<str>,
        function: ScoreFunction,
    },
}

/// What a function declaration or an arrow holds: plain parameters,
/// bindings, then one result. `(a) => result` has no bindings.
#[derive(Clone, Debug, PartialEq)]
pub struct ScoreFunction {
    pub parameters: Vec<Box<str>>,
    pub bindings: Vec<(Box<str>, ScoreExpr)>,
    pub result: ScoreExpr,
}

/// One part of an object literal.
#[derive(Clone, Debug, PartialEq)]
pub enum ScoreProperty {
    /// `key: value`, or `key` alone for a name of that spelling.
    Entry { key: Box<str>, value: ScoreExpr },
    /// `...source`: every property of another object.
    Spread(ScoreExpr),
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
    /// `{ key: value, ...source }`: plain keys and spreads, in source order.
    Object(Vec<ScoreProperty>),
    /// `[first, second]`: an array with no hole and no spread.
    Array(Vec<ScoreExpr>),
    /// `object.property`, read and not called.
    Member {
        object: Box<ScoreExpr>,
        property: Box<str>,
    },
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
    /// `(...parameters) => result`, or the same with a block of bindings
    /// and one `return`.
    Arrow(Box<ScoreFunction>),
}

/// The [`ScoreProgram`] of a program parsed with no error, or `None` when
/// any part of it is another shape.
pub(crate) fn read(program: &Program<'_>) -> Option<ScoreProgram> {
    if program.hashbang.is_some() || !program.directives.is_empty() {
        return None;
    }
    let mut statements = Vec::with_capacity(program.body.len());
    for statement in &program.body {
        statements.push(match statement {
            Statement::ExpressionStatement(statement) => {
                ScoreStatement::Expression(expression(&statement.expression, 0)?)
            }
            Statement::VariableDeclaration(declaration) => {
                let (name, value) = binding(declaration, 0)?;
                ScoreStatement::Binding { name, value }
            }
            Statement::FunctionDeclaration(function) => ScoreStatement::Function {
                name: function.id.as_ref()?.name.as_str().into(),
                function: declared_function(function)?,
            },
            _ => return None,
        });
    }
    Some(ScoreProgram { statements })
}

/// `const name = value`, or `let` with no later assignment: no statement a
/// program reads assigns.
fn binding(declaration: &VariableDeclaration<'_>, depth: usize) -> Option<(Box<str>, ScoreExpr)> {
    if !matches!(
        declaration.kind,
        VariableDeclarationKind::Const | VariableDeclarationKind::Let
    ) || declaration.declare
    {
        return None;
    }
    let [declarator] = declaration.declarations.as_slice() else {
        return None;
    };
    let BindingPattern::BindingIdentifier(name) = &declarator.id else {
        return None;
    };
    if declarator.definite || declarator.type_annotation.is_some() {
        return None;
    }
    Some((
        name.name.as_str().into(),
        expression(declarator.init.as_ref()?, depth)?,
    ))
}

fn declared_function(function: &Function<'_>) -> Option<ScoreFunction> {
    if function.r#async
        || function.generator
        || function.declare
        || function.type_parameters.is_some()
        || function.this_param.is_some()
        || function.return_type.is_some()
    {
        return None;
    }
    block_function(parameters(&function.params)?, function.body.as_ref()?, 0)
}

fn arrow_function(arrow: &ArrowFunctionExpression<'_>, depth: usize) -> Option<ScoreFunction> {
    if arrow.r#async || arrow.type_parameters.is_some() || arrow.return_type.is_some() {
        return None;
    }
    let parameters = parameters(&arrow.params)?;
    match &arrow.body {
        ArrowFunctionBody::FunctionBody(body) => block_function(parameters, body, depth),
        result => Some(ScoreFunction {
            parameters,
            bindings: Vec::new(),
            result: expression(result.as_expression()?, depth)?,
        }),
    }
}

/// Plain names: no default, no pattern, no rest.
fn parameters(parameters: &FormalParameters<'_>) -> Option<Vec<Box<str>>> {
    if parameters.rest.is_some() {
        return None;
    }
    parameters
        .items
        .iter()
        .map(|parameter| match &parameter.pattern {
            BindingPattern::BindingIdentifier(name)
                if parameter.initializer.is_none()
                    && !parameter.optional
                    && parameter.type_annotation.is_none()
                    && parameter.decorators.is_empty() =>
            {
                Some(name.name.as_str().into())
            }
            _ => None,
        })
        .collect()
}

/// The function of `parameters` and the body `{ const ...; return result }`,
/// read at `depth` levels inside its statement.
fn block_function(
    parameters: Vec<Box<str>>,
    body: &FunctionBody<'_>,
    depth: usize,
) -> Option<ScoreFunction> {
    if !body.directives.is_empty() {
        return None;
    }
    let (Statement::ReturnStatement(returned), leading) = body.statements.split_last()? else {
        return None;
    };
    let bindings = leading
        .iter()
        .map(|statement| match statement {
            Statement::VariableDeclaration(declaration) => binding(declaration, depth),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    Some(ScoreFunction {
        parameters,
        bindings,
        result: expression(returned.argument.as_ref()?, depth)?,
    })
}

/// `node` read at `depth` levels inside its statement.
fn expression(node: &Expression<'_>, depth: usize) -> Option<ScoreExpr> {
    if depth >= crate::MAX_SOURCE_NESTING {
        return None;
    }
    let depth = depth + 1;
    match node.without_parentheses() {
        Expression::NumericLiteral(number) => Some(ScoreExpr::Number(number.value)),
        Expression::StringLiteral(string) if !string.lone_surrogates => {
            Some(ScoreExpr::String(string.value.as_str().into()))
        }
        Expression::Identifier(name) => Some(ScoreExpr::Name(name.name.as_str().into())),
        Expression::UnaryExpression(unary) if unary.operator == UnaryOperator::UnaryNegation => {
            let ScoreExpr::Number(value) = expression(&unary.argument, depth)? else {
                return None;
            };
            Some(ScoreExpr::Number(-value))
        }
        Expression::BinaryExpression(binary) => {
            let (ScoreExpr::Number(left), ScoreExpr::Number(right)) = (
                expression(&binary.left, depth)?,
                expression(&binary.right, depth)?,
            ) else {
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
        Expression::CallExpression(call) => call_expression(call, depth),
        Expression::ObjectExpression(object) => object_expression(object, depth),
        Expression::ArrayExpression(array) => Some(ScoreExpr::Array(
            array
                .elements
                .iter()
                .map(|element| expression(element.as_expression()?, depth))
                .collect::<Option<Vec<_>>>()?,
        )),
        Expression::ArrowFunctionExpression(arrow) => {
            Some(ScoreExpr::Arrow(Box::new(arrow_function(arrow, depth)?)))
        }
        Expression::StaticMemberExpression(member) if !member.optional => Some(ScoreExpr::Member {
            object: Box::new(expression(&member.object, depth)?),
            property: member.property.name.as_str().into(),
        }),
        _ => None,
    }
}

/// An object literal of plain `key: value` properties and spreads. A
/// `__proto__` key sets the prototype instead of a property, so it is
/// refused with every computed key, method and accessor.
fn object_expression(object: &ObjectExpression<'_>, depth: usize) -> Option<ScoreExpr> {
    let properties = object
        .properties
        .iter()
        .map(|property| {
            let property = match property {
                ObjectPropertyKind::ObjectProperty(property) => property,
                ObjectPropertyKind::SpreadProperty(spread) => {
                    return Some(ScoreProperty::Spread(expression(&spread.argument, depth)?));
                }
            };
            if property.kind != PropertyKind::Init || property.method || property.computed {
                return None;
            }
            let key: Box<str> = match &property.key {
                PropertyKey::StaticIdentifier(name) => name.name.as_str().into(),
                PropertyKey::StringLiteral(string) if !string.lone_surrogates => {
                    string.value.as_str().into()
                }
                _ => return None,
            };
            if &*key == "__proto__" {
                return None;
            }
            Some(ScoreProperty::Entry {
                key,
                value: expression(&property.value, depth)?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(ScoreExpr::Object(properties))
}

fn call_expression(call: &CallExpression<'_>, depth: usize) -> Option<ScoreExpr> {
    if call.optional || call.type_arguments.is_some() {
        return None;
    }
    let arguments = call
        .arguments
        .iter()
        .map(|argument| match argument {
            Argument::SpreadElement(_) => None,
            argument => expression(argument.to_expression(), depth),
        })
        .collect::<Option<Vec<_>>>()?;
    match call.callee.without_parentheses() {
        Expression::Identifier(callee) => Some(ScoreExpr::Call {
            callee: callee.name.as_str().into(),
            arguments,
        }),
        Expression::StaticMemberExpression(member) if !member.optional => Some(ScoreExpr::Method {
            receiver: Box::new(expression(&member.object, depth)?),
            method: member.property.name.as_str().into(),
            arguments,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{ScoreExpr, ScoreFunction, ScoreProgram, ScoreProperty, ScoreStatement, read};
    use oxc_allocator::Allocator;
    use oxc_parser::Parser;
    use oxc_span::SourceType;

    /// `code` read as the transpiler reads the program it prints.
    fn score_program(code: &str) -> Option<ScoreProgram> {
        crate::on_parse_stack(|| {
            let allocator = Allocator::default();
            let parsed = Parser::new(&allocator, code, SourceType::mjs()).parse();
            (!parsed.panicked && parsed.diagnostics.is_empty())
                .then(|| read(&parsed.program))
                .flatten()
        })
        .flatten()
    }

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
    fn a_function_reads_as_parameters_bindings_and_its_result() {
        let program = score_program(
            "function lead(notes) { const base = note(notes); return base.s(m('x', 9)) }",
        );
        let mini = call(
            "m",
            vec![ScoreExpr::String("x".into()), ScoreExpr::Number(9.0)],
        );
        assert_eq!(
            program,
            Some(ScoreProgram {
                statements: vec![ScoreStatement::Function {
                    name: "lead".into(),
                    function: ScoreFunction {
                        parameters: vec!["notes".into()],
                        bindings: vec![(
                            "base".into(),
                            call("note", vec![ScoreExpr::Name("notes".into())])
                        )],
                        result: method(ScoreExpr::Name("base".into()), "s", vec![mini]),
                    },
                }],
            })
        );
    }

    #[test]
    fn an_arrow_reads_as_a_function_with_or_without_a_block() {
        let reversed = |bindings| {
            ScoreExpr::Arrow(Box::new(ScoreFunction {
                parameters: vec!["x".into()],
                bindings,
                result: method(ScoreExpr::Name("x".into()), "rev", vec![]),
            }))
        };
        for (code, bindings) in [
            ("s.every(4, x => x.rev())", vec![]),
            ("s.every(4, (x) => { return x.rev() })", vec![]),
            (
                "s.every(4, x => { const y = 1; return x.rev() })",
                vec![("y".into(), ScoreExpr::Number(1.0))],
            ),
        ] {
            let every = method(
                ScoreExpr::Name("s".into()),
                "every",
                vec![ScoreExpr::Number(4.0), reversed(bindings)],
            );
            assert_eq!(
                score_program(code),
                Some(ScoreProgram {
                    statements: vec![ScoreStatement::Expression(every)],
                }),
                "{code}"
            );
        }
    }

    #[test]
    fn an_object_reads_as_its_properties_and_a_member_as_a_read() {
        let program =
            score_program("const kit = { kick, 'snare drum': 2, ...more };\nkit.kick.fast(2)");
        let kit = ScoreExpr::Object(vec![
            ScoreProperty::Entry {
                key: "kick".into(),
                value: ScoreExpr::Name("kick".into()),
            },
            ScoreProperty::Entry {
                key: "snare drum".into(),
                value: ScoreExpr::Number(2.0),
            },
            ScoreProperty::Spread(ScoreExpr::Name("more".into())),
        ]);
        let read = ScoreExpr::Member {
            object: Box::new(ScoreExpr::Name("kit".into())),
            property: "kick".into(),
        };
        assert_eq!(
            program,
            Some(ScoreProgram {
                statements: vec![
                    ScoreStatement::Binding {
                        name: "kit".into(),
                        value: kit
                    },
                    ScoreStatement::Expression(method(read, "fast", vec![ScoreExpr::Number(2.0)])),
                ],
            })
        );
    }

    #[test]
    fn an_array_reads_as_its_elements() {
        assert_eq!(
            score_program("[a, 2]"),
            Some(ScoreProgram {
                statements: vec![ScoreStatement::Expression(ScoreExpr::Array(vec![
                    ScoreExpr::Name("a".into()),
                    ScoreExpr::Number(2.0),
                ]))],
            })
        );
    }

    #[test]
    fn an_expression_past_the_nesting_limit_is_left_to_javascript() {
        let deep = format!("s{}", ".fast(2)".repeat(crate::MAX_SOURCE_NESTING));
        assert_eq!(score_program(&deep), None);
    }

    #[test]
    fn any_other_shape_is_left_to_javascript() {
        for code in [
            "s.every(4, async x => x)",
            "s.every(4, (x = 1) => x)",
            "s.every(4, ({ x }) => x)",
            "s.every(4, x => { x.rev() })",
            "s.every(4, function (x) { return x })",
            "function f(a = 1) { return a }",
            "function f(...a) { return a[0] }",
            "function f() { g(); return 1 }",
            "function f() { if (a) { return 1 } return 2 }",
            "function f() { const a = 1 }",
            "async function f() { return 1 }",
            "let n = 1; n = 2;",
            "const [a] = list;",
            "s(...parts)",
            "s(m('bd', 2))?.fast(2)",
            "s(m('bd', 2))['fast'](2)",
            "({ __proto__: a })",
            "({ [key]: 1 })",
            "({ get a() { return 1 } })",
            "({ a() { return 1 } })",
            "[a, , b]",
            "[...a]",
            "a?.b",
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
