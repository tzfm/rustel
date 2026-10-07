use super::*;

// -- higher-ranked closure coercions ----------------------------------------
//
// `Function::new` needs a closure that is generic over the JS lifetime, but a
// Rust closure written inline infers ONE lifetime and then fails the invariance
// check on `Class<'js, _>`. Passing it through a function whose bound is
// `for<'js>` forces the higher-ranked signature at the definition site. One
// helper per argument shape; each is a no-op at runtime.

pub(super) type NativeResult<'js> =
    rquickjs::Result<rquickjs::Class<'js, NativePatternWrapper<'js>>>;

pub(super) fn hr_rest<F>(f: F) -> F
where
    F: for<'js> Fn(Ctx<'js>, rquickjs::function::Rest<rquickjs::Value<'js>>) -> NativeResult<'js>,
{
    f
}

pub(super) fn hr_value<F>(f: F) -> F
where
    F: for<'js> Fn(Ctx<'js>, rquickjs::Value<'js>) -> NativeResult<'js>,
{
    f
}

pub(super) fn hr_this_rest<F>(f: F) -> F
where
    F: for<'js> Fn(
        Ctx<'js>,
        rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
        rquickjs::function::Rest<rquickjs::Value<'js>>,
    ) -> NativeResult<'js>,
{
    f
}

/// One optional argument. Paired with [`set_function_length`] because rquickjs
/// cannot give both the arity and the tolerance this surface needs.
pub(super) fn hr_this_arg<F>(f: F) -> F
where
    F: for<'js> Fn(
        Ctx<'js>,
        rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
        rquickjs::function::Opt<rquickjs::Value<'js>>,
    ) -> NativeResult<'js>,
{
    f
}

/// Set `f.length`, which rquickjs derives from the Rust signature.
///
/// `polyBind`/`stepBind` are `length === 1` - ordinary class methods taking
/// `func` - and calling them with NO argument does not throw:
/// `fmap(undefined)` constructs and fails at query time. rquickjs offers those
/// two properties on different parameter types and no type with both. A
/// required `Value` reports 1 but rejects the zero-argument call outright, so
/// `pure('bd').polyBind()` would throw where Strudel returns no haps;
/// `Opt<Value>` accepts the call but reports 0.
///
/// Use `Opt<Value>` to accept missing arguments, then restore the arity
/// here. `curry` reads `length` to decide when to invoke the function.
pub(super) fn set_function_length(function: &Function<'_>, length: usize) -> rquickjs::Result<()> {
    function.set_length(length)
}

/// No arguments at all, so the installed function reports `length === 0`.
pub(super) fn hr_this_nullary<F>(f: F) -> F
where
    F: for<'js> Fn(
        Ctx<'js>,
        rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
    ) -> NativeResult<'js>,
{
    f
}

pub(super) fn hr_thisval_rest<F>(f: F) -> F
where
    F: for<'js> Fn(
        Ctx<'js>,
        rquickjs::function::This<rquickjs::Value<'js>>,
        rquickjs::function::Rest<rquickjs::Value<'js>>,
    ) -> NativeResult<'js>,
{
    f
}

pub(super) fn hr_this_rest_to_value<F>(f: F) -> F
where
    F: for<'js> Fn(
        Ctx<'js>,
        rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
        rquickjs::function::Rest<rquickjs::Value<'js>>,
    ) -> rquickjs::Result<rquickjs::Value<'js>>,
{
    f
}

/// `other = sequence(other)` - the composer wrappers sequence their rest args.
pub(super) fn sequence_args_bridged<'js>(
    ctx: &Ctx<'js>,
    args: &[rquickjs::Value<'js>],
) -> rquickjs::Result<(Pattern, Vec<Sidecar<'js>>)> {
    if args.is_empty() {
        return Ok((rustel_core::silence(), Vec::new()));
    }
    let mut patterns = Vec::with_capacity(args.len());
    let mut sidecars = Vec::new();
    for arg in args {
        let (pattern, nested) = reify_list_element_bridged(ctx, arg)?;
        patterns.push(pattern);
        sidecars.extend(nested);
    }
    let pattern = if patterns.len() == 1 {
        patterns.pop().expect("one sequence argument")
    } else {
        rustel_core::fastcat(patterns)
    };
    Ok((pattern, sidecars))
}

/// Refuse JavaScript callbacks at a registered combinator boundary unless the
/// registration explicitly declares that it consumes a pattern transformer.
///
/// One-input methods deliberately sequence several scalar arguments, so an
/// ordinary arity check cannot reject `.fast(2, 4)`. A function must not enter
/// that scalar sequence, though: reifying it as a pattern makes malformed calls
/// such as `.ply(2, callback)` look valid until query time (or even produce
/// plausible output).
pub(crate) fn reject_unexpected_registered_functions(
    ctx: &Ctx<'_>,
    name: &str,
    takes_function: bool,
    args: &[rquickjs::Value<'_>],
) -> rquickjs::Result<()> {
    if !takes_function && args.iter().any(rquickjs::Value::is_function) {
        return Err(throw_type_error(
            ctx,
            &format!("Invalid argument: .{name}() does not accept a function"),
        ));
    }
    Ok(())
}

pub(super) fn compose_call<'js>(
    ctx: Ctx<'js>,
    this: &rquickjs::Value<'js>,
    args: &[rquickjs::Value<'js>],
    op: rustel_core::compose::ComposeOp,
    how: rustel_core::compose::Alignment,
) -> rquickjs::Result<rquickjs::Class<'js, NativePatternWrapper<'js>>> {
    if args.is_empty() {
        // Composing with a sequence of nothing empties the pattern: a bare
        // `.add()` mid-chain takes the track out of the set silently.
        return Err(refuse_empty_call(&ctx, &format!(".{}", op.name()), 1));
    }
    let (receiver, receiver_sidecar) = reify_bridged(&ctx, this)?;
    let (other, nested) = sequence_args_bridged(&ctx, args)?;
    let pattern = rustel_core::compose::compose(&receiver, &other, op, how);
    // Both operands' cells, including cells owned by a configured parser's
    // returned wrapper: `s("bd").every(2, f).add(dynamicString)` must keep
    // every callback reachable through the resulting graph.
    let mut sidecars = vec![receiver_sidecar];
    sidecars.extend(nested);
    derive_wrapper(ctx, pattern, &sidecars)
}

/// Install the direct `_set` composer before the public `set` getter.
///
/// Unlike registered raw combinators, this method closes over the composer
/// operation and maps the strict receiver directly.  Keeping the factory in
/// JavaScript preserves the anonymous, length-one, constructible function and
/// the lexical arrow callback exposed to a replaced `fmap`.
pub(super) fn install_raw_set<'js>(
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
) -> Result<(), String> {
    let raw_set: rquickjs::Function = ctx
        .eval(
            r#"(function () {
                 "use strict";
                 const op = (a, b) => b;
                 return function (value) {
                   return this.fmap((x) => op(x, value));
                 };
               })()"#,
        )
        .map_err(|error| describe_js_error(ctx, error))?;
    proto
        .set("_set", raw_set)
        .map_err(|error| error.to_string())
}

/// Install the direct `_keep` composer before the public `keep` getter.
///
/// This intentionally remains a separate factory from `_set`: each composer
/// keeps its own lexical operation while exposing the same strict,
/// anonymous, length-one mapping function on the prototype.
pub(super) fn install_raw_keep<'js>(
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
) -> Result<(), String> {
    let raw_keep: rquickjs::Function = ctx
        .eval(
            r#"(function () {
                 "use strict";
                 const op = (a) => a;
                 return function (value) {
                   return this.fmap((x) => op(x, value));
                 };
               })()"#,
        )
        .map_err(|error| describe_js_error(ctx, error))?;
    proto
        .set("_keep", raw_keep)
        .map_err(|error| error.to_string())
}

/// Install the direct `_keepif` composer before the public `keepif`
/// getter.
///
/// Its lexical operation is deliberately kept separate from `_set` and
/// `_keep`: JavaScript ToBoolean selects the exact mapper input or one
/// `undefined` value without turning that hap into silence.
pub(super) fn install_raw_keepif<'js>(
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
) -> Result<(), String> {
    let raw_keepif: rquickjs::Function = ctx
        .eval(
            r#"(function () {
                 "use strict";
                 const op = (a, b) => (b ? a : undefined);
                 return function (value) {
                   return this.fmap((x) => op(x, value));
                 };
               })()"#,
        )
        .map_err(|error| describe_js_error(ctx, error))?;
    proto
        .set("_keepif", raw_keepif)
        .map_err(|error| error.to_string())
}

/// Install the direct strict-equality `_eqt` composer before the
/// public `eqt` getter.
///
/// The comparison must execute inside the lexical JavaScript mapper.  Routing
/// it through the native public composer would structurally compare or
/// materialise values that must compare by exact JavaScript identity.
pub(super) fn install_raw_eqt<'js>(
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
) -> Result<(), String> {
    let raw_eqt: rquickjs::Function = ctx
        .eval(
            r#"(function () {
                 "use strict";
                 const op = (a, b) => a === b;
                 return function (value) {
                   return this.fmap((x) => op(x, value));
                 };
               })()"#,
        )
        .map_err(|error| describe_js_error(ctx, error))?;
    proto
        .set("_eqt", raw_eqt)
        .map_err(|error| error.to_string())
}

/// Install the direct strict-inequality `_net` composer before the
/// public `net` getter.
///
/// Keep this as a separate JavaScript factory: the lexical `!==` comparison
/// must observe exact JavaScript identity without native materialisation or a
/// shared callback changing the raw function's closure and reflection shape.
pub(super) fn install_raw_net<'js>(
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
) -> Result<(), String> {
    let raw_net: rquickjs::Function = ctx
        .eval(
            r#"(function () {
                 "use strict";
                 const op = (a, b) => a !== b;
                 return function (value) {
                   return this.fmap((x) => op(x, value));
                 };
               })()"#,
        )
        .map_err(|error| describe_js_error(ctx, error))?;
    proto
        .set("_net", raw_net)
        .map_err(|error| error.to_string())
}

/// Install the direct logical-AND `_and` composer before the public
/// `and` getter.
///
/// Keep this operation in its own JavaScript factory so ECMAScript truthiness
/// and exact operand selection happen inside the fresh lexical mapper before
/// bridge materialisation, without routing through native public coercion.
pub(super) fn install_raw_and<'js>(
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
) -> Result<(), String> {
    let raw_and: rquickjs::Function = ctx
        .eval(
            r#"(function () {
                 "use strict";
                 const op = (a, b) => a && b;
                 return function (value) {
                   return this.fmap((x) => op(x, value));
                 };
               })()"#,
        )
        .map_err(|error| describe_js_error(ctx, error))?;
    proto
        .set("_and", raw_and)
        .map_err(|error| error.to_string())
}

/// Install the direct logical-OR `_or` composer before the public
/// `or` getter.
///
/// Keep operand selection inside its own JavaScript mapper so truthiness is
/// applied before bridge materialisation, without routing selection through
/// the native public composer.
pub(super) fn install_raw_or<'js>(
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
) -> Result<(), String> {
    let raw_or: rquickjs::Function = ctx
        .eval(
            r#"(function () {
                 "use strict";
                 const op = (a, b) => a || b;
                 return function (value) {
                   return this.fmap((x) => op(x, value));
                 };
               })()"#,
        )
        .map_err(|error| describe_js_error(ctx, error))?;
    proto.set("_or", raw_or).map_err(|error| error.to_string())
}

/// Install one composer as a prototype **getter** returning a callable object.
///
/// ```js
/// Object.defineProperty(Pattern.prototype, what, { get: function () {
///   const wrapper = (...other) => pat[what][DEFAULT_ALIGNMENT](...other);
///   for (const how of ALIGNMENTS) wrapper[how.toLowerCase()] = …;
///   return wrapper;
/// }});
/// ```
/// The getter is not a detail: `pat.add` must be callable *and* carry the
/// alignment methods, and it must close over `pat`. QuickJS cannot express
/// "callable object with properties" from Rust directly, so the callable is
/// built in JS from a Rust-provided table - the bodies are still native.
pub(super) fn install_composer<'js>(
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
    op: rustel_core::compose::ComposeOp,
    alignments: rquickjs::Object<'js>,
    default_apply: rquickjs::Function<'js>,
) -> Result<(), String> {
    let make: rquickjs::Function = ctx
        .eval(
            r#"(function (alignments, defaultApply) {
                 return function () {
                   const pat = this;
                   const wrapper = (...other) => defaultApply.apply(pat, other);
                   for (const how of Object.keys(alignments)) {
                     wrapper[how] = (...other) => alignments[how].apply(pat, other);
                   }
                   return wrapper;
                 };
               })"#,
        )
        .map_err(|e| e.to_string())?;
    let getter: rquickjs::Function = make
        .call((alignments, default_apply))
        .map_err(|e: rquickjs::Error| e.to_string())?;
    let define: rquickjs::Function = ctx
        .eval(
            r#"(function (target, name, getter) {
                 Object.defineProperty(target, name, { get: getter, configurable: true });
               })"#,
        )
        .map_err(|e| e.to_string())?;
    define
        .call::<_, ()>((proto.clone(), op.name(), getter))
        .map_err(|e: rquickjs::Error| e.to_string())?;
    Ok(())
}

/// Install one registered combinator as both a prototype method and a free
/// function, exactly as `register()` does.
pub(super) fn install_combinator<'js>(
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
    globals: &rquickjs::Object<'js>,
    name: &str,
    registration: rustel_core::register::Registration,
) -> Result<(), String> {
    let arity = registration.arity;
    let indexed_transformer = matches!(name, "echoWith" | "echowith" | "stutWith" | "stutwith");
    let method_name: std::sync::Arc<str> = std::sync::Arc::from(name);
    let method_registration = registration.clone();
    let method_indexed_transformer = indexed_transformer;
    let method = Function::new(
        ctx.clone(),
        hr_this_rest(move |ctx, this, args| {
            let (receiver, mut sidecars) = {
                let borrowed = this.0.borrow();
                (borrowed.pattern.clone(), vec![Sidecar::of(&borrowed)])
            };
            // Registered-method arity rule: an arity-2 method sequences
            // several arguments, so `.fast(1, 2)` is
            // `.fast(sequence(1, 2))`. Any other arity mismatch throws.
            // Dropping the extra arguments would make
            // `.scale('C:major', 'C:minor')` play only the first and would
            // hide typos.
            let expected = method_registration.arity.saturating_sub(1);
            let midi = method_name.as_ref() == "midi";
            let serial = method_name.as_ref() == "serial";
            // A bare `.osc()` means the default port, as its entry promises
            // ("with no argument the port is 57120"): nothing said is an
            // answer here, not a sequence of nothing.
            let bare_osc = method_name.as_ref() == "osc" && args.0.is_empty();
            if midi && args.0.len() > 2 {
                return Err(throw_type_error(
                    &ctx,
                    ".midi() accepts at most a port and an options object",
                ));
            }
            if serial && args.0.len() > 4 {
                return Err(throw_type_error(
                    &ctx,
                    ".serial() accepts at most baud, sendcrc, singlecharids, and port.",
                ));
            }
            if serial
                && let Some(port) = args.0.get(3)
                && !(port.is_string() || port.is_undefined())
            {
                return Err(throw_type_error(
                    &ctx,
                    ".serial() needs its port in SINGLE quotes - .serial(38400, 0, 0, '/dev/ttyUSB0'). Double quotes are parsed as mini-notation before .serial() sees it, so the name never arrives.",
                ));
            }
            if midi
                && let Some(port) = args.0.first()
                && !(port.is_string() || port.is_number() || port.is_undefined())
            {
                // The message names the fix. The player wrote a literal
                // string, but `"Pilote IAC Bus 1"` arrives parsed as
                // mini-notation into four words, and the name cannot be
                // recovered from that. So this refuses instead of guessing.
                // `midin` and `midikeys` refuse the same way.
                return Err(throw_type_error(
                    &ctx,
                    ".midi() needs a port name in SINGLE quotes - .midi('IAC Bus 1') - or an index, .midi(0). Double quotes are parsed as mini-notation before .midi() sees them, so the name never arrives. `rustel devices` lists what is plugged in.",
                ));
            }
            if midi
                && let Some(options) = args.0.get(1)
                && !options.is_undefined()
                && (options.as_object().is_none()
                    || options.is_array()
                    || options.is_function()
                    || unwrap_pattern(options).is_some())
            {
                return Err(throw_type_error(
                    &ctx,
                    ".midi() options must be a plain object",
                ));
            }
            reject_unexpected_registered_functions(
                &ctx,
                method_name.as_ref(),
                method_registration.takes_function,
                &args.0,
            )?;
            let (arg_pats, arg_sidecars) = if midi || serial || bare_osc {
                // A bare `.midi()` - or `.osc()` - deliberately stays
                // argument-free so the native body selects its default
                // port. Sequencing zero arguments would create silence,
                // then silently filter it out.
                reify_registered_args(&ctx, &args.0, method_indexed_transformer)?
            } else if expected == 1 && args.0.len() != 1 {
                // Several arguments to a one-input method are a sequence:
                // `.fast(2, 4)` is `.fast("2 4")`. No arguments is not: a
                // sequence of nothing is silence, and silence through the
                // body silences the whole chain with no error in the score.
                // So an empty call is refused like any other wrong count.
                if args.0.is_empty() {
                    return Err(throw_type_error(
                        &ctx,
                        &format!(".{method_name}() expects {expected} inputs but got 0."),
                    ));
                }
                let (pattern, nested) = sequence_args_bridged(&ctx, &args.0)?;
                (vec![pattern], nested)
            } else {
                let (patterns, nested) =
                    reify_registered_args(&ctx, &args.0, method_indexed_transformer)?;
                if patterns.len() != expected {
                    return Err(throw_type_error(
                        &ctx,
                        &format!(
                            ".{method_name}() expects {expected} inputs but got {}.",
                            patterns.len()
                        ),
                    ));
                }
                (patterns, nested)
            };
            sidecars.extend(arg_sidecars);
            // The body may apply a transformer EAGERLY and hand it this
            // receiver; the callback can keep what it is given.
            publish_owner_cells(&sidecars)?;
            let pattern = method_registration.call(&arg_pats, receiver);
            rethrow_eager_callback_exception(&ctx)?;
            derive_wrapper(ctx, pattern, &sidecars)
        }),
    )
    .map_err(|e| e.to_string())?;
    proto.set(name, method).map_err(|e| e.to_string())?;

    // MIDI output is a custom prototype method, not a registered
    // free export. Exposing a generated `midi(...)` bypassed the raw-port
    // refusal above and could silently route malformed calls to device zero.
    if matches!(name, "midi" | "serial") {
        globals.remove(name).map_err(|e| e.to_string())?;
        return Ok(());
    }

    // Free-function form: the PATTERN IS THE LAST ARGUMENT.
    let free = Function::new(
        ctx.clone(),
        hr_rest(move |ctx, args| {
            let (mut pats, sidecars) = reify_registered_args(&ctx, &args.0, indexed_transformer)?;
            let receiver = pats.pop().unwrap_or_else(rustel_core::silence);
            publish_owner_cells(&sidecars)?;
            let pattern = registration.call(&pats, receiver);
            rethrow_eager_callback_exception(&ctx)?;
            derive_wrapper(ctx, pattern, &sidecars)
        }),
    )
    .map_err(|e| e.to_string())?;
    // The free form is CURRIED - `rustelScope[name] =
    // curry(pfunc, null, arity)` - so `fast(2)` is a transformer awaiting a
    // pattern, not a call with the factor mistaken for one. The curry wrapper
    // is built in JavaScript because it must produce a real callable that also
    // carries properties; the body it calls is still the native one.
    let raw_name = format!("__rustel_raw_{name}");
    globals
        .set(raw_name.as_str(), free)
        .map_err(|e| e.to_string())?;
    let curry: rquickjs::Function = ctx
        .eval(
            r#"(function (raw, arity, name) {
                 const make = (collected) => {
                   const f = (...more) => {
                     const args = collected.concat(more);
                     return args.length >= arity ? raw(...args) : make(args);
                   };
                   f.__rustel_combinator = { name, args: collected };
                   return f;
                 };
                 return make([]);
               })"#,
        )
        .map_err(|e| e.to_string())?;
    let raw: rquickjs::Value = globals.get(raw_name.as_str()).map_err(|e| e.to_string())?;
    let curried: rquickjs::Value = curry
        .call((raw, arity, name))
        .map_err(|e: rquickjs::Error| e.to_string())?;
    globals.set(name, curried).map_err(|e| e.to_string())?;
    globals
        .remove(raw_name.as_str())
        .map_err(|e| e.to_string())?;
    Ok(())
}

mod array_guard;
pub(crate) mod effects;
mod patterns;
mod setup;

pub(crate) use array_guard::*;
pub(crate) use effects::*;
pub(crate) use patterns::*;
pub(crate) use setup::*;
