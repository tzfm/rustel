use super::*;
use rustel_core::{JoinMode, PickIndexMode};

fn argument<'js>(
    ctx: &Ctx<'js>,
    args: &[rquickjs::Value<'js>],
    index: usize,
) -> rquickjs::Value<'js> {
    args.get(index)
        .cloned()
        .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()))
}

pub(super) fn install<'js>(
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
    globals: &rquickjs::Object<'js>,
) -> Result<(), String> {
    let wchoose = Function::new(
        ctx.clone(),
        hr_rest(move |ctx, args| native_wchoose(ctx, args, false)),
    )
    .map_err(|error| error.to_string())?;
    configure_function(&wchoose, "wchoose", 0, false).map_err(|error| error.to_string())?;
    globals
        .set("wchoose", wchoose)
        .map_err(|error| error.to_string())?;

    let wchoose_cycles = Function::new(
        ctx.clone(),
        hr_rest(move |ctx, args| native_wchoose(ctx, args, true)),
    )
    .map_err(|error| error.to_string())?;
    configure_function(&wchoose_cycles, "wchooseCycles", 0, false)
        .map_err(|error| error.to_string())?;
    globals
        .set("wchooseCycles", wchoose_cycles.clone())
        .map_err(|error| error.to_string())?;
    globals
        .set("wrandcat", wchoose_cycles)
        .map_err(|error| error.to_string())?;

    let pick_specs = [
        ("pick", PickIndexMode::Clamp, JoinMode::Inner),
        ("pickmod", PickIndexMode::Remainder, JoinMode::Inner),
        ("pickOut", PickIndexMode::Clamp, JoinMode::Outer),
        ("pickmodOut", PickIndexMode::Remainder, JoinMode::Outer),
        ("pickRestart", PickIndexMode::Clamp, JoinMode::Restart),
        (
            "pickmodRestart",
            PickIndexMode::Remainder,
            JoinMode::Restart,
        ),
        ("pickReset", PickIndexMode::Clamp, JoinMode::Reset),
        ("pickmodReset", PickIndexMode::Remainder, JoinMode::Reset),
        ("inhabit", PickIndexMode::Clamp, JoinMode::Squeeze),
        ("pickSqueeze", PickIndexMode::Clamp, JoinMode::Squeeze),
        ("inhabitmod", PickIndexMode::Remainder, JoinMode::Squeeze),
        (
            "pickmodSqueeze",
            PickIndexMode::Remainder,
            JoinMode::Squeeze,
        ),
    ];

    for (name, index_mode, join_mode) in pick_specs {
        let method = Function::new(
            ctx.clone(),
            hr_this_rest(move |ctx, this, args| {
                let (selector, mut sidecars) = {
                    let wrapper = this.0.borrow();
                    (wrapper.pattern.clone(), vec![Sidecar::of(&wrapper)])
                };
                if args.0.len() != 1 {
                    // Several arguments are a sequence to pick from; none
                    // is a mistake. A sequence of nothing is silence, and
                    // a silent lookup silences the whole chain with no
                    // error in the score.
                    if args.0.is_empty() {
                        return Err(throw_type_error(
                            &ctx,
                            &format!(".{name}() expects 1 input but got 0."),
                        ));
                    }
                    let (lookup, nested) = sequence_args_bridged(&ctx, &args.0)?;
                    sidecars.extend(nested);
                    return derive_wrapper(
                        ctx,
                        rustel_core::pick_patternified(selector, lookup, index_mode, join_mode),
                        &sidecars,
                    );
                }
                let lookup = argument(&ctx, &args.0, 0);
                derive_pick(ctx, &lookup, selector, sidecars, index_mode, join_mode)
            }),
        )
        .map_err(|error| error.to_string())?;
        proto.set(name, method).map_err(|error| error.to_string())?;

        let raw_method = Function::new(
            ctx.clone(),
            hr_this_rest(move |ctx, this, args| {
                let (selector, sidecar) = {
                    let wrapper = this.0.borrow();
                    (wrapper.pattern.clone(), Sidecar::of(&wrapper))
                };
                let lookup = argument(&ctx, &args.0, 0);
                derive_pick_unpatternified(
                    ctx,
                    &lookup,
                    selector,
                    vec![sidecar],
                    index_mode,
                    join_mode,
                )
            }),
        )
        .map_err(|error| error.to_string())?;
        proto
            .set(format!("_{name}"), raw_method)
            .map_err(|error| error.to_string())?;

        let raw_free = Function::new(
            ctx.clone(),
            hr_rest(move |ctx, args| {
                let lookup = argument(&ctx, &args.0, 0);
                let selector = argument(&ctx, &args.0, 1);
                native_pick(ctx, lookup, selector, index_mode, join_mode)
            }),
        )
        .map_err(|error| error.to_string())?;
        let free = native_named_curry(ctx, raw_free, 2, "curried", "partial", true)
            .map_err(|error| describe_js_error(ctx, error))?;
        globals.set(name, free).map_err(|error| error.to_string())?;
    }

    let pick = Function::new(
        ctx.clone(),
        hr_rest(move |ctx, args| {
            let mut lookup = argument(&ctx, &args.0, 0);
            let mut selector = argument(&ctx, &args.0, 1);
            if is_array(&ctx, &selector)? {
                std::mem::swap(&mut lookup, &mut selector);
            }
            native_pick(ctx, lookup, selector, PickIndexMode::Clamp, JoinMode::Inner)
        }),
    )
    .map_err(|error| error.to_string())?;
    configure_function(&pick, "pick", 2, true).map_err(|error| error.to_string())?;
    globals
        .set("pick", pick)
        .map_err(|error| error.to_string())?;

    let squeeze = Function::new(
        ctx.clone(),
        hr_rest(move |ctx, args| {
            if args.0.is_empty() {
                return Err(refuse_empty_call(&ctx, "squeeze", 2));
            }
            let selector = argument(&ctx, &args.0, 0);
            let lookup = argument(&ctx, &args.0, 1);
            let (selector, selector_sidecar) = reify_bridged(&ctx, &selector)?;
            let (lookup, mut sidecars) = native_squeeze_lookup(&ctx, &lookup)?;
            sidecars.push(selector_sidecar);
            derive_wrapper(
                ctx,
                rustel_core::pick(selector, lookup, PickIndexMode::Modulo, JoinMode::Squeeze),
                &sidecars,
            )
        }),
    )
    .map_err(|error| error.to_string())?;
    configure_function(&squeeze, "squeeze", 2, false).map_err(|error| error.to_string())?;
    globals
        .set("squeeze", squeeze)
        .map_err(|error| error.to_string())?;

    for (name, index_mode) in [
        ("pickF", PickIndexMode::Clamp),
        ("pickmodF", PickIndexMode::Remainder),
    ] {
        let compat_swap = name == "pickF";
        let method = Function::new(
            ctx.clone(),
            hr_this_rest(move |ctx, this, args| {
                if args.0.len() != 2 {
                    return Err(rquickjs::Exception::throw_message(
                        &ctx,
                        &format!(".{name}() expects 2 inputs but got {}.", args.0.len()),
                    ));
                }
                let (receiver, sidecar) = {
                    let wrapper = this.0.borrow();
                    (wrapper.pattern.clone(), Sidecar::of(&wrapper))
                };
                derive_pick_f(
                    ctx,
                    &args.0[0],
                    &args.0[1],
                    receiver,
                    vec![sidecar],
                    index_mode,
                    compat_swap,
                )
            }),
        )
        .map_err(|error| error.to_string())?;
        proto.set(name, method).map_err(|error| error.to_string())?;

        let raw_method = Function::new(
            ctx.clone(),
            hr_this_rest(move |ctx, this, args| {
                let (receiver, sidecar) = {
                    let wrapper = this.0.borrow();
                    (wrapper.pattern.clone(), Sidecar::of(&wrapper))
                };
                let pick_value = argument(&ctx, &args.0, 0);
                let lookup_value = argument(&ctx, &args.0, 1);
                derive_pick_f(
                    ctx,
                    &pick_value,
                    &lookup_value,
                    receiver,
                    vec![sidecar],
                    index_mode,
                    compat_swap,
                )
            }),
        )
        .map_err(|error| error.to_string())?;
        proto
            .set(format!("_{name}"), raw_method)
            .map_err(|error| error.to_string())?;

        let raw_free = Function::new(
            ctx.clone(),
            hr_rest(move |ctx, args| {
                let pick_value = argument(&ctx, &args.0, 0);
                let lookup_value = argument(&ctx, &args.0, 1);
                let receiver_value = argument(&ctx, &args.0, 2);
                let (receiver, receiver_sidecar) = reify_bridged(&ctx, &receiver_value)?;
                derive_pick_f(
                    ctx,
                    &pick_value,
                    &lookup_value,
                    receiver,
                    vec![receiver_sidecar],
                    index_mode,
                    compat_swap,
                )
            }),
        )
        .map_err(|error| error.to_string())?;
        let free = native_named_curry(ctx, raw_free, 3, "curried", "partial", true)
            .map_err(|error| describe_js_error(ctx, error))?;
        globals.set(name, free).map_err(|error| error.to_string())?;
    }

    Ok(())
}
