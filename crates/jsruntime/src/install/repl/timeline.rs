use super::*;

fn apply<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
    arguments: rquickjs::function::Rest<rquickjs::Value<'js>>,
    state: &rustel_core::TimelineState,
) -> rquickjs::Result<rquickjs::Class<'js, NativePatternWrapper<'js>>> {
    let (pattern, pattern_sidecar) = {
        let wrapper = this.0.borrow();
        (wrapper.pattern.clone(), Sidecar::of(&wrapper))
    };
    let mut sidecars = vec![pattern_sidecar];
    let time_pattern = if arguments.0.len() == 1 {
        let (time_pattern, sidecar) = reify_bridged(&ctx, &arguments.0[0])?;
        sidecars.push(sidecar);
        time_pattern
    } else {
        let mut patterns = Vec::with_capacity(arguments.0.len());
        for argument in &arguments.0 {
            let (pattern, nested) = reify_list_element_bridged(&ctx, argument)?;
            patterns.push(pattern);
            sidecars.extend(nested);
        }
        rustel_core::fastcat(patterns)
    };
    derive_wrapper(
        ctx,
        rustel_core::timeline(time_pattern, pattern, state.clone()),
        &sidecars,
    )
}

fn raw_apply<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Value<'js>>,
    arguments: rquickjs::function::Rest<rquickjs::Value<'js>>,
    state: &rustel_core::TimelineState,
) -> rquickjs::Result<rquickjs::Class<'js, NativePatternWrapper<'js>>> {
    let time_value = arguments
        .0
        .first()
        .cloned()
        .unwrap_or_else(|| this.0.clone());
    let pattern_value = if arguments.0.is_empty() {
        common::undefined(&ctx)
    } else {
        arguments.0.get(1).cloned().unwrap_or(this.0)
    };
    let (time_pattern, time_sidecar) = reify_bridged(&ctx, &time_value)?;
    let (pattern, pattern_sidecar, direct_throw) = match unwrap_pattern(&pattern_value) {
        Some((pattern, sidecar)) => (pattern, sidecar, false),
        None => {
            let message = if pattern_value.is_undefined() {
                "Cannot read properties of undefined (reading 'late')"
            } else if pattern_value.is_null() {
                "Cannot read properties of null (reading 'late')"
            } else {
                "pat.late is not a function"
            };
            (
                rustel_core::query_error_pattern(message),
                Sidecar::default(),
                true,
            )
        }
    };
    let wrapper = derive_wrapper(
        ctx,
        rustel_core::timeline(time_pattern, pattern, state.clone()),
        &[pattern_sidecar, time_sidecar],
    )?;
    if direct_throw {
        let query = Function::new(
            wrapper.ctx().clone(),
            hr_this_rest_to_value(native_polymeter_zero_query_method),
        )?;
        set_function_length(&query, 1)?;
        query.set_name("query")?;
        let object = wrapper
            .clone()
            .into_value()
            .into_object()
            .expect("Pattern wrapper");
        query.set(NATIVE_QUERY_MARKER, object.clone())?;
        wrapper.borrow_mut().native_query = Some(query.clone());
        object.set("query", query)?;
    }
    Ok(wrapper)
}

fn method<'js>(
    ctx: &Ctx<'js>,
    state: rustel_core::TimelineState,
) -> rquickjs::Result<Function<'js>> {
    let function = Function::new(
        ctx.clone(),
        hr_this_rest(move |ctx, this, arguments| apply(ctx, this, arguments, &state)),
    )?;
    configure_function(&function, "", 0, true)?;
    Ok(function)
}

fn raw_method<'js>(
    ctx: &Ctx<'js>,
    state: rustel_core::TimelineState,
) -> rquickjs::Result<Function<'js>> {
    let function = Function::new(
        ctx.clone(),
        hr_thisval_rest(move |ctx, this, arguments| raw_apply(ctx, this, arguments, &state)),
    )?;
    configure_function(&function, "", 0, true)?;
    Ok(function)
}

pub(super) fn install<'js>(
    runtime: &JsRuntime,
    ctx: &Ctx<'js>,
    globals: &rquickjs::Object<'js>,
) -> Result<(), String> {
    let proto = rquickjs::Class::<NativePatternWrapper>::prototype(ctx)
        .map_err(|error| error.to_string())?
        .ok_or("NativePatternWrapper has no prototype")?;
    proto
        .set(
            "timeline",
            method(ctx, runtime.timeline_state.clone()).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    proto
        .set(
            "_timeline",
            raw_method(ctx, runtime.timeline_state.clone()).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    let scope: rquickjs::Object = globals
        .get("rustelScope")
        .map_err(|error| error.to_string())?;
    scope.remove("timeline").map_err(|error| error.to_string())
}
