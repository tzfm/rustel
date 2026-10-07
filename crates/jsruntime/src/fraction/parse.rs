use super::*;

pub(crate) fn tokenise_fraction_string(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut digits = String::new();
    for ch in value.chars().filter(|ch| *ch != '_') {
        if ch.is_ascii_digit() {
            digits.push(ch);
            continue;
        }
        if !digits.is_empty() {
            tokens.push(std::mem::take(&mut digits));
        }
        // JavaScript's `/./g` does not match line terminators.  The original
        // regexp consequently skips them instead of tokenising them.
        if !matches!(ch, '\n' | '\r' | '\u{2028}' | '\u{2029}') {
            tokens.push(ch.to_string());
        }
    }
    if !digits.is_empty() {
        tokens.push(digits);
    }
    tokens
}

pub(crate) fn assigned<'js>(
    ctx: &Ctx<'js>,
    token: Option<&String>,
    sign: &BigInt,
) -> rquickjs::Result<BigInt> {
    let token = token.ok_or_else(|| invalid_parameter(ctx))?;
    coerce_bigint(ctx, token.clone().into_js(ctx)?)
        .map(|value| value * sign)
        .map_err(|_| invalid_parameter(ctx))
}

pub(crate) fn token_is(tokens: &[String], index: usize, expected: &str) -> bool {
    tokens.get(index).is_some_and(|token| token == expected)
}

pub(crate) fn parse_string<'js>(ctx: &Ctx<'js>, value: &str) -> rquickjs::Result<Parts> {
    let tokens = tokenise_fraction_string(value);
    if tokens.is_empty() {
        return Err(invalid_parameter(ctx));
    }

    let mut index = 0usize;
    let mut sign = BigInt::one();
    let mut v = BigInt::zero();
    let mut w = BigInt::zero();
    let mut x = BigInt::zero();
    let mut y = BigInt::one();
    let mut z = BigInt::one();

    if token_is(&tokens, index, "-") {
        sign = -BigInt::one();
        index += 1;
    } else if token_is(&tokens, index, "+") {
        index += 1;
    }

    if tokens.len() == index + 1 {
        w = assigned(ctx, tokens.get(index), &sign)?;
        index += 1;
    } else if token_is(&tokens, index + 1, ".") || token_is(&tokens, index, ".") {
        if !token_is(&tokens, index, ".") {
            v = assigned(ctx, tokens.get(index), &sign)?;
            index += 1;
        }
        index += 1;

        let finite_decimal = index + 1 == tokens.len()
            || ((token_is(&tokens, index + 1, "(") && token_is(&tokens, index + 3, ")"))
                || (token_is(&tokens, index + 1, "'") && token_is(&tokens, index + 3, "'")));
        if finite_decimal {
            let token = tokens.get(index).ok_or_else(|| invalid_parameter(ctx))?;
            w = assigned(ctx, Some(token), &sign)?;
            y = pow10(ctx, token.len(), "decimal parsing")?;
            index += 1;
        }

        let repeating = (token_is(&tokens, index, "(") && token_is(&tokens, index + 2, ")"))
            || (token_is(&tokens, index, "'") && token_is(&tokens, index + 2, "'"));
        if repeating {
            let token = tokens
                .get(index + 1)
                .ok_or_else(|| invalid_parameter(ctx))?;
            x = assigned(ctx, Some(token), &sign)?;
            z = pow10(ctx, token.len(), "repeating-decimal parsing")? - 1;
            index += 3;
        }
    } else if token_is(&tokens, index + 1, "/") || token_is(&tokens, index + 1, ":") {
        w = assigned(ctx, tokens.get(index), &sign)?;
        y = assigned(ctx, tokens.get(index + 2), &BigInt::one())?;
        index += 3;
    } else if token_is(&tokens, index + 3, "/") && token_is(&tokens, index + 1, " ") {
        v = assigned(ctx, tokens.get(index), &sign)?;
        w = assigned(ctx, tokens.get(index + 2), &sign)?;
        y = assigned(ctx, tokens.get(index + 4), &BigInt::one())?;
        index += 5;
    }

    if tokens.len() > index {
        return Err(invalid_parameter(ctx));
    }
    let d = check_size(ctx, &y * &z, "string parsing")?;
    if d.is_zero() {
        return Err(division_by_zero(ctx));
    }
    let n = check_size(ctx, x + &d * v + z * w, "string parsing")?;
    Ok(Parts {
        s: if n.is_negative() {
            -BigInt::one()
        } else {
            BigInt::one()
        },
        n: n.abs(),
        d: d.abs(),
    })
}

pub(crate) fn parse_number<'js>(ctx: &Ctx<'js>, mut value: f64) -> rquickjs::Result<Parts> {
    if value.is_nan() {
        return Err(invalid_parameter(ctx));
    }
    // fraction.js eventually asks BigInt(Infinity) after ten million Farey
    // iterations. Preserve the exception class without doing attacker-sized
    // work first. The global BigInt binding is replaceable by a score, so it
    // can also return successfully here instead of throwing.
    if !value.is_finite() {
        let raw = Value::new_float(ctx.clone(), value);
        let _ = coerce_bigint(ctx, raw)?;
        return Err(rquickjs::Exception::throw_range(
            ctx,
            "cannot convert NaN or Infinity to BigInt",
        ));
    }

    let negative = value < 0.0;
    if negative {
        value = -value;
    }
    let mut n = BigInt::zero();
    let mut d = BigInt::one();

    if value % 1.0 == 0.0 {
        n = coerce_bigint(ctx, Value::new_float(ctx.clone(), value))?;
    } else if value > 0.0 {
        let mut z = 1.0;
        let (mut a, mut b, mut c, mut dd) = (0.0, 1.0, 1.0, 1.0);
        const LIMIT: f64 = 10_000_000.0;
        if value >= 1.0 {
            z = 10f64.powf((1.0 + value.log10()).floor());
            value /= z;
        }
        let mut steps = 0usize;
        while b <= LIMIT && dd <= LIMIT {
            if steps >= MAX_FAREY_BATCHES {
                return Err(native_limit(ctx, "number parsing"));
            }
            steps += 1;
            let mediant = (a + c) / (b + dd);
            if value == mediant {
                if b + dd <= LIMIT {
                    n = BigInt::from_f64(a + c).ok_or_else(|| invalid_parameter(ctx))?;
                    d = BigInt::from_f64(b + dd).ok_or_else(|| invalid_parameter(ctx))?;
                } else if dd > b {
                    n = BigInt::from_f64(c).ok_or_else(|| invalid_parameter(ctx))?;
                    d = BigInt::from_f64(dd).ok_or_else(|| invalid_parameter(ctx))?;
                } else {
                    n = BigInt::from_f64(a).ok_or_else(|| invalid_parameter(ctx))?;
                    d = BigInt::from_f64(b).ok_or_else(|| invalid_parameter(ctx))?;
                }
                break;
            } else if value > mediant {
                // Consecutive lower-bound updates are additions of the same
                // upper bound. Batch all but the last possible update, then
                // let the next loop perform the exact Fraction.js comparison.
                // This is the same Stern-Brocot walk without attacker-sized
                // linear runs for an otherwise ordinary binary64 value.
                let ratio = (value * b - a) / (c - value * dd);
                let before_direction_change = (ratio.floor() - 1.0).max(1.0);
                let before_limit_exit = ((LIMIT - b) / dd).floor() + 1.0;
                let batch = before_direction_change.min(before_limit_exit).max(1.0);
                a += batch * c;
                b += batch * dd;
            } else {
                let ratio = (c - value * dd) / (value * b - a);
                let before_direction_change = (ratio.floor() - 1.0).max(1.0);
                let before_limit_exit = ((LIMIT - dd) / b).floor() + 1.0;
                let batch = before_direction_change.min(before_limit_exit).max(1.0);
                c += batch * a;
                dd += batch * b;
            }
            if b > LIMIT {
                n = BigInt::from_f64(c).ok_or_else(|| invalid_parameter(ctx))?;
                d = BigInt::from_f64(dd).ok_or_else(|| invalid_parameter(ctx))?;
            } else {
                n = BigInt::from_f64(a).ok_or_else(|| invalid_parameter(ctx))?;
                d = BigInt::from_f64(b).ok_or_else(|| invalid_parameter(ctx))?;
            }
        }
        let raw_n = n.to_f64().ok_or_else(|| invalid_parameter(ctx))?;
        let raw_d = d.to_f64().ok_or_else(|| invalid_parameter(ctx))?;
        n = coerce_bigint(ctx, Value::new_float(ctx.clone(), raw_n))?
            * coerce_bigint(ctx, Value::new_float(ctx.clone(), z))?;
        d = coerce_bigint(ctx, Value::new_float(ctx.clone(), raw_d))?;
    }

    // A score may replace the global BigInt binding. A successful coercion
    // can therefore produce zero even when the source denominator was valid.
    // Method arguments use these parts directly, before normalisation.
    if d.is_zero() {
        return Err(division_by_zero(ctx));
    }

    Ok(Parts {
        s: if negative {
            -BigInt::one()
        } else {
            BigInt::one()
        },
        n,
        d,
    })
}

pub(crate) fn parse<'js>(
    ctx: &Ctx<'js>,
    first: Value<'js>,
    second: Option<Value<'js>>,
) -> rquickjs::Result<Parts> {
    if first.is_undefined() || first.is_null() {
        return Ok(Parts::zero());
    }
    if let Some(second) = second.filter(|value| !value.is_undefined()) {
        let n = parse_integer_argument(ctx, first)?;
        let d = parse_integer_argument(ctx, second)?;
        if d.is_zero() {
            return Err(division_by_zero(ctx));
        }
        let product = &n * &d;
        return Ok(Parts {
            s: if product.is_negative() {
                -BigInt::one()
            } else {
                BigInt::one()
            },
            n: n.abs(),
            d: d.abs(),
        });
    }

    if is_object_type(&first) {
        let object = first.as_object().ok_or_else(|| invalid_parameter(ctx))?;
        let (mut n, d) = if object.contains_key("d")? && object.contains_key("n")? {
            let mut n = coerce_bigint(ctx, object.get("n")?)?;
            let d = coerce_bigint(ctx, object.get("d")?)?;
            if object.contains_key("s")? {
                n *= coerce_bigint(ctx, object.get("s")?)?;
            }
            (n, d)
        } else if object.contains_key(0)? {
            let n = coerce_bigint(ctx, object.get(0)?)?;
            let d = if object.contains_key(1)? {
                coerce_bigint(ctx, object.get(1)?)?
            } else {
                BigInt::one()
            };
            (n, d)
        } else {
            return Err(invalid_parameter(ctx));
        };
        if d.is_zero() {
            return Err(division_by_zero(ctx));
        }
        let product = &n * &d;
        let sign = if product.is_negative() {
            -BigInt::one()
        } else {
            BigInt::one()
        };
        n = n.abs();
        return Ok(Parts {
            s: sign,
            n,
            d: d.abs(),
        });
    }
    if let Some(number) = first.as_number() {
        return parse_number(ctx, number);
    }
    if let Some(string) = first.as_string() {
        return parse_string(ctx, &string.to_string()?);
    }
    if first.type_of() == Type::BigInt {
        let n = bigint_from_primitive(ctx, &first)?;
        return Ok(Parts {
            s: if n.is_negative() {
                -BigInt::one()
            } else {
                BigInt::one()
            },
            n: n.abs(),
            d: BigInt::one(),
        });
    }
    Err(invalid_parameter(ctx))
}

pub(crate) fn argument<'js>(ctx: &Ctx<'js>, args: &[Value<'js>]) -> rquickjs::Result<Parts> {
    let first = args
        .first()
        .cloned()
        .unwrap_or_else(|| Value::new_undefined(ctx.clone()));
    parse(ctx, first, args.get(1).cloned())
}
