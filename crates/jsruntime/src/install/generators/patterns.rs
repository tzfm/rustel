use super::*;
use rustel_core::{Value, ops::PatOps, register::Registration};

#[derive(Clone)]
struct GeneratorOps {
    floor: Registration,
    log2: Registration,
    round: Registration,
    segment: Registration,
}

impl GeneratorOps {
    fn new() -> Self {
        let get = |name| {
            registry()
                .get(name)
                .unwrap_or_else(|| panic!("missing native registration {name}"))
                .clone()
        };
        Self {
            floor: get("floor"),
            log2: get("log2"),
            round: get("round"),
            segment: get("segment"),
        }
    }

    fn run(&self, count: Pattern) -> Pattern {
        let zero = rustel_core::pure(Value::F64(0.0));
        let ranged = rustel_core::compose::compose(
            &rustel_core::signal::saw(),
            &count,
            rustel_core::compose::ComposeOp::Mul,
            rustel_core::compose::default_alignment(),
        );
        let ranged = rustel_core::compose::compose(
            &ranged,
            &zero,
            rustel_core::compose::ComposeOp::Add,
            rustel_core::compose::default_alignment(),
        );
        let rounded = self.round.call(&[], ranged);
        self.segment.call(&[count], rounded)
    }

    fn bit_count(&self, value: Pattern) -> Pattern {
        let logged = self.log2.call(&[], value);
        let floored = self.floor.call(&[], logged);
        rustel_core::compose::compose(
            &floored,
            &rustel_core::pure(Value::F64(1.0)),
            rustel_core::compose::ComposeOp::Add,
            rustel_core::compose::default_alignment(),
        )
    }

    fn binary_n(&self, number: Pattern, bits: Pattern) -> Pattern {
        let positions = self.run(bits.clone());
        let positions = rustel_core::compose::compose(
            &positions,
            &rustel_core::pure(Value::F64(-1.0)),
            rustel_core::compose::ComposeOp::Mul,
            rustel_core::compose::default_alignment(),
        );
        let last = rustel_core::compose::compose(
            &bits,
            &rustel_core::pure(Value::F64(1.0)),
            rustel_core::compose::ComposeOp::Sub,
            rustel_core::compose::default_alignment(),
        );
        let positions = rustel_core::compose::compose(
            &positions,
            &last,
            rustel_core::compose::ComposeOp::Add,
            rustel_core::compose::default_alignment(),
        );
        let number = self.segment.call(&[bits], number);
        let shifted = rustel_core::compose::compose(
            &number,
            &positions,
            rustel_core::compose::ComposeOp::Brshift,
            rustel_core::compose::default_alignment(),
        );
        rustel_core::compose::compose(
            &shifted,
            &rustel_core::pure(Value::F64(1.0)),
            rustel_core::compose::ComposeOp::Band,
            rustel_core::compose::default_alignment(),
        )
    }
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

fn argument_with_default<'js>(
    ctx: &Ctx<'js>,
    args: &[rquickjs::Value<'js>],
    index: usize,
    default: f64,
) -> rquickjs::Value<'js> {
    let value = argument(ctx, args, index);
    if value.is_undefined() {
        rquickjs::Value::new_number(ctx.clone(), default)
    } else {
        value
    }
}

fn js_to_int32(value: f64) -> i32 {
    if !value.is_finite() {
        return 0;
    }
    let value = value.trunc().rem_euclid(4_294_967_296.0);
    if value >= 2_147_483_648.0 {
        (value - 4_294_967_296.0) as i32
    } else {
        value as i32
    }
}

fn js_to_number(value: &Value) -> f64 {
    match value {
        Value::Undefined => f64::NAN,
        Value::Null => 0.0,
        Value::Bool(value) => f64::from(*value),
        Value::F64(value) => *value,
        Value::Str(value) => {
            let value = value.trim();
            if value.is_empty() {
                0.0
            } else if value == "Infinity" || value == "+Infinity" {
                f64::INFINITY
            } else if value == "-Infinity" {
                f64::NEG_INFINITY
            } else if let Some(value) = value
                .strip_prefix("0x")
                .or_else(|| value.strip_prefix("0X"))
            {
                u64::from_str_radix(value, 16).map_or(f64::NAN, |value| value as f64)
            } else if let Some(value) = value
                .strip_prefix("0b")
                .or_else(|| value.strip_prefix("0B"))
            {
                u64::from_str_radix(value, 2).map_or(f64::NAN, |value| value as f64)
            } else if let Some(value) = value
                .strip_prefix("0o")
                .or_else(|| value.strip_prefix("0O"))
            {
                u64::from_str_radix(value, 8).map_or(f64::NAN, |value| value as f64)
            } else {
                value.parse::<f64>().unwrap_or(f64::NAN)
            }
        }
        Value::List(_)
        | Value::Object(_)
        | Value::Pattern(_)
        | Value::JsValue(_)
        | Value::Function(_)
        | Value::Haps(_) => f64::NAN,
    }
}

fn binary_list_value(number: &Value, bits: &Value) -> Value {
    let number = js_to_number(number);
    let bits = js_to_number(bits);
    let start = bits - 1.0;
    if start.is_nan() || start < 0.0 {
        return Value::List(Vec::new());
    }
    let count = if start >= u64::MAX as f64 {
        // Past u64's range the cast saturates; the refusal reports the least count over the limit.
        rustel_core::MAX_STEPWISE_ENTRIES + 1
    } else {
        start.floor() as u64 + 1
    };
    if rustel_core::charge_stepwise_entries("binaryNL", count).is_err() {
        return Value::Undefined;
    }
    let mut values = Vec::new();
    if values.try_reserve_exact(count as usize).is_err() {
        rustel_core::refuse_host_memory();
        return Value::Undefined;
    }
    let number = js_to_int32(number);
    for index in 0..count {
        let shift = js_to_int32(start - index as f64) as u32 & 31;
        values.push(Value::F64(f64::from((number >> shift) & 1)));
    }
    Value::List(values)
}

fn binary_list(number: Pattern, bits: Pattern) -> Pattern {
    number.app_left_with(bits, binary_list_value)
}

fn sliced_digits(mut digits: Vec<Value>, places: &Value) -> Vec<Value> {
    if !places.js_truthy() {
        return digits;
    }
    let places = js_to_number(places);
    if (digits.len() as f64).partial_cmp(&places) != Some(std::cmp::Ordering::Greater) {
        return digits;
    }
    let start = (-places).trunc();
    let index = if start.is_infinite() && start.is_sign_negative() {
        0
    } else if start < 0.0 {
        (digits.len() as f64 + start).max(0.0) as usize
    } else if start.is_nan() {
        0
    } else {
        (start as usize).min(digits.len())
    };
    digits.drain(..index);
    digits
}

fn digit_pattern<P: PatOps>(number: &Value, radix: &Value, places: &Value) -> P {
    let mut number = js_to_number(number);
    let radix = js_to_number(radix);
    let mut digits = Vec::new();
    while number > 0.0 {
        if digits.len() as u64 == rustel_core::MAX_STEPWISE_ENTRIES {
            return P::pat_query_limit(rustel_core::QueryLimit::StepwiseExpansion {
                operation: "base",
                minimum_entries: rustel_core::MAX_STEPWISE_ENTRIES + 1,
                limit: rustel_core::MAX_STEPWISE_ENTRIES,
            });
        }
        if let Err(limit) = rustel_core::charge_stepwise_entries("base", 1) {
            return P::pat_query_limit(limit);
        }
        if digits.try_reserve(1).is_err() {
            return P::pat_query_limit(rustel_core::QueryLimit::HostMemory);
        }
        digits.push(Value::F64(number % radix));
        number = (number / radix).floor();
    }
    digits.reverse();
    let digits = sliced_digits(digits, places);
    if digits.is_empty() {
        P::pat_silence()
    } else {
        P::pat_fastcat(digits.into_iter().map(P::pat_pure).collect())
    }
}

fn base_inner<P: PatOps>(number: P, radix: P, places: P) -> P {
    places.squeeze_bind(move |places| {
        let places = places.clone();
        let number = number.clone();
        radix.squeeze_bind(move |radix| {
            let radix = radix.clone();
            let places = places.clone();
            number.squeeze_bind(move |number| digit_pattern::<P>(number, &radix, &places))
        })
    })
}

fn base(number: Pattern, radix: Pattern, places: Pattern) -> Pattern {
    match (
        number.as_pure_pattern(),
        radix.as_pure_pattern(),
        places.as_pure_pattern(),
    ) {
        (Some(number), Some(radix), Some(places)) => {
            base_inner(number, radix, places).pattern().clone()
        }
        _ => base_inner(number, radix, places),
    }
}

fn install_function<'js>(
    globals: &rquickjs::Object<'js>,
    name: &str,
    function: Function<'js>,
    length: usize,
) -> Result<(), String> {
    configure_function(&function, "", length, false).map_err(|error| error.to_string())?;
    globals
        .set(name, function)
        .map_err(|error| error.to_string())
}

pub(super) fn install<'js>(
    ctx: &Ctx<'js>,
    globals: &rquickjs::Object<'js>,
    pointer: Option<&rustel_core::host_value::Pointer>,
) -> Result<(), String> {
    for (name, pattern) in rustel_core::signal::signals(pointer) {
        let wrapper = new_wrapper(ctx, NativePatternWrapper::plain(pattern))
            .map_err(|error| error.to_string())?;
        globals
            .set(name, wrapper)
            .map_err(|error| error.to_string())?;
    }

    let irand = Function::new(
        ctx.clone(),
        hr_rest(move |ctx, args| {
            let value = argument(&ctx, &args.0, 0);
            let (pattern, sidecar) = reify_bridged(&ctx, &value)?;
            derive_wrapper(ctx, rustel_core::signal::irand(&pattern), &[sidecar])
        }),
    )
    .map_err(|error| error.to_string())?;
    globals
        .set("irand", irand)
        .map_err(|error| error.to_string())?;

    let rand_list = Function::new(
        ctx.clone(),
        hr_rest(move |ctx, args| {
            let value = argument(&ctx, &args.0, 0);
            let (pattern, sidecar) = reify_bridged(&ctx, &value)?;
            derive_wrapper(ctx, rustel_core::signal::rand_list(&pattern), &[sidecar])
        }),
    )
    .map_err(|error| error.to_string())?;
    globals
        .set("randL", rand_list)
        .map_err(|error| error.to_string())?;

    let randrun = Function::new(
        ctx.clone(),
        hr_rest(move |ctx, args| {
            // Core allocates and sorts `count` entries per sample, so the count
            // is charged against the stepwise pool; the saturating cast makes
            // an infinite count a refusal and a negative or NaN one empty.
            let count = args
                .0
                .first()
                .and_then(rquickjs::Value::as_number)
                .unwrap_or(0.0) as u64;
            if let Err(limit) = rustel_core::charge_stepwise_entries("randrun", count) {
                return derive_wrapper(ctx, rustel_core::query_limit_pattern(limit), &[]);
            }
            derive_wrapper(ctx, rustel_core::signal::randrun(count as usize), &[])
        }),
    )
    .map_err(|error| error.to_string())?;
    globals
        .set("randrun", randrun)
        .map_err(|error| error.to_string())?;

    let app_left = Function::new(
        ctx.clone(),
        hr_rest(move |ctx, args| {
            let left = argument(&ctx, &args.0, 0);
            let right = argument(&ctx, &args.0, 1);
            let (left, left_sidecar) = reify_bridged(&ctx, &left)?;
            let (right, right_sidecar) = reify_bridged(&ctx, &right)?;
            derive_wrapper(
                ctx,
                rustel_core::app_left_call(&left, &right),
                &[left_sidecar, right_sidecar],
            )
        }),
    )
    .map_err(|error| error.to_string())?;
    globals
        .set("__rustelAppLeft", app_left)
        .map_err(|error| error.to_string())?;

    let app_both = Function::new(
        ctx.clone(),
        hr_rest(move |ctx, args| {
            let left = argument(&ctx, &args.0, 0);
            let right = argument(&ctx, &args.0, 1);
            let (left, left_sidecar) = reify_bridged(&ctx, &left)?;
            let (right, right_sidecar) = reify_bridged(&ctx, &right)?;
            derive_wrapper(
                ctx,
                rustel_core::app_both_call(&left, &right),
                &[left_sidecar, right_sidecar],
            )
        }),
    )
    .map_err(|error| error.to_string())?;
    globals
        .set("__rustelAppBoth", app_both)
        .map_err(|error| error.to_string())?;

    let brand_by = Function::new(
        ctx.clone(),
        hr_rest(move |ctx, args| {
            let value = argument(&ctx, &args.0, 0);
            let (pattern, sidecar) = reify_bridged(&ctx, &value)?;
            derive_wrapper(ctx, rustel_core::signal::brand_by(&pattern), &[sidecar])
        }),
    )
    .map_err(|error| error.to_string())?;
    globals
        .set("brandBy", brand_by)
        .map_err(|error| error.to_string())?;

    let ops = GeneratorOps::new();
    let run_ops = ops.clone();
    let run = Function::new(
        ctx.clone(),
        hr_rest(move |ctx, args| {
            let count = argument(&ctx, &args.0, 0);
            let (count, sidecar) = reify_bridged(&ctx, &count)?;
            derive_wrapper(ctx, run_ops.run(count), &[sidecar])
        }),
    )
    .map_err(|error| error.to_string())?;
    install_function(globals, "run", run, 1)?;

    let reference = Function::new(
        ctx.clone(),
        hr_rest(move |ctx, args| {
            let accessor = argument(&ctx, &args.0, 0);
            let Some(function) = accessor.as_function() else {
                return derive_wrapper(
                    ctx,
                    rustel_core::query_error_pattern("ref accessor is not a function"),
                    &[],
                );
            };
            let (id, sidecar) = bridge_callable(&ctx, function.clone())?;
            derive_wrapper(ctx, rustel_core::ref_pattern(id), &[sidecar])
        }),
    )
    .map_err(|error| error.to_string())?;
    install_function(globals, "ref", reference, 1)?;

    // `signal(f)` is the `time` signal mapped through `f`: one hap per query
    // span, valued `f(begin)` with the begin in cycles as a number.
    let signal = Function::new(
        ctx.clone(),
        hr_rest(move |ctx, args| {
            let function = argument(&ctx, &args.0, 0);
            let Some(function) = function.as_function() else {
                return derive_wrapper(
                    ctx,
                    rustel_core::query_error_pattern("signal expects a function of time"),
                    &[],
                );
            };
            let (id, sidecar) = bridge_callable(&ctx, function.clone())?;
            derive_wrapper(ctx, rustel_core::signal::time().fmap_js(id), &[sidecar])
        }),
    )
    .map_err(|error| error.to_string())?;
    install_function(globals, "signal", signal, 1)?;

    let binary_n_ops = ops.clone();
    let binary_n = Function::new(
        ctx.clone(),
        hr_rest(move |ctx, args| {
            let bits = argument_with_default(&ctx, &args.0, 1, 16.0);
            let (bits, bits_sidecar) = reify_bridged(&ctx, &bits)?;
            let number = argument(&ctx, &args.0, 0);
            let (number, number_sidecar) = reify_bridged(&ctx, &number)?;
            derive_wrapper(
                ctx,
                binary_n_ops.binary_n(number, bits),
                &[bits_sidecar, number_sidecar],
            )
        }),
    )
    .map_err(|error| error.to_string())?;
    install_function(globals, "binaryN", binary_n, 1)?;

    let binary_ops = ops.clone();
    let binary = Function::new(
        ctx.clone(),
        hr_rest(move |ctx, args| {
            let input = argument(&ctx, &args.0, 0);
            let (for_bits, bits_sidecar) = reify_bridged(&ctx, &input)?;
            let bits = binary_ops.bit_count(for_bits);
            let (number, number_sidecar) = reify_bridged(&ctx, &input)?;
            derive_wrapper(
                ctx,
                binary_ops.binary_n(number, bits),
                &[bits_sidecar, number_sidecar],
            )
        }),
    )
    .map_err(|error| error.to_string())?;
    install_function(globals, "binary", binary, 1)?;

    let binary_nl = Function::new(
        ctx.clone(),
        hr_rest(move |ctx, args| {
            let number = argument(&ctx, &args.0, 0);
            let (number, number_sidecar) = reify_bridged(&ctx, &number)?;
            let bits = argument_with_default(&ctx, &args.0, 1, 16.0);
            let (bits, bits_sidecar) = reify_bridged(&ctx, &bits)?;
            derive_wrapper(
                ctx,
                binary_list(number, bits),
                &[number_sidecar, bits_sidecar],
            )
        }),
    )
    .map_err(|error| error.to_string())?;
    install_function(globals, "binaryNL", binary_nl, 1)?;

    let binary_l_ops = ops;
    let binary_l = Function::new(
        ctx.clone(),
        hr_rest(move |ctx, args| {
            let input = argument(&ctx, &args.0, 0);
            let (for_bits, bits_sidecar) = reify_bridged(&ctx, &input)?;
            let bits = binary_l_ops.bit_count(for_bits);
            let (number, number_sidecar) = reify_bridged(&ctx, &input)?;
            derive_wrapper(
                ctx,
                binary_list(number, bits),
                &[bits_sidecar, number_sidecar],
            )
        }),
    )
    .map_err(|error| error.to_string())?;
    install_function(globals, "binaryL", binary_l, 1)?;

    let base_function = Function::new(
        ctx.clone(),
        hr_rest(move |ctx, args| {
            let input = argument(&ctx, &args.0, 0);
            let (number, mut sidecars) = if is_array(&ctx, &input)? {
                sequence_args_bridged(&ctx, std::slice::from_ref(&input))?
            } else {
                let (pattern, sidecar) = reify_bridged(&ctx, &input)?;
                (pattern, vec![sidecar])
            };
            let radix = argument_with_default(&ctx, &args.0, 1, 10.0);
            let (radix, radix_sidecar) = reify_bridged(&ctx, &radix)?;
            let places = argument_with_default(&ctx, &args.0, 2, 0.0);
            let (places, places_sidecar) = reify_bridged(&ctx, &places)?;
            sidecars.push(radix_sidecar);
            sidecars.push(places_sidecar);
            derive_wrapper(ctx, base(number, radix, places), &sidecars)
        }),
    )
    .map_err(|error| error.to_string())?;
    install_function(globals, "base", base_function, 1)
}
