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

use std::collections::HashMap;
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

type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<WordHasher>>;

/// Deduplicate immutable payloads and pure subgraphs across `root`, then remap
/// any additional node references (such as native memo parts) through the same
/// canonical node table so they share the deduplicated graph instead of pinning
/// old nodes.
pub fn dedup_score(
    root: &mut Pattern,
    remap_extras: impl FnMut(&mut dyn FnMut(&mut crate::PatternNodeRef)),
) {
    dedup_score_with_reused(root, &[], remap_extras);
}

/// Stack-only check for tiny graphs (`<= 20` nodes) with nothing worthwhile to
/// deduplicate, avoiding all hash-table allocations on small scores.
fn is_tiny_graph(root: &NodeCell) -> bool {
    const LIMIT: usize = 20;
    let mut stack: [Option<&NodeCell>; 24] = [None; 24];
    let mut top = 1usize;
    stack[0] = Some(root);
    let mut visited = 0usize;
    fn push<'a>(
        stack: &mut [Option<&'a NodeCell>; 24],
        top: &mut usize,
        cell: &'a NodeCell,
    ) -> bool {
        if *top >= stack.len() {
            return false;
        }
        stack[*top] = Some(cell);
        *top += 1;
        true
    }
    while top > 0 {
        top -= 1;
        let cell = stack[top].expect("stack slot present");
        visited += 1;
        if visited > LIMIT {
            return false;
        }
        match &cell.kind {
            Node::Sequence(layers) => {
                visited += layers.len();
                if visited > LIMIT {
                    return false;
                }
            }
            Node::Stack(pats) | Node::SlowCat(pats) | Node::SlowCatPrime(pats) => {
                for p in pats.iter() {
                    if !push(&mut stack, &mut top, p.node_arc()) {
                        return false;
                    }
                }
            }
            Node::StackPure(cells) | Node::SlowCatPure(cells) | Node::SlowCatPrimePure(cells) => {
                for c in cells.iter() {
                    if !push(&mut stack, &mut top, c) {
                        return false;
                    }
                }
            }
            Node::FastSmall(inner, ..)
            | Node::ShiftSmall(inner, ..)
            | Node::ControlMap(inner, ..)
            | Node::SplitQueries(inner, _)
            | Node::Join(inner, ..) => {
                if !push(&mut stack, &mut top, inner) {
                    return false;
                }
            }
            Node::Constants(node) => {
                if !push(&mut stack, &mut top, node.0.node_arc()) {
                    return false;
                }
            }
            Node::AppWave(app) => {
                if !push(&mut stack, &mut top, app.inner.node_arc()) {
                    return false;
                }
            }
            Node::UiVisual(node) => {
                if !push(&mut stack, &mut top, node.inner.node_arc()) {
                    return false;
                }
            }
            Node::Fast(node) => {
                if !push(&mut stack, &mut top, node.0.node_arc()) {
                    return false;
                }
            }
            Node::Shift(node) => {
                if !push(&mut stack, &mut top, node.0.node_arc()) {
                    return false;
                }
            }
            Node::SplitQueriesBoxed(p)
            | Node::Rev(p)
            | Node::StepJoin(p)
            | Node::PolyJoin(p)
            | Node::PurePattern(p)
            | Node::SortHapsByPart(p)
            | Node::Collect(p)
            | Node::CycleOnsetIndex(p)
            | Node::CycleOnsetCount(p) => {
                if !push(&mut stack, &mut top, p.node_arc()) {
                    return false;
                }
            }
            Node::AppLeft(app) => {
                if !push(&mut stack, &mut top, app.0.node_arc())
                    || !push(&mut stack, &mut top, app.1.node_arc())
                {
                    return false;
                }
            }
            Node::AppRight(app) | Node::AppBoth(app) => {
                if !push(&mut stack, &mut top, app.0.node_arc())
                    || !push(&mut stack, &mut top, app.1.node_arc())
                {
                    return false;
                }
            }
            Node::Degrade(b) => {
                if !push(&mut stack, &mut top, b.0.node_arc()) {
                    return false;
                }
            }
            Node::Cpm(b) => {
                if !push(&mut stack, &mut top, b.0.node_arc()) {
                    return false;
                }
            }
            Node::ControlMapBoxed(b) => {
                if !push(&mut stack, &mut top, b.0.node_arc()) {
                    return false;
                }
            }
            Node::JoinBoxed(b) => {
                if !push(&mut stack, &mut top, b.0.node_arc()) {
                    return false;
                }
            }
            Node::AddContext(b) => {
                if !push(&mut stack, &mut top, b.0.node_arc()) {
                    return false;
                }
            }
            Node::FilterValues(b) => {
                if !push(&mut stack, &mut top, b.0.node_arc()) {
                    return false;
                }
            }
            Node::FilterHaps(b) => {
                if !push(&mut stack, &mut top, b.0.node_arc()) {
                    return false;
                }
            }
            Node::MapHapsOwned(b) => {
                if !push(&mut stack, &mut top, b.0.node_arc()) {
                    return false;
                }
            }
            Node::MapHapsNative(b) => {
                if !push(&mut stack, &mut top, b.0.node_arc()) {
                    return false;
                }
            }
            Node::ExpandHapsNative(b) => {
                if !push(&mut stack, &mut top, b.0.node_arc()) {
                    return false;
                }
            }
            Node::FmapNative(b) => {
                if !push(&mut stack, &mut top, b.0.node_arc()) {
                    return false;
                }
            }
            Node::WithRandSeed(b) => {
                if !push(&mut stack, &mut top, b.0.node_arc()) {
                    return false;
                }
            }
            Node::FastGap(b) => {
                if !push(&mut stack, &mut top, b.0.node_arc()) {
                    return false;
                }
            }
            Node::RepeatCycles(b) => {
                if !push(&mut stack, &mut top, b.0.node_arc()) {
                    return false;
                }
            }
            Node::LoopAt(b) => {
                if !push(&mut stack, &mut top, b.0.node_arc()) {
                    return false;
                }
            }
            Node::Silence | Node::Pure(_) | Node::PureAt(_) | Node::Signal(_) => {}
            _ => return false,
        }
    }
    true
}

thread_local! {
    /// Reusable cleared deduper tables so repeated cold evals and edits avoid
    /// allocating and freeing 14 hash maps on every call while retaining zero
    /// `Arc` references between builds.
    static SPARE_DEDUPER: std::cell::RefCell<Option<Deduper>> =
        const { std::cell::RefCell::new(None) };
    static SPARE_SCAN: std::cell::RefCell<Option<ScanScratch>> =
        const { std::cell::RefCell::new(None) };
}

#[inline]
fn hash_with(f: impl FnOnce(&mut WordHasher)) -> u64 {
    let mut h = WordHasher::default();
    f(&mut h);
    h.finish()
}

/// Compact open-addressed interning table packing `(hash32 << 32) | index` into
/// one `u64` slot (8 B instead of 16 B). Avoids rehashing during growth, skips
/// clearing when empty, and lets callers probe by reference before allocating.
struct InternTable<T> {
    items: Vec<T>,
    slots: Vec<u64>,
}

const EMPTY_SLOT: u64 = u64::MAX;

#[inline(always)]
const fn mix_hash32(hash: u64) -> u32 {
    (hash ^ (hash >> 32)) as u32
}

impl<T> Default for InternTable<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            slots: Vec::new(),
        }
    }
}

impl<T> InternTable<T> {
    #[inline]
    fn clear(&mut self) {
        if !self.items.is_empty() {
            self.items.clear();
            self.slots.fill(EMPTY_SLOT);
        }
    }

    #[inline]
    fn capacity(&self) -> usize {
        self.slots.len()
    }

    fn reserve(&mut self, additional: usize) {
        if additional == 0 {
            return;
        }
        let needed = self.items.len().saturating_add(additional);
        self.items.reserve(additional);
        let target = needed.saturating_mul(2).max(16).next_power_of_two();
        if self.slots.len() < target {
            self.grow_to(target);
        }
    }

    fn grow_to(&mut self, new_len: usize) {
        let mut next = vec![EMPTY_SLOT; new_len];
        let mask = new_len - 1;
        for &entry in &self.slots {
            if (entry as u32) != u32::MAX {
                let mut idx = ((entry >> 32) as usize) & mask;
                while (next[idx] as u32) != u32::MAX {
                    idx = (idx + 1) & mask;
                }
                next[idx] = entry;
            }
        }
        self.slots = next;
    }

    /// Look up an existing entry matching `(hash, eq)` or insert `make(cand)`.
    /// Returns `(canonical_ref, found_existing)`.
    #[inline]
    fn intern_with<C>(
        &mut self,
        hash: u64,
        cand: C,
        mut eq: impl FnMut(&T, &C) -> bool,
        make: impl FnOnce(C) -> T,
    ) -> (&T, bool) {
        if self.items.len() * 2 >= self.slots.len() {
            let next_len = (self.slots.len() * 2).max(16);
            self.grow_to(next_len);
        }
        let h32 = mix_hash32(hash);
        let tag = u64::from(h32) << 32;
        let mask = self.slots.len() - 1;
        let mut idx = (h32 as usize) & mask;
        loop {
            let entry = self.slots[idx];
            let id = entry as u32;
            if id == u32::MAX {
                let new_id = self.items.len() as u32;
                self.items.push(make(cand));
                self.slots[idx] = tag | u64::from(new_id);
                return (&self.items[new_id as usize], false);
            }
            if ((entry >> 32) as u32) == h32 && eq(&self.items[id as usize], &cand) {
                return (&self.items[id as usize], true);
            }
            idx = (idx + 1) & mask;
        }
    }

    /// Look up an existing entry matching `(hash, eq)` or insert `make()`.
    /// Returns `(canonical_ref, found_existing)`.
    #[inline]
    fn intern(
        &mut self,
        hash: u64,
        mut eq: impl FnMut(&T) -> bool,
        make: impl FnOnce() -> T,
    ) -> (&T, bool) {
        self.intern_with(hash, (), |entry, ()| eq(entry), |()| make())
    }

    /// Mutable variant of [`Self::intern`] returning `(&mut T, found_existing)`.
    #[inline]
    fn intern_mut(
        &mut self,
        hash: u64,
        mut eq: impl FnMut(&T) -> bool,
        make: impl FnOnce() -> T,
    ) -> (&mut T, bool) {
        if self.items.len() * 2 >= self.slots.len() {
            let next_len = (self.slots.len() * 2).max(16);
            self.grow_to(next_len);
        }
        let h32 = mix_hash32(hash);
        let tag = u64::from(h32) << 32;
        let mask = self.slots.len() - 1;
        let mut idx = (h32 as usize) & mask;
        loop {
            let entry = self.slots[idx];
            let id = entry as u32;
            if id == u32::MAX {
                let new_id = self.items.len() as u32;
                self.items.push(make());
                self.slots[idx] = tag | u64::from(new_id);
                return (&mut self.items[new_id as usize], false);
            }
            if ((entry >> 32) as u32) == h32 && eq(&self.items[id as usize]) {
                return (&mut self.items[id as usize], true);
            }
            idx = (idx + 1) & mask;
        }
    }
}

/// Canonical entry in `Deduper::layers`: stores `Arc<NodeCell>` directly when a
/// pure `Node::Sequence` owns the slice, collapsing layer and sequence-node
/// interning into a single hash lookup and a single `Arc::clone`.
enum LayerCanon {
    Cell(Arc<NodeCell>),
    Layers(Arc<[Layer]>),
}

impl LayerCanon {
    #[inline(always)]
    fn layers(&self) -> &Arc<[Layer]> {
        match self {
            Self::Cell(cell) => match &cell.kind {
                Node::Sequence(layers) => layers,
                _ => unreachable!("LayerCanon::Cell always holds Node::Sequence"),
            },
            Self::Layers(layers) => layers,
        }
    }
}

/// Lightweight open-addressed set of non-zero pointer addresses (`usize`),
/// avoiding `std::collections::HashSet` growth churn during cold-eval scans.
#[derive(Default)]
struct PtrSet {
    slots: Vec<usize>,
    len: usize,
}

impl PtrSet {
    #[inline]
    fn clear(&mut self) {
        if self.len > 0 {
            self.slots.fill(0);
            self.len = 0;
        }
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    fn capacity(&self) -> usize {
        self.slots.len()
    }

    #[inline(always)]
    fn hash(ptr: usize) -> usize {
        ((ptr as u64 >> 3).wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 16) as usize
    }

    fn grow(&mut self) {
        let next_len = (self.slots.len() * 2).max(64);
        let mut next = vec![0usize; next_len];
        let mask = next_len - 1;
        for &p in &self.slots {
            if p != 0 {
                let mut idx = Self::hash(p) & mask;
                while next[idx] != 0 {
                    idx = (idx + 1) & mask;
                }
                next[idx] = p;
            }
        }
        self.slots = next;
    }

    #[inline]
    fn insert(&mut self, ptr: usize) -> bool {
        debug_assert!(ptr != 0);
        if self.len * 2 >= self.slots.len() {
            self.grow();
        }
        let mask = self.slots.len() - 1;
        let mut idx = Self::hash(ptr) & mask;
        loop {
            let slot = self.slots[idx];
            if slot == 0 {
                self.slots[idx] = ptr;
                self.len += 1;
                return true;
            }
            if slot == ptr {
                return false;
            }
            idx = (idx + 1) & mask;
        }
    }

    #[inline]
    fn contains(&self, ptr: usize) -> bool {
        if self.len == 0 {
            return false;
        }
        let mask = self.slots.len() - 1;
        let mut idx = Self::hash(ptr) & mask;
        loop {
            let slot = self.slots[idx];
            if slot == 0 {
                return false;
            }
            if slot == ptr {
                return true;
            }
            idx = (idx + 1) & mask;
        }
    }
}

/// Lightweight open-addressed map from non-zero pointer addresses (`usize`) to `V`.
/// Keeps values in a dense `Vec<V>` indexed by `u32` so `clear()` drops only the
/// live entries and resets `slots` with a single vectorized fill.
struct PtrMap<V> {
    slots: Vec<(usize, u32)>,
    vals: Vec<V>,
}

impl<V> Default for PtrMap<V> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            vals: Vec::new(),
        }
    }
}

impl<V> PtrMap<V> {
    #[inline]
    fn clear(&mut self) {
        if !self.vals.is_empty() {
            self.vals.clear();
            self.slots.fill((0, 0));
        }
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.vals.is_empty()
    }

    #[inline]
    fn capacity(&self) -> usize {
        self.slots.len()
    }

    fn reserve(&mut self, additional: usize) {
        if additional == 0 {
            return;
        }
        self.vals.reserve(additional);
        let target = self
            .vals
            .len()
            .saturating_add(additional)
            .saturating_mul(2)
            .max(32)
            .next_power_of_two();
        if self.slots.len() < target {
            self.grow_to(target);
        }
    }

    #[cold]
    #[inline(never)]
    fn grow_to(&mut self, next_len: usize) {
        let mut next = vec![(0usize, 0u32); next_len];
        let mask = next_len - 1;
        for &(ptr, id) in &self.slots {
            if ptr != 0 {
                let mut idx = PtrSet::hash(ptr) & mask;
                while next[idx].0 != 0 {
                    idx = (idx + 1) & mask;
                }
                next[idx] = (ptr, id);
            }
        }
        self.slots = next;
    }

    #[inline]
    fn insert(&mut self, ptr: usize, val: V) {
        debug_assert!(ptr != 0);
        if self.vals.len() * 2 >= self.slots.len() {
            let next_len = (self.slots.len() * 2).max(32);
            self.grow_to(next_len);
        }
        let mask = self.slots.len() - 1;
        let mut idx = PtrSet::hash(ptr) & mask;
        loop {
            let (slot_ptr, slot_id) = self.slots[idx];
            if slot_ptr == 0 {
                let id = self.vals.len() as u32;
                self.vals.push(val);
                self.slots[idx] = (ptr, id);
                return;
            }
            if slot_ptr == ptr {
                self.vals[slot_id as usize] = val;
                return;
            }
            idx = (idx + 1) & mask;
        }
    }

    #[inline]
    fn get(&self, ptr: usize) -> Option<&V> {
        if self.vals.is_empty() {
            return None;
        }
        let mask = self.slots.len() - 1;
        let mut idx = PtrSet::hash(ptr) & mask;
        loop {
            let (slot_ptr, slot_id) = self.slots[idx];
            if slot_ptr == 0 {
                return None;
            }
            if slot_ptr == ptr {
                return Some(&self.vals[slot_id as usize]);
            }
            idx = (idx + 1) & mask;
        }
    }

    #[inline]
    fn contains_key(&self, ptr: usize) -> bool {
        self.get(ptr).is_some()
    }
}

#[inline(always)]
fn node_intern_tag(node: &Node) -> usize {
    match node {
        Node::Sequence(_) => 1,
        Node::Pure(_) => 2,
        Node::PureAt(_) => 3,
        Node::FastSmall(..) => 4,
        Node::ShiftSmall(..) => 5,
        Node::ControlMap(..) => 6,
        Node::SplitQueries(..) => 7,
        Node::Fast(_) => 8,
        Node::Shift(_) => 9,
        Node::Degrade(_) => 10,
        Node::Stack(_) => 11,
        Node::StackPure(_) => 12,
        Node::SlowCat(_) => 13,
        Node::SlowCatPure(_) => 14,
        Node::SlowCatPrime(_) => 15,
        Node::SlowCatPrimePure(_) => 16,
        Node::Constants(_) => 17,
        Node::AppWave(_) => 18,
        _ => 0,
    }
}

#[cold]
#[inline(never)]
fn grow_val_table(val_table: &mut Vec<u64>, needed: usize) {
    let mut next_len = (val_table.len() * 2).max(64);
    while needed * 2 >= next_len {
        next_len *= 2;
    }
    let mut next = vec![EMPTY_SLOT; next_len];
    let next_mask = next_len - 1;
    for &entry in val_table.iter() {
        if (entry as u32) != u32::MAX {
            let mut idx = ((entry >> 32) as usize) & next_mask;
            while (next[idx] as u32) != u32::MAX {
                idx = (idx + 1) & next_mask;
            }
            next[idx] = entry;
        }
    }
    *val_table = next;
}

#[derive(Default)]
struct ScanScratch {
    seen_nodes: PtrSet,
    seen_layers: PtrSet,
    node_count: usize,
    seq_count: usize,
    layer_count: usize,
    node_kind_counts: [u32; 19],
    seen_ptrs: PtrSet,
    /// `(src_idx, map_val_start, map_val_len)` for each unique built-in map.
    unique_maps: Vec<(u32, u32, u16)>,
    map_table: Vec<u64>,
    /// `(ref_count, pool_remap_idx)` for each distinct value id.
    val_meta: Vec<(u32, u16)>,
    val_table: Vec<u64>,
    map_val_ids: Vec<u32>,
    detached: Vec<usize>,
}

impl ScanScratch {
    fn clear(&mut self) {
        self.seen_nodes.clear();
        self.seen_layers.clear();
        self.node_count = 0;
        self.seq_count = 0;
        self.layer_count = 0;
        self.node_kind_counts = [0; 19];
        self.seen_ptrs.clear();
        self.unique_maps.clear();
        self.map_table.clear();
        self.val_meta.clear();
        self.val_table.clear();
        self.map_val_ids.clear();
        self.detached.clear();
    }

    fn is_small(&self) -> bool {
        self.seen_nodes.capacity() <= 512
            && self.seen_layers.capacity() <= 512
            && self.seen_ptrs.capacity() <= 512
            && self.unique_maps.capacity() <= 512
            && self.map_table.capacity() <= 1024
            && self.val_meta.capacity() <= 512
            && self.val_table.capacity() <= 1024
            && self.map_val_ids.capacity() <= 1024
            && self.detached.capacity() <= 512
    }
}

/// Like [`dedup_score`], pre-seeding already-canonical node references reused
/// from a prior build so incremental edits skip re-walking unchanged subtrees.
pub fn dedup_score_with_reused(
    root: &mut Pattern,
    reused: &[crate::PatternNodeRef],
    mut remap_extras: impl FnMut(&mut dyn FnMut(&mut crate::PatternNodeRef)),
) {
    // Tiny single-track scores have nothing to share across tracks or layers.
    if is_tiny_graph(root.node_arc()) {
        return;
    }
    let mut deduper = SPARE_DEDUPER
        .try_with(|cell| cell.borrow_mut().take())
        .ok()
        .flatten()
        .unwrap_or_default();
    let mut scan = SPARE_SCAN
        .try_with(|cell| cell.borrow_mut().take())
        .ok()
        .flatten()
        .unwrap_or_default();
    if !reused.is_empty() {
        deduper.visited.reserve(reused.len());
        for node_ref in reused {
            deduper
                .visited
                .insert(node_ref.identity(), Arc::clone(&node_ref.0));
        }
    }
    let (builtins_pre_deduped, unpooled_builtins) = collect_and_pool_builtins(
        root.node_arc(),
        &deduper.visited,
        &mut scan,
        &mut deduper.pooled_builtins,
    );
    deduper.builtins_pre_deduped = builtins_pre_deduped || unpooled_builtins <= 1;
    deduper.node_kind_counts = scan.node_kind_counts;
    deduper.seq_count = scan.seq_count;
    // Temporarily detach extra handles (such as memo parts) that point to
    // newly-built nodes reachable from `root` so their refcount drops to 1 and
    // `Arc::get_mut` can deduplicate them in place without reallocating.
    let has_reused = !deduper.visited.is_empty();
    let has_shared = !scan.seen_nodes.is_empty();
    if has_shared {
        let silence_arc = Arc::clone(crate::silence().node_arc());
        remap_extras(&mut |node_ref: &mut crate::PatternNodeRef| {
            let id = node_ref.identity();
            if scan.seen_nodes.contains(id) && (!has_reused || !deduper.visited.contains_key(id)) {
                scan.detached.push(id);
                deduper.detached_ids.insert(id);
                node_ref.0 = Arc::clone(&silence_arc);
            } else {
                scan.detached.push(0);
            }
        });
    }
    let cap = scan.node_count;
    if !deduper.detached_ids.is_empty() {
        deduper.visited.reserve(deduper.detached_ids.len);
    }
    let seq_count = scan.seq_count;
    let layer_count = scan.layer_count;
    if layer_count >= 2 {
        let est = (layer_count / 2).max(4);
        deduper.values.reserve(est);
        deduper.dresses.reserve((seq_count / 2).clamp(4, 64));
        deduper.compact_grids.reserve(est);
    }
    if seq_count >= 2 {
        deduper.layers.reserve((seq_count / 2).max(4));
    }
    if cap >= 2 {
        deduper.nodes.reserve((cap / 2).max(8));
    }
    deduper.dedup_pattern(root);
    if !scan.detached.is_empty() || !deduper.visited.is_empty() {
        let mut idx = 0usize;
        let mut remap = |node_ref: &mut crate::PatternNodeRef| {
            let orig = scan.detached.get(idx).copied().unwrap_or(0);
            idx += 1;
            let lookup = if orig != 0 { orig } else { node_ref.identity() };
            if let Some(canon) = deduper.visited.get(lookup) {
                node_ref.0 = Arc::clone(canon);
            } else {
                debug_assert_eq!(orig, 0, "detached reachable node missing from visited");
            }
        };
        remap_extras(&mut remap);
        if !scan.detached.is_empty() {
            debug_assert_eq!(
                idx,
                scan.detached.len(),
                "remap_extras visited a different number of handles on the second pass"
            );
        }
    }
    deduper.clear();
    scan.clear();
    if deduper.is_small() {
        let _ = SPARE_DEDUPER.try_with(|cell| *cell.borrow_mut() = Some(deduper));
    }
    if scan.is_small() {
        let _ = SPARE_SCAN.try_with(|cell| *cell.borrow_mut() = Some(scan));
    }
}

/// Fast read-only pre-scan collecting surviving unpooled built-in maps so value
/// pooling runs in a single pass with zero duplicate node/dress/layer rebuilds.
#[allow(clippy::mutable_key_type)]
fn collect_and_pool_builtins<'a>(
    root: &'a Arc<NodeCell>,
    skip_nodes: &PtrMap<Arc<NodeCell>>,
    scan: &mut ScanScratch,
    out: &mut PtrMap<Arc<dyn BuiltinEntries>>,
) -> (bool, usize) {
    let ScanScratch {
        seen_nodes,
        seen_layers,
        node_count,
        seq_count,
        layer_count,
        node_kind_counts,
        seen_ptrs,
        unique_maps,
        map_table,
        val_meta,
        val_table,
        map_val_ids,
        ..
    } = scan;
    type UnpooledMap<'b> = &'b Arc<dyn BuiltinEntries>;
    struct SmallRefVec<'a> {
        inline: [Option<UnpooledMap<'a>>; 128],
        len: usize,
        overflow: Vec<UnpooledMap<'a>>,
    }
    impl<'a> SmallRefVec<'a> {
        #[inline(always)]
        fn push(&mut self, item: UnpooledMap<'a>) {
            if self.overflow.is_empty() && self.len < self.inline.len() {
                self.inline[self.len] = Some(item);
                self.len += 1;
            } else {
                if self.overflow.is_empty() {
                    self.overflow.reserve(256);
                    for slot in &self.inline[..self.len] {
                        self.overflow.push(slot.expect("inline slot"));
                    }
                }
                self.overflow.push(item);
            }
        }
        #[inline(always)]
        fn len(&self) -> usize {
            if self.overflow.is_empty() {
                self.len
            } else {
                self.overflow.len()
            }
        }
        #[inline(always)]
        fn get(&self, idx: usize) -> UnpooledMap<'a> {
            if self.overflow.is_empty() {
                self.inline[idx].expect("inline slot")
            } else {
                self.overflow[idx]
            }
        }
    }
    let mut unpooled: SmallRefVec<'a> = SmallRefVec {
        inline: [None; 128],
        len: 0,
        overflow: Vec::new(),
    };
    struct SmallNodeStack<'a> {
        inline: [Option<&'a Arc<NodeCell>>; 32],
        len: usize,
        overflow: Vec<&'a Arc<NodeCell>>,
    }
    impl<'a> SmallNodeStack<'a> {
        #[inline]
        fn push(&mut self, cell: &'a Arc<NodeCell>) {
            if self.overflow.is_empty() && self.len < self.inline.len() {
                self.inline[self.len] = Some(cell);
                self.len += 1;
            } else {
                self.overflow.push(cell);
            }
        }
        #[inline]
        fn pop(&mut self) -> Option<&'a Arc<NodeCell>> {
            if let Some(cell) = self.overflow.pop() {
                return Some(cell);
            }
            if self.len > 0 {
                self.len -= 1;
                return self.inline[self.len].take();
            }
            None
        }
    }
    let mut stack = SmallNodeStack {
        inline: [None; 32],
        len: 0,
        overflow: Vec::new(),
    };
    stack.push(root);
    let has_skip = !skip_nodes.is_empty();
    let mut last_dress = 0usize;
    let mut last_values = 0usize;
    while let Some(cell) = stack.pop() {
        if Arc::strong_count(cell) > 1 {
            let id = Arc::as_ptr(cell) as usize;
            if (has_skip && skip_nodes.contains_key(id)) || !seen_nodes.insert(id) {
                continue;
            }
        }
        *node_count += 1;
        node_kind_counts[node_intern_tag(&cell.kind)] += 1;
        match &cell.kind {
            Node::Sequence(layers) => {
                let visit = if Arc::strong_count(layers) == 1 {
                    true
                } else {
                    let lid = Arc::as_ptr(layers) as *const Layer as usize;
                    seen_layers.insert(lid)
                };
                if visit {
                    *seq_count += 1;
                    *layer_count += layers.len();
                    for layer in layers.iter() {
                        layer.for_each_unpooled_builtin(
                            &mut last_dress,
                            &mut last_values,
                            &mut |ptr| seen_ptrs.insert(ptr),
                            &mut |entries, _, _| unpooled.push(entries),
                        );
                    }
                }
            }
            Node::Pure(val) => {
                if let Value::Object(map) = val
                    && let Some(entries) = map.as_builtin()
                    && let Slice::Builtin(keys, _) = entries.as_slice()
                    && (2..=MAX_BUILTIN_MAP_LEN).contains(&keys.len())
                    && (Arc::strong_count(entries) == 1
                        || seen_ptrs.insert(Arc::as_ptr(entries) as *const () as usize))
                {
                    unpooled.push(entries);
                }
            }
            Node::PureAt(boxed) => {
                if let Value::Object(map) = &boxed.0
                    && let Some(entries) = map.as_builtin()
                    && let Slice::Builtin(keys, _) = entries.as_slice()
                    && (2..=MAX_BUILTIN_MAP_LEN).contains(&keys.len())
                    && (Arc::strong_count(entries) == 1
                        || seen_ptrs.insert(Arc::as_ptr(entries) as *const () as usize))
                {
                    unpooled.push(entries);
                }
            }
            Node::Constants(node) => {
                stack.push(node.0.node_arc());
                for m in node.1.iter() {
                    if let Value::Object(map) = &m.value
                        && let Some(entries) = map.as_builtin()
                        && let Slice::Builtin(keys, _) = entries.as_slice()
                        && (2..=MAX_BUILTIN_MAP_LEN).contains(&keys.len())
                        && (Arc::strong_count(entries) == 1
                            || seen_ptrs.insert(Arc::as_ptr(entries) as *const () as usize))
                    {
                        unpooled.push(entries);
                    }
                }
            }
            Node::Stack(pats) | Node::SlowCat(pats) | Node::SlowCatPrime(pats) => {
                for p in pats.iter() {
                    stack.push(p.node_arc());
                }
            }
            Node::StackPure(cells) | Node::SlowCatPure(cells) | Node::SlowCatPrimePure(cells) => {
                for c in cells.iter() {
                    stack.push(c);
                }
            }
            Node::FastSmall(inner, ..)
            | Node::ShiftSmall(inner, ..)
            | Node::ControlMap(inner, ..)
            | Node::SplitQueries(inner, _)
            | Node::Join(inner, ..) => stack.push(inner),
            Node::SplitQueriesBoxed(p)
            | Node::Rev(p)
            | Node::StepJoin(p)
            | Node::PolyJoin(p)
            | Node::PurePattern(p)
            | Node::SortHapsByPart(p)
            | Node::Collect(p)
            | Node::CycleOnsetIndex(p)
            | Node::CycleOnsetCount(p) => stack.push(p.node_arc()),
            Node::Fast(n) => stack.push(n.0.node_arc()),
            Node::Shift(n) => stack.push(n.0.node_arc()),
            Node::AppWave(app) => stack.push(app.inner.node_arc()),
            Node::UiVisual(node) => stack.push(node.inner.node_arc()),
            Node::Degrade(b) => stack.push(b.0.node_arc()),
            Node::Cpm(b) => stack.push(b.0.node_arc()),
            Node::AddContext(b) => stack.push(b.0.node_arc()),
            Node::FilterValues(b) => stack.push(b.0.node_arc()),
            Node::FilterHaps(b) => stack.push(b.0.node_arc()),
            Node::MapHapsOwned(b) => stack.push(b.0.node_arc()),
            Node::MapHapsNative(b) => stack.push(b.0.node_arc()),
            Node::ExpandHapsNative(b) => stack.push(b.0.node_arc()),
            Node::FmapNative(b) => stack.push(b.0.node_arc()),
            Node::WithRandSeed(b) => stack.push(b.0.node_arc()),
            Node::FastGap(b) => stack.push(b.0.node_arc()),
            Node::RepeatCycles(b) => stack.push(b.0.node_arc()),
            Node::LoopAt(b) => stack.push(b.0.node_arc()),
            Node::ControlMapBoxed(b) => stack.push(b.0.node_arc()),
            Node::JoinBoxed(b) => stack.push(b.0.node_arc()),
            Node::WithQueryTime(b) | Node::WithHapTime(b) => stack.push(b.0.node_arc()),
            Node::WithQuerySpan(b) | Node::WithHapSpan(b) => stack.push(b.0.node_arc()),
            Node::MapHapsWithState(b) => stack.push(b.0.node_arc()),
            Node::InnerJoin(b) => stack.push(b.0.node_arc()),
            Node::PatternOfPure(b) => stack.push(b.0.node_arc()),
            Node::PatternOfPureOperand(b, _) => {
                stack.push(b.0.node_arc());
                stack.push(b.1.node_arc());
            }
            Node::PatternOfHap(b) => {
                stack.push(b.0.node_arc());
                for p in b.1.iter() {
                    stack.push(p.node_arc());
                }
            }
            Node::AppLeft(a) => {
                stack.push(a.0.node_arc());
                stack.push(a.1.node_arc());
            }
            Node::AppRight(a) | Node::AppBoth(a) => {
                stack.push(a.0.node_arc());
                stack.push(a.1.node_arc());
            }
            Node::Pick(pick) => {
                stack.push(pick.selector.node_arc());
                for p in pick.lookup.patterns() {
                    stack.push(p.node_arc());
                }
            }
            Node::PickPatternified { patterns, .. } | Node::Arp(patterns) => {
                stack.push(patterns.0.node_arc());
                stack.push(patterns.1.node_arc());
            }
            Node::ChooseCycles(b) => {
                for p in b.0.iter() {
                    stack.push(p.node_arc());
                }
            }
            Node::WChoose(c) => {
                stack.push(c.chooser.node_arc());
                stack.push(c.total.node_arc());
                for p in c.cumulative.iter().chain(c.values.iter()) {
                    stack.push(p.node_arc());
                }
            }
            Node::XenPatterns(b) => {
                stack.push(b.0.node_arc());
                for p in b.1.iter() {
                    stack.push(p.node_arc());
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
            | Node::Timeline(..) => {}
            #[cfg(non_send_node_mutation)]
            Node::NonSendMutation(_) => {}
        }
    }
    let unpooled_len = unpooled.len();
    if unpooled_len < 4 {
        return (false, unpooled_len);
    }
    // Necessary upper-bound check before allocating or hashing value/map tables:
    // pooling requires `saved >= 2048` and `saved >= poolable.len() * 48`,
    // while the shared pool costs at least `16 + 24 * 2 = 64 B`. If even
    // zero-cost perfect value sharing cannot clear both thresholds, skip
    // pooling immediately and let `dedup_cell_mut` intern any exact duplicates.
    let mut total_vals = 0usize;
    let mut max_saved = 0usize;
    let mut max_surplus = 0usize;
    for i in 0..unpooled_len {
        let len = unpooled.get(i).keys().len();
        total_vals += len;
        let old_b = 16 + ((26 * len + 7) & !7);
        let new_b = 32 + ((4 * len + 7) & !7);
        let gain = old_b.saturating_sub(new_b);
        max_saved += gain;
        max_surplus += gain.saturating_sub(48);
    }
    // Value pooling only outperforms exact map interning when many distinct
    // maps share overlapping value subsets, which carries a real pool table
    // cost (`16 + 24 * pool_len`) on top of the `2048 B` net savings floor.
    if max_saved < 3072 || max_surplus < 64 {
        return (false, unpooled_len);
    }
    struct SmallValVec<'b> {
        inline: [Option<&'b Value>; 128],
        len: usize,
        overflow: Vec<&'b Value>,
    }
    impl<'b> SmallValVec<'b> {
        #[inline(always)]
        fn push(&mut self, item: &'b Value, est: usize) {
            if self.overflow.is_empty() && self.len < self.inline.len() {
                self.inline[self.len] = Some(item);
                self.len += 1;
            } else {
                if self.overflow.is_empty() {
                    self.overflow.reserve(est.max(256));
                    for slot in &self.inline[..self.len] {
                        self.overflow.push(slot.expect("inline val"));
                    }
                }
                self.overflow.push(item);
            }
        }
        #[inline(always)]
        fn get(&self, idx: usize) -> &'b Value {
            if self.overflow.is_empty() {
                self.inline[idx].expect("inline val")
            } else {
                self.overflow[idx]
            }
        }
    }
    let mut distinct_vals: SmallValVec<'a> = SmallValVec {
        inline: [None; 128],
        len: 0,
        overflow: Vec::new(),
    };
    let mut val_table_len = total_vals.clamp(32, 512).next_power_of_two();
    let mut val_mask = val_table_len - 1;
    val_table.clear();
    val_table.resize(val_table_len, EMPTY_SLOT);
    let map_table_len = (unpooled_len.saturating_mul(2).max(16)).next_power_of_two();
    let map_mask = map_table_len - 1;
    map_table.clear();
    map_table.resize(map_table_len, EMPTY_SLOT);
    struct StackBuf<T: Copy, const N: usize> {
        inline: [T; N],
        len: usize,
        overflow: Vec<T>,
    }
    impl<T: Copy, const N: usize> StackBuf<T, N> {
        #[inline(always)]
        fn new(zero: T) -> Self {
            Self {
                inline: [zero; N],
                len: 0,
                overflow: Vec::new(),
            }
        }
        #[inline(always)]
        fn push(&mut self, item: T) {
            if self.overflow.is_empty() && self.len < N {
                self.inline[self.len] = item;
                self.len += 1;
            } else {
                if self.overflow.is_empty() {
                    self.overflow.reserve(N * 2);
                    self.overflow.extend_from_slice(&self.inline[..self.len]);
                }
                self.overflow.push(item);
            }
        }
        #[inline(always)]
        fn as_slice(&self) -> &[T] {
            if self.overflow.is_empty() {
                &self.inline[..self.len]
            } else {
                &self.overflow
            }
        }
        #[inline(always)]
        fn len(&self) -> usize {
            if self.overflow.is_empty() {
                self.len
            } else {
                self.overflow.len()
            }
        }
        #[inline(always)]
        fn is_empty(&self) -> bool {
            self.len() == 0
        }
    }
    let mut duplicates = StackBuf::<(usize, u32), 16>::new((0, 0));
    let est_distinct = (total_vals / 2).clamp(16, 128);
    val_meta.reserve(est_distinct);
    map_val_ids.reserve(total_vals);
    unique_maps.reserve(unpooled_len);
    'maps: for src_idx in 0..unpooled_len {
        let map = unpooled.get(src_idx);
        let Slice::Builtin(keys, values) = map.as_slice() else {
            continue;
        };
        if (val_meta.len() + values.len()) * 2 >= val_table_len {
            grow_val_table(val_table, val_meta.len() + values.len());
            val_table_len = val_table.len();
            val_mask = val_table_len - 1;
        }
        let start = map_val_ids.len();
        for val in values.iter() {
            let h32 = match val {
                Value::F64(n) => {
                    let bits = n.to_bits();
                    mix_hash32(bits.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x101)
                }
                Value::Str(s) => {
                    let bytes = s.as_bytes();
                    let h = if bytes.len() <= 8 {
                        (crate::value::pack_le_u64(bytes) ^ ((bytes.len() as u64) << 56) ^ 0x202)
                            .wrapping_mul(0x517c_c1b7_2722_0a95)
                    } else {
                        let mut h =
                            (bytes.len() as u64).wrapping_mul(0x517c_c1b7_2722_0a95) ^ 0x202;
                        let (chunks, rem) = bytes.as_chunks::<8>();
                        for chunk in chunks {
                            let w = u64::from_ne_bytes(*chunk);
                            h = (h.rotate_left(5) ^ w).wrapping_mul(0x517c_c1b7_2722_0a95);
                        }
                        if !rem.is_empty() {
                            h = (h.rotate_left(5) ^ crate::value::pack_le_u64(rem))
                                .wrapping_mul(0x517c_c1b7_2722_0a95);
                        }
                        h
                    };
                    mix_hash32(h)
                }
                Value::Bool(b) => mix_hash32(0x303u64 ^ u64::from(*b)),
                Value::Null => mix_hash32(0x404),
                Value::Undefined => mix_hash32(0x505),
                _ => {
                    if !val.is_data(0) {
                        map_val_ids.truncate(start);
                        continue 'maps;
                    }
                    mix_hash32(val.data_hash())
                }
            };
            let tag = u64::from(h32) << 32;
            let mut slot = (h32 as usize) & val_mask;
            let id = loop {
                let entry = val_table[slot];
                let entry_id = entry as u32;
                if entry_id == u32::MAX {
                    let new_id = val_meta.len() as u32;
                    val_table[slot] = tag | u64::from(new_id);
                    val_meta.push((0, u16::MAX));
                    distinct_vals.push(val, est_distinct);
                    break new_id;
                }
                if ((entry >> 32) as u32) == h32 {
                    let other = distinct_vals.get(entry_id as usize);
                    let matched = match (val, other) {
                        (Value::F64(a), Value::F64(b)) => a.to_bits() == b.to_bits(),
                        (Value::Str(a), Value::Str(b)) => a == b,
                        (Value::Bool(a), Value::Bool(b)) => a == b,
                        (Value::Null, Value::Null) | (Value::Undefined, Value::Undefined) => true,
                        _ => other.same_data(val),
                    };
                    if matched {
                        break entry_id;
                    }
                }
                slot = (slot + 1) & val_mask;
            };
            map_val_ids.push(id);
        }
        let vids = &map_val_ids[start..];
        let mut mh = keys.len() as u64;
        for (&k, &v) in keys.iter().zip(vids.iter()) {
            let pair = (u64::from(k) << 32) | u64::from(v);
            mh = (mh.rotate_left(5) ^ pair).wrapping_mul(0x517c_c1b7_2722_0a95);
        }
        let mh32 = mix_hash32(mh);
        let mtag = u64::from(mh32) << 32;
        let mut mslot = (mh32 as usize) & map_mask;
        let existing = loop {
            let entry = map_table[mslot];
            let eid = entry as u32;
            if eid == u32::MAX {
                let new_u = unique_maps.len() as u32;
                map_table[mslot] = mtag | u64::from(new_u);
                break None;
            }
            if ((entry >> 32) as u32) == mh32 {
                let (other_src, os, olen) = unique_maps[eid as usize];
                let okeys = unpooled.get(other_src as usize).keys();
                if okeys == keys
                    && &map_val_ids[os as usize..os as usize + usize::from(olen)] == vids
                {
                    break Some(other_src);
                }
            }
            mslot = (mslot + 1) & map_mask;
        };
        if let Some(canon_src) = existing {
            let ptr = Arc::as_ptr(map) as *const () as usize;
            duplicates.push((ptr, canon_src));
            map_val_ids.truncate(start);
        } else {
            for &vid in vids {
                val_meta[vid as usize].0 += 1;
            }
            unique_maps.push((src_idx as u32, start as u32, values.len() as u16));
        }
    }
    if unique_maps.len() >= 4 {
        let mut poolable_idx = StackBuf::<u32, 128>::new(0);
        for (idx, &(_, start, len)) in unique_maps.iter().enumerate() {
            let len = usize::from(len);
            if len == 0 {
                continue;
            }
            let vids = &map_val_ids[start as usize..start as usize + len];
            let mut shared = 0usize;
            for &vid in vids {
                if val_meta[vid as usize].0 >= 2 {
                    shared += 1;
                }
            }
            let unique = len - shared;
            if 22 * shared > 2 * unique + 16 {
                poolable_idx.push(idx as u32);
            }
        }
        if poolable_idx.len() >= 2 {
            let mut pool_ids = StackBuf::<u32, 128>::new(0);
            let mut old_bytes = 0usize;
            let mut new_maps_bytes = 0usize;
            let mut overflow = false;
            'assign: for &idx in poolable_idx.as_slice() {
                let (_, start, len) = unique_maps[idx as usize];
                let len = usize::from(len);
                old_bytes += 16 + ((26 * len + 7) & !7);
                new_maps_bytes += 32 + ((4 * len + 7) & !7);
                for &vid in &map_val_ids[start as usize..start as usize + len] {
                    if val_meta[vid as usize].1 == u16::MAX {
                        let Ok(pidx) = u16::try_from(pool_ids.len()) else {
                            overflow = true;
                            break 'assign;
                        };
                        if pidx == u16::MAX {
                            overflow = true;
                            break 'assign;
                        }
                        val_meta[vid as usize].1 = pidx;
                        pool_ids.push(vid);
                    }
                }
            }
            let new_bytes = 16 + 24 * pool_ids.len() + new_maps_bytes;
            let saved = old_bytes.saturating_sub(new_bytes);
            if !overflow && saved >= 2048 && saved >= poolable_idx.len() * 48 {
                out.reserve(poolable_idx.len() + duplicates.len());
                let pool: Arc<[Value]> = pool_ids
                    .as_slice()
                    .iter()
                    .map(|&vid| distinct_vals.get(vid as usize).clone())
                    .collect();
                let mut vals_buf = [0u16; MAX_BUILTIN_MAP_LEN];
                for &idx in poolable_idx.as_slice() {
                    let (src_idx, start, len) = unique_maps[idx as usize];
                    let map = unpooled.get(src_idx as usize);
                    let keys = map.keys();
                    let len = usize::from(len);
                    for (i, &vid) in map_val_ids[start as usize..start as usize + len]
                        .iter()
                        .enumerate()
                    {
                        vals_buf[i] = val_meta[vid as usize].1;
                    }
                    let pooled = make_pooled_builtin_arc(Arc::clone(&pool), keys, &vals_buf[..len]);
                    out.insert(Arc::as_ptr(map) as *const () as usize, pooled);
                }
            }
        }
    }
    if !duplicates.is_empty() {
        out.reserve(duplicates.len());
    }
    for &(dup_ptr, canon_src) in duplicates.as_slice() {
        let canon = unpooled.get(canon_src as usize);
        let canon_ptr = Arc::as_ptr(canon) as *const () as usize;
        if let Some(pooled) = out.get(canon_ptr) {
            out.insert(dup_ptr, Arc::clone(pooled));
        } else {
            out.insert(dup_ptr, Arc::clone(canon));
        }
    }
    (true, unpooled_len)
}

#[derive(Default)]
struct Deduper {
    visited: PtrMap<Arc<NodeCell>>,
    detached_ids: PtrSet,
    seen_values: PtrMap<Arc<[Value]>>,
    seen_dresses: PtrMap<Arc<Dress>>,
    seen_layers: PtrMap<Arc<[Layer]>>,
    plain_extras_one: Option<(usize, Arc<PatternExtras>)>,
    plain_extras: FastMap<usize, Arc<PatternExtras>>,
    pooled_builtins: PtrMap<Arc<dyn BuiltinEntries>>,
    builtins_pre_deduped: bool,
    node_kind_counts: [u32; 19],
    seq_count: usize,
    builtins: InternTable<Arc<dyn BuiltinEntries>>,
    keys: InternTable<Arc<Keys>>,
    values: InternTable<Arc<[Value]>>,
    dresses: InternTable<Arc<Dress>>,
    compact_grids: InternTable<CompactGrid>,
    last_compact_grid: Option<(usize, CompactGrid)>,
    grids: InternTable<Arc<Grid>>,
    layers: InternTable<LayerCanon>,
    nodes: InternTable<Arc<NodeCell>>,
}

impl Deduper {
    fn clear(&mut self) {
        self.visited.clear();
        self.detached_ids.clear();
        self.seen_values.clear();
        self.seen_dresses.clear();
        self.seen_layers.clear();
        self.plain_extras_one = None;
        self.plain_extras.clear();
        self.pooled_builtins.clear();
        self.builtins_pre_deduped = false;
        self.node_kind_counts = [0; 19];
        self.seq_count = 0;
        self.builtins.clear();
        self.keys.clear();
        self.values.clear();
        self.dresses.clear();
        self.compact_grids.clear();
        self.last_compact_grid = None;
        self.grids.clear();
        self.layers.clear();
        self.nodes.clear();
    }

    fn is_small(&self) -> bool {
        self.visited.capacity() <= 512
            && self.detached_ids.capacity() <= 512
            && self.seen_values.capacity() <= 512
            && self.seen_dresses.capacity() <= 512
            && self.seen_layers.capacity() <= 512
            && self.plain_extras.capacity() <= 256
            && self.pooled_builtins.capacity() <= 512
            && self.builtins.capacity() <= 512
            && self.keys.capacity() <= 512
            && self.values.capacity() <= 512
            && self.dresses.capacity() <= 512
            && self.compact_grids.capacity() <= 512
            && self.grids.capacity() <= 512
            && self.layers.capacity() <= 512
            && self.nodes.capacity() <= 512
    }

    #[inline]
    fn intern_builtin_ref(
        &mut self,
        entries: &Arc<dyn BuiltinEntries>,
        known_data: bool,
    ) -> Option<Arc<dyn BuiltinEntries>> {
        if !self.pooled_builtins.is_empty() {
            let ptr = Arc::as_ptr(entries) as *const () as usize;
            if let Some(pooled) = self.pooled_builtins.get(ptr) {
                return Some(Arc::clone(pooled));
            }
        }
        if self.builtins_pre_deduped
            && matches!(entries.as_slice(), Slice::Builtin(k, _) if k.len() >= 2 && k.len() <= MAX_BUILTIN_MAP_LEN)
        {
            return None;
        }
        if !known_data && !entries.as_slice().all_values_data() {
            return None;
        }
        let h = hash_builtin_entries(entries.as_ref());
        let (canon, existed) = self.builtins.intern(
            h,
            |other| {
                Arc::ptr_eq(other, entries)
                    || same_builtin_entries(other.as_ref(), entries.as_ref())
            },
            || Arc::clone(entries),
        );
        (existed && !Arc::ptr_eq(canon, entries)).then(|| Arc::clone(canon))
    }

    fn dedup_value_mut(&mut self, value: &mut Value) {
        if let Value::Object(map) = value
            && map.as_builtin().is_some()
        {
            map.dedup_builtin(|entries, made| self.intern_builtin_ref(entries, made));
        }
    }

    fn dedup_value(&mut self, value: &Value) -> Option<Value> {
        if let Value::Object(map) = value {
            return map
                .dedup_builtin_cloned(|entries, made| self.intern_builtin_ref(entries, made))
                .map(Value::Object);
        }
        None
    }

    fn dedup_values_mut(&mut self, values: &mut Arc<[Value]>, may_have_objects: bool) {
        if values.is_empty() {
            return;
        }
        if let Some(slice) = Arc::get_mut(values) {
            let mut hasher = WordHasher::default();
            slice.len().hash(&mut hasher);
            let mut can_alias = true;
            if may_have_objects {
                for val in slice.iter_mut() {
                    if let Value::Object(map) = val
                        && map.as_builtin().is_some()
                    {
                        map.dedup_builtin(|entries, made| self.intern_builtin_ref(entries, made));
                        if map
                            .as_builtin()
                            .is_some_and(|entries| Arc::strong_count(entries) == 1)
                        {
                            can_alias = false;
                        }
                    }
                    if can_alias {
                        hash_interned_value(val, &mut hasher);
                    }
                }
            } else {
                for val in slice.iter() {
                    hash_interned_value(val, &mut hasher);
                }
            }
            if can_alias {
                let h = hasher.finish();
                let (canon, existed) = self.values.intern(
                    h,
                    |other| same_value_slice(other, values),
                    || Arc::clone(values),
                );
                if existed {
                    *values = Arc::clone(canon);
                }
            }
            return;
        }
        let next = self.dedup_values(values, may_have_objects);
        if !Arc::ptr_eq(values, &next) {
            *values = next;
        }
    }

    fn dedup_values(&mut self, values: &Arc<[Value]>, may_have_objects: bool) -> Arc<[Value]> {
        if values.is_empty() {
            return Arc::clone(values);
        }
        let id = Arc::as_ptr(values) as *const Value as usize;
        if let Some(existing) = self.seen_values.get(id) {
            return Arc::clone(existing);
        }
        let mut owned: Option<Vec<Value>> = None;
        if may_have_objects {
            for (idx, val) in values.iter().enumerate() {
                if let Some(next_val) = self.dedup_value(val) {
                    let vec = owned.get_or_insert_with(|| values.to_vec());
                    vec[idx] = next_val;
                }
            }
        }
        let canon = match owned {
            Some(vec) => {
                let h = hash_value_slice(&vec);
                let (canon, _) = self.values.intern_with(
                    h,
                    vec,
                    |other, cand| same_value_slice(other, cand),
                    Arc::from,
                );
                Arc::clone(canon)
            }
            None => {
                let h = hash_value_slice(values);
                let (canon, _) = self.values.intern(
                    h,
                    |other| Arc::ptr_eq(other, values) || same_value_slice(other, values),
                    || Arc::clone(values),
                );
                Arc::clone(canon)
            }
        };
        self.seen_values.insert(id, Arc::clone(&canon));
        canon
    }

    fn dedup_dress_mut(&mut self, dress: &mut Arc<Dress>) {
        if let Some(inner) = Arc::get_mut(dress) {
            let Self {
                builtins,
                keys,
                pooled_builtins,
                builtins_pre_deduped,
                ..
            } = self;
            let pre = *builtins_pre_deduped;
            let mut dedup_builtin = |entries: &Arc<dyn BuiltinEntries>, known_data: bool| {
                if !pooled_builtins.is_empty() {
                    let ptr = Arc::as_ptr(entries) as *const () as usize;
                    if let Some(pooled) = pooled_builtins.get(ptr) {
                        return Some(Arc::clone(pooled));
                    }
                }
                if pre
                    && matches!(entries.as_slice(), Slice::Builtin(k, _) if k.len() >= 2 && k.len() <= MAX_BUILTIN_MAP_LEN)
                {
                    return None;
                }
                if !known_data && !entries.as_slice().all_values_data() {
                    return None;
                }
                let h = hash_builtin_entries(entries.as_ref());
                let (canon, existed) = builtins.intern(
                    h,
                    |other| {
                        Arc::ptr_eq(other, entries)
                            || same_builtin_entries(other.as_ref(), entries.as_ref())
                    },
                    || Arc::clone(entries),
                );
                (existed && !Arc::ptr_eq(canon, entries)).then(|| Arc::clone(canon))
            };
            let mut dedup_keys = |k: &Arc<Keys>| {
                if k.scale_definition.is_some() || k.pick_lookup.is_some() {
                    return None;
                }
                let h = hash_keys(k);
                let (canon, existed) = keys.intern(
                    h,
                    |other| Arc::ptr_eq(other, k) || same_keys(other, k),
                    || Arc::clone(k),
                );
                (existed && !Arc::ptr_eq(canon, k)).then(|| Arc::clone(canon))
            };
            inner.dedup_inner_mut(&mut dedup_builtin, &mut dedup_keys);
            if dress.can_intern() && dress.can_alias() {
                let h = hash_with(|st| dress.hash_intern(st));
                let (canon, existed) =
                    self.dresses
                        .intern(h, |other| other.eq_intern(dress), || Arc::clone(dress));
                if existed {
                    *dress = Arc::clone(canon);
                }
            }
            return;
        }
        let next = self.dedup_dress(dress);
        if !Arc::ptr_eq(dress, &next) {
            *dress = next;
        }
    }

    fn dedup_dress(&mut self, dress: &Arc<Dress>) -> Arc<Dress> {
        let id = Arc::as_ptr(dress) as usize;
        if let Some(existing) = self.seen_dresses.get(id) {
            return Arc::clone(existing);
        }
        let updated = self.dedup_dress_inner_opt(dress);
        let canon = match updated {
            Some(updated) => {
                if updated.can_intern() {
                    let h = hash_with(|st| updated.hash_intern(st));
                    let (canon, _) = self.dresses.intern_with(
                        h,
                        updated,
                        |other, cand| other.eq_intern(cand),
                        Arc::new,
                    );
                    Arc::clone(canon)
                } else {
                    Arc::new(updated)
                }
            }
            None => {
                if dress.can_intern() {
                    let h = hash_with(|st| dress.hash_intern(st));
                    let (canon, _) = self.dresses.intern(
                        h,
                        |other| Arc::ptr_eq(other, dress) || other.eq_intern(dress),
                        || Arc::clone(dress),
                    );
                    Arc::clone(canon)
                } else {
                    Arc::clone(dress)
                }
            }
        };
        self.seen_dresses.insert(id, Arc::clone(&canon));
        canon
    }

    fn dedup_dress_inner_opt(&mut self, dress: &Arc<Dress>) -> Option<Dress> {
        let Self {
            builtins,
            keys,
            pooled_builtins,
            builtins_pre_deduped,
            ..
        } = self;
        let pre = *builtins_pre_deduped;
        let mut dedup_builtin = |entries: &Arc<dyn BuiltinEntries>, known_data: bool| {
            if !pooled_builtins.is_empty() {
                let ptr = Arc::as_ptr(entries) as *const () as usize;
                if let Some(pooled) = pooled_builtins.get(ptr) {
                    return Some(Arc::clone(pooled));
                }
            }
            if pre
                && matches!(entries.as_slice(), Slice::Builtin(k, _) if k.len() >= 2 && k.len() <= MAX_BUILTIN_MAP_LEN)
            {
                return None;
            }
            if !known_data && !entries.as_slice().all_values_data() {
                return None;
            }
            let h = hash_builtin_entries(entries.as_ref());
            let (canon, existed) = builtins.intern(
                h,
                |other| {
                    Arc::ptr_eq(other, entries)
                        || same_builtin_entries(other.as_ref(), entries.as_ref())
                },
                || Arc::clone(entries),
            );
            (existed && !Arc::ptr_eq(canon, entries)).then(|| Arc::clone(canon))
        };
        let mut dedup_keys = |k: &Arc<Keys>| {
            if k.scale_definition.is_some() || k.pick_lookup.is_some() {
                return None;
            }
            let h = hash_keys(k);
            let (canon, existed) = keys.intern(
                h,
                |other| Arc::ptr_eq(other, k) || same_keys(other, k),
                || Arc::clone(k),
            );
            (existed && !Arc::ptr_eq(canon, k)).then(|| Arc::clone(canon))
        };
        dress.dedup_inner(&mut dedup_builtin, &mut dedup_keys)
    }

    fn dedup_compact_grid(&mut self, grid: &CompactGrid) -> CompactGrid {
        let ptr = grid.arc_ptr();
        if ptr != 0
            && let Some((seen_ptr, canon)) = &self.last_compact_grid
            && *seen_ptr == ptr
        {
            return canon.clone();
        }
        let h = hash_compact_grid(grid);
        let (canon, existed) =
            self.compact_grids
                .intern(h, |other| same_compact_grid(other, grid), || grid.clone());
        let canon = canon.clone();
        if existed && ptr != 0 {
            self.last_compact_grid = Some((ptr, canon.clone()));
        }
        canon
    }

    fn dedup_compact_grid_mut(&mut self, grid: &mut CompactGrid) {
        let ptr = grid.arc_ptr();
        if ptr != 0
            && let Some((seen_ptr, canon)) = &self.last_compact_grid
            && *seen_ptr == ptr
        {
            if !grid.ptr_eq(canon) {
                *grid = canon.clone();
            }
            return;
        }
        let h = hash_compact_grid(grid);
        let (canon, existed) =
            self.compact_grids
                .intern(h, |other| same_compact_grid(other, grid), || grid.clone());
        if existed && !grid.ptr_eq(canon) {
            let canon = canon.clone();
            if ptr != 0 {
                self.last_compact_grid = Some((ptr, canon.clone()));
            }
            *grid = canon;
        }
    }

    fn dedup_grid(&mut self, grid: &Arc<Grid>) -> Arc<Grid> {
        let h = hash_with(|st| grid.hash_intern(st));
        let (canon, _) = self.grids.intern(
            h,
            |other| Arc::ptr_eq(other, grid) || other.eq_intern(grid),
            || Arc::clone(grid),
        );
        Arc::clone(canon)
    }

    fn dedup_grid_mut(&mut self, grid: &mut Arc<Grid>) {
        let h = hash_with(|st| grid.hash_intern(st));
        let (canon, existed) = self.grids.intern(
            h,
            |other| Arc::ptr_eq(other, grid) || other.eq_intern(grid),
            || Arc::clone(grid),
        );
        if existed && !Arc::ptr_eq(grid, canon) {
            *grid = Arc::clone(canon);
        }
    }

    fn dedup_layers_mut(&mut self, layers: &mut Arc<[Layer]>) {
        if let Some(slice) = Arc::get_mut(layers) {
            for layer in slice.iter_mut() {
                layer.dedup_in_mut(self);
            }
            if self.seq_count >= 2 && layers.iter().all(Layer::can_intern) {
                let h = hash_layer_slice(layers);
                let (canon, existed) = self.layers.intern(
                    h,
                    |other| same_layer_slice(other.layers(), layers),
                    || LayerCanon::Layers(Arc::clone(layers)),
                );
                if existed {
                    *layers = Arc::clone(canon.layers());
                }
            }
            return;
        }
        let next = self.dedup_layers(layers);
        if !Arc::ptr_eq(layers, &next) {
            *layers = next;
        }
    }

    fn dedup_layers(&mut self, layers: &Arc<[Layer]>) -> Arc<[Layer]> {
        let id = Arc::as_ptr(layers) as *const Layer as usize;
        if let Some(existing) = self.seen_layers.get(id) {
            return Arc::clone(existing);
        }
        let canon: Arc<[Layer]> = if layers.len() <= 8 {
            let mut stack: [Option<Layer>; 8] = Default::default();
            let mut changed = false;
            for (idx, layer) in layers.iter().enumerate() {
                if let Some(updated_layer) = layer.dedup_in(self) {
                    if !changed {
                        for (dst, src) in stack[..idx].iter_mut().zip(&layers[..idx]) {
                            *dst = Some(src.clone());
                        }
                        changed = true;
                    }
                    stack[idx] = Some(updated_layer);
                } else if changed {
                    stack[idx] = Some(layer.clone());
                }
            }
            if changed {
                let len = layers.len();
                if stack[..len]
                    .iter()
                    .all(|slot| slot.as_ref().expect("layer present").can_intern())
                {
                    let h = hash_with(|st| {
                        len.hash(st);
                        for slot in &stack[..len] {
                            slot.as_ref().expect("layer present").hash_intern(st);
                        }
                    });
                    let (canon, _) = self.layers.intern_with(
                        h,
                        &mut stack[..len],
                        |other, cand| {
                            let other = other.layers();
                            other.len() == len
                                && other
                                    .iter()
                                    .zip(cand.iter())
                                    .all(|(a, b)| a.eq_intern(b.as_ref().expect("layer present")))
                        },
                        |cand| {
                            LayerCanon::Layers(
                                cand.iter_mut()
                                    .map(|slot| slot.take().expect("layer present"))
                                    .collect(),
                            )
                        },
                    );
                    Arc::clone(canon.layers())
                } else {
                    stack[..len]
                        .iter_mut()
                        .map(|slot| slot.take().expect("layer present"))
                        .collect()
                }
            } else if layers.iter().all(Layer::can_intern) {
                let h = hash_layer_slice(layers);
                let (canon, _) = self.layers.intern(
                    h,
                    |other| {
                        let other = other.layers();
                        Arc::ptr_eq(other, layers) || same_layer_slice(other, layers)
                    },
                    || LayerCanon::Layers(Arc::clone(layers)),
                );
                Arc::clone(canon.layers())
            } else {
                Arc::clone(layers)
            }
        } else {
            let mut owned: Option<Vec<Layer>> = None;
            for (idx, layer) in layers.iter().enumerate() {
                if let Some(updated_layer) = layer.dedup_in(self) {
                    let vec = owned.get_or_insert_with(|| layers.to_vec());
                    vec[idx] = updated_layer;
                }
            }
            match owned {
                Some(vec) => {
                    if vec.iter().all(Layer::can_intern) {
                        let h = hash_layer_slice(&vec);
                        let (canon, _) = self.layers.intern_with(
                            h,
                            vec,
                            |other, cand| same_layer_slice(other.layers(), cand),
                            |vec| LayerCanon::Layers(Arc::from(vec)),
                        );
                        Arc::clone(canon.layers())
                    } else {
                        Arc::from(vec)
                    }
                }
                None => {
                    if layers.iter().all(Layer::can_intern) {
                        let h = hash_layer_slice(layers);
                        let (canon, _) = self.layers.intern(
                            h,
                            |other| {
                                let other = other.layers();
                                Arc::ptr_eq(other, layers) || same_layer_slice(other, layers)
                            },
                            || LayerCanon::Layers(Arc::clone(layers)),
                        );
                        Arc::clone(canon.layers())
                    } else {
                        Arc::clone(layers)
                    }
                }
            }
        };
        self.seen_layers.insert(id, Arc::clone(&canon));
        canon
    }

    fn dedup_pattern(&mut self, pat: &mut Pattern) -> bool {
        let mut changed = false;
        let plain_extras_one = &mut self.plain_extras_one;
        let plain_extras = &mut self.plain_extras;
        pat.dedup_plain_extras(&mut |key, ex| {
            let ex_ptr = Arc::as_ptr(ex) as usize;
            if let Some((first_key, first_ex)) = plain_extras_one {
                if *first_key == key {
                    let canon_ptr = Arc::as_ptr(first_ex) as usize;
                    if ex_ptr != canon_ptr {
                        changed = true;
                        return Some(Arc::clone(first_ex));
                    }
                    return None;
                }
            } else {
                *plain_extras_one = Some((key, Arc::clone(ex)));
                return None;
            }
            let canon = plain_extras.entry(key).or_insert_with(|| Arc::clone(ex));
            let canon_ptr = Arc::as_ptr(canon) as usize;
            if ex_ptr != canon_ptr {
                changed = true;
                Some(Arc::clone(canon))
            } else {
                None
            }
        });
        let flags = pat.node_flags();
        if self.dedup_cell_mut(pat.node_arc_mut(), flags) {
            changed = true;
        }
        changed
    }

    fn dedup_cell_mut(&mut self, cell: &mut Arc<NodeCell>, flags: u8) -> bool {
        let id = Arc::as_ptr(cell) as usize;
        if let Some(node_cell) = Arc::get_mut(cell) {
            // Fast combined path for pure `Node::Sequence`: interns both the
            // layer slice and the enclosing `NodeCell` in one `self.layers`
            // lookup and one `Arc::clone`, bypassing `self.nodes` completely.
            if (flags & 0x0f) == 0 && matches!(node_cell.kind, Node::Sequence(_)) {
                let (can_intern_seq, hash_opt) = {
                    let Node::Sequence(layers) = &mut node_cell.kind else {
                        unreachable!()
                    };
                    if let Some(slice) = Arc::get_mut(layers) {
                        for layer in slice.iter_mut() {
                            layer.dedup_in_mut(self);
                        }
                    } else {
                        let next_layers = self.dedup_layers(layers);
                        if !Arc::ptr_eq(layers, &next_layers) {
                            *layers = next_layers;
                        }
                    }
                    if self.seq_count >= 2 && layers.iter().all(|l| l.can_intern() && l.can_alias())
                    {
                        (true, hash_layer_slice(layers))
                    } else {
                        (false, 0)
                    }
                };
                let mut replaced = false;
                if can_intern_seq {
                    let Node::Sequence(layers_ro) = &cell.kind else {
                        unreachable!()
                    };
                    let (entry, existed) = self.layers.intern_mut(
                        hash_opt,
                        |other| {
                            let ol = other.layers();
                            Arc::ptr_eq(ol, layers_ro) || same_layer_slice(ol, layers_ro)
                        },
                        || LayerCanon::Cell(Arc::clone(cell)),
                    );
                    if existed {
                        match entry {
                            LayerCanon::Cell(canon_cell) => {
                                *cell = Arc::clone(canon_cell);
                                replaced = true;
                            }
                            LayerCanon::Layers(canon_layers) => {
                                let canon_layers = Arc::clone(canon_layers);
                                if let Some(nc) = Arc::get_mut(cell)
                                    && let Node::Sequence(l) = &mut nc.kind
                                {
                                    *l = canon_layers;
                                }
                                *entry = LayerCanon::Cell(Arc::clone(cell));
                            }
                        }
                    }
                }
                if !self.detached_ids.is_empty() && self.detached_ids.contains(id) {
                    self.visited.insert(id, Arc::clone(cell));
                }
                return replaced;
            }
            let children_can_alias = self.dedup_node_in_place(&mut node_cell.kind);
            let mut replaced = false;
            if children_can_alias
                && self.node_kind_counts[node_intern_tag(&cell.kind)] >= 2
                && can_intern_node(&cell.kind, flags)
            {
                let h = hash_node_kind(&cell.kind);
                let (canon, existed) = self.nodes.intern(
                    h,
                    |other| same_node_kind(&other.kind, &cell.kind),
                    || Arc::clone(cell),
                );
                if existed {
                    *cell = Arc::clone(canon);
                    replaced = true;
                }
            }
            if !self.detached_ids.is_empty() && self.detached_ids.contains(id) {
                self.visited.insert(id, Arc::clone(cell));
            }
            return replaced;
        }
        if !self.visited.is_empty()
            && let Some(existing) = self.visited.get(id)
        {
            if !Arc::ptr_eq(cell, existing) {
                *cell = Arc::clone(existing);
                return true;
            }
            return false;
        }
        let next = self.dedup_cell(cell, flags);
        if !Arc::ptr_eq(cell, &next) {
            *cell = next;
            true
        } else {
            false
        }
    }

    /// Deduplicate children of `kind` in place and return whether every child
    /// `Arc<NodeCell>` compared by `Arc::ptr_eq` in [`same_node_kind`] has
    /// `Arc::strong_count > 1` (i.e. can possibly alias another node's child).
    /// When `false`, the parent node can never match any existing or future
    /// entry in `self.nodes`, so hashing and interning it is skipped cleanly.
    fn dedup_node_in_place(&mut self, kind: &mut Node) -> bool {
        match kind {
            Node::Sequence(layers) => {
                self.dedup_layers_mut(layers);
                Arc::strong_count(layers) > 1
            }
            Node::Pure(val) => {
                self.dedup_value_mut(val);
                true
            }
            Node::PureAt(boxed) => {
                self.dedup_value_mut(&mut boxed.0);
                true
            }
            Node::Stack(pats) | Node::SlowCat(pats) | Node::SlowCatPrime(pats) => {
                let mut all_alias = true;
                for pat in pats.iter_mut() {
                    self.dedup_pattern(pat);
                    all_alias &= Arc::strong_count(pat.node_arc()) > 1;
                }
                all_alias
            }
            Node::StackPure(cells) | Node::SlowCatPure(cells) | Node::SlowCatPrimePure(cells) => {
                let mut all_alias = true;
                for cell in cells.iter_mut() {
                    self.dedup_cell_mut(cell, 0);
                    all_alias &= Arc::strong_count(cell) > 1;
                }
                all_alias
            }
            Node::FastSmall(inner, f, ..)
            | Node::ShiftSmall(inner, f, ..)
            | Node::ControlMap(inner, f, ..)
            | Node::SplitQueries(inner, f) => {
                self.dedup_cell_mut(inner, *f);
                Arc::strong_count(inner) > 1
            }
            Node::Join(inner, f, ..) => {
                self.dedup_cell_mut(inner, *f);
                false
            }
            Node::SplitQueriesBoxed(pat)
            | Node::Rev(pat)
            | Node::StepJoin(pat)
            | Node::PolyJoin(pat)
            | Node::PurePattern(pat)
            | Node::SortHapsByPart(pat)
            | Node::Collect(pat)
            | Node::CycleOnsetIndex(pat)
            | Node::CycleOnsetCount(pat) => {
                self.dedup_pattern(pat);
                false
            }
            Node::Fast(node) => {
                self.dedup_pattern(&mut node.0);
                Arc::strong_count(node.0.node_arc()) > 1
            }
            Node::Shift(node) => {
                self.dedup_pattern(&mut node.0);
                Arc::strong_count(node.0.node_arc()) > 1
            }
            Node::Degrade(boxed) => {
                self.dedup_pattern(&mut boxed.0);
                Arc::strong_count(boxed.0.node_arc()) > 1
            }
            Node::AppWave(app) => {
                self.dedup_pattern(&mut app.inner);
                Arc::strong_count(app.inner.node_arc()) > 1
            }
            Node::Constants(node) => {
                self.dedup_pattern(&mut node.0);
                for m in node.1.iter_mut() {
                    self.dedup_value_mut(&mut m.value);
                }
                Arc::strong_count(node.0.node_arc()) > 1
            }
            Node::Pick(pick) => {
                self.dedup_pattern(&mut pick.selector);
                match &mut pick.lookup {
                    CompactPickLookup::Dense(pats) => {
                        for p in pats.iter_mut() {
                            self.dedup_pattern(p);
                        }
                    }
                    CompactPickLookup::Array { entries, .. } => {
                        for (_, p) in entries.iter_mut() {
                            self.dedup_pattern(p);
                        }
                    }
                    CompactPickLookup::Object { entries, .. } => {
                        for (_, p) in entries.iter_mut() {
                            self.dedup_pattern(p);
                        }
                    }
                }
                false
            }
            Node::AppLeft(app) => {
                self.dedup_pattern(&mut app.0);
                self.dedup_pattern(&mut app.1);
                app.2 = ValueReading::default();
                false
            }
            Node::AppRight(app) | Node::AppBoth(app) => {
                self.dedup_pattern(&mut app.0);
                self.dedup_pattern(&mut app.1);
                false
            }
            Node::Cpm(boxed) => {
                self.dedup_pattern(&mut boxed.0);
                false
            }
            Node::UiVisual(node) => {
                self.dedup_pattern(&mut node.inner);
                false
            }
            Node::ControlMapBoxed(boxed) => {
                self.dedup_pattern(&mut boxed.0);
                false
            }
            Node::JoinBoxed(boxed) => {
                self.dedup_pattern(&mut boxed.0);
                false
            }
            Node::AddContext(boxed) => {
                self.dedup_pattern(&mut boxed.0);
                false
            }
            Node::FilterValues(boxed) => {
                self.dedup_pattern(&mut boxed.0);
                false
            }
            Node::FilterHaps(boxed) => {
                self.dedup_pattern(&mut boxed.0);
                false
            }
            Node::MapHapsOwned(boxed) => {
                self.dedup_pattern(&mut boxed.0);
                false
            }
            Node::MapHapsNative(boxed) => {
                self.dedup_pattern(&mut boxed.0);
                false
            }
            Node::ExpandHapsNative(boxed) => {
                self.dedup_pattern(&mut boxed.0);
                false
            }
            Node::FmapNative(boxed) => {
                self.dedup_pattern(&mut boxed.0);
                false
            }
            Node::WithRandSeed(boxed) => {
                self.dedup_pattern(&mut boxed.0);
                false
            }
            Node::FastGap(boxed) => {
                self.dedup_pattern(&mut boxed.0);
                false
            }
            Node::RepeatCycles(boxed) => {
                self.dedup_pattern(&mut boxed.0);
                false
            }
            Node::LoopAt(boxed) => {
                self.dedup_pattern(&mut boxed.0);
                false
            }
            Node::WithQueryTime(boxed) => {
                self.dedup_pattern(&mut boxed.0);
                false
            }
            Node::WithHapTime(boxed) => {
                self.dedup_pattern(&mut boxed.0);
                false
            }
            Node::WithQuerySpan(boxed) => {
                self.dedup_pattern(&mut boxed.0);
                false
            }
            Node::WithHapSpan(boxed) => {
                self.dedup_pattern(&mut boxed.0);
                false
            }
            Node::MapHapsWithState(boxed) => {
                self.dedup_pattern(&mut boxed.0);
                false
            }
            Node::InnerJoin(boxed) => {
                self.dedup_pattern(&mut boxed.0);
                false
            }
            Node::ChooseCycles(boxed) => {
                for p in boxed.0.iter_mut() {
                    self.dedup_pattern(p);
                }
                false
            }
            Node::PatternOfPure(boxed) => {
                self.dedup_pattern(&mut boxed.0);
                boxed.2 = ConstructionMemo::default();
                false
            }
            Node::PatternOfPureOperand(pair, _) => {
                self.dedup_pattern(&mut pair.0);
                self.dedup_pattern(&mut pair.1);
                pair.2 = ConstructionMemo::default();
                false
            }
            Node::PatternOfHap(bind) => {
                self.dedup_pattern(&mut bind.0);
                for p in &mut bind.1 {
                    self.dedup_pattern(p);
                }
                false
            }
            Node::WChoose(choice) => {
                self.dedup_pattern(&mut choice.chooser);
                self.dedup_pattern(&mut choice.total);
                for p in &mut choice.cumulative {
                    self.dedup_pattern(p);
                }
                for p in &mut choice.values {
                    self.dedup_pattern(p);
                }
                false
            }
            Node::Arp(pair) => {
                self.dedup_pattern(&mut pair.0);
                self.dedup_pattern(&mut pair.1);
                false
            }
            Node::PickPatternified { patterns, .. } => {
                self.dedup_pattern(&mut patterns.0);
                self.dedup_pattern(&mut patterns.1);
                false
            }
            Node::XenPatterns(boxed) => {
                self.dedup_pattern(&mut boxed.0);
                for p in &mut boxed.1 {
                    self.dedup_pattern(p);
                }
                false
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
            | Node::Timeline(..) => false,
            #[cfg(non_send_node_mutation)]
            Node::NonSendMutation(_) => false,
        }
    }

    fn dedup_pat_child(&mut self, pat: &Pattern) -> Option<Pattern> {
        let next_node = self.dedup_cell(pat.node_arc(), pat.node_flags());
        let node_changed = !Arc::ptr_eq(pat.node_arc(), &next_node);
        let extras_changed = pat.plain_runtime_settings_key().is_some_and(|key| {
            if let Some((first_key, first_ex)) = &self.plain_extras_one
                && *first_key == key
            {
                return pat.extras_ptr() != Arc::as_ptr(first_ex) as usize;
            }
            self.plain_extras
                .get(&key)
                .is_some_and(|canon| pat.extras_ptr() != Arc::as_ptr(canon) as usize)
        });
        if !node_changed && !extras_changed {
            // Still register this plain_extras as canonical if first seen.
            if let Some(key) = pat.plain_runtime_settings_key() {
                let already = self
                    .plain_extras_one
                    .as_ref()
                    .is_some_and(|(k, _)| *k == key)
                    || self.plain_extras.contains_key(&key);
                if !already {
                    let mut tmp = pat.clone();
                    let plain_extras_one = &mut self.plain_extras_one;
                    let plain_extras = &mut self.plain_extras;
                    tmp.dedup_plain_extras(&mut |k, ex| {
                        if plain_extras_one.is_none() {
                            *plain_extras_one = Some((k, Arc::clone(ex)));
                        } else {
                            plain_extras.entry(k).or_insert_with(|| Arc::clone(ex));
                        }
                        None
                    });
                }
            }
            return None;
        }
        let mut updated = pat.clone();
        if node_changed {
            updated.set_node_arc(next_node);
        }
        if extras_changed {
            let plain_extras_one = &mut self.plain_extras_one;
            let plain_extras = &mut self.plain_extras;
            updated.dedup_plain_extras(&mut |k, ex| {
                if let Some((first_key, first_ex)) = plain_extras_one
                    && *first_key == k
                {
                    return (!Arc::ptr_eq(ex, first_ex)).then(|| Arc::clone(first_ex));
                }
                let canon = plain_extras.entry(k).or_insert_with(|| Arc::clone(ex));
                (!Arc::ptr_eq(ex, canon)).then(|| Arc::clone(canon))
            });
        }
        Some(updated)
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
        if let Some(existing) = self.visited.get(id) {
            return Arc::clone(existing);
        }
        if (flags & 0x0f) == 0
            && let Node::Sequence(layers) = &cell.kind
        {
            let next = self.dedup_layers(layers);
            let canon = if self.seq_count >= 2 && next.iter().all(Layer::can_intern) {
                let h = hash_layer_slice(&next);
                let (entry, existed) = self.layers.intern_mut(
                    h,
                    |other| {
                        let ol = other.layers();
                        Arc::ptr_eq(ol, &next) || same_layer_slice(ol, &next)
                    },
                    || {
                        let c = if Arc::ptr_eq(layers, &next) {
                            Arc::clone(cell)
                        } else {
                            Arc::new(NodeCell {
                                kind: Node::Sequence(Arc::clone(&next)),
                            })
                        };
                        LayerCanon::Cell(c)
                    },
                );
                if existed {
                    match entry {
                        LayerCanon::Cell(canon_cell) => Arc::clone(canon_cell),
                        LayerCanon::Layers(_) => {
                            let c = if Arc::ptr_eq(layers, &next) {
                                Arc::clone(cell)
                            } else {
                                Arc::new(NodeCell {
                                    kind: Node::Sequence(Arc::clone(&next)),
                                })
                            };
                            *entry = LayerCanon::Cell(Arc::clone(&c));
                            c
                        }
                    }
                } else {
                    match entry {
                        LayerCanon::Cell(canon_cell) => Arc::clone(canon_cell),
                        LayerCanon::Layers(_) => unreachable!("just inserted LayerCanon::Cell"),
                    }
                }
            } else if Arc::ptr_eq(layers, &next) {
                Arc::clone(cell)
            } else {
                Arc::new(NodeCell {
                    kind: Node::Sequence(next),
                })
            };
            self.visited.insert(id, Arc::clone(&canon));
            return canon;
        }
        let rebuilt: Option<Node> = match &cell.kind {
            Node::Sequence(layers) => {
                let next = self.dedup_layers(layers);
                (!Arc::ptr_eq(layers, &next)).then_some(Node::Sequence(next))
            }
            Node::Pure(val) => self.dedup_value(val).map(Node::Pure),
            Node::PureAt(boxed) => self
                .dedup_value(&boxed.0)
                .map(|v| Node::PureAt(Box::new((v, boxed.1.clone())))),
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
                    cache: std::array::from_fn(|_| std::sync::OnceLock::new()),
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
        let canon = match rebuilt {
            Some(kind) => {
                if self.node_kind_counts[node_intern_tag(&kind)] >= 2
                    && can_intern_node(&kind, flags)
                    && node_children_can_alias(&kind)
                {
                    let h = hash_node_kind(&kind);
                    let (canon, _) = self.nodes.intern_with(
                        h,
                        kind,
                        |other, cand| same_node_kind(&other.kind, cand),
                        |kind| Arc::new(NodeCell { kind }),
                    );
                    Arc::clone(canon)
                } else {
                    Arc::new(NodeCell { kind })
                }
            }
            None => {
                if self.node_kind_counts[node_intern_tag(&cell.kind)] >= 2
                    && can_intern_node(&cell.kind, flags)
                    && node_children_can_alias(&cell.kind)
                {
                    let h = hash_node_kind(&cell.kind);
                    let (canon, _) = self.nodes.intern(
                        h,
                        |other| Arc::ptr_eq(other, cell) || same_node_kind(&other.kind, &cell.kind),
                        || Arc::clone(cell),
                    );
                    Arc::clone(canon)
                } else {
                    Arc::clone(cell)
                }
            }
        };
        self.visited.insert(id, Arc::clone(&canon));
        canon
    }
}

impl LayerDeduper for Deduper {
    fn dedup_values(&mut self, values: &Arc<[Value]>, may_have_objects: bool) -> Arc<[Value]> {
        Self::dedup_values(self, values, may_have_objects)
    }

    fn dedup_values_mut(&mut self, values: &mut Arc<[Value]>, may_have_objects: bool) {
        Self::dedup_values_mut(self, values, may_have_objects);
    }

    fn dedup_dress(&mut self, dress: &Arc<Dress>) -> Arc<Dress> {
        Self::dedup_dress(self, dress)
    }

    fn dedup_dress_mut(&mut self, dress: &mut Arc<Dress>) {
        Self::dedup_dress_mut(self, dress);
    }

    fn dedup_compact_grid(&mut self, grid: &CompactGrid) -> CompactGrid {
        Self::dedup_compact_grid(self, grid)
    }

    fn dedup_compact_grid_mut(&mut self, grid: &mut CompactGrid) {
        Self::dedup_compact_grid_mut(self, grid);
    }

    fn dedup_grid(&mut self, grid: &Arc<Grid>) -> Arc<Grid> {
        Self::dedup_grid(self, grid)
    }

    fn dedup_grid_mut(&mut self, grid: &mut Arc<Grid>) {
        Self::dedup_grid_mut(self, grid);
    }
}

// --- Arc<[Value]> ---

#[inline]
fn hash_interned_value(v: &Value, state: &mut impl Hasher) {
    match v {
        Value::F64(n) => {
            state.write_u64(n.to_bits() ^ 0x3003_0000_0000_0000);
        }
        Value::Str(s) => {
            let bytes = s.as_bytes();
            if bytes.len() <= 8 {
                state.write_u64(
                    crate::value::pack_le_u64(bytes) ^ ((bytes.len() as u64) << 56) ^ 0x4400_0000,
                );
            } else {
                state.write_u8(4);
                state.write(bytes);
            }
        }
        Value::Object(map) => {
            if let Some(entries) = map.as_builtin() {
                state.write_u64((Arc::as_ptr(entries) as *const () as u64) ^ 0x5500_0000_0000_0000);
            } else {
                hash_data(v, state);
            }
        }
        Value::Bool(b) => state.write_u8(if *b { 0x21 } else { 0x20 }),
        Value::Null => state.write_u8(1),
        Value::Undefined => state.write_u8(0),
        _ => hash_data(v, state),
    }
}

#[inline]
fn same_interned_value(a: &Value, b: &Value) -> bool {
    if let (Value::Object(am), Value::Object(bm)) = (a, b)
        && let (Some(ae), Some(be)) = (am.as_builtin(), bm.as_builtin())
    {
        return Arc::ptr_eq(ae, be);
    }
    a.same_data(b)
}

#[inline]
fn hash_value_slice(values: &[Value]) -> u64 {
    hash_with(|state| {
        values.len().hash(state);
        for v in values {
            hash_interned_value(v, state);
        }
    })
}

#[inline]
fn same_value_slice(a: &[Value], b: &[Value]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|(x, y)| same_interned_value(x, y))
}

// --- Arc<dyn BuiltinEntries> ---

#[inline]
fn hash_u16_slice(keys: &[u16], state: &mut WordHasher) {
    state.write_usize(keys.len());
    let (chunks, rem) = keys.as_chunks::<4>();
    for &[a, b, c, d] in chunks {
        state.write_u64(
            u64::from(a) | (u64::from(b) << 16) | (u64::from(c) << 32) | (u64::from(d) << 48),
        );
    }
    if !rem.is_empty() {
        let mut word = 0u64;
        for (i, &k) in rem.iter().enumerate() {
            word |= u64::from(k) << (i * 16);
        }
        state.write_u64(word);
    }
}

#[inline]
fn hash_builtin_value(v: &Value, state: &mut WordHasher) {
    match v {
        Value::F64(n) => state.write_u64(n.to_bits() ^ 0x3003_0000_0000_0000),
        Value::Str(s) => {
            let bytes = s.as_bytes();
            if bytes.len() <= 8 {
                state.write_u64(
                    crate::value::pack_le_u64(bytes) ^ ((bytes.len() as u64) << 56) ^ 0x4400_0000,
                );
            } else {
                state.write_u8(4);
                state.write(bytes);
            }
        }
        Value::Bool(b) => state.write_u8(if *b { 0x21 } else { 0x20 }),
        Value::Null => state.write_u8(1),
        Value::Undefined => state.write_u8(0),
        _ => hash_data(v, state),
    }
}

fn hash_builtin_entries(entries: &dyn BuiltinEntries) -> u64 {
    hash_with(|state| match entries.as_slice() {
        Slice::Builtin(keys, values) => {
            hash_u16_slice(keys, state);
            for v in values {
                hash_builtin_value(v, state);
            }
        }
        Slice::Pooled(keys, indices, pool) => {
            hash_u16_slice(keys, state);
            for &idx in indices {
                hash_builtin_value(&pool[usize::from(idx)], state);
            }
        }
        Slice::Plain(pairs) => {
            state.write_usize(pairs.len());
            for (_, v) in pairs {
                hash_builtin_value(v, state);
            }
        }
    })
}

fn same_builtin_entries(a: &dyn BuiltinEntries, b: &dyn BuiltinEntries) -> bool {
    if std::ptr::eq(
        a as *const dyn BuiltinEntries as *const (),
        b as *const dyn BuiltinEntries as *const (),
    ) {
        return true;
    }
    match (a.as_slice(), b.as_slice()) {
        (Slice::Builtin(ak, av), Slice::Builtin(bk, bv)) => {
            ak == bk && av.iter().zip(bv.iter()).all(|(x, y)| x.same_data(y))
        }
        (Slice::Pooled(ak, ai, ap), Slice::Pooled(bk, bi, bp)) => {
            ak == bk
                && ai
                    .iter()
                    .zip(bi.iter())
                    .all(|(&x, &y)| ap[usize::from(x)].same_data(&bp[usize::from(y)]))
        }
        _ => {
            let ak = a.keys();
            let bk = b.keys();
            if ak != bk {
                return false;
            }
            let a_slice = a.as_slice();
            let b_slice = b.as_slice();
            (0..ak.len()).all(|i| a_slice.at(i).1.same_data(b_slice.at(i).1))
        }
    }
}

// --- Arc<Keys> ---

fn hash_keys(k: &Keys) -> u64 {
    hash_with(|state| {
        k.scale.as_deref().hash(state);
        k.edo_size.map(f64::to_bits).hash(state);
        k.tags.as_deref().hash(state);
        k.log_line.as_deref().hash(state);
        k.live_controls.hash(state);
        k.slider_binding.hash(state);
        k.ui_visuals.hash(state);
    })
}

fn same_keys(a: &Keys, b: &Keys) -> bool {
    a.scale.as_deref() == b.scale.as_deref()
        && a.edo_size.map(f64::to_bits) == b.edo_size.map(f64::to_bits)
        && a.tags.as_deref() == b.tags.as_deref()
        && a.log_line.as_deref() == b.log_line.as_deref()
        && a.live_controls == b.live_controls
        && a.slider_binding == b.slider_binding
        && a.ui_visuals == b.ui_visuals
}

// --- CompactGrid ---

fn hash_compact_grid(grid: &CompactGrid) -> u64 {
    hash_with(|state| {
        let (ticks, base, steps, packed) = grid.parts();
        let header = (u64::from(ticks.get()) << 32) | u64::from(base);
        state.write_u64(header);
        let lens = ((steps.len() as u64) << 32) | (packed.map_or(0, |p| p.len()) as u64);
        state.write_u64(lens);
        if let Some(first) = steps.first() {
            first.hash_intern(state);
            if steps.len() > 1 {
                steps[steps.len() - 1].hash_intern(state);
            }
        }
    })
}

fn same_compact_grid(a: &CompactGrid, b: &CompactGrid) -> bool {
    if a.ptr_eq(b) {
        return true;
    }
    let (at, ab, asteps, apacked) = a.parts();
    let (bt, bb, bsteps, bpacked) = b.parts();
    at == bt && ab == bb && asteps == bsteps && apacked == bpacked
}

// --- Arc<[Layer]> ---

#[inline]
fn hash_layer_slice(layers: &[Layer]) -> u64 {
    hash_with(|state| {
        layers.len().hash(state);
        for layer in layers {
            layer.hash_intern(state);
        }
    })
}

#[inline]
fn same_layer_slice(a: &[Layer], b: &[Layer]) -> bool {
    a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x.eq_intern(y))
}

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

/// Whether every child `Arc<NodeCell>` / `Arc<[Layer]>` compared by `Arc::ptr_eq`
/// in [`same_node_kind`] has `Arc::strong_count > 1` (i.e. can possibly alias
/// another `NodeCell`'s child in `self.nodes`).
fn node_children_can_alias(kind: &Node) -> bool {
    match kind {
        Node::Sequence(layers) => Arc::strong_count(layers) > 1,
        Node::FastSmall(inner, ..)
        | Node::ShiftSmall(inner, ..)
        | Node::ControlMap(inner, ..)
        | Node::SplitQueries(inner, ..) => Arc::strong_count(inner) > 1,
        Node::Fast(n) => Arc::strong_count(n.0.node_arc()) > 1,
        Node::Shift(n) => Arc::strong_count(n.0.node_arc()) > 1,
        Node::Degrade(b) => Arc::strong_count(b.0.node_arc()) > 1,
        Node::AppWave(app) => Arc::strong_count(app.inner.node_arc()) > 1,
        Node::Constants(c) => Arc::strong_count(c.0.node_arc()) > 1,
        Node::Stack(pats) | Node::SlowCat(pats) | Node::SlowCatPrime(pats) => {
            pats.iter().all(|p| Arc::strong_count(p.node_arc()) > 1)
        }
        Node::StackPure(cells) | Node::SlowCatPure(cells) | Node::SlowCatPrimePure(cells) => {
            cells.iter().all(|c| Arc::strong_count(c) > 1)
        }
        _ => true,
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
        Node::PureAt(b) => b.0.is_data(0),
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

fn hash_node_kind(kind: &Node) -> u64 {
    hash_with(|state| match kind {
        Node::Sequence(layers) => {
            1u8.hash(state);
            (Arc::as_ptr(layers) as *const Layer as usize).hash(state);
            layers.len().hash(state);
        }
        Node::Pure(v) => {
            2u8.hash(state);
            hash_data(v, state);
        }
        Node::PureAt(b) => {
            18u8.hash(state);
            hash_data(&b.0, state);
            b.1.len().hash(state);
            for range in b.1.iter() {
                range.hash(state);
            }
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
                item.steps.hash(state);
                item.math_inside.hash(state);
                item.wave.hash_intern(state);
            }
        }
        _ => {}
    })
}

fn same_node_kind(left: &Node, right: &Node) -> bool {
    match (left, right) {
        (Node::Sequence(a), Node::Sequence(b)) => Arc::ptr_eq(a, b),
        (Node::Pure(a), Node::Pure(b)) => a.same_data(b),
        (Node::PureAt(a), Node::PureAt(b)) => a.1 == b.1 && a.0.same_data(&b.0),
        (Node::FastSmall(ac, af, an, ad, as_), Node::FastSmall(bc, bf, bn, bd, bs)) => {
            Arc::ptr_eq(ac, bc) && af == bf && an == bn && ad == bd && as_ == bs
        }
        (Node::ShiftSmall(ac, af, an, ad, ae), Node::ShiftSmall(bc, bf, bn, bd, be)) => {
            Arc::ptr_eq(ac, bc) && af == bf && an == bn && ad == bd && ae == be
        }
        (Node::ControlMap(ac, af, ai, au), Node::ControlMap(bc, bf, bi, bu)) => {
            Arc::ptr_eq(ac, bc) && af == bf && ai == bi && au == bu
        }
        (Node::SplitQueries(ac, af), Node::SplitQueries(bc, bf)) => Arc::ptr_eq(ac, bc) && af == bf,
        (Node::Fast(a), Node::Fast(b)) => same_pat_shallow(&a.0, &b.0) && a.1 == b.1 && a.2 == b.2,
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
                        && x.steps == y.steps
                        && x.math_inside == y.math_inside
                        && x.wave.same(&y.wave)
                })
        }
        _ => false,
    }
}
