use super::*;

pub(crate) const POLYMETER_OPERATION: &str = "polymeter";

pub(crate) fn polymeter_refusal<'js>(
    ctx: Ctx<'js>,
    limit: rustel_core::QueryLimit,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    derive_wrapper(ctx, rustel_core::query_limit_pattern(limit), &[])
        .map(rquickjs::Class::into_value)
}

pub(crate) fn polymeter_host_memory<'js>(ctx: Ctx<'js>) -> rquickjs::Result<rquickjs::Value<'js>> {
    let limit = rustel_core::mark_stepwise_refusal(rustel_core::QueryLimit::HostMemory);
    polymeter_refusal(ctx, limit)
}

pub(crate) fn suppress_polymeter_sidecars<'js>(
    ctx: &Ctx<'js>,
    sidecars: &[Sidecar<'js>],
) -> rquickjs::Result<Option<rquickjs::Value<'js>>> {
    // This reuses stepalt's capped exclusion set deliberately. The typed
    // overflow label remains `stepalt ownership` because both operations share
    // the same resource boundary.
    // Exact lexical silence/nothing sentinels cannot carry durable per-call
    // exclusions, so this proof applies to the live bridge frames in which an
    // unrelated opaque outer graph could otherwise harvest discarded cells.
    let mut discarded = match CappedExclusions::new(0) {
        Ok(ids) => ids,
        Err(OwnershipSetError::Allocation) => {
            return polymeter_host_memory(ctx.clone()).map(Some);
        }
        Err(OwnershipSetError::Limit(_)) => {
            unreachable!("an empty exclusion builder cannot exceed its cap")
        }
    };
    let protected = HashSet::new();
    for sidecar in sidecars {
        for id in sidecar
            .ids
            .iter()
            .chain(sidecar.excluded_frame_ids.iter())
            .copied()
        {
            if let Err(error) = discarded.insert(id, &protected) {
                return match error {
                    OwnershipSetError::Limit(limit) => ownership_refusal_wrapper(ctx, limit)
                        .map(rquickjs::Class::into_value)
                        .map(Some),
                    OwnershipSetError::Allocation => polymeter_host_memory(ctx.clone()).map(Some),
                };
            }
        }
    }
    if let Err(error) = suppress_in_all_bridge_frames(&discarded.finish()) {
        return match error {
            OwnershipSetError::Limit(limit) => ownership_refusal_wrapper(ctx, limit)
                .map(rquickjs::Class::into_value)
                .map(Some),
            OwnershipSetError::Allocation => polymeter_host_memory(ctx.clone()).map(Some),
        };
    }
    Ok(None)
}

/// Modern `polymeter` over the call's arguments, already split on `hasSteps`.
/// The operands are host slices, so their count is exact and no
/// `Array.prototype` accessor runs while they are read.
pub(crate) fn native_polymeter_modern<'js>(
    ctx: Ctx<'js>,
    retained: &[rquickjs::Value<'js>],
    filtered: &[rquickjs::Value<'js>],
) -> rquickjs::Result<rquickjs::Value<'js>> {
    // Each retained operand is a lane. A lane count past the stepwise cap is
    // refused before any lane buffer is reserved or any lane is read; an
    // accepted count is charged once, below. Filtered operands are not lanes.
    let retained_count = u64::try_from(retained.len()).unwrap_or(u64::MAX);
    if retained_count > rustel_core::MAX_STEPWISE_ENTRIES {
        if let Err(limit) =
            rustel_core::charge_stepwise_entries(POLYMETER_OPERATION, retained_count)
        {
            return polymeter_refusal(ctx, limit);
        }
        unreachable!("a lane count above the public cap must be refused")
    }

    let mut patterns = Vec::new();
    if patterns.try_reserve_exact(retained.len()).is_err() {
        return polymeter_host_memory(ctx);
    }
    let mut retained_sidecars = Vec::new();
    if retained_sidecars.try_reserve_exact(retained.len()).is_err() {
        return polymeter_host_memory(ctx);
    }
    for value in retained {
        let Some((mut pattern, sidecar)) = unwrap_pattern(value) else {
            return Err(throw_type_error(
                &ctx,
                "polymeter native path requires Pattern operands",
            ));
        };
        if let Some(object) = value.as_object()
            && let Some(wrapper) = rquickjs::Class::<NativePatternWrapper>::from_object(object)
        {
            pattern = pattern_with_own_steps(&ctx, &wrapper)?;
        }
        let Some(steps) = pattern.steps else {
            return Err(throw_type_error(
                &ctx,
                "polymeter operand lost its step count during filtering",
            ));
        };
        patterns.push((pattern, steps));
        retained_sidecars.push(sidecar);
    }

    // Sized by the call's own arguments, which the host already holds, so no
    // element budget applies.
    let mut filtered_sidecars = Vec::new();
    if filtered_sidecars.try_reserve_exact(filtered.len()).is_err() {
        return polymeter_host_memory(ctx);
    }
    for value in filtered {
        if let Some((_, sidecar)) = unwrap_pattern(value) {
            filtered_sidecars.push(sidecar);
        }
    }

    if patterns.is_empty() {
        if let Some(refusal) = suppress_polymeter_sidecars(&ctx, &filtered_sidecars)? {
            return Ok(refusal);
        }
        return Ok(rquickjs::Value::new_undefined(ctx));
    }

    // Variadic lcm seeds from the final argument, as Strudel's Fraction does.
    let (last, rest) = patterns.split_last().expect("non-empty modern polymeter");
    let mut steps = last.1;
    for (_, source_steps) in rest {
        let Some(next) = steps.checked_lcm(*source_steps) else {
            let limit =
                rustel_core::mark_stepwise_refusal(rustel_core::QueryLimit::NativeFraction {
                    operation: POLYMETER_OPERATION,
                });
            return polymeter_refusal(ctx, limit);
        };
        steps = next;
    }

    if steps == Fraction::ZERO {
        if let Err(limit) =
            rustel_core::charge_stepwise_entries(POLYMETER_OPERATION, retained_count)
        {
            return polymeter_refusal(ctx, limit);
        }
        let mut discarded = retained_sidecars;
        discarded.extend(filtered_sidecars);
        if let Some(refusal) = suppress_polymeter_sidecars(&ctx, &discarded)? {
            return Ok(refusal);
        }
        return Ok(rquickjs::Value::new_null(ctx));
    }

    let mut split_work = 0_u64;
    let mut ratios = Vec::new();
    if ratios.try_reserve_exact(patterns.len()).is_err() {
        return polymeter_host_memory(ctx);
    }
    for (_, source_steps) in &patterns {
        let Some(ratio) = steps.checked_div(*source_steps) else {
            let limit =
                rustel_core::mark_stepwise_refusal(rustel_core::QueryLimit::NativeFraction {
                    operation: POLYMETER_OPERATION,
                });
            return polymeter_refusal(ctx, limit);
        };
        let magnitude = ratio.numer().unsigned_abs();
        let denominator = ratio.denom() as u128;
        let rounded =
            (magnitude / denominator).saturating_add(u128::from(magnitude % denominator != 0));
        let rounded = u64::try_from(rounded).unwrap_or(u64::MAX);
        split_work = split_work.saturating_add(rounded);
        ratios.push(ratio);
    }
    let work = retained_count.max(split_work);
    if let Err(limit) = rustel_core::charge_stepwise_entries(POLYMETER_OPERATION, work) {
        return polymeter_refusal(ctx, limit);
    }

    let mut paced = Vec::new();
    if paced.try_reserve_exact(patterns.len()).is_err() {
        return polymeter_host_memory(ctx);
    }
    for ((pattern, _), ratio) in patterns.into_iter().zip(ratios) {
        paced.push(checked_polymeter_raw_fast(pattern, ratio).with_steps(None));
    }
    rustel_core::note_stepwise_entries_materialised(retained_count);
    let pattern = rustel_core::stack(paced).with_steps(Some(steps));
    derive_wrapper_from_explicit_sources(ctx, pattern, &retained_sidecars, &filtered_sidecars)
        .map(rquickjs::Class::into_value)
}

#[derive(Clone)]
pub(crate) enum LegacyPolymeterSource<'js> {
    Direct(rquickjs::Value<'js>),
    Slot {
        array: rquickjs::Array<'js>,
        index: usize,
    },
}

impl<'js> LegacyPolymeterSource<'js> {
    fn read(&self) -> rquickjs::Result<rquickjs::Value<'js>> {
        match self {
            Self::Direct(value) => Ok(value.clone()),
            Self::Slot { array, index } => array.get(*index),
        }
    }
}

pub(crate) enum LegacyPolymeterKind<'js> {
    Leaf,
    Empty(rquickjs::Array<'js>),
    Singleton {
        array: rquickjs::Array<'js>,
        child: usize,
    },
    Cat {
        array: rquickjs::Array<'js>,
        children: Vec<usize>,
        length: u64,
    },
}

pub(crate) struct LegacyPolymeterNode<'js> {
    source: LegacyPolymeterSource<'js>,
    kind: LegacyPolymeterKind<'js>,
    count: u64,
    logical: u64,
}

pub(crate) enum LegacyPolymeterWork<'js> {
    Visit(LegacyPolymeterSource<'js>),
    FinishSingleton(usize),
    FinishCat { node: usize, length: usize },
}

pub(crate) fn same_js_value(
    ctx: &Ctx<'_>,
    left: &rquickjs::Value<'_>,
    right: &rquickjs::Value<'_>,
) -> bool {
    // SAFETY: both values belong to the live `ctx`; strict equality neither
    // retains the values nor invokes user JavaScript.
    unsafe { rquickjs::qjs::JS_IsStrictEqual(ctx.as_raw().as_ptr(), left.as_raw(), right.as_raw()) }
}

pub(crate) fn checked_polymeter_raw_fast(pattern: Pattern, factor: Fraction) -> Pattern {
    let steps = pattern.steps;
    let query_factor = factor;
    pattern
        .with_query_time(move |time| {
            time.checked_mul(query_factor).unwrap_or_else(|| {
                rustel_core::mark_stepwise_refusal(rustel_core::QueryLimit::NativeFraction {
                    operation: POLYMETER_OPERATION,
                });
                Fraction::ZERO
            })
        })
        .with_hap_time(move |time| {
            if factor == Fraction::ZERO {
                rustel_core::signal_query_error(|| "Division by Zero".into());
                return Fraction::ZERO;
            }
            time.checked_div(factor).unwrap_or_else(|| {
                rustel_core::mark_stepwise_refusal(rustel_core::QueryLimit::NativeFraction {
                    operation: POLYMETER_OPERATION,
                });
                Fraction::ZERO
            })
        })
        .with_steps(steps)
}

pub(crate) fn native_polymeter_legacy<'js>(
    ctx: Ctx<'js>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let top_count = u64::try_from(args.0.len()).unwrap_or(u64::MAX);
    // A score hands polymeter its lane arrays directly (see `js_array_len`);
    // a claim that cannot be read counts as the maximum, so the stepwise
    // charge refuses it below.
    let direct_array_slots = args.0.iter().fold(0_u64, |total, value| {
        total.saturating_add(value.as_array().map_or(0, |array| {
            js_array_len(array).map_or(u64::MAX, |len| u64::try_from(len).unwrap_or(u64::MAX))
        }))
    });
    let direct_source = top_count.max(direct_array_slots);
    if direct_source > rustel_core::MAX_STEPWISE_ENTRIES {
        if let Err(limit) = rustel_core::charge_stepwise_entries(POLYMETER_OPERATION, direct_source)
        {
            return polymeter_refusal(ctx, limit);
        }
        unreachable!("a source count above the public cap must be refused")
    }

    let mut nodes = Vec::new();
    let mut roots = Vec::new();
    let mut work = Vec::new();
    let mut completed = Vec::new();
    let mut array_slots = 0_u64;
    for raw_root in &args.0 {
        work.push(LegacyPolymeterWork::Visit(LegacyPolymeterSource::Direct(
            raw_root.clone(),
        )));
        while let Some(item) = work.pop() {
            match item {
                LegacyPolymeterWork::Visit(source) => {
                    let value = source.read()?;
                    let Some(array) = value.as_array().cloned() else {
                        let index = nodes.len();
                        nodes.push(LegacyPolymeterNode {
                            source,
                            kind: LegacyPolymeterKind::Leaf,
                            count: 1,
                            logical: 1,
                        });
                        completed.push(index);
                        continue;
                    };
                    let length = js_array_len(&array)?;
                    array_slots =
                        array_slots.saturating_add(u64::try_from(length).unwrap_or(u64::MAX));
                    let discovered_source = top_count.max(array_slots);
                    if discovered_source > rustel_core::MAX_STEPWISE_ENTRIES {
                        if let Err(limit) = rustel_core::charge_stepwise_entries(
                            POLYMETER_OPERATION,
                            discovered_source,
                        ) {
                            return polymeter_refusal(ctx, limit);
                        }
                        unreachable!("a discovered source count above the cap must be refused")
                    }
                    let node = nodes.len();
                    if length == 0 {
                        nodes.push(LegacyPolymeterNode {
                            source,
                            kind: LegacyPolymeterKind::Empty(array),
                            count: 0,
                            logical: 0,
                        });
                        completed.push(node);
                    } else if length == 1 {
                        nodes.push(LegacyPolymeterNode {
                            source,
                            kind: LegacyPolymeterKind::Empty(array.clone()),
                            count: 0,
                            logical: 0,
                        });
                        work.push(LegacyPolymeterWork::FinishSingleton(node));
                        work.push(LegacyPolymeterWork::Visit(LegacyPolymeterSource::Slot {
                            array,
                            index: 0,
                        }));
                    } else {
                        nodes.push(LegacyPolymeterNode {
                            source,
                            kind: LegacyPolymeterKind::Empty(array.clone()),
                            count: 0,
                            logical: 0,
                        });
                        work.push(LegacyPolymeterWork::FinishCat { node, length });
                        for index in (0..length).rev() {
                            work.push(LegacyPolymeterWork::Visit(LegacyPolymeterSource::Slot {
                                array: array.clone(),
                                index,
                            }));
                        }
                    }
                }
                LegacyPolymeterWork::FinishSingleton(node) => {
                    let child = completed.pop().expect("planned singleton child");
                    let (count, logical) = (nodes[child].count, nodes[child].logical);
                    let array = match &nodes[node].kind {
                        LegacyPolymeterKind::Empty(array) => array.clone(),
                        _ => unreachable!("singleton placeholder kind"),
                    };
                    nodes[node].kind = LegacyPolymeterKind::Singleton { array, child };
                    nodes[node].count = count;
                    nodes[node].logical = logical;
                    completed.push(node);
                }
                LegacyPolymeterWork::FinishCat { node, length } => {
                    let first = completed.len() - length;
                    let children = completed.split_off(first);
                    let logical_sum = children.iter().fold(0_u64, |total, child| {
                        total.saturating_add(nodes[*child].logical)
                    });
                    let length_u64 = u64::try_from(length).unwrap_or(u64::MAX);
                    let array = match &nodes[node].kind {
                        LegacyPolymeterKind::Empty(array) => array.clone(),
                        _ => unreachable!("cat placeholder kind"),
                    };
                    nodes[node].kind = LegacyPolymeterKind::Cat {
                        array,
                        children,
                        length: length_u64,
                    };
                    nodes[node].count = length_u64;
                    nodes[node].logical = length_u64.max(logical_sum);
                    completed.push(node);
                }
            }
        }
        roots.push(completed.pop().expect("one completed polymeter root"));
        debug_assert!(completed.is_empty());
    }

    let source_count = top_count.max(array_slots);
    let target = roots.first().map_or(0, |root| nodes[*root].count);
    let live_lanes = roots.iter().filter(|root| nodes[**root].count != 0).count();
    let mut expansion = 0_u64;
    for root in &roots {
        let node = &nodes[*root];
        if node.count == 0 {
            continue;
        }
        let product = u128::from(node.logical).saturating_mul(u128::from(target));
        let divisor = u128::from(node.count);
        let scaled = (product / divisor).saturating_add(u128::from(product % divisor != 0));
        expansion = expansion.saturating_add(u64::try_from(scaled).unwrap_or(u64::MAX));
    }
    let work_count = source_count
        .max(u64::try_from(live_lanes).unwrap_or(u64::MAX))
        .max(expansion);
    if let Err(limit) = rustel_core::charge_stepwise_entries(POLYMETER_OPERATION, work_count) {
        return polymeter_refusal(ctx, limit);
    }

    let mut patterns: Vec<Option<Pattern>> = Vec::new();
    if patterns.try_reserve_exact(nodes.len()).is_err() {
        return polymeter_host_memory(ctx);
    }
    patterns.resize_with(nodes.len(), || None);
    let mut sidecars = Vec::new();
    if sidecars.try_reserve_exact(nodes.len()).is_err() {
        return polymeter_host_memory(ctx);
    }

    // Re-enter the planned trees in the pinned depth-first order. Leaf slot
    // values are fetched only here, so an earlier parser call can still mutate
    // a later scalar value. Any scalar/Array shape change is rejected instead
    // of silently applying the stale resource plan to a different tree.
    work.clear();
    for root in &roots {
        let mut node_stack = vec![*root];
        while let Some(node_index) = node_stack.pop() {
            let current = nodes[node_index].source.read()?;
            match &nodes[node_index].kind {
                LegacyPolymeterKind::Leaf => {
                    if current.as_array().is_some() {
                        return Err(throw_type_error(
                            &ctx,
                            "polymeter Array nesting changed while reifying",
                        ));
                    }
                    let (pattern, sidecar) = reify_direct_bridged(&ctx, &current)?;
                    patterns[node_index] = Some(pattern);
                    sidecars.push(sidecar);
                }
                LegacyPolymeterKind::Empty(array) => {
                    let planned = array.clone().into_value();
                    // These re-reads run after parser callbacks (user
                    // JavaScript), which may have re-lengthened a planned
                    // array, so the staleness check reads through the guard
                    // (see `js_array_len`) and treats an unreadable length as
                    // changed.
                    if !same_js_value(&ctx, &current, &planned)
                        || js_array_len(array).unwrap_or(usize::MAX) != 0
                    {
                        return Err(throw_type_error(
                            &ctx,
                            "polymeter Array nesting changed while reifying",
                        ));
                    }
                }
                LegacyPolymeterKind::Singleton { array, child } => {
                    let planned = array.clone().into_value();
                    if !same_js_value(&ctx, &current, &planned)
                        || js_array_len(array).unwrap_or(usize::MAX) != 1
                    {
                        return Err(throw_type_error(
                            &ctx,
                            "polymeter Array nesting changed while reifying",
                        ));
                    }
                    node_stack.push(*child);
                }
                LegacyPolymeterKind::Cat {
                    array,
                    children,
                    length,
                } => {
                    let planned = array.clone().into_value();
                    if !same_js_value(&ctx, &current, &planned)
                        || js_array_len(array)
                            .ok()
                            .and_then(|len| u64::try_from(len).ok())
                            != Some(*length)
                    {
                        return Err(throw_type_error(
                            &ctx,
                            "polymeter Array nesting changed while reifying",
                        ));
                    }
                    node_stack.extend(children.iter().rev().copied());
                }
            }
        }
    }

    for node_index in (0..nodes.len()).rev() {
        match &nodes[node_index].kind {
            LegacyPolymeterKind::Leaf => {}
            LegacyPolymeterKind::Empty(_) => {
                patterns[node_index] = Some(rustel_core::silence());
            }
            LegacyPolymeterKind::Singleton { child, .. } => {
                patterns[node_index] = patterns[*child].clone();
            }
            LegacyPolymeterKind::Cat {
                children, length, ..
            } => {
                let children = children
                    .iter()
                    .map(|child| {
                        patterns[*child]
                            .as_ref()
                            .expect("planned child pattern")
                            .clone()
                            .with_steps(None)
                    })
                    .collect();
                patterns[node_index] = Some(
                    rustel_core::fastcat(children)
                        .with_steps(Some(Fraction::int(i128::from(*length)))),
                );
            }
        }
    }

    let mut known_steps = Vec::new();
    for root in &roots {
        if nodes[*root].count != 0
            && let Some(steps) = patterns[*root].as_ref().expect("root pattern").steps
        {
            known_steps.push(steps);
        }
    }
    let final_steps = if let Some((last, rest)) = known_steps.split_last() {
        let mut result = *last;
        for steps in rest {
            let Some(next) = result.checked_lcm(*steps) else {
                let limit =
                    rustel_core::mark_stepwise_refusal(rustel_core::QueryLimit::NativeFraction {
                        operation: POLYMETER_OPERATION,
                    });
                return polymeter_refusal(ctx, limit);
            };
            result = next;
        }
        Some(result)
    } else {
        None
    };

    let target_fraction = Fraction::int(i128::from(target));
    let mut lanes = Vec::new();
    if lanes.try_reserve_exact(live_lanes).is_err() {
        return polymeter_host_memory(ctx);
    }
    for root in &roots {
        let count = nodes[*root].count;
        if count == 0 {
            continue;
        }
        let Some(factor) = target_fraction.checked_div(Fraction::int(i128::from(count))) else {
            let limit =
                rustel_core::mark_stepwise_refusal(rustel_core::QueryLimit::NativeFraction {
                    operation: POLYMETER_OPERATION,
                });
            return polymeter_refusal(ctx, limit);
        };
        let pattern = patterns[*root].take().expect("live root pattern");
        lanes.push(if target == count {
            pattern
        } else {
            checked_polymeter_raw_fast(pattern, factor)
        });
    }
    rustel_core::note_stepwise_entries_materialised(source_count.max(live_lanes as u64));
    let result = rustel_core::stack(
        lanes
            .into_iter()
            .map(|pattern| pattern.with_steps(None))
            .collect(),
    )
    .with_steps(final_steps);
    let wrapper = derive_wrapper_from_explicit_sources(ctx.clone(), result, &sidecars, &[])?;
    if target == 0 && live_lanes != 0 {
        let query = Function::new(
            ctx.clone(),
            hr_this_rest_to_value(native_polymeter_zero_query_method),
        )?;
        set_function_length(&query, 1)?;
        query.set_name("query")?;
        let value = wrapper.clone().into_value();
        let object = value.as_object().expect("a Pattern wrapper is an object");
        query.set(NATIVE_QUERY_MARKER, object.clone())?;
        wrapper.borrow_mut().native_query = Some(query.clone());
        object.set("query", query)?;
    }
    Ok(wrapper.into_value())
}
