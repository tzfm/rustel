// Fraction.js v5.2.1
// Source: https://registry.npmjs.org/fraction.js/-/fraction.js-5.2.1.tgz
//
// MIT License
//
// Copyright (c) 2024 Robert Eisele
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

//! Rust `fraction.js@5.2.1` compatibility surface.
//!
//! Instances deliberately remain ordinary JavaScript objects.  The upstream
//! package exposes writable, enumerable `s`, `n`, and `d` BigInt fields, and
//! its methods read those fields on every call.  Keeping that shape preserves
//! user mutation, subclass/prototype inspection, and callback-facing objects;
//! only parsing, arithmetic, comparison, and formatting cross into Rust.
//!
//! The implementation is a Rust port of Robert Eisele's MIT-licensed
//! fraction.js 5.2.1.

use num_bigint::BigInt;
use num_traits::{FromPrimitive, One, Signed, ToPrimitive, Zero};
use rquickjs::{
    BigInt as JsBigInt, Coerced, Ctx, FromJs, Function, IntoJs, Object, Type, Value,
    function::{Rest, This},
    object::Property,
};
use std::collections::BTreeMap;

const RAW_FRACTION_SLOT: &str = "__native_raw_fraction";
const INTRINSIC_BIGINT_SLOT: &str = "__native_fraction_bigint";
const MAX_CYCLE_LEN: usize = 2_000;
const MAX_CYCLE_MODULUS_BITS: u64 = 16_384;
const MAX_DECIMAL_DIGITS: usize = 65_536;
const MAX_CONTINUED_LEN: usize = 16_384;
const MAX_FAREY_BATCHES: usize = 100_000;

// A score must not turn arbitrary-precision compatibility into unbounded host
// work.  One million bits is far beyond musical time values (about 300,000
// decimal digits) while keeping a single result below 128 KiB.
const MAX_RESULT_BITS: u64 = 1_048_576;
const MAX_GCD_STEPS: usize = 100_000;
const MAX_GCD_WORK_BITS: u64 = 32_000_000;
const MAX_FACTOR_STEPS: usize = 100_000;
const MAX_FACTOR_WORK_BITS: u64 = 8_000_000;
const MAX_CYCLE_WORK_BITS: u64 = 4_000_000;
const MAX_DECIMAL_WORK_BITS: u64 = 32_000_000;
const MAX_CONTINUED_WORK_BITS: u64 = 32_000_000;
const MAX_SIMPLIFY_STEPS: usize = 100_000;

#[derive(Clone, Debug)]
pub(crate) struct Parts {
    s: BigInt,
    n: BigInt,
    d: BigInt,
}

impl Parts {
    fn zero() -> Self {
        Self {
            s: BigInt::one(),
            n: BigInt::zero(),
            d: BigInt::one(),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Method {
    Abs,
    Neg,
    Add,
    Sub,
    Mul,
    Div,
    Clone,
    Mod,
    Gcd,
    Lcm,
    Inverse,
    Pow,
    Log,
    Equals,
    Lt,
    Lte,
    Gt,
    Gte,
    Compare,
    Ceil,
    Floor,
    Round,
    RoundTo,
    Divisible,
    ValueOf,
    ToString,
    ToFraction,
    ToLatex,
    ToContinued,
    Simplify,
}

impl Method {
    const ALL: &'static [(&'static str, usize, Method)] = &[
        ("abs", 0, Method::Abs),
        ("neg", 0, Method::Neg),
        ("add", 2, Method::Add),
        ("sub", 2, Method::Sub),
        ("mul", 2, Method::Mul),
        ("div", 2, Method::Div),
        ("clone", 0, Method::Clone),
        ("mod", 2, Method::Mod),
        ("gcd", 2, Method::Gcd),
        ("lcm", 2, Method::Lcm),
        ("inverse", 0, Method::Inverse),
        ("pow", 2, Method::Pow),
        ("log", 2, Method::Log),
        ("equals", 2, Method::Equals),
        ("lt", 2, Method::Lt),
        ("lte", 2, Method::Lte),
        ("gt", 2, Method::Gt),
        ("gte", 2, Method::Gte),
        ("compare", 2, Method::Compare),
        ("ceil", 1, Method::Ceil),
        ("floor", 1, Method::Floor),
        ("round", 1, Method::Round),
        ("roundTo", 2, Method::RoundTo),
        ("divisible", 2, Method::Divisible),
        ("valueOf", 0, Method::ValueOf),
        ("toString", 1, Method::ToString),
        ("toFraction", 1, Method::ToFraction),
        ("toLatex", 1, Method::ToLatex),
        ("toContinued", 0, Method::ToContinued),
        ("simplify", 1, Method::Simplify),
    ];
}

fn invalid_parameter(ctx: &Ctx<'_>) -> rquickjs::Error {
    rquickjs::Exception::throw_message(ctx, "Invalid argument")
}

fn non_integer_parameter(ctx: &Ctx<'_>) -> rquickjs::Error {
    rquickjs::Exception::throw_message(ctx, "Parameters must be integer")
}

fn division_by_zero(ctx: &Ctx<'_>) -> rquickjs::Error {
    rquickjs::Exception::throw_message(ctx, "Division by Zero")
}

fn invalid_bigint_operation(ctx: &Ctx<'_>) -> rquickjs::Error {
    rquickjs::Exception::throw_range(ctx, "invalid operation")
}

fn native_limit(ctx: &Ctx<'_>, operation: &str) -> rquickjs::Error {
    rquickjs::Exception::throw_range(
        ctx,
        &format!("native Fraction {operation} exceeds the bounded arithmetic limit"),
    )
}

fn check_size<'js>(ctx: &Ctx<'js>, value: BigInt, operation: &str) -> rquickjs::Result<BigInt> {
    if value.magnitude().bits() > MAX_RESULT_BITS {
        Err(native_limit(ctx, operation))
    } else {
        Ok(value)
    }
}

pub(crate) fn bigint_from_primitive<'js>(
    ctx: &Ctx<'js>,
    value: &Value<'js>,
) -> rquickjs::Result<BigInt> {
    let bigint = value
        .as_big_int()
        .ok_or_else(|| rquickjs::Exception::throw_type(ctx, "cannot convert to BigInt"))?;
    // QuickJS's public conversion returns the low 64 bits for oversized
    // BigInts.  Round-trip that candidate with strict equality before using
    // it: musical timing values then avoid decimal allocation/parsing, while
    // arbitrary-precision inputs still take the exact string fallback.
    let low = bigint.clone().to_i64()?;
    let signed = JsBigInt::from_i64(ctx.clone(), low)?;
    let signed_equal = unsafe {
        rquickjs::qjs::JS_IsStrictEqual(
            ctx.as_raw().as_ptr(),
            bigint.as_value().as_raw(),
            signed.as_value().as_raw(),
        )
    };
    if signed_equal {
        return Ok(BigInt::from(low));
    }
    let unsigned = JsBigInt::from_u64(ctx.clone(), low as u64)?;
    let unsigned_equal = unsafe {
        rquickjs::qjs::JS_IsStrictEqual(
            ctx.as_raw().as_ptr(),
            bigint.as_value().as_raw(),
            unsigned.as_value().as_raw(),
        )
    };
    if unsigned_equal {
        return Ok(BigInt::from(low as u64));
    }
    // Reject on decimal length BEFORE parsing into a Rust BigInt. Coercion and
    // `parse` allocate on the host heap, which the QuickJS budget does not
    // see - a planted oversized BigInt would otherwise blow RSS while the
    // realm's own ceiling still looked fine.
    let text = Coerced::<String>::from_js(ctx, bigint.clone().into_value())?.0;
    // log10(2) ≈ 0.30103; allow a sign and a digit of slack.
    let max_decimal_chars = (MAX_RESULT_BITS as usize * 301 / 1000).saturating_add(2);
    if text.len() > max_decimal_chars {
        return Err(native_limit(ctx, "input"));
    }
    text.parse::<BigInt>()
        .map_err(|_| rquickjs::Exception::throw_type(ctx, "invalid BigInt value"))
        .and_then(|value| check_size(ctx, value, "input"))
}

fn intrinsic_bigint_constructor<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Function<'js>> {
    super::host_stack(ctx)?
        .as_object()
        .get(INTRINSIC_BIGINT_SLOT)
}

fn coerce_bigint<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<BigInt> {
    // `BigInt` is a global binding in fraction.js, so replacing it after the
    // module loads is observable while parsing. Keep that behavior for public
    // inputs; only host-created output values use the captured intrinsic.
    let constructor: Function = ctx.globals().get("BigInt")?;
    let converted: Value = constructor.call((value,))?;
    bigint_from_primitive(ctx, &converted)
}

fn js_bigint<'js>(ctx: &Ctx<'js>, value: &BigInt) -> rquickjs::Result<JsBigInt<'js>> {
    if let Some(value) = value.to_i64() {
        return JsBigInt::from_i64(ctx.clone(), value);
    }
    if let Some(value) = value.to_u64() {
        return JsBigInt::from_u64(ctx.clone(), value);
    }
    intrinsic_bigint_constructor(ctx)?.call((value.to_string(),))
}

fn set_parts<'js>(ctx: &Ctx<'js>, object: &Object<'js>, parts: &Parts) -> rquickjs::Result<()> {
    object.set("s", js_bigint(ctx, &parts.s)?)?;
    object.set("n", js_bigint(ctx, &parts.n)?)?;
    object.set("d", js_bigint(ctx, &parts.d)?)?;
    Ok(())
}

fn read_parts<'js>(ctx: &Ctx<'js>, object: &Object<'js>) -> rquickjs::Result<Parts> {
    Ok(Parts {
        s: bigint_from_primitive(ctx, &object.get("s")?)?,
        n: bigint_from_primitive(ctx, &object.get("n")?)?,
        d: bigint_from_primitive(ctx, &object.get("d")?)?,
    })
}

pub(crate) fn show_parts<'js>(ctx: &Ctx<'js>, object: &Object<'js>) -> rquickjs::Result<String> {
    let parts = read_parts(ctx, object)?;
    Ok(format!("{}/{}", parts.s * parts.n, parts.d))
}

fn read_field<'js>(
    ctx: &Ctx<'js>,
    object: &Object<'js>,
    name: &'static str,
) -> rquickjs::Result<BigInt> {
    bigint_from_primitive(ctx, &object.get(name)?)
}

// This is the package's sign-preserving Euclidean helper, not `num_integer`'s
// always-positive gcd.  Negative denominators are observable after users
// mutate the public fields.
fn gcd_charge<'js>(
    ctx: &Ctx<'js>,
    a: &BigInt,
    b: &BigInt,
    steps: &mut usize,
    work_bits: &mut u64,
    operation: &str,
) -> rquickjs::Result<()> {
    *steps = steps.saturating_add(1);
    *work_bits = work_bits.saturating_add(a.magnitude().bits().max(b.magnitude().bits()).max(1));
    if *steps > MAX_GCD_STEPS || *work_bits > MAX_GCD_WORK_BITS {
        Err(native_limit(ctx, operation))
    } else {
        Ok(())
    }
}

fn gcd<'js>(
    ctx: &Ctx<'js>,
    mut a: BigInt,
    mut b: BigInt,
    operation: &str,
) -> rquickjs::Result<BigInt> {
    if a.is_zero() {
        return Ok(b);
    }
    if b.is_zero() {
        return Ok(a);
    }
    let mut steps = 0usize;
    let mut work_bits = 0u64;
    loop {
        gcd_charge(ctx, &a, &b, &mut steps, &mut work_bits, operation)?;
        a %= &b;
        if a.is_zero() {
            return Ok(b);
        }
        gcd_charge(ctx, &a, &b, &mut steps, &mut work_bits, operation)?;
        b %= &a;
        if b.is_zero() {
            return Ok(a);
        }
    }
}

fn normalise<'js>(
    ctx: &Ctx<'js>,
    numerator: BigInt,
    denominator: BigInt,
) -> rquickjs::Result<Parts> {
    if denominator.is_zero() {
        return Err(division_by_zero(ctx));
    }
    let sign = if numerator.is_negative() {
        -BigInt::one()
    } else {
        BigInt::one()
    };
    let numerator = numerator.abs();
    let divisor = gcd(ctx, numerator.clone(), denominator.clone(), "normalisation")?;
    let n = check_size(ctx, numerator / &divisor, "normalisation")?;
    let d = check_size(ctx, denominator / divisor, "normalisation")?;
    Ok(Parts { s: sign, n, d })
}

fn current_prototype<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Option<Object<'js>>> {
    let raw: Function = super::host_stack(ctx)?.as_object().get(RAW_FRACTION_SLOT)?;
    let value: Value = raw.get("prototype")?;
    if value.is_null() {
        Ok(None)
    } else if let Some(object) = value.as_object() {
        Ok(Some(object.clone()))
    } else {
        Err(rquickjs::Exception::throw_type(
            ctx,
            "object prototype may only be an Object or null",
        ))
    }
}

fn object_with_parts<'js>(
    ctx: &Ctx<'js>,
    parts: &Parts,
    prototype: Option<Object<'js>>,
) -> rquickjs::Result<Value<'js>> {
    let object = Object::new(ctx.clone())?;
    object.set_prototype(prototype.as_ref())?;
    set_parts(ctx, &object, parts)?;
    Ok(object.into_value())
}

fn is_instance_of<'js>(
    ctx: &Ctx<'js>,
    object: &Object<'js>,
    constructor: &Function<'js>,
) -> rquickjs::Result<bool> {
    let result = unsafe {
        rquickjs::qjs::JS_IsInstanceOf(
            ctx.as_raw().as_ptr(),
            object.as_value().as_raw(),
            constructor.as_value().as_raw(),
        )
    };
    if result < 0 {
        Err(rquickjs::Error::Exception)
    } else {
        Ok(result != 0)
    }
}

fn new_fraction<'js>(
    ctx: &Ctx<'js>,
    numerator: BigInt,
    denominator: BigInt,
) -> rquickjs::Result<Value<'js>> {
    let parts = normalise(ctx, numerator, denominator)?;
    let prototype = current_prototype(ctx)?;
    object_with_parts(ctx, &parts, prototype)
}

fn is_object_type(value: &Value<'_>) -> bool {
    matches!(
        value.type_of(),
        Type::Object | Type::Array | Type::Promise | Type::Exception | Type::Proxy
    )
}

fn is_truthy<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<bool> {
    Ok(Coerced::<bool>::from_js(ctx, value)?.0)
}

fn global_is_nan<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<bool> {
    let function: Function = ctx.globals().get("isNaN")?;
    let result: Value = function.call((value,))?;
    is_truthy(ctx, result)
}

fn parse_integer_argument<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<BigInt> {
    if value.type_of() == Type::BigInt {
        return bigint_from_primitive(ctx, &value);
    }
    if global_is_nan(ctx, value.clone())? {
        return Err(invalid_parameter(ctx));
    }
    let numeric = Coerced::<f64>::from_js(ctx, value.clone())?.0;
    if numeric % 1.0 != 0.0 {
        return Err(non_integer_parameter(ctx));
    }
    coerce_bigint(ctx, value)
}

fn checked_pow10<'js>(
    ctx: &Ctx<'js>,
    exponent: BigInt,
    operation: &str,
) -> rquickjs::Result<BigInt> {
    if exponent.is_negative() {
        return Err(rquickjs::Exception::throw_range(ctx, "negative exponent"));
    }
    let exponent = exponent
        .to_u32()
        .ok_or_else(|| native_limit(ctx, operation))?;
    let estimated_bits = (f64::from(exponent) * std::f64::consts::LOG2_10).ceil() as u64;
    if estimated_bits > MAX_RESULT_BITS {
        return Err(native_limit(ctx, operation));
    }
    Ok(BigInt::from(10u8).pow(exponent))
}

fn pow10<'js>(ctx: &Ctx<'js>, digits: usize, operation: &str) -> rquickjs::Result<BigInt> {
    let exponent = coerce_bigint(ctx, digits.into_js(ctx)?)?;
    checked_pow10(ctx, exponent, operation)
}

mod install;
mod math;
mod methods;
mod parse;

pub(crate) use install::ecma_to_i32;
pub(super) use install::{install, seed_factory};
pub(crate) use math::*;
pub(crate) use methods::*;
pub(crate) use parse::*;
