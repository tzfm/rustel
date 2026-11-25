//! The outline of a score: its statements and calls as a tree, each node
//! with the form it has, so a host can say which parts the native tier
//! can take and which only the script engine runs.
//!
//! The outline reads the text of the score as written. It does not run
//! the transpiler's rewrites, so its places are those of the text.

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, ArrayExpressionElement, ArrowFunctionBody, BinaryOperator, BindingPattern,
    Expression, ObjectPropertyKind, Program, PropertyKey, Statement, UnaryOperator,
};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType, Span};
use std::collections::HashSet;

/// How deep the outline goes. A deeper part is one node with its text.
const MAX_DEPTH: u16 = 48;
/// The most characters of text a label keeps.
const LABEL_CHARS: usize = 72;

/// What a node of the outline is, for the question of who runs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutlineForm {
    /// A form the native reader takes: a binding, a lane, a literal, a
    /// name, an array, an object or an arrow.
    Read,
    /// A call of a name. `bound` when the score binds the name itself.
    Call { name: String, bound: bool },
    /// A method call of a name on what the node above it gives.
    Method { name: String },
    /// A form only the script engine runs: an operator, a loop, a
    /// template, and every other form the reader does not take.
    Script,
}

/// One node of the outline, in the order of the text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlineNode {
    /// How many nodes are above it.
    pub depth: u16,
    /// What the node shows: its name and its short arguments.
    pub label: String,
    /// The line of the score where it starts, counted from 1.
    pub line: u32,
    /// The byte of the score where it starts.
    pub start: u32,
    pub form: OutlineForm,
}

/// The outline of `source`, or the parser's message when the text does
/// not parse.
pub fn outline(source: &str) -> Result<Vec<OutlineNode>, String> {
    if let Err(error) = crate::check_nesting(source) {
        return Err(error.message);
    }
    crate::on_parse_stack(source, |source| {
        let allocator = Allocator::default();
        let parsed = Parser::new(&allocator, source, SourceType::mjs()).parse();
        if let Some(error) = parsed.diagnostics.first() {
            return Err(error.message.to_string());
        }
        if parsed.panicked {
            return Err("the score does not parse".to_owned());
        }
        let mut outline = Outline::new(source, &parsed.program);
        for statement in &parsed.program.body {
            outline.statement(statement, 0);
        }
        Ok(outline.nodes)
    })
    .unwrap_or_else(|| Err("the score does not parse".to_owned()))
}

struct Outline<'s> {
    source: &'s str,
    /// The names the score binds at its top level.
    bound: HashSet<&'s str>,
    /// The byte where each line starts.
    lines: Vec<u32>,
    nodes: Vec<OutlineNode>,
}

impl<'s> Outline<'s> {
    fn new(source: &'s str, program: &Program<'s>) -> Self {
        let mut bound = HashSet::new();
        for statement in &program.body {
            match statement {
                Statement::VariableDeclaration(declaration) => {
                    for declarator in &declaration.declarations {
                        if let BindingPattern::BindingIdentifier(name) = &declarator.id {
                            bound.insert(name.name.as_str());
                        }
                    }
                }
                Statement::FunctionDeclaration(function) => {
                    if let Some(name) = &function.id {
                        bound.insert(name.name.as_str());
                    }
                }
                _ => {}
            }
        }
        let lines = std::iter::once(0)
            .chain(
                source
                    .bytes()
                    .enumerate()
                    .filter(|(_, byte)| *byte == b'\n')
                    .map(|(at, _)| at as u32 + 1),
            )
            .collect();
        Self {
            source,
            bound,
            lines,
            nodes: Vec::new(),
        }
    }

    fn line(&self, span: Span) -> u32 {
        self.lines.partition_point(|start| *start <= span.start) as u32
    }

    /// The text of `span` on one line, cut to a label's length.
    fn text(&self, span: Span) -> String {
        let text = self
            .source
            .get(span.start as usize..span.end as usize)
            .unwrap_or_default();
        short(text)
    }

    fn push(&mut self, depth: u16, span: Span, label: String, form: OutlineForm) {
        let line = self.line(span);
        self.nodes.push(OutlineNode {
            depth,
            label,
            line,
            start: span.start,
            form,
        });
    }

    fn statement(&mut self, statement: &Statement<'s>, depth: u16) {
        match statement {
            Statement::ExpressionStatement(statement) => {
                self.expression(&statement.expression, depth);
            }
            Statement::LabeledStatement(labeled) => {
                let label = format!("{}:", labeled.label.name);
                self.push(depth, labeled.span, label, OutlineForm::Read);
                self.statement(&labeled.body, depth + 1);
            }
            Statement::VariableDeclaration(declaration) => {
                for declarator in &declaration.declarations {
                    let name = match &declarator.id {
                        BindingPattern::BindingIdentifier(name) => name.name.to_string(),
                        other => self.text(other.span()),
                    };
                    let label = format!("{} {name}", declaration.kind.as_str());
                    self.push(depth, declarator.span, label, OutlineForm::Read);
                    if let Some(init) = &declarator.init {
                        self.expression(init, depth + 1);
                    }
                }
            }
            Statement::FunctionDeclaration(function) => {
                let name = function.id.as_ref().map_or("", |name| name.name.as_str());
                let form = if function.r#async || function.generator {
                    OutlineForm::Script
                } else {
                    OutlineForm::Read
                };
                self.push(depth, function.span, format!("function {name}"), form);
            }
            Statement::EmptyStatement(_) => {}
            other => {
                let span = other.span();
                self.push(depth, span, self.text(span), OutlineForm::Script);
            }
        }
    }

    fn expression(&mut self, expression: &Expression<'s>, depth: u16) {
        if depth >= MAX_DEPTH {
            let span = expression.span();
            self.push(depth, span, self.text(span), OutlineForm::Read);
            return;
        }
        match expression.get_inner_expression() {
            Expression::CallExpression(_) | Expression::StaticMemberExpression(_) => {
                self.chain(expression.get_inner_expression(), depth);
            }
            Expression::ArrayExpression(array) => {
                self.push(depth, array.span, "[ ]".to_owned(), OutlineForm::Read);
                for element in &array.elements {
                    match element {
                        ArrayExpressionElement::SpreadElement(spread) => {
                            self.push(
                                depth + 1,
                                spread.span,
                                self.text(spread.span),
                                OutlineForm::Script,
                            );
                        }
                        ArrayExpressionElement::Elision(_) => {}
                        element => {
                            if let Some(element) = element.as_expression() {
                                self.expression(element, depth + 1);
                            }
                        }
                    }
                }
            }
            Expression::ObjectExpression(object) => {
                let read = object.properties.iter().all(|property| match property {
                    ObjectPropertyKind::ObjectProperty(property) => {
                        !property.computed && !property.method
                    }
                    ObjectPropertyKind::SpreadProperty(_) => true,
                });
                let form = if read {
                    OutlineForm::Read
                } else {
                    OutlineForm::Script
                };
                self.push(depth, object.span, self.text(object.span), form);
                for property in &object.properties {
                    if let ObjectPropertyKind::ObjectProperty(property) = property
                        && !simple(&property.value)
                    {
                        let key = match &property.key {
                            PropertyKey::StaticIdentifier(key) => key.name.to_string(),
                            key => self.text(key.span()),
                        };
                        self.push(
                            depth + 1,
                            property.span,
                            format!("{key}:"),
                            OutlineForm::Read,
                        );
                        self.expression(&property.value, depth + 2);
                    }
                }
            }
            Expression::ArrowFunctionExpression(arrow) => {
                let params = self.text(arrow.params.span);
                let form = if arrow.r#async {
                    OutlineForm::Script
                } else {
                    OutlineForm::Read
                };
                self.push(depth, arrow.span, format!("{params} =>"), form);
                match &arrow.body {
                    ArrowFunctionBody::FunctionBody(body) => {
                        for statement in &body.statements {
                            self.statement(statement, depth + 1);
                        }
                    }
                    result => {
                        if let Some(result) = result.as_expression() {
                            self.expression(result, depth + 1);
                        }
                    }
                }
            }
            Expression::UnaryExpression(unary)
                if unary.operator == UnaryOperator::UnaryNegation
                    && matches!(unary.argument, Expression::NumericLiteral(_)) =>
            {
                self.push(depth, unary.span, self.text(unary.span), OutlineForm::Read);
            }
            other => {
                let form = if simple(other) {
                    OutlineForm::Read
                } else {
                    OutlineForm::Script
                };
                let span = other.span();
                self.push(depth, span, self.text(span), form);
            }
        }
    }

    /// A chain of calls and members, one node for each link at the same
    /// depth, from the first to the last: `s("bd").fast(2)` is `s "bd"`
    /// then `.fast 2`.
    fn chain(&mut self, expression: &Expression<'s>, depth: u16) {
        let mut links = Vec::new();
        let mut at = expression;
        loop {
            match at {
                Expression::CallExpression(call) => match call.callee.get_inner_expression() {
                    Expression::StaticMemberExpression(member) if !member.optional => {
                        links.push(Link::Method {
                            name: member.property.name.as_str(),
                            arguments: Some(&call.arguments),
                            span: call.span,
                        });
                        at = member.object.get_inner_expression();
                    }
                    Expression::Identifier(name) if !call.optional => {
                        links.push(Link::Call {
                            name: name.name.as_str(),
                            arguments: &call.arguments,
                            span: call.span,
                        });
                        break;
                    }
                    // `f()(1)` or `a[0](1)`: shown as script. Handing the
                    // call back to `expression` would loop forever.
                    _ => {
                        links.push(Link::Script(call.span));
                        break;
                    }
                },
                Expression::StaticMemberExpression(member) if !member.optional => {
                    links.push(Link::Method {
                        name: member.property.name.as_str(),
                        arguments: None,
                        span: member.span,
                    });
                    at = member.object.get_inner_expression();
                }
                other => {
                    links.push(Link::Other(other));
                    break;
                }
            }
        }
        for link in links.into_iter().rev() {
            match link {
                Link::Call {
                    name,
                    arguments,
                    span,
                } => {
                    let form = OutlineForm::Call {
                        name: name.to_owned(),
                        bound: self.bound.contains(name),
                    };
                    self.call(name.to_owned(), arguments, span, form, depth);
                }
                Link::Method {
                    name,
                    arguments: Some(arguments),
                    span,
                } => {
                    let form = OutlineForm::Method {
                        name: name.to_owned(),
                    };
                    self.call(format!(".{name}"), arguments, span, form, depth);
                }
                Link::Method {
                    name,
                    arguments: None,
                    span,
                } => {
                    self.push(depth, span, format!(".{name}"), OutlineForm::Read);
                }
                Link::Other(expression) => self.expression(expression, depth),
                Link::Script(span) => {
                    self.push(depth, span, self.text(span), OutlineForm::Script);
                }
            }
        }
    }

    /// A call node: its name and its short arguments on its line, and a
    /// child for each argument with parts of its own.
    fn call(
        &mut self,
        name: String,
        arguments: &oxc_allocator::Vec<'s, Argument<'s>>,
        span: Span,
        form: OutlineForm,
        depth: u16,
    ) {
        let mut label = name;
        for argument in arguments {
            if let Some(argument) = argument.as_expression()
                && simple(argument)
            {
                label.push(' ');
                label.push_str(&self.text(argument.span()));
            }
        }
        self.push(depth, span, short(&label), form);
        for argument in arguments {
            match argument {
                Argument::SpreadElement(spread) => {
                    self.push(
                        depth + 1,
                        spread.span,
                        self.text(spread.span),
                        OutlineForm::Script,
                    );
                }
                argument => {
                    if let Some(argument) = argument.as_expression()
                        && !simple(argument)
                    {
                        self.expression(argument, depth + 1);
                    }
                }
            }
        }
    }
}

/// One link of a chain of calls.
enum Link<'a, 's> {
    Call {
        name: &'s str,
        arguments: &'a oxc_allocator::Vec<'s, Argument<'s>>,
        span: Span,
    },
    /// A method call, or a member read with no call.
    Method {
        name: &'s str,
        arguments: Option<&'a oxc_allocator::Vec<'s, Argument<'s>>>,
        span: Span,
    },
    /// The first link when it is not a call of a name.
    Other(&'a Expression<'s>),
    /// A first call whose callee is no name or member, as text.
    Script(Span),
}

/// A literal or a name: shown on its call's line, with no node of its own.
fn simple(expression: &Expression<'_>) -> bool {
    if number(expression) {
        return true;
    }
    match expression.get_inner_expression() {
        Expression::NumericLiteral(_)
        | Expression::StringLiteral(_)
        | Expression::BooleanLiteral(_)
        | Expression::NullLiteral(_)
        | Expression::Identifier(_) => true,
        Expression::UnaryExpression(unary) => {
            unary.operator == UnaryOperator::UnaryNegation
                && matches!(unary.argument, Expression::NumericLiteral(_))
        }
        _ => false,
    }
}

/// A number, or arithmetic of numbers, which the reader works out as it
/// reads.
fn number(expression: &Expression<'_>) -> bool {
    match expression.get_inner_expression() {
        Expression::NumericLiteral(_) => true,
        Expression::UnaryExpression(unary) => {
            unary.operator == UnaryOperator::UnaryNegation && number(&unary.argument)
        }
        Expression::BinaryExpression(binary) => {
            matches!(
                binary.operator,
                BinaryOperator::Addition
                    | BinaryOperator::Subtraction
                    | BinaryOperator::Multiplication
                    | BinaryOperator::Division
            ) && number(&binary.left)
                && number(&binary.right)
        }
        _ => false,
    }
}

/// `text` on one line, its runs of space made one, and cut short.
fn short(text: &str) -> String {
    let mut line = String::with_capacity(text.len().min(LABEL_CHARS + 3));
    for word in text.split_whitespace() {
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
        if line.chars().count() > LABEL_CHARS {
            break;
        }
    }
    if line.chars().count() > LABEL_CHARS {
        line = line.chars().take(LABEL_CHARS).collect();
        line.push('…');
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(source: &str) -> Vec<String> {
        outline(source)
            .expect("an outline")
            .iter()
            .map(|node| {
                let form = match &node.form {
                    OutlineForm::Read => "read".to_owned(),
                    OutlineForm::Call { name, bound } => format!("call {name} {bound}"),
                    OutlineForm::Method { name } => format!("method {name}"),
                    OutlineForm::Script => "script".to_owned(),
                };
                format!(
                    "{}{} @{} ({form})",
                    "  ".repeat(node.depth.into()),
                    node.label,
                    node.line
                )
            })
            .collect()
    }

    #[test]
    fn a_lane_shows_its_chain_and_its_arguments() {
        assert_eq!(
            lines("$: s(\"bd sd\").fast(2)\n  .room(.3)"),
            [
                "$: @1 (read)",
                "  s \"bd sd\" @1 (call s false)",
                "  .fast 2 @1 (method fast)",
                "  .room .3 @1 (method room)",
            ]
        );
    }

    #[test]
    fn a_nested_pattern_is_a_child_of_its_call() {
        assert_eq!(
            lines("const k = 1\nstack(\n  s(\"bd\"),\n  note(\"c\").s(k)\n)"),
            [
                "const k @1 (read)",
                "  1 @1 (read)",
                "stack @2 (call stack false)",
                "  s \"bd\" @3 (call s false)",
                "  note \"c\" @4 (call note false)",
                "  .s k @4 (method s)",
            ]
        );
    }

    #[test]
    fn script_forms_and_bound_names_are_said() {
        assert_eq!(
            lines("function f(x) { return x }\nfor (const a of b) {}\nf(x + 1)"),
            [
                "function f @1 (read)",
                "for (const a of b) {} @2 (script)",
                "f @3 (call f true)",
                "  x + 1 @3 (script)",
            ]
        );
    }

    #[test]
    fn an_arrow_shows_its_body() {
        assert_eq!(
            lines("n(\"0\").every(2, x => x.rev())"),
            [
                "n \"0\" @1 (call n false)",
                ".every 2 @1 (method every)",
                "  x => @1 (read)",
                "    x @1 (read)",
                "    .rev @1 (method rev)",
            ]
        );
    }

    #[test]
    fn arithmetic_of_numbers_is_read() {
        assert_eq!(
            lines("s(\"hh\").gain(1 + 0.2 * -2)"),
            [
                "s \"hh\" @1 (call s false)",
                ".gain 1 + 0.2 * -2 @1 (method gain)",
            ]
        );
    }

    #[test]
    fn a_long_label_is_cut_short() {
        let long = format!("note(\"{}\")", "c ".repeat(60));
        let label = &outline(&long).expect("an outline")[0].label;
        assert!(label.ends_with('…'), "{label}");
    }

    #[test]
    fn a_call_of_a_call_is_script() {
        assert_eq!(
            lines("foo()(1).fast(2)\na[0](1)"),
            [
                "foo()(1) @1 (script)",
                ".fast 2 @1 (method fast)",
                "a[0](1) @2 (script)",
            ]
        );
    }

    #[test]
    fn a_text_that_does_not_parse_says_so() {
        assert!(outline("s(\"bd\"").is_err());
    }
}
