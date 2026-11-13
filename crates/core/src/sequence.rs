//! Values at places that are known when a pattern is built.
//!
//! A [`sequence`] holds the haps of one period of each of its layers, so a
//! query copies them and walks no graph. A constructor whose answer is
//! known from such haps alone folds it into them: a control of each value,
//! a constant that each hap meets, a stack, a change of speed. The chain
//! `s("bd*2 hh").bank("tr909").gain(0.8)` is then one node of three haps,
//! where it was a node for each call, each argument and each location.
//!
//! A layer holds the places of its steps apart from their values, and each
//! value once. A fold that changes values makes each value once and keeps
//! the places, which the layers of a chain of folds share.
//!
//! Each fold reads the graph and runs no query, so it answers under every
//! settings state as the nodes it stands for do. A fold that would raise a
//! query error leaves its node in place, which raises the error at each
//! query.

use crate::compose::ComposeOp;
use crate::{
    Combine, Hap, Locations, LookupFlow, Meeting, Node, OwnedHapMapFn, Pattern, QueryLimit, State,
    TimeSpan, Value, push_budgeted, query_stop_requested, refuse, silence, without_query_signals,
};
use rustel_fraction::Fraction;
use std::sync::{Arc, OnceLock};

/// The most steps one layer holds after a fold. A fold that makes more
/// keeps its own node.
const MAX_FOLDED_STEPS: usize = 256;

/// The largest numerator or denominator of a period after a fold. A speed
/// past it keeps its own node, which names itself when a time leaves the
/// native range.
const MAX_PERIOD_TERM: i128 = 1 << 20;

/// One step of a [`sequence`], as a caller gives it: a value over a window
/// of its layer's period, with the source locations its hap carries.
#[derive(Clone, Debug)]
pub struct SequenceStep {
    /// The whole of the step's hap, as shares of the period.
    whole: TimeSpan,
    /// The part of the hap, inside the whole.
    part: TimeSpan,
    value: Value,
    locations: Locations,
}

impl SequenceStep {
    /// A step that holds `value` from `begin` to `end`, as shares of the
    /// period, at `locations`.
    pub fn new(begin: Fraction, end: Fraction, value: Value, locations: Locations) -> Self {
        let window = TimeSpan::new(begin, end);
        Self {
            whole: window,
            part: window,
            value,
            locations,
        }
    }

    /// Where the part of the step begins, as a share of the period.
    pub fn begin(&self) -> Fraction {
        self.part.begin
    }

    /// Where the part of the step ends, as a share of the period.
    pub fn end(&self) -> Fraction {
        self.part.end
    }

    /// Where the whole of the step begins and ends, as shares of the period.
    pub fn whole(&self) -> (Fraction, Fraction) {
        (self.whole.begin, self.whole.end)
    }

    pub fn value(&self) -> &Value {
        &self.value
    }

    pub fn locations(&self) -> &Locations {
        &self.locations
    }

    /// This step with `whole` as its whole and `part`, a span inside it, as
    /// its part: a step that the bound of a cycle cut holds a part of its
    /// whole.
    pub fn placed(&self, whole: (Fraction, Fraction), part: (Fraction, Fraction)) -> Self {
        Self {
            whole: TimeSpan::new(whole.0, whole.1),
            part: TimeSpan::new(part.0, part.1),
            ..self.clone()
        }
    }

    /// More locations, after the step's own.
    pub fn locate(&mut self, locations: &[(usize, usize)]) {
        self.locations.extend_from_slice(locations);
    }
}

/// Steps that repeat every `period` cycles, as a caller of [`sequence`]
/// gives them.
#[derive(Clone, Debug)]
pub struct SequenceLayer {
    pub period: Fraction,
    pub steps: Vec<SequenceStep>,
}

/// Where one step of a layer is, and which value it holds.
#[derive(Clone)]
struct Place {
    /// The whole of the hap, as shares of the period.
    whole: TimeSpan,
    /// The part of a hap that a meeting cut, inside the whole. `None` for a
    /// part that is the whole.
    part: Option<Box<TimeSpan>>,
    /// The place of the step's value among the layer's values.
    value: u32,
    /// The locations of the step's own value.
    locations: Locations,
}

impl Place {
    fn part(&self) -> TimeSpan {
        self.part.as_deref().copied().unwrap_or(self.whole)
    }
}

/// One layer of a sequence node: the places of its steps, and each value
/// of them once. Each hap holds a plain value and its locations: a fold
/// that would give a hap more keeps its own node.
#[derive(Clone)]
pub(crate) struct Layer {
    pub(crate) period: Fraction,
    /// Shared by each layer that a fold makes of the same places.
    places: Arc<[Place]>,
    values: Arc<[Value]>,
    /// The locations of each hap before its step's own: those of the
    /// constants that the steps met, the last first.
    before: Locations,
    /// The locations of each hap after its step's own.
    after: Locations,
    /// Whether the part of each step lies in one cycle, in every period: a
    /// constant, which is another hap in each cycle, then cuts no step.
    in_cycles: bool,
    /// The locations of each step's hap, as one list, made at the first
    /// query of a layer that has locations of its own.
    contexts: OnceLock<Box<[Locations]>>,
}

impl From<SequenceLayer> for Layer {
    /// Equal data values share one place among the values.
    fn from(layer: SequenceLayer) -> Self {
        let mut values: Vec<Value> = Vec::new();
        let mut known: std::collections::HashMap<u64, Vec<u32>> = std::collections::HashMap::new();
        let places: Vec<Place> = layer
            .steps
            .into_iter()
            .map(|step| {
                let same = step.value.is_data(0).then(|| {
                    let same = known.entry(step.value.data_hash()).or_default();
                    let found = same
                        .iter()
                        .copied()
                        .find(|index| values[*index as usize].same_data(&step.value));
                    (same, found)
                });
                let value = match same {
                    Some((_, Some(found))) => found,
                    other => {
                        let index = u32::try_from(values.len()).unwrap_or(u32::MAX);
                        if let Some((same, _)) = other {
                            same.push(index);
                        }
                        values.push(step.value);
                        index
                    }
                };
                Place {
                    whole: step.whole,
                    part: (step.part != step.whole).then(|| Box::new(step.part)),
                    value,
                    locations: step.locations,
                }
            })
            .collect();
        Self::new(layer.period, places.into(), values.into())
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
    fn new(period: Fraction, places: Arc<[Place]>, values: Arc<[Value]>) -> Self {
        Self {
            in_cycles: lie_in_cycles(period, &places),
            period,
            places,
            values,
            before: Locations::new(),
            after: Locations::new(),
            contexts: OnceLock::new(),
        }
    }

    /// This layer with other places, values or locations: the lists of
    /// locations it made are not those of the new layer.
    fn with(&self, change: impl FnOnce(&mut Self)) -> Self {
        let mut layer = Self {
            contexts: OnceLock::new(),
            ..self.clone()
        };
        change(&mut layer);
        layer
    }

    /// Whether the period is more than zero, each step holds one of the
    /// values, and the steps are in the order of their haps: the part of
    /// each lies inside its whole and inside the period, begins at or after
    /// the part before it and ends at or after it, and no part is empty.
    fn is_sound(&self) -> bool {
        let inside = |place: &Place| {
            let (whole, part) = (place.whole, place.part());
            (place.value as usize) < self.values.len()
                && Fraction::ZERO <= whole.begin
                && whole.begin <= part.begin
                && part.begin < part.end
                && part.end <= whole.end
                && whole.end <= Fraction::ONE
        };
        let ordered = self.places.windows(2).all(|pair| {
            let (a, b) = (pair[0].part(), pair[1].part());
            a.begin <= b.begin && a.end <= b.end
        });
        self.period > Fraction::ZERO && self.places.iter().all(inside) && ordered
    }

    /// The locations of the hap of step `index`: those before, the step's
    /// own, then those after.
    fn context(&self, index: usize) -> Locations {
        if self.before.is_empty() && self.after.is_empty() {
            return self.places[index].locations.clone();
        }
        let contexts = self.contexts.get_or_init(|| {
            self.places
                .iter()
                .map(|place| {
                    let mut context = self.before.clone();
                    context.append(&place.locations);
                    context.append(&self.after);
                    context
                })
                .collect()
        });
        contexts[index].clone()
    }

    /// The hap of step `index` at `whole` and `part`.
    fn hap_at(&self, index: usize, whole: TimeSpan, part: TimeSpan) -> Hap {
        let place = &self.places[index];
        let mut hap = Hap::new(Some(whole), part, self.values[place.value as usize].clone());
        hap.context = self.context(index);
        hap
    }

    /// The haps of one period, as a layer with a period of one cycle
    /// answers them for cycle zero.
    pub(crate) fn period_haps(&self) -> Vec<Hap> {
        self.places
            .iter()
            .enumerate()
            .map(|(index, place)| self.hap_at(index, place.whole, place.part()))
            .collect()
    }

    /// The values of the steps, each once.
    pub(crate) fn values(&self) -> &[Value] {
        &self.values
    }

    /// Whether this layer and `other` answer the same haps, with values
    /// that `same_value` holds to be the same.
    pub(crate) fn same(&self, other: &Self, same_value: impl Fn(&Value, &Value) -> bool) -> bool {
        let value = |layer: &Self, place: &Place| layer.values.get(place.value as usize).cloned();
        self.period == other.period
            && self.before == other.before
            && self.after == other.after
            && self.places.len() == other.places.len()
            && self.places.iter().zip(other.places.iter()).all(|(a, b)| {
                (a.whole, a.part()) == (b.whole, b.part())
                    && a.locations == b.locations
                    && match (value(self, a), value(other, b)) {
                        (Some(a), Some(b)) => same_value(&a, &b),
                        _ => false,
                    }
            })
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
            let first = self
                .places
                .partition_point(|place| place.part().end <= begin);
            let met = self.places[first..].partition_point(|place| {
                let from = place.part().begin;
                from <= begin || (!instant && from < end)
            });
            out.reserve(met);
            for (index, place) in self.places.iter().enumerate().skip(first).take(met) {
                let whole = TimeSpan::new(
                    in_range(cycles_at(at(place.whole.begin)?))?,
                    in_range(cycles_at(at(place.whole.end)?))?,
                );
                // A step that is not cut and lies in the piece is its whole.
                let part = match &place.part {
                    None if place.whole.begin >= begin && place.whole.end <= end => whole,
                    _ => TimeSpan::new(
                        in_range(cycles_at(at(place.part().begin)?.max(piece.begin)))?,
                        in_range(cycles_at(at(place.part().end)?.min(piece.end)))?,
                    ),
                };
                if !push_budgeted(out, self.hap_at(index, whole, part)) {
                    return Err(SequenceStop::Query);
                }
            }
        }
        Ok(())
    }

    /// The places of this layer cut at the bounds of the cycles, with their
    /// period: each part then lies in one cycle. `None` for more steps than
    /// a fold makes.
    fn cut_at_cycles(&self) -> Option<(Fraction, Vec<Place>)> {
        // A period of a whole count of cycles has the bounds of the cycles
        // at the same places of each period. Any other period has them
        // there in a count of its turns.
        let (cycles, turns) = (self.period.numer(), self.period.denom());
        let count = usize::try_from(turns).ok()?;
        if self.places.len().checked_mul(count)? > MAX_FOLDED_STEPS {
            return None;
        }
        let period = Fraction::int(cycles);
        let turns = Fraction::int(turns);
        let mut places = Vec::with_capacity(self.places.len() * count);
        for turn in 0..count {
            let turn = Fraction::int(i128::try_from(turn).ok()?);
            let at = |share: Fraction| share.checked_add(turn)?.checked_div(turns);
            for place in self.places.iter() {
                let whole = TimeSpan::new(at(place.whole.begin)?, at(place.whole.end)?);
                let part = place.part();
                let part = TimeSpan::new(at(part.begin)?, at(part.end)?);
                // The bounds of the cycles inside the part, in order.
                let end = part.end.checked_mul(period)?;
                let mut from = part.begin;
                let mut bound = from
                    .checked_mul(period)?
                    .floor()
                    .checked_add(Fraction::ONE)?;
                let mut push = |piece: TimeSpan| {
                    places.push(Place {
                        whole,
                        part: (piece != whole).then(|| Box::new(piece)),
                        value: place.value,
                        locations: place.locations.clone(),
                    });
                    places.len() <= MAX_FOLDED_STEPS
                };
                while bound < end {
                    let to = bound.checked_div(period)?;
                    if !push(TimeSpan::new(from, to)) {
                        return None;
                    }
                    from = to;
                    bound = bound.checked_add(Fraction::ONE)?;
                }
                if !push(TimeSpan::new(from, part.end)) {
                    return None;
                }
            }
        }
        Some((period, places))
    }
}

/// Whether the part of each of `places` lies in one cycle, in every turn of
/// a period of `period` cycles. Read from the period where that is enough:
/// `false` for a period that only a cut settles.
fn lie_in_cycles(period: Fraction, places: &[Place]) -> bool {
    // One cycle holds a whole count of such periods.
    if period.numer() == 1 {
        return true;
    }
    // A period of a whole count of cycles has their bounds at the same
    // places of each turn.
    period.denom() == 1
        && places.iter().all(|place| {
            let part = place.part();
            match (part.begin.checked_mul(period), part.end.checked_mul(period)) {
                (Some(begin), Some(end)) => end <= begin.floor() + Fraction::ONE,
                _ => false,
            }
        })
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
    layers
        .iter()
        .all(Layer::is_sound)
        .then(|| node_of_sound(layers))
}

/// The sequence node of `layers`, which are sound: a fold of sound layers
/// that moves no step.
fn node_of_sound(layers: Vec<Layer>) -> Pattern {
    let layers: Arc<[Layer]> = layers
        .into_iter()
        .filter(|layer| !layer.places.is_empty())
        .collect();
    if layers.is_empty() {
        silence()
    } else {
        Pattern::of(Node::Sequence(layers))
    }
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
        [layer] if layer.period == Fraction::ONE => match &*layer.places {
            [place] if place.whole == cycle && place.part.is_none() => layer
                .values
                .get(place.value as usize)
                .filter(|value| value.is_data(0)),
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

/// A hap of a plain value that fills cycle zero, with no location.
fn cycle_hap(value: Value) -> Hap {
    let cycle = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
    Hap::new(Some(cycle), cycle, value)
}

/// Whether `hap` holds a value and its locations and nothing more: what a
/// step of a sequence holds.
fn is_plain(hap: &Hap) -> bool {
    hap.live_controls == [0; 4]
        && hap.slider_binding == 0
        && hap.ui_visuals == 0
        && hap.pick_lookup.is_none()
        && hap.keys.is_empty()
}

/// The hap that `values` answers in each cycle, as of cycle zero, when the
/// graph says it with no query: a `pure` value, at its locations, or a
/// sequence of one value that fills each cycle.
fn constant(values: &Pattern) -> Option<Hap> {
    if !folds(values) {
        return None;
    }
    match values.node_kind() {
        Node::Pure(value) if value.is_data(0) => Some(cycle_hap(value.clone())),
        Node::AddContext(inner, locations) => {
            let mut hap = constant(inner)?;
            hap.context.extend_from_slice(locations);
            Some(hap)
        }
        Node::Sequence(layers) => {
            let mut hap = cycle_hap(constant_value(layers)?.clone());
            hap.context = layers[0].context(0);
            Some(hap)
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
    let hap = constant(pattern)?;
    let place = Place {
        whole: hap.whole?,
        part: None,
        value: 0,
        locations: hap.context,
    };
    Some(std::borrow::Cow::Owned(vec![Layer::new(
        Fraction::ONE,
        Arc::new([place]),
        Arc::new([hap.value]),
    )]))
}

/// `layers` with each value that `change` makes of a value of them, as one
/// sequence. `None` when `change` gives none, or raises a query error: the
/// error belongs to each query of the node that the fold stands for.
fn revalued(
    layers: Vec<Layer>,
    mut change: impl FnMut(&Value) -> Option<Value>,
) -> Option<Pattern> {
    without_query_signals(|| {
        let layers = layers
            .into_iter()
            .map(|mut layer| {
                let values: Option<Vec<Value>> = layer.values.iter().map(&mut change).collect();
                layer.values = values?.into();
                Some(layer)
            })
            .collect::<Option<Vec<_>>>()?;
        Some(node_of_sound(layers))
    })
}

/// `pattern` with `locations` after the locations of each hap, as
/// [`Pattern::with_added_context`] gives, when `pattern` is a sequence. A
/// `pure` keeps its own node, which says that it is one.
pub(crate) fn located(pattern: &Pattern, locations: &[(usize, usize)]) -> Option<Pattern> {
    let layers = folded(pattern)?
        .iter()
        .map(|layer| layer.with(|layer| layer.after.extend_from_slice(locations)))
        .collect();
    Some(node_of_sound(layers))
}

/// `pattern` with each hap changed by `map`, a map that reads no span and
/// no state, when `pattern` is a sequence or a constant of data values and
/// the map gives a plain value for each.
pub(crate) fn mapped(pattern: &Pattern, map: &OwnedHapMapFn) -> Option<Pattern> {
    let layers = layers(pattern)?.into_owned();
    revalued(layers, |value| {
        // A value of the script is read through its host, at each query.
        if !value.is_data(0) {
            return None;
        }
        let hap = map(cycle_hap(value.clone()))?;
        // A map that reads no span moves no hap.
        let cycle = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
        ((hap.whole, hap.part) == (Some(cycle), cycle) && is_plain(&hap) && hap.context.is_empty())
            .then_some(hap.value)
    })
}

/// The `appLeft` of `functions` and `values` by `op`, when `functions` is a
/// sequence or a constant and `values` is a constant of a plain value: each
/// step meets the constant where the step is. The constant is another hap
/// in each cycle, so a step that crosses a bound of a cycle is cut there,
/// as the meeting cuts its hap.
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
    if !is_plain(&constant) {
        return None;
    }
    let cut = layers
        .iter()
        .map(|layer| {
            Some(layer.with(|layer| {
                // The value side's locations come first.
                let mut before = constant.context.clone();
                before.append(&layer.before);
                layer.before = before;
            }))
            .and_then(|layer| {
                if layer.in_cycles {
                    return Some(layer);
                }
                let (period, places) = layer.cut_at_cycles()?;
                let cut = Layer {
                    period,
                    places: places.into(),
                    in_cycles: true,
                    ..layer
                };
                cut.is_sound().then_some(cut)
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let apply = Combine::Composed(op);
    // The meeting of a value with the constant's, with no location: the
    // locations of the constant go before those of each step.
    let meets = cycle_hap(constant.value.clone());
    revalued(cut, |value| {
        let mut hap = cycle_hap(value.clone());
        let part = hap.part;
        (Meeting::in_place(&mut hap, &meets, part, &apply, flow) && is_plain(&hap))
            .then_some(hap.value)
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
    Some(node_of_sound(stacked))
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
            let period = layer.period.checked_div(factor).filter(small)?;
            Some(Layer {
                period,
                in_cycles: lie_in_cycles(period, &layer.places),
                ..layer.clone()
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(node_of_sound(layers))
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
