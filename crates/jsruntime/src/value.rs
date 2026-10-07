use num_traits::ToPrimitive as _;

use super::*;

impl<'js> Trace<'js> for PatternWrapper<'js> {
    fn trace<'a>(&self, tracer: Tracer<'a, 'js>) {
        if self.pattern.purity().opaque {
            // The graph contains an opaque closure, so which callbacks it will
            // materialise cannot be enumerated statically. Mark EVERY cell.
            for cell in &self.cells {
                cell.trace(tracer);
            }
        } else {
            // O(k) over the precomputed reachable set. The derive would mark
            // every cell rather than the reachable subset.
            for id in self.pattern.reachable_callbacks() {
                if let Some(cell) = self.cell(*id) {
                    cell.trace(tracer);
                }
            }
        }
    }
}

impl Drop for PatternWrapper<'_> {
    fn drop(&mut self) {
        WRAPPERS_DROPPED.with(|c| c.set(c.get() + 1));
    }
}

// ---------------------------------------------------------------------------
// Value marshalling
// ---------------------------------------------------------------------------

pub(super) fn to_js<'js>(ctx: &Ctx<'js>, v: &Value) -> rquickjs::Result<rquickjs::Value<'js>> {
    Ok(match v {
        Value::Undefined => rquickjs::Value::new_undefined(ctx.clone()),
        Value::Null => rquickjs::Value::new_null(ctx.clone()),
        Value::Bool(value) => rquickjs::Value::new_bool(ctx.clone(), *value),
        Value::F64(f) if *f == 0.0 && f.is_sign_negative() => {
            // rquickjs::Value::new_number optimises integral floats to the
            // QuickJS integer tag, which canonicalises -0 to +0. Reify/pure
            // exposes the value through `__pure`, where Object.is observes
            // that distinction exactly.
            rquickjs::Value::new_float(ctx.clone(), *f)
        }
        Value::F64(f) => rquickjs::Value::new_number(ctx.clone(), *f),
        Value::Str(s) => rquickjs::String::from_str(ctx.clone(), s)?.into_value(),
        Value::Function(f) => {
            if let Some(id) = f.callback_id() {
                // Public reify/pure preserves the exact JS function as both
                // `__pure` and the eventual hap value. The wrapper owns the
                // callback cell; the bridge frame owns it during construction.
                JsRuntime::with_callback(ctx, id, |function| Ok(function.into_value())).map_err(
                    |message| {
                        rquickjs::Error::new_from_js_message(
                            "FunctionRef",
                            "JavaScript function",
                            message,
                        )
                    },
                )?
            } else {
                // Native transformer references are consumed by joins and do
                // not have a unique JavaScript function object to reconstruct.
                rquickjs::String::from_str(ctx.clone(), &format!("{f:?}"))?.into_value()
            }
        }
        Value::Haps(haps) => {
            let array = rquickjs::Array::new(ctx.clone())?;
            for (index, hap) in haps.as_slice().iter().enumerate() {
                array.set(index, callback_hap(ctx, hap)?)?;
            }
            array.into_value()
        }
        Value::Pattern(pattern) => {
            JsRuntime::with_js_value(ctx, pattern.id(), Ok).map_err(|message| {
                rquickjs::Error::new_from_js_message("PatternValue", "JavaScript Pattern", message)
            })?
        }
        Value::JsValue(reference) => {
            JsRuntime::with_js_value(ctx, reference.id(), Ok).map_err(|message| {
                rquickjs::Error::new_from_js_message("JsValue", "JavaScript value", message)
            })?
        }
        Value::List(xs) => {
            let arr = rquickjs::Array::new(ctx.clone())?;
            for (i, x) in xs.iter().enumerate() {
                arr.set(i, to_js(ctx, x)?)?;
            }
            arr.into_value()
        }
        Value::Object(entries) => {
            let object = rquickjs::Object::new(ctx.clone())?;
            for (key, value) in entries {
                object.set(key.as_str(), to_js(ctx, value)?)?;
            }
            object.into_value()
        }
    })
}

/// Read a time value exactly: a bridge Fraction's integer `n`/`d`/`s` limbs
/// cross without passing through f64, and limbs whose value does not fit an
/// `i128` fraction are refused. A plain number takes the same Farey search as
/// `Fraction(number)`, so both routes read `1/3` as 1/3.
pub(super) fn fraction_property<'js>(
    ctx: &Ctx<'js>,
    object: &rquickjs::Object<'js>,
    name: &str,
) -> rquickjs::Result<Fraction> {
    let unrepresentable = || {
        throw_type_error(
            ctx,
            &format!("{name} is not a finite number representable as a time fraction"),
        )
    };
    let value: rquickjs::Value = object.get(name)?;
    if let Some(fraction) = value.as_object() {
        // `Some(None)` is an integer limb outside `i128`.
        let read_int = |field: &str| -> rquickjs::Result<Option<Option<i128>>> {
            let raw: rquickjs::Value = fraction.get(field)?;
            if raw.as_big_int().is_some() {
                // Exact, unlike rquickjs `to_i64`, which wraps modulo 2^64.
                let int = crate::fraction::bigint_from_primitive(ctx, &raw)?;
                return Ok(Some(int.to_i128()));
            }
            if let Some(number) = raw.as_number()
                && number.is_finite()
                && number.fract() == 0.0
            {
                return Ok(Some(number.to_i128()));
            }
            Ok(None)
        };
        if let (Some(n), Some(d), Some(sign)) = (read_int("n")?, read_int("d")?, read_int("s")?)
            && d != Some(0)
        {
            return n
                .zip(d)
                .zip(sign)
                .and_then(|((n, d), sign)| Fraction::checked_new(sign.checked_mul(n)?, d))
                .ok_or_else(unrepresentable);
        }
    }
    let number = number_property(ctx, object, name)?;
    // NaN, ±∞ and magnitudes beyond i128 have no rational here, so they are
    // refused rather than rounded.
    Fraction::from_f64(number).ok_or_else(unrepresentable)
}

pub(super) fn number_property<'js>(
    ctx: &Ctx<'js>,
    object: &rquickjs::Object<'js>,
    name: &str,
) -> rquickjs::Result<f64> {
    let value: rquickjs::Value = object.get(name)?;
    if let Some(number) = value.as_number() {
        return Ok(number);
    }
    let number: Function = ctx.globals().get("Number")?;
    number.call((value,))
}

/// [`from_js_within`] with a fresh element budget. This converter cannot
/// throw, so a container past the budget or nested deeper than
/// [`MAX_JS_VALUE_DEPTH`] converts as `Value::Undefined`; a
/// caller that can refuse instead (a combinator reference) uses the budgeted
/// form directly.
pub(super) fn from_js(v: &rquickjs::Value<'_>) -> Value {
    // A primitive draws nothing, so only a container pays for the ceiling
    // lookup; both current callers pass primitives.
    let cap = if v.is_object() {
        js_element_cap::<Value>(v.ctx()).unwrap_or(0)
    } else {
        0
    };
    from_js_within(v, &Cell::new(cap)).unwrap_or(Value::Undefined)
}

/// Mirror a JS value as a host [`Value`], charging array slots and object
/// properties against one budget shared by the whole tree. A sparse array's length can
/// be much larger than its storage on the JS heap.
///
/// Returns `None` when the tree exceeds the budget, nests deeper than
/// [`MAX_JS_VALUE_DEPTH`], or a string's bytes, a BigInt's digits, an
/// array's length or host storage cannot be obtained. Budget refusal itself
/// does not throw.
pub(super) fn from_js_within(v: &rquickjs::Value<'_>, remaining: &Cell<usize>) -> Option<Value> {
    from_js_within_depth(v, remaining, 0)
}

fn from_js_within_depth(
    v: &rquickjs::Value<'_>,
    remaining: &Cell<usize>,
    depth: usize,
) -> Option<Value> {
    if depth >= MAX_JS_VALUE_DEPTH && v.is_object() {
        return None;
    }
    Some(if v.is_undefined() {
        Value::Undefined
    } else if v.is_null() {
        Value::Null
    } else if let Some(value) = v.as_bool() {
        Value::Bool(value)
    } else if let Some(n) = v.as_number() {
        Value::F64(n)
    } else if let Some(s) = v.as_string() {
        Value::Str(js_string_text(s)?)
    } else if v.is_big_int() {
        // `Number(bigint)`: the exact decimal digits parse to the correctly
        // rounded f64, and magnitudes past f64 read as ±infinity.
        let digits = rquickjs::Coerced::<String>::from_js(v.ctx(), v.clone()).ok()?;
        Value::F64(digits.0.parse().ok()?)
    } else if let Some(arr) = v.as_array() {
        // A length claim (see `js_array_len`), charged before the walk and
        // walked exactly; a failing element read is skipped, as the old
        // iterator's `flatten` did.
        let len = js_array_len(arr).ok()?;
        if !try_charge_js_elements(len, remaining) {
            return None;
        }
        let mut items = Vec::new();
        items.try_reserve_exact(len).ok()?;
        for index in 0..len {
            if let Ok(item) = arr.get::<rquickjs::Value>(index) {
                items.push(from_js_within_depth(&item, remaining, depth + 1)?);
            }
        }
        Value::List(items)
    } else if let Some(object) = v.as_object() {
        // Each property charges one element, so a child shared by several
        // keys cannot multiply the walk past the budget. Arrays reached
        // through the values charge their lengths.
        let mut entries = Vec::new();
        for (key, value) in object.props::<String, rquickjs::Value>().flatten() {
            if !try_charge_js_elements(1, remaining) {
                return None;
            }
            entries.push((key, from_js_within_depth(&value, remaining, depth + 1)?));
        }
        Value::object(entries)
    } else {
        Value::Str(format!("{v:?}"))
    })
}

/// A JS string's text with each lone surrogate read as one U+FFFD, as
/// `String.prototype.toWellFormed` reads it. `None` when QuickJS cannot
/// produce the string's bytes.
fn js_string_text(s: &rquickjs::String<'_>) -> Option<String> {
    let bytes = s.clone().to_cstring().ok()?;
    // SAFETY: `bytes` keeps its `len()` bytes alive until it drops, after
    // `wtf8_repair` has copied them.
    let raw = unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast::<u8>(), bytes.len()) };
    Some(wtf8_repair(raw))
}

/// Decodes QuickJS's C-string bytes, which keep a lone surrogate as its
/// 3-byte WTF-8 encoding: each such sequence becomes one U+FFFD, and any
/// other ill-formed byte decodes lossily.
fn wtf8_repair(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut valid_from = 0;
    let mut index = 0;
    while index < bytes.len() {
        // ED A0..=BF encodes exactly U+D800..=U+DFFF, and ED is never a
        // continuation byte, so a match here always starts a sequence.
        if bytes[index] == 0xED
            && index + 2 < bytes.len()
            && matches!(bytes[index + 1], 0xA0..=0xBF)
            && matches!(bytes[index + 2], 0x80..=0xBF)
        {
            out.push_str(&String::from_utf8_lossy(&bytes[valid_from..index]));
            out.push('\u{FFFD}');
            index += 3;
            valid_from = index;
        } else {
            index += 1;
        }
    }
    out.push_str(&String::from_utf8_lossy(&bytes[valid_from..]));
    out
}

pub(super) fn fraction_seed<'js>(
    ctx: &Ctx<'js>,
    value: Fraction,
) -> rquickjs::Result<rquickjs::Object<'js>> {
    let seed = rquickjs::Object::new(ctx.clone())?;
    seed.set("n", value.numer().to_string())?;
    seed.set("d", value.denom().to_string())?;
    Ok(seed)
}

pub(super) fn span_seed<'js>(
    ctx: &Ctx<'js>,
    span: rustel_core::TimeSpan,
) -> rquickjs::Result<rquickjs::Object<'js>> {
    let seed = rquickjs::Object::new(ctx.clone())?;
    seed.set("begin", fraction_seed(ctx, span.begin)?)?;
    seed.set("end", fraction_seed(ctx, span.end)?)?;
    Ok(seed)
}

pub(super) fn context_to_js<'js>(
    ctx: &Ctx<'js>,
    context: &[(usize, usize)],
) -> rquickjs::Result<rquickjs::Object<'js>> {
    let out = rquickjs::Object::new(ctx.clone())?;
    let locations = rquickjs::Array::new(ctx.clone())?;
    for (index, (start, end)) in context.iter().enumerate() {
        let location = rquickjs::Object::new(ctx.clone())?;
        location.set("start", *start)?;
        location.set("end", *end)?;
        locations.set(index, location)?;
    }
    out.set("locations", locations)?;
    Ok(out)
}

/// [`context_to_js`] plus the side-channel context keys the native Hap
/// carries (`edoSize`, set by xen's combinators and read back by ftrans on
/// the JS side of the bridge).
pub(super) fn hap_context_to_js<'js>(
    ctx: &Ctx<'js>,
    hap: &Hap,
) -> rquickjs::Result<rquickjs::Object<'js>> {
    let out = context_to_js(ctx, &hap.context)?;
    if let Some(edo_size) = hap.edo_size_context() {
        out.set("edoSize", edo_size)?;
    }
    // `hap.hasTag(t)` reads `this.context.tags`, so the list has to be on the
    // object a filter callback receives, not only on the native hap.
    if let Some(tags) = hap.tags_context() {
        let array = rquickjs::Array::new(ctx.clone())?;
        for (index, tag) in tags.iter().enumerate() {
            array.set(index, tag.as_ref())?;
        }
        out.set("tags", array)?;
    }
    // `context.scaleDefinition`, as edoScale tagged it. The stored Value is
    // the same JSON shape the raw argument object serializes to.
    if let Some(definition) = hap.scale_definition_context() {
        out.set("scaleDefinition", to_js(ctx, definition)?)?;
    }
    if let Some(line) = hap.log_line() {
        out.set("logLine", line)?;
    }
    Ok(out)
}

/// Convert one native hap into the real object shape `arpWith` exposes.
///
/// The factory is cached on the runtime-private host root, so creating a
/// chord allocates only the Hap/Fraction/TimeSpan instances themselves and
/// does not compile JavaScript or create per-hap method functions. The root is
/// internal bookkeeping and is not exposed for user mutation.
pub(super) fn callback_hap<'js>(
    ctx: &Ctx<'js>,
    hap: &Hap,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let stack = host_stack(ctx)?;
    let factory: Function = stack.as_object().get(HAP_FACTORY)?;
    let whole: rquickjs::Value = match hap.whole {
        Some(span) => span_seed(ctx, span)?.into_value(),
        None => rquickjs::Value::new_undefined(ctx.clone()),
    };
    let part = span_seed(ctx, hap.part)?;
    let value = to_js(ctx, &hap.value)?;
    let context = hap_context_to_js(ctx, hap)?;
    factory.call((whole, part, value, context))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonthrowing_conversion_fails_closed_on_cycles_and_deep_objects() {
        let runtime = JsRuntime::new().expect("runtime");
        with_ctx(&runtime.ctx, |ctx| {
            for source in [
                "(() => { const o = {}; o.self = o; return o; })()",
                "(() => { let o = 1; for (let i = 0; i < 300; i++) o = {next: o}; return o; })()",
            ] {
                let value: rquickjs::Value = ctx.eval(source).expect("JS value");
                assert!(
                    from_js_within(&value, &Cell::new(4096)).is_none(),
                    "{source} must exceed the depth cap"
                );
            }

            let shallow: rquickjs::Value = ctx.eval("({next: {value: 1}})").expect("shallow value");
            assert!(from_js_within(&shallow, &Cell::new(4096)).is_some());
        });
    }
}
