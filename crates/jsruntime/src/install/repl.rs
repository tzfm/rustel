//! Native terminal score bindings.

use super::*;

mod common;
mod lanes;
mod query;
mod tempo;
mod timeline;
mod visuals;

pub(super) use visuals::install_clear_scope;

pub(super) fn install<'js>(
    runtime: &JsRuntime,
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
    globals: &rquickjs::Object<'js>,
) -> Result<(), String> {
    query::install(ctx, proto)?;
    tempo::install(runtime, ctx, globals)?;
    visuals::install(ctx, proto, globals)?;
    lanes::install(ctx, proto, globals)?;
    timeline::install(runtime, ctx, globals)?;
    Ok(())
}
