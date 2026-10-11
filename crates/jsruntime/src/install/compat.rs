use super::*;

mod bindings;
mod gamepad;
mod midi;
mod patterns;
mod script_helpers;

pub(super) use script_helpers::REFERENCE_ENTRIES;

pub(super) fn install<'js>(
    runtime: &JsRuntime,
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
    globals: &rquickjs::Object<'js>,
) -> Result<(), String> {
    patterns::install(ctx, proto, globals)?;
    script_helpers::install(runtime, ctx, globals)?;
    gamepad::install(runtime, ctx, globals)?;
    midi::install(runtime, ctx, globals)
}
