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
    Combine, Hap, LookupFlow, Node, Pattern, PatternPair, SpanUse, State, TimeSpan, Value,
    applied_hap,
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
fn is_unfused() -> bool {
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
        let scale = Fraction::int(count);
        let first = span.begin.checked_mul(scale)?.floor().numer();
        let after = if span.begin == span.end {
            first.checked_add(1)?
        } else {
            span.end.checked_mul(scale)?.ceil().numer()
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
pub(crate) fn app_right_onto(
    pair: &PatternPair,
    apply: &Combine,
    flow: LookupFlow,
    state: &State,
    out: &mut Vec<Hap>,
) -> bool {
    let PatternPair(functions, values) = pair;
    let Some(segment) = Segment::of(functions, values, apply, flow) else {
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
        if values.runtime_settings.is_some() || values.is_volatile() || functions.is_discrete() {
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
    /// `values` as a control of a segment of a wave, when it is one and
    /// the application sets each number in place: any other application
    /// is the graph's, with one query of the side for all its haps.
    pub(crate) fn of(values: &Pattern, apply: &Combine, flow: LookupFlow) -> Option<Self> {
        let sets = matches!(apply, Combine::Composed(crate::compose::ComposeOp::Set))
            && flow == LookupFlow::Set;
        if !sets || is_unfused() {
            return None;
        }
        // The kinds alone say that most sides are none, before the global
        // pool of controls is read for the name.
        let Node::MapHapsOwned(inner, _, SpanUse::Ignores) = values.node_kind() else {
            return None;
        };
        if !Wave::holds_segment(inner) {
            return None;
        }
        let (inner, named) = ValueSignal::named(values)?;
        let (wave, steps) = Wave::of_steps(inner)?;
        Some(Self {
            key: named.key,
            wave,
            steps,
        })
    }

    /// Whether this control answers for `hf` in place: a hap of data
    /// controls with no lookup, whose part lies in its whole and has width.
    /// Asked of every function hap before the control answers any, so that
    /// a side that it cannot answer is queried once for all of them.
    pub(crate) fn takes(hf: &Hap) -> bool {
        hf.pick_lookup.is_none()
            && matches!(hf.value, Value::Object(_))
            && hf.value.is_data(0)
            && hf.part.begin < hf.part.end
            && hf.part.intersection(&hf.whole_or_part()) == Some(hf.part)
    }

    /// The number that the graph answers for a function hap cut to `part`,
    /// when `part` lies in one step. The wave is asked over that step.
    pub(crate) fn number(&self, part: TimeSpan, controls: &crate::OrderedMap) -> Option<f64> {
        if part.begin >= part.end {
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

    /// The number of each step that `part` meets, in order, with the piece
    /// of `part` in the step, into `out`. `None` for a part of no width, for
    /// a span that the graph refuses or whose budget it refuses, and for a
    /// step whose number only the graph gives. A query stop ends the walk
    /// early, and the caller reads the stop.
    pub(crate) fn steps_over(
        &self,
        part: TimeSpan,
        controls: &crate::OrderedMap,
        out: &mut Vec<(TimeSpan, f64)>,
    ) -> Option<()> {
        if part.begin >= part.end {
            return None;
        }
        let grid = StepGrid::over(i128::from(self.steps), part)?;
        for time in [part.begin, part.end] {
            Wave::plain(time, 48)?;
        }
        out.reserve(grid.len());
        let mut begin = grid.time(grid.steps().start)?;
        for k in grid.steps() {
            if crate::query_stop_requested() {
                return Some(());
            }
            let end = grid.time(k + 1)?;
            let piece = TimeSpan::new(begin.max(part.begin), end.min(part.end));
            out.push((piece, self.wave.value_at(begin, controls)?));
            begin = end;
        }
        Some(())
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
/// [`Self::cut_under`] counts, and each part is read in its time.
pub(crate) struct Wave {
    body: crate::NumberSignal,
    /// The changes of time, the outermost first.
    times: Box<[WaveTime]>,
    /// The arithmetic, the innermost first: each op with its number.
    math: Box<[(crate::compose::ComposeOp, f64)]>,
    /// For each number whose time may cut a cycle of the query, the count
    /// of the changes of time over it, in rising order.
    cut_under: Box<[usize]>,
}

/// One change of time over a [`Wave`].
enum WaveTime {
    /// `fast(factor)`: what lies inside is asked at `t * factor`.
    Fast(Fraction),
    /// A shift by `by`: what lies inside is asked at `t - by`.
    Late(Fraction),
}

impl WaveTime {
    /// `time` as what lies inside is asked.
    fn inward(&self, time: Fraction) -> Option<Fraction> {
        match self {
            Self::Fast(factor) => time.checked_mul(*factor),
            Self::Late(by) => time.checked_sub(*by),
        }
    }
}

impl Wave {
    /// The most changes of time over a wave. With [`Self::plain`] times
    /// and factors, no time of a wave with so many leaves the native range.
    const MOST_TIMES: usize = 3;

    /// `pattern` as a wave, when it is one.
    pub(crate) fn of(pattern: &Pattern) -> Option<Self> {
        Self::read_in(pattern, None)
    }

    /// `pattern` as a wave read in steps, as a `segment` of one is, with
    /// the count of its steps a cycle.
    fn of_steps(pattern: &Pattern) -> Option<(Self, u32)> {
        let mut steps = None;
        let wave = Self::read_in(pattern, Some(&mut steps))?;
        Some((wave, u32::try_from(steps?).ok()?))
    }

    fn read_in(pattern: &Pattern, steps: Option<&mut Option<i128>>) -> Option<Self> {
        let mut times = Vec::new();
        let mut math = Vec::new();
        let mut cut_under = Vec::new();
        let body = Self::read(
            pattern,
            &mut |time| times.push(time),
            &mut |op, number, cut| {
                math.push((op, number));
                cut_under.extend(cut);
            },
            steps,
        )?;
        // The walk met the outermost number first.
        math.reverse();
        cut_under.dedup();
        Some(Self {
            body,
            times: times.into(),
            math: math.into(),
            cut_under: cut_under.into(),
        })
    }

    /// The body of `pattern`, when it is a wave: each change of time goes
    /// to `time` and each number of the arithmetic to `number`, the
    /// outermost first. For a number whose time may cut a cycle of the
    /// query, `number` also takes the count of the changes of time over it.
    fn read(
        mut pattern: &Pattern,
        time: &mut dyn FnMut(WaveTime),
        number: &mut dyn FnMut(crate::compose::ComposeOp, f64, Option<usize>),
        mut steps: Option<&mut Option<i128>>,
    ) -> Option<crate::NumberSignal> {
        let mut times = 0;
        // Whether a cycle of the query lies in one cycle of the time here.
        let mut cycles_kept = true;
        loop {
            if !pattern.tables_may_answer() {
                return None;
            }
            pattern = match pattern.node_kind() {
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
                        && let Some((count, functions)) = Self::segment(pattern) =>
                {
                    *steps = Some(count);
                    let mut timed = false;
                    let body = Self::read(functions, &mut |_| timed = true, number, None)?;
                    return (!timed).then_some(body);
                }
                Node::Fast(inner, factor, _) if **factor > Fraction::ZERO => {
                    cycles_kept &= factor.numer() == 1;
                    time(WaveTime::Fast(Self::plain(**factor, 16)?));
                    times += 1;
                    inner
                }
                Node::Shift(inner, by, _) => {
                    cycles_kept &= by.denom() == 1;
                    time(WaveTime::Late(Self::plain(**by, 16)?));
                    times += 1;
                    inner
                }
                // A run of numbers, the last application first.
                Node::Constants(inner, run) => {
                    for meeting in run.iter().rev() {
                        let (op, other) = meeting.number()?;
                        op.of_numbers(0.0, 0.0)?;
                        number(op, other, (!cycles_kept).then_some(times));
                    }
                    inner
                }
                // A `pure` of a number answers the number in every cycle,
                // on a hap that carries nothing more: no place in the
                // text, no key, no visual and no slider.
                Node::AppLeft(pair, Combine::Composed(op), _, _) => {
                    let crate::PatternPair(functions, values) = &**pair;
                    let Node::Pure(crate::Value::F64(other)) = values.node_kind() else {
                        return None;
                    };
                    // An op of arithmetic makes a number of two numbers.
                    op.of_numbers(0.0, 0.0)?;
                    if !values.tables_may_answer() {
                        return None;
                    }
                    number(*op, *other, (!cycles_kept).then_some(times));
                    functions
                }
                _ => return None,
            };
        }
    }

    /// Whether `pattern` is a run of numbers over a `segment`, by its kinds
    /// alone.
    fn holds_segment(mut pattern: &Pattern) -> bool {
        // A `range` is a run of constants, or two applications of one.
        for _ in 0..Self::MOST_TIMES {
            pattern = match pattern.node_kind() {
                Node::Constants(inner, _) => inner,
                Node::AppLeft(pair, ..) => &pair.0,
                _ => break,
            };
        }
        matches!(pattern.node_kind(), Node::FilterValues(inner, _) if matches!(inner.node_kind(), Node::AppRight(..)))
    }

    /// The count of steps and the pattern of the signal, when `pattern` is a
    /// `segment` that holds nothing but its values: a pulse with no place in
    /// the text and a value that keeps the hap of the signal.
    fn segment(pattern: &Pattern) -> Option<(i128, &Pattern)> {
        if is_unfused() {
            return None;
        }
        let functions = pattern.under_structure()?;
        let Node::FilterValues(structured, _) = pattern.node_kind() else {
            return None;
        };
        let Node::AppRight(pair, ..) = structured.node_kind() else {
            return None;
        };
        let Node::Sequence(layers) = pair.1.node_kind() else {
            return None;
        };
        let (count, pulse) = crate::sequence::pulse(layers)?;
        (pulse.js_truthy()
            && structured.tables_may_answer()
            && !pair.1.is_volatile()
            && pair.1.runtime_settings.is_none())
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
        let mut cut = self.cut_under.iter().copied().peekable();
        let mut inside = part;
        for (over, change) in self.times.iter().enumerate() {
            time = change.inward(time)?;
            if cut.peek().is_some() {
                inside = TimeSpan::new(change.inward(inside.begin)?, change.inward(inside.end)?);
                if cut.next_if_eq(&(over + 1)).is_some() && !in_one_cycle(inside) {
                    return None;
                }
            }
        }
        self.value_at(time, controls)
    }

    /// The number of the wave at `time`, with its arithmetic.
    fn value_at(&self, time: Fraction, controls: &crate::OrderedMap) -> Option<f64> {
        let mut number = (self.body)(time, controls);
        for (op, other) in &self.math {
            number = op.of_numbers(number, *other)?;
        }
        Some(number)
    }
}

/// The name that `values` sets, when it is a control of a [`Wave`]: each
/// of its haps then holds one number under that name, and nothing more.
pub(crate) fn wave_name(values: &Pattern) -> Option<crate::Key> {
    let (inner, named) = ValueSignal::named(values)?;
    Wave::read(inner, &mut |_| (), &mut |_, _, _| (), None).map(|_| named.key)
}
