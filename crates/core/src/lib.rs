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

use context_keys::ContextKeys;
pub use locations::Locations;
use rustel_fraction::Fraction;
pub use sequence::{
    GivenLayer, IndexedLayer, SequenceBuilder, SequenceLayer, SequenceStep, SourceStep,
    SourcedLayer, StepSource, TickCut, TickStep, sequence, sequence_of, sequence_of_cut_ticks,
    sequence_of_indexed_slice, sequence_of_sourced, sequence_of_ticks,
};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};
pub use table::{DEFAULT_TABLE_BUDGET, set_table_budget, table_bytes};
pub use value::{Key, OrderedMap, Value, WordHasher};

// ---------------------------------------------------------------------------
// TimeSpan
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TimeSpan {
    pub begin: Fraction,
    pub end: Fraction,
}

impl TimeSpan {
    pub fn new(begin: Fraction, end: Fraction) -> Self {
        Self { begin, end }
    }

    /// Splits the span at cycle boundaries, passing each piece to `visit`
    /// without heap allocation. Returns `false` if the span exceeds
    /// [`MAX_QUERY_SPAN_CYCLES`] (after publishing the typed refusal).
    pub(crate) fn for_each_cycle(&self, mut visit: impl FnMut(TimeSpan)) -> bool {
        let mut begin = self.begin;
        let end = self.end;
        if begin == end {
            visit(TimeSpan::new(begin, end));
            return true;
        }
        let Some(count) = self
            .cycle_count()
            .filter(|count| *count <= MAX_QUERY_SPAN_CYCLES)
        else {
            refuse(QueryLimit::QuerySpan {
                cycles: MAX_QUERY_SPAN_CYCLES,
            });
            return false;
        };
        let _ = count;
        let end_sam = end.sam();
        while end > begin {
            if begin.sam() == end_sam {
                visit(TimeSpan::new(begin, end));
                break;
            }
            let next_begin = begin.next_sam();
            visit(TimeSpan::new(begin, next_begin));
            begin = next_begin;
        }
        true
    }

    /// Splits the span at cycle boundaries.
    pub fn span_cycles(&self) -> Vec<TimeSpan> {
        let mut begin = self.begin;
        let end = self.end;
        let end_sam = end.sam();

        // Zero-width spans are supported and yield exactly themselves.
        if begin == end {
            return vec![TimeSpan::new(begin, end)];
        }

        // This loop pushes one entry per cycle, so its allocation is driven by
        // user-controlled span width. `fast(1e30)` would otherwise request a
        // vector with roughly 10^30 entries; no observable result exists for
        // that input, so the runtime refuses it.
        //
        // The failure travels on the typed resource-refusal channel so a
        // caller cannot mistake an uncomputed oversized query for silence.
        let Some(count) = self
            .cycle_count()
            .filter(|count| *count <= MAX_QUERY_SPAN_CYCLES)
        else {
            refuse(QueryLimit::QuerySpan {
                cycles: MAX_QUERY_SPAN_CYCLES,
            });
            return Vec::new();
        };

        // The count is at most the limit above, so it is a size.
        let mut spans = Vec::with_capacity(usize::try_from(count).unwrap_or_default());
        while end > begin {
            if begin.sam() == end_sam {
                spans.push(TimeSpan::new(begin, self.end));
                break;
            }
            let next_begin = begin.next_sam();
            spans.push(TimeSpan::new(begin, next_begin));
            begin = next_begin;
        }
        spans
    }

    /// Stack-backed cycle split for spans covering at most 8 cycles.
    /// Returns `None` when the span covers more than 8 cycles or exceeds limits,
    /// in which case callers fall back to [`Self::span_cycles`].
    #[inline]
    pub(crate) fn span_cycles_small(&self) -> Option<([TimeSpan; 8], usize)> {
        let mut begin = self.begin;
        let end = self.end;
        let zero = TimeSpan::new(Fraction::ZERO, Fraction::ZERO);
        let mut buf = [zero; 8];
        if begin == end {
            buf[0] = TimeSpan::new(begin, end);
            return Some((buf, 1));
        }
        let count = self.cycle_count().filter(|count| *count <= 8)?;
        if count <= 0 {
            return Some((buf, 0));
        }
        let end_sam = end.sam();
        let mut len = 0usize;
        while end > begin {
            if begin.sam() == end_sam {
                buf[len] = TimeSpan::new(begin, end);
                len += 1;
                break;
            }
            let next_begin = begin.next_sam();
            buf[len] = TimeSpan::new(begin, next_begin);
            len += 1;
            begin = next_begin;
        }
        Some((buf, len))
    }

    /// How many entries `span_cycles` produces for a span with a width, or
    /// `None` for a count outside the native range.
    ///
    /// Computed from the endpoints, never by iterating - iteration is the
    /// thing being guarded against.
    fn cycle_count(&self) -> Option<i128> {
        if self.end <= self.begin {
            return Some(0);
        }
        // `span_cycles` emits one entry for every integer cycle bucket the
        // half-open span touches. `ceil(end) - floor(begin)` is that count
        // exactly; using floor(width) allowed one extra allocation whenever
        // either edge was fractional.
        let first_cycle = self.begin.floor().numer();
        let after_last_cycle = self.end.ceil().numer();
        after_last_cycle.checked_sub(first_cycle)
    }

    pub fn duration(&self) -> Fraction {
        self.end.sub(self.begin)
    }

    /// Shift to an equal-duration span whose begin is in cycle zero.
    pub fn cycle_arc(&self) -> TimeSpan {
        let begin = self.begin.cycle_pos();
        TimeSpan::new(begin, begin.add(self.duration()))
    }

    pub fn with_time(&self, f: impl Fn(Fraction) -> Fraction) -> TimeSpan {
        TimeSpan::new(f(self.begin), f(self.end))
    }

    pub fn with_end(&self, f: impl Fn(Fraction) -> Fraction) -> TimeSpan {
        TimeSpan::new(self.begin, f(self.end))
    }

    pub fn with_cycle(&self, f: impl Fn(Fraction) -> Fraction) -> TimeSpan {
        let sam = self.begin.sam();
        TimeSpan::new(
            sam.add(f(self.begin.sub(sam))),
            sam.add(f(self.end.sub(sam))),
        )
    }

    /// Returns `None` when the spans do not intersect.
    ///
    /// Zero-width rule (the classic off-by-one source): a point intersection
    /// does NOT count if it sits at the end of a non-zero-width span.
    pub fn intersection(&self, other: &TimeSpan) -> Option<TimeSpan> {
        if self.begin == other.begin && self.end == other.end {
            return (self.begin <= self.end).then_some(*self);
        }
        let b = self.begin.max(other.begin);
        let e = self.end.min(other.end);
        if b > e {
            return None;
        }
        if b == e {
            if b == self.end && self.begin < self.end {
                return None;
            }
            if b == other.end && other.begin < other.end {
                return None;
            }
        }
        Some(TimeSpan::new(b, e))
    }

    pub fn midpoint(&self) -> Fraction {
        self.begin.add(self.duration().div(Fraction::int(2)))
    }

    pub fn show(&self) -> String {
        format!("{} → {}", self.begin.show(), self.end.show())
    }
}

/// Largest number of cycle splits `span_cycles` will materialise.
///
/// Real queries span a handful of cycles; the offline renderer's longest
/// documented run is far below this. The bound exists so a hostile or mistaken
/// factor fails fast instead of exhausting memory.
pub const MAX_QUERY_SPAN_CYCLES: i128 = 1_000_000;

/// How deep `Pattern::query` may recurse before refusing.
///
/// The limit is sized for unoptimised stack frames, which are substantially
/// larger than release frames. A value of 512 leaves conservative headroom on
/// the CLI's 64 MiB worker stack. Some combinators create several nodes, so
/// a chain of them reaches the limit in fewer than 512 operations.
pub const MAX_PATTERN_DEPTH: u32 = 512;

/// `sortHapsByPart()`, including its throw on analog haps.
///
/// Shared by every entry point that claims this ordering, so a second path
/// cannot quietly use a more tolerant comparator. See
/// [`Pattern::query_arc_sorted`] for why the throw is preserved.
pub fn sort_haps_by_part(haps: Vec<Hap>) -> Vec<Hap> {
    // One element: `Array.prototype.sort` never calls the comparator, so an
    // analog hap survives.
    if haps.len() < 2 {
        return haps;
    }
    if haps.iter().any(|hap| hap.whole.is_none()) {
        signal_query_error(|| {
            "sortHapsByPart: Cannot read properties of undefined (reading 'begin')".into()
        });
        return Vec::new();
    }
    // Every hap is discrete here, so `query_order` compares part begin, part
    // end, whole begin and whole end.
    stable_sorted(haps)
}

/// Sort host results without discarding continuous haps that have no whole span.
/// Explicit `sortHapsByPart()` calls keep the comparator above.
pub fn sort_haps_for_query(haps: Vec<Hap>) -> Vec<Hap> {
    stable_sorted(haps)
}

/// Part begin, part end, then the whole. An analog hap's missing whole
/// sorts first.
#[inline(always)]
fn query_order(a: &Hap, b: &Hap) -> std::cmp::Ordering {
    if a.part.begin != b.part.begin {
        return a.part.begin.cmp(&b.part.begin);
    }
    if a.part.end != b.part.end {
        return a.part.end.cmp(&b.part.end);
    }
    if a.whole == b.whole {
        return std::cmp::Ordering::Equal;
    }
    match (a.whole, b.whole) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(aw), Some(bw)) => {
            if aw.begin != bw.begin {
                aw.begin.cmp(&bw.begin)
            } else {
                aw.end.cmp(&bw.end)
            }
        }
    }
}

/// The most haps that [`stable_sorted`] sorts where they are: what a short
/// span answers, as playback asks one. The answer of a run takes room for
/// as many.
pub(crate) const FEW_HAPS: usize = 64;

/// A float that orders as `time` does where the floats of two times differ:
/// the float of a time whose parts a float holds exactly, which the one
/// rounding of their division keeps in order. `NAN` for any other time,
/// which only the time itself orders.
#[inline(always)]
fn order_key(time: Fraction) -> f64 {
    /// The first whole number that a float skips.
    const EXACT: i128 = 1 << 53;
    match time.numer().abs() < EXACT && time.denom() < EXACT {
        true => time.to_f64(),
        false => f64::NAN,
    }
}

/// Precomputed float keys for [`query_order`] across part begin and whole begin.
#[derive(Clone, Copy)]
struct OrderKey {
    pb: f64,
    wb: f64,
}

impl OrderKey {
    const ZERO: Self = Self { pb: 0.0, wb: 0.0 };

    #[inline(always)]
    fn of(hap: &Hap, prev: Option<(&Hap, Self)>) -> Self {
        let pb = match prev {
            Some((ph, pk)) if ph.part.begin == hap.part.begin => pk.pb,
            _ => order_key(hap.part.begin),
        };
        let wb = match hap.whole {
            None => f64::NEG_INFINITY,
            Some(w) if w.begin == hap.part.begin => pb,
            Some(w) => match prev {
                Some((ph, pk)) if ph.whole.is_some_and(|pw| pw.begin == w.begin) => pk.wb,
                _ => order_key(w.begin),
            },
        };
        Self { pb, wb }
    }
}

#[inline(always)]
fn cmp_order_keys(ak: OrderKey, bk: OrderKey, a: &Hap, b: &Hap) -> std::cmp::Ordering {
    if ak.pb < bk.pb {
        return std::cmp::Ordering::Less;
    }
    if ak.pb > bk.pb {
        return std::cmp::Ordering::Greater;
    }
    if a.part.begin != b.part.begin {
        return a.part.begin.cmp(&b.part.begin);
    }
    if a.part.end != b.part.end {
        return a.part.end.cmp(&b.part.end);
    }
    if ak.wb < bk.wb {
        return std::cmp::Ordering::Less;
    }
    if ak.wb > bk.wb {
        return std::cmp::Ordering::Greater;
    }
    if a.whole == b.whole {
        return std::cmp::Ordering::Equal;
    }
    match (a.whole, b.whole) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(aw), Some(bw)) => {
            if aw.begin != bw.begin {
                aw.begin.cmp(&bw.begin)
            } else {
                aw.end.cmp(&bw.end)
            }
        }
    }
}

#[inline(never)]
fn sort_few_haps<const CAP: usize>(haps: &mut [Hap], first_bad: usize) {
    let n = haps.len();
    let mut keys = [OrderKey::ZERO; CAP];
    keys[0] = OrderKey::of(&haps[0], None);
    for i in 1..n {
        keys[i] = OrderKey::of(&haps[i], Some((&haps[i - 1], keys[i - 1])));
    }
    let mut indices = [0u8; CAP];
    for (slot, dst) in indices[..n].iter_mut().enumerate() {
        *dst = slot as u8;
    }
    for i in first_bad..n {
        let key = indices[i];
        let key_idx = key as usize;
        let key_k = keys[key_idx];
        let key_hap = &haps[key_idx];
        let mut j = i;
        while j > 0 {
            let prev_idx = indices[j - 1] as usize;
            let greater = cmp_order_keys(keys[prev_idx], key_k, &haps[prev_idx], key_hap)
                == std::cmp::Ordering::Greater;
            if !greater {
                break;
            }
            indices[j] = indices[j - 1];
            j -= 1;
        }
        indices[j] = key;
    }
    // Apply the permutation in place by following disjoint cycles so each
    // hap moves once per cycle step with zero heap allocation.
    for i in 0..n {
        let mut dst = i;
        let mut src = indices[dst] as usize;
        if src == dst {
            continue;
        }
        while src != i {
            haps.swap(dst, src);
            indices[dst] = dst as u8;
            dst = src;
            src = indices[dst] as usize;
        }
        indices[dst] = dst as u8;
    }
}

/// A stable sort of `haps` by [`query_order`] that moves each hap once.
///
/// The sort orders indices and then moves each hap once. A hap is large, and
/// a stable sort moves each element many times. Equal haps keep their input
/// order, exactly as `sort_by` keeps them. Precomputing float keys for part
/// and whole begins avoids repeated rational cross-multiplications.
fn stable_sorted(mut haps: Vec<Hap>) -> Vec<Hap> {
    let n = haps.len();
    if n <= 1 {
        return haps;
    }
    if n <= FEW_HAPS {
        let Some(first_bad) =
            (1..n).find(|&at| query_order(&haps[at - 1], &haps[at]) == std::cmp::Ordering::Greater)
        else {
            return haps;
        };
        if n <= 16 {
            sort_few_haps::<16>(&mut haps, first_bad);
        } else {
            sort_few_haps::<FEW_HAPS>(&mut haps, first_bad);
        }
        return haps;
    }
    let Ok(len) = u32::try_from(haps.len()) else {
        haps.sort_by(query_order);
        return haps;
    };
    let mut keys = Vec::with_capacity(haps.len());
    let mut prev: Option<(&Hap, OrderKey)> = None;
    for hap in &haps {
        let k = OrderKey::of(hap, prev);
        keys.push(k);
        prev = Some((hap, k));
    }
    let order = |a: usize, b: usize| cmp_order_keys(keys[a], keys[b], &haps[a], &haps[b]);
    if (1..haps.len()).all(|at| order(at - 1, at) != std::cmp::Ordering::Greater) {
        return haps;
    }
    let mut indices: Vec<u32> = (0..len).collect();
    indices.sort_by(|&a, &b| order(a as usize, b as usize));
    let mut sorted = Vec::with_capacity(haps.len());
    // SAFETY: `indices` is a permutation of `0..haps.len()`, so every hap is
    // read exactly once, and `set_len(0)` makes `haps` forget them before it
    // drops. Nothing between the reads and `set_len` can unwind: `sorted`
    // holds room for every push.
    unsafe {
        for index in indices {
            sorted.push(std::ptr::read(haps.as_ptr().add(index as usize)));
        }
        haps.set_len(0);
    }
    sorted
}

/// The whole cycle containing `t`.
pub fn whole_cycle(t: Fraction) -> TimeSpan {
    TimeSpan::new(t.sam(), t.next_sam())
}

impl TimeSpan {
    /// Whether this span is not empty and ends inside the cycle it begins
    /// in, so [`TimeSpan::span_cycles`] gives it back whole.
    #[inline]
    fn within_one_cycle(&self) -> bool {
        if !(self.begin < self.end) {
            return false;
        }
        let begin_cycle = self.begin.floor_int();
        if self.end.denom() == 1 {
            begin_cycle
                .checked_add(1)
                .is_some_and(|next| self.end.numer() <= next)
        } else {
            // Reduced `end` with `denom > 1` is non-integer, so `end <= begin_cycle + 1`
            // holds iff `end.floor_int() <= begin_cycle`.
            self.end.floor_int() <= begin_cycle
        }
    }
}

// ---------------------------------------------------------------------------
// Hap
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Hap {
    /// `None` means analog/continuous: the value was sampled at the part's
    /// midpoint and has no notional extent.
    pub whole: Option<TimeSpan>,
    pub part: TimeSpan,
    pub value: Value,
    /// Source spans, concatenated on every bind. Drives editor highlighting,
    /// so it must survive every combinator that carries context.
    pub context: Locations,
    /// Optional per-hap metadata (`scale`, `edoSize`, `scaleDefinition`,
    /// `tags`, `logLine`, live slider bindings, visual slots, and JS lookup
    /// shape). Most haps carry none, so this stays a null pointer.
    keys: ContextKeys,
}

const _: () = assert!(std::mem::size_of::<Hap>() == 192);

/// Read a `tags` key off a JS context object: an array of strings, anything
/// else ignored. A score that somehow writes a non-array there loses the tag,
/// not the hap.
fn tags_from_value(value: &Value) -> Option<Arc<[Arc<str>]>> {
    let Value::List(items) = value else {
        return None;
    };
    let tags: Vec<Arc<str>> = items
        .iter()
        .filter_map(|item| match item {
            // A handle of the text.
            Value::Str(text) => Some(text.to_arc()),
            _ => None,
        })
        .collect();
    (!tags.is_empty()).then(|| Arc::from(tags))
}

impl Hap {
    pub fn new(whole: Option<TimeSpan>, part: TimeSpan, value: Value) -> Self {
        Self {
            whole,
            part,
            value,
            context: Locations::new(),
            keys: ContextKeys::default(),
        }
    }

    #[inline]
    pub(crate) fn of_sequence(
        whole: TimeSpan,
        part: TimeSpan,
        value: Value,
        context: Locations,
        keys: ContextKeys,
    ) -> Self {
        Self {
            whole: Some(whole),
            part,
            value,
            context,
            keys,
        }
    }

    fn with_pick_lookup(mut self, lookup: Arc<PickLookup>) -> Self {
        self.keys.edit().pick_lookup = Some(lookup);
        self
    }

    /// Direct live slider bindings: gain, low-pass cutoff, low-pass resonance,
    /// then the direct FM route's index. Zero is unbound.
    #[inline]
    pub fn live_controls(&self) -> [u64; 4] {
        self.keys.get().live_controls
    }

    #[inline]
    pub(crate) fn slider_binding(&self) -> u64 {
        self.keys.get().slider_binding
    }

    #[inline]
    pub(crate) fn set_live_control(&mut self, slot: usize, binding: u64) {
        if binding == 0 && self.keys.is_empty() {
            return;
        }
        self.keys.edit().live_controls[slot] = binding;
        if binding == 0 {
            self.keys.trim();
        }
    }

    #[inline]
    fn set_live_bindings(&mut self, live_controls: [u64; 4], slider_binding: u64) {
        if live_controls == [0; 4] && slider_binding == 0 {
            if !self.keys.is_empty() {
                let keys = self.keys.get();
                if keys.live_controls != [0; 4] || keys.slider_binding != 0 {
                    let edit = self.keys.edit();
                    edit.live_controls = [0; 4];
                    edit.slider_binding = 0;
                    self.keys.trim();
                }
            }
            return;
        }
        let edit = self.keys.edit();
        edit.live_controls = live_controls;
        edit.slider_binding = slider_binding;
    }

    #[inline]
    fn clear_live_bindings(&mut self) {
        self.set_live_bindings([0; 4], 0);
    }

    #[inline]
    fn pick_lookup_arc(&self) -> Option<Arc<PickLookup>> {
        self.keys.get().pick_lookup.clone()
    }

    /// `hap.context.scale`, exactly as `.scale()` tagged it.
    pub fn scale_context(&self) -> Option<&str> {
        self.keys.get().scale.as_deref()
    }

    /// Set the scale context tag.
    pub fn with_scale_context(mut self, scale: Arc<str>) -> Self {
        self.keys.edit().scale = Some(scale);
        self
    }

    /// `hap.context.edoSize`, exactly as `.xen()` tagged it.
    pub fn edo_size_context(&self) -> Option<f64> {
        self.keys.get().edo_size
    }

    /// Every tag this hap carries, oldest first.
    pub fn tags_context(&self) -> Option<&[Arc<str>]> {
        self.keys.get().tags.as_deref()
    }

    /// The text `.log()` will print when this hap triggers, if any.
    pub fn log_line(&self) -> Option<&str> {
        self.keys.get().log_line.as_deref()
    }

    /// `hap.hasTag(t)` - what a `filter` callback asks.
    pub fn has_tag(&self, tag: &str) -> bool {
        self.keys
            .get()
            .tags
            .as_deref()
            .is_some_and(|tags| tags.iter().any(|held| &**held == tag))
    }

    /// Append a tag - never replaces, so tagging twice keeps both.
    pub fn with_tag_context(mut self, tag: Arc<str>) -> Self {
        let mut tags: Vec<Arc<str>> = self.tags_context().unwrap_or_default().to_vec();
        tags.push(tag);
        self.keys.edit().tags = Some(Arc::from(tags));
        self
    }

    /// `.log()`'s already-formatted text, to be printed when this hap
    /// triggers.
    pub fn with_log_line_context(mut self, line: impl Into<Arc<str>>) -> Self {
        self.keys.edit().log_line = Some(line.into());
        self
    }

    pub fn with_edo_size_context(mut self, edo_size: f64) -> Self {
        self.keys.edit().edo_size = Some(edo_size);
        self
    }

    /// `hap.context.scaleDefinition`, as `edoScale` tagged it. Stored behind
    /// an `Arc`; callback-visible JSON matches the raw argument object.
    pub fn scale_definition_context(&self) -> Option<&Value> {
        self.keys.get().scale_definition.as_deref()
    }

    /// Set the scale-definition context tag.
    pub fn with_scale_definition_context(mut self, definition: Arc<Value>) -> Self {
        self.keys.edit().scale_definition = Some(definition);
        self
    }

    pub fn with_context(mut self, ctx: impl Into<Locations>) -> Self {
        self.context = ctx.into();
        self
    }

    pub fn ui_visuals_context(&self) -> u64 {
        self.keys.get().ui_visuals
    }

    pub fn with_ui_visual_slot(mut self, slot: u8) -> Self {
        self.keys.add_ui_visual_slot(slot);
        self
    }

    fn with_ui_visuals_context(mut self, visuals: u64) -> Self {
        if visuals != 0 || !self.keys.is_empty() {
            self.keys.edit().ui_visuals = visuals;
            if visuals == 0 {
                self.keys.trim();
            }
        }
        self
    }

    pub fn pick_lookup(&self) -> Option<&PickLookup> {
        self.keys.get().pick_lookup.as_deref()
    }

    pub fn whole_or_part(&self) -> TimeSpan {
        self.whole.unwrap_or(self.part)
    }

    /// Effective duration, honoring the `duration` and `clip` controls. A clip
    /// whose product with the duration leaves the native fraction range is
    /// ignored, and the un-clipped duration is returned.
    pub fn duration(&self) -> Fraction {
        let mut duration = self
            .value
            .get("duration")
            .and_then(Value::as_f64)
            .and_then(Fraction::from_f64)
            .or_else(|| self.whole.map(|whole| whole.duration()))
            .unwrap_or_else(|| self.part.duration());
        if let Some(clip) = self
            .value
            .get("clip")
            .and_then(Value::as_f64)
            .and_then(Fraction::from_f64)
        {
            duration = duration.checked_mul(clip).unwrap_or(duration);
        }
        duration
    }

    pub fn end_clipped(&self) -> Fraction {
        self.whole_or_part().begin.add(self.duration())
    }

    pub fn is_active(&self, current: Fraction) -> bool {
        let whole = self.whole_or_part();
        whole.begin <= current && self.end_clipped() >= current
    }

    pub fn is_in_past(&self, current: Fraction) -> bool {
        current > self.end_clipped()
    }

    pub fn is_in_near_past(&self, margin: Fraction, current: Fraction) -> bool {
        current.sub(margin) <= self.end_clipped()
    }

    pub fn is_in_future(&self, current: Fraction) -> bool {
        current < self.whole_or_part().begin
    }

    pub fn is_in_near_future(&self, margin: Fraction, current: Fraction) -> bool {
        let begin = self.whole_or_part().begin;
        current < begin && current > begin.sub(margin)
    }

    pub fn is_within_time(&self, min: Fraction, max: Fraction) -> bool {
        self.whole_or_part().begin <= max && self.end_clipped() >= min
    }

    /// Whether the whole begins exactly where the part does.
    pub fn has_onset(&self) -> bool {
        self.whole
            .map(|w| w.begin == self.part.begin)
            .unwrap_or(false)
    }

    pub fn with_span(&self, f: impl Fn(&TimeSpan) -> TimeSpan) -> Hap {
        Hap {
            whole: self.whole.as_ref().map(&f),
            part: f(&self.part),
            value: self.value.clone(),
            context: self.context.clone(),
            keys: self.keys.clone(),
        }
    }

    /// Changes the spans in place. The caller owns the hap, so no value or
    /// context is cloned.
    fn update_span(&mut self, f: impl Fn(&TimeSpan) -> TimeSpan) {
        if let Some(whole) = self.whole.as_mut() {
            *whole = f(whole);
        }
        self.part = f(&self.part);
    }

    /// Haps cut by the same query share the mapped times of its edges, and
    /// chord/stack haps sharing whole boundaries reuse the last mapped value.
    fn map_times(
        haps: &mut [Self],
        span: TimeSpan,
        edges: Option<(Fraction, Fraction)>,
        map: impl Fn(Fraction) -> Fraction,
    ) {
        let mut begin = edges.map(|(b, _)| b);
        let mut end = edges.map(|(_, e)| e);
        let mut last_wb: Option<(Fraction, Fraction)> = None;
        let mut last_we: Option<(Fraction, Fraction)> = None;
        for hap in haps {
            let part = hap.part;
            hap.part.begin = if part.begin == span.begin {
                *begin.get_or_insert_with(|| map(part.begin))
            } else if let Some((raw, mapped)) = last_wb
                && raw == part.begin
            {
                mapped
            } else if let Some((raw, mapped)) = last_we
                && raw == part.begin
            {
                mapped
            } else {
                let mapped = map(part.begin);
                last_wb = Some((part.begin, mapped));
                mapped
            };
            hap.part.end = if part.end == span.end {
                *end.get_or_insert_with(|| map(part.end))
            } else if let Some((raw, mapped)) = last_we
                && raw == part.end
            {
                mapped
            } else if let Some((raw, mapped)) = last_wb
                && raw == part.end
            {
                mapped
            } else {
                let mapped = map(part.end);
                last_we = Some((part.end, mapped));
                mapped
            };
            if let Some(whole) = &mut hap.whole {
                whole.begin = if whole.begin == part.begin {
                    hap.part.begin
                } else if let Some((raw, mapped)) = last_wb
                    && raw == whole.begin
                {
                    mapped
                } else {
                    let mapped = map(whole.begin);
                    last_wb = Some((whole.begin, mapped));
                    mapped
                };
                whole.end = if whole.end == part.end {
                    hap.part.end
                } else if let Some((raw, mapped)) = last_we
                    && raw == whole.end
                {
                    mapped
                } else {
                    let mapped = map(whole.end);
                    last_we = Some((whole.end, mapped));
                    mapped
                };
            }
        }
    }

    /// Replace only the visible part of a hap, retaining its notional whole.
    ///
    /// Native extension nodes use this for the same intersected-child shape
    /// as JavaScript's `new Hap(whole, part, value, context)` constructor.
    pub fn with_part(mut self, part: TimeSpan) -> Hap {
        self.part = part;
        self
    }

    /// JavaScript `hap.combineContext(other)`, retaining this hap's spans and
    /// value while merging the other hap's contextual metadata.
    pub fn combine_context(&self, other: &Hap) -> Hap {
        let mut context = self.context.clone();
        context.append(&other.context);
        let keys = ContextKeys::merge_with(
            &other.keys,
            &self.keys,
            self.live_controls(),
            self.slider_binding(),
            self.pick_lookup_arc(),
        );
        Hap {
            whole: self.whole,
            part: self.part,
            value: self.value.clone(),
            context,
            keys,
        }
    }

    pub fn with_value(&self, f: impl Fn(&Value) -> Value) -> Hap {
        let value = f(&self.value);
        let keys = if self.keys.is_empty() {
            ContextKeys::NONE
        } else {
            let mut keys = self.keys.clone();
            let k = keys.get();
            let drop_lookup = k.pick_lookup.is_some() && value != self.value;
            let clear_live = k.live_controls != [0; 4] || k.slider_binding != 0;
            if drop_lookup || clear_live {
                let edit = keys.edit();
                if drop_lookup {
                    edit.pick_lookup = None;
                }
                edit.live_controls = [0; 4];
                edit.slider_binding = 0;
                keys.trim();
            }
            keys
        };
        Hap {
            whole: self.whole,
            part: self.part,
            value,
            context: self.context.clone(),
            keys,
        }
    }

    /// Changes the value of an owned hap. The context moves instead of
    /// cloning. The lookup shape survives only when the value is unchanged.
    fn map_value(mut self, f: impl FnOnce(&Value) -> Value) -> Hap {
        self.map_value_in_place(f);
        self
    }

    /// [`Self::map_value`] without moving the hap.
    fn map_value_in_place(&mut self, f: impl FnOnce(&Value) -> Value) {
        let value = f(&self.value);
        if !self.keys.is_empty() {
            let k = self.keys.get();
            let drop_lookup = k.pick_lookup.is_some() && value != self.value;
            let clear_live = k.live_controls != [0; 4] || k.slider_binding != 0;
            if drop_lookup || clear_live {
                let edit = self.keys.edit();
                if drop_lookup {
                    edit.pick_lookup = None;
                }
                edit.live_controls = [0; 4];
                edit.slider_binding = 0;
                self.keys.trim();
            }
        }
        self.value = value;
    }

    /// The owned hap with a new value and the same live control bindings.
    /// Public because a caller that interns an [`OwnedHapMapFn`] for
    /// [`Pattern::fmap_keeping_live_with`] must keep exactly these
    /// semantics.
    pub fn into_value_keeping_live(mut self, value: Value) -> Hap {
        if self.keys.get().pick_lookup.is_some() {
            // The lookup shape was of the value before the change.
            self.keys.edit().pick_lookup = None;
            self.keys.trim();
        }
        self.value = value;
        self
    }

    fn without_live_controls(mut self) -> Self {
        self.clear_live_bindings();
        self
    }

    /// Trusted pitch rewrites can carry named audio controls unchanged.
    /// A raw slider value still loses its identity when its pitch changes.
    fn with_pitch_live_controls(mut self, input: &Hap) -> Self {
        self.clear_live_bindings();
        let input_live = input.live_controls();
        if input_live != [0; 4] {
            let mut kept = [0; 4];
            for (index, key) in ["gain", "cutoff", "resonance", "fmi"]
                .into_iter()
                .enumerate()
            {
                if input_live[index] != 0
                    && let Some(before) = input.value.get(key).and_then(Value::as_f64)
                    && self.value.get(key).and_then(Value::as_f64) == Some(before)
                {
                    kept[index] = input_live[index];
                }
            }
            if kept != [0; 4] {
                self.keys.edit().live_controls = kept;
            }
        }
        self
    }

    fn with_unchanged_live_controls(self, input: &Hap) -> Self {
        let mut mapped = self.with_pitch_live_controls(input);
        let binding = input.slider_binding();
        if binding != 0
            && let Some(before) = input.value.as_f64()
            && mapped.value.as_f64() == Some(before)
        {
            mapped.keys.edit().slider_binding = binding;
        }
        mapped
    }

    /// `Hap.show(true)` - the compact snapshot serialiser.
    ///
    /// Sigils: `⇜` whole starts before part · `(…)` fragment · `⇝` whole ends
    /// after part · `~` analog.
    ///
    pub fn show(&self) -> String {
        self.show_with(true)
    }

    pub fn show_with(&self, compact: bool) -> String {
        let value = match &self.value {
            Value::List(_) | Value::Object(_) => {
                if compact {
                    self.value.compact_json()
                } else {
                    self.value
                        .json_stringify()
                        .unwrap_or_else(|| "undefined".into())
                }
            }
            value => value.show(),
        };
        let spans = match self.whole {
            // Compatibility quirk: strudel.cc stringifies the show method
            // itself for analog haps (a missing `()`), and the exact text is
            // observable - so it is reproduced verbatim.
            None => "~show() {\n    return this.begin.show() + ' → ' + this.end.show();\n  }"
                .to_string(),
            Some(w) => {
                let is_whole = w.begin == self.part.begin && w.end == self.part.end;
                let mut s = String::new();
                if w.begin != self.part.begin {
                    s.push_str(&format!("{} ⇜ ", w.begin.show()));
                }
                if !is_whole {
                    s.push('(');
                }
                s.push_str(&self.part.show());
                if !is_whole {
                    s.push(')');
                }
                if w.end != self.part.end {
                    s.push_str(&format!(" ⇝ {}", w.end.show()));
                }
                s
            }
        };
        format!("[ {spans} | {value} ]")
    }

    pub fn show_whole(&self, compact: bool) -> String {
        let span = self
            .whole
            .map(|whole| whole.show())
            .unwrap_or_else(|| "~".into());
        let value = match &self.value {
            Value::List(_) | Value::Object(_) if compact => self.value.compact_json(),
            Value::List(_) | Value::Object(_) => self
                .value
                .json_stringify()
                .unwrap_or_else(|| "undefined".into()),
            value => value.show(),
        };
        format!("{span}: {value}")
    }
}

impl fmt::Debug for Hap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hap")
            .field("whole", &self.whole)
            .field("part", &self.part)
            .field("value", &self.value)
            .field("context", &self.context)
            .finish_non_exhaustive()
    }
}

/// A value-preserving bind may rebuild `pure(value)` to retime the hap. Carry
/// the exact lookup only when the inner result kept the SAME identity-bearing
/// value; a merely equal user value must not inherit hidden Pattern entries.
/// The hap an inner, outer or mix join makes of an outer hap `outer` and one
/// hap `inner` of the pattern it names, at `whole`. See `Node::Join`.
fn joined_hap(outer: &Hap, inner: Hap, whole: Option<TimeSpan>) -> Hap {
    let pick_lookup = joined_pick_lookup(outer, &inner);
    let mut context = outer.context.clone();
    context.append(&inner.context);
    let keys = ContextKeys::merge_with(
        &inner.keys,
        &outer.keys,
        inner.live_controls(),
        inner.slider_binding(),
        pick_lookup,
    );
    Hap {
        whole,
        part: inner.part,
        value: inner.value,
        context,
        keys,
    }
}

fn joined_pick_lookup(outer: &Hap, inner: &Hap) -> Option<Arc<PickLookup>> {
    inner.pick_lookup_arc().or_else(|| {
        (outer.keys.get().pick_lookup.is_some() && inner.value == outer.value)
            .then(|| outer.pick_lookup_arc())
            .flatten()
    })
}

impl fmt::Display for Hap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.show())
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct State {
    pub span: TimeSpan,
    /// Controls carried through the query. `_cps` lives here - `glide`-style
    /// user patterns read it to tell a real trigger from a lookahead query.
    pub controls: OrderedMap,
}

impl State {
    pub fn new(span: TimeSpan) -> Self {
        Self {
            span,
            controls: OrderedMap::new(),
        }
    }
    pub fn set_span(&self, span: TimeSpan) -> Self {
        Self {
            span,
            controls: self.controls.clone(),
        }
    }
    pub fn with_span(&self, f: impl Fn(&TimeSpan) -> TimeSpan) -> Self {
        self.set_span(f(&self.span))
    }

    /// Shallow object spread: existing key order is retained when overwritten,
    /// and newly introduced controls append in source order.
    pub fn set_controls(&self, controls: &OrderedMap) -> Self {
        let mut merged = self.controls.clone();
        for (key, value) in controls {
            merged.insert_key(key.clone(), value.clone());
        }
        Self {
            span: self.span,
            controls: merged,
        }
    }
}

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

const _: () = assert!(std::mem::size_of::<Pattern>() == 24);
// A score builds thousands of nodes, so a node's size is most of a graph's
// memory. A variant that grows every node fails here: box its rare part.
const _: () = assert!(std::mem::size_of::<NodeCell>() <= 24);

/// A rewrite of the pitch of each hap, as [`Pattern::pitched`] applies it.
struct PitchRewrite {
    /// The hap that the rewrite makes of a hap.
    hap: HapMapFn,
    /// The note that the rewrite makes of a note or a step that a hap
    /// holds alone.
    note: sequence::NoteFn,
    /// Context keys added by this rewrite (for example, `scale` tag), avoiding
    /// a dummy hap rewrite per layer during sequence folding.
    keys: Option<context_keys::ContextKeys>,
}

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

// ---------------------------------------------------------------------------
// The JS boundary, from L1's side.
//
// L1 never contains a QuickJS value. A node that invokes user code stores an
// opaque `CallbackId`; the host that can resolve it is provided by L2 and
// reached through a thread-local, because a QuickJS runtime is owned by exactly
// one thread and is not `Send`.
//
// Pure patterns never consult the host. Queries of pure patterns with no
// host installed assert this property.
// ---------------------------------------------------------------------------

/// Opaque index into L2's trace-managed callback sidecar.
pub type CallbackId = usize;

/// What a bind callback is handed.
///
/// `fmap(func)` gives `func` whatever the outer hap holds, and after
/// transpilation that is a pattern as often as it is a scalar: `pure("bd")`
/// transpiles to a pattern-of-patterns, `pure('bd')` stays a string.
pub enum BindArg<'a> {
    Value(&'a Value),
    Pattern(Pattern),
}

/// What a bind callback produced.
///
/// `fmap` replaces the hap's value with the callback's return value. A
/// non-pattern return is therefore the new value, and the join words its
/// error from that value. This is why `stepBind(x => undefined)` and
/// `stepBind(x => 42)` throw different `TypeError`s.
pub enum BindResult {
    Pattern(Pattern),
    Value(Value),
}

/// Implemented by L2 (`rustel-jsruntime`). L1 depends only on this trait.
pub trait CallbackHost {
    /// Report an ordinary query error contained by one stacked child.
    fn log_query_error(&self, _message: &str) {}

    /// `fmap`-style: transform one value.
    fn call_value(&self, id: CallbackId, v: &Value) -> Result<Value, String>;
    fn call_ref(&self, id: CallbackId) -> Result<Pattern, String> {
        Err(format!("callback {id} is not a ref accessor"))
    }
    fn call_pick_lookup(&self, id: CallbackId) -> Result<PickLookup, String> {
        Err(format!("JavaScript value {id} is not a pick lookup"))
    }
    fn call_materialize_value(&self, id: CallbackId) -> Result<Value, String> {
        Err(format!("JavaScript value {id} cannot be materialized"))
    }
    /// A user-authored `new Pattern(state => …)`.
    fn call_query(&self, id: CallbackId, state: &State) -> Result<Vec<Hap>, String>;
    /// A bind callback - `polyBind(x => ...)` / `stepBind(x => ...)`.
    ///
    /// Two things separate this from `call_pattern`, which is a pattern
    /// transformer (`every(4, x => x.fast(2))`):
    ///
    /// * the argument may be either a hap value or a whole pattern, because
    ///   `fmap` passes on whatever the outer hap carried and transpiled
    ///   mini-notation makes that a pattern;
    /// * a return value that is not a pattern is not an `Err` - it becomes
    ///   the hap's value and the failure surfaces only in the join. An error
    ///   here would make `stepBind(x => 42)` (throws during construction)
    ///   indistinguishable from `stepBind(x => { throw })` (constructs, then
    ///   empties the query).
    fn call_bind(&self, id: CallbackId, _arg: BindArg<'_>) -> Result<BindResult, String> {
        Err(format!("callback {id} is not a bind callback"))
    }

    /// A pattern transformer - `every(4, x => x.fast(2))`.
    ///
    /// Defaults to an error so an older host that never sees a function-valued
    /// argument does not have to implement it.
    fn call_pattern(&self, id: CallbackId, _pattern: Pattern) -> Result<Pattern, String> {
        Err(format!("callback {id} is not a pattern transformer"))
    }

    /// A pattern transformer with a bounded native candidate and its original
    /// JavaScript callback retained as the compatibility fallback.
    ///
    /// Hosts that do not implement callback IR remain correct by taking the
    /// ordinary callback path. The runtime that owns the callable decides
    /// whether to execute, dual-run, or bypass the candidate.
    fn call_pattern_ir(
        &self,
        id: CallbackId,
        _program: &callback_ir::PatternTransformProgram,
        pattern: Pattern,
    ) -> Result<Pattern, String> {
        self.call_pattern(id, pattern)
    }

    /// An indexed transformer as `echoWith` uses it.
    ///
    /// Deliberately a batch rather than a unary callback with an extra
    /// argument: every `func(pattern, index)` call runs first, and only then
    /// are the raw results reified `stack`-style (scalars, strings and nested
    /// arrays are all acceptable). The phase boundary lives in L2 because a
    /// later callback may mutate an earlier result or replace the string
    /// parser before any result is reified.
    fn call_pattern_indexed_batch(
        &self,
        id: CallbackId,
        _patterns: Vec<(Pattern, i64)>,
    ) -> Result<Vec<Pattern>, String> {
        Err(format!(
            "callback {id} is not an indexed pattern transformer"
        ))
    }

    /// An `arpWith` callback - receives one congruent chord as `Hap[]` and
    /// returns something `reify()` turns into a pattern.
    ///
    /// The result stays a [`Pattern`] rather than being collapsed to a chosen
    /// value here: callbacks may return `fastcat(...haps)` or `stack(...haps)`,
    /// and that returned pattern's timing is part of `arpWith` semantics.
    fn call_haps(&self, id: CallbackId, _haps: &[Hap]) -> Result<Pattern, String> {
        Err(format!("callback {id} is not a hap-array callback"))
    }
    fn call_hap_predicate(&self, id: CallbackId, _hap: &Hap) -> Result<bool, String> {
        Err(format!("callback {id} is not a hap predicate"))
    }
    /// A `filterValues` predicate - receive one value, answer whether to keep
    /// the hap.
    ///
    /// Distinct from [`CallbackHost::call_value`] so the filter path can
    /// contain a throw per hap (see [`crate::signal_callback_failure`])
    /// without changing `fmap`'s abort-and-empty contract.
    fn call_value_predicate(&self, id: CallbackId, v: &Value) -> Result<bool, String> {
        self.call_value(id, v).map(|value| value.js_truthy())
    }
    fn call_time_predicate(&self, id: CallbackId, _time: Fraction) -> Result<bool, String> {
        Err(format!("callback {id} is not a time predicate"))
    }
    fn call_span_transform(&self, id: CallbackId, _span: TimeSpan) -> Result<TimeSpan, String> {
        Err(format!("callback {id} is not a span transform"))
    }
}

thread_local! {
    /// Erased to a thin pointer + vtable pair with a fabricated lifetime; only
    /// ever set by `with_callback_host`, which guarantees the referent outlives
    /// the scope in which it is readable.
    /// `Cell`, not `RefCell`: queries NEST (a JS callback can query another
    /// pattern), so an outstanding `borrow()` during the inner install would
    /// panic with "RefCell already borrowed". `Cell` has no runtime borrow
    /// state to conflict.
    static CURRENT_HOST: std::cell::Cell<Option<*const (dyn CallbackHost + 'static)>> =
        const { std::cell::Cell::new(None) };
    /// An `arpWith` callback may return a pattern that reaches `arpWith` again.
    /// Count that logical re-entry while the returned pattern is queried;
    /// ordinary native pattern composition remains unlimited by this guard.
    static CALLBACK_RECURSION_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Optional producer-side observation of JavaScript callback crossings.
    /// The pointer is installed only around one profiled query and never
    /// escapes that scope. Nested callback re-entry is counted but timed only
    /// at the outermost boundary so elapsed time is not double-counted.
    static CALLBACK_QUERY_METRICS: std::cell::Cell<Option<*mut CallbackQueryMetrics>> =
        const { std::cell::Cell::new(None) };
    static CALLBACK_QUERY_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    #[cfg(feature = "callback-census")]
    static CALLBACK_QUERY_KIND_SAMPLE_SEQUENCE: std::cell::Cell<u64> =
        const { std::cell::Cell::new(0) };
}

/// A callback-host operation counted during one pattern query.
///
/// This is plain producer-thread data. It is neither shared with nor updated
/// by the audio callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CallbackQueryKind {
    Value,
    Reference,
    PickLookup,
    MaterializeValue,
    Pattern,
    PatternIr,
    IndexedPatternBatch,
    Haps,
    HapPredicate,
    TimePredicate,
    SpanTransform,
    Bind,
    Query,
}

impl CallbackQueryKind {
    pub const ALL: [Self; CALLBACK_QUERY_KIND_COUNT] = [
        Self::Value,
        Self::Reference,
        Self::PickLookup,
        Self::MaterializeValue,
        Self::Pattern,
        Self::PatternIr,
        Self::IndexedPatternBatch,
        Self::Haps,
        Self::HapPredicate,
        Self::TimePredicate,
        Self::SpanTransform,
        Self::Bind,
        Self::Query,
    ];

    const fn index(self) -> usize {
        self as usize
    }
}

const CALLBACK_QUERY_KIND_COUNT: usize = 13;

/// Exact callback crossings grouped by host-boundary operation.
///
/// The fixed-size array keeps collection bounded and allocation-free. Timing
/// remains sampled separately by [`CallbackQueryMetrics`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CallbackQueryKindCounts {
    counts: [u64; CALLBACK_QUERY_KIND_COUNT],
}

impl CallbackQueryKindCounts {
    pub fn get(&self, kind: CallbackQueryKind) -> u64 {
        self.counts[kind.index()]
    }

    pub fn total(&self) -> u64 {
        self.counts.iter().copied().fold(0, u64::saturating_add)
    }

    #[cfg(any(feature = "callback-census", test))]
    fn increment(&mut self, kind: CallbackQueryKind) {
        let count = &mut self.counts[kind.index()];
        *count = count.saturating_add(1);
    }
}

/// Aggregate host-boundary work for one pattern query.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CallbackQueryMetrics {
    calls: u64,
    #[cfg(any(feature = "callback-census", test))]
    kinds: CallbackQueryKindCounts,
    #[cfg(any(feature = "callback-census", test))]
    kinds_sampled: bool,
    busy_nanos: u64,
    outer_calls: u64,
    sampled_outer_calls: u64,
    sampled_busy_nanos: u64,
}

impl CallbackQueryMetrics {
    pub fn calls(&self) -> u64 {
        self.calls
    }

    /// Exact kind counts when this query was selected for the bounded census.
    pub fn kind_sample(&self) -> Option<CallbackQueryKindCounts> {
        #[cfg(any(feature = "callback-census", test))]
        {
            self.kinds_sampled.then_some(self.kinds)
        }
        #[cfg(not(any(feature = "callback-census", test)))]
        {
            None
        }
    }

    /// Estimated time inside the callback host boundary.
    ///
    /// Calls are exact. Timing samples one out of every eight outermost
    /// crossings and scales their mean across the query, avoiding a clock
    /// read around every small callback while retaining a useful phase split.
    pub fn busy_nanos(&self) -> u64 {
        self.busy_nanos
    }
}

const CALLBACK_TIMING_SAMPLE_INTERVAL: u64 = 8;
#[cfg(feature = "callback-census")]
const CALLBACK_KIND_SAMPLE_INTERVAL: u64 = 8;

/// Observe JavaScript callback crossings made by `f`.
///
/// Nested installs restore the previous recorder and depth on every exit,
/// including unwinding. The query owner supplies the storage, so recording is
/// bounded and allocation-free.
pub fn with_callback_query_metrics<R>(
    metrics: &mut CallbackQueryMetrics,
    f: impl FnOnce() -> R,
) -> R {
    #[cfg(feature = "callback-census")]
    let sample_kinds = CALLBACK_QUERY_KIND_SAMPLE_SEQUENCE.with(|sequence| {
        let current = sequence.get();
        sequence.set(current.wrapping_add(1));
        current.is_multiple_of(CALLBACK_KIND_SAMPLE_INTERVAL)
    });
    #[cfg(not(feature = "callback-census"))]
    let sample_kinds = false;
    with_callback_query_metrics_policy(metrics, sample_kinds, f)
}

fn with_callback_query_metrics_policy<R>(
    metrics: &mut CallbackQueryMetrics,
    sample_kinds: bool,
    f: impl FnOnce() -> R,
) -> R {
    struct Guard {
        metrics: *mut CallbackQueryMetrics,
        previous: Option<*mut CallbackQueryMetrics>,
        previous_depth: u32,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            // SAFETY: the caller-owned recorder outlives this complete guard
            // scope and no reference to it crosses a callback.
            let metrics = unsafe { &mut *self.metrics };
            if let Some(busy_nanos) = metrics
                .sampled_busy_nanos
                .saturating_mul(metrics.outer_calls)
                .checked_div(metrics.sampled_outer_calls)
            {
                metrics.busy_nanos = busy_nanos;
            }
            CALLBACK_QUERY_METRICS.with(|slot| slot.set(self.previous));
            CALLBACK_QUERY_DEPTH.with(|slot| slot.set(self.previous_depth));
        }
    }

    *metrics = CallbackQueryMetrics::default();
    #[cfg(any(feature = "callback-census", test))]
    {
        metrics.kinds_sampled = sample_kinds;
    }
    #[cfg(not(any(feature = "callback-census", test)))]
    {
        let _ = sample_kinds;
    }
    let previous = CALLBACK_QUERY_METRICS.with(|slot| slot.replace(Some(metrics)));
    let previous_depth = CALLBACK_QUERY_DEPTH.with(|slot| slot.replace(0));
    let _guard = Guard {
        metrics,
        previous,
        previous_depth,
    };
    f()
}

struct CallbackQueryTimer {
    metrics: Option<*mut CallbackQueryMetrics>,
    started: Option<std::time::Instant>,
    previous_depth: u32,
}

impl CallbackQueryTimer {
    fn enter(kind: CallbackQueryKind) -> Self {
        #[cfg(not(any(feature = "callback-census", test)))]
        let _ = kind;
        let metrics = CALLBACK_QUERY_METRICS.with(|slot| slot.get());
        let Some(metrics) = metrics else {
            return Self {
                metrics: None,
                started: None,
                previous_depth: 0,
            };
        };
        // SAFETY: `with_callback_query_metrics` owns this pointer for the
        // complete dynamic scope. No reference is retained across a callback,
        // so nested re-entry cannot alias a live `&mut`.
        unsafe {
            (*metrics).calls = (*metrics).calls.saturating_add(1);
            #[cfg(any(feature = "callback-census", test))]
            if (*metrics).kinds_sampled {
                (*metrics).kinds.increment(kind);
            }
        }
        let previous_depth = CALLBACK_QUERY_DEPTH.with(|depth| {
            let current = depth.get();
            depth.set(current.saturating_add(1));
            current
        });
        let sampled = if previous_depth == 0 {
            // SAFETY: same scoped pointer contract as above.
            let outer_call = unsafe {
                (*metrics).outer_calls = (*metrics).outer_calls.saturating_add(1);
                (*metrics).outer_calls
            };
            outer_call % CALLBACK_TIMING_SAMPLE_INTERVAL == 1
        } else {
            false
        };
        if sampled {
            // SAFETY: same scoped pointer contract as above.
            unsafe {
                (*metrics).sampled_outer_calls = (*metrics).sampled_outer_calls.saturating_add(1);
            }
        }
        Self {
            metrics: Some(metrics),
            started: sampled.then(std::time::Instant::now),
            previous_depth,
        }
    }
}

impl Drop for CallbackQueryTimer {
    fn drop(&mut self) {
        CALLBACK_QUERY_DEPTH.with(|depth| depth.set(self.previous_depth));
        let (Some(metrics), Some(started)) = (self.metrics, self.started) else {
            return;
        };
        let elapsed = started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        // SAFETY: same scoped pointer contract as `enter`; the recorder still
        // outlives this guard and no mutable reference crosses the callback.
        unsafe {
            (*metrics).sampled_busy_nanos = (*metrics).sampled_busy_nanos.saturating_add(elapsed);
        }
    }
}

#[inline]
fn measure_host_callback<R>(kind: CallbackQueryKind, f: impl FnOnce() -> R) -> R {
    let _timer = CallbackQueryTimer::enter(kind);
    f()
}

// Low enough to unwind safely on a 1 MiB process stack. Self-re-entry must
// become a query error before Rust's non-unwinding stack abort.
const MAX_CALLBACK_RECURSION_DEPTH: usize = 16;

struct CallbackRecursionGuard;

impl CallbackRecursionGuard {
    fn enter() -> Option<Self> {
        CALLBACK_RECURSION_DEPTH.with(|depth| {
            let current = depth.get();
            if current >= MAX_CALLBACK_RECURSION_DEPTH {
                None
            } else {
                depth.set(current + 1);
                Some(Self)
            }
        })
    }
}

impl Drop for CallbackRecursionGuard {
    fn drop(&mut self) {
        CALLBACK_RECURSION_DEPTH.with(|depth| {
            let current = depth.get();
            debug_assert!(current > 0, "callback recursion depth underflow");
            depth.set(current.saturating_sub(1));
        });
    }
}

/// Installs `host` for the duration of `f`. L2 wraps every query in this.
///
/// # Safety
/// The raw pointer never escapes this call: it is stored, used only while `f`
/// runs on this thread, and cleared before returning (including on unwind).
pub fn with_callback_host<R>(host: &dyn CallbackHost, f: impl FnOnce() -> R) -> R {
    struct Guard(Option<*const (dyn CallbackHost + 'static)>);
    impl Drop for Guard {
        fn drop(&mut self) {
            CURRENT_HOST.with(|h| h.set(self.0));
        }
    }
    let prev = CURRENT_HOST.with(|h| {
        // SAFETY: the fabricated 'static lifetime never escapes - the guard
        // restores the previous value before this function returns, including
        // on unwind, so the pointer is unreadable after `host` could die.
        let p: *const (dyn CallbackHost + 'static) =
            unsafe { std::mem::transmute::<*const dyn CallbackHost, _>(host) };
        h.replace(Some(p))
    });
    let _guard = Guard(prev);
    f()
}

/// Whether a callback host is installed on this thread.
pub(crate) fn host_installed() -> bool {
    CURRENT_HOST.with(|host| host.get().is_some())
}

fn host_call_value(id: CallbackId, v: &Value) -> Value {
    CURRENT_HOST.with(|h| match h.get() {
        // SAFETY: set only by `with_callback_host`, which outlives this call.
        // As with `host_call_pattern`: a throw inside the user's function is
        // a query error, not a reason to abort the process.
        Some(p) => match measure_host_callback(CallbackQueryKind::Value, || {
            unsafe { &*p }.call_value(id, v)
        }) {
            Ok(value) => value,
            Err(error) => {
                signal_query_error(move || format!("callback {id} failed: {error}"));
                Value::Undefined
            }
        },
        None => panic!(
            "callback {id} invoked with no host installed. A pattern classified \
             PURE must never reach here: purity is what guarantees it needs no host."
        ),
    })
}

fn host_call_ref(id: CallbackId) -> Pattern {
    CURRENT_HOST.with(|host| match host.get() {
        // SAFETY: set only by `with_callback_host`, which outlives this call.
        Some(pointer) => match measure_host_callback(CallbackQueryKind::Reference, || {
            unsafe { &*pointer }.call_ref(id)
        }) {
            Ok(pattern) => pattern,
            Err(error) => {
                signal_query_error(move || format!("ref callback {id} failed: {error}"));
                silence()
            }
        },
        None => panic!(
            "ref callback {id} invoked with no host installed. A pattern \
             classified PURE must never reach here"
        ),
    })
}

fn host_call_pick_lookup(id: CallbackId) -> Option<PickLookup> {
    CURRENT_HOST.with(|host| match host.get() {
        Some(pointer) => {
            match measure_host_callback(CallbackQueryKind::PickLookup, || {
                unsafe { &*pointer }.call_pick_lookup(id)
            }) {
                Ok(lookup) => Some(lookup),
                Err(error) => {
                    signal_query_error(move || error);
                    None
                }
            }
        }
        None => panic!("JavaScript lookup {id} reached with no host installed"),
    })
}

pub fn materialize_js_value(value: &Value) -> Value {
    match value {
        Value::JsValue(reference) => CURRENT_HOST.with(|host| match host.get() {
            Some(pointer) => {
                match measure_host_callback(CallbackQueryKind::MaterializeValue, || {
                    unsafe { &*pointer }.call_materialize_value(reference.id())
                }) {
                    Ok(value) => value,
                    Err(error) => {
                        signal_query_error(move || error);
                        Value::Undefined
                    }
                }
            }
            None => {
                // A top-level query materialises JS-owned containers before
                // its host scope unwinds. Keep this fallback non-fatal for
                // raw-query callers: rendering a hap must never turn a
                // live-coding value into a Rust process abort merely because
                // it crossed the wrong phase boundary.
                signal_query_error(|| {
                    format!(
                        "JavaScript value {} reached with no host installed",
                        reference.id()
                    )
                });
                Value::Undefined
            }
        }),
        // Controls can wrap a JS-owned value before the outer boundary sees
        // it (`s(pure(['bd']))` is `{s: <opaque array>}`). Recurse through the
        // native container or the inner identity would still escape the host.
        Value::List(values) => Value::list(values.iter().map(materialize_js_value).collect()),
        Value::Object(values) => Value::object(
            values
                .iter()
                .map(|(key, value)| (key.to_string(), materialize_js_value(value))),
        ),
        Value::Haps(haps) => Value::Haps(value::HapList::new(
            haps.as_slice()
                .iter()
                .map(|hap| hap.with_value(materialize_js_value))
                .collect(),
        )),
        _ => value.clone(),
    }
}

/// Apply a user JavaScript function to a pattern, through the host.
///
/// `every(4, x => x.fast(2))` puts the closure in a hap; this is where it is
/// called. Like the other host entry points, a pattern classified PURE must
/// never reach here.
pub(crate) fn host_call_pattern(id: CallbackId, pattern: Pattern) -> Pattern {
    CURRENT_HOST.with(|h| match h.get() {
        // SAFETY: as above.
        Some(p) => match measure_host_callback(CallbackQueryKind::Pattern, || {
            unsafe { &*p }.call_pattern(id, pattern)
        }) {
            Ok(result) => result,
            // A throw inside the user's function is a user error. It reaches
            // the `queryArc` boundary, which yields no haps. A panic here
            // would end the process for a mistake in a live-coded expression.
            // A missing host is a false-pure classification, and it panics
            // below.
            Err(error) => {
                signal_query_error(move || format!("pattern callback {id} failed: {error}"));
                silence()
            }
        },
        None => panic!(
            "pattern callback {id} invoked with no host installed. A pattern \
             classified PURE must never reach here: purity is what guarantees it needs no host."
        ),
    })
}

/// Apply a bounded native callback candidate through its owning host.
///
/// The host retains the original JavaScript function and owns selection and
/// differential policy. A missing host remains a false-pure classification,
/// exactly like the compatibility-only pattern callback path.
///
/// Only the identity callback was a candidate before method chains, and its
/// failures keep their wording. A chain stands for an ordinary callback, so a
/// failure of its JavaScript fallback reads as that callback's would.
pub(crate) fn host_call_pattern_ir(
    id: CallbackId,
    program: &callback_ir::PatternTransformProgram,
    pattern: Pattern,
) -> Pattern {
    CURRENT_HOST.with(|host| match host.get() {
        // SAFETY: set only by `with_callback_host`, which outlives this call.
        Some(pointer) => match measure_host_callback(CallbackQueryKind::PatternIr, || {
            unsafe { &*pointer }.call_pattern_ir(id, program, pattern)
        }) {
            Ok(result) => result,
            Err(error) if program.is_empty() => {
                signal_query_error(move || format!("pattern callback IR {id} failed: {error}"));
                silence()
            }
            Err(error) => {
                signal_query_error(move || format!("pattern callback {id} failed: {error}"));
                silence()
            }
        },
        None => panic!(
            "pattern callback IR {id} invoked with no host installed. A pattern \
             classified PURE must never reach here: purity is what guarantees it needs no host."
        ),
    })
}

/// Apply one JavaScript callback to a complete indexed batch.
///
/// `echoWith`'s two phases live behind this one host call: JavaScript invokes
/// every callback first, then performs `stack`-style reification of every raw
/// result. A throw stops the batch and becomes the same query error as the
/// unary transformer path; a missing host remains a hard purity failure.
pub(crate) fn host_call_pattern_indexed_batch(
    id: CallbackId,
    patterns: Vec<(Pattern, i64)>,
) -> Vec<Pattern> {
    CURRENT_HOST.with(|host| match host.get() {
        // SAFETY: set only by `with_callback_host`, which outlives this call.
        Some(pointer) => match measure_host_callback(CallbackQueryKind::IndexedPatternBatch, || {
            unsafe { &*pointer }.call_pattern_indexed_batch(id, patterns)
        }) {
            Ok(result) => result,
            Err(error) => {
                signal_query_error(move || {
                    format!("indexed pattern callback {id} failed: {error}")
                });
                Vec::new()
            }
        },
        None => panic!(
            "indexed pattern callback {id} invoked with no host installed. A pattern \
             classified PURE must never reach here: purity is what guarantees it needs no host."
        ),
    })
}

/// Apply an `arpWith` callback to one congruent chord through the host.
///
/// A throw is a query error; the outer `queryArc` boundary discards the
/// entire partial query. A missing host
/// remains a hard purity failure.
fn host_call_haps(id: CallbackId, haps: &[Hap]) -> Pattern {
    CURRENT_HOST.with(|h| match h.get() {
        // SAFETY: set only by `with_callback_host`, which outlives this call.
        Some(p) => match measure_host_callback(CallbackQueryKind::Haps, || {
            unsafe { &*p }.call_haps(id, haps)
        }) {
            Ok(result) => result,
            Err(error) => {
                signal_query_error(move || format!("hap-array callback {id} failed: {error}"));
                silence()
            }
        },
        None => panic!(
            "hap-array callback {id} invoked with no host installed. A pattern \
             classified PURE must never reach here: purity is what guarantees it needs no host."
        ),
    })
}

/// Apply a user JavaScript `filterHaps` predicate through the host.
///
/// A throw inside the predicate does NOT abort the query: the hap it was
/// asked to judge keeps playing (fail open), the failure is recorded on the
/// contained-callback-failure channel, and the query's caller reports it
/// once. On stage, one broken predicate must not stop the set.
fn host_call_hap_predicate(id: CallbackId, hap: &Hap) -> bool {
    CURRENT_HOST.with(|host| match host.get() {
        Some(pointer) => match measure_host_callback(CallbackQueryKind::HapPredicate, || unsafe {
            &*pointer
        }
        .call_hap_predicate(id, hap)) {
            Ok(result) => result,
            Err(error) => {
                signal_callback_failure(move || format!("hap predicate {id} failed: {error}"));
                true
            }
        },
        None => panic!(
            "hap predicate {id} invoked with no host installed. A pattern classified PURE must never reach here"
        ),
    })
}

/// Apply a user JavaScript `filterValues` predicate through the host, with
/// the same contain-and-fail-open contract as [`host_call_hap_predicate`].
fn host_call_value_predicate(id: CallbackId, v: &Value) -> bool {
    CURRENT_HOST.with(|host| match host.get() {
        Some(pointer) => match measure_host_callback(CallbackQueryKind::Value, || {
            unsafe { &*pointer }.call_value_predicate(id, v)
        }) {
            Ok(keep) => keep,
            Err(error) => {
                signal_callback_failure(move || format!("callback {id} failed: {error}"));
                true
            }
        },
        None => panic!(
            "callback {id} invoked with no host installed. A pattern classified \
             PURE must never reach here: purity is what guarantees it needs no host."
        ),
    })
}

fn host_call_time_predicate(id: CallbackId, time: Fraction) -> bool {
    CURRENT_HOST.with(|host| match host.get() {
        Some(pointer) => match measure_host_callback(CallbackQueryKind::TimePredicate, || unsafe {
            &*pointer
        }
        .call_time_predicate(id, time)) {
            Ok(result) => result,
            Err(error) => {
                signal_query_error(move || format!("time predicate {id} failed: {error}"));
                false
            }
        },
        None => panic!(
            "time predicate {id} invoked with no host installed. A pattern classified PURE must never reach here"
        ),
    })
}

fn host_call_span_transform(id: CallbackId, span: TimeSpan) -> TimeSpan {
    CURRENT_HOST.with(|host| match host.get() {
        Some(pointer) => match measure_host_callback(CallbackQueryKind::SpanTransform, || unsafe {
            &*pointer
        }
        .call_span_transform(id, span)) {
            Ok(result) => result,
            Err(error) => {
                signal_query_error(move || format!("span transform {id} failed: {error}"));
                span
            }
        },
        None => panic!(
            "span transform {id} invoked with no host installed. A pattern classified PURE must never reach here"
        ),
    })
}

/// Apply a user JavaScript bind callback, through the host.
///
/// A non-pattern return becomes the hap's value, and the join fails on it
/// later. Only a throw (or an argument that is not callable) is a query
/// error here: `fmap` never inspects what the callback returns.
pub(crate) fn host_call_bind(id: CallbackId, arg: BindArg<'_>) -> BindResult {
    CURRENT_HOST.with(|h| match h.get() {
        // SAFETY: as above.
        Some(p) => match measure_host_callback(CallbackQueryKind::Bind, || {
            unsafe { &*p }.call_bind(id, arg)
        }) {
            Ok(result) => result,
            // A throw inside the user's function is user error: it unwinds to
            // the `queryArc` boundary, which yields no haps. Reporting a
            // PATTERN rather than a value keeps this distinct from a
            // non-pattern return, which does NOT stop the query here.
            Err(error) => {
                signal_query_error(move || format!("bind callback {id} failed: {error}"));
                BindResult::Pattern(silence())
            }
        },
        None => panic!(
            "bind callback {id} invoked with no host installed. A pattern \
             classified PURE must never reach here: purity is what guarantees it needs no host."
        ),
    })
}

fn host_call_query(id: CallbackId, state: &State) -> Vec<Hap> {
    CURRENT_HOST.with(|h| match h.get() {
        // SAFETY: as above.
        Some(p) => match measure_host_callback(CallbackQueryKind::Query, || {
            unsafe { &*p }.call_query(id, state)
        }) {
            Ok(haps) => haps,
            Err(error) => {
                signal_query_error(move || format!("query callback {id} failed: {error}"));
                Vec::new()
            }
        },
        None => panic!(
            "query callback {id} invoked with no host installed. A pattern \
             classified PURE must never reach here: purity is what guarantees it needs no host."
        ),
    })
}

/// How an `app*` node makes one value of a function value and an argument
/// value.
#[derive(Clone)]
pub(crate) enum Combine {
    /// [`compose::compose_op`] of this op, which a meeting can run on the
    /// structure hap's value where it is. See [`Meeting::in_place`].
    Composed(compose::ComposeOp),
    /// Multiple constant `set` controls merged into one union map.
    SetMerged,
    /// Any other function, boxed so [`Combine`] stays 16 bytes.
    With(Box<CombineFn>),
}

const _: () = assert!(std::mem::size_of::<Combine>() == 16);

/// A function that makes one value of a function value and an argument
/// value.
pub(crate) type CombineFn = Arc<dyn Fn(&Value, &Value) -> Value + Send + Sync>;

impl Combine {
    #[inline]
    pub(crate) fn with(combine: CombineFn) -> Self {
        Self::With(Box::new(combine))
    }

    fn apply(&self, function: &Value, value: &Value) -> Value {
        match self {
            Combine::Composed(op) => compose::compose_op(*op, function, value),
            Combine::SetMerged => compose::compose_op(compose::ComposeOp::Set, function, value),
            Combine::With(apply) => apply(function, value),
        }
    }

    /// The compose op that a meeting can run on the structure hap's value
    /// where that value is, when this function is one.
    fn in_place_op(&self) -> Option<compose::ComposeOp> {
        match self {
            Combine::Composed(op) => Some(*op),
            Combine::SetMerged => Some(compose::ComposeOp::Set),
            Combine::With(_) => None,
        }
    }
}

/// Whether a per-hap map looks at the spans of the hap it is given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpanUse {
    /// The map may read or replace the spans.
    Reads,
    /// The map keeps the spans and reads none of them. Its result for a hap
    /// is then the same wherever the hap sits in time, so a precomputed
    /// table may run it once per entry.
    Ignores,
}
/// Maps a value to a whole pattern, for a `PatternOf` awaiting a join.
type ToPatternFn = Arc<dyn Fn(&Value) -> Pattern + Send + Sync>;
type HapToPatternFn = Arc<dyn Fn(&Hap) -> Pattern + Send + Sync>;
/// A closure whose RETURN TYPE proves it cannot materialise JavaScript.
type ToPurePatternFn = Arc<dyn Fn(&Value) -> purity::PurePattern + Send + Sync>;

/// The payload of a [`Node::PatternOfPureOperand`]: a registered body and
/// the chain width it unpacks values to.
///
/// Both come from the process-wide registry table and the source text, so
/// two evaluations of the same call make equal payloads. The pattern the
/// body reads is NOT part of the payload: it rides in the node as a child
/// the adopt walk compares structurally, which is what lets an unchanged
/// operand carry its answers across evaluations and edits. A host that
/// rebuilt its combinator registry per evaluation would mint fresh body
/// objects and lose that carry, never wrong answers; every current host
/// installs the registry once per runtime.
pub struct PatternedPureMap {
    body: register::PureBody,
    chain: usize,
    /// The constant arguments of the call, each at its position, with
    /// `None` at a position that a value of the chain fills. Empty when
    /// every argument rides in the chain.
    constants: Box<[Option<Value>]>,
}

impl PatternedPureMap {
    fn apply(&self, value: &Value, operand: &Pattern) -> purity::PurePattern {
        let pure = operand
            .as_pure_pattern()
            .expect("a PatternOfPureOperand is built from a pure operand");
        if self.chain == 1 {
            if self.constants.is_empty() {
                return (self.body)(std::slice::from_ref(value), pure);
            }
            let values: Vec<Value> = self
                .constants
                .iter()
                .map(|constant| match constant {
                    Some(c) => c.clone(),
                    None => value.clone(),
                })
                .collect();
            return (self.body)(&values, pure);
        }
        let values = register::argument_values(value, self.chain, &self.constants);
        (self.body)(&values, pure)
    }
}

/// The most constructions one `PatternOfPure` node keeps.
///
/// Argument values repeat: a patterned argument such as `"0,1,2,3"` names
/// four values, and every query of every cycle asks the same four again. A
/// bind's carrier repeats even more - `pure(x).inner_bind(f)` asks about
/// `x` once per cycle per query. Keeping the answers is what lets the graph
/// they build (and any cache inside it) live across queries instead of being
/// rebuilt and thrown away each time.
const CONSTRUCTION_MEMO_ENTRIES: usize = 32;

/// The rate of a `cpm`, in cycles each minute, and the speed change that
/// it makes of the tempo of the scheduler.
pub(crate) struct TempoRate {
    pub(crate) cpm: Fraction,
    /// The speed change at the tempo that asked first, with the bits of
    /// that tempo. A float reads as a fraction slowly, and a session seldom
    /// changes its tempo.
    read: std::sync::OnceLock<(u64, Option<Fraction>, Option<TempoGrid>)>,
}

/// A sixteenth of a child cycle as ticks of the scheduler. Borrows its
/// reduction table from the thread-local cache so a `cpm` node holds no
/// `TickGrid` vector on the heap.
#[derive(Clone, Copy)]
struct TempoGrid {
    denom: std::num::NonZeroU32,
    cycle_ticks: i64,
}

impl TempoGrid {
    const STEPS: i64 = 16;

    fn new(speed: Fraction) -> Option<Self> {
        if speed <= Fraction::ZERO || speed == Fraction::ONE {
            return None;
        }
        let tick = Fraction::new(1, i128::from(Self::STEPS)).checked_div(speed)?;
        let denom_i64 = i64::try_from(tick.denom()).ok()?;
        if denom_i64 <= 0 || denom_i64 > rustel_fraction::TickGrid::MAX_DENOM {
            return None;
        }
        let denom = std::num::NonZeroU32::new(u32::try_from(denom_i64).ok()?)?;
        Some(Self {
            denom,
            cycle_ticks: i64::try_from(tick.numer()).ok()?.checked_mul(Self::STEPS)?,
        })
    }

    #[inline]
    fn time(&self, time: Fraction) -> Option<Fraction> {
        let d = time.denom();
        if d > i128::from(Self::STEPS) || !(d as u32).is_power_of_two() {
            return None;
        }
        let n = i64::try_from(time.numer()).ok()?;
        let ticks = n.checked_mul(self.cycle_ticks >> d.trailing_zeros())?;
        Some(sequence::with_tick_grid_if(
            self.denom,
            false,
            |grid| match grid {
                Some(grid) => grid.time(ticks),
                None => Fraction::from_ticks(i128::from(ticks), self.denom),
            },
        ))
    }
}

impl TempoRate {
    pub(crate) fn new(cpm: Fraction) -> Self {
        Self {
            cpm,
            read: std::sync::OnceLock::new(),
        }
    }

    /// How many times as fast the pattern runs at `cps` cycles each second
    /// of the scheduler. `None` for a speed outside the native range.
    fn speed_at(&self, cps: f64) -> Option<(Fraction, Option<&TempoGrid>)> {
        let read = || Fraction::from_f64(self.cpm.to_f64() / 60.0 / cps);
        let (kept, speed, grid) = self.read.get_or_init(|| {
            let speed = read();
            (cps.to_bits(), speed, speed.and_then(TempoGrid::new))
        });
        match *kept == cps.to_bits() {
            true => Some(((*speed)?, grid.as_ref())),
            false => Some((read()?, None)),
        }
    }
}

/// Patterns a `PatternOfPure` closure has already built, by the value it was
/// asked about and the settings snapshot it was asked under.
///
/// Exact: a native body is a function of its argument values and the
/// selected settings, so the pattern it built for a value IS the pattern it
/// would build again. NaN never equals itself and is never kept; a volatile
/// construction (one that would start fresh memory each time) is not kept
/// either, so sharing cannot change what it answers. Bounded, and drained
/// onto the destruction worklist like any other child.
#[derive(Default)]
pub(crate) struct ConstructionMemo {
    entries: Mutex<Constructions>,
    /// Round-robin victim once full. The key set is a handful of argument
    /// values, so plain replacement is enough and never thrashes on it.
    victim: std::sync::atomic::AtomicUsize,
}

/// What a [`ConstructionMemo`] holds.
#[derive(Default)]
struct Constructions {
    kept: Vec<ConstructionEntry>,
}

struct ConstructionEntry {
    value: Value,
    settings: settings::SettingsStateId,
    /// The standings of the compiled programs the construction applied. See
    /// [`callback_ir::recording_reads`].
    read: Box<[Arc<callback_ir::Standing>]>,
    pattern: purity::PurePattern,
}

/// Whether two memo keys name the same construction.
///
/// Stricter than `Value`'s equality for numbers: `-0.0 == 0.0` there, while a
/// body may well tell them apart (`1 / x`, or how the value prints), so the
/// bits have to agree. Containers compare element by element the same way;
/// everything else as `Value` compares.
fn same_key(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::F64(x), Value::F64(y)) => x.to_bits() == y.to_bits(),
        (Value::List(xs), Value::List(ys)) => {
            xs.len() == ys.len() && xs.iter().zip(ys.iter()).all(|(x, y)| same_key(x, y))
        }
        (Value::Object(xs), Value::Object(ys)) => {
            xs.len() == ys.len()
                && xs
                    .iter()
                    .zip(ys.iter())
                    .all(|((kx, x), (ky, y))| kx == ky && same_key(x, y))
        }
        _ => a == b,
    }
}

/// Whether a value may serve as a memo key: numbers, strings, booleans and
/// nothings, and lists and objects of those. A NaN could never be found
/// again; a pattern, function, hap list or host-owned value can hold a graph,
/// and a key holding a graph is how a cycle would form.
fn plain_key(value: &Value) -> bool {
    match value {
        Value::Undefined | Value::Null | Value::Bool(_) | Value::Str(_) => true,
        Value::F64(number) => !number.is_nan(),
        Value::List(items) => items.iter().all(plain_key),
        Value::Object(map) => map.iter().all(|(_, item)| plain_key(item)),
        Value::Pattern(_) | Value::JsValue(_) | Value::Function(_) | Value::Haps(_) => false,
    }
}

impl ConstructionMemo {
    fn lock(&self) -> std::sync::MutexGuard<'_, Constructions> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The pattern built for `value` under `settings`, building it with
    /// `construct` when nothing kept answers.
    fn construct(
        &self,
        value: &Value,
        settings: &settings::SettingsStateId,
        construct: impl FnOnce(&Value) -> purity::PurePattern,
    ) -> purity::PurePattern {
        {
            let constructions = self.lock();
            if let Some(kept) = constructions.kept.iter().find(|entry| {
                entry.settings == *settings
                    && same_key(&entry.value, value)
                    && callback_ir::reads_hold(&entry.read)
            }) {
                callback_ir::note_read(&kept.read);
                return kept.pattern.clone();
            }
        }
        // Built outside the lock: construction is arbitrary native code.
        let (built, read) = callback_ir::recording_reads(|| construct(value));
        let keep = plain_key(value)
            && !built.pattern().is_volatile()
            && !query_error_pending()
            && !budget_exhausted();
        if keep {
            let entry = ConstructionEntry {
                value: value.clone(),
                settings: settings.clone(),
                read,
                pattern: built.clone(),
            };
            let mut constructions = self.lock();
            if constructions.kept.len() < CONSTRUCTION_MEMO_ENTRIES {
                // Most memos hold a single value; allocate exact capacity on
                // the first insert rather than Rust's default 4-slot growth.
                if constructions.kept.is_empty() {
                    constructions.kept.reserve_exact(1);
                }
                constructions.kept.push(entry);
            } else {
                let victim = self
                    .victim
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    % CONSTRUCTION_MEMO_ENTRIES;
                constructions.kept[victim] = entry;
            }
        }
        built
    }

    /// Hand every kept pattern to the destruction worklist.
    fn drain_into(&self, out: &mut Vec<Pattern>) {
        let mut constructions = self.lock();
        out.extend(constructions.kept.drain(..).map(|entry| entry.pattern.0));
    }
}
/// A native value transform.
type ValueFn = Arc<dyn Fn(&Value) -> Value + Send + Sync>;
/// A native value predicate. Public because a combinator that interns its
/// predicate hands the built object to [`crate::ops::PatOps::filter_values_with`].
pub type ValuePredicate = Arc<dyn Fn(&Value) -> bool + Send + Sync>;
/// A native hap predicate - `filterHaps`, which sees whole/part, not just the
/// value. `discreteOnly`/`onsetsOnly` are the two uses the joins need.
type HapPredicate = Arc<dyn Fn(&Hap) -> bool + Send + Sync>;
type HapMapFn = Arc<dyn Fn(&Hap) -> Option<Hap> + Send + Sync>;
/// A native hap rewrite that owns the hap. Public because the REPL's lane
/// tag interns its map and hands the built object to
/// [`crate::Pattern::fmap_keeping_live_with`].
pub type OwnedHapMapFn = Arc<dyn Fn(Hap) -> Option<Hap> + Send + Sync>;
type StateHapMapFn = Arc<dyn Fn(&State, &Hap) -> Option<Hap> + Send + Sync>;
type HapExpandFn = Arc<dyn Fn(&Hap, &mut Vec<Hap>) + Send + Sync>;
/// A native continuous signal sampled once per query.
type SignalFn = Arc<dyn Fn(&State) -> Value + Send + Sync>;
/// A time transform applied to query or hap spans.
type TimeFn = Arc<dyn Fn(Fraction) -> Fraction + Send + Sync>;
/// A whole-span transform - `withQuerySpan` / `withHapSpan`. Distinct from
/// `TimeFn` because `zoom` and `revv` map begin and end **jointly**, and
/// `withCycle` needs the span's own `sam()`.
type SpanFn = Arc<dyn Fn(&TimeSpan) -> TimeSpan + Send + Sync>;

/// The most closures one intern pool keeps.
const MAX_INTERNED_CLOSURES: usize = 4096;

/// One closure object per key, for the whole process.
///
/// A node holds a transform as an opaque closure object, and two builds of
/// one construction agree on it only while it is the same object. A closure
/// whose behaviour is a pure function of a small deterministic key - a
/// capture-free body, or plain data it captured - is interned here: the
/// same key built again hands out the same object, so the unchanged
/// subtree carries its answer tables across evaluations and edits instead
/// of rebuilding them.
///
/// Past [`MAX_INTERNED_CLOSURES`] keys a pool stops taking entries: a
/// score that mints one key a call must not grow it without end, and a
/// fresh closure only loses the carry.
pub fn interned_closure<K, F: ?Sized>(
    pool: &std::sync::Mutex<std::collections::HashMap<K, Arc<F>>>,
    key: K,
    make: impl FnOnce() -> Arc<F>,
) -> Arc<F>
where
    K: std::hash::Hash + PartialEq + Eq,
{
    if let Ok(entries) = pool.lock()
        && let Some(found) = entries.get(&key)
    {
        return Arc::clone(found);
    }
    let made = make();
    if let Ok(mut entries) = pool.lock()
        && entries.len() < MAX_INTERNED_CLOSURES
    {
        entries.insert(key, Arc::clone(&made));
    }
    made
}

/// Live `Node` allocations. This cheap always-on counter observes Rust graph
/// lifetime independently of the JavaScript heap; aggregate RSS cannot make
/// that distinction.
static LIVE_NODES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The predicate of each [`Pattern::remove_undefineds`]: the strip a
/// `keepif` makes is the same payload wherever it is built again, so that
/// subtree carries its answers. See [`interned_closure`].
static KEEP_DEFINED: std::sync::OnceLock<ValuePredicate> = std::sync::OnceLock::new();

/// Number of `Node`s currently allocated across all patterns.
pub fn live_node_count() -> u64 {
    LIVE_NODES.load(std::sync::atomic::Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Query errors - the `queryArc` try/catch.
//
// Several operations **throw** mid-query: `parseNumeral` on a value that is
// neither numeric nor a note name, `TimeSpan.intersection_e` on disjoint
// spans. A stack contains the failure to one child; otherwise it reaches
// `queryArc`, which logs it and returns no haps.
//
// Rust has no exception to unwind, and making `query` return a `Result` would
// put a branch on the hottest path in the scheduler for a case only user
// error reaches. A thread-local flag carries the error to the nearest stack
// child boundary or the outer query boundary, which discards its result.
//
// writer                     slot                       reader
// signal_query_error      -> QUERY_ERROR             -> stack child boundary
//                                                       or queryArc boundary
// stack child boundary    -> CALLBACK_FAILURE,       -> queryArc boundary
//                            CONTAINED_THROW
// signal_callback_failure -> CALLBACK_FAILURE        -> queryArc boundary
// queryArc boundary       -> QUERY_CALLBACK_FAILURE, -> top-level caller
// (no query error)           QUERY_CONTAINED_THROW
// ---------------------------------------------------------------------------

/// The error slots of the table above, in one thread-local: a boundary
/// reads and writes several of them at its entry and at its end, and
/// reaches the thread one time for each.
struct QueryErrors {
    /// `QUERY_ERROR`: the pending error of the query.
    error: std::cell::RefCell<Option<String>>,
    /// `CALLBACK_FAILURE`: a failure the query contained rather than
    /// aborted on. The failure is reported once by the query's caller;
    /// first one wins per boundary.
    callback_failure: std::cell::RefCell<Option<String>>,
    /// `QUERY_CALLBACK_FAILURE`: the contained callback failure published
    /// by the last finished `queryArc` boundary, for the top-level caller
    /// to take and report. Cleared at top-level query entry so a stale
    /// failure cannot be attributed to the next independent query.
    published_callback_failure: std::cell::RefCell<Option<String>>,
    /// `CONTAINED_THROW`: a query error a stack contained to one child.
    /// The siblings keep playing, but the caller still reports a throw;
    /// first one wins per boundary.
    contained_throw: std::cell::RefCell<Option<String>>,
    /// `QUERY_CONTAINED_THROW`: the contained throw published by the last
    /// finished `queryArc` boundary, cleared with the published callback
    /// failure.
    published_contained_throw: std::cell::RefCell<Option<String>>,
    /// How many `queryArc`-style boundaries are live on this thread. See
    /// [`query_in_progress`].
    boundaries: std::cell::Cell<u32>,
}

thread_local! {
    static QUERY_ERRORS: QueryErrors = const {
        QueryErrors {
            error: std::cell::RefCell::new(None),
            callback_failure: std::cell::RefCell::new(None),
            published_callback_failure: std::cell::RefCell::new(None),
            contained_throw: std::cell::RefCell::new(None),
            published_contained_throw: std::cell::RefCell::new(None),
            boundaries: std::cell::Cell::new(0),
        }
    };
}

/// Whether a query is in progress on this thread: a `Pattern::query` frame,
/// or a `queryArc`-style boundary around one - including the cycle-0 probe
/// `stepJoin`/`stepBind` run while their pattern is still being constructed,
/// which resolves a pattern-of-patterns outside any query frame.
///
/// A host uses this to tell a callback the score's own method call invoked
/// synchronously, outside every query, from one a query reached. Only the
/// former throws straight back into the score; the latter belongs to the
/// query's boundary, which answers silence, as `queryArc` does.
pub fn query_in_progress() -> bool {
    QUERY_ERRORS.with(|errors| errors.boundaries.get() > 0)
        || QUERY_DEPTH.with(|depth| depth.get() > 0)
}

/// Record a mid-query failure. The first one wins, as a thrown exception
/// would ensure.
pub fn signal_query_error(message: impl FnOnce() -> String) {
    QUERY_ERRORS.with(|errors| {
        let mut slot = errors.error.borrow_mut();
        if slot.is_none() {
            *slot = Some(message());
            set_stop_bit(STOP_ERROR_PENDING, true);
        }
    });
}

/// Take and clear the pending query error, if any.
pub fn take_query_error() -> Option<String> {
    set_stop_bit(STOP_ERROR_PENDING, false);
    QUERY_ERRORS.with(|errors| errors.error.borrow_mut().take())
}

/// Record a failure the query contains, reported once by its caller instead
/// of emptying the whole arc. Filters keep the affected hap; stacks discard
/// the failed child's haps and keep the other children playing.
///
/// The flag also prevents result caches from retaining an incomplete answer.
pub fn signal_callback_failure(message: impl FnOnce() -> String) {
    QUERY_ERRORS.with(|errors| {
        let mut slot = errors.callback_failure.borrow_mut();
        if slot.is_none() {
            *slot = Some(message());
        }
    });
}

/// Take the contained callback failure published by the last completed
/// `queryArc` boundary, if any.
pub fn take_query_callback_failure() -> Option<String> {
    QUERY_ERRORS.with(|errors| errors.published_callback_failure.borrow_mut().take())
}

/// Take the query error a stack contained, published by the last completed
/// `queryArc` boundary, if any. The same failure is also published as a
/// contained callback failure; this channel says that a stack child threw.
pub fn take_query_contained_throw() -> Option<String> {
    QUERY_ERRORS.with(|errors| errors.published_contained_throw.borrow_mut().take())
}

fn signal_contained_throw(message: String) {
    QUERY_ERRORS.with(|errors| {
        errors.contained_throw.borrow_mut().get_or_insert(message);
    });
}

/// Whether the query in progress can no longer be trusted to have answered
/// completely: a mid-query error, a contained callback failure, a resource
/// refusal, an expired deadline, or a cancellation request.
///
/// A result cache asks this before keeping what a child answered. What an
/// interrupted query produced is what the `queryArc` boundary is about to
/// discard, and keeping it would serve the discarded answer to the next,
/// healthy query. The flags are sticky for the top-level query, so an
/// interruption anywhere earlier in it also refuses the cache - the
/// conservative side.
pub fn query_interrupted() -> bool {
    query_error_pending()
        || QUERY_ERRORS.with(|errors| errors.callback_failure.borrow().is_some())
        || budget_exhausted()
        || cancellation_observed()
        || cancellation_requested()
}

thread_local! {
    /// How many times a query on this thread has reached something no
    /// result cache may keep: a volatile pattern, a handle bound to a
    /// runtime of its own, or an incomplete raw stack query. Monotonic, so a
    /// cache reads it before and after the evaluation it is about to keep.
    static UNCACHEABLE_TOUCHES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn note_uncacheable() {
    UNCACHEABLE_TOUCHES.with(|touches| touches.set(touches.get().wrapping_add(1)));
}

/// How many times a query on this thread has so far reached something a
/// result cache must not keep - see [`purity::Purity::volatile`].
///
/// The static classification is what a node reads when it is built; this is
/// what it reads around the evaluation itself, because a pattern-of-patterns
/// materialises its inner graphs only at query time, and one of those may be
/// volatile although the carrier is not. A cache that sees this move during
/// an evaluation keeps nothing from it.
pub fn uncacheable_touches() -> u64 {
    UNCACHEABLE_TOUCHES.with(|touches| touches.get())
}

impl QueryErrors {
    /// A fresh top-level query starts with no contained failure to report.
    fn clear_published(&self) {
        *self.published_callback_failure.borrow_mut() = None;
        *self.published_contained_throw.borrow_mut() = None;
    }

    /// What the boundary that ends now holds, taken: its error, the
    /// failure it contained and the throw a stack contained in it.
    fn take_inner(&self) -> [Option<String>; 3] {
        let taken = [
            self.error.borrow_mut().take(),
            self.callback_failure.borrow_mut().take(),
            self.contained_throw.borrow_mut().take(),
        ];
        let _ = QUERY_THREAD.try_with(|thread| thread.set_stop_bit(STOP_ERROR_PENDING, false));
        taken
    }
}

/// A fresh top-level query starts with no contained failure to report.
fn clear_query_callback_failure() {
    QUERY_ERRORS.with(QueryErrors::clear_published);
}

/// One `queryArc`-style error boundary.
///
/// The pending outer exception must survive nested queries, and any exception
/// raised inside this boundary must be discarded on unwind just as JavaScript
/// stack unwinding would discard it. Keeping that restoration in `Drop` avoids
/// poisoning the next query when a native checked-arithmetic panic is caught by
/// an embedding host.
///
/// The contained-callback-failure channel gets the same treatment: stashed on
/// entry, restored on exit, and whatever the inner query contained is
/// published for the outermost caller to take.
struct QueryErrorBoundary {
    outer: Option<String>,
    outer_callback_failure: Option<String>,
    outer_contained_throw: Option<String>,
    restored: bool,
}

impl QueryErrorBoundary {
    fn enter() -> Self {
        QUERY_ERRORS.with(|errors| {
            errors
                .boundaries
                .set(errors.boundaries.get().saturating_add(1));
            let [outer, outer_callback_failure, outer_contained_throw] = errors.take_inner();
            Self {
                outer,
                outer_callback_failure,
                outer_contained_throw,
                restored: false,
            }
        })
    }

    fn finish(mut self) -> Option<String> {
        QUERY_ERRORS.with(|errors| {
            let [inner, inner_callback_failure, inner_contained_throw] = errors.take_inner();
            self.restore_in(errors);
            if inner.is_some() {
                // The queryArc aborted. Discard contained failures with
                // the partial haps, including one that a nested boundary
                // already published. Report only the throw; a "kept
                // playing" warning would be false for a silent window.
                errors.clear_published();
            } else {
                if let Some(failure) = inner_callback_failure {
                    *errors.published_callback_failure.borrow_mut() = Some(failure);
                }
                if let Some(thrown) = inner_contained_throw {
                    *errors.published_contained_throw.borrow_mut() = Some(thrown);
                }
            }
            inner
        })
    }

    /// Put back what the query around this boundary held at its entry.
    fn restore_in(&mut self, errors: &QueryErrors) {
        if self.restored {
            return;
        }
        let outer = self.outer.take();
        let has_error = outer.is_some();
        *errors.error.borrow_mut() = outer;
        *errors.callback_failure.borrow_mut() = self.outer_callback_failure.take();
        *errors.contained_throw.borrow_mut() = self.outer_contained_throw.take();
        let _ = QUERY_THREAD.try_with(|thread| thread.set_stop_bit(STOP_ERROR_PENDING, has_error));
        self.restored = true;
    }
}

impl Drop for QueryErrorBoundary {
    fn drop(&mut self) {
        // The slots are gone when the thread ends.
        let _ = QUERY_ERRORS.try_with(|errors| {
            self.restore_in(errors);
            errors
                .boundaries
                .set(errors.boundaries.get().saturating_sub(1));
        });
    }
}

/// Whether a query exception has already been raised.
///
/// Callback-bearing loops use this to stop at the first throw. Continuing to
/// invoke user code would expose side effects an unwinding exception would
/// have prevented, even though `queryArc` discards all partial haps anyway.
pub(crate) fn query_error_pending() -> bool {
    QUERY_ERRORS.with(|errors| errors.error.borrow().is_some())
}

/// Contain ordinary child errors at a stack boundary, for both event queries
/// and pattern-of-pattern resolution. Structural refusals remain query-wide.
fn query_stack_child<T>(query: impl FnOnce() -> Vec<T>) -> Vec<T> {
    if query_error_pending() {
        return Vec::new();
    }
    let haps = query();
    match stack_child_failed() {
        true => Vec::new(),
        false => haps,
    }
}

/// The haps of the children `pats` of a stack, after those of `out`. Each
/// child writes after the one before it. A child that fails writes
/// nothing, and its siblings stay. `before` runs before each child is
/// asked.
fn query_stack_onto(
    thread: &QueryThread,
    pats: &[Pattern],
    state: &State,
    out: &mut Vec<Hap>,
    before: impl Fn(),
) {
    let from = out.len();
    for pat in pats.iter().filter(|pat| !pat.is_rest()) {
        before();
        stack_child_onto(thread, out, |out| {
            pat.answer_onto(thread, state, table::Order::Graph, out);
        });
        let held = out.len() - from;
        if !thread.charge(held) {
            out.truncate(from);
            return;
        }
        thread.note_accumulated(held);
    }
}

fn query_stack_pure_onto(
    thread: &QueryThread,
    cells: &[Arc<NodeCell>],
    state: &State,
    out: &mut Vec<Hap>,
) {
    let from = out.len();
    for cell in cells
        .iter()
        .filter(|cell| !matches!(cell.kind, Node::Silence))
    {
        stack_child_onto(thread, out, |out| {
            plain_pattern(cell, 0).answer_onto(thread, state, table::Order::Graph, out);
        });
        let held = out.len() - from;
        if !thread.charge(held) {
            out.truncate(from);
            return;
        }
        thread.note_accumulated(held);
    }
}

/// [`query_stack_child`] for a child that writes its haps after those of
/// `out`: the haps of a child that failed go, and those before them stay.
fn stack_child_onto(thread: &QueryThread, out: &mut Vec<Hap>, query: impl FnOnce(&mut Vec<Hap>)) {
    if thread.error_pending() {
        return;
    }
    let from = out.len();
    query(out);
    if stack_child_failed_on(thread) {
        out.truncate(from);
    }
}

/// Whether the child of a stack that a query just asked gave no answer to
/// keep: the query stops, or the child threw. The stack contains the throw
/// and reports it.
fn stack_child_failed() -> bool {
    QUERY_THREAD.with(stack_child_failed_on)
}

fn stack_child_failed_on(thread: &QueryThread) -> bool {
    if thread.stop_requested() || thread.cancel_observed.get() {
        return true;
    }
    if !thread.error_pending() {
        return false;
    }
    let Some(message) = take_query_error() else {
        return false;
    };
    CURRENT_HOST.with(|host| {
        if let Some(pointer) = host.get() {
            // SAFETY: installed only by `with_callback_host`, which
            // outlives this call, including nested queries.
            unsafe { &*pointer }.log_query_error(&message);
        }
    });
    if QUERY_ERRORS.with(|errors| errors.boundaries.get() > 0) {
        signal_callback_failure(|| message.clone());
        signal_contained_throw(message);
    } else {
        // Raw queries have no boundary to clear a contained failure.
        // Invalidate enclosing caches without poisoning later queries.
        note_uncacheable();
    }
    true
}

// Materialise only what holds a JavaScript-owned identity. On a value that
// holds none, the walk rebuilds every container to hand back the same data,
// so the hap keeps its value as it is. The outer query boundary applies the
// same rule to the haps it returns.
fn materialize_if_js(value: &Value) -> Value {
    if value.contains_js_value() {
        materialize_js_value(value)
    } else {
        value.clone()
    }
}

// Keep callback temporaries out of the recursive query dispatcher. The child
// query finishes before entering this helper, so its frame is not multiplied
// by every node in a pattern graph.
fn map_js_haps(
    haps: Vec<Hap>,
    id: CallbackId,
    preserve_controls: bool,
    predicate: Option<fn(&Value) -> bool>,
) -> Vec<Hap> {
    let mut out = Vec::with_capacity(haps.len());
    for hap in haps {
        // Every iteration can enter QuickJS, and a wide query holds tens of
        // thousands of haps. Check inside the loop so callback and GC work
        // cannot keep the producer occupied after cancellation or a deadline.
        // A pending query error is deliberately NOT a stop: the turn
        // boundary turns a throwing callback into silence after every hap
        // had its turn, which the `and`/`or` lazy-truthy contracts pin.
        if query_stop_requested() {
            return Vec::new();
        }
        let mapped = hap.with_value(|v| {
            if let Some(predicate) = predicate {
                // Final event inspection consumes JS containers once, after
                // all user callbacks have run.
                let value = materialize_if_js(v);
                return if predicate(&value) {
                    materialize_if_js(&host_call_value(id, &value))
                } else {
                    value
                };
            }
            let value = host_call_value(id, v);
            if preserve_controls {
                // Trusted control rewrites consume their result here, so
                // compare native values before retaining bindings. User fmap
                // keeps opaque JS identity.
                materialize_if_js(&value)
            } else {
                value
            }
        });
        out.push(if preserve_controls {
            mapped.with_unchanged_live_controls(&hap)
        } else {
            mapped
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Hap budget.
//
// `MAX_QUERY_SPAN_CYCLES` bounds how wide one query may be. It does not bound
// how dense it is: `s("bd").ply(2000000)` covers a single cycle and still
// asks for two million haps. Nesting multiplies: `ply(1000).ply(1000)` asks
// for a million. The span guard cannot see this, because the expansion
// happens inside one cycle.
//
// The budget is charged as haps are produced, not after. A check on a finished
// `Vec<Hap>`, in the scheduler's queue or at the caller, is too late: the
// allocation it is meant to prevent has already happened.
//
// Exhaustion does not go through `signal_query_error`. That channel means
// "the query threw here", and `queryArc` turns it into an empty result. That
// is correct for a user's throwing callback and wrong for resource
// exhaustion, where an empty result looks like a pattern that produced
// nothing. Exhaustion has its own flag, which the query error boundary does
// not clear.
// ---------------------------------------------------------------------------

/// A query refused for RESOURCE reasons rather than pattern semantics.
///
/// Deliberately not a `String` and deliberately not the query-error channel:
/// this has to be impossible to mistake for "the pattern produced nothing".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryLimit {
    /// More haps than [`DEFAULT_HAP_BUDGET`] were produced.
    HapBudget { budget: u64 },
    /// The query span was wider than [`MAX_QUERY_SPAN_CYCLES`].
    ///
    /// This is a resource refusal and travels on the typed channel. On the
    /// query-error channel, the `queryArc` boundary would turn it into an
    /// empty result, which looks like a pattern that produces nothing.
    QuerySpan { cycles: i128 },
    /// The JavaScript host ran out of heap.
    ///
    /// Distinct from a user throw, which `queryArc` turns into silence. An
    /// exhausted heap is a resource refusal: reporting it as an empty query
    /// would say "this pattern produces nothing" about a pattern that was
    /// never evaluated.
    HostMemory,
    /// One setup turn queued more runnable JavaScript jobs than the host will
    /// execute. This is independent of the CPU deadline: an unbounded chain of
    /// individually cheap microtasks can stay below every per-job timing
    /// sample while still monopolising the session.
    JsJobBudget { budget: usize },
    /// A bounded synchronous score or query turn found runnable JavaScript
    /// jobs.
    ///
    /// Setup evaluation has an explicit async microtask contract; score
    /// construction and query callbacks do not. Treating this host-policy
    /// refusal as an ordinary JavaScript message would let Session's Mini
    /// compatibility fallback install a different score, or let `queryArc`
    /// turn an incomplete query into plausible silence.
    JsPendingJobs,
    /// A bounded JavaScript evaluation occupied the runtime past its CPU
    /// deadline. This is a resource refusal, not a syntax/user exception.
    JsCpuDeadline { millis: u64 },
    /// A query ran past the wall-clock deadline installed around it - the
    /// live probe's ceiling, a scheduler tick's slice - in native work, with
    /// no JavaScript involved: `fill` under `struct` under `fill` spends its
    /// time in pattern recursion. `millis` is how long the deadline allowed
    /// from the moment it was installed.
    QueryDeadline { millis: u64 },
    /// A Euclidean rhythm would allocate an oversized intermediate mask.
    EuclidSteps { steps: u64, limit: u64 },
    /// A mini-notation range would materialise too many pattern nodes.
    MiniRange { elements: u64, limit: u64 },
    /// `echoWith` would allocate and transform too many delayed copies during
    /// pattern construction, before any query hap budget exists.
    EchoCopies { copies: u64, limit: u64 },
    /// `iter`/`chunk` would materialise too many rotated copies during pattern
    /// construction.
    ///
    /// Both build a `Vec` sized directly from the caller's part count, before
    /// any query hap budget exists. An unguarded `chunk(1e9)` would request a
    /// `Vec` of 10^9 patterns, and a failed allocation aborts the process.
    /// strudel.cc throws a catchable `RangeError` here and plays on, so a
    /// refusal is also the compatible behaviour.
    IterParts {
        operation: &'static str,
        parts: u64,
        limit: u64,
    },
    /// A stepwise operation would materialise too many retained or query-split
    /// entries.
    ///
    /// `minimum_entries` is exact for ordinary finite inputs and otherwise a
    /// conservative lower bound discovered by the bounded preflight. The
    /// operation is carried structurally so callers do not have to infer which
    /// constructor was refused from an error string.
    StepwiseExpansion {
        operation: &'static str,
        minimum_entries: u64,
        limit: u64,
    },
    /// A public numeric input or exact intermediate cannot be represented by
    /// the native checked `i128/i128` Fraction layer.
    NativeFraction { operation: &'static str },
    /// A pattern graph nested deeper than the query traversal can safely
    /// recurse through.
    ///
    /// `query_node` descends one native frame per wrapper, so a source that
    /// chains enough combinators (`p = p.fast(2).slow(2)` in a loop) would
    /// overflow the stack. A Rust stack overflow aborts the process: it is
    /// not a panic, `catch_unwind` never sees it, and the session cannot
    /// recover. `Pattern::query` refuses a depth above [`MAX_PATTERN_DEPTH`]
    /// instead.
    GraphDepth { depth: u32, limit: u32 },
    /// The caller asked for the query to stop.
    ///
    /// `Pattern::query` reads the cancellation flag as it recurses, so a
    /// dense query stops between nodes instead of running to completion.
    Cancelled,
}

impl std::fmt::Display for QueryLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GraphDepth { depth, limit } => write!(
                f,
                "pattern graph nests {depth} levels deep, past the {limit} the \
                 query can traverse without overflowing the stack"
            ),
            Self::Cancelled => write!(f, "the query was cancelled"),
            Self::HostMemory => write!(
                f,
                "the JavaScript heap was exhausted and the query was refused. \
                 This is a resource limit, not an empty pattern."
            ),
            Self::JsJobBudget { budget } => write!(
                f,
                "the JavaScript setup queued more than {budget} runnable jobs and was refused"
            ),
            Self::JsPendingJobs => write!(
                f,
                "bounded synchronous JavaScript execution does not support runnable JavaScript jobs"
            ),
            Self::JsCpuDeadline { millis } => write!(
                f,
                "JavaScript evaluation exceeded its {millis} ms CPU deadline and was refused"
            ),
            Self::QueryDeadline { millis } => write!(
                f,
                "the query exceeded its {millis} ms deadline and was refused"
            ),
            Self::EuclidSteps { steps, limit } => write!(
                f,
                "the Euclidean rhythm requested {steps} steps, above the native limit of {limit}, and was refused. This is a resource limit, not an empty pattern."
            ),
            Self::MiniRange { elements, limit } => write!(
                f,
                "the mini range requested {elements} elements, above the native limit of {limit}, and was refused. This is a resource limit, not an empty pattern."
            ),
            Self::EchoCopies { copies, limit } => write!(
                f,
                "echoWith requested {copies} copies, above the native limit of {limit}, and was refused. This is a resource limit, not an empty pattern."
            ),
            Self::IterParts {
                operation,
                parts,
                limit,
            } => write!(
                f,
                "{operation} requested {parts} parts, above the native limit of {limit}, and was refused. This is a resource limit, not an empty pattern."
            ),
            Self::StepwiseExpansion {
                operation,
                minimum_entries,
                limit,
            } => write!(
                f,
                "{operation} would require at least {minimum_entries} stepwise entries, above the native limit of {limit}, and was refused. This is a resource limit, not an empty pattern."
            ),
            Self::NativeFraction { operation } => write!(
                f,
                "{operation} requires fraction arithmetic outside the native checked i128 range and was refused. This is a native representation limit, not an empty pattern."
            ),
            Self::QuerySpan { cycles } => write!(
                f,
                "the query span covers more than {cycles} cycles and was \
                 refused. This is a resource limit, not an empty pattern."
            ),
            Self::HapBudget { budget } => write!(
                f,
                "the query produced more than {budget} haps and was refused. \
                 This is a resource limit, not an empty pattern: the result \
                 would have been truncated, so nothing is returned instead."
            ),
        }
    }
}

impl std::error::Error for QueryLimit {}

/// Outcome of one outer `queryArc` boundary, distinguishing genuine silence
/// from a mid-query throw. The boundary turns a throw into no haps; live
/// reload must not treat that as a successful replacement of a sounding score.
#[derive(Debug, Clone)]
pub enum QueryArcOutcome {
    Haps(Vec<Hap>),
    Thrown(String),
}

/// Haps one top-level query may produce before it is refused.
///
/// Five million is far past any musical query - a 32-per-cycle pattern would
/// need 150,000 cycles to reach it - while bounding one query's haps to a few
/// hundred megabytes rather than all of memory.
pub const DEFAULT_HAP_BUDGET: u64 = 5_000_000;

/// Logical source/expansion high-water one stepwise scalar invocation or
/// top-level query may plan.
///
/// This counts the largest charged construction stage, not the sum of every
/// simultaneously live implementation buffer or its bytes. JavaScript rest
/// arguments, normalized sources, sidecars, and reserved output vectors can
/// overlap while remaining individually bounded by their checked plans.
pub const MAX_STEPWISE_ENTRIES: u64 = 16_384;

/// The counters a query reads on every node and every hap, in one
/// thread-local.
///
/// Where reaching a thread-local is a function call, as on macOS, every
/// separate slot is one more call per node. Repeated reads of one slot in a
/// function share one call. Each slot below is a view of one field and
/// keeps the `with` of the thread-local it replaced.
struct QueryThread {
    hap_budget: std::cell::Cell<Option<u64>>,
    depth: std::cell::Cell<u32>,
    stop: std::cell::Cell<u8>,
    peak: std::cell::Cell<u64>,
    /// Wall-clock deadline for native query work, mirroring the JS CPU
    /// deadline. QuickJS's interrupt handler only fires while JavaScript
    /// runs; a pattern like `chop(32).slow(0.001)` spends its time in
    /// native query recursion (millions of cheap spans, each below every
    /// hap budget). Without this deadline a single save could occupy the
    /// producer for minutes: the live set goes silent while the process
    /// stays alive.
    deadline: std::cell::Cell<Option<std::time::Instant>>,
    /// The time a query is allowed, when its deadline is not made yet:
    /// see [`with_query_budget`].
    budget: std::cell::Cell<Option<std::time::Duration>>,
    /// How long the installed deadline allowed, in milliseconds from its
    /// installation, so a refusal can say what it was measured against
    /// rather than reporting a zero it never had.
    deadline_millis: std::cell::Cell<u64>,
    /// Polling counter: `Instant::now()` is far too expensive per node,
    /// but the sample has to be frequent enough that a deep recursion
    /// notices the deadline in tens of milliseconds, not seconds.
    deadline_tick: std::cell::Cell<u32>,
    /// Sticky for the lifetime of the installed deadline.
    ///
    /// If the first expiry cleared the deadline, the rest of the query
    /// would have no bound: every later `Pattern::query` would read an
    /// empty slot and run at full speed. The refusal flag is the only
    /// other check, and `with_hap_budget` swaps it out at every non-nested
    /// boundary. A JS `queryArc` callback re-entry is such a boundary.
    /// Keeping expiry sticky ensures nested queries cannot escape the
    /// deadline.
    deadline_expired: std::cell::Cell<bool>,
    part_asked: std::cell::Cell<PartAsked>,
    /// Set while a cancellable query runs. Erased to a pointer because
    /// the flag is owned by the caller and shared with whatever thread
    /// will set it.
    cancel: std::cell::Cell<Option<*const std::sync::atomic::AtomicBool>>,
    cancel_observed: std::cell::Cell<bool>,
    /// Haps materialised at leaves on this thread since the last reset.
    materialised: std::cell::Cell<u64>,
    /// Pointer identity of the top `RuntimeSettings` bound on this thread, or 0.
    current_runtime: std::cell::Cell<usize>,
    /// Remaining stepwise construction entries in the current top-level query.
    stepwise_remaining: std::cell::Cell<Option<u64>>,
}

impl QueryThread {
    /// Mirror one stop signal into the stop word.
    fn set_stop_bit(&self, bit: u8, on: bool) {
        let word = self.stop.get();
        self.stop.set(if on { word | bit } else { word & !bit });
    }

    /// Whether a query error is currently pending on this thread (mirrored
    /// from `QUERY_ERRORS` so hot loops avoid a second TLS lookup).
    #[inline]
    fn error_pending(&self) -> bool {
        self.stop.get() & STOP_ERROR_PENDING != 0
    }

    /// See [`query_stop_requested`].
    fn stop_requested(&self) -> bool {
        let word = self.stop.get();
        if word & (STOP_REFUSED | STOP_DEADLINE_PASSED) != 0 {
            return true;
        }
        if word & STOP_CANCEL_ARMED != 0 && self.cancellation_requested() {
            return true;
        }
        word & STOP_DEADLINE_ARMED != 0 && self.deadline_after(1)
    }

    /// See [`cancellation_requested`].
    fn cancellation_requested(&self) -> bool {
        // The word answers the common case: no flag installed means no other
        // thread can be asking this one to stop.
        if self.stop.get() & STOP_CANCEL_ARMED == 0 {
            return false;
        }
        let requested = match self.cancel.get() {
            None => false,
            // SAFETY: set only by `with_cancellation`, which outlives this read.
            Some(flag) => unsafe { &*flag }.load(std::sync::atomic::Ordering::Relaxed),
        };
        if requested {
            self.cancel_observed.set(true);
        }
        requested
    }

    /// See [`query_deadline_after`].
    fn deadline_after(&self, checks: u32) -> bool {
        // Once expired, STAY expired: every remaining query in this scope returns
        // empty immediately, so a deep recursion unwinds in microseconds instead
        // of continuing to build haps nobody will hear. Re-refusing is harmless
        // (the first limit wins) and is what re-arms the refusal after an inner
        // `with_hap_budget` boundary has swapped it out.
        if self.deadline_expired.get() {
            refuse(QueryLimit::QueryDeadline {
                millis: self.deadline_millis.get(),
            });
            return true;
        }
        let (deadline, budget) = (self.deadline.get(), self.budget.get());
        if deadline.is_none() && budget.is_none() {
            return false;
        }
        // The clock is read when the count passes a multiple of 64.
        let before = self.deadline_tick.get();
        let after = before.wrapping_add(checks);
        self.deadline_tick.set(after);
        if after / 64 == before / 64 {
            return false;
        }
        let now = std::time::Instant::now();
        let Some(deadline) = deadline else {
            // The first read of the clock in a budget: the deadline is
            // that long from here.
            self.deadline.set(budget.map(|budget| now + budget));
            return false;
        };
        if now < deadline {
            return false;
        }
        self.deadline_expired.set(true);
        self.set_stop_bit(STOP_DEADLINE_PASSED, true);
        refuse(QueryLimit::QueryDeadline {
            millis: self.deadline_millis.get(),
        });
        true
    }

    /// See [`charge_haps`].
    fn charge(&self, n: usize) -> bool {
        match self.hap_budget.get() {
            // No budget installed: not inside a top-level query (a unit test
            // calling `query` directly, say). Unbudgeted rather than zero-budget,
            // so nothing silently starts failing outside the boundary.
            None => true,
            Some(limit) if n as u64 > limit => {
                refuse(QueryLimit::HapBudget { budget: limit });
                false
            }
            Some(_) => true,
        }
    }

    /// Record a vector that has just grown to `len`.
    #[inline]
    fn note_accumulated(&self, len: usize) {
        self.peak.set(self.peak.get().max(len as u64));
    }

    /// Record `n` haps materialised at a leaf without an extra TLS lookup.
    #[inline]
    pub(crate) fn note_materialised(&self, n: usize) {
        self.materialised
            .set(self.materialised.get().saturating_add(n as u64));
    }

    /// [`push_budgeted_from`] on the counters of this thread: a walk that
    /// makes many haps reaches the thread one time.
    fn push_budgeted(&self, out: &mut Vec<Hap>, from: usize, hap: Hap) -> bool {
        if !self.charge(out.len() - from + 1) {
            return false;
        }
        out.push(hap);
        self.note_accumulated(out.len() - from);
        true
    }
}

pub(crate) fn set_current_runtime_ptr(ptr: usize) {
    let _ = QUERY_THREAD.try_with(|thread| thread.current_runtime.set(ptr));
}

thread_local! {
    static QUERY_THREAD: QueryThread = const {
        QueryThread {
            hap_budget: std::cell::Cell::new(None),
            depth: std::cell::Cell::new(0),
            stop: std::cell::Cell::new(0),
            peak: std::cell::Cell::new(0),
            deadline: std::cell::Cell::new(None),
            budget: std::cell::Cell::new(None),
            deadline_millis: std::cell::Cell::new(0),
            deadline_tick: std::cell::Cell::new(0),
            deadline_expired: std::cell::Cell::new(false),
            part_asked: std::cell::Cell::new(PartAsked::No),
            cancel: std::cell::Cell::new(None),
            cancel_observed: std::cell::Cell::new(false),
            materialised: std::cell::Cell::new(0),
            current_runtime: std::cell::Cell::new(0),
            stepwise_remaining: std::cell::Cell::new(None),
        }
    };
}

/// A view of one field of this thread's [`QueryThread`].
struct QueryCell<T: 'static>(fn(&QueryThread) -> &T);

impl<T: 'static> QueryCell<T> {
    #[inline]
    fn with<R>(&'static self, f: impl FnOnce(&T) -> R) -> R {
        QUERY_THREAD.with(|thread| f((self.0)(thread)))
    }
}

/// Haps still allowed in the current top-level query. `None` outside one.
static HAP_BUDGET: QueryCell<std::cell::Cell<Option<u64>>> = QueryCell(|q| &q.hap_budget);
/// How many `Pattern::query` frames are live on this thread.
///
/// Counted at the boundary rather than derived from the graph because
/// `Pattern::of` cannot see a node's children without a per-variant match,
/// and a match that must be extended for every future combinator is a
/// limit that will eventually be forgotten. One counter on the one
/// recursive entry point cannot be.
static QUERY_DEPTH: QueryCell<std::cell::Cell<u32>> = QueryCell(|q| &q.depth);
/// The stop signals as one word — a refusal recorded, a deadline armed
/// or past, a cancellation flag installed — so the checks a query makes
/// per hap read one thread-local instead of walking each slot. The
/// separate slots stay the source of truth; the word only mirrors them.
static QUERY_STOP: QueryCell<std::cell::Cell<u8>> = QueryCell(|q| &q.stop);
static PEAK_CHARGE: QueryCell<std::cell::Cell<u64>> = QueryCell(|q| &q.peak);

/// Whether a query on this thread asks a graph over a part of the span of
/// a restart or a reset join, which cuts the answer to that part. See
/// [`asked_over_part`].
static PART_ASKED: QueryCell<std::cell::Cell<PartAsked>> = QueryCell(|q| &q.part_asked);

/// The state of a query over a part of a span.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PartAsked {
    /// No query asks over a part.
    No,
    /// A query asks, and each pattern that a join made in it so far answers
    /// by its haps.
    Asked,
    /// A join of the query made a pattern that refuses the part.
    Refused,
}

/// Run `ask`, a query of a graph over a part of a span, for a join that
/// cuts the answer to the part. `None` when a join of the graph made a
/// pattern that [refuses the part](refuses_part): the answer for the part
/// is then not known to be the answer for the span, cut.
fn asked_over_part<R>(ask: impl FnOnce() -> R) -> Option<R> {
    /// Puts back the state of the query around this one, also on unwind.
    struct Around(PartAsked);
    impl Drop for Around {
        fn drop(&mut self) {
            PART_ASKED.with(|asked| asked.set(self.0));
        }
    }
    let around = Around(PART_ASKED.with(|asked| asked.replace(PartAsked::Asked)));
    let answer = ask();
    let refused = PART_ASKED.with(std::cell::Cell::get) == PartAsked::Refused;
    let outer = around.0;
    drop(around);
    if !refused {
        return Some(answer);
    }
    // The query that follows asks over the span of this one. A query over
    // a part around this one made that span, so it is refused too.
    if outer != PartAsked::No {
        PART_ASKED.with(|asked| asked.set(PartAsked::Refused));
    }
    None
}

/// Whether a query over a part of a span is in progress on this thread.
fn part_is_asked() -> bool {
    PART_ASKED.with(std::cell::Cell::get) != PartAsked::No
}

/// Whether `made`, a pattern that a join is about to ask in a query over a
/// part, refuses that query: it reads the span, or it is volatile. The join
/// then asks it nothing, so a volatile graph is asked one time, by the
/// query over the span that follows.
fn refuses_part(made: &Pattern) -> bool {
    let refuses = made.span_read() == purity::SpanRead::Yes || made.is_volatile();
    if refuses {
        PART_ASKED.with(|asked| asked.set(PartAsked::Refused));
    }
    refuses
}

/// Alias kept for the shrink/grow boundary.
pub const MAX_STEPWISE_SEGMENTS: u64 = MAX_STEPWISE_ENTRIES;

static STEPWISE_ENTRIES_REMAINING: QueryCell<std::cell::Cell<Option<u64>>> =
    QueryCell(|q| &q.stepwise_remaining);

thread_local! {
    /// Set when a RESOURCE limit refuses the query. Separate from
    /// `QUERY_ERROR` so the `queryArc` boundary cannot turn a refusal into a
    /// plausible silence, and typed so the caller learns which limit fired.
    static REFUSAL: std::cell::RefCell<Option<QueryLimit>> =
        const { std::cell::RefCell::new(None) };
    /// Entries materialised after preflight. Thread-local so parallel tests do
    /// not race.
    static STEPWISE_ENTRIES_MATERIALISED: std::cell::Cell<u64> =
        const { std::cell::Cell::new(0) };
}

/// [`QUERY_STOP`] bits: the refusal slot holds a limit, a deadline is
/// installed, that deadline has passed, a cancellation flag is installed.
const STOP_REFUSED: u8 = 1;
const STOP_DEADLINE_ARMED: u8 = 2;
const STOP_DEADLINE_PASSED: u8 = 4;
const STOP_CANCEL_ARMED: u8 = 8;
const STOP_ERROR_PENDING: u8 = 16;

/// Mirror one stop signal into [`QUERY_STOP`].
fn set_stop_bit(bit: u8, on: bool) {
    QUERY_THREAD.with(|thread| thread.set_stop_bit(bit, on));
}

/// Charge a complete planned stepwise expansion before any Pattern nodes are
/// constructed or raw inputs are reified. A top-level query is cumulative; a
/// scalar body outside one is bounded independently and carries its typed
/// refusal in the returned graph.
pub fn charge_stepwise_entries(operation: &'static str, entries: u64) -> Result<(), QueryLimit> {
    STEPWISE_ENTRIES_REMAINING.with(|remaining| match remaining.get() {
        None if entries <= MAX_STEPWISE_ENTRIES => Ok(()),
        None => Err(QueryLimit::StepwiseExpansion {
            operation,
            minimum_entries: entries,
            limit: MAX_STEPWISE_ENTRIES,
        }),
        Some(left) if entries <= left => {
            remaining.set(Some(left - entries));
            Ok(())
        }
        Some(left) => {
            let used = MAX_STEPWISE_ENTRIES - left;
            let limit = QueryLimit::StepwiseExpansion {
                operation,
                minimum_entries: used.saturating_add(entries),
                limit: MAX_STEPWISE_ENTRIES,
            };
            refuse(limit.clone());
            Err(limit)
        }
    })
}

/// Progressive-segment spelling retained for the core combinators.
pub(crate) fn charge_stepwise_segments(segments: u64) -> Result<(), QueryLimit> {
    charge_stepwise_entries("shrink/grow", segments)
}

/// Publish a non-expansion stepwise refusal immediately when a patterned body
/// is running inside a query, while leaving scalar construction to carry the
/// limit in its returned graph.
pub fn mark_stepwise_refusal(limit: QueryLimit) -> QueryLimit {
    STEPWISE_ENTRIES_REMAINING.with(|remaining| {
        if remaining.get().is_some() {
            refuse(limit.clone());
        }
    });
    limit
}

/// Run `f` apart from the query around it, for work whose result outlives that
/// query. A query error or a resource refusal raised inside makes the result
/// `None`, and the enclosing query sees neither.
fn without_query_signals<T>(f: impl FnOnce() -> Option<T>) -> Option<T> {
    /// Holds the enclosing query's refusal and puts it back, also on unwind.
    struct RefusalScope {
        outer: Option<QueryLimit>,
        finished: bool,
    }
    impl RefusalScope {
        fn enter() -> Self {
            let outer = REFUSAL.with(|slot| slot.borrow_mut().take());
            set_stop_bit(STOP_REFUSED, outer.is_some());
            Self {
                outer,
                finished: false,
            }
        }
        /// Whether a refusal was raised inside.
        fn finish(mut self) -> bool {
            self.finished = true;
            let outer = self.outer.take();
            let outer_refused = outer.is_some();
            let inner = REFUSAL.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), outer));
            set_stop_bit(STOP_REFUSED, outer_refused);
            inner.is_some()
        }
    }
    impl Drop for RefusalScope {
        fn drop(&mut self) {
            if !self.finished {
                let outer = self.outer.take();
                set_stop_bit(STOP_REFUSED, outer.is_some());
                REFUSAL.with(|slot| *slot.borrow_mut() = outer);
            }
        }
    }
    let boundary = QueryErrorBoundary::enter();
    let refusal = RefusalScope::enter();
    let result = f();
    let refused = refusal.finish();
    let thrown = boundary.finish();
    if refused || thrown.is_some() {
        return None;
    }
    result
}

/// One time map of [`Node::Fast`] or [`Node::Shift`]. A result outside the
/// native fraction range marks the `NativeFraction` refusal for `operation`
/// and becomes time zero until the query boundary reports the refusal.
fn checked_time(time: Option<Fraction>, operation: &'static str) -> Fraction {
    time.unwrap_or_else(|| {
        mark_stepwise_refusal(QueryLimit::NativeFraction { operation });
        Fraction::ZERO
    })
}

fn fast_query_time(time: Fraction, factor: Fraction, operation: &'static str) -> Fraction {
    checked_time(time.checked_mul(factor), operation)
}

fn fast_hap_time(time: Fraction, factor: Fraction, operation: &'static str) -> Fraction {
    checked_time(time.checked_div(factor), operation)
}

fn shift_query_time(time: Fraction, by: Fraction, operation: &'static str) -> Fraction {
    checked_time(time.checked_sub(by), operation)
}

fn shift_hap_time(time: Fraction, by: Fraction, operation: &'static str) -> Fraction {
    checked_time(time.checked_add(by), operation)
}

pub fn note_stepwise_entries_materialised(entries: u64) {
    STEPWISE_ENTRIES_MATERIALISED.with(|count| {
        count.set(count.get().saturating_add(entries));
    });
}

pub(crate) fn note_stepwise_segments_materialised(segments: u64) {
    note_stepwise_entries_materialised(segments);
}

/// Completed expansion entries reported by stepwise constructors on this
/// thread since the last reset. This is a mutation-sensitive construction
/// counter, not an allocator or total-resident-memory measurement.
pub fn stepwise_entries_materialised() -> u64 {
    STEPWISE_ENTRIES_MATERIALISED.with(std::cell::Cell::get)
}

/// Getter kept for the progressive-segment tests.
pub fn stepwise_segments_materialised() -> u64 {
    stepwise_entries_materialised()
}

pub fn reset_stepwise_entries_materialised() {
    STEPWISE_ENTRIES_MATERIALISED.with(|count| count.set(0));
}

/// Reset kept for the progressive-segment tests.
pub fn reset_stepwise_segments_materialised() {
    reset_stepwise_entries_materialised();
}

/// Check `n` haps against the budget, returning `false` if it is exceeded.
///
/// This is a high-water bound (the largest vector any single step may hold),
/// not a running total. `query` charges every node's result, and a hap
/// produced at a leaf passes up through each of its ancestors. A running
/// total would count the same haps once per level: a 64-hap pattern four
/// levels deep would be charged 256. That measures tree depth, not size.
///
/// The high-water bound also matches the purpose: the concern is a single
/// oversized `Vec<Hap>`, and sibling vectors are bounded by depth times the
/// budget.
///
/// Callers must check the result and stop producing; the flag makes the failure
/// visible at the boundary either way.
pub(crate) fn charge_haps(n: usize) -> bool {
    QUERY_THREAD.with(|thread| thread.charge(n))
}

/// The hap budget of the current top-level query, or `None` outside one.
pub(crate) fn hap_budget() -> Option<u64> {
    HAP_BUDGET.with(std::cell::Cell::get)
}

/// Push into an accumulating result, refusing before the budget is exceeded.
///
/// Returns `false` when the caller must STOP producing. The generic charge in
/// `query` sees a node's result only once it is finished, which is too late for
/// an AMPLIFIER: a join's output is the product of its inputs, so two legal
/// operands can multiply into something enormous inside one loop. Charging per
/// push bounds the vector as it grows rather than inspecting it after completion.
pub(crate) fn push_budgeted(out: &mut Vec<Hap>, hap: Hap) -> bool {
    push_budgeted_from(out, 0, hap)
}

/// [`push_budgeted`] into a list that held `from` haps of other nodes
/// before this one wrote: the bound is on the haps of this node, as it is
/// Push `hap` onto `out`, which held `from` haps before this node wrote.
pub(crate) fn push_budgeted_from(out: &mut Vec<Hap>, from: usize, hap: Hap) -> bool {
    QUERY_THREAD.with(|thread| thread.push_budgeted(out, from, hap))
}

/// Keep the haps of `out` from `from` on that `keep` takes, in their order.
/// `keep` may change a hap that it takes.
pub(crate) fn retain_from(out: &mut Vec<Hap>, from: usize, mut keep: impl FnMut(&mut Hap) -> bool) {
    let mut kept = from;
    for at in from..out.len() {
        if keep(&mut out[at]) {
            if kept != at {
                out.swap(kept, at);
            }
            kept += 1;
        }
    }
    out.truncate(kept);
}

thread_local! {
    /// The empty lists that the walks of this thread gave back, each with
    /// the room it grew to.
    static SPARE_LISTS: std::cell::RefCell<Vec<Vec<Hap>>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Empty `ResolvedHap` buffers recycled across join queries on this thread.
    static SPARE_RESOLVED: std::cell::RefCell<Vec<Vec<ResolvedHap>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn take_resolved_list() -> Vec<ResolvedHap> {
    SPARE_RESOLVED
        .try_with(|spare| spare.borrow_mut().pop())
        .ok()
        .flatten()
        .unwrap_or_default()
}

fn put_resolved_list(mut list: Vec<ResolvedHap>) {
    const MOST_LISTS: usize = 4;
    const MOST_ROOM: usize = 16;
    list.clear();
    if list.capacity() <= MOST_ROOM {
        let _ = SPARE_RESOLVED.try_with(|spare| {
            let mut spare = spare.borrow_mut();
            if spare.len() < MOST_LISTS {
                spare.push(list);
            }
        });
    }
}

/// Run `work` with an empty list, for the haps that a node reads from the
/// node under it and makes its own haps of. The list goes back to the
/// thread after the work with its room, so the next node takes it and
/// allocates nothing.
pub(crate) fn with_list<R>(work: impl FnOnce(&mut Vec<Hap>) -> R) -> R {
    /// The most lists that a thread keeps, and the most haps that a kept
    /// list has room for.
    const MOST_LISTS: usize = 8;
    const MOST_ROOM: usize = 64;
    // The lists are gone when the thread ends.
    let spare = SPARE_LISTS.try_with(|spare| spare.borrow_mut().pop());
    let mut list = spare
        .ok()
        .flatten()
        .unwrap_or_else(|| Vec::with_capacity(4));
    let result = work(&mut list);
    list.clear();
    if list.capacity() > 0 && list.capacity() <= MOST_ROOM {
        let _ = SPARE_LISTS.try_with(|spare| {
            let mut spare = spare.borrow_mut();
            if spare.len() < MOST_LISTS {
                spare.push(list);
            }
        });
    }
    result
}

/// Each hap of `out` from place `from` on through `map`, which takes the
/// hap: the hap that `map` gives takes a place in the list, in the order of
/// the haps, and a hap that `map` drops leaves the list.
fn map_from(out: &mut Vec<Hap>, from: usize, mut map: impl FnMut(Hap) -> Option<Hap>) {
    let len = out.len();
    if from >= len {
        return;
    }
    // SAFETY: the list forgets the haps from `from` on before any is read,
    // so each is read one time and none is dropped by the list: should
    // `map` unwind, the haps that it did not take leak. A hap is written at
    // or before the place of the hap that was read last, inside the room of
    // the list, and the list then holds the haps written so far.
    unsafe {
        out.set_len(from);
        let base = out.as_mut_ptr();
        let mut kept = from;
        for at in from..len {
            if let Some(mapped) = map(std::ptr::read(base.add(at))) {
                std::ptr::write(base.add(kept), mapped);
                kept += 1;
                out.set_len(kept);
            }
        }
    }
}

/// Put `haps` after those of `out`. An empty list with no room takes the
/// list as it is, with no copy of a hap.
pub(crate) fn hand_over(mut haps: Vec<Hap>, out: &mut Vec<Hap>) {
    match out.capacity() {
        0 => *out = haps,
        _ => out.append(&mut haps),
    }
}

/// Concatenate per-branch results, refusing before the budget is exceeded.
///
/// The `flat_map(...).collect()` producers (`stack`, `splitQueries`,
/// `chooseCycles`, `slowcat`) grow by concatenation, so each branch can be
/// within the budget while the total is not: two 100-hap siblings make 200
/// under a budget of 100. Extending under the bound stops at the first
/// branch that would exceed it.
///
/// Returns `None` once the budget is spent, so callers propagate rather than
/// truncate.
pub(crate) fn extend_budgeted<T>(branches: impl IntoIterator<Item = Vec<T>>) -> Option<Vec<T>> {
    let mut branches = branches.into_iter();
    // The first branch is the answer so far, where it is.
    let mut out: Vec<T> = branches.next().unwrap_or_default();
    if !charge_haps(out.len()) {
        return None;
    }
    note_accumulated(out.len());
    for branch in branches {
        if !charge_haps(out.len() + branch.len()) {
            return None;
        }
        out.extend(branch);
        note_accumulated(out.len());
    }
    Some(out)
}

/// Report that the JavaScript host exhausted its heap.
///
/// Uses the resource channel rather than `signal_query_error`. The latter
/// means "the query threw here", which `queryArc` turns into an empty
/// result, and an out-of-memory callback must not be reported as silence.
///
/// L2 calls this when its budget-aware allocator structurally denies a request;
/// no exception text participates in the classification.
pub fn refuse_host_memory() {
    refuse(QueryLimit::HostMemory);
}

/// Report that synchronous JavaScript crossed its elapsed-time interrupt
/// deadline.
///
/// The JavaScript bridge calls this while a core query boundary is active so
/// callback-driven patterns cannot turn an interrupt exception into ordinary
/// `queryArc` silence. Like every structural refusal, the first limit raised
/// by the query wins. The stable public variant is named `JsCpuDeadline`, but
/// this mechanism is not process CPU-time accounting.
pub fn refuse_js_cpu_deadline(millis: u64) {
    refuse(QueryLimit::JsCpuDeadline { millis });
}

/// Report that a synchronous JavaScript query left runnable jobs behind.
///
/// Query-time JavaScript has no async-job contract. This therefore travels on
/// the typed resource channel rather than the query-error channel, where it
/// would be indistinguishable from a genuinely silent pattern.
pub fn refuse_js_pending_jobs() {
    refuse(QueryLimit::JsPendingJobs);
}

/// Refuse an oversized Euclidean mask through the typed resource channel.
pub fn refuse_euclid_steps(steps: u64, limit: u64) {
    refuse(QueryLimit::EuclidSteps { steps, limit });
}

/// Refuse a native-fraction overflow through the typed resource channel.
/// Extension nodes use this where their span shifts leave the range an
/// `i128` numerator can hold.
pub fn refuse_native_fraction(operation: &'static str) {
    refuse(QueryLimit::NativeFraction { operation });
}

/// Charge a growing collection outside this crate, by its current length.
///
/// `charge_haps` is a high-water bound on a single vector, so the caller
/// must pass the length the vector is about to reach. A constant 1 never
/// exceeds a positive budget and would accept unlimited pushes.
///
/// Returns `false` when the caller must stop.
pub fn charge_hap_at(len_after_push: usize) -> bool {
    charge_haps(len_after_push)
}

/// Whether the current query has already exceeded its budget.
///
/// The expired deadline counts even when no refusal is recorded: every
/// `with_hap_budget` boundary swaps the refusal out, and a JS `queryArc`
/// callback crosses one on every re-entry. Without this, a deep pattern
/// keeps making re-entries after its deadline. Each one is cheap, but tens
/// of thousands of them cost seconds on the producer thread.
pub(crate) fn budget_exhausted() -> bool {
    QUERY_STOP.with(|slot| slot.get()) & (STOP_REFUSED | STOP_DEADLINE_PASSED) != 0
}

/// All three stop signals in one thread-local read: a recorded refusal, a
/// past deadline, an installed cancellation flag. The word cannot answer
/// alone for the armed cases — a deadline must still sample the clock, and
/// a cancellation flag belongs to another thread — so those fall back to
/// their full checks.
pub(crate) fn query_stop_requested() -> bool {
    QUERY_THREAD.with(QueryThread::stop_requested)
}

/// Record a resource refusal. The first one wins, as with query errors.
pub(crate) fn refuse(limit: QueryLimit) {
    REFUSAL.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(limit);
            set_stop_bit(STOP_REFUSED, true);
        }
    });
}

/// The largest hap vector that actually EXISTED since the last reset.
///
/// Distinct from [`haps_materialised`], which counts leaf production, and from
/// the amount charged, which records an intended size rather than a realised
/// one. This is the only observable that separates "refused while
/// concatenating" from "concatenated everything, then refused": both end in the
/// same error, and the leaves are identical in both, so nothing else can tell
/// them apart.
pub fn peak_hap_vector() -> u64 {
    PEAK_CHARGE.with(|c| c.get())
}

/// Record a vector that has just grown to `len`.
fn note_accumulated(len: usize) {
    QUERY_THREAD.with(|thread| thread.note_accumulated(len));
}

/// Run `f` with `flag` as the cancellation signal for any query it performs.
///
/// The flag is read by `query` as it recurses, so a long query stops between
/// nodes rather than at the end. Another thread sets it - a signal watcher, a
/// UI, a scheduler - which is why this takes a shared atomic rather than a
/// thread-local.
///
/// # Safety
/// The pointer never escapes: it is cleared before returning, including on
/// unwind, so it cannot be read after `flag` could die.
pub fn with_cancellation<R>(flag: &std::sync::atomic::AtomicBool, f: impl FnOnce() -> R) -> R {
    /// Puts back the flag and the observation of the scope around this
    /// one, also on unwind.
    struct Around(Option<*const std::sync::atomic::AtomicBool>, bool);
    impl Drop for Around {
        fn drop(&mut self) {
            QUERY_THREAD.with(|thread| {
                thread.set_stop_bit(STOP_CANCEL_ARMED, self.0.is_some());
                thread.cancel.set(self.0);
                thread.cancel_observed.set(self.1);
            });
        }
    }
    let _around = QUERY_THREAD.with(|thread| {
        let previous = thread.cancel.replace(Some(flag as *const _));
        thread.set_stop_bit(STOP_CANCEL_ARMED, true);
        // A fresh scope starts with nothing observed; the enclosing
        // scope's observation is restored when it ends.
        Around(previous, thread.cancel_observed.replace(false))
    });
    f()
}

/// Run `f` with a native query deadline installed (restored afterwards).
pub fn with_query_deadline<T>(deadline: std::time::Instant, f: impl FnOnce() -> T) -> T {
    with_query_deadline_from(std::time::Instant::now(), deadline, f)
}

/// [`with_query_deadline`] for a caller that made `deadline` from a read of
/// the clock at `now`, so the clock is read one time.
pub fn with_query_deadline_from<T>(
    now: std::time::Instant,
    deadline: std::time::Instant,
    f: impl FnOnce() -> T,
) -> T {
    let _around = QUERY_THREAD.with(|thread| {
        // A budget around this deadline starts here at the latest.
        if thread.deadline.get().is_none()
            && let Some(budget) = thread.budget.get()
        {
            thread.deadline.set(Some(now + budget));
        }
        // A NESTED install must never buy more time than the deadline that
        // contains it: the live probe's 150 ms ceiling was being replaced
        // by the 2 s JS budget installed for a callback inside it, so
        // nothing bounded the probe at all.
        let previous = thread.deadline.get();
        let effective = match previous {
            Some(outer) => outer.min(deadline),
            None => deadline,
        };
        thread.deadline.set(Some(effective));
        let allowed = effective
            .saturating_duration_since(now)
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        let millis = thread.deadline_millis.replace(allowed);
        thread.set_stop_bit(STOP_DEADLINE_ARMED, true);
        // A nested scope starts un-expired and re-detects within its own
        // sampling interval; the enclosing scope's verdict is restored
        // when it ends.
        let expired = thread.deadline_expired.replace(false);
        thread.set_stop_bit(STOP_DEADLINE_PASSED, false);
        thread.deadline_tick.set(0);
        DeadlineAround {
            deadline: previous,
            budget: thread.budget.get(),
            millis,
            expired,
        }
    });
    f()
}

/// Puts back the deadline, the budget and the verdict of the scope around
/// a deadline or a budget, also on unwind.
struct DeadlineAround {
    deadline: Option<std::time::Instant>,
    budget: Option<std::time::Duration>,
    millis: u64,
    expired: bool,
}

impl Drop for DeadlineAround {
    fn drop(&mut self) {
        QUERY_THREAD.with(|thread| {
            let armed = self.deadline.is_some() || self.budget.is_some();
            thread.set_stop_bit(STOP_DEADLINE_ARMED, armed);
            thread.deadline.set(self.deadline);
            thread.budget.set(self.budget);
            thread.deadline_millis.set(self.millis);
            thread.set_stop_bit(STOP_DEADLINE_PASSED, self.expired);
            thread.deadline_expired.set(self.expired);
        });
    }
}

/// Run `f` with a native query deadline of `budget` from now, with no read
/// of the clock here. The query reads the clock at its first sample, and
/// its deadline is `budget` from that read, so a query of a few nodes reads
/// no clock at all. Inside another deadline or budget this is
/// [`with_query_deadline`] of `budget` from now.
pub fn with_query_budget<T>(budget: std::time::Duration, f: impl FnOnce() -> T) -> T {
    let around = QUERY_THREAD.with(|thread| {
        if thread.deadline.get().is_some() || thread.budget.get().is_some() {
            return None;
        }
        thread.budget.set(Some(budget));
        let allowed = budget.as_millis().min(u128::from(u64::MAX)) as u64;
        let millis = thread.deadline_millis.replace(allowed);
        thread.set_stop_bit(STOP_DEADLINE_ARMED, true);
        let expired = thread.deadline_expired.replace(false);
        thread.set_stop_bit(STOP_DEADLINE_PASSED, false);
        thread.deadline_tick.set(0);
        Some(DeadlineAround {
            deadline: None,
            budget: None,
            millis,
            expired,
        })
    });
    match around {
        Some(_around) => f(),
        None => {
            let now = std::time::Instant::now();
            with_query_deadline_from(now, now + budget, f)
        }
    }
}

/// Whether the installed native query deadline has passed. Sampled every
/// 64th call so the clock read stays off the hot path.
pub(crate) fn query_deadline_expired() -> bool {
    query_deadline_after(1)
}

/// [`query_deadline_expired`] for `checks` checks at once. A table answer
/// advances the sampling by the haps it made, as the graph advances it by
/// the leaves it visits, so a deadline is seen after the same amount of
/// work.
pub(crate) fn query_deadline_after(checks: u32) -> bool {
    QUERY_THREAD.with(|thread| thread.deadline_after(checks))
}

/// How long the deadline installed around the current query allowed, in
/// milliseconds from its installation; `None` outside any deadline.
pub fn installed_query_deadline_millis() -> Option<u64> {
    QUERY_THREAD.with(|thread| {
        (thread.deadline.get().is_some() || thread.budget.get().is_some())
            .then(|| thread.deadline_millis.get())
    })
}

/// Whether the current query has been asked to stop.
pub(crate) fn cancellation_requested() -> bool {
    QUERY_THREAD.with(QueryThread::cancellation_requested)
}

/// Whether a cancellation has been observed inside the innermost
/// `with_cancellation` scope. The shared flag belongs to another thread
/// and may be cleared again at any moment, so a result cache asks this
/// rather than the flag: a window cut short by a stop stays cut short.
static CANCELLATION_OBSERVED: QueryCell<std::cell::Cell<bool>> = QueryCell(|q| &q.cancel_observed);

/// Whether a cancellation was observed inside the innermost cancellable
/// scope, whatever the shared flag says now.
pub(crate) fn cancellation_observed() -> bool {
    CANCELLATION_OBSERVED.with(|observed| observed.get())
}

/// Haps materialised on this thread since [`reset_haps_materialised`].
///
/// Tests use this to distinguish "refused" from "refused after building the
/// haps". Both return the same result, so the return value cannot tell them
/// apart. The budget is charged before production so that the allocation
/// does not happen. Without this counter the pre-charge could be deleted
/// and every budget test would still pass.
pub fn haps_materialised() -> u64 {
    QUERY_THREAD.with(|t| t.materialised.get())
}

/// Reset the materialisation counter.
pub fn reset_haps_materialised() {
    QUERY_THREAD.with(|t| {
        t.materialised.set(0);
        t.peak.set(0);
    });
}

fn note_materialised(n: usize) {
    QUERY_THREAD.with(|t| t.note_materialised(n));
}

/// Install a budget for `f`, restoring the previous one afterwards.
///
/// Nested top-level queries (a callback querying another pattern) share the
/// outer budget rather than getting a fresh one. Otherwise a pattern could
/// evade the limit by querying in a loop.
fn with_hap_budget<R>(budget: u64, f: impl FnOnce() -> R) -> (R, Option<QueryLimit>) {
    let (previous, nested, previous_stepwise, had_refusal) = QUERY_THREAD.with(|thread| {
        let previous = thread.hap_budget.get();
        let nested = previous.is_some();
        let previous_stepwise = thread.stepwise_remaining.get();
        let had_refusal = !nested && (thread.stop.get() & STOP_REFUSED != 0);
        if !nested {
            thread.hap_budget.set(Some(budget));
            thread.stepwise_remaining.set(Some(MAX_STEPWISE_ENTRIES));
            if had_refusal {
                thread.set_stop_bit(STOP_REFUSED, false);
            }
        }
        (previous, nested, previous_stepwise, had_refusal)
    });
    let previous_refusal = if had_refusal {
        REFUSAL.with(|f| f.borrow_mut().take())
    } else {
        None
    };
    struct Guard {
        previous_budget: Option<u64>,
        previous_refusal: Option<QueryLimit>,
        previous_stepwise: Option<u64>,
        nested: bool,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            let previous = self.previous_refusal.take();
            let restore_refusal = !self.nested && previous.is_some();
            let mut touch_refusal = restore_refusal;
            QUERY_THREAD.with(|thread| {
                thread.hap_budget.set(self.previous_budget);
                if !self.nested {
                    thread.stepwise_remaining.set(self.previous_stepwise);
                    let had_inner = thread.stop.get() & STOP_REFUSED != 0;
                    if restore_refusal || had_inner {
                        thread.set_stop_bit(STOP_REFUSED, restore_refusal);
                        touch_refusal = true;
                    }
                }
            });
            if touch_refusal {
                REFUSAL.with(|slot| *slot.borrow_mut() = previous);
            }
        }
    }
    let guard = Guard {
        previous_budget: previous,
        previous_refusal,
        previous_stepwise,
        nested,
    };
    let out = f();
    let refusal = if QUERY_STOP.with(|s| s.get() & STOP_REFUSED != 0) {
        REFUSAL.with(|f| f.borrow().clone())
    } else {
        None
    };
    drop(guard);
    (out, refusal)
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
            let congruent = match (hap.whole, other.whole) {
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
            let inners = pattern.query(&state.set_span(outer.part));
            if query_error_pending() {
                return Vec::new();
            }
            for inner in inners {
                let whole = match (outer.whole, inner.whole) {
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
                    Hap {
                        whole,
                        part: inner.part,
                        value: Value::list(values),
                        context,
                        keys: ContextKeys::merge_with(&inner.keys, &outer.keys, [0; 4], 0, None),
                    },
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
        let match_state = state.set_span(random_hap.part);
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
                let Some(part) = weights_hap.part.intersection(&total_hap.part) else {
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
                let chooser_whole = weights_hap.whole;
                let mut chooser_context = total_hap.context.clone();
                chooser_context.append(&weights_hap.context);

                // `pat.bind(match)`: MIX the random hap with the
                // selected-pattern carrier.
                let outer_whole = match (random_hap.whole, chooser_whole) {
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
                Hap {
                    whole: if inner {
                        chosen.whole
                    } else {
                        carrier.outer_whole
                    },
                    part: chosen.part,
                    keys: ContextKeys::merge(&chosen.keys, &carrier.keys),
                    value: chosen.value,
                    context,
                },
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
        let shared = Arc::new(PatternExtras {
            runtime_settings: Some(settings.clone()),
            ..PatternExtras::default()
        });
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

    fn select(&self, value: &Value, index_mode: PickIndexMode) -> Option<&Pattern> {
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
                if !numeric.is_finite() || numeric < 0.0 || numeric > usize::MAX as f64 {
                    return None;
                }
                let index = numeric as usize;
                entries
                    .iter()
                    .find_map(|(candidate, pattern)| (*candidate == index).then_some(pattern))
            }
            Self::Object { entries, .. } => {
                if let Value::Str(key) = value {
                    return entries
                        .iter()
                        .find_map(|(candidate, pattern)| (candidate == key).then_some(pattern));
                }
                let key = pick_property_key(value);
                entries
                    .iter()
                    .find_map(|(candidate, pattern)| (candidate.as_str() == key).then_some(pattern))
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

    fn select(&self, value: &Value, index_mode: PickIndexMode) -> Option<&Pattern> {
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
                patterns.get(index)
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
            }
            Self::Object { entries, .. } => {
                if let Value::Str(key) = value {
                    return entries.iter().find_map(|(candidate, pattern)| {
                        (&**candidate == key.as_str()).then_some(pattern)
                    });
                }
                let key = pick_property_key(value);
                entries.iter().find_map(|(candidate, pattern)| {
                    (&**candidate == key.as_str()).then_some(pattern)
                })
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
) -> Option<usize> {
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
    if !numeric.is_finite() || numeric < 0.0 || numeric > usize::MAX as f64 {
        return None;
    }
    Some(numeric as usize)
}

#[derive(Clone, Copy)]
enum LookupRef<'a> {
    Full(&'a PickLookup),
    Compact(&'a CompactPickLookup),
}

impl<'a> LookupRef<'a> {
    #[inline]
    fn select(self, value: &Value, index_mode: PickIndexMode) -> Option<&'a Pattern> {
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
    Hap {
        whole,
        part,
        value: met.value,
        context,
        keys,
    }
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
            hf.part = part;
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
        hf.part = part;
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
        hf.part = part;
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
        hf.part = part;
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
fn pick_where_it_is(outer: &Hap, inner: &mut Hap, whole: Option<TimeSpan>, part: TimeSpan) {
    let mut context = outer.context.clone();
    context.append(&inner.context);
    inner.whole = whole;
    inner.part = part;
    inner.keys = ContextKeys::merge(&inner.keys, &outer.keys);
    inner.context = context;
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
        selector.answer_onto(thread, state, table::Order::Graph, selectors);
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
    ) && outer.whole.is_none()
    {
        // All three special joins begin with `discreteOnly()`: analog
        // carriers are removed before their selected value is dereferenced.
        return Some(());
    }
    let Some(inner_pattern) = lookup.select(&outer.value, index_mode) else {
        // `lookup[key]` is `undefined`; the selected value becomes a hap
        // normally and fails only when the join reaches it.
        signal_query_error(|| match mode {
            JoinMode::Squeeze => {
                "Cannot read properties of undefined (reading '_focusSpan')".into()
            }
            JoinMode::Reset | JoinMode::Restart => {
                "Cannot read properties of undefined (reading 'late')".into()
            }
            _ => "x.value.query is not a function".into(),
        });
        return None;
    };
    // The haps of the picked pattern, from here on.
    let picked = out.len();
    match mode {
        JoinMode::Inner | JoinMode::Outer | JoinMode::Mix => {
            inner_pattern.answer_onto(
                thread,
                &state.set_span(outer.part),
                table::Order::Graph,
                out,
            );
            for inner in &mut out[picked..] {
                let whole = match mode {
                    JoinMode::Inner => inner.whole,
                    JoinMode::Outer => outer.whole,
                    JoinMode::Mix => match (outer.whole, inner.whole) {
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
                let part = inner.part;
                pick_where_it_is(outer, inner, whole, part);
            }
        }
        JoinMode::Squeeze => {
            let Some(outer_whole) = outer.whole else {
                return Some(());
            };
            inner_pattern.query_focused_onto(
                thread,
                outer.whole_or_part(),
                &state.set_span(outer.part),
                out,
            );
            retain_from(out, picked, |inner| {
                let whole = match inner.whole {
                    Some(inner_whole) => match inner_whole.intersection(&outer_whole) {
                        Some(whole) => Some(whole),
                        None => return false,
                    },
                    None => None,
                };
                let Some(part) = inner.part.intersection(&outer.part) else {
                    return false;
                };
                inner.whole = whole;
                inner.part = part;
                inner.keys = ContextKeys::merge_with(
                    &outer.keys,
                    &inner.keys,
                    inner.live_controls(),
                    inner.slider_binding(),
                    inner.pick_lookup_arc(),
                );
                inner.context.append(&outer.context);
                true
            });
        }
        JoinMode::Reset | JoinMode::Restart => {
            let Some(outer_whole) = outer.whole else {
                return Some(());
            };
            let shift = if mode == JoinMode::Restart {
                outer_whole.begin
            } else {
                outer_whole.begin.cycle_pos()
            };
            let meet = |inner: &mut Hap, part| {
                let whole = inner
                    .whole
                    .and_then(|inner_whole| inner_whole.intersection(&outer_whole));
                pick_where_it_is(outer, inner, whole, part);
            };
            if let Node::Sequence(layers) = inner_pattern.node_kind()
                && inner_pattern.is_discrete()
            {
                // Each sequence step already lies inside the part it answers.
                let asked = state.set_span(outer.part);
                if shift == Fraction::ZERO || sequence::repeats_at(layers, shift, outer.part) {
                    inner_pattern.answer_onto(thread, &asked, table::Order::Graph, out);
                } else {
                    inner_pattern.query_moved_onto(thread, shift, "late", &asked, out);
                }
                for inner in &mut out[picked..] {
                    meet(inner, inner.part);
                }
            } else {
                inner_pattern.query_restarted_onto(thread, shift, state, outer.part, out);
                retain_from(out, picked, |inner| {
                    let Some(part) = inner.part.intersection(&outer.part) else {
                        return false;
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
                    &state.set_span(lookup_hap.hap.part),
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
                Hap {
                    whole: inner.whole,
                    part: inner.part,
                    keys: ContextKeys::merge(&inner.keys, &lookup_hap.hap.keys),
                    value: inner.value,
                    context,
                },
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

/// How the value side of an `appLeft` is read with no query of its graph for
/// each function hap, found once by the first query that meets the side.
#[derive(Default)]
pub(crate) struct ValueReading {
    found: std::sync::OnceLock<Option<Box<Reading>>>,
}

/// What a value side was found to be, and the settings state it was found
/// under. It answers only while that state is in force, as a table does.
struct Reading {
    settings: settings::SettingsStateId,
    values: ReadValues,
}

enum ReadValues {
    /// The side answers every cycle with one hap that fills the cycle and
    /// that only its span tells from the next cycle's, as a map of a `pure`
    /// gives. Asked for any span, the side answers one hap per cycle piece
    /// of the span, as [`Node::Pure`] does: this hap of cycle zero, moved
    /// to the piece. In a box, so that a signal's reading is not as large.
    Constant(Box<Hap>),
    /// The side is a signal, as `.lpf(sine.range(200, 2000))` gives, which
    /// answers as a table samples it. Stored inline inside `Box<Reading>` to
    /// avoid a second heap allocation and indirection.
    Signal(table::ValueSignal),
}

impl ValueReading {
    /// What `values` was found to be, when the settings state it was found
    /// under is in force.
    ///
    /// A constant is found by a query of cycle zero. A query that already
    /// failed or stopped does not look, and a look that fails or stops is
    /// not kept. Every query so signals what it did before.
    fn of(&self, values: &Pattern, state: &State) -> Option<&ReadValues> {
        let current = |reading: &Reading| settings::is_current_state(&reading.settings);
        if let Some(found) = self.found.get() {
            return found
                .as_deref()
                .filter(|reading| current(reading))
                .map(|reading| &reading.values);
        }
        let unsettled = || query_error_pending() || query_stop_requested();
        if unsettled() {
            return None;
        }
        let settings = settings::current_state_id();
        let found = match values.constant_cycle(state) {
            Some(hap) => Some(ReadValues::Constant(Box::new(hap))),
            None => table::ValueSignal::of(values).map(ReadValues::Signal),
        };
        if unsettled() {
            return None;
        }
        self.found
            .get_or_init(|| found.map(|values| Box::new(Reading { settings, values })))
            .as_deref()
            .filter(|reading| current(reading))
            .map(|reading| &reading.values)
    }

    /// Forget what a query read of the function side, which is another
    /// pattern now. The value side is what it was.
    fn forget_functions(&mut self) {
        self.found = std::sync::OnceLock::new();
    }
}

/// Pack a single `(begin, end)` range into a non-zero `u32` for [`RunApply`]:
/// `begin` in the high 20 bits and `length + 1` in the low 12 bits, leaving
/// `0` as the empty sentinel.
#[inline]
fn pack_run_loc((begin, end): (usize, usize)) -> Option<u32> {
    const LEN_BITS: u32 = 12;
    const MAX_LEN: u32 = (1 << LEN_BITS) - 2;
    const MAX_BEGIN: u32 = (1 << (32 - LEN_BITS)) - 1;
    let length = u32::try_from(end.checked_sub(begin)?).ok()?;
    let begin = u32::try_from(begin).ok()?;
    (begin <= MAX_BEGIN && length <= MAX_LEN).then_some((begin << LEN_BITS) | (length + 1))
}

#[inline]
fn unpack_run_loc(packed: u32) -> (usize, usize) {
    const LEN_BITS: u32 = 12;
    const LEN_MASK: u32 = (1 << LEN_BITS) - 1;
    let begin = (packed >> LEN_BITS) as usize;
    let length = ((packed & LEN_MASK) - 1) as usize;
    (begin, begin + length)
}

#[derive(Clone)]
struct RunSpilled {
    combine: Combine,
    flow: LookupFlow,
    single_key: u16,
    loc0: u32,
    more_context: Option<Locations>,
}

/// Compact 16-byte header for [`RunMeeting`] packing [`Combine`],
/// [`LookupFlow`], an inline single built-in control key, and up to one inline
/// packed source location (`loc0 != 0`). Rare `Combine::With` closures and
/// multi-range locations spill into [`RunSpilled`] so [`RunMeeting`] stays 40
/// bytes.
#[derive(Clone)]
pub(crate) enum RunApply {
    Composed {
        op: compose::ComposeOp,
        flow: LookupFlow,
        merged: bool,
        /// Non-zero packed built-in key when `RunMeeting::value` holds the
        /// single control value inline without allocating a `BuiltinMap<1>`.
        single_key: u16,
        loc0: u32,
    },
    Spilled(Box<RunSpilled>),
}

const _: () = assert!(std::mem::size_of::<RunApply>() <= 16);

impl RunApply {
    fn from_parts(
        apply: Combine,
        flow: LookupFlow,
        single_key: u16,
        loc0: u32,
        more_context: Option<Locations>,
    ) -> Self {
        if more_context.is_none() {
            match apply {
                Combine::Composed(op) => {
                    return Self::Composed {
                        op,
                        flow,
                        merged: false,
                        single_key,
                        loc0,
                    };
                }
                Combine::SetMerged => {
                    return Self::Composed {
                        op: compose::ComposeOp::Set,
                        flow,
                        merged: true,
                        single_key,
                        loc0,
                    };
                }
                Combine::With(_) => {}
            }
        }
        Self::Spilled(Box::new(RunSpilled {
            combine: apply,
            flow,
            single_key,
            loc0,
            more_context,
        }))
    }

    #[inline]
    fn single_key(&self) -> u16 {
        match self {
            Self::Composed { single_key, .. } => *single_key,
            Self::Spilled(s) => s.single_key,
        }
    }

    #[inline]
    fn flow(&self) -> LookupFlow {
        match self {
            Self::Composed { flow, .. } => *flow,
            Self::Spilled(s) => s.flow,
        }
    }

    #[inline]
    fn loc0(&self) -> u32 {
        match self {
            Self::Composed { loc0, .. } => *loc0,
            Self::Spilled(s) => s.loc0,
        }
    }

    #[inline]
    fn more_context(&self) -> Option<&Locations> {
        match self {
            Self::Composed { .. } => None,
            Self::Spilled(s) => s.more_context.as_ref(),
        }
    }

    #[inline]
    fn in_place_op(&self) -> Option<compose::ComposeOp> {
        match self {
            Self::Composed { op, .. } => Some(*op),
            Self::Spilled(s) => match &s.combine {
                Combine::Composed(op) => Some(*op),
                Combine::SetMerged => Some(compose::ComposeOp::Set),
                Combine::With(_) => None,
            },
        }
    }

    fn combine(&self) -> Combine {
        match self {
            Self::Composed { merged: true, .. } => Combine::SetMerged,
            Self::Composed { op, .. } => Combine::Composed(*op),
            Self::Spilled(s) => s.combine.clone(),
        }
    }

    fn same(&self, other: &Self) -> bool {
        if self.flow() != other.flow() || self.single_key() != other.single_key() {
            return false;
        }
        match (self, other) {
            (
                Self::Composed {
                    op: a, merged: ma, ..
                },
                Self::Composed {
                    op: b, merged: mb, ..
                },
            ) => a == b && ma == mb,
            _ => match (self.combine(), other.combine()) {
                (Combine::Composed(a), Combine::Composed(b)) => a == b,
                (Combine::SetMerged, Combine::SetMerged) => true,
                (Combine::With(a), Combine::With(b)) => Arc::ptr_eq(&a, &b),
                _ => false,
            },
        }
    }
}

/// One meeting of a run: the constant of one application, carrying only its
/// value and source locations (cycle spans are synthesized on demand). Sets of
/// maps of controls in a row are one meeting, which the build made of them.
/// See [`Pattern::set_control`].
#[derive(Clone)]
pub(crate) struct RunMeeting {
    pub(crate) value: Value,
    pub(crate) apply: RunApply,
}

const _: () = assert!(std::mem::size_of::<RunMeeting>() <= 40);

impl RunMeeting {
    #[inline]
    pub(crate) fn flow(&self) -> LookupFlow {
        self.apply.flow()
    }

    #[inline]
    pub(crate) fn combine(&self) -> Combine {
        self.apply.combine()
    }

    #[inline]
    fn in_place_op(&self) -> Option<compose::ComposeOp> {
        self.apply.in_place_op()
    }

    #[inline]
    fn values_meet_in_place(&self, function: &Value, value: &Value, flow: LookupFlow) -> bool {
        let Some(op) = self.apply.in_place_op() else {
            return false;
        };
        let compose = Combine::Composed(op);
        Meeting::values_meet_in_place(function, value, &compose, flow)
    }

    #[inline]
    fn context_is_empty(&self) -> bool {
        self.apply.loc0() == 0 && self.apply.more_context().is_none()
    }

    fn context(&self) -> Locations {
        if let Some(more) = self.apply.more_context() {
            more.clone()
        } else if self.apply.loc0() != 0 {
            let mut out = Locations::new();
            out.push(unpack_run_loc(self.apply.loc0()));
            out
        } else {
            Locations::new()
        }
    }

    #[inline]
    fn prepend_context_to(&self, target: &mut Locations) {
        if let Some(more) = self.apply.more_context() {
            let mut merged = more.clone();
            merged.append(target);
            *target = merged;
        } else if self.apply.loc0() != 0 {
            let mut merged = Locations::new();
            merged.push(unpack_run_loc(self.apply.loc0()));
            merged.append(target);
            *target = merged;
        }
    }

    fn same_context(&self, other: &Self) -> bool {
        match (self.apply.more_context(), other.apply.more_context()) {
            (None, None) => self.apply.loc0() == other.apply.loc0(),
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }

    fn pack_context(context: Locations) -> (u32, Option<Locations>) {
        if context.is_empty() {
            (0, None)
        } else if context.len() == 1
            && let Some(range) = context.get(0)
            && let Some(packed) = pack_run_loc(range)
        {
            (packed, None)
        } else {
            (0, Some(context))
        }
    }

    /// Materialize this constant as a cycle-zero [`Hap`] for fallback paths.
    pub(crate) fn as_hap(&self) -> Hap {
        let cycle = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
        let value = match self.apply.single_key() {
            0 => self.value.clone(),
            packed => Value::Object(OrderedMap::from_single_packed(packed, self.value.clone())),
        };
        let mut hap = Hap::new(Some(cycle), cycle, value);
        hap.context = self.context();
        hap
    }

    /// Meet the function hap `hf` with the constant as its application
    /// does for any hap: the constant is one hap in each cycle that the
    /// whole of `hf` covers, or its part. What the meetings make comes
    /// after the haps of `out`. `false` when the query stops.
    fn meet(
        &self,
        thread: &QueryThread,
        hf: Hap,
        mut cache: Option<&mut value::SharedWaveCache>,
        out: &mut Vec<Hap>,
    ) -> bool {
        let mut fallback: Option<(Hap, Combine)> = None;
        let mut stopped = false;
        let mut single: Option<TimeSpan> = None;
        let mut count = 0usize;
        hf.whole_or_part().for_each_cycle(|piece| {
            if let Some(part) = hf.part.intersection(&piece) {
                single = Some(part);
                count += 1;
            }
        });
        if count == 0 {
            return true;
        }
        if count == 1 {
            let part = single.expect("count == 1");
            let mut hap = hf;
            if !Meeting::in_place_run_meeting(&mut hap, self, part, cache) {
                let constant = self.as_hap();
                let apply = self.combine();
                hap = applied_hap(&hap, &constant, hap.whole, part, &apply, self.flow());
            }
            return !thread.stop_requested() && thread.push_budgeted(out, 0, hap);
        }
        hf.whole_or_part().for_each_cycle(|piece| {
            if stopped {
                return;
            }
            let Some(part) = hf.part.intersection(&piece) else {
                return;
            };
            let mut hap = hf.clone();
            if !Meeting::in_place_run_meeting(&mut hap, self, part, cache.as_deref_mut()) {
                let (constant, apply) =
                    fallback.get_or_insert_with(|| (self.as_hap(), self.combine()));
                hap = applied_hap(&hap, constant, hap.whole, part, apply, self.flow());
            }
            if thread.stop_requested() || !thread.push_budgeted(out, 0, hap) {
                stopped = true;
            }
        });
        !stopped
    }

    /// The meeting of the constant `value` at `context` by `apply`, as the
    /// `appLeft` of a `pure` of the value at those places makes it: the
    /// value in every cycle, on a hap that carries nothing more.
    fn of(apply: Combine, flow: LookupFlow, mut value: Value, context: Locations) -> Self {
        let mut single_key = 0u16;
        if let Value::Object(map) = &mut value {
            map.undress_patched();
            if matches!(apply, Combine::Composed(compose::ComposeOp::Set))
                && flow == LookupFlow::Set
                && let Some((packed, inner_val)) = map.single_packed_entry()
            {
                single_key = packed;
                value = inner_val.clone();
            }
        }
        let (loc0, more_context) = Self::pack_context(context);
        Self {
            value,
            apply: RunApply::from_parts(apply, flow, single_key, loc0, more_context),
        }
    }

    /// The op and the number of this meeting, when it is one of a number
    /// by a known op, on a hap that carries nothing more: no place in the
    /// text.
    pub(crate) fn number(&self) -> Option<(compose::ComposeOp, f64)> {
        match (&self.apply, &self.value) {
            (
                RunApply::Composed {
                    op, single_key: 0, ..
                },
                Value::F64(other),
            ) if self.context_is_empty() => Some((*op, *other)),
            _ => None,
        }
    }

    /// Whether the runs `left` and `right` of [`Node::Constants`] meet the
    /// same constants in the same way and in the same order: each value
    /// down to the bits of its numbers, at the same places in the text.
    pub(crate) fn same(left: &[Self], right: &[Self]) -> bool {
        left.len() == right.len()
            && left.iter().zip(right).all(|(a, b)| {
                a.apply.same(&b.apply) && a.value.same_data(&b.value) && a.same_context(b)
            })
    }

    /// Whether this meeting is a `set` of a constant map of controls, which
    /// the `set` of another map after it may join. A union with a map of
    /// `value` alone keeps the other side as it is, so such a constant
    /// takes none.
    fn takes_sets(&self) -> bool {
        let sets = self.apply.in_place_op() == Some(compose::ComposeOp::Set);
        if self.apply.single_key() != 0 {
            return self.flow() == LookupFlow::Set && sets;
        }
        let controls = self.value.as_object();
        self.flow() == LookupFlow::Set
            && sets
            && controls.is_some_and(|controls| !compose::union_drops(controls))
    }

    /// Whether this meeting takes the `set` of the map of controls
    /// `control` after it, as one constant of the union of the two: each
    /// hap then meets the union as it meets the two in turn.
    fn takes_set_after(&self, control: &Value) -> bool {
        let set = Combine::Composed(compose::ComposeOp::Set);
        let map = Value::Object(OrderedMap::new());
        self.takes_sets()
            && matches!(control, Value::Object(_))
            && control.is_data(0)
            && Meeting::values_meet_in_place(&map, control, &set, LookupFlow::Set)
    }

    /// Join the `set` of `control` at `context`, which this meeting
    /// [takes](Self::takes_set_after): the constant is then the union, with
    /// the locations of the later control first.
    fn of_single_control(packed_key: u16, value: Value) -> Self {
        Self {
            value,
            apply: RunApply::from_parts(
                Combine::Composed(compose::ComposeOp::Set),
                LookupFlow::Set,
                packed_key,
                0,
                None,
            ),
        }
    }

    fn set_single_after(&mut self, packed_key: u16, key: &Key, value: Value) {
        let prev_key = self.apply.single_key();
        if prev_key != 0 {
            if prev_key == packed_key {
                self.value = value;
                return;
            }
            let prev_val = std::mem::replace(&mut self.value, Value::Undefined);
            self.value = Value::Object(if packed_key != 0 {
                OrderedMap::from_two_packed(prev_key, prev_val, packed_key, value)
            } else {
                let mut map = OrderedMap::from_single_packed(prev_key, prev_val);
                map.set_data(key, value);
                map.mark_made_of_data();
                map
            });
            let flow = self.apply.flow();
            let (loc0, more) = Self::pack_context(self.context());
            self.apply = RunApply::from_parts(Combine::SetMerged, flow, 0, loc0, more);
            return;
        }
        if let Value::Object(map) = &mut self.value {
            map.set_data(key, value);
            map.mark_made_of_data();
        } else {
            let mut single = OrderedMap::of_one(key.clone(), value);
            single.mark_made_of_data();
            compose::compose_data_flat(
                compose::ComposeOp::Set,
                &mut self.value,
                &Value::Object(single),
            );
        }
        let flow = self.apply.flow();
        let (loc0, more) = Self::pack_context(self.context());
        self.apply = RunApply::from_parts(Combine::SetMerged, flow, 0, loc0, more);
    }

    fn set_after(&mut self, control: &Value, mut context: Locations) {
        let single_key = self.apply.single_key();
        if single_key != 0 {
            let val = std::mem::replace(&mut self.value, Value::Undefined);
            self.value = Value::Object(OrderedMap::from_single_packed(single_key, val));
        }
        compose::compose_data_flat(compose::ComposeOp::Set, &mut self.value, control);
        if !self.context_is_empty() {
            context.append(&self.context());
        }
        let flow = self.apply.flow();
        let (new_loc0, new_more) = Self::pack_context(context);
        self.apply = RunApply::from_parts(Combine::SetMerged, flow, 0, new_loc0, new_more);
    }
}

/// How one query reads a value side. It is settled at the first function
/// hap, so a query with none reads nothing.
enum Answering<'a> {
    /// The side's constant hap, to move from piece to piece.
    Constant(Hap),
    /// The side's signal, and a copy of the query's state for it to read.
    Signal(&'a table::ValueSignal, State),
    /// A query of the side's graph for each function hap.
    Graph,
}

/// What one query keeps from function hap to function hap while a
/// [`ValueSide`] meets them.
/// Cached per-span step-wave values, cut pieces, and shared patched table so
/// chord haps sharing `hf.part` and a `Heads::Keyed` base table do zero extra
/// allocations or grid time divisions.
struct StepWaveCache {
    part: TimeSpan,
    values: Arc<[Value]>,
    pieces: Vec<TimeSpan>,
    /// `(base, patched_table)` for the last keyed base table or sequence layer.
    table: Option<(value::PatchedBase, Arc<value::Table>)>,
}

#[derive(Default)]
struct StepWaveCacheSet {
    slots: [Option<StepWaveCache>; 4],
    next: u8,
}

impl StepWaveCacheSet {
    #[inline]
    fn find(&mut self, part: TimeSpan) -> Option<&mut StepWaveCache> {
        self.slots
            .iter_mut()
            .flatten()
            .find(|cached| cached.part == part)
    }

    fn insert(&mut self, cache: StepWaveCache) -> &mut StepWaveCache {
        let idx = match self.slots.iter().position(Option::is_none) {
            Some(empty) => empty,
            None => {
                let idx = usize::from(self.next) & 3;
                self.next = self.next.wrapping_add(1);
                idx
            }
        };
        self.slots[idx].insert(cache)
    }

    fn take_victim_pieces(&mut self, count: usize) -> Vec<TimeSpan> {
        if self.slots.iter().all(Option::is_some) {
            let idx = usize::from(self.next) & 3;
            if let Some(mut prev) = self.slots[idx].take() {
                prev.pieces.clear();
                prev.pieces.reserve(count);
                return prev.pieces;
            }
        }
        Vec::with_capacity(count)
    }
}

#[derive(Default)]
struct Meetings<'a> {
    answering: Option<Answering<'a>>,
    /// The haps the value side answers for one function hap.
    values: Vec<Hap>,
    /// The side as the control of a segment of a wave, when it is one.
    steps: Option<&'a fused::StepWave>,
    /// Cached step-wave evaluations and shared overlay tables for up to 4
    /// distinct `hf.part` spans, preventing thrashing across interleaved layers.
    step_cache: StepWaveCacheSet,
    /// Per-(met, part) overlay caches for chord notes meeting a wave signal.
    wave_cache: [Option<(TimeSpan, TimeSpan, value::SharedWaveCache)>; 2],
    wave_cache_next: u8,
}

impl<'a> Meetings<'a> {
    fn wave_overlay_cache(&mut self, met: TimeSpan, part: TimeSpan) -> &mut value::SharedWaveCache {
        if let Some(idx) = self
            .wave_cache
            .iter()
            .position(|slot| matches!(slot, Some((m, p, _)) if *m == met && *p == part))
        {
            return &mut self.wave_cache[idx].as_mut().expect("found slot").2;
        }
        let idx = if let Some(empty) = self.wave_cache.iter().position(Option::is_none) {
            empty
        } else {
            let victim = usize::from(self.wave_cache_next & 1);
            self.wave_cache_next = self.wave_cache_next.wrapping_add(1);
            victim
        };
        self.wave_cache[idx] = Some((met, part, value::SharedWaveCache::default()));
        &mut self.wave_cache[idx].as_mut().expect("inserted slot").2
    }
    /// The gate of `side`, when it is the gate of a degrade that this
    /// query reads: the meeting keeps the function hap as it is. See
    /// [`table::Gate`].
    fn gate(&mut self, side: &ValueSide<'a>, state: &State) -> Option<&'a table::Gate> {
        let keeps = side.flow == LookupFlow::Left
            && matches!(side.apply, Combine::With(combine) if combinators::keeps_left(combine));
        match self.answering.get_or_insert_with(|| side.answering(state)) {
            Answering::Signal(signal, _) if keeps => {
                let signal: &'a table::ValueSignal = signal;
                signal.gate()
            }
            _ => None,
        }
    }

    /// The name and the wave of `side`, when it is a control of a signal
    /// of numbers that this query reads. See [`table::Wave`].
    fn wave(&mut self, side: &ValueSide<'a>, state: &State) -> Option<(&'a Key, &'a table::Wave)> {
        match self.answering.get_or_insert_with(|| side.answering(state)) {
            Answering::Signal(signal, _) => {
                let signal: &'a table::ValueSignal = signal;
                signal.wave()
            }
            Answering::Constant(_) | Answering::Graph => None,
        }
    }

    /// The haps `side` answers over `met` for a function hap with `part`,
    /// and the control that names their values, when they are the values
    /// of a signal under a control.
    fn values(
        &mut self,
        thread: &QueryThread,
        side: &ValueSide<'a>,
        met: TimeSpan,
        part: TimeSpan,
        state: &State,
    ) -> (&[Hap], Option<&'a table::Named>) {
        let Self {
            answering, values, ..
        } = self;
        // The list keeps its room from one function hap to the next.
        let graph = |values: &mut Vec<Hap>| {
            values.clear();
            side.values.answer_onto(
                thread,
                &state.set_span(side.values.asked_for(met, part)),
                table::Order::Graph,
                values,
            );
        };
        match answering.get_or_insert_with(|| side.answering(state)) {
            Answering::Constant(constant) => {
                // One piece, the common case: the constant moved in place,
                // with no allocation.
                if met.within_one_cycle() {
                    constant.move_to_piece(met);
                    (std::slice::from_ref(constant), None)
                } else {
                    values.clear();
                    met.for_each_cycle(|piece| {
                        let mut hap = constant.clone();
                        hap.move_to_piece(piece);
                        values.push(hap);
                    });
                    (values, None)
                }
            }
            // A table samples a signal for a part in one cycle, and so does
            // the walk. Any other part asks the graph, which names each
            // value.
            Answering::Signal(signal, probe) => {
                let signal: &'a table::ValueSignal = signal;
                if part.within_one_cycle() && signal.answer(met, part, probe, values).is_some() {
                    return (values, signal.control());
                }
                graph(values);
                (values, None)
            }
            Answering::Graph => {
                graph(values);
                (values, None)
            }
        }
    }
}

/// One cycle of a concatenation appended onto `out`: the pattern whose turn the
/// cycle of `state` is writes its haps there, each moved to its cycle. The span
/// of `state` lies in one cycle.
fn slowcat_cycle_onto(thread: &QueryThread, pats: &[Pattern], state: &State, out: &mut Vec<Hap>) {
    if pats.is_empty() {
        return;
    }
    let n = pats.len() as i128;
    let cycle = state.span.begin.sam().numer();
    let pat = &pats[cycle.rem_euclid(n) as usize];
    slowcat_cycle_item_onto(thread, n, cycle, pat, state, out);
}

fn slowcat_cycle_pure_onto(
    thread: &QueryThread,
    cells: &[Arc<NodeCell>],
    state: &State,
    out: &mut Vec<Hap>,
) {
    if cells.is_empty() {
        return;
    }
    let n = cells.len() as i128;
    let cycle = state.span.begin.sam().numer();
    let cell = &cells[cycle.rem_euclid(n) as usize];
    if matches!(cell.kind, Node::Silence) {
        return;
    }
    let pat = plain_pattern(cell, 0);
    slowcat_cycle_item_onto(thread, n, cycle, &pat, state, out);
}

fn slowcat_cycle_item_onto(
    thread: &QueryThread,
    n: i128,
    cycle: i128,
    pat: &Pattern,
    state: &State,
    out: &mut Vec<Hap>,
) {
    if pat.is_rest() {
        return;
    }

    // `cycle - floor(cycle / n)` lies between 0 and `cycle` and is
    // never `i128::MIN`, so it and its negation fit. A count of whole
    // cycles moves a time with no division.
    let offset = cycle - cycle.div_euclid(n);
    let shift = |span: TimeSpan, by: i128| {
        Some(TimeSpan::new(
            span.begin.checked_add_int(by)?,
            span.end.checked_add_int(by)?,
        ))
    };
    let from = out.len();
    let moved = shift(state.span, -offset).and_then(|inner_span| {
        pat.answer_onto(
            thread,
            &state.set_span(inner_span),
            table::Order::Graph,
            out,
        );
        out[from..].iter_mut().try_for_each(|hap| {
            let part = shift(hap.part, offset)?;
            // A hap that is not cut reads the times of its part one time.
            if let Some(whole) = hap.whole.as_mut() {
                *whole = match *whole == hap.part {
                    true => part,
                    false => shift(*whole, offset)?,
                };
            }
            hap.part = part;
            Some(())
        })
    });
    if moved.is_none() {
        out.truncate(from);
        refuse(QueryLimit::NativeFraction {
            operation: "slowcat",
        });
    }
}

/// The value side of one `appLeft` node, as a query meets it.
#[derive(Clone, Copy)]
struct ValueSide<'a> {
    functions: &'a Pattern,
    values: &'a Pattern,
    apply: &'a Combine,
    flow: LookupFlow,
    reading: &'a ValueReading,
}

/// Reusable 4-slot stack cache for small (`2..=8` steps) segmented waves so
/// 1/8-cycle chord windows share one [`value::StepPatch`] across all steps and
/// every chord note from the same base.
type StepPatchCache = [Option<(value::PatchedBase, Option<u32>, value::StepPatch)>; 4];

struct SmallStepSlot {
    part: TimeSpan,
    count: u8,
    pieces: [TimeSpan; 8],
    nums: [f64; 8],
    step_patches: StepPatchCache,
    step_next: u8,
    fallback_caches: Option<Box<[value::SharedWaveCache; 8]>>,
}

impl SmallStepSlot {
    fn new(part: TimeSpan, count: usize, pieces: [TimeSpan; 8], nums: [f64; 8]) -> Self {
        Self {
            part,
            count: count as u8,
            pieces,
            nums,
            step_patches: [None, None, None, None],
            step_next: 0,
            fallback_caches: None,
        }
    }
}

/// Apply one segmented wave `(key, wave)` to `hf` using inline stack buffers
/// when `count <= 8`, sharing one [`value::StepPatch`] across all steps and
/// across chord haps from the same uniform-key base.
#[allow(clippy::too_many_arguments)]
fn meet_steps_small(
    thread: &QueryThread,
    mut hf: Hap,
    key: &Key,
    wave: &fused::Wave,
    grid: &fused::StepGrid,
    small_cache: &mut [Option<SmallStepSlot>; 4],
    small_next: &mut u8,
    state: &State,
    from: usize,
    out: &mut Vec<Hap>,
) -> Option<bool> {
    let count = grid.len();
    debug_assert!((2..=8).contains(&count));
    let slot_idx = if let Some(idx) = small_cache
        .iter()
        .position(|s| matches!(s, Some(slot) if slot.part == hf.part))
    {
        idx
    } else {
        let mut pieces = [TimeSpan::new(Fraction::ZERO, Fraction::ZERO); 8];
        let mut nums = [0.0f64; 8];
        let mut begin = grid.time(grid.steps().start)?;
        for (i, k) in grid.steps().enumerate() {
            if thread.stop_requested() {
                return Some(false);
            }
            let end = grid.time(k + 1)?;
            pieces[i] = grid.cut(k, TimeSpan::new(begin, end), hf.part);
            nums[i] = wave.step_value_at(begin, &state.controls)?;
            begin = end;
        }
        let idx = if let Some(empty) = small_cache.iter().position(Option::is_none) {
            empty
        } else {
            let victim = usize::from(*small_next & 3);
            *small_next = small_next.wrapping_add(1);
            victim
        };
        small_cache[idx] = Some(SmallStepSlot::new(hf.part, count, pieces, nums));
        idx
    };
    let slot = small_cache[slot_idx].as_mut().expect("small slot present");
    let count = usize::from(slot.count);
    let apply = Combine::Composed(compose::ComposeOp::Set);
    out.reserve(count);
    let bulk_ok = thread.charge(out.len() - from + count);
    // Single-allocation StepPatch fast path: one `Arc<StepPatch>` covers all
    // `count` steps AND every chord note sharing the same uniform-key base
    // (or the same `(base, row)` for non-uniform bases).
    if let Value::Object(map) = &hf.value
        && let Some((base, row, uniform)) = map.step_base()
    {
        let fixed_row = (!uniform).then_some(row);
        let sp = if let Some((_, _, cached)) = slot
            .step_patches
            .iter()
            .flatten()
            .find(|(b, r, _)| b.same_source(&base) && *r == fixed_row)
        {
            Some(cached.clone())
        } else if let Some(mut built) =
            value::StepPatch::new_for_row(base.clone(), row, &[key], true, count)
        {
            for (i, &num) in slot.nums.iter().enumerate().take(count) {
                built.set_single_slot(i, num);
            }
            let sp = built.finish();
            let idx = if let Some(empty) = slot.step_patches.iter().position(Option::is_none) {
                empty
            } else {
                let v = usize::from(slot.step_next & 3);
                slot.step_next = slot.step_next.wrapping_add(1);
                v
            };
            slot.step_patches[idx] = Some((base, fixed_row, sp.clone()));
            Some(sp)
        } else {
            None
        };
        if let Some(sp) = sp {
            if !hf.keys.is_empty() {
                let live = controls::live_slot(key.as_str());
                let k = hf.keys.get();
                if k.slider_binding != 0 || live.is_some_and(|s| k.live_controls[s] != 0) {
                    let edit = hf.keys.edit();
                    if let Some(s) = live {
                        edit.live_controls[s] = 0;
                    }
                    edit.slider_binding = 0;
                    hf.keys.trim();
                }
            }
            let mut template = Some(hf);
            for (i, &piece) in slot.pieces.iter().enumerate().take(count) {
                let mut hap = if i + 1 == count {
                    template.take().expect("last step moves template")
                } else {
                    template.as_ref().expect("template present").clone()
                };
                if let Value::Object(controls) = &mut hap.value {
                    controls.set_step_patch(&sp, row, i);
                }
                hap.part = piece;
                if bulk_ok {
                    out.push(hap);
                } else if thread.stop_requested() || !thread.push_budgeted(out, from, hap) {
                    return Some(false);
                }
            }
            if bulk_ok {
                thread.note_accumulated(out.len() - from);
            }
            fused::note(fused::Hit::StepMany);
            return Some(true);
        }
    }
    let caches = slot.fallback_caches.get_or_insert_with(|| {
        Box::new(std::array::from_fn(|_| value::SharedWaveCache::default()))
    });
    let mut template = Some(hf);
    for i in 0..count {
        let mut hap = if i + 1 == count {
            template.take().expect("last step moves template")
        } else {
            template.as_ref().expect("template present").clone()
        };
        if !Meeting::set_number_in_place_cached(
            &mut hap,
            key,
            slot.nums[i],
            slot.pieces[i],
            &apply,
            LookupFlow::Set,
            Some(&mut caches[i]),
        ) {
            return None;
        }
        if bulk_ok {
            out.push(hap);
        } else if thread.stop_requested() || !thread.push_budgeted(out, from, hap) {
            return Some(false);
        }
    }
    if bulk_ok {
        thread.note_accumulated(out.len() - from);
    }
    fused::note(fused::Hit::StepMany);
    Some(true)
}

impl<'a> ValueSide<'a> {
    /// The value side of `node`, when `node` is an `appLeft`.
    fn of(node: &'a Node) -> Option<Self> {
        let Node::AppLeft(pair) = node else {
            return None;
        };
        let AppLeftNode(functions, values, reading, apply, flow) = &**pair;
        Some(Self {
            functions,
            values,
            apply,
            flow: *flow,
            reading,
        })
    }

    /// How a query of `state` reads this side: with no query of its graph
    /// when the side was found to be a constant or a signal, and the signal
    /// may answer as its table does.
    fn answering(&self, state: &State) -> Answering<'a> {
        match self.reading.of(self.values, state) {
            Some(ReadValues::Constant(hap)) => Answering::Constant(Hap::clone(hap)),
            Some(ReadValues::Signal(signal)) if self.values.tables_may_answer() => {
                Answering::Signal(signal, state.clone())
            }
            _ => Answering::Graph,
        }
    }

    /// The haps that the function haps `functions` make with this side,
    /// from one query of the side, when its graph is one of
    /// [discrete](Pattern::is_discrete) haps that no reading answers. Each
    /// function hap asks the side over its part, so one query over all the
    /// parts holds the answer for each: the value haps that meet the part,
    /// cut to it, in the order of the answer.
    ///
    /// The haps come after those of `out`, and `functions` is then empty.
    /// `false` leaves both as they are: for a few function haps, which
    /// their own queries serve, for parts that leave most of the time
    /// between them free, for a part of no width, and for a side that a
    /// reading or its own queries answer.
    fn met_in_one_query(
        &self,
        functions: &mut Vec<Hap>,
        state: &State,
        out: &mut Vec<Hap>,
    ) -> bool {
        /// The fewest function haps that one query of the values repays.
        const REPAYS_FROM: usize = 4;
        let width = |span: TimeSpan| span.end.to_f64() - span.begin.to_f64();
        let asked = functions.len() >= REPAYS_FROM
            && self.values.is_discrete()
            && self.reading.of(self.values, state).is_none()
            && functions.iter().all(|hf| hf.part.begin < hf.part.end);
        let all = asked
            .then(|| {
                Some(TimeSpan::new(
                    functions.iter().map(|hf| hf.part.begin).min()?,
                    functions.iter().map(|hf| hf.part.end).max()?,
                ))
            })
            .flatten();
        let Some(all) = all else {
            return false;
        };
        // The values between the parts are made for nothing.
        let filled: f64 = functions.iter().map(|hf| width(hf.part)).sum();
        if filled * 2.0 < width(all) {
            return false;
        }
        with_list(|values| {
            QUERY_THREAD.with(|thread| {
                self.values
                    .answer_onto(thread, &state.set_span(all), table::Order::Graph, values);
            });
            let n_vals = values.len();
            // Stack fast path for <= 32 value haps (all normal window queries):
            // zero heap allocations for `by_begin`, `latest_end`, and `meeting`.
            let mut small_by_begin = [0usize; 32];
            let mut small_latest_end = [Fraction::ZERO; 32];
            let mut small_meeting = [0usize; 32];
            let heap_by_begin;
            let mut heap_latest_end = Vec::new();
            let mut heap_meeting = Vec::new();
            let (by_begin, latest_end): (&[usize], &[Fraction]) = if n_vals <= 32 {
                for (i, slot) in small_by_begin[..n_vals].iter_mut().enumerate() {
                    *slot = i;
                }
                small_by_begin[..n_vals]
                    .sort_by(|a, b| values[*a].part.begin.cmp(&values[*b].part.begin));
                let mut latest = Fraction::ZERO;
                for (i, &index) in small_by_begin[..n_vals].iter().enumerate() {
                    let end = values[index].part.end;
                    latest = if i == 0 { end } else { end.max(latest) };
                    small_latest_end[i] = latest;
                }
                (&small_by_begin[..n_vals], &small_latest_end[..n_vals])
            } else {
                let mut indices: Vec<usize> = (0..n_vals).collect();
                indices.sort_by(|a, b| values[*a].part.begin.cmp(&values[*b].part.begin));
                heap_by_begin = indices;
                heap_latest_end.reserve(n_vals);
                for &index in &heap_by_begin {
                    let end = values[index].part.end;
                    heap_latest_end.push(
                        heap_latest_end
                            .last()
                            .map_or(end, |latest| end.max(*latest)),
                    );
                }
                (&heap_by_begin, &heap_latest_end)
            };
            let from = out.len();
            out.reserve(functions.len());
            for mut hf in functions.drain(..) {
                let span = hf.part;
                let first = latest_end.partition_point(|end| *end <= span.begin);
                let after = by_begin.partition_point(|index| values[*index].part.begin < span.end);
                let slice = by_begin.get(first..after).unwrap_or_default();
                let meeting: &[usize] = if n_vals <= 32 {
                    let mut m_len = 0usize;
                    for &index in slice {
                        if values[index].part.end > span.begin {
                            small_meeting[m_len] = index;
                            m_len += 1;
                        }
                    }
                    small_meeting[..m_len].sort_unstable();
                    &small_meeting[..m_len]
                } else {
                    heap_meeting.clear();
                    heap_meeting.extend(
                        slice
                            .iter()
                            .copied()
                            .filter(|index| values[*index].part.end > span.begin),
                    );
                    heap_meeting.sort_unstable();
                    &heap_meeting
                };
                // The part of the value hap, as the query of the part cuts it,
                // then where the two haps meet.
                let met = |hv: &Hap| {
                    let cut =
                        TimeSpan::new(hv.part.begin.max(span.begin), hv.part.end.min(span.end));
                    span.intersection(&cut)
                };
                // One meeting is the hap's own, as in [`Self::meet`].
                if let [index] = meeting
                    && let Some(part) = met(&values[*index])
                {
                    let hv = &values[*index];
                    if !Meeting::in_place(&mut hf, hv, part, self.apply, self.flow) {
                        hf = applied_hap(&hf, hv, hf.whole, part, self.apply, self.flow);
                    }
                    if query_stop_requested() || !push_budgeted_from(out, from, hf) {
                        out.truncate(from);
                        return;
                    }
                    continue;
                }
                for hv in meeting.iter().map(|index| &values[*index]) {
                    let Some(part) = met(hv) else {
                        continue;
                    };
                    let hap = applied_hap(&hf, hv, hf.whole, part, self.apply, self.flow);
                    if query_stop_requested() || !push_budgeted_from(out, from, hap) {
                        out.truncate(from);
                        return;
                    }
                }
            }
        });
        true
    }

    /// The haps of the function hap `hf` that a wave in steps makes, one
    /// for each step that its part meets, with the number of the step set
    /// under `key`. Whether the query goes on, or `None` for a meeting that
    /// the haps of the steps make, with `out` as it was.
    #[allow(clippy::too_many_arguments)]
    fn meet_steps(
        &self,
        thread: &QueryThread,
        hf: &Hap,
        steps: &fused::StepWave,
        step_cache: &mut StepWaveCacheSet,
        state: &State,
        from: usize,
        out: &mut Vec<Hap>,
    ) -> Option<bool> {
        let set = matches!(self.apply, Combine::Composed(compose::ComposeOp::Set))
            && self.flow == LookupFlow::Set;
        if !set || hf.pick_lookup().is_some() || !hf.value.is_data(0) {
            return None;
        }
        let Value::Object(base_map) = &hf.value else {
            return None;
        };
        let cached = if step_cache.find(hf.part).is_some() {
            step_cache.find(hf.part).expect("checked present")
        } else {
            let grid = steps.grid_over(hf.part)?;
            let count = grid.len();
            if count == 0 {
                return Some(true);
            }
            if thread.stop_requested() {
                return Some(false);
            }
            // Single-step shortcut avoids allocating a shared Table or cache.
            if count == 1 {
                let k = grid.steps().start;
                let begin = grid.time(k)?;
                let end = grid.time(k + 1)?;
                let piece = grid.cut(k, TimeSpan::new(begin, end), hf.part);
                let number = steps.value_at(begin, &state.controls)?;
                let mut hap = hf.clone();
                if !Meeting::set_number_in_place(
                    &mut hap, &steps.key, number, piece, self.apply, self.flow,
                ) {
                    return None;
                }
                if thread.stop_requested() || !thread.push_budgeted(out, from, hap) {
                    return Some(false);
                }
                fused::note(fused::Hit::StepMany);
                return Some(true);
            }
            if count <= 8 {
                let mut pieces = [TimeSpan::new(Fraction::ZERO, Fraction::ZERO); 8];
                let mut nums = [0.0f64; 8];
                let mut begin = grid.time(grid.steps().start)?;
                for (i, k) in grid.steps().enumerate() {
                    if thread.stop_requested() {
                        return Some(false);
                    }
                    let end = grid.time(k + 1)?;
                    pieces[i] = grid.cut(k, TimeSpan::new(begin, end), hf.part);
                    nums[i] = steps.value_at(begin, &state.controls)?;
                    begin = end;
                }
                out.reserve(count);
                let bulk_ok = thread.charge(out.len() - from + count);
                for i in 0..count {
                    let mut hap = hf.clone();
                    if !Meeting::set_number_in_place(
                        &mut hap, &steps.key, nums[i], pieces[i], self.apply, self.flow,
                    ) {
                        return None;
                    }
                    if bulk_ok {
                        out.push(hap);
                    } else if thread.stop_requested() || !thread.push_budgeted(out, from, hap) {
                        return Some(false);
                    }
                }
                if bulk_ok {
                    thread.note_accumulated(out.len() - from);
                }
                fused::note(fused::Hit::StepMany);
                return Some(true);
            }
            let mut pieces = step_cache.take_victim_pieces(count);
            let mut begin = grid.time(grid.steps().start)?;
            let values: Arc<[Value]> = if count <= 32 {
                let mut nums = [0.0f64; 32];
                for (slot, k) in nums[..count].iter_mut().zip(grid.steps()) {
                    if thread.stop_requested() {
                        return Some(false);
                    }
                    let end = grid.time(k + 1)?;
                    pieces.push(grid.cut(k, TimeSpan::new(begin, end), hf.part));
                    *slot = steps.value_at(begin, &state.controls)?;
                    begin = end;
                }
                nums[..count].iter().map(|&n| Value::F64(n)).collect()
            } else {
                let mut vals = Vec::with_capacity(count);
                for k in grid.steps() {
                    if thread.stop_requested() {
                        return Some(false);
                    }
                    let end = grid.time(k + 1)?;
                    pieces.push(grid.cut(k, TimeSpan::new(begin, end), hf.part));
                    vals.push(Value::F64(steps.value_at(begin, &state.controls)?));
                    begin = end;
                }
                Arc::from(vals)
            };
            step_cache.insert(StepWaveCache {
                part: hf.part,
                values,
                pieces,
                table: None,
            })
        };
        let count = cached.pieces.len();
        // Fast path: share one `PatchedTable` across all chord haps backed by
        // the same keyed base table or direct-keyed sequence layer.
        let shared_patched = if let Some((base_source, base_row)) = base_map.dressed_base() {
            if let Some((cached_base, cached_patched)) = &cached.table
                && cached_base.same_source(&base_source)
            {
                Some((Arc::clone(cached_patched), base_row))
            } else if let Some(patched) = value::Table::patched_keyed_table(
                &base_source,
                steps.key.clone(),
                Arc::clone(&cached.values),
            ) {
                let patched = Arc::new(patched);
                cached.table = Some((base_source, Arc::clone(&patched)));
                Some((patched, base_row))
            } else {
                None
            }
        } else {
            None
        };
        let fallback_table = if shared_patched.is_none() {
            Some(Arc::new(value::Table::patched(
                base_map,
                steps.key.clone(),
                Arc::clone(&cached.values),
            )?))
        } else {
            None
        };
        let mut template = hf.clone();
        // Clear value on the template so per-step clones do not bump base_map's Arc.
        template.value = Value::Undefined;
        if !template.keys.is_empty() {
            let slot = controls::live_slot(steps.key.as_str());
            let k = template.keys.get();
            if k.slider_binding != 0 || slot.is_some_and(|s| k.live_controls[s] != 0) {
                let edit = template.keys.edit();
                if let Some(slot) = slot {
                    edit.live_controls[slot] = 0;
                }
                edit.slider_binding = 0;
                template.keys.trim();
            }
        }
        out.reserve(count);
        let bulk_ok = thread.charge(out.len() - from + count);
        for (index, &piece) in cached.pieces.iter().enumerate() {
            let map = match &shared_patched {
                Some((table, base_row)) => table.map_packed(*base_row, index),
                None => fallback_table
                    .as_ref()
                    .expect("fallback table")
                    .map(index)?,
            };
            let mut hap = template.clone();
            hap.value = Value::Object(map);
            hap.part = piece;
            if bulk_ok {
                out.push(hap);
            } else if thread.stop_requested() || !thread.push_budgeted(out, from, hap) {
                return Some(false);
            }
        }
        if bulk_ok {
            thread.note_accumulated(out.len() - from);
        }
        fused::note(fused::Hit::StepMany);
        Some(true)
    }

    /// Meet the function hap `hf` with the value side and push what they
    /// make after the haps of `out`, which held `from` haps of other nodes
    /// before this application wrote. `false` when the query stops.
    /// [`Self::meet_on`] for a test, which has no thread in hand.
    #[cfg(test)]
    pub(crate) fn meet(
        &self,
        hf: Hap,
        meetings: &mut Meetings<'a>,
        state: &State,
        from: usize,
        out: &mut Vec<Hap>,
    ) -> bool {
        QUERY_THREAD.with(|thread| self.meet_on(thread, hf, meetings, state, from, out))
    }

    fn meet_on(
        &self,
        thread: &QueryThread,
        mut hf: Hap,
        meetings: &mut Meetings<'a>,
        state: &State,
        from: usize,
        out: &mut Vec<Hap>,
    ) -> bool {
        let met = hf.whole_or_part();
        // The control of a segment of a wave takes the number of each step
        // that the part meets. A hap that it cannot set is met as any other.
        if let Some(steps) = meetings.steps.as_ref() {
            let part = hf.part;
            match steps.number(part, &state.controls) {
                Some(number) => {
                    if Meeting::set_number_in_place(
                        &mut hf, &steps.key, number, part, self.apply, self.flow,
                    ) {
                        fused::note(fused::Hit::StepOne);
                        return !thread.stop_requested() && thread.push_budgeted(out, from, hf);
                    }
                }
                None => {
                    if let Some(stopped) = self.meet_steps(
                        thread,
                        &hf,
                        steps,
                        &mut meetings.step_cache,
                        state,
                        from,
                        out,
                    ) {
                        return stopped;
                    }
                }
            }
        }
        // A gate says whether the hap stays as it is, with no hap of its
        // own. A hap with a lookup keeps it only while its value equals
        // itself, which the meeting reads.
        if hf.pick_lookup().is_none()
            && let Some(gate) = meetings.gate(self, state)
            && hf.part.within_one_cycle()
            && let Some(part) = hf.part.intersection(&met)
            && let Some(passes) = gate.passes(met, part, &state.controls)
        {
            hf.part = part;
            return !passes || (!thread.stop_requested() && thread.push_budgeted(out, from, hf));
        }
        // A wave gives its number with no hap of its own.
        if let Some((key, wave)) = meetings.wave(self, state)
            && hf.part.within_one_cycle()
            && let Some(part) = hf.part.intersection(&met)
            && let Some(number) = wave.number(met, part, &state.controls)
            && Meeting::set_number_in_place_cached(
                &mut hf,
                key,
                number,
                part,
                self.apply,
                self.flow,
                Some(meetings.wave_overlay_cache(met, part)),
            )
        {
            return !thread.stop_requested() && thread.push_budgeted(out, from, hf);
        }
        let (inner, named) = meetings.values(thread, self, met, hf.part, state);
        let Some(named) = named else {
            return meet_each(thread, hf, inner, self.apply, self.flow, from, out);
        };
        // The one value of a signal is set under the name of its control.
        if let [hv] = inner
            && let Some(part) = hf.part.intersection(&hv.part)
            && Meeting::set_in_place(&mut hf, &named.key, hv, part, self.apply, self.flow)
        {
            return !thread.stop_requested() && thread.push_budgeted(out, from, hf);
        }
        // Any other meeting reads each value as the control names it.
        let inner: Vec<Hap> = inner
            .iter()
            .filter_map(|hv| (named.map)(hv.clone()))
            .collect();
        meet_each(thread, hf, &inner, self.apply, self.flow, from, out)
    }
}

/// Meet the function hap `hf` with the value haps `inner` of an application
/// that combines by `apply`, and push what they make after the haps of
/// `out`, which held `from` haps of other nodes before the application
/// wrote. `false` when the query stops.
fn meet_each(
    thread: &QueryThread,
    mut hf: Hap,
    inner: &[Hap],
    apply: &Combine,
    flow: LookupFlow,
    from: usize,
    out: &mut Vec<Hap>,
) -> bool {
    // One value hap meets this function hap, and that one meeting is
    // the hap's own: the hap is consumed where it is, as a sampled
    // meeting is. Several meetings share the function hap as their
    // base, so each keeps a copy of it.
    if let [hv] = inner
        && let Some(part) = hf.part.intersection(&hv.part)
    {
        // A refused in-place meeting leaves the hap unchanged.
        if !Meeting::in_place(&mut hf, hv, part, apply, flow) {
            hf = applied_hap(&hf, hv, hf.whole, part, apply, flow);
        }
        return !thread.stop_requested() && thread.push_budgeted(out, from, hf);
    }
    for hv in inner {
        let Some(part) = hf.part.intersection(&hv.part) else {
            continue;
        };
        let hap = applied_hap(&hf, hv, hf.whole, part, apply, flow);
        if thread.stop_requested() || !thread.push_budgeted(out, from, hap) {
            return false;
        }
    }
    true
}

/// Answer `state` after the haps of `out`: each hap of `base` meets the
/// constant of each of `meetings` in turn.
///
/// A hap inside one cycle meets each constant over the same part and stays
/// one hap, so each meeting is made where the hap is, or of the hap and the
/// constant. A hap that covers several cycles meets each constant as the
/// node of its application does. A query that stops writes no hap.
fn query_run(
    thread: &QueryThread,
    base: &Pattern,
    meetings: &[RunMeeting],
    state: &State,
    out: &mut Vec<Hap>,
) {
    let from = out.len();
    base.answer_onto(thread, state, table::Order::Graph, out);
    if out.len() == from || thread.stop_requested() {
        return;
    }
    // Single-hap fast path: avoid stack-initializing `SharedWaveCache` arrays
    // and running the `retain_from` closure when a voice emits one hap.
    if out.len() - from == 1 {
        let hf = &mut out[from];
        let met = hf.whole_or_part();
        match hf.part.intersection(&met) {
            None => {
                out.truncate(from);
                return;
            }
            Some(part) if part.within_one_cycle() => {
                for meeting in meetings {
                    if Meeting::in_place_run_meeting(hf, meeting, part, None) {
                        continue;
                    }
                    let constant = meeting.as_hap();
                    let apply = meeting.combine();
                    *hf = applied_hap(hf, &constant, hf.whole, part, &apply, meeting.flow());
                    if thread.stop_requested() {
                        out.truncate(from);
                        return;
                    }
                }
                if thread.stop_requested() {
                    out.truncate(from);
                }
                return;
            }
            Some(_) => {}
        }
    }
    // Fast in-place path when every hap's active intersection lies within a
    // single cycle (true for all sub-cycle windows even when `whole` spans
    // multiple cycles): zero scratch buffer, zero Hap moves.
    let single_cycle = out[from..].iter().all(|hf| hf.part.within_one_cycle())
        || out[from..].iter().all(|hf| {
            let met = hf.whole_or_part();
            match hf.part.intersection(&met) {
                Some(part) => part.within_one_cycle(),
                None => true,
            }
        });
    if single_cycle {
        let mut stopped = false;
        if let [meeting] = meetings {
            let mut cache = value::SharedWaveCache::default();
            retain_from(out, from, |hf| {
                if stopped {
                    return false;
                }
                let Some(part) = (match hf.whole {
                    None => (hf.part.begin <= hf.part.end).then_some(hf.part),
                    Some(whole) if whole == hf.part => {
                        (hf.part.begin <= hf.part.end).then_some(hf.part)
                    }
                    Some(whole) => hf.part.intersection(&whole),
                }) else {
                    return false;
                };
                if !Meeting::in_place_run_meeting(hf, meeting, part, Some(&mut cache)) {
                    let constant = meeting.as_hap();
                    let apply = meeting.combine();
                    *hf = applied_hap(hf, &constant, hf.whole, part, &apply, meeting.flow());
                    if thread.stop_requested() {
                        stopped = true;
                        return false;
                    }
                }
                if thread.stop_requested() {
                    stopped = true;
                    return false;
                }
                true
            });
            if stopped {
                out.truncate(from);
            }
            return;
        }
        let mut caches: [value::SharedWaveCache; 4] = Default::default();
        retain_from(out, from, |hf| {
            if stopped {
                return false;
            }
            let Some(part) = (match hf.whole {
                None => (hf.part.begin <= hf.part.end).then_some(hf.part),
                Some(whole) if whole == hf.part => {
                    (hf.part.begin <= hf.part.end).then_some(hf.part)
                }
                Some(whole) => hf.part.intersection(&whole),
            }) else {
                return false;
            };
            for (m_idx, meeting) in meetings.iter().enumerate() {
                if Meeting::in_place_run_meeting(hf, meeting, part, caches.get_mut(m_idx)) {
                    continue;
                }
                let constant = meeting.as_hap();
                let apply = meeting.combine();
                *hf = applied_hap(hf, &constant, hf.whole, part, &apply, meeting.flow());
                if thread.stop_requested() {
                    stopped = true;
                    return false;
                }
            }
            if thread.stop_requested() {
                stopped = true;
                return false;
            }
            true
        });
        if stopped {
            out.truncate(from);
        }
        return;
    }
    // Multi-cycle fallback: move haps into scratch list and expand per cycle.
    let answered = with_list(|haps| {
        haps.extend(out.drain(from..));
        out.reserve(haps.len());
        let use_cache = haps.len() > 1;
        let mut caches: [value::SharedWaveCache; 4] = Default::default();
        with_list(|made| {
            with_list(|next| {
                for mut hf in haps.drain(..) {
                    let met = hf.whole_or_part();
                    if !met.within_one_cycle() {
                        made.push(hf);
                        for (m_idx, meeting) in meetings.iter().enumerate() {
                            let mut cache = if use_cache {
                                caches.get_mut(m_idx)
                            } else {
                                None
                            };
                            for hf in made.drain(..) {
                                if !meeting.meet(thread, hf, cache.as_deref_mut(), next) {
                                    return None;
                                }
                            }
                            std::mem::swap(made, next);
                        }
                        out.append(made);
                        continue;
                    }
                    let Some(part) = hf.part.intersection(&met) else {
                        continue;
                    };
                    for (m_idx, meeting) in meetings.iter().enumerate() {
                        let cache = if use_cache {
                            caches.get_mut(m_idx)
                        } else {
                            None
                        };
                        if Meeting::in_place_run_meeting(&mut hf, meeting, part, cache) {
                            continue;
                        }
                        let constant = meeting.as_hap();
                        let apply = meeting.combine();
                        hf = applied_hap(&hf, &constant, hf.whole, part, &apply, meeting.flow());
                        if thread.stop_requested() {
                            return None;
                        }
                    }
                    if thread.stop_requested() || !thread.push_budgeted(out, from, hf) {
                        return None;
                    }
                }
                Some(())
            })
        })
    });
    if answered.is_none() {
        out.truncate(from);
    }
}

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
            Node::PureAt(boxed) if boxed.0.is_data(0) => Some(Hap {
                whole: Some(cycle),
                part: cycle,
                value: boxed.0.clone(),
                context: boxed.1.clone(),
                keys: ContextKeys::default(),
            }),
            Node::Sequence(_) => {
                let (value, context) = sequence::constant_value_at(self)?;
                Some(Hap {
                    whole: Some(cycle),
                    part: cycle,
                    value,
                    context,
                    keys: ContextKeys::default(),
                })
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
            (Some(hap), true) if hap.whole == Some(cycle) && hap.part == cycle => Some(hap),
            _ => None,
        }
    }
}

impl Hap {
    /// Move this cycle-zero hap of a [`Pattern::is_constant_per_cycle`]
    /// pattern to the hap that pattern answers for `piece`, one cycle piece
    /// of a span: the piece's cycle as the whole, the piece as the part.
    fn move_to_piece(&mut self, piece: TimeSpan) {
        self.whole = Some(whole_cycle(piece.begin));
        self.part = piece;
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
        | Node::Sequence(_)
        | Node::QueryError(_)
        | Node::QueryLimit(_)
        | Node::Signal(_)
        | Node::JsQuery(_)
        | Node::FastSmall(..)
        | Node::ShiftSmall(..)
        | Node::ControlMap(..)
        | Node::Join(..)
        | Node::SplitQueries(..) => None,
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
        intern: &mut dyn FnMut(usize, &Arc<PatternExtras>) -> Arc<PatternExtras>,
    ) {
        if let Some(ex) = &self.extras
            && let Some(key) = ex.plain_runtime_settings_key()
        {
            let canon = intern(key, ex);
            if !Arc::ptr_eq(ex, &canon) {
                self.extras = Some(canon);
            }
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

    /// Attach an owner without allocating a graph wrapper. The innermost owner
    /// wins, matching nested runtime scopes while flattening repeated exports.
    pub(crate) fn attach_runtime_settings(&mut self, settings: settings::RuntimeSettings) {
        if !self.has_runtime_settings() {
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
        self.extras_mut().runtime_settings = Some(settings);
    }

    /// The haps of this pattern, moved `shift` later, that a restart or a
    /// reset join reads for an outer hap with `part`, appended to `out`: the
    /// haps of the query's span, which the join then cuts to the part.
    ///
    /// A graph of discrete haps is asked over the part alone. The join cuts
    /// every other hap away, so the answer after the cut is the same, and
    /// the haps of the other sections of the span are not made. A query
    /// that fails or is refused only outside the part so answers. A graph
    /// whose joins make patterns at the query is asked over the part too,
    /// in a query that walks: each join then says when a pattern that it
    /// made reads the span, and a table says nothing. Any other graph is
    /// asked over the span, where it reads its values.
    fn query_restarted_onto(
        &self,
        thread: &QueryThread,
        shift: Fraction,
        state: &State,
        part: TimeSpan,
        out: &mut Vec<Hap>,
    ) {
        use purity::SpanRead;
        // What `self.late(shift)` answers, with no node for it.
        let over_part = |out: &mut Vec<Hap>| {
            self.query_moved_onto(thread, shift, "late", &state.set_span(part), out);
        };
        let over_span =
            |out: &mut Vec<Hap>| self.query_moved_onto(thread, shift, "late", state, out);
        let reads = match self.is_cacheable() {
            true => self.span_read(),
            false => SpanRead::Yes,
        };
        if reads == SpanRead::No {
            return over_part(out);
        }
        match self.node_kind() {
            // A stack answers its layers one after the other, so each layer
            // is asked as its own haps allow. A handle that binds a runtime
            // or is marked volatile is asked as it is.
            Node::Stack(layers) if !self.has_runtime_settings() && !self.is_volatile() => {
                let from = out.len();
                for layer in layers.iter().filter(|layer| !layer.is_rest()) {
                    stack_child_onto(thread, out, |out| {
                        layer.query_restarted_onto(thread, shift, state, part, out);
                    });
                    let held = out.len() - from;
                    if !thread.charge(held) {
                        out.truncate(from);
                        return;
                    }
                    thread.note_accumulated(held);
                }
            }
            Node::StackPure(cells) if !self.has_runtime_settings() && !self.is_volatile() => {
                let from = out.len();
                for cell in cells
                    .iter()
                    .filter(|cell| !matches!(cell.kind, Node::Silence))
                {
                    stack_child_onto(thread, out, |out| {
                        plain_pattern(cell, 0)
                            .query_restarted_onto(thread, shift, state, part, out);
                    });
                    let held = out.len() - from;
                    if !thread.charge(held) {
                        out.truncate(from);
                        return;
                    }
                    thread.note_accumulated(held);
                }
            }
            _ if reads == SpanRead::Made && part != state.span => {
                let from = out.len();
                if asked_over_part(|| over_part(out)).is_none() {
                    out.truncate(from);
                    over_span(out);
                }
            }
            _ => over_span(out),
        }
    }

    /// The haps of this pattern, `factor` times as fast: what a
    /// [`Node::Fast`] over it answers.
    fn query_faster_onto(
        &self,
        thread: &QueryThread,
        factor: Fraction,
        grid: Option<&TempoGrid>,
        operation: &'static str,
        state: &State,
        out: &mut Vec<Hap>,
    ) {
        if factor == Fraction::ONE {
            self.answer_onto(thread, state, table::Order::Graph, out);
            return;
        }
        let mul_pow2 = (factor.denom() == 1
            && factor.numer() > 0
            && (factor.numer() as u128).is_power_of_two())
        .then(|| (factor.numer() as u128).trailing_zeros());
        let div_pow2 = (factor.numer() == 1 && (factor.denom() as u128).is_power_of_two())
            .then(|| (factor.denom() as u128).trailing_zeros());
        let asked = if let Some(shift) = mul_pow2 {
            state.with_span(|span| {
                span.with_time(|t| {
                    t.checked_mul_pow2(shift)
                        .unwrap_or_else(|| fast_query_time(t, factor, operation))
                })
            })
        } else if let Some(shift) = div_pow2 {
            state.with_span(|span| {
                span.with_time(|t| {
                    t.checked_div_pow2(shift)
                        .unwrap_or_else(|| fast_query_time(t, factor, operation))
                })
            })
        } else {
            state.with_span(|span| span.with_time(|t| fast_query_time(t, factor, operation)))
        };
        let edges = (!thread.stop_requested() && factor != Fraction::ZERO)
            .then_some((state.span.begin, state.span.end));
        let from = out.len();
        self.answer_onto(thread, &asked, table::Order::Graph, out);
        if out.len() == from {
            return;
        }
        if let Some(shift) = mul_pow2 {
            Hap::map_times(&mut out[from..], asked.span, edges, |t| {
                t.checked_div_pow2(shift)
                    .unwrap_or_else(|| fast_hap_time(t, factor, operation))
            });
        } else if let Some(shift) = div_pow2 {
            Hap::map_times(&mut out[from..], asked.span, edges, |t| {
                t.checked_mul_pow2(shift)
                    .unwrap_or_else(|| fast_hap_time(t, factor, operation))
            });
        } else {
            Hap::map_times(&mut out[from..], asked.span, edges, |t| {
                grid.and_then(|grid| grid.time(t))
                    .unwrap_or_else(|| fast_hap_time(t, factor, operation))
            });
        }
    }

    /// The haps of this pattern, `by` later: what a [`Node::Shift`] over
    /// it answers.
    fn query_moved_onto(
        &self,
        thread: &QueryThread,
        by: Fraction,
        operation: &'static str,
        state: &State,
        out: &mut Vec<Hap>,
    ) {
        if by == Fraction::ZERO {
            self.answer_onto(thread, state, table::Order::Graph, out);
            return;
        }
        let whole_by = (by.denom() == 1).then(|| by.numer());
        let asked = if let Some(neg) = whole_by.and_then(i128::checked_neg) {
            state.with_span(|span| {
                span.with_time(|t| checked_time(t.checked_add_int(neg), operation))
            })
        } else {
            state.with_span(|span| span.with_time(|t| shift_query_time(t, by, operation)))
        };
        let edges = (!thread.stop_requested()).then_some((state.span.begin, state.span.end));
        let from = out.len();
        self.answer_onto(thread, &asked, table::Order::Graph, out);
        if out.len() == from {
            return;
        }
        if let Some(whole) = whole_by {
            Hap::map_times(&mut out[from..], asked.span, edges, |t| {
                checked_time(t.checked_add_int(whole), operation)
            });
        } else {
            Hap::map_times(&mut out[from..], asked.span, edges, |t| {
                shift_hap_time(t, by, operation)
            });
        }
    }

    /// Answer `self.focus_span(focus_span)` directly without constructing
    /// temporary `Late`/`Fast`/`Late` graph nodes on each query.
    fn query_focused_onto(
        &self,
        thread: &QueryThread,
        focus_span: TimeSpan,
        state: &State,
        out: &mut Vec<Hap>,
    ) {
        let (b, e) = (focus_span.begin, focus_span.end);
        if e == b {
            signal_query_error(|| "Division by zero".into());
            return;
        }
        let Some(rate) = e
            .checked_sub(b)
            .and_then(|span| Fraction::ONE.checked_div(span))
        else {
            refuse(QueryLimit::NativeFraction { operation: "focus" });
            return;
        };
        let Some(whole_cycles) = b.sam().checked_neg() else {
            refuse(QueryLimit::NativeFraction { operation: "focus" });
            return;
        };
        if rate <= Fraction::ZERO {
            let focused = self.focus_span(focus_span);
            focused.answer_onto(thread, state, table::Order::Graph, out);
            return;
        }
        if b == Fraction::ZERO && rate == Fraction::ONE && whole_cycles == Fraction::ZERO {
            self.answer_onto(thread, state, table::Order::Graph, out);
            return;
        }
        let inward = |mut t: Fraction| {
            if b != Fraction::ZERO {
                t = shift_query_time(t, b, "late");
            }
            if rate != Fraction::ONE {
                t = fast_query_time(t, rate, "fast");
            }
            if whole_cycles != Fraction::ZERO {
                t = shift_query_time(t, whole_cycles, "late");
            }
            t
        };
        let asked = state.with_span(|span| span.with_time(inward));
        if thread.stop_requested() {
            return;
        }
        let edges = Some((state.span.begin, state.span.end));
        let from = out.len();
        self.answer_onto(thread, &asked, table::Order::Graph, out);
        Hap::map_times(&mut out[from..], asked.span, edges, |mut t| {
            if whole_cycles != Fraction::ZERO {
                t = shift_hap_time(t, whole_cycles, "late");
            }
            if rate != Fraction::ONE {
                t = fast_hap_time(t, rate, "fast");
            }
            if b != Fraction::ZERO {
                t = shift_hap_time(t, b, "late");
            }
            t
        });
    }

    /// Fast in-place path for a chain of 1..=8 wave or single-step `StepWave`
    /// `AppWave`/`AppLeft` nodes (`.lpf(sine...).pan(rand)`): walks the wave
    /// spine, queries the base directly into `out`, caches chord samples, and
    /// applies all numeric patches in a single `OrderedMap` allocation.
    fn try_query_wave_chain_onto(
        &self,
        thread: &QueryThread,
        state: &State,
        out: &mut Vec<Hap>,
    ) -> bool {
        const MAX_WAVES: usize = 8;
        #[derive(Clone, Copy)]
        enum QuickWave<'a> {
            Wave(&'a Key, &'a table::Wave),
            Step(&'a fused::StepWave),
            StepParts(&'a Key, &'a table::Wave, u32),
        }
        impl<'a> QuickWave<'a> {
            #[inline]
            fn key(self) -> &'a Key {
                match self {
                    Self::Wave(key, _) | Self::StepParts(key, _, _) => key,
                    Self::Step(step) => &step.key,
                }
            }
            #[inline]
            fn is_step(self) -> bool {
                matches!(self, Self::Step(_) | Self::StepParts(..))
            }
            #[inline]
            fn sample(self, met: TimeSpan, part: TimeSpan, controls: &OrderedMap) -> Option<f64> {
                match self {
                    Self::Wave(_, wave) => wave.number(met, part, controls),
                    Self::Step(step) => step.number(part, controls),
                    Self::StepParts(_, wave, steps) => {
                        fused::StepWave::step_number(wave, steps, part, controls)
                    }
                }
            }
        }
        let mut waves: [Option<QuickWave<'_>>; MAX_WAVES] = [None; MAX_WAVES];
        let mut count = 0usize;
        let mut base = self;
        loop {
            if count > 0 && (base.has_runtime_settings() || base.is_volatile()) {
                break;
            }
            match base.node_kind() {
                Node::AppWave(app) => {
                    let items = &*app.waves;
                    if count + items.len() > MAX_WAVES {
                        if count == 0 {
                            return false;
                        }
                        break;
                    }
                    let mut ok = true;
                    let saved = count;
                    for item in items.iter().rev() {
                        let Some(key) = controls::builtin_key_ref_by_id(item.key_id) else {
                            ok = false;
                            break;
                        };
                        if item.steps == 0 {
                            waves[count] = Some(QuickWave::Wave(key, &item.wave));
                        } else if fused::StepWave::step_covers_span(item.steps, state.span) {
                            waves[count] = Some(QuickWave::StepParts(key, &item.wave, item.steps));
                        } else {
                            ok = false;
                            break;
                        }
                        count += 1;
                    }
                    if !ok {
                        count = saved;
                        if count == 0 {
                            return false;
                        }
                        break;
                    }
                    base = &app.inner;
                }
                Node::AppLeft(_) => {
                    let Some(side) = ValueSide::of(base.node_kind()) else {
                        break;
                    };
                    if !matches!(side.apply, Combine::Composed(compose::ComposeOp::Set))
                        || side.flow != LookupFlow::Set
                        || !side.values.tables_may_answer()
                    {
                        break;
                    }
                    let Some(ReadValues::Signal(signal)) = side.reading.of(side.values, state)
                    else {
                        break;
                    };
                    let found = if let Some((key, wave)) = signal.wave() {
                        Some(QuickWave::Wave(key, wave))
                    } else {
                        signal
                            .step_wave()
                            .filter(|step| step.covers_span(state.span))
                            .map(QuickWave::Step)
                    };
                    let Some(found) = found else {
                        break;
                    };
                    if count >= MAX_WAVES {
                        break;
                    }
                    waves[count] = Some(found);
                    count += 1;
                    base = side.functions;
                }
                _ => break,
            }
        }
        if count == 0 {
            return false;
        }
        let first = waves[0].expect("at least one wave");
        if base.is_volatile() {
            return false;
        }
        // Reverse so innermost wave applies first and outermost wins on same key.
        waves[..count].reverse();
        let has_step = waves[..count]
            .iter()
            .any(|w| w.is_some_and(QuickWave::is_step));
        let from = out.len();
        base.answer_onto(thread, state, table::Order::Graph, out);
        if thread.stop_requested() {
            out.truncate(from);
            return true;
        }
        let emitted = out.len() - from;
        if emitted == 0 {
            return true;
        }
        let clear_live_keys = |hap: &mut Hap, waves: &[Option<QuickWave<'_>>]| {
            if !hap.keys.is_empty() {
                let k = hap.keys.get();
                let mut clear_slot = k.slider_binding != 0;
                if !clear_slot {
                    for w in waves {
                        if let Some(s) = controls::live_slot(w.expect("wave").key().as_str())
                            && k.live_controls[s] != 0
                        {
                            clear_slot = true;
                            break;
                        }
                    }
                }
                if clear_slot {
                    let edit = hap.keys.edit();
                    for w in waves {
                        if let Some(s) = controls::live_slot(w.expect("wave").key().as_str()) {
                            edit.live_controls[s] = 0;
                        }
                    }
                    edit.slider_binding = 0;
                    hap.keys.trim();
                }
            }
        };
        // Fast single-hap path: skips initializing the 4-slot `SampleSlot`
        // stack cache on the common 1-hap window query.
        if emitted == 1 {
            let hap = &mut out[from];
            let met = hap.whole_or_part();
            let part = hap.part.intersection(&met);
            let ok = hap.pick_lookup().is_none()
                && hap.part.within_one_cycle()
                && hap.value.is_data(0)
                && matches!(hap.value, Value::Object(_))
                && if has_step {
                    hap.part.begin < hap.part.end && part == Some(hap.part)
                } else {
                    part.is_some()
                };
            let Some(part) = part.filter(|_| ok) else {
                out.truncate(from);
                return false;
            };
            let mut patches: [(&Key, f64); MAX_WAVES] = [(first.key(), 0.0); MAX_WAVES];
            for i in 0..count {
                let item = waves[i].expect("wave present");
                let Some(num) = item.sample(met, part, &state.controls) else {
                    out.truncate(from);
                    return false;
                };
                if item.is_step() {
                    fused::note(fused::Hit::StepOne);
                }
                patches[i] = (item.key(), num);
            }
            clear_live_keys(hap, &waves[..count]);
            if let Value::Object(controls) = &mut hap.value {
                if count == 1 {
                    controls.set_data_single_cached(patches[0].0, &Value::F64(patches[0].1), None);
                } else {
                    let mut dummy = value::SharedWaveCache::default();
                    controls.set_data_many_cached(&patches[..count], &mut dummy);
                }
            }
            hap.part = part;
            return true;
        }
        let can_apply_all = out[from..].iter().all(|hap| {
            hap.pick_lookup().is_none()
                && hap.part.within_one_cycle()
                && hap.value.is_data(0)
                && matches!(hap.value, Value::Object(_))
                && if has_step {
                    hap.part.begin < hap.part.end
                        && hap.part.intersection(&hap.whole_or_part()) == Some(hap.part)
                } else {
                    hap.part.intersection(&hap.whole_or_part()).is_some()
                }
        });
        if !can_apply_all {
            out.truncate(from);
            return false;
        }
        // Fast batched StepPatch path: when 1..=4 unique wave keys apply to
        // multiple haps sharing a base across 2+ distinct timestamps (even when
        // bases are interleaved across voices), pack up to `16 / n_unique`
        // timestamps per base into one `Arc<StepPatch>`.
        //
        // Uses compact `u8` index arrays on the stack (referencing haps in `out`)
        // to avoid zeroing multi-KB `TimeSpan` buffers per base.
        struct SampleSlot {
            met: TimeSpan,
            part: TimeSpan,
            nums: [f64; MAX_WAVES],
            cache: value::SharedWaveCache,
        }
        let apply_fallback_slice = |out: &mut [Hap], mask: Option<u64>| -> bool {
            let mut samples: [Option<SampleSlot>; 4] = [None, None, None, None];
            let mut sample_next = 0u8;
            let mut patches: [(&Key, f64); MAX_WAVES] = [(first.key(), 0.0); MAX_WAVES];
            let mut apply_one = |hap: &mut Hap| -> bool {
                let met = hap.whole_or_part();
                let part = hap
                    .part
                    .intersection(&met)
                    .expect("checked non-empty intersection");
                let slot_idx = if let Some(idx) = samples
                    .iter()
                    .position(|s| matches!(s, Some(slot) if slot.met == met && slot.part == part))
                {
                    idx
                } else {
                    let mut nums = [0.0f64; MAX_WAVES];
                    for i in 0..count {
                        let Some(num) =
                            waves[i]
                                .expect("wave present")
                                .sample(met, part, &state.controls)
                        else {
                            return false;
                        };
                        nums[i] = num;
                    }
                    let idx = if let Some(empty) = samples.iter().position(Option::is_none) {
                        empty
                    } else {
                        let victim = usize::from(sample_next & 3);
                        sample_next = sample_next.wrapping_add(1);
                        victim
                    };
                    samples[idx] = Some(SampleSlot {
                        met,
                        part,
                        nums,
                        cache: value::SharedWaveCache::default(),
                    });
                    idx
                };
                let slot = samples[slot_idx].as_mut().expect("sample slot populated");
                for i in 0..count {
                    let item = waves[i].expect("wave present");
                    if item.is_step() {
                        fused::note(fused::Hit::StepOne);
                    }
                    patches[i] = (item.key(), slot.nums[i]);
                }
                clear_live_keys(hap, &waves[..count]);
                if let Value::Object(controls) = &mut hap.value {
                    controls.set_data_many_cached(&patches[..count], &mut slot.cache);
                }
                hap.part = part;
                true
            };
            match mask {
                Some(mut rem) => {
                    while rem != 0 {
                        let j = rem.trailing_zeros() as usize;
                        rem &= rem - 1;
                        if !apply_one(&mut out[j]) {
                            return false;
                        }
                    }
                }
                None => {
                    for hap in out {
                        if !apply_one(hap) {
                            return false;
                        }
                    }
                }
            }
            true
        };
        if count <= value::StepPatch::MAX_KEYS {
            let mut unique_keys: [&Key; value::StepPatch::MAX_KEYS] =
                [first.key(); value::StepPatch::MAX_KEYS];
            let mut key_slot = [0u8; value::StepPatch::MAX_KEYS];
            let mut n_unique = 0usize;
            for i in 0..count {
                let key = waves[i].expect("wave present").key();
                if let Some(pos) = unique_keys[..n_unique].iter().position(|&k| k == key) {
                    key_slot[i] = pos as u8;
                } else {
                    unique_keys[n_unique] = key;
                    key_slot[i] = n_unique as u8;
                    n_unique += 1;
                }
            }
            let max_slots = value::StepPatch::MAX_VALUES / n_unique;
            if max_slots >= 2 {
                let total = out.len() - from;
                let mut chunk_base = 0usize;
                while chunk_base < total {
                    let chunk_len = (total - chunk_base).min(64);
                    let full_mask = if chunk_len == 64 {
                        u64::MAX
                    } else {
                        (1u64 << chunk_len) - 1
                    };
                    let mut done_mask = 0u64;
                    let mut skip_mask = 0u64;
                    // Reuse sampled wave values across distinct bases in the same chunk.
                    // Each entry stores the representative hap index in this chunk.
                    let mut shared_hap = [0u8; 16];
                    let mut shared_nums = [[0.0f64; value::StepPatch::MAX_KEYS]; 16];
                    let mut n_shared = 0usize;
                    let mut hap_idx = [0u8; 64];
                    let mut hap_rows = [0u32; 64];
                    let mut hap_slots = [0u8; 64];
                    while (done_mask | skip_mask) != full_mask {
                        let start = (!(done_mask | skip_mask)).trailing_zeros() as usize;
                        let Some((base, first_row, uniform)) =
                            (match &out[from + chunk_base + start].value {
                                Value::Object(m) => m.step_base(),
                                _ => None,
                            })
                        else {
                            skip_mask |= 1u64 << start;
                            continue;
                        };
                        let mut slot_hap = [0u8; 16];
                        let mut slot_nums = [[0.0f64; value::StepPatch::MAX_KEYS]; 16];
                        let mut n_slots = 0usize;
                        let mut m_len = 0usize;
                        let handled = done_mask | skip_mask;
                        for j in start..chunk_len {
                            if handled & (1u64 << j) != 0 {
                                continue;
                            }
                            let hap = &out[from + chunk_base + j];
                            let row = if j == start {
                                first_row
                            } else {
                                let Value::Object(m) = &hap.value else {
                                    continue;
                                };
                                let Some(r) = m.matches_step_base(&base) else {
                                    continue;
                                };
                                if !uniform && r != first_row {
                                    continue;
                                }
                                r
                            };
                            let met = hap.whole_or_part();
                            let part = hap
                                .part
                                .intersection(&met)
                                .expect("checked non-empty intersection");
                            let s_idx = if let Some(existing) =
                                slot_hap[..n_slots].iter().position(|&rep| {
                                    let rh = &out[from + chunk_base + usize::from(rep)];
                                    let rm = rh.whole_or_part();
                                    rm == met && rh.part.intersection(&rm) == Some(part)
                                }) {
                                existing
                            } else if n_slots < max_slots {
                                let vals = if let Some(cached) =
                                    shared_hap[..n_shared].iter().position(|&rep| {
                                        let rh = &out[from + chunk_base + usize::from(rep)];
                                        let rm = rh.whole_or_part();
                                        rm == met && rh.part.intersection(&rm) == Some(part)
                                    }) {
                                    shared_nums[cached]
                                } else {
                                    let mut vals = [0.0f64; value::StepPatch::MAX_KEYS];
                                    for i in 0..count {
                                        let Some(num) = waves[i].expect("wave present").sample(
                                            met,
                                            part,
                                            &state.controls,
                                        ) else {
                                            out.truncate(from);
                                            return false;
                                        };
                                        vals[usize::from(key_slot[i])] = num;
                                    }
                                    if n_shared < 16 {
                                        shared_hap[n_shared] = j as u8;
                                        shared_nums[n_shared] = vals;
                                        n_shared += 1;
                                    }
                                    vals
                                };
                                let idx = n_slots;
                                slot_hap[idx] = j as u8;
                                slot_nums[idx] = vals;
                                n_slots += 1;
                                idx
                            } else {
                                continue;
                            };
                            hap_idx[m_len] = j as u8;
                            hap_rows[m_len] = row;
                            hap_slots[m_len] = s_idx as u8;
                            m_len += 1;
                        }
                        if n_slots < 2 {
                            for &j in &hap_idx[..m_len] {
                                skip_mask |= 1u64 << j;
                            }
                            continue;
                        }
                        let Some(mut built) = value::StepPatch::new_for_row(
                            base,
                            first_row,
                            &unique_keys[..n_unique],
                            true,
                            n_slots * n_unique,
                        ) else {
                            for &j in &hap_idx[..m_len] {
                                skip_mask |= 1u64 << j;
                            }
                            continue;
                        };
                        for (s, nums) in slot_nums[..n_slots].iter().enumerate() {
                            built.set_slot(s, &nums[..n_unique]);
                        }
                        let sp = built.finish();
                        for k in 0..m_len {
                            let j = usize::from(hap_idx[k]);
                            let hap = &mut out[from + chunk_base + j];
                            for wave in &waves[..count] {
                                if wave.expect("wave present").is_step() {
                                    fused::note(fused::Hit::StepOne);
                                }
                            }
                            clear_live_keys(hap, &waves[..count]);
                            if let Value::Object(controls) = &mut hap.value {
                                controls.set_step_patch(
                                    &sp,
                                    hap_rows[k],
                                    usize::from(hap_slots[k]),
                                );
                            }
                            let met = hap.whole_or_part();
                            hap.part = hap
                                .part
                                .intersection(&met)
                                .expect("checked non-empty intersection");
                            done_mask |= 1u64 << j;
                        }
                    }
                    if skip_mask != 0
                        && !apply_fallback_slice(
                            &mut out[from + chunk_base..from + chunk_base + chunk_len],
                            Some(skip_mask),
                        )
                    {
                        out.truncate(from);
                        return false;
                    }
                    chunk_base += chunk_len;
                }
                return true;
            }
        }
        if !apply_fallback_slice(&mut out[from..], None) {
            out.truncate(from);
            return false;
        }
        true
    }

    /// Fast single-pass segmented wave application when `2..=4` waves share one
    /// multi-step rate (`shared_steps`) and every hap splits into `1..=8` steps.
    /// Kept out-of-line so its stack slots do not inflate callers that take the
    /// sequential single-step path.
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn try_query_app_wave_uniform_steps_onto(
        items: &[AppWaveItem],
        shared_steps: u32,
        has_continuous: bool,
        single_step_mask: u8,
        thread: &QueryThread,
        state: &State,
        from: usize,
        buf: &mut Vec<Hap>,
        out: &mut Vec<Hap>,
    ) -> Option<bool> {
        let mut keys: [Option<&Key>; 4] = [None; 4];
        for (i, item) in items.iter().enumerate() {
            keys[i] = Some(controls::builtin_key_ref_by_id(item.key_id)?);
        }
        let first_key = keys[0].expect("at least two items");
        let n_waves = items.len();
        let mut unique_keys: [&Key; value::StepPatch::MAX_KEYS] =
            [first_key; value::StepPatch::MAX_KEYS];
        let mut key_slot = [0u8; 4];
        let mut n_unique = 0usize;
        for w in 0..n_waves {
            let key = keys[w].expect("key");
            if let Some(pos) = unique_keys[..n_unique].iter().position(|&k| k == key) {
                key_slot[w] = pos as u8;
            } else {
                unique_keys[n_unique] = key;
                key_slot[w] = n_unique as u8;
                n_unique += 1;
            }
        }
        struct MultiStepSlot {
            met: TimeSpan,
            part: TimeSpan,
            count: u8,
            pieces: [TimeSpan; 8],
            nums: [[f64; 4]; 8],
            step_patches: StepPatchCache,
            step_next: u8,
            fallback_caches: Option<Box<[value::SharedWaveCache; 8]>>,
        }
        let mut slots: [Option<MultiStepSlot>; 4] = [None, None, None, None];
        let mut slot_next = 0u8;
        let mut patches: [(&Key, f64); 4] = [(first_key, 0.0); 4];
        for hf in buf.drain(..) {
            let met = if has_continuous {
                hf.whole_or_part()
            } else {
                hf.part
            };
            let slot_idx = if let Some(idx) = slots.iter().position(|s| {
                matches!(s, Some(slot) if slot.part == hf.part && (!has_continuous || slot.met == met))
            }) {
                idx
            } else {
                let grid = fused::StepWave::step_grid_over(shared_steps, hf.part)?;
                let count = grid.len();
                let mut pieces = [TimeSpan::new(Fraction::ZERO, Fraction::ZERO); 8];
                let mut nums = [[0.0f64; 4]; 8];
                let mut begin = grid.time(grid.steps().start)?;
                for (s_idx, k) in grid.steps().enumerate() {
                    if thread.stop_requested() {
                        out.truncate(from);
                        return Some(true);
                    }
                    let end = grid.time(k + 1)?;
                    let piece = grid.cut(k, TimeSpan::new(begin, end), hf.part);
                    pieces[s_idx] = piece;
                    for (w, item) in items.iter().enumerate() {
                        nums[s_idx][w] = if item.steps == 0 {
                            item.wave.number(met, piece, &state.controls)?
                        } else if single_step_mask & (1u8 << w) != 0 {
                            fused::StepWave::step_number(
                                &item.wave,
                                item.steps,
                                piece,
                                &state.controls,
                            )?
                        } else {
                            item.wave.step_value_at(begin, &state.controls)?
                        };
                    }
                    begin = end;
                }
                let idx = if let Some(empty) = slots.iter().position(Option::is_none) {
                    empty
                } else {
                    let victim = usize::from(slot_next & 3);
                    slot_next = slot_next.wrapping_add(1);
                    victim
                };
                slots[idx] = Some(MultiStepSlot {
                    met,
                    part: hf.part,
                    count: count as u8,
                    pieces,
                    nums,
                    step_patches: [None, None, None, None],
                    step_next: 0,
                    fallback_caches: None,
                });
                idx
            };
            let slot = slots[slot_idx].as_mut().expect("multi-step slot present");
            let count = usize::from(slot.count);
            out.reserve(count);
            let bulk_ok = thread.charge(out.len() - from + count);
            let mut template = hf;
            if !template.keys.is_empty() {
                let k = template.keys.get();
                let mut clear_slot = k.slider_binding != 0;
                if !clear_slot {
                    for key in keys[..n_waves].iter().flatten() {
                        if let Some(s) = controls::live_slot(key.as_str())
                            && k.live_controls[s] != 0
                        {
                            clear_slot = true;
                            break;
                        }
                    }
                }
                if clear_slot {
                    let edit = template.keys.edit();
                    for key in keys[..n_waves].iter().flatten() {
                        if let Some(s) = controls::live_slot(key.as_str()) {
                            edit.live_controls[s] = 0;
                        }
                    }
                    edit.slider_binding = 0;
                    template.keys.trim();
                }
            }
            // Single-allocation StepPatch fast path across all `count` steps.
            if count * n_unique <= value::StepPatch::MAX_VALUES
                && let Value::Object(map) = &template.value
                && let Some((base, row, uniform)) = map.step_base()
            {
                let fixed_row = (!uniform).then_some(row);
                let sp = if let Some((_, _, cached)) = slot
                    .step_patches
                    .iter()
                    .flatten()
                    .find(|(b, r, _)| b.same_source(&base) && *r == fixed_row)
                {
                    Some(cached.clone())
                } else if let Some(mut built) = value::StepPatch::new_for_row(
                    base.clone(),
                    row,
                    &unique_keys[..n_unique],
                    true,
                    count * n_unique,
                ) {
                    let mut vals = [0.0f64; value::StepPatch::MAX_KEYS];
                    for (s_idx, row_nums) in slot.nums.iter().enumerate().take(count) {
                        for w in 0..n_waves {
                            vals[usize::from(key_slot[w])] = row_nums[w];
                        }
                        built.set_slot(s_idx, &vals[..n_unique]);
                    }
                    let sp = built.finish();
                    let idx =
                        if let Some(empty) = slot.step_patches.iter().position(Option::is_none) {
                            empty
                        } else {
                            let v = usize::from(slot.step_next & 3);
                            slot.step_next = slot.step_next.wrapping_add(1);
                            v
                        };
                    slot.step_patches[idx] = Some((base, fixed_row, sp.clone()));
                    Some(sp)
                } else {
                    None
                };
                if let Some(sp) = sp {
                    let mut template = Some(template);
                    for (s_idx, &piece) in slot.pieces.iter().enumerate().take(count) {
                        let mut hap = if s_idx + 1 == count {
                            template.take().expect("last step moves template")
                        } else {
                            template.as_ref().expect("template present").clone()
                        };
                        if let Value::Object(controls) = &mut hap.value {
                            controls.set_step_patch(&sp, row, s_idx);
                        }
                        hap.part = piece;
                        if bulk_ok {
                            out.push(hap);
                        } else if thread.stop_requested() || !thread.push_budgeted(out, from, hap) {
                            out.truncate(from);
                            return Some(true);
                        }
                    }
                    if bulk_ok {
                        thread.note_accumulated(out.len() - from);
                    }
                    fused::note(fused::Hit::StepMany);
                    continue;
                }
            }
            let caches = slot.fallback_caches.get_or_insert_with(|| {
                Box::new(std::array::from_fn(|_| value::SharedWaveCache::default()))
            });
            let mut template = Some(template);
            for s_idx in 0..count {
                let mut hap = if s_idx + 1 == count {
                    template.take().expect("last step moves template")
                } else {
                    template.as_ref().expect("template present").clone()
                };
                for w in 0..n_waves {
                    patches[w] = (keys[w].expect("key"), slot.nums[s_idx][w]);
                }
                if let Value::Object(controls) = &mut hap.value {
                    controls.set_data_many_cached(&patches[..n_waves], &mut caches[s_idx]);
                }
                hap.part = slot.pieces[s_idx];
                if bulk_ok {
                    out.push(hap);
                } else if thread.stop_requested() || !thread.push_budgeted(out, from, hap) {
                    out.truncate(from);
                    return Some(true);
                }
            }
            if bulk_ok {
                thread.note_accumulated(out.len() - from);
            }
            fused::note(fused::Hit::StepMany);
        }
        Some(true)
    }

    /// Direct execution for [`Node::AppWave`] when one or more items are
    /// multi-step segmented waves (`steps > 0`) spanning multiple steps of
    /// `state.span`: avoids rebuilding `AppLeft` + `OnceLock` graphs and uses
    /// stack buffers for `count <= 8` window steps.
    fn try_query_app_wave_steps_onto(
        &self,
        app: &AppWaveNode,
        thread: &QueryThread,
        state: &State,
        out: &mut Vec<Hap>,
    ) -> bool {
        if app.inner.is_volatile() {
            return false;
        }
        let from = out.len();
        let items = &*app.waves;
        let ok = with_list(|buf| {
            app.inner
                .answer_onto(thread, state, table::Order::Graph, buf);
            if thread.stop_requested() {
                return Some(true);
            }
            if buf.is_empty() {
                return Some(true);
            }
            let apply = Combine::Composed(compose::ComposeOp::Set);
            let mut shared_steps = 0u32;
            let mut uniform_steps = items.len() <= 4;
            let mut has_continuous = false;
            let mut single_step_mask = 0u8;
            for (idx, item) in items.iter().enumerate() {
                if item.steps == 0 {
                    has_continuous = true;
                } else if fused::StepWave::step_covers_span(item.steps, state.span) {
                    single_step_mask |= 1u8 << idx;
                } else if shared_steps == 0 {
                    shared_steps = item.steps;
                } else if shared_steps != item.steps {
                    uniform_steps = false;
                    break;
                }
            }
            if uniform_steps
                && shared_steps > 0
                && items.len() >= 2
                && buf.iter().all(|hf| {
                    fused::StepWave::takes(hf)
                        && (!has_continuous || (hf.whole.is_some() && hf.part.within_one_cycle()))
                        && fused::StepWave::step_grid_over(shared_steps, hf.part)
                            .is_some_and(|g| (1..=8).contains(&g.len()))
                })
            {
                return Self::try_query_app_wave_uniform_steps_onto(
                    items,
                    shared_steps,
                    has_continuous,
                    single_step_mask,
                    thread,
                    state,
                    from,
                    buf,
                    out,
                );
            }
            for (item_idx, item) in items.iter().enumerate() {
                let key = controls::builtin_key_ref_by_id(item.key_id)?;
                let is_last = item_idx + 1 == items.len();
                if item.steps == 0 {
                    let mut wave_cache = value::SharedWaveCache::default();
                    let mut last_span: Option<(TimeSpan, TimeSpan, f64)> = None;
                    for hap in buf.iter_mut() {
                        if hap.pick_lookup().is_some()
                            || !hap.part.within_one_cycle()
                            || !hap.value.is_data(0)
                            || !matches!(hap.value, Value::Object(_))
                        {
                            return None;
                        }
                        let met = hap.whole_or_part();
                        let part = hap.part.intersection(&met)?;
                        let num = match last_span {
                            Some((lm, lp, ln)) if lm == met && lp == part => ln,
                            _ => {
                                let n = item.wave.number(met, part, &state.controls)?;
                                last_span = Some((met, part, n));
                                wave_cache = value::SharedWaveCache::default();
                                n
                            }
                        };
                        if !Meeting::set_number_in_place_cached(
                            hap,
                            key,
                            num,
                            part,
                            &apply,
                            LookupFlow::Set,
                            Some(&mut wave_cache),
                        ) {
                            return None;
                        }
                    }
                    if is_last {
                        out.append(buf);
                    }
                    continue;
                }
                // Segmented step wave (`item.steps > 0`).
                let valid = buf.iter().all(fused::StepWave::takes);
                if !valid {
                    return None;
                }
                let apply_into = |src: &mut Vec<Hap>,
                                  dst: &mut Vec<Hap>,
                                  dst_from: usize|
                 -> Option<bool> {
                    let mut one_cache = value::SharedWaveCache::default();
                    let mut one_last: Option<(TimeSpan, f64)> = None;
                    let mut small_cache: [Option<SmallStepSlot>; 4] = [None, None, None, None];
                    let mut small_next = 0u8;
                    let mut large_cache = StepWaveCacheSet::default();
                    for mut hf in src.drain(..) {
                        let cached_one = match one_last {
                            Some((lp, ln)) if lp == hf.part => Some(ln),
                            _ => fused::StepWave::step_number(
                                &item.wave,
                                item.steps,
                                hf.part,
                                &state.controls,
                            )
                            .inspect(|&n| {
                                one_last = Some((hf.part, n));
                                one_cache = value::SharedWaveCache::default();
                            }),
                        };
                        if let Some(num) = cached_one {
                            let part = hf.part;
                            if !Meeting::set_number_in_place_cached(
                                &mut hf,
                                key,
                                num,
                                part,
                                &apply,
                                LookupFlow::Set,
                                Some(&mut one_cache),
                            ) {
                                return None;
                            }
                            if thread.stop_requested() || !thread.push_budgeted(dst, dst_from, hf) {
                                return Some(false);
                            }
                            fused::note(fused::Hit::StepOne);
                            continue;
                        }
                        let grid = fused::StepWave::step_grid_over(item.steps, hf.part)?;
                        let count = grid.len();
                        if count == 0 {
                            continue;
                        }
                        if count == 1 {
                            let k = grid.steps().start;
                            let begin = grid.time(k)?;
                            let end = grid.time(k + 1)?;
                            let piece = grid.cut(k, TimeSpan::new(begin, end), hf.part);
                            let num = item.wave.step_value_at(begin, &state.controls)?;
                            if one_last.is_none_or(|(lp, _)| lp != hf.part) {
                                one_last = Some((hf.part, num));
                                one_cache = value::SharedWaveCache::default();
                            }
                            if !Meeting::set_number_in_place_cached(
                                &mut hf,
                                key,
                                num,
                                piece,
                                &apply,
                                LookupFlow::Set,
                                Some(&mut one_cache),
                            ) {
                                return None;
                            }
                            if thread.stop_requested() || !thread.push_budgeted(dst, dst_from, hf) {
                                return Some(false);
                            }
                            fused::note(fused::Hit::StepMany);
                            continue;
                        }
                        if count <= 8 {
                            if !meet_steps_small(
                                thread,
                                hf,
                                key,
                                &item.wave,
                                &grid,
                                &mut small_cache,
                                &mut small_next,
                                state,
                                dst_from,
                                dst,
                            )? {
                                return Some(false);
                            }
                            continue;
                        }
                        // Large multi-cycle step count (> 8): share one `Table` across steps.
                        let Value::Object(base_map) = &hf.value else {
                            return None;
                        };
                        let cached = if large_cache.find(hf.part).is_some() {
                            large_cache.find(hf.part).expect("checked present")
                        } else {
                            let mut pieces = large_cache.take_victim_pieces(count);
                            let mut begin = grid.time(grid.steps().start)?;
                            let mut vals = Vec::with_capacity(count);
                            for k in grid.steps() {
                                if thread.stop_requested() {
                                    return Some(false);
                                }
                                let end = grid.time(k + 1)?;
                                pieces.push(grid.cut(k, TimeSpan::new(begin, end), hf.part));
                                vals.push(Value::F64(
                                    item.wave.step_value_at(begin, &state.controls)?,
                                ));
                                begin = end;
                            }
                            large_cache.insert(StepWaveCache {
                                part: hf.part,
                                values: Arc::from(vals),
                                pieces,
                                table: None,
                            })
                        };
                        let shared_patched =
                            if let Some((base_source, base_row)) = base_map.dressed_base() {
                                if let Some((cached_base, cached_patched)) = &cached.table
                                    && cached_base.same_source(&base_source)
                                {
                                    Some((Arc::clone(cached_patched), base_row))
                                } else if let Some(patched) = value::Table::patched_keyed_table(
                                    &base_source,
                                    key.clone(),
                                    Arc::clone(&cached.values),
                                ) {
                                    let patched = Arc::new(patched);
                                    cached.table = Some((base_source, Arc::clone(&patched)));
                                    Some((patched, base_row))
                                } else {
                                    None
                                }
                            } else {
                                None
                            };
                        let fallback_table = if shared_patched.is_none() {
                            Some(Arc::new(value::Table::patched(
                                base_map,
                                key.clone(),
                                Arc::clone(&cached.values),
                            )?))
                        } else {
                            None
                        };
                        let mut template = hf.clone();
                        template.value = Value::Undefined;
                        if !template.keys.is_empty() {
                            let slot = controls::live_slot(key.as_str());
                            let k = template.keys.get();
                            if k.slider_binding != 0
                                || slot.is_some_and(|s| k.live_controls[s] != 0)
                            {
                                let edit = template.keys.edit();
                                if let Some(slot) = slot {
                                    edit.live_controls[slot] = 0;
                                }
                                edit.slider_binding = 0;
                                template.keys.trim();
                            }
                        }
                        let step_count = cached.pieces.len();
                        dst.reserve(step_count);
                        let bulk_ok = thread.charge(dst.len() - dst_from + step_count);
                        for (index, &piece) in cached.pieces.iter().enumerate() {
                            let map = match &shared_patched {
                                Some((table, base_row)) => table.map_packed(*base_row, index),
                                None => fallback_table
                                    .as_ref()
                                    .expect("fallback table")
                                    .map(index)?,
                            };
                            let mut hap = template.clone();
                            hap.value = Value::Object(map);
                            hap.part = piece;
                            if bulk_ok {
                                dst.push(hap);
                            } else if thread.stop_requested()
                                || !thread.push_budgeted(dst, dst_from, hap)
                            {
                                return Some(false);
                            }
                        }
                        if bulk_ok {
                            thread.note_accumulated(dst.len() - dst_from);
                        }
                        fused::note(fused::Hit::StepMany);
                    }
                    Some(true)
                };
                if is_last {
                    if !apply_into(buf, out, from)? {
                        out.truncate(from);
                        return Some(true);
                    }
                } else {
                    let cont = with_list(|next| {
                        let res = apply_into(buf, next, 0)?;
                        if res {
                            std::mem::swap(buf, next);
                        }
                        Some(res)
                    })?;
                    if !cont {
                        out.truncate(from);
                        return Some(true);
                    }
                }
            }
            Some(true)
        });
        match ok {
            Some(_) => true,
            None => {
                out.truncate(from);
                false
            }
        }
    }

    /// Fast in-place path for a degrade gate `AppLeft` node.
    fn try_query_gate_onto(
        &self,
        thread: &QueryThread,
        side: ValueSide<'_>,
        state: &State,
        out: &mut Vec<Hap>,
    ) -> bool {
        let keeps = side.flow == LookupFlow::Left
            && matches!(side.apply, Combine::With(combine) if combinators::keeps_left(combine));
        if !keeps || !side.values.tables_may_answer() || side.functions.is_volatile() {
            return false;
        }
        let Some(ReadValues::Signal(signal)) = side.reading.of(side.values, state) else {
            return false;
        };
        let Some(gate) = signal.gate() else {
            return false;
        };
        let from = out.len();
        side.functions
            .answer_onto(thread, state, table::Order::Graph, out);
        if thread.stop_requested() {
            out.truncate(from);
            return true;
        }
        if out.len() == from {
            return true;
        }
        if !out[from..]
            .iter()
            .all(|hap| hap.pick_lookup().is_none() && hap.part.within_one_cycle())
        {
            out.truncate(from);
            return false;
        }
        let mut bailed = false;
        retain_from(out, from, |hap| {
            if bailed {
                return false;
            }
            let met = hap.whole_or_part();
            let Some(part) = hap.part.intersection(&met) else {
                return false;
            };
            match gate.passes(met, part, &state.controls) {
                Some(true) => {
                    hap.part = part;
                    true
                }
                Some(false) => false,
                None => {
                    bailed = true;
                    false
                }
            }
        });
        if bailed {
            out.truncate(from);
            return false;
        }
        true
    }

    /// Whether the graph is one of discrete haps: it answers for a part of
    /// a span as for the span, cut and in the same order. One query over a
    /// span then holds the answer for each part of the span.
    fn is_discrete(&self) -> bool {
        self.is_cacheable() && self.span_read() == purity::SpanRead::No
    }

    /// The span to ask this side of an application over, for a hap of the
    /// other side that sounds over `whole` and is cut to `part`: what the
    /// two haps make is cut to `part` too. A graph of
    /// [discrete](Self::is_discrete) haps answers for the part as for the
    /// whole, cut, so it is asked over the part alone. Any other graph is
    /// asked over the whole, and so is each graph for a part of no width.
    fn asked_for(&self, whole: TimeSpan, part: TimeSpan) -> TimeSpan {
        match part != whole && part.begin < part.end && self.is_discrete() {
            true => part,
            false => whole,
        }
    }

    /// Append the haps of this [discrete](Self::is_discrete) pattern for each of
    /// `parts` directly onto `out`, from one query over all of them: the haps of
    /// each part in turn, each cut to its part, as a query for each part answers.
    /// The parts follow one another with no time between them.
    ///
    /// Returns `false` (leaving `out` untouched) for an answer with a hap of no
    /// width, which a part that begins at its time and a part that ends there
    /// read differently.
    fn asked_for_parts_onto(
        &self,
        thread: &QueryThread,
        parts: &[TimeSpan],
        state: &State,
        out: &mut Vec<Hap>,
    ) -> bool {
        thread_local! {
            static SPARE_MET: std::cell::RefCell<Vec<(usize, usize, bool)>> =
                const { std::cell::RefCell::new(Vec::new()) };
            static SPARE_SLOTS: std::cell::RefCell<Vec<Option<Hap>>> =
                const { std::cell::RefCell::new(Vec::new()) };
        }
        let (Some(first_part), Some(last_part)) = (parts.first(), parts.last()) else {
            return true;
        };
        let all = TimeSpan::new(first_part.begin, last_part.end);
        with_list(|haps| {
            let asked = state.set_span(all);
            self.answer_onto(thread, &asked, table::Order::Graph, haps);
            if haps.iter().any(|hap| hap.part.begin >= hap.part.end) {
                return false;
            }
            // Each hap at each part that it meets, and whether the part is the
            // last one, which takes the hap.
            let mut met = SPARE_MET
                .try_with(|cell| std::mem::take(&mut *cell.borrow_mut()))
                .unwrap_or_default();
            met.clear();
            met.reserve(haps.len());
            for (index, hap) in haps.iter().enumerate() {
                let first = parts.partition_point(|part| part.end <= hap.part.begin);
                let after = parts.partition_point(|part| part.begin < hap.part.end);
                met.extend((first..after).map(|part| (part, index, part + 1 == after)));
            }
            // Haps in the order of the parts, each in one part: the answer.
            if met.len() == haps.len() && met.is_sorted_by_key(|(part, ..)| *part) {
                met.clear();
                if met.capacity() > 0 && met.capacity() <= 128 {
                    let _ = SPARE_MET.try_with(|cell| *cell.borrow_mut() = met);
                }
                if out.capacity() == 0 {
                    *out = std::mem::take(haps);
                } else {
                    out.append(haps);
                }
                return true;
            }
            // The sort keeps the order of the answer in each part.
            met.sort_by_key(|(part, ..)| *part);
            let mut slots = SPARE_SLOTS
                .try_with(|cell| std::mem::take(&mut *cell.borrow_mut()))
                .unwrap_or_default();
            slots.clear();
            slots.extend(haps.drain(..).map(Some));
            let from = out.len();
            out.reserve(met.len());
            let mut ok = true;
            for &(part_idx, index, last) in &met {
                let hap = match last {
                    true => slots[index].take(),
                    false => slots[index].clone(),
                };
                let Some(mut hap) = hap else {
                    ok = false;
                    break;
                };
                let part = parts[part_idx];
                hap.part =
                    TimeSpan::new(hap.part.begin.max(part.begin), hap.part.end.min(part.end));
                out.push(hap);
            }
            if !ok {
                out.truncate(from);
            }
            slots.clear();
            if slots.capacity() > 0 && slots.capacity() <= 64 {
                let _ = SPARE_SLOTS.try_with(|cell| *cell.borrow_mut() = slots);
            }
            met.clear();
            if met.capacity() > 0 && met.capacity() <= 128 {
                let _ = SPARE_MET.try_with(|cell| *cell.borrow_mut() = met);
            }
            ok
        })
    }

    /// The haps that an `appRight` makes of this pattern of functions and
    /// the value haps `values`, from one query of this pattern, when its
    /// graph is one of [discrete](Self::is_discrete) haps. Each value hap
    /// asks the functions over its whole, so one query over all the wholes
    /// holds the answer for each.
    ///
    /// `None` for a few value haps, which their own queries serve, for
    /// wholes that leave most of the time between them free, for a graph
    /// that may read the span, and for a whole of no width.
    fn met_in_one_query_onto(
        &self,
        values: &[Hap],
        apply: &Combine,
        flow: LookupFlow,
        thread: &QueryThread,
        state: &State,
        out: &mut Vec<Hap>,
    ) -> bool {
        /// The fewest value haps that one query of the functions repays.
        const REPAYS_FROM: usize = 4;
        if values.len() < REPAYS_FROM || !self.is_discrete() {
            return false;
        }
        // This graph answers for the part of a hap as for its whole, cut.
        let asked = |hv: &Hap| hv.part;
        if values.iter().any(|hv| asked(hv).begin >= asked(hv).end) {
            return false;
        }
        let Some(min_begin) = values.iter().map(|hv| asked(hv).begin).min() else {
            return false;
        };
        let Some(max_end) = values.iter().map(|hv| asked(hv).end).max() else {
            return false;
        };
        let all = TimeSpan::new(min_begin, max_end);
        // The functions between the wholes are made for nothing.
        let width = |span: TimeSpan| span.end.to_f64() - span.begin.to_f64();
        let filled: f64 = values.iter().map(|hv| width(asked(hv))).sum();
        if filled * 2.0 < width(all) {
            return false;
        }
        with_list(|functions| {
            self.answer_onto(thread, &state.set_span(all), table::Order::Graph, functions);
            let n_funcs = functions.len();
            // Stack fast path for <= 32 function haps: zero heap allocations for
            // `by_begin`, `latest_end`, and `meeting`.
            let mut small_by_begin = [0usize; 32];
            let mut small_latest_end = [Fraction::ZERO; 32];
            let mut small_meeting = [0usize; 32];
            let heap_by_begin;
            let mut heap_latest_end = Vec::new();
            let mut heap_meeting = Vec::new();
            let (by_begin, latest_end): (&[usize], &[Fraction]) = if n_funcs <= 32 {
                for (i, slot) in small_by_begin[..n_funcs].iter_mut().enumerate() {
                    *slot = i;
                }
                small_by_begin[..n_funcs]
                    .sort_by(|a, b| functions[*a].part.begin.cmp(&functions[*b].part.begin));
                let mut latest = Fraction::ZERO;
                for (i, &index) in small_by_begin[..n_funcs].iter().enumerate() {
                    let end = functions[index].part.end;
                    latest = if i == 0 { end } else { end.max(latest) };
                    small_latest_end[i] = latest;
                }
                (&small_by_begin[..n_funcs], &small_latest_end[..n_funcs])
            } else {
                let mut indices: Vec<usize> = (0..n_funcs).collect();
                indices.sort_by(|a, b| functions[*a].part.begin.cmp(&functions[*b].part.begin));
                heap_by_begin = indices;
                heap_latest_end.reserve(n_funcs);
                for &index in &heap_by_begin {
                    let end = functions[index].part.end;
                    heap_latest_end.push(
                        heap_latest_end
                            .last()
                            .map_or(end, |latest| end.max(*latest)),
                    );
                }
                (&heap_by_begin, &heap_latest_end)
            };
            let from = out.len();
            out.reserve(values.len());
            for hv in values {
                let span = asked(hv);
                let first = latest_end.partition_point(|end| *end <= span.begin);
                let after =
                    by_begin.partition_point(|index| functions[*index].part.begin < span.end);
                let range = by_begin.get(first..after).unwrap_or_default();
                let met_indices: &[usize] = if range.len() <= 32 {
                    let mut m_len = 0usize;
                    for &index in range {
                        if functions[index].part.end > span.begin {
                            small_meeting[m_len] = index;
                            m_len += 1;
                        }
                    }
                    small_meeting[..m_len].sort_unstable();
                    &small_meeting[..m_len]
                } else {
                    heap_meeting.clear();
                    heap_meeting.extend(
                        range
                            .iter()
                            .copied()
                            .filter(|index| functions[*index].part.end > span.begin),
                    );
                    heap_meeting.sort_unstable();
                    &heap_meeting
                };
                for &index in met_indices {
                    let hf = &functions[index];
                    // The part of the function hap, as the query of the whole
                    // cuts it, then where the two haps meet.
                    let cut =
                        TimeSpan::new(hf.part.begin.max(span.begin), hf.part.end.min(span.end));
                    let Some(part) = cut.intersection(&hv.part) else {
                        continue;
                    };
                    let hap = applied_hap(hf, hv, hv.whole, part, apply, flow);
                    if thread.stop_requested() || !thread.push_budgeted(out, from, hap) {
                        out.truncate(from);
                        return;
                    }
                }
            }
        });
        true
    }

    /// The query. Everything else is built on this.
    pub fn query(&self, state: &State) -> Vec<Hap> {
        let mut haps = Vec::new();
        self.query_onto(state, &mut haps);
        haps
    }

    /// [`Self::query`], with the haps after those that `out` holds. A walk
    /// writes the haps of its nodes in one list: a node that changes the
    /// haps of the node under it changes them where they are, and a stack
    /// copies none.
    fn query_onto(&self, state: &State, out: &mut Vec<Hap>) {
        QUERY_THREAD.with(|thread| {
            self.answer_onto(thread, state, table::Order::Graph, out);
        });
    }

    /// [`Self::query`] with the haps in `order`, and whether a table gave
    /// the answer, so that it holds data only. The list has room for
    /// `room` haps: it is the list of the query of a caller, which takes
    /// most answers with no move.
    fn answer_with_room(
        &self,
        state: &State,
        order: table::Order,
        room: usize,
    ) -> (Vec<Hap>, bool) {
        let mut haps = Vec::with_capacity(room);
        let data_only =
            QUERY_THREAD.with(|thread| self.answer_onto(thread, state, order, &mut haps));
        (haps, data_only)
    }

    /// [`Self::answer_with_room`] after the haps of `out`, on the counters
    /// of the thread that asks, reached one time for the node.
    fn answer_onto(
        &self,
        thread: &QueryThread,
        state: &State,
        order: table::Order,
        out: &mut Vec<Hap>,
    ) -> bool {
        let outermost = thread.depth.get() == 0;
        if self.is_volatile() {
            note_uncacheable();
        }
        match self.runtime_settings() {
            Some(settings) => {
                if thread.current_runtime.get() == settings.ptr_id() {
                    let owner = outermost.then_some(settings);
                    self.query_with_bound_runtime(thread, state, owner, order, out)
                } else {
                    // The child binds a runtime of its own, whose snapshot
                    // can move without the enclosing one changing.
                    note_uncacheable();
                    let _scope = settings.bind();
                    self.query_with_bound_runtime(thread, state, Some(settings), order, out)
                }
            }
            None => {
                let metadata_owner = if outermost && thread.current_runtime.get() != 0 {
                    settings::RuntimeSettings::current_requested()
                } else {
                    None
                };
                self.query_with_bound_runtime(thread, state, metadata_owner.as_ref(), order, out)
            }
        }
    }

    /// The haps of this node after those of `out`, and whether they hold
    /// data only. A query that stops writes none.
    fn query_with_bound_runtime(
        &self,
        thread: &QueryThread,
        state: &State,
        metadata_owner: Option<&settings::RuntimeSettings>,
        order: table::Order,
        out: &mut Vec<Hap>,
    ) -> bool {
        // Restores the counter however this frame leaves - including on a
        // panic, so a caught query error cannot leave the thread's depth
        // permanently charged and refuse every later query.
        struct DepthGuard<'a>(&'a QueryThread);
        impl Drop for DepthGuard<'_> {
            fn drop(&mut self) {
                let next = self.0.depth.get().saturating_sub(1);
                self.0.depth.set(next);
                if next == 0 {
                    context_keys::end_query_merge_cache();
                }
            }
        }
        let depth = thread.depth.get().saturating_add(1);
        thread.depth.set(depth);
        if depth == 1 {
            context_keys::begin_query_merge_cache();
        }
        let _guard = DepthGuard(thread);
        if depth > MAX_PATTERN_DEPTH {
            refuse(QueryLimit::GraphDepth {
                depth,
                limit: MAX_PATTERN_DEPTH,
            });
            return true;
        }
        if thread.stop_requested() {
            return true;
        }
        let from = out.len();
        self.query_node_onto(thread, state, out);
        if order == table::Order::Sorted {
            let haps = match from {
                0 => std::mem::take(out),
                _ => out.split_off(from),
            };
            hand_over(sort_haps_for_query(haps), out);
        }
        let data_only = matches!(
            self.node_kind(),
            Node::Sequence(layers) if sequence::holds_data(layers)
        );
        if let Some(settings) = metadata_owner.filter(|_| !data_only) {
            for at in from..out.len() {
                if !out[at].value.attach_runtime_settings(settings, 0) {
                    out.truncate(from);
                    return true;
                }
            }
        }
        // Already built by the time we see it - recorded so a producer that
        // grows without an incremental check is visible as the vector it made.
        let made = out.len() - from;
        thread.note_accumulated(made);
        if !thread.charge(made) {
            out.truncate(from);
            return true;
        }
        data_only
    }

    /// The haps of this `Pure` or `PurePickLookup` node of `value`: one a
    /// cycle, each aligned to its whole cycle.
    fn query_value_onto(
        &self,
        thread: &QueryThread,
        value: &Value,
        state: &State,
        out: &mut Vec<Hap>,
    ) {
        let single_cycle = state.span.begin <= state.span.end && state.span.within_one_cycle();
        if single_cycle {
            if !thread.charge(1) {
                return;
            }
            note_materialised(1);
            if thread.stop_requested() {
                return;
            }
            let lookup = match self.node_kind() {
                Node::PurePickLookup(_) => {
                    let Some(lookup) = self.as_pick_lookup().map(Arc::new) else {
                        return;
                    };
                    Some(lookup)
                }
                _ => None,
            };
            let hap = Hap::new(
                Some(whole_cycle(state.span.begin)),
                state.span,
                value.clone(),
            );
            match lookup {
                Some(lookup) => out.push(hap.with_pick_lookup(lookup)),
                None => out.push(hap),
            }
            return;
        }
        // Charged before the haps are built. This is the one node that
        // can produce a large vector in a single pass (one hap per
        // cycle, up to `MAX_QUERY_SPAN_CYCLES` of them), so charging it
        // afterwards, like the generic wrapper in `query` does, would
        // let a million haps exist first. Everything else grows by
        // combining smaller results, which the wrapper catches.
        let count = if state.span.begin == state.span.end {
            1usize
        } else {
            let Some(c) = state
                .span
                .cycle_count()
                .filter(|c| *c <= MAX_QUERY_SPAN_CYCLES)
            else {
                refuse(QueryLimit::QuerySpan {
                    cycles: MAX_QUERY_SPAN_CYCLES,
                });
                return;
            };
            usize::try_from(c).unwrap_or_default()
        };
        if !thread.charge(count) {
            return;
        }
        note_materialised(count);
        let lookup = match self.node_kind() {
            Node::PurePickLookup(_) => {
                let Some(lookup) = self.as_pick_lookup().map(Arc::new) else {
                    return;
                };
                Some(lookup)
            }
            _ => None,
        };
        let from = out.len();
        out.reserve(count);
        let mut stopped = false;
        state.span.for_each_cycle(|subspan| {
            if stopped {
                return;
            }
            if thread.stop_requested() {
                stopped = true;
                return;
            }
            let hap = Hap::new(Some(whole_cycle(subspan.begin)), subspan, value.clone());
            match &lookup {
                Some(lookup) => out.push(hap.with_pick_lookup(Arc::clone(lookup))),
                None => out.push(hap),
            }
        });
        if stopped {
            out.truncate(from);
        }
    }

    /// The haps of this node after those that `out` holds. A node of a
    /// kind that a walk meets often writes them there. A node of any other
    /// kind makes its list, and the list moves there.
    fn query_node_onto(&self, thread: &QueryThread, state: &State, out: &mut Vec<Hap>) {
        match self.node_kind() {
            Node::Silence => {}

            Node::Pure(value) => {
                self.query_value_onto(thread, value, state, out);
            }
            Node::PureAt(boxed) => {
                let from = out.len();
                self.query_value_onto(thread, &boxed.0, state, out);
                for hap in &mut out[from..] {
                    hap.context.append(&boxed.1);
                }
            }
            Node::PurePickLookup(boxed) => {
                self.query_value_onto(thread, &boxed.0, state, out);
            }

            Node::Sequence(layers) => sequence::query_sequence_onto(thread, layers, state, out),

            Node::Fast(node) => {
                node.0
                    .query_faster_onto(thread, node.1, None, node.2, state, out);
            }

            Node::FastSmall(cell, flags, numer, denom, is_slow) => {
                let factor = Fraction::new(i128::from(*numer), i128::from(*denom));
                let op = if *is_slow { "slow" } else { "fast" };
                plain_pattern(cell, *flags).query_faster_onto(thread, factor, None, op, state, out);
            }

            Node::Shift(node) => {
                node.0.query_moved_onto(thread, node.1, node.2, state, out);
            }

            Node::ShiftSmall(cell, flags, numer, denom, is_early) => {
                let offset = Fraction::new(i128::from(*numer), i128::from(*denom));
                let op = if *is_early { "early" } else { "late" };
                plain_pattern(cell, *flags).query_moved_onto(thread, offset, op, state, out);
            }

            Node::Cpm(boxed) => {
                let (pat, rate) = (&boxed.0, &boxed.1);
                let scheduler_cps = state
                    .controls
                    .get("_cps")
                    .and_then(Value::as_f64)
                    .filter(|cps| cps.is_finite() && *cps > 0.0)
                    .unwrap_or(0.5);
                // What `pat.fast(speed)` answers, with no node for it.
                if let Some((speed, grid)) = rate.speed_at(scheduler_cps)
                    && speed.numer() != 0
                {
                    pat.query_faster_onto(thread, speed, grid, "fast", state, out);
                }
            }

            Node::FilterValues(boxed) => {
                let (pat, predicate) = (&boxed.0, &boxed.1);
                let from = out.len();
                pat.answer_onto(thread, state, table::Order::Graph, out);
                retain_from(out, from, |hap| predicate(&hap.value));
            }

            Node::FilterHaps(boxed) => {
                let (pat, predicate) = (&boxed.0, &boxed.1);
                let from = out.len();
                pat.answer_onto(thread, state, table::Order::Graph, out);
                retain_from(out, from, |hap| predicate(hap));
            }

            Node::Degrade(boxed) => {
                let (pattern, amount, kind) = (&boxed.0, boxed.1, boxed.2);
                let random_seed = state
                    .controls
                    .get("randSeed")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                let from = out.len();
                pattern.answer_onto(thread, state, table::Order::Graph, out);
                match kind {
                    DegradeKind::Seeded(seed) => {
                        retain_from(out, from, |hap| {
                            let time =
                                hap.whole_or_part().begin.to_f64() + 0.0003 * f64::from(seed);
                            rng::rand_at_time(time, random_seed) > amount
                        });
                    }
                    DegradeKind::Rand | DegradeKind::RandInverted => {
                        let inverted = kind == DegradeKind::RandInverted;
                        retain_from(out, from, |hap| {
                            let met = hap.whole_or_part();
                            let Some(part) = hap.part.intersection(&met) else {
                                return false;
                            };
                            let r = rng::rand_at_time(met.begin.to_f64(), random_seed);
                            let coin = if inverted { 1.0 - r } else { r };
                            if coin > amount {
                                hap.part = part;
                                true
                            } else {
                                false
                            }
                        });
                    }
                }
            }

            Node::Stack(pats) => query_stack_onto(thread, pats, state, out, || {}),
            Node::StackPure(cells) => query_stack_pure_onto(thread, cells, state, out),

            Node::AddContext(boxed) => {
                let (pat, locs) = (&boxed.0, &boxed.1);
                let from = out.len();
                // A located value is two nodes, and one visit answers for
                // both. Any other handle is queried: it may bind a runtime.
                match pat.node_kind() {
                    Node::Pure(value) if !pat.has_runtime_settings() => {
                        pat.query_value_onto(thread, value, state, out);
                    }
                    _ => {
                        pat.answer_onto(thread, state, table::Order::Graph, out);
                    }
                }
                for hap in &mut out[from..] {
                    hap.context.extend_from_slice(locs);
                }
            }

            Node::AppWave(app) => {
                if self.try_query_wave_chain_onto(thread, state, out) {
                    return;
                }
                if self.try_query_app_wave_steps_onto(app, thread, state, out) {
                    return;
                }
                // Rare fallback for non-object haps, multi-cycle parts, >48-bit
                // rationals, or cut_mask boundaries: rebuild exact wave chain.
                let mut current = app.inner.clone();
                for item in app.waves.iter() {
                    let values =
                        item.wave
                            .to_control_pattern(item.key_id, item.math_inside, item.steps);
                    current = Pattern::of(Node::AppLeft(Box::new(AppLeftNode(
                        current,
                        values,
                        ValueReading::default(),
                        Combine::Composed(compose::ComposeOp::Set),
                        LookupFlow::Set,
                    ))));
                }
                current.answer_onto(thread, state, table::Order::Graph, out);
            }

            // Function haps drive: values are queried over each function
            // hap's wholeOrPart, and the function side's whole wins.
            node @ Node::AppLeft(..) => {
                let side = ValueSide::of(node).expect("an application");
                // Fast in-place path for a chain of 1..=8 wave controls
                // (`.lpf(sine...).pan(rand)`): queries the base directly into
                // `out`, caches chord samples, and applies all patches in one
                // `OrderedMap` allocation with no `with_list` scratch copy.
                if self.try_query_wave_chain_onto(thread, state, out) {
                    return;
                }
                // Fast in-place path for a degrade gate: filters `out` directly.
                if self.try_query_gate_onto(thread, side, state, out) {
                    return;
                }
                with_list(|functions| {
                    side.functions
                        .answer_onto(thread, state, table::Order::Graph, functions);
                    if functions.is_empty() {
                        return;
                    }
                    // The control of a segment of a wave takes each number
                    // where the function hap is (cached once in `ValueReading`).
                    let steps = if matches!(side.apply, Combine::Composed(compose::ComposeOp::Set))
                        && side.flow == LookupFlow::Set
                        && side.values.tables_may_answer()
                        && let Some(ReadValues::Signal(signal)) =
                            side.reading.of(side.values, state)
                    {
                        signal
                            .step_wave()
                            .filter(|_| functions.iter().all(fused::StepWave::takes))
                    } else {
                        None
                    };
                    if steps.is_none() && side.met_in_one_query(functions, state, out) {
                        return;
                    }
                    let from = out.len();
                    let mut meetings = Meetings {
                        steps,
                        ..Meetings::default()
                    };
                    out.reserve(functions.len());
                    for hf in functions.drain(..) {
                        if !side.meet_on(thread, hf, &mut meetings, state, from, out) {
                            out.truncate(from);
                            return;
                        }
                    }
                });
            }

            // A signal under steps of its own, read in equal steps of a
            // cycle as `segment` gives, answers in one walk of the steps.
            Node::AppRight(node) => {
                if fused::app_right_onto(node, state, out) {
                    return;
                }
                let AppNode(pat_func, pat_val, apply, flow) = &**node;
                with_list(|values| {
                    pat_val.answer_onto(thread, state, table::Order::Graph, values);
                    if values.is_empty() {
                        return;
                    }
                    if pat_func.met_in_one_query_onto(values, apply, *flow, thread, state, out) {
                        return;
                    }
                    let mut sampler =
                        table::signal_of(pat_func).map(|signal| (signal, state.clone()));
                    let from = out.len();
                    out.reserve(values.len());
                    with_list(|funcs| {
                        for hv in values.drain(..) {
                            let asked = pat_func.asked_for(hv.whole_or_part(), hv.part);
                            let sampled = sampler.as_mut().is_some_and(|(signal, probe)| {
                                signal.answer(asked, hv.part, probe, funcs).is_some()
                            });
                            if !sampled {
                                funcs.clear();
                                pat_func.answer_onto(
                                    thread,
                                    &state.set_span(asked),
                                    table::Order::Graph,
                                    funcs,
                                );
                            }
                            for hf in funcs.iter() {
                                if let Some(part) = hf.part.intersection(&hv.part) {
                                    let hap = applied_hap(hf, &hv, hv.whole, part, apply, *flow);
                                    if thread.stop_requested()
                                        || !thread.push_budgeted(out, from, hap)
                                    {
                                        out.truncate(from);
                                        return;
                                    }
                                }
                            }
                        }
                    });
                });
            }

            Node::Pick(pick) => query_pick(
                thread,
                &pick.selector,
                LookupRef::Compact(&pick.lookup),
                pick.index_mode,
                pick.mode,
                state,
                out,
                &|| {},
            ),

            Node::Constants(node) => query_run(thread, &node.0, &node.1, state, out),

            Node::WithQueryTime(boxed) => {
                let s = state.with_span(|sp| sp.with_time(|t| (boxed.1)(t)));
                boxed.0.answer_onto(thread, &s, table::Order::Graph, out);
            }

            Node::WithHapTime(boxed) => {
                let from = out.len();
                boxed.0.answer_onto(thread, state, table::Order::Graph, out);
                for hap in &mut out[from..] {
                    hap.update_span(|sp| sp.with_time(|t| (boxed.1)(t)));
                }
            }

            Node::WithQuerySpan(boxed) => {
                boxed.0.answer_onto(
                    thread,
                    &state.with_span(|sp| (boxed.1)(sp)),
                    table::Order::Graph,
                    out,
                );
            }

            Node::WithHapSpan(boxed) => {
                let from = out.len();
                boxed.0.answer_onto(thread, state, table::Order::Graph, out);
                for hap in &mut out[from..] {
                    hap.update_span(|sp| (boxed.1)(sp));
                }
            }

            Node::Rev(pat) => {
                let cycle = state.span.begin.sam();
                let next_cycle = state.span.begin.next_sam();
                let reflect = |sp: &TimeSpan| {
                    let b = cycle.add(next_cycle.sub(sp.begin));
                    let e = cycle.add(next_cycle.sub(sp.end));
                    TimeSpan::new(e, b)
                };
                let from = out.len();
                pat.answer_onto(
                    thread,
                    &state.set_span(reflect(&state.span)),
                    table::Order::Graph,
                    out,
                );
                for hap in &mut out[from..] {
                    hap.update_span(reflect);
                }
            }

            Node::FmapNative(boxed) => {
                let from = out.len();
                boxed.0.answer_onto(thread, state, table::Order::Graph, out);
                for hap in &mut out[from..] {
                    hap.map_value_in_place(|value| (boxed.1)(value));
                }
            }

            Node::ExpandHapsNative(boxed) => {
                let (pat, f) = (&boxed.0, &boxed.1);
                let from = out.len();
                with_list(|haps| {
                    pat.answer_onto(thread, state, table::Order::Graph, haps);
                    for h in haps.drain(..) {
                        f(&h, out);
                        let count = out.len() - from;
                        if thread.stop_requested() || !thread.charge(count) {
                            out.truncate(from);
                            return;
                        }
                        thread.note_accumulated(count);
                    }
                });
            }

            Node::SlowCatPrime(pats) => {
                if pats.is_empty() {
                    return;
                }
                let n = pats.len() as i128;
                let index = state.span.begin.floor().numer().rem_euclid(n);
                pats[index as usize].answer_onto(thread, state, table::Order::Graph, out);
            }

            Node::SlowCatPrimePure(cells) => {
                if cells.is_empty() {
                    return;
                }
                let n = cells.len() as i128;
                let index = state.span.begin.floor().numer().rem_euclid(n);
                let cell = &cells[index as usize];
                if !matches!(cell.kind, Node::Silence) {
                    plain_pattern(cell, 0).answer_onto(thread, state, table::Order::Graph, out);
                }
            }

            Node::Signal(signal) => {
                out.push(Hap::new(None, state.span, signal(state)));
            }

            Node::ControlMap(cell, flags, id, unnamed_only) => {
                let from = out.len();
                plain_pattern(cell, *flags).answer_onto(thread, state, table::Order::Graph, out);
                if let Some(spec) = controls::builtin_spec_by_id(*id) {
                    map_from(out, from, |hap| {
                        controls::apply_control_hap(spec, *unnamed_only, hap)
                    });
                }
            }

            Node::ControlMapBoxed(boxed) => {
                let (pat, id, unnamed_only) = (&boxed.0, boxed.1, boxed.2);
                let from = out.len();
                pat.answer_onto(thread, state, table::Order::Graph, out);
                if let Some(spec) = controls::builtin_spec_by_id(id) {
                    map_from(out, from, |hap| {
                        controls::apply_control_hap(spec, unnamed_only, hap)
                    });
                }
            }

            // A map of each hap, where the hap is. A hap that the map drops
            // leaves the list.
            Node::MapHapsNative(boxed) => {
                let (pat, map) = (&boxed.0, &boxed.1);
                let from = out.len();
                pat.answer_onto(thread, state, table::Order::Graph, out);
                let mut kept = from;
                for at in from..out.len() {
                    if let Some(mapped) = map(&out[at]) {
                        out[kept] = mapped;
                        kept += 1;
                    }
                }
                out.truncate(kept);
            }

            Node::MapHapsOwned(boxed) => {
                let (pat, map) = (&boxed.0, &boxed.1);
                let from = out.len();
                pat.answer_onto(thread, state, table::Order::Graph, out);
                map_from(out, from, |hap| map(hap));
            }

            Node::UiVisual(node) => {
                let from = out.len();
                node.inner
                    .answer_onto(thread, state, table::Order::Graph, out);
                let mut local = [None, None];
                let mut local_next = 0u8;
                for hap in &mut out[from..] {
                    hap.keys.add_ui_visual_slot_cached(
                        node.slot,
                        &node.cache,
                        &mut local,
                        &mut local_next,
                    );
                }
            }

            // One constituent per cycle. `offset` keeps constituent cycles
            // contiguous: the fourth cycle of a three-pattern slowcat is the
            // SECOND cycle of the first pattern, not the fourth. The wrapping
            // `split_queries` keeps a multi-cycle query from being served by
            // a single constituent. A shifted time outside the native range
            // refuses as `NativeFraction { operation: "slowcat" }`.
            Node::SlowCat(pats) => slowcat_cycle_onto(thread, pats, state, out),
            Node::SlowCatPure(cells) => slowcat_cycle_pure_onto(thread, cells, state, out),

            Node::ChooseCycles(boxed) => {
                let (patterns, seed) = (&boxed.0, &boxed.1);
                if patterns.is_empty() {
                    return;
                }
                let random_seed = state
                    .controls
                    .get("randSeed")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                let small;
                let heap;
                let pieces: &[TimeSpan] = if let Some((buf, len)) = state.span.span_cycles_small() {
                    small = buf;
                    &small[..len]
                } else {
                    heap = state.span.span_cycles();
                    &heap
                };
                let from = out.len();
                for &span in pieces {
                    // `segment(1)` is `struct(pure(true))`, whose appRight
                    // queries `rand` over the pure hap's whole (a full
                    // cycle), so the signal is sampled at the cycle start.
                    // A sample at the sub-span's begin would make the choice
                    // vary within a cycle: `vowel("[a|e|i|o|u]")` would pick
                    // a different vowel per event instead of per cycle.
                    let sample_time = span.begin.sam().to_f64() + 0.0003 * f64::from(*seed);
                    let random = rng::rand_at_time(sample_time, random_seed);
                    let index =
                        ((random * patterns.len() as f64).floor() as usize).min(patterns.len() - 1);
                    let cycle = state.set_span(span);
                    patterns[index].answer_onto(thread, &cycle, table::Order::Graph, out);
                    let count = out.len() - from;
                    if thread.stop_requested() || !thread.charge(count) {
                        out.truncate(from);
                        return;
                    }
                    thread.note_accumulated(count);
                }
            }

            // A span in one cycle is one piece, which the pattern answers
            // where the haps of the walk are; multi-cycle spans use a stack
            // split when covering at most 8 cycles and write straight onto `out`.
            Node::SplitQueries(..) | Node::SplitQueriesBoxed(..) => {
                let plain_owned;
                let pat = match self.node_kind() {
                    Node::SplitQueries(cell, flags) => {
                        plain_owned = plain_pattern(cell, *flags);
                        &plain_owned
                    }
                    Node::SplitQueriesBoxed(pat) => pat,
                    _ => unreachable!(),
                };
                let concatenated = match pat.node_kind() {
                    Node::SlowCat(pats) if !pat.has_runtime_settings() => Some(Ok(pats.as_ref())),
                    Node::SlowCatPure(cells) if !pat.has_runtime_settings() => {
                        Some(Err(cells.as_ref()))
                    }
                    _ => None,
                };
                if state.span.within_one_cycle() {
                    match concatenated {
                        Some(Ok(pats)) => slowcat_cycle_onto(thread, pats, state, out),
                        Some(Err(cells)) => slowcat_cycle_pure_onto(thread, cells, state, out),
                        None => {
                            pat.answer_onto(thread, state, table::Order::Graph, out);
                        }
                    }
                    return;
                }
                let small;
                let heap;
                let pieces: &[TimeSpan] = if let Some((buf, len)) = state.span.span_cycles_small() {
                    small = buf;
                    &small[..len]
                } else {
                    heap = state.span.span_cycles();
                    &heap
                };
                // A graph of discrete haps answers for all the cycles in one query.
                if pieces.len() > 1
                    && pat.is_discrete()
                    && pat.asked_for_parts_onto(thread, pieces, state, out)
                {
                    return;
                }
                let from = out.len();
                for &sub in pieces {
                    let cycle = state.set_span(sub);
                    match concatenated {
                        Some(Ok(pats)) => slowcat_cycle_onto(thread, pats, &cycle, out),
                        Some(Err(cells)) => slowcat_cycle_pure_onto(thread, cells, &cycle, out),
                        None => {
                            pat.answer_onto(thread, &cycle, table::Order::Graph, out);
                        }
                    }
                    let count = out.len() - from;
                    if thread.stop_requested() || !thread.charge(count) {
                        out.truncate(from);
                        return;
                    }
                    thread.note_accumulated(count);
                }
            }

            _ => hand_over(self.query_node(state), out),
        }
    }

    /// The haps of this node, of a kind that makes its list.
    fn query_node(&self, state: &State) -> Vec<Hap> {
        match self.node_kind() {
            // A kind that writes its haps after those of a list.
            Node::Silence
            | Node::Pure(_)
            | Node::PureAt(_)
            | Node::PurePickLookup(..)
            | Node::Sequence(_)
            | Node::Fast(..)
            | Node::FastSmall(..)
            | Node::Shift(..)
            | Node::ShiftSmall(..)
            | Node::Cpm(..)
            | Node::FilterValues(..)
            | Node::FilterHaps(..)
            | Node::Degrade(..)
            | Node::Stack(_)
            | Node::StackPure(_)
            | Node::AddContext(..)
            | Node::AppLeft(..)
            | Node::AppWave(..)
            | Node::AppRight(..)
            | Node::Pick(..)
            | Node::SlowCat(_)
            | Node::SlowCatPure(_)
            | Node::SlowCatPrime(_)
            | Node::SlowCatPrimePure(_)
            | Node::WithQueryTime(..)
            | Node::WithHapTime(..)
            | Node::WithQuerySpan(..)
            | Node::WithHapSpan(..)
            | Node::Rev(..)
            | Node::FmapNative(..)
            | Node::ExpandHapsNative(..)
            | Node::Signal(..)
            | Node::ControlMap(..)
            | Node::ControlMapBoxed(..)
            | Node::MapHapsNative(..)
            | Node::MapHapsOwned(..)
            | Node::UiVisual(..)
            | Node::ChooseCycles(..)
            | Node::SplitQueries(..)
            | Node::SplitQueriesBoxed(..)
            | Node::Constants(..) => {
                let mut haps = Vec::new();
                QUERY_THREAD.with(|thread| self.query_node_onto(thread, state, &mut haps));
                haps
            }

            Node::QueryError(message) => {
                signal_query_error(|| message.to_string());
                Vec::new()
            }

            Node::QueryLimit(limit) => {
                refuse((**limit).clone());
                Vec::new()
            }

            Node::WithQuerySpanJs(boxed) => {
                let (pat, id) = (&boxed.0, boxed.1);
                let span = host_call_span_transform(id, state.span);
                if query_error_pending() {
                    Vec::new()
                } else {
                    pat.query(&state.set_span(span))
                }
            }

            Node::FilterValuesJs(boxed) => {
                let (pat, id) = (&boxed.0, boxed.1);
                let mut output = Vec::new();
                for hap in pat.query(state) {
                    // A throwing predicate CONTAINS the failure and keeps the
                    // hap (fail open): one broken filter must not stop the set.
                    let keep = host_call_value_predicate(id, &hap.value);
                    if query_error_pending() {
                        return Vec::new();
                    }
                    if keep {
                        output.push(hap);
                    }
                }
                output
            }

            Node::FilterHapsJs(boxed) => {
                let (pat, id) = (&boxed.0, boxed.1);
                let mut output = Vec::new();
                for hap in pat.query(state) {
                    // A throwing predicate CONTAINS the failure and keeps the
                    // hap (fail open): one broken filter must not stop the set.
                    let keep = host_call_hap_predicate(id, &hap);
                    if query_error_pending() {
                        return Vec::new();
                    }
                    if keep {
                        output.push(hap);
                    }
                }
                output
            }

            Node::FilterWhenJs(boxed) => {
                let (pat, id) = (&boxed.0, boxed.1);
                let mut output = Vec::new();
                for hap in pat.query(state) {
                    let Some(whole) = hap.whole else {
                        signal_query_error(|| {
                            "Cannot read properties of undefined (reading 'begin')".into()
                        });
                        return Vec::new();
                    };
                    let keep = host_call_time_predicate(id, whole.begin);
                    if query_error_pending() {
                        return Vec::new();
                    }
                    if keep {
                        output.push(hap);
                    }
                }
                output
            }

            Node::SortHapsByPart(pat) => sort_haps_by_part(pat.query(state)),

            Node::MapHapsWithState(boxed) => boxed
                .0
                .query(state)
                .into_iter()
                .filter_map(|hap| (boxed.1)(state, &hap))
                .collect(),

            Node::Collect(pat) => collect_congruent(pat.query(state))
                .into_iter()
                .filter_map(|group| {
                    let carrier = group.first()?;
                    let ui_visuals = group
                        .iter()
                        .fold(0_u64, |visuals, hap| visuals | hap.ui_visuals_context());
                    // Every span in the group, not the carrier's alone: a
                    // collected chord is heard as one thing and was typed
                    // as several, and an editor lighting only the first of
                    // them says a note sounded that did not.
                    let mut context: Vec<(usize, usize)> =
                        group.iter().flat_map(|hap| hap.context.iter()).collect();
                    context.sort_unstable();
                    context.dedup();
                    Some(
                        Hap::new(
                            carrier.whole,
                            carrier.part,
                            Value::Haps(value::HapList::new(group)),
                        )
                        .with_ui_visuals_context(ui_visuals)
                        .with_context(context),
                    )
                })
                .collect(),

            Node::CycleOnsetIndex(pat) => query_cycle_onset_index(pat, state),
            Node::CycleOnsetCount(pat) => query_cycle_onset_count(pat, state),

            // Extension implementations may rewrite public Hap values. They
            // have no provenance contract, so only later direct controls bind.
            Node::Extension(extension) => extension
                .query(state)
                .into_iter()
                .map(Hap::without_live_controls)
                .collect(),

            // Out of line: locals in query_node's frame multiply across the
            // recursion (see query_wchoose's note; the 64-layer fast/slow
            // control aborts otherwise).
            Node::WithRandSeed(boxed) => query_with_rand_seed(&boxed.0, boxed.1, state),

            Node::LoopAt(boxed) => {
                let cps = state
                    .controls
                    .get("_cps")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.5);
                combinators::loop_at_cps(&boxed.0, boxed.1, cps).query(state)
            }
            Node::MidiKeys(boxed) => query_midi_keys(&boxed.0, &boxed.1, state),

            Node::WChoose(choice) => query_wchoose(
                &choice.chooser,
                &choice.total,
                &choice.cumulative,
                &choice.values,
                choice.inner,
                state,
            ),

            Node::PickPatternified {
                patterns,
                index_mode,
                mode,
            } => query_patternified_pick(&patterns.0, &patterns.1, *index_mode, *mode, state),

            // innerBind: the inner hap's whole survives; context locations
            // concatenate.
            Node::InnerJoin(boxed) => {
                let (pat, f) = (&boxed.0, &boxed.1);
                let mut out = Vec::new();
                for outer in pat.query(state) {
                    let inner_pat = f(&outer.value);
                    for inner in inner_pat.query(&state.set_span(outer.part)) {
                        let mut ctx = outer.context.clone();
                        ctx.append(&inner.context);
                        if !push_budgeted(
                            &mut out,
                            Hap {
                                whole: inner.whole, // innerJoin: (_, b) => b
                                part: inner.part,
                                keys: ContextKeys::merge_with(
                                    &inner.keys,
                                    &outer.keys,
                                    inner.live_controls(),
                                    inner.slider_binding(),
                                    joined_pick_lookup(&outer, &inner),
                                ),
                                value: inner.value.clone(),
                                context: ctx,
                            },
                        ) {
                            return Vec::new();
                        }
                    }
                }
                out
            }

            // See `Pattern::fast_gap`.
            Node::FastGap(boxed) => {
                let (pat, factor) = (&boxed.0, boxed.1);
                let span = state.span;
                let cycle = span.begin.sam();
                let one = Fraction::ONE;
                // Checked on the way IN as well as out: the span scaled by the
                // factor overflows just as readily as the haps scaled back by
                // it, and only fixing the mapping left this side still able to
                // panic.
                let Some(inner_span) = (|| {
                    let bpos = span.begin.checked_sub(cycle)?.checked_mul(factor)?.min(one);
                    let epos = span.end.checked_sub(cycle)?.checked_mul(factor)?.min(one);
                    // `if (bpos >= 1) return undefined` - drops zero-width
                    // queries landing at the start of the next cycle.
                    if bpos >= one {
                        return None;
                    }
                    Some(TimeSpan::new(
                        cycle.checked_add(bpos)?,
                        cycle.checked_add(epos)?,
                    ))
                })() else {
                    // An unrepresentable span and the empty `bpos >= 1` case
                    // both land here. The empty case is ordinary and silent;
                    // an overflow additionally records a refusal below, so the
                    // two stay distinguishable to the caller.
                    if span
                        .begin
                        .checked_sub(cycle)
                        .and_then(|d| d.checked_mul(factor))
                        .is_none()
                    {
                        refuse(QueryLimit::NativeFraction {
                            operation: "fastGap",
                        });
                    }
                    return Vec::new();
                };
                // The division by `factor` can leave the representable range:
                // a very small factor scales a span into numerators no i128
                // can hold. An unchecked overflow would panic, which ends the
                // set, not the query.
                //
                // Refuse as a resource limit instead, as the stepwise family
                // does when its exact arithmetic leaves the native range.
                // Refuse the whole node rather than drop the offending hap:
                // a partial span plays wrong events with no error, while a
                // refusal is visible and the last good score keeps playing.
                let mapped: Option<Vec<Hap>> = pat
                    .query(&state.set_span(inner_span))
                    .into_iter()
                    .map(|hap| Self::fast_gap_unmap_hap(&hap, factor))
                    .collect();
                match mapped {
                    Some(haps) => haps,
                    None => {
                        refuse(QueryLimit::NativeFraction {
                            operation: "fastGap",
                        });
                        Vec::new()
                    }
                }
            }

            Node::RepeatCycles(boxed) => {
                let (pattern, count) = (&boxed.0, boxed.1);
                // Every shift is checked: a span at the far edge of the
                // native range refuses rather than unwinds, as `fastGap`
                // does.
                let refuse_shift = || {
                    refuse(QueryLimit::NativeFraction {
                        operation: "repeatCycles",
                    });
                };
                let cycle = state.span.begin.sam();
                let Some(delta) = cycle
                    .checked_div(count)
                    .and_then(|source| cycle.checked_sub(source.sam()))
                else {
                    refuse_shift();
                    return Vec::new();
                };
                let shifted = state.with_span(|span| {
                    match (span.begin.checked_sub(delta), span.end.checked_sub(delta)) {
                        (Some(begin), Some(end)) => TimeSpan::new(begin, end),
                        _ => {
                            refuse_shift();
                            *span
                        }
                    }
                });
                pattern
                    .query(&shifted)
                    .into_iter()
                    .map(|mut hap| {
                        hap.update_span(|span| {
                            span.with_time(|time| match time.checked_add(delta) {
                                Some(shifted) => shifted,
                                None => {
                                    refuse_shift();
                                    time
                                }
                            })
                        });
                        hap
                    })
                    .collect()
            }

            // Both sides are queried over the same span; parts intersect, and
            // `appBoth`'s whole is `intersection_e` - undefined if either side
            // is analog.
            Node::AppBoth(node) => {
                let AppNode(pat_func, pat_val, apply, lookup_flow) = &**node;
                let funcs = pat_func.query(state);
                let vals = pat_val.query(state);
                let mut out = Vec::new();
                for hf in &funcs {
                    for hv in &vals {
                        let Some(part) = hf.part.intersection(&hv.part) else {
                            continue;
                        };
                        let whole = match (hf.whole, hv.whole) {
                            (Some(a), Some(b)) => match a.intersection(&b) {
                                Some(w) => Some(w),
                                // `intersection_e` THROWS; see `queryArc`.
                                None => {
                                    signal_query_error(|| "TimeSpans do not intersect".into());
                                    continue;
                                }
                            },
                            _ => None,
                        };
                        let hap = applied_hap(hf, hv, whole, part, apply, *lookup_flow);
                        if query_stop_requested() {
                            return Vec::new();
                        }
                        if !push_budgeted(&mut out, hap) {
                            return Vec::new();
                        }
                    }
                }
                out
            }

            Node::FmapJs(boxed) => map_js_haps(boxed.0.query(state), boxed.1, false, None),
            Node::FmapJsPreservingControls(boxed) => {
                map_js_haps(boxed.0.query(state), boxed.1, true, boxed.2)
            }

            Node::JsQuery(id) => host_call_query(*id, state),

            Node::Timeline(pair, timelines) => {
                let PatternPair(time_pattern, pattern) = &**pair;
                let scheduler = state.controls.get("cyclist").is_some_and(Value::js_truthy);
                let mut result = Vec::new();
                for time_hap in time_pattern.query(state) {
                    let key = pick_property_key(&time_hap.value);
                    let is_zero = matches!(&time_hap.value, Value::F64(value) if *value == 0.0);
                    let offset = if is_zero {
                        Fraction::ZERO
                    } else if let Some(offset) = timelines.get(&key) {
                        offset
                    } else {
                        let arc = time_hap.whole_or_part();
                        if !scheduler || state.span.begin < arc.midpoint() {
                            arc.begin
                        } else {
                            arc.end
                        }
                    };
                    if scheduler {
                        timelines.insert(key, offset);
                        if !is_zero {
                            let negative = Value::F64(-pick_js_number(&time_hap.value));
                            timelines.remove(&pick_property_key(&negative));
                        }
                    }
                    for hap in pattern.late(offset).query(&state.set_span(time_hap.part)) {
                        let mut context = hap.context.clone();
                        context.append(&time_hap.context);
                        if !push_budgeted(
                            &mut result,
                            Hap {
                                whole: hap.whole,
                                part: hap.part,
                                keys: ContextKeys::merge_with(
                                    &time_hap.keys,
                                    &hap.keys,
                                    hap.live_controls(),
                                    hap.slider_binding(),
                                    hap.pick_lookup_arc(),
                                ),
                                value: hap.value,
                                context,
                            },
                        ) {
                            return Vec::new();
                        }
                    }
                }
                result
            }

            // A `PatternOf*` queried without a join would yield PATTERN-VALUED
            // haps, which nothing downstream can serialise; the node resolves
            // only through a join. Routing the failure through
            // `signal_query_error` matters because the shape is reachable from
            // user-authored source (`pure(pure("bd")).polyJoin()` after
            // transpilation), and a live-coded mistake must not abort the
            // process.
            Node::PatternOfPure(..)
            | Node::PatternOfPureOperand(..)
            | Node::PatternOfDynamic(..)
            | Node::PatternOfHap(..) => {
                signal_query_error(pattern_valued_hap_error);
                Vec::new()
            }

            Node::XenPatterns(boxed) => xen::query_xen_patterns(&boxed.0, &boxed.1, state),

            // bindWhole: each inner pattern is queried over its outer hap's
            // part; the mode picks whose whole survives, and contexts
            // concatenate outer-then-inner.
            Node::Join(..) | Node::JoinBoxed(..) => {
                let (plain_owned, mode);
                let inner = match self.node_kind() {
                    Node::Join(cell, flags, m) => {
                        plain_owned = plain_pattern(cell, *flags);
                        mode = m;
                        &plain_owned
                    }
                    Node::JoinBoxed(boxed) => {
                        mode = &boxed.1;
                        &boxed.0
                    }
                    _ => unreachable!(),
                };
                // Every mode queries the pattern-of-patterns over the SAME
                // state, so one resolution serves all of them. `resolve_for_join`
                // is total: a receiver that is not a pattern-of at all still
                // produces haps, whose values simply are not patterns.
                let mut resolved = inner.resolve_for_join(state);
                // The join dereferences each value as a pattern with no guard,
                // so a non-pattern value is a `TypeError` that the `queryArc`
                // boundary turns into no haps at all.
                if resolved.iter().any(|r| r.inner.is_none()) {
                    put_resolved_list(resolved);
                    signal_query_error(|| "x.value.query is not a function".into());
                    return Vec::new();
                }

                let result = match mode {
                    JoinMode::Inner | JoinMode::Outer | JoinMode::Mix => {
                        let part_asked = part_is_asked();
                        let mut out = Vec::new();
                        let ok = with_list(|inner_haps| {
                            for ResolvedHap { hap: a, inner } in resolved.drain(..) {
                                let inner_pat = inner.expect("checked all patterns");
                                if part_asked && refuses_part(&inner_pat) {
                                    return false;
                                }
                                inner_haps.clear();
                                let asked = state.set_span(a.part);
                                QUERY_THREAD.with(|thread| {
                                    inner_pat.answer_onto(
                                        thread,
                                        &asked,
                                        table::Order::Graph,
                                        inner_haps,
                                    );
                                });
                                for b in inner_haps.drain(..) {
                                    let whole = match mode {
                                        JoinMode::Inner => b.whole,
                                        JoinMode::Outer => a.whole,
                                        // `bind`'s whole_func: undefined if either
                                        // side is analog, else `intersection_e`.
                                        JoinMode::Mix => match (a.whole, b.whole) {
                                            (Some(x), Some(y)) => match x.intersection(&y) {
                                                Some(i) => Some(i),
                                                // `intersection_e` THROWS, and the
                                                // throw reaches `queryArc`, which
                                                // then yields no haps at all.
                                                None => {
                                                    signal_query_error(|| {
                                                        "TimeSpans do not intersect".into()
                                                    });
                                                    continue;
                                                }
                                            },
                                            _ => None,
                                        },
                                        _ => unreachable!("handled by the outer match"),
                                    };
                                    if !push_budgeted(&mut out, joined_hap(&a, b, whole)) {
                                        return false;
                                    }
                                }
                            }
                            true
                        });
                        if ok { out } else { Vec::new() }
                    }

                    // Each inner pattern is focused onto its outer hap's
                    // wholeOrPart. Two PRESENT but disjoint wholes drop the
                    // hap; context order is INNER-then-outer, the reverse of
                    // bindWhole.
                    JoinMode::Squeeze => {
                        let part_asked = part_is_asked();
                        let mut out = Vec::new();
                        let ok = with_list(|inner_haps| {
                            for ResolvedHap {
                                hap: outer,
                                inner: inner_pat,
                            } in resolved.drain(..)
                            {
                                let inner_pat = inner_pat.expect("checked all patterns");
                                // `discreteOnly()`: analog outer haps are dropped.
                                let Some(outer_whole) = outer.whole else {
                                    continue;
                                };
                                if part_asked && refuses_part(&inner_pat) {
                                    return false;
                                }
                                inner_haps.clear();
                                let asked = state.set_span(outer.part);
                                QUERY_THREAD.with(|thread| {
                                    inner_pat.query_focused_onto(
                                        thread,
                                        outer.whole_or_part(),
                                        &asked,
                                        inner_haps,
                                    );
                                });
                                for inner in inner_haps.drain(..) {
                                    let whole = match inner.whole {
                                        Some(iw) => match iw.intersection(&outer_whole) {
                                            Some(w) => Some(w),
                                            None => continue,
                                        },
                                        None => None,
                                    };
                                    let Some(part) = inner.part.intersection(&outer.part) else {
                                        continue;
                                    };
                                    let keys = ContextKeys::merge_with(
                                        &outer.keys,
                                        &inner.keys,
                                        inner.live_controls(),
                                        inner.slider_binding(),
                                        joined_pick_lookup(&outer, &inner),
                                    );
                                    let mut ctx = inner.context;
                                    ctx.append(&outer.context);
                                    if !push_budgeted(
                                        &mut out,
                                        Hap {
                                            whole,
                                            part,
                                            keys,
                                            value: inner.value,
                                            context: ctx,
                                        },
                                    ) {
                                        return false;
                                    }
                                }
                            }
                            true
                        });
                        if ok { out } else { Vec::new() }
                    }

                    // The inner pattern is queried over the whole state span,
                    // not the outer part. A whole that fails to intersect
                    // yields an analog hap rather than dropping it; only a
                    // missing `part` filters.
                    JoinMode::Reset | JoinMode::Restart => {
                        let mut out = Vec::new();
                        let ok = with_list(|inner_haps| {
                            for ResolvedHap {
                                hap: outer,
                                inner: inner_pat,
                            } in resolved.drain(..)
                            {
                                let inner_pat = inner_pat.expect("checked all patterns");
                                let Some(outer_whole) = outer.whole else {
                                    continue;
                                };
                                let shift = if *mode == JoinMode::Restart {
                                    outer_whole.begin
                                } else {
                                    outer_whole.begin.cycle_pos()
                                };
                                inner_haps.clear();
                                QUERY_THREAD.with(|thread| {
                                    inner_pat.query_restarted_onto(
                                        thread, shift, state, outer.part, inner_haps,
                                    );
                                });
                                for inner in inner_haps.drain(..) {
                                    let Some(part) = inner.part.intersection(&outer.part) else {
                                        continue;
                                    };
                                    let whole =
                                        inner.whole.and_then(|iw| iw.intersection(&outer_whole));
                                    let mut ctx = outer.context.clone();
                                    ctx.append(&inner.context);
                                    if !push_budgeted(
                                        &mut out,
                                        Hap {
                                            whole,
                                            part,
                                            keys: ContextKeys::merge_with(
                                                &inner.keys,
                                                &outer.keys,
                                                inner.live_controls(),
                                                inner.slider_binding(),
                                                joined_pick_lookup(&outer, &inner),
                                            ),
                                            value: inner.value,
                                            context: ctx,
                                        },
                                    ) {
                                        return false;
                                    }
                                }
                            }
                            true
                        });
                        if ok { out } else { Vec::new() }
                    }
                };
                put_resolved_list(resolved);
                result
            }

            // The outer pattern is queried over CYCLE ZERO after shifting, so
            // the slice boundaries are always cycle-relative.
            Node::StepJoin(pat) => {
                let sam = state.span.begin.sam();
                let shifted = pat.early(sam);
                let cycle = state.set_span(TimeSpan::new(Fraction::ZERO, Fraction::ONE));
                let resolved = shifted.resolve_for_join(&cycle);
                if budget_exhausted() {
                    return Vec::new();
                }
                // Slicing calls `value.withHap(...)`, so a hap whose value
                // is not a pattern is a `TypeError` here. At query time that
                // reaches `queryArc` and empties the result; at construction
                // time it escapes as a real throw - see `try_step_join`.
                let resolved = match ResolvedHap::all_patterns(resolved) {
                    Ok(resolved) => resolved,
                    Err(value) => {
                        signal_query_error(|| step_join_value_error(&value));
                        return Vec::new();
                    }
                };
                step_slices_pattern(&resolved).query(state)
            }

            // outerJoin semantics: the OUTER hap's whole survives.
            Node::PolyJoin(pat) => {
                let resolved = pat.resolve_for_join(state);
                // A non-pattern value reads a missing step count, which the
                // fraction layer turns into `Division by Zero`; `queryArc`
                // catches it. `polyJoin`'s `fmap` is lazy, so unlike
                // `stepJoin` this is always a query-time failure, never a
                // construction one.
                let resolved = match ResolvedHap::all_patterns(resolved) {
                    Ok(resolved) => resolved,
                    Err(_) => {
                        signal_query_error(|| "Division by Zero".into());
                        return Vec::new();
                    }
                };
                let outer_steps = pat.steps();
                let mut out = Vec::new();
                for (outer, inner) in resolved {
                    let extended = poly_extend(&inner, outer_steps);
                    for b in extended.query(&state.set_span(outer.part)) {
                        let mut ctx = outer.context.clone();
                        ctx.append(&b.context);
                        if !push_budgeted(
                            &mut out,
                            Hap {
                                whole: outer.whole,
                                part: b.part,
                                keys: ContextKeys::merge(&b.keys, &outer.keys),
                                value: b.value.clone(),
                                context: ctx,
                            },
                        ) {
                            return Vec::new();
                        }
                    }
                }
                out
            }

            Node::Arp(pair) => {
                let PatternPair(pat, indices) = &**pair;
                let mut out = Vec::new();
                // `collect()`: congruent haps - equal whole, analog haps
                // grouping together - in first-seen order, which is the order
                // `groupHapsBy` produces and therefore the chord's voice order.
                for group in collect_congruent(pat.query(state)) {
                    // The carrier hap has no context of its own, so
                    // `innerJoin` contributes only the index hap's context.
                    let carrier = &group[0];
                    for index_hap in indices.query(&state.set_span(carrier.part)) {
                        // Negative integers wrap; a fractional index misses
                        // and the following dereference throws.
                        let Some(picked) = js_index(&index_hap.value, group.len()) else {
                            signal_query_error(|| {
                                "Cannot read properties of undefined (reading 'value')".into()
                            });
                            return Vec::new();
                        };
                        let picked = &group[picked];
                        // `withHap`: the joined hap's context first, then the
                        // selected hap's - `h.combineContext(h.value)`.
                        let mut context = index_hap.context.clone();
                        context.append(&picked.context);
                        if !push_budgeted(
                            &mut out,
                            Hap {
                                // `innerJoin`: the INNER (index) hap's whole.
                                whole: index_hap.whole,
                                part: index_hap.part,
                                keys: ContextKeys::merge(&picked.keys, &index_hap.keys),
                                value: picked.value.clone(),
                                context,
                            },
                        ) {
                            return Vec::new();
                        }
                    }
                }
                out
            }

            Node::ArpWith(boxed) => {
                let (pat, id) = (&boxed.0, &boxed.1);
                let mut out = Vec::new();
                for group in collect_congruent(pat.query(state)) {
                    let carrier = &group[0];
                    // `fmap((v) => reify(func(v))).innerJoin()`: the callback
                    // result is a real pattern and is queried over the
                    // collected chord's part. Its haps provide the joined
                    // timing and their VALUES are Hap-shaped objects.
                    let selected = host_call_haps(*id, &group);
                    let Some(_recursion) = CallbackRecursionGuard::enter() else {
                        signal_query_error(|| "Maximum call stack size exceeded".into());
                        return Vec::new();
                    };
                    for inner in selected.query(&state.set_span(carrier.part)) {
                        let materialized = materialize_js_value(&inner.value);
                        let Some(object) = materialized.as_object() else {
                            // `null.value` / `undefined.value` throw at the
                            // first dereference. Other primitives are boxed,
                            // yield `undefined` for `.value`, then fail in
                            // `combineContext` at `b.context.locations`.
                            let message = match materialized {
                                Value::Null => "Cannot read properties of null (reading 'value')",
                                Value::Undefined => {
                                    "Cannot read properties of undefined (reading 'value')"
                                }
                                _ => "Cannot read properties of undefined (reading 'locations')",
                            };
                            signal_query_error(|| message.into());
                            return Vec::new();
                        };
                        // `h.value.value`: a missing `value` property is legal
                        // JavaScript and produces an undefined-valued hap.
                        let value = object.get("value").cloned().unwrap_or(Value::Undefined);
                        // `h.combineContext(h.value)` reads
                        // `b.context.locations`. Missing/undefined/null throws;
                        // other primitives are boxed by JavaScript and simply
                        // contribute no `locations` property.
                        let context = match object.get("context") {
                            None | Some(Value::Undefined) => {
                                signal_query_error(|| {
                                    "Cannot read properties of undefined (reading 'locations')"
                                        .into()
                                });
                                return Vec::new();
                            }
                            Some(Value::Null) => {
                                signal_query_error(|| {
                                    "Cannot read properties of null (reading 'locations')".into()
                                });
                                return Vec::new();
                            }
                            Some(Value::Object(context)) => Some(context),
                            Some(_) => None,
                        };
                        let mut locations = inner.context.clone();
                        if let Some(Value::List(items)) =
                            context.and_then(|context| context.get("locations"))
                        {
                            let mut added = Vec::new();
                            for item in items.iter() {
                                let Some(location) = item.as_object() else {
                                    continue;
                                };
                                let Some(start) = location.get("start").and_then(Value::as_f64)
                                else {
                                    continue;
                                };
                                let Some(end) = location.get("end").and_then(Value::as_f64) else {
                                    continue;
                                };
                                if start.is_finite()
                                    && end.is_finite()
                                    && start >= 0.0
                                    && end >= 0.0
                                    && start.fract() == 0.0
                                    && end.fract() == 0.0
                                    && start <= usize::MAX as f64
                                    && end <= usize::MAX as f64
                                {
                                    added.push((start as usize, end as usize));
                                }
                            }
                            // One extend: past the inline ranges, each call
                            // copies the whole list.
                            locations.extend_from_slice(&added);
                        }
                        if !push_budgeted(
                            &mut out,
                            Hap {
                                whole: inner.whole,
                                part: inner.part,
                                value,
                                context: locations,
                                // `combineContext(h.value)`: each key on the
                                // value-side context wins, and the receiving
                                // hap keeps each key that side lacks. `.log()`
                                // writes its formatted text under `logLine`,
                                // which the runtime prints on the onset.
                                keys: ContextKeys::merge_with(
                                    &ContextKeys::of(context_keys::Keys {
                                        scale: context
                                            .and_then(|context| context.get("scale"))
                                            .and_then(|scale| match scale {
                                                Value::Str(name) => Some(name.to_arc()),
                                                _ => None,
                                            }),
                                        edo_size: context
                                            .and_then(|context| context.get("edoSize"))
                                            .and_then(Value::as_f64),
                                        scale_definition: context
                                            .and_then(|context| context.get("scaleDefinition"))
                                            .cloned()
                                            .map(Arc::new),
                                        tags: context
                                            .and_then(|context| context.get("tags"))
                                            .and_then(tags_from_value),
                                        log_line: context
                                            .and_then(|context| context.get("logLine"))
                                            .and_then(|value| match value {
                                                Value::Str(text) => Some(text.to_arc()),
                                                _ => None,
                                            }),
                                        ..Default::default()
                                    }),
                                    &inner.keys,
                                    [0; 4],
                                    0,
                                    None,
                                ),
                            },
                        ) {
                            return Vec::new();
                        }
                    }
                    if query_error_pending() {
                        return Vec::new();
                    }
                }
                out
            }

            Node::ArpWithPattern(pair) => {
                let PatternPair(pat, callbacks) = &**pair;
                let mut out = Vec::new();
                for callback_hap in callbacks.query(state) {
                    let Value::Function(function) = &callback_hap.value else {
                        signal_query_error(|| "func is not a function".into());
                        return Vec::new();
                    };
                    let Some(id) = function.callback_id() else {
                        signal_query_error(|| "func is not a function".into());
                        return Vec::new();
                    };
                    let selected =
                        arp_with(pat.clone(), id).query(&state.set_span(callback_hap.part));
                    if query_error_pending() {
                        return Vec::new();
                    }
                    for mut hap in selected {
                        let mut context = callback_hap.context.clone();
                        context.append(&hap.context);
                        hap.context = context;
                        if !push_budgeted(&mut out, hap) {
                            return Vec::new();
                        }
                    }
                }
                out
            }

            // Pattern-valued haps, as above. Reachable from user source:
            // `pure(pure("bd")).polyJoin()` transpiles to a THREE-level nest,
            // so the join flattens one level and leaves a `PurePattern` to be
            // queried directly.
            Node::PurePattern(_) | Node::PatternOfJs(..) => {
                signal_query_error(pattern_valued_hap_error);
                Vec::new()
            }
        }
    }

    /// Query a `PatternOf*` (possibly behind time transforms) and resolve each
    /// hap's inner pattern.
    ///
    /// `Join` does this inline, but `stepJoin` needs the inner patterns as
    /// values so it can slice and `stepcat` them. Top-level pattern-of-pattern
    /// nodes retain their exact timing here rather than being flattened.
    ///
    /// `None` means "this node is not a pattern-of-patterns at all", which is
    /// a different thing from a pattern-of-patterns that resolves to nothing.
    /// Callers that just want the haps a join consumes want
    /// [`Pattern::resolve_for_join`] instead.
    fn resolve_pattern_of(&self, state: &State) -> Option<Vec<ResolvedHap>> {
        let resolve = || {
            let mut resolved = self.resolve_pattern_of_inner(state);
            if let (Some(settings), Some(resolved)) = (self.runtime_settings(), &mut resolved) {
                for hap in resolved {
                    if let Some(inner) = &mut hap.inner {
                        inner.attach_runtime_settings(settings.clone());
                    }
                }
            }
            resolved
        };
        match self.runtime_settings() {
            Some(settings) => settings.with(resolve),
            None => resolve(),
        }
    }

    fn resolve_pattern_of_inner(&self, state: &State) -> Option<Vec<ResolvedHap>> {
        match self.node_kind() {
            Node::PatternOfPure(boxed) => {
                let (outer, f, memo) = &**boxed;
                let mut resolved = take_resolved_list();
                let settings = settings::current_state_id();
                let ok = with_list(|outer_haps| {
                    QUERY_THREAD.with(|thread| {
                        outer.answer_onto(thread, state, table::Order::Graph, outer_haps);
                    });
                    for hap in outer_haps.drain(..) {
                        let inner = memo
                            .construct(&hap.value, &settings, |value| f(value))
                            .pattern()
                            .clone();
                        if query_error_pending() || budget_exhausted() {
                            return false;
                        }
                        resolved.push(ResolvedHap::of(hap, inner));
                    }
                    true
                });
                if !ok {
                    resolved.clear();
                }
                Some(resolved)
            }
            Node::PatternOfPureOperand(boxed, map) => {
                let OperandPureNode(outer, operand, memo) = &**boxed;
                let mut resolved = take_resolved_list();
                let settings = settings::current_state_id();
                let ok = with_list(|outer_haps| {
                    QUERY_THREAD.with(|thread| {
                        outer.answer_onto(thread, state, table::Order::Graph, outer_haps);
                    });
                    for mut hap in outer_haps.drain(..) {
                        let inner = memo
                            .construct(&hap.value, &settings, |value| map.apply(value, operand))
                            .pattern()
                            .clone();
                        if query_error_pending() || budget_exhausted() {
                            return false;
                        }
                        if hap.pick_lookup().is_some() {
                            hap.keys.edit().pick_lookup = None;
                            hap.keys.trim();
                        }
                        resolved.push(ResolvedHap::of(hap, inner));
                    }
                    true
                });
                if !ok {
                    resolved.clear();
                }
                Some(resolved)
            }
            Node::PatternOfDynamic(boxed) => {
                let (outer, f) = (&boxed.0, &boxed.1);
                let mut resolved = take_resolved_list();
                let ok = with_list(|outer_haps| {
                    QUERY_THREAD.with(|thread| {
                        outer.answer_onto(thread, state, table::Order::Graph, outer_haps);
                    });
                    for hap in outer_haps.drain(..) {
                        let inner = f(&hap.value);
                        // Iteration unwinds on the first throwing callback, as
                        // Array.map would. A host callback records that throw in the
                        // shared query-error channel and returns silence so Rust
                        // itself stays panic-free; stop HERE or later carrier haps
                        // would still expose user side effects before queryArc
                        // eventually discarded the answer.
                        if query_error_pending() || budget_exhausted() {
                            return false;
                        }
                        resolved.push(ResolvedHap::of(hap, inner));
                    }
                    true
                });
                if !ok {
                    resolved.clear();
                }
                Some(resolved)
            }
            Node::PatternOfHap(bind) => {
                let HapBind(outer, _, f) = &**bind;
                let mut resolved = take_resolved_list();
                let ok = with_list(|outer_haps| {
                    QUERY_THREAD.with(|thread| {
                        outer.answer_onto(thread, state, table::Order::Graph, outer_haps);
                    });
                    for hap in outer_haps.drain(..) {
                        let inner = f(&hap);
                        if query_error_pending() || budget_exhausted() {
                            return false;
                        }
                        resolved.push(ResolvedHap::of(hap, inner));
                    }
                    true
                });
                if !ok {
                    resolved.clear();
                }
                Some(resolved)
            }
            // `pure(pattern)`: one hap per cycle, each carrying the same known
            // inner pattern. Mirrors `Node::Pure`'s query exactly, including
            // its pre-charge: if every inner pattern is silent, the join's
            // output stays empty and no later vector exists for the generic
            // check to catch, so the charge belongs here.
            Node::PurePattern(inner) => {
                let spans = state.span.span_cycles();
                if !charge_haps(spans.len()) {
                    return Some(Vec::new());
                }
                note_materialised(spans.len());
                Some(
                    spans
                        .into_iter()
                        .map(|subspan| {
                            ResolvedHap::of(
                                Hap::new(
                                    Some(whole_cycle(subspan.begin)),
                                    subspan,
                                    Value::Undefined,
                                ),
                                (**inner).clone(),
                            )
                        })
                        .collect(),
                )
            }

            // `fmap(func)` with a user callback. `fmap` is representation-
            // agnostic: whatever the outer hap carries is what `func`
            // receives. After transpilation the outer is often itself
            // pattern-valued - `pure("bd")` becomes a pattern-of-patterns
            // while `pure('bd')` stays a string - and the callback must then
            // be handed a PATTERN. Squeezing that through `Value` is
            // impossible by design, so the two cases dispatch on the outer's
            // representation, per hap. The single/double quote distinction
            // is the transpiler's.
            Node::PatternOfJs(boxed) => {
                let (outer, id) = (&boxed.0, &boxed.1);
                let mut out_haps = Vec::new();
                match outer.resolve_pattern_of(state) {
                    Some(resolved) => {
                        for resolved in resolved {
                            let out = match resolved.inner {
                                Some(inner) => host_call_bind(*id, BindArg::Pattern(inner)),
                                // A nested level that is itself not a pattern:
                                // the hap's own value is what the callback gets.
                                None => host_call_bind(*id, BindArg::Value(&resolved.hap.value)),
                            };
                            if query_error_pending() || budget_exhausted() {
                                return Some(Vec::new());
                            }
                            out_haps.push(ResolvedHap::from_bind(resolved.hap, out));
                        }
                    }
                    None => {
                        for hap in outer.query(state) {
                            let out = host_call_bind(*id, BindArg::Value(&hap.value));
                            if query_error_pending() || budget_exhausted() {
                                return Some(Vec::new());
                            }
                            out_haps.push(ResolvedHap::from_bind(hap, out));
                        }
                    }
                }
                Some(out_haps)
            }

            // Unwrap through the ordinary structure/time transformations a
            // pattern-of-patterns may be wrapped in before the join. Matching
            // only the bare node made `polyJoin` return silence for anything
            // that had been `fast`ed, `early`d or split.
            Node::SplitQueries(cell, flags) => {
                plain_pattern(cell, *flags).resolve_pattern_of(state)
            }
            Node::SplitQueriesBoxed(inner) => inner.resolve_pattern_of(state),
            Node::AddContext(boxed) => boxed.0.resolve_pattern_of(state).map(|resolved| {
                let locs = &boxed.1;
                resolved
                    .into_iter()
                    .map(|mut resolved| {
                        resolved.hap.context.extend_from_slice(locs);
                        resolved
                    })
                    .collect()
            }),
            Node::WithQueryTime(boxed) => boxed
                .0
                .resolve_pattern_of(&state.with_span(|sp| sp.with_time(|t| (boxed.1)(t)))),
            // `rev` reflects the query span and then reflects the haps back, so
            // it cannot delegate the way the time transforms above do: the
            // same reflection applies on both sides. Without this arm,
            // `pure("bd sd").rev().polyJoin()` falls through to a plain query
            // of the pattern-of-patterns and produces nothing instead of its
            // two haps.
            Node::Rev(inner) => {
                let cycle = state.span.begin.sam();
                let next_cycle = state.span.begin.next_sam();
                let reflect = |sp: &TimeSpan| {
                    let b = cycle.add(next_cycle.sub(sp.begin));
                    let e = cycle.add(next_cycle.sub(sp.end));
                    TimeSpan::new(e, b)
                };
                inner
                    .resolve_pattern_of(&state.set_span(reflect(&state.span)))
                    .map(|resolved| {
                        resolved
                            .into_iter()
                            .map(|resolved| ResolvedHap {
                                hap: resolved.hap.with_span(reflect),
                                inner: resolved.inner,
                            })
                            .collect()
                    })
            }
            Node::WithHapTime(boxed) => boxed.0.resolve_pattern_of(state).map(|resolved| {
                resolved
                    .into_iter()
                    .map(|resolved| ResolvedHap {
                        hap: resolved.hap.with_span(|sp| sp.with_time(|t| (boxed.1)(t))),
                        inner: resolved.inner,
                    })
                    .collect()
            }),
            Node::Fast(node) => node
                .0
                .resolve_pattern_of(
                    &state.with_span(|sp| sp.with_time(|t| fast_query_time(t, node.1, node.2))),
                )
                .map(|resolved| {
                    resolved
                        .into_iter()
                        .map(|resolved| ResolvedHap {
                            hap: resolved
                                .hap
                                .with_span(|sp| sp.with_time(|t| fast_hap_time(t, node.1, node.2))),
                            inner: resolved.inner,
                        })
                        .collect()
                }),
            Node::FastSmall(cell, flags, numer, denom, is_slow) => {
                let factor = Fraction::new(i128::from(*numer), i128::from(*denom));
                let op = if *is_slow { "slow" } else { "fast" };
                plain_pattern(cell, *flags)
                    .resolve_pattern_of(
                        &state.with_span(|sp| sp.with_time(|t| fast_query_time(t, factor, op))),
                    )
                    .map(|resolved| {
                        resolved
                            .into_iter()
                            .map(|resolved| ResolvedHap {
                                hap: resolved
                                    .hap
                                    .with_span(|sp| sp.with_time(|t| fast_hap_time(t, factor, op))),
                                inner: resolved.inner,
                            })
                            .collect()
                    })
            }
            Node::Shift(node) => node
                .0
                .resolve_pattern_of(
                    &state.with_span(|sp| sp.with_time(|t| shift_query_time(t, node.1, node.2))),
                )
                .map(|resolved| {
                    resolved
                        .into_iter()
                        .map(|resolved| ResolvedHap {
                            hap: resolved.hap.with_span(|sp| {
                                sp.with_time(|t| shift_hap_time(t, node.1, node.2))
                            }),
                            inner: resolved.inner,
                        })
                        .collect()
                }),
            Node::ShiftSmall(cell, flags, numer, denom, is_early) => {
                let offset = Fraction::new(i128::from(*numer), i128::from(*denom));
                let op = if *is_early { "early" } else { "late" };
                plain_pattern(cell, *flags)
                    .resolve_pattern_of(
                        &state.with_span(|sp| sp.with_time(|t| shift_query_time(t, offset, op))),
                    )
                    .map(|resolved| {
                        resolved
                            .into_iter()
                            .map(|resolved| ResolvedHap {
                                hap: resolved.hap.with_span(|sp| {
                                    sp.with_time(|t| shift_hap_time(t, offset, op))
                                }),
                                inner: resolved.inner,
                            })
                            .collect()
                    })
            }
            // `stack` resolves each branch and concatenates, mirroring the
            // query's flatten. `seqPLoop` (and any join over a stacked
            // pattern-of-patterns) otherwise fell through to the plain-query
            // fallback and hit the pattern-valued refusal with no haps at all.
            Node::Stack(children) => Some(
                extend_budgeted(
                    children
                        .iter()
                        .map(|child| query_stack_child(|| child.resolve_for_join(state))),
                )
                // A refused concatenation resolves to silence; the refusal
                // itself travels on the query-wide channel.
                .unwrap_or_default(),
            ),
            Node::StackPure(cells) => Some(
                extend_budgeted(cells.iter().map(|cell| {
                    query_stack_child(|| plain_pattern(cell, 0).resolve_for_join(state))
                }))
                .unwrap_or_default(),
            ),
            // `fastGap` on a pattern-of-patterns: the join must consume the
            // same compressed outer windows the query path produces, or
            // `pure(p).compress(b, e).innerJoin()` - the shape `seqPLoop`
            // builds - falls through to the plain-query fallback and hits the
            // pattern-valued refusal. The in/out scaling mirrors the query
            // arm exactly.
            Node::FastGap(boxed) => {
                let (inner, factor) = (&boxed.0, boxed.1);
                let span = state.span;
                let one = Fraction::ONE;
                let mut resolved_haps = Vec::new();
                for subspan in span.span_cycles() {
                    let cycle = subspan.begin.sam();
                    let Some(inner_span) = (|| {
                        let bpos = subspan
                            .begin
                            .checked_sub(cycle)?
                            .checked_mul(factor)?
                            .min(one);
                        let epos = subspan
                            .end
                            .checked_sub(cycle)?
                            .checked_mul(factor)?
                            .min(one);
                        if bpos >= one {
                            return None;
                        }
                        Some(TimeSpan::new(
                            cycle.checked_add(bpos)?,
                            cycle.checked_add(epos)?,
                        ))
                    })() else {
                        if subspan
                            .begin
                            .checked_sub(cycle)
                            .and_then(|d| d.checked_mul(factor))
                            .is_none()
                        {
                            refuse(QueryLimit::NativeFraction {
                                operation: "fastGap",
                            });
                            return Some(Vec::new());
                        }
                        // The compressed window lies before this cycle; the
                        // query path drops it silently, so skip it here too.
                        continue;
                    };
                    let mapped: Option<Vec<ResolvedHap>> = inner
                        .resolve_pattern_of(&state.set_span(inner_span))?
                        .into_iter()
                        .map(|mut resolved| {
                            resolved.hap = Self::fast_gap_unmap_hap(&resolved.hap, factor)?;
                            Some(resolved)
                        })
                        .collect();
                    let Some(haps) = mapped else {
                        refuse(QueryLimit::NativeFraction {
                            operation: "fastGap",
                        });
                        return Some(Vec::new());
                    };
                    // The per-cycle results concatenate, so each extend is
                    // charged: the branches are individually legal while the
                    // running total is not.
                    if !charge_haps(resolved_haps.len() + haps.len()) {
                        return Some(Vec::new());
                    }
                    resolved_haps.extend(haps);
                    note_accumulated(resolved_haps.len());
                }
                Some(resolved_haps)
            }
            _ => None,
        }
    }

    /// What a join actually consumes: total where [`Pattern::resolve_pattern_of`]
    /// is partial.
    ///
    /// A receiver that is not a pattern-of-patterns still produces haps -
    /// `pure('bd').polyJoin()` queries one hap holding the string `'bd'`.
    /// `fmap` never inspects what it stores, so the failure belongs to
    /// the join that dereferences the value, and each join fails differently
    /// (`polyJoin` reads `._steps`, `stepJoin` calls `.withHap`). Reporting
    /// "not a pattern-of" from here instead would collapse those into one
    /// shape.
    fn resolve_for_join(&self, state: &State) -> Vec<ResolvedHap> {
        self.resolve_pattern_of(state).unwrap_or_else(|| {
            let mut resolved = take_resolved_list();
            with_list(|outer_haps| {
                QUERY_THREAD.with(|thread| {
                    self.answer_onto(thread, state, table::Order::Graph, outer_haps);
                });
                for hap in outer_haps.drain(..) {
                    let entry = match &hap.value {
                        // Ordinary `fmap` is representation-agnostic: a
                        // callback may return a Pattern, store it in the hap, and
                        // a following join must consume it. `FmapJs` materialises
                        // that wrapper as `Value::Pattern`; treating every
                        // fallback hap as scalar made
                        // `p.fmap(x => pure(x)).innerJoin()` silently empty.
                        Value::Pattern(value) => ResolvedHap {
                            inner: Some(value.pattern().clone()),
                            hap: hap.with_value(|_| Value::Undefined),
                        },
                        _ => ResolvedHap { hap, inner: None },
                    };
                    resolved.push(entry);
                }
            });
            resolved
        })
    }

    /// The hap mapping `fastGap` applies on the way OUT: within-cycle
    /// positions divided back by the factor, clamped at the cycle boundary.
    /// Shared by the query path and the pattern-of-patterns resolution so a
    /// join consumes exactly the same compressed windows the query produces.
    fn fast_gap_unmap_hap(hap: &Hap, factor: Fraction) -> Option<Hap> {
        let begin = hap.part.begin;
        let end = hap.part.end;
        let cyc = begin.sam();
        let one = Fraction::ONE;
        let begin_pos = begin.checked_sub(cyc)?.checked_div(factor)?.min(one);
        let end_pos = end.checked_sub(cyc)?.checked_div(factor)?.min(one);
        let new_part = TimeSpan::new(cyc.checked_add(begin_pos)?, cyc.checked_add(end_pos)?);
        let new_whole = match hap.whole {
            Some(w) => Some(TimeSpan::new(
                new_part
                    .begin
                    .checked_sub(begin.checked_sub(w.begin)?.checked_div(factor)?)?,
                new_part
                    .end
                    .checked_add(w.end.checked_sub(end)?.checked_div(factor)?)?,
            )),
            None => None,
        };
        Some(Hap {
            whole: new_whole,
            part: new_part,
            value: hap.value.clone(),
            context: hap.context.clone(),
            keys: hap.keys.clone(),
        })
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
        if matches!(self.node_kind(), Node::Sequence(_)) && sequence::folds(&self) {
            let steps = self.steps();
            let runtime = self.runtime_settings().cloned();
            let pure_loc = self.pure_loc_raw();
            if let Some(cell) = self.own_node()
                && let Node::Sequence(layers) = &mut cell.kind
                && sequence::locate_in_place(layers, locs)
            {
                let node = Arc::clone(&self.node);
                self = Self::holding(node).with_steps(steps);
                if let Some(runtime) = runtime {
                    self.attach_runtime_settings(runtime);
                }
                if let Some(loc) = pure_loc {
                    self.set_pure_loc_raw(Some(loc));
                }
                return self;
            }
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
        if let Some(folded) = sequence::with_ui_visual_slot(self, slot) {
            return folded.with_steps_of(self);
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

    /// Owned variant of [`Self::with_ui_visual_slot`] that updates an unshared
    /// `Node::Sequence` in place without cloning layers or `NodeCell`.
    pub fn with_ui_visual_slot_owned(mut self, slot: u8) -> Self {
        if self.extras.is_none()
            && let Some(cell) = Arc::get_mut(&mut self.node)
            && let Node::Sequence(layers) = &mut cell.kind
            && sequence::with_ui_visual_slot_in_place(layers, slot)
        {
            return self;
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
    fn meets_each_control(&self) -> bool {
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

    /// Set a constant of `control`, a map of controls at `context`, on
    /// this pattern, as the `set` of a `pure` of the control at those
    /// places does: where the pattern takes it, in a node of its own where
    /// the pattern meets constants so, and by that `set` otherwise.
    fn set_in_turn(&mut self, control: Value, context: Locations) {
        if self.meets_constants() && control.is_data(0) {
            if !self.set_control(&control, &context) {
                *self = self.met_control(&control, context);
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
                    if matches!(picked.node_kind(), Node::Sequence(_)) && sequence::folds(picked) {
                        let p_steps = picked.steps();
                        let p_runtime = picked.runtime_settings().cloned();
                        if let Some(p_cell) = picked.own_node()
                            && let Node::Sequence(layers) = &mut p_cell.kind
                        {
                            sequence::set_here_cached(
                                layers,
                                control,
                                Locations::new(),
                                &mut dress_cache,
                            );
                            let p_node = Arc::clone(&picked.node);
                            *picked = Self::holding(p_node).with_steps(p_steps);
                            if let Some(p_runtime) = p_runtime {
                                picked.attach_runtime_settings(p_runtime);
                            }
                            continue;
                        }
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
                let mut patterns: Vec<Pattern> =
                    cells.iter().map(|cell| plain_pattern(cell, 0)).collect();
                for pattern in patterns.iter_mut().filter(|pattern| !pattern.is_rest()) {
                    if let Some((waves, under)) = pattern.taking(control) {
                        pattern.write(waves, &under, context.clone(), apart);
                        continue;
                    }
                    if pattern.meets_each_control() && control.is_data(0) {
                        *pattern = pattern.met_row(control, context.clone(), apart.has_many());
                        continue;
                    }
                    let row = Self::apart_under(control, apart.each());
                    for (each, at) in row {
                        pattern.set_in_turn(each, at);
                    }
                }
                if can_pack_pure_cells(&patterns) {
                    *cells = pack_pure_cells(patterns);
                } else {
                    cell.kind = Node::Stack(patterns.into_boxed_slice());
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
            Node::StackPure(cells) => cells.iter().any(|cell| {
                !matches!(cell.kind, Node::Silence) && plain_pattern(cell, 0).takes_control(control)
            }),
            Node::FilterValues(..) => self
                .under_structure()
                .is_some_and(|functions| functions.takes_control(control)),
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
        static POOL: std::sync::OnceLock<
            std::sync::Mutex<std::collections::HashMap<usize, ToPurePatternFn>>,
        > = std::sync::OnceLock::new();
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

    /// `queryArc(begin, end)` - the public pattern-query boundary.
    ///
    /// Also the boundary at which a mid-query failure becomes an empty
    /// result. Nested `query_arc` calls (a JS callback querying another
    /// pattern) absorb their own errors: the inner boundary catches first.
    pub fn query_arc(&self, begin: Fraction, end: Fraction) -> Vec<Hap> {
        self.query_state(&State::new(TimeSpan::new(begin, end)))
    }

    /// `queryArc`'s error boundary for callers that need to supply a full
    /// [`State`] rather than just a span - the scheduler attaches `_cps`, and
    /// the JS host attaches scheduler controls.
    ///
    /// **Use this, not `query`, at any top-level entry point.** `query` is the
    /// raw recursive step and has no boundary: a pattern whose query fails
    /// mid-way (`note("c").add("x")` reaches `parseNumeral`'s throw) hands
    /// back NaN-valued haps instead of the empty result the boundary
    /// guarantees. On the scheduling path that is not cosmetic - those haps
    /// become onset events, scheduling audio where the score must be silent.
    pub fn query_state(&self, state: &State) -> Vec<Hap> {
        self.try_query_state(state).unwrap_or_default()
    }

    /// `query_state`, distinguishing RESOURCE EXHAUSTION from an ordinary
    /// query throw.
    ///
    /// Both end in no haps, and conflating them is how a limit becomes a lie: a
    /// caller cannot tell "this pattern produces nothing" from "this pattern
    /// was refused", so a truncated render looks like a correct silent one.
    /// Exhaustion therefore travels on its own flag, which the `queryArc`
    /// boundary below deliberately does not clear.
    pub fn try_query_state(&self, state: &State) -> Result<Vec<Hap>, QueryLimit> {
        self.try_query_state_with_budget(state, DEFAULT_HAP_BUDGET)
    }

    /// One outer `queryArc` that keeps the throw distinct from empty haps.
    ///
    /// Steady-state scheduling still uses [`Self::try_query_state`] (silence
    /// for that window, clock keeps running). A live replacement must call
    /// this so a typo that constructs but throws at query time cannot replace
    /// the sounding score with silence.
    pub fn query_arc_outcome(&self, state: &State) -> Result<QueryArcOutcome, QueryLimit> {
        self.query_arc_outcome_with_budget(state, DEFAULT_HAP_BUDGET)
    }

    /// `try_query_state` with an explicit budget.
    ///
    /// Public so embedders and tests can exercise the same resource boundary
    /// with a smaller limit instead of allocating millions of haps.
    ///
    /// Also genuinely useful: an embedder scheduling on a small device can pick
    /// a tighter bound than this build's default.
    pub fn try_query_state_with_budget(
        &self,
        state: &State,
        budget: u64,
    ) -> Result<Vec<Hap>, QueryLimit> {
        match self.query_arc_outcome_with_budget(state, budget)? {
            QueryArcOutcome::Haps(haps) => Ok(haps),
            // The `queryArc` catch: this window is silent. The throw is
            // not a resource refusal and must not become scheduled NaNs.
            QueryArcOutcome::Thrown(_) => Ok(Vec::new()),
        }
    }

    /// [`Self::query_arc_outcome`] with an explicit hap budget.
    pub fn query_arc_outcome_with_budget(
        &self,
        state: &State,
        budget: u64,
    ) -> Result<QueryArcOutcome, QueryLimit> {
        self.query_arc_outcome_in(state, budget, table::Order::Graph)
    }

    /// [`Self::query_arc_outcome_with_budget`] with the haps sorted as
    /// [`sort_haps_for_query`] sorts them. A precomputed answer comes sorted
    /// already, so this is cheaper than sorting the outcome afterwards.
    pub fn query_arc_outcome_sorted_with_budget(
        &self,
        state: &State,
        budget: u64,
    ) -> Result<QueryArcOutcome, QueryLimit> {
        self.query_arc_outcome_in(state, budget, table::Order::Sorted)
    }

    fn query_arc_outcome_in(
        &self,
        state: &State,
        budget: u64,
        order: table::Order,
    ) -> Result<QueryArcOutcome, QueryLimit> {
        // A fresh top-level query reports only its own contained failures; a
        // stale publication from a construction probe or an earlier query
        // must not be attributed here.
        clear_query_callback_failure();
        // The flag is thread-local and shared, so an enclosing boundary's
        // pending error is stashed and restored around this one. Nesting then
        // behaves right: the INNER boundary catches first and the outer one
        // is unaffected.
        let error_boundary = QueryErrorBoundary::enter();
        let metadata_owner = self
            .runtime_settings()
            .cloned()
            .or_else(settings::RuntimeSettings::current_requested);
        let query = || {
            with_hap_budget(budget, || {
                let raw_query_attaches_metadata = QUERY_DEPTH.with(|depth| depth.get() == 0);
                // Short windows start at 20 (3,840 B <= 4 KiB page) and adapt
                // up to 64 per root pattern once denser windows are observed,
                // avoiding repeated `Vec<Hap>` reallocs without slowing tiny
                // scores or retaining memory on the pattern.
                thread_local! {
                    static SHORT_QUERY_HINT: std::cell::Cell<(usize, u16)> =
                        const { std::cell::Cell::new((0, 20)) };
                }
                let is_short = state.span.end - state.span.begin <= Fraction::ONE;
                let root_ptr = Arc::as_ptr(&self.node) as usize;
                let room = if is_short {
                    SHORT_QUERY_HINT.with(|hint| {
                        let (ptr, cap) = hint.get();
                        if ptr == root_ptr {
                            usize::from(cap)
                        } else {
                            20
                        }
                    })
                } else {
                    64
                };
                let (mut haps, data_only) = self.answer_with_room(state, order, room);
                if is_short {
                    let len = haps.len();
                    let needed = if len <= 20 {
                        20u16
                    } else if len <= 32 {
                        32u16
                    } else if len <= 48 {
                        48u16
                    } else {
                        64u16
                    };
                    SHORT_QUERY_HINT.with(|hint| {
                        let (ptr, cap) = hint.get();
                        let next = if ptr == root_ptr {
                            cap.max(needed)
                        } else {
                            needed
                        };
                        hint.set((root_ptr, next));
                    });
                }
                // Pure graphs with no bound runtime cannot produce JS-owned
                // values, and tables hold data only.
                if data_only || (self.is_pure() && metadata_owner.is_none()) {
                    return haps;
                }
                // JS-owned arrays/objects preserve their identity throughout the
                // graph, but the host that can resolve that identity exists only
                // during the outer query. Convert returned values here so every
                // later renderer (CLI JSON, scheduler events, Hap::show) receives
                // an ordinary host-independent Value.
                for hap in &mut haps {
                    let materialized = hap.value.contains_js_value();
                    if materialized {
                        hap.value = materialize_js_value(&hap.value);
                        hap.clear_live_bindings();
                    }
                    if let Some(settings) = &metadata_owner
                        && (materialized || !raw_query_attaches_metadata)
                        && !hap.value.attach_runtime_settings(settings, 0)
                    {
                        return Vec::new();
                    }
                }
                haps
            })
        };
        // The owner governs the complete public query operation, including
        // JS container materialisation after the recursive graph returns.
        let (haps, refusal) = match &metadata_owner {
            Some(settings) => settings.with(query),
            None => query(),
        };
        let thrown = error_boundary.finish();
        // Cancellation is checked FIRST: a query stopped part way will usually
        // also look under-budget, and reporting the limit would be misleading.
        if cancellation_requested() {
            return Err(QueryLimit::Cancelled);
        }
        if let Some(limit) = refusal {
            return Err(limit);
        }
        Ok(match thrown {
            Some(message) => QueryArcOutcome::Thrown(message),
            None => QueryArcOutcome::Haps(haps),
        })
    }

    /// Sorted as Strudel's `sortHapsByPart()` does. The order is part of the
    /// serialized query output.
    ///
    /// Four sort keys: part.begin, part.end, **whole.begin, whole.end** -
    /// omitting the whole keys leaves ties in the wrong order whenever two
    /// haps share a part.
    ///
    /// # Analog haps make the comparator throw
    ///
    /// The comparator reads `whole` unconditionally, so any analog hap among
    /// two or more results makes it throw and the enclosing `queryArc` yields
    /// no haps. Substituting `whole_or_part()` would return both haps; the
    /// throw-and-silence behaviour is preserved instead. A single element is
    /// never compared, so exactly one analog hap survives untouched.
    pub fn query_arc_sorted(&self, begin: Fraction, end: Fraction) -> Vec<Hap> {
        // `query_arc` owns the recursive query's error boundary, but sorting
        // is itself observable JavaScript-compatible work: two analog haps
        // make the comparator throw. Keep a second boundary around that final
        // phase so its error becomes this call's empty result and cannot poison
        // the next independent query on the worker thread.
        let error_boundary = QueryErrorBoundary::enter();
        let haps = sort_haps_by_part(self.query_arc(begin, end));
        match error_boundary.finish() {
            Some(_) => Vec::new(),
            None => haps,
        }
    }

    /// `query_arc_sorted`, propagating a resource refusal.
    ///
    /// The untyped form above discards the refusal. That is the right default
    /// for internal call sites that cannot act on one, but not for a
    /// top-level entry point: there, a discarded refusal looks like a pattern
    /// that produced nothing.
    pub fn try_query_arc_sorted(
        &self,
        begin: Fraction,
        end: Fraction,
    ) -> Result<Vec<Hap>, QueryLimit> {
        self.try_query_arc_sorted_with_budget(begin, end, DEFAULT_HAP_BUDGET)
    }

    /// `try_query_arc_sorted` with an explicit budget. See
    /// [`Pattern::try_query_state_with_budget`].
    pub fn try_query_arc_sorted_with_budget(
        &self,
        begin: Fraction,
        end: Fraction,
        budget: u64,
    ) -> Result<Vec<Hap>, QueryLimit> {
        let state = State::new(TimeSpan::new(begin, end));
        let error_boundary = QueryErrorBoundary::enter();
        let haps = sort_haps_by_part(self.try_query_state_with_budget(&state, budget)?);
        Ok(match error_boundary.finish() {
            Some(_) => Vec::new(),
            None => haps,
        })
    }

    /// Query and sort a window while preserving query and comparator errors.
    /// The outcome distinguishes a thrown error from an empty window.
    pub fn try_query_arc_sorted_outcome_with_budget(
        &self,
        begin: Fraction,
        end: Fraction,
        budget: u64,
    ) -> Result<QueryArcOutcome, QueryLimit> {
        let state = State::new(TimeSpan::new(begin, end));
        let error_boundary = QueryErrorBoundary::enter();
        let outcome = self.query_arc_outcome_with_budget(&state, budget)?;
        Ok(match outcome {
            // Keep the boundary active until the comparator has finished.
            QueryArcOutcome::Haps(haps) => {
                let sorted = sort_haps_by_part(haps);
                match error_boundary.finish() {
                    Some(message) => QueryArcOutcome::Thrown(message),
                    None => QueryArcOutcome::Haps(sorted),
                }
            }
            // The inner query boundary already captured this error.
            thrown @ QueryArcOutcome::Thrown(_) => {
                let _ = error_boundary.finish();
                thrown
            }
        })
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
                spec.id | (u16::from(value::Table::mark_bit(spec.name())) << 10)
            } else {
                0
            };
            if matches!(self.node_kind(), Node::Sequence(_)) && sequence::takes_plain_set(&self) {
                let steps = self.steps();
                let runtime = self.runtime_settings().cloned();
                if let Some(cell) = self.own_node()
                    && let Node::Sequence(layers) = &mut cell.kind
                {
                    sequence::set_single_here(layers, &spec.key(), value.clone());
                    let node = Arc::clone(&self.node);
                    self = Self::holding(node).with_steps(steps);
                    if let Some(runtime) = runtime {
                        self.attach_runtime_settings(runtime);
                    }
                    return Ok(self);
                }
            } else if let Node::Constants(node) = self.node_kind()
                && self.same_runtime_owner(&node.0)
                && node.1.last().is_some_and(RunMeeting::takes_sets)
            {
                let steps = self.steps();
                let runtime = self.runtime_settings().cloned();
                if let Some(cell) = self.own_node()
                    && let Node::Constants(node) = &mut cell.kind
                    && let Some(last) = node.1.last_mut()
                {
                    last.set_single_after(packed_key, &spec.key(), value.clone());
                    let node = Arc::clone(&self.node);
                    self = Self::holding(node).with_steps(steps);
                    if let Some(runtime) = runtime {
                        self.attach_runtime_settings(runtime);
                    }
                    return Ok(self);
                }
            } else if packed_key != 0 && !self.takes_constants() && self.meets_constants() {
                let steps = self.steps();
                return Ok(self
                    .met([RunMeeting::of_single_control(packed_key, value.clone())])
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
            let packed_key = spec.id | (u16::from(value::Table::mark_bit(spec.name())) << 10);
            if self.can_set_packed_direct(&[packed_key]) {
                let mut val = [value.clone()];
                self.set_packed_direct(&[packed_key], &val, context);
                val[0] = Value::Undefined;
                return Ok(self);
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
        let Some((waves, under)) = self.taking(union) else {
            return Err(self);
        };
        self.write(waves, &under, context, apart);
        Ok(self)
    }

    fn can_set_packed_direct(&self, keys: &[u16]) -> bool {
        match self.node_kind() {
            Node::Sequence(_) => sequence::takes_plain_set(self),
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
                    let packed =
                        item.key_id | (u16::from(value::Table::mark_bit(key.as_str())) << 10);
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
                    if matches!(cell.kind, Node::Silence) {
                        continue;
                    }
                    if !plain_pattern(cell, 0).can_set_packed_direct(keys) {
                        return false;
                    }
                    any = true;
                }
                any
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

    fn set_packed_direct(&mut self, keys: &[u16], vals: &[Value], context: Locations) {
        let steps = self.steps();
        let runtime = self.runtime_settings().cloned();
        let Some(cell) = self.own_node() else {
            debug_assert!(false, "can_set_packed_direct matched an unwritable node");
            return;
        };
        match &mut cell.kind {
            Node::Sequence(layers) => sequence::set_packed_here(layers, keys, vals, context),
            Node::AppWave(app) => {
                let mut combined_keys = [0u16; 16];
                let mut combined_vals = [const { Value::Undefined }; 16];
                let mut len = 0usize;
                for item in app.waves.iter() {
                    let key = controls::builtin_key_ref_by_id(item.key_id)
                        .expect("validated wave key id");
                    let packed =
                        item.key_id | (u16::from(value::Table::mark_bit(key.as_str())) << 10);
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
                app.inner.set_packed_direct(
                    &combined_keys[..total],
                    &combined_vals[..total],
                    context,
                );
            }
            Node::Stack(patterns) => {
                for pattern in patterns.iter_mut().filter(|p| !p.is_rest()) {
                    pattern.set_packed_direct(keys, vals, context.clone());
                }
            }
            Node::StackPure(cells) => {
                let mut patterns: Vec<Pattern> =
                    cells.iter().map(|cell| plain_pattern(cell, 0)).collect();
                for pattern in patterns.iter_mut().filter(|p| !p.is_rest()) {
                    pattern.set_packed_direct(keys, vals, context.clone());
                }
                if can_pack_pure_cells(&patterns) {
                    *cells = pack_pure_cells(patterns);
                } else {
                    cell.kind = Node::Stack(patterns.into_boxed_slice());
                }
            }
            Node::Pick(pick) => {
                let ok = sequence::selecting_in_place(
                    &mut pick.selector,
                    &context,
                    Self::picks_whole_cycles(pick.mode),
                );
                debug_assert!(ok, "validated plain selector");
                for picked in pick.lookup.patterns_mut().filter(|p| !p.is_rest()) {
                    picked.set_packed_direct(keys, vals, Locations::new());
                }
            }
            _ => unreachable!("validated by can_set_packed_direct"),
        }
        let node = Arc::clone(&self.node);
        *self = Self::holding(node).with_steps(steps);
        if let Some(runtime) = runtime {
            self.attach_runtime_settings(runtime);
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
        if self.can_set_packed_direct(keys) {
            self.set_packed_direct(keys, vals, context);
            return Ok(self);
        }
        let union = Value::Object(OrderedMap::from_packed_builtin(keys, vals));
        self.with_row(&union, context, apart)
    }

    /// Whether a constant control may go into this pattern, by its kinds
    /// alone: the gate of a row of controls, which costs less than the
    /// controls. [`Self::takes_control`] decides for a control.
    pub(crate) fn takes_constants(&self) -> bool {
        match self.node_kind() {
            Node::Sequence(_) => true,
            Node::Pick(pick) => pick
                .lookup
                .patterns()
                .all(|picked| picked.is_rest() || picked.takes_constants()),
            Node::Stack(patterns) => patterns
                .iter()
                .any(|pattern| !pattern.is_rest() && pattern.takes_constants()),
            Node::StackPure(cells) => cells.iter().any(|cell| {
                !matches!(cell.kind, Node::Silence) && plain_pattern(cell, 0).takes_constants()
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
        static POOL: std::sync::OnceLock<
            std::sync::Mutex<std::collections::HashMap<usize, HapMapFn>>,
        > = std::sync::OnceLock::new();
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
    fn interned_pitch_rewrite<F, G>(
        key: &str,
        make: impl FnOnce() -> (F, G, Option<Arc<str>>),
    ) -> Arc<PitchRewrite>
    where
        F: Fn(&Hap) -> Option<Hap> + Send + Sync + 'static,
        G: Fn(&Value) -> Option<Value> + Send + Sync + 'static,
    {
        static POOL: std::sync::OnceLock<
            std::sync::Mutex<std::collections::HashMap<String, Arc<PitchRewrite>>>,
        > = std::sync::OnceLock::new();
        let pool = POOL.get_or_init(Default::default);
        // The name is copied only for a body that the pool takes.
        let held = pool
            .lock()
            .ok()
            .and_then(|entries| entries.get(key).map(Arc::clone));
        held.unwrap_or_else(|| {
            interned_closure(pool, key.to_owned(), || {
                let (of_hap, of_note, scale_tag) = make();
                let hap: HapMapFn = Arc::new(move |hap: &Hap| {
                    of_hap(hap).map(|mapped| mapped.with_pitch_live_controls(hap))
                });
                let keys = scale_tag.map(|tag| {
                    context_keys::ContextKeys::of(context_keys::Keys {
                        scale: Some(tag),
                        ..Default::default()
                    })
                });
                Arc::new(PitchRewrite {
                    hap,
                    note: Box::new(of_note),
                    keys,
                })
            })
        })
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
        let mut dress_cache = sequence::RenoteDressCache::default();
        self.pitched_owned(&interned, &mut dress_cache)
            .with_steps(steps)
    }

    /// [`Self::mapped_haps`] of a rewrite of pitch, which reads no span.
    /// Steps that `note` or `n` names take their new note where they are:
    /// see [`sequence::renoted`]. So does each pattern of a pick.
    fn pitched(&self, rewrite: &PitchRewrite) -> Self {
        let mut dress_cache = sequence::RenoteDressCache::default();
        self.pitched_cached(rewrite, &mut dress_cache)
    }

    fn pitched_owned(
        mut self,
        rewrite: &PitchRewrite,
        dress_cache: &mut sequence::RenoteDressCache,
    ) -> Self {
        if let Node::Sequence(layers) = self.node_kind()
            && sequence::folds(&self)
            && Arc::strong_count(&self.node) == 1
            && Arc::strong_count(layers) == 1
        {
            let steps = self.steps();
            let runtime = self.runtime_settings().cloned();
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
                let node = Arc::clone(&self.node);
                self = Self::holding(node).with_steps(steps);
                if let Some(runtime) = runtime {
                    self.attach_runtime_settings(runtime);
                }
                return self;
            }
        } else if let Node::Pick(picked) = self.node_kind()
            && picked.mode != JoinMode::Squeeze
            && sequence::folds(&self)
            && sequence::is_plain_selector(&picked.selector, false)
            && Arc::strong_count(&self.node) == 1
        {
            let steps = self.steps();
            let runtime = self.runtime_settings().cloned();
            if let Some(cell) = self.own_node()
                && let Node::Pick(picked) = &mut cell.kind
            {
                for slot in picked.lookup.patterns_mut() {
                    let owned = std::mem::replace(slot, silence());
                    *slot = owned.pitched_owned(rewrite, dress_cache);
                }
                let node = Arc::clone(&self.node);
                self = Self::holding(node).with_steps(steps);
                if let Some(runtime) = runtime {
                    self.attach_runtime_settings(runtime);
                }
                return self;
            }
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
        static POOL: std::sync::OnceLock<
            std::sync::Mutex<std::collections::HashMap<usize, HapExpandFn>>,
        > = std::sync::OnceLock::new();
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
            let Some(whole) = hap.whole else {
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
            let Some(whole) = hap.whole else {
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
        self.filter_haps(|hap| hap.whole.is_some())
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
        | Node::Sequence(_)
        | Node::QueryError(_)
        | Node::QueryLimit(_)
        | Node::Signal(_)
        | Node::JsQuery(_) => {}
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
                | Node::Sequence(_)
                | Node::QueryError(_)
                | Node::QueryLimit(_)
                | Node::Signal(_)
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
        breakpoints.push(hap.part.begin);
        breakpoints.push(hap.part.end);
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
            if span.intersection(&hap.part).is_none() {
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
        | Node::Sequence(_) => No,
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
        | Node::Signal(_)
        | Node::QueryError(_)
        | Node::QueryLimit(_) => Purity::pure(),

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
        if onsets.iter().any(|hap| hap.whole.is_none()) {
            signal_query_error(|| {
                "isobite: Cannot read properties of undefined (reading 'begin')".into()
            });
            return Vec::new();
        }
        // Stable: onsets with one begin keep their query order.
        onsets.sort_by_key(|hap| hap.whole.map(|whole| whole.begin));
    }
    onsets
        .into_iter()
        .enumerate()
        .filter_map(|(index, hap)| {
            let part = state.span.intersection(&hap.part)?;
            let mut indexed = hap.with_value(|_| Value::F64(index as f64));
            indexed.part = part;
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

/// [`signal`] of numbers for a body that captures nothing: one closure
/// object per body, so the same signal built by a later evaluation is the
/// same payload, as [`interned_closure`] gives. [`number_signal`] reads the
/// body back.
pub fn signal_static(body: NumberSignal) -> Pattern {
    let sampled = || -> SignalFn {
        Arc::new(move |state: &State| Value::F64(body(state.span.begin, &state.controls)))
    };
    // The bodies are functions of the program, so the list is short and
    // stays so.
    let interned = match NUMBER_SIGNALS.lock() {
        Ok(mut known) => {
            let found = known
                .iter()
                .find(|(of, _)| std::ptr::fn_addr_eq(*of, body))
                .map(|(_, signal)| Arc::clone(signal));
            found.unwrap_or_else(|| {
                let signal = sampled();
                known.push((body, Arc::clone(&signal)));
                signal
            })
        }
        Err(_) => sampled(),
    };
    Pattern::of(Node::Signal(interned))
}

/// The body of `signal`, when [`signal_static`] made it: the signal then
/// answers the number of the body at the begin of the span, and no more.
pub(crate) fn number_signal(signal: &SignalFn) -> Option<NumberSignal> {
    let known = NUMBER_SIGNALS.lock().ok()?;
    known
        .iter()
        .find(|(_, of)| Arc::ptr_eq(of, signal))
        .map(|(body, _)| *body)
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
            let Some(whole) = hap.whole else {
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

fn slowcat_slice_with_steps(pats: &mut [Option<Pattern>], steps: Option<Fraction>) -> Pattern {
    if pats.len() == 1 {
        return pats[0].take().expect("one slowcat input");
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
        slowcat_slice_with_steps(pats, None)
            .fast(Fraction::int(n as i128))
            .with_steps(Some(Fraction::int(n as i128)))
    } else {
        slowcat_slice(pats)
    }
}

fn slowcat_with_steps(mut pats: Vec<Pattern>, steps: Option<Fraction>) -> Pattern {
    if pats.len() == 1 {
        return pats.pop().expect("one slowcat input");
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
        // The step count is `n`, so the inputs' step LCM is not computed and
        // cannot refuse.
        slowcat_with_steps(pats, None)
            .fast(Fraction::int(n as i128))
            .with_steps(Some(Fraction::int(n as i128)))
    } else {
        slowcat(pats)
    }
}

#[cfg(test)]
mod step_combination_overflow_tests {
    use super::*;

    fn stepped(steps: Fraction) -> Pattern {
        pure(Value::Str("bd".into())).with_steps(Some(steps))
    }

    fn refusal(pattern: &Pattern) -> Option<QueryLimit> {
        pattern
            .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            .err()
    }

    #[test]
    fn stacking_and_catenation_refuse_unrepresentable_lcm() {
        let pair = || vec![stepped(Fraction::int(i128::MAX)), stepped(Fraction::int(2))];
        for (pattern, operation) in [(stack(pair()), "stack"), (slowcat(pair()), "slowcat")] {
            assert_eq!(
                refusal(&pattern),
                Some(QueryLimit::NativeFraction { operation }),
                "{operation} should refuse an unrepresentable step LCM"
            );
        }
    }

    /// `fastcat`'s step count is its length, so an unrepresentable LCM of its
    /// inputs' step counts does not refuse it.
    #[test]
    fn fastcat_plays_inputs_whose_step_lcm_is_unrepresentable() {
        let pattern = fastcat(vec![
            stepped(Fraction::int(i128::MAX)),
            stepped(Fraction::int(2)),
        ]);
        assert_eq!(pattern.steps(), Some(Fraction::int(2)));
        let haps = pattern
            .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            .expect("fastcat plays");
        let wholes: Vec<_> = haps.into_iter().map(|hap| hap.whole).collect();
        assert_eq!(
            wholes,
            [
                Some(TimeSpan::new(Fraction::ZERO, Fraction::new(1, 2))),
                Some(TimeSpan::new(Fraction::new(1, 2), Fraction::ONE)),
            ]
        );
    }

    #[test]
    fn app_both_refuses_unrepresentable_lcm() {
        let pattern = stepped(Fraction::int(i128::MAX))
            .app_both_with(stepped(Fraction::int(2)), |a, _| a.clone());
        assert_eq!(
            refusal(&pattern),
            Some(QueryLimit::NativeFraction {
                operation: "appBoth"
            })
        );
    }

    #[test]
    fn poly_join_refuses_unrepresentable_step_ratio() {
        let pattern = stepped(Fraction::int(i128::MAX))
            .fmap_to_pattern(|_| stepped(Fraction::new(1, 2)))
            .poly_join();
        assert_eq!(
            refusal(&pattern),
            Some(QueryLimit::NativeFraction {
                operation: "polyJoin"
            })
        );
    }

    #[test]
    fn small_step_combinations_keep_their_steps() {
        let pair = || vec![stepped(Fraction::int(3)), stepped(Fraction::int(2))];
        assert_eq!(stack(pair()).steps(), Some(Fraction::int(6)));
        assert_eq!(slowcat(pair()).steps(), Some(Fraction::int(6)));
        assert_eq!(fastcat(pair()).steps(), Some(Fraction::int(2)));
        assert_eq!(
            stepped(Fraction::int(3))
                .app_both_with(stepped(Fraction::int(2)), |a, _| a.clone())
                .steps(),
            Some(Fraction::int(6))
        );
    }
}

#[cfg(test)]
mod fastgap_overflow_tests {
    use super::*;

    /// `fastGap` divides a span by its factor, so a small factor far from
    /// cycle zero can leave the i128 fraction range. No factor and no span
    /// may panic: the query returns its haps or a refusal.
    #[test]
    fn a_hostile_gap_is_refused_or_returned_but_never_panics() {
        for (numer, denom) in [(1, 1), (1, 1_000_000), (1, i128::MAX / 4), (3, 7)] {
            let factor = Fraction::new(numer, denom);
            let pattern = pure(Value::Str("x".into())).fast_gap(factor);
            for from in [0i128, 1, 1_000_000, i128::MAX / 8] {
                let begin = Fraction::int(from);
                let end = begin.checked_add(Fraction::ONE).unwrap_or(begin);
                // The assertion is that this returns at all.
                let _ = pattern.try_query_arc_sorted(begin, end);
            }
        }
    }
}

#[cfg(test)]
mod focus_overflow_tests {
    use super::*;

    /// The two spellings a hostile score can put in `focus("…", "…")`:
    /// `arg_fraction` converts them (each is an in-range `i128` rational),
    /// and their difference is `2^128 − 2`, which no native fraction holds.
    /// The unchecked `e − b` used to panic at score-evaluation time, where
    /// nothing on the producer path catches it.
    #[test]
    fn extreme_bounds_refuse_the_window_subtraction() {
        let b = "-170141183460469231731687303715884105727"
            .parse::<Fraction>()
            .expect("an in-range literal converts");
        let e = "170141183460469231731687303715884105727"
            .parse::<Fraction>()
            .expect("an in-range literal converts");
        let focused = pure(Value::Str("bd".into())).focus(b, e);
        assert_eq!(
            focused
                .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
                .err(),
            Some(QueryLimit::NativeFraction { operation: "focus" }),
            "focus across the whole i128 range must refuse, not panic"
        );
    }

    /// A span of exactly `i128::MIN` is representable, but its reciprocal is
    /// `−1/2^127`, whose denominator no `i128/i128` fraction holds: `div` is
    /// a component swap for a numerator of one, and the swapped denominator
    /// is the one magnitude past `i128::MAX`. The unchecked reciprocal
    /// panicked there.
    #[test]
    fn a_min_width_window_refuses_the_reciprocal() {
        let focused =
            pure(Value::Str("bd".into())).focus(Fraction::int(i128::MAX), Fraction::int(-1));
        assert_eq!(
            focused
                .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
                .err(),
            Some(QueryLimit::NativeFraction { operation: "focus" }),
            "the reciprocal of an i128::MIN-wide window must refuse, not panic"
        );
    }

    /// `b` at `i128::MIN` shifts by a whole cycle of `i128::MIN`. `early(t)`
    /// is `late(t.neg())`, and `i128::MIN` has no `i128` negation. The shift
    /// must not panic.
    #[test]
    fn a_min_whole_cycle_shift_refuses_the_negation() {
        let focused =
            pure(Value::Str("bd".into())).focus(Fraction::int(i128::MIN), Fraction::int(-1));
        assert_eq!(
            focused
                .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
                .err(),
            Some(QueryLimit::NativeFraction { operation: "focus" }),
            "shifting by a whole i128::MIN cycle must refuse, not panic"
        );
    }

    /// The ordinary windows the reference documents keep their events and
    /// their refusal-free queries.
    #[test]
    fn ordinary_windows_still_focus_their_events() {
        let bd = pure(Value::Str("bd".into()));
        let halved = bd
            .focus(Fraction::ZERO, Fraction::new(1, 2))
            .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            .expect("focus(0, 1/2) is ordinary");
        assert_eq!(halved.len(), 2, "focus(0, 1/2) is fast(2)");
        let focused = bd
            .focus(Fraction::new(1, 4), Fraction::new(3, 4))
            .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            .expect("focus(1/4, 3/4) is ordinary");
        // The window is half a cycle wide, so its content repeats twice per
        // cycle, and the copy straddling the query start from the previous
        // cycle covers `[0, 1/4)`: bd sounds across the whole cycle, thrice.
        assert_eq!(focused.len(), 3, "the middle half loops to fill the cycle");
        for hap in halved.iter().chain(focused.iter()) {
            assert_eq!(hap.value, Value::Str("bd".into()));
        }
    }
}

#[cfg(test)]
mod time_mapping_overflow_tests {
    use super::*;

    /// One large factor or shift is legal, but a chain of two leaves the
    /// i128 range at query time. Each chain must return the typed
    /// `NativeFraction` refusal instead of a panic.
    #[test]
    fn chained_time_factors_refuse_instead_of_panicking() {
        let bd = || pure(Value::Str("bd".into()));
        let factor = Fraction::from_f64(1e30).expect("1e30 is inside the conversion bound");
        let shift = Fraction::from_f64(1.7e38).expect("1.7e38 is inside the conversion bound");
        let twice_hurried = {
            let once = combinators::hurry(&pure(Value::Str("c".into())), 1e30);
            combinators::hurry(&once, 1e30)
        };
        for (built, operation) in [
            (bd().fast(factor).fast(factor), "fast"),
            // `hurry` is `fast` plus a speed control; the temporal half is
            // what overflows, so the refusal names it.
            (twice_hurried, "fast"),
            (bd().late(shift).late(shift), "late"),
            (bd().early(shift).early(shift), "early"),
            // `slow` is `fast` by the reciprocal; the refusal names the
            // operation the score wrote.
            (
                bd().slow(Fraction::ONE.div(factor))
                    .slow(Fraction::ONE.div(factor)),
                "slow",
            ),
        ] {
            assert!(
                matches!(
                    built.try_query_arc_sorted(Fraction::ZERO, Fraction::ONE),
                    Err(QueryLimit::NativeFraction {
                        operation: got
                    }) if got == operation
                ),
                "the chained {operation} must refuse through the typed channel"
            );
        }
    }

    /// A zoom slot built from exact string literals at ±(2^127 - 1) has a
    /// width (`2^128 - 2`) no i128 fraction can hold. The `e.sub(s)` at
    /// construction used to panic on it; the slot must instead build and
    /// refuse when queried.
    #[test]
    fn an_extreme_zoom_slot_refuses_instead_of_panicking() {
        let begin: Fraction = "-170141183460469231731687303715884105727"
            .parse()
            .expect("-i128::MAX parses exactly");
        let end: Fraction = "170141183460469231731687303715884105727"
            .parse()
            .expect("i128::MAX parses exactly");
        let zoomed = pure(Value::Str("bd".into())).zoom(begin, end);
        assert!(
            matches!(
                zoomed.try_query_arc_sorted(Fraction::ZERO, Fraction::ONE),
                Err(QueryLimit::NativeFraction { operation: "zoom" })
            ),
            "the unrepresentable zoom slot must refuse through the typed channel"
        );
    }

    /// Single arguments at the i128 edge, which a score reaches from a plain
    /// number (`Fraction::from_f64(-2^127)` is exactly `i128::MIN`), must
    /// build without a panic and refuse when queried.
    #[test]
    fn edge_arguments_refuse_at_query_time_instead_of_panicking() {
        let bd = || pure(Value::Str("bd".into()));
        let min = Fraction::from_f64(-(2f64.powi(127))).expect("-2^127 converts exactly");
        assert_eq!(min, Fraction::int(i128::MIN));
        let max = Fraction::int(i128::MAX);
        for (built, operation) in [
            (bd().early(min), "early"),
            (bd().slow(min), "slow"),
            (bd().focus(min, max), "focus"),
        ] {
            assert!(
                matches!(
                    built.try_query_arc_sorted(Fraction::ZERO, Fraction::ONE),
                    Err(QueryLimit::NativeFraction {
                        operation: got
                    }) if got == operation
                ),
                "{operation} at the i128 edge must refuse through the typed channel"
            );
        }
        // Ordinary arguments on the same paths are untouched.
        let slowed = bd()
            .slow(Fraction::int(2))
            .try_query_arc_sorted(Fraction::ZERO, Fraction::int(2))
            .expect("slow(2) is legal");
        assert_eq!(slowed.len(), 1, "slow(2) lost or duplicated events");
        let focused = bd()
            .focus(Fraction::ZERO, Fraction::new(1, 2))
            .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            .expect("focus(0, 1/2) is legal");
        assert_eq!(focused.len(), 2, "focus(0, 1/2) lost or duplicated events");
    }

    /// The single `fast(1e30)` keeps its existing behavior: the span it
    /// requests is refused by the cycle-count guard (`MAX_QUERY_SPAN_CYCLES`),
    /// not by fraction overflow - the mapping itself is representable.
    #[test]
    fn a_single_huge_fast_keeps_its_cycle_span_refusal() {
        let factor = Fraction::from_f64(1e30).expect("1e30 is inside the conversion bound");
        let once = pure(Value::Str("bd".into())).fast(factor);
        assert!(
            matches!(
                once.try_query_arc_sorted(Fraction::ZERO, Fraction::ONE),
                Err(QueryLimit::QuerySpan { .. })
            ),
            "one legal factor must stay the ordinary span refusal, not a new one"
        );
        // An ordinary factor on the same paths is untouched.
        let doubled = pure(Value::Str("bd".into())).fast(Fraction::int(2));
        let haps = doubled
            .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            .expect("fast(2) is legal");
        assert_eq!(haps.len(), 2, "fast(2) lost or duplicated events");
    }
}

#[cfg(test)]
mod query_in_progress_tests {
    //! `query_in_progress` distinguishes ordinary score construction from callbacks
    //! reached by a query or a construction-time probe.

    use super::*;
    use std::sync::{Arc, Mutex};

    /// Ordinary construction is outside a query. The `stepJoin` cycle-0 probe
    /// counts as a query because it calls the pattern's function. Its boundary
    /// remains active after the carrier's `Pattern::query` call returns.
    #[test]
    fn a_construction_probe_and_a_query_are_in_progress_and_construction_is_not() {
        assert!(!query_in_progress(), "nothing is being queried");
        let seen: Arc<Mutex<Vec<bool>>> = Arc::default();
        let record = seen.clone();
        let carrier = pure(Value::Str("bd".into())).fmap_to_pattern(move |value| {
            record.lock().expect("record").push(query_in_progress());
            pure(value.clone())
        });

        let joined = carrier.try_step_join().expect("stepJoin");
        assert!(!query_in_progress(), "the probe's boundary was left open");
        assert_eq!(
            *seen.lock().expect("seen"),
            [true],
            "the construction-time probe must count as a query"
        );

        let haps = joined.query_arc_sorted(Fraction::ZERO, Fraction::ONE);
        assert!(!haps.is_empty(), "the joined pattern plays");
        assert!(!query_in_progress(), "the query's boundary was left open");
        let seen = seen.lock().expect("seen");
        assert!(
            seen.len() > 1 && seen.iter().all(|inside| *inside),
            "a query's callbacks run inside it: {seen:?}"
        );
    }
}

#[cfg(test)]
mod slowcat_overflow_tests {
    //! `slowcat` answers every query whose shifted times fit the native fraction
    //! range and refuses the rest as `NativeFraction { operation: "slowcat" }`.

    use super::*;

    fn bd_sd() -> Pattern {
        slowcat(vec![
            pure(Value::Str("bd".into())),
            pure(Value::Str("sd".into())),
        ])
    }

    #[test]
    fn a_large_time_denominator_still_gets_its_exact_answer() {
        let denominator = 100_000_000_000_000_000_000_000_000_000_000_000_000i128;
        let begin = Fraction::new(1, denominator);
        let end = Fraction::new(2, denominator);
        let haps = bd_sd()
            .try_query_arc_sorted(begin, end)
            .expect("the exact answer fits");
        assert_eq!(haps.len(), 1);
        assert_eq!(haps[0].value, Value::Str("bd".into()));
        assert_eq!(haps[0].part, TimeSpan::new(begin, end));
        assert_eq!(
            haps[0].whole,
            Some(TimeSpan::new(Fraction::ZERO, Fraction::ONE))
        );
    }

    #[test]
    fn a_hap_shifted_past_the_native_range_refuses() {
        let pat = slowcat(vec![
            pure(Value::Str("bd".into())).slow(Fraction::int(i128::MAX)),
            pure(Value::Str("sd".into())),
        ]);
        assert_eq!(
            pat.try_query_arc_sorted(Fraction::int(2), Fraction::int(3))
                .err(),
            Some(QueryLimit::NativeFraction {
                operation: "slowcat"
            })
        );
    }

    #[test]
    fn ordinary_slowcat_queries_keep_their_cycle_order() {
        let values: Vec<_> = bd_sd()
            .try_query_arc_sorted(Fraction::ZERO, Fraction::int(4))
            .expect("ordinary slowcat query")
            .into_iter()
            .map(|hap| hap.value)
            .collect();
        assert_eq!(
            values,
            ["bd", "sd", "bd", "sd"].map(|value| Value::Str(value.into()))
        );
    }
}

#[cfg(test)]
mod pattern_of_join_resolution_tests {
    use super::*;

    /// `seqPLoop`'s exact graph shape:
    /// `stack(pure(p).compress(a, b), ...).slow(total).innerJoin()`.
    /// The join must resolve through the stack and fastGap wrappers.
    #[test]
    fn a_join_over_stacked_compressed_patterns_resolves_each_window() {
        let bd = pure(Value::Str("bd".into()));
        let cp = pure(Value::Str("cp".into()));
        let sections = stack(vec![
            pure_pattern(bd).compress(Fraction::ZERO, Fraction::new(2, 3)),
            pure_pattern(cp).compress(Fraction::new(1, 3), Fraction::ONE),
        ]);
        let pattern = sections.slow(Fraction::int(3)).inner_join();
        let haps = pattern
            .try_query_arc_sorted(Fraction::ZERO, Fraction::int(4))
            .expect("the seqPLoop shape must not be refused");
        assert!(!haps.is_empty(), "seqPLoop windows must produce haps");
        for hap in &haps {
            assert!(
                matches!(hap.value, Value::Str(_)),
                "a pattern-valued hap leaked through the join: {hap:?}"
            );
        }
        // bd owns [0, 2) and [3, 4) of the four queried cycles; cp owns
        // [1, 3). Both branches must be audible.
        let has_bd = haps.iter().any(|hap| hap.value == Value::Str("bd".into()));
        let has_cp = haps.iter().any(|hap| hap.value == Value::Str("cp".into()));
        assert!(has_bd, "the first seqPLoop section is missing: {haps:?}");
        assert!(has_cp, "the second seqPLoop section is missing: {haps:?}");
    }
}

#[cfg(test)]
mod callback_query_metrics_tests {
    use super::*;

    struct NestedHost;

    impl CallbackHost for NestedHost {
        fn call_value(&self, id: CallbackId, value: &Value) -> Result<Value, String> {
            if id == 0 {
                let _ = host_call_value(1, value);
            } else {
                // The innermost call takes a time that every clock reads.
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Ok(value.clone())
        }

        fn call_query(&self, _id: CallbackId, _state: &State) -> Result<Vec<Hap>, String> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn callback_metrics_count_nested_crossings_without_losing_scope() {
        let host = NestedHost;
        let mut metrics = CallbackQueryMetrics::default();
        with_callback_host(&host, || {
            with_callback_query_metrics_policy(&mut metrics, true, || {
                let value = Value::F64(1.0);
                assert_eq!(host_call_value(0, &value), value);
            });
        });
        assert_eq!(metrics.calls(), 2);
        let kinds = metrics.kind_sample().expect("forced callback-kind sample");
        assert_eq!(kinds.total(), metrics.calls());
        assert_eq!(kinds.get(CallbackQueryKind::Value), 2);
        for kind in CallbackQueryKind::ALL {
            if kind != CallbackQueryKind::Value {
                assert_eq!(kinds.get(kind), 0, "unexpected {kind:?}");
            }
        }
        assert!(metrics.busy_nanos() > 0);
        CALLBACK_QUERY_DEPTH.with(|depth| assert_eq!(depth.get(), 0));
        CALLBACK_QUERY_METRICS.with(|slot| assert!(slot.get().is_none()));
    }

    #[test]
    fn nested_metric_recorders_restore_the_outer_recorder() {
        let host = NestedHost;
        let mut outer = CallbackQueryMetrics::default();
        let mut inner = CallbackQueryMetrics::default();
        with_callback_host(&host, || {
            with_callback_query_metrics_policy(&mut outer, true, || {
                let value = Value::F64(1.0);
                let _ = host_call_value(1, &value);
                with_callback_query_metrics_policy(&mut inner, true, || {
                    let _ = host_call_value(1, &value);
                });
                let _ = host_call_value(1, &value);
            });
        });
        assert_eq!(outer.calls(), 2);
        assert_eq!(inner.calls(), 1);
        assert_eq!(
            outer
                .kind_sample()
                .expect("outer kind sample")
                .get(CallbackQueryKind::Value),
            2
        );
        assert_eq!(
            inner
                .kind_sample()
                .expect("inner kind sample")
                .get(CallbackQueryKind::Value),
            1
        );
    }

    #[test]
    fn an_unsampled_kind_census_keeps_the_exact_aggregate() {
        let host = NestedHost;
        let mut metrics = CallbackQueryMetrics::default();
        with_callback_host(&host, || {
            with_callback_query_metrics_policy(&mut metrics, false, || {
                let _ = host_call_value(1, &Value::F64(1.0));
            });
        });
        assert_eq!(metrics.calls(), 1);
        assert!(metrics.kind_sample().is_none());
    }

    #[test]
    fn each_host_call_reads_the_installed_host_and_counts_its_kind() {
        let host = NestedHost;
        let mut metrics = CallbackQueryMetrics::default();
        let value = Value::F64(1.0);
        let haps = pure(value.clone()).query_arc(Fraction::ZERO, Fraction::ONE);
        let span = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
        let program = callback_ir::PatternTransformProgram::new([]).unwrap();
        with_callback_host(&host, || {
            with_callback_query_metrics_policy(&mut metrics, true, || {
                let _ = host_call_value(1, &value);
                let _ = host_call_value_predicate(1, &value);
                let _ = host_call_ref(1);
                let _ = host_call_pick_lookup(1);
                let _ = host_call_pattern(1, pure(value.clone()));
                let _ = host_call_pattern_ir(1, &program, pure(value.clone()));
                let _ = host_call_pattern_indexed_batch(1, vec![(pure(value.clone()), 0)]);
                let _ = host_call_haps(1, &haps);
                let _ = host_call_hap_predicate(1, &haps[0]);
                let _ = host_call_time_predicate(1, Fraction::ZERO);
                let _ = host_call_span_transform(1, span);
                let _ = host_call_bind(1, BindArg::Value(&value));
                let _ = host_call_query(1, &State::new(span));
            });
        });
        let kinds = metrics.kind_sample().expect("forced callback-kind sample");
        for kind in CallbackQueryKind::ALL {
            let expected = match kind {
                // The value call and the value predicate share this kind.
                CallbackQueryKind::Value => 2,
                CallbackQueryKind::MaterializeValue => 0,
                _ => 1,
            };
            assert_eq!(kinds.get(kind), expected, "{kind:?}");
        }
    }
}

#[cfg(test)]
mod set_lookup_tests {
    use super::*;

    fn object(entries: &[(&str, Value)]) -> Value {
        Value::object(
            entries
                .iter()
                .map(|(key, value)| ((*key).into(), value.clone())),
        )
    }

    fn entries(lookup: &PickLookup) -> Vec<(String, Value)> {
        let PickLookup::Object { entries, .. } = lookup else {
            panic!("a set lookup is an object");
        };
        entries
            .iter()
            .map(|(key, pattern)| (key.to_string(), pattern.as_pure().expect("a pure entry")))
            .collect()
    }

    #[test]
    fn a_skipped_set_lookup_equals_the_lookup_of_the_merged_value() {
        let members = [
            Value::F64(0.0),
            Value::F64(-0.0),
            Value::F64(1.0),
            Value::Bool(true),
            Value::Str("x".into()),
            Value::list(vec![Value::F64(-0.0)]),
        ];
        let mut values = vec![Value::Undefined, Value::Null, Value::Str("ab".into())];
        values.extend(members.iter().cloned());
        for key in ["value", "0", "a"] {
            for member in &members {
                values.push(object(&[(key, member.clone())]));
            }
        }
        for first in &members {
            for second in &members {
                values.push(object(&[("value", first.clone()), ("a", second.clone())]));
            }
        }

        let span = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
        let mut skipped = 0;
        for left in &values {
            for right in &values {
                let left_hap = Hap::new(Some(span), span, left.clone());
                let right_hap = Hap::new(Some(span), span, right.clone());
                for merged in [
                    controls::set_value(left, right),
                    compose::compose_op(compose::ComposeOp::Set, left, right),
                ] {
                    if !set_lookup_follows_value(&merged, &left_hap, &right_hap) {
                        continue;
                    }
                    skipped += 1;
                    // The lookup the merge stored before, then the one a pick derives.
                    let Some(eager) = (merged != *right)
                        .then(|| PickLookup::set_object(&left_hap, &right_hap, &merged))
                        .flatten()
                    else {
                        continue;
                    };
                    let derived = PickLookup::from_value(&merged).expect("an object value");
                    assert_eq!(eager.enumerable_len(), derived.enumerable_len());
                    let (eager, derived) = (entries(&eager), entries(&derived));
                    assert_eq!(eager.len(), derived.len());
                    for ((eager_key, eager_value), (key, value)) in eager.iter().zip(&derived) {
                        assert!(
                            eager_key == key && same_key(eager_value, value),
                            "{left:?} set {right:?}: {eager_key} {eager_value:?}, {key} {value:?}"
                        );
                    }
                }
            }
        }
        assert!(skipped > 1_000, "only {skipped} merges skipped the lookup");
    }
}

#[cfg(test)]
mod stable_sort_tests {
    use super::*;

    fn hap(begin: i128, end: i128, label: &str) -> Hap {
        let span = TimeSpan::new(Fraction::new(begin, 4), Fraction::new(end, 4));
        Hap::new(Some(span), span, Value::Str(label.into()))
    }

    fn labels(haps: &[Hap]) -> Vec<String> {
        haps.iter().map(|hap| hap.value.show()).collect()
    }

    #[test]
    fn equal_haps_keep_their_order_and_every_hap_moves_once() {
        let haps = vec![
            hap(2, 3, "c"),
            hap(0, 1, "a1"),
            hap(1, 2, "b"),
            hap(0, 1, "a2"),
            hap(0, 2, "long"),
        ];
        let sorted = sort_haps_for_query(haps);
        assert_eq!(labels(&sorted), ["a1", "a2", "long", "b", "c"]);
        let already = sort_haps_for_query(sorted);
        assert_eq!(labels(&already), ["a1", "a2", "long", "b", "c"]);
    }

    #[test]
    fn many_haps_sort_as_their_times_order_them() {
        // Times that floats tell apart, times with the same float, and
        // times past what a float holds exactly.
        let far = 1_i128 << 60;
        let times = |index: i128| match index % 4 {
            0 => Fraction::new(index * 7 % 23, 5),
            1 => Fraction::new(far + index % 3, far),
            2 => Fraction::new(far * 3 + index % 5, 3),
            _ => Fraction::new(index % 9, 4),
        };
        // Two times that their floats put in the other order: the first
        // is the later one, and its float is the lower one.
        let later = Fraction::new(far + 128, far);
        let earlier = Fraction::new(far + 200, far + 100);
        assert!(later > earlier && later.to_f64() < earlier.to_f64());
        let haps: Vec<Hap> = (0..64)
            .map(times)
            .chain([later, earlier])
            .zip(0_i32..)
            .map(|(begin, index)| {
                let width = Fraction::new(i128::from(index % 3 + 1), 7);
                let span = TimeSpan::new(begin, begin + width);
                Hap::new(Some(span), span, Value::F64(f64::from(index)))
            })
            .collect();
        let mut expected = haps.clone();
        expected.sort_by(query_order);
        assert_eq!(labels(&sort_haps_for_query(haps)), labels(&expected));
    }
}

#[cfg(test)]
mod constant_values_tests {
    use super::*;

    fn half(n: i128) -> Fraction {
        Fraction::new(n, 2)
    }

    fn span(begin: Fraction, end: Fraction) -> TimeSpan {
        TimeSpan::new(begin, end)
    }

    fn summed(functions: Pattern) -> Pattern {
        let add = |a: &Value, b: &Value| match (a, b) {
            (Value::F64(a), Value::F64(b)) => Value::F64(a + b),
            _ => Value::Undefined,
        };
        functions.app_left_by(
            pure(Value::F64(10.0)),
            Combine::with(Arc::new(add)),
            LookupFlow::Infer,
        )
    }

    fn meetings(
        pattern: &Pattern,
        begin: Fraction,
        end: Fraction,
    ) -> Vec<(TimeSpan, TimeSpan, f64)> {
        pattern
            .query_arc(begin, end)
            .into_iter()
            .map(|hap| {
                (
                    hap.whole.expect("a discrete hap"),
                    hap.part,
                    hap.value.as_f64().expect("a number"),
                )
            })
            .collect()
    }

    #[test]
    fn a_constant_value_side_meets_each_function_hap_once_per_cycle() {
        let inside = summed(pure(Value::F64(1.0)));
        assert_eq!(
            meetings(&inside, Fraction::ZERO, half(1)),
            [(
                span(Fraction::ZERO, Fraction::ONE),
                span(Fraction::ZERO, half(1)),
                11.0
            )]
        );
        // A function hap across cycle 1 meets the value hap of each cycle it
        // covers, and keeps its own whole.
        let across = summed(pure(Value::F64(1.0)).late(half(1)));
        assert_eq!(
            meetings(&across, half(1), half(3)),
            [
                (span(half(1), half(3)), span(half(1), Fraction::ONE), 11.0),
                (span(half(1), half(3)), span(Fraction::ONE, half(3)), 11.0),
            ]
        );
    }
}

#[cfg(test)]
mod constant_run_tests {
    use super::*;
    use crate::compose::{Alignment, ComposeOp, compose};

    fn third(n: i128) -> Fraction {
        Fraction::new(n, 3)
    }

    pub(super) fn control(key: &str, value: Value, location: Option<(usize, usize)>) -> Pattern {
        let controls = pure(Value::object([(key.to_string(), value)]));
        match location {
            Some(location) => controls.with_added_context(vec![location]),
            None => controls,
        }
    }

    /// `base` with each of `values` applied to it in turn.
    pub(super) fn chain(base: &Pattern, values: &[(ComposeOp, Pattern)]) -> Pattern {
        values.iter().fold(base.clone(), |pattern, (op, values)| {
            compose(&pattern, values, *op, Alignment::In)
        })
    }

    /// The same chain, with each value behind a stack of one. A stack
    /// answers as its pattern does and is not read as constant, so each
    /// application meets its value through a query, one node at a time.
    pub(super) fn chain_of_queries(base: &Pattern, values: &[(ComposeOp, Pattern)]) -> Pattern {
        let queried: Vec<(ComposeOp, Pattern)> = values
            .iter()
            .map(|(op, values)| (*op, stack(vec![values.clone()])))
            .collect();
        chain(base, &queried)
    }

    /// Everything a meeting decides of each hap `pattern` answers.
    pub(super) fn answers(pattern: &Pattern, begin: Fraction, end: Fraction) -> Vec<String> {
        pattern
            .query_arc(begin, end)
            .iter()
            .map(|hap| {
                format!(
                    "{hap:?} {:?} {} {} {} {}",
                    hap.live_controls(),
                    hap.slider_binding(),
                    hap.ui_visuals_context(),
                    hap.keys.is_empty(),
                    hap.pick_lookup().is_some()
                )
            })
            .collect()
    }

    pub(super) fn bases() -> Vec<(&'static str, Pattern)> {
        let note = |n: f64| pure(Value::object([("note".to_string(), Value::F64(n))]));
        vec![
            ("two notes a cycle", fastcat(vec![note(0.0), note(1.0)])),
            (
                "a note of three cycles",
                note(2.0).slow(Fraction::new(3, 1)),
            ),
            (
                "a note with a key of the chain",
                compose(
                    &note(3.0),
                    &control("gain", Value::F64(0.1), Some((1, 2))),
                    ComposeOp::Set,
                    Alignment::In,
                ),
            ),
            ("a number", pure(Value::F64(5.0))),
            ("a string", pure(Value::Str("bd".into()))),
            ("a signal", crate::signal::saw()),
            ("a tagged note", note(4.0).with_ui_visual_slot(3)),
        ]
    }

    fn spans() -> [(Fraction, Fraction); 4] {
        [
            (Fraction::ZERO, Fraction::ONE),
            (Fraction::ZERO, Fraction::new(4, 1)),
            (third(1), third(5)),
            (third(7), third(8)),
        ]
    }

    /// The constants that a pattern meets are one node, as the two numbers
    /// of a `range` are, and a control after them. Two runs of the same
    /// constants at the same places are the same construction. A constant
    /// with a place of its own in the text keeps the node of its
    /// application.
    #[test]
    fn constants_that_a_pattern_meets_are_one_node() {
        let number = |n: f64| pure(Value::F64(n));
        let ranged = |to: f64| {
            let numbers = [
                (ComposeOp::Mul, number(to - 200.0)),
                (ComposeOp::Add, number(200.0)),
            ];
            chain(&crate::signal::saw(), &numbers)
        };
        let pattern = ranged(800.0);
        let Node::Constants(node) = pattern.node_kind() else {
            panic!("a range of a signal is no run of numbers");
        };
        let (base, run) = (&node.0, &node.1);
        assert!(matches!(base.node_kind(), Node::Signal(_)));
        let numbers: Vec<(ComposeOp, f64)> = run.iter().filter_map(RunMeeting::number).collect();
        assert_eq!(numbers, [(ComposeOp::Mul, 600.0), (ComposeOp::Add, 200.0)]);
        assert!(pattern.same_construction(&ranged(800.0)));
        assert!(!pattern.same_construction(&ranged(801.0)));
        // A `range` makes the run with no pattern of a number, and answers
        // as its two compositions do. A constant folds each number in.
        let range = |pattern: &Pattern| crate::combinators::range(pattern, 200.0, 800.0);
        let direct = range(&crate::signal::saw());
        assert!(pattern.same_construction(&direct));
        let composed = sequence::unfolded(|| range(&crate::signal::saw()));
        assert!(matches!(composed.node_kind(), Node::AppLeft(..)));
        for (begin, end) in spans() {
            assert_eq!(answers(&direct, begin, end), answers(&composed, begin, end));
        }
        let constant = range(&number(0.5));
        assert!(!matches!(constant.node_kind(), Node::Constants(..)));
        assert_eq!(
            answers(&constant, Fraction::ZERO, Fraction::ONE),
            answers(&number(500.0), Fraction::ZERO, Fraction::ONE)
        );

        let located = number(2.0).with_pure_loc((1, 2));
        let applied = compose(
            &crate::signal::saw(),
            &located,
            ComposeOp::Mul,
            Alignment::In,
        );
        assert!(matches!(applied.node_kind(), Node::AppLeft(..)));

        let quiet = |at: (usize, usize)| {
            let gain = control("gain", Value::F64(0.5), Some(at));
            chain(&ranged(800.0), &[(ComposeOp::Set, gain)])
        };
        let set = quiet((3, 6));
        let Node::Constants(node) = set.node_kind() else {
            panic!("a control after a range is no run of constants");
        };
        let (base, run) = (&node.0, &node.1);
        assert!(matches!(base.node_kind(), Node::Signal(_)));
        assert_eq!(run.len(), 3);
        assert!(set.same_construction(&quiet((3, 6))));
        assert!(!set.same_construction(&quiet((4, 7))));
    }

    #[test]
    fn a_chain_of_constants_answers_as_each_application_in_turn() {
        let chains = [
            (
                "controls",
                vec![
                    (
                        ComposeOp::Set,
                        control("s", Value::Str("saw".into()), Some((3, 6))),
                    ),
                    (ComposeOp::Set, control("gain", Value::F64(0.8), None)),
                    (
                        ComposeOp::Set,
                        control("s", Value::Str("sine".into()), Some((10, 14))),
                    ),
                    (
                        ComposeOp::Set,
                        control("cutoff", Value::F64(800.0), Some((20, 23))),
                    ),
                ],
            ),
            (
                "arithmetic",
                vec![
                    (ComposeOp::Mul, pure(Value::F64(600.0))),
                    (ComposeOp::Add, pure(Value::F64(200.0))),
                    (ComposeOp::Sub, pure(Value::F64(1.5))),
                ],
            ),
            (
                "sets on each side of an addition",
                vec![
                    (
                        ComposeOp::Set,
                        control("s", Value::Str("saw".into()), Some((3, 6))),
                    ),
                    (
                        ComposeOp::Set,
                        control("gain", Value::F64(0.5), Some((8, 11))),
                    ),
                    (ComposeOp::Add, control("gain", Value::F64(0.25), None)),
                    (
                        ComposeOp::Set,
                        control("pan", Value::F64(0.5), Some((20, 23))),
                    ),
                    (
                        ComposeOp::Set,
                        control("s", Value::Str("sine".into()), None),
                    ),
                    (
                        ComposeOp::Set,
                        control("room", Value::F64(0.3), Some((30, 33))),
                    ),
                ],
            ),
            (
                "a number among controls",
                vec![
                    (ComposeOp::Set, control("s", Value::Str("saw".into()), None)),
                    // A bare number sets nothing on a control object.
                    (ComposeOp::Set, pure(Value::F64(3.0))),
                    (
                        ComposeOp::Add,
                        control("gain", Value::F64(0.8), Some((4, 7))),
                    ),
                    (ComposeOp::Set, control("pan", Value::F64(0.5), None)),
                ],
            ),
        ];
        for (chain_name, values) in &chains {
            for (base_name, base) in bases() {
                let (run, in_turn) = (chain(&base, values), chain_of_queries(&base, values));
                for (begin, end) in spans() {
                    assert_eq!(
                        answers(&run, begin, end),
                        answers(&in_turn, begin, end),
                        "{chain_name} on {base_name} over {begin:?}..{end:?}"
                    );
                }
            }
        }
    }

    /// A function call gives a handle that binds the runtime of the call,
    /// and each pattern made of it binds that runtime too.
    #[test]
    fn a_chain_reads_through_the_handles_of_the_runtime_in_force_alone() {
        use crate::rng::{self, RngMode};
        use crate::settings::RuntimeSettings;
        let runtime = |mode: RngMode| {
            let settings = RuntimeSettings::default();
            settings.with(|| rng::use_rng(mode));
            settings
        };
        let (one, other) = (runtime(RngMode::Precise), runtime(RngMode::Legacy));
        // A base that tells the runtime in force when a query asks it.
        let told = || {
            fastcat(vec![pure(Value::F64(0.0)), pure(Value::F64(1.0))]).fmap(|_| {
                let mode = Value::Str(format!("{:?}", rng::rng_mode()).into());
                Value::object([("mode".to_string(), mode)])
            })
        };
        let values = [
            (ComposeOp::Set, control("s", Value::Str("saw".into()), None)),
            (
                ComposeOp::Set,
                control("gain", Value::F64(0.8), Some((4, 7))),
            ),
            (ComposeOp::Set, control("pan", Value::F64(0.5), None)),
            (
                ComposeOp::Set,
                control("room", Value::F64(0.3), Some((9, 12))),
            ),
        ];
        type Chain = fn(&Pattern, &[(ComposeOp, Pattern)]) -> Pattern;
        // Each handle of the chain binds the runtime of the base.
        let of_one = |chain: Chain| chain(&told().with_runtime_settings(one.clone()), &values);
        // A handle in the chain binds another runtime than the outermost.
        let of_two = |chain: Chain| {
            let inner = chain(&told(), &values[..2]).with_runtime_settings(other.clone());
            let mut outer = chain(&inner, &values[2..]);
            outer.force_runtime_settings(one.clone());
            outer
        };
        for (name, mode, run, in_turn) in [
            (
                "one runtime",
                "Precise",
                of_one(chain),
                of_one(chain_of_queries),
            ),
            (
                "two runtimes",
                "Legacy",
                of_two(chain),
                of_two(chain_of_queries),
            ),
        ] {
            for (begin, end) in spans() {
                let answered = answers(&run, begin, end);
                assert!(answered.iter().all(|hap| hap.contains(mode)), "{name}");
                assert_eq!(answered, answers(&in_turn, begin, end), "{name}");
            }
        }
    }

    #[test]
    fn a_chain_under_a_queried_value_still_answers_as_one() {
        // The outer application queries its value, so only the two inner
        // applications make a run, under a node that is none.
        let base = fastcat(vec![pure(Value::F64(1.0)), pure(Value::F64(2.0))]);
        let inner = [
            (ComposeOp::Mul, pure(Value::F64(3.0))),
            (ComposeOp::Add, pure(Value::F64(4.0))),
        ];
        let patterned = fastcat(vec![pure(Value::F64(10.0)), pure(Value::F64(20.0))]);
        let outer = |inner: Pattern| compose(&inner, &patterned, ComposeOp::Add, Alignment::In);
        let (run, in_turn) = (
            outer(chain(&base, &inner)),
            outer(chain_of_queries(&base, &inner)),
        );
        for (begin, end) in spans() {
            assert_eq!(answers(&run, begin, end), answers(&in_turn, begin, end));
        }
    }
}

#[cfg(test)]
mod value_signal_tests {
    use super::constant_run_tests::{answers, bases, chain, chain_of_queries, control};
    use super::*;
    use crate::combinators::range;
    use crate::compose::{Alignment, ComposeOp, compose};
    use crate::signal::{saw, sine};

    fn whole(n: i128) -> Fraction {
        Fraction::new(n, 1)
    }

    fn third(n: i128) -> Fraction {
        Fraction::new(n, 3)
    }

    /// The `segment` of a signal answers as the graph of the segment does,
    /// read where it is or under steps of its own, for spans that cut its
    /// pulses and for a rate that is not whole.
    #[test]
    fn a_segmented_signal_walk_answers_as_its_graph() {
        for (name, signal) in signals() {
            for rate in [whole(8), third(8)] {
                let read = combinators::segment(&signal, rate).mark_walked();
                let graph = combinators::segment(&signal.mark_volatile(), rate);
                for (begin, end) in spans() {
                    assert_eq!(
                        answers(&read, begin, end),
                        answers(&graph, begin, end),
                        "{name} at rate {rate} over {begin}..{end}"
                    );
                }
            }
        }
        // A pulse of more cycles than a query may ask refuses as the graph
        // refuses, also under a constant that meets each of its cycles.
        let ranged = range(&sine(), 200.0, 2000.0);
        let slow = Fraction::new(1, MAX_QUERY_SPAN_CYCLES + 5);
        let read = combinators::segment(&ranged, slow).mark_walked();
        let graph = combinators::segment(&ranged.mark_volatile(), slow);
        let span = (whole(0), whole(1));
        assert_eq!(
            format!("{:?}", read.try_query_arc_sorted(span.0, span.1)),
            format!("{:?}", graph.try_query_arc_sorted(span.0, span.1)),
        );
    }

    /// The one walk of the steps of a `segment` answers what the walk of
    /// its graph answers, for each wave under each map, counts that are
    /// powers of two and counts that are not, and spans that are negative,
    /// of no width, between two steps, and in one step or many.
    #[test]
    fn a_fused_segment_walk_answers_as_the_graph_walk() {
        use crate::combinators::rangex;
        type Map = Box<dyn Fn(&Pattern) -> Pattern>;
        let maps: Vec<(&str, Map)> = vec![
            ("bare", Box::new(|signal| signal.clone())),
            ("range", Box::new(|signal| range(signal, 200.0, 2000.0))),
            ("rangex", Box::new(|signal| rangex(signal, 200.0, 2000.0))),
            ("arithmetic", Box::new(arithmetic)),
            ("fast", Box::new(|signal| signal.fast(whole(3)))),
            (
                "slow",
                Box::new(|signal| range(signal, 0.0, 4.0).slow(third(5))),
            ),
            ("late", Box::new(|signal| signal.late(third(1)))),
        ];
        let spans: Vec<(Fraction, Fraction)> = {
            let sixteenth = |n: i128| Fraction::new(n, 16);
            vec![
                (whole(0), whole(1)),
                (whole(0), whole(3)),
                (whole(-2), whole(1)),
                (third(-4), third(-1)),
                (third(-5), third(5)),
                (third(1), third(5)),
                (third(7), third(8)),
                (third(2), third(2)),
                (whole(0), whole(0)),
                (whole(-1), whole(-1)),
                (sixteenth(3), sixteenth(3)),
                (sixteenth(2), sixteenth(2)),
                (sixteenth(5), sixteenth(6)),
                (sixteenth(1), sixteenth(9)),
                (Fraction::new(1, 1000), Fraction::new(3, 1000)),
                (whole(7), Fraction::new(15, 2)),
            ]
        };
        use std::ops::AddAssign;
        let mut ran = std::collections::BTreeMap::new();
        for (wave, signal) in crate::signal::signals() {
            for (map, apply) in &maps {
                let signal = apply(&signal);
                for count in [1, 2, 3, 5, 8, 12, 48, 64, 100] {
                    let rate = whole(count);
                    let read = combinators::segment(&signal, rate).mark_walked();
                    let graph = combinators::segment(&signal.mark_volatile(), rate);
                    for (begin, end) in &spans {
                        let (walked, none) =
                            fused::counting(|| fused::unfused(|| answers(&graph, *begin, *end)));
                        let (fused, hits) = fused::counting(|| answers(&read, *begin, *end));
                        let case = format!("{wave} {map} in {count} steps over {begin}..{end}");
                        assert_eq!(fused, walked, "{case}");
                        assert_eq!(none, fused::Hits::default(), "{case}: the graph ran fused");
                        ran.entry(wave).or_insert(0).add_assign(hits.segment);
                    }
                }
            }
        }
        // The triangle waves are built from a pattern of two ramps, and no
        // signal: the graph answers them.
        let never: Vec<_> = ran
            .iter()
            .filter(|(wave, hits)| **hits == 0 && !wave.contains("tri"))
            .collect();
        assert!(
            never.is_empty(),
            "the walk of the steps never ran for {never:?}"
        );
    }

    /// A segment pattern, made again for each case. A control of a segment
    /// that is to be read as a wave is made again until it is: the name of a
    /// control comes from a pool that another test can leave out of reach.
    fn built(make: &dyn Fn() -> Pattern, as_steps: bool) -> Pattern {
        let sets = Combine::Composed(ComposeOp::Set);
        for _ in 0..200 {
            let pattern = make();
            if !as_steps || fused::StepWave::of(&pattern, &sets, LookupFlow::Set).is_some() {
                return pattern;
            }
            std::thread::yield_now();
        }
        panic!("a control of a segment is not read as a control of a wave");
    }

    /// A segment on the value side of a meeting answers as a query of it
    /// for each hap of the other side does: the haps of the steps that the
    /// function hap meets, cut to it, under each control and map.
    #[test]
    fn a_segmented_value_side_answers_as_a_query_of_it_for_each_hap() {
        type Make = Box<dyn Fn() -> Pattern>;
        let rand = crate::signal::rand;
        let segments: Vec<(&str, bool, Make)> = vec![
            (
                "a segment",
                false,
                Box::new(|| combinators::segment(&sine(), whole(8))),
            ),
            (
                "a ranged segment",
                false,
                Box::new(|| range(&combinators::segment(&saw(), whole(5)), 200.0, 2000.0)),
            ),
            (
                "a segment of a ranged signal",
                false,
                Box::new(|| {
                    combinators::segment(&range(&sine(), 0.0, 4.0).slow(third(4)), whole(64))
                }),
            ),
            (
                "a segment of a random signal",
                false,
                Box::new(move || combinators::segment(&rand(), whole(8))),
            ),
            (
                "a slow segment",
                false,
                Box::new(|| combinators::segment(&saw(), whole(3)).slow(whole(2))),
            ),
            (
                "a named segment",
                true,
                Box::new(|| named("cutoff", combinators::segment(&sine(), whole(16)))),
            ),
            (
                "a named ranged segment",
                true,
                Box::new(move || {
                    named(
                        "gain",
                        range(&combinators::segment(&rand(), whole(8)), 0.2, 0.9),
                    )
                }),
            ),
        ];
        let mut total = fused::Hits::default();
        for op in [ComposeOp::Set, ComposeOp::Add, ComposeOp::Mul] {
            for (begin, end) in spans() {
                for ((structure_name, structure), (name, as_steps, make)) in structures()
                    .into_iter()
                    .flat_map(|structure| segments.iter().map(move |s| (structure.clone(), s)))
                {
                    let signal = built(make, *as_steps && op == ComposeOp::Set);
                    let read = compose(&structure, &signal, op, Alignment::In);
                    let queried =
                        compose(&structure, &stack(vec![signal.clone()]), op, Alignment::In);
                    let (graph, none) =
                        fused::counting(|| fused::unfused(|| answers(&queried, begin, end)));
                    let case =
                        format!("{op:?} of {name} on {structure_name} over {begin:?}..{end:?}");
                    assert_eq!(none, fused::Hits::default(), "{case}: the graph ran fused");
                    for read in [read.mark_walked(), read] {
                        let (fused, hits) = fused::counting(|| answers(&read, begin, end));
                        assert_eq!(fused, graph, "{case}");
                        total.segment += hits.segment;
                        total.step_one += hits.step_one;
                        total.step_many += hits.step_many;
                    }
                }
            }
        }
        // The walks ran: each kind of answer was made in some case.
        assert!(total.segment > 0, "{total:?}");
        assert!(total.step_one > 0, "{total:?}");
        assert!(total.step_many > 0, "{total:?}");
    }

    /// A segment charges the hap budget one hap at a time, as the graph
    /// does, also for a signal that makes several haps for a step.
    #[test]
    fn a_segment_refuses_the_hap_budget_as_the_graph_does() {
        let many = range(&sine(), 0.0, 1.0).fast(whole(256));
        let read = combinators::segment(&many, whole(64)).mark_walked();
        let graph = combinators::segment(&many.mark_volatile(), whole(64));
        let ask = |pattern: &Pattern, budget: u64| {
            format!(
                "{:?}",
                pattern.try_query_arc_sorted_with_budget(whole(0), whole(1), budget)
            )
        };
        for budget in [0, 1, 10, 63, 64, 100, 255, 256, 257, 10_000] {
            let (walked, none) = fused::counting(|| fused::unfused(|| ask(&graph, budget)));
            let (fused, hits) = fused::counting(|| ask(&read, budget));
            assert_eq!(fused, walked, "a budget of {budget}");
            assert_eq!(none, fused::Hits::default());
            // A budget under the steps is the graph's, and one over them is
            // the walk's.
            assert_eq!(hits.segment > 0, budget >= 64, "a budget of {budget}");
        }
    }

    /// The walk of the steps stops at the hap that passes the budget, and
    /// leaves no hap, as the graph path of the application does. The walk
    /// is asked as it is, with no query around it that would charge the
    /// haps it holds.
    #[test]
    fn a_segment_walk_charges_each_hap_of_a_step_to_the_budget() {
        let many = range(&sine(), 0.0, 1.0).fast(whole(256));
        let segment = combinators::segment(&many, whole(64));
        let Node::FilterValues(boxed) = segment.node_kind() else {
            panic!("a segment is a filter of an application");
        };
        let structured = &boxed.0;
        let Node::AppRight(node) = structured.node_kind() else {
            panic!("a segment is an application");
        };
        let state = State::new(TimeSpan::new(whole(0), whole(1)));
        for (budget, refused) in [(100, true), (255, true), (256, false), (1000, false)] {
            let mut out = Vec::new();
            let (_, refusal) =
                with_hap_budget(budget, || fused::app_right_onto(node, &state, &mut out));
            assert_eq!(refusal.is_some(), refused, "a budget of {budget}");
            assert_eq!(
                out.len(),
                if refused { 0 } else { 256 },
                "a budget of {budget}"
            );
        }
    }

    /// A segment stops at a requested stop as the graph does, for a span
    /// that holds no multiple of sixteen steps.
    #[test]
    fn a_segment_stops_as_the_graph_does() {
        let read = combinators::segment(&range(&sine(), 200.0, 2000.0), whole(16)).mark_walked();
        let graph = combinators::segment(&range(&sine(), 200.0, 2000.0).mark_volatile(), whole(16));
        let stopped = std::sync::atomic::AtomicBool::new(true);
        for (begin, end) in [
            (Fraction::new(1, 16), whole(1)),
            (Fraction::new(-15, 16), whole(0)),
        ] {
            let ask = |pattern: &Pattern| {
                with_cancellation(&stopped, || {
                    format!("{:?}", pattern.try_query_arc_sorted(begin, end))
                })
            };
            let walked = fused::unfused(|| ask(&graph));
            assert_eq!(ask(&read), walked, "over {begin}..{end}");
        }
    }

    fn signals() -> Vec<(&'static str, Pattern)> {
        let cutoff = |signal: Pattern| {
            signal.fmap(|value| Value::object([("cutoff".to_string(), value.clone())]))
        };
        vec![
            ("a signal", saw()),
            ("a ranged signal", range(&sine(), 200.0, 2000.0)),
            (
                "a slow ranged signal",
                range(&sine(), 200.0, 2000.0).slow(whole(4)),
            ),
            ("a fast late signal", saw().fast(whole(3)).late(third(1))),
            (
                "a control of a slow signal",
                cutoff(range(&saw(), 0.0, 1.0).slow(whole(2))),
            ),
            (
                "a signal with a place in the text",
                cutoff(sine()).with_added_context(vec![(4, 8)]),
            ),
            // A control by its name sets its value where the hap is.
            (
                "a named control",
                named("cutoff", range(&sine(), 200.0, 2000.0)),
            ),
            ("a named gain", named("gain", saw().slow(whole(3)))),
            (
                "a named control with a place in the text",
                named("pan", sine().with_added_context(vec![(4, 8)])),
            ),
            ("a named note", named("note", saw().fast(whole(2)))),
            // A control of several names reads a number as its first name.
            (
                "a named control of several names",
                crate::controls::ControlSpec::new(["cutoff", "resonance"].map(Arc::from))
                    .pattern(&range(&sine(), 200.0, 2000.0)),
            ),
            ("a control named value", named("value", saw())),
            // A signal of numbers gives each number with no hap of its own.
            (
                "a named ranged signal, slowed",
                named("cutoff", range(&sine(), 200.0, 2000.0).slow(whole(4))),
            ),
            (
                "a named slow signal, ranged",
                named("cutoff", range(&saw().slow(third(4)), 0.5, 4.0)),
            ),
            (
                "a named late signal of arithmetic",
                named("gain", arithmetic(&saw().late(whole(2)).fast(third(1)))),
            ),
            (
                "a named random signal",
                named(
                    "pan",
                    range(&crate::signal::rand(), -1.0, 1.0).late(whole(1)),
                ),
            ),
            // A number of these may cut the hap of the signal inside a
            // cycle of the query: the graph answers a part that it cuts.
            (
                "a named ranged signal, slowed by a part of a cycle",
                named("cutoff", range(&sine(), 200.0, 2000.0).slow(third(4))),
            ),
            (
                "a named ranged signal, sped up",
                named("cutoff", range(&saw(), 0.0, 8.0).fast(whole(3))),
            ),
            (
                "a named ranged signal, late by a part of a cycle",
                named("gain", range(&saw(), 0.0, 8.0).late(third(1))),
            ),
            (
                "a named signal with a number at a place in the text",
                named(
                    "gain",
                    compose(
                        &sine(),
                        &pure(Value::F64(3.0)).with_added_context(vec![(2, 3)]),
                        ComposeOp::Mul,
                        Alignment::In,
                    ),
                ),
            ),
        ]
    }

    /// `signal` under each op of arithmetic, with a number for each.
    fn arithmetic(signal: &Pattern) -> Pattern {
        use ComposeOp::{Add, Band, Blshift, Bor, Brshift, Bxor, Div, Mod, Mul, Pow, Sub};
        [
            Mul, Add, Sub, Div, Pow, Mod, Blshift, Bor, Bxor, Band, Brshift,
        ]
        .into_iter()
        .zip([9.5, 1.25, 0.75, 0.5, 1.5, 7.0, 2.0, 5.0, 9.0, 14.0, 1.0])
        .fold(signal.clone(), |signal, (op, number)| {
            compose(&signal, &pure(Value::F64(number)), op, Alignment::In)
        })
    }

    /// `signal` as the values of the control `name`.
    fn named(name: &str, signal: Pattern) -> Pattern {
        crate::controls::ControlSpec::new([Arc::from(name)]).pattern(&signal)
    }

    fn structures() -> Vec<(&'static str, Pattern)> {
        let note = |n: f64| pure(Value::object([("note".to_string(), Value::F64(n))]));
        // Notes that sliders drive, as a control of a slider makes them.
        let driven = Pattern::of(Node::MapHapsOwned(Box::new((
            fastcat(vec![note(3.0), note(4.0), note(5.0)]),
            Arc::new(|mut hap: Hap| {
                hap.set_live_bindings([9, 8, 7, 6], 5);
                Some(hap)
            }),
            SpanUse::Ignores,
        ))));
        // A number that equals no number, with a lookup: a meeting keeps
        // the lookup of a value that equals itself.
        let looked_up = Pattern::of(Node::MapHapsOwned(Box::new((
            fastcat(vec![pure(Value::F64(f64::NAN)), pure(Value::F64(2.0))]),
            Arc::new(|hap: Hap| {
                let lookup = PickLookup::Array {
                    enumerable_len: 0,
                    length: 0,
                    entries: Vec::new(),
                };
                Some(hap.with_pick_lookup(Arc::new(lookup)))
            }),
            SpanUse::Ignores,
        ))));
        // Notes whose wholes begin after their parts: a meeting cuts each
        // part to the time that the whole holds too.
        let late = Pattern::of(Node::MapHapsOwned(Box::new((
            fastcat(vec![note(6.0), note(7.0), note(8.0)]),
            Arc::new(|mut hap: Hap| {
                let whole = hap.whole?;
                let quarter = (whole.end - whole.begin) * Fraction::new(1, 4);
                hap.whole = Some(TimeSpan::new(whole.begin + quarter, whole.end));
                Some(hap)
            }),
            SpanUse::Reads,
        ))));
        vec![
            ("two notes a cycle", fastcat(vec![note(0.0), note(1.0)])),
            ("notes that sliders drive", driven),
            ("numbers with a lookup", looked_up),
            ("notes with late wholes", late),
            (
                "eight notes a cycle",
                fastcat((0..8).map(|n| note(f64::from(n))).collect()),
            ),
            ("a note of three cycles", note(2.0).slow(whole(3))),
            ("a number", pure(Value::F64(5.0))),
            ("a signal", saw()),
        ]
    }

    fn spans() -> [(Fraction, Fraction); 6] {
        [
            (Fraction::ZERO, Fraction::ONE),
            (Fraction::ZERO, whole(4)),
            (third(1), third(5)),
            (third(7), third(8)),
            (third(2), third(2)),
            (third(-4), third(-1)),
        ]
    }

    #[test]
    fn a_signal_value_side_answers_as_a_query_of_it_for_each_hap() {
        for op in [ComposeOp::Set, ComposeOp::Add, ComposeOp::Mul] {
            for (begin, end) in spans() {
                // Each span asks new graphs, so that the walk answers it and
                // no table does.
                for ((structure_name, structure), (signal_name, signal)) in
                    structures().into_iter().flat_map(|structure| {
                        signals().into_iter().map(move |s| (structure.clone(), s))
                    })
                {
                    let read = compose(&structure, &signal, op, Alignment::In);
                    // A stack answers as its pattern does and is no signal,
                    // so the application queries it for each hap.
                    let queried = compose(&structure, &stack(vec![signal]), op, Alignment::In);
                    // A graph that is marked as walked reads the signal in
                    // its walk. Any other graph may read it in a table.
                    for read in [read.mark_walked(), read] {
                        assert_eq!(
                            answers(&read, begin, end),
                            answers(&queried, begin, end),
                            "{op:?} of {signal_name} on {structure_name} over {begin:?}..{end:?}"
                        );
                    }
                }
            }
        }
    }

    /// The gate of a degrade says whether each hap stays, as a query of
    /// the gate for the hap says it: for the coin of `degradeBy` and of
    /// `undegradeBy`, for a wave of other speeds and numbers, and for a
    /// signal that is no wave.
    #[test]
    fn a_degrade_keeps_the_haps_that_a_query_of_its_gate_passes() {
        use crate::combinators::{degrade_by, degrade_by_with, undegrade_by};
        use crate::signal::{rand, rand_inverted};
        let coins: Vec<(&str, Pattern)> = vec![
            ("the coin", rand()),
            ("the other side of the coin", rand_inverted()),
            ("a slow late coin", rand().slow(whole(3)).late(third(1))),
            ("a ranged coin", range(&rand(), 0.2, 0.9).fast(whole(2))),
            ("a sine", sine()),
            (
                "a map of a coin",
                rand().fmap(|value| Value::F64(value.as_f64().unwrap_or(0.0) * 0.75)),
            ),
        ];
        let seeds = [None, Some(3.5)];
        for (structure_name, structure) in structures() {
            for (coin_name, coin) in &coins {
                for amount in [0.0, 0.3, 0.5, 0.9] {
                    for ((begin, end), seed) in spans().into_iter().zip(seeds.into_iter().cycle()) {
                        let read = degrade_by_with(&structure, coin, amount);
                        // A stack of the coin is no signal, so the degrade
                        // queries it for each hap.
                        let queried =
                            degrade_by_with(&structure, &stack(vec![coin.clone()]), amount);
                        let asked = |pattern: &Pattern| {
                            let mut state = State::new(TimeSpan::new(begin, end));
                            if let Some(seed) = seed {
                                state.controls.insert("randSeed".into(), Value::F64(seed));
                            }
                            let haps = pattern.query_state(&state);
                            let bound = |hap: &Hap| {
                                let lookup = hap.pick_lookup().is_some();
                                format!("{hap:?} {:?} {lookup}", hap.live_controls())
                            };
                            haps.iter().map(bound).collect::<Vec<_>>()
                        };
                        let name = format!(
                            "{coin_name} above {amount} on {structure_name} over \
                             {begin:?}..{end:?}"
                        );
                        for read in [read.mark_walked(), read] {
                            assert_eq!(asked(&read), asked(&queried), "{name}");
                        }
                        // The gate under another meeting gives its number.
                        let gate_of = |degrade: &Pattern| match degrade.node_kind() {
                            Node::AppLeft(pair, ..) => pair.1.clone(),
                            _ => panic!("a degrade is an application"),
                        };
                        let added = |degrade: &Pattern| {
                            let gate = gate_of(degrade);
                            compose(&structure, &gate, ComposeOp::Add, Alignment::In).mark_walked()
                        };
                        let again = degrade_by_with(&structure, coin, amount);
                        assert_eq!(asked(&added(&again)), asked(&added(&queried)), "{name}");
                    }
                }
            }
            // The two degrades of a score are the two sides of one coin.
            for (begin, end) in spans() {
                let kept = answers(&degrade_by(&structure, 0.4).mark_walked(), begin, end);
                let dropped = answers(&undegrade_by(&structure, 0.6).mark_walked(), begin, end);
                // Each hap that a degrade can keep: a coin is above -1.
                let all = answers(&degrade_by(&structure, -1.0).mark_walked(), begin, end);
                assert_eq!(kept.len() + dropped.len(), all.len(), "{structure_name}");
                assert!(
                    kept.iter().all(|hap| !dropped.contains(hap)),
                    "{structure_name}"
                );
            }
        }
    }

    /// A function call gives a handle that binds the runtime of the call.
    /// A constant after a wave of such a handle goes under the wave, and
    /// the application keeps its runtime.
    #[test]
    fn a_constant_under_a_wave_keeps_the_runtime_that_the_wave_binds() {
        use crate::rng::{self, RngMode};
        use crate::settings::RuntimeSettings;
        let runtime = |mode: RngMode| {
            let settings = RuntimeSettings::default();
            settings.with(|| rng::use_rng(mode));
            settings
        };
        let gain = || {
            [(
                ComposeOp::Set,
                control("gain", Value::F64(0.5), Some((4, 7))),
            )]
        };
        // The coin of a runtime is the coin of its random numbers.
        let coin = || [(ComposeOp::Set, named("pan", crate::signal::rand()))];
        type Chain = fn(&Pattern, &[(ComposeOp, Pattern)]) -> Pattern;
        let of = |settings: &RuntimeSettings, chain: Chain| {
            let swept = chain(&steps_of_other_names(), &coin());
            chain(&swept.with_runtime_settings(settings.clone()), &gain())
        };
        let runtimes = [runtime(RngMode::Precise), runtime(RngMode::Legacy)];
        for settings in &runtimes {
            let set = of(settings, chain);
            let inner = match set.node_kind() {
                Node::AppLeft(pair, ..) => &pair.0,
                Node::AppWave(app) => &app.inner,
                _ => panic!("the application of the wave is the last one"),
            };
            assert!(matches!(inner.node_kind(), Node::Sequence(_)));
            for (begin, end) in spans() {
                assert_eq!(
                    answers(&set, begin, end),
                    answers(&of(settings, chain_of_queries), begin, end),
                    "{begin:?}..{end:?}"
                );
            }
        }
        // Steps of another runtime than the wave over them keep the
        // constant over the wave, which reads its coin under its own.
        let [one, other] = &runtimes;
        let of_two = |chain: Chain| {
            let steps = steps_of_other_names().with_runtime_settings(other.clone());
            let mut swept = chain(&steps, &coin());
            swept.force_runtime_settings(one.clone());
            chain(&swept, &gain())
        };
        for (begin, end) in spans() {
            assert_eq!(
                answers(&of_two(chain), begin, end),
                answers(&of_two(chain_of_queries), begin, end),
                "{begin:?}..{end:?}"
            );
        }
        let [one, other] =
            runtimes.map(|settings| answers(&of(&settings, chain), whole(0), whole(1)));
        assert_ne!(one, other, "each runtime has its own coin");
    }

    /// Chains of controls with a wave among them.
    fn chains_with_waves() -> Vec<(&'static str, Vec<(ComposeOp, Pattern)>)> {
        let wave = |name: &str, signal: Pattern| (ComposeOp::Set, named(name, signal));
        let number = |key: &str, n: f64| (ComposeOp::Set, control(key, Value::F64(n), None));
        let sound = |name: &str, at: (usize, usize)| {
            (
                ComposeOp::Set,
                control("s", Value::Str(name.into()), Some(at)),
            )
        };
        let cutoff = || range(&sine(), 200.0, 2000.0).slow(whole(4));
        vec![
            (
                "constants after a wave",
                vec![
                    wave("cutoff", cutoff()),
                    number("resonance", 8.0),
                    sound("saw", (3, 6)),
                    number("gain", 0.8),
                ],
            ),
            (
                "a constant of the name of the wave",
                vec![
                    wave("cutoff", cutoff()),
                    number("resonance", 8.0),
                    number("cutoff", 300.0),
                    number("gain", 0.8),
                ],
            ),
            (
                "two waves among constants",
                vec![
                    sound("saw", (3, 6)),
                    wave("cutoff", cutoff()),
                    number("note", 7.0),
                    wave("gain", saw().late(whole(1))),
                    number("pan", 0.25),
                    sound("sine", (10, 14)),
                    number("cutoff", 500.0),
                    number("room", 0.5),
                ],
            ),
            (
                "a wave of a name of the base",
                vec![
                    wave("note", saw()),
                    number("gain", 0.5),
                    wave("gain", cutoff()),
                    number("note", 9.0),
                ],
            ),
            (
                "an addition after a wave",
                vec![
                    wave("gain", saw()),
                    (ComposeOp::Add, control("gain", Value::F64(0.25), None)),
                    number("pan", 0.5),
                ],
            ),
            (
                "a constant after a wave that may cut a part",
                vec![
                    wave("cutoff", range(&saw(), 0.0, 8.0).fast(whole(3))),
                    number("resonance", 8.0),
                ],
            ),
            (
                "a value alone after a wave",
                vec![
                    wave("cutoff", cutoff()),
                    number("gain", 0.8),
                    number("value", 2.0),
                    number("pan", 0.25),
                ],
            ),
        ]
    }

    /// A sequence of steps that hold other names, so that a new name finds
    /// its place in each map.
    fn steps_of_other_names() -> Pattern {
        let steps = [
            vec![("note", 0.0), ("gain", 0.3)],
            vec![("cutoff", 90.0), ("note", 1.0)],
            vec![("note", 2.0)],
        ];
        let steps = steps
            .into_iter()
            .zip(0..)
            .map(|(entries, index)| {
                let controls = entries
                    .into_iter()
                    .map(|(key, n)| (key.to_string(), Value::F64(n)));
                SequenceStep::new(
                    third(index),
                    third(index + 1),
                    Value::object(controls),
                    Locations::from(vec![(index as usize, index as usize + 2)]),
                )
            })
            .collect();
        sequence(vec![SequenceLayer {
            period: Fraction::ONE,
            steps,
        }])
        .expect("steps in order")
    }

    #[test]
    fn a_chain_with_a_wave_answers_as_each_application_in_turn() {
        let bases = bases()
            .into_iter()
            .chain([("steps of other names", steps_of_other_names())]);
        for (base_name, base) in bases {
            for (chain_name, values) in chains_with_waves() {
                for (begin, end) in spans() {
                    let queried = chain_of_queries(&base, &values);
                    // Each span asks a new graph, so that the walk answers.
                    let read = chain(&base, &values);
                    for read in [read.mark_walked(), read] {
                        assert_eq!(
                            answers(&read, begin, end),
                            answers(&queried, begin, end),
                            "{chain_name} on {base_name} over {begin:?}..{end:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn constants_after_a_wave_are_set_under_its_application() {
        let (_, values) = chains_with_waves().swap_remove(0);
        let set = chain(&steps_of_other_names(), &values);
        let inner = match set.node_kind() {
            Node::AppLeft(pair) => {
                assert!(matches!(pair.3, Combine::Composed(ComposeOp::Set)));
                &pair.0
            }
            Node::AppWave(app) => &app.inner,
            _ => panic!("the application of the wave is the last one"),
        };
        assert!(
            matches!(inner.node_kind(), Node::Sequence(_)),
            "the steps hold each constant"
        );
    }
}

#[cfg(test)]
mod restart_tests {
    use super::*;

    fn whole(n: i128) -> Fraction {
        Fraction::new(n, 1)
    }

    fn third(n: i128) -> Fraction {
        Fraction::new(n, 3)
    }

    fn note(n: f64) -> Pattern {
        pure(Value::object([("note".to_string(), Value::F64(n))]))
            .with_added_context(vec![(n as usize, n as usize + 1)])
    }

    fn steps(from: i32, count: i32) -> Pattern {
        fastcat((0..count).map(|n| note(f64::from(from + n))).collect())
    }

    #[test]
    fn cpm_keeps_exact_step_times_when_the_scheduler_tempo_changes() {
        let source = stack(vec![steps(0, 16), steps(20, 3), crate::signal::saw()]);
        for cpm in [32, 33] {
            let tempo = source.cpm(whole(cpm)).mark_walked();
            for (cps, denominator) in [(0.5, 30), (0.75, 45), (0.5, 30)] {
                let speed = Fraction::new(cpm, denominator);
                let walked = source
                    .with_query_time(move |t| t * speed)
                    .with_hap_time(move |t| t / speed)
                    .mark_walked();
                for tick in -24..48 {
                    for width in [0, 1, 5, 11] {
                        let span =
                            TimeSpan::new(Fraction::new(tick, 13), Fraction::new(tick + width, 13));
                        let mut state = State::new(span);
                        state.controls.insert("_cps".into(), Value::F64(cps));
                        assert_eq!(
                            format!("{:?}", tempo.query(&state)),
                            format!("{:?}", walked.query(&state)),
                            "{span:?} at {cpm} cpm and {cps} cps"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn speed_and_shift_restore_shared_edges_of_cut_haps() {
        let source = stack(vec![steps(0, 4), steps(10, 3), crate::signal::saw()]);
        for speed in [Fraction::ONE, Fraction::new(11, 10), Fraction::new(2, 3)] {
            for shift in [Fraction::ZERO, whole(-2), Fraction::new(3, 5)] {
                let mapped = source.late(shift).fast(speed).mark_walked();
                let walked = source
                    .with_query_time(move |t| t - shift)
                    .with_hap_time(move |t| t + shift)
                    .with_query_time(move |t| t * speed)
                    .with_hap_time(move |t| t / speed)
                    .mark_walked();
                for tick in -16..32 {
                    for width in [0, 1, 5, 13] {
                        let begin = Fraction::new(tick, 8);
                        let end = Fraction::new(tick + width, 8);
                        assert_eq!(
                            format!("{:?}", mapped.query_arc(begin, end)),
                            format!("{:?}", walked.query_arc(begin, end)),
                            "{begin:?}..{end:?} at {speed:?} shifted by {shift:?}"
                        );
                    }
                }
            }
        }
    }

    /// Sections of discrete haps: steps, stacks, alternations, structures,
    /// speed changes, a coin and controls, with a signal as the value of one.
    fn discrete_sections() -> Vec<Pattern> {
        use crate::compose::{Alignment, ComposeOp, compose};
        let cutoff = crate::signal::saw()
            .slow(whole(3))
            .fmap(|value| Value::object([("cutoff".to_string(), value.clone())]));
        vec![
            steps(0, 4),
            stack(vec![steps(10, 3), steps(20, 2).slow(whole(2))]),
            slowcat(vec![steps(30, 2), note(33.0), steps(34, 3)]).fast(third(2)),
            compose(&steps(40, 8), &cutoff, ComposeOp::Set, Alignment::In),
            crate::combinators::struct_with(
                &steps(50, 2).slow(whole(3)),
                &fastcat(
                    [true, false, true, true]
                        .map(|on| pure(Value::Bool(on)))
                        .to_vec(),
                ),
            )
            .late(third(1)),
            steps(60, 5).degrade_by_seeded(0.5, 7),
            crate::combinators::sometimes_by(&steps(70, 6), 0.5, None),
            picked(
                JoinMode::Restart,
                vec![steps(80, 3), steps(90, 2).slow(whole(2))],
                third(2),
            ),
        ]
    }

    /// Sections with a layer that reads the span: a stack of steps and a
    /// signal, alone and in another stack.
    fn sections_with_a_signal() -> Vec<Pattern> {
        let mixed = stack(vec![steps(100, 4), crate::signal::saw(), steps(110, 3)]);
        vec![mixed.clone(), stack(vec![steps(120, 2), mixed])]
    }

    /// A pick of `sections` in turn, each for `cycles` cycles.
    fn picked(mode: JoinMode, sections: Vec<Pattern>, cycles: Fraction) -> Pattern {
        let count = sections.len();
        let selector = fastcat((0..count).map(|n| pure(Value::F64(n as f64))).collect())
            .slow(cycles * Fraction::int(count as i128));
        pick(
            selector,
            PickLookup::Array {
                enumerable_len: count,
                length: count,
                entries: sections.into_iter().enumerate().collect(),
            },
            PickIndexMode::Clamp,
            mode,
        )
    }

    /// `section` behind a time map that changes no time. The join does not
    /// know the map, so it asks such a section over the span of the query.
    fn asked_over_the_span(section: Pattern) -> Pattern {
        section.with_query_time(|time| time)
    }

    #[test]
    fn restarted_sequence_steps_keep_their_parts_and_clip_their_wholes() {
        let section = || {
            let at = |n| Fraction::new(n, 8);
            let step = SequenceStep::new(at(0), at(8), Value::F64(1.0), vec![(2, 5)].into());
            sequence(vec![SequenceLayer {
                period: whole(2),
                steps: vec![
                    step.placed((at(0), at(6)), (at(1), at(4))),
                    step.placed((at(3), at(8)), (at(4), at(7))),
                ],
            }])
            .expect("ordered steps")
        };
        for mode in [JoinMode::Reset, JoinMode::Restart] {
            let pick = |choice| picked(mode, vec![choice], third(4)).mark_walked();
            let direct = pick(section());
            let graph = pick(asked_over_the_span(section()));
            for start in -36..36 {
                for width in [0, 1, 3, 9, 19] {
                    let state = State::new(TimeSpan::new(
                        Fraction::new(start, 9),
                        Fraction::new(start + width, 9),
                    ));
                    assert_eq!(
                        format!("{:?}", direct.query(&state)),
                        format!("{:?}", graph.query(&state)),
                        "{mode:?} at {start}/9 for {width}/9"
                    );
                }
            }
        }
    }

    #[test]
    fn a_restart_repeats_every_layer_only_at_a_shared_period() {
        let section = || {
            let layer = |period, value| SequenceLayer {
                period,
                steps: vec![SequenceStep::new(
                    Fraction::ZERO,
                    third(2),
                    Value::F64(value),
                    Locations::new(),
                )],
            };
            sequence(vec![layer(whole(2), 1.0), layer(third(2), 2.0)])
                .expect("two layers of different periods")
        };
        for cycles in [whole(2), whole(3), third(5)] {
            let pick = |choice| picked(JoinMode::Restart, vec![choice], cycles).mark_walked();
            let repeated = pick(section());
            let moved = pick(asked_over_the_span(section()));
            for tick in -30..45 {
                for width in [0, 1, 4, 13] {
                    let span = TimeSpan::new(third(tick), third(tick + width));
                    let state = State::new(span);
                    assert_eq!(
                        format!("{:?}", repeated.query(&state)),
                        format!("{:?}", moved.query(&state)),
                        "{span:?} with a restart every {cycles:?} cycles"
                    );
                }
            }
        }
    }

    #[test]
    fn a_restart_asks_discrete_haps_over_their_part_alone() {
        let spans = [
            (Fraction::ZERO, whole(16)),
            (third(-5), third(22)),
            (third(7), third(8)),
            (whole(2), third(7)),
            (third(13), third(29)),
            (third(7), third(7)),
            (whole(14), whole(14)),
        ];
        let sections = || {
            let mut sections = discrete_sections();
            sections.extend(sections_with_a_signal());
            sections
        };
        for mode in [JoinMode::Restart, JoinMode::Reset] {
            for (begin, end) in spans {
                // Each span asks new graphs, so that the walk answers it
                // and no table does. Each section holds two cycles and a
                // third of one.
                let cut = picked(mode, sections(), third(7));
                let whole_span = picked(
                    mode,
                    sections().into_iter().map(asked_over_the_span).collect(),
                    third(7),
                );
                assert_eq!(
                    format!("{:?}", cut.query_arc(begin, end)),
                    format!("{:?}", whole_span.query_arc(begin, end)),
                    "{mode:?} over {begin:?}..{end:?}"
                );
            }
        }
    }

    /// A sequence of `values`, one a cycle.
    fn each_cycle(values: &[f64]) -> Pattern {
        let count = values.len() as i128;
        let steps = values
            .iter()
            .zip(0..)
            .map(|(value, index)| {
                SequenceStep::new(
                    Fraction::new(index, count),
                    Fraction::new(index + 1, count),
                    Value::F64(*value),
                    Locations::new(),
                )
            })
            .collect();
        sequence(vec![SequenceLayer {
            period: Fraction::int(count),
            steps,
        }])
        .expect("steps in order")
    }

    fn transformer(
        name: &str,
        apply: impl Fn(Pattern) -> Pattern + Send + Sync + 'static,
    ) -> Pattern {
        pure(Value::Function(std::sync::Arc::new(
            crate::value::FunctionRef::registered(name, Arc::new(apply), true),
        )))
    }

    /// `every` of a count for each cycle, with `apply`, over `receiver`: a
    /// join that makes its patterns at the query.
    fn every_cycle(
        counts: &[f64],
        name: &str,
        apply: impl Fn(Pattern) -> Pattern + Send + Sync + 'static,
        receiver: Pattern,
    ) -> Pattern {
        crate::register::default_registry()
            .get("every")
            .expect("registered every")
            .call(&[each_cycle(counts), transformer(name, apply)], receiver)
    }

    /// Sections with a join that makes patterns of discrete haps at the
    /// query: a count with a transformer, and a count of repeats.
    fn sections_made_at_the_query() -> Vec<Pattern> {
        let registry = crate::register::default_registry();
        let ply = registry.get("ply").expect("registered ply");
        vec![
            every_cycle(&[2.0, 3.0], "rev", |pattern| pattern.rev(), steps(130, 4)),
            ply.call(&[each_cycle(&[2.0, 1.0, 3.0])], steps(140, 3)),
            stack(vec![
                steps(150, 2),
                ply.call(&[each_cycle(&[1.0, 2.0])], steps(160, 2)),
            ]),
        ]
    }

    /// A section with a join that makes a pattern that reads the span.
    fn section_that_makes_a_signal() -> Pattern {
        every_cycle(
            &[1.0, 2.0],
            "with a signal",
            |pattern| stack(vec![pattern, crate::signal::saw()]),
            steps(170, 3),
        )
    }

    #[test]
    fn a_restart_asks_patterns_made_at_the_query_over_their_part() {
        let spans = [
            (Fraction::ZERO, whole(16)),
            (third(-5), third(22)),
            (third(7), third(8)),
            (third(13), third(29)),
            (third(7), third(7)),
        ];
        let sections = || {
            let mut sections = sections_made_at_the_query();
            sections.push(section_that_makes_a_signal());
            sections
        };
        for mode in [JoinMode::Restart, JoinMode::Reset] {
            for (begin, end) in spans {
                // A walk asks over the part, and falls back for the section
                // that makes a signal. A query that may read a table asks
                // over the span.
                let whole_span = picked(
                    mode,
                    sections().into_iter().map(asked_over_the_span).collect(),
                    third(7),
                );
                for cut in [
                    picked(mode, sections(), third(7)).mark_walked(),
                    picked(mode, sections(), third(7)),
                ] {
                    assert_eq!(
                        format!("{:?}", cut.query_arc(begin, end)),
                        format!("{:?}", whole_span.query_arc(begin, end)),
                        "{mode:?} over {begin:?}..{end:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_join_says_at_the_query_when_a_pattern_that_it_made_reads_the_span() {
        use purity::SpanRead;
        let state = State::new(TimeSpan::new(third(1), third(8)));
        for section in sections_made_at_the_query() {
            assert_eq!(section.purity().span_read, SpanRead::Made);
            assert!(asked_over_part(|| section.query(&state)).is_some());
        }
        let signal = section_that_makes_a_signal();
        assert_eq!(signal.purity().span_read, SpanRead::Made);
        assert!(asked_over_part(|| signal.query(&state)).is_none());
        // A query over a part around the refused one is refused too, and
        // no query asks over a part after them.
        let around = asked_over_part(|| asked_over_part(|| signal.query(&state)).is_none());
        assert_eq!(around, None);
        assert!(!part_is_asked());
        assert!(!signal.query(&state).is_empty());
    }

    #[test]
    fn a_part_query_asks_no_volatile_pattern_that_a_join_made() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let asked = Arc::new(AtomicUsize::new(0));
        let section = || {
            let asked = Arc::clone(&asked);
            every_cycle(
                &[1.0, 1.0],
                "counted",
                move |pattern| {
                    let asked = Arc::clone(&asked);
                    pattern
                        .with_query_span(move |span| {
                            asked.fetch_add(1, Ordering::Relaxed);
                            *span
                        })
                        .mark_volatile()
                },
                steps(180, 2),
            )
        };
        let sections = |section: Pattern| {
            picked(JoinMode::Restart, vec![section, steps(190, 2)], whole(2)).mark_walked()
        };
        let over_the_span = sections(asked_over_the_span(section()));
        let expected = format!("{:?}", over_the_span.query_arc(Fraction::ZERO, whole(8)));
        let asks = asked.swap(0, Ordering::Relaxed);
        assert!(asks > 0);
        let cut = sections(section());
        assert_eq!(
            format!("{:?}", cut.query_arc(Fraction::ZERO, whole(8))),
            expected
        );
        assert_eq!(asked.load(Ordering::Relaxed), asks);
    }

    /// What `pattern` answers to a top-level query of `span` as `state`
    /// says, in the order of the graph and sorted.
    fn asked(pattern: &Pattern, span: TimeSpan, state: &State) -> String {
        let state = state.set_span(span);
        let graph = pattern.query_arc_outcome_with_budget(&state, DEFAULT_HAP_BUDGET);
        let contained = take_query_contained_throw();
        let sorted = pattern.query_arc_outcome_sorted_with_budget(&state, DEFAULT_HAP_BUDGET);
        format!(
            "{graph:?} {contained:?} {sorted:?} {:?}",
            take_query_contained_throw()
        )
    }

    /// `count` spans of `width` in a row from `from`.
    fn in_a_row(from: Fraction, width: Fraction, count: i128) -> Vec<TimeSpan> {
        (0..count)
            .map(|index| {
                let begin = from + width * Fraction::int(index);
                TimeSpan::new(begin, begin + width)
            })
            .collect()
    }

    /// Graphs of discrete haps, whole songs in small: sections one after
    /// the other, with joins that make patterns at the query, at a tempo.
    fn songs() -> Vec<(&'static str, Pattern)> {
        let made = || picked(JoinMode::Restart, sections_made_at_the_query(), third(4));
        vec![
            ("steps", steps(0, 4)),
            (
                "sections",
                picked(JoinMode::Restart, discrete_sections(), third(7)),
            ),
            (
                "sections that reset",
                picked(JoinMode::Reset, discrete_sections(), third(2)),
            ),
            ("sections made at the query", made()),
            (
                "a song at a tempo",
                stack(vec![made(), steps(200, 3)]).cpm(Fraction::int(45)),
            ),
            // A long step sounds on when a short one begins and ends in a
            // query: the cut parts say which comes first.
            (
                "long steps over short ones",
                stack(vec![steps(210, 2), steps(220, 16)]),
            ),
            // A cycle of the song is more than a cycle of its patterns:
            // one that asks for each cycle, and one that does not.
            (
                "patterns in turn at a tempo",
                stack(vec![
                    in_turn(300).split_queries(),
                    in_turn(340),
                    Pattern::of(Node::SlowCatPrime(
                        vec![in_turn(360), steps(370, 3)].into_boxed_slice(),
                    ))
                    .split_queries(),
                    steps(380, 2),
                ])
                .cpm(Fraction::int(35)),
            ),
        ]
    }

    /// Three patterns, one a cycle: steps, lanes, and steps of two cycles.
    fn in_turn(from: i32) -> Pattern {
        slowcat(vec![
            steps(from, 2),
            stack(vec![steps(from + 10, 3), note(f64::from(from + 15))]),
            steps(from + 20, 4).slow(whole(2)),
        ])
    }

    /// A map of the haps of a list from a place on keeps the haps before
    /// the place and the order of the others, and a hap that the map drops
    /// leaves. A map that unwinds drops no hap two times.
    #[test]
    fn a_map_of_the_haps_of_a_list_writes_each_where_it_is() {
        let span = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
        let shared: Arc<str> = "x".into();
        let hap = |n: i32| Hap::new(Some(span), span, Value::F64(f64::from(n)));
        let numbers = |haps: &[Hap]| -> Vec<f64> {
            haps.iter().filter_map(|hap| hap.value.as_f64()).collect()
        };
        let mut haps: Vec<Hap> = (0..6).map(hap).collect();
        crate::map_from(&mut haps, 2, |hap| match hap.value.as_f64() {
            Some(3.0 | 5.0) => None,
            number => Some(hap.with_value(|_| Value::F64(number.unwrap_or(0.0) * 10.0))),
        });
        assert_eq!(numbers(&haps), [0.0, 1.0, 20.0, 40.0]);
        crate::map_from(&mut haps, 4, |_| None);
        assert_eq!(numbers(&haps), [0.0, 1.0, 20.0, 40.0]);

        // Five haps hold the text. The map takes the fourth and unwinds:
        // that hap drops, and the fifth is not read.
        let held = || {
            let mut hap = Hap::new(Some(span), span, Value::F64(0.0));
            hap.keys.edit().scale = Some(Arc::clone(&shared));
            hap
        };
        let mut haps: Vec<Hap> = (0..5).map(|_| held()).collect();
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut taken = 0;
            crate::map_from(&mut haps, 1, |hap| {
                taken += 1;
                assert!(taken < 3, "the map stops");
                Some(hap)
            });
        }));
        assert!(unwound.is_err());
        assert_eq!(haps.len(), 3);
        drop(haps);
        assert_eq!(Arc::strong_count(&shared), 2);
    }

    #[test]
    fn a_run_of_short_queries_matches_fresh_walks() {
        let widths = [Fraction::new(1, 8), Fraction::new(5, 48), third(1)];
        let tempo = |cps: f64| {
            let mut state = State::new(TimeSpan::new(Fraction::ZERO, Fraction::ONE));
            state.controls.insert("_cps".to_string(), Value::F64(cps));
            state
        };
        for (index, (name, _)) in songs().into_iter().enumerate() {
            for width in widths {
                for state in [
                    State::new(TimeSpan::new(Fraction::ZERO, Fraction::ONE)),
                    tempo(0.75),
                ] {
                    for start in [third(-4), Fraction::ZERO] {
                        let song = || songs().swap_remove(index).1.mark_walked();
                        let run = song();
                        for span in in_a_row(start, width, 24) {
                            assert_eq!(
                                asked(&run, span, &state),
                                asked(&song(), span, &state),
                                "{name} over {span:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_tempo_change_keeps_what_its_pattern_says_of_the_span() {
        use purity::SpanRead;
        let read = |pattern: Pattern| pattern.purity().span_read;
        assert_eq!(read(steps(0, 4).cpm(Fraction::int(30))), SpanRead::No);
        let made = sections_made_at_the_query().swap_remove(0);
        assert_eq!(read(made.cpm(Fraction::int(30))), SpanRead::Made);
        assert_eq!(
            read(crate::signal::saw().cpm(Fraction::int(30))),
            SpanRead::Yes
        );
        assert_eq!(read(steps(0, 4).cpm(Fraction::int(-30))), SpanRead::Yes);
    }

    /// A sequence of control maps `{n}`, each at its location: `count`
    /// steps in each of `cycles` cycles, from `from`.
    fn degrees(from: i32, count: i128, cycles: i128) -> Pattern {
        let total = count * cycles;
        let steps = (0..total)
            .map(|index| {
                let degree = f64::from(from) + (index % 7) as f64;
                let mut locations = Locations::new();
                locations.push((
                    from as usize + index as usize,
                    from as usize + index as usize + 1,
                ));
                SequenceStep::new(
                    Fraction::new(index, total),
                    Fraction::new(index + 1, total),
                    Value::object([("n".to_string(), Value::F64(degree))]),
                    locations,
                )
            })
            .collect();
        sequence(vec![SequenceLayer {
            period: Fraction::int(cycles),
            steps,
        }])
        .expect("steps in order")
    }

    /// A pick of `sections` in turn by a sequence, each for `length` cycles.
    fn picked_by_a_sequence(mode: JoinMode, sections: Vec<Pattern>, length: Fraction) -> Pattern {
        let count = sections.len() as i128;
        let steps = (0..count)
            .map(|index| {
                let mut locations = Locations::new();
                locations.push((900 + index as usize, 901 + index as usize));
                SequenceStep::new(
                    Fraction::new(index, count),
                    Fraction::new(index + 1, count),
                    Value::F64(index as f64),
                    locations,
                )
            })
            .collect();
        let selector = sequence(vec![SequenceLayer {
            period: length * Fraction::int(count),
            steps,
        }])
        .expect("steps in order");
        pick(
            selector,
            PickLookup::Array {
                enumerable_len: sections.len(),
                length: sections.len(),
                entries: sections.into_iter().enumerate().collect(),
            },
            PickIndexMode::Clamp,
            mode,
        )
    }

    #[test]
    fn a_change_after_a_pick_answers_as_the_change_of_what_it_picks() {
        use crate::compose::{Alignment, ComposeOp, compose};
        use crate::controls::ControlSpec;
        let spans = [
            (Fraction::ZERO, whole(12)),
            (third(-5), third(22)),
            (third(7), third(8)),
            (third(13), third(29)),
            (third(7), third(7)),
        ];
        let control = |key: &str, value: f64, at: usize| {
            pure(Value::F64(value))
                .with_added_context(vec![(at, at + 2)])
                .map_control(&ControlSpec::new([key]), false)
        };
        // A scale, then two controls: what `n(..).scale(..).gain(..)` is
        // after a pick.
        let changed = |pattern: Pattern| {
            let scaled = crate::combinators::scale(&pattern, Value::Str("C:minor".into()));
            let quiet = compose(
                &scaled,
                &control("gain", 0.5, 700),
                ComposeOp::Set,
                Alignment::In,
            );
            compose(
                &quiet,
                &control("room", 0.2, 710),
                ComposeOp::Set,
                Alignment::In,
            )
        };
        let answers = |pattern: &Pattern, begin: Fraction, end: Fraction| -> Vec<String> {
            pattern
                .query_arc(begin, end)
                .iter()
                .map(|hap| format!("{hap:?} {:?} {:?}", hap.keys, hap.live_controls()))
                .collect()
        };
        // Steps in one cycle each, a rest, a control of a rest as `n("~")`
        // is, and a step of three cycles.
        let rest = || silence().map_control(&ControlSpec::new(["n"]), false);
        assert!(rest().is_rest() && changed(rest()).is_rest());
        let short = || vec![degrees(0, 4, 1), silence(), rest(), degrees(20, 3, 2)];
        let long = || vec![degrees(0, 4, 1), degrees(40, 1, 3).slow(whole(3))];
        for mode in [
            JoinMode::Inner,
            JoinMode::Outer,
            JoinMode::Mix,
            JoinMode::Restart,
            JoinMode::Reset,
        ] {
            let cases: [(&str, &dyn Fn() -> Pattern, bool); 3] = [
                // The picked patterns take the scale and the controls.
                (
                    "whole cycles",
                    &|| picked_by_a_sequence(mode, short(), whole(2)),
                    true,
                ),
                // A restart moves a section by a part of a cycle, where
                // the constant of the pick cuts another way.
                (
                    "parts of a cycle",
                    &|| picked_by_a_sequence(mode, short(), third(7)),
                    !matches!(mode, JoinMode::Restart | JoinMode::Reset),
                ),
                // A step of three cycles is cut by the constant.
                (
                    "a long step",
                    &|| picked_by_a_sequence(mode, long(), whole(2)),
                    false,
                ),
            ];
            for (name, build, all_inside) in cases {
                let inside = changed(build());
                assert_eq!(
                    matches!(inside.node_kind(), Node::Pick { .. }),
                    all_inside,
                    "{mode:?}, {name}"
                );
                let outside = sequence::unfolded(|| changed(build()));
                assert!(!matches!(outside.node_kind(), Node::Pick { .. }));
                for (begin, end) in spans {
                    assert_eq!(
                        answers(&inside, begin, end),
                        answers(&outside, begin, end),
                        "{mode:?}, {name}, over {begin:?}..{end:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_structure_asks_discrete_functions_in_one_query() {
        use crate::compose::{Alignment, ComposeOp, compose};
        let spans = [
            (Fraction::ZERO, whole(4)),
            (third(-5), third(22)),
            (third(7), third(8)),
            (third(7), third(7)),
        ];
        let answers = |pattern: &Pattern, begin: Fraction, end: Fraction| -> Vec<String> {
            pattern
                .query_arc(begin, end)
                .iter()
                .map(|hap| format!("{hap:?} {:?} {:?}", hap.keys, hap.live_controls()))
                .collect()
        };
        // Steps that fill the cycle, steps that overlap, and long steps.
        let structures = || {
            let on = |count: i32| fastcat((0..count).map(|_| pure(Value::Bool(true))).collect());
            vec![
                on(12),
                stack(vec![on(3), on(4).late(third(1))]),
                on(5).slow(whole(3)),
            ]
        };
        for (index, functions) in discrete_sections().into_iter().enumerate() {
            for (kind, structure) in structures().into_iter().enumerate() {
                assert_eq!(functions.purity().span_read, purity::SpanRead::No);
                let structured = |functions: &Pattern| {
                    compose(functions, &structure, ComposeOp::KeepIf, Alignment::Out)
                };
                // Behind a time map that changes no time, the functions are
                // asked for each step of the structure.
                let (one_query, each_step) = (
                    structured(&functions),
                    structured(&asked_over_the_span(functions.clone())),
                );
                for (begin, end) in spans {
                    assert_eq!(
                        answers(&one_query, begin, end),
                        answers(&each_step, begin, end),
                        "section {index}, structure {kind}, over {begin:?}..{end:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_split_of_queries_asks_a_discrete_pattern_once_for_all_the_cycles() {
        let spans = [
            (Fraction::ZERO, whole(4)),
            (third(-5), third(22)),
            (third(7), third(8)),
            (third(7), third(7)),
        ];
        let answers = |pattern: &Pattern, begin: Fraction, end: Fraction| -> Vec<String> {
            pattern
                .query_arc(begin, end)
                .iter()
                .map(|hap| format!("{hap:?} {:?} {:?}", hap.keys, hap.live_controls()))
                .collect()
        };
        for (index, section) in discrete_sections().into_iter().enumerate() {
            assert_eq!(section.purity().span_read, purity::SpanRead::No);
            // Behind a time map that changes no time, the section is asked
            // for each cycle.
            let (one_query, each_cycle) = (
                section.split_queries(),
                asked_over_the_span(section.clone()).split_queries(),
            );
            // The bind of a value to the section, which `sometimes` was
            // before it was a split: each cycle of the value asks the
            // section and adds nothing to its haps.
            let bound = pure(Value::F64(0.5))
                .fmap_to_pattern(move |_| section.clone())
                .inner_join();
            for (begin, end) in spans {
                let split = answers(&one_query, begin, end);
                assert_eq!(
                    split,
                    answers(&each_cycle, begin, end),
                    "section {index}, each cycle, over {begin:?}..{end:?}"
                );
                assert_eq!(
                    split,
                    answers(&bound, begin, end),
                    "section {index}, the bind, over {begin:?}..{end:?}"
                );
            }
        }
    }

    #[test]
    fn an_application_asks_a_discrete_side_over_the_part_of_a_hap() {
        use crate::compose::{Alignment, ComposeOp, compose};
        // Windows inside a note of three cycles, a window of no width, and
        // spans that hold the note.
        let spans = [
            (third(1), third(2)),
            (third(4), third(5)),
            (third(7), third(8)),
            (third(7), third(7)),
            (Fraction::ZERO, whole(4)),
            (third(-5), third(22)),
        ];
        let answers = |pattern: &Pattern, begin: Fraction, end: Fraction| -> Vec<String> {
            pattern
                .query_arc(begin, end)
                .iter()
                .map(|hap| format!("{hap:?} {:?} {:?}", hap.keys, hap.live_controls()))
                .collect()
        };
        let long = || note(300.0).slow(whole(3));
        for (index, section) in discrete_sections().into_iter().enumerate() {
            // The long note gives the structure, on each side in turn.
            for alignment in [Alignment::In, Alignment::Out] {
                let met = |section: &Pattern| match alignment {
                    Alignment::Out => compose(section, &long(), ComposeOp::Set, alignment),
                    _ => compose(&long(), section, ComposeOp::Set, alignment),
                };
                // Behind a time map that changes no time, the section is
                // asked over the whole of the note.
                let (over_part, over_whole) =
                    (met(&section), met(&asked_over_the_span(section.clone())));
                for (begin, end) in spans {
                    assert_eq!(
                        answers(&over_part, begin, end),
                        answers(&over_whole, begin, end),
                        "section {index}, {alignment:?}, over {begin:?}..{end:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_graph_that_reads_the_span_is_no_graph_of_discrete_haps() {
        let discrete = |pattern: &Pattern| pattern.purity().span_read == purity::SpanRead::No;
        for section in discrete_sections() {
            assert!(discrete(&section));
            assert!(!discrete(&asked_over_the_span(section)));
        }
        for section in sections_with_a_signal() {
            assert!(!discrete(&section));
        }
        assert!(!discrete(&crate::signal::saw()));
    }
}
