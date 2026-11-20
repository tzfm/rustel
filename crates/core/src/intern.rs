/*
intern.rs - post-evaluation structural DAG deduplication for immutable pattern graphs
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! Post-evaluation structural deduplication for finalized native score graphs.
//!
//! Running once over the surviving root pattern (rather than during intermediate
//! builder chains) keeps `Arc::get_mut` in-place mutation working at full speed
//! during evaluation, skips tiny single-leaf scores with zero allocations, and
//! deduplicates `Arc<dyn BuiltinEntries>`, `Arc<Keys>`, `Arc<[Value]>`,
//! `Arc<Dress>`, `CompactGrid`, `Arc<Grid>`, `Arc<[Layer]>`, and pure
//! `Arc<NodeCell>` subgraphs across tracks and sections.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::sync::Arc;

use crate::context_keys::Keys;
use crate::sequence::{CompactGrid, Dress, Grid, Layer, LayerDeduper};
use crate::value::{
    BuiltinEntries, MAX_BUILTIN_MAP_LEN, Slice, Value, WordHasher, hash_data,
    make_pooled_builtin_arc,
};
use crate::{
    AppLeftNode, AppNode, AppWaveNode, CompactPickLookup, ConstantsMeetings, ConstantsNode,
    ConstructionMemo, DegradeKind, FastNode, HapBind, Node, NodeCell, OperandPureNode, Pattern,
    PatternExtras, PatternPair, PickNode, RunApply, RunMeeting, ShiftNode, TempoRate, UiVisualNode,
    ValueReading, WeightedChoice,
};

type FastSet<T> = HashSet<T, BuildHasherDefault<WordHasher>>;
type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<WordHasher>>;

/// Deduplicate immutable payloads and pure subgraphs across `root`, then remap
/// any additional node references (such as native memo parts) through the same
/// canonical node table so they share the deduplicated graph instead of pinning
/// old nodes.
pub fn dedup_score(
    root: &mut Pattern,
    remap_extras: impl FnOnce(&mut dyn FnMut(&mut crate::PatternNodeRef)),
) {
    // Tiny single-leaf scores have nothing to share across tracks or layers.
    match &root.node_arc().kind {
        Node::Silence | Node::Pure(_) => return,
        Node::Sequence(layers) if layers.len() <= 1 => return,
        _ => {}
    }
    let mut deduper = Deduper::default();
    deduper.dedup_pattern(root);
    if let Some(pooled) = deduper.build_pooled_builtins() {
        let mut pass2 = Deduper {
            pooled_builtins: pooled,
            ..Deduper::default()
        };
        pass2.dedup_pattern(root);
        for canon in deduper.visited.values_mut() {
            let id2 = Arc::as_ptr(canon) as usize;
            if let Some(final_canon) = pass2.visited.get(&id2) {
                *canon = Arc::clone(final_canon);
            }
        }
    }
    let mut remap = |node_ref: &mut crate::PatternNodeRef| {
        if let Some(canon) = deduper.visited.get(&node_ref.identity()) {
            node_ref.0 = Arc::clone(canon);
        }
    };
    remap_extras(&mut remap);
}

#[derive(Default)]
struct Deduper {
    visited: FastMap<usize, Arc<NodeCell>>,
    seen_values: FastMap<usize, Arc<[Value]>>,
    seen_dresses: FastMap<usize, Arc<Dress>>,
    seen_layers: FastMap<usize, Arc<[Layer]>>,
    plain_extras: FastMap<usize, Arc<PatternExtras>>,
    pooled_builtins: FastMap<usize, Arc<dyn BuiltinEntries>>,
    builtins: FastSet<InternedBuiltin>,
    keys: FastSet<InternedKeys>,
    values: FastSet<InternedValues>,
    dresses: FastSet<InternedDress>,
    compact_grids: FastSet<InternedCompactGrid>,
    grids: FastSet<InternedGrid>,
    layers: FastSet<InternedLayers>,
    nodes: FastSet<InternedNode>,
}

impl Deduper {
    fn intern_builtin(
        &mut self,
        entries: Arc<dyn BuiltinEntries>,
        known_data: bool,
    ) -> Arc<dyn BuiltinEntries> {
        let ptr = Arc::as_ptr(&entries) as *const () as usize;
        if let Some(pooled) = self.pooled_builtins.get(&ptr) {
            return Arc::clone(pooled);
        }
        if !known_data && !entries.as_slice().all_values_data() {
            return entries;
        }
        let key = InternedBuiltin(entries);
        if let Some(existing) = self.builtins.get(&key) {
            return Arc::clone(&existing.0);
        }
        let out = Arc::clone(&key.0);
        self.builtins.insert(key);
        out
    }

    /// Pool repeated control values across surviving unpooled built-in maps into
    /// one shared `Arc<[Value]>` whenever doing so strictly reduces retained bytes.
    #[allow(clippy::mutable_key_type)]
    fn build_pooled_builtins(&self) -> Option<FastMap<usize, Arc<dyn BuiltinEntries>>> {
        if self.builtins.len() < 4 {
            return None;
        }
        let unpooled: Vec<&Arc<dyn BuiltinEntries>> = self
            .builtins
            .iter()
            .filter_map(|map| match map.0.as_slice() {
                Slice::Builtin(keys, _) if keys.len() >= 2 && keys.len() <= MAX_BUILTIN_MAP_LEN => {
                    Some(&map.0)
                }
                _ => None,
            })
            .collect();
        if unpooled.len() < 4 {
            return None;
        }
        let mut freq: FastMap<DataValueRef<'_>, u32> = FastMap::default();
        for map in &unpooled {
            if let Slice::Builtin(_, values) = map.as_slice() {
                for val in values {
                    if val.is_data(0) {
                        *freq.entry(DataValueRef(val)).or_insert(0) += 1;
                    }
                }
            }
        }
        let mut poolable: Vec<&Arc<dyn BuiltinEntries>> = Vec::new();
        for map in &unpooled {
            if let Slice::Builtin(_, values) = map.as_slice() {
                let len = values.len();
                let mut shared = 0usize;
                for val in values {
                    if freq.get(&DataValueRef(val)).copied().unwrap_or(0) >= 2 {
                        shared += 1;
                    }
                }
                let unique = len - shared;
                if 22 * shared > 2 * unique + 16 {
                    poolable.push(*map);
                }
            }
        }
        if poolable.len() < 2 {
            return None;
        }
        let mut pool_index: FastMap<DataValueRef<'_>, u16> = FastMap::default();
        let mut pool_vec: Vec<Value> = Vec::new();
        let mut old_bytes = 0usize;
        let mut new_maps_bytes = 0usize;
        for map in &poolable {
            if let Slice::Builtin(_, values) = map.as_slice() {
                let len = values.len();
                old_bytes += 16 + ((26 * len + 7) & !7);
                new_maps_bytes += 32 + ((4 * len + 7) & !7);
                for val in values {
                    let key = DataValueRef(val);
                    if let std::collections::hash_map::Entry::Vacant(entry) = pool_index.entry(key)
                    {
                        let idx = u16::try_from(pool_vec.len()).ok()?;
                        pool_vec.push(val.clone());
                        entry.insert(idx);
                    }
                }
            }
        }
        let new_bytes = 16 + 24 * pool_vec.len() + new_maps_bytes;
        if new_bytes + 64 >= old_bytes {
            return None;
        }
        let pool: Arc<[Value]> = pool_vec.into();
        let mut out: FastMap<usize, Arc<dyn BuiltinEntries>> = FastMap::default();
        let mut vals_buf = [0u16; MAX_BUILTIN_MAP_LEN];
        for map in poolable {
            if let Slice::Builtin(keys, values) = map.as_slice() {
                let len = keys.len();
                for (i, val) in values.iter().enumerate() {
                    vals_buf[i] = pool_index[&DataValueRef(val)];
                }
                let pooled = make_pooled_builtin_arc(Arc::clone(&pool), keys, &vals_buf[..len]);
                out.insert(Arc::as_ptr(map) as *const () as usize, pooled);
            }
        }
        Some(out)
    }

    fn dedup_value(&mut self, value: &Value) -> Option<Value> {
        if let Value::Object(map) = value
            && map.as_builtin().is_some()
        {
            let mut next = map.clone();
            if next.dedup_builtin(|entries, made| self.intern_builtin(entries, made)) {
                return Some(Value::Object(next));
            }
        }
        None
    }

    fn dedup_values(&mut self, values: &Arc<[Value]>) -> Arc<[Value]> {
        if values.is_empty() {
            return Arc::clone(values);
        }
        let id = Arc::as_ptr(values) as *const Value as usize;
        if let Some(existing) = self.seen_values.get(&id) {
            return Arc::clone(existing);
        }
        let mut owned: Option<Vec<Value>> = None;
        for (idx, val) in values.iter().enumerate() {
            if let Some(next_val) = self.dedup_value(val) {
                let vec = owned.get_or_insert_with(|| values.to_vec());
                vec[idx] = next_val;
            }
        }
        let candidate: Arc<[Value]> = match owned {
            Some(vec) => vec.into(),
            None => Arc::clone(values),
        };
        let key = InternedValues(candidate);
        let canon = if let Some(existing) = self.values.get(&key) {
            Arc::clone(&existing.0)
        } else {
            let out = Arc::clone(&key.0);
            self.values.insert(key);
            out
        };
        self.seen_values.insert(id, Arc::clone(&canon));
        canon
    }

    fn dedup_dress(&mut self, dress: &Arc<Dress>) -> Arc<Dress> {
        let id = Arc::as_ptr(dress) as usize;
        if let Some(existing) = self.seen_dresses.get(&id) {
            return Arc::clone(existing);
        }
        let candidate = self.dedup_dress_inner(dress);
        let canon = if candidate.can_intern() {
            let key = InternedDress(candidate);
            if let Some(existing) = self.dresses.get(&key) {
                Arc::clone(&existing.0)
            } else {
                let out = Arc::clone(&key.0);
                self.dresses.insert(key);
                out
            }
        } else {
            candidate
        };
        self.seen_dresses.insert(id, Arc::clone(&canon));
        canon
    }

    fn dedup_dress_inner(&mut self, dress: &Arc<Dress>) -> Arc<Dress> {
        let Self {
            builtins,
            keys,
            pooled_builtins,
            ..
        } = self;
        let mut dedup_builtin = |entries: Arc<dyn BuiltinEntries>, known_data: bool| {
            let ptr = Arc::as_ptr(&entries) as *const () as usize;
            if let Some(pooled) = pooled_builtins.get(&ptr) {
                return Arc::clone(pooled);
            }
            if !known_data && !entries.as_slice().all_values_data() {
                return entries;
            }
            let key = InternedBuiltin(entries);
            if let Some(existing) = builtins.get(&key) {
                return Arc::clone(&existing.0);
            }
            let out = Arc::clone(&key.0);
            builtins.insert(key);
            out
        };
        let mut dedup_keys = |k: Arc<Keys>| {
            if k.scale_definition.is_some() || k.pick_lookup.is_some() {
                return k;
            }
            let key = InternedKeys(k);
            if let Some(existing) = keys.get(&key) {
                return Arc::clone(&existing.0);
            }
            let out = Arc::clone(&key.0);
            keys.insert(key);
            out
        };
        match dress.dedup_inner(&mut dedup_builtin, &mut dedup_keys) {
            Some(updated) => Arc::new(updated),
            None => Arc::clone(dress),
        }
    }

    fn dedup_compact_grid(&mut self, grid: &CompactGrid) -> CompactGrid {
        let key = InternedCompactGrid(grid.clone());
        if let Some(existing) = self.compact_grids.get(&key) {
            return existing.0.clone();
        }
        let out = key.0.clone();
        self.compact_grids.insert(key);
        out
    }

    fn dedup_grid(&mut self, grid: &Arc<Grid>) -> Arc<Grid> {
        let key = InternedGrid(Arc::clone(grid));
        if let Some(existing) = self.grids.get(&key) {
            return Arc::clone(&existing.0);
        }
        let out = Arc::clone(&key.0);
        self.grids.insert(key);
        out
    }

    fn dedup_layers(&mut self, layers: &Arc<[Layer]>) -> Arc<[Layer]> {
        let id = Arc::as_ptr(layers) as *const Layer as usize;
        if let Some(existing) = self.seen_layers.get(&id) {
            return Arc::clone(existing);
        }
        let mut owned: Option<Vec<Layer>> = None;
        for (idx, layer) in layers.iter().enumerate() {
            if let Some(updated_layer) = layer.dedup_in(self) {
                let vec = owned.get_or_insert_with(|| layers.to_vec());
                vec[idx] = updated_layer;
            }
        }
        let candidate: Arc<[Layer]> = match owned {
            Some(vec) => vec.into(),
            None => Arc::clone(layers),
        };
        let canon = if candidate.iter().all(Layer::can_intern) {
            let key = InternedLayers(candidate);
            if let Some(existing) = self.layers.get(&key) {
                Arc::clone(&existing.0)
            } else {
                let out = Arc::clone(&key.0);
                self.layers.insert(key);
                out
            }
        } else {
            candidate
        };
        self.seen_layers.insert(id, Arc::clone(&canon));
        canon
    }

    fn dedup_pattern(&mut self, pat: &mut Pattern) -> bool {
        let mut changed = false;
        let plain_extras = &mut self.plain_extras;
        pat.dedup_plain_extras(&mut |key, ex| {
            let canon = plain_extras.entry(key).or_insert_with(|| Arc::clone(ex));
            if !Arc::ptr_eq(ex, canon) {
                changed = true;
            }
            Arc::clone(canon)
        });
        let next = self.dedup_cell(pat.node_arc(), pat.node_flags());
        if !Arc::ptr_eq(pat.node_arc(), &next) {
            pat.set_node_arc(next);
            changed = true;
        }
        changed
    }

    fn dedup_pat_child(&mut self, pat: &Pattern) -> Option<Pattern> {
        let mut updated = pat.clone();
        self.dedup_pattern(&mut updated).then_some(updated)
    }

    fn dedup_slice(&mut self, pats: &[Pattern]) -> Option<Box<[Pattern]>> {
        let mut owned: Option<Vec<Pattern>> = None;
        for (idx, pat) in pats.iter().enumerate() {
            if let Some(updated) = self.dedup_pat_child(pat) {
                let vec = owned.get_or_insert_with(|| pats.to_vec());
                vec[idx] = updated;
            }
        }
        owned.map(Vec::into_boxed_slice)
    }

    fn dedup_pure_cells(&mut self, cells: &[Arc<NodeCell>]) -> Option<Box<[Arc<NodeCell>]>> {
        let mut owned: Option<Vec<Arc<NodeCell>>> = None;
        for (idx, cell) in cells.iter().enumerate() {
            let next = self.dedup_cell(cell, 0);
            if !Arc::ptr_eq(cell, &next) {
                let vec = owned.get_or_insert_with(|| cells.to_vec());
                vec[idx] = next;
            }
        }
        owned.map(Vec::into_boxed_slice)
    }

    fn dedup_cell(&mut self, cell: &Arc<NodeCell>, flags: u8) -> Arc<NodeCell> {
        let id = Arc::as_ptr(cell) as usize;
        if let Some(existing) = self.visited.get(&id) {
            return Arc::clone(existing);
        }
        let rebuilt: Option<Node> = match &cell.kind {
            Node::Sequence(layers) => {
                let next = self.dedup_layers(layers);
                (!Arc::ptr_eq(layers, &next)).then_some(Node::Sequence(next))
            }
            Node::Pure(val) => self.dedup_value(val).map(Node::Pure),
            Node::Stack(pats) => self.dedup_slice(pats).map(Node::Stack),
            Node::StackPure(cells) => self.dedup_pure_cells(cells).map(Node::StackPure),
            Node::SlowCat(pats) => self.dedup_slice(pats).map(Node::SlowCat),
            Node::SlowCatPure(cells) => self.dedup_pure_cells(cells).map(Node::SlowCatPure),
            Node::SlowCatPrime(pats) => self.dedup_slice(pats).map(Node::SlowCatPrime),
            Node::SlowCatPrimePure(cells) => {
                self.dedup_pure_cells(cells).map(Node::SlowCatPrimePure)
            }
            Node::FastSmall(inner, f, n, d, is_slow) => {
                let next = self.dedup_cell(inner, *f);
                (!Arc::ptr_eq(inner, &next)).then_some(Node::FastSmall(next, *f, *n, *d, *is_slow))
            }
            Node::ShiftSmall(inner, f, n, d, is_early) => {
                let next = self.dedup_cell(inner, *f);
                (!Arc::ptr_eq(inner, &next))
                    .then_some(Node::ShiftSmall(next, *f, *n, *d, *is_early))
            }
            Node::ControlMap(inner, f, key_id, unnamed) => {
                let next = self.dedup_cell(inner, *f);
                (!Arc::ptr_eq(inner, &next))
                    .then_some(Node::ControlMap(next, *f, *key_id, *unnamed))
            }
            Node::SplitQueries(inner, f) => {
                let next = self.dedup_cell(inner, *f);
                (!Arc::ptr_eq(inner, &next)).then_some(Node::SplitQueries(next, *f))
            }
            Node::SplitQueriesBoxed(pat) => self
                .dedup_pat_child(pat)
                .map(|p| Node::SplitQueriesBoxed(Box::new(p))),
            Node::Fast(node) => self
                .dedup_pat_child(&node.0)
                .map(|p| Node::Fast(Box::new(FastNode(p, node.1, node.2)))),
            Node::Shift(node) => self
                .dedup_pat_child(&node.0)
                .map(|p| Node::Shift(Box::new(ShiftNode(p, node.1, node.2)))),
            Node::Degrade(boxed) => self
                .dedup_pat_child(&boxed.0)
                .map(|p| Node::Degrade(Box::new((p, boxed.1, boxed.2)))),
            Node::AppWave(app) => self.dedup_pat_child(&app.inner).map(|inner| {
                Node::AppWave(Box::new(AppWaveNode {
                    inner,
                    waves: app.waves.clone(),
                }))
            }),
            Node::Constants(node) => {
                let next_inner = self.dedup_pat_child(&node.0);
                let mut meetings_owned: Option<Vec<RunMeeting>> = None;
                for (idx, m) in node.1.iter().enumerate() {
                    if let Some(next_val) = self.dedup_value(&m.value) {
                        let vec = meetings_owned.get_or_insert_with(|| node.1.to_vec());
                        vec[idx].value = next_val;
                    }
                }
                if next_inner.is_some() || meetings_owned.is_some() {
                    let inner = next_inner.unwrap_or_else(|| node.0.clone());
                    let meetings = match meetings_owned {
                        Some(mut vec) if vec.len() == 1 => ConstantsMeetings::One(vec.remove(0)),
                        Some(vec) => ConstantsMeetings::Many(vec.into_boxed_slice()),
                        None => node.1.clone(),
                    };
                    Some(Node::Constants(Box::new(ConstantsNode(inner, meetings))))
                } else {
                    None
                }
            }
            Node::Pick(pick) => {
                let next_sel = self.dedup_pat_child(&pick.selector);
                let next_lookup = match &pick.lookup {
                    CompactPickLookup::Dense(pats) => {
                        self.dedup_slice(pats).map(CompactPickLookup::Dense)
                    }
                    CompactPickLookup::Array {
                        enumerable_len,
                        length,
                        entries,
                    } => {
                        let mut owned: Option<Vec<(usize, Pattern)>> = None;
                        for (idx, (_, pat)) in entries.iter().enumerate() {
                            if let Some(next) = self.dedup_pat_child(pat) {
                                let vec = owned.get_or_insert_with(|| entries.to_vec());
                                vec[idx].1 = next;
                            }
                        }
                        owned.map(|vec| CompactPickLookup::Array {
                            enumerable_len: *enumerable_len,
                            length: *length,
                            entries: vec.into_boxed_slice(),
                        })
                    }
                    CompactPickLookup::Object {
                        enumerable_len,
                        entries,
                    } => {
                        let mut owned: Option<Vec<(crate::value::SharedStr, Pattern)>> = None;
                        for (idx, (_, pat)) in entries.iter().enumerate() {
                            if let Some(next) = self.dedup_pat_child(pat) {
                                let vec = owned.get_or_insert_with(|| entries.to_vec());
                                vec[idx].1 = next;
                            }
                        }
                        owned.map(|vec| CompactPickLookup::Object {
                            enumerable_len: *enumerable_len,
                            entries: vec.into_boxed_slice(),
                        })
                    }
                };
                if next_sel.is_some() || next_lookup.is_some() {
                    Some(Node::Pick(Box::new(PickNode {
                        selector: next_sel.unwrap_or_else(|| pick.selector.clone()),
                        lookup: next_lookup.unwrap_or_else(|| pick.lookup.clone()),
                        index_mode: pick.index_mode,
                        mode: pick.mode,
                    })))
                } else {
                    None
                }
            }
            Node::AppLeft(app) => {
                let p0 = self.dedup_pat_child(&app.0);
                let p1 = self.dedup_pat_child(&app.1);
                if p0.is_some() || p1.is_some() {
                    Some(Node::AppLeft(Box::new(AppLeftNode(
                        p0.unwrap_or_else(|| app.0.clone()),
                        p1.unwrap_or_else(|| app.1.clone()),
                        ValueReading::default(),
                        app.3.clone(),
                        app.4,
                    ))))
                } else {
                    None
                }
            }
            Node::AppRight(app) => {
                let p0 = self.dedup_pat_child(&app.0);
                let p1 = self.dedup_pat_child(&app.1);
                if p0.is_some() || p1.is_some() {
                    Some(Node::AppRight(Box::new(AppNode(
                        p0.unwrap_or_else(|| app.0.clone()),
                        p1.unwrap_or_else(|| app.1.clone()),
                        app.2.clone(),
                        app.3,
                    ))))
                } else {
                    None
                }
            }
            Node::AppBoth(app) => {
                let p0 = self.dedup_pat_child(&app.0);
                let p1 = self.dedup_pat_child(&app.1);
                if p0.is_some() || p1.is_some() {
                    Some(Node::AppBoth(Box::new(AppNode(
                        p0.unwrap_or_else(|| app.0.clone()),
                        p1.unwrap_or_else(|| app.1.clone()),
                        app.2.clone(),
                        app.3,
                    ))))
                } else {
                    None
                }
            }
            Node::AddContext(boxed) => {
                let next = self.dedup_cell(boxed.0.node_arc(), boxed.0.node_flags());
                if !Arc::ptr_eq(boxed.0.node_arc(), &next) {
                    let mut p = boxed.0.clone();
                    p.set_node_arc(next);
                    Some(Node::AddContext(Box::new((p, Arc::clone(&boxed.1)))))
                } else {
                    None
                }
            }
            Node::FilterValues(boxed) => {
                let next = self.dedup_cell(boxed.0.node_arc(), boxed.0.node_flags());
                if !Arc::ptr_eq(boxed.0.node_arc(), &next) {
                    let mut p = boxed.0.clone();
                    p.set_node_arc(next);
                    Some(Node::FilterValues(Box::new((p, Arc::clone(&boxed.1)))))
                } else {
                    None
                }
            }
            Node::FilterHaps(boxed) => {
                let next = self.dedup_cell(boxed.0.node_arc(), boxed.0.node_flags());
                if !Arc::ptr_eq(boxed.0.node_arc(), &next) {
                    let mut p = boxed.0.clone();
                    p.set_node_arc(next);
                    Some(Node::FilterHaps(Box::new((p, Arc::clone(&boxed.1)))))
                } else {
                    None
                }
            }
            Node::MapHapsOwned(boxed) => {
                let next = self.dedup_cell(boxed.0.node_arc(), boxed.0.node_flags());
                if !Arc::ptr_eq(boxed.0.node_arc(), &next) {
                    let mut p = boxed.0.clone();
                    p.set_node_arc(next);
                    Some(Node::MapHapsOwned(Box::new((
                        p,
                        Arc::clone(&boxed.1),
                        boxed.2,
                    ))))
                } else {
                    None
                }
            }
            Node::MapHapsNative(boxed) => {
                let next = self.dedup_cell(boxed.0.node_arc(), boxed.0.node_flags());
                if !Arc::ptr_eq(boxed.0.node_arc(), &next) {
                    let mut p = boxed.0.clone();
                    p.set_node_arc(next);
                    Some(Node::MapHapsNative(Box::new((
                        p,
                        Arc::clone(&boxed.1),
                        boxed.2,
                    ))))
                } else {
                    None
                }
            }
            Node::ExpandHapsNative(boxed) => {
                let next = self.dedup_cell(boxed.0.node_arc(), boxed.0.node_flags());
                if !Arc::ptr_eq(boxed.0.node_arc(), &next) {
                    let mut p = boxed.0.clone();
                    p.set_node_arc(next);
                    Some(Node::ExpandHapsNative(Box::new((
                        p,
                        Arc::clone(&boxed.1),
                        boxed.2,
                    ))))
                } else {
                    None
                }
            }
            Node::FmapNative(boxed) => {
                let next = self.dedup_cell(boxed.0.node_arc(), boxed.0.node_flags());
                if !Arc::ptr_eq(boxed.0.node_arc(), &next) {
                    let mut p = boxed.0.clone();
                    p.set_node_arc(next);
                    Some(Node::FmapNative(Box::new((p, Arc::clone(&boxed.1)))))
                } else {
                    None
                }
            }
            Node::Rev(pat) => {
                let next = self.dedup_cell(pat.node_arc(), pat.node_flags());
                if !Arc::ptr_eq(pat.node_arc(), &next) {
                    let mut p = (**pat).clone();
                    p.set_node_arc(next);
                    Some(Node::Rev(Box::new(p)))
                } else {
                    None
                }
            }
            Node::WithRandSeed(boxed) => {
                let next = self.dedup_cell(boxed.0.node_arc(), boxed.0.node_flags());
                if !Arc::ptr_eq(boxed.0.node_arc(), &next) {
                    let mut p = boxed.0.clone();
                    p.set_node_arc(next);
                    Some(Node::WithRandSeed(Box::new((p, boxed.1))))
                } else {
                    None
                }
            }
            Node::FastGap(boxed) => {
                let next = self.dedup_cell(boxed.0.node_arc(), boxed.0.node_flags());
                if !Arc::ptr_eq(boxed.0.node_arc(), &next) {
                    let mut p = boxed.0.clone();
                    p.set_node_arc(next);
                    Some(Node::FastGap(Box::new((p, boxed.1))))
                } else {
                    None
                }
            }
            Node::RepeatCycles(boxed) => {
                let next = self.dedup_cell(boxed.0.node_arc(), boxed.0.node_flags());
                if !Arc::ptr_eq(boxed.0.node_arc(), &next) {
                    let mut p = boxed.0.clone();
                    p.set_node_arc(next);
                    Some(Node::RepeatCycles(Box::new((p, boxed.1))))
                } else {
                    None
                }
            }
            Node::LoopAt(boxed) => {
                let next = self.dedup_cell(boxed.0.node_arc(), boxed.0.node_flags());
                if !Arc::ptr_eq(boxed.0.node_arc(), &next) {
                    let mut p = boxed.0.clone();
                    p.set_node_arc(next);
                    Some(Node::LoopAt(Box::new((p, boxed.1))))
                } else {
                    None
                }
            }
            Node::Cpm(boxed) => self
                .dedup_pat_child(&boxed.0)
                .map(|p| Node::Cpm(Box::new((p, TempoRate::new(boxed.1.cpm))))),
            Node::UiVisual(node) => self.dedup_pat_child(&node.inner).map(|inner| {
                Node::UiVisual(Box::new(UiVisualNode {
                    inner,
                    slot: node.slot,
                    cache: std::sync::OnceLock::new(),
                }))
            }),
            Node::ControlMapBoxed(boxed) => self
                .dedup_pat_child(&boxed.0)
                .map(|p| Node::ControlMapBoxed(Box::new((p, boxed.1, boxed.2)))),
            Node::Join(inner, f, mode) => {
                let next = self.dedup_cell(inner, *f);
                (!Arc::ptr_eq(inner, &next)).then_some(Node::Join(next, *f, *mode))
            }
            Node::JoinBoxed(boxed) => self
                .dedup_pat_child(&boxed.0)
                .map(|p| Node::JoinBoxed(Box::new((p, boxed.1)))),
            Node::StepJoin(pat) => {
                let next = self.dedup_cell(pat.node_arc(), pat.node_flags());
                if !Arc::ptr_eq(pat.node_arc(), &next) {
                    let mut p = (**pat).clone();
                    p.set_node_arc(next);
                    Some(Node::StepJoin(Box::new(p)))
                } else {
                    None
                }
            }
            Node::PolyJoin(pat) => {
                let next = self.dedup_cell(pat.node_arc(), pat.node_flags());
                if !Arc::ptr_eq(pat.node_arc(), &next) {
                    let mut p = (**pat).clone();
                    p.set_node_arc(next);
                    Some(Node::PolyJoin(Box::new(p)))
                } else {
                    None
                }
            }
            Node::PurePattern(pat) => {
                let next = self.dedup_cell(pat.node_arc(), pat.node_flags());
                if !Arc::ptr_eq(pat.node_arc(), &next) {
                    let mut p = (**pat).clone();
                    p.set_node_arc(next);
                    Some(Node::PurePattern(Box::new(p)))
                } else {
                    None
                }
            }
            Node::SortHapsByPart(pat) => {
                let next = self.dedup_cell(pat.node_arc(), pat.node_flags());
                if !Arc::ptr_eq(pat.node_arc(), &next) {
                    let mut p = (**pat).clone();
                    p.set_node_arc(next);
                    Some(Node::SortHapsByPart(Box::new(p)))
                } else {
                    None
                }
            }
            Node::Collect(pat) => {
                let next = self.dedup_cell(pat.node_arc(), pat.node_flags());
                if !Arc::ptr_eq(pat.node_arc(), &next) {
                    let mut p = (**pat).clone();
                    p.set_node_arc(next);
                    Some(Node::Collect(Box::new(p)))
                } else {
                    None
                }
            }
            Node::CycleOnsetIndex(pat) => {
                let next = self.dedup_cell(pat.node_arc(), pat.node_flags());
                if !Arc::ptr_eq(pat.node_arc(), &next) {
                    let mut p = (**pat).clone();
                    p.set_node_arc(next);
                    Some(Node::CycleOnsetIndex(Box::new(p)))
                } else {
                    None
                }
            }
            Node::CycleOnsetCount(pat) => {
                let next = self.dedup_cell(pat.node_arc(), pat.node_flags());
                if !Arc::ptr_eq(pat.node_arc(), &next) {
                    let mut p = (**pat).clone();
                    p.set_node_arc(next);
                    Some(Node::CycleOnsetCount(Box::new(p)))
                } else {
                    None
                }
            }
            Node::WithQueryTime(boxed) => {
                let next = self.dedup_cell(boxed.0.node_arc(), boxed.0.node_flags());
                if !Arc::ptr_eq(boxed.0.node_arc(), &next) {
                    let mut p = boxed.0.clone();
                    p.set_node_arc(next);
                    Some(Node::WithQueryTime(Box::new((p, Arc::clone(&boxed.1)))))
                } else {
                    None
                }
            }
            Node::WithHapTime(boxed) => {
                let next = self.dedup_cell(boxed.0.node_arc(), boxed.0.node_flags());
                if !Arc::ptr_eq(boxed.0.node_arc(), &next) {
                    let mut p = boxed.0.clone();
                    p.set_node_arc(next);
                    Some(Node::WithHapTime(Box::new((p, Arc::clone(&boxed.1)))))
                } else {
                    None
                }
            }
            Node::WithQuerySpan(boxed) => {
                let next = self.dedup_cell(boxed.0.node_arc(), boxed.0.node_flags());
                if !Arc::ptr_eq(boxed.0.node_arc(), &next) {
                    let mut p = boxed.0.clone();
                    p.set_node_arc(next);
                    Some(Node::WithQuerySpan(Box::new((p, Arc::clone(&boxed.1)))))
                } else {
                    None
                }
            }
            Node::WithHapSpan(boxed) => {
                let next = self.dedup_cell(boxed.0.node_arc(), boxed.0.node_flags());
                if !Arc::ptr_eq(boxed.0.node_arc(), &next) {
                    let mut p = boxed.0.clone();
                    p.set_node_arc(next);
                    Some(Node::WithHapSpan(Box::new((p, Arc::clone(&boxed.1)))))
                } else {
                    None
                }
            }
            Node::MapHapsWithState(boxed) => {
                let next = self.dedup_cell(boxed.0.node_arc(), boxed.0.node_flags());
                if !Arc::ptr_eq(boxed.0.node_arc(), &next) {
                    let mut p = boxed.0.clone();
                    p.set_node_arc(next);
                    Some(Node::MapHapsWithState(Box::new((p, Arc::clone(&boxed.1)))))
                } else {
                    None
                }
            }
            Node::InnerJoin(boxed) => {
                let next = self.dedup_cell(boxed.0.node_arc(), boxed.0.node_flags());
                if !Arc::ptr_eq(boxed.0.node_arc(), &next) {
                    let mut p = boxed.0.clone();
                    p.set_node_arc(next);
                    Some(Node::InnerJoin(Box::new((p, Arc::clone(&boxed.1)))))
                } else {
                    None
                }
            }
            Node::ChooseCycles(boxed) => self
                .dedup_slice(&boxed.0)
                .map(|pats| Node::ChooseCycles(Box::new((pats, boxed.1)))),
            Node::PatternOfPure(boxed) => {
                let next = self.dedup_cell(boxed.0.node_arc(), boxed.0.node_flags());
                if !Arc::ptr_eq(boxed.0.node_arc(), &next) {
                    let mut p = boxed.0.clone();
                    p.set_node_arc(next);
                    Some(Node::PatternOfPure(Box::new((
                        p,
                        Arc::clone(&boxed.1),
                        ConstructionMemo::default(),
                    ))))
                } else {
                    None
                }
            }
            Node::PatternOfPureOperand(pair, map) => {
                let n0 = self.dedup_cell(pair.0.node_arc(), pair.0.node_flags());
                let n1 = self.dedup_cell(pair.1.node_arc(), pair.1.node_flags());
                if !Arc::ptr_eq(pair.0.node_arc(), &n0) || !Arc::ptr_eq(pair.1.node_arc(), &n1) {
                    let mut p0 = pair.0.clone();
                    let mut p1 = pair.1.clone();
                    p0.set_node_arc(n0);
                    p1.set_node_arc(n1);
                    Some(Node::PatternOfPureOperand(
                        Box::new(OperandPureNode(p0, p1, ConstructionMemo::default())),
                        Arc::clone(map),
                    ))
                } else {
                    None
                }
            }
            Node::PatternOfHap(bind) => {
                let n0 = self.dedup_cell(bind.0.node_arc(), bind.0.node_flags());
                let deps = self.dedup_slice(&bind.1);
                if !Arc::ptr_eq(bind.0.node_arc(), &n0) || deps.is_some() {
                    let mut p0 = bind.0.clone();
                    p0.set_node_arc(n0);
                    let new_deps = deps
                        .map(<[Pattern]>::into_vec)
                        .unwrap_or_else(|| bind.1.clone());
                    Some(Node::PatternOfHap(Box::new(HapBind(
                        p0,
                        new_deps,
                        Arc::clone(&bind.2),
                    ))))
                } else {
                    None
                }
            }
            Node::WChoose(choice) => {
                let nc = self.dedup_cell(choice.chooser.node_arc(), choice.chooser.node_flags());
                let nt = self.dedup_cell(choice.total.node_arc(), choice.total.node_flags());
                let ncum = self.dedup_slice(&choice.cumulative);
                let nval = self.dedup_slice(&choice.values);
                if !Arc::ptr_eq(choice.chooser.node_arc(), &nc)
                    || !Arc::ptr_eq(choice.total.node_arc(), &nt)
                    || ncum.is_some()
                    || nval.is_some()
                {
                    let mut chooser = choice.chooser.clone();
                    let mut total = choice.total.clone();
                    chooser.set_node_arc(nc);
                    total.set_node_arc(nt);
                    Some(Node::WChoose(Box::new(WeightedChoice {
                        chooser,
                        total,
                        cumulative: ncum
                            .map(<[Pattern]>::into_vec)
                            .unwrap_or_else(|| choice.cumulative.clone()),
                        values: nval
                            .map(<[Pattern]>::into_vec)
                            .unwrap_or_else(|| choice.values.clone()),
                        inner: choice.inner,
                    })))
                } else {
                    None
                }
            }
            Node::Arp(pair) => {
                let n0 = self.dedup_cell(pair.0.node_arc(), pair.0.node_flags());
                let n1 = self.dedup_cell(pair.1.node_arc(), pair.1.node_flags());
                if !Arc::ptr_eq(pair.0.node_arc(), &n0) || !Arc::ptr_eq(pair.1.node_arc(), &n1) {
                    let mut p0 = pair.0.clone();
                    let mut p1 = pair.1.clone();
                    p0.set_node_arc(n0);
                    p1.set_node_arc(n1);
                    Some(Node::Arp(Box::new(PatternPair(p0, p1))))
                } else {
                    None
                }
            }
            Node::PickPatternified {
                patterns,
                index_mode,
                mode,
            } => {
                let n0 = self.dedup_cell(patterns.0.node_arc(), patterns.0.node_flags());
                let n1 = self.dedup_cell(patterns.1.node_arc(), patterns.1.node_flags());
                if !Arc::ptr_eq(patterns.0.node_arc(), &n0)
                    || !Arc::ptr_eq(patterns.1.node_arc(), &n1)
                {
                    let mut p0 = patterns.0.clone();
                    let mut p1 = patterns.1.clone();
                    p0.set_node_arc(n0);
                    p1.set_node_arc(n1);
                    Some(Node::PickPatternified {
                        patterns: Box::new(PatternPair(p0, p1)),
                        index_mode: *index_mode,
                        mode: *mode,
                    })
                } else {
                    None
                }
            }
            Node::XenPatterns(boxed) => {
                let n0 = self.dedup_cell(boxed.0.node_arc(), boxed.0.node_flags());
                let nr = self.dedup_slice(&boxed.1);
                if !Arc::ptr_eq(boxed.0.node_arc(), &n0) || nr.is_some() {
                    let mut p0 = boxed.0.clone();
                    p0.set_node_arc(n0);
                    let ratios = nr
                        .map(<[Pattern]>::into_vec)
                        .unwrap_or_else(|| boxed.1.clone());
                    Some(Node::XenPatterns(Box::new((p0, ratios))))
                } else {
                    None
                }
            }
            Node::Silence
            | Node::QueryError(_)
            | Node::QueryLimit(_)
            | Node::Signal(_)
            | Node::Extension(_)
            | Node::PurePickLookup(_)
            | Node::MidiKeys(_)
            | Node::PatternOfDynamic(_)
            | Node::WithQuerySpanJs(_)
            | Node::ArpWith(_)
            | Node::ArpWithPattern(_)
            | Node::PatternOfJs(_)
            | Node::FilterValuesJs(_)
            | Node::FilterHapsJs(_)
            | Node::FilterWhenJs(_)
            | Node::FmapJs(_)
            | Node::FmapJsPreservingControls(_)
            | Node::JsQuery(_)
            | Node::Timeline(..) => None,
            #[cfg(non_send_node_mutation)]
            Node::NonSendMutation(_) => None,
        };
        let candidate = match rebuilt {
            Some(kind) => Arc::new(NodeCell { kind }),
            None => Arc::clone(cell),
        };
        let canon = if can_intern_node(&candidate.kind, flags) {
            let key = InternedNode(candidate);
            if let Some(existing) = self.nodes.get(&key) {
                Arc::clone(&existing.0)
            } else {
                let out = Arc::clone(&key.0);
                self.nodes.insert(key);
                out
            }
        } else {
            candidate
        };
        self.visited.insert(id, Arc::clone(&canon));
        canon
    }
}

impl LayerDeduper for Deduper {
    fn dedup_values(&mut self, values: &Arc<[Value]>) -> Arc<[Value]> {
        Self::dedup_values(self, values)
    }

    fn dedup_dress(&mut self, dress: &Arc<Dress>) -> Arc<Dress> {
        Self::dedup_dress(self, dress)
    }

    fn dedup_compact_grid(&mut self, grid: &CompactGrid) -> CompactGrid {
        Self::dedup_compact_grid(self, grid)
    }

    fn dedup_grid(&mut self, grid: &Arc<Grid>) -> Arc<Grid> {
        Self::dedup_grid(self, grid)
    }
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

struct DataValueRef<'a>(&'a Value);

impl Hash for DataValueRef<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        hash_data(self.0, state);
    }
}

impl PartialEq for DataValueRef<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.0.same_data(other.0)
    }
}

impl Eq for DataValueRef<'_> {}

// --- Arc<dyn BuiltinEntries> ---

struct InternedBuiltin(Arc<dyn BuiltinEntries>);

impl Hash for InternedBuiltin {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let keys = self.0.keys();
        let slice = self.0.as_slice();
        keys.hash(state);
        for i in 0..keys.len() {
            hash_data(slice.at(i).1, state);
        }
    }
}

impl PartialEq for InternedBuiltin {
    fn eq(&self, other: &Self) -> bool {
        if Arc::ptr_eq(&self.0, &other.0) {
            return true;
        }
        let ak = self.0.keys();
        let bk = other.0.keys();
        if ak != bk {
            return false;
        }
        let a_slice = self.0.as_slice();
        let b_slice = other.0.as_slice();
        (0..ak.len()).all(|i| a_slice.at(i).1.same_data(b_slice.at(i).1))
    }
}

impl Eq for InternedBuiltin {}

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

// --- CompactGrid ---

struct InternedCompactGrid(CompactGrid);

impl Hash for InternedCompactGrid {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let (ticks, base, steps, packed) = self.0.parts();
        ticks.get().hash(state);
        base.hash(state);
        for step in steps {
            step.hash_intern(state);
        }
        packed.hash(state);
    }
}

impl PartialEq for InternedCompactGrid {
    fn eq(&self, other: &Self) -> bool {
        if self.0.ptr_eq(&other.0) {
            return true;
        }
        let (at, ab, asteps, apacked) = self.0.parts();
        let (bt, bb, bsteps, bpacked) = other.0.parts();
        at == bt && ab == bb && asteps == bsteps && apacked == bpacked
    }
}

impl Eq for InternedCompactGrid {}

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

// --- Arc<[Layer]> ---

struct InternedLayers(Arc<[Layer]>);

impl Hash for InternedLayers {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.len().hash(state);
        for layer in self.0.iter() {
            layer.hash_intern(state);
        }
    }
}

impl PartialEq for InternedLayers {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
            || (self.0.len() == other.0.len()
                && self
                    .0
                    .iter()
                    .zip(other.0.iter())
                    .all(|(a, b)| a.eq_intern(b)))
    }
}

impl Eq for InternedLayers {}

// --- Pure Arc<NodeCell> ---

#[inline]
fn same_pat_shallow(a: &Pattern, b: &Pattern) -> bool {
    Arc::ptr_eq(a.node_arc(), b.node_arc())
        && a.packed_steps() == b.packed_steps()
        && a.extras_ptr() == b.extras_ptr()
        && a.has_internable_extras()
        && b.has_internable_extras()
}

#[inline]
fn hash_pat_shallow(p: &Pattern, state: &mut impl Hasher) {
    (Arc::as_ptr(p.node_arc()) as usize).hash(state);
    p.packed_steps().hash(state);
    p.extras_ptr().hash(state);
}

#[inline]
fn hash_degrade_kind(kind: DegradeKind, state: &mut impl Hasher) {
    match kind {
        DegradeKind::Seeded(seed) => {
            0u8.hash(state);
            seed.hash(state);
        }
        DegradeKind::Rand => 1u8.hash(state),
        DegradeKind::RandInverted => 2u8.hash(state),
    }
}

fn can_intern_node(node: &Node, flags: u8) -> bool {
    // Only intern pure, non-volatile, non-host-voicing nodes (low 4 bits == 0).
    if (flags & 0x0f) != 0 {
        return false;
    }
    match node {
        Node::Sequence(_)
        | Node::FastSmall(..)
        | Node::ShiftSmall(..)
        | Node::ControlMap(..)
        | Node::SplitQueries(..)
        | Node::StackPure(_)
        | Node::SlowCatPure(_)
        | Node::SlowCatPrimePure(_) => true,
        Node::Pure(v) => v.is_data(0),
        Node::Fast(n) => n.0.has_internable_extras(),
        Node::Shift(n) => n.0.has_internable_extras(),
        Node::Degrade(b) => b.0.has_internable_extras(),
        Node::Stack(pats) | Node::SlowCat(pats) | Node::SlowCatPrime(pats) => {
            pats.iter().all(Pattern::has_internable_extras)
        }
        Node::Constants(c) => {
            c.0.has_internable_extras()
                && c.1
                    .iter()
                    .all(|m| matches!(m.apply, RunApply::Composed { .. }) && m.value.is_data(0))
        }
        Node::AppWave(app) => app.inner.has_internable_extras(),
        _ => false,
    }
}

struct InternedNode(Arc<NodeCell>);

impl Hash for InternedNode {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match &self.0.kind {
            Node::Sequence(layers) => {
                1u8.hash(state);
                (Arc::as_ptr(layers) as *const Layer as usize).hash(state);
                layers.len().hash(state);
            }
            Node::Pure(v) => {
                2u8.hash(state);
                hash_data(v, state);
            }
            Node::FastSmall(cell, f, n, d, is_slow) => {
                3u8.hash(state);
                (Arc::as_ptr(cell) as usize).hash(state);
                f.hash(state);
                n.hash(state);
                d.hash(state);
                is_slow.hash(state);
            }
            Node::ShiftSmall(cell, f, n, d, is_early) => {
                4u8.hash(state);
                (Arc::as_ptr(cell) as usize).hash(state);
                f.hash(state);
                n.hash(state);
                d.hash(state);
                is_early.hash(state);
            }
            Node::ControlMap(cell, f, id, unnamed) => {
                5u8.hash(state);
                (Arc::as_ptr(cell) as usize).hash(state);
                f.hash(state);
                id.hash(state);
                unnamed.hash(state);
            }
            Node::SplitQueries(cell, f) => {
                6u8.hash(state);
                (Arc::as_ptr(cell) as usize).hash(state);
                f.hash(state);
            }
            Node::Fast(n) => {
                7u8.hash(state);
                hash_pat_shallow(&n.0, state);
                n.1.numer().hash(state);
                n.1.denom().hash(state);
                n.2.hash(state);
            }
            Node::Shift(n) => {
                8u8.hash(state);
                hash_pat_shallow(&n.0, state);
                n.1.numer().hash(state);
                n.1.denom().hash(state);
                n.2.hash(state);
            }
            Node::Degrade(b) => {
                9u8.hash(state);
                hash_pat_shallow(&b.0, state);
                b.1.to_bits().hash(state);
                hash_degrade_kind(b.2, state);
            }
            Node::Stack(pats) => {
                10u8.hash(state);
                pats.len().hash(state);
                for p in pats.iter() {
                    hash_pat_shallow(p, state);
                }
            }
            Node::StackPure(cells) => {
                14u8.hash(state);
                cells.len().hash(state);
                for c in cells.iter() {
                    (Arc::as_ptr(c) as usize).hash(state);
                }
            }
            Node::SlowCat(pats) => {
                11u8.hash(state);
                pats.len().hash(state);
                for p in pats.iter() {
                    hash_pat_shallow(p, state);
                }
            }
            Node::SlowCatPure(cells) => {
                15u8.hash(state);
                cells.len().hash(state);
                for c in cells.iter() {
                    (Arc::as_ptr(c) as usize).hash(state);
                }
            }
            Node::SlowCatPrime(pats) => {
                12u8.hash(state);
                pats.len().hash(state);
                for p in pats.iter() {
                    hash_pat_shallow(p, state);
                }
            }
            Node::SlowCatPrimePure(cells) => {
                16u8.hash(state);
                cells.len().hash(state);
                for c in cells.iter() {
                    (Arc::as_ptr(c) as usize).hash(state);
                }
            }
            Node::Constants(c) => {
                13u8.hash(state);
                hash_pat_shallow(&c.0, state);
                c.1.len().hash(state);
                for m in c.1.iter() {
                    if let RunApply::Composed {
                        op,
                        flow,
                        merged,
                        single_key,
                        loc0,
                    } = m.apply
                    {
                        (op as u8).hash(state);
                        (flow as u8).hash(state);
                        merged.hash(state);
                        single_key.hash(state);
                        loc0.hash(state);
                    }
                    hash_data(&m.value, state);
                }
            }
            Node::AppWave(app) => {
                17u8.hash(state);
                hash_pat_shallow(&app.inner, state);
                app.waves.len().hash(state);
                for item in app.waves.iter() {
                    item.key_id.hash(state);
                    item.math_inside.hash(state);
                    item.wave.hash_intern(state);
                }
            }
            _ => {}
        }
    }
}

impl PartialEq for InternedNode {
    fn eq(&self, other: &Self) -> bool {
        if Arc::ptr_eq(&self.0, &other.0) {
            return true;
        }
        match (&self.0.kind, &other.0.kind) {
            (Node::Sequence(a), Node::Sequence(b)) => Arc::ptr_eq(a, b),
            (Node::Pure(a), Node::Pure(b)) => a.same_data(b),
            (Node::FastSmall(ac, af, an, ad, as_), Node::FastSmall(bc, bf, bn, bd, bs)) => {
                Arc::ptr_eq(ac, bc) && af == bf && an == bn && ad == bd && as_ == bs
            }
            (Node::ShiftSmall(ac, af, an, ad, ae), Node::ShiftSmall(bc, bf, bn, bd, be)) => {
                Arc::ptr_eq(ac, bc) && af == bf && an == bn && ad == bd && ae == be
            }
            (Node::ControlMap(ac, af, ai, au), Node::ControlMap(bc, bf, bi, bu)) => {
                Arc::ptr_eq(ac, bc) && af == bf && ai == bi && au == bu
            }
            (Node::SplitQueries(ac, af), Node::SplitQueries(bc, bf)) => {
                Arc::ptr_eq(ac, bc) && af == bf
            }
            (Node::Fast(a), Node::Fast(b)) => {
                same_pat_shallow(&a.0, &b.0) && a.1 == b.1 && a.2 == b.2
            }
            (Node::Shift(a), Node::Shift(b)) => {
                same_pat_shallow(&a.0, &b.0) && a.1 == b.1 && a.2 == b.2
            }
            (Node::Degrade(a), Node::Degrade(b)) => {
                same_pat_shallow(&a.0, &b.0) && a.1.to_bits() == b.1.to_bits() && a.2 == b.2
            }
            (Node::Stack(a), Node::Stack(b))
            | (Node::SlowCat(a), Node::SlowCat(b))
            | (Node::SlowCatPrime(a), Node::SlowCatPrime(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| same_pat_shallow(x, y))
            }
            (Node::StackPure(a), Node::StackPure(b))
            | (Node::SlowCatPure(a), Node::SlowCatPure(b))
            | (Node::SlowCatPrimePure(a), Node::SlowCatPrimePure(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| Arc::ptr_eq(x, y))
            }
            (Node::Constants(a), Node::Constants(b)) => {
                same_pat_shallow(&a.0, &b.0)
                    && a.1.len() == b.1.len()
                    && a.1
                        .iter()
                        .zip(b.1.iter())
                        .all(|(x, y)| match (&x.apply, &y.apply) {
                            (
                                RunApply::Composed {
                                    op: ao,
                                    flow: af,
                                    merged: am,
                                    single_key: ask,
                                    loc0: al,
                                },
                                RunApply::Composed {
                                    op: bo,
                                    flow: bf,
                                    merged: bm,
                                    single_key: bsk,
                                    loc0: bl,
                                },
                            ) => {
                                ao == bo
                                    && af == bf
                                    && am == bm
                                    && ask == bsk
                                    && al == bl
                                    && x.value.same_data(&y.value)
                            }
                            _ => false,
                        })
            }
            (Node::AppWave(a), Node::AppWave(b)) => {
                same_pat_shallow(&a.inner, &b.inner)
                    && a.waves.len() == b.waves.len()
                    && a.waves.iter().zip(b.waves.iter()).all(|(x, y)| {
                        x.key_id == y.key_id
                            && x.math_inside == y.math_inside
                            && x.wave.same(&y.wave)
                    })
            }
            _ => false,
        }
    }
}

impl Eq for InternedNode {}
