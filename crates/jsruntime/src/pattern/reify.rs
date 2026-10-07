use super::*;

/// Reconstruct one parser-insensitive argument captured by a tagged native
/// combinator reference.
///
/// Callers must first reject any deferred string (and any unbridged function).
/// User-facing inputs go through [`reify_bridged`], which owns the mutable
/// lexical string-parser contract and callback sidecars.
///
/// `remaining` is the reconstruction's one element budget (see
/// [`combinator_reference`]). `None` means this argument cannot be rebuilt
/// within it, and the whole reference fails closed.
pub(crate) fn reify_native_reference_arg(
    value: &rquickjs::Value<'_>,
    remaining: &Cell<usize>,
) -> Option<Pattern> {
    if let Some((pattern, _)) = unwrap_pattern(value) {
        return Some(pattern);
    }
    if let Some(object) = value.as_object() {
        // A combinator reference or partial application - `every(4, rev)`,
        // `jux(fast(2))`. The curry wrapper installed below tags itself with
        // the combinator name and the arguments bound so far, so the reference
        // can be turned back into a native transformer instead of degrading to
        // `pure(<object>)`.
        if let Some(func) = combinator_reference_within(object.clone(), remaining) {
            return Some(rustel_core::pure(Value::Function(func)));
        }
        // A tagged function that could not be rebuilt fails closed with its
        // caller rather than degrading to the object form of the function.
        if value.is_function() {
            return None;
        }
    }
    // The curry binds its arguments by reference, so a bound array's length
    // is whatever the score claimed (`jux(fast(new Array(2 ** 31)))`): it is
    // mirrored under the shared budget, and past it this refuses.
    from_js_within(value, remaining).map(rustel_core::pure)
}

/// Reify every argument with [`reify_bridged`] and keep one sidecar per
/// argument.
///
/// A tagged combinator reference such as `rev` in `every(4, rev)` needs no
/// cell. An arrow function such as `every(4, x => x.fast(2))` becomes a
/// callback cell in its sidecar.
pub(crate) fn reify_args<'js>(
    ctx: &Ctx<'js>,
    args: &[rquickjs::Value<'js>],
) -> rquickjs::Result<(Vec<Pattern>, Vec<Sidecar<'js>>)> {
    let mut pats = Vec::with_capacity(args.len());
    let mut sidecars = Vec::with_capacity(args.len());
    for arg in args {
        let (pattern, sidecar) = reify_bridged(ctx, arg)?;
        pats.push(pattern);
        sidecars.push(sidecar);
    }
    Ok((pats, sidecars))
}

/// Reify one argument, bridging a user-authored function into a callback cell.
///
/// A JS function that is NOT a registered-combinator reference becomes an
/// opaque `CallbackId` plus a cell that the resulting wrapper owns. The id
/// travels inside `Value::Function`, so `rustel-core` never holds a JSValue -
/// it holds an index it cannot dereference without the host.
///
/// Everything reachable this way is classified IMPURE, which is what keeps
/// `every(4, x => x.fast(2))` off the tight-lookahead path.
pub(crate) fn reify_bridged<'js>(
    ctx: &Ctx<'js>,
    value: &rquickjs::Value<'js>,
) -> rquickjs::Result<(Pattern, Sidecar<'js>)> {
    if let Some((pattern, sidecar)) = unwrap_pattern(value) {
        return Ok((pattern, sidecar));
    }
    if value.is_string() {
        let stack = host_stack(ctx)?;
        let lexical_reify: Function = stack.as_object().get(LEXICAL_REIFY)?;
        let parsed: rquickjs::Value = lexical_reify.call((value.clone(),))?;
        let Some((pattern, sidecar)) = unwrap_pattern(&parsed) else {
            return Err(throw_type_error(
                ctx,
                "the configured string parser did not return a Pattern",
            ));
        };
        // Do not reify the parser result a second time: it is returned
        // verbatim, and callback cells belong to this exact wrapper.
        return Ok((pattern, sidecar));
    }
    if let Some(object) = value.as_object()
        && !value.is_function()
        && object
            .get::<_, rquickjs::Value>("d")
            .is_ok_and(|d| d.as_big_int().is_some())
        && object
            .get::<_, rquickjs::Value>("n")
            .is_ok_and(|n| n.as_big_int().is_some())
        && object.contains_key("s").unwrap_or(false)
    {
        // A bridge Fraction argument (`zoom(Fraction(1).div(3), …)`).
        // `Number(fraction)` then the Farey search in `Fraction::from_f64`
        // restores the exact rational - 1/3 stays 1/3, not 333333/1000000.
        let number: Function = ctx.globals().get("Number")?;
        let coerced: f64 = number.call((value.clone(),))?;
        return Ok((rustel_core::pure(Value::F64(coerced)), Sidecar::default()));
    }
    if value.is_function() {
        if let Some(object) = value.as_object()
            && let Some(reference) = combinator_reference(object.clone())
        {
            // A native combinator reference needs no cell at all.
            return Ok((
                rustel_core::pure(Value::Function(reference)),
                Sidecar::default(),
            ));
        }
        let function = value
            .as_function()
            .ok_or_else(|| {
                rquickjs::Error::new_from_js_message("value", "Function", "not callable")
            })?
            .clone();
        let (function, sidecar) = bridge_pattern_callable(ctx, function)?;
        return Ok((rustel_core::pure(Value::Function(function)), sidecar));
    }
    let (bridged, nested) = materialize_js_value(ctx, value)?;
    let pattern = rustel_core::pure(bridged);
    let sidecar = Sidecar::merge_all(nested).map_err(ownership_set_error_to_js)?;
    Ok((pattern, sidecar))
}

/// Reify `echoWith`'s callback argument without losing the exact callable that
/// receives its second, indexed argument.
///
/// Reconstructing a tagged patternified reference such as `fast(2)` as the
/// ordinary unary native transformer is observably wrong here: the
/// curry/pfunc sees the extra index, takes its variadic general path, and the
/// resulting query fails when a Pattern value is used as the next `appLeft`
/// function. Keep those callables in JavaScript so its own curry defines the
/// behavior. The sole unpatternified registered reference is `rev`; its
/// declared unary body ignores extra arguments, so the native unary
/// representation remains exact and retains its purity proof.
pub(crate) fn reify_indexed_transformer<'js>(
    ctx: &Ctx<'js>,
    value: &rquickjs::Value<'js>,
) -> rquickjs::Result<(Pattern, Sidecar<'js>)> {
    let Some(function) = value.as_function() else {
        return reify_bridged(ctx, value);
    };

    if let Some(object) = value.as_object() {
        let native_unpatternified = object
            .get::<_, rquickjs::Object>("__rustel_combinator")
            .ok()
            .and_then(|tag| {
                let name: String = tag.get("name").ok()?;
                let bound: rquickjs::Array = tag.get("args").ok()?;
                // A forged tag can name any args array (see
                // `js_array_len`). A huge claim can pass this probe;
                // `combinator_reference` then refuses it, and the function
                // is bridged.
                let bound_len = js_array_len(&bound).ok()?;
                let registration = registry().get(&name)?;
                (!registration.patternify && bound_len.saturating_add(1) >= registration.arity)
                    .then_some(())
            })
            .is_some();
        if native_unpatternified && let Some(reference) = combinator_reference(object.clone()) {
            return Ok((
                rustel_core::pure(Value::Function(reference)),
                Sidecar::default(),
            ));
        }
    }

    let (id, sidecar) = bridge_callable(ctx, function.clone())?;
    Ok((
        rustel_core::pure(Value::Function(rustel_core::value::FunctionRef::js(id))),
        sidecar,
    ))
}

pub(crate) fn reify_registered_args<'js>(
    ctx: &Ctx<'js>,
    args: &[rquickjs::Value<'js>],
    indexed_transformer: bool,
) -> rquickjs::Result<(Vec<Pattern>, Vec<Sidecar<'js>>)> {
    let mut patterns = Vec::with_capacity(args.len());
    let mut sidecars = Vec::with_capacity(args.len());
    for (index, argument) in args.iter().enumerate() {
        let (pattern, sidecar) = if indexed_transformer && index == 2 {
            reify_indexed_transformer(ctx, argument)?
        } else {
            reify_bridged(ctx, argument)?
        };
        patterns.push(pattern);
        sidecars.push(sidecar);
    }
    Ok((patterns, sidecars))
}

/// Invoke the original lexical `reify` exactly, preserving the returned
/// wrapper's JS-owned value identity.
///
/// Native combinator arguments often need [`reify_bridged`] to materialize an
/// object for Rust semantics. Direct `reify` sites do not: arrays,
/// objects, and tagged functions remain the exact JS values stored by `pure`.
pub(crate) fn reify_direct_bridged<'js>(
    ctx: &Ctx<'js>,
    value: &rquickjs::Value<'js>,
) -> rquickjs::Result<(Pattern, Sidecar<'js>)> {
    let stack = host_stack(ctx)?;
    let lexical_reify: Function = stack.as_object().get(LEXICAL_REIFY)?;
    let reified: rquickjs::Value = lexical_reify.call((value.clone(),))?;
    unwrap_pattern(&reified).ok_or_else(|| {
        throw_type_error(ctx, "the configured string parser did not return a Pattern")
    })
}

/// Invoke the original `reify` closure and return its exact JavaScript value.
///
/// The public list constructors use this as a captured host rather than
/// consulting the replaceable `globalThis.reify`. Keeping the wrapper value
/// intact is observable for `slowcat(p) === p` and for the recursively nested
/// singleton forms `slowcat([p])` / `slowcat([[p]])`.
pub(crate) fn reify_list_value<'js>(
    ctx: Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let stack = host_stack(&ctx)?;
    let lexical_reify: Function = stack.as_object().get(LEXICAL_REIFY)?;
    lexical_reify.call((value,))
}

/// `reify` as the LIST CONSTRUCTORS apply it.
///
/// ```js
/// export function slowcat(...pats) {
///   pats = pats.map((pat) => (Array.isArray(pat) ? fastcat(...pat) : reify(pat)));
/// }
/// ```
/// A nested ARRAY is a sub-sequence, not a value: `stack("g3", "b3", ["e4",
/// "d4"])` is `"g3,b3,[e4 d4]"`.
/// List-constructor reification with JavaScript value ownership.
///
/// `fastcat(f, g)` is user-visible input to `arpWith`, so every leaf must keep
/// its callback cells. Every leaf enters the original lexical `reify`, so
/// primitive strings use the configured parser while other JavaScript values
/// retain exact identity; nested arrays retain fastcat recursion.
pub(crate) fn reify_list_element_bridged<'js>(
    ctx: &Ctx<'js>,
    value: &rquickjs::Value<'js>,
) -> rquickjs::Result<(Pattern, Vec<Sidecar<'js>>)> {
    let remaining = Cell::new(js_element_cap::<Pattern>(ctx)?);
    reify_list_element_within(ctx, value, 0, &remaining)
}

/// [`reify_list_element_bridged`] charging the element budget its whole
/// nested-array recursion shares: every level's slots stay reserved until the
/// levels below return, so a copy that fits the cap alone must not let the
/// tree sum past it. `depth` counts the enclosing arrays and is refused at
/// [`MAX_LIST_DEPTH`], which also stops a self-containing array.
fn reify_list_element_within<'js>(
    ctx: &Ctx<'js>,
    value: &rquickjs::Value<'js>,
    depth: usize,
    remaining: &Cell<usize>,
) -> rquickjs::Result<(Pattern, Vec<Sidecar<'js>>)> {
    if depth == MAX_LIST_DEPTH {
        return Err(rquickjs::Exception::throw_range(
            ctx,
            "nested list depth exceeds the native limit",
        ));
    }
    let stack = host_stack(ctx)?;
    let array_items: Function = stack.as_object().get(ARRAY_ITEMS)?;
    let items: rquickjs::Value = array_items.call((value.clone(),))?;
    if !items.is_undefined() {
        let array = items.as_array().ok_or_else(|| {
            rquickjs::Error::new_from_js_message(
                "array spread",
                "Array",
                "private Array.isArray helper returned a non-array",
            )
        })?;
        // A host-filled spread copy is still a length claim (see
        // `js_array_len`): charged to the tree's budget before the host Vec.
        let len = js_array_len(array)?;
        let mut pats = reserve_js_elements::<Pattern>(ctx, len, remaining)?;
        let mut sidecars = Vec::new();
        for index in 0..len {
            let item = array.get::<rquickjs::Value>(index)?;
            let (pattern, nested) = reify_list_element_within(ctx, &item, depth + 1, remaining)?;
            pats.push(pattern);
            sidecars.extend(nested);
        }
        return Ok((rustel_core::fastcat(pats), sidecars));
    }
    let (pattern, sidecar) = reify_direct_bridged(ctx, value)?;
    Ok((pattern, vec![sidecar]))
}

/// Does this value contain a JavaScript function this host cannot bridge?
///
/// The check recurses through partial applications. `every(4, x => x)` is
/// itself a tagged combinator reference, so a shallow "is it tagged?" test
/// accepts it. The arrow function inside is then reified as a plain object,
/// and the combinator behaves as the identity:
/// `jux(every(2, x => x.fast(2)))` would return plausible, wrong haps.
///
/// Every args array walked is charged against `remaining`, the
/// reconstruction's one element budget (see [`combinator_reference`]).
pub(crate) fn requires_callable_bridge(
    value: &rquickjs::Value<'_>,
    depth: usize,
    remaining: &Cell<usize>,
) -> bool {
    if depth >= 64 {
        return true;
    }
    // Reconstructing a tagged native reference would retain only the Rust
    // graph and drop this exact wrapper's callback cells, writable own query,
    // and JS-visible metadata. Keep the callable intact whenever one of its
    // captured arguments is a Pattern.
    if unwrap_pattern(value).is_some() {
        return true;
    }
    if !value.is_function() {
        return false;
    }
    let Some(object) = value.as_object() else {
        return true;
    };
    let Ok(tag) = object.get::<_, rquickjs::Object>("__rustel_combinator") else {
        // A plain function: no tag, nothing to bridge it with.
        return true;
    };
    let Ok(bound) = tag.get::<_, rquickjs::Array>("args") else {
        return false;
    };
    any_genuine_bound_arg(&tag, &bound, remaining, |inner| {
        requires_callable_bridge(&inner, depth + 1, remaining)
    })
}

/// Does a tagged native transformer retain a raw string for a later curried
/// invocation?
///
/// Such a transformer cannot be reconstructed eagerly in Rust: the
/// then-current lexical `reify` runs only when the curry is finally saturated.
/// Bridging the exact JS callable preserves parser replacement and argument
/// order. Nested tagged transforms are inspected with a fail-closed depth cap;
/// ordinary arrays/objects are values and are not recursively patternified.
pub(crate) fn has_deferred_string(
    value: &rquickjs::Value<'_>,
    depth: usize,
    remaining: &Cell<usize>,
) -> bool {
    if value.is_string() {
        return true;
    }
    if depth >= 64 || !value.is_function() {
        return depth >= 64;
    }
    let Some(object) = value.as_object() else {
        return false;
    };
    let Ok(tag) = object.get::<_, rquickjs::Object>("__rustel_combinator") else {
        return false;
    };
    let Ok(bound) = tag.get::<_, rquickjs::Array>("args") else {
        return false;
    };
    any_genuine_bound_arg(&tag, &bound, remaining, |inner| {
        has_deferred_string(&inner, depth + 1, remaining)
    })
}

/// How many arguments a `__rustel_combinator` tag binds, if a genuine curry
/// could have made it.
///
/// The curry calls through once `arity` arguments are bound, so a genuine
/// tag names a registration and holds fewer than its arity. Anything else was
/// forged, and every walker refuses it on this answer alone, before a single
/// element is read.
fn genuine_bound_len(name: &str, bound: &rquickjs::Array<'_>) -> Option<usize> {
    let len = js_array_len(bound).ok()?;
    (len < registry().get(name)?.arity).then_some(len)
}

/// A tag's bound arguments, when a genuine curry could have made the tag and
/// the reconstruction's budget covers them; `None` fails closed.
///
/// The tag is a plain property score code can forge, so its args array is
/// untrusted: one no genuine curry could have made (see
/// [`genuine_bound_len`]), or one past `remaining`, is refused before a
/// single element is read. The elements are then read by index, never
/// through `iter()` (see `js_array_len`). Failed element reads are skipped.
fn genuine_bound_args<'a, 'js>(
    name: &str,
    bound: &'a rquickjs::Array<'js>,
    remaining: &Cell<usize>,
) -> Option<impl Iterator<Item = rquickjs::Value<'js>> + use<'a, 'js>> {
    let len = genuine_bound_len(name, bound)?;
    if !try_charge_js_elements(len, remaining) {
        return None;
    }
    Some((0..len).filter_map(move |index| bound.get::<rquickjs::Value>(index).ok()))
}

/// The fail-closed walk both tag predicates share: whether `hit` holds for
/// any of the tag's bound arguments - and `true` outright when they cannot be
/// walked (see [`genuine_bound_args`]), so a forged tag keeps the function
/// off the native path.
fn any_genuine_bound_arg<'js>(
    tag: &rquickjs::Object<'js>,
    bound: &rquickjs::Array<'js>,
    remaining: &Cell<usize>,
    hit: impl FnMut(rquickjs::Value<'js>) -> bool,
) -> bool {
    let name: Option<String> = tag.get("name").ok();
    name.and_then(|name| genuine_bound_args(&name, bound, remaining))
        .is_none_or(|mut args| args.any(hit))
}

/// Read the `__rustel_combinator` tag off an installed combinator function.
///
/// Argument-array walks and arrays mirrored by `from_js_within` share one
/// element budget, including arrays reached through object properties.
/// Exceeding it leaves the function on the ordinary callback path.
pub(crate) fn combinator_reference(
    object: rquickjs::Object<'_>,
) -> Option<rustel_core::value::FunctionRef> {
    let remaining = Cell::new(js_element_cap::<Value>(object.ctx()).ok()?);
    combinator_reference_within(object, &remaining)
}

/// [`combinator_reference`] charging a budget shared with the tag it is
/// nested in.
fn combinator_reference_within(
    object: rquickjs::Object<'_>,
    remaining: &Cell<usize>,
) -> Option<rustel_core::value::FunctionRef> {
    let tag: rquickjs::Object = object.get("__rustel_combinator").ok()?;
    let name: String = tag.get("name").ok()?;
    let bound: rquickjs::Array = tag.get("args").ok()?;
    let registration = registry().get(&name)?.clone();
    // Defence in depth: never build a transformer around an argument that
    // cannot be bridged. `reify_args` rejects these first, but
    // `superimpose`/`layer` reach this function directly.
    //
    // The tag is forgeable, so the args array is untrusted here too (see
    // `genuine_bound_args`): each bound argument would become a host
    // Pattern, far larger than the element the budget counts.
    let bound_values: Vec<rquickjs::Value> =
        genuine_bound_args(&name, &bound, remaining)?.collect();
    if bound_values
        .iter()
        .any(|value| requires_callable_bridge(value, 0, remaining))
        || bound_values
            .iter()
            .any(|value| has_deferred_string(value, 0, remaining))
    {
        return None;
    }
    let args = bound_values
        .iter()
        .map(|value| reify_native_reference_arg(value, remaining))
        .collect::<Option<Vec<Pattern>>>()?;
    // Provably pure only when the body is native AND the bound arguments are
    // pure; otherwise the transformer is applied through the conservative path.
    let provable_pure = matches!(
        registration.func,
        rustel_core::register::CombinatorFn::Native(_)
    ) && args.iter().all(Pattern::is_pure);
    let bound_args = args;
    Some(rustel_core::value::FunctionRef::registered(
        &name,
        std::sync::Arc::new(move |pattern| registration.call(&bound_args, pattern)),
        provable_pure,
    ))
}
