use super::{patterns::choose_lookup, *};
/// A distortion of `pattern` from the arguments of a script call, as
/// [`distort_with_algorithm`] builds it.
fn distort_with<'js>(
    ctx: &Ctx<'js>,
    pattern: Pattern,
    args: &rquickjs::function::Rest<rquickjs::Value<'js>>,
    algorithm: &'static str,
    sidecars: &mut Vec<Sidecar<'js>>,
) -> rquickjs::Result<Pattern> {
    let first = args
        .0
        .first()
        .cloned()
        .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()));
    let (amount, sidecar) = reify_bridged(ctx, &first)?;
    sidecars.push(sidecar);
    let volume = match args.0.get(1).filter(|value| !value.is_undefined()) {
        Some(volume) => {
            let (volume, sidecar) = reify_bridged(ctx, volume)?;
            sidecars.push(sidecar);
            Some(volume)
        }
        None => None,
    };
    Ok(distort_with_algorithm(pattern, amount, volume, algorithm))
}

pub(super) fn distort_method<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
    algorithm: &'static str,
) -> NativeResult<'js> {
    let (receiver, mut sidecars) = {
        let wrapper = this.0.borrow();
        (wrapper.pattern.clone(), vec![Sidecar::of(&wrapper)])
    };
    let distorted = distort_with(&ctx, receiver, &args, algorithm, &mut sidecars)?;
    derive_wrapper(ctx, distorted, &sidecars)
}

pub(super) fn distort_free<'js>(
    ctx: Ctx<'js>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
    algorithm: &'static str,
) -> NativeResult<'js> {
    let receiver = rustel_core::pure(Value::object(std::iter::empty()));
    let mut sidecars = Vec::new();
    let distorted = distort_with(&ctx, receiver, &args, algorithm, &mut sidecars)?;
    derive_wrapper(ctx, distorted, &sidecars)
}

pub(super) fn span_signal(kind: &str) -> Pattern {
    match kind {
        "cyclesPer" => {
            rustel_core::state_signal(|state| Value::F64(state.span.duration().to_f64()))
        }
        "perx" => rustel_core::state_signal(|state| {
            Value::F64((1.0 / state.span.duration().to_f64()).log2() + 1.0)
        }),
        _ => rustel_core::state_signal(|state| Value::F64(1.0 / state.span.duration().to_f64())),
    }
}

pub(super) fn append_pattern_value(left: &Value, right: &Value) -> Value {
    let mut values = match rustel_core::materialize_js_value(left) {
        Value::List(values) => values.to_vec(),
        value => vec![value],
    };
    values.push(rustel_core::materialize_js_value(right));
    Value::list(values)
}

fn reify_pattern_list<'js>(
    ctx: &Ctx<'js>,
    value: &rquickjs::Value<'js>,
) -> rquickjs::Result<(Pattern, Vec<Sidecar<'js>>)> {
    let Some(array) = value.as_array() else {
        let (pattern, sidecar) = reify_bridged(ctx, value)?;
        return Ok((pattern, vec![sidecar]));
    };
    let mut result = rustel_core::pure(Value::list(Vec::new()));
    let mut sidecars = reserve_js_array(ctx, js_array_len(array)?)?;
    for value in array.iter::<rquickjs::Value>() {
        let (pattern, sidecar) = reify_bridged(ctx, &value?)?;
        sidecars.push(sidecar);
        result = result.app_both_with(pattern, append_pattern_value);
    }
    Ok((result, sidecars))
}

fn with_list_control(pattern: Pattern, values: Pattern, name: &'static str) -> Pattern {
    pattern.app_left_with(values, move |current, value| {
        let current = rustel_core::materialize_js_value(current);
        let mut controls = match current {
            Value::Object(controls) => controls,
            _ => rustel_core::OrderedMap::new(),
        };
        controls.insert(name.into(), rustel_core::materialize_js_value(value));
        Value::Object(controls)
    })
}

pub(super) fn list_control_method<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
    name: &'static str,
) -> NativeResult<'js> {
    let (receiver, mut sidecars) = {
        let wrapper = this.0.borrow();
        (wrapper.pattern.clone(), vec![Sidecar::of(&wrapper)])
    };
    let value = args
        .0
        .first()
        .cloned()
        .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()));
    let (values, mut nested) = reify_pattern_list(&ctx, &value)?;
    sidecars.append(&mut nested);
    derive_wrapper(ctx, with_list_control(receiver, values, name), &sidecars)
}

pub(super) fn list_control_free<'js>(
    ctx: Ctx<'js>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
    name: &'static str,
) -> NativeResult<'js> {
    let value = args
        .0
        .first()
        .cloned()
        .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()));
    let (values, sidecars) = reify_pattern_list(&ctx, &value)?;
    let name = name.to_owned();
    let pattern = values.fmap(move |value| {
        Value::object([(name.clone(), rustel_core::materialize_js_value(value))])
    });
    derive_wrapper(ctx, pattern, &sidecars)
}

pub(super) fn parray<'js>(ctx: Ctx<'js>, value: rquickjs::Value<'js>) -> NativeResult<'js> {
    if value.as_array().is_none() {
        return Err(rquickjs::Exception::throw_type(
            &ctx,
            "parray expects an array",
        ));
    }
    let (pattern, sidecars) = reify_pattern_list(&ctx, &value)?;
    derive_wrapper(ctx, pattern, &sidecars)
}

pub(super) fn fx<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> NativeResult<'js> {
    let (receiver, mut sidecars) = {
        let wrapper = this.0.borrow();
        (wrapper.pattern.clone(), vec![Sidecar::of(&wrapper)])
    };
    let values = choose_lookup(&ctx, &args.0)?;
    let (effects, mut nested) = reify_pattern_list(&ctx, &values.into_value())?;
    sidecars.append(&mut nested);
    let pattern = receiver.app_left_with_lookup(
        effects,
        |current, effects| {
            let current = rustel_core::materialize_js_value(current);
            let mut controls = match current {
                Value::Object(controls) => controls,
                _ => rustel_core::OrderedMap::new(),
            };
            let mut combined = match controls
                .get("FX")
                .cloned()
                .unwrap_or_else(|| Value::list(Vec::new()))
            {
                Value::List(values) => values.to_vec(),
                value => vec![value],
            };
            match rustel_core::materialize_js_value(effects) {
                Value::List(values) => combined.extend_from_slice(&values),
                value => combined.push(value),
            }
            controls.insert("FX".into(), Value::list(combined));
            Value::Object(controls)
        },
        rustel_core::LookupFlow::LeftControls,
    );
    derive_wrapper(ctx, pattern, &sidecars)
}
