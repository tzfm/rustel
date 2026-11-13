//! Values at places that are known when a pattern is built.
//!
//! A [`sequence`] holds the haps of one period of each of its layers, so a
//! query copies them and walks no graph. A constructor whose answer is
//! known from such haps alone folds it into them: a control of each value,
//! a constant that each hap meets, a stack, a change of speed. The chain
//! `s("bd*2 hh").bank("tr909").gain(0.8)` is then one node of three haps,
//! where it was a node for each call, each argument and each location.
//!
//! Each fold reads the graph and runs no query, so it answers under every
//! settings state as the nodes it stands for do. A fold that would raise a
//! query error leaves its node in place, which raises the error at each
//! query.

use crate::compose::ComposeOp;
use crate::context_keys::ContextKeys;
use crate::{
    Combine, Hap, Locations, LookupFlow, Meeting, Node, OwnedHapMapFn, Pattern, PickLookup,
    QueryLimit, State, TimeSpan, Value, push_budgeted, query_stop_requested, refuse, silence,
    without_query_signals,
};
use rustel_fraction::Fraction;
use std::sync::Arc;

/// The most steps one layer holds after a fold. A fold that makes more
/// keeps its own node.
const MAX_FOLDED_STEPS: usize = 256;

/// The largest numerator or denominator of a period after a fold. A speed
/// past it keeps its own node, which names itself when a time leaves the
/// native range.
const MAX_PERIOD_TERM: i128 = 1 << 20;

/// One step of a [`sequence`]: a hap of each period of its layer.
#[derive(Clone)]
pub struct SequenceStep {
    /// The whole of the hap, as shares of the period.
    whole: TimeSpan,
    /// The part of a hap that a meeting cut, inside the whole. `None` for a
    /// part that is the whole.
    part: Option<Box<TimeSpan>>,
    value: Value,
    locations: Locations,
    /// What few haps carry. `None` for a hap of a value and its locations.
    rest: Option<Box<Rest>>,
}

/// The fields of a hap that a plain value leaves empty.
#[derive(Clone)]
struct Rest {
    live_controls: [u64; 4],
    slider_binding: u64,
    ui_visuals: u64,
    pick_lookup: Option<Arc<PickLookup>>,
    keys: ContextKeys,
}

impl std::fmt::Debug for SequenceStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SequenceStep")
            .field("whole", &self.whole)
            .field("part", &self.part())
            .field("value", &self.value)
            .field("locations", &self.locations)
            .finish_non_exhaustive()
    }
}

impl SequenceStep {
    /// A step that holds `value` from `begin` to `end`, as shares of the
    /// period, at `locations`.
    pub fn new(begin: Fraction, end: Fraction, value: Value, locations: Locations) -> Self {
        Self {
            whole: TimeSpan::new(begin, end),
            part: None,
            value,
            locations,
            rest: None,
        }
    }

    /// Where the part of the step begins, as a share of the period.
    pub fn begin(&self) -> Fraction {
        self.part().begin
    }

    /// Where the part of the step ends, as a share of the period.
    pub fn end(&self) -> Fraction {
        self.part().end
    }

    pub fn value(&self) -> &Value {
        &self.value
    }

    pub fn locations(&self) -> &Locations {
        &self.locations
    }

    /// This step, whole and part, from `begin` to `end`.
    pub fn at(&self, begin: Fraction, end: Fraction) -> Self {
        Self {
            whole: TimeSpan::new(begin, end),
            part: None,
            ..self.clone()
        }
    }

    /// More locations, after the step's own.
    pub fn locate(&mut self, locations: &[(usize, usize)]) {
        self.locations.extend_from_slice(locations);
    }

    fn part(&self) -> TimeSpan {
        self.part.as_deref().copied().unwrap_or(self.whole)
    }

    /// The hap of this step at `whole` and `part`.
    fn hap_at(&self, whole: TimeSpan, part: TimeSpan) -> Hap {
        let mut hap = Hap::new(Some(whole), part, self.value.clone());
        hap.context = self.locations.clone();
        if let Some(rest) = &self.rest {
            hap.live_controls = rest.live_controls;
            hap.slider_binding = rest.slider_binding;
            hap.ui_visuals = rest.ui_visuals;
            hap.pick_lookup = rest.pick_lookup.clone();
            hap.keys = rest.keys.clone();
        }
        hap
    }

    /// The hap of the period that begins at time zero, with its whole and
    /// its part as shares of the period.
    fn hap(&self) -> Hap {
        self.hap_at(self.whole, self.part())
    }

    /// The step of `hap`, a hap with a whole.
    fn of(hap: Hap) -> Option<Self> {
        let whole = hap.whole?;
        let plain = hap.live_controls == [0; 4]
            && hap.slider_binding == 0
            && hap.ui_visuals == 0
            && hap.pick_lookup.is_none()
            && hap.keys.is_empty();
        Some(Self {
            whole,
            part: (hap.part != whole).then(|| Box::new(hap.part)),
            value: hap.value,
            locations: hap.context,
            rest: (!plain).then(|| {
                Box::new(Rest {
                    live_controls: hap.live_controls,
                    slider_binding: hap.slider_binding,
                    ui_visuals: hap.ui_visuals,
                    pick_lookup: hap.pick_lookup,
                    keys: hap.keys,
                })
            }),
        })
    }

    /// Whether this step and `other` are the same hap, with values that
    /// `same_value` holds to be the same. Steps that carry more than a
    /// value and its locations are not compared and are not the same.
    pub(crate) fn same(&self, other: &Self, same_value: impl Fn(&Value, &Value) -> bool) -> bool {
        self.rest.is_none()
            && other.rest.is_none()
            && (self.whole, self.part()) == (other.whole, other.part())
            && self.locations == other.locations
            && same_value(&self.value, &other.value)
    }
}

/// Steps that repeat every `period` cycles, as a caller of [`sequence`]
/// gives them.
#[derive(Clone, Debug)]
pub struct SequenceLayer {
    pub period: Fraction,
    pub steps: Vec<SequenceStep>,
}

/// The steps of one layer of a sequence node. Layers that a stack or a
/// change of speed makes of the same steps share them.
#[derive(Clone)]
pub(crate) struct Layer {
    pub(crate) period: Fraction,
    pub(crate) steps: Arc<[SequenceStep]>,
}

impl From<SequenceLayer> for Layer {
    fn from(layer: SequenceLayer) -> Self {
        Self {
            period: layer.period,
            steps: layer.steps.into(),
        }
    }
}

/// Why a [`Layer`] stops before the end of a span.
enum SequenceStop {
    /// A time left the native range.
    Range,
    /// The query stopped, or its budget refused the next hap.
    Query,
}

impl Layer {
    /// Whether the period is more than zero and the steps are in the order
    /// of their haps: the part of each lies inside its whole and inside the
    /// period, begins at or after the part before it and ends at or after
    /// it, and no part is empty.
    fn is_sound(&self) -> bool {
        let inside = |step: &SequenceStep| {
            let (whole, part) = (step.whole, step.part());
            Fraction::ZERO <= whole.begin
                && whole.begin <= part.begin
                && part.begin < part.end
                && part.end <= whole.end
                && whole.end <= Fraction::ONE
        };
        let ordered = self
            .steps
            .windows(2)
            .all(|pair| pair[0].begin() <= pair[1].begin() && pair[0].end() <= pair[1].end());
        self.period > Fraction::ZERO && self.steps.iter().all(inside) && ordered
    }

    /// The haps of one period, as a layer with a period of one cycle
    /// answers them for cycle zero.
    pub(crate) fn period_haps(&self) -> Vec<Hap> {
        self.steps.iter().map(SequenceStep::hap).collect()
    }

    /// Push the haps of this layer in `span`.
    fn query(&self, span: TimeSpan, out: &mut Vec<Hap>) -> Result<(), SequenceStop> {
        let in_range = |time: Option<Fraction>| time.ok_or(SequenceStop::Range);
        // A period of one cycle, the common case, maps no time.
        let cycles = self.period == Fraction::ONE;
        let periods_at = |time: Fraction| {
            if cycles {
                Some(time)
            } else {
                time.checked_div(self.period)
            }
        };
        let cycles_at = |time: Fraction| {
            if cycles {
                Some(time)
            } else {
                time.checked_mul(self.period)
            }
        };
        // The span in periods: each turn of the steps is one cycle of it.
        let asked = TimeSpan::new(
            in_range(periods_at(span.begin))?,
            in_range(periods_at(span.end))?,
        );
        for piece in asked.span_cycles() {
            if query_stop_requested() {
                return Err(SequenceStop::Query);
            }
            let turn = piece.begin.sam();
            let begin = in_range(piece.begin.checked_sub(turn))?;
            let end = in_range(piece.end.checked_sub(turn))?;
            // The time of the period's share `share` in this turn.
            let at = |share: Fraction| in_range(turn.checked_add(share));
            // The steps that meet the piece. A span of no width asks for the
            // step that holds its one time.
            let instant = begin == end;
            let first = self.steps.partition_point(|step| step.end() <= begin);
            let met = self.steps[first..]
                .partition_point(|step| step.begin() <= begin || (!instant && step.begin() < end));
            out.reserve(met);
            for step in &self.steps[first..first + met] {
                let whole = TimeSpan::new(
                    in_range(cycles_at(at(step.whole.begin)?))?,
                    in_range(cycles_at(at(step.whole.end)?))?,
                );
                let part = TimeSpan::new(
                    in_range(cycles_at(at(step.begin())?.max(piece.begin)))?,
                    in_range(cycles_at(at(step.end())?.min(piece.end)))?,
                );
                if !push_budgeted(out, step.hap_at(whole, part)) {
                    return Err(SequenceStop::Query);
                }
            }
        }
        Ok(())
    }

    /// The steps of this layer with a period of a whole count of cycles, or
    /// of one cycle in a whole count of periods: the bounds of the cycles
    /// are then at the same places of each period. `None` for more steps
    /// than a fold makes.
    fn in_whole_cycles(&self) -> Option<(Fraction, std::borrow::Cow<'_, [SequenceStep]>)> {
        let (cycles, turns) = (self.period.numer(), self.period.denom());
        if cycles == 1 || turns == 1 {
            return Some((self.period, std::borrow::Cow::Borrowed(&self.steps)));
        }
        let count = usize::try_from(turns).ok()?;
        if self.steps.len().checked_mul(count)? > MAX_FOLDED_STEPS {
            return None;
        }
        let turns = Fraction::int(turns);
        let mut steps = Vec::with_capacity(self.steps.len() * count);
        for turn in 0..count {
            let turn = Fraction::int(i128::try_from(turn).ok()?);
            let at = |share: Fraction| share.checked_add(turn)?.checked_div(turns);
            let span = |span: TimeSpan| Some(TimeSpan::new(at(span.begin)?, at(span.end)?));
            for step in self.steps.iter() {
                steps.push(SequenceStep {
                    whole: span(step.whole)?,
                    part: match &step.part {
                        Some(part) => Some(Box::new(span(**part)?)),
                        None => None,
                    },
                    ..step.clone()
                });
            }
        }
        Some((Fraction::int(cycles), std::borrow::Cow::Owned(steps)))
    }
}

/// The pieces of `part`, a part of a period of `period` cycles, between the
/// bounds of the cycles. The period is one of [`Layer::in_whole_cycles`].
fn cycle_pieces(part: TimeSpan, period: Fraction) -> Option<Vec<TimeSpan>> {
    let begin = part.begin.checked_mul(period)?;
    let end = part.end.checked_mul(period)?;
    let mut pieces = Vec::with_capacity(1);
    let mut from = part.begin;
    let mut bound = begin.floor().checked_add(Fraction::ONE)?;
    while bound < end {
        let to = bound.checked_div(period)?;
        pieces.push(TimeSpan::new(from, to));
        from = to;
        bound = bound.checked_add(Fraction::ONE)?;
    }
    pieces.push(TimeSpan::new(from, part.end));
    Some(pieces)
}

/// Values at places that are known when the pattern is built. The steps of
/// each layer repeat every period of the layer. Each step is one hap a
/// period, whose whole is the step's window and whose value is the step's.
///
/// This is what a stack of concatenations of plain values answers, each at
/// a constant speed, nested concatenations and stacks of plain values
/// inside them included. It answers in the same order: layer by layer, and
/// each layer in the order of time. It is one node where they are one node
/// for each value and each level, so it holds and visits a fraction of them.
///
/// `None` for a layer that is not sound: a period that is not above zero,
/// or steps that are empty, outside the period or not in the order of
/// time. A layer with no step answers nothing and is not kept.
pub fn sequence(layers: Vec<SequenceLayer>) -> Option<Pattern> {
    node(layers.into_iter().map(Layer::from).collect())
}

/// The sequence node of `layers`, when each is sound.
fn node(layers: Vec<Layer>) -> Option<Pattern> {
    if !layers.iter().all(Layer::is_sound) {
        return None;
    }
    let layers: Arc<[Layer]> = layers
        .into_iter()
        .filter(|layer| !layer.steps.is_empty())
        .collect();
    Some(if layers.is_empty() {
        silence()
    } else {
        Pattern::of(Node::Sequence(layers))
    })
}

/// The haps of `layers` in `state`'s span. See [`sequence`].
pub(crate) fn query_sequence(layers: &[Layer], state: &State) -> Vec<Hap> {
    let mut out = Vec::new();
    for layer in layers {
        match layer.query(state.span, &mut out) {
            Ok(()) => {}
            Err(SequenceStop::Range) => {
                refuse(QueryLimit::NativeFraction {
                    operation: "sequence",
                });
                return Vec::new();
            }
            Err(SequenceStop::Query) => return Vec::new(),
        }
    }
    out
}

/// The one data value of `layers`, when they are one value that fills each
/// cycle, as a `pure` is.
pub(crate) fn constant_value(layers: &[Layer]) -> Option<&Value> {
    let cycle = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
    match layers {
        [layer] if layer.period == Fraction::ONE => match &*layer.steps {
            [step] if step.whole == cycle && step.part.is_none() && step.value.is_data(0) => {
                Some(&step.value)
            }
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
thread_local! {
    /// Set by a test to build each node that a fold stands for.
    static UNFOLDED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Build with each node that a fold stands for: the graph that a fold must
/// answer as, and the graph of a test that reads the kinds of its nodes.
#[cfg(test)]
pub(crate) fn unfolded<R>(build: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            UNFOLDED.set(self.0);
        }
    }
    let _restore = Restore(UNFOLDED.replace(true));
    build()
}

/// Whether a constructor may fold `pattern`: its handle binds no runtime
/// and is not marked volatile.
fn folds(pattern: &Pattern) -> bool {
    #[cfg(test)]
    if UNFOLDED.get() {
        return false;
    }
    pattern.runtime_settings.is_none() && !pattern.is_volatile()
}

/// The layers of `pattern`, when it is a sequence that a constructor folds
/// into.
fn folded(pattern: &Pattern) -> Option<&[Layer]> {
    match pattern.node_kind() {
        Node::Sequence(layers) if folds(pattern) => Some(layers),
        _ => None,
    }
}

/// The hap that `values` answers in each cycle, as of cycle zero, when the
/// graph says it with no query: a `pure` value, at its locations, or a
/// sequence of one value that fills each cycle.
fn constant(values: &Pattern) -> Option<Hap> {
    if !folds(values) {
        return None;
    }
    match values.node_kind() {
        Node::Pure(value) if value.is_data(0) => {
            let cycle = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
            Some(Hap::new(Some(cycle), cycle, value.clone()))
        }
        Node::AddContext(inner, locations) => {
            let mut hap = constant(inner)?;
            hap.context.extend_from_slice(locations);
            Some(hap)
        }
        Node::Sequence(layers) => {
            constant_value(layers)?;
            Some(layers[0].steps[0].hap())
        }
        _ => None,
    }
}

/// The layers that `pattern` answers as, when it is a sequence or a
/// constant: a constant is one step that fills each cycle.
fn layers(pattern: &Pattern) -> Option<std::borrow::Cow<'_, [Layer]>> {
    if let Some(layers) = folded(pattern) {
        return Some(std::borrow::Cow::Borrowed(layers));
    }
    let step = SequenceStep::of(constant(pattern)?)?;
    Some(std::borrow::Cow::Owned(vec![Layer {
        period: Fraction::ONE,
        steps: Arc::new([step]),
    }]))
}

/// Each of `layers` with the steps that `change` gives for each step, as
/// one sequence. Steps with equal control maps share one map. `None` when
/// `change` gives none, or raises a query error: the error belongs to each
/// query of the node that the fold stands for.
fn refolded(
    layers: &[Layer],
    mut change: impl FnMut(&Layer, &mut Vec<SequenceStep>) -> Option<Fraction>,
) -> Option<Pattern> {
    without_query_signals(|| {
        let mut changed = Vec::with_capacity(layers.len());
        for layer in layers {
            let mut steps = Vec::with_capacity(layer.steps.len());
            let period = change(layer, &mut steps)?;
            crate::value::share_equal_maps(steps.iter_mut().map(|step| &mut step.value));
            changed.push(Layer {
                period,
                steps: steps.into(),
            });
        }
        node(changed)
    })
}

/// `pattern` with `locations` after the locations of each hap, as
/// [`Pattern::with_added_context`] gives, when `pattern` is a sequence. A
/// `pure` keeps its own node, which says that it is one.
pub(crate) fn located(pattern: &Pattern, locations: &[(usize, usize)]) -> Option<Pattern> {
    refolded(folded(pattern)?, |layer, steps| {
        steps.extend(layer.steps.iter().map(|step| {
            let mut step = step.clone();
            step.locate(locations);
            step
        }));
        Some(layer.period)
    })
}

/// `pattern` with each hap changed by `map`, a map that reads no span and
/// no state, when `pattern` is a sequence or a constant of data values.
pub(crate) fn mapped(pattern: &Pattern, map: &OwnedHapMapFn) -> Option<Pattern> {
    refolded(&layers(pattern)?, |layer, steps| {
        for step in layer.steps.iter() {
            // A value of the script is read through its host, at each query.
            if !step.value.is_data(0) {
                return None;
            }
            let hap = step.hap();
            let (whole, part) = (hap.whole, hap.part);
            if let Some(hap) = map(hap) {
                // A map that reads no span moves no hap.
                if (hap.whole, hap.part) != (whole, part) {
                    return None;
                }
                steps.push(SequenceStep::of(hap)?);
            }
        }
        Some(layer.period)
    })
}

/// The `appLeft` of `functions` and `values` by `op`, when `functions` is a
/// sequence or a constant and `values` is a constant: each step meets the
/// constant where the step is. The constant is another hap in each cycle,
/// so a step that crosses a bound of a cycle is cut there, as the meeting
/// cuts its hap.
///
/// `None` also for a meeting that [`Meeting::in_place`] does not make, and
/// for cut steps that are not in the order of time.
pub(crate) fn applied(
    functions: &Pattern,
    values: &Pattern,
    op: ComposeOp,
    flow: LookupFlow,
) -> Option<Pattern> {
    let layers = layers(functions)?;
    let constant = constant(values)?;
    let apply = Combine::Composed(op);
    refolded(&layers, |layer, steps| {
        let (period, whole_cycles) = layer.in_whole_cycles()?;
        for step in whole_cycles.iter() {
            for piece in cycle_pieces(step.part(), period)? {
                let mut hap = step.hap();
                let made = Meeting::in_place(&mut hap, &constant, piece, &apply, flow);
                if !made || steps.len() == MAX_FOLDED_STEPS {
                    return None;
                }
                steps.push(SequenceStep::of(hap)?);
            }
        }
        Some(period)
    })
}

/// The stack of `patterns`, when each is a sequence, a constant or silence:
/// the layers of each, one after the other.
pub(crate) fn stacked(patterns: &[Pattern]) -> Option<Pattern> {
    let mut stacked = Vec::new();
    for pattern in patterns {
        if pattern.is_rest() && pattern.runtime_settings.is_none() {
            continue;
        }
        stacked.extend(layers(pattern)?.iter().cloned());
    }
    node(stacked)
}

/// `pattern`, `factor` times as fast, when it is a sequence or a constant
/// and the factor is above zero.
pub(crate) fn faster(pattern: &Pattern, factor: Fraction) -> Option<Pattern> {
    let layers = layers(pattern)?;
    if factor <= Fraction::ZERO {
        return None;
    }
    let small =
        |period: &Fraction| period.numer() <= MAX_PERIOD_TERM && period.denom() <= MAX_PERIOD_TERM;
    let layers = layers
        .iter()
        .map(|layer| {
            Some(Layer {
                period: layer.period.checked_div(factor).filter(small)?,
                steps: Arc::clone(&layer.steps),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    node(layers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::{Alignment, compose};
    use crate::controls::ControlSpec;
    use crate::{fastcat, pure, stack};

    fn frac((n, d): (i128, i128)) -> Fraction {
        Fraction::new(n, d)
    }

    fn located_value(value: f64) -> Pattern {
        pure(Value::F64(value)).with_added_context(vec![(value as usize, value as usize + 1)])
    }

    fn step(begin: (i128, i128), end: (i128, i128), value: f64) -> SequenceStep {
        let mut locations = Locations::new();
        locations.push((value as usize, value as usize + 1));
        SequenceStep::new(frac(begin), frac(end), Value::F64(value), locations)
    }

    fn layer(period: (i128, i128), steps: Vec<SequenceStep>) -> SequenceLayer {
        SequenceLayer {
            period: frac(period),
            steps,
        }
    }

    fn spans() -> [(Fraction, Fraction); 8] {
        [
            (Fraction::ZERO, frac((3, 1))),
            (frac((1, 4)), frac((3, 4))),
            (frac((-5, 4)), frac((1, 8))),
            (frac((2, 3)), frac((23, 3))),
            (Fraction::ZERO, frac((13, 1))),
            // A span of no width asks for what sounds at its one time.
            (frac((1, 2)), frac((1, 2))),
            (frac((7, 3)), frac((7, 3))),
            (frac((3, 1)), frac((3, 1))),
        ]
    }

    fn answers(pattern: &Pattern, begin: Fraction, end: Fraction) -> Vec<String> {
        pattern
            .query_arc(begin, end)
            .iter()
            .map(|hap| {
                format!(
                    "{hap:?} {:?} {} {}",
                    hap.live_controls, hap.slider_binding, hap.ui_visuals
                )
            })
            .collect()
    }

    fn assert_same_answers(name: &str, one_node: &Pattern, nodes: &Pattern) {
        assert_eq!(one_node.steps, nodes.steps, "the steps of {name}");
        for (begin, end) in spans() {
            assert_eq!(
                answers(one_node, begin, end),
                answers(nodes, begin, end),
                "{name} over {begin:?}..{end:?}"
            );
        }
    }

    /// Sequences of numbers: steps, a rest, a chord, a slow layer with a
    /// step of three cycles, a layer of one cycle and a half, and a value
    /// that fills each cycle.
    fn sequences() -> Vec<(&'static str, Vec<SequenceLayer>)> {
        vec![
            (
                "steps",
                vec![layer(
                    (1, 1),
                    vec![
                        step((0, 1), (1, 4), 1.0),
                        step((1, 4), (1, 2), 2.0),
                        step((3, 4), (1, 1), 3.0),
                    ],
                )],
            ),
            (
                "a chord and fast steps",
                vec![
                    layer(
                        (1, 1),
                        vec![step((0, 1), (1, 2), 4.0), step((0, 1), (1, 2), 5.0)],
                    ),
                    layer((1, 3), vec![step((0, 1), (1, 1), 6.0)]),
                ],
            ),
            (
                "steps of several cycles",
                vec![layer(
                    (4, 1),
                    vec![step((0, 1), (3, 4), 7.0), step((3, 4), (1, 1), 8.0)],
                )],
            ),
            (
                "a period of a cycle and a half",
                vec![layer(
                    (3, 2),
                    vec![step((0, 1), (1, 2), 9.0), step((1, 2), (1, 1), 10.0)],
                )],
            ),
            (
                "one value a cycle",
                vec![layer((1, 1), vec![step((0, 1), (1, 1), 11.0)])],
            ),
        ]
    }

    fn built(layers: &[SequenceLayer]) -> Pattern {
        sequence(layers.to_vec()).expect("sound layers")
    }

    /// A lane whose second application raises a query error at each query.
    #[test]
    fn a_fold_leaves_an_application_that_fails_to_its_node() {
        let steps = || built(&sequences()[0].1);
        let failing = |pattern: Pattern| {
            let text = pure(Value::Str("no number".into()));
            compose(&pattern, &text, ComposeOp::Add, Alignment::In)
        };
        let folded = failing(steps());
        assert!(!matches!(folded.node_kind(), Node::Sequence(_)));
        assert!(!crate::query_error_pending());
        for (begin, end) in spans() {
            assert_eq!(
                format!("{:?}", folded.try_query_arc_sorted(begin, end)),
                format!(
                    "{:?}",
                    unfolded(|| failing(steps())).try_query_arc_sorted(begin, end)
                ),
            );
        }
    }

    #[test]
    fn a_sequence_answers_as_the_concatenation_of_its_values() {
        let one_node = sequence(vec![layer(
            (1, 1),
            vec![
                step((0, 1), (1, 2), 1.0),
                step((1, 2), (1, 1), 2.0),
                step((1, 2), (1, 1), 3.0),
            ],
        )])
        .expect("ordered steps inside the cycle");
        let nodes = fastcat(vec![
            located_value(1.0),
            unfolded(|| stack(vec![located_value(2.0), located_value(3.0)])),
        ]);
        for (begin, end) in spans() {
            assert_eq!(answers(&one_node, begin, end), answers(&nodes, begin, end));
        }
    }

    #[test]
    fn layers_answer_as_a_stack_of_concatenations_at_their_speeds() {
        let one_node = sequence(vec![
            layer(
                (1, 2),
                vec![step((0, 1), (1, 2), 1.0), step((1, 2), (1, 1), 2.0)],
            ),
            // A layer with a rest in it, slower than the cycle.
            layer((3, 1), vec![step((1, 2), (1, 1), 3.0)]),
            layer(
                (3, 2),
                vec![
                    step((0, 1), (1, 3), 4.0),
                    step((1, 3), (2, 3), 5.0),
                    step((2, 3), (1, 1), 6.0),
                ],
            ),
        ])
        .expect("ordered steps inside each period");
        let nodes = unfolded(|| {
            stack(vec![
                fastcat(vec![located_value(1.0), located_value(2.0)]).fast(frac((2, 1))),
                fastcat(vec![silence(), located_value(3.0)]).slow(frac((3, 1))),
                fastcat(vec![
                    located_value(4.0),
                    located_value(5.0),
                    located_value(6.0),
                ])
                .slow(frac((3, 2))),
            ])
        });
        for (begin, end) in spans() {
            assert_eq!(answers(&one_node, begin, end), answers(&nodes, begin, end));
        }
    }

    #[test]
    fn steps_out_of_order_or_outside_the_period_make_no_sequence() {
        for steps in [
            vec![step((1, 2), (1, 1), 1.0), step((0, 1), (1, 2), 2.0)],
            vec![step((0, 1), (3, 2), 1.0)],
            vec![step((1, 2), (1, 2), 1.0)],
            vec![step((-1, 2), (1, 2), 1.0)],
        ] {
            assert!(sequence(vec![layer((1, 1), steps)]).is_none());
        }
        let whole = || vec![step((0, 1), (1, 1), 1.0)];
        assert!(sequence(vec![layer((0, 1), whole())]).is_none());
        assert!(sequence(vec![layer((-1, 2), whole())]).is_none());
        assert!(sequence(Vec::new()).is_some());
    }

    #[test]
    fn a_control_of_a_sequence_answers_as_the_map_of_each_hap() {
        for (name, layers) in sequences() {
            let control = |pattern: Pattern| pattern.map_control(ControlSpec::new(["n"]), false);
            let folded = control(built(&layers));
            assert!(matches!(folded.node_kind(), Node::Sequence(_)), "{name}");
            assert_same_answers(name, &folded, &unfolded(|| control(built(&layers))));
        }
    }

    #[test]
    fn locations_of_a_sequence_answer_as_their_own_node() {
        for (name, layers) in sequences() {
            let located = |pattern: Pattern| pattern.with_added_context(vec![(70, 72), (80, 81)]);
            let folded = located(built(&layers));
            assert!(matches!(folded.node_kind(), Node::Sequence(_)), "{name}");
            assert_same_answers(name, &folded, &unfolded(|| located(built(&layers))));
        }
    }

    #[test]
    fn a_constant_applied_to_a_sequence_answers_as_the_application() {
        let controls = |key: &str, value: f64| {
            pure(Value::F64(value))
                .with_added_context(vec![(90, 93)])
                .map_control(ControlSpec::new([key]), false)
        };
        for (name, layers) in sequences() {
            // Numbers under arithmetic, and control objects that a
            // constant sets, adds to and sets again.
            let chains: [(&str, &dyn Fn(Pattern) -> Pattern); 2] = [
                ("arithmetic", &|pattern: Pattern| {
                    let scaled = compose(
                        &pattern,
                        &pure(Value::F64(3.0)),
                        ComposeOp::Mul,
                        Alignment::In,
                    );
                    compose(&scaled, &located_value(0.5), ComposeOp::Add, Alignment::In)
                }),
                ("controls", &|pattern: Pattern| {
                    let notes = pattern.map_control(ControlSpec::new(["n"]), false);
                    let set = compose(
                        &notes,
                        &controls("gain", 0.8),
                        ComposeOp::Set,
                        Alignment::In,
                    );
                    let added = compose(&set, &controls("n", 12.0), ComposeOp::Add, Alignment::In);
                    compose(
                        &added,
                        &controls("gain", 0.25),
                        ComposeOp::Set,
                        Alignment::In,
                    )
                }),
            ];
            for (chain_name, chain) in chains {
                let folded = chain(built(&layers));
                assert!(
                    matches!(folded.node_kind(), Node::Sequence(_)),
                    "{chain_name} of {name}"
                );
                assert_same_answers(
                    &format!("{chain_name} of {name}"),
                    &folded,
                    &unfolded(|| chain(built(&layers))),
                );
            }
        }
    }

    #[test]
    fn a_stack_and_a_speed_of_sequences_answer_as_their_own_nodes() {
        let all = || sequences().into_iter().map(|(_, layers)| built(&layers));
        let stacked = || stack(all().collect());
        let folded = stacked();
        assert!(matches!(folded.node_kind(), Node::Sequence(_)));
        assert_same_answers("a stack", &folded, &unfolded(stacked));
        for (name, layers) in sequences() {
            for factor in [(2, 1), (1, 3), (3, 2), (5, 7)] {
                let sped = |pattern: Pattern| pattern.fast(frac(factor)).slow(frac((2, 1)));
                let folded = sped(built(&layers));
                assert!(matches!(folded.node_kind(), Node::Sequence(_)), "{name}");
                assert_same_answers(
                    &format!("{name} at {factor:?}"),
                    &folded,
                    &unfolded(|| sped(built(&layers))),
                );
            }
        }
    }

    #[test]
    fn a_fold_leaves_what_it_does_not_know_to_its_node() {
        let steps = || built(&sequences()[0].1);
        // A value that is no constant, a factor that is not above zero, a
        // handle that is marked volatile, and a stack with a signal in it.
        let patterned = fastcat(vec![pure(Value::F64(1.0)), pure(Value::F64(2.0))]);
        let unknown = [
            compose(&steps(), &patterned, ComposeOp::Add, Alignment::In),
            steps().fast(frac((-2, 1))),
            compose(
                &steps().mark_volatile(),
                &pure(Value::F64(1.0)),
                ComposeOp::Add,
                Alignment::In,
            ),
            stack(vec![steps(), crate::signal::saw()]),
        ];
        for pattern in unknown {
            assert!(!matches!(pattern.node_kind(), Node::Sequence(_)));
        }
    }
}
