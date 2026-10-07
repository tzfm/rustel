use super::*;

mod lists;
mod stepwise;
mod values;

pub(super) struct SemanticSurface<'js> {
    pub(super) globals: rquickjs::Object<'js>,
    pub(super) proto: rquickjs::Object<'js>,
    pub(super) set_canonical_polymeter_pace: Function<'js>,
    pub(super) set_canonical_zoom: Function<'js>,
    pub(super) install_canonical_shrink_grow: Function<'js>,
}

pub(super) fn install<'js>(
    _runtime: &JsRuntime,
    ctx: Ctx<'js>,
) -> Result<SemanticSurface<'js>, String> {
    let globals = values::install(&ctx)?;
    let proto = rquickjs::Class::<NativePatternWrapper>::prototype(&ctx)
        .map_err(|error| error.to_string())?
        .ok_or("NativePatternWrapper has no prototype")?;
    let installed_lists = lists::install(&ctx, &globals, &proto)?;
    Ok(SemanticSurface {
        globals,
        proto,
        set_canonical_polymeter_pace: installed_lists.set_canonical_polymeter_pace,
        set_canonical_zoom: installed_lists.set_canonical_zoom,
        install_canonical_shrink_grow: installed_lists.install_canonical_shrink_grow,
    })
}
