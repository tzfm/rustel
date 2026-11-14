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

use crate::compose::{ComposeOp, compose_data_in_place, is_op_closure};
use crate::context_keys::ContextKeys;
use crate::{
    Combine, Hap, HapMapFn, Locations, LookupFlow, MAX_QUERY_SPAN_CYCLES, Meeting, Node,
    OwnedHapMapFn, Pattern, QueryLimit, State, TimeSpan, Value, ValuePredicate, applied_hap,
    charge_haps, note_accumulated, query_stop_requested, refuse, silence, without_query_signals,
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
    /// The whole of the hap, in cycles from the start of the period.
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
    /// The value of each step, each once, before the controls that `set`
    /// holds.
    values: Arc<[Value]>,
    /// A control map that each value takes when it is read: the union of
    /// the constants that a chain of controls set, the last one last. See
    /// [`Self::set`].
    set: Option<Value>,
    /// The values with `set` in them, made at the first read.
    read: OnceLock<Arc<[Value]>>,
    /// Whether each value is data only: a hap of the layer then holds
    /// nothing that a query resolves or binds to a runtime.
    data: bool,
    /// Whether each value is a map of controls.
    maps: bool,
    /// The keys of the context of each hap, as a map of the haps set them:
    /// the scale that `scale` names, for one.
    keys: ContextKeys,
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

impl Layer {
    /// The layer of `layer`, with the times of its steps in cycles. Equal
    /// data values share one place among the values. `None` for a time
    /// outside the native range.
    fn of(layer: SequenceLayer) -> Option<Self> {
        let period = layer.period;
        let cycles = |span: TimeSpan| {
            Some(TimeSpan::new(
                span.begin.checked_mul(period)?,
                span.end.checked_mul(period)?,
            ))
        };
        let mut values: Vec<Value> = Vec::new();
        let mut known: std::collections::HashMap<u64, Vec<u32>> = std::collections::HashMap::new();
        let mut places = Vec::with_capacity(layer.steps.len());
        for step in layer.steps {
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
                    let index = u32::try_from(values.len()).ok()?;
                    if let Some((same, _)) = other {
                        same.push(index);
                    }
                    values.push(step.value);
                    index
                }
            };
            places.push(Place {
                whole: cycles(step.whole)?,
                part: match step.part != step.whole {
                    true => Some(Box::new(cycles(step.part)?)),
                    false => None,
                },
                value,
                locations: step.locations,
            });
        }
        Some(Self::new(period, places.into(), values.into()))
    }
}

/// The side of an application that a layer is. The locations of the value
/// side come first on a hap, and the keys of the function side.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Functions,
    Values,
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
            data: values.iter().all(|value| value.is_data(0)),
            maps: values.iter().all(|value| matches!(value, Value::Object(_))),
            values,
            set: None,
            read: OnceLock::new(),
            keys: ContextKeys::default(),
            before: Locations::new(),
            after: Locations::new(),
            contexts: OnceLock::new(),
        }
    }

    /// This layer with `values` in the place of its own, one for one.
    fn valued(mut self, values: Arc<[Value]>) -> Self {
        self.data = values.iter().all(|value| value.is_data(0));
        self.maps = values.iter().all(|value| matches!(value, Value::Object(_)));
        self.values = values;
        self.set = None;
        self.read = OnceLock::new();
        self
    }

    /// This layer where each value takes the keys of the one value of
    /// `values`, a map of controls that fills each cycle, as a `set` of it
    /// gives them. No value is made now: the map joins those that the
    /// layer holds, so a chain of such controls makes each value one time,
    /// at the first read.
    ///
    /// `None` for any other meeting: a step that the constant cuts, values
    /// that are not all maps of controls, another op, and a meeting that
    /// [`Meeting::in_place`] does not make.
    fn set(&self, values: &Layer, apply: &Combine, flow: LookupFlow) -> Option<Self> {
        let Combine::Composed(ComposeOp::Set) = apply else {
            return None;
        };
        if !(self.in_cycles && self.maps && values.fills_each_cycle() && values.keys.is_empty()) {
            return None;
        }
        let control = values.values().first()?;
        // Each value is a map of data, so each meets the control as the
        // first one does.
        let (first, meets) = (
            cycle_hap(self.values.first()?.clone()),
            cycle_hap(control.clone()),
        );
        if !(matches!(control, Value::Object(_))
            && Meeting::meets_in_place(&first, &meets, apply, flow))
        {
            return None;
        }
        let mut layer = self.with(|layer| {
            // The value side's locations come first.
            let mut before = values.context(0);
            before.append(&layer.before);
            layer.before = before;
        });
        layer.set = Some(match layer.set.take() {
            Some(mut set) => {
                compose_data_in_place(ComposeOp::Set, &mut set, control);
                set
            }
            None => control.clone(),
        });
        layer.read = OnceLock::new();
        Some(layer)
    }

    /// Whether each value is data only.
    pub(crate) fn is_data(&self) -> bool {
        self.data
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
    /// values, and the steps are in the order of their haps. See
    /// [`Self::is_in_order`].
    fn is_sound(&self) -> bool {
        self.is_in_order()
            && self
                .places
                .iter()
                .all(|place| (place.value as usize) < self.values.len())
    }

    /// Whether the period is more than zero and the steps are in the order
    /// of their haps: the part of each lies inside its whole and inside the
    /// period, begins at or after the part before it and ends at or after
    /// it, and no part is empty.
    fn is_in_order(&self) -> bool {
        let inside = |place: &Place| {
            let (whole, part) = (place.whole, place.part());
            Fraction::ZERO <= whole.begin
                && whole.begin <= part.begin
                && part.begin < part.end
                && part.end <= whole.end
                && whole.end <= self.period
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
        let mut hap = Hap::new(
            Some(whole),
            part,
            self.values()[place.value as usize].clone(),
        );
        hap.context = self.context(index);
        hap.keys = self.keys.clone();
        hap
    }

    /// The haps of one period, as a layer with a period of one cycle
    /// answers them for cycle zero: each time as a share of the period.
    /// `None` for a time outside the native range.
    pub(crate) fn period_haps(&self) -> Option<Vec<Hap>> {
        let share = |span: TimeSpan| {
            Some(TimeSpan::new(
                span.begin.checked_div(self.period)?,
                span.end.checked_div(self.period)?,
            ))
        };
        self.places
            .iter()
            .enumerate()
            .map(|(index, place)| {
                Some(self.hap_at(index, share(place.whole)?, share(place.part())?))
            })
            .collect()
    }

    /// The values of the steps, each once, with the controls that a chain
    /// of constants set.
    pub(crate) fn values(&self) -> &[Value] {
        let Some(set) = &self.set else {
            return &self.values;
        };
        self.read.get_or_init(|| {
            self.values
                .iter()
                .map(|value| {
                    let mut value = value.clone();
                    compose_data_in_place(ComposeOp::Set, &mut value, set);
                    value
                })
                .collect()
        })
    }

    /// Whether this layer and `other` answer the same haps, with values
    /// that `same_value` holds to be the same. The values are read as the
    /// layers hold them: the same values with the same controls set are the
    /// same, and no value is made to know it.
    pub(crate) fn same(&self, other: &Self, same_value: impl Fn(&Value, &Value) -> bool) -> bool {
        let value = |layer: &Self, place: &Place| layer.values.get(place.value as usize).cloned();
        let same_set = match (&self.set, &other.set) {
            (Some(a), Some(b)) => same_value(a, b),
            (None, None) => true,
            _ => false,
        };
        same_set
            && self.keys.same(&other.keys)
            && self.period == other.period
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

    /// Push the haps of this layer in `turn` of its period, for a query of
    /// `span`.
    fn push(&self, turn: &Turn, span: TimeSpan, out: &mut Vec<Hap>) -> Result<(), SequenceStop> {
        let instant = span.begin == span.end;
        // The steps that the span meets.
        let first = self
            .places
            .partition_point(|place| place.part().end <= turn.begin);
        let met = self.places[first..].partition_point(|place| {
            let from = place.part().begin;
            from <= turn.begin || (!instant && from < turn.end)
        });
        if met == 0 {
            return Ok(());
        }
        if !charge_haps(out.len() + met) {
            return Err(SequenceStop::Query);
        }
        out.reserve(met);
        for (index, place) in self.places.iter().enumerate().skip(first).take(met) {
            let whole = TimeSpan::new(turn.moved(place.whole.begin)?, turn.moved(place.whole.end)?);
            let within = place.part();
            let part = match &place.part {
                // A step that is not cut and lies in the span is its
                // whole.
                None if within.begin >= turn.begin && within.end <= turn.end => whole,
                _ => TimeSpan::new(
                    turn.moved(within.begin)?.max(span.begin),
                    turn.moved(within.end)?.min(span.end),
                ),
            };
            out.push(self.hap_at(index, whole, part));
        }
        note_accumulated(out.len());
        Ok(())
    }

    /// Whether this layer is one step that fills each cycle, as a constant
    /// is.
    fn fills_each_cycle(&self) -> bool {
        let cycle = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
        self.period == Fraction::ONE
            && matches!(&*self.places, [place] if place.whole == cycle && place.part.is_none())
    }

    /// This layer where each step meets the steps of `values`, as the hap
    /// of the step meets their haps in an application: the step keeps its
    /// whole, and its part is cut to the part of each step of `values` that
    /// it meets. `side` is the side of the application that this layer is:
    /// the function side of an `appLeft`, or the value side of an
    /// `appRight`, where `values` holds the functions. With the layer, each
    /// pair of values that meet, this layer's first: a step with the value
    /// `n` holds what the pair `n` makes, which the caller puts in the
    /// place of the pairs.
    ///
    /// `None` for more steps than a fold makes, for a period past the
    /// largest term, and for steps that are then not in the order of time.
    fn met(&self, values: &Layer, side: Side) -> Option<(Layer, Vec<(u32, u32)>)> {
        let keys = match side {
            Side::Functions => ContextKeys::merge(&self.keys, &values.keys),
            Side::Values => ContextKeys::merge(&values.keys, &self.keys),
        };
        // A step that lies in one cycle meets a value that fills the cycle
        // over the whole part: the places stay, and the locations of the
        // value go beside those of each step.
        if self.in_cycles && values.fills_each_cycle() {
            let pairs = (0..u32::try_from(self.values.len()).ok()?)
                .map(|value| (value, 0))
                .collect();
            let met = self.with(|layer| {
                match side {
                    Side::Functions => {
                        let mut before = values.context(0);
                        before.append(&layer.before);
                        layer.before = before;
                    }
                    Side::Values => layer.after.append(&values.context(0)),
                }
                layer.keys = keys;
            });
            return Some((met, pairs));
        }
        // Both layers repeat in the least period that holds a whole count
        // of turns of each.
        let small = |term: i128| (1..=MAX_PERIOD_TERM).contains(&term);
        let period = self.period.checked_lcm(values.period)?;
        if !(small(period.numer()) && small(period.denom())) {
            return None;
        }
        let turns = period.checked_div(self.period)?.numer();
        let moved = |span: TimeSpan, start: Fraction| {
            Some(TimeSpan::new(
                span.begin.checked_add(start)?,
                span.end.checked_add(start)?,
            ))
        };
        let mut pairs: Vec<(u32, u32)> = Vec::new();
        let mut places = Vec::new();
        for turn in 0..turns {
            let start = self.period.checked_mul(Fraction::int(turn))?;
            for place in self.places.iter() {
                let (whole, part) = (moved(place.whole, start)?, moved(place.part(), start)?);
                // The locations of each hap of this step. Those after stay
                // with the layer of the function side, which they end.
                let mut own = self.before.clone();
                own.append(&place.locations);
                if side == Side::Values {
                    own.append(&self.after);
                }
                // The turns of `values` that the part meets, in order.
                let mut at = part.begin.checked_div(values.period)?.floor();
                let last = part.end.checked_div(values.period)?.ceil();
                while at < last {
                    let value_start = values.period.checked_mul(at)?;
                    // The part, from the start of the turn, and the steps
                    // of the turn that it meets: they are in the order of
                    // time.
                    let (begin, end) = (
                        part.begin.checked_sub(value_start)?,
                        part.end.checked_sub(value_start)?,
                    );
                    let first = values
                        .places
                        .partition_point(|value_place| value_place.part().end <= begin);
                    let meeting = values.places[first..]
                        .iter()
                        .take_while(|value_place| value_place.part().begin < end);
                    for (value_index, value_place) in (first..).zip(meeting) {
                        let value_part = moved(value_place.part(), value_start)?;
                        let piece = TimeSpan::new(
                            part.begin.max(value_part.begin),
                            part.end.min(value_part.end),
                        );
                        if places.len() == MAX_FOLDED_STEPS {
                            return None;
                        }
                        // The pairs are few: one for each value that a
                        // value meets.
                        let pair = (place.value, value_place.value);
                        let value = match pairs.iter().position(|known| *known == pair) {
                            Some(value) => value,
                            None => {
                                pairs.push(pair);
                                pairs.len() - 1
                            }
                        };
                        let value = u32::try_from(value).ok()?;
                        // The value side's locations come first.
                        let locations = match side {
                            Side::Functions => {
                                let mut locations = values.context(value_index);
                                locations.append(&own);
                                locations
                            }
                            Side::Values => {
                                let mut locations = own.clone();
                                locations.append(&values.context(value_index));
                                locations
                            }
                        };
                        places.push(Place {
                            whole,
                            part: (piece != whole).then(|| Box::new(piece)),
                            value,
                            locations,
                        });
                    }
                    at = at.checked_add(Fraction::ONE)?;
                }
            }
        }
        let met = Layer {
            in_cycles: lie_in_cycles(period, &places),
            period,
            places: places.into(),
            before: Locations::new(),
            after: match side {
                Side::Functions => self.after.clone(),
                Side::Values => Locations::new(),
            },
            contexts: OnceLock::new(),
            keys,
            ..self.clone()
        };
        met.is_in_order().then_some((met, pairs))
    }
}

/// The most turns of a period that [`lie_in_cycles`] reads.
const MAX_TURNS_READ: i128 = 64;

/// Whether the part of each of `places` lies in one cycle, in every turn of
/// a period of `period` cycles. `false` also for a period whose turns are
/// too many to read: a cut then settles it.
fn lie_in_cycles(period: Fraction, places: &[Place]) -> bool {
    // The bounds of the cycles are at the same places in each count of
    // turns that makes a whole count of cycles.
    let turns = period.denom();
    turns <= MAX_TURNS_READ
        && (0..turns).all(|turn| {
            let Some(start) = period.checked_mul(Fraction::int(turn)) else {
                return false;
            };
            places.iter().all(|place| {
                let part = place.part();
                match (part.begin.checked_add(start), part.end.checked_add(start)) {
                    (Some(begin), Some(end)) => end <= begin.floor() + Fraction::ONE,
                    _ => false,
                }
            })
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
    node(
        layers
            .into_iter()
            .map(Layer::of)
            .collect::<Option<Vec<_>>>()?,
    )
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

/// One turn of a period, for a query of a span.
#[derive(Clone, Copy)]
struct Turn {
    index: i128,
    /// Where the turn starts.
    start: Fraction,
    /// The cycles in `start` when it is a whole count of them, which moves
    /// a time with no division.
    whole_cycles: Option<i128>,
    /// The span, from the start of the turn.
    begin: Fraction,
    end: Fraction,
}

impl Turn {
    /// `time`, which is from the start of the turn, as a time of the query.
    fn moved(&self, time: Fraction) -> Result<Fraction, SequenceStop> {
        match self.whole_cycles {
            Some(cycles) => time.checked_add_int(cycles),
            None => time.checked_add(self.start),
        }
        .ok_or(SequenceStop::Range)
    }
}

/// The turns of a period that a span meets. Layers of one period read each
/// turn here, so layers that follow one another work it out one time.
struct Turns {
    period: Fraction,
    span: TimeSpan,
    indices: std::ops::Range<i128>,
    /// The turn that was read last.
    read: Option<Turn>,
}

impl Turns {
    fn of(period: Fraction, span: TimeSpan) -> Result<Self, SequenceStop> {
        let in_range = |turn: Option<i128>| turn.ok_or(SequenceStop::Range);
        // A span of no width meets the turn that holds its one time.
        let first = in_range(span.begin.checked_div_floor(period))?;
        let last = match span.begin == span.end {
            true => in_range(first.checked_add(1))?,
            false => in_range(span.end.checked_div_ceil(period))?,
        };
        // One turn is one cycle of the steps: a span of more turns than a
        // query walks cycles is refused as that query is.
        if last
            .checked_sub(first)
            .is_none_or(|turns| turns > MAX_QUERY_SPAN_CYCLES)
        {
            refuse(QueryLimit::QuerySpan {
                cycles: MAX_QUERY_SPAN_CYCLES,
            });
            return Err(SequenceStop::Query);
        }
        Ok(Self {
            period,
            span,
            indices: first..last,
            read: None,
        })
    }

    fn turn(&mut self, index: i128) -> Result<Turn, SequenceStop> {
        if let Some(turn) = self.read.filter(|turn| turn.index == index) {
            return Ok(turn);
        }
        let in_range = |time: Option<Fraction>| time.ok_or(SequenceStop::Range);
        let start = match self.period == Fraction::ONE {
            true => Fraction::int(index),
            false => in_range(self.period.checked_mul(Fraction::int(index)))?,
        };
        let turn = Turn {
            index,
            start,
            whole_cycles: (start.denom() == 1).then(|| start.numer()),
            begin: in_range(self.span.begin.checked_sub(start))?,
            end: in_range(self.span.end.checked_sub(start))?,
        };
        self.read = Some(turn);
        Ok(turn)
    }
}

/// Push the haps of `layers`, which share a period, in `span`: each layer
/// in turn, as a stack of them answers.
fn query_layers(layers: &[Layer], span: TimeSpan, out: &mut Vec<Hap>) -> Result<(), SequenceStop> {
    let Some(first) = layers.first() else {
        return Ok(());
    };
    let mut turns = Turns::of(first.period, span)?;
    for layer in layers {
        for index in turns.indices.clone() {
            // The query of the node asked before the first turn.
            if index != turns.indices.start && query_stop_requested() {
                return Err(SequenceStop::Query);
            }
            layer.push(&turns.turn(index)?, span, out)?;
        }
    }
    Ok(())
}

/// The haps of `layers` in `state`'s span. See [`sequence`].
pub(crate) fn query_sequence(layers: &[Layer], state: &State) -> Vec<Hap> {
    let mut out = Vec::new();
    for layers in layers.chunk_by(|a, b| a.period == b.period) {
        match query_layers(layers, state.span, &mut out) {
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

/// Whether each hap of `layers` holds data only.
pub(crate) fn holds_data(layers: &[Layer]) -> bool {
    layers.iter().all(Layer::is_data)
}

/// Whether the graph of `pattern` holds each value that it answers, and
/// each is data only: a sequence or a constant of data values.
pub(crate) fn is_data(pattern: &Pattern) -> bool {
    match pattern.node_kind() {
        Node::Sequence(layers) => holds_data(layers),
        Node::Pure(value) => value.is_data(0),
        Node::AddContext(inner, _) => is_data(inner),
        _ => false,
    }
}

/// The one data value of `layers`, when they are one value that fills each
/// cycle, as a `pure` is.
pub(crate) fn constant_value(layers: &[Layer]) -> Option<&Value> {
    let cycle = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
    match layers {
        // A reader makes a hap of the value alone, so keys make no constant.
        [layer] if layer.period == Fraction::ONE && layer.keys.is_empty() => match &*layer.places {
            [place] if place.whole == cycle && place.part.is_none() => layer
                .values()
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

/// Whether a constructor may fold `pattern`: its handle is not marked
/// volatile, and the runtime that it binds, if any, changes no answer.
pub(crate) fn folds(pattern: &Pattern) -> bool {
    #[cfg(test)]
    if UNFOLDED.get() {
        return false;
    }
    // A runtime goes to the functions and the patterns that a value holds.
    // Silence and data hold none, so they answer the same under each
    // runtime.
    let reads_no_runtime = || pattern.is_rest() || is_data(pattern);
    !pattern.is_volatile() && (pattern.runtime_settings.is_none() || reads_no_runtime())
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
    holds_a_value_and_keys(hap) && hap.keys.is_empty()
}

/// Whether `hap` holds a value, its locations and the keys of its context,
/// and nothing more: what a step holds with the keys of its layer.
fn holds_a_value_and_keys(hap: &Hap) -> bool {
    hap.live_controls == [0; 4]
        && hap.slider_binding == 0
        && hap.ui_visuals == 0
        && hap.pick_lookup.is_none()
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
            .map(|layer| {
                let values: Option<Vec<Value>> = layer.values().iter().map(&mut change).collect();
                Some(layer.valued(values?.into()))
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

/// `selector`, the sequence that selects for a pick, with `locations`
/// before the locations of each hap, when each hap of it holds its value
/// and its locations alone. The join of a pick puts the locations of its
/// selector first and keeps the keys of the hap that it picked, so a change
/// of the picked patterns is then a change of the pick. With no location,
/// the selector as it is.
///
/// `whole_cycles` asks more: each step begins on a whole cycle, so a
/// restart or a reset moves the picked pattern by whole cycles, and its
/// cycles stay the cycles of the query.
pub(crate) fn selecting(
    selector: &Pattern,
    locations: &Locations,
    whole_cycles: bool,
) -> Option<Pattern> {
    let whole = |time: Fraction| time.denom() == 1;
    let layers = folded(selector)?;
    let plain = layers.iter().all(|layer| {
        layer.keys.is_empty()
            && (!whole_cycles
                || (whole(layer.period)
                    && layer.places.iter().all(|place| whole(place.whole.begin))))
    });
    if !plain {
        return None;
    }
    if locations.is_empty() {
        return Some(selector.clone());
    }
    let layers = layers
        .iter()
        .map(|layer| {
            layer.with(|layer| {
                let mut before = locations.clone();
                before.append(&layer.before);
                layer.before = before;
            })
        })
        .collect();
    Some(node_of_sound(layers).with_steps(selector.steps))
}

/// Whether `pattern` is a sequence whose steps each lie in one cycle, or
/// silence: a constant, which is another hap in each cycle, then cuts no
/// hap of it.
pub(crate) fn lies_in_cycles(pattern: &Pattern) -> bool {
    match folded(pattern) {
        Some(layers) => layers.iter().all(|layer| layer.in_cycles),
        None => pattern.is_rest() && folds(pattern),
    }
}

/// The value of `values` and its locations, when `values` is a constant
/// of a plain data value. See [`constant`].
pub(crate) fn constant_value_at(values: &Pattern) -> Option<(Value, Locations)> {
    let hap = constant(values)?;
    is_plain(&hap).then_some((hap.value, hap.context))
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

/// `pattern` with each hap changed by `map`, a map that reads no span, when
/// `pattern` is a sequence or a constant of data values. The map can set
/// the keys of the context, as `scale` does: the haps of a layer then hold
/// the same keys.
///
/// `None` when the map drops a hap, moves it, gives it more than a value
/// and keys, or gives two haps of one layer other keys.
pub(crate) fn mapped_haps(pattern: &Pattern, map: &HapMapFn) -> Option<Pattern> {
    let layers = layers(pattern)?.into_owned();
    let cycle = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
    without_query_signals(|| {
        let layers = layers
            .into_iter()
            .map(|layer| {
                let mut keys: Option<ContextKeys> = None;
                let values = layer
                    .values()
                    .iter()
                    .map(|value| {
                        // A value of the script is read through its host,
                        // at each query.
                        if !value.is_data(0) {
                            return None;
                        }
                        let mut hap = cycle_hap(value.clone());
                        hap.keys = layer.keys.clone();
                        let hap = map(&hap)?;
                        let keys = keys.get_or_insert_with(|| hap.keys.clone());
                        ((hap.whole, hap.part) == (Some(cycle), cycle)
                            && holds_a_value_and_keys(&hap)
                            && hap.context.is_empty()
                            && keys.same(&hap.keys))
                        .then_some(hap.value)
                    })
                    .collect::<Option<Vec<Value>>>()?;
                let mut layer = layer.valued(values.into());
                if let Some(keys) = keys {
                    layer.keys = keys;
                }
                Some(layer)
            })
            .collect::<Option<Vec<_>>>()?;
        Some(node_of_sound(layers))
    })
}

/// Whether a fold may run `apply` when a pattern is built: it is a compose
/// op, a function of its two values and nothing more.
fn runs_at_the_build(apply: &Combine) -> bool {
    match apply {
        Combine::Composed(_) => true,
        Combine::With(closure) => is_op_closure(closure),
    }
}

/// What `function` and `value` make by `apply`, as the hap of a step makes
/// it where the two meet, when they make a value and nothing more.
fn made(function: &Value, value: &Value, apply: &Combine, flow: LookupFlow) -> Option<Value> {
    let mut hap = cycle_hap(function.clone());
    let meets = cycle_hap(value.clone());
    let part = hap.part;
    // A meeting that is not made in place is made as its node makes it.
    if !Meeting::in_place(&mut hap, &meets, part, apply, flow) {
        hap = applied_hap(&hap, &meets, hap.whole, part, apply, flow);
    }
    is_plain(&hap).then_some(hap.value)
}

/// The `appLeft` of `functions` and `values` by `apply`, when `functions`
/// is a sequence or a constant and `values` is one layer of plain values,
/// or a constant of one: each step meets the values where the step is, and
/// is cut to each that it meets, as the meeting cuts its hap. A constant is
/// another hap in each cycle, so it cuts a step that crosses a bound of a
/// cycle there.
///
/// `None` for values of more layers than one, which answer each step layer
/// by layer, for a value that is not data, for a function that is no
/// compose op, for a meeting that makes more than a value, and for cut
/// steps that are not in the order of time.
pub(crate) fn applied(
    functions: &Pattern,
    values: &Pattern,
    apply: &Combine,
    flow: LookupFlow,
) -> Option<Pattern> {
    if !runs_at_the_build(apply) {
        return None;
    }
    let layers = layers(functions)?;
    let values = self::layers(values)?;
    let [values] = &*values else {
        return None;
    };
    // A value of the script is read through its host, at each query.
    if !(values.is_data() && holds_data(&layers)) {
        return None;
    }
    without_query_signals(|| {
        let met = layers
            .iter()
            .map(|layer| {
                if let Some(set) = layer.set(values, apply, flow) {
                    return Some(set);
                }
                let (met, pairs) = layer.met(values, Side::Functions)?;
                // What each pair of values makes, with no location: the
                // locations of both sides are those of the steps.
                let values = pairs
                    .into_iter()
                    .map(|(function, value)| {
                        made(
                            layer.values().get(function as usize)?,
                            values.values().get(value as usize)?,
                            apply,
                            flow,
                        )
                    })
                    .collect::<Option<Vec<Value>>>()?;
                Some(met.valued(values.into()))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(node_of_sound(met))
    })
}

/// The `appRight` of `functions` and `values` by `apply`, when `values` is
/// a sequence and `functions` is one layer of data values, or a constant
/// of one, as the `struct` of a sequence is: each step of `values` keeps
/// its whole, and is cut to each step of `functions` that it meets.
///
/// `None` for functions of more layers than one, which answer each step
/// layer by layer, for a value that is not data, for a function that is no
/// compose op, for a meeting that makes more than a value, and for cut
/// steps that are not in the order of time.
pub(crate) fn applied_out(
    functions: &Pattern,
    values: &Pattern,
    apply: &Combine,
    flow: LookupFlow,
) -> Option<Pattern> {
    if !runs_at_the_build(apply) {
        return None;
    }
    let functions = layers(functions)?;
    let [functions] = &*functions else {
        return None;
    };
    let values = layers(values)?;
    // A value of the script is read through its host, at each query.
    if !(functions.is_data() && holds_data(&values)) {
        return None;
    }
    without_query_signals(|| {
        let met = values
            .iter()
            .map(|layer| {
                let (met, pairs) = layer.met(functions, Side::Values)?;
                // What each pair of values makes, with no location: the
                // locations of both sides are those of the steps.
                let values = pairs
                    .into_iter()
                    .map(|(value, function)| {
                        let hf = cycle_hap(functions.values().get(function as usize)?.clone());
                        let hv = cycle_hap(layer.values().get(value as usize)?.clone());
                        let hap = applied_hap(&hf, &hv, hv.whole, hv.part, apply, flow);
                        is_plain(&hap).then_some(hap.value)
                    })
                    .collect::<Option<Vec<Value>>>()?;
                Some(met.valued(values.into()))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(node_of_sound(met))
    })
}

/// `pattern` with no step whose value `keep` refuses, when `pattern` is a
/// sequence of data values: what a filter of the values answers.
pub(crate) fn filtered(pattern: &Pattern, keep: &ValuePredicate) -> Option<Pattern> {
    let layers = folded(pattern)?;
    if !holds_data(layers) {
        return None;
    }
    without_query_signals(|| {
        let layers = layers
            .iter()
            .map(|layer| {
                let kept: Vec<bool> = layer.values().iter().map(|value| keep(value)).collect();
                if kept.iter().all(|kept| *kept) {
                    return layer.clone();
                }
                let places: Arc<[Place]> = layer
                    .places
                    .iter()
                    .filter(|place| kept.get(place.value as usize).copied().unwrap_or(false))
                    .cloned()
                    .collect();
                layer.with(|layer| {
                    layer.in_cycles = lie_in_cycles(layer.period, &places);
                    layer.places = places;
                })
            })
            .collect();
        Some(node_of_sound(layers))
    })
}

/// The stack of `patterns`, when each is a sequence, a constant or silence:
/// the layers of each, one after the other.
pub(crate) fn stacked(patterns: &[Pattern]) -> Option<Pattern> {
    let mut stacked = Vec::new();
    for pattern in patterns {
        if pattern.is_rest() && folds(pattern) {
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
    let faster = |span: TimeSpan| {
        Some(TimeSpan::new(
            span.begin.checked_div(factor)?,
            span.end.checked_div(factor)?,
        ))
    };
    let layers = layers
        .iter()
        .map(|layer| {
            let period = layer.period.checked_div(factor).filter(small)?;
            // The times of the steps are in cycles, so they change with
            // the speed.
            let places = layer
                .places
                .iter()
                .map(|place| {
                    Some(Place {
                        whole: faster(place.whole)?,
                        part: match &place.part {
                            Some(part) => Some(Box::new(faster(**part)?)),
                            None => None,
                        },
                        ..place.clone()
                    })
                })
                .collect::<Option<Arc<[Place]>>>()?;
            Some(layer.with(|layer| {
                layer.in_cycles = lie_in_cycles(period, &places);
                layer.period = period;
                layer.places = places;
            }))
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
                    "{hap:?} {:?} {} {} {:?}",
                    hap.live_controls, hap.slider_binding, hap.ui_visuals, hap.keys
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
            let chains: [(&str, &dyn Fn(Pattern) -> Pattern); 3] = [
                // A chain of constants that each set a control: a key that
                // is new, and a key that an earlier constant set.
                ("set controls", &|pattern: Pattern| {
                    let notes = pattern.map_control(ControlSpec::new(["n"]), false);
                    [("gain", 0.8), ("room", 0.3), ("gain", 0.25), ("n", 7.0)]
                        .into_iter()
                        .fold(notes, |so_far, (key, value)| {
                            compose(
                                &so_far,
                                &controls(key, value),
                                ComposeOp::Set,
                                Alignment::In,
                            )
                        })
                }),
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
    fn values_applied_to_a_sequence_answer_as_the_application() {
        let controls =
            |pattern: Pattern, key: &str| pattern.map_control(ControlSpec::new([key]), false);
        let mut folds = Vec::new();
        for (name, layers) in sequences() {
            for (value_name, value_layers) in sequences() {
                let name = format!("{name} with {value_name}");
                // Numbers under arithmetic, and control objects that the
                // values set.
                let added = |pattern: Pattern| {
                    compose(
                        &pattern,
                        &built(&value_layers),
                        ComposeOp::Add,
                        Alignment::In,
                    )
                };
                let set = |pattern: Pattern| {
                    compose(
                        &controls(pattern, "n"),
                        &controls(built(&value_layers), "gain"),
                        ComposeOp::Set,
                        Alignment::In,
                    )
                };
                let chains: [&dyn Fn(Pattern) -> Pattern; 2] = [&added, &set];
                for chain in chains {
                    let folded = chain(built(&layers));
                    if matches!(folded.node_kind(), Node::Sequence(_)) {
                        folds.push(name.clone());
                    }
                    assert_same_answers(&name, &folded, &unfolded(|| chain(built(&layers))));
                }
            }
        }
        // Steps meet steps of another period, and steps that they cut.
        for name in [
            "steps with steps of several cycles",
            "steps of several cycles with steps",
            "steps with a period of a cycle and a half",
            "a period of a cycle and a half with steps of several cycles",
            "a chord and fast steps with one value a cycle",
        ] {
            assert_eq!(
                folds.iter().filter(|fold| *fold == name).count(),
                2,
                "{name}"
            );
        }
        // Values of two layers answer each step layer by layer.
        assert!(
            !folds
                .iter()
                .any(|fold| fold.ends_with("with a chord and fast steps"))
        );
    }

    #[test]
    fn a_structure_of_a_sequence_answers_as_the_application() {
        // Steps that keep and steps that drop, in one cycle and in a period
        // of a cycle and a half.
        let structures = || {
            vec![
                (
                    "a structure of one cycle",
                    vec![layer(
                        (1, 1),
                        vec![
                            step((0, 1), (1, 4), 1.0),
                            step((1, 4), (1, 2), 0.0),
                            step((1, 2), (1, 1), 1.0),
                        ],
                    )],
                ),
                (
                    "a structure of two layers",
                    vec![
                        layer(
                            (3, 2),
                            vec![step((0, 1), (1, 3), 0.0), step((1, 3), (1, 1), 1.0)],
                        ),
                        layer((1, 2), vec![step((1, 4), (3, 4), 1.0)]),
                    ],
                ),
            ]
        };
        let mut folds = Vec::new();
        for (name, layers) in sequences() {
            for (structure_name, structure) in structures() {
                let name = format!("{name} with {structure_name}");
                // `struct` and `mask`, and numbers that the values add to.
                let chains: [(&str, ComposeOp, Alignment); 3] = [
                    ("struct", ComposeOp::KeepIf, Alignment::Out),
                    ("mask", ComposeOp::KeepIf, Alignment::In),
                    ("add out", ComposeOp::Add, Alignment::Out),
                ];
                for (chain_name, op, how) in chains {
                    let chain = |pattern: Pattern| compose(&pattern, &built(&structure), op, how);
                    let folded = chain(built(&layers));
                    if matches!(folded.node_kind(), Node::Sequence(_) | Node::Silence) {
                        folds.push(format!("{chain_name} of {name}"));
                    }
                    assert_same_answers(
                        &format!("{chain_name} of {name}"),
                        &folded,
                        &unfolded(|| chain(built(&layers))),
                    );
                }
            }
        }
        // Values of two layers answer each step layer by layer, and so do
        // functions of two layers: any other pair is one node.
        assert!(folds.len() >= 18, "{folds:#?}");
    }

    #[test]
    fn a_scale_of_a_sequence_answers_as_the_map_of_each_hap() {
        let scaled = |pattern: Pattern, name: &str| {
            let degrees = pattern.map_control(ControlSpec::new(["n"]), false);
            crate::combinators::scale(&degrees, Value::Str(name.into()))
        };
        for (name, layers) in sequences() {
            // The scale names each note and its own key of the context,
            // and controls and another scale come after it.
            let chains: [(&str, &dyn Fn(Pattern) -> Pattern); 3] = [
                ("a scale", &|pattern: Pattern| scaled(pattern, "C:minor")),
                ("controls after a scale", &|pattern: Pattern| {
                    let gain = pure(Value::F64(0.5)).map_control(ControlSpec::new(["gain"]), false);
                    compose(
                        &scaled(pattern, "D:dorian"),
                        &gain,
                        ComposeOp::Set,
                        Alignment::In,
                    )
                }),
                ("a scale of numbers", &|pattern: Pattern| {
                    crate::combinators::scale(&pattern, Value::Str("E:major".into()))
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
        // A step that is no degree drops its hap at each query: the map
        // keeps its node.
        let text = |pattern: Pattern| {
            let words = pattern.fmap(|_| Value::Str("no degree".into()));
            crate::combinators::scale(&words, Value::Str("C:minor".into()))
        };
        let dropped = text(built(&sequences()[0].1));
        assert!(!matches!(dropped.node_kind(), Node::Sequence(_)));
        assert_same_answers(
            "a step that is no degree",
            &dropped,
            &unfolded(|| text(built(&sequences()[0].1))),
        );
    }

    /// A function call gives a handle that binds the runtime of the call.
    #[test]
    fn a_fold_reads_data_through_the_runtime_that_a_handle_binds() {
        let bound = |pattern: Pattern| {
            pattern.with_runtime_settings(crate::settings::RuntimeSettings::default())
        };
        let changed = |pattern: Pattern| {
            stack(vec![
                pattern,
                bound(located_value(2.0)),
                bound(crate::silence()),
            ])
            .map_control(ControlSpec::new(["n"]), false)
            .fast(frac((3, 2)))
        };
        for (name, layers) in sequences() {
            let folded = changed(bound(built(&layers)));
            assert!(matches!(folded.node_kind(), Node::Sequence(_)), "{name}");
            assert_same_answers(name, &folded, &unfolded(|| changed(bound(built(&layers)))));
        }
        // A list of haps is no data: a query reads it under the runtime.
        let haps = bound(pure(Value::Haps(crate::value::HapList::new(Vec::new()))));
        let kept = stack(vec![built(&sequences()[0].1), haps]);
        assert!(matches!(kept.node_kind(), Node::Stack(_)));
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
        // Values of two layers, a factor that is not above zero, a handle
        // that is marked volatile, and a stack with a signal in it.
        let layered = built(&sequences()[1].1);
        let unknown = [
            compose(&steps(), &layered, ComposeOp::Add, Alignment::In),
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
