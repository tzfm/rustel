//! Tree-shaken prebake bundling for sharing a single score.
//!
//! A score may call helpers defined in the global or local prebake
//! (`register(...)`, `globalThis.*`, `window.*`, `Pattern.prototype.*`,
//! `setFunction(...)`), which may in turn depend on other prebake helpers or
//! file-local declarations (`const`, `let`, `var`, `function`, `class`).
//!
//! This module resolves the minimal transitive closure of prebake top-level
//! statements referenced by a score and inserts them right after any leading
//! `@key` metadata header (`@title`, `@by`, ...).

use std::collections::{HashMap, HashSet, VecDeque};

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, ArrowFunctionExpression, AssignmentExpression, AssignmentOperator, AssignmentTarget,
    AwaitExpression, BinaryOperator, BindingPattern, CallExpression, ComputedMemberExpression,
    Expression, ForOfStatement, ForStatementLeft, Function, IdentifierReference, ImportExpression,
    MemberExpression, ObjectExpression, ObjectPropertyKind, Program, SimpleAssignmentTarget,
    Statement, StaticMemberExpression, TemplateLiteral, UnaryExpression, UnaryOperator,
    UpdateExpression, VariableDeclarator,
};
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use oxc_semantic::{Scoping, SemanticBuilder};
use oxc_span::{GetSpan, SourceType};
use oxc_syntax::{scope::ScopeFlags, symbol::SymbolFlags, symbol::SymbolId};

/// Maximum number of names expanded statically from one computed-key loop or
/// string expression before falling back to dynamic tracking.
const MAX_STATIC_NAMES: usize = 256;

/// Built-in JavaScript globals whose member accesses (e.g. `Math.min`,
/// `Object.keys`, `console.log`) are not Pattern method calls.
const JS_BUILTIN_NAMESPACES: &[&str] = &[
    "Array", "BigInt", "Boolean", "Date", "Error", "Function", "Intl", "JSON", "Map", "Math",
    "Number", "Object", "Promise", "Proxy", "Reflect", "RegExp", "Set", "String", "Symbol",
    "WeakMap", "WeakSet", "console",
];

/// Result of bundling a score with only the prebake statements it references.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrebakeBundle {
    /// The score text with referenced prebake statements inserted after any
    /// leading metadata header.
    pub source: String,
    /// Number of prebake top-level statements included.
    pub included_statements: usize,
    /// True when any non-blank prebake failed to parse.
    pub unparseable_prebake: bool,
    /// True when the score itself failed to parse.
    pub unparseable_score: bool,
    /// True when any prebake contains dynamic definitions (computed
    /// `register`, `globalThis[k] = ...`, `import(...)`, etc.) that static
    /// analysis cannot trace.
    pub has_dynamic_prebake: bool,
}

/// Bundle `score` with only the top-level statements from `prebakes` (ordered
/// global first, then local) that the score directly or transitively uses.
pub fn bundle_score_with_prebakes(score: &str, prebakes: &[&str]) -> PrebakeBundle {
    let mut bundle = PrebakeBundle {
        source: score.to_owned(),
        ..PrebakeBundle::default()
    };

    let mut units: Vec<PrebakeUnit> = Vec::new();
    for (file_index, prebake) in prebakes.iter().enumerate() {
        if prebake.trim().is_empty() {
            continue;
        }
        match analyze_prebake_file(prebake, file_index) {
            Some(file) => {
                bundle.has_dynamic_prebake |= file.has_dynamic_exports;
                units.extend(file.units);
            }
            None => {
                bundle.unparseable_prebake = true;
            }
        }
    }

    if units.is_empty() {
        return bundle;
    }

    let Some(score_needs) = analyze_score(score) else {
        bundle.unparseable_score = true;
        return bundle;
    };

    let mut global_exporters: HashMap<String, Vec<usize>> = HashMap::new();
    let mut method_exporters: HashMap<String, Vec<usize>> = HashMap::new();
    let mut local_providers: HashMap<(usize, SymbolId), Vec<usize>> = HashMap::new();

    for (index, unit) in units.iter().enumerate() {
        for name in &unit.exported_globals {
            global_exporters
                .entry(name.clone())
                .or_default()
                .push(index);
        }
        for name in &unit.exported_methods {
            method_exporters
                .entry(name.clone())
                .or_default()
                .push(index);
        }
        for local in &unit.declared_locals {
            local_providers
                .entry((unit.file_index, local.symbol))
                .or_default()
                .push(index);
        }
        for &symbol in &unit.mutated_locals {
            local_providers
                .entry((unit.file_index, symbol))
                .or_default()
                .push(index);
        }
    }

    let mut selected = vec![false; units.len()];
    let mut queue = VecDeque::new();
    let enqueue = |index: usize, selected: &mut [bool], queue: &mut VecDeque<usize>| {
        if !selected[index] {
            selected[index] = true;
            queue.push_back(index);
        }
    };

    for name in &score_needs.needed_globals {
        if let Some(provider) = resolve_exporter(&global_exporters, name, None) {
            enqueue(provider, &mut selected, &mut queue);
        }
    }
    for name in &score_needs.needed_methods {
        if let Some(provider) = resolve_exporter(&method_exporters, name, None) {
            enqueue(provider, &mut selected, &mut queue);
        }
    }

    while let Some(index) = queue.pop_front() {
        let unit = &units[index];
        for local in &unit.declared_locals {
            if let Some(providers) = local_providers.get(&(unit.file_index, local.symbol)) {
                for &provider in providers {
                    enqueue(provider, &mut selected, &mut queue);
                }
            }
        }
        for &symbol in &unit.referenced_locals {
            if let Some(providers) = local_providers.get(&(unit.file_index, symbol)) {
                for &provider in providers {
                    enqueue(provider, &mut selected, &mut queue);
                }
            }
        }
        for (name, eager) in &unit.referenced_globals {
            if let Some(provider) = resolve_exporter(&global_exporters, name, Some((index, *eager)))
            {
                enqueue(provider, &mut selected, &mut queue);
            }
        }
        for (name, eager) in &unit.referenced_methods {
            if let Some(provider) = resolve_exporter(&method_exporters, name, Some((index, *eager)))
            {
                enqueue(provider, &mut selected, &mut queue);
            }
        }
    }

    let selected_indices: Vec<usize> = selected
        .iter()
        .enumerate()
        .filter_map(|(index, &used)| used.then_some(index))
        .collect();

    if selected_indices.is_empty() {
        return bundle;
    }

    bundle.included_statements = selected_indices.len();
    let inlined = format_selected_units(
        &units,
        &selected_indices,
        &score_needs.top_level_locals,
        &score_needs.needed_globals,
    );
    bundle.source = splice_into_score(score, &inlined);
    bundle
}

#[derive(Clone, Debug)]
struct LocalBinding {
    symbol: SymbolId,
    name: String,
    /// `true` for `var` or `function` declarations (function-scoped); `false`
    /// for `const`, `let`, `class`, `using` (block-scoped).
    function_scoped: bool,
}

#[derive(Clone, Debug)]
struct PrebakeUnit {
    text: String,
    file_index: usize,
    exported_globals: Vec<String>,
    exported_methods: Vec<String>,
    declared_locals: Vec<LocalBinding>,
    mutated_locals: Vec<SymbolId>,
    referenced_locals: Vec<SymbolId>,
    referenced_globals: Vec<(String, bool)>,
    referenced_methods: Vec<(String, bool)>,
    uses_top_level_await: bool,
    /// Whether this statement could merge with a following `(`, `[`, `` ` ``,
    /// regex `/`, `+`, or `-` token under JavaScript ASI when it lacks a
    /// trailing semicolon (for example `ExpressionStatement` or
    /// `VariableDeclaration`, including `const o = {}` or `f = () => {}`).
    may_need_asi_semicolon: bool,
}

struct AnalyzedPrebakeFile {
    units: Vec<PrebakeUnit>,
    has_dynamic_exports: bool,
}

struct ScoreNeeds {
    needed_globals: Vec<String>,
    needed_methods: Vec<String>,
    top_level_locals: HashSet<String>,
}

/// Resolve the prebake unit that provides exported `name`.
///
/// Later definitions override earlier ones (so local overrides global).
/// Eager top-level reads inside a prebake unit `req` can only observe earlier
/// definitions (`< req`), while deferred reads inside function bodies observe
/// the final definition (unless `req` itself is the final exporter, in which
/// case it falls back to the latest earlier exporter).
fn resolve_exporter(
    exporters: &HashMap<String, Vec<usize>>,
    name: &str,
    requester: Option<(usize, bool)>,
) -> Option<usize> {
    let list = exporters.get(name)?;
    let last = *list.last()?;
    match requester {
        None => Some(last),
        Some((req, true)) => list.iter().rev().copied().find(|&index| index < req),
        Some((req, false)) if last != req => Some(last),
        Some((req, false)) => list.iter().rev().copied().find(|&index| index < req),
    }
}

fn analyze_prebake_file(source: &str, file_index: usize) -> Option<AnalyzedPrebakeFile> {
    if crate::check_nesting(source).is_err() {
        return None;
    }
    crate::on_parse_stack(source, move |source| {
        let allocator = Allocator::default();
        let parsed = Parser::new(&allocator, source, SourceType::mjs()).parse();
        if parsed.panicked || !parsed.diagnostics.is_empty() {
            return None;
        }
        let semantic = SemanticBuilder::new().build(&parsed.program).semantic;
        let scoping = semantic.scoping();
        let root_symbols = collect_root_symbols(&parsed.program, scoping);
        let mut consts = FileConstants::new(scoping);
        let mut units = Vec::with_capacity(parsed.program.body.len());
        let mut has_dynamic_exports = false;

        for (stmt_index, statement) in parsed.program.body.iter().enumerate() {
            let declared_locals = root_symbols.get(stmt_index).cloned().unwrap_or_default();
            let declared_set: HashSet<SymbolId> =
                declared_locals.iter().map(|local| local.symbol).collect();
            let mut analyzer = StatementAnalyzer::new(scoping, &mut consts, &declared_set);
            analyzer.record_top_level_call_mutation(statement);
            analyzer.visit_statement(statement);
            has_dynamic_exports |= analyzer.has_dynamic_exports;

            let span = statement.span();
            let text = source
                .get(span.start as usize..span.end as usize)
                .unwrap_or_default()
                .trim()
                .to_owned();

            let may_need_asi_semicolon = !text.ends_with(';')
                && matches!(
                    statement,
                    Statement::ExpressionStatement(_)
                        | Statement::VariableDeclaration(_)
                        | Statement::DoWhileStatement(_)
                );

            units.push(PrebakeUnit {
                text,
                file_index,
                exported_globals: analyzer.exported_globals,
                exported_methods: analyzer.exported_methods,
                declared_locals,
                mutated_locals: analyzer.mutated_locals,
                referenced_locals: analyzer.referenced_locals,
                referenced_globals: analyzer.referenced_globals,
                referenced_methods: analyzer.referenced_methods,
                uses_top_level_await: analyzer.uses_top_level_await,
                may_need_asi_semicolon,
            });
        }

        Some(AnalyzedPrebakeFile {
            units,
            has_dynamic_exports,
        })
    })
    .flatten()
}

fn analyze_score(source: &str) -> Option<ScoreNeeds> {
    if crate::check_nesting(source).is_err() {
        return None;
    }
    crate::on_parse_stack(source, move |source| {
        let allocator = Allocator::default();
        let parsed = Parser::new(&allocator, source, SourceType::mjs()).parse();
        if parsed.panicked || !parsed.diagnostics.is_empty() {
            return None;
        }
        let semantic = SemanticBuilder::new().build(&parsed.program).semantic;
        let scoping = semantic.scoping();
        let root = scoping.root_scope_id();
        let top_level_locals: HashSet<String> = scoping
            .symbol_ids()
            .filter(|&symbol| scoping.symbol_scope_id(symbol) == root)
            .map(|symbol| scoping.symbol_name(symbol).to_owned())
            .collect();

        let mut consts = FileConstants::new(scoping);
        let empty_declared = HashSet::new();
        let mut score_exported_globals: HashSet<String> = HashSet::new();
        let mut score_exported_methods: HashSet<String> = HashSet::new();
        let mut needed_globals = Vec::new();
        let mut needed_methods = Vec::new();
        let mut seen_globals = HashSet::new();
        let mut seen_methods = HashSet::new();

        for statement in &parsed.program.body {
            let mut analyzer = StatementAnalyzer::new(scoping, &mut consts, &empty_declared);
            analyzer.visit_statement(statement);

            for (name, _) in analyzer.referenced_globals {
                if !score_exported_globals.contains(&name) && seen_globals.insert(name.clone()) {
                    needed_globals.push(name);
                }
            }
            for (name, _) in analyzer.referenced_methods {
                if !score_exported_methods.contains(&name) && seen_methods.insert(name.clone()) {
                    needed_methods.push(name);
                }
            }
            for name in analyzer.exported_globals {
                score_exported_globals.insert(name);
            }
            for name in analyzer.exported_methods {
                score_exported_methods.insert(name);
            }
        }

        Some(ScoreNeeds {
            needed_globals,
            needed_methods,
            top_level_locals,
        })
    })
    .flatten()
}

/// Group every root-scope symbol in `program` under the index of the top-level
/// statement whose span contains its declaration.
fn collect_root_symbols(program: &Program<'_>, scoping: &Scoping) -> Vec<Vec<LocalBinding>> {
    let root = scoping.root_scope_id();
    let mut by_statement = vec![Vec::new(); program.body.len()];
    let spans: Vec<(u32, u32)> = program
        .body
        .iter()
        .map(|statement| {
            let span = statement.span();
            (span.start, span.end)
        })
        .collect();

    for symbol in scoping.symbol_ids() {
        if scoping.symbol_scope_id(symbol) != root {
            continue;
        }
        let span = scoping.symbol_span(symbol);
        let flags = scoping.symbol_flags(symbol);
        let function_scoped = flags.contains(SymbolFlags::FunctionScopedVariable)
            || flags.contains(SymbolFlags::Function);
        let binding = LocalBinding {
            symbol,
            name: scoping.symbol_name(symbol).to_owned(),
            function_scoped,
        };
        if let Some(index) = spans
            .iter()
            .position(|&(start, end)| span.start >= start && span.end <= end)
        {
            by_statement[index].push(binding);
        }
    }
    by_statement
}

fn is_shared_global_expr(expression: &Expression<'_>, scoping: &Scoping) -> bool {
    matches!(
        expression.get_inner_expression(),
        Expression::Identifier(identifier)
            if matches!(identifier.name.as_str(), "globalThis" | "window" | "rustelScope")
                && resolved_symbol(scoping, identifier).is_none()
    )
}

fn is_pattern_prototype(expression: &Expression<'_>, scoping: &Scoping) -> bool {
    let Expression::StaticMemberExpression(member) = expression.get_inner_expression() else {
        return false;
    };
    member.property.name == "prototype"
        && matches!(
            member.object.get_inner_expression(),
            Expression::Identifier(identifier)
                if identifier.name == "Pattern" && resolved_symbol(scoping, identifier).is_none()
        )
}

fn is_js_builtin_namespace(expression: &Expression<'_>, scoping: &Scoping) -> bool {
    matches!(
        expression.get_inner_expression(),
        Expression::Identifier(identifier)
            if JS_BUILTIN_NAMESPACES.contains(&identifier.name.as_str())
                && resolved_symbol(scoping, identifier).is_none()
    )
}

fn resolved_symbol(scoping: &Scoping, identifier: &IdentifierReference<'_>) -> Option<SymbolId> {
    identifier
        .reference_id
        .get()
        .and_then(|reference| scoping.get_reference(reference).symbol_id())
}

fn static_template_value<'a, 'b>(template: &'b TemplateLiteral<'a>) -> Option<&'b str> {
    if !template.expressions.is_empty() || template.quasis.len() != 1 {
        return None;
    }
    template.quasis[0].value.cooked.map(|value| value.as_str())
}

/// Immutable string/list/object bindings and `register` aliases tracked across
/// a single parsed file so loops such as `for (const name of names) register(name, ...)`
/// resolve statically.
struct FileConstants<'s> {
    scoping: &'s Scoping,
    const_strings: HashMap<SymbolId, Vec<String>>,
    const_lists: HashMap<SymbolId, Vec<String>>,
    const_object_keys: HashMap<SymbolId, Vec<String>>,
    register_aliases: HashSet<SymbolId>,
}

impl<'s> FileConstants<'s> {
    fn new(scoping: &'s Scoping) -> Self {
        Self {
            scoping,
            const_strings: HashMap::new(),
            const_lists: HashMap::new(),
            const_object_keys: HashMap::new(),
            register_aliases: HashSet::new(),
        }
    }

    fn is_register(&self, expression: &Expression<'_>) -> bool {
        match expression.get_inner_expression() {
            Expression::Identifier(identifier) => match resolved_symbol(self.scoping, identifier) {
                Some(symbol) => self.register_aliases.contains(&symbol),
                None => identifier.name == "register",
            },
            expr => expr.as_member_expression().is_some_and(|member| {
                member.static_property_name() == Some("register")
                    && is_shared_global_expr(member.object(), self.scoping)
            }),
        }
    }

    fn is_set_function(&self, expression: &Expression<'_>) -> bool {
        matches!(
            expression.get_inner_expression(),
            Expression::Identifier(identifier)
                if identifier.name == "setFunction"
                    && resolved_symbol(self.scoping, identifier).is_none()
        )
    }

    fn static_object_keys(object: &ObjectExpression<'_>) -> Option<Vec<String>> {
        let mut keys = Vec::new();
        for property in &object.properties {
            match property {
                ObjectPropertyKind::ObjectProperty(property) if !property.computed => {
                    let name = property.key.static_name()?;
                    keys.push(name.into_owned());
                    if keys.len() > MAX_STATIC_NAMES {
                        return None;
                    }
                }
                _ => return None,
            }
        }
        if keys.is_empty() {
            return None;
        }
        Some(keys)
    }

    fn eval_string_expr(&self, expr: &Expression<'_>) -> Option<Vec<String>> {
        match expr.get_inner_expression() {
            Expression::StringLiteral(literal) => Some(vec![literal.value.to_string()]),
            Expression::Identifier(identifier) => {
                let symbol = resolved_symbol(self.scoping, identifier)?;
                self.const_strings.get(&symbol).cloned()
            }
            Expression::TemplateLiteral(template) => {
                let mut acc = vec![template.quasis.first()?.value.cooked?.to_string()];
                for (expr, quasi) in template
                    .expressions
                    .iter()
                    .zip(template.quasis.iter().skip(1))
                {
                    let parts = self.eval_string_expr(expr)?;
                    let suffix = quasi.value.cooked?;
                    let mut next = Vec::new();
                    for prefix in &acc {
                        for part in &parts {
                            if next.len() >= MAX_STATIC_NAMES {
                                return None;
                            }
                            let mut combined =
                                String::with_capacity(prefix.len() + part.len() + suffix.len());
                            combined.push_str(prefix);
                            combined.push_str(part);
                            combined.push_str(suffix.as_str());
                            next.push(combined);
                        }
                    }
                    acc = next;
                }
                Some(acc)
            }
            Expression::BinaryExpression(binary) if binary.operator == BinaryOperator::Addition => {
                let left = self.eval_string_expr(&binary.left)?;
                let right = self.eval_string_expr(&binary.right)?;
                let mut out = Vec::new();
                for l in &left {
                    for r in &right {
                        if out.len() >= MAX_STATIC_NAMES {
                            return None;
                        }
                        let mut combined = String::with_capacity(l.len() + r.len());
                        combined.push_str(l);
                        combined.push_str(r);
                        out.push(combined);
                    }
                }
                Some(out)
            }
            _ => None,
        }
    }

    fn eval_iterable_keys(&self, expr: &Expression<'_>) -> Option<(Vec<String>, bool)> {
        match expr.get_inner_expression() {
            Expression::ArrayExpression(array) => {
                let mut out = Vec::new();
                for element in &array.elements {
                    let expr = element.as_expression()?;
                    let values = self.eval_string_expr(expr)?;
                    if out.len() + values.len() > MAX_STATIC_NAMES {
                        return None;
                    }
                    out.extend(values);
                }
                Some((out, false))
            }
            Expression::Identifier(identifier) => {
                let symbol = resolved_symbol(self.scoping, identifier)?;
                self.const_lists
                    .get(&symbol)
                    .cloned()
                    .map(|list| (list, false))
            }
            Expression::CallExpression(call) => {
                let entries = if call.callee.is_specific_member_access("Object", "keys") {
                    false
                } else if call.callee.is_specific_member_access("Object", "entries") {
                    true
                } else {
                    return None;
                };
                let target = call
                    .arguments
                    .first()?
                    .as_expression()?
                    .get_inner_expression();
                let keys = match target {
                    Expression::ObjectExpression(object) => Self::static_object_keys(object)?,
                    Expression::Identifier(identifier) => {
                        let symbol = resolved_symbol(self.scoping, identifier)?;
                        self.const_object_keys.get(&symbol)?.clone()
                    }
                    _ => return None,
                };
                Some((keys, entries))
            }
            _ => None,
        }
    }

    fn binding_key_symbol(pattern: &BindingPattern<'_>, is_entries: bool) -> Option<SymbolId> {
        if is_entries {
            let BindingPattern::ArrayPattern(array) = pattern else {
                return None;
            };
            let first = array.elements.first()?.as_ref()?;
            let BindingPattern::BindingIdentifier(identifier) = first else {
                return None;
            };
            identifier.symbol_id.get()
        } else {
            let BindingPattern::BindingIdentifier(identifier) = pattern else {
                return None;
            };
            identifier.symbol_id.get()
        }
    }
}

struct StatementAnalyzer<'s, 'c> {
    scoping: &'s Scoping,
    consts: &'c mut FileConstants<'s>,
    declared_locals: &'c HashSet<SymbolId>,
    exported_globals: Vec<String>,
    exported_methods: Vec<String>,
    mutated_locals: Vec<SymbolId>,
    referenced_locals: Vec<SymbolId>,
    referenced_globals: Vec<(String, bool)>,
    referenced_methods: Vec<(String, bool)>,
    seen_mutated: HashSet<SymbolId>,
    seen_locals: HashSet<SymbolId>,
    seen_globals: HashSet<(String, bool)>,
    seen_methods: HashSet<(String, bool)>,
    has_dynamic_exports: bool,
    uses_top_level_await: bool,
    function_depth: usize,
}

impl<'s, 'c> StatementAnalyzer<'s, 'c> {
    fn new(
        scoping: &'s Scoping,
        consts: &'c mut FileConstants<'s>,
        declared_locals: &'c HashSet<SymbolId>,
    ) -> Self {
        Self {
            scoping,
            consts,
            declared_locals,
            exported_globals: Vec::new(),
            exported_methods: Vec::new(),
            mutated_locals: Vec::new(),
            referenced_locals: Vec::new(),
            referenced_globals: Vec::new(),
            referenced_methods: Vec::new(),
            seen_mutated: HashSet::new(),
            seen_locals: HashSet::new(),
            seen_globals: HashSet::new(),
            seen_methods: HashSet::new(),
            has_dynamic_exports: false,
            uses_top_level_await: false,
            function_depth: 0,
        }
    }

    fn root_local_symbol(&self, identifier: &IdentifierReference<'_>) -> Option<SymbolId> {
        let symbol = resolved_symbol(self.scoping, identifier)?;
        (self.scoping.symbol_scope_id(symbol) == self.scoping.root_scope_id()).then_some(symbol)
    }

    fn root_receiver_local(&self, expr: &Expression<'_>) -> Option<SymbolId> {
        let mut current = expr.get_inner_expression();
        loop {
            match current {
                Expression::Identifier(identifier) => return self.root_local_symbol(identifier),
                Expression::StaticMemberExpression(member) => {
                    current = member.object.get_inner_expression();
                }
                Expression::ComputedMemberExpression(member) => {
                    current = member.object.get_inner_expression();
                }
                _ => return None,
            }
        }
    }

    fn record_local_ref(&mut self, symbol: SymbolId) {
        if !self.declared_locals.contains(&symbol) && self.seen_locals.insert(symbol) {
            self.referenced_locals.push(symbol);
        }
    }

    fn record_local_mutation(&mut self, symbol: SymbolId) {
        if self.function_depth == 0
            && !self.declared_locals.contains(&symbol)
            && self.seen_mutated.insert(symbol)
        {
            self.mutated_locals.push(symbol);
        }
    }

    fn record_top_level_call_mutation(&mut self, statement: &Statement<'_>) {
        let Statement::ExpressionStatement(expr_stmt) = statement else {
            return;
        };
        let expr = match expr_stmt.expression.get_inner_expression() {
            Expression::AwaitExpression(awaited) => awaited.argument.get_inner_expression(),
            other => other,
        };
        if let Expression::CallExpression(call) = expr
            && let Some(member) = call.callee.as_member_expression()
            && let Some(symbol) = self.root_receiver_local(member.object())
        {
            self.record_local_mutation(symbol);
        }
    }

    fn record_global_ref(&mut self, name: &str) {
        let key = (name.to_owned(), self.function_depth == 0);
        if self.seen_globals.insert(key.clone()) {
            self.referenced_globals.push(key);
        }
    }

    fn record_method_ref(&mut self, name: &str) {
        let key = (name.to_owned(), self.function_depth == 0);
        if self.seen_methods.insert(key.clone()) {
            self.referenced_methods.push(key);
        }
    }

    fn export_global(&mut self, name: &str) {
        self.exported_globals.push(name.to_owned());
    }

    fn export_method(&mut self, name: &str) {
        self.exported_methods.push(name.to_owned());
    }

    fn export_registered(&mut self, name: &str) {
        self.export_global(name);
        self.export_method(name);
        if !name.starts_with('_') {
            let raw = format!("_{name}");
            self.export_global(&raw);
            self.export_method(&raw);
        }
    }

    fn collect_register_arg(&mut self, argument: Option<&Argument<'_>>) {
        match argument {
            None => {}
            Some(Argument::ArrayExpression(array)) => {
                for element in &array.elements {
                    match element.as_expression() {
                        Some(expr) => match self.consts.eval_string_expr(expr) {
                            Some(names) => {
                                for name in names {
                                    self.export_registered(&name);
                                }
                            }
                            None => self.has_dynamic_exports = true,
                        },
                        None => self.has_dynamic_exports = true,
                    }
                }
            }
            Some(arg) => match arg.as_expression() {
                Some(expr) => match self.consts.eval_string_expr(expr) {
                    Some(names) => {
                        for name in names {
                            self.export_registered(&name);
                        }
                    }
                    None => self.has_dynamic_exports = true,
                },
                None => self.has_dynamic_exports = true,
            },
        }
    }

    fn collect_set_function_arg(&mut self, argument: Option<&Argument<'_>>) {
        let definition = match argument {
            None => return,
            Some(Argument::ObjectExpression(definition)) => definition,
            Some(_) => {
                self.has_dynamic_exports = true;
                return;
            }
        };
        for property in &definition.properties {
            match property {
                ObjectPropertyKind::ObjectProperty(property)
                    if !property.computed
                        && property.key.static_name().as_deref() == Some("name") =>
                {
                    match self.consts.eval_string_expr(&property.value) {
                        Some(names) => {
                            for name in names {
                                self.export_global(&name);
                                self.export_method(&name);
                            }
                        }
                        None => self.has_dynamic_exports = true,
                    }
                    return;
                }
                ObjectPropertyKind::SpreadProperty(_) => {
                    self.has_dynamic_exports = true;
                    return;
                }
                _ => {}
            }
        }
    }

    fn collect_assign_target(&mut self, argument: Option<&Argument<'_>>, is_global: bool) {
        let Some(expr) = argument.and_then(Argument::as_expression) else {
            self.has_dynamic_exports = true;
            return;
        };
        let keys = match expr.get_inner_expression() {
            Expression::ObjectExpression(object) => FileConstants::static_object_keys(object),
            Expression::Identifier(identifier) => resolved_symbol(self.scoping, identifier)
                .and_then(|symbol| self.consts.const_object_keys.get(&symbol).cloned()),
            _ => None,
        };
        match keys {
            Some(keys) => {
                for key in keys {
                    if is_global {
                        self.export_global(&key);
                    } else {
                        self.export_method(&key);
                    }
                }
            }
            None => self.has_dynamic_exports = true,
        }
    }

    fn record_target_mutation(&mut self, target: &AssignmentTarget<'_>) {
        match target {
            AssignmentTarget::AssignmentTargetIdentifier(identifier) => {
                if let Some(symbol) = self.root_local_symbol(identifier) {
                    self.record_local_mutation(symbol);
                }
            }
            AssignmentTarget::StaticMemberExpression(member) => {
                if let Some(symbol) = self.root_receiver_local(&member.object) {
                    self.record_local_mutation(symbol);
                }
            }
            AssignmentTarget::ComputedMemberExpression(member) => {
                if let Some(symbol) = self.root_receiver_local(&member.object) {
                    self.record_local_mutation(symbol);
                }
            }
            _ => {}
        }
    }

    fn record_simple_target_mutation(&mut self, target: &SimpleAssignmentTarget<'_>) {
        match target {
            SimpleAssignmentTarget::AssignmentTargetIdentifier(identifier) => {
                if let Some(symbol) = self.root_local_symbol(identifier) {
                    self.record_local_mutation(symbol);
                }
            }
            SimpleAssignmentTarget::StaticMemberExpression(member) => {
                if let Some(symbol) = self.root_receiver_local(&member.object) {
                    self.record_local_mutation(symbol);
                }
            }
            SimpleAssignmentTarget::ComputedMemberExpression(member) => {
                if let Some(symbol) = self.root_receiver_local(&member.object) {
                    self.record_local_mutation(symbol);
                }
            }
            _ => {}
        }
    }
}

impl<'a> Visit<'a> for StatementAnalyzer<'_, '_> {
    fn visit_function(&mut self, function: &Function<'a>, flags: ScopeFlags) {
        self.function_depth += 1;
        walk::walk_function(self, function, flags);
        self.function_depth -= 1;
    }

    fn visit_arrow_function_expression(&mut self, function: &ArrowFunctionExpression<'a>) {
        self.function_depth += 1;
        walk::walk_arrow_function_expression(self, function);
        self.function_depth -= 1;
    }

    fn visit_await_expression(&mut self, expression: &AwaitExpression<'a>) {
        if self.function_depth == 0 {
            self.uses_top_level_await = true;
        }
        walk::walk_await_expression(self, expression);
    }

    fn visit_variable_declarator(&mut self, declarator: &VariableDeclarator<'a>) {
        if let BindingPattern::BindingIdentifier(identifier) = &declarator.id
            && let Some(symbol) = identifier.symbol_id.get()
            && !self.scoping.symbol_is_mutated(symbol)
            && let Some(init) = &declarator.init
        {
            if self.consts.is_register(init) {
                self.consts.register_aliases.insert(symbol);
            }
            match init.get_inner_expression() {
                Expression::ObjectExpression(object) => {
                    if let Some(keys) = FileConstants::static_object_keys(object) {
                        self.consts.const_object_keys.insert(symbol, keys);
                    }
                }
                Expression::ArrayExpression(_) => {
                    if let Some((list, false)) = self.consts.eval_iterable_keys(init) {
                        self.consts.const_lists.insert(symbol, list);
                    }
                }
                _ => {
                    if let Some(values) = self.consts.eval_string_expr(init) {
                        self.consts.const_strings.insert(symbol, values);
                    }
                }
            }
        }
        walk::walk_variable_declarator(self, declarator);
    }

    fn visit_for_of_statement(&mut self, statement: &ForOfStatement<'a>) {
        if statement.r#await && self.function_depth == 0 {
            self.uses_top_level_await = true;
        }
        let bound =
            self.consts
                .eval_iterable_keys(&statement.right)
                .and_then(|(keys, is_entries)| {
                    let ForStatementLeft::VariableDeclaration(decl) = &statement.left else {
                        return None;
                    };
                    let declarator = decl.declarations.first()?;
                    let symbol = FileConstants::binding_key_symbol(&declarator.id, is_entries)?;
                    (!self.scoping.symbol_is_mutated(symbol)).then_some((symbol, keys))
                });
        if let Some((symbol, keys)) = bound {
            let previous = self.consts.const_strings.insert(symbol, keys);
            walk::walk_for_of_statement(self, statement);
            match previous {
                Some(old) => {
                    self.consts.const_strings.insert(symbol, old);
                }
                None => {
                    self.consts.const_strings.remove(&symbol);
                }
            }
        } else {
            walk::walk_for_of_statement(self, statement);
        }
    }

    fn visit_identifier_reference(&mut self, identifier: &IdentifierReference<'a>) {
        match resolved_symbol(self.scoping, identifier) {
            Some(symbol) => {
                if self.scoping.symbol_scope_id(symbol) == self.scoping.root_scope_id() {
                    self.record_local_ref(symbol);
                }
            }
            None => {
                let name = identifier.name.as_str();
                if !matches!(name, "globalThis" | "window" | "rustelScope") {
                    self.record_global_ref(name);
                }
            }
        }
    }

    fn visit_static_member_expression(&mut self, member: &StaticMemberExpression<'a>) {
        let name = member.property.name.as_str();
        if is_shared_global_expr(&member.object, self.scoping) {
            self.record_global_ref(name);
            return;
        }
        if is_pattern_prototype(&member.object, self.scoping) {
            self.record_method_ref(name);
            return;
        }
        if !is_js_builtin_namespace(&member.object, self.scoping) {
            self.record_method_ref(name);
        }
        self.visit_expression(&member.object);
    }

    fn visit_computed_member_expression(&mut self, member: &ComputedMemberExpression<'a>) {
        let resolved = match member.expression.get_inner_expression() {
            Expression::StringLiteral(literal) => Some(vec![literal.value.to_string()]),
            Expression::TemplateLiteral(template) => {
                static_template_value(template).map(|value| vec![value.to_owned()])
            }
            expr => self.consts.eval_string_expr(expr),
        };
        if is_shared_global_expr(&member.object, self.scoping) {
            if let Some(names) = resolved {
                for name in names {
                    self.record_global_ref(&name);
                }
            }
            self.visit_expression(&member.expression);
            return;
        }
        if is_pattern_prototype(&member.object, self.scoping) {
            if let Some(names) = resolved {
                for name in names {
                    self.record_method_ref(&name);
                }
            }
            self.visit_expression(&member.expression);
            return;
        }
        if !is_js_builtin_namespace(&member.object, self.scoping)
            && let Some(names) = resolved
        {
            for name in names {
                self.record_method_ref(&name);
            }
        }
        walk::walk_computed_member_expression(self, member);
    }

    fn visit_assignment_expression(&mut self, assignment: &AssignmentExpression<'a>) {
        self.record_target_mutation(&assignment.left);
        if let Some(member) = assignment.left.as_member_expression() {
            let object = member.object();
            let is_global = is_shared_global_expr(object, self.scoping);
            let is_proto = is_pattern_prototype(object, self.scoping);
            if is_global || is_proto {
                let mut resolved_names: Vec<String> = Vec::new();
                match member.static_property_name() {
                    Some(name) => resolved_names.push(name.to_owned()),
                    None => match member {
                        MemberExpression::ComputedMemberExpression(computed) => {
                            if let Some(names) = self.consts.eval_string_expr(&computed.expression)
                            {
                                resolved_names.extend(names);
                            } else {
                                self.has_dynamic_exports = true;
                            }
                            self.visit_expression(&computed.expression);
                        }
                        _ => self.has_dynamic_exports = true,
                    },
                }
                let also_reads = assignment.operator != AssignmentOperator::Assign;
                for name in &resolved_names {
                    if is_global {
                        self.export_global(name);
                        if also_reads {
                            self.record_global_ref(name);
                        }
                    } else {
                        self.export_method(name);
                        if also_reads {
                            self.record_method_ref(name);
                        }
                    }
                }
                self.visit_expression(&assignment.right);
                return;
            }
        }
        walk::walk_assignment_expression(self, assignment);
    }

    fn visit_update_expression(&mut self, update: &UpdateExpression<'a>) {
        self.record_simple_target_mutation(&update.argument);
        walk::walk_update_expression(self, update);
    }

    fn visit_unary_expression(&mut self, unary: &UnaryExpression<'a>) {
        if unary.operator == UnaryOperator::Delete
            && let Some(symbol) = self.root_receiver_local(&unary.argument)
        {
            self.record_local_mutation(symbol);
        }
        walk::walk_unary_expression(self, unary);
    }

    fn visit_import_expression(&mut self, expression: &ImportExpression<'a>) {
        self.has_dynamic_exports = true;
        walk::walk_import_expression(self, expression);
    }

    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        if let Some(member) = call.callee.as_member_expression()
            && member.static_property_name() == Some("forEach")
            && let Some((keys, is_entries)) = self.consts.eval_iterable_keys(member.object())
            && let Some(callback) = call.arguments.first().and_then(Argument::as_expression)
        {
            let symbol = match callback.get_inner_expression() {
                Expression::ArrowFunctionExpression(arrow) => {
                    arrow.params.items.first().and_then(|param| {
                        FileConstants::binding_key_symbol(&param.pattern, is_entries)
                    })
                }
                Expression::FunctionExpression(func) => {
                    func.params.items.first().and_then(|param| {
                        FileConstants::binding_key_symbol(&param.pattern, is_entries)
                    })
                }
                _ => None,
            }
            .filter(|symbol| !self.scoping.symbol_is_mutated(*symbol));
            if let Some(symbol) = symbol {
                let previous = self.consts.const_strings.insert(symbol, keys);
                walk::walk_call_expression(self, call);
                match previous {
                    Some(old) => {
                        self.consts.const_strings.insert(symbol, old);
                    }
                    None => {
                        self.consts.const_strings.remove(&symbol);
                    }
                }
                return;
            }
        }

        if self.consts.is_register(&call.callee) {
            self.collect_register_arg(call.arguments.first());
        } else if self.consts.is_set_function(&call.callee) {
            self.collect_set_function_arg(call.arguments.first());
        } else if call.callee.is_specific_member_access("Object", "assign")
            && let Some(target) = call.arguments.first().and_then(Argument::as_expression)
        {
            if let Some(symbol) = self.root_receiver_local(target) {
                self.record_local_mutation(symbol);
            }
            let is_global = is_shared_global_expr(target, self.scoping);
            let is_proto = is_pattern_prototype(target, self.scoping);
            if is_global || is_proto {
                for argument in call.arguments.iter().skip(1) {
                    self.collect_assign_target(Some(argument), is_global);
                }
            }
        } else if call
            .callee
            .is_specific_member_access("Object", "defineProperty")
            && let Some(target) = call.arguments.first().and_then(Argument::as_expression)
        {
            if let Some(symbol) = self.root_receiver_local(target) {
                self.record_local_mutation(symbol);
            }
            let is_global = is_shared_global_expr(target, self.scoping);
            let is_proto = is_pattern_prototype(target, self.scoping);
            if is_global || is_proto {
                match call
                    .arguments
                    .get(1)
                    .and_then(Argument::as_expression)
                    .and_then(|key| self.consts.eval_string_expr(key))
                {
                    Some(names) => {
                        for name in names {
                            if is_global {
                                self.export_global(&name);
                            } else {
                                self.export_method(&name);
                            }
                        }
                    }
                    None => self.has_dynamic_exports = true,
                }
            }
        }

        walk::walk_call_expression(self, call);
    }
}

struct FormattedInlined {
    text: String,
    may_need_asi_semicolon: bool,
}

/// Format selected prebake units in declaration order. When a prebake file's
/// selected file-local declarations would collide with or shadow a top-level
/// binding or referenced global in the score or another selected prebake file,
/// isolate that file's block.
fn format_selected_units(
    units: &[PrebakeUnit],
    selected_indices: &[usize],
    score_locals: &HashSet<String>,
    score_globals: &[String],
) -> FormattedInlined {
    let score_global_set: HashSet<&str> = score_globals.iter().map(String::as_str).collect();
    let mut local_names_by_file: HashMap<usize, HashMap<String, bool>> = HashMap::new();
    let mut referenced_globals_by_file: HashMap<usize, HashSet<&str>> = HashMap::new();
    for &index in selected_indices {
        let unit = &units[index];
        let entry = local_names_by_file.entry(unit.file_index).or_default();
        for local in &unit.declared_locals {
            let slot = entry.entry(local.name.clone()).or_insert(false);
            *slot |= local.function_scoped;
        }
        let globals = referenced_globals_by_file
            .entry(unit.file_index)
            .or_default();
        for (name, _) in &unit.referenced_globals {
            globals.insert(name.as_str());
        }
    }

    let mut wrap_mode: HashMap<usize, WrapMode> = HashMap::new();
    for (&file_index, names) in &local_names_by_file {
        let mut collides = false;
        let mut needs_iife = false;
        for (name, &function_scoped) in names {
            let collides_score =
                score_locals.contains(name) || score_global_set.contains(name.as_str());
            let collides_other_file = local_names_by_file
                .iter()
                .any(|(&other, other_names)| other != file_index && other_names.contains_key(name))
                || referenced_globals_by_file
                    .iter()
                    .any(|(&other, other_globals)| {
                        other != file_index && other_globals.contains(name.as_str())
                    });
            if collides_score || collides_other_file {
                collides = true;
                needs_iife |= function_scoped;
            }
        }
        if collides {
            let uses_await = selected_indices
                .iter()
                .filter(|&&index| units[index].file_index == file_index)
                .any(|&index| units[index].uses_top_level_await);
            wrap_mode.insert(
                file_index,
                if needs_iife {
                    if uses_await {
                        WrapMode::AsyncIife
                    } else {
                        WrapMode::SyncIife
                    }
                } else {
                    WrapMode::Block
                },
            );
        }
    }

    let mut chunks: Vec<(String, bool)> = Vec::new();
    let mut pos = 0;
    while pos < selected_indices.len() {
        let file_index = units[selected_indices[pos]].file_index;
        let end = selected_indices[pos..]
            .iter()
            .position(|&index| units[index].file_index != file_index)
            .map_or(selected_indices.len(), |offset| pos + offset);
        let group = &selected_indices[pos..end];
        pos = end;

        match wrap_mode.get(&file_index).copied() {
            None => {
                for &index in group {
                    chunks.push((
                        units[index].text.clone(),
                        units[index].may_need_asi_semicolon,
                    ));
                }
            }
            Some(mode) => {
                let inner = join_statements(
                    group
                        .iter()
                        .map(|&index| {
                            (
                                units[index].text.as_str(),
                                units[index].may_need_asi_semicolon,
                            )
                        })
                        .collect::<Vec<_>>(),
                );
                let indented = inner
                    .text
                    .lines()
                    .map(|line| {
                        if line.is_empty() {
                            String::new()
                        } else {
                            format!("  {line}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let (wrapped, may_need_asi) = match mode {
                    WrapMode::Block => (format!("{{\n{indented}\n}}"), false),
                    WrapMode::SyncIife => (format!("(() => {{\n{indented}\n}})()"), true),
                    WrapMode::AsyncIife => {
                        (format!("await (async () => {{\n{indented}\n}})()"), true)
                    }
                };
                chunks.push((wrapped, may_need_asi));
            }
        }
    }

    join_statements(
        chunks
            .iter()
            .map(|(text, asi)| (text.as_str(), *asi))
            .collect(),
    )
}

#[derive(Clone, Copy)]
enum WrapMode {
    Block,
    SyncIife,
    AsyncIife,
}

fn join_statements(statements: Vec<(&str, bool)>) -> FormattedInlined {
    let mut out = String::new();
    let mut last_may_need_asi = false;
    for (statement, may_need_asi) in statements {
        if !out.is_empty() {
            if last_may_need_asi && starts_with_asi_ambiguous_token(statement) {
                out.push(';');
            }
            out.push('\n');
        }
        out.push_str(statement);
        last_may_need_asi = may_need_asi;
    }
    FormattedInlined {
        text: out,
        may_need_asi_semicolon: last_may_need_asi,
    }
}

/// Whether the first non-comment token of `next` can continue a preceding
/// unterminated expression or declaration under JavaScript ASI.
fn starts_with_asi_ambiguous_token(next: &str) -> bool {
    let mut rest = next;
    loop {
        rest = rest.trim_start();
        if let Some(after_slashes) = rest.strip_prefix("//") {
            match after_slashes.find('\n') {
                Some(nl) => rest = &after_slashes[nl + 1..],
                None => return false,
            }
        } else if let Some(after_open) = rest.strip_prefix("/*") {
            match after_open.find("*/") {
                Some(close) => rest = &after_open[close + 2..],
                None => return false,
            }
        } else {
            break;
        }
    }
    rest.starts_with(['(', '[', '`', '/', '+', '-'])
}

/// Insert `inlined` after any leading `@key` metadata header (`@title`, `@by`)
/// so the score's metadata stays at line 1.
fn splice_into_score(score: &str, inlined: &FormattedInlined) -> String {
    let split = metadata_header_end(score);
    let (header, rest) = score.split_at(split);
    let body = if split == 0 {
        rest.trim_start_matches(['\r', '\n'])
    } else {
        rest.trim_start_matches([' ', '\t'])
            .trim_start_matches(['\r', '\n'])
    };

    let mut out = String::with_capacity(score.len() + inlined.text.len() + 6);
    if !header.trim().is_empty() {
        out.push_str(header.trim_end());
        out.push_str("\n\n");
    }
    out.push_str(&inlined.text);
    if body.is_empty() {
        out.push('\n');
    } else {
        if inlined.may_need_asi_semicolon && starts_with_asi_ambiguous_token(body) {
            out.push(';');
        }
        out.push_str("\n\n");
        out.push_str(body);
    }
    out
}

/// Byte offset immediately after the last leading comment block/line that
/// carries a `@key` metadata field, or `0` when the score has no leading
/// metadata header.
fn metadata_header_end(text: &str) -> usize {
    let mut cursor = 0;
    let mut last_meta_end = 0;
    loop {
        let rest = &text[cursor..];
        let trimmed = rest.trim_start();
        let start = text.len() - trimmed.len();
        if let Some(after_slashes) = trimmed.strip_prefix("//") {
            let line_end = match after_slashes.find('\n') {
                Some(nl) => start + 2 + nl + 1,
                None => text.len(),
            };
            let comment = &text[start..line_end];
            if comment_has_meta_field(comment) {
                last_meta_end = line_end;
            }
            cursor = line_end;
            if cursor >= text.len() {
                break;
            }
        } else if let Some(after_open) = trimmed.strip_prefix("/*") {
            let block_end = match after_open.find("*/") {
                Some(close) => start + 2 + close + 2,
                None => text.len(),
            };
            let comment = &text[start..block_end];
            if comment_has_meta_field(comment) {
                last_meta_end = block_end;
            }
            cursor = block_end;
            if cursor >= text.len() {
                break;
            }
        } else {
            break;
        }
    }
    last_meta_end
}

fn comment_has_meta_field(comment: &str) -> bool {
    comment.lines().any(|line| {
        let line = line.trim();
        let line = line.strip_prefix("/*").unwrap_or(line);
        let line = line.strip_prefix("//").unwrap_or(line);
        let line = line.strip_prefix('*').unwrap_or(line);
        let line = line.strip_suffix("*/").unwrap_or(line).trim();
        let Some(rest) = line.strip_prefix('@') else {
            return false;
        };
        let key = match rest.find(char::is_whitespace) {
            Some(at) => &rest[..at],
            None => rest,
        };
        !key.is_empty()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundles_only_referenced_helpers_and_transitive_deps_in_order() {
        let global = "\
register('unused', pat => pat.gain(0.5))
register('rhtm', (amount, subdiv, cycle, len, pat) => pat.seg(subdiv).mask(brandBy(amount).seg(subdiv).rib(cycle, len)))
register('sbs', (prob, s, fn, pat) => {
  return pat.when(rand.seed(s).lt(prob), fn)
})
register('width', (w, d, s, pat) => {
  const sl = 0.5 - (s / 2)
  const sr = 0.5 + (s / 2)
  return pat.pan(sl).sub(note(-w))
    .superimpose(x => x.late(d).pan(sr).add(note(w)))
})
register('sw', pat => pat.width(0.05, 1 / 75, .7))
samples('local:/Users/example/private-drums')
";
        let score = "\
$: n(\"<-2 0 -4 -3>\").scale(\"C:major\")
  .rhtm(\"<0.3 0.6>\", 16, 43, 1)
  .sw()

$: n(\"<0 1 2 3>\")
  .sbs(.3, 78, x => x.strans(4))
";
        let bundled = bundle_score_with_prebakes(score, &[global]);
        assert_eq!(bundled.included_statements, 4);
        assert!(!bundled.unparseable_prebake);
        assert!(!bundled.unparseable_score);
        assert!(!bundled.has_dynamic_prebake);
        assert!(!bundled.source.contains("unused"));
        assert!(!bundled.source.contains("private-drums"));
        let width_pos = bundled
            .source
            .find("register('width'")
            .expect("width included");
        let sw_pos = bundled.source.find("register('sw'").expect("sw included");
        let score_pos = bundled.source.find("$: n(\"<-2").expect("score included");
        assert!(width_pos < sw_pos && sw_pos < score_pos);

        // Bundling an already-bundled score is idempotent.
        let again = bundle_score_with_prebakes(&bundled.source, &[global]);
        assert_eq!(again.included_statements, 0);
        assert_eq!(again.source, bundled.source);
    }

    #[test]
    fn preserves_leading_metadata_header_and_handles_local_overrides_and_file_locals() {
        let global = "\
const unusedConst = 99
const baseOffset = 0.5
const scales = ['C:major']
const unusedDerived = scales.map(x => x.toLowerCase())
function panPair(spread) {
  return [baseOffset - spread / 2, baseOffset + spread / 2]
}
register('width', (w, s, pat) => {
  const [l, r] = panPair(s)
  return pat.scale(scales[0]).pan(l).superimpose(x => x.pan(r).add(note(w)))
})
globalThis.lead = () => note('c3')
globalThis.wrapMe = x => x * 2
";
        let local = "\
globalThis.lead = () => note('d3').width(0.05, 0.7)
const origWrap = globalThis.wrapMe
globalThis.wrapMe = x => origWrap(x) + 1
";
        let score = "\
/*
@title night drive
@by john
*/

// main voice
$: lead().gain(wrapMe(0.2))
";
        let bundled = bundle_score_with_prebakes(score, &[global, local]);
        assert!(!bundled.source.contains("unusedConst"));
        assert!(!bundled.source.contains("unusedDerived"));
        assert!(!bundled.source.contains("note('c3')"));
        assert!(bundled.source.contains("globalThis.wrapMe = x => x * 2"));
        assert!(
            bundled
                .source
                .contains("const origWrap = globalThis.wrapMe")
        );
        assert!(
            bundled
                .source
                .contains("globalThis.wrapMe = x => origWrap(x) + 1\n\n// main voice")
        );
        assert!(
            bundled
                .source
                .starts_with("/*\n@title night drive\n@by john\n*/\n\nconst baseOffset")
        );
        assert!(
            bundled
                .source
                .ends_with("// main voice\n$: lead().gain(wrapMe(0.2))\n")
        );
    }

    #[test]
    fn isolates_colliding_and_shadowing_file_locals_and_reports_dynamic_prebakes() {
        let global = "\
const s = 0.7
globalThis.double = x => x * s
";
        let local = "\
const factor = 3
globalThis.triple = x => x * factor
for (const k of dynamicList) {
  globalThis[k] = () => s(k)
}
";
        let score = "\
const factor = 10
$: s('bd').gain(double(1) + triple(1) + factor)
";
        let bundled = bundle_score_with_prebakes(score, &[global, local]);
        assert_eq!(bundled.included_statements, 4);
        assert!(bundled.has_dynamic_prebake);
        assert!(
            bundled
                .source
                .contains("{\n  const s = 0.7\n  globalThis.double = x => x * s\n}")
        );
        assert!(
            bundled
                .source
                .contains("{\n  const factor = 3\n  globalThis.triple = x => x * factor\n}")
        );

        // The bundled output parses cleanly without redeclaration errors.
        let allocator = Allocator::default();
        let parsed = Parser::new(&allocator, &bundled.source, SourceType::mjs()).parse();
        assert!(
            !parsed.panicked && parsed.diagnostics.is_empty(),
            "{:?}",
            parsed.diagnostics
        );
        let semantic = SemanticBuilder::new()
            .with_check_syntax_error(true)
            .build(&parsed.program);
        assert!(
            semantic.diagnostics.is_empty(),
            "{:?}",
            semantic.diagnostics
        );
    }

    #[test]
    fn handles_raw_registered_methods_asi_boundaries_and_unparseable_inputs() {
        let prebake = "register('swell', (amt, pat) => pat.gain(amt))";
        let score = "$: s('bd')._swell(0.5)";
        let bundled = bundle_score_with_prebakes(score, &[prebake]);
        assert_eq!(bundled.included_statements, 1);
        assert!(bundled.source.contains("register('swell'"));

        // An unterminated arrow/object expression before a parenthesized score
        // statement gets a protective semicolon so JS ASI cannot merge them.
        let asi_prebake = "globalThis.bump = x => { return x }";
        let asi_score = "(s('bd')).gain(bump(0.5))";
        let asi = bundle_score_with_prebakes(asi_score, &[asi_prebake]);
        assert_eq!(
            asi.source,
            "globalThis.bump = x => { return x };\n\n(s('bd')).gain(bump(0.5))"
        );

        let broken_score = "$: s('bd'(";
        let unparsed = bundle_score_with_prebakes(broken_score, &[prebake]);
        assert!(unparsed.unparseable_score);
        assert_eq!(unparsed.included_statements, 0);
        assert_eq!(unparsed.source, broken_score);
    }
}
