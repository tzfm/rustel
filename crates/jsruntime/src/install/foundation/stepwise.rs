use super::*;
use rquickjs::{
    Coerced,
    function::{Constructor, Rest, This},
};

const CANONICAL_STEPS_GETTER: &str = "canonicalStepsGetter";
const CANONICAL_STEPS_SETTER: &str = "canonicalStepsSetter";
const CANONICAL_HAS_STEPS_GETTER: &str = "canonicalHasStepsGetter";
const CANONICAL_SHRINKLIST: &str = "canonicalShrinklist";
const CANONICAL_SHRINK_TOKEN: &str = "canonicalShrinkToken";
const CANONICAL_SHRINKLISTS: &str = "canonicalShrinklists";
const WEAKMAP_GET: &str = "weakMapGet";
const WEAKMAP_SET: &str = "weakMapSet";
const OWN_DESCRIPTOR: &str = "getOwnPropertyDescriptor";
const GET_PROTOTYPE: &str = "getPrototypeOf";
const SAFE_INTEGER: &str = "isSafeInteger";
const NUMBER: &str = "Number";
const ARRAY_IS_ARRAY: &str = "arrayIsArray";
const ARRAY_PUSH: &str = "arrayPush";
const ARRAY_MAP: &str = "arrayMap";
const ARRAY_REVERSE: &str = "arrayReverse";
const ARRAY_REDUCE: &str = "arrayReduce";
const ARRAY_ITERATOR: &str = "arrayIterator";
const FRACTION_VALUE_OF: &str = "fractionValueOf";
const FRACTION_TO_FRACTION: &str = "fractionToFraction";
const CANONICAL_TAKE: &str = "canonicalTake";
const FINALIZE_REGISTERED_PURE: &str = "finalizeRegisteredPure";
const MAP_PATTERN: &str = "shrinkMapPattern";
const MAP_CANONICAL: &str = "shrinkMapCanonical";

fn undefined<'js>(ctx: &Ctx<'js>) -> rquickjs::Value<'js> {
    rquickjs::Value::new_undefined(ctx.clone())
}

fn array_prototype<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<rquickjs::Object<'js>> {
    let array: Function = ctx.globals().get("Array")?;
    array.get("prototype")
}

fn captured_is_array<'js>(ctx: &Ctx<'js>, value: rquickjs::Value<'js>) -> rquickjs::Result<bool> {
    let function: Function = values::state(ctx)?.get(ARRAY_IS_ARRAY)?;
    function.call((value,))
}

fn weak_get<'js>(
    ctx: &Ctx<'js>,
    key: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let state = values::state(ctx)?;
    let map: rquickjs::Value = state.get(CANONICAL_SHRINKLISTS)?;
    let get: Function = state.get(WEAKMAP_GET)?;
    values::call_with_this(&get, map, [key])
}

fn weak_set<'js>(
    ctx: &Ctx<'js>,
    key: rquickjs::Value<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<()> {
    let state = values::state(ctx)?;
    let map: rquickjs::Value = state.get(CANONICAL_SHRINKLISTS)?;
    let set: Function = state.get(WEAKMAP_SET)?;
    values::call_with_this(&set, map, [key, value])?;
    Ok(())
}

fn own_descriptor<'js>(
    ctx: &Ctx<'js>,
    target: rquickjs::Value<'js>,
    name: &str,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let function: Function = values::state(ctx)?.get(OWN_DESCRIPTOR)?;
    function.call((target, name))
}

fn descriptor_for<'js>(
    ctx: &Ctx<'js>,
    target: rquickjs::Value<'js>,
    name: &str,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let state = values::state(ctx)?;
    let get_prototype: Function = state.get(GET_PROTOTYPE)?;
    let mut cursor = target;
    while !cursor.is_null() {
        let descriptor = own_descriptor(ctx, cursor.clone(), name)?;
        if !descriptor.is_undefined() {
            return Ok(descriptor);
        }
        cursor = get_prototype.call((cursor,))?;
    }
    Ok(undefined(ctx))
}

fn descriptor_field<'js>(
    ctx: &Ctx<'js>,
    descriptor: rquickjs::Value<'js>,
    name: &str,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    if descriptor.is_undefined() {
        Ok(undefined(ctx))
    } else {
        values::get_property(ctx, descriptor, name)
    }
}

fn has_canonical_pattern_surface<'js>(
    ctx: &Ctx<'js>,
    pat: rquickjs::Value<'js>,
) -> rquickjs::Result<bool> {
    let state = values::state(ctx)?;
    let stored_steps = own_descriptor(ctx, pat.clone(), "__steps")?;
    if stored_steps.is_undefined() {
        return Ok(false);
    }
    let stored_descriptor = values::object_for_property(ctx, stored_steps)?;
    if !stored_descriptor.contains_key("value")? {
        return Ok(false);
    }
    let steps = descriptor_for(ctx, pat.clone(), "_steps")?;
    let has_steps = descriptor_for(ctx, pat.clone(), "hasSteps")?;
    let zoom = descriptor_for(ctx, pat, "zoom")?;
    let steps_get = descriptor_field(ctx, steps.clone(), "get")?;
    let steps_set = descriptor_field(ctx, steps, "set")?;
    let has_steps_get = descriptor_field(ctx, has_steps, "get")?;
    let zoom_object = if zoom.is_undefined() {
        return Ok(false);
    } else {
        values::object_for_property(ctx, zoom)?
    };
    if !zoom_object.contains_key("value")?
        || !values::strict_equal(
            ctx,
            &steps_get,
            &state.get::<_, rquickjs::Value>(CANONICAL_STEPS_GETTER)?,
        )
        || !values::strict_equal(
            ctx,
            &steps_set,
            &state.get::<_, rquickjs::Value>(CANONICAL_STEPS_SETTER)?,
        )
        || !values::strict_equal(
            ctx,
            &has_steps_get,
            &state.get::<_, rquickjs::Value>(CANONICAL_HAS_STEPS_GETTER)?,
        )
        || !values::strict_equal(
            ctx,
            &zoom_object.get::<_, rquickjs::Value>("value")?,
            &state.get::<_, rquickjs::Value>(lists::CANONICAL_ZOOM)?,
        )
    {
        return Ok(false);
    }
    let stored_value: rquickjs::Value = stored_descriptor.get("value")?;
    if !stored_value.is_object() && !stored_value.is_function() {
        return Ok(false);
    }
    let value_of = descriptor_for(ctx, stored_value, "valueOf")?;
    if value_of.is_undefined() {
        return Ok(false);
    }
    let value_of = values::object_for_property(ctx, value_of)?;
    Ok(value_of.contains_key("value")?
        && values::strict_equal(
            ctx,
            &value_of.get::<_, rquickjs::Value>("value")?,
            &state.get::<_, rquickjs::Value>(FRACTION_VALUE_OF)?,
        ))
}

fn has_canonical_array_primitives<'js>(ctx: &Ctx<'js>, grow: bool) -> rquickjs::Result<bool> {
    let state = values::state(ctx)?;
    let prototype = array_prototype(ctx)?;
    for (property, saved) in [
        ("push", ARRAY_PUSH),
        ("map", ARRAY_MAP),
        ("reduce", ARRAY_REDUCE),
    ] {
        let current: rquickjs::Value = prototype.get(property)?;
        let saved: rquickjs::Value = state.get(saved)?;
        if !values::strict_equal(ctx, &current, &saved) {
            return Ok(false);
        }
    }
    let iterator: rquickjs::Value = prototype.get(rquickjs::Symbol::iterator(ctx.clone()))?;
    if !values::strict_equal(
        ctx,
        &iterator,
        &state.get::<_, rquickjs::Value>(ARRAY_ITERATOR)?,
    ) {
        return Ok(false);
    }
    if grow {
        let reverse: rquickjs::Value = prototype.get("reverse")?;
        if !values::strict_equal(
            ctx,
            &reverse,
            &state.get::<_, rquickjs::Value>(ARRAY_REVERSE)?,
        ) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn fraction_string<'js>(
    ctx: &Ctx<'js>,
    fraction: rquickjs::Value<'js>,
) -> rquickjs::Result<String> {
    let function: Function = values::state(ctx)?.get(FRACTION_TO_FRACTION)?;
    let result = values::call_with_this(&function, fraction, [])?;
    Ok(Coerced::<String>::from_js(ctx, result)?.0)
}

fn metadata<'js>(
    ctx: &Ctx<'js>,
    token: rquickjs::Value<'js>,
    charged: Option<u64>,
    refusal: Option<rquickjs::Value<'js>>,
    fast: Option<(String, bool)>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let metadata = rquickjs::Object::new(ctx.clone())?;
    metadata.set("token", token)?;
    if let Some(charged) = charged {
        metadata.set("charged", charged)?;
    }
    if let Some(refusal) = refusal {
        metadata.set("refusal", refusal)?;
    }
    if let Some((amount, grow)) = fast {
        let value = rquickjs::Object::new(ctx.clone())?;
        value.set("amount", amount)?;
        value.set("grow", grow)?;
        metadata.set("fast", value)?;
    }
    Ok(metadata.into_value())
}

fn pattern_class<'js>(
    ctx: &Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Class<'js, NativePatternWrapper<'js>>> {
    value
        .as_object()
        .and_then(rquickjs::Class::<NativePatternWrapper>::from_object)
        .ok_or_else(|| throw_type_error(ctx, "shrink/grow receiver is not a Pattern"))
}

fn shrink_map_callback<'js>(
    ctx: Ctx<'js>,
    range: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let state = values::state(&ctx)?;
    let pat: rquickjs::Value = state.get(MAP_PATTERN)?;
    let canonical: bool = state.get(MAP_CANONICAL)?;
    let range = values::object_for_property(&ctx, range)?;
    let start: rquickjs::Value = range.get(0u32)?;
    let end: rquickjs::Value = range.get(1u32)?;
    let zoom = values::get_property(&ctx, pat.clone(), "zoom")?;
    let canonical_zoom: rquickjs::Value = state.get(lists::CANONICAL_ZOOM)?;
    if !values::strict_equal(&ctx, &zoom, &canonical_zoom) {
        let zoom = zoom
            .into_function()
            .ok_or_else(|| throw_type_error(&ctx, "pat.zoom is not a function"))?;
        return values::call_with_this(&zoom, pat, [start, end]);
    }
    let start = fraction_string(&ctx, values::exact_fraction(&ctx, start)?)?;
    let end = fraction_string(&ctx, values::exact_fraction(&ctx, end)?)?;
    native_shrinklist_zoom(
        ctx.clone(),
        pattern_class(&ctx, pat)?,
        start,
        end,
        canonical,
    )
    .map(rquickjs::Class::into_value)
}

fn map_ranges<'js>(
    ctx: &Ctx<'js>,
    ranges: rquickjs::Array<'js>,
    pat: rquickjs::Value<'js>,
    canonical: bool,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let state = values::state(ctx)?;
    let previous_pat: rquickjs::Value = state.get(MAP_PATTERN)?;
    let previous_canonical: rquickjs::Value = state.get(MAP_CANONICAL)?;
    state.set(MAP_PATTERN, pat)?;
    state.set(MAP_CANONICAL, canonical)?;
    let callback = Function::new(ctx.clone(), shrink_map_callback)?;
    configure_function(&callback, "", 1, false)?;
    let result = values::call_method(ctx, ranges.into_value(), "map", [callback.into_value()]);
    let restore_pat = state.set(MAP_PATTERN, previous_pat);
    let restore_canonical = state.set(MAP_CANONICAL, previous_canonical);
    restore_pat?;
    restore_canonical?;
    result
}

fn shrinklist<'js>(
    ctx: Ctx<'js>,
    this: This<rquickjs::Value<'js>>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let pat = this.0;
    let has_steps = values::get_property(&ctx, pat.clone(), "hasSteps")?;
    if !values::truthy(&ctx, has_steps)? {
        return Ok(values::array(&ctx, [pat])?.into_value());
    }

    let amount = values::argument(&ctx, &args.0, 0);
    let pair = values::dynamic_is_array(&ctx, amount.clone())?;
    let (amount_value, times) = if pair {
        let values = values::spread(&ctx, amount)?;
        (values.get(0usize)?, values.get(1usize)?)
    } else {
        (amount, values::get_property(&ctx, pat.clone(), "_steps")?)
    };
    let mut amount_value = values::exact_fraction(&ctx, amount_value)?;
    if times.is_number() && times.as_number() == Some(0.0) {
        return Ok(values::array(&ctx, [pat])?.into_value());
    }

    let state = values::state(&ctx)?;
    let token: rquickjs::Value = state.get(CANONICAL_SHRINK_TOKEN)?;
    let canonical = !token.is_undefined();
    if canonical {
        let token_object = values::object_for_property(&ctx, token.clone())?;
        let direct: rquickjs::Value = token_object.get("direct")?;
        let grow: rquickjs::Value = token_object.get("grow")?;
        if values::truthy(&ctx, direct)?
            && has_canonical_pattern_surface(&ctx, pat.clone())?
            && has_canonical_array_primitives(&ctx, values::truthy(&ctx, grow.clone())?)?
        {
            let steps = values::get_property(&ctx, pat.clone(), "_steps")?;
            let number: Function = state.get(NUMBER)?;
            let numeric_steps: rquickjs::Value = number.call((steps.clone(),))?;
            let numeric_steps_number = numeric_steps.as_number().unwrap_or(f64::NAN);
            let amount_descriptor = descriptor_for(&ctx, amount_value.clone(), "toFraction")?;
            let amount_method = descriptor_field(&ctx, amount_descriptor, "value")?;
            let default_times = !pair
                || (times.is_number()
                    && {
                        let safe: Function = state.get(SAFE_INTEGER)?;
                        safe.call::<_, bool>((times.clone(),))?
                    }
                    && times.as_number() == Some(numeric_steps_number));
            let safe_steps: Function = state.get(SAFE_INTEGER)?;
            if default_times
                && safe_steps.call::<_, bool>((numeric_steps.clone(),))?
                && numeric_steps_number > 0.0
                && numeric_steps_number <= rustel_core::MAX_STEPWISE_ENTRIES as f64
                && values::strict_equal(
                    &ctx,
                    &amount_method,
                    &state.get::<_, rquickjs::Value>(FRACTION_TO_FRACTION)?,
                )
            {
                let result = rquickjs::Array::new(ctx.clone())?;
                let amount = fraction_string(&ctx, amount_value)?;
                let grow = values::truthy(&ctx, grow)?;
                weak_set(
                    &ctx,
                    result.clone().into_value(),
                    metadata(&ctx, token, None, None, Some((amount, grow)))?,
                )?;
                return Ok(result.into_value());
            }
        }
    }

    let from_start = Coerced::<f64>::from_js(&ctx, amount_value.clone())?.0 > 0.0;
    let ranges = rquickjs::Array::new(ctx.clone())?;
    let steps = values::get_property(&ctx, pat.clone(), "_steps")?;
    let mut range_count = 0usize;
    if from_start {
        let one = values::exact_fraction(&ctx, rquickjs::Value::new_int(ctx.clone(), 1))?;
        let seg = values::call_method(&ctx, one, "div", [steps])?;
        let seg = values::call_method(&ctx, seg, "mul", [amount_value.clone()])?;
        let mut index = 0usize;
        loop {
            let times_value = Coerced::<f64>::from_js(&ctx, times.clone())?.0;
            if (index as f64).partial_cmp(&times_value) != Some(std::cmp::Ordering::Less) {
                break;
            }
            let start = values::call_method(
                &ctx,
                seg.clone(),
                "mul",
                [rquickjs::Value::new_float(ctx.clone(), index as f64)],
            )?;
            if values::truthy(
                &ctx,
                values::call_method(
                    &ctx,
                    start.clone(),
                    "gt",
                    [rquickjs::Value::new_int(ctx.clone(), 1)],
                )?,
            )? {
                break;
            }
            ranges.set(
                range_count,
                values::array(&ctx, [start, rquickjs::Value::new_int(ctx.clone(), 1)])?,
            )?;
            range_count += 1;
            if range_count > rustel_core::MAX_STEPWISE_ENTRIES as usize {
                break;
            }
            index += 1;
        }
    } else {
        let zero = values::exact_fraction(&ctx, rquickjs::Value::new_int(ctx.clone(), 0))?;
        amount_value = values::call_method(&ctx, zero.clone(), "sub", [amount_value])?;
        let one = values::exact_fraction(&ctx, rquickjs::Value::new_int(ctx.clone(), 1))?;
        let seg = values::call_method(&ctx, one.clone(), "div", [steps])?;
        let seg = values::call_method(&ctx, seg, "mul", [amount_value])?;
        let mut index = 0usize;
        loop {
            let times_value = Coerced::<f64>::from_js(&ctx, times.clone())?.0;
            if (index as f64).partial_cmp(&times_value) != Some(std::cmp::Ordering::Less) {
                break;
            }
            let offset = values::call_method(
                &ctx,
                seg.clone(),
                "mul",
                [rquickjs::Value::new_float(ctx.clone(), index as f64)],
            )?;
            let end = values::call_method(&ctx, one.clone(), "sub", [offset])?;
            if values::truthy(
                &ctx,
                values::call_method(
                    &ctx,
                    end.clone(),
                    "lt",
                    [rquickjs::Value::new_int(ctx.clone(), 0)],
                )?,
            )? {
                break;
            }
            ranges.set(range_count, values::array(&ctx, [zero.clone(), end])?)?;
            range_count += 1;
            if range_count > rustel_core::MAX_STEPWISE_ENTRIES as usize {
                break;
            }
            index += 1;
        }
    }

    let refusal = native_shrinklist_charge(
        ctx.clone(),
        range_count as u64,
        range_count as u64,
        canonical,
    )?;
    if !refusal.is_undefined() {
        let result = values::array(&ctx, [refusal.clone()])?;
        if canonical {
            weak_set(
                &ctx,
                result.clone().into_value(),
                metadata(&ctx, token, Some(range_count as u64), Some(refusal), None)?,
            )?;
        }
        return Ok(result.into_value());
    }

    let result = map_ranges(&ctx, ranges, pat, canonical)?;
    let length: usize = values::get_property(&ctx, result.clone(), "length")?
        .as_number()
        .unwrap_or(0.0) as usize;
    native_shrinklist_materialised(length as u64);
    if canonical {
        weak_set(
            &ctx,
            result.clone(),
            metadata(&ctx, token, Some(range_count as u64), None, None)?,
        )?;
    }
    Ok(result)
}

fn free_shrinklist<'js>(
    ctx: Ctx<'js>,
    amount: rquickjs::Value<'js>,
    pat: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    values::call_method(&ctx, pat, "shrinklist", [amount])
}

fn growlist<'js>(
    ctx: Ctx<'js>,
    this: This<rquickjs::Value<'js>>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let result = values::call_method(
        &ctx,
        this.0,
        "shrinklist",
        [values::argument(&ctx, &args.0, 0)],
    )?;
    values::call_method(&ctx, result, "reverse", [])
}

fn free_growlist<'js>(
    ctx: Ctx<'js>,
    amount: rquickjs::Value<'js>,
    pat: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    values::call_method(&ctx, pat, "growlist", [amount])
}

fn preflight<'js>(
    ctx: &Ctx<'js>,
    list: rquickjs::Value<'js>,
    token: rquickjs::Value<'js>,
    pat: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let prior = weak_get(ctx, list.clone())?;
    if !prior.is_undefined() {
        let prior = values::object_for_property(ctx, prior.clone())?;
        let prior_token: rquickjs::Value = prior.get("token")?;
        if values::strict_equal(ctx, &prior_token, &token) {
            let fast: rquickjs::Value = prior.get("fast")?;
            if !fast.is_undefined() {
                let fast = values::object_for_property(ctx, fast)?;
                let amount: String = fast.get("amount")?;
                let grow: bool = fast.get("grow")?;
                return native_canonical_shrink_grow(
                    ctx.clone(),
                    pattern_class(ctx, pat)?,
                    amount,
                    grow,
                )
                .map(rquickjs::Class::into_value);
            }
        }
    }
    if !captured_is_array(ctx, list.clone())? {
        return Ok(undefined(ctx));
    }
    let length = values::get_property(ctx, list.clone(), "length")?
        .as_number()
        .unwrap_or(0.0) as u64;
    if !prior.is_undefined() {
        let prior = values::object_for_property(ctx, prior)?;
        let prior_token: rquickjs::Value = prior.get("token")?;
        if values::strict_equal(ctx, &prior_token, &token) {
            let refusal: rquickjs::Value = prior.get("refusal")?;
            if !refusal.is_undefined() {
                return Ok(refusal);
            }
            let charged = prior.get::<_, rquickjs::Value>("charged")?;
            let charged = charged.as_number().unwrap_or(0.0) as u64;
            if length <= charged {
                return Ok(undefined(ctx));
            }
            let added = length - charged;
            let refusal = native_shrinklist_charge(ctx.clone(), added, length, true)?;
            if !refusal.is_undefined() {
                return Ok(refusal);
            }
            native_shrinklist_materialised(added);
            return Ok(undefined(ctx));
        }
    }
    let refusal = native_shrinklist_charge(ctx.clone(), length, length, true)?;
    if !refusal.is_undefined() {
        return Ok(refusal);
    }
    native_shrinklist_materialised(length);
    Ok(undefined(ctx))
}

fn reduce_steps<'js>(
    ctx: &Ctx<'js>,
    list: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let reducer = Function::new(
        ctx.clone(),
        |ctx: Ctx<'js>, left: rquickjs::Value<'js>, right: rquickjs::Value<'js>| {
            let steps = values::get_property(&ctx, right, "_steps")?;
            values::call_method(&ctx, left, "add", [steps])
        },
    )?;
    configure_function(&reducer, "", 2, false)?;
    let zero = values::exact_fraction(ctx, rquickjs::Value::new_int(ctx.clone(), 0))?;
    values::call_method(ctx, list, "reduce", [reducer.into_value(), zero])
}

fn canonical_body<'js>(
    grow: bool,
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let amount = values::argument(&ctx, &args.0, 0);
    let pat = values::argument(&ctx, &args.0, 1);
    let has_steps = values::get_property(&ctx, pat.clone(), "hasSteps")?;
    if !values::truthy(&ctx, has_steps)? {
        return values::state(&ctx)?.get(lists::NOTHING);
    }
    let token = rquickjs::Object::new(ctx.clone())?;
    token.set("direct", false)?;
    token.set("grow", grow)?;
    let token_value = token.clone().into_value();
    let state = values::state(&ctx)?;
    let previous: rquickjs::Value = state.get(CANONICAL_SHRINK_TOKEN)?;
    state.set(CANONICAL_SHRINK_TOKEN, token_value.clone())?;
    let shrinklist = values::get_property(&ctx, pat.clone(), "shrinklist");
    let result = match shrinklist {
        Ok(shrinklist) => {
            let canonical: rquickjs::Value = state.get(CANONICAL_SHRINKLIST)?;
            token.set(
                "direct",
                values::strict_equal(&ctx, &shrinklist, &canonical),
            )?;
            let helper_amount = if grow {
                let zero = values::exact_fraction(&ctx, rquickjs::Value::new_int(ctx.clone(), 0))?;
                values::call_method(&ctx, zero, "sub", [amount])?
            } else {
                amount
            };
            let function = shrinklist
                .into_function()
                .ok_or_else(|| throw_type_error(&ctx, "pat.shrinklist is not a function"))?;
            values::call_with_this(&function, pat.clone(), [helper_amount])
        }
        Err(error) => Err(error),
    };
    let restore = state.set(CANONICAL_SHRINK_TOKEN, previous);
    restore?;
    let mut list = result?;
    let preflight = preflight(&ctx, list.clone(), token_value, pat.clone())?;
    if !preflight.is_undefined() {
        return Ok(preflight);
    }
    if grow {
        list = values::call_method(&ctx, list, "reverse", [])?;
    }
    // A host-filled spread copy is still a length claim (see
    // `js_array_len`), so it is copied out through the guard.
    let entries = values::spread(&ctx, list.clone())?;
    let stepcat: Function = state.get(lists::STEPCAT)?;
    let result: rquickjs::Value =
        stepcat.call((Rest(js_array_values::<rquickjs::Value>(&ctx, &entries)?),))?;
    let steps = reduce_steps(&ctx, list)?;
    values::object_for_property(&ctx, result.clone())?.set("_steps", steps)?;
    Ok(result)
}

fn join_step<'js>(
    ctx: Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    values::call_method(&ctx, value, "stepJoin", [])
}

fn body_swing<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    values::call_method(
        &ctx,
        values::argument(&ctx, &args.0, 1),
        "swingBy",
        [
            rquickjs::Value::new_float(ctx.clone(), 1.0 / 3.0),
            values::argument(&ctx, &args.0, 0),
        ],
    )
}

fn call_transform<'js>(
    ctx: &Ctx<'js>,
    function: rquickjs::Value<'js>,
    pat: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    function
        .into_function()
        .ok_or_else(|| throw_type_error(ctx, "func is not a function"))?
        .call((pat,))
}

fn body_apply<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    call_transform(
        &ctx,
        values::argument(&ctx, &args.0, 0),
        values::argument(&ctx, &args.0, 1),
    )
}

fn body_when<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let pat = values::argument(&ctx, &args.0, 2);
    if values::truthy(&ctx, values::argument(&ctx, &args.0, 0))? {
        call_transform(&ctx, values::argument(&ctx, &args.0, 1), pat)
    } else {
        Ok(pat)
    }
}

fn body_sometimes<'js>(
    probability: f64,
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    values::call_method(
        &ctx,
        values::argument(&ctx, &args.0, 1),
        "sometimesBy",
        [
            rquickjs::Value::new_float(ctx.clone(), probability),
            values::argument(&ctx, &args.0, 0),
        ],
    )
}

fn body_never<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    Ok(values::argument(&ctx, &args.0, 1))
}

fn body_range<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let min = values::argument(&ctx, &args.0, 0);
    let max = values::argument(&ctx, &args.0, 1);
    let pat = values::argument(&ctx, &args.0, 2);
    let mul = values::method(&ctx, &pat, "mul")?;
    let difference =
        Coerced::<f64>::from_js(&ctx, max)?.0 - Coerced::<f64>::from_js(&ctx, min.clone())?.0;
    let multiplied = values::call_with_this(
        &mul,
        pat,
        [rquickjs::Value::new_float(ctx.clone(), difference)],
    )?;
    values::call_method(&ctx, multiplied, "add", [min])
}

fn body_range2<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let bipolar = values::call_method(&ctx, values::argument(&ctx, &args.0, 2), "fromBipolar", [])?;
    values::call_method(
        &ctx,
        bipolar,
        "_range",
        [
            values::argument(&ctx, &args.0, 0),
            values::argument(&ctx, &args.0, 1),
        ],
    )
}

fn raw_take<'js>(
    ctx: &Ctx<'js>,
    amount: rquickjs::Value<'js>,
    pat: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let state = values::state(ctx)?;
    let nothing: rquickjs::Value = state.get(lists::NOTHING)?;
    let has_steps = values::get_property(ctx, pat.clone(), "hasSteps")?;
    if !values::truthy(ctx, has_steps)? {
        return Ok(nothing);
    }
    let steps = values::get_property(ctx, pat.clone(), "_steps")?;
    if values::truthy(
        ctx,
        values::call_method(
            ctx,
            steps.clone(),
            "lte",
            [rquickjs::Value::new_int(ctx.clone(), 0)],
        )?,
    )? {
        return Ok(nothing);
    }
    let mut amount = values::exact_fraction(ctx, amount)?;
    if values::truthy(
        ctx,
        values::call_method(
            ctx,
            amount.clone(),
            "eq",
            [rquickjs::Value::new_int(ctx.clone(), 0)],
        )?,
    )? {
        return Ok(nothing);
    }
    let flip = Coerced::<f64>::from_js(ctx, amount.clone())?.0 < 0.0;
    if flip {
        amount = values::call_method(ctx, amount, "abs", [])?;
    }
    let steps = values::get_property(ctx, pat.clone(), "_steps")?;
    let fraction = values::call_method(ctx, amount, "div", [steps])?;
    if values::truthy(
        ctx,
        values::call_method(
            ctx,
            fraction.clone(),
            "lte",
            [rquickjs::Value::new_int(ctx.clone(), 0)],
        )?,
    )? {
        return Ok(nothing);
    }
    if values::truthy(
        ctx,
        values::call_method(
            ctx,
            fraction.clone(),
            "gte",
            [rquickjs::Value::new_int(ctx.clone(), 1)],
        )?,
    )? {
        return Ok(pat);
    }
    let zoom = values::get_property(ctx, pat.clone(), "zoom")?;
    let canonical_zoom: rquickjs::Value = state.get(lists::CANONICAL_ZOOM)?;
    if flip {
        let one = values::exact_fraction(ctx, rquickjs::Value::new_int(ctx.clone(), 1))?;
        let begin = values::call_method(ctx, one, "sub", [fraction])?;
        if values::strict_equal(ctx, &zoom, &canonical_zoom) {
            return native_shrinklist_zoom(
                ctx.clone(),
                pattern_class(ctx, pat)?,
                fraction_string(ctx, begin)?,
                "1".into(),
                false,
            )
            .map(rquickjs::Class::into_value);
        }
        return values::call_with_this(
            &zoom
                .into_function()
                .ok_or_else(|| throw_type_error(ctx, "pat.zoom is not a function"))?,
            pat,
            [begin, rquickjs::Value::new_int(ctx.clone(), 1)],
        );
    }
    if values::strict_equal(ctx, &zoom, &canonical_zoom) {
        return native_shrinklist_zoom(
            ctx.clone(),
            pattern_class(ctx, pat)?,
            "0".into(),
            fraction_string(ctx, fraction)?,
            false,
        )
        .map(rquickjs::Class::into_value);
    }
    values::call_with_this(
        &zoom
            .into_function()
            .ok_or_else(|| throw_type_error(ctx, "pat.zoom is not a function"))?,
        pat,
        [rquickjs::Value::new_int(ctx.clone(), 0), fraction],
    )
}

fn body_take<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    raw_take(
        &ctx,
        values::argument(&ctx, &args.0, 0),
        values::argument(&ctx, &args.0, 1),
    )
}

fn body_drop<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let pat = values::argument(&ctx, &args.0, 1);
    let has_steps = values::get_property(&ctx, pat.clone(), "hasSteps")?;
    if !values::truthy(&ctx, has_steps)? {
        return values::state(&ctx)?.get(lists::NOTHING);
    }
    let amount = values::exact_fraction(&ctx, values::argument(&ctx, &args.0, 0))?;
    let negative = values::truthy(
        &ctx,
        values::call_method(
            &ctx,
            amount.clone(),
            "lt",
            [rquickjs::Value::new_int(ctx.clone(), 0)],
        )?,
    )?;
    let take = values::get_property(&ctx, pat.clone(), "take")?;
    let steps = values::get_property(&ctx, pat.clone(), "_steps")?;
    let helper_amount = if negative {
        values::call_method(&ctx, steps, "add", [amount])?
    } else {
        let difference = values::call_method(&ctx, steps, "sub", [amount])?;
        let zero = values::exact_fraction(&ctx, rquickjs::Value::new_int(ctx.clone(), 0))?;
        values::call_method(&ctx, zero, "sub", [difference])?
    };
    let state = values::state(&ctx)?;
    let canonical: rquickjs::Value = state.get(CANONICAL_TAKE)?;
    if values::strict_equal(&ctx, &take, &canonical) {
        let result = raw_take(&ctx, helper_amount, pat)?;
        let finalize: Function = state.get(FINALIZE_REGISTERED_PURE)?;
        return finalize.call((result,));
    }
    values::call_with_this(
        &take
            .into_function()
            .ok_or_else(|| throw_type_error(&ctx, "pat.take is not a function"))?,
        pat,
        [helper_amount],
    )
}

fn body_expand<'js>(
    divide: bool,
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let factor = values::argument(&ctx, &args.0, 0);
    let callback = Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, steps: rquickjs::Value<'js>| {
            let operation = values::method(&ctx, &steps, if divide { "div" } else { "mul" })?;
            let factor = values::exact_fraction(&ctx, factor.clone())?;
            values::call_with_this(&operation, steps, [factor])
        },
    )?;
    configure_function(&callback, "", 1, false)?;
    values::call_method(
        &ctx,
        values::argument(&ctx, &args.0, 1),
        "withSteps",
        [callback.into_value()],
    )
}

fn body_extend<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let factor = values::argument(&ctx, &args.0, 0);
    let fast = values::call_method(
        &ctx,
        values::argument(&ctx, &args.0, 1),
        "fast",
        [factor.clone()],
    )?;
    values::call_method(&ctx, fast, "expand", [factor])
}

fn body_replicate<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let factor = values::argument(&ctx, &args.0, 0);
    let repeated = values::call_method(
        &ctx,
        values::argument(&ctx, &args.0, 1),
        "repeatCycles",
        [factor.clone()],
    )?;
    let fast = values::call_method(&ctx, repeated, "fast", [factor.clone()])?;
    values::call_method(&ctx, fast, "expand", [factor])
}

fn body<'js>(
    _ctx: &Ctx<'js>,
    arity: usize,
    function: Function<'js>,
) -> rquickjs::Result<Function<'js>> {
    function.set_length(arity)?;
    Ok(function)
}

fn register<'js>(
    ctx: &Ctx<'js>,
    registrar: &Function<'js>,
    name: &str,
    body: Function<'js>,
    options: Vec<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let mut args = Vec::with_capacity(options.len() + 2);
    args.push(rquickjs::String::from_str(ctx.clone(), name)?.into_value());
    args.push(body.into_value());
    args.extend(options);
    values::call_with_this(registrar, undefined(ctx), args)
}

fn registered_options<'js>(ctx: &Ctx<'js>, join: &Function<'js>) -> Vec<rquickjs::Value<'js>> {
    vec![
        rquickjs::Value::new_bool(ctx.clone(), true),
        rquickjs::Value::new_bool(ctx.clone(), false),
        join.clone().into_value(),
    ]
}

fn install_registered<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let register_canonical = values::argument(&ctx, &args.0, 0)
        .into_function()
        .ok_or_else(|| throw_type_error(&ctx, "registerCanonical is not a function"))?;
    let register_raw = values::argument(&ctx, &args.0, 1)
        .into_function()
        .ok_or_else(|| throw_type_error(&ctx, "registerRawOnly is not a function"))?;
    let finalize = values::argument(&ctx, &args.0, 2)
        .into_function()
        .ok_or_else(|| throw_type_error(&ctx, "finalizeRegisteredPure is not a function"))?;
    let state = values::state(&ctx)?;
    state.set(FINALIZE_REGISTERED_PURE, finalize)?;
    let proto: rquickjs::Object = rquickjs::Class::<NativePatternWrapper>::prototype(&ctx)?
        .ok_or_else(|| throw_type_error(&ctx, "Pattern has no prototype"))?;

    let plain = [
        ("swing", 2, Function::new(ctx.clone(), body_swing)?),
        ("apply", 2, Function::new(ctx.clone(), body_apply)?),
        ("when", 3, Function::new(ctx.clone(), body_when)?),
        (
            "often",
            2,
            Function::new(ctx.clone(), |ctx, args| body_sometimes(0.75, ctx, args))?,
        ),
        (
            "rarely",
            2,
            Function::new(ctx.clone(), |ctx, args| body_sometimes(0.25, ctx, args))?,
        ),
        (
            "almostNever",
            2,
            Function::new(ctx.clone(), |ctx, args| body_sometimes(0.1, ctx, args))?,
        ),
        (
            "almostAlways",
            2,
            Function::new(ctx.clone(), |ctx, args| body_sometimes(0.9, ctx, args))?,
        ),
        ("never", 2, Function::new(ctx.clone(), body_never)?),
        ("always", 2, Function::new(ctx.clone(), body_apply)?),
    ];
    for (name, arity, function) in plain {
        register(
            &ctx,
            &register_raw,
            name,
            body(&ctx, arity, function)?,
            vec![],
        )?;
    }

    let join = Function::new(ctx.clone(), join_step)?;
    configure_function(&join, "", 1, false)?;
    for (name, arity, function) in [
        ("range", 3, Function::new(ctx.clone(), body_range)?),
        ("range2", 3, Function::new(ctx.clone(), body_range2)?),
    ] {
        register(
            &ctx,
            &register_raw,
            name,
            body(&ctx, arity, function)?,
            registered_options(&ctx, &join),
        )?;
    }

    state.set(CANONICAL_TAKE, proto.get::<_, rquickjs::Value>("take")?)?;
    register(
        &ctx,
        &register_raw,
        "take",
        body(&ctx, 2, Function::new(ctx.clone(), body_take)?)?,
        registered_options(&ctx, &join),
    )?;
    register(
        &ctx,
        &register_raw,
        "drop",
        body(&ctx, 2, Function::new(ctx.clone(), body_drop)?)?,
        registered_options(&ctx, &join),
    )?;
    for (name, function) in [
        (
            "expand",
            Function::new(ctx.clone(), |ctx, args| body_expand(false, ctx, args))?,
        ),
        ("extend", Function::new(ctx.clone(), body_extend)?),
        ("replicate", Function::new(ctx.clone(), body_replicate)?),
        (
            "contract",
            Function::new(ctx.clone(), |ctx, args| body_expand(true, ctx, args))?,
        ),
    ] {
        register(
            &ctx,
            &register_raw,
            name,
            body(&ctx, 2, function)?,
            registered_options(&ctx, &join),
        )?;
    }

    let steps = own_descriptor(&ctx, proto.clone().into_value(), "_steps")?;
    let has_steps = own_descriptor(&ctx, proto.clone().into_value(), "hasSteps")?;
    state.set(
        CANONICAL_STEPS_GETTER,
        descriptor_field(&ctx, steps.clone(), "get")?,
    )?;
    state.set(
        CANONICAL_STEPS_SETTER,
        descriptor_field(&ctx, steps, "set")?,
    )?;
    state.set(
        CANONICAL_HAS_STEPS_GETTER,
        descriptor_field(&ctx, has_steps, "get")?,
    )?;

    let shrink_body = body(
        &ctx,
        2,
        Function::new(ctx.clone(), |ctx, args| canonical_body(false, ctx, args))?,
    )?;
    let shrink = register(
        &ctx,
        &register_canonical,
        "shrink",
        shrink_body,
        registered_options(&ctx, &join),
    )?;
    let shrink_method: rquickjs::Value = proto.get("shrink")?;
    let grow_body = body(
        &ctx,
        2,
        Function::new(ctx.clone(), |ctx, args| canonical_body(true, ctx, args))?,
    )?;
    let grow = register(
        &ctx,
        &register_canonical,
        "grow",
        grow_body,
        registered_options(&ctx, &join),
    )?;
    let grow_method: rquickjs::Value = proto.get("grow")?;
    Ok(values::array(&ctx, [shrink, grow, shrink_method, grow_method])?.into_value())
}

fn capture<'js>(
    state: &rquickjs::Object<'js>,
    target: &rquickjs::Object<'js>,
    property: impl rquickjs::IntoAtom<'js>,
    name: &str,
) -> rquickjs::Result<()> {
    state.set(name, target.get::<_, rquickjs::Value>(property)?)
}

pub(super) fn install<'js>(
    ctx: &Ctx<'js>,
    globals: &rquickjs::Object<'js>,
    proto: &rquickjs::Object<'js>,
) -> rquickjs::Result<Function<'js>> {
    let state = values::state(ctx)?;
    let object: Function = globals.get("Object")?;
    capture(&state, &object, "getOwnPropertyDescriptor", OWN_DESCRIPTOR)?;
    capture(&state, &object, "getPrototypeOf", GET_PROTOTYPE)?;
    let number: Function = globals.get("Number")?;
    capture(&state, &number, "isSafeInteger", SAFE_INTEGER)?;
    state.set(NUMBER, number)?;
    let array: Function = globals.get("Array")?;
    capture(&state, &array, "isArray", ARRAY_IS_ARRAY)?;
    let array_prototype: rquickjs::Object = array.get("prototype")?;
    capture(&state, &array_prototype, "push", ARRAY_PUSH)?;
    capture(&state, &array_prototype, "map", ARRAY_MAP)?;
    capture(&state, &array_prototype, "reverse", ARRAY_REVERSE)?;
    capture(&state, &array_prototype, "reduce", ARRAY_REDUCE)?;
    capture(
        &state,
        &array_prototype,
        rquickjs::Symbol::iterator(ctx.clone()),
        ARRAY_ITERATOR,
    )?;
    let fraction: Function = globals.get("Fraction")?;
    let raw_fraction: Function = fraction.get("_original")?;
    let fraction_prototype: rquickjs::Object = raw_fraction.get("prototype")?;
    capture(&state, &fraction_prototype, "valueOf", FRACTION_VALUE_OF)?;
    capture(
        &state,
        &fraction_prototype,
        "toFraction",
        FRACTION_TO_FRACTION,
    )?;
    let weak_map: Constructor = globals.get("WeakMap")?;
    let weak_map_instance: rquickjs::Object = weak_map.construct(())?;
    let weak_map_prototype: rquickjs::Object = weak_map.get("prototype")?;
    capture(&state, &weak_map_prototype, "get", WEAKMAP_GET)?;
    capture(&state, &weak_map_prototype, "set", WEAKMAP_SET)?;
    state.set(CANONICAL_SHRINKLISTS, weak_map_instance)?;
    for name in [
        CANONICAL_STEPS_GETTER,
        CANONICAL_STEPS_SETTER,
        CANONICAL_HAS_STEPS_GETTER,
        CANONICAL_SHRINK_TOKEN,
        CANONICAL_TAKE,
        FINALIZE_REGISTERED_PURE,
        MAP_PATTERN,
        MAP_CANONICAL,
    ] {
        state.set(name, undefined(ctx))?;
    }

    let shrink_method = Function::new(ctx.clone(), shrinklist)?;
    configure_function(&shrink_method, "", 1, true)?;
    proto.set("shrinklist", shrink_method.clone())?;
    state.set(CANONICAL_SHRINKLIST, shrink_method)?;
    let shrink = Function::new(ctx.clone(), free_shrinklist)?;
    configure_function(&shrink, "shrinklist", 2, false)?;
    globals.set("shrinklist", shrink)?;

    let grow_method = Function::new(ctx.clone(), growlist)?;
    configure_function(&grow_method, "", 1, true)?;
    proto.set("growlist", grow_method)?;
    let grow = Function::new(ctx.clone(), free_growlist)?;
    configure_function(&grow, "growlist", 2, false)?;
    globals.set("growlist", grow)?;

    let installer = Function::new(ctx.clone(), install_registered)?;
    configure_function(&installer, "installCanonicalShrinkGrow", 3, false)?;
    Ok(installer)
}
