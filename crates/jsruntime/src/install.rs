//! Construction-time bindings for the QuickJS score realm.
//!
//! Pattern semantics and control operations belong in `rustel-core`. Code stays
//! here only when the public contract depends on ECMAScript identity, coercion,
//! reflection, or a user callback that must remain on the JavaScript heap.

use super::*;

mod callable;
mod compat;
mod core_utils;
#[cfg(feature = "extensions")]
mod extensions;
/// Recording a visuals sketch. Compiled only with the `hydra` feature; the
/// default build refuses the names instead, from `unsupported`.
#[cfg(feature = "hydra")]
mod hydra;
#[cfg(feature = "hydra")]
pub use hydra::HYDRA_SCOPE_POLICY;
mod native_surface;
mod repl;
mod slider;
mod surface;
mod unsupported;

mod foundation;
mod generators;
mod pattern_bindings;

use callable::{configure_function, native_curry, native_named_curry};
use foundation::SemanticSurface;
#[cfg(feature = "hydra")]
pub use hydra::HydraCandidate;
#[cfg(feature = "hydra")]
pub(crate) use hydra::{HydraChains, restore_surface as restore_hydra_surface};

pub fn reference_entries() -> impl Iterator<Item = &'static rustel_core::reference::ReferenceEntry>
{
    unsupported::REFERENCE_ENTRIES
        .iter()
        .chain(slider::REFERENCE_ENTRIES.iter())
        .chain(generators::REFERENCE_ENTRIES.iter())
        .chain(native_surface::bindings::REFERENCE_ENTRIES.iter())
        .chain(pattern_bindings::REFERENCE_ENTRIES.iter())
        .chain(surface::REFERENCE_ENTRIES.iter())
        .chain(crate::surface::JOIN_REFERENCE_ENTRIES.iter())
}

fn install_logger<'js>(
    runtime: &JsRuntime,
    ctx: &Ctx<'js>,
    globals: &rquickjs::Object<'js>,
) -> Result<(), String> {
    let current: rquickjs::Value = globals.get("logger").map_err(|error| error.to_string())?;
    if !current.is_undefined() {
        return Ok(());
    }
    let sink = runtime.logs.clone();
    let logger = Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, args: rquickjs::function::Rest<rquickjs::Value<'js>>| {
            let message = args
                .0
                .first()
                .cloned()
                .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()));
            let message = rquickjs::Coerced::<String>::from_js(&ctx, message)?.0;
            let kind = args.0.get(1).cloned();
            let line = match kind {
                Some(kind) if rquickjs::Coerced::<bool>::from_js(&ctx, kind.clone())?.0 => {
                    let kind = rquickjs::Coerced::<String>::from_js(&ctx, kind)?.0;
                    format!("[{kind}] {message}")
                }
                _ => message,
            };
            let mut sink = sink.borrow_mut();
            if sink.len() < MAX_BUFFERED_LOGS {
                sink.push(line);
            }
            Ok::<(), rquickjs::Error>(())
        },
    )
    .map_err(|error| error.to_string())?;
    configure_function(&logger, "logger", 2, false).map_err(|error| error.to_string())?;
    globals
        .set("logger", logger)
        .map_err(|error| error.to_string())
}

impl JsRuntime {
    /// Install the native pure-pattern host surface used by transpiled user
    /// JavaScript. Returned objects are real `PatternWrapper` instances, so
    /// identity and prototype methods use the same callback ownership shape.
    ///
    /// The surface is generated from the native registries. Controls are
    /// installed before combinators because later registrations replace
    /// same-named control aliases on the prototype.
    pub fn install_semantic_bindings(&self) -> Result<(), String> {
        let _settings = self.core_settings.bind();
        with_ctx(&self.ctx, |ctx| -> Result<(), String> {
            let surface = foundation::install(self, ctx.clone())?;
            install_logger(self, &ctx, &surface.globals)?;
            generators::install(self, ctx.clone(), &surface)?;
            #[cfg(feature = "extensions")]
            extensions::install(&ctx, &surface.proto, &surface.globals)?;
            pattern_bindings::install(ctx.clone(), &surface)?;
            let registration_seal = install_user_setup_surface(
                &ctx,
                &surface.proto,
                &surface.globals,
                &surface.install_canonical_shrink_grow,
            )?;
            native_surface::install(&ctx, &surface.proto, &surface.globals)?;
            core_utils::install(&ctx, &surface.globals)?;
            unsupported::install(self, &ctx, &surface.globals)?;
            compat::install(self, &ctx, &surface.proto, &surface.globals)?;
            repl::install(self, &ctx, &surface.proto, &surface.globals)?;
            #[cfg(feature = "hydra")]
            hydra::install(self, &ctx, &surface.globals)?;
            install_midimap_surface(&ctx)?;
            registration_seal.finish(&surface.proto)?;
            repl::install_clear_scope(&ctx)?;
            Ok(())
        })
    }
}
