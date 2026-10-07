use super::*;
use rquickjs::function::Rest;

const DEFAULT_STORE: usize = 6;
const DEFAULT_TOKEN_PREFIX: &str = "\0rustel-private-voicing-default:";

#[derive(Clone)]
struct DefaultState {
    next: Rc<Cell<u64>>,
    flags: Rc<
        RefCell<std::collections::HashMap<String, std::sync::Arc<std::sync::atomic::AtomicBool>>>,
    >,
    any_retired: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

fn argument<'js>(
    ctx: &Ctx<'js>,
    args: &[rquickjs::Value<'js>],
    index: usize,
) -> rquickjs::Value<'js> {
    args.get(index)
        .cloned()
        .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()))
}

fn configure(
    function: &Function<'_>,
    name: &str,
    length: usize,
    constructible: bool,
) -> rquickjs::Result<()> {
    function.set_name(name)?;
    function.set_length(length)?;
    function.set_constructor(constructible);
    if constructible {
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
        )?;
    }
    Ok(())
}

fn current_registry<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<rquickjs::Object<'js>> {
    host_voicing_sets(ctx)?
        .get::<rquickjs::Value>(1)?
        .into_object()
        .ok_or_else(|| throw_type_error(ctx, "selected voicing registry is not an object"))
}

fn default_store<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<rquickjs::Object<'js>> {
    host_voicing_sets(ctx)?.get(DEFAULT_STORE)
}

fn copy_enumerable<'js>(
    ctx: &Ctx<'js>,
    target: &rquickjs::Object<'js>,
    source: rquickjs::Value<'js>,
) -> rquickjs::Result<()> {
    if source.is_null() || source.is_undefined() {
        return Ok(());
    }
    let source = if let Some(object) = source.as_object() {
        object.clone()
    } else {
        let object: Function = ctx.globals().get("Object")?;
        object.call((source,))?
    };
    let filter = rquickjs::object::Filter::new()
        .string()
        .symbol()
        .enum_only();
    for key in source.own_keys::<rquickjs::Atom>(filter) {
        let key = key?;
        target.set(key.clone(), source.get::<_, rquickjs::Value>(key)?)?;
    }
    Ok(())
}

fn shallow_registry_copy<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<rquickjs::Object<'js>> {
    let result = rquickjs::Object::new(ctx.clone())?;
    copy_enumerable(ctx, &result, current_registry(ctx)?.into_value())?;
    Ok(result)
}

fn discard_released(ctx: &Ctx<'_>, state: &DefaultState) -> rquickjs::Result<()> {
    use std::sync::atomic::Ordering;

    if !state.any_retired.swap(false, Ordering::AcqRel) {
        return Ok(());
    }
    let retired = state
        .flags
        .borrow()
        .iter()
        .filter_map(|(token, flag)| flag.load(Ordering::Acquire).then_some(token.clone()))
        .collect::<Vec<_>>();
    let store = default_store(ctx)?;
    let mut flags = state.flags.borrow_mut();
    for token in retired {
        store.remove(token.as_str())?;
        flags.remove(&token);
    }
    Ok(())
}

fn stringify<'js>(ctx: &Ctx<'js>, value: rquickjs::Value<'js>) -> rquickjs::Result<Option<String>> {
    ctx.json_stringify(value)?
        .map(|value| value.to_cstring().map(|value| value.as_str().to_owned()))
        .transpose()
}

fn register_native<'js>(
    ctx: &Ctx<'js>,
    name: rquickjs::Value<'js>,
    dictionary: rquickjs::Value<'js>,
    range: rquickjs::Value<'js>,
    add_signature: bool,
) -> rquickjs::Result<String> {
    let Some(name) = name.as_string() else {
        let function = if add_signature {
            "addVoicings"
        } else {
            "registerVoicings"
        };
        return Err(rquickjs::Exception::throw_message(
            ctx,
            &format!("{function}: the name must be a string"),
        ));
    };
    let name = name.clone().to_cstring()?.as_str().to_owned();
    let json = stringify(ctx, dictionary)?
        .ok_or_else(|| throw_type_error(ctx, "voicing dictionary is not JSON serializable"))?;
    let range_json = if range.is_undefined() {
        None
    } else {
        Some(
            stringify(ctx, range)?
                .ok_or_else(|| throw_type_error(ctx, "voicing range is not JSON serializable"))?,
        )
    };
    rustel_core::voicings::register_user_dict_json_with_range(
        &name,
        &json,
        range_json.as_deref(),
        add_signature,
    )
    .map_err(|message| throw_type_error(ctx, &message))?;
    Ok(name)
}

fn edo<'js>(ctx: Ctx<'js>, name: rquickjs::Value<'js>) -> rquickjs::Result<rquickjs::Array<'js>> {
    let name = rquickjs::Coerced::<String>::from_js(&ctx, name)?.0;
    let values = rustel_core::xen::edo(&name)
        .map_err(|message| rquickjs::Exception::throw_message(&ctx, &message))?;
    let result = rquickjs::Array::new(ctx.clone())?;
    for (index, value) in values.into_iter().enumerate() {
        result.set(index, value)?;
    }
    Ok(result)
}

fn install_edo(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
    let edo = Function::new(ctx.clone(), edo)?;
    configure(&edo, "edo", 1, true)?;
    set_host_global(ctx, "edo", edo)?;
    set_host_global(ctx, "packageName", "@strudel/edo")
}

fn set_default<'js>(
    state: &DefaultState,
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let value = argument(&ctx, &args.0, 0);
    discard_released(&ctx, state)?;
    if let Some(name) = value.as_string() {
        rustel_core::voicings::set_default_voicings(name.clone().to_cstring()?.as_str());
    } else {
        let index = state.next.get();
        state.next.set(index.wrapping_add(1));
        let token = format!("{DEFAULT_TOKEN_PREFIX}{index}");
        default_store(&ctx)?.set(token.as_str(), value.clone())?;
        let retired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        state
            .flags
            .borrow_mut()
            .insert(token.clone(), std::sync::Arc::clone(&retired));
        let lease = rustel_core::settings::VoicingDictionaryLease::new(
            retired,
            std::sync::Arc::clone(&state.any_retired),
        );
        rustel_core::voicings::set_default_voicings_with_lease(token, lease);
    }
    discard_released(&ctx, state)?;
    Ok(value)
}

fn fork_registry<'js>(ctx: Ctx<'js>) -> rquickjs::Result<rquickjs::Object<'js>> {
    shallow_registry_copy(&ctx)
}

fn select_registry<'js>(ctx: Ctx<'js>, registry: rquickjs::Value<'js>) -> rquickjs::Result<()> {
    host_voicing_sets(&ctx)?.set(1, registry)
}

fn add_voicings<'js>(ctx: Ctx<'js>, args: Rest<rquickjs::Value<'js>>) -> rquickjs::Result<()> {
    let name = argument(&ctx, &args.0, 0);
    let dictionary = argument(&ctx, &args.0, 1);
    let range = match args.0.get(2) {
        Some(value) if !value.is_undefined() => value.clone(),
        _ => {
            let value = rquickjs::Array::new(ctx.clone())?;
            value.set(0, "F3")?;
            value.set(1, "A4")?;
            value.into_value()
        }
    };
    let name = register_native(&ctx, name, dictionary.clone(), range.clone(), true)?;
    let entry = rquickjs::Object::new(ctx.clone())?;
    entry.set("dictionary", dictionary)?;
    entry.set("range", range)?;
    current_registry(&ctx)?.set(name, entry)
}

fn register_voicings<'js>(ctx: Ctx<'js>, args: Rest<rquickjs::Value<'js>>) -> rquickjs::Result<()> {
    let name = argument(&ctx, &args.0, 0);
    let dictionary = argument(&ctx, &args.0, 1);
    let options = match args.0.get(2) {
        Some(value) if !value.is_undefined() => value.clone(),
        _ => rquickjs::Object::new(ctx.clone())?.into_value(),
    };
    let range = if options.is_null() || options.is_undefined() {
        rquickjs::Value::new_undefined(ctx.clone())
    } else if let Some(object) = options.as_object() {
        object.get("range")?
    } else {
        let object: Function = ctx.globals().get("Object")?;
        object
            .call::<_, rquickjs::Object>((options.clone(),))?
            .get("range")?
    };
    let name = register_native(&ctx, name, dictionary.clone(), range, false)?;
    let entry = rquickjs::Object::new(ctx.clone())?;
    entry.set("dictionary", dictionary)?;
    copy_enumerable(&ctx, &entry, options)?;
    current_registry(&ctx)?.set(name, entry)
}

fn set_voicing_range<'js>(
    ctx: Ctx<'js>,
    name: rquickjs::Value<'js>,
    range: rquickjs::Value<'js>,
) -> rquickjs::Result<()> {
    let entry: rquickjs::Object = current_registry(&ctx)?.get(name.clone())?;
    let dictionary: rquickjs::Value = entry.get("dictionary")?;
    add_voicings(ctx, Rest(vec![name, dictionary, range]))
}

fn voicing_alias<'js>(
    ctx: Ctx<'js>,
    symbol: rquickjs::Value<'js>,
    alias: rquickjs::Value<'js>,
    sets: rquickjs::Value<'js>,
) -> rquickjs::Result<()> {
    let sets = if let Some(array) = sets.as_array() {
        array.clone()
    } else {
        let array = rquickjs::Array::new(ctx.clone())?;
        array.set(0, sets)?;
        array
    };
    // A length claim, even for the one-set copy above (see `js_array_len`):
    // read through the guard and charged before the walk.
    let len = js_array_len(&sets)?;
    charge_js_array::<rquickjs::Value>(&ctx, len)?;
    for index in 0..len {
        let set: rquickjs::Object = sets.get(index)?;
        let value: rquickjs::Value = set.get(symbol.clone())?;
        set.set(alias.clone(), value)?;
    }
    Ok(())
}

pub(super) fn install(ctx: &Ctx<'_>) -> Result<(), String> {
    install_edo(ctx)
        .map_err(|error| format!("xen glue globals: {}", describe_js_error(ctx, error)))?;

    let globals = ctx.globals();
    let scope: rquickjs::Object = globals
        .get("rustelScope")
        .map_err(|error| error.to_string())?;
    let parsed = ctx
        .json_parse(rustel_core::voicings::registry_json())
        .map_err(|error| describe_js_error(ctx, error))?;
    let registry: rquickjs::Object = parsed
        .into_object()
        .ok_or("voicing registry JSON is not an object")?
        .get("registry")
        .map_err(|error| describe_js_error(ctx, error))?;
    let store = rquickjs::Object::new(ctx.clone()).map_err(|error| error.to_string())?;
    store
        .set_prototype(None)
        .map_err(|error| error.to_string())?;

    let state = DefaultState {
        next: Rc::new(Cell::new(0)),
        flags: Rc::new(RefCell::new(std::collections::HashMap::new())),
        any_retired: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };

    let default_state = state.clone();
    let set_default = Function::new(ctx.clone(), move |ctx, args| {
        set_default(&default_state, ctx, args)
    })
    .map_err(|error| error.to_string())?;
    configure(&set_default, "setDefaultVoicings", 1, false).map_err(|error| error.to_string())?;

    let reset_state = state.clone();
    let reset = Function::new(ctx.clone(), move |ctx: Ctx<'_>| -> rquickjs::Result<()> {
        discard_released(&ctx, &reset_state)?;
        rustel_core::voicings::reset_voicings();
        discard_released(&ctx, &reset_state)
    })
    .map_err(|error| error.to_string())?;
    configure(&reset, "resetVoicings", 0, false).map_err(|error| error.to_string())?;

    let sync_state = state.clone();
    let sync = Function::new(ctx.clone(), move |ctx: Ctx<'_>| -> rquickjs::Result<()> {
        discard_released(&ctx, &sync_state)?;
        if !rustel_core::voicings::selected_default_voicings_is_host_owned() {
            return Ok(());
        }
        let token = rustel_core::voicings::selected_default_voicings();
        let store = default_store(&ctx)?;
        if !store.contains_key(token.as_str())? {
            return rustel_core::voicings::sync_host_default_json(&token, None)
                .map_err(|message| throw_type_error(&ctx, &message));
        }
        let value: rquickjs::Value = store.get(token.as_str())?;
        let json = match stringify(&ctx, value) {
            Ok(json) => json,
            Err(rquickjs::Error::Exception) => {
                let _ = ctx.catch();
                None
            }
            Err(error) => return Err(error),
        };
        rustel_core::voicings::sync_host_default_json(&token, json.as_deref())
            .map_err(|message| throw_type_error(&ctx, &message))?;
        discard_released(&ctx, &sync_state)
    })
    .map_err(|error| error.to_string())?;
    configure(&sync, "syncDefaultVoicings", 0, false).map_err(|error| error.to_string())?;

    let fork = Function::new(ctx.clone(), fork_registry).map_err(|error| error.to_string())?;
    configure(&fork, "forkVoicingRegistry", 0, false).map_err(|error| error.to_string())?;
    let select = Function::new(ctx.clone(), select_registry).map_err(|error| error.to_string())?;
    configure(&select, "selectVoicingRegistry", 1, false).map_err(|error| error.to_string())?;

    let add = Function::new(ctx.clone(), add_voicings).map_err(|error| error.to_string())?;
    configure(&add, "addVoicings", 2, false).map_err(|error| error.to_string())?;

    let register =
        Function::new(ctx.clone(), register_voicings).map_err(|error| error.to_string())?;
    configure(&register, "registerVoicings", 2, false).map_err(|error| error.to_string())?;

    let set_range =
        Function::new(ctx.clone(), set_voicing_range).map_err(|error| error.to_string())?;
    configure(&set_range, "setVoicingRange", 2, false).map_err(|error| error.to_string())?;

    let alias = Function::new(ctx.clone(), voicing_alias).map_err(|error| error.to_string())?;
    configure(&alias, "voicingAlias", 3, false).map_err(|error| error.to_string())?;

    let voicing_sets = host_voicing_sets(ctx).map_err(|error| describe_js_error(ctx, error))?;
    for (index, value) in [
        registry.clone().into_value(),
        registry.clone().into_value(),
        rquickjs::Value::new_null(ctx.clone()),
        fork.into_value(),
        select.into_value(),
        sync.into_value(),
        store.into_value(),
    ]
    .into_iter()
    .enumerate()
    {
        voicing_sets
            .set(index, value)
            .map_err(|error| describe_js_error(ctx, error))?;
    }

    for (name, value) in [
        ("setDefaultVoicings", set_default.into_value()),
        ("resetVoicings", reset.into_value()),
        ("addVoicings", add.into_value()),
        ("registerVoicings", register.into_value()),
        ("setVoicingRange", set_range.into_value()),
        ("voicingAlias", alias.into_value()),
        ("voicingRegistry", registry.into_value()),
    ] {
        set_host_global(ctx, name, value).map_err(|error| describe_js_error(ctx, error))?;
    }
    for name in [
        "voicings",
        "rootNotes",
        "voicing",
        "setDefaultVoicings",
        "resetVoicings",
    ] {
        let value: rquickjs::Value = globals.get(name).map_err(|error| error.to_string())?;
        scope
            .set(name, value)
            .map_err(|error| describe_js_error(ctx, error))?;
    }
    Ok(())
}
