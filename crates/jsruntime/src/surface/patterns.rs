use super::*;

pub(crate) fn native_pure<'js>(
    ctx: Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Class<'js, NativePatternWrapper<'js>>> {
    // `pure(pattern)` is both a known pattern-of-pattern graph AND a public
    // exact-value promise: the input wrapper appears unchanged in `__pure`.
    // Keep the native `PurePattern` node so joins retain their established
    // pattern-valued failure boundary, then attach the exact JS value as the
    // ordinary own property the constructor creates. That property is
    // itself a GC edge, so it needs no frame-local callback id and also works
    // in the raw inspection evaluator which opens no BridgeFrame.
    if let Some((inner, sidecar)) = unwrap_pattern(&value) {
        let result = derive_wrapper(ctx, rustel_core::pure_pattern(inner), &[sidecar])?;
        let result_value = result.clone().into_value();
        let result_object = result_value
            .as_object()
            .expect("a native Pattern wrapper is an object");
        result_object.set("__pure", value)?;
        return Ok(result);
    }
    // `pure(fast(2))` stores that exact callable as a VALUE. Do not
    // reuse the combinator-reference optimisation from argument reification:
    // doing so reconstructs a Rust FunctionRef later and loses `===`, own
    // properties and closure identity. Ordinary `every(2, fast(2))` still
    // takes the native fast path through `reify_bridged`.
    if let Some(function) = value.as_function() {
        let (id, sidecar) = bridge_callable(&ctx, function.clone())?;
        return derive_wrapper(
            ctx,
            rustel_core::pure(Value::Function(rustel_core::value::FunctionRef::js(id))),
            &[sidecar],
        );
    }
    let (bridged, sidecars, _) = from_js_bridged_value(&ctx, &value)?;
    derive_wrapper(ctx, rustel_core::pure(bridged), &sidecars)
}

pub(crate) fn native_silence<'js>(
    ctx: Ctx<'js>,
) -> rquickjs::Result<rquickjs::Class<'js, NativePatternWrapper<'js>>> {
    new_wrapper(&ctx, NativePatternWrapper::plain(rustel_core::silence()))
}

pub(crate) fn native_gap<'js>(
    ctx: Ctx<'js>,
    steps: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Class<'js, NativePatternWrapper<'js>>> {
    let steps = if steps.is_undefined() {
        None
    } else {
        steps.as_number().and_then(Fraction::from_f64)
    };
    new_gap_wrapper(
        &ctx,
        NativePatternWrapper::plain(rustel_core::silence().with_steps(steps)),
    )
}

pub(crate) fn native_mini<'js>(
    ctx: Ctx<'js>,
    source: String,
    offset: i64,
) -> rquickjs::Result<rquickjs::Class<'js, NativePatternWrapper<'js>>> {
    let mut pattern =
        rustel_mini::mini_at(&source, usize::try_from(offset).unwrap_or(0)).map_err(|error| {
            rquickjs::Error::new_from_js_message("string", "Pattern", error.to_string())
        })?;
    if offset < 0 {
        pattern = pattern.map_haps_native(|hap| Some(hap.clone().with_context(Vec::new())));
    }
    new_wrapper(&ctx, NativePatternWrapper::plain(pattern))
}

/// Public `mini(...strings)`, distinct from transpiler-facing `m(str, offset)`.
///
/// Strudel installs `mini` as the mutable parser behind `miniAllStrings`, so
/// it must accept one argument (and its rest-argument surface has length zero).
/// Reusing `m` here made `setStringParser(mini)` fail before parsing anything.
///
/// The double-quotes plugin rewrites `mini("0 3 7")` to `mini(m("0 3 7", N))`.
/// `mini` always `` `${str}` ``-coerces, so that Pattern becomes
/// `"[object Object]"` and `note(mini("0 3 7")).add(note("<0 5>"))` queries
/// to zero haps (numeral parse throw inside `queryArc`). Sequence identity
/// for an already-built Pattern is the user-facing contract: one Pattern
/// argument is returned as-is, matching `sequence(p) === p`.
pub(crate) fn native_mini_sequence<'js>(
    ctx: Ctx<'js>,
    sources: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Class<'js, NativePatternWrapper<'js>>> {
    if sources.0.len() == 1
        && let Some(object) = sources.0[0].as_object()
        && let Some(wrapper) = rquickjs::Class::<NativePatternWrapper>::from_object(object)
    {
        return Ok(wrapper);
    }

    let mut patterns = Vec::with_capacity(sources.0.len());
    let mut sidecars = Vec::new();
    let stack = host_stack(&ctx)?;
    let coerce: Function = stack.as_object().get(MINI_STRING_COERCE)?;
    for source in sources.0 {
        if let Some((pattern, sidecar)) = unwrap_pattern(&source) {
            patterns.push(pattern);
            sidecars.push(sidecar);
            continue;
        }
        let source: String = coerce.call((source,))?;
        patterns.push(rustel_mini::mini(&source).map_err(|error| {
            rquickjs::Error::new_from_js_message("string", "Pattern", error.to_string())
        })?);
    }
    let pattern = if patterns.is_empty() {
        // Empty slowcat IS silence, so `sequence()`
        // carries silence's one-step metadata.
        rustel_core::silence()
    } else if patterns.len() == 1 {
        patterns.pop().expect("one mini argument")
    } else {
        rustel_core::fastcat(patterns)
    };
    if sidecars.is_empty() {
        new_wrapper(&ctx, NativePatternWrapper::plain(pattern))
    } else {
        derive_wrapper(ctx, pattern, &sidecars)
    }
}

pub(crate) fn native_wchoose<'js>(
    ctx: Ctx<'js>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
    cycles: bool,
) -> rquickjs::Result<rquickjs::Class<'js, NativePatternWrapper<'js>>> {
    let stack = host_stack(&ctx)?;
    let pair_at: Function = stack.as_object().get(WCHOOSE_PAIR_AT)?;

    // Two distinct passes: every pair[0] first, then every
    // pair[1]. Preserve that observable getter/proxy order.
    let mut values = Vec::with_capacity(args.0.len());
    let mut sidecars = Vec::with_capacity(args.0.len() * 2);
    for pair in &args.0 {
        if pair.is_null() || pair.is_undefined() {
            let kind = if pair.is_null() { "null" } else { "undefined" };
            return Err(throw_type_error(
                &ctx,
                &format!("Cannot read properties of {kind} (reading '0')"),
            ));
        }
        let value: rquickjs::Value = pair_at.call((pair.clone(), 0usize))?;
        let (value, value_sidecar) = reify_bridged(&ctx, &value)?;
        values.push(value);
        sidecars.push(value_sidecar);
    }
    let mut weights = Vec::with_capacity(args.0.len());
    for pair in &args.0 {
        let weight: rquickjs::Value = pair_at.call((pair.clone(), 1usize))?;
        if weight.is_null() || weight.is_undefined() {
            // `total.add(weight)` reaches register()'s step-source propagation
            // immediately, before the next pair's getter and before any query
            // exists. Preserve both the construction phase and the early
            // abort of the second accessor pass.
            let kind = if weight.is_null() {
                "null"
            } else {
                "undefined"
            };
            return Err(throw_type_error(
                &ctx,
                &format!("Cannot read properties of {kind} (reading '__steps_source')"),
            ));
        }
        // `reify` applies to VALUES, but WEIGHTS are reached through
        // `total.add(weight)`, whose composer calls `sequence([weight])`.
        // Consequently an array weight is a nested fastcat, not one list-valued
        // hap. This happens INSIDE the second pass before its next pair getter,
        // preserving nested array/proxy accessor order too. Keep bridging at
        // every leaf so function-valued weights retain their callback cells.
        let (weight, weight_sidecars) = reify_list_element_bridged(&ctx, &weight)?;
        weights.push(weight);
        sidecars.extend(weight_sidecars);
    }
    let pairs = values.into_iter().zip(weights).collect();
    derive_wrapper(ctx, rustel_core::wchoose(pairs, cycles), &sidecars)
}

pub(crate) fn native_pick_lookup<'js>(
    ctx: &Ctx<'js>,
    lookup: &rquickjs::Value<'js>,
) -> rquickjs::Result<(rustel_core::PickLookup, Vec<Sidecar<'js>>)> {
    let stack = host_stack(ctx)?;
    let shape_lookup: Function = stack.as_object().get(PICK_LOOKUP_SHAPE)?;
    let shape: rquickjs::Array = shape_lookup.call((lookup.clone(),))?;
    let is_array: bool = shape.get(0)?;
    let enumerable_len: usize = shape.get(1)?;
    let length: usize = shape.get(2)?;
    let entries: rquickjs::Array = shape.get(3)?;
    // `entries` is host-filled, or returned by `Object.entries`, which a
    // score can replace; either way its length is a claim (see
    // `js_array_len`): read through the guard, charged, and walked by index.
    let len = js_array_len(&entries)?;
    let mut sidecars = reserve_js_array(ctx, len)?;

    if is_array {
        let mut mapped = reserve_js_array(ctx, len)?;
        for entry in 0..len {
            let pair: rquickjs::Array = entries.get(entry)?;
            let index: usize = pair.get(0)?;
            let value: rquickjs::Value = pair.get(1)?;
            let (pattern, sidecar) = reify_bridged(ctx, &value)?;
            mapped.push((index, pattern));
            sidecars.push(sidecar);
        }
        Ok((
            rustel_core::PickLookup::Array {
                enumerable_len,
                length,
                entries: mapped,
            },
            sidecars,
        ))
    } else {
        let mut mapped = reserve_js_array(ctx, len)?;
        for entry in 0..len {
            let pair: rquickjs::Array = entries.get(entry)?;
            let key: String = pair.get(0)?;
            let value: rquickjs::Value = pair.get(1)?;
            let (pattern, sidecar) = reify_bridged(ctx, &value)?;
            mapped.push((key, pattern));
            sidecars.push(sidecar);
        }
        Ok((
            rustel_core::PickLookup::Object {
                enumerable_len,
                entries: mapped,
            },
            sidecars,
        ))
    }
}

pub(crate) fn native_squeeze_lookup<'js>(
    ctx: &Ctx<'js>,
    lookup: &rquickjs::Value<'js>,
) -> rquickjs::Result<(rustel_core::PickLookup, Vec<Sidecar<'js>>)> {
    let stack = host_stack(ctx)?;
    let shape_lookup: Function = stack.as_object().get(SQUEEZE_LOOKUP_SHAPE)?;
    let shape: rquickjs::Array = shape_lookup.call((lookup.clone(),))?;
    let length: usize = shape.get(0)?;
    let entries: rquickjs::Array = shape.get(1)?;
    // As `native_pick_lookup`: the host-filled `entries` length is a claim.
    let len = js_array_len(&entries)?;
    let mut mapped = reserve_js_array(ctx, len)?;
    let mut sidecars = reserve_js_array(ctx, len)?;
    for entry in 0..len {
        let pair: rquickjs::Array = entries.get(entry)?;
        let index: usize = pair.get(0)?;
        let value: rquickjs::Value = pair.get(1)?;
        let (pattern, sidecar) = reify_bridged(ctx, &value)?;
        mapped.push((index, pattern));
        sidecars.push(sidecar);
    }
    Ok((
        rustel_core::PickLookup::Array {
            enumerable_len: length,
            length,
            entries: mapped,
        },
        sidecars,
    ))
}

pub(crate) fn native_pick<'js>(
    ctx: Ctx<'js>,
    lookup_value: rquickjs::Value<'js>,
    selector_value: rquickjs::Value<'js>,
    index_mode: rustel_core::PickIndexMode,
    join_mode: rustel_core::JoinMode,
) -> rquickjs::Result<rquickjs::Class<'js, NativePatternWrapper<'js>>> {
    let (selector, selector_sidecar) = reify_bridged(&ctx, &selector_value)?;
    derive_pick(
        ctx,
        &lookup_value,
        selector,
        vec![selector_sidecar],
        index_mode,
        join_mode,
    )
}

pub(crate) fn derive_pick<'js>(
    ctx: Ctx<'js>,
    lookup_value: &rquickjs::Value<'js>,
    selector: Pattern,
    mut sidecars: Vec<Sidecar<'js>>,
    index_mode: rustel_core::PickIndexMode,
    join_mode: rustel_core::JoinMode,
) -> rquickjs::Result<rquickjs::Class<'js, NativePatternWrapper<'js>>> {
    if let Some(object) = lookup_value.as_object()
        && let Some(wrapper) = rquickjs::Class::<NativePatternWrapper>::from_object(object)
    {
        let borrowed = wrapper.borrow();
        if let Some(Value::JsValue(reference)) = borrowed.pattern.as_pure() {
            let sidecar = Sidecar::of(&borrowed);
            drop(borrowed);
            sidecars.push(sidecar);
            let (lookup, mut lookup_sidecars) =
                JsRuntime::with_js_value(&ctx, reference.id(), |value| {
                    native_pick_lookup(&ctx, &value).map_err(|error| error.to_string())
                })
                .map_err(|message| {
                    rquickjs::Error::new_from_js_message("JavaScript value", "pick lookup", message)
                })?;
            sidecars.append(&mut lookup_sidecars);
            return derive_wrapper(
                ctx,
                rustel_core::pick(selector, lookup, index_mode, join_mode),
                &sidecars,
            );
        }
        if let Some(lookup) = borrowed.pattern.as_pick_lookup() {
            sidecars.push(Sidecar::of(&borrowed));
            let mut pattern = rustel_core::pick(selector, lookup, index_mode, join_mode);
            if let Some(loc) = borrowed.pattern.pure_loc() {
                pattern = pattern.with_added_context(vec![loc]);
            }
            return derive_wrapper(ctx, pattern, &sidecars);
        }
        if let Some(value) = borrowed.pattern.as_pure()
            && let Some(lookup) = rustel_core::PickLookup::from_value(&value)
        {
            sidecars.push(Sidecar::of(&borrowed));
            let mut pattern = rustel_core::pick(selector, lookup, index_mode, join_mode);
            if let Some(loc) = borrowed.pattern.pure_loc() {
                pattern = pattern.with_added_context(vec![loc]);
            }
            return derive_wrapper(ctx, pattern, &sidecars);
        }
    }
    // register()'s fast-path predicate is loose `arg.__pure != undefined`.
    // Both null and undefined fail it and therefore reach `_pick` only at
    // query time through the patternified path, where queryArc makes the
    // Object.keys failure silent. Raw `_pick` does not take this branch.
    if lookup_value.is_null() || lookup_value.is_undefined() {
        let (lookup_pattern, lookup_sidecar) = reify_bridged(&ctx, lookup_value)?;
        sidecars.push(lookup_sidecar);
        return derive_wrapper(
            ctx,
            rustel_core::pick_patternified(selector, lookup_pattern, index_mode, join_mode),
            &sidecars,
        );
    }
    if let Some((lookup_pattern, lookup_sidecar)) = unwrap_pattern(lookup_value) {
        sidecars.push(lookup_sidecar);
        return derive_wrapper(
            ctx,
            rustel_core::pick_patternified(selector, lookup_pattern, index_mode, join_mode),
            &sidecars,
        );
    }
    let (lookup, mut lookup_sidecars) = native_pick_lookup(&ctx, lookup_value)?;
    sidecars.append(&mut lookup_sidecars);
    derive_wrapper(
        ctx,
        rustel_core::pick(selector, lookup, index_mode, join_mode),
        &sidecars,
    )
}

pub(crate) fn derive_pick_unpatternified<'js>(
    ctx: Ctx<'js>,
    lookup_value: &rquickjs::Value<'js>,
    selector: Pattern,
    mut sidecars: Vec<Sidecar<'js>>,
    index_mode: rustel_core::PickIndexMode,
    join_mode: rustel_core::JoinMode,
) -> rquickjs::Result<rquickjs::Class<'js, NativePatternWrapper<'js>>> {
    let (lookup, mut lookup_sidecars) = native_pick_lookup(&ctx, lookup_value)?;
    sidecars.append(&mut lookup_sidecars);
    derive_wrapper(
        ctx,
        rustel_core::pick(selector, lookup, index_mode, join_mode),
        &sidecars,
    )
}

pub(crate) fn derive_pick_f<'js>(
    ctx: Ctx<'js>,
    pick_value: &rquickjs::Value<'js>,
    lookup_value: &rquickjs::Value<'js>,
    receiver: Pattern,
    mut sidecars: Vec<Sidecar<'js>>,
    index_mode: rustel_core::PickIndexMode,
    compat_swap: bool,
) -> rquickjs::Result<rquickjs::Class<'js, NativePatternWrapper<'js>>> {
    if compat_swap {
        // The compatibility test belongs to the VALUE delivered per hap, not
        // to the construction-time argument. `segment(pure([rev]))` is not a
        // pure-view array but still yields arrays, and the swap applies. Keep
        // one metadata-aware native bind so mixed streams decide independently
        // without querying either argument twice.
        let (first, first_sidecar) = reify_bridged(&ctx, pick_value)?;
        let (second, second_sidecar) = reify_bridged(&ctx, lookup_value)?;
        sidecars.extend([first_sidecar, second_sidecar]);
        return derive_wrapper(
            ctx,
            rustel_core::pick_f_compat(receiver, first, second, index_mode),
            &sidecars,
        );
    }
    let (pick_value, lookup_value) = (pick_value, lookup_value);
    let (selector, selector_sidecar) = reify_bridged(&ctx, pick_value)?;
    sidecars.push(selector_sidecar);
    let functions = if let Some(object) = lookup_value.as_object()
        && let Some(wrapper) = rquickjs::Class::<NativePatternWrapper>::from_object(object)
        && let Some(lookup) = wrapper.borrow().pattern.as_pick_lookup()
    {
        let borrowed = wrapper.borrow();
        sidecars.push(Sidecar::of(&borrowed));
        let mut functions =
            rustel_core::pick(selector, lookup, index_mode, rustel_core::JoinMode::Inner);
        if let Some(loc) = borrowed.pattern.pure_loc() {
            functions = functions.with_added_context(vec![loc]);
        }
        functions
    } else if let Some((lookup_pattern, lookup_sidecar)) = unwrap_pattern(lookup_value) {
        if let Some(value) = lookup_pattern.as_pure()
            && let Some(lookup) = rustel_core::PickLookup::from_value(&value)
        {
            sidecars.push(lookup_sidecar);
            let mut functions =
                rustel_core::pick(selector, lookup, index_mode, rustel_core::JoinMode::Inner);
            if let Some(loc) = lookup_pattern.pure_loc() {
                functions = functions.with_added_context(vec![loc]);
            }
            functions
        } else {
            sidecars.push(lookup_sidecar);
            rustel_core::pick_patternified(
                selector,
                lookup_pattern,
                index_mode,
                rustel_core::JoinMode::Inner,
            )
        }
    } else {
        let (lookup, mut lookup_sidecars) = native_pick_lookup(&ctx, lookup_value)?;
        sidecars.append(&mut lookup_sidecars);
        rustel_core::pick(selector, lookup, index_mode, rustel_core::JoinMode::Inner)
    };
    derive_wrapper(
        ctx,
        rustel_core::apply_functions_strict(receiver, functions),
        &sidecars,
    )
}
