//! The plain values of a mini-notation pattern, at places that are known
//! when the pattern is built.

use crate::{Build, ParseError, Postfix, Span};
use rustel_core::{
    Locations, Pattern, SequenceBuilder, SequenceLayer, SequenceStep, SourceStep, SourcedLayer,
    StepSource, TickCut, TickStep, Value, pure, sequence, sequence_of_cut_ticks,
    sequence_of_indexed_slice, sequence_of_sourced, sequence_of_ticks, silence,
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

    /// One cycle of already-placed steps over `count` equal slots (fast path
    /// for flat sequences of atoms and rests with no per-atom `Vec` churn).
    pub(crate) fn from_single_cycle_steps(steps: Vec<SequenceStep>, count: usize) -> Option<Self> {
        let count = whole(count)?;
        Some(Self {
            layers: layer(1, steps)?,
            steps: Some(count),
            atom: false,
        })
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
        let mut layers = Vec::with_capacity(parts.len());
        for (part, window) in parts.iter().zip(windows) {
            let Some(part) = part else {
                continue;
            };
            let inside = Fraction::ZERO <= window.0 && window.0 < window.1;
            if !(inside && window.1 <= Fraction::ONE) {
                return None;
            }
            let turns = part.turns()?;
            let held: usize = part.layers.iter().map(|layer| layer.steps.len()).sum();
            let room = held.saturating_mul(usize::try_from(turns).ok()?);
            let mut steps = Vec::with_capacity(room.min(MAX_STEPS));
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
        let pulses = mask.iter().filter(|&&set| set != 0).count();
        let est = pulses.saturating_mul(usize::try_from(turns).unwrap_or(1));
        let mut steps = Vec::with_capacity(est.min(MAX_STEPS));
        let mut cycle = Vec::new();
        for turn in 0..turns {
            // The steps of this cycle, at their places in it.
            cycle.clear();
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
        if let [layer] = self.layers.as_mut_slice() {
            let Some(period) = layer.period.checked_div(factor).filter(small) else {
                return Err(self);
            };
            layer.period = period;
            self.atom = false;
            return Ok(self);
        }
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
        let count_usize = named.steps.len();
        let mut stack_rates = [0i128; 16];
        let mut heap_rates: Vec<i128> = if count_usize > stack_rates.len() {
            Vec::with_capacity(count_usize)
        } else {
            Vec::new()
        };
        for (index, step) in named.steps.iter().enumerate() {
            let fills = (
                whole(index)?.checked_div(count)?,
                whole(index + 1)?.checked_div(count)?,
            );
            let rate = rate_of(step.value())?;
            let valid = step.whole() == fills
                && (step.begin(), step.end()) == fills
                && rate.denom() == 1
                && rate >= Fraction::ONE;
            if !valid {
                return None;
            }
            if count_usize <= stack_rates.len() {
                stack_rates[index] = rate.numer();
            } else {
                heap_rates.push(rate.numer());
            }
        }
        let rate_vals: &[i128] = if count_usize <= stack_rates.len() {
            &stack_rates[..count_usize]
        } else {
            &heap_rates
        };
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
        let own_per_cycle = if own.period == Fraction::ONE {
            own.steps.len()
        } else {
            (own.steps.len() / usize::try_from(own.period.numer()).unwrap_or(1).max(1)).max(1)
        };
        let mut est_steps = 0usize;
        for cycle in 0..turns {
            let idx = usize::try_from(cycle % count.numer()).ok()?;
            est_steps = est_steps.saturating_add(
                usize::try_from(rate_vals[idx])
                    .unwrap_or(1)
                    .saturating_mul(own_per_cycle),
            );
        }
        let mut steps = Vec::with_capacity(est_steps.min(1024));
        let single_unit_step = (own.period == Fraction::ONE && own.steps.len() == 1).then(|| {
            let step = &own.steps[0];
            (step.begin() == Fraction::ZERO
                && step.end() == Fraction::ONE
                && step.whole() == (Fraction::ZERO, Fraction::ONE))
                .then_some(step)
        });
        let single_unit_step = single_unit_step.flatten();
        for cycle in 0..turns {
            let idx = usize::try_from(cycle % count.numer()).ok()?;
            let rate = *rate_vals.get(idx)?;
            let at = named.steps[idx].locations();
            let has_at = !at.is_empty();
            // The cycles of this rhythm that the cycle holds, one after
            // the other.
            let first_turn = cycle.checked_mul(rate)?;
            let slot_denom = rate.checked_mul(turns)?;
            let scale = Fraction::checked_new(1, slot_denom)?;
            let mut slot_start = Fraction::checked_new(first_turn, slot_denom)?;
            if let Some(unit_step) = single_unit_step {
                let mut base = unit_step.clone();
                if has_at {
                    base.locate_first_locations(at);
                }
                for part in 0..rate {
                    if steps.len() == MAX_STEPS {
                        return None;
                    }
                    let slot_end = Fraction::checked_new(first_turn + part + 1, slot_denom)?;
                    let span = (slot_start, slot_end);
                    steps.push(base.placed(span, span));
                    slot_start = slot_end;
                }
                continue;
            }
            for part in 0..rate {
                let slot_end = Fraction::checked_new(first_turn + part + 1, slot_denom)?;
                let from = steps.len();
                let turn = first_turn.checked_add(part)?;
                self.squeeze_cycle_placed(turn, slot_start, slot_end, scale, &mut steps)?;
                // The locations of the rate come first on each hap.
                if has_at {
                    for step in &mut steps[from..] {
                        step.locate_first_locations(at);
                    }
                }
                slot_start = slot_end;
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
            let count_int = count.numer();
            let turns = self.turns()?;
            let slot_denom = count_int.checked_mul(turns)?;
            let scale = Fraction::checked_new(1, slot_denom)?;
            let held: usize = self.layers.iter().map(|layer| layer.steps.len()).sum();
            let est = held
                .saturating_mul(usize::try_from(turns).unwrap_or(1))
                .saturating_mul(usize::try_from(count_int).unwrap_or(1));
            let mut steps = Vec::with_capacity(est.min(MAX_STEPS));
            for turn in 0..turns {
                let base_slot = turn.checked_mul(count_int)?;
                let mut slot_start = Fraction::checked_new(base_slot, slot_denom)?;
                for part in 0..count_int {
                    let slot_end = Fraction::checked_new(base_slot + part + 1, slot_denom)?;
                    self.squeeze_cycle_placed(turn, slot_start, slot_end, scale, &mut steps)?;
                    slot_start = slot_end;
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
        let width = window.1.checked_sub(window.0)?;
        let offset = Fraction::int(target).checked_add(window.0)?;
        let turns = Fraction::int(turns);
        // The bounds of the cycle, at their places: most steps begin or end
        // on one.
        let first = offset.checked_div(turns)?;
        let last = offset.checked_add(width)?.checked_div(turns)?;
        // The width of the window, as a share of the period.
        let scale = width.checked_div(turns)?;
        self.squeeze_cycle_placed(turn, first, last, scale, out)
    }

    fn squeeze_cycle_placed(
        &self,
        turn: i128,
        first: Fraction,
        last: Fraction,
        scale: Fraction,
        out: &mut Vec<SequenceStep>,
    ) -> Option<()> {
        let cycle = Fraction::int(turn);
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
    match locations.len() {
        0 => pure(value),
        1 => rustel_core::pure_at(value, locations[0]),
        _ => {
            let mut locs = rustel_core::Locations::new();
            locs.extend_from_slice(&locations);
            rustel_core::pure_locations(value, locs)
        }
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
    pace: (i128, i128),
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
        let numer = i128::from(at).checked_mul(self.pace.0)?;
        let denom = i128::from(parts).checked_mul(self.pace.1)?;
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
    /// it: the offset of its begin and its length. A word is far shorter
    /// than the range of the length.
    Atom {
        value: u32,
        begin: u32,
        len: u16,
    },
    /// Two steps or more of the weight one, one after the other: their ids
    /// among the ids, which need no weights.
    EqualSequence(Among),
    /// Two steps or more, one after the other, of any weights: their parts
    /// among the parts.
    WeightedSequence(Among),
    /// The patterns of a stack, among the stacked plans.
    Stack(Among),
    /// The patterns of a stack that come one after the other among the
    /// plans: the first plan, and the plan after the last. They need no
    /// list.
    StackRun(Among),
    /// Lanes, each the sequence of its steps at one unit of weight a
    /// cycle, among the lanes.
    Alternation(Among),
    /// The pattern at a whole rate, where the text writes the rate among
    /// the sites, and whether the pattern has one layer at most. A walk
    /// writes no layers at a speed: they answer one after the other.
    Faster(Id, u32, u16, bool),
    /// The pattern, a whole count of times as slow, and where the text
    /// writes the count among the sites.
    Slower(Id, u32, u16),
    /// Each cycle of the pattern, a whole count of times in the cycle.
    Repeated(Id, u32),
}

impl Kind {
    /// The atom of the value at `value`, which the text writes at `at`.
    /// `None` for a word longer than the range of a length.
    fn atom(value: u32, at: At) -> Option<Self> {
        let len = u16::try_from(at.1.checked_sub(at.0)?).ok()?;
        Some(Self::Atom {
            value,
            begin: at.0,
            len,
        })
    }

    /// Whether the steps of a sequence kind have the weight one.
    fn is_equal(self) -> bool {
        matches!(self, Self::EqualSequence(_))
    }

    /// Whether the patterns of a stack kind are a run of plans.
    fn is_run(self) -> bool {
        matches!(self, Self::StackRun(_))
    }
}

/// Where the atom that begins at `begin` and is `len` long is written.
fn written(begin: u32, len: u16) -> At {
    (begin, begin + u32::from(len))
}

/// Compact 8-byte encoding of [`Kind`] stored in [`Plans::kinds`].
#[derive(Clone, Copy)]
struct PackedKind(u64);

const _: () = assert!(std::mem::size_of::<PackedKind>() == 8);

impl PackedKind {
    const TAG_SHIFT: u32 = 60;
    const REST: u64 = 0;
    const ATOM: u64 = 1;
    const EQUAL_SEQ: u64 = 2;
    const WEIGHTED_SEQ: u64 = 3;
    const STACK: u64 = 4;
    const STACK_RUN: u64 = 5;
    const ALTERNATION: u64 = 6;
    const FASTER: u64 = 7;
    const SLOWER: u64 = 8;
    const REPEATED: u64 = 9;

    const MASK_16: u64 = 0xffff;
    const MASK_24: u64 = 0xff_ffff;
    const MASK_32: u64 = 0xffff_ffff;

    fn pack(kind: Kind, base_offset: u32) -> Option<Self> {
        let word = match kind {
            Kind::Rest => Self::REST << Self::TAG_SHIFT,
            Kind::Atom { value, begin, len } => {
                let value = u16::try_from(value).ok()?;
                let rel = begin.checked_sub(base_offset)?;
                if u64::from(rel) > Self::MASK_24 {
                    return None;
                }
                (Self::ATOM << Self::TAG_SHIFT)
                    | u64::from(value)
                    | (u64::from(rel) << 16)
                    | (u64::from(len) << 40)
            }
            Kind::EqualSequence(Among(a, b)) => {
                (Self::EQUAL_SEQ << Self::TAG_SHIFT) | u64::from(a) | (u64::from(b) << 16)
            }
            Kind::WeightedSequence(Among(a, b)) => {
                (Self::WEIGHTED_SEQ << Self::TAG_SHIFT) | u64::from(a) | (u64::from(b) << 16)
            }
            Kind::Stack(Among(a, b)) => {
                (Self::STACK << Self::TAG_SHIFT) | u64::from(a) | (u64::from(b) << 16)
            }
            Kind::StackRun(Among(a, b)) => {
                (Self::STACK_RUN << Self::TAG_SHIFT) | u64::from(a) | (u64::from(b) << 16)
            }
            Kind::Alternation(Among(a, b)) => {
                (Self::ALTERNATION << Self::TAG_SHIFT) | u64::from(a) | (u64::from(b) << 16)
            }
            Kind::Faster(id, rate, site, one_layer) => {
                if u64::from(rate) > Self::MASK_24 {
                    return None;
                }
                (Self::FASTER << Self::TAG_SHIFT)
                    | u64::from(id.0)
                    | (u64::from(rate) << 16)
                    | (u64::from(site) << 40)
                    | (u64::from(one_layer) << 56)
            }
            Kind::Slower(id, times, site) => {
                if u64::from(times) > Self::MASK_24 {
                    return None;
                }
                (Self::SLOWER << Self::TAG_SHIFT)
                    | u64::from(id.0)
                    | (u64::from(times) << 16)
                    | (u64::from(site) << 40)
            }
            Kind::Repeated(id, times) => {
                (Self::REPEATED << Self::TAG_SHIFT) | u64::from(id.0) | (u64::from(times) << 16)
            }
        };
        Some(Self(word))
    }

    #[inline]
    fn unpack(self, base_offset: u32) -> Kind {
        let w = self.0;
        match w >> Self::TAG_SHIFT {
            Self::REST => Kind::Rest,
            Self::ATOM => Kind::Atom {
                value: (w & Self::MASK_16) as u32,
                begin: base_offset + ((w >> 16) & Self::MASK_24) as u32,
                len: ((w >> 40) & Self::MASK_16) as u16,
            },
            Self::EQUAL_SEQ => Kind::EqualSequence(Among(
                (w & Self::MASK_16) as u16,
                ((w >> 16) & Self::MASK_16) as u16,
            )),
            Self::WEIGHTED_SEQ => Kind::WeightedSequence(Among(
                (w & Self::MASK_16) as u16,
                ((w >> 16) & Self::MASK_16) as u16,
            )),
            Self::STACK => Kind::Stack(Among(
                (w & Self::MASK_16) as u16,
                ((w >> 16) & Self::MASK_16) as u16,
            )),
            Self::STACK_RUN => Kind::StackRun(Among(
                (w & Self::MASK_16) as u16,
                ((w >> 16) & Self::MASK_16) as u16,
            )),
            Self::ALTERNATION => Kind::Alternation(Among(
                (w & Self::MASK_16) as u16,
                ((w >> 16) & Self::MASK_16) as u16,
            )),
            Self::FASTER => Kind::Faster(
                Id((w & Self::MASK_16) as u16),
                ((w >> 16) & Self::MASK_24) as u32,
                ((w >> 40) & Self::MASK_16) as u16,
                ((w >> 56) & 1) != 0,
            ),
            Self::SLOWER => Kind::Slower(
                Id((w & Self::MASK_16) as u16),
                ((w >> 16) & Self::MASK_24) as u32,
                ((w >> 40) & Self::MASK_16) as u16,
            ),
            Self::REPEATED => Kind::Repeated(
                Id((w & Self::MASK_16) as u16),
                ((w >> 16) & Self::MASK_32) as u32,
            ),
            _ => unreachable!("invalid packed kind tag"),
        }
    }
}

/// Where a plan is among the [`Plans`] of its text.
#[derive(Clone, Copy)]
struct Id(u16);

/// Where the text writes a word or a factor: two offsets in the score.
type At = (u32, u32);

/// The offsets of `at`, as a hap holds a location.
fn place(at: At) -> (usize, usize) {
    (at.0 as usize, at.1 as usize)
}

/// A run of one of the lists of [`Plans`]: its first entry, and the entry
/// after its last.
#[derive(Clone, Copy)]
struct Among(u16, u16);

impl Among {
    /// The run from `first` to the end of a list of `len` entries.
    fn from(first: usize, len: usize) -> Option<Self> {
        Some(Self(u16::try_from(first).ok()?, u16::try_from(len).ok()?))
    }

    fn range(self) -> Range<usize> {
        self.0 as usize..self.1 as usize
    }
}

/// The steps of a sequence: each fills units of weight. Steps that all have
/// the weight one are plain ids among the ids, which need no weights. The
/// steps of other weights are among the parts.
#[derive(Clone, Copy)]
struct Steps {
    parts: Among,
    /// Whether each step has the weight one: the steps are then in one
    /// layer. Steps of other weights are a layer each, as they answer.
    equal: bool,
}

impl Steps {
    fn len(self) -> usize {
        self.parts.range().len()
    }
}

/// One step of a sequence that has steps of other weights: its plan, and
/// the unit after its last unit of weight. Its first unit is the one after
/// the last of the step before it.
type Part = (Id, u32);

/// One step as a walk reads it: its plan, and the units of weight that it
/// fills, from the first to the one after the last.
type Step = (Id, (u32, u32));

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
    /// A plan, in the window of the cycle from the unit `from` to the unit
    /// `to` of `of` units.
    Plan {
        plan: Id,
        from: u32,
        to: u32,
        of: u32,
    },
    /// The steps of a sequence, over the cycle.
    Steps(Steps),
}

impl Root {
    /// The window of the cycle that the root writes.
    fn window(self) -> Window {
        match self {
            Self::Plan { from, to, of, .. } => Window {
                begin: i64::from(from),
                end: i64::from(to),
                parts: i64::from(of),
            },
            Self::Steps(_) => Window::CYCLE,
        }
    }
}

/// A count of cycles as a fraction of whole numbers, in the range of a
/// product of a few counts: a lane, a speed or a slow count each take a
/// factor of it.
#[derive(Clone, Copy)]
struct Pace {
    numer: u32,
    denom: u32,
}

impl Pace {
    const ONE: Self = Self { numer: 1, denom: 1 };

    /// This pace times `numer / denom`, in lowest terms. `None` for a zero,
    /// and outside the range of the whole numbers.
    fn times(self, numer: u32, denom: u32) -> Option<Self> {
        if numer == 0 || denom == 0 {
            return None;
        }
        let numer = u64::from(self.numer) * u64::from(numer);
        let denom = u64::from(self.denom) * u64::from(denom);
        let (mut divisor, mut rest) = (numer, denom);
        while rest != 0 {
            (divisor, rest) = (rest, divisor % rest);
        }
        Some(Self {
            numer: u32::try_from(numer / divisor).ok()?,
            denom: u32::try_from(denom / divisor).ok()?,
        })
    }

    fn terms(self) -> (i128, i128) {
        (i128::from(self.numer), i128::from(self.denom))
    }

    fn fraction(self) -> Option<Fraction> {
        Fraction::checked_new(i128::from(self.numer), i128::from(self.denom))
    }
}

/// Inline storage for speed/slow sites above a layer (0 or 1 site inline with
/// zero heap allocation, spilling to `Box<[At]>` only for nested speeds).
enum AfterSites {
    Empty,
    One(At),
    Many(Box<[At]>),
}

impl AfterSites {
    fn from_above(above: &[At]) -> Self {
        match *above {
            [] => Self::Empty,
            [one] => Self::One(one),
            _ => Self::Many(above.iter().rev().copied().collect()),
        }
    }

    #[inline]
    fn as_slice(&self) -> &[At] {
        match self {
            Self::Empty => &[],
            Self::One(one) => std::slice::from_ref(one),
            Self::Many(many) => many,
        }
    }
}

/// One layer of the rhythm of a plan.
struct LayerPlan {
    root: Root,
    /// The cycles of the walk after which the layer repeats.
    turns: i64,
    /// The cycles of the pattern that one cycle of the walk takes. A lane
    /// of an alternation, a speed and a slow count each scale it for the
    /// layers under them.
    pace: Pace,
    /// Where the text writes the speeds and the slow counts above the
    /// layer, the innermost first. Zero or one site stays inline.
    after: AfterSites,
}

impl LayerPlan {
    /// The layer of `root`, under the speeds and slow counts of `above`,
    /// the outermost first.
    fn of(root: Root, turns: i64, above: &[At]) -> Self {
        Self {
            root,
            turns,
            pace: Pace::ONE,
            // One list for the layer, the innermost first.
            after: AfterSites::from_above(above),
        }
    }

    /// The cycles of the pattern after which the layer repeats.
    fn period(&self) -> Option<Fraction> {
        Fraction::int(i128::from(self.turns)).checked_mul(self.pace.fraction()?)
    }

    /// Whether the layer holds a list of its `count` steps.
    fn is_listed(&self, count: usize) -> bool {
        let short = |period: Fraction| period < Fraction::int(READ_PERIOD);
        count <= SMALL_LISTED || (self.period().is_some_and(short) && count <= MAX_STEPS)
    }
}

/// Layers with at most this many steps always materialize directly as a
/// compact grid rather than retaining `Plans` or building lazily during a
/// window query.
const SMALL_LISTED: usize = 8;

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
    kinds: Vec<PackedKind>,
    /// What the planner knows of each construct. A pattern that reads its
    /// steps from the plans keeps none of it.
    facts: Vec<Facts>,
    /// The steps of each sequence and of each lane with steps of other
    /// weights than one, one after the other.
    parts: Vec<Part>,
    /// The steps of each sequence and of each lane of steps of the weight
    /// one, one after the other.
    ids: Vec<Id>,
    /// Where the text writes each speed and each slow count.
    sites: Vec<At>,
    /// The patterns of each stack.
    stacked: Vec<Id>,
    /// The lanes of each alternation.
    lanes: Vec<Steps>,
    /// Starting score byte offset of this mini string, subtracted from packed
    /// atom offsets so each atom fits in 8 bytes.
    base_offset: u32,
    /// Whether a step may be longer than a cycle: a plan is a slow count,
    /// or a step of a lane fills more than one cycle.
    long: bool,
}

impl Plans {
    #[inline]
    fn kind(&self, id: Id) -> Kind {
        self.kinds[id.0 as usize].unpack(self.base_offset)
    }

    fn facts(&self, id: Id) -> Facts {
        self.facts[id.0 as usize]
    }

    fn add(&mut self, kind: Kind, facts: Facts) -> Option<Id> {
        let id = u16::try_from(self.kinds.len()).ok()?;
        let packed = PackedKind::pack(kind, self.base_offset)?;
        self.kinds.push(packed);
        self.facts.push(facts);
        Some(Id(id))
    }

    /// The `at`th step of `steps`.
    fn step(&self, steps: Steps, at: usize) -> Step {
        let first = steps.parts.0 as usize;
        if steps.equal {
            // The steps are few enough for the range of the units.
            let unit = at as u32;
            return (self.ids[first + at], (unit, unit + 1));
        }
        let (plan, to) = self.parts[first + at];
        let from = match at {
            0 => 0,
            _ => self.parts[first + at - 1].1,
        };
        (plan, (from, to))
    }

    /// The steps of `steps`, in the order of time.
    fn each(&self, steps: Steps) -> impl Iterator<Item = Step> + '_ {
        (0..steps.len()).map(move |at| self.step(steps, at))
    }

    /// The units of weight of `steps`.
    fn total(&self, steps: Steps) -> u32 {
        // Steps with no step are equal: the others end at their last part.
        debug_assert!(steps.equal || steps.len() > 0);
        match (steps.equal, steps.len()) {
            (true, len) => len as u32,
            (false, len) => self.parts[steps.parts.0 as usize + len - 1].1,
        }
    }

    /// The place of the site `at` among the sites.
    fn site(&mut self, at: At) -> Option<u16> {
        let place = u16::try_from(self.sites.len()).ok()?;
        self.sites.push(at);
        Some(place)
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
            // A weight of zero would make the count of steps the total.
            debug_assert!(weight > 0);
            total = total.checked_add(weight)?;
            self.parts.push((plan, total));
        }
        let equal = total as usize == self.parts.len() - first;
        if !equal {
            return Some(Steps {
                parts: Among::from(first, self.parts.len())?,
                equal,
            });
        }
        // Steps of the weight one need no weights.
        let first_id = self.ids.len();
        self.ids
            .extend(self.parts.drain(first..).map(|(plan, _)| plan));
        Some(Steps {
            parts: Among::from(first_id, self.ids.len())?,
            equal,
        })
    }

    /// The plan of the sequence of `steps`, which reports their weights as
    /// its step count. One step is the sequence, at any weight.
    fn sequence(&mut self, steps: Steps) -> Option<Id> {
        let total = i64::from(self.total(steps));
        let id = match steps.len() {
            1 => {
                let plan = self.step(steps, 0).0;
                // The list of one step is no more than its plan. It leaves
                // no room when it is the last made.
                let first = steps.parts.0 as usize;
                let list_len = match steps.equal {
                    true => self.ids.len(),
                    false => self.parts.len(),
                };
                debug_assert_eq!(steps.parts.1 as usize, list_len);
                if steps.parts.1 as usize == list_len {
                    match steps.equal {
                        true => self.ids.truncate(first),
                        false => self.parts.truncate(first),
                    }
                }
                plan
            }
            _ => {
                let facts = Facts {
                    turns: self.turns(steps)?,
                    steps: total,
                    layers: self.layers(steps),
                    word: false,
                };
                self.add(
                    match steps.equal {
                        true => Kind::EqualSequence(steps.parts),
                        false => Kind::WeightedSequence(steps.parts),
                    },
                    facts,
                )?
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
        let listed = &self.stacked[first..];
        // The members of a stack often come one after the other among the
        // plans, as words do: they need no list.
        let run = !listed.is_empty() && listed.windows(2).all(|pair| pair[1].0 == pair[0].0 + 1);
        if run {
            let (begin, end) = (
                listed[0].0 as usize,
                listed[listed.len() - 1].0 as usize + 1,
            );
            self.stacked.truncate(first);
            return self.add(Kind::StackRun(Among::from(begin, end)?), facts);
        }
        let patterns = Among::from(first, self.stacked.len())?;
        self.add(Kind::Stack(patterns), facts)
    }

    /// The patterns of a stack, in the order of time.
    fn stacked(&self, patterns: Among, run: bool) -> impl Iterator<Item = Id> + '_ {
        patterns.range().map(move |at| self.stacked_at(at, run))
    }

    /// The pattern at `at` among the patterns of a stack.
    fn stacked_at(&self, at: usize, run: bool) -> Id {
        match run {
            // The plans are fewer than the range of an id.
            true => Id(at as u16),
            false => self.stacked[at],
        }
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
            let own = self.turns(lane)?.checked_mul(i64::from(self.total(lane)))?;
            turns = common_turns(turns, own)?;
            layers = layers.checked_add(sounding)?;
            // A step of a weight above one fills as many cycles, but for
            // a `!` of the weight, which fills each with one of its own.
            let fills = |(plan, (from, to)): Step| self.lane_step(plan, to - from).is_some();
            let steps_fill = self.each(lane).all(fills);
            self.long |= !steps_fill;
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

    /// The positive rational factor that the atom at `factor` writes, as
    /// `(numer, denom)` and its source span.
    fn fraction_factor(&self, values: &[Value], factor: Id) -> Option<(u32, u32, At)> {
        let Kind::Atom { value, begin, len } = self.kind(factor) else {
            return None;
        };
        let count = crate::fraction_site_value(values.get(value as usize)?)?;
        if count <= Fraction::ZERO
            || count.numer() > MAX_PERIOD_TERM
            || count.denom() > MAX_PERIOD_TERM
        {
            return None;
        }
        let numer = u32::try_from(count.numer()).ok()?;
        let denom = u32::try_from(count.denom()).ok()?;
        Some((numer, denom, written(begin, len)))
    }

    fn faster_whole(&mut self, pattern: Id, rate: u32, at: At) -> Option<Id> {
        let under = self.facts(pattern);
        // The period of each layer, divided by the rate.
        let period = Fraction::checked_new(i128::from(under.turns), i128::from(rate))?;
        let facts = Facts {
            turns: i64::try_from(period.numer()).ok()?,
            word: false,
            ..under
        };
        let speed = self.site(at)?;
        self.add(Kind::Faster(pattern, rate, speed, under.layers <= 1), facts)
    }

    fn slower_whole(&mut self, pattern: Id, times: u32, at: At) -> Option<Id> {
        let under = self.facts(pattern);
        let facts = Facts {
            turns: under.turns.checked_mul(i64::from(times))?,
            word: false,
            ..under
        };
        self.long = true;
        let speed = self.site(at)?;
        self.add(Kind::Slower(pattern, times, speed), facts)
    }

    /// The plan of `pattern` at the speed that `factor` writes.
    fn faster(&mut self, values: &[Value], pattern: Id, factor: Id) -> Option<Id> {
        let (numer, denom, at) = self.fraction_factor(values, factor)?;
        if denom == 1 {
            self.faster_whole(pattern, numer, at)
        } else if numer == 1 {
            self.slower_whole(pattern, denom, at)
        } else {
            None
        }
    }

    /// The plan of `pattern`, as many times as slow as `factor` writes.
    fn slower(&mut self, values: &[Value], pattern: Id, factor: Id) -> Option<Id> {
        let (numer, denom, at) = self.fraction_factor(values, factor)?;
        if denom == 1 {
            self.slower_whole(pattern, numer, at)
        } else if numer == 1 {
            self.faster_whole(pattern, denom, at)
        } else {
            None
        }
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

    /// Whether every step of `plan` covers its whole cycle `[turn, turn + 1]`,
    /// so a Euclidean mask splits it cleanly into equal steps with no cut.
    fn covers_whole_cycle(&self, plan: Id) -> bool {
        match self.kind(plan) {
            Kind::Atom { .. } | Kind::Rest => true,
            Kind::Stack(items) => self.stacked[items.range()]
                .iter()
                .all(|&item| self.covers_whole_cycle(item)),
            Kind::StackRun(items) => (items.0..items.1).all(|id| self.covers_whole_cycle(Id(id))),
            Kind::Alternation(lanes) => self.lanes[lanes.range()]
                .iter()
                .all(|&steps| steps.len() == 1 && self.covers_whole_cycle(self.step(steps, 0).0)),
            Kind::Faster(inner, 1, _, _) => self.covers_whole_cycle(inner),
            _ => false,
        }
    }

    /// Extract a plain integer atom and its source span.
    fn integer_atom(&self, values: &[Value], id: Id) -> Option<(i32, At)> {
        let Kind::Atom { value, begin, len } = self.kind(id) else {
            return None;
        };
        let count = crate::fraction_site_value(values.get(value as usize)?)?;
        if count.denom() != 1 {
            return None;
        }
        let n = i32::try_from(count.numer()).ok()?;
        Some((n, written(begin, len)))
    }

    /// Euclidean rhythm `pattern(pulses, steps[, rotation])` directly in plans
    /// when `pulses`, `steps`, `rotation` are constant integers and `pattern`
    /// covers whole cycles.
    fn euclid(
        &mut self,
        values: &[Value],
        pattern: Id,
        pulses: (Id, u32),
        steps: (Id, u32),
        rotation: Option<(Id, u32)>,
    ) -> Option<Id> {
        if pulses.1 != 1 || steps.1 != 1 || rotation.is_some_and(|(_, w)| w != 1) {
            return None;
        }
        let (p, p_at) = self.integer_atom(values, pulses.0)?;
        let (s, s_at) = self.integer_atom(values, steps.0)?;
        let rot = match rotation {
            Some((r_id, _)) => Some(self.integer_atom(values, r_id)?),
            None => None,
        };
        if p < 1 || s < p || s > MAX_STEPS as i32 || !self.covers_whole_cycle(pattern) {
            return None;
        }
        let r = rot.map_or(0, |(v, _)| v);
        let mask = rustel_core::euclid::euclid_rot(p, s as usize, r as isize).ok()?;
        let rest = self.add(
            Kind::Rest,
            Facts {
                turns: 1,
                steps: 1,
                layers: 0,
                word: false,
            },
        )?;
        let step_list = self.steps(
            mask.into_iter()
                .map(|set| Some((if set != 0 { pattern } else { rest }, 1))),
            false,
        )?;
        let seq = self.sequence(step_list)?;
        let with_p = self.faster_whole(seq, 1, p_at)?;
        let with_s = self.faster_whole(with_p, 1, s_at)?;
        if let Some((_, r_at)) = rot {
            self.faster_whole(with_s, 1, r_at)
        } else {
            Some(with_s)
        }
    }

    /// The plan of the words of `chain` and then the word `last`, with `:`
    /// between them: one value, a list of the value of each word, at `at`,
    /// where the text writes them all. `None` for a side that is no word:
    /// a rest, and anything in brackets.
    fn tail(&mut self, values: &mut Vec<Value>, chain: Id, last: Id, at: At) -> Option<Id> {
        let word = |plan: Id| match (self.kind(plan), self.facts(plan).word) {
            (Kind::Atom { value, .. }, true) => Some(value as usize),
            _ => None,
        };
        let (words, last) = (word(chain)?, word(last)?);
        // A handle of each value, for the list.
        let last = values.get(last)?.clone();
        let list: Arc<[Value]> = match values.get(words)?.clone() {
            Value::List(list) => {
                let mut out = Vec::with_capacity(list.len() + 1);
                out.extend_from_slice(&list);
                out.push(last);
                Arc::from(out)
            }
            first => Arc::from([first, last]),
        };
        let held = u32::try_from(values.len()).ok()?;
        values.push(Value::List(list));
        self.kinds[chain.0 as usize] = PackedKind::pack(Kind::atom(held, at)?, self.base_offset)?;
        Some(chain)
    }

    /// The common multiple of the periods of `steps`.
    fn turns(&self, steps: Steps) -> Option<i64> {
        let turns = |turns, (plan, _): Step| common_turns(turns, self.facts(plan).turns);
        self.each(steps).try_fold(1, turns)
    }

    /// How many layers the sequence of `steps` has: those of its one
    /// step, one layer for steps of the weight one, and a layer for each
    /// step that holds a value otherwise.
    fn layers(&self, steps: Steps) -> u32 {
        let sounding = self
            .each(steps)
            .filter(|(plan, _)| self.facts(*plan).layers > 0);
        // The steps of a text are far fewer than the range holds.
        let sounding = sounding.count() as u32;
        match steps.len() {
            1 => self.facts(self.step(steps, 0).0).layers,
            _ if steps.equal => sounding.min(1),
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
            Kind::Atom { value, begin, len } => {
                asked.step(window, value, written(begin, len), speeds)
            }
            kind @ (Kind::EqualSequence(parts) | Kind::WeightedSequence(parts)) => {
                let equal = kind.is_equal();
                self.walk_steps(Steps { parts, equal }, cycle, window, speeds, asked)
            }
            kind @ (Kind::Stack(patterns) | Kind::StackRun(patterns)) => self
                .stacked(patterns, kind.is_run())
                .try_for_each(|pattern| self.walk(pattern, cycle, window, speeds, asked)),
            Kind::Alternation(lanes) => self.lanes[lanes.range()]
                .iter()
                .try_for_each(|lane| self.walk_lane(*lane, cycle, window, speeds, asked)),
            Kind::Faster(pattern, rate, speed, one_layer) => {
                if rate > 1 && !one_layer {
                    return None;
                }
                let rate = i64::from(rate);
                let at = self.sites[speed as usize];
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
        let part =
            |from: u32, to: u32, total: i64| window.parts((i64::from(from), i64::from(to)), total);
        // The steps are in the order of time: those before the first
        // step that the span meets are passed with no walk of them. A step
        // is past when its end is, so the passed steps need no start.
        let is_past =
            |to: u32, total: i64| part(0, to, total).is_some_and(|window| asked.is_past(window));
        let range = steps.parts.range();
        if steps.equal {
            let ids = &self.ids[range];
            if let [plan] = ids {
                return self.walk(*plan, cycle, window, speeds, asked);
            }
            // The steps are few enough for the range of the units.
            let total = ids.len() as i64;
            let first = passed(ids.len(), |at| is_past(at as u32 + 1, total));
            for (at, plan) in ids.iter().enumerate().skip(first) {
                let window = part(at as u32, at as u32 + 1, total)?;
                if asked.is_ahead(window) {
                    break;
                }
                self.walk(*plan, cycle, window, speeds, asked)?;
            }
            return Some(());
        }
        let parts = &self.parts[range];
        // The steps of a list of more than one step end at their last part.
        let total = i64::from(parts.last()?.1);
        let first = parts.partition_point(|(_, to)| is_past(*to, total));
        // The start of a step is the end of the step before it.
        let mut from = first.checked_sub(1).map_or(0, |before| parts[before].1);
        for (plan, to) in &parts[first..] {
            let window = part(from, *to, total)?;
            if asked.is_ahead(window) {
                break;
            }
            self.walk(*plan, cycle, window, speeds, asked)?;
            from = *to;
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
        let range = steps.parts.range();
        if steps.equal {
            let ids = &self.ids[range];
            if ids.is_empty() {
                return Some(());
            }
            // Each step fills one cycle: the turn of the cycle names it.
            let total = ids.len() as i64;
            let (turn, unit) = (cycle.div_euclid(total), cycle.rem_euclid(total));
            return self.walk(ids[unit as usize], turn, window, speeds, asked);
        }
        let parts = &self.parts[range];
        let total = i64::from(parts.last()?.1);
        let (turn, unit) = (cycle.div_euclid(total), cycle.rem_euclid(total));
        let at = parts.partition_point(|(_, to)| i64::from(*to) <= unit);
        let (plan, to) = *parts.get(at)?;
        let from = at.checked_sub(1).map_or(0, |before| parts[before].1);
        self.walk(
            self.lane_step(plan, to - from)?,
            turn,
            window,
            speeds,
            asked,
        )
    }

    /// [`Self::walk`] for what a layer writes.
    fn walk_root(&self, root: Root, cycle: i64, asked: &mut Asked<'_>) -> Option<()> {
        match root {
            Root::Plan { plan, .. } => self.walk(plan, cycle, root.window(), None, asked),
            Root::Steps(steps) => self.walk_steps(steps, cycle, Window::CYCLE, None, asked),
        }
    }

    /// The layers of the rhythm of the construct at `id`, after those of
    /// `out`. A construct with no value has no layer.
    fn layers_onto(&self, id: Id, out: &mut Vec<LayerPlan>, above: &mut Vec<At>) -> Option<()> {
        let small = |period: &Fraction| {
            period.numer() <= MAX_PERIOD_TERM && period.denom() <= MAX_PERIOD_TERM
        };
        // The layers from `from` on, `numer / denom` times as slow.
        let slower = |layers: &mut [LayerPlan], numer: u32, denom: u32| {
            layers.iter_mut().try_for_each(|layer| {
                layer.pace = layer.pace.times(numer, denom)?;
                layer.period().filter(small).map(drop)
            })
        };
        let facts = self.facts(id);
        if facts.layers == 0 {
            return Some(());
        }
        match self.kind(id) {
            Kind::Rest => Some(()),
            kind @ (Kind::EqualSequence(parts) | Kind::WeightedSequence(parts)) => {
                let equal = kind.is_equal();
                self.layers_of_steps(Steps { parts, equal }, out, above)
            }
            kind @ (Kind::Stack(patterns) | Kind::StackRun(patterns)) => self
                .stacked(patterns, kind.is_run())
                .try_for_each(|pattern| self.layers_onto(pattern, out, above)),
            Kind::Alternation(lanes) => self.lanes[lanes.range()].iter().try_for_each(|lane| {
                let from = out.len();
                self.layers_of_steps(*lane, out, above)?;
                slower(&mut out[from..], self.total(*lane), 1)
            }),
            kind @ (Kind::Faster(pattern, count, speed, _)
            | Kind::Slower(pattern, count, speed)) => {
                let from = out.len();
                // The place of the factor comes after those of a value.
                above.push(self.sites[speed as usize]);
                self.layers_onto(pattern, out, above)?;
                above.pop();
                // A speed divides the period of each layer, and a slow
                // count multiplies it.
                let (numer, denom) = match kind {
                    Kind::Faster(..) => (1, count),
                    _ => (count, 1),
                };
                slower(&mut out[from..], numer, denom)
            }
            // An atom, and a repeat: one layer of each cycle.
            Kind::Atom { .. } | Kind::Repeated(..) => {
                let root = Root::Plan {
                    plan: id,
                    from: 0,
                    to: 1,
                    of: 1,
                };
                out.push(LayerPlan::of(root, facts.turns, above));
                Some(())
            }
        }
    }

    /// The layers of the sequence of `steps`, after those of `out`: those
    /// of its one step, one layer for steps of the weight one, and a layer
    /// for each step otherwise.
    fn layers_of_steps(
        &self,
        steps: Steps,
        out: &mut Vec<LayerPlan>,
        above: &mut Vec<At>,
    ) -> Option<()> {
        if self.layers(steps) == 0 {
            return Some(());
        }
        match steps.len() {
            1 => self.layers_onto(self.step(steps, 0).0, out, above),
            _ if steps.equal => {
                out.push(LayerPlan::of(Root::Steps(steps), self.turns(steps)?, above));
                Some(())
            }
            _ => self.each(steps).try_for_each(|(plan, (from, to))| {
                let facts = self.facts(plan);
                if facts.layers > 0 {
                    let of = self.total(steps);
                    let root = Root::Plan { plan, from, to, of };
                    out.push(LayerPlan::of(root, facts.turns, above));
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
            Root::Plan { plan, of, .. } => self.survey(plan, layer.turns, i64::from(of))?.count,
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
            Kind::Atom { .. } => Survey {
                count: cycles,
                fills: true,
            },
            kind @ (Kind::EqualSequence(list) | Kind::WeightedSequence(list)) => Survey {
                count: self.survey_steps(
                    Steps {
                        parts: list,
                        equal: kind.is_equal(),
                    },
                    cycles,
                    parts,
                )?,
                fills: false,
            },
            kind @ (Kind::Stack(patterns) | Kind::StackRun(patterns)) => {
                let patterns = self.stacked(patterns, kind.is_run());
                Survey {
                    count: together(patterns.map(|pattern| self.survey(pattern, cycles, parts)))?,
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
        let parts = match steps.len() {
            1 => parts,
            _ => parts.checked_mul(i64::from(self.total(steps)))?,
        };
        sum(self
            .each(steps)
            .map(|(plan, _)| Some(self.survey(plan, cycles, parts)?.count)))
    }

    /// [`Self::survey`] for the lane of `steps`.
    fn survey_lane(&self, steps: Steps, cycles: i64, parts: i64) -> Option<Survey> {
        let mut lane = Survey {
            count: 0,
            fills: true,
        };
        let total = i64::from(self.total(steps));
        // The cycles hold each step of the lane a whole count of times.
        if cycles % total != 0 {
            return None;
        }
        for (plan, (from, to)) in self.each(steps) {
            let weight = to - from;
            let step = self.survey(self.lane_step(plan, weight)?, cycles / total, parts)?;
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
            Kind::Atom { value, begin, len } => {
                let place = &mut local[value as usize];
                if *place == 0 {
                    held.push(value);
                    *place = held.len() as u32;
                }
                self.kinds[id.0 as usize] = PackedKind::pack(
                    Kind::Atom {
                        value: *place - 1,
                        begin,
                        len,
                    },
                    self.base_offset,
                )
                .expect("localized atom value fits packed kind");
            }
            kind @ (Kind::EqualSequence(parts) | Kind::WeightedSequence(parts)) => {
                let equal = kind.is_equal();
                self.localize_steps(Steps { parts, equal }, local, held)
            }
            kind @ (Kind::Stack(patterns) | Kind::StackRun(patterns)) => {
                for at in patterns.range() {
                    self.localize(self.stacked_at(at, kind.is_run()), local, held);
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
        for at in 0..steps.len() {
            self.localize(self.step(steps, at).0, local, held);
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
            Root::Plan { plan, .. } => self.localize(plan, local, &mut held),
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
        pace: (i128, i128),
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
            let numer = time.numer().checked_mul(pace.1)?;
            let denom = time.denom().checked_mul(pace.0)?;
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
            after: after.as_slice(),
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
            if let Root::Plan { .. } = root
                && (asked.is_past(root.window()) || asked.is_ahead(root.window()))
            {
                continue;
            }
            self.walk_root(*root, asked.cycle, &mut asked)?;
        }
        Some(())
    }

    /// Populate `steps` with the steps of `layer` in cycles with deduplicated
    /// layer-local value indices, returning the layer's value slice and reusing
    /// caller-owned scratch buffers.
    #[allow(clippy::too_many_arguments)]
    fn listed_indexed_into(
        &self,
        layer: &LayerPlan,
        period: Fraction,
        count: usize,
        values: &[Value],
        local: &mut Vec<u32>,
        held: &mut Vec<u32>,
        steps: &mut Vec<SourceStep>,
    ) -> Option<Arc<[Value]>> {
        if local.len() < values.len() {
            local.resize(values.len(), 0);
        }
        held.clear();
        steps.clear();
        steps.reserve(count);
        let written = self.read(
            layer,
            layer.pace.terms(),
            Fraction::ZERO,
            period,
            &mut |mut step| {
                let global = step.value as usize;
                let place = &mut local[global];
                if *place == 0 {
                    held.push(step.value);
                    *place = held.len() as u32;
                }
                step.value = *place - 1;
                steps.push(step);
            },
        );
        for &global in held.iter() {
            local[global as usize] = 0;
        }
        written?;
        let layer_values: Arc<[Value]> = held
            .iter()
            .map(|&global| values[global as usize].clone())
            .collect();
        Some(layer_values)
    }

    /// Populate `steps` with the steps of `layer` in cycles remapping global
    /// value indices through `remap` (when all layers share one deduplicated
    /// `Arc<[Value]>`).
    fn listed_raw_into(
        &self,
        layer: &LayerPlan,
        period: Fraction,
        count: usize,
        remap: &[u32],
        steps: &mut Vec<SourceStep>,
    ) -> Option<()> {
        steps.clear();
        steps.reserve(count);
        self.read(
            layer,
            layer.pace.terms(),
            Fraction::ZERO,
            period,
            &mut |mut step| {
                step.value = remap[step.value as usize];
                steps.push(step);
            },
        )
    }

    /// These plans with what a walk reads of them, and no more: a pattern
    /// keeps them for as long as it plays.
    fn for_walks(mut self) -> Self {
        self.kinds.shrink_to_fit();
        self.parts.shrink_to_fit();
        self.ids.shrink_to_fit();
        self.sites.shrink_to_fit();
        self.stacked.shrink_to_fit();
        self.lanes.shrink_to_fit();
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
    /// How many steps a period holds, at most the most steps of a layer.
    count: u32,
}

impl StepSource for PlanLayer {
    fn count(&self) -> usize {
        self.count as usize
    }

    fn steps(
        &self,
        begin: Fraction,
        end: Fraction,
        each: &mut dyn FnMut(SourceStep),
    ) -> Option<()> {
        self.plans
            .read(&self.layer, self.layer.pace.terms(), begin, end, each)
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
    /// Reusable parser open-step stack.
    parser_steps: Vec<crate::Slice<Option<(Id, u32)>>>,
    /// Reusable parser open-group item stack.
    parser_items: Vec<Option<Id>>,
    /// Reusable parser open-alternation lane stack.
    parser_lanes: Vec<Option<Steps>>,
    /// Reusable scratch buffers for layer planning and step materialisation.
    layers_buf: Vec<LayerPlan>,
    counted_layers_buf: Vec<(LayerPlan, usize)>,
    above_buf: Vec<At>,
    steps_buf: Vec<SourceStep>,
    local_buf: Vec<u32>,
    held_buf: Vec<u32>,
}

impl Room {
    /// Whether the room holds no list: a new room, or one that a reader
    /// put away.
    pub fn is_bare(&self) -> bool {
        let Plans {
            kinds,
            facts,
            parts,
            ids,
            sites,
            stacked,
            lanes,
            base_offset: _,
            long: _,
        } = &self.plans;
        [
            kinds.capacity(),
            facts.capacity(),
            parts.capacity(),
            ids.capacity(),
            sites.capacity(),
            stacked.capacity(),
            lanes.capacity(),
            self.values.capacity(),
            self.words.capacity(),
            self.known.capacity(),
            self.parser_steps.capacity(),
            self.parser_items.capacity(),
            self.parser_lanes.capacity(),
            self.layers_buf.capacity(),
            self.counted_layers_buf.capacity(),
            self.above_buf.capacity(),
            self.steps_buf.capacity(),
            self.local_buf.capacity(),
            self.held_buf.capacity(),
        ]
        .iter()
        .all(|held| *held == 0)
    }

    /// Empty the room for the plans of `src`, with none to move: a word
    /// and its space are two bytes or more.
    fn clear(&mut self, src: &str, base_offset: u32) {
        let Plans {
            kinds,
            facts,
            parts,
            ids,
            sites,
            stacked,
            lanes,
            base_offset: stored_base,
            long,
        } = &mut self.plans;
        let words = (src.len() / 2 + 1).max(32);
        kinds.clear();
        kinds.reserve(words);
        facts.clear();
        facts.reserve(words);
        parts.clear();
        parts.reserve(words);
        ids.clear();
        ids.reserve(words);
        sites.clear();
        stacked.clear();
        lanes.clear();
        *stored_base = base_offset;
        *long = false;
        self.values.clear();
        self.values.reserve(16);
        self.words.clear();
        self.words.reserve(16);
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
        let Ok(base_offset) = u32::try_from(offset) else {
            return Ok(None);
        };
        room.clear(src, base_offset);
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
    #[cfg(test)]
    fn layers(&self) -> Option<Vec<(LayerPlan, usize)>> {
        let plans = &self.room.plans;
        let mut layers = Vec::new();
        plans.layers_onto(self.root, &mut layers, &mut Vec::new())?;
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
    pub(crate) fn pattern(mut self) -> Option<Pattern> {
        let steps = self.steps();
        if let Kind::Atom { value, begin, len } = self.room.plans.kind(self.root) {
            let value = self.room.values.swap_remove(value as usize);
            let at = place(written(begin, len));
            return Some(rustel_core::pure_at(value, at).with_steps(steps));
        }
        // Fast path for flat and shallow trees of atoms, rests, sequences,
        // repeats, alternations, and stacks: build `SmallGridStep` grids
        // directly from integer ticks with zero `Fraction` math, `survey`,
        // `walk`, or `SourceStep` allocations.
        if let Some(pattern) = self.try_tick_pattern(steps) {
            return Some(pattern);
        }
        if let Some(pattern) = self.try_cut_tick_pattern(steps) {
            return Some(pattern);
        }
        let Room {
            plans,
            values,
            layers_buf,
            counted_layers_buf,
            above_buf,
            steps_buf,
            local_buf,
            held_buf,
            ..
        } = self.room;
        layers_buf.clear();
        above_buf.clear();
        plans.layers_onto(self.root, layers_buf, above_buf)?;
        counted_layers_buf.clear();
        for layer in layers_buf.drain(..) {
            let count = plans.reads(&layer)?;
            counted_layers_buf.push((layer, count));
        }
        let in_cycles = !plans.long;
        // Materializing all layers as compact `SmallGridStep` grids is both
        // smaller and faster to query whenever the grids fit within the
        // retained footprint of `Plans` + `PlanLayer` + `Sourced`.
        let all_fit_grid = counted_layers_buf
            .iter()
            .all(|(_, count)| *count <= MAX_STEPS);
        let force_listed = all_fit_grid && {
            let mut sourced_layers = 0usize;
            let mut sourced_grid_bytes = 0usize;
            for (layer, count) in counted_layers_buf.iter() {
                if !layer.is_listed(*count) {
                    sourced_layers += 1;
                    sourced_grid_bytes += *count * 8 + 64;
                }
            }
            if sourced_layers == 0 {
                false
            } else {
                let plan_bytes = 192
                    + plans.kinds.len() * 8
                    + plans.ids.len() * 2
                    + plans.parts.len() * 8
                    + plans.sites.len() * 8
                    + plans.stacked.len() * 2
                    + plans.lanes.len() * 8
                    + sourced_layers * 136;
                sourced_grid_bytes <= plan_bytes + 64
            }
        };
        // Each layer that reads the plans holds its own values.
        let mut own = Vec::new();
        if !force_listed {
            for (layer, count) in counted_layers_buf.iter() {
                if !layer.is_listed(*count) {
                    own.push(plans.values_of(layer, values, local_buf));
                }
            }
        }
        // A layer that reads the plans keeps them for as long as it
        // plays: the room then makes new ones.
        let kept = (!own.is_empty()).then(|| Arc::new(std::mem::take(plans).for_walks()));
        let plans = kept.as_deref().unwrap_or(plans);
        let mut own = own.into_iter();
        if counted_layers_buf.len() == 1 {
            let (layer, count) = counted_layers_buf.pop()?;
            let period = layer.period()?;
            let pattern = if force_listed || layer.is_listed(count) {
                let layer_values = plans.listed_indexed_into(
                    &layer, period, count, values, local_buf, held_buf, steps_buf,
                )?;
                let pattern =
                    sequence_of_indexed_slice(period, layer_values, in_cycles, steps_buf)?;
                steps_buf.clear();
                pattern
            } else {
                sequence_of_sourced(SourcedLayer {
                    period,
                    values: own.next()?,
                    in_cycles,
                    steps: Arc::new(PlanLayer {
                        plans: Arc::clone(kept.as_ref()?),
                        layer,
                        count: u32::try_from(count).ok()?,
                    }),
                })?
            };
            return Some(pattern.with_steps(steps));
        }
        let mut builder = SequenceBuilder::with_capacity(counted_layers_buf.len());
        let orig_values_len = values.len();
        let shared_values: Option<Arc<[Value]>> = if kept.is_none() {
            build_shared_values(values, local_buf, held_buf)
        } else {
            None
        };
        let built = (|| -> Option<()> {
            for (layer, count) in counted_layers_buf.drain(..) {
                let period = layer.period()?;
                if force_listed || layer.is_listed(count) {
                    let layer_values = if let Some(shared) = &shared_values {
                        plans.listed_raw_into(&layer, period, count, local_buf, steps_buf)?;
                        Arc::clone(shared)
                    } else {
                        plans.listed_indexed_into(
                            &layer, period, count, values, local_buf, held_buf, steps_buf,
                        )?
                    };
                    builder.push_indexed_slice(period, layer_values, in_cycles, steps_buf);
                    steps_buf.clear();
                } else {
                    builder.push_sourced(SourcedLayer {
                        period,
                        values: own.next()?,
                        in_cycles,
                        steps: Arc::new(PlanLayer {
                            // A handle of the plans, for each layer.
                            plans: Arc::clone(kept.as_ref()?),
                            layer,
                            count: u32::try_from(count).ok()?,
                        }),
                    });
                }
            }
            Some(())
        })();
        if shared_values.is_some() {
            local_buf[..orig_values_len].fill(0);
        }
        built?;
        Some(builder.finish()?.with_steps(steps))
    }

    fn try_tick_pattern(&mut self, steps: Option<Fraction>) -> Option<Pattern> {
        let Room {
            plans,
            values,
            local_buf,
            held_buf,
            ..
        } = &mut *self.room;
        let mut specs = [TickLayerSpec {
            source: TickLayerSource::Node(self.root),
            total_cycles: 1,
            ticks: 1,
        }; 64];
        let mut spec_len = 0usize;
        if !collect_tick_layers(plans, self.root, &mut specs, &mut spec_len, 0) {
            return None;
        }
        if spec_len == 0 {
            return Some(silence().with_steps(steps));
        }
        if local_buf.len() < values.len() {
            local_buf.resize(values.len(), 0);
        }
        let mut raw_steps = [(0u16, 0u16, 0u16, 0u32, 0u16); 128];
        if spec_len == 1 {
            let spec = specs[0];
            let (layer_values, step_len, in_cycles) = emit_tick_layer(
                plans,
                spec,
                values,
                local_buf,
                held_buf,
                &mut raw_steps,
                true,
            );
            let period = if spec.total_cycles == 1 {
                Fraction::ONE
            } else {
                Fraction::int(i128::from(spec.total_cycles))
            };
            let ticks = std::num::NonZeroU32::new(spec.ticks)?;
            return sequence_of_ticks(
                period,
                ticks,
                layer_values,
                in_cycles,
                &raw_steps[..step_len],
            )
            .map(|pattern| pattern.with_steps(steps));
        }
        let mut builder = SequenceBuilder::with_capacity(spec_len);
        let orig_values_len = values.len();
        let shared_values: Option<Arc<[Value]>> = if plans.sites.is_empty() {
            build_shared_values(values, local_buf, held_buf)
        } else {
            None
        };
        let built = (|| -> Option<()> {
            for &spec in &specs[..spec_len] {
                let (layer_values, step_len, in_cycles) = if let Some(shared) = &shared_values {
                    let (step_len, in_cycles) =
                        emit_tick_layer_shared(plans, spec, local_buf, &mut raw_steps);
                    (Arc::clone(shared), step_len, in_cycles)
                } else {
                    emit_tick_layer(
                        plans,
                        spec,
                        values,
                        local_buf,
                        held_buf,
                        &mut raw_steps,
                        false,
                    )
                };
                let period = if spec.total_cycles == 1 {
                    Fraction::ONE
                } else {
                    Fraction::int(i128::from(spec.total_cycles))
                };
                let ticks = std::num::NonZeroU32::new(spec.ticks)?;
                if !builder.push_ticks(
                    period,
                    ticks,
                    layer_values,
                    in_cycles,
                    &raw_steps[..step_len],
                ) {
                    return None;
                }
            }
            Some(())
        })();
        if shared_values.is_some() {
            local_buf[..orig_values_len].fill(0);
        }
        built?;
        Some(builder.finish()?.with_steps(steps))
    }

    /// Direct integer-tick fast path for single-layer equal sequences holding
    /// alternations, whole speeds (`*N`), and whole slow counts (`/N`) — plus
    /// top-level `/N` and `*N` of an atom or single-lane equal alternation.
    fn try_cut_tick_pattern(&mut self, steps: Option<Fraction>) -> Option<Pattern> {
        let Room {
            plans,
            values,
            local_buf,
            held_buf,
            ..
        } = &mut *self.room;
        #[derive(Clone, Copy)]
        enum FlatSlot {
            Rest,
            Atom {
                value: u16,
                begin: u32,
                len: u16,
                extra: (u32, u16),
            },
            Alt {
                ids: Among,
                extra: (u32, u16),
            },
            SlowAtom {
                value: u16,
                begin: u32,
                len: u16,
                times: u32,
                extra: (u32, u16),
            },
            SlowAlt {
                ids: Among,
                times: u32,
                extra: (u32, u16),
            },
            FastAtom {
                value: u16,
                begin: u32,
                len: u16,
                rate: u32,
                extra: (u32, u16),
            },
            FastAlt {
                ids: Among,
                rate: u32,
                extra: (u32, u16),
            },
        }
        let extra_of = |plans: &Plans, site_idx: u16| -> Option<(u32, u16)> {
            let (b, e) = plans.sites[site_idx as usize];
            let len = e.checked_sub(b)?;
            if len == 0 || len >= 255 {
                return None;
            }
            Some((b, len as u16))
        };
        let flat_alt_ids = |plans: &Plans, id: Id| -> Option<Among> {
            let Kind::Alternation(lanes) = plans.kind(id) else {
                return None;
            };
            let [lane] = plans.lanes[lanes.range()] else {
                return None;
            };
            if !lane.equal || lane.parts.range().is_empty() {
                return None;
            }
            for &child in &plans.ids[lane.parts.range()] {
                match plans.kind(child) {
                    Kind::Rest => {}
                    Kind::Atom { value, len, .. } => {
                        if u16::try_from(value).is_err() || len >= 255 {
                            return None;
                        }
                    }
                    _ => return None,
                }
            }
            Some(lane.parts)
        };
        let classify = |plans: &Plans, id: Id| -> Option<FlatSlot> {
            match plans.kind(id) {
                Kind::Rest => Some(FlatSlot::Rest),
                Kind::Atom { value, begin, len } => {
                    if len >= 255 {
                        return None;
                    }
                    Some(FlatSlot::Atom {
                        value: u16::try_from(value).ok()?,
                        begin,
                        len,
                        extra: (0, 0),
                    })
                }
                Kind::Alternation(_) => Some(FlatSlot::Alt {
                    ids: flat_alt_ids(plans, id)?,
                    extra: (0, 0),
                }),
                Kind::Slower(inner, times, speed) if (1..=64).contains(&times) => {
                    let extra = extra_of(plans, speed)?;
                    match plans.kind(inner) {
                        Kind::Rest => Some(FlatSlot::Rest),
                        Kind::Atom { value, begin, len } if len > 0 && len < 255 => {
                            Some(FlatSlot::SlowAtom {
                                value: u16::try_from(value).ok()?,
                                begin,
                                len,
                                times,
                                extra,
                            })
                        }
                        Kind::Alternation(_) => Some(FlatSlot::SlowAlt {
                            ids: flat_alt_ids(plans, inner)?,
                            times,
                            extra,
                        }),
                        _ => None,
                    }
                }
                Kind::Faster(inner, rate, speed, true) if (1..=16).contains(&rate) => {
                    let extra = extra_of(plans, speed)?;
                    match plans.kind(inner) {
                        Kind::Rest => Some(FlatSlot::Rest),
                        Kind::Atom { value, begin, len } if len > 0 && len < 255 => {
                            Some(FlatSlot::FastAtom {
                                value: u16::try_from(value).ok()?,
                                begin,
                                len,
                                rate,
                                extra,
                            })
                        }
                        Kind::Alternation(_) => Some(FlatSlot::FastAlt {
                            ids: flat_alt_ids(plans, inner)?,
                            rate,
                            extra,
                        }),
                        _ => None,
                    }
                }
                _ => None,
            }
        };
        let mut raw_steps = [TickStep {
            part_begin: 0,
            part_end: 0,
            value: 0,
            loc0_begin: 0,
            loc0_len: 0,
            loc1_begin: 0,
            loc1_len: 0,
        }; 128];
        let mut cuts = [TickCut {
            index: 0,
            whole_begin: 0,
            whole_end: 0,
        }; 128];
        let mut step_len = 0usize;
        let mut cut_len = 0usize;
        let mut in_cycles = true;
        let (period, ticks) = match plans.kind(self.root) {
            Kind::Slower(..) | Kind::Faster(..) => {
                let slot = classify(plans, self.root)?;
                match slot {
                    FlatSlot::Rest => return Some(silence().with_steps(steps)),
                    FlatSlot::SlowAtom {
                        value,
                        begin,
                        len,
                        times,
                        extra,
                    } => {
                        if times > u32::from(u16::MAX) {
                            return None;
                        }
                        in_cycles = times == 1;
                        raw_steps[0] = TickStep {
                            part_begin: 0,
                            part_end: times as u16,
                            value,
                            loc0_begin: begin,
                            loc0_len: len,
                            loc1_begin: extra.0,
                            loc1_len: extra.1,
                        };
                        step_len = 1;
                        let period = if times == 1 {
                            Fraction::ONE
                        } else {
                            Fraction::int(i128::from(times))
                        };
                        (period, 1u32)
                    }
                    FlatSlot::SlowAlt { ids, times, extra } => {
                        let slice = &plans.ids[ids.range()];
                        let p = slice.len() as u32;
                        let period_int = p.checked_mul(times)?;
                        if period_int > u32::from(u16::MAX) || slice.len() > raw_steps.len() {
                            return None;
                        }
                        in_cycles = times == 1;
                        for (i, &child) in slice.iter().enumerate() {
                            if let Kind::Atom { value, begin, len } = plans.kind(child) {
                                if len == 0 {
                                    return None;
                                }
                                let b = (i as u32) * times;
                                raw_steps[step_len] = TickStep {
                                    part_begin: b as u16,
                                    part_end: (b + times) as u16,
                                    value: value as u16,
                                    loc0_begin: begin,
                                    loc0_len: len,
                                    loc1_begin: extra.0,
                                    loc1_len: extra.1,
                                };
                                step_len += 1;
                            }
                        }
                        let period = if period_int == 1 {
                            Fraction::ONE
                        } else {
                            Fraction::int(i128::from(period_int))
                        };
                        (period, 1u32)
                    }
                    FlatSlot::FastAtom {
                        value,
                        begin,
                        len,
                        rate,
                        extra,
                    } => {
                        // One step over period `1 / rate` fits `CompactGrid::OneStep`
                        // with zero heap grid allocation.
                        raw_steps[0] = TickStep {
                            part_begin: 0,
                            part_end: 1,
                            value,
                            loc0_begin: begin,
                            loc0_len: len,
                            loc1_begin: extra.0,
                            loc1_len: extra.1,
                        };
                        step_len = 1;
                        (Fraction::checked_new(1, i128::from(rate))?, rate)
                    }
                    FlatSlot::FastAlt { ids, rate, extra } => {
                        let slice = &plans.ids[ids.range()];
                        let p = slice.len() as u32;
                        if slice.len() > raw_steps.len() {
                            return None;
                        }
                        for (i, &child) in slice.iter().enumerate() {
                            if let Kind::Atom { value, begin, len } = plans.kind(child) {
                                if len == 0 {
                                    return None;
                                }
                                raw_steps[step_len] = TickStep {
                                    part_begin: i as u16,
                                    part_end: (i + 1) as u16,
                                    value: value as u16,
                                    loc0_begin: begin,
                                    loc0_len: len,
                                    loc1_begin: extra.0,
                                    loc1_len: extra.1,
                                };
                                step_len += 1;
                            }
                        }
                        (
                            Fraction::checked_new(i128::from(p), i128::from(rate))?,
                            rate,
                        )
                    }
                    _ => return None,
                }
            }
            Kind::EqualSequence(parts) => {
                let ids = &plans.ids[parts.range()];
                let k = ids.len();
                if !(2..=16).contains(&k) {
                    return None;
                }
                let mut slots = [FlatSlot::Rest; 16];
                let mut period_cycles = 1u32;
                let mut sub_ticks = 1u32;
                for (j, &child) in ids.iter().enumerate() {
                    let slot = classify(plans, child)?;
                    slots[j] = slot;
                    match slot {
                        FlatSlot::Rest | FlatSlot::Atom { .. } => {}
                        FlatSlot::Alt { ids, .. } => {
                            period_cycles = lcm_u32(period_cycles, ids.range().len() as u32)?;
                        }
                        FlatSlot::SlowAtom { times, .. } => {
                            period_cycles = lcm_u32(period_cycles, times)?;
                        }
                        FlatSlot::SlowAlt { ids, times, .. } => {
                            let p = (ids.range().len() as u32).checked_mul(times)?;
                            period_cycles = lcm_u32(period_cycles, p)?;
                        }
                        FlatSlot::FastAtom { rate, .. } => {
                            sub_ticks = lcm_u32(sub_ticks, rate)?;
                        }
                        FlatSlot::FastAlt { ids, rate, .. } => {
                            let p = ids.range().len() as u32;
                            period_cycles = lcm_u32(period_cycles, p / gcd_u32(p, rate))?;
                            sub_ticks = lcm_u32(sub_ticks, rate)?;
                        }
                    }
                    if period_cycles > 64 || sub_ticks > 16 {
                        return None;
                    }
                }
                let ticks = (k as u32).checked_mul(sub_ticks)?;
                if u64::from(period_cycles) * u64::from(ticks) > u64::from(u16::MAX) {
                    return None;
                }
                for t in 0..period_cycles {
                    let cycle_base = t * ticks;
                    for (j, &slot) in slots[..k].iter().enumerate() {
                        let slot_start = cycle_base + (j as u32) * sub_ticks;
                        match slot {
                            FlatSlot::Rest => {}
                            FlatSlot::Atom {
                                value,
                                begin,
                                len,
                                extra,
                            } => {
                                if step_len >= raw_steps.len() {
                                    return None;
                                }
                                raw_steps[step_len] = TickStep {
                                    part_begin: slot_start as u16,
                                    part_end: (slot_start + sub_ticks) as u16,
                                    value,
                                    loc0_begin: begin,
                                    loc0_len: len,
                                    loc1_begin: extra.0,
                                    loc1_len: extra.1,
                                };
                                step_len += 1;
                            }
                            FlatSlot::Alt { ids, extra } => {
                                let slice = &plans.ids[ids.range()];
                                let child = slice[(t as usize) % slice.len()];
                                if let Kind::Atom { value, begin, len } = plans.kind(child) {
                                    if step_len >= raw_steps.len() {
                                        return None;
                                    }
                                    raw_steps[step_len] = TickStep {
                                        part_begin: slot_start as u16,
                                        part_end: (slot_start + sub_ticks) as u16,
                                        value: value as u16,
                                        loc0_begin: begin,
                                        loc0_len: len,
                                        loc1_begin: extra.0,
                                        loc1_len: extra.1,
                                    };
                                    step_len += 1;
                                }
                            }
                            FlatSlot::SlowAtom {
                                value,
                                begin,
                                len,
                                times,
                                extra,
                            } => {
                                if step_len >= raw_steps.len() {
                                    return None;
                                }
                                let rem = t % times;
                                if times > 1 {
                                    let wb = i64::from(slot_start)
                                        - i64::from(rem) * i64::from(sub_ticks);
                                    let we = i64::from(slot_start)
                                        + i64::from(times - rem) * i64::from(sub_ticks);
                                    let wb = u16::try_from(wb).ok()?;
                                    let we = u16::try_from(we).ok()?;
                                    cuts[cut_len] = TickCut {
                                        index: step_len as u16,
                                        whole_begin: wb,
                                        whole_end: we,
                                    };
                                    cut_len += 1;
                                }
                                raw_steps[step_len] = TickStep {
                                    part_begin: slot_start as u16,
                                    part_end: (slot_start + sub_ticks) as u16,
                                    value,
                                    loc0_begin: begin,
                                    loc0_len: len,
                                    loc1_begin: extra.0,
                                    loc1_len: extra.1,
                                };
                                step_len += 1;
                            }
                            FlatSlot::SlowAlt { ids, times, extra } => {
                                let slice = &plans.ids[ids.range()];
                                let inner_cycle = (t / times) as usize;
                                let rem = t % times;
                                let child = slice[inner_cycle % slice.len()];
                                if let Kind::Atom { value, begin, len } = plans.kind(child) {
                                    if len == 0 || step_len >= raw_steps.len() {
                                        return None;
                                    }
                                    if times > 1 {
                                        let wb = i64::from(slot_start)
                                            - i64::from(rem) * i64::from(sub_ticks);
                                        let we = i64::from(slot_start)
                                            + i64::from(times - rem) * i64::from(sub_ticks);
                                        let wb = u16::try_from(wb).ok()?;
                                        let we = u16::try_from(we).ok()?;
                                        cuts[cut_len] = TickCut {
                                            index: step_len as u16,
                                            whole_begin: wb,
                                            whole_end: we,
                                        };
                                        cut_len += 1;
                                    }
                                    raw_steps[step_len] = TickStep {
                                        part_begin: slot_start as u16,
                                        part_end: (slot_start + sub_ticks) as u16,
                                        value: value as u16,
                                        loc0_begin: begin,
                                        loc0_len: len,
                                        loc1_begin: extra.0,
                                        loc1_len: extra.1,
                                    };
                                    step_len += 1;
                                }
                            }
                            FlatSlot::FastAtom {
                                value,
                                begin,
                                len,
                                rate,
                                extra,
                            } => {
                                let w = sub_ticks / rate;
                                for r in 0..rate {
                                    if step_len >= raw_steps.len() {
                                        return None;
                                    }
                                    let pb = slot_start + r * w;
                                    raw_steps[step_len] = TickStep {
                                        part_begin: pb as u16,
                                        part_end: (pb + w) as u16,
                                        value,
                                        loc0_begin: begin,
                                        loc0_len: len,
                                        loc1_begin: extra.0,
                                        loc1_len: extra.1,
                                    };
                                    step_len += 1;
                                }
                            }
                            FlatSlot::FastAlt { ids, rate, extra } => {
                                let slice = &plans.ids[ids.range()];
                                let w = sub_ticks / rate;
                                for r in 0..rate {
                                    let child = slice[((t * rate + r) as usize) % slice.len()];
                                    if let Kind::Atom { value, begin, len } = plans.kind(child) {
                                        if len == 0 || step_len >= raw_steps.len() {
                                            return None;
                                        }
                                        let pb = slot_start + r * w;
                                        raw_steps[step_len] = TickStep {
                                            part_begin: pb as u16,
                                            part_end: (pb + w) as u16,
                                            value: value as u16,
                                            loc0_begin: begin,
                                            loc0_len: len,
                                            loc1_begin: extra.0,
                                            loc1_len: extra.1,
                                        };
                                        step_len += 1;
                                    }
                                }
                            }
                        }
                    }
                }
                let period = if period_cycles == 1 {
                    Fraction::ONE
                } else {
                    Fraction::int(i128::from(period_cycles))
                };
                (period, ticks)
            }
            _ => return None,
        };
        if step_len == 0 {
            return Some(silence().with_steps(steps));
        }
        // Validate location bounds up front before moving values out of `Room`.
        let mut min_begin = u32::MAX;
        let mut max_begin = 0u32;
        for step in &raw_steps[..step_len] {
            if step.loc0_len > 0 {
                min_begin = min_begin.min(step.loc0_begin);
                max_begin = max_begin.max(step.loc0_begin);
            }
            if step.loc1_len > 0 {
                min_begin = min_begin.min(step.loc1_begin);
                max_begin = max_begin.max(step.loc1_begin);
            }
        }
        let base = if min_begin == u32::MAX { 0 } else { min_begin };
        if base >= (1 << 31) || (min_begin != u32::MAX && max_begin - base >= (1 << 22)) {
            return None;
        }
        let layer_values: Arc<[Value]> = if values.len() <= 64 {
            let mut local = [0u16; 64];
            let mut held = [0u16; 64];
            let mut held_len = 0usize;
            for step in &mut raw_steps[..step_len] {
                let global_idx = usize::from(step.value);
                let slot = &mut local[global_idx];
                if *slot == 0 {
                    held[held_len] = global_idx as u16;
                    held_len += 1;
                    *slot = held_len as u16;
                }
                step.value = *slot - 1;
            }
            held[..held_len]
                .iter()
                .map(|&idx| std::mem::replace(&mut values[usize::from(idx)], Value::Undefined))
                .collect()
        } else {
            if local_buf.len() < values.len() {
                local_buf.resize(values.len(), 0);
            }
            held_buf.clear();
            for step in &mut raw_steps[..step_len] {
                let global_idx = usize::from(step.value);
                let slot = &mut local_buf[global_idx];
                if *slot == 0 {
                    held_buf.push(global_idx as u32);
                    *slot = held_buf.len() as u32;
                }
                step.value = (*slot - 1) as u16;
            }
            held_buf
                .iter()
                .map(|&idx| {
                    local_buf[idx as usize] = 0;
                    std::mem::replace(&mut values[idx as usize], Value::Undefined)
                })
                .collect()
        };
        let ticks = std::num::NonZeroU32::new(ticks)?;
        sequence_of_cut_ticks(
            period,
            ticks,
            layer_values,
            in_cycles,
            &raw_steps[..step_len],
            &cuts[..cut_len],
        )
        .map(|pattern| pattern.with_steps(steps))
    }
}

#[derive(Clone, Copy)]
enum TickLayerSource {
    Node(Id),
    EqualLane(Steps),
    WeightedStep { id: Id, from: u32, to: u32, of: u32 },
}

#[derive(Clone, Copy)]
struct TickLayerSpec {
    source: TickLayerSource,
    total_cycles: u32,
    ticks: u32,
}

#[derive(Clone, Copy)]
struct TickCheck {
    ticks: u32,
    steps: usize,
    min_begin: u32,
    max_begin: u32,
    max_len: u16,
}

impl TickCheck {
    fn new() -> Self {
        Self {
            ticks: 1,
            steps: 0,
            min_begin: u32::MAX,
            max_begin: 0,
            max_len: 0,
        }
    }

    fn locs_fit(&self) -> bool {
        let base = if self.min_begin == u32::MAX {
            0
        } else {
            self.min_begin
        };
        base < (1 << 31)
            && (self.min_begin == u32::MAX || self.max_begin - base < (1 << 22))
            && self.max_len < 255
    }
}

fn gcd_u32(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a
}

fn lcm_u32(a: u32, b: u32) -> Option<u32> {
    (a / gcd_u32(a, b)).checked_mul(b)
}

fn check_tick_tree(
    plans: &Plans,
    id: Id,
    w_num: u32,
    w_den: u32,
    total_cycles: u32,
    depth: u8,
    acc: &mut TickCheck,
) -> bool {
    if depth > 4 {
        return false;
    }
    match plans.kind(id) {
        Kind::Rest => {
            let Some(next_ticks) = lcm_u32(acc.ticks, w_den) else {
                return false;
            };
            if u64::from(total_cycles) * u64::from(next_ticks) > u64::from(u16::MAX) {
                return false;
            }
            acc.ticks = next_ticks;
            true
        }
        Kind::Atom { value, begin, len } => {
            if u16::try_from(value).is_err() {
                return false;
            }
            acc.steps += 1;
            if acc.steps > 128 {
                return false;
            }
            let Some(next_ticks) = lcm_u32(acc.ticks, w_den) else {
                return false;
            };
            if u64::from(total_cycles) * u64::from(next_ticks) > u64::from(u16::MAX) {
                return false;
            }
            acc.ticks = next_ticks;
            if len > 0 {
                acc.min_begin = acc.min_begin.min(begin);
                acc.max_begin = acc.max_begin.max(begin);
                acc.max_len = acc.max_len.max(len);
            }
            true
        }
        Kind::Repeated(inner, times) => {
            if times == 0 || times > 128 {
                return false;
            }
            let g = gcd_u32(w_num, times);
            let c_num = w_num / g;
            let Some(c_den) = w_den.checked_mul(times / g) else {
                return false;
            };
            let mut sub = TickCheck {
                ticks: acc.ticks,
                steps: 0,
                min_begin: acc.min_begin,
                max_begin: acc.max_begin,
                max_len: acc.max_len,
            };
            if !check_tick_tree(
                plans,
                inner,
                c_num,
                c_den,
                total_cycles,
                depth + 1,
                &mut sub,
            ) {
                return false;
            }
            let Some(added) = sub.steps.checked_mul(times as usize) else {
                return false;
            };
            if acc.steps + added > 128 {
                return false;
            }
            acc.steps += added;
            acc.ticks = sub.ticks;
            acc.min_begin = sub.min_begin;
            acc.max_begin = sub.max_begin;
            acc.max_len = sub.max_len;
            true
        }
        Kind::EqualSequence(parts) => {
            let steps = Steps { parts, equal: true };
            let sub_total = plans.total(steps);
            if sub_total == 0 {
                return false;
            }
            let g2 = gcd_u32(w_num, sub_total);
            let c_num = w_num / g2;
            let Some(c_den) = w_den.checked_mul(sub_total / g2) else {
                return false;
            };
            for &child in &plans.ids[parts.range()] {
                if !check_tick_tree(plans, child, c_num, c_den, total_cycles, depth + 1, acc) {
                    return false;
                }
            }
            true
        }
        Kind::WeightedSequence(parts) if depth > 0 || w_den > 1 || total_cycles > 1 => {
            let steps = Steps {
                parts,
                equal: false,
            };
            let sub_total = plans.total(steps);
            if sub_total == 0 {
                return false;
            }
            for (child, (from, to)) in plans.each(steps) {
                let weight = to - from;
                let g1 = gcd_u32(w_num, sub_total);
                let num1 = w_num / g1;
                let den1 = sub_total / g1;
                let g2 = gcd_u32(weight, den1);
                let c_num = match num1.checked_mul(weight / g2) {
                    Some(v) => v,
                    None => return false,
                };
                let c_den = match w_den.checked_mul(den1 / g2) {
                    Some(v) => v,
                    None => return false,
                };
                // Also ensure `from` boundary divides cleanly on `acc.ticks`.
                let g_off = gcd_u32(w_num, sub_total);
                let off_den = sub_total / g_off;
                let g_f = gcd_u32(from, off_den);
                let Some(start_den) = w_den.checked_mul(off_den / g_f) else {
                    return false;
                };
                let Some(next_ticks) = lcm_u32(acc.ticks, start_den) else {
                    return false;
                };
                if u64::from(total_cycles) * u64::from(next_ticks) > u64::from(u16::MAX) {
                    return false;
                }
                acc.ticks = next_ticks;
                if !check_tick_tree(plans, child, c_num, c_den, total_cycles, depth + 1, acc) {
                    return false;
                }
            }
            true
        }
        kind @ (Kind::Stack(patterns) | Kind::StackRun(patterns))
            if total_cycles > 1 || w_den > 1 =>
        {
            let mut any = false;
            for child in plans.stacked(patterns, kind.is_run()) {
                match plans.kind(child) {
                    Kind::Rest => {}
                    Kind::Atom { value, begin, len } => {
                        if u16::try_from(value).is_err() {
                            return false;
                        }
                        acc.steps += 1;
                        if acc.steps > 128 {
                            return false;
                        }
                        any = true;
                        if len > 0 {
                            acc.min_begin = acc.min_begin.min(begin);
                            acc.max_begin = acc.max_begin.max(begin);
                            acc.max_len = acc.max_len.max(len);
                        }
                    }
                    _ => return false,
                }
            }
            if any {
                let Some(next_ticks) = lcm_u32(acc.ticks, w_den) else {
                    return false;
                };
                if u64::from(total_cycles) * u64::from(next_ticks) > u64::from(u16::MAX) {
                    return false;
                }
                acc.ticks = next_ticks;
            }
            true
        }
        _ => false,
    }
}

fn collect_tick_layers(
    plans: &Plans,
    id: Id,
    out: &mut [TickLayerSpec; 64],
    len: &mut usize,
    depth: u8,
) -> bool {
    if depth > 2 {
        return false;
    }
    match plans.kind(id) {
        Kind::Rest => true,
        Kind::Atom { .. } | Kind::Repeated(..) | Kind::EqualSequence(_) => {
            if *len >= out.len() {
                return false;
            }
            let mut acc = TickCheck::new();
            if !check_tick_tree(plans, id, 1, 1, 1, 0, &mut acc) || !acc.locs_fit() {
                return false;
            }
            if acc.steps > 0 {
                out[*len] = TickLayerSpec {
                    source: TickLayerSource::Node(id),
                    total_cycles: 1,
                    ticks: acc.ticks,
                };
                *len += 1;
            }
            true
        }
        Kind::WeightedSequence(parts) => {
            let steps = Steps {
                parts,
                equal: false,
            };
            let sub_total = plans.total(steps);
            if sub_total == 0 {
                return false;
            }
            for (child, (from, to)) in plans.each(steps) {
                if matches!(plans.kind(child), Kind::Rest) {
                    continue;
                }
                if *len >= out.len() {
                    return false;
                }
                let w = to - from;
                let g_start = gcd_u32(from, sub_total);
                let g_w = gcd_u32(w, sub_total);
                let mut acc = TickCheck::new();
                acc.ticks = sub_total / g_start;
                if acc.ticks > u32::from(u16::MAX) {
                    return false;
                }
                if !check_tick_tree(plans, child, w / g_w, sub_total / g_w, 1, 0, &mut acc)
                    || !acc.locs_fit()
                {
                    return false;
                }
                if acc.steps > 0 {
                    out[*len] = TickLayerSpec {
                        source: TickLayerSource::WeightedStep {
                            id: child,
                            from,
                            to,
                            of: sub_total,
                        },
                        total_cycles: 1,
                        ticks: acc.ticks,
                    };
                    *len += 1;
                }
            }
            true
        }
        Kind::Alternation(lanes) => {
            for &lane in &plans.lanes[lanes.range()] {
                let total = plans.total(lane);
                if total == 0 || total > u32::from(u16::MAX) {
                    return false;
                }
                if lane.equal {
                    if *len >= out.len() {
                        return false;
                    }
                    let mut acc = TickCheck::new();
                    for &child in &plans.ids[lane.parts.range()] {
                        if !check_tick_tree(plans, child, 1, 1, total, 0, &mut acc) {
                            return false;
                        }
                    }
                    if !acc.locs_fit() {
                        return false;
                    }
                    if acc.steps > 0 {
                        out[*len] = TickLayerSpec {
                            source: TickLayerSource::EqualLane(lane),
                            total_cycles: total,
                            ticks: acc.ticks,
                        };
                        *len += 1;
                    }
                } else {
                    for (child, (from, to)) in plans.each(lane) {
                        if matches!(plans.kind(child), Kind::Rest) {
                            continue;
                        }
                        if *len >= out.len() {
                            return false;
                        }
                        let mut acc = TickCheck::new();
                        if !check_tick_tree(plans, child, to - from, 1, total, 0, &mut acc)
                            || !acc.locs_fit()
                        {
                            return false;
                        }
                        if acc.steps > 0 {
                            out[*len] = TickLayerSpec {
                                source: TickLayerSource::WeightedStep {
                                    id: child,
                                    from,
                                    to,
                                    of: 1,
                                },
                                total_cycles: total,
                                ticks: acc.ticks,
                            };
                            *len += 1;
                        }
                    }
                }
            }
            true
        }
        kind @ (Kind::Stack(patterns) | Kind::StackRun(patterns)) => {
            for child in plans.stacked(patterns, kind.is_run()) {
                if !collect_tick_layers(plans, child, out, len, depth + 1) {
                    return false;
                }
            }
            true
        }
        _ => false,
    }
}

struct TickEmit<'a> {
    ticks: u32,
    out: &'a mut [(u16, u16, u16, u32, u16); 128],
    step_len: usize,
    in_cycles: bool,
}

fn emit_tick_tree(
    plans: &Plans,
    id: Id,
    start_tick: u32,
    width_ticks: u32,
    emit: &mut TickEmit<'_>,
) {
    match plans.kind(id) {
        Kind::Rest => {}
        Kind::Atom { value, begin, len } => {
            let end_tick = start_tick + width_ticks;
            if start_tick / emit.ticks != (end_tick - 1) / emit.ticks {
                emit.in_cycles = false;
            }
            emit.out[emit.step_len] =
                (start_tick as u16, end_tick as u16, value as u16, begin, len);
            emit.step_len += 1;
        }
        Kind::Repeated(inner, times) => {
            let sub_width = width_ticks / times;
            for j in 0..times {
                emit_tick_tree(plans, inner, start_tick + j * sub_width, sub_width, emit);
            }
        }
        Kind::EqualSequence(parts) => {
            let ids = &plans.ids[parts.range()];
            let sub_width = width_ticks / (ids.len() as u32);
            for (i, &child) in ids.iter().enumerate() {
                emit_tick_tree(
                    plans,
                    child,
                    start_tick + (i as u32) * sub_width,
                    sub_width,
                    emit,
                );
            }
        }
        Kind::WeightedSequence(parts) => {
            let steps = Steps {
                parts,
                equal: false,
            };
            let sub_total = u64::from(plans.total(steps));
            for (child, (from, to)) in plans.each(steps) {
                let offset = (u64::from(from) * u64::from(width_ticks) / sub_total) as u32;
                let sub_width = (u64::from(to - from) * u64::from(width_ticks) / sub_total) as u32;
                emit_tick_tree(plans, child, start_tick + offset, sub_width, emit);
            }
        }
        kind @ (Kind::Stack(patterns) | Kind::StackRun(patterns)) => {
            for child in plans.stacked(patterns, kind.is_run()) {
                emit_tick_tree(plans, child, start_tick, width_ticks, emit);
            }
        }
        _ => unreachable!("validated by check_tick_tree"),
    }
}

fn build_shared_values(
    values: &mut Vec<Value>,
    local_buf: &mut Vec<u32>,
    held_buf: &mut Vec<u32>,
) -> Option<Arc<[Value]>> {
    if values.len() > 64
        || !values
            .iter()
            .all(|v| matches!(v, Value::F64(_) | Value::Str(_) | Value::Bool(_)))
    {
        return None;
    }
    if local_buf.len() < values.len() {
        local_buf.resize(values.len(), 0);
    }
    held_buf.clear();
    for i in 0..values.len() {
        let v = &values[i];
        let found = held_buf
            .iter()
            .position(|&u| match (&values[u as usize], v) {
                (Value::F64(a), Value::F64(b)) => a.to_bits() == b.to_bits(),
                (Value::Str(a), Value::Str(b)) => a == b,
                (Value::Bool(a), Value::Bool(b)) => a == b,
                _ => false,
            });
        match found {
            Some(pos) => local_buf[i] = pos as u32,
            None => {
                local_buf[i] = held_buf.len() as u32;
                held_buf.push(i as u32);
            }
        }
    }
    let shared: Arc<[Value]> = held_buf
        .iter()
        .map(|&idx| std::mem::replace(&mut values[idx as usize], Value::Undefined))
        .collect();
    values.clear();
    Some(shared)
}

fn emit_tick_layer_shared(
    plans: &Plans,
    spec: TickLayerSpec,
    remap: &[u32],
    raw_steps: &mut [(u16, u16, u16, u32, u16); 128],
) -> (usize, bool) {
    let mut emit = TickEmit {
        ticks: spec.ticks,
        out: raw_steps,
        step_len: 0,
        in_cycles: true,
    };
    match spec.source {
        TickLayerSource::Node(id) => {
            emit_tick_tree(plans, id, 0, spec.ticks, &mut emit);
        }
        TickLayerSource::EqualLane(lane) => {
            for (i, &child) in plans.ids[lane.parts.range()].iter().enumerate() {
                emit_tick_tree(plans, child, (i as u32) * spec.ticks, spec.ticks, &mut emit);
            }
        }
        TickLayerSource::WeightedStep { id, from, to, of } => {
            let start = (u64::from(from) * u64::from(spec.ticks) / u64::from(of)) as u32;
            let width = (u64::from(to - from) * u64::from(spec.ticks) / u64::from(of)) as u32;
            emit_tick_tree(plans, id, start, width, &mut emit);
        }
    }
    let step_len = emit.step_len;
    let in_cycles = emit.in_cycles;
    for step in &mut raw_steps[..step_len] {
        step.2 = remap[step.2 as usize] as u16;
    }
    (step_len, in_cycles)
}

fn emit_tick_layer(
    plans: &Plans,
    spec: TickLayerSpec,
    values: &mut [Value],
    local_buf: &mut [u32],
    held_buf: &mut Vec<u32>,
    raw_steps: &mut [(u16, u16, u16, u32, u16); 128],
    take_values: bool,
) -> (Arc<[Value]>, usize, bool) {
    let mut emit = TickEmit {
        ticks: spec.ticks,
        out: raw_steps,
        step_len: 0,
        in_cycles: true,
    };
    match spec.source {
        TickLayerSource::Node(id) => {
            emit_tick_tree(plans, id, 0, spec.ticks, &mut emit);
        }
        TickLayerSource::EqualLane(lane) => {
            for (i, &child) in plans.ids[lane.parts.range()].iter().enumerate() {
                emit_tick_tree(plans, child, (i as u32) * spec.ticks, spec.ticks, &mut emit);
            }
        }
        TickLayerSource::WeightedStep { id, from, to, of } => {
            let start = (u64::from(from) * u64::from(spec.ticks) / u64::from(of)) as u32;
            let width = (u64::from(to - from) * u64::from(spec.ticks) / u64::from(of)) as u32;
            emit_tick_tree(plans, id, start, width, &mut emit);
        }
    }
    let step_len = emit.step_len;
    let in_cycles = emit.in_cycles;
    held_buf.clear();
    for step in &mut raw_steps[..step_len] {
        let global_idx = usize::from(step.2);
        let slot = &mut local_buf[global_idx];
        if *slot == 0 {
            held_buf.push(global_idx as u32);
            *slot = held_buf.len() as u32;
        }
        step.2 = (*slot - 1) as u16;
    }
    let layer_values: Arc<[Value]> = held_buf
        .iter()
        .map(|&idx| {
            local_buf[idx as usize] = 0;
            if take_values {
                std::mem::replace(&mut values[idx as usize], Value::Undefined)
            } else {
                values[idx as usize].clone()
            }
        })
        .collect();
    (layer_values, step_len, in_cycles)
}

/// The count of the first `len` steps that `is_past` is true of, which are
/// the steps before the first that is not: `is_past` is false after them.
fn passed(len: usize, is_past: impl Fn(usize) -> bool) -> usize {
    let (mut low, mut high) = (0, len);
    while low < high {
        let middle = low + (high - low) / 2;
        match is_past(middle) {
            true => low = middle + 1,
            false => high = middle,
        }
    }
    low
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
        // Most mini literals have only a handful of distinct words: scan
        // linearly up to 12 entries to avoid zeroing a 64..512-slot hash table.
        if self.room.known.is_empty() && self.room.words.len() < 12 {
            let wb = word.as_bytes();
            let want_len = wb.len();
            let src_bytes = self.src.as_bytes();
            if let Some(&b0) = wb.first() {
                for &((s, e), val) in &self.room.words {
                    if e - s == want_len
                        && src_bytes[s] == b0
                        && (want_len == 1 || &src_bytes[s..e] == wb)
                    {
                        return Some(val);
                    }
                }
            } else {
                for &((s, e), val) in &self.room.words {
                    if e == s {
                        return Some(val);
                    }
                }
            }
            return self.learn(word, end);
        }
        if self.room.known.is_empty() {
            // Two slots for each word that the text may hold: a word and
            // its space are two bytes or more.
            let slots = (self.src.len() + 2).next_power_of_two().max(64);
            self.room.known.resize(slots, 0);
            let mask = slots - 1;
            for i in 0..self.room.words.len() {
                let mut pos = self.slot(self.word(i));
                while self.room.known[pos] != 0 {
                    pos = (pos + 1) & mask;
                }
                self.room.known[pos] = (i + 1) as u32;
            }
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
                Kind::atom(self.value(word, span.end)?, at)?
            }
        };
        let facts = Facts {
            turns: 1,
            steps: 1,
            layers: u32::from(matches!(kind, Kind::Atom { .. })),
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

    fn alternation(
        &mut self,
        lanes: impl ExactSizeIterator<Item = Option<Steps>>,
        _: Span,
    ) -> Option<Id> {
        self.room.plans.alternation(lanes)
    }

    fn stack(&mut self, items: impl ExactSizeIterator<Item = Option<Id>>, _: Span) -> Option<Id> {
        self.room.plans.stack(items)
    }

    fn choice(
        &mut self,
        _: impl ExactSizeIterator<Item = Option<Id>>,
        _: u32,
        _: Span,
    ) -> Option<Id> {
        None
    }

    fn feet(
        &mut self,
        _: impl ExactSizeIterator<Item = Option<Id>>,
        _: u32,
        _: Span,
    ) -> Option<Id> {
        None
    }

    fn polymeter(
        &mut self,
        _: impl ExactSizeIterator<Item = Option<Id>>,
        _: Option<Option<Id>>,
        _: Span,
    ) -> Option<Id> {
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
            Postfix::Euclid {
                pulses,
                steps,
                rotation,
            } => {
                let rotation = match rotation {
                    Some(r) => Some(r?),
                    None => None,
                };
                plans.euclid(values, pattern?, pulses?, steps?, rotation)
            }
            Postfix::Tail(element) => {
                let at = written_at(self.src, self.offset, &span)?;
                plans.tail(values, pattern?, element?, at)
            }
            _ => None,
        }
    }

    fn take_steps(&mut self) -> Vec<crate::Slice<Self::Step>> {
        let mut steps = std::mem::take(&mut self.room.parser_steps);
        steps.clear();
        steps
    }

    fn return_steps(&mut self, steps: Vec<crate::Slice<Self::Step>>) {
        self.room.parser_steps = steps;
    }

    fn take_items(&mut self) -> Vec<Self::Node> {
        let mut items = std::mem::take(&mut self.room.parser_items);
        items.clear();
        items
    }

    fn return_items(&mut self, items: Vec<Self::Node>) {
        self.room.parser_items = items;
    }

    fn take_lanes(&mut self) -> Vec<Self::Lane> {
        let mut lanes = std::mem::take(&mut self.room.parser_lanes);
        lanes.clear();
        lanes
    }

    fn return_lanes(&mut self, lanes: Vec<Self::Lane>) {
        self.room.parser_lanes = lanes;
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

    #[test]
    fn a_plan_holds_few_bytes() {
        use std::mem::size_of;
        assert!(size_of::<PackedKind>() <= 8);
        assert!(size_of::<Id>() <= 2);
        assert!(size_of::<Part>() <= 8);
        assert!(size_of::<Root>() <= 20);
        assert!(size_of::<Pace>() <= 8);
        assert!(size_of::<LayerPlan>() <= 56);
        assert!(size_of::<PlanLayer>() <= 72);
    }

    fn plans_of(text: &str, room: &mut Room) -> Plans {
        assert!(Planned::read(text, 0, room).unwrap().is_some());
        std::mem::take(&mut room.plans)
    }

    /// Steps of the weight one are plain ids with no weights.
    #[test]
    fn equal_steps_keep_ids_and_no_parts() {
        let plans = plans_of("<a b c d e>", &mut Room::default());
        assert!(plans.parts.is_empty());
        // The five steps of the lane, and no list for an atom alone.
        assert_eq!(plans.ids.len(), 5);
    }

    /// Steps of other weights keep the end of each, and read the start of a
    /// step from the end of the step before it.
    #[test]
    fn weighted_steps_read_their_units_from_their_ends() {
        let plans = plans_of("<a@3 b c@2>", &mut Room::default());
        assert!(plans.ids.is_empty());
        let lane = plans.lanes[0];
        assert!(!lane.equal);
        assert_eq!(plans.total(lane), 6);
        let units: Vec<_> = plans.each(lane).map(|(_, units)| units).collect();
        assert_eq!(units, [(0, 3), (3, 4), (4, 6)]);
        assert_eq!(passed(3, |at| plans.step(lane, at).1.1 <= 3), 1);
    }

    /// A list that a pattern keeps has no spare room.
    #[test]
    fn kept_plans_hold_no_spare_room() {
        let mut room = Room::default();
        let text = "<[a,b,c] [d,e,f]@2 g h i j k l>*2";
        let kept = plans_of(text, &mut room).for_walks();
        assert!(kept.facts.is_empty());
        assert_eq!(kept.kinds.capacity(), kept.kinds.len());
        assert_eq!(kept.parts.capacity(), kept.parts.len());
        assert_eq!(kept.ids.capacity(), kept.ids.len());
        assert_eq!(kept.sites.capacity(), kept.sites.len());
        assert_eq!(kept.stacked.capacity(), kept.stacked.len());
        assert_eq!(kept.lanes.capacity(), kept.lanes.len());
    }

    /// A stack of words keeps no list of its members, and a stack of other
    /// constructs does.
    #[test]
    fn a_stack_of_words_keeps_no_list() {
        let plans = plans_of("<[a,b,c] [d,e,f]>", &mut Room::default());
        assert!(plans.stacked.is_empty());
        let plans = plans_of("<[a,b c] [d,e]>", &mut Room::default());
        assert_eq!(plans.stacked.len(), 2);
        let run = |plans: &Plans, id: usize| match plans.kind(Id(id as u16)) {
            Kind::StackRun(patterns) => plans.stacked(patterns, true).map(|id| id.0).collect(),
            _ => Vec::new(),
        };
        let plans = plans_of("[a,b,c]", &mut Room::default());
        assert_eq!(run(&plans, 3), [0, 1, 2]);
    }

    /// A sequence of one step is its step: it keeps no list.
    #[test]
    fn a_sequence_of_one_step_keeps_no_list() {
        let plans = plans_of("[[a]] b", &mut Room::default());
        assert_eq!(plans.ids.len(), 2);
        assert!(plans.parts.is_empty());
    }

    #[test]
    fn a_pace_stays_exact_and_in_lowest_terms() {
        let pace = Pace::ONE.times(6, 1).unwrap().times(1, 4).unwrap();
        assert_eq!((pace.numer, pace.denom), (3, 2));
        assert!(Pace::ONE.times(u32::MAX, 1).unwrap().times(2, 1).is_none());
        assert!(
            Pace::ONE
                .times(1 << 20, 1)
                .unwrap()
                .times(1, 1 << 20)
                .is_some_and(|pace| pace.terms() == (1, 1))
        );
    }

    /// The places of the speeds above a layer come innermost first, one
    /// list for the layer.
    #[test]
    fn a_layer_lists_the_places_of_its_speeds_innermost_first() {
        let mut room = Room::default();
        let planned = Planned::read("<a b c d e>*2/3", 0, &mut room)
            .unwrap()
            .unwrap();
        let layers = planned.layers().unwrap();
        let after = layers[0].0.after.as_slice();
        assert_eq!(after.len(), 2);
        assert!(after[0].0 < after[1].0);
    }

    #[test]
    fn a_pace_refuses_a_zero() {
        assert!(Pace::ONE.times(0, 1).is_none());
        assert!(Pace::ONE.times(1, 0).is_none());
        assert!(Pace::ONE.times(0, 0).is_none());
    }

    /// A layer under no speed keeps no list of places.
    #[test]
    fn a_layer_under_no_speed_has_an_empty_after() {
        let mut room = Room::default();
        let planned = Planned::read("<a b c d e>", 0, &mut room).unwrap().unwrap();
        let layers = planned.layers().unwrap();
        assert!(layers[0].0.after.as_slice().is_empty());
        let planned = Planned::read("<a b c d e>*2", 0, &mut room)
            .unwrap()
            .unwrap();
        let layers = planned.layers().unwrap();
        assert_eq!(layers[0].0.after.as_slice().len(), 1);
    }

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
