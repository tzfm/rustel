use super::*;
use rquickjs::{
    class::{JsCell, JsClass, Readable},
    function::{Args, Params},
};

#[derive(Clone, Copy)]
struct CurryShape {
    root_name: &'static str,
    partial_name: &'static str,
    constructible: bool,
}

#[derive(Trace, JsLifetime)]
pub(super) struct NativeCurry<'js> {
    raw: Function<'js>,
    collected: Vec<rquickjs::Value<'js>>,
    #[qjs(skip_trace)]
    arity: usize,
    #[qjs(skip_trace)]
    shape: Option<CurryShape>,
}

impl<'js> JsClass<'js> for NativeCurry<'js> {
    const NAME: &'static str = "NativeCurry";
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
        let (raw, arity, shape, mut collected) = {
            let current = this.borrow();
            (
                current.raw.clone(),
                current.arity,
                current.shape,
                current.collected.clone(),
            )
        };
        collected.extend((0..params.len()).filter_map(|index| params.arg(index)));
        if collected.len() < arity {
            return Ok(
                native_curry_inner(params.ctx(), raw, arity, collected, shape, true)?.into_value(),
            );
        }
        let mut args = Args::new(params.ctx().clone(), collected.len());
        args.push_args(collected)?;
        args.apply(&raw)
    }
}

fn native_curry_inner<'js>(
    ctx: &Ctx<'js>,
    raw: Function<'js>,
    arity: usize,
    collected: Vec<rquickjs::Value<'js>>,
    shape: Option<CurryShape>,
    partial: bool,
) -> rquickjs::Result<rquickjs::Class<'js, NativeCurry<'js>>> {
    let callable = rquickjs::Class::instance(
        ctx.clone(),
        NativeCurry {
            raw,
            collected,
            arity,
            shape,
        },
    )?;
    if let Some(shape) = shape {
        let function = callable
            .clone()
            .into_value()
            .into_function()
            .expect("a callable class is a function");
        let name = if partial {
            shape.partial_name
        } else {
            shape.root_name
        };
        configure_function(&function, name, 0, shape.constructible)?;
    }
    Ok(callable)
}

pub(super) fn native_curry<'js>(
    ctx: &Ctx<'js>,
    raw: Function<'js>,
    arity: usize,
    collected: Vec<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Class<'js, NativeCurry<'js>>> {
    native_curry_inner(ctx, raw, arity, collected, None, false)
}

pub(super) fn native_named_curry<'js>(
    ctx: &Ctx<'js>,
    raw: Function<'js>,
    arity: usize,
    root_name: &'static str,
    partial_name: &'static str,
    constructible: bool,
) -> rquickjs::Result<rquickjs::Class<'js, NativeCurry<'js>>> {
    native_curry_inner(
        ctx,
        raw,
        arity,
        Vec::new(),
        Some(CurryShape {
            root_name,
            partial_name,
            constructible,
        }),
        false,
    )
}

pub(super) fn configure_function(
    function: &Function<'_>,
    name: &str,
    length: usize,
    constructible: bool,
) -> rquickjs::Result<()> {
    function.set_name(name)?;
    function.set_length(length)?;
    function.set_constructor(constructible);
    if !constructible {
        return Ok(());
    }

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
    )
}
