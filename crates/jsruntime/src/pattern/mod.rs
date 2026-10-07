use super::*;

/// The native registries, built once. Both are `Send + Sync` (their bodies are
/// `Arc<dyn Fn … + Send + Sync>`), so a process-wide `OnceLock` is enough and
/// the ~1100 closures are not rebuilt per runtime.
pub(super) fn registry() -> &'static rustel_core::register::Registry {
    static REG: std::sync::OnceLock<rustel_core::register::Registry> = std::sync::OnceLock::new();
    #[cfg(feature = "extensions")]
    {
        REG.get_or_init(rustel_ext::default_registry)
    }
    #[cfg(not(feature = "extensions"))]
    {
        REG.get_or_init(rustel_core::register::default_registry)
    }
}

pub(super) fn control_registry() -> &'static rustel_core::controls::ControlRegistry {
    rustel_core::controls::default_control_registry()
}

/// Allocate a trace-managed cell for a callable, unconditionally.
///
/// `reify_bridged` deliberately converts a TAGGED installed function into a
/// native `FunctionRef` instead of a cell. `polyBind`/`stepBind` need a
/// value->pattern callable, which a native combinator reference is not, so
/// this path must always bridge the actual JS function it was handed.
pub(super) fn bridge_callable<'js>(
    ctx: &Ctx<'js>,
    function: Function<'js>,
) -> rquickjs::Result<(CallbackId, Sidecar<'js>)> {
    let id = with_bridge_frame(|frame: &BridgeFrame<'js>| frame.next_id()).ok_or_else(|| {
        rquickjs::Error::new_from_js_message(
            "function",
            "Pattern",
            "no bridge frame is open; a callback can only be created inside an \
             evaluation or a query",
        )
    })?;
    let cell = rquickjs::Class::instance(ctx.clone(), CallbackCell::function(function.clone()))?;
    CELLS_CREATED.with(|c| c.set(c.get() + 1));
    with_bridge_frame(|frame: &BridgeFrame<'js>| {
        frame.pending.borrow_mut().push((id, function.clone()));
    });
    Ok((id, Sidecar::one(id, cell)))
}

/// Bridge a `Pattern -> Pattern` callable and attach a proven native candidate
/// when its captured source is one of the closed callback-IR shapes.
///
/// The JavaScript function is bridged first and remains owned by the returned
/// sidecar in every case. Source inspection is an optional optimisation: any
/// extraction, parse, or IR-construction failure takes the complete QuickJS
/// path with the exact same callback id.
pub(super) fn bridge_pattern_callable<'js>(
    ctx: &Ctx<'js>,
    function: Function<'js>,
) -> rquickjs::Result<(rustel_core::value::FunctionRef, Sidecar<'js>)> {
    let (id, sidecar) = bridge_callable(ctx, function.clone())?;
    let fallback = || rustel_core::value::FunctionRef::js(id);

    let source = host_stack(ctx).and_then(|stack| {
        let source: Function = stack.as_object().get(CALLBACK_SOURCE)?;
        let mut args = rquickjs::function::Args::new_unsized(ctx.clone());
        args.this(function.into_value())?;
        args.apply::<String>(&source)
    });
    let Ok(source) = source else {
        return Ok((fallback(), sidecar));
    };
    let cache = host_pattern_transform_ir_cache(ctx).ok();
    let cached = cache.as_ref().and_then(|cache| cache.borrow().get(&source));
    let candidate = if let Some(candidate) = cached {
        candidate
    } else {
        let Ok(candidate) = rustel_transpiler::lower_pattern_transform_callback(&source) else {
            return Ok((fallback(), sidecar));
        };
        if let Some(cache) = &cache {
            cache.borrow_mut().insert(&source, candidate);
        }
        candidate
    };

    let program = match candidate {
        rustel_transpiler::PatternTransformCandidate::Identity => {
            rustel_core::callback_ir::PatternTransformProgram::new([])
        }
    };
    let Ok(program) = program else {
        return Ok((fallback(), sidecar));
    };
    Ok((
        rustel_core::value::FunctionRef::js_pattern_ir(id, program),
        sidecar,
    ))
}

pub(super) fn bridge_js_value<'js>(
    ctx: &Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<(CallbackId, Sidecar<'js>)> {
    let id = with_bridge_frame(|frame: &BridgeFrame<'js>| frame.next_id()).ok_or_else(|| {
        rquickjs::Error::new_from_js_message(
            "value",
            "native value",
            "no bridge frame is open; a JavaScript value can only be captured inside an evaluation or a query",
        )
    })?;
    let cell = rquickjs::Class::instance(ctx.clone(), CallbackCell::value(value))?;
    with_bridge_frame(|frame: &BridgeFrame<'js>| {
        frame.harvested.borrow_mut().push((id, cell.clone()));
    });
    Ok((id, Sidecar::one(id, cell)))
}

/// Extract `(Pattern, Sidecar)` from either wrapper family.
///
/// The single place that knows how a JS value becomes a pattern. It accepts
/// the builder's `PatternWrapper` as well as `NativePatternWrapper`, so
/// neither falls through to `pure(<object>)`. The sidecar comes with the
/// pattern: a pattern copied without its sidecar carries `CallbackId`s whose
/// owning cells are never imported, and the ids cannot resolve at query time.
pub(super) fn unwrap_pattern<'js>(value: &rquickjs::Value<'js>) -> Option<(Pattern, Sidecar<'js>)> {
    let object = value.as_object()?;
    if let Some(w) = rquickjs::Class::<NativePatternWrapper>::from_object(object) {
        let b = w.borrow();
        return Some((b.pattern.clone(), Sidecar::of(&b)));
    }
    if let Some(w) = rquickjs::Class::<PatternWrapper>::from_object(object) {
        let b = w.borrow();
        return Some((
            b.pattern.clone(),
            Sidecar {
                ids: b.ids.clone(),
                cells: b.cells.clone(),
                excluded_frame_ids: Rc::new(HashSet::new()),
            },
        ));
    }
    None
}

/// The per-instance `Pattern.query` data property.
///
/// The constructor assigns its query argument as an OWN writable/enumerable/
/// configurable field. Native wrappers still query their Rust graph here, but
/// the function itself is allocated per wrapper so the observable surface is
/// not replaced by a shared enumerable prototype method.
pub(super) fn native_query_method<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let effects = host_effect_boundary(&ctx)?;
    let _effect_policy = EffectPolicyScope::enter_state(&effects, EffectPolicy::NONE);
    let state_value = args
        .0
        .first()
        .and_then(rquickjs::Value::as_object)
        .ok_or_else(|| throw_type_error(&ctx, "Pattern.query requires a State"))?;
    let span: rquickjs::Object = state_value.get("span")?;
    let begin = fraction_property(&ctx, &span, "begin")?;
    let end = fraction_property(&ctx, &span, "end")?;
    let mut state = State::new(rustel_core::TimeSpan::new(begin, end));
    let mut control_sidecars = Vec::new();
    if let Ok(controls) = state_value.get::<_, rquickjs::Object>("controls") {
        // Every control is kept in the one State, so their keys (per
        // `js_key_elements`) and values share one budget.
        let budget = Cell::new(js_element_cap::<Value>(&ctx)?);
        for entry in controls.props::<String, rquickjs::Value>() {
            let (name, value) = entry?;
            charge_js_elements(&ctx, js_key_elements(&name), &budget)?;
            let (value, sidecars) = materialize_js_value_within(&ctx, &value, &budget)?;
            control_sidecars
                .try_reserve(sidecars.len())
                .map_err(|_| rquickjs::Error::Allocation)?;
            control_sidecars.extend(sidecars);
            state.controls.insert(name, value);
        }
    }
    // Every control is part of one query State. Reconcile the complete owner
    // set before any exclusion is published, so a later control can override
    // an earlier control's exclusion without an order-sensitive false cap.
    harvest_sidecars(control_sidecars)?;
    let pattern = this.0.borrow().pattern.clone();
    let _scope = WrapperQueryScope::push(&ctx, this.0.clone())?;
    let haps = pattern.query(&state);
    let array = rquickjs::Array::new(ctx.clone())?;
    for (index, hap) in haps.iter().enumerate() {
        array.set(index, callback_hap(&ctx, hap)?)?;
    }
    Ok(array.into_value())
}

/// Expose uncaught raw query errors on legacy zero-factor query surfaces.
/// Stacks contain errors from individual lanes before they reach this
/// wrapper; a direct call still observes any error outside that boundary.
pub(super) fn native_polymeter_zero_query_method<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let result = native_query_method(ctx.clone(), this, args)?;
    if let Some(message) = rustel_core::take_query_error() {
        return Err(rquickjs::Exception::throw_message(&ctx, &message));
    }
    Ok(result)
}

/// The direct-call behavior of `gap`, `silence`, and `nothing` queries.
///
/// Pinned core constructs these with `new Pattern(() => [], steps)`. This helper
/// closes the directly observed anonymous name, zero length, and fresh empty
/// result for a missing/malformed state while the native graph remains
/// responsible for activation and queries. The host's observable marker and the
/// wider native query family's constructibility are pre-existing broader
/// Pattern-surface gaps; neither full `Reflect.ownKeys(query)` nor generic
/// nested-query reflection is claimed by this slice.
pub(super) fn gap_query_method<'js>(ctx: Ctx<'js>) -> rquickjs::Result<rquickjs::Array<'js>> {
    rquickjs::Array::new(ctx)
}

/// Synchronise the observable own `__steps` field into the native graph at a
/// method boundary. Pattern methods read `this._steps`; keeping only the
/// construction-time Rust value made direct writable-field changes cosmetic.
pub(super) fn pattern_with_own_steps<'js>(
    ctx: &Ctx<'js>,
    wrapper: &rquickjs::Class<'js, NativePatternWrapper<'js>>,
) -> rquickjs::Result<Pattern> {
    let value = wrapper.clone().into_value();
    let object = value.as_object().expect("a Pattern class is an object");
    let raw: rquickjs::Value = object.get("__steps")?;
    let steps = if raw.is_undefined() {
        None
    } else {
        let number: Function = ctx.globals().get("Number")?;
        let numeric: f64 = number.call((raw,))?;
        Fraction::from_f64(numeric)
    };
    Ok(wrapper.borrow().pattern.clone().with_steps(steps))
}

/// Construct a JS-visible pattern wrapper.
///
/// **Every** path that hands a `NativePatternWrapper` to JavaScript goes
/// through here, because `polyJoin` is a class FIELD:
///
/// ```js
/// polyJoin = function () {
///   const pp = this;
///   return pp.fmap((p) => p.extend(pp._steps.div(p._steps))).outerJoin();
/// };
/// ```
///
/// A class field is created per instance, not on the prototype, which means
/// `Pattern.prototype.polyJoin === undefined`,
/// `hasOwnProperty('polyJoin') === true`, a writable/enumerable/configurable
/// data property, `length === 0`, `name === 'polyJoin'`, and a distinct
/// function object per instance (`a.polyJoin !== b.polyJoin`). Installing it on
/// the prototype instead matched the arity and the haps but none of the rest.
///
/// The cost is one `Function` allocation per wrapper, the same as a
/// class-field initialiser.
#[derive(Clone, Copy)]
pub(super) enum QuerySurface {
    Native,
    Gap,
}

pub(super) fn new_wrapper_with_query_surface<'js>(
    ctx: &Ctx<'js>,
    wrapper: NativePatternWrapper<'js>,
    query_surface: QuerySurface,
) -> rquickjs::Result<rquickjs::Class<'js, NativePatternWrapper<'js>>> {
    let pure = wrapper.pattern.as_pure();
    let pure_loc = wrapper.pattern.pure_loc();
    let steps = wrapper.pattern.steps;
    let instance = rquickjs::Class::instance(ctx.clone(), wrapper)?;
    let poly_join = Function::new(
        ctx.clone(),
        hr_this_nullary(|ctx, this| {
            let receiver = pattern_with_own_steps(&ctx, &this.0)?;
            let sidecars = {
                let borrowed = this.0.borrow();
                vec![Sidecar::of(&borrowed)]
            };
            derive_wrapper(ctx, receiver.poly_join(), &sidecars)
        }),
    )?;
    poly_join.set_name("polyJoin")?;
    // A plain `set` creates a writable/enumerable/configurable data property,
    // which is exactly what a class field produces.
    let object = rquickjs::Object::from_value(instance.clone().into_value())
        .expect("a class instance is an object");
    // Field/constructor order is observable through Object.keys: the class
    // field runs first, then the constructor assigns query, _Pattern,
    // __steps and optional pure metadata in that order.
    object.set("polyJoin", poly_join)?;
    let query = match query_surface {
        QuerySurface::Native => {
            let query = Function::new(ctx.clone(), hr_this_rest_to_value(native_query_method))?;
            set_function_length(&query, 1)?;
            query.set_name("query")?;
            query
        }
        QuerySurface::Gap => {
            let query = Function::new(ctx.clone(), gap_query_method)?;
            set_function_length(&query, 0)?;
            query.set_name("")?;
            query
        }
    };
    // Keep the public marker only as a route to the private owner. Callers
    // must also compare the exact function with `owner.native_query`; a copied
    // or forged marker alone does not establish graph ownership.
    query.set(NATIVE_QUERY_MARKER, object.clone())?;
    instance.borrow_mut().native_query = Some(query.clone());
    object.set("query", query)?;
    // The constructor creates this as an ordinary own field. Several
    // setup/prebake paths use it instead of `instanceof` so patterns from the
    // same realm remain recognisable through generic `reify`.
    object.set("_Pattern", true)?;
    // Pattern's constructor always creates `__steps` as an ordinary own data
    // property, even when undefined. Keep the exact Fraction object when the
    // private factory is already installed; wrappers created during bootstrap
    // legitimately have no user-visible step metadata yet.
    let steps_value = match steps {
        Some(steps) => {
            let stack = host_stack(ctx)?;
            match stack.as_object().get::<_, Function>(FRACTION_FACTORY) {
                Ok(factory) => factory.call((fraction_seed(ctx, steps)?,))?,
                Err(_) => rquickjs::Value::new_undefined(ctx.clone()),
            }
        }
        None => rquickjs::Value::new_undefined(ctx.clone()),
    };
    object.set("__steps", steps_value)?;
    // A pure JS-owned value may have been captured by a wrapper created in an
    // earlier evaluation. Its sidecar is already inside `instance`, but there
    // is no evaluation-scratch entry for that old callback/value id. Make the
    // wrapper being constructed the temporary resolver while materialising
    // its own `__pure` field. Pushing only the raw Pattern would make exact
    // identity work in the creating turn and fail after GC/reload.
    let _pure_scope = if pure.is_some() {
        Some(WrapperQueryScope::push(ctx, instance.clone())?)
    } else {
        None
    };
    if let Some(value) = pure {
        object.set("__pure", to_js(ctx, &value)?)?;
    }
    if let Some((start, end)) = pure_loc {
        let location = rquickjs::Object::new(ctx.clone())?;
        location.set("start", start)?;
        location.set("end", end)?;
        object.set("__pure_loc", location)?;
    }
    Ok(instance)
}

pub(super) fn new_wrapper<'js>(
    ctx: &Ctx<'js>,
    wrapper: NativePatternWrapper<'js>,
) -> rquickjs::Result<rquickjs::Class<'js, NativePatternWrapper<'js>>> {
    new_wrapper_with_query_surface(ctx, wrapper, QuerySurface::Native)
}

pub(super) fn new_gap_wrapper<'js>(
    ctx: &Ctx<'js>,
    wrapper: NativePatternWrapper<'js>,
) -> rquickjs::Result<rquickjs::Class<'js, NativePatternWrapper<'js>>> {
    new_wrapper_with_query_surface(ctx, wrapper, QuerySurface::Gap)
}

/// Build the wrapper a callback is handed, owning the cells its graph needs.
///
/// `NativePatternWrapper::plain` owns nothing. That is safe only while some
/// other wrapper keeps the callbacks alive, and JavaScript can break that
/// assumption:
///
/// ```js
/// base.every(2, (x) => { globalThis.escaped = x; return x; });
/// base = null;                 // the only owner is gone
/// escaped                      // ...and this graph still has its ids
/// ```
///
/// The argument is a genuine root once JS keeps it, so it has to own what it
/// reaches. Ownership comes from the two places a cell can be live at call
/// time: the wrapper currently being queried (either family - a builder graph
/// hands out ids too), and the open bridge frames, which is where a cell lives
/// during `stepBind`'s construction-time cycle-0 probe, before any wrapper
/// exists (see `rustel_core::query_in_progress`).
///
/// Scoping follows `derive_wrapper` exactly, and for the same reason: a PRECISE
/// graph imports only the ids it can reach, so an escape cannot root callbacks
/// it has nothing to do with, while an OPAQUE graph - whose reachable set is by
/// definition incomplete - conservatively takes what is available. A graph that
/// reaches no callbacks at all imports nothing and allocates nothing.
pub(super) fn callback_argument<'js>(
    ctx: &Ctx<'js>,
    pattern: Pattern,
) -> rquickjs::Result<rquickjs::Class<'js, NativePatternWrapper<'js>>> {
    let opaque = pattern.purity().opaque;
    let reachable_slice = pattern.reachable_callbacks();
    let mut reachable = HashSet::new();
    reachable
        .try_reserve(reachable_slice.len())
        .map_err(|_| rquickjs::Error::Allocation)?;
    reachable.extend(reachable_slice.iter().copied());
    if !opaque && reachable.is_empty() {
        return new_wrapper(ctx, NativePatternWrapper::plain(pattern));
    }

    let mut ids: Vec<CallbackId> = Vec::new();
    let mut cells: Vec<rquickjs::Class<'js, CallbackCell<'js>>> = Vec::new();
    let mut seen_ids = HashSet::new();
    // A free function rather than a closure: a closure capturing `ids` mutably
    // would hold that borrow across the `pending` scan below, which needs to
    // read it.
    fn take<'js>(
        ids: &mut Vec<CallbackId>,
        cells: &mut Vec<rquickjs::Class<'js, CallbackCell<'js>>>,
        seen_ids: &mut HashSet<CallbackId>,
        wanted: bool,
        id: CallbackId,
        cell: rquickjs::Class<'js, CallbackCell<'js>>,
    ) {
        if wanted && seen_ids.insert(id) {
            ids.push(id);
            cells.push(cell);
        }
    }
    let wanted = |id: &CallbackId| opaque || reachable.contains(id);
    let mut querying_exclusions = Rc::new(HashSet::new());

    // The wrapper being queried owns the cells of the graph this argument was
    // carved out of. `with_callback` reads the same slot, so anything resolvable
    // right now is findable here.
    let querying = host_stack(ctx)
        .ok()
        .filter(|stack| !stack.is_empty())
        .and_then(|stack| stack.get::<rquickjs::Value<'js>>(stack.len() - 1).ok());
    if let Some(object) = querying.as_ref().and_then(rquickjs::Value::as_object) {
        if let Some(w) = rquickjs::Class::<PatternWrapper>::from_object(object) {
            let b = w.borrow();
            seen_ids
                .try_reserve(b.ids.len())
                .map_err(|_| rquickjs::Error::Allocation)?;
            for (id, cell) in b.ids.iter().zip(b.cells.iter()) {
                take(
                    &mut ids,
                    &mut cells,
                    &mut seen_ids,
                    wanted(id),
                    *id,
                    cell.clone(),
                );
            }
        } else if let Some(w) = rquickjs::Class::<NativePatternWrapper>::from_object(object) {
            let b = w.borrow();
            querying_exclusions = b.excluded_frame_ids.clone();
            seen_ids
                .try_reserve(b.ids.len())
                .map_err(|_| rquickjs::Error::Allocation)?;
            for (id, cell) in b.ids.iter().zip(b.cells.iter()) {
                take(
                    &mut ids,
                    &mut cells,
                    &mut seen_ids,
                    wanted(id),
                    *id,
                    cell.clone(),
                );
            }
        }
    }

    // Frames, innermost first. `harvested` already holds cells; `pending` holds
    // the raw functions, so a wanted id gets a cell built for it here -
    // `stepBind`'s construction-time probe has no wrapper to inherit from
    // yet. `with_callback` resolves through the cell's function, so a second
    // cell over it is the same callback, not a copy of it.
    let frames = BRIDGE_FRAMES.with(|frames| frames.borrow().clone());
    let frame_capacity = frames.iter().fold(0_usize, |total, ptr| {
        // SAFETY: every pointer remains live while its BridgeScope is stacked.
        let frame = unsafe { &*ptr.cast::<BridgeFrame<'js>>() };
        total
            .saturating_add(frame.harvested.borrow().len())
            .saturating_add(frame.pending.borrow().len())
    });
    seen_ids
        .try_reserve(frame_capacity)
        .map_err(|_| rquickjs::Error::Allocation)?;
    for (index, ptr) in frames.iter().enumerate().rev() {
        // SAFETY: pushed by `BridgeScope`, which outlives this call.
        let frame = unsafe { &*ptr.cast::<BridgeFrame<'js>>() };
        for (id, cell) in frame.harvested.borrow().iter() {
            let excluded = querying_exclusions.contains(id)
                || frames[index..].iter().any(|ptr| {
                    // SAFETY: every pointer remains live while its scope is stacked.
                    let frame = unsafe { &*ptr.cast::<BridgeFrame<'js>>() };
                    frame.suppressed.borrow().contains(id)
                });
            let allowed =
                wanted(id) && (!excluded || reachable.contains(id) || seen_ids.contains(id));
            take(
                &mut ids,
                &mut cells,
                &mut seen_ids,
                allowed,
                *id,
                cell.clone(),
            );
        }
        // Collected first so `take`'s mutable borrow of `ids` does not overlap
        // the wanted-check, and so no cell is allocated for an id already held.
        let missing: Vec<(CallbackId, Function<'js>)> = frame
            .pending
            .borrow()
            .iter()
            .filter(|(id, _)| {
                let excluded = querying_exclusions.contains(id)
                    || frames[index..].iter().any(|ptr| {
                        // SAFETY: every pointer remains live while its scope is stacked.
                        let frame = unsafe { &*ptr.cast::<BridgeFrame<'js>>() };
                        frame.suppressed.borrow().contains(id)
                    });
                wanted(id)
                    && (!excluded || reachable.contains(id) || seen_ids.contains(id))
                    && !seen_ids.contains(id)
            })
            .map(|(id, func)| (*id, func.clone()))
            .collect();
        for (id, func) in missing {
            let cell = rquickjs::Class::instance(ctx.clone(), CallbackCell::function(func))?;
            CELLS_CREATED.with(|c| c.set(c.get() + 1));
            take(&mut ids, &mut cells, &mut seen_ids, true, id, cell);
        }
    }
    let mut protected = reachable;
    protected
        .try_reserve(seen_ids.len())
        .map_err(|_| rquickjs::Error::Allocation)?;
    protected.extend(seen_ids.iter().copied());
    let exclusion_hint = frames.iter().fold(querying_exclusions.len(), |total, ptr| {
        // SAFETY: every pointer remains live while its scope is stacked.
        let frame = unsafe { &*ptr.cast::<BridgeFrame<'js>>() };
        total.saturating_add(frame.suppressed.borrow().len())
    });
    let mut exclusions = CappedExclusions::new(exclusion_hint).map_err(|error| match error {
        OwnershipSetError::Allocation => rquickjs::Error::Allocation,
        OwnershipSetError::Limit(_) => unreachable!("an empty exclusion builder cannot exceed cap"),
    })?;
    for id in querying_exclusions.iter().copied() {
        if let Err(error) = exclusions.insert(id, &protected) {
            return match error {
                OwnershipSetError::Limit(limit) => ownership_refusal_wrapper(ctx, limit),
                OwnershipSetError::Allocation => Err(rquickjs::Error::Allocation),
            };
        }
    }
    for ptr in &frames {
        // SAFETY: every pointer remains live while its scope is stacked.
        let frame = unsafe { &*ptr.cast::<BridgeFrame<'js>>() };
        for id in frame.suppressed.borrow().iter().copied() {
            if let Err(error) = exclusions.insert(id, &protected) {
                return match error {
                    OwnershipSetError::Limit(limit) => ownership_refusal_wrapper(ctx, limit),
                    OwnershipSetError::Allocation => Err(rquickjs::Error::Allocation),
                };
            }
        }
    }

    new_wrapper(
        ctx,
        NativePatternWrapper {
            pattern,
            native_query: None,
            ids,
            cells,
            explicit_ownership_complete: false,
            excluded_frame_ids: Rc::new(exclusions.finish()),
        },
    )
}

mod polymeter;
mod reify;
mod stepalt;
mod stepwise;

pub(crate) use polymeter::*;
pub(crate) use reify::*;
pub(crate) use stepalt::*;
pub(crate) use stepwise::*;
