//! `register()` - the extension mechanism.
//!
//! One function implements aliasing, currying, patternification and `_steps`
//! propagation for ~138 built-ins plus every user `register()` call. Keeping
//! those rules in one abstraction prevents individual combinators from
//! implementing subtly different registration behavior.
//!
//! Two rules are semantically observable, not optimisations:
//!   * the pure-argument fast path merges source locations, so it changes
//!     `context`, not just speed;
//!   * the pattern argument is last in the free-function form but is the
//!     receiver in the method form.

use crate::ops::PatOps;
use crate::purity::PurePattern;
use crate::{Hap, Pattern, Value};
use rustel_fraction::Fraction;
use std::sync::Arc;

// The registered surface, split by family: each file owns its calls.
// Entry order inside default_registry follows the original table.
mod degrade;
mod envelope;
mod euclid;
mod io;
mod math;
mod stepwise;
mod structure;
mod time;
mod tonal;
mod xen;

/// Which layer declared a registration.
///
/// Installation order decides shadowing, and the later declaration wins:
/// controls SHADOW same-named pattern combinators (`density` is the
/// control, not `fast`'s alias), and the late combinator declarations
/// shadow the controls in turn (`ds` is the envelope shorthand, not
/// `delaysync`'s alias). Recording the site lets the host reproduce that
/// ordering instead of guessing it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeclaredIn {
    /// An extension registration, installed before the built-in layers.
    /// A built-in registration with an overlapping name replaces it.
    Extension(Origin),
    /// The pattern layer, installed after extensions and before controls.
    /// A control with the same name overrides it.
    PatternModule,
    /// The controls layer, below the control declarations - installed last,
    /// so it overrides a control of the same name.
    ControlsModule,
}

/// Whose extension a name is.
///
/// Core records an opaque, stable label; it deliberately does not enumerate
/// extension authors. The statically linked extension crate owns those names
/// and their registrations, so adding an extension does not require editing
/// the core registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Origin(&'static str);

impl Origin {
    /// Declare one credited extension origin.
    pub const fn new(label: &'static str) -> Self {
        Self(label)
    }

    /// The grouping key, lower case. The reference title-cases it for display
    /// so that one spelling here cannot split a group in two.
    pub const fn label(self) -> &'static str {
        self.0
    }
}

impl DeclaredIn {
    /// True for a registration declared by an extension.
    pub fn is_extension(self) -> bool {
        matches!(self, DeclaredIn::Extension(_))
    }

    /// Whose extension it is, for a name that is one.
    pub fn origin(self) -> Option<Origin> {
        match self {
            DeclaredIn::Extension(origin) => Some(origin),
            _ => None,
        }
    }
}

/// Which join the patternified general path uses. `register()` defaults to
/// `innerJoin`; `stepRegister` defaults to `stepJoin`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinKind {
    Inner,
    Outer,
    Mix,
    /// `stepRegister`'s default. `expand`/`extend` are declared with it, so
    /// a PATTERNED argument slices the cycle at hap boundaries and
    /// `stepcat`s the pieces instead of taking the inner hap's whole;
    /// routing them through `innerJoin` produced half the haps.
    Step,
}

/// A registered combinator's behaviour, independent of how it is called.
///
/// `args` excludes the pattern; `pat` is always the final operand.
///
/// The variant is the purity proof. There is no boolean flag, because a flag
/// can mark an impure body as pure (see the purity tests).
#[derive(Clone)]
pub enum CombinatorFn {
    /// Provably introduces no JavaScript. Built only via [`native_combinator!`],
    /// which generates both typed views from a single body expression, so the
    /// `PurePattern`-typed view cannot disagree with the `Pattern`-typed one.
    Native(NativeCombinator),
    /// A native body whose leading arguments remain patterns instead of being
    /// sampled into scalar values. Extensions use this for structural
    /// application; the generic host still installs it like any registration.
    NativePatterned(NativePatternedCombinator),
    /// May materialise anything, including JS. Always classified impure.
    Dynamic(AnyBody),
}

/// Two views of one body. The `pure` view's signature is the proof: a
/// `PurePattern` can only be built from a pattern already classified pure, so
/// a body that typechecks against it cannot return a JS-backed pattern.
/// The `PurePattern`-typed view. Its signature is the purity proof.
pub type PureBody = Arc<dyn Fn(&[Value], PurePattern) -> PurePattern + Send + Sync>;
/// The `Pattern`-typed view of the SAME body, for impure operands.
pub type AnyBody = Arc<dyn Fn(&[Value], Pattern) -> Pattern + Send + Sync>;
/// A statically pure body that consumes leading arguments as patterns.
pub type PurePatternedBody = Arc<dyn Fn(&[PurePattern], PurePattern) -> PurePattern + Send + Sync>;
/// An arbitrary-pattern view of the same patterned body.
pub type AnyPatternedBody = Arc<dyn Fn(&[Pattern], Pattern) -> Pattern + Send + Sync>;

#[derive(Clone)]
pub struct NativeCombinator {
    pub(crate) pure: PureBody,
    pub(crate) any: AnyBody,
}

#[derive(Clone)]
pub struct NativePatternedCombinator {
    pure: PurePatternedBody,
    any: AnyPatternedBody,
}

/// Builds a [`CombinatorFn::Native`] from ONE body expression.
///
/// The same tokens are instantiated against `PurePattern` and `Pattern`, which
/// both expose the purity-preserving combinator set. Writing a body that
/// touches JavaScript fails to compile against `PurePattern`; writing two
/// different bodies is impossible because the macro accepts only one.
#[macro_export]
macro_rules! native_combinator {
    (|$args:ident, $pat:ident| $body:expr) => {
        $crate::register::CombinatorFn::Native($crate::register::NativeCombinator::new(
            ::std::sync::Arc::new(
                |$args: &[$crate::Value], $pat: $crate::purity::PurePattern| $body,
            ),
            ::std::sync::Arc::new(|$args: &[$crate::Value], $pat: $crate::Pattern| $body),
        ))
    };
}

/// Builds a native combinator whose leading arguments stay as patterns.
///
/// One body is instantiated for both typed views, so the `PurePattern` form
/// proves that the pure route cannot introduce a JavaScript callback.
#[macro_export]
macro_rules! native_patterned_combinator {
    (|$args:ident, $pat:ident| $body:expr) => {
        $crate::register::CombinatorFn::NativePatterned(
            $crate::register::NativePatternedCombinator::new(
                ::std::sync::Arc::new(
                    |$args: &[$crate::purity::PurePattern], $pat: $crate::purity::PurePattern| {
                        $body
                    },
                ),
                ::std::sync::Arc::new(|$args: &[$crate::Pattern], $pat: $crate::Pattern| $body),
            ),
        )
    };
}

impl NativeCombinator {
    pub fn new(pure: PureBody, any: AnyBody) -> Self {
        Self { pure, any }
    }
}

impl NativePatternedCombinator {
    pub fn new(pure: PurePatternedBody, any: AnyPatternedBody) -> Self {
        Self { pure, any }
    }

    fn apply(&self, args: &[Pattern], pat: Pattern) -> Pattern {
        let pure_args = args
            .iter()
            .map(Pattern::as_pure_pattern)
            .collect::<Option<Vec<_>>>();
        match (pure_args, pat.as_pure_pattern()) {
            (Some(args), Some(pat)) => (self.pure)(&args, pat).pattern().clone(),
            _ => (self.any)(args, pat),
        }
    }
}

impl CombinatorFn {
    /// Apply to an arbitrary operand.
    ///
    /// A native body applied to an impure operand still yields an impure
    /// result, because `Pattern`'s own construction rules propagate the
    /// operand's impurity - no special case is needed.
    fn apply(&self, args: &[Value], pat: Pattern) -> Pattern {
        match self {
            // Use the pure view when the operand is pure and no argument contains
            // a JS function. Binds through the `Pattern` view use the opaque dynamic
            // constructor and mark their results impure. The `PurePattern` return
            // type preserves the proof needed for tight-lookahead scheduling.
            CombinatorFn::Native(n) => match pat.as_pure_pattern() {
                Some(pure) if !args.iter().any(Value::has_js_function) => {
                    (n.pure)(args, pure).pattern().clone()
                }
                _ => (n.any)(args, pat),
            },
            CombinatorFn::NativePatterned(n) => {
                let args = args.iter().cloned().map(crate::pure).collect::<Vec<_>>();
                n.apply(&args, pat)
            }
            CombinatorFn::Dynamic(f) => f(args, pat),
        }
    }
}

/// Registration record. Names are **owned**, not `&'static`, because user code
/// calls `register()` at runtime and in prebake scripts.
#[derive(Clone)]
pub struct Registration {
    pub names: Vec<Arc<str>>,
    /// The reference entry a musician reads about the combinator, owned by
    /// the registration so a combinator cannot exist without its
    /// documentation.
    pub reference: crate::reference::ReferenceEntry,
    /// The declaration layer, which decides precedence against a same-named
    /// control. See [`DeclaredIn`].
    pub declared_in: DeclaredIn,
    /// True when a leading argument is a pattern TRANSFORMER (`every(4, rev)`).
    ///
    /// The general path cannot see argument values at construction time, so it
    /// cannot tell a native transformer from a JavaScript one. Declaring the
    /// possibility makes it take the opaque dynamic constructor, which is
    /// conservative: a false-impure costs latency, a false-pure costs
    /// correctness.
    pub takes_function: bool,
    /// The arity, INCLUDING the trailing pattern.
    pub arity: usize,
    pub patternify: bool,
    pub preserve_steps: bool,
    pub join: JoinKind,
    pub func: CombinatorFn,
}

impl Registration {
    /// The patternified entry point.
    ///
    /// `args` are the leading arguments as *patterns* (already `reify`d).
    pub fn call(&self, args: &[Pattern], pat: Pattern) -> Pattern {
        let result = if let CombinatorFn::NativePatterned(body) = &self.func {
            body.apply(args, pat.clone())
        } else if self.arity == 1 || args.is_empty() {
            self.func.apply(&[], pat.clone())
        } else if !self.patternify {
            // Non-patternified: leading args must be pure values. A non-pure
            // one keeps its position as Null. Dropping it would shift every
            // later argument one place left, and the body could not tell "no
            // argument" from "an argument that is not a literal".
            let vals: Vec<Value> = args
                .iter()
                .map(|p| p.as_pure().unwrap_or(Value::Null))
                .collect();
            self.func.apply(&vals, pat.clone())
        } else if args.len() > self.arity.saturating_sub(1) && all_pure(args).is_none() {
            // Over-applied call: with more leading arguments than the arity
            // allows, the curry chain ends up applying a Pattern as if it
            // were a function, and the query fails with this exact error.
            // `echoWith(..., fast(2))` reaches it; absorbing the extra
            // argument instead would produce plausible-but-wrong haps.
            crate::query_error_pattern("hap_func.value is not a function")
        } else if let Some(pures) = all_pure(args) {
            // ---- fast path ---------------------------------------------
            // All leading args pure. Not merely an optimisation: it also
            // concatenates the pure args' source locations onto the result's
            // context.
            let locs: Vec<(usize, usize)> =
                args.iter().flat_map(|p| p.pure_loc().into_iter()).collect();
            let out = self.func.apply(&pures, pat.clone());
            if locs.is_empty() {
                out
            } else {
                out.with_added_context(locs)
            }
        } else {
            // ---- general path ------------------------------------------
            // Build up a partial-application chain: `left.fmap` produces a
            // pattern of "functions", each subsequent `appLeft` supplies the
            // next argument, and the join flattens the resulting
            // pattern-of-patterns. This is where the combinator body runs *at
            // query time* - the reason querying can never be real-time
            // itself.
            let func = self.func.clone();
            let pat_for_fn = pat.clone();
            let n_args = args.len();

            // Accumulate argument values positionally.
            let mut acc: Pattern = args[0].fmap_collect();
            for a in &args[1..] {
                acc = acc.app_left_collect(a.clone());
            }

            // Which constructor is used is decided by the combinator's TYPE,
            // not by a flag. A `Native` body applied to a pure operand is
            // provably pure, so purity can be inherited soundly; anything else
            // goes through the dynamic constructor and is impure.
            let unpack = move |v: &Value| -> Vec<Value> {
                let vals = match v {
                    Value::List(xs) => xs.clone(),
                    single => vec![single.clone()],
                };
                debug_assert_eq!(vals.len(), n_args);
                vals
            };
            let inner = match (&func, pat_for_fn.as_pure_pattern()) {
                // A body that consumes a transformer cannot take the pure
                // route: the argument values are only known at query time, so
                // there is no way to know the transformer is native.
                _ if self.takes_function => {
                    acc.fmap_to_pattern(move |v| func.apply(&unpack(v), pat_for_fn.clone()))
                }
                // Native body + pure operand: the closure's return type proves
                // no JavaScript can appear, so purity is inherited SOUNDLY.
                (CombinatorFn::Native(n), Some(pure_operand)) => {
                    let pf = n.pure.clone();
                    acc.fmap_to_pure_pattern(move |v| pf(&unpack(v), pure_operand.clone()))
                }
                // Anything else is an opaque closure -> always impure.
                _ => acc.fmap_to_pattern(move |v| func.apply(&unpack(v), pat_for_fn.clone())),
            };
            match self.join {
                JoinKind::Inner => inner.inner_join(),
                JoinKind::Outer => inner.outer_join(),
                JoinKind::Mix => inner.mix_join(),
                JoinKind::Step => inner.step_join(),
            }
        };

        if self.preserve_steps {
            result.with_steps(pat.steps)
        } else {
            result
        }
    }

    /// The unpatternified `_name` form, calling `func` directly.
    pub fn call_unpatternified(&self, args: &[Value], pat: Pattern) -> Pattern {
        let result = self.func.apply(args, pat.clone());
        if self.preserve_steps {
            result.with_steps(pat.steps)
        } else {
            result
        }
    }
}

fn all_pure(args: &[Pattern]) -> Option<Vec<Value>> {
    let mut out = Vec::with_capacity(args.len());
    for a in args {
        out.push(a.as_pure()?);
    }
    Some(out)
}

/// The registry. Maps every name **and alias** to its registration.
#[derive(Default, Clone)]
pub struct Registry {
    entries: Vec<Registration>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, reg: Registration) {
        // Remove overlapping extension registrations before adding a built-in.
        // `get` returns the first match, and extensions are installed first.
        if !reg.declared_in.is_extension() {
            self.entries.retain(|entry| {
                !entry.declared_in.is_extension()
                    || !entry.names.iter().any(|n| reg.names.contains(n))
            });
        }
        self.entries.push(reg);
    }

    pub fn get(&self, name: &str) -> Option<&Registration> {
        self.entries
            .iter()
            .find(|r| r.names.iter().any(|n| &**n == name))
    }

    /// Every registered name, aliases included - the acceptance checklist.
    pub fn names(&self) -> Vec<&str> {
        self.entries
            .iter()
            .flat_map(|r| r.names.iter().map(|n| &**n))
            .collect()
    }

    /// Every registration's reference entry, in registration order: the
    /// documentation lives where the combinator does.
    pub fn reference_entries(&self) -> impl Iterator<Item = &crate::reference::ReferenceEntry> {
        self.entries.iter().map(|r| &r.reference)
    }

    /// The registrations' entries the reference shows, in registration order.
    ///
    /// Unsupported features keep their entries to explain Rustel's limits.
    /// [`COMBINATORS_REFERENCE_HIDDEN`] affects documentation only; every
    /// combinator remains registered.
    pub fn visible_reference_entries(
        &self,
    ) -> impl Iterator<Item = &crate::reference::ReferenceEntry> {
        self.entries
            .iter()
            .map(|r| &r.reference)
            .filter(|entry| !COMBINATORS_REFERENCE_HIDDEN.contains(&entry.name))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Convenience constructor for a default `register(names, func)` entry.
pub fn registration(
    names: &[&str],
    reference: crate::reference::ReferenceEntry,
    arity: usize,
    func: CombinatorFn,
) -> Registration {
    Registration {
        names: names.iter().map(|n| Arc::from(*n)).collect(),
        reference,
        declared_in: DeclaredIn::PatternModule,
        takes_function: false,
        arity,
        patternify: true,
        preserve_steps: false,
        join: JoinKind::Inner,
        func,
    }
}

// ---------------------------------------------------------------------------
// The first registered combinator: `fast` / `density`.
// ---------------------------------------------------------------------------

/// Upstream's entry for the first registered combinator, carried beside it:
/// Documentation text from the Strudel project (AGPL-3.0-or-later),
/// https://strudel.cc.
const FAST: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "fast",
    synonyms: &["density"],
    summary: "Speed up a pattern by the given factor.",
    description: "Speed up a pattern by the given factor. Used by \"*\" in mini notation.",
    params: &[crate::reference::ReferenceParam {
        name: "factor",
        r#type: "number | Pattern",
        description: "speed up factor",
    }],
    examples: &["s(\"bd hh sd hh\").fast(2) // s(\"[bd hh sd hh]*2\")"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

/// `fast`/`density` - the first registered combinator; step-preserving.
pub fn register_fast(reg: &mut Registry) {
    reg.register(Registration {
        names: vec![Arc::from("fast"), Arc::from("density")],
        reference: FAST,
        declared_in: DeclaredIn::PatternModule,
        takes_function: false,
        arity: 2,
        patternify: true,
        preserve_steps: true,
        join: JoinKind::Inner,
        func: crate::native_combinator!(|args, pat| {
            // A MISSING argument and an UNCONVERTIBLE one are different
            // failures and must not share a default. `fast()` with no factor is
            // the identity; `fast(1e300)` is a factor that does not exist in
            // this crate's `i128` rationals, and answering "the pattern,
            // unchanged" would be a silent wrong answer to a question that has
            // no answer. Silence is the observable "nothing", and it is what
            // `fast(0)` already means.
            match args.first() {
                None => pat,
                Some(value) => match value_to_fraction(value) {
                    Some(factor) => pat.fast(factor),
                    None => PatOps::pat_silence(),
                },
            }
        }),
    });
}

/// Values reaching a numeric combinator may be numbers or numeric strings
/// (mini-notation atoms are strings).
pub fn value_to_fraction(v: &Value) -> Option<Fraction> {
    match v {
        // `Fraction(number)` is a Farey search, NOT a decimal expansion - see
        // `Fraction::from_f64`. Reading `1/3` as `3333333333333333/1e16`
        // changes event boundaries.
        Value::F64(f) => Fraction::from_f64(*f),
        // `Fraction(string)` accepts rational forms such as "3/4" as well as
        // decimals. Using Fraction's parser also keeps Display round-trippable
        // instead of maintaining a second, narrower grammar here.
        Value::Str(s) => s.parse().ok(),
        Value::Bool(value) => Some(Fraction::int(i128::from(*value))),
        Value::Null => Some(Fraction::ZERO),
        Value::Undefined
        | Value::List(_)
        | Value::Object(_)
        | Value::Function(_)
        | Value::Pattern(_)
        | Value::Haps(_)
        | Value::JsValue(_) => None,
    }
}

// ---------------------------------------------------------------------------
// The registered surface.
//
// Every entry below is a `register(names, body, patternify, preserveSteps)`
// record whose body delegates to `combinators.rs`, so one implementation
// serves both purity views.
// ---------------------------------------------------------------------------

/// `register(names, func, true, preserve_steps)` - the default form.
pub(super) fn add(
    r: &mut Registry,
    names: &[&str],
    reference: crate::reference::ReferenceEntry,
    arity: usize,
    preserve_steps: bool,
    func: CombinatorFn,
) {
    add_in(
        r,
        DeclaredIn::PatternModule,
        names,
        reference,
        arity,
        preserve_steps,
        func,
    );
}

/// `register(...)` with an explicit declaration site.
pub fn add_in(
    r: &mut Registry,
    declared_in: DeclaredIn,
    names: &[&str],
    reference: crate::reference::ReferenceEntry,
    arity: usize,
    preserve_steps: bool,
    func: CombinatorFn,
) {
    add_full(
        r,
        declared_in,
        names,
        reference,
        arity,
        preserve_steps,
        false,
        func,
    );
}

/// `register(...)` for a combinator that consumes a pattern TRANSFORMER.
pub(super) fn add_fn(
    r: &mut Registry,
    names: &[&str],
    reference: crate::reference::ReferenceEntry,
    arity: usize,
    preserve_steps: bool,
    func: CombinatorFn,
) {
    add_fn_in(
        r,
        DeclaredIn::PatternModule,
        names,
        reference,
        arity,
        preserve_steps,
        func,
    );
}

/// `add_fn` with an explicit declaration site, for extensions.
pub fn add_fn_in(
    r: &mut Registry,
    declared_in: DeclaredIn,
    names: &[&str],
    reference: crate::reference::ReferenceEntry,
    arity: usize,
    preserve_steps: bool,
    func: CombinatorFn,
) {
    add_full(
        r,
        declared_in,
        names,
        reference,
        arity,
        preserve_steps,
        true,
        func,
    );
}

#[allow(clippy::too_many_arguments)]
fn add_full(
    r: &mut Registry,
    declared_in: DeclaredIn,
    names: &[&str],
    reference: crate::reference::ReferenceEntry,
    arity: usize,
    preserve_steps: bool,
    takes_function: bool,
    func: CombinatorFn,
) {
    r.register(Registration {
        names: names.iter().map(|n| Arc::from(*n)).collect(),
        reference,
        declared_in,
        takes_function,
        arity,
        patternify: true,
        preserve_steps,
        join: JoinKind::Inner,
        func,
    });
}

/// `stepRegister(names, func, …)` - same as `add`, but the patternified
/// general path joins with `stepJoin` rather than `innerJoin`.
pub(super) fn add_step(
    r: &mut Registry,
    names: &[&str],
    reference: crate::reference::ReferenceEntry,
    arity: usize,
    preserve_steps: bool,
    func: CombinatorFn,
) {
    r.register(Registration {
        names: names.iter().map(|n| Arc::from(*n)).collect(),
        reference,
        declared_in: DeclaredIn::PatternModule,
        takes_function: false,
        arity,
        patternify: true,
        preserve_steps,
        join: JoinKind::Step,
        func,
    });
}

/// `register(names, func, false, preserve_steps)` - leading arguments are NOT
/// patternified, so they arrive as plain values.
pub(super) fn add_unpatternified(
    r: &mut Registry,
    names: &[&str],
    reference: crate::reference::ReferenceEntry,
    arity: usize,
    preserve_steps: bool,
    func: CombinatorFn,
) {
    r.register(Registration {
        names: names.iter().map(|n| Arc::from(*n)).collect(),
        reference,
        declared_in: DeclaredIn::PatternModule,
        takes_function: false,
        arity,
        patternify: false,
        preserve_steps,
        join: JoinKind::Inner,
        func,
    });
}

/// Positional argument as a `Fraction`. A missing argument means the caller
/// under-applied - NaN-arithmetic territory; zero is the closest total
/// answer.
pub(super) fn arg_fraction(args: &[Value], index: usize) -> Fraction {
    args.get(index)
        .and_then(value_to_fraction)
        .unwrap_or(Fraction::ZERO)
}

/// Canonical shrink/grow cannot use `arg_fraction`'s total zero fallback:
/// pinned amount zero deliberately repeats the whole receiver, so mapping
/// Infinity, NaN, a pair/list, or an out-of-range number to zero turns a small
/// conversion failure into the most expensive valid construction.
pub(super) fn stepwise_amount(args: &[Value], index: usize) -> Result<Fraction, &'static str> {
    let Some(value) = args.get(index) else {
        return Ok(Fraction::ZERO);
    };
    match value {
        Value::Undefined | Value::Null => Ok(Fraction::ZERO),
        Value::F64(value) if value.is_infinite() => {
            Err("The number Infinity cannot be converted to a BigInt because it is not an integer")
        }
        Value::F64(value) if value.is_nan() => Err("Invalid argument"),
        _ => value_to_fraction(value).ok_or("Invalid argument"),
    }
}

pub(super) fn stepwise_input_refusal<P: PatOps>(operation: &'static str) -> P {
    P::pat_query_limit(crate::mark_stepwise_refusal(
        crate::QueryLimit::NativeFraction { operation },
    ))
}

/// Positional argument as a LITERAL string, with no mini-notation parsing.
///
/// Numbers stringify, so a port index reaches the same place a port name does.
pub(super) fn arg_text(args: &[Value], index: usize) -> String {
    match args.get(index) {
        Some(Value::Str(text)) => text.clone(),
        Some(Value::F64(number)) => {
            if number.fract() == 0.0 && number.is_finite() {
                format!("{}", *number as i64)
            } else {
                format!("{number}")
            }
        }
        // A bare `.midi()` means the first port.
        None => "0".to_string(),
        // An argument that did not arrive as a literal. A port name with a
        // space is parsed as mini-notation on the way in, so
        // "IAC Driver Bus 1" arrives as four events, not a name. A fallback
        // to "0" would send the set to the first device without a warning.
        // The sentinel matches no device, so the MIDI path reports it and
        // the audio continues.
        Some(_) => crate::UNREADABLE_MIDI_PORT.to_string(),
    }
}

/// Positional argument as a pattern transformer, if it is one.
pub(super) fn arg_function(args: &[Value], index: usize) -> Option<&crate::value::FunctionRef> {
    args.get(index).and_then(Value::as_function)
}

/// Positional argument as an f64 via `parseNumeral`.
pub(super) fn arg_number(args: &[Value], index: usize) -> f64 {
    args.get(index)
        .and_then(|v| crate::util::parse_numeral(v).ok())
        .unwrap_or(f64::NAN)
}

/// The combinator entries the studio's reference does not show.
///
/// Only calls that do nothing in both Rustel and Strudel belong here.
/// Unsupported Rustel features remain visible with their limitations.
pub const COMBINATORS_REFERENCE_HIDDEN: &[&str] = &[];

/// A registry containing the built-in combinators, without extensions.
pub fn default_registry() -> Registry {
    let mut r = Registry::new();
    register_fast(&mut r);

    time::register(&mut r);
    tonal::register(&mut r);
    xen::register(&mut r);
    structure::register(&mut r);
    math::register(&mut r);
    stepwise::register(&mut r);
    euclid::register(&mut r);
    degrade::register(&mut r);
    io::register(&mut r);
    envelope::register(&mut r);

    r
}

/// Install the built-in combinators after any extensions in a host registry.
/// Registering built-ins last removes extensions with overlapping names.
pub fn install_default_registry(registry: &mut Registry) {
    for registration in default_registry().entries {
        registry.register(registration);
    }
}

// Re-exported for tests that need to build haps directly.
pub use crate::TimeSpan as _TimeSpanReexport;
#[allow(unused)]
fn _unused(_: &Hap) {}

#[cfg(test)]
mod numeric_string_tests {
    use super::{default_registry, value_to_fraction};
    use crate::combinators::{MAX_ECHO_COPIES, MAX_ITER_PARTS};
    use crate::value::FunctionRef;
    use crate::{QueryLimit, Value, pure};
    use rustel_fraction::{Fraction, MAX_FRACTION_SOURCE_BYTES};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn decimal_string_overflow_is_refused_not_panicked_or_wrapped() {
        assert_eq!(
            value_to_fraction(&Value::Str("-0.5".into())),
            Some(Fraction::new(-1, 2))
        );
        assert_eq!(
            value_to_fraction(&Value::Str(
                "0.111111111111111111111111111111111111111".into()
            )),
            None
        );
        assert_eq!(
            value_to_fraction(&Value::Str("3/4".into())),
            Some(Fraction::new(3, 4))
        );
        assert_eq!(
            value_to_fraction(&Value::Str(".5".into())),
            Some(Fraction::new(1, 2))
        );
        assert_eq!(
            value_to_fraction(&Value::Str("0.(3)".into())),
            Some(Fraction::new(1, 3))
        );
        assert_eq!(value_to_fraction(&Value::Str(" 3/4".into())), None);
        assert_eq!(
            value_to_fraction(&Value::Str("9".repeat(MAX_FRACTION_SOURCE_BYTES + 1))),
            None
        );
    }

    #[test]
    fn construction_fanout_is_refused_before_transformer_callbacks() {
        let registry = default_registry();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let transformer = FunctionRef::registered(
            "countedConstructionTransform",
            Arc::new(move |pattern| {
                counted.fetch_add(1, Ordering::SeqCst);
                pattern.rev()
            }),
            true,
        );
        let callback = pure(Value::Function(transformer));
        let count = pure(Value::F64((MAX_ITER_PARTS + 1) as f64));
        let receiver = pure(Value::Str("a".into()));
        let cases = [
            ("iter", "iter", vec![count.clone()]),
            ("chunk", "chunk", vec![count.clone(), callback.clone()]),
            ("applyN", "applyN", vec![count.clone(), callback.clone()]),
            ("firstOf", "firstOf", vec![count.clone(), callback.clone()]),
            ("lastOf", "lastOf", vec![count.clone(), callback.clone()]),
        ];

        for (name, operation, args) in cases {
            let pattern = registry
                .get(name)
                .unwrap_or_else(|| panic!("missing {name}"))
                .call(&args, receiver.clone());
            assert!(
                matches!(
                    pattern.try_query_arc_sorted(Fraction::ZERO, Fraction::ONE),
                    Err(QueryLimit::IterParts {
                        operation: actual_operation,
                        parts,
                        limit,
                    }) if actual_operation == operation
                        && parts == MAX_ITER_PARTS + 1
                        && limit == MAX_ITER_PARTS
                ),
                "{name} did not surface its typed construction limit"
            );
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "oversized construction must be refused before user callbacks run"
        );
    }

    #[test]
    fn construction_fanout_accepts_the_exact_limit() {
        let registry = default_registry();
        let pattern = registry.get("iter").expect("registered iter").call(
            &[pure(Value::F64(MAX_ITER_PARTS as f64))],
            pure(Value::Str("a".into())),
        );
        assert!(
            pattern
                .try_query_arc_sorted(Fraction::ZERO, Fraction::ZERO)
                .is_ok(),
            "the exact documented fanout limit must be accepted"
        );
    }

    #[test]
    fn registered_euclid_family_surfaces_the_native_limit_at_query_time() {
        let registry = default_registry();
        let value = |number| pure(Value::F64(number));
        let receiver = || pure(Value::Str("a".into()));
        let cases = [
            ("euclid", vec![value(3.0), value(16_385.0)]),
            ("euclidRot", vec![value(3.0), value(16_385.0), value(0.0)]),
            (
                "bjork",
                vec![pure(Value::List(vec![
                    Value::F64(3.0),
                    Value::F64(16_385.0),
                ]))],
            ),
            ("euclidLegato", vec![value(3.0), value(16_385.0)]),
            (
                "euclidLegatoRot",
                vec![value(3.0), value(16_385.0), value(0.0)],
            ),
        ];

        for (name, args) in cases {
            let pattern = registry
                .get(name)
                .unwrap_or_else(|| panic!("missing {name}"))
                .call(&args, receiver());
            assert!(
                matches!(
                    pattern.try_query_arc_sorted(Fraction::ZERO, Fraction::ONE),
                    Err(QueryLimit::EuclidSteps {
                        steps: 16_385,
                        limit: 16_384
                    })
                ),
                "{name} silently laundered the native limit"
            );
        }
    }

    #[test]
    fn registered_echo_refuses_unbounded_copy_counts_before_any_callback() {
        let registry = default_registry();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let transformer = FunctionRef::registered(
            "countedIdentity",
            Arc::new(move |pattern| {
                counted.fetch_add(1, Ordering::SeqCst);
                pattern
            }),
            true,
        );
        let callback = pure(Value::Function(transformer));
        let receiver = pure(Value::Str("a".into()));
        let echo_with = registry.get("echoWith").expect("registered echoWith");

        let cases = [
            ((MAX_ECHO_COPIES + 1) as f64, MAX_ECHO_COPIES + 1),
            (f64::INFINITY, i64::MAX as u64),
        ];
        for (number, copies) in cases {
            let pattern = echo_with.call(
                &[
                    pure(Value::F64(number)),
                    pure(Value::F64(0.125)),
                    callback.clone(),
                ],
                receiver.clone(),
            );
            assert!(
                matches!(
                    pattern.try_query_arc_sorted(Fraction::ZERO, Fraction::ONE),
                    Err(QueryLimit::EchoCopies {
                        copies: actual,
                        limit: MAX_ECHO_COPIES,
                    }) if actual == copies
                ),
                "echoWith({number}, ...) silently laundered its construction limit"
            );
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "the finite construction guard must run before the indexed callback batch"
        );
    }

    #[test]
    fn registered_echo_accepts_the_exact_copy_limit() {
        let registry = default_registry();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let transformer = FunctionRef::registered(
            "countedIdentity",
            Arc::new(move |pattern| {
                counted.fetch_add(1, Ordering::SeqCst);
                pattern
            }),
            true,
        );
        let pattern = registry.get("echoWith").expect("registered echoWith").call(
            &[
                pure(Value::F64(MAX_ECHO_COPIES as f64)),
                pure(Value::F64(0.0)),
                pure(Value::Function(transformer)),
            ],
            pure(Value::Str("a".into())),
        );

        assert_eq!(
            calls.load(Ordering::SeqCst),
            MAX_ECHO_COPIES as usize,
            "the exact documented copy limit must be accepted"
        );
        assert!(
            pattern
                .try_query_arc_sorted(Fraction::ZERO, Fraction::ZERO)
                .is_ok(),
            "the accepted boundary must not carry a latent resource refusal"
        );
    }
}

/// The reference a musician reads must match the registration a score calls.
#[cfg(test)]
mod reference_honesty_tests {
    use super::{COMBINATORS_REFERENCE_HIDDEN, default_registry};
    use crate::{Value, pure};
    use rustel_fraction::Fraction;

    /// A patternified combinator's arity counts the pattern receiver as its
    /// last argument, so its entry documents exactly `arity - 1` arguments.
    /// The handful that do not are argued beside their name, and the guard
    /// fails both when a new deviation appears and when an argued one stops
    /// deviating.
    #[test]
    fn every_combinator_documents_the_arguments_it_reads() {
        let registry = default_registry();
        let mut deviating: Vec<&str> = registry
            .entries
            .iter()
            .filter(|reg| reg.reference.params.len() != reg.arity.saturating_sub(1))
            .map(|reg| reg.reference.name)
            .collect();
        deviating.sort_unstable();
        let mut argued: Vec<&str> = vec![
            // One `attack:decay:sustain:release` argument, documented as the
            // four envelope components it encodes.
            "adsr",
            // Documents the receiver's value alongside the control name.
            "control",
            // One argument documented as its parts.
            "euclidLegato",
            // `amt` plus the `edoSize` it falls back to.
            "ftranspose",
            // The message plus the device it is sent to.
            "midi",
            // The message plus the device it is sent to.
            "sysex",
        ];
        argued.sort_unstable();
        assert_eq!(
            deviating, argued,
            "a combinator's documented params must match the arguments it reads"
        );
    }

    /// A combinator called with no arguments curries or substitutes its
    /// documented default; it never panics. Swept over the whole registry so
    /// a new combinator cannot regress this.
    #[test]
    fn a_bare_call_of_every_combinator_yields_a_pattern_and_never_panics() {
        let registry = default_registry();
        let mut panicked: Vec<&str> = Vec::new();
        for reg in &registry.entries {
            let name = reg.reference.name;
            let receiver = pure(Value::Str("a".into()));
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let pattern = reg.call(&[], receiver);
                let _ = pattern.try_query_arc_sorted(Fraction::ZERO, Fraction::ONE);
            }));
            if outcome.is_err() {
                panicked.push(name);
            }
        }
        assert!(
            panicked.is_empty(),
            "bare calls panicked: {}",
            panicked.join(", ")
        );
    }

    #[test]
    fn the_combinator_hidden_list_and_the_entries_agree() {
        let registry = default_registry();
        let documented: std::collections::BTreeSet<&str> = registry
            .reference_entries()
            .map(|entry| entry.name)
            .collect();
        for hidden in COMBINATORS_REFERENCE_HIDDEN {
            assert!(
                documented.contains(*hidden),
                "`{hidden}` is hidden from the reference but no combinator registers it"
            );
        }
    }

    #[test]
    fn unsupported_keyboard_calls_remain_documented() {
        let registry = default_registry();
        for name in ["whenKey", "keyDown"] {
            let entry = registry
                .visible_reference_entries()
                .find(|entry| entry.name == name)
                .unwrap_or_else(|| panic!("`{name}` needs a reference entry"));
            assert!(
                entry.summary.contains("unsupported in Rustel"),
                "`{name}` must explain that keyboard input is unsupported"
            );
        }
    }
}
