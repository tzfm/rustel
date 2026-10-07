/*
value.rs - JavaScript-compatible semantic values
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

use std::{fmt, sync::Arc};

/// An insertion-ordered JavaScript object.
///
/// Assignment to an existing property replaces its value without moving it,
/// matching ordinary JavaScript objects. Enumeration additionally applies the
/// ECMAScript integer-index rule: canonical array-index keys precede string
/// keys and are ordered numerically.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OrderedMap {
    entries: Vec<(String, Value)>,
}

impl OrderedMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_entries(entries: impl IntoIterator<Item = (String, Value)>) -> Self {
        let mut out = Self::new();
        for (key, value) in entries {
            out.insert(key, value);
        }
        out
    }

    pub fn insert(&mut self, key: String, value: Value) -> Option<Value> {
        if let Some((_, current)) = self.entries.iter_mut().find(|(name, _)| name == &key) {
            return Some(std::mem::replace(current, value));
        }
        self.entries.push((key, value));
        None
    }

    /// Vec-compatible spelling retained for state-control call sites.
    pub fn push(&mut self, entry: (String, Value)) {
        self.insert(entry.0, entry.1);
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries
            .iter()
            .find_map(|(name, value)| (name == key).then_some(value))
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.entries
            .iter_mut()
            .find_map(|(name, value)| (name == key).then_some(value))
    }

    pub fn remove(&mut self, key: &str) -> Option<Value> {
        let index = self.entries.iter().position(|(name, _)| name == key)?;
        Some(self.entries.remove(index).1)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.entries.iter().any(|(name, _)| name == key)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.entries
            .iter()
            .map(|(name, value)| (name.as_str(), value))
    }

    /// ECMAScript `Object.keys` order.
    pub fn js_entries(&self) -> Vec<(&str, &Value)> {
        let mut integer = Vec::new();
        let mut strings = Vec::new();
        for (position, (key, value)) in self.entries.iter().enumerate() {
            if let Some(index) = array_index(key) {
                integer.push((index, key.as_str(), value));
            } else {
                strings.push((position, key.as_str(), value));
            }
        }
        integer.sort_by_key(|(index, _, _)| *index);
        integer
            .into_iter()
            .map(|(_, key, value)| (key, value))
            .chain(strings.into_iter().map(|(_, key, value)| (key, value)))
            .collect()
    }
}

impl<'a> IntoIterator for &'a OrderedMap {
    type Item = (&'a String, &'a Value);
    type IntoIter = std::iter::Map<
        std::slice::Iter<'a, (String, Value)>,
        fn(&(String, Value)) -> (&String, &Value),
    >;

    fn into_iter(self) -> Self::IntoIter {
        fn pair(entry: &(String, Value)) -> (&String, &Value) {
            (&entry.0, &entry.1)
        }
        self.entries.iter().map(pair)
    }
}

fn array_index(key: &str) -> Option<u32> {
    if key.is_empty() || (key.len() > 1 && key.starts_with('0')) {
        return None;
    }
    let value = key.parse::<u32>().ok()?;
    // 2^32 - 1 is not an array index in ECMAScript.
    (value != u32::MAX && value.to_string() == key).then_some(value)
}

/// A pattern transformer carried **as a value**.
///
/// Upstream reifies a function argument into `pure(func)`, so `every(4, rev)`
/// and `jux(x => x.fast(2))` put a callable inside a hap and apply it at query
/// time. The `Value` variant keeps the callable where strudel.cc puts it.
///
/// The kind decides purity. The two basic kinds:
///
/// * `Native` carries the same two typed views `native_combinator!` produces,
///   so applying it to a pure pattern is provably still pure;
/// * `Js` carries only an opaque callback id, so applying it can materialise
///   anything and the result is impure.
///
/// [`Value`] derives `Debug` and `PartialEq`, so both are implemented here by
/// **identity**: two closures are the same function only if they are the same
/// allocation, which is exactly what JavaScript's `===` says about functions.
#[derive(Clone)]
pub struct FunctionRef {
    id: u64,
    name: Option<std::sync::Arc<str>>,
    kind: FunctionKind,
    /// The first runtime that exposed this callable as metadata. An outer
    /// container must not replace the realm already owned by a nested value.
    runtime_settings: Option<crate::settings::RuntimeSettings>,
}

#[derive(Clone)]
enum FunctionKind {
    Native {
        pure: std::sync::Arc<
            dyn Fn(crate::purity::PurePattern) -> crate::purity::PurePattern + Send + Sync,
        >,
        any: std::sync::Arc<dyn Fn(crate::Pattern) -> crate::Pattern + Send + Sync>,
    },
    /// An opaque JavaScript function. Resolved through the callback host.
    Js(crate::CallbackId),
    /// A bounded native candidate whose exact JavaScript callable remains
    /// owned by the callback sidecar as the compatibility fallback.
    JsPatternIr {
        fallback: crate::CallbackId,
        program: crate::callback_ir::PatternTransformProgram,
    },
    /// A registered combinator with its leading arguments already bound -
    /// `every(4, fast(2))`. Native code, but only *provably* pure when those
    /// bound arguments are themselves pure, so the flag is carried rather than
    /// assumed.
    Registered {
        apply: std::sync::Arc<dyn Fn(crate::Pattern) -> crate::Pattern + Send + Sync>,
        provable_pure: bool,
    },
}

static NEXT_FUNCTION_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl FunctionRef {
    /// A transformer with both typed views, built from one body.
    ///
    /// Prefer [`crate::native_function!`], which instantiates a single body
    /// expression against both types so the two views cannot disagree.
    pub fn native(
        name: Option<&str>,
        pure: std::sync::Arc<
            dyn Fn(crate::purity::PurePattern) -> crate::purity::PurePattern + Send + Sync,
        >,
        any: std::sync::Arc<dyn Fn(crate::Pattern) -> crate::Pattern + Send + Sync>,
    ) -> Self {
        Self {
            id: NEXT_FUNCTION_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            name: name.map(std::sync::Arc::from),
            kind: FunctionKind::Native { pure, any },
            runtime_settings: None,
        }
    }

    /// A partially applied registered combinator, as `fast(2)` produces.
    ///
    /// `provable_pure` must be true only when the body is native AND every
    /// bound argument is pure; the caller knows both, this type does not.
    pub fn registered(
        name: &str,
        apply: std::sync::Arc<dyn Fn(crate::Pattern) -> crate::Pattern + Send + Sync>,
        provable_pure: bool,
    ) -> Self {
        Self {
            id: NEXT_FUNCTION_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            name: Some(std::sync::Arc::from(name)),
            kind: FunctionKind::Registered {
                apply,
                provable_pure,
            },
            runtime_settings: None,
        }
    }

    /// A JavaScript function, held only as an opaque id.
    pub fn js(id: crate::CallbackId) -> Self {
        Self {
            id: NEXT_FUNCTION_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            name: None,
            kind: FunctionKind::Js(id),
            runtime_settings: None,
        }
    }

    /// A JavaScript pattern transformer with a proven native candidate.
    pub fn js_pattern_ir(
        fallback: crate::CallbackId,
        program: crate::callback_ir::PatternTransformProgram,
    ) -> Self {
        Self {
            id: NEXT_FUNCTION_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            name: None,
            kind: FunctionKind::JsPatternIr { fallback, program },
            runtime_settings: None,
        }
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    fn attach_runtime_settings(&mut self, settings: crate::settings::RuntimeSettings) {
        // A function imported from another runtime keeps that inner realm;
        // wrapping its container changes neither identity nor module scope.
        if self.runtime_settings.is_none() {
            self.runtime_settings = Some(settings);
        }
    }

    /// True when applying this cannot introduce JavaScript.
    pub fn is_native(&self) -> bool {
        match &self.kind {
            FunctionKind::Native { .. } => true,
            FunctionKind::Registered { provable_pure, .. } => *provable_pure,
            FunctionKind::Js(_) | FunctionKind::JsPatternIr { .. } => false,
        }
    }

    /// The callback id, for a JS function.
    pub fn callback_id(&self) -> Option<crate::CallbackId> {
        match self.kind {
            FunctionKind::Js(id) => Some(id),
            FunctionKind::JsPatternIr { fallback, .. } => Some(fallback),
            _ => None,
        }
    }

    /// Apply to an arbitrary pattern.
    pub fn apply(&self, pattern: crate::Pattern) -> crate::Pattern {
        let owner = self
            .runtime_settings
            .clone()
            .or_else(crate::settings::RuntimeSettings::current_requested);
        if let Some(settings) = owner {
            return settings.with(|| {
                let mut output = self.apply_unbound(pattern);
                output.force_runtime_settings(settings.clone());
                output
            });
        }
        self.apply_unbound(pattern)
    }

    /// Invoke a JavaScript callable as a value-to-value function.
    ///
    /// A bridged function may be used in more than one authored role. Pattern
    /// transformers use [`Self::apply`]; extension nodes implementing generic
    /// predicates use this value boundary. Native/registered pattern
    /// transformers have no value-call contract and report a query error.
    pub fn apply_value(&self, value: &Value) -> Value {
        let owner = self
            .runtime_settings
            .clone()
            .or_else(crate::settings::RuntimeSettings::current_requested);
        if let Some(settings) = owner {
            return settings.with(|| self.apply_value_unbound(value));
        }
        self.apply_value_unbound(value)
    }

    fn apply_value_unbound(&self, value: &Value) -> Value {
        match &self.kind {
            FunctionKind::Js(id) => crate::host_call_value(*id, value),
            FunctionKind::JsPatternIr { fallback, .. } => crate::host_call_value(*fallback, value),
            FunctionKind::Native { .. } | FunctionKind::Registered { .. } => {
                crate::signal_query_error(|| {
                    "pattern transformer cannot be called as a value predicate".into()
                });
                Value::Undefined
            }
        }
    }

    fn apply_unbound(&self, pattern: crate::Pattern) -> crate::Pattern {
        match &self.kind {
            FunctionKind::Native { any, .. } => any(pattern),
            FunctionKind::Registered { apply, .. } => apply(pattern),
            FunctionKind::Js(id) => crate::host_call_pattern(*id, pattern),
            FunctionKind::JsPatternIr { fallback, program } => {
                crate::host_call_pattern_ir(*fallback, program, pattern)
            }
        }
    }

    /// Apply this callable to every `(pattern, index)` pair as one
    /// `echoWith` batch.
    ///
    /// JavaScript callbacks cross one dedicated host boundary because their
    /// raw return values must all be collected before `stack` reifies any of
    /// them. Native fixed-arity functions and reconstructed registered
    /// transformers already return Patterns, so JavaScript's extra argument is
    /// ignored and the same unary body is applied in ascending index order.
    pub fn apply_indexed_batch(&self, patterns: Vec<(crate::Pattern, i64)>) -> Vec<crate::Pattern> {
        let owner = self
            .runtime_settings
            .clone()
            .or_else(crate::settings::RuntimeSettings::current_requested);
        if let Some(settings) = owner {
            return settings.with(|| {
                let mut out = self.apply_indexed_batch_unbound(patterns);
                for pattern in &mut out {
                    pattern.force_runtime_settings(settings.clone());
                }
                out
            });
        }
        self.apply_indexed_batch_unbound(patterns)
    }

    fn apply_indexed_batch_unbound(
        &self,
        patterns: Vec<(crate::Pattern, i64)>,
    ) -> Vec<crate::Pattern> {
        if let Some(id) = match self.kind {
            FunctionKind::Js(id) => Some(id),
            FunctionKind::JsPatternIr { fallback, .. } => Some(fallback),
            _ => None,
        } {
            return crate::host_call_pattern_indexed_batch(id, patterns);
        }

        let mut out = Vec::new();
        if out.try_reserve_exact(patterns.len()).is_err() {
            return vec![crate::query_limit_pattern(crate::QueryLimit::HostMemory)];
        }
        for (pattern, _) in patterns {
            let transformed = match &self.kind {
                FunctionKind::Native { any, .. } => any(pattern),
                FunctionKind::Registered { apply, .. } => apply(pattern),
                FunctionKind::Js(_) | FunctionKind::JsPatternIr { .. } => {
                    unreachable!("handled above")
                }
            };
            out.push(transformed);
            if crate::query_error_pending() {
                break;
            }
        }
        out
    }

    /// Apply to a pure pattern, keeping the purity proof.
    ///
    /// `None` for a JS function: there is no sound way to promise purity for
    /// something that can materialise anything. Callers must fall back to
    /// [`Self::apply`], which yields an impure pattern.
    pub fn apply_pure(
        &self,
        pattern: crate::purity::PurePattern,
    ) -> Option<crate::purity::PurePattern> {
        let owner = self
            .runtime_settings
            .clone()
            .or_else(crate::settings::RuntimeSettings::current_requested);
        if let Some(settings) = owner {
            return settings.with(|| {
                self.apply_pure_unbound(pattern).map(|pattern| {
                    let mut pattern = pattern.pattern().clone();
                    pattern.force_runtime_settings(settings.clone());
                    crate::purity::PurePattern::assert_pure(pattern)
                })
            });
        }
        self.apply_pure_unbound(pattern)
    }

    fn apply_pure_unbound(
        &self,
        pattern: crate::purity::PurePattern,
    ) -> Option<crate::purity::PurePattern> {
        match &self.kind {
            FunctionKind::Native { pure, .. } => Some(pure(pattern)),
            FunctionKind::Registered {
                apply,
                provable_pure,
            } => provable_pure
                .then(|| crate::purity::PurePattern::assert_pure(apply(pattern.pattern().clone()))),
            FunctionKind::Js(_) | FunctionKind::JsPatternIr { .. } => None,
        }
    }

    /// Pure counterpart of [`Self::apply_indexed_batch`].
    ///
    /// A JavaScript callback has no such view; returning `None` is the same
    /// representation-level proof used by unary transformer application.
    pub fn apply_pure_indexed_batch(
        &self,
        patterns: Vec<(crate::purity::PurePattern, i64)>,
    ) -> Option<Vec<crate::purity::PurePattern>> {
        let owner = self
            .runtime_settings
            .clone()
            .or_else(crate::settings::RuntimeSettings::current_requested);
        if let Some(settings) = owner {
            return settings.with(|| {
                self.apply_pure_indexed_batch_unbound(patterns)
                    .map(|mut out| {
                        for pattern in &mut out {
                            let mut owned = pattern.pattern().clone();
                            owned.force_runtime_settings(settings.clone());
                            *pattern = crate::purity::PurePattern::assert_pure(owned);
                        }
                        out
                    })
            });
        }
        self.apply_pure_indexed_batch_unbound(patterns)
    }

    fn apply_pure_indexed_batch_unbound(
        &self,
        patterns: Vec<(crate::purity::PurePattern, i64)>,
    ) -> Option<Vec<crate::purity::PurePattern>> {
        let mut out = Vec::new();
        if out.try_reserve_exact(patterns.len()).is_err() {
            return Some(vec![crate::purity::PurePattern::assert_pure(
                crate::query_limit_pattern(crate::QueryLimit::HostMemory),
            )]);
        }
        match &self.kind {
            FunctionKind::Native { pure, .. } => {
                for (pattern, _) in patterns {
                    out.push(pure(pattern));
                    if crate::query_error_pending() {
                        break;
                    }
                }
                Some(out)
            }
            FunctionKind::Registered {
                apply,
                provable_pure,
            } if *provable_pure => {
                for (pattern, _) in patterns {
                    out.push(crate::purity::PurePattern::assert_pure(apply(
                        pattern.pattern().clone(),
                    )));
                    if crate::query_error_pending() {
                        break;
                    }
                }
                Some(out)
            }
            FunctionKind::Registered { .. }
            | FunctionKind::Js(_)
            | FunctionKind::JsPatternIr { .. } => None,
        }
    }
}

impl fmt::Debug for FunctionRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.kind, &self.name) {
            (FunctionKind::Native { .. }, Some(name)) => write!(f, "fn {name}#{}", self.id),
            (FunctionKind::Native { .. }, None) => write!(f, "fn #{}", self.id),
            (FunctionKind::Registered { .. }, Some(name)) => write!(f, "fn {name}#{}", self.id),
            (FunctionKind::Registered { .. }, None) => write!(f, "fn #{}", self.id),
            (FunctionKind::Js(callback), _) => write!(f, "js fn #{} (cb {callback})", self.id),
            (FunctionKind::JsPatternIr { fallback, program }, _) => write!(
                f,
                "js fn #{} (cb {fallback}, ir v{}:{} ops)",
                self.id,
                program.version(),
                program.len()
            ),
        }
    }
}

/// True when `text` is a [`FunctionRef`] rendered by the `Debug` impl above.
///
/// A hap can legitimately carry a function - it is the intermediate of a
/// join - and the rendering keeps its identity rather than dropping it. Code
/// further down has to tell that apart from a note name, and this sits beside
/// the impl that writes it so the two cannot drift.
pub fn is_rendered_function(text: &str) -> bool {
    text.starts_with("fn ") || text.starts_with("js fn ")
}

/// Function identity, as JavaScript's `===` defines it.
impl PartialEq for FunctionRef {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

/// Builds a [`FunctionRef`] from ONE body expression, instantiated against both
/// purity views - the same trick `native_combinator!` uses, for the same
/// reason: two hand-written bodies could disagree, and one cannot.
#[macro_export]
macro_rules! native_function {
    ($name:expr, |$pat:ident| $body:expr) => {
        $crate::value::FunctionRef::native(
            ::std::option::Option::Some($name),
            ::std::sync::Arc::new(|$pat: $crate::purity::PurePattern| $body),
            ::std::sync::Arc::new(|$pat: $crate::Pattern| $body),
        )
    };
}

/// A Pattern held as an ordinary JavaScript value.
///
/// The graph itself is pure Rust and remains `Send + Sync`; `id` supplies the
/// reference identity JavaScript uses for `===`. Callback cells still live in
/// L2 and are reached only through the graph's opaque callback ids.
#[derive(Clone)]
pub struct PatternValue {
    id: crate::CallbackId,
    pattern: crate::Pattern,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JsValueRef {
    id: crate::CallbackId,
    array: bool,
}

impl JsValueRef {
    pub fn new(id: crate::CallbackId, array: bool) -> Self {
        Self { id, array }
    }

    pub fn id(self) -> crate::CallbackId {
        self.id
    }

    pub fn is_array(self) -> bool {
        self.array
    }
}

impl PatternValue {
    pub fn new(id: crate::CallbackId, pattern: crate::Pattern) -> Self {
        Self { id, pattern }
    }

    pub fn pattern(&self) -> &crate::Pattern {
        &self.pattern
    }

    pub fn id(&self) -> crate::CallbackId {
        self.id
    }
}

impl fmt::Debug for PatternValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("PatternValue").field(&self.id).finish()
    }
}

impl PartialEq for PatternValue {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

#[derive(Clone)]
pub struct HapList(Arc<Vec<crate::Hap>>);

impl HapList {
    pub fn new(haps: Vec<crate::Hap>) -> Self {
        Self(Arc::new(haps))
    }

    pub fn as_slice(&self) -> &[crate::Hap] {
        &self.0
    }
}

impl fmt::Debug for HapList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("HapList").field(&self.0.len()).finish()
    }
}

impl PartialEq for HapList {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// A native representation of values that can cross the score's semantic
/// boundary. Arrays and objects are recursive; object order is observable.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    F64(f64),
    Str(String),
    List(Vec<Value>),
    Object(OrderedMap),
    /// A native Pattern nested in a JavaScript array/object value.
    Pattern(Box<PatternValue>),
    /// An array/object still owned by the JavaScript host. Core carries only
    /// this opaque id; reading or re-exposing it goes through CallbackHost.
    JsValue(JsValueRef),
    /// A pattern transformer. Upstream's `reify(func)` puts one of these in a
    /// hap; see [`FunctionRef`].
    Function(FunctionRef),
    /// Congruent haps carried by `collect()` until a score callback consumes
    /// them.
    Haps(HapList),
}

impl Value {
    pub fn object(entries: impl IntoIterator<Item = (String, Value)>) -> Self {
        Self::Object(OrderedMap::from_entries(entries))
    }

    pub fn is_nullish(&self) -> bool {
        matches!(self, Self::Undefined | Self::Null)
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::F64(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&OrderedMap> {
        match self {
            Self::Object(value) => Some(value),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_object()?.get(key)
    }

    /// JavaScript string coercion used for primitive hap values. For arrays and
    /// objects this returns the compact `Hap.show(true)` representation.
    pub fn show(&self) -> String {
        match self {
            Self::Undefined => "undefined".into(),
            Self::Null => "null".into(),
            Self::Bool(value) => value.to_string(),
            Self::F64(value) => js_number(*value),
            Self::Str(value) => value.clone(),
            Self::List(_) | Self::Object(_) => self.compact_json(),
            // `String(fn)` is the source text on strudel.cc; a stable identity is
            // more useful and never appears in a snapshot.
            Self::Function(f) => format!("{f:?}"),
            Self::Pattern(_) => "[object Object]".into(),
            Self::Haps(haps) => format!("[{}]", haps.0.len()),
            Self::JsValue(_) => crate::materialize_js_value(self).show(),
        }
    }

    /// `JSON.stringify`, for the JSON-compatible value shapes used by scores.
    ///
    /// A top-level `undefined` has no JSON text in JavaScript; this method
    /// returns `None` for that exact case. Undefined object properties are
    /// omitted, while undefined array entries become `null`.
    pub fn json_stringify(&self) -> Option<String> {
        if let Self::Str(value) = self {
            let mut out = String::with_capacity(value.len().saturating_add(2));
            write_json_quote(&mut out, value);
            return Some(out);
        }
        let mut out = String::new();
        write_json_value(&mut out, self, JsonPosition::Top).then_some(out)
    }

    /// Upstream `Hap.show(true)` performs:
    /// `JSON.stringify(v).slice(1,-1).replaceAll('"','').replaceAll(',',' ')`.
    pub fn compact_json(&self) -> String {
        let json = self.json_stringify().unwrap_or_else(|| "undefined".into());
        let inner = if matches!(self, Self::List(_) | Self::Object(_)) && json.len() >= 2 {
            &json[1..json.len() - 1]
        } else {
            &json
        };
        inner.replace('"', "").replace(',', " ")
    }

    /// Does this value carry a JavaScript function anywhere inside it?
    ///
    /// The purity machinery keys off this: a combinator body run under the
    /// `PurePattern` view must not be handed a callable it cannot classify, so
    /// the dispatcher checks the ARGUMENTS as well as the operand.
    pub fn has_js_function(&self) -> bool {
        match self {
            Self::Function(f) => !f.is_native(),
            // Even a native-pure inner pattern carries the identity of its
            // original JS wrapper. Keep it off every host-less fast path.
            Self::Pattern(_) => true,
            Self::Haps(haps) => haps.0.iter().any(|hap| hap.value.has_js_function()),
            Self::JsValue(_) => true,
            Self::List(items) => items.iter().any(Value::has_js_function),
            Self::Object(map) => map.iter().any(|(_, v)| v.has_js_function()),
            _ => false,
        }
    }

    /// Whether resolving this value requires the JavaScript host.
    ///
    /// Used at the outer query boundary so fully native control maps are not
    /// cloned recursively on every hap merely to discover that they contain
    /// no JS-owned identity.
    pub fn contains_js_value(&self) -> bool {
        match self {
            Self::JsValue(_) => true,
            Self::Haps(haps) => haps.0.iter().any(|hap| hap.value.contains_js_value()),
            Self::List(items) => items.iter().any(Value::contains_js_value),
            Self::Object(map) => map.iter().any(|(_, value)| value.contains_js_value()),
            _ => false,
        }
    }

    /// The transformer this value carries, if it is one.
    pub fn as_function(&self) -> Option<&FunctionRef> {
        match self {
            Self::Function(f) => Some(f),
            _ => None,
        }
    }

    /// Attach settings to nested executable metadata without allocating graph
    /// wrappers. Traversal is explicitly bounded because List/Object depth is
    /// user-controlled and Rust stack overflow aborts the process.
    pub(crate) fn attach_runtime_settings(
        &mut self,
        settings: &crate::settings::RuntimeSettings,
        depth: u32,
    ) -> bool {
        if crate::cancellation_requested() || crate::query_deadline_expired() {
            return false;
        }
        if depth > crate::MAX_PATTERN_DEPTH {
            crate::refuse(crate::QueryLimit::GraphDepth {
                depth,
                limit: crate::MAX_PATTERN_DEPTH,
            });
            return false;
        }
        match self {
            Self::List(values) => {
                for value in values {
                    if !value.attach_runtime_settings(settings, depth + 1) {
                        return false;
                    }
                }
            }
            Self::Object(values) => {
                for (_, value) in &mut values.entries {
                    if !value.attach_runtime_settings(settings, depth + 1) {
                        return false;
                    }
                }
            }
            Self::Pattern(value) => {
                value.pattern.attach_runtime_settings(settings.clone());
            }
            Self::Function(function) => {
                function.attach_runtime_settings(settings.clone());
            }
            Self::Haps(haps) => {
                for hap in Arc::make_mut(&mut haps.0) {
                    if !hap
                        .value
                        .attach_runtime_settings(settings, depth.saturating_add(1))
                    {
                        return false;
                    }
                }
            }
            Self::Undefined
            | Self::Null
            | Self::Bool(_)
            | Self::F64(_)
            | Self::Str(_)
            | Self::JsValue(_) => {}
        }
        true
    }

    pub fn js_truthy(&self) -> bool {
        match self {
            Self::Undefined | Self::Null => false,
            Self::Bool(value) => *value,
            Self::F64(value) => *value != 0.0 && !value.is_nan(),
            Self::Str(value) => !value.is_empty(),
            // Functions are objects: always truthy.
            Self::List(_)
            | Self::Object(_)
            | Self::Function(_)
            | Self::Pattern(_)
            | Self::Haps(_)
            | Self::JsValue(_) => true,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.show())
    }
}

#[derive(Clone, Copy)]
enum JsonPosition {
    Top,
    Array,
    Object,
}

fn write_json_value(out: &mut String, value: &Value, position: JsonPosition) -> bool {
    match value {
        // `JSON.stringify` treats a function exactly as it treats `undefined`.
        Value::Undefined | Value::Function(_) => match position {
            JsonPosition::Top | JsonPosition::Object => false,
            JsonPosition::Array => {
                out.push_str("null");
                true
            }
        },
        Value::Pattern(_) => {
            out.push_str("{}");
            true
        }
        Value::Haps(haps) => {
            use std::fmt::Write;
            let _ = write!(out, "[{}]", haps.0.len());
            true
        }
        Value::JsValue(_) => write_json_value(out, &crate::materialize_js_value(value), position),
        Value::Null => {
            out.push_str("null");
            true
        }
        Value::Bool(value) => {
            out.push_str(if *value { "true" } else { "false" });
            true
        }
        Value::F64(value) if !value.is_finite() => {
            out.push_str("null");
            true
        }
        Value::F64(value) => {
            if *value == 0.0 {
                // JSON.stringify(-0) produces "0".
                out.push('0');
            } else {
                let mut buffer = dragonbox_ecma::Buffer::new();
                out.push_str(buffer.format_finite(*value));
            }
            true
        }
        Value::Str(value) => {
            write_json_quote(out, value);
            true
        }
        Value::List(values) => {
            out.push('[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    out.push(',');
                }
                // Array entries always have JSON text: values omitted at the
                // top level or in objects use JavaScript's `null` placeholder.
                write_json_value(out, value, JsonPosition::Array);
            }
            out.push(']');
            true
        }
        Value::Object(values) => {
            out.push('{');
            let mut first = true;
            for (key, value) in values.js_entries() {
                write_json_property(out, key, value, &mut first);
            }
            out.push('}');
            true
        }
    }
}

fn write_json_property(out: &mut String, key: &str, value: &Value, first: &mut bool) {
    match value {
        Value::Undefined | Value::Function(_) => return,
        Value::JsValue(_) => {
            // Resolve omission before writing the key: an omitted property's
            // key must not grow the output buffer, however long it is.
            write_json_property(out, key, &crate::materialize_js_value(value), first);
            return;
        }
        _ => {}
    }
    if !*first {
        out.push(',');
    }
    write_json_quote(out, key);
    out.push(':');
    write_json_value(out, value, JsonPosition::Object);
    *first = false;
}

fn js_number(value: f64) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value == f64::INFINITY {
        return "Infinity".into();
    }
    if value == f64::NEG_INFINITY {
        return "-Infinity".into();
    }
    if value == 0.0 {
        // String(-0) produces "0".
        return "0".into();
    }
    // Rust's Display uses different fixed/exponential thresholds and omits
    // ECMAScript's `+` on a positive exponent. This formatter implements the
    // Number::toString decimal form used by String(number), Hap.show and
    // JSON.stringify.
    let mut buffer = dragonbox_ecma::Buffer::new();
    buffer.format_finite(value).to_owned()
}

fn write_json_quote(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch <= '\u{001f}' => {
                use std::fmt::Write;
                let _ = write!(out, "\\u{:04x}", ch as u32);
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::{OrderedMap, Value};

    #[test]
    fn object_keys_follow_ecmascript_order() {
        let value = Value::object([
            ("b".into(), Value::F64(1.0)),
            ("10".into(), Value::F64(10.0)),
            ("2".into(), Value::F64(2.0)),
            ("a".into(), Value::F64(3.0)),
        ]);
        assert_eq!(
            value.json_stringify().as_deref(),
            Some(r#"{"2":2,"10":10,"b":1,"a":3}"#)
        );
    }

    #[test]
    fn replacement_keeps_string_key_position() {
        let mut value = OrderedMap::new();
        value.insert("a".into(), Value::F64(1.0));
        value.insert("b".into(), Value::F64(2.0));
        value.insert("a".into(), Value::F64(3.0));
        assert_eq!(
            Value::Object(value).json_stringify().as_deref(),
            Some(r#"{"a":3,"b":2}"#)
        );
    }

    #[test]
    fn nested_compact_format_matches_hap_show_algorithm() {
        let value = Value::List(vec![
            Value::Str("c".into()),
            Value::Str("d".into()),
            Value::List(vec![Value::Str("e".into()), Value::Str("f".into())]),
        ]);
        assert_eq!(value.compact_json(), "c d [e f]");
    }

    #[test]
    fn json_handles_js_numeric_and_undefined_rules() {
        assert_eq!(
            Value::F64(f64::NAN).json_stringify().as_deref(),
            Some("null")
        );
        assert_eq!(Value::F64(-0.0).json_stringify().as_deref(), Some("0"));
        assert_eq!(
            Value::List(vec![Value::Undefined])
                .json_stringify()
                .as_deref(),
            Some("[null]")
        );
        assert_eq!(Value::Undefined.json_stringify(), None);
    }

    #[test]
    fn json_materializes_omitted_values_once_in_enumeration_order() {
        struct MaterializeHost {
            omitted: Value,
            calls: std::cell::RefCell<Vec<crate::CallbackId>>,
        }

        impl crate::CallbackHost for MaterializeHost {
            fn call_value(&self, id: crate::CallbackId, _: &Value) -> Result<Value, String> {
                panic!("unexpected value callback {id}")
            }

            fn call_query(
                &self,
                id: crate::CallbackId,
                _: &crate::State,
            ) -> Result<Vec<crate::Hap>, String> {
                panic!("unexpected query callback {id}")
            }

            fn call_materialize_value(&self, id: crate::CallbackId) -> Result<Value, String> {
                self.calls.borrow_mut().push(id);
                Ok(match id {
                    1 | 3 | 5 => self.omitted.clone(),
                    2 | 4 => Value::F64(id as f64),
                    _ => panic!("unexpected materialization callback {id}"),
                })
            }
        }

        let opaque = |id| Value::JsValue(super::JsValueRef::new(id, false));
        // Integer keys move ahead of string keys. The resulting traversal
        // omits its first, middle, and last properties after materialization.
        let object = Value::object([
            ("middle".into(), opaque(3)),
            ("10".into(), opaque(2)),
            ("2".into(), opaque(1)),
            ("after".into(), opaque(4)),
            ("last".into(), opaque(5)),
        ]);
        let array = Value::List((1..=5).map(opaque).collect());
        let all_omitted = Value::object([("tail".into(), opaque(3)), ("2".into(), opaque(1))]);

        for omitted in [
            Value::Undefined,
            Value::Function(super::FunctionRef::js(99)),
        ] {
            let host = MaterializeHost {
                omitted,
                calls: std::cell::RefCell::new(Vec::new()),
            };
            crate::with_callback_host(&host, || {
                assert_eq!(
                    object.json_stringify().as_deref(),
                    Some(r#"{"10":2,"after":4}"#)
                );
                assert_eq!(host.calls.take(), vec![1, 2, 3, 4, 5]);

                assert_eq!(
                    array.json_stringify().as_deref(),
                    Some("[null,2,null,4,null]")
                );
                assert_eq!(host.calls.take(), vec![1, 2, 3, 4, 5]);

                assert_eq!(all_omitted.json_stringify().as_deref(), Some("{}"));
                assert_eq!(host.calls.take(), vec![1, 3]);

                assert_eq!(opaque(1).json_stringify(), None);
                assert_eq!(host.calls.take(), vec![1]);

                let omitted_long_key = Value::object([("\"".repeat(16_384), opaque(3))]);
                let mut output = String::with_capacity(8);
                let capacity = output.capacity();
                assert!(super::write_json_value(
                    &mut output,
                    &omitted_long_key,
                    super::JsonPosition::Top
                ));
                assert_eq!(output, "{}");
                assert_eq!(output.capacity(), capacity);
                assert_eq!(host.calls.take(), vec![3]);
            });
        }
    }

    #[test]
    fn numeric_text_uses_ecmascript_thresholds() {
        let cases = [
            (1e20, "100000000000000000000"),
            (1e21, "1e+21"),
            (1e-6, "0.000001"),
            (1e-7, "1e-7"),
            (1.2345678901234568e30, "1.2345678901234568e+30"),
            (-0.0, "0"),
        ];
        for (value, expected) in cases {
            assert_eq!(Value::F64(value).show(), expected);
            assert_eq!(
                Value::F64(value).json_stringify().as_deref(),
                Some(expected)
            );
        }
    }
}
