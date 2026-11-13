//! Scores evaluated without JavaScript.
//!
//! A transpiled score made only of bindings, mini-notation, numbers, free
//! controls, list constructors, tempo setters, built-in methods and lanes
//! builds its graph here, from the same native operations the JavaScript
//! surface calls, and stages it as [`JsRuntime::execute_score`] does. Every
//! name is proved against the slot installation recorded for it, so a score
//! that replaced one is evaluated by JavaScript. The native tier never
//! reports an error: anything it does not prove, or that would fail, is left
//! to the JavaScript tier, which owns every message.

use super::*;
use crate::surface::{
    BuiltinGlobal, BuiltinMethod, ListKind, MINI_CALLEE, PristineNames, SPECIAL_REGISTERED_METHODS,
    prove_builtin_global, prove_builtin_method, prove_lane_method, prove_mini_callee,
    prove_surface_intrinsics,
};
use rustel_transpiler::{ScoreExpr, ScoreStatement};

/// A value of a natively evaluated score.
enum Native {
    Pattern(Pattern),
    Number(f64),
    /// A raw string, which the native tier only passes on unread.
    String,
}

/// One playing lane, by the name it is collected under.
struct Lane {
    name: String,
    tag: String,
    pattern: Pattern,
}

/// What evaluating the statements of a score built.
#[derive(Default)]
struct Turn {
    bindings: BTreeMap<Box<str>, Pattern>,
    lanes: Vec<Lane>,
    /// The next number of an anonymous `$` lane.
    anonymous: u32,
    /// The tempo the score set last, in cycles per second.
    cps: Option<f64>,
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

    fn build_native(
        &self,
        program: &rustel_transpiler::ScoreProgram,
        names: &Names<'_, '_>,
    ) -> Option<NativeScore> {
        // Composers read the default alignment when they are called, and
        // every score starts from `In`.
        rustel_core::compose::set_default_alignment(rustel_core::compose::Alignment::In);
        let mut turn = Turn::default();
        let mut last = None;
        for statement in &program.statements {
            last = match statement {
                ScoreStatement::Binding { name, value } => {
                    let Native::Pattern(pattern) = turn.evaluate(names, value, self)? else {
                        return None;
                    };
                    if names.global(name).is_some()
                        || turn.bindings.insert(name.clone(), pattern).is_some()
                    {
                        return None;
                    }
                    None
                }
                ScoreStatement::Expression(expression) => {
                    Some(turn.evaluate(names, expression, self)?)
                }
            };
        }
        let pattern = if turn.lanes.is_empty() {
            // With no lane, the score plays its last expression.
            match last? {
                Native::Pattern(pattern) => pattern,
                Native::Number(_) | Native::String => return None,
            }
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

impl Turn {
    fn evaluate(
        &mut self,
        names: &Names<'_, '_>,
        expression: &ScoreExpr,
        runtime: &JsRuntime,
    ) -> Option<Native> {
        Some(match expression {
            ScoreExpr::Number(value) => Native::Number(*value),
            ScoreExpr::String(_) => Native::String,
            ScoreExpr::Name(name) => Native::Pattern(self.bindings.get(name)?.clone()),
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

    fn call(
        &mut self,
        names: &Names<'_, '_>,
        callee: &str,
        arguments: &[ScoreExpr],
        runtime: &JsRuntime,
    ) -> Option<Native> {
        if self.bindings.contains_key(callee) {
            return None;
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
        }
    }

    fn method(
        &mut self,
        names: &Names<'_, '_>,
        receiver: Pattern,
        method: &str,
        arguments: &[ScoreExpr],
        runtime: &JsRuntime,
    ) -> Option<Native> {
        if method == LANE_METHOD {
            return self.lane(names, receiver, arguments);
        }
        if SPECIAL_REGISTERED_METHODS.contains(&method) || PLUGIN_METHODS.contains(&method) {
            return None;
        }
        let builtin = names.method(method)?;
        // A registered method that sequences or refuses its arguments shapes
        // JavaScript values the native tier does not hold, so it takes its
        // exact count only.
        if let BuiltinMethod::Registered(registration) = &builtin
            && arguments.len() != registration.arity.saturating_sub(1)
        {
            return None;
        }
        let mut values = Vec::with_capacity(arguments.len());
        for argument in arguments {
            values.push(self.pattern_argument(names, argument, runtime)?);
        }
        builtin.apply(&receiver, values).map(Native::Pattern)
    }

    /// An argument as the surface reifies it: a pattern as it is, a number
    /// as a pure number. A raw string would go through a string parser a
    /// score may set, so it is left to JavaScript.
    fn pattern_argument(
        &mut self,
        names: &Names<'_, '_>,
        argument: &ScoreExpr,
        runtime: &JsRuntime,
    ) -> Option<Pattern> {
        match self.evaluate(names, argument, runtime)? {
            Native::Pattern(pattern) => Some(pattern),
            Native::Number(value) => Some(rustel_core::pure(rustel_core::Value::F64(value))),
            Native::String => None,
        }
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
    ) -> Option<Native> {
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
