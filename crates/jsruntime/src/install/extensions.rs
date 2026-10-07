//! Host adapters for statically linked score extensions.
//!
//! Public extension names and authorship live in `rustel-ext`. This module
//! implements the small JavaScript capabilities their descriptors request;
//! it does not know which collection chose a name or how musicians spell it.

use super::*;
use rquickjs::function::Rest;

fn extension_state_property(key: &str) -> String {
    format!("__rustel_extension_state:{key}")
}

fn initialize_pattern_states<'js>(ctx: &Ctx<'js>) -> Result<(), String> {
    let stack = host_stack(ctx).map_err(|error| error.to_string())?;
    for state in rustel_ext::pattern_states() {
        let wrapper = derive_wrapper(ctx.clone(), (state.initial)(), &[])
            .map_err(|error| describe_js_error(ctx, error))?;
        stack
            .as_object()
            .set(extension_state_property(state.key), wrapper)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn read_pattern_state<'js>(
    ctx: &Ctx<'js>,
    key: &str,
) -> rquickjs::Result<(rustel_core::Pattern, Sidecar<'js>)> {
    let stack = host_stack(ctx)?;
    let value: rquickjs::Value = stack.as_object().get(extension_state_property(key))?;
    unwrap_pattern(&value).ok_or_else(|| {
        throw_type_error(
            ctx,
            &format!("extension pattern state `{key}` is not a Pattern"),
        )
    })
}

fn call_pattern_callable<'js>(
    ctx: Ctx<'js>,
    declaration: rustel_ext::PatternCallable,
    args: &[rquickjs::Value<'js>],
    receiver: Option<(rustel_core::Pattern, Sidecar<'js>)>,
) -> rquickjs::Result<rquickjs::Class<'js, NativePatternWrapper<'js>>> {
    let (patterns, mut sidecars) = reify_registered_args(&ctx, args, false)?;
    let (receiver_pattern, receiver_sidecar) = match receiver {
        Some((pattern, sidecar)) => (Some(pattern), Some(sidecar)),
        None => (None, None),
    };
    if let Some(sidecar) = receiver_sidecar {
        sidecars.push(sidecar);
    }
    publish_owner_cells(&sidecars)?;
    let receiver_pattern = receiver_pattern.as_ref();

    let pattern = match declaration.behavior {
        rustel_ext::PatternCallableBehavior::Stateless(call) => call(&patterns, receiver_pattern),
        rustel_ext::PatternCallableBehavior::Fallible(call) => {
            call(&patterns, receiver_pattern).map_err(|message| throw_type_error(&ctx, message))?
        }
        rustel_ext::PatternCallableBehavior::ReadState { key, call } => {
            let (state, sidecar) = read_pattern_state(&ctx, key)?;
            sidecars.push(sidecar);
            call(&state, &patterns, receiver_pattern)
        }
        rustel_ext::PatternCallableBehavior::WriteState { key } => {
            let pattern = patterns
                .first()
                .cloned()
                .unwrap_or_else(rustel_core::silence);
            let sources = sidecars.first().cloned().into_iter().collect::<Vec<_>>();
            let wrapper = derive_wrapper(ctx.clone(), pattern, &sources)?;
            host_stack(&ctx)?
                .as_object()
                .set(extension_state_property(key), wrapper)?;
            rustel_core::silence()
        }
    };
    derive_wrapper(ctx, pattern, &sidecars)
}

fn install_pattern_callable<'js>(
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
    globals: &rquickjs::Object<'js>,
    declaration: rustel_ext::PatternCallable,
) -> Result<(), String> {
    for name in declaration.names {
        if declaration.surface.global {
            let call = declaration;
            let function = Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, args: Rest<rquickjs::Value<'js>>| {
                    call_pattern_callable(ctx, call, &args.0, None)
                },
            )
            .map_err(|error| error.to_string())?;
            configure_function(&function, name, declaration.arity, false)
                .map_err(|error| error.to_string())?;
            globals
                .set(*name, function)
                .map_err(|error| error.to_string())?;
        }

        if declaration.surface.method {
            let call = declaration;
            let function = Function::new(
                ctx.clone(),
                hr_this_rest(move |ctx, this, args| {
                    let receiver = {
                        let borrowed = this.0.borrow();
                        (borrowed.pattern.clone(), Sidecar::of(&borrowed))
                    };
                    call_pattern_callable(ctx, call, &args.0, Some(receiver))
                }),
            )
            .map_err(|error| error.to_string())?;
            configure_function(&function, name, declaration.arity, false)
                .map_err(|error| error.to_string())?;
            proto
                .set(*name, function)
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn install_value_callable<'js>(
    ctx: &Ctx<'js>,
    globals: &rquickjs::Object<'js>,
    declaration: rustel_ext::ValueCallable,
) -> Result<(), String> {
    for name in declaration.names {
        let call = declaration.call;
        let function = Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, args: Rest<rquickjs::Value<'js>>| {
                let mut values = Vec::with_capacity(args.0.len());
                // Every argument is kept for the one call: one budget.
                let budget = Cell::new(js_element_cap::<Value>(&ctx)?);
                for argument in &args.0 {
                    let (value, _) = materialize_js_value_within(&ctx, argument, &budget)?;
                    values.push(value);
                }
                let value = call(&values).map_err(|message| throw_type_error(&ctx, message))?;
                to_js(&ctx, &value)
            },
        )
        .map_err(|error| error.to_string())?;
        configure_function(&function, name, declaration.arity, false)
            .map_err(|error| error.to_string())?;
        globals
            .set(*name, function)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub(super) fn install<'js>(
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
    globals: &rquickjs::Object<'js>,
) -> Result<(), String> {
    initialize_pattern_states(ctx)?;
    for declaration in rustel_ext::pattern_callables() {
        install_pattern_callable(ctx, proto, globals, *declaration)?;
    }
    for declaration in rustel_ext::value_callables() {
        install_value_callable(ctx, globals, *declaration)?;
    }
    Ok(())
}
