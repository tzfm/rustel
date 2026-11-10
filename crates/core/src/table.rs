/*
table.rs - precomputed answers for periodic patterns
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! Precomputed answers for periodic patterns.
//!
//! A pure graph of sequences, speed changes, shifts, stacks and value maps
//! repeats after a whole number of cycles. Its [`Tables`] hold the haps of one
//! period in the order the graph returns them. A query shifts and clips those
//! haps instead of walking the graph.
//!
//! A table keeps the contract of the nodes it stands for. For every query
//! span with `begin < end`, it returns the same haps, with the same values,
//! contexts and metadata, in the same order. Each part is the entry's part
//! clipped to the query span. A query that the graph might refuse, or that
//! takes a table's own arithmetic outside the native range, goes back to the
//! graph, so every refusal stays the graph's. See [`Work`].

use crate::settings::SettingsStateId;
use crate::{
    CombineFn, Hap, HapMapFn, LookupFlow, Meeting, Node, OwnedHapMapFn, Pattern, SignalFn, SpanUse,
    State, TimeSpan, ValueFn, ValuePredicate, applied_hap,
};
use rustel_fraction::Fraction;
use std::sync::{Arc, OnceLock};

/// The most haps one table may hold, and the most cycles a build answers
/// one at a time. Above it the graph answers.
const MAX_ENTRIES: usize = 8192;

/// The most haps one query reserves room for ahead of making them.
const MAX_RESERVED: usize = 1 << 16;

/// The most graph levels one table build walks. Each level is a few native
/// frames, so this stays well below [`crate::MAX_PATTERN_DEPTH`].
const MAX_BUILD_DEPTH: u32 = 128;

/// The most entries a build scans while two sides meet. Past it the graph
/// answers, so a build stays short next to the query that asked for it.
const MAX_BUILD_SCAN: usize = 1 << 20;

/// The most bits a time of the graph's walk may take before a table leaves
/// the query to the graph. An `i128` holds 127, and the rest covers the
/// rounding of the estimate.
const TIME_BITS: u32 = 120;

thread_local! {
    static BUILD_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// One level of a table build, released when dropped, also on unwind.
pub(crate) struct BuildDepth;

impl BuildDepth {
    /// `None` past [`MAX_BUILD_DEPTH`].
    pub(crate) fn enter() -> Option<Self> {
        BUILD_DEPTH.with(|depth| {
            let next = depth.get().checked_add(1)?;
            (next <= MAX_BUILD_DEPTH).then(|| {
                depth.set(next);
                Self
            })
        })
    }
}

impl Drop for BuildDepth {
    fn drop(&mut self) {
        BUILD_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

/// One period of haps. Entry times belong to the window that starts at cycle
/// zero. A part may reach outside `[0, period)`.
#[derive(Clone, Debug)]
struct Layer {
    /// Whole cycles after which the entries repeat. At least one.
    period: i128,
    entries: Vec<Hap>,
    /// The earliest part begin and the latest part end over the entries.
    reach: Option<TimeSpan>,
    /// The parts never overlap and come in time order, also from one period
    /// to the next. Clipping keeps that, so every answer is in query order.
    ordered: bool,
    /// The entries stably sorted as [`crate::sort_haps_for_query`] sorts,
    /// when that order also never overlaps and `entries` is not in it
    /// already. A sorted answer then needs no sort.
    sorted: Option<Vec<Hap>>,
    /// Every entry owns no heap memory, so a byte copy of the entries is a
    /// clone of them. See [`Hap::owns_no_heap`].
    copyable: bool,
    /// For haps whose values a signal gives at query time: the entries are
    /// the structure, and this makes each value. See [`Sampler`].
    sampler: Option<Arc<Sampler>>,
    /// Every entry is a whole hap: its part is its whole.
    whole_haps: bool,
    /// For entries out of time order that come cycle by cycle, each in one
    /// cycle of `[0, period)`: entry `cycle_starts[c]` is the first of cycle
    /// `c`, and the last item is the entry count. See [`Find::ByCycle`].
    cycle_starts: Option<Vec<u32>>,
}

/// The entries a query answers from, and how it finds the ones of a period
/// that the span only cuts.
#[derive(Clone, Copy)]
struct Answering<'a> {
    haps: &'a [Hap],
    find: Find<'a>,
}

#[derive(Clone, Copy)]
enum Find<'a> {
    /// The parts come in time order and never overlap, so their begins and
    /// ends both rise. Two binary searches find the ones a span meets.
    Search,
    /// The entries come cycle by cycle, each part in one cycle. The span's
    /// cycles give the range. See [`Layer::cycle_starts`].
    ByCycle(&'a [u32]),
    /// Every entry is looked at.
    Scan,
}

impl Answering<'_> {
    /// The entries of the period `shift` cycles on that may meet `span`. A
    /// narrow query of a long period looks at its own cycle only, as the
    /// graph does.
    fn near(&self, span: TimeSpan, shift: i128) -> Option<&[Hap]> {
        let back = shift.checked_neg()?;
        let begin = span.begin.checked_add_int(back)?;
        let end = span.end.checked_add_int(back)?;
        Some(match self.find {
            Find::Search => {
                let from = self.haps.partition_point(|hap| hap.part.end <= begin);
                let to = from + self.haps[from..].partition_point(|hap| hap.part.begin < end);
                &self.haps[from..to]
            }
            Find::ByCycle(starts) => {
                let cycles = starts.len() - 1;
                let at = |cycle: i128| {
                    let cycle = usize::try_from(cycle.max(0)).unwrap_or(usize::MAX);
                    starts[cycle.min(cycles)] as usize
                };
                &self.haps[at(begin.floor().numer())..at(end.ceil().numer())]
            }
            Find::Scan => self.haps,
        })
    }
}

/// The values of a layer that meets a signal: `appLeft` or `appRight` with a
/// signal on the other side. The entries are that structure. Each hap meets
/// the signal sampled over its whole, as the graph samples it, and then goes
/// through the value steps above the meeting.
#[derive(Clone)]
struct Sampler {
    signal: SignalFn,
    /// What the signal's own pattern does to its hap before the meeting.
    signal_steps: Vec<Step>,
    /// The side of the meeting the signal is on.
    side: Side,
    apply: CombineFn,
    flow: LookupFlow,
    then: Vec<Step>,
}

impl std::fmt::Debug for Sampler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sampler")
            .field("side", &self.side)
            .field("steps", &self.then.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug)]
enum Side {
    /// `appRight`: the signal gives the function values.
    Function,
    /// `appLeft`: the signal gives the argument values.
    Value,
}

impl Sampler {
    /// Push the hap a structure `entry`, at `whole` and `part`, makes with
    /// the signal sampled over that whole, unless a step drops it. The
    /// meeting reads the entry's value and metadata, never its own spans.
    /// `None` when the met value is not data, which only the graph answers.
    ///
    /// `signal` is scratch: the signal's hap, as the graph would make it, is
    /// written into it.
    fn sample(
        &self,
        entry: &Hap,
        whole: TimeSpan,
        part: TimeSpan,
        probe: &mut State,
        signal: &mut Hap,
        out: &mut Vec<Hap>,
    ) -> Option<()> {
        probe.span = whole;
        let value = (self.signal)(probe);
        if self.signal_steps.is_empty() {
            // Nothing else of a bare signal's hap changes from one to the next.
            signal.part = whole;
            signal.value = value;
        } else {
            *signal = Hap::new(None, whole, value);
            if !self
                .signal_steps
                .iter()
                .all(|step| step.run_in_place(signal))
            {
                return Some(());
            }
        }
        let signal = &*signal;
        out.push(match self.side {
            Side::Function => applied_hap(signal, entry, Some(whole), part, &self.apply, self.flow),
            Side::Value => applied_hap(entry, signal, Some(whole), part, &self.apply, self.flow),
        });
        let met = out.last_mut()?;
        if !self.then.iter().all(|step| step.run_in_place(met)) {
            out.pop();
        } else if !met.value.is_data(0) {
            return None;
        }
        Some(())
    }
}

/// A per-hap change that does not look at time: run once per entry, or per
/// hap on a sampled layer, whose values the query makes.
#[derive(Clone)]
enum Step {
    Map(ValueFn),
    Keep(ValuePredicate),
    MapHap(HapMapFn),
    MapOwned(OwnedHapMapFn),
    Locations(Arc<Vec<(usize, usize)>>),
}

impl Step {
    /// The hap after this step, or `None` when the step drops it.
    fn run(&self, mut hap: Hap) -> Option<Hap> {
        self.run_in_place(&mut hap).then_some(hap)
    }

    /// Run this step on `hap` where it is. `false` when the step drops it.
    #[inline]
    fn run_in_place(&self, hap: &mut Hap) -> bool {
        match self {
            Step::Map(f) => {
                hap.map_value_in_place(|value| f(value));
                true
            }
            Step::Keep(keep) => keep(&hap.value),
            Step::MapHap(f) => Self::replaced(hap, f(hap)),
            Step::MapOwned(f) => Self::run_owned(f, hap),
            Step::Locations(locations) => {
                hap.context.extend_from_slice(locations);
                true
            }
        }
    }

    fn replaced(hap: &mut Hap, mapped: Option<Hap>) -> bool {
        match mapped {
            Some(mapped) => {
                *hap = mapped;
                true
            }
            None => false,
        }
    }

    /// A map that takes the hap by value. Kept out of line: the hap it moves
    /// would make every step pay for its room.
    #[inline(never)]
    fn run_owned(f: &OwnedHapMapFn, hap: &mut Hap) -> bool {
        let owned = std::mem::replace(hap, Hap::new(None, hap.part, crate::Value::Undefined));
        Self::replaced(hap, f(owned))
    }
}

/// Layers answered one after the other, as a stack answers its children.
#[derive(Clone, Debug)]
pub(crate) struct Tables {
    layers: Vec<Layer>,
    /// What the graph's walk does for a query. See [`Work`].
    work: Work,
    /// The longest whole of an entry, in cycles: how far an answer's times
    /// reach past the query span.
    extent: f64,
    /// The most bits in the denominator of an entry's time.
    entry_bits: u32,
    /// Every layer's haps of one shared period in one sorted layer, for a
    /// sorted answer of several layers. Built when first asked for. See
    /// [`Tables::union`].
    union: OnceLock<Option<Layer>>,
    /// The settings snapshot the cached table was built under, the only one
    /// it answers for. See [`crate::settings::SettingsStateId`].
    state: Option<SettingsStateId>,
}

/// What the graph's walk does for a query, as bounds a table checks before
/// it answers. A query that the graph might refuse goes back to it, so the
/// refusal stays the graph's.
#[derive(Clone, Copy, Debug)]
struct Work {
    /// The cycles one span split walks. The graph refuses a walk above
    /// [`crate::MAX_QUERY_SPAN_CYCLES`].
    cycles: Cycles,
    /// The haps one vector of the walk holds. The graph refuses a vector
    /// above the hap budget.
    haps: Count,
    /// The levels of the walk, this node's own included. The graph refuses a
    /// level past [`crate::MAX_PATTERN_DEPTH`].
    levels: u32,
    /// How large the times of the walk grow. The graph refuses a time outside
    /// the native range.
    times: Times,
}

impl Work {
    /// A leaf: one level, which walks `cycles`.
    fn leaf(cycles: Cycles) -> Self {
        Self {
            cycles,
            haps: Count::default(),
            levels: 1,
            times: Times::SAME,
        }
    }

    /// A node that asks this child over the query span itself.
    fn above(self) -> Self {
        Self {
            levels: self.levels.saturating_add(1),
            ..self
        }
    }

    fn max(self, other: Self) -> Self {
        Self {
            cycles: self.cycles.max(other.cycles),
            haps: self.haps.max(other.haps),
            levels: self.levels.max(other.levels),
            times: self.times.max(other.times),
        }
    }
}

/// The cycles one span split walks for a query `w` cycles wide:
/// `per_cycle * w + fixed`.
#[derive(Clone, Copy, Debug)]
struct Cycles {
    per_cycle: Fraction,
    fixed: Fraction,
}

impl Cycles {
    /// Silence walks nothing.
    const NONE: Self = Self {
        per_cycle: Fraction::ZERO,
        fixed: Fraction::ZERO,
    };

    /// A walk over the query span itself: `ceil(end) - floor(begin)` cycles,
    /// at most two more than the width.
    fn walk() -> Self {
        Self {
            per_cycle: Fraction::ONE,
            fixed: Fraction::int(2),
        }
    }

    fn at(self, width: Fraction) -> Option<Fraction> {
        self.per_cycle.checked_mul(width)?.checked_add(self.fixed)
    }

    fn max(self, other: Self) -> Self {
        Self {
            per_cycle: self.per_cycle.max(other.per_cycle),
            fixed: self.fixed.max(other.fixed),
        }
    }

    /// A child queried over spans `factor` times as wide.
    fn scaled(self, factor: Fraction) -> Option<Self> {
        Some(Self {
            per_cycle: self.per_cycle.checked_mul(factor)?,
            fixed: self.fixed,
        })
    }

    /// A child queried over spans at most `width` wide, whatever the query.
    fn bounded(self, width: Fraction) -> Option<Self> {
        Some(Self {
            per_cycle: Fraction::ZERO,
            fixed: self.at(width)?,
        })
    }

    /// A walk over the query span, then a child queried a cycle at a time.
    fn split(child: Self) -> Option<Self> {
        Some(Self::walk().max(child.bounded(Fraction::ONE)?))
    }
}

/// The haps one vector of the walk holds for a query `w` cycles wide: at
/// most `per_cycle * ceil(w) + fixed`.
#[derive(Clone, Copy, Debug, Default)]
struct Count {
    per_cycle: u64,
    fixed: u64,
}

impl Count {
    /// The answer of `layers`. A span `w` wide meets at most
    /// `(w + reach) / period + 1` periods of a layer, each with all of its
    /// entries.
    fn of(layers: &[Layer]) -> Self {
        layers.iter().fold(Self::default(), |count, layer| {
            let entries = u64::try_from(layer.entries.len()).unwrap_or(u64::MAX);
            let period = u64::try_from(layer.period).unwrap_or(u64::MAX).max(1);
            let reach = layer.reach.map_or(0, |reach| {
                reach
                    .end
                    .checked_sub(reach.begin)
                    .map_or(u64::MAX, cycles_up)
            });
            let periods = reach.div_ceil(period).saturating_add(2);
            Self {
                per_cycle: count.per_cycle.saturating_add(entries.div_ceil(period)),
                fixed: count.fixed.saturating_add(entries.saturating_mul(periods)),
            }
        })
    }

    fn at(self, width: Fraction) -> u64 {
        self.per_cycle
            .saturating_mul(cycles_up(width))
            .saturating_add(self.fixed)
    }

    fn max(self, other: Self) -> Self {
        Self {
            per_cycle: self.per_cycle.max(other.per_cycle),
            fixed: self.fixed.max(other.fixed),
        }
    }

    /// A child asked over spans `factor` times as wide. `ceil(factor * w)`
    /// is at most `factor * w + 1`.
    fn scaled(self, factor: Fraction) -> Self {
        Self {
            per_cycle: Fraction::int(i128::from(self.per_cycle))
                .checked_mul(factor)
                .map_or(u64::MAX, cycles_up),
            fixed: self.fixed.saturating_add(self.per_cycle),
        }
    }

    /// A child asked over spans at most `width` wide, whatever the query.
    fn bounded(self, width: Fraction) -> Self {
        Self {
            per_cycle: 0,
            fixed: self.at(width),
        }
    }
}

/// How large the times of the walk grow for a query time `t`: at most
/// `scale * |t| + shift` in size, with at most `denominator_bits` more bits
/// in a denominator than `t` or an entry's time has.
///
/// A hap that never reaches the answer, one a filter drops or one on the
/// other side of a meeting, can have a longer whole than any entry. A whole
/// grows long only through a slow-down, and the slow-down's factor adds at
/// least as many bits to `denominator_bits` as the whole gains in size, so
/// the bound covers those haps too. A rule that lengthens wholes in another
/// way must add their size here.
#[derive(Clone, Copy, Debug)]
struct Times {
    scale: f64,
    shift: f64,
    denominator_bits: u32,
}

impl Times {
    /// The query's own times. The bounds of their cycle add one.
    const SAME: Self = Self {
        scale: 1.0,
        shift: 1.0,
        denominator_bits: 0,
    };

    fn max(self, other: Self) -> Self {
        Self {
            scale: self.scale.max(other.scale),
            shift: self.shift.max(other.shift),
            denominator_bits: self.denominator_bits.max(other.denominator_bits),
        }
    }

    /// A child asked at `t * factor`.
    fn scaled(self, factor: Fraction) -> Self {
        Self {
            scale: (self.scale * factor.to_f64().abs()).max(1.0),
            shift: self.shift,
            denominator_bits: self
                .denominator_bits
                .saturating_add(log2_up(factor.denom())),
        }
    }

    /// A child asked at `t - by`.
    fn shifted(self, by: Fraction) -> Self {
        Self {
            scale: self.scale,
            shift: self.shift + self.scale * by.to_f64().abs(),
            denominator_bits: self.denominator_bits.saturating_add(log2_up(by.denom())),
        }
    }

    /// A child asked inside the query's cycle, or one next to it.
    fn cycled(self) -> Self {
        Self {
            shift: self.shift + self.scale,
            ..self
        }
    }

    /// A child asked over the wholes of haps that reach `extent` cycles past
    /// the query span.
    fn reaching(self, extent: f64) -> Self {
        Self {
            shift: self.shift + self.scale * extent,
            ..self
        }
    }
}

/// `ceil(x)` as a count: zero at or below zero, `u64::MAX` past it.
fn cycles_up(x: Fraction) -> u64 {
    let up = x.ceil().numer();
    if up <= 0 {
        0
    } else {
        u64::try_from(up).unwrap_or(u64::MAX)
    }
}

/// `ceil(log2(n))` for a positive `n`: the bits a product with `n` may add.
fn log2_up(n: i128) -> u32 {
    128 - n.unsigned_abs().saturating_sub(1).leading_zeros()
}

/// The bits in the denominator of `t`.
fn denominator_bits(t: Fraction) -> u32 {
    128 - t.denom().unsigned_abs().leading_zeros()
}

/// The bits of the whole part of `x`, rounded up.
fn magnitude_bits(x: f64) -> u32 {
    if !x.is_finite() {
        return u32::MAX;
    }
    if x < 1.0 {
        return 0;
    }
    // `x` is below `2^(e - 1022)`, with `e` its biased exponent.
    let exponent = ((x.to_bits() >> 52) & 0x7ff) as u32;
    exponent.saturating_sub(1022)
}

impl Layer {
    fn new(period: i128, mut entries: Vec<Hap>) -> Option<Self> {
        if period < 1 || entries.len() > MAX_ENTRIES {
            return None;
        }
        let mut reach: Option<TimeSpan> = None;
        for entry in &entries {
            // An entry is a discrete hap with a part of positive width, so
            // a clipped part is never empty when it meets the query. A pick
            // lookup holds patterns of its own, and a value must be data.
            entry.whole?;
            if entry.part.begin >= entry.part.end
                || entry.pick_lookup.is_some()
                || !entry.value.is_data(0)
            {
                return None;
            }
            reach = Some(match reach {
                None => entry.part,
                Some(span) => TimeSpan::new(
                    span.begin.min(entry.part.begin),
                    span.end.max(entry.part.end),
                ),
            });
        }
        // Every query clones these values. An interned map clones as a
        // pointer.
        for entry in &mut entries {
            if let crate::Value::Object(object) = &entry.value
                && let Some(interned) = object.interned()
            {
                entry.value = crate::Value::Object(interned);
            }
        }
        let ordered = in_time_order(&entries, period);
        let sorted = (!ordered)
            .then(|| crate::sort_haps_for_query(entries.clone()))
            .filter(|sorted| in_time_order(sorted, period));
        let copyable = entries.iter().all(Hap::owns_no_heap);
        let whole_haps = entries.iter().all(|entry| entry.whole == Some(entry.part));
        let cycle_starts = if ordered {
            None
        } else {
            cycle_starts(&entries, period)
        };
        Some(Self {
            period,
            entries,
            reach,
            ordered,
            sorted,
            copyable,
            sampler: None,
            whole_haps,
            cycle_starts,
        })
    }

    fn with_sampler(mut self, sampler: Option<Arc<Sampler>>) -> Self {
        self.sampler = sampler;
        self
    }

    /// The entries to answer from: in graph order, or sorted. `None` when
    /// this layer has no sorted form that clipping keeps sorted.
    fn entries_in(&self, order: Order) -> Option<Answering<'_>> {
        match order {
            Order::Graph | Order::Sorted if self.ordered => Some(Answering {
                haps: &self.entries,
                find: Find::Search,
            }),
            Order::Graph => Some(Answering {
                haps: &self.entries,
                find: self
                    .cycle_starts
                    .as_deref()
                    .map_or(Find::Scan, Find::ByCycle),
            }),
            Order::Sorted => self.sorted.as_deref().map(|haps| Answering {
                haps,
                find: Find::Search,
            }),
        }
    }

    /// Whether this layer answers in `order` at all. Asked of every layer
    /// before any samples a signal, so that a declined answer runs no step.
    fn answers_in(&self, order: Order) -> bool {
        self.reach.is_none() || self.entries_in(order).is_some()
    }

    /// The entries of `cycles` consecutive periods from cycle zero.
    fn repeated(&self, cycles: i128) -> Option<Vec<Hap>> {
        // A sampled layer's values exist only in a query.
        if self.sampler.is_some() {
            return None;
        }
        // Any number of copies of nothing is nothing, found at once.
        if self.entries.is_empty() {
            return Some(Vec::new());
        }
        let copies = usize::try_from(cycles.checked_div(self.period)?).ok()?;
        if copies.checked_mul(self.entries.len())? > MAX_ENTRIES {
            return None;
        }
        let mut out = Vec::with_capacity(copies * self.entries.len());
        for copy in 0..copies {
            let shift = i128::try_from(copy).ok()?.checked_mul(self.period)?;
            for entry in &self.entries {
                out.push(shifted(entry, shift)?);
            }
        }
        Some(out)
    }

    /// Append the haps for `span` (`begin < end`) in `order`. `None` means
    /// the graph must answer, and `out` is then unchanged.
    fn query_into(
        &self,
        state: &State,
        order: Order,
        out: &mut Vec<Hap>,
        stop: &mut Stop,
    ) -> Option<()> {
        let Some(reach) = self.reach else {
            return Some(());
        };
        let entries = self.entries_in(order)?;
        let start = out.len();
        let answered = match &self.sampler {
            Some(sampler) if self.copyable && sampler.signal_steps.is_empty() => {
                self.emit_sampled_in_place(sampler, entries, state, reach, out, stop)
            }
            Some(sampler) => self.emit_sampled(sampler, entries, state, reach, out, stop),
            None => self.emit(entries, state.span, reach, out, stop),
        };
        if answered.is_none() {
            out.truncate(start);
        }
        answered
    }

    /// [`Self::emit_sampled`] for a bare signal over entries that own no heap
    /// memory: each period's entries are copied at once, as [`Self::emit`]
    /// copies them, and each hap then becomes the met hap where it is. A bare
    /// signal's hap carries no metadata, so the meeting keeps the entry's.
    fn emit_sampled_in_place(
        &self,
        sampler: &Sampler,
        entries: Answering<'_>,
        state: &State,
        reach: TimeSpan,
        out: &mut Vec<Hap>,
        stop: &mut Stop,
    ) -> Option<()> {
        let from = out.len();
        self.emit(entries, state.span, reach, out, stop)?;
        let mut probe = state.clone();
        let mut signal = Hap::new(None, state.span, crate::Value::Undefined);
        let mut kept = from;
        for index in from..out.len() {
            let hap = &mut out[index];
            let whole = hap.whole?;
            probe.span = whole;
            signal.part = whole;
            signal.value = (sampler.signal)(&probe);
            let met = match sampler.side {
                Side::Function => Meeting::of(&signal, hap, &sampler.apply, sampler.flow),
                Side::Value => Meeting::of(hap, &signal, &sampler.apply, sampler.flow),
            };
            met.settle(hap);
            if sampler.then.iter().all(|step| step.run_in_place(hap)) {
                if !hap.value.is_data(0) {
                    return None;
                }
                if kept != index {
                    out.swap(kept, index);
                }
                kept += 1;
            }
        }
        out.truncate(kept);
        Some(())
    }

    /// [`Self::emit`] for a sampled layer: each hap meets the signal.
    fn emit_sampled(
        &self,
        sampler: &Sampler,
        entries: Answering<'_>,
        state: &State,
        reach: TimeSpan,
        out: &mut Vec<Hap>,
        stop: &mut Stop,
    ) -> Option<()> {
        let span = state.span;
        let periods = self.periods(span, reach)?;
        if periods.first > periods.last {
            return Some(());
        }
        out.reserve(self.meetings(entries, span, periods)?.min(MAX_RESERVED));
        let mut probe = state.clone();
        let mut signal = Hap::new(None, span, crate::Value::Undefined);
        for k in periods.first..=periods.last {
            let shift = k.checked_mul(self.period)?;
            let inside = periods.is_inside(k);
            let from = out.len();
            let mut met = 0;
            let near = if inside {
                entries.haps
            } else {
                entries.near(span, shift)?
            };
            for entry in near {
                let shifted = shifted_span(entry.part, shift)?;
                let part = if inside {
                    shifted
                } else {
                    match shifted.intersection(&span) {
                        Some(part) => part,
                        None => continue,
                    }
                };
                let whole = match entry.whole? {
                    _ if self.whole_haps => shifted,
                    whole if whole == entry.part => shifted,
                    whole => shifted_span(whole, shift)?,
                };
                met += 1;
                sampler.sample(entry, whole, part, &mut probe, &mut signal, out)?;
            }
            // Every meeting counts, also one that a step dropped after.
            if !stop.allows(from, met) {
                return Some(());
            }
        }
        Some(())
    }

    /// The periods `span` meets, for parts that stay within `reach`.
    fn periods(&self, span: TimeSpan, reach: TimeSpan) -> Option<Periods> {
        let period = Fraction::int(self.period);
        let first = span
            .begin
            .checked_sub(reach.end)?
            .checked_div(period)?
            .floor()
            .numer()
            .checked_add(1)?;
        let last = span
            .end
            .checked_sub(reach.begin)?
            .checked_div(period)?
            .ceil()
            .numer()
            .checked_sub(1)?;
        // Period `k` lies inside when `reach` moved by `k` periods does.
        let inside = (
            span.begin
                .checked_sub(reach.begin)?
                .checked_div(period)?
                .ceil()
                .numer(),
            span.end
                .checked_sub(reach.end)?
                .checked_div(period)?
                .floor()
                .numer(),
        );
        Some(Periods {
            first,
            last,
            inside,
        })
    }

    /// How many entries of every period meet `span`: all of them in a period
    /// that lies inside, and those counted one by one in a period it cuts.
    fn meetings(&self, entries: Answering<'_>, span: TimeSpan, periods: Periods) -> Option<usize> {
        let mut total = periods.inside_count()?.saturating_mul(entries.haps.len());
        for k in periods.cut() {
            let shift = k.checked_mul(self.period)?;
            for entry in entries.near(span, shift)? {
                if shifted_span(entry.part, shift)?
                    .intersection(&span)
                    .is_some()
                {
                    total = total.saturating_add(1);
                }
            }
        }
        Some(total)
    }

    fn emit(
        &self,
        entries: Answering<'_>,
        span: TimeSpan,
        reach: TimeSpan,
        out: &mut Vec<Hap>,
        stop: &mut Stop,
    ) -> Option<()> {
        let all = entries.haps;
        let periods = self.periods(span, reach)?;
        if periods.first > periods.last {
            return Some(());
        }
        // The exact size first, so the room is reserved once.
        out.reserve(self.meetings(entries, span, periods)?.min(MAX_RESERVED));
        for k in periods.first..=periods.last {
            let shift = k.checked_mul(self.period)?;
            if periods.is_inside(k) {
                if !stop.allows(out.len(), all.len()) {
                    return Some(());
                }
                // A whole period: clone the entries in one go, then shift them.
                let from = out.len();
                if self.copyable {
                    out.reserve(all.len());
                    // SAFETY: every entry owns no heap memory (`copyable`), so
                    // a byte copy of it is a clone. `reserve` made room for
                    // all of them, and they are initialised before the length
                    // covers them.
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            all.as_ptr(),
                            out.as_mut_ptr().add(from),
                            all.len(),
                        );
                        out.set_len(from + all.len());
                    }
                } else {
                    out.extend_from_slice(all);
                }
                if self.whole_haps {
                    for hap in &mut out[from..] {
                        let part = shifted_span(hap.part, shift)?;
                        hap.part = part;
                        hap.whole = Some(part);
                    }
                } else {
                    for hap in &mut out[from..] {
                        let part = shifted_span(hap.part, shift)?;
                        if let Some(whole) = &mut hap.whole {
                            *whole = if *whole == hap.part {
                                part
                            } else {
                                shifted_span(*whole, shift)?
                            };
                        }
                        hap.part = part;
                    }
                }
                continue;
            }
            let from = out.len();
            for entry in entries.near(span, shift)? {
                let Some(part) = shifted_span(entry.part, shift)?.intersection(&span) else {
                    continue;
                };
                let mut hap = entry.clone();
                hap.whole = match entry.whole {
                    Some(whole) => Some(shifted_span(whole, shift)?),
                    None => None,
                };
                hap.part = part;
                out.push(hap);
            }
            if !stop.allows(from, out.len() - from) {
                return Some(());
            }
        }
        Some(())
    }
}

/// The periods a span meets: from `first` to `last` each has a part in it,
/// and from `inside.0` to `inside.1` each lies in it whole.
#[derive(Clone, Copy, Debug)]
struct Periods {
    first: i128,
    last: i128,
    inside: (i128, i128),
}

impl Periods {
    fn is_inside(self, k: i128) -> bool {
        self.inside.0 <= k && k <= self.inside.1
    }

    /// The periods that lie inside, as a range from `first` to `last`.
    fn inside_range(self) -> (i128, i128) {
        (self.inside.0.max(self.first), self.inside.1.min(self.last))
    }

    /// How many periods lie inside.
    fn inside_count(self) -> Option<usize> {
        let (from, to) = self.inside_range();
        if from > to {
            return Some(0);
        }
        usize::try_from(to.checked_sub(from)?.checked_add(1)?).ok()
    }

    /// The periods the span cuts: those before and after the inside ones.
    fn cut(self) -> impl Iterator<Item = i128> {
        let (from, to) = self.inside_range();
        let end = self.last.saturating_add(1);
        let (head, tail) = if from <= to {
            (from, to.saturating_add(1))
        } else {
            (end, end)
        };
        (self.first..head).chain(tail..end)
    }
}

/// The order an answer comes in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Order {
    /// The order the graph returns its haps in.
    Graph,
    /// The order of [`crate::sort_haps_for_query`].
    Sorted,
}

/// Whether `entries` never overlap and come in time order, also from one
/// period to the next.
fn in_time_order(entries: &[Hap], period: i128) -> bool {
    entries
        .windows(2)
        .all(|pair| pair[0].part.end <= pair[1].part.begin)
        && match (entries.first(), entries.last()) {
            (Some(first), Some(last)) => first
                .part
                .begin
                .checked_add_int(period)
                .is_some_and(|next| last.part.end <= next),
            _ => true,
        }
}

/// [`Layer::cycle_starts`] for entries that come cycle by cycle, each part
/// inside one cycle of `[0, period)`. `None` for any other entries.
fn cycle_starts(entries: &[Hap], period: i128) -> Option<Vec<u32>> {
    let cycles = usize::try_from(period).ok()?;
    if cycles > MAX_ENTRIES {
        return None;
    }
    let mut starts = Vec::with_capacity(cycles + 1);
    for (index, entry) in entries.iter().enumerate() {
        let cycle = entry.part.begin.floor().numer();
        if entry.part.end > Fraction::int(cycle.checked_add(1)?) {
            return None;
        }
        let cycle = usize::try_from(cycle).ok()?;
        // An entry before the last one's cycle breaks the order.
        if cycle >= cycles || cycle + 1 < starts.len() {
            return None;
        }
        let index = u32::try_from(index).ok()?;
        while starts.len() <= cycle {
            starts.push(index);
        }
    }
    let count = u32::try_from(entries.len()).ok()?;
    starts.resize(cycles + 1, count);
    Some(starts)
}

/// How one answer is asked for and when it stops. A query looks at
/// cancellation before its first haps, and then at cancellation and the
/// deadline every [`STOP_SAMPLE`] haps. Each hap counts as one deadline
/// check, the leaf the graph would have visited for it. A build stops past
/// [`MAX_ENTRIES`] haps and drops the answer, so no table is built from part
/// of one.
pub(crate) struct Stop {
    phase: Phase,
    /// The query frames down to the node that answers, its own included.
    depth: u32,
    /// The hap budget, read once. `None` when the query has none.
    budget: Option<u64>,
    /// Haps answered since cancellation and the deadline were last looked at.
    unsampled: usize,
}

/// Where an answer is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// A query before its first haps.
    Fresh,
    /// A query that may still grow.
    Answering,
    /// A table build.
    Building,
    /// The answer stopped. A query is refused, and a build is dropped.
    Stopped,
}

/// How many haps a query answers between two looks at cancellation and the
/// deadline.
const STOP_SAMPLE: usize = 1024;

impl Stop {
    /// A query of the node `depth` frames down.
    pub(crate) fn query(depth: u32) -> Self {
        Self {
            phase: Phase::Fresh,
            depth,
            budget: crate::hap_budget(),
            unsampled: 0,
        }
    }

    fn build() -> Self {
        Self {
            phase: Phase::Building,
            depth: 0,
            budget: None,
            unsampled: 0,
        }
    }

    /// Look at cancellation before the first haps. `false` when it stopped.
    fn begin(&mut self) -> bool {
        if self.phase == Phase::Fresh {
            self.phase = if crate::cancellation_requested() {
                Phase::Stopped
            } else {
                Phase::Answering
            };
        }
        self.phase != Phase::Stopped
    }

    /// Whether the answer may go on after growing from `len` haps by `more`.
    fn allows(&mut self, len: usize, more: usize) -> bool {
        match self.phase {
            Phase::Stopped => {}
            Phase::Building => {
                if len.saturating_add(more) > MAX_ENTRIES {
                    self.phase = Phase::Stopped;
                }
            }
            Phase::Fresh | Phase::Answering => {
                self.unsampled = self.unsampled.saturating_add(more);
                if self.unsampled >= STOP_SAMPLE
                    && (crate::cancellation_requested() || self.sample())
                {
                    self.phase = Phase::Stopped;
                }
            }
        }
        self.phase != Phase::Stopped
    }

    /// Count the haps since the last look as deadline checks.
    fn sample(&mut self) -> bool {
        let checks = u32::try_from(std::mem::take(&mut self.unsampled)).unwrap_or(u32::MAX);
        crate::query_deadline_after(checks)
    }

    /// Count the last haps of an answer as deadline checks. A deadline seen
    /// here refuses the answer at the query boundary.
    pub(crate) fn finish(&mut self) {
        if self.phase == Phase::Answering && self.unsampled > 0 {
            self.sample();
        }
    }

    fn stopped(&self) -> bool {
        self.phase == Phase::Stopped
    }
}

impl Tables {
    fn new(layers: Vec<Layer>, work: Work) -> Self {
        let mut extent = 0.0f64;
        let mut entry_bits = 0;
        for entry in layers.iter().flat_map(|layer| &layer.entries) {
            let whole = entry.whole.unwrap_or(entry.part);
            let length = whole
                .end
                .checked_sub(whole.begin)
                .map_or(f64::INFINITY, |length| length.to_f64());
            extent = extent.max(length);
            for time in [entry.part.begin, entry.part.end, whole.begin, whole.end] {
                entry_bits = entry_bits.max(denominator_bits(time));
            }
        }
        // The node's own answer is one of the walk's vectors.
        let work = Work {
            haps: work.haps.max(Count::of(&layers)),
            ..work
        };
        Self {
            layers,
            work,
            extent,
            entry_bits,
            union: OnceLock::new(),
            state: None,
        }
    }

    /// These tables, kept for the settings snapshot `state`, which was in
    /// force when their build began.
    pub(crate) fn built_under(self, state: SettingsStateId) -> Self {
        Self {
            state: Some(state),
            ..self
        }
    }

    /// Whether the settings in force are those the table was built under.
    pub(crate) fn current(&self) -> bool {
        self.state
            .as_ref()
            .is_some_and(crate::settings::is_current_state)
    }

    /// The haps for `span` in graph order, or `None` when the graph must
    /// answer.
    pub(crate) fn query(&self, state: &State, stop: &mut Stop) -> Option<Vec<Hap>> {
        self.query_in(state, Order::Graph, stop)
    }

    /// The haps for `span` sorted as [`crate::sort_haps_for_query`] sorts
    /// them, or `None` when the graph must answer.
    pub(crate) fn query_sorted(&self, state: &State, stop: &mut Stop) -> Option<Vec<Hap>> {
        if self.layers.len() > 1
            && let Some(union) = self.union()
        {
            if !self.answerable(state.span, stop)? {
                return None;
            }
            let mut haps = Vec::new();
            if stop.begin() {
                union.query_into(state, Order::Graph, &mut haps, stop)?;
            }
            // Clipping at the span's ends can reorder a few haps. The sort
            // checks the order first and keeps it when nothing moved.
            return Some(crate::sort_haps_for_query(haps));
        }
        let haps = self.query_in(state, Order::Sorted, stop)?;
        // Each layer is sorted. Equal haps keep the layer order a stable
        // sort of the graph's answer keeps.
        Some(if self.layers.len() > 1 {
            crate::sort_haps_for_query(haps)
        } else {
            haps
        })
    }

    /// Every layer's haps over one shared period, stably sorted: equal haps
    /// keep their layer's order, as a stable sort of the graph's answer
    /// keeps them. Only for layers of whole haps inside that period, so that
    /// a clipped hap can never tie with one of another layer it did not
    /// tie with before.
    fn union(&self) -> Option<&Layer> {
        self.union
            .get_or_init(|| {
                let period = self.period()?;
                let mut entries = Vec::new();
                for layer in &self.layers {
                    entries.extend(layer.repeated(period)?);
                    if entries.len() > MAX_ENTRIES {
                        return None;
                    }
                }
                let window = TimeSpan::new(Fraction::ZERO, Fraction::int(period));
                let inside = |span: TimeSpan| span.begin >= window.begin && span.end <= window.end;
                if !entries
                    .iter()
                    .all(|entry| entry.whole == Some(entry.part) && inside(entry.part))
                {
                    return None;
                }
                Layer::new(period, crate::sort_haps_for_query(entries))
            })
            .as_ref()
    }

    /// Whether a span may be answered here at all: wider than zero, and with
    /// nothing in the graph's walk for it that the graph could refuse.
    fn answerable(&self, span: TimeSpan, stop: &Stop) -> Option<bool> {
        if span.begin >= span.end {
            return Some(false);
        }
        let width = span.end.checked_sub(span.begin)?;
        let work = self.work;
        Some(
            work.cycles.at(width)? <= Fraction::int(crate::MAX_QUERY_SPAN_CYCLES)
                && stop.depth.saturating_add(work.levels)
                    <= crate::MAX_PATTERN_DEPTH.saturating_add(1)
                && stop
                    .budget
                    .is_none_or(|budget| work.haps.at(width) <= budget)
                && self.fits(span),
        )
    }

    /// Whether every time of the graph's walk for `span`, and every time of
    /// the answer, stays well inside the native range. A numerator holds the
    /// size of its value times its denominator.
    fn fits(&self, span: TimeSpan) -> bool {
        let times = self.work.times;
        let size = span.begin.to_f64().abs().max(span.end.to_f64().abs()) + self.extent;
        let below = denominator_bits(span.begin)
            .max(denominator_bits(span.end))
            .max(self.entry_bits)
            .saturating_add(times.denominator_bits);
        magnitude_bits(times.scale * size + times.shift).saturating_add(below) <= TIME_BITS
    }

    fn query_in(&self, state: &State, order: Order, stop: &mut Stop) -> Option<Vec<Hap>> {
        if !self.answerable(state.span, stop)?
            || !self.layers.iter().all(|layer| layer.answers_in(order))
        {
            return None;
        }
        let mut out = Vec::new();
        if !stop.begin() {
            return Some(out);
        }
        for layer in &self.layers {
            layer.query_into(state, order, &mut out, stop)?;
            if stop.stopped() {
                break;
            }
        }
        Some(out)
    }

    /// The whole answer for `span` while a table is built, or `None` past
    /// [`MAX_ENTRIES`] haps.
    fn answer(&self, span: TimeSpan) -> Option<Vec<Hap>> {
        // A sampled layer's values exist only in a query.
        if self.layers.iter().any(|layer| layer.sampler.is_some()) {
            return None;
        }
        let mut stop = Stop::build();
        let haps = self.query(&State::new(span), &mut stop)?;
        (!stop.stopped()).then_some(haps)
    }

    /// The least period shared by every layer.
    fn period(&self) -> Option<i128> {
        self.layers
            .iter()
            .try_fold(1i128, |period, layer| lcm(period, layer.period))
    }

    /// Every layer mapped, with the walk `work`.
    fn map_layers(&self, work: Work, f: impl FnMut(&Layer) -> Option<Layer>) -> Option<Self> {
        Some(Self::new(
            self.layers.iter().map(f).collect::<Option<_>>()?,
            work,
        ))
    }

    /// One layer of `cycles` periods, each answered cycle by cycle. This is
    /// the order of a graph that splits its queries at cycle boundaries and
    /// asks `children`, `levels_above` levels down, one cycle at a time.
    fn by_cycle(
        cycles: i128,
        children: Work,
        levels_above: u32,
        mut cycle: impl FnMut(i128) -> Option<Vec<Hap>>,
    ) -> Option<Self> {
        // Every cycle is asked, an empty one too.
        if cycles > i128::try_from(MAX_ENTRIES).ok()? {
            return None;
        }
        let mut entries = Vec::new();
        for c in 0..cycles {
            entries.extend(cycle(c)?);
            if entries.len() > MAX_ENTRIES {
                return None;
            }
        }
        let work = Work {
            cycles: Cycles::split(children.cycles)?,
            haps: children.haps.bounded(Fraction::ONE),
            levels: children.levels.saturating_add(levels_above),
            times: children.times.cycled(),
        };
        Some(Self::new(vec![Layer::new(cycles, entries)?], work))
    }

    /// The longest whole over every layer's entries.
    fn longest_whole(&self) -> Option<Fraction> {
        let mut longest = Fraction::ZERO;
        for layer in &self.layers {
            for entry in &layer.entries {
                let whole = entry.whole?;
                longest = longest.max(whole.end.checked_sub(whole.begin)?);
            }
        }
        Some(longest)
    }
}

/// Build the tables for one node from the tables of its children. `None`
/// means the node is answered by the graph.
pub(crate) fn build(node: &Node) -> Option<Tables> {
    // Each arm returns its tables as they are: a build nests once per graph
    // level, so this frame stays small.
    match node {
        Node::Silence => Some(Tables::new(Vec::new(), Work::leaf(Cycles::NONE))),
        Node::Pure(value) if value.is_data(0) => pure(value),
        Node::AddContext(pattern, locations) => {
            stepped(pattern.tables()?, Step::Locations(locations.clone()))
        }
        Node::Fast(pattern, factor, _) => fast(pattern.tables()?, *factor),
        Node::Shift(pattern, by, _) => shift(pattern.tables()?, *by),
        Node::Stack(patterns) => stack(patterns),
        Node::SplitQueries(inner) => split_queries(inner),
        Node::FmapNative(pattern, f) => stepped(pattern.tables()?, Step::Map(f.clone())),
        Node::FilterValues(pattern, keep) => stepped(pattern.tables()?, Step::Keep(keep.clone())),
        Node::MapHapsNative(pattern, f, SpanUse::Ignores) => {
            stepped(pattern.tables()?, Step::MapHap(f.clone()))
        }
        Node::MapHapsOwned(pattern, f, SpanUse::Ignores) => {
            stepped(pattern.tables()?, Step::MapOwned(f.clone()))
        }
        Node::AppLeft(functions, values, apply, flow) => match signal_of(values) {
            Some(signal) => sampled(functions.tables()?, signal, Side::Value, apply, *flow),
            None => applied(functions.tables()?, values.tables()?, |function, value| {
                let part = function.part.intersection(&value.part)?;
                Some(applied_hap(
                    function,
                    value,
                    function.whole,
                    part,
                    apply,
                    *flow,
                ))
            }),
        },
        Node::AppRight(functions, values, apply, flow) => match signal_of(functions) {
            Some(signal) => sampled(values.tables()?, signal, Side::Function, apply, *flow),
            None => applied(values.tables()?, functions.tables()?, |value, function| {
                let part = function.part.intersection(&value.part)?;
                Some(applied_hap(
                    function,
                    value,
                    value.whole,
                    part,
                    apply,
                    *flow,
                ))
            }),
        },
        _ => None,
    }
}

/// `pure(value)`: one whole hap per cycle.
fn pure(value: &crate::Value) -> Option<Tables> {
    let cycle = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
    let hap = Hap::new(Some(cycle), cycle, value.clone());
    Some(Tables::new(
        vec![Layer::new(1, vec![hap])?],
        Work::leaf(Cycles::walk()),
    ))
}

/// A span-blind per-hap step: run once per entry, or added to a sampled
/// layer's steps, since its values exist only in a query.
fn stepped(tables: &Tables, step: Step) -> Option<Tables> {
    tables.map_layers(tables.work.above(), |layer| {
        if let Some(sampler) = &layer.sampler {
            let mut stepped = Sampler::clone(sampler);
            stepped.then.push(step.clone());
            return Some(layer.clone().with_sampler(Some(Arc::new(stepped))));
        }
        let mut entries = Vec::with_capacity(layer.entries.len());
        for entry in &layer.entries {
            if let Some(mapped) = step.run(entry.clone()) {
                if mapped.whole != entry.whole || mapped.part != entry.part {
                    return None;
                }
                entries.push(mapped);
            }
        }
        Layer::new(layer.period, entries)
    })
}

/// A signal under span-blind value steps, the steps innermost first. `None`
/// for anything else, or for a graph that may not answer from tables: one
/// that may answer differently to the same query, or that binds another
/// runtime.
fn signal_of(pattern: &Pattern) -> Option<(SignalFn, Vec<Step>)> {
    if !pattern.tables_may_answer() {
        return None;
    }
    let _depth = BuildDepth::enter()?;
    let (inner, step) = match pattern.node_kind() {
        Node::Signal(signal) => return Some((signal.clone(), Vec::new())),
        Node::FmapNative(inner, f) => (inner, Step::Map(f.clone())),
        Node::FilterValues(inner, keep) => (inner, Step::Keep(keep.clone())),
        Node::MapHapsNative(inner, f, SpanUse::Ignores) => (inner, Step::MapHap(f.clone())),
        Node::MapHapsOwned(inner, f, SpanUse::Ignores) => (inner, Step::MapOwned(f.clone())),
        Node::AddContext(inner, locations) => (inner, Step::Locations(locations.clone())),
        _ => return None,
    };
    let (signal, mut steps) = signal_of(inner)?;
    steps.push(step);
    Some((signal, steps))
}

/// `structure` meeting a signal: its own haps, with values the query makes.
fn sampled(
    structure: &Tables,
    (signal, signal_steps): (SignalFn, Vec<Step>),
    side: Side,
    apply: &CombineFn,
    flow: LookupFlow,
) -> Option<Tables> {
    let chain = u32::try_from(signal_steps.len()).ok()?.saturating_add(1);
    let work = Work {
        cycles: structure.work.cycles,
        // The signal answers one hap for each whole it is asked over.
        haps: structure.work.haps.max(Count {
            per_cycle: 0,
            fixed: 1,
        }),
        levels: structure.work.levels.max(chain).saturating_add(1),
        times: structure
            .work
            .times
            .max(Times::SAME.reaching(structure.extent)),
    };
    structure.map_layers(work, |layer| {
        // One meeting per layer. A second signal is the graph's.
        if layer.sampler.is_some() {
            return None;
        }
        let sampler = Sampler {
            signal: signal.clone(),
            signal_steps: signal_steps.clone(),
            side,
            apply: apply.clone(),
            flow,
            then: Vec::new(),
        };
        Some(layer.clone().with_sampler(Some(Arc::new(sampler))))
    })
}

/// `appLeft` and `appRight`: every hap of the structure side meets the haps
/// the other side has over its whole. `meet` makes the hap for one pair, or
/// `None` when the two do not overlap.
fn applied(
    structure: &Tables,
    other: &Tables,
    meet: impl Fn(&Hap, &Hap) -> Option<Hap>,
) -> Option<Tables> {
    let period = lcm(structure.period()?, other.period()?)?;
    // The other side is asked over each whole of the structure side.
    let longest = structure.longest_whole()?;
    let work = Work {
        cycles: structure
            .work
            .cycles
            .max(other.work.cycles.bounded(longest)?),
        haps: structure.work.haps.max(other.work.haps.bounded(longest)),
        levels: structure
            .work
            .levels
            .max(other.work.levels)
            .saturating_add(1),
        times: structure
            .work
            .times
            .max(other.work.times.reaching(structure.extent)),
    };
    let mut scanned = 0usize;
    structure.map_layers(work, |layer| {
        let mut entries = Vec::new();
        // The haps cut from one whole share it, so it is asked once.
        let mut asked: Option<(TimeSpan, Vec<Hap>)> = None;
        for hap in layer.repeated(period)? {
            let whole = hap.whole?;
            if asked.as_ref().is_none_or(|(span, _)| *span != whole) {
                asked = Some((whole, other.answer(whole)?));
            }
            let (_, answer) = asked.as_ref()?;
            scanned = scanned.saturating_add(answer.len());
            if scanned > MAX_BUILD_SCAN {
                return None;
            }
            for met in answer {
                if let Some(entry) = meet(&hap, met) {
                    entries.push(entry);
                    if entries.len() > MAX_ENTRIES {
                        return None;
                    }
                }
            }
        }
        Layer::new(period, entries)
    })
}

/// `fast(factor)`: every time divided by `factor`. The new period is the
/// least whole number of cycles that holds a whole number of old periods.
fn fast(tables: &Tables, factor: Fraction) -> Option<Tables> {
    if factor <= Fraction::ZERO {
        return None;
    }
    let work = Work {
        cycles: tables.work.cycles.scaled(factor)?,
        haps: tables.work.haps.scaled(factor),
        levels: tables.work.levels.saturating_add(1),
        times: tables.work.times.scaled(factor),
    };
    tables.map_layers(work, |layer| {
        let scaled = Fraction::int(layer.period).checked_div(factor)?;
        let copies = scaled.denom();
        let period = scaled.numer();
        let entries = layer
            .repeated(layer.period.checked_mul(copies)?)?
            .iter()
            .map(|entry| retimed(entry, |t| t.checked_div(factor)))
            .collect::<Option<_>>()?;
        Layer::new(period, entries)
    })
}

/// A shift by `by` cycles: every time plus `by`.
fn shift(tables: &Tables, by: Fraction) -> Option<Tables> {
    let work = Work {
        levels: tables.work.levels.saturating_add(1),
        times: tables.work.times.shifted(by),
        ..tables.work
    };
    tables.map_layers(work, |layer| {
        // A signal is sampled where its meeting is. See `Sampler`.
        if layer.sampler.is_some() {
            return None;
        }
        let entries = layer
            .entries
            .iter()
            .map(|entry| retimed(entry, |t| t.checked_add(by)))
            .collect::<Option<_>>()?;
        Layer::new(layer.period, entries)
    })
}

/// A stack: every child's layers, one after the other.
fn stack(patterns: &[Pattern]) -> Option<Tables> {
    let mut layers = Vec::new();
    let mut work: Option<Work> = None;
    for pattern in patterns {
        let child = pattern.tables()?;
        layers.extend(child.layers.iter().cloned());
        work = Some(work.map_or(child.work, |work| work.max(child.work)));
    }
    let work = work.map_or_else(|| Work::leaf(Cycles::NONE), Work::above);
    Some(Tables::new(layers, work))
}

/// `splitQueries` answers one cycle at a time. Over a `slowcat` it picks the
/// constituent of each cycle. Over anything else it clips at cycle bounds.
fn split_queries(inner: &Pattern) -> Option<Tables> {
    match inner.node_kind() {
        Node::SlowCat(patterns) => return slowcat(patterns),
        Node::SlowCatPrime(patterns) => return slowcat_prime(patterns),
        Node::Rev(pattern) => return rev(pattern.tables()?),
        _ => {}
    }
    let tables = inner.tables()?;
    let period = tables.period()?;
    Tables::by_cycle(period, tables.work, 1, |c| tables.answer(cycle_span(c)?))
}

/// One constituent per cycle, each continuing from its own last cycle: cycle
/// `c` of `n` constituents plays cycle `c div n` of constituent `c mod n`.
fn slowcat(patterns: &[Pattern]) -> Option<Tables> {
    let count = i128::try_from(patterns.len()).ok()?;
    if count == 0 {
        return None;
    }
    let children = patterns
        .iter()
        .map(Pattern::tables)
        .collect::<Option<Vec<_>>>()?;
    let mut shared = 1i128;
    for child in &children {
        shared = lcm(shared, child.period()?)?;
    }
    let cycles = shared.checked_mul(count)?;
    let work = constituents(&children);
    Tables::by_cycle(cycles, work, 2, |c| {
        let child = &children[usize::try_from(c.rem_euclid(count)).ok()?];
        let local = c.div_euclid(count);
        let offset = c.checked_sub(local)?;
        child
            .answer(cycle_span(local)?)?
            .iter()
            .map(|hap| shifted(hap, offset))
            .collect()
    })
}

/// `slowcatPrime`: cycle `c` of `n` constituents plays cycle `c` of
/// constituent `c mod n`, so the constituents skip the cycles of the others.
fn slowcat_prime(patterns: &[Pattern]) -> Option<Tables> {
    let count = i128::try_from(patterns.len()).ok()?;
    if count == 0 {
        return None;
    }
    let children = patterns
        .iter()
        .map(Pattern::tables)
        .collect::<Option<Vec<_>>>()?;
    let mut cycles = count;
    for child in &children {
        cycles = lcm(cycles, child.period()?)?;
    }
    let work = constituents(&children);
    Tables::by_cycle(cycles, work, 2, |c| {
        children[usize::try_from(c.rem_euclid(count)).ok()?].answer(cycle_span(c)?)
    })
}

/// The walk of every constituent of an alternation, together.
fn constituents(children: &[&Tables]) -> Work {
    children
        .iter()
        .map(|child| child.work)
        .reduce(Work::max)
        .unwrap_or(Work::leaf(Cycles::NONE))
}

/// `rev`: each cycle reflected in itself, haps in the order the cycle had.
fn rev(tables: &Tables) -> Option<Tables> {
    let period = tables.period()?;
    Tables::by_cycle(period, tables.work, 2, |c| {
        let cycle = Fraction::int(c);
        let next = Fraction::int(c.checked_add(1)?);
        let reflect = |span: TimeSpan| {
            Some(TimeSpan::new(
                cycle.checked_add(next.checked_sub(span.end)?)?,
                cycle.checked_add(next.checked_sub(span.begin)?)?,
            ))
        };
        tables
            .answer(cycle_span(c)?)?
            .into_iter()
            .map(|mut hap| {
                hap.part = reflect(hap.part)?;
                hap.whole = match hap.whole {
                    Some(whole) => Some(reflect(whole)?),
                    None => None,
                };
                Some(hap)
            })
            .collect()
    })
}

/// The span of cycle `c`.
fn cycle_span(c: i128) -> Option<TimeSpan> {
    Some(TimeSpan::new(
        Fraction::int(c),
        Fraction::int(c.checked_add(1)?),
    ))
}

/// A copy of `hap` moved by whole cycles.
fn shifted(hap: &Hap, cycles: i128) -> Option<Hap> {
    let mut hap = hap.clone();
    hap.part = shifted_span(hap.part, cycles)?;
    hap.whole = match hap.whole {
        Some(whole) => Some(shifted_span(whole, cycles)?),
        None => None,
    };
    Some(hap)
}

fn shifted_span(span: TimeSpan, cycles: i128) -> Option<TimeSpan> {
    Some(TimeSpan::new(
        span.begin.checked_add_int(cycles)?,
        span.end.checked_add_int(cycles)?,
    ))
}

/// A copy of `hap` with every time mapped by an order-preserving `map`.
fn retimed(hap: &Hap, map: impl Fn(Fraction) -> Option<Fraction>) -> Option<Hap> {
    let span = |span: TimeSpan| Some(TimeSpan::new(map(span.begin)?, map(span.end)?));
    let mut hap = hap.clone();
    hap.part = span(hap.part)?;
    hap.whole = match hap.whole {
        Some(whole) => Some(span(whole)?),
        None => None,
    };
    Some(hap)
}

fn lcm(a: i128, b: i128) -> Option<i128> {
    fn gcd(mut a: i128, mut b: i128) -> i128 {
        while b != 0 {
            (a, b) = (b, a % b);
        }
        a
    }
    a.checked_div(gcd(a, b))?.checked_mul(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::{Alignment, ComposeOp, compose};
    use crate::{Value, fastcat, pure, silence, slowcat, stack};

    fn frac(n: i128, d: i128) -> Fraction {
        Fraction::new(n, d)
    }

    /// The same graph twice: once with tables, once with volatile leaves,
    /// which keep every node above them on the graph.
    struct Twin {
        tabled: Pattern,
        graph: Pattern,
    }

    fn leaf(value: f64, location: usize) -> Twin {
        let value = Value::F64(value);
        Twin {
            tabled: pure(value.clone()).with_added_context(vec![(location, location + 1)]),
            graph: pure(value)
                .mark_volatile()
                .with_added_context(vec![(location, location + 1)]),
        }
    }

    fn map(twins: Vec<Twin>, f: impl Fn(Vec<Pattern>) -> Pattern) -> Twin {
        let (tabled, graph): (Vec<_>, Vec<_>) = twins
            .into_iter()
            .map(|twin| (twin.tabled, twin.graph))
            .unzip();
        Twin {
            tabled: f(tabled),
            graph: f(graph),
        }
    }

    fn one(twin: Twin, f: impl Fn(&Pattern) -> Pattern) -> Twin {
        Twin {
            tabled: f(&twin.tabled),
            graph: f(&twin.graph),
        }
    }

    fn seq(values: &[f64]) -> Twin {
        map(
            values
                .iter()
                .enumerate()
                .map(|(index, value)| leaf(*value, index * 2))
                .collect(),
            fastcat,
        )
    }

    fn describe(haps: &[Hap]) -> Vec<String> {
        haps.iter()
            .map(|hap| format!("{} {:?}", hap.show(), hap.context))
            .collect()
    }

    fn spans() -> Vec<(Fraction, Fraction)> {
        let mut spans = vec![
            (frac(0, 1), frac(1, 1)),
            (frac(0, 1), frac(16, 1)),
            (frac(1, 3), frac(5, 2)),
            (frac(-7, 4), frac(1, 5)),
            (frac(3, 1), frac(7, 2)),
            (frac(1, 7), frac(2, 7)),
            (frac(-3, 1), frac(-1, 1)),
            (frac(5, 8), frac(41, 8)),
            (frac(99, 1), frac(103, 3) * frac(3, 1)),
        ];
        for begin in -5..12 {
            for width in [1, 2, 3, 5, 8, 13] {
                spans.push((frac(begin, 4), frac(begin, 4) + frac(width, 6)));
            }
        }
        spans
    }

    fn assert_same(name: &str, twin: &Twin) {
        assert_same_over(name, twin, &spans());
    }

    fn assert_same_over(name: &str, twin: &Twin, spans: &[(Fraction, Fraction)]) {
        assert!(twin.tabled.tables().is_some(), "{name} has no table");
        assert!(
            twin.graph.tables().is_none(),
            "{name}: the graph twin has a table"
        );
        for &(begin, end) in spans {
            let answer = twin.tabled.query_arc(begin, end);
            let state = crate::State::new(TimeSpan::new(begin, end));
            let sorted = match twin
                .tabled
                .query_arc_outcome_sorted_with_budget(&state, u64::MAX)
            {
                Ok(crate::QueryArcOutcome::Haps(haps)) => describe(&haps),
                other => panic!("{name}: {other:?}"),
            };
            assert_eq!(
                sorted,
                describe(&crate::sort_haps_for_query(answer.clone())),
                "{name} sorted over {begin} .. {end}"
            );
            let tabled = describe(&answer);
            let graph = describe(&twin.graph.query_arc(begin, end));
            assert_eq!(tabled, graph, "{name} over {begin} .. {end}");
        }
    }

    #[test]
    fn sequences_match_the_graph() {
        assert_same("seq", &seq(&[0.0, 1.0, 2.0]));
        assert_same("one", &seq(&[4.0]));
        assert_same(
            "nested",
            &map(
                vec![seq(&[0.0, 1.0]), leaf(2.0, 9), seq(&[3.0, 4.0, 5.0])],
                fastcat,
            ),
        );
    }

    #[test]
    fn speed_changes_and_shifts_match_the_graph() {
        for (n, d) in [(2, 1), (3, 1), (1, 2), (2, 3), (3, 2), (5, 7)] {
            assert_same(
                &format!("fast {n}/{d}"),
                &one(seq(&[0.0, 1.0, 2.0]), |p| p.fast(frac(n, d))),
            );
        }
        for (n, d) in [(1, 3), (-1, 4), (5, 2), (-7, 3)] {
            assert_same(
                &format!("late {n}/{d}"),
                &one(seq(&[0.0, 1.0]), |p| p.late(frac(n, d))),
            );
        }
        assert_same(
            "early then slow",
            &one(seq(&[0.0, 1.0, 2.0, 3.0]), |p| {
                p.early(frac(1, 4)).slow(frac(3, 2))
            }),
        );
    }

    #[test]
    fn alternations_and_stacks_match_the_graph() {
        assert_same(
            "slowcat",
            &map(vec![seq(&[0.0, 1.0]), leaf(2.0, 9), seq(&[3.0])], slowcat),
        );
        assert_same(
            "slowcat of slow",
            &map(
                vec![
                    one(seq(&[0.0, 1.0, 2.0]), |p| p.slow(frac(2, 1))),
                    leaf(5.0, 7),
                ],
                slowcat,
            ),
        );
        assert_same(
            "stack",
            &map(
                vec![
                    seq(&[0.0, 1.0]),
                    one(seq(&[2.0, 3.0, 4.0]), |p| p.late(frac(1, 6))),
                ],
                stack,
            ),
        );
        assert_same(
            "slowcat of stack",
            &map(
                vec![
                    map(vec![seq(&[0.0, 1.0]), leaf(2.0, 9)], stack),
                    seq(&[3.0, 4.0]),
                ],
                slowcat,
            ),
        );
        assert_same(
            "fastcat of slowcat",
            &map(
                vec![
                    map(vec![leaf(1.0, 1), leaf(2.0, 3)], slowcat),
                    seq(&[3.0, 4.0]),
                ],
                fastcat,
            ),
        );
    }

    #[test]
    fn value_maps_match_the_graph() {
        assert_same(
            "fmap",
            &one(seq(&[0.0, 1.0, 2.0]), |p| {
                p.fmap(|value| Value::F64(value.as_f64().unwrap_or(0.0) * 2.0))
            }),
        );
    }

    fn named(twin: Twin, name: &str) -> Twin {
        let spec = crate::controls::ControlSpec::new([name]);
        one(twin, |p| spec.pattern(p))
    }

    fn composed(left: Twin, right: Twin, op: ComposeOp, how: Alignment) -> Twin {
        Twin {
            tabled: compose(&left.tabled, &right.tabled, op, how),
            graph: compose(&left.graph, &right.graph, op, how),
        }
    }

    #[test]
    fn controls_and_their_combinations_match_the_graph() {
        let drums = named(seq(&[0.0, 1.0, 2.0, 3.0, 4.0, 5.0]), "n");
        let gains = named(seq(&[0.8, 0.6]), "gain");
        assert_same("n", &named(seq(&[0.0, 1.0, 2.0]), "n"));
        assert_same(
            "set in",
            &composed(drums, gains, ComposeOp::Set, Alignment::In),
        );
        let notes = named(seq(&[0.0, 4.0, 7.0]), "note");
        let offsets = named(map(vec![leaf(0.0, 20), leaf(12.0, 22)], slowcat), "note");
        assert_same(
            "add in",
            &composed(notes, offsets, ComposeOp::Add, Alignment::In),
        );
        let left = named(one(seq(&[0.0, 1.0, 2.0]), |p| p.slow(frac(3, 2))), "n");
        let right = named(seq(&[5.0, 6.0]), "gain");
        assert_same(
            "set out",
            &composed(left, right, ComposeOp::Set, Alignment::Out),
        );
    }

    #[test]
    fn reversed_and_skipping_cycles_match_the_graph() {
        assert_same("rev", &one(seq(&[0.0, 1.0, 2.0]), |p| p.rev()));
        assert_same(
            "rev of a shifted slow sequence",
            &one(seq(&[0.0, 1.0, 2.0]), |p| {
                p.slow(frac(2, 1)).late(frac(1, 3)).rev()
            }),
        );
        let every = map(
            vec![
                one(seq(&[0.0, 1.0, 2.0, 3.0]), |p| p.rev()),
                seq(&[0.0, 1.0, 2.0, 3.0]),
                seq(&[0.0, 1.0, 2.0, 3.0]),
            ],
            crate::slowcat_prime,
        );
        assert_same("slowcatPrime", &every);
    }

    #[test]
    fn euclidean_rhythms_match_the_graph() {
        for (pulses, steps, rotation) in [(3, 8, 0), (5, 8, 2), (7, 4, 0), (2, 5, 9)] {
            assert_same(
                &format!("euclid {pulses},{steps},{rotation}"),
                &one(named(seq(&[0.0, 1.0]), "s"), |p| {
                    crate::combinators::euclid_with_steps(p, pulses, f64::from(steps), rotation)
                }),
            );
        }
    }

    #[test]
    fn stacks_of_whole_sequences_match_the_graph_sorted_and_unsorted() {
        let drums = map(
            vec![
                one(seq(&[1.0]), |p| p.fast(frac(4, 1))),
                one(seq(&[2.0]), |p| p.fast(frac(8, 1))),
                seq(&[3.0, 4.0]),
                one(seq(&[5.0, 6.0, 7.0]), |p| p.slow(frac(2, 1))),
                seq(&[8.0, 9.0, 10.0, 11.0]),
            ],
            stack,
        );
        assert!(
            drums
                .tabled
                .tables()
                .is_some_and(|tables| tables.union().is_some())
        );
        assert_same("stack of sequences", &drums);
    }

    #[test]
    fn a_signal_met_by_a_structure_matches_the_graph() {
        let segmented = Twin {
            tabled: crate::combinators::segment(&crate::signal::rand(), frac(8, 1)),
            graph: crate::combinators::segment(&crate::signal::rand().mark_volatile(), frac(8, 1)),
        };
        assert_same("rand.segment", &segmented);
        let gains = composed(
            named(seq(&[0.0, 1.0, 2.0]), "s"),
            named(
                Twin {
                    tabled: crate::signal::rand(),
                    graph: crate::signal::rand().mark_volatile(),
                },
                "gain",
            ),
            ComposeOp::Set,
            Alignment::In,
        );
        assert_same("set in with a signal", &gains);
        let located = |signal: Pattern| {
            crate::combinators::segment(&signal.with_added_context(vec![(50, 51)]), frac(4, 1))
                .with_added_context(vec![(60, 61)])
        };
        assert_same(
            "locations on both sides of a meeting",
            &Twin {
                tabled: located(crate::signal::rand()),
                graph: located(crate::signal::rand().mark_volatile()),
            },
        );
    }

    #[test]
    fn a_scale_matches_the_graph() {
        let degrees = named(seq(&[0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]), "n");
        assert_same(
            "scale",
            &one(degrees, |p| {
                crate::combinators::scale(p, Value::Str("c:major".into()))
            }),
        );
    }

    #[test]
    fn a_silent_step_and_silence_match_the_graph() {
        let twin = map(
            vec![
                leaf(1.0, 1),
                Twin {
                    tabled: silence(),
                    graph: silence(),
                },
            ],
            fastcat,
        );
        assert_same("rest", &twin);
    }

    #[test]
    fn a_zero_width_or_inverted_span_goes_to_the_graph() {
        let pattern = seq(&[0.0, 1.0]).tabled;
        let tables = pattern.tables().expect("a sequence has a table");
        let point = TimeSpan::new(frac(1, 2), frac(1, 2));
        assert!(
            tables
                .query(&crate::State::new(point), &mut Stop::query(1))
                .is_none()
        );
        let inverted = TimeSpan::new(frac(1, 1), frac(1, 2));
        assert!(
            tables
                .query(&crate::State::new(inverted), &mut Stop::query(1))
                .is_none()
        );
    }

    #[test]
    fn a_graph_deeper_than_a_build_walks_is_left_to_the_graph() {
        let mut deep = pure(Value::F64(1.0));
        for _ in 0..20_000 {
            deep = deep.fast(frac(2, 1)).slow(frac(2, 1));
        }
        assert!(deep.tables().is_none());
        let mut shallow = pure(Value::F64(1.0));
        for _ in 0..20 {
            shallow = shallow.fast(frac(2, 1)).slow(frac(2, 1));
        }
        assert!(shallow.tables().is_some());
    }

    #[test]
    fn bitwise_copies_of_whole_periods_clone_the_entries() {
        let twin = named(seq(&[0.0, 1.0, 2.0]), "n");
        let tables = twin
            .tabled
            .tables()
            .expect("a control sequence has a table");
        assert!(tables.layers.iter().all(|layer| layer.copyable));
        let answer = twin.tabled.query_arc(frac(0, 1), frac(3, 1));
        assert_eq!(
            describe(&answer),
            describe(&twin.graph.query_arc(frac(0, 1), frac(3, 1)))
        );
        drop(answer);
        let clipped = twin.tabled.query_arc(frac(1, 2), frac(5, 2));
        assert_eq!(
            describe(&clipped),
            describe(&twin.graph.query_arc(frac(1, 2), frac(5, 2)))
        );
    }

    #[test]
    fn a_script_value_has_no_table() {
        let pattern = pure(Value::JsValue(crate::value::JsValueRef::new(7, false)));
        assert!(pattern.tables().is_none());
    }

    fn outcome(pattern: &Pattern, span: TimeSpan, budget: u64) -> Result<Vec<String>, String> {
        let state = crate::State::new(span);
        match pattern.query_arc_outcome_with_budget(&state, budget) {
            Ok(crate::QueryArcOutcome::Haps(haps)) => Ok(describe(&haps)),
            Ok(other) => Err(format!("{other:?}")),
            Err(limit) => Err(format!("{limit:?}")),
        }
    }

    fn rand_twin() -> Twin {
        Twin {
            tabled: crate::signal::rand(),
            graph: crate::signal::rand().mark_volatile(),
        }
    }

    #[test]
    fn an_empty_layer_under_a_large_factor_and_a_sparse_cycle_walk_build_at_once() {
        let never = pure(Value::F64(1.0)).filter_values(|_| false);
        let fast = never.fast(frac(1_000_000_000_000, 1));
        let tables = fast.tables().expect("nothing repeated is a table");
        assert!(tables.layers.iter().all(|layer| layer.entries.is_empty()));
        let sparse = fastcat(vec![silence(), pure(Value::F64(1.0))])
            .slow(frac(1_000_000_000, 1))
            .rev();
        assert!(sparse.tables().is_none());
    }

    #[test]
    fn a_throw_in_a_sampled_layer_stays_with_its_stack_child() {
        let throwing = one(rand_twin(), |signal| {
            crate::combinators::segment(signal, frac(4, 1)).fmap(|value| {
                crate::signal_query_error(|| "no scale".into());
                value.clone()
            })
        });
        let both = map(vec![seq(&[1.0, 2.0]), throwing], stack);
        assert!(both.tabled.tables().is_some());
        let span = TimeSpan::new(frac(0, 1), frac(2, 1));
        let tabled = outcome(&both.tabled, span, u64::MAX);
        assert_eq!(tabled, outcome(&both.graph, span, u64::MAX));
        assert_eq!(tabled.map(|haps| haps.len()), Ok(4));
    }

    #[test]
    fn a_hap_budget_refuses_exactly_where_the_graph_refuses() {
        let span = TimeSpan::new(frac(0, 1), frac(1, 1));
        let dropped = one(seq(&[1.0]), |p| {
            p.fast(frac(2000, 1)).filter_values(|_| false)
        });
        let refused = outcome(&dropped.graph, span, 1000);
        assert!(refused.is_err());
        assert_eq!(outcome(&dropped.tabled, span, 1000), refused);
        let hats = one(seq(&[1.0]), |p| p.fast(frac(600, 1)));
        let kicks = one(rand_twin(), |signal| {
            crate::combinators::segment(signal, frac(600, 1))
                .filter_values(|value| value.as_f64().is_some_and(|value| value < 0.1))
        });
        let both = map(vec![hats, kicks], stack);
        let answered = outcome(&both.graph, span, 1000);
        assert!(answered.is_ok());
        assert_eq!(outcome(&both.tabled, span, 1000), answered);
    }

    #[test]
    fn a_query_the_graph_refuses_for_depth_or_time_goes_to_the_graph() {
        let tables = seq(&[0.0, 1.0]).tabled.tables().expect("a table").clone();
        let span = TimeSpan::new(frac(0, 1), frac(1, 1));
        let deepest = crate::MAX_PATTERN_DEPTH + 1 - tables.work.levels;
        assert_eq!(tables.answerable(span, &Stop::query(deepest)), Some(true));
        assert_eq!(
            tables.answerable(span, &Stop::query(deepest + 1)),
            Some(false)
        );
        let far = Fraction::int(1i128 << 118);
        let far = TimeSpan::new(far, far + Fraction::ONE);
        assert_eq!(tables.answerable(far, &Stop::query(1)), Some(false));
        let huge = Fraction::int(100_000_000_000_000_000_000);
        let shifted = one(seq(&[1.0]), |p| {
            p.slow(huge)
                .fast(huge)
                .late(Fraction::int(10_000_000_000_000_000_000))
        });
        let refused = outcome(&shifted.graph, span, u64::MAX);
        assert!(refused.is_err());
        assert_eq!(outcome(&shifted.tabled, span, u64::MAX), refused);
    }

    #[test]
    fn a_node_made_inside_a_query_builds_its_table_on_its_second_query() {
        let made = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = made.clone();
        let joined = pure(Value::F64(1.0))
            .fmap_to_pattern(move |value| {
                let inner = pure(value.clone()).fast(frac(2, 1));
                seen.lock().expect("not poisoned").push(inner.clone());
                inner
            })
            .inner_join();
        let span = (frac(0, 1), frac(1, 1));
        let _ = joined.query_arc(span.0, span.1);
        let inner = made.lock().expect("not poisoned")[0].clone();
        assert!(inner.built_tables().is_none(), "one query builds nothing");
        let _ = inner.query_arc(span.0, span.1);
        assert!(inner.built_tables().is_some(), "a second query builds it");
        let top = pure(Value::F64(1.0)).fast(frac(2, 1));
        let _ = top.query_arc(span.0, span.1);
        assert!(
            top.built_tables().is_some(),
            "made outside a query, built at once"
        );
    }

    #[test]
    fn meetings_made_in_byte_copies_match_the_graph() {
        let segmented = one(rand_twin(), |signal| {
            crate::combinators::segment(signal, frac(8, 1))
        });
        let tables = segmented.tabled.tables().expect("a sampled table");
        assert!(
            tables
                .layers
                .iter()
                .all(|layer| layer.copyable && layer.sampler.is_some())
        );
        for (begin, end) in [(frac(0, 1), frac(2, 1)), (frac(1, 3), frac(7, 4))] {
            assert_eq!(
                describe(&segmented.tabled.query_arc(begin, end)),
                describe(&segmented.graph.query_arc(begin, end)),
                "{begin} .. {end}"
            );
        }
    }

    #[test]
    fn a_table_answers_only_under_the_settings_it_was_built_under() {
        use crate::rng::{RngMode, rng_mode, use_rng};
        crate::settings::RuntimeSettings::default().with(|| {
            use_rng(RngMode::Legacy);
            let mode = pure(Value::F64(0.0)).fmap(|_| {
                Value::F64(if rng_mode() == RngMode::Precise {
                    1.0
                } else {
                    0.0
                })
            });
            let legacy = describe(&mode.query_arc(frac(0, 1), frac(1, 1)));
            assert!(mode.built_tables().is_some());
            use_rng(RngMode::Precise);
            assert!(
                mode.built_tables().is_none(),
                "a new setting leaves the table"
            );
            let precise = describe(&mode.query_arc(frac(0, 1), frac(1, 1)));
            assert_ne!(legacy, precise);
        });
    }

    #[test]
    fn a_throw_before_a_layer_with_no_sorted_form_stays_with_its_stack_child() {
        let throwing = one(rand_twin(), |signal| {
            crate::combinators::segment(signal, frac(4, 1)).fmap(|value| {
                crate::signal_query_error(|| "no scale".into());
                value.clone()
            })
        });
        let chord = map(
            vec![map(vec![leaf(0.0, 1), leaf(1.0, 3)], stack), leaf(2.0, 5)],
            fastcat,
        );
        let both = map(vec![throwing, chord], stack);
        let state = crate::State::new(TimeSpan::new(frac(0, 1), frac(1, 1)));
        let sorted = |pattern: &Pattern| match pattern
            .query_arc_outcome_sorted_with_budget(&state, u64::MAX)
        {
            Ok(crate::QueryArcOutcome::Haps(haps)) => Ok(describe(&haps)),
            Ok(other) => Err(format!("{other:?}")),
            Err(limit) => Err(format!("{limit:?}")),
        };
        let tabled = sorted(&both.tabled);
        assert_eq!(tabled, sorted(&both.graph));
        assert_eq!(tabled.map(|haps| haps.len()), Ok(3));
    }

    #[test]
    fn a_cancelled_or_late_query_is_refused_as_the_graph_refuses_it() {
        let many = named(seq(&[0.0; 64]), "n");
        let span = TimeSpan::new(frac(0, 1), frac(64, 1));
        let flag = std::sync::atomic::AtomicBool::new(true);
        let cancelled = |pattern: &Pattern| {
            crate::with_cancellation(&flag, || outcome(pattern, span, u64::MAX))
        };
        let refused = cancelled(&many.graph);
        assert!(refused.is_err());
        assert_eq!(cancelled(&many.tabled), refused);
        let past = std::time::Instant::now();
        let late = |pattern: &Pattern| {
            crate::with_query_deadline(past, || outcome(pattern, span, u64::MAX))
        };
        let refused = late(&many.graph);
        assert!(refused.is_err());
        assert_eq!(late(&many.tabled), refused);
    }

    /// Spans one twentieth of a cycle wide, as a live tick asks, here and
    /// there over `cycles` cycles.
    fn ticks(cycles: i128) -> Vec<(Fraction, Fraction)> {
        (0..cycles * 20)
            .step_by(7)
            .map(|tick| (frac(tick, 20), frac(tick + 1, 20)))
            .collect()
    }

    fn alternation(count: usize) -> Twin {
        map(
            (0..count).map(|index| leaf(index as f64, index)).collect(),
            slowcat,
        )
    }

    fn chords(count: usize) -> Twin {
        map(
            (0..count)
                .map(|index| {
                    map(
                        vec![leaf(index as f64, 3 * index), leaf(0.5, 3 * index + 1)],
                        stack,
                    )
                })
                .collect(),
            slowcat,
        )
    }

    #[test]
    fn narrow_queries_of_long_alternations_match_the_graph() {
        assert_same_over("alternation", &alternation(300), &ticks(300));
        assert_same_over("chords", &chords(200), &ticks(200));
        assert_same_over(
            "alternation with a sequence",
            &composed(
                named(alternation(64), "n"),
                named(seq(&[0.0, 7.0]), "n"),
                ComposeOp::Add,
                Alignment::In,
            ),
            &ticks(64),
        );
    }

    #[test]
    fn a_narrow_query_looks_at_its_own_cycle_only() {
        let tick = TimeSpan::new(frac(5003, 20), frac(5004, 20));
        for (name, twin, per_cycle) in [
            ("alternation", alternation(1000), 1),
            ("chords", chords(1000), 2),
        ] {
            let tables = twin.tabled.tables().expect("a table");
            let [layer] = tables.layers.as_slice() else {
                panic!("{name}: one layer");
            };
            let entries = layer.entries_in(Order::Graph).expect("graph order");
            assert_eq!(
                entries.near(tick, 0).map(<[Hap]>::len),
                Some(per_cycle),
                "{name}"
            );
        }
    }
}
