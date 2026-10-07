//! The array guard: every read of a JavaScript array's length, and every
//! host reservation sized by one, goes through these helpers.
//!
//! A JavaScript `length` is a claim a score controls, not backed storage (see
//! [`js_array_len`] for why rquickjs' own length reads cannot be trusted with
//! one, and [`reserve_js_elements`] for why the claim is charged before any
//! host memory is reserved).

use super::*;

/// Element-slot budget derived from the live QuickJS heap ceiling.
///
/// The divisor is the size of the host element type, so the requested slots
/// fit within that byte limit. This does not account for allocations owned
/// by each element, other buffers, or extra capacity the allocator provides.
pub(crate) fn js_element_cap<T>(ctx: &Ctx<'_>) -> rquickjs::Result<usize> {
    Ok(host_heap_ceiling(ctx)? / std::mem::size_of::<T>())
}

/// The `length` of a JavaScript array, read without asserting that it fits an
/// `i32`.
///
/// rquickjs' `Array::len` asserts an int, but QuickJS stores any length past
/// `i32::MAX` as a float64 - `new Array(2 ** 31)`, `a.length = 3e9` - so
/// `len()`, and `iter()` which calls it, PANIC on such an array before any
/// guard can refuse it. An array's `length` is always an own uint32 data
/// property, so reading it as an `f64` is exact and runs no JavaScript. Every
/// guarded site reads the length here first; once [`reserve_js_elements`] has
/// accepted it, it is bounded by the heap ceiling (which only ever lowers from
/// [`DEFAULT_JS_MEMORY_LIMIT`]) - far below `2^31` - so a `len()`/`iter()`
/// that follows it cannot panic.
///
/// This is the one statement of the rule; call sites point here. It covers
/// every array a score can reach, including copies the HOST fills: rquickjs'
/// `Array::set` honours an indexed setter a score installed on
/// `Array.prototype`, and that setter can re-length the copy past
/// `i32::MAX`. Read the length once and walk exactly that many indices, so a
/// getter that re-lengthens the array mid-walk cannot move the bound.
pub(crate) fn js_array_len(array: &rquickjs::Array<'_>) -> rquickjs::Result<usize> {
    let length: f64 = array.as_object().get("length")?;
    // Saturating, and exact for every uint32.
    Ok(length as usize)
}

/// Reserve host slots for `len` JavaScript elements, charging the
/// materialization's remaining element budget, and refuse safely rather than
/// abort the process.
///
/// A JavaScript `length` is a claim, not backed storage: `new Array(1e9)` is
/// sparse and nearly free in QuickJS, so a `Vec::with_capacity` fed that
/// length eagerly reserves tens of gigabytes OUTSIDE the heap budget - which
/// only sees QuickJS's allocator - and the failed allocation becomes
/// `handle_alloc_error`: an `abort()` that takes down the whole host. So the
/// claimed length is checked against `remaining` first (a budget shared by
/// one materialization's entire recursion, so a tree of individually modest
/// sparse arrays cannot sum past it either), and the reservation itself goes
/// through `try_reserve_exact`, so even a SYSTEM refusal comes back as a
/// catchable JavaScript exception instead of an abort.
pub(crate) fn reserve_js_elements<T>(
    ctx: &Ctx<'_>,
    len: usize,
    remaining: &Cell<usize>,
) -> rquickjs::Result<Vec<T>> {
    charge_js_elements(ctx, len, remaining)?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(len)
        .map_err(|_| host_reservation_refused(ctx, len))?;
    Ok(values)
}

/// The one-shot form of [`reserve_js_elements`], for a call site that mirrors
/// a single JS array rather than recursing through a value tree: the budget
/// is the full [`js_element_cap`] and nothing else draws on it.
pub(crate) fn reserve_js_array<T>(ctx: &Ctx<'_>, len: usize) -> rquickjs::Result<Vec<T>> {
    let remaining = Cell::new(js_element_cap::<T>(ctx)?);
    reserve_js_elements(ctx, len, &remaining)
}

/// [`reserve_js_elements`] without the reservation: the claim is charged and
/// refused with the same RangeError, but nothing is allocated. For a walk
/// that visits every claimed element yet keeps few or none of them - a
/// filter, a dedup, a mean - so its host Vec grows only with what it keeps.
pub(crate) fn charge_js_elements(
    ctx: &Ctx<'_>,
    len: usize,
    remaining: &Cell<usize>,
) -> rquickjs::Result<()> {
    let left = remaining.get();
    if try_charge_js_elements(len, remaining) {
        return Ok(());
    }
    Err(rquickjs::Exception::throw_range(
        ctx,
        &format!(
            "cannot materialize {len} JavaScript element(s): the \
             host reservation exceeds the remaining {left}-element budget \
             derived from the QuickJS heap ceiling"
        ),
    ))
}

/// The one-shot form of [`charge_js_elements`], budgeted in `T`s.
pub(crate) fn charge_js_array<T>(ctx: &Ctx<'_>, len: usize) -> rquickjs::Result<()> {
    let remaining = Cell::new(js_element_cap::<T>(ctx)?);
    charge_js_elements(ctx, len, &remaining)
}

/// [`charge_js_elements`] for a walk that cannot throw - a converter, a
/// fail-closed predicate. `false` means the claim exceeds what is left; the
/// caller refuses without leaving a pending exception behind.
pub(crate) fn try_charge_js_elements(len: usize, remaining: &Cell<usize>) -> bool {
    let left = remaining.get();
    if len > left {
        return false;
    }
    remaining.set(left - len);
    true
}

/// The elements one control name of a query State charges: one for an
/// array-index key, none for a named key. Typed arrays and String wrappers
/// enumerate an index key per element of compact backing storage, so those
/// keys draw the budget an array's length does; a named key already costs
/// the JS heap. The recursive value walkers charge one element for each
/// property instead.
pub(crate) fn js_key_elements(key: &str) -> usize {
    usize::from(key.parse::<u32>().is_ok())
}

/// The RangeError for a charged claim the system allocator still refused.
pub(crate) fn host_reservation_refused(ctx: &Ctx<'_>, len: usize) -> rquickjs::Error {
    rquickjs::Exception::throw_range(
        ctx,
        &format!("the host could not reserve memory for {len} materialized values"),
    )
}

/// Copy a JavaScript array into a host Vec through the guard: the length is
/// read once with [`js_array_len`] and charged with [`reserve_js_array`]
/// before the Vec exists, then exactly that many indices are read, so a
/// getter that re-lengthens the array mid-walk cannot push past the
/// reservation.
pub(crate) fn js_array_values<'js, T: FromJs<'js>>(
    ctx: &Ctx<'js>,
    array: &rquickjs::Array<'js>,
) -> rquickjs::Result<Vec<T>> {
    let remaining = Cell::new(js_element_cap::<T>(ctx)?);
    js_array_values_within(ctx, array, &remaining)
}

/// [`js_array_values`] charging a caller-owned budget, seeded from
/// `js_element_cap::<T>`, for a caller that copies a whole tree of arrays
/// and keeps them alive together: each copy alone fitting the cap must not
/// let the tree sum past it.
pub(crate) fn js_array_values_within<'js, T: FromJs<'js>>(
    ctx: &Ctx<'js>,
    array: &rquickjs::Array<'js>,
    remaining: &Cell<usize>,
) -> rquickjs::Result<Vec<T>> {
    let len = js_array_len(array)?;
    let mut values = reserve_js_elements::<T>(ctx, len, remaining)?;
    for index in 0..len {
        values.push(array.get(index)?);
    }
    Ok(values)
}
