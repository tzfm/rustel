//! Pattern engine core: `TimeSpan`, `Hap`, `State`, `Pattern` and the joins.
//!
//! A `Pattern` is a pure function from a `State` to the haps active during its
//! span. Query semantics track the Strudel language exactly, including its
//! observable error behaviour.

pub mod callback_ir;
pub mod combinators;
pub mod compose;
mod construction;
mod context_keys;
pub mod controls;
pub mod gamepad;
pub mod locations;
pub mod midi_in;
pub mod midimap;
#[rustfmt::skip]
mod control_catalog;
// Keep the public module path stable.
pub mod controls_generated {
    pub use super::control_catalog::*;
}
pub mod euclid;
pub mod extension_node;
pub mod fdlibm;
pub mod host_value;
pub mod intern;
pub mod ops;
pub mod purity;
pub mod reference;
pub mod register;
pub mod rng;
pub mod settings;
pub mod signal;
pub mod tonal;
pub mod tonaljs;
pub mod tonaljs_scales;
pub mod tune;
pub mod util;
pub mod value;
pub mod voicings;
pub mod xen;

mod fused;
pub use fused::Wave;
mod sequence;
mod table;

mod hap;
mod host;
mod query;
#[cfg(test)]
mod tests;

#[cfg(test)]
use hap::query_order;
pub use hap::{
    Hap, MAX_PATTERN_DEPTH, MAX_QUERY_SPAN_CYCLES, State, TimeSpan, sort_haps_by_part,
    sort_haps_for_query, whole_cycle,
};
pub(crate) use hap::{joined_pick_lookup, order_key, tags_from_value};
pub(crate) use host::*;
pub use host::{
    BindArg, BindResult, CallbackHost, CallbackId, CallbackQueryKind, CallbackQueryKindCounts,
    CallbackQueryMetrics, DEFAULT_HAP_BUDGET, MAX_STEPWISE_ENTRIES, MAX_STEPWISE_SEGMENTS,
    OwnedHapMapFn, PatternedPureMap, QueryArcOutcome, QueryLimit, ValuePredicate, charge_hap_at,
    charge_stepwise_entries, haps_materialised, installed_query_deadline_millis, interned_closure,
    live_node_count, mark_stepwise_refusal, materialize_js_value,
    note_stepwise_entries_materialised, peak_hap_vector, query_in_progress, query_interrupted,
    refuse_euclid_steps, refuse_host_memory, refuse_js_cpu_deadline, refuse_js_pending_jobs,
    refuse_native_fraction, reset_haps_materialised, reset_stepwise_entries_materialised,
    reset_stepwise_segments_materialised, signal_callback_failure, signal_query_error,
    stepwise_entries_materialised, stepwise_segments_materialised, take_query_callback_failure,
    take_query_contained_throw, take_query_error, uncacheable_touches, with_callback_host,
    with_callback_query_metrics, with_cancellation, with_cancellation_and_query_budget,
    with_query_budget, with_query_deadline, with_query_deadline_from,
};
pub(crate) use query::*;

use context_keys::ContextKeys;
pub use locations::Locations;
use rustel_fraction::Fraction;
pub use sequence::{
    GivenLayer, IndexedLayer, SequenceBuilder, SequenceLayer, SequenceStep, SourceStep,
    SourcedLayer, StepSource, TickCut, TickStep, sequence, sequence_of, sequence_of_cut_ticks,
    sequence_of_equal_values, sequence_of_indexed_slice, sequence_of_indexed_ticks_plain,
    sequence_of_sourced, sequence_of_ticks, sequence_of_ticks_plain,
};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};
pub use table::{DEFAULT_TABLE_BUDGET, set_table_budget, table_bytes};
pub use value::{Key, OrderedMap, Value, WordHasher, arc_single_value};

// ---------------------------------------------------------------------------
// Pattern
//
// `Pattern` is a pure function from a State to the Haps active during it.
// The node enum keeps the graph inspectable, which is what makes purity
// classification a structural property rather than a guess.
// ---------------------------------------------------------------------------

/// Bit-packed purity flags stored on [`NodeCell`] (1 byte).
const FLAG_IMPURE: u8 = 1 << 0;
const FLAG_OPAQUE: u8 = 1 << 1;
const FLAG_VOLATILE: u8 = 1 << 2;
const FLAG_HOST_VOICINGS: u8 = 1 << 3;
const FLAG_SPAN_MASK: u8 = 0b11 << 4;
const FLAG_SPAN_NO: u8 = 0 << 4;
const FLAG_SPAN_MADE: u8 = 1 << 4;
const FLAG_SPAN_YES: u8 = 2 << 4;

fn pack_purity_flags(purity: &purity::Purity) -> u8 {
    let mut flags = 0u8;
    if purity.impure {
        flags |= FLAG_IMPURE;
    }
    if purity.opaque {
        flags |= FLAG_OPAQUE;
    }
    if purity.volatile {
        flags |= FLAG_VOLATILE;
    }
    if purity.host_voicings {
        flags |= FLAG_HOST_VOICINGS;
    }
    flags |= match purity.span_read {
        purity::SpanRead::No => FLAG_SPAN_NO,
        purity::SpanRead::Made => FLAG_SPAN_MADE,
        purity::SpanRead::Yes => FLAG_SPAN_YES,
    };
    flags
}

fn unpack_span_read(flags: u8) -> purity::SpanRead {
    match flags & FLAG_SPAN_MASK {
        FLAG_SPAN_NO => purity::SpanRead::No,
        FLAG_SPAN_MADE => purity::SpanRead::Made,
        _ => purity::SpanRead::Yes,
    }
}

/// Ultra-rare handle-level metadata kept behind one `Box` so [`PatternExtras`]
/// stays 24 bytes.
#[derive(Clone, Default)]
struct PatternExtrasRare {
    large_steps: Option<Fraction>,
    pure_loc: Option<(u32, u32)>,
    reachable: Option<Arc<Vec<CallbackId>>>,
}

/// Rare handle-level metadata kept out-of-line so [`Pattern`] stays 24 bytes.
/// Compact single-range `pure_loc` (packed via [`pack_run_loc`]) sits inline so
/// located `pure` handles avoid a secondary `PatternExtrasRare` box allocation.
#[derive(Clone, Default)]
pub(crate) struct PatternExtras {
    runtime_settings: Option<settings::RuntimeSettings>,
    rare: Option<Box<PatternExtrasRare>>,
    pure_loc_packed: u32,
}

const _: () = assert!(size_of::<PatternExtras>() <= 24);

impl PatternExtras {
    #[inline]
    pub(crate) fn of_runtime_settings(settings: settings::RuntimeSettings) -> Self {
        Self {
            runtime_settings: Some(settings),
            rare: None,
            pure_loc_packed: 0,
        }
    }

    /// True when this extras record carries only `runtime_settings`, so
    /// ancestor nodes with no reachable JS callbacks can share its `Arc`.
    #[inline]
    pub(crate) fn is_plain_runtime_settings(&self) -> bool {
        self.rare.is_none() && self.pure_loc_packed == 0 && self.runtime_settings.is_some()
    }

    #[inline]
    pub(crate) fn plain_runtime_settings_key(&self) -> Option<usize> {
        if self.is_plain_runtime_settings() {
            self.runtime_settings
                .as_ref()
                .map(settings::RuntimeSettings::handle_ptr)
        } else {
            None
        }
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.runtime_settings.is_none() && self.rare.is_none() && self.pure_loc_packed == 0
    }

    #[inline]
    fn reachable(&self) -> Option<&Arc<Vec<CallbackId>>> {
        self.rare.as_deref().and_then(|r| r.reachable.as_ref())
    }

    #[inline]
    fn large_steps(&self) -> Option<Fraction> {
        self.rare.as_deref().and_then(|r| r.large_steps)
    }

    #[inline]
    fn pure_loc(&self) -> Option<(u32, u32)> {
        if self.pure_loc_packed != 0 {
            let (b, e) = unpack_run_loc(self.pure_loc_packed);
            return Some((b as u32, e as u32));
        }
        self.rare.as_deref().and_then(|r| r.pure_loc)
    }

    fn set_large_steps(&mut self, steps: Option<Fraction>) {
        match steps {
            Some(steps) => self.rare.get_or_insert_with(Default::default).large_steps = Some(steps),
            None => {
                if let Some(rare) = self.rare.as_deref_mut() {
                    rare.large_steps = None;
                    if rare.pure_loc.is_none() {
                        self.rare = None;
                    }
                }
            }
        }
    }

    fn set_pure_loc(&mut self, loc: Option<(u32, u32)>) {
        match loc {
            Some((b, e)) => {
                if let Some(packed) = pack_run_loc((b as usize, e as usize)) {
                    self.pure_loc_packed = packed;
                    if let Some(rare) = self.rare.as_deref_mut() {
                        rare.pure_loc = None;
                        if rare.large_steps.is_none() {
                            self.rare = None;
                        }
                    }
                } else {
                    self.pure_loc_packed = 0;
                    self.rare.get_or_insert_with(Default::default).pure_loc = Some((b, e));
                }
            }
            None => {
                self.pure_loc_packed = 0;
                if let Some(rare) = self.rare.as_deref_mut() {
                    rare.pure_loc = None;
                    if rare.large_steps.is_none() {
                        self.rare = None;
                    }
                }
            }
        }
    }
}

/// Bitmask for inline small `_steps` denominator in `Pattern::step_denom`
/// (low 24 bits, up to 16,777,215; larger denominators spill to `PatternExtras`).
const STEP_DENOM_MASK: u32 = 0x00ff_ffff;
/// High 8 bits of `Pattern::step_denom` hold the node's packed purity/span
/// flags so [`NodeCell`] carries zero flag/padding bytes.
const NODE_FLAGS_SHIFT: u32 = 24;

/// Compact 8-byte reference to a [`Pattern`]'s underlying graph node, used by
/// dense caches that keep per-handle metadata only for non-plain handles.
#[doc(hidden)]
#[derive(Clone)]
pub struct PatternNodeRef(pub(crate) Arc<NodeCell>);

#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<PatternNodeRef>() == 8);

impl PatternNodeRef {
    #[inline]
    pub fn strong_count(&self) -> usize {
        Arc::strong_count(&self.0)
    }

    #[inline]
    pub fn identity(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }
}

/// Rare per-handle metadata split out of a [`Pattern`] when stored densely.
#[doc(hidden)]
#[derive(Clone)]
pub struct PatternHandleMeta {
    step_numer: i32,
    step_denom_low: u32,
    extras: Option<Arc<PatternExtras>>,
}

/// Shared plain `runtime_settings` extras handle stored once per dense cache
/// (such as the native score memo) so parts sharing the session's runtime
/// settings avoid per-part [`PatternHandleMeta`] entries.
#[doc(hidden)]
#[derive(Clone)]
pub struct PatternSharedExtras(Arc<PatternExtras>);

impl PatternSharedExtras {
    /// Construct a reusable handle carrying only `settings`, allowing a
    /// runtime to export patterns without allocating a fresh `PatternExtras`
    /// `Arc` on every evaluation.
    #[doc(hidden)]
    #[inline]
    pub fn for_runtime_settings(settings: settings::RuntimeSettings) -> Self {
        Self(Arc::new(PatternExtras::of_runtime_settings(settings)))
    }

    #[inline]
    fn matches(&self, other: &PatternExtras) -> bool {
        match (&self.0.runtime_settings, &other.runtime_settings) {
            (Some(a), Some(b)) => a.same_handle(b),
            _ => false,
        }
    }
}

/// Compact metadata tag for [`Pattern::split_compact`].
#[doc(hidden)]
pub const COMPACT_META_STEPS: u8 = 0;
#[doc(hidden)]
pub const COMPACT_META_PURE_LOC: u8 = 1;
#[doc(hidden)]
pub const COMPACT_META_SHARED_EXTRAS: u8 = 2;

impl Pattern {
    /// Split this handle into its 8-byte node reference, 1-byte packed flags,
    /// inline small steps `(i16, u16)` (or a 32-bit inline `pure_loc`), a 1-byte
    /// metadata tag, and optional rare metadata when steps overflow or
    /// non-shared `extras` are present.
    #[doc(hidden)]
    #[inline]
    pub fn split_compact(
        self,
        shared: &mut Option<PatternSharedExtras>,
    ) -> (PatternNodeRef, u8, i16, u16, u8, Option<PatternHandleMeta>) {
        let flags = self.node_flags();
        let denom_low = self.inline_step_denom();
        if self.extras.is_none() {
            if denom_low == 0 && self.step_numer != 0 {
                let packed = self.step_numer as u32;
                return (
                    PatternNodeRef(self.node),
                    flags,
                    (packed & 0xffff) as u16 as i16,
                    (packed >> 16) as u16,
                    COMPACT_META_PURE_LOC,
                    None,
                );
            }
            if let (Ok(n), Ok(d)) = (i16::try_from(self.step_numer), u16::try_from(denom_low)) {
                return (
                    PatternNodeRef(self.node),
                    flags,
                    n,
                    d,
                    COMPACT_META_STEPS,
                    None,
                );
            }
        } else if let Some(ex) = &self.extras
            && ex.is_plain_runtime_settings()
            && let (Ok(n), Ok(d)) = (i16::try_from(self.step_numer), u16::try_from(denom_low))
        {
            let matched = match shared {
                Some(existing) => existing.matches(ex),
                None => {
                    *shared = Some(PatternSharedExtras(Arc::clone(ex)));
                    true
                }
            };
            if matched {
                return (
                    PatternNodeRef(self.node),
                    flags,
                    n,
                    d,
                    COMPACT_META_SHARED_EXTRAS,
                    None,
                );
            }
        }
        let meta = PatternHandleMeta {
            step_numer: self.step_numer,
            step_denom_low: denom_low,
            extras: self.extras,
        };
        (
            PatternNodeRef(self.node),
            flags,
            0,
            0,
            COMPACT_META_STEPS,
            Some(meta),
        )
    }

    /// When this pattern is a [`Node::FastSmall`] or [`Node::Fast`] wrapping a
    /// child node, reconstruct that child handle with `steps` so incremental
    /// memoization can retain it without cloning before `fast`/`slow` runs.
    #[doc(hidden)]
    #[inline]
    pub fn fast_inner_pattern(&self, steps: Option<Fraction>) -> Option<Pattern> {
        match self.node_kind() {
            Node::FastSmall(inner, flags, ..) => {
                // Share the child node already owned by this `FastSmall` wrapper.
                Some(
                    Pattern::from_compact(
                        &PatternNodeRef(Arc::clone(inner)),
                        *flags,
                        0,
                        0,
                        COMPACT_META_STEPS,
                        None,
                        None,
                    )
                    .with_steps(steps),
                )
            }
            Node::Fast(node) => Some(node.0.clone()),
            _ => None,
        }
    }

    /// Reconstruct a [`Pattern`] from [`Self::split_compact`].
    #[doc(hidden)]
    #[inline]
    pub fn from_compact(
        node: &PatternNodeRef,
        flags: u8,
        step_numer: i16,
        step_denom: u16,
        meta_kind: u8,
        shared: Option<&PatternSharedExtras>,
        meta: Option<&PatternHandleMeta>,
    ) -> Self {
        match meta {
            None if meta_kind == COMPACT_META_PURE_LOC => {
                let packed = u32::from(step_numer as u16) | (u32::from(step_denom) << 16);
                Self {
                    node: Arc::clone(&node.0),
                    step_numer: packed as i32,
                    step_denom: u32::from(flags) << NODE_FLAGS_SHIFT,
                    extras: None,
                }
            }
            None if meta_kind == COMPACT_META_SHARED_EXTRAS => Self {
                node: Arc::clone(&node.0),
                step_numer: i32::from(step_numer),
                step_denom: (u32::from(flags) << NODE_FLAGS_SHIFT) | u32::from(step_denom),
                extras: shared.map(|s| Arc::clone(&s.0)),
            },
            None => Self {
                node: Arc::clone(&node.0),
                step_numer: i32::from(step_numer),
                step_denom: (u32::from(flags) << NODE_FLAGS_SHIFT) | u32::from(step_denom),
                extras: None,
            },
            Some(meta) => Self {
                node: Arc::clone(&node.0),
                step_numer: meta.step_numer,
                step_denom: (u32::from(flags) << NODE_FLAGS_SHIFT) | meta.step_denom_low,
                extras: meta.extras.clone(),
            },
        }
    }
}

#[derive(Clone)]
pub struct Pattern {
    node: Arc<NodeCell>,
    /// Packed small `_steps` numerator; active when `(step_denom & STEP_DENOM_MASK) != 0`.
    step_numer: i32,
    /// Low 24 bits: inline `_steps` denominator (`0` means none or `extras.large_steps`).
    /// High 8 bits: packed purity and span-read flags for `node`.
    step_denom: u32,
    /// Rare handle-level metadata (`pure_loc`, `runtime_settings`, large
    /// `i128` step counts, handle-added volatility, or reachable JS callbacks).
    /// `None` on nearly all pure handles.
    extras: Option<Arc<PatternExtras>>,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<Pattern>() == 24);
// A score builds thousands of nodes, so a node's size is most of a graph's
// memory. A variant that grows every node fails here: box its rare part.
const _: () = assert!(std::mem::size_of::<NodeCell>() <= 24);

/// A rewrite of the pitch of each hap, as [`Pattern::pitched`] applies it.
struct PitchRewrite {
    /// Interning key (for example, scale name) for lock-free thread-local hits.
    key: Arc<str>,
    /// The hap that the rewrite makes of a hap.
    hap: HapMapFn,
    /// The note that the rewrite makes of a note or a step that a hap
    /// holds alone.
    note: sequence::NoteFn,
    /// Context keys added by this rewrite (for example, `scale` tag), avoiding
    /// a dummy hap rewrite per layer during sequence folding.
    keys: Option<context_keys::ContextKeys>,
    /// Prebuilt `Dress` for renoting a bare (`dress == None`) layer under
    /// `keys`, shared across all voices using the same rewrite.
    bare_dress: Option<Arc<sequence::Dress>>,
    /// Optional precomputed note names for integer scale degrees `-8..24`,
    /// allowing `IntRange` scale renoting without virtual calls or TLS guards.
    degree_notes: Option<Arc<[value::SharedStr; 32]>>,
}

thread_local! {
    static PITCH_REWRITE_TLS: std::cell::RefCell<([Option<Arc<PitchRewrite>>; 4], usize)> =
        const { std::cell::RefCell::new(([const { None }; 4], 0)) };
}

static PITCH_REWRITE_POOL: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, std::sync::Weak<PitchRewrite>>>,
> = std::sync::OnceLock::new();

/// The controls of a row of a chain, each apart as a map of controls with
/// its locations, for a node that holds them so. Most nodes take the
/// union of a row alone, so the list is made when a node first reads it.
pub(crate) struct Apart<'a> {
    count: usize,
    make: &'a dyn Fn() -> Vec<(Value, Locations)>,
    made: std::cell::OnceCell<Vec<(Value, Locations)>>,
}

impl<'a> Apart<'a> {
    pub(crate) fn new(count: usize, make: &'a dyn Fn() -> Vec<(Value, Locations)>) -> Self {
        Self {
            count,
            make,
            made: std::cell::OnceCell::new(),
        }
    }

    #[inline]
    fn has_many(&self) -> bool {
        self.count > 1
    }

    fn each(&self) -> &[(Value, Locations)] {
        self.made.get_or_init(self.make)
    }
}

/// A lock-free graph node shared by every handle to it. Purity flags travel in
/// the high byte of [`Pattern::step_denom`] so `NodeCell` stays 24 bytes.
pub(crate) struct NodeCell {
    pub(crate) kind: Node,
}

fn purity_from_plain_flags(flags: u8) -> purity::Purity {
    purity::Purity::from_packed(
        flags & FLAG_IMPURE != 0,
        flags & FLAG_OPAQUE != 0,
        flags & FLAG_VOLATILE != 0,
        flags & FLAG_HOST_VOICINGS != 0,
        unpack_span_read(flags),
        None,
    )
}

#[inline(always)]
pub(crate) fn plain_flags_cacheable(flags: u8) -> bool {
    (flags & (FLAG_IMPURE | FLAG_VOLATILE)) == 0
        && ((flags & FLAG_HOST_VOICINGS) == 0 || !settings::default_voicings_is_host_owned())
}

#[inline(always)]
pub(crate) fn plain_pattern(node: &Arc<NodeCell>, flags: u8) -> Pattern {
    Pattern {
        node: Arc::clone(node),
        step_numer: 0,
        step_denom: u32::from(flags) << NODE_FLAGS_SHIFT,
        extras: None,
    }
}

#[inline]
pub(crate) fn plain_pattern_owned(node: Arc<NodeCell>, flags: u8) -> Pattern {
    Pattern {
        node,
        step_numer: 0,
        step_denom: u32::from(flags) << NODE_FLAGS_SHIFT,
        extras: None,
    }
}

/// Mutate every non-silence entry of a compact pure-cell slice in place without
/// cloning `Arc<NodeCell>` handles. Returns `Some(boxed_patterns)` only if an
/// entry ceased to satisfy [`is_plain_pure_cell`] and the parent must widen to
/// its full `Box<[Pattern]>` variant.
fn mutate_pure_cells(
    cells: &mut Box<[Arc<NodeCell>]>,
    mut f: impl FnMut(&mut Pattern),
) -> Option<Box<[Pattern]>> {
    let mut spilled: Option<Vec<Pattern>> = None;
    for idx in 0..cells.len() {
        if matches!(cells[idx].kind, Node::Silence) {
            if let Some(vec) = spilled.as_mut() {
                vec.push(plain_pattern(&cells[idx], 0));
            }
            continue;
        }
        let owned = std::mem::replace(&mut cells[idx], silence().node);
        let mut pat = plain_pattern_owned(owned, 0);
        f(&mut pat);
        if let Some(vec) = spilled.as_mut() {
            vec.push(pat);
        } else if is_plain_pure_cell(&pat) {
            cells[idx] = pat.node;
        } else {
            let mut vec = Vec::with_capacity(cells.len());
            for prev in &cells[..idx] {
                vec.push(plain_pattern(prev, 0));
            }
            vec.push(pat);
            spilled = Some(vec);
        }
    }
    spilled.map(Vec::into_boxed_slice)
}

/// Borrowed view of either a full [`Pattern`] handle or a plain `(NodeCell, flags)`
/// child stored inline inside compact 24-byte [`Node`] variants.
#[derive(Clone, Copy)]
pub(crate) enum NodeRef<'a> {
    Pat(&'a Pattern),
    Plain(&'a NodeCell, u8),
}

impl<'a> NodeRef<'a> {
    #[inline(always)]
    pub(crate) fn kind(self) -> &'a Node {
        match self {
            Self::Pat(pat) => pat.node_kind(),
            Self::Plain(cell, _) => &cell.kind,
        }
    }

    #[inline(always)]
    pub(crate) fn has_runtime_settings(self) -> bool {
        match self {
            Self::Pat(pat) => pat.has_runtime_settings(),
            Self::Plain(..) => false,
        }
    }

    #[inline(always)]
    pub(crate) fn is_cacheable(self) -> bool {
        match self {
            Self::Pat(pat) => pat.is_cacheable(),
            Self::Plain(_, flags) => plain_flags_cacheable(flags),
        }
    }

    #[inline(always)]
    pub(crate) fn tables_may_answer(self) -> bool {
        match self {
            Self::Pat(pat) => pat.tables_may_answer(),
            Self::Plain(_, flags) => plain_flags_cacheable(flags),
        }
    }

    pub(crate) fn under_structure(self) -> Option<&'a Pattern> {
        let Node::FilterValues(boxed) = self.kind() else {
            return None;
        };
        let (structured, keep) = (&boxed.0, &boxed.1);
        let Node::AppRight(node) = structured.node_kind() else {
            return None;
        };
        let AppNode(functions, structure, Combine::With(combine), LookupFlow::Left) = &**node
        else {
            return None;
        };
        let kept = KEEP_DEFINED
            .get()
            .is_some_and(|defined| Arc::ptr_eq(defined, keep))
            && compose::is_keep_if(combine);
        let keeps_structured = match self {
            Self::Pat(pat) => pat.keeps_runtime_over(structured),
            Self::Plain(..) => !structured.has_runtime_settings(),
        };
        let bound = keeps_structured && structured.keeps_runtime_over(functions);
        (kept && sequence::is_bare(structure) && bound).then_some(functions)
    }
}

#[derive(Clone, Debug, Default)]
pub struct TimelineState {
    offsets: Arc<Mutex<BTreeMap<String, Fraction>>>,
}

impl TimelineState {
    /// Clears the offsets, and gives the offsets before.
    pub fn take(&self) -> BTreeMap<String, Fraction> {
        std::mem::take(
            &mut self
                .offsets
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    /// Puts back offsets that [`Self::take`] gave.
    pub fn restore(&self, offsets: BTreeMap<String, Fraction>) {
        *self
            .offsets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = offsets;
    }

    fn get(&self, key: &str) -> Option<Fraction> {
        self.offsets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(key)
            .copied()
    }

    fn insert(&self, key: String, value: Fraction) {
        self.offsets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, value);
    }

    fn remove(&self, key: &str) {
        self.offsets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(key);
    }
}

/// Which operand's `whole` survives a join.
///
/// The first three are `bindWhole` specialisations and share one loop; the
/// last three align the inner pattern differently and have their own loops.
/// One enum because they all consume the same `PatternOf*` node, which is
/// what carries the purity proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinMode {
    /// `innerJoin`: `(_, b) => b`
    Inner,
    /// `outerJoin`: `(a) => a`
    Outer,
    /// `join`/`bind`: `a.intersection_e(b)`
    Mix,
    /// `squeezeJoin`: each inner pattern is `focusSpan`ed onto the outer hap's
    /// `wholeOrPart`, so a whole cycle of the inner fits each outer event.
    Squeeze,
    /// `resetJoin(false)`: inner patterns are re-aligned so their **cycle
    /// start** lands on each outer onset.
    Reset,
    /// `resetJoin(true)` / `restartJoin`: inner patterns are re-aligned so their
    /// **cycle zero** lands on each outer onset.
    Restart,
}

/// How a binary value application carries an out-of-band pick lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LookupFlow {
    /// Preserve the source whose identity-bearing value was returned.
    Infer,
    /// Canonical default-alignment `add`, semantically identical to `Infer`
    /// but retained as a non-executing structural witness for callback IR.
    Add,
    /// `keep` / `keepif`: the left value is the result.
    Left,
    /// Native augmentation retains the left object's named audio controls.
    /// The result is an object, so a raw slider binding does not survive.
    LeftControls,
    /// The right value is the result.
    Right,
    /// `set`: right replaces scalars/arrays; objects merge right over left.
    Set,
}

/// The message for a hap whose value would have to be a `Pattern`.
///
/// Strudel can produce this intermediate value but cannot serialize it. Reject
/// it here with a useful message rather than exposing an opaque host value.
fn pattern_valued_hap_error() -> String {
    "a top-level pattern-of-patterns node is join-only - query a join \
     (polyJoin/stepJoin/polyBind/stepBind) rather than the intermediate node itself"
        .into()
}

/// Step slicing calls `value.withHap(...)` with no guard, and V8 words the
/// resulting `TypeError` two different ways depending on the value.
///
/// `undefined.withHap` fails on the property read; `'bd'.withHap` reads
/// `undefined` and fails on the call. Both are reachable
/// (`pure(undefined).stepJoin()` and `pure('bd').stepJoin()`), and the
/// message is observable through `try`/`catch`, so both wordings are
/// reproduced.
fn step_join_value_error(value: &Value) -> String {
    match value {
        Value::Undefined => "Cannot read properties of undefined (reading 'withHap')".to_string(),
        Value::Null => "Cannot read properties of null (reading 'withHap')".to_string(),
        _ => "x.value.withHap is not a function".to_string(),
    }
}

/// `groupHapsBy(congruent, haps)` - haps with equal wholes, first-seen order.
///
/// Congruence short-circuits only when both wholes are missing. An analog
/// hap compared with a discrete one dereferences its missing whole and
/// throws. The throw is reproduced here as a query error, because a stack
/// of an analog signal and a discrete note is easy to write.
fn collect_congruent(haps: Vec<Hap>) -> Vec<Vec<Hap>> {
    let mut groups: Vec<Vec<Hap>> = Vec::new();
    for hap in haps {
        let mut placed = false;
        for group in &mut groups {
            let other = &group[0];
            let congruent = match (hap.whole(), other.whole()) {
                (None, None) => true,
                // `this.whole.equals(other.whole)` with `this.whole` undefined.
                (None, Some(_)) => {
                    signal_query_error(|| {
                        "Cannot read properties of undefined (reading 'equals')".into()
                    });
                    return Vec::new();
                }
                (Some(_), None) => false,
                (Some(a), Some(b)) => a == b,
            };
            if congruent {
                group.push(hap.clone());
                placed = true;
                break;
            }
        }
        if !placed {
            groups.push(vec![hap]);
        }
    }
    groups
}

/// `haps[_mod(i, haps.length)]` - JavaScript array indexing with the
/// Euclidean `_mod`, so negative integers wrap.
///
/// `None` where JS yields `undefined`: a fractional index (`_mod` keeps the
/// fraction and `haps[2.5]` is undefined), a non-numeric value, or an empty
/// group.
fn js_index(value: &Value, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let i = value.as_f64().or_else(|| match value {
        Value::Str(s) => s.trim().parse::<f64>().ok(),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        Value::Null => Some(0.0),
        _ => None,
    })?;
    // `_mod`'s `((i % n) + n) % n` maps a tiny negative `i` to 0;
    // `rem_euclid` would round it up to `n`. The clamp keeps it in bounds.
    let rem = crate::util::modulo_f64(i, len as f64);
    if rem.fract() != 0.0 {
        return None;
    }
    Some((rem as usize).min(len - 1))
}

/// The cumulative-weight sequence: a bind fold that concatenates each weight
/// onto a list, one pattern at a time.
///
/// Intermediate cumulative weights are ordinary `Value::List`s. Each
/// iteration uses bind's MIX whole and context order exactly.
fn query_weight_sequence(cumulative: &[Pattern], state: &State) -> Vec<Hap> {
    let spans = state.span.span_cycles();
    if !charge_haps(spans.len()) {
        return Vec::new();
    }
    note_materialised(spans.len());
    let mut current: Vec<Hap> = spans
        .into_iter()
        .map(|part| Hap::new(Some(whole_cycle(part.begin)), part, Value::list(Vec::new())))
        .collect();

    for pattern in cumulative {
        let mut next = Vec::new();
        for outer in current {
            let inners = pattern.query(&state.set_span(outer.part()));
            if query_error_pending() {
                return Vec::new();
            }
            for inner in inners {
                let whole = match (outer.whole(), inner.whole()) {
                    (Some(a), Some(b)) => match a.intersection(&b) {
                        Some(span) => Some(span),
                        None => {
                            signal_query_error(|| "TimeSpans do not intersect".into());
                            return Vec::new();
                        }
                    },
                    _ => None,
                };
                let mut context = outer.context.clone();
                context.append(&inner.context);
                let mut values = match &outer.value {
                    Value::List(values) => values.to_vec(),
                    _ => Vec::new(),
                };
                values.push(inner.value.clone());
                if !push_budgeted(
                    &mut next,
                    Hap::from_parts(
                        whole,
                        inner.part(),
                        Value::list(values),
                        context,
                        ContextKeys::merge_with(&inner.keys, &outer.keys, [0; 4], 0, None),
                    ),
                ) {
                    return Vec::new();
                }
            }
        }
        current = next;
    }
    current
}

/// One selected-pattern carrier produced by the complete `pat.bind(match)`
/// phase, before the final outerJoin/innerJoin queries any selected value.
struct WChooseCarrier {
    // Selection can miss (no threshold exceeds the random value). A miss is
    // merely an undefined selection; the failure is not raised until the
    // final join tries to query that carrier. Keeping the absence here
    // preserves callbacks/errors from earlier joined carriers.
    selected: Option<usize>,
    outer_whole: Option<TimeSpan>,
    part: TimeSpan,
    context: Locations,
    keys: ContextKeys,
}

#[derive(Clone)]
struct PickLookupHap {
    hap: Hap,
    lookup: Arc<PickLookup>,
}

/// Query the weighted-choice node out of line.
///
/// This is deliberately not an inline match arm in `Pattern::query_node`.
/// That function recurses once per graph node; putting weighted choice's
/// nested-loop locals in its frame made an unrelated 64-layer native graph
/// overflow the CLI's main-thread stack even though it never selected this
/// variant. Keeping the large frame here makes its cost pay-for-play.
#[inline(never)]
fn query_wchoose(
    chooser: &Pattern,
    total: &Pattern,
    cumulative: &[Pattern],
    values: &[Pattern],
    inner: bool,
    state: &State,
) -> Vec<Hap> {
    debug_assert_eq!(
        cumulative.len(),
        values.len(),
        "one cumulative threshold must exist for every weighted value"
    );
    // The COMPLETE carrier pattern is built and queried first; only after
    // that succeeds does the final outerJoin/innerJoin query selected values.
    // Interleaving these phases leaks selected-value callback side effects
    // before a later weight throws.
    let random_haps = chooser.query(state);
    if query_error_pending() {
        return Vec::new();
    }
    let mut carriers = Vec::new();
    for random_hap in random_haps {
        // `pat.bind(match)`: match is queried over the random hap's part, and
        // MIX whole/context are applied below after the weighted threshold has
        // selected a value pattern.
        let match_state = state.set_span(random_hap.part());
        let weights_haps = query_weight_sequence(cumulative, &match_state);
        if query_error_pending() {
            return Vec::new();
        }
        for weights_hap in weights_haps {
            // `findpat = total.mul(r)`, queried by appLeft over the weight-list
            // hap's whole. `total` always retains the discrete pure(0)
            // structure, so scalar multiplication changes only its value.
            let find_state = state.set_span(weights_hap.whole_or_part());
            let total_haps = total.query(&find_state);
            if query_error_pending() {
                return Vec::new();
            }
            for total_hap in total_haps {
                let Some(part) = weights_hap.part().intersection(&total_hap.part()) else {
                    continue;
                };
                let find = compose::compose_op(
                    compose::ComposeOp::Mul,
                    &total_hap.value,
                    &random_hap.value,
                );
                let Value::List(weights) = &weights_hap.value else {
                    // Internal invariant: `query_weight_sequence` constructs
                    // every carrier itself and always stores a Value::List.
                    debug_assert!(false, "weighted sequence carrier was not a list");
                    return Vec::new();
                };
                let selected = weights.iter().position(|weight| {
                    compose::ComposeOp::Gt
                        .apply_scalar(weight, &find)
                        .js_truthy()
                });

                // `weightspat.fmap(...).appLeft(findpat)`: function side whole,
                // find context first.
                let chooser_whole = weights_hap.whole();
                let mut chooser_context = total_hap.context.clone();
                chooser_context.append(&weights_hap.context);

                // `pat.bind(match)`: MIX the random hap with the
                // selected-pattern carrier.
                let outer_whole = match (random_hap.whole(), chooser_whole) {
                    (Some(a), Some(b)) => match a.intersection(&b) {
                        Some(span) => Some(span),
                        None => {
                            signal_query_error(|| "TimeSpans do not intersect".into());
                            return Vec::new();
                        }
                    },
                    _ => None,
                };
                let mut outer_context = random_hap.context.clone();
                outer_context.append(&chooser_context);

                if !charge_haps(carriers.len() + 1) {
                    return Vec::new();
                }
                carriers.push(WChooseCarrier {
                    selected,
                    outer_whole,
                    part,
                    context: outer_context,
                    keys: ContextKeys::merge_with(
                        &weights_hap.keys,
                        &ContextKeys::merge_with(
                            &total_hap.keys,
                            &random_hap.keys,
                            [0; 4],
                            0,
                            None,
                        ),
                        [0; 4],
                        0,
                        None,
                    ),
                });
            }
        }
    }

    // Final `.outerJoin()` for wchoose or `.innerJoin()` for wchooseCycles.
    let mut out = Vec::new();
    for carrier in carriers {
        let Some(selected) = carrier.selected else {
            // A missed selection is undefined and only dereferenced when
            // outerJoin/innerJoin reaches this carrier, so earlier carriers
            // have already run.
            signal_query_error(|| "Cannot read properties of undefined (reading 'query')".into());
            return Vec::new();
        };
        let chosen_haps = values[selected].query(&state.set_span(carrier.part));
        if query_error_pending() {
            return Vec::new();
        }
        for chosen in chosen_haps {
            let mut context = carrier.context.clone();
            context.append(&chosen.context);
            if !push_budgeted(
                &mut out,
                Hap::from_parts(
                    if inner {
                        chosen.whole()
                    } else {
                        carrier.outer_whole
                    },
                    chosen.part(),
                    chosen.value,
                    context,
                    ContextKeys::merge(&chosen.keys, &carrier.keys),
                ),
            ) {
                return Vec::new();
            }
        }
    }
    out
}

/// A JavaScript lookup after `objectMap(lookup, reify)`.
///
/// Arrays keep their JavaScript `length`, their separate `Object.keys` count,
/// and only the indices that `Array.prototype.map` visited. The distinction is
/// observable for sparse arrays and arrays with enumerable non-index fields.
#[derive(Clone)]
pub enum PickLookup {
    Array {
        enumerable_len: usize,
        length: usize,
        entries: Vec<(usize, Pattern)>,
    },
    Object {
        enumerable_len: usize,
        entries: Vec<(value::SharedStr, Pattern)>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickIndexMode {
    Clamp,
    Remainder,
    Modulo,
}

impl PickLookup {
    fn attach_runtime_settings(&mut self, settings: &settings::RuntimeSettings) -> bool {
        if cancellation_requested() || query_deadline_expired() {
            return false;
        }
        let shared = settings::RuntimeSettings::plain_extras_for(settings)
            .unwrap_or_else(|| Arc::new(PatternExtras::of_runtime_settings(settings.clone())));
        match self {
            Self::Array { entries, .. } => {
                for (_, pattern) in entries {
                    if cancellation_requested() || query_deadline_expired() {
                        return false;
                    }
                    pattern.attach_runtime_settings_shared(settings, &shared);
                }
            }
            Self::Object { entries, .. } => {
                for (_, pattern) in entries {
                    if cancellation_requested() || query_deadline_expired() {
                        return false;
                    }
                    pattern.attach_runtime_settings_shared(settings, &shared);
                }
            }
        }
        true
    }

    fn reify_value(value: &Value) -> Pattern {
        match value {
            Value::Pattern(pattern) => pattern.pattern().clone(),
            value => pure(value.clone()),
        }
    }

    pub fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::List(values) => Some(Self::Array {
                enumerable_len: values.len(),
                length: values.len(),
                entries: values
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(index, value)| (index, Self::reify_value(&value)))
                    .collect(),
            }),
            Value::Object(values) => Some(Self::Object {
                enumerable_len: values.len(),
                entries: values
                    .iter()
                    .map(|(key, value)| (value::SharedStr::from(key), Self::reify_value(value)))
                    .collect(),
            }),
            // Object.entries boxes strings and exposes one enumerable
            // character per index. Other non-null primitives have no keys.
            Value::Str(value) => {
                let entries: Vec<_> = value
                    .chars()
                    .enumerate()
                    .map(|(index, value)| {
                        (
                            value::SharedStr::from(index.to_string()),
                            pure(Value::Str(value.to_string().into())),
                        )
                    })
                    .collect();
                Some(Self::Object {
                    enumerable_len: entries.len(),
                    entries,
                })
            }
            Value::Bool(_)
            | Value::F64(_)
            | Value::Function(_)
            | Value::Pattern(_)
            | Value::Haps(_)
            | Value::JsValue(_) => Some(Self::Object {
                enumerable_len: 0,
                entries: Vec::new(),
            }),
            Value::Null | Value::Undefined => None,
        }
    }

    fn enumerable_len(&self) -> usize {
        match self {
            Self::Array { enumerable_len, .. } | Self::Object { enumerable_len, .. } => {
                *enumerable_len
            }
        }
    }

    fn patterns(&self) -> PickPatterns<'_> {
        match self {
            Self::Array { entries, .. } => PickPatterns::Array(entries.iter()),
            Self::Object { entries, .. } => PickPatterns::Object(entries.iter()),
        }
    }

    /// The pattern `value` picks, or the key it looked up and did not find,
    /// as the lookup read it. A list reads a name that is no number as NaN;
    /// that miss gives the name as written.
    fn select(&self, value: &Value, index_mode: PickIndexMode) -> Result<&Pattern, String> {
        // Property access coerces the actual JavaScript selector.
        // `pure(['1']).pick(['a', 'b'])` therefore selects `b`; treating an
        // opaque JS-owned array as an ordinary object turns it into NaN and
        // silently misses. Materialise at this semantic boundary, just as the
        // value-combining operators do.
        let materialized;
        let value = if matches!(value, Value::JsValue(_)) {
            materialized = materialize_js_value(value);
            &materialized
        } else {
            value
        };
        match self {
            Self::Array {
                enumerable_len,
                length,
                entries,
            } => {
                let rounded = crate::util::js_round(pick_js_number(value));
                let numeric = match index_mode {
                    PickIndexMode::Clamp if rounded.is_nan() => f64::NAN,
                    PickIndexMode::Clamp => rounded.max(0.0).min(length.saturating_sub(1) as f64),
                    PickIndexMode::Remainder => rounded % *enumerable_len as f64,
                    PickIndexMode::Modulo => {
                        let len = *enumerable_len as f64;
                        ((rounded % len) + len) % len
                    }
                };
                // JavaScript property access turns -0 into "0". Every other
                // negative, NaN, infinite or out-of-range number misses.
                // A name that is no number misses as NaN; the name as
                // written says more than NaN does.
                if numeric.is_nan() {
                    return Err(pick_property_key(value));
                }
                if !numeric.is_finite() || numeric < 0.0 || numeric > usize::MAX as f64 {
                    return Err(numeric.to_string());
                }
                let index = numeric as usize;
                entries
                    .iter()
                    .find_map(|(candidate, pattern)| (*candidate == index).then_some(pattern))
                    .ok_or_else(|| index.to_string())
            }
            Self::Object { entries, .. } => {
                if let Value::Str(key) = value {
                    return entries
                        .iter()
                        .find_map(|(candidate, pattern)| (candidate == key).then_some(pattern))
                        .ok_or_else(|| key.as_str().to_owned());
                }
                let key = pick_property_key(value);
                entries
                    .iter()
                    .find_map(|(candidate, pattern)| (candidate.as_str() == key).then_some(pattern))
                    .ok_or(key)
            }
        }
    }

    fn set_object(left: &Hap, right: &Hap, output: &Value) -> Option<Arc<Self>> {
        let Self::Object {
            enumerable_len,
            mut entries,
        } = Self::from_value(output)?
        else {
            return None;
        };
        // Begin with the complete visible output (including scalar promotion
        // to `{value: ...}`), then retain exact JavaScript shape metadata in
        // Object.assign order: left first, right wins.
        for source in [left, right] {
            let Some(Self::Object { entries: exact, .. }) = source
                .pick_lookup()
                .cloned()
                .or_else(|| Self::from_value(&source.value))
            else {
                continue;
            };
            for (key, pattern) in exact {
                if let Some((_, current)) = entries.iter_mut().find(|(name, _)| name == &key) {
                    *current = pattern;
                }
            }
        }
        scoped_pick_lookup(Self::Object {
            enumerable_len,
            entries,
        })
    }
}

/// Compact exact-sized lookup stored inside [`PickNode`]: dense `0..n` arrays
/// drop the index column and spare `Vec` capacity.
enum PickPatterns<'a> {
    Dense(std::slice::Iter<'a, Pattern>),
    Array(std::slice::Iter<'a, (usize, Pattern)>),
    Object(std::slice::Iter<'a, (value::SharedStr, Pattern)>),
}

impl<'a> Iterator for PickPatterns<'a> {
    type Item = &'a Pattern;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Dense(it) => it.next(),
            Self::Array(it) => it.next().map(|(_, p)| p),
            Self::Object(it) => it.next().map(|(_, p)| p),
        }
    }
}

enum PickPatternsMut<'a> {
    Dense(std::slice::IterMut<'a, Pattern>),
    Array(std::slice::IterMut<'a, (usize, Pattern)>),
    CompactObject(std::slice::IterMut<'a, (value::SharedStr, Pattern)>),
}

impl<'a> Iterator for PickPatternsMut<'a> {
    type Item = &'a mut Pattern;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Dense(it) => it.next(),
            Self::Array(it) => it.next().map(|(_, p)| p),
            Self::CompactObject(it) => it.next().map(|(_, p)| p),
        }
    }
}

#[derive(Clone)]
pub(crate) enum CompactPickLookup {
    Dense(Box<[Pattern]>),
    Array {
        enumerable_len: usize,
        length: usize,
        entries: Box<[(usize, Pattern)]>,
    },
    Object {
        enumerable_len: usize,
        entries: Box<[(crate::value::SharedStr, Pattern)]>,
    },
}

impl CompactPickLookup {
    fn from_lookup(lookup: PickLookup) -> Self {
        match lookup {
            PickLookup::Array {
                enumerable_len,
                length,
                entries,
            } => {
                let dense = enumerable_len == entries.len()
                    && length == entries.len()
                    && entries.iter().enumerate().all(|(i, (idx, _))| i == *idx);
                if dense {
                    Self::Dense(entries.into_iter().map(|(_, p)| p).collect())
                } else {
                    Self::Array {
                        enumerable_len,
                        length,
                        entries: entries.into_boxed_slice(),
                    }
                }
            }
            PickLookup::Object {
                enumerable_len,
                entries,
            } => Self::Object {
                enumerable_len,
                entries: entries.into_boxed_slice(),
            },
        }
    }

    fn try_map(&self, mut change: impl FnMut(&Pattern) -> Option<Pattern>) -> Option<Self> {
        Some(match self {
            Self::Dense(patterns) => Self::Dense(
                patterns
                    .iter()
                    .map(&mut change)
                    .collect::<Option<Vec<_>>>()?
                    .into_boxed_slice(),
            ),
            Self::Array {
                enumerable_len,
                length,
                entries,
            } => Self::Array {
                enumerable_len: *enumerable_len,
                length: *length,
                entries: entries
                    .iter()
                    .map(|(index, pattern)| Some((*index, change(pattern)?)))
                    .collect::<Option<Vec<_>>>()?
                    .into_boxed_slice(),
            },
            Self::Object {
                enumerable_len,
                entries,
            } => Self::Object {
                enumerable_len: *enumerable_len,
                entries: entries
                    .iter()
                    .map(|(key, pattern)| Some((key.clone(), change(pattern)?)))
                    .collect::<Option<Vec<_>>>()?
                    .into_boxed_slice(),
            },
        })
    }

    fn patterns(&self) -> PickPatterns<'_> {
        match self {
            Self::Dense(patterns) => PickPatterns::Dense(patterns.iter()),
            Self::Array { entries, .. } => PickPatterns::Array(entries.iter()),
            Self::Object { entries, .. } => PickPatterns::Object(entries.iter()),
        }
    }

    fn patterns_mut(&mut self) -> PickPatternsMut<'_> {
        match self {
            Self::Dense(patterns) => PickPatternsMut::Dense(patterns.iter_mut()),
            Self::Array { entries, .. } => PickPatternsMut::Array(entries.iter_mut()),
            Self::Object { entries, .. } => PickPatternsMut::CompactObject(entries.iter_mut()),
        }
    }

    fn drain_into(&mut self, out: &mut Vec<Pattern>) {
        match self {
            Self::Dense(patterns) => {
                out.extend(std::mem::take(patterns).into_vec());
            }
            Self::Array { entries, .. } => {
                out.extend(
                    std::mem::take(entries)
                        .into_vec()
                        .into_iter()
                        .map(|(_, p)| p),
                );
            }
            Self::Object { entries, .. } => {
                out.extend(
                    std::mem::take(entries)
                        .into_vec()
                        .into_iter()
                        .map(|(_, p)| p),
                );
            }
        }
    }

    fn select(&self, value: &Value, index_mode: PickIndexMode) -> Result<&Pattern, String> {
        let materialized;
        let value = if matches!(value, Value::JsValue(_)) {
            materialized = materialize_js_value(value);
            &materialized
        } else {
            value
        };
        match self {
            Self::Dense(patterns) => {
                let len = patterns.len();
                let index = pick_array_index(value, len, len, index_mode)?;
                patterns.get(index).ok_or_else(|| index.to_string())
            }
            Self::Array {
                enumerable_len,
                length,
                entries,
            } => {
                let index = pick_array_index(value, *enumerable_len, *length, index_mode)?;
                entries
                    .iter()
                    .find_map(|(candidate, pattern)| (*candidate == index).then_some(pattern))
                    .ok_or_else(|| index.to_string())
            }
            Self::Object { entries, .. } => {
                if let Value::Str(key) = value {
                    return entries
                        .iter()
                        .find_map(|(candidate, pattern)| {
                            (&**candidate == key.as_str()).then_some(pattern)
                        })
                        .ok_or_else(|| key.as_str().to_owned());
                }
                let key = pick_property_key(value);
                entries
                    .iter()
                    .find_map(|(candidate, pattern)| {
                        (&**candidate == key.as_str()).then_some(pattern)
                    })
                    .ok_or(key)
            }
        }
    }
}

#[inline]
fn pick_array_index(
    value: &Value,
    enumerable_len: usize,
    length: usize,
    index_mode: PickIndexMode,
) -> Result<usize, String> {
    let rounded = crate::util::js_round(pick_js_number(value));
    let numeric = match index_mode {
        PickIndexMode::Clamp if rounded.is_nan() => f64::NAN,
        PickIndexMode::Clamp => rounded.max(0.0).min(length.saturating_sub(1) as f64),
        PickIndexMode::Remainder => rounded % enumerable_len as f64,
        PickIndexMode::Modulo => {
            let len = enumerable_len as f64;
            ((rounded % len) + len) % len
        }
    };
    if numeric.is_nan() {
        return Err(pick_property_key(value));
    }
    if !numeric.is_finite() || numeric < 0.0 || numeric > usize::MAX as f64 {
        return Err(numeric.to_string());
    }
    Ok(numeric as usize)
}

#[derive(Clone, Copy)]
enum LookupRef<'a> {
    Full(&'a PickLookup),
    Compact(&'a CompactPickLookup),
}

impl<'a> LookupRef<'a> {
    #[inline]
    fn select(self, value: &Value, index_mode: PickIndexMode) -> Result<&'a Pattern, String> {
        match self {
            Self::Full(lookup) => lookup.select(value, index_mode),
            Self::Compact(lookup) => lookup.select(value, index_mode),
        }
    }
}

fn scoped_pick_lookup(mut lookup: PickLookup) -> Option<Arc<PickLookup>> {
    if let Some(settings) = settings::RuntimeSettings::current_requested()
        && !lookup.attach_runtime_settings(&settings)
    {
        return None;
    }
    Some(Arc::new(lookup))
}

/// True when the eager `set` lookup equals the lookup a pick derives from
/// the merged value.
///
/// `set_object` starts from the members of the merged value, then overwrites
/// each entry with the member of the left value, then of the right value.
/// The two lookups are equal when each overwriting member has the bits of
/// the merged member. `same_key` compares bits, so a zero of the other sign
/// keeps the eager path.
fn set_lookup_follows_value(value: &Value, left: &Hap, right: &Hap) -> bool {
    if left.pick_lookup().is_some()
        || right.pick_lookup().is_some()
        || value.has_js_function()
        // A string gives one lookup entry for each character.
        || matches!(left.value, Value::Str(_))
        || matches!(right.value, Value::Str(_))
    {
        return false;
    }
    let merged = |key: &str, member: &Value| {
        value
            .get(key)
            .is_some_and(|merged| same_key(merged, member))
    };
    let right_members = right.value.as_object();
    if !right_members.is_none_or(|members| members.iter().all(|(key, member)| merged(key, member)))
    {
        return false;
    }
    left.value.as_object().is_none_or(|members| {
        members.iter().all(|(key, member)| {
            right_members.is_some_and(|right| right.contains_key(key)) || merged(key, member)
        })
    })
}

fn applied_pick_lookup(
    flow: LookupFlow,
    value: &Value,
    left: &Hap,
    right: &Hap,
) -> Option<Arc<PickLookup>> {
    match flow {
        LookupFlow::Left => {
            let lookup = left.pick_lookup_arc()?;
            (value == &left.value).then_some(lookup)
        }
        LookupFlow::Right => {
            let lookup = right.pick_lookup_arc()?;
            (value == &right.value).then_some(lookup)
        }
        LookupFlow::Set => {
            // A pick derives this lookup from the value. Plain data needs no
            // eager copy.
            if set_lookup_follows_value(value, left, right) {
                return None;
            }
            if value == &right.value {
                right.pick_lookup_arc()
            } else {
                PickLookup::set_object(left, right, value).or_else(|| {
                    (value == &left.value)
                        .then(|| left.pick_lookup_arc())
                        .flatten()
                })
            }
        }
        LookupFlow::Infer | LookupFlow::Add | LookupFlow::LeftControls => {
            if left.pick_lookup().is_none() && right.pick_lookup().is_none() {
                return None;
            }
            if value == &left.value {
                left.pick_lookup_arc()
            } else if value == &right.value {
                right.pick_lookup_arc()
            } else {
                None
            }
        }
    }
}

/// Provenance follows semantic ownership, never equality of sampled numbers.
fn applied_live_controls(flow: LookupFlow, left: &Hap, right: &Hap) -> ([u64; 4], u64) {
    if left.keys.is_empty() && right.keys.is_empty() {
        return ([0; 4], 0);
    }
    match flow {
        LookupFlow::Left => (left.live_controls(), left.slider_binding()),
        LookupFlow::LeftControls => (left.live_controls(), 0),
        LookupFlow::Right => (right.live_controls(), right.slider_binding()),
        LookupFlow::Set => {
            // Opaque objects can run score code when materialized. Refuse the
            // optional binding rather than inspecting them a second time.
            if matches!(left.value, Value::JsValue(_)) || matches!(right.value, Value::JsValue(_)) {
                return ([0; 4], 0);
            }
            let right_live = right.live_controls();
            if !matches!(
                (&left.value, &right.value),
                (Value::Object(_), _) | (_, Value::Object(_))
            ) {
                return (right_live, right.slider_binding());
            }
            let mut bindings = left.live_controls();
            if let Value::Object(values) = &right.value {
                for (key, _) in values {
                    if let Some(slot) = controls::live_slot(key.as_str()) {
                        bindings[slot] = right_live[slot];
                    }
                }
            }
            (bindings, 0)
        }
        LookupFlow::Infer | LookupFlow::Add => ([0; 4], 0),
    }
}

/// The hap an `app*` node makes from a function hap `hf` and a value hap `hv`
/// that meet in `part`. The context lists the value side's locations first.
/// For every context tag, the function side wins.
fn applied_hap(
    hf: &Hap,
    hv: &Hap,
    whole: Option<TimeSpan>,
    part: TimeSpan,
    apply: &Combine,
    flow: LookupFlow,
) -> Hap {
    let mut context = hv.context.clone();
    context.append(&hf.context);
    let met = Meeting::of(hf, hv, apply, flow);
    let keys = ContextKeys::merge_with(
        &hf.keys,
        &hv.keys,
        met.live_controls,
        met.slider_binding,
        met.pick_lookup,
    );
    Hap::from_parts(whole, part, met.value, context, keys)
}

/// What a function hap and a value hap decide together when they meet: the
/// value, its lookup shape and its live bindings. The rest of a met hap is
/// the two haps' metadata merged, as in [`applied_hap`].
pub(crate) struct Meeting {
    pub(crate) value: Value,
    pub(crate) pick_lookup: Option<Arc<PickLookup>>,
    pub(crate) live_controls: [u64; 4],
    pub(crate) slider_binding: u64,
}

impl Meeting {
    pub(crate) fn of(hf: &Hap, hv: &Hap, apply: &Combine, flow: LookupFlow) -> Self {
        let value = apply.apply(&hf.value, &hv.value);
        let pick_lookup = if matches!(apply, Combine::SetMerged)
            && flow == LookupFlow::Set
            && hf.value.has_js_function()
        {
            PickLookup::set_object(hf, hv, &value)
        } else {
            applied_pick_lookup(flow, &value, hf, hv)
        };
        let (live_controls, slider_binding) = applied_live_controls(flow, hf, hv);
        Self {
            value,
            pick_lookup,
            live_controls,
            slider_binding,
        }
    }

    /// [`applied_hap`] of `hf` and `hv` at `hf`'s whole and at `part`, made
    /// where `hf` is, so that the meeting copies none of `hf`. Only for a
    /// known compose op of two data values, where neither hap carries a
    /// pick lookup and the meeting makes none. `false`, with `hf`
    /// unchanged, otherwise.
    ///
    /// A run of sets is made as one `set` of `hv`, the union of its
    /// controls. A `set` of maps of controls is a union that keeps the keys
    /// in order and takes the later value of a repeated key, so the union
    /// of the controls first, then with the hap, is the same map.
    /// [`Self::in_place`] against a plain [`RunMeeting`] constant that carries
    /// no bindings, keys, visuals, or pick lookup.
    fn in_place_run_meeting(
        hf: &mut Hap,
        meeting: &RunMeeting,
        part: TimeSpan,
        cache: Option<&mut value::SharedWaveCache>,
    ) -> bool {
        let single_key = meeting.apply.single_key();
        if single_key != 0 {
            if hf.pick_lookup().is_some()
                || !matches!(hf.value, Value::Object(_))
                || !hf.value.is_data(0)
            {
                return false;
            }
            let Some(key) = controls::builtin_key_ref_by_id(single_key & 0x03ff) else {
                return false;
            };
            if !hf.keys.is_empty() {
                let hf_live = hf.live_controls();
                let hf_binding = hf.slider_binding();
                if hf_live != [0; 4] || hf_binding != 0 {
                    let mut bindings = hf_live;
                    if let Some(slot) = controls::live_slot(key.as_str()) {
                        bindings[slot] = 0;
                    }
                    hf.set_live_bindings(bindings, 0);
                }
            }
            if let Value::Object(map) = &mut hf.value {
                map.set_data_single_cached(key, &meeting.value, cache);
            }
            hf.set_part(part);
            meeting.prepend_context_to(&mut hf.context);
            return true;
        }
        let Some(op) = meeting.in_place_op() else {
            return false;
        };
        let flow = meeting.flow();
        if hf.pick_lookup().is_some()
            || !meeting.values_meet_in_place(&hf.value, &meeting.value, flow)
        {
            return false;
        }
        if !hf.keys.is_empty() {
            let hf_live = hf.live_controls();
            let hf_binding = hf.slider_binding();
            if hf_live != [0; 4] || hf_binding != 0 {
                let (live_controls, slider_binding) = match flow {
                    LookupFlow::Left => (hf_live, hf_binding),
                    LookupFlow::LeftControls => (hf_live, 0),
                    LookupFlow::Right | LookupFlow::Infer | LookupFlow::Add => ([0; 4], 0),
                    LookupFlow::Set => {
                        if hf_live == [0; 4]
                            || !matches!(
                                (&hf.value, &meeting.value),
                                (Value::Object(_), _) | (_, Value::Object(_))
                            )
                        {
                            ([0; 4], 0)
                        } else {
                            let mut bindings = hf_live;
                            if let Value::Object(values) = &meeting.value {
                                for (key, _) in values {
                                    if let Some(slot) = controls::live_slot(key.as_str()) {
                                        bindings[slot] = 0;
                                    }
                                }
                            }
                            (bindings, 0)
                        }
                    }
                };
                hf.set_live_bindings(live_controls, slider_binding);
            }
        }
        compose::compose_data_in_place_cached(op, &mut hf.value, &meeting.value, cache);
        hf.set_part(part);
        meeting.prepend_context_to(&mut hf.context);
        true
    }

    pub(crate) fn in_place(
        hf: &mut Hap,
        hv: &Hap,
        part: TimeSpan,
        apply: &Combine,
        flow: LookupFlow,
    ) -> bool {
        let Some(op) = apply.in_place_op() else {
            return false;
        };
        if !Self::meets_in_place(hf, hv, apply, flow) {
            return false;
        }
        let has_keys = !hf.keys.is_empty() || !hv.keys.is_empty();
        let (live_controls, slider_binding) = if has_keys {
            applied_live_controls(flow, hf, hv)
        } else {
            ([0; 4], 0)
        };
        compose::compose_data_in_place(op, &mut hf.value, &hv.value);
        hf.set_part(part);
        // The value side's locations come first.
        if !hv.context.is_empty() {
            let mut context = hv.context.clone();
            context.append(&hf.context);
            hf.context = context;
        }
        if has_keys {
            hf.keys =
                ContextKeys::merge_with(&hf.keys, &hv.keys, live_controls, slider_binding, None);
        }
        true
    }

    /// [`Self::in_place`] of `hf` and the hap that a control of one name
    /// makes of `hv`, which holds the value of `hv` under `key`: the
    /// meeting sets the value where `hf` is, with no map of the control.
    /// `false`, with `hf` unchanged, for any other meeting: a value that
    /// the control names in another way, a function hap that holds no map
    /// of data, another op.
    pub(crate) fn set_in_place(
        hf: &mut Hap,
        key: &Key,
        hv: &Hap,
        part: TimeSpan,
        apply: &Combine,
        flow: LookupFlow,
    ) -> bool {
        // The control names a number that no slider drives `{key: number}`.
        let Value::F64(number) = hv.value else {
            return false;
        };
        if !hv.keys.is_empty() {
            let k = hv.keys.get();
            if k.slider_binding != 0 || k.live_controls != [0; 4] || k.pick_lookup.is_some() {
                return false;
            }
        }
        if !Self::set_number_in_place(hf, key, number, part, apply, flow) {
            return false;
        }
        // The value side's locations come first.
        if !hv.context.is_empty() {
            let mut context = hv.context.clone();
            context.append(&hf.context);
            hf.context = context;
        }
        if !hv.keys.is_empty() {
            hf.keys = ContextKeys::merge(&hf.keys, &hv.keys);
        }
        true
    }

    /// [`Self::set_in_place`] of `hf` and a hap that holds `number` and
    /// nothing more: no place in the text, no key, no visual and no slider,
    /// as the hap of a [wave](table::Wave) is.
    pub(crate) fn set_number_in_place(
        hf: &mut Hap,
        key: &Key,
        number: f64,
        part: TimeSpan,
        apply: &Combine,
        flow: LookupFlow,
    ) -> bool {
        Self::set_number_in_place_cached(hf, key, number, part, apply, flow, None)
    }

    pub(crate) fn set_number_in_place_cached(
        hf: &mut Hap,
        key: &Key,
        number: f64,
        part: TimeSpan,
        apply: &Combine,
        flow: LookupFlow,
        cache: Option<&mut value::SharedWaveCache>,
    ) -> bool {
        let set =
            matches!(apply, Combine::Composed(compose::ComposeOp::Set)) && flow == LookupFlow::Set;
        if !set || hf.pick_lookup().is_some() || !hf.value.is_data(0) {
            return false;
        }
        let Value::Object(controls) = &mut hf.value else {
            return false;
        };
        controls.set_data_single_cached(key, &Value::F64(number), cache);
        // The bindings of a set of the map: the control that the signal
        // sets has no slider, and the hap has none of its own.
        if !hf.keys.is_empty() {
            let k = hf.keys.get();
            let slot = controls::live_slot(key.as_str());
            if k.slider_binding != 0 || slot.is_some_and(|s| k.live_controls[s] != 0) {
                let edit = hf.keys.edit();
                if let Some(slot) = slot {
                    edit.live_controls[slot] = 0;
                }
                edit.slider_binding = 0;
                hf.keys.trim();
            }
        }
        hf.set_part(part);
        true
    }

    /// Whether [`Self::in_place`] makes the meeting of `hf` and `hv`.
    pub(crate) fn meets_in_place(hf: &Hap, hv: &Hap, apply: &Combine, flow: LookupFlow) -> bool {
        hf.pick_lookup().is_none()
            && hv.pick_lookup().is_none()
            && Self::values_meet_in_place(&hf.value, &hv.value, apply, flow)
    }

    /// Whether [`Self::in_place`] makes the meeting of two haps that hold
    /// the values `function` and `value` and no pick lookup.
    pub(crate) fn values_meet_in_place(
        function: &Value,
        value: &Value,
        apply: &Combine,
        flow: LookupFlow,
    ) -> bool {
        let Some(op) = apply.in_place_op() else {
            return false;
        };
        if !function.is_data(0) || !value.is_data(0) {
            return false;
        }
        // A set meeting makes a lookup of a string side, or of a union that
        // keeps the keys of `value` out. See `set_lookup_follows_value`.
        flow != LookupFlow::Set
            || (op == compose::ComposeOp::Set
                && !matches!(function, Value::Str(_))
                && !matches!(value, Value::Str(_))
                && !value.as_object().is_some_and(compose::union_drops))
    }
}

pub(crate) fn pick_js_number(value: &Value) -> f64 {
    match value {
        Value::Undefined => f64::NAN,
        Value::Null => 0.0,
        Value::Bool(value) => f64::from(*value),
        Value::F64(value) => *value,
        Value::Str(value) => pick_js_string_number(value),
        Value::List(values) => pick_js_string_number(&pick_array_string(values)),
        Value::Object(_)
        | Value::Function(_)
        | Value::Pattern(_)
        | Value::Haps(_)
        | Value::JsValue(_) => f64::NAN,
    }
}

pub(crate) fn pick_js_string_number(value: &str) -> f64 {
    // ECMAScript WhiteSpace includes U+FEFF; Rust's `str::trim` does not.
    let value = value.trim_matches(|ch: char| ch.is_whitespace() || ch == '\u{feff}');
    if value.is_empty() {
        return 0.0;
    }
    match value {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    if value.eq_ignore_ascii_case("inf") || value.eq_ignore_ascii_case("infinity") {
        return f64::NAN;
    }
    for (prefix, radix) in [
        ("0x", 16),
        ("0X", 16),
        ("0b", 2),
        ("0B", 2),
        ("0o", 8),
        ("0O", 8),
    ] {
        if let Some(digits) = value.strip_prefix(prefix) {
            if digits.is_empty() {
                return f64::NAN;
            }
            let mut out = 0.0;
            for digit in digits.chars() {
                let Some(digit) = digit.to_digit(radix) else {
                    return f64::NAN;
                };
                out = out * f64::from(radix) + f64::from(digit);
            }
            return out;
        }
    }
    value.parse().unwrap_or(f64::NAN)
}

pub(crate) fn pick_array_string(values: &[Value]) -> String {
    values
        .iter()
        .map(|value| match value {
            Value::Undefined | Value::Null => String::new(),
            Value::List(values) => pick_array_string(values),
            Value::Object(_) => "[object Object]".into(),
            Value::Function(function) => format!("{function:?}"),
            Value::Pattern(_) => "[object Object]".into(),
            Value::F64(value) => pick_js_number_string(*value),
            other => other.show(),
        })
        .collect::<Vec<_>>()
        .join(",")
}

pub(crate) fn pick_js_number_string(value: f64) -> String {
    if !value.is_finite() || value == 0.0 {
        return Value::F64(value).show();
    }
    let absolute = value.abs();
    if !(1e-6..1e21).contains(&absolute) {
        let scientific = format!("{:e}", value);
        let (mantissa, exponent) = scientific
            .split_once('e')
            .expect("Rust scientific formatting always has an exponent");
        let exponent: i32 = exponent
            .parse()
            .expect("Rust scientific exponent is an integer");
        return format!("{mantissa}e{exponent:+}");
    }
    value.to_string()
}

fn pick_property_key(value: &Value) -> String {
    match value {
        Value::Undefined => "undefined".into(),
        Value::Null => "null".into(),
        Value::Bool(value) => value.to_string(),
        Value::F64(value) => pick_js_number_string(*value),
        Value::Str(value) => value.to_string(),
        Value::List(values) => values
            .iter()
            .map(|value| match value {
                Value::Undefined | Value::Null => String::new(),
                Value::List(_) => pick_property_key(value),
                Value::Object(_) => "[object Object]".into(),
                other => other.show(),
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
        Value::Function(function) => format!("{function:?}"),
        Value::Haps(haps) => vec!["[object Object]"; haps.as_slice().len()].join(","),
        Value::Pattern(_) | Value::JsValue(_) => "[object Object]".into(),
    }
}

/// Update `inner` in place for a pick join with `outer` at `whole` and `part`.
#[inline]
fn pick_where_it_is(outer: &Hap, inner: &mut Hap, whole: Option<TimeSpan>, part: TimeSpan) {
    inner.set_spans(whole, part);
    if !outer.keys.is_empty() {
        inner.keys = ContextKeys::merge(&inner.keys, &outer.keys);
    }
    if !outer.context.is_empty() {
        inner.context.prepend(&outer.context);
    }
}

/// `_pick(...).{inner,outer,squeeze,reset,restart}Join()` out of line, so the
/// join's nested locals do not enlarge every recursive `query_node` frame.
/// The haps come after those of `out`. A pick that fails writes none.
/// `before` runs before each hap of the selector picks its pattern.
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn query_pick(
    thread: &QueryThread,
    selector: &Pattern,
    lookup: LookupRef<'_>,
    index_mode: PickIndexMode,
    mode: JoinMode,
    state: &State,
    out: &mut Vec<Hap>,
    before: &dyn Fn(),
) {
    let from = out.len();
    let picked = with_list(|selectors| {
        selector.answer_child_onto(thread, state, selectors);
        if thread.error_pending() {
            return None;
        }
        for outer in selectors.drain(..) {
            before();
            pick_of(thread, &outer, lookup, index_mode, mode, state, from, out)?;
            if thread.error_pending() {
                return None;
            }
        }
        Some(())
    });
    if picked.is_none() {
        out.truncate(from);
    }
}

/// What a pick makes of the selector hap `outer`, after the haps of `out`,
/// which held `from` haps of other nodes before the pick wrote. `None`
/// when the pick fails or stops.
#[allow(clippy::too_many_arguments)]
fn pick_of(
    thread: &QueryThread,
    outer: &Hap,
    lookup: LookupRef<'_>,
    index_mode: PickIndexMode,
    mode: JoinMode,
    state: &State,
    from: usize,
    out: &mut Vec<Hap>,
) -> Option<()> {
    if matches!(
        mode,
        JoinMode::Squeeze | JoinMode::Reset | JoinMode::Restart
    ) && outer.whole().is_none()
    {
        // All three special joins begin with `discreteOnly()`: analog
        // carriers are removed before their selected value is dereferenced.
        return Some(());
    }
    let inner_pattern = match lookup.select(&outer.value, index_mode) {
        Ok(pattern) => pattern,
        Err(key) => {
            // `lookup[key]` is `undefined`; the selected value becomes a hap
            // normally and fails only when the join reaches it.
            signal_query_error(|| format!("no {key:?} in the pick lookup"));
            return None;
        }
    };
    // The haps of the picked pattern, from here on.
    let picked = out.len();
    match mode {
        JoinMode::Inner | JoinMode::Outer | JoinMode::Mix => {
            inner_pattern.answer_child_onto(thread, &state.set_span(outer.part()), out);
            for inner in &mut out[picked..] {
                let whole = match mode {
                    JoinMode::Inner => inner.whole(),
                    JoinMode::Outer => outer.whole(),
                    JoinMode::Mix => match (outer.whole(), inner.whole()) {
                        (Some(a), Some(b)) => match a.intersection(&b) {
                            Some(span) => Some(span),
                            None => {
                                signal_query_error(|| "TimeSpans do not intersect".into());
                                return None;
                            }
                        },
                        _ => None,
                    },
                    _ => unreachable!(),
                };
                let part = inner.part();
                pick_where_it_is(outer, inner, whole, part);
            }
        }
        JoinMode::Squeeze => {
            let Some(outer_whole) = outer.whole() else {
                return Some(());
            };
            inner_pattern.query_focused_onto(
                thread,
                outer.whole_or_part(),
                &state.set_span(outer.part()),
                out,
            );
            let mut loc_cache = locations::LocMergeCache::new();
            retain_from(out, picked, |inner| {
                let whole = match inner.whole() {
                    Some(inner_whole) => match inner_whole.intersection(&outer_whole) {
                        Some(whole) => Some(whole),
                        None => return false,
                    },
                    None => None,
                };
                let Some(part) = inner.part().intersection(&outer.part()) else {
                    return false;
                };
                inner.set_spans(whole, part);
                if !outer.keys.is_empty() {
                    inner.keys = ContextKeys::merge_with(
                        &outer.keys,
                        &inner.keys,
                        inner.live_controls(),
                        inner.slider_binding(),
                        inner.pick_lookup_arc(),
                    );
                }
                if !outer.context.is_empty() {
                    loc_cache.append_into(&mut inner.context, &outer.context);
                }
                true
            });
        }
        JoinMode::Reset | JoinMode::Restart => {
            let Some(outer_whole) = outer.whole() else {
                return Some(());
            };
            let shift = if mode == JoinMode::Restart {
                outer_whole.begin
            } else {
                outer_whole.begin.cycle_pos()
            };
            let outer_has_keys = !outer.keys.is_empty();
            let outer_has_ctx = !outer.context.is_empty();
            let outer_whole_int = (outer_whole.begin.denom() == 1 && outer_whole.end.denom() == 1)
                .then(|| (outer_whole.begin.numer(), outer_whole.end.numer()));
            let mut loc_cache = locations::LocMergeCache::new();
            let mut meet = |inner: &mut Hap, part| {
                let whole = inner.whole().and_then(|inner_whole| {
                    if let Some((ob, oe)) = outer_whole_int
                        && inner_whole.begin < inner_whole.end
                        && inner_whole.begin.floor_int() >= ob
                        && if inner_whole.end.denom() == 1 {
                            inner_whole.end.numer() <= oe
                        } else {
                            inner_whole.end.floor_int() < oe
                        }
                    {
                        return Some(inner_whole);
                    }
                    inner_whole.intersection(&outer_whole)
                });
                inner.set_spans(whole, part);
                if outer_has_keys {
                    inner.keys = ContextKeys::merge(&inner.keys, &outer.keys);
                }
                if outer_has_ctx {
                    loc_cache.prepend_into(&outer.context, &mut inner.context);
                }
            };
            if let Node::Sequence(layers) = inner_pattern.node_kind()
                && inner_pattern.is_discrete()
            {
                // Each sequence step already lies inside the part it answers.
                let asked = state.set_span(outer.part());
                if shift == Fraction::ZERO || sequence::repeats_at(layers, shift, outer.part()) {
                    inner_pattern.answer_child_onto(thread, &asked, out);
                } else {
                    inner_pattern.query_moved_onto(thread, shift, "late", &asked, out);
                }
                for inner in &mut out[picked..] {
                    meet(inner, inner.part());
                }
            } else {
                inner_pattern.query_restarted_onto(thread, shift, state, outer.part(), out);
                retain_from(out, picked, |inner| {
                    let part = if inner.part() == outer.part() {
                        if inner.part().begin <= inner.part().end {
                            inner.part()
                        } else {
                            return false;
                        }
                    } else {
                        let Some(part) = inner.part().intersection(&outer.part()) else {
                            return false;
                        };
                        part
                    };
                    meet(inner, part);
                    true
                });
            }
        }
    }
    // The pick holds the haps of each selector hap so far.
    let held = out.len() - from;
    if !thread.charge(held) {
        return None;
    }
    thread.note_accumulated(held);
    Some(())
}

#[inline(never)]
fn query_patternified_pick(
    selector: &Pattern,
    lookup_pattern: &Pattern,
    index_mode: PickIndexMode,
    mode: JoinMode,
    state: &State,
) -> Vec<Hap> {
    // The general registered path: pick from each lookup-carrier hap, then
    // inner-join. Query the complete lookup carrier pattern first, then join
    // each selected result in carrier order so query errors and callback side
    // effects keep their phase.
    let mut lookups = Vec::new();
    for hap in lookup_pattern.query(state) {
        let lookup = hap
            .pick_lookup_arc()
            .or_else(|| match hap.value {
                Value::JsValue(reference) => {
                    host_call_pick_lookup(reference.id()).and_then(scoped_pick_lookup)
                }
                _ => None,
            })
            .or_else(|| PickLookup::from_value(&hap.value).and_then(scoped_pick_lookup));
        let Some(lookup) = lookup else {
            if query_stop_requested() {
                return Vec::new();
            }
            signal_query_error(|| "Cannot convert undefined or null to object".into());
            return Vec::new();
        };
        // `lookup_pattern.query` already enforces the per-vector hap budget
        // before returning. This second carrier vector therefore cannot be
        // longer than the budget; charging the same length again would be a
        // dead check, not an independent safety boundary.
        lookups.push(PickLookupHap { hap, lookup });
    }
    if query_error_pending() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for lookup_hap in lookups {
        let mut selected = Vec::new();
        if lookup_hap.lookup.enumerable_len() != 0 {
            QUERY_THREAD.with(|thread| {
                query_pick(
                    thread,
                    selector,
                    LookupRef::Full(lookup_hap.lookup.as_ref()),
                    index_mode,
                    mode,
                    &state.set_span(lookup_hap.hap.part()),
                    &mut selected,
                    &|| {},
                );
            });
        }
        if query_error_pending() {
            return Vec::new();
        }
        for inner in selected {
            let mut context = lookup_hap.hap.context.clone();
            context.append(&inner.context);
            if !push_budgeted(
                &mut out,
                Hap::from_parts(
                    inner.whole(),
                    inner.part(),
                    inner.value,
                    context,
                    ContextKeys::merge(&inner.keys, &lookup_hap.hap.keys),
                ),
            ) {
                return Vec::new();
            }
        }
    }
    out
}

/// One outer hap of a pattern-of-patterns, with the pattern it carries.
///
/// `inner` is `None` where the hap value is not a pattern, as in
/// `stepBind(x => 42)`. Hap values are untyped, so the value is stored and
/// fails only when a join dereferences it. Treating it as "no hap" would
/// erase the distinction between `stepBind(x => 42)`, which throws during
/// construction, and `stepBind(x => { throw })`, which constructs and
/// empties the query.
struct ResolvedHap {
    hap: Hap,
    inner: Option<Pattern>,
}

impl ResolvedHap {
    fn of(hap: Hap, inner: Pattern) -> Self {
        Self {
            hap,
            inner: Some(inner),
        }
    }

    /// `fmap` replaces the hap's value with what the callback returned, so a
    /// non-pattern return is stored in the hap. The join words its error
    /// from that value, not from the one the callback was given.
    fn from_bind(hap: Hap, result: BindResult) -> Self {
        match result {
            BindResult::Pattern(inner) => Self {
                hap: hap.with_value(|_| Value::Undefined),
                inner: Some(inner),
            },
            BindResult::Value(value) => Self {
                hap: hap.with_value(|_| value.clone()),
                inner: None,
            },
        }
    }

    /// `Err` carries the value of the first hap whose value is not a
    /// pattern. Every join then fails as a whole (the `TypeError` unwinds
    /// past the remaining haps), and the value is needed because the message
    /// V8 produces depends on it.
    fn all_patterns(resolved: Vec<Self>) -> Result<Vec<(Hap, Pattern)>, Value> {
        let mut out = Vec::with_capacity(resolved.len());
        for r in resolved {
            match r.inner {
                Some(inner) => out.push((r.hap, inner)),
                None => return Err(r.hap.value),
            }
        }
        Ok(out)
    }
}

/// Asserted against the production types.
///
/// L1 must stay ownable by the scheduler, which means the actual private
/// [`Node`] and the public [`Pattern`] must be `Send + Sync`. That is not a
/// style preference: it is what makes "no QuickJS value can be stored in a
/// pattern node" a compile-time fact rather than a convention.
///
/// The connection to QuickJS is exact. `rquickjs::Function<'js>` contains
/// `Ctx<'js>` → `NonNull<JSContext>`, and `Persistent<Function>` contains
/// `*mut JSRuntime`; both are `!Send + !Sync`. A `Node` variant holding
/// either - directly or transitively - stops being `Send + Sync`, and this
/// assertion stops compiling. `rustel-core` deliberately has no dependency
/// on rquickjs.
///
/// Callbacks are held as an opaque [`CallbackId`], an integer index resolved by
/// L2 through a thread-local host. That indirection is the whole design, and
/// this is where it is enforced.
///
/// The integration test verifies this assertion by compiling a mutation with a
/// `!Send + !Sync` payload in the real `Node`.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Node>();
    assert_send_sync::<Pattern>();
    assert_send_sync::<std::sync::Arc<Node>>();
    assert_send_sync::<std::sync::Arc<NodeCell>>();
    assert_send_sync::<Value>();
    assert_send_sync::<Hap>();
};

pub(crate) enum Node {
    Silence,
    Pure(Value),
    /// A `pure(value)` carrying source locations directly in one node, avoiding
    /// the extra `Node::Pure` + `Arc<[(usize, usize)]>` + `Node::AddContext`
    /// wrapper allocations for located mini-notation constant atoms.
    PureAt(Box<(Value, Locations)>),
    /// Equal-stepped integer range over one cycle (`0 .. 63`), keeping `Node`
    /// 24 bytes with zero heap allocation beyond the `NodeCell`.
    IntRange {
        start: i32,
        count: u32,
        loc0: u32,
        loc1: u32,
        named_id: u16,
        step_neg: bool,
    },
    /// Cycle-rotating `iter(times)` / `iterBack(times)` without duplicating
    /// `parts` rotated copies of `inner`.
    Iter(Box<IterNode>),
    /// Built-in stateless scalar signal (`rand`, `sine`, `saw`, ...), stored
    /// directly as a bare function pointer without closure allocation or mutex.
    NumberSignal(NumberSignal),
    /// Uniform integer-rate sampling of a built-in scalar signal
    /// (`rand.segment(128)`, `sine.segment(16)`), keeping `Node` 24 bytes with
    /// zero wrapper allocations.
    SegmentSignal {
        body: NumberSignal,
        steps: u32,
    },
    /// The steps of every cycle, as one node. See [`sequence`].
    Sequence(Arc<[sequence::Layer]>),
    /// Mutation control - never built.
    ///
    /// Stands in for a QuickJS value stored in L1: `Rc` is `!Send + !Sync`
    /// for the same reason `rquickjs::Function` is, so its presence must break
    /// the `Send + Sync` assertion above. If that assertion stops having an
    /// effect, this variant compiles.
    #[cfg(non_send_node_mutation)]
    NonSendMutation(std::rc::Rc<()>),
    /// A pattern that always fails at query time.
    ///
    /// Several `TypeError`s are reachable only once the query runs. One is
    /// `pat.polyBind(42)`, where `fmap(42)` builds and then calls a
    /// non-function. This node keeps the failure in the query phase instead
    /// of throwing at the host boundary: an expression the language can
    /// build must build here too, and it must empty the query rather than
    /// abort the evaluation.
    QueryError(Arc<str>),
    /// A resource refusal whose construction is valid but whose query cannot
    /// be answered within the native runtime's bound. The refusal is emitted
    /// here, inside the top-level query boundary; setting the thread-local at
    /// construction time would be cleared as stale before the query begins.
    QueryLimit(Box<QueryLimit>),
    /// Query-time transform of the query span, then of the resulting hap spans.
    /// `fast`/`slow` are expressed with these two.
    WithQueryTime(Box<(Pattern, TimeFn)>),
    WithHapTime(Box<(Pattern, TimeFn)>),
    /// `fast` by an exact factor: query times are multiplied by it and hap
    /// times divided by it. See [`checked_time`] for overflow.
    Fast(Box<FastNode>),
    /// Inline small-fraction `fast`/`slow` over a plain handle (`slow` when
    /// `bool` is true), avoiding an 80-byte `Box<FastNode>` while keeping
    /// [`Node`] 24 bytes.
    FastSmall(Arc<NodeCell>, u8, i16, u16, bool),
    /// `late` by an exact offset (`early` is a negative one): subtracted from
    /// query times and added to hap times. See [`checked_time`] for overflow.
    Shift(Box<ShiftNode>),
    /// Inline small-fraction `late`/`early` over a plain handle (`early` when
    /// `bool` is true), avoiding an 80-byte `Box<ShiftNode>` while keeping
    /// [`Node`] 24 bytes.
    ShiftSmall(Arc<NodeCell>, u8, i16, u16, bool),
    /// `withQuerySpan` / `withHapSpan` - begin and end mapped jointly.
    WithQuerySpan(Box<(Pattern, SpanFn)>),
    WithQuerySpanJs(Box<(Pattern, CallbackId)>),
    WithHapSpan(Box<(Pattern, SpanFn)>),
    /// `rev` - reflect each cycle. Its own node because the reflection pivot
    /// comes from the query span's cycle, so the hap transform is not a
    /// function of the hap alone.
    Rev(Box<Pattern>),
    /// `stepJoin` - re-slices the outer cycle on every query, so it cannot be
    /// expressed as a `Join` mode.
    StepJoin(Box<Pattern>),
    /// `polyJoin` - extends each inner pattern to the outer's step count, then
    /// joins taking the outer whole. Resolved at query time so it survives
    /// wrapping.
    PolyJoin(Box<Pattern>),
    /// `arp(indices, pat)` - index into each chord.
    ///
    /// Its own node because the collect/select/unwrap pipeline's intermediate
    /// values are a `Hap[]` and then a `Hap`, neither of which `Value` holds.
    /// Doing the grouping, the index selection and the final unwrap in one
    /// place means the haps never have to be representable as values at all.
    Arp(Box<PatternPair>),
    /// Native semantics supplied by a statically linked extension.
    ///
    /// Core retains only the execution contract. Public names, authored
    /// behavior and any specialized query work remain in the owning crate.
    Extension(Box<dyn extension_node::ExtensionPatternNode>),
    /// `arpWith(func, pat)` - hand each congruent chord to a JavaScript
    /// callback, reify its result, join it over the chord span, then unwrap the
    /// returned Hap-shaped value.
    ///
    /// The callback id is opaque to L1. `Hap[]` and the callback's JavaScript
    /// values exist only at the L2 boundary; the returned pattern is native.
    ArpWith(Box<(Pattern, CallbackId)>),
    /// The general `register()` path for `arpWith`: the callback itself is a
    /// pattern (for example `.arpWith(f, g)` sequences two functions). Each
    /// callback-valued hap selects one ordinary [`Node::ArpWith`] over its
    /// part, then inner-joins that result.
    ArpWithPattern(Box<PatternPair>),
    /// `pure(pattern)` - a pattern whose every hap value is a known inner
    /// pattern.
    ///
    /// The top-level `pure(pattern)` form keeps a dedicated node so joins can
    /// resolve it without confusing it with ordinary nested container values.
    /// Its purity is inherited exactly: a pure inner stays pure and queryable
    /// with no host, and an impure one keeps its precise reachable set.
    /// Routing it through `PatternOfDynamic` would make it falsely opaque.
    PurePattern(Box<Pattern>),
    /// A pure JavaScript array/object whose exact lookup shape travels beside
    /// its ordinary value until pick consumes it.
    PurePickLookup(Box<(Value, PickLookup)>),
    /// `polyBind`/`stepBind` - the inner pattern comes from a user JavaScript
    /// callback applied to each outer hap value.
    ///
    /// Holds only the opaque id. Always impure and opaque: the callback may
    /// return a graph whose own callbacks cannot be enumerated statically, so
    /// `gc_mark` must retain conservatively.
    PatternOfJs(Box<(Pattern, CallbackId)>),
    /// Native value transform.
    FmapNative(Box<(Pattern, ValueFn)>),
    FilterValues(Box<(Pattern, ValuePredicate)>),
    FilterValuesJs(Box<(Pattern, CallbackId)>),
    /// `filterHaps` - the predicate sees the whole hap. `discreteOnly` and
    /// `onsetsOnly` are the instances the joins need.
    FilterHaps(Box<(Pattern, HapPredicate)>),
    FilterHapsJs(Box<(Pattern, CallbackId)>),
    FilterWhenJs(Box<(Pattern, CallbackId)>),
    SortHapsByPart(Box<Pattern>),
    /// Built-in control naming map by 1-based registry id over a plain handle,
    /// avoiding closure boxing and pool locks on non-sequence patterns such as
    /// signals while keeping [`Node`] 24 bytes.
    ControlMap(Arc<NodeCell>, u8, u16, bool),
    /// Boxed fallback of [`Node::ControlMap`] when the inner handle carries
    /// `_steps` or [`PatternExtras`].
    ControlMapBoxed(Box<(Pattern, u16, bool)>),
    /// `withHaps` with native per-hap drop: the map-then-`removeUndefineds`
    /// shape (`scale` uses it). A closure that hits a query-time throw signals
    /// through `signal_query_error` and returns `None`; the flag surfaces at
    /// the query boundary like every other query-time error.
    MapHapsNative(Box<(Pattern, HapMapFn, SpanUse)>),
    /// A native per-hap map that takes ownership of the incoming hap, so a
    /// value or context change does not clone them.
    MapHapsOwned(Box<(Pattern, OwnedHapMapFn, SpanUse)>),
    UiVisual(Box<UiVisualNode>),
    MapHapsWithState(Box<(Pattern, StateHapMapFn)>),
    /// One hap expands to N haps sharing its span - `voicing()`'s
    /// `stack(...notes).note().set(rest)` + outerJoin collapses to exactly
    /// this shape natively (each note covers the source hap's whole/part).
    ExpandHapsNative(Box<(Pattern, HapExpandFn, SpanUse)>),
    Collect(Box<Pattern>),
    /// `isobite`: the onsets in the cycle of the query, each valued by its
    /// position among them. An onset is a hap with a value not loosely 0.
    CycleOnsetIndex(Box<Pattern>),
    /// `isobite`: one hap over the cycle of the query, valued by the number
    /// of onsets in the cycle.
    CycleOnsetCount(Box<Pattern>),
    /// `withSeed(() => n)`: rewrite `state.controls.randSeed`
    /// for the subtree, which every random signal reads. The registered
    /// `seed(n)` combinator is the constant form; function-valued `withSeed`
    /// is a JS surface the native scope does not expose.
    WithRandSeed(Box<(Pattern, f64)>),
    Signal(SignalFn),
    ChooseCycles(Box<(Box<[Pattern]>, u32)>),
    /// Weighted random choice. See [`WeightedChoice`].
    WChoose(Box<WeightedChoice>),
    /// A selector pattern choosing reified entries from a JavaScript array or
    /// object, followed by the requested join.
    Pick(Box<PickNode>),
    /// A [`Node::Pick`] whose lookup is a pattern: the selector, then the
    /// lookup.
    PickPatternified {
        patterns: Box<PatternPair>,
        index_mode: PickIndexMode,
        mode: JoinMode,
    },
    Degrade(Box<(Pattern, f64, DegradeKind)>),
    Stack(Box<[Pattern]>),
    /// Compact [`Node::Stack`] when every child has `flags == 0` and no extras:
    /// 8 bytes per child instead of 24 bytes.
    StackPure(Box<[Arc<NodeCell>]>),
    SlowCat(Box<[Pattern]>),
    /// Compact [`Node::SlowCat`] when every child has `flags == 0` and no extras.
    SlowCatPure(Box<[Arc<NodeCell>]>),
    /// `slowcatPrime` - cycle-skipping concatenation used by `every`/`lastOf`.
    SlowCatPrime(Box<[Pattern]>),
    /// Compact [`Node::SlowCatPrime`] when every child has `flags == 0` and no extras.
    SlowCatPrimePure(Box<[Arc<NodeCell>]>),
    /// `innerJoin` over a pattern-of-patterns produced by a native function.
    InnerJoin(Box<(Pattern, ToPatternFn)>),
    SplitQueries(Arc<NodeCell>, u8),
    SplitQueriesBoxed(Box<Pattern>),
    /// `fastGap`: speed up, leaving a gap rather than repeating.
    FastGap(Box<(Pattern, Fraction)>),
    RepeatCycles(Box<(Pattern, Fraction)>),
    LoopAt(Box<(Pattern, Fraction)>),
    /// Run this pattern at an absolute cycles-per-minute rate. Unlike a
    /// plain `fast(cpm / 60)`, this divides by the scheduler's `_cps`, so
    /// `.cpm(30)` means the same thing when the session default is 0.5 CPS.
    Cpm(Box<(Pattern, TempoRate)>),
    MidiKeys(Box<(Pattern, Arc<midi_in::InputPort>)>),
    /// `appLeft`: a pattern of functions applied to a pattern of values,
    /// keeping the FUNCTION side's whole. This is the fold `register()` uses
    /// when a leading argument is itself a pattern.
    AppLeft(Box<AppLeftNode>),
    /// Collapsed `appLeft(..., ControlMap(wave, key_id, false), Set, Set)`:
    /// stores the compiled [`fused::Wave`] inline with zero `OnceLock` and
    /// drops the temporary wave graph immediately.
    AppWave(Box<AppWaveNode>),
    /// A pattern whose every hap meets constants in turn: what `.gain(0.8)`,
    /// `sine.range(200, 2000)` and `.add(7)` give of a pattern that is no
    /// sequence. One node for the run, where each constant was an `appLeft`
    /// and a pattern of the constant, to find at the first query. The
    /// meetings are in the order of the applications.
    Constants(Box<ConstantsNode>),
    /// `appRight`: keeps the VALUE side's whole and structure.
    AppRight(Box<AppNode>),
    /// `appBoth`: both sides are queried over the SAME span and the wholes
    /// intersect. Tidal's `<*>`.
    AppBoth(Box<AppNode>),
    /// Appends source locations to every hap's context.
    AddContext(Box<AddContextNode>),
    /// A pattern whose values become patterns via a closure that is
    /// **statically proven pure by its return type** (`-> PurePattern`).
    /// The constructions this node has answered so far ride beside the
    /// closure; see [`ConstructionMemo`].
    PatternOfPure(Box<(Pattern, ToPurePatternFn, ConstructionMemo)>),
    /// [`PatternOfPure`] for a registered combinator body that also reads a
    /// pattern operand. The operand rides in the node as a compared child,
    /// not inside the closure as a hidden capture, so a rebuild whose
    /// operand is unchanged agrees with its counterpart and carries its
    /// answers. The payload - the registered body plus the chain width it
    /// unpacks values to - is the same in every evaluation of one call.
    PatternOfPureOperand(Box<OperandPureNode>, Arc<PatternedPureMap>),
    /// A pattern whose values become patterns via an **opaque** closure. The
    /// closure may materialise anything, including JavaScript, so this is
    /// always classified impure with an incomplete reachable set.
    PatternOfDynamic(Box<(Pattern, ToPatternFn)>),
    /// Native metadata-aware bind. The closure receives the complete outer
    /// hap, including exact lookup shape, while `dependencies` makes every
    /// graph it may return explicit to purity/ownership analysis.
    PatternOfHap(Box<HapBind>),
    /// Tune from ratio patterns while keeping the source note's spans.
    XenPatterns(Box<(Pattern, Vec<Pattern>)>),
    /// Flatten a `PatternOf`, choosing whose whole survives.
    Join(Arc<NodeCell>, u8, JoinMode),
    /// Boxed fallback of [`Node::Join`] when the inner handle carries `_steps`
    /// or [`PatternExtras`].
    JoinBoxed(Box<(Pattern, JoinMode)>),
    /// `fmap` with a USER callback. Holds only the opaque id.
    FmapJs(Box<(Pattern, CallbackId)>),
    /// Trusted host finalization retains unchanged native control bindings.
    FmapJsPreservingControls(Box<FmapJsControlsNode>),
    /// A user-authored `new Pattern(state => …)`. Holds only the opaque id.
    JsQuery(CallbackId),
    Timeline(Box<PatternPair>, TimelineState),
}

type AddContextNode = (Pattern, Arc<[(usize, usize)]>);
type FmapJsControlsNode = (Pattern, CallbackId, Option<fn(&Value) -> bool>);

/// Mode of [`Node::Degrade`]: seeded offset (`degradeBySeeded`), standard
/// `rand` (`degradeBy`), or complementary `1 - rand` (`undegradeBy`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DegradeKind {
    Seeded(u32),
    Rand,
    RandInverted,
}

/// Boxed payload of [`Node::Fast`].
pub(crate) struct FastNode(
    pub(crate) Pattern,
    pub(crate) Fraction,
    pub(crate) &'static str,
);

/// Boxed payload of [`Node::Shift`].
pub(crate) struct ShiftNode(
    pub(crate) Pattern,
    pub(crate) Fraction,
    pub(crate) &'static str,
);

/// Boxed payload of [`Node::Iter`].
pub(crate) struct IterNode {
    pub(crate) inner: Pattern,
    pub(crate) times: Fraction,
    pub(crate) parts: u64,
    pub(crate) back: bool,
}

/// Boxed payload of [`Node::UiVisual`].
pub(crate) struct UiVisualNode {
    pub(crate) inner: Pattern,
    pub(crate) slot: u8,
    pub(crate) cache: context_keys::UiVisualCache,
}

/// Boxed payload of [`Node::Pick`].
pub(crate) struct PickNode {
    pub(crate) selector: Pattern,
    pub(crate) lookup: CompactPickLookup,
    pub(crate) index_mode: PickIndexMode,
    pub(crate) mode: JoinMode,
}

/// One inline wave control inside [`AppWaveNode`].
#[derive(Clone)]
pub(crate) struct AppWaveItem {
    pub(crate) wave: fused::Wave,
    /// `0` for a continuous wave; `> 0` for a `steps`-per-cycle segmented wave.
    pub(crate) steps: u32,
    pub(crate) key_id: u16,
    pub(crate) math_inside: bool,
}

/// Compact storage for one or more consecutive inline wave controls: a single
/// wave sits inline with no secondary allocation, while stacked wave controls
/// (`.lpf(sine...).pan(rand)`) collapse into one [`AppWaveNode`].
#[derive(Clone)]
pub(crate) enum AppWaves {
    One(AppWaveItem),
    Many(Box<[AppWaveItem]>),
}

impl std::ops::Deref for AppWaves {
    type Target = [AppWaveItem];

    #[inline(always)]
    fn deref(&self) -> &[AppWaveItem] {
        match self {
            Self::One(one) => std::slice::from_ref(one),
            Self::Many(many) => many,
        }
    }
}

/// Boxed payload of [`Node::AppWave`].
pub(crate) struct AppWaveNode {
    pub(crate) inner: Pattern,
    pub(crate) waves: AppWaves,
}

const _: () = assert!(std::mem::size_of::<AppWaveNode>() <= 80);

/// Compact storage for one or more [`RunMeeting`]s in [`ConstantsNode`]: a
/// single meeting (the common case) sits inline with no secondary slice allocation.
#[derive(Clone)]
pub(crate) enum ConstantsMeetings {
    One(RunMeeting),
    Many(Box<[RunMeeting]>),
}

impl ConstantsMeetings {
    fn from_iter(iter: impl IntoIterator<Item = RunMeeting>) -> Self {
        let mut iter = iter.into_iter();
        let Some(first) = iter.next() else {
            return Self::Many(Box::new([]));
        };
        let Some(second) = iter.next() else {
            return Self::One(first);
        };
        let mut vec = Vec::with_capacity(2 + iter.size_hint().0);
        vec.push(first);
        vec.push(second);
        vec.extend(iter);
        Self::Many(vec.into_boxed_slice())
    }

    fn extend_in_place(&mut self, iter: impl IntoIterator<Item = RunMeeting>) {
        let mut iter = iter.into_iter();
        let Some(first_new) = iter.next() else {
            return;
        };
        let prev = std::mem::replace(self, Self::Many(Box::new([])));
        let mut vec = match prev {
            Self::One(one) => {
                let mut v = Vec::with_capacity(2 + iter.size_hint().0);
                v.push(one);
                v
            }
            Self::Many(many) => {
                let mut v = many.into_vec();
                v.reserve(1 + iter.size_hint().0);
                v
            }
        };
        vec.push(first_new);
        vec.extend(iter);
        *self = Self::Many(vec.into_boxed_slice());
    }
}

impl std::ops::Deref for ConstantsMeetings {
    type Target = [RunMeeting];

    #[inline(always)]
    fn deref(&self) -> &[RunMeeting] {
        match self {
            Self::One(one) => std::slice::from_ref(one),
            Self::Many(many) => many,
        }
    }
}

impl std::ops::DerefMut for ConstantsMeetings {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut [RunMeeting] {
        match self {
            Self::One(one) => std::slice::from_mut(one),
            Self::Many(many) => many,
        }
    }
}

/// Boxed payload of [`Node::Constants`].
pub(crate) struct ConstantsNode(pub(crate) Pattern, pub(crate) ConstantsMeetings);

#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<ConstantsNode>() <= 64);

/// The two children of a node that has two: functions and values, a
/// pattern and its operand, a selector and its lookup. One box for both,
/// so the node of every other kind stays small.
pub(crate) struct PatternPair(pub(crate) Pattern, pub(crate) Pattern);

impl PatternPair {
    /// The first child that binds a runtime, if any.
    #[inline]
    fn runtime_settings_source(&self) -> Option<&Pattern> {
        if self.0.has_runtime_settings() {
            Some(&self.0)
        } else if self.1.has_runtime_settings() {
            Some(&self.1)
        } else {
            None
        }
    }
}

/// Boxed payload of [`Node::AppRight`] and [`Node::AppBoth`].
pub(crate) struct AppNode(
    pub(crate) Pattern,
    pub(crate) Pattern,
    pub(crate) Combine,
    pub(crate) LookupFlow,
);

impl AppNode {
    #[inline]
    fn runtime_settings_source(&self) -> Option<&Pattern> {
        if self.0.has_runtime_settings() {
            Some(&self.0)
        } else if self.1.has_runtime_settings() {
            Some(&self.1)
        } else {
            None
        }
    }
}

/// Boxed payload of [`Node::AppLeft`], keeping `ValueReading`, `Combine`, and
/// `LookupFlow` out of every other [`Node`] variant.
pub(crate) struct AppLeftNode(
    pub(crate) Pattern,
    pub(crate) Pattern,
    ValueReading,
    pub(crate) Combine,
    pub(crate) LookupFlow,
);

const _: () = assert!(std::mem::size_of::<AppLeftNode>() <= 88);

impl AppLeftNode {
    #[inline]
    fn runtime_settings_source(&self) -> Option<&Pattern> {
        if self.0.has_runtime_settings() {
            Some(&self.0)
        } else if self.1.has_runtime_settings() {
            Some(&self.1)
        } else {
            None
        }
    }
}

/// Boxed payload of [`Node::PatternOfPureOperand`], keeping `ConstructionMemo`
/// out of every other [`Node`] variant.
pub(crate) struct OperandPureNode(
    pub(crate) Pattern,
    pub(crate) Pattern,
    pub(crate) ConstructionMemo,
);

impl OperandPureNode {
    #[inline]
    fn runtime_settings_source(&self) -> Option<&Pattern> {
        if self.0.has_runtime_settings() {
            Some(&self.0)
        } else if self.1.has_runtime_settings() {
            Some(&self.1)
        } else {
            None
        }
    }
}

/// The patterns and the flag of a [`Node::WChoose`]. `values` and cumulative
/// `weights` are patterns, not construction-time scalars; `chooser` is `rand`
/// for `wchoose` and `rand.segment(1)` for `wchooseCycles`. The boolean
/// selects the final inner join (`true`) versus outer join (`false`).
pub(crate) struct WeightedChoice {
    pub(crate) chooser: Pattern,
    pub(crate) total: Pattern,
    pub(crate) cumulative: Vec<Pattern>,
    pub(crate) values: Vec<Pattern>,
    pub(crate) inner: bool,
}

/// The payload of a [`Node::PatternOfHap`]: the outer pattern, every graph
/// that the bind may return, and the bind.
pub(crate) struct HapBind(
    pub(crate) Pattern,
    pub(crate) Vec<Pattern>,
    pub(crate) HapToPatternFn,
);

impl Pattern {
    /// Whether this pattern answers every cycle with one hap that fills the
    /// cycle and that only its span tells from the next cycle's. Read from
    /// the node kinds alone: a data value, under maps that do not read the
    /// span.
    fn is_constant_per_cycle(&self) -> bool {
        self.constant_levels().is_some()
    }

    /// The levels of this graph, its own node included, when the pattern
    /// [is constant per cycle](Self::is_constant_per_cycle).
    fn constant_levels(&self) -> Option<u32> {
        if self.has_runtime_settings() || self.is_volatile() {
            return None;
        }
        match self.node_kind() {
            Node::Pure(value) => value.is_data(0).then_some(1),
            Node::PureAt(boxed) => boxed.0.is_data(0).then_some(1),
            // One value that fills each cycle, as a control of a `pure` is
            // once it is folded.
            Node::Sequence(layers) => sequence::constant_value(layers).map(|_| 1),
            Node::ControlMap(cell, flags, _, _) => plain_pattern(cell, *flags)
                .constant_levels()?
                .checked_add(1),
            Node::ControlMapBoxed(boxed) => boxed.0.constant_levels()?.checked_add(1),
            Node::AddContext(boxed) => boxed.0.constant_levels()?.checked_add(1),
            Node::FmapNative(boxed) => boxed.0.constant_levels()?.checked_add(1),
            Node::MapHapsNative(boxed) if boxed.2 == SpanUse::Ignores => {
                boxed.0.constant_levels()?.checked_add(1)
            }
            Node::MapHapsOwned(boxed) if boxed.2 == SpanUse::Ignores => {
                boxed.0.constant_levels()?.checked_add(1)
            }
            Node::UiVisual(node) => node.inner.constant_levels()?.checked_add(1),
            _ => None,
        }
    }

    /// Direct cycle-zero constant extraction for common constant graphs,
    /// avoiding a full query pass and `Vec<Hap>` allocation on first visit.
    fn try_constant_cycle_direct(&self, cycle: TimeSpan) -> Option<Hap> {
        if self.has_runtime_settings() || self.is_volatile() {
            return None;
        }
        match self.node_kind() {
            Node::Pure(value) if value.is_data(0) => {
                Some(Hap::new(Some(cycle), cycle, value.clone()))
            }
            Node::PureAt(boxed) if boxed.0.is_data(0) => Some(Hap::of_sequence(
                cycle,
                cycle,
                boxed.0.clone(),
                boxed.1.clone(),
                ContextKeys::default(),
            )),
            Node::Sequence(_) => {
                let (value, context) = sequence::constant_value_at(self)?;
                Some(Hap::of_sequence(
                    cycle,
                    cycle,
                    value,
                    context,
                    ContextKeys::default(),
                ))
            }
            Node::AddContext(boxed) => {
                let mut hap = boxed.0.try_constant_cycle_direct(cycle)?;
                hap.context.extend_from_slice(&boxed.1);
                Some(hap)
            }
            Node::ControlMap(cell, flags, id, unnamed_only) => {
                let hap = plain_pattern(cell, *flags).try_constant_cycle_direct(cycle)?;
                let spec = controls::builtin_spec_by_id(*id)?;
                controls::apply_control_hap(spec, *unnamed_only, hap)
            }
            Node::ControlMapBoxed(boxed) => {
                let hap = boxed.0.try_constant_cycle_direct(cycle)?;
                let spec = controls::builtin_spec_by_id(boxed.1)?;
                controls::apply_control_hap(spec, boxed.2, hap)
            }
            Node::UiVisual(node) => {
                let mut hap = node.inner.try_constant_cycle_direct(cycle)?;
                hap.keys.add_ui_visual_slot(node.slot);
                Some(hap)
            }
            _ => None,
        }
    }

    /// The hap this pattern answers for cycle zero, when the pattern
    /// [is constant per cycle](Self::is_constant_per_cycle) and its graph
    /// answers so. No node of such a graph reads the controls of `state`.
    fn constant_cycle(&self, state: &State) -> Option<Hap> {
        let cycle = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
        if let Some(hap) = self.try_constant_cycle_direct(cycle) {
            return Some(hap);
        }
        if !self.is_constant_per_cycle() {
            return None;
        }
        let mut haps = self.query(&state.set_span(cycle));
        match (haps.pop(), haps.is_empty()) {
            (Some(hap), true) if hap.whole() == Some(cycle) && hap.part() == cycle => Some(hap),
            _ => None,
        }
    }
}

impl Hap {
    /// Move this cycle-zero hap of a [`Pattern::is_constant_per_cycle`]
    /// pattern to the hap that pattern answers for `piece`, one cycle piece
    /// of a span: the piece's cycle as the whole, the piece as the part.
    fn move_to_piece(&mut self, piece: TimeSpan) {
        self.set_spans(Some(whole_cycle(piece.begin)), piece);
    }
}

/// Derived combinators keep the runtime of their receiver without adding a
/// wrapper node. Children with their own different runtime still rebind when
/// queried; this selection governs work performed by the parent itself, such
/// as an `fmap` callback returning a Pattern value.
fn inherited_runtime_settings_source(node: &Node) -> Option<&Pattern> {
    fn from_patterns<'a>(patterns: impl IntoIterator<Item = &'a Pattern>) -> Option<&'a Pattern> {
        patterns
            .into_iter()
            .find(|pattern| pattern.has_runtime_settings())
    }

    match node {
        Node::Silence
        | Node::Pure(_)
        | Node::PureAt(_)
        | Node::IntRange { .. }
        | Node::Sequence(_)
        | Node::QueryError(_)
        | Node::QueryLimit(_)
        | Node::Signal(_)
        | Node::NumberSignal(_)
        | Node::SegmentSignal { .. }
        | Node::JsQuery(_)
        | Node::FastSmall(..)
        | Node::ShiftSmall(..)
        | Node::ControlMap(..)
        | Node::Join(..)
        | Node::SplitQueries(..) => None,
        Node::Iter(node) => node.inner.has_runtime_settings().then_some(&node.inner),
        Node::PatternOfPureOperand(pair, _) => pair.runtime_settings_source(),
        #[cfg(non_send_node_mutation)]
        Node::NonSendMutation(_) => None,
        Node::Rev(pattern)
        | Node::StepJoin(pattern)
        | Node::PolyJoin(pattern)
        | Node::PurePattern(pattern)
        | Node::SortHapsByPart(pattern)
        | Node::Collect(pattern)
        | Node::CycleOnsetIndex(pattern)
        | Node::CycleOnsetCount(pattern)
        | Node::SplitQueriesBoxed(pattern) => pattern.has_runtime_settings().then_some(pattern),
        Node::ControlMapBoxed(boxed) => boxed.0.has_runtime_settings().then_some(&boxed.0),
        Node::JoinBoxed(boxed) => boxed.0.has_runtime_settings().then_some(&boxed.0),
        Node::Fast(node) => node.0.has_runtime_settings().then_some(&node.0),
        Node::Shift(node) => node.0.has_runtime_settings().then_some(&node.0),
        Node::Constants(node) => node.0.has_runtime_settings().then_some(&node.0),
        Node::UiVisual(node) => node.inner.has_runtime_settings().then_some(&node.inner),
        Node::WithQueryTime(boxed) | Node::WithHapTime(boxed) => {
            boxed.0.has_runtime_settings().then_some(&boxed.0)
        }
        Node::WithQuerySpan(boxed) | Node::WithHapSpan(boxed) => {
            boxed.0.has_runtime_settings().then_some(&boxed.0)
        }
        Node::WithQuerySpanJs(boxed)
        | Node::ArpWith(boxed)
        | Node::PatternOfJs(boxed)
        | Node::FilterValuesJs(boxed)
        | Node::FilterHapsJs(boxed)
        | Node::FilterWhenJs(boxed)
        | Node::FmapJs(boxed) => boxed.0.has_runtime_settings().then_some(&boxed.0),
        Node::FmapNative(boxed) => boxed.0.has_runtime_settings().then_some(&boxed.0),
        Node::FilterValues(boxed) => boxed.0.has_runtime_settings().then_some(&boxed.0),
        Node::FilterHaps(boxed) => boxed.0.has_runtime_settings().then_some(&boxed.0),
        Node::MapHapsNative(boxed) => boxed.0.has_runtime_settings().then_some(&boxed.0),
        Node::MapHapsOwned(boxed) => boxed.0.has_runtime_settings().then_some(&boxed.0),
        Node::MapHapsWithState(boxed) => boxed.0.has_runtime_settings().then_some(&boxed.0),
        Node::ExpandHapsNative(boxed) => boxed.0.has_runtime_settings().then_some(&boxed.0),
        Node::WithRandSeed(boxed) => boxed.0.has_runtime_settings().then_some(&boxed.0),
        Node::Degrade(boxed) => boxed.0.has_runtime_settings().then_some(&boxed.0),
        Node::InnerJoin(boxed) | Node::PatternOfDynamic(boxed) => {
            boxed.0.has_runtime_settings().then_some(&boxed.0)
        }
        Node::FastGap(boxed) | Node::RepeatCycles(boxed) | Node::LoopAt(boxed) => {
            boxed.0.has_runtime_settings().then_some(&boxed.0)
        }
        Node::Cpm(boxed) => boxed.0.has_runtime_settings().then_some(&boxed.0),
        Node::MidiKeys(boxed) => boxed.0.has_runtime_settings().then_some(&boxed.0),
        Node::AddContext(boxed) => boxed.0.has_runtime_settings().then_some(&boxed.0),
        Node::PatternOfPure(boxed) => boxed.0.has_runtime_settings().then_some(&boxed.0),
        Node::FmapJsPreservingControls(boxed) => boxed.0.has_runtime_settings().then_some(&boxed.0),
        Node::Extension(extension) => from_patterns(extension.children()),
        Node::Timeline(pair, _) => pair
            .1
            .has_runtime_settings()
            .then_some(&pair.1)
            .or_else(|| pair.0.has_runtime_settings().then_some(&pair.0)),
        Node::AppLeft(pair) => pair.runtime_settings_source(),
        Node::AppWave(app) => app.inner.has_runtime_settings().then_some(&app.inner),
        Node::AppRight(node) | Node::AppBoth(node) => node.runtime_settings_source(),
        Node::Arp(pair)
        | Node::ArpWithPattern(pair)
        | Node::PickPatternified { patterns: pair, .. } => pair.runtime_settings_source(),
        Node::PurePickLookup(boxed) => from_patterns(boxed.1.patterns()),
        Node::ChooseCycles(boxed) => from_patterns(&boxed.0),
        Node::Stack(patterns) | Node::SlowCat(patterns) | Node::SlowCatPrime(patterns) => {
            from_patterns(patterns)
        }
        Node::StackPure(_) | Node::SlowCatPure(_) | Node::SlowCatPrimePure(_) => None,
        Node::WChoose(choice) => choice
            .chooser
            .has_runtime_settings()
            .then_some(&choice.chooser)
            .or_else(|| choice.total.has_runtime_settings().then_some(&choice.total))
            .or_else(|| from_patterns(&choice.cumulative))
            .or_else(|| from_patterns(&choice.values)),
        Node::Pick(pick) => pick
            .selector
            .has_runtime_settings()
            .then_some(&pick.selector)
            .or_else(|| from_patterns(pick.lookup.patterns())),
        Node::PatternOfHap(bind) => bind
            .0
            .has_runtime_settings()
            .then_some(&bind.0)
            .or_else(|| from_patterns(&bind.1)),
        Node::XenPatterns(boxed) => boxed
            .0
            .has_runtime_settings()
            .then_some(&boxed.0)
            .or_else(|| from_patterns(&boxed.1)),
    }
}

/// Inherit `runtime_settings` and `reachable` into an `Option<Arc<PatternExtras>>`,
/// reusing the child's `Arc<PatternExtras>` directly when it carries only
/// `runtime_settings` and `reachable` is empty.
#[inline]
fn inherited_extras(
    node: &Node,
    reachable: Option<Arc<Vec<CallbackId>>>,
) -> Option<Arc<PatternExtras>> {
    let source = inherited_runtime_settings_source(node);
    match (source, reachable) {
        (None, None) => None,
        (Some(pat), None)
            if pat
                .extras
                .as_deref()
                .is_some_and(PatternExtras::is_plain_runtime_settings) =>
        {
            pat.extras.clone()
        }
        (source, reachable) => Some(Arc::new(PatternExtras {
            runtime_settings: source.and_then(|p| p.runtime_settings().cloned()),
            rare: reachable.map(|reachable| {
                Box::new(PatternExtrasRare {
                    reachable: Some(reachable),
                    ..PatternExtrasRare::default()
                })
            }),
            ..PatternExtras::default()
        })),
    }
}

impl Pattern {
    /// Every constructor routes through here, so purity is computed exactly
    /// once per node and can never be forgotten by a new combinator.
    fn of(node: Node) -> Self {
        LIVE_NODES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut purity = compute_purity(&node);
        purity.span_read = span_read(&node);
        let flags = pack_purity_flags(&purity);
        let extras = inherited_extras(&node, purity.take_reachable());
        Pattern {
            node: Arc::new(NodeCell { kind: node }),
            step_numer: 0,
            step_denom: u32::from(flags) << NODE_FLAGS_SHIFT,
            extras,
        }
    }

    /// The handle that a constructor gives of `node`: what the kind of the
    /// node says, and no metadata of a handle.
    fn holding(node: Arc<NodeCell>) -> Self {
        let mut purity = compute_purity(&node.kind);
        purity.span_read = span_read(&node.kind);
        let flags = pack_purity_flags(&purity);
        let extras = inherited_extras(&node.kind, purity.take_reachable());
        Pattern {
            node,
            step_numer: 0,
            step_denom: u32::from(flags) << NODE_FLAGS_SHIFT,
            extras,
        }
    }

    #[inline(always)]
    pub(crate) fn node_flags(&self) -> u8 {
        (self.step_denom >> NODE_FLAGS_SHIFT) as u8
    }

    #[inline(always)]
    pub(crate) fn node_arc(&self) -> &Arc<NodeCell> {
        &self.node
    }

    #[inline(always)]
    pub(crate) fn node_arc_mut(&mut self) -> &mut Arc<NodeCell> {
        &mut self.node
    }

    #[inline(always)]
    pub(crate) fn set_node_arc(&mut self, node: Arc<NodeCell>) {
        self.node = node;
    }

    #[inline(always)]
    pub(crate) fn has_internable_extras(&self) -> bool {
        self.extras
            .as_deref()
            .is_none_or(PatternExtras::is_plain_runtime_settings)
    }

    #[inline(always)]
    pub(crate) fn extras_ptr(&self) -> usize {
        self.extras.as_ref().map_or(0, |e| Arc::as_ptr(e) as usize)
    }

    #[inline(always)]
    pub(crate) fn plain_runtime_settings_key(&self) -> Option<usize> {
        self.extras
            .as_deref()
            .and_then(PatternExtras::plain_runtime_settings_key)
    }

    #[inline]
    pub(crate) fn dedup_plain_extras(
        &mut self,
        intern: &mut impl FnMut(usize, &Arc<PatternExtras>) -> Option<Arc<PatternExtras>>,
    ) {
        if let Some(ex) = &self.extras
            && let Some(key) = ex.plain_runtime_settings_key()
            && let Some(canon) = intern(key, ex)
        {
            self.extras = Some(canon);
        }
    }

    #[inline(always)]
    pub(crate) fn packed_steps(&self) -> (i32, u32) {
        (self.step_numer, self.step_denom)
    }

    #[inline(always)]
    fn inline_step_denom(&self) -> u32 {
        self.step_denom & STEP_DENOM_MASK
    }

    #[inline(always)]
    fn inline_pure_loc(&self) -> u32 {
        if self.inline_step_denom() == 0 {
            self.step_numer as u32
        } else {
            0
        }
    }

    #[inline(always)]
    fn is_plain_handle(&self) -> bool {
        self.inline_step_denom() == 0 && self.step_numer == 0 && self.extras.is_none()
    }

    #[inline]
    fn control_map_node(inner: Pattern, id: u16, unnamed_only: bool) -> Node {
        if inner.is_plain_handle() {
            let flags = inner.node_flags();
            Node::ControlMap(inner.node, flags, id, unnamed_only)
        } else {
            Node::ControlMapBoxed(Box::new((inner, id, unnamed_only)))
        }
    }

    #[inline]
    fn join_node(inner: Pattern, mode: JoinMode) -> Node {
        if inner.is_plain_handle() {
            let flags = inner.node_flags();
            Node::Join(inner.node, flags, mode)
        } else {
            Node::JoinBoxed(Box::new((inner, mode)))
        }
    }

    fn extras_mut(&mut self) -> &mut PatternExtras {
        Arc::make_mut(
            self.extras
                .get_or_insert_with(|| Arc::new(PatternExtras::default())),
        )
    }

    #[inline(always)]
    fn effective_flags(&self) -> u8 {
        self.node_flags()
    }

    #[inline(always)]
    pub(crate) fn runtime_settings(&self) -> Option<&settings::RuntimeSettings> {
        self.extras
            .as_deref()
            .and_then(|e| e.runtime_settings.as_ref())
    }

    #[inline(always)]
    pub(crate) fn has_runtime_settings(&self) -> bool {
        self.runtime_settings().is_some()
    }

    #[inline(always)]
    pub(crate) fn span_read(&self) -> purity::SpanRead {
        unpack_span_read(self.node_flags())
    }

    #[inline(always)]
    pub(crate) fn same_purity_flags(&self, other: &Self) -> bool {
        let mask = FLAG_IMPURE | FLAG_OPAQUE | FLAG_VOLATILE | FLAG_HOST_VOICINGS;
        (self.effective_flags() & mask) == (other.effective_flags() & mask)
    }

    pub(crate) fn same_steps(&self, other: &Self) -> bool {
        let a_denom = self.inline_step_denom();
        let b_denom = other.inline_step_denom();
        if a_denom != b_denom {
            return false;
        }
        if a_denom != 0 {
            return self.step_numer == other.step_numer;
        }
        let a = self.extras.as_deref().and_then(PatternExtras::large_steps);
        let b = other.extras.as_deref().and_then(PatternExtras::large_steps);
        a == b
    }

    pub(crate) fn clear_impure(&mut self) {
        self.step_denom &= !(u32::from(FLAG_IMPURE) << NODE_FLAGS_SHIFT);
    }

    fn node_kind(&self) -> &Node {
        &self.node.kind
    }

    /// Whether this pattern is silence, which a query has no need to visit.
    fn is_rest(&self) -> bool {
        matches!(self.node_kind(), Node::Silence)
    }

    /// Whether this handle may use stateless signal shortcuts under the runtime
    /// in force: a cacheable graph with no mismatching runtime owner.
    fn tables_may_answer(&self) -> bool {
        self.is_cacheable()
            && self
                .runtime_settings()
                .is_none_or(settings::RuntimeSettings::is_current_requested)
    }

    /// Whether this pattern is one node of plain values at their places.
    /// For tests that hold a fold.
    #[doc(hidden)]
    pub fn is_sequence(&self) -> bool {
        matches!(self.node_kind(), Node::Sequence(_))
    }

    /// Answer tables have been removed in favour of direct graph walks.
    #[doc(hidden)]
    pub fn answers_from_tables(&self, _span: TimeSpan) -> bool {
        false
    }

    /// The cached classification. Impurity is monotonic: once impure, a
    /// pattern never becomes pure again.
    pub fn purity(&self) -> purity::Purity {
        let flags = self.effective_flags();
        purity::Purity::from_packed(
            flags & FLAG_IMPURE != 0,
            flags & FLAG_OPAQUE != 0,
            flags & FLAG_VOLATILE != 0,
            flags & FLAG_HOST_VOICINGS != 0,
            unpack_span_read(self.node_flags()),
            self.extras.as_deref().and_then(|e| e.reachable().cloned()),
        )
    }

    #[inline(always)]
    pub fn is_pure(&self) -> bool {
        self.effective_flags() & FLAG_IMPURE == 0
    }

    /// Whether this node is a sequence carrying any lazy sourced layer.
    pub fn has_sourced_layers(&self) -> bool {
        matches!(self.node_kind(), Node::Sequence(layers) if sequence::has_source(layers))
    }

    /// Whether a result cache may keep what this pattern answers: no
    /// JavaScript, and nothing reachable that answers differently to the
    /// same query. See [`purity::Purity::volatile`].
    #[inline(always)]
    pub fn is_cacheable(&self) -> bool {
        self.is_pure() && !self.is_volatile()
    }

    /// Whether two identical queries may answer differently under the
    /// settings in force. See [`purity::Purity::volatile`] and
    /// [`purity::Purity::host_voicings`].
    #[inline(always)]
    fn is_volatile(&self) -> bool {
        let flags = self.effective_flags();
        (flags & FLAG_VOLATILE != 0)
            || ((flags & FLAG_HOST_VOICINGS != 0) && settings::default_voicings_is_host_owned())
    }

    /// All patterns now walk directly without building answer tables.
    pub fn mark_walked(&self) -> Self {
        self.clone()
    }

    /// This handle, declared volatile - see [`purity::Purity::volatile`].
    pub fn mark_volatile(&self) -> Self {
        let mut pattern = self.clone();
        pattern.step_denom |= u32::from(FLAG_VOLATILE) << NODE_FLAGS_SHIFT;
        pattern
    }

    /// This handle, also reaching the callbacks `ids`, with its purity kept.
    pub(crate) fn retaining_callbacks(self, ids: impl IntoIterator<Item = CallbackId>) -> Self {
        let mut pattern = self;
        let mut purity = pattern.purity();
        purity.reach(ids);
        let reachable = purity.take_reachable();
        if reachable.is_some() {
            pattern
                .extras_mut()
                .rare
                .get_or_insert_with(Box::default)
                .reachable = reachable;
        }
        pattern
    }

    /// This handle, marked as reading the default voicing dictionary. See
    /// [`purity::Purity::host_voicings`].
    pub(crate) fn mark_host_voicings(&self) -> Self {
        let mut pattern = self.clone();
        pattern.step_denom |= u32::from(FLAG_HOST_VOICINGS) << NODE_FLAGS_SHIFT;
        pattern
    }

    /// Callbacks reachable from this subtree - what L2's `gc_mark` marks.
    pub fn reachable_callbacks(&self) -> &[CallbackId] {
        self.extras
            .as_deref()
            .and_then(PatternExtras::reachable)
            .map_or(&[], |v| v.as_slice())
    }

    /// Upgrade to the type the tight-lookahead path accepts. `None` for an
    /// impure graph, so the guarantee is enforced by the type system.
    pub fn as_pure_pattern(&self) -> Option<purity::PurePattern> {
        if self.is_pure() {
            Some(purity::PurePattern(self.clone()))
        } else {
            None
        }
    }

    fn same_runtime_owner(&self, other: &Self) -> bool {
        match (self.runtime_settings(), other.runtime_settings()) {
            (Some(left), Some(right)) => left.same_handle(right),
            (None, None) => true,
            _ => false,
        }
    }

    /// `_steps` of this pattern: how many steps one cycle of it has.
    pub fn steps(&self) -> Option<Fraction> {
        let denom = self.inline_step_denom();
        if denom != 0 {
            Some(Fraction::new(
                i128::from(self.step_numer),
                i128::from(denom),
            ))
        } else {
            self.extras.as_deref().and_then(PatternExtras::large_steps)
        }
    }

    pub fn with_steps(mut self, steps: Option<Fraction>) -> Self {
        let flags_bits = self.step_denom & !STEP_DENOM_MASK;
        let saved_pure_loc = self.inline_pure_loc();
        match steps {
            None => {
                self.step_denom = flags_bits;
                self.step_numer = saved_pure_loc as i32;
                if self
                    .extras
                    .as_deref()
                    .is_some_and(|e| e.large_steps().is_some())
                {
                    self.extras_mut().set_large_steps(None);
                }
                if self.step_numer == 0
                    && let Some(packed) = self
                        .extras
                        .as_deref()
                        .map(|e| e.pure_loc_packed)
                        .filter(|&p| p != 0)
                {
                    self.step_numer = packed as i32;
                    self.extras_mut().pure_loc_packed = 0;
                }
                if self.extras.as_deref().is_some_and(PatternExtras::is_empty) {
                    self.extras = None;
                }
            }
            Some(steps) => {
                let denom = u32::try_from(steps.denom())
                    .ok()
                    .filter(|&d| d != 0 && d <= STEP_DENOM_MASK);
                match (i32::try_from(steps.numer()), denom) {
                    (Ok(numer), Some(denom)) => {
                        self.step_numer = numer;
                        self.step_denom = flags_bits | denom;
                        if saved_pure_loc != 0 {
                            self.extras_mut().pure_loc_packed = saved_pure_loc;
                        }
                        if self
                            .extras
                            .as_deref()
                            .is_some_and(|e| e.large_steps().is_some())
                        {
                            self.extras_mut().set_large_steps(None);
                            if self.extras.as_deref().is_some_and(PatternExtras::is_empty) {
                                self.extras = None;
                            }
                        }
                    }
                    _ => {
                        self.step_numer = saved_pure_loc as i32;
                        self.step_denom = flags_bits;
                        self.extras_mut().set_large_steps(Some(steps));
                    }
                }
            }
        }
        self
    }

    /// [`Self::with_steps`] of the steps of `other`, which the handle
    /// takes as they are held.
    pub fn with_steps_of(mut self, other: &Self) -> Self {
        let saved_pure_loc = self.inline_pure_loc();
        let other_denom = other.inline_step_denom();
        if other_denom != 0 {
            self.step_numer = other.step_numer;
            self.step_denom = (self.step_denom & !STEP_DENOM_MASK) | other_denom;
            if saved_pure_loc != 0 {
                self.extras_mut().pure_loc_packed = saved_pure_loc;
            }
        } else {
            self.step_denom &= !STEP_DENOM_MASK;
            self.step_numer = saved_pure_loc as i32;
            if self.step_numer == 0
                && let Some(packed) = self
                    .extras
                    .as_deref()
                    .map(|e| e.pure_loc_packed)
                    .filter(|&p| p != 0)
            {
                self.step_numer = packed as i32;
                self.extras_mut().pure_loc_packed = 0;
            }
        }
        let large = other.extras.as_deref().and_then(PatternExtras::large_steps);
        if large.is_some()
            || self
                .extras
                .as_deref()
                .is_some_and(|e| e.large_steps().is_some())
        {
            self.extras_mut().set_large_steps(large);
        }
        if self.extras.as_deref().is_some_and(PatternExtras::is_empty) {
            self.extras = None;
        }
        self
    }

    /// Attach the module state that must be active whenever this graph is
    /// queried outside its owning runtime. An existing owner is preserved.
    #[doc(hidden)]
    pub fn with_runtime_settings(mut self, settings: settings::RuntimeSettings) -> Self {
        self.attach_runtime_settings(settings);
        self
    }

    /// Attach a pre-allocated plain `runtime_settings` extras handle with zero
    /// heap allocations when `self.extras` is empty.
    #[doc(hidden)]
    #[inline]
    pub fn with_shared_runtime_settings(mut self, shared: &PatternSharedExtras) -> Self {
        if self.extras.is_none() {
            // Share the runtime's single plain `PatternExtras` allocation.
            self.extras = Some(Arc::clone(&shared.0));
        } else if !self.has_runtime_settings() {
            self.extras_mut().runtime_settings = shared.0.runtime_settings.clone();
        }
        self
    }

    /// Attach an owner without allocating a graph wrapper. The innermost owner
    /// wins, matching nested runtime scopes while flattening repeated exports.
    pub(crate) fn attach_runtime_settings(&mut self, settings: settings::RuntimeSettings) {
        if self.extras.is_none() {
            if let Some(shared) = settings::RuntimeSettings::plain_extras_for(&settings) {
                self.extras = Some(shared);
                return;
            }
            self.extras = Some(Arc::new(PatternExtras::of_runtime_settings(settings)));
        } else if !self.has_runtime_settings() {
            self.extras_mut().runtime_settings = Some(settings);
        }
    }

    fn attach_runtime_settings_shared(
        &mut self,
        settings: &settings::RuntimeSettings,
        shared: &Arc<PatternExtras>,
    ) {
        if self.extras.is_none() {
            self.extras = Some(Arc::clone(shared));
        } else if !self.has_runtime_settings() {
            self.extras_mut().runtime_settings = Some(settings.clone());
        }
    }

    /// Select the lexical owner of a newly returned root while leaving the
    /// owners already carried by its child graph intact.
    pub(crate) fn force_runtime_settings(&mut self, settings: settings::RuntimeSettings) {
        if self.extras.is_none()
            || self
                .extras
                .as_deref()
                .is_some_and(PatternExtras::is_plain_runtime_settings)
        {
            if let Some(shared) = settings::RuntimeSettings::plain_extras_for(&settings) {
                self.extras = Some(shared);
                return;
            }
            self.extras = Some(Arc::new(PatternExtras::of_runtime_settings(settings)));
            return;
        }
        self.extras_mut().runtime_settings = Some(settings);
    }

    /// `__pure` - the value if this is a `pure` pattern, else `None`.
    /// `register()`'s fast path keys off this, and it is semantically
    /// observable because that path also merges source locations.
    fn as_pure_inner(&self) -> Option<Value> {
        match self.node_kind() {
            Node::Pure(v) => Some(v.clone()),
            Node::PureAt(boxed) => Some(boxed.0.clone()),
            Node::PurePickLookup(boxed) => Some(boxed.0.clone()),
            // `withContext` preserves the pure fast-path metadata;
            // AddContext is that operation's native representation.
            Node::AddContext(boxed) => boxed.0.as_pure_inner(),
            _ => None,
        }
    }

    pub fn as_pure(&self) -> Option<Value> {
        let mut value = self.as_pure_inner()?;
        let owner = self
            .runtime_settings()
            .cloned()
            .or_else(settings::RuntimeSettings::current_requested);
        if let Some(settings) = owner
            && !value.attach_runtime_settings(&settings, 0)
        {
            return None;
        }
        Some(value)
    }

    fn pick_lookup_with_runtime_settings(&self, mut lookup: PickLookup) -> Option<PickLookup> {
        let owner = self
            .runtime_settings()
            .cloned()
            .or_else(settings::RuntimeSettings::current_requested);
        if let Some(settings) = owner
            && !lookup.attach_runtime_settings(&settings)
        {
            return None;
        }
        Some(lookup)
    }

    pub fn as_pick_lookup(&self) -> Option<PickLookup> {
        let lookup = match self.node_kind() {
            Node::PurePickLookup(boxed) => Some(boxed.1.clone()),
            _ => None,
        }?;
        self.pick_lookup_with_runtime_settings(lookup)
    }

    /// `__pure_loc` - the source span attached to a pure value, if any.
    pub fn pure_loc(&self) -> Option<(usize, usize)> {
        self.pure_loc_raw()
            .map(|(begin, end)| (begin as usize, end as usize))
    }

    fn pure_loc_raw(&self) -> Option<(u32, u32)> {
        let inline = self.inline_pure_loc();
        if inline != 0 {
            let (begin, end) = unpack_run_loc(inline);
            return Some((begin as u32, end as u32));
        }
        self.extras.as_deref().and_then(PatternExtras::pure_loc)
    }

    fn set_pure_loc_raw(&mut self, loc: Option<(u32, u32)>) {
        match loc {
            Some((begin, end)) => {
                if self.inline_step_denom() == 0
                    && let Some(packed) = pack_run_loc((begin as usize, end as usize))
                {
                    self.step_numer = packed as i32;
                    if self
                        .extras
                        .as_deref()
                        .is_some_and(|e| e.pure_loc().is_some())
                    {
                        self.extras_mut().set_pure_loc(None);
                        if self.extras.as_deref().is_some_and(PatternExtras::is_empty) {
                            self.extras = None;
                        }
                    }
                } else {
                    if self.inline_step_denom() == 0 {
                        self.step_numer = 0;
                    }
                    self.extras_mut().set_pure_loc(Some((begin, end)));
                }
            }
            None => {
                if self.inline_step_denom() == 0 {
                    self.step_numer = 0;
                }
                if self
                    .extras
                    .as_deref()
                    .is_some_and(|e| e.pure_loc().is_some())
                {
                    self.extras_mut().set_pure_loc(None);
                    if self.extras.as_deref().is_some_and(PatternExtras::is_empty) {
                        self.extras = None;
                    }
                }
            }
        }
    }

    pub fn with_pure_loc(mut self, loc: (usize, usize)) -> Self {
        // A span of a score, which is far shorter than the count of `u32`.
        let loc = u32::try_from(loc.0).ok().zip(u32::try_from(loc.1).ok());
        if loc.is_some() || self.pure_loc_raw().is_some() {
            self.set_pure_loc_raw(loc);
        }
        self
    }

    /// Append source locations to every hap's context, as
    /// `withContext(ctx => ({...ctx, locations: [...ctx.locations, ...locs]}))`.
    pub fn with_added_context(&self, locs: Vec<(usize, usize)>) -> Self {
        if let Some(located) = sequence::located(self, &locs) {
            return located.with_steps_of(self);
        }
        if !self.has_runtime_settings() {
            match self.node_kind() {
                Node::Pure(value) => {
                    let mut out = Locations::new();
                    out.extend_from_slice(&locs);
                    let mut pattern = Pattern::of(Node::PureAt(Box::new((value.clone(), out))))
                        .with_steps_of(self);
                    if let Some(loc) = self.pure_loc_raw() {
                        pattern.set_pure_loc_raw(Some(loc));
                    }
                    return pattern;
                }
                Node::PureAt(boxed) => {
                    let mut out = boxed.1.clone();
                    out.extend_from_slice(&locs);
                    let mut pattern = Pattern::of(Node::PureAt(Box::new((boxed.0.clone(), out))))
                        .with_steps_of(self);
                    if let Some(loc) = self.pure_loc_raw() {
                        pattern.set_pure_loc_raw(Some(loc));
                    }
                    return pattern;
                }
                Node::IntRange {
                    start,
                    count,
                    loc0,
                    loc1,
                    named_id,
                    step_neg,
                } if let Some((n0, n1)) = append_int_range_locs(*loc0, *loc1, &locs) => {
                    return Pattern::of(Node::IntRange {
                        start: *start,
                        count: *count,
                        loc0: n0,
                        loc1: n1,
                        named_id: *named_id,
                        step_neg: *step_neg,
                    })
                    .with_steps_of(self);
                }
                _ => {}
            }
        }
        let locs: Arc<[(usize, usize)]> = Arc::from(locs);
        let mut pattern =
            Pattern::of(Node::AddContext(Box::new((self.clone(), locs)))).with_steps_of(self);
        if let Some(loc) = self.pure_loc_raw() {
            pattern.set_pure_loc_raw(Some(loc));
        }
        pattern
    }

    /// Borrowed variant of [`Self::with_added_context`] that avoids allocating
    /// a temporary `Vec` when folded into a sequence or materialized as one `Arc`.
    /// Owned variant of [`Self::with_added_context_slice`]: mutates a uniquely
    /// owned `Node::Sequence` in place instead of cloning layers and `NodeCell`.
    pub(crate) fn with_added_context_owned(mut self, locs: &[(usize, usize)]) -> Self {
        if locs.is_empty() {
            return self;
        }
        if matches!(self.node_kind(), Node::Sequence(_))
            && sequence::folds(&self)
            && let Some(cell) = self.own_node()
            && let Node::Sequence(layers) = &mut cell.kind
            && sequence::locate_in_place(layers, locs)
        {
            return self;
        }
        if !self.has_runtime_settings()
            && let Node::IntRange { loc0, loc1, .. } = self.node_kind()
            && let Some((n0, n1)) = append_int_range_locs(*loc0, *loc1, locs)
            && let Some(cell) = self.own_node()
            && let Node::IntRange { loc0, loc1, .. } = &mut cell.kind
        {
            *loc0 = n0;
            *loc1 = n1;
            return self;
        }
        self.with_added_context_slice(locs)
    }

    pub fn with_added_context_slice(&self, locs: &[(usize, usize)]) -> Self {
        if let Some(located) = sequence::located(self, locs) {
            return located.with_steps_of(self);
        }
        if !self.has_runtime_settings() {
            match self.node_kind() {
                Node::Pure(value) => {
                    let mut out = Locations::new();
                    out.extend_from_slice(locs);
                    let mut pattern = Pattern::of(Node::PureAt(Box::new((value.clone(), out))))
                        .with_steps_of(self);
                    if let Some(loc) = self.pure_loc_raw() {
                        pattern.set_pure_loc_raw(Some(loc));
                    }
                    return pattern;
                }
                Node::PureAt(boxed) => {
                    let mut out = boxed.1.clone();
                    out.extend_from_slice(locs);
                    let mut pattern = Pattern::of(Node::PureAt(Box::new((boxed.0.clone(), out))))
                        .with_steps_of(self);
                    if let Some(loc) = self.pure_loc_raw() {
                        pattern.set_pure_loc_raw(Some(loc));
                    }
                    return pattern;
                }
                Node::IntRange {
                    start,
                    count,
                    loc0,
                    loc1,
                    named_id,
                    step_neg,
                } if let Some((n0, n1)) = append_int_range_locs(*loc0, *loc1, locs) => {
                    return Pattern::of(Node::IntRange {
                        start: *start,
                        count: *count,
                        loc0: n0,
                        loc1: n1,
                        named_id: *named_id,
                        step_neg: *step_neg,
                    })
                    .with_steps_of(self);
                }
                _ => {}
            }
        }
        let locs: Arc<[(usize, usize)]> = Arc::from(locs);
        let mut pattern =
            Pattern::of(Node::AddContext(Box::new((self.clone(), locs)))).with_steps_of(self);
        if let Some(loc) = self.pure_loc_raw() {
            pattern.set_pure_loc_raw(Some(loc));
        }
        pattern
    }

    /// Append a [`Locations`] list without intermediate `Vec` allocation.
    pub(crate) fn with_added_locations(&self, locs: &Locations) -> Self {
        if let Some(located) = locs.with_slice(|slice| sequence::located(self, slice)) {
            return located.with_steps_of(self);
        }
        if !self.has_runtime_settings() {
            match self.node_kind() {
                Node::Pure(value) => {
                    let mut pattern =
                        Pattern::of(Node::PureAt(Box::new((value.clone(), locs.clone()))))
                            .with_steps_of(self);
                    if let Some(loc) = self.pure_loc_raw() {
                        pattern.set_pure_loc_raw(Some(loc));
                    }
                    return pattern;
                }
                Node::PureAt(boxed) => {
                    let mut out = boxed.1.clone();
                    out.append(locs);
                    let mut pattern = Pattern::of(Node::PureAt(Box::new((boxed.0.clone(), out))))
                        .with_steps_of(self);
                    if let Some(loc) = self.pure_loc_raw() {
                        pattern.set_pure_loc_raw(Some(loc));
                    }
                    return pattern;
                }
                _ => {}
            }
        }
        let locs = locs.to_arc_slice();
        let mut pattern =
            Pattern::of(Node::AddContext(Box::new((self.clone(), locs)))).with_steps_of(self);
        if let Some(loc) = self.pure_loc_raw() {
            pattern.set_pure_loc_raw(Some(loc));
        }
        pattern
    }

    pub fn with_ui_visual_slot(&self, slot: u8) -> Self {
        if !sequence::builds_each_node() && !self.is_volatile() {
            match self.node_kind() {
                Node::Silence if !self.has_runtime_settings() => return self.clone(),
                Node::Sequence(layers) if sequence::folds(self) => {
                    return Pattern::of(Node::Sequence(sequence::with_ui_visual_slot_layers(
                        layers, slot,
                    )))
                    .with_steps_of(self);
                }
                _ => {}
            }
        }
        let mut pattern = Pattern::of(Node::UiVisual(Box::new(UiVisualNode {
            inner: self.clone(),
            slot,
            cache: std::array::from_fn(|_| std::sync::OnceLock::new()),
        })))
        .with_steps_of(self);
        if let Some(loc) = self.pure_loc_raw() {
            pattern.set_pure_loc_raw(Some(loc));
        }
        pattern
    }

    /// Owned variant of [`Self::with_ui_visual_slot`] that updates unshared
    /// `Sequence` nodes in place without cloning layers or `NodeCell`s.
    pub fn with_ui_visual_slot_owned(mut self, slot: u8) -> Self {
        if !sequence::builds_each_node() && !self.is_volatile() {
            match self.node_kind() {
                Node::Silence if !self.has_runtime_settings() => return self,
                Node::Sequence(_)
                    if sequence::folds(&self) && Arc::strong_count(&self.node) == 1 =>
                {
                    if let Some(own) = Arc::get_mut(&mut self.node)
                        && let Node::Sequence(layers) = &mut own.kind
                    {
                        if !sequence::with_ui_visual_slot_in_place(layers, slot) {
                            *layers = sequence::with_ui_visual_slot_layers(layers, slot);
                        }
                        return self;
                    }
                }
                _ => {}
            }
        }
        self.with_ui_visual_slot(slot)
    }

    /// Collect this pattern's values into a one-element `Value::List`, the seed
    /// of `register()`'s positional-argument fold.
    pub fn fmap_collect(&self) -> Self {
        self.fmap(|v| Value::list(vec![v.clone()]))
    }

    /// Append the next positional argument via `appLeft`.
    pub fn app_left_collect(&self, other: Pattern) -> Self {
        Pattern::of(Node::AppLeft(Box::new(AppLeftNode(
            self.clone(),
            other,
            ValueReading::default(),
            Combine::with(Arc::new(|acc: &Value, v: &Value| match acc {
                Value::List(xs) => {
                    let mut out = xs.to_vec();
                    out.push(v.clone());
                    Value::list(out)
                }
                other => Value::list_from_arc(Arc::from([other.clone(), v.clone()])),
            })),
            LookupFlow::Infer,
        ))))
    }

    /// Apply a native curried-value equivalent with `appLeft` alignment.
    ///
    /// `result._steps = this._steps` - structure comes from the function side.
    pub fn app_left_with(
        &self,
        other: Pattern,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
    ) -> Self {
        self.app_left_with_lookup(other, combine, LookupFlow::Infer)
    }

    pub fn app_left_with_lookup(
        &self,
        other: Pattern,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
        lookup_flow: LookupFlow,
    ) -> Self {
        self.app_left_by(other, Combine::with(Arc::new(combine)), lookup_flow)
    }

    /// [`Self::app_left_with_lookup`] of [`compose::compose_op`] for `op`.
    pub fn app_left_composed(
        &self,
        other: Pattern,
        op: compose::ComposeOp,
        lookup_flow: LookupFlow,
    ) -> Self {
        self.app_left_by(other, Combine::Composed(op), lookup_flow)
    }

    /// Owned variant of [`Self::app_left_composed`] that mutates a uniquely-owned
    /// single-layer sequence in place when `sequence::applied_owned` succeeds.
    pub fn app_left_composed_owned(
        self,
        other: Pattern,
        op: compose::ComposeOp,
        lookup_flow: LookupFlow,
    ) -> Self {
        let steps = self.steps();
        if self.is_rest() && sequence::folds(&self) {
            return self;
        }
        let combine = Combine::Composed(op);
        match sequence::applied_owned(self, &other, &combine, lookup_flow) {
            Ok(applied) => applied.with_steps(steps),
            Err(this) => this.app_left_by(other, combine, lookup_flow),
        }
    }

    pub(crate) fn app_left_by(
        &self,
        other: Pattern,
        combine: Combine,
        lookup_flow: LookupFlow,
    ) -> Self {
        let steps = self.steps();
        // No hap meets a value, so the values are not asked.
        if self.is_rest() && sequence::folds(self) {
            return self.clone();
        }
        let folded =
            sequence::applied(self, &other, &combine, lookup_flow).or_else(|| match &combine {
                Combine::Composed(op) => self
                    .picked_applied(&other, *op, lookup_flow)
                    .or_else(|| self.set_constant(&other, *op, lookup_flow)),
                Combine::SetMerged | Combine::With(_) => None,
            });
        if let Some(applied) = folded {
            return applied.with_steps(steps);
        }
        if let Some(met) = self.met_constant(&other, &combine, lookup_flow) {
            return met.with_steps(steps);
        }
        if !sequence::builds_each_node()
            && !self.is_volatile()
            && matches!(combine, Combine::Composed(compose::ComposeOp::Set))
            && lookup_flow == LookupFlow::Set
            && !other.has_runtime_settings()
            && other.is_cacheable()
            && let Some((inner_ref, key_id)) = match other.node_kind() {
                Node::ControlMap(cell, flags, key_id, false) if *key_id != 0 => {
                    Some((NodeRef::Plain(cell, *flags), *key_id))
                }
                Node::ControlMapBoxed(boxed) if !boxed.2 && boxed.1 != 0 => {
                    Some((NodeRef::Pat(&boxed.0), boxed.1))
                }
                _ => None,
            }
            && let Some((wave, steps_per_cycle, math_inside)) =
                if let Some((wave, math_inside)) = fused::Wave::of_inline_ref(inner_ref) {
                    Some((wave, 0u32, math_inside))
                } else if !fused::is_unfused()
                    && controls::builtin_name_str_by_id(key_id) != Some("value")
                    && let Some((wave, steps_per_cycle, math_inside)) =
                        fused::Wave::of_inline_steps_ref(inner_ref)
                {
                    Some((wave, steps_per_cycle, math_inside))
                } else {
                    None
                }
        {
            return self
                .clone()
                .apply_inline_wave(key_id, wave, steps_per_cycle, math_inside);
        }
        Pattern::of(Node::AppLeft(Box::new(AppLeftNode(
            self.clone(),
            other,
            ValueReading::default(),
            combine,
            lookup_flow,
        ))))
        .with_steps(steps)
    }

    /// Attach a validated inline wave directly to `self` as [`Node::AppWave`],
    /// stacking up to 8 wave controls into one node without building
    /// intermediate signal or control patterns.
    pub(crate) fn apply_inline_wave(
        mut self,
        key_id: u16,
        wave: fused::Wave,
        steps_per_cycle: u32,
        math_inside: bool,
    ) -> Self {
        if self.is_rest() && sequence::folds(&self) {
            return self;
        }
        let steps = self.steps();
        let item = AppWaveItem {
            wave,
            steps: steps_per_cycle,
            key_id,
            math_inside,
        };
        let can_append = matches!(
            self.node_kind(),
            Node::AppWave(existing)
                if !self.has_runtime_settings()
                    && self.pure_loc_raw().is_none()
                    && self.keeps_runtime_over(&existing.inner)
                    && existing.waves.len() < 8
        );
        if can_append {
            if let Some(cell) = Arc::get_mut(&mut self.node)
                && let Node::AppWave(existing) = &mut cell.kind
            {
                let mut combined = Vec::with_capacity(existing.waves.len() + 1);
                let prev = std::mem::replace(&mut existing.waves, AppWaves::Many(Box::new([])));
                match prev {
                    AppWaves::One(one) => combined.push(one),
                    AppWaves::Many(many) => combined.extend(many.into_vec()),
                }
                combined.push(item);
                existing.waves = AppWaves::Many(combined.into_boxed_slice());
                return self.with_steps(steps);
            }
            if let Node::AppWave(existing) = self.node_kind() {
                let mut combined = Vec::with_capacity(existing.waves.len() + 1);
                combined.extend(existing.waves.iter().cloned());
                combined.push(item);
                return Pattern::of(Node::AppWave(Box::new(AppWaveNode {
                    inner: existing.inner.clone(),
                    waves: AppWaves::Many(combined.into_boxed_slice()),
                })))
                .with_steps(steps);
            }
        }
        Pattern::of(Node::AppWave(Box::new(AppWaveNode {
            inner: self,
            waves: AppWaves::One(item),
        })))
        .with_steps(steps)
    }

    /// This pattern with each hap met by the constant that `other` is, by
    /// `combine`: one node for a run of such meetings, as
    /// `sine.range(200, 2000)` makes two, and no node for the constant.
    /// `None` when the graph of `other` does not say its constant, and for
    /// a constant with a place of its own in the text: that application
    /// keeps its node, and a query finds the constant.
    fn met_constant(&self, other: &Pattern, combine: &Combine, flow: LookupFlow) -> Option<Self> {
        if other.pure_loc_raw().is_some() {
            return None;
        }
        let (value, context) = sequence::constant_value_at(other)?;
        // A handle of the op, or of the function.
        let apply = combine.clone();
        Some(self.met([RunMeeting::of(apply, flow, value, context)]))
    }

    /// This pattern with each hap met by each of `numbers` in turn, as a
    /// composition `in` of a `pure` of each makes it: one node for the run,
    /// with no pattern of a number. `None` when the pattern takes no run
    /// so, for the caller to compose each number: a sequence, a constant
    /// and a pick fold each application into their steps.
    pub(crate) fn met_numbers(&self, numbers: &[(compose::ComposeOp, f64)]) -> Option<Self> {
        if !self.meets_constants() || numbers.is_empty() {
            return None;
        }
        // An op of arithmetic makes a number of two numbers.
        if !numbers
            .iter()
            .all(|(op, _)| op.of_numbers(0.0, 0.0).is_some())
        {
            return None;
        }
        let meetings = numbers.iter().map(|(op, number)| {
            let apply = Combine::Composed(*op);
            RunMeeting::of(
                apply,
                op.lookup_flow(),
                Value::F64(*number),
                Locations::new(),
            )
        });
        Some(self.met(meetings).with_steps(self.steps()))
    }

    /// Whether an application of a constant on this pattern is a meeting
    /// in a node of its own, with nothing to try before it: silence, a
    /// sequence, a constant and a pick fold an application into their
    /// steps.
    fn meets_constants(&self) -> bool {
        let folds = self.is_rest()
            || sequence::holds_steps(self)
            || matches!(self.node_kind(), Node::Pick(..));
        !folds && !sequence::builds_each_node()
    }

    /// This pattern with each hap met by the `set` of a constant of
    /// `control`, a map of controls of data at `context`: the node that the
    /// `set` of a `pure` of the control gives of a pattern that
    /// [meets constants](Self::meets_constants), with no pattern of the
    /// constant.
    fn met_control(&self, control: &Value, context: Locations) -> Self {
        let set = Combine::Composed(compose::ComposeOp::Set);
        // A handle of the control, for the run to hold.
        let meeting = RunMeeting::of(set, LookupFlow::Set, control.clone(), context);
        self.met([meeting]).with_steps(self.steps())
    }

    /// Whether each constant control on this pattern is a meeting in a node
    /// of its own: the pattern [meets constants](Self::meets_constants),
    /// and it is of no kind that takes a control where it is. A row of
    /// controls is then one meeting.
    pub(crate) fn meets_each_control(&self) -> bool {
        !self.takes_constants() && self.meets_constants()
    }

    /// This pattern with each hap met by the `set` of a constant of each
    /// control of a row in turn, as one meeting: `union` is the union of
    /// the row, a map of controls of data, at `context`, the locations of
    /// each control, the last one first. See [`Self::met_control`].
    fn met_row(&self, union: &Value, context: Locations, multi: bool) -> Self {
        let apply = if multi {
            Combine::SetMerged
        } else {
            Combine::Composed(compose::ComposeOp::Set)
        };
        let meeting = RunMeeting::of(apply, LookupFlow::Set, union.clone(), context);
        self.met([meeting]).with_steps(self.steps())
    }

    fn met_row_owned(self, union: &Value, context: Locations, multi: bool) -> Self {
        let apply = if multi {
            Combine::SetMerged
        } else {
            Combine::Composed(compose::ComposeOp::Set)
        };
        let steps = self.steps();
        let meeting = RunMeeting::of(apply, LookupFlow::Set, union.clone(), context);
        self.met_owned([meeting]).with_steps(steps)
    }

    /// Set a constant of `control`, a map of controls at `context`, on
    /// this pattern, as the `set` of a `pure` of the control at those
    /// places does: where the pattern takes it, in a node of its own where
    /// the pattern meets constants so, and by that `set` otherwise.
    fn set_in_turn(&mut self, control: Value, context: Locations) {
        if self.meets_constants() && control.is_data(0) {
            if !self.set_control(&control, &context) {
                let steps = self.steps();
                let taken = std::mem::replace(self, silence());
                let set = Combine::Composed(compose::ComposeOp::Set);
                let meeting = RunMeeting::of(set, LookupFlow::Set, control, context);
                *self = taken.met_owned([meeting]).with_steps(steps);
            }
            return;
        }
        let mut constant = pure(control);
        if !context.is_empty() {
            constant = constant.with_added_locations(&context);
        }
        let set = Combine::Composed(compose::ComposeOp::Set);
        *self = self.app_left_by(constant, set, LookupFlow::Set);
    }

    /// This pattern with each hap met by the constants of `meetings` in
    /// turn, as one node. A run joins the run of its receiver: each hap of
    /// the pattern under both meets the constants of both in turn. The node
    /// takes the runtime of the pattern under it, so a receiver that binds
    /// another one stays a pattern of its own.
    fn met(&self, meetings: impl IntoIterator<Item = RunMeeting>) -> Self {
        let (base, meetings) = match self.node_kind() {
            Node::Constants(node) if self.same_runtime_owner(&node.0) => (
                node.0.clone(),
                ConstantsMeetings::from_iter(node.1.iter().cloned().chain(meetings)),
            ),
            _ => (self.clone(), ConstantsMeetings::from_iter(meetings)),
        };
        Pattern::of(Node::Constants(Box::new(ConstantsNode(base, meetings))))
    }

    /// Owned variant of [`Self::met`] that extends a uniquely-owned
    /// `Node::Constants` in place with zero node or box allocations.
    fn met_owned(mut self, meetings: impl IntoIterator<Item = RunMeeting>) -> Self {
        if let Node::Constants(node) = self.node_kind()
            && self.same_runtime_owner(&node.0)
            && Arc::strong_count(&self.node) == 1
            && let Some(cell) = self.own_node()
            && let Node::Constants(node) = &mut cell.kind
        {
            node.1.extend_in_place(meetings);
            return self;
        }
        self.met(meetings)
    }

    /// This pattern with the constant that `other` is set on it by
    /// [`Self::set_control`], when `other` is a constant of a plain value
    /// that `op` sets and this pattern takes it.
    fn set_constant(
        &self,
        other: &Pattern,
        op: compose::ComposeOp,
        flow: LookupFlow,
    ) -> Option<Self> {
        if op != compose::ComposeOp::Set || flow != LookupFlow::Set {
            return None;
        }
        let (control, context) = sequence::constant_value_at(other)?;
        // A handle of this node: the set then writes a copy.
        let mut set = self.clone();
        set.set_control(&control, &context).then_some(set)
    }

    /// Set the constant `control`, a map of controls at `context`, on this
    /// pattern where it is, as the `set` of a constant of it gives. `false`,
    /// with this pattern as it was, when the pattern does not take it so.
    ///
    /// A sequence takes the control in each step. See
    /// [`sequence::Layer::set_here`].
    ///
    /// The `set` of a constant map of controls, the last meeting of a run
    /// of constants, takes it as one more control: one meeting then stands
    /// for each. Its constant is the union of the controls, which a meeting
    /// takes where a hap of plain controls is, as the applications in turn
    /// do. The meeting holds each control too, so a hap that does not meet
    /// in place, such as a string, meets them one by one. A chain of
    /// constant controls on a pattern that is no sequence, such as
    /// `.fast(2).lpq(8).release(0.2)`, is then one node for the chain,
    /// where it was two for each control.
    ///
    /// The node is written where no other handle holds it, as the receiver
    /// of the next call of a chain is. Another holder keeps its node, and
    /// this handle takes a copy.
    fn set_control(&mut self, control: &Value, context: &Locations) -> bool {
        let Some((waves, under)) = self.taking(control) else {
            return false;
        };
        // A handle of the control, as the one control of its row.
        let alone = || vec![(control.clone(), context.clone())];
        self.write(waves, &under, context.clone(), &Apart::new(1, &alone));
        true
    }

    /// Whether [`Self::set_control`] sets `control` on this pattern: under
    /// the applications of waves that end at it, or at its own node.
    fn takes_control(&self, control: &Value) -> bool {
        self.taking(control).is_some()
    }

    /// Where this pattern [takes](Self::takes_control) `control`: the count
    /// of applications of waves over the node that takes it, and the
    /// control as that node takes it. `None` when the pattern does not
    /// take the control.
    fn taking<'c>(&self, control: &'c Value) -> Option<(usize, std::borrow::Cow<'c, Value>)> {
        use std::borrow::Cow;
        match self.control_under_waves(control) {
            Some((waves, under, pattern)) => pattern
                .takes_here(&under)
                .then_some((waves, Cow::Owned(under))),
            None => self
                .takes_here(control)
                .then_some((0, Cow::Borrowed(control))),
        }
    }

    /// [`Self::set_control`] of `control`, the union of the controls
    /// `apart` that a chain sets one after the other, at `context`, the
    /// locations of each, on a pattern that [takes](Self::takes_control)
    /// it: no node under it is asked again. A node that holds each control
    /// of a row apart reads `apart`: see [`Self::apart_under`].
    fn write_row(&mut self, control: &Value, context: Locations, apart: &Apart<'_>) {
        // The control and the count alone, so that the pattern is free to
        // write.
        let under = self
            .control_under_waves(control)
            .map(|(waves, under, _)| (waves, under));
        match under {
            Some((waves, under)) => self.write(waves, &under, context, apart),
            None => self.write(0, control, context, apart),
        }
    }

    /// Each control of the row `apart` as a pattern under waves takes it:
    /// with the names of those waves first, as `control` holds them, which
    /// is the union of the row under the waves.
    fn apart_under<'a>(
        control: &'a Value,
        apart: &'a [(Value, Locations)],
    ) -> impl ExactSizeIterator<Item = (Value, Locations)> + 'a {
        let named = |key: &Key| {
            apart
                .iter()
                .any(|(one, _)| one.as_object().is_some_and(|one| one.contains_key(key)))
        };
        let mut names = OrderedMap::new();
        for (key, place) in control.as_object().into_iter().flatten() {
            if !named(key) {
                names.set_data(key, place.clone());
            }
        }
        apart.iter().map(move |(one, at)| {
            // A handle of the control, or a map of the names with it.
            let mut each = match names.is_empty() {
                true => return (one.clone(), at.clone()),
                false => Value::Object(names.clone()),
            };
            compose::compose_data_in_place(compose::ComposeOp::Set, &mut each, one);
            (each, at.clone())
        })
    }

    /// Write `control` on the pattern under `waves` applications of waves,
    /// which takes it at its own node: each application stays over the
    /// control. See [`Self::control_under_waves`] and [`Self::write_row`].
    fn write(&mut self, waves: usize, control: &Value, context: Locations, apart: &Apart<'_>) {
        let steps = self.steps();
        // The runtime that this handle binds, which the node does not say.
        let runtime = self.runtime_settings().cloned();
        let Some(cell) = self.own_node() else {
            debug_assert!(
                false,
                "a pattern that takes a control is of a kind to write"
            );
            return;
        };
        match &mut cell.kind {
            Node::AppLeft(pair) if waves > 0 => {
                pair.0.write(waves - 1, control, context, apart);
                // What a query read of the functions before is not their
                // haps now.
                pair.2.forget_functions();
            }
            Node::AppWave(app) if waves > 0 => {
                app.inner.write(waves - 1, control, context, apart);
            }
            Node::Sequence(layers) => sequence::set_here(layers, control, context),
            Node::Degrade(boxed) => boxed.0.write(0, control, context, apart),
            Node::Constants(node) => {
                let last = node.1.last_mut().expect("a run of one meeting or more");
                last.set_after(control, context);
            }
            Node::Pick(pick) => {
                // The locations of the constant come first on each hap,
                // through the selector.
                let ok = sequence::selecting_in_place(
                    &mut pick.selector,
                    &context,
                    Self::picks_whole_cycles(pick.mode),
                );
                debug_assert!(ok, "a selector that takes the locations of a control");
                // The selector holds the locations of the row.
                let bare = || {
                    let each = apart.each().iter();
                    each.map(|(one, _)| (one.clone(), Locations::new()))
                        .collect()
                };
                let apart = Apart::new(apart.count, &bare);
                let mut dress_cache = sequence::RenoteDressCache::default();
                for picked in pick
                    .lookup
                    .patterns_mut()
                    .filter(|picked| !picked.is_rest())
                {
                    if matches!(picked.node_kind(), Node::Sequence(_))
                        && sequence::folds(picked)
                        && let Some(p_cell) = picked.own_node()
                        && let Node::Sequence(layers) = &mut p_cell.kind
                    {
                        sequence::set_here_cached(
                            layers,
                            control,
                            Locations::new(),
                            &mut dress_cache,
                        );
                        continue;
                    }
                    picked.write_row(control, Locations::new(), &apart);
                }
            }
            Node::Stack(patterns) => {
                for pattern in patterns.iter_mut().filter(|pattern| !pattern.is_rest()) {
                    // The locations of the control, for each pattern.
                    if let Some((waves, under)) = pattern.taking(control) {
                        pattern.write(waves, &under, context.clone(), apart);
                        continue;
                    }
                    // A pattern that takes no control meets a constant of
                    // each control of the row, as it met them in the stack.
                    if pattern.meets_each_control() && control.is_data(0) {
                        *pattern = pattern.met_row(control, context.clone(), apart.has_many());
                        continue;
                    }
                    let row = Self::apart_under(control, apart.each());
                    for (each, at) in row {
                        pattern.set_in_turn(each, at);
                    }
                }
            }
            Node::StackPure(cells) => {
                if let Some(patterns) = mutate_pure_cells(cells, |pattern| {
                    if let Some((waves, under)) = pattern.taking(control) {
                        pattern.write(waves, &under, context.clone(), apart);
                    } else if pattern.meets_each_control() && control.is_data(0) {
                        *pattern = pattern.met_row(control, context.clone(), apart.has_many());
                    } else {
                        let row = Self::apart_under(control, apart.each());
                        for (each, at) in row {
                            pattern.set_in_turn(each, at);
                        }
                    }
                }) {
                    cell.kind = Node::Stack(patterns);
                }
            }
            // The control goes to the pattern under the structure.
            Node::FilterValues(boxed) => boxed.0.write(0, control, context, apart),
            Node::AppRight(node) => node.0.write_row(control, context, apart),
            _ => unreachable!("a node that takes a control is of a kind that `own_node` writes"),
        }
        // The handle that a constructor gives of the node, as the fold of
        // a `set` gives one, with the runtime of this handle.
        let node = Arc::clone(&self.node);
        *self = Self::holding(node).with_steps(steps);
        if let Some(runtime) = runtime {
            self.attach_runtime_settings(runtime);
        }
    }

    /// Whether the node of this pattern takes `control`: see
    /// [`Self::takes_control`].
    fn takes_here(&self, control: &Value) -> bool {
        match self.node_kind() {
            Node::Sequence(_) => sequence::takes_set(self, control),
            // The last constant of a run takes the control into its map.
            // The new handle binds what the pattern under the run binds, so
            // this handle must bind the same.
            Node::Constants(node) => {
                self.same_runtime_owner(&node.0)
                    && node
                        .1
                        .last()
                        .is_some_and(|last| last.takes_set_after(control))
            }
            // Each step of a picked sequence meets the constant where the
            // step is, in the picked pattern as in the pick. A squeeze
            // orders the context and the keys another way.
            Node::Pick(pick) => {
                pick.mode != JoinMode::Squeeze
                    && sequence::folds(self)
                    && sequence::is_plain_selector(
                        &pick.selector,
                        Self::picks_whole_cycles(pick.mode),
                    )
                    && pick.lookup.patterns().all(|picked| {
                        (picked.is_rest() && sequence::folds(picked))
                            || (sequence::lies_in_cycles(picked) && picked.takes_control(control))
                    })
            }
            // A stack answers the haps of each pattern as they are, in
            // their order, so the constant meets each hap in its pattern
            // as it meets it in the stack. One pattern that takes the
            // control repays it: each other pattern meets the constant
            // through a node of its own.
            Node::Stack(patterns) => patterns
                .iter()
                .any(|pattern| !pattern.is_rest() && pattern.takes_control(control)),
            Node::StackPure(cells) => cells.iter().any(|cell| match &cell.kind {
                Node::Silence => false,
                Node::Sequence(layers) => sequence::takes_set_layers(layers, control),
                _ => plain_pattern(cell, 0).takes_control(control),
            }),
            Node::FilterValues(..) => self
                .under_structure()
                .is_some_and(|functions| functions.takes_control(control)),
            Node::Degrade(boxed) if matches!(boxed.0.node_kind(), Node::Sequence(_)) => {
                self.keeps_runtime_over(&boxed.0) && sequence::takes_set(&boxed.0, control)
            }
            _ => false,
        }
    }

    /// The applications of waves that end at this pattern, as a count, the
    /// control to set under them, so that the applications over it are the
    /// `set` of `control` after them, and the pattern under them. `None`
    /// when this pattern ends no such application that the control goes
    /// under.
    ///
    /// The application of a control of a [wave](table::Wave) sets one
    /// number under one name and gives a hap nothing more. A `set` of other
    /// names after it is then the same `set` before it: each sets its own
    /// names. The name of each wave comes first in the control, with a
    /// number that the wave replaces, so that the name keeps its place in
    /// each map. A chain such as `.lpf(sine.range(200, 2000)).lpq(8)` then
    /// sets its constants where the steps are, and a query meets the wave
    /// alone.
    fn control_under_waves(&self, control: &Value) -> Option<(usize, Value, &Self)> {
        // A union with a map of `value` alone keeps the other side as it
        // is, and its meeting still gives the hap a lookup: that `set`
        // keeps its node.
        let controls = control
            .as_object()
            .filter(|controls| !compose::union_drops(controls))?;
        // The names of the waves, the outermost first. Keep up to 8 inline on
        // the stack to avoid per-call heap allocations on modulated chains.
        let mut inline_names: [Option<Key>; 8] = Default::default();
        let mut inline_len = 0usize;
        let mut extra_names: Vec<Key> = Vec::new();
        let mut depth = 0usize;
        let mut under = self;
        loop {
            match under.node_kind() {
                Node::AppWave(app) => {
                    if !under.keeps_runtime_over(&app.inner) {
                        return None;
                    }
                    let mut ok = true;
                    let saved_inline = inline_len;
                    let saved_extra = extra_names.len();
                    for item in app.waves.iter().rev() {
                        let Some(key) = controls::builtin_key_ref_by_id(item.key_id) else {
                            ok = false;
                            break;
                        };
                        if controls.contains_key(key) {
                            return None;
                        }
                        if inline_len < inline_names.len() {
                            inline_names[inline_len] = Some(key.clone());
                            inline_len += 1;
                        } else {
                            extra_names.push(key.clone());
                        }
                    }
                    if !ok {
                        for slot in &mut inline_names[saved_inline..inline_len] {
                            *slot = None;
                        }
                        inline_len = saved_inline;
                        extra_names.truncate(saved_extra);
                        break;
                    }
                    depth += 1;
                    under = &app.inner;
                }
                Node::AppLeft(pair)
                    if matches!(pair.3, Combine::Composed(compose::ComposeOp::Set))
                        && pair.4 == LookupFlow::Set =>
                {
                    let Some(name) = table::wave_name(&pair.1) else {
                        break;
                    };
                    if controls.contains_key(&name) || !under.keeps_runtime_over(&pair.0) {
                        return None;
                    }
                    if inline_len < inline_names.len() {
                        inline_names[inline_len] = Some(name);
                        inline_len += 1;
                    } else {
                        extra_names.push(name);
                    }
                    depth += 1;
                    under = &pair.0;
                }
                _ => break,
            }
        }
        if inline_len == 0 || !control.is_data(0) {
            return None;
        }
        // Build the combined map in one allocation: innermost wave keys first
        // (deduplicated, with placeholder 0.0 values) followed by `controls`
        // (guaranteed disjoint by the `contains_key` checks above).
        if extra_names.is_empty() {
            let mut dedup: [Option<Key>; 8] = Default::default();
            let mut dedup_len = 0usize;
            for name in inline_names[..inline_len].iter_mut().rev() {
                let key = name.take().expect("filled inline wave name");
                if !dedup[..dedup_len].iter().flatten().any(|seen| seen == &key) {
                    dedup[dedup_len] = Some(key);
                    dedup_len += 1;
                }
            }
            let wave_entries = dedup[..dedup_len]
                .iter_mut()
                .map(|slot| (slot.take().expect("dedup key"), Value::F64(0.0)));
            let control_entries = controls.into_iter().map(|(k, v)| (k.clone(), v.clone()));
            let set = Value::Object(OrderedMap::of_keys(wave_entries.chain(control_entries)));
            return Some((depth, set, under));
        }
        // The innermost wave sets its name first.
        let mut places = OrderedMap::new();
        for name in extra_names.iter().rev() {
            places.set_data(name, Value::F64(0.0));
        }
        for name in inline_names[..inline_len].iter().rev().flatten() {
            places.set_data(name, Value::F64(0.0));
        }
        let mut set = Value::Object(places);
        compose::compose_data_in_place(compose::ComposeOp::Set, &mut set, control);
        Some((depth, set, under))
    }

    /// Whether a pick that joins by `mode` moves a picked pattern by whole
    /// cycles alone, so that the cycles of the pattern stay the cycles of
    /// the query. See [`sequence::selecting`].
    fn picks_whole_cycles(mode: JoinMode) -> bool {
        matches!(mode, JoinMode::Reset | JoinMode::Restart)
    }

    /// The node of this handle, to write where it is. Where another handle
    /// holds the node, this handle takes a node of its own first, with a
    /// handle of each child. `None` for a kind that [`Self::set_control`]
    /// does not write.
    fn own_node(&mut self) -> Option<&mut NodeCell> {
        if Arc::get_mut(&mut self.node).is_none() {
            let copy = match self.node_kind() {
                Node::Sequence(layers) => Node::Sequence(Arc::clone(layers)),
                Node::AppLeft(pair) => Node::AppLeft(Box::new(AppLeftNode(
                    pair.0.clone(),
                    pair.1.clone(),
                    ValueReading::default(),
                    pair.3.clone(),
                    pair.4,
                ))),
                Node::AppWave(app) => Node::AppWave(Box::new(AppWaveNode {
                    inner: app.inner.clone(),
                    waves: app.waves.clone(),
                })),
                // A handle of the pattern under the run, and a copy of the run.
                Node::Constants(node) => {
                    Node::Constants(Box::new(ConstantsNode(node.0.clone(), node.1.clone())))
                }
                Node::Pick(pick) => Node::Pick(Box::new(PickNode {
                    selector: pick.selector.clone(),
                    lookup: pick.lookup.clone(),
                    index_mode: pick.index_mode,
                    mode: pick.mode,
                })),
                // A handle of each pattern of the stack.
                Node::Stack(patterns) => Node::Stack(patterns.clone()),
                Node::StackPure(cells) => Node::StackPure(cells.clone()),
                // A handle of each side of a structure, and of its filter.
                Node::FilterValues(boxed) => {
                    Node::FilterValues(Box::new((boxed.0.clone(), Arc::clone(&boxed.1))))
                }
                Node::Degrade(boxed) => {
                    Node::Degrade(Box::new((boxed.0.clone(), boxed.1, boxed.2)))
                }
                Node::AppRight(node) => Node::AppRight(Box::new(AppNode(
                    node.0.clone(),
                    node.1.clone(),
                    node.2.clone(),
                    node.3,
                ))),
                _ => return None,
            };
            self.node = Self::of(copy).node;
        }
        Arc::get_mut(&mut self.node)
    }

    /// Apply a native curried-value equivalent with `appRight` alignment.
    ///
    /// `result._steps = pat_val._steps` - structure comes from the value side.
    pub fn app_right_with(
        &self,
        other: Pattern,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
    ) -> Self {
        self.app_right_with_lookup(other, combine, LookupFlow::Infer)
    }

    pub fn app_right_with_lookup(
        &self,
        other: Pattern,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
        lookup_flow: LookupFlow,
    ) -> Self {
        self.app_right_by(other, Combine::with(Arc::new(combine)), lookup_flow)
    }

    /// [`Self::app_right_with_lookup`] of [`compose::compose_op`] for `op`.
    pub fn app_right_composed(
        &self,
        other: Pattern,
        op: compose::ComposeOp,
        lookup_flow: LookupFlow,
    ) -> Self {
        self.app_right_by(other, Combine::Composed(op), lookup_flow)
    }

    pub(crate) fn app_right_by(
        &self,
        other: Pattern,
        combine: Combine,
        lookup_flow: LookupFlow,
    ) -> Self {
        let steps = other.steps();
        if let Some(applied) = sequence::applied_out(self, &other, &combine, lookup_flow) {
            return applied.with_steps(steps);
        }
        Pattern::of(Node::AppRight(Box::new(AppNode(
            self.clone(),
            other,
            combine,
            lookup_flow,
        ))))
        .with_steps(steps)
    }

    /// `appBoth` - both sides queried over the same span, wholes intersected.
    ///
    /// `result._steps = lcm(pat_val._steps, pat_func._steps)`.
    pub fn app_both_with(
        &self,
        other: Pattern,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
    ) -> Self {
        self.app_both_with_lookup(other, combine, LookupFlow::Infer)
    }

    pub fn app_both_with_lookup(
        &self,
        other: Pattern,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
        lookup_flow: LookupFlow,
    ) -> Self {
        self.app_both_by(other, Combine::with(Arc::new(combine)), lookup_flow)
    }

    /// [`Self::app_both_with_lookup`] of [`compose::compose_op`] for `op`.
    pub fn app_both_composed(
        &self,
        other: Pattern,
        op: compose::ComposeOp,
        lookup_flow: LookupFlow,
    ) -> Self {
        self.app_both_by(other, Combine::Composed(op), lookup_flow)
    }

    pub(crate) fn app_both_by(
        &self,
        other: Pattern,
        combine: Combine,
        lookup_flow: LookupFlow,
    ) -> Self {
        let steps = match (other.steps(), self.steps()) {
            (Some(a), Some(b)) => match a.checked_lcm(b) {
                Some(steps) => Some(steps),
                None => {
                    return query_limit_pattern(mark_stepwise_refusal(
                        QueryLimit::NativeFraction {
                            operation: "appBoth",
                        },
                    ));
                }
            },
            (Some(a), None) => Some(a),
            (None, b) => b,
        };
        Pattern::of(Node::AppBoth(Box::new(AppNode(
            self.clone(),
            other,
            combine,
            lookup_flow,
        ))))
        .with_steps(steps)
    }

    /// Map each value to a **provably pure** pattern.
    ///
    /// The closure's return type is the proof: a `PurePattern` can only be
    /// obtained from a pattern already classified pure, so this cannot smuggle
    /// in a callback. Purity is therefore inherited from the accumulator
    /// *soundly*, rather than by a promise that can lie.
    pub fn fmap_to_pure_pattern(
        &self,
        f: impl Fn(&Value) -> purity::PurePattern + Send + Sync + 'static,
    ) -> Self {
        // `fmap` carries `_steps`; these are `fmap` with a pattern-valued
        // result, so they must too - `stepJoin` reads the outer step count.
        let steps = self.steps();
        Pattern::of(Node::PatternOfPure(Box::new((
            self.clone(),
            Arc::new(f),
            ConstructionMemo::default(),
        ))))
        .with_steps(steps)
    }

    /// [`fmap_to_pure_pattern`] for a body that captures nothing: one
    /// closure object per body, so the same map built by a later
    /// evaluation is the same payload. See [`interned_closure`].
    pub fn fmap_to_pure_pattern_static(&self, body: fn(&Value) -> purity::PurePattern) -> Self {
        type WeakToPurePatternFn =
            std::sync::Weak<dyn Fn(&Value) -> purity::PurePattern + Send + Sync>;
        type ToPurePatternPool =
            std::sync::Mutex<std::collections::HashMap<usize, WeakToPurePatternFn>>;
        static POOL: std::sync::OnceLock<ToPurePatternPool> = std::sync::OnceLock::new();
        let steps = self.steps();
        let interned = interned_closure(POOL.get_or_init(Default::default), body as usize, || {
            let mapped: ToPurePatternFn = Arc::new(body);
            mapped
        });
        Pattern::of(Node::PatternOfPure(Box::new((
            self.clone(),
            interned,
            ConstructionMemo::default(),
        ))))
        .with_steps(steps)
    }

    /// [`Self::fmap_to_pure_pattern`] for a registered combinator's pure
    /// body, with the receiver pattern the body reads carried openly in the
    /// node: the adopt walk compares that operand structurally, so an
    /// unchanged operand agrees across evaluations and its answers carry,
    /// where a hidden capture would refuse on every rebuild.
    ///
    /// `constants` are the arguments that do not ride in this pattern, each
    /// at its position, with `None` at each position that a value of this
    /// pattern fills: empty when every argument rides. The caller answers
    /// for the pure view of each: see [`register::Registration::call`].
    pub fn fmap_to_pure_pattern_with_operand(
        &self,
        operand: &purity::PurePattern,
        body: register::PureBody,
        chain: usize,
        constants: Vec<Option<Value>>,
    ) -> Self {
        let steps = self.steps();
        Pattern::of(Node::PatternOfPureOperand(
            Box::new(OperandPureNode(
                self.clone(),
                operand.pattern().clone(),
                ConstructionMemo::default(),
            )),
            Arc::new(PatternedPureMap {
                body,
                chain,
                constants: constants.into(),
            }),
        ))
        .with_steps(steps)
    }

    /// Map each value to an arbitrary pattern.
    ///
    /// **Always classified impure**, because the closure is opaque and may
    /// materialise JavaScript at query time. This is the constructor every
    /// unknown or dynamic path must use - including `register()` with a
    /// non-native combinator body.
    ///
    /// There is deliberately no way for a caller to declare purity: a
    /// caller-supplied flag could carry a false-pure classification.
    pub fn fmap_to_pattern(&self, f: impl Fn(&Value) -> Pattern + Send + Sync + 'static) -> Self {
        let steps = self.steps();
        Pattern::of(Node::PatternOfDynamic(Box::new((
            self.clone(),
            Arc::new(f),
        ))))
        .with_steps(steps)
    }

    /// Native control augmentation using `outerBind` timing. The receiver's
    /// named live controls survive only when the combined values are unchanged.
    /// Both patterns remain explicit dependencies for callback ownership.
    pub fn outer_bind_controls(
        &self,
        inputs: Pattern,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
    ) -> Self {
        let combine = Arc::new(combine);
        let bind: HapToPatternFn = Arc::new({
            // A handle of the inputs, which the bind asks for each hap.
            let inputs = inputs.clone();
            move |outer| {
                let outer = outer.clone();
                let combine = combine.clone();
                inputs.map_haps_preserving_live(
                    move |inner| {
                        Some(
                            inner
                                .with_value(|value| combine(&outer.value, value))
                                .with_pitch_live_controls(&outer),
                        )
                    },
                    SpanUse::Ignores,
                )
            }
        });
        Pattern::of(Node::PatternOfHap(Box::new(HapBind(
            self.clone(),
            vec![inputs],
            bind,
        ))))
        .outer_join()
        .with_steps_of(self)
    }

    /// `fmap` with a user JS callback. **Marks the pattern impure.**
    ///
    /// The step count survives, as in every other `fmap` here. Without it the
    /// result is unusable as a `polyJoin` input: the join divides by an
    /// undefined step count and throws, which empties any poly-aligned
    /// expression that contains the JS-mapped pattern.
    pub fn fmap_js(&self, id: CallbackId) -> Self {
        let steps = self.steps();
        Pattern::of(Node::FmapJs(Box::new((self.clone(), id)))).with_steps(steps)
    }

    /// Trusted host metadata rewrite, with the same callback ownership and
    /// query limits as `fmap_js`. This must not implement user-authored maps:
    /// those deliberately invalidate slider provenance even for equal values.
    pub fn fmap_js_preserving_controls(&self, id: CallbackId) -> Self {
        Pattern::of(Node::FmapJsPreservingControls(Box::new((
            self.clone(),
            id,
            None,
        ))))
        .with_steps_of(self)
    }

    /// Materialize final event values, then apply a trusted metadata rewrite
    /// only where the native predicate matches. Unmatched values avoid a JS
    /// round-trip. This belongs after every user-authored transform.
    pub fn finalize_js_controls_if(&self, id: CallbackId, predicate: fn(&Value) -> bool) -> Self {
        Pattern::of(Node::FmapJsPreservingControls(Box::new((
            self.clone(),
            id,
            Some(predicate),
        ))))
        .with_steps_of(self)
    }

    /// The graph [`Self::finalize_js_controls_if`] rewrites, or this graph
    /// when its root is no such rewrite.
    #[doc(hidden)]
    pub fn without_final_controls(&self) -> &Pattern {
        match &self.node.kind {
            Node::FmapJsPreservingControls(boxed) if boxed.2.is_some() => &boxed.0,
            _ => self,
        }
    }

    /// `innerJoin` - the inner hap's whole survives. `register()`'s default.
    pub fn inner_join(&self) -> Self {
        Pattern::of(Self::join_node(self.clone(), JoinMode::Inner))
    }
    /// `outerJoin` - the outer hap's whole survives.
    pub fn outer_join(&self) -> Self {
        Pattern::of(Self::join_node(self.clone(), JoinMode::Outer))
    }
    /// `join`/`bind` - wholes intersect.
    pub fn mix_join(&self) -> Self {
        Pattern::of(Self::join_node(self.clone(), JoinMode::Mix))
    }
    /// `squeezeJoin` - a whole cycle of each inner pattern is squeezed into the
    /// corresponding outer hap. What `ply`, `chop`, `euclid`'s squeeze variants
    /// and mini-notation's `@`-weighted replication are built from.
    pub fn squeeze_join(&self) -> Self {
        Pattern::of(Self::join_node(self.clone(), JoinMode::Squeeze))
    }
    /// `resetJoin` - inner cycle start re-aligned to each outer onset.
    pub fn reset_join(&self) -> Self {
        Pattern::of(Self::join_node(self.clone(), JoinMode::Reset))
    }
    /// `restartJoin` - inner cycle zero re-aligned to each outer onset.
    pub fn restart_join(&self) -> Self {
        Pattern::of(Self::join_node(self.clone(), JoinMode::Restart))
    }

    /// `polyBind(func)` with a USER JavaScript callback: `fmap` then
    /// `polyJoin`.
    pub fn poly_bind_js(&self, id: CallbackId) -> Self {
        pattern_of_js(self.clone(), id).poly_join()
    }

    /// `stepBind(func)` with a user JavaScript callback.
    ///
    /// `Err` on the construction-time failure - see [`Pattern::try_step_join`].
    /// This call can invoke the callback before any query, so the host's
    /// bridge frame must still be open.
    pub fn try_step_bind_js(&self, id: CallbackId) -> Result<Self, String> {
        pattern_of_js(self.clone(), id).try_step_join()
    }

    /// `polyJoin` - each inner pattern is `extend`ed to the outer's step count
    /// before an `outerJoin`.
    ///
    /// A node rather than a construction-time rewrite because structure and
    /// time transformations can wrap the pattern before join resolution, which
    /// therefore happens through the same query-time walker `stepJoin` uses.
    ///
    /// A missing step count on either side throws - which `queryArc` turns
    /// into no haps. Reproduced rather than smoothed over.
    pub fn poly_join(&self) -> Self {
        let steps = self.steps();
        Pattern::of(Node::PolyJoin(Box::new(self.clone()))).with_steps(steps)
    }

    // -- combinators --------------------------------------------------------

    pub fn with_query_time(
        &self,
        f: impl Fn(Fraction) -> Fraction + Send + Sync + 'static,
    ) -> Self {
        Pattern::of(Node::WithQueryTime(Box::new((self.clone(), Arc::new(f)))))
    }

    pub fn with_hap_time(&self, f: impl Fn(Fraction) -> Fraction + Send + Sync + 'static) -> Self {
        Pattern::of(Node::WithHapTime(Box::new((self.clone(), Arc::new(f)))))
    }

    pub fn with_query_span(
        &self,
        f: impl Fn(&TimeSpan) -> TimeSpan + Send + Sync + 'static,
    ) -> Self {
        Pattern::of(Node::WithQuerySpan(Box::new((self.clone(), Arc::new(f)))))
    }

    pub fn with_query_span_js(&self, id: CallbackId) -> Self {
        Pattern::of(Node::WithQuerySpanJs(Box::new((self.clone(), id))))
    }

    pub fn with_hap_span(&self, f: impl Fn(&TimeSpan) -> TimeSpan + Send + Sync + 'static) -> Self {
        Pattern::of(Node::WithHapSpan(Box::new((self.clone(), Arc::new(f)))))
    }

    /// `rev` - reverse each cycle. `preserveSteps = true`, `patternify = false`.
    pub fn rev(&self) -> Self {
        if let Some(reversed) = sequence::reversed(self) {
            return reversed.with_steps_of(self);
        }
        Pattern::of(Node::Rev(Box::new(self.clone())))
            .split_queries()
            .with_steps_of(self)
    }

    /// `revv` - reverse the whole timeline, not each cycle: negate both the
    /// query span and the hap spans.
    pub fn revv(&self) -> Self {
        fn negate(span: &TimeSpan) -> TimeSpan {
            // A span at the far edge of the native range cannot be negated;
            // the query refuses rather than unwinds. The returned span is
            // never read once the refusal flag is set.
            match (span.end.checked_neg(), span.begin.checked_neg()) {
                (Some(end), Some(begin)) => TimeSpan::new(end, begin),
                (None, _) | (_, None) => {
                    refuse(QueryLimit::NativeFraction { operation: "revv" });
                    *span
                }
            }
        }
        self.with_query_span(negate).with_hap_span(negate)
    }

    /// `zoom(s, e)` - play the `[s, e)` slice of the pattern over the full
    /// cycle. Unlike `focus`, the slice is taken **within** each cycle.
    ///
    /// The width, scaled step count, and cycle maps must fit the native
    /// fraction range; otherwise the query reports `NativeFraction`.
    pub fn zoom(&self, s: Fraction, e: Fraction) -> Self {
        combinators::checked_zoom(self, s, e, "zoom")
    }

    /// Map every value.
    ///
    /// `_steps` is carried through: mapping VALUES does not change structure.
    /// Dropping it made `s("bd sd cp").pace(4)` a no-op, because `pace` reads
    /// the step count it is supposed to rescale.
    pub fn fmap(&self, f: impl Fn(&Value) -> Value + Send + Sync + 'static) -> Self {
        let steps = self.steps();
        Pattern::of(Node::FmapNative(Box::new((self.clone(), Arc::new(f))))).with_steps(steps)
    }

    /// Changes hap values and keeps the live control bindings of each hap.
    /// `f` returns the new value, or `None` for a value that stays as it is.
    /// This is for a key that no slider moves, such as a routing name.
    /// [`Pattern::fmap`] drops the bindings, and a sounding voice then stops
    /// following its sliders.
    pub fn fmap_keeping_live(
        &self,
        f: impl Fn(&Value) -> Option<Value> + Send + Sync + 'static,
    ) -> Self {
        let steps = self.steps();
        self.map_haps_owned(move |hap| {
            Some(match f(&hap.value) {
                Some(value) => hap.into_value_keeping_live(value),
                None => hap,
            })
        })
        .with_steps(steps)
    }

    /// [`Self::fmap_keeping_live`] with an already-built map, for a caller
    /// that interned one by what it reads. See [`interned_closure`].
    pub fn fmap_keeping_live_with(&self, map: OwnedHapMapFn) -> Self {
        let steps = self.steps();
        Pattern::of(Node::MapHapsOwned(Box::new((
            self.clone(),
            map,
            SpanUse::Ignores,
        ))))
        .with_steps(steps)
    }

    /// Tag every hap produced by this pattern with a stable lane id, keeping
    /// live slider bindings and visual slots intact.
    pub fn with_lane(&self, lane: u32) -> Self {
        static LANE_MAPS: [std::sync::OnceLock<OwnedHapMapFn>; 64] =
            [const { std::sync::OnceLock::new() }; 64];
        let map = if let Some(slot) = usize::try_from(lane).ok().filter(|&slot| slot < 64) {
            // Share one closure per common lane index so repeated evaluations
            // reuse the map pointer without per-eval allocation.
            Arc::clone(
                LANE_MAPS[slot]
                    .get_or_init(|| Arc::new(move |hap: Hap| Some(hap.with_lane_context(lane)))),
            )
        } else {
            Arc::new(move |hap: Hap| Some(hap.with_lane_context(lane)))
        };
        self.fmap_keeping_live_with(map)
    }

    /// Host-only provenance for a native query-time slider. Arbitrary value
    /// maps discard this tag before it can become an audio parameter binding.
    pub fn with_slider_binding(&self, binding: u64) -> Self {
        self.map_haps_owned(move |hap| {
            let mut hap = hap.without_live_controls();
            if binding != 0 {
                hap.keys.edit().slider_binding = binding;
            }
            Some(hap)
        })
        .with_steps_of(self)
    }

    /// This pattern with the control of `spec` set to the constant `value`
    /// by [`Self::with_control`], with no pattern of the constant between.
    /// `Err` gives the pattern back as it was: `value` is no constant of
    /// one plain value, or the pattern does not take the control so.
    ///
    /// The control is what the map of the control makes of a hap of the
    /// value: [`controls::ControlSpec::with_value`] of the value, and
    /// nothing more on the hap.
    fn set_constant_control(
        self,
        spec: &controls::ControlSpec,
        value: &Self,
    ) -> Result<Self, Self> {
        match sequence::constant_value_at(value) {
            Some((value, context)) => self.with_control(&spec.with_value(value), context),
            None => Err(self),
        }
    }

    /// [`Self::set_constant_control`] for a `pure` of the data value
    /// `value`, with no pattern of it.
    pub(crate) fn set_control_value(
        mut self,
        spec: &controls::ControlSpec,
        value: &Value,
    ) -> Result<Self, Self> {
        if !value.is_data(0) {
            return Err(self);
        }
        // Plain scalar control on a built-in non-`"value"` key: avoid allocating
        // a temporary 1-entry `BuiltinMap<1>` when writing into a Sequence,
        // Constants run, or fresh single-control Constants node.
        if matches!(value, Value::F64(_) | Value::Str(_) | Value::Bool(_)) && spec.name() != "value"
        {
            let packed_key = if spec.id != 0 {
                controls::builtin_packed_key(spec.id)
            } else {
                0
            };
            if packed_key != 0 && self.can_set_packed_direct(&[packed_key]) {
                let mut shared_entries = None;
                let mut shared_bare_dress = None;
                let mut shared_last_dress = None;
                self.set_packed_direct(
                    &[packed_key],
                    std::slice::from_ref(value),
                    Locations::new(),
                    &mut shared_entries,
                    &mut shared_bare_dress,
                    &mut shared_last_dress,
                );
                return Ok(self);
            } else if matches!(self.node_kind(), Node::Sequence(_))
                && sequence::takes_plain_set(&self)
            {
                if let Some(cell) = self.own_node()
                    && let Node::Sequence(layers) = &mut cell.kind
                {
                    sequence::set_single_here(layers, &spec.key(), value.clone());
                    return Ok(self);
                }
            } else if let Node::Constants(node) = self.node_kind()
                && self.same_runtime_owner(&node.0)
                && node.1.last().is_some_and(RunMeeting::takes_sets)
            {
                if let Some(cell) = self.own_node()
                    && let Node::Constants(node) = &mut cell.kind
                    && let Some(last) = node.1.last_mut()
                {
                    last.set_single_after(packed_key, &spec.key(), value.clone());
                    return Ok(self);
                }
            } else if packed_key != 0 && !self.takes_constants() && self.meets_constants() {
                let steps = self.steps();
                return Ok(self
                    .met_owned([RunMeeting::of_single_control(packed_key, value.clone())])
                    .with_steps(steps));
            }
        }
        // A handle of the value, for the control to hold.
        self.with_control(&spec.with_value(value.clone()), Locations::new())
    }

    /// [`Self::set_control_value`] carrying source `context` (for single-word
    /// mini literals), avoiding an intermediate `Node::PureAt` and temporary
    /// `BuiltinMap<1>` when writing into a foldable sequence.
    pub(crate) fn set_control_value_at(
        mut self,
        spec: &controls::ControlSpec,
        value: &Value,
        context: Locations,
    ) -> Result<Self, (Self, Locations)> {
        if context.is_empty() {
            return self
                .set_control_value(spec, value)
                .map_err(|back| (back, Locations::new()));
        }
        if !value.is_data(0) {
            return Err((self, context));
        }
        if matches!(value, Value::F64(_) | Value::Str(_) | Value::Bool(_))
            && spec.id != 0
            && spec.name() != "value"
        {
            let packed_key = controls::builtin_packed_key(spec.id);
            if self.can_set_packed_direct(&[packed_key]) {
                let mut shared_entries = None;
                let mut shared_bare_dress = None;
                let mut shared_last_dress = None;
                self.set_packed_direct(
                    &[packed_key],
                    std::slice::from_ref(value),
                    context,
                    &mut shared_entries,
                    &mut shared_bare_dress,
                    &mut shared_last_dress,
                );
                return Ok(self);
            }
            if matches!(self.node_kind(), Node::Sequence(_)) {
                return Err((self, context));
            }
            if self.meets_each_control() {
                let steps = self.steps();
                let meeting = RunMeeting::of_single_control_at(packed_key, value.clone(), context);
                return Ok(self.met_owned([meeting]).with_steps(steps));
            }
        }
        let control = spec.with_value(value.clone());
        let saved = context.clone();
        self.with_control(&control, context)
            .map_err(|back| (back, saved))
    }

    /// This pattern after [`Self::set_control`] of each control of a row
    /// in turn, as one `set_control` of `union`, the union of the row, at
    /// `context`, the locations of each control, the last one first. `Err`
    /// gives the pattern back as it was, for the caller to set each
    /// control in turn.
    ///
    /// A `set` of maps of controls is a union that keeps the keys in order
    /// and takes the later value of a repeated key, so the union of the
    /// controls first, then with a step, is the same map. A pattern that
    /// takes the union takes each control, and a node that holds each
    /// control of a row apart, as a run of sets does, takes them from
    /// `apart`: see [`Self::write_row`].
    pub(crate) fn with_row(
        mut self,
        union: &Value,
        context: Locations,
        apart: &Apart<'_>,
    ) -> Result<Self, Self> {
        if self.meets_each_control() && union.is_data(0) {
            return Ok(self.met_row_owned(union, context, apart.has_many()));
        }
        let Some((waves, under)) = self.taking(union) else {
            return Err(self);
        };
        self.write(waves, &under, context, apart);
        Ok(self)
    }

    fn can_set_packed_direct(&self, keys: &[u16]) -> bool {
        match self.node_kind() {
            Node::Sequence(_) => sequence::takes_plain_set(self),
            Node::Constants(node) => {
                self.same_runtime_owner(&node.0)
                    && node.1.last().is_some_and(RunMeeting::takes_sets)
            }
            Node::AppWave(app) => {
                if !self.keeps_runtime_over(&app.inner) {
                    return false;
                }
                let mut combined = [0u16; 16];
                let mut len = 0usize;
                for item in app.waves.iter() {
                    let Some(key) = controls::builtin_key_ref_by_id(item.key_id) else {
                        return false;
                    };
                    if key.as_str() == "value" || keys.iter().any(|&k| (k & 0x3ff) == item.key_id) {
                        return false;
                    }
                    let packed = controls::builtin_packed_key(item.key_id);
                    if !combined[..len].contains(&packed) {
                        if len >= combined.len() {
                            return false;
                        }
                        combined[len] = packed;
                        len += 1;
                    }
                }
                if len + keys.len() > combined.len() {
                    return false;
                }
                combined[len..len + keys.len()].copy_from_slice(keys);
                app.inner
                    .can_set_packed_direct(&combined[..len + keys.len()])
            }
            Node::Stack(patterns) => {
                let mut any = false;
                for pattern in patterns.iter() {
                    if pattern.is_rest() {
                        continue;
                    }
                    if !pattern.can_set_packed_direct(keys) {
                        return false;
                    }
                    any = true;
                }
                any
            }
            Node::StackPure(cells) => {
                let mut any = false;
                for cell in cells.iter() {
                    let ok = match &cell.kind {
                        Node::Silence => continue,
                        Node::Sequence(layers) => sequence::takes_plain_set_layers(layers),
                        _ => plain_pattern(cell, 0).can_set_packed_direct(keys),
                    };
                    if !ok {
                        return false;
                    }
                    any = true;
                }
                any
            }
            Node::FilterValues(..) => self
                .under_structure()
                .is_some_and(|functions| functions.can_set_packed_direct(keys)),
            Node::Degrade(boxed) if matches!(boxed.0.node_kind(), Node::Sequence(_)) => {
                self.keeps_runtime_over(&boxed.0) && sequence::takes_plain_set(&boxed.0)
            }
            Node::Pick(pick) => {
                pick.mode != JoinMode::Squeeze
                    && sequence::folds(self)
                    && sequence::is_plain_selector(
                        &pick.selector,
                        Self::picks_whole_cycles(pick.mode),
                    )
                    && pick.lookup.patterns().all(|picked| {
                        (picked.is_rest() && sequence::folds(picked))
                            || (sequence::lies_in_cycles(picked)
                                && picked.can_set_packed_direct(keys))
                    })
            }
            _ => false,
        }
    }

    fn set_packed_direct(
        &mut self,
        keys: &[u16],
        vals: &[Value],
        context: Locations,
        shared_entries: &mut Option<Arc<dyn value::BuiltinEntries>>,
        shared_bare_dress: &mut Option<Arc<sequence::Dress>>,
        shared_last_dress: &mut Option<(Arc<sequence::Dress>, Arc<sequence::Dress>)>,
    ) {
        let Some(cell) = self.own_node() else {
            debug_assert!(false, "can_set_packed_direct matched an unwritable node");
            return;
        };
        match &mut cell.kind {
            Node::Sequence(layers) => sequence::set_packed_here(
                layers,
                keys,
                vals,
                context,
                shared_entries,
                shared_bare_dress,
                shared_last_dress,
            ),
            Node::Constants(node) => {
                if let Some(last) = node.1.last_mut() {
                    last.set_packed_after(keys, vals, context, shared_entries);
                }
            }
            Node::Degrade(boxed) => {
                boxed.0.set_packed_direct(
                    keys,
                    vals,
                    context,
                    shared_entries,
                    shared_bare_dress,
                    shared_last_dress,
                );
            }
            Node::AppWave(app) => {
                let mut combined_keys = [0u16; 16];
                let mut combined_vals = [const { Value::Undefined }; 16];
                let mut len = 0usize;
                for item in app.waves.iter() {
                    let packed = controls::builtin_packed_key(item.key_id);
                    if !combined_keys[..len].contains(&packed) {
                        combined_keys[len] = packed;
                        combined_vals[len] = Value::F64(0.0);
                        len += 1;
                    }
                }
                combined_keys[len..len + keys.len()].copy_from_slice(keys);
                for (i, val) in vals.iter().enumerate() {
                    combined_vals[len + i] = val.clone();
                }
                let total = len + keys.len();
                let mut wave_entries = None;
                let mut wave_bare_dress = None;
                let mut wave_last_dress = None;
                app.inner.set_packed_direct(
                    &combined_keys[..total],
                    &combined_vals[..total],
                    context,
                    &mut wave_entries,
                    &mut wave_bare_dress,
                    &mut wave_last_dress,
                );
            }
            Node::Stack(patterns) => {
                for pattern in patterns.iter_mut().filter(|p| !p.is_rest()) {
                    pattern.set_packed_direct(
                        keys,
                        vals,
                        context.clone(),
                        shared_entries,
                        shared_bare_dress,
                        shared_last_dress,
                    );
                }
            }
            Node::StackPure(cells) => {
                if let Some(patterns) = mutate_pure_cells(cells, |pattern| {
                    pattern.set_packed_direct(
                        keys,
                        vals,
                        context.clone(),
                        shared_entries,
                        shared_bare_dress,
                        shared_last_dress,
                    );
                }) {
                    cell.kind = Node::Stack(patterns);
                }
            }
            Node::FilterValues(boxed) => {
                if let Some(app_cell) = boxed.0.own_node()
                    && let Node::AppRight(node) = &mut app_cell.kind
                {
                    node.0.set_packed_direct(
                        keys,
                        vals,
                        context,
                        shared_entries,
                        shared_bare_dress,
                        shared_last_dress,
                    );
                }
            }
            Node::Pick(pick) => {
                let ok = sequence::selecting_in_place(
                    &mut pick.selector,
                    &context,
                    Self::picks_whole_cycles(pick.mode),
                );
                debug_assert!(ok, "validated plain selector");
                let mut pick_bare_dress = None;
                let mut pick_last_dress = None;
                for picked in pick.lookup.patterns_mut().filter(|p| !p.is_rest()) {
                    picked.set_packed_direct(
                        keys,
                        vals,
                        Locations::new(),
                        shared_entries,
                        &mut pick_bare_dress,
                        &mut pick_last_dress,
                    );
                }
            }
            _ => unreachable!("validated by can_set_packed_direct"),
        }
    }

    /// Fast path of [`Self::with_row`] for stack-packed built-in controls:
    /// writes directly into foldable sequences, wave chains, stacks, and picks
    /// without allocating a temporary `BuiltinMap` union.
    pub(crate) fn with_packed_row(
        mut self,
        keys: &[u16],
        vals: &mut [Value],
        context: Locations,
        apart: &Apart<'_>,
    ) -> Result<Self, Self> {
        if let Node::Sequence(layers) = self.node_kind()
            && sequence::takes_plain_set_layers(layers)
            && let Some(cell) = self.own_node()
            && let Node::Sequence(layers) = &mut cell.kind
        {
            sequence::set_packed_layers_owned(layers, keys, vals, context);
            return Ok(self);
        }
        if self.can_set_packed_direct(keys) {
            let mut shared_entries = None;
            let mut shared_bare_dress = None;
            let mut shared_last_dress = None;
            self.set_packed_direct(
                keys,
                vals,
                context,
                &mut shared_entries,
                &mut shared_bare_dress,
                &mut shared_last_dress,
            );
            return Ok(self);
        }
        if matches!(self.node_kind(), Node::Sequence(_)) {
            return Err(self);
        }
        let union = Value::Object(OrderedMap::from_packed_builtin(keys, vals));
        if self.meets_each_control() {
            return Ok(self.met_row_owned(&union, context, apart.has_many()));
        }
        self.with_row(&union, context, apart)
    }

    /// Whether a constant control may go into this pattern, by its kinds
    /// alone: the gate of a row of controls, which costs less than the
    /// controls. [`Self::takes_control`] decides for a control.
    pub(crate) fn takes_constants(&self) -> bool {
        match self.node_kind() {
            Node::Sequence(_) => true,
            Node::Degrade(boxed) if matches!(boxed.0.node_kind(), Node::Sequence(_)) => true,
            Node::Pick(pick) => pick
                .lookup
                .patterns()
                .all(|picked| picked.is_rest() || picked.takes_constants()),
            Node::Stack(patterns) => patterns
                .iter()
                .any(|pattern| !pattern.is_rest() && pattern.takes_constants()),
            Node::StackPure(cells) => cells.iter().any(|cell| match &cell.kind {
                Node::Silence => false,
                Node::Sequence(_) => true,
                _ => plain_pattern(cell, 0).takes_constants(),
            }),
            Node::FilterValues(..) => self.under_structure().is_some_and(Self::takes_constants),
            Node::Constants(node) => node.1.last().is_some_and(RunMeeting::takes_sets),
            // A control of a wave, which a constant goes under.
            Node::AppLeft(pair)
                if matches!(pair.3, Combine::Composed(compose::ComposeOp::Set))
                    && pair.4 == LookupFlow::Set =>
            {
                table::wave_name(&pair.1).is_some() && pair.0.takes_constants()
            }
            Node::AppWave(app) => app.inner.takes_constants(),
            _ => false,
        }
    }

    /// This pattern with the constant `control`, a map of controls at
    /// `context`, set on it as the `set` of a constant of it does: by
    /// [`Self::set_control`] where the pattern takes it, and as a meeting
    /// in a node of its own where the pattern
    /// [meets constants](Self::meets_constants) so. `Err` gives the pattern
    /// back as it was, for the caller to compose that `set`.
    pub(crate) fn with_control(
        mut self,
        control: &Value,
        context: Locations,
    ) -> Result<Self, Self> {
        if self.set_control(control, &context) {
            return Ok(self);
        }
        match self.meets_constants() && control.is_data(0) {
            true => Ok(self.met_control(control, context)),
            false => Err(self),
        }
    }

    fn map_control(&self, spec: &controls::ControlSpec, unnamed_only: bool) -> Self {
        // One closure object per distinct spec: the same control built again
        // is the same payload, so its subtree carries its answers.
        // A control of no hap is silence.
        if self.is_rest() && sequence::folds(self) {
            return self.clone();
        }
        if sequence::folds(self)
            && !self.has_runtime_settings()
            && let Node::IntRange {
                start,
                count,
                loc0,
                loc1,
                named_id,
                step_neg,
            } = self.node_kind()
        {
            if *named_id == 0 && spec.id != 0 && spec.names.len() == 1 && spec.packed_key != 0 {
                return Pattern::of(Node::IntRange {
                    start: *start,
                    count: *count,
                    loc0: *loc0,
                    loc1: *loc1,
                    named_id: spec.id,
                    step_neg: *step_neg,
                })
                .with_steps_of(self);
            }
            if *named_id != 0 && unnamed_only {
                return self.clone();
            }
        }
        // The steps of a sequence take the name of the control when they
        // are read.
        if let Some(named) = sequence::named(self, spec, unnamed_only) {
            return named.with_steps_of(self);
        }
        if spec.id != 0 && !sequence::holds_steps(self) {
            return Pattern::of(Self::control_map_node(self.clone(), spec.id, unnamed_only))
                .with_steps_of(self);
        }
        let map = controls::interned_control_map(spec, unnamed_only);
        if let Some(mapped) = sequence::mapped(self, &map) {
            return mapped.with_steps_of(self);
        }
        if spec.id != 0 {
            return Pattern::of(Self::control_map_node(self.clone(), spec.id, unnamed_only))
                .with_steps_of(self);
        }
        Pattern::of(Node::MapHapsOwned(Box::new((
            self.clone(),
            map,
            SpanUse::Ignores,
        ))))
        .with_steps_of(self)
    }

    /// Owned version of [`Self::map_control`] that names an unshared
    /// `Node::Sequence` in place, avoiding `Vec<Layer>`, `Arc<[Layer]>`, and
    /// `Arc<NodeCell>` allocations on `s("...")` and `note("...")`.
    pub(crate) fn map_control_owned(
        mut self,
        spec: &controls::ControlSpec,
        unnamed_only: bool,
    ) -> Self {
        if sequence::folds(&self)
            && matches!(self.node_kind(), Node::Sequence(_))
            && let Some(cell) = Arc::get_mut(&mut self.node)
            && let Node::Sequence(layers) = &mut cell.kind
            && sequence::named_in_place(layers, spec, unnamed_only)
        {
            return self;
        }
        if sequence::folds(&self)
            && !self.has_runtime_settings()
            && let Node::IntRange { named_id, .. } = self.node_kind()
        {
            if *named_id == 0 && spec.id != 0 && spec.names.len() == 1 && spec.packed_key != 0 {
                if let Some(cell) = Arc::get_mut(&mut self.node)
                    && let Node::IntRange { named_id, .. } = &mut cell.kind
                {
                    *named_id = spec.id;
                    return self;
                }
            } else if *named_id != 0 && unnamed_only {
                return self;
            }
        }
        self.map_control(spec, unnamed_only)
    }

    pub fn filter_values(
        &self,
        predicate: impl Fn(&Value) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.filter_values_with(Arc::new(predicate))
    }

    /// [`filter_values`] with an already-built predicate, for a combinator
    /// that interned one by what it reads. See [`interned_closure`].
    pub fn filter_values_with(&self, predicate: ValuePredicate) -> Self {
        if let Some(filtered) = sequence::filtered(self, &predicate) {
            return filtered.with_steps_of(self);
        }
        Pattern::of(Node::FilterValues(Box::new((self.clone(), predicate)))).with_steps_of(self)
    }

    pub fn filter_values_js(&self, id: CallbackId) -> Self {
        Pattern::of(Node::FilterValuesJs(Box::new((self.clone(), id))))
    }

    /// `filterHaps(hap_test)` - the predicate sees whole and part, not just
    /// the value. Unlike `filterValues`, `_steps` is not carried through.
    pub fn filter_haps(&self, predicate: impl Fn(&Hap) -> bool + Send + Sync + 'static) -> Self {
        Pattern::of(Node::FilterHaps(Box::new((
            self.clone(),
            Arc::new(predicate),
        ))))
    }

    pub fn filter_haps_js(&self, id: CallbackId) -> Self {
        Pattern::of(Node::FilterHapsJs(Box::new((self.clone(), id))))
    }

    pub fn filter_when_js(&self, id: CallbackId) -> Self {
        Pattern::of(Node::FilterWhenJs(Box::new((self.clone(), id))))
    }

    pub fn sort_haps_by_part_pattern(&self) -> Self {
        Pattern::of(Node::SortHapsByPart(Box::new(self.clone())))
    }

    /// `seed(n)` - pin the random stream for this subtree.
    pub fn with_rand_seed(&self, seed: f64) -> Self {
        Pattern::of(Node::WithRandSeed(Box::new((self.clone(), seed)))).with_steps_of(self)
    }

    /// Per-hap rewrite where `None` drops the hap. Steps are preserved by the
    /// callers registered `preserveSteps`.
    pub fn map_haps_native(&self, f: impl Fn(&Hap) -> Option<Hap> + Send + Sync + 'static) -> Self {
        self.map_haps_preserving_live(
            move |hap| f(hap).map(Hap::without_live_controls),
            SpanUse::Reads,
        )
    }

    /// [`map_haps_native`] for a body that captures nothing: one closure
    /// object per body, so the same rewrite built by a later evaluation is
    /// the same payload. See [`interned_closure`].
    pub fn map_haps_native_static(&self, body: fn(&Hap) -> Option<Hap>) -> Self {
        type WeakHapMapFn = std::sync::Weak<dyn Fn(&Hap) -> Option<Hap> + Send + Sync>;
        type HapMapPool = std::sync::Mutex<std::collections::HashMap<usize, WeakHapMapFn>>;
        static POOL: std::sync::OnceLock<HapMapPool> = std::sync::OnceLock::new();
        let steps = self.steps();
        let interned = interned_closure(POOL.get_or_init(Default::default), body as usize, || {
            let mapped: HapMapFn =
                Arc::new(move |hap: &Hap| body(hap).map(Hap::without_live_controls));
            mapped
        });
        Pattern::of(Node::MapHapsNative(Box::new((
            self.clone(),
            interned,
            SpanUse::Reads,
        ))))
        .with_steps(steps)
    }

    fn map_haps_preserving_live(
        &self,
        f: impl Fn(&Hap) -> Option<Hap> + Send + Sync + 'static,
        spans: SpanUse,
    ) -> Self {
        self.mapped_haps(Arc::new(f), spans)
    }

    /// The node of `map` over this pattern. A map that reads no span folds
    /// into a sequence: see [`sequence::mapped_haps`]. Over a pick, it is
    /// the map of each picked pattern: see [`Self::picked_with`].
    fn mapped_haps(&self, map: HapMapFn, spans: SpanUse) -> Self {
        if spans == SpanUse::Ignores {
            // A map of no hap is silence.
            if self.is_rest() && sequence::folds(self) {
                return self.clone();
            }
            if let Some(mapped) = sequence::mapped_haps(self, &map) {
                return mapped;
            }
            let picked = self.picked_with(&Locations::new(), false, |pattern| {
                Some(pattern.mapped_haps(Arc::clone(&map), spans))
            });
            if let Some(picked) = picked {
                return picked;
            }
        }
        Pattern::of(Node::MapHapsNative(Box::new((self.clone(), map, spans))))
    }

    /// This pick with what `change` makes of each pattern that it picks,
    /// and with `locations` first on each hap, for a change of the value,
    /// the keys or the controls of a hap. The join of a pick gives those of
    /// the picked hap to its own hap, so the change of the picked patterns
    /// is the change of the pick. See [`sequence::selecting`] for
    /// `locations` and `whole_cycles`.
    ///
    /// `None` when this is no pick, when it is a squeeze, which orders the
    /// context and the keys another way, when a handle binds a runtime or
    /// is volatile, and when the selector or `change` refuses.
    fn picked_with(
        &self,
        locations: &Locations,
        whole_cycles: bool,
        change: impl FnMut(&Pattern) -> Option<Pattern>,
    ) -> Option<Self> {
        let Node::Pick(picked) = self.node_kind() else {
            return None;
        };
        if picked.mode == JoinMode::Squeeze || !sequence::folds(self) {
            return None;
        }
        let whole_cycles =
            whole_cycles && matches!(picked.mode, JoinMode::Reset | JoinMode::Restart);
        let selector = sequence::selecting(&picked.selector, locations, whole_cycles)?;
        Some(pick_compact(
            selector,
            picked.lookup.try_map(change)?,
            picked.index_mode,
            picked.mode,
        ))
    }

    /// The `appLeft` of this pick and `values` by `op`, when `values` is a
    /// constant of a plain value and each picked pattern is a sequence of
    /// steps that lie in one cycle: each step then meets the constant where
    /// the step is, in the picked pattern as in the pick. The locations of
    /// the constant come first on each hap, through the selector.
    fn picked_applied(
        &self,
        values: &Pattern,
        op: compose::ComposeOp,
        flow: LookupFlow,
    ) -> Option<Self> {
        let (value, locations) = sequence::constant_value_at(values)?;
        self.picked_with(&locations, true, |pattern| {
            if !sequence::lies_in_cycles(pattern) {
                return None;
            }
            if pattern.is_rest() {
                return Some(pattern.clone());
            }
            // A sequence takes a `set` of the value with no pattern of the
            // value between.
            let set = (op == compose::ComposeOp::Set && flow == LookupFlow::Set)
                .then(|| {
                    // A handle of the picked pattern: the set writes a copy.
                    let mut set = pattern.clone();
                    set.set_control(&value, &Locations::new()).then_some(set)
                })
                .flatten();
            let applied = set.unwrap_or_else(|| {
                pattern.app_left_by(pure(value.clone()), Combine::Composed(op), flow)
            });
            // A meeting that the fold leaves to its node is the pick's.
            matches!(applied.node_kind(), Node::Sequence(_)).then_some(applied)
        })
    }

    /// Maps each hap and gives the closure ownership of it. Use this when the
    /// change can move the value or context instead of cloning them. `f`
    /// keeps the spans and reads none of them.
    fn map_haps_owned(&self, f: impl Fn(Hap) -> Option<Hap> + Send + Sync + 'static) -> Self {
        Pattern::of(Node::MapHapsOwned(Box::new((
            self.clone(),
            Arc::new(f),
            SpanUse::Ignores,
        ))))
    }

    /// A pitch rewrite. `f` keeps the spans and reads none of them.
    fn map_pitch_haps_native(
        &self,
        f: impl Fn(&Hap) -> Option<Hap> + Send + Sync + 'static,
    ) -> Self {
        self.map_haps_preserving_live(
            move |hap| f(hap).map(|mapped| mapped.with_pitch_live_controls(hap)),
            SpanUse::Ignores,
        )
    }

    /// [`Self::map_pitch_haps_native`] for a body that is a pure function of
    /// the name it reads: one closure object per name, so the same rewrite
    /// built by a later evaluation is the same payload and its subtree
    /// carries its answers. See [`interned_closure`].
    ///
    /// `make` gives the body for a name that has none yet, so a body that
    /// holds a table of its own, as a scale holds its notes, is made one
    /// time for the name. It gives the rewrite of a hap, and the note that
    /// the rewrite makes of a note or a step that a hap holds alone.
    fn interned_pitch_rewrite_with_degrees<F, G>(
        key: &str,
        make: impl FnOnce() -> (F, G, Option<Arc<str>>, Option<Arc<[value::SharedStr; 32]>>),
    ) -> Arc<PitchRewrite>
    where
        F: Fn(&Hap) -> Option<Hap> + Send + Sync + 'static,
        G: Fn(&Value) -> Option<Value> + Send + Sync + 'static,
    {
        const MAX_CACHED_KEY_BYTES: usize = 64;
        let build = || {
            let (of_hap, of_note, scale_tag, degree_notes) = make();
            let hap: HapMapFn = Arc::new(move |hap: &Hap| {
                of_hap(hap).map(|mapped| mapped.with_pitch_live_controls(hap))
            });
            let keys = scale_tag.map(|tag| {
                context_keys::ContextKeys::of(context_keys::Keys {
                    scale: Some(tag),
                    ..Default::default()
                })
            });
            let bare_dress = keys.as_ref().and_then(sequence::Dress::for_bare_renote);
            Arc::new(PitchRewrite {
                key: Arc::from(key),
                hap,
                note: Box::new(of_note),
                keys,
                bare_dress,
                degree_notes,
            })
        };
        if key.len() > MAX_CACHED_KEY_BYTES {
            return build();
        }
        if let Ok(Some(hit)) = PITCH_REWRITE_TLS.try_with(|cell| {
            let cache = cell.borrow();
            cache
                .0
                .iter()
                .flatten()
                .find(|entry| entry.key.as_ref() == key)
                .map(Arc::clone)
        }) {
            return hit;
        }
        let pool = PITCH_REWRITE_POOL.get_or_init(Default::default);
        // The name is copied only for a body that the pool takes.
        let held = pool
            .lock()
            .ok()
            .and_then(|entries| entries.get(key).and_then(std::sync::Weak::upgrade));
        let rewrite = held.unwrap_or_else(|| interned_closure(pool, key.to_owned(), build));
        let _ = PITCH_REWRITE_TLS.try_with(|cell| {
            if let Ok(mut cache) = cell.try_borrow_mut() {
                let idx = cache.1 & 3;
                cache.0[idx] = Some(Arc::clone(&rewrite));
                cache.1 = cache.1.wrapping_add(1);
            }
        });
        rewrite
    }

    fn interned_pitch_rewrite<F, G>(
        key: &str,
        make: impl FnOnce() -> (F, G, Option<Arc<str>>),
    ) -> Arc<PitchRewrite>
    where
        F: Fn(&Hap) -> Option<Hap> + Send + Sync + 'static,
        G: Fn(&Value) -> Option<Value> + Send + Sync + 'static,
    {
        Self::interned_pitch_rewrite_with_degrees(key, || {
            let (of_hap, of_note, scale_tag) = make();
            (of_hap, of_note, scale_tag, None)
        })
    }

    /// Direct owned pitch rewrite for scale lookups carrying precomputed
    /// integer degree notes: borrows the TLS-cached rewrite without refcount
    /// traffic when renoting a uniquely-owned `Node::IntRange` in place.
    pub(crate) fn map_scale_pitch_owned<F, G>(
        mut self,
        key: &str,
        make: impl FnOnce() -> (F, G, Option<Arc<str>>, Option<Arc<[value::SharedStr; 32]>>),
    ) -> Self
    where
        F: Fn(&Hap) -> Option<Hap> + Send + Sync + 'static,
        G: Fn(&Value) -> Option<Value> + Send + Sync + 'static,
    {
        let steps = self.steps();
        if let Node::IntRange {
            start,
            count,
            loc0,
            loc1,
            named_id,
            step_neg,
        } = *self.node_kind()
            && key.len() <= 64
            && sequence::folds(&self)
            && Arc::strong_count(&self.node) == 1
        {
            let fast_layer = PITCH_REWRITE_TLS
                .try_with(|cell| {
                    let cache = cell.borrow();
                    let entry = cache
                        .0
                        .iter()
                        .flatten()
                        .find(|entry| entry.key.as_ref() == key)?;
                    let mut dress_cache = if loc0 == 0 && entry.keys.is_some() {
                        sequence::RenoteDressCache::with_bare(entry.bare_dress.clone())
                    } else {
                        sequence::RenoteDressCache::default()
                    };
                    sequence::renoted_int_range_layer(
                        start,
                        count,
                        loc0,
                        loc1,
                        named_id,
                        step_neg,
                        &entry.hap,
                        &entry.note,
                        entry.degree_notes.as_deref(),
                        entry.keys.as_ref(),
                        &mut dress_cache,
                    )
                })
                .ok()
                .flatten();
            if let Some(layer) = fast_layer
                && let Some(cell) = Arc::get_mut(&mut self.node)
            {
                cell.kind = Node::Sequence(Arc::from([layer]));
                return self.with_steps(steps);
            }
        }
        let interned = Self::interned_pitch_rewrite_with_degrees(key, make);
        let skip_bare_cache = matches!(self.node_kind(), Node::IntRange { loc0, .. } if *loc0 != 0);
        let mut dress_cache = if !skip_bare_cache && interned.keys.is_some() {
            sequence::RenoteDressCache::with_bare(interned.bare_dress.clone())
        } else {
            sequence::RenoteDressCache::default()
        };
        self.pitched_owned(&interned, &mut dress_cache)
            .with_steps(steps)
    }

    fn map_pitch_haps_native_interned<F, G>(
        &self,
        key: &str,
        make: impl FnOnce() -> (F, G, Option<Arc<str>>),
    ) -> Self
    where
        F: Fn(&Hap) -> Option<Hap> + Send + Sync + 'static,
        G: Fn(&Value) -> Option<Value> + Send + Sync + 'static,
    {
        let steps = self.steps();
        let interned = Self::interned_pitch_rewrite(key, make);
        self.pitched(&interned).with_steps(steps)
    }

    /// Owned variant of [`Self::map_pitch_haps_native_interned`]: mutates
    /// uniquely-owned `Sequence` and `Pick` nodes in place instead of cloning
    /// layers, value slices, and lookup tables.
    pub(crate) fn map_pitch_haps_native_interned_owned<F, G>(
        self,
        key: &str,
        make: impl FnOnce() -> (F, G, Option<Arc<str>>),
    ) -> Self
    where
        F: Fn(&Hap) -> Option<Hap> + Send + Sync + 'static,
        G: Fn(&Value) -> Option<Value> + Send + Sync + 'static,
    {
        let steps = self.steps();
        let interned = Self::interned_pitch_rewrite(key, make);
        let skip_bare_cache = matches!(self.node_kind(), Node::IntRange { loc0, .. } if *loc0 != 0);
        let mut dress_cache = if !skip_bare_cache && interned.keys.is_some() {
            sequence::RenoteDressCache::with_bare(interned.bare_dress.clone())
        } else {
            sequence::RenoteDressCache::default()
        };
        self.pitched_owned(&interned, &mut dress_cache)
            .with_steps(steps)
    }

    /// [`Self::mapped_haps`] of a rewrite of pitch, which reads no span.
    /// Steps that `note` or `n` names take their new note where they are:
    /// see [`sequence::renoted`]. So does each pattern of a pick.
    fn pitched(&self, rewrite: &PitchRewrite) -> Self {
        let mut dress_cache = if rewrite.keys.is_some() {
            sequence::RenoteDressCache::with_bare(rewrite.bare_dress.clone())
        } else {
            sequence::RenoteDressCache::default()
        };
        self.pitched_cached(rewrite, &mut dress_cache)
    }

    fn pitched_owned(
        mut self,
        rewrite: &PitchRewrite,
        dress_cache: &mut sequence::RenoteDressCache,
    ) -> Self {
        if let Node::IntRange {
            start,
            count,
            loc0,
            loc1,
            named_id,
            step_neg,
        } = *self.node_kind()
            && sequence::folds(&self)
            && Arc::strong_count(&self.node) == 1
            && let Some(layer) = sequence::renoted_int_range_layer(
                start,
                count,
                loc0,
                loc1,
                named_id,
                step_neg,
                &rewrite.hap,
                &rewrite.note,
                rewrite.degree_notes.as_deref(),
                rewrite.keys.as_ref(),
                dress_cache,
            )
            && let Some(cell) = Arc::get_mut(&mut self.node)
        {
            cell.kind = Node::Sequence(Arc::from([layer]));
            return self;
        }
        if let Node::Sequence(layers) = self.node_kind()
            && sequence::folds(&self)
            && Arc::strong_count(&self.node) == 1
            && Arc::strong_count(layers) == 1
        {
            if let Some(cell) = Arc::get_mut(&mut self.node)
                && let Node::Sequence(layers) = &mut cell.kind
                && sequence::renote_in_place(
                    layers,
                    &rewrite.hap,
                    &rewrite.note,
                    rewrite.keys.as_ref(),
                    dress_cache,
                )
            {
                return self;
            }
        } else if let Node::Pick(picked) = self.node_kind()
            && picked.mode != JoinMode::Squeeze
            && sequence::folds(&self)
            && sequence::is_plain_selector(&picked.selector, false)
            && Arc::strong_count(&self.node) == 1
            && let Some(cell) = self.own_node()
            && let Node::Pick(picked) = &mut cell.kind
        {
            for slot in picked.lookup.patterns_mut() {
                let owned = std::mem::replace(slot, silence());
                *slot = owned.pitched_owned(rewrite, dress_cache);
            }
            return self;
        }
        self.pitched_cached(rewrite, dress_cache)
    }

    fn pitched_cached(
        &self,
        rewrite: &PitchRewrite,
        dress_cache: &mut sequence::RenoteDressCache,
    ) -> Self {
        if let Some(renoted) = sequence::renoted_cached(
            self,
            &rewrite.hap,
            &rewrite.note,
            rewrite.keys.as_ref(),
            dress_cache,
        ) {
            return renoted;
        }
        let picked = self.picked_with(&Locations::new(), false, |pattern| {
            Some(pattern.pitched_cached(rewrite, dress_cache))
        });
        // A handle of the rewrite, for the node that holds it.
        picked.unwrap_or_else(|| self.mapped_haps(Arc::clone(&rewrite.hap), SpanUse::Ignores))
    }

    fn expand_pitch_haps_native(
        &self,
        f: impl Fn(&Hap) -> Vec<Hap> + Send + Sync + 'static,
    ) -> Self {
        Pattern::of(Node::ExpandHapsNative(Box::new((
            self.clone(),
            Arc::new(move |hap, out| {
                for mapped in f(hap) {
                    out.push(mapped.with_pitch_live_controls(hap));
                }
            }),
            SpanUse::Ignores,
        ))))
    }

    /// [`Self::expand_pitch_haps_native`] for a body that captures nothing:
    /// one closure object per body, so the same expansion built by a later
    /// evaluation is the same payload. See [`interned_closure`].
    fn expand_pitch_haps_native_static(&self, body: fn(&Hap, &mut Vec<Hap>)) -> Self {
        type WeakHapExpandFn = std::sync::Weak<dyn Fn(&Hap, &mut Vec<Hap>) + Send + Sync>;
        type HapExpandPool = std::sync::Mutex<std::collections::HashMap<usize, WeakHapExpandFn>>;
        static POOL: std::sync::OnceLock<HapExpandPool> = std::sync::OnceLock::new();
        let steps = self.steps();
        let interned = interned_closure(POOL.get_or_init(Default::default), body as usize, || {
            let expand: HapExpandFn = Arc::new(move |hap: &Hap, out: &mut Vec<Hap>| {
                let before = out.len();
                body(hap, out);
                let input_live = hap.live_controls() != [0; 4];
                if input_live
                    || out[before..].iter().any(|m| {
                        !m.keys.is_empty() && {
                            let k = m.keys.get();
                            k.slider_binding != 0 || k.live_controls != [0; 4]
                        }
                    })
                {
                    for mapped in &mut out[before..] {
                        let taken = std::mem::replace(
                            mapped,
                            Hap::new(
                                None,
                                TimeSpan::new(Fraction::ZERO, Fraction::ZERO),
                                Value::Undefined,
                            ),
                        );
                        *mapped = taken.with_pitch_live_controls(hap);
                    }
                }
            });
            expand
        });
        Pattern::of(Node::ExpandHapsNative(Box::new((
            self.clone(),
            interned,
            SpanUse::Ignores,
        ))))
        .with_steps(steps)
    }

    pub fn map_haps_with_state(
        &self,
        f: impl Fn(&State, &Hap) -> Option<Hap> + Send + Sync + 'static,
    ) -> Self {
        Pattern::of(Node::MapHapsWithState(Box::new((
            self.clone(),
            Arc::new(move |state, hap| f(state, hap).map(Hap::without_live_controls)),
        ))))
    }

    /// One hap → N haps (same classification rules as `map_haps_native`).
    pub fn expand_haps_native(&self, f: impl Fn(&Hap) -> Vec<Hap> + Send + Sync + 'static) -> Self {
        Pattern::of(Node::ExpandHapsNative(Box::new((
            self.clone(),
            Arc::new(move |hap, out| {
                for mapped in f(hap) {
                    out.push(mapped.without_live_controls());
                }
            }),
            SpanUse::Reads,
        ))))
    }

    pub fn collect(&self) -> Self {
        Pattern::of(Node::Collect(Box::new(self.clone())))
    }

    /// The onsets of each cycle, valued by their position in the cycle.
    /// See [`combinators::isobite`].
    pub fn cycle_onset_indices(&self) -> Self {
        Pattern::of(Node::CycleOnsetIndex(Box::new(self.clone()))).split_queries()
    }

    /// One hap per cycle, valued by the number of onsets in the cycle.
    /// See [`combinators::isobite`].
    pub fn cycle_onset_count(&self) -> Self {
        Pattern::of(Node::CycleOnsetCount(Box::new(self.clone()))).split_queries()
    }

    pub fn loop_at(&self, factor: Fraction) -> Self {
        Pattern::of(Node::LoopAt(Box::new((self.clone(), factor))))
    }

    pub fn cpm(&self, cpm: Fraction) -> Self {
        Pattern::of(Node::Cpm(Box::new((self.clone(), TempoRate::new(cpm)))))
    }

    /// Sets each [`combinators::slice`] event's `speed` so its slice fills the
    /// event; keeps the receiver's step count.
    pub fn splice(&self) -> Self {
        self.map_haps_with_state(|state, hap| {
            let Value::Object(value) = materialize_js_value(&hap.value) else {
                signal_query_error(|| "splice requires object-valued haps".into());
                return None;
            };
            let Some(whole) = hap.whole() else {
                signal_query_error(|| {
                    "Cannot read properties of undefined (reading 'duration')".into()
                });
                return None;
            };
            let truthy_number = |value: Option<&Value>, fallback: f64| {
                value
                    .filter(|value| value.js_truthy())
                    .map(combinators::as_js_number)
                    .unwrap_or(fallback)
            };
            let cps = truthy_number(state.controls.get("_cps"), 1.0);
            let slices =
                combinators::as_js_number(value.get("_slices").unwrap_or(&Value::Undefined));
            let prior_speed = truthy_number(value.get("speed"), 1.0);
            let speed = cps / slices / whole.duration().to_f64() * prior_speed;
            let mut next = OrderedMap::from_entries([
                ("speed".into(), Value::F64(speed)),
                ("unit".into(), Value::Str("c".into())),
            ]);
            for (name, value) in &value {
                next.insert_key(name.clone(), value.clone());
            }
            Some(hap.with_value(|_| Value::Object(next.clone())))
        })
        .with_steps_of(self)
    }

    pub fn fit(&self) -> Self {
        self.map_haps_with_state(|state, hap| {
            let Value::Object(mut value) = materialize_js_value(&hap.value) else {
                signal_query_error(|| "Cannot use 'in' operator on a primitive value".into());
                return None;
            };
            let Some(whole) = hap.whole() else {
                signal_query_error(|| {
                    "Cannot read properties of undefined (reading 'duration')".into()
                });
                return None;
            };
            let begin = value
                .get("begin")
                .map(combinators::as_js_number)
                .unwrap_or(0.0);
            let end = value
                .get("end")
                .map(combinators::as_js_number)
                .unwrap_or(1.0);
            let cps = state
                .controls
                .get("_cps")
                .filter(|value| value.js_truthy())
                .map(combinators::as_js_number)
                .unwrap_or(1.0);
            let speed = cps / whole.duration().to_f64() * (end - begin);
            value.insert("speed".into(), Value::F64(speed));
            value.insert("unit".into(), Value::Str("c".into()));
            Some(hap.with_value(|_| Value::Object(value.clone())))
        })
    }

    /// `tag(name)` - append a context tag to every hap.
    ///
    /// The point is `hap.hasTag(name)` inside a later `filter`: mark haps in
    /// one branch of a `when`, then keep only those, which is how a score
    /// selects what an earlier transform touched.
    pub fn tag(&self, name: Arc<str>) -> Self {
        self.map_haps_preserving_live(
            move |hap| Some(hap.clone().with_tag_context(name.clone())),
            SpanUse::Ignores,
        )
    }

    /// Add a native graph node whose semantics are owned by a statically
    /// linked extension.
    pub fn extension_node(node: impl extension_node::ExtensionPatternNode + 'static) -> Self {
        Pattern::of(Node::Extension(Box::new(node)))
    }

    /// `discreteOnly()` - drop analog (whole-less) haps.
    pub fn discrete_only(&self) -> Self {
        self.filter_haps(|hap| hap.whole().is_some())
    }

    /// `onsetsOnly()` - keep only haps whose whole begins where their part does.
    pub fn onsets_only(&self) -> Self {
        self.filter_haps(Hap::has_onset)
    }

    pub fn remove_undefineds(&self) -> Self {
        let predicate = KEEP_DEFINED
            .get_or_init(|| {
                let predicate: ValuePredicate =
                    Arc::new(|value: &Value| !matches!(value, Value::Undefined | Value::Null));
                predicate
            })
            .clone();
        self.filter_values_with(predicate)
    }

    /// Whether `predicate` is the interned [`Self::remove_undefineds`] filter.
    #[inline]
    pub(crate) fn is_keep_defined(predicate: &ValuePredicate) -> bool {
        KEEP_DEFINED
            .get()
            .is_some_and(|keep| Arc::ptr_eq(keep, predicate))
    }

    /// The pattern under the structure that this pattern gives it, when
    /// this pattern is what `segment` and `struct` make: the values of a
    /// pattern where a structure is set, with the wholes of the structure.
    ///
    /// A `set` of a constant after such a structure is then the same `set`
    /// before it: each hap of the structure holds a value of the pattern
    /// as it is, over a part of the hap of that value. The structure must
    /// give its haps nothing more, or its locations would come after those
    /// of the constant. Each handle keeps its runtime: see
    /// [`Self::keeps_runtime_over`].
    fn under_structure(&self) -> Option<&Self> {
        let Node::FilterValues(boxed) = self.node_kind() else {
            return None;
        };
        let (structured, keep) = (&boxed.0, &boxed.1);
        let Node::AppRight(node) = structured.node_kind() else {
            return None;
        };
        let AppNode(functions, structure, Combine::With(combine), LookupFlow::Left) = &**node
        else {
            return None;
        };
        let kept = KEEP_DEFINED
            .get()
            .is_some_and(|defined| Arc::ptr_eq(defined, keep))
            && compose::is_keep_if(combine);
        let bound = self.keeps_runtime_over(structured) && structured.keeps_runtime_over(functions);
        (kept && sequence::is_bare(structure) && bound).then_some(functions)
    }

    /// Whether this handle binds the runtime that `inner`, a pattern of its
    /// node, binds, or `inner` binds none: a constructor of this node then
    /// gives a handle that takes the runtime of this one.
    fn keeps_runtime_over(&self, inner: &Self) -> bool {
        match (self.runtime_settings(), inner.runtime_settings()) {
            (_, None) => true,
            (Some(own), Some(inner)) => own.same_handle(inner),
            (None, Some(_)) => false,
        }
    }

    pub fn degrade_by_seeded(&self, amount: f64, seed: u32) -> Self {
        Pattern::of(Node::Degrade(Box::new((
            self.clone(),
            amount,
            DegradeKind::Seeded(seed),
        ))))
        .with_steps_of(self)
    }

    pub(crate) fn degrade_by_rand(&self, amount: f64, inverted: bool) -> Self {
        let kind = if inverted {
            DegradeKind::RandInverted
        } else {
            DegradeKind::Rand
        };
        Pattern::of(Node::Degrade(Box::new((self.clone(), amount, kind)))).with_steps_of(self)
    }

    pub fn repeat_cycles(&self, count: Fraction) -> Self {
        if count == Fraction::ZERO {
            return silence();
        }
        Pattern::of(Node::RepeatCycles(Box::new((self.clone(), count))))
            .split_queries()
            .with_steps_of(self)
    }

    /// Structure `self` with a binary pattern. The structure pattern's wholes
    /// survive, as in `keepif.out(structure)`.
    pub fn struct_with(&self, structure: Pattern) -> Self {
        self.app_right_with_lookup(
            structure,
            |value, keep| {
                if keep.js_truthy() {
                    value.clone()
                } else {
                    Value::Undefined
                }
            },
            LookupFlow::Left,
        )
        .remove_undefineds()
    }

    /// Default `set.in` alignment: preserve this pattern's structure and let
    /// the right-hand value override common object controls.
    pub fn set(&self, other: &Pattern) -> Self {
        self.app_left_with_lookup(other.clone(), controls::set_value, LookupFlow::Set)
            .with_steps_of(self)
    }

    pub fn inner_join_with(&self, f: impl Fn(&Value) -> Pattern + Send + Sync + 'static) -> Self {
        Pattern::of(Node::InnerJoin(Box::new((self.clone(), Arc::new(f)))))
    }

    pub fn split_queries(&self) -> Self {
        if self.is_plain_handle() {
            Pattern::of(Node::SplitQueries(
                Arc::clone(&self.node),
                self.node_flags(),
            ))
        } else {
            Pattern::of(Node::SplitQueriesBoxed(Box::new(self.clone())))
        }
    }

    /// Speed up by `factor`; `_steps` is kept.
    ///
    /// A representable factor can still overflow when applied to a query
    /// time, especially after another speed change. Both time maps are
    /// checked and report `NativeFraction` on overflow.
    pub fn fast(&self, factor: Fraction) -> Self {
        combinators::checked_fast(self, factor, "fast")
    }

    pub(crate) fn fast_owned(self, factor: Fraction) -> Self {
        if factor.numer() == 0 {
            return silence();
        }
        self.fast_node_owned(factor, "fast")
    }

    pub fn slow(&self, factor: Fraction) -> Self {
        if factor.numer() == 0 {
            return silence();
        }
        // The reciprocal of i128::MIN needs an unrepresentable denominator.
        // Keep `slow` in the diagnostic, including failures in the time maps.
        match Fraction::ONE.checked_div(factor) {
            Some(rate) => combinators::checked_fast(self, rate, "slow"),
            None => query_limit_pattern(mark_stepwise_refusal(QueryLimit::NativeFraction {
                operation: "slow",
            })),
        }
    }

    pub(crate) fn slow_owned(self, factor: Fraction) -> Self {
        if factor.numer() == 0 {
            return silence();
        }
        match Fraction::ONE.checked_div(factor) {
            Some(rate) => self.fast_node_owned(rate, "slow"),
            None => query_limit_pattern(mark_stepwise_refusal(QueryLimit::NativeFraction {
                operation: "slow",
            })),
        }
    }

    fn fast_node_owned(mut self, factor: Fraction, operation: &'static str) -> Self {
        if factor > Fraction::ZERO
            && sequence::folds(&self)
            && matches!(self.node_kind(), Node::Sequence(_))
            && let Some(cell) = Arc::get_mut(&mut self.node)
            && let Node::Sequence(layers) = &mut cell.kind
            && sequence::faster_in_place(layers, factor)
        {
            return self;
        }
        let steps = self.steps();
        self.fast_node(factor, operation).with_steps(steps)
    }

    /// Shift later in time. `_late(t)`.
    ///
    /// `_steps` survives a shift: moving a pattern in time does not change
    /// how many steps it has. Without it, `focus()` (which is
    /// `_early`/`_fast`/`_late`) would return a step-less pattern, and
    /// `polyJoin` would take the missing-`_steps` throw path.
    pub fn late(&self, t: Fraction) -> Self {
        self.shift_time(t, "late")
    }

    /// Shift earlier in time. `_early(t)`.
    pub fn early(&self, t: Fraction) -> Self {
        // i128::MIN has no representable positive counterpart.
        match t.checked_neg() {
            Some(shifted) => self.shift_time(shifted, "early"),
            None => query_limit_pattern(mark_stepwise_refusal(QueryLimit::NativeFraction {
                operation: "early",
            })),
        }
    }

    /// Subtract `t` from query times and add it to event times, preserving
    /// steps. On overflow, record `NativeFraction` and use zero until the query
    /// boundary reports the refusal.
    fn shift_time(&self, t: Fraction, operation: &'static str) -> Self {
        if let Some(shifted) = sequence::shifted(self, t) {
            return shifted.with_steps_of(self);
        }
        let is_early = match operation {
            "late" => Some(false),
            "early" => Some(true),
            _ => None,
        };
        let node = if self.is_plain_handle()
            && let Some(is_early) = is_early
            && let (Ok(n), Ok(d)) = (i16::try_from(t.numer()), u16::try_from(t.denom()))
        {
            Node::ShiftSmall(Arc::clone(&self.node), self.node_flags(), n, d, is_early)
        } else {
            Node::Shift(Box::new(ShiftNode(self.clone(), t, operation)))
        };
        Pattern::of(node).with_steps_of(self)
    }

    /// The [`Node::Fast`] behind [`combinators::checked_fast`].
    pub(crate) fn fast_node(&self, factor: Fraction, operation: &'static str) -> Self {
        if let Some(faster) = sequence::faster(self, factor) {
            return faster;
        }
        let is_slow = match operation {
            "fast" => Some(false),
            "slow" => Some(true),
            _ => None,
        };
        let node = if self.is_plain_handle()
            && let Some(is_slow) = is_slow
            && let (Ok(n), Ok(d)) = (i16::try_from(factor.numer()), u16::try_from(factor.denom()))
        {
            Node::FastSmall(Arc::clone(&self.node), self.node_flags(), n, d, is_slow)
        } else {
            Node::Fast(Box::new(FastNode(self.clone(), factor, operation)))
        };
        Pattern::of(node)
    }

    /// Speeds up like `fast`, but leaves a **gap** for the rest of the cycle
    /// rather than repeating. This is what weighted sequences are built from -
    /// plain `fast` repeats and is the wrong primitive.
    pub fn fast_gap(&self, factor: Fraction) -> Self {
        Pattern::of(Node::FastGap(Box::new((self.clone(), factor)))).split_queries()
    }

    /// `compress(b, e)` - squeeze into the `[b, e)` slot of each cycle.
    /// Out-of-range or inverted bounds yield silence; a window whose width
    /// leaves the native fraction range refuses as
    /// `NativeFraction { operation: "compress" }`.
    pub fn compress(&self, b: Fraction, e: Fraction) -> Self {
        if b > e
            || b > Fraction::ONE
            || e > Fraction::ONE
            || b < Fraction::ZERO
            || e < Fraction::ZERO
        {
            return silence();
        }
        // A zero-width window is a division by zero, which THROWS. `compress`
        // runs inside `pressBy`'s `fmap`, i.e. at query time, so the throw is
        // caught by `queryArc` and the whole query yields nothing.
        // `pressBy(1)` is the reachable case.
        if e == b {
            signal_query_error(|| "Division by zero".into());
            return silence();
        }
        let Some(rate) = e
            .checked_sub(b)
            .and_then(|width| Fraction::ONE.checked_div(width))
        else {
            return query_limit_pattern(mark_stepwise_refusal(QueryLimit::NativeFraction {
                operation: "compress",
            }));
        };
        self.fast_gap(rate).late(b)
    }

    /// `focus(b, e)` - like `compress`, but leaves no gaps and the window may
    /// be wider than a cycle.
    ///
    /// The `early(b.sam())` is what lets the window straddle cycles: only the
    /// whole-cycle part of `b` is removed before the speed change.
    ///
    /// The window width, its reciprocal, and the whole-cycle shift amount are
    /// checked at construction even when both bounds fit individually; an
    /// unrepresentable one refuses as `NativeFraction { operation: "focus" }`.
    /// Overflow in the query-time maps refuses as `late` or `fast`.
    pub fn focus(&self, b: Fraction, e: Fraction) -> Self {
        if e == b {
            // Division by zero throws. See `compress`.
            signal_query_error(|| "Division by zero".into());
            return silence();
        }
        let Some(rate) = e
            .checked_sub(b)
            .and_then(|span| Fraction::ONE.checked_div(span))
        else {
            return focus_native_fraction_refusal();
        };
        // `early(t)` is `late(t.neg())`; taking the checked negation keeps
        // the one unrepresentable shift a refusal rather than a panic.
        let Some(whole_cycles) = b.sam().checked_neg() else {
            return focus_native_fraction_refusal();
        };
        self.late(whole_cycles).fast(rate).late(b)
    }

    /// `stepJoin` - flatten a pattern of patterns by slicing the outer cycle
    /// at every hap boundary and `stepcat`ing the pieces.
    ///
    /// The step count is taken from cycle zero once, at construction, while
    /// the query re-slices per cycle. A pattern whose structure varies by
    /// cycle therefore keeps cycle zero's step count.
    ///
    /// Ignores the construction-time throw: native combinators build the
    /// pattern-of-patterns themselves, so it is unreachable for them.
    /// Anything driven by user source wants [`Pattern::try_step_join`].
    pub fn step_join(&self) -> Self {
        self.try_step_join()
            .unwrap_or_else(|_| Pattern::of(Node::StepJoin(Box::new(self.clone()))))
    }

    /// `stepJoin`, reporting the CONSTRUCTION-time failure.
    ///
    /// The phase boundary is load-bearing and not where it first appears:
    ///
    /// * the cycle-zero probe is a full `queryArc`, so anything that throws
    ///   during it is CAUGHT and yields no haps. That is why
    ///   `pure('bd').stepBind(x => { throw })` constructs fine and only empties
    ///   the later query.
    /// * slicing then runs OUTSIDE that boundary and dereferences each value
    ///   as a pattern. A hap whose value is not one therefore throws here,
    ///   before the pattern exists at all - `pure('bd').stepJoin()` and
    ///   `pure('bd').stepBind(x => 42)` both fail this way.
    /// * No haps at all is not a failure: `silence.stepJoin()` constructs.
    pub fn try_step_join(&self) -> Result<Self, String> {
        let cycle = State::new(TimeSpan::new(Fraction::ZERO, Fraction::ONE));
        // A `queryArc` boundary: stash any pending error, absorb the
        // ones this construction query raises, restore. Without the stash a
        // callback that throws here would leak its error into the NEXT
        // top-level query and empty an unrelated result.
        let error_boundary = QueryErrorBoundary::enter();
        // This construction-time `queryArc(0, 1)` needs the same resource
        // ownership as a public query. In particular, patterned shrink/grow
        // bodies can build many retained inner graphs here before StepJoin
        // exists. A local boundary captures that refusal; a nested call shares
        // its enclosing query's cumulative construction budget.
        let (resolved, refusal) =
            with_hap_budget(DEFAULT_HAP_BUDGET, || self.resolve_for_join(&cycle));
        let query_failed = error_boundary.finish().is_some();
        if let Some(limit) = refusal {
            return Ok(query_limit_pattern(limit));
        }

        let first_steps = if query_failed {
            // `queryArc` caught it, so an EMPTY hap list is sliced.
            step_slices_pattern(&[]).steps()
        } else {
            let resolved = ResolvedHap::all_patterns(resolved)
                .map_err(|value| step_join_value_error(&value))?;
            step_slices_pattern(&resolved).steps()
        };
        Ok(Pattern::of(Node::StepJoin(Box::new(self.clone()))).with_steps(first_steps))
    }

    /// `_focusSpan(span)` - `focus` over a `TimeSpan`. `squeezeJoin` uses this
    /// to fit a whole inner cycle into each outer hap.
    pub fn focus_span(&self, span: TimeSpan) -> Self {
        self.focus(span.begin, span.end)
    }
}

/// Carry a focus-window overflow to the next query, or mark the active query
/// immediately when construction happens inside one.
fn focus_native_fraction_refusal() -> Pattern {
    query_limit_pattern(mark_stepwise_refusal(QueryLimit::NativeFraction {
        operation: "focus",
    }))
}

/// A childless stand-in, cheap to produce: cloning it is a refcount bump and
/// `Silence` carries an empty purity, so no allocation happens on the drop
/// path where this is used.
fn silence_placeholder() -> Pattern {
    thread_local! {
        static SILENCE: Pattern = Pattern::of(Node::Silence);
    }
    SILENCE
        .try_with(Pattern::clone)
        // During thread-local teardown the cache may already be gone; a fresh
        // one is correct, just not free.
        .unwrap_or_else(|_| Pattern::of(Node::Silence))
}

/// Swap a child out for the placeholder, so the parent stops owning it.
/// Silence has no graph to take apart, so it stays: a node that was drained
/// before, whose children are placeholders, is then done at no cost.
fn take_child(child: &mut Pattern, out: &mut Vec<Pattern>) {
    if child.is_rest() {
        return;
    }
    out.push(std::mem::replace(child, silence_placeholder()));
}

/// Move every `Pattern` child out of a node, leaving it childless.
///
/// Children are swapped out rather than destructured because `Node`
/// implements `Drop`, and Rust will not let a value move out of one. The
/// match is EXHAUSTIVE on purpose: a wildcard would let a future combinator
/// reintroduce recursive destruction silently, and the symptom of that is a
/// process abort rather than a test failure. A new variant breaks this build.
///
/// Patterns reached through a `Value` are not drained. A value nests patterns
/// only as deeply as the source literal that wrote it, which the parser
/// bounds, whereas the combinator chain drained here is what a loop can grow
/// without limit.
fn drain_pattern_children(node: &mut Node, out: &mut Vec<Pattern>) {
    fn drain_lookup(lookup: &mut PickLookup, out: &mut Vec<Pattern>) {
        let entries: Vec<&mut Pattern> = match lookup {
            PickLookup::Array { entries, .. } => {
                entries.iter_mut().map(|(_, pattern)| pattern).collect()
            }
            PickLookup::Object { entries, .. } => {
                entries.iter_mut().map(|(_, pattern)| pattern).collect()
            }
        };
        for entry in entries {
            take_child(entry, out);
        }
    }
    match node {
        Node::Silence
        | Node::Pure(_)
        | Node::PureAt(_)
        | Node::IntRange { .. }
        | Node::Sequence(_)
        | Node::QueryError(_)
        | Node::QueryLimit(_)
        | Node::Signal(_)
        | Node::NumberSignal(_)
        | Node::SegmentSignal { .. }
        | Node::JsQuery(_) => {}
        Node::Iter(node) => take_child(&mut node.inner, out),
        #[cfg(non_send_node_mutation)]
        Node::NonSendMutation(_) => {}
        Node::Rev(pattern)
        | Node::StepJoin(pattern)
        | Node::PolyJoin(pattern)
        | Node::PurePattern(pattern)
        | Node::SortHapsByPart(pattern)
        | Node::Collect(pattern)
        | Node::CycleOnsetIndex(pattern)
        | Node::CycleOnsetCount(pattern)
        | Node::SplitQueriesBoxed(pattern) => take_child(pattern, out),
        Node::ControlMapBoxed(boxed) => take_child(&mut boxed.0, out),
        Node::JoinBoxed(boxed) => take_child(&mut boxed.0, out),
        Node::Fast(node) => take_child(&mut node.0, out),
        Node::FastSmall(cell, flags, ..)
        | Node::ShiftSmall(cell, flags, ..)
        | Node::ControlMap(cell, flags, ..)
        | Node::Join(cell, flags, ..)
        | Node::SplitQueries(cell, flags) => {
            let taken = std::mem::replace(cell, Arc::clone(&silence().node));
            out.push(Pattern {
                node: taken,
                step_numer: 0,
                step_denom: u32::from(*flags) << NODE_FLAGS_SHIFT,
                extras: None,
            });
        }
        Node::Shift(node) => take_child(&mut node.0, out),
        Node::Constants(node) => take_child(&mut node.0, out),
        Node::UiVisual(node) => take_child(&mut node.inner, out),
        Node::WithQueryTime(boxed) | Node::WithHapTime(boxed) => take_child(&mut boxed.0, out),
        Node::WithQuerySpan(boxed) | Node::WithHapSpan(boxed) => take_child(&mut boxed.0, out),
        Node::WithQuerySpanJs(boxed)
        | Node::ArpWith(boxed)
        | Node::PatternOfJs(boxed)
        | Node::FilterValuesJs(boxed)
        | Node::FilterHapsJs(boxed)
        | Node::FilterWhenJs(boxed)
        | Node::FmapJs(boxed) => take_child(&mut boxed.0, out),
        Node::FmapNative(boxed) => take_child(&mut boxed.0, out),
        Node::FilterValues(boxed) => take_child(&mut boxed.0, out),
        Node::FilterHaps(boxed) => take_child(&mut boxed.0, out),
        Node::MapHapsNative(boxed) => take_child(&mut boxed.0, out),
        Node::MapHapsOwned(boxed) => take_child(&mut boxed.0, out),
        Node::MapHapsWithState(boxed) => take_child(&mut boxed.0, out),
        Node::ExpandHapsNative(boxed) => take_child(&mut boxed.0, out),
        Node::WithRandSeed(boxed) => take_child(&mut boxed.0, out),
        Node::Degrade(boxed) => take_child(&mut boxed.0, out),
        Node::InnerJoin(boxed) | Node::PatternOfDynamic(boxed) => take_child(&mut boxed.0, out),
        Node::FastGap(boxed) | Node::RepeatCycles(boxed) | Node::LoopAt(boxed) => {
            take_child(&mut boxed.0, out)
        }
        Node::Cpm(boxed) => take_child(&mut boxed.0, out),
        Node::MidiKeys(boxed) => take_child(&mut boxed.0, out),
        Node::AddContext(boxed) => take_child(&mut boxed.0, out),
        Node::FmapJsPreservingControls(boxed) => take_child(&mut boxed.0, out),
        // The kept constructions are children in every sense that matters
        // here: each is a graph of its own, taken apart on the same worklist.
        Node::PatternOfPure(boxed) => {
            take_child(&mut boxed.0, out);
            boxed.2.drain_into(out);
        }
        Node::Extension(extension) => extension.drain_children(out),
        Node::AppLeft(pair) => {
            take_child(&mut pair.0, out);
            take_child(&mut pair.1, out);
        }
        Node::AppWave(app) => take_child(&mut app.inner, out),
        Node::AppRight(node) | Node::AppBoth(node) => {
            take_child(&mut node.0, out);
            take_child(&mut node.1, out);
        }
        Node::PatternOfPureOperand(pair, _) => {
            take_child(&mut pair.0, out);
            take_child(&mut pair.1, out);
            pair.2.drain_into(out);
        }
        Node::Arp(pair)
        | Node::ArpWithPattern(pair)
        | Node::Timeline(pair, _)
        | Node::PickPatternified { patterns: pair, .. } => {
            take_child(&mut pair.0, out);
            take_child(&mut pair.1, out);
        }
        Node::ChooseCycles(boxed) => out.extend(std::mem::take(&mut boxed.0).into_vec()),
        Node::Stack(patterns) | Node::SlowCat(patterns) | Node::SlowCatPrime(patterns) => {
            out.extend(std::mem::take(patterns).into_vec())
        }
        Node::StackPure(cells) | Node::SlowCatPure(cells) | Node::SlowCatPrimePure(cells) => {
            out.extend(
                std::mem::take(cells)
                    .into_vec()
                    .into_iter()
                    .map(|node| Pattern {
                        node,
                        step_numer: 0,
                        step_denom: 0,
                        extras: None,
                    }),
            );
        }
        Node::PatternOfHap(bind) => {
            take_child(&mut bind.0, out);
            out.extend(std::mem::take(&mut bind.1));
        }
        Node::XenPatterns(boxed) => {
            take_child(&mut boxed.0, out);
            out.extend(std::mem::take(&mut boxed.1));
        }
        Node::WChoose(choice) => {
            take_child(&mut choice.chooser, out);
            take_child(&mut choice.total, out);
            out.extend(std::mem::take(&mut choice.cumulative));
            out.extend(std::mem::take(&mut choice.values));
        }
        Node::PurePickLookup(boxed) => drain_lookup(&mut boxed.1, out),
        Node::Pick(pick) => {
            take_child(&mut pick.selector, out);
            pick.lookup.drain_into(out);
        }
    }
}

thread_local! {
    /// Reusable empty worklists for iterative `Node::drop`, avoiding a heap
    /// allocation per dropped temporary node during score construction.
    static DROP_WORK: std::cell::RefCell<Vec<Vec<Pattern>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

impl Drop for Node {
    /// Destroy the graph iteratively.
    ///
    /// A recursive drop uses one native frame per level. A chain built by a
    /// loop (`p = p.fast(2).slow(2)`) would overflow the stack on
    /// destruction, and a Rust stack overflow aborts the process: no panic,
    /// no `catch_unwind`, no recovery. The query depth limit does not help,
    /// because the drop can run before any query.
    ///
    /// Children move onto a heap worklist instead, so graph depth costs heap
    /// rather than stack. Every node reached here is childless by the time it
    /// drops, so it recurses no further.
    fn drop(&mut self) {
        LIVE_NODES.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        if matches!(
            self,
            Node::Silence
                | Node::Pure(_)
                | Node::IntRange { .. }
                | Node::Sequence(_)
                | Node::QueryError(_)
                | Node::QueryLimit(_)
                | Node::Signal(_)
                | Node::NumberSignal(_)
                | Node::SegmentSignal { .. }
                | Node::JsQuery(_)
        ) {
            return;
        }
        let mut pending = DROP_WORK
            .try_with(|pool| pool.borrow_mut().pop())
            .ok()
            .flatten()
            .unwrap_or_default();
        drain_pattern_children(self, &mut pending);
        while let Some(pattern) = pending.pop() {
            // Only the last owner has a graph to take apart; anyone else is
            // just releasing a reference.
            if let Some(mut cell) = Arc::into_inner(pattern.node) {
                drain_pattern_children(&mut cell.kind, &mut pending);
            }
        }
        if pending.capacity() <= 64 {
            let _ = DROP_WORK.try_with(|pool| {
                let mut pool = pool.borrow_mut();
                if pool.len() < 4 {
                    pool.push(pending);
                }
            });
        }
    }
}

/// Slice `[0, 1)` at every hap boundary, stack the patterns whose part meets
/// each slice (each carrying its outer hap's context), and `stepcat` the
/// pieces.
///
/// No total step count is computed: Strudel's retiming pass has a
/// destructuring slip that always leaves it undefined, so each slice keeps
/// its own `_steps` or none. Reproduced exactly rather than repaired.
fn step_slices_pattern(resolved: &[(Hap, Pattern)]) -> Pattern {
    let mut breakpoints: Vec<Fraction> = vec![Fraction::ZERO, Fraction::ONE];
    for (hap, _) in resolved {
        breakpoints.push(hap.part().begin);
        breakpoints.push(hap.part().end);
    }
    breakpoints.sort();
    breakpoints.dedup();

    let mut items: Vec<(Option<Fraction>, Pattern)> = Vec::new();
    for window in breakpoints.windows(2) {
        let span = TimeSpan::new(window[0], window[1]);
        // The inner patterns whose part meets this slice, each carrying the
        // outer hap's context.
        let mut layers: Vec<Pattern> = Vec::new();
        for (hap, inner) in resolved {
            if span.intersection(&hap.part()).is_none() {
                continue;
            }
            layers.push(if hap.context.is_empty() {
                inner.clone()
            } else {
                inner.with_added_context(hap.context.to_vec())
            });
        }
        let stacked = stack(layers);
        // Retiming keeps each pattern's own `_steps`, or leaves it undefined.
        items.push((stacked.steps(), stacked));
    }
    crate::combinators::stepcat(&items)
}

/// The per-inner scaling `polyJoin` applies: extend each inner pattern to the
/// outer step count. A missing step count on either side is a query error.
fn poly_extend(pattern: &Pattern, outer_steps: Option<Fraction>) -> Pattern {
    match (outer_steps, pattern.steps()) {
        (Some(outer), Some(inner)) if inner != Fraction::ZERO => {
            let Some(factor) = outer.checked_div(inner) else {
                return query_limit_pattern(mark_stepwise_refusal(QueryLimit::NativeFraction {
                    operation: "polyJoin",
                }));
            };
            let fast = pattern.fast(factor);
            let steps = match fast.steps().map(|steps| steps.checked_mul(factor)) {
                Some(None) => {
                    return query_limit_pattern(mark_stepwise_refusal(
                        QueryLimit::NativeFraction {
                            operation: "polyJoin",
                        },
                    ));
                }
                Some(Some(steps)) => Some(steps),
                None => None,
            };
            fast.with_steps(steps)
        }
        _ => {
            signal_query_error(|| "polyJoin: a pattern has no step count".into());
            silence()
        }
    }
}

/// Whether the answer of `node` depends on the span that asks, beyond the
/// cut of each hap's part. See [`purity::SpanRead`].
///
/// Read from the kind of the node and the classification of its children.
/// A kind that is not known to answer by its haps says that it reads the
/// span.
fn span_read(node: &Node) -> purity::SpanRead {
    use purity::SpanRead::{Made, No, Yes};
    let of = |pattern: &Pattern| pattern.span_read();
    let most = |patterns: &mut dyn Iterator<Item = &Pattern>| patterns.map(of).max().unwrap_or(No);
    match node {
        Node::Silence
        | Node::Pure(_)
        | Node::PureAt(_)
        | Node::PurePickLookup(..)
        | Node::IntRange { .. }
        | Node::SegmentSignal { .. }
        | Node::Sequence(_) => No,
        Node::Iter(node) => of(&node.inner),
        // A time map that keeps the order of time, and maps of each hap that
        // read no span.
        Node::Fast(node) if node.1 > Fraction::ZERO => of(&node.0),
        Node::FastSmall(_, flags, numer, _, _) if *numer > 0 => unpack_span_read(*flags),
        // A `fast` by the tempo of the query, which the span does not say.
        Node::Cpm(boxed) if boxed.1.cpm > Fraction::ZERO => of(&boxed.0),
        Node::ControlMap(_, flags, _, _) => unpack_span_read(*flags),
        Node::ControlMapBoxed(boxed) => of(&boxed.0),
        Node::Shift(node) => of(&node.0),
        Node::ShiftSmall(_, flags, ..) => unpack_span_read(*flags),
        Node::Constants(node) => of(&node.0),
        Node::UiVisual(node) => of(&node.inner),
        Node::AddContext(boxed) => of(&boxed.0),
        Node::FmapNative(boxed) => of(&boxed.0),
        Node::FilterValues(boxed) => of(&boxed.0),
        Node::MapHapsNative(boxed) if boxed.2 == SpanUse::Ignores => of(&boxed.0),
        Node::MapHapsOwned(boxed) if boxed.2 == SpanUse::Ignores => of(&boxed.0),
        Node::ExpandHapsNative(boxed) if boxed.2 == SpanUse::Ignores => of(&boxed.0),
        // A hap is kept or dropped by the begin of its whole.
        Node::Degrade(boxed) => of(&boxed.0),
        Node::Stack(patterns) => most(&mut patterns.iter()),
        Node::StackPure(_) => No,
        // One cycle at a time: the node inside is asked over each cycle's
        // piece of the span, where each of these kinds maps time in order.
        Node::SplitQueries(cell, flags) => match &cell.kind {
            Node::SlowCat(patterns) | Node::SlowCatPrime(patterns) => most(&mut patterns.iter()),
            Node::SlowCatPure(_) | Node::SlowCatPrimePure(_) => No,
            Node::FastGap(boxed) | Node::RepeatCycles(boxed) => of(&boxed.0),
            Node::Rev(pattern) => of(pattern),
            _ => unpack_span_read(*flags),
        },
        Node::SplitQueriesBoxed(inner) => match inner.node_kind() {
            Node::SlowCat(patterns) | Node::SlowCatPrime(patterns) => most(&mut patterns.iter()),
            Node::SlowCatPure(_) | Node::SlowCatPrimePure(_) => No,
            Node::FastGap(boxed) | Node::RepeatCycles(boxed) => of(&boxed.0),
            Node::Rev(pattern) => of(pattern),
            _ => of(inner),
        },
        // The side that gives the structure is cut by the query. The other
        // side is asked over the whole of each hap, whatever the query.
        Node::AppLeft(pair) => of(&pair.0),
        Node::AppWave(app) => of(&app.inner),
        Node::AppRight(pair) => of(&pair.1),
        Node::AppBoth(pair) => of(&pair.0).max(of(&pair.1)),
        // Each hap of the selector asks its pattern over the hap's part, or
        // over the span and keeps the part: the same haps, for patterns of
        // discrete haps. A squeeze makes a pattern for each hap.
        Node::Pick(pick) if pick.mode != JoinMode::Squeeze => {
            of(&pick.selector).max(most(&mut pick.lookup.patterns()))
        }
        // Each hap of the carrier asks a pattern over the hap's part. The
        // body makes the pattern at the query, where the join reads how it
        // answers.
        Node::Join(
            cell,
            _,
            JoinMode::Inner | JoinMode::Outer | JoinMode::Mix | JoinMode::Squeeze,
        ) => match &cell.kind {
            Node::PatternOfPureOperand(pair, ..) => of(&pair.0).max(Made),
            Node::PatternOfPure(boxed) => of(&boxed.0).max(Made),
            _ => Yes,
        },
        Node::JoinBoxed(boxed)
            if matches!(
                boxed.1,
                JoinMode::Inner | JoinMode::Outer | JoinMode::Mix | JoinMode::Squeeze
            ) =>
        {
            match boxed.0.node_kind() {
                Node::PatternOfPureOperand(pair, ..) => of(&pair.0).max(Made),
                Node::PatternOfPure(boxed) => of(&boxed.0).max(Made),
                _ => Yes,
            }
        }
        _ => Yes,
    }
}

/// The single place impurity is derived. Adding a `Node` variant without a
/// branch here fails to compile, so a new combinator cannot silently be
/// classified pure.
fn compute_purity(node: &Node) -> purity::Purity {
    use purity::Purity;
    fn value_purity(value: &Value) -> Purity {
        match value {
            Value::Function(function) => function
                .callback_id()
                .map(Purity::with_callback)
                .unwrap_or_else(Purity::pure),
            Value::Pattern(pattern) => Purity::merge(vec![
                pattern.pattern().purity(),
                Purity::with_callback(pattern.id()),
            ]),
            Value::JsValue(reference) => Purity::with_callback(reference.id()),
            Value::List(values) => Purity::merge(values.iter().map(value_purity)),
            Value::Object(values) => {
                Purity::merge(values.iter().map(|(_, value)| value_purity(value)))
            }
            Value::Haps(haps) => {
                Purity::merge(haps.as_slice().iter().map(|hap| value_purity(&hap.value)))
            }
            _ => Purity::pure(),
        }
    }
    match node {
        // Query-time errors and limits hold no callback or JavaScript, so they
        // stay queryable with no host installed.
        Node::Pure(value) => value_purity(value),
        Node::PureAt(boxed) => value_purity(&boxed.0),
        // Data holds no callback, and the values of a score are data.
        Node::Sequence(layers) => Purity::merge(
            layers
                .iter()
                .filter(|layer| !layer.is_data())
                .flat_map(|layer| layer.held_values())
                .filter(|value| !value.is_data(0))
                .map(value_purity),
        ),
        Node::PurePickLookup(boxed) => {
            Purity::merge(boxed.1.patterns().map(Pattern::purity))
        }
        Node::Silence
        | Node::IntRange { .. }
        | Node::Signal(_)
        | Node::NumberSignal(_)
        | Node::SegmentSignal { .. }
        | Node::QueryError(_)
        | Node::QueryLimit(_) => Purity::pure(),
        Node::Iter(node) => node.inner.purity(),

        // Any node holding a callback id is impure and contributes it.
        Node::FmapJs(boxed)
        | Node::WithQuerySpanJs(boxed)
        | Node::FilterValuesJs(boxed)
        | Node::FilterHapsJs(boxed)
        | Node::FilterWhenJs(boxed) => {
            Purity::merge([boxed.0.purity(), Purity::with_callback(boxed.1)])
        }
        Node::FmapJsPreservingControls(boxed) => {
            Purity::merge([boxed.0.purity(), Purity::with_callback(boxed.1)])
        }
        Node::JsQuery(id) => Purity::with_callback(*id),

        Node::Rev(p)
        | Node::StepJoin(p)
        | Node::PolyJoin(p)
        // Exact inheritance: a known pure inner stays pure.
        | Node::PurePattern(p)
        | Node::SortHapsByPart(p)
        | Node::Collect(p)
        | Node::CycleOnsetIndex(p)
        | Node::CycleOnsetCount(p)
        | Node::SplitQueriesBoxed(p) => p.purity(),
        Node::ControlMapBoxed(boxed) => boxed.0.purity(),
        Node::JoinBoxed(boxed) => boxed.0.purity(),
        Node::Fast(node) => node.0.purity(),
        Node::FastSmall(_, flags, ..)
        | Node::ShiftSmall(_, flags, ..)
        | Node::ControlMap(_, flags, ..)
        | Node::Join(_, flags, ..)
        | Node::SplitQueries(_, flags) => purity_from_plain_flags(*flags),
        Node::Shift(node) => node.0.purity(),
        Node::Constants(node) => node.0.purity(),
        Node::UiVisual(node) => node.inner.purity(),
        Node::WithQueryTime(boxed) | Node::WithHapTime(boxed) => boxed.0.purity(),
        Node::WithQuerySpan(boxed) | Node::WithHapSpan(boxed) => boxed.0.purity(),
        Node::FmapNative(boxed) => boxed.0.purity(),
        Node::FilterValues(boxed) => boxed.0.purity(),
        Node::FilterHaps(boxed) => boxed.0.purity(),
        Node::MapHapsNative(boxed) => boxed.0.purity(),
        Node::MapHapsOwned(boxed) => boxed.0.purity(),
        Node::MapHapsWithState(boxed) => boxed.0.purity(),
        Node::ExpandHapsNative(boxed) => boxed.0.purity(),
        Node::WithRandSeed(boxed) => boxed.0.purity(),
        Node::Degrade(boxed) => boxed.0.purity(),
        Node::FastGap(boxed) | Node::RepeatCycles(boxed) | Node::LoopAt(boxed) => boxed.0.purity(),
        Node::Cpm(boxed) => boxed.0.purity(),
        Node::AddContext(boxed) => boxed.0.purity(),
        // Sound: the closure's return type forbids JavaScript.
        Node::PatternOfPure(boxed) => boxed.0.purity(),
        Node::InnerJoin(boxed) => boxed.0.purity(),
        // Sound for the same reason; the operand was proven pure when the
        // node was built, and is re-proven by its own branch when rebuilt.
        Node::PatternOfPureOperand(pair, _) => Purity::merge([pair.0.purity(), pair.1.purity()]),

        // A live port answers with whatever was played since the last query.
        Node::MidiKeys(boxed) => Purity::merge([boxed.0.purity(), Purity::volatile()]),

        Node::PatternOfJs(boxed) => Purity::merge([
            boxed.0.purity(),
            Purity::with_callback(boxed.1),
            Purity::opaque(),
        ]),

        Node::Extension(extension) => {
            let children = Purity::merge(extension.children().iter().map(Pattern::purity));
            if extension.volatile() {
                Purity::merge([children, Purity::volatile()])
            } else {
                children
            }
        }
        Node::PatternOfDynamic(boxed) => Purity::merge([boxed.0.purity(), Purity::opaque()]),
        Node::PatternOfHap(bind) => {
            Purity::merge(std::iter::once(bind.0.purity()).chain(bind.1.iter().map(Pattern::purity)))
        }
        Node::XenPatterns(boxed) => Purity::merge(
            std::iter::once(boxed.0.purity()).chain(boxed.1.iter().map(Pattern::purity)),
        ),

        Node::Stack(ps) | Node::SlowCat(ps) | Node::SlowCatPrime(ps) => {
            Purity::merge(ps.iter().map(Pattern::purity))
        }
        Node::StackPure(_) | Node::SlowCatPure(_) | Node::SlowCatPrimePure(_) => Purity::pure(),
        Node::ChooseCycles(boxed) => Purity::merge(boxed.0.iter().map(Pattern::purity)),
        Node::WChoose(choice) => Purity::merge(
            [choice.chooser.purity(), choice.total.purity()]
                .into_iter()
                .chain(choice.cumulative.iter().map(Pattern::purity))
                .chain(choice.values.iter().map(Pattern::purity)),
        ),
        Node::Pick(pick) => Purity::merge(
            std::iter::once(pick.selector.purity())
                .chain(pick.lookup.patterns().map(Pattern::purity)),
        ),
        Node::AppLeft(pair) => Purity::merge([pair.0.purity(), pair.1.purity()]),
        Node::AppWave(app) => app.inner.purity(),
        Node::AppRight(node) | Node::AppBoth(node) => {
            Purity::merge([node.0.purity(), node.1.purity()])
        }
        Node::PickPatternified { patterns: pair, .. } => {
            Purity::merge([pair.0.purity(), pair.1.purity()])
        }

        // `arp` reaches no JavaScript of its own: it is grouping plus an index
        // lookup, so it is exactly as pure as the chord and the indices are.
        Node::Arp(pair) => Purity::merge([pair.0.purity(), pair.1.purity()]),

        // The return pattern is chosen by JavaScript at query time. Its
        // callback reachability cannot be enumerated statically, so this is
        // impure, names its own cell, and is opaque for ownership marking.
        Node::ArpWith(boxed) => Purity::merge([
            boxed.0.purity(),
            Purity::with_callback(boxed.1),
            Purity::opaque(),
        ]),
        Node::ArpWithPattern(pair) => {
            Purity::merge([pair.0.purity(), pair.1.purity(), Purity::opaque()])
        }
        // Its offsets are remembered from one query to the next.
        Node::Timeline(pair, _) => {
            Purity::merge([pair.0.purity(), pair.1.purity(), Purity::volatile()])
        }
    }
}

// -- constructors -----------------------------------------------------------

/// A user-authored `new Pattern(state => …)`. **Always impure.**
pub fn js_query(id: CallbackId) -> Pattern {
    Pattern::of(Node::JsQuery(id))
}

pub fn timeline(time_pattern: Pattern, pattern: Pattern, state: TimelineState) -> Pattern {
    Pattern::of(Node::Timeline(
        Box::new(PatternPair(time_pattern, pattern)),
        state,
    ))
}

/// `arp(indices, pat)` - play a chord one voice at a time.
///
/// `register('arp', ..., false)`: the indices are not patternified, so a
/// pattern argument stays one pattern rather than being lifted hap-by-hap.
pub fn arp(pattern: Pattern, indices: Pattern) -> Pattern {
    Pattern::of(Node::Arp(Box::new(PatternPair(pattern, indices))))
}

/// `arpWith(func, pat)` - select or pattern a chord through a JS callback.
///
/// Always impure: the callback is invoked at query time and may return a
/// pattern that itself reaches callbacks not visible in this graph yet.
pub fn arp_with(pattern: Pattern, callback: CallbackId) -> Pattern {
    Pattern::of(Node::ArpWith(Box::new((pattern, callback))))
}

/// General registered `arpWith` path where callback functions vary in time.
pub fn arp_with_pattern(pattern: Pattern, callbacks: Pattern) -> Pattern {
    Pattern::of(Node::ArpWithPattern(Box::new(PatternPair(
        pattern, callbacks,
    ))))
}

#[inline(never)]
fn query_midi_keys(lengths: &Pattern, port: &Arc<midi_in::InputPort>, state: &State) -> Vec<Hap> {
    let place = state.controls.contains_key("_cps").then(|| {
        (
            i64::try_from(state.span.begin.numer()).ok(),
            i64::try_from(state.span.begin.denom()).ok(),
        )
    });
    let place = place.and_then(|(numerator, denominator)| Some((numerator?, denominator?)));
    let mut hits = Vec::new();
    port.keys.select(
        state.span.begin.to_f64(),
        state.span.end.to_f64(),
        midi_in::now_nanos(),
        place,
        &mut hits,
    );
    // How fast the cycle is running, for turning the distance back to the
    // finger into a distance back in cycles.
    let cps = state
        .controls
        .get("_cps")
        .and_then(|value| match value {
            Value::F64(cps) => Some(*cps),
            _ => None,
        })
        .filter(|cps| cps.is_finite() && *cps > 0.0);
    let mut out = Vec::new();
    for hit in hits {
        let at = Fraction::new(i128::from(hit.num), i128::from(hit.den));
        // Sample the length where the key was struck, not where the note
        // first sounds.
        //
        // A note sounds at the frontier the scheduler has reached, which
        // is a few milliseconds after the key press. Near a boundary in the
        // length pattern, a sample at the frontier can read the wrong side:
        // `keys("0.01 1")` played just before the half cycle would get the
        // long length instead of the short one. The onset cannot move
        // earlier than the frontier, but the strike time is known.
        let struck = cps
            .and_then(|cps| {
                let back = hit.struck_nanos_ago as f64 / 1_000_000_000.0 * cps;
                Fraction::from_f64(back).map(|back| at.sub(back))
            })
            .unwrap_or(at);
        let sampled = lengths.query(&state.set_span(TimeSpan::new(struck, struck)));
        let length = sampled
            .first()
            .filter(|hap| !hap.value.is_nullish())
            .map(|hap| register::value_to_fraction(&hap.value))
            .unwrap_or_else(|| Fraction::from_f64(0.5));
        let Some(length) = length else {
            signal_query_error(|| "Invalid MIDI note length".into());
            return Vec::new();
        };
        let whole = TimeSpan::new(at, at.add(length));
        // The length's own source span comes with it, so the editor can
        // highlight the element that chose it: `keys("0.01 1")` highlights
        // `0.01` or `1`. The note itself comes from a keyboard, not from
        // the score, so no other part of this hap has a source span.
        let context = sampled
            .first()
            .map(|hap| hap.context.clone())
            .unwrap_or_default();
        if !push_budgeted(
            &mut out,
            Hap::new(
                Some(whole),
                whole,
                Value::object([
                    ("note".into(), Value::F64(f64::from(hit.note))),
                    (
                        "velocity".into(),
                        Value::F64(f64::from(hit.velocity) / 127.0),
                    ),
                    ("midichan".into(), Value::F64(f64::from(hit.channel))),
                ]),
            )
            .with_context(context),
        ) {
            return Vec::new();
        }
    }
    out
}

/// The haps `isobite` counts in the cycle of the query: each hap with a value
/// not loosely equal to 0.
fn cycle_onsets(structure: &Pattern, state: &State) -> Vec<Hap> {
    let zero = Value::F64(0.0);
    structure
        .query(&state.set_span(whole_cycle(state.span.begin)))
        .into_iter()
        .filter(|hap| !compose::js_loose_eq(&hap.value, &zero))
        .collect()
}

/// Each onset, cut to the query span and valued by its position in the cycle.
/// Two or more onsets are ordered by the begin of their whole, so an analog
/// hap among them is a query error, as in [`sort_haps_by_part`].
#[inline(never)]
fn query_cycle_onset_index(structure: &Pattern, state: &State) -> Vec<Hap> {
    let mut onsets = cycle_onsets(structure, state);
    if onsets.len() > 1 {
        if onsets.iter().any(|hap| hap.whole().is_none()) {
            signal_query_error(|| {
                "isobite: Cannot read properties of undefined (reading 'begin')".into()
            });
            return Vec::new();
        }
        // Stable: onsets with one begin keep their query order.
        onsets.sort_by_key(|hap| hap.whole().map(|whole| whole.begin));
    }
    onsets
        .into_iter()
        .enumerate()
        .filter_map(|(index, hap)| {
            let part = state.span.intersection(&hap.part())?;
            let mut indexed = hap.with_value(|_| Value::F64(index as f64));
            indexed.set_part(part);
            Some(indexed)
        })
        .collect()
}

/// One hap over the cycle of the query, valued by its onset count.
#[inline(never)]
fn query_cycle_onset_count(structure: &Pattern, state: &State) -> Vec<Hap> {
    let count = cycle_onsets(structure, state).len();
    vec![Hap::new(
        Some(whole_cycle(state.span.begin)),
        state.span,
        Value::F64(count as f64),
    )]
}

/// `withSeed`'s query, out of `query_node`'s recursive frame.
#[inline(never)]
fn query_with_rand_seed(pat: &Pattern, seed: f64, state: &State) -> Vec<Hap> {
    let mut controls = state.controls.clone();
    controls.insert("randSeed".into(), Value::F64(seed));
    pat.query(&state.set_controls(&controls))
}

/// The port name `.midi()` records when its argument did not reach it as a
/// literal string or number.
///
/// Matched by exact equality, before any device is consulted, so it can be
/// something a musician reading a session log can understand rather than an
/// unprintable byte. Carried as a port name, not as a refusal, because a
/// mistyped device must not take the audio down with it.
pub const UNREADABLE_MIDI_PORT: &str = "(unreadable port name)";

/// A pattern that fails when queried, for a `TypeError` that only surfaces at
/// query time.
///
/// `pat.polyBind(42)` is the reachable case: `fmap(42)` constructs without
/// complaint and calls the non-function later, so the host must build
/// something rather than throw. See
/// `crates/jsruntime/tests/bind_ownership.rs`.
pub fn query_error_pattern(message: impl Into<Arc<str>>) -> Pattern {
    Pattern::of(Node::QueryError(message.into()))
}

/// A pure pattern that refuses through the typed resource channel when it is
/// queried.
///
/// Resource limits discovered while constructing a registered combinator
/// cannot set the thread-local refusal immediately: a later top-level query
/// intentionally clears stale refusals on entry. Carrying the typed limit in
/// the graph makes it fire at the boundary that can return it to the caller.
pub fn query_limit_pattern(limit: QueryLimit) -> Pattern {
    Pattern::of(Node::QueryLimit(Box::new(limit)))
}

pub fn ref_pattern(id: CallbackId) -> Pattern {
    pure(Value::F64(1.0))
        .fmap_to_pattern(move |_| host_call_ref(id))
        .inner_join()
}

/// `silence` carries one step (`gap(1)`); `nothing` is `gap(0)`. Without the
/// step count, `polyJoin(silence)` takes the missing-`_steps` throw path,
/// which empties the whole `queryArc`, including the haps of a sibling in
/// the same `sequence`.
pub fn silence() -> Pattern {
    static SILENCE: std::sync::LazyLock<Pattern> =
        std::sync::LazyLock::new(|| Pattern::of(Node::Silence).with_steps(Some(Fraction::ONE)));
    SILENCE.clone()
}

/// `pure(pattern)` - one known inner pattern per cycle. Outer `_steps` is 1,
/// as `pure`'s is.
pub fn pure_pattern(inner: Pattern) -> Pattern {
    Pattern::of(Node::PurePattern(Box::new(inner))).with_steps(Some(Fraction::ONE))
}

/// A pattern-of-patterns whose inner patterns come from a JS callback.
///
/// `polyBind`/`stepBind` both begin with `fmap`, which carries `_steps`.
/// `polyJoin` divides by the outer step count, so without it every bind
/// would take the missing-`_steps` throw path.
pub fn pattern_of_js(outer: Pattern, id: CallbackId) -> Pattern {
    let steps = outer.steps();
    Pattern::of(Node::PatternOfJs(Box::new((outer, id)))).with_steps(steps)
}

pub fn pure(v: Value) -> Pattern {
    Pattern::of(Node::Pure(v)).with_steps(Some(Fraction::ONE))
}

/// [`pure`] carrying pre-packed source locations in one node without an outer
/// `Node::AddContext` wrapper.
pub fn pure_locations(v: Value, locations: Locations) -> Pattern {
    if locations.is_empty() {
        return pure(v);
    }
    Pattern::of(Node::PureAt(Box::new((v, locations)))).with_steps(Some(Fraction::ONE))
}

/// [`pure`] carrying a single `(begin, end)` source location in one node.
pub fn pure_at(v: Value, at: (usize, usize)) -> Pattern {
    let mut locations = Locations::new();
    locations.push(at);
    Pattern::of(Node::PureAt(Box::new((v, locations)))).with_steps(Some(Fraction::ONE))
}

pub fn pure_pick_lookup(value: Value, lookup: PickLookup) -> Pattern {
    Pattern::of(Node::PurePickLookup(Box::new((value, lookup)))).with_steps(Some(Fraction::ONE))
}

/// A continuous value sampled once at the query span begin.
pub fn signal(f: impl Fn(Fraction, &OrderedMap) -> Value + Send + Sync + 'static) -> Pattern {
    Pattern::of(Node::Signal(Arc::new(move |state| {
        f(state.span.begin, &state.controls)
    })))
}

/// The body of a signal of numbers: the number at a time, under the
/// controls of the query.
pub type NumberSignal = fn(Fraction, &OrderedMap) -> f64;

/// Each signal of numbers that [`signal_static`] made, with its body.
static NUMBER_SIGNALS: Mutex<Vec<(NumberSignal, SignalFn)>> = Mutex::new(Vec::new());

/// [`signal`] of numbers for a body that captures nothing: stored inline as
/// `Node::NumberSignal(body)` without closure allocation or mutex lookup.
pub fn signal_static(body: NumberSignal) -> Pattern {
    Pattern::of(Node::NumberSignal(body))
}

/// The body of `signal`, when [`signal_static`] made it (legacy closure fallback).
pub(crate) fn number_signal(signal: &SignalFn) -> Option<NumberSignal> {
    let known = NUMBER_SIGNALS.lock().ok()?;
    known
        .iter()
        .find(|(_, of)| Arc::ptr_eq(of, signal))
        .map(|(body, _)| *body)
}

impl Pattern {
    /// Whether querying this pattern is guaranteed settings-independent solely
    /// from its immutable node flags (excluding `FLAG_HOST_VOICINGS`), so a
    /// runtime can cache this property once when installing an active pattern.
    #[inline]
    pub fn is_query_settings_independent_static(&self) -> bool {
        if self.has_runtime_settings()
            || (self.effective_flags() & (FLAG_VOLATILE | FLAG_HOST_VOICINGS)) != 0
        {
            return false;
        }
        match self.node_kind() {
            Node::IntRange { .. } => true,
            Node::SegmentSignal { body, .. } => !signal::is_rand_body(*body),
            Node::Sequence(layers) => sequence::holds_data(layers),
            Node::Iter(node) => {
                !node.inner.has_runtime_settings()
                    && (node.inner.effective_flags() & (FLAG_VOLATILE | FLAG_HOST_VOICINGS)) == 0
                    && sequence::is_data(&node.inner)
            }
            Node::Fast(node) => {
                !node.0.has_runtime_settings()
                    && (node.0.effective_flags() & (FLAG_VOLATILE | FLAG_HOST_VOICINGS)) == 0
                    && match node.0.node_kind() {
                        Node::IntRange { .. } => true,
                        Node::Sequence(layers) => sequence::holds_data(layers),
                        Node::Iter(it) => {
                            !it.inner.has_runtime_settings()
                                && (it.inner.effective_flags()
                                    & (FLAG_VOLATILE | FLAG_HOST_VOICINGS))
                                    == 0
                                && sequence::is_data(&it.inner)
                        }
                        _ => false,
                    }
            }
            Node::FastSmall(cell, flags, ..)
                if (*flags & (FLAG_VOLATILE | FLAG_HOST_VOICINGS)) == 0 =>
            {
                match &cell.kind {
                    Node::IntRange { .. } => true,
                    Node::Sequence(layers) => sequence::holds_data(layers),
                    Node::Iter(it) => {
                        !it.inner.has_runtime_settings()
                            && (it.inner.effective_flags() & (FLAG_VOLATILE | FLAG_HOST_VOICINGS))
                                == 0
                            && sequence::is_data(&it.inner)
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// Whether querying this pattern cannot read `RuntimeSettings`, cannot
    /// invoke callbacks or throw query errors, and yields data-only values.
    #[inline]
    pub fn is_query_settings_independent(&self) -> bool {
        if self.has_runtime_settings() || self.is_volatile() {
            return false;
        }
        match self.node_kind() {
            Node::IntRange { .. } => true,
            Node::SegmentSignal { body, .. } => !signal::is_rand_body(*body),
            Node::Sequence(layers) => sequence::holds_data(layers),
            Node::Iter(node) => {
                !node.inner.has_runtime_settings()
                    && !node.inner.is_volatile()
                    && sequence::is_data(&node.inner)
            }
            Node::Fast(node) => {
                !node.0.has_runtime_settings()
                    && !node.0.is_volatile()
                    && match node.0.node_kind() {
                        Node::IntRange { .. } => true,
                        Node::Sequence(layers) => sequence::holds_data(layers),
                        Node::Iter(it) => {
                            !it.inner.has_runtime_settings()
                                && !it.inner.is_volatile()
                                && sequence::is_data(&it.inner)
                        }
                        _ => false,
                    }
            }
            Node::FastSmall(cell, flags, ..) if (*flags & FLAG_VOLATILE) == 0 => match &cell.kind {
                Node::IntRange { .. } => true,
                Node::Sequence(layers) => sequence::holds_data(layers),
                Node::Iter(it) => {
                    !it.inner.has_runtime_settings()
                        && !it.inner.is_volatile()
                        && sequence::is_data(&it.inner)
                }
                _ => false,
            },
            _ => false,
        }
    }

    pub(crate) fn pat_segment(&self, rate: Fraction) -> Self {
        if !sequence::builds_each_node()
            && !self.has_runtime_settings()
            && !self.is_volatile()
            && let Node::NumberSignal(body) = *self.node_kind()
            && rate.denom() == 1
            && rate.numer() >= 1
            && let Ok(steps) = u32::try_from(rate.numer())
        {
            return Pattern::of(Node::SegmentSignal { body, steps }).with_steps(Some(rate));
        }
        let pulse = pure(Value::Bool(true)).fast(rate);
        compose::compose(
            self,
            &pulse,
            compose::ComposeOp::KeepIf,
            compose::Alignment::Out,
        )
        .with_steps(Some(rate))
    }
}

pub fn state_signal(f: impl Fn(&State) -> Value + Send + Sync + 'static) -> Pattern {
    Pattern::of(Node::Signal(Arc::new(f)))
}

pub fn midi_keys(lengths: Pattern, port: Arc<midi_in::InputPort>) -> Pattern {
    Pattern::of(Node::MidiKeys(Box::new((lengths, port))))
}

pub fn unjoin(pattern: Pattern, pieces: Pattern, function: Option<&value::FunctionRef>) -> Pattern {
    let source = pattern.clone();
    let transform = function.cloned();
    let mut dependencies = vec![pattern];
    if let Some(function) = &transform {
        dependencies.push(pure(Value::Function(Arc::new(function.clone()))));
    }
    Pattern::of(Node::PatternOfHap(Box::new(HapBind(
        pieces,
        dependencies,
        Arc::new(move |hap| {
            if !hap.value.js_truthy() {
                return source.clone();
            }
            let Some(whole) = hap.whole() else {
                signal_query_error(|| {
                    "Cannot read properties of undefined (reading 'begin')".into()
                });
                return silence();
            };
            let section = combinators::ribbon(&source, whole.begin, whole.duration());
            transform
                .as_ref()
                .map_or(section.clone(), |function| function.apply(section))
        }),
    ))))
}

pub fn chunk_into(pattern: Pattern, count: f64, function: Option<&value::FunctionRef>) -> Pattern {
    if !count.is_finite() || count.fract() != 0.0 || count < 1.0 {
        return query_error_pattern("Invalid array length");
    }
    let parts = count.min(u64::MAX as f64) as u64;
    if parts > combinators::MAX_ITER_PARTS {
        return query_limit_pattern(QueryLimit::IterParts {
            operation: "chunkInto",
            parts,
            limit: combinators::MAX_ITER_PARTS,
        });
    }
    let selector = fastcat(
        std::iter::once(pure(Value::Bool(true)))
            .chain((1..parts).map(|_| pure(Value::Bool(false))))
            .collect(),
    );
    let pieces = combinators::iter(&selector, Fraction::from(i128::from(parts)), true);
    unjoin(pattern, pieces, function).inner_join()
}

pub fn steady(value: Value) -> Pattern {
    signal(move |_, _| value.clone())
}

pub fn choose_cycles(patterns: Vec<Pattern>, seed: u32) -> Pattern {
    if patterns.is_empty() {
        silence()
    } else {
        Pattern::of(Node::ChooseCycles(Box::new((
            patterns.into_boxed_slice(),
            seed,
        ))))
    }
}

/// `wchoose` / `wchooseCycles`.
///
/// Each pair is `(value_pattern, weight_pattern)`. Weights are accumulated
/// through the ordinary patternified `add.in` route, so their temporal
/// structure and query-time errors are preserved. `cycles` switches the
/// chooser from continuous `rand` to `rand.segment(1)` and the final join
/// from outer to inner - the whole difference between the two.
pub fn wchoose(pairs: Vec<(Pattern, Pattern)>, cycles: bool) -> Pattern {
    let mut values = Vec::with_capacity(pairs.len());
    let mut cumulative = Vec::with_capacity(pairs.len());
    let mut total = pure(Value::F64(0.0));
    for (value, weight) in pairs {
        values.push(value);
        total = compose::compose(
            &total,
            &weight,
            compose::ComposeOp::Add,
            compose::Alignment::In,
        );
        cumulative.push(total.clone());
    }
    let chooser = if cycles {
        combinators::segment(&signal::rand(), Fraction::ONE)
    } else {
        signal::rand()
    };
    Pattern::of(Node::WChoose(Box::new(WeightedChoice {
        chooser,
        total,
        cumulative,
        values,
        inner: cycles,
    })))
}

fn pick_compact(
    selector: Pattern,
    lookup: CompactPickLookup,
    index_mode: PickIndexMode,
    mode: JoinMode,
) -> Pattern {
    let steps = (mode == JoinMode::Outer)
        .then_some(selector.steps())
        .flatten();
    Pattern::of(Node::Pick(Box::new(PickNode {
        selector,
        lookup,
        index_mode,
        mode,
    })))
    .with_steps(steps)
}

/// The shared `_pick` core plus its selected join.
pub fn pick(
    selector: Pattern,
    lookup: PickLookup,
    index_mode: PickIndexMode,
    mode: JoinMode,
) -> Pattern {
    if lookup.enumerable_len() == 0 {
        // `_pick` returns `silence`, then the public family still applies its
        // join. Every join drops `_steps`, including on an empty pattern.
        silence().with_steps(None)
    } else {
        pick_compact(
            selector,
            CompactPickLookup::from_lookup(lookup),
            index_mode,
            mode,
        )
    }
}

/// [`pick`] for a dense 0-based array lookup without building an intermediate
/// `Vec<(usize, Pattern)>`.
pub fn pick_dense(
    selector: Pattern,
    patterns: Box<[Pattern]>,
    index_mode: PickIndexMode,
    mode: JoinMode,
) -> Pattern {
    if patterns.is_empty() {
        silence().with_steps(None)
    } else {
        pick_compact(
            selector,
            CompactPickLookup::Dense(patterns),
            index_mode,
            mode,
        )
    }
}

pub fn pick_patternified(
    selector: Pattern,
    lookup: Pattern,
    index_mode: PickIndexMode,
    mode: JoinMode,
) -> Pattern {
    Pattern::of(Node::PickPatternified {
        patterns: Box::new(PatternPair(selector, lookup)),
        index_mode,
        mode,
    })
}

/// `pickF`'s historical argument swap is tested against the value delivered
/// by the first argument pattern for EACH hap. A metadata-aware bind keeps
/// that one query and gives array haps their exact holes/keys lookup.
pub fn pick_f_compat(
    receiver: Pattern,
    first: Pattern,
    second: Pattern,
    index_mode: PickIndexMode,
) -> Pattern {
    let steps = first.steps();
    let receiver_dep = receiver.clone();
    let second_dep = second.clone();
    let mapped = Pattern::of(Node::PatternOfHap(Box::new(HapBind(
        first,
        vec![receiver.clone(), second.clone()],
        Arc::new(move |hap| {
            let functions = if matches!(hap.value, Value::List(_))
                || matches!(hap.value, Value::JsValue(reference) if reference.is_array())
            {
                let Some(lookup) = hap
                    .pick_lookup_arc()
                    .or_else(|| match hap.value {
                        Value::JsValue(reference) => {
                            host_call_pick_lookup(reference.id()).and_then(scoped_pick_lookup)
                        }
                        _ => None,
                    })
                    .or_else(|| PickLookup::from_value(&hap.value).and_then(scoped_pick_lookup))
                else {
                    if query_stop_requested() {
                        return silence();
                    }
                    return query_error_pattern("Cannot convert undefined or null to object");
                };
                pick(
                    second_dep.clone(),
                    lookup.as_ref().clone(),
                    index_mode,
                    JoinMode::Inner,
                )
            } else {
                pick_patternified(
                    pure(hap.value.clone()),
                    second_dep.clone(),
                    index_mode,
                    JoinMode::Inner,
                )
            };
            apply_functions_strict(receiver_dep.clone(), functions)
        }),
    ))))
    .with_steps(steps);
    mapped.inner_join()
}

/// Value-level function application for the JS `appLeft`/`appBoth` surface:
/// the left pattern's values are JS closures (the
/// `withValue((v) => (x) => ...)` idiom); combining calls the closure with
/// the sampled right value. A non-function left value is a query error,
/// exactly like calling a non-function in JavaScript.
fn call_function_value(f: &Value, x: &Value) -> Value {
    match f {
        Value::Function(function) => match function.callback_id() {
            Some(id) => host_call_value(id, x),
            None => {
                signal_query_error(|| "appLeft function has no callable host".into());
                Value::Undefined
            }
        },
        _ => {
            signal_query_error(|| "appLeft left value is not a function".into());
            Value::Undefined
        }
    }
}

/// `left.appLeft(right)` with function-valued left haps.
pub fn app_left_call(left: &Pattern, right: &Pattern) -> Pattern {
    left.app_left_with(right.clone(), call_function_value)
}

/// `left.appBoth(right)` with function-valued left haps.
pub fn app_both_call(left: &Pattern, right: &Pattern) -> Pattern {
    left.app_both_with(right.clone(), call_function_value)
}

/// `pat.apply(function_pattern)` as `pickF` reaches it: a selected
/// non-function is a query-time TypeError, not an identity fallback.
pub fn apply_functions_strict(receiver: Pattern, functions: Pattern) -> Pattern {
    functions
        .fmap_to_pattern(move |value| match value {
            Value::Function(function) => function.apply(receiver.clone()),
            _ => query_error_pattern("func is not a function"),
        })
        .inner_join()
}

/// Step-count lcm. Unknowns are **removed first**, so a single known step
/// count survives rather than poisoning the result; only an all-unknown list
/// yields `None`.
fn lcm_steps(pats: &[Pattern]) -> Result<Option<Fraction>, ()> {
    // The variadic fraction lcm starts with the final known operand. Keep
    // that ordering, including the sign of a lone step count, while refusing
    // any intermediate result that is outside the native representation.
    let mut known = pats.iter().filter_map(|p| p.steps());
    let Some(mut steps) = known.next_back() else {
        return Ok(None);
    };
    for next in known {
        steps = steps.checked_lcm(next).ok_or(())?;
    }
    Ok(Some(steps))
}

#[inline]
fn is_plain_pure_cell(p: &Pattern) -> bool {
    p.extras.is_none() && p.node_flags() == 0
}

#[inline]
fn can_pack_pure_cells(pats: &[Pattern]) -> bool {
    pats.iter().all(is_plain_pure_cell)
}

fn lcm_steps_opt(pats: &[Option<Pattern>]) -> Result<Option<Fraction>, ()> {
    let mut known = pats
        .iter()
        .filter_map(|p| p.as_ref().and_then(Pattern::steps));
    let Some(mut steps) = known.next_back() else {
        return Ok(None);
    };
    for next in known {
        steps = steps.checked_lcm(next).ok_or(())?;
    }
    Ok(Some(steps))
}

#[inline]
fn pack_pure_cells(pats: Vec<Pattern>) -> Box<[Arc<NodeCell>]> {
    pats.into_iter().map(|p| p.node).collect()
}

pub fn stack(pats: Vec<Pattern>) -> Pattern {
    let steps = match lcm_steps(&pats) {
        Ok(steps) => steps,
        Err(()) => {
            return query_limit_pattern(mark_stepwise_refusal(QueryLimit::NativeFraction {
                operation: "stack",
            }));
        }
    };
    if let Some(stacked) = sequence::stacked(&pats) {
        return stacked.with_steps(steps);
    }
    let node = if can_pack_pure_cells(&pats) {
        Node::StackPure(pack_pure_cells(pats))
    } else {
        Node::Stack(pats.into_boxed_slice())
    };
    Pattern::of(node).with_steps(steps)
}

/// Whether a value can carry plugin controls (`vst`/`vsti`) or an opaque
/// JavaScript value that might materialize those keys.
#[inline]
pub(crate) fn value_may_be_plugin_note(value: &Value) -> bool {
    match value {
        Value::JsValue(_) => true,
        Value::Object(controls) => controls.contains_key("vst") || controls.contains_key("vsti"),
        _ => false,
    }
}

impl Pattern {
    /// Whether querying this pattern could produce a hap carrying `vst`/`vsti`
    /// plugin controls or an opaque `JsValue` payload. Conservative: returns
    /// `true` for any impure graph or opaque hap-mapping node.
    pub fn may_have_plugin_notes(&self) -> bool {
        if !self.is_pure() {
            return true;
        }
        let mut stack: Vec<&NodeCell> = Vec::with_capacity(16);
        stack.push(self.node.as_ref());
        while let Some(cell) = stack.pop() {
            match &cell.kind {
                Node::Silence
                | Node::IntRange { .. }
                | Node::Signal(_)
                | Node::NumberSignal(_)
                | Node::SegmentSignal { .. }
                | Node::QueryError(_)
                | Node::QueryLimit(_) => {}
                Node::Iter(node) => stack.push(node.inner.node.as_ref()),
                Node::Pure(val) => {
                    if value_may_be_plugin_note(val) {
                        return true;
                    }
                }
                Node::PureAt(boxed) => {
                    if value_may_be_plugin_note(&boxed.0) {
                        return true;
                    }
                }
                Node::PurePickLookup(boxed) => {
                    if value_may_be_plugin_note(&boxed.0) {
                        return true;
                    }
                }
                Node::Sequence(layers) => {
                    if layers.iter().any(sequence::Layer::may_have_plugin_notes) {
                        return true;
                    }
                }
                Node::Stack(pats) | Node::SlowCat(pats) | Node::SlowCatPrime(pats) => {
                    for p in pats.iter() {
                        stack.push(p.node.as_ref());
                    }
                }
                Node::StackPure(cells)
                | Node::SlowCatPure(cells)
                | Node::SlowCatPrimePure(cells) => {
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
                | Node::CycleOnsetCount(p) => stack.push(p.node.as_ref()),
                Node::Fast(n) => stack.push(n.0.node.as_ref()),
                Node::Shift(n) => stack.push(n.0.node.as_ref()),
                Node::Degrade(b) => stack.push(b.0.node.as_ref()),
                Node::Cpm(b) => stack.push(b.0.node.as_ref()),
                Node::AddContext(b) => stack.push(b.0.node.as_ref()),
                Node::FilterValues(b) => stack.push(b.0.node.as_ref()),
                Node::FilterHaps(b) => stack.push(b.0.node.as_ref()),
                Node::FmapNative(b) => stack.push(b.0.node.as_ref()),
                Node::MapHapsNative(b) => stack.push(b.0.node.as_ref()),
                Node::ExpandHapsNative(b) => stack.push(b.0.node.as_ref()),
                Node::MapHapsWithState(b) => stack.push(b.0.node.as_ref()),
                Node::WithRandSeed(b) => stack.push(b.0.node.as_ref()),
                Node::FastGap(b) => stack.push(b.0.node.as_ref()),
                Node::RepeatCycles(b) => stack.push(b.0.node.as_ref()),
                Node::LoopAt(b) => stack.push(b.0.node.as_ref()),
                Node::ControlMapBoxed(b) => stack.push(b.0.node.as_ref()),
                Node::JoinBoxed(b) => stack.push(b.0.node.as_ref()),
                Node::WithQueryTime(b) | Node::WithHapTime(b) => stack.push(b.0.node.as_ref()),
                Node::WithQuerySpan(b) | Node::WithHapSpan(b) => stack.push(b.0.node.as_ref()),
                Node::InnerJoin(b) => stack.push(b.0.node.as_ref()),
                Node::PatternOfPure(b) => stack.push(b.0.node.as_ref()),
                Node::Timeline(b, _) => {
                    stack.push(b.0.node.as_ref());
                    stack.push(b.1.node.as_ref());
                }
                Node::AppWave(app) => stack.push(app.inner.node.as_ref()),
                Node::UiVisual(node) => stack.push(node.inner.node.as_ref()),
                Node::Constants(node) => {
                    stack.push(node.0.node.as_ref());
                    for m in node.1.iter() {
                        if value_may_be_plugin_note(&m.value) {
                            return true;
                        }
                    }
                }
                Node::AppLeft(a) => {
                    stack.push(a.0.node.as_ref());
                    stack.push(a.1.node.as_ref());
                }
                Node::AppRight(a) | Node::AppBoth(a) => {
                    stack.push(a.0.node.as_ref());
                    stack.push(a.1.node.as_ref());
                }
                Node::Pick(pick) => {
                    stack.push(pick.selector.node.as_ref());
                    for p in pick.lookup.patterns() {
                        stack.push(p.node.as_ref());
                    }
                }
                Node::PickPatternified { patterns, .. } | Node::Arp(patterns) => {
                    stack.push(patterns.0.node.as_ref());
                    stack.push(patterns.1.node.as_ref());
                }
                Node::ChooseCycles(b) => {
                    for p in b.0.iter() {
                        stack.push(p.node.as_ref());
                    }
                }
                Node::WChoose(c) => {
                    stack.push(c.chooser.node.as_ref());
                    stack.push(c.total.node.as_ref());
                    for p in c.cumulative.iter().chain(c.values.iter()) {
                        stack.push(p.node.as_ref());
                    }
                }
                Node::PatternOfPureOperand(b, _) => {
                    stack.push(b.0.node.as_ref());
                    stack.push(b.1.node.as_ref());
                }
                Node::PatternOfHap(b) => {
                    stack.push(b.0.node.as_ref());
                    for p in b.1.iter() {
                        stack.push(p.node.as_ref());
                    }
                }
                Node::XenPatterns(b) => {
                    stack.push(b.0.node.as_ref());
                    for p in b.1.iter() {
                        stack.push(p.node.as_ref());
                    }
                }
                _ => return true,
            }
        }
        false
    }
}

/// Stack top-level score lanes without flattening sequence children into one
/// merged `Node::Sequence`, preserving each lane's `Arc<NodeCell>` identity for
/// incremental edit memo reuse and avoiding cross-track super-period LCM growth.
pub fn stack_lanes(mut pats: Vec<Pattern>) -> Pattern {
    if pats.is_empty() {
        return silence();
    }
    if pats.len() == 1 {
        return pats.pop().expect("single lane");
    }
    let steps = match lcm_steps(&pats) {
        Ok(steps) => steps,
        Err(()) => {
            return query_limit_pattern(mark_stepwise_refusal(QueryLimit::NativeFraction {
                operation: "stack",
            }));
        }
    };
    let node = if can_pack_pure_cells(&pats) {
        Node::StackPure(pack_pure_cells(pats))
    } else {
        Node::Stack(pats.into_boxed_slice())
    };
    Pattern::of(node).with_steps(steps)
}

/// [`stack`] taking patterns from a stack-allocated slice without an
/// intermediate `Vec<Pattern>` heap allocation.
pub fn stack_slice(pats: &mut [Option<Pattern>]) -> Pattern {
    let steps = match lcm_steps_opt(pats) {
        Ok(steps) => steps,
        Err(()) => {
            return query_limit_pattern(mark_stepwise_refusal(QueryLimit::NativeFraction {
                operation: "stack",
            }));
        }
    };
    if let Some(stacked) = sequence::stacked_opt(pats) {
        return stacked.with_steps(steps);
    }
    let node = if pats
        .iter()
        .all(|p| p.as_ref().is_some_and(is_plain_pure_cell))
    {
        Node::StackPure(
            pats.iter_mut()
                .map(|p| p.take().expect("slot present").node)
                .collect(),
        )
    } else {
        Node::Stack(
            pats.iter_mut()
                .map(|p| p.take().expect("slot present"))
                .collect(),
        )
    };
    Pattern::of(node).with_steps(steps)
}

/// [`slowcat`] taking patterns from a stack-allocated slice without an
/// intermediate `Vec<Pattern>` heap allocation.
pub fn slowcat_slice(pats: &mut [Option<Pattern>]) -> Pattern {
    if pats.is_empty() {
        return silence();
    }
    if pats.len() == 1 {
        return pats[0].take().expect("one slowcat input");
    }
    let steps = match lcm_steps_opt(pats) {
        Ok(steps) => steps,
        Err(_) => {
            return query_limit_pattern(mark_stepwise_refusal(QueryLimit::NativeFraction {
                operation: "slowcat",
            }));
        }
    };
    slowcat_slice_with_steps(pats, steps)
}

#[inline]
fn is_plain_pure_data_or_silence(p: &Pattern) -> bool {
    !p.has_runtime_settings()
        && !p.is_volatile()
        && p.pure_loc_raw().is_none()
        && match p.node_kind() {
            Node::Pure(v) => v.is_data(0),
            Node::Silence => true,
            _ => false,
        }
}

fn try_equal_sequence_from_slice(
    pats: &[Option<Pattern>],
    period: Fraction,
    ticks: usize,
    steps: Option<Fraction>,
) -> Option<Pattern> {
    let n = pats.len();
    if !(2..=131_072).contains(&n) {
        return None;
    }
    if !pats
        .iter()
        .all(|p| p.as_ref().is_some_and(is_plain_pure_data_or_silence))
    {
        return None;
    }
    let mut values = Vec::new();
    let mut tick_indices = Vec::new();
    for (idx, slot) in pats.iter().enumerate() {
        if let Some(p) = slot
            && let Node::Pure(v) = p.node_kind()
        {
            values.push(v.clone());
            tick_indices.push(idx as u32);
        }
    }
    let values: Arc<[Value]> = values.into();
    sequence::sequence_of_equal_slots(period, ticks, values, &tick_indices, true)
        .map(|p| p.with_steps(steps))
}

fn try_equal_sequence_from_vec(
    pats: &[Pattern],
    period: Fraction,
    ticks: usize,
    steps: Option<Fraction>,
) -> Option<Pattern> {
    let n = pats.len();
    if !(2..=131_072).contains(&n) || !pats.iter().all(is_plain_pure_data_or_silence) {
        return None;
    }
    let mut values = Vec::new();
    let mut tick_indices = Vec::new();
    for (idx, p) in pats.iter().enumerate() {
        if let Node::Pure(v) = p.node_kind() {
            values.push(v.clone());
            tick_indices.push(idx as u32);
        }
    }
    let values: Arc<[Value]> = values.into();
    sequence::sequence_of_equal_slots(period, ticks, values, &tick_indices, true)
        .map(|p| p.with_steps(steps))
}

fn slowcat_slice_with_steps(pats: &mut [Option<Pattern>], steps: Option<Fraction>) -> Pattern {
    if pats.len() == 1 {
        return pats[0].take().expect("one slowcat input");
    }
    if pats.len() <= 4096
        && let Some(seq) =
            try_equal_sequence_from_slice(pats, Fraction::int(pats.len() as i128), 1, steps)
    {
        return seq;
    }
    let node = if pats
        .iter()
        .all(|p| p.as_ref().is_some_and(is_plain_pure_cell))
    {
        Node::SlowCatPure(
            pats.iter_mut()
                .map(|p| p.take().expect("slot present").node)
                .collect(),
        )
    } else {
        Node::SlowCat(
            pats.iter_mut()
                .map(|p| p.take().expect("slot present"))
                .collect(),
        )
    };
    Pattern::of(node).split_queries().with_steps(steps)
}

/// [`fastcat`] taking patterns from a stack-allocated slice without an
/// intermediate `Vec<Pattern>` heap allocation.
pub fn fastcat_slice(pats: &mut [Option<Pattern>]) -> Pattern {
    let n = pats.len();
    if n == 0 {
        return silence();
    }
    if n > 1 {
        let steps = Some(Fraction::int(n as i128));
        if let Some(seq) = try_equal_sequence_from_slice(pats, Fraction::ONE, n, steps) {
            return seq;
        }
        slowcat_slice_with_steps(pats, None)
            .fast(Fraction::int(n as i128))
            .with_steps(steps)
    } else {
        slowcat_slice(pats)
    }
}

fn slowcat_with_steps(mut pats: Vec<Pattern>, steps: Option<Fraction>) -> Pattern {
    if pats.len() == 1 {
        return pats.pop().expect("one slowcat input");
    }
    if pats.len() <= 4096
        && let Some(seq) =
            try_equal_sequence_from_vec(&pats, Fraction::int(pats.len() as i128), 1, steps)
    {
        return seq;
    }
    let node = if can_pack_pure_cells(&pats) {
        Node::SlowCatPure(pack_pure_cells(pats))
    } else {
        Node::SlowCat(pats.into_boxed_slice())
    };
    Pattern::of(node).split_queries().with_steps(steps)
}

/// One pattern per cycle.
///
/// A single input is returned as-is - identity, not a wrapper. The
/// `split_queries` is essential: without it a query spanning several cycles
/// is served entirely by whichever pattern the *start* cycle selects.
pub fn slowcat(pats: Vec<Pattern>) -> Pattern {
    if pats.is_empty() {
        return silence();
    }
    let steps = match lcm_steps(&pats) {
        Ok(steps) => steps,
        Err(_) => {
            return query_limit_pattern(mark_stepwise_refusal(QueryLimit::NativeFraction {
                operation: "slowcat",
            }));
        }
    };
    slowcat_with_steps(pats, steps)
}

#[inline]
fn append_int_range_locs(loc0: u32, loc1: u32, locs: &[(usize, usize)]) -> Option<(u32, u32)> {
    match locs {
        [] => Some((loc0, loc1)),
        [a] => {
            let p = pack_run_loc(*a)?;
            if loc0 == 0 {
                Some((p, 0))
            } else if loc1 == 0 {
                Some((loc0, p))
            } else {
                None
            }
        }
        [a, b] if loc0 == 0 && loc1 == 0 => Some((pack_run_loc(*a)?, pack_run_loc(*b)?)),
        _ => None,
    }
}

/// Single-cycle equal-stepped integer range `[start, start + (count - 1) * step]`
/// without per-step heap allocations.
pub fn int_range(start: i32, count: u32, step_neg: bool) -> Pattern {
    int_range_at(start, count, step_neg, &[])
}

/// Like [`int_range`], packing up to two source locations directly into the
/// range node so mini ranges build in a single `NodeCell` allocation.
pub fn int_range_at(start: i32, count: u32, step_neg: bool, locs: &[(usize, usize)]) -> Pattern {
    if count == 0 {
        return silence();
    }
    if count == 1 {
        let base = pure(Value::F64(f64::from(start)));
        return if locs.is_empty() {
            base
        } else {
            base.with_added_context_slice(locs)
        };
    }
    if let Some((loc0, loc1)) = append_int_range_locs(0, 0, locs) {
        return Pattern::of(Node::IntRange {
            start,
            count,
            loc0,
            loc1,
            named_id: 0,
            step_neg,
        })
        .with_steps(Some(Fraction::int(i128::from(count))));
    }
    Pattern::of(Node::IntRange {
        start,
        count,
        loc0: 0,
        loc1: 0,
        named_id: 0,
        step_neg,
    })
    .with_steps(Some(Fraction::int(i128::from(count))))
    .with_added_context_slice(locs)
}

/// Fold `every(n, x => x.rev())` / `firstOf(n, rev)` directly onto a sequence
/// pattern in one pass when `2 <= n <= 6`, mutating uniquely-owned nodes in
/// place.
pub fn sequence_every_rev_owned(pattern: Pattern, n: usize) -> Result<Pattern, Pattern> {
    sequence::every_rev_owned(pattern, n)
}

impl Pattern {
    /// Cycle-rotating `iter(times)` / `iterBack(times)` stored compactly as a
    /// single [`Node::Iter`] without duplicating `parts` rotated copies.
    pub(crate) fn pat_iter(&self, times: Fraction, parts: u64, back: bool) -> Pattern {
        if parts == 0 {
            return silence();
        }
        if sequence::builds_each_node() {
            let mut pats = Vec::new();
            if pats.try_reserve_exact(parts as usize).is_err() {
                return query_limit_pattern(QueryLimit::HostMemory);
            }
            for i in 0..parts {
                let offset = Fraction::int(i128::from(i)).div(times);
                pats.push(if back {
                    self.late(offset)
                } else {
                    self.early(offset)
                });
            }
            return slowcat(pats);
        }
        if parts == 1 {
            return self.clone();
        }
        let steps = match self.steps() {
            None => None,
            Some(s) => match s.checked_lcm(s) {
                Some(v) => Some(v),
                None => {
                    return query_limit_pattern(mark_stepwise_refusal(
                        QueryLimit::NativeFraction {
                            operation: "slowcat",
                        },
                    ));
                }
            },
        };
        if self.is_rest() && sequence::folds(self) {
            return silence().with_steps(steps);
        }
        Pattern::of(Node::Iter(Box::new(IterNode {
            inner: self.clone(),
            times,
            parts,
            back,
        })))
        .with_steps(steps)
    }
}

/// Align every pattern's known step grid, concatenate one slowed pattern per
/// cycle, then restore the least-common step rate.
///
/// This is the graph behind the free `zip(...pats)`. Missing
/// step metadata is filtered before construction. For two or more retained
/// operands, both the retained list and the query-split fanout implied by the
/// LCM share the query-wide stepwise allowance.
pub fn zip(mut pats: Vec<Pattern>) -> Pattern {
    pats.retain(|pattern| pattern.steps().is_some());
    if pats.is_empty() {
        return slowcat_with_steps(Vec::new(), None);
    }

    if pats
        .iter()
        .any(|pattern| pattern.steps().is_some_and(|steps| steps.numer() == 0))
    {
        // The JavaScript boundary throws this eagerly. Keep direct core calls
        // panic-free and non-silent if one bypasses that boundary.
        return query_error_pattern("Division by Zero");
    }

    // Validate every `_slow(pat._steps)` reciprocal before building any of
    // the slowed graph. `checked_div` also catches a denominator outside the
    // native Fraction representation (notably a step count of i128::MIN).
    if pats.iter().any(|pattern| {
        Fraction::ONE
            .checked_div(pattern.steps().expect("filtered known steps"))
            .is_none()
    }) {
        let limit = mark_stepwise_refusal(QueryLimit::NativeFraction { operation: "zip" });
        return query_limit_pattern(limit);
    }

    // Variadic `lcm` seeds its fold with the final operand. Preserve the lone
    // operand's sign while making every actual LCM non-negative.
    let (last, rest) = pats.split_last().expect("non-empty zip inputs");
    let mut steps = last.steps().expect("filtered known steps");
    for pattern in rest {
        let Some(next) = steps.checked_lcm(pattern.steps().expect("filtered known steps")) else {
            let limit = mark_stepwise_refusal(QueryLimit::NativeFraction { operation: "zip" });
            return query_limit_pattern(limit);
        };
        steps = next;
    }

    if pats.len() >= 2 {
        let retained = u64::try_from(pats.len()).unwrap_or(u64::MAX);
        let magnitude = steps.numer().unsigned_abs();
        let denominator = steps.denom() as u128;
        let rounded =
            (magnitude / denominator).saturating_add(u128::from(magnitude % denominator != 0));
        let query_splits = u64::try_from(rounded).unwrap_or(u64::MAX);
        let work = retained.max(query_splits);
        if let Err(limit) = charge_stepwise_entries("zip", work) {
            return query_limit_pattern(limit);
        }
    }

    let retained = u64::try_from(pats.len()).unwrap_or(u64::MAX);
    let mut slowed = Vec::new();
    if slowed.try_reserve_exact(pats.len()).is_err() {
        let limit = mark_stepwise_refusal(QueryLimit::HostMemory);
        return query_limit_pattern(limit);
    }
    for pattern in pats {
        let reciprocal = Fraction::ONE
            .checked_div(pattern.steps().expect("filtered known steps"))
            .expect("zip reciprocals were validated before construction");
        slowed.push(pattern.fast(reciprocal));
    }
    note_stepwise_entries_materialised(retained);

    slowcat_with_steps(slowed, Some(steps))
        .fast(steps)
        .with_steps(Some(steps))
}

/// `slowcatPrime` - like `slowcat`, but **skips** cycles instead of shifting
/// them. The cycle index is Euclidean, so negative cycles wrap instead of
/// going silent.
///
/// `every`/`firstOf`/`lastOf` use this, not `slowcat`; the difference is
/// visible the moment the operand has more than one cycle of its own.
pub fn slowcat_prime(pats: Vec<Pattern>) -> Pattern {
    if pats.is_empty() {
        return silence();
    }
    if let Some(seq) = sequence::slowcat_primed(&pats) {
        return seq;
    }
    let node = if can_pack_pure_cells(&pats) {
        Node::SlowCatPrimePure(pack_pure_cells(pats))
    } else {
        Node::SlowCatPrime(pats.into_boxed_slice())
    };
    Pattern::of(node).split_queries()
}

/// All patterns squeezed into one cycle.
///
/// The guard is `> 1`, not `> 0`: a single-element `fastcat` is just that
/// element, and keeps ITS step count rather than being forced to 1.
pub fn fastcat(pats: Vec<Pattern>) -> Pattern {
    let n = pats.len();
    if n == 0 {
        return silence();
    }
    if n > 1 {
        let steps = Some(Fraction::int(n as i128));
        if let Some(seq) = try_equal_sequence_from_vec(&pats, Fraction::ONE, n, steps) {
            return seq;
        }
        // The step count is `n`, so the inputs' step LCM is not computed and
        // cannot refuse.
        slowcat_with_steps(pats, None)
            .fast(Fraction::int(n as i128))
            .with_steps(steps)
    } else {
        slowcat(pats)
    }
}
