//! Pattern controls and event-time graph compilation for K().

use super::*;
use rquickjs::function::{Rest, This};

type Wrapper<'js> = rquickjs::Class<'js, NativePatternWrapper<'js>>;

pub(super) fn method<'js>(
    ctx: Ctx<'js>,
    this: This<Wrapper<'js>>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<Wrapper<'js>> {
    let (pattern, owner) = {
        let wrapper = this.0.borrow();
        (wrapper.pattern.clone(), Sidecar::of(&wrapper))
    };
    build(ctx, pattern, vec![owner], args.0)
}

pub(super) fn free<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<Wrapper<'js>> {
    build(
        ctx,
        rustel_core::pure(Value::object([])),
        Vec::new(),
        args.0,
    )
}

fn build<'js>(
    ctx: Ctx<'js>,
    receiver: Pattern,
    mut sidecars: Vec<Sidecar<'js>>,
    args: Vec<rquickjs::Value<'js>>,
) -> rquickjs::Result<Wrapper<'js>> {
    let source = args
        .first()
        .and_then(|value| value.as_string())
        .ok_or_else(|| rquickjs::Exception::throw_type(&ctx, "worklet expects graph source text"))?
        .to_string()?;
    if source.len() > 64 * 1024 || args.len() > 1025 {
        return Err(rquickjs::Exception::throw_range(
            &ctx,
            "KabelSalat source or input limit exceeded",
        ));
    }
    let mut inputs = rustel_core::pure(Value::List(Vec::new()));
    for argument in args.iter().skip(1) {
        let (pattern, owner) = reify_bridged(&ctx, argument)?;
        sidecars.push(owner);
        inputs = inputs.app_left_with(pattern, |left, right| {
            let Value::List(mut values) = rustel_core::materialize_js_value(left) else {
                return Value::Undefined;
            };
            values.push(rustel_core::materialize_js_value(right));
            Value::List(values)
        });
    }
    let pattern = receiver.outer_bind_controls(inputs, move |current, input| {
        let current = rustel_core::materialize_js_value(current);
        let mut controls = match &current {
            Value::Object(controls) => controls.clone(),
            _ => rustel_core::OrderedMap::new(),
        };
        let mut inputs = match controls.get("workletInputs").cloned() {
            Some(Value::List(inputs)) => inputs,
            _ => Vec::new(),
        };
        if let Value::List(values) = rustel_core::materialize_js_value(input) {
            inputs.extend(values);
        }
        controls.insert("workletSrc".into(), Value::Str(source.clone()));
        controls.insert("workletInputs".into(), Value::List(inputs));
        Value::Object(controls)
    });
    // Keep graph compilation on the bounded score thread, after later controls.
    let identity = Function::new(ctx.clone(), |value: rquickjs::Value<'js>| value)?;
    let (id, owner) = bridge_callable(&ctx, identity)?;
    owner.cells[0].borrow_mut().kabelsalat = true;
    sidecars.push(owner);
    derive_wrapper(ctx, pattern.fmap_js_preserving_controls(id), &sidecars)
}

pub(crate) fn finalize<'js>(
    ctx: &Ctx<'js>,
    wrapper: Wrapper<'js>,
) -> rquickjs::Result<Wrapper<'js>> {
    let (pattern, owner, needed) = {
        let borrowed = wrapper.borrow();
        (
            borrowed.pattern.clone(),
            Sidecar::of(&borrowed),
            borrowed.pattern.purity().impure
                || borrowed.cells.iter().any(|cell| cell.borrow().kabelsalat),
        )
    };
    if !needed {
        return Ok(wrapper);
    }
    let function = Function::new(ctx.clone(), compile_event)?;
    let (id, compiler) = bridge_callable(ctx, function)?;
    derive_wrapper(
        ctx.clone(),
        pattern.finalize_js_controls_if(id, has_graph),
        &[owner, compiler],
    )
}

fn has_graph(value: &Value) -> bool {
    let has_source = |value: &Value| {
        value
            .get("workletSrc")
            .is_some_and(|source| !matches!(source, Value::Undefined))
    };
    if has_source(value) {
        return true;
    }
    match value.get("FX") {
        Some(Value::List(effects)) => effects.iter().any(has_source),
        _ => false,
    }
}

fn copy_object<'js>(
    ctx: &Ctx<'js>,
    object: &rquickjs::Object<'js>,
) -> rquickjs::Result<rquickjs::Object<'js>> {
    let copy = rquickjs::Object::new(ctx.clone())?;
    for (index, property) in object.props::<String, rquickjs::Value>().enumerate() {
        if index >= 4096 {
            return Err(rquickjs::Exception::throw_range(
                ctx,
                "KabelSalat event has too many controls",
            ));
        }
        let (key, value) = property?;
        copy.set(key, value)?;
    }
    Ok(copy)
}

fn frequency<'js>(ctx: &Ctx<'js>, object: &rquickjs::Object<'js>) -> rquickjs::Result<f64> {
    let note: rquickjs::Value = object.get("note")?;
    let midi = if !rquickjs::Coerced::<bool>::from_js(ctx, note.clone())?.0 {
        Some(36.0)
    } else if let Some(note) = note.as_string() {
        Some(
            rustel_core::util::note_to_midi(&note.to_string()?, 3)
                .map_err(|error| rquickjs::Exception::throw_type(ctx, &error))?,
        )
    } else {
        note.as_number()
    };
    let freq: rquickjs::Value = object.get("freq")?;
    let freq = if !rquickjs::Coerced::<bool>::from_js(ctx, freq.clone())?.0
        && let Some(midi) = midi
    {
        rustel_core::util::midi_to_freq(midi)
    } else {
        rquickjs::Coerced::<f64>::from_js(ctx, freq)?.0
    };
    let octave: rquickjs::Value = object.get("octave")?;
    let octave = if octave.is_undefined() {
        0.0
    } else {
        rquickjs::Coerced::<f64>::from_js(ctx, octave)?.0
    };
    // The bound frequency is serialized into the event's graph, and the
    // corpus compares event hashes exactly across hosts. Host `pow`
    // differs in its last bits between platforms, so trim before it binds.
    // The trim stays below 1e-8 relative, under the audio tolerance.
    Ok(rustel_core::tonal::trim_frequency(
        freq * 2.0f64.powf(octave),
    ))
}

fn compile_stage<'js>(
    ctx: &Ctx<'js>,
    value: rquickjs::Value<'js>,
    freq: f64,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let Some(object) = value.as_object() else {
        return Ok(value);
    };
    let source: rquickjs::Value = object.get("workletSrc")?;
    if source.is_undefined() {
        return Ok(value);
    }
    let source = String::from_js(ctx, source)?;
    let values: rquickjs::Value = object.get("workletInputs")?;
    let mut inputs = Vec::new();
    if let Some(array) = values.as_array() {
        if array.len() > 1024 {
            return Err(rquickjs::Exception::throw_range(
                ctx,
                "KabelSalat pattern inputs exceed 1024 values",
            ));
        }
        for value in array.iter::<rquickjs::Coerced<f64>>() {
            inputs.push(value?.0);
        }
    }
    let graph = super::kabelsalat::compile_graph(ctx, &source, &inputs, freq)?;
    rustel_kabel::Program::from_json(&graph)
        .map_err(|error| rquickjs::Exception::throw_type(ctx, &error))?;
    let copy = copy_object(ctx, object)?;
    copy.set("workletGraph", graph)?;
    Ok(copy.into_value())
}

fn compile_event<'js>(
    ctx: Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let Some(object) = value.as_object() else {
        return Ok(value);
    };
    let source: rquickjs::Value = object.get("workletSrc")?;
    let effects: rquickjs::Value = object.get("FX")?;
    if source.is_undefined() && effects.is_undefined() {
        return Ok(value);
    }
    let mut has_graph = !source.is_undefined();
    if let Some(effects) = effects.as_array() {
        if effects.len() > 64 {
            return Err(rquickjs::Exception::throw_range(
                &ctx,
                "KabelSalat effect chain exceeds 64 stages",
            ));
        }
        for effect in effects.iter::<rquickjs::Value>() {
            if let Some(effect) = effect?.as_object() {
                let source: rquickjs::Value = effect.get("workletSrc")?;
                has_graph |= !source.is_undefined();
            }
        }
    }
    if !has_graph {
        return Ok(value);
    }
    let freq = frequency(&ctx, object)?;
    let result = compile_stage(&ctx, value.clone(), freq)?;
    if let Some(effects) = effects.as_array() {
        let compiled = rquickjs::Array::new(ctx.clone())?;
        for (index, effect) in effects.iter::<rquickjs::Value>().enumerate() {
            compiled.set(index, compile_stage(&ctx, effect?, freq)?)?;
        }
        let copy = copy_object(&ctx, result.as_object().unwrap_or(object))?;
        copy.set("FX", compiled)?;
        Ok(copy.into_value())
    } else {
        Ok(result)
    }
}
