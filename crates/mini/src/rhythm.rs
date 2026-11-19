//! The plain values of a mini-notation pattern, at places that are known
//! when the pattern is built.

use crate::{Build, ParseError, Postfix, Span};
use rustel_core::{
    GivenLayer, Locations, Pattern, SequenceLayer, SequenceStep, SourceStep, SourcedLayer,
    StepSource, Value, pure, sequence, sequence_of, silence,
};
use rustel_fraction::Fraction;
use std::ops::Range;
use std::sync::Arc;

/// The most steps that one layer lists. A layer of a plan with more reads
/// the plan at each query, and a rhythm with more keeps a node for each
/// value.
const MAX_STEPS: usize = 256;

/// The largest numerator or denominator of a period. A speed past it keeps
/// its own node, which names itself when a time leaves the native range.
const MAX_PERIOD_TERM: i128 = 1 << 20;

/// Atoms and rests, their concatenations, stacks and alternations, each at
/// a constant speed: values at places that are known when the pattern is
/// built.
///
/// A rhythm answers as the graph of the same constructs does, in the same
/// order, and reports the same step count. It becomes one node, a
/// [`sequence`], where the graph is a node for each value and each level.
pub(crate) struct Rhythm {
    layers: Vec<SequenceLayer>,
    /// The step count that the graph of the same constructs reports.
    steps: Option<Fraction>,
    /// Whether this is one atom as it is written, which builds a `pure`. A
    /// construct around an atom answers the same value each cycle and is no
    /// `pure`: a function that reads its argument tells the two apart.
    atom: bool,
}

impl Rhythm {
    /// A rest: what `silence()` answers.
    pub(crate) fn rest() -> Self {
        Self {
            layers: Vec::new(),
            steps: Some(Fraction::ONE),
            atom: false,
        }
    }

    /// One value a cycle: what `pure(value)` answers, at `location`.
    pub(crate) fn atom(value: Value, location: Option<(usize, usize)>) -> Self {
        let mut locations = Locations::new();
        if let Some(location) = location {
            locations.push(location);
        }
        let step = SequenceStep::new(Fraction::ZERO, Fraction::ONE, value, locations);
        Self {
            layers: vec![SequenceLayer {
                period: Fraction::ONE,
                steps: vec![step],
            }],
            steps: Some(Fraction::ONE),
            atom: true,
        }
    }

    /// `fastcat` of two parts or more: in each cycle, that cycle of each
    /// part, one after the other.
    pub(crate) fn concatenated(parts: &[&Rhythm]) -> Option<Self> {
        let count = whole(parts.len())?;
        let turns = parts
            .iter()
            .try_fold(1, |turns, part| lcm(turns, part.turns()?))?;
        // Room for each step of each part, in each cycle.
        let held = |part: &&Rhythm| {
            part.layers
                .iter()
                .map(|layer| layer.steps.len())
                .sum::<usize>()
        };
        let room = parts.iter().map(held).sum::<usize>();
        let room = room.saturating_mul(usize::try_from(turns).ok()?);
        let mut steps = Vec::with_capacity(room.min(MAX_STEPS));
        for turn in 0..turns {
            for (index, part) in parts.iter().enumerate() {
                let window = (
                    whole(index)?.checked_div(count)?,
                    whole(index + 1)?.checked_div(count)?,
                );
                part.squeeze_cycle(turn, window, turns, &mut steps)?;
            }
        }
        Some(Self {
            layers: layer(turns, steps)?,
            steps: Some(count),
            atom: false,
        })
    }

    /// `timecat` of `parts`: a stack of each part, squeezed into its window
    /// of the cycle. `None` is a part of no weight, which is silent.
    pub(crate) fn squeezed(
        parts: &[Option<&Rhythm>],
        windows: &[(Fraction, Fraction)],
    ) -> Option<Self> {
        let mut layers = Vec::new();
        for (part, window) in parts.iter().zip(windows) {
            let Some(part) = part else {
                continue;
            };
            let inside = Fraction::ZERO <= window.0 && window.0 < window.1;
            if !(inside && window.1 <= Fraction::ONE) {
                return None;
            }
            let turns = part.turns()?;
            let mut steps = Vec::new();
            for turn in 0..turns {
                part.squeeze_cycle(turn, *window, turns, &mut steps)?;
            }
            layers.extend(layer(turns, steps)?);
        }
        // A squeezed part reports no step count, and a silent part reports
        // one step.
        let steps = parts.iter().any(Option::is_none).then_some(Fraction::ONE);
        Some(Self {
            layers,
            steps,
            atom: false,
        })
    }

    /// This rhythm with the structure of `mask`, which divides each cycle
    /// into equal steps: in each step that is set, the values that sound
    /// over the whole step, from the start of the step to its end. `None`
    /// for a value that begins or ends inside a set step, which the
    /// structure cuts.
    pub(crate) fn struck(&self, mask: &[u8]) -> Option<Self> {
        let count = whole(mask.len())?;
        let turns = self.turns()?;
        let period = Fraction::int(turns);
        let mut steps = Vec::new();
        for turn in 0..turns {
            // The steps of this cycle, at their places in it.
            let mut cycle = Vec::new();
            self.squeeze_cycle(turn, (Fraction::ZERO, Fraction::ONE), 1, &mut cycle)?;
            let start = Fraction::int(turn);
            for (index, _) in mask.iter().enumerate().filter(|(_, set)| **set != 0) {
                let begin = start.checked_add(whole(index)?.checked_div(count)?)?;
                let end = start.checked_add(whole(index + 1)?.checked_div(count)?)?;
                for step in &cycle {
                    if step.end() <= begin || step.begin() >= end {
                        continue;
                    }
                    let cut = step.begin() > begin || step.end() < end;
                    if cut || steps.len() == MAX_STEPS {
                        return None;
                    }
                    let window = (begin.checked_div(period)?, end.checked_div(period)?);
                    steps.push(step.placed(window, window));
                }
            }
        }
        Some(Self {
            layers: layer(turns, steps)?,
            steps: Some(count),
            atom: false,
        })
    }

    /// `stack` of `parts`. On `Err`, the parts as they were: their step
    /// counts have no common multiple in the native range.
    pub(crate) fn stacked(parts: Vec<Rhythm>) -> Result<Self, Vec<Rhythm>> {
        // The common multiple of the step counts that are known, from the
        // last, as a stack of the graphs counts it.
        let steps = {
            let mut known = parts.iter().filter_map(|part| part.steps);
            match known.next_back() {
                None => Some(None),
                Some(last) => known
                    .try_fold(last, |steps, next| steps.checked_lcm(next))
                    .map(Some),
            }
        };
        match steps {
            Some(steps) => Ok(Self {
                layers: parts.into_iter().flat_map(|part| part.layers).collect(),
                steps,
                atom: false,
            }),
            None => Err(parts),
        }
    }

    /// This rhythm, `factor` times as fast, for a factor above zero. On
    /// `Err`, the rhythm as it was.
    pub(crate) fn faster(mut self, factor: Fraction) -> Result<Self, Self> {
        if factor <= Fraction::ZERO {
            return Err(self);
        }
        let small = |period: &Fraction| {
            period.numer() <= MAX_PERIOD_TERM && period.denom() <= MAX_PERIOD_TERM
        };
        let periods: Option<Vec<Fraction>> = self
            .layers
            .iter()
            .map(|layer| layer.period.checked_div(factor).filter(small))
            .collect();
        match periods {
            Some(periods) => {
                for (layer, period) in self.layers.iter_mut().zip(periods) {
                    layer.period = period;
                }
                self.atom = false;
                Ok(self)
            }
            None => Err(self),
        }
    }

    /// This rhythm with each cycle at the speed that `rates` names for it,
    /// as `fast` of a pattern of rates plays it: `bd*<1 2 4>` is one, two,
    /// then four `bd` a cycle. `rate_of` reads the rate that a value names.
    ///
    /// `None` leaves both rhythms to their graph: for a rhythm of more
    /// layers than one, which the graph answers cycle by cycle and one
    /// node layer by layer, for a step whose part crosses a bound of its
    /// cycle, which the graph cuts at the cycles of the rates alone, and
    /// for rates that are not one whole number above zero for each cycle.
    pub(crate) fn at_rates(
        &self,
        rates: &Rhythm,
        rate_of: impl Fn(&Value) -> Option<Fraction>,
    ) -> Option<Self> {
        let ([own], [named]) = (self.layers.as_slice(), rates.layers.as_slice()) else {
            return None;
        };
        // One rate a cycle: step `n` of the rates fills cycle `n`.
        let count = whole(named.steps.len())?;
        if named.period != count {
            return None;
        }
        let rates = named
            .steps
            .iter()
            .enumerate()
            .map(|(index, step)| {
                let fills = (
                    whole(index)?.checked_div(count)?,
                    whole(index + 1)?.checked_div(count)?,
                );
                let rate = rate_of(step.value())?;
                let named = step.whole() == fills
                    && (step.begin(), step.end()) == fills
                    && rate.denom() == 1
                    && rate >= Fraction::ONE;
                named.then(|| (rate.numer(), step.locations().to_vec()))
            })
            .collect::<Option<Vec<_>>>()?;
        // The part of each step lies in one cycle of its own time.
        let in_a_cycle = own.steps.iter().all(|step| {
            let times =
                (step.begin().checked_mul(own.period)).zip(step.end().checked_mul(own.period));
            times.is_some_and(|(begin, end)| end <= begin.floor() + Fraction::ONE)
        });
        if !in_a_cycle {
            return None;
        }
        let turns = lcm(count.numer(), self.turns()?)?;
        let mut steps = Vec::new();
        for cycle in 0..turns {
            let (rate, at) = rates.get(usize::try_from(cycle % count.numer()).ok()?)?;
            // The cycles of this rhythm that the cycle holds, one after
            // the other.
            let first = cycle.checked_mul(*rate)?;
            for part in 0..*rate {
                let window = (
                    Fraction::int(part).checked_div(Fraction::int(*rate))?,
                    Fraction::int(part + 1).checked_div(Fraction::int(*rate))?,
                );
                let from = steps.len();
                let turn = first.checked_add(part)?;
                self.squeeze_cycle_into(turn, cycle, window, turns, &mut steps)?;
                // The locations of the rate come first on each hap.
                for step in &mut steps[from..] {
                    step.locate_first(at);
                }
            }
        }
        Some(Self {
            layers: layer(turns, steps)?,
            steps: self.steps,
            atom: false,
        })
    }

    /// `repeatCycles(count).fast(count)`, for a whole count above zero: each
    /// cycle of this rhythm, `count` times in the cycle. On `Err`, the
    /// rhythm as it was.
    pub(crate) fn replicated(self, count: Fraction) -> Result<Self, Self> {
        let layers = (|| {
            if count.denom() != 1 || count < Fraction::ONE {
                return None;
            }
            let turns = self.turns()?;
            let mut steps = Vec::new();
            for turn in 0..turns {
                let mut at = Fraction::ZERO;
                while at < count {
                    let next = at.checked_add(Fraction::ONE)?;
                    let window = (at.checked_div(count)?, next.checked_div(count)?);
                    self.squeeze_cycle(turn, window, turns, &mut steps)?;
                    at = next;
                }
            }
            layer(turns, steps)
        })();
        match layers {
            Some(layers) => Ok(Self {
                layers,
                steps: self.steps,
                atom: false,
            }),
            None => Err(self),
        }
    }

    /// Every hap also at `locations`, after its own.
    pub(crate) fn located(mut self, locations: &[(usize, usize)]) -> Self {
        for step in self.layers.iter_mut().flat_map(|layer| &mut layer.steps) {
            step.locate(locations);
        }
        self
    }

    /// The pattern of this rhythm: a `pure` for an atom, and one node for
    /// any other rhythm.
    pub(crate) fn into_pattern(self) -> Pattern {
        let Self {
            mut layers,
            steps,
            atom,
        } = self;
        let pattern = if atom {
            match layers.pop().and_then(|mut layer| layer.steps.pop()) {
                Some(step) => constant(step.value().clone(), step.locations().to_vec()),
                None => silence(),
            }
        } else {
            sequence(layers).expect("each layer of a rhythm is sound when it is made")
        };
        pattern.with_steps(steps)
    }

    /// The cycles after which every layer repeats.
    fn turns(&self) -> Option<i128> {
        self.layers
            .iter()
            .try_fold(1, |turns, layer| lcm(turns, layer.period.numer()))
    }

    /// Add the steps of cycle `turn` to `out`, in the order a query of that
    /// cycle answers them. Each is squeezed into `window` of the cycle,
    /// which is one of the `turns` cycles of the period of `out`. A step
    /// that crosses a bound of the cycle is cut there, as a query of the
    /// cycle cuts its hap: the part is the piece in the cycle, and the
    /// whole stays whole.
    fn squeeze_cycle(
        &self,
        turn: i128,
        window: (Fraction, Fraction),
        turns: i128,
        out: &mut Vec<SequenceStep>,
    ) -> Option<()> {
        self.squeeze_cycle_into(turn, turn, window, turns, out)
    }

    /// [`Self::squeeze_cycle`] of cycle `turn`, into `window` of cycle
    /// `target` of the period of `out`.
    fn squeeze_cycle_into(
        &self,
        turn: i128,
        target: i128,
        window: (Fraction, Fraction),
        turns: i128,
        out: &mut Vec<SequenceStep>,
    ) -> Option<()> {
        let cycle = Fraction::int(turn);
        let width = window.1.checked_sub(window.0)?;
        let offset = Fraction::int(target).checked_add(window.0)?;
        let turns = Fraction::int(turns);
        // The bounds of the cycle, at their places: most steps begin or end
        // on one.
        let first = offset.checked_div(turns)?;
        let last = offset.checked_add(width)?.checked_div(turns)?;
        // The width of the window, as a share of the period.
        let scale = width.checked_div(turns)?;
        // A time of the cycle, at its place in the window, as a share of
        // the period.
        let place = |time: Fraction| {
            if time == Fraction::ZERO {
                return Some(first);
            }
            if time == Fraction::ONE {
                return Some(last);
            }
            time.checked_mul(scale)?.checked_add(first)
        };
        for layer in &self.layers {
            // A layer of one cycle holds each step at its time in the
            // cycle, in every cycle.
            if layer.period == Fraction::ONE {
                for step in &layer.steps {
                    let (begin, end) = (step.begin(), step.end());
                    if end <= Fraction::ZERO || begin >= Fraction::ONE {
                        continue;
                    }
                    if out.len() == MAX_STEPS {
                        return None;
                    }
                    let part = (
                        place(begin.max(Fraction::ZERO))?,
                        place(end.min(Fraction::ONE))?,
                    );
                    // A step that the cycle does not cut reads the places
                    // of its part.
                    let uncut = begin >= Fraction::ZERO && end <= Fraction::ONE;
                    let whole = match step.whole() {
                        whole if uncut && whole == (begin, end) => part,
                        whole => (place(whole.0)?, place(whole.1)?),
                    };
                    out.push(step.placed(whole, part));
                }
                continue;
            }
            // The time of a step in a turn of the layer, from the start of
            // the cycle.
            let time = |at: Fraction, share: Fraction| {
                at.checked_add(share)?
                    .checked_mul(layer.period)?
                    .checked_sub(cycle)
            };
            // The turns of the layer that meet the cycle.
            let mut at = cycle.checked_div(layer.period)?.floor();
            let last = cycle
                .checked_add(Fraction::ONE)?
                .checked_div(layer.period)?
                .ceil();
            while at < last {
                for step in &layer.steps {
                    let (begin, end) = (time(at, step.begin())?, time(at, step.end())?);
                    if end <= Fraction::ZERO || begin >= Fraction::ONE {
                        continue;
                    }
                    if out.len() == MAX_STEPS {
                        return None;
                    }
                    let whole = step.whole();
                    let whole = (place(time(at, whole.0)?)?, place(time(at, whole.1)?)?);
                    let part = (
                        place(begin.max(Fraction::ZERO))?,
                        place(end.min(Fraction::ONE))?,
                    );
                    out.push(step.placed(whole, part));
                }
                at = at.checked_add(Fraction::ONE)?;
            }
        }
        Some(())
    }
}

/// One value a cycle, as `pure` answers it, at `locations`.
pub(crate) fn constant(value: Value, locations: Vec<(usize, usize)>) -> Pattern {
    match locations.is_empty() {
        true => pure(value),
        false => pure(value).with_added_context(locations),
    }
}

/// A window of a cycle: from `begin` to `end`, of `parts` equal parts.
#[derive(Clone, Copy)]
struct Window {
    begin: i64,
    end: i64,
    parts: i64,
}

impl Window {
    const CYCLE: Self = Self {
        begin: 0,
        end: 1,
        parts: 1,
    };

    /// The parts `from..to` of `count` equal parts of this window.
    fn parts(self, (from, to): (i64, i64), count: i64) -> Option<Self> {
        let width = self.end.checked_sub(self.begin)?;
        let start = self.begin.checked_mul(count)?;
        Some(Self {
            begin: start.checked_add(width.checked_mul(from)?)?,
            end: start.checked_add(width.checked_mul(to)?)?,
            parts: self.parts.checked_mul(count)?,
        })
    }
}

/// The speeds that a step is under, the innermost first: each holds where
/// the text writes its rate.
struct Speeds<'a> {
    at: At,
    above: Option<&'a Speeds<'a>>,
}

/// The steps of one layer that a reader asks for, as a walk of the cycles
/// that the span meets writes them: each at its time in cycles from the
/// start of the period.
struct Asked<'a> {
    /// The span, in cycles of the walk from the cycle that the walk
    /// writes: each bound as a whole number over a whole number above
    /// zero, far inside their range.
    from: (i128, i128),
    to: (i128, i128),
    /// Whether the span has no width: it then meets the steps that hold
    /// its one time.
    instant: bool,
    /// The cycle that the walk writes.
    cycle: i64,
    /// The cycles of the pattern that one cycle of the walk takes.
    pace: Fraction,
    /// Where the text writes the speeds and the slow counts above the
    /// layer, the innermost first.
    after: &'a [At],
    each: &'a mut dyn FnMut(SourceStep),
    /// The last time that the walk made, and the bound that it is the
    /// time of: the next step often begins there.
    last: Option<((i64, i64), Fraction)>,
}

impl Asked<'_> {
    /// Whether `window` of the cycle that the walk writes ends before the
    /// first step that the span meets.
    fn is_past(&self, window: Window) -> bool {
        bound_to(window.end, window.parts, self.from).is_le()
    }

    /// Whether `window` begins after the last step that the span meets.
    fn is_ahead(&self, window: Window) -> bool {
        // A span of no width meets the steps that hold its one time.
        match self.instant {
            true => bound_to(window.begin, window.parts, self.from).is_gt(),
            false => bound_to(window.begin, window.parts, self.to).is_ge(),
        }
    }

    /// The time of `bound`, a bound of the `parts` equal parts of a cycle,
    /// in the cycle that the walk writes: in cycles of the pattern from
    /// the start of the period.
    fn time(&mut self, bound: i64, parts: i64) -> Option<Fraction> {
        let at = self.cycle.checked_mul(parts)?.checked_add(bound)?;
        if let Some((known, time)) = self.last
            && known == (at, parts)
        {
            return Some(time);
        }
        let numer = i128::from(at).checked_mul(self.pace.numer())?;
        let denom = i128::from(parts).checked_mul(self.pace.denom())?;
        let time = Fraction::checked_new(numer, denom)?;
        self.last = Some(((at, parts), time));
        Some(time)
    }

    /// Give the reader the step over `window` that holds the value at
    /// `value`, which the text writes at `written`, under `speeds`, when
    /// the span meets it: a step that holds the begin of the span, or
    /// that begins inside the span.
    fn step(
        &mut self,
        window: Window,
        value: u32,
        written: At,
        mut speeds: Option<&Speeds<'_>>,
    ) -> Option<()> {
        if self.is_past(window) || self.is_ahead(window) {
            return Some(());
        }
        let begin = self.time(window.begin, window.parts)?;
        let end = self.time(window.end, window.parts)?;
        // The place of the value, then the rates of the speeds that the
        // step is under, the innermost first, then those above its layer.
        let mut locations = Locations::new();
        locations.push(place(written));
        while let Some(speed) = speeds {
            locations.push(place(speed.at));
            speeds = speed.above;
        }
        for at in self.after {
            locations.push(place(*at));
        }
        (self.each)(SourceStep {
            begin,
            end,
            value,
            locations,
        });
        Some(())
    }
}

/// How `bound`, a bound of the `parts` equal parts of a cycle, lies to
/// the time `numer / denom`. Whole numbers compare, with no fraction made.
fn bound_to(bound: i64, parts: i64, (numer, denom): (i128, i128)) -> std::cmp::Ordering {
    (i128::from(bound) * denom).cmp(&(numer * i128::from(parts)))
}

/// The largest term of a bound of a span that a layer reads: a product
/// with a count of the parts of a cycle then stays in the range.
const MOST_SPAN_TERM: i128 = 1 << 80;

/// The least common multiple of two counts of cycles of one or more, up to
/// the largest term of a period: [`lcm`] in machine words.
fn common_turns(left: i64, right: i64) -> Option<i64> {
    if left == right {
        return Some(left);
    }
    let (mut divisor, mut rest) = (left, right);
    while rest != 0 {
        (divisor, rest) = (rest, divisor % rest);
    }
    let multiple = (left / divisor).checked_mul(right)?;
    (i128::from(multiple) <= MAX_PERIOD_TERM).then_some(multiple)
}

/// A construct of whole parts, as one walk for each cycle of its period
/// writes its rhythm: atoms, words with `:` between them, rests, sequences
/// of steps of whole weights, stacks, alternations, whole speeds, whole
/// slow counts and whole repeats.
///
/// The constructs build the same rhythm level by level, where each level
/// places the steps of the level under it again. A kind holds what a walk
/// reads of a construct, in few bytes: a pattern that reads its steps from
/// the plans keeps the kinds for as long as it plays.
#[derive(Clone, Copy)]
enum Kind {
    Rest,
    /// The place of a value among the values, and where the text writes
    /// it.
    Atom(u32, At),
    /// Two steps or more, one after the other.
    Sequence(Steps),
    /// The patterns of a stack, among the stacked plans.
    Stack(Among),
    /// Lanes, each the sequence of its steps at one unit of weight a
    /// cycle, among the lanes.
    Alternation(Among),
    /// The pattern at a whole rate, where the text writes the rate, and
    /// whether the pattern has one layer at most. A walk writes no layers
    /// at a speed: they answer one after the other.
    Faster(Id, u32, At, bool),
    /// The pattern, a whole count of times as slow, and where the text
    /// writes the count.
    Slower(Id, u32, At),
    /// Each cycle of the pattern, a whole count of times in the cycle.
    Repeated(Id, u32),
}

/// Where a plan is among the [`Plans`] of its text.
#[derive(Clone, Copy)]
struct Id(u32);

/// Where the text writes a word or a factor: two offsets in the score.
type At = (u32, u32);

/// The offsets of `at`, as a hap holds a location.
fn place(at: At) -> (usize, usize) {
    (at.0 as usize, at.1 as usize)
}

/// A run of one of the lists of [`Plans`]: its first entry, and the entry
/// after its last.
#[derive(Clone, Copy)]
struct Among(u32, u32);

impl Among {
    /// The run from `first` to the end of a list of `len` entries.
    fn from(first: usize, len: usize) -> Option<Self> {
        Some(Self(u32::try_from(first).ok()?, u32::try_from(len).ok()?))
    }

    fn range(self) -> Range<usize> {
        self.0 as usize..self.1 as usize
    }
}

/// The steps of a sequence, among the parts: each with the units of weight
/// that it fills, of `total` units.
#[derive(Clone, Copy)]
struct Steps {
    parts: Among,
    total: u32,
}

impl Steps {
    /// Whether each step has the weight one: the steps are then in one
    /// layer. Steps of other weights are a layer each, as they answer.
    fn are_equal(&self) -> bool {
        self.parts.range().len() == self.total as usize
    }
}

/// One step of a sequence: its plan, and the units of weight that it
/// fills, from the first to the one after the last.
type Part = (Id, (u32, u32));

/// What the planner knows of a construct while it plans the constructs
/// around it.
#[derive(Clone, Copy)]
struct Facts {
    /// The cycles after which the rhythm repeats: the common multiple of
    /// the periods of its layers.
    turns: i64,
    /// The step count that the pattern of the rhythm reports.
    steps: i64,
    /// How many layers the rhythm has.
    layers: u32,
    /// Whether this is a word as the text writes it, or words with `:`
    /// between them, with no brackets around it.
    word: bool,
}

/// What a walk of each cycle writes as one layer.
#[derive(Clone, Copy)]
enum Root {
    /// A plan, in a window of the cycle.
    Plan(Id, Window),
    /// The steps of a sequence, over the cycle.
    Steps(Steps),
}

/// One layer of the rhythm of a plan.
struct LayerPlan {
    root: Root,
    /// The cycles of the walk after which the layer repeats.
    turns: i64,
    /// The cycles of the pattern that one cycle of the walk takes. A lane
    /// of an alternation, a speed and a slow count each scale it for the
    /// layers under them.
    pace: Fraction,
    /// Where the text writes the speeds and the slow counts above the
    /// layer, the innermost first.
    after: Vec<At>,
}

impl LayerPlan {
    fn of(root: Root, turns: i64) -> Self {
        Self {
            root,
            turns,
            pace: Fraction::ONE,
            after: Vec::new(),
        }
    }

    /// The cycles of the pattern after which the layer repeats.
    fn period(&self) -> Option<Fraction> {
        Fraction::int(i128::from(self.turns)).checked_mul(self.pace)
    }

    /// Whether the layer holds a list of its `count` steps.
    fn is_listed(&self, count: usize) -> bool {
        let short = |period: Fraction| period < Fraction::int(READ_PERIOD);
        self.period().is_some_and(short) && count <= MAX_STEPS
    }
}

/// The cycles of a period from which a layer keeps no list of its steps,
/// and reads its plan at each query: a query meets few steps of a long
/// period, and each of them again only after the period. A list of a
/// short period makes each query faster.
const READ_PERIOD: i128 = 4;

/// The most equal parts of a cycle that a layer cuts its cycle into: a
/// window of the walk then stays far inside the range of its whole
/// numbers.
const MOST_PARTS: i64 = 1 << 40;

/// The most steps that a layer holds in a period: a fold that reads each
/// place makes that many.
const MOST_STEPS: i64 = 1 << 16;

/// What a walk of some cycles of a construct writes, for a query that
/// reads the steps from the plans.
struct Survey {
    /// How many steps the cycles hold.
    count: i64,
    /// Whether each step fills the window of the construct.
    fills: bool,
}

/// The plans of one text, each after the plans that it holds. A plan names
/// the plans under it by their places here, so the plans of a text are a
/// few lists, with no list for each construct.
#[derive(Default)]
struct Plans {
    kinds: Vec<Kind>,
    /// What the planner knows of each construct. A pattern that reads its
    /// steps from the plans keeps none of it.
    facts: Vec<Facts>,
    /// The steps of each sequence and of each lane, one after the other.
    parts: Vec<Part>,
    /// The patterns of each stack.
    stacked: Vec<Id>,
    /// The lanes of each alternation.
    lanes: Vec<Steps>,
    /// Whether a step may be longer than a cycle: a plan is a slow count,
    /// or a step of a lane fills more than one cycle.
    long: bool,
}

impl Plans {
    fn kind(&self, id: Id) -> Kind {
        self.kinds[id.0 as usize]
    }

    fn facts(&self, id: Id) -> Facts {
        self.facts[id.0 as usize]
    }

    fn add(&mut self, kind: Kind, facts: Facts) -> Option<Id> {
        let id = u32::try_from(self.kinds.len()).ok()?;
        self.kinds.push(kind);
        self.facts.push(facts);
        Some(Id(id))
    }

    fn parts(&self, steps: Steps) -> &[Part] {
        &self.parts[steps.parts.range()]
    }

    /// The steps of a sequence, each with its weight. `None` for a step
    /// that is no construct of whole parts, and for a sequence that marks
    /// itself as the source of the step count.
    fn steps(
        &mut self,
        steps: impl Iterator<Item = Option<(Id, u32)>>,
        marked: bool,
    ) -> Option<Steps> {
        if marked {
            return None;
        }
        let first = self.parts.len();
        let mut total = 0u32;
        for step in steps {
            let (plan, weight) = step?;
            let from = total;
            total = total.checked_add(weight)?;
            self.parts.push((plan, (from, total)));
        }
        Some(Steps {
            parts: Among::from(first, self.parts.len())?,
            total,
        })
    }

    /// The plan of the sequence of `steps`, which reports their weights as
    /// its step count. One step is the sequence, at any weight.
    fn sequence(&mut self, steps: Steps) -> Option<Id> {
        let total = i64::from(steps.total);
        let id = match *self.parts(steps) {
            [(plan, _)] => plan,
            _ => {
                let facts = Facts {
                    turns: self.turns(steps)?,
                    steps: total,
                    layers: self.layers(steps),
                    word: false,
                };
                self.add(Kind::Sequence(steps), facts)?
            }
        };
        let facts = &mut self.facts[id.0 as usize];
        (facts.steps, facts.word) = (total, false);
        Some(id)
    }

    /// The plan of patterns that sound together. `None` for a pattern
    /// that is no construct of whole parts, and for periods with no common
    /// multiple in the range of a period.
    fn stack(&mut self, patterns: impl Iterator<Item = Option<Id>>) -> Option<Id> {
        let first = self.stacked.len();
        let (mut turns, mut layers) = (1, 0u32);
        for pattern in patterns {
            let pattern = pattern?;
            let facts = self.facts(pattern);
            turns = common_turns(turns, facts.turns)?;
            layers = layers.checked_add(facts.layers)?;
            self.stacked.push(pattern);
        }
        let facts = Facts {
            turns,
            steps: 1,
            layers,
            word: false,
        };
        let patterns = Among::from(first, self.stacked.len())?;
        self.add(Kind::Stack(patterns), facts)
    }

    /// The plan of lanes that each take one step a cycle. `None` for a
    /// lane that is no construct of whole parts.
    fn alternation(&mut self, lanes: impl Iterator<Item = Option<Steps>>) -> Option<Id> {
        let first = self.lanes.len();
        let (mut turns, mut layers) = (1, 0u32);
        for lane in lanes {
            let lane = lane?;
            let sounding = self.layers(lane);
            // A lane with no value has no layer, and so no period: the
            // alternation holds the other lanes alone.
            if sounding == 0 {
                continue;
            }
            let own = self.turns(lane)?.checked_mul(i64::from(lane.total))?;
            turns = common_turns(turns, own)?;
            layers = layers.checked_add(sounding)?;
            // A step of a weight above one fills as many cycles, but for
            // a `!` of the weight, which fills each with one of its own.
            let fills = |(plan, (from, to)): &Part| self.lane_step(*plan, to - from).is_some();
            self.long |= !self.parts(lane).iter().all(fills);
            self.lanes.push(lane);
        }
        let facts = Facts {
            turns,
            steps: 1,
            layers,
            word: false,
        };
        let lanes = Among::from(first, self.lanes.len())?;
        self.add(Kind::Alternation(lanes), facts)
    }

    /// The whole count that the atom at `factor` writes, and where the
    /// text writes it. `None` for a factor that is no whole number of one
    /// or more as one atom.
    fn whole_factor(&self, values: &[Value], factor: Id) -> Option<(u32, At)> {
        let Kind::Atom(value, at) = self.kind(factor) else {
            return None;
        };
        let count = crate::fraction_site_value(values.get(value as usize)?)?;
        Some((whole_count(count)?, at))
    }

    /// The plan of `pattern` at the speed that `factor` writes.
    fn faster(&mut self, values: &[Value], pattern: Id, factor: Id) -> Option<Id> {
        let (rate, at) = self.whole_factor(values, factor)?;
        let under = self.facts(pattern);
        // The period of each layer, divided by the rate.
        let period = Fraction::checked_new(i128::from(under.turns), i128::from(rate))?;
        let facts = Facts {
            turns: i64::try_from(period.numer()).ok()?,
            word: false,
            ..under
        };
        self.add(Kind::Faster(pattern, rate, at, under.layers <= 1), facts)
    }

    /// The plan of `pattern`, as many times as slow as `factor` writes.
    fn slower(&mut self, values: &[Value], pattern: Id, factor: Id) -> Option<Id> {
        let (times, at) = self.whole_factor(values, factor)?;
        let under = self.facts(pattern);
        let facts = Facts {
            turns: under.turns.checked_mul(i64::from(times))?,
            word: false,
            ..under
        };
        self.long = true;
        self.add(Kind::Slower(pattern, times, at), facts)
    }

    /// The plan of each cycle of `pattern`, `amount` times in the cycle.
    /// `None` for an amount that is no whole number of one or more.
    fn repeated(&mut self, pattern: Id, amount: f64) -> Option<Id> {
        let times = whole_count(Fraction::from_f64(amount)?)?;
        let under = self.facts(pattern);
        let facts = Facts {
            layers: under.layers.min(1),
            word: false,
            ..under
        };
        self.add(Kind::Repeated(pattern, times), facts)
    }

    /// The plan of the words of `chain` and then the word `last`, with `:`
    /// between them: one value, a list of the value of each word, at `at`,
    /// where the text writes them all. `None` for a side that is no word:
    /// a rest, and anything in brackets.
    fn tail(&mut self, values: &mut Vec<Value>, chain: Id, last: Id, at: At) -> Option<Id> {
        let word = |plan: Id| match (self.kind(plan), self.facts(plan).word) {
            (Kind::Atom(value, _), true) => Some(value as usize),
            _ => None,
        };
        let (words, last) = (word(chain)?, word(last)?);
        // A handle of each value, for the list.
        let last = values.get(last)?.clone();
        let list = match values.get(words)?.clone() {
            Value::List(mut list) => {
                list.push(last);
                list
            }
            first => vec![first, last],
        };
        let held = u32::try_from(values.len()).ok()?;
        values.push(Value::List(list));
        self.kinds[chain.0 as usize] = Kind::Atom(held, at);
        Some(chain)
    }

    /// The common multiple of the periods of `steps`.
    fn turns(&self, steps: Steps) -> Option<i64> {
        let turns = |turns, (plan, _): &Part| common_turns(turns, self.facts(*plan).turns);
        self.parts(steps).iter().try_fold(1, turns)
    }

    /// How many layers the sequence of `steps` has: those of its one
    /// step, one layer for steps of the weight one, and a layer for each
    /// step that holds a value otherwise.
    fn layers(&self, steps: Steps) -> u32 {
        let parts = self.parts(steps);
        let sounding = parts
            .iter()
            .filter(|(plan, _)| self.facts(*plan).layers > 0);
        // The steps of a text are far fewer than the range holds.
        let sounding = sounding.count() as u32;
        match parts {
            [(plan, _)] => self.facts(*plan).layers,
            _ if steps.are_equal() => sounding.min(1),
            _ => sounding,
        }
    }

    /// The plan that the step `plan` of a lane writes in each of the
    /// cycles that its `weight` gives it: the step itself at the weight
    /// one, a rest at any weight, and the pattern of a `!` of the weight.
    /// `None` for any other step, which fills its cycles as one.
    fn lane_step(&self, plan: Id, weight: u32) -> Option<Id> {
        match (weight, self.kind(plan)) {
            (1, _) | (_, Kind::Rest) => Some(plan),
            (weight, Kind::Repeated(pattern, times)) if times == weight => Some(pattern),
            _ => None,
        }
    }

    /// Give `asked` the steps of cycle `cycle` of the construct at `id`
    /// that its span meets, in `window` of the cycle that the walk writes,
    /// in the order of time. `None` for what one walk does not write so:
    /// a step that a cycle would cut, and layers at a speed, which answer
    /// one after the other.
    fn walk(
        &self,
        id: Id,
        cycle: i64,
        window: Window,
        speeds: Option<&Speeds<'_>>,
        asked: &mut Asked<'_>,
    ) -> Option<()> {
        match self.kind(id) {
            Kind::Rest => Some(()),
            Kind::Atom(value, at) => asked.step(window, value, at, speeds),
            Kind::Sequence(steps) => self.walk_steps(steps, cycle, window, speeds, asked),
            Kind::Stack(patterns) => self.stacked[patterns.range()]
                .iter()
                .try_for_each(|pattern| self.walk(*pattern, cycle, window, speeds, asked)),
            Kind::Alternation(lanes) => self.lanes[lanes.range()]
                .iter()
                .try_for_each(|lane| self.walk_lane(*lane, cycle, window, speeds, asked)),
            Kind::Faster(pattern, rate, at, one_layer) => {
                if rate > 1 && !one_layer {
                    return None;
                }
                let rate = i64::from(rate);
                let speeds = Speeds { at, above: speeds };
                let first = cycle.checked_mul(rate)?;
                for part in first_part(window, rate, asked)?..rate {
                    let window = window.parts((part, part + 1), rate)?;
                    if asked.is_ahead(window) {
                        break;
                    }
                    let cycle = first.checked_add(part)?;
                    self.walk(pattern, cycle, window, Some(&speeds), asked)?;
                }
                Some(())
            }
            // A cycle cuts the steps of a slow pattern.
            Kind::Slower(..) => None,
            // The same cycle of the pattern, each time.
            Kind::Repeated(pattern, times) => {
                let times = i64::from(times);
                for part in first_part(window, times, asked)?..times {
                    let window = window.parts((part, part + 1), times)?;
                    if asked.is_ahead(window) {
                        break;
                    }
                    self.walk(pattern, cycle, window, speeds, asked)?;
                }
                Some(())
            }
        }
    }

    /// [`Self::walk`] for the sequence of `steps`.
    fn walk_steps(
        &self,
        steps: Steps,
        cycle: i64,
        window: Window,
        speeds: Option<&Speeds<'_>>,
        asked: &mut Asked<'_>,
    ) -> Option<()> {
        let parts = match self.parts(steps) {
            [(plan, _)] => return self.walk(*plan, cycle, window, speeds, asked),
            parts => parts,
        };
        let total = i64::from(steps.total);
        let part = |(from, to): (u32, u32)| window.parts((i64::from(from), i64::from(to)), total);
        // The steps are in the order of time: those before the first
        // step that the span meets are passed with no walk of them.
        let past = |(_, units): &Part| part(*units).is_some_and(|window| asked.is_past(window));
        for (plan, units) in &parts[parts.partition_point(past)..] {
            let window = part(*units)?;
            if asked.is_ahead(window) {
                break;
            }
            self.walk(*plan, cycle, window, speeds, asked)?;
        }
        Some(())
    }

    /// [`Self::walk`] for the lane of `steps`: the step whose turn the
    /// cycle is, at its next cycle. A step of a weight above one fills as
    /// many cycles: a `!` of that count fills each with one cycle of its
    /// pattern, and each cycle cuts any other step.
    fn walk_lane(
        &self,
        steps: Steps,
        cycle: i64,
        window: Window,
        speeds: Option<&Speeds<'_>>,
        asked: &mut Asked<'_>,
    ) -> Option<()> {
        if steps.total == 0 {
            return Some(());
        }
        let total = i64::from(steps.total);
        let (turn, unit) = (cycle.div_euclid(total), cycle.rem_euclid(total));
        let parts = self.parts(steps);
        let at = parts.partition_point(|(_, (_, to))| i64::from(*to) <= unit);
        let (plan, (from, to)) = parts.get(at)?;
        self.walk(
            self.lane_step(*plan, to - from)?,
            turn,
            window,
            speeds,
            asked,
        )
    }

    /// [`Self::walk`] for what a layer writes.
    fn walk_root(&self, root: Root, cycle: i64, asked: &mut Asked<'_>) -> Option<()> {
        match root {
            Root::Plan(id, window) => self.walk(id, cycle, window, None, asked),
            Root::Steps(steps) => self.walk_steps(steps, cycle, Window::CYCLE, None, asked),
        }
    }

    /// The layers of the rhythm of the construct at `id`, after those of
    /// `out`. A construct with no value has no layer.
    fn layers_onto(&self, id: Id, out: &mut Vec<LayerPlan>) -> Option<()> {
        let small = |period: &Fraction| {
            period.numer() <= MAX_PERIOD_TERM && period.denom() <= MAX_PERIOD_TERM
        };
        // The layers from `from` on, `factor` times as slow.
        let slower = |layers: &mut [LayerPlan], factor: Fraction| {
            layers.iter_mut().try_for_each(|layer| {
                layer.pace = layer.pace.checked_mul(factor)?;
                layer.period().filter(small).map(drop)
            })
        };
        let facts = self.facts(id);
        if facts.layers == 0 {
            return Some(());
        }
        match self.kind(id) {
            Kind::Rest => Some(()),
            Kind::Sequence(steps) => self.layers_of_steps(steps, out),
            Kind::Stack(patterns) => self.stacked[patterns.range()]
                .iter()
                .try_for_each(|pattern| self.layers_onto(*pattern, out)),
            Kind::Alternation(lanes) => self.lanes[lanes.range()].iter().try_for_each(|lane| {
                let from = out.len();
                self.layers_of_steps(*lane, out)?;
                slower(&mut out[from..], Fraction::int(i128::from(lane.total)))
            }),
            kind @ (Kind::Faster(pattern, count, at, _) | Kind::Slower(pattern, count, at)) => {
                let from = out.len();
                self.layers_onto(pattern, out)?;
                let count = Fraction::int(i128::from(count));
                // A speed divides the period of each layer, and a slow
                // count multiplies it.
                let longer = match kind {
                    Kind::Faster(..) => Fraction::ONE.checked_div(count)?,
                    _ => count,
                };
                slower(&mut out[from..], longer)?;
                // The place of the factor comes after those of a value.
                for layer in &mut out[from..] {
                    layer.after.push(at);
                }
                Some(())
            }
            // An atom, and a repeat: one layer of each cycle.
            Kind::Atom(..) | Kind::Repeated(..) => {
                out.push(LayerPlan::of(Root::Plan(id, Window::CYCLE), facts.turns));
                Some(())
            }
        }
    }

    /// The layers of the sequence of `steps`, after those of `out`: those
    /// of its one step, one layer for steps of the weight one, and a layer
    /// for each step otherwise.
    fn layers_of_steps(&self, steps: Steps, out: &mut Vec<LayerPlan>) -> Option<()> {
        if self.layers(steps) == 0 {
            return Some(());
        }
        match self.parts(steps) {
            [(plan, _)] => self.layers_onto(*plan, out),
            _ if steps.are_equal() => {
                out.push(LayerPlan::of(Root::Steps(steps), self.turns(steps)?));
                Some(())
            }
            parts => parts.iter().try_for_each(|(plan, (from, to))| {
                let units = (i64::from(*from), i64::from(*to));
                let window = Window::CYCLE.parts(units, i64::from(steps.total))?;
                let facts = self.facts(*plan);
                if facts.layers > 0 {
                    out.push(LayerPlan::of(Root::Plan(*plan, window), facts.turns));
                }
                Some(())
            }),
        }
    }

    /// How many steps `layer` holds in a period, when a walk writes each
    /// cycle whole and in the order of time. `None` for any other layer,
    /// and for more steps than a layer holds.
    fn reads(&self, layer: &LayerPlan) -> Option<usize> {
        let count = match layer.root {
            Root::Plan(id, window) => self.survey(id, layer.turns, window.parts)?.count,
            Root::Steps(steps) => self.survey_steps(steps, layer.turns, 1)?,
        };
        usize::try_from(count).ok().filter(|_| count <= MOST_STEPS)
    }

    /// What a walk of `cycles` cycles of the construct at `id` writes,
    /// from cycle zero, in windows of `parts` equal parts of a cycle.
    /// `None` when a walk does not write each cycle whole and in the order
    /// of time, or cuts a cycle into too many parts.
    fn survey(&self, id: Id, cycles: i64, parts: i64) -> Option<Survey> {
        if parts > MOST_PARTS {
            return None;
        }
        Some(match self.kind(id) {
            Kind::Rest => Survey {
                count: 0,
                fills: true,
            },
            Kind::Atom(..) => Survey {
                count: cycles,
                fills: true,
            },
            Kind::Sequence(steps) => Survey {
                count: self.survey_steps(steps, cycles, parts)?,
                fills: false,
            },
            Kind::Stack(patterns) => {
                let patterns = self.stacked[patterns.range()].iter();
                Survey {
                    count: together(patterns.map(|pattern| self.survey(*pattern, cycles, parts)))?,
                    fills: true,
                }
            }
            Kind::Alternation(lanes) => match &self.lanes[lanes.range()] {
                [lane] => self.survey_lane(*lane, cycles, parts)?,
                lanes => Survey {
                    count: together(
                        lanes
                            .iter()
                            .map(|lane| self.survey_lane(*lane, cycles, parts)),
                    )?,
                    fills: true,
                },
            },
            Kind::Faster(pattern, rate, _, one_layer) => {
                if rate > 1 && !one_layer {
                    return None;
                }
                let rate = i64::from(rate);
                let under =
                    self.survey(pattern, cycles.checked_mul(rate)?, parts.checked_mul(rate)?)?;
                Survey {
                    fills: rate == 1 && under.fills,
                    ..under
                }
            }
            Kind::Repeated(pattern, times) => {
                let times = i64::from(times);
                let under = self.survey(pattern, cycles, parts.checked_mul(times)?)?;
                Survey {
                    count: under.count.checked_mul(times)?,
                    fills: times == 1 && under.fills,
                }
            }
            Kind::Slower(..) => return None,
        })
    }

    /// How many steps `cycles` cycles of the sequence of `steps` hold. See
    /// [`Self::survey`]: a sequence is in order when each step is.
    fn survey_steps(&self, steps: Steps, cycles: i64, parts: i64) -> Option<i64> {
        let list = self.parts(steps);
        let parts = match list.len() {
            1 => parts,
            _ => parts.checked_mul(i64::from(steps.total))?,
        };
        sum(list
            .iter()
            .map(|(plan, _)| Some(self.survey(*plan, cycles, parts)?.count)))
    }

    /// [`Self::survey`] for the lane of `steps`.
    fn survey_lane(&self, steps: Steps, cycles: i64, parts: i64) -> Option<Survey> {
        let mut lane = Survey {
            count: 0,
            fills: true,
        };
        let total = i64::from(steps.total);
        // The cycles hold each step of the lane a whole count of times.
        if cycles % total != 0 {
            return None;
        }
        for (plan, (from, to)) in self.parts(steps) {
            let weight = to - from;
            let step = self.survey(self.lane_step(*plan, weight)?, cycles / total, parts)?;
            // Each cycle of the step writes the same cycle of its plan.
            let count = step.count.checked_mul(i64::from(weight))?;
            lane = Survey {
                count: lane.count.checked_add(count)?,
                fills: lane.fills && step.fills,
            };
        }
        Some(lane)
    }

    /// Give each atom under `id` the place of its value among the values
    /// of its layer, which `held` lists by their places among the values
    /// of the text. `local` holds, for each value of the text, its place
    /// in the layer and one more, or zero for a value that the layer does
    /// not hold yet.
    fn localize(&mut self, id: Id, local: &mut [u32], held: &mut Vec<u32>) {
        match self.kind(id) {
            Kind::Rest => {}
            Kind::Atom(value, at) => {
                let place = &mut local[value as usize];
                if *place == 0 {
                    held.push(value);
                    *place = held.len() as u32;
                }
                self.kinds[id.0 as usize] = Kind::Atom(*place - 1, at);
            }
            Kind::Sequence(steps) => self.localize_steps(steps, local, held),
            Kind::Stack(patterns) => {
                for pattern in patterns.range() {
                    self.localize(self.stacked[pattern], local, held);
                }
            }
            Kind::Alternation(lanes) => {
                for lane in lanes.range() {
                    self.localize_steps(self.lanes[lane], local, held);
                }
            }
            Kind::Faster(pattern, ..) | Kind::Slower(pattern, ..) | Kind::Repeated(pattern, _) => {
                self.localize(pattern, local, held);
            }
        }
    }

    /// [`Self::localize`] for each step of `steps`.
    fn localize_steps(&mut self, steps: Steps, local: &mut [u32], held: &mut Vec<u32>) {
        for part in steps.parts.range() {
            self.localize(self.parts[part].0, local, held);
        }
    }

    /// The values of `layer`, each one time, after [`Self::localize`] gave
    /// each of its atoms its place among them.
    fn values_of(
        &mut self,
        layer: &LayerPlan,
        values: &[Value],
        local: &mut Vec<u32>,
    ) -> Arc<[Value]> {
        local.resize(values.len(), 0);
        let mut held = Vec::new();
        match layer.root {
            Root::Plan(id, _) => self.localize(id, local, &mut held),
            Root::Steps(steps) => self.localize_steps(steps, local, &mut held),
        }
        // The next layer holds none of these values yet.
        for value in &held {
            local[*value as usize] = 0;
        }
        // A handle of each value, for the layer.
        let values = held.iter().map(|value| values[*value as usize].clone());
        values.collect()
    }

    /// Give `each` the steps of `layer` that meet the span from `begin`
    /// to `end`, in the order of time: a step that holds `begin`, and a
    /// step that begins after `begin` and before `end`. One cycle of the
    /// walk takes `pace` of the unit of the span, and each step is at its
    /// time in that unit.
    fn read(
        &self,
        layer: &LayerPlan,
        pace: Fraction,
        begin: Fraction,
        end: Fraction,
        each: &mut dyn FnMut(SourceStep),
    ) -> Option<()> {
        let LayerPlan {
            root, turns, after, ..
        } = layer;
        // The span in cycles of the walk: each bound over the pace, as a
        // whole number over a whole number, with no fraction made.
        let walked = |time: Fraction| {
            let numer = time.numer().checked_mul(pace.denom())?;
            let denom = time.denom().checked_mul(pace.numer())?;
            (numer.abs() <= MOST_SPAN_TERM && denom <= MOST_SPAN_TERM).then_some((numer, denom))
        };
        let (from, to) = (walked(begin)?, walked(end)?);
        let instant = begin == end;
        let first = from.0.div_euclid(from.1).max(0);
        let last = match instant {
            true => first + 1,
            // The cycle after the one that holds the last time before
            // the end.
            false => -(-to.0).div_euclid(to.1),
        };
        let mut asked = Asked {
            from,
            to,
            instant,
            cycle: 0,
            pace,
            after,
            each,
            last: None,
        };
        for cycle in first..last.min(i128::from(*turns)) {
            // The span, from the cycle that the walk writes.
            let back = |(numer, denom): (i128, i128)| (numer - cycle * denom, denom);
            (asked.from, asked.to) = (back(from), back(to));
            asked.cycle = i64::try_from(cycle).ok()?;
            // A step of a sequence with weights sounds in its own window
            // of the cycle only.
            if let Root::Plan(_, window) = root
                && (asked.is_past(*window) || asked.is_ahead(*window))
            {
                continue;
            }
            self.walk_root(*root, asked.cycle, &mut asked)?;
        }
        Some(())
    }

    /// The steps of `layer` in a list, each as a share of its period,
    /// with its value among `values`.
    fn listed(
        &self,
        layer: &LayerPlan,
        count: usize,
        values: &[Value],
    ) -> Option<Vec<SequenceStep>> {
        let mut steps = Vec::with_capacity(count);
        // One cycle of the walk is one of the equal shares of the period.
        let share = Fraction::checked_new(1, i128::from(layer.turns))?;
        self.read(layer, share, Fraction::ZERO, Fraction::ONE, &mut |step| {
            // A handle of the value, for the step.
            let value = values[step.value as usize].clone();
            let locations = step.locations;
            steps.push(SequenceStep::new(step.begin, step.end, value, locations));
        })?;
        Some(steps)
    }

    /// These plans with what a walk reads of them, and no more: a pattern
    /// keeps them for as long as it plays.
    fn for_walks(mut self) -> Self {
        self.kinds.shrink_to_fit();
        self.parts.shrink_to_fit();
        Self {
            facts: Vec::new(),
            ..self
        }
    }
}

/// One layer of a plan as a [`StepSource`]: a query walks the plan over
/// the cycles that its span meets, with no list of the steps.
struct PlanLayer {
    plans: Arc<Plans>,
    layer: LayerPlan,
    /// How many steps a period holds.
    count: usize,
}

impl StepSource for PlanLayer {
    fn count(&self) -> usize {
        self.count
    }

    fn steps(
        &self,
        begin: Fraction,
        end: Fraction,
        each: &mut dyn FnMut(SourceStep),
    ) -> Option<()> {
        self.plans
            .read(&self.layer, self.layer.pace, begin, end, each)
    }
}

/// The room that a read of a text takes: its plans, the value of each
/// atom, and the words that it wrote. A reader of many texts keeps one
/// room, so a text takes new room only for the plans that its pattern
/// keeps.
#[derive(Default)]
pub struct Room {
    plans: Plans,
    /// The value of each atom.
    values: Vec<Value>,
    /// The words that the parse read, each one time: where the text
    /// writes it first, and the place of its value among the values.
    words: Vec<((usize, usize), u32)>,
    /// Where each word is among `words`, and one more, at a slot near the
    /// hash of the word. A slot of zero holds no word. The slots are made
    /// when the text writes a second word.
    known: Vec<u32>,
}

impl Room {
    /// Whether the room holds no list: a new room, or one that a reader
    /// put away.
    pub fn is_bare(&self) -> bool {
        let Plans {
            kinds,
            facts,
            parts,
            stacked,
            lanes,
            long: _,
        } = &self.plans;
        [
            kinds.capacity(),
            facts.capacity(),
            parts.capacity(),
            stacked.capacity(),
            lanes.capacity(),
            self.values.capacity(),
            self.words.capacity(),
            self.known.capacity(),
        ]
        .iter()
        .all(|held| *held == 0)
    }

    /// Empty the room for the plans of `src`, with none to move: a word
    /// and its space are two bytes or more.
    fn clear(&mut self, src: &str) {
        let Plans {
            kinds,
            facts,
            parts,
            stacked,
            lanes,
            long,
        } = &mut self.plans;
        let words = src.len() / 2 + 1;
        kinds.clear();
        kinds.reserve(words);
        facts.clear();
        facts.reserve(words);
        parts.clear();
        parts.reserve(words);
        stacked.clear();
        lanes.clear();
        *long = false;
        self.values.clear();
        self.words.clear();
        self.known.clear();
    }
}

/// The plans that the parse of a text of whole parts made in a room, and
/// the plan of the whole text.
pub(crate) struct Planned<'r> {
    room: &'r mut Room,
    root: Id,
}

impl<'r> Planned<'r> {
    /// The plans of `src`, which begins `offset` bytes after its opening
    /// quote in a score, when it is a construct of whole parts: atoms,
    /// words with `:` between them, rests, sequences of steps of whole
    /// weights, stacks, alternations, whole speeds, whole slow counts and
    /// whole repeats. The parse of the text makes the plans, with no
    /// syntax tree. `Ok(None)` for any other construct.
    pub(crate) fn read(
        src: &str,
        offset: usize,
        room: &'r mut Room,
    ) -> Result<Option<Self>, ParseError> {
        room.clear(src);
        let planner = Planner { src, offset, room };
        let (root, planner) = crate::read(src, planner)?;
        let room = planner.room;
        Ok(root.map(|root| Self { room, root }))
    }

    /// The step count that the pattern of the text reports.
    fn steps(&self) -> Option<Fraction> {
        let steps = self.room.plans.facts(self.root).steps;
        Some(Fraction::int(i128::from(steps)))
    }

    /// The layers of the text, each with the count of its steps. `None`
    /// when a walk does not write each cycle of a layer whole and in the
    /// order of time. See [`Plans::reads`].
    fn layers(&self) -> Option<Vec<(LayerPlan, usize)>> {
        let plans = &self.room.plans;
        let mut layers = Vec::new();
        plans.layers_onto(self.root, &mut layers)?;
        let counted = layers.into_iter().map(|layer| {
            let count = plans.reads(&layer)?;
            Some((layer, count))
        });
        counted.collect()
    }

    /// Whether a layer of the text reads the plans at each query.
    #[cfg(test)]
    pub(crate) fn reads_plans(&self) -> bool {
        let reads = |(layer, count): &(LayerPlan, usize)| !layer.is_listed(*count);
        self.layers().is_some_and(|layers| layers.iter().any(reads))
    }

    /// The pattern of the text: one atom is a constant, as it is written.
    /// Any other text is a sequence. A layer of a short period holds a
    /// list of its steps, and a layer of a long period reads the plans at
    /// each query. `None` when a walk does not write each cycle of a
    /// layer whole and in the order of time: the constructs then build
    /// the pattern level by level.
    pub(crate) fn pattern(self) -> Option<Pattern> {
        let steps = self.steps();
        let layers = match self.room.plans.kind(self.root) {
            Kind::Atom(value, at) => {
                let value = self.room.values.swap_remove(value as usize);
                return Some(constant(value, vec![place(at)]).with_steps(steps));
            }
            _ => self.layers()?,
        };
        let Room { plans, values, .. } = self.room;
        let in_cycles = !plans.long;
        // Each layer that reads the plans holds its own values.
        let mut local = Vec::new();
        let mut own = Vec::new();
        for (layer, count) in &layers {
            if !layer.is_listed(*count) {
                own.push(plans.values_of(layer, values, &mut local));
            }
        }
        // A layer that reads the plans keeps them for as long as it
        // plays: the room then makes new ones.
        let kept = (!own.is_empty()).then(|| Arc::new(std::mem::take(plans).for_walks()));
        let plans = kept.as_deref().unwrap_or(plans);
        let mut own = own.into_iter();
        let given = layers.into_iter().map(|(layer, count)| {
            let period = layer.period()?;
            if layer.is_listed(count) {
                let steps = plans.listed(&layer, count, values)?;
                return Some(GivenLayer::Listed(SequenceLayer { period, steps }));
            }
            Some(GivenLayer::Sourced(SourcedLayer {
                period,
                values: own.next()?,
                in_cycles,
                steps: Arc::new(PlanLayer {
                    // A handle of the plans, for each layer.
                    plans: Arc::clone(kept.as_ref()?),
                    layer,
                    count,
                }),
            }))
        });
        Some(sequence_of(given.collect::<Option<_>>()?)?.with_steps(steps))
    }
}

/// The first of the `count` equal parts of `window` that does not end
/// before the first step that the span of `asked` meets.
fn first_part(window: Window, count: i64, asked: &Asked<'_>) -> Option<i64> {
    let (mut low, mut high) = (0, count);
    while low < high {
        let middle = low + (high - low) / 2;
        match asked.is_past(window.parts((middle, middle + 1), count)?) {
            true => low = middle + 1,
            false => high = middle,
        }
    }
    Some(low)
}

/// How many steps constructs that sound together hold. They are in order
/// when each fills the window: their steps then begin and end together.
/// `None` for one that does not, or that a query may not read.
fn together(mut surveys: impl Iterator<Item = Option<Survey>>) -> Option<i64> {
    surveys.try_fold(0i64, |count, survey| {
        let survey = survey.filter(|survey| survey.fills)?;
        count.checked_add(survey.count)
    })
}

/// The sum of `counts`. `None` for a count that is none, and for a sum
/// outside the range.
fn sum(mut counts: impl Iterator<Item = Option<i64>>) -> Option<i64> {
    counts.try_fold(0i64, |sum, count| sum.checked_add(count?))
}

/// A whole count of one or more, in the range of a period.
fn whole_count(count: Fraction) -> Option<u32> {
    let whole = count.denom() == 1 && count >= Fraction::ONE && count.numer() <= MAX_PERIOD_TERM;
    whole.then(|| u32::try_from(count.numer()).ok()).flatten()
}

/// The builder of the [`Plans`] of a text: what a parse makes of a
/// construct of whole parts. `None` for any other construct, and for each
/// construct around it.
struct Planner<'a, 'r> {
    /// The text of the pattern, and where it begins in its score.
    src: &'a str,
    offset: usize,
    room: &'r mut Room,
}

impl<'a> Planner<'a, '_> {
    /// The place of the value of `word`, which ends at `end` of the text,
    /// among the values: a word that the text writes again has the value
    /// that it made the first time.
    fn value(&mut self, word: &'a str, end: usize) -> Option<u32> {
        if self.room.words.is_empty() {
            return self.learn(word, end);
        }
        if self.room.known.is_empty() {
            // Two slots for each word that the text may hold: a word and
            // its space are two bytes or more.
            let slots = (self.src.len() + 2).next_power_of_two();
            self.room.known.resize(slots, 0);
            let first = self.slot(self.word(0));
            self.room.known[first] = 1;
        }
        let mut slot = self.slot(word);
        loop {
            match self.room.known[slot] {
                0 => {
                    let value = self.learn(word, end)?;
                    self.room.known[slot] = u32::try_from(self.room.words.len()).ok()?;
                    return Some(value);
                }
                known if self.word(known as usize - 1) == word => {
                    return Some(self.room.words[known as usize - 1].1);
                }
                _ => slot = (slot + 1) & (self.room.known.len() - 1),
            }
        }
    }

    /// The word at `place` among the words, as the text writes it.
    fn word(&self, place: usize) -> &'a str {
        let ((start, end), _) = self.room.words[place];
        &self.src[start..end]
    }

    /// The slot that the hash of `word` names.
    fn slot(&self, word: &str) -> usize {
        // The FNV hash of the bytes, which mixes into its high bits.
        let hash = word.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
        (hash >> 32) as usize & (self.room.known.len() - 1)
    }

    /// The place of the value of a word that the text writes for the
    /// first time, and that ends at `end` of the text.
    fn learn(&mut self, word: &'a str, end: usize) -> Option<u32> {
        let value = u32::try_from(self.room.values.len()).ok()?;
        self.room.values.push(crate::atom_value(word));
        self.room.words.push(((end - word.len(), end), value));
        Some(value)
    }
}

/// Where the leaf at `span` of `src` is in the score: `src` begins
/// `offset` bytes after its opening quote.
fn written_at(src: &str, offset: usize, span: &Span) -> Option<At> {
    let (begin, end) = crate::leaf_place(src, offset, span);
    Some((u32::try_from(begin).ok()?, u32::try_from(end).ok()?))
}

impl<'a> Build<'a> for Planner<'a, '_> {
    type Node = Option<Id>;
    type Step = Option<(Id, u32)>;
    type Lane = Option<Steps>;

    fn atom(&mut self, word: &'a str, _: Span, span: Span) -> Option<Id> {
        let kind = match word {
            "-" | "~" => Kind::Rest,
            _ => {
                let at = written_at(self.src, self.offset, &span)?;
                Kind::Atom(self.value(word, span.end)?, at)
            }
        };
        let facts = Facts {
            turns: 1,
            steps: 1,
            layers: u32::from(matches!(kind, Kind::Atom(..))),
            word: true,
        };
        self.room.plans.add(kind, facts)
    }

    fn step(&mut self, node: Option<Id>, _: Span, weight: f64) -> Option<(Id, u32)> {
        let whole = weight >= 1.0 && weight.fract() == 0.0 && weight <= MAX_PERIOD_TERM as f64;
        // A weight in the range of a period is a whole number exactly.
        whole.then(|| Some((node?, weight as u32))).flatten()
    }

    fn sequence(
        &mut self,
        steps: impl ExactSizeIterator<Item = Self::Step>,
        marked: bool,
        _: Span,
    ) -> Option<Id> {
        let steps = self.room.plans.steps(steps, marked)?;
        self.room.plans.sequence(steps)
    }

    fn lane(
        &mut self,
        steps: impl ExactSizeIterator<Item = Self::Step>,
        marked: bool,
    ) -> Option<Steps> {
        self.room.plans.steps(steps, marked)
    }

    fn alternation(&mut self, lanes: Vec<Option<Steps>>, _: Span) -> Option<Id> {
        self.room.plans.alternation(lanes.into_iter())
    }

    fn stack(&mut self, items: Vec<Option<Id>>, _: Span) -> Option<Id> {
        self.room.plans.stack(items.into_iter())
    }

    fn choice(&mut self, _: Vec<Option<Id>>, _: u32, _: Span) -> Option<Id> {
        None
    }

    fn feet(&mut self, _: Vec<Option<Id>>, _: u32, _: Span) -> Option<Id> {
        None
    }

    fn polymeter(&mut self, _: Vec<Option<Id>>, _: Option<Option<Id>>, _: Span) -> Option<Id> {
        None
    }

    fn postfix(
        &mut self,
        pattern: Option<Id>,
        operator: Postfix<Self::Node, Self::Step>,
        span: Span,
    ) -> Option<Id> {
        let Room { plans, values, .. } = &mut *self.room;
        match operator {
            Postfix::Fast(factor) => plans.faster(values, pattern?, factor?),
            Postfix::Slow(factor) => plans.slower(values, pattern?, factor?),
            Postfix::Replicate(amount) => plans.repeated(pattern?, amount),
            Postfix::Tail(element) => {
                let at = written_at(self.src, self.offset, &span)?;
                plans.tail(values, pattern?, element?, at)
            }
            _ => None,
        }
    }
}

/// The layer of `steps`, which repeat every `turns` cycles, or no layer for
/// no steps. `None` for steps that are not in the order of time, which one
/// layer does not hold.
fn layer(turns: i128, steps: Vec<SequenceStep>) -> Option<Vec<SequenceLayer>> {
    if steps.is_empty() {
        return Some(Vec::new());
    }
    let ordered = steps
        .windows(2)
        .all(|pair| pair[0].begin() <= pair[1].begin() && pair[0].end() <= pair[1].end());
    ordered.then(|| {
        vec![SequenceLayer {
            period: Fraction::int(turns),
            steps,
        }]
    })
}

fn whole(count: usize) -> Option<Fraction> {
    i128::try_from(count).ok().map(Fraction::int)
}

/// The least common multiple of two counts of cycles, up to the largest
/// term of a period.
fn lcm(left: i128, right: i128) -> Option<i128> {
    let multiple = Fraction::int(left)
        .checked_lcm(Fraction::int(right))?
        .numer();
    (multiple <= MAX_PERIOD_TERM).then_some(multiple)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A word that a text writes again has the value that it made the
    /// first time: a text of many words holds one value for each word.
    #[test]
    fn a_word_that_a_text_writes_again_is_one_value() {
        let words = ["bd", "b", "bdd", "hh", "h", "sd"];
        let text: Vec<&str> = (0..48).map(|at| words[at * 5 % words.len()]).collect();
        let text = text.join(" ");
        let mut room = Room::default();
        let read = Planned::read(&text, 0, &mut room);
        assert!(
            matches!(read, Ok(Some(_))),
            "{text} is a text of whole parts"
        );
        assert_eq!(room.values.len(), words.len());
    }
}
