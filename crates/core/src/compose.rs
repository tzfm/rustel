/*
compose.rs - the COMPOSERS × ALIGNMENTS operator matrix
Composition operators adapted from Strudel packages/core/pattern.mjs.
Copyright (C) 2025 Strudel contributors

Rust implementation and additions:
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! ~24 operators × 8 alignments of pattern methods, generated from two
//! tables: 190-odd near-duplicate methods written out by hand would drift,
//! the two tables are a page.
//!
//! Two behaviours are easy to lose in the generation:
//!
//! * `keepif` bypasses the compose wrapper entirely and then strips
//!   undefineds - it must *discard* `b`'s value rather than union it.
//!   `struct` and `mask` are `keepif` under different alignments.
//! * every other operator goes through [`compose_op`], which promotes a bare
//!   value to `{ value: … }` as soon as **either** side is a control object.

use crate::ops::PatOps;
use crate::util;
use crate::value::{OrderedMap, Value};
use std::sync::Arc;

/// The eight alignments an operator can compose under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alignment {
    In,
    Out,
    Mix,
    Squeeze,
    SqueezeOut,
    Reset,
    Restart,
    /// The eighth alignment, and the easiest to forget: without it the
    /// matrix is 24x7 and `.poly` simply does not exist.
    Poly,
}

/// The default alignment, settable by `setDefaultJoin`. The runtime resets
/// it to `in` at the start of every score evaluation.
pub fn default_alignment() -> Alignment {
    Alignment::ALL[crate::settings::default_join() as usize % Alignment::ALL.len()]
}

pub fn set_default_alignment(alignment: Alignment) {
    let index = Alignment::ALL
        .iter()
        .position(|candidate| *candidate == alignment)
        .unwrap_or(0);
    crate::settings::set_default_join(index as u8);
}

impl Alignment {
    /// The lowercase method suffix (`add.squeezeout`); `squeezein` aliases
    /// `squeeze`.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "in" => Alignment::In,
            "out" => Alignment::Out,
            "mix" => Alignment::Mix,
            "squeeze" | "squeezein" => Alignment::Squeeze,
            "squeezeout" => Alignment::SqueezeOut,
            "reset" => Alignment::Reset,
            "restart" => Alignment::Restart,
            "poly" => Alignment::Poly,
            _ => return None,
        })
    }

    pub const ALL: &'static [Alignment] = &[
        Alignment::In,
        Alignment::Out,
        Alignment::Mix,
        Alignment::Squeeze,
        Alignment::SqueezeOut,
        Alignment::Reset,
        Alignment::Restart,
        Alignment::Poly,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Alignment::In => "in",
            Alignment::Out => "out",
            Alignment::Mix => "mix",
            Alignment::Squeeze => "squeeze",
            Alignment::SqueezeOut => "squeezeout",
            Alignment::Reset => "reset",
            Alignment::Restart => "restart",
            Alignment::Poly => "poly",
        }
    }
}

/// One entry of the COMPOSERS table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, std::hash::Hash)]
pub enum ComposeOp {
    /// The right operand wins.
    Set,
    /// The left operand wins.
    Keep,
    /// The left operand where the right is truthy, else undefined.
    KeepIf,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Band,
    Bor,
    Bxor,
    Blshift,
    Brshift,
    Lt,
    Gt,
    Lte,
    Gte,
    Eq,
    Eqt,
    Ne,
    Net,
    And,
    Or,
}

impl ComposeOp {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "set" => ComposeOp::Set,
            "keep" => ComposeOp::Keep,
            "keepif" => ComposeOp::KeepIf,
            "add" => ComposeOp::Add,
            "sub" => ComposeOp::Sub,
            "mul" => ComposeOp::Mul,
            "div" => ComposeOp::Div,
            "mod" => ComposeOp::Mod,
            "pow" => ComposeOp::Pow,
            "band" => ComposeOp::Band,
            "bor" => ComposeOp::Bor,
            "bxor" => ComposeOp::Bxor,
            "blshift" => ComposeOp::Blshift,
            "brshift" => ComposeOp::Brshift,
            "lt" => ComposeOp::Lt,
            "gt" => ComposeOp::Gt,
            "lte" => ComposeOp::Lte,
            "gte" => ComposeOp::Gte,
            "eq" => ComposeOp::Eq,
            "eqt" => ComposeOp::Eqt,
            "ne" => ComposeOp::Ne,
            "net" => ComposeOp::Net,
            "and" => ComposeOp::And,
            "or" => ComposeOp::Or,
            _ => return None,
        })
    }

    pub const ALL: &'static [ComposeOp] = &[
        ComposeOp::Set,
        ComposeOp::Keep,
        ComposeOp::KeepIf,
        ComposeOp::Add,
        ComposeOp::Sub,
        ComposeOp::Mul,
        ComposeOp::Div,
        ComposeOp::Mod,
        ComposeOp::Pow,
        ComposeOp::Band,
        ComposeOp::Bor,
        ComposeOp::Bxor,
        ComposeOp::Blshift,
        ComposeOp::Brshift,
        ComposeOp::Lt,
        ComposeOp::Gt,
        ComposeOp::Lte,
        ComposeOp::Gte,
        ComposeOp::Eq,
        ComposeOp::Eqt,
        ComposeOp::Ne,
        ComposeOp::Net,
        ComposeOp::And,
        ComposeOp::Or,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ComposeOp::Set => "set",
            ComposeOp::Keep => "keep",
            ComposeOp::KeepIf => "keepif",
            ComposeOp::Add => "add",
            ComposeOp::Sub => "sub",
            ComposeOp::Mul => "mul",
            ComposeOp::Div => "div",
            ComposeOp::Mod => "mod",
            ComposeOp::Pow => "pow",
            ComposeOp::Band => "band",
            ComposeOp::Bor => "bor",
            ComposeOp::Bxor => "bxor",
            ComposeOp::Blshift => "blshift",
            ComposeOp::Brshift => "brshift",
            ComposeOp::Lt => "lt",
            ComposeOp::Gt => "gt",
            ComposeOp::Lte => "lte",
            ComposeOp::Gte => "gte",
            ComposeOp::Eq => "eq",
            ComposeOp::Eqt => "eqt",
            ComposeOp::Ne => "ne",
            ComposeOp::Net => "net",
            ComposeOp::And => "and",
            ComposeOp::Or => "or",
        }
    }

    /// The scalar body, applied to two **non-object** values.
    ///
    /// The arithmetic ops coerce both sides through `parse_numeral` first -
    /// `"3".add("4")` is 7, not `"34"`.
    pub fn apply_scalar(self, a: &Value, b: &Value) -> Value {
        use ComposeOp::*;
        match self {
            Set => b.clone(),
            Keep => a.clone(),
            KeepIf => {
                if b.js_truthy() {
                    a.clone()
                } else {
                    Value::Undefined
                }
            }
            Add | Sub | Mul | Div | Mod | Pow | Band | Bor | Bxor | Blshift | Brshift => {
                let (x, y) = match (util::parse_numeral(a), util::parse_numeral(b)) {
                    (Ok(x), Ok(y)) => (x, y),
                    // A value that is neither numeric nor a note name
                    // aborts the whole query (no haps at all) - it must not
                    // quietly become NaN.
                    _ => {
                        let (a, b) = (a.show(), b.show());
                        crate::signal_query_error(move || {
                            format!("cannot parse as numeral: \"{a}\" or \"{b}\"")
                        });
                        (f64::NAN, f64::NAN)
                    }
                };
                Value::F64(match self {
                    Add => x + y,
                    Sub => x - y,
                    Mul => x * y,
                    Div => x / y,
                    // Floor-mod, not the truncating remainder.
                    Mod => util::modulo_f64(x, y),
                    Pow => x.powf(y),
                    Band => f64::from(to_int32(x) & to_int32(y)),
                    Bor => f64::from(to_int32(x) | to_int32(y)),
                    Bxor => f64::from(to_int32(x) ^ to_int32(y)),
                    Blshift => f64::from(to_int32(x) << (to_uint32(y) & 31)),
                    Brshift => f64::from(to_int32(x) >> (to_uint32(y) & 31)),
                    _ => unreachable!(),
                })
            }
            // Relational comparison is tri-state: NaN on either side makes
            // all four operators false. Defining `>=` as `!(<)` is therefore
            // wrong: `2 >= "bd"` is false, not true.
            Lt => Value::Bool(js_less_than(a, b) == Some(true)),
            Gt => Value::Bool(js_less_than(b, a) == Some(true)),
            Lte => Value::Bool(js_less_than(b, a) == Some(false)),
            Gte => Value::Bool(js_less_than(a, b) == Some(false)),
            Eq => Value::Bool(js_loose_eq(a, b)),
            Eqt => Value::Bool(js_strict_eq(a, b)),
            Ne => Value::Bool(!js_loose_eq(a, b)),
            Net => Value::Bool(!js_strict_eq(a, b)),
            // `a && b` / `a || b` return an OPERAND, not a boolean.
            And => {
                if a.js_truthy() {
                    b.clone()
                } else {
                    a.clone()
                }
            }
            Or => {
                if a.js_truthy() {
                    a.clone()
                } else {
                    b.clone()
                }
            }
        }
    }
}

/// A plain control object - not an array, not a Fraction.
fn non_array_object(v: &Value) -> bool {
    matches!(v, Value::Object(_))
}

/// Composes two values: when either side is a control object both are
/// promoted to objects and unioned key-wise; otherwise the scalar op runs.
pub fn compose_op(op: ComposeOp, a: &Value, b: &Value) -> Value {
    // Materialisation recursively clones ordinary lists/control maps as a
    // side effect of looking for host-owned identities. Native composition is
    // the overwhelmingly common path, and `contains_js_value` already proves
    // when that work is unnecessary.
    let materialized_a = a
        .contains_js_value()
        .then(|| crate::materialize_js_value(a));
    let materialized_b = b
        .contains_js_value()
        .then(|| crate::materialize_js_value(b));
    let a = materialized_a.as_ref().unwrap_or(a);
    let b = materialized_b.as_ref().unwrap_or(b);
    if non_array_object(a) || non_array_object(b) {
        let a_obj = as_object(a);
        let b_obj = as_object(b);
        union_with_obj(op, &a_obj, &b_obj)
    } else {
        op.apply_scalar(a, b)
    }
}

fn as_object(v: &Value) -> OrderedMap {
    match v {
        Value::Object(o) => o.clone(),
        other => OrderedMap::from_entries([("value".to_string(), other.clone())]),
    }
}

/// Key-wise union: `a`'s key order, then `b`'s new keys, common keys
/// combined. A `b` that is only `{value: …}` bails out and returns `a`
/// unchanged ("can't do arithmetic on control pattern"). The bail-out is
/// not a nicety: `note("c").add(1)` promotes `1` to `{value: 1}`, so the
/// addition is **dropped** rather than creating a stray `value` key; the
/// example snapshots depend on it.
fn union_with_obj(op: ComposeOp, a: &OrderedMap, b: &OrderedMap) -> Value {
    if union_drops(b) {
        return Value::Object(a.clone());
    }
    let mut out = a.clone();
    union_into(op, &mut out, b);
    Value::Object(out)
}

/// Whether a union with `b` keeps the other side unchanged: `b` is only
/// `{value: …}`. See [`union_with_obj`].
pub(crate) fn union_drops(b: &OrderedMap) -> bool {
    b.len() == 1
        && b.get("value")
            .is_some_and(|v| !matches!(v, Value::Undefined))
}

/// `b`'s keys added to `out`: common keys replaced where they are by the
/// combined value, then `b`'s new keys in `b`'s order.
fn union_into(op: ComposeOp, out: &mut OrderedMap, b: &OrderedMap) {
    let entries = b.entries();
    if entries.is_empty() {
        return;
    }
    // One hold of the entries for every key of `b`, not one per key: each
    // hold pays for the map's copy and its shared-count bookkeeping.
    let out = out.entries_mut(entries.len());
    for (key, b_value) in entries {
        match out.iter_mut().find(|(name, _)| name == key) {
            Some((_, current)) => *current = op.apply_scalar(current, b_value),
            None => out.push((key.clone(), b_value.clone())),
        }
    }
}

/// [`compose_op`] of two data values, which hold no script object, made
/// where `a` is. A union of two maps adds `b`'s keys to `a`'s map, which it
/// copies only when it is shared. Data made of data is data, so the union's
/// map is marked as holding data only, and the next compose of it does not
/// walk its entries to learn that again.
pub(crate) fn compose_data_in_place(op: ComposeOp, a: &mut Value, b: &Value) {
    debug_assert!(a.is_data(0) && b.is_data(0));
    if let (Value::Object(map), Value::Object(other)) = (&mut *a, b)
        && !union_drops(other)
    {
        union_into(op, map, other);
        map.mark_made_of_data();
        return;
    }
    *a = compose_op(op, a, b);
}

/// One cell of the generated matrix: `pat.<op>.<how>(other)`.
///
/// No composer declares a preprocess step, so none is modelled.
pub fn compose<P: PatOps>(pat: &P, other: &P, op: ComposeOp, how: Alignment) -> P {
    // The squeeze/reset/restart alignments go through a bind, and `Pattern`'s
    // bind must use the opaque dynamic constructor. Re-entering the pure
    // instantiation when both operands are pure is what keeps `struct`,
    // `mask` and `add.squeeze` on the tight-lookahead path.
    match (pat.as_pure_view(), other.as_pure_view()) {
        (Some(a), Some(b)) => P::from_pure_view(compose_inner(&a, &b, op, how)),
        _ => compose_inner(pat, other, op, how),
    }
}

fn compose_inner<P: PatOps>(pat: &P, other: &P, op: ComposeOp, how: Alignment) -> P {
    let lookup_flow = match op {
        ComposeOp::Set => crate::LookupFlow::Set,
        ComposeOp::Keep | ComposeOp::KeepIf => crate::LookupFlow::Left,
        ComposeOp::Add => crate::LookupFlow::Add,
        _ => crate::LookupFlow::Infer,
    };
    // `keepif` bypasses `_composeOp` so that `b`'s value is discarded rather
    // than unioned, then strips the undefineds the falsy branch produced.
    if op == ComposeOp::KeepIf {
        apply_alignment(pat, other, how, interned_combine(op, true), lookup_flow)
            .remove_undefineds()
    } else {
        // A direct alignment names its op, so that a precomputed table can
        // run it on the structure hap's value where that value is.
        match how {
            Alignment::In => pat.app_left_composed(other.clone(), op, lookup_flow),
            Alignment::Out => pat.app_right_composed(other.clone(), op, lookup_flow),
            Alignment::Mix => pat.app_both_composed(other.clone(), op, lookup_flow),
            _ => apply_alignment(pat, other, how, interned_combine(op, false), lookup_flow),
        }
    }
}

/// The closure of each compose op that a pattern was built with. See
/// [`interned_combine`].
static OP_CLOSURES: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<(ComposeOp, bool), crate::CombineFn>>,
> = std::sync::OnceLock::new();

/// Whether `combine` is the closure of a compose op: a function of its two
/// values and nothing more, which a fold may run when a pattern is built.
pub(crate) fn is_op_closure(combine: &crate::CombineFn) -> bool {
    OP_CLOSURES.get().is_some_and(|closures| {
        closures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .any(|closure| Arc::ptr_eq(closure, combine))
    })
}

/// The combine closure one compose op builds, interned: the same op built
/// by a later evaluation is the same closure object, so that node agrees
/// with its counterpart by pointer and the answers of the unchanged
/// subtree carry across evaluations and edits. `scalar_only` is the
/// `keepif` body, which discards `b`'s value rather than unioning it.
/// See [`crate::interned_closure`].
fn interned_combine(op: ComposeOp, scalar_only: bool) -> crate::CombineFn {
    crate::interned_closure(
        OP_CLOSURES.get_or_init(Default::default),
        (op, scalar_only),
        || {
            let combine: crate::CombineFn = Arc::new(move |a: &Value, b: &Value| {
                if scalar_only {
                    op.apply_scalar(a, b)
                } else {
                    compose_op(op, a, b)
                }
            });
            combine
        },
    )
}

/// The `_op*` family. `combine(a, b)` receives the LEFT pattern's value first,
/// whichever way round the alignment queries the two patterns.
///
/// Generic over [`PatOps`] so the squeeze/reset/restart alignments reach the
/// **pure** bind constructor when both operands are pure. Written concretely
/// against `Pattern` they would have to use the opaque dynamic constructor,
/// which would classify every `struct`/`mask`/`add.squeeze` as impure.
fn apply_alignment<P: PatOps>(
    pat: &P,
    other: &P,
    how: Alignment,
    combine: crate::CombineFn,
    lookup_flow: crate::LookupFlow,
) -> P {
    match how {
        Alignment::In => pat.app_left_interned(other.clone(), combine, lookup_flow),
        Alignment::Out => pat.app_right_interned(other.clone(), combine, lookup_flow),
        Alignment::Mix => pat.app_both_interned(other.clone(), combine, lookup_flow),

        Alignment::Squeeze => {
            let other = other.clone();
            pat.squeeze_bind(move |a| {
                let a = a.clone();
                let combine = combine.clone();
                other.fmap(move |b| combine(&a, b))
            })
        }

        // The OUTER pattern is `other` here, and the argument order flips.
        Alignment::SqueezeOut => {
            let this = pat.clone();
            other.squeeze_bind(move |a| {
                let a = a.clone();
                let combine = combine.clone();
                this.fmap(move |b| combine(b, &a))
            })
        }

        // The OUTER pattern is `this`, unlike reset/restart - the step
        // count that scales each inner pattern comes from the left.
        Alignment::Poly => {
            let other = other.clone();
            pat.poly_bind(move |b| {
                let b = b.clone();
                let combine = combine.clone();
                other.fmap(move |a| combine(a, &b))
            })
        }

        // `other` is the outer pattern for both.
        Alignment::Reset | Alignment::Restart => {
            let this = pat.clone();
            let inner = move |b: &Value| {
                let b = b.clone();
                let combine = combine.clone();
                this.fmap(move |a| combine(a, &b))
            };
            if how == Alignment::Reset {
                other.reset_bind(inner)
            } else {
                other.restart_bind(inner)
            }
        }
    }
}

// -- JavaScript comparison semantics ----------------------------------------

/// `ToInt32` - what `&`, `|`, `^`, `<<`, `>>` coerce their operands with.
pub(crate) fn to_int32(x: f64) -> i32 {
    if !x.is_finite() {
        return 0;
    }
    let truncated = x.trunc();
    let modulo = truncated.rem_euclid(4_294_967_296.0);
    if modulo >= 2_147_483_648.0 {
        (modulo - 4_294_967_296.0) as i32
    } else {
        modulo as i32
    }
}

fn to_uint32(x: f64) -> u32 {
    to_int32(x) as u32
}

/// JS's Abstract Relational Comparison `a < b`.
///
/// String-string compares by UTF-16 code unit, so an astral code point
/// orders by its surrogate pair, under the characters above the BMP that
/// Rust's scalar ordering places above it; anything else is coerced to a
/// number, and `None` models the spec's `undefined` result when either side
/// is NaN. All four relational operators treat `None` as `false`.
fn js_less_than(a: &Value, b: &Value) -> Option<bool> {
    if let (Value::Str(x), Value::Str(y)) = (a, b) {
        return Some(utf16_cmp(x, y) == std::cmp::Ordering::Less);
    }
    let (x, y) = (js_to_number(a), js_to_number(b));
    if x.is_nan() || y.is_nan() {
        return None;
    }
    Some(x < y)
}

/// Compare two strings by UTF-16 code unit, as JavaScript does.
fn utf16_cmp(x: &str, y: &str) -> std::cmp::Ordering {
    let mut x = x.encode_utf16();
    let mut y = y.encode_utf16();
    loop {
        match (x.next(), y.next()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(unit_x), Some(unit_y)) => {
                if unit_x != unit_y {
                    return unit_x.cmp(&unit_y);
                }
            }
        }
    }
}

/// JS `==` restricted to the value shapes patterns actually carry.
pub(crate) fn js_loose_eq(a: &Value, b: &Value) -> bool {
    use Value::*;
    match (a, b) {
        (Undefined | Null, Undefined | Null) => true,
        (Undefined | Null, _) | (_, Undefined | Null) => false,
        (Str(x), Str(y)) => x == y,
        (Object(_) | List(_), Object(_) | List(_)) => js_strict_eq(a, b),
        _ => {
            let (x, y) = (js_to_number(a), js_to_number(b));
            !x.is_nan() && !y.is_nan() && x == y
        }
    }
}

/// JS `===`, except objects and arrays compare structurally: patterns copy
/// values freely, so identity would be unstable after a copy.
fn js_strict_eq(a: &Value, b: &Value) -> bool {
    use Value::*;
    match (a, b) {
        (Undefined, Undefined) | (Null, Null) => true,
        (Bool(x), Bool(y)) => x == y,
        (F64(x), F64(y)) => x == y,
        (Str(x), Str(y)) => x == y,
        (List(x), List(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| js_strict_eq(p, q))
        }
        (Object(x), Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| js_strict_eq(v, w)))
        }
        _ => false,
    }
}

/// `Number(v)`.
fn js_to_number(v: &Value) -> f64 {
    match v {
        Value::Undefined => f64::NAN,
        Value::Null => 0.0,
        Value::Bool(b) => f64::from(*b),
        Value::F64(x) => *x,
        Value::Str(s) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse::<f64>().unwrap_or(f64::NAN)
            }
        }
        Value::List(items) if items.is_empty() => 0.0,
        Value::List(items) if items.len() == 1 => js_to_number(&items[0]),
        Value::List(_)
        | Value::Object(_)
        | Value::Function(_)
        | Value::Pattern(_)
        | Value::Haps(_)
        | Value::JsValue(_) => f64::NAN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic_on_a_control_pattern_is_dropped() {
        // note("c").add(1): 1 promotes to {value: 1}, which trips the
        // union bail-out, so the left operand survives unchanged.
        let left = Value::object([("note".into(), Value::Str("c".into()))]);
        assert_eq!(compose_op(ComposeOp::Add, &left, &Value::F64(1.0)), left);
    }

    #[test]
    fn common_keys_are_combined_and_new_keys_appended() {
        let left = Value::object([
            ("gain".into(), Value::F64(1.0)),
            ("s".into(), Value::Str("bd".into())),
        ]);
        let right = Value::object([
            ("gain".into(), Value::F64(2.0)),
            ("n".into(), Value::F64(3.0)),
        ]);
        assert_eq!(
            compose_op(ComposeOp::Add, &left, &right),
            Value::object([
                ("gain".into(), Value::F64(3.0)),
                ("s".into(), Value::Str("bd".into())),
                ("n".into(), Value::F64(3.0)),
            ])
        );
    }

    #[test]
    fn numeric_strings_add_numerically() {
        assert_eq!(
            ComposeOp::Add.apply_scalar(&Value::Str("3".into()), &Value::Str("4".into())),
            Value::F64(7.0)
        );
    }

    #[test]
    fn relational_string_comparison_uses_utf16_order() {
        // An astral code point is the surrogate pair D800 DC00 in UTF-16,
        // which sorts under U+E000; Rust's scalar ordering says otherwise.
        let astral = Value::Str("\u{10000}".into());
        let above_bmp = Value::Str("\u{E000}".into());
        assert_eq!(
            ComposeOp::Lt.apply_scalar(&astral, &above_bmp),
            Value::Bool(true)
        );
        assert_eq!(
            ComposeOp::Gt.apply_scalar(&astral, &above_bmp),
            Value::Bool(false)
        );
        // Two astral code points order by their surrogate pairs, and an
        // ordinary pair keeps the order both systems agree on.
        let next_astral = Value::Str("\u{10001}".into());
        assert_eq!(
            ComposeOp::Lt.apply_scalar(&astral, &next_astral),
            Value::Bool(true)
        );
        assert_eq!(
            ComposeOp::Lte.apply_scalar(&Value::Str("a".into()), &Value::Str("b".into())),
            Value::Bool(true)
        );
    }

    #[test]
    fn logical_ops_return_an_operand() {
        assert_eq!(
            ComposeOp::Or.apply_scalar(&Value::F64(0.0), &Value::Str("x".into())),
            Value::Str("x".into())
        );
    }
}
