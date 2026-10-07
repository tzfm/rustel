use super::{controls::append_pattern_value, *};

fn modulator_subcontrol(kind: &str, key: &str) -> String {
    let key = key.to_lowercase();
    let canonical = match kind {
        "lfo" => match key.as_str() {
            "c" => "control",
            "sc" => "subControl",
            "r" => "rate",
            "dep" | "dr" => "depth",
            "da" => "depthabs",
            "dc" => "dcoffset",
            "sh" => "shape",
            "sk" => "skew",
            "cu" => "curve",
            "s" => "sync",
            "rt" => "retrig",
            _ => key.as_str(),
        },
        "env" => match key.as_str() {
            "c" => "control",
            "sc" => "subControl",
            "att" | "a" => "attack",
            "dec" | "d" => "decay",
            "sus" | "s" => "sustain",
            "rel" | "r" => "release",
            "dep" | "dr" => "depth",
            "da" => "depthabs",
            "ac" => "acurve",
            "dc" => "dcurve",
            "rc" => "rcurve",
            _ => key.as_str(),
        },
        "bmod" => match key.as_str() {
            "b" => "bus",
            "c" => "control",
            "sc" => "subControl",
            "dep" | "dr" => "depth",
            "da" => "depthabs",
            _ => key.as_str(),
        },
        _ => key.as_str(),
    };
    canonical.to_owned()
}

fn property_key(value: &Value) -> String {
    match rustel_core::materialize_js_value(value) {
        Value::Str(value) => value,
        Value::F64(value) => Value::F64(value).show(),
        Value::Bool(value) => value.to_string(),
        Value::Null => "null".into(),
        Value::Undefined => "undefined".into(),
        _ => "[object Object]".into(),
    }
}

fn canonical_control_value(value: &Value) -> Value {
    match rustel_core::materialize_js_value(value) {
        Value::Str(value) => Value::Str(
            control_registry()
                .canonical_name(&value)
                .unwrap_or(&value)
                .to_owned(),
        ),
        value => value,
    }
}

fn apply_modulator_value(
    pair: &Value,
    setting: &Value,
    kind: &'static str,
    key: &str,
    default: &std::sync::Mutex<Option<Value>>,
) -> Value {
    let Value::List(mut pair) = rustel_core::materialize_js_value(pair) else {
        rustel_core::signal_query_error(|| "invalid modulator state".into());
        return Value::Undefined;
    };
    if pair.len() < 2 {
        rustel_core::signal_query_error(|| "invalid modulator state".into());
        return Value::Undefined;
    }
    let mut controls = match pair.remove(0) {
        Value::Object(controls) => controls,
        _ => {
            rustel_core::signal_query_error(|| "modulate expects object-valued haps".into());
            return Value::Undefined;
        }
    };
    let mut id = pair.remove(0);
    let default_value = {
        let mut stored = default
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if stored.is_none() {
            let mut value = controls
                .js_entries()
                .last()
                .map(|(name, _)| {
                    Value::Str(
                        control_registry()
                            .canonical_name(name)
                            .unwrap_or(name)
                            .to_owned(),
                    )
                })
                .unwrap_or(Value::Undefined);
            if let Value::Str(control) = &value
                && matches!(control.as_str(), "lfo" | "env" | "bmod")
                && let Some(Value::Object(modulator)) = controls.get(control)
                && let Some((last, _)) = modulator
                    .js_entries()
                    .into_iter()
                    .rfind(|(name, _)| *name != "__ids")
            {
                value = Value::Str(format!("{control}_{last}"));
            }
            *stored = Some(value);
        }
        stored.clone().unwrap_or(Value::Undefined)
    };
    let mut modulator = match controls.get(kind).cloned() {
        Some(Value::Object(value)) => value,
        _ => rustel_core::OrderedMap::from_entries([(
            "__ids".into(),
            Value::Object(rustel_core::OrderedMap::new()),
        )]),
    };
    let ids: Vec<String> = modulator
        .js_entries()
        .into_iter()
        .filter_map(|(name, _)| (name != "__ids").then_some(name.to_owned()))
        .collect();
    if id.is_nullish() {
        id = Value::F64(ids.len() as f64);
    }
    let id_key = property_key(&id);
    let mut entry = match modulator.get(&id_key).cloned() {
        Some(Value::Object(entry)) => entry,
        _ => rustel_core::OrderedMap::from_entries([("control".into(), default_value)]),
    };
    if !matches!(setting, Value::Undefined) {
        let value = if key == "control" || key == "subControl" {
            canonical_control_value(setting)
        } else {
            rustel_core::materialize_js_value(setting)
        };
        entry.insert(key.into(), value);
    }
    modulator.insert(id_key, Value::Object(entry));
    controls.insert(kind.into(), Value::Object(modulator));
    Value::List(vec![Value::Object(controls), id])
}

fn install_modulator<'js>(
    ctx: Ctx<'js>,
    receiver: Pattern,
    mut sidecars: Vec<Sidecar<'js>>,
    kind: &'static str,
    config: rquickjs::Value<'js>,
    id: rquickjs::Value<'js>,
) -> NativeResult<'js> {
    let (id, id_sidecar) = reify_bridged(&ctx, &id)?;
    sidecars.push(id_sidecar);
    let mut entries = vec![(
        "control".to_owned(),
        rquickjs::Value::new_undefined(ctx.clone()),
    )];
    if let Some(object) = config.as_object() {
        for entry in object.props::<String, rquickjs::Value>() {
            let (key, value) = entry?;
            if let Some(current) = entries.iter_mut().find(|(name, _)| name == &key) {
                current.1 = value;
            } else {
                entries.push((key, value));
            }
        }
    }
    let default = std::sync::Arc::new(std::sync::Mutex::new(None));
    let mut output = receiver
        .fmap(|value| Value::List(vec![value.clone()]))
        .app_left_with(id, append_pattern_value);
    for (raw_key, value) in entries {
        let key = modulator_subcontrol(kind, &raw_key);
        let (value, sidecar) = reify_bridged(&ctx, &value)?;
        sidecars.push(sidecar);
        let default = std::sync::Arc::clone(&default);
        output = output.app_left_with(value, move |pair, setting| {
            apply_modulator_value(pair, setting, kind, &key, &default)
        });
    }
    let output = output.fmap(|value| match rustel_core::materialize_js_value(value) {
        Value::List(values) => values.first().cloned().unwrap_or(Value::Undefined),
        _ => Value::Undefined,
    });
    derive_wrapper(ctx, output, &sidecars)
}

pub(super) fn modulate<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> NativeResult<'js> {
    let (receiver, sidecar) = {
        let wrapper = this.0.borrow();
        (wrapper.pattern.clone(), Sidecar::of(&wrapper))
    };
    let kind = args
        .0
        .first()
        .and_then(rquickjs::Value::as_string)
        .and_then(|value| value.to_string().ok());
    let Some(kind) = kind.and_then(|kind| match kind.as_str() {
        "lfo" => Some("lfo"),
        "env" => Some("env"),
        "bmod" => Some("bmod"),
        _ => None,
    }) else {
        return derive_wrapper(ctx, receiver, &[sidecar]);
    };
    let config = args
        .0
        .get(1)
        .cloned()
        .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()));
    let id = args
        .0
        .get(2)
        .cloned()
        .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()));
    install_modulator(ctx, receiver, vec![sidecar], kind, config, id)
}

pub(super) fn modulator_method<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
    kind: &'static str,
) -> NativeResult<'js> {
    let (receiver, sidecar) = {
        let wrapper = this.0.borrow();
        (wrapper.pattern.clone(), Sidecar::of(&wrapper))
    };
    let config = args
        .0
        .first()
        .cloned()
        .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()));
    let id = args
        .0
        .get(1)
        .cloned()
        .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()));
    install_modulator(ctx, receiver, vec![sidecar], kind, config, id)
}

pub(super) fn modulator_free<'js>(
    ctx: Ctx<'js>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
    kind: &'static str,
) -> NativeResult<'js> {
    let config = args
        .0
        .first()
        .cloned()
        .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()));
    let id = rquickjs::Value::new_undefined(ctx.clone());
    install_modulator(
        ctx,
        rustel_core::pure(Value::Object(rustel_core::OrderedMap::new())),
        Vec::new(),
        kind,
        config,
        id,
    )
}
