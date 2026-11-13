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

use crate::compose::ComposeOp;
use crate::settings::SettingsStateId;
use crate::{
    Combine, Hap, HapExpandFn, HapMapFn, JoinMode, LookupFlow, Meeting, Node, OwnedHapMapFn,
    Pattern, PickIndexMode, PickLookup, SignalFn, SpanUse, State, TimeSpan, ValueFn,
    ValuePredicate, applied_hap, joined_hap, picked_hap,
};
use rustel_fraction::Fraction;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

/// The most haps one table may hold, and the most cycles a build answers
/// one at a time. Above it the graph answers.
const MAX_ENTRIES: usize = 8192;

/// The most haps one query reserves room for ahead of making them.
const MAX_RESERVED: usize = 1 << 16;

// Buffers of haps that one query hands out and takes back, so that a
// nested answer does not allocate its own. Per thread, without locks; a
// buffer returns on drop, empty but with its room.
thread_local! {
    static HAP_BUFFERS: std::cell::RefCell<Vec<Vec<Hap>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// One buffer from [`HAP_BUFFERS`], returned when it drops.
struct HapBuffer(Vec<Hap>);

impl HapBuffer {
    fn take() -> Self {
        let pooled = HAP_BUFFERS.with(|pool| pool.borrow_mut().pop());
        Self(pooled.unwrap_or_default())
    }
}

impl Drop for HapBuffer {
    fn drop(&mut self) {
        if self.0.capacity() <= MAX_RESERVED {
            self.0.clear();
            HAP_BUFFERS.with(|pool| pool.borrow_mut().push(std::mem::take(&mut self.0)));
        }
    }
}

impl std::ops::Deref for HapBuffer {
    type Target = Vec<Hap>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for HapBuffer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

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
    /// the structure, and these make each value. See [`Sampled`].
    sampled: Option<Arc<Sampled>>,
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

/// The entries of one period that may meet a span. The ones in `inside`
/// lie in it whole. The others may need clipping, or not meet it at all.
struct Near<'a> {
    haps: &'a [Hap],
    /// Where `haps` begin among the entries.
    start: usize,
    inside: std::ops::Range<usize>,
}

impl Answering<'_> {
    /// The entries of the period `shift` cycles on that may meet `span`. A
    /// narrow query of a long period looks at its own cycle only, as the
    /// graph does.
    fn near(&self, span: TimeSpan, shift: i128) -> Option<Near<'_>> {
        let back = shift.checked_neg()?;
        let begin = span.begin.checked_add_int(back)?;
        let end = span.end.checked_add_int(back)?;
        let (haps, start, first, last) = match self.find {
            Find::Search => {
                let from = self.haps.partition_point(|hap| hap.part.end <= begin);
                let to = from + self.haps[from..].partition_point(|hap| hap.part.begin < end);
                let haps = &self.haps[from..to];
                // Begins and ends both rise, so the parts inside come together.
                (
                    haps,
                    from,
                    haps.partition_point(|hap| hap.part.begin < begin),
                    haps.partition_point(|hap| hap.part.end <= end),
                )
            }
            Find::ByCycle(starts) => {
                let cycles = starts.len() - 1;
                let at = |cycle: i128| {
                    let cycle = usize::try_from(cycle.max(0)).unwrap_or(usize::MAX);
                    starts[cycle.min(cycles)] as usize
                };
                let from = at(begin.floor().numer());
                // Each part lies in its own cycle, so the cycles inside the
                // span hold the parts inside it.
                (
                    &self.haps[from..at(end.ceil().numer())],
                    from,
                    at(begin.ceil().numer()) - from,
                    at(end.floor().numer()) - from,
                )
            }
            Find::Scan => (self.haps, 0, 0, 0),
        };
        Some(Near {
            haps,
            start,
            inside: first..last.max(first),
        })
    }

    /// At most how many haps the period `shift` cycles on gives `span`.
    fn most(&self, span: TimeSpan, shift: i128) -> Option<usize> {
        let near = self.near(span, shift)?;
        if !matches!(self.find, Find::Scan) {
            return Some(near.haps.len());
        }
        // A scan may look at many more entries than meet the span.
        let mut count = 0usize;
        for entry in near.haps {
            if shifted_span(entry.part, shift)?
                .intersection(&span)
                .is_some()
            {
                count += 1;
            }
        }
        Some(count)
    }
}

/// The values of a layer that meets signals: `appLeft` or `appRight` with a
/// signal on the other side, once or more, with value steps between and
/// after. The entries are the structure. Each meeting samples its signal
/// over the hap's whole, as the graph does, so its values exist only in a
/// query.
#[derive(Clone)]
struct Sampler {
    stages: Vec<Stage>,
    /// The most haps one entry becomes: a signal that meets a constant can
    /// cut a hap at the constant's cycle bounds.
    most: u64,
}

/// One meeting of a [`Sampler`], or a value step after one.
#[derive(Clone)]
enum Stage {
    Meet {
        signal: Signal,
        /// The side of the meeting the signal is on.
        side: Side,
        apply: Combine,
        flow: LookupFlow,
    },
    Step(Step),
    /// The outer hap of a pick or a join, met by each hap after the inner
    /// pattern's own meetings, as the graph meets it.
    Outer(Arc<Outer>),
    /// Meetings in a row, each with a constant whose keys it sets on the
    /// structure's value: `signal` is the constant that they make together.
    /// Haps that can meet it in place, each in one of its cycles, meet it
    /// as one. Others meet `each` in turn.
    Fused {
        signal: Signal,
        each: Vec<Stage>,
    },
}

/// The outer hap of a pick or a join, which a [`Stage::Outer`] merges into
/// each hap: its context first, then the inner hap's.
struct Outer {
    hap: Hap,
    /// A pick's merge, or else a join's. See `picked_hap` and `joined_hap`.
    picked: bool,
}

impl Outer {
    /// `inner` as the pick or the join makes it of the outer hap, at the
    /// whole and part the rule gave it.
    fn merge(&self, inner: Hap) -> Hap {
        let (whole, part) = (inner.whole, inner.part);
        if self.picked {
            picked_hap(&self.hap, inner, whole, part)
        } else {
            joined_hap(&self.hap, inner, whole)
        }
    }
}

impl std::fmt::Debug for Sampler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sampler")
            .field("stages", &self.stages.len())
            .field("most", &self.most)
            .finish_non_exhaustive()
    }
}

/// The samplers of a sampled layer's entries: entry `i` runs
/// `samplers[of[i]]`. An entry whose value is its own has a sampler with no
/// meeting.
#[derive(Debug)]
struct Sampled {
    samplers: Vec<Arc<Sampler>>,
    of: Vec<u32>,
}

impl Sampled {
    /// Whether every entry runs the same sampler.
    fn is_uniform(&self) -> bool {
        self.samplers.len() == 1
    }

    /// The sampler every entry runs, when they all run one.
    fn uniform(&self) -> Option<&Sampler> {
        match self.samplers.as_slice() {
            [sampler] => Some(sampler),
            _ => None,
        }
    }

    /// The sampler entry `index` runs.
    fn of(&self, index: usize) -> Option<&Sampler> {
        let sampler = usize::try_from(*self.of.get(index)?).ok()?;
        self.samplers.get(sampler).map(|sampler| &**sampler)
    }

    /// The most haps all the entries become together.
    fn total(&self) -> u64 {
        let mosts: Vec<u64> = self.samplers.iter().map(|sampler| sampler.most).collect();
        self.of.iter().fold(0u64, |total, &sampler| {
            let most = usize::try_from(sampler)
                .ok()
                .and_then(|sampler| mosts.get(sampler))
                .copied()
                .unwrap_or(u64::MAX);
            total.saturating_add(most)
        })
    }

    /// The most haps one entry becomes.
    fn most(&self) -> u64 {
        self.samplers
            .iter()
            .map(|sampler| sampler.most)
            .max()
            .unwrap_or(1)
    }

    /// Every sampler changed by `change`.
    fn map(&self, change: impl Fn(&Sampler) -> Option<Sampler>) -> Option<Self> {
        Some(Self {
            samplers: self
                .samplers
                .iter()
                .map(|sampler| change(sampler).map(Arc::new))
                .collect::<Option<_>>()?,
            of: self.of.clone(),
        })
    }

    /// The samplers of `copies` copies of the entries, as
    /// [`Layer::copies`] repeats them.
    fn repeated(&self, copies: usize) -> Option<Self> {
        let mut of = Vec::with_capacity(self.of.len().checked_mul(copies)?);
        for _ in 0..copies {
            of.extend_from_slice(&self.of);
        }
        Some(Self {
            samplers: self.samplers.clone(),
            of,
        })
    }
}

#[derive(Clone, Copy, Debug)]
enum Side {
    /// `appRight`: the signal gives the function values.
    Function,
    /// `appLeft`: the signal gives the argument values.
    Value,
}

/// What a meeting meets, and the nodes of its own graph above it, innermost
/// first: what the graph does to the source's haps before the meeting.
#[derive(Clone)]
struct Signal {
    source: Source,
    ops: Vec<SignalOp>,
}

/// The pattern at the bottom of a [`Signal`].
#[derive(Clone)]
enum Source {
    /// A signal: one hap over the span it is asked over, valued there.
    Signal(SignalFn),
    /// A constant: one whole hap a cycle, which is `hap` in cycle zero.
    Constant(Box<Hap>),
    /// The haps a pattern answers over one whole. See [`Fixed`].
    Fixed(Arc<Fixed>),
}

/// The haps a pattern answers over `whole`, a whole of period zero, with the
/// sampler of each one a query makes the value of. Asked over the same
/// whole in another period, it answers the same haps moved there.
struct Fixed {
    whole: TimeSpan,
    haps: Vec<Hap>,
    samplers: Vec<Option<Arc<Sampler>>>,
}

/// What the per-cycle nodes of a [`Signal`] chose on the way in, innermost
/// last, kept in place: a signal under more of them answers through the
/// graph.
struct Chosen {
    taken: [Fraction; 8],
    len: usize,
}

impl Default for Chosen {
    fn default() -> Self {
        Self {
            taken: [Fraction::ZERO; 8],
            len: 0,
        }
    }
}

impl Chosen {
    fn push(&mut self, choice: Fraction) -> Option<()> {
        *self.taken.get_mut(self.len)? = choice;
        self.len += 1;
        Some(())
    }

    fn pop(&mut self) -> Option<Fraction> {
        self.len = self.len.checked_sub(1)?;
        self.taken.get(self.len).copied()
    }
}

/// One node between a signal and its meeting.
#[derive(Clone)]
enum SignalOp {
    /// A span-blind value step.
    Step(Step),
    /// `fast(factor)` of what lies inside: it is asked at `t * factor`.
    Fast(Fraction),
    /// A shift by `by` of what lies inside: it is asked at `t - by`.
    Shift(Fraction),
    /// A map of each cycle of what lies inside, chosen by the cycle that the
    /// hap's part lies in at this level, as the node chooses by the cycle of
    /// its query.
    Cycle(CycleMap),
    /// `pickRestart`: what lies inside is moved to start where its selector
    /// hap starts. That hap starts at `begin` in period zero and every
    /// `period` cycles on; the hap's part says which one it is in.
    Restart { begin: Fraction, period: i128 },
    /// A meeting with a constant, a pattern of one whole hap a cycle: `hap`
    /// is its hap of cycle zero, on `side` of the meeting. The signal keeps
    /// the structure, so its hap is cut at the cycle bounds.
    Constant {
        hap: Box<Hap>,
        side: Side,
        apply: Combine,
        flow: LookupFlow,
        /// The levels of the constant's own walk.
        levels: u32,
    },
}

/// How a node maps each cycle of its child. See [`SignalOp::Cycle`].
#[derive(Clone, Copy, Debug)]
enum CycleMap {
    /// `rev`: reflected in the cycle.
    Reflect,
    /// `slowcat` of this many constituents: cycle `c` plays cycle
    /// `floor(c / count)` of its constituent.
    SlowCat(i128),
    /// `fastGap(factor)`: the cycle plays its child sped up from its start.
    Gap(Fraction),
    /// `repeatCycles(count)`: cycle `c` plays cycle `floor(c / count)`.
    Repeat(Fraction),
}

impl CycleMap {
    /// The span inside for `span` outside, in cycle `c`.
    fn inward(self, span: TimeSpan, c: i128) -> Option<TimeSpan> {
        let cycle = Fraction::int(c);
        Some(match self {
            Self::Reflect => reflected(span, c)?,
            Self::Gap(factor) => {
                let map = |t: Fraction| {
                    t.checked_sub(cycle)?
                        .checked_mul(factor)?
                        .checked_add(cycle)
                };
                TimeSpan::new(map(span.begin)?, map(span.end)?)
            }
            Self::SlowCat(_) | Self::Repeat(_) => {
                let by = Fraction::int(self.offset(c)?);
                TimeSpan::new(span.begin.checked_sub(by)?, span.end.checked_sub(by)?)
            }
        })
    }

    /// `hap` inside, in cycle `c` outside, moved back out.
    fn outward(self, hap: &mut Hap, c: i128) -> Option<()> {
        let cycle = Fraction::int(c);
        let span = |span: TimeSpan| -> Option<TimeSpan> {
            Some(match self {
                Self::Reflect => reflected(span, c)?,
                Self::Gap(factor) => {
                    let map = |t: Fraction| {
                        t.checked_sub(cycle)?
                            .checked_div(factor)?
                            .checked_add(cycle)
                    };
                    TimeSpan::new(map(span.begin)?, map(span.end)?)
                }
                Self::SlowCat(_) | Self::Repeat(_) => {
                    let by = Fraction::int(self.offset(c)?);
                    TimeSpan::new(span.begin.checked_add(by)?, span.end.checked_add(by)?)
                }
            })
        };
        hap.part = span(hap.part)?;
        hap.whole = match hap.whole {
            Some(whole) => Some(span(whole)?),
            None => None,
        };
        Some(())
    }

    /// The whole cycles a shifting map moves cycle `c` by.
    fn offset(self, c: i128) -> Option<i128> {
        match self {
            Self::SlowCat(count) => c.checked_sub(c.div_euclid(count)),
            Self::Repeat(count) => {
                c.checked_sub(Fraction::int(c).checked_div(count)?.floor().numer())
            }
            Self::Reflect | Self::Gap(_) => None,
        }
    }
}

impl Signal {
    /// The constant's hap, when it meets all of `part`: the source is a
    /// constant under speed changes and shifts only, and `part` lies in one
    /// cycle of the constant's own time.
    fn constant_over(&self, part: TimeSpan) -> Option<&Hap> {
        let Source::Constant(constant) = &self.source else {
            return None;
        };
        // Shifts by whole cycles keep a span in one cycle or across two.
        if self.ops.iter().all(|op| match op {
            SignalOp::Shift(by) | SignalOp::Restart { begin: by, .. } => by.denom() == 1,
            _ => false,
        }) {
            return in_one_cycle(part).then_some(&**constant);
        }
        let moved = |span: TimeSpan, by: Fraction| {
            Some(TimeSpan::new(
                span.begin.checked_sub(by)?,
                span.end.checked_sub(by)?,
            ))
        };
        let mut inside = part;
        for op in self.ops.iter().rev() {
            inside = match *op {
                SignalOp::Fast(factor) if factor > Fraction::ZERO => TimeSpan::new(
                    inside.begin.checked_mul(factor)?,
                    inside.end.checked_mul(factor)?,
                ),
                SignalOp::Shift(by) => moved(inside, by)?,
                SignalOp::Restart { begin, period } => {
                    moved(inside, restart_offset(inside.begin, begin, period)?)?
                }
                _ => return None,
            };
        }
        in_one_cycle(inside).then_some(&**constant)
    }

    /// The signal alone, whose hap only its value changes.
    fn bare(&self) -> Option<&SignalFn> {
        match &self.source {
            Source::Signal(sample) if self.ops.is_empty() => Some(sample),
            _ => None,
        }
    }

    /// The haps this signal's graph answers for `span`, in its order, into
    /// `out`, which it clears first. `part` is the part of the hap that asks,
    /// which says where a per-cycle node is.
    fn answer(
        &self,
        span: TimeSpan,
        part: TimeSpan,
        probe: &mut State,
        out: &mut Vec<Hap>,
    ) -> Option<()> {
        let scaled = |span: TimeSpan, factor: Fraction| {
            Some(TimeSpan::new(
                span.begin.checked_mul(factor)?,
                span.end.checked_mul(factor)?,
            ))
        };
        let moved = |span: TimeSpan, by: Fraction| {
            Some(TimeSpan::new(
                span.begin.checked_sub(by)?,
                span.end.checked_sub(by)?,
            ))
        };
        // The span and part each level is asked over, from the outermost
        // in, and what each per-cycle node chose there.
        let mut asked = span;
        let mut cut = part;
        let mut chosen = Chosen::default();
        for op in self.ops.iter().rev() {
            match op {
                SignalOp::Fast(factor) => {
                    asked = scaled(asked, *factor)?;
                    cut = scaled(cut, *factor)?;
                }
                SignalOp::Shift(by) => {
                    asked = moved(asked, *by)?;
                    cut = moved(cut, *by)?;
                }
                SignalOp::Cycle(map) => {
                    let c = cut.begin.floor().numer();
                    asked = map.inward(asked, c)?;
                    cut = map.inward(cut, c)?;
                    chosen.push(Fraction::int(c))?;
                }
                SignalOp::Restart { begin, period } => {
                    let by = restart_offset(cut.begin, *begin, *period)?;
                    asked = moved(asked, by)?;
                    cut = moved(cut, by)?;
                    chosen.push(by)?;
                }
                SignalOp::Step(_) | SignalOp::Constant { .. } => {}
            }
        }
        out.clear();
        match &self.source {
            Source::Signal(sample) => {
                probe.span = asked;
                let value = sample(probe);
                out.push(Hap::new(None, asked, value));
            }
            Source::Fixed(fixed) => {
                let by = asked.begin.checked_sub(fixed.whole.begin)?;
                let mut scratch = HapBuffer::take();
                for (hap, sampler) in fixed.haps.iter().zip(&fixed.samplers) {
                    let hap = retimed(hap, |t| t.checked_add(by))?;
                    match sampler {
                        Some(sampler) => {
                            sampler.run(&hap, hap.whole?, hap.part, probe, &mut scratch, out)?
                        }
                        None => out.push(hap),
                    }
                }
            }
            Source::Constant(constant) => {
                for cycle in
                    asked.begin.floor().numer()..=asked.end.ceil().numer().checked_sub(1)?
                {
                    let mut hap = shifted(constant, cycle)?;
                    let Some(part) = hap.part.intersection(&asked) else {
                        continue;
                    };
                    hap.part = part;
                    out.push(hap);
                }
            }
        }
        for op in &self.ops {
            match op {
                SignalOp::Step(step) => out.retain_mut(|hap| step.run_in_place(hap)),
                SignalOp::Fast(factor) => {
                    for hap in out.iter_mut() {
                        retime(hap, |t| t.checked_div(*factor))?;
                    }
                }
                SignalOp::Shift(by) => {
                    for hap in out.iter_mut() {
                        retime(hap, |t| t.checked_add(*by))?;
                    }
                }
                SignalOp::Cycle(map) => {
                    let c = chosen.pop()?.numer();
                    for hap in out.iter_mut() {
                        map.outward(hap, c)?;
                    }
                }
                SignalOp::Restart { .. } => {
                    let by = chosen.pop()?;
                    for hap in out.iter_mut() {
                        retime(hap, |t| t.checked_add(by))?;
                    }
                }
                SignalOp::Constant {
                    hap: constant,
                    side,
                    apply,
                    flow,
                    ..
                } => {
                    // A hap in one cycle meets all of the constant's hap
                    // there, which is the same in every cycle.
                    if out.iter().all(|hap| in_one_cycle(hap.part)) {
                        for hap in out.iter_mut() {
                            let part = hap.part;
                            if !(matches!(side, Side::Value)
                                && Meeting::in_place(hap, constant, part, apply, *flow))
                            {
                                *hap = match side {
                                    Side::Value => {
                                        applied_hap(hap, constant, hap.whole, part, apply, *flow)
                                    }
                                    Side::Function => {
                                        applied_hap(constant, hap, hap.whole, part, apply, *flow)
                                    }
                                };
                            }
                        }
                        continue;
                    }
                    let mut met = Vec::with_capacity(out.len());
                    for hap in out.iter() {
                        // The constant asked over this hap's part: one hap
                        // for each cycle the part meets.
                        let first = hap.part.begin.floor().numer();
                        let last = hap.part.end.ceil().numer().checked_sub(1)?;
                        for cycle in first..=last {
                            let mut other = shifted(constant, cycle)?;
                            let Some(part) = other.part.intersection(&hap.part) else {
                                continue;
                            };
                            other.part = part;
                            met.push(match side {
                                Side::Value => {
                                    applied_hap(hap, &other, hap.whole, part, apply, *flow)
                                }
                                Side::Function => {
                                    applied_hap(&other, hap, hap.whole, part, apply, *flow)
                                }
                            });
                        }
                    }
                    *out = met;
                }
            }
        }
        Some(())
    }
}

impl Sampler {
    /// The haps the structure `entry` makes at `whole` and `part`, pushed in
    /// the graph's order. Each meeting reads the signal's haps over the
    /// whole, which every hap cut from the entry keeps. `None` when a met
    /// value is not data, which only the graph answers.
    ///
    /// `signal` is scratch for the signal's haps.
    fn run(
        &self,
        entry: &Hap,
        whole: TimeSpan,
        part: TimeSpan,
        probe: &mut State,
        signal: &mut Vec<Hap>,
        out: &mut Vec<Hap>,
    ) -> Option<()> {
        let start = out.len();
        let mut hap = entry.clone();
        hap.whole = Some(whole);
        hap.part = part;
        out.push(hap);
        if self.stages.is_empty() {
            // An entry's own value is data.
            return Some(());
        }
        for stage in &self.stages {
            stage.run(whole, part, probe, signal, out, start)?;
        }
        out[start..]
            .iter()
            .all(|hap| hap.value.is_data(0))
            .then_some(())
    }

    /// `stage` run after the others. A meeting that sets a constant's keys
    /// joins one that does so just before it. See [`Stage::Fused`].
    fn push(&mut self, stage: Stage) {
        if let Some(last) = self.stages.last_mut()
            && let Some(fused) = last.fused_with(&stage)
        {
            *last = fused;
            return;
        }
        self.stages.push(stage);
    }

    /// The bare signal of the first meeting, its side and combination, when
    /// every stage after it is a value step: such a layer meets its signal
    /// where each hap is. See [`Layer::emit_sampled_in_place`].
    fn in_place(&self) -> Option<(&SignalFn, Side, &Combine, LookupFlow, &[Stage])> {
        let (
            Stage::Meet {
                signal,
                side,
                apply,
                flow,
            },
            rest,
        ) = self.stages.split_first()?
        else {
            return None;
        };
        let sample = signal.bare()?;
        rest.iter()
            .all(|stage| matches!(stage, Stage::Step(_)))
            .then_some((sample, *side, apply, *flow, rest))
    }
}

impl Stage {
    /// Run this stage on the haps that one entry makes, `out[start..]`, at
    /// the entry's `whole` and `part`. See [`Sampler::run`].
    fn run(
        &self,
        whole: TimeSpan,
        part: TimeSpan,
        probe: &mut State,
        signal: &mut Vec<Hap>,
        out: &mut Vec<Hap>,
        start: usize,
    ) -> Option<()> {
        match self {
            Stage::Meet {
                signal: source,
                side,
                apply,
                flow,
            } => {
                let meet = |structure: &Hap, cut: TimeSpan, met: &Hap| match side {
                    Side::Function => applied_hap(met, structure, Some(whole), cut, apply, *flow),
                    Side::Value => applied_hap(structure, met, Some(whole), cut, apply, *flow),
                };
                // A constant meets all of a part that lies in one of its
                // cycles, with the same hap in every cycle.
                let answered = !matches!(source.source, Source::Constant(_));
                if answered {
                    source.answer(whole, part, probe, signal)?;
                }
                let only = |structure: &Hap| {
                    if answered {
                        only_meeting(structure.part, signal)
                    } else {
                        let constant = source.constant_over(structure.part)?;
                        Some((structure.part, constant))
                    }
                };
                if out[start..]
                    .iter()
                    .all(|structure| only(structure).is_some())
                {
                    // Each hap meets one hap, so it becomes the met hap
                    // where it is.
                    for hap in &mut out[start..] {
                        let (cut, met) = only(hap)?;
                        if !(matches!(side, Side::Value)
                            && Meeting::in_place(hap, met, cut, apply, *flow))
                        {
                            *hap = meet(hap, cut, met);
                        }
                    }
                    return Some(());
                }
                if !answered {
                    source.answer(whole, part, probe, signal)?;
                }
                let mut next = HapBuffer::take();
                next.reserve(signal.len());
                for structure in &out[start..] {
                    for met in signal.iter() {
                        if let Some(cut) = structure.part.intersection(&met.part) {
                            next.push(meet(structure, cut, met));
                        }
                    }
                }
                out.truncate(start);
                out.append(&mut next);
            }
            Stage::Fused {
                signal: source,
                each,
            } => {
                let apply = Combine::Composed(ComposeOp::Set);
                let Source::Constant(constant) = &source.source else {
                    return None;
                };
                if out[start..].iter().all(|hap| {
                    source.constant_over(hap.part).is_some()
                        && Meeting::meets_in_place(hap, constant, &apply, LookupFlow::Set)
                }) {
                    for hap in &mut out[start..] {
                        let part = hap.part;
                        if !Meeting::in_place(hap, constant, part, &apply, LookupFlow::Set) {
                            return None;
                        }
                    }
                    return Some(());
                }
                for stage in each {
                    stage.run(whole, part, probe, signal, out, start)?;
                }
            }
            Stage::Step(step) => {
                let mut kept = start;
                for index in start..out.len() {
                    if step.run_in_place(&mut out[index]) {
                        if kept != index {
                            out.swap(kept, index);
                        }
                        kept += 1;
                    }
                }
                out.truncate(kept);
            }
            Stage::Outer(outer) => {
                for hap in &mut out[start..] {
                    let inner =
                        std::mem::replace(hap, Hap::new(None, part, crate::Value::Undefined));
                    *hap = outer.merge(inner);
                }
            }
        }
        Some(())
    }

    /// The constant hap this stage meets, when it is a meeting that sets
    /// that hap's keys on the structure's value as they are, in the
    /// constant's own time: one that may join others. See [`Stage::Fused`].
    fn setting(&self) -> Option<&Hap> {
        let Stage::Meet {
            signal,
            side: Side::Value,
            apply: Combine::Composed(ComposeOp::Set),
            flow: LookupFlow::Set,
        } = self
        else {
            return None;
        };
        let Source::Constant(constant) = &signal.source else {
            return None;
        };
        let keys = constant.value.as_object()?;
        (signal.ops.is_empty()
            && constant.pick_lookup.is_none()
            && !crate::compose::union_drops(keys)
            && constant.value.is_data(0))
        .then_some(constant)
    }

    /// This stage and `next` as one, when both set constants' keys.
    ///
    /// Meeting the constant they make together sets what meeting both in
    /// turn sets: the union keeps the first's keys in order and then adds
    /// the second's, the second's locations come first, and the second's
    /// live controls win for its own keys.
    fn fused_with(&self, next: &Stage) -> Option<Stage> {
        let constant = next.setting()?;
        let (before, mut each) = match self {
            Stage::Fused { signal, each } if signal.ops.is_empty() => match &signal.source {
                Source::Constant(before) => (&**before, each.clone()),
                _ => return None,
            },
            stage => (stage.setting()?, vec![stage.clone()]),
        };
        let cycle = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
        let together = applied_hap(
            before,
            constant,
            Some(cycle),
            cycle,
            &Combine::Composed(ComposeOp::Set),
            LookupFlow::Set,
        );
        if together.pick_lookup.is_some() {
            return None;
        }
        // Each hap reads the constant's map.
        if !matches!(together.value, crate::Value::Object(_)) {
            return None;
        }
        each.push(next.clone());
        Some(Stage::Fused {
            signal: Signal {
                source: Source::Constant(Box::new(together)),
                ops: Vec::new(),
            },
            each,
        })
    }

    /// `ops` added outside each signal this stage meets.
    fn extend_ops(&mut self, ops: &[SignalOp]) {
        match self {
            Stage::Meet { signal, .. } => signal.ops.extend_from_slice(ops),
            Stage::Fused { signal, each } => {
                signal.ops.extend_from_slice(ops);
                for stage in each {
                    stage.extend_ops(ops);
                }
            }
            Stage::Step(_) | Stage::Outer(_) => {}
        }
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

/// The answer tables one node holds, in the state a query finds them.
#[derive(Debug, Default)]
pub(crate) enum TableSlot {
    /// No build has put tables here yet.
    #[default]
    Empty,
    /// A build under this settings state made none, so the graph answers.
    /// A later state may build again: the compiled programs a build applies
    /// can change with the state.
    GraphAnswers(SettingsStateId),
    /// Built tables, answering while [`Tables::current`] upholds them and
    /// replaced by a fresh build the first query after they stop.
    Ready(Arc<Tables>),
}

/// Layers answered one after the other, as a stack answers its children.
#[derive(Debug)]
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
    /// Which layers meet each cycle, for tables of many layers. Built when
    /// first asked for. See [`LayerIndex`].
    index: OnceLock<Option<LayerIndex>>,
    /// Every layer answers in sorted order. See [`Layer::answers_in`].
    sorted: bool,
    /// The settings snapshot the cached table was built under, the only one
    /// it answers for. See [`crate::settings::SettingsStateId`].
    state: Option<SettingsStateId>,
    /// The standings of the compiled programs the build applied. The table
    /// answers only while they all hold. See
    /// [`crate::callback_ir::recording_reads`].
    read: Box<[Arc<crate::callback_ir::Standing>]>,
    /// The share of [`TABLE_BUDGET`] these tables hold while a node keeps
    /// them. `None` for tables built for one answer only.
    charge: Option<Charge>,
}

/// The bytes the kept tables of this process may hold together, unless a
/// host sets another budget with [`set_table_budget`].
pub const DEFAULT_TABLE_BUDGET: usize = 32 << 20;

static TABLE_BUDGET: AtomicUsize = AtomicUsize::new(DEFAULT_TABLE_BUDGET);
static TABLE_BYTES: AtomicUsize = AtomicUsize::new(0);

/// Set how many bytes the kept tables of this process may hold together.
/// Tables kept already stay until they drop. A table past the budget
/// answers the query that built it and is not kept: its node is walked.
pub fn set_table_budget(bytes: usize) {
    TABLE_BUDGET.store(bytes, Ordering::Relaxed);
}

/// The bytes the kept tables of this process hold now.
pub fn table_bytes() -> usize {
    TABLE_BYTES.load(Ordering::Relaxed)
}

/// A kept table's share of the budget, given back when the table drops.
#[derive(Debug)]
struct Charge(usize);

impl Charge {
    /// `bytes` of the budget, when that much is left.
    fn take(bytes: usize) -> Option<Self> {
        let budget = TABLE_BUDGET.load(Ordering::Relaxed);
        TABLE_BYTES
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |held| {
                held.checked_add(bytes).filter(|&total| total <= budget)
            })
            .ok()
            .map(|_| Self(bytes))
    }
}

impl Drop for Charge {
    fn drop(&mut self) {
        TABLE_BYTES.fetch_sub(self.0, Ordering::Relaxed);
    }
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
            // A meeting with a signal can cut one entry into several haps.
            let entries = match &layer.sampled {
                Some(sampled) => sampled.total(),
                None => u64::try_from(layer.entries.len()).unwrap_or(u64::MAX),
            };
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

    /// A child asked at `t - s`, and its haps moved by `s` after, where `s`
    /// is a time of an entry that reaches `extent` cycles past the query
    /// span, or the position of one in its cycle, with at most `bits` bits in
    /// its denominator. `|s|` is at most `|t| + extent + 1`.
    fn restarted(self, extent: f64, bits: u32) -> Self {
        Self {
            scale: self.scale * 2.0 + 1.0,
            shift: self.shift + (self.scale + 1.0) * (extent + 1.0),
            denominator_bits: self.denominator_bits.saturating_add(bits),
        }
    }

    /// A child asked at times of entries with at most `bits` bits in their
    /// denominators: an answer may hold none of those times.
    fn finer(self, bits: u32) -> Self {
        Self {
            denominator_bits: self.denominator_bits.saturating_add(bits),
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

/// Whether clipping parts to a query span can never make two of `entries`
/// tie in the sorted order when they did not tie before. Two haps tie when
/// their parts and wholes are equal, and clipping changes only parts. Two
/// entries with one whole and overlapping parts that differ can be clipped
/// to equal parts, so the entries that share a whole must have equal or
/// disjoint parts.
fn ties_survive_clipping(entries: &[Hap]) -> bool {
    if entries.iter().all(|entry| entry.whole == Some(entry.part)) {
        return true;
    }
    let mut spans = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some(whole) = entry.whole else {
            return false;
        };
        spans.push((whole.begin, whole.end, entry.part.begin, entry.part.end));
    }
    spans.sort_unstable();
    spans.windows(2).all(|pair| {
        let (before, after) = (pair[0], pair[1]);
        // Sorted by whole and then part, the parts of one whole are equal or
        // disjoint when each one equals the one before it, or begins at its
        // end or later.
        (before.0, before.1) != (after.0, after.1)
            || (before.2, before.3) == (after.2, after.3)
            || before.3 <= after.2
    })
}

/// `floor((a - b) / period)` for a positive `period`, exact, and with no
/// fraction reduced on the way: a query asks this of every layer.
fn periods_below(a: Fraction, b: Fraction, period: i128) -> Option<i128> {
    // `a - b` over the product of the denominators, which are positive.
    let numerator = a
        .numer()
        .checked_mul(b.denom())?
        .checked_sub(b.numer().checked_mul(a.denom())?)?;
    let denominator = a.denom().checked_mul(b.denom())?;
    // `floor(floor(x) / n)` is `floor(x / n)` for a whole `n > 0`.
    floor_div(floor_div(numerator, denominator)?, period)
}

/// How far `pickRestart` moves the copy of its pattern that time `at` is
/// in: to the start of the selector hap that starts at `begin` in period
/// zero, in the period of `period` cycles that `at` is in.
fn restart_offset(at: Fraction, begin: Fraction, period: i128) -> Option<Fraction> {
    let copy = periods_below(at, begin, period)?;
    begin.checked_add(Fraction::int(copy.checked_mul(period)?))
}

/// `floor(n / d)` for a positive `d`, in 64 bits when both fit.
fn floor_div(n: i128, d: i128) -> Option<i128> {
    match (i64::try_from(n), i64::try_from(d)) {
        (Ok(n), Ok(d)) => Some(i128::from(n.div_euclid(d))),
        _ => n.checked_div_euclid(d),
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
        // A period that repeats a control map holds it once.
        crate::value::share_equal_maps(entries.iter_mut().map(|entry| &mut entry.value));
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
            sampled: None,
            whole_haps,
            cycle_starts,
        })
    }

    fn with_sampled(mut self, sampled: Option<Sampled>) -> Self {
        // A sorted form holds the entries in another order than the samplers
        // of each entry are kept in.
        if sampled
            .as_ref()
            .is_some_and(|sampled| !sampled.is_uniform())
        {
            self.sorted = None;
        }
        self.sampled = sampled.map(Arc::new);
        self
    }

    /// This layer with `stage` run after what each entry runs already.
    /// `most` is the most haps the stage makes of one hap. An entry whose
    /// value is its own runs a `fixed` stage here, once: what such a stage
    /// reads repeats every cycle, so what it makes repeats with the entry.
    /// Every other entry runs `stage` in each query, last of its sampler.
    fn staged(&self, stage: &Stage, most: u64, fixed: bool) -> Option<Layer> {
        let sampled = self.sampled.as_deref();
        // Each sampler changes once, however many entries run it. The one
        // with no stage is that of the entries whose value is their own.
        let changed: Vec<Option<Arc<Sampler>>> = sampled.map_or_else(Vec::new, |sampled| {
            sampled
                .samplers
                .iter()
                .map(|sampler| {
                    (!sampler.stages.is_empty()).then(|| {
                        let mut sampler = Sampler::clone(sampler);
                        sampler.push(stage.clone());
                        sampler.most = sampler.most.saturating_mul(most);
                        Arc::new(sampler)
                    })
                })
                .collect()
        });
        let alone = Sampler {
            stages: vec![stage.clone()],
            most,
        };
        let own = (!fixed).then(|| Arc::new(alone.clone()));
        let mut out = Structure::default();
        for (index, entry) in self.entries.iter().enumerate() {
            let sampler = match sampled {
                Some(sampled) => changed
                    .get(usize::try_from(*sampled.of.get(index)?).ok()?)?
                    .clone(),
                None => None,
            };
            match sampler.or_else(|| own.clone()) {
                Some(sampler) => {
                    out.haps.push(entry.clone());
                    out.samplers.push(Some(sampler));
                }
                None => run_fixed(&alone, entry, &mut out)?,
            }
            if out.haps.len() > MAX_ENTRIES {
                return None;
            }
        }
        out.into_layer(self.period)
    }

    /// The entries to answer from: in graph order, or sorted. `None` when
    /// this layer has no sorted form that clipping keeps sorted.
    fn entries_in(&self, order: Order) -> Option<Answering<'_>> {
        // A sampler that cuts one entry into several haps gives them in the
        // order of its meetings, which need not be the sorted one.
        if order == Order::Sorted
            && self
                .sampled
                .as_ref()
                .is_some_and(|sampled| sampled.most() > 1)
        {
            return None;
        }
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
        if self.sampled.is_some() {
            return None;
        }
        self.copies(cycles)
    }

    /// The entries of `cycles` consecutive periods from cycle zero, also of
    /// a sampled layer, whose entries are its structure.
    fn copies(&self, cycles: i128) -> Option<Vec<Hap>> {
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
    /// the graph must answer, and `out` is then unchanged. `Some(true)` when
    /// the span may have clipped a part, which can change the sorted order.
    fn query_into(
        &self,
        state: &State,
        order: Order,
        out: &mut Vec<Hap>,
        stop: &mut Stop,
    ) -> Option<bool> {
        let Some(reach) = self.reach else {
            return Some(false);
        };
        let entries = self.entries_in(order)?;
        let start = out.len();
        let answered = match &self.sampled {
            Some(sampled) => match sampled.uniform().filter(|_| self.copyable) {
                Some(sampler) if sampler.in_place().is_some() => {
                    self.emit_sampled_in_place(sampler, entries, state, reach, out, stop)
                }
                _ => self.emit_sampled(sampled, entries, state, reach, out, stop),
            },
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
    ) -> Option<bool> {
        let (sample, side, apply, flow, then) = sampler.in_place()?;
        let from = out.len();
        let clipped = self.emit(entries, state.span, reach, out, stop)?;
        let mut probe = state.clone();
        let mut signal = Hap::new(None, state.span, crate::Value::Undefined);
        let mut kept = from;
        for index in from..out.len() {
            let hap = &mut out[index];
            let whole = hap.whole?;
            probe.span = whole;
            signal.part = whole;
            signal.value = sample(&probe);
            let met = match side {
                Side::Function => Meeting::of(&signal, hap, apply, flow),
                Side::Value => Meeting::of(hap, &signal, apply, flow),
            };
            met.settle(hap);
            if then.iter().all(|stage| match stage {
                Stage::Step(step) => step.run_in_place(hap),
                Stage::Meet { .. } | Stage::Outer(_) | Stage::Fused { .. } => false,
            }) {
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
        Some(clipped)
    }

    /// Append the haps for `span` to `out` in the graph's order, each with
    /// its sampler, not run.
    fn structure_into(&self, span: TimeSpan, out: &mut Structure, stop: &mut Stop) -> Option<()> {
        let Some(reach) = self.reach else {
            return Some(());
        };
        let entries = self.entries_in(Order::Graph)?;
        let Some(sampled) = &self.sampled else {
            self.emit(entries, span, reach, &mut out.haps, stop)?;
            out.samplers.resize(out.haps.len(), None);
            return Some(());
        };
        let periods = self.periods(span, reach)?;
        if periods.first > periods.last {
            return Some(());
        }
        for k in periods.first..=periods.last {
            let shift = k.checked_mul(self.period)?;
            let from = out.haps.len();
            let (near, first) = if periods.is_inside(k) {
                (entries.haps, 0)
            } else {
                let near = entries.near(span, shift)?;
                (near.haps, near.start)
            };
            for (index, entry) in (first..).zip(near) {
                let Some(part) = shifted_span(entry.part, shift)?.intersection(&span) else {
                    continue;
                };
                let mut hap = entry.clone();
                hap.whole = match entry.whole {
                    Some(whole) => Some(shifted_span(whole, shift)?),
                    None => None,
                };
                hap.part = part;
                out.haps.push(hap);
                let sampler = usize::try_from(*sampled.of.get(index)?).ok()?;
                out.samplers
                    .push(Some(Arc::clone(sampled.samplers.get(sampler)?)));
            }
            if !stop.allows(from, out.haps.len() - from) {
                return Some(());
            }
        }
        Some(())
    }

    /// [`Self::emit`] for a sampled layer: each hap meets the signal.
    fn emit_sampled(
        &self,
        sampled: &Sampled,
        entries: Answering<'_>,
        state: &State,
        reach: TimeSpan,
        out: &mut Vec<Hap>,
        stop: &mut Stop,
    ) -> Option<bool> {
        let span = state.span;
        let periods = self.periods(span, reach)?;
        if periods.first > periods.last {
            return Some(false);
        }
        out.reserve(self.most(entries, span, periods)?.min(MAX_RESERVED));
        let mut probe = state.clone();
        let mut signal = HapBuffer::take();
        for k in periods.first..=periods.last {
            let shift = k.checked_mul(self.period)?;
            let inside = periods.is_inside(k);
            let from = out.len();
            let mut met = 0;
            let (near, first) = if inside {
                (entries.haps, 0)
            } else {
                let near = entries.near(span, shift)?;
                (near.haps, near.start)
            };
            for (index, entry) in (first..).zip(near) {
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
                sampled
                    .of(index)?
                    .run(entry, whole, part, &mut probe, &mut signal, out)?;
            }
            // Every meeting counts, also one that a step dropped after.
            if !stop.allows(from, met) {
                return Some(true);
            }
        }
        Some(true)
    }

    /// The periods `span` meets, for parts that stay within `reach`.
    fn periods(&self, span: TimeSpan, reach: TimeSpan) -> Option<Periods> {
        let period = self.period;
        // `ceil(x)` is `-floor(-x)`.
        let first = periods_below(span.begin, reach.end, period)?.checked_add(1)?;
        let last = periods_below(reach.begin, span.end, period)?
            .checked_neg()?
            .checked_sub(1)?;
        // Period `k` lies inside when `reach` moved by `k` periods does.
        let inside = (
            periods_below(reach.begin, span.begin, period)?.checked_neg()?,
            periods_below(span.end, reach.end, period)?,
        );
        Some(Periods {
            first,
            last,
            inside,
        })
    }

    /// At most how many haps `span` takes from `entries`: all of them in a
    /// period that lies inside, and the near ones of a period it cuts.
    fn most(&self, entries: Answering<'_>, span: TimeSpan, periods: Periods) -> Option<usize> {
        let mut total = periods.inside_count()?.saturating_mul(entries.haps.len());
        for k in periods.cut() {
            let shift = k.checked_mul(self.period)?;
            total = total.saturating_add(entries.most(span, shift)?);
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
    ) -> Option<bool> {
        let all = entries.haps;
        let periods = self.periods(span, reach)?;
        if periods.first > periods.last {
            return Some(false);
        }
        // The size first, so the room is reserved once.
        out.reserve(self.most(entries, span, periods)?.min(MAX_RESERVED));
        let mut clipped = false;
        for k in periods.first..=periods.last {
            let shift = k.checked_mul(self.period)?;
            if periods.is_inside(k) {
                if !stop.allows(out.len(), all.len()) {
                    return Some(clipped);
                }
                self.copy_shifted(all, shift, out)?;
                continue;
            }
            let from = out.len();
            let near = entries.near(span, shift)?;
            for entry in &near.haps[..near.inside.start] {
                clipped |= Self::push_clipped(entry, span, shift, out)?;
            }
            self.copy_shifted(&near.haps[near.inside.clone()], shift, out)?;
            for entry in &near.haps[near.inside.end..] {
                clipped |= Self::push_clipped(entry, span, shift, out)?;
            }
            if !stop.allows(from, out.len() - from) {
                return Some(clipped);
            }
        }
        Some(clipped)
    }

    /// Append `entries` moved `shift` cycles on: cloned in one go, then
    /// shifted where they are.
    fn copy_shifted(&self, entries: &[Hap], shift: i128, out: &mut Vec<Hap>) -> Option<()> {
        let from = out.len();
        if self.copyable {
            out.reserve(entries.len());
            // SAFETY: every entry owns no heap memory (`copyable`), so a byte
            // copy of it is a clone. `reserve` made room for all of them, and
            // they are initialised before the length covers them.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    entries.as_ptr(),
                    out.as_mut_ptr().add(from),
                    entries.len(),
                );
                out.set_len(from + entries.len());
            }
        } else {
            out.extend_from_slice(entries);
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
        Some(())
    }

    /// Append `entry` moved `shift` cycles on with its part clipped to
    /// `span`, if they meet. `Some(true)` when the clip changed the part.
    fn push_clipped(entry: &Hap, span: TimeSpan, shift: i128, out: &mut Vec<Hap>) -> Option<bool> {
        let shifted = shifted_span(entry.part, shift)?;
        let Some(part) = shifted.intersection(&span) else {
            return Some(false);
        };
        let mut hap = entry.clone();
        hap.whole = match entry.whole {
            Some(whole) => Some(shifted_span(whole, shift)?),
            None => None,
        };
        hap.part = part;
        out.push(hap);
        Some(part != shifted)
    }
}

/// The haps a child answers for a rule that takes them in: in the graph's
/// order, each with the sampler that makes its value in a query, not run.
#[derive(Default)]
struct Structure {
    haps: Vec<Hap>,
    samplers: Vec<Option<Arc<Sampler>>>,
}

impl Structure {
    /// The haps moved by `map`, and every sampler under `ops`, the time map
    /// of its haps, innermost first, so that each signal is still asked where
    /// it was.
    fn retimed(self, map: impl Fn(&Hap) -> Option<Hap>, ops: &[SignalOp]) -> Option<Self> {
        let haps = self.haps.iter().map(map).collect::<Option<_>>()?;
        // Each sampler is moved once, however many haps run it.
        let mut moved: Vec<(*const Sampler, Arc<Sampler>)> = Vec::new();
        let samplers = self
            .samplers
            .into_iter()
            .map(|sampler| {
                let sampler = sampler?;
                let key = Arc::as_ptr(&sampler);
                if let Some((_, done)) = moved.iter().find(|(seen, _)| *seen == key) {
                    return Some(Arc::clone(done));
                }
                let mut changed = Sampler::clone(&sampler);
                for stage in &mut changed.stages {
                    stage.extend_ops(ops);
                }
                let changed = Arc::new(changed);
                moved.push((key, Arc::clone(&changed)));
                Some(changed)
            })
            .collect();
        Some(Self { haps, samplers })
    }

    fn append(&mut self, mut other: Self) {
        self.haps.append(&mut other.haps);
        self.samplers.append(&mut other.samplers);
    }

    /// One layer of these haps, repeating every `period` cycles.
    fn into_layer(self, period: i128) -> Option<Layer> {
        let sampled = if self.samplers.iter().any(Option::is_some) {
            Some(sampled_of(&self.samplers)?)
        } else {
            None
        };
        Some(Layer::new(period, self.haps)?.with_sampled(sampled))
    }
}

/// The samplers of entries that each run `samplers[i]`. An entry whose
/// value is its own runs a sampler with no stage.
fn sampled_of(samplers: &[Option<Arc<Sampler>>]) -> Option<Sampled> {
    let mut distinct: Vec<Arc<Sampler>> = Vec::new();
    let mut own: Option<u32> = None;
    let mut of = Vec::with_capacity(samplers.len());
    for sampler in samplers {
        let index = match sampler {
            Some(sampler) => match distinct.iter().position(|seen| Arc::ptr_eq(seen, sampler)) {
                Some(index) => u32::try_from(index).ok()?,
                None => {
                    distinct.push(Arc::clone(sampler));
                    u32::try_from(distinct.len() - 1).ok()?
                }
            },
            None => match own {
                Some(index) => index,
                None => {
                    distinct.push(Arc::new(Sampler {
                        stages: Vec::new(),
                        most: 1,
                    }));
                    let index = u32::try_from(distinct.len() - 1).ok()?;
                    own = Some(index);
                    index
                }
            },
        };
        of.push(index);
    }
    Some(Sampled {
        samplers: distinct,
        of,
    })
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

/// Tables with this many layers or more find the layers a query meets
/// through a [`LayerIndex`].
const INDEXED_LAYERS: usize = 8;

/// The layers whose parts meet each cycle of the layers' shared period. A
/// weighted sequence of `n` steps is a stack of `n` layers, and a query
/// looks only at those its cycles meet.
#[derive(Clone, Debug)]
struct LayerIndex {
    period: i128,
    /// The layers that meet cycle `c` of the period are
    /// `layers[starts[c]..starts[c + 1]]`, in layer order.
    starts: Vec<u32>,
    layers: Vec<u32>,
}

impl LayerIndex {
    /// `None` for a shared period of more than [`MAX_ENTRIES`] cycles, or an
    /// index of more than [`MAX_BUILD_SCAN`] items.
    fn new(layers: &[Layer]) -> Option<Self> {
        let period = layers
            .iter()
            .try_fold(1i128, |period, layer| lcm(period, layer.period))?;
        let cycles = usize::try_from(period).ok()?;
        if cycles > MAX_ENTRIES || cycles.checked_mul(layers.len())? > MAX_BUILD_SCAN {
            return None;
        }
        // The cycles of its own period that each layer's parts meet.
        let mut meets = Vec::with_capacity(layers.len());
        for layer in layers {
            let own = usize::try_from(layer.period).ok()?;
            let mut cycles = vec![false; own];
            for entry in &layer.entries {
                let first = entry.part.begin.floor().numer();
                let last = entry.part.end.ceil().numer().checked_sub(1)?;
                if last.checked_sub(first)? >= layer.period.checked_sub(1)? {
                    cycles.fill(true);
                    break;
                }
                for cycle in first..=last {
                    cycles[usize::try_from(cycle.rem_euclid(layer.period)).ok()?] = true;
                }
            }
            meets.push(cycles);
        }
        let mut starts = Vec::with_capacity(cycles + 1);
        let mut ids = Vec::new();
        for cycle in 0..cycles {
            starts.push(u32::try_from(ids.len()).ok()?);
            for (id, own) in meets.iter().enumerate() {
                if own[cycle % own.len()] {
                    ids.push(u32::try_from(id).ok()?);
                }
            }
        }
        starts.push(u32::try_from(ids.len()).ok()?);
        Some(Self {
            period,
            starts,
            layers: ids,
        })
    }

    /// The layers that may meet `span`, in layer order, or `None` when the
    /// span meets every cycle of the period. A part meets `span` only inside
    /// one of the cycles from `floor(begin)` to `ceil(end) - 1`.
    fn meeting(&self, span: TimeSpan) -> Option<Vec<u32>> {
        let first = span.begin.floor().numer();
        let last = span.end.ceil().numer().checked_sub(1)?;
        if last.checked_sub(first)? >= self.period.checked_sub(1)? {
            return None;
        }
        let mut ids = Vec::new();
        for cycle in first..=last {
            let cycle = usize::try_from(cycle.rem_euclid(self.period)).ok()?;
            ids.extend_from_slice(
                &self.layers[self.starts[cycle] as usize..self.starts[cycle + 1] as usize],
            );
        }
        ids.sort_unstable();
        ids.dedup();
        Some(ids)
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
        let sorted = layers.iter().all(|layer| layer.answers_in(Order::Sorted));
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
            index: OnceLock::new(),
            sorted,
            state: None,
            read: Box::new([]),
            charge: None,
        }
    }

    /// These tables, kept for the settings snapshot `state`, which was in
    /// force when their build began, and for the standings `read` of the
    /// compiled programs the build applied.
    /// These tables for one answer, with the standings of the programs
    /// their build applied.
    pub(crate) fn reading(self, read: Box<[Arc<crate::callback_ir::Standing>]>) -> Self {
        Self { read, ..self }
    }

    pub(crate) fn built_under(
        self,
        state: SettingsStateId,
        read: Box<[Arc<crate::callback_ir::Standing>]>,
    ) -> Self {
        Self {
            state: Some(state),
            read,
            ..self
        }
    }

    /// Take this table's share of the budget so a node may keep it. `false`
    /// when the budget has no room: the table answers once and drops.
    pub(crate) fn charge(&mut self) -> bool {
        self.charge = Charge::take(self.bytes());
        self.charge.is_some()
    }

    /// About the bytes these tables hold: their entries in each order kept,
    /// and the union a sorted answer of several layers builds.
    fn bytes(&self) -> usize {
        let kept: usize = self
            .layers
            .iter()
            .map(|layer| layer.entries.len() + layer.sorted.as_ref().map_or(0, Vec::len))
            .sum();
        let union = if self.layers.len() > 1 {
            self.layers.iter().map(|layer| layer.entries.len()).sum()
        } else {
            0
        };
        (kept + union) * std::mem::size_of::<Hap>()
    }

    /// Whether the settings in force are those the table was built under,
    /// and every compiled program the build applied still stands for its
    /// callback.
    pub(crate) fn current(&self) -> bool {
        crate::callback_ir::reads_hold(&self.read)
            && self
                .state
                .as_ref()
                .is_some_and(crate::settings::is_current_state)
    }

    /// The standings of the compiled programs the build applied.
    pub(crate) fn read(&self) -> &[Arc<crate::callback_ir::Standing>] {
        &self.read
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
            let mut clipped = false;
            if stop.begin() {
                clipped = union.query_into(state, Order::Graph, &mut haps, stop)?;
            }
            // The union's haps come sorted. Clipping at the span's ends can
            // reorder a few, and the sort keeps the order when none moved.
            return Some(if clipped {
                crate::sort_haps_for_query(haps)
            } else {
                haps
            });
        }
        // A layer without a sorted form answers in the graph's order, and the
        // stable sort below makes that the graph's answer sorted.
        let order = if self.sorted {
            Order::Sorted
        } else {
            Order::Graph
        };
        let haps = self.query_in(state, order, stop)?;
        // Each layer is sorted. Equal haps keep the layer order a stable
        // sort of the graph's answer keeps.
        Some(if self.layers.len() > 1 || order == Order::Graph {
            crate::sort_haps_for_query(haps)
        } else {
            haps
        })
    }

    /// Every layer's haps over one shared period, stably sorted: equal haps
    /// keep their layer's order, as a stable sort of the graph's answer
    /// keeps them. Only for haps inside that period that clipping cannot
    /// make tie with a hap they did not tie with before. See
    /// [`ties_survive_clipping`].
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
                if !entries.iter().all(|entry| inside(entry.part))
                    || !ties_survive_clipping(&entries)
                {
                    return None;
                }
                Layer::new(period, crate::sort_haps_for_query(entries))
            })
            .as_ref()
    }

    /// Whether a span may be answered here at all: wider than zero, and with
    /// nothing in the graph's walk for it that the graph could refuse.
    pub(crate) fn answerable(&self, span: TimeSpan, stop: &Stop) -> Option<bool> {
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
        if !self.answerable(state.span, stop)? || !(order == Order::Graph || self.sorted) {
            return None;
        }
        let mut out = Vec::new();
        if !stop.begin() {
            return Some(out);
        }
        let meeting = self
            .layer_index()
            .and_then(|index| index.meeting(state.span));
        let layers: &mut dyn Iterator<Item = &Layer> = match &meeting {
            Some(ids) => &mut ids.iter().map(|&id| &self.layers[id as usize]),
            None => &mut self.layers.iter(),
        };
        for layer in layers {
            layer.query_into(state, order, &mut out, stop)?;
            if stop.stopped() {
                break;
            }
        }
        Some(out)
    }

    /// The [`LayerIndex`] of tables with many layers.
    fn layer_index(&self) -> Option<&LayerIndex> {
        if self.layers.len() < INDEXED_LAYERS {
            return None;
        }
        self.index
            .get_or_init(|| LayerIndex::new(&self.layers))
            .as_ref()
    }

    /// The haps for `span` while a table is built, sampled ones with their
    /// samplers not run, or `None` past [`MAX_ENTRIES`] haps.
    fn structure(&self, span: TimeSpan) -> Option<Structure> {
        let mut stop = Stop::build();
        if !self.answerable(span, &stop)? {
            return None;
        }
        let mut out = Structure::default();
        for layer in &self.layers {
            layer.structure_into(span, &mut out, &mut stop)?;
            if stop.stopped() {
                return None;
            }
        }
        Some(out)
    }

    /// The whole answer for `span` while a table is built, or `None` past
    /// [`MAX_ENTRIES`] haps.
    fn answer(&self, span: TimeSpan) -> Option<Vec<Hap>> {
        // A sampled layer's values exist only in a query.
        if self.layers.iter().any(|layer| layer.sampled.is_some()) {
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
        mut cycle: impl FnMut(i128) -> Option<Structure>,
    ) -> Option<Self> {
        // Every cycle is asked, an empty one too.
        if cycles > i128::try_from(MAX_ENTRIES).ok()? {
            return None;
        }
        let mut entries = Structure::default();
        for c in 0..cycles {
            entries.append(cycle(c)?);
            if entries.haps.len() > MAX_ENTRIES {
                return None;
            }
        }
        let work = Work {
            cycles: Cycles::split(children.cycles)?,
            haps: children.haps.bounded(Fraction::ONE),
            levels: children.levels.saturating_add(levels_above),
            times: children.times.cycled(),
        };
        Some(Self::new(vec![entries.into_layer(cycles)?], work))
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
        Node::Sequence(layers) => sequence(layers),
        Node::AddContext(pattern, locations) => {
            stepped(&*pattern.tables()?, Step::Locations(locations.clone()))
        }
        Node::Fast(pattern, factor, _) => fast(&*pattern.tables()?, *factor),
        Node::Shift(pattern, by, _) => shift(&*pattern.tables()?, *by),
        Node::Stack(patterns) => stack(patterns),
        Node::SplitQueries(inner) => split_queries(inner),
        Node::Pick {
            selector,
            lookup,
            index_mode,
            mode,
        } => picked(&*selector.tables()?, lookup, *index_mode, *mode),
        Node::Join(inner, mode) => match inner.node_kind() {
            Node::PatternOfPure(carrier, f, memo) => joined(&*carrier.tables()?, f, memo, *mode),
            Node::PatternOfPureOperand(carrier, operand, map, memo) => {
                // The registered body fixed to this node's operand: one
                // closure per build, never per evaluation.
                let construct: crate::ToPurePatternFn = {
                    let map = Arc::clone(map);
                    let operand = operand.clone();
                    Arc::new(move |value| map.apply(value, &operand))
                };
                joined(&*carrier.tables()?, &construct, memo, *mode)
            }
            _ => None,
        },
        Node::FmapNative(pattern, f) => stepped(&*pattern.tables()?, Step::Map(f.clone())),
        Node::FilterValues(pattern, keep) => stepped(&*pattern.tables()?, Step::Keep(keep.clone())),
        Node::MapHapsNative(pattern, f, SpanUse::Ignores) => {
            stepped(&*pattern.tables()?, Step::MapHap(f.clone()))
        }
        Node::MapHapsOwned(pattern, f, SpanUse::Ignores) => {
            stepped(&*pattern.tables()?, Step::MapOwned(f.clone()))
        }
        Node::ExpandHapsNative(pattern, f, SpanUse::Ignores) => expanded(&*pattern.tables()?, f),
        Node::AppLeft(functions, values, apply, flow, _) => match signal_of(values) {
            Some(signal) => sampled(&*functions.tables()?, signal, Side::Value, apply, *flow),
            None if sampling(functions) || sampling(values) => {
                met(functions, values, Side::Value, apply, *flow)
            }
            None => applied(
                &*functions.tables()?,
                &*values.tables()?,
                |function, value| {
                    let part = function.part.intersection(&value.part)?;
                    Some(applied_hap(
                        function,
                        value,
                        function.whole,
                        part,
                        apply,
                        *flow,
                    ))
                },
            ),
        },
        Node::AppRight(functions, values, apply, flow) => match signal_of(functions) {
            Some(signal) => sampled(&*values.tables()?, signal, Side::Function, apply, *flow),
            None if sampling(values) || sampling(functions) => {
                met(values, functions, Side::Function, apply, *flow)
            }
            None => applied(
                &*values.tables()?,
                &*functions.tables()?,
                |value, function| {
                    let part = function.part.intersection(&value.part)?;
                    Some(applied_hap(
                        function,
                        value,
                        value.whole,
                        part,
                        apply,
                        *flow,
                    ))
                },
            ),
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

/// A span-blind per-hap step: run once per entry whose value is its own,
/// and added to the samplers of the others, whose values exist only in a
/// query.
fn stepped(tables: &Tables, step: Step) -> Option<Tables> {
    let stage = Stage::Step(step);
    tables.map_layers(tables.work.above(), |layer| layer.staged(&stage, 1, true))
}

/// Run `sampler`, whose stages read only what repeats every cycle, on
/// `entry` where it is, while a table is built. What it makes joins `out`
/// with values of its own. `None` when a step moves the hap, which a
/// span-blind step must not do.
fn run_fixed(sampler: &Sampler, entry: &Hap, out: &mut Structure) -> Option<()> {
    let whole = entry.whole?;
    let from = out.haps.len();
    sampler.run(
        entry,
        whole,
        entry.part,
        &mut State::new(whole),
        &mut Vec::new(),
        &mut out.haps,
    )?;
    let stepped = sampler
        .stages
        .iter()
        .all(|stage| matches!(stage, Stage::Step(_)));
    let inside = |part: TimeSpan| {
        if stepped {
            part == entry.part
        } else {
            part.begin >= entry.part.begin && part.end <= entry.part.end
        }
    };
    if !out.haps[from..]
        .iter()
        .all(|hap| hap.whole == Some(whole) && inside(hap.part))
    {
        return None;
    }
    out.samplers.resize(out.haps.len(), None);
    Some(())
}

/// A span-blind map of each hap to several, all at its whole and part, in
/// the order the map gives them.
fn expanded(tables: &Tables, expand: &HapExpandFn) -> Option<Tables> {
    tables.map_layers(tables.work.above(), |layer| {
        // A sampled layer's values exist only in a query.
        if layer.sampled.is_some() {
            return None;
        }
        let mut entries = Vec::with_capacity(layer.entries.len());
        for entry in &layer.entries {
            for mapped in expand(entry) {
                if mapped.whole != entry.whole || mapped.part != entry.part {
                    return None;
                }
                entries.push(mapped);
                if entries.len() > MAX_ENTRIES {
                    return None;
                }
            }
        }
        Layer::new(layer.period, entries)
    })
}

/// A signal under span-blind value steps, speed changes, shifts and
/// meetings with constants, as a [`Signal`]. `None` for anything else, or for
/// a graph that may not answer from tables: one that may answer differently
/// to the same query, or that binds another runtime.
fn signal_of(pattern: &Pattern) -> Option<Signal> {
    if !pattern.tables_may_answer() {
        return None;
    }
    let _depth = BuildDepth::enter()?;
    let (inner, op) = match pattern.node_kind() {
        Node::Signal(signal) => {
            return Some(Signal {
                source: Source::Signal(signal.clone()),
                ops: Vec::new(),
            });
        }
        Node::FmapNative(inner, f) => (inner, SignalOp::Step(Step::Map(f.clone()))),
        Node::FilterValues(inner, keep) => (inner, SignalOp::Step(Step::Keep(keep.clone()))),
        Node::MapHapsNative(inner, f, SpanUse::Ignores) => {
            (inner, SignalOp::Step(Step::MapHap(f.clone())))
        }
        Node::MapHapsOwned(inner, f, SpanUse::Ignores) => {
            (inner, SignalOp::Step(Step::MapOwned(f.clone())))
        }
        Node::AddContext(inner, locations) => {
            (inner, SignalOp::Step(Step::Locations(locations.clone())))
        }
        Node::Fast(inner, factor, _) if *factor > Fraction::ZERO => {
            (inner, SignalOp::Fast(*factor))
        }
        Node::Shift(inner, by, _) => (inner, SignalOp::Shift(*by)),
        // The signal is the structure side, and a constant gives the rest.
        Node::AppLeft(functions, values, apply, flow, _) => {
            let (hap, levels) = constant_of(values)?;
            (
                functions,
                SignalOp::Constant {
                    hap,
                    side: Side::Value,
                    apply: apply.clone(),
                    flow: *flow,
                    levels,
                },
            )
        }
        Node::AppRight(functions, values, apply, flow) => {
            let (hap, levels) = constant_of(functions)?;
            (
                values,
                SignalOp::Constant {
                    hap,
                    side: Side::Function,
                    apply: apply.clone(),
                    flow: *flow,
                    levels,
                },
            )
        }
        _ => return None,
    };
    let mut signal = signal_of(inner)?;
    signal.ops.push(op);
    Some(signal)
}

/// A signal on the value side of a meeting, for a query that walks the
/// graph: the walk reads the signal as a table of the meeting samples it,
/// with no query of the signal's graph.
pub(crate) struct ValueSignal(Signal);

impl ValueSignal {
    /// `values` as a signal, when it is one that a table can sample.
    pub(crate) fn of(values: &Pattern) -> Option<Self> {
        signal_of(values).map(Self)
    }

    /// The haps `values` answers for `span`, in its order, into `out`.
    /// `part` is the part of the hap that asks, which lies in one cycle.
    /// `None` for a span that only the graph answers. `probe` is a copy of
    /// the query's state.
    pub(crate) fn answer(
        &self,
        span: TimeSpan,
        part: TimeSpan,
        probe: &mut State,
        out: &mut Vec<Hap>,
    ) -> Option<()> {
        self.0.answer(span, part, probe, out)
    }
}

/// The hap of cycle zero of a pattern whose tables hold one whole hap a
/// cycle, such as `pure(value)`, and the levels of its walk.
fn constant_of(pattern: &Pattern) -> Option<(Box<Hap>, u32)> {
    let cycle = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
    // A constant by its node kinds is read from its graph, apart from the
    // query around this build. It needs no table, so it is a constant with
    // any table budget.
    if let Some(levels) = pattern.constant_levels() {
        if !pattern.tables_may_answer() {
            return None;
        }
        let hap = crate::without_query_signals(|| pattern.constant_cycle(&State::new(cycle)))?;
        return hap.keys.is_empty().then(|| (Box::new(hap), levels));
    }
    let tables = pattern.tables()?;
    let [layer] = tables.layers.as_slice() else {
        return None;
    };
    let [entry] = layer.entries.as_slice() else {
        return None;
    };
    (layer.period == 1
        && layer.sampled.is_none()
        && entry.keys.is_empty()
        && entry.whole == Some(cycle)
        && entry.part == cycle)
        .then(|| (Box::new(entry.clone()), tables.work.levels))
}

/// What the graph walks to answer `signal` over a whole at most `width`
/// cycles long, reached `extent` cycles past the query span, and the most
/// haps that answer holds.
fn signal_work(signal: &Signal, width: Fraction, extent: f64, bits: u32) -> Option<(Work, u64)> {
    // The span each level is asked over is at most this wide, from the
    // outermost in. A constant walks the cycles of its span, and each cycle
    // bound in it can cut the signal's haps once more.
    let mut cycles = Cycles::NONE;
    // A signal answers one hap over the span it is asked over. A constant
    // answers its hap once a cycle of the span, counted below: a whole hap
    // a cycle, not one more than that, or every meeting of one multiplies
    // the haps the next is charged for.
    let mut most = if matches!(signal.source, Source::Constant(_)) {
        0u64
    } else {
        1u64
    };
    let mut wide = width;
    // The signal, and one level for each node above it. A constant's own
    // pattern is a child of its meeting.
    let mut levels = u32::try_from(signal.ops.len()).ok()?.saturating_add(1);
    for (depth, op) in signal.ops.iter().rev().enumerate() {
        match op {
            SignalOp::Fast(factor) => wide = wide.checked_mul(*factor)?,
            SignalOp::Constant {
                levels: constant, ..
            } => {
                cycles = cycles.max(Cycles::walk().bounded(wide)?);
                most = most.saturating_add(cycles_up(wide));
                let below = u32::try_from(depth).ok()?.saturating_add(1);
                levels = levels.max(below.saturating_add(*constant));
            }
            SignalOp::Cycle(CycleMap::Gap(factor)) => wide = wide.checked_mul(*factor)?,
            SignalOp::Step(_)
            | SignalOp::Shift(_)
            | SignalOp::Cycle(_)
            | SignalOp::Restart { .. } => {}
        }
    }
    if let Source::Constant(_) = signal.source {
        cycles = cycles.max(Cycles::walk().bounded(wide)?);
        most = most.saturating_add(cycles_up(wide));
    }
    // The times of each level, from the signal out.
    let mut times = Times::SAME;
    for op in &signal.ops {
        times = match op {
            SignalOp::Fast(factor) => times.scaled(*factor),
            SignalOp::Shift(by) => times.shifted(*by),
            SignalOp::Cycle(_) | SignalOp::Restart { .. } => times.cycled(),
            SignalOp::Step(_) | SignalOp::Constant { .. } => times,
        };
    }
    let work = Work {
        cycles,
        haps: Count {
            per_cycle: 0,
            fixed: most,
        },
        levels,
        times: times.reaching(extent).finer(bits),
    };
    Some((work, most))
}

/// `structure` meeting a signal: its own haps, with values the query makes.
/// A layer that meets a signal already meets this one after it.
fn sampled(
    structure: &Tables,
    signal: Signal,
    side: Side,
    apply: &Combine,
    flow: LookupFlow,
) -> Option<Tables> {
    let (walk, most) = signal_work(
        &signal,
        structure.longest_whole()?,
        structure.extent,
        structure.entry_bits,
    )?;
    let work = Work {
        cycles: structure.work.cycles.max(walk.cycles),
        haps: structure.work.haps.max(walk.haps),
        levels: structure.work.levels.max(walk.levels).saturating_add(1),
        times: structure.work.times.max(walk.times),
    };
    // A constant repeats every cycle. A signal has a value of its own at
    // every time.
    let fixed = matches!(signal.source, Source::Constant(_)) && signal.ops.is_empty();
    let stage = Stage::Meet {
        signal,
        side,
        apply: apply.clone(),
        flow,
    };
    structure.map_layers(work, |layer| layer.staged(&stage, most, fixed))
}

/// Whether the tables of `pattern` hold a layer whose values a query makes.
fn sampling(pattern: &Pattern) -> bool {
    pattern
        .tables()
        .is_some_and(|tables| tables.layers.iter().any(|layer| layer.sampled.is_some()))
}

/// A `structure` meeting `other` on `side` where either holds layers whose
/// values a query makes: a constant meets as one more stage of every
/// sampler, and anything else as a stage of each entry's own, which holds
/// the haps `other` answers over that entry's whole.
fn met(
    structure: &Pattern,
    other: &Pattern,
    side: Side,
    apply: &Combine,
    flow: LookupFlow,
) -> Option<Tables> {
    if sampling(structure) && constant_of(other).is_some() {
        return met_constant(structure, other, side, apply, flow);
    }
    let structure = structure.tables()?;
    let other = other.tables()?;
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
        times: structure.work.times.max(
            other
                .work
                .times
                .reaching(structure.extent)
                .finer(structure.entry_bits),
        ),
    };
    let mut scanned = 0usize;
    structure.map_layers(work, |layer| {
        let copies = usize::try_from(period.checked_div(layer.period)?).ok()?;
        let entries = layer.copies(period)?;
        let samplers = match &layer.sampled {
            Some(sampled) => {
                let sampled = sampled.repeated(copies)?;
                (0..entries.len())
                    .map(|index| {
                        let sampler = usize::try_from(*sampled.of.get(index)?).ok()?;
                        Some(Some(Arc::clone(sampled.samplers.get(sampler)?)))
                    })
                    .collect::<Option<Vec<_>>>()?
            }
            None => vec![None; entries.len()],
        };
        let mut out = Structure::default();
        for (entry, sampler) in entries.into_iter().zip(samplers) {
            let whole = entry.whole?;
            let fixed = other.structure(whole)?;
            scanned = scanned.saturating_add(fixed.haps.len());
            if scanned > MAX_BUILD_SCAN {
                return None;
            }
            let most = fixed
                .samplers
                .iter()
                .map(|sampler| sampler.as_ref().map_or(1, |sampler| sampler.most))
                .fold(0u64, u64::saturating_add)
                .max(1);
            // An entry whose value is its own meets haps whose values are
            // their own here, once.
            let here = sampler
                .as_ref()
                .is_none_or(|sampler| sampler.stages.is_empty())
                && fixed.samplers.iter().all(Option::is_none);
            let mut sampler = match sampler {
                Some(sampler) => Sampler::clone(&sampler),
                None => Sampler {
                    stages: Vec::new(),
                    most: 1,
                },
            };
            sampler.push(Stage::Meet {
                signal: Signal {
                    source: Source::Fixed(Arc::new(Fixed {
                        whole,
                        haps: fixed.haps,
                        samplers: fixed.samplers,
                    })),
                    ops: Vec::new(),
                },
                side,
                apply: apply.clone(),
                flow,
            });
            sampler.most = sampler.most.saturating_mul(most);
            if here {
                run_fixed(&sampler, &entry, &mut out)?;
            } else {
                out.haps.push(entry);
                out.samplers.push(Some(Arc::new(sampler)));
            }
            if out.haps.len() > MAX_ENTRIES {
                return None;
            }
        }
        out.into_layer(period)
    })
}

/// A `structure` with sampled layers meeting a constant on `side`: the
/// constant's hap of each cycle the whole meets, as one more meeting of the
/// sampled layers. See [`sampled`].
fn met_constant(
    structure: &Pattern,
    constant: &Pattern,
    side: Side,
    apply: &Combine,
    flow: LookupFlow,
) -> Option<Tables> {
    let (hap, levels) = constant_of(constant)?;
    let signal = Signal {
        source: Source::Constant(hap),
        ops: Vec::new(),
    };
    let tables = sampled(&*structure.tables()?, signal, side, apply, flow)?;
    // The constant's own walk sits below the meeting.
    Some(Tables::new(
        tables.layers,
        Work {
            levels: tables.work.levels.max(levels.saturating_add(1)),
            ..tables.work
        },
    ))
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
        times: structure.work.times.max(
            other
                .work
                .times
                .reaching(structure.extent)
                .finer(structure.entry_bits),
        ),
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

/// `pick`: each selector hap's value picks a pattern from `lookup`, and the
/// join keeps that pattern's haps inside the selector hap. `restart` moves
/// the pattern to start where the selector hap starts, and `reset` to start
/// where that hap's cycle starts. The other joins keep it in place.
fn picked(
    selectors: &Tables,
    lookup: &PickLookup,
    index_mode: PickIndexMode,
    mode: JoinMode,
) -> Option<Tables> {
    let moved = match mode {
        JoinMode::Restart | JoinMode::Reset => true,
        JoinMode::Inner | JoinMode::Outer | JoinMode::Mix => false,
        JoinMode::Squeeze => return None,
    };
    // A restarted pattern moves with its selector hap, so the answer repeats
    // with the selector. Any other join repeats when both do.
    let mut period = selectors.period()?;
    let mut picked: Option<Work> = None;
    for layer in &selectors.layers {
        for entry in &layer.entries {
            let tables = lookup.select(&entry.value, index_mode)?.tables()?;
            if mode != JoinMode::Restart {
                period = lcm(period, tables.period()?)?;
            }
            picked = Some(picked.map_or(tables.work, |work| work.max(tables.work)));
        }
    }
    let picked = picked.unwrap_or(Work::leaf(Cycles::NONE));
    // The graph asks a moved pattern over the whole query span, through one
    // more level, and any other over the part of each selector hap.
    let work = Work {
        cycles: selectors.work.cycles.max(picked.cycles),
        haps: selectors.work.haps.max(picked.haps),
        levels: selectors
            .work
            .levels
            .max(picked.levels.saturating_add(u32::from(moved)))
            .saturating_add(1),
        times: selectors.work.times.max(if moved {
            picked
                .times
                .restarted(selectors.extent, selectors.entry_bits)
        } else {
            // Asked over the part of each selector hap.
            picked.times.finer(selectors.entry_bits)
        }),
    };
    let mut scanned = 0usize;
    selectors.map_layers(work, |layer| {
        let mut entries = Structure::default();
        for outer in layer.repeated(period)? {
            let mut met = Vec::new();
            let whole = outer.whole?;
            let tables = lookup.select(&outer.value, index_mode)?.tables()?;
            let by = match mode {
                JoinMode::Restart => whole.begin,
                JoinMode::Reset => whole.begin.cycle_pos(),
                _ => Fraction::ZERO,
            };
            let span = TimeSpan::new(
                outer.part.begin.checked_sub(by)?,
                outer.part.end.checked_sub(by)?,
            );
            let mut answer = tables.structure(span)?;
            scanned = scanned.saturating_add(answer.haps.len());
            if scanned > MAX_BUILD_SCAN {
                return None;
            }
            if moved {
                // A signal inside is asked where the moved pattern was. A
                // restart moves each copy of the selector hap by its own
                // start, which a selector hap no longer than the period
                // names.
                let op = if mode == JoinMode::Restart {
                    if whole.end.checked_sub(whole.begin)? > Fraction::int(period)
                        && answer.samplers.iter().any(Option::is_some)
                    {
                        return None;
                    }
                    SignalOp::Restart {
                        begin: whole.begin,
                        period,
                    }
                } else {
                    SignalOp::Shift(by)
                };
                answer = answer.retimed(|hap| retimed(hap, |t| t.checked_add(by)), &[op])?;
            }
            for (inner, sampler) in answer.haps.into_iter().zip(answer.samplers) {
                let Some(part) = inner.part.intersection(&outer.part) else {
                    continue;
                };
                let joined = match mode {
                    JoinMode::Inner => inner.whole,
                    JoinMode::Outer => outer.whole,
                    // Wholes that do not meet throw, which the graph handles.
                    JoinMode::Mix => Some(whole.intersection(&inner.whole?)?),
                    _ => inner
                        .whole
                        .and_then(|inner_whole| inner_whole.intersection(&whole)),
                };
                match sampler {
                    None => {
                        entries.haps.push(picked_hap(&outer, inner, joined, part));
                        entries.samplers.push(None);
                    }
                    Some(sampler) => {
                        // A sampled hap samples its signals over its own
                        // whole, and meets the selector hap after its own
                        // meetings.
                        if joined != inner.whole {
                            return None;
                        }
                        let mut inner = inner;
                        inner.part = part;
                        entries.haps.push(inner);
                        entries
                            .samplers
                            .push(Some(outered(&sampler, &outer, true, &mut met)));
                    }
                }
                if entries.haps.len() > MAX_ENTRIES {
                    return None;
                }
            }
        }
        entries.into_layer(period)
    })
}

/// `sampler` with the outer hap `outer` of a pick (`picked`) or a join
/// merged in last. `met` keeps those made for one outer hap, so that its
/// inner haps share them.
fn outered(
    sampler: &Arc<Sampler>,
    outer: &Hap,
    picked: bool,
    met: &mut Vec<(*const Sampler, Arc<Sampler>)>,
) -> Arc<Sampler> {
    let key = Arc::as_ptr(sampler);
    if let Some((_, done)) = met.iter().find(|(seen, _)| *seen == key) {
        return Arc::clone(done);
    }
    let mut changed = Sampler::clone(sampler);
    changed.stages.push(Stage::Outer(Arc::new(Outer {
        hap: outer.clone(),
        picked,
    })));
    let changed = Arc::new(changed);
    met.push((key, Arc::clone(&changed)));
    changed
}

/// An inner, outer or mix join of a carrier whose values name patterns
/// through a pure construction: each carrier hap asks the pattern its value
/// names over the carrier hap's part. The construction is the one the graph
/// keeps, so both ask the same pattern. See `Node::Join`.
fn joined(
    carrier: &Tables,
    construct: &crate::ToPurePatternFn,
    memo: &crate::ConstructionMemo,
    mode: JoinMode,
) -> Option<Tables> {
    if !matches!(mode, JoinMode::Inner | JoinMode::Outer | JoinMode::Mix) {
        return None;
    }
    let settings = crate::settings::current_state_id();
    // The pattern each carrier value names, each built once.
    let mut named: Vec<(&crate::Value, Pattern)> = Vec::new();
    let mut period = carrier.period()?;
    let mut inner: Option<Work> = None;
    for layer in &carrier.layers {
        for entry in &layer.entries {
            if named
                .iter()
                .any(|(value, _)| crate::same_key(value, &entry.value))
            {
                continue;
            }
            let pattern = memo
                .kept_for_table(&entry.value, &settings, |value| construct(value))?
                .pattern()
                .clone();
            let tables = pattern.tables()?;
            period = lcm(period, tables.period()?)?;
            inner = Some(inner.map_or(tables.work, |work| work.max(tables.work)));
            named.push((&entry.value, pattern));
        }
    }
    let named = |value: &crate::Value| {
        named
            .iter()
            .find(|(named, _)| crate::same_key(named, value))
            .map(|(_, pattern)| pattern)
    };
    let inner = inner.unwrap_or(Work::leaf(Cycles::NONE));
    // The join resolves the carrier through one more level, and asks each
    // pattern over the part of a carrier hap.
    let work = Work {
        cycles: carrier.work.cycles.max(inner.cycles),
        haps: carrier.work.haps.max(inner.haps),
        levels: carrier
            .work
            .levels
            .saturating_add(1)
            .max(inner.levels)
            .saturating_add(1),
        times: carrier
            .work
            .times
            .max(inner.times.finer(carrier.entry_bits)),
    };
    let mut scanned = 0usize;
    carrier.map_layers(work, |layer| {
        let mut entries = Structure::default();
        for outer in layer.repeated(period)? {
            let mut met = Vec::new();
            let answer = named(&outer.value)?.tables()?.structure(outer.part)?;
            scanned = scanned.saturating_add(answer.haps.len());
            if scanned > MAX_BUILD_SCAN {
                return None;
            }
            for (hap, sampler) in answer.haps.into_iter().zip(answer.samplers) {
                let whole = match mode {
                    JoinMode::Inner => hap.whole,
                    JoinMode::Outer => outer.whole,
                    // Wholes that do not meet throw, which the graph handles.
                    _ => Some(outer.whole?.intersection(&hap.whole?)?),
                };
                match sampler {
                    None => {
                        entries.haps.push(joined_hap(&outer, hap, whole));
                        entries.samplers.push(None);
                    }
                    Some(sampler) => {
                        // A sampled hap samples its signals over its own
                        // whole, and meets the carrier hap after its own
                        // meetings.
                        if whole != hap.whole {
                            return None;
                        }
                        entries.haps.push(hap);
                        entries
                            .samplers
                            .push(Some(outered(&sampler, &outer, false, &mut met)));
                    }
                }
                if entries.haps.len() > MAX_ENTRIES {
                    return None;
                }
            }
        }
        entries.into_layer(period)
    })
}

/// The tables of a [`crate::sequence`]: the steps of each layer as one cycle
/// a period, at the layer's speed, as a [`fast`] of them is.
fn sequence(layers: &[crate::sequence::Layer]) -> Option<Tables> {
    let mut all = Vec::new();
    let mut work: Option<Work> = None;
    for layer in layers {
        let steps = Tables::new(
            vec![Layer::new(1, layer.period_haps())?],
            Work::leaf(Cycles::walk()),
        );
        let timed = if layer.period == Fraction::ONE {
            steps
        } else {
            fast(&steps, Fraction::ONE.checked_div(layer.period)?)?
        };
        all.extend(timed.layers.iter().cloned());
        work = Some(work.map_or(timed.work, |work| work.max(timed.work)));
    }
    // One node answers for every layer at its speed: the levels of a leaf.
    let work = Work { levels: 1, ..work? };
    Some(Tables::new(all, work))
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
            .copies(layer.period.checked_mul(copies)?)?
            .iter()
            .map(|entry| retimed(entry, |t| t.checked_div(factor)))
            .collect::<Option<_>>()?;
        // A signal is sampled where its meeting is, so it speeds up with it.
        let sampled = match &layer.sampled {
            Some(sampled) => Some(moved(
                &sampled.repeated(usize::try_from(copies).ok()?)?,
                SignalOp::Fast(factor),
            )?),
            None => None,
        };
        Some(Layer::new(period, entries)?.with_sampled(sampled))
    })
}

/// The samplers of a layer under one more time map of that layer, `op`:
/// each meeting's signal is asked through it.
fn moved(sampled: &Sampled, op: SignalOp) -> Option<Sampled> {
    sampled.map(|sampler| {
        let mut sampler = Sampler::clone(sampler);
        for stage in &mut sampler.stages {
            stage.extend_ops(std::slice::from_ref(&op));
        }
        Some(sampler)
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
        let entries = layer
            .entries
            .iter()
            .map(|entry| retimed(entry, |t| t.checked_add(by)))
            .collect::<Option<_>>()?;
        // A signal is sampled where its meeting is, so it moves with it.
        let sampled = match &layer.sampled {
            Some(sampled) => Some(moved(sampled, SignalOp::Shift(by))?),
            None => None,
        };
        Some(Layer::new(layer.period, entries)?.with_sampled(sampled))
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
        Node::Rev(pattern) => return rev(&*pattern.tables()?),
        Node::FastGap(pattern, factor) => return fast_gap(&*pattern.tables()?, *factor),
        Node::RepeatCycles(pattern, count) => return repeat_cycles(&*pattern.tables()?, *count),
        _ => {}
    }
    let tables = inner.tables()?;
    let period = tables.period()?;
    Tables::by_cycle(period, tables.work, 1, |c| tables.structure(cycle_span(c)?))
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
        child.structure(cycle_span(local)?)?.retimed(
            |hap| shifted(hap, offset),
            &[SignalOp::Cycle(CycleMap::SlowCat(count))],
        )
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
        children[usize::try_from(c.rem_euclid(count)).ok()?].structure(cycle_span(c)?)
    })
}

/// The walk of every constituent of an alternation, together.
fn constituents(children: &[Arc<Tables>]) -> Work {
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
        tables.structure(cycle_span(c)?)?.retimed(
            |hap| {
                let mut hap = hap.clone();
                hap.part = reflected(hap.part, c)?;
                hap.whole = match hap.whole {
                    Some(whole) => Some(reflected(whole, c)?),
                    None => None,
                };
                Some(hap)
            },
            &[SignalOp::Cycle(CycleMap::Reflect)],
        )
    })
}

/// `fastGap(factor)`: cycle `c` plays the inner pattern from the start of
/// cycle `c`, `factor` times as fast, and then nothing until the cycle ends.
/// A factor below one plays only the start of the inner cycle.
fn fast_gap(tables: &Tables, factor: Fraction) -> Option<Tables> {
    if factor <= Fraction::ZERO {
        return None;
    }
    let period = tables.period()?;
    let reach = factor.min(Fraction::ONE);
    // The inner pattern is asked at `c + (t - c) * factor`, within
    // `max(factor, 1)` of `t`.
    let children = Work {
        times: tables
            .work
            .times
            .shifted(factor.max(Fraction::ONE))
            .finer(log2_up(factor.denom())),
        ..tables.work
    };
    Tables::by_cycle(period, children, 2, |c| {
        let begin = Fraction::int(c);
        // A signal inside is asked at `(t - c) * factor + c`.
        tables
            .structure(TimeSpan::new(begin, begin.checked_add(reach)?))?
            .retimed(
                |hap| Pattern::fast_gap_unmap_hap(hap, factor),
                &[SignalOp::Cycle(CycleMap::Gap(factor))],
            )
    })
}

/// `repeatCycles(count)`: cycle `c` plays cycle `floor(c / count)` of the
/// inner pattern. The answer repeats after `numer(count)` inner periods.
fn repeat_cycles(tables: &Tables, count: Fraction) -> Option<Tables> {
    if count <= Fraction::ZERO {
        return None;
    }
    let period = tables.period()?.checked_mul(count.numer())?;
    // The walk divides the cycle by `count`, and asks the inner pattern up
    // to `1 / count` times as far out.
    let children = Work {
        times: tables.work.times.scaled(Fraction::ONE.checked_div(count)?),
        ..tables.work
    };
    Tables::by_cycle(period, children, 2, |c| {
        let source = Fraction::int(c).checked_div(count)?.floor().numer();
        let delta = c.checked_sub(source)?;
        tables.structure(cycle_span(source)?)?.retimed(
            |hap| shifted(hap, delta),
            &[SignalOp::Cycle(CycleMap::Repeat(count))],
        )
    })
}

/// `span` reflected in cycle `c`, as `rev` reflects it.
fn reflected(span: TimeSpan, c: i128) -> Option<TimeSpan> {
    let cycle = Fraction::int(c);
    let next = Fraction::int(c.checked_add(1)?);
    Some(TimeSpan::new(
        cycle.checked_add(next.checked_sub(span.end)?)?,
        cycle.checked_add(next.checked_sub(span.begin)?)?,
    ))
}

/// Whether `span` lies in one cycle.
fn in_one_cycle(span: TimeSpan) -> bool {
    span.begin
        .floor()
        .checked_add_int(1)
        .is_some_and(|next| span.end <= next)
}

/// The one hap of `haps` that meets `part`, with the part they share, when
/// exactly one does.
fn only_meeting(part: TimeSpan, haps: &[Hap]) -> Option<(TimeSpan, &Hap)> {
    let mut found = None;
    for hap in haps {
        if let Some(cut) = part.intersection(&hap.part) {
            if found.is_some() {
                return None;
            }
            found = Some((cut, hap));
        }
    }
    found
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
    let mut hap = hap.clone();
    retime(&mut hap, map)?;
    Some(hap)
}

/// [`retimed`] where `hap` is. `None`, with `hap` changed in part, when a
/// time leaves the native range.
fn retime(hap: &mut Hap, map: impl Fn(Fraction) -> Option<Fraction>) -> Option<()> {
    let span = |span: TimeSpan| Some(TimeSpan::new(map(span.begin)?, map(span.end)?));
    hap.part = span(hap.part)?;
    hap.whole = match hap.whole {
        Some(whole) => Some(span(whole)?),
        None => None,
    };
    Some(())
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

    /// Each hap with all it carries: value, locations, live bindings,
    /// visuals, lookup and context keys.
    fn describe(haps: &[Hap]) -> Vec<String> {
        haps.iter()
            .map(|hap| {
                format!(
                    "{} {:?} live={:?}/{} visuals={} lookup={} keys={:?}",
                    hap.show(),
                    hap.context,
                    hap.live_controls,
                    hap.slider_binding,
                    hap.ui_visuals,
                    hap.pick_lookup.is_some(),
                    hap.keys
                )
            })
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
        // A node for each speed change: a fold makes one node of them all.
        let levels = |count: usize| {
            crate::sequence::unfolded(|| {
                (0..count).fold(pure(Value::F64(1.0)), |pattern, _| {
                    pattern.fast(frac(2, 1)).slow(frac(2, 1))
                })
            })
        };
        assert!(levels(20_000).tables().is_none());
        assert!(levels(20).tables().is_some());
    }

    #[test]
    fn a_walked_graph_builds_no_table() {
        let graph = || map(vec![leaf(1.0, 0), seq(&[2.0, 3.0]), leaf(4.0, 9)], slowcat).tabled;
        let child = graph();
        let walked = child.mark_walked();
        // A node that is built over a walked graph is walked too.
        let over = walked.late(frac(1, 4));
        let tabled = graph();
        for _ in 0..4 {
            for (begin, end) in [(frac(0, 1), frac(5, 1)), (frac(1, 3), frac(7, 3))] {
                assert_eq!(
                    describe(&walked.query_arc(begin, end)),
                    describe(&tabled.query_arc(begin, end))
                );
                over.query_arc(begin, end);
            }
        }
        assert!(tabled.built_tables().is_some());
        for pattern in [&walked, &child, &over] {
            assert!(pattern.built_tables().is_none());
        }
    }

    #[test]
    fn bitwise_copies_of_whole_periods_clone_the_entries() {
        let twin = seq(&[0.0, 1.0, 2.0]);
        let tables = twin.tabled.tables().expect("a number sequence has a table");
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
                .all(|layer| layer.copyable && layer.sampled.is_some())
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
                entries.near(tick, 0).map(|near| near.haps.len()),
                Some(per_cycle),
                "{name}"
            );
        }
    }

    /// Everything a hap carries: its spans, value and locations, and the
    /// metadata a join merges.
    fn describe_all(haps: &[Hap]) -> Vec<String> {
        haps.iter()
            .map(|hap| {
                let keys = hap.keys.get();
                format!(
                    "{} {:?} tags {:?} scale {:?} {:?} edo {:?} log {:?} \
                     visuals {} live {:?} slider {} lookup {}",
                    hap.show(),
                    hap.context,
                    keys.tags,
                    keys.scale,
                    keys.scale_definition,
                    keys.edo_size,
                    keys.log_line,
                    hap.ui_visuals,
                    hap.live_controls,
                    hap.slider_binding,
                    hap.pick_lookup.is_some(),
                )
            })
            .collect()
    }

    /// The answer a host gets in `order`, or the refusal.
    fn answer_in(pattern: &Pattern, span: TimeSpan, order: Order) -> Result<Vec<String>, String> {
        let state = crate::State::new(span);
        let answer = match order {
            Order::Graph => pattern.query_arc_outcome_with_budget(&state, u64::MAX),
            Order::Sorted => pattern.query_arc_outcome_sorted_with_budget(&state, u64::MAX),
        };
        match answer {
            Ok(crate::QueryArcOutcome::Haps(haps)) => Ok(describe_all(&haps)),
            Ok(other) => Err(format!("{other:?}")),
            Err(limit) => Err(format!("{limit:?}")),
        }
    }

    /// The tabled twin answers every span from its own tables, with every
    /// hap and its metadata as the graph twin gives them: in the graph's
    /// order, and sorted as a stable sort of the graph's answer. A span that
    /// the tables leave to the graph fails.
    fn assert_twins_over(name: &str, twin: &Twin, spans: &[(Fraction, Fraction)]) {
        assert!(twin.tabled.tables().is_some(), "{name} has no table");
        assert!(
            twin.graph.tables().is_none(),
            "{name}: the graph twin has a table"
        );
        for &(begin, end) in spans {
            let span = TimeSpan::new(begin, end);
            assert!(
                twin.tabled.answers_from_tables(span),
                "{name} over {begin} .. {end} went to the graph"
            );
            let graph = twin.graph.query_arc(begin, end);
            assert_eq!(
                describe_all(&twin.tabled.query_arc(begin, end)),
                describe_all(&graph),
                "{name} over {begin} .. {end}"
            );
            let sorted = Ok(describe_all(&crate::sort_haps_for_query(graph)));
            assert_eq!(
                answer_in(&twin.graph, span, Order::Sorted),
                sorted,
                "{name}: the graph twin sorted over {begin} .. {end}"
            );
            assert_eq!(
                answer_in(&twin.tabled, span, Order::Sorted),
                sorted,
                "{name} sorted over {begin} .. {end}"
            );
        }
    }

    fn assert_twins(name: &str, twin: &Twin) {
        assert_twins_over(name, twin, &spans());
    }

    /// Spans across the bounds of `period`-cycle periods, before and after
    /// cycle zero: narrow ones, wide ones, and ones that end or begin there.
    fn straddles(period: i128) -> Vec<(Fraction, Fraction)> {
        let mut spans = Vec::new();
        for k in -2..=3 {
            let bound = Fraction::int(k * period);
            for (before, after) in [
                (frac(1, 20), frac(1, 20)),
                (frac(1, 3), frac(1, 4)),
                (frac(2, 1), frac(1, 2)),
                (frac(0, 1), frac(1, 7)),
                (frac(1, 7), frac(0, 1)),
                (frac(7, 2), frac(9, 4)),
            ] {
                spans.push((bound - before, bound + after));
            }
        }
        spans
    }

    /// A weighted sequence, as `a@2 b` writes one: each step squeezed into
    /// its share of the cycle by `compress`.
    fn weighted(steps: Vec<(Fraction, Twin)>) -> Twin {
        let (weights, twins): (Vec<_>, Vec<_>) = steps.into_iter().unzip();
        map(twins, move |patterns| {
            let items: Vec<_> = weights
                .iter()
                .map(|weight| Some(*weight))
                .zip(patterns)
                .collect();
            crate::combinators::stepcat(&items)
        })
    }

    /// Six cycles: a sequence, a leaf, and a sequence slowed over two cycles.
    fn alternating() -> Twin {
        map(
            vec![
                seq(&[0.0, 1.0]),
                leaf(2.0, 9),
                one(seq(&[3.0, 4.0, 5.0]), |p| p.slow(frac(2, 1))),
            ],
            slowcat,
        )
    }

    #[test]
    fn fast_gaps_above_and_below_one_match_the_graph() {
        for (n, d) in [
            (2, 1),
            (3, 1),
            (3, 2),
            (5, 4),
            (1, 1),
            (4, 5),
            (1, 2),
            (2, 7),
        ] {
            let factor = frac(n, d);
            assert_twins(
                &format!("fastGap {n}/{d}"),
                &one(seq(&[0.0, 1.0, 2.0]), |p| p.fast_gap(factor)),
            );
            assert_twins(
                &format!("fastGap {n}/{d} of a shifted alternation"),
                &one(alternating(), |p| p.late(frac(1, 4)).fast_gap(factor)),
            );
            assert_twins(
                &format!("fastGap {n}/{d} of a slow sequence"),
                &one(seq(&[0.0, 1.0]), |p| p.slow(frac(5, 2)).fast_gap(factor)),
            );
            assert_twins(
                &format!("fastGap {n}/{d} under fast"),
                &one(seq(&[0.0, 1.0, 2.0]), |p| {
                    p.fast_gap(factor).fast(frac(3, 2))
                }),
            );
        }
    }

    #[test]
    fn weighted_steps_and_compress_before_or_after_a_speed_change_match_the_graph() {
        assert_twins(
            "a@2 b",
            &weighted(vec![(frac(2, 1), leaf(0.0, 0)), (frac(1, 1), leaf(1.0, 2))]),
        );
        assert_twins(
            "a b@2",
            &weighted(vec![(frac(1, 1), leaf(0.0, 0)), (frac(2, 1), leaf(1.0, 2))]),
        );
        assert_twins(
            "a@3 [b c] d@1/2",
            &weighted(vec![
                (frac(3, 1), leaf(0.0, 0)),
                (frac(1, 1), seq(&[1.0, 2.0])),
                (frac(1, 2), leaf(3.0, 8)),
            ]),
        );
        assert_twins(
            "<a b>@2 c <d e f>@3/2",
            &weighted(vec![
                (frac(2, 1), map(vec![leaf(0.0, 0), leaf(1.0, 2)], slowcat)),
                (frac(1, 1), leaf(2.0, 4)),
                (
                    frac(3, 2),
                    map(vec![leaf(3.0, 6), leaf(4.0, 8), leaf(5.0, 10)], slowcat),
                ),
            ]),
        );
        assert_twins(
            "a slow step@2 b",
            &weighted(vec![
                (
                    frac(2, 1),
                    one(seq(&[0.0, 1.0, 2.0]), |p| p.slow(frac(3, 2))),
                ),
                (frac(1, 1), leaf(3.0, 8)),
            ]),
        );
        for (begin, end) in [
            (frac(0, 1), frac(1, 1)),
            (frac(0, 1), frac(1, 3)),
            (frac(1, 3), frac(1, 1)),
            (frac(1, 4), frac(3, 4)),
            (frac(1, 5), frac(1, 2)),
            (frac(2, 3), frac(5, 6)),
        ] {
            assert_twins(
                &format!("compress {begin} {end}"),
                &one(seq(&[0.0, 1.0, 2.0]), |p| p.compress(begin, end)),
            );
            assert_twins(
                &format!("compress {begin} {end} of a fast sequence"),
                &one(seq(&[0.0, 1.0, 2.0]), |p| {
                    p.fast(frac(2, 1)).compress(begin, end)
                }),
            );
            assert_twins(
                &format!("fast of compress {begin} {end}"),
                &one(seq(&[0.0, 1.0, 2.0]), |p| {
                    p.compress(begin, end).fast(frac(2, 1))
                }),
            );
            assert_twins(
                &format!("slow of compress {begin} {end} of an alternation"),
                &one(alternating(), |p| p.compress(begin, end).slow(frac(3, 2))),
            );
        }
        assert_twins(
            "compress of compress",
            &one(seq(&[0.0, 1.0, 2.0]), |p| {
                p.compress(frac(1, 4), frac(3, 4))
                    .compress(frac(1, 3), frac(1, 1))
            }),
        );
    }

    #[test]
    fn repeated_cycles_whole_and_fractional_match_the_graph_also_under_fast() {
        for (n, d) in [(1, 1), (2, 1), (3, 1), (3, 2), (5, 3), (1, 2), (2, 3)] {
            let count = frac(n, d);
            assert_twins(
                &format!("repeatCycles {n}/{d}"),
                &one(alternating(), |p| p.repeat_cycles(count)),
            );
            assert_twins(
                &format!("repeatCycles {n}/{d} under fast"),
                &one(alternating(), |p| p.repeat_cycles(count).fast(frac(3, 2))),
            );
            assert_twins(
                &format!("repeatCycles {n}/{d} of a fast alternation"),
                &one(alternating(), |p| p.fast(frac(2, 1)).repeat_cycles(count)),
            );
            assert_twins(
                &format!("repeatCycles {n}/{d} of a shifted slow sequence"),
                &one(seq(&[0.0, 1.0, 2.0]), |p| {
                    p.slow(frac(3, 2)).late(frac(1, 3)).repeat_cycles(count)
                }),
            );
            assert_twins(
                &format!("repeatCycles {n}/{d} in a sequence"),
                &map(
                    vec![
                        one(alternating(), |p| p.repeat_cycles(count)),
                        leaf(7.0, 30),
                    ],
                    fastcat,
                ),
            );
        }
    }

    /// What a selector picks from: periods of one, two and three cycles, a
    /// chord of two layers, and a tagged hap longer than a cycle that starts
    /// between cycles.
    fn choices() -> Vec<Twin> {
        vec![
            seq(&[10.0, 11.0]),
            map(vec![leaf(20.0, 20), seq(&[21.0, 22.0])], slowcat),
            map(
                vec![
                    seq(&[30.0, 31.0, 32.0]),
                    one(leaf(33.0, 30), |p| p.slow(frac(3, 1))),
                ],
                stack,
            ),
            one(leaf(40.0, 40), |p| {
                p.slow(frac(2, 1)).late(frac(1, 3)).tag("chosen".into())
            }),
        ]
    }

    fn picked_from(
        selector: Twin,
        choices: Vec<Twin>,
        index_mode: PickIndexMode,
        mode: JoinMode,
    ) -> Twin {
        let count = choices.len();
        let lookup = |entries: Vec<(usize, Pattern)>| PickLookup::Array {
            enumerable_len: count,
            length: count,
            entries,
        };
        let (tabled, graph): (Vec<_>, Vec<_>) = choices
            .into_iter()
            .enumerate()
            .map(|(index, twin)| ((index, twin.tabled), (index, twin.graph)))
            .unzip();
        Twin {
            tabled: crate::pick(selector.tabled, lookup(tabled), index_mode, mode),
            graph: crate::pick(selector.graph, lookup(graph), index_mode, mode),
        }
    }

    /// Slow haps that start between cycles, cut at every cycle.
    fn cut_late_steps() -> Twin {
        one(seq(&[3.0, 1.0, 2.0]), |p| {
            p.slow(frac(5, 2)).late(frac(1, 4)).split_queries()
        })
    }

    /// Steps, weighted steps, a tagged alternation, and cut late steps.
    fn selectors() -> Vec<(&'static str, Twin)> {
        vec![
            ("steps", seq(&[0.0, 1.0, 2.0, 3.0])),
            (
                "weighted steps",
                weighted(vec![
                    (frac(2, 1), leaf(2.0, 50)),
                    (frac(1, 1), leaf(0.0, 52)),
                    (frac(1, 2), map(vec![leaf(3.0, 54), leaf(1.0, 56)], slowcat)),
                ]),
            ),
            (
                "an alternation",
                one(
                    map(
                        vec![leaf(1.0, 60), seq(&[3.0, 0.0]), leaf(2.0, 62)],
                        slowcat,
                    ),
                    |p| p.tag("selected".into()),
                ),
            ),
            ("cut late steps", cut_late_steps()),
        ]
    }

    #[test]
    fn picks_with_every_join_but_squeeze_match_the_graph() {
        for mode in [
            JoinMode::Inner,
            JoinMode::Outer,
            JoinMode::Mix,
            JoinMode::Restart,
            JoinMode::Reset,
        ] {
            for (name, selector) in selectors() {
                assert_twins(
                    &format!("{mode:?} pick by {name}"),
                    &picked_from(selector, choices(), PickIndexMode::Clamp, mode),
                );
            }
            assert_twins(
                &format!("{mode:?} pick by indices that wrap"),
                &picked_from(
                    seq(&[5.0, -1.0, 6.0]),
                    choices(),
                    PickIndexMode::Modulo,
                    mode,
                ),
            );
            assert_twins(
                &format!("{mode:?} pick by indices past the ends"),
                &picked_from(seq(&[9.0, -3.0]), choices(), PickIndexMode::Clamp, mode),
            );
            assert_twins(
                &format!("{mode:?} pick under fast and a shift"),
                &one(
                    picked_from(seq(&[1.0, 3.0, 2.0]), choices(), PickIndexMode::Clamp, mode),
                    |p| p.fast(frac(3, 2)).late(frac(1, 5)),
                ),
            );
        }
        let keyed = |choices: Vec<Twin>, graph: bool| PickLookup::Object {
            enumerable_len: choices.len(),
            entries: choices
                .into_iter()
                .enumerate()
                .map(|(index, twin)| {
                    let pattern = if graph { twin.graph } else { twin.tabled };
                    (index.to_string(), pattern)
                })
                .collect(),
        };
        let selector = seq(&[2.0, 0.0, 1.0]);
        assert_twins(
            "a restart pick from an object",
            &Twin {
                tabled: crate::pick(
                    selector.tabled,
                    keyed(choices(), false),
                    PickIndexMode::Clamp,
                    JoinMode::Restart,
                ),
                graph: crate::pick(
                    selector.graph,
                    keyed(choices(), true),
                    PickIndexMode::Clamp,
                    JoinMode::Restart,
                ),
            },
        );
    }

    #[test]
    fn a_squeezed_pick_stays_on_the_graph() {
        let squeezed = picked_from(
            seq(&[0.0, 1.0, 3.0]),
            choices(),
            PickIndexMode::Clamp,
            JoinMode::Squeeze,
        );
        assert!(squeezed.tabled.tables().is_none());
        for (begin, end) in spans() {
            let span = TimeSpan::new(begin, end);
            assert!(!squeezed.tabled.answers_from_tables(span));
            assert_eq!(
                describe_all(&squeezed.tabled.query_arc(begin, end)),
                describe_all(&squeezed.graph.query_arc(begin, end)),
                "squeeze over {begin} .. {end}"
            );
            assert_eq!(
                answer_in(&squeezed.tabled, span, Order::Sorted),
                answer_in(&squeezed.graph, span, Order::Sorted),
                "squeeze sorted over {begin} .. {end}"
            );
        }
    }

    #[test]
    fn a_chord_inside_a_sequence_answers_sorted_queries_from_its_one_layer() {
        // Notes of different lengths at one onset, and a slow one cut at the
        // cycle: the sorted order is not the graph's.
        let chord = || {
            map(
                vec![
                    map(
                        vec![
                            seq(&[3.0, 1.0]),
                            leaf(2.0, 7),
                            one(leaf(0.0, 9), |p| p.slow(frac(2, 1))),
                        ],
                        stack,
                    ),
                    leaf(4.0, 11),
                    seq(&[5.0, 6.0]),
                ],
                fastcat,
            )
        };
        for (name, twin) in [
            ("a chord in a sequence", chord()),
            ("a shifted chord", one(chord(), |p| p.late(frac(1, 5)))),
            ("a reversed chord", one(chord(), |p| p.rev())),
            (
                "a chord in an alternation",
                map(vec![chord(), seq(&[7.0, 8.0])], slowcat),
            ),
        ] {
            let tables = twin.tabled.tables().expect("a table");
            assert_eq!(tables.layers.len(), 1, "{name}: one layer");
            assert!(!tables.sorted, "{name}: its layer has no sorted form");
            assert_twins(name, &twin);
        }
    }

    /// A hap `cycles` long, cut at every cycle by split queries.
    fn cut(value: f64, location: usize, cycles: i128) -> Twin {
        one(leaf(value, location), move |p| {
            p.slow(Fraction::int(cycles)).split_queries()
        })
    }

    #[test]
    fn a_stack_of_fragments_answers_sorted_queries_through_its_union() {
        for (name, twin) in [
            (
                "a cut hap and a sequence",
                map(vec![cut(1.0, 1, 3), seq(&[2.0, 3.0])], stack),
            ),
            (
                "cut haps of one whole",
                map(
                    vec![
                        cut(2.0, 1, 2),
                        cut(1.0, 3, 2),
                        one(seq(&[4.0, 5.0]), |p| p.slow(frac(2, 1))),
                    ],
                    stack,
                ),
            ),
            (
                "cut sequences",
                map(
                    vec![
                        one(seq(&[1.0, 2.0, 3.0]), |p| {
                            p.slow(frac(5, 2)).split_queries()
                        }),
                        seq(&[4.0, 5.0, 6.0]),
                        cut(7.0, 9, 4),
                    ],
                    stack,
                ),
            ),
        ] {
            let tables = twin.tabled.tables().expect("a table");
            assert!(tables.layers.len() > 1, "{name}: layers");
            assert!(
                tables
                    .layers
                    .iter()
                    .flat_map(|layer| &layer.entries)
                    .any(|entry| entry.whole != Some(entry.part)),
                "{name} holds fragments"
            );
            assert!(tables.union().is_some(), "{name} has a union");
            assert_twins(name, &twin);
        }
    }

    #[test]
    fn fragments_that_share_a_whole_and_overlap_leave_the_union_out() {
        // The whole hap comes first in the stack. The same hap cut at the
        // cycle sorts before it in a union. Clipped to `0 .. 1/2`, both tie,
        // and the stack's order must decide.
        let twin = map(
            vec![one(leaf(1.0, 1), |p| p.slow(frac(2, 1))), cut(2.0, 3, 2)],
            stack,
        );
        let tables = twin.tabled.tables().expect("a table");
        let entries: Vec<Hap> = tables
            .layers
            .iter()
            .flat_map(|layer| layer.entries.iter().cloned())
            .collect();
        assert!(!ties_survive_clipping(&entries));
        assert!(tables.union().is_none());
        let mut spans = spans();
        spans.push((frac(0, 1), frac(1, 2)));
        spans.push((frac(3, 2), frac(2, 1)));
        assert_twins_over("a whole hap over its own cut", &twin, &spans);
        let state = crate::State::new(TimeSpan::new(frac(0, 1), frac(1, 2)));
        let Ok(crate::QueryArcOutcome::Haps(haps)) = twin
            .tabled
            .query_arc_outcome_sorted_with_budget(&state, u64::MAX)
        else {
            panic!("an answer");
        };
        let values: Vec<_> = haps.iter().map(|hap| hap.value.as_f64()).collect();
        assert_eq!(values, [Some(1.0), Some(2.0)]);
        // A cut layer moved across the period's bounds leaves it out too.
        let moved = map(
            vec![one(cut(1.0, 1, 2), |p| p.late(frac(1, 3))), cut(2.0, 3, 2)],
            stack,
        );
        assert!(
            moved
                .tabled
                .tables()
                .is_some_and(|tables| tables.union().is_none())
        );
        assert_twins("a cut hap over a moved cut hap", &moved);
    }

    /// A leaf in cycle `at` of every `period` cycles, and rests in the others.
    fn sparse(value: f64, location: usize, at: usize, period: usize) -> Twin {
        map(
            (0..period)
                .map(|cycle| {
                    if cycle == at {
                        leaf(value, location)
                    } else {
                        Twin {
                            tabled: silence(),
                            graph: silence(),
                        }
                    }
                })
                .collect(),
            slowcat,
        )
    }

    #[test]
    fn many_layers_answer_narrow_and_straddling_queries_through_their_index() {
        // Ten layers, each in one cycle of its own period: 60 cycles shared.
        let layers = || -> Vec<Twin> {
            [
                (2, 0),
                (3, 1),
                (4, 3),
                (5, 2),
                (6, 5),
                (3, 0),
                (4, 1),
                (2, 1),
                (5, 4),
                (6, 0),
            ]
            .into_iter()
            .enumerate()
            .map(|(index, (period, at))| sparse(index as f64, 2 * index, at, period))
            .collect()
        };
        // Parts across cycles and across the shared period's bounds: no
        // union, so sorted queries take the index too.
        let crossing = || {
            let mut twins = layers();
            twins.push(one(sparse(10.0, 20, 2, 3), |p| p.late(frac(1, 2))));
            twins.push(one(sparse(11.0, 22, 0, 2), |p| {
                p.slow(frac(3, 2)).early(frac(1, 3))
            }));
            twins
        };
        // A weighted sequence of nine steps, some resting in some cycles.
        let steps = || {
            weighted(
                (0..9)
                    .map(|index| {
                        let step = if index % 2 == 0 {
                            sparse(index as f64, 30 + 2 * index, index % 3, 3)
                        } else {
                            leaf(index as f64, 30 + 2 * index)
                        };
                        (frac(index as i128 % 3 + 1, 2), step)
                    })
                    .collect(),
            )
        };
        let mut asked = ticks(130);
        asked.extend(straddles(60));
        asked.extend(spans());
        for (name, twin, union) in [
            ("sparse layers", map(layers(), stack), true),
            ("crossing layers", map(crossing(), stack), false),
            ("nine weighted steps", steps(), true),
            (
                "nine weighted steps, shifted",
                one(steps(), |p| p.late(frac(1, 3))),
                false,
            ),
        ] {
            let tables = twin.tabled.tables().expect("a table");
            assert!(tables.layers.len() >= INDEXED_LAYERS, "{name}: layers");
            let index = tables.layer_index().expect("an index");
            let narrow = TimeSpan::new(frac(1, 4), frac(1, 2));
            assert!(
                index
                    .meeting(narrow)
                    .is_some_and(|ids| ids.len() < tables.layers.len()),
                "{name}: a narrow query meets fewer layers"
            );
            assert_eq!(tables.union().is_some(), union, "{name}: union");
            assert_twins_over(name, &twin, &asked);
        }
    }

    #[test]
    fn far_queries_of_gaps_repeats_and_picks_are_refused_as_the_graph_refuses_them() {
        let mut twins = vec![
            (
                "fastGap 3/2".to_string(),
                one(seq(&[0.0, 1.0, 2.0]), |p| p.fast_gap(frac(3, 2))),
            ),
            (
                "fastGap 2/7".to_string(),
                one(seq(&[0.0, 1.0, 2.0]), |p| p.fast_gap(frac(2, 7))),
            ),
            (
                "repeatCycles 3/2".to_string(),
                one(alternating(), |p| p.repeat_cycles(frac(3, 2))),
            ),
        ];
        for mode in [JoinMode::Inner, JoinMode::Restart, JoinMode::Reset] {
            twins.push((
                format!("{mode:?} pick"),
                picked_from(cut_late_steps(), choices(), PickIndexMode::Clamp, mode),
            ));
        }
        let (mut refused, mut answered) = (0, 0);
        for (name, twin) in &twins {
            assert!(twin.tabled.tables().is_some(), "{name} has no table");
            for bits in [90, 100, 104, 106, 110, 112, 114, 118, 120, 122, 124] {
                for sign in [1i128, -1] {
                    for offset in [frac(0, 1), frac(1, 3), frac(1, 1 << 20)] {
                        let Some((begin, end)) = Fraction::int(sign << bits)
                            .checked_add(offset)
                            .and_then(|begin| Some((begin, begin.checked_add(frac(1, 7))?)))
                        else {
                            continue;
                        };
                        let span = TimeSpan::new(begin, end);
                        let graph = answer_in(&twin.graph, span, Order::Graph);
                        refused += usize::from(graph.is_err());
                        answered += usize::from(twin.tabled.answers_from_tables(span));
                        assert_eq!(
                            answer_in(&twin.tabled, span, Order::Graph),
                            graph,
                            "{name} over {begin} .. {end}"
                        );
                        assert_eq!(
                            answer_in(&twin.tabled, span, Order::Sorted),
                            answer_in(&twin.graph, span, Order::Sorted),
                            "{name} sorted over {begin} .. {end}"
                        );
                    }
                }
            }
        }
        assert!(refused > 0, "the graph refuses some far query");
        assert!(answered > 0, "the tables answer some far query");
    }

    /// A signal, tabled, and the same signal on the graph.
    fn signal_twin(signal: fn() -> Pattern) -> Twin {
        Twin {
            tabled: signal(),
            graph: signal().mark_volatile(),
        }
    }

    fn constant_twin(value: f64) -> Twin {
        Twin {
            tabled: pure(Value::F64(value)),
            graph: pure(Value::F64(value)),
        }
    }

    /// `range(low, high)`: times `high - low`, then plus `low`, both in place.
    fn ranged(twin: Twin, low: f64, high: f64) -> Twin {
        let scaled = composed(
            twin,
            constant_twin(high - low),
            ComposeOp::Mul,
            Alignment::In,
        );
        composed(scaled, constant_twin(low), ComposeOp::Add, Alignment::In)
    }

    fn controlled(structure: Twin, signal: Twin, name: &str) -> Twin {
        composed(
            structure,
            named(signal, name),
            ComposeOp::Set,
            Alignment::In,
        )
    }

    #[test]
    fn signals_under_constants_speed_changes_and_shifts_match_the_graph() {
        let notes = || named(seq(&[0.0, 1.0, 2.0]), "n");
        let sine = || signal_twin(crate::signal::sine);
        let cases = vec![
            ("range", ranged(sine(), 500.0, 1200.0)),
            (
                "range of a slow signal",
                ranged(one(sine(), |p| p.slow(frac(4, 1))), 500.0, 1200.0),
            ),
            (
                "a slow range",
                one(ranged(signal_twin(crate::signal::saw), 1.0, 2.0), |p| {
                    p.slow(frac(4, 1))
                }),
            ),
            (
                "an early signal times two",
                composed(
                    one(sine(), |p| p.early(frac(1, 4))),
                    constant_twin(2.0),
                    ComposeOp::Mul,
                    Alignment::In,
                ),
            ),
        ];
        for (name, signal) in cases {
            assert_twins(name, &controlled(notes(), signal, "lpf"));
        }
    }

    #[test]
    fn two_signals_and_constants_after_a_signal_match_the_graph() {
        let lpf = || {
            controlled(
                named(seq(&[0.0, 1.0]), "n"),
                signal_twin(crate::signal::sine),
                "lpf",
            )
        };
        let gain = || controlled(lpf(), signal_twin(crate::signal::saw), "gain");
        assert_twins("two signals", &gain());
        assert_twins(
            "a constant after two signals",
            &controlled(gain(), constant_twin(0.5), "room"),
        );
        assert_twins(
            "a constant after a signal",
            &controlled(lpf(), constant_twin(10.0), "lpq"),
        );
    }

    #[test]
    fn a_sampled_layer_moved_and_sped_up_matches_the_graph() {
        let met = || {
            controlled(
                named(seq(&[0.0, 1.0, 2.0]), "n"),
                signal_twin(crate::signal::sine),
                "lpf",
            )
        };
        assert_twins("late", &one(met(), |p| p.late(frac(1, 3))));
        assert_twins("fast", &one(met(), |p| p.fast(frac(3, 2))));
        assert_twins(
            "slow then late",
            &one(met(), |p| p.slow(frac(3, 1)).late(frac(1, 4))),
        );
        assert_twins(
            "a signal after a shift",
            &controlled(
                one(met(), |p| p.late(frac(1, 3))),
                signal_twin(crate::signal::saw),
                "gain",
            ),
        );
    }

    #[test]
    fn a_long_whole_meets_a_cut_signal_in_pieces_like_the_graph() {
        let long = named(one(leaf(1.0, 1), |p| p.slow(frac(4, 1))), "n");
        let cut = ranged(signal_twin(crate::signal::sine), 0.0, 10.0);
        assert_twins("a whole cut at each cycle", &controlled(long, cut, "lpf"));
    }

    #[test]
    fn voicings_expand_from_tables_like_the_graph() {
        let chords = |volatile: bool| {
            let chord = |name: &str, at: usize| {
                let pattern = pure(Value::Str(name.into()));
                let pattern = if volatile {
                    pattern.mark_volatile()
                } else {
                    pattern
                };
                pattern.with_added_context(vec![(at, at + 1)])
            };
            crate::voicings::voicing(&slowcat(vec![
                chord("Am7", 1),
                chord("G7", 3),
                fastcat(vec![chord("C", 5), chord("Dm", 7)]),
            ]))
        };
        assert_twins(
            "voicing",
            &Twin {
                tabled: chords(false),
                graph: chords(true),
            },
        );
    }

    #[test]
    fn a_restart_of_tabled_choices_by_a_graph_selector_matches_the_graph() {
        // A volatile selector keeps the pick on the graph. Its tabled
        // choices answer each selector hap over that hap's part only.
        let selector = || seq(&[0.0, 1.0, 2.0]).graph;
        let pick = |choices: Vec<Pattern>, mode| {
            let count = choices.len();
            crate::pick(
                selector(),
                PickLookup::Array {
                    enumerable_len: count,
                    length: count,
                    entries: choices.into_iter().enumerate().collect(),
                },
                PickIndexMode::Clamp,
                mode,
            )
        };
        for mode in [JoinMode::Restart, JoinMode::Reset] {
            let (tabled, graph): (Vec<_>, Vec<_>) = choices()
                .into_iter()
                .map(|twin| (twin.tabled, twin.graph))
                .unzip();
            assert!(tabled.iter().all(|choice| choice.tables().is_some()));
            let (mixed, walked) = (pick(tabled, mode), pick(graph, mode));
            for (begin, end) in spans() {
                let span = TimeSpan::new(begin, end);
                for order in [Order::Graph, Order::Sorted] {
                    assert_eq!(
                        answer_in(&mixed, span, order),
                        answer_in(&walked, span, order),
                        "{mode:?} over {begin} .. {end} in {order:?}"
                    );
                }
            }
        }
    }

    /// A plain value, tabled, and the same value on the graph.
    fn plain(value: f64) -> Twin {
        Twin {
            tabled: pure(Value::F64(value)),
            graph: pure(Value::F64(value)).mark_volatile(),
        }
    }

    /// `value` at the one step of `steps` that `index` names, rests elsewhere.
    fn lone(value: f64, index: usize, steps: usize) -> Twin {
        map(
            (0..steps)
                .map(|step| {
                    if step == index {
                        plain(value)
                    } else {
                        Twin {
                            tabled: silence(),
                            graph: silence(),
                        }
                    }
                })
                .collect(),
            fastcat,
        )
    }

    /// The outcome of the tabled twin over `span` is the graph's, a refusal
    /// included.
    fn assert_same_outcome(name: &str, twin: &Twin, span: TimeSpan) {
        for order in [Order::Graph, Order::Sorted] {
            assert_eq!(
                answer_in(&twin.tabled, span, order),
                answer_in(&twin.graph, span, order),
                "{name} over {} .. {} in {order:?}",
                span.begin,
                span.end
            );
        }
    }

    #[test]
    fn far_queries_that_ask_children_at_entry_times_are_refused_as_the_graph_refuses_them() {
        let far = TimeSpan::new(Fraction::int(1 << 67), Fraction::int((1 << 67) + 1));
        // The inner pattern is asked at times of the selector and the
        // structure, whose denominators no answer may hold.
        let selector = || one(plain(0.0), |p| p.late(frac(1, (1 << 40) + 1)));
        let nudged = || {
            one(lone(5.0, 1, 3), |p| {
                p.late(frac(1, (1 << 20) + 1)).early(frac(1, (1 << 20) + 1))
            })
        };
        for mode in [JoinMode::Inner, JoinMode::Mix, JoinMode::Outer] {
            let pick = picked_from(selector(), vec![nudged()], PickIndexMode::Clamp, mode);
            assert_same_outcome(&format!("{mode:?} pick"), &pick, far);
        }
        let met = map(vec![selector(), nudged()], |sides| {
            sides[0]
                .app_left_with_lookup(sides[1].clone(), |left, _| left.clone(), LookupFlow::Left)
                .filter_values(|_| false)
        });
        assert_same_outcome("a meeting that a filter empties", &met, far);
        // Coprime denominators, over many far begins.
        let prime = 1_048_583;
        let selector = || one(plain(0.0), |p| p.late(frac(1, (1 << 60) + 33)));
        let nudged = || {
            one(lone(5.0, 1, 3), |p| {
                p.late(frac(1, prime)).early(frac(1, prime))
            })
        };
        for mode in [JoinMode::Inner, JoinMode::Mix] {
            let pick = picked_from(selector(), vec![nudged()], PickIndexMode::Clamp, mode);
            for power in 47..=55 {
                let begin = Fraction::int(1 << power);
                let span = TimeSpan::new(begin, begin + Fraction::ONE);
                assert_same_outcome(&format!("{mode:?} pick at 2^{power}"), &pick, span);
            }
        }
    }

    #[test]
    fn a_reset_moves_its_choice_by_up_to_a_cycle_and_refuses_as_the_graph_does() {
        let selector = one(lone(0.0, 0, 8192), |p| p.early(frac(1, 1 << 20)));
        let pick = picked_from(
            selector,
            vec![lone(5.0, 0, 8191)],
            PickIndexMode::Clamp,
            JoinMode::Reset,
        );
        let span = TimeSpan::new(frac(1, (1 << 95) + 1), frac(2, (1 << 95) + 1));
        assert_same_outcome("reset", &pick, span);
    }

    #[test]
    fn a_gap_below_one_asks_its_child_near_the_query_and_refuses_as_the_graph_does() {
        // A node for the speed change, which names itself in the refusal.
        let gap = crate::sequence::unfolded(|| {
            one(plain(5.0), |p| {
                p.fast(frac(8191, 1)).fast_gap(frac(1, 8192))
            })
        });
        for tail in [62, 64] {
            let begin = Fraction::int(1 << 40) + frac(1, (1 << tail) + 1);
            let end = Fraction::int(1 << 40) + frac(2, (1 << tail) + 1);
            assert_same_outcome("fastGap below one", &gap, TimeSpan::new(begin, end));
        }
    }

    #[test]
    fn a_join_of_named_patterns_matches_the_graph() {
        // `fast("<1 2>")` and `ply`-like patterned arguments join the patterns
        // a pure construction names for each carrier value.
        let speeds = map(vec![leaf(1.0, 70), leaf(2.0, 72), leaf(3.0, 74)], slowcat);
        let carried = |carrier: Pattern, body: Pattern| {
            carrier.fmap_to_pure_pattern(move |value| {
                let factor = crate::util::parse_numeral(value).unwrap_or(1.0);
                body.as_pure_pattern()
                    .expect("a pure body")
                    .fast(Fraction::from_f64(factor).expect("a factor"))
            })
        };
        for mode in [JoinMode::Inner, JoinMode::Outer, JoinMode::Mix] {
            let body = seq(&[0.0, 1.0]);
            let twin = Twin {
                tabled: Pattern::of(Node::Join(
                    carried(speeds.tabled.clone(), body.tabled),
                    mode,
                )),
                graph: Pattern::of(Node::Join(carried(speeds.graph.clone(), body.graph), mode)),
            };
            assert_twins(&format!("{mode:?} join"), &twin);
        }
    }

    /// Notes met by a signal: a sampled layer.
    fn sampled_notes(values: &[f64]) -> Twin {
        controlled(
            named(seq(values), "n"),
            signal_twin(crate::signal::sine),
            "lpf",
        )
    }

    #[test]
    fn sampled_layers_under_per_cycle_nodes_match_the_graph_in_every_period() {
        assert_twins("rev", &one(sampled_notes(&[0.0, 1.0, 2.0]), |p| p.rev()));
        assert_twins(
            "slowcat",
            &map(
                vec![
                    sampled_notes(&[0.0, 1.0]),
                    seq(&[5.0]),
                    sampled_notes(&[7.0]),
                ],
                slowcat,
            ),
        );
        assert_twins(
            "fastGap",
            &one(sampled_notes(&[0.0, 1.0]), |p| p.fast_gap(frac(3, 2))),
        );
        assert_twins(
            "fastGap below one",
            &one(sampled_notes(&[0.0, 1.0]), |p| p.fast_gap(frac(2, 3))),
        );
        assert_twins(
            "repeatCycles",
            &one(
                controlled(
                    named(map(vec![seq(&[0.0, 1.0]), seq(&[2.0])], slowcat), "n"),
                    rand_twin(),
                    "gain",
                ),
                |p| p.repeat_cycles(frac(3, 2)),
            ),
        );
        assert_twins(
            "rev then slow then a signal",
            &controlled(
                one(sampled_notes(&[0.0, 1.0]), |p| p.rev().slow(frac(3, 1))),
                signal_twin(crate::signal::saw),
                "gain",
            ),
        );
    }

    #[test]
    fn picks_of_sampled_choices_match_the_graph_in_every_period() {
        let choices = || {
            vec![
                sampled_notes(&[0.0, 1.0]),
                seq(&[3.0]),
                sampled_notes(&[4.0, 5.0, 6.0]),
            ]
        };
        for mode in [JoinMode::Restart, JoinMode::Reset, JoinMode::Inner] {
            // A selector hap that cuts a sampled hap's whole leaves the pick
            // to the graph. Every pick answers as the graph does.
            for (name, selector) in selectors() {
                let pick = picked_from(selector, choices(), PickIndexMode::Clamp, mode);
                for (begin, end) in spans() {
                    assert_same_outcome(
                        &format!("{mode:?} pick of sampled choices by {name}"),
                        &pick,
                        TimeSpan::new(begin, end),
                    );
                }
            }
            // Selector haps of whole cycles hold whole notes, so the pick
            // answers from tables.
            let cycles = map(vec![leaf(0.0, 80), leaf(2.0, 82), leaf(1.0, 84)], slowcat);
            assert_twins(
                &format!("{mode:?} pick of sampled choices by cycles"),
                &picked_from(cycles, choices(), PickIndexMode::Clamp, mode),
            );
        }
    }

    /// A signal held over `steps` segments a cycle, as `segment` holds it.
    fn segmented(signal: Twin, steps: i128) -> Twin {
        one(signal, |p| crate::combinators::segment(p, frac(steps, 1)))
    }

    #[test]
    fn meetings_with_sampled_or_patterned_sides_match_the_graph() {
        let notes = || named(seq(&[0.0, 1.0, 2.0, 3.0]), "note");
        assert_twins(
            "a segmented signal as a control",
            &controlled(
                notes(),
                ranged(
                    segmented(signal_twin(crate::signal::sine), 6),
                    200.0,
                    8000.0,
                ),
                "lpf",
            ),
        );
        assert_twins(
            "a segmented random signal under a constant",
            &controlled(
                controlled(notes(), segmented(rand_twin(), 8), "gain"),
                constant_twin(0.5),
                "room",
            ),
        );
        assert_twins(
            "a patterned control after a signal",
            &controlled(
                sampled_notes(&[0.0, 1.0]),
                named(seq(&[0.5, 1.0, 0.25]), "gain"),
                "gain",
            ),
        );
        // A long whole cut into pieces that `rev` gives back in falling time.
        let long = named(map(vec![leaf(0.0, 90), leaf(4.0, 92)], slowcat), "note");
        let long = one(long, |p| p.slow(frac(2, 1)));
        assert_twins(
            "pieces of a segmented signal under rev",
            &one(
                controlled(long, segmented(signal_twin(crate::signal::sine), 3), "gain"),
                |p| p.rev(),
            ),
        );
    }

    #[test]
    fn a_sampled_choice_meets_its_selector_after_its_own_meetings() {
        // A meeting puts the other side's locations first, so a located
        // control after a signal comes before the choice's own locations, and
        // the selector's come before both.
        let located = || {
            controlled(
                sampled_notes(&[0.0, 1.0]),
                named(leaf(0.5, 60), "room"),
                "room",
            )
        };
        let cycles = || map(vec![leaf(0.0, 80), leaf(1.0, 84)], slowcat);
        for mode in [JoinMode::Restart, JoinMode::Reset, JoinMode::Inner] {
            assert_twins(
                &format!("{mode:?} pick of a located sampled choice"),
                &picked_from(
                    cycles(),
                    vec![located(), seq(&[3.0])],
                    PickIndexMode::Clamp,
                    mode,
                ),
            );
        }
    }

    /// A constant at `location`, which every meeting with it carries.
    fn located_constant(value: f64, location: usize) -> Twin {
        let located = || pure(Value::F64(value)).with_added_context(vec![(location, location + 1)]);
        Twin {
            tabled: located(),
            graph: located(),
        }
    }

    /// Controls set from located constants after a signal, one key twice.
    fn set_in_a_row(structure: Twin) -> Twin {
        let set =
            |twin, value, location, name| controlled(twin, located_constant(value, location), name);
        set(
            set(set(structure, 10.0, 30, "lpq"), 0.2, 32, "decay"),
            0.5,
            34,
            "lpq",
        )
    }

    #[test]
    fn constants_set_in_a_row_after_a_signal_match_the_graph() {
        let row = || set_in_a_row(sampled_notes(&[0.0, 1.0, 2.0]));
        assert_twins("in a row", &row());
        assert_twins("then late", &one(row(), |p| p.late(frac(1, 3))));
        assert_twins("then fast", &one(row(), |p| p.fast(frac(3, 2))));
        assert_twins("then rev", &one(row(), |p| p.rev()));
        assert_twins(
            "then a signal and a constant",
            &controlled(
                controlled(row(), signal_twin(crate::signal::saw), "gain"),
                constant_twin(0.3),
                "room",
            ),
        );
        let long = controlled(
            named(one(leaf(1.0, 1), |p| p.slow(frac(5, 2))), "n"),
            signal_twin(crate::signal::sine),
            "lpf",
        );
        assert_twins("a whole across cycles", &set_in_a_row(long));
        let cycles = || map(vec![leaf(0.0, 80), leaf(1.0, 84)], slowcat);
        for mode in [JoinMode::Restart, JoinMode::Reset, JoinMode::Inner] {
            assert_twins(
                &format!("{mode:?} pick of constants in a row"),
                &picked_from(
                    cycles(),
                    vec![row(), seq(&[3.0])],
                    PickIndexMode::Clamp,
                    mode,
                ),
            );
        }
    }

    #[test]
    fn constants_that_make_lookups_or_keep_the_value_match_the_graph() {
        // A string meets a control with a lookup of its characters, which
        // each later meeting carries on.
        let word = |text: &str, location: usize| {
            let word =
                || pure(Value::Str(text.into())).with_added_context(vec![(location, location + 1)]);
            Twin {
                tabled: word(),
                graph: word().mark_volatile(),
            }
        };
        let words = map(vec![word("bd", 1), word("sd", 3)], fastcat);
        assert_twins(
            "a string structure",
            &set_in_a_row(controlled(words, signal_twin(crate::signal::sine), "lpf")),
        );
        // A control of only `value` leaves the structure's value as it is.
        let only_value = Twin {
            tabled: pure(Value::object([("value".into(), Value::F64(3.0))])),
            graph: pure(Value::object([("value".into(), Value::F64(3.0))])),
        };
        let kept = composed(
            set_in_a_row(sampled_notes(&[0.0, 1.0])),
            only_value,
            ComposeOp::Set,
            Alignment::In,
        );
        assert_twins("a value-only control", &set_in_a_row(kept));
        assert_twins(
            "a constant on the structure side",
            &composed(
                named(located_constant(0.5, 40), "room"),
                sampled_notes(&[0.0, 1.0, 2.0]),
                ComposeOp::Set,
                Alignment::Out,
            ),
        );
        assert_twins(
            "a sum after a signal",
            &composed(
                sampled_notes(&[0.0, 1.0]),
                named(located_constant(2.0, 44), "n"),
                ComposeOp::Add,
                Alignment::In,
            ),
        );
    }

    #[test]
    fn plain_entries_of_a_sampled_layer_match_the_graph() {
        let mixed = || {
            map(
                vec![
                    sampled_notes(&[0.0, 1.0]),
                    named(seq(&[5.0, 6.0, 7.0]), "s"),
                ],
                stack,
            )
        };
        assert_twins(
            "a constant",
            &controlled(mixed(), located_constant(0.5, 50), "room"),
        );
        assert_twins("a step", &one(mixed(), |p| p.fmap(Value::clone)));
        assert_twins(
            "a patterned side",
            &composed(
                mixed(),
                named(seq(&[0.1, 0.2, 0.3]), "gain"),
                ComposeOp::Set,
                Alignment::In,
            ),
        );
        let long = map(
            vec![
                sampled_notes(&[0.0]),
                named(one(leaf(4.0, 9), |p| p.slow(frac(3, 2))), "s"),
            ],
            stack,
        );
        assert_twins(
            "a long plain whole cut by a constant",
            &controlled(long, located_constant(0.5, 50), "room"),
        );
    }

    /// The stages of every sampler in `twin`'s tables.
    fn stages(twin: &Twin) -> Vec<Vec<&'static str>> {
        let tables = twin.tabled.tables().expect("tables");
        let mut out = Vec::new();
        for layer in &tables.layers {
            let Some(sampled) = &layer.sampled else {
                continue;
            };
            for sampler in &sampled.samplers {
                out.push(
                    sampler
                        .stages
                        .iter()
                        .map(|stage| match stage {
                            Stage::Meet { .. } => "meet",
                            Stage::Fused { .. } => "fused",
                            Stage::Step(_) => "step",
                            Stage::Outer(_) => "outer",
                        })
                        .collect(),
                );
            }
        }
        out
    }

    #[test]
    fn constants_set_in_a_row_meet_once_and_plain_entries_meet_nothing() {
        let row = set_in_a_row(sampled_notes(&[0.0, 1.0, 2.0]));
        assert_eq!(stages(&row), vec![vec!["meet", "fused"]]);
        let mixed = map(
            vec![sampled_notes(&[0.0, 1.0]), named(seq(&[5.0, 6.0]), "s")],
            stack,
        );
        let stepped = controlled(
            one(mixed, |p| p.fmap(Value::clone)),
            constant_twin(0.5),
            "room",
        );
        // The plain layer has no sampler at all, and the sampled one meets
        // its signal, then the constant, with the step between.
        assert_eq!(stages(&stepped), vec![vec!["meet", "step", "meet"]]);
    }
}
