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
/// The tag of each lane with a lane number, by lane name. See `collect`.
const TAGS: usize = 5;
/// The pattern of each muted lane with a tag, by tag. See `collect`.
const MUTED: usize = 6;
/// The pattern of the last `.q()`, for the lane call after it. See `quiet`.
const QUIET: usize = 7;

fn bare_object<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<rquickjs::Object<'js>> {
    let object = rquickjs::Object::new(ctx.clone())?;
    object.set_prototype(None)?;
    Ok(object)
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
    // The label rewrite gives a labeled statement its lane number as the
    // second argument. The plugin notes of the lane then carry `#<number>`.
    let tag = common::argument(&ctx, &args.0, 1)
        .as_number()
        .filter(|number| *number >= 0.0 && number.fract() == 0.0)
        .map(|number| format!("#{}", number as u64));
    let quieted = quieted(&ctx, &state, &this.0)?;
    let mut id = common::argument(&ctx, &args.0, 0);
    if let Some(name) = id.as_string() {
        let mut name = name.to_string()?;
        if name.starts_with('_') || name.ends_with('_') {
            // A muted lane goes to the shadow with the tag it has when it
            // plays. A muted `$` lane with no number has no such tag.
            let named = !name.contains('$');
            let tag = tag.or_else(|| named.then(|| name.trim_matches('_').to_owned()));
            if let Some(tag) = tag {
                let muted: rquickjs::Object = state.get(MUTED)?;
                muted.set(tag, quieted.unwrap_or(this.0))?;
            }
            return state.get(SILENCE);
        }
        if name.contains('$') {
            let index: u32 = state.get(ANONYMOUS_INDEX)?;
            name.push_str(&index.to_string());
            state.set(ANONYMOUS_INDEX, index.saturating_add(1))?;
            id = rquickjs::String::from_str(ctx.clone(), &name)?.into_value();
        }
        // A lane that plays the silence of a `.q()` has the pattern before
        // the `.q()` in the shadow, with the tag of the lane.
        if let Some(quieted) = quieted {
            let muted: rquickjs::Object = state.get(MUTED)?;
            muted.set(tag.clone().unwrap_or(name), quieted)?;
        }
    }
    let patterns: rquickjs::Object = state.get(PATTERNS)?;
    patterns.set(id.clone(), this.0.clone())?;
    // With no number, this removes the tag of an earlier lane with the same
    // name.
    let tags: rquickjs::Object = state.get(TAGS)?;
    tags.set(id, tag)?;
    Ok(this.0)
}

/// The pattern of the `.q()` right before this lane call, when the lane
/// plays the silence of that `.q()`. Each lane call clears it.
fn quieted<'js>(
    ctx: &Ctx<'js>,
    state: &rquickjs::Array<'js>,
    this: &rquickjs::Value<'js>,
) -> rquickjs::Result<Option<rquickjs::Value<'js>>> {
    let quieted: rquickjs::Value = state.get(QUIET)?;
    if quieted.is_undefined() {
        return Ok(None);
    }
    state.set(QUIET, common::undefined(ctx))?;
    let silence: rquickjs::Value = state.get(SILENCE)?;
    Ok((quieted.is_object() && same_js_value(ctx, this, &silence)).then_some(quieted))
}

/// `.q()`: silence in place of the pattern. The lane call after it keeps
/// the pattern for the shadow of the score: see `quieted`. With a second
/// `.q()` before that call, the lane cannot tell which pattern is its own,
/// and it keeps none.
fn quiet<'js>(
    ctx: Ctx<'js>,
    this: This<rquickjs::Value<'js>>,
    _args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let state = state(&ctx)?;
    let held: rquickjs::Value = state.get(QUIET)?;
    if held.is_undefined() {
        state.set(QUIET, this.0)?;
    } else {
        state.set(QUIET, rquickjs::Value::new_null(ctx))?;
    }
    state.get(SILENCE)
}

fn reset<'js>(ctx: Ctx<'js>) -> rquickjs::Result<()> {
    let state = state(&ctx)?;
    state.set(PATTERNS, bare_object(&ctx)?)?;
    state.set(TAGS, bare_object(&ctx)?)?;
    state.set(MUTED, bare_object(&ctx)?)?;
    state.set(QUIET, common::undefined(&ctx))?;
    state.set(ANONYMOUS_INDEX, 0_u32)?;
    state.set(ALL_TRANSFORM, rquickjs::Value::new_null(ctx))
}

/// The REPL's `hush()`: drops every lane collected so far, muted lanes too,
/// and the `all` transform, and returns silence. Lanes collected after the
/// call still play.
fn hush<'js>(ctx: Ctx<'js>) -> rquickjs::Result<rquickjs::Value<'js>> {
    reset(ctx.clone())?;
    state(&ctx)?.get(SILENCE)
}

/// A lane with its tag on each plugin note. Only a plugin note carries the
/// tag. Each note keeps its slider bindings.
fn tagged<'js>(
    ctx: &Ctx<'js>,
    pattern: &rquickjs::Value<'js>,
    tag: String,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let (pattern, sidecar) = reify_bridged(ctx, pattern)?;
    let pattern = pattern.fmap_keeping_live(move |value| {
        let plugin = |controls: &rustel_core::OrderedMap| {
            controls.contains_key("vst") || controls.contains_key("vsti")
        };
        match value {
            Value::Object(controls) if plugin(controls) => {}
            // A value JavaScript owns shows its keys after a copy.
            Value::JsValue(_) => {}
            _ => return None,
        }
        let Value::Object(mut controls) = rustel_core::materialize_js_value(value) else {
            return None;
        };
        if !plugin(&controls) {
            return None;
        }
        controls.insert("__rustel_lane".into(), Value::Str(tag.clone()));
        Some(Value::Object(controls))
    });
    Ok(derive_wrapper(ctx.clone(), pattern, &[sidecar])?.into_value())
}

/// The shadow of a score when its evaluation ran a plugin call: the lanes
/// that do not play, each with its tag, through the `all` transform. The
/// transform runs a second time here, in `scope`, the plugin scope before
/// its first run. A note then has the bindings it has when it plays. The
/// console lines of the second run go, and a throw drops the shadow.
fn shadow_of<'js>(
    ctx: &Ctx<'js>,
    state: &rquickjs::Array<'js>,
    lanes: Vec<(String, rquickjs::Value<'js>)>,
    transform: Option<&Function<'js>>,
    scope: Option<PluginScope>,
    logs: &RefCell<Vec<String>>,
) -> rquickjs::Result<Option<rquickjs::Class<'js, NativePatternWrapper<'js>>>> {
    let ran = with_bridge_frame(|frame: &BridgeFrame<'js>| frame.ran_plugin_calls());
    if lanes.is_empty() || !ran.unwrap_or(false) {
        return Ok(None);
    }
    let build = || -> rquickjs::Result<rquickjs::Value<'js>> {
        let mut shadow = Vec::with_capacity(lanes.len());
        for (tag, pattern) in lanes {
            shadow.push(tagged(ctx, &pattern, tag)?);
        }
        let stack: Function = state.get(STACK)?;
        let shadow = common::call(&stack, common::undefined(ctx), shadow)?;
        match transform {
            Some(transform) => transform.call((shadow,)),
            None => Ok(shadow),
        }
    };
    let kept = logs.borrow().len();
    let built = with_bridge_frame(|frame: &BridgeFrame<'js>| frame.in_plugin_scope(scope, build));
    logs.borrow_mut().truncate(kept);
    match built {
        Some(Ok(shadow)) => Ok(rquickjs::Class::from_value(&shadow).ok()),
        Some(Err(rquickjs::Error::Exception)) => {
            let _ = ctx.catch();
            Ok(None)
        }
        Some(Err(error)) => Err(error),
        None => Ok(None),
    }
}

fn finish<'js>(
    ctx: Ctx<'js>,
    returned: rquickjs::Value<'js>,
    logs: &RefCell<Vec<String>>,
    deadline: &Cell<Option<std::time::Instant>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let started = std::time::Instant::now();
    let state = state(&ctx)?;
    let patterns: rquickjs::Object = state.get(PATTERNS)?;
    let tags: rquickjs::Object = state.get(TAGS)?;
    let solo = |name: &str| name.len() > 1 && name.starts_with('S');
    let mut solo_active = false;
    for name in patterns.keys::<String>() {
        solo_active |= solo(&name?);
    }
    let mut lanes = Vec::new();
    // The lanes that do not play, with their tags: the lanes a solo drops
    // and the muted lanes.
    let mut shadow = Vec::new();
    for entry in patterns.props::<String, rquickjs::Value>() {
        let (name, pattern) = entry?;
        let plays = !solo_active || solo(&name);
        // A lane with no number has its name as tag.
        let tag: Option<String> = tags.get(name.as_str())?;
        let tag = tag.unwrap_or(name);
        if plays {
            lanes.push(tagged(&ctx, &pattern, tag)?);
        } else {
            shadow.push((tag, pattern));
        }
    }
    let muted: rquickjs::Object = state.get(MUTED)?;
    for entry in muted.props::<String, rquickjs::Value>() {
        shadow.push(entry?);
    }

    let mut result = if lanes.is_empty() {
        returned
    } else {
        let stack: Function = state.get(STACK)?;
        common::call(&stack, common::undefined(&ctx), lanes)?
    };
    let transform: rquickjs::Value = state.get(ALL_TRANSFORM)?;
    let transform = if rquickjs::Coerced::<bool>::from_js(&ctx, transform.clone())?.0 {
        let error = || throw_type_error(&ctx, "all transform is not a function");
        Some(transform.into_function().ok_or_else(error)?)
    } else {
        None
    };
    // The plugin scope before the transform: see `shadow_of`.
    let scope = with_bridge_frame(|frame: &BridgeFrame<'js>| frame.copy_plugin_scope()).flatten();
    if let Some(transform) = &transform {
        result = transform.call((result,))?;
    }
    // The shadow does the work above a second time, for the lanes that do
    // not play. A shadow that runs past the deadline refuses the score.
    // With less than 2 times this time left, the score has no shadow.
    let took = started.elapsed();
    let fits = deadline
        .get()
        .is_none_or(|at| at.saturating_duration_since(std::time::Instant::now()) > took * 2);
    let shadow = if fits {
        shadow_of(&ctx, &state, shadow, transform.as_ref(), scope, logs)?
    } else {
        None
    };
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
    // The evaluation stages the shadow with the score.
    if let Some(shadow) = shadow {
        host_stack(&ctx)?.as_object().set(LANE_SHADOW, shadow)?;
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
    runtime: &JsRuntime,
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
            bare_object(ctx).map_err(|error| error.to_string())?,
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
    state
        .set(TAGS, bare_object(ctx).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    state
        .set(MUTED, bare_object(ctx).map_err(|error| error.to_string())?)
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
    let logs = runtime.logs.clone();
    let deadline = runtime.deadline.clone();
    let finish = Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, returned: rquickjs::Value<'js>| {
            finish(ctx, returned, &logs, &deadline)
        },
    )
    .map_err(|error| error.to_string())?;
    host.as_object()
        .set(LANE_RESET, reset)
        .map_err(|error| error.to_string())?;
    host.as_object()
        .set(LANE_FINISH, finish)
        .map_err(|error| error.to_string())?;
    Ok(())
}
