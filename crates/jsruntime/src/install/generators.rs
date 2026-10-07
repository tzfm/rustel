use super::*;
use rquickjs::function::This;

mod effects;
mod patterns;
mod pick;

pub(super) use effects::REFERENCE_ENTRIES;

fn is_array<'js>(ctx: &Ctx<'js>, value: &rquickjs::Value<'js>) -> rquickjs::Result<bool> {
    let array: rquickjs::Object = ctx.globals().get("Array")?;
    let is_array: Function = array.get("isArray")?;
    is_array.call((This(array), value.clone()))
}

pub(super) fn install<'js>(
    runtime: &JsRuntime,
    ctx: Ctx<'js>,
    surface: &SemanticSurface<'js>,
) -> Result<(), String> {
    patterns::install(&ctx, &surface.globals, runtime.pointer.as_ref())?;
    slider::install(&ctx)?;
    effects::install(runtime, &ctx)?;
    pick::install(&ctx, &surface.proto, &surface.globals)
}
