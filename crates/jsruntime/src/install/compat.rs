use super::*;

mod bindings;
mod gamepad;
mod midi;
mod patterns;

pub(super) fn install<'js>(
    runtime: &JsRuntime,
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
    globals: &rquickjs::Object<'js>,
) -> Result<(), String> {
    patterns::install(ctx, proto, globals)?;
    gamepad::install(runtime, ctx, globals)?;
    midi::install(runtime, ctx, globals)
}
