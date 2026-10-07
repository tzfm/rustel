use super::*;
use rquickjs::{
    IntoJs,
    class::{JsCell, JsClass, Readable},
    function::{Args, Params, Rest, This},
    object::Property,
};

#[derive(Trace, JsLifetime)]
struct NativePipeline<'js> {
    functions: Vec<Function<'js>>,
}

impl<'js> JsClass<'js> for NativePipeline<'js> {
    const NAME: &'static str = "NativePipeline";
    const KIND: rquickjs::class::ClassKind = rquickjs::class::ClassKind::Callable;
    type Mutable = Readable;

    fn prototype(ctx: &Ctx<'js>) -> rquickjs::Result<Option<rquickjs::Object<'js>>> {
        Ok(Some(Function::prototype(ctx.clone())))
    }

    fn constructor(
        _ctx: &Ctx<'js>,
    ) -> rquickjs::Result<Option<rquickjs::function::Constructor<'js>>> {
        Ok(None)
    }

    fn call<'a>(
        this: &JsCell<'js, Self>,
        params: Params<'a, 'js>,
    ) -> rquickjs::Result<rquickjs::Value<'js>> {
        let functions = this.borrow().functions.clone();
        let Some((last, rest)) = functions.split_last() else {
            return Ok(params
                .arg(0)
                .unwrap_or_else(|| rquickjs::Value::new_undefined(params.ctx().clone())));
        };
        let values = (0..params.len()).filter_map(|index| params.arg(index));
        let mut result = call_values(
            last,
            rquickjs::Value::new_undefined(params.ctx().clone()),
            values,
        )?;
        for function in rest.iter().rev() {
            result = function.call((result,))?;
        }
        Ok(result)
    }
}

#[derive(Trace, JsLifetime)]
struct NativeMappedArgs<'js> {
    function: Function<'js>,
    mapper: Function<'js>,
}

impl<'js> JsClass<'js> for NativeMappedArgs<'js> {
    const NAME: &'static str = "NativeMappedArgs";
    const KIND: rquickjs::class::ClassKind = rquickjs::class::ClassKind::Callable;
    type Mutable = Readable;

    fn prototype(ctx: &Ctx<'js>) -> rquickjs::Result<Option<rquickjs::Object<'js>>> {
        Ok(Some(Function::prototype(ctx.clone())))
    }

    fn constructor(
        _ctx: &Ctx<'js>,
    ) -> rquickjs::Result<Option<rquickjs::function::Constructor<'js>>> {
        Ok(None)
    }

    fn call<'a>(
        this: &JsCell<'js, Self>,
        params: Params<'a, 'js>,
    ) -> rquickjs::Result<rquickjs::Value<'js>> {
        let (function, mapper) = {
            let state = this.borrow();
            (state.function.clone(), state.mapper.clone())
        };
        let input = array(
            params.ctx(),
            (0..params.len()).filter_map(|index| params.arg(index)),
        )?;
        // Walk the count this host wrote, never `input.len()`: the copy is
        // host-filled, but its length is still a claim (see `js_array_len`).
        let count = params.len();
        let mut mapped = Vec::with_capacity(count);
        for index in 0..count {
            let value: rquickjs::Value = input.get(index)?;
            mapped.push(mapper.call((value, index, input.clone()))?);
        }
        call_values(
            &function,
            rquickjs::Value::new_undefined(params.ctx().clone()),
            mapped,
        )
    }
}

#[derive(Trace, JsLifetime)]
struct NativeCoreCurry<'js> {
    function: Function<'js>,
    overload: Option<Function<'js>>,
    collected: Vec<rquickjs::Value<'js>>,
    #[qjs(skip_trace)]
    arity: usize,
}

impl<'js> JsClass<'js> for NativeCoreCurry<'js> {
    const NAME: &'static str = "NativeCoreCurry";
    const KIND: rquickjs::class::ClassKind = rquickjs::class::ClassKind::Callable;
    type Mutable = Readable;

    fn prototype(ctx: &Ctx<'js>) -> rquickjs::Result<Option<rquickjs::Object<'js>>> {
        Ok(Some(Function::prototype(ctx.clone())))
    }

    fn constructor(
        _ctx: &Ctx<'js>,
    ) -> rquickjs::Result<Option<rquickjs::function::Constructor<'js>>> {
        Ok(None)
    }

    fn call<'a>(
        this: &JsCell<'js, Self>,
        params: Params<'a, 'js>,
    ) -> rquickjs::Result<rquickjs::Value<'js>> {
        let (function, overload, arity, mut collected) = {
            let state = this.borrow();
            (
                state.function.clone(),
                state.overload.clone(),
                state.arity,
                state.collected.clone(),
            )
        };
        collected.extend((0..params.len()).filter_map(|index| params.arg(index)));
        if collected.len() < arity {
            return build_curry(params.ctx(), function, overload, arity, collected);
        }
        call_values(&function, params.this(), collected)
    }
}

fn build_curry<'js>(
    ctx: &Ctx<'js>,
    function: Function<'js>,
    overload: Option<Function<'js>>,
    arity: usize,
    collected: Vec<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let callable = rquickjs::Class::instance(
        ctx.clone(),
        NativeCoreCurry {
            function,
            overload: overload.clone(),
            collected: collected.clone(),
            arity,
        },
    )?;
    let value = callable.into_value();
    if let Some(overload) = overload {
        let args = array(ctx, collected)?;
        let _: rquickjs::Value = overload.call((value.clone(), args))?;
    }
    Ok(value)
}

fn argument<'js>(
    ctx: &Ctx<'js>,
    args: &[rquickjs::Value<'js>],
    index: usize,
) -> rquickjs::Value<'js> {
    args.get(index)
        .cloned()
        .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()))
}

fn array<'js>(
    ctx: &Ctx<'js>,
    values: impl IntoIterator<Item = rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Array<'js>> {
    let result = rquickjs::Array::new(ctx.clone())?;
    for (index, value) in values.into_iter().enumerate() {
        result.set(index, value)?;
    }
    Ok(result)
}

fn expect_array<'js>(
    ctx: &Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Array<'js>> {
    value
        .into_array()
        .ok_or_else(|| rquickjs::Exception::throw_type(ctx, "value is not an array"))
}

fn call_values<'js>(
    function: &Function<'js>,
    this: rquickjs::Value<'js>,
    values: impl IntoIterator<Item = rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let values: Vec<_> = values.into_iter().collect();
    let mut args = Args::new(function.ctx().clone(), values.len());
    args.this(this)?;
    args.push_args(values)?;
    args.apply(function)
}

fn method<'js>(
    ctx: &Ctx<'js>,
    value: &rquickjs::Value<'js>,
    name: &str,
) -> rquickjs::Result<Function<'js>> {
    let object = if let Some(object) = value.as_object() {
        object.clone()
    } else {
        let constructor: Function = ctx.globals().get("Object")?;
        constructor.call((value.clone(),))?
    };
    object.get(name)
}

fn call_method<'js>(
    ctx: &Ctx<'js>,
    receiver: rquickjs::Value<'js>,
    name: &str,
    values: impl IntoIterator<Item = rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let function = method(ctx, &receiver, name)?;
    call_values(&function, receiver, values)
}

fn number<'js>(ctx: &Ctx<'js>, value: rquickjs::Value<'js>) -> rquickjs::Result<f64> {
    Ok(rquickjs::Coerced::<f64>::from_js(ctx, value)?.0)
}

fn string<'js>(ctx: &Ctx<'js>, value: rquickjs::Value<'js>) -> rquickjs::Result<String> {
    Ok(rquickjs::Coerced::<String>::from_js(ctx, value)?.0)
}

fn truthy<'js>(ctx: &Ctx<'js>, value: rquickjs::Value<'js>) -> rquickjs::Result<bool> {
    Ok(rquickjs::Coerced::<bool>::from_js(ctx, value)?.0)
}

fn loose_equal(
    ctx: &Ctx<'_>,
    left: &rquickjs::Value<'_>,
    right: &rquickjs::Value<'_>,
) -> rquickjs::Result<bool> {
    let result =
        unsafe { rquickjs::qjs::JS_IsEqual(ctx.as_raw().as_ptr(), left.as_raw(), right.as_raw()) };
    if result < 0 {
        Err(rquickjs::Error::Exception)
    } else {
        Ok(result != 0)
    }
}

fn set_if_missing<'js>(
    globals: &rquickjs::Object<'js>,
    name: &str,
    length: usize,
    function: Function<'js>,
) -> rquickjs::Result<()> {
    let current: rquickjs::Value = globals.get(name)?;
    if !current.is_undefined() {
        return Ok(());
    }
    configure_function(&function, name, length, false)?;
    globals.prop(
        name,
        Property::from(function)
            .writable()
            .configurable()
            .enumerable(),
    )
}

fn numeric_result<'js>(ctx: &Ctx<'js>, value: f64) -> rquickjs::Result<rquickjs::Value<'js>> {
    value.into_js(ctx)
}

fn modulo<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let n = number(&ctx, argument(&ctx, &args.0, 0))?;
    let m = number(&ctx, argument(&ctx, &args.0, 1))?;
    numeric_result(&ctx, ((n % m) + m) % m)
}

fn flatten<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let input = expect_array(&ctx, argument(&ctx, &args.0, 0))?;
    // Every length here is a claim (see `js_array_len`). The outer array and
    // each nested one are charged against ONE shared element budget, like
    // `materialize_js_value`'s tree: `flatten([new Array(N), new Array(N)])`
    // must refuse once, not grow an unbudgeted host Vec toward 2N slots.
    let len = js_array_len(&input)?;
    let remaining = Cell::new(js_element_cap::<rquickjs::Value>(&ctx)?);
    let mut output = reserve_js_elements(&ctx, len, &remaining)?;
    for index in 0..len {
        let item = input.get::<rquickjs::Value>(index)?;
        if let Some(nested) = item.as_array() {
            // A charged nested array lands straight in `output`, which is
            // grown fallibly to hold it AND every outer slot still to come,
            // so no push below grows the Vec on the infallible path.
            let nested_len = js_array_len(nested)?;
            charge_js_elements(&ctx, nested_len, &remaining)?;
            output
                .try_reserve(nested_len + (len - index - 1))
                .map_err(|_| host_reservation_refused(&ctx, nested_len))?;
            for nested_index in 0..nested_len {
                output.push(nested.get::<rquickjs::Value>(nested_index)?);
            }
        } else {
            output.push(item);
        }
    }
    Ok(array(&ctx, output)?.into_value())
}

fn clamp<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let value = number(&ctx, argument(&ctx, &args.0, 0))?;
    let min = number(&ctx, argument(&ctx, &args.0, 1))?;
    let max = number(&ctx, argument(&ctx, &args.0, 2))?;
    let result = if value.is_nan() || min.is_nan() || max.is_nan() {
        f64::NAN
    } else {
        value.max(min).min(max)
    };
    numeric_result(&ctx, result)
}

fn pipeline<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
    compose: bool,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let mut functions = Vec::with_capacity(args.0.len());
    for value in args.0 {
        functions.push(
            value
                .into_function()
                .ok_or_else(|| rquickjs::Exception::throw_type(&ctx, "value is not a function"))?,
        );
    }
    if compose {
        functions.reverse();
    }
    Ok(rquickjs::Class::instance(ctx, NativePipeline { functions })?.into_value())
}

fn curry<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let function = argument(&ctx, &args.0, 0)
        .into_function()
        .ok_or_else(|| rquickjs::Exception::throw_type(&ctx, "func is not a function"))?;
    let overload = args
        .0
        .get(1)
        .and_then(rquickjs::Value::as_function)
        .cloned();
    let arity = match args.0.get(2) {
        Some(value) if !value.is_undefined() => number(&ctx, value.clone())?,
        _ => number(&ctx, function.get("length")?)?,
    };
    build_curry(
        &ctx,
        function,
        overload,
        arity.max(0.0).trunc() as usize,
        Vec::new(),
    )
}

fn uniq<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let input = expect_array(&ctx, argument(&ctx, &args.0, 0))?;
    // The walk visits every claimed element (see `js_array_len`), so the
    // claim is charged up front; the output keeps only the distinct values,
    // so it grows with those rather than being reserved at the claim.
    let len = js_array_len(&input)?;
    charge_js_array::<rquickjs::Value>(&ctx, len)?;
    let mut keys = std::collections::HashSet::new();
    let mut output = Vec::new();
    for index in 0..len {
        let item = input.get::<rquickjs::Value>(index)?;
        if keys.insert(string(&ctx, item.clone())?) {
            output.push(item);
        }
    }
    Ok(array(&ctx, output)?.into_value())
}

fn uniqsort<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let input = expect_array(&ctx, argument(&ctx, &args.0, 0))?;
    let sorted = call_method(&ctx, input.clone().into_value(), "sort", std::iter::empty())?;
    let sorted = expect_array(&ctx, sorted)?;
    // `sort` is the receiver's own - possibly replaced - method, so the RESULT
    // is what gets guarded (see `js_array_len`). The dedup walks every
    // element, so the length is charged up front, and its output grows only
    // with the values it keeps.
    let len = js_array_len(&sorted)?;
    charge_js_array::<rquickjs::Value>(&ctx, len)?;
    let mut output = Vec::new();
    let mut previous: Option<rquickjs::Value> = None;
    for index in 0..len {
        let item = sorted.get::<rquickjs::Value>(index)?;
        let duplicate = previous
            .as_ref()
            .map(|prior| loose_equal(&ctx, prior, &item))
            .transpose()?
            .unwrap_or(false);
        if !duplicate {
            output.push(item.clone());
        }
        previous = Some(item);
    }
    Ok(array(&ctx, output)?.into_value())
}

fn rational_compare<'js>(
    ctx: Ctx<'js>,
    left: rquickjs::Value<'js>,
    right: rquickjs::Value<'js>,
) -> rquickjs::Result<f64> {
    let compared = call_method(&ctx, left, "compare", [right])?;
    number(&ctx, compared)
}

fn uniqsortr<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let input = expect_array(&ctx, argument(&ctx, &args.0, 0))?;
    let compare = Function::new(ctx.clone(), rational_compare)?;
    let sorted = call_method(&ctx, input.into_value(), "sort", [compare.into_value()])?;
    let sorted = expect_array(&ctx, sorted)?;
    // As `uniqsort`: the sorted result is guarded and charged before the
    // dedup walks it.
    let len = js_array_len(&sorted)?;
    charge_js_array::<rquickjs::Value>(&ctx, len)?;
    let mut output = Vec::new();
    let mut previous: Option<rquickjs::Value> = None;
    for index in 0..len {
        let item = sorted.get::<rquickjs::Value>(index)?;
        let duplicate = if let Some(prior) = &previous {
            !truthy(
                &ctx,
                call_method(&ctx, item.clone(), "ne", [prior.clone()])?,
            )?
        } else {
            false
        };
        if !duplicate {
            output.push(item.clone());
        }
        previous = Some(item);
    }
    Ok(array(&ctx, output)?.into_value())
}

fn rotate<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let receiver = argument(&ctx, &args.0, 0);
    let offset = argument(&ctx, &args.0, 1);
    let tail = call_method(&ctx, receiver.clone(), "slice", [offset.clone()])?;
    let head = call_method(&ctx, receiver, "slice", [0_i32.into_js(&ctx)?, offset])?;
    call_method(&ctx, tail, "concat", [head])
}

fn list_range<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let min = number(&ctx, argument(&ctx, &args.0, 0))?;
    let max = number(&ctx, argument(&ctx, &args.0, 1))?;
    let length = max - min + 1.0;
    if length.is_nan() || length <= 0.0 {
        return Ok(array(&ctx, std::iter::empty())?.into_value());
    }
    if !length.is_finite() || length > u32::MAX as f64 {
        return Err(rquickjs::Exception::throw_range(
            &ctx,
            "invalid array length",
        ));
    }
    // This length is arithmetic on numbers the score supplied, not a JS
    // array's `length` claim, so the array guards in this file do not cover
    // it. An unbounded `collect` would grow a host `Vec` of raw QuickJS
    // values that neither the QuickJS heap budget nor the interrupt deadline
    // sees: the loop allocates nothing inside QuickJS. So the numeric length
    // goes through the same element-cap reservation
    // (`js_element_cap::<rquickjs::Value>`, charged by `try_reserve_exact`).
    // A range past the ceiling, or a system refusal to reserve, comes back
    // as a catchable RangeError and never an abort.
    let len = length.trunc() as usize;
    let mut output = reserve_js_array::<rquickjs::Value>(&ctx, len)?;
    for index in 0..len {
        output.push((index as f64 + min).into_js(&ctx)?);
    }
    Ok(array(&ctx, output)?.into_value())
}

fn split_at<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let index = argument(&ctx, &args.0, 0);
    let value = argument(&ctx, &args.0, 1);
    let before = call_method(
        &ctx,
        value.clone(),
        "slice",
        [0_i32.into_js(&ctx)?, index.clone()],
    )?;
    let after = call_method(&ctx, value, "slice", [index])?;
    Ok(array(&ctx, [before, after])?.into_value())
}

fn zip_with<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let function = argument(&ctx, &args.0, 0)
        .into_function()
        .ok_or_else(|| rquickjs::Exception::throw_type(&ctx, "f is not a function"))?;
    let left = expect_array(&ctx, argument(&ctx, &args.0, 1))?;
    let right = expect_array(&ctx, argument(&ctx, &args.0, 2))?;
    let len = js_array_len(&left)?;
    let mut output = reserve_js_array(&ctx, len)?;
    for index in 0..len {
        output.push(function.call::<_, rquickjs::Value>((
            left.get::<rquickjs::Value>(index)?,
            right.get::<rquickjs::Value>(index)?,
        ))?);
    }
    Ok(array(&ctx, output)?.into_value())
}

fn pairs<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let input = expect_array(&ctx, argument(&ctx, &args.0, 0))?;
    let len = js_array_len(&input)?.saturating_sub(1);
    let mut output = reserve_js_array(&ctx, len)?;
    for index in 0..len {
        output.push(
            array(
                &ctx,
                [
                    input.get::<rquickjs::Value>(index)?,
                    input.get::<rquickjs::Value>(index + 1)?,
                ],
            )?
            .into_value(),
        );
    }
    Ok(array(&ctx, output)?.into_value())
}

fn remove_undefineds<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let input = expect_array(&ctx, argument(&ctx, &args.0, 0))?;
    // The filter walks every claimed element (see `js_array_len`), so the
    // claim is charged up front; the output grows only with the values it
    // keeps.
    let len = js_array_len(&input)?;
    charge_js_array::<rquickjs::Value>(&ctx, len)?;
    let mut output = Vec::new();
    for index in 0..len {
        let item = input.get::<rquickjs::Value>(index)?;
        if !item.is_null() && !item.is_undefined() {
            output.push(item);
        }
    }
    Ok(array(&ctx, output)?.into_value())
}

fn constant<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    Ok(argument(&ctx, &args.0, 0))
}

fn average_array<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let input = expect_array(&ctx, argument(&ctx, &args.0, 0))?;
    // The mean WALKS every element the length claims (see `js_array_len`),
    // so the claim is charged like a materialization and refused up front
    // instead of costing two billion FFI reads on the way to a NaN. Nothing
    // is kept, so nothing is reserved.
    let len = js_array_len(&input)?;
    charge_js_array::<rquickjs::Value>(&ctx, len)?;
    if len == 0 {
        return Err(rquickjs::Exception::throw_type(
            &ctx,
            "reduce of empty array with no initial value",
        ));
    }
    let mut sum = number(&ctx, input.get(0)?)?;
    for index in 1..len {
        sum += number(&ctx, input.get(index)?)?;
    }
    numeric_result(&ctx, sum / len as f64)
}

fn nan_fallback<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let value = argument(&ctx, &args.0, 0);
    if number(&ctx, value.clone())?.is_nan() {
        let fallback = match args.0.get(1) {
            Some(fallback) if !fallback.is_undefined() => fallback.clone(),
            _ => 0_i32.into_js(&ctx)?,
        };
        let logger: rquickjs::Value = ctx.globals().get("logger")?;
        if let Some(logger) = logger.as_function() {
            let message = format!(
                "\"{}\" is not a number, falling back to {}",
                string(&ctx, value)?,
                string(&ctx, fallback.clone())?
            );
            logger.call::<_, ()>((message, "warning"))?;
        }
        Ok(fallback)
    } else {
        Ok(value)
    }
}

fn object_map<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let input = argument(&ctx, &args.0, 0);
    let function = argument(&ctx, &args.0, 1)
        .into_function()
        .ok_or_else(|| rquickjs::Exception::throw_type(&ctx, "fn is not a function"))?;
    if let Some(input) = input.as_array() {
        let len = js_array_len(input)?;
        let mut output = reserve_js_array(&ctx, len)?;
        for index in 0..len {
            output.push(function.call::<_, rquickjs::Value>((
                input.get::<rquickjs::Value>(index)?,
                index,
                input.clone(),
            ))?);
        }
        return Ok(array(&ctx, output)?.into_value());
    }
    let object = input
        .into_object()
        .ok_or_else(|| rquickjs::Exception::throw_type(&ctx, "value is not an object"))?;
    let output = rquickjs::Object::new(ctx.clone())?;
    for (index, key) in object.keys::<String>().enumerate() {
        let key = key?;
        let value: rquickjs::Value = object.get(key.as_str())?;
        let mapped: rquickjs::Value = function.call((value, key.clone(), index))?;
        output.set(key, mapped)?;
    }
    Ok(output.into_value())
}

fn map_args<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let function = argument(&ctx, &args.0, 0)
        .into_function()
        .ok_or_else(|| rquickjs::Exception::throw_type(&ctx, "fn is not a function"))?;
    let mapper = argument(&ctx, &args.0, 1)
        .into_function()
        .ok_or_else(|| rquickjs::Exception::throw_type(&ctx, "mapFn is not a function"))?;
    Ok(rquickjs::Class::instance(ctx, NativeMappedArgs { function, mapper })?.into_value())
}

fn parse_numeral<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let value = argument(&ctx, &args.0, 0);
    let numeric = number(&ctx, value.clone())?;
    if !numeric.is_nan() {
        return numeric_result(&ctx, numeric);
    }
    let text = string(&ctx, value.clone())?;
    if rustel_core::util::is_note(&text) {
        return numeric_result(
            &ctx,
            rustel_core::util::note_to_midi(&text, 3)
                .map_err(|message| rquickjs::Exception::throw_message(&ctx, &message))?,
        );
    }
    Err(rquickjs::Exception::throw_message(
        &ctx,
        &format!("cannot parse as numeral: \"{text}\""),
    ))
}

fn parse_fractional<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let value = argument(&ctx, &args.0, 0);
    let numeric = number(&ctx, value.clone())?;
    if !numeric.is_nan() {
        return numeric_result(&ctx, numeric);
    }
    let text = string(&ctx, value)?;
    let special = match text.as_str() {
        "pi" => Some(std::f64::consts::PI),
        "w" => Some(1.0),
        "h" => Some(0.5),
        "q" => Some(0.25),
        "e" => Some(0.125),
        "s" => Some(0.0625),
        "t" => Some(1.0 / 3.0),
        "f" => Some(0.2),
        "x" => Some(1.0 / 6.0),
        _ => None,
    };
    if let Some(value) = special {
        numeric_result(&ctx, value)
    } else {
        Err(rquickjs::Exception::throw_message(
            &ctx,
            &format!("cannot parse as fractional: \"{text}\""),
        ))
    }
}

fn mapped_parser<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
    parser: fn(Ctx<'js>, Rest<rquickjs::Value<'js>>) -> rquickjs::Result<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let function = argument(&ctx, &args.0, 0)
        .into_function()
        .ok_or_else(|| rquickjs::Exception::throw_type(&ctx, "fn is not a function"))?;
    let mapper = Function::new(ctx.clone(), parser)?;
    Ok(rquickjs::Class::instance(ctx, NativeMappedArgs { function, mapper })?.into_value())
}

fn cycle_to_seconds<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let cycle = number(&ctx, argument(&ctx, &args.0, 0))?;
    let cps = number(&ctx, argument(&ctx, &args.0, 1))?;
    numeric_result(&ctx, cycle / cps)
}

fn get_sound_index<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let raw = argument(&ctx, &args.0, 0);
    let raw = if raw.is_null() || raw.is_undefined() {
        0_i32.into_js(&ctx)?
    } else {
        raw
    };
    let n = number(&ctx, raw)?;
    let n = if n.is_nan() { 0.0 } else { n };
    let n = (n + 0.5).floor();
    let count = number(&ctx, argument(&ctx, &args.0, 1))?;
    numeric_result(&ctx, ((n % count) + count) % count)
}

fn get_accidentals_offset<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let value = argument(&ctx, &args.0, 0);
    if value.is_null() || value.is_undefined() {
        return numeric_result(&ctx, 0.0);
    }
    let mut total = 0.0;
    for accidental in string(&ctx, value)?.chars() {
        total += match accidental {
            '#' | 's' => 1.0,
            'b' | 'f' => -1.0,
            _ => f64::NAN,
        };
    }
    numeric_result(&ctx, if total.is_nan() { 0.0 } else { total })
}

fn sol_to_note<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let n = number(&ctx, argument(&ctx, &args.0, 0))?;
    let notation = args
        .0
        .get(1)
        .filter(|value| !value.is_undefined())
        .cloned()
        .map(|value| string(&ctx, value))
        .transpose()?
        .unwrap_or_else(|| "letters".to_owned());
    let notes: &[&str] = match notation.as_str() {
        "solfeggio" => &[
            "Do", "Reb", "Re", "Mib", "Mi", "Fa", "Solb", "Sol", "Lab", "La", "Sib", "Si",
        ],
        "indian" => &["Sa", "Re", "Ga", "Ma", "Pa", "Dha", "Ni"],
        "german" => &[
            "C", "Db", "D", "Eb", "E", "F", "Gb", "G", "Ab", "A", "Hb", "H",
        ],
        "byzantine" => &[
            "Ni", "Pab", "Pa", "Voub", "Vou", "Ga", "Dib", "Di", "Keb", "Ke", "Zob", "Zo",
        ],
        "japanese" => &["I", "Ro", "Ha", "Ni", "Ho", "He", "To"],
        _ => &[
            "C", "Db", "D", "Eb", "E", "F", "Gb", "G", "Ab", "A", "Bb", "B",
        ],
    };
    let index = n % 12.0;
    let note = if index >= 0.0 && index.fract() == 0.0 {
        notes.get(index as usize).copied().unwrap_or("undefined")
    } else {
        "undefined"
    };
    format!("{note}{}", (n / 12.0).floor() - 1.0).into_js(&ctx)
}

fn get_event_offset_ms<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let target = number(&ctx, argument(&ctx, &args.0, 0))?;
    let current = number(&ctx, argument(&ctx, &args.0, 1))?;
    numeric_result(&ctx, (target - current) * 1000.0)
}

fn stringify_values<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let value = argument(&ctx, &args.0, 0);
    if !value.is_object() {
        return Ok(value);
    }
    let json: rquickjs::Object = ctx.globals().get("JSON")?;
    let stringify: Function = json.get("stringify")?;
    let rendered: rquickjs::Value = stringify.call((This(json), value))?;
    let compact = args
        .0
        .get(1)
        .is_some_and(|value| truthy(&ctx, value.clone()).unwrap_or(false));
    if !compact {
        return Ok(rendered);
    }
    let mut rendered = string(&ctx, rendered)?;
    if rendered.len() >= 2 {
        rendered = rendered[1..rendered.len() - 1]
            .replace('"', "")
            .replace(',', " ");
    }
    rendered.into_js(&ctx)
}

fn get_frequency<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let hap = argument(&ctx, &args.0, 0)
        .into_object()
        .ok_or_else(|| rquickjs::Exception::throw_type(&ctx, "hap is not an object"))?;
    let value: rquickjs::Value = hap.get("value")?;
    let context: rquickjs::Object = hap.get("context")?;
    let context_type: rquickjs::Value = context.get("type")?;
    let frequency = context_type
        .as_string()
        .is_some_and(|value| value.to_string().ok().as_deref() == Some("frequency"));
    if let Some(object) = value.as_object() {
        let freq: rquickjs::Value = object.get("freq")?;
        if truthy(&ctx, freq.clone())? {
            return Ok(freq);
        }
        for key in ["note", "n", "value"] {
            let candidate: rquickjs::Value = object.get(key)?;
            if !candidate.is_null() && !candidate.is_undefined() {
                let get_freq: Function = ctx.globals().get("getFreq")?;
                return get_freq.call((candidate,));
            }
        }
        let get_freq: Function = ctx.globals().get("getFreq")?;
        return get_freq.call((rquickjs::Value::new_undefined(ctx),));
    }
    if value.is_number() && !frequency {
        let midi_to_freq: Function = ctx.globals().get("midiToFreq")?;
        return midi_to_freq.call((value,));
    }
    if let Some(text) = value.as_string()
        && rustel_core::util::is_note(&text.to_string()?)
    {
        let midi = rustel_core::util::note_to_midi(&text.to_string()?, 3)
            .map_err(|message| rquickjs::Exception::throw_message(&ctx, &message))?;
        return numeric_result(&ctx, rustel_core::util::midi_to_freq(midi));
    }
    if !value.is_number() {
        return Err(rquickjs::Exception::throw_message(
            &ctx,
            &format!("not a note or frequency: {}", string(&ctx, value)?),
        ));
    }
    Ok(value)
}

fn get_playable_note_value<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let hap = argument(&ctx, &args.0, 0)
        .into_object()
        .ok_or_else(|| rquickjs::Exception::throw_type(&ctx, "hap is not an object"))?;
    let value: rquickjs::Value = hap.get("value")?;
    let context: rquickjs::Object = hap.get("context")?;
    let context_type: rquickjs::Value = context.get("type")?;
    let frequency = context_type
        .as_string()
        .is_some_and(|value| value.to_string().ok().as_deref() == Some("frequency"));
    let mut note = value.clone();
    if let Some(object) = note.as_object().cloned()
        && object.as_array().is_none()
    {
        note = rquickjs::Value::new_undefined(ctx.clone());
        for key in ["note", "n", "value"] {
            let candidate: rquickjs::Value = object.get(key)?;
            if truthy(&ctx, candidate.clone())? {
                note = candidate;
                break;
            }
        }
        if note.is_undefined() {
            return Err(rquickjs::Exception::throw_message(
                &ctx,
                "cannot find a playable note",
            ));
        }
    }
    if note.is_number() && !frequency {
        let midi_to_freq: Function = ctx.globals().get("midiToFreq")?;
        return midi_to_freq.call((value,));
    }
    if note.is_number() && frequency {
        return Ok(value);
    }
    if let Some(text) = note.as_string()
        && rustel_core::util::is_note(&text.to_string()?)
    {
        return Ok(note);
    }
    Err(rquickjs::Exception::throw_message(
        &ctx,
        &format!("not a note: {}", string(&ctx, note)?),
    ))
}

fn install_key_alias<'js>(ctx: &Ctx<'js>, globals: &rquickjs::Object<'js>) -> rquickjs::Result<()> {
    let current: rquickjs::Value = globals.get("keyAlias")?;
    if !current.is_undefined() {
        return Ok(());
    }
    let entries = array(
        ctx,
        [
            ("control", "Control"),
            ("ctrl", "Control"),
            ("alt", "Alt"),
            ("shift", "Shift"),
            ("down", "ArrowDown"),
            ("up", "ArrowUp"),
            ("left", "ArrowLeft"),
            ("right", "ArrowRight"),
        ]
        .into_iter()
        .map(|(key, value)| {
            array(ctx, [key.into_js(ctx)?, value.into_js(ctx)?]).map(rquickjs::Array::into_value)
        })
        .collect::<Result<Vec<_>, _>>()?,
    )?;
    let constructor: rquickjs::function::Constructor = globals.get("Map")?;
    let map: rquickjs::Value = constructor.construct((entries,))?;
    globals.set("keyAlias", map)
}

pub(super) fn install<'js>(ctx: &Ctx<'js>, globals: &rquickjs::Object<'js>) -> Result<(), String> {
    macro_rules! install {
        ($name:literal, $length:expr, $function:expr) => {
            set_if_missing(
                globals,
                $name,
                $length,
                Function::new(ctx.clone(), $function).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
        };
    }

    install!("_mod", 2, modulo);
    install!("flatten", 1, flatten);
    install!("clamp", 3, clamp);
    install!("compose", 0, |ctx, args| pipeline(ctx, args, true));
    install!("pipe", 0, |ctx, args| pipeline(ctx, args, false));
    install!("curry", 2, curry);
    install!("uniq", 1, uniq);
    install!("uniqsort", 1, uniqsort);
    install!("uniqsortr", 1, uniqsortr);
    install!("rotate", 2, rotate);
    install!("listRange", 2, list_range);
    install!("objectMap", 2, object_map);
    install!("zipWith", 3, zip_with);
    install!("splitAt", 2, split_at);
    install!("pairs", 1, pairs);
    install!("removeUndefineds", 1, remove_undefineds);
    install!("constant", 2, constant);
    install!("averageArray", 1, average_array);
    install!("nanFallback", 1, nan_fallback);
    install!("stringifyValues", 1, stringify_values);
    install!("mapArgs", 2, map_args);
    install!("numeralArgs", 1, |ctx, args| mapped_parser(
        ctx,
        args,
        parse_numeral
    ));
    install!("fractionalArgs", 1, |ctx, args| mapped_parser(
        ctx,
        args,
        parse_fractional
    ));
    install!("parseNumeral", 1, parse_numeral);
    install!("parseFractional", 1, parse_fractional);
    install!("cycleToSeconds", 2, cycle_to_seconds);
    install!("getSoundIndex", 2, get_sound_index);
    install!("getAccidentalsOffset", 1, get_accidentals_offset);
    install!("sol2note", 1, sol_to_note);
    install!("getEventOffsetMs", 2, get_event_offset_ms);
    install!("getFrequency", 1, get_frequency);
    install!("getPlayableNoteValue", 1, get_playable_note_value);
    install_key_alias(ctx, globals).map_err(|error| error.to_string())
}
