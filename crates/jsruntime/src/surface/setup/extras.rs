use super::*;
use rquickjs::{
    Coerced, FromJs,
    class::{JsCell, JsClass, Readable},
    function::{Params, Rest},
};

fn note_to_midi<'js>(ctx: Ctx<'js>, args: Rest<rquickjs::Value<'js>>) -> rquickjs::Result<f64> {
    let note = common::argument(&ctx, &args.0, 0);
    let source = note
        .as_string()
        .map(|value| value.clone().to_cstring())
        .transpose()?
        .map(|value| value.as_str().to_owned());
    let Some(source) = source else {
        let shown = common::property_name(&ctx, note)?;
        return Err(rquickjs::Exception::throw_message(
            &ctx,
            &format!("not a note: \"{shown}\""),
        ));
    };
    let bytes = source.as_bytes();
    let valid_pitch = bytes
        .first()
        .is_some_and(|value| value.to_ascii_lowercase().is_ascii_lowercase())
        && matches!(bytes[0].to_ascii_lowercase(), b'a'..=b'g');
    if !valid_pitch {
        return Err(rquickjs::Exception::throw_message(
            &ctx,
            &format!("not a note: \"{source}\""),
        ));
    }
    let mut split = 1;
    let mut accidental_offset = 0_i32;
    while split < bytes.len() {
        accidental_offset += match bytes[split] {
            b'#' | b's' => 1,
            b'b' | b'f' => -1,
            _ => break,
        };
        split += 1;
    }
    let octave_text = &source[split..];
    let digits = octave_text.strip_prefix('-').unwrap_or(octave_text);
    if !digits.bytes().all(|value| value.is_ascii_digit()) {
        return Err(rquickjs::Exception::throw_message(
            &ctx,
            &format!("not a note: \"{source}\""),
        ));
    }
    let octave: f64 = if octave_text.is_empty() {
        let default = args
            .0
            .get(1)
            .filter(|value| !value.is_undefined())
            .cloned()
            .unwrap_or_else(|| rquickjs::Value::new_int(ctx.clone(), 3));
        let number: Function = ctx.globals().get("Number")?;
        number.call((default,))?
    } else {
        let number: Function = ctx.globals().get("Number")?;
        number.call((rquickjs::String::from_str(ctx.clone(), octave_text)?,))?
    };
    let chroma = match bytes[0].to_ascii_lowercase() {
        b'c' => 0,
        b'd' => 2,
        b'e' => 4,
        b'f' => 5,
        b'g' => 7,
        b'a' => 9,
        b'b' => 11,
        _ => unreachable!(),
    };
    Ok((octave + 1.0) * 12.0 + f64::from(chroma + accidental_offset))
}

#[derive(Trace, JsLifetime)]
struct NativeArpPicker<'js> {
    haps: rquickjs::Value<'js>,
    #[qjs(skip_trace)]
    length: f64,
}

impl<'js> JsClass<'js> for NativeArpPicker<'js> {
    const NAME: &'static str = "NativeArpPicker";
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
        let (haps, length) = {
            let picker = this.borrow();
            (picker.haps.clone(), picker.length)
        };
        let index = Coerced::<f64>::from_js(
            params.ctx(),
            params
                .arg(0)
                .unwrap_or_else(|| common::undefined(params.ctx())),
        )?
        .0;
        let key = ((index % length) + length) % length;
        let reflect: rquickjs::Object = params.ctx().globals().get("Reflect")?;
        let get: Function = reflect.get("get")?;
        common::call(
            &get,
            reflect.into_value(),
            [haps, rquickjs::Value::new_number(params.ctx().clone(), key)],
        )
    }
}

#[derive(Trace, JsLifetime)]
struct NativeArpSelector<'js> {
    reify: Function<'js>,
    indices: rquickjs::Value<'js>,
}

impl<'js> JsClass<'js> for NativeArpSelector<'js> {
    const NAME: &'static str = "NativeArpSelector";
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
        let (reify, indices) = {
            let selector = this.borrow();
            (selector.reify.clone(), selector.indices.clone())
        };
        let haps = params
            .arg(0)
            .unwrap_or_else(|| common::undefined(params.ctx()));
        let length = Coerced::<f64>::from_js(
            params.ctx(),
            common::get(params.ctx(), haps.clone(), "length")?,
        )?
        .0;
        let picker =
            rquickjs::Class::instance(params.ctx().clone(), NativeArpPicker { haps, length })?
                .into_value()
                .into_function()
                .expect("a callable class is a function");
        common::configure(&picker, "", 1, false)?;
        let pattern: rquickjs::Value = reify.call((indices,))?;
        common::call_method(params.ctx(), pattern, "fmap", [picker.into_value()])
    }
}

#[derive(Trace, JsLifetime)]
struct NativeRawArpSelector<'js> {
    reify: Function<'js>,
}

impl<'js> JsClass<'js> for NativeRawArpSelector<'js> {
    const NAME: &'static str = "NativeRawArpSelector";
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
        let reify = this.borrow().reify.clone();
        let selector = rquickjs::Class::instance(
            params.ctx().clone(),
            NativeArpSelector {
                reify,
                indices: params
                    .arg(0)
                    .unwrap_or_else(|| common::undefined(params.ctx())),
            },
        )?
        .into_value()
        .into_function()
        .expect("a callable class is a function");
        common::configure(&selector, "", 1, false)?;
        Ok(selector.into_value())
    }
}

pub(super) struct Surface<'js> {
    pub(super) note_to_midi: Function<'js>,
    pub(super) raw_arp_selector: Function<'js>,
}

pub(super) fn install<'js>(ctx: &Ctx<'js>, reify: Function<'js>) -> Result<Surface<'js>, String> {
    let note_to_midi =
        Function::new(ctx.clone(), note_to_midi).map_err(|error| error.to_string())?;
    common::configure(&note_to_midi, "noteToMidi", 1, false).map_err(|error| error.to_string())?;
    let raw_arp_selector = rquickjs::Class::instance(ctx.clone(), NativeRawArpSelector { reify })
        .map_err(|error| error.to_string())?
        .into_value()
        .into_function()
        .expect("a callable class is a function");
    common::configure(&raw_arp_selector, "rawArpSelector", 1, true)
        .map_err(|error| error.to_string())?;
    Ok(Surface {
        note_to_midi,
        raw_arp_selector,
    })
}
