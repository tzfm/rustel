use super::*;
use rquickjs::{
    Coerced,
    function::{Args, Rest, This},
    object::Property,
};

pub(super) const FOUNDATION_STATE: &str = "__native_foundation_state";

const RAW_FRACTION: &str = "rawFraction";
const FRACTION_SEED: &str = "fractionSeed";
const FRACTION: &str = "fraction";
const HAP: &str = "Hap";
const TIME_SPAN: &str = "TimeSpan";
const STATE: &str = "State";
const RAW_VALUE_OF: &str = "rawValueOf";

#[derive(Clone, Copy)]
enum FractionMethod {
    Sam,
    NextSam,
    WholeCycle,
    CyclePos,
    Lt,
    Gt,
    Lte,
    Gte,
    Eq,
    Ne,
    Max,
    Maximum,
    Min,
    MulMaybe,
    DivMaybe,
    AddMaybe,
    SubMaybe,
    Show,
    Or,
}

#[derive(Clone, Copy)]
enum SpanMethod {
    SpanCycles,
    Duration,
    CycleArc,
    WithTime,
    WithEnd,
    WithCycle,
    Intersection,
    IntersectionE,
    Midpoint,
    Equals,
    Show,
}

#[derive(Clone, Copy)]
enum HapMethod {
    Duration,
    EndClipped,
    IsActive,
    IsInPast,
    IsInNearPast,
    IsInFuture,
    IsInNearFuture,
    IsWithinTime,
    WholeOrPart,
    WithSpan,
    WithValue,
    HasOnset,
    HasTag,
    ResolveState,
    SpanEquals,
    Equals,
    Show,
    ShowWhole,
    CombineContext,
    SetContext,
    EnsureObjectValue,
}

#[derive(Clone, Copy)]
enum StateMethod {
    SetSpan,
    WithSpan,
    SetControls,
}

pub(super) fn state<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<rquickjs::Object<'js>> {
    host_stack(ctx)?.as_object().get(FOUNDATION_STATE)
}

pub(super) fn strict_equal(
    ctx: &Ctx<'_>,
    left: &rquickjs::Value<'_>,
    right: &rquickjs::Value<'_>,
) -> bool {
    unsafe { rquickjs::qjs::JS_IsStrictEqual(ctx.as_raw().as_ptr(), left.as_raw(), right.as_raw()) }
}

pub(super) fn truthy<'js>(ctx: &Ctx<'js>, value: rquickjs::Value<'js>) -> rquickjs::Result<bool> {
    Ok(Coerced::<bool>::from_js(ctx, value)?.0)
}

pub(super) fn argument<'js>(
    ctx: &Ctx<'js>,
    args: &[rquickjs::Value<'js>],
    index: usize,
) -> rquickjs::Value<'js> {
    args.get(index)
        .cloned()
        .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()))
}

pub(super) fn call_with_this<'js>(
    function: &Function<'js>,
    this: rquickjs::Value<'js>,
    values: impl IntoIterator<Item = rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let mut args = Args::new_unsized(function.ctx().clone());
    args.this(this)?;
    args.push_args(values)?;
    args.apply(function)
}

pub(super) fn object_for_property<'js>(
    ctx: &Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Object<'js>> {
    if value.is_null() {
        return Err(throw_type_error(
            ctx,
            "Cannot convert undefined or null to object",
        ));
    }
    if value.is_undefined() {
        return Err(throw_type_error(
            ctx,
            "Cannot convert undefined or null to object",
        ));
    }
    if let Some(object) = value.as_object() {
        return Ok(object.clone());
    }
    let object: Function = ctx.globals().get("Object")?;
    object.call((value,))
}

pub(super) fn get_property<'js>(
    ctx: &Ctx<'js>,
    value: rquickjs::Value<'js>,
    name: &str,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    object_for_property(ctx, value)?.get(name)
}

pub(super) fn method<'js>(
    ctx: &Ctx<'js>,
    value: &rquickjs::Value<'js>,
    name: &str,
) -> rquickjs::Result<Function<'js>> {
    object_for_property(ctx, value.clone())?.get(name)
}

pub(super) fn call_method<'js>(
    ctx: &Ctx<'js>,
    receiver: rquickjs::Value<'js>,
    name: &str,
    values: impl IntoIterator<Item = rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let function = method(ctx, &receiver, name)?;
    call_with_this(&function, receiver, values)
}

pub(super) fn dynamic_is_array<'js>(
    ctx: &Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<bool> {
    let array: Function = ctx.globals().get("Array")?;
    let is_array: Function = array.get("isArray")?;
    is_array.call((This(array), value))
}

pub(super) fn exact_fraction<'js>(
    ctx: &Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let fraction: Function = state(ctx)?.get(FRACTION)?;
    fraction.call((value,))
}

fn raw_fraction<'js>(
    ctx: &Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let fraction: Function = state(ctx)?.get(RAW_FRACTION)?;
    fraction.call((value,))
}

pub(super) fn array<'js>(
    ctx: &Ctx<'js>,
    values: impl IntoIterator<Item = rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Array<'js>> {
    let result = rquickjs::Array::new(ctx.clone())?;
    for (index, value) in values.into_iter().enumerate() {
        result.set(index, value)?;
    }
    Ok(result)
}

fn destructure_pair<'js>(
    ctx: &Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<(rquickjs::Value<'js>, rquickjs::Value<'js>)> {
    let source = object_for_property(ctx, value.clone())?;
    let iterator: Function = source.get(rquickjs::Symbol::iterator(ctx.clone()))?;
    let iterator = call_with_this(&iterator, value, [])?;
    let iterator = object_for_property(ctx, iterator)?;
    let next: Function = iterator.get("next")?;
    let mut output = [
        rquickjs::Value::new_undefined(ctx.clone()),
        rquickjs::Value::new_undefined(ctx.clone()),
    ];
    let mut done = false;
    for slot in &mut output {
        let item = call_with_this(&next, iterator.clone().into_value(), [])?;
        let item = object_for_property(ctx, item)?;
        done = truthy(ctx, item.get("done")?)?;
        if done {
            break;
        }
        *slot = item.get("value")?;
    }
    if !done {
        let close: rquickjs::Value = iterator.get("return")?;
        if !close.is_null() && !close.is_undefined() {
            let close = close
                .into_function()
                .ok_or_else(|| throw_type_error(ctx, "iterator.return is not a function"))?;
            let result = call_with_this(&close, iterator.into_value(), [])?;
            if !result.is_object() {
                return Err(throw_type_error(
                    ctx,
                    "iterator.return returned a non-object",
                ));
            }
        }
    }
    let [first, second] = output;
    Ok((first, second))
}

fn construct_instance<'js>(
    ctx: &Ctx<'js>,
    class: &str,
    fields: impl IntoIterator<Item = (&'static str, rquickjs::Value<'js>)>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let constructor: Function = state(ctx)?.get(class)?;
    let prototype: rquickjs::Object = constructor.get("prototype")?;
    let instance = rquickjs::Object::new(ctx.clone())?;
    instance.set_prototype(Some(&prototype))?;
    for (name, value) in fields {
        instance.set(name, value)?;
    }
    Ok(instance.into_value())
}

fn construct_span<'js>(
    ctx: &Ctx<'js>,
    begin: rquickjs::Value<'js>,
    end: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let begin = raw_fraction(ctx, begin)?;
    let end = raw_fraction(ctx, end)?;
    construct_instance(ctx, TIME_SPAN, [("begin", begin), ("end", end)])
}

fn construct_hap<'js>(
    ctx: &Ctx<'js>,
    whole: rquickjs::Value<'js>,
    part: rquickjs::Value<'js>,
    value: rquickjs::Value<'js>,
    context: rquickjs::Value<'js>,
    stateful: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    construct_instance(
        ctx,
        HAP,
        [
            ("whole", whole),
            ("part", part),
            ("value", value),
            ("context", context),
            ("stateful", stateful),
        ],
    )
}

fn construct_state<'js>(
    ctx: &Ctx<'js>,
    span: rquickjs::Value<'js>,
    controls: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    construct_instance(ctx, STATE, [("span", span), ("controls", controls)])
}

fn class_target<'js>(
    ctx: &Ctx<'js>,
    this: rquickjs::Value<'js>,
    class: &str,
) -> rquickjs::Result<rquickjs::Object<'js>> {
    let Some(target) = this.as_function() else {
        return Err(throw_type_error(
            ctx,
            &format!("Class constructor {class} cannot be invoked without 'new'"),
        ));
    };
    let target_prototype: rquickjs::Value = target.get("prototype")?;
    let prototype = if let Some(prototype) = target_prototype.as_object() {
        prototype.clone()
    } else {
        let object: Function = ctx.globals().get("Object")?;
        object.get("prototype")?
    };
    let instance = rquickjs::Object::new(ctx.clone())?;
    instance.set_prototype(Some(&prototype))?;
    Ok(instance)
}

fn time_span_constructor<'js>(
    ctx: Ctx<'js>,
    this: This<rquickjs::Value<'js>>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let instance = class_target(&ctx, this.0, TIME_SPAN)?;
    instance.set("begin", raw_fraction(&ctx, argument(&ctx, &args.0, 0))?)?;
    instance.set("end", raw_fraction(&ctx, argument(&ctx, &args.0, 1))?)?;
    Ok(instance.into_value())
}

fn hap_constructor<'js>(
    ctx: Ctx<'js>,
    this: This<rquickjs::Value<'js>>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let instance = class_target(&ctx, this.0, HAP)?;
    let context = argument(&ctx, &args.0, 3);
    let context = if context.is_undefined() {
        rquickjs::Object::new(ctx.clone())?.into_value()
    } else {
        context
    };
    let stateful = argument(&ctx, &args.0, 4);
    let stateful = if stateful.is_undefined() {
        rquickjs::Value::new_bool(ctx.clone(), false)
    } else {
        stateful
    };
    for (name, value) in [
        ("whole", argument(&ctx, &args.0, 0)),
        ("part", argument(&ctx, &args.0, 1)),
        ("value", argument(&ctx, &args.0, 2)),
        ("context", context),
        ("stateful", stateful),
    ] {
        instance.set(name, value)?;
    }
    Ok(instance.into_value())
}

fn state_constructor<'js>(
    ctx: Ctx<'js>,
    this: This<rquickjs::Value<'js>>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let instance = class_target(&ctx, this.0, STATE)?;
    let controls = argument(&ctx, &args.0, 1);
    let controls = if controls.is_undefined() {
        rquickjs::Object::new(ctx.clone())?.into_value()
    } else {
        controls
    };
    instance.set("span", argument(&ctx, &args.0, 0))?;
    instance.set("controls", controls)?;
    Ok(instance.into_value())
}

fn fraction_method<'js>(
    kind: FractionMethod,
    ctx: Ctx<'js>,
    this: This<rquickjs::Value<'js>>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let this = this.0;
    match kind {
        FractionMethod::Sam => call_method(&ctx, this, "floor", []),
        FractionMethod::NextSam => {
            let sam = call_method(&ctx, this, "sam", [])?;
            call_method(&ctx, sam, "add", [rquickjs::Value::new_int(ctx.clone(), 1)])
        }
        FractionMethod::WholeCycle => {
            let sam = call_method(&ctx, this.clone(), "sam", [])?;
            let next = call_method(&ctx, this, "nextSam", [])?;
            construct_span(&ctx, sam, next)
        }
        FractionMethod::CyclePos => {
            let sam = call_method(&ctx, this.clone(), "sam", [])?;
            call_method(&ctx, this, "sub", [sam])
        }
        FractionMethod::Lt
        | FractionMethod::Gt
        | FractionMethod::Lte
        | FractionMethod::Gte
        | FractionMethod::Eq
        | FractionMethod::Ne => {
            let comparison: i32 = call_method(&ctx, this, "compare", [argument(&ctx, &args.0, 0)])?
                .as_int()
                .unwrap_or_default();
            let result = match kind {
                FractionMethod::Lt => comparison < 0,
                FractionMethod::Gt => comparison > 0,
                FractionMethod::Lte => comparison <= 0,
                FractionMethod::Gte => comparison >= 0,
                FractionMethod::Eq => comparison == 0,
                FractionMethod::Ne => comparison != 0,
                _ => unreachable!(),
            };
            Ok(rquickjs::Value::new_bool(ctx, result))
        }
        FractionMethod::Max | FractionMethod::Min => {
            let other = argument(&ctx, &args.0, 0);
            let method = if matches!(kind, FractionMethod::Max) {
                "gt"
            } else {
                "lt"
            };
            let keep: bool = call_method(&ctx, this.clone(), method, [other.clone()])?
                .as_bool()
                .unwrap_or(false);
            Ok(if keep { this } else { other })
        }
        FractionMethod::Maximum => {
            let mut maximum = this;
            for value in args.0 {
                let other = raw_fraction(&ctx, value)?;
                maximum = call_method(&ctx, other, "max", [maximum])?;
            }
            Ok(maximum)
        }
        FractionMethod::MulMaybe
        | FractionMethod::DivMaybe
        | FractionMethod::AddMaybe
        | FractionMethod::SubMaybe => {
            let other = argument(&ctx, &args.0, 0);
            if other.is_undefined() {
                return Ok(other);
            }
            let name = match kind {
                FractionMethod::MulMaybe => "mul",
                FractionMethod::DivMaybe => "div",
                FractionMethod::AddMaybe => "add",
                FractionMethod::SubMaybe => "sub",
                _ => unreachable!(),
            };
            call_method(&ctx, this, name, [other])
        }
        FractionMethod::Show => {
            let object = object_for_property(&ctx, this)?;
            let text = fraction::show_parts(&ctx, &object)?;
            Ok(rquickjs::String::from_str(ctx, &text)?.into_value())
        }
        FractionMethod::Or => {
            let zero = rquickjs::Value::new_int(ctx.clone(), 0);
            let empty = call_method(&ctx, this.clone(), "eq", [zero])?
                .as_bool()
                .unwrap_or(false);
            Ok(if empty {
                argument(&ctx, &args.0, 0)
            } else {
                this
            })
        }
    }
}

fn span_method<'js>(
    kind: SpanMethod,
    ctx: Ctx<'js>,
    this: This<rquickjs::Value<'js>>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let this = object_for_property(&ctx, this.0)?;
    let begin: rquickjs::Value = this.get("begin")?;
    let end: rquickjs::Value = this.get("end")?;
    match kind {
        SpanMethod::SpanCycles => {
            let spans = rquickjs::Array::new(ctx.clone())?;
            let end_sam = call_method(&ctx, end.clone(), "sam", [])?;
            let equal = call_method(&ctx, begin.clone(), "equals", [end.clone()])?
                .as_bool()
                .unwrap_or(false);
            if equal {
                spans.set(0, construct_span(&ctx, begin, end)?)?;
                return Ok(spans.into_value());
            }
            let mut cursor = begin;
            let mut index = 0usize;
            loop {
                let after = call_method(&ctx, end.clone(), "gt", [cursor.clone()])?
                    .as_bool()
                    .unwrap_or(false);
                if !after {
                    break;
                }
                let cursor_sam = call_method(&ctx, cursor.clone(), "sam", [])?;
                let same = call_method(&ctx, cursor_sam, "equals", [end_sam.clone()])?
                    .as_bool()
                    .unwrap_or(false);
                if same {
                    spans.set(index, construct_span(&ctx, cursor, this.get("end")?)?)?;
                    break;
                }
                let next = call_method(&ctx, cursor.clone(), "nextSam", [])?;
                spans.set(index, construct_span(&ctx, cursor, next.clone())?)?;
                index += 1;
                cursor = next;
            }
            Ok(spans.into_value())
        }
        SpanMethod::Duration => call_method(&ctx, end, "sub", [begin]),
        SpanMethod::CycleArc => {
            let start = call_method(&ctx, begin, "cyclePos", [])?;
            let duration: rquickjs::Value = this.get("duration")?;
            let finish = call_method(&ctx, start.clone(), "add", [duration])?;
            construct_span(&ctx, start, finish)
        }
        SpanMethod::WithTime => {
            let callback = argument(&ctx, &args.0, 0)
                .into_function()
                .ok_or_else(|| throw_type_error(&ctx, "f is not a function"))?;
            let begin = callback.call((begin,))?;
            let end = callback.call((end,))?;
            construct_span(&ctx, begin, end)
        }
        SpanMethod::WithEnd => {
            let callback = argument(&ctx, &args.0, 0)
                .into_function()
                .ok_or_else(|| throw_type_error(&ctx, "f is not a function"))?;
            let end = callback.call((end,))?;
            construct_span(&ctx, begin, end)
        }
        SpanMethod::WithCycle => {
            let callback = argument(&ctx, &args.0, 0)
                .into_function()
                .ok_or_else(|| throw_type_error(&ctx, "f is not a function"))?;
            let sam = call_method(&ctx, begin.clone(), "sam", [])?;
            let relative_begin = call_method(&ctx, begin, "sub", [sam.clone()])?;
            let relative_end = call_method(&ctx, end, "sub", [sam.clone()])?;
            let begin = callback.call((relative_begin,))?;
            let end = callback.call((relative_end,))?;
            let begin = call_method(&ctx, sam.clone(), "add", [begin])?;
            let end = call_method(&ctx, sam, "add", [end])?;
            construct_span(&ctx, begin, end)
        }
        SpanMethod::Intersection | SpanMethod::IntersectionE => {
            let other = object_for_property(&ctx, argument(&ctx, &args.0, 0))?;
            let other_begin: rquickjs::Value = other.get("begin")?;
            let other_end: rquickjs::Value = other.get("end")?;
            let clipped_begin = call_method(&ctx, begin.clone(), "max", [other_begin.clone()])?;
            let clipped_end = call_method(&ctx, end.clone(), "min", [other_end.clone()])?;
            let disjoint = call_method(&ctx, clipped_begin.clone(), "gt", [clipped_end.clone()])?
                .as_bool()
                .unwrap_or(false);
            let mut output = None;
            if !disjoint {
                let point =
                    call_method(&ctx, clipped_begin.clone(), "equals", [clipped_end.clone()])?
                        .as_bool()
                        .unwrap_or(false);
                let excluded = if point {
                    let at_own_end =
                        call_method(&ctx, clipped_begin.clone(), "equals", [end.clone()])?
                            .as_bool()
                            .unwrap_or(false);
                    let own_positive = call_method(&ctx, begin, "lt", [end])?
                        .as_bool()
                        .unwrap_or(false);
                    let at_other_end =
                        call_method(&ctx, clipped_begin.clone(), "equals", [other_end.clone()])?
                            .as_bool()
                            .unwrap_or(false);
                    let other_positive = call_method(&ctx, other_begin, "lt", [other_end])?
                        .as_bool()
                        .unwrap_or(false);
                    (at_own_end && own_positive) || (at_other_end && other_positive)
                } else {
                    false
                };
                if !excluded {
                    output = Some(construct_span(&ctx, clipped_begin, clipped_end)?);
                }
            }
            if let Some(output) = output {
                Ok(output)
            } else if matches!(kind, SpanMethod::IntersectionE) {
                let message =
                    rquickjs::String::from_str(ctx.clone(), "TimeSpans do not intersect")?;
                Err(ctx.throw(message.into_value()))
            } else {
                Ok(rquickjs::Value::new_undefined(ctx))
            }
        }
        SpanMethod::Midpoint => {
            let duration: rquickjs::Value = this.get("duration")?;
            let half = call_method(
                &ctx,
                duration,
                "div",
                [rquickjs::Value::new_int(ctx.clone(), 2)],
            )?;
            call_method(&ctx, begin, "add", [half])
        }
        SpanMethod::Equals => {
            let other = object_for_property(&ctx, argument(&ctx, &args.0, 0))?;
            let other_begin: rquickjs::Value = other.get("begin")?;
            let first = call_method(&ctx, begin, "equals", [other_begin])?
                .as_bool()
                .unwrap_or(false);
            if !first {
                return Ok(rquickjs::Value::new_bool(ctx, false));
            }
            let other_end: rquickjs::Value = other.get("end")?;
            let second = call_method(&ctx, end, "equals", [other_end])?
                .as_bool()
                .unwrap_or(false);
            Ok(rquickjs::Value::new_bool(ctx, second))
        }
        SpanMethod::Show => {
            let begin = call_method(&ctx, begin, "show", [])?;
            let end = call_method(&ctx, end, "show", [])?;
            let begin = Coerced::<String>::from_js(&ctx, begin)?.0;
            let end = Coerced::<String>::from_js(&ctx, end)?.0;
            Ok(rquickjs::String::from_str(ctx, &format!("{begin} → {end}"))?.into_value())
        }
    }
}

fn optional_property<'js>(
    ctx: &Ctx<'js>,
    value: rquickjs::Value<'js>,
    name: &str,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    if value.is_null() || value.is_undefined() {
        Ok(rquickjs::Value::new_undefined(ctx.clone()))
    } else {
        get_property(ctx, value, name)
    }
}

fn relational_number<'js>(
    ctx: &Ctx<'js>,
    left: rquickjs::Value<'js>,
    right: rquickjs::Value<'js>,
    predicate: impl FnOnce(f64, f64) -> bool,
) -> rquickjs::Result<bool> {
    let left = Coerced::<f64>::from_js(ctx, left)?.0;
    let right = Coerced::<f64>::from_js(ctx, right)?.0;
    Ok(predicate(left, right))
}

fn json_value<'js>(
    ctx: &Ctx<'js>,
    value: rquickjs::Value<'js>,
    compact: bool,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    if !value.is_object() {
        return Ok(value);
    }
    let json: rquickjs::Object = ctx.globals().get("JSON")?;
    let stringify: Function = json.get("stringify")?;
    let rendered: rquickjs::Value = stringify.call((This(json), value))?;
    if !compact {
        return Ok(rendered);
    }
    let mut text = Coerced::<String>::from_js(ctx, rendered)?.0;
    if text.len() >= 2 {
        text = text[1..text.len() - 1].to_owned();
    }
    text = text.replace('"', "").replace(',', " ");
    Ok(rquickjs::String::from_str(ctx.clone(), &text)?.into_value())
}

fn hap_method<'js>(
    kind: HapMethod,
    ctx: Ctx<'js>,
    this: This<rquickjs::Value<'js>>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let this = object_for_property(&ctx, this.0)?;
    let whole: rquickjs::Value = this.get("whole")?;
    let part: rquickjs::Value = this.get("part")?;
    let value: rquickjs::Value = this.get("value")?;
    let context: rquickjs::Value = this.get("context")?;
    match kind {
        HapMethod::Duration => {
            let raw_duration = optional_property(&ctx, value.clone(), "duration")?;
            let mut duration = if raw_duration.is_number() {
                raw_fraction(&ctx, raw_duration)?
            } else {
                let whole = object_for_property(&ctx, whole)?;
                let begin: rquickjs::Value = whole.get("begin")?;
                let end: rquickjs::Value = whole.get("end")?;
                call_method(&ctx, end, "sub", [begin])?
            };
            let clip = optional_property(&ctx, value, "clip")?;
            if clip.is_number() {
                duration = call_method(&ctx, duration, "mul", [clip])?;
            }
            Ok(duration)
        }
        HapMethod::EndClipped => {
            let whole = object_for_property(&ctx, whole)?;
            let begin: rquickjs::Value = whole.get("begin")?;
            let duration: rquickjs::Value = this.get("duration")?;
            call_method(&ctx, begin, "add", [duration])
        }
        HapMethod::IsActive
        | HapMethod::IsInPast
        | HapMethod::IsInNearPast
        | HapMethod::IsInFuture
        | HapMethod::IsInNearFuture
        | HapMethod::IsWithinTime => {
            let whole = object_for_property(&ctx, whole)?;
            let begin: rquickjs::Value = whole.get("begin")?;
            let end: rquickjs::Value = this.get("endClipped")?;
            let result = match kind {
                HapMethod::IsActive => {
                    let time = argument(&ctx, &args.0, 0);
                    relational_number(&ctx, begin, time.clone(), |a, b| a <= b)?
                        && relational_number(&ctx, end, time, |a, b| a >= b)?
                }
                HapMethod::IsInPast => {
                    relational_number(&ctx, argument(&ctx, &args.0, 0), end, |a, b| a > b)?
                }
                HapMethod::IsInNearPast => {
                    let margin = Coerced::<f64>::from_js(&ctx, argument(&ctx, &args.0, 0))?.0;
                    let time = Coerced::<f64>::from_js(&ctx, argument(&ctx, &args.0, 1))?.0;
                    let end = Coerced::<f64>::from_js(&ctx, end)?.0;
                    time - margin <= end
                }
                HapMethod::IsInFuture => {
                    relational_number(&ctx, argument(&ctx, &args.0, 0), begin, |a, b| a < b)?
                }
                HapMethod::IsInNearFuture => {
                    let margin = Coerced::<f64>::from_js(&ctx, argument(&ctx, &args.0, 0))?.0;
                    let time = Coerced::<f64>::from_js(&ctx, argument(&ctx, &args.0, 1))?.0;
                    let begin = Coerced::<f64>::from_js(&ctx, begin)?.0;
                    time < begin && time > begin - margin
                }
                HapMethod::IsWithinTime => {
                    let min = argument(&ctx, &args.0, 0);
                    let max = argument(&ctx, &args.0, 1);
                    relational_number(&ctx, begin, max, |a, b| a <= b)?
                        && relational_number(&ctx, end, min, |a, b| a >= b)?
                }
                _ => unreachable!(),
            };
            Ok(rquickjs::Value::new_bool(ctx, result))
        }
        HapMethod::WholeOrPart => Ok(if truthy(&ctx, whole.clone())? {
            whole
        } else {
            part
        }),
        HapMethod::WithSpan => {
            let callback = argument(&ctx, &args.0, 0)
                .into_function()
                .ok_or_else(|| throw_type_error(&ctx, "f is not a function"))?;
            let whole = if truthy(&ctx, whole.clone())? {
                callback.call((whole,))?
            } else {
                rquickjs::Value::new_undefined(ctx.clone())
            };
            let part = callback.call((part,))?;
            construct_hap(
                &ctx,
                whole,
                part,
                value,
                context,
                rquickjs::Value::new_bool(ctx.clone(), false),
            )
        }
        HapMethod::WithValue => {
            let callback = argument(&ctx, &args.0, 0)
                .into_function()
                .ok_or_else(|| throw_type_error(&ctx, "f is not a function"))?;
            let value = callback.call((value,))?;
            construct_hap(
                &ctx,
                whole,
                part,
                value,
                context,
                rquickjs::Value::new_bool(ctx.clone(), false),
            )
        }
        HapMethod::HasOnset => {
            if whole.is_undefined() {
                return Ok(rquickjs::Value::new_bool(ctx, false));
            }
            let whole = object_for_property(&ctx, whole)?;
            let part = object_for_property(&ctx, part)?;
            let begin: rquickjs::Value = whole.get("begin")?;
            let part_begin: rquickjs::Value = part.get("begin")?;
            call_method(&ctx, begin, "equals", [part_begin])
        }
        HapMethod::HasTag => {
            let tags = optional_property(&ctx, context, "tags")?;
            if tags.is_undefined() || tags.is_null() {
                return Ok(rquickjs::Value::new_undefined(ctx));
            }
            call_method(&ctx, tags, "includes", [argument(&ctx, &args.0, 0)])
        }
        HapMethod::ResolveState => {
            let state_value = argument(&ctx, &args.0, 0);
            let stateful: rquickjs::Value = this.get("stateful")?;
            if truthy(&ctx, stateful)?
                && call_method(&ctx, this.clone().into_value(), "hasOnset", [])?
                    .as_bool()
                    .unwrap_or(false)
            {
                let callback = value
                    .into_function()
                    .ok_or_else(|| throw_type_error(&ctx, "this.value is not a function"))?;
                let pair = call_with_this(&callback, this.clone().into_value(), [state_value])?;
                let (next, value) = destructure_pair(&ctx, pair)?;
                let hap = construct_hap(
                    &ctx,
                    whole,
                    part,
                    value,
                    context,
                    rquickjs::Value::new_bool(ctx.clone(), false),
                )?;
                return Ok(array(&ctx, [next, hap])?.into_value());
            }
            Ok(array(&ctx, [state_value, this.into_value()])?.into_value())
        }
        HapMethod::SpanEquals => {
            let other = object_for_property(&ctx, argument(&ctx, &args.0, 0))?;
            let other_whole: rquickjs::Value = other.get("whole")?;
            let both_missing = (whole.is_null() || whole.is_undefined())
                && (other_whole.is_null() || other_whole.is_undefined());
            if both_missing {
                return Ok(rquickjs::Value::new_bool(ctx, true));
            }
            call_method(&ctx, whole, "equals", [other_whole])
        }
        HapMethod::Equals => {
            let other = object_for_property(&ctx, argument(&ctx, &args.0, 0))?;
            let spans = call_method(
                &ctx,
                this.clone().into_value(),
                "spanEquals",
                [other.clone().into_value()],
            )?
            .as_bool()
            .unwrap_or(false);
            if !spans {
                return Ok(rquickjs::Value::new_bool(ctx, false));
            }
            let other_part: rquickjs::Value = other.get("part")?;
            let parts = call_method(&ctx, part, "equals", [other_part])?
                .as_bool()
                .unwrap_or(false);
            let other_value: rquickjs::Value = other.get("value")?;
            Ok(rquickjs::Value::new_bool(
                ctx.clone(),
                parts && strict_equal(&ctx, &value, &other_value),
            ))
        }
        HapMethod::Show | HapMethod::ShowWhole => {
            let compact = truthy(&ctx, argument(&ctx, &args.0, 0))?;
            let rendered = json_value(&ctx, value, compact)?;
            let rendered = Coerced::<String>::from_js(&ctx, rendered)?.0;
            if matches!(kind, HapMethod::ShowWhole) {
                let span = if whole.is_undefined() {
                    "~".to_owned()
                } else {
                    let shown = call_method(&ctx, whole, "show", [])?;
                    Coerced::<String>::from_js(&ctx, shown)?.0
                };
                return Ok(
                    rquickjs::String::from_str(ctx, &format!("{span}: {rendered}"))?.into_value(),
                );
            }
            let spans = if whole.is_undefined() {
                let part = object_for_property(&ctx, part)?;
                let shown: rquickjs::Value = part.get("show")?;
                format!("~{}", Coerced::<String>::from_js(&ctx, shown)?.0)
            } else {
                let whole = object_for_property(&ctx, whole)?;
                let part = object_for_property(&ctx, part)?;
                let whole_begin: rquickjs::Value = whole.get("begin")?;
                let whole_end: rquickjs::Value = whole.get("end")?;
                let part_begin: rquickjs::Value = part.get("begin")?;
                let part_end: rquickjs::Value = part.get("end")?;
                let starts =
                    call_method(&ctx, whole_begin.clone(), "equals", [part_begin.clone()])?
                        .as_bool()
                        .unwrap_or(false);
                let ends = call_method(&ctx, whole_end.clone(), "equals", [part_end.clone()])?
                    .as_bool()
                    .unwrap_or(false);
                let mut spans = String::new();
                if !starts {
                    let shown = call_method(&ctx, whole_begin, "show", [])?;
                    spans.push_str(&Coerced::<String>::from_js(&ctx, shown)?.0);
                    spans.push_str(" ⇜ ");
                }
                if !(starts && ends) {
                    spans.push('(');
                }
                let shown = call_method(&ctx, part.clone().into_value(), "show", [])?;
                spans.push_str(&Coerced::<String>::from_js(&ctx, shown)?.0);
                if !(starts && ends) {
                    spans.push(')');
                }
                if !ends {
                    let shown = call_method(&ctx, whole_end, "show", [])?;
                    spans.push_str(" ⇝ ");
                    spans.push_str(&Coerced::<String>::from_js(&ctx, shown)?.0);
                }
                spans
            };
            Ok(rquickjs::String::from_str(ctx, &format!("[ {spans} | {rendered} ]"))?.into_value())
        }
        HapMethod::CombineContext => {
            let other = object_for_property(&ctx, argument(&ctx, &args.0, 0))?;
            let other_context: rquickjs::Value = other.get("context")?;
            let result = rquickjs::Object::new(ctx.clone())?;
            copy_enumerable(&ctx, &result, context.clone())?;
            copy_enumerable(&ctx, &result, other_context.clone())?;
            let left = optional_property(&ctx, context, "locations")?;
            let right = optional_property(&ctx, other_context, "locations")?;
            let left = if truthy(&ctx, left.clone())? {
                left
            } else {
                rquickjs::Array::new(ctx.clone())?.into_value()
            };
            let right = if truthy(&ctx, right.clone())? {
                right
            } else {
                rquickjs::Array::new(ctx.clone())?.into_value()
            };
            let locations = call_method(&ctx, left, "concat", [right])?;
            result.set("locations", locations)?;
            Ok(result.into_value())
        }
        HapMethod::SetContext => construct_hap(
            &ctx,
            whole,
            part,
            value,
            argument(&ctx, &args.0, 0),
            rquickjs::Value::new_bool(ctx.clone(), false),
        ),
        HapMethod::EnsureObjectValue => {
            if value.is_null() || (value.is_object() && !value.is_function()) {
                return Ok(rquickjs::Value::new_undefined(ctx));
            }
            let value = Coerced::<String>::from_js(&ctx, value)?.0;
            Err(rquickjs::Exception::throw_message(
                &ctx,
                &format!(
                    "expected hap.value to be an object, but got \"{value}\". Hint: append .note() or .s() to the end"
                ),
            ))
        }
    }
}

fn copy_enumerable<'js>(
    ctx: &Ctx<'js>,
    target: &rquickjs::Object<'js>,
    source: rquickjs::Value<'js>,
) -> rquickjs::Result<()> {
    if source.is_null() || source.is_undefined() {
        return Ok(());
    }
    let source = object_for_property(ctx, source)?;
    let filter = rquickjs::object::Filter::new()
        .string()
        .symbol()
        .enum_only();
    for key in source.own_keys::<rquickjs::Atom>(filter) {
        let key = key?;
        let value: rquickjs::Value = source.get(key.clone())?;
        target.set(key, value)?;
    }
    Ok(())
}

fn state_method<'js>(
    kind: StateMethod,
    ctx: Ctx<'js>,
    this: This<rquickjs::Value<'js>>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let this = object_for_property(&ctx, this.0)?;
    let span: rquickjs::Value = this.get("span")?;
    let controls: rquickjs::Value = this.get("controls")?;
    match kind {
        StateMethod::SetSpan => construct_state(&ctx, argument(&ctx, &args.0, 0), controls),
        StateMethod::WithSpan => {
            let callback = argument(&ctx, &args.0, 0)
                .into_function()
                .ok_or_else(|| throw_type_error(&ctx, "func is not a function"))?;
            let span = callback.call((span,))?;
            construct_state(&ctx, span, controls)
        }
        StateMethod::SetControls => {
            let merged = rquickjs::Object::new(ctx.clone())?;
            copy_enumerable(&ctx, &merged, controls)?;
            copy_enumerable(&ctx, &merged, argument(&ctx, &args.0, 0))?;
            construct_state(&ctx, span, merged.into_value())
        }
    }
}

fn define_method<'js>(
    prototype: &rquickjs::Object<'js>,
    name: &str,
    length: usize,
    function: Function<'js>,
) -> rquickjs::Result<()> {
    configure_function(&function, name, length, false)?;
    prototype.prop(name, Property::from(function).writable().configurable())
}

fn define_getter<'js>(
    ctx: &Ctx<'js>,
    prototype: &rquickjs::Object<'js>,
    name: &str,
    function: Function<'js>,
) -> rquickjs::Result<()> {
    configure_function(&function, &format!("get {name}"), 0, false)?;
    let descriptor = rquickjs::Object::new(ctx.clone())?;
    descriptor.set("get", function)?;
    descriptor.set("configurable", true)?;
    let object: Function = ctx.globals().get("Object")?;
    let define: Function = object.get("defineProperty")?;
    define.call::<_, rquickjs::Value>((This(object), prototype.clone(), name, descriptor))?;
    Ok(())
}

fn install_fraction_methods<'js>(ctx: &Ctx<'js>, raw: &Function<'js>) -> rquickjs::Result<()> {
    let prototype: rquickjs::Object = raw.get("prototype")?;
    for (name, length, kind) in [
        ("sam", 0, FractionMethod::Sam),
        ("nextSam", 0, FractionMethod::NextSam),
        ("wholeCycle", 0, FractionMethod::WholeCycle),
        ("cyclePos", 0, FractionMethod::CyclePos),
        ("lt", 1, FractionMethod::Lt),
        ("gt", 1, FractionMethod::Gt),
        ("lte", 1, FractionMethod::Lte),
        ("gte", 1, FractionMethod::Gte),
        ("eq", 1, FractionMethod::Eq),
        ("ne", 1, FractionMethod::Ne),
        ("max", 1, FractionMethod::Max),
        ("maximum", 0, FractionMethod::Maximum),
        ("min", 1, FractionMethod::Min),
        ("mulmaybe", 1, FractionMethod::MulMaybe),
        ("divmaybe", 1, FractionMethod::DivMaybe),
        ("addmaybe", 1, FractionMethod::AddMaybe),
        ("submaybe", 1, FractionMethod::SubMaybe),
        ("show", 0, FractionMethod::Show),
        ("or", 1, FractionMethod::Or),
    ] {
        let function = Function::new(ctx.clone(), move |ctx, this, args| {
            fraction_method(kind, ctx, this, args)
        })?;
        configure_function(&function, "", length, true)?;
        prototype.set(name, function)?;
    }
    Ok(())
}

fn install_class<'js>(
    _ctx: &Ctx<'js>,
    class: &str,
    length: usize,
    constructor: Function<'js>,
    prototype: rquickjs::Object<'js>,
) -> rquickjs::Result<Function<'js>> {
    constructor.set_name(class)?;
    constructor.set_length(length)?;
    constructor.set_constructor(true);
    constructor
        .as_inner()
        .prop("prototype", Property::from(prototype.clone()))?;
    prototype.prop(
        "constructor",
        Property::from(constructor.clone())
            .writable()
            .configurable(),
    )?;
    Ok(constructor)
}

fn install_classes<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<()> {
    let span_proto = rquickjs::Object::new(ctx.clone())?;
    for (name, length, kind) in [
        ("cycleArc", 0, SpanMethod::CycleArc),
        ("withTime", 1, SpanMethod::WithTime),
        ("withEnd", 1, SpanMethod::WithEnd),
        ("withCycle", 1, SpanMethod::WithCycle),
        ("intersection", 1, SpanMethod::Intersection),
        ("intersection_e", 1, SpanMethod::IntersectionE),
        ("midpoint", 0, SpanMethod::Midpoint),
        ("equals", 1, SpanMethod::Equals),
        ("show", 0, SpanMethod::Show),
    ] {
        let function = Function::new(ctx.clone(), move |ctx, this, args| {
            span_method(kind, ctx, this, args)
        })?;
        define_method(&span_proto, name, length, function)?;
    }
    for (name, kind) in [
        ("spanCycles", SpanMethod::SpanCycles),
        ("duration", SpanMethod::Duration),
    ] {
        let getter = Function::new(ctx.clone(), move |ctx, this, args| {
            span_method(kind, ctx, this, args)
        })?;
        define_getter(ctx, &span_proto, name, getter)?;
    }
    let span = install_class(
        ctx,
        TIME_SPAN,
        2,
        Function::new(ctx.clone(), time_span_constructor)?,
        span_proto,
    )?;
    state(ctx)?.set(TIME_SPAN, span.clone())?;
    ctx.globals().set(TIME_SPAN, span)?;

    let hap_proto = rquickjs::Object::new(ctx.clone())?;
    for (name, length, kind) in [
        ("isActive", 1, HapMethod::IsActive),
        ("isInPast", 1, HapMethod::IsInPast),
        ("isInNearPast", 2, HapMethod::IsInNearPast),
        ("isInFuture", 1, HapMethod::IsInFuture),
        ("isInNearFuture", 2, HapMethod::IsInNearFuture),
        ("isWithinTime", 2, HapMethod::IsWithinTime),
        ("wholeOrPart", 0, HapMethod::WholeOrPart),
        ("withSpan", 1, HapMethod::WithSpan),
        ("withValue", 1, HapMethod::WithValue),
        ("hasOnset", 0, HapMethod::HasOnset),
        ("hasTag", 1, HapMethod::HasTag),
        ("resolveState", 1, HapMethod::ResolveState),
        ("spanEquals", 1, HapMethod::SpanEquals),
        ("equals", 1, HapMethod::Equals),
        ("show", 0, HapMethod::Show),
        ("showWhole", 0, HapMethod::ShowWhole),
        ("combineContext", 1, HapMethod::CombineContext),
        ("setContext", 1, HapMethod::SetContext),
        ("ensureObjectValue", 0, HapMethod::EnsureObjectValue),
    ] {
        let function = Function::new(ctx.clone(), move |ctx, this, args| {
            hap_method(kind, ctx, this, args)
        })?;
        define_method(&hap_proto, name, length, function)?;
    }
    for (name, kind) in [
        ("duration", HapMethod::Duration),
        ("endClipped", HapMethod::EndClipped),
    ] {
        let getter = Function::new(ctx.clone(), move |ctx, this, args| {
            hap_method(kind, ctx, this, args)
        })?;
        define_getter(ctx, &hap_proto, name, getter)?;
    }
    let hap = install_class(
        ctx,
        HAP,
        3,
        Function::new(ctx.clone(), hap_constructor)?,
        hap_proto,
    )?;
    state(ctx)?.set(HAP, hap.clone())?;
    ctx.globals().set(HAP, hap)?;

    let state_proto = rquickjs::Object::new(ctx.clone())?;
    for (name, length, kind) in [
        ("setSpan", 1, StateMethod::SetSpan),
        ("withSpan", 1, StateMethod::WithSpan),
        ("setControls", 1, StateMethod::SetControls),
    ] {
        let function = Function::new(ctx.clone(), move |ctx, this, args| {
            state_method(kind, ctx, this, args)
        })?;
        define_method(&state_proto, name, length, function)?;
    }
    let state_class = install_class(
        ctx,
        STATE,
        1,
        Function::new(ctx.clone(), state_constructor)?,
        state_proto,
    )?;
    state(ctx)?.set(STATE, state_class.clone())?;
    ctx.globals().set(STATE, state_class)?;
    Ok(())
}

fn raw_span<'js>(
    ctx: &Ctx<'js>,
    raw: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    if raw.is_null() || raw.is_undefined() {
        return Ok(rquickjs::Value::new_undefined(ctx.clone()));
    }
    let raw = object_for_property(ctx, raw)?;
    let seed: Function = state(ctx)?.get(FRACTION_SEED)?;
    let begin = seed.call((raw.get::<_, rquickjs::Object>("begin")?,))?;
    let end = seed.call((raw.get::<_, rquickjs::Object>("end")?,))?;
    construct_span(ctx, begin, end)
}

fn hap_factory<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let whole = raw_span(&ctx, argument(&ctx, &args.0, 0))?;
    let part = raw_span(&ctx, argument(&ctx, &args.0, 1))?;
    let context = argument(&ctx, &args.0, 3);
    let context = if context.is_undefined() {
        rquickjs::Object::new(ctx.clone())?.into_value()
    } else {
        context
    };
    construct_hap(
        &ctx,
        whole,
        part,
        argument(&ctx, &args.0, 2),
        context,
        rquickjs::Value::new_bool(ctx.clone(), false),
    )
}

fn state_factory<'js>(
    ctx: Ctx<'js>,
    raw_span_value: rquickjs::Value<'js>,
    controls: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let controls = if controls.is_undefined() {
        rquickjs::Object::new(ctx.clone())?.into_value()
    } else {
        controls
    };
    construct_state(&ctx, raw_span(&ctx, raw_span_value)?, controls)
}

fn fraction_surface<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    raw_fraction(&ctx, argument(&ctx, &args.0, 0))
}

fn pair_at<'js>(
    ctx: Ctx<'js>,
    pair: rquickjs::Value<'js>,
    index: usize,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    object_for_property(&ctx, pair)?.get(index as u32)
}

pub(super) fn spread<'js>(
    ctx: &Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Array<'js>> {
    let object = object_for_property(ctx, value.clone())?;
    let iterator_symbol = rquickjs::Symbol::iterator(ctx.clone());
    let iterator: Function = object.get(iterator_symbol)?;
    let iterator = call_with_this(&iterator, value, [])?;
    let iterator = object_for_property(ctx, iterator)?;
    let next: Function = iterator.get("next")?;
    let output = rquickjs::Array::new(ctx.clone())?;
    let mut index = 0usize;
    loop {
        let item = call_with_this(&next, iterator.clone().into_value(), [])?;
        let item = object_for_property(ctx, item)?;
        let done: rquickjs::Value = item.get("done")?;
        if truthy(ctx, done)? {
            break;
        }
        output.set(index, item.get::<_, rquickjs::Value>("value")?)?;
        index = index
            .checked_add(1)
            .ok_or_else(|| rquickjs::Exception::throw_range(ctx, "array length overflow"))?;
    }
    Ok(output)
}

fn array_items<'js>(
    ctx: Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    if dynamic_is_array(&ctx, value.clone())? {
        Ok(spread(&ctx, value)?.into_value())
    } else {
        Ok(rquickjs::Value::new_undefined(ctx))
    }
}

fn mini_string<'js>(
    ctx: Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::String<'js>> {
    Ok(Coerced::<rquickjs::String>::from_js(&ctx, value)?.0)
}

fn map_pair<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    Ok(array(
        &ctx,
        [argument(&ctx, &args.0, 1), argument(&ctx, &args.0, 0)],
    )?
    .into_value())
}

fn mapped_entries<'js>(
    ctx: &Ctx<'js>,
    source: rquickjs::Value<'js>,
) -> rquickjs::Result<(rquickjs::Value<'js>, rquickjs::Array<'js>)> {
    let callback = Function::new(ctx.clone(), map_pair)?;
    configure_function(&callback, "", 2, false)?;
    let mapped = call_method(ctx, source, "map", [callback.into_value()])?;
    let mapped_object = object_for_property(ctx, mapped.clone())?;
    let entries = rquickjs::Array::new(ctx.clone())?;
    // `map` can return any object. Read its length once, truncating fractions
    // and treating NaN and negatives as empty, as `filter` does. Property
    // reads may invoke JavaScript, but the host loop has no deadline check,
    // so charge its bound before entering it.
    let length: rquickjs::Value = mapped_object.get("length")?;
    let length = Coerced::<f64>::from_js(ctx, length)?.0 as usize;
    charge_js_array::<rquickjs::Value>(ctx, length)?;
    let mut output = 0usize;
    for input in 0..length {
        let input_key = u32::try_from(input)
            .map_err(|_| rquickjs::Exception::throw_range(ctx, "array index overflow"))?;
        if mapped_object.contains_key(input_key)? {
            entries.set(output, mapped_object.get::<_, rquickjs::Value>(input_key)?)?;
            output += 1;
        }
    }
    Ok((mapped, entries))
}

fn object_static<'js>(
    ctx: &Ctx<'js>,
    name: &str,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let object: Function = ctx.globals().get("Object")?;
    let function: Function = object.get(name)?;
    function.call((This(object), value))
}

fn pick_lookup_shape<'js>(
    ctx: Ctx<'js>,
    lookup: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let is_array = dynamic_is_array(&ctx, lookup.clone())?;
    let keys = object_static(&ctx, "keys", lookup.clone())?;
    let enumerable_len = object_for_property(&ctx, keys)?.get::<_, usize>("length")?;
    if is_array {
        let length = object_for_property(&ctx, lookup.clone())?.get::<_, usize>("length")?;
        let (_, entries) = mapped_entries(&ctx, lookup)?;
        return Ok(array(
            &ctx,
            [
                rquickjs::Value::new_bool(ctx.clone(), true),
                rquickjs::Value::new_int(ctx.clone(), enumerable_len as i32),
                rquickjs::Value::new_int(ctx.clone(), length as i32),
                entries.into_value(),
            ],
        )?
        .into_value());
    }
    let entries = object_static(&ctx, "entries", lookup)?;
    Ok(array(
        &ctx,
        [
            rquickjs::Value::new_bool(ctx.clone(), false),
            rquickjs::Value::new_int(ctx.clone(), enumerable_len as i32),
            rquickjs::Value::new_int(ctx.clone(), 0),
            entries,
        ],
    )?
    .into_value())
}

fn squeeze_lookup_shape<'js>(
    ctx: Ctx<'js>,
    xs: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    if xs.is_null() {
        return Err(throw_type_error(
            &ctx,
            "Cannot read properties of null (reading 'map')",
        ));
    }
    if xs.is_undefined() {
        return Err(throw_type_error(
            &ctx,
            "Cannot read properties of undefined (reading 'map')",
        ));
    }
    let object = object_for_property(&ctx, xs.clone())?;
    let map: rquickjs::Value = object.get("map")?;
    if !map.is_function() {
        return Err(throw_type_error(&ctx, "xs.map is not a function"));
    }
    let length: usize = object.get("length")?;
    let (_, entries) = mapped_entries(&ctx, xs)?;
    Ok(array(
        &ctx,
        [
            rquickjs::Value::new_int(ctx.clone(), length as i32),
            entries.into_value(),
        ],
    )?
    .into_value())
}

fn gap<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let steps = argument(&ctx, &args.0, 0);
    let normalized = if steps.is_undefined() {
        steps
    } else {
        exact_fraction(&ctx, steps)?
    };
    let numeric = if normalized.is_undefined() {
        normalized.clone()
    } else {
        let raw_value_of: Function = state(&ctx)?.get(RAW_VALUE_OF)?;
        call_with_this(&raw_value_of, normalized.clone(), [])?
    };
    let result = native_gap(ctx.clone(), numeric)?.into_value();
    object_for_property(&ctx, result.clone())?.set("__steps", normalized)?;
    Ok(result)
}

fn pure<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Class<'js, NativePatternWrapper<'js>>> {
    native_pure(ctx.clone(), argument(&ctx, &args.0, 0))
}

pub(super) fn install<'js>(ctx: &Ctx<'js>) -> Result<rquickjs::Object<'js>, String> {
    let globals = ctx.globals();
    let foundation = rquickjs::Object::new(ctx.clone()).map_err(|error| error.to_string())?;
    host_stack(ctx)
        .map_err(|error| error.to_string())?
        .as_object()
        .set(FOUNDATION_STATE, foundation.clone())
        .map_err(|error| error.to_string())?;

    let raw_fraction = fraction::install(ctx)
        .map_err(|error| format!("install native Fraction: {}", describe_js_error(ctx, error)))?;
    let fraction_seed =
        fraction::seed_factory(ctx).map_err(|error| describe_js_error(ctx, error))?;
    foundation
        .set(RAW_FRACTION, raw_fraction.clone())
        .map_err(|error| error.to_string())?;
    foundation
        .set(FRACTION_SEED, fraction_seed.clone())
        .map_err(|error| error.to_string())?;
    install_fraction_methods(ctx, &raw_fraction).map_err(|error| describe_js_error(ctx, error))?;

    let fraction =
        Function::new(ctx.clone(), fraction_surface).map_err(|error| error.to_string())?;
    configure_function(&fraction, "fraction", 1, false).map_err(|error| error.to_string())?;
    fraction
        .set("_original", raw_fraction.clone())
        .map_err(|error| error.to_string())?;
    foundation
        .set(FRACTION, fraction.clone())
        .map_err(|error| error.to_string())?;
    let raw_prototype: rquickjs::Object = raw_fraction
        .get("prototype")
        .map_err(|error| error.to_string())?;
    foundation
        .set(
            RAW_VALUE_OF,
            raw_prototype
                .get::<_, Function>("valueOf")
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;

    install_classes(ctx).map_err(|error| describe_js_error(ctx, error))?;

    let stack = host_stack(ctx).map_err(|error| error.to_string())?;
    stack
        .as_object()
        .set(FRACTION_FACTORY, fraction_seed)
        .map_err(|error| error.to_string())?;
    stack
        .as_object()
        .set(FRACTION_COERCE, fraction.clone())
        .map_err(|error| error.to_string())?;
    stack
        .as_object()
        .set(
            HAP_FACTORY,
            Function::new(ctx.clone(), hap_factory).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    stack
        .as_object()
        .set(
            STATE_FACTORY,
            Function::new(ctx.clone(), state_factory).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    stack
        .as_object()
        .set(
            WCHOOSE_PAIR_AT,
            Function::new(ctx.clone(), pair_at).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    stack
        .as_object()
        .set(
            ARRAY_ITEMS,
            Function::new(ctx.clone(), array_items).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    stack
        .as_object()
        .set(
            MINI_STRING_COERCE,
            Function::new(ctx.clone(), mini_string).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    stack
        .as_object()
        .set(
            PICK_LOOKUP_SHAPE,
            Function::new(ctx.clone(), pick_lookup_shape).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    stack
        .as_object()
        .set(
            SQUEEZE_LOOKUP_SHAPE,
            Function::new(ctx.clone(), squeeze_lookup_shape).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;

    globals
        .set("Fraction", fraction.clone())
        .map_err(|error| error.to_string())?;
    let pure = Function::new(ctx.clone(), pure).map_err(|error| error.to_string())?;
    configure_function(&pure, "pure", 1, true).map_err(|error| error.to_string())?;
    globals
        .set("pure", pure)
        .map_err(|error| error.to_string())?;
    let gap = Function::new(ctx.clone(), gap).map_err(|error| error.to_string())?;
    configure_function(&gap, "gap", 1, false).map_err(|error| error.to_string())?;
    globals
        .set("gap", gap.clone())
        .map_err(|error| error.to_string())?;
    let silence: rquickjs::Value = gap
        .call((1,))
        .map_err(|error| describe_js_error(ctx, error))?;
    let nothing: rquickjs::Value = gap
        .call((0,))
        .map_err(|error| describe_js_error(ctx, error))?;
    globals
        .set("nothing", nothing)
        .map_err(|error| error.to_string())?;
    globals
        .set("silence", silence)
        .map_err(|error| error.to_string())?;
    globals
        .set(
            "__silence",
            Function::new(ctx.clone(), native_silence).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    globals
        .set(
            "m",
            Function::new(ctx.clone(), native_mini).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    let mini =
        Function::new(ctx.clone(), native_mini_sequence).map_err(|error| error.to_string())?;
    mini.set_name("mini").map_err(|error| error.to_string())?;
    globals
        .set("mini", mini)
        .map_err(|error| error.to_string())?;

    Ok(globals)
}
