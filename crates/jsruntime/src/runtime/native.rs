//! Scores evaluated without JavaScript.
//!
//! A transpiled score made only of bindings, functions, objects,
//! mini-notation, numbers, signals, free controls, list constructors, tempo
//! setters, built-in methods and lanes builds its graph here, from the same
//! native operations the JavaScript surface calls, and stages it as
//! [`JsRuntime::execute_score`] does. Every name is proved against the slot
//! installation recorded for it, so a score that replaced one is evaluated
//! by JavaScript. The native tier never reports an error: anything it does
//! not prove, or that would fail, is left to the JavaScript tier, which owns
//! every message.

use super::*;
use crate::surface::{
    BuiltinGlobal, BuiltinMethod, ListKind, MINI_CALLEE, PristineNames, SPECIAL_REGISTERED_METHODS,
    prove_builtin_global, prove_builtin_method, prove_lane_method, prove_mini_callee,
    prove_surface_intrinsics,
};
use rustel_transpiler::{ScoreExpr, ScoreFunction, ScoreProgram, ScoreStatement};

/// A value of a natively evaluated score.
#[derive(Clone)]
enum Native<'p> {
    Pattern(Pattern),
    Number(f64),
    /// A raw string, which the native tier only passes on unread.
    String,
    /// An object's properties, in the order JavaScript enumerates them.
    Object(Rc<[(&'p str, Native<'p>)]>),
}

impl Native<'_> {
    /// The value as the surface reifies an argument: a pattern as it is, a
    /// number as a pure number. A raw string would go through a string
    /// parser a score may set, and an object would keep its JavaScript
    /// identity, so both are left to JavaScript.
    fn reified(&self) -> Option<Pattern> {
        match self {
            Self::Pattern(pattern) => Some(pattern.clone()),
            Self::Number(value) => Some(rustel_core::pure(rustel_core::Value::F64(*value))),
            Self::String | Self::Object(_) => None,
        }
    }
}

/// One playing lane, by the name it is collected under.
struct Lane {
    name: String,
    tag: String,
    pattern: Pattern,
}

/// What evaluating the statements of a score built.
#[derive(Default)]
struct Turn<'p> {
    /// The functions the score declares. JavaScript hoists them, so each is
    /// callable from the first statement.
    functions: BTreeMap<&'p str, &'p ScoreFunction>,
    bindings: BTreeMap<&'p str, Native<'p>>,
    /// The names of each function call in progress, innermost last.
    frames: Vec<Frame<'p>>,
    lanes: Vec<Lane>,
    /// The next number of an anonymous `$` lane.
    anonymous: u32,
    /// The tempo the score set last, in cycles per second.
    cps: Option<f64>,
    /// How deep the evaluation is, across calls, and how many expressions
    /// it evaluated. Past either bound the score is left to JavaScript, so
    /// a deep or runaway score cannot exhaust the stack or the time here.
    depth: usize,
    steps: usize,
}

/// The deepest evaluation, across function calls, the native tier takes on.
const MAX_DEPTH: usize = 128;

/// The most expressions the native tier evaluates for one score.
const MAX_STEPS: usize = 1 << 16;

/// The names one function call binds: its parameters, then its bindings,
/// which hold no value until their statement runs.
struct Frame<'p>(Vec<(&'p str, Option<Native<'p>>)>);

/// What a name of a score means where it is read.
enum Resolved<'t, 'p> {
    /// A value the score bound, or `None` before its binding runs.
    Bound(Option<&'t Native<'p>>),
    Function(&'p ScoreFunction),
    /// A name the score does not bind, for the host to resolve.
    Host,
}

/// A natively built score, ready to stage.
pub(super) struct NativeScore {
    pattern: Pattern,
    cps: Option<f64>,
}

/// Where the native tier reads what a name means.
enum Names<'a, 'js> {
    /// An installed realm: each name must still hold its recorded slot.
    Realm(&'a Ctx<'js>),
    /// A realm with no surface yet, where no score has run: the names a
    /// fresh installation proves.
    Pristine(&'a PristineNames),
}

impl Names<'_, '_> {
    fn method(&self, name: &str) -> Option<BuiltinMethod> {
        match self {
            Self::Realm(ctx) => prove_builtin_method(ctx, name),
            Self::Pristine(names) => names.method(name),
        }
    }

    fn global(&self, name: &str) -> Option<BuiltinGlobal> {
        match self {
            Self::Realm(ctx) => prove_builtin_global(ctx, name),
            Self::Pristine(names) => names.global(name),
        }
    }

    fn mini(&self) -> bool {
        match self {
            Self::Realm(ctx) => prove_mini_callee(ctx),
            Self::Pristine(_) => true,
        }
    }

    fn lane(&self) -> bool {
        match self {
            Self::Realm(ctx) => prove_lane_method(ctx),
            Self::Pristine(_) => true,
        }
    }
}

thread_local! {
    /// What a fresh installation proves, taken once per thread from a
    /// throwaway realm. A realm with no surface reads its names here.
    static PRISTINE: std::cell::OnceCell<Option<Rc<PristineNames>>> =
        const { std::cell::OnceCell::new() };
}

/// The names of a fresh installation. See [`PRISTINE`].
fn pristine_names() -> Option<Rc<PristineNames>> {
    PRISTINE.with(|pristine| {
        pristine
            .get_or_init(|| {
                let realm = JsRuntime::new().ok()?;
                realm.ensure_surface().ok()?;
                with_ctx(&realm, |ctx| PristineNames::of(&ctx)).map(Rc::new)
            })
            .clone()
    })
}

impl JsRuntime {
    /// Build the score in transpiled `code` without JavaScript. `None` when
    /// the native tier does not prove the whole score: the JavaScript tier
    /// then evaluates it, and owns every error.
    pub(super) fn native_score(&self, code: &str) -> Option<NativeScore> {
        if !self.native_tier.get() {
            return None;
        }
        let program = rustel_transpiler::score_program(code)?;
        if self.bindings_installed.get() {
            with_ctx(self, |ctx| {
                (prove_surface_intrinsics(&ctx) && !gain_curve_set(&ctx))
                    .then(|| self.build_native(&program, &Names::Realm(&ctx)))
                    .flatten()
            })
        } else {
            let names = pristine_names()?;
            self.build_native(&program, &Names::Pristine(&names))
        }
    }

    fn build_native(&self, program: &ScoreProgram, names: &Names<'_, '_>) -> Option<NativeScore> {
        // Composers read the default alignment when they are called, and
        // every score starts from `In`.
        rustel_core::compose::set_default_alignment(rustel_core::compose::Alignment::In);
        let mut turn = Turn::default();
        for statement in &program.statements {
            if let ScoreStatement::Function(function) = statement
                && turn.functions.insert(&function.name, function).is_some()
            {
                return None;
            }
        }
        let mut last = None;
        for statement in &program.statements {
            last = match statement {
                ScoreStatement::Binding { name, value } => {
                    let value = turn.evaluate(names, value, self)?;
                    // A name the host or a function of the score holds would
                    // be shadowed or declared twice.
                    if names.global(name).is_some()
                        || turn.functions.contains_key(&**name)
                        || turn.bindings.insert(name, value).is_some()
                    {
                        return None;
                    }
                    None
                }
                ScoreStatement::Expression(expression) => {
                    Some(turn.evaluate(names, expression, self)?)
                }
                ScoreStatement::Function(_) => None,
            };
        }
        let pattern = if turn.lanes.is_empty() {
            // With no lane, the score plays its last expression.
            let Native::Pattern(pattern) = last? else {
                return None;
            };
            pattern
        } else {
            rustel_core::stack(
                turn.lanes
                    .into_iter()
                    .map(|lane| crate::install::lane_tagged(&lane.pattern, lane.tag))
                    .collect(),
            )
        };
        Some(NativeScore {
            pattern,
            cps: turn.cps,
        })
    }

    /// Stage a natively built score as [`Self::execute_score`] stages one:
    /// its tempo with the score's effects, and its pattern where the commit
    /// picks it up.
    ///
    /// The JavaScript tier puts a final rewrite over an impure graph, for
    /// the events a worklet graph rides on. Only JavaScript makes such an
    /// event, so a native graph goes in play without it.
    pub(super) fn stage_native(&self, score: NativeScore) -> Result<(), QueryError> {
        if let Some(cps) = score.cps {
            let mut transaction = self.effect_transaction.borrow_mut();
            let effects = transaction.as_mut().ok_or_else(|| {
                QueryError::Message("a native score set its tempo outside a score turn".into())
            })?;
            effects.cps = Some(cps);
        }
        let offsets = self.timeline_state.take();
        self.last_good_timeline.borrow_mut().get_or_insert(offsets);
        with_ctx(self, |ctx| {
            let wrapper = derive_wrapper(ctx.clone(), score.pattern, &[])
                .map_err(|error| QueryError::Message(describe_js_error(&ctx, error)))?;
            host_score_candidate(&ctx)
                .and_then(|candidate| candidate.set(0, wrapper))
                .map_err(|error| QueryError::Message(describe_js_error(&ctx, error)))
        })?;
        self.evaluated_natively.set(true);
        Ok(())
    }
}

impl<'p> Turn<'p> {
    fn evaluate(
        &mut self,
        names: &Names<'_, '_>,
        expression: &'p ScoreExpr,
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        self.steps += 1;
        if self.steps > MAX_STEPS || self.depth >= MAX_DEPTH {
            return None;
        }
        self.depth += 1;
        let value = self.evaluate_here(names, expression, runtime);
        self.depth -= 1;
        value
    }

    fn evaluate_here(
        &mut self,
        names: &Names<'_, '_>,
        expression: &'p ScoreExpr,
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        Some(match expression {
            ScoreExpr::Number(value) => Native::Number(*value),
            ScoreExpr::String(_) => Native::String,
            ScoreExpr::Name(name) => match self.resolve(name) {
                Resolved::Bound(value) => value?.clone(),
                Resolved::Function(_) => return None,
                Resolved::Host => match names.global(name)? {
                    BuiltinGlobal::Signal(pattern) => Native::Pattern(pattern),
                    _ => return None,
                },
            },
            ScoreExpr::Object(properties) => self.object(names, properties, runtime)?,
            ScoreExpr::Member { object, property } => {
                let Native::Object(properties) = self.evaluate(names, object, runtime)? else {
                    return None;
                };
                // A property the object does not own reads its prototype.
                let (_, value) = properties.iter().find(|(key, _)| key == &&**property)?;
                value.clone()
            }
            ScoreExpr::Call { callee, arguments } => {
                self.call(names, callee, arguments, runtime)?
            }
            ScoreExpr::Method {
                receiver,
                method,
                arguments,
            } => {
                let Native::Pattern(receiver) = self.evaluate(names, receiver, runtime)? else {
                    return None;
                };
                self.method(names, receiver, method, arguments, runtime)?
            }
        })
    }

    /// An object literal's properties. JavaScript enumerates integer-like
    /// keys first and keeps a repeated key at its first place, so an object
    /// with either is left to JavaScript.
    fn object(
        &mut self,
        names: &Names<'_, '_>,
        properties: &'p [(Box<str>, ScoreExpr)],
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        let mut object: Vec<(&'p str, Native<'p>)> = Vec::with_capacity(properties.len());
        for (key, value) in properties {
            let integer_like = !key.is_empty() && key.bytes().all(|byte| byte.is_ascii_digit());
            if integer_like || object.iter().any(|(held, _)| held == &&**key) {
                return None;
            }
            object.push((key, self.evaluate(names, value, runtime)?));
        }
        Some(Native::Object(object.into()))
    }

    /// What `name` means here: a name of the innermost call, then of the
    /// score. A function body never sees its caller's names.
    fn resolve(&self, name: &str) -> Resolved<'_, 'p> {
        if let Some(Frame(frame)) = self.frames.last()
            && let Some((_, value)) = frame.iter().find(|(bound, _)| *bound == name)
        {
            return Resolved::Bound(value.as_ref());
        }
        if let Some(value) = self.bindings.get(name) {
            return Resolved::Bound(Some(value));
        }
        self.functions
            .get(name)
            .map_or(Resolved::Host, |function| Resolved::Function(function))
    }

    fn call(
        &mut self,
        names: &Names<'_, '_>,
        callee: &str,
        arguments: &'p [ScoreExpr],
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        match self.resolve(callee) {
            Resolved::Bound(_) => return None,
            Resolved::Function(function) => {
                return self.call_function(names, function, arguments, runtime);
            }
            Resolved::Host => {}
        }
        if callee == MINI_CALLEE {
            let [ScoreExpr::String(source), ScoreExpr::Number(offset)] = arguments else {
                return None;
            };
            if !names.mini() || offset.fract() != 0.0 || !offset.is_finite() {
                return None;
            }
            return crate::surface::transpiled_mini(source, *offset as i64)
                .ok()
                .map(Native::Pattern);
        }
        match names.global(callee)? {
            BuiltinGlobal::Control => {
                let spec = crate::pattern::control_registry().get(callee)?;
                let [argument] = arguments else {
                    return None;
                };
                let value = self.pattern_argument(names, argument, runtime)?;
                Some(Native::Pattern(spec.standalone(&value)))
            }
            BuiltinGlobal::List(kind) => {
                let mut patterns = Vec::with_capacity(arguments.len());
                for argument in arguments {
                    let Native::Pattern(pattern) = self.evaluate(names, argument, runtime)? else {
                        return None;
                    };
                    patterns.push(pattern);
                }
                Some(Native::Pattern(list(kind, patterns)?))
            }
            BuiltinGlobal::Tempo { per_minute } => {
                let [ScoreExpr::Number(value)] = arguments else {
                    return None;
                };
                let cps = if per_minute { value / 60.0 } else { *value };
                if !runtime.effect_policy.get().allows(EffectPolicy::TEMPO)
                    || !cps.is_finite()
                    || cps <= 0.0
                {
                    return None;
                }
                self.cps = Some(cps);
                Some(Native::Pattern(rustel_core::silence()))
            }
            // A signal is a value: calling it throws.
            BuiltinGlobal::Signal(_) => None,
        }
    }

    /// Call a function of the score as JavaScript does: every argument
    /// evaluated where the call is, then the body with its parameters bound.
    fn call_function(
        &mut self,
        names: &Names<'_, '_>,
        function: &'p ScoreFunction,
        arguments: &'p [ScoreExpr],
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        let mut values = Vec::with_capacity(arguments.len());
        for argument in arguments {
            values.push(self.evaluate(names, argument, runtime)?);
        }
        // A missing argument is `undefined`, which the native tier does not
        // hold. An extra one is evaluated and not bound.
        if values.len() < function.parameters.len() {
            return None;
        }
        let mut frame = Vec::with_capacity(function.parameters.len() + function.bindings.len());
        let bound = function
            .parameters
            .iter()
            .zip(values.into_iter().map(Some))
            .chain(function.bindings.iter().map(|(name, _)| (name, None)));
        for (name, value) in bound {
            // A name declared twice is a JavaScript error, or a parameter
            // that hides another.
            if frame.iter().any(|(held, _)| *held == &**name) {
                return None;
            }
            frame.push((&**name, value));
        }
        self.frames.push(Frame(frame));
        let result = self.function_body(names, function, runtime);
        self.frames.pop();
        result
    }

    fn function_body(
        &mut self,
        names: &Names<'_, '_>,
        function: &'p ScoreFunction,
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        let parameters = function.parameters.len();
        for (index, (_, value)) in function.bindings.iter().enumerate() {
            let value = self.evaluate(names, value, runtime)?;
            let Frame(frame) = self.frames.last_mut()?;
            frame[parameters + index].1 = Some(value);
        }
        self.evaluate(names, &function.result, runtime)
    }

    fn method(
        &mut self,
        names: &Names<'_, '_>,
        receiver: Pattern,
        method: &str,
        arguments: &'p [ScoreExpr],
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        if method == LANE_METHOD {
            return self.lane(names, receiver, arguments);
        }
        if SPECIAL_REGISTERED_METHODS.contains(&method) || PLUGIN_METHODS.contains(&method) {
            return None;
        }
        let builtin = names.method(method)?;
        match &builtin {
            BuiltinMethod::Pick { index, join } => {
                return self.pick(names, receiver, (*index, *join), arguments, runtime);
            }
            BuiltinMethod::Visual => return self.visual(names, receiver, arguments, runtime),
            // A registered method that sequences or refuses its arguments
            // shapes JavaScript values the native tier does not hold, so it
            // takes its exact count only.
            BuiltinMethod::Registered(registration)
                if arguments.len() != registration.arity.saturating_sub(1) =>
            {
                return None;
            }
            _ => {}
        }
        let mut values = Vec::with_capacity(arguments.len());
        for argument in arguments {
            values.push(self.pattern_argument(names, argument, runtime)?);
        }
        builtin.apply(&receiver, values).map(Native::Pattern)
    }

    /// `selector.pick(lookup)` with one object, as the method builds it:
    /// the object's properties in order, each value reified.
    fn pick(
        &mut self,
        names: &Names<'_, '_>,
        selector: Pattern,
        (index, join): (rustel_core::PickIndexMode, rustel_core::JoinMode),
        arguments: &'p [ScoreExpr],
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        let [lookup] = arguments else {
            return None;
        };
        let Native::Object(properties) = self.evaluate(names, lookup, runtime)? else {
            return None;
        };
        let entries = properties
            .iter()
            .map(|(key, value)| Some(((*key).to_owned(), value.reified()?)))
            .collect::<Option<Vec<_>>>()?;
        let lookup = rustel_core::PickLookup::Object {
            enumerable_len: entries.len(),
            entries,
        };
        Some(Native::Pattern(rustel_core::pick(
            selector, lookup, index, join,
        )))
    }

    /// `pattern.pianoroll(id, slot)` and every other visual: the pattern
    /// tagged with the slot the second argument names, or as it is.
    fn visual(
        &mut self,
        names: &Names<'_, '_>,
        pattern: Pattern,
        arguments: &'p [ScoreExpr],
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        let slot = match arguments {
            [] | [ScoreExpr::String(_)] => None,
            [ScoreExpr::String(_), slot] => match self.evaluate(names, slot, runtime)? {
                Native::Number(slot) => crate::install::visual_slot(slot),
                _ => None,
            },
            _ => return None,
        };
        Some(Native::Pattern(match slot {
            Some(slot) => pattern.with_ui_visual_slot(slot),
            None => pattern,
        }))
    }

    /// An argument as the surface reifies it. See [`Native::reified`].
    fn pattern_argument(
        &mut self,
        names: &Names<'_, '_>,
        argument: &'p ScoreExpr,
        runtime: &JsRuntime,
    ) -> Option<Pattern> {
        self.evaluate(names, argument, runtime)?.reified()
    }

    /// `pattern.p(id, number)`: collect a lane as the REPL's lane method
    /// does. A muted lane plays nothing. A solo lane, a name a JavaScript
    /// object would order apart, and a name used twice are left to
    /// JavaScript.
    fn lane(
        &mut self,
        names: &Names<'_, '_>,
        pattern: Pattern,
        arguments: &[ScoreExpr],
    ) -> Option<Native<'p>> {
        let [ScoreExpr::String(id), ScoreExpr::Number(number)] = arguments else {
            return None;
        };
        if !names.lane() {
            return None;
        }
        let tag = (*number >= 0.0 && number.fract() == 0.0).then(|| format!("#{}", *number as u64));
        if id.starts_with('_') || id.ends_with('_') {
            return Some(Native::Pattern(rustel_core::silence()));
        }
        let mut name = id.to_string();
        if name.contains('$') {
            name.push_str(&self.anonymous.to_string());
            self.anonymous = self.anonymous.saturating_add(1);
        }
        let solo = name.len() > 1 && name.starts_with('S');
        let integer_like = name.parse::<u32>().is_ok();
        if solo || integer_like || self.lanes.iter().any(|lane| lane.name == name) {
            return None;
        }
        self.lanes.push(Lane {
            tag: tag.unwrap_or_else(|| name.clone()),
            name,
            pattern: pattern.clone(),
        });
        Some(Native::Pattern(pattern))
    }
}

/// The REPL's lane method on the Pattern prototype.
const LANE_METHOD: &str = "p";

/// Methods that route notes to a plugin. A plugin lane has a shadow and plugin
/// effects that only the JavaScript tier stages.
const PLUGIN_METHODS: &[&str] = &["vst", "vsti"];

/// What a list constructor builds, as the JavaScript constructor builds it.
fn list(kind: ListKind, mut patterns: Vec<Pattern>) -> Option<Pattern> {
    if patterns.is_empty() {
        return None;
    }
    Some(match kind {
        ListKind::Stack => rustel_core::stack(patterns),
        // One value is the value itself.
        ListKind::SlowCat | ListKind::FastCat if patterns.len() == 1 => {
            patterns.pop().expect("one list value")
        }
        ListKind::SlowCat => rustel_core::slowcat(patterns),
        ListKind::FastCat => rustel_core::fastcat(patterns),
    })
}

/// Whether a score set a gain curve, which every later score's lanes go
/// through in JavaScript.
fn gain_curve_set(ctx: &Ctx<'_>) -> bool {
    ctx.globals()
        .get::<_, rquickjs::Value>(crate::install::native_surface::bindings::GAIN_CURVE)
        .map_or(true, |curve| !curve.is_undefined())
}
