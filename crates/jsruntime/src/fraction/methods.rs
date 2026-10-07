use super::*;

pub(crate) fn invoke<'js>(
    method: Method,
    ctx: Ctx<'js>,
    this: This<Object<'js>>,
    args: Rest<Value<'js>>,
) -> rquickjs::Result<Value<'js>> {
    let this_object = this.0;
    let args = args.0;
    match method {
        Method::Abs => {
            let n = read_field(&ctx, &this_object, "n")?;
            let d = read_field(&ctx, &this_object, "d")?;
            new_fraction(&ctx, n, d)
        }
        Method::Neg => {
            let left = read_parts(&ctx, &this_object)?;
            new_fraction(&ctx, -left.s * left.n, left.d)
        }
        Method::Add => {
            let right = argument(&ctx, &args)?;
            let s = read_field(&ctx, &this_object, "s")?;
            let n = read_field(&ctx, &this_object, "n")?;
            let d_numerator = read_field(&ctx, &this_object, "d")?;
            let d_result = read_field(&ctx, &this_object, "d")?;
            new_fraction(
                &ctx,
                s * n * &right.d + &right.s * d_numerator * &right.n,
                d_result * right.d,
            )
        }
        Method::Sub => {
            let right = argument(&ctx, &args)?;
            let s = read_field(&ctx, &this_object, "s")?;
            let n = read_field(&ctx, &this_object, "n")?;
            let d_numerator = read_field(&ctx, &this_object, "d")?;
            let d_result = read_field(&ctx, &this_object, "d")?;
            new_fraction(
                &ctx,
                s * n * &right.d - &right.s * d_numerator * &right.n,
                d_result * right.d,
            )
        }
        Method::Mul => {
            let (left, right) = binary_parts(&ctx, &this_object, &args)?;
            new_fraction(&ctx, left.s * right.s * left.n * right.n, left.d * right.d)
        }
        Method::Div => {
            let (left, right) = binary_parts(&ctx, &this_object, &args)?;
            new_fraction(&ctx, left.s * right.s * left.n * right.d, left.d * right.n)
        }
        Method::Clone => {
            let left = read_parts(&ctx, &this_object)?;
            new_fraction(&ctx, left.s * left.n, left.d)
        }
        Method::Mod => {
            if args.first().is_none_or(Value::is_undefined) {
                let left = read_parts(&ctx, &this_object)?;
                if left.d.is_zero() {
                    return Err(invalid_bigint_operation(&ctx));
                }
                return new_fraction(&ctx, left.s * left.n % left.d, BigInt::one());
            }
            let right = argument(&ctx, &args)?;
            let d_check = read_field(&ctx, &this_object, "d")?;
            let divisor = &right.n * d_check;
            if divisor.is_zero() {
                return Err(division_by_zero(&ctx));
            }
            let s = read_field(&ctx, &this_object, "s")?;
            let n = read_field(&ctx, &this_object, "n")?;
            let d_modulus = read_field(&ctx, &this_object, "d")?;
            let d_result = read_field(&ctx, &this_object, "d")?;
            if d_modulus.is_zero() {
                return Err(invalid_bigint_operation(&ctx));
            }
            new_fraction(
                &ctx,
                s * (right.d.clone() * n) % (&right.n * d_modulus),
                right.d * d_result,
            )
        }
        Method::Gcd => {
            let right = argument(&ctx, &args)?;
            let n = read_field(&ctx, &this_object, "n")?;
            let d_numerator = read_field(&ctx, &this_object, "d")?;
            let d_result = read_field(&ctx, &this_object, "d")?;
            new_fraction(
                &ctx,
                gcd(&ctx, right.n, n, "gcd")? * gcd(&ctx, right.d.clone(), d_numerator, "gcd")?,
                right.d * d_result,
            )
        }
        Method::Lcm => {
            let right = argument(&ctx, &args)?;
            if right.n.is_zero() {
                let n = read_field(&ctx, &this_object, "n")?;
                if n.is_zero() {
                    return new_fraction(&ctx, BigInt::zero(), BigInt::one());
                }
            }
            let n_result = read_field(&ctx, &this_object, "n")?;
            let n_gcd = read_field(&ctx, &this_object, "n")?;
            let d_gcd = read_field(&ctx, &this_object, "d")?;
            new_fraction(
                &ctx,
                &right.n * n_result,
                gcd(&ctx, right.n, n_gcd, "lcm")? * gcd(&ctx, right.d, d_gcd, "lcm")?,
            )
        }
        Method::Inverse => {
            let s = read_field(&ctx, &this_object, "s")?;
            let d = read_field(&ctx, &this_object, "d")?;
            let n = read_field(&ctx, &this_object, "n")?;
            new_fraction(&ctx, s * d, n)
        }
        Method::Pow => {
            let (left, right) = binary_parts(&ctx, &this_object, &args)?;
            pow_method(&ctx, &left, right)
        }
        Method::Log => {
            let (left, right) = binary_parts(&ctx, &this_object, &args)?;
            log_method(&ctx, &left, right)
        }
        Method::Equals | Method::Lt | Method::Lte | Method::Gt | Method::Gte => {
            let (left, right) = binary_parts(&ctx, &this_object, &args)?;
            let comparison = compare_parts(&left, &right);
            into_value(
                &ctx,
                match method {
                    Method::Equals => comparison.is_zero(),
                    Method::Lt => comparison < BigInt::zero(),
                    Method::Lte => comparison <= BigInt::zero(),
                    Method::Gt => comparison > BigInt::zero(),
                    Method::Gte => comparison >= BigInt::zero(),
                    _ => unreachable!(),
                },
            )
        }
        Method::Compare => {
            let (left, right) = binary_parts(&ctx, &this_object, &args)?;
            let comparison = compare_parts(&left, &right);
            into_value(
                &ctx,
                if comparison.is_positive() {
                    1
                } else if comparison.is_negative() {
                    -1
                } else {
                    0
                },
            )
        }
        Method::Ceil | Method::Floor | Method::Round => {
            let scale = places(&ctx, &args)?;
            let first_s = read_field(&ctx, &this_object, "s")?;
            let first_n = read_field(&ctx, &this_object, "n")?;
            let first_d = read_field(&ctx, &this_object, "d")?;
            if first_d.is_zero() {
                return Err(invalid_bigint_operation(&ctx));
            }
            let quotient = &first_s * &scale * first_n / first_d;
            let numerator = match method {
                Method::Ceil => {
                    let n = read_field(&ctx, &this_object, "n")?;
                    let d = read_field(&ctx, &this_object, "d")?;
                    if d.is_zero() {
                        return Err(invalid_bigint_operation(&ctx));
                    }
                    let remainder = &scale * n % d;
                    let increment = if remainder.is_positive() {
                        let s = read_field(&ctx, &this_object, "s")?;
                        (!s.is_negative()).then_some(BigInt::one())
                    } else {
                        None
                    };
                    quotient + increment.unwrap_or_else(BigInt::zero)
                }
                Method::Floor => {
                    let n = read_field(&ctx, &this_object, "n")?;
                    let d = read_field(&ctx, &this_object, "d")?;
                    if d.is_zero() {
                        return Err(invalid_bigint_operation(&ctx));
                    }
                    let remainder = &scale * n % d;
                    let decrement = if remainder.is_positive() {
                        let s = read_field(&ctx, &this_object, "s")?;
                        s.is_negative().then_some(BigInt::one())
                    } else {
                        None
                    };
                    quotient - decrement.unwrap_or_else(BigInt::zero)
                }
                Method::Round => {
                    let multiplier_s = read_field(&ctx, &this_object, "s")?;
                    let comparison_s = read_field(&ctx, &this_object, "s")?;
                    let n = read_field(&ctx, &this_object, "n")?;
                    let d_modulus = read_field(&ctx, &this_object, "d")?;
                    if d_modulus.is_zero() {
                        return Err(invalid_bigint_operation(&ctx));
                    }
                    let remainder = &scale * n % d_modulus;
                    let d_comparison = read_field(&ctx, &this_object, "d")?;
                    let bias = if comparison_s.is_negative() {
                        BigInt::zero()
                    } else {
                        BigInt::one()
                    };
                    quotient
                        + multiplier_s
                            * if bias + 2 * remainder > d_comparison {
                                BigInt::one()
                            } else {
                                BigInt::zero()
                            }
                }
                _ => unreachable!(),
            };
            new_fraction(&ctx, numerator, scale)
        }
        Method::RoundTo => {
            let right = argument(&ctx, &args)?;
            let left_n = read_field(&ctx, &this_object, "n")?;
            let left_d = read_field(&ctx, &this_object, "d")?;
            let n = left_n * &right.d;
            let d = left_d * &right.n;
            if d.is_zero() {
                return Err(invalid_bigint_operation(&ctx));
            }
            let remainder = &n % &d;
            let mut multiple = &n / &d;
            if &remainder + &remainder >= d {
                multiple += 1;
            }
            let s = read_field(&ctx, &this_object, "s")?;
            new_fraction(&ctx, s * multiple * right.n, right.d)
        }
        Method::Divisible => {
            let right = argument(&ctx, &args)?;
            let first_d = read_field(&ctx, &this_object, "d")?;
            let divisor = &right.n * first_d;
            if divisor.is_zero() {
                return into_value(&ctx, false);
            }
            let n = read_field(&ctx, &this_object, "n")?;
            let second_d = read_field(&ctx, &this_object, "d")?;
            let second_divisor = &right.n * second_d;
            if second_divisor.is_zero() {
                return Err(invalid_bigint_operation(&ctx));
            }
            into_value(&ctx, (n * right.d % second_divisor).is_zero())
        }
        Method::ValueOf => {
            let left = read_parts(&ctx, &this_object)?;
            let numerator = (&left.s * &left.n).to_f64().unwrap_or_else(|| {
                if left.s.is_negative() {
                    f64::NEG_INFINITY
                } else {
                    f64::INFINITY
                }
            });
            let denominator = left.d.to_f64().unwrap_or(f64::INFINITY);
            into_value(&ctx, numerator / denominator)
        }
        Method::ToString => {
            let n = read_field(&ctx, &this_object, "n")?;
            let d = read_field(&ctx, &this_object, "d")?;
            let s = read_field(&ctx, &this_object, "s")?;
            let left = Parts { s, n, d };
            into_value(&ctx, to_decimal(&ctx, &left, &args)?)
        }
        Method::ToFraction | Method::ToLatex => {
            let n = read_field(&ctx, &this_object, "n")?;
            let d = read_field(&ctx, &this_object, "d")?;
            let s = read_field(&ctx, &this_object, "s")?;
            let left = Parts { s, n, d };
            let mixed = match args.first() {
                Some(value) => is_truthy(&ctx, value.clone())?,
                None => false,
            };
            let mut n = left.n;
            let d = left.d;
            if d.is_zero() {
                return Err(invalid_bigint_operation(&ctx));
            }
            let mut output = if left.s.is_negative() {
                "-".to_owned()
            } else {
                String::new()
            };
            if d.is_one() {
                output.push_str(&n.to_string());
            } else {
                let whole = &n / &d;
                if mixed && whole > BigInt::zero() {
                    output.push_str(&whole.to_string());
                    if matches!(method, Method::ToFraction) {
                        output.push(' ');
                    }
                    n %= &d;
                }
                if matches!(method, Method::ToLatex) {
                    output.push_str("\\frac{");
                    output.push_str(&n.to_string());
                    output.push_str("}{");
                    output.push_str(&d.to_string());
                    output.push('}');
                } else {
                    output.push_str(&n.to_string());
                    output.push('/');
                    output.push_str(&d.to_string());
                }
            }
            into_value(&ctx, output)
        }
        Method::ToContinued => {
            let n = read_field(&ctx, &this_object, "n")?;
            let d = read_field(&ctx, &this_object, "d")?;
            let left = Parts {
                s: BigInt::one(),
                n,
                d,
            };
            if left.d.is_zero() {
                return Err(invalid_bigint_operation(&ctx));
            }
            let array = rquickjs::Array::new(ctx.clone())?;
            for (index, value) in continued(&ctx, &left)?.iter().enumerate() {
                array.set(index, js_bigint(&ctx, value)?)?;
            }
            Ok(array.into_value())
        }
        Method::Simplify => {
            let eps_value = args
                .first()
                .cloned()
                .unwrap_or_else(|| Value::new_undefined(ctx.clone()));
            let eps = if is_truthy(&ctx, eps_value.clone())? {
                Coerced::<f64>::from_js(&ctx, eps_value)?.0
            } else {
                0.001
            };
            let reciprocal = 1.0 / eps;
            let int32 = ecma_to_i32(reciprocal);
            let ieps = coerce_bigint(&ctx, int32.into_js(&ctx)?)?;

            let abs: Function = this_object.get("abs")?;
            let absolute_value: Value = abs.call((This(this_object.clone()),))?;
            let absolute = absolute_value
                .as_object()
                .ok_or_else(|| rquickjs::Exception::throw_type(&ctx, "not an object"))?
                .clone();
            let to_continued: Function = absolute.get("toContinued")?;
            let continued_value: Value = to_continued.call((This(absolute.clone()),))?;
            let sequence = continued_value
                .as_object()
                .ok_or_else(|| rquickjs::Exception::throw_type(&ctx, "not an object"))?
                .clone();
            let raw_length = Coerced::<f64>::from_js(&ctx, sequence.get("length")?)?.0;
            if !raw_length.is_finite() || raw_length > MAX_CONTINUED_LEN as f64 {
                return Err(native_limit(&ctx, "simplification"));
            }
            let length = if raw_length > 0.0 {
                raw_length.ceil() as usize
            } else {
                0
            };
            let simplify_steps = length
                .checked_mul(length.saturating_sub(1))
                .map(|value| value / 2)
                .ok_or_else(|| native_limit(&ctx, "simplification"))?;
            if simplify_steps > MAX_SIMPLIFY_STEPS {
                return Err(native_limit(&ctx, "simplification"));
            }

            for index in 1..length {
                let seed = bigint_from_primitive(&ctx, &sequence.get((index - 1) as u32)?)?;
                let mut candidate_value = new_fraction(&ctx, seed, BigInt::one())?;
                for item_index in (0..index - 1).rev() {
                    let candidate = candidate_value
                        .as_object()
                        .ok_or_else(|| rquickjs::Exception::throw_type(&ctx, "not an object"))?
                        .clone();
                    let inverse: Function = candidate.get("inverse")?;
                    let inverse_value: Value = inverse.call((This(candidate),))?;
                    let inverse_object = inverse_value
                        .as_object()
                        .ok_or_else(|| rquickjs::Exception::throw_type(&ctx, "not an object"))?
                        .clone();
                    let add: Function = inverse_object.get("add")?;
                    let item: Value = sequence.get(item_index as u32)?;
                    candidate_value = add.call((This(inverse_object), item))?;
                }

                let candidate = candidate_value
                    .as_object()
                    .ok_or_else(|| rquickjs::Exception::throw_type(&ctx, "not an object"))?
                    .clone();
                let sub: Function = candidate.get("sub")?;
                let delta_value: Value =
                    sub.call((This(candidate.clone()), absolute_value.clone()))?;
                let delta = delta_value
                    .as_object()
                    .ok_or_else(|| rquickjs::Exception::throw_type(&ctx, "not an object"))?;
                let delta_n = read_field(&ctx, delta, "n")?;
                let delta_d = read_field(&ctx, delta, "d")?;
                if delta_n * &ieps < delta_d {
                    let mul: Function = candidate.get("mul")?;
                    let sign: Value = this_object.get("s")?;
                    return mul.call((This(candidate), sign));
                }
            }
            Ok(this_object.into_value())
        }
    }
}
