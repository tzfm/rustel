use super::*;

pub(crate) fn ecma_to_i32(value: f64) -> i32 {
    if !value.is_finite() || value == 0.0 {
        return 0;
    }
    let integer = value.trunc();
    let modulo = integer.rem_euclid(4_294_967_296.0);
    if modulo >= 2_147_483_648.0 {
        (modulo - 4_294_967_296.0) as i32
    } else {
        modulo as i32
    }
}

pub(crate) fn constructor<'js>(
    ctx: Ctx<'js>,
    this: This<Value<'js>>,
    args: Rest<Value<'js>>,
) -> rquickjs::Result<Value<'js>> {
    let first = args
        .0
        .first()
        .cloned()
        .unwrap_or_else(|| Value::new_undefined(ctx.clone()));
    let parsed = parse(&ctx, first, args.0.get(1).cloned())?;
    let parts = normalise(&ctx, parsed.s * parsed.n, parsed.d)?;
    // QuickJS passes the new-target function as `this` to a native
    // constructor. Build the object JavaScript would have allocated, then run
    // the original `this instanceof Fraction` decision. This preserves
    // subclasses, unrelated Reflect.construct targets, primitive new-target
    // prototypes, and a user-defined Symbol.hasInstance hook.
    if let Some(new_target) = this.0.as_function() {
        let candidate = Object::new(ctx.clone())?;
        let target_prototype: Value = new_target.get("prototype")?;
        if let Some(prototype) = target_prototype.as_object() {
            candidate.set_prototype(Some(prototype))?;
        }
        let raw: Function = crate::host_stack(&ctx)?
            .as_object()
            .get(RAW_FRACTION_SLOT)?;
        if is_instance_of(&ctx, &candidate, &raw)? {
            set_parts(&ctx, &candidate, &parts)?;
            return Ok(candidate.into_value());
        }
    }
    let prototype = current_prototype(&ctx)?;
    object_with_parts(&ctx, &parts, prototype)
}

pub(crate) fn native_seed<'js>(ctx: Ctx<'js>, seed: Object<'js>) -> rquickjs::Result<Value<'js>> {
    let numerator = seed
        .get::<_, String>("n")?
        .parse::<BigInt>()
        .map_err(|_| invalid_parameter(&ctx))?;
    let denominator = seed
        .get::<_, String>("d")?
        .parse::<BigInt>()
        .map_err(|_| invalid_parameter(&ctx))?;
    new_fraction(&ctx, numerator, denominator)
}

/// Convert the trusted exact string pair produced by the Rust bridge without
/// routing it back through JavaScript BigInt coercion and the public parser.
pub(crate) fn seed_factory<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Function<'js>> {
    Function::new(ctx.clone(), native_seed)
}

/// Install the callable/constructible raw Fraction object and its ordinary
/// mutable prototype. The caller wraps it in the public one-argument
/// `Fraction` function.
pub(crate) fn install<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Function<'js>> {
    let stack = crate::host_stack(ctx)?;
    let bigint: Function = ctx.globals().get("BigInt")?;
    stack.as_object().set(INTRINSIC_BIGINT_SLOT, bigint)?;

    let prototype = Object::new(ctx.clone())?;
    prototype.set("s", JsBigInt::from_i64(ctx.clone(), 1)?)?;
    prototype.set("n", JsBigInt::from_i64(ctx.clone(), 0)?)?;
    prototype.set("d", JsBigInt::from_i64(ctx.clone(), 1)?)?;
    for &(name, length, method) in Method::ALL {
        let function = Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, this: This<Object<'js>>, args: Rest<Value<'js>>| {
                invoke(method, ctx, this, args)
            },
        )?
        .with_name(name)?
        .with_length(length)?;
        prototype.set(name, function)?;
    }

    let raw = Function::new(ctx.clone(), constructor)?
        .with_name("Fraction")?
        .with_length(2)?
        .with_constructor(true);
    raw.prop("prototype", Property::from(prototype).writable())?;
    stack.as_object().set(RAW_FRACTION_SLOT, raw.clone())?;
    Ok(raw)
}
