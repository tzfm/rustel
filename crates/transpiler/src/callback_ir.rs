//! Conservative AST recognition for callback-IR candidates.
//!
//! The runtime asks this module about the source returned by the realm's
//! captured `Function.prototype.toString`. Recognition is fail-closed: a
//! function that is not one exact supported shape remains an ordinary QuickJS
//! callback.
//!
//! Two shapes are recognised. The identity arrow returns its parameter. A
//! method chain calls methods on its parameter in sequence, and each argument
//! is a literal, the transpiler's own mini-notation call with literal
//! arguments, or one nested arrow of these same two shapes, in a method's
//! function slot. Which method names the runtime can compile is the runtime's
//! decision. This module knows only the syntax.

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, ArrowFunctionBody, ArrowFunctionExpression, BindingPattern, CallExpression,
    Expression, Statement, UnaryOperator,
};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};

/// Parsing one callback again is bounded independently from the complete
/// score. Real callback expressions are tiny. A larger function remains on the
/// compatibility tier instead of adding unbounded per-callback parse work.
pub const MAX_CALLBACK_IR_SOURCE_BYTES: usize = 4 * 1024;

/// How deeply one recognised arrow may take another as a method argument.
/// Each level lowers its own bounded program, so this bounds the work one
/// recognition performs.
pub const MAX_CALLBACK_TRANSFORM_DEPTH: usize = 3;

/// The callee the transpiler writes for every mini-notation literal.
const MINI_CALLEE: &str = "m";

/// The largest integer a JavaScript number holds exactly.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// A callback shape whose semantics have a native IR representation.
#[derive(Clone, Debug, PartialEq)]
pub enum PatternTransformCandidate {
    /// One ordinary parameter returned directly: `pattern => pattern`.
    Identity,
    /// Method calls on the parameter, in call order:
    /// `pattern => pattern.fast(2).rev()`.
    MethodChain(Box<[CallbackMethodCall]>),
}

/// One call of a method chain.
#[derive(Clone, Debug, PartialEq)]
pub struct CallbackMethodCall {
    pub method: Box<str>,
    pub arguments: Box<[CallbackArgument]>,
}

/// One literal argument of a method call.
#[derive(Clone, Debug, PartialEq)]
pub enum CallbackArgument {
    /// A number literal, negated when the source negates it.
    Number(f64),
    Bool(bool),
    /// A string literal the transpiler left as a string.
    String(Box<str>),
    /// The transpiler's mini-notation call `m(source, offset)`.
    Mini {
        source: Box<str>,
        offset: i64,
    },
    /// A nested arrow in a method's function slot: the exact source of the
    /// arrow, as its span reads in the parsed callback, and the shape the
    /// runtime compiles for it.
    Transform {
        source: Box<str>,
        candidate: Box<PatternTransformCandidate>,
    },
}

/// Why source stayed on the complete QuickJS compatibility tier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PatternTransformFallbackReason {
    SourceTooLarge,
    ParseFailure,
    NotArrowExpression,
    AsyncArrow,
    ParameterShape,
    BodyShape,
    /// A call receiver that is neither the parameter nor another call.
    ReceiverShape,
    /// A callee that is not a static, non-optional method of the receiver.
    CalleeShape,
    /// An argument that is not a supported literal.
    ArgumentShape,
    /// A call of the mini-notation callee with other arguments than the
    /// transpiler writes.
    MiniCallShape,
    /// A mini-notation call inside a callback whose parameter is named like
    /// the mini-notation callee.
    MiniCalleeShadowed,
}

/// Recognise one bounded, capture-free `Pattern -> Pattern` expression.
///
/// This uses the same ES parser as score transpilation. It does not infer
/// purity from text matching or accept a block body that merely looks
/// equivalent: every admitted shape has its own syntactic proof.
pub fn lower_pattern_transform_callback(
    source: &str,
) -> Result<PatternTransformCandidate, PatternTransformFallbackReason> {
    if source.len() > MAX_CALLBACK_IR_SOURCE_BYTES {
        return Err(PatternTransformFallbackReason::SourceTooLarge);
    }
    // The parser recurses once per nesting level. The lexical bound keeps a
    // pathological source from reaching it.
    if crate::check_nesting(source).is_err() {
        return Err(PatternTransformFallbackReason::ParseFailure);
    }

    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::mjs()).parse();
    if parsed.panicked || !parsed.diagnostics.is_empty() {
        return Err(PatternTransformFallbackReason::ParseFailure);
    }
    let [Statement::ExpressionStatement(statement)] = parsed.program.body.as_slice() else {
        return Err(PatternTransformFallbackReason::NotArrowExpression);
    };
    let Expression::ArrowFunctionExpression(arrow) = statement.expression.without_parentheses()
    else {
        return Err(PatternTransformFallbackReason::NotArrowExpression);
    };
    arrow_candidate(arrow, source, 0)
}

/// Recognise one arrow of either recognised shape, at `depth` levels of
/// method-argument nesting. The same proof runs for the callback's own arrow
/// and for a nested arrow a method call takes as its function argument.
fn arrow_candidate(
    arrow: &ArrowFunctionExpression<'_>,
    source: &str,
    depth: usize,
) -> Result<PatternTransformCandidate, PatternTransformFallbackReason> {
    if arrow.r#async {
        return Err(PatternTransformFallbackReason::AsyncArrow);
    }
    let [parameter] = arrow.params.items.as_slice() else {
        return Err(PatternTransformFallbackReason::ParameterShape);
    };
    if arrow.params.rest.is_some()
        || parameter.initializer.is_some()
        || parameter.optional
        || parameter.type_annotation.is_some()
        || !parameter.decorators.is_empty()
        || arrow.type_parameters.is_some()
        || arrow.return_type.is_some()
    {
        return Err(PatternTransformFallbackReason::ParameterShape);
    }
    let BindingPattern::BindingIdentifier(parameter) = &parameter.pattern else {
        return Err(PatternTransformFallbackReason::ParameterShape);
    };
    let parameter = parameter.name.as_str();
    let body = match &arrow.body {
        ArrowFunctionBody::FunctionBody(_) => {
            return Err(PatternTransformFallbackReason::BodyShape);
        }
        body => body.to_expression().without_parentheses(),
    };
    match body {
        Expression::Identifier(identifier) if identifier.name == parameter => {
            Ok(PatternTransformCandidate::Identity)
        }
        Expression::CallExpression(_) => method_chain(body, parameter, source, depth),
        _ => Err(PatternTransformFallbackReason::BodyShape),
    }
}

/// Walk a call chain from its last call back to the parameter.
fn method_chain(
    body: &Expression<'_>,
    parameter: &str,
    source: &str,
    depth: usize,
) -> Result<PatternTransformCandidate, PatternTransformFallbackReason> {
    let mut calls = Vec::new();
    let mut receiver = body;
    loop {
        match receiver {
            Expression::Identifier(identifier) if identifier.name == parameter => break,
            Expression::CallExpression(call) => {
                let (method, object) = method_callee(call)?;
                let arguments = call
                    .arguments
                    .iter()
                    .map(|argument| literal_argument(argument, parameter, source, depth))
                    .collect::<Result<Box<[_]>, _>>()?;
                calls.push(CallbackMethodCall {
                    method: method.into(),
                    arguments,
                });
                receiver = object.without_parentheses();
            }
            _ => return Err(PatternTransformFallbackReason::ReceiverShape),
        }
    }
    calls.reverse();
    Ok(PatternTransformCandidate::MethodChain(
        calls.into_boxed_slice(),
    ))
}

/// The method name and receiver of `receiver.method(...)`.
fn method_callee<'s, 'a>(
    call: &'s CallExpression<'a>,
) -> Result<(&'s str, &'s Expression<'a>), PatternTransformFallbackReason> {
    if call.optional || call.type_arguments.is_some() {
        return Err(PatternTransformFallbackReason::CalleeShape);
    }
    let Expression::StaticMemberExpression(member) = call.callee.without_parentheses() else {
        return Err(PatternTransformFallbackReason::CalleeShape);
    };
    if member.optional {
        return Err(PatternTransformFallbackReason::CalleeShape);
    }
    Ok((member.property.name.as_str(), &member.object))
}

fn literal_argument(
    argument: &Argument<'_>,
    parameter: &str,
    source: &str,
    depth: usize,
) -> Result<CallbackArgument, PatternTransformFallbackReason> {
    let Some(expression) = argument.as_expression() else {
        return Err(PatternTransformFallbackReason::ArgumentShape);
    };
    let expression = expression.without_parentheses();
    if let Some(number) = number_literal(expression) {
        return Ok(CallbackArgument::Number(number));
    }
    match expression {
        Expression::BooleanLiteral(literal) => Ok(CallbackArgument::Bool(literal.value)),
        Expression::StringLiteral(literal) if !literal.lone_surrogates => {
            Ok(CallbackArgument::String(literal.value.as_str().into()))
        }
        Expression::CallExpression(call) => mini_call(call, parameter),
        // A nested arrow is a method's function argument. Recognising it
        // against the same two shapes proves it capture-free, so its own
        // source is all a runtime needs to bridge an equivalent fallback.
        Expression::ArrowFunctionExpression(arrow) => {
            if depth >= MAX_CALLBACK_TRANSFORM_DEPTH {
                return Err(PatternTransformFallbackReason::ArgumentShape);
            }
            let candidate = arrow_candidate(arrow, source, depth + 1)?;
            let span = arrow.span();
            let text = source
                .get(span.start as usize..span.end as usize)
                .ok_or(PatternTransformFallbackReason::ArgumentShape)?;
            Ok(CallbackArgument::Transform {
                source: text.into(),
                candidate: Box::new(candidate),
            })
        }
        _ => Err(PatternTransformFallbackReason::ArgumentShape),
    }
}

/// A number literal, or a negated one.
fn number_literal(expression: &Expression<'_>) -> Option<f64> {
    match expression {
        Expression::NumericLiteral(literal) => Some(literal.value),
        Expression::UnaryExpression(unary) if unary.operator == UnaryOperator::UnaryNegation => {
            match unary.argument.without_parentheses() {
                Expression::NumericLiteral(literal) => Some(-literal.value),
                _ => None,
            }
        }
        _ => None,
    }
}

/// `m('source', offset)`, exactly as the transpiler writes a mini-notation
/// literal.
fn mini_call(
    call: &CallExpression<'_>,
    parameter: &str,
) -> Result<CallbackArgument, PatternTransformFallbackReason> {
    let Expression::Identifier(callee) = call.callee.without_parentheses() else {
        return Err(PatternTransformFallbackReason::ArgumentShape);
    };
    if callee.name != MINI_CALLEE {
        return Err(PatternTransformFallbackReason::ArgumentShape);
    }
    if parameter == MINI_CALLEE {
        return Err(PatternTransformFallbackReason::MiniCalleeShadowed);
    }
    if call.optional || call.type_arguments.is_some() {
        return Err(PatternTransformFallbackReason::MiniCallShape);
    }
    let [source, offset] = call.arguments.as_slice() else {
        return Err(PatternTransformFallbackReason::MiniCallShape);
    };
    let Some(Expression::StringLiteral(source)) = source.as_expression() else {
        return Err(PatternTransformFallbackReason::MiniCallShape);
    };
    if source.lone_surrogates {
        return Err(PatternTransformFallbackReason::MiniCallShape);
    }
    let offset = offset
        .as_expression()
        .and_then(|offset| number_literal(offset.without_parentheses()))
        .filter(|offset| offset.fract() == 0.0 && offset.abs() <= MAX_SAFE_INTEGER)
        .ok_or(PatternTransformFallbackReason::MiniCallShape)?;
    Ok(CallbackArgument::Mini {
        source: source.value.as_str().into(),
        offset: offset as i64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(calls: &[(&str, &[CallbackArgument])]) -> PatternTransformCandidate {
        PatternTransformCandidate::MethodChain(
            calls
                .iter()
                .map(|(method, arguments)| CallbackMethodCall {
                    method: (*method).into(),
                    arguments: arguments.to_vec().into_boxed_slice(),
                })
                .collect(),
        )
    }

    fn mini(source: &str, offset: i64) -> CallbackArgument {
        CallbackArgument::Mini {
            source: source.into(),
            offset,
        }
    }

    fn transform(source: &str, candidate: PatternTransformCandidate) -> CallbackArgument {
        CallbackArgument::Transform {
            source: source.into(),
            candidate: Box::new(candidate),
        }
    }

    #[test]
    fn recognises_direct_identity_arrows() {
        for source in [
            "x => x",
            "(pattern) => pattern",
            "(x)=>(x)",
            "(x => x)",
            "m => m",
        ] {
            assert_eq!(
                lower_pattern_transform_callback(source),
                Ok(PatternTransformCandidate::Identity),
                "{source}"
            );
        }
    }

    #[test]
    fn recognises_method_chains_with_literal_arguments() {
        use CallbackArgument::{Bool, Number, String};
        let cases: Vec<(&str, PatternTransformCandidate)> = vec![
            ("(x) => x.rev()", chain(&[("rev", &[])])),
            ("x => x.fast(2)", chain(&[("fast", &[Number(2.0)])])),
            ("x => x.gain(0.5)", chain(&[("gain", &[Number(0.5)])])),
            ("x => x.add(-12)", chain(&[("add", &[Number(-12.0)])])),
            ("x => x.add(-(3))", chain(&[("add", &[Number(-3.0)])])),
            (
                "y => y.fast(1, 2)",
                chain(&[("fast", &[Number(1.0), Number(2.0)])]),
            ),
            ("x => x.legato(true)", chain(&[("legato", &[Bool(true)])])),
            (
                "x => x.bbexpr('t*2')",
                chain(&[("bbexpr", &[String("t*2".into())])]),
            ),
            (
                "(x) => x.struct(m('[- x x]*4', 24))",
                chain(&[("struct", &[mini("[- x x]*4", 24)])]),
            ),
            ("x => x.s(m('bd', -1))", chain(&[("s", &[mini("bd", -1)])])),
            (
                "(x) => (x).fast(2).rev()",
                chain(&[("fast", &[Number(2.0)]), ("rev", &[])]),
            ),
            (
                "x => (x.fast(2)).rev()",
                chain(&[("fast", &[Number(2.0)]), ("rev", &[])]),
            ),
            (
                "x => x.s(m('supersaw', 3)).vib(5).fm(3).fmh(12).detune(m('<0.9@2 1.5 2>/3', 40))",
                chain(&[
                    ("s", &[mini("supersaw", 3)]),
                    ("vib", &[Number(5.0)]),
                    ("fm", &[Number(3.0)]),
                    ("fmh", &[Number(12.0)]),
                    ("detune", &[mini("<0.9@2 1.5 2>/3", 40)]),
                ]),
            ),
            ("m => m.rev()", chain(&[("rev", &[])])),
            (
                "x => x.fast((y) => y)",
                chain(&[(
                    "fast",
                    &[transform("(y) => y", PatternTransformCandidate::Identity)],
                )]),
            ),
            (
                "y => y.when(m('< 0 [0!3 1]>', 27), (x) => x.clip(0.75).ply(m('<2 2 2 4>', 40)))",
                chain(&[(
                    "when",
                    &[
                        mini("< 0 [0!3 1]>", 27),
                        transform(
                            "(x) => x.clip(0.75).ply(m('<2 2 2 4>', 40))",
                            PatternTransformCandidate::MethodChain(
                                vec![
                                    CallbackMethodCall {
                                        method: "clip".into(),
                                        arguments: vec![Number(0.75)].into_boxed_slice(),
                                    },
                                    CallbackMethodCall {
                                        method: "ply".into(),
                                        arguments: vec![mini("<2 2 2 4>", 40)].into_boxed_slice(),
                                    },
                                ]
                                .into_boxed_slice(),
                            ),
                        ),
                    ],
                )]),
            ),
            (
                "x => x.in(m('1 0', 7))",
                chain(&[("in", &[mini("1 0", 7)])]),
            ),
        ];
        for (source, expected) in cases {
            assert_eq!(
                lower_pattern_transform_callback(source),
                Ok(expected),
                "{source}"
            );
        }
    }

    #[test]
    fn negative_zero_keeps_its_sign() {
        let Ok(PatternTransformCandidate::MethodChain(calls)) =
            lower_pattern_transform_callback("x => x.add(-0)")
        else {
            panic!("a negated zero is a literal");
        };
        let [CallbackArgument::Number(value)] = &*calls[0].arguments else {
            panic!("one number argument");
        };
        assert_eq!(value.to_bits(), (-0.0f64).to_bits());
    }

    #[test]
    fn rejects_nearby_observable_or_dynamic_shapes() {
        use PatternTransformFallbackReason as Reason;
        for (source, reason) in [
            ("async x => x", Reason::AsyncArrow),
            ("async x => x.rev()", Reason::AsyncArrow),
            ("() => x", Reason::ParameterShape),
            ("(x, y) => x", Reason::ParameterShape),
            ("({ x }) => x", Reason::ParameterShape),
            ("(x = 1) => x", Reason::ParameterShape),
            ("(...x) => x", Reason::ParameterShape),
            ("x => y", Reason::BodyShape),
            ("x => x.rev", Reason::BodyShape),
            ("x => 1", Reason::BodyShape),
            ("x => (x.rev(), x)", Reason::BodyShape),
            ("x => { return x; }", Reason::BodyShape),
            ("x => { return x.rev(); }", Reason::BodyShape),
            ("x => x?.rev()", Reason::BodyShape),
            ("x => x.rev?.()", Reason::BodyShape),
            ("x => new x.rev()", Reason::BodyShape),
            ("function (x) { return x; }", Reason::ParseFailure),
            (
                "function f(x) { return x.rev(); }",
                Reason::NotArrowExpression,
            ),
            // A method's own source text is not a standalone expression.
            ("rev(x) { return x.rev(); }", Reason::ParseFailure),
            ("x => y.rev()", Reason::ReceiverShape),
            ("x => x.add.squeeze(1)", Reason::ReceiverShape),
            ("x => rev(x)", Reason::CalleeShape),
            ("x => x[\"rev\"]()", Reason::CalleeShape),
            ("x => x.fast(2)()", Reason::CalleeShape),
            ("x => x.fast(...[2])", Reason::ArgumentShape),
            ("x => x.fast(k)", Reason::ArgumentShape),
            ("x => x.fast(x)", Reason::ArgumentShape),
            ("x => x.fast(+2)", Reason::ArgumentShape),
            ("x => x.fast(2n)", Reason::ArgumentShape),
            ("x => x.fast(null)", Reason::ArgumentShape),
            ("x => x.fast(undefined)", Reason::ArgumentShape),
            ("x => x.fast([2])", Reason::ArgumentShape),
            ("x => x.fast({ a: 2 })", Reason::ArgumentShape),
            ("x => x.fast(`2`)", Reason::ArgumentShape),
            ("x => x.fast(`${k}`)", Reason::ArgumentShape),
            ("x => x.fast(tag`2`)", Reason::ArgumentShape),
            ("x => x.fast(1 + 1)", Reason::ArgumentShape),
            // A nested arrow that captures, blocks, or recurses too deep is
            // not one of the two shapes, so the whole chain stays QuickJS.
            ("x => x.fast((y) => z.rev())", Reason::ReceiverShape),
            ("x => x.fast((y) => { return y; })", Reason::BodyShape),
            ("x => x.fast(async (y) => y)", Reason::AsyncArrow),
            ("x => x.fast((y, z) => y)", Reason::ParameterShape),
            (
                "x => x.fast((y) => y.fast((z) => z.fast((w) => w.fast((v) => v))))",
                Reason::ArgumentShape,
            ),
            ("x => x.fast(sine.range(1, 2))", Reason::ArgumentShape),
            ("x => x.fast(mini('1 2'))", Reason::ArgumentShape),
            ("x => x.fast(m.call(null, '1 2', 3))", Reason::ArgumentShape),
            ("x => x.fast(m?.('1 2', 3))", Reason::ArgumentShape),
            ("x => x.fast('\\uD800')", Reason::ArgumentShape),
            ("x => x.fast(m('1 2'))", Reason::MiniCallShape),
            ("x => x.fast(m('1 2', 3, 4))", Reason::MiniCallShape),
            ("x => x.fast(m('1 2', 0.5))", Reason::MiniCallShape),
            ("x => x.fast(m('1 2', k))", Reason::MiniCallShape),
            ("x => x.fast(m(k, 3))", Reason::MiniCallShape),
            ("x => x.fast(m(...['1 2', 3]))", Reason::MiniCallShape),
            ("x => x.fast(m('\\uD800', 3))", Reason::MiniCallShape),
            (
                "x => x.fast(m('1', 9007199254740992))",
                Reason::MiniCallShape,
            ),
            ("m => m.fast(m('1 2', 3))", Reason::MiniCalleeShadowed),
        ] {
            assert_eq!(
                lower_pattern_transform_callback(source),
                Err(reason),
                "{source}"
            );
        }
    }

    #[test]
    fn rejects_unbounded_and_invalid_sources() {
        assert_eq!(
            lower_pattern_transform_callback(&"x".repeat(MAX_CALLBACK_IR_SOURCE_BYTES + 1)),
            Err(PatternTransformFallbackReason::SourceTooLarge)
        );
        assert_eq!(
            lower_pattern_transform_callback("x =>"),
            Err(PatternTransformFallbackReason::ParseFailure)
        );
        assert_eq!(
            lower_pattern_transform_callback("x => x; y => y"),
            Err(PatternTransformFallbackReason::NotArrowExpression)
        );
        let deep = format!("x => {}x{}", "(".repeat(600), ")".repeat(600));
        assert_eq!(
            lower_pattern_transform_callback(&deep),
            Err(PatternTransformFallbackReason::ParseFailure)
        );
    }
}
