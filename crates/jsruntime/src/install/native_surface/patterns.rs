use super::{values::coerced_number, *};
pub(super) fn identity<'js>(value: rquickjs::Value<'js>) -> rquickjs::Value<'js> {
    value
}

pub(super) fn identity_method<'js>(
    this: rquickjs::function::This<rquickjs::Value<'js>>,
    _args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> rquickjs::Value<'js> {
    this.0
}

pub(super) fn hush<'js>(
    ctx: Ctx<'js>,
    _this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
) -> NativeResult<'js> {
    derive_wrapper(ctx, rustel_core::silence(), &[])
}

pub(super) fn split_queries<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
) -> NativeResult<'js> {
    let (pattern, sidecar) = {
        let wrapper = this.0.borrow();
        (wrapper.pattern.split_queries(), Sidecar::of(&wrapper))
    };
    derive_wrapper(ctx, pattern, &[sidecar])
}

fn bind<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
    mode: rustel_core::JoinMode,
) -> NativeResult<'js> {
    let (receiver, mut sidecars) = {
        let wrapper = this.0.borrow();
        (wrapper.pattern.clone(), vec![Sidecar::of(&wrapper)])
    };
    let Some(function) = args.0.first().and_then(rquickjs::Value::as_function) else {
        return derive_wrapper(
            ctx,
            rustel_core::query_error_pattern("func is not a function"),
            &sidecars,
        );
    };
    let (id, sidecar) = bridge_callable(&ctx, function.clone())?;
    sidecars.push(sidecar);
    let mapped = receiver.fmap_js(id);
    let pattern = match mode {
        rustel_core::JoinMode::Inner => mapped.inner_join(),
        rustel_core::JoinMode::Outer => mapped.outer_join(),
        rustel_core::JoinMode::Mix => mapped.mix_join(),
        rustel_core::JoinMode::Squeeze => mapped.squeeze_join(),
        rustel_core::JoinMode::Reset => mapped.reset_join(),
        rustel_core::JoinMode::Restart => mapped.restart_join(),
    };
    derive_wrapper(ctx, pattern, &sidecars)
}

pub(super) fn inner_bind<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> NativeResult<'js> {
    bind(ctx, this, args, rustel_core::JoinMode::Inner)
}

pub(super) fn outer_bind<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> NativeResult<'js> {
    bind(ctx, this, args, rustel_core::JoinMode::Outer)
}

pub(super) fn mix_bind<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> NativeResult<'js> {
    bind(ctx, this, args, rustel_core::JoinMode::Mix)
}

pub(super) fn squeeze_bind<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> NativeResult<'js> {
    bind(ctx, this, args, rustel_core::JoinMode::Squeeze)
}

pub(super) fn app_left<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> NativeResult<'js> {
    app(ctx, this, args, false)
}

pub(super) fn app_both<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> NativeResult<'js> {
    app(ctx, this, args, true)
}

fn app<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
    both: bool,
) -> NativeResult<'js> {
    if args.0.is_empty() {
        let name = if both { ".appBoth" } else { ".appLeft" };
        return Err(refuse_empty_call(&ctx, name, 1));
    }
    let (left, mut sidecars) = {
        let wrapper = this.0.borrow();
        (wrapper.pattern.clone(), vec![Sidecar::of(&wrapper)])
    };
    let right = args
        .0
        .first()
        .cloned()
        .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()));
    let (right, sidecar) = reify_bridged(&ctx, &right)?;
    sidecars.push(sidecar);
    let pattern = if both {
        rustel_core::app_both_call(&left, &right)
    } else {
        rustel_core::app_left_call(&left, &right)
    };
    derive_wrapper(ctx, pattern, &sidecars)
}

pub(super) fn piano<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
) -> NativeResult<'js> {
    let (pattern, sidecar) = {
        let wrapper = this.0.borrow();
        (wrapper.pattern.clone(), Sidecar::of(&wrapper))
    };
    let max_pan = rustel_core::util::note_to_midi("C8", 3).unwrap_or(108.0);
    let pattern = pattern.fmap(move |value| {
        let value = rustel_core::materialize_js_value(value);
        let mut controls = match value {
            Value::Object(controls) => controls,
            _ => rustel_core::OrderedMap::new(),
        };
        if controls
            .get("clip")
            .is_none_or(rustel_core::Value::is_nullish)
        {
            controls.insert("clip".into(), Value::F64(1.0));
        }
        controls.insert("s".into(), Value::Str("piano".into()));
        controls.insert("release".into(), Value::F64(0.1));
        let midi = rustel_core::util::value_to_midi(&Value::Object(controls.clone()), Some(36.0))
            .unwrap_or(36.0);
        let pan = (midi.round() / max_pan).min(1.0) * 0.5 + 0.25;
        let previous = controls
            .get("pan")
            .filter(|value| value.js_truthy())
            .cloned()
            .unwrap_or(Value::F64(1.0));
        let pan = rustel_core::compose::ComposeOp::Mul.apply_scalar(&previous, &Value::F64(pan));
        controls.insert("pan".into(), pan);
        Value::Object(controls)
    });
    derive_wrapper(ctx, pattern, &[sidecar])
}

pub(super) fn arrange<'js>(
    ctx: Ctx<'js>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> NativeResult<'js> {
    let mut items = Vec::with_capacity(args.0.len());
    let mut sidecars = Vec::new();
    let mut total = Fraction::ZERO;
    for section in args.0 {
        let array = section.as_array().ok_or_else(|| {
            rquickjs::Exception::throw_type(&ctx, "an arrangement section must be an array")
        })?;
        let cycles_value: rquickjs::Value = array.get(0)?;
        let cycles = Fraction::from_f64(coerced_number(&ctx, cycles_value)?).ok_or_else(|| {
            rquickjs::Exception::throw_range(&ctx, "arrangement cycles must be finite")
        })?;
        let value: rquickjs::Value = array.get(1)?;
        let (pattern, nested) = reify_bridged(&ctx, &value)?;
        sidecars.push(nested);
        total = total.checked_add(cycles).ok_or_else(|| {
            rquickjs::Exception::throw_range(&ctx, "arrangement total cycles overflow")
        })?;
        items.push((Some(cycles), pattern.fast(cycles)));
    }
    let pattern = rustel_core::combinators::stepcat(&items).slow(total);
    derive_wrapper(ctx, pattern, &sidecars)
}

pub(super) fn choose_lookup<'js>(
    ctx: &Ctx<'js>,
    values: &[rquickjs::Value<'js>],
) -> rquickjs::Result<rquickjs::Array<'js>> {
    let array = rquickjs::Array::new(ctx.clone())?;
    for (index, value) in values.iter().enumerate() {
        array.set(index, value.clone())?;
    }
    Ok(array)
}

fn choose_from<'js>(
    ctx: Ctx<'js>,
    selector: Pattern,
    mut sidecars: Vec<Sidecar<'js>>,
    values: &[rquickjs::Value<'js>],
    mode: rustel_core::JoinMode,
) -> NativeResult<'js> {
    let lookup = choose_lookup(&ctx, values)?;
    let len = values.len() as f64;
    let selector = rustel_core::combinators::range(&selector, 0.0, len)
        .fmap(|value| Value::F64(value.as_f64().unwrap_or(f64::NAN).floor()));
    let (lookup, mut nested) = native_pick_lookup(&ctx, &lookup.into_value())?;
    sidecars.append(&mut nested);
    derive_wrapper(
        ctx,
        rustel_core::pick(selector, lookup, rustel_core::PickIndexMode::Clamp, mode),
        &sidecars,
    )
}

pub(super) fn choose_with<'js>(
    ctx: Ctx<'js>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
    mode: rustel_core::JoinMode,
) -> NativeResult<'js> {
    let selector_value = args
        .0
        .first()
        .cloned()
        .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()));
    let values = args
        .0
        .get(1)
        .and_then(rquickjs::Value::as_array)
        .ok_or_else(|| rquickjs::Exception::throw_type(&ctx, "choices must be an array"))?;
    // `choose` mirrors one host slot per claimed choice: copied out through
    // the guard (see `js_array_values`).
    let choices = js_array_values::<rquickjs::Value>(&ctx, values)?;
    let (selector, sidecar) = reify_bridged(&ctx, &selector_value)?;
    choose_from(ctx, selector, vec![sidecar], &choices, mode)
}

pub(super) fn choose_random<'js>(
    ctx: Ctx<'js>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
    cycles: bool,
) -> NativeResult<'js> {
    let selector = if cycles {
        rustel_core::combinators::segment(&rustel_core::signal::rand(), Fraction::ONE)
    } else {
        rustel_core::signal::rand()
    };
    let mode = if cycles {
        rustel_core::JoinMode::Inner
    } else {
        rustel_core::JoinMode::Outer
    };
    choose_from(ctx, selector, Vec::new(), &args.0, mode)
}

pub(super) fn choose_inner_random<'js>(
    ctx: Ctx<'js>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> NativeResult<'js> {
    choose_from(
        ctx,
        rustel_core::signal::rand(),
        Vec::new(),
        &args.0,
        rustel_core::JoinMode::Inner,
    )
}

pub(super) fn choose_method<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
    bipolar: bool,
) -> NativeResult<'js> {
    if args.0.is_empty() {
        let name = if bipolar { ".choose2" } else { ".choose" };
        return Err(refuse_empty_call(&ctx, name, 1));
    }
    let (selector, sidecar) = {
        let wrapper = this.0.borrow();
        (wrapper.pattern.clone(), Sidecar::of(&wrapper))
    };
    let selector = if bipolar {
        rustel_core::combinators::from_bipolar(&selector)
    } else {
        selector
    };
    choose_from(
        ctx,
        selector,
        vec![sidecar],
        &args.0,
        rustel_core::JoinMode::Outer,
    )
}
