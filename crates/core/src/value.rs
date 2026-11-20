/*
value.rs - JavaScript-compatible semantic values
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

use std::{fmt, hash::Hash, sync::Arc};

/// An insertion-ordered JavaScript object.
///
/// Assignment to an existing property replaces its value without moving it,
/// matching ordinary JavaScript objects. Enumeration additionally applies the
/// ECMAScript integer-index rule: canonical array-index keys precede string
/// keys and are ordered numerically.
#[derive(Clone)]
pub struct OrderedMap {
    entries: Entries,
}

/// Maximum inline patch slots carried by a [`Patched`] overlay before
/// materialising a flat slice.
const MAX_PATCHES: usize = 8;

/// Up to [`MAX_PATCHES`] key/value overrides or additions over a shared
/// dressed [`Table`] row, avoiding flattening `Table::tail` on dynamic
/// control chains. The row index lives alongside the `Arc<Patched>` in
/// [`Entries::Patched`] so chord notes from the same uniform-key table can
/// share one overlay allocation.
#[derive(Clone)]
pub(crate) struct Patched {
    base: Arc<Table>,
    base_len: u16,
    len: u8,
    appended: u8,
    has_replace: bool,
    made: bool,
    marks: u64,
    entries: [Option<(Key, Value, u16)>; MAX_PATCHES],
}

impl Patched {
    fn empty(base: Arc<Table>, row: u32, made: bool) -> Option<Self> {
        let base_len = u16::try_from(base.layout(row).len()).ok()?;
        let marks = base.marks();
        Some(Self {
            base,
            base_len,
            len: 0,
            appended: 0,
            has_replace: false,
            made,
            marks,
            entries: [const { None }; MAX_PATCHES],
        })
    }

    fn of_one(
        base: Arc<Table>,
        row: u32,
        key: Key,
        value: Value,
        replace: Option<usize>,
        made: bool,
    ) -> Option<Self> {
        let base_len = u16::try_from(base.layout(row).len()).ok()?;
        let rep = match replace {
            Some(at) => u16::try_from(at).ok()?,
            None => u16::MAX,
        };
        let marks = base.marks() | Table::mark(key.as_str());
        let appended = u8::from(rep == u16::MAX);
        let has_replace = rep != u16::MAX;
        let mut entries = [const { None }; MAX_PATCHES];
        entries[0] = Some((key, value, rep));
        Some(Self {
            base,
            base_len,
            len: 1,
            appended,
            has_replace,
            made,
            marks,
            entries,
        })
    }

    #[inline(always)]
    fn len(&self) -> usize {
        usize::from(self.base_len) + usize::from(self.appended)
    }

    #[inline(always)]
    fn at(&self, row: u32, index: usize) -> (&Key, &Value) {
        let base_len = usize::from(self.base_len);
        if index < base_len {
            if self.has_replace {
                for slot in &self.entries[..usize::from(self.len)] {
                    if let Some((k, v, rep)) = slot
                        && usize::from(*rep) == index
                    {
                        return (k, v);
                    }
                }
            }
            return self.base.layout(row).at(index);
        }
        let mut want = index - base_len;
        for slot in &self.entries[..usize::from(self.len)] {
            if let Some((k, v, rep)) = slot
                && *rep == u16::MAX
            {
                if want == 0 {
                    return (k, v);
                }
                want -= 1;
            }
        }
        unreachable!("overlay index out of bounds")
    }

    #[inline(always)]
    fn get<K: ?Sized + Named>(&self, row: u32, key: &K) -> Option<&Value>
    where
        Key: PartialEq<K>,
    {
        if self.marks & Table::mark(key.name()) == 0 {
            return None;
        }
        for slot in &self.entries[..usize::from(self.len)] {
            if let Some((k, v, _)) = slot
                && k == key
            {
                return Some(v);
            }
        }
        if self.base.marks() & Table::mark(key.name()) == 0 {
            return None;
        }
        self.base.layout(row).get(key)
    }

    #[inline(always)]
    fn position<K: ?Sized + Named>(&self, row: u32, key: &K) -> Option<usize>
    where
        Key: PartialEq<K>,
    {
        if self.marks & Table::mark(key.name()) == 0 {
            return None;
        }
        let mut append_pos = 0usize;
        for (k, _, rep) in self.entries[..usize::from(self.len)].iter().flatten() {
            if k == key {
                return Some(if *rep == u16::MAX {
                    usize::from(self.base_len) + append_pos
                } else {
                    usize::from(*rep)
                });
            }
            if *rep == u16::MAX {
                append_pos += 1;
            }
        }
        if self.base.marks() & Table::mark(key.name()) == 0 {
            return None;
        }
        self.base.layout(row).position(key)
    }

    fn try_set(&mut self, row: u32, key: &Key, value: Value, data_only: bool) -> Result<(), Value> {
        let len = usize::from(self.len);
        for slot in &mut self.entries[..len] {
            if let Some((k, v, _)) = slot
                && k == key
            {
                *v = value;
                if !data_only {
                    self.made = false;
                }
                return Ok(());
            }
        }
        if len >= MAX_PATCHES {
            return Err(value);
        }
        let mark = Table::mark(key.as_str());
        let replace = if self.base.marks() & mark == 0 {
            u16::MAX
        } else {
            match self.base.layout(row).position(key) {
                Some(idx) => idx as u16,
                None => u16::MAX,
            }
        };
        self.entries[len] = Some((key.clone(), value, replace));
        self.len += 1;
        if replace == u16::MAX {
            self.appended += 1;
        } else {
            self.has_replace = true;
        }
        self.marks |= mark;
        if !data_only {
            self.made = false;
        }
        Ok(())
    }

    fn try_merge_one(
        &mut self,
        row: u32,
        key: &Key,
        other: &Value,
        change: impl FnOnce(&Value, &Value) -> Value,
    ) -> bool {
        let len = usize::from(self.len);
        for slot in &mut self.entries[..len] {
            if let Some((k, v, _)) = slot
                && k == key
            {
                *v = change(v, other);
                self.made = false;
                return true;
            }
        }
        if len >= MAX_PATCHES {
            return false;
        }
        let mark = Table::mark(key.as_str());
        let (value, replace) = if self.base.marks() & mark == 0 {
            (other.clone(), u16::MAX)
        } else {
            let layout = self.base.layout(row);
            match layout.position(key) {
                Some(idx) => (change(layout.at(idx).1, other), idx as u16),
                None => (other.clone(), u16::MAX),
            }
        };
        self.entries[len] = Some((key.clone(), value, replace));
        self.len += 1;
        if replace == u16::MAX {
            self.appended += 1;
        } else {
            self.has_replace = true;
        }
        self.marks |= mark;
        self.made = false;
        true
    }

    fn has_patch_for(&self, key: &Key) -> bool {
        self.entries[..usize::from(self.len)]
            .iter()
            .any(|slot| slot.as_ref().is_some_and(|(k, _, _)| k == key))
    }
}

/// Reusable per-timestamp or per-run overlay cache for chord haps sharing one
/// uniform-key [`Table`] or [`Patched`] overlay.
#[derive(Default)]
pub(crate) struct SharedWaveCache {
    dressed: Option<Arc<Patched>>,
    patched: Option<(Arc<Patched>, Arc<Patched>)>,
}

impl SharedWaveCache {
    #[inline]
    pub(crate) fn clear(&mut self) {
        self.dressed = None;
        self.patched = None;
    }
}

/// The entries of a map, in one allocation that holds them and no more.
/// Both kinds are shared until a write, which copies them first.
#[derive(Clone)]
enum Entries {
    /// A clone is a reference count step.
    Shared(Arc<[(Key, Value)]>),
    /// Shared entries a compose made of data values only, so the map holds
    /// data until it is written again. See [`compose::compose_data_in_place`].
    Made(Arc<[(Key, Value)]>),
    /// No entries and no allocation: what a fresh map holds.
    Empty,
    /// The entries of one step of a layer, which the other steps share:
    /// see [`Table`]. The map holds a handle of the table and the step.
    Dressed(Arc<Table>, u32),
    /// A small overlay of up to [`MAX_PATCHES`] entries over a dressed [`Table`] row.
    Patched(Arc<Patched>, u32),
}

/// What the steps of one layer share when each reads as a map: its own
/// entries first (the head), then the entries that a set of controls
/// gives every step (the tail). This is the map that a compose of the set
/// onto the head makes, as long as no key is in both.
pub(crate) struct Table {
    /// The head of each step. Two variants allow Rust to niche-optimize into 32 B.
    heads: Heads,
    /// The entries of the set, none for a layer with no set.
    tail: Option<Arc<[(Key, Value)]>>,
    /// Low 63 bits hold key presence marks; bit 63 is `MADE_BIT` (compose of data).
    marks_and_made: u64,
}

const _: () = assert!(std::mem::size_of::<Table>() == 56);

/// The entries of one map, shared.
type Shares = Arc<[(Key, Value)]>;

struct PatchedHeads {
    base: Shares,
    key: Key,
    replace: usize,
    values: Arc<[Value]>,
}

struct PatchedKeyedHeads {
    head_key: Key,
    head_value: Value,
    key: Key,
    values: Arc<[Value]>,
}

struct PatchedTableHeads {
    base: Arc<Table>,
    key: Key,
    values: Arc<[Value]>,
}

/// Rare head layouts kept behind one `Box` so [`Heads`] has two variants and
/// niche-optimizes to 32 bytes.
enum OtherHeads {
    /// The entries of the map of each step.
    Maps(Box<[Shares]>),
    /// One shared base map with one key replaced at `replace` (or appended
    /// when `replace == base.len()`) per step.
    Patched(PatchedHeads),
    /// A dressed `Keyed` step `(head_key, head_value)` plus shared `Table::tail`
    /// with one appended `(key, values[index])` per step.
    PatchedKeyed(PatchedKeyedHeads),
    /// Shared across chord notes from one `Heads::Keyed` base table: row index
    /// packs `(base_row << 16) | step_index`.
    PatchedTable(PatchedTableHeads),
    /// Non-table layer values stored inside `Layer::values_read` so `OnceLock`
    /// stays 16 bytes.
    LayerMade(Arc<[Value]>),
    /// Layer read cache with pre-combined spilling contexts.
    LayerWithContexts {
        values: LayerCachedValues,
        contexts: Box<[crate::Locations]>,
    },
}

pub(crate) enum LayerCachedValues {
    Held,
    Made(Arc<[Value]>),
    Shared(Arc<Table>),
}

/// Decoded view of a `Layer::values_read` entry.
pub(crate) enum LayerReadView<'a> {
    Held,
    Made(&'a [Value]),
    Shared(&'a Arc<Table>),
}

/// The head of each step of a [`Table`].
enum Heads {
    /// The value of each step, each once, under one key.
    Keyed(Key, Arc<[Value]>),
    /// Rare layouts behind one `Box`.
    Other(Box<OtherHeads>),
}

impl Table {
    const MADE_BIT: u64 = 1u64 << 63;

    #[inline(always)]
    fn marks(&self) -> u64 {
        self.marks_and_made & !Self::MADE_BIT
    }

    #[inline(always)]
    fn is_made(&self) -> bool {
        self.marks_and_made & Self::MADE_BIT != 0
    }

    /// Wrap non-table precomputed layer values inside a `Table` cell for `Layer`.
    pub(crate) fn layer_made(values: Arc<[Value]>) -> Self {
        Self {
            heads: Heads::Other(Box::new(OtherHeads::LayerMade(values))),
            tail: None,
            marks_and_made: 0,
        }
    }

    /// Wrap layer values plus pre-combined spilling contexts for `Layer`.
    pub(crate) fn layer_with_contexts(
        values: LayerCachedValues,
        contexts: Box<[crate::Locations]>,
    ) -> Self {
        Self {
            heads: Heads::Other(Box::new(OtherHeads::LayerWithContexts { values, contexts })),
            tail: None,
            marks_and_made: 0,
        }
    }

    /// Decode a `Layer::values_read` cell into its values view and optional contexts.
    #[inline]
    pub(crate) fn as_layer_read(
        self: &Arc<Self>,
    ) -> (LayerReadView<'_>, Option<&[crate::Locations]>) {
        match &self.heads {
            Heads::Other(other) => match &**other {
                OtherHeads::LayerMade(values) => (LayerReadView::Made(values), None),
                OtherHeads::LayerWithContexts { values, contexts } => {
                    let view = match values {
                        LayerCachedValues::Held => LayerReadView::Held,
                        LayerCachedValues::Made(vals) => LayerReadView::Made(vals),
                        LayerCachedValues::Shared(table) => LayerReadView::Shared(table),
                    };
                    (view, Some(contexts))
                }
                _ => (LayerReadView::Shared(self), None),
            },
            Heads::Keyed(_, _) => (LayerReadView::Shared(self), None),
        }
    }

    /// Whether every row of this table shares identical key positions, so a
    /// [`Patched`] overlay built for one row applies unchanged to any other.
    #[inline]
    fn has_uniform_keys(&self) -> bool {
        !matches!(&self.heads, Heads::Other(o) if matches!(&**o, OtherHeads::Maps(_)))
    }

    /// The table of `values` under `key`, with `set` after: `None` where
    /// a map would not read the same as the compose of `set` onto the
    /// value. That is no value or no data, a set that is no data, a key
    /// that `set` also holds, and, with no key, a value that is no map.
    pub(crate) fn new(
        values: &Arc<[Value]>,
        key: Option<Key>,
        set: Option<&OrderedMap>,
    ) -> Option<Self> {
        if values.is_empty() || u32::try_from(values.len()).is_err() {
            return None;
        }
        // An empty set is no set, in whatever form its map holds nothing.
        let made = set.is_some();
        let set = set.filter(|set| !set.is_empty());
        if set.is_some_and(|set| !set.pairs().all(|(_, value)| value.is_data(0))) {
            return None;
        }
        let tail: Option<Arc<[(Key, Value)]>> = set.and_then(|set| match &set.entries {
            Entries::Shared(entries) | Entries::Made(entries) => Some(Arc::clone(entries)),
            Entries::Empty => None,
            Entries::Dressed(..) | Entries::Patched(..) => Some(set.layout().to_shares()),
        });
        let held = |name: &Key| {
            tail.as_deref()
                .is_some_and(|tail| tail.iter().any(|(held, _)| held == name))
        };
        let heads = match key {
            Some(key) => {
                if held(&key) || !values.iter().all(|value| value.is_data(0)) {
                    return None;
                }
                Heads::Keyed(key, Arc::clone(values))
            }
            // Without a set there is nothing to share.
            None if set.is_none() => return None,
            None => {
                let mut maps = Vec::with_capacity(values.len());
                for value in values.iter() {
                    let Value::Object(map) = value else {
                        return None;
                    };
                    let entries: Arc<[(Key, Value)]> = match &map.entries {
                        Entries::Shared(entries) | Entries::Made(entries) => Arc::clone(entries),
                        Entries::Empty => Arc::from([]),
                        // The entries of a dressed map, copied once.
                        Entries::Dressed(..) | Entries::Patched(..) => map.layout().to_shares(),
                    };
                    let bare = entries.is_empty() && tail.is_none();
                    if bare || entries.iter().any(|(name, _)| held(name)) || !value.is_data(0) {
                        return None;
                    }
                    maps.push(entries);
                }
                Heads::Other(Box::new(OtherHeads::Maps(maps.into())))
            }
        };
        let mut table = Self {
            heads,
            tail,
            marks_and_made: 0,
        };
        table.marks_and_made = table.marks_of_keys() | if made { Self::MADE_BIT } else { 0 };
        Some(table)
    }

    /// Build a single `Table` that appends `(key, values[step_idx])` onto any
    /// row of a `Heads::Keyed` `base_table`, allowing all chord notes sharing
    /// `base_table` to reuse one allocation.
    pub(crate) fn patched_keyed_table(
        base_table: &Arc<Table>,
        key: Key,
        values: Arc<[Value]>,
    ) -> Option<Self> {
        if values.is_empty() || values.len() > 0xffff {
            return None;
        }
        let Heads::Keyed(head_key, head_values) = &base_table.heads else {
            return None;
        };
        if head_values.len() > 0xffff || head_key == &key {
            return None;
        }
        let mark = Self::mark(&key);
        if base_table.marks() & mark != 0
            && base_table
                .tail
                .as_deref()
                .is_some_and(|tail| tail.iter().any(|(name, _)| name == &key))
        {
            return None;
        }
        let marks = base_table.marks() | mark;
        Some(Self {
            heads: Heads::Other(Box::new(OtherHeads::PatchedTable(PatchedTableHeads {
                base: Arc::clone(base_table),
                key,
                values,
            }))),
            tail: None,
            marks_and_made: marks | Self::MADE_BIT,
        })
    }

    /// Map view for a `(base_row, step_idx)` pair on a `patched_keyed_table`.
    #[inline]
    pub(crate) fn map_packed(self: &Arc<Self>, base_row: u32, step_idx: usize) -> OrderedMap {
        OrderedMap {
            entries: Entries::Dressed(Arc::clone(self), (base_row << 16) | (step_idx as u32)),
        }
    }

    /// A table of data maps sharing `base`, each setting `key` to one of
    /// `values` (all data values). Replaces `key` in place when `base` holds
    /// it, or appends `key` at the end.
    pub(crate) fn patched(base: &OrderedMap, key: Key, values: Arc<[Value]>) -> Option<Self> {
        if values.is_empty() || u32::try_from(values.len()).is_err() {
            return None;
        }
        // Fast path: appending a new key onto a dressed `Keyed` table keeps
        // the shared `tail` slice intact with zero `to_shares()` copy.
        if let Entries::Dressed(base_table, base_idx) = &base.entries
            && let Heads::Keyed(head_key, head_values) = &base_table.heads
            && head_key != &key
            && (base_table.marks() & Self::mark(&key) == 0
                || base_table
                    .tail
                    .as_deref()
                    .is_none_or(|tail| !tail.iter().any(|(name, _)| name == &key)))
        {
            let marks = base_table.marks() | Self::mark(&key);
            return Some(Self {
                heads: Heads::Other(Box::new(OtherHeads::PatchedKeyed(PatchedKeyedHeads {
                    head_key: head_key.clone(),
                    head_value: head_values[*base_idx as usize].clone(),
                    key,
                    values,
                }))),
                tail: base_table.tail.clone(),
                marks_and_made: marks | Self::MADE_BIT,
            });
        }
        let entries: Shares = match &base.entries {
            Entries::Shared(entries) | Entries::Made(entries) => Arc::clone(entries),
            Entries::Empty => Arc::from([]),
            Entries::Dressed(..) | Entries::Patched(..) => base.layout().to_shares(),
        };
        let replace = entries
            .iter()
            .position(|(name, _)| name == &key)
            .unwrap_or(entries.len());
        let mut table = Self {
            heads: Heads::Other(Box::new(OtherHeads::Patched(PatchedHeads {
                base: entries,
                key,
                replace,
                values,
            }))),
            tail: None,
            marks_and_made: 0,
        };
        table.marks_and_made = table.marks_of_keys() | Self::MADE_BIT;
        Some(table)
    }

    /// The mark of a key name: one of 63 bits (leaving bit 63 for `MADE_BIT`),
    /// from the length and the first, middle and last bytes, which is all that
    /// a lookup of a name that most steps lack needs to rule it out.
    #[inline(always)]
    fn mark(name: &str) -> u64 {
        let bytes = name.as_bytes();
        let (Some(&first), Some(&last)) = (bytes.first(), bytes.last()) else {
            return 1;
        };
        let middle = bytes[bytes.len() / 2];
        let mixed = (bytes.len() as u32 & 0xff)
            | u32::from(first) << 8
            | u32::from(last) << 16
            | u32::from(middle) << 24;
        1u64 << ((mixed.wrapping_mul(0x9E37_79B1) >> 26) % 63)
    }

    /// The marks of every key name that a step of this table holds.
    fn marks_of_keys(&self) -> u64 {
        let of = |entries: &[(Key, Value)]| {
            entries
                .iter()
                .fold(0, |marks, (name, _)| marks | Self::mark(name))
        };
        let tail = self.tail.as_deref().map_or(0, of);
        tail | match &self.heads {
            Heads::Keyed(key, _) => Self::mark(key),
            Heads::Other(other) => match &**other {
                OtherHeads::Maps(maps) => maps.iter().fold(0, |marks, map| marks | of(map)),
                OtherHeads::Patched(p) => of(&p.base) | Self::mark(&p.key),
                OtherHeads::PatchedKeyed(p) => Self::mark(&p.head_key) | Self::mark(&p.key),
                OtherHeads::PatchedTable(p) => p.base.marks() | Self::mark(&p.key),
                OtherHeads::LayerMade(_) | OtherHeads::LayerWithContexts { .. } => 0,
            },
        }
    }

    /// The value of step `index` as a map, for a step that the table holds.
    #[inline]
    pub(crate) fn map(self: &Arc<Self>, index: usize) -> Option<OrderedMap> {
        let held = match &self.heads {
            Heads::Keyed(_, values) => values.len(),
            Heads::Other(other) => match &**other {
                OtherHeads::Maps(maps) => maps.len(),
                OtherHeads::Patched(p) => p.values.len(),
                OtherHeads::PatchedKeyed(p) => p.values.len(),
                OtherHeads::PatchedTable(p) => p.values.len(),
                OtherHeads::LayerMade(_) | OtherHeads::LayerWithContexts { .. } => return None,
            },
        };
        // The table holds no more steps than a u32 counts.
        (index < held).then(|| OrderedMap {
            entries: Entries::Dressed(Arc::clone(self), index as u32),
        })
    }

    #[inline(always)]
    fn layout(&self, index: u32) -> Layout<'_> {
        let tail = self.tail.as_deref().unwrap_or(&[]);
        match &self.heads {
            Heads::Keyed(key, values) => Layout::Keyed(key, &values[index as usize], tail),
            Heads::Other(other) => match &**other {
                OtherHeads::Maps(maps) => Layout::Split(&maps[index as usize], tail),
                OtherHeads::Patched(p) => {
                    Layout::Patched(&p.base, p.replace, &p.key, &p.values[index as usize])
                }
                OtherHeads::PatchedKeyed(p) => Layout::KeyedExtra(
                    &p.head_key,
                    &p.head_value,
                    tail,
                    &p.key,
                    &p.values[index as usize],
                ),
                OtherHeads::PatchedTable(p) => {
                    let base_row = (index >> 16) as usize;
                    let step_idx = (index & 0xffff) as usize;
                    let Heads::Keyed(hk, hvals) = &p.base.heads else {
                        unreachable!("PatchedTable wraps Heads::Keyed");
                    };
                    let base_tail = p.base.tail.as_deref().unwrap_or(&[]);
                    Layout::KeyedExtra(hk, &hvals[base_row], base_tail, &p.key, &p.values[step_idx])
                }
                OtherHeads::LayerMade(_) | OtherHeads::LayerWithContexts { .. } => {
                    unreachable!("layer-internal cache cannot be read as OrderedMap")
                }
            },
        }
    }
}

/// The text of a key name, for the lookups that a key or a string asks.
trait Named {
    fn name(&self) -> &str;
}

impl Named for Key {
    #[inline(always)]
    fn name(&self) -> &str {
        self.as_str()
    }
}

impl Named for str {
    #[inline(always)]
    fn name(&self) -> &str {
        self
    }
}

/// How a map lays its entries out, in the order of the map.
#[derive(Clone, Copy)]
enum Layout<'a> {
    /// All the entries in one slice.
    Plain(&'a [(Key, Value)]),
    /// One entry of a key and its value, then a slice.
    Keyed(&'a Key, &'a Value, &'a [(Key, Value)]),
    /// A slice, then another.
    Split(&'a [(Key, Value)], &'a [(Key, Value)]),
    /// A slice with index `replace` overridden by `(key, value)` (or appended
    /// when `replace == base.len()`).
    Patched(&'a [(Key, Value)], usize, &'a Key, &'a Value),
    /// One leading entry, a shared middle slice, and one appended entry.
    KeyedExtra(&'a Key, &'a Value, &'a [(Key, Value)], &'a Key, &'a Value),
    /// A small overlay on top of a dressed table row.
    Overlay(&'a Patched, u32),
}

impl<'a> Layout<'a> {
    #[inline]
    fn len(self) -> usize {
        match self {
            Self::Plain(entries) => entries.len(),
            Self::Keyed(_, _, tail) => 1 + tail.len(),
            Self::Split(head, tail) => head.len() + tail.len(),
            Self::Patched(base, replace, _, _) => base.len() + usize::from(replace == base.len()),
            Self::KeyedExtra(_, _, tail, _, _) => 2 + tail.len(),
            Self::Overlay(patched, _) => patched.len(),
        }
    }

    #[inline]
    fn pairs(self) -> Pairs<'a> {
        let inner = match self {
            Self::Plain(entries) => PairsInner::Plain(entries.iter()),
            Self::Keyed(key, value, tail) => PairsInner::Keyed(key, value, tail.iter()),
            Self::Split(head, tail) => PairsInner::Split(head.iter(), tail.iter()),
            Self::Patched(base, replace, key, value) => PairsInner::Patched(
                base[..replace].iter(),
                Some((key, value)),
                base.get(replace + 1..).unwrap_or(&[]).iter(),
            ),
            Self::KeyedExtra(hk, hv, tail, ek, ev) => {
                PairsInner::KeyedExtra(Some((hk, hv)), tail.iter(), Some((ek, ev)))
            }
            Self::Overlay(patched, row) => PairsInner::Overlay {
                patched,
                row,
                index: 0,
                total: patched.len(),
            },
        };
        Pairs(inner)
    }

    /// The entry at `index`.
    #[inline]
    fn at(self, index: usize) -> (&'a Key, &'a Value) {
        let pair = |(key, value): &'a (Key, Value)| (key, value);
        match self {
            Self::Plain(entries) => pair(&entries[index]),
            Self::Keyed(key, value, tail) => match index {
                0 => (key, value),
                _ => pair(&tail[index - 1]),
            },
            Self::Split(head, tail) => match index.checked_sub(head.len()) {
                None => pair(&head[index]),
                Some(index) => pair(&tail[index]),
            },
            Self::Patched(base, replace, key, value) => match index == replace {
                true => (key, value),
                false => pair(&base[index]),
            },
            Self::KeyedExtra(hk, hv, tail, ek, ev) => {
                if index == 0 {
                    (hk, hv)
                } else if index <= tail.len() {
                    pair(&tail[index - 1])
                } else {
                    (ek, ev)
                }
            }
            Self::Overlay(patched, row) => patched.at(row, index),
        }
    }

    /// The place of the entry of `key`.
    #[inline(always)]
    fn position<K: ?Sized + Named>(self, key: &K) -> Option<usize>
    where
        Key: PartialEq<K>,
    {
        let find = |entries: &[(Key, Value)]| entries.iter().position(|(name, _)| name == key);
        match self {
            Self::Plain(entries) => find(entries),
            Self::Keyed(name, _, tail) => match name == key {
                true => Some(0),
                false => find(tail).map(|at| at + 1),
            },
            Self::Split(head, tail) => find(head).or_else(|| find(tail).map(|at| at + head.len())),
            Self::Patched(base, replace, name, _) => match name == key {
                true => Some(replace),
                false => find(base),
            },
            Self::KeyedExtra(hk, _, tail, ek, _) => {
                if hk == key {
                    Some(0)
                } else if ek == key {
                    Some(1 + tail.len())
                } else {
                    find(tail).map(|at| at + 1)
                }
            }
            Self::Overlay(patched, row) => patched.position(row, key),
        }
    }

    #[inline(always)]
    fn get<K: ?Sized + Named>(self, key: &K) -> Option<&'a Value>
    where
        Key: PartialEq<K>,
    {
        let find = |entries: &'a [(Key, Value)]| {
            entries
                .iter()
                .find_map(|(name, value)| (name == key).then_some(value))
        };
        match self {
            Self::Plain(entries) => find(entries),
            Self::Keyed(name, value, tail) => match name == key {
                true => Some(value),
                false => find(tail),
            },
            Self::Split(head, tail) => find(head).or_else(|| find(tail)),
            Self::Patched(base, _, name, value) => match name == key {
                true => Some(value),
                false => find(base),
            },
            Self::KeyedExtra(hk, hv, tail, ek, ev) => {
                if ek == key {
                    Some(ev)
                } else if hk == key {
                    Some(hv)
                } else {
                    find(tail)
                }
            }
            Self::Overlay(patched, row) => patched.get(row, key),
        }
    }
}

impl Layout<'_> {
    /// The entries in one allocation. Iterators that know their length
    /// collect into one allocation of the exact size, with no list first.
    fn to_shares(self) -> Shares {
        let pair = |(key, value): &(Key, Value)| (key.clone(), value.clone());
        match self {
            Self::Plain(entries) => entries.iter().map(pair).collect(),
            Self::Keyed(key, value, tail) => std::iter::once((key.clone(), value.clone()))
                .chain(tail.iter().map(pair))
                .collect(),
            Self::Split(head, tail) => head.iter().chain(tail).map(pair).collect(),
            Self::Patched(base, replace, key, value) => base[..replace]
                .iter()
                .map(pair)
                .chain(std::iter::once((key.clone(), value.clone())))
                .chain(base.get(replace + 1..).unwrap_or(&[]).iter().map(pair))
                .collect(),
            Self::KeyedExtra(hk, hv, tail, ek, ev) => std::iter::once((hk.clone(), hv.clone()))
                .chain(tail.iter().map(pair))
                .chain(std::iter::once((ek.clone(), ev.clone())))
                .collect(),
            Self::Overlay(patched, row) => (0..patched.len())
                .map(|index| {
                    let (key, value) = patched.at(row, index);
                    (key.clone(), value.clone())
                })
                .collect(),
        }
    }

    /// The entries in one allocation, where the entry at `at` takes `value`,
    /// or `value` comes last under `key` when `at` is `None`.
    fn with(self, key: &Key, at: Option<usize>, value: Value) -> Shares {
        let value = std::cell::Cell::new(Some(value));
        let pair = |index: usize, name: &Key, held: &Value| match at == Some(index) {
            true => (name.clone(), value.take().expect("the value goes in once")),
            false => (name.clone(), held.clone()),
        };
        let last = at
            .is_none()
            .then(|| (key.clone(), value.take().expect("the value goes in once")));
        match self {
            Self::Plain(entries) => entries
                .iter()
                .enumerate()
                .map(|(index, (name, held))| pair(index, name, held))
                .chain(last)
                .collect(),
            Self::Keyed(name, held, tail) => std::iter::once(pair(0, name, held))
                .chain(
                    tail.iter()
                        .enumerate()
                        .map(|(index, (name, held))| pair(index + 1, name, held)),
                )
                .chain(last)
                .collect(),
            Self::Split(head, tail) => head
                .iter()
                .enumerate()
                .map(|(index, (name, held))| pair(index, name, held))
                .chain(
                    tail.iter()
                        .enumerate()
                        .map(|(index, (name, held))| pair(head.len() + index, name, held)),
                )
                .chain(last)
                .collect(),
            Self::Patched(base, replace, name, held) => base[..replace]
                .iter()
                .enumerate()
                .map(|(index, (entry_key, entry_val))| pair(index, entry_key, entry_val))
                .chain(std::iter::once(pair(replace, name, held)))
                .chain(
                    base.get(replace + 1..)
                        .unwrap_or(&[])
                        .iter()
                        .enumerate()
                        .map(|(index, (entry_key, entry_val))| {
                            pair(replace + 1 + index, entry_key, entry_val)
                        }),
                )
                .chain(last)
                .collect(),
            Self::KeyedExtra(hk, hv, tail, ek, ev) => std::iter::once(pair(0, hk, hv))
                .chain(
                    tail.iter()
                        .enumerate()
                        .map(|(index, (name, held))| pair(index + 1, name, held)),
                )
                .chain(std::iter::once(pair(1 + tail.len(), ek, ev)))
                .chain(last)
                .collect(),
            Self::Overlay(patched, row) => (0..patched.len())
                .map(|index| {
                    let (name, held) = patched.at(row, index);
                    pair(index, name, held)
                })
                .chain(last)
                .collect(),
        }
    }
}

/// The entries of a map, in order.
#[derive(Clone)]
pub struct Pairs<'a>(PairsInner<'a>);

#[derive(Clone)]
enum PairsInner<'a> {
    /// The entries left in one slice.
    Plain(std::slice::Iter<'a, (Key, Value)>),
    /// One entry, then the entries left in a slice.
    Keyed(&'a Key, &'a Value, std::slice::Iter<'a, (Key, Value)>),
    /// The entries left in a slice, then those in another.
    Split(
        std::slice::Iter<'a, (Key, Value)>,
        std::slice::Iter<'a, (Key, Value)>,
    ),
    /// Prefix slice, one overridden/appended entry, then suffix slice.
    Patched(
        std::slice::Iter<'a, (Key, Value)>,
        Option<(&'a Key, &'a Value)>,
        std::slice::Iter<'a, (Key, Value)>,
    ),
    /// Leading entry, shared middle slice, then appended entry.
    KeyedExtra(
        Option<(&'a Key, &'a Value)>,
        std::slice::Iter<'a, (Key, Value)>,
        Option<(&'a Key, &'a Value)>,
    ),
    /// Entries from a patched overlay over a dressed table row.
    Overlay {
        patched: &'a Patched,
        row: u32,
        index: usize,
        total: usize,
    },
}

impl<'a> Iterator for Pairs<'a> {
    type Item = (&'a Key, &'a Value);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let (key, value) = match &mut self.0 {
            PairsInner::Plain(entries) => entries.next()?,
            PairsInner::Keyed(key, value, tail) => {
                let (key, value) = (*key, *value);
                self.0 = PairsInner::Plain(tail.clone());
                return Some((key, value));
            }
            PairsInner::Split(head, tail) => head.next().or_else(|| tail.next())?,
            PairsInner::Patched(head, middle, tail) => {
                if let Some((k, v)) = head.next() {
                    return Some((k, v));
                }
                if let Some(pair) = middle.take() {
                    return Some(pair);
                }
                let (k, v) = tail.next()?;
                return Some((k, v));
            }
            PairsInner::KeyedExtra(head, middle, extra) => {
                if let Some(pair) = head.take() {
                    return Some(pair);
                }
                if let Some((k, v)) = middle.next() {
                    return Some((k, v));
                }
                return extra.take();
            }
            PairsInner::Overlay {
                patched,
                row,
                index,
                total,
            } => {
                if *index >= *total {
                    return None;
                }
                let pair = patched.at(*row, *index);
                *index += 1;
                return Some(pair);
            }
        };
        Some((key, value))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = match &self.0 {
            PairsInner::Plain(entries) => entries.len(),
            PairsInner::Keyed(_, _, tail) => 1 + tail.len(),
            PairsInner::Split(head, tail) => head.len() + tail.len(),
            PairsInner::Patched(head, middle, tail) => {
                head.len() + usize::from(middle.is_some()) + tail.len()
            }
            PairsInner::KeyedExtra(head, middle, extra) => {
                usize::from(head.is_some()) + middle.len() + usize::from(extra.is_some())
            }
            PairsInner::Overlay { index, total, .. } => total.saturating_sub(*index),
        };
        (len, Some(len))
    }
}

impl ExactSizeIterator for Pairs<'_> {}

/// The entries that a map held, for the map that takes their place: they
/// move where no other map shares them, and are copied otherwise.
enum Held<'a> {
    Own(&'a mut [(Key, Value)]),
    View(Layout<'a>),
}

impl Held<'_> {
    /// Whether the held entries have an entry of `key`.
    fn holds(&self, key: &Key) -> bool {
        match self {
            Self::Own(entries) => entries.iter().any(|(name, _)| name == key),
            Self::View(layout) => layout.position(key).is_some(),
        }
    }

    /// The entry at `index`, to hold in the new map.
    fn take(&mut self, index: usize) -> (Key, Value) {
        match self {
            // A handle of the key, and the value itself.
            Self::Own(entries) => {
                let (key, value) = &mut entries[index];
                (key.clone(), std::mem::replace(value, Value::Undefined))
            }
            Self::View(layout) => {
                let (key, value) = layout.at(index);
                (key.clone(), value.clone())
            }
        }
    }
}

impl Default for OrderedMap {
    fn default() -> Self {
        Self {
            entries: Entries::Empty,
        }
    }
}

impl PartialEq for OrderedMap {
    fn eq(&self, other: &Self) -> bool {
        if let (Layout::Plain(a), Layout::Plain(b)) = (self.layout(), other.layout()) {
            return a == b;
        }
        self.len() == other.len()
            && self
                .pairs()
                .zip(other.pairs())
                .all(|((a_key, a), (b_key, b))| a_key == b_key && a == b)
    }
}

impl fmt::Debug for OrderedMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The list of pairs that a plain map prints as its entries.
        f.debug_struct("OrderedMap")
            .field("entries", &DebugPairs(self))
            .finish()
    }
}

/// The entries of a map as a list, for [`fmt::Debug`].
struct DebugPairs<'a>(&'a OrderedMap);

impl fmt::Debug for DebugPairs<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.0.pairs()).finish()
    }
}

impl OrderedMap {
    #[inline(always)]
    fn layout(&self) -> Layout<'_> {
        match &self.entries {
            Entries::Shared(entries) | Entries::Made(entries) => Layout::Plain(entries),
            Entries::Empty => Layout::Plain(&[]),
            Entries::Dressed(table, index) => table.layout(*index),
            Entries::Patched(patched, row) => Layout::Overlay(patched, *row),
        }
    }

    #[inline(always)]
    pub(crate) fn pairs(&self) -> Pairs<'_> {
        self.layout().pairs()
    }

    /// This map with its entries in a plain map of its own, which a write
    /// needs and a set that a dress holds does not pin a table with.
    pub(crate) fn undressed(mut self) -> Self {
        self.undress();
        self
    }

    /// Copy an overlay map into a compact plain slice so long-lived graph
    /// nodes do not retain `Patched` overlays or pin source tables.
    pub(crate) fn undress_patched(&mut self) {
        if let Entries::Patched(patched, row) = &self.entries {
            let entries = Layout::Overlay(patched, *row).to_shares();
            self.entries = match patched.made {
                true => Entries::Made(entries),
                false => Entries::Shared(entries),
            };
        }
    }

    /// If this map is backed by a dressed [`Table`], return `(table, row)`.
    #[inline]
    pub(crate) fn dressed_table(&self) -> Option<(&Arc<Table>, u32)> {
        match &self.entries {
            Entries::Dressed(table, row) => Some((table, *row)),
            _ => None,
        }
    }

    /// The entries of a dressed map copied into a plain one, which keeps
    /// whether a compose made them of data.
    fn undress(&mut self) {
        match &self.entries {
            Entries::Dressed(table, index) => {
                let entries = table.layout(*index).to_shares();
                self.entries = match table.is_made() {
                    true => Entries::Made(entries),
                    false => Entries::Shared(entries),
                };
            }
            Entries::Patched(patched, row) => {
                let entries = Layout::Overlay(patched, *row).to_shares();
                self.entries = match patched.made {
                    true => Entries::Made(entries),
                    false => Entries::Shared(entries),
                };
            }
            _ => {}
        }
    }

    /// The entries to write where they are. Entries another map shares
    /// are copied first, once.
    pub(crate) fn entries_mut(&mut self) -> &mut [(Key, Value)] {
        self.undress();
        // A map a compose made of data holds that guarantee only until it is
        // written again; the compose re-marks its own writes.
        if let Entries::Made(entries) = &self.entries {
            let entries = Arc::clone(entries);
            self.entries = Entries::Shared(entries);
        }
        let Entries::Shared(entries) = &mut self.entries else {
            return &mut [];
        };
        if Arc::get_mut(entries).is_none() {
            *entries = Arc::from(&entries[..]);
        }
        Arc::get_mut(entries).expect("entries that no other map shares")
    }

    /// A map of `entries`, each of another key.
    fn of(entries: Arc<[(Key, Value)]>) -> Self {
        Self {
            entries: match entries.is_empty() {
                true => Entries::Empty,
                false => Entries::Shared(entries),
            },
        }
    }

    /// The shared flat entries when this map is already undressed and non-empty.
    #[inline]
    pub(crate) fn as_shares(&self) -> Option<&Arc<[(Key, Value)]>> {
        match &self.entries {
            Entries::Shared(entries) | Entries::Made(entries) => Some(entries),
            _ => None,
        }
    }

    /// Reconstruct a data-only map from undressed shared entries.
    #[inline]
    pub(crate) fn from_made_shares(entries: Arc<[(Key, Value)]>) -> Self {
        Self {
            entries: if entries.is_empty() {
                Entries::Empty
            } else {
                Entries::Made(entries)
            },
        }
    }

    /// A map of `entries`, each of another key, in their order.
    pub(crate) fn of_keys(entries: impl IntoIterator<Item = (Key, Value)>) -> Self {
        Self::of(entries.into_iter().collect())
    }

    /// Put `count` entries in the place of the entries of this map: `entry`
    /// gives each in turn, from the entries that the map held. One
    /// allocation holds the new entries.
    fn rebuild(
        &mut self,
        count: usize,
        mut entry: impl FnMut(&mut Held<'_>, usize) -> (Key, Value),
    ) {
        let mut taken = std::mem::replace(&mut self.entries, Entries::Empty);
        let mut held = match &mut taken {
            Entries::Shared(entries) | Entries::Made(entries) => match Arc::get_mut(entries) {
                Some(own) => Held::Own(own),
                None => Held::View(Layout::Plain(entries)),
            },
            Entries::Empty => Held::View(Layout::Plain(&[])),
            Entries::Dressed(table, index) => Held::View(table.layout(*index)),
            Entries::Patched(patched, row) => Held::View(Layout::Overlay(patched, *row)),
        };
        // A range with a map says its length, so the entries are made
        // where the new map holds them.
        *self = Self::of((0..count).map(|index| entry(&mut held, index)).collect());
    }

    /// Add `entry`, of a key that the map does not hold, after the others.
    /// One allocation holds the new entries: the old ones move where no
    /// other map shares them, and are copied otherwise.
    fn append(&mut self, entry: (Key, Value)) {
        if let Entries::Patched(patched, _) = &mut self.entries
            && let Some(own) = Arc::get_mut(patched)
            && usize::from(own.len) < MAX_PATCHES
        {
            let (key, value) = entry;
            let mark = Table::mark(key.as_str());
            let idx = usize::from(own.len);
            own.entries[idx] = Some((key, value, u16::MAX));
            own.len += 1;
            own.appended += 1;
            own.marks |= mark;
            own.made = false;
            return;
        }
        if let Entries::Dressed(table, index) = &self.entries
            && let Some(patched) = Patched::of_one(
                Arc::clone(table),
                *index,
                entry.0.clone(),
                entry.1.clone(),
                None,
                false,
            )
        {
            self.entries = Entries::Patched(Arc::new(patched), *index);
            return;
        }
        let held: Shares = match std::mem::replace(&mut self.entries, Entries::Empty) {
            Entries::Shared(mut entries) | Entries::Made(mut entries) => {
                match Arc::get_mut(&mut entries) {
                    // A handle of the key, and the value itself.
                    Some(own) => own
                        .iter_mut()
                        .map(|(key, value)| {
                            (key.clone(), std::mem::replace(value, Value::Undefined))
                        })
                        .chain(std::iter::once(entry))
                        .collect(),
                    None => entries
                        .iter()
                        .cloned()
                        .chain(std::iter::once(entry))
                        .collect(),
                }
            }
            Entries::Empty => Arc::new([entry]),
            Entries::Dressed(table, index) => {
                let (key, value) = entry;
                table.layout(index).with(&key, None, value)
            }
            Entries::Patched(patched, row) => {
                let (key, value) = entry;
                Layout::Overlay(&patched, row).with(&key, None, value)
            }
        };
        self.entries = Entries::Shared(held);
    }

    /// Each entry of `other` in this map. An entry of a key that the map
    /// holds changes the value of that key where it is, to what `change`
    /// makes of the value and the one of `other`. The other entries come
    /// after, in the order of `other`.
    pub(crate) fn merge(&mut self, other: &Self, mut change: impl FnMut(&Value, &Value) -> Value) {
        // Nothing to add changes nothing, and a dressed map stays dressed.
        if other.is_empty() {
            return;
        }
        // A control that sets one key, which most chains do: one lookup, and
        // one write.
        let other_len = other.len();
        if other_len == 1
            && let Some((key, value)) = other.pairs().next()
        {
            return self.merge_one(key, value, change);
        }
        // Small merges onto a dressed or uniquely-owned patched map stay in
        // the inline overlay without flattening `Table::tail`.
        if other_len <= MAX_PATCHES {
            match &mut self.entries {
                Entries::Dressed(table, row) => {
                    let row = *row;
                    if let Some(mut patched) = Patched::empty(Arc::clone(table), row, false) {
                        for (key, value) in other.pairs() {
                            let ok = patched.try_merge_one(row, key, value, &mut change);
                            debug_assert!(ok);
                        }
                        self.entries = Entries::Patched(Arc::new(patched), row);
                        return;
                    }
                }
                Entries::Patched(patched, row) => {
                    let row = *row;
                    if let Some(own) = Arc::get_mut(patched) {
                        let extra = other.pairs().filter(|(k, _)| !own.has_patch_for(k)).count();
                        if usize::from(own.len) + extra <= MAX_PATCHES {
                            for (key, value) in other.pairs() {
                                let ok = own.try_merge_one(row, key, value, &mut change);
                                debug_assert!(ok);
                            }
                            return;
                        }
                    }
                }
                _ => {}
            }
        }
        let of = |key: &Key| other.layout().get(key);
        let fresh = other
            .pairs()
            .filter(|(key, _)| self.position(*key).is_none())
            .count();
        if fresh == 0 {
            for (key, value) in self.entries_mut() {
                if let Some(other) = of(key) {
                    *value = change(value, other);
                }
            }
            return;
        }
        let held = self.len();
        let mut more = other.pairs();
        self.rebuild(held + fresh, |entries, index| {
            if index < held {
                let (key, value) = entries.take(index);
                return match of(&key) {
                    Some(other) => (key, change(&value, other)),
                    None => (key, value),
                };
            }
            // The next entry of `other` whose key the map did not hold.
            let (key, value) = more
                .find(|(key, _)| !entries.holds(key))
                .expect("a key that the map did not hold");
            (key.clone(), value.clone())
        });
    }

    /// [`Self::merge`] of the one entry `key` and `other`.
    fn merge_one(
        &mut self,
        key: &Key,
        other: &Value,
        mut change: impl FnMut(&Value, &Value) -> Value,
    ) {
        if let Entries::Patched(patched, row) = &mut self.entries
            && let row = *row
            && let Some(own) = Arc::get_mut(patched)
            && own.try_merge_one(row, key, other, &mut change)
        {
            return;
        }
        if let Entries::Dressed(table, row) = &self.entries {
            let row = *row;
            let at = self.position(key);
            let value = match at {
                Some(at) => change(table.layout(row).at(at).1, other),
                None => other.clone(),
            };
            if let Some(patched) = Patched::of_one(
                Arc::clone(table),
                row,
                key.clone(),
                value.clone(),
                at,
                false,
            ) {
                self.entries = Entries::Patched(Arc::new(patched), row);
                return;
            }
            self.entries = Entries::Shared(table.layout(row).with(key, at, value));
            return;
        }
        let at = self.position(key);
        match (&self.entries, at) {
            (Entries::Patched(..), at) => {
                let value = match at {
                    Some(at) => change(self.layout().at(at).1, other),
                    None => other.clone(),
                };
                self.entries = Entries::Shared(self.layout().with(key, at, value));
            }
            (_, Some(at)) => {
                let entries = self.entries_mut();
                let value = change(&entries[at].1, other);
                entries[at].1 = value;
            }
            (_, None) => self.append((key.clone(), other.clone())),
        }
    }

    /// The place of the entry of `key`. A dressed map rules out a name that
    /// no step of its table holds without a scan.
    #[inline(always)]
    fn position<K: ?Sized + Named>(&self, key: &K) -> Option<usize>
    where
        Key: PartialEq<K>,
    {
        match &self.entries {
            Entries::Empty => None,
            Entries::Shared(entries) | Entries::Made(entries) => {
                Layout::Plain(entries).position(key)
            }
            Entries::Dressed(table, index) => match table.marks() & Table::mark(key.name()) {
                0 => None,
                _ => table.layout(*index).position(key),
            },
            Entries::Patched(patched, row) => patched.position(*row, key),
        }
    }

    /// Whether the map's own storage answers for its data-ness: an empty
    /// map holds nothing else, and a made map was made of data values only.
    pub(crate) fn holds_only_data(&self) -> bool {
        match &self.entries {
            Entries::Empty | Entries::Made(_) => true,
            Entries::Dressed(table, _) => table.is_made(),
            Entries::Patched(patched, _) => patched.made,
            Entries::Shared(_) => false,
        }
    }

    /// Whether the map's storage tells that it holds data only, with no
    /// walk of its entries. A table also holds data only when no compose
    /// made it: it refuses a value that is no data.
    pub(crate) fn is_known_data(&self) -> bool {
        matches!(self.entries, Entries::Dressed(..)) || self.holds_only_data()
    }

    /// A hash of this map's data that agrees with [`Self::same_data`].
    #[cfg(test)]
    pub(crate) fn data_hash(&self) -> u64 {
        let mut hasher = WordHasher::default();
        hash_data_entries(self.layout(), &mut hasher);
        std::hash::Hasher::finish(&hasher)
    }

    /// Whether two maps hold the same keys and the same data, in order and
    /// down to the bits of every number. `==` makes `0.0` equal to `-0.0`,
    /// which divide apart, and a NaN unequal to itself.
    pub(crate) fn same_data(&self, other: &Self) -> bool {
        if let (Entries::Dressed(a, at), Entries::Dressed(b, to)) = (&self.entries, &other.entries)
            && Arc::ptr_eq(a, b)
            && at == to
        {
            // The same step of one table is one map.
            return true;
        }
        same_entries(self.pairs(), other.pairs())
    }

    /// Whether the map holds no allocation, so a clone copies nothing.
    #[cfg(test)]
    pub(crate) fn owns_no_heap(&self) -> bool {
        matches!(self.entries, Entries::Empty)
    }

    /// Mark this map, whose writes a compose of data values just finished,
    /// as holding data only. A map another writer shares, or an empty one,
    /// is left alone: the guarantee belongs to this compose's own unique
    /// copy.
    pub(crate) fn mark_made_of_data(&mut self) {
        let current = std::mem::replace(&mut self.entries, Entries::Empty);
        self.entries = match current {
            Entries::Shared(entries) if Arc::strong_count(&entries) == 1 => Entries::Made(entries),
            Entries::Patched(mut patched, row) => {
                if let Some(own) = Arc::get_mut(&mut patched) {
                    own.made = true;
                }
                Entries::Patched(patched, row)
            }
            other => other,
        };
    }
    pub fn new() -> Self {
        Self::default()
    }

    /// Set `key` to `value`, a data value, in this map of data: a `set` of
    /// the map of that one entry, with no such map. The map then holds
    /// data only.
    pub(crate) fn set_data(&mut self, key: &Key, mut value: Value) {
        if let Entries::Patched(patched, row) = &mut self.entries
            && let row = *row
            && let Some(own) = Arc::get_mut(patched)
        {
            match own.try_set(row, key, value, true) {
                Ok(()) => return,
                Err(returned) => value = returned,
            }
        }
        if let Entries::Dressed(table, row) = &self.entries {
            let row = *row;
            let at = self.position(key);
            if let Some(patched) =
                Patched::of_one(Arc::clone(table), row, key.clone(), value.clone(), at, true)
            {
                self.entries = Entries::Patched(Arc::new(patched), row);
                return;
            }
            let entries = table.layout(row).with(key, at, value);
            self.entries = Entries::Made(entries);
            return;
        }
        if let Entries::Patched(..) = &self.entries {
            let entries = self.layout().with(key, self.position(key), value);
            self.entries = Entries::Made(entries);
            return;
        }
        match self.position(key) {
            Some(index) => self.entries_mut()[index].1 = value,
            None => self.append((key.clone(), value)),
        }
        self.mark_made_of_data();
    }

    /// Set several numeric data controls, reusing one [`Patched`] allocation
    /// across chord haps that share a uniform-key [`Table`] or [`Patched`]
    /// overlay at the same sample.
    pub(crate) fn set_data_many_cached(
        &mut self,
        patches: &[(&Key, f64)],
        cache: &mut SharedWaveCache,
    ) {
        if !patches.is_empty() && patches.len() <= MAX_PATCHES {
            match &mut self.entries {
                Entries::Dressed(table, row) if table.has_uniform_keys() => {
                    let row = *row;
                    if let Some(cached) = &cache.dressed
                        && Arc::ptr_eq(&cached.base, table)
                    {
                        self.entries = Entries::Patched(Arc::clone(cached), row);
                        return;
                    }
                    if let Some(mut patched) = Patched::empty(Arc::clone(table), row, true) {
                        for (key, num) in patches {
                            let _ = patched.try_set(row, key, Value::F64(*num), true);
                        }
                        let arc = Arc::new(patched);
                        cache.dressed = Some(Arc::clone(&arc));
                        self.entries = Entries::Patched(arc, row);
                        return;
                    }
                }
                Entries::Patched(prev, row)
                    if prev.base.has_uniform_keys() && Arc::strong_count(prev) > 1 =>
                {
                    let row = *row;
                    if let Some((cached_prev, cached_out)) = &cache.patched
                        && Arc::ptr_eq(cached_prev, prev)
                    {
                        self.entries = Entries::Patched(Arc::clone(cached_out), row);
                        return;
                    }
                    let mut extra = 0usize;
                    for (i, (key, _)) in patches.iter().enumerate() {
                        if !prev.has_patch_for(key)
                            && !patches[..i].iter().any(|(other, _)| other == key)
                        {
                            extra += 1;
                        }
                    }
                    if usize::from(prev.len) + extra <= MAX_PATCHES {
                        let mut next = (**prev).clone();
                        for (key, num) in patches {
                            let _ = next.try_set(row, key, Value::F64(*num), true);
                        }
                        next.made = true;
                        let arc = Arc::new(next);
                        cache.patched = Some((Arc::clone(prev), Arc::clone(&arc)));
                        self.entries = Entries::Patched(arc, row);
                        return;
                    }
                }
                _ => {}
            }
        }
        self.set_data_many(patches);
    }

    /// Merge all entries of `other` onto `self` with overwrite (`ComposeOp::Set`)
    /// semantics on data maps, reusing one [`Patched`] allocation across chord
    /// haps that share a uniform-key [`Table`] or [`Patched`] overlay.
    pub(crate) fn merge_set_data_cached(&mut self, other: &Self, cache: &mut SharedWaveCache) {
        if other.is_empty() {
            return;
        }
        let other_len = other.len();
        if other_len <= MAX_PATCHES {
            match &mut self.entries {
                Entries::Dressed(table, row) if table.has_uniform_keys() => {
                    let row = *row;
                    if let Some(cached) = &cache.dressed
                        && Arc::ptr_eq(&cached.base, table)
                    {
                        self.entries = Entries::Patched(Arc::clone(cached), row);
                        return;
                    }
                    if let Some(mut patched) = Patched::empty(Arc::clone(table), row, true) {
                        for (key, value) in other.pairs() {
                            let ok = patched.try_set(row, key, value.clone(), true).is_ok();
                            debug_assert!(ok);
                        }
                        let arc = Arc::new(patched);
                        cache.dressed = Some(Arc::clone(&arc));
                        self.entries = Entries::Patched(arc, row);
                        return;
                    }
                }
                Entries::Patched(prev, row) if prev.base.has_uniform_keys() => {
                    let row = *row;
                    if Arc::strong_count(prev) > 1 {
                        if let Some((cached_prev, cached_out)) = &cache.patched
                            && Arc::ptr_eq(cached_prev, prev)
                        {
                            self.entries = Entries::Patched(Arc::clone(cached_out), row);
                            return;
                        }
                        let extra = other
                            .pairs()
                            .filter(|(k, _)| !prev.has_patch_for(k))
                            .count();
                        if usize::from(prev.len) + extra <= MAX_PATCHES {
                            let mut next = (**prev).clone();
                            for (key, value) in other.pairs() {
                                let ok = next.try_set(row, key, value.clone(), true).is_ok();
                                debug_assert!(ok);
                            }
                            next.made = true;
                            let arc = Arc::new(next);
                            cache.patched = Some((Arc::clone(prev), Arc::clone(&arc)));
                            self.entries = Entries::Patched(arc, row);
                            return;
                        }
                    } else if let Some(own) = Arc::get_mut(prev) {
                        let extra = other.pairs().filter(|(k, _)| !own.has_patch_for(k)).count();
                        if usize::from(own.len) + extra <= MAX_PATCHES {
                            for (key, value) in other.pairs() {
                                let ok = own.try_set(row, key, value.clone(), true).is_ok();
                                debug_assert!(ok);
                            }
                            own.made = true;
                            return;
                        }
                    }
                }
                _ => {}
            }
        }
        self.merge(other, |_, value| value.clone());
        self.mark_made_of_data();
    }

    /// Set several numeric data controls in one pass, allocating at most once.
    /// Later entries in `patches` win over earlier ones of the same key while
    /// preserving insertion order.
    pub(crate) fn set_data_many(&mut self, patches: &[(&Key, f64)]) {
        match patches {
            [] => {}
            [(key, value)] => self.set_data(key, Value::F64(*value)),
            _ => {
                if patches.len() <= MAX_PATCHES {
                    match &mut self.entries {
                        Entries::Dressed(table, row) => {
                            let row = *row;
                            if let Some(mut patched) = Patched::empty(Arc::clone(table), row, true)
                            {
                                for (key, num) in patches {
                                    let _ = patched.try_set(row, key, Value::F64(*num), true);
                                }
                                self.entries = Entries::Patched(Arc::new(patched), row);
                                return;
                            }
                        }
                        Entries::Patched(patched, row) => {
                            let row = *row;
                            if let Some(own) = Arc::get_mut(patched) {
                                let mut extra = 0usize;
                                for (i, (key, _)) in patches.iter().enumerate() {
                                    if !own.has_patch_for(key)
                                        && !patches[..i].iter().any(|(prev, _)| prev == key)
                                    {
                                        extra += 1;
                                    }
                                }
                                if usize::from(own.len) + extra <= MAX_PATCHES {
                                    for (key, num) in patches {
                                        let _ = own.try_set(row, key, Value::F64(*num), true);
                                    }
                                    return;
                                }
                            }
                        }
                        _ => {}
                    }
                }
                let layout = self.layout();
                let base_len = layout.len();
                // For each patch, record whether it appends a new key after the
                // base entries, and skip earlier duplicates of a fresh key.
                let mut append_indices = [0usize; 8];
                let mut append_len = 0usize;
                for (i, (key, _)) in patches.iter().enumerate() {
                    if self.position(*key).is_some() {
                        continue;
                    }
                    if patches[..i].iter().any(|(prev, _)| prev == key) {
                        continue;
                    }
                    append_indices[append_len] = i;
                    append_len += 1;
                }
                let mut taken = std::mem::replace(&mut self.entries, Entries::Empty);
                let mut held = match &mut taken {
                    Entries::Shared(entries) | Entries::Made(entries) => {
                        match Arc::get_mut(entries) {
                            Some(own) => Held::Own(own),
                            None => Held::View(Layout::Plain(entries)),
                        }
                    }
                    Entries::Empty => Held::View(Layout::Plain(&[])),
                    Entries::Dressed(table, index) => Held::View(table.layout(*index)),
                    Entries::Patched(patched, row) => Held::View(Layout::Overlay(patched, *row)),
                };
                let count = base_len + append_len;
                let entries: Shares = (0..count)
                    .map(|index| {
                        if index < base_len {
                            let (key, value) = held.take(index);
                            if let Some((_, num)) = patches.iter().rev().find(|(k, _)| **k == key) {
                                (key, Value::F64(*num))
                            } else {
                                (key, value)
                            }
                        } else {
                            let patch_idx = append_indices[index - base_len];
                            let key = patches[patch_idx].0;
                            let (_, num) = patches
                                .iter()
                                .rev()
                                .find(|(k, _)| *k == key)
                                .expect("patch key is present");
                            (key.clone(), Value::F64(*num))
                        }
                    })
                    .collect();
                self.entries = Entries::Made(entries);
            }
        }
    }

    /// A map of one entry: the control of one value, which a graph holds
    /// for each step.
    pub(crate) fn of_one(key: Key, value: Value) -> Self {
        Self {
            entries: Entries::Shared(Arc::new([(key, value)])),
        }
    }

    pub fn from_entries(entries: impl IntoIterator<Item = (String, Value)>) -> Self {
        // The entries in one list first: a later entry of a key takes the
        // place of the value of that key.
        let mut held: Vec<(Key, Value)> = Vec::new();
        for (key, value) in entries {
            match held.iter_mut().find(|(name, _)| *name == *key) {
                Some((_, current)) => *current = value,
                None => held.push((Key::from(key), value)),
            }
        }
        Self::of(held.into())
    }

    pub fn insert(&mut self, key: String, value: Value) -> Option<Value> {
        self.insert_key(Key::from(key), value)
    }

    /// [`Self::insert`] of a key that another map or a control already
    /// holds, which the map then shares.
    pub fn insert_key(&mut self, key: Key, value: Value) -> Option<Value> {
        if let Entries::Patched(patched, row) = &mut self.entries
            && let row = *row
            && let Some(own) = Arc::get_mut(patched)
        {
            let len = usize::from(own.len);
            for slot in &mut own.entries[..len] {
                if let Some((k, v, _)) = slot
                    && *k == key
                {
                    own.made = false;
                    return Some(std::mem::replace(v, value));
                }
            }
            if len < MAX_PATCHES {
                let mark = Table::mark(key.as_str());
                let (old, replace) = if own.base.marks() & mark == 0 {
                    (None, u16::MAX)
                } else {
                    let layout = own.base.layout(row);
                    match layout.position(&key) {
                        Some(idx) => (Some(layout.at(idx).1.clone()), idx as u16),
                        None => (None, u16::MAX),
                    }
                };
                own.entries[len] = Some((key, value, replace));
                own.len += 1;
                if replace == u16::MAX {
                    own.appended += 1;
                } else {
                    own.has_replace = true;
                }
                own.marks |= mark;
                own.made = false;
                return old;
            }
        }
        if let Entries::Dressed(table, row) = &self.entries {
            let row = *row;
            let at = self.position(&key);
            let old = at.map(|idx| table.layout(row).at(idx).1.clone());
            if let Some(patched) = Patched::of_one(
                Arc::clone(table),
                row,
                key.clone(),
                value.clone(),
                at,
                false,
            ) {
                self.entries = Entries::Patched(Arc::new(patched), row);
                return old;
            }
            let entries = table.layout(row).with(&key, at, value);
            self.entries = Entries::Shared(entries);
            return old;
        }
        match self.position(&key) {
            Some(index) => Some(std::mem::replace(&mut self.entries_mut()[index].1, value)),
            None => {
                self.append((key, value));
                None
            }
        }
    }

    /// Vec-compatible spelling retained for state-control call sites.
    pub fn push(&mut self, entry: (String, Value)) {
        self.insert(entry.0, entry.1);
    }

    #[inline]
    pub fn get(&self, key: &str) -> Option<&Value> {
        match &self.entries {
            Entries::Empty => None,
            Entries::Shared(entries) | Entries::Made(entries) => Layout::Plain(entries).get(key),
            Entries::Dressed(table, index) => match table.marks() & Table::mark(key) {
                0 => None,
                _ => table.layout(*index).get(key),
            },
            Entries::Patched(patched, row) => patched.get(*row, key),
        }
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        let index = self.position(key)?;
        Some(&mut self.entries_mut()[index].1)
    }

    pub fn remove(&mut self, key: &str) -> Option<Value> {
        let at = self.position(key)?;
        let removed = std::mem::replace(&mut self.entries_mut()[at].1, Value::Undefined);
        // The entries but the one at `at`.
        self.rebuild(self.len() - 1, |entries, index| {
            entries.take(index + usize::from(index >= at))
        });
        Some(removed)
    }

    #[inline]
    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.layout().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.pairs().map(|(name, value)| (name.as_str(), value))
    }

    /// ECMAScript `Object.keys` order.
    pub fn js_entries(&self) -> Vec<(&str, &Value)> {
        let mut integer = Vec::new();
        let mut strings = Vec::new();
        for (position, (key, value)) in self.pairs().enumerate() {
            if let Some(index) = array_index(key) {
                integer.push((index, key.as_str(), value));
            } else {
                strings.push((position, key.as_str(), value));
            }
        }
        integer.sort_by_key(|(index, _, _)| *index);
        integer
            .into_iter()
            .map(|(_, key, value)| (key, value))
            .chain(strings.into_iter().map(|(_, key, value)| (key, value)))
            .collect()
    }
}

impl<'a> IntoIterator for &'a OrderedMap {
    type Item = (&'a Key, &'a Value);
    type IntoIter = Pairs<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.pairs()
    }
}

/// A key of an [`OrderedMap`]. Every copy of a map, and every map that a
/// control makes, shares its key's one allocation.
#[derive(Clone, Eq)]
pub struct Key(Arc<str>);

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        // Maps that share a key's allocation answer by pointer; only keys
        // from different allocations compare their text.
        Arc::ptr_eq(&self.0, &other.0) || self.0 == other.0
    }
}

impl Hash for Key {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // By text, so equal keys hash equal whether or not they share an
        // allocation.
        self.0.hash(state)
    }
}

impl Key {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::ops::Deref for Key {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<str> for Key {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl PartialEq<str> for Key {
    fn eq(&self, other: &str) -> bool {
        &*self.0 == other
    }
}

impl PartialEq<&str> for Key {
    fn eq(&self, other: &&str) -> bool {
        &*self.0 == *other
    }
}

impl From<String> for Key {
    fn from(key: String) -> Self {
        Self(Arc::from(key))
    }
}

impl From<&str> for Key {
    fn from(key: &str) -> Self {
        Self(Arc::from(key))
    }
}

impl From<Arc<str>> for Key {
    fn from(key: Arc<str>) -> Self {
        Self(key)
    }
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&*self.0, f)
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&*self.0, f)
    }
}

fn array_index(key: &str) -> Option<u32> {
    if key.is_empty() || (key.len() > 1 && key.starts_with('0')) {
        return None;
    }
    let value = key.parse::<u32>().ok()?;
    // 2^32 - 1 is not an array index in ECMAScript.
    (value != u32::MAX && value.to_string() == key).then_some(value)
}

/// A pattern transformer carried **as a value**.
///
/// Upstream reifies a function argument into `pure(func)`, so `every(4, rev)`
/// and `jux(x => x.fast(2))` put a callable inside a hap and apply it at query
/// time. The `Value` variant keeps the callable where strudel.cc puts it.
///
/// The kind decides purity. The two basic kinds:
///
/// * `Native` carries the same two typed views `native_combinator!` produces,
///   so applying it to a pure pattern is provably still pure.
/// * `Js` carries only an opaque callback id, so applying it can materialise
///   anything and the result is impure.
///
/// A JavaScript callback compiled to a native program keeps its callback id
/// as a value and gains a pure view as a transformer. See
/// [`FunctionRef::lowered_js`].
///
/// [`Value`] derives `Debug` and `PartialEq`, so both are implemented here by
/// **identity**: two closures are the same function only if they are the same
/// allocation, which is exactly what JavaScript's `===` says about functions.
#[derive(Clone)]
pub struct FunctionRef {
    id: u64,
    name: Option<std::sync::Arc<str>>,
    kind: FunctionKind,
    /// The first runtime that exposed this callable as metadata. An outer
    /// container must not replace the realm already owned by a nested value.
    runtime_settings: Option<crate::settings::RuntimeSettings>,
}

#[derive(Clone)]
enum FunctionKind {
    Native {
        pure: std::sync::Arc<
            dyn Fn(crate::purity::PurePattern) -> crate::purity::PurePattern + Send + Sync,
        >,
        any: std::sync::Arc<dyn Fn(crate::Pattern) -> crate::Pattern + Send + Sync>,
    },
    /// An opaque JavaScript function. Resolved through the callback host.
    Js(crate::CallbackId),
    /// A bounded native candidate whose exact JavaScript callable remains
    /// owned by the callback sidecar as the compatibility fallback.
    JsPatternIr {
        fallback: crate::CallbackId,
        program: crate::callback_ir::PatternTransformProgram,
    },
    /// A JavaScript callback whose source compiled to a native program that
    /// builds the same graph. As a transformer it has a pure view that runs
    /// the program while the program stands for the callback. As a value it
    /// is still the JavaScript function.
    LoweredJs {
        fallback: crate::CallbackId,
        program: crate::callback_ir::PatternTransformProgram,
    },
    /// A registered combinator with its leading arguments already bound -
    /// `every(4, fast(2))`. Native code, but only *provably* pure when those
    /// bound arguments are themselves pure, so the flag is carried rather than
    /// assumed.
    Registered {
        apply: std::sync::Arc<dyn Fn(crate::Pattern) -> crate::Pattern + Send + Sync>,
        provable_pure: bool,
    },
}

static NEXT_FUNCTION_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl FunctionRef {
    /// A transformer with both typed views, built from one body.
    ///
    /// Prefer [`crate::native_function!`], which instantiates a single body
    /// expression against both types so the two views cannot disagree.
    pub fn native(
        name: Option<&str>,
        pure: std::sync::Arc<
            dyn Fn(crate::purity::PurePattern) -> crate::purity::PurePattern + Send + Sync,
        >,
        any: std::sync::Arc<dyn Fn(crate::Pattern) -> crate::Pattern + Send + Sync>,
    ) -> Self {
        Self {
            id: NEXT_FUNCTION_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            name: name.map(std::sync::Arc::from),
            kind: FunctionKind::Native { pure, any },
            runtime_settings: None,
        }
    }

    /// A partially applied registered combinator, as `fast(2)` produces.
    ///
    /// `provable_pure` must be true only when the body is native AND every
    /// bound argument is pure; the caller knows both, this type does not.
    pub fn registered(
        name: &str,
        apply: std::sync::Arc<dyn Fn(crate::Pattern) -> crate::Pattern + Send + Sync>,
        provable_pure: bool,
    ) -> Self {
        Self {
            id: NEXT_FUNCTION_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            name: Some(std::sync::Arc::from(name)),
            kind: FunctionKind::Registered {
                apply,
                provable_pure,
            },
            runtime_settings: None,
        }
    }

    /// A JavaScript function, held only as an opaque id.
    pub fn js(id: crate::CallbackId) -> Self {
        Self {
            id: NEXT_FUNCTION_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            name: None,
            kind: FunctionKind::Js(id),
            runtime_settings: None,
        }
    }

    /// A JavaScript pattern transformer with a proven native candidate.
    pub fn js_pattern_ir(
        fallback: crate::CallbackId,
        program: crate::callback_ir::PatternTransformProgram,
    ) -> Self {
        Self {
            id: NEXT_FUNCTION_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            name: None,
            kind: FunctionKind::JsPatternIr { fallback, program },
            runtime_settings: None,
        }
    }

    /// A JavaScript callback with a native program that stands for it.
    ///
    /// The owning runtime issues the program only while it can prove that
    /// the program builds the graph the callback builds, and marks the
    /// program stale before that can change. While the program is current,
    /// the pure view runs it and never enters JavaScript. A stale function
    /// takes no pure view, and a pure graph built before asks the host for
    /// the callback. The ordinary view asks the host, which runs the program
    /// or the callback.
    pub fn lowered_js(
        fallback: crate::CallbackId,
        program: crate::callback_ir::PatternTransformProgram,
    ) -> Self {
        Self {
            id: NEXT_FUNCTION_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            name: None,
            kind: FunctionKind::LoweredJs { fallback, program },
            runtime_settings: None,
        }
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    fn attach_runtime_settings(&mut self, settings: crate::settings::RuntimeSettings) {
        // A function imported from another runtime keeps that inner realm;
        // wrapping its container changes neither identity nor module scope.
        if self.runtime_settings.is_none() {
            self.runtime_settings = Some(settings);
        }
    }

    /// True when this has a pure view as a transformer: applying that view
    /// cannot introduce JavaScript.
    ///
    /// A lowered JavaScript callback is native here while its program stands
    /// for it, and still carries its callback id as a value. See
    /// [`Value::has_js_function`].
    pub fn is_native(&self) -> bool {
        match &self.kind {
            FunctionKind::Native { .. } => true,
            FunctionKind::LoweredJs { program, .. } => program.is_current(),
            FunctionKind::Registered { provable_pure, .. } => *provable_pure,
            FunctionKind::Js(_) | FunctionKind::JsPatternIr { .. } => false,
        }
    }

    /// Whether this is a JavaScript callback with its compiled program: the
    /// one function value a callback program may hold, in its registration's
    /// function slot. The host owns the choice between the program and the
    /// callback, in either kind.
    pub(crate) fn carries_pattern_program(&self) -> bool {
        matches!(
            self.kind,
            FunctionKind::LoweredJs { .. } | FunctionKind::JsPatternIr { .. }
        )
    }

    /// The script callback a pure view of this function may still need: a
    /// lowered callback whose program can go stale. See
    /// [`crate::Pattern::retaining_callbacks`].
    pub(crate) fn callback_kept_by_pure_view(&self) -> Option<crate::CallbackId> {
        match &self.kind {
            FunctionKind::LoweredJs { fallback, program } if program.can_go_stale() => {
                Some(*fallback)
            }
            _ => None,
        }
    }

    /// Every script callback a wrapper owning this function must keep alive:
    /// the fallback a lowered callback needs once its program can go stale,
    /// the fallback a pattern-IR callback needs at every application, and -
    /// for either kind - the same set for every function-valued argument the
    /// program holds, which can go stale on its own while the program stands.
    ///
    /// A closure that captures a function value cannot show it to purity, so
    /// [`crate::Pattern::retaining_callbacks`] is the only channel these ids
    /// reach a wrapper's harvest through.
    pub(crate) fn retained_callbacks(&self) -> Vec<crate::CallbackId> {
        let mut ids = Vec::new();
        match &self.kind {
            FunctionKind::Js(id) => ids.push(*id),
            FunctionKind::JsPatternIr { fallback, program } => {
                ids.push(*fallback);
                ids.extend(program.retained_callback_ids());
            }
            FunctionKind::LoweredJs { fallback, program } => {
                if program.can_go_stale() {
                    ids.push(*fallback);
                }
                ids.extend(program.retained_callback_ids());
            }
            FunctionKind::Native { .. } | FunctionKind::Registered { .. } => {}
        }
        ids
    }

    /// Whether a later application may have to differ from this one: a
    /// lowered callback whose program can go stale must reach its script
    /// callback again once it does, so applying it once does not stand for
    /// applying it at every query.
    pub(crate) fn can_go_stale(&self) -> bool {
        self.callback_kept_by_pure_view().is_some()
    }

    /// Whether applying this once stands for applying it at every query: a
    /// function with a native view that cannot go stale always applies the
    /// same way, so a combinator may apply it once while it builds its
    /// graph. A JavaScript callback, a host-bound transformer, or a lowered
    /// callback that can go stale does not: those applications run at query
    /// time, once per carrier hap, and a program that stops standing for its
    /// callback must reach the callback again.
    pub(crate) fn stands_for_every_application(&self) -> bool {
        self.is_native() && !self.can_go_stale()
    }

    /// The callback id, for a JS function.
    pub fn callback_id(&self) -> Option<crate::CallbackId> {
        match self.kind {
            FunctionKind::Js(id) => Some(id),
            FunctionKind::JsPatternIr { fallback, .. }
            | FunctionKind::LoweredJs { fallback, .. } => Some(fallback),
            _ => None,
        }
    }

    /// Apply to an arbitrary pattern.
    pub fn apply(&self, pattern: crate::Pattern) -> crate::Pattern {
        let owner = self
            .runtime_settings
            .clone()
            .or_else(crate::settings::RuntimeSettings::current_requested);
        if let Some(settings) = owner {
            return settings.with(|| {
                let mut output = self.apply_unbound(pattern);
                output.force_runtime_settings(settings.clone());
                output
            });
        }
        self.apply_unbound(pattern)
    }

    /// Invoke a JavaScript callable as a value-to-value function.
    ///
    /// A bridged function may be used in more than one authored role. Pattern
    /// transformers use [`Self::apply`]; extension nodes implementing generic
    /// predicates use this value boundary. Native/registered pattern
    /// transformers have no value-call contract and report a query error.
    pub fn apply_value(&self, value: &Value) -> Value {
        let owner = self
            .runtime_settings
            .clone()
            .or_else(crate::settings::RuntimeSettings::current_requested);
        if let Some(settings) = owner {
            return settings.with(|| self.apply_value_unbound(value));
        }
        self.apply_value_unbound(value)
    }

    fn apply_value_unbound(&self, value: &Value) -> Value {
        match &self.kind {
            FunctionKind::Js(id) => crate::host_call_value(*id, value),
            FunctionKind::JsPatternIr { fallback, .. }
            | FunctionKind::LoweredJs { fallback, .. } => crate::host_call_value(*fallback, value),
            FunctionKind::Native { .. } | FunctionKind::Registered { .. } => {
                crate::signal_query_error(|| {
                    "pattern transformer cannot be called as a value predicate".into()
                });
                Value::Undefined
            }
        }
    }

    /// Apply with no runtime settings bound to the result.
    pub(crate) fn apply_unbound(&self, pattern: crate::Pattern) -> crate::Pattern {
        match &self.kind {
            FunctionKind::Native { any, .. } => any(pattern),
            FunctionKind::Registered { apply, .. } => apply(pattern),
            FunctionKind::Js(id) => crate::host_call_pattern(*id, pattern),
            FunctionKind::JsPatternIr { fallback, program } => {
                crate::host_call_pattern_ir(*fallback, program, pattern)
            }
            // The host owns the choice between the program and the callback.
            // With no host, nothing can run the callback, and only a current
            // program stands for it.
            FunctionKind::LoweredJs { fallback, program } => {
                if crate::host_installed() {
                    crate::host_call_pattern_ir(*fallback, program, pattern)
                } else if program.is_current() {
                    program.apply(pattern)
                } else {
                    stale_without_host(*fallback)
                }
            }
        }
    }

    /// Apply this callable to every `(pattern, index)` pair as one
    /// `echoWith` batch.
    ///
    /// JavaScript callbacks cross one dedicated host boundary because their
    /// raw return values must all be collected before `stack` reifies any of
    /// them. Native fixed-arity functions and reconstructed registered
    /// transformers already return Patterns, so JavaScript's extra argument is
    /// ignored and the same unary body is applied in ascending index order.
    pub fn apply_indexed_batch(&self, patterns: Vec<(crate::Pattern, i64)>) -> Vec<crate::Pattern> {
        let owner = self
            .runtime_settings
            .clone()
            .or_else(crate::settings::RuntimeSettings::current_requested);
        if let Some(settings) = owner {
            return settings.with(|| {
                let mut out = self.apply_indexed_batch_unbound(patterns);
                for pattern in &mut out {
                    pattern.force_runtime_settings(settings.clone());
                }
                out
            });
        }
        self.apply_indexed_batch_unbound(patterns)
    }

    fn apply_indexed_batch_unbound(
        &self,
        patterns: Vec<(crate::Pattern, i64)>,
    ) -> Vec<crate::Pattern> {
        if let Some(id) = match self.kind {
            FunctionKind::Js(id) => Some(id),
            FunctionKind::JsPatternIr { fallback, .. } => Some(fallback),
            FunctionKind::LoweredJs { fallback, .. } if crate::host_installed() => Some(fallback),
            _ => None,
        } {
            return crate::host_call_pattern_indexed_batch(id, patterns);
        }

        let mut out = Vec::new();
        if out.try_reserve_exact(patterns.len()).is_err() {
            return vec![crate::query_limit_pattern(crate::QueryLimit::HostMemory)];
        }
        // The JavaScript batch calls the callback for every pattern, and the
        // callback ignores the index it is handed.
        if let FunctionKind::LoweredJs { fallback, program } = &self.kind {
            if !program.is_current() {
                return vec![stale_without_host(*fallback)];
            }
            out.extend(
                patterns
                    .into_iter()
                    .map(|(pattern, _)| program.apply(pattern)),
            );
            return out;
        }
        for (pattern, _) in patterns {
            let transformed = match &self.kind {
                FunctionKind::Native { any, .. } => any(pattern),
                FunctionKind::Registered { apply, .. } => apply(pattern),
                FunctionKind::Js(_)
                | FunctionKind::JsPatternIr { .. }
                | FunctionKind::LoweredJs { .. } => {
                    unreachable!("handled above")
                }
            };
            out.push(transformed);
            if crate::query_error_pending() {
                break;
            }
        }
        out
    }

    /// Apply to a pure pattern, keeping the purity proof.
    ///
    /// `None` for a JS function: there is no sound way to promise purity for
    /// something that can materialise anything. Callers must fall back to
    /// [`Self::apply`], which yields an impure pattern.
    pub fn apply_pure(
        &self,
        pattern: crate::purity::PurePattern,
    ) -> Option<crate::purity::PurePattern> {
        let owner = self
            .runtime_settings
            .clone()
            .or_else(crate::settings::RuntimeSettings::current_requested);
        if let Some(settings) = owner {
            return settings.with(|| {
                self.apply_pure_unbound(pattern).map(|pattern| {
                    let mut pattern = pattern.pattern().clone();
                    pattern.force_runtime_settings(settings.clone());
                    crate::purity::PurePattern::assert_pure(pattern)
                })
            });
        }
        self.apply_pure_unbound(pattern)
    }

    /// The pure view with no runtime settings bound to the result.
    pub(crate) fn apply_pure_unbound(
        &self,
        pattern: crate::purity::PurePattern,
    ) -> Option<crate::purity::PurePattern> {
        match &self.kind {
            FunctionKind::Native { pure, .. } => Some(pure(pattern)),
            FunctionKind::LoweredJs { fallback, program } => Some(if program.is_current() {
                // A construction kept with what the program built answers
                // only while the program stands for its callback.
                crate::callback_ir::note_read(program.standings());
                program.apply_pure(pattern)
            } else if crate::host_installed() {
                crate::purity::PurePattern::host_built(crate::host_call_pattern_ir(
                    *fallback,
                    program,
                    pattern.pattern().clone(),
                ))
            } else {
                crate::purity::PurePattern::assert_pure(stale_without_host(*fallback))
            }),
            FunctionKind::Registered {
                apply,
                provable_pure,
            } => provable_pure
                .then(|| crate::purity::PurePattern::assert_pure(apply(pattern.pattern().clone()))),
            FunctionKind::Js(_) | FunctionKind::JsPatternIr { .. } => None,
        }
    }

    /// Pure counterpart of [`Self::apply_indexed_batch`].
    ///
    /// A JavaScript callback has no such view; returning `None` is the same
    /// representation-level proof used by unary transformer application.
    pub fn apply_pure_indexed_batch(
        &self,
        patterns: Vec<(crate::purity::PurePattern, i64)>,
    ) -> Option<Vec<crate::purity::PurePattern>> {
        let owner = self
            .runtime_settings
            .clone()
            .or_else(crate::settings::RuntimeSettings::current_requested);
        if let Some(settings) = owner {
            return settings.with(|| {
                self.apply_pure_indexed_batch_unbound(patterns)
                    .map(|mut out| {
                        for pattern in &mut out {
                            let mut owned = pattern.pattern().clone();
                            owned.force_runtime_settings(settings.clone());
                            *pattern = crate::purity::PurePattern::assert_pure(owned);
                        }
                        out
                    })
            });
        }
        self.apply_pure_indexed_batch_unbound(patterns)
    }

    fn apply_pure_indexed_batch_unbound(
        &self,
        patterns: Vec<(crate::purity::PurePattern, i64)>,
    ) -> Option<Vec<crate::purity::PurePattern>> {
        let mut out = Vec::new();
        if out.try_reserve_exact(patterns.len()).is_err() {
            return Some(vec![crate::purity::PurePattern::assert_pure(
                crate::query_limit_pattern(crate::QueryLimit::HostMemory),
            )]);
        }
        match &self.kind {
            FunctionKind::Native { pure, .. } => {
                for (pattern, _) in patterns {
                    out.push(pure(pattern));
                    if crate::query_error_pending() {
                        break;
                    }
                }
                Some(out)
            }
            FunctionKind::Registered {
                apply,
                provable_pure,
            } if *provable_pure => {
                for (pattern, _) in patterns {
                    out.push(crate::purity::PurePattern::assert_pure(apply(
                        pattern.pattern().clone(),
                    )));
                    if crate::query_error_pending() {
                        break;
                    }
                }
                Some(out)
            }
            // The callback ignores the index it would be handed, so the unary
            // program stands for each call. The JavaScript batch calls the
            // callback for every pattern, whatever an earlier call signaled.
            FunctionKind::LoweredJs { program, .. } if program.is_current() => {
                crate::callback_ir::note_read(program.standings());
                out.extend(
                    patterns
                        .into_iter()
                        .map(|(pattern, _)| program.apply_pure(pattern)),
                );
                Some(out)
            }
            FunctionKind::LoweredJs { fallback, .. } => Some(if crate::host_installed() {
                crate::host_call_pattern_indexed_batch(
                    *fallback,
                    patterns
                        .into_iter()
                        .map(|(pattern, index)| (pattern.pattern().clone(), index))
                        .collect(),
                )
                .into_iter()
                .map(crate::purity::PurePattern::host_built)
                .collect()
            } else {
                vec![crate::purity::PurePattern::assert_pure(stale_without_host(
                    *fallback,
                ))]
            }),
            FunctionKind::Registered { .. }
            | FunctionKind::Js(_)
            | FunctionKind::JsPatternIr { .. } => None,
        }
    }
}

/// What a stale lowered callback answers when no host can run its
/// JavaScript. The runtime that issued the program queries every graph that
/// can reach it through its host, so only a query outside that runtime gets
/// here.
fn stale_without_host(fallback: crate::CallbackId) -> crate::Pattern {
    crate::signal_query_error(move || {
        format!(
            "pattern callback {fallback} needs its script host, because a method it calls changed after it was compiled"
        )
    });
    crate::silence()
}

impl fmt::Debug for FunctionRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.kind, &self.name) {
            (FunctionKind::Native { .. }, Some(name)) => write!(f, "fn {name}#{}", self.id),
            (FunctionKind::Native { .. }, None) => write!(f, "fn #{}", self.id),
            (FunctionKind::Registered { .. }, Some(name)) => write!(f, "fn {name}#{}", self.id),
            (FunctionKind::Registered { .. }, None) => write!(f, "fn #{}", self.id),
            (FunctionKind::Js(callback), _) => write!(f, "js fn #{} (cb {callback})", self.id),
            (FunctionKind::JsPatternIr { fallback, program }, _) => write!(
                f,
                "js fn #{} (cb {fallback}, ir v{}:{} ops)",
                self.id,
                program.version(),
                program.len()
            ),
            (FunctionKind::LoweredJs { fallback, program }, _) => write!(
                f,
                "js fn #{} (cb {fallback}, native ir v{}:{} ops)",
                self.id,
                program.version(),
                program.len()
            ),
        }
    }
}

/// True when `text` is a [`FunctionRef`] rendered by the `Debug` impl above.
///
/// A hap can legitimately carry a function - it is the intermediate of a
/// join - and the rendering keeps its identity rather than dropping it. Code
/// further down has to tell that apart from a note name, and this sits beside
/// the impl that writes it so the two cannot drift.
pub fn is_rendered_function(text: &str) -> bool {
    text.starts_with("fn ") || text.starts_with("js fn ")
}

/// Function identity, as JavaScript's `===` defines it.
impl PartialEq for FunctionRef {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

/// Builds a [`FunctionRef`] from ONE body expression, instantiated against both
/// purity views - the same trick `native_combinator!` uses, for the same
/// reason: two hand-written bodies could disagree, and one cannot.
#[macro_export]
macro_rules! native_function {
    ($name:expr, |$pat:ident| $body:expr) => {
        $crate::value::FunctionRef::native(
            ::std::option::Option::Some($name),
            ::std::sync::Arc::new(|$pat: $crate::purity::PurePattern| $body),
            ::std::sync::Arc::new(|$pat: $crate::Pattern| $body),
        )
    };
}

/// A Pattern held as an ordinary JavaScript value.
///
/// The graph itself is pure Rust and remains `Send + Sync`; `id` supplies the
/// reference identity JavaScript uses for `===`. Callback cells still live in
/// L2 and are reached only through the graph's opaque callback ids.
#[derive(Clone)]
pub struct PatternValue {
    id: crate::CallbackId,
    pattern: crate::Pattern,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JsValueRef {
    id: crate::CallbackId,
    array: bool,
}

impl JsValueRef {
    pub fn new(id: crate::CallbackId, array: bool) -> Self {
        Self { id, array }
    }

    pub fn id(self) -> crate::CallbackId {
        self.id
    }

    pub fn is_array(self) -> bool {
        self.array
    }
}

impl PatternValue {
    pub fn new(id: crate::CallbackId, pattern: crate::Pattern) -> Self {
        Self { id, pattern }
    }

    pub fn pattern(&self) -> &crate::Pattern {
        &self.pattern
    }

    pub fn id(&self) -> crate::CallbackId {
        self.id
    }
}

impl fmt::Debug for PatternValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("PatternValue").field(&self.id).finish()
    }
}

impl PartialEq for PatternValue {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

#[derive(Clone)]
pub struct HapList(Arc<Vec<crate::Hap>>);

impl HapList {
    pub fn new(haps: Vec<crate::Hap>) -> Self {
        Self(Arc::new(haps))
    }

    pub fn as_slice(&self) -> &[crate::Hap] {
        &self.0
    }
}

impl fmt::Debug for HapList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("HapList").field(&self.0.len()).finish()
    }
}

impl PartialEq for HapList {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Maximum byte length stored inline in [`SharedStr`] with zero heap allocation.
const INLINE_STR_MAX: usize = 14;

/// A compact immutable string value: up to 14 UTF-8 bytes inline in 16 bytes,
/// spilling to a shared heap allocation for longer strings.
#[derive(Clone)]
pub struct SharedStr(SharedStrRepr);

#[derive(Clone)]
enum SharedStrRepr {
    Inline {
        len: u8,
        bytes: [u8; INLINE_STR_MAX],
    },
    Heap(Arc<Box<str>>),
}

const _: () = assert!(std::mem::size_of::<SharedStr>() == 16);

impl SharedStr {
    #[inline]
    pub fn as_str(&self) -> &str {
        match &self.0 {
            SharedStrRepr::Inline { len, bytes } => {
                // Inline bytes are always copied from valid UTF-8 `&str`.
                std::str::from_utf8(&bytes[..usize::from(*len)]).unwrap_or("")
            }
            SharedStrRepr::Heap(text) => text,
        }
    }

    #[inline]
    pub fn to_arc(&self) -> Arc<str> {
        Arc::from(self.as_str())
    }
}

impl Default for SharedStr {
    #[inline]
    fn default() -> Self {
        Self(SharedStrRepr::Inline {
            len: 0,
            bytes: [0; INLINE_STR_MAX],
        })
    }
}

impl std::ops::Deref for SharedStr {
    type Target = str;

    #[inline]
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for SharedStr {
    #[inline]
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::borrow::Borrow<str> for SharedStr {
    #[inline]
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq for SharedStr {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (SharedStrRepr::Heap(a), SharedStrRepr::Heap(b)) if Arc::ptr_eq(a, b) => true,
            _ => self.as_str() == other.as_str(),
        }
    }
}

impl Eq for SharedStr {}

impl PartialOrd for SharedStr {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SharedStr {
    #[inline]
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl Hash for SharedStr {
    #[inline]
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl PartialEq<str> for SharedStr {
    #[inline]
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for SharedStr {
    #[inline]
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<String> for SharedStr {
    #[inline]
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<Arc<str>> for SharedStr {
    #[inline]
    fn eq(&self, other: &Arc<str>) -> bool {
        self.as_str() == &**other
    }
}

impl fmt::Debug for SharedStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl fmt::Display for SharedStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.as_str(), f)
    }
}

impl From<&str> for SharedStr {
    #[inline]
    fn from(s: &str) -> Self {
        if s.len() <= INLINE_STR_MAX {
            let mut bytes = [0u8; INLINE_STR_MAX];
            bytes[..s.len()].copy_from_slice(s.as_bytes());
            Self(SharedStrRepr::Inline {
                len: s.len() as u8,
                bytes,
            })
        } else {
            Self(SharedStrRepr::Heap(Arc::new(Box::<str>::from(s))))
        }
    }
}

impl From<String> for SharedStr {
    #[inline]
    fn from(s: String) -> Self {
        if s.len() <= INLINE_STR_MAX {
            Self::from(s.as_str())
        } else {
            Self(SharedStrRepr::Heap(Arc::new(s.into_boxed_str())))
        }
    }
}

impl From<Box<str>> for SharedStr {
    #[inline]
    fn from(s: Box<str>) -> Self {
        if s.len() <= INLINE_STR_MAX {
            Self::from(&*s)
        } else {
            Self(SharedStrRepr::Heap(Arc::new(s)))
        }
    }
}

impl From<Arc<str>> for SharedStr {
    #[inline]
    fn from(s: Arc<str>) -> Self {
        Self::from(&*s)
    }
}

impl From<&String> for SharedStr {
    #[inline]
    fn from(s: &String) -> Self {
        Self::from(s.as_str())
    }
}

impl From<std::borrow::Cow<'_, str>> for SharedStr {
    #[inline]
    fn from(s: std::borrow::Cow<'_, str>) -> Self {
        match s {
            std::borrow::Cow::Borrowed(b) => Self::from(b),
            std::borrow::Cow::Owned(o) => Self::from(o),
        }
    }
}

impl From<SharedStr> for Arc<str> {
    #[inline]
    fn from(s: SharedStr) -> Self {
        s.to_arc()
    }
}

impl From<&SharedStr> for Arc<str> {
    #[inline]
    fn from(s: &SharedStr) -> Self {
        s.to_arc()
    }
}

impl From<SharedStr> for String {
    #[inline]
    fn from(s: SharedStr) -> Self {
        s.as_str().to_owned()
    }
}

/// A native representation of values that can cross the score's semantic
/// boundary. Arrays and objects are recursive; object order is observable.
#[derive(Debug, PartialEq)]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    F64(f64),
    /// Text: stored inline up to 14 bytes or shared on the heap.
    Str(SharedStr),
    List(Box<Vec<Value>>),
    Object(OrderedMap),
    /// A native Pattern nested in a JavaScript array/object value.
    Pattern(Box<PatternValue>),
    /// An array/object still owned by the JavaScript host. Core carries only
    /// this opaque id; reading or re-exposing it goes through CallbackHost.
    JsValue(JsValueRef),
    /// A pattern transformer. Upstream's `reify(func)` puts one of these in a
    /// hap; see [`FunctionRef`].
    Function(Box<FunctionRef>),
    /// Congruent haps carried by `collect()` until a score callback consumes
    /// them.
    Haps(HapList),
}

const _: () = assert!(std::mem::size_of::<Value>() == 24);
const _: () = assert!(std::mem::size_of::<Option<Value>>() == 24);

impl Value {
    /// Construct a `Value::List` from a `Vec<Value>`.
    #[inline]
    pub fn list(values: Vec<Value>) -> Self {
        Self::List(Box::new(values))
    }
}

impl Clone for Value {
    /// A number, flag, inline text or nullish value copies in place. The
    /// others own heap memory and clone out of line.
    #[inline]
    fn clone(&self) -> Self {
        match self {
            Self::Undefined => Self::Undefined,
            Self::Null => Self::Null,
            Self::Bool(value) => Self::Bool(*value),
            Self::F64(value) => Self::F64(*value),
            Self::Str(SharedStr(SharedStrRepr::Inline { len, bytes })) => {
                Self::Str(SharedStr(SharedStrRepr::Inline {
                    len: *len,
                    bytes: *bytes,
                }))
            }
            shared => shared.clone_shared(),
        }
    }
}

/// A fast hash for keys compared only within one process: one rotate,
/// xor and multiply per word. Not resistant to chosen collisions, which
/// cost a longer equality scan and never a wrong answer.
#[derive(Default)]
pub struct WordHasher(u64);

impl WordHasher {
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}

impl std::hash::Hasher for WordHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let (words, rest) = bytes.as_chunks::<8>();
        for word in words {
            self.add(u64::from_le_bytes(*word));
        }
        let mut tail = [0u8; 8];
        tail[..rest.len()].copy_from_slice(rest);
        self.add(u64::from_le_bytes(tail) ^ rest.len() as u64);
    }

    fn write_u8(&mut self, value: u8) {
        self.add(u64::from(value));
    }

    fn write_u64(&mut self, value: u64) {
        self.add(value);
    }

    fn write_usize(&mut self, value: usize) {
        self.add(value as u64);
    }
}

/// Feed a map's entries to `state`, numbers by their bits.
fn hash_data_entries(layout: Layout<'_>, state: &mut impl std::hash::Hasher) {
    fn slice(entries: &[(Key, Value)], state: &mut impl std::hash::Hasher) {
        for (key, value) in entries {
            key.hash(state);
            hash_data(value, state);
        }
    }
    layout.len().hash(state);
    match layout {
        Layout::Plain(entries) => slice(entries, state),
        Layout::Keyed(key, value, tail) => {
            key.hash(state);
            hash_data(value, state);
            slice(tail, state);
        }
        Layout::Split(head, tail) => {
            slice(head, state);
            slice(tail, state);
        }
        Layout::Patched(base, replace, key, value) => {
            slice(&base[..replace], state);
            key.hash(state);
            hash_data(value, state);
            slice(base.get(replace + 1..).unwrap_or(&[]), state);
        }
        Layout::KeyedExtra(hk, hv, tail, ek, ev) => {
            hk.hash(state);
            hash_data(hv, state);
            slice(tail, state);
            ek.hash(state);
            hash_data(ev, state);
        }
        Layout::Overlay(patched, row) => {
            for index in 0..patched.len() {
                let (key, value) = patched.at(row, index);
                key.hash(state);
                hash_data(value, state);
            }
        }
    }
}

/// Feed a data value to `state`, numbers by their bits.
fn hash_data(value: &Value, state: &mut impl std::hash::Hasher) {
    match value {
        Value::Undefined => 0u8.hash(state),
        Value::Null => 1u8.hash(state),
        Value::Bool(value) => (2u8, value).hash(state),
        Value::F64(value) => (3u8, value.to_bits()).hash(state),
        Value::Str(value) => (4u8, value).hash(state),
        Value::List(items) => {
            (5u8, items.len()).hash(state);
            for item in items.iter() {
                hash_data(item, state);
            }
        }
        Value::Object(object) => {
            6u8.hash(state);
            hash_data_entries(object.layout(), state);
        }
        Value::Pattern(_) | Value::JsValue(_) | Value::Function(_) | Value::Haps(_) => {
            7u8.hash(state)
        }
    }
}

/// Whether two data values are the same down to the bits of every number.
fn same_data(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::F64(a), Value::F64(b)) => a.to_bits() == b.to_bits(),
        (Value::List(a), Value::List(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| same_data(a, b))
        }
        (Value::Object(a), Value::Object(b)) => a.same_data(b),
        _ => a == b,
    }
}

/// [`same_data`] for the entries of two maps, in order.
fn same_entries(a: Pairs<'_>, b: Pairs<'_>) -> bool {
    a.len() == b.len()
        && a.zip(b)
            .all(|((a_key, a), (b_key, b))| a_key == b_key && same_data(a, b))
}

impl Value {
    #[inline(never)]
    fn clone_shared(&self) -> Self {
        match self {
            Self::Undefined => Self::Undefined,
            Self::Null => Self::Null,
            Self::Bool(value) => Self::Bool(*value),
            Self::F64(value) => Self::F64(*value),
            Self::Str(value) => Self::Str(value.clone()),
            Self::List(values) => Self::List(values.clone()),
            Self::Object(values) => Self::Object(values.clone()),
            Self::Pattern(value) => Self::Pattern(value.clone()),
            Self::JsValue(value) => Self::JsValue(*value),
            Self::Function(value) => Self::Function(value.clone()),
            Self::Haps(value) => Self::Haps(value.clone()),
        }
    }
}

/// Whether a value contains metadata that [`Value::attach_runtime_settings`]
/// would change. A primitive object does not need a copy-on-write fork.
fn needs_runtime_settings(value: &Value, depth: u32) -> bool {
    if depth > crate::MAX_PATTERN_DEPTH {
        // Let the attach pass report the depth refusal.
        return true;
    }
    match value {
        Value::Pattern(_) | Value::Function(_) => true,
        Value::List(values) => values
            .iter()
            .any(|value| needs_runtime_settings(value, depth.saturating_add(1))),
        // An empty map holds nothing, and a made map was made of data
        // values only: data binds no runtime.
        Value::Object(values) if values.is_known_data() => false,
        Value::Object(values) => values
            .iter()
            .any(|(_, value)| needs_runtime_settings(value, depth.saturating_add(1))),
        Value::Haps(haps) => haps
            .as_slice()
            .iter()
            .any(|hap| needs_runtime_settings(&hap.value, depth.saturating_add(1))),
        Value::Undefined
        | Value::Null
        | Value::Bool(_)
        | Value::F64(_)
        | Value::Str(_)
        | Value::JsValue(_) => false,
    }
}

impl Value {
    /// Whether this is data only: no script object, pattern, function or hap
    /// list, each of which a query resolves anew.
    pub(crate) fn is_data(&self, depth: u32) -> bool {
        if depth > crate::MAX_PATTERN_DEPTH {
            return false;
        }
        match self {
            Value::Undefined | Value::Null | Value::Bool(_) | Value::F64(_) | Value::Str(_) => true,
            Value::List(items) => items.iter().all(|item| item.is_data(depth + 1)),
            // An empty map holds nothing, and a made map was made of data
            // values only.
            Value::Object(object) if object.is_known_data() => true,
            Value::Object(object) => object.iter().all(|(_, item)| item.is_data(depth + 1)),
            Value::Pattern(_) | Value::JsValue(_) | Value::Function(_) | Value::Haps(_) => false,
        }
    }

    /// A hash of this data value, numbers by their bits. Values that are
    /// [the same data](Self::same_data) hash the same.
    pub(crate) fn data_hash(&self) -> u64 {
        let mut hasher = WordHasher::default();
        hash_data(self, &mut hasher);
        std::hash::Hasher::finish(&hasher)
    }

    /// Whether this data value and `other` are the same down to the bits of
    /// every number.
    pub(crate) fn same_data(&self, other: &Value) -> bool {
        same_data(self, other)
    }

    pub fn object(entries: impl IntoIterator<Item = (String, Value)>) -> Self {
        Self::Object(OrderedMap::from_entries(entries))
    }

    pub fn is_nullish(&self) -> bool {
        matches!(self, Self::Undefined | Self::Null)
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::F64(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&OrderedMap> {
        match self {
            Self::Object(value) => Some(value),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_object()?.get(key)
    }

    /// JavaScript string coercion used for primitive hap values. For arrays and
    /// objects this returns the compact `Hap.show(true)` representation.
    pub fn show(&self) -> String {
        match self {
            Self::Undefined => "undefined".into(),
            Self::Null => "null".into(),
            Self::Bool(value) => value.to_string(),
            Self::F64(value) => js_number(*value),
            Self::Str(value) => value.to_string(),
            Self::List(_) | Self::Object(_) => self.compact_json(),
            // `String(fn)` is the source text on strudel.cc; a stable identity is
            // more useful and never appears in a snapshot.
            Self::Function(f) => format!("{f:?}"),
            Self::Pattern(_) => "[object Object]".into(),
            Self::Haps(haps) => format!("[{}]", haps.0.len()),
            Self::JsValue(_) => crate::materialize_js_value(self).show(),
        }
    }

    /// `JSON.stringify`, for the JSON-compatible value shapes used by scores.
    ///
    /// A top-level `undefined` has no JSON text in JavaScript; this method
    /// returns `None` for that exact case. Undefined object properties are
    /// omitted, while undefined array entries become `null`.
    pub fn json_stringify(&self) -> Option<String> {
        if let Self::Str(value) = self {
            let mut out = String::with_capacity(value.len().saturating_add(2));
            write_json_quote(&mut out, value);
            return Some(out);
        }
        let mut out = String::new();
        write_json_value(&mut out, self, JsonPosition::Top).then_some(out)
    }

    /// Upstream `Hap.show(true)` performs:
    /// `JSON.stringify(v).slice(1,-1).replaceAll('"','').replaceAll(',',' ')`.
    pub fn compact_json(&self) -> String {
        let json = self.json_stringify().unwrap_or_else(|| "undefined".into());
        let inner = if matches!(self, Self::List(_) | Self::Object(_)) && json.len() >= 2 {
            &json[1..json.len() - 1]
        } else {
            &json
        };
        inner.replace('"', "").replace(',', " ")
    }

    /// Does this value carry a JavaScript function anywhere inside it?
    ///
    /// The purity machinery keys off this: a combinator body run under the
    /// `PurePattern` view must not be handed a callable it cannot classify, so
    /// the dispatcher checks the ARGUMENTS as well as the operand. A lowered
    /// callback counts here, because as a value it is the JavaScript
    /// function. Only a transformer role may use its pure view.
    pub fn has_js_function(&self) -> bool {
        match self {
            Self::Function(f) => !f.is_native() || f.callback_id().is_some(),
            // Even a native-pure inner pattern carries the identity of its
            // original JS wrapper. Keep it off every host-less fast path.
            Self::Pattern(_) => true,
            Self::Haps(haps) => haps.0.iter().any(|hap| hap.value.has_js_function()),
            Self::JsValue(_) => true,
            Self::List(items) => items.iter().any(Value::has_js_function),
            // An empty map holds nothing, and a made map was made of data
            // values only. Data holds no function.
            Self::Object(map) if map.is_known_data() => false,
            Self::Object(map) => map.iter().any(|(_, v)| v.has_js_function()),
            _ => false,
        }
    }

    /// Whether resolving this value requires the JavaScript host.
    ///
    /// Used at the outer query boundary so fully native control maps are not
    /// cloned recursively on every hap merely to discover that they contain
    /// no JS-owned identity.
    pub fn contains_js_value(&self) -> bool {
        match self {
            Self::JsValue(_) => true,
            Self::Haps(haps) => haps.0.iter().any(|hap| hap.value.contains_js_value()),
            Self::List(items) => items.iter().any(Value::contains_js_value),
            // An empty map holds nothing, and a made map was made of data
            // values only. Data holds nothing a host
            // owns.
            Self::Object(map) if map.is_known_data() => false,
            Self::Object(map) => map.iter().any(|(_, value)| value.contains_js_value()),
            _ => false,
        }
    }

    /// The transformer this value carries, if it is one.
    pub fn as_function(&self) -> Option<&FunctionRef> {
        match self {
            Self::Function(f) => Some(f),
            _ => None,
        }
    }

    /// Attach settings to nested executable metadata without allocating graph
    /// wrappers. Traversal is explicitly bounded because List/Object depth is
    /// user-controlled and Rust stack overflow aborts the process.
    pub(crate) fn attach_runtime_settings(
        &mut self,
        settings: &crate::settings::RuntimeSettings,
        depth: u32,
    ) -> bool {
        // Most values of a query are maps of controls that say they hold
        // data only: such a map has nothing to attach.
        if matches!(self, Self::Object(values) if values.is_known_data()) {
            return true;
        }
        if crate::cancellation_requested() || crate::query_deadline_expired() {
            return false;
        }
        if depth > crate::MAX_PATTERN_DEPTH {
            crate::refuse(crate::QueryLimit::GraphDepth {
                depth,
                limit: crate::MAX_PATTERN_DEPTH,
            });
            return false;
        }
        match self {
            Self::List(values) => {
                if !values
                    .iter()
                    .any(|value| needs_runtime_settings(value, depth + 1))
                {
                    return true;
                }
                for value in values.iter_mut() {
                    if !value.attach_runtime_settings(settings, depth + 1) {
                        return false;
                    }
                }
            }
            Self::Object(values) => {
                if !values
                    .iter()
                    .any(|(_, value)| needs_runtime_settings(value, depth + 1))
                {
                    return true;
                }
                for (_, value) in values.entries_mut() {
                    if !value.attach_runtime_settings(settings, depth + 1) {
                        return false;
                    }
                }
            }
            Self::Pattern(value) => {
                value.pattern.attach_runtime_settings(settings.clone());
            }
            Self::Function(function) => {
                function.attach_runtime_settings(settings.clone());
            }
            Self::Haps(haps) => {
                if !haps
                    .as_slice()
                    .iter()
                    .any(|hap| needs_runtime_settings(&hap.value, depth.saturating_add(1)))
                {
                    return true;
                }
                for hap in Arc::make_mut(&mut haps.0) {
                    if !hap
                        .value
                        .attach_runtime_settings(settings, depth.saturating_add(1))
                    {
                        return false;
                    }
                }
            }
            Self::Undefined
            | Self::Null
            | Self::Bool(_)
            | Self::F64(_)
            | Self::Str(_)
            | Self::JsValue(_) => {}
        }
        true
    }

    pub fn js_truthy(&self) -> bool {
        match self {
            Self::Undefined | Self::Null => false,
            Self::Bool(value) => *value,
            Self::F64(value) => *value != 0.0 && !value.is_nan(),
            Self::Str(value) => !value.is_empty(),
            // Functions are objects: always truthy.
            Self::List(_)
            | Self::Object(_)
            | Self::Function(_)
            | Self::Pattern(_)
            | Self::Haps(_)
            | Self::JsValue(_) => true,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.show())
    }
}

#[derive(Clone, Copy)]
enum JsonPosition {
    Top,
    Array,
    Object,
}

fn write_json_value(out: &mut String, value: &Value, position: JsonPosition) -> bool {
    match value {
        // `JSON.stringify` treats a function exactly as it treats `undefined`.
        Value::Undefined | Value::Function(_) => match position {
            JsonPosition::Top | JsonPosition::Object => false,
            JsonPosition::Array => {
                out.push_str("null");
                true
            }
        },
        Value::Pattern(_) => {
            out.push_str("{}");
            true
        }
        Value::Haps(haps) => {
            use std::fmt::Write;
            let _ = write!(out, "[{}]", haps.0.len());
            true
        }
        Value::JsValue(_) => write_json_value(out, &crate::materialize_js_value(value), position),
        Value::Null => {
            out.push_str("null");
            true
        }
        Value::Bool(value) => {
            out.push_str(if *value { "true" } else { "false" });
            true
        }
        Value::F64(value) if !value.is_finite() => {
            out.push_str("null");
            true
        }
        Value::F64(value) => {
            if *value == 0.0 {
                // JSON.stringify(-0) produces "0".
                out.push('0');
            } else {
                let mut buffer = dragonbox_ecma::Buffer::new();
                out.push_str(buffer.format_finite(*value));
            }
            true
        }
        Value::Str(value) => {
            write_json_quote(out, value);
            true
        }
        Value::List(values) => {
            out.push('[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    out.push(',');
                }
                // Array entries always have JSON text: values omitted at the
                // top level or in objects use JavaScript's `null` placeholder.
                write_json_value(out, value, JsonPosition::Array);
            }
            out.push(']');
            true
        }
        Value::Object(values) => {
            out.push('{');
            let mut first = true;
            for (key, value) in values.js_entries() {
                write_json_property(out, key, value, &mut first);
            }
            out.push('}');
            true
        }
    }
}

fn write_json_property(out: &mut String, key: &str, value: &Value, first: &mut bool) {
    match value {
        Value::Undefined | Value::Function(_) => return,
        Value::JsValue(_) => {
            // Resolve omission before writing the key: an omitted property's
            // key must not grow the output buffer, however long it is.
            write_json_property(out, key, &crate::materialize_js_value(value), first);
            return;
        }
        _ => {}
    }
    if !*first {
        out.push(',');
    }
    write_json_quote(out, key);
    out.push(':');
    write_json_value(out, value, JsonPosition::Object);
    *first = false;
}

fn js_number(value: f64) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value == f64::INFINITY {
        return "Infinity".into();
    }
    if value == f64::NEG_INFINITY {
        return "-Infinity".into();
    }
    if value == 0.0 {
        // String(-0) produces "0".
        return "0".into();
    }
    // Rust's Display uses different fixed/exponential thresholds and omits
    // ECMAScript's `+` on a positive exponent. This formatter implements the
    // Number::toString decimal form used by String(number), Hap.show and
    // JSON.stringify.
    let mut buffer = dragonbox_ecma::Buffer::new();
    buffer.format_finite(value).to_owned()
}

fn write_json_quote(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch <= '\u{001f}' => {
                use std::fmt::Write;
                let _ = write!(out, "\\u{:04x}", ch as u32);
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::{Key, OrderedMap, Table, Value};
    use std::sync::Arc;

    /// A table makes no map where a plain compose would not read the same.
    #[test]
    fn a_table_refuses_what_a_compose_would_read_otherwise() {
        let values: Arc<[Value]> = Arc::from([Value::Str("c3".into())]);
        let key = || Some(Key::from("note"));
        let data = OrderedMap::from_entries([("gain".to_string(), Value::F64(1.0))]);
        let no_data = OrderedMap::from_entries([(
            "gain".to_string(),
            Value::Haps(super::HapList::new(Vec::new())),
        )]);
        assert!(Table::new(&values, key(), Some(&data)).is_some());
        assert!(
            Table::new(&values, key(), Some(&no_data)).is_none(),
            "a set that is no data"
        );
        assert!(
            Table::new(&values, None, None).is_none(),
            "no key and no set"
        );
        assert!(
            Table::new(&values, None, Some(&data)).is_none(),
            "a value that is no map"
        );
        assert!(
            Table::new(&Arc::from([]), key(), Some(&data)).is_none(),
            "no values"
        );
    }

    /// A map that reads a shared table holds no more than a plain one.
    #[test]
    fn a_dressed_map_is_no_larger_than_a_plain_one() {
        assert_eq!(
            std::mem::size_of::<OrderedMap>(),
            std::mem::size_of::<super::Entries>()
        );
        assert_eq!(std::mem::size_of::<super::Entries>(), 24);
        assert_eq!(std::mem::size_of::<Value>(), 24);
    }

    #[test]
    fn a_later_entry_of_a_key_takes_the_place_of_its_value() {
        let map = OrderedMap::from_entries([
            ("s".to_string(), Value::F64(1.0)),
            ("n".to_string(), Value::F64(2.0)),
            ("s".to_string(), Value::F64(3.0)),
        ]);
        let entries: Vec<_> = map.iter().collect();
        assert_eq!(entries, [("s", &Value::F64(3.0)), ("n", &Value::F64(2.0))]);
    }

    #[test]
    fn object_keys_follow_ecmascript_order() {
        let value = Value::object([
            ("b".into(), Value::F64(1.0)),
            ("10".into(), Value::F64(10.0)),
            ("2".into(), Value::F64(2.0)),
            ("a".into(), Value::F64(3.0)),
        ]);
        assert_eq!(
            value.json_stringify().as_deref(),
            Some(r#"{"2":2,"10":10,"b":1,"a":3}"#)
        );
    }

    #[test]
    fn replacement_keeps_string_key_position() {
        let mut value = OrderedMap::new();
        value.insert("a".into(), Value::F64(1.0));
        value.insert("b".into(), Value::F64(2.0));
        value.insert("a".into(), Value::F64(3.0));
        assert_eq!(
            Value::Object(value).json_stringify().as_deref(),
            Some(r#"{"a":3,"b":2}"#)
        );
    }

    #[test]
    fn nested_compact_format_matches_hap_show_algorithm() {
        let value = Value::list(vec![
            Value::Str("c".into()),
            Value::Str("d".into()),
            Value::list(vec![Value::Str("e".into()), Value::Str("f".into())]),
        ]);
        assert_eq!(value.compact_json(), "c d [e f]");
    }

    #[test]
    fn json_handles_js_numeric_and_undefined_rules() {
        assert_eq!(
            Value::F64(f64::NAN).json_stringify().as_deref(),
            Some("null")
        );
        assert_eq!(Value::F64(-0.0).json_stringify().as_deref(), Some("0"));
        assert_eq!(
            Value::list(vec![Value::Undefined])
                .json_stringify()
                .as_deref(),
            Some("[null]")
        );
        assert_eq!(Value::Undefined.json_stringify(), None);
    }

    #[test]
    fn json_materializes_omitted_values_once_in_enumeration_order() {
        struct MaterializeHost {
            omitted: Value,
            calls: std::cell::RefCell<Vec<crate::CallbackId>>,
        }

        impl crate::CallbackHost for MaterializeHost {
            #[expect(
                clippy::panic_in_result_fn,
                reason = "this test double must panic when called"
            )]
            fn call_value(&self, id: crate::CallbackId, _: &Value) -> Result<Value, String> {
                panic!("unexpected value callback {id}")
            }

            #[expect(
                clippy::panic_in_result_fn,
                reason = "this test double must panic when called"
            )]
            fn call_query(
                &self,
                id: crate::CallbackId,
                _: &crate::State,
            ) -> Result<Vec<crate::Hap>, String> {
                panic!("unexpected query callback {id}")
            }

            #[expect(
                clippy::panic_in_result_fn,
                reason = "the unexpected id is a test failure"
            )]
            fn call_materialize_value(&self, id: crate::CallbackId) -> Result<Value, String> {
                self.calls.borrow_mut().push(id);
                Ok(match id {
                    1 | 3 | 5 => self.omitted.clone(),
                    2 | 4 => Value::F64(id as f64),
                    _ => panic!("unexpected materialization callback {id}"),
                })
            }
        }

        let opaque = |id| Value::JsValue(super::JsValueRef::new(id, false));
        // Integer keys move ahead of string keys. The resulting traversal
        // omits its first, middle, and last properties after materialization.
        let object = Value::object([
            ("middle".into(), opaque(3)),
            ("10".into(), opaque(2)),
            ("2".into(), opaque(1)),
            ("after".into(), opaque(4)),
            ("last".into(), opaque(5)),
        ]);
        let array = Value::list((1..=5).map(opaque).collect());
        let all_omitted = Value::object([("tail".into(), opaque(3)), ("2".into(), opaque(1))]);

        for omitted in [
            Value::Undefined,
            Value::Function(Box::new(super::FunctionRef::js(99))),
        ] {
            let host = MaterializeHost {
                omitted,
                calls: std::cell::RefCell::new(Vec::new()),
            };
            crate::with_callback_host(&host, || {
                assert_eq!(
                    object.json_stringify().as_deref(),
                    Some(r#"{"10":2,"after":4}"#)
                );
                assert_eq!(host.calls.take(), vec![1, 2, 3, 4, 5]);

                assert_eq!(
                    array.json_stringify().as_deref(),
                    Some("[null,2,null,4,null]")
                );
                assert_eq!(host.calls.take(), vec![1, 2, 3, 4, 5]);

                assert_eq!(all_omitted.json_stringify().as_deref(), Some("{}"));
                assert_eq!(host.calls.take(), vec![1, 3]);

                assert_eq!(opaque(1).json_stringify(), None);
                assert_eq!(host.calls.take(), vec![1]);

                let omitted_long_key = Value::object([("\"".repeat(16_384), opaque(3))]);
                let mut output = String::with_capacity(8);
                let capacity = output.capacity();
                assert!(super::write_json_value(
                    &mut output,
                    &omitted_long_key,
                    super::JsonPosition::Top
                ));
                assert_eq!(output, "{}");
                assert_eq!(output.capacity(), capacity);
                assert_eq!(host.calls.take(), vec![3]);
            });
        }
    }

    #[test]
    fn numeric_text_uses_ecmascript_thresholds() {
        let cases = [
            (1e20, "100000000000000000000"),
            (1e21, "1e+21"),
            (1e-6, "0.000001"),
            (1e-7, "1e-7"),
            (1.2345678901234568e30, "1.2345678901234568e+30"),
            (-0.0, "0"),
        ];
        for (value, expected) in cases {
            assert_eq!(Value::F64(value).show(), expected);
            assert_eq!(
                Value::F64(value).json_stringify().as_deref(),
                Some(expected)
            );
        }
    }

    #[test]
    fn a_lowered_callback_is_a_native_transformer_and_a_javascript_value() {
        let program = crate::callback_ir::PatternTransformProgram::new([]).unwrap();
        let lowered = super::FunctionRef::lowered_js(9, program);
        assert!(lowered.is_native());
        assert_eq!(lowered.callback_id(), Some(9));
        let value = Value::Function(Box::new(lowered.clone()));
        assert!(value.has_js_function());
        assert!(!crate::pure(value).is_pure());
        assert!(super::is_rendered_function(&format!("{lowered:?}")));
        assert!(format!("{lowered:?}").ends_with("(cb 9, native ir v13:0 ops)"));

        let pattern = crate::pure(Value::F64(4.0));
        let pure = crate::purity::PurePattern::assert_pure(pattern.clone());
        let transformed = lowered.apply_pure(pure).expect("a pure view");
        assert!(transformed.pattern().is_pure());
        // With no host installed, the program stands for the callback.
        assert_eq!(
            lowered.apply(pattern).query_arc(
                rustel_fraction::Fraction::ZERO,
                rustel_fraction::Fraction::ONE
            )[0]
            .value,
            Value::F64(4.0)
        );
    }

    #[test]
    fn a_stale_lowered_callback_leaves_the_pure_view_and_needs_its_host() {
        let standing = crate::callback_ir::Standing::new();
        let program = crate::callback_ir::PatternTransformProgram::new([])
            .unwrap()
            .with_standings([standing.clone()]);
        let lowered = super::FunctionRef::lowered_js(9, program);
        assert!(lowered.is_native());
        standing.mark_stale();
        assert!(!lowered.is_native(), "a stale program takes no pure view");
        assert!(Value::Function(Box::new(lowered.clone())).has_js_function());

        // A pure graph built before the change still applies it. With no
        // host, nothing can run the callback, so it answers with an error.
        let _ = crate::take_query_error();
        let pattern = crate::pure(Value::F64(4.0));
        let pure = crate::purity::PurePattern::assert_pure(pattern.clone());
        let applied = lowered.apply_pure(pure).expect("a pure view");
        assert!(
            applied
                .query_arc(
                    rustel_fraction::Fraction::ZERO,
                    rustel_fraction::Fraction::ONE
                )
                .is_empty()
        );
        let error = crate::take_query_error().expect("a query error");
        assert!(error.contains("needs its script host"), "{error}");
        assert!(
            lowered
                .apply(pattern)
                .query_arc(
                    rustel_fraction::Fraction::ZERO,
                    rustel_fraction::Fraction::ONE
                )
                .is_empty()
        );
        assert!(crate::take_query_error().is_some());
    }

    /// A set that holds no entry, in the form of a map that was emptied, is
    /// no set: the table reads as the plain map and refuses a bare step.
    #[test]
    fn an_emptied_set_counts_as_no_set() {
        for entries in [
            super::Entries::Shared(Arc::from([])),
            super::Entries::Made(Arc::from([])),
        ] {
            let emptied = OrderedMap { entries };
            assert!(emptied.is_empty());
            check_emptied(&emptied);
        }
    }

    fn check_emptied(emptied: &OrderedMap) {
        // A step with no entry has nothing to share.
        let bare: Arc<[Value]> = Arc::from([Value::Object(OrderedMap::new())]);
        assert!(Table::new(&bare, None, Some(emptied)).is_none());
        // A named value reads as the map of its one entry.
        let key = Key::from("note");
        let values: Arc<[Value]> = Arc::from([Value::F64(1.0)]);
        let table = Table::new(&values, Some(key.clone()), Some(emptied)).expect("a table");
        let read = Arc::new(table).map(0).expect("step 0");
        let plain = OrderedMap::of_one(key, Value::F64(1.0));
        assert_eq!(read, plain);
        assert_eq!(read.len(), 1);
        assert!(!read.owns_no_heap() && !plain.owns_no_heap());
    }

    /// A lookup of a name that no step holds finds nothing, and every name
    /// that a step holds is found, whatever its mark is.
    #[test]
    fn a_lookup_through_the_marks_finds_what_the_map_holds() {
        let names = [
            "s", "n", "note", "gain", "lpf", "lpq", "room", "delay", "distort", "pan", "orbit",
            "release", "sound", "speed", "shape", "cutoff", "decay", "vst", "vsti", "duration",
        ];
        let tail: Vec<(String, Value)> = names[4..]
            .iter()
            .map(|name| (name.to_string(), Value::F64(2.0)))
            .collect();
        let set = OrderedMap::from_entries(tail);
        let values: Arc<[Value]> = Arc::from([Value::F64(1.0)]);
        let table = Arc::new(Table::new(&values, Some(Key::from("note")), Some(&set)).unwrap());
        let map = table.map(0).unwrap();
        let plain = {
            let mut plain = OrderedMap::from_entries([("note".to_string(), Value::F64(1.0))]);
            for (key, value) in set.iter() {
                plain.insert(key.to_string(), value.clone());
            }
            plain
        };
        for name in names.iter().copied().chain(["x", "", "gains", "notes"]) {
            assert_eq!(map.get(name), plain.get(name), "{name}");
            assert_eq!(map.contains_key(name), plain.contains_key(name), "{name}");
        }
    }

    /// A merge of no entry changes nothing, and leaves a dressed map as it is.
    #[test]
    fn a_merge_of_an_empty_map_leaves_the_map() {
        let values: Arc<[Value]> = Arc::from([Value::F64(1.0)]);
        let set = OrderedMap::from_entries([("s".to_string(), Value::Str("x".into()))]);
        let table = Arc::new(Table::new(&values, Some(Key::from("note")), Some(&set)).unwrap());
        let mut map = table.map(0).unwrap();
        let before = map.clone();
        map.merge(&OrderedMap::new(), |_, other| other.clone());
        assert_eq!(map, before);
        assert!(matches!(map.entries, super::Entries::Dressed(..)));
    }
}
