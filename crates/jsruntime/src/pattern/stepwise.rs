use super::*;

pub(crate) type RawStepcatPair<'js> = (Option<Fraction>, rquickjs::Value<'js>, bool, bool);
pub(crate) type WeightedPattern = (Option<Fraction>, Pattern);
pub(crate) type BridgedStepcatItems<'js> = (Vec<WeightedPattern>, Vec<Sidecar<'js>>);

/// `[weight, pattern, ...ignored]` - `stepcat`/`timecat`'s explicit-weight
/// argument form.
///
/// `findsteps` treats every array as the tuple itself; later array
/// destructuring observes only slots zero and one. The pattern slot goes
/// through direct `reify`, not list sequencing, so a nested array remains one
/// pure array value. `None` is reserved for a non-array argument, which the
/// caller then weighs by the pattern's own step count.
pub(crate) fn stepcat_pair_bridged<'js>(
    value: &rquickjs::Value<'js>,
) -> rquickjs::Result<Option<RawStepcatPair<'js>>> {
    let Some(array) = value.as_array() else {
        return Ok(None);
    };
    let weight: rquickjs::Value = array.get(0)?;
    let pattern: rquickjs::Value = array.get(1)?;
    let weight_was_undefined = weight.is_undefined();
    let pattern_was_undefined = pattern.is_undefined();
    let weight = weight.as_number().and_then(Fraction::from_f64);
    Ok(Some((
        weight,
        pattern,
        weight_was_undefined,
        pattern_was_undefined,
    )))
}

/// Prepare `stepcat`/`timecat` arguments without invoking `reify` for a
/// multi-entry zero-width branch. Pinned Strudel resolves missing weights and
/// checks `Fraction(time).eq(0)` before it reifies each branch; a singleton is
/// deliberately different and always reifies before applying its step count.
pub(crate) fn stepcat_items_bridged<'js>(
    ctx: &Ctx<'js>,
    args: &[rquickjs::Value<'js>],
) -> rquickjs::Result<BridgedStepcatItems<'js>> {
    let mut raw_items = Vec::with_capacity(args.len());
    for arg in args {
        match stepcat_pair_bridged(arg)? {
            Some((weight, pattern, weight_was_undefined, pattern_was_undefined)) => {
                if args.len() == 1 && weight_was_undefined {
                    if pattern_was_undefined {
                        return Err(throw_type_error(
                            ctx,
                            "Cannot read properties of undefined (reading '__steps_source')",
                        ));
                    }
                    if pattern.is_null() {
                        return Err(throw_type_error(
                            ctx,
                            "Cannot read properties of null (reading '__steps_source')",
                        ));
                    }
                }
                raw_items.push((weight, pattern));
            }
            None => {
                if arg.is_undefined() {
                    return Err(throw_type_error(
                        ctx,
                        "Cannot read properties of undefined (reading '_steps')",
                    ));
                }
                if arg.is_null() {
                    return Err(throw_type_error(
                        ctx,
                        "Cannot read properties of null (reading '_steps')",
                    ));
                }
                // Pinned `findsteps` reads `x._steps ?? 1` BEFORE the later
                // `reify(x)`. A string parser may return a multi-step Pattern,
                // but that must not retroactively change this raw argument's
                // weight.
                let steps = unwrap_pattern(arg)
                    .and_then(|(raw, _)| raw.steps)
                    .or(Some(Fraction::ONE));
                raw_items.push((steps, arg.clone()));
            }
        }
    }

    // Resolve the same missing-weight average that core `stepcat` will use so
    // the JS bridge knows which raw values are skipped before reification.
    let mut resolved: Vec<Option<Fraction>> = raw_items.iter().map(|(w, _)| *w).collect();
    let all_weights_missing = resolved.iter().all(Option::is_none);
    if resolved.iter().any(Option::is_none) {
        let known: Vec<Fraction> = resolved.iter().filter_map(|weight| *weight).collect();
        // With no known weight the all-missing `fastcat` path below applies.
        // When the known weights have no representable average, the missing
        // weights stay unresolved and core `stepcat` refuses with the typed
        // limit.
        if let Some(average) = rustel_core::combinators::stepcat_missing_weight(&known) {
            for weight in &mut resolved {
                if weight.is_none() {
                    *weight = Some(average);
                }
            }
        }
    }

    let mut items = Vec::with_capacity(raw_items.len());
    let mut sidecars = Vec::new();
    for ((weight, raw), resolved_weight) in raw_items.into_iter().zip(resolved) {
        // With no known weight the result is
        // `fastcat(...timepats.map(x => x[1]))`. List-constructor reification
        // is recursive, so a nested array here is a subsequence rather than
        // one pure array-valued hap.
        if all_weights_missing {
            let (pattern, nested) = reify_list_element_bridged(ctx, &raw)?;
            items.push((weight, pattern));
            sidecars.extend(nested);
            continue;
        }
        if args.len() > 1 && resolved_weight == Some(Fraction::ZERO) {
            items.push((weight, rustel_core::silence()));
            continue;
        }
        let (pattern, sidecar) = reify_direct_bridged(ctx, &raw)?;
        items.push((weight, pattern));
        sidecars.push(sidecar);
    }
    Ok((items, sidecars))
}

/// Native graph builder behind the lexical `Pattern.prototype.tour` method.
///
/// The JavaScript wrapper passes the raw receiver first. Every other raw value
/// is deliberately cloned into its final position before `stepcat` reifies
/// anything: pinned `tour` expands the argument list first, so a configured
/// string parser runs once per occurrence rather than once per distinct input.
/// The expansion is charged before the expanded vector is allocated.
///
/// The mapped insertion groups keep array-valued arguments and receiver
/// occurrences nested, while the direct receiver and final `...many` tail go
/// through `[].concat` and flatten ordinary arrays one level. Custom
/// `Symbol.isConcatSpreadable`, Proxy traps, and inherited concat mutation
/// remain outside this native preflight boundary; ordinary arrays (including
/// holes, which become `undefined` under the later spread) follow the pinned
/// shape.
pub(crate) fn native_tour<'js>(
    ctx: Ctx<'js>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> NativeResult<'js> {
    let receiver = args
        .0
        .first()
        .cloned()
        .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()));
    let many = args.0.get(1..).unwrap_or_default();
    let many_len = u64::try_from(many.len()).ok();
    let grouped = many_len.and_then(|length| {
        length
            .checked_add(1)
            .and_then(|width| length.checked_mul(width))
    });
    // Each array's length claim (see `js_array_len`) is read ONCE; one that
    // cannot be read collapses the count to `None`, which the charge below
    // refuses.
    let widths = std::iter::once(&receiver)
        .chain(many)
        .map(|value| match value.as_array() {
            Some(array) => js_array_len(array).ok(),
            None => Some(1),
        })
        .collect::<Option<Vec<usize>>>();
    let entries = widths
        .as_deref()
        .zip(grouped)
        .and_then(|(widths, grouped)| {
            widths.iter().try_fold(grouped, |total, &width| {
                total.checked_add(u64::try_from(width).ok()?)
            })
        })
        .unwrap_or(u64::MAX);

    if let Err(limit) = rustel_core::charge_stepwise_entries("tour", entries) {
        return derive_wrapper(ctx, rustel_core::query_limit_pattern(limit), &[]);
    }
    // An unread width was charged as the maximum and refused above.
    let widths = widths.unwrap_or_default();

    // `entries` cannot exceed the public bound after a successful charge, so
    // this conversion is valid even on a narrow host.
    let capacity = usize::try_from(entries).expect("bounded tour entry count fits usize");
    let mut expanded = Vec::new();
    if expanded.try_reserve_exact(capacity).is_err() {
        let limit = rustel_core::mark_stepwise_refusal(rustel_core::QueryLimit::HostMemory);
        return derive_wrapper(ctx, rustel_core::query_limit_pattern(limit), &[]);
    }

    // n=2: mapped groups [a,b,this], [a,this,b], followed by the concat tail
    // [this,a,b]. Nested arrays stay intact in the groups and flatten only in
    // that final tail.
    for split in (1..=many.len()).rev() {
        expanded.extend(many[..split].iter().cloned());
        expanded.push(receiver.clone());
        expanded.extend(many[split..].iter().cloned());
    }
    // Exactly the charged widths, never a fresh length read: reading an
    // element can run a user getter that re-lengthens a LATER array, and a
    // re-read would then push past the reservation - or past `i32::MAX`.
    // Holes read as `undefined`, as under the pinned spread, and so do slots
    // a getter has truncated since.
    for (value, width) in std::iter::once(&receiver).chain(many).zip(widths) {
        if let Some(array) = value.as_array() {
            for index in 0..width {
                expanded.push(array.get(index)?);
            }
        } else {
            expanded.push(value.clone());
        }
    }
    debug_assert_eq!(expanded.len(), capacity);
    rustel_core::note_stepwise_entries_materialised(entries);

    let (items, sidecars) = stepcat_items_bridged(&ctx, &expanded)?;
    let mut pattern = rustel_core::combinators::stepcat(&items);
    if many.is_empty()
        && unwrap_pattern(&receiver).is_some_and(|(source, _)| source.steps.is_none())
    {
        // Pinned singleton `stepcat` calls `withSteps`, which deliberately
        // preserves `undefined` when the source Pattern has no step metadata.
        pattern = pattern.with_steps(None);
    }
    derive_wrapper(ctx, pattern, &sidecars)
}

/// Charge a JavaScript-planned shrink-list expansion before its first zoom or
/// `stepcat` wrapper is materialised. `entries` is the as-yet uncharged suffix;
/// `minimum_entries` is the complete array length. Keeping both values lets a
/// canonical `shrink`/`grow` consumer accept the exact array already charged by
/// the default helper without double-spending the query pool, while still
/// refusing an array that an override grew beyond the per-invocation boundary
/// outside a query. The optional return is a Pattern carrying the typed
/// refusal, since eager construction has no core refusal channel of its own.
pub(crate) fn native_shrinklist_charge<'js>(
    ctx: Ctx<'js>,
    entries: u64,
    minimum_entries: u64,
    canonical: bool,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let operation = if canonical {
        "shrink/grow"
    } else {
        "shrinklist"
    };
    let refusal = match rustel_core::charge_stepwise_entries(operation, entries) {
        Ok(()) if minimum_entries <= rustel_core::MAX_STEPWISE_ENTRIES => None,
        Ok(()) => Some(rustel_core::mark_stepwise_refusal(
            rustel_core::QueryLimit::StepwiseExpansion {
                operation,
                minimum_entries,
                limit: rustel_core::MAX_STEPWISE_ENTRIES,
            },
        )),
        Err(limit) => Some(limit),
    };
    match refusal {
        None => Ok(rquickjs::Value::new_undefined(ctx)),
        Some(limit) => derive_wrapper(ctx, rustel_core::query_limit_pattern(limit), &[])
            .map(rquickjs::Class::into_value),
    }
}

/// Materialisation is counted only after all planned zoom wrappers exist.
/// `growlist` deliberately reaches this through its dynamic
/// `this.shrinklist` call and therefore never charges or records a second time.
pub(crate) fn native_shrinklist_materialised(entries: u64) {
    rustel_core::note_stepwise_entries_materialised(entries);
}

pub(crate) fn shrinklist_fraction_projection(text: &str) -> Option<Fraction> {
    let (numerator, denominator) = text.split_once('/').unwrap_or((text, "1"));
    let numerator = numerator.parse::<i128>().ok()?;
    let denominator = denominator.parse::<i128>().ok()?;
    (numerator != i128::MIN && denominator > 0).then(|| Fraction::new(numerator, denominator))
}

/// Exact canonical zoom path for the list helpers. Registered combinator
/// arguments normally travel through Pattern reification; passing a
/// fraction.js object there would turn the endpoint into an opaque value, and
/// passing a binary64 projection would lose exact non-binary rationals. The JS
/// planner therefore supplies lexical `Fraction#toFraction()` strings only
/// when the dynamically-read method is still the canonical zoom. An override
/// remains entirely in JavaScript and receives the original Fraction objects.
pub(crate) fn native_shrinklist_zoom<'js>(
    ctx: Ctx<'js>,
    source: rquickjs::Class<'js, NativePatternWrapper<'js>>,
    start: String,
    end: String,
    canonical: bool,
) -> NativeResult<'js> {
    let (Some(start), Some(end)) = (
        shrinklist_fraction_projection(&start),
        shrinklist_fraction_projection(&end),
    ) else {
        let limit = rustel_core::QueryLimit::NativeFraction {
            operation: if canonical {
                "shrink/grow"
            } else {
                "shrinklist"
            },
        };
        return ownership_refusal_wrapper(&ctx, limit);
    };
    let receiver = pattern_with_own_steps(&ctx, &source)?;
    let mut result = receiver.zoom(start, end);
    if start >= end {
        // Core's raw zero-width zoom is an empty one-step wrapper. Pinned
        // register() derives a distinct wrapper from `nothing`, retaining
        // zero steps without sharing the lexical singleton's identity. That
        // wrapper also retains the zoom receiver's ownership even though its
        // empty graph cannot reach those ids, so bypass reachability pruning
        // for this metadata-only derivation.
        result = result.with_steps(Some(Fraction::ZERO));
        let wrapper = {
            let borrowed = source.borrow();
            borrowed.with_pattern(result)
        };
        return new_wrapper(&ctx, wrapper);
    }
    let sidecar = {
        let borrowed = source.borrow();
        Sidecar::of(&borrowed)
    };
    derive_wrapper(ctx, result, &[sidecar])
}

/// Native graph builder for the guarded canonical-helper fast path. JavaScript
/// still performs the dynamic `receiver.shrinklist` getter and call first; the
/// helper returns a private WeakMap-marked Array only when its method, Pattern
/// metadata, Array primitives and zoom method are all the captured canonical
/// values. Core then owns the single resource charge and graph materialisation.
pub(crate) fn native_canonical_shrink_grow<'js>(
    ctx: Ctx<'js>,
    source: rquickjs::Class<'js, NativePatternWrapper<'js>>,
    amount: String,
    grow: bool,
) -> NativeResult<'js> {
    let Some(mut amount) = shrinklist_fraction_projection(&amount) else {
        return ownership_refusal_wrapper(
            &ctx,
            rustel_core::QueryLimit::NativeFraction {
                operation: "shrink/grow",
            },
        );
    };
    if grow {
        let Some(original) = amount.checked_neg() else {
            return ownership_refusal_wrapper(
                &ctx,
                rustel_core::QueryLimit::NativeFraction {
                    operation: "shrink/grow",
                },
            );
        };
        amount = original;
    }
    let receiver = pattern_with_own_steps(&ctx, &source)?;
    let result = if grow {
        rustel_core::combinators::grow(&receiver, amount)
    } else {
        rustel_core::combinators::shrink(&receiver, amount)
    };
    let sidecar = {
        let borrowed = source.borrow();
        Sidecar::of(&borrowed)
    };
    derive_wrapper(ctx, result, &[sidecar])
}

/// Native graph builder behind the lexical free `zip(...pats)` declaration.
///
/// The JavaScript wrapper has already applied the `hasSteps` filter.
/// Values that survive are therefore consumed strictly as Patterns: this path
/// must not invoke the mutable string parser or any other reifier. Each source
/// sidecar is retained because the resulting graph can query every operand
/// after the original wrappers have been collected.
pub(crate) fn native_zip<'js>(
    ctx: Ctx<'js>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> NativeResult<'js> {
    let mut patterns = Vec::new();
    if patterns.try_reserve_exact(args.0.len()).is_err() {
        let limit = rustel_core::mark_stepwise_refusal(rustel_core::QueryLimit::HostMemory);
        return derive_wrapper(ctx, rustel_core::query_limit_pattern(limit), &[]);
    }
    let mut sidecars = Vec::new();
    if sidecars.try_reserve_exact(args.0.len()).is_err() {
        let limit = rustel_core::mark_stepwise_refusal(rustel_core::QueryLimit::HostMemory);
        return derive_wrapper(ctx, rustel_core::query_limit_pattern(limit), &[]);
    }

    for value in &args.0 {
        let Some((pattern, sidecar)) = unwrap_pattern(value) else {
            return Err(throw_type_error(&ctx, "pat._slow is not a function"));
        };
        if pattern.steps.is_some_and(|steps| steps.numer() == 0) {
            return Err(rquickjs::Exception::throw_message(&ctx, "Division by Zero"));
        }
        patterns.push(pattern);
        sidecars.push(sidecar);
    }

    derive_wrapper(ctx, rustel_core::zip(patterns), &sidecars)
}
