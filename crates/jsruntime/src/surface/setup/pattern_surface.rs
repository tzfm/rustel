use super::*;
use rquickjs::{
    class::{JsCell, JsClass, Readable},
    function::{Opt, Params, Rest, This},
};

const CONSTRUCT_IMPL: &str = "__rustel_construct_pattern";
const CONSTRUCT_IMPL_C: &[u8] = b"__rustel_construct_pattern\0";
const PATTERN_C: &[u8] = b"Pattern\0";

unsafe extern "C" fn construct_pattern(
    raw_ctx: *mut rquickjs::qjs::JSContext,
    new_target: rquickjs::qjs::JSValue,
    argc: rquickjs::qjs::c_int,
    argv: *mut rquickjs::qjs::JSValue,
) -> rquickjs::qjs::JSValue {
    if new_target.tag == i64::from(rquickjs::qjs::JS_TAG_UNDEFINED) {
        return unsafe {
            rquickjs::qjs::JS_ThrowTypeError(
                raw_ctx,
                c"Class constructor Pattern cannot be invoked without 'new'".as_ptr(),
            )
        };
    }
    let implementation = unsafe {
        rquickjs::qjs::JS_GetPropertyStr(raw_ctx, new_target, CONSTRUCT_IMPL_C.as_ptr().cast())
    };
    if implementation.tag == i64::from(rquickjs::qjs::JS_TAG_EXCEPTION) {
        return implementation;
    }
    let undefined = rquickjs::qjs::JSValue {
        u: rquickjs::qjs::JSValueUnion { int32: 0 },
        tag: i64::from(rquickjs::qjs::JS_TAG_UNDEFINED),
    };
    let result = unsafe { rquickjs::qjs::JS_Call(raw_ctx, implementation, undefined, argc, argv) };
    unsafe { rquickjs::qjs::JS_FreeValue(raw_ctx, implementation) };
    result
}

#[derive(Trace, JsLifetime)]
struct NativePatternDispatch<'js> {
    target: rquickjs::Class<'js, NativePatternWrapper<'js>>,
}

impl<'js> JsClass<'js> for NativePatternDispatch<'js> {
    const NAME: &'static str = "NativePatternDispatch";
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
        let target = this.borrow().target.clone().into_value();
        let query = common::method(params.ctx(), &target, "query")?;
        common::call(
            &query,
            target,
            (0..params.len()).filter_map(|index| params.arg(index)),
        )
    }
}

fn normalized_steps<'js>(
    ctx: &Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<(Option<Fraction>, rquickjs::Value<'js>)> {
    if value.is_undefined() {
        return Ok((None, value));
    }
    let stack = host_stack(ctx)?;
    let fraction: Function = stack.as_object().get(FRACTION_COERCE)?;
    let normalized: rquickjs::Value = fraction.call((value,))?;
    let number: Function = ctx.globals().get("Number")?;
    let numeric: f64 = number.call((normalized.clone(),))?;
    Ok((Fraction::from_f64(numeric), normalized))
}

fn pattern_implementation<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Class<'js, NativePatternWrapper<'js>>> {
    let query = common::argument(&ctx, &args.0, 0);
    let (steps, normalized) = normalized_steps(&ctx, common::argument(&ctx, &args.0, 1))?;
    let instance = new_wrapper(
        &ctx,
        NativePatternWrapper::plain(
            rustel_core::query_error_pattern("this.query is not a function").with_steps(steps),
        ),
    )?;
    let dispatch = rquickjs::Class::instance(
        ctx.clone(),
        NativePatternDispatch {
            target: instance.clone(),
        },
    )?
    .into_value()
    .into_function()
    .expect("a callable class is a function");
    common::configure(&dispatch, "dispatch", 1, false)?;
    let (id, sidecar) = bridge_callable(&ctx, dispatch)?;
    {
        let mut wrapper = instance.borrow_mut();
        wrapper.pattern = rustel_core::js_query(id).with_steps(steps);
        wrapper.native_query = None;
        wrapper.ids = sidecar.ids;
        wrapper.cells = sidecar.cells;
        wrapper.excluded_frame_ids = sidecar.excluded_frame_ids;
    }
    let object = instance
        .clone()
        .into_value()
        .into_object()
        .expect("Pattern object");
    object.set("query", query)?;
    object.set("__steps", normalized)?;
    Ok(instance)
}

fn native_constructor<'js>(
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
) -> rquickjs::Result<Function<'js>> {
    let implementation = Function::new(ctx.clone(), pattern_implementation)?;
    let raw = unsafe {
        rquickjs::qjs::JS_NewCFunction2(
            ctx.as_raw().as_ptr(),
            Some(construct_pattern),
            PATTERN_C.as_ptr().cast(),
            2,
            rquickjs::qjs::JSCFunctionEnum_JS_CFUNC_constructor_or_func,
            0,
        )
    };
    let pattern = Function::from_value(unsafe { rquickjs::Value::from_raw(ctx.clone(), raw) })?;
    pattern.as_inner().prop(
        CONSTRUCT_IMPL,
        rquickjs::object::Property::from(implementation),
    )?;
    unsafe {
        rquickjs::qjs::JS_SetConstructor(ctx.as_raw().as_ptr(), pattern.as_raw(), proto.as_raw())
    };
    Ok(pattern)
}

fn set_steps_value<'js>(
    ctx: &Ctx<'js>,
    target: rquickjs::Value<'js>,
    raw_steps: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let object = target
        .as_object()
        .ok_or_else(|| throw_type_error(ctx, "setSteps target is not a Pattern"))?;
    let wrapper = rquickjs::Class::<NativePatternWrapper>::from_object(object)
        .ok_or_else(|| throw_type_error(ctx, "setSteps target is not a Pattern"))?;
    let (steps, normalized) = normalized_steps(ctx, raw_steps)?;
    let pattern = wrapper.borrow().pattern.clone().with_steps(steps);
    wrapper.borrow_mut().pattern = pattern;
    object.set("__steps", normalized)?;
    Ok(target)
}

fn steps_getter<'js>(
    ctx: Ctx<'js>,
    This(target): This<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    common::get(&ctx, target, "__steps")
}

fn steps_setter<'js>(
    ctx: Ctx<'js>,
    This(target): This<rquickjs::Value<'js>>,
    steps: rquickjs::Value<'js>,
) -> rquickjs::Result<()> {
    set_steps_value(&ctx, target, steps)?;
    Ok(())
}

fn set_steps_method<'js>(
    ctx: Ctx<'js>,
    This(target): This<rquickjs::Value<'js>>,
    Opt(steps): Opt<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    set_steps_value(
        &ctx,
        target,
        steps.unwrap_or_else(|| common::undefined(&ctx)),
    )
}

fn has_steps_getter<'js>(
    ctx: Ctx<'js>,
    This(target): This<rquickjs::Value<'js>>,
) -> rquickjs::Result<bool> {
    Ok(!common::get(&ctx, target, "_steps")?.is_undefined())
}

fn native_query_source<'js>(
    ctx: &Ctx<'js>,
    target: &rquickjs::Value<'js>,
    query: &rquickjs::Value<'js>,
) -> rquickjs::Result<Option<rquickjs::Class<'js, NativePatternWrapper<'js>>>> {
    let Some(query_function) = query.as_function() else {
        return Ok(None);
    };
    if let Some(target_wrapper) = target
        .as_object()
        .and_then(rquickjs::Class::<NativePatternWrapper>::from_object)
    {
        let is_own = target_wrapper
            .borrow()
            .native_query
            .as_ref()
            .is_some_and(|native| same_js_value(ctx, query, native));
        if is_own {
            return Ok(Some(target_wrapper));
        }
    }
    let owner: rquickjs::Value = query_function.get(NATIVE_QUERY_MARKER)?;
    let Some(owner_wrapper) = owner
        .as_object()
        .and_then(rquickjs::Class::<NativePatternWrapper>::from_object)
    else {
        return Ok(None);
    };
    let is_owner = owner_wrapper
        .borrow()
        .native_query
        .as_ref()
        .is_some_and(|native| same_js_value(ctx, query, native));
    Ok(is_owner.then_some(owner_wrapper))
}

fn construct<'js>(
    constructor: &Function<'js>,
    query: rquickjs::Value<'js>,
    steps: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let constructor =
        rquickjs::function::Constructor::from_value(constructor.clone().into_value())?;
    constructor.construct((query, steps))
}

#[derive(Trace, JsLifetime)]
struct NativeWithSteps<'js> {
    constructor: Function<'js>,
}

impl<'js> JsClass<'js> for NativeWithSteps<'js> {
    const NAME: &'static str = "NativeWithSteps";
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
        let constructor = this.borrow().constructor.clone();
        let target = params.this();
        let query = common::get(params.ctx(), target.clone(), "query")?;
        let current = common::get(params.ctx(), target.clone(), "_steps")?;
        let steps = if current.is_undefined() {
            current
        } else {
            let current = common::get(params.ctx(), target.clone(), "_steps")?;
            let callback = params
                .arg(0)
                .and_then(rquickjs::Value::into_function)
                .ok_or_else(|| throw_type_error(params.ctx(), "func is not a function"))?;
            callback.call((current,))?
        };
        let Some(source) = native_query_source(params.ctx(), &target, &query)? else {
            return construct(&constructor, query, steps);
        };
        let (native_steps, normalized) = normalized_steps(params.ctx(), steps)?;
        let (pattern, sidecar, native_query) = {
            let borrowed = source.borrow();
            (
                borrowed
                    .pattern
                    .with_query_span(|span| *span)
                    .with_steps(native_steps),
                Sidecar::of(&borrowed),
                borrowed.native_query.clone(),
            )
        };
        let result = derive_wrapper(params.ctx().clone(), pattern, &[sidecar])?;
        result.borrow_mut().native_query = native_query;
        let object = result
            .clone()
            .into_value()
            .into_object()
            .expect("Pattern object");
        object.set("query", query)?;
        object.set("__steps", normalized)?;
        Ok(result.into_value())
    }
}

pub(super) fn copy_steps<'js>(
    ctx: &Ctx<'js>,
    target: rquickjs::Value<'js>,
    source: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let source_steps = source
        .as_object()
        .and_then(rquickjs::Class::<NativePatternWrapper>::from_object)
        .and_then(|wrapper| wrapper.borrow().pattern.steps);
    let object = target
        .as_object()
        .ok_or_else(|| throw_type_error(ctx, "registered result is not a Pattern"))?;
    let wrapper = rquickjs::Class::<NativePatternWrapper>::from_object(object)
        .ok_or_else(|| throw_type_error(ctx, "registered result is not a Pattern"))?;
    let pattern = wrapper.borrow().pattern.clone().with_steps(source_steps);
    wrapper.borrow_mut().pattern = pattern;
    let visible: rquickjs::Value = common::object(ctx, source)?.get("__steps")?;
    object.set("__steps", visible)?;
    Ok(target)
}

pub(super) fn with_locations<'js>(
    ctx: &Ctx<'js>,
    target: rquickjs::Value<'js>,
    locations: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let mut native = Vec::new();
    if let Some(array) = locations.as_array() {
        // Host-filled, but still a length claim (see `js_array_len`): read
        // through the guard and charged.
        let len = js_array_len(array)?;
        native = reserve_js_array(ctx, len)?;
        for index in 0..len {
            let location: rquickjs::Object = array.get(index)?;
            native.push((location.get("start")?, location.get("end")?));
        }
    }
    let object = target
        .as_object()
        .ok_or_else(|| throw_type_error(ctx, "registered result is not a Pattern"))?;
    let wrapper = rquickjs::Class::<NativePatternWrapper>::from_object(object)
        .ok_or_else(|| throw_type_error(ctx, "registered result is not a Pattern"))?;
    let pattern = pattern_with_own_steps(ctx, &wrapper)?.with_added_context(native);
    let sidecar = Sidecar::of(&wrapper.borrow());
    let result = derive_wrapper(ctx.clone(), pattern, &[sidecar])?;
    let result_object = result
        .clone()
        .into_value()
        .into_object()
        .expect("Pattern object");
    result_object.set("__steps", object.get::<_, rquickjs::Value>("__steps")?)?;
    let pure: rquickjs::Value = object.get("__pure")?;
    if pure.is_undefined() {
        result_object.remove("__pure")?;
        result_object.remove("__pure_loc")?;
    } else {
        result_object.set("__pure", pure)?;
        result_object.set(
            "__pure_loc",
            object.get::<_, rquickjs::Value>("__pure_loc")?,
        )?;
    }
    Ok(result.into_value())
}

#[derive(Trace, JsLifetime)]
struct NativeWithHaps<'js> {
    constructor: Function<'js>,
}

#[derive(Trace, JsLifetime)]
struct NativeWithHapsQuery<'js> {
    pattern: rquickjs::Value<'js>,
    callback: rquickjs::Value<'js>,
}

impl<'js> JsClass<'js> for NativeWithHapsQuery<'js> {
    const NAME: &'static str = "NativeWithHapsQuery";
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
        let (pattern, callback) = {
            let state = this.borrow();
            (state.pattern.clone(), state.callback.clone())
        };
        let query = common::method(params.ctx(), &pattern, "query")?;
        let haps = common::call(
            &query,
            pattern,
            [params
                .arg(0)
                .unwrap_or_else(|| common::undefined(params.ctx()))],
        )?;
        let callback = callback
            .into_function()
            .ok_or_else(|| throw_type_error(params.ctx(), "func is not a function"))?;
        callback.call((haps,))
    }
}

impl<'js> JsClass<'js> for NativeWithHaps<'js> {
    const NAME: &'static str = "NativeWithHaps";
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
        let constructor = this.borrow().constructor.clone();
        let Some(callback) = params.arg(0) else {
            return Err(refuse_empty_call(params.ctx(), ".withHaps", 1));
        };
        let query = rquickjs::Class::instance(
            params.ctx().clone(),
            NativeWithHapsQuery {
                pattern: params.this(),
                callback,
            },
        )?
        .into_value()
        .into_function()
        .expect("a callable class is a function");
        common::configure(&query, "", 1, true)?;
        construct(
            &constructor,
            query.into_value(),
            common::undefined(params.ctx()),
        )
    }
}

#[derive(Trace, JsLifetime)]
struct NativeHapMapper<'js> {
    callback: rquickjs::Value<'js>,
}

impl<'js> JsClass<'js> for NativeHapMapper<'js> {
    const NAME: &'static str = "NativeHapMapper";
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
        let callback = this.borrow().callback.clone();
        let haps = params
            .arg(0)
            .unwrap_or_else(|| common::undefined(params.ctx()));
        common::call_method(params.ctx(), haps, "map", [callback])
    }
}

fn with_hap<'js>(
    ctx: Ctx<'js>,
    This(pattern): This<rquickjs::Value<'js>>,
    Opt(callback): Opt<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    if callback.is_none() {
        return Err(refuse_empty_call(&ctx, ".withHap", 1));
    }
    let mapper = rquickjs::Class::instance(
        ctx.clone(),
        NativeHapMapper {
            callback: callback.unwrap_or_else(|| common::undefined(&ctx)),
        },
    )?
    .into_value()
    .into_function()
    .expect("a callable class is a function");
    common::configure(&mapper, "", 1, true)?;
    common::call_method(&ctx, pattern, "withHaps", [mapper.into_value()])
}

fn fmap<'js>(
    ctx: Ctx<'js>,
    This(pattern): This<rquickjs::Value<'js>>,
    Opt(callback): Opt<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let Some(callback) = callback else {
        return Err(refuse_empty_call(&ctx, ".fmap", 1));
    };
    common::call_method(&ctx, pattern, "withValue", [callback])
}

pub(super) fn install<'js>(
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
) -> Result<Function<'js>, String> {
    let pattern = native_constructor(ctx, proto).map_err(|error| error.to_string())?;

    let getter = Function::new(ctx.clone(), steps_getter).map_err(|error| error.to_string())?;
    common::configure(&getter, "get _steps", 0, false).map_err(|error| error.to_string())?;
    let setter = Function::new(ctx.clone(), steps_setter).map_err(|error| error.to_string())?;
    common::configure(&setter, "set _steps", 1, false).map_err(|error| error.to_string())?;
    common::define_accessor(ctx, proto, "_steps", Some(getter), Some(setter), true)
        .map_err(|error| error.to_string())?;

    let set_steps =
        Function::new(ctx.clone(), set_steps_method).map_err(|error| error.to_string())?;
    common::configure(&set_steps, "setSteps", 1, true).map_err(|error| error.to_string())?;
    proto
        .prop(
            "setSteps",
            rquickjs::object::Property::from(set_steps)
                .writable()
                .configurable(),
        )
        .map_err(|error| error.to_string())?;

    let with_steps = rquickjs::Class::instance(
        ctx.clone(),
        NativeWithSteps {
            constructor: pattern.clone(),
        },
    )
    .map_err(|error| error.to_string())?
    .into_value()
    .into_function()
    .expect("a callable class is a function");
    common::configure(&with_steps, "withSteps", 1, false).map_err(|error| error.to_string())?;
    proto
        .prop(
            "withSteps",
            rquickjs::object::Property::from(with_steps)
                .writable()
                .configurable(),
        )
        .map_err(|error| error.to_string())?;

    let has_steps =
        Function::new(ctx.clone(), has_steps_getter).map_err(|error| error.to_string())?;
    common::configure(&has_steps, "get hasSteps", 0, false).map_err(|error| error.to_string())?;
    common::define_accessor(ctx, proto, "hasSteps", Some(has_steps), None, true)
        .map_err(|error| error.to_string())?;

    let fmap = Function::new(ctx.clone(), fmap).map_err(|error| error.to_string())?;
    common::configure(&fmap, "fmap", 1, true).map_err(|error| error.to_string())?;
    proto
        .prop(
            "fmap",
            rquickjs::object::Property::from(fmap)
                .writable()
                .configurable(),
        )
        .map_err(|error| error.to_string())?;

    let with_haps = rquickjs::Class::instance(
        ctx.clone(),
        NativeWithHaps {
            constructor: pattern.clone(),
        },
    )
    .map_err(|error| error.to_string())?
    .into_value()
    .into_function()
    .expect("a callable class is a function");
    common::configure(&with_haps, "withHaps", 1, true).map_err(|error| error.to_string())?;
    proto
        .prop(
            "withHaps",
            rquickjs::object::Property::from(with_haps)
                .writable()
                .configurable(),
        )
        .map_err(|error| error.to_string())?;

    let with_hap = Function::new(ctx.clone(), with_hap).map_err(|error| error.to_string())?;
    common::configure(&with_hap, "withHap", 1, true).map_err(|error| error.to_string())?;
    proto
        .prop(
            "withHap",
            rquickjs::object::Property::from(with_hap)
                .writable()
                .configurable(),
        )
        .map_err(|error| error.to_string())?;
    Ok(pattern)
}
