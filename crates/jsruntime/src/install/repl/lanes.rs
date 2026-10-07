use super::*;
use rquickjs::{
    function::{Rest, This},
    object::Property,
};

const LANE_STATE: &str = "__rustel_lane_state";
const PATTERNS: usize = 0;
const ANONYMOUS_INDEX: usize = 1;
const ALL_TRANSFORM: usize = 2;
const SILENCE: usize = 3;
const STACK: usize = 4;

fn new_patterns<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<rquickjs::Object<'js>> {
    let patterns = rquickjs::Object::new(ctx.clone())?;
    patterns.set_prototype(None)?;
    Ok(patterns)
}

fn state<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<rquickjs::Array<'js>> {
    host_stack(ctx)?.as_object().get(LANE_STATE)
}

fn all<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let state = state(&ctx)?;
    state.set(ALL_TRANSFORM, common::argument(&ctx, &args.0, 0))?;
    state.get(SILENCE)
}

fn collect<'js>(
    ctx: Ctx<'js>,
    this: This<rquickjs::Value<'js>>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let state = state(&ctx)?;
    let mut id = common::argument(&ctx, &args.0, 0);
    if let Some(name) = id.as_string() {
        let mut name = name.to_string()?;
        if name.starts_with('_') || name.ends_with('_') {
            return state.get(SILENCE);
        }
        if name.contains('$') {
            let index: u32 = state.get(ANONYMOUS_INDEX)?;
            name.push_str(&index.to_string());
            state.set(ANONYMOUS_INDEX, index.saturating_add(1))?;
            id = rquickjs::String::from_str(ctx.clone(), &name)?.into_value();
        }
    }
    let patterns: rquickjs::Object = state.get(PATTERNS)?;
    patterns.set(id, this.0.clone())?;
    Ok(this.0)
}

fn quiet<'js>(
    ctx: Ctx<'js>,
    _this: This<rquickjs::Value<'js>>,
    _args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    state(&ctx)?.get(SILENCE)
}

fn reset<'js>(ctx: Ctx<'js>) -> rquickjs::Result<()> {
    let state = state(&ctx)?;
    state.set(PATTERNS, new_patterns(&ctx)?)?;
    state.set(ANONYMOUS_INDEX, 0_u32)?;
    state.set(ALL_TRANSFORM, rquickjs::Value::new_null(ctx))
}

/// The REPL's `hush()`: drops every lane collected so far and the `all`
/// transform, and returns silence. Lanes collected after the call still play.
fn hush<'js>(ctx: Ctx<'js>) -> rquickjs::Result<rquickjs::Value<'js>> {
    reset(ctx.clone())?;
    state(&ctx)?.get(SILENCE)
}

fn finish<'js>(
    ctx: Ctx<'js>,
    returned: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let state = state(&ctx)?;
    let patterns: rquickjs::Object = state.get(PATTERNS)?;
    let mut lanes = Vec::new();
    let mut solo_active = false;
    for entry in patterns.props::<String, rquickjs::Value>() {
        let (name, pattern) = entry?;
        let solo = name.len() > 1 && name.starts_with('S');
        if solo && !solo_active {
            lanes.clear();
            solo_active = true;
        }
        if !solo_active || solo {
            lanes.push(pattern);
        }
    }

    let mut result = if lanes.is_empty() {
        returned
    } else {
        let stack: Function = state.get(STACK)?;
        common::call(&stack, common::undefined(&ctx), lanes)?
    };
    let transform: rquickjs::Value = state.get(ALL_TRANSFORM)?;
    if rquickjs::Coerced::<bool>::from_js(&ctx, transform.clone())?.0 {
        let transform = transform
            .into_function()
            .ok_or_else(|| throw_type_error(&ctx, "all transform is not a function"))?;
        result = transform.call((result,))?;
    }
    // Install the gain curve after lane composition so it covers the full
    // result. The value transform includes the default gain of 0.8 and any
    // explicit velocity; without a curve, values pass through unchanged.
    let curve: rquickjs::Value = ctx
        .globals()
        .get(crate::install::native_surface::bindings::GAIN_CURVE)?;
    if let Some(curve) = curve.into_function() {
        let apply: Function = ctx.eval(APPLY_GAIN_CURVE)?;
        result = apply.call((result, curve))?;
    }
    Ok(result)
}

/// The values of a pattern through the gain curve. Only objects carry
/// controls; anything else is left as it is, and so is a value that is not
/// a pattern at all.
const APPLY_GAIN_CURVE: &str = r#"(pattern, curve) => {
  if (!pattern || typeof pattern.withValue !== 'function') return pattern;
  return pattern.withValue((value) => {
    if (value === null || typeof value !== 'object' || Array.isArray(value)) return value;
    const shaped = { ...value, gain: curve(value.gain ?? 0.8) };
    if (value.velocity !== undefined) shaped.velocity = curve(value.velocity);
    return shaped;
  });
}"#;

pub(super) fn install<'js>(
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
    globals: &rquickjs::Object<'js>,
) -> Result<(), String> {
    let state = rquickjs::Array::new(ctx.clone()).map_err(|error| error.to_string())?;
    state
        .as_object()
        .set_prototype(None)
        .map_err(|error| error.to_string())?;
    state
        .set(
            PATTERNS,
            new_patterns(ctx).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    state
        .set(ANONYMOUS_INDEX, 0_u32)
        .map_err(|error| error.to_string())?;
    state
        .set(ALL_TRANSFORM, rquickjs::Value::new_null(ctx.clone()))
        .map_err(|error| error.to_string())?;
    state
        .set(
            SILENCE,
            globals
                .get::<_, rquickjs::Value>("silence")
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    state
        .set(
            STACK,
            globals
                .get::<_, rquickjs::Value>("stack")
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;

    let host = host_stack(ctx).map_err(|error| error.to_string())?;
    host.as_object()
        .set(LANE_STATE, state)
        .map_err(|error| error.to_string())?;

    let all = Function::new(ctx.clone(), all).map_err(|error| error.to_string())?;
    configure_function(&all, "", 1, false).map_err(|error| error.to_string())?;
    globals.set("all", all).map_err(|error| error.to_string())?;

    let hush = Function::new(ctx.clone(), hush).map_err(|error| error.to_string())?;
    configure_function(&hush, "hush", 0, false).map_err(|error| error.to_string())?;
    globals
        .set("hush", hush)
        .map_err(|error| error.to_string())?;

    let collect = Function::new(ctx.clone(), collect).map_err(|error| error.to_string())?;
    configure_function(&collect, "", 1, true).map_err(|error| error.to_string())?;
    proto
        .prop(
            "p",
            Property::from(collect)
                .writable()
                .enumerable()
                .configurable(),
        )
        .map_err(|error| error.to_string())?;

    let quiet = Function::new(ctx.clone(), quiet).map_err(|error| error.to_string())?;
    configure_function(&quiet, "", 0, true).map_err(|error| error.to_string())?;
    proto
        .prop(
            "q",
            Property::from(quiet).writable().enumerable().configurable(),
        )
        .map_err(|error| error.to_string())?;

    let reset = Function::new(ctx.clone(), reset).map_err(|error| error.to_string())?;
    let finish = Function::new(ctx.clone(), finish).map_err(|error| error.to_string())?;
    host.as_object()
        .set(LANE_RESET, reset)
        .map_err(|error| error.to_string())?;
    host.as_object()
        .set(LANE_FINISH, finish)
        .map_err(|error| error.to_string())?;
    Ok(())
}
