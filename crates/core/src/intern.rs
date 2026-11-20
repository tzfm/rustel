/*
intern.rs - per-evaluation structural interning for immutable pattern payloads
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! Thread-local structural interner scoped to a single native score evaluation.
//!
//! While [`with_eval_interner`] is active, immutable `Arc` payloads shared across
//! layers and tracks (`Arc<[Value]>`, `Arc<dyn BuiltinEntries>`, `Arc<Keys>`,
//! `Arc<Dress>`, `Arc<dyn CompactGridData>`, `Arc<Grid>`) deduplicate to one
//! allocation per structural value. When the guard drops at the end of
//! evaluation, all interner tables are freed immediately so zero memory is
//! retained between evaluations and no cross-thread locks are ever taken.

use std::cell::RefCell;
use std::collections::HashSet;
use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::sync::Arc;

use crate::context_keys::Keys;
use crate::sequence::{CompactGrid, Dress, Grid};
use crate::value::{BuiltinEntries, Value, WordHasher, hash_data};

type FastSet<T> = HashSet<T, BuildHasherDefault<WordHasher>>;

#[derive(Default)]
struct EvalInterner {
    values: FastSet<InternedValues>,
    builtins: FastSet<InternedBuiltin>,
    keys: FastSet<InternedKeys>,
    dresses: FastSet<InternedDress>,
    compact_grids: FastSet<InternedCompactGrid>,
    grids: FastSet<InternedGrid>,
}

thread_local! {
    static INTERNER: RefCell<Option<EvalInterner>> = const { RefCell::new(None) };
}

/// Run `f` with a fresh thread-local structural interner that is dropped as
/// soon as `f` finishes (or unwinds). Nested calls reuse the active interner.
pub fn with_eval_interner<R>(f: impl FnOnce() -> R) -> R {
    let installed = INTERNER.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = Some(EvalInterner::default());
            true
        } else {
            false
        }
    });
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            if self.0 {
                let _ = INTERNER.try_with(|cell| {
                    *cell.borrow_mut() = None;
                });
            }
        }
    }
    let _reset = Reset(installed);
    f()
}

// --- Arc<[Value]> ---

struct InternedValues(Arc<[Value]>);

impl Hash for InternedValues {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.len().hash(state);
        for v in self.0.iter() {
            hash_data(v, state);
        }
    }
}

impl PartialEq for InternedValues {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
            || (self.0.len() == other.0.len()
                && self
                    .0
                    .iter()
                    .zip(other.0.iter())
                    .all(|(a, b)| a.same_data(b)))
    }
}

impl Eq for InternedValues {}

pub(crate) fn intern_values(values: Arc<[Value]>) -> Arc<[Value]> {
    if values.is_empty() {
        return values;
    }
    INTERNER.with(|cell| {
        let Ok(mut slot) = cell.try_borrow_mut() else {
            return values;
        };
        let Some(interner) = slot.as_mut() else {
            return values;
        };
        let key = InternedValues(values);
        if let Some(existing) = interner.values.get(&key) {
            return Arc::clone(&existing.0);
        }
        let out = Arc::clone(&key.0);
        interner.values.insert(key);
        out
    })
}

// --- Arc<dyn BuiltinEntries> ---

struct InternedBuiltin(Arc<dyn BuiltinEntries>);

impl Hash for InternedBuiltin {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let (keys, values) = self.0.parts();
        keys.hash(state);
        for v in values {
            hash_data(v, state);
        }
    }
}

impl PartialEq for InternedBuiltin {
    fn eq(&self, other: &Self) -> bool {
        if Arc::ptr_eq(&self.0, &other.0) {
            return true;
        }
        let (ak, av) = self.0.parts();
        let (bk, bv) = other.0.parts();
        ak == bk && av.iter().zip(bv).all(|(a, b)| a.same_data(b))
    }
}

impl Eq for InternedBuiltin {}

pub(crate) fn intern_builtin(
    entries: Arc<dyn BuiltinEntries>,
    known_data: bool,
) -> Arc<dyn BuiltinEntries> {
    if !known_data && !entries.parts().1.iter().all(|v| v.is_data(0)) {
        return entries;
    }
    INTERNER.with(|cell| {
        let Ok(mut slot) = cell.try_borrow_mut() else {
            return entries;
        };
        let Some(interner) = slot.as_mut() else {
            return entries;
        };
        let key = InternedBuiltin(entries);
        if let Some(existing) = interner.builtins.get(&key) {
            return Arc::clone(&existing.0);
        }
        let out = Arc::clone(&key.0);
        interner.builtins.insert(key);
        out
    })
}

// --- Arc<Keys> ---

struct InternedKeys(Arc<Keys>);

impl Hash for InternedKeys {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.scale.as_deref().hash(state);
        self.0.edo_size.map(f64::to_bits).hash(state);
        self.0.tags.as_deref().hash(state);
        self.0.log_line.as_deref().hash(state);
        self.0.live_controls.hash(state);
        self.0.slider_binding.hash(state);
        self.0.ui_visuals.hash(state);
    }
}

impl PartialEq for InternedKeys {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
            || (self.0.scale.as_deref() == other.0.scale.as_deref()
                && self.0.edo_size.map(f64::to_bits) == other.0.edo_size.map(f64::to_bits)
                && self.0.tags.as_deref() == other.0.tags.as_deref()
                && self.0.log_line.as_deref() == other.0.log_line.as_deref()
                && self.0.live_controls == other.0.live_controls
                && self.0.slider_binding == other.0.slider_binding
                && self.0.ui_visuals == other.0.ui_visuals)
    }
}

impl Eq for InternedKeys {}

pub(crate) fn intern_context_keys(keys: Arc<Keys>) -> Arc<Keys> {
    if keys.scale_definition.is_some() || keys.pick_lookup.is_some() {
        return keys;
    }
    INTERNER.with(|cell| {
        let Ok(mut slot) = cell.try_borrow_mut() else {
            return keys;
        };
        let Some(interner) = slot.as_mut() else {
            return keys;
        };
        let key = InternedKeys(keys);
        if let Some(existing) = interner.keys.get(&key) {
            return Arc::clone(&existing.0);
        }
        let out = Arc::clone(&key.0);
        interner.keys.insert(key);
        out
    })
}

// --- Arc<Dress> ---

struct InternedDress(Arc<Dress>);

impl Hash for InternedDress {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash_intern(state);
    }
}

impl PartialEq for InternedDress {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0.eq_intern(&other.0)
    }
}

impl Eq for InternedDress {}

pub(crate) fn intern_dress(dress: Arc<Dress>) -> Arc<Dress> {
    if !dress.can_intern() {
        return dress;
    }
    INTERNER.with(|cell| {
        let Ok(mut slot) = cell.try_borrow_mut() else {
            return dress;
        };
        let Some(interner) = slot.as_mut() else {
            return dress;
        };
        let key = InternedDress(dress);
        if let Some(existing) = interner.dresses.get(&key) {
            return Arc::clone(&existing.0);
        }
        let out = Arc::clone(&key.0);
        interner.dresses.insert(key);
        out
    })
}

// --- CompactGrid ---

struct InternedCompactGrid(CompactGrid);

impl Hash for InternedCompactGrid {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let (ticks, base, steps) = self.0.parts();
        ticks.get().hash(state);
        base.hash(state);
        for step in steps {
            step.hash_intern(state);
        }
    }
}

impl PartialEq for InternedCompactGrid {
    fn eq(&self, other: &Self) -> bool {
        if self.0.ptr_eq(&other.0) {
            return true;
        }
        let (at, ab, asteps) = self.0.parts();
        let (bt, bb, bsteps) = other.0.parts();
        at == bt && ab == bb && asteps == bsteps
    }
}

impl Eq for InternedCompactGrid {}

pub(crate) fn intern_compact_grid(grid: CompactGrid) -> CompactGrid {
    let (_, base, steps) = grid.parts();
    if (base & !(1 << 31)) != 0 || steps.iter().any(|s| s.has_loc()) {
        return grid;
    }
    INTERNER.with(|cell| {
        let Ok(mut slot) = cell.try_borrow_mut() else {
            return grid;
        };
        let Some(interner) = slot.as_mut() else {
            return grid;
        };
        let key = InternedCompactGrid(grid);
        if let Some(existing) = interner.compact_grids.get(&key) {
            return existing.0.clone();
        }
        let out = key.0.clone();
        interner.compact_grids.insert(key);
        out
    })
}

// --- Arc<Grid> ---

struct InternedGrid(Arc<Grid>);

impl Hash for InternedGrid {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash_intern(state);
    }
}

impl PartialEq for InternedGrid {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0.eq_intern(&other.0)
    }
}

impl Eq for InternedGrid {}

pub(crate) fn intern_grid(grid: Arc<Grid>) -> Arc<Grid> {
    if !grid.can_intern() {
        return grid;
    }
    INTERNER.with(|cell| {
        let Ok(mut slot) = cell.try_borrow_mut() else {
            return grid;
        };
        let Some(interner) = slot.as_mut() else {
            return grid;
        };
        let key = InternedGrid(grid);
        if let Some(existing) = interner.grids.get(&key) {
            return Arc::clone(&existing.0);
        }
        let out = Arc::clone(&key.0);
        interner.grids.insert(key);
        out
    })
}
