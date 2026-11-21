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

use std::collections::hash_map::Entry;
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
    remap_extras: impl FnMut(&mut dyn FnMut(&mut crate::PatternNodeRef)),
) {
    dedup_score_with_reused(root, &[], remap_extras);
}

/// Stack-only check for tiny graphs (`<= 12` nodes) with nothing worthwhile to
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

#[derive(Default)]
struct ScanScratch {
    seen_nodes: FastSet<usize>,
    seen_layers: FastSet<usize>,
    seen_ptrs: FastSet<usize>,
    unpooled: Vec<Arc<dyn BuiltinEntries>>,
    unique_set: FastMap<InternedBuiltin, ()>,
    unique_maps: Vec<Arc<dyn BuiltinEntries>>,
    duplicates: Vec<(usize, Arc<dyn BuiltinEntries>)>,
    counts: Vec<u32>,
    map_val_ids: Vec<u32>,
    map_spans: Vec<(u32, u16)>,
    poolable_idx: Vec<usize>,
    remap: Vec<u16>,
    detached: Vec<usize>,
}

impl ScanScratch {
    fn clear(&mut self) {
        self.seen_nodes.clear();
        self.seen_layers.clear();
        self.seen_ptrs.clear();
        self.unpooled.clear();
        self.unique_set.clear();
        self.unique_maps.clear();
        self.duplicates.clear();
        self.counts.clear();
        self.map_val_ids.clear();
        self.map_spans.clear();
        self.poolable_idx.clear();
        self.remap.clear();
        self.detached.clear();
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
    deduper.clear();
    scan.clear();
    if !reused.is_empty() {
        deduper.visited.reserve(reused.len() * 2);
        for node_ref in reused {
            deduper
                .visited
                .insert(node_ref.identity(), Arc::clone(&node_ref.0));
        }
    }
    let builtins_pre_deduped = collect_and_pool_builtins(
        root.node_arc(),
        &deduper.visited,
        &mut scan,
        &mut deduper.pooled_builtins,
    );
    deduper.builtins_pre_deduped = builtins_pre_deduped;
    // Temporarily detach extra handles (such as memo parts) that point to
    // newly-built nodes reachable from `root` so their refcount drops to 1 and
    // `Arc::get_mut` can deduplicate them in place without reallocating.
    let silence_arc = Arc::clone(crate::silence().node_arc());
    remap_extras(&mut |node_ref: &mut crate::PatternNodeRef| {
        let id = node_ref.identity();
        if scan.seen_nodes.contains(&id) && !deduper.visited.contains_key(&id) {
            scan.detached.push(id);
            deduper.detached_ids.insert(id);
            node_ref.0 = Arc::clone(&silence_arc);
        } else {
            scan.detached.push(0);
        }
    });
    let cap = scan.seen_nodes.len();
    deduper.values.reserve(cap / 2);
    deduper.dresses.reserve(cap / 2);
    deduper.compact_grids.reserve(cap / 2);
    deduper.layers.reserve(cap / 2);
    deduper.nodes.reserve(cap);
    deduper.dedup_pattern(root);
    let mut idx = 0usize;
    let mut remap = |node_ref: &mut crate::PatternNodeRef| {
        let orig = scan.detached.get(idx).copied().unwrap_or(0);
        idx += 1;
        let lookup = if orig != 0 { orig } else { node_ref.identity() };
        if let Some(canon) = deduper.visited.get(&lookup) {
            node_ref.0 = Arc::clone(canon);
        } else {
            debug_assert_eq!(orig, 0, "detached reachable node missing from visited");
        }
    };
    remap_extras(&mut remap);
    deduper.clear();
    scan.clear();
    if deduper.visited.capacity() <= 4096 && deduper.nodes.capacity() <= 4096 {
        let _ = SPARE_DEDUPER.try_with(|cell| *cell.borrow_mut() = Some(deduper));
    }
    if scan.seen_nodes.capacity() <= 4096 && scan.unpooled.capacity() <= 4096 {
        let _ = SPARE_SCAN.try_with(|cell| *cell.borrow_mut() = Some(scan));
    }
}

/// Fast read-only pre-scan collecting surviving unpooled built-in maps so value
/// pooling runs in a single pass with zero duplicate node/dress/layer rebuilds.
#[allow(clippy::mutable_key_type)]
fn collect_and_pool_builtins(
    root: &Arc<NodeCell>,
    skip_nodes: &FastMap<usize, Arc<NodeCell>>,
    scan: &mut ScanScratch,
    out: &mut FastMap<usize, Arc<dyn BuiltinEntries>>,
) -> bool {
    let ScanScratch {
        seen_nodes,
        seen_layers,
        seen_ptrs,
        unpooled,
        unique_set,
        unique_maps,
        duplicates,
        counts,
        map_val_ids,
        map_spans,
        poolable_idx,
        remap,
        ..
    } = scan;
    let mut record = |entries: &Arc<dyn BuiltinEntries>| {
        if let Slice::Builtin(keys, _) = entries.as_slice()
            && keys.len() >= 2
            && keys.len() <= MAX_BUILTIN_MAP_LEN
        {
            let ptr = Arc::as_ptr(entries) as *const () as usize;
            if seen_ptrs.insert(ptr) {
                unpooled.push(Arc::clone(entries));
            }
        }
    };
    struct SmallNodeStack<'a> {
        inline: [Option<&'a NodeCell>; 64],
        len: usize,
        overflow: Vec<&'a NodeCell>,
    }
    impl<'a> SmallNodeStack<'a> {
        #[inline]
        fn push(&mut self, cell: &'a NodeCell) {
            if self.overflow.is_empty() && self.len < self.inline.len() {
                self.inline[self.len] = Some(cell);
                self.len += 1;
            } else {
                self.overflow.push(cell);
            }
        }
        #[inline]
        fn pop(&mut self) -> Option<&'a NodeCell> {
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
        inline: [None; 64],
        len: 0,
        overflow: Vec::new(),
    };
    stack.push(root.as_ref());
    while let Some(cell) = stack.pop() {
        let id = std::ptr::from_ref(cell) as usize;
        if skip_nodes.contains_key(&id) || !seen_nodes.insert(id) {
            continue;
        }
        match &cell.kind {
            Node::Sequence(layers) => {
                let lid = Arc::as_ptr(layers) as *const Layer as usize;
                if seen_layers.insert(lid) {
                    for layer in layers.iter() {
                        layer.for_each_unpooled_builtin(&mut record);
                    }
                }
            }
            Node::Pure(val) => {
                if let Value::Object(map) = val
                    && let Some(entries) = map.as_builtin()
                {
                    record(entries);
                }
            }
            Node::PureAt(boxed) => {
                if let Value::Object(map) = &boxed.0
                    && let Some(entries) = map.as_builtin()
                {
                    record(entries);
                }
            }
            Node::Constants(node) => {
                stack.push(node.0.node_arc().as_ref());
                for m in node.1.iter() {
                    if let Value::Object(map) = &m.value
                        && let Some(entries) = map.as_builtin()
                    {
                        record(entries);
                    }
                }
            }
            Node::Stack(pats) | Node::SlowCat(pats) | Node::SlowCatPrime(pats) => {
                for p in pats.iter() {
                    stack.push(p.node_arc().as_ref());
                }
            }
            Node::StackPure(cells) | Node::SlowCatPure(cells) | Node::SlowCatPrimePure(cells) => {
                for c in cells.iter() {
                    stack.push(c.as_ref());
                }
            }
            Node::FastSmall(inner, ..)
            | Node::ShiftSmall(inner, ..)
            | Node::ControlMap(inner, ..)
            | Node::SplitQueries(inner, _)
            | Node::Join(inner, ..) => stack.push(inner.as_ref()),
            Node::SplitQueriesBoxed(p)
            | Node::Rev(p)
            | Node::StepJoin(p)
            | Node::PolyJoin(p)
            | Node::PurePattern(p)
            | Node::SortHapsByPart(p)
            | Node::Collect(p)
            | Node::CycleOnsetIndex(p)
            | Node::CycleOnsetCount(p) => stack.push(p.node_arc().as_ref()),
            Node::Fast(n) => stack.push(n.0.node_arc().as_ref()),
            Node::Shift(n) => stack.push(n.0.node_arc().as_ref()),
            Node::AppWave(app) => stack.push(app.inner.node_arc().as_ref()),
            Node::UiVisual(node) => stack.push(node.inner.node_arc().as_ref()),
            Node::Degrade(b) => stack.push(b.0.node_arc().as_ref()),
            Node::Cpm(b) => stack.push(b.0.node_arc().as_ref()),
            Node::AddContext(b) => stack.push(b.0.node_arc().as_ref()),
            Node::FilterValues(b) => stack.push(b.0.node_arc().as_ref()),
            Node::FilterHaps(b) => stack.push(b.0.node_arc().as_ref()),
            Node::MapHapsOwned(b) => stack.push(b.0.node_arc().as_ref()),
            Node::MapHapsNative(b) => stack.push(b.0.node_arc().as_ref()),
            Node::ExpandHapsNative(b) => stack.push(b.0.node_arc().as_ref()),
            Node::FmapNative(b) => stack.push(b.0.node_arc().as_ref()),
            Node::WithRandSeed(b) => stack.push(b.0.node_arc().as_ref()),
            Node::FastGap(b) => stack.push(b.0.node_arc().as_ref()),
            Node::RepeatCycles(b) => stack.push(b.0.node_arc().as_ref()),
            Node::LoopAt(b) => stack.push(b.0.node_arc().as_ref()),
            Node::ControlMapBoxed(b) => stack.push(b.0.node_arc().as_ref()),
            Node::JoinBoxed(b) => stack.push(b.0.node_arc().as_ref()),
            Node::WithQueryTime(b) | Node::WithHapTime(b) => stack.push(b.0.node_arc().as_ref()),
            Node::WithQuerySpan(b) | Node::WithHapSpan(b) => stack.push(b.0.node_arc().as_ref()),
            Node::MapHapsWithState(b) => stack.push(b.0.node_arc().as_ref()),
            Node::InnerJoin(b) => stack.push(b.0.node_arc().as_ref()),
            Node::PatternOfPure(b) => stack.push(b.0.node_arc().as_ref()),
            Node::PatternOfPureOperand(b, _) => {
                stack.push(b.0.node_arc().as_ref());
                stack.push(b.1.node_arc().as_ref());
            }
            Node::PatternOfHap(b) => {
                stack.push(b.0.node_arc().as_ref());
                for p in b.1.iter() {
                    stack.push(p.node_arc().as_ref());
                }
            }
            Node::AppLeft(a) => {
                stack.push(a.0.node_arc().as_ref());
                stack.push(a.1.node_arc().as_ref());
            }
            Node::AppRight(a) | Node::AppBoth(a) => {
                stack.push(a.0.node_arc().as_ref());
                stack.push(a.1.node_arc().as_ref());
            }
            Node::Pick(pick) => {
                stack.push(pick.selector.node_arc().as_ref());
                for p in pick.lookup.patterns() {
                    stack.push(p.node_arc().as_ref());
                }
            }
            Node::PickPatternified { patterns, .. } | Node::Arp(patterns) => {
                stack.push(patterns.0.node_arc().as_ref());
                stack.push(patterns.1.node_arc().as_ref());
            }
            Node::ChooseCycles(b) => {
                for p in b.0.iter() {
                    stack.push(p.node_arc().as_ref());
                }
            }
            Node::WChoose(c) => {
                stack.push(c.chooser.node_arc().as_ref());
                stack.push(c.total.node_arc().as_ref());
                for p in c.cumulative.iter().chain(c.values.iter()) {
                    stack.push(p.node_arc().as_ref());
                }
            }
            Node::XenPatterns(b) => {
                stack.push(b.0.node_arc().as_ref());
                for p in b.1.iter() {
                    stack.push(p.node_arc().as_ref());
                }
            }
            _ => {}
        }
    }
    if unpooled.len() < 4 {
        return false;
    }
    // Deduplicate identical maps once so frequency counts reflect unique
    // surviving maps and `Deduper` does not re-hash `len >= 2` built-in maps.
    unique_set.reserve(unpooled.len());
    unique_maps.reserve(unpooled.len());
    for map in unpooled.drain(..) {
        if !map.as_slice().all_values_data() {
            continue;
        }
        let ptr = Arc::as_ptr(&map) as *const () as usize;
        match unique_set.entry(InternedBuiltin(map)) {
            Entry::Occupied(entry) => {
                duplicates.push((ptr, Arc::clone(&entry.key().0)));
            }
            Entry::Vacant(entry) => {
                unique_maps.push(Arc::clone(&entry.key().0));
                entry.insert(());
            }
        }
    }
    out.reserve(duplicates.len() + unique_maps.len());
    if unique_maps.len() >= 4 {
        // Hash each distinct data value once into dense ids, then use flat
        // array lookups for frequency counting, poolability, and packing.
        let mut val_ids: FastMap<DataValueRef<'_>, u32> =
            FastMap::with_capacity_and_hasher(unique_maps.len() * 2, Default::default());
        counts.reserve(unique_maps.len() * 2);
        let mut distinct_vals: Vec<&Value> = Vec::with_capacity(unique_maps.len() * 2);
        map_val_ids.reserve(unique_maps.len() * 4);
        map_spans.reserve(unique_maps.len());
        for map in unique_maps.iter() {
            let start = map_val_ids.len() as u32;
            if let Slice::Builtin(_, values) = map.as_slice() {
                for val in values {
                    let id = match val_ids.entry(DataValueRef(val)) {
                        std::collections::hash_map::Entry::Occupied(entry) => {
                            let id = *entry.get();
                            counts[id as usize] += 1;
                            id
                        }
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            let id = counts.len() as u32;
                            counts.push(1);
                            distinct_vals.push(val);
                            entry.insert(id);
                            id
                        }
                    };
                    map_val_ids.push(id);
                }
                map_spans.push((start, values.len() as u16));
            } else {
                map_spans.push((start, 0));
            }
        }
        poolable_idx.reserve(unique_maps.len());
        for (idx, &(start, len)) in map_spans.iter().enumerate() {
            let len = usize::from(len);
            if len == 0 {
                continue;
            }
            let vids = &map_val_ids[start as usize..start as usize + len];
            let mut shared = 0usize;
            for &vid in vids {
                if counts[vid as usize] >= 2 {
                    shared += 1;
                }
            }
            let unique = len - shared;
            if 22 * shared > 2 * unique + 16 {
                poolable_idx.push(idx);
            }
        }
        if poolable_idx.len() >= 2 {
            remap.resize(counts.len(), u16::MAX);
            let mut pool_vec: Vec<Value> = Vec::with_capacity(counts.len());
            let mut old_bytes = 0usize;
            let mut new_maps_bytes = 0usize;
            let mut overflow = false;
            'assign: for &idx in poolable_idx.iter() {
                let (start, len) = map_spans[idx];
                let len = usize::from(len);
                old_bytes += 16 + ((26 * len + 7) & !7);
                new_maps_bytes += 32 + ((4 * len + 7) & !7);
                for &vid in &map_val_ids[start as usize..start as usize + len] {
                    if remap[vid as usize] == u16::MAX {
                        let Ok(pidx) = u16::try_from(pool_vec.len()) else {
                            overflow = true;
                            break 'assign;
                        };
                        if pidx == u16::MAX {
                            overflow = true;
                            break 'assign;
                        }
                        remap[vid as usize] = pidx;
                        pool_vec.push(distinct_vals[vid as usize].clone());
                    }
                }
            }
            let new_bytes = 16 + 24 * pool_vec.len() + new_maps_bytes;
            if !overflow && new_bytes + 64 < old_bytes {
                let pool: Arc<[Value]> = pool_vec.into();
                let mut vals_buf = [0u16; MAX_BUILTIN_MAP_LEN];
                for &idx in poolable_idx.iter() {
                    let map = &unique_maps[idx];
                    if let Slice::Builtin(keys, _) = map.as_slice() {
                        let (start, len) = map_spans[idx];
                        let len = usize::from(len);
                        for (i, &vid) in map_val_ids[start as usize..start as usize + len]
                            .iter()
                            .enumerate()
                        {
                            vals_buf[i] = remap[vid as usize];
                        }
                        let pooled =
                            make_pooled_builtin_arc(Arc::clone(&pool), keys, &vals_buf[..len]);
                        out.insert(Arc::as_ptr(map) as *const () as usize, pooled);
                    }
                }
            }
        }
    }
    for (dup_ptr, canon) in duplicates.drain(..) {
        let canon_ptr = Arc::as_ptr(&canon) as *const () as usize;
        if let Some(pooled) = out.get(&canon_ptr) {
            out.insert(dup_ptr, Arc::clone(pooled));
        } else {
            out.insert(dup_ptr, canon);
        }
    }
    true
}

#[derive(Default)]
struct Deduper {
    visited: FastMap<usize, Arc<NodeCell>>,
    detached_ids: FastSet<usize>,
    seen_values: FastMap<usize, Arc<[Value]>>,
    seen_dresses: FastMap<usize, Arc<Dress>>,
    seen_layers: FastMap<usize, Arc<[Layer]>>,
    plain_extras: FastMap<usize, Arc<PatternExtras>>,
    pooled_builtins: FastMap<usize, Arc<dyn BuiltinEntries>>,
    builtins_pre_deduped: bool,
    builtins: FastMap<InternedBuiltin, ()>,
    keys: FastMap<InternedKeys, ()>,
    values: FastMap<InternedValues, ()>,
    dresses: FastMap<InternedDress, ()>,
    compact_grids: FastMap<InternedCompactGrid, ()>,
    grids: FastMap<InternedGrid, ()>,
    layers: FastMap<InternedLayers, ()>,
    nodes: FastMap<InternedNode, ()>,
}

impl Deduper {
    fn clear(&mut self) {
        self.visited.clear();
        self.detached_ids.clear();
        self.seen_values.clear();
        self.seen_dresses.clear();
        self.seen_layers.clear();
        self.plain_extras.clear();
        self.pooled_builtins.clear();
        self.builtins_pre_deduped = false;
        self.builtins.clear();
        self.keys.clear();
        self.values.clear();
        self.dresses.clear();
        self.compact_grids.clear();
        self.grids.clear();
        self.layers.clear();
        self.nodes.clear();
    }

    fn intern_builtin(
        &mut self,
        entries: Arc<dyn BuiltinEntries>,
        known_data: bool,
    ) -> Arc<dyn BuiltinEntries> {
        let ptr = Arc::as_ptr(&entries) as *const () as usize;
        if let Some(pooled) = self.pooled_builtins.get(&ptr) {
            return Arc::clone(pooled);
        }
        if self.builtins_pre_deduped
            && matches!(entries.as_slice(), Slice::Builtin(k, _) if k.len() >= 2 && k.len() <= MAX_BUILTIN_MAP_LEN)
        {
            return entries;
        }
        if !known_data && !entries.as_slice().all_values_data() {
            return entries;
        }
        match self.builtins.entry(InternedBuiltin(entries)) {
            Entry::Occupied(entry) => Arc::clone(&entry.key().0),
            Entry::Vacant(entry) => {
                let out = Arc::clone(&entry.key().0);
                entry.insert(());
                out
            }
        }
    }

    fn dedup_value_mut(&mut self, value: &mut Value) {
        if let Value::Object(map) = value
            && map.as_builtin().is_some()
        {
            map.dedup_builtin(|entries, made| self.intern_builtin(entries, made));
        }
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

    fn dedup_values_mut(&mut self, values: &mut Arc<[Value]>) {
        if values.is_empty() {
            return;
        }
        if let Some(slice) = Arc::get_mut(values) {
            for val in slice.iter_mut() {
                self.dedup_value_mut(val);
            }
            match self.values.entry(InternedValues(Arc::clone(values))) {
                Entry::Occupied(entry) => *values = Arc::clone(&entry.key().0),
                Entry::Vacant(entry) => {
                    entry.insert(());
                }
            }
            return;
        }
        let next = self.dedup_values(values);
        if !Arc::ptr_eq(values, &next) {
            *values = next;
        }
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
        let canon = match self.values.entry(InternedValues(candidate)) {
            Entry::Occupied(entry) => Arc::clone(&entry.key().0),
            Entry::Vacant(entry) => {
                let out = Arc::clone(&entry.key().0);
                entry.insert(());
                out
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
            let mut dedup_builtin = |entries: Arc<dyn BuiltinEntries>, known_data: bool| {
                let ptr = Arc::as_ptr(&entries) as *const () as usize;
                if let Some(pooled) = pooled_builtins.get(&ptr) {
                    return Arc::clone(pooled);
                }
                if pre
                    && matches!(entries.as_slice(), Slice::Builtin(k, _) if k.len() >= 2 && k.len() <= MAX_BUILTIN_MAP_LEN)
                {
                    return entries;
                }
                if !known_data && !entries.as_slice().all_values_data() {
                    return entries;
                }
                match builtins.entry(InternedBuiltin(entries)) {
                    Entry::Occupied(entry) => Arc::clone(&entry.key().0),
                    Entry::Vacant(entry) => {
                        let out = Arc::clone(&entry.key().0);
                        entry.insert(());
                        out
                    }
                }
            };
            let mut dedup_keys = |k: Arc<Keys>| {
                if k.scale_definition.is_some() || k.pick_lookup.is_some() {
                    return k;
                }
                match keys.entry(InternedKeys(k)) {
                    Entry::Occupied(entry) => Arc::clone(&entry.key().0),
                    Entry::Vacant(entry) => {
                        let out = Arc::clone(&entry.key().0);
                        entry.insert(());
                        out
                    }
                }
            };
            inner.dedup_inner_mut(&mut dedup_builtin, &mut dedup_keys);
            if dress.can_intern() {
                match self.dresses.entry(InternedDress(Arc::clone(dress))) {
                    Entry::Occupied(entry) => *dress = Arc::clone(&entry.key().0),
                    Entry::Vacant(entry) => {
                        entry.insert(());
                    }
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
        if let Some(existing) = self.seen_dresses.get(&id) {
            return Arc::clone(existing);
        }
        let candidate = self.dedup_dress_inner(dress);
        let canon = if candidate.can_intern() {
            match self.dresses.entry(InternedDress(candidate)) {
                Entry::Occupied(entry) => Arc::clone(&entry.key().0),
                Entry::Vacant(entry) => {
                    let out = Arc::clone(&entry.key().0);
                    entry.insert(());
                    out
                }
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
            builtins_pre_deduped,
            ..
        } = self;
        let pre = *builtins_pre_deduped;
        let mut dedup_builtin = |entries: Arc<dyn BuiltinEntries>, known_data: bool| {
            let ptr = Arc::as_ptr(&entries) as *const () as usize;
            if let Some(pooled) = pooled_builtins.get(&ptr) {
                return Arc::clone(pooled);
            }
            if pre
                && matches!(entries.as_slice(), Slice::Builtin(k, _) if k.len() >= 2 && k.len() <= MAX_BUILTIN_MAP_LEN)
            {
                return entries;
            }
            if !known_data && !entries.as_slice().all_values_data() {
                return entries;
            }
            match builtins.entry(InternedBuiltin(entries)) {
                Entry::Occupied(entry) => Arc::clone(&entry.key().0),
                Entry::Vacant(entry) => {
                    let out = Arc::clone(&entry.key().0);
                    entry.insert(());
                    out
                }
            }
        };
        let mut dedup_keys = |k: Arc<Keys>| {
            if k.scale_definition.is_some() || k.pick_lookup.is_some() {
                return k;
            }
            match keys.entry(InternedKeys(k)) {
                Entry::Occupied(entry) => Arc::clone(&entry.key().0),
                Entry::Vacant(entry) => {
                    let out = Arc::clone(&entry.key().0);
                    entry.insert(());
                    out
                }
            }
        };
        match dress.dedup_inner(&mut dedup_builtin, &mut dedup_keys) {
            Some(updated) => Arc::new(updated),
            None => Arc::clone(dress),
        }
    }

    fn dedup_compact_grid(&mut self, grid: &CompactGrid) -> CompactGrid {
        match self.compact_grids.entry(InternedCompactGrid(grid.clone())) {
            Entry::Occupied(entry) => entry.key().0.clone(),
            Entry::Vacant(entry) => {
                let out = entry.key().0.clone();
                entry.insert(());
                out
            }
        }
    }

    fn dedup_compact_grid_mut(&mut self, grid: &mut CompactGrid) {
        match self.compact_grids.entry(InternedCompactGrid(grid.clone())) {
            Entry::Occupied(entry) => {
                if !grid.ptr_eq(&entry.key().0) {
                    *grid = entry.key().0.clone();
                }
            }
            Entry::Vacant(entry) => {
                entry.insert(());
            }
        }
    }

    fn dedup_grid(&mut self, grid: &Arc<Grid>) -> Arc<Grid> {
        match self.grids.entry(InternedGrid(Arc::clone(grid))) {
            Entry::Occupied(entry) => Arc::clone(&entry.key().0),
            Entry::Vacant(entry) => {
                let out = Arc::clone(&entry.key().0);
                entry.insert(());
                out
            }
        }
    }

    fn dedup_grid_mut(&mut self, grid: &mut Arc<Grid>) {
        match self.grids.entry(InternedGrid(Arc::clone(grid))) {
            Entry::Occupied(entry) => {
                if !Arc::ptr_eq(grid, &entry.key().0) {
                    *grid = Arc::clone(&entry.key().0);
                }
            }
            Entry::Vacant(entry) => {
                entry.insert(());
            }
        }
    }

    fn dedup_layers_mut(&mut self, layers: &mut Arc<[Layer]>) {
        if let Some(slice) = Arc::get_mut(layers) {
            for layer in slice.iter_mut() {
                layer.dedup_in_mut(self);
            }
            if layers.iter().all(Layer::can_intern) {
                match self.layers.entry(InternedLayers(Arc::clone(layers))) {
                    Entry::Occupied(entry) => *layers = Arc::clone(&entry.key().0),
                    Entry::Vacant(entry) => {
                        entry.insert(());
                    }
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
            match self.layers.entry(InternedLayers(candidate)) {
                Entry::Occupied(entry) => Arc::clone(&entry.key().0),
                Entry::Vacant(entry) => {
                    let out = Arc::clone(&entry.key().0);
                    entry.insert(());
                    out
                }
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
        let flags = pat.node_flags();
        if self.dedup_cell_mut(pat.node_arc_mut(), flags) {
            changed = true;
        }
        changed
    }

    fn dedup_cell_mut(&mut self, cell: &mut Arc<NodeCell>, flags: u8) -> bool {
        let id = Arc::as_ptr(cell) as usize;
        if let Some(node_cell) = Arc::get_mut(cell) {
            self.dedup_node_in_place(&mut node_cell.kind);
            let mut replaced = false;
            if can_intern_node(&cell.kind, flags) {
                match self.nodes.entry(InternedNode(Arc::clone(cell))) {
                    Entry::Occupied(entry) => {
                        *cell = Arc::clone(&entry.key().0);
                        replaced = true;
                    }
                    Entry::Vacant(entry) => {
                        entry.insert(());
                    }
                }
            }
            if self.detached_ids.contains(&id) {
                self.visited.insert(id, Arc::clone(cell));
            }
            return replaced;
        }
        if let Some(existing) = self.visited.get(&id) {
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

    fn dedup_node_in_place(&mut self, kind: &mut Node) {
        match kind {
            Node::Sequence(layers) => self.dedup_layers_mut(layers),
            Node::Pure(val) => self.dedup_value_mut(val),
            Node::PureAt(boxed) => self.dedup_value_mut(&mut boxed.0),
            Node::Stack(pats) | Node::SlowCat(pats) | Node::SlowCatPrime(pats) => {
                for pat in pats.iter_mut() {
                    self.dedup_pattern(pat);
                }
            }
            Node::StackPure(cells) | Node::SlowCatPure(cells) | Node::SlowCatPrimePure(cells) => {
                for cell in cells.iter_mut() {
                    self.dedup_cell_mut(cell, 0);
                }
            }
            Node::FastSmall(inner, f, ..)
            | Node::ShiftSmall(inner, f, ..)
            | Node::ControlMap(inner, f, ..)
            | Node::SplitQueries(inner, f)
            | Node::Join(inner, f, ..) => {
                self.dedup_cell_mut(inner, *f);
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
            }
            Node::Fast(node) => {
                self.dedup_pattern(&mut node.0);
            }
            Node::Shift(node) => {
                self.dedup_pattern(&mut node.0);
            }
            Node::Degrade(boxed) => {
                self.dedup_pattern(&mut boxed.0);
            }
            Node::AppWave(app) => {
                self.dedup_pattern(&mut app.inner);
            }
            Node::Constants(node) => {
                self.dedup_pattern(&mut node.0);
                for m in node.1.iter_mut() {
                    self.dedup_value_mut(&mut m.value);
                }
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
            }
            Node::AppLeft(app) => {
                self.dedup_pattern(&mut app.0);
                self.dedup_pattern(&mut app.1);
                app.2 = ValueReading::default();
            }
            Node::AppRight(app) | Node::AppBoth(app) => {
                self.dedup_pattern(&mut app.0);
                self.dedup_pattern(&mut app.1);
            }
            Node::Cpm(boxed) => {
                self.dedup_pattern(&mut boxed.0);
            }
            Node::UiVisual(node) => {
                self.dedup_pattern(&mut node.inner);
            }
            Node::ControlMapBoxed(boxed) => {
                self.dedup_pattern(&mut boxed.0);
            }
            Node::JoinBoxed(boxed) => {
                self.dedup_pattern(&mut boxed.0);
            }
            Node::AddContext(boxed) => {
                self.dedup_pattern(&mut boxed.0);
            }
            Node::FilterValues(boxed) => {
                self.dedup_pattern(&mut boxed.0);
            }
            Node::FilterHaps(boxed) => {
                self.dedup_pattern(&mut boxed.0);
            }
            Node::MapHapsOwned(boxed) => {
                self.dedup_pattern(&mut boxed.0);
            }
            Node::MapHapsNative(boxed) => {
                self.dedup_pattern(&mut boxed.0);
            }
            Node::ExpandHapsNative(boxed) => {
                self.dedup_pattern(&mut boxed.0);
            }
            Node::FmapNative(boxed) => {
                self.dedup_pattern(&mut boxed.0);
            }
            Node::WithRandSeed(boxed) => {
                self.dedup_pattern(&mut boxed.0);
            }
            Node::FastGap(boxed) => {
                self.dedup_pattern(&mut boxed.0);
            }
            Node::RepeatCycles(boxed) => {
                self.dedup_pattern(&mut boxed.0);
            }
            Node::LoopAt(boxed) => {
                self.dedup_pattern(&mut boxed.0);
            }
            Node::WithQueryTime(boxed) => {
                self.dedup_pattern(&mut boxed.0);
            }
            Node::WithHapTime(boxed) => {
                self.dedup_pattern(&mut boxed.0);
            }
            Node::WithQuerySpan(boxed) => {
                self.dedup_pattern(&mut boxed.0);
            }
            Node::WithHapSpan(boxed) => {
                self.dedup_pattern(&mut boxed.0);
            }
            Node::MapHapsWithState(boxed) => {
                self.dedup_pattern(&mut boxed.0);
            }
            Node::InnerJoin(boxed) => {
                self.dedup_pattern(&mut boxed.0);
            }
            Node::ChooseCycles(boxed) => {
                for p in boxed.0.iter_mut() {
                    self.dedup_pattern(p);
                }
            }
            Node::PatternOfPure(boxed) => {
                self.dedup_pattern(&mut boxed.0);
                boxed.2 = ConstructionMemo::default();
            }
            Node::PatternOfPureOperand(pair, _) => {
                self.dedup_pattern(&mut pair.0);
                self.dedup_pattern(&mut pair.1);
                pair.2 = ConstructionMemo::default();
            }
            Node::PatternOfHap(bind) => {
                self.dedup_pattern(&mut bind.0);
                for p in &mut bind.1 {
                    self.dedup_pattern(p);
                }
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
            }
            Node::Arp(pair) => {
                self.dedup_pattern(&mut pair.0);
                self.dedup_pattern(&mut pair.1);
            }
            Node::PickPatternified { patterns, .. } => {
                self.dedup_pattern(&mut patterns.0);
                self.dedup_pattern(&mut patterns.1);
            }
            Node::XenPatterns(boxed) => {
                self.dedup_pattern(&mut boxed.0);
                for p in &mut boxed.1 {
                    self.dedup_pattern(p);
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

    fn dedup_pat_child(&mut self, pat: &Pattern) -> Option<Pattern> {
        let next_node = self.dedup_cell(pat.node_arc(), pat.node_flags());
        let node_changed = !Arc::ptr_eq(pat.node_arc(), &next_node);
        let extras_changed = pat.plain_runtime_settings_key().is_some_and(|key| {
            self.plain_extras
                .get(&key)
                .is_some_and(|canon| pat.extras_ptr() != Arc::as_ptr(canon) as usize)
        });
        if !node_changed && !extras_changed {
            // Still register this plain_extras as canonical if first seen.
            if let Some(key) = pat.plain_runtime_settings_key()
                && !self.plain_extras.contains_key(&key)
            {
                let mut tmp = pat.clone();
                let plain_extras = &mut self.plain_extras;
                tmp.dedup_plain_extras(&mut |k, ex| {
                    Arc::clone(plain_extras.entry(k).or_insert_with(|| Arc::clone(ex)))
                });
            }
            return None;
        }
        let mut updated = pat.clone();
        if node_changed {
            updated.set_node_arc(next_node);
        }
        if extras_changed {
            let plain_extras = &mut self.plain_extras;
            updated.dedup_plain_extras(&mut |k, ex| {
                Arc::clone(plain_extras.entry(k).or_insert_with(|| Arc::clone(ex)))
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
        if let Some(existing) = self.visited.get(&id) {
            return Arc::clone(existing);
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
        let candidate = match rebuilt {
            Some(kind) => Arc::new(NodeCell { kind }),
            None => Arc::clone(cell),
        };
        let canon = if can_intern_node(&candidate.kind, flags) {
            match self.nodes.entry(InternedNode(candidate)) {
                Entry::Occupied(entry) => Arc::clone(&entry.key().0),
                Entry::Vacant(entry) => {
                    let out = Arc::clone(&entry.key().0);
                    entry.insert(());
                    out
                }
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

    fn dedup_values_mut(&mut self, values: &mut Arc<[Value]>) {
        Self::dedup_values_mut(self, values);
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

struct InternedValues(Arc<[Value]>);

#[inline]
fn hash_interned_value(v: &Value, state: &mut impl Hasher) {
    if let Value::Object(map) = v
        && let Some(entries) = map.as_builtin()
    {
        0x55u8.hash(state);
        (Arc::as_ptr(entries) as *const () as usize).hash(state);
        return;
    }
    hash_data(v, state);
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

impl Hash for InternedValues {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.len().hash(state);
        for v in self.0.iter() {
            hash_interned_value(v, state);
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
                    .all(|(a, b)| same_interned_value(a, b)))
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
        match self.0.as_slice() {
            Slice::Builtin(keys, values) => {
                keys.hash(state);
                for v in values {
                    hash_data(v, state);
                }
            }
            Slice::Pooled(keys, indices, pool) => {
                keys.hash(state);
                for &idx in indices {
                    hash_data(&pool[usize::from(idx)], state);
                }
            }
            Slice::Plain(pairs) => {
                pairs.len().hash(state);
                for (_, v) in pairs {
                    hash_data(v, state);
                }
            }
        }
    }
}

impl PartialEq for InternedBuiltin {
    fn eq(&self, other: &Self) -> bool {
        if Arc::ptr_eq(&self.0, &other.0) {
            return true;
        }
        match (self.0.as_slice(), other.0.as_slice()) {
            (Slice::Builtin(ak, av), Slice::Builtin(bk, bv)) => {
                ak == bk && av.iter().zip(bv.iter()).all(|(a, b)| a.same_data(b))
            }
            (Slice::Pooled(ak, ai, ap), Slice::Pooled(bk, bi, bp)) => {
                ak == bk
                    && ai
                        .iter()
                        .zip(bi.iter())
                        .all(|(&a, &b)| ap[usize::from(a)].same_data(&bp[usize::from(b)]))
            }
            _ => {
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
                            && x.steps == y.steps
                            && x.math_inside == y.math_inside
                            && x.wave.same(&y.wave)
                    })
            }
            _ => false,
        }
    }
}

impl Eq for InternedNode {}
