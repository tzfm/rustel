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
    BuiltinGlobal, BuiltinMethod, ListKind, ListedNames, MINI_CALLEE, SPECIAL_REGISTERED_METHODS,
    prove_builtin_global, prove_builtin_method, prove_lane_method, prove_mini_callee,
    prove_surface_intrinsics,
};
use rustel_core::callback_ir::{PatternTransformInstruction, PatternTransformProgram};
use rustel_core::controls::{ChainArgument, ControlSpec};
use rustel_core::register::FunctionArgument;
use rustel_core::value::FunctionRef;
use rustel_transpiler::{
    Held, ScoreExpr, ScoreFunction, ScoreProgram, ScoreProperty, ScoreStatement, Written,
};

mod memo;

pub(super) use memo::Memo;

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

/// Inline-optimized instruction buffer for an arrow chain, avoiding a heap
/// `Vec` for zero- and one-step arrows (`x => x.fast(2)`).
#[derive(Clone, Default)]
enum ChainSteps {
    #[default]
    Empty,
    One(PatternTransformInstruction),
    Many(Vec<PatternTransformInstruction>),
}

impl ChainSteps {
    fn push(&mut self, step: PatternTransformInstruction) {
        match std::mem::take(self) {
            Self::Empty => *self = Self::One(step),
            Self::One(first) => *self = Self::Many(vec![first, step]),
            Self::Many(mut vec) => {
                vec.push(step);
                *self = Self::Many(vec);
            }
        }
    }
}

/// The method calls an arrow's body made on its parameter, as the steps
/// that apply them to any pattern.
#[derive(Clone)]
struct Chain {
    /// The arrow whose parameter this is, by the order of compilation.
    arrow: usize,
    steps: ChainSteps,
}

/// One call of a control with one argument, as the score writes it.
#[derive(Clone, Copy)]
struct ControlCall<'p> {
    spec: &'static ControlSpec,
    method: &'p str,
    argument: &'p ScoreExpr<'p>,
    written: Written,
}

/// One playing lane, by the name it is collected under.
struct Lane {
    name: String,
    tag: String,
    pattern: Pattern,
}

/// What each name of a score names. A turn looks up each name that it
/// reads, so the map hashes a word at a time.
type Named<'p, T> =
    std::collections::HashMap<&'p str, T, std::hash::BuildHasherDefault<rustel_core::WordHasher>>;

/// What evaluating the statements of a score built.
struct Turn<'p> {
    /// The expressions that the expressions of the program hold.
    held: &'p Held<'p>,
    /// The functions the score declares. JavaScript hoists them, so each is
    /// callable from the first statement.
    functions: Named<'p, (u32, &'p ScoreFunction<'p>)>,
    /// The names the score binds. JavaScript takes each name from the first
    /// statement, and it holds no value until its own statement runs.
    bindings: Named<'p, (u32, Option<Native<'p>>)>,
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
    /// What the build before this one kept, when this build can take from
    /// it. See [`memo`].
    reuse: Option<memo::Reuse<'p>>,
    /// The last byte of the innermost call that the native tier does not
    /// take, in the text of the score: where a host says the score needs
    /// JavaScript.
    left: Option<u32>,
    /// The lists of the chains of control calls before, empty, with their
    /// room for the next chains: a chain inside a chain takes its own.
    spare: TurnScratch<'p>,
    /// The list of the numbers of a call before, with its room for the
    /// next call.
    numbers: Vec<Value>,
    /// Per-evaluation cache of parsed mini-notation literals (control_id = 0)
    /// and directly named controls (control_id = spec.id) evaluated inside
    /// function or arrow bodies, dropped when the build ends.
    mini_cache: std::collections::HashMap<
        (u16, usize, usize, i64),
        Pattern,
        std::hash::BuildHasherDefault<rustel_core::WordHasher>,
    >,
}

/// One control call of a chain, as [`rustel_core::controls::apply_chain`]
/// takes it.
type ChainCall = rustel_core::controls::ChainCall<'static>;

/// The deepest evaluation, across function calls, the native tier takes on.
const MAX_DEPTH: usize = 128;

/// The most expressions the native tier evaluates for one score.
const MAX_STEPS: usize = 1 << 16;

#[derive(Default)]
struct TurnScratch<'p> {
    calls: Vec<Vec<ControlCall<'p>>>,
    chains: Vec<Vec<ChainCall>>,
    frame_names: Vec<Vec<(&'p str, Option<Native<'p>>)>>,
    call_values: Vec<Vec<Native<'p>>>,
}

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
        mut names: Vec<(&'p str, Option<Native<'p>>)>,
    ) -> Option<Self> {
        names.clear();
        names.reserve(function.parameters.len() + function.bindings.len());
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
    /// A value a function call or an arrow in progress bound, with the
    /// number of that call among them, or `None` before its binding runs.
    Local(usize, Option<&'t Native<'p>>),
    /// A value the score bound, or `None` before its binding runs, with
    /// the number of the name among the names of the score.
    Bound(u32, Option<&'t Native<'p>>),
    /// A function the score declares, with the number of its name.
    Function(u32, &'p ScoreFunction<'p>),
    /// A name the score does not bind, for the host to resolve.
    Host,
}

/// A natively built score, ready to stage.
pub(super) struct NativeScore {
    pub(super) pattern: Pattern,
    pub(super) cps: Option<f64>,
}

/// Where the native tier reads what a name means.
enum Names<'a, 'js> {
    /// An installed realm: each name must still hold its recorded slot.
    Realm(&'a Ctx<'js>),
    /// A realm with no surface yet, where no score has run: the names a
    /// fresh installation proves.
    Listed(ListedNames),
}

impl Names<'_, '_> {
    fn method(&self, name: &str) -> Option<BuiltinMethod> {
        match self {
            Self::Realm(ctx) => prove_builtin_method(ctx, name),
            Self::Listed(names) => names.method(name),
        }
    }

    fn global(&self, name: &str) -> Option<BuiltinGlobal> {
        match self {
            Self::Realm(ctx) => prove_builtin_global(ctx, name),
            Self::Listed(names) => names.global(name),
        }
    }

    fn is_signal(&self, name: &str) -> bool {
        match self {
            Self::Realm(ctx) => matches!(
                prove_builtin_global(ctx, name),
                Some(BuiltinGlobal::Signal(_))
            ),
            Self::Listed(names) => names.is_signal(name),
        }
    }

    fn mini(&self) -> bool {
        match self {
            Self::Realm(ctx) => prove_mini_callee(ctx),
            Self::Listed(_) => true,
        }
    }

    fn lane(&self) -> bool {
        match self {
            Self::Realm(ctx) => prove_lane_method(ctx),
            Self::Listed(_) => true,
        }
    }
}

impl JsRuntime {
    /// Build the transpiled score `program` without JavaScript. `None` when
    /// the score is no plain program or the native tier does not prove all
    /// of it: the JavaScript tier then evaluates it, and owns every error.
    ///
    /// `source` is the text that `program` was read from, when the reader
    /// of the text of a score read it. The build then takes each part that
    /// the build before made of the same text. See [`memo`].
    pub(super) fn native_score(
        &self,
        program: Option<&ScoreProgram>,
        source: Option<&str>,
    ) -> Option<NativeScore> {
        self.native_left.set(None);
        if !self.native_tier.get() {
            return None;
        }
        let program = program?;
        // A name of a realm can change between two builds, so a build in a
        // realm keeps no part, and the parts of a build before it are of no
        // use to a later one.
        let kept = self.native_memo.take();
        let built = if self.bindings_installed.get() {
            with_ctx(self, |ctx| {
                if prove_surface_intrinsics(&ctx) && !gain_curve_set(&ctx) {
                    self.build_native(program, &Names::Realm(&ctx), None)
                } else {
                    Err(None)
                }
            })
        } else {
            let reuse = source.map(|source| memo::Reuse::new(source, kept, program));
            self.build_native(program, &Names::Listed(ListedNames), reuse)
        };
        match built {
            Ok((mut score, reuse)) => {
                let pending_source = self.pending_source.borrow_mut().take();
                if let Some(reuse) = reuse {
                    let (mut memo, reused_nodes) = reuse.finish(score.cps, pending_source);
                    rustel_core::intern::dedup_score_with_reused(
                        &mut score.pattern,
                        &reused_nodes,
                        |remap| {
                            memo.remap_nodes(remap);
                        },
                    );
                    self.native_memo.replace(memo);
                } else {
                    rustel_core::intern::dedup_score(&mut score.pattern, |_| {});
                }
                Some(score)
            }
            Err(left) => {
                self.pending_source.borrow_mut().take();
                // The place is in the text that the program was read from.
                self.native_left.set(left.filter(|_| source.is_some()));
                None
            }
        }
    }

    /// Reuse the active native score when `source` is byte-identical to the
    /// last native build and no script engine has started.
    pub fn try_reuse_identical_native_score(
        &self,
        source: &str,
    ) -> Option<(Pattern, Option<f64>, rustel_core::settings::RuntimeSettings)> {
        if !self.native_tier.get() || self.bindings_installed.get() || self.realm.get().is_some() {
            return None;
        }
        let memo = self.native_memo.borrow();
        if memo.source.as_deref() != Some(source) {
            return None;
        }
        let pattern = self
            .native_active
            .borrow()
            .as_ref()?
            .clone()
            .with_runtime_settings(self.core_settings.clone());
        self.evaluated_natively.set(true);
        self.native_left.set(None);
        Some((pattern, memo.cps, self.core_settings.clone()))
    }

    /// The score that `program` builds and what the build keeps, or where
    /// in the text the native tier stopped, when it knows.
    fn build_native<'p>(
        &self,
        program: &'p ScoreProgram<'p>,
        names: &Names<'_, '_>,
        reuse: Option<memo::Reuse<'p>>,
    ) -> Result<(NativeScore, Option<memo::Reuse<'p>>), Option<u32>> {
        // Composers read the default alignment when they are called, and
        // every score starts from `In`.
        rustel_core::compose::set_default_alignment(rustel_core::compose::Alignment::In);
        let mut turn = Turn {
            held: &program.held,
            functions: Named::default(),
            bindings: Named::default(),
            frames: Vec::new(),
            arrows: 0,
            lanes: Vec::new(),
            anonymous: 0,
            cps: None,
            depth: 0,
            steps: 0,
            reuse,
            left: None,
            spare: Default::default(),
            numbers: Vec::new(),
            mini_cache: Default::default(),
        };
        match self.build_statements(program, names, &mut turn) {
            Some(score) => Ok((score, turn.reuse)),
            None => Err(turn.left),
        }
    }

    fn build_statements<'p>(
        &self,
        program: &'p ScoreProgram<'p>,
        names: &Names<'_, '_>,
        turn: &mut Turn<'p>,
    ) -> Option<NativeScore> {
        // A name declared twice is a JavaScript error. Each name has the
        // number of its statement among the statements with a name.
        let mut named = 0;
        for statement in &program.statements {
            let declared_once = match statement {
                ScoreStatement::Function { name, function, .. } => {
                    !turn.bindings.contains_key(&**name)
                        && turn.functions.insert(name, (named, function)).is_none()
                }
                ScoreStatement::Binding { name, .. } => {
                    !turn.functions.contains_key(&**name)
                        && turn.bindings.insert(name, (named, None)).is_none()
                }
                ScoreStatement::Expression(_) => continue,
            };
            if !declared_once {
                return None;
            }
            named += 1;
        }
        let mut last = None;
        for statement in &program.statements {
            last = match statement {
                ScoreStatement::Binding { name, value, .. } => {
                    // The name reads what its statement reads.
                    if let Some(reuse) = &mut turn.reuse {
                        reuse.begin(0);
                    }
                    let value = turn.part(names, value, self);
                    let (number, bound) = turn.bindings.get_mut(&**name)?;
                    if let Some(reuse) = &mut turn.reuse {
                        let read = reuse.end();
                        reuse.named(*number, read);
                    }
                    *bound = Some(value?);
                    None
                }
                ScoreStatement::Expression(expression) => Some(turn.part(names, expression, self)?),
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
                std::mem::take(&mut turn.lanes)
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
    ///
    /// A native graph is quick to walk, so it goes in play marked as
    /// walked: it keeps no table, and its memory is that of its score. No
    /// wrapper holds it, so the stage runs no step of the realm.
    pub(super) fn stage_native(&self, score: NativeScore) -> Result<(), QueryError> {
        if let Some(cps) = score.cps {
            let mut transaction = self.bridge().effect_transaction.borrow_mut();
            let effects = transaction.effects_mut().ok_or_else(|| {
                QueryError::Message("a native score set its tempo outside a score turn".into())
            })?;
            effects.cps = Some(cps);
        }
        if let Some(offsets) = self
            .timeline_state
            .get()
            .map(rustel_core::TimelineState::take)
            && !offsets.is_empty()
        {
            self.last_good_timeline.borrow_mut().get_or_insert(offsets);
        }
        *self.native_staged.borrow_mut() = Some(score.pattern.mark_walked());
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
        if value.is_none()
            && self.left.is_none()
            && let ScoreExpr::Call { written, .. } | ScoreExpr::Method { written, .. } = expression
            && written.len > 0
        {
            self.left = Some(written.start.saturating_add(written.len - 1));
        }
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
                Resolved::Local(frame, value) => {
                    let value = value?.clone();
                    if let Some(reuse) = &mut self.reuse {
                        reuse.reads_frame(frame);
                    }
                    value
                }
                Resolved::Bound(number, value) => {
                    let value = value?.clone();
                    if let Some(reuse) = &mut self.reuse {
                        reuse.reads(number);
                    }
                    value
                }
                Resolved::Function(..) => return None,
                Resolved::Host => match names.global(name)? {
                    BuiltinGlobal::Signal(pattern) => Native::Pattern(pattern),
                    _ => return None,
                },
            },
            ScoreExpr::Object(properties) => self.object(names, properties, runtime)?,
            ScoreExpr::Array(elements) => {
                let mut array = Vec::with_capacity(elements.len());
                for element in self.held.row(*elements) {
                    array.push(self.evaluate(names, element, runtime)?);
                }
                Native::Array(array.into())
            }
            ScoreExpr::Member { object, property } => {
                let object = self.held.at(*object);
                let Native::Object(properties) = self.evaluate(names, object, runtime)? else {
                    return None;
                };
                // A property the object does not own reads its prototype.
                let (_, value) = properties.iter().find(|(key, _)| key == &&**property)?;
                value.clone()
            }
            ScoreExpr::Call {
                callee, arguments, ..
            } => self.call(names, callee, self.held.row(*arguments), runtime)?,
            ScoreExpr::Method {
                receiver,
                method,
                arguments,
                written,
            } => {
                let arguments = self.held.row(*arguments);
                if let [first_arg] = arguments
                    && let Some(BuiltinMethod::Control(first_spec)) = builtin_method(names, method)
                {
                    let first = ControlCall {
                        spec: first_spec,
                        method,
                        argument: first_arg,
                        written: *written,
                    };
                    return self.control_chain_from(names, first, self.held.at(*receiver), runtime);
                }
                match self.receiver(names, self.held.at(*receiver), method, runtime)? {
                    Native::Pattern(receiver) => {
                        self.method(names, receiver, method, arguments, runtime)?
                    }
                    Native::Chain(chain) => {
                        Native::Chain(self.chained(names, chain, method, arguments, runtime)?)
                    }
                    _ => return None,
                }
            }
            ScoreExpr::Arrow(function) => {
                Native::Function(self.arrow(names, self.held.function(*function), runtime)?)
            }
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
            steps: ChainSteps::Empty,
        });
        let names_buf = self.spare.frame_names.pop().unwrap_or_default();
        self.frames
            .push(Frame::of(function, [parameter], true, names_buf)?);
        let result = self.function_body(names, function, runtime);
        if let Some(mut popped) = self.frames.pop() {
            popped.names.clear();
            self.spare.frame_names.push(popped.names);
        }
        let Native::Chain(chain) = result? else {
            return None;
        };
        if chain.arrow != arrow {
            return None;
        }
        match chain.steps {
            ChainSteps::One(PatternTransformInstruction::Apply(mut func)) if func.is_native() => {
                func.clear_name();
                Some(func)
            }
            steps => {
                let program = match steps {
                    ChainSteps::Empty => PatternTransformProgram::new(std::iter::empty()).ok()?,
                    ChainSteps::One(step) => PatternTransformProgram::new([step]).ok()?,
                    ChainSteps::Many(vec) => PatternTransformProgram::new(vec).ok()?,
                };
                Some(FunctionRef::native_program(program))
            }
        }
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
                    let value = self.part(names, value, runtime)?;
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
        for (number, frame) in self.frames.iter().enumerate().rev() {
            if let Some((_, value)) = frame.names.iter().find(|(bound, _)| *bound == name) {
                return Resolved::Local(number, value.as_ref());
            }
            if !frame.arrow {
                break;
            }
        }
        if let Some((number, value)) = self.bindings.get(name) {
            return Resolved::Bound(*number, value.as_ref());
        }
        self.functions
            .get(name)
            .map_or(Resolved::Host, |(number, function)| {
                Resolved::Function(*number, function)
            })
    }

    fn call(
        &mut self,
        names: &Names<'_, '_>,
        callee: &'p str,
        arguments: &'p [ScoreExpr],
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        match self.resolve(callee) {
            Resolved::Local(..) | Resolved::Bound(..) => return None,
            Resolved::Function(number, function) => {
                return self.call_function(names, number, function, arguments, runtime);
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
            let offset = *offset as i64;
            if !self.frames.is_empty() {
                let key = (0u16, source.as_ptr() as usize, source.len(), offset);
                if let Some(cached) = self.mini_cache.get(&key) {
                    return Some(Native::Pattern(cached.clone()));
                }
                let pat = crate::surface::parse_mini_in_room(source, offset).ok()?;
                self.mini_cache.insert(key, pat.clone());
                return Some(Native::Pattern(pat));
            }
            return crate::surface::parse_mini_in_room(source, offset)
                .ok()
                .map(Native::Pattern);
        }
        match names.global(callee)? {
            BuiltinGlobal::Control(spec) => {
                let [argument] = arguments else {
                    return None;
                };
                if let ScoreExpr::Call {
                    callee: inner_callee,
                    arguments: inner_args,
                    ..
                } = argument
                    && inner_callee == MINI_CALLEE
                    && let [ScoreExpr::String(source), ScoreExpr::Number(offset)] =
                        self.held.row(*inner_args)
                    && names.mini()
                    && offset.fract() == 0.0
                    && offset.is_finite()
                {
                    // Account for the inner `m(...)` step.
                    if self.steps >= MAX_STEPS || self.depth >= MAX_DEPTH {
                        return None;
                    }
                    self.steps += 1;
                    let offset = *offset as i64;
                    let spec_id = spec.builtin_id();
                    if !self.frames.is_empty() && spec_id != 0 {
                        let key = (spec_id, source.as_ptr() as usize, source.len(), offset);
                        if let Some(cached) = self.mini_cache.get(&key) {
                            return Some(Native::Pattern(cached.clone()));
                        }
                        let named = if let Ok(at) = usize::try_from(offset)
                            && let Some((value, locations)) =
                                rustel_mini::word_constant_value(source, at)
                        {
                            spec.standalone_word(value, locations)
                        } else {
                            let pat = crate::surface::parse_mini_in_room(source, offset).ok()?;
                            spec.standalone_owned(pat)
                        };
                        self.mini_cache.insert(key, named.clone());
                        return Some(Native::Pattern(named));
                    }
                    if let Ok(at) = usize::try_from(offset)
                        && let Some((value, locations)) =
                            rustel_mini::word_constant_value(source, at)
                    {
                        return Some(Native::Pattern(spec.standalone_word(value, locations)));
                    }
                    let pat = crate::surface::parse_mini_in_room(source, offset).ok()?;
                    return Some(Native::Pattern(spec.standalone_owned(pat)));
                }
                let value = self.pattern_argument(names, argument, runtime)?;
                Some(Native::Pattern(spec.standalone_owned(value)))
            }
            BuiltinGlobal::List(kind) => {
                if arguments.len() <= 8 {
                    let mut buf: [Option<Pattern>; 8] = [const { None }; 8];
                    for (slot, argument) in buf.iter_mut().zip(arguments) {
                        let Native::Pattern(pattern) = self.part(names, argument, runtime)? else {
                            return None;
                        };
                        *slot = Some(pattern);
                    }
                    return Some(Native::Pattern(list_slice(
                        kind,
                        &mut buf[..arguments.len()],
                    )?));
                }
                let mut patterns = Vec::with_capacity(arguments.len());
                for argument in arguments {
                    let Native::Pattern(pattern) = self.part(names, argument, runtime)? else {
                        return None;
                    };
                    patterns.push(pattern);
                }
                // The list holds each argument, or folds it into its node.
                // A folded argument stays a part: an edit of one argument
                // then builds that one, and the list takes the others.
                Some(Native::Pattern(list(kind, patterns)?))
            }
            BuiltinGlobal::Tempo { per_minute } => {
                let [ScoreExpr::Number(value)] = arguments else {
                    return None;
                };
                let cps = if per_minute { value / 60.0 } else { *value };
                if self.compiling()
                    || !runtime.effect_policy().allows(EffectPolicy::TEMPO)
                    || !cps.is_finite()
                    || cps <= 0.0
                {
                    return None;
                }
                self.cps = Some(cps);
                self.effect();
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
        number: u32,
        function: &'p ScoreFunction,
        arguments: &'p [ScoreExpr],
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        let kept = self.reuse.as_ref().map(memo::Reuse::mark);
        let mut values = self.spare.call_values.pop().unwrap_or_default();
        values.clear();
        values.reserve(arguments.len());
        for argument in arguments {
            let Some(val) = self.part(names, argument, runtime) else {
                values.clear();
                self.spare.call_values.push(values);
                return None;
            };
            values.push(val);
        }
        // A missing argument is `undefined`, which the native tier does not
        // hold. An extra one is evaluated and not bound.
        if values.len() < function.parameters.len() {
            values.clear();
            self.spare.call_values.push(values);
            return None;
        }
        let names_buf = self.spare.frame_names.pop().unwrap_or_default();
        let frame = Frame::of(function, values.drain(..), false, names_buf);
        self.spare.call_values.push(values);
        let frame = frame?;
        // The call reads the function, and the function what its body
        // reads.
        if let Some(reuse) = &mut self.reuse {
            reuse.reads(number);
            reuse.begin(self.frames.len());
        }
        self.frames.push(frame);
        let result = self.function_body(names, function, runtime);
        if let Some(mut popped) = self.frames.pop() {
            popped.names.clear();
            self.spare.frame_names.push(popped.names);
        }
        if let Some(reuse) = &mut self.reuse {
            let read = reuse.end();
            reuse.named(number, read);
            // The result holds each argument, or the body folded it.
            if let Some(kept) = kept {
                reuse.release(kept);
            }
        }
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
            let value = self.part(names, value, runtime)?;
            self.frames.last_mut()?.names[parameters + index].1 = Some(value);
        }
        self.evaluate(names, &function.result, runtime)
    }

    /// The receiver of a call of `method`. The receiver of the lane method
    /// is the code of a statement with a label, so it is a part.
    fn receiver(
        &mut self,
        names: &Names<'_, '_>,
        receiver: &'p ScoreExpr,
        method: &str,
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        if method == LANE_METHOD {
            self.part(names, receiver, runtime)
        } else {
            self.evaluate(names, receiver, runtime)
        }
    }

    /// Collect a chain of control calls starting from `first` and apply them,
    /// keeping chains of up to 12 calls entirely on the stack.
    fn control_chain_from(
        &mut self,
        names: &Names<'_, '_>,
        first: ControlCall<'p>,
        mut under: &'p ScoreExpr<'p>,
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        const INLINE: usize = 12;
        let held = self.held;
        let mut inline = [first; INLINE];
        let mut len = 1usize;
        let mut spilled: Option<Vec<ControlCall<'p>>> = None;
        while let ScoreExpr::Method {
            receiver,
            method,
            arguments,
            written,
        } = under
            && let [argument] = held.row(*arguments)
            && let Some(BuiltinMethod::Control(spec)) = builtin_method(names, method)
        {
            let call = ControlCall {
                spec,
                method,
                argument,
                written: *written,
            };
            if let Some(vec) = &mut spilled {
                vec.push(call);
            } else if len < INLINE {
                inline[len] = call;
                len += 1;
            } else {
                let mut vec = self.spare.calls.pop().unwrap_or_default();
                vec.extend_from_slice(&inline);
                vec.push(call);
                spilled = Some(vec);
            }
            under = held.at(*receiver);
        }
        if let Some(mut calls) = spilled {
            let made = self.control_chain_slice(names, &calls, under, runtime);
            calls.clear();
            self.spare.calls.push(calls);
            made
        } else {
            self.control_chain_slice(names, &inline[..len], under, runtime)
        }
    }

    fn eval_control_arg(
        &mut self,
        names: &Names<'_, '_>,
        call: &ControlCall<'p>,
        runtime: &JsRuntime,
    ) -> Option<ChainArgument> {
        Some(match call.argument {
            ScoreExpr::Number(number) => ChainArgument::Number(*number),
            ScoreExpr::String(text) => ChainArgument::Constant(
                rustel_core::Value::Str(text.as_ref().into()),
                rustel_core::Locations::new(),
            ),
            ScoreExpr::Call {
                callee, arguments, ..
            } if *callee == MINI_CALLEE
                && names.mini()
                && matches!(self.resolve(MINI_CALLEE), Resolved::Host)
                && let [ScoreExpr::String(source), ScoreExpr::Number(offset)] =
                    self.held.row(*arguments)
                && offset.fract() == 0.0
                && offset.is_finite()
                && let Ok(at) = usize::try_from(*offset as i64)
                && let Some((value, locations)) = rustel_mini::word_constant_value(source, at) =>
            {
                self.steps += 1;
                if self.steps > MAX_STEPS {
                    return None;
                }
                ChainArgument::Constant(value, locations)
            }
            argument => {
                if let Some(wave) = self.try_eval_wave_expr(names, argument) {
                    wave
                } else {
                    match self.pattern_argument(names, argument, runtime) {
                        Some(value) => ChainArgument::Pattern(value),
                        None => {
                            if self.left.is_none() && call.written.len > 0 {
                                self.left =
                                    Some(call.written.start.saturating_add(call.written.len - 1));
                            }
                            return None;
                        }
                    }
                }
            }
        })
    }

    /// The pattern of a chain of control calls on `receiver`: each call as
    /// [`Self::method`] makes it, with the constants in a row set in one
    /// step. See [`rustel_core::controls::apply_chain`].
    fn control_chain_slice(
        &mut self,
        names: &Names<'_, '_>,
        calls: &[ControlCall<'p>],
        receiver: &'p ScoreExpr,
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        // Each call but the last is one step that no evaluation counted.
        self.steps += calls.len() - 1;
        if self.steps > MAX_STEPS {
            return None;
        }
        // A pattern, or the parameter of an arrow that is being compiled.
        enum Receiver {
            Pattern(Pattern),
            Parameter(Chain),
        }
        let receiver = match self.evaluate(names, receiver, runtime) {
            Some(Native::Pattern(pattern)) => Receiver::Pattern(pattern),
            Some(Native::Chain(parameter)) => Receiver::Parameter(parameter),
            _ => {
                if let Some(call) = calls.last()
                    && self.left.is_none()
                    && call.written.len > 0
                {
                    self.left = Some(call.written.start.saturating_add(call.written.len - 1));
                }
                return None;
            }
        };
        const INLINE_CHAIN: usize = 12;
        if calls.len() <= INLINE_CHAIN {
            let dummy = calls[0].spec;
            let mut chain: [ChainCall; INLINE_CHAIN] =
                std::array::from_fn(|_| (dummy, ChainArgument::Number(0.0)));
            let mut len = 0usize;
            for call in calls.iter().rev() {
                let argument = self.eval_control_arg(names, call, runtime)?;
                chain[len] = (call.spec, argument);
                len += 1;
            }
            return Some(match receiver {
                Receiver::Pattern(pattern) => Native::Pattern(
                    rustel_core::controls::apply_chain_owned(pattern, &mut chain[..len]),
                ),
                Receiver::Parameter(mut parameter) => {
                    let name = calls.last()?.method;
                    let step =
                        rustel_core::controls::chain_transformer(name, chain.into_iter().take(len));
                    parameter
                        .steps
                        .push(PatternTransformInstruction::Apply(step));
                    Native::Chain(parameter)
                }
            });
        }
        let mut chain = self.spare.chains.pop().unwrap_or_default();
        for call in calls.iter().rev() {
            let argument = self.eval_control_arg(names, call, runtime)?;
            chain.push((call.spec, argument));
        }
        let made = match receiver {
            Receiver::Pattern(pattern) => Native::Pattern(
                rustel_core::controls::apply_chain_owned(pattern, &mut chain),
            ),
            // The arrow applies the chain to each pattern in one step, as
            // a pattern takes it here.
            Receiver::Parameter(mut parameter) => {
                let name = calls.last()?.method;
                let step = rustel_core::controls::chain_transformer(name, chain.drain(..));
                parameter
                    .steps
                    .push(PatternTransformInstruction::Apply(step));
                Native::Chain(parameter)
            }
        };
        chain.clear();
        self.spare.chains.push(chain);
        Some(made)
    }

    /// Try to evaluate a control argument directly as an inline [`rustel_core::Wave`]
    /// without constructing intermediate signal, time, math, or segment pattern nodes.
    fn try_eval_wave_expr(
        &mut self,
        names: &Names<'_, '_>,
        expr: &'p ScoreExpr<'p>,
    ) -> Option<ChainArgument> {
        use rustel_core::compose::{Alignment, ComposeOp, default_alignment};
        use rustel_fraction::Fraction;

        if default_alignment() != Alignment::In {
            return None;
        }
        let mut times: [(bool, Fraction); 3] = [(false, Fraction::ZERO); 3];
        let mut times_len = 0usize;
        let mut math_outer: [(ComposeOp, f64); 8] = [(ComposeOp::Add, 0.0); 8];
        let mut math_len = 0usize;
        let mut steps = 0u32;
        let mut math_depth: Option<u8> = None;
        let mut nodes = 0usize;
        let mut cur = expr;

        while let ScoreExpr::Method {
            receiver,
            method,
            arguments,
            ..
        } = cur
        {
            nodes += 1;
            let args = self.held.row(*arguments);
            let method_name: &str = method;
            match method_name {
                "seg" | "segment" => {
                    if steps != 0 || times_len != 0 {
                        return None;
                    }
                    let Some(BuiltinMethod::Registered(reg)) = builtin_method(names, method_name)
                    else {
                        return None;
                    };
                    if reg.names.first().map(|s| &**s) != Some("segment") {
                        return None;
                    }
                    let [ScoreExpr::Number(n)] = args else {
                        return None;
                    };
                    if !n.is_finite() || n.fract() != 0.0 || *n < 2.0 || *n > u32::MAX as f64 {
                        return None;
                    }
                    steps = *n as u32;
                }
                "slow" | "sparsity" => {
                    let Some(BuiltinMethod::Registered(reg)) = builtin_method(names, method_name)
                    else {
                        return None;
                    };
                    if reg.names.first().map(|s| &**s) != Some("slow") {
                        return None;
                    }
                    let [ScoreExpr::Number(n)] = args else {
                        return None;
                    };
                    let f = Fraction::from_f64(*n)?;
                    if f <= Fraction::ZERO {
                        return None;
                    }
                    let inv = Fraction::ONE.checked_div(f)?;
                    if inv != Fraction::ONE {
                        if times_len >= times.len() {
                            return None;
                        }
                        times[times_len] = (true, inv);
                        times_len += 1;
                    }
                }
                "fast" | "density" => {
                    let Some(BuiltinMethod::Registered(reg)) = builtin_method(names, method_name)
                    else {
                        return None;
                    };
                    if reg.names.first().map(|s| &**s) != Some("fast") {
                        return None;
                    }
                    let [ScoreExpr::Number(n)] = args else {
                        return None;
                    };
                    let f = Fraction::from_f64(*n)?;
                    if f <= Fraction::ZERO {
                        return None;
                    }
                    if f != Fraction::ONE {
                        if times_len >= times.len() {
                            return None;
                        }
                        times[times_len] = (true, f);
                        times_len += 1;
                    }
                }
                "late" | "early" => {
                    let Some(BuiltinMethod::Registered(reg)) = builtin_method(names, method_name)
                    else {
                        return None;
                    };
                    if reg.names.first().map(|s| &**s) != Some(method_name) {
                        return None;
                    }
                    let [ScoreExpr::Number(n)] = args else {
                        return None;
                    };
                    let mut offset = Fraction::from_f64(*n)?;
                    if method_name == "early" {
                        offset = offset.checked_neg()?;
                    }
                    if offset != Fraction::ZERO {
                        if times_len >= times.len() {
                            return None;
                        }
                        times[times_len] = (false, offset);
                        times_len += 1;
                    }
                }
                "range" => {
                    let Some(BuiltinMethod::Registered(reg)) = builtin_method(names, method_name)
                    else {
                        return None;
                    };
                    if reg.names.first().map(|s| &**s) != Some("range") {
                        return None;
                    }
                    let [ScoreExpr::Number(min), ScoreExpr::Number(max)] = args else {
                        return None;
                    };
                    if math_len + 2 > math_outer.len() {
                        return None;
                    }
                    let depth = times_len as u8;
                    match math_depth {
                        None => math_depth = Some(depth),
                        Some(prev) if prev == depth => {}
                        _ => return None,
                    }
                    // Outer-to-inner: Add(min) wraps Mul(max - min).
                    math_outer[math_len] = (ComposeOp::Add, *min);
                    math_outer[math_len + 1] = (ComposeOp::Mul, *max - *min);
                    math_len += 2;
                }
                "add" | "sub" | "mul" | "div" => {
                    let Some(BuiltinMethod::Composer(op)) = builtin_method(names, method_name)
                    else {
                        return None;
                    };
                    let [ScoreExpr::Number(n)] = args else {
                        return None;
                    };
                    if math_len >= math_outer.len() {
                        return None;
                    }
                    let depth = times_len as u8;
                    match math_depth {
                        None => math_depth = Some(depth),
                        Some(prev) if prev == depth => {}
                        _ => return None,
                    }
                    math_outer[math_len] = (op, *n);
                    math_len += 1;
                }
                _ => return None,
            }
            cur = self.held.at(*receiver);
        }

        let ScoreExpr::Name(signal_name) = cur else {
            return None;
        };
        nodes += 1;
        if !matches!(self.resolve(signal_name), Resolved::Host) {
            return None;
        }
        let body = rustel_core::signal::static_signal_body(signal_name)?;
        if !names.is_signal(signal_name) {
            return None;
        }
        let math_inside = match math_depth {
            None => true,
            Some(d) if usize::from(d) == times_len => true,
            Some(0) => false,
            _ => return None,
        };
        math_outer[..math_len].reverse();
        let wave = rustel_core::Wave::from_inline_parts(
            body,
            &times[..times_len],
            &math_outer[..math_len],
            math_inside,
        )?;
        if steps > 0 && wave.has_cuts() {
            return None;
        }
        if self.steps.saturating_add(nodes) > MAX_STEPS {
            return None;
        }
        self.steps += nodes;
        Some(ChainArgument::Wave(wave, steps, math_inside))
    }

    /// Evaluate `expression` where the score writes a part. The build
    /// takes the pattern that the build before made of the same call, or
    /// evaluates the call and keeps its pattern. See [`memo`].
    fn part(
        &mut self,
        names: &Names<'_, '_>,
        expression: &'p ScoreExpr,
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        let (ScoreExpr::Call { written, .. } | ScoreExpr::Method { written, .. }) = expression
        else {
            return self.evaluate(names, expression, runtime);
        };
        let Some(reuse) = self.reuse.as_mut().filter(|_| written.len > 0) else {
            return self.evaluate(names, expression, runtime);
        };
        if let Some(pattern) = reuse.find(written) {
            return Some(Native::Pattern(pattern));
        }
        reuse.begin(self.frames.len());
        let value = self.evaluate(names, expression, runtime);
        let reuse = self.reuse.as_mut()?;
        let read = reuse.end();
        let value = value?;
        if let Native::Pattern(pattern) = &value {
            reuse.keep(*written, pattern, read);
        }
        Some(value)
    }

    /// The build ran an effect of the score.
    fn effect(&mut self) {
        if let Some(reuse) = &mut self.reuse {
            reuse.effect();
        }
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
        // A registration reads the value of each pure argument. It takes
        // numbers, single-word mini literals, and trailing arrow transformers
        // directly without wrapping each argument in a Pattern.
        if let BuiltinMethod::Registered(registration) = &builtin
            && arguments.len() == registration.arity.saturating_sub(1)
        {
            if arguments.len() <= 4 {
                let takes_transformer =
                    registration.function_argument == FunctionArgument::Transformer;
                let allow_mini_word = !registration.is_native_patterned();
                let (last, leading) = match arguments.split_last() {
                    Some((last, leading)) => (Some(last), leading),
                    None => (None, arguments),
                };
                let mut stack_vals = [const { Value::Undefined }; 4];
                let mut mini_steps = 0usize;
                let mut direct = true;
                let mut try_literal = |argument: &'p ScoreExpr<'p>, slot: &mut Value| -> bool {
                    match argument {
                        ScoreExpr::Number(number) => {
                            *slot = Value::F64(*number);
                            true
                        }
                        ScoreExpr::Call {
                            callee, arguments, ..
                        } if allow_mini_word
                            && *callee == MINI_CALLEE
                            && names.mini()
                            && matches!(self.resolve(MINI_CALLEE), Resolved::Host)
                            && let [ScoreExpr::String(source), ScoreExpr::Number(offset)] =
                                self.held.row(*arguments)
                            && offset.fract() == 0.0
                            && offset.is_finite()
                            && let Ok(at) = usize::try_from(*offset as i64)
                            && let Some((value, _)) =
                                rustel_mini::word_constant_value(source, at) =>
                        {
                            *slot = value;
                            mini_steps += 1;
                            true
                        }
                        _ => false,
                    }
                };
                for (slot, argument) in stack_vals.iter_mut().zip(leading) {
                    if !try_literal(argument, slot) {
                        direct = false;
                        break;
                    }
                }
                if direct
                    && let Some(last) = last
                    && !try_literal(last, &mut stack_vals[leading.len()])
                {
                    if matches!(last, ScoreExpr::Arrow(_)) && takes_transformer {
                        if let Native::Function(transformer) =
                            self.evaluate(names, last, runtime)?
                        {
                            stack_vals[leading.len()] = Value::function(transformer);
                        } else {
                            return None;
                        }
                    } else {
                        direct = false;
                    }
                }
                if direct {
                    if mini_steps > 0 {
                        self.steps += mini_steps;
                        if self.steps > MAX_STEPS {
                            return None;
                        }
                    }
                    return Some(Native::Pattern(
                        registration.call_unpatternified(&stack_vals[..arguments.len()], receiver),
                    ));
                }
            } else {
                let mut numbers = std::mem::take(&mut self.numbers);
                numbers.clear();
                numbers.extend(arguments.iter().map_while(|argument| match argument {
                    ScoreExpr::Number(number) => Some(Value::F64(*number)),
                    _ => None,
                }));
                if numbers.len() == arguments.len() {
                    let made = registration.call_unpatternified(&numbers, receiver);
                    self.numbers = numbers;
                    return Some(Native::Pattern(made));
                }
                self.numbers = numbers;
            }
        }
        if arguments.len() <= 4 {
            let takes_transformer = match &builtin {
                BuiltinMethod::Registered(registration) => {
                    if arguments.len() != registration.arity.saturating_sub(1) {
                        return None;
                    }
                    registration.function_argument == FunctionArgument::Transformer
                }
                _ => false,
            };
            let mut buf: [Option<Pattern>; 4] = [None, None, None, None];
            let (last, leading) = match arguments.split_last() {
                Some((last, leading)) => (Some(last), leading),
                None => (None, arguments),
            };
            for (slot, argument) in buf.iter_mut().zip(leading) {
                *slot = Some(self.pattern_argument(names, argument, runtime)?);
            }
            if let Some(last) = last {
                buf[leading.len()] = Some(match self.evaluate(names, last, runtime)? {
                    Native::Function(transformer) if takes_transformer => {
                        rustel_core::pure(Value::function(transformer))
                    }
                    value => value.reified()?,
                });
            }
            return builtin
                .apply_small(receiver, &mut buf[..arguments.len()])
                .map(Native::Pattern);
        }
        let (values, _) = self.method_arguments(names, &builtin, arguments, runtime)?;
        builtin.apply(receiver, values).map(Native::Pattern)
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
                    rustel_core::pure(Value::function(transformer))
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
                    .map(|(key, value)| Some(((*key).into(), value.reified()?)))
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
            Some(slot) => pattern.with_ui_visual_slot_owned(slot),
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
        self.effect();
        Some(Native::Pattern(pattern))
    }
}

/// The REPL's lane method on the Pattern prototype.
const LANE_METHOD: &str = "p";

/// Methods that route notes to a plugin. A plugin lane has a shadow and plugin
/// effects that only the JavaScript tier stages.
const PLUGIN_METHODS: &[&str] = &["vst", "vsti"];

/// The built-in method `name` names, unless it takes a route of its own.
/// The inventory lists no method with such a route, so the names of a
/// realm alone are read against those routes.
fn builtin_method(names: &Names<'_, '_>, name: &str) -> Option<BuiltinMethod> {
    let own_route = || SPECIAL_REGISTERED_METHODS.contains(&name) || PLUGIN_METHODS.contains(&name);
    if matches!(names, Names::Realm(_)) && own_route() {
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

fn list_slice(kind: ListKind, patterns: &mut [Option<Pattern>]) -> Option<Pattern> {
    if patterns.is_empty() {
        return None;
    }
    Some(match kind {
        ListKind::Stack => rustel_core::stack_slice(patterns),
        ListKind::SlowCat | ListKind::FastCat if patterns.len() == 1 => {
            patterns[0].take().expect("one list value")
        }
        ListKind::SlowCat => rustel_core::slowcat_slice(patterns),
        ListKind::FastCat => rustel_core::fastcat_slice(patterns),
    })
}

/// Whether a score set a gain curve, which every later score's lanes go
/// through in JavaScript.
fn gain_curve_set(ctx: &Ctx<'_>) -> bool {
    ctx.globals()
        .get::<_, rquickjs::Value>(crate::install::native_surface::bindings::GAIN_CURVE)
        .map_or(true, |curve| !curve.is_undefined())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_inventory_lists_no_method_with_a_route_of_its_own() {
        for name in SPECIAL_REGISTERED_METHODS.iter().chain(PLUGIN_METHODS) {
            assert!(ListedNames.method(name).is_none(), "{name}");
        }
    }
}
