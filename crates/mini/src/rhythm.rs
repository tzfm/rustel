//! The plain values of a mini-notation pattern, at places that are known
//! when the pattern is built.

use rustel_core::{
    Locations, Pattern, SequenceLayer, SequenceStep, Value, pure, sequence, silence,
};
use rustel_fraction::Fraction;

/// The most steps one layer holds. A pattern with more keeps a node for
/// each value.
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
        let mut steps = Vec::new();
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
                Some(step) if step.locations().is_empty() => pure(step.value().clone()),
                Some(step) => {
                    pure(step.value().clone()).with_added_context(step.locations().to_vec())
                }
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
        let cycle = Fraction::int(turn);
        let width = window.1.checked_sub(window.0)?;
        let offset = cycle.checked_add(window.0)?;
        let turns = Fraction::int(turns);
        // A time of the cycle, at its place in the window, as a share of
        // the period.
        let place = |time: Fraction| {
            time.checked_mul(width)?
                .checked_add(offset)?
                .checked_div(turns)
        };
        for layer in &self.layers {
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
