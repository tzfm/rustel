use super::*;
use rquickjs::{
    function::{Rest, This},
    object::Property,
};

pub(super) const CANONICAL_POLYMETER_PACE: &str = "canonicalPolymeterPace";
pub(super) const CANONICAL_ZOOM: &str = "canonicalZoom";
pub(super) const NOTHING: &str = "nothing";
pub(super) const SILENCE: &str = "silence";
pub(super) const STEPCAT: &str = "stepcat";

#[derive(Clone, Copy)]
enum CatKind {
    Slow,
    Fast,
}

#[derive(Clone, Copy)]
enum ListMethod {
    Stack,
    Sequence,
    Seq,
    Cat,
    FastCat,
    SlowCat,
}

fn wrapper_values<'js>(
    ctx: &Ctx<'js>,
    values: &[rquickjs::Value<'js>],
) -> rquickjs::Result<(Vec<Pattern>, Vec<Sidecar<'js>>)> {
    let mut patterns = Vec::with_capacity(values.len());
    let mut sidecars = Vec::with_capacity(values.len());
    for value in values {
        let Some((pattern, sidecar)) = unwrap_pattern(value) else {
            return Err(throw_type_error(
                ctx,
                "the configured string parser did not return a Pattern",
            ));
        };
        patterns.push(pattern);
        sidecars.push(sidecar);
    }
    Ok((patterns, sidecars))
}

fn finish_cat<'js>(
    ctx: Ctx<'js>,
    values: Vec<rquickjs::Value<'js>>,
    kind: CatKind,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    if values.len() == 1 {
        return Ok(values.into_iter().next().expect("one list value"));
    }
    let (patterns, sidecars) = wrapper_values(&ctx, &values)?;
    let pattern = match kind {
        CatKind::Slow => rustel_core::slowcat(patterns),
        CatKind::Fast if patterns.is_empty() => rustel_core::slowcat(patterns),
        CatKind::Fast => rustel_core::fastcat(patterns),
    };
    derive_wrapper(ctx, pattern, &sidecars).map(rquickjs::Class::into_value)
}

/// Flatten a list constructor's arguments, one nested array per level.
///
/// Every level's copy is kept alive until the whole tree is normalized, so
/// all of them charge ONE element budget, `remaining`, seeded once per
/// constructor call: a copy that fits the cap alone must not let a few
/// hundred nested levels each claim a heap ceiling of host memory.
fn normalize_cat<'js>(
    ctx: &Ctx<'js>,
    values: Vec<rquickjs::Value<'js>>,
    depth: usize,
    remaining: &Cell<usize>,
) -> rquickjs::Result<Vec<rquickjs::Value<'js>>> {
    if depth == MAX_LIST_DEPTH {
        return Err(rquickjs::Exception::throw_range(
            ctx,
            "nested list depth exceeds the native limit",
        ));
    }
    let stack = host_stack(ctx)?;
    let array_items: Function = stack.as_object().get(ARRAY_ITEMS)?;
    // One slot per value: a nested level's values were charged when its
    // parent copied them, and the top level's are the call's own arguments.
    let mut normalized = Vec::new();
    normalized
        .try_reserve_exact(values.len())
        .map_err(|_| host_reservation_refused(ctx, values.len()))?;
    for value in values {
        let items: rquickjs::Value = array_items.call((value.clone(),))?;
        if let Some(items) = items.as_array() {
            // A host-filled spread copy is still a length claim (see
            // `js_array_len`): copied out through the guard, charged to the
            // tree's budget.
            let nested = js_array_values_within(ctx, items, remaining)?;
            let nested = normalize_cat(ctx, nested, depth + 1, remaining)?;
            normalized.push(finish_cat(ctx.clone(), nested, CatKind::Fast)?);
        } else {
            normalized.push(reify_list_value(ctx.clone(), value)?);
        }
    }
    Ok(normalized)
}

/// The one element budget a list constructor's whole [`normalize_cat`] tree
/// draws on.
fn list_budget(ctx: &Ctx<'_>) -> rquickjs::Result<Cell<usize>> {
    Ok(Cell::new(js_element_cap::<rquickjs::Value>(ctx)?))
}

fn stack<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let normalized = normalize_cat(&ctx, args.0, 0, &list_budget(&ctx)?)?;
    let (patterns, sidecars) = wrapper_values(&ctx, &normalized)?;
    derive_wrapper(ctx, rustel_core::stack(patterns), &sidecars).map(rquickjs::Class::into_value)
}

fn cat<'js>(
    kind: CatKind,
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let normalized = normalize_cat(&ctx, args.0, 0, &list_budget(&ctx)?)?;
    finish_cat(ctx, normalized, kind)
}

fn stepcat<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let (items, sidecars) = stepcat_items_bridged(&ctx, &args.0)?;
    derive_wrapper(ctx, rustel_core::combinators::stepcat(&items), &sidecars)
        .map(rquickjs::Class::into_value)
}

fn tour<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let pat = values::argument(&ctx, &args.0, 0);
    let method = values::method(&ctx, &pat, "tour")?;
    values::call_with_this(&method, pat, args.0.into_iter().skip(1))
}

fn tour_method<'js>(
    ctx: Ctx<'js>,
    this: This<rquickjs::Value<'js>>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let mut values = Vec::with_capacity(args.0.len() + 1);
    values.push(this.0);
    values.extend(args.0);
    native_tour(ctx, Rest(values)).map(rquickjs::Class::into_value)
}

fn zip<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let mut retained = Vec::new();
    for value in args.0 {
        let has_steps = values::get_property(&ctx, value.clone(), "hasSteps")?;
        if values::truthy(&ctx, has_steps)? {
            retained.push(value);
        }
    }
    native_zip(ctx, Rest(retained)).map(rquickjs::Class::into_value)
}

fn stepalt<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let result = native_stepalt(ctx.clone(), args)?;
    if !result.is_undefined() {
        return Ok(result);
    }
    let nothing: rquickjs::Value = values::state(&ctx)?.get(NOTHING)?;
    values::object_for_property(&ctx, nothing.clone())?.set("_steps", 0)?;
    Ok(nothing)
}

fn polymeter<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    if let Some(first) = args.0.first()
        && values::dynamic_is_array(&ctx, first.clone())?
    {
        return native_polymeter_legacy(ctx, args);
    }

    let mut retained = Vec::new();
    let mut filtered = Vec::new();
    for value in args.0 {
        let has_steps = values::get_property(&ctx, value.clone(), "hasSteps")?;
        if values::truthy(&ctx, has_steps)? {
            retained.push(value);
        } else {
            filtered.push(value);
        }
    }

    let state = values::state(&ctx)?;
    let canonical_pace: rquickjs::Value = state.get(CANONICAL_POLYMETER_PACE)?;
    for value in &retained {
        let pace = values::get_property(&ctx, value.clone(), "pace")?;
        if !values::strict_equal(&ctx, &pace, &canonical_pace) {
            return Err(throw_type_error(
                &ctx,
                "polymeter native path does not support an overridden pace",
            ));
        }
    }

    let result = native_polymeter_modern(ctx.clone(), &retained, &filtered)?;
    if result.is_undefined() {
        state.get(SILENCE)
    } else if result.is_null() {
        state.get(NOTHING)
    } else {
        Ok(result)
    }
}

fn set_canonical_pace<'js>(ctx: Ctx<'js>, value: rquickjs::Value<'js>) -> rquickjs::Result<()> {
    let state = values::state(&ctx)?;
    let prior: rquickjs::Value = state.get(CANONICAL_POLYMETER_PACE)?;
    if !prior.is_undefined() {
        return Err(rquickjs::Exception::throw_message(
            &ctx,
            "canonical polymeter pace was already installed",
        ));
    }
    state.set(CANONICAL_POLYMETER_PACE, value)
}

fn set_canonical_zoom<'js>(ctx: Ctx<'js>, value: rquickjs::Value<'js>) -> rquickjs::Result<()> {
    let state = values::state(&ctx)?;
    let prior: rquickjs::Value = state.get(CANONICAL_ZOOM)?;
    if !prior.is_undefined() {
        return Err(rquickjs::Exception::throw_message(
            &ctx,
            "canonical zoom was already installed",
        ));
    }
    state.set(CANONICAL_ZOOM, value)
}

fn list_method<'js>(
    kind: ListMethod,
    ctx: Ctx<'js>,
    this: This<rquickjs::Value<'js>>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let mut values = Vec::with_capacity(args.0.len() + 1);
    values.push(this.0);
    values.extend(args.0);
    match kind {
        ListMethod::Stack => stack(ctx, Rest(values)),
        ListMethod::Sequence | ListMethod::Seq | ListMethod::FastCat => {
            cat(CatKind::Fast, ctx, Rest(values))
        }
        ListMethod::Cat | ListMethod::SlowCat => cat(CatKind::Slow, ctx, Rest(values)),
    }
}

fn ordinary<'js>(
    _ctx: &Ctx<'js>,
    name: &str,
    length: usize,
    function: Function<'js>,
) -> rquickjs::Result<Function<'js>> {
    configure_function(&function, name, length, true)?;
    Ok(function)
}

fn arrow<'js>(
    name: &str,
    length: usize,
    function: Function<'js>,
) -> rquickjs::Result<Function<'js>> {
    configure_function(&function, name, length, false)?;
    Ok(function)
}

fn define_list_method<'js>(
    proto: &rquickjs::Object<'js>,
    name: &str,
    kind: ListMethod,
) -> rquickjs::Result<()> {
    let function = Function::new(proto.ctx().clone(), move |ctx, this, args| {
        list_method(kind, ctx, this, args)
    })?;
    configure_function(&function, name, 0, false)?;
    proto.prop(name, Property::from(function).writable().configurable())
}

pub(super) struct InstalledLists<'js> {
    pub(super) set_canonical_polymeter_pace: Function<'js>,
    pub(super) set_canonical_zoom: Function<'js>,
    pub(super) install_canonical_shrink_grow: Function<'js>,
}

pub(super) fn install<'js>(
    ctx: &Ctx<'js>,
    globals: &rquickjs::Object<'js>,
    proto: &rquickjs::Object<'js>,
) -> Result<InstalledLists<'js>, String> {
    let state = values::state(ctx).map_err(|error| error.to_string())?;
    state
        .set(
            CANONICAL_POLYMETER_PACE,
            rquickjs::Value::new_undefined(ctx.clone()),
        )
        .map_err(|error| error.to_string())?;
    state
        .set(CANONICAL_ZOOM, rquickjs::Value::new_undefined(ctx.clone()))
        .map_err(|error| error.to_string())?;
    state
        .set(
            NOTHING,
            globals
                .get::<_, rquickjs::Value>("nothing")
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    state
        .set(
            SILENCE,
            globals
                .get::<_, rquickjs::Value>("silence")
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;

    let stack_fn = ordinary(
        ctx,
        "stack",
        0,
        Function::new(ctx.clone(), stack).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    globals
        .set("stack", stack_fn.clone())
        .map_err(|error| error.to_string())?;
    for alias in ["polyrhythm", "pr"] {
        globals
            .set(alias, stack_fn.clone())
            .map_err(|error| error.to_string())?;
    }

    for (name, kind) in [
        ("cat", CatKind::Slow),
        ("slowcat", CatKind::Slow),
        ("fastcat", CatKind::Fast),
        ("sequence", CatKind::Fast),
        ("seq", CatKind::Fast),
    ] {
        let function = ordinary(
            ctx,
            name,
            0,
            Function::new(ctx.clone(), move |ctx, args| cat(kind, ctx, args))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        globals
            .set(name, function)
            .map_err(|error| error.to_string())?;
    }

    let stepcat_fn = ordinary(
        ctx,
        "stepcat",
        0,
        Function::new(ctx.clone(), stepcat).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    globals
        .set("stepcat", stepcat_fn.clone())
        .map_err(|error| error.to_string())?;
    globals
        .set("timecat", stepcat_fn.clone())
        .map_err(|error| error.to_string())?;
    state
        .set(STEPCAT, stepcat_fn)
        .map_err(|error| error.to_string())?;

    for (name, length, function) in [
        (
            "tour",
            1,
            Function::new(ctx.clone(), tour).map_err(|error| error.to_string())?,
        ),
        (
            "zip",
            0,
            Function::new(ctx.clone(), zip).map_err(|error| error.to_string())?,
        ),
        (
            "stepalt",
            0,
            Function::new(ctx.clone(), stepalt).map_err(|error| error.to_string())?,
        ),
    ] {
        let function = ordinary(ctx, name, length, function).map_err(|error| error.to_string())?;
        globals
            .set(name, function)
            .map_err(|error| error.to_string())?;
    }
    let tour_method = Function::new(ctx.clone(), tour_method).map_err(|error| error.to_string())?;
    configure_function(&tour_method, "", 0, true).map_err(|error| error.to_string())?;
    proto
        .set("tour", tour_method)
        .map_err(|error| error.to_string())?;

    let polymeter = ordinary(
        ctx,
        "polymeter",
        0,
        Function::new(ctx.clone(), polymeter).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    globals
        .set("polymeter", polymeter.clone())
        .map_err(|error| error.to_string())?;
    globals
        .set("pm", polymeter)
        .map_err(|error| error.to_string())?;

    let install_canonical_shrink_grow = super::stepwise::install(ctx, globals, proto)
        .map_err(|error| describe_js_error(ctx, error))?;

    for (name, kind) in [
        ("stack", ListMethod::Stack),
        ("sequence", ListMethod::Sequence),
        ("seq", ListMethod::Seq),
        ("cat", ListMethod::Cat),
        ("fastcat", ListMethod::FastCat),
        ("slowcat", ListMethod::SlowCat),
    ] {
        define_list_method(proto, name, kind).map_err(|error| error.to_string())?;
    }

    let set_canonical_polymeter_pace = arrow(
        "setCanonicalPolymeterPace",
        1,
        Function::new(ctx.clone(), set_canonical_pace).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let set_canonical_zoom = arrow(
        "setCanonicalZoom",
        1,
        Function::new(ctx.clone(), set_canonical_zoom).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;

    Ok(InstalledLists {
        set_canonical_polymeter_pace,
        set_canonical_zoom,
        install_canonical_shrink_grow,
    })
}
