//! Conservative AST recognition for callback-IR candidates.
//!
//! The runtime asks this module about the source returned by the realm's
//! captured `Function.prototype.toString`.  Recognition is deliberately
//! fail-closed: a function that is not one exact supported expression remains
//! an ordinary QuickJS callback.

use oxc_allocator::Allocator;
use oxc_ast::ast::{ArrowFunctionBody, BindingPattern, Expression, Statement};
use oxc_parser::Parser;
use oxc_span::SourceType;

/// Parsing one callback again is bounded independently from the complete
/// score. Real callback expressions are tiny; a larger function remains on the
/// compatibility tier instead of adding unbounded per-callback parse work.
pub const MAX_CALLBACK_IR_SOURCE_BYTES: usize = 4 * 1024;

/// A callback shape whose semantics have a native IR representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PatternTransformCandidate {
    /// One ordinary parameter returned directly: `pattern => pattern`.
    Identity,
}

/// Why source stayed on the complete QuickJS compatibility tier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PatternTransformFallbackReason {
    SourceTooLarge,
    ObviousDynamicSyntax,
    ParseFailure,
    NotArrowExpression,
    AsyncArrow,
    ParameterShape,
    BodyShape,
}

/// Recognise one bounded, capture-free `Pattern -> Pattern` expression.
///
/// This uses the same ES parser as score transpilation. It intentionally does
/// not infer purity from text matching or accept a block body that merely
/// looks equivalent: every newly admitted shape gets its own semantic proof.
pub fn lower_pattern_transform_callback(
    source: &str,
) -> Result<PatternTransformCandidate, PatternTransformFallbackReason> {
    if source.len() > MAX_CALLBACK_IR_SOURCE_BYTES {
        return Err(PatternTransformFallbackReason::SourceTooLarge);
    }
    // A direct identifier return cannot contain member-access punctuation.
    // This is only a fail-closed rejection filter: comments can make it reject
    // a semantically eligible callback, which merely preserves QuickJS. The
    // filter never admits code; every native candidate still comes from the
    // complete AST proof below.
    if source.as_bytes().contains(&b'.') {
        return Err(PatternTransformFallbackReason::ObviousDynamicSyntax);
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
    {
        return Err(PatternTransformFallbackReason::ParameterShape);
    }
    let BindingPattern::BindingIdentifier(parameter) = &parameter.pattern else {
        return Err(PatternTransformFallbackReason::ParameterShape);
    };
    let body = match &arrow.body {
        ArrowFunctionBody::FunctionBody(_) => {
            return Err(PatternTransformFallbackReason::BodyShape);
        }
        body => body.to_expression().without_parentheses(),
    };
    let Expression::Identifier(body) = body else {
        return Err(PatternTransformFallbackReason::BodyShape);
    };
    if body.name != parameter.name {
        return Err(PatternTransformFallbackReason::BodyShape);
    }
    Ok(PatternTransformCandidate::Identity)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_only_direct_identity_arrows() {
        for source in ["x => x", "(pattern) => pattern", "(x)=>(x)"] {
            assert_eq!(
                lower_pattern_transform_callback(source),
                Ok(PatternTransformCandidate::Identity),
                "{source}"
            );
        }
    }

    #[test]
    fn rejects_nearby_observable_or_dynamic_shapes() {
        for (source, reason) in [
            ("async x => x", PatternTransformFallbackReason::AsyncArrow),
            ("() => x", PatternTransformFallbackReason::ParameterShape),
            (
                "(x, y) => x",
                PatternTransformFallbackReason::ParameterShape,
            ),
            (
                "({ x }) => x",
                PatternTransformFallbackReason::ParameterShape,
            ),
            (
                "(x = 1) => x",
                PatternTransformFallbackReason::ParameterShape,
            ),
            ("x => y", PatternTransformFallbackReason::BodyShape),
            (
                "x => x.rev()",
                PatternTransformFallbackReason::ObviousDynamicSyntax,
            ),
            (
                "x => { return x; }",
                PatternTransformFallbackReason::BodyShape,
            ),
            (
                "function (x) { return x; }",
                PatternTransformFallbackReason::ParseFailure,
            ),
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
        assert_eq!(
            lower_pattern_transform_callback("x /* conservative . filter */ => x"),
            Err(PatternTransformFallbackReason::ObviousDynamicSyntax)
        );
    }
}
