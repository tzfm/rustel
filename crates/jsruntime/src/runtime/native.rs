//! Scores evaluated without JavaScript.
//!
//! A transpiled score made only of bindings, functions, arrows, objects,
//! arrays, mini-notation, numbers, signals, free controls, list
//! constructors, tempo setters, built-in methods and lanes builds its graph
//! here, from the same native operations the JavaScript surface calls, and
//! stages it as [`JsRuntime::execute_score`] does. Every name is proved
//! against the slot installation recorded for it, so a score that replaced
//! one is evaluated by JavaScript. The native tier never reports an error:
//! anything it does not prove, or that would fail, is left to the JavaScript
//! tier, which owns every message.

use super::*;
use crate::surface::{
    BuiltinGlobal, BuiltinMethod, ListKind, MINI_CALLEE, PristineNames, SPECIAL_REGISTERED_METHODS,
    prove_builtin_global, prove_builtin_method, prove_lane_method, prove_mini_callee,
    prove_surface_intrinsics,
};
use rustel_core::callback_ir::{PatternTransformInstruction, PatternTransformProgram};
use rustel_core::register::FunctionArgument;
use rustel_core::value::FunctionRef;
use rustel_transpiler::{ScoreExpr, ScoreFunction, ScoreProgram, ScoreProperty, ScoreStatement};
use std::sync::Arc;

/// A value of a natively evaluated score.
#[derive(Clone)]
enum Native<'p> {
    Pattern(Pattern),
    Number(f64),
    /// A raw string, which the native tier only passes on unread.
    String,
    /// An object's properties, in the order JavaScript enumerates them.
    Object(Rc<[(&'p str, Native<'p>)]>),
    /// An array's elements.
    Array(Rc<[Native<'p>]>),
    /// An arrow of the score, compiled to a pattern transformer.
    Function(FunctionRef),
    /// The parameter of an arrow that is being compiled, after the method
    /// calls its body made on it so far.
    Chain(Chain),
}

impl Native<'_> {
    /// The value as the surface reifies an argument: a pattern as it is, a
    /// number as a pure number. A raw string would go through a string
    /// parser a score may set, and an object or array would keep its
    /// JavaScript identity, so those are left to JavaScript. A transformer
    /// is an argument only where [`Turn::method_arguments`] takes one, and
    /// an arrow's parameter is no value yet.
    fn reified(&self) -> Option<Pattern> {
        match self {
            Self::Pattern(pattern) => Some(pattern.clone()),
            Self::Number(value) => Some(rustel_core::pure(Value::F64(*value))),
            Self::String
            | Self::Object(_)
            | Self::Array(_)
            | Self::Function(_)
            | Self::Chain(_) => None,
        }
    }
}

/// The method calls an arrow's body made on its parameter, as the steps
/// that apply them to any pattern.
#[derive(Clone)]
struct Chain {
    /// The arrow whose parameter this is, by the order of compilation.
    arrow: usize,
    steps: Vec<PatternTransformInstruction>,
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
    /// The names the score binds. JavaScript takes each name from the first
    /// statement, and it holds no value until its own statement runs.
    bindings: BTreeMap<&'p str, Option<Native<'p>>>,
    /// The names of each function call and arrow in progress, innermost
    /// last.
    frames: Vec<Frame<'p>>,
    /// How many arrows the turn compiled, which numbers the next one.
    arrows: usize,
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

/// The names one function call or arrow binds: its parameters, then its
/// bindings, which hold no value until their statement runs.
struct Frame<'p> {
    names: Vec<(&'p str, Option<Native<'p>>)>,
    /// Whether this is an arrow's frame: its body also reads the names of
    /// the frame it is written in. A declared function reads its own names
    /// and the score's, never its caller's.
    arrow: bool,
}

impl<'p> Frame<'p> {
    /// The frame of `function` with `values` for its parameters, in order.
    /// `None` for a name declared twice: a JavaScript error, or a parameter
    /// that hides another.
    fn of(
        function: &'p ScoreFunction,
        values: impl IntoIterator<Item = Native<'p>>,
        arrow: bool,
    ) -> Option<Self> {
        let mut names = Vec::with_capacity(function.parameters.len() + function.bindings.len());
        let bound = function
            .parameters
            .iter()
            .zip(values.into_iter().map(Some))
            .chain(function.bindings.iter().map(|(name, _)| (name, None)));
        for (name, value) in bound {
            if names.iter().any(|(held, _)| *held == &**name) {
                return None;
            }
            names.push((&**name, value));
        }
        Some(Self { names, arrow })
    }
}

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
    /// Build the transpiled score `program` without JavaScript. `None` when
    /// the score is no plain program or the native tier does not prove all
    /// of it: the JavaScript tier then evaluates it, and owns every error.
    pub(super) fn native_score(&self, program: Option<&ScoreProgram>) -> Option<NativeScore> {
        if !self.native_tier.get() {
            return None;
        }
        let program = program?;
        if self.bindings_installed.get() {
            with_ctx(self, |ctx| {
                (prove_surface_intrinsics(&ctx) && !gain_curve_set(&ctx))
                    .then(|| self.build_native(program, &Names::Realm(&ctx)))
                    .flatten()
            })
        } else {
            let names = pristine_names()?;
            self.build_native(program, &Names::Pristine(&names))
        }
    }

    fn build_native(&self, program: &ScoreProgram, names: &Names<'_, '_>) -> Option<NativeScore> {
        // Composers read the default alignment when they are called, and
        // every score starts from `In`.
        rustel_core::compose::set_default_alignment(rustel_core::compose::Alignment::In);
        let mut turn = Turn::default();
        // A name declared twice is a JavaScript error.
        for statement in &program.statements {
            let declared_once = match statement {
                ScoreStatement::Function { name, function } => {
                    !turn.bindings.contains_key(&**name)
                        && turn.functions.insert(name, function).is_none()
                }
                ScoreStatement::Binding { name, .. } => {
                    !turn.functions.contains_key(&**name)
                        && turn.bindings.insert(name, None).is_none()
                }
                ScoreStatement::Expression(_) => true,
            };
            if !declared_once {
                return None;
            }
        }
        let mut last = None;
        for statement in &program.statements {
            last = match statement {
                ScoreStatement::Binding { name, value } => {
                    let value = turn.evaluate(names, value, self)?;
                    *turn.bindings.get_mut(&**name)? = Some(value);
                    None
                }
                ScoreStatement::Expression(expression) => {
                    Some(turn.evaluate(names, expression, self)?)
                }
                ScoreStatement::Function { .. } => None,
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
            ScoreExpr::Array(elements) => {
                let mut array = Vec::with_capacity(elements.len());
                for element in elements {
                    array.push(self.evaluate(names, element, runtime)?);
                }
                Native::Array(array.into())
            }
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
            } => match self.evaluate(names, receiver, runtime)? {
                Native::Pattern(receiver) => {
                    self.method(names, receiver, method, arguments, runtime)?
                }
                Native::Chain(chain) => {
                    Native::Chain(self.chained(names, chain, method, arguments, runtime)?)
                }
                _ => return None,
            },
            ScoreExpr::Arrow(function) => Native::Function(self.arrow(names, function, runtime)?),
        })
    }

    /// Compile `(parameter) => result` to a pattern transformer.
    ///
    /// The body runs once, here, with the parameter standing for any
    /// pattern: each method it calls on the parameter becomes one step of
    /// the program. Every other value the body reads is the value it has
    /// here, which is the value it has at every call, as a score assigns to
    /// no name. The body must give its own parameter back, through steps
    /// that each keep a pure pattern pure.
    fn arrow(
        &mut self,
        names: &Names<'_, '_>,
        function: &'p ScoreFunction,
        runtime: &JsRuntime,
    ) -> Option<FunctionRef> {
        let [_] = function.parameters.as_slice() else {
            return None;
        };
        let arrow = self.arrows;
        self.arrows += 1;
        let parameter = Native::Chain(Chain {
            arrow,
            steps: Vec::new(),
        });
        self.frames.push(Frame::of(function, [parameter], true)?);
        let result = self.function_body(names, function, runtime);
        self.frames.pop();
        let Native::Chain(chain) = result? else {
            return None;
        };
        if chain.arrow != arrow {
            return None;
        }
        let program = Arc::new(PatternTransformProgram::new(chain.steps).ok()?);
        let pure = Arc::clone(&program);
        Some(FunctionRef::native(
            None,
            Arc::new(move |pattern| pure.apply_pure(pattern)),
            Arc::new(move |pattern| program.apply(pattern)),
        ))
    }

    /// Whether an arrow is being compiled. Its body runs again at every
    /// application of the transformer, so it can have no effect of its own.
    fn compiling(&self) -> bool {
        self.frames.iter().any(|frame| frame.arrow)
    }

    /// An object literal's properties, as JavaScript enumerates them: in
    /// the order written, a spread's own properties in their order, and a
    /// repeated key at its first place with its last value. JavaScript
    /// enumerates integer-like keys first, so an object with one is left to
    /// JavaScript.
    fn object(
        &mut self,
        names: &Names<'_, '_>,
        properties: &'p [ScoreProperty],
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        fn set<'p>(object: &mut Vec<(&'p str, Native<'p>)>, key: &'p str, value: Native<'p>) {
            match object.iter_mut().find(|(held, _)| *held == key) {
                Some((_, held)) => *held = value,
                None => object.push((key, value)),
            }
        }
        let mut object = Vec::with_capacity(properties.len());
        for property in properties {
            match property {
                ScoreProperty::Entry { key, value } => {
                    if !key.is_empty() && key.bytes().all(|byte| byte.is_ascii_digit()) {
                        return None;
                    }
                    let value = self.evaluate(names, value, runtime)?;
                    set(&mut object, key, value);
                }
                ScoreProperty::Spread(source) => {
                    let Native::Object(source) = self.evaluate(names, source, runtime)? else {
                        return None;
                    };
                    for (key, value) in source.iter() {
                        set(&mut object, key, value.clone());
                    }
                }
            }
        }
        Some(Native::Object(object.into()))
    }

    /// What `name` means here: a name of the innermost frame, of each frame
    /// an arrow is written in, then of the score.
    fn resolve(&self, name: &str) -> Resolved<'_, 'p> {
        for frame in self.frames.iter().rev() {
            if let Some((_, value)) = frame.names.iter().find(|(bound, _)| *bound == name) {
                return Resolved::Bound(value.as_ref());
            }
            if !frame.arrow {
                break;
            }
        }
        if let Some(value) = self.bindings.get(name) {
            return Resolved::Bound(value.as_ref());
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
                if self.compiling()
                    || !runtime.effect_policy.get().allows(EffectPolicy::TEMPO)
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
        self.frames.push(Frame::of(function, values, false)?);
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
            self.frames.last_mut()?.names[parameters + index].1 = Some(value);
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
        let builtin = builtin_method(names, method)?;
        match &builtin {
            BuiltinMethod::Pick { index, join } => {
                return self.pick(names, receiver, (*index, *join), arguments, runtime);
            }
            BuiltinMethod::Visual => return self.visual(names, receiver, arguments, runtime),
            BuiltinMethod::Layer { keep_original } => {
                return self.layer(names, receiver, *keep_original, arguments, runtime);
            }
            _ => {}
        }
        let (values, _) = self.method_arguments(names, &builtin, arguments, runtime)?;
        builtin.apply(&receiver, values).map(Native::Pattern)
    }

    /// `pattern.layer(...transformers)` or `superimpose`: a stack of what
    /// each transformer makes of the pattern. The method throws for no
    /// argument and for one that is no transformer.
    fn layer(
        &mut self,
        names: &Names<'_, '_>,
        pattern: Pattern,
        keep_original: bool,
        arguments: &'p [ScoreExpr],
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        if arguments.is_empty() {
            return None;
        }
        let mut transformers = Vec::with_capacity(arguments.len());
        for argument in arguments {
            let Native::Function(transformer) = self.evaluate(names, argument, runtime)? else {
                return None;
            };
            transformers.push(transformer);
        }
        Some(Native::Pattern(if keep_original {
            rustel_core::combinators::superimpose(&pattern, &transformers)
        } else {
            rustel_core::combinators::layer(&pattern, &transformers)
        }))
    }

    /// One more method call on an arrow's parameter: the step that applies
    /// the method to any pattern as [`Self::method`] applies it to one.
    fn chained(
        &mut self,
        names: &Names<'_, '_>,
        mut chain: Chain,
        method: &str,
        arguments: &'p [ScoreExpr],
        runtime: &JsRuntime,
    ) -> Option<Chain> {
        let builtin = builtin_method(names, method)?;
        let (values, holds_transformer) =
            self.method_arguments(names, &builtin, arguments, runtime)?;
        chain
            .steps
            .push(builtin.step(method, values, holds_transformer)?);
        Some(chain)
    }

    /// The arguments of a method call as the method reifies them, and
    /// whether the last one is a transformer.
    ///
    /// A registered method that sequences or refuses its arguments shapes
    /// JavaScript values the native tier does not hold, so it takes its
    /// exact count only. A transformer is an argument only where a
    /// registered method takes one, as its last argument.
    fn method_arguments(
        &mut self,
        names: &Names<'_, '_>,
        builtin: &BuiltinMethod,
        arguments: &'p [ScoreExpr],
        runtime: &JsRuntime,
    ) -> Option<(Vec<Pattern>, bool)> {
        let takes_transformer = match builtin {
            BuiltinMethod::Registered(registration) => {
                if arguments.len() != registration.arity.saturating_sub(1) {
                    return None;
                }
                registration.function_argument == FunctionArgument::Transformer
            }
            _ => false,
        };
        let (last, leading) = match arguments.split_last() {
            Some((last, leading)) => (Some(last), leading),
            None => (None, arguments),
        };
        let mut values = Vec::with_capacity(arguments.len());
        for argument in leading {
            values.push(self.pattern_argument(names, argument, runtime)?);
        }
        let mut holds_transformer = false;
        if let Some(last) = last {
            values.push(match self.evaluate(names, last, runtime)? {
                Native::Function(transformer) if takes_transformer => {
                    holds_transformer = true;
                    rustel_core::pure(Value::Function(Box::new(transformer)))
                }
                value => value.reified()?,
            });
        }
        Some((values, holds_transformer))
    }

    /// `selector.pick(lookup)` with one object or array, as the method
    /// builds it: the properties or elements in order, each value reified.
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
        let lookup = match self.evaluate(names, lookup, runtime)? {
            Native::Object(properties) => rustel_core::PickLookup::Object {
                enumerable_len: properties.len(),
                entries: properties
                    .iter()
                    .map(|(key, value)| Some(((*key).to_owned(), value.reified()?)))
                    .collect::<Option<_>>()?,
            },
            Native::Array(elements) => rustel_core::PickLookup::Array {
                enumerable_len: elements.len(),
                length: elements.len(),
                entries: elements
                    .iter()
                    .enumerate()
                    .map(|(index, value)| Some((index, value.reified()?)))
                    .collect::<Option<_>>()?,
            },
            _ => return None,
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
        if self.compiling() || !names.lane() {
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

/// The built-in method `name` names, unless it takes a route of its own.
fn builtin_method(names: &Names<'_, '_>, name: &str) -> Option<BuiltinMethod> {
    if SPECIAL_REGISTERED_METHODS.contains(&name) || PLUGIN_METHODS.contains(&name) {
        return None;
    }
    names.method(name)
}

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
