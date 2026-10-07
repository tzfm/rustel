use super::{
    values::{coerced_number, coerced_string},
    *,
};

pub(super) fn console_call<'js>(
    ctx: Ctx<'js>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
    level: Option<&'static str>,
) {
    let mut parts = Vec::with_capacity(args.0.len());
    for value in args.0 {
        let Ok(value) = coerced_string(&ctx, value) else {
            return;
        };
        parts.push(value);
    }
    let Ok(logger) = ctx.globals().get::<_, Function>("logger") else {
        return;
    };
    match level {
        Some(level) => {
            let _ = logger.call::<_, ()>((parts.join(" "), level));
        }
        None => {
            let _ = logger.call::<_, ()>((parts.join(" "),));
        }
    }
}

pub(super) fn resolved_undefined<'js>(
    ctx: Ctx<'js>,
    _args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Promise<'js>> {
    let (promise, resolve, _) = rquickjs::Promise::new(&ctx)?;
    resolve.call::<_, ()>((rquickjs::Value::new_undefined(ctx),))?;
    Ok(promise)
}

pub(super) fn empty_object<'js>(
    ctx: Ctx<'js>,
    _args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Object<'js>> {
    rquickjs::Object::new(ctx)
}

pub(super) fn filter_callback<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
    haps: bool,
) -> NativeResult<'js> {
    if args.0.is_empty() {
        let name = if haps { ".filterHaps" } else { ".filterValues" };
        return Err(refuse_empty_call(&ctx, name, 1));
    }
    let (receiver, mut sidecars) = {
        let wrapper = this.0.borrow();
        (wrapper.pattern.clone(), vec![Sidecar::of(&wrapper)])
    };
    let Some(function) = args.0.first().and_then(rquickjs::Value::as_function) else {
        return derive_wrapper(
            ctx,
            rustel_core::query_error_pattern("filter predicate is not a function"),
            &sidecars,
        );
    };
    let (id, sidecar) = bridge_callable(&ctx, function.clone())?;
    sidecars.push(sidecar);
    let pattern = if haps {
        receiver.filter_haps_js(id)
    } else {
        receiver.filter_values_js(id)
    };
    derive_wrapper(ctx, pattern, &sidecars)
}

pub(super) fn with_query_span<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> NativeResult<'js> {
    if args.0.is_empty() {
        return Err(refuse_empty_call(&ctx, ".withQuerySpan", 1));
    }
    let (receiver, mut sidecars) = {
        let wrapper = this.0.borrow();
        (wrapper.pattern.clone(), vec![Sidecar::of(&wrapper)])
    };
    let Some(function) = args.0.first().and_then(rquickjs::Value::as_function) else {
        return derive_wrapper(
            ctx,
            rustel_core::query_error_pattern("withQuerySpan callback is not a function"),
            &sidecars,
        );
    };
    let (id, sidecar) = bridge_callable(&ctx, function.clone())?;
    sidecars.push(sidecar);
    derive_wrapper(ctx, receiver.with_query_span_js(id), &sidecars)
}

pub(super) fn sort_haps_by_part<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
) -> NativeResult<'js> {
    let (pattern, sidecar) = {
        let wrapper = this.0.borrow();
        (
            wrapper.pattern.sort_haps_by_part_pattern(),
            Sidecar::of(&wrapper),
        )
    };
    derive_wrapper(ctx, pattern, &[sidecar])
}

fn fraction_gcd(mut left: Fraction, mut right: Fraction) -> Fraction {
    while right != Fraction::ZERO {
        let remainder = left.rem(right);
        left = right;
        right = remainder;
    }
    left
}

fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

pub(super) fn draw_line<'js>(
    ctx: Ctx<'js>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<String> {
    let pattern_value = args
        .0
        .first()
        .cloned()
        .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()));
    let chars = match args.0.get(1) {
        Some(value) => coerced_number(&ctx, value.clone())?,
        None => 60.0,
    };
    if !chars.is_finite() || chars > 100_000.0 {
        return Err(rquickjs::Exception::throw_range(
            &ctx,
            "drawLine width is too large",
        ));
    }
    let (pattern, sidecar) = reify_bridged(&ctx, &pattern_value)?;
    let owner = derive_wrapper(ctx.clone(), pattern.clone(), &[sidecar])?;
    let _scope = WrapperQueryScope::push(&ctx, owner)?;
    let mut cycle = 0_i64;
    let mut position = Fraction::ZERO;
    let mut lines = vec![String::new()];
    let mut empty_line = String::new();
    while utf16_len(&lines[0]) < chars.max(0.0) as usize && cycle < chars.max(0.0) as i64 + 1 {
        let begin = Fraction::from(cycle);
        let haps = pattern.query_arc_sorted(begin, begin.add(Fraction::ONE));
        let durations: Vec<Fraction> = haps
            .iter()
            .filter(|hap| hap.has_onset())
            .map(rustel_core::Hap::duration)
            .collect();
        if durations.is_empty() {
            for line in &mut lines {
                line.push_str("|.");
            }
            empty_line.push_str("|.");
            position = position.add(Fraction::ONE);
            cycle += 1;
            continue;
        }
        let width = durations
            .into_iter()
            .reduce(fraction_gcd)
            .unwrap_or(Fraction::ONE);
        if width == Fraction::ZERO {
            break;
        }
        let slots = Fraction::ONE.div(width).to_f64();
        if !(0.0..=100_000.0).contains(&slots) {
            return Err(rquickjs::Exception::throw_range(
                &ctx,
                "drawLine resolution is too large",
            ));
        }
        for line in &mut lines {
            line.push('|');
        }
        empty_line.push('|');
        for _ in 0..slots as usize {
            let slot_begin = position;
            let slot_end = position.add(width);
            let matches: Vec<&rustel_core::Hap> = haps
                .iter()
                .filter(|hap| {
                    hap.whole
                        .is_some_and(|whole| whole.begin <= slot_begin && whole.end >= slot_end)
                })
                .collect();
            while lines.len() < matches.len() {
                lines.push(empty_line.clone());
            }
            for (index, line) in lines.iter_mut().enumerate() {
                match matches.get(index) {
                    Some(hap) if hap.whole.is_some_and(|whole| whole.begin == slot_begin) => {
                        line.push_str(&hap.value.show());
                    }
                    Some(_) => line.push('-'),
                    None => line.push('.'),
                }
            }
            empty_line.push('.');
            position = slot_end;
        }
        cycle += 1;
    }
    Ok(lines.join("\n"))
}
