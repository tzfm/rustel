use super::{controls::append_pattern_value, values::coerced_number, *};

fn fade_gain(value: &Value) -> f64 {
    let value = rustel_core::util::parse_numeral(value).unwrap_or(f64::NAN);
    if value < 0.5 {
        1.0
    } else {
        1.0 - (value - 0.5) / 0.5
    }
}

fn xfade_pattern(a: Pattern, position: Pattern, b: Pattern) -> Pattern {
    let gain_a =
        position.fmap(|value| Value::object([("gain".into(), Value::F64(fade_gain(value)))]));
    let gain_b = position.fmap(|value| {
        let value = rustel_core::util::parse_numeral(value).unwrap_or(f64::NAN);
        Value::object([(
            "gain".into(),
            Value::F64(fade_gain(&Value::F64(1.0 - value))),
        )])
    });
    rustel_core::stack(vec![
        rustel_core::compose::compose(
            &a,
            &gain_a,
            rustel_core::compose::ComposeOp::Mul,
            rustel_core::compose::default_alignment(),
        ),
        rustel_core::compose::compose(
            &b,
            &gain_b,
            rustel_core::compose::ComposeOp::Mul,
            rustel_core::compose::default_alignment(),
        ),
    ])
}

pub(super) fn xfade_free<'js>(
    ctx: Ctx<'js>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> NativeResult<'js> {
    let mut patterns = Vec::with_capacity(3);
    let mut sidecars = Vec::with_capacity(3);
    for index in [0, 1, 2] {
        let value = args
            .0
            .get(index)
            .cloned()
            .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()));
        let (pattern, sidecar) = reify_bridged(&ctx, &value)?;
        patterns.push(pattern);
        sidecars.push(sidecar);
    }
    derive_wrapper(
        ctx,
        xfade_pattern(patterns.remove(0), patterns.remove(0), patterns.remove(0)),
        &sidecars,
    )
}

pub(super) fn xfade_method<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> NativeResult<'js> {
    let (a, mut sidecars) = {
        let wrapper = this.0.borrow();
        (wrapper.pattern.clone(), vec![Sidecar::of(&wrapper)])
    };
    let position = args
        .0
        .first()
        .cloned()
        .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()));
    let b = args
        .0
        .get(1)
        .cloned()
        .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()));
    let (position, position_sidecar) = reify_bridged(&ctx, &position)?;
    let (b, b_sidecar) = reify_bridged(&ctx, &b)?;
    sidecars.extend([position_sidecar, b_sidecar]);
    derive_wrapper(ctx, xfade_pattern(a, position, b), &sidecars)
}

fn morph_value(value: &Value) -> Pattern {
    let Value::List(parts) = rustel_core::materialize_js_value(value) else {
        return rustel_core::query_error_pattern("morph expects three values");
    };
    let Some(Value::List(from)) = parts.first().map(rustel_core::materialize_js_value) else {
        return rustel_core::query_error_pattern("morph expects an array source");
    };
    let Some(Value::List(to)) = parts.get(1).map(rustel_core::materialize_js_value) else {
        return rustel_core::query_error_pattern("morph expects an array target");
    };
    let Some(by) = parts
        .get(2)
        .and_then(rustel_core::register::value_to_fraction)
    else {
        return rustel_core::query_error_pattern("morph amount is invalid");
    };
    if from.is_empty() {
        return rustel_core::silence();
    }
    let from_positions: Vec<usize> = from
        .iter()
        .enumerate()
        .filter_map(|(index, value)| value.js_truthy().then_some(index))
        .collect();
    let to_positions: Vec<usize> = to
        .iter()
        .enumerate()
        .filter_map(|(index, value)| value.js_truthy().then_some(index))
        .collect();
    if to_positions.len() < from_positions.len() || to.is_empty() {
        return rustel_core::query_error_pattern("morph masks have incompatible onsets");
    }
    let duration = Fraction::ONE.div(Fraction::from(from.len() as i64));
    let from_len = Fraction::from(from.len() as i64);
    let to_len = Fraction::from(to.len() as i64);
    // A groove offset outside the native fraction range refuses the query.
    let patterns: Option<Vec<_>> = from_positions
        .into_iter()
        .zip(to_positions)
        .map(|(from, to)| {
            let from = Fraction::from(from as i64).div(from_len);
            let to = Fraction::from(to as i64).div(to_len);
            let begin = by.checked_mul(to.checked_sub(from)?)?.checked_add(from)?;
            let end = begin.checked_add(duration)?;
            Some(rustel_core::pure(Value::Bool(true)).compress(begin, end))
        })
        .collect();
    let Some(patterns) = patterns else {
        return rustel_core::query_limit_pattern(rustel_core::mark_stepwise_refusal(
            rustel_core::QueryLimit::NativeFraction { operation: "morph" },
        ));
    };
    rustel_core::stack(patterns).split_queries()
}

pub(super) fn morph<'js>(
    ctx: Ctx<'js>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> NativeResult<'js> {
    let mut sidecars = Vec::with_capacity(3);
    let mut patterns = Vec::with_capacity(3);
    for index in 0..3 {
        let value = args
            .0
            .get(index)
            .cloned()
            .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()));
        let (pattern, sidecar) = reify_bridged(&ctx, &value)?;
        sidecars.push(sidecar);
        patterns.push(pattern);
    }
    let from = patterns.remove(0).fmap_collect();
    let combined = from
        .app_right_with(patterns.remove(0), append_pattern_value)
        .app_right_with(patterns.remove(0), append_pattern_value);
    let pattern = combined
        .fmap_to_pure_pattern(|value| {
            rustel_core::purity::PurePattern::assert_pure(morph_value(value))
        })
        .inner_join();
    derive_wrapper(ctx, pattern, &sidecars)
}

pub(super) fn seq_p_loop<'js>(
    ctx: Ctx<'js>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> NativeResult<'js> {
    let mut previous = Fraction::ZERO;
    let mut sections = Vec::with_capacity(args.0.len());
    let mut sidecars = Vec::with_capacity(args.0.len());
    for value in args.0 {
        let section = value.as_array().ok_or_else(|| {
            rquickjs::Exception::throw_type(&ctx, "seqPLoop sections must be arrays")
        })?;
        // Only the length is compared, but through the guard (see
        // `js_array_len`).
        let (start_index, stop_index, pattern_index) = if js_array_len(section)? == 2 {
            (None, 0, 1)
        } else {
            (Some(0), 1, 2)
        };
        let start = match start_index {
            Some(index) => {
                let value: rquickjs::Value = section.get(index)?;
                Fraction::from_f64(coerced_number(&ctx, value)?).ok_or_else(|| {
                    rquickjs::Exception::throw_range(&ctx, "seqPLoop start must be finite")
                })?
            }
            None => previous,
        };
        let stop_value: rquickjs::Value = section.get(stop_index)?;
        let stop = Fraction::from_f64(coerced_number(&ctx, stop_value)?).ok_or_else(|| {
            rquickjs::Exception::throw_range(&ctx, "seqPLoop stop must be finite")
        })?;
        let pattern_value: rquickjs::Value = section.get(pattern_index)?;
        let (pattern, sidecar) = reify_bridged(&ctx, &pattern_value)?;
        sidecars.push(sidecar);
        sections.push((start, stop, pattern));
        previous = stop;
    }
    let total = previous;
    if total == Fraction::ZERO {
        return derive_wrapper(ctx, rustel_core::silence(), &sidecars);
    }
    // A bound scaled by the total can leave the native fraction range; that
    // refuses like a non-finite bound.
    let patterns = sections
        .into_iter()
        .map(|(start, stop, pattern)| {
            let (begin, end) = start
                .checked_div(total)
                .zip(stop.checked_div(total))
                .ok_or_else(|| {
                    rquickjs::Exception::throw_range(
                        &ctx,
                        "seqPLoop section bounds exceed the native fraction range",
                    )
                })?;
            Ok(rustel_core::pure_pattern(pattern).compress(begin, end))
        })
        .collect::<rquickjs::Result<Vec<_>>>()?;
    let pattern = rustel_core::stack(patterns).slow(total).inner_join();
    derive_wrapper(ctx, pattern, &sidecars)
}
