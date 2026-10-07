use super::*;
use rquickjs::function::Args;

pub(super) fn define_method<'js>(
    proto: &rquickjs::Object<'js>,
    name: &str,
    function: Function<'js>,
    function_name: &str,
    length: usize,
) -> Result<(), String> {
    function
        .set_name(function_name)
        .map_err(|error| error.to_string())?;
    function
        .set_length(length)
        .map_err(|error| error.to_string())?;
    function.set_constructor(true);
    let constructor_prototype =
        rquickjs::Object::new(function.ctx().clone()).map_err(|error| error.to_string())?;
    constructor_prototype
        .prop(
            "constructor",
            rquickjs::object::Property::from(function.clone())
                .writable()
                .configurable(),
        )
        .map_err(|error| error.to_string())?;
    function
        .as_inner()
        .prop(
            "prototype",
            rquickjs::object::Property::from(constructor_prototype).writable(),
        )
        .map_err(|error| error.to_string())?;
    proto
        .prop(
            name,
            rquickjs::object::Property::from(function)
                .writable()
                .configurable(),
        )
        .map_err(|error| error.to_string())
}

/// How upstream exports a registration, which decides whether it is also a
/// free function. A single or destructured `register` export is a callable
/// global upstream; the name-list object from `register([...])` is not.
#[derive(Clone, Copy)]
pub(super) enum UpstreamExport {
    /// The export is the function: installed as a method and as a free
    /// curried function that takes the pattern last.
    Callable,
    /// The export is the name-list object: installed as a method only.
    NameListObject,
}

/// Install `registration` as the pattern method `name`, and also as a free
/// curried function when `export` is [`UpstreamExport::Callable`].
pub(super) fn install_registration<'js>(
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
    globals: &rquickjs::Object<'js>,
    name: &'static str,
    registration: rustel_core::register::Registration,
    export: UpstreamExport,
    indexed_transformer: bool,
) -> Result<(), String> {
    let method_registration = registration.clone();
    let method = Function::new(
        ctx.clone(),
        hr_this_rest(move |ctx, this, args| {
            let (receiver, mut sidecars) = {
                let wrapper = this.0.borrow();
                (wrapper.pattern.clone(), vec![Sidecar::of(&wrapper)])
            };
            reject_unexpected_registered_functions(
                &ctx,
                name,
                method_registration.takes_function,
                &args.0,
            )?;
            let expected = method_registration.arity.saturating_sub(1);
            let (arguments, nested) = if expected == 1 && args.0.len() != 1 {
                // Several arguments to a one-input method are a sequence:
                // `.fast(2, 4)` is `.fast("2 4")`. No arguments is not: a
                // sequence of nothing is silence, and silence through the
                // body silences the whole chain with no error in the score.
                // So an empty call is refused like any other wrong count.
                if args.0.is_empty() {
                    return Err(throw_type_error(
                        &ctx,
                        &format!(".{name}() expects {expected} inputs but got 0."),
                    ));
                }
                let (pattern, nested) = sequence_args_bridged(&ctx, &args.0)?;
                (vec![pattern], nested)
            } else {
                let (arguments, nested) =
                    reify_registered_args(&ctx, &args.0, indexed_transformer)?;
                if arguments.len() != expected {
                    return Err(throw_type_error(
                        &ctx,
                        &format!(
                            ".{name}() expects {expected} inputs but got {}.",
                            arguments.len()
                        ),
                    ));
                }
                (arguments, nested)
            };
            sidecars.extend(nested);
            publish_owner_cells(&sidecars)?;
            let pattern = method_registration.call(&arguments, receiver);
            rethrow_eager_callback_exception(&ctx)?;
            derive_wrapper(ctx, pattern, &sidecars)
        }),
    )
    .map_err(|error| error.to_string())?;
    proto.set(name, method).map_err(|error| error.to_string())?;

    match export {
        UpstreamExport::NameListObject => return Ok(()),
        UpstreamExport::Callable => {}
    }
    let arity = registration.arity;
    let raw = Function::new(
        ctx.clone(),
        hr_rest(move |ctx, args| {
            let (mut patterns, sidecars) =
                reify_registered_args(&ctx, &args.0, indexed_transformer)?;
            let receiver = patterns.pop().unwrap_or_else(rustel_core::silence);
            publish_owner_cells(&sidecars)?;
            let pattern = registration.call(&patterns, receiver);
            rethrow_eager_callback_exception(&ctx)?;
            derive_wrapper(ctx, pattern, &sidecars)
        }),
    )
    .map_err(|error| error.to_string())?;
    globals
        .set(
            name,
            native_curry(ctx, raw, arity, Vec::new()).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())
}

pub(super) type PatternBody = std::sync::Arc<dyn Fn(&[Pattern]) -> Pattern + Send + Sync + 'static>;

/// Install `body` as the pattern method `name`, and also as a free curried
/// function when `export` is [`UpstreamExport::Callable`]. `body` receives
/// every argument as a pattern, the receiver last.
pub(super) fn install_pattern_arguments<'js>(
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
    globals: &rquickjs::Object<'js>,
    name: &'static str,
    arity: usize,
    body: PatternBody,
    export: UpstreamExport,
) -> Result<(), String> {
    let method_body = body.clone();
    let method = Function::new(
        ctx.clone(),
        hr_this_rest(move |ctx, this, args| {
            let expected = arity.saturating_sub(1);
            let (mut arguments, mut sidecars) = if expected == 1 && args.0.len() != 1 {
                // Several arguments to a one-input method are a sequence:
                // `.fast(2, 4)` is `.fast("2 4")`. No arguments is not: a
                // sequence of nothing is silence, and silence through the
                // body silences the whole chain with no error in the score.
                // So an empty call is refused like any other wrong count.
                if args.0.is_empty() {
                    return Err(throw_type_error(
                        &ctx,
                        &format!(".{name}() expects {expected} inputs but got 0."),
                    ));
                }
                let (pattern, sidecars) = sequence_args_bridged(&ctx, &args.0)?;
                (vec![pattern], sidecars)
            } else {
                let (patterns, sidecars) = reify_registered_args(&ctx, &args.0, false)?;
                if patterns.len() != expected {
                    return Err(throw_type_error(
                        &ctx,
                        &format!(
                            ".{name}() expects {expected} inputs but got {}.",
                            patterns.len()
                        ),
                    ));
                }
                (patterns, sidecars)
            };
            let receiver = {
                let wrapper = this.0.borrow();
                sidecars.push(Sidecar::of(&wrapper));
                wrapper.pattern.clone()
            };
            arguments.push(receiver);
            derive_wrapper(ctx, method_body(&arguments), &sidecars)
        }),
    )
    .map_err(|error| error.to_string())?;
    proto.set(name, method).map_err(|error| error.to_string())?;

    match export {
        UpstreamExport::NameListObject => return Ok(()),
        UpstreamExport::Callable => {}
    }
    let raw = Function::new(
        ctx.clone(),
        hr_rest(move |ctx, args| {
            let (patterns, sidecars) = reify_registered_args(&ctx, &args.0, false)?;
            derive_wrapper(ctx, body(&patterns), &sidecars)
        }),
    )
    .map_err(|error| error.to_string())?;
    globals
        .set(
            name,
            native_curry(ctx, raw, arity, Vec::new()).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())
}

pub(super) fn install_method_fallback<'js>(
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
    globals: &rquickjs::Object<'js>,
    name: &'static str,
    arity: usize,
) -> Result<(), String> {
    let current: rquickjs::Value = globals.get(name).map_err(|error| error.to_string())?;
    if !current.is_undefined() {
        return Ok(());
    }
    let method: rquickjs::Value = proto.get(name).map_err(|error| error.to_string())?;
    if !method.is_function() {
        return Ok(());
    }
    let raw = Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, args: rquickjs::function::Rest<rquickjs::Value<'js>>| {
            let Some(terminal) = args.0.get(arity.saturating_sub(1)) else {
                return Ok(rquickjs::Value::new_undefined(ctx));
            };
            let reify: Function = ctx.globals().get("reify")?;
            let receiver: rquickjs::Value = reify.call((terminal.clone(),))?;
            let method: Function = receiver
                .as_object()
                .ok_or_else(|| {
                    rquickjs::Error::new_from_js_message("value", "Pattern", "not a pattern")
                })?
                .get(name)?;
            let mut call = Args::new(ctx.clone(), arity.saturating_sub(1));
            call.this(receiver)?;
            if name == "superimpose" {
                let transforms = args
                    .0
                    .first()
                    .and_then(rquickjs::Value::as_array)
                    .ok_or_else(|| {
                        rquickjs::Error::new_from_js_message("value", "Array", "not iterable")
                    })?;
                // Each transform becomes one argument of the method call, so
                // the array is copied out through the guard before the call's
                // argument vector grows (see `js_array_values`).
                for transform in js_array_values::<rquickjs::Value>(&ctx, transforms)? {
                    call.push_arg(transform)?;
                }
            } else {
                for argument in args.0.iter().take(arity.saturating_sub(1)) {
                    call.push_arg(argument.clone())?;
                }
            }
            call.apply(&method)
        },
    )
    .map_err(|error| error.to_string())?;
    globals
        .set(
            name,
            native_curry(ctx, raw, arity, Vec::new()).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())
}
