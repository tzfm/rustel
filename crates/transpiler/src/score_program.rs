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
use rustel_score::{
    Held, ScoreExpr, ScoreFunction, ScoreProgram, ScoreProperty, ScoreStatement, ScoreStr, Written,
};

/// The [`ScoreProgram`] of a program parsed with no error, or `None` when
/// any part of it is another shape.
pub(crate) fn read(program: &Program<'_>) -> Option<ScoreProgram<'static>> {
    if program.hashbang.is_some() || !program.directives.is_empty() {
        return None;
    }
    let mut statements = Vec::with_capacity(program.body.len());
    let mut held = Held::default();
    let held = &mut held;
    for statement in &program.body {
        statements.push(match statement {
            Statement::ExpressionStatement(statement) => {
                ScoreStatement::Expression(expression(&statement.expression, 0, held)?)
            }
            Statement::VariableDeclaration(declaration) => {
                let (name, value) = binding(declaration, 0, held)?;
                ScoreStatement::Binding {
                    name,
                    value,
                    written: Written::default(),
                }
            }
            Statement::FunctionDeclaration(function) => {
                let name = held.hold_string(function.id.as_ref()?.name.as_str().to_owned())?;
                ScoreStatement::Function {
                    name,
                    function: declared_function(function, held)?,
                    written: Written::default(),
                }
            }
            _ => return None,
        });
    }
    Some(ScoreProgram {
        statements,
        held: std::mem::take(held),
    })
}

/// `const name = value`, or `let` with no later assignment: no statement a
/// program reads assigns.
fn binding(
    declaration: &VariableDeclaration<'_>,
    depth: usize,
    held: &mut Held<'static>,
) -> Option<(ScoreStr, ScoreExpr)> {
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
    let name = held.hold_string(name.name.as_str().to_owned())?;
    Some((name, expression(declarator.init.as_ref()?, depth, held)?))
}

fn declared_function(function: &Function<'_>, held: &mut Held<'static>) -> Option<ScoreFunction> {
    if function.r#async
        || function.generator
        || function.declare
        || function.type_parameters.is_some()
        || function.this_param.is_some()
        || function.return_type.is_some()
    {
        return None;
    }
    block_function(
        parameters(&function.params, held)?,
        function.body.as_ref()?,
        0,
        held,
    )
}

fn arrow_function(
    arrow: &ArrowFunctionExpression<'_>,
    depth: usize,
    held: &mut Held<'static>,
) -> Option<ScoreFunction> {
    if arrow.r#async || arrow.type_parameters.is_some() || arrow.return_type.is_some() {
        return None;
    }
    let parameters = parameters(&arrow.params, held)?;
    match &arrow.body {
        ArrowFunctionBody::FunctionBody(body) => block_function(parameters, body, depth, held),
        result => Some(ScoreFunction {
            parameters: parameters.into(),
            bindings: Vec::new(),
            result: expression(result.as_expression()?, depth, held)?,
        }),
    }
}

/// Plain names: no default, no pattern, no rest.
fn parameters(
    parameters: &FormalParameters<'_>,
    held: &mut Held<'static>,
) -> Option<Vec<ScoreStr>> {
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
                held.hold_string(name.name.as_str().to_owned())
            }
            _ => None,
        })
        .collect()
}

/// The function of `parameters` and the body `{ const ...; return result }`,
/// read at `depth` levels inside its statement.
fn block_function(
    parameters: Vec<ScoreStr>,
    body: &FunctionBody<'_>,
    depth: usize,
    held: &mut Held<'static>,
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
            Statement::VariableDeclaration(declaration) => binding(declaration, depth, held),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    Some(ScoreFunction {
        parameters: parameters.into(),
        bindings,
        result: expression(returned.argument.as_ref()?, depth, held)?,
    })
}

/// `node` read at `depth` levels inside its statement.
fn expression(node: &Expression<'_>, depth: usize, held: &mut Held<'static>) -> Option<ScoreExpr> {
    if depth >= crate::MAX_SOURCE_NESTING {
        return None;
    }
    let depth = depth + 1;
    match node.without_parentheses() {
        Expression::NumericLiteral(number) => Some(ScoreExpr::Number(number.value)),
        Expression::StringLiteral(string) if !string.lone_surrogates => Some(ScoreExpr::String(
            held.hold_string(string.value.as_str().to_owned())?,
        )),
        Expression::Identifier(name) => Some(ScoreExpr::Name(
            held.hold_string(name.name.as_str().to_owned())?,
        )),
        Expression::UnaryExpression(unary) if unary.operator == UnaryOperator::UnaryNegation => {
            let ScoreExpr::Number(value) = expression(&unary.argument, depth, held)? else {
                return None;
            };
            Some(ScoreExpr::Number(-value))
        }
        Expression::BinaryExpression(binary) => {
            let (ScoreExpr::Number(left), ScoreExpr::Number(right)) = (
                expression(&binary.left, depth, held)?,
                expression(&binary.right, depth, held)?,
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
        Expression::CallExpression(call) => call_expression(call, depth, held),
        Expression::ObjectExpression(object) => object_expression(object, depth, held),
        Expression::ArrayExpression(array) => {
            let elements = array
                .elements
                .iter()
                .map(|element| expression(element.as_expression()?, depth, held))
                .collect::<Option<Vec<_>>>()?;
            Some(ScoreExpr::Array(held.hold_row(elements)?))
        }
        Expression::ArrowFunctionExpression(arrow) => {
            let func = arrow_function(arrow, depth, held)?;
            Some(ScoreExpr::Arrow(held.hold_function(func)?))
        }
        Expression::StaticMemberExpression(member) if !member.optional => {
            let object = expression(&member.object, depth, held)?;
            Some(ScoreExpr::Member {
                object: held.hold(object)?,
                property: held.hold_string(member.property.name.as_str().to_owned())?,
            })
        }
        _ => None,
    }
}

/// An object literal of plain `key: value` properties and spreads. A
/// `__proto__` key sets the prototype instead of a property, so it is
/// refused with every computed key, method and accessor.
fn object_expression(
    object: &ObjectExpression<'_>,
    depth: usize,
    held: &mut Held<'static>,
) -> Option<ScoreExpr> {
    let properties = object
        .properties
        .iter()
        .map(|property| {
            let property = match property {
                ObjectPropertyKind::ObjectProperty(property) => property,
                ObjectPropertyKind::SpreadProperty(spread) => {
                    return Some(ScoreProperty::Spread(expression(
                        &spread.argument,
                        depth,
                        held,
                    )?));
                }
            };
            if property.kind != PropertyKind::Init || property.method || property.computed {
                return None;
            }
            let key = match &property.key {
                PropertyKey::StaticIdentifier(name) => name.name.as_str(),
                PropertyKey::StringLiteral(string) if !string.lone_surrogates => {
                    string.value.as_str()
                }
                _ => return None,
            };
            if key == "__proto__" {
                return None;
            }
            let key = held.hold_string(key.to_owned())?;
            Some(ScoreProperty::Entry {
                key,
                value: expression(&property.value, depth, held)?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(ScoreExpr::Object(held.hold_props(properties)?))
}

fn call_expression(
    call: &CallExpression<'_>,
    depth: usize,
    held: &mut Held<'static>,
) -> Option<ScoreExpr> {
    if call.optional || call.type_arguments.is_some() {
        return None;
    }
    let arguments = call
        .arguments
        .iter()
        .map(|argument| match argument {
            Argument::SpreadElement(_) => None,
            argument => expression(argument.to_expression(), depth, held),
        })
        .collect::<Option<Vec<_>>>()?;
    match call.callee.without_parentheses() {
        Expression::Identifier(callee) => {
            if callee.name.as_str() == "m"
                && let [ScoreExpr::String(source), ScoreExpr::Number(offset)] = arguments.as_slice()
                && offset.fract() == 0.0
                && offset.is_finite()
                && let Ok(offset) = u32::try_from(*offset as i64)
            {
                return Some(ScoreExpr::Mini {
                    source: *source,
                    offset,
                });
            }
            Some(ScoreExpr::Call {
                callee: held.hold_string(callee.name.as_str().to_owned())?,
                arguments: held.hold_row(arguments)?,
                written: Written::default(),
            })
        }
        Expression::StaticMemberExpression(member) if !member.optional => {
            let receiver = expression(&member.object, depth, held)?;
            Some(ScoreExpr::Method {
                receiver: held.hold(receiver)?,
                method: held.hold_string(member.property.name.as_str().to_owned())?,
                arguments: held.hold_row(arguments)?,
                written: Written::default(),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Held, ScoreExpr, ScoreFunction, ScoreProgram, ScoreProperty, ScoreStatement, Written, read,
    };
    use oxc_allocator::Allocator;
    use oxc_parser::Parser;
    use oxc_span::SourceType;
    use std::cell::RefCell;

    thread_local! {
        /// What the expressions that a test wrote so far hold.
        static HELD: RefCell<Held<'static>> = RefCell::new(Held::default());
    }

    /// The program of `statements`, which a test wrote with the functions
    /// below.
    fn program(statements: Vec<ScoreStatement>) -> Option<ScoreProgram<'static>> {
        Some(ScoreProgram {
            statements,
            held: HELD.take(),
        })
    }

    /// `code` read as the transpiler reads the program it prints.
    fn score_program(code: &str) -> Option<ScoreProgram<'static>> {
        crate::on_parse_stack(code, |code| {
            let allocator = Allocator::default();
            let parsed = Parser::new(&allocator, code, SourceType::mjs()).parse();
            (!parsed.panicked && parsed.diagnostics.is_empty())
                .then(|| read(&parsed.program))
                .flatten()
        })
        .flatten()
    }

    fn text(s: &str) -> rustel_score::ScoreStr {
        HELD.with_borrow_mut(|held| held.hold_str(s).expect("a str"))
    }

    fn call(callee: &'static str, arguments: Vec<ScoreExpr>) -> ScoreExpr {
        ScoreExpr::Call {
            callee: text(callee),
            arguments: HELD.with_borrow_mut(|held| held.hold_row(arguments).expect("a row")),
            written: Written::default(),
        }
    }

    fn array(elements: Vec<ScoreExpr>) -> ScoreExpr {
        ScoreExpr::Array(HELD.with_borrow_mut(|held| held.hold_row(elements).expect("a row")))
    }

    fn member(object: ScoreExpr, property: &'static str) -> ScoreExpr {
        ScoreExpr::Member {
            object: HELD.with_borrow_mut(|held| held.hold(object).expect("a place")),
            property: text(property),
        }
    }

    fn method(receiver: ScoreExpr, method: &'static str, arguments: Vec<ScoreExpr>) -> ScoreExpr {
        HELD.with_borrow_mut(|held| ScoreExpr::Method {
            receiver: held.hold(receiver).expect("a place"),
            method: held.hold_str(method).expect("a str"),
            arguments: held.hold_row(arguments).expect("a row"),
            written: Written::default(),
        })
    }

    #[test]
    fn a_transpiled_score_reads_as_bindings_and_expressions() {
        let read = score_program(
            "setcpm(120 / 4);\nconst drums = s(m('bd sd', 30)).gain(0.8);\ndrums.room(-0.3).p('$', 0);",
        );
        let mini = ScoreExpr::Mini {
            source: text("bd sd"),
            offset: 30,
        };
        assert_eq!(
            read,
            program(vec![
                ScoreStatement::Expression(call("setcpm", vec![ScoreExpr::Number(30.0)])),
                ScoreStatement::Binding {
                    name: text("drums"),
                    value: method(call("s", vec![mini]), "gain", vec![ScoreExpr::Number(0.8)]),
                    written: Written::default(),
                },
                ScoreStatement::Expression(method(
                    method(
                        ScoreExpr::Name(text("drums")),
                        "room",
                        vec![ScoreExpr::Number(-0.3)]
                    ),
                    "p",
                    vec![ScoreExpr::String(text("$")), ScoreExpr::Number(0.0)],
                )),
            ])
        );
    }

    #[test]
    fn a_function_reads_as_parameters_bindings_and_its_result() {
        let read = score_program(
            "function lead(notes) { const base = note(notes); return base.s(m('x', 9)) }",
        );
        let mini = ScoreExpr::Mini {
            source: text("x"),
            offset: 9,
        };
        assert_eq!(
            read,
            program(vec![ScoreStatement::Function {
                name: text("lead"),
                function: ScoreFunction {
                    parameters: vec![text("notes")].into(),
                    bindings: vec![(
                        text("base"),
                        call("note", vec![ScoreExpr::Name(text("notes"))])
                    )],
                    result: method(ScoreExpr::Name(text("base")), "s", vec![mini]),
                },
                written: Written::default(),
            }])
        );
    }

    #[test]
    fn an_arrow_reads_as_a_function_with_or_without_a_block() {
        let reversed = |bindings: &[(&str, ScoreExpr)]| {
            let func = ScoreFunction {
                parameters: vec![text("x")].into(),
                bindings: bindings.iter().map(|(k, v)| (text(k), *v)).collect(),
                result: method(ScoreExpr::Name(text("x")), "rev", vec![]),
            };
            ScoreExpr::Arrow(HELD.with_borrow_mut(|held| held.hold_function(func).expect("a func")))
        };
        for (code, bindings) in [
            ("s.every(4, x => x.rev())", &[][..]),
            ("s.every(4, (x) => { return x.rev() })", &[][..]),
            (
                "s.every(4, x => { const y = 1; return x.rev() })",
                &[("y", ScoreExpr::Number(1.0))][..],
            ),
        ] {
            let every = method(
                ScoreExpr::Name(text("s")),
                "every",
                vec![ScoreExpr::Number(4.0), reversed(bindings)],
            );
            assert_eq!(
                score_program(code),
                program(vec![ScoreStatement::Expression(every)]),
                "{code}"
            );
        }
    }

    #[test]
    fn an_object_reads_as_its_properties_and_a_member_as_a_read() {
        let read =
            score_program("const kit = { kick, 'snare drum': 2, ...more };\nkit.kick.fast(2)");
        let kit = ScoreExpr::Object(HELD.with_borrow_mut(|held| {
            let k1 = held.hold_str("kick").expect("a str");
            let k2 = held.hold_str("snare drum").expect("a str");
            let more = held.hold_str("more").expect("a str");
            held.hold_props(vec![
                ScoreProperty::Entry {
                    key: k1,
                    value: ScoreExpr::Name(k1),
                },
                ScoreProperty::Entry {
                    key: k2,
                    value: ScoreExpr::Number(2.0),
                },
                ScoreProperty::Spread(ScoreExpr::Name(more)),
            ])
            .expect("props")
        }));
        let kick = member(ScoreExpr::Name(text("kit")), "kick");
        assert_eq!(
            read,
            program(vec![
                ScoreStatement::Binding {
                    name: text("kit"),
                    value: kit,
                    written: Written::default(),
                },
                ScoreStatement::Expression(method(kick, "fast", vec![ScoreExpr::Number(2.0)])),
            ])
        );
    }

    #[test]
    fn an_array_reads_as_its_elements() {
        assert_eq!(
            score_program("[a, 2]"),
            program(vec![ScoreStatement::Expression(array(vec![
                ScoreExpr::Name(text("a")),
                ScoreExpr::Number(2.0),
            ]))])
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
