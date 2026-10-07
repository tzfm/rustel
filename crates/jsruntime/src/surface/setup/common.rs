use super::*;
use rquickjs::{Coerced, FromJs, function::Args};

pub(super) fn undefined<'js>(ctx: &Ctx<'js>) -> rquickjs::Value<'js> {
    rquickjs::Value::new_undefined(ctx.clone())
}

pub(super) fn argument<'js>(
    ctx: &Ctx<'js>,
    args: &[rquickjs::Value<'js>],
    index: usize,
) -> rquickjs::Value<'js> {
    args.get(index).cloned().unwrap_or_else(|| undefined(ctx))
}

pub(super) fn call<'js>(
    function: &Function<'js>,
    this: rquickjs::Value<'js>,
    values: impl IntoIterator<Item = rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let mut args = Args::new_unsized(function.ctx().clone());
    args.this(this)?;
    args.push_args(values)?;
    args.apply(function)
}

pub(super) fn object<'js>(
    ctx: &Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Object<'js>> {
    if value.is_null() || value.is_undefined() {
        return Err(throw_type_error(
            ctx,
            "Cannot convert undefined or null to object",
        ));
    }
    if let Some(object) = value.as_object() {
        return Ok(object.clone());
    }
    let constructor: Function = ctx.globals().get("Object")?;
    constructor.call((value,))
}

pub(super) fn get<'js>(
    ctx: &Ctx<'js>,
    value: rquickjs::Value<'js>,
    name: &str,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    object(ctx, value)?.get(name)
}

pub(super) fn method<'js>(
    ctx: &Ctx<'js>,
    value: &rquickjs::Value<'js>,
    name: &str,
) -> rquickjs::Result<Function<'js>> {
    object(ctx, value.clone())?.get(name)
}

pub(super) fn call_method<'js>(
    ctx: &Ctx<'js>,
    receiver: rquickjs::Value<'js>,
    name: &str,
    values: impl IntoIterator<Item = rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let function = method(ctx, &receiver, name)?;
    call(&function, receiver, values)
}

pub(super) fn truthy<'js>(ctx: &Ctx<'js>, value: rquickjs::Value<'js>) -> rquickjs::Result<bool> {
    Ok(Coerced::<bool>::from_js(ctx, value)?.0)
}

pub(super) fn property_name<'js>(
    ctx: &Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<String> {
    Ok(Coerced::<String>::from_js(ctx, value)?.0)
}

pub(super) fn array<'js>(
    ctx: &Ctx<'js>,
    values: impl IntoIterator<Item = rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Array<'js>> {
    let array = rquickjs::Array::new(ctx.clone())?;
    for (index, value) in values.into_iter().enumerate() {
        array.set(index, value)?;
    }
    Ok(array)
}

pub(super) fn configure(
    function: &Function<'_>,
    name: &str,
    length: usize,
    constructible: bool,
) -> rquickjs::Result<()> {
    function.as_inner().remove("length")?;
    function.as_inner().remove("name")?;
    function.set_length(length)?;
    function.set_name(name)?;
    function.set_constructor(constructible);
    if constructible {
        let prototype = rquickjs::Object::new(function.ctx().clone())?;
        prototype.prop(
            "constructor",
            rquickjs::object::Property::from(function.clone())
                .writable()
                .configurable(),
        )?;
        function.as_inner().prop(
            "prototype",
            rquickjs::object::Property::from(prototype).writable(),
        )?;
    }
    Ok(())
}

pub(super) fn define_accessor<'js>(
    ctx: &Ctx<'js>,
    target: &rquickjs::Object<'js>,
    name: &str,
    getter: Option<Function<'js>>,
    setter: Option<Function<'js>>,
    configurable: bool,
) -> rquickjs::Result<()> {
    let descriptor = rquickjs::Object::new(ctx.clone())?;
    if let Some(getter) = getter {
        descriptor.set("get", getter)?;
    }
    if let Some(setter) = setter {
        descriptor.set("set", setter)?;
    }
    descriptor.set("configurable", configurable)?;
    let object: rquickjs::Object = ctx.globals().get("Object")?;
    let define: Function = object.get("defineProperty")?;
    call(
        &define,
        object.into_value(),
        [
            target.clone().into_value(),
            rquickjs::String::from_str(ctx.clone(), name)?.into_value(),
            descriptor.into_value(),
        ],
    )?;
    Ok(())
}

pub(super) fn define_reserved<'js, K>(
    target: &rquickjs::Object<'js>,
    name: K,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<()>
where
    K: rquickjs::IntoAtom<'js>,
{
    target.prop(name, rquickjs::object::Property::from(value))
}
