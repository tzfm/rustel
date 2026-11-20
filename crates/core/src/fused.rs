//! Stateless walks of a signal read in equal steps of a cycle, as `segment`
//! gives. They keep nothing between queries: each one reads the grid of
//! steps that a span meets, and answers what the graph answers for it.
//!
//! - [`Segment`] answers an `appRight` of a signal and a pulse.
//! - [`StepWave`] answers the control of a segment of a wave for a function
//!   hap, as one number for each step, with no hap of the side.
//! - [`Wave`] is a signal of numbers read with no hap of its own.
//!
//! Each walk asks [`StepGrid`] for its steps, so the window, the limits,
//! the times of the steps and the cuts at the ends of a span have one
//! owner. A walk that cannot answer a span says so with `None`, and the
//! graph answers it.

use crate::table::{HapBuffer, Signal, ValueSignal, in_one_cycle, signal_of};
use crate::{
    Combine, Hap, LookupFlow, Node, Pattern, SpanUse, State, TimeSpan, Value, applied_hap,
};
use rustel_fraction::Fraction;

#[cfg(test)]
thread_local! {
    /// Set by a test to read no segment as a signal, so that the graph
    /// answers it.
    static UNFUSED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// What the fused walks answered on this thread. See [`counting`].
    static HITS: std::cell::Cell<Hits> = const { std::cell::Cell::new(Hits::NONE) };
}

/// The fused answers of a thread, counted for a test.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Hits {
    /// Spans that [`Segment`] answered.
    pub(crate) segment: u32,
    /// Function haps that took one number of a [`StepWave`].
    pub(crate) step_one: u32,
    /// Function haps that took the numbers of several steps.
    pub(crate) step_many: u32,
}

#[cfg(test)]
impl Hits {
    const NONE: Self = Self {
        segment: 0,
        step_one: 0,
        step_many: 0,
    };
}

/// Which fused answer was made.
#[derive(Clone, Copy)]
pub(crate) enum Hit {
    Segment,
    StepOne,
    StepMany,
}

/// Count `hit` for a test. Nothing in a build that is no test.
#[inline]
pub(crate) fn note(hit: Hit) {
    #[cfg(test)]
    HITS.with(|hits| {
        let mut counted = hits.get();
        match hit {
            Hit::Segment => counted.segment += 1,
            Hit::StepOne => counted.step_one += 1,
            Hit::StepMany => counted.step_many += 1,
        }
        hits.set(counted);
    });
    #[cfg(not(test))]
    let _ = hit;
}

/// Run `f` with each segment answered by the graph that it stands for.
#[cfg(test)]
pub(crate) fn unfused<R>(f: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            UNFUSED.set(self.0);
        }
    }
    let _restore = Restore(UNFUSED.replace(true));
    f()
}

/// Run `f`, and count what the fused walks answered in it.
#[cfg(test)]
pub(crate) fn counting<R>(f: impl FnOnce() -> R) -> (R, Hits) {
    let before = HITS.replace(Hits::NONE);
    let made = f();
    (made, HITS.replace(before))
}

/// Whether a test asks the graph to answer each segment.
#[inline]
pub(crate) fn is_unfused() -> bool {
    #[cfg(test)]
    return UNFUSED.get();
    #[cfg(not(test))]
    false
}

/// The steps of a cycle that a span meets: the integer indexes `first` up
/// to `after`, with their limits. The end of step `k` is the begin of step
/// `k + 1`, so a walk makes each time once.
#[derive(Clone, Copy)]
pub(crate) struct StepGrid {
    count: i128,
    first: i128,
    after: i128,
}

impl StepGrid {
    /// The most bits of an index of a step. Times of that many bits over
    /// a count of at most 32 bits stay below the 53 bits of an `f64`.
    const INDEX_BITS: u32 = 48;

    /// The steps of `count` a cycle that `span` meets. A span of no width
    /// meets the one step it lies in. `None` for a span that the graph
    /// refuses, that its hap budget refuses, or whose indexes are not plain:
    /// the graph answers those.
    pub(crate) fn over(count: i128, span: TimeSpan) -> Option<Self> {
        if span.begin > span.end || count < 1 {
            return None;
        }
        let (first, after) = if let (Ok(c), Ok(bn), Ok(bd), Ok(en), Ok(ed)) = (
            i64::try_from(count),
            i64::try_from(span.begin.numer()),
            i64::try_from(span.begin.denom()),
            i64::try_from(span.end.numer()),
            i64::try_from(span.end.denom()),
        ) && let Some(b_scaled) = bn.checked_mul(c)
            && let Some(e_scaled) = en.checked_mul(c)
            && let Some(neg_e) = e_scaled.checked_neg()
        {
            let first = i128::from(b_scaled.div_euclid(bd));
            let after = if span.begin == span.end {
                first.checked_add(1)?
            } else {
                -i128::from(neg_e.div_euclid(ed))
            };
            (first, after)
        } else {
            let scale = Fraction::int(count);
            let first = span.begin.checked_mul(scale)?.floor().numer();
            let after = if span.begin == span.end {
                first.checked_add(1)?
            } else {
                span.end.checked_mul(scale)?.ceil().numer()
            };
            (first, after)
        };
        let reached = after.checked_sub(first)?;
        let plain = |index: i128| index.unsigned_abs() >> Self::INDEX_BITS == 0;
        if !(1..=crate::MAX_QUERY_SPAN_CYCLES).contains(&reached)
            || crate::hap_budget().is_some_and(|budget| reached > i128::from(budget))
            || !plain(first)
            || !plain(after)
        {
            return None;
        }
        Some(Self {
            count,
            first,
            after,
        })
    }

    /// The count of steps.
    pub(crate) fn len(&self) -> usize {
        (self.after - self.first) as usize
    }

    /// The indexes of the steps, in order.
    pub(crate) fn steps(&self) -> std::ops::Range<i128> {
        self.first..self.after
    }

    /// The time of the boundary `k`. A count that is a power of two
    /// reduces by a shift, with no gcd.
    pub(crate) fn time(&self, k: i128) -> Option<Fraction> {
        let count = self.count;
        match count & (count - 1) == 0 {
            true => Fraction::dyadic(k, count.trailing_zeros()),
            false => Fraction::checked_new(k, count),
        }
    }

    /// `whole`, the step `k`, as far as `span` reaches. Only the first and
    /// the last step can be cut by the span.
    pub(crate) fn cut(&self, k: i128, whole: TimeSpan, span: TimeSpan) -> TimeSpan {
        match k == self.first || k + 1 == self.after {
            true => TimeSpan::new(whole.begin.max(span.begin), whole.end.min(span.end)),
            false => whole,
        }
    }
}

/// The haps of an `appRight` that a fused walk answers, into `out`. `false`
/// leaves `out` as it is, for an application that the graph answers.
pub(crate) fn app_right_onto(node: &crate::AppNode, state: &State, out: &mut Vec<Hap>) -> bool {
    let crate::AppNode(functions, values, apply, flow) = node;
    let Some(segment) = Segment::of(functions, values, apply, *flow) else {
        return false;
    };
    segment
        .answer(state.span, &mut state.clone(), out)
        .is_some()
}

/// A signal read in `count` equal steps of a cycle, as `segment` gives: an
/// `appRight` of the signal and a pulse of equal steps.
pub(crate) struct Segment {
    signal: Signal,
    count: i128,
    /// The value of each step of the pulse.
    pulse: Value,
    apply: Combine,
    flow: LookupFlow,
}

impl Segment {
    /// The segment that `functions` and `values` make, when they are the
    /// signal and the pulse of one.
    pub(crate) fn of(
        functions: &Pattern,
        values: &Pattern,
        apply: &Combine,
        flow: LookupFlow,
    ) -> Option<Self> {
        if is_unfused() {
            return None;
        }
        let Node::Sequence(layers) = values.node_kind() else {
            return None;
        };
        // A graph of discrete haps is asked over a part, not its whole.
        if values.has_runtime_settings() || values.is_volatile() || functions.is_discrete() {
            return None;
        }
        let (count, pulse) = crate::sequence::pulse(layers)?;
        Some(Self {
            signal: signal_of(functions)?,
            count,
            pulse: pulse.clone(),
            apply: apply.clone(),
            flow,
        })
    }

    /// The haps of the segment over `span` into `out`, in the order of its
    /// graph: the signal read over each step the span meets. `None` leaves
    /// `out` as it is, for a span that only the graph answers.
    ///
    /// Each hap is charged to the hap budget and checked against the query
    /// stop as the graph path of the application does, so a signal that
    /// makes many haps for a step stops at the same hap. Nothing is charged
    /// before a walk that falls back to the graph.
    pub(crate) fn answer(
        &self,
        span: TimeSpan,
        probe: &mut State,
        out: &mut Vec<Hap>,
    ) -> Option<()> {
        let grid = StepGrid::over(self.count, span)?;
        note(Hit::Segment);
        let mut start = grid.time(grid.steps().start)?;
        let from = out.len();
        out.reserve(grid.len());
        let mut funcs = HapBuffer::take();
        for k in grid.steps() {
            let Some(stop) = grid.time(k + 1) else {
                out.truncate(from);
                return None;
            };
            let whole = TimeSpan::new(start, stop);
            let part = grid.cut(k, whole, span);
            start = stop;
            let value = Hap::new(Some(whole), part, self.pulse.clone());
            if self.signal.answer(whole, part, probe, &mut funcs).is_none() {
                out.truncate(from);
                return None;
            }
            for function in funcs.iter() {
                let Some(met) = function.part.intersection(&part) else {
                    continue;
                };
                let hap = applied_hap(function, &value, value.whole, met, &self.apply, self.flow);
                if crate::query_stop_requested() || !crate::push_budgeted_from(out, from, hap) {
                    out.truncate(from);
                    return Some(());
                }
            }
        }
        Some(())
    }
}

/// The control of a segment of a wave, as `.gain(sine.segment(64))` gives:
/// a function hap takes the number of each step that its part meets, with
/// no hap of the side. Made for each query that meets it, so that a score
/// holds nothing of it between queries.
pub(crate) struct StepWave {
    pub(crate) key: crate::Key,
    wave: Wave,
    /// The steps of a cycle.
    steps: u32,
}

impl StepWave {
    /// `values` as a control of a segment of a wave, independent of the
    /// enclosing `AppLeft`'s combine/flow (cached once per `ValueReading`).
    pub(crate) fn of_pattern(values: &Pattern) -> Option<Self> {
        let inner = match values.node_kind() {
            Node::ControlMap(cell, flags, _, false) => crate::NodeRef::Plain(cell, *flags),
            Node::ControlMapBoxed(boxed) if !boxed.2 => crate::NodeRef::Pat(&boxed.0),
            Node::MapHapsOwned(boxed) if boxed.2 == SpanUse::Ignores => {
                crate::NodeRef::Pat(&boxed.0)
            }
            _ => return None,
        };
        if !Wave::holds_segment_ref(inner) {
            return None;
        }
        let (inner, key) = ValueSignal::named_key(values)?;
        let (wave, steps) = Wave::of_steps_ref(inner)?;
        Some(Self { key, wave, steps })
    }

    #[cfg(test)]
    pub(crate) fn of(values: &Pattern, apply: &Combine, flow: LookupFlow) -> Option<Self> {
        let sets = matches!(apply, Combine::Composed(crate::compose::ComposeOp::Set))
            && flow == LookupFlow::Set;
        if !sets || is_unfused() {
            return None;
        }
        Self::of_pattern(values)
    }

    /// Whether this control answers for `hf` in place: a hap of data
    /// controls with no lookup, whose part lies in its whole and has width.
    /// Asked of every function hap before the control answers any, so that
    /// a side that it cannot answer is queried once for all of them.
    pub(crate) fn takes(hf: &Hap) -> bool {
        hf.pick_lookup().is_none()
            && matches!(hf.value, Value::Object(_))
            && hf.value.is_data(0)
            && hf.part.begin < hf.part.end
            && hf.part.intersection(&hf.whole_or_part()) == Some(hf.part)
    }

    /// Whether `span` lies entirely inside a single step of `self.steps`, so
    /// every hap within `span` is guaranteed to take one step with no split.
    #[inline]
    pub(crate) fn covers_span(&self, span: TimeSpan) -> bool {
        if span.begin >= span.end {
            return false;
        }
        let steps = i64::from(self.steps);
        if let (Ok(bn), Ok(bd), Ok(en), Ok(ed)) = (
            i64::try_from(span.begin.numer()),
            i64::try_from(span.begin.denom()),
            i64::try_from(span.end.numer()),
            i64::try_from(span.end.denom()),
        ) && let Some(b_scaled) = bn.checked_mul(steps)
            && let Some(e_scaled) = en.checked_mul(steps)
        {
            let k = b_scaled.div_euclid(bd);
            return k
                .checked_add(1)
                .and_then(|next| next.checked_mul(ed))
                .is_some_and(|bound| e_scaled <= bound);
        }
        false
    }

    /// The number that the graph answers for a function hap cut to `part`,
    /// when `part` lies in one step. The wave is asked over that step.
    #[inline]
    pub(crate) fn number(&self, part: TimeSpan, controls: &crate::OrderedMap) -> Option<f64> {
        if part.begin >= part.end {
            return None;
        }
        let steps = i64::from(self.steps);
        // Fast 64-bit check: `part` lies inside one step `[k/steps, (k+1)/steps]`.
        if let (Ok(bn), Ok(bd), Ok(en), Ok(ed)) = (
            i64::try_from(part.begin.numer()),
            i64::try_from(part.begin.denom()),
            i64::try_from(part.end.numer()),
            i64::try_from(part.end.denom()),
        ) && let Some(b_scaled) = bn.checked_mul(steps)
            && let Some(e_scaled) = en.checked_mul(steps)
        {
            let k = b_scaled.div_euclid(bd);
            if let Some(bound) = k.checked_add(1).and_then(|next| next.checked_mul(ed))
                && e_scaled <= bound
                && ((bn.unsigned_abs()
                    | bd as u64
                    | en.unsigned_abs()
                    | ed as u64
                    | k.unsigned_abs()
                    | k.saturating_add(1).unsigned_abs())
                    >> 48)
                    == 0
                && let Some(d) = std::num::NonZeroU32::new(self.steps)
            {
                let begin = Fraction::from_ticks(i128::from(k), d);
                return self.wave.value_at(begin, controls);
            }
            return None;
        }
        let grid = StepGrid::over(i128::from(self.steps), part)?;
        if grid.len() != 1 {
            return None;
        }
        let first = grid.steps().start;
        let step = TimeSpan::new(grid.time(first)?, grid.time(first + 1)?);
        self.wave.number(step, part, controls)
    }

    /// Walk the steps of this wave that `part` meets, calling `each` with
    /// `(step_part, value)` in order, with no intermediate allocation.
    /// Returns `None` for a part of no width, a span the graph refuses or
    /// whose budget it refuses, or a step whose number only the graph gives.
    pub(crate) fn grid_over(&self, part: TimeSpan) -> Option<StepGrid> {
        if part.begin >= part.end {
            return None;
        }
        let grid = StepGrid::over(i128::from(self.steps), part)?;
        for time in [part.begin, part.end] {
            Wave::plain(time, 48)?;
        }
        Some(grid)
    }

    #[inline]
    pub(crate) fn value_at(&self, time: Fraction, controls: &crate::OrderedMap) -> Option<f64> {
        self.wave.value_at(time, controls)
    }
}

/// Compact storage for arithmetic ops on a wave: identity, single op, or
/// `.range(lo, hi)` (`mul` then `add`) stay inline with zero heap allocation,
/// while deeper chains (such as `.rangex`) spill to a boxed slice.
#[derive(Clone)]
enum WaveMath {
    None,
    One(crate::compose::ComposeOp, f64),
    MulAdd { mul: f64, add: f64 },
    Boxed(Box<[(crate::compose::ComposeOp, f64)]>),
}

impl WaveMath {
    fn from_ops(ops: &[(crate::compose::ComposeOp, f64)]) -> Self {
        use crate::compose::ComposeOp;
        match ops {
            [] => Self::None,
            [(op, value)] => Self::One(*op, *value),
            [(ComposeOp::Mul, mul), (ComposeOp::Add, add)] => Self::MulAdd {
                mul: *mul,
                add: *add,
            },
            many => Self::Boxed(many.into()),
        }
    }

    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::None, Self::None) => true,
            (Self::One(ao, av), Self::One(bo, bv)) => ao == bo && av.to_bits() == bv.to_bits(),
            (Self::MulAdd { mul: am, add: aa }, Self::MulAdd { mul: bm, add: ba }) => {
                am.to_bits() == bm.to_bits() && aa.to_bits() == ba.to_bits()
            }
            (Self::Boxed(a), Self::Boxed(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .zip(b.iter())
                        .all(|((ao, av), (bo, bv))| ao == bo && av.to_bits() == bv.to_bits())
            }
            _ => false,
        }
    }

    fn apply_to_pattern(&self, pattern: Pattern) -> Pattern {
        let apply_ops = |pattern: Pattern, ops: &[(crate::compose::ComposeOp, f64)]| {
            pattern.met_numbers(ops).unwrap_or_else(|| {
                let mut current = pattern;
                for &(op, number) in ops {
                    current = current.app_left_by(
                        crate::pure(Value::F64(number)),
                        Combine::Composed(op),
                        op.lookup_flow(),
                    );
                }
                current
            })
        };
        match self {
            Self::None => pattern,
            Self::One(op, value) => apply_ops(pattern, &[(*op, *value)]),
            Self::MulAdd { mul, add } => apply_ops(
                pattern,
                &[
                    (crate::compose::ComposeOp::Mul, *mul),
                    (crate::compose::ComposeOp::Add, *add),
                ],
            ),
            Self::Boxed(ops) => apply_ops(pattern, ops),
        }
    }

    #[inline]
    fn apply(&self, mut number: f64) -> Option<f64> {
        match self {
            Self::None => Some(number),
            Self::One(op, other) => op.of_numbers(number, *other),
            Self::MulAdd { mul, add } => Some(number * *mul + *add),
            Self::Boxed(ops) => {
                for (op, other) in ops.iter() {
                    number = op.of_numbers(number, *other)?;
                }
                Some(number)
            }
        }
    }
}

/// Compact storage for up to [`Wave::MOST_TIMES`] time changes and their
/// cycle-cutting bitmask: 0 or 1 time change stays inline in 16 bytes.
#[derive(Clone)]
enum WaveTimes {
    None,
    One(WaveTime, u8),
    Many(Box<([WaveTime; Wave::MOST_TIMES], u8, u8)>),
}

impl WaveTimes {
    fn from_raw(times: [WaveTime; Wave::MOST_TIMES], len: u8, cut_mask: u8) -> Self {
        match len {
            0 => Self::None,
            1 => Self::One(times[0], cut_mask),
            _ => Self::Many(Box::new((times, len, cut_mask))),
        }
    }

    fn same(&self, other: &Self) -> bool {
        self.parts() == other.parts()
    }

    #[inline]
    fn parts(&self) -> (&[WaveTime], u8) {
        match self {
            Self::None => (&[], 0),
            Self::One(time, cut_mask) => (std::slice::from_ref(time), *cut_mask),
            Self::Many(boxed) => (&boxed.0[..usize::from(boxed.1)], boxed.2),
        }
    }
}

/// A signal of numbers, as `sine.range(200, 2000).slow(4)` is: a
/// [`signal_static`](crate::signal_static) under speed changes, shifts and
/// arithmetic with numbers. It gives the number that its graph answers for
/// a hap, with no hap of its own.
///
/// The graph answers one hap for the span that a function hap asks over,
/// and each number of the arithmetic cuts that hap at the cycles of its own
/// time. A wave answers for a part that meets one of those haps: a part in
/// one cycle of the time of each number. The time of a number is the time
/// of the query, or a time that keeps a cycle of the query in one cycle,
/// for most waves. Any other number is under changes of time that
/// `cut_mask` records, and each part is read in its time.
#[derive(Clone)]
pub(crate) struct Wave {
    /// The arithmetic, the innermost first: each op with its number.
    math: WaveMath,
    /// The changes of time, outermost first, plus the cycle-cutting bitmask.
    times: WaveTimes,
    body: crate::NumberSignal,
}

const _: () = assert!(std::mem::size_of::<Wave>() <= 48);

/// One change of time over a [`Wave`]. Factors and shifts are bounded below
/// 2^16 by [`Wave::plain`], so 32-bit components are lossless and compact.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WaveTime {
    /// `fast(factor)`: what lies inside is asked at `t * (n / d)`.
    Fast(i32, std::num::NonZeroU32),
    /// A shift by `by`: what lies inside is asked at `t - (n / d)`.
    Late(i32, std::num::NonZeroU32),
}

impl WaveTime {
    const ONE: std::num::NonZeroU32 = match std::num::NonZeroU32::new(1) {
        Some(v) => v,
        None => unreachable!(),
    };
    const IDENTITY: Self = Self::Late(0, Self::ONE);

    fn fast(factor: Fraction) -> Option<Self> {
        let f = Wave::plain(factor, 16)?;
        Some(Self::Fast(
            i32::try_from(f.numer()).ok()?,
            std::num::NonZeroU32::new(u32::try_from(f.denom()).ok()?)?,
        ))
    }

    fn late(by: Fraction) -> Option<Self> {
        let f = Wave::plain(by, 16)?;
        Some(Self::Late(
            i32::try_from(f.numer()).ok()?,
            std::num::NonZeroU32::new(u32::try_from(f.denom()).ok()?)?,
        ))
    }

    /// `time` as what lies inside is asked.
    #[inline]
    fn inward(&self, time: Fraction) -> Option<Fraction> {
        match *self {
            Self::Fast(n, d) => {
                let factor = Fraction::from_ticks(i128::from(n), d);
                time.checked_mul(factor)
            }
            Self::Late(n, d) => {
                if n == 0 {
                    return Some(time);
                }
                if d.get() == 1 {
                    return time.checked_add_int(-i128::from(n));
                }
                let by = Fraction::from_ticks(i128::from(n), d);
                time.checked_sub(by)
            }
        }
    }
}

impl Wave {
    /// The most changes of time over a wave. With [`Self::plain`] times
    /// and factors, no time of a wave with so many leaves the native range.
    const MOST_TIMES: usize = 3;

    /// `pattern` as a wave, when it is one.
    pub(crate) fn of(pattern: &Pattern) -> Option<Self> {
        Self::of_ref(crate::NodeRef::Pat(pattern))
    }

    /// `pattern` as a wave, when it is one.
    pub(crate) fn of_ref(pattern: crate::NodeRef<'_>) -> Option<Self> {
        Self::read_in(pattern, None, false).map(|(wave, _)| wave)
    }

    /// `pattern` as a self-contained inline wave with no bound runtime settings
    /// and a lossless `to_control_pattern` reconstruction (`math_inside` records
    /// whether all arithmetic sits inside or outside the time changes).
    pub(crate) fn of_inline_ref(pattern: crate::NodeRef<'_>) -> Option<(Self, bool)> {
        let (wave, math_inside) = Self::read_in(pattern, None, true)?;
        let (_, cut_mask) = wave.times.parts();
        if cut_mask != 0 {
            return None;
        }
        Some((wave, math_inside?))
    }

    pub(crate) fn same(&self, other: &Self) -> bool {
        std::ptr::fn_addr_eq(self.body, other.body)
            && self.math.same(&other.math)
            && self.times.same(&other.times)
    }

    pub(crate) fn hash_intern(&self, state: &mut impl std::hash::Hasher) {
        use std::hash::Hash;
        (self.body as usize).hash(state);
        match &self.math {
            WaveMath::None => 0u8.hash(state),
            WaveMath::One(op, v) => {
                1u8.hash(state);
                (*op as u8).hash(state);
                v.to_bits().hash(state);
            }
            WaveMath::MulAdd { mul, add } => {
                2u8.hash(state);
                mul.to_bits().hash(state);
                add.to_bits().hash(state);
            }
            WaveMath::Boxed(ops) => {
                3u8.hash(state);
                ops.len().hash(state);
                for (op, v) in ops.iter() {
                    (*op as u8).hash(state);
                    v.to_bits().hash(state);
                }
            }
        }
        let (times, cut_mask) = self.times.parts();
        cut_mask.hash(state);
        times.len().hash(state);
        for t in times {
            match *t {
                WaveTime::Fast(n, d) => {
                    0u8.hash(state);
                    n.hash(state);
                    d.get().hash(state);
                }
                WaveTime::Late(n, d) => {
                    1u8.hash(state);
                    n.hash(state);
                    d.get().hash(state);
                }
            }
        }
    }

    /// Rebuild the exact `Node::ControlMap` pattern that this inline wave
    /// collapsed from, used only on the rare fallback path (non-object haps,
    /// multi-cycle parts, >48-bit rationals, or `cut_mask` boundaries).
    pub(crate) fn to_control_pattern(&self, key_id: u16, math_inside: bool) -> Pattern {
        let apply_times = |mut pat: Pattern| {
            let (times, _) = self.times.parts();
            for change in times.iter().rev() {
                match *change {
                    WaveTime::Fast(n, d) => {
                        pat = pat.fast(Fraction::from_ticks(i128::from(n), d));
                    }
                    WaveTime::Late(n, d) => {
                        pat = pat.late(Fraction::from_ticks(i128::from(n), d));
                    }
                }
            }
            pat
        };
        let pat = crate::signal_static(self.body);
        let pat = if math_inside {
            apply_times(self.math.apply_to_pattern(pat))
        } else {
            self.math.apply_to_pattern(apply_times(pat))
        };
        Pattern::of(Pattern::control_map_node(pat, key_id, false))
    }

    /// `pattern` as a wave read in steps, as a `segment` of one is, with
    /// the count of its steps a cycle.
    fn of_steps_ref(pattern: crate::NodeRef<'_>) -> Option<(Self, u32)> {
        let mut steps = None;
        let (wave, _) = Self::read_in(pattern, Some(&mut steps), false)?;
        Some((wave, u32::try_from(steps?).ok()?))
    }

    fn read_in(
        pattern: crate::NodeRef<'_>,
        steps: Option<&mut Option<i128>>,
        strict_runtime: bool,
    ) -> Option<(Self, Option<bool>)> {
        let mut times = [WaveTime::IDENTITY; Self::MOST_TIMES];
        let mut times_len = 0u8;
        let mut inline_math = [(crate::compose::ComposeOp::Add, 0.0); 4];
        let mut inline_len = 0u8;
        let mut spilled_math: Option<Vec<(crate::compose::ComposeOp, f64)>> = None;
        let mut cut_mask = 0u8;
        let mut math_depth: Option<Option<u8>> = None;
        let body = Self::read(
            pattern,
            strict_runtime,
            &mut |time| {
                let idx = usize::from(times_len);
                if idx < Self::MOST_TIMES {
                    times[idx] = time;
                }
                times_len = times_len.saturating_add(1);
            },
            &mut |op, number, depth, cut| {
                let depth_u8 = depth as u8;
                math_depth = match math_depth {
                    None => Some(Some(depth_u8)),
                    Some(Some(prev)) if prev == depth_u8 => Some(Some(prev)),
                    _ => Some(None),
                };
                if let Some(vec) = &mut spilled_math {
                    vec.push((op, number));
                } else if usize::from(inline_len) < inline_math.len() {
                    inline_math[usize::from(inline_len)] = (op, number);
                    inline_len += 1;
                } else {
                    let mut vec = Vec::with_capacity(8);
                    vec.extend_from_slice(&inline_math);
                    vec.push((op, number));
                    spilled_math = Some(vec);
                }
                if let Some(over) = cut
                    && over < 8
                {
                    cut_mask |= 1u8 << over;
                }
            },
            steps,
        )?;
        // The walk met the outermost number first.
        let math = match spilled_math {
            Some(mut vec) => {
                vec.reverse();
                WaveMath::from_ops(&vec)
            }
            None => {
                inline_math[..usize::from(inline_len)].reverse();
                WaveMath::from_ops(&inline_math[..usize::from(inline_len)])
            }
        };
        let math_inside = match math_depth {
            None => Some(true),
            Some(Some(d)) if d == times_len => Some(true),
            Some(Some(0)) => Some(false),
            _ => None,
        };
        Some((
            Self {
                math,
                times: WaveTimes::from_raw(times, times_len, cut_mask),
                body,
            },
            math_inside,
        ))
    }

    /// The body of `pattern`, when it is a wave: each change of time goes
    /// to `time` and each number of the arithmetic to `number`, the
    /// outermost first. For a number whose time may cut a cycle of the
    /// query, `number` also takes the count of the changes of time over it.
    fn read(
        mut pattern: crate::NodeRef<'_>,
        strict_runtime: bool,
        time: &mut dyn FnMut(WaveTime),
        number: &mut dyn FnMut(crate::compose::ComposeOp, f64, usize, Option<usize>),
        mut steps: Option<&mut Option<i128>>,
    ) -> Option<crate::NumberSignal> {
        let mut times = 0;
        // Whether a cycle of the query lies in one cycle of the time here.
        let mut cycles_kept = true;
        loop {
            if strict_runtime {
                if pattern.has_runtime_settings() || !pattern.is_cacheable() {
                    return None;
                }
            } else if !pattern.tables_may_answer() {
                return None;
            }
            pattern = match pattern.kind() {
                Node::Signal(signal) => {
                    return (times <= Self::MOST_TIMES)
                        .then(|| crate::number_signal(signal))
                        .flatten();
                }
                // A segment, with no change of time on either side of it:
                // the number of the step that a part lies in.
                Node::FilterValues(..)
                    if let Some(steps) = steps.as_deref_mut()
                        && steps.is_none()
                        && times == 0
                        && let Some((count, functions)) = Self::segment_ref(pattern) =>
                {
                    *steps = Some(count);
                    let mut timed = false;
                    let body = Self::read(
                        crate::NodeRef::Pat(functions),
                        strict_runtime,
                        &mut |_| timed = true,
                        number,
                        None,
                    )?;
                    return (!timed).then_some(body);
                }
                Node::Fast(node) if node.1 > Fraction::ZERO => {
                    cycles_kept &= node.1.numer() == 1;
                    time(WaveTime::fast(node.1)?);
                    times += 1;
                    crate::NodeRef::Pat(&node.0)
                }
                Node::FastSmall(cell, flags, numer, denom, _) if *numer > 0 => {
                    let factor = Fraction::new(i128::from(*numer), i128::from(*denom));
                    cycles_kept &= factor.numer() == 1;
                    time(WaveTime::fast(factor)?);
                    times += 1;
                    crate::NodeRef::Plain(cell, *flags)
                }
                Node::Shift(node) => {
                    cycles_kept &= node.1.denom() == 1;
                    time(WaveTime::late(node.1)?);
                    times += 1;
                    crate::NodeRef::Pat(&node.0)
                }
                Node::ShiftSmall(cell, flags, numer, denom, _) => {
                    let offset = Fraction::new(i128::from(*numer), i128::from(*denom));
                    cycles_kept &= offset.denom() == 1;
                    time(WaveTime::late(offset)?);
                    times += 1;
                    crate::NodeRef::Plain(cell, *flags)
                }
                // A run of numbers, the last application first.
                Node::Constants(node) => {
                    for meeting in node.1.iter().rev() {
                        let (op, other) = meeting.number()?;
                        op.of_numbers(0.0, 0.0)?;
                        number(op, other, times, (!cycles_kept).then_some(times));
                    }
                    crate::NodeRef::Pat(&node.0)
                }
                // A `pure` of a number answers the number in every cycle,
                // on a hap that carries nothing more: no place in the
                // text, no key, no visual and no slider.
                Node::AppLeft(pair) => {
                    let crate::AppLeftNode(functions, values, _, Combine::Composed(op), _) =
                        &**pair
                    else {
                        return None;
                    };
                    let Node::Pure(crate::Value::F64(other)) = values.node_kind() else {
                        return None;
                    };
                    // An op of arithmetic makes a number of two numbers.
                    op.of_numbers(0.0, 0.0)?;
                    if strict_runtime {
                        if values.has_runtime_settings()
                            || !values.is_cacheable()
                            || values.pure_loc_raw().is_some()
                        {
                            return None;
                        }
                    } else if !values.tables_may_answer() {
                        return None;
                    }
                    number(*op, *other, times, (!cycles_kept).then_some(times));
                    crate::NodeRef::Pat(functions)
                }
                _ => return None,
            };
        }
    }

    /// Whether `pattern` is a run of numbers over a `segment`, by its kinds
    /// alone.
    fn holds_segment_ref(mut pattern: crate::NodeRef<'_>) -> bool {
        // A `range` is a run of constants, or two applications of one.
        for _ in 0..Self::MOST_TIMES {
            pattern = match pattern.kind() {
                Node::Constants(node) => crate::NodeRef::Pat(&node.0),
                Node::AppLeft(pair) => crate::NodeRef::Pat(&pair.0),
                _ => break,
            };
        }
        matches!(pattern.kind(), Node::FilterValues(boxed) if matches!(boxed.0.node_kind(), Node::AppRight(..)))
    }

    /// The count of steps and the pattern of the signal, when `pattern` is a
    /// `segment` that holds nothing but its values: a pulse with no place in
    /// the text and a value that keeps the hap of the signal.
    fn segment_ref(pattern: crate::NodeRef<'_>) -> Option<(i128, &Pattern)> {
        if is_unfused() {
            return None;
        }
        let functions = pattern.under_structure()?;
        let Node::FilterValues(boxed) = pattern.kind() else {
            return None;
        };
        let structured = &boxed.0;
        let Node::AppRight(node) = structured.node_kind() else {
            return None;
        };
        let Node::Sequence(layers) = node.1.node_kind() else {
            return None;
        };
        let (count, pulse) = crate::sequence::pulse(layers)?;
        (pulse.js_truthy()
            && structured.tables_may_answer()
            && !node.1.is_volatile()
            && !node.1.has_runtime_settings())
        .then_some((count, functions))
    }

    /// `time`, when it and its parts lie below `bits` bits: a time of
    /// ordinary playback, or a factor a score writes.
    fn plain(time: Fraction, bits: u32) -> Option<Fraction> {
        (time.numer().unsigned_abs() >> bits == 0 && time.denom() >> bits == 0).then_some(time)
    }

    /// The number of the hap that the graph of the wave answers for a
    /// function hap that asks over `met` and is cut to `part`, a part in
    /// one cycle, when that one hap meets all of `part`. `None` for a part
    /// that a number of the wave cuts, and for a time outside ordinary
    /// playback: the graph answers both, so that every refusal stays the
    /// graph's.
    pub(crate) fn number(
        &self,
        met: TimeSpan,
        part: TimeSpan,
        controls: &crate::OrderedMap,
    ) -> Option<f64> {
        for time in [met.end, part.begin, part.end] {
            Self::plain(time, 48)?;
        }
        let mut time = Self::plain(met.begin, 48)?;
        // The part in the time of each number that may cut it.
        let mut inside = part;
        let (times, cut_mask) = self.times.parts();
        for (over, change) in times.iter().enumerate() {
            time = change.inward(time)?;
            if cut_mask != 0 {
                inside = TimeSpan::new(change.inward(inside.begin)?, change.inward(inside.end)?);
                if (cut_mask & (1u8 << (over + 1))) != 0 && !in_one_cycle(inside) {
                    return None;
                }
            }
        }
        self.value_at(time, controls)
    }

    /// The number of the wave at `time`, with its arithmetic.
    #[inline]
    fn value_at(&self, time: Fraction, controls: &crate::OrderedMap) -> Option<f64> {
        let number = (self.body)(time, controls);
        self.math.apply(number)
    }
}

/// The name that `values` sets, when it is a control of a [`Wave`]: each
/// of its haps then holds one number under that name, and nothing more.
pub(crate) fn wave_name(values: &Pattern) -> Option<crate::Key> {
    let (inner, key) = ValueSignal::named_key(values)?;
    Wave::read(inner, false, &mut |_| (), &mut |_, _, _, _| (), None).map(|_| key)
}
