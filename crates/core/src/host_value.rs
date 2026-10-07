/*
host_value.rs - One number a host writes and a score reads at query time
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! A reading the host owns, such as a pointer position, handed to a score as
//! a pattern.
//!
//! The host writes from whichever thread sees the input and queries read on
//! the thread that plays, so the number is an atomic: a write never blocks a
//! query, a query never blocks a write, and neither allocates.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{Pattern, Value};

/// One number a host writes and patterns read when they are queried.
///
/// Clones share the number. It reads 0 until the host sets it.
#[derive(Clone, Default)]
pub struct HostValue(Arc<Cell>);

#[derive(Default)]
struct Cell {
    /// The number's `f64` bits.
    bits: AtomicU64,
    /// How many stores changed `bits`.
    changes: AtomicU64,
}

impl HostValue {
    /// Store `value`; a non-finite value stores 0.
    pub fn set(&self, value: f64) {
        let value = if value.is_finite() { value } else { 0.0 };
        let bits = value.to_bits();
        if self.0.bits.swap(bits, Ordering::Relaxed) != bits {
            self.0.changes.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The number last stored.
    pub fn get(&self) -> f64 {
        f64::from_bits(self.0.bits.load(Ordering::Relaxed))
    }

    /// How many stores changed the number. A host that requeries when this
    /// moves hears a change before its lookahead runs out.
    pub fn changes(&self) -> u64 {
        self.0.changes.load(Ordering::Relaxed)
    }

    /// A continuous signal of the number, sampled at each query's begin like
    /// `saw`. Volatile, so no result cache keeps what it answered.
    pub fn signal(&self) -> Pattern {
        let cell = self.clone();
        crate::signal(move |_, _| Value::F64(cell.get())).mark_volatile()
    }
}

impl fmt::Debug for HostValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostValue")
            .field("value", &self.get())
            .field("changes", &self.changes())
            .finish()
    }
}

/// A pointer's position as fractions of the host's surface, 0 at the left
/// and top.
#[derive(Clone, Debug, Default)]
pub struct Pointer {
    pub x: HostValue,
    pub y: HostValue,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustel_fraction::Fraction;

    /// The value the signal answers for the first cycle.
    fn first_value(pattern: &Pattern) -> Value {
        let haps = pattern.query_arc(Fraction::ZERO, Fraction::ONE);
        assert_eq!(haps.len(), 1, "one event per cycle");
        haps[0].value.clone()
    }

    #[test]
    fn a_value_nobody_set_reads_zero() {
        let value = HostValue::default();
        assert_eq!(value.get(), 0.0);
        assert_eq!(first_value(&value.signal()), Value::F64(0.0));
    }

    #[test]
    fn a_signal_built_before_a_set_reads_the_value_set_since() {
        let value = HostValue::default();
        let signal = value.signal();
        value.clone().set(0.25);
        assert_eq!(first_value(&signal), Value::F64(0.25));
        assert_eq!(first_value(&value.signal()), Value::F64(0.25));
    }

    #[test]
    fn a_non_finite_value_stores_zero() {
        let value = HostValue::default();
        for number in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            value.set(0.5);
            value.set(number);
            assert_eq!(value.get(), 0.0, "{number}");
        }
    }

    #[test]
    fn changes_count_only_stores_that_change_the_value() {
        let value = HostValue::default();
        value.set(0.0);
        assert_eq!(value.changes(), 0);
        value.set(0.5);
        value.set(0.5);
        assert_eq!(value.changes(), 1);
        value.set(0.75);
        value.set(f64::NAN);
        value.set(0.0);
        assert_eq!(value.changes(), 3);
    }

    #[test]
    fn the_signal_is_not_cacheable() {
        let value = HostValue::default();
        assert!(!value.signal().is_cacheable());
    }

    #[test]
    fn a_pointer_queries_as_the_position_set() {
        let pointer = Pointer::default();
        let (x, y) = (pointer.x.signal(), pointer.y.signal());
        pointer.x.set(0.25);
        pointer.y.set(0.75);
        assert_eq!(first_value(&x), Value::F64(0.25));
        assert_eq!(first_value(&y), Value::F64(0.75));
    }
}
