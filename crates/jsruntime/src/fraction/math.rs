use super::*;

pub(crate) fn compare_parts(left: &Parts, right: &Parts) -> BigInt {
    &left.s * &left.n * &right.d - &right.s * &right.n * &left.d
}

pub(crate) fn checked_pow<'js>(
    ctx: &Ctx<'js>,
    base: &BigInt,
    exponent: &BigInt,
    operation: &str,
) -> rquickjs::Result<BigInt> {
    if exponent.is_negative() {
        return Err(rquickjs::Exception::throw_range(ctx, "negative exponent"));
    }
    if exponent.is_zero() || base.is_one() {
        return Ok(BigInt::one());
    }
    if base.is_zero() {
        return Ok(BigInt::zero());
    }
    if base == &-BigInt::one() {
        return Ok(if (exponent & BigInt::one()).is_one() {
            -BigInt::one()
        } else {
            BigInt::one()
        });
    }
    let bits = base.magnitude().bits().max(1);
    if exponent > &BigInt::from(MAX_RESULT_BITS / bits + 1) {
        return Err(native_limit(ctx, operation));
    }
    let exponent = exponent
        .to_u32()
        .ok_or_else(|| native_limit(ctx, operation))?;
    check_size(ctx, base.pow(exponent), operation)
}

pub(crate) fn factorize<'js>(
    ctx: &Ctx<'js>,
    value: &BigInt,
    operation: &str,
) -> rquickjs::Result<BTreeMap<BigInt, BigInt>> {
    let mut factors = BTreeMap::new();
    let mut n = value.clone();
    let mut i = BigInt::from(2u8);
    let mut square = BigInt::from(4u8);
    let mut steps = 0usize;
    let bit_cost = value.magnitude().bits().max(1);
    let work_steps = (MAX_FACTOR_WORK_BITS / bit_cost).max(1) as usize;
    let step_limit = MAX_FACTOR_STEPS.min(work_steps);
    while square <= n {
        while (&n % &i).is_zero() {
            n /= &i;
            *factors.entry(i.clone()).or_insert_with(BigInt::zero) += 1;
        }
        square += 1 + 2 * &i;
        i += 1;
        steps += 1;
        if steps > step_limit {
            return Err(native_limit(ctx, operation));
        }
    }
    if n != *value {
        if n > BigInt::one() {
            *factors.entry(n).or_insert_with(BigInt::zero) += 1;
        }
    } else {
        *factors.entry(value.clone()).or_insert_with(BigInt::zero) += 1;
    }
    Ok(factors)
}

pub(crate) fn pow_method<'js>(
    ctx: &Ctx<'js>,
    this: &Parts,
    exponent: Parts,
) -> rquickjs::Result<Value<'js>> {
    if exponent.d.is_one() {
        if exponent.s.is_negative() {
            let n = checked_pow(ctx, &(&this.s * &this.d), &exponent.n, "pow")?;
            let d = checked_pow(ctx, &this.n, &exponent.n, "pow")?;
            return new_fraction(ctx, n, d);
        }
        let n = checked_pow(ctx, &(&this.s * &this.n), &exponent.n, "pow")?;
        let d = checked_pow(ctx, &this.d, &exponent.n, "pow")?;
        return new_fraction(ctx, n, d);
    }
    if this.s.is_negative() {
        return Ok(Value::new_null(ctx.clone()));
    }
    let mut numerator = BigInt::one();
    let mut denominator = BigInt::one();
    for (prime, count) in factorize(ctx, &this.n, "pow factorization")? {
        if prime.is_one() {
            continue;
        }
        if prime.is_zero() {
            numerator = BigInt::zero();
            break;
        }
        let count = count * &exponent.n;
        if &count % &exponent.d != BigInt::zero() {
            return Ok(Value::new_null(ctx.clone()));
        }
        let parsed_prime = coerce_bigint(ctx, prime.to_string().into_js(ctx)?)?;
        numerator *= checked_pow(ctx, &parsed_prime, &(count / &exponent.d), "pow")?;
        numerator = check_size(ctx, numerator, "pow")?;
    }
    for (prime, count) in factorize(ctx, &this.d, "pow factorization")? {
        if prime.is_one() {
            continue;
        }
        let count = count * &exponent.n;
        if &count % &exponent.d != BigInt::zero() {
            return Ok(Value::new_null(ctx.clone()));
        }
        let parsed_prime = coerce_bigint(ctx, prime.to_string().into_js(ctx)?)?;
        denominator *= checked_pow(ctx, &parsed_prime, &(count / &exponent.d), "pow")?;
        denominator = check_size(ctx, denominator, "pow")?;
    }
    if exponent.s.is_negative() {
        new_fraction(ctx, denominator, numerator)
    } else {
        new_fraction(ctx, numerator, denominator)
    }
}

pub(crate) fn log_method<'js>(
    ctx: &Ctx<'js>,
    this: &Parts,
    base: Parts,
) -> rquickjs::Result<Value<'js>> {
    if this.s <= BigInt::zero() || base.s <= BigInt::zero() {
        return Ok(Value::new_null(ctx.clone()));
    }
    let mut base_factors = factorize(ctx, &base.n, "log factorization")?;
    for (prime, count) in factorize(ctx, &base.d, "log factorization")? {
        *base_factors.entry(prime).or_insert_with(BigInt::zero) -= count;
    }
    let mut number_factors = factorize(ctx, &this.n, "log factorization")?;
    for (prime, count) in factorize(ctx, &this.d, "log factorization")? {
        *number_factors.entry(prime).or_insert_with(BigInt::zero) -= count;
    }
    let mut primes = BTreeMap::new();
    for prime in base_factors.keys().chain(number_factors.keys()) {
        if !prime.is_one() {
            primes.insert(prime.clone(), ());
        }
    }
    let mut result: Option<(BigInt, BigInt)> = None;
    for prime in primes.keys() {
        let base_exp = base_factors
            .get(prime)
            .cloned()
            .unwrap_or_else(BigInt::zero);
        let number_exp = number_factors
            .get(prime)
            .cloned()
            .unwrap_or_else(BigInt::zero);
        if base_exp.is_zero() {
            if !number_exp.is_zero() {
                return Ok(Value::new_null(ctx.clone()));
            }
            continue;
        }
        let divisor = gcd(
            ctx,
            number_exp.clone(),
            base_exp.clone(),
            "log normalisation",
        )?;
        let current = (number_exp / &divisor, base_exp / divisor);
        if let Some((n, d)) = &result {
            if &current.0 * d != n * &current.1 {
                return Ok(Value::new_null(ctx.clone()));
            }
        } else {
            result = Some(current);
        }
    }
    match result {
        Some((n, d)) => new_fraction(ctx, n, d),
        None => Ok(Value::new_null(ctx.clone())),
    }
}

pub(crate) fn places<'js>(ctx: &Ctx<'js>, args: &[Value<'js>]) -> rquickjs::Result<BigInt> {
    let value = args
        .first()
        .cloned()
        .unwrap_or_else(|| Value::new_undefined(ctx.clone()));
    let exponent = if is_truthy(ctx, value.clone())? {
        coerce_bigint(ctx, value)?
    } else {
        BigInt::zero()
    };
    if exponent.is_negative() {
        return Err(rquickjs::Exception::throw_range(ctx, "negative exponent"));
    }
    checked_pow10(ctx, exponent, "decimal places")
}

pub(crate) fn modpow(mut base: BigInt, mut exponent: BigInt, modulus: &BigInt) -> BigInt {
    let mut result = BigInt::one();
    while exponent > BigInt::zero() {
        if (&exponent & BigInt::one()).is_one() {
            result = result * &base % modulus;
        }
        base = &base * &base % modulus;
        exponent >>= 1;
    }
    result
}

pub(crate) fn cycle_len(mut denominator: BigInt) -> usize {
    if denominator.magnitude().bits() > MAX_CYCLE_MODULUS_BITS {
        return 0;
    }
    if let Some(shift) = denominator.trailing_zeros() {
        denominator >>= shift;
    }
    let mut factor_steps = 0usize;
    while (&denominator % 5u8).is_zero() {
        denominator /= 5u8;
        factor_steps += 1;
        if factor_steps > MAX_CYCLE_LEN {
            return 0;
        }
    }
    if denominator.is_one() {
        return 0;
    }
    let mut remainder = BigInt::from(10u8) % &denominator;
    let mut length = 1usize;
    let bit_cost = denominator.magnitude().bits().max(1);
    let work_steps = (MAX_CYCLE_WORK_BITS / bit_cost).max(1) as usize;
    let step_limit = MAX_CYCLE_LEN.min(work_steps);
    while !remainder.is_one() {
        remainder = remainder * 10u8 % &denominator;
        if length > step_limit {
            return 0;
        }
        length += 1;
    }
    length
}

pub(crate) fn cycle_start<'js>(
    ctx: &Ctx<'js>,
    denominator: &BigInt,
    length: &BigInt,
) -> rquickjs::Result<BigInt> {
    let mut first = BigInt::one();
    let mut second = modpow(BigInt::from(10u8), length.clone(), denominator);
    for offset in 0..300usize {
        if first == second {
            return coerce_bigint(ctx, offset.into_js(ctx)?);
        }
        first = first * 10u8 % denominator;
        second = second * 10u8 % denominator;
    }
    Ok(BigInt::zero())
}

pub(crate) fn bounded_loop_count<'js>(
    ctx: &Ctx<'js>,
    value: &BigInt,
    limit: usize,
    operation: &str,
) -> rquickjs::Result<usize> {
    if value.is_negative() {
        return Ok(0);
    }
    let count = value
        .to_usize()
        .ok_or_else(|| native_limit(ctx, operation))?;
    if count > limit {
        Err(native_limit(ctx, operation))
    } else {
        Ok(count)
    }
}

pub(crate) fn check_loop_work<'js>(
    ctx: &Ctx<'js>,
    iterations: usize,
    bits: u64,
    limit: u64,
    operation: &str,
) -> rquickjs::Result<()> {
    let work = bits.max(1).saturating_mul(iterations as u64);
    if work > limit {
        Err(native_limit(ctx, operation))
    } else {
        Ok(())
    }
}

pub(crate) fn decimal_limit<'js>(ctx: &Ctx<'js>, args: &[Value<'js>]) -> rquickjs::Result<usize> {
    let value = args
        .first()
        .cloned()
        .unwrap_or_else(|| Value::new_undefined(ctx.clone()));
    if !is_truthy(ctx, value.clone())? {
        return Ok(15);
    }
    if value.type_of() == Type::BigInt {
        let value = bigint_from_primitive(ctx, &value)?;
        return Ok(if value.is_positive() {
            value
                .to_usize()
                .unwrap_or(MAX_DECIMAL_DIGITS)
                .min(MAX_DECIMAL_DIGITS)
        } else {
            MAX_DECIMAL_DIGITS
        });
    }
    let number = Coerced::<f64>::from_js(ctx, value)?.0;
    if number.is_finite() && number > 0.0 && number.fract() == 0.0 {
        return Ok((number as usize).min(MAX_DECIMAL_DIGITS));
    }
    // A truthy negative, non-integral, or infinite loop counter never reaches
    // JavaScript's numeric zero.  Terminating fractions still finish when the
    // remainder does; recurring values hit this explicit safety ceiling.
    Ok(MAX_DECIMAL_DIGITS)
}

pub(crate) fn to_decimal<'js>(
    ctx: &Ctx<'js>,
    this: &Parts,
    args: &[Value<'js>],
) -> rquickjs::Result<String> {
    if this.d.is_zero() {
        return Err(invalid_bigint_operation(ctx));
    }
    let mut numerator = this.n.clone();
    let denominator = this.d.clone();
    let digits = decimal_limit(ctx, args)?;
    let raw_repeating = cycle_len(denominator.clone());
    let repeating = if raw_repeating == 0 {
        BigInt::zero()
    } else {
        coerce_bigint(ctx, raw_repeating.into_js(ctx)?)?
    };
    let offset = cycle_start(ctx, &denominator, &repeating)?;
    let mut output = if this.s.is_negative() {
        "-".to_owned()
    } else {
        String::new()
    };
    output.push_str(&(numerator.clone() / &denominator).to_string());
    numerator %= &denominator;
    numerator *= 10u8;
    if !numerator.is_zero() {
        output.push('.');
    }
    if !repeating.is_zero() {
        let offset = bounded_loop_count(ctx, &offset, MAX_DECIMAL_DIGITS, "decimal formatting")?;
        let repeating =
            bounded_loop_count(ctx, &repeating, MAX_DECIMAL_DIGITS, "decimal formatting")?;
        check_loop_work(
            ctx,
            offset.saturating_add(repeating),
            denominator.magnitude().bits(),
            MAX_DECIMAL_WORK_BITS,
            "decimal formatting",
        )?;
        for _ in 0..offset {
            output.push_str(&(numerator.clone() / &denominator).to_string());
            numerator %= &denominator;
            numerator *= 10u8;
        }
        output.push('(');
        for _ in 0..repeating {
            output.push_str(&(numerator.clone() / &denominator).to_string());
            numerator %= &denominator;
            numerator *= 10u8;
        }
        output.push(')');
    } else {
        check_loop_work(
            ctx,
            digits,
            denominator.magnitude().bits(),
            MAX_DECIMAL_WORK_BITS,
            "decimal formatting",
        )?;
        for _ in 0..digits {
            if numerator.is_zero() {
                break;
            }
            output.push_str(&(numerator.clone() / &denominator).to_string());
            numerator %= &denominator;
            numerator *= 10u8;
        }
    }
    Ok(output)
}

pub(crate) fn continued<'js>(ctx: &Ctx<'js>, this: &Parts) -> rquickjs::Result<Vec<BigInt>> {
    let mut numerator = this.n.clone();
    let mut denominator = this.d.clone();
    let mut result = Vec::new();
    let mut work_bits = 0u64;
    loop {
        if denominator.is_zero() {
            return Err(invalid_bigint_operation(ctx));
        }
        let iteration_bits = numerator
            .magnitude()
            .bits()
            .max(denominator.magnitude().bits())
            .max(1);
        if work_bits.saturating_add(iteration_bits) > MAX_CONTINUED_WORK_BITS {
            return Err(native_limit(ctx, "continued fraction"));
        }
        work_bits += iteration_bits;
        result.push(&numerator / &denominator);
        let remainder = &numerator % &denominator;
        numerator = denominator;
        denominator = remainder;
        if numerator.is_one() {
            break;
        }
        if result.len() >= MAX_CONTINUED_LEN {
            return Err(native_limit(ctx, "continued fraction"));
        }
    }
    Ok(result)
}

pub(crate) fn into_value<'js, T: IntoJs<'js>>(
    ctx: &Ctx<'js>,
    value: T,
) -> rquickjs::Result<Value<'js>> {
    value.into_js(ctx)
}

pub(crate) fn binary_parts<'js>(
    ctx: &Ctx<'js>,
    this: &Object<'js>,
    args: &[Value<'js>],
) -> rquickjs::Result<(Parts, Parts)> {
    // fraction.js parses/coerces the argument before it reads the receiver.
    // That order matters when a valueOf/getter mutates writable s/n/d fields.
    let right = argument(ctx, args)?;
    let left = read_parts(ctx, this)?;
    Ok((left, right))
}
