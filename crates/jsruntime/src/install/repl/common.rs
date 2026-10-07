use super::*;
use rquickjs::function::Args;
use std::ffi::CString;

pub(super) fn undefined<'js>(ctx: &Ctx<'js>) -> rquickjs::Value<'js> {
    rquickjs::Value::new_undefined(ctx.clone())
}

pub(super) fn argument<'js>(
    ctx: &Ctx<'js>,
    values: &[rquickjs::Value<'js>],
    index: usize,
) -> rquickjs::Value<'js> {
    values.get(index).cloned().unwrap_or_else(|| undefined(ctx))
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

fn value_from_raw<'js>(
    ctx: &Ctx<'js>,
    value: rquickjs::qjs::JSValue,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    if value.tag == i64::from(rquickjs::qjs::JS_TAG_EXCEPTION) {
        Err(rquickjs::Error::Exception)
    } else {
        Ok(unsafe { rquickjs::Value::from_raw(ctx.clone(), value) })
    }
}

pub(super) fn get_property<'js>(
    ctx: &Ctx<'js>,
    value: rquickjs::Value<'js>,
    name: &str,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let name = CString::new(name).map_err(rquickjs::Error::InvalidString)?;
    let value = unsafe {
        rquickjs::qjs::JS_GetPropertyStr(ctx.as_raw().as_ptr(), value.as_raw(), name.as_ptr())
    };
    value_from_raw(ctx, value)
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

pub(super) fn call_method<'js>(
    ctx: &Ctx<'js>,
    receiver: rquickjs::Value<'js>,
    name: &str,
    values: impl IntoIterator<Item = rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let method: Function = object(ctx, receiver.clone())?.get(name)?;
    call(&method, receiver, values)
}
