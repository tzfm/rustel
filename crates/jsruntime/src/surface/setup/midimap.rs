use super::*;
use rquickjs::{
    class::{JsCell, JsClass, Readable},
    function::Params,
};

#[derive(Trace, JsLifetime)]
struct NativeMidimaps<'js> {
    stringify: Function<'js>,
    promise: Function<'js>,
    resolve: Function<'js>,
}

impl<'js> JsClass<'js> for NativeMidimaps<'js> {
    const NAME: &'static str = "NativeMidimaps";
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
        let (stringify, promise, resolve) = {
            let state = this.borrow();
            (
                state.stringify.clone(),
                state.promise.clone(),
                state.resolve.clone(),
            )
        };
        let mut map = params
            .arg(0)
            .unwrap_or_else(|| common::undefined(params.ctx()));
        if map.is_string() {
            return Err(rquickjs::Exception::throw_message(
                params.ctx(),
                "midimaps('github:...') needs the network, which score evaluation does not have - inline the map object instead",
            ));
        }
        if map.is_null() || map.is_undefined() {
            map = rquickjs::Object::new(params.ctx().clone())?.into_value();
        }
        let json = stringify
            .call::<_, rquickjs::Value>((map,))?
            .into_string()
            .ok_or_else(|| throw_type_error(params.ctx(), "MIDI maps must be JSON values"))?
            .to_cstring()?;
        rustel_core::midimap::register_midi_maps_json(json.as_str())
            .map_err(|message| throw_type_error(params.ctx(), &message))?;
        common::call(
            &resolve,
            promise.into_value(),
            [common::undefined(params.ctx())],
        )
    }
}

#[derive(Trace, JsLifetime)]
struct NativeDefaultMidimap<'js> {
    stringify: Function<'js>,
}

impl<'js> JsClass<'js> for NativeDefaultMidimap<'js> {
    const NAME: &'static str = "NativeDefaultMidimap";
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
        let stringify = this.borrow().stringify.clone();
        let mapping = params
            .arg(0)
            .unwrap_or_else(|| common::undefined(params.ctx()));
        let json = stringify
            .call::<_, rquickjs::Value>((mapping,))?
            .into_string()
            .ok_or_else(|| throw_type_error(params.ctx(), "MIDI map must be a JSON value"))?
            .to_cstring()?;
        rustel_core::midimap::register_midi_map_json("default", json.as_str())
            .map_err(|message| throw_type_error(params.ctx(), &message))?;
        Ok(common::undefined(params.ctx()))
    }
}

pub(super) fn install<'js>(ctx: &Ctx<'js>) -> Result<(), String> {
    let globals = ctx.globals();
    let json: rquickjs::Object = globals.get("JSON").map_err(|error| error.to_string())?;
    let stringify: Function = json.get("stringify").map_err(|error| error.to_string())?;
    let promise: Function = globals.get("Promise").map_err(|error| error.to_string())?;
    let resolve: Function = promise.get("resolve").map_err(|error| error.to_string())?;

    let midimaps = rquickjs::Class::instance(
        ctx.clone(),
        NativeMidimaps {
            stringify: stringify.clone(),
            promise,
            resolve,
        },
    )
    .map_err(|error| error.to_string())?
    .into_value()
    .into_function()
    .expect("a callable class is a function");
    common::configure(&midimaps, "midimaps", 1, false).map_err(|error| error.to_string())?;

    let defaultmidimap = rquickjs::Class::instance(ctx.clone(), NativeDefaultMidimap { stringify })
        .map_err(|error| error.to_string())?
        .into_value()
        .into_function()
        .expect("a callable class is a function");
    common::configure(&defaultmidimap, "defaultmidimap", 1, false)
        .map_err(|error| error.to_string())?;

    globals
        .set("midimaps", midimaps.clone())
        .map_err(|error| error.to_string())?;
    globals
        .set("defaultmidimap", defaultmidimap.clone())
        .map_err(|error| error.to_string())?;
    if let Ok(scope) = globals.get::<_, rquickjs::Object>("rustelScope") {
        scope
            .set("midimaps", midimaps)
            .map_err(|error| error.to_string())?;
        scope
            .set("defaultmidimap", defaultmidimap)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}
