use super::{patterns::choose_lookup, *};
fn distort_pattern(pattern: Pattern, args: Pattern, algorithm: &'static str) -> Pattern {
    let args = args.fmap(
        move |value| match rustel_core::materialize_js_value(value) {
            Value::List(mut values) => {
                values.push(Value::Str(algorithm.into()));
                Value::List(values)
            }
            value => Value::List(vec![value, Value::F64(1.0), Value::Str(algorithm.into())]),
        },
    );
    control_registry()
        .get("distort")
        .expect("distort control")
        .apply(&pattern, Some(args))
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
    let value = args
        .0
        .first()
        .cloned()
        .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()));
    let (args, sidecar) = reify_bridged(&ctx, &value)?;
    sidecars.push(sidecar);
    derive_wrapper(ctx, distort_pattern(receiver, args, algorithm), &sidecars)
}

pub(super) fn distort_free<'js>(
    ctx: Ctx<'js>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
    algorithm: &'static str,
) -> NativeResult<'js> {
    let value = args
        .0
        .first()
        .cloned()
        .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()));
    let (args, sidecar) = reify_bridged(&ctx, &value)?;
    let receiver = rustel_core::pure(Value::object(std::iter::empty()));
    derive_wrapper(ctx, distort_pattern(receiver, args, algorithm), &[sidecar])
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
        Value::List(values) => values,
        value => vec![value],
    };
    values.push(rustel_core::materialize_js_value(right));
    Value::List(values)
}

fn reify_pattern_list<'js>(
    ctx: &Ctx<'js>,
    value: &rquickjs::Value<'js>,
) -> rquickjs::Result<(Pattern, Vec<Sidecar<'js>>)> {
    let Some(array) = value.as_array() else {
        let (pattern, sidecar) = reify_bridged(ctx, value)?;
        return Ok((pattern, vec![sidecar]));
    };
    let mut result = rustel_core::pure(Value::List(Vec::new()));
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
    let pattern = receiver.app_left_with(effects, |current, effects| {
        let current = rustel_core::materialize_js_value(current);
        let mut controls = match current {
            Value::Object(controls) => controls,
            _ => rustel_core::OrderedMap::new(),
        };
        let mut combined = match controls
            .get("FX")
            .cloned()
            .unwrap_or(Value::List(Vec::new()))
        {
            Value::List(values) => values,
            value => vec![value],
        };
        match rustel_core::materialize_js_value(effects) {
            Value::List(values) => combined.extend(values),
            value => combined.push(value),
        }
        controls.insert("FX".into(), Value::List(combined));
        Value::Object(controls)
    });
    derive_wrapper(ctx, pattern, &sidecars)
}
