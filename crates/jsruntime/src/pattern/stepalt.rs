use super::*;

/// Native allocation and ownership boundary behind lexical `stepalt`.
///
/// Pinned core first reifies every source group, computes the group-length
/// LCM, expands one entry per group and cycle, filters non-positive step
/// patterns, then feeds the survivors to lexical `stepcat`. Native preserves
/// that accepted-case order, but deliberately plans and charges the complete
/// source/expansion shape before invoking the configurable string parser. An
/// oversized call therefore performs no parser or indexed-property work.
/// Raw rest arguments and the caller's Arrays already exist at this boundary;
/// Proxy/holes/accessor fidelity remains outside this bounded surface. Accepted
/// ordering is pinned for stable dense ordinary Arrays; a parser that mutates
/// a later group's length/indexes after this preflight remains an explicit
/// phase-order divergence.
pub(crate) fn native_stepalt<'js>(
    ctx: Ctx<'js>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let refusal = |ctx: Ctx<'js>, limit| {
        derive_wrapper(ctx, rustel_core::query_limit_pattern(limit), &[])
            .map(rquickjs::Class::into_value)
    };
    let host_memory = |ctx: Ctx<'js>| {
        let limit = rustel_core::mark_stepwise_refusal(rustel_core::QueryLimit::HostMemory);
        refusal(ctx, limit)
    };

    // `groups.map(...)` owns one outer slot per argument even for an empty
    // Array, while its inner maps own one slot per source value. Bound both
    // vectors by taking their high-water cardinality.
    let group_count = u64::try_from(args.0.len()).unwrap_or(u64::MAX);
    let mut group_lengths = Vec::new();
    if group_lengths.try_reserve_exact(args.0.len()).is_err() {
        return host_memory(ctx);
    }
    let mut source_items = 0_u64;
    for group in &args.0 {
        // A length claim (see `js_array_len`); one that cannot be read
        // counts as the maximum, which the stepwise charge refuses.
        let length = group.as_array().map_or(1_u64, |array| {
            js_array_len(array).map_or(u64::MAX, |len| u64::try_from(len).unwrap_or(u64::MAX))
        });
        source_items = source_items.saturating_add(length);
        group_lengths.push(length);
    }
    let source_count = source_items.max(group_count);

    // Variadic lcm seeds from the final argument. Lengths are
    // non-negative integers, so the fold order is not otherwise observable,
    // but retaining it keeps the no/single-argument edge exact.
    let cycles = if let Some((&last, rest)) = group_lengths.split_last() {
        let mut cycles = Fraction::int(i128::from(last));
        for length in rest {
            let Some(next) = cycles.checked_lcm(Fraction::int(i128::from(*length))) else {
                let limit =
                    rustel_core::mark_stepwise_refusal(rustel_core::QueryLimit::NativeFraction {
                        operation: "stepalt",
                    });
                return refusal(ctx, limit);
            };
            cycles = next;
        }
        cycles.numer().unsigned_abs()
    } else {
        0
    };
    let expanded = cycles
        .checked_mul(u128::from(group_count))
        .and_then(|entries| u64::try_from(entries).ok())
        .unwrap_or(u64::MAX);
    let work = source_count.max(expanded);
    if let Err(limit) = rustel_core::charge_stepwise_entries("stepalt", work) {
        return refusal(ctx, limit);
    }

    // Reserve every native expansion-owned buffer before the first reify
    // call. Host-memory failures from these planned vectors therefore cannot
    // follow parser side effects. Reification/QuickJS and ownership metadata
    // have separate fallible allocations after this boundary.
    let mut groups: Vec<Vec<(Pattern, Sidecar<'js>, bool)>> = Vec::new();
    if groups.try_reserve_exact(args.0.len()).is_err() {
        return host_memory(ctx);
    }
    for length in &group_lengths {
        let Ok(length) = usize::try_from(*length) else {
            return host_memory(ctx);
        };
        let mut group = Vec::new();
        if group.try_reserve_exact(length).is_err() {
            return host_memory(ctx);
        }
        groups.push(group);
    }
    let Ok(source_capacity) = usize::try_from(source_items) else {
        return host_memory(ctx);
    };
    let mut retained_sidecars = Vec::new();
    if retained_sidecars
        .try_reserve_exact(source_capacity)
        .is_err()
    {
        return host_memory(ctx);
    }
    let mut filtered_sidecars = Vec::new();
    if filtered_sidecars
        .try_reserve_exact(source_capacity)
        .is_err()
    {
        return host_memory(ctx);
    }
    let Ok(expanded_capacity) = usize::try_from(expanded) else {
        return host_memory(ctx);
    };
    let mut items = Vec::new();
    if items.try_reserve_exact(expanded_capacity).is_err() {
        return host_memory(ctx);
    }
    let mut compressed = Vec::new();
    if compressed.try_reserve_exact(expanded_capacity).is_err() {
        return host_memory(ctx);
    }
    let mut plan = Vec::new();
    if plan.try_reserve_exact(expanded_capacity).is_err() {
        return host_memory(ctx);
    }

    // Accepted calls retain the pinned map order and reify each source only
    // once, before any cycle expansion. Repeated entries share the resulting
    // Pattern graph and need only the source sidecar to keep JS values alive.
    for ((raw_group, length), patterns) in args
        .0
        .iter()
        .zip(group_lengths.iter())
        .zip(groups.iter_mut())
    {
        if let Some(array) = raw_group.as_array() {
            for index in 0..*length {
                let raw: rquickjs::Value = array.get(index as usize)?;
                let (pattern, sidecar) = reify_direct_bridged(&ctx, &raw)?;
                patterns.push((pattern, sidecar, false));
            }
        } else {
            let (pattern, sidecar) = reify_direct_bridged(&ctx, raw_group)?;
            patterns.push((pattern, sidecar, false));
        }
    }

    let cycle_count = usize::try_from(cycles).expect("charged stepalt cycle count fits usize");
    let mut total = Fraction::ZERO;
    let mut retained = 0_usize;
    for cycle in 0..cycle_count {
        for group in &mut groups {
            // A zero-length group makes the LCM zero, so no expansion reaches
            // this loop. Every group here therefore has an indexed source.
            let index = cycle % group.len();
            let source = &mut group[index];
            if let Some(steps) = source.0.steps
                && steps > Fraction::ZERO
            {
                let Some(next) = total.checked_add(steps) else {
                    let limit = rustel_core::mark_stepwise_refusal(
                        rustel_core::QueryLimit::NativeFraction {
                            operation: "stepalt",
                        },
                    );
                    return refusal(ctx, limit);
                };
                total = next;
                source.2 = true;
                retained += 1;
            }
        }
    }

    if retained == 0 {
        // Even though lexical `stepalt` returns the captured `nothing` object,
        // JS-owned cells created while reifying these now-discarded sources
        // remain in bridge scratch until the turn ends. Record that proof so a
        // later same-turn opaque composition cannot resurrect them unless an
        // explicit owner is republished first. The exact lexical `nothing`
        // singleton cannot safely carry per-call persistent provenance:
        // republishing an old owner removes its id from live suppression, so a
        // still-later unrelated opaque result (in this turn or a future one)
        // may conservatively retain it. That ordering is an explicit residual
        // of preserving `stepalt(...) === nothing`.
        let mut filtered_ids = match CappedExclusions::new(0) {
            Ok(ids) => ids,
            Err(OwnershipSetError::Allocation) => return host_memory(ctx),
            Err(OwnershipSetError::Limit(_)) => {
                unreachable!("an empty exclusion builder cannot exceed cap")
            }
        };
        let protected = HashSet::new();
        for group in &groups {
            for (_, sidecar, _) in group {
                for id in sidecar
                    .ids
                    .iter()
                    .chain(sidecar.excluded_frame_ids.iter())
                    .copied()
                {
                    if let Err(error) = filtered_ids.insert(id, &protected) {
                        return match error {
                            OwnershipSetError::Limit(limit) => {
                                ownership_refusal_wrapper(&ctx, limit)
                                    .map(rquickjs::Class::into_value)
                            }
                            OwnershipSetError::Allocation => host_memory(ctx),
                        };
                    }
                }
            }
        }
        let filtered_ids = filtered_ids.finish();
        if let Err(error) = suppress_in_all_bridge_frames(&filtered_ids) {
            return match error {
                OwnershipSetError::Limit(limit) => {
                    ownership_refusal_wrapper(&ctx, limit).map(rquickjs::Class::into_value)
                }
                OwnershipSetError::Allocation => host_memory(ctx),
            };
        }
        // The lexical JavaScript declaration returns its captured `nothing`
        // singleton for this sentinel, preserving `stepalt() === nothing`.
        return Ok(rquickjs::Value::new_undefined(ctx));
    }
    for cycle in 0..cycle_count {
        for group in &groups {
            let pattern = &group[cycle % group.len()].0;
            if let Some(steps) = pattern.steps
                && steps > Fraction::ZERO
            {
                items.push(pattern.clone());
            }
        }
    }
    debug_assert_eq!(items.len(), retained);
    debug_assert!(
        plan.capacity() >= expanded_capacity,
        "stepalt compression plan must be reserved before source reification"
    );

    // Plan every ratio with checked arithmetic, as core `stepcat` does, then
    // build the same compressed graph with child step metadata cleared. A
    // total or intermediate ratio outside the native Fraction range refuses
    // as `stepalt` before the graph is built.
    let pattern = if items.len() == 1 {
        items
            .pop()
            .expect("one retained stepalt item")
            .with_steps(Some(total))
    } else {
        let mut begin = Fraction::ZERO;
        for item in &items {
            let steps = item.steps.expect("retained stepalt item has steps");
            let Some((end, cat_begin, factor)) = (|| {
                let end = begin.checked_add(steps)?;
                let cat_begin = begin.checked_div(total)?;
                let cat_end = end.checked_div(total)?;
                let width = cat_end.checked_sub(cat_begin)?;
                let factor = Fraction::ONE.checked_div(width)?;
                Some((end, cat_begin, factor))
            })() else {
                let limit =
                    rustel_core::mark_stepwise_refusal(rustel_core::QueryLimit::NativeFraction {
                        operation: "stepalt",
                    });
                return refusal(ctx, limit);
            };
            plan.push((cat_begin, factor));
            begin = end;
        }

        for (item, (cat_begin, factor)) in items.into_iter().zip(plan) {
            compressed.push(item.with_steps(None).fast_gap(factor).late(cat_begin));
        }
        rustel_core::stack(compressed).with_steps(Some(total))
    };
    rustel_core::note_stepwise_entries_materialised(retained as u64);

    // A filtered source is not reachable from the final graph. Do not offer
    // its sidecar to `derive_wrapper`: precise graphs would prune it anyway,
    // but an unrelated retained opaque source deliberately keeps every
    // sidecar it is handed as a conservative ownership rule.
    for group in groups {
        for (_, sidecar, was_retained) in group {
            if was_retained {
                retained_sidecars.push(sidecar);
            } else {
                filtered_sidecars.push(sidecar);
            }
        }
    }

    derive_wrapper_from_explicit_sources(ctx, pattern, &retained_sidecars, &filtered_sidecars)
        .map(rquickjs::Class::into_value)
}
