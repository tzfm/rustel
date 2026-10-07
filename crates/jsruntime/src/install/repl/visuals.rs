use super::*;
use rquickjs::{
    function::{Rest, This},
    object::Property,
};

const DEFAULT_STATE: &str = "__rustel_default_state";
const DEFAULTS: usize = 0;
const PRISTINE: usize = 1;
const LIVE: usize = 2;

fn state<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<rquickjs::Array<'js>> {
    host_stack(ctx)?.as_object().get(DEFAULT_STATE)
}

fn map_from<'js>(
    ctx: &Ctx<'js>,
    values: &rquickjs::Object<'js>,
) -> rquickjs::Result<rquickjs::Object<'js>> {
    let entries = rquickjs::Array::new(ctx.clone())?;
    for (index, entry) in values.props::<String, rquickjs::Value>().enumerate() {
        let (key, value) = entry?;
        let pair = rquickjs::Array::new(ctx.clone())?;
        pair.set(0, key)?;
        pair.set(1, value)?;
        entries.set(index, pair)?;
    }
    let constructor: rquickjs::function::Constructor = ctx.globals().get("Map")?;
    let value: rquickjs::Value = constructor.construct((entries,))?;
    value
        .into_object()
        .ok_or_else(|| throw_type_error(ctx, "Map constructor returned a non-object"))
}

fn map_method<'js>(
    ctx: &Ctx<'js>,
    map: rquickjs::Object<'js>,
    name: &str,
    values: impl IntoIterator<Item = rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    common::call_method(ctx, map.clone().into_value(), name, values)
}

fn set_default<'js>(ctx: Ctx<'js>, args: Rest<rquickjs::Value<'js>>) -> rquickjs::Result<()> {
    let state = state(&ctx)?;
    let defaults: rquickjs::Object = state.get(DEFAULTS)?;
    defaults.set(
        common::argument(&ctx, &args.0, 0),
        common::argument(&ctx, &args.0, 1),
    )
}

fn reset_defaults<'js>(ctx: Ctx<'js>) -> rquickjs::Result<()> {
    let state = state(&ctx)?;
    let defaults: rquickjs::Object = state.get(DEFAULTS)?;
    let pristine: rquickjs::Object = state.get(PRISTINE)?;
    for entry in pristine.props::<rquickjs::Atom, rquickjs::Value>() {
        let (key, value) = entry?;
        defaults.set(key, value)?;
    }
    Ok(())
}

fn set_default_value<'js>(ctx: Ctx<'js>, args: Rest<rquickjs::Value<'js>>) -> rquickjs::Result<()> {
    let map: rquickjs::Object = state(&ctx)?.get(LIVE)?;
    map_method(
        &ctx,
        map,
        "set",
        [
            common::argument(&ctx, &args.0, 0),
            common::argument(&ctx, &args.0, 1),
        ],
    )?;
    Ok(())
}

fn set_default_values<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<()> {
    let input = common::object(&ctx, common::argument(&ctx, &args.0, 0))?;
    let map: rquickjs::Object = state(&ctx)?.get(LIVE)?;
    for entry in input.props::<String, rquickjs::Value>() {
        let (key, value) = entry?;
        map_method(
            &ctx,
            map.clone(),
            "set",
            [
                rquickjs::String::from_str(ctx.clone(), &key)?.into_value(),
                value,
            ],
        )?;
    }
    Ok(())
}

fn get_default_value<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let map: rquickjs::Object = state(&ctx)?.get(LIVE)?;
    map_method(&ctx, map, "get", [common::argument(&ctx, &args.0, 0)])
}

fn reset_default_values<'js>(ctx: Ctx<'js>) -> rquickjs::Result<()> {
    let state = state(&ctx)?;
    let defaults: rquickjs::Object = state.get(DEFAULTS)?;
    state.set(LIVE, map_from(&ctx, &defaults)?)
}

fn clear_scope<'js>(ctx: Ctx<'js>) -> rquickjs::Result<rquickjs::Value<'js>> {
    let globals = ctx.globals();
    let protected = host_protected_globals(&ctx)?;
    let keys: rquickjs::Object = globals.get("userDefinedKeys")?;
    let iterator = common::call_method(&ctx, keys.clone().into_value(), "values", [])?
        .into_object()
        .ok_or_else(|| throw_type_error(&ctx, "userDefinedKeys iterator is not an object"))?;
    loop {
        let next = common::call_method(&ctx, iterator.clone().into_value(), "next", [])?
            .into_object()
            .ok_or_else(|| {
                throw_type_error(&ctx, "userDefinedKeys iterator result is not an object")
            })?;
        let done: rquickjs::Value = next.get("done")?;
        if rquickjs::Coerced::<bool>::from_js(&ctx, done)?.0 {
            break;
        }
        // Cleanup removes only string keys not registered by the host.
        let value: rquickjs::Value = next.get("value")?;
        let Some(key) = value.as_string() else {
            continue;
        };
        let key = key.to_string()?;
        let is_protected = protected.borrow().contains(&key);
        if !is_protected {
            globals.remove(key.as_str())?;
        }
    }
    common::call_method(&ctx, keys.into_value(), "clear", [])?;
    globals.get("silence")
}

pub(in crate::install) fn install_clear_scope(ctx: &Ctx<'_>) -> Result<(), String> {
    // Initial bindings seed the host-only set; late installers add their names.
    let names: rquickjs::Array = ctx
        .eval("Object.getOwnPropertyNames(globalThis)")
        .map_err(|error| error.to_string())?;
    let protected = host_protected_globals(ctx).map_err(|error| error.to_string())?;
    for key in names.iter::<String>() {
        let key = key.map_err(|error| error.to_string())?;
        protected.borrow_mut().insert(key);
    }
    let clear = Function::new(ctx.clone(), clear_scope).map_err(|error| error.to_string())?;
    configure_function(&clear, "", 0, false).map_err(|error| error.to_string())?;
    set_host_global(ctx, "clearScope", clear).map_err(|error| error.to_string())
}

fn set_version_defaults<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<()> {
    reset_default_values(ctx.clone())?;
    let version = common::argument(&ctx, &args.0, 0);
    let expected = rquickjs::String::from_str(ctx.clone(), "1.0")?.into_value();
    if unsafe {
        rquickjs::qjs::JS_IsStrictEqual(ctx.as_raw().as_ptr(), version.as_raw(), expected.as_raw())
    } {
        let map: rquickjs::Object = state(&ctx)?.get(LIVE)?;
        map_method(
            &ctx,
            map,
            "set",
            [
                rquickjs::String::from_str(ctx.clone(), "fanchor")?.into_value(),
                rquickjs::Value::new_float(ctx.clone(), 0.5),
            ],
        )?;
    }
    Ok(())
}

fn visual_pattern<'js>(
    ctx: Ctx<'js>,
    this: This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Class<'js, NativePatternWrapper<'js>>> {
    let Some(slot) = args
        .0
        .get(1)
        .and_then(rquickjs::Value::as_int)
        .and_then(|slot| u8::try_from(slot).ok())
        .filter(|slot| *slot < 64)
    else {
        return Ok(this.0);
    };
    let (pattern, sidecar) = {
        let wrapper = this.0.borrow();
        (
            wrapper.pattern.with_ui_visual_slot(slot),
            Sidecar::of(&wrapper),
        )
    };
    derive_wrapper(ctx, pattern, &[sidecar])
}

pub(super) fn install<'js>(
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
    globals: &rquickjs::Object<'js>,
) -> Result<(), String> {
    for name in [
        "_scope",
        "_tscope",
        "_pianoroll",
        "_punchcard",
        "_wordfall",
        "_spiral",
        "_pitchwheel",
        "_spectrum",
        "scope",
        "tscope",
        "pianoroll",
        "punchcard",
        "wordfall",
        "spiral",
        "pitchwheel",
        "spectrum",
    ] {
        let current: rquickjs::Value = proto.get(name).map_err(|error| error.to_string())?;
        if !rquickjs::Coerced::<bool>::from_js(ctx, current)
            .map_err(|error| error.to_string())?
            .0
        {
            let identity =
                Function::new(ctx.clone(), visual_pattern).map_err(|error| error.to_string())?;
            configure_function(&identity, "", 0, true).map_err(|error| error.to_string())?;
            proto
                .prop(name, Property::from(identity).writable().configurable())
                .map_err(|error| error.to_string())?;
        }
    }
    for name in ["theme", "fontFamily", "fontSize"] {
        let current: rquickjs::Value = proto.get(name).map_err(|error| error.to_string())?;
        if !rquickjs::Coerced::<bool>::from_js(ctx, current)
            .map_err(|error| error.to_string())?
            .0
        {
            let identity = Function::new(
                ctx.clone(),
                |this: This<rquickjs::Value<'js>>, _args: Rest<rquickjs::Value<'js>>| this.0,
            )
            .map_err(|error| error.to_string())?;
            configure_function(&identity, "", 0, true).map_err(|error| error.to_string())?;
            proto
                .prop(name, Property::from(identity).writable().configurable())
                .map_err(|error| error.to_string())?;
        }
    }
    let markcss = Function::new(
        ctx.clone(),
        |this: This<rquickjs::Value<'js>>, _args: Rest<rquickjs::Value<'js>>| this.0,
    )
    .map_err(|error| error.to_string())?;
    configure_function(&markcss, "", 0, true).map_err(|error| error.to_string())?;
    proto
        .prop("markcss", Property::from(markcss).writable().configurable())
        .map_err(|error| error.to_string())?;

    for name in [
        "scope",
        "tscope",
        "pianoroll",
        "punchcard",
        "wordfall",
        "spiral",
        "pitchwheel",
        "spectrum",
    ] {
        let current: rquickjs::Value = globals.get(name).map_err(|error| error.to_string())?;
        if current.as_function().is_none() {
            // strudel.cc's page-level spellings: `all(pianoroll)` hands the
            // painter a pattern; `all(pianoroll({ labels: 1 }))` hands it
            // options first and expects a function that takes the pattern.
            let visual = Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, args: Rest<rquickjs::Value<'js>>| {
                    let first = common::argument(&ctx, &args.0, 0);
                    let is_pattern = first.as_object().is_some_and(|object| {
                        object
                            .get::<_, rquickjs::Value>(name)
                            .is_ok_and(|method| method.is_function())
                    });
                    if is_pattern {
                        return common::call_method(&ctx, first, name, std::iter::empty());
                    }
                    let painter = Function::new(
                        ctx.clone(),
                        move |ctx: Ctx<'js>, args: Rest<rquickjs::Value<'js>>| {
                            common::call_method(
                                &ctx,
                                common::argument(&ctx, &args.0, 0),
                                name,
                                std::iter::once(first.clone()),
                            )
                        },
                    )?;
                    Ok(painter.into_value())
                },
            )
            .map_err(|error| error.to_string())?;
            configure_function(&visual, "visual", 1, false).map_err(|error| error.to_string())?;
            globals
                .set(name, visual)
                .map_err(|error| error.to_string())?;
        }
    }

    let strudel_scope: rquickjs::Value = globals
        .get("strudelScope")
        .map_err(|error| error.to_string())?;
    if strudel_scope.is_undefined() {
        globals
            .set("strudelScope", globals.clone())
            .map_err(|error| error.to_string())?;
        let constructor: rquickjs::function::Constructor =
            globals.get("Set").map_err(|error| error.to_string())?;
        let keys: rquickjs::Object = constructor
            .construct(())
            .map_err(|error| error.to_string())?;
        globals
            .set("userDefinedKeys", keys)
            .map_err(|error| error.to_string())?;
    }

    let defaults = rquickjs::Object::new(ctx.clone()).map_err(|error| error.to_string())?;
    defaults
        .set("s", "triangle")
        .map_err(|error| error.to_string())?;
    defaults
        .set("gain", 0.8_f64)
        .map_err(|error| error.to_string())?;
    defaults
        .set("postgain", 1_f64)
        .map_err(|error| error.to_string())?;
    defaults
        .set("density", ".03")
        .map_err(|error| error.to_string())?;
    let channels = rquickjs::Array::new(ctx.clone()).map_err(|error| error.to_string())?;
    channels.set(0, 1_i32).map_err(|error| error.to_string())?;
    channels.set(1, 2_i32).map_err(|error| error.to_string())?;
    defaults
        .set("channels", channels)
        .map_err(|error| error.to_string())?;
    for (key, value) in [
        ("phaserdepth", 0.75),
        ("shapevol", 1.0),
        ("distortvol", 1.0),
        ("distorttype", 0.0),
        ("delay", 0.0),
        ("busgain", 1.0),
    ] {
        defaults
            .set(key, value)
            .map_err(|error| error.to_string())?;
    }
    defaults
        .set("byteBeatExpression", "0")
        .map_err(|error| error.to_string())?;
    for (key, value) in [
        ("delayfeedback", 0.5),
        ("delaysync", 3.0 / 16.0),
        ("orbit", 1.0),
        ("i", 1.0),
        ("velocity", 1.0),
        ("fft", 8.0),
        ("tremolodepth", 1.0),
        ("tremolophase", 0.0),
        ("release", 0.01),
    ] {
        defaults
            .set(key, value)
            .map_err(|error| error.to_string())?;
    }
    let pristine = rquickjs::Object::new(ctx.clone()).map_err(|error| error.to_string())?;
    for entry in defaults.props::<rquickjs::Atom, rquickjs::Value>() {
        let (key, value) = entry.map_err(|error| error.to_string())?;
        pristine
            .set(key, value)
            .map_err(|error| error.to_string())?;
    }
    let live = map_from(ctx, &defaults).map_err(|error| error.to_string())?;
    let default_state = rquickjs::Array::new(ctx.clone()).map_err(|error| error.to_string())?;
    default_state
        .as_object()
        .set_prototype(None)
        .map_err(|error| error.to_string())?;
    default_state
        .set(DEFAULTS, defaults)
        .map_err(|error| error.to_string())?;
    default_state
        .set(PRISTINE, pristine)
        .map_err(|error| error.to_string())?;
    default_state
        .set(LIVE, live)
        .map_err(|error| error.to_string())?;
    host_stack(ctx)
        .map_err(|error| error.to_string())?
        .as_object()
        .set(DEFAULT_STATE, default_state)
        .map_err(|error| error.to_string())?;

    let current: rquickjs::Value = globals
        .get("setDefault")
        .map_err(|error| error.to_string())?;
    if current.as_function().is_none() {
        macro_rules! install_function {
            ($name:literal, $length:literal, $function:expr) => {{
                let function =
                    Function::new(ctx.clone(), $function).map_err(|error| error.to_string())?;
                configure_function(&function, "", $length, false)
                    .map_err(|error| error.to_string())?;
                globals
                    .set($name, function)
                    .map_err(|error| error.to_string())?;
            }};
        }
        install_function!("setDefault", 2, set_default);
        install_function!("resetDefaults", 0, reset_defaults);
        install_function!("setDefaultValue", 2, set_default_value);
        install_function!("setDefaultValues", 1, set_default_values);
        install_function!("getDefaultValue", 1, get_default_value);
        install_function!("resetDefaultValues", 0, reset_default_values);
        install_function!("setVersionDefaults", 1, set_version_defaults);
    }

    for name in ["theme", "fontFamily", "fontSize"] {
        let current: rquickjs::Value = globals.get(name).map_err(|error| error.to_string())?;
        if current.as_function().is_none() {
            let style = Function::new(
                ctx.clone(),
                |ctx: Ctx<'js>, args: Rest<rquickjs::Value<'js>>| {
                    let value = common::argument(&ctx, &args.0, 0);
                    let pattern = common::argument(&ctx, &args.0, 1);
                    Ok::<_, rquickjs::Error>(if pattern.is_null() || pattern.is_undefined() {
                        value
                    } else {
                        pattern
                    })
                },
            )
            .map_err(|error| error.to_string())?;
            configure_function(&style, "", 2, false).map_err(|error| error.to_string())?;
            globals
                .set(name, style)
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}
