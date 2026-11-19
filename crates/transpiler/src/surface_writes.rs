//! What a text can write on the surface that compiled callbacks read.
//!
//! The runtime compiles a callback such as `x => x.rev()` against the
//! built-in `rev`. The compiled program stays exact only while nothing
//! replaces what the callback would read when it runs: the `rev` slot of the
//! Pattern prototype, the global `m` that builds mini-notation, and the few
//! intrinsics the built-in methods read. Before a text runs, the runtime asks
//! this module which of those the text can write, and closes compilation for
//! each of them.
//!
//! The report over-approximates. JavaScript writes a named property only
//! through a member assignment, update or `delete`, through a class field,
//! which a base constructor that returns another object can define on any
//! object, through the reflection capabilities of `Object`, `Reflect` and the
//! `__define*__` methods, or through a host function. A name that a member
//! write or a class field spells statically is listed, on whatever object
//! the text reaches. The host `register` is listed only when the text calls
//! it directly, by its global name or as a member of a name for the global
//! object. Every other way to write, and every way to reach code that no
//! report covers, such as `eval`, `Function`, `register` reached any other
//! way or a computed property key, makes the report unbounded.

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    AccessorProperty, Argument, ArrayExpressionElement, AssignmentTargetPropertyIdentifier,
    AssignmentTargetPropertyProperty, BindingProperty, CallExpression, ComputedMemberExpression,
    Expression, Function, IdentifierReference, ImportExpression, Program, PropertyDefinition,
    PropertyKey, SimpleAssignmentTarget, StaticMemberExpression, ThisExpression, UnaryExpression,
    UnaryOperator, WithStatement,
};
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use oxc_semantic::{Scoping, SemanticBuilder};
use oxc_span::SourceType;
use oxc_syntax::scope::ScopeFlags;
use oxc_syntax::symbol::SymbolFlags;

/// What one text can write. See the module documentation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SurfaceWrites {
    /// The text can write a name this report does not list.
    pub unbounded: bool,
    /// Property names the text can assign or delete, on any object. A
    /// `register` call lists the names it defines.
    pub member_names: Vec<String>,
    /// Global variables the text can assign or delete, and the names it
    /// declares at its top level.
    pub global_names: Vec<String>,
    /// The text declares `m`, the callee of every mini-notation literal, in
    /// some scope.
    pub declares_mini_callee: bool,
}

/// Names whose appearance anywhere gives a text an unlisted way to write:
/// reflection, code evaluation, and the host functions that change how
/// strings or registrations behave.
const CAPABILITIES: &[&str] = &[
    "eval",
    "Function",
    "Object",
    "Reflect",
    "constructor",
    "__proto__",
    "__defineGetter__",
    "__defineSetter__",
    "setStringParser",
    "registerCanonical",
    "registerRawOnly",
    "worklet",
];

/// Names that hold the global object or the score scope. A text may only
/// test their type or read or write one named member of them. The runtime
/// tests that every global that holds the global object is listed.
const HOLDERS: &[&str] = &[
    "globalThis",
    "window",
    "self",
    "rustelScope",
    "strudelScope",
];

/// The host function that defines a Pattern method by name.
const REGISTER: &str = "register";

/// The callee of every mini-notation literal the transpiler writes.
const MINI_CALLEE: &str = "m";

/// Parse a text the runtime evaluates as written and report its writes.
///
/// A text that does not parse cleanly is reported unbounded.
pub fn surface_writes(source: &str) -> SurfaceWrites {
    let unbounded = || SurfaceWrites {
        unbounded: true,
        ..SurfaceWrites::default()
    };
    if crate::check_nesting(source).is_err() {
        return unbounded();
    }
    crate::on_parse_stack(|| {
        let allocator = Allocator::default();
        let parsed = Parser::new(&allocator, source, SourceType::mjs()).parse();
        if parsed.panicked || !parsed.diagnostics.is_empty() {
            return unbounded();
        }
        let semantic = SemanticBuilder::new().build(&parsed.program).semantic;
        program_surface_writes(&parsed.program, semantic.scoping())
    })
    .unwrap_or_else(unbounded)
}

/// Report the writes of a parsed program whose scopes are already resolved.
pub(crate) fn program_surface_writes(program: &Program<'_>, scoping: &Scoping) -> SurfaceWrites {
    let mut collector = WriteCollector {
        scoping,
        writes: SurfaceWrites::default(),
        functions: 0,
    };
    let root = scoping.root_scope_id();
    for symbol in scoping.symbol_ids() {
        let name = scoping.symbol_name(symbol);
        if name == MINI_CALLEE {
            collector.writes.declares_mini_callee = true;
        }
        // A script's top-level declarations are properties of the global
        // object or bindings that every later script sees. A function
        // declared in a block of a script that is not strict also binds the
        // name in the enclosing function, or on the global object at the top.
        let scope = scoping.symbol_scope_id(symbol);
        let block_function = scoping.symbol_flags(symbol).contains(SymbolFlags::Function)
            && !scoping.scope_flags(scope).is_var();
        if scope == root || block_function {
            collector.writes.global_names.push(name.to_owned());
        }
    }
    collector.visit_program(program);
    collector.writes
}

struct WriteCollector<'s> {
    scoping: &'s Scoping,
    writes: SurfaceWrites,
    /// Depth of ordinary functions. Outside every function, `this` can be
    /// the global object.
    functions: usize,
}

impl WriteCollector<'_> {
    fn is_global(&self, identifier: &IdentifierReference<'_>) -> bool {
        identifier
            .reference_id
            .get()
            .is_none_or(|reference| self.scoping.get_reference(reference).symbol_id().is_none())
    }

    fn unbounded(&mut self) {
        self.writes.unbounded = true;
    }

    /// Whether `expression` is a name for the global object or the score
    /// scope.
    fn is_holder(&self, expression: &Expression<'_>) -> bool {
        match expression.without_parentheses() {
            Expression::Identifier(identifier) => {
                HOLDERS.contains(&identifier.name.as_str()) && self.is_global(identifier)
            }
            Expression::ThisExpression(_) => self.functions == 0,
            _ => false,
        }
    }

    /// Whether `expression` names the host `register`: the global, or a
    /// member of a holder.
    fn is_register(&self, expression: &Expression<'_>) -> bool {
        match expression.without_parentheses() {
            Expression::Identifier(identifier) => {
                identifier.name == REGISTER && self.is_global(identifier)
            }
            Expression::StaticMemberExpression(member) => {
                member.property.name == REGISTER && self.is_holder(&member.object)
            }
            _ => false,
        }
    }

    /// A member read, write or destructuring key named `name`, on any
    /// object. A direct `register` call on a holder is taken before the walk
    /// reaches its callee.
    fn check_member_name(&mut self, name: &str) {
        if CAPABILITIES.contains(&name) || name == REGISTER {
            self.unbounded();
        }
    }

    /// A class field defines its key on the object the constructor's `this`
    /// is, which a base constructor that returns another object can choose.
    fn write_class_key(&mut self, key: &PropertyKey<'_>) {
        if matches!(key, PropertyKey::PrivateIdentifier(_)) {
            return;
        }
        match key.static_name() {
            Some(name) => {
                self.check_member_name(&name);
                self.write_member(&name);
            }
            None => self.unbounded(),
        }
    }

    /// A destructuring key reads the named member. A computed key reads a
    /// member no report can name.
    fn check_key(&mut self, key: &PropertyKey<'_>) {
        match key.static_name() {
            Some(name) => self.check_member_name(&name),
            None if matches!(key, PropertyKey::PrivateIdentifier(_)) => {}
            None => self.unbounded(),
        }
    }

    fn write_member(&mut self, name: &str) {
        self.writes.member_names.push(name.to_owned());
    }

    fn write_global(&mut self, identifier: &IdentifierReference<'_>) {
        if self.is_global(identifier) {
            self.writes.global_names.push(identifier.name.to_string());
        }
    }

    /// The member a computed key names, when it is a literal.
    fn literal_key(member: &ComputedMemberExpression<'_>) -> Option<String> {
        match member.expression.without_parentheses() {
            Expression::StringLiteral(literal) => Some(literal.value.to_string()),
            Expression::NumericLiteral(literal) => Some(literal.value.to_string()),
            _ => None,
        }
    }

    fn write_computed(&mut self, member: &ComputedMemberExpression<'_>) {
        match Self::literal_key(member) {
            Some(name) => self.write_member(&name),
            None => self.unbounded(),
        }
    }

    /// The names a `register` call defines on the Pattern prototype: each
    /// name and its raw `_name` form.
    fn register_names(&mut self, call: &CallExpression<'_>) {
        let define = |collector: &mut Self, name: &str| {
            collector.write_member(name);
            collector.write_member(&format!("_{name}"));
        };
        match call.arguments.first() {
            // The call throws before it defines anything.
            None => {}
            Some(Argument::StringLiteral(literal)) => define(self, literal.value.as_str()),
            Some(Argument::TemplateLiteral(template)) => match template.single_quasi() {
                Some(name) => define(self, name.as_str()),
                None => self.unbounded(),
            },
            Some(Argument::ArrayExpression(array)) => {
                for element in &array.elements {
                    match element {
                        ArrayExpressionElement::StringLiteral(literal) => {
                            define(self, literal.value.as_str());
                        }
                        ArrayExpressionElement::TemplateLiteral(template) => {
                            match template.single_quasi() {
                                Some(name) => define(self, name.as_str()),
                                None => self.unbounded(),
                            }
                        }
                        _ => self.unbounded(),
                    }
                }
            }
            Some(_) => self.unbounded(),
        }
    }
}

impl<'a> Visit<'a> for WriteCollector<'_> {
    fn visit_identifier_reference(&mut self, identifier: &IdentifierReference<'a>) {
        if !self.is_global(identifier) {
            return;
        }
        let name = identifier.name.as_str();
        // Every allowed use of a holder or of `register` is taken before the
        // walk reaches the bare name.
        if CAPABILITIES.contains(&name) || HOLDERS.contains(&name) || name == REGISTER {
            self.unbounded();
        }
    }

    fn visit_this_expression(&mut self, _this: &ThisExpression) {
        if self.functions == 0 {
            self.unbounded();
        }
    }

    fn visit_function(&mut self, function: &Function<'a>, flags: ScopeFlags) {
        self.functions += 1;
        walk::walk_function(self, function, flags);
        self.functions -= 1;
    }

    fn visit_static_member_expression(&mut self, member: &StaticMemberExpression<'a>) {
        let name = member.property.name.as_str();
        self.check_member_name(name);
        if self.is_holder(&member.object) {
            if HOLDERS.contains(&name) || name == REGISTER {
                self.unbounded();
            }
            // One named member of a holder: the holder itself is not
            // visited as a bare name.
            return;
        }
        walk::walk_static_member_expression(self, member);
    }

    fn visit_computed_member_expression(&mut self, member: &ComputedMemberExpression<'a>) {
        match Self::literal_key(member) {
            Some(name) => self.check_member_name(&name),
            None => self.unbounded(),
        }
        walk::walk_computed_member_expression(self, member);
    }

    fn visit_simple_assignment_target(&mut self, target: &SimpleAssignmentTarget<'a>) {
        match target {
            SimpleAssignmentTarget::AssignmentTargetIdentifier(identifier) => {
                self.write_global(identifier);
            }
            SimpleAssignmentTarget::StaticMemberExpression(member) => {
                self.write_member(member.property.name.as_str());
            }
            SimpleAssignmentTarget::ComputedMemberExpression(member) => {
                self.write_computed(member);
            }
            SimpleAssignmentTarget::PrivateFieldExpression(_) => {}
            _ => self.unbounded(),
        }
        walk::walk_simple_assignment_target(self, target);
    }

    fn visit_assignment_target_property_identifier(
        &mut self,
        property: &AssignmentTargetPropertyIdentifier<'a>,
    ) {
        self.check_member_name(property.binding.name.as_str());
        self.write_global(&property.binding);
        walk::walk_assignment_target_property_identifier(self, property);
    }

    fn visit_assignment_target_property_property(
        &mut self,
        property: &AssignmentTargetPropertyProperty<'a>,
    ) {
        self.check_key(&property.name);
        walk::walk_assignment_target_property_property(self, property);
    }

    fn visit_binding_property(&mut self, property: &BindingProperty<'a>) {
        self.check_key(&property.key);
        walk::walk_binding_property(self, property);
    }

    fn visit_unary_expression(&mut self, unary: &UnaryExpression<'a>) {
        match unary.operator {
            UnaryOperator::Typeof if self.is_holder(&unary.argument) => return,
            UnaryOperator::Delete => match unary.argument.without_parentheses() {
                Expression::StaticMemberExpression(member) => {
                    self.write_member(member.property.name.as_str());
                }
                Expression::ComputedMemberExpression(member) => self.write_computed(member),
                Expression::Identifier(identifier) => self.write_global(identifier),
                _ => {}
            },
            _ => {}
        }
        walk::walk_unary_expression(self, unary);
    }

    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        if self.is_register(&call.callee) {
            self.register_names(call);
            self.visit_arguments(&call.arguments);
            return;
        }
        walk::walk_call_expression(self, call);
    }

    fn visit_property_definition(&mut self, property: &PropertyDefinition<'a>) {
        self.write_class_key(&property.key);
        walk::walk_property_definition(self, property);
    }

    fn visit_accessor_property(&mut self, property: &AccessorProperty<'a>) {
        self.write_class_key(&property.key);
        walk::walk_accessor_property(self, property);
    }

    fn visit_import_expression(&mut self, expression: &ImportExpression<'a>) {
        self.unbounded();
        walk::walk_import_expression(self, expression);
    }

    fn visit_with_statement(&mut self, statement: &WithStatement<'a>) {
        self.unbounded();
        walk::walk_with_statement(self, statement);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounded(member_names: &[&str], global_names: &[&str]) -> SurfaceWrites {
        SurfaceWrites {
            unbounded: false,
            member_names: member_names.iter().map(|name| (*name).to_owned()).collect(),
            global_names: global_names.iter().map(|name| (*name).to_owned()).collect(),
            declares_mini_callee: false,
        }
    }

    #[test]
    fn a_score_without_writes_is_bounded_and_empty() {
        for source in [
            r#"n(m("0 1", 2)).sometimes(x => x.rev()).every(4, x => x.add(12))"#,
            "s('bd*4').layer(x => x.struct(m('[- x x]*4', 9))).gain(sine.range(0.2, 0.8))",
            "const pick = { a: s('bd') }; pick.a",
            "typeof globalThis === 'object' ? globalThis.s('bd') : silence",
            "function f(p) { return this.x ?? p; } f(1)",
            "const { a, b: [c] } = { a: 1, b: [2] }; a + c",
            "arr[0]; arr['length']",
        ] {
            let writes = surface_writes(source);
            assert!(!writes.unbounded, "{source}: {writes:?}");
            assert!(writes.member_names.is_empty(), "{source}: {writes:?}");
            assert!(!writes.declares_mini_callee, "{source}");
        }
    }

    #[test]
    fn static_member_and_global_writes_are_listed() {
        for (source, members, globals) in [
            ("Pattern.prototype.rev = x => x", &["rev"][..], &[][..]),
            (
                "p.fast += 1; p.slow++; --p.ply",
                &["fast", "slow", "ply"],
                &[],
            ),
            ("delete Pattern.prototype.rev", &["rev"], &[]),
            ("p['rev'] = 1; p[0] = 2", &["rev", "0"], &[]),
            ("[a.b, ...c.d] = [1, 2]", &["b", "d"], &[]),
            ("({ k: a.b } = o)", &["b"], &[]),
            ("for (a.b of xs) {}", &["b"], &[]),
            ("globalThis.m = 1; window.k = 2", &["m", "k"], &[]),
            ("this.k = 1", &["k"], &[]),
            ("m = 1; z ||= 2", &[], &["m", "z"]),
            ("({ q } = o)", &[], &["q"]),
            ("a.b ??= 1", &["b"], &[]),
        ] {
            assert_eq!(
                surface_writes(source),
                bounded(members, globals),
                "{source}"
            );
        }
    }

    #[test]
    fn top_level_declarations_are_global_names() {
        assert_eq!(
            surface_writes("let rev = 1; function Array() {} { var k = 2 }"),
            bounded(&[], &["rev", "Array", "k"])
        );
        assert_eq!(
            surface_writes("function f() { let local = 1; return local }"),
            bounded(&[], &["f"])
        );
    }

    #[test]
    fn a_function_declared_in_a_block_is_a_global_name() {
        // A script that is not strict binds it on the global object.
        assert_eq!(
            surface_writes("{ function Array() {} } if (k) { function m2() {} }"),
            bounded(&[], &["Array", "m2"])
        );
        assert_eq!(
            surface_writes("function f() { { function g() {} } }"),
            bounded(&[], &["f", "g"])
        );
        assert_eq!(
            surface_writes("function f() { function g() {} } (function h() {})"),
            bounded(&[], &["f"])
        );
    }

    #[test]
    fn class_fields_are_member_writes_on_any_object() {
        assert_eq!(
            surface_writes(
                "class S extends B { rev = 1; static fast = 2; #own = 3; 'add' = 4; [`ply`] = 5; m; isArray = 6 }"
            ),
            bounded(&["rev", "fast", "add", "ply", "m", "isArray"], &["S"])
        );
        // A method is defined on a prototype the class creates.
        assert_eq!(
            surface_writes("class S { rev() {} static fast() {} get add() { return 1 } }"),
            bounded(&[], &["S"])
        );
        for source in [
            "class S { [key] = 1 }",
            "class S { Object = 1 }",
            "class S { register = 1 }",
            "(class { __proto__ = 1 })",
        ] {
            assert!(surface_writes(source).unbounded, "{source}");
        }
    }

    #[test]
    fn declarations_of_the_mini_callee_are_reported_in_any_scope() {
        for source in [
            "const m = 1",
            "function f(m) { return m }",
            "x => { let m = 2; return x }",
            "try {} catch (m) {}",
            "class m {}",
            "(function m() {})",
        ] {
            assert!(surface_writes(source).declares_mini_callee, "{source}");
        }
        assert!(!surface_writes("m('a', 1); k.m").declares_mini_callee);
    }

    #[test]
    fn register_calls_list_the_names_they_define() {
        for (source, members) in [
            ("register('rev', (p) => p)", &["rev", "_rev"][..]),
            ("register(`fast`, (a, p) => p)", &["fast", "_fast"]),
            ("register(['a', `b`], (p) => p)", &["a", "_a", "b", "_b"]),
            ("globalThis.register('k', (p) => p)", &["k", "_k"]),
            ("rustelScope.register('k', (p) => p)", &["k", "_k"]),
            ("strudelScope.register('k', (p) => p)", &["k", "_k"]),
            ("window.register('k', (p) => p)", &["k", "_k"]),
        ] {
            assert_eq!(surface_writes(source), bounded(members, &[]), "{source}");
        }
        for source in [
            "register(name, (p) => p)",
            "register(`a${b}`, (p) => p)",
            "register(['a', name], (p) => p)",
            "const r = register",
            "register.call(null, 'k', (p) => p)",
            "globalThis.register",
            "new register('k', (p) => p)",
            "({ register })",
            // Any other object can be the global object.
            "g.register('k', (p) => p)",
            "globalThis.valueOf().register('k', (p) => p)",
            "strudelScope['register']('k', (p) => p)",
            "const { register: r } = strudelScope",
            "({ register: r } = g)",
            "function f() { return this.register('k', (p) => p) }",
        ] {
            assert!(surface_writes(source).unbounded, "{source}");
        }
        // A local function of the same name is not the host's.
        assert_eq!(
            surface_writes("function g(register) { register('k') }"),
            bounded(&[], &["g"])
        );
    }

    #[test]
    fn capabilities_and_dynamic_access_are_unbounded() {
        for source in [
            "eval('1')",
            "new Function('return 1')",
            "Object.keys({})",
            "Reflect.set(o, 'k', 1)",
            "x.constructor",
            "x.__proto__",
            "x['constructor']",
            "x.__defineSetter__('k', f)",
            "setStringParser(mini)",
            "registerCanonical('k', f)",
            "x.worklet('a')",
            "import('a')",
            "o[k]",
            "o[k] = 1",
            "o[`k`]",
            "delete o[k]",
            "const { constructor: c } = []",
            "({ constructor } = [])",
            "const { [k]: v } = o",
            "({ [k]: v } = o)",
            "with (o) { k }",
        ] {
            assert!(surface_writes(source).unbounded, "{source}");
        }
        // A local binding of a capability name is not the capability.
        assert!(!surface_writes("function f(Function) { return Function }").unbounded);
    }

    #[test]
    fn holders_may_only_be_tested_or_read_by_one_member() {
        for source in [
            "typeof globalThis",
            "globalThis.s('bd')",
            "window.k",
            "rustelScope.note",
            "typeof this",
            "this.k",
        ] {
            assert!(!surface_writes(source).unbounded, "{source}");
        }
        for source in [
            "const g = globalThis",
            "f(window)",
            "globalThis.globalThis",
            "window.Object",
            "self['k']",
            "const t = this",
            "globalThis = 1",
            "globalThis?.window",
        ] {
            assert!(surface_writes(source).unbounded, "{source}");
        }
        // Inside an ordinary function, `this` is not the global object.
        assert!(!surface_writes("function f() { const t = this; return t }").unbounded);
    }

    #[test]
    fn transpile_reports_the_writes_of_the_code_that_runs() {
        let options = crate::TranspileOptions::default();
        let output = crate::transpile(
            r#"register('k', p => p); s("bd").every(2, x => x.fast("<2 4>"))"#,
            &options,
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(output.surface_writes, bounded(&["k", "_k"], &[]));
        // A double-quoted name is a mini-notation call, which no report can
        // name.
        let output = crate::transpile(r#"register("k", p => p)"#, &options);
        assert!(output.surface_writes.unbounded);
    }

    #[test]
    fn a_text_that_does_not_parse_is_unbounded() {
        assert!(surface_writes("let = ;").unbounded);
        assert!(surface_writes(&format!("{}1{}", "(".repeat(600), ")".repeat(600))).unbounded);
    }
}
