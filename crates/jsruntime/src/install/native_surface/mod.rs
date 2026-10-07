use super::*;

pub(crate) mod bindings;
mod callbacks;
mod composition;
mod controls;
mod modulation;
mod patterns;
mod values;

pub(super) fn install<'js>(
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
    globals: &rquickjs::Object<'js>,
) -> Result<(), String> {
    bindings::install(ctx, proto, globals)
}
