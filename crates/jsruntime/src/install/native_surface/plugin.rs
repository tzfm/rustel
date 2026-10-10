use super::*;

/// What one argument of a plugin call sets in the plugin object of a hap.
enum Field {
    /// The plugin name. It starts a new object. A second `vst` call adds
    /// its object after the first: the effects of a note are a chain, in
    /// call order. A second `vsti` call replaces the first.
    Name,
    /// A key of the object the score wrote. `preset` is the name of a
    /// preset file. Each other key is a parameter of the plugin.
    Key(String),
}

/// Sets one field of a plugin object of a hap. An instrument has one
/// object under `vsti`. The effects are a list under `vst`, and a key goes
/// to the last object of the list:
/// `vsti: { name, preset, params: { key: value } }`, `vst: [{ name }, { name }]`.
fn set_plugin_field(hap: &Value, call: &str, field: &Field, setting: &Value) -> Value {
    let Value::Object(mut controls) = rustel_core::materialize_js_value(hap) else {
        rustel_core::signal_query_error(|| format!("{call} expects object-valued haps"));
        return Value::Undefined;
    };
    let setting = rustel_core::materialize_js_value(setting);
    if matches!(setting, Value::Undefined) {
        return Value::Object(controls);
    }
    let chain = call == "vst";
    let mut effects = match controls.get(call) {
        Some(Value::List(effects)) if chain => effects.clone(),
        _ => Vec::new(),
    };
    let held = if chain {
        effects.pop()
    } else {
        controls.get(call).cloned()
    };
    let mut plugin = match (held, field) {
        (Some(Value::Object(plugin)), Field::Key(_)) => plugin,
        (held, _) => {
            // A new name keeps the effect before it in the chain.
            effects.extend(held.filter(|_| chain));
            rustel_core::OrderedMap::new()
        }
    };
    match field {
        Field::Name => {
            plugin.insert("name".into(), setting);
        }
        Field::Key(key) if key == "preset" => {
            plugin.insert("preset".into(), setting);
        }
        Field::Key(key) => {
            let mut params = match plugin.get("params") {
                Some(Value::Object(params)) => params.clone(),
                _ => rustel_core::OrderedMap::new(),
            };
            params.insert(key.clone(), setting);
            plugin.insert("params".into(), Value::Object(params));
        }
    }
    let plugin = Value::Object(plugin);
    let value = if chain {
        effects.push(plugin);
        Value::List(effects)
    } else {
        plugin
    };
    controls.insert(call.into(), value);
    Value::Object(controls)
}

/// `.vst(name, { param: value, preset: name })`: the note goes through an
/// effect plugin on its orbit. `.vsti(name, { ... })`: an instrument plugin
/// on the orbit makes the sound of the note. Each field of the object is a
/// pattern, so a slider or a mini-notation string gives one value to each
/// note.
pub(super) fn plugin_method<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
    call: &'static str,
) -> NativeResult<'js> {
    let (receiver, sidecars) = {
        let wrapper = this.0.borrow();
        (wrapper.pattern.clone(), vec![Sidecar::of(&wrapper)])
    };
    install_plugin(ctx, receiver, sidecars, args, call)
}

/// `vsti(name, { ... })` and `vst(name, { ... })` at the start of a chain:
/// one note for each cycle, with the plugin.
pub(super) fn plugin_free<'js>(
    ctx: Ctx<'js>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
    call: &'static str,
) -> NativeResult<'js> {
    let receiver = rustel_core::pure(Value::Object(rustel_core::OrderedMap::new()));
    install_plugin(ctx, receiver, Vec::new(), args, call)
}

fn install_plugin<'js>(
    ctx: Ctx<'js>,
    receiver: Pattern,
    mut sidecars: Vec<Sidecar<'js>>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
    call: &'static str,
) -> NativeResult<'js> {
    let name = args
        .0
        .first()
        .and_then(rquickjs::Value::as_string)
        .and_then(|name| name.to_string().ok())
        .filter(|name| !name.trim().is_empty());
    let Some(name) = name else {
        return Err(rquickjs::Exception::throw_type(
            &ctx,
            &format!("{call} expects the name of a plugin as its first argument"),
        ));
    };
    let name = Value::Str(name);
    let mut output = receiver.fmap(move |hap| set_plugin_field(hap, call, &Field::Name, &name));
    if let Some(config) = args.0.get(1).and_then(rquickjs::Value::as_object) {
        for entry in config.props::<String, rquickjs::Value>() {
            let (key, value) = entry?;
            let (value, sidecar) = reify_bridged(&ctx, &value)?;
            sidecars.push(sidecar);
            let field = Field::Key(key);
            // The note keeps the live links of its own controls.
            output = output.app_left_with_lookup(
                value,
                move |hap, setting| set_plugin_field(hap, call, &field, setting),
                rustel_core::LookupFlow::Left,
            );
        }
    }
    derive_wrapper(ctx, output, &sidecars)
}
