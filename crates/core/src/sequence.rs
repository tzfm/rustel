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

use crate::compose::{ComposeOp, compose_data_in_place, is_op_closure, union_drops};
use crate::context_keys::ContextKeys;
use crate::value::Table;
use crate::{
    Combine, Hap, HapMapFn, Locations, LookupFlow, MAX_QUERY_SPAN_CYCLES, Meeting, Node,
    OwnedHapMapFn, Pattern, QueryLimit, State, TimeSpan, Value, ValuePredicate, applied_hap,
    charge_haps, note_accumulated, query_stop_requested, refuse, silence, without_query_signals,
};
use rustel_fraction::{Fraction, TickGrid};
use std::borrow::Cow;
use std::num::NonZeroU32;
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
    /// The part of a hap that the bound of a cycle cut, inside the whole.
    /// `None` for a part that is the whole.
    part: Option<Box<TimeSpan>>,
    value: Value,
    locations: Locations,
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
        }
    }

    fn part(&self) -> &TimeSpan {
        self.part.as_deref().unwrap_or(&self.whole)
    }

    /// Where the part of the step begins, as a share of the period.
    pub fn begin(&self) -> Fraction {
        self.part().begin
    }

    /// Where the part of the step ends, as a share of the period.
    pub fn end(&self) -> Fraction {
        self.part().end
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
            part: (part != whole).then(|| Box::new(TimeSpan::new(part.0, part.1))),
            ..self.clone()
        }
    }

    /// More locations, after the step's own.
    pub fn locate(&mut self, locations: &[(usize, usize)]) {
        self.locations.extend_from_slice(locations);
    }

    /// More locations, before the step's own.
    pub fn locate_first(&mut self, locations: &[(usize, usize)]) {
        let mut first: Locations = locations.iter().copied().collect();
        first.append(&self.locations);
        self.locations = first;
    }
}

/// Steps that repeat every `period` cycles, as a caller of [`sequence`]
/// gives them.
#[derive(Clone, Debug)]
pub struct SequenceLayer {
    pub period: Fraction,
    pub steps: Vec<SequenceStep>,
}

/// The steps of one layer, written when a reader asks, with no list of
/// them: a layer of [`sequence_of`] answers a query from its source.
///
/// A source holds one step or more in each period. The steps are in the
/// order of time: each begins and ends at or after the step before it.
/// Each lies inside the period.
pub trait StepSource: Send + Sync {
    /// How many steps a period holds.
    fn count(&self) -> usize;

    /// Give `each` the steps that meet the span from `begin` to `end`, in
    /// cycles from the start of the period, in the order of time: a step
    /// that holds `begin`, and a step that begins after `begin` and before
    /// `end`. A span of no width meets the steps that hold its one time.
    /// `None` for a span that the source does not read: the layer then
    /// answers from a list of the steps of the period, which each source
    /// reads from zero to its end.
    fn steps(&self, begin: Fraction, end: Fraction, each: &mut dyn FnMut(SourceStep))
    -> Option<()>;
}

/// One step that a [`StepSource`] writes: its window in cycles from the
/// start of the period, the place of its value among the values of its
/// layer, and its locations.
pub struct SourceStep {
    pub begin: Fraction,
    pub end: Fraction,
    pub value: u32,
    pub locations: Locations,
}

/// One layer of [`sequence_of`]: steps that repeat every `period` cycles,
/// each with one of `values`.
pub struct SourcedLayer {
    pub period: Fraction,
    pub values: Arc<[Value]>,
    /// Whether each step lies in one cycle, in every period.
    pub in_cycles: bool,
    pub steps: Arc<dyn StepSource>,
}

/// A layer of a sequence, as its maker gives it.
pub enum GivenLayer {
    /// Each step, in a list.
    Listed(SequenceLayer),
    /// The steps at a source, which the layer reads when a query asks.
    Sourced(SourcedLayer),
}

/// Where a layer keeps its steps.
#[derive(Clone)]
enum Steps {
    /// Each step in a list.
    Listed(Listing),
    /// The steps as their source writes them for each query.
    Source(Arc<Sourced>),
}

/// The source of the steps of a layer, and their list when a fold read
/// them: each layer that a fold makes of the same steps shares it.
struct Sourced {
    steps: Arc<dyn StepSource>,
    read: OnceLock<Listing>,
}

/// The steps of a layer in a list: on a grid when their times fit one,
/// otherwise each at its place.
#[derive(Clone)]
enum Listing {
    Places(Listed),
    Grid(Arc<Grid>),
}

/// A step as a maker gives it, before the layer keeps it in a list.
struct Raw {
    whole: TimeSpan,
    /// `None` for a part that is the whole.
    part: Option<TimeSpan>,
    value: u32,
    locations: Locations,
}

impl Raw {
    fn part(&self) -> TimeSpan {
        self.part.unwrap_or(self.whole)
    }

    fn place(self) -> Place {
        Place::at(self.whole, self.part, self.value, self.locations)
    }
}

impl Listing {
    /// The list of `raws`: a grid when their times fit one. A list that
    /// does not fit becomes places one time, here.
    fn of(mut raws: Vec<Raw>) -> Self {
        if let Some(grid) = Grid::build(&mut raws) {
            return Self::Grid(Arc::new(grid));
        }
        let small = |span: &TimeSpan| {
            i32::try_from(span.begin.denom()).is_ok() && i32::try_from(span.end.denom()).is_ok()
        };
        let small_denominators = raws
            .iter()
            .all(|raw| small(&raw.whole) && raw.part.as_ref().is_none_or(small));
        Self::Places(Listed {
            places: raws.into_iter().map(Raw::place).collect(),
            small_denominators,
        })
    }

    fn len(&self) -> usize {
        match self {
            Self::Places(listed) => listed.places.len(),
            Self::Grid(grid) => grid.steps.len(),
        }
    }

    fn part(&self, index: usize) -> TimeSpan {
        match self {
            Self::Places(listed) => listed.places[index].part(),
            Self::Grid(grid) => grid.part(index),
        }
    }

    fn whole(&self, index: usize) -> TimeSpan {
        match self {
            Self::Places(listed) => listed.places[index].whole,
            Self::Grid(grid) => grid.whole(index),
        }
    }

    /// The whole of the step, and its part when it holds one of its own.
    /// A grid searches its cuts once for both.
    fn times(&self, index: usize) -> (TimeSpan, Option<TimeSpan>) {
        match self {
            Self::Places(listed) => {
                let place = &listed.places[index];
                (place.whole, place.part.as_ref().map(|part| **part))
            }
            Self::Grid(grid) => grid.times(index),
        }
    }

    /// Whether the step holds a part of its own.
    fn is_cut(&self, index: usize) -> bool {
        match self {
            Self::Places(listed) => listed.places[index].part.is_some(),
            Self::Grid(grid) => grid.cut(index).is_some(),
        }
    }

    fn value(&self, index: usize) -> u32 {
        match self {
            Self::Places(listed) => listed.places[index].value,
            Self::Grid(grid) => grid.steps[index].value,
        }
    }

    fn locations(&self, index: usize) -> Locations {
        match self {
            Self::Places(listed) => listed.places[index].locations.clone(),
            Self::Grid(grid) => grid.locations(index),
        }
    }

    /// The step as a [`Raw`], for a fold to change.
    fn raw(&self, index: usize) -> Raw {
        let (whole, part) = self.times(index);
        Raw {
            whole,
            part,
            value: self.value(index),
            locations: self.locations(index),
        }
    }

    /// Whether each time of the list has a denominator that fits 32 bits.
    fn small_denominators(&self) -> bool {
        match self {
            Self::Places(listed) => listed.small_denominators,
            Self::Grid(grid) => grid.small_denominators(),
        }
    }
}

/// The places of one layer and whether their times fit a restart at a small offset.
#[derive(Clone)]
struct Listed {
    places: Arc<[Place]>,
    small_denominators: bool,
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
    /// The [key](time_key) of where the part begins.
    begins: f32,
    /// The locations of the step's own value.
    locations: Locations,
}

/// One step on a [`Grid`]: 16 bytes, where a [`Place`] takes 112.
#[derive(Clone, Copy)]
struct GridStep {
    /// Where the part begins and ends, in ticks.
    part_begin: u32,
    part_end: u32,
    /// The place of the step's value among the layer's values.
    value: u32,
    loc: PackedLoc,
}

/// The locations of a step in 4 bytes. `0` is no location. A set top bit
/// is a place in the spare list. Otherwise one range: its begin from the
/// base of the grid in the high 20 bits and its length plus one in the low
/// 11, so a range of no length stays apart from no range.
#[derive(Clone, Copy, PartialEq, Eq)]
struct PackedLoc(u32);

impl PackedLoc {
    const SPARE: u32 = 1 << 31;
    const LENGTH_BITS: u32 = 11;
    const LENGTH_MASK: u32 = (1 << Self::LENGTH_BITS) - 1;
    const OFFSET_LIMIT: u32 = 1 << 20;

    /// Whether the range is short enough, and begins low enough, to pack
    /// after some base.
    fn fits((begin, end): (usize, usize)) -> bool {
        end.checked_sub(begin)
            .is_some_and(|length| length < Self::LENGTH_MASK as usize)
            && begin < Grid::SMALL as usize
    }

    /// The range from `base`, when it fits.
    fn range(base: u32, (begin, end): (usize, usize)) -> Option<Self> {
        let offset = u32::try_from(begin).ok()?.checked_sub(base)?;
        let length = u32::try_from(end.checked_sub(begin)?).ok()?;
        (offset < Self::OFFSET_LIMIT && length < Self::LENGTH_MASK)
            .then(|| Self(offset << Self::LENGTH_BITS | (length + 1)))
    }
}

/// A step whose whole is not its part.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Cut {
    /// The place of the step.
    index: u32,
    /// The whole, in ticks.
    begin: u32,
    end: u32,
}

/// The steps of a layer on an integer grid of `1 / ticks` cycles, kept
/// as ticks, when each time of them fits in 32 bits.
#[derive(Clone)]
struct Grid {
    /// The ticks in a cycle: the least common multiple of the denominators.
    ticks: NonZeroU32,
    /// The begin that the packed ranges count from, below 2^31. The top bit
    /// is [`Listing::small_denominators`].
    base: u32,
    steps: Box<[GridStep]>,
    /// The reduced time of each residue, for a tick size that is no power
    /// of two: a time then takes no gcd.
    reductions: Option<Arc<TickGrid>>,
    /// What most layers have none of.
    side: Option<Arc<Spare>>,
}

/// The cuts and the spare locations of a [`Grid`].
struct Spare {
    /// The cut steps, in the order of their places.
    cuts: Box<[Cut]>,
    /// The locations that do not pack.
    spare: Box<[Locations]>,
}

impl Grid {
    const SMALL: u32 = 1 << 31;

    /// The steps of `raws` on a grid, or `None` for times that do not fit.
    /// A grid takes the locations that do not pack out of `raws`, and
    /// nothing else.
    fn build(raws: &mut [Raw]) -> Option<Self> {
        u32::try_from(raws.len()).ok()?;
        // The tick size, and the least begin of a range that packs.
        let mut ticks: u64 = 1;
        let mut base: Option<u32> = None;
        // The step before ends where the next begins, and most parts are
        // the whole, so the same denominator comes again and again.
        let mut last = 1;
        let mut small_denominators = true;
        for raw in raws.iter() {
            let times = [raw.whole].into_iter().chain(raw.part);
            for time in times.flat_map(|span| [span.begin, span.end]) {
                if time.denom() == last {
                    continue;
                }
                last = time.denom();
                small_denominators &= i32::try_from(last).is_ok();
                // A denominator past 32 bits refuses before the product of
                // two of them can overflow.
                let denom = u64::from(u32::try_from(last).ok()?);
                if !ticks.is_multiple_of(denom) {
                    ticks = ticks / gcd(ticks, denom) * denom;
                    u32::try_from(ticks).ok()?;
                }
            }
            if let (1, Some(range)) = (raw.locations.len(), raw.locations.get(0))
                && PackedLoc::fits(range)
            {
                // The top bit of the base is a flag of the grid.
                if let Ok(begin) = u32::try_from(range.0)
                    && begin < Self::SMALL
                {
                    base = Some(base.map_or(begin, |base| base.min(begin)));
                }
            }
        }
        let base = base.unwrap_or(0);
        let ticks = NonZeroU32::new(u32::try_from(ticks).ok()?)?;
        // The scale of the denominator before is kept: it comes again.
        let scaled = std::cell::Cell::new((1, ticks.get()));
        let on_grid = |time: Fraction| {
            let (denom, scale) = scaled.get();
            let scale = if denom == time.denom() {
                scale
            } else {
                let scale = ticks.get() / u32::try_from(time.denom()).ok()?;
                scaled.set((time.denom(), scale));
                scale
            };
            u32::try_from(u64::from(u32::try_from(time.numer()).ok()?) * u64::from(scale)).ok()
        };
        let mut cuts = Vec::new();
        // The places of the steps whose locations do not pack.
        let mut spare = Vec::new();
        let mut fits = true;
        // The steps are collected in one allocation, with no list first.
        let steps: Box<[GridStep]> = raws
            .iter()
            .enumerate()
            .map(|(index, raw)| {
                let mut step = || {
                    let (begin, end) = (on_grid(raw.whole.begin)?, on_grid(raw.whole.end)?);
                    let (part_begin, part_end) = match raw.part {
                        None => (begin, end),
                        Some(part) => {
                            cuts.push(Cut {
                                index: index as u32,
                                begin,
                                end,
                            });
                            (on_grid(part.begin)?, on_grid(part.end)?)
                        }
                    };
                    let packed = match (raw.locations.len(), raw.locations.get(0)) {
                        (0, _) => Some(PackedLoc(0)),
                        (1, Some(range)) => PackedLoc::range(base, range),
                        _ => None,
                    };
                    let loc = packed.unwrap_or_else(|| {
                        spare.push(index);
                        PackedLoc(PackedLoc::SPARE | (spare.len() - 1) as u32)
                    });
                    Some(GridStep {
                        part_begin,
                        part_end,
                        value: raw.value,
                        loc,
                    })
                };
                step().unwrap_or_else(|| {
                    fits = false;
                    GridStep {
                        part_begin: 0,
                        part_end: 0,
                        value: 0,
                        loc: PackedLoc(0),
                    }
                })
            })
            .collect();
        if !fits || u32::try_from(spare.len()).ok()? >= PackedLoc::SPARE {
            return None;
        }
        // The grid fits, so the locations move with no copy.
        let side = (!cuts.is_empty() || !spare.is_empty()).then(|| {
            Arc::new(Spare {
                cuts: cuts.into(),
                spare: spare
                    .iter()
                    .map(|index| std::mem::take(&mut raws[*index].locations))
                    .collect(),
            })
        });
        let base = base | if small_denominators { Self::SMALL } else { 0 };
        Some(Self {
            ticks,
            base,
            steps,
            reductions: tick_grid(ticks),
            side,
        })
    }

    fn small_denominators(&self) -> bool {
        self.base & Self::SMALL != 0
    }

    /// `ticks` as a time in cycles, reduced.
    #[inline]
    #[cfg(test)]
    fn made(&self, ticks: i128) -> Fraction {
        match i64::try_from(ticks) {
            Ok(ticks) => self.made_small(ticks),
            Err(_) => Fraction::from_ticks(ticks, self.ticks),
        }
    }

    /// [`Self::made`] of ticks that fit 64 bits, which a query has.
    #[inline]
    fn made_small(&self, ticks: i64) -> Fraction {
        match &self.reductions {
            Some(reductions) => reductions.time(ticks),
            None => Fraction::from_ticks(i128::from(ticks), self.ticks),
        }
    }

    /// `ticks` as a time in cycles.
    fn time(&self, ticks: u32) -> Fraction {
        self.made_small(i64::from(ticks))
    }

    /// The floor and the ceiling of `time` in ticks. `None` past 64 bits.
    fn ticks_at(&self, time: Fraction) -> Option<(i64, i64)> {
        let scaled = i64::try_from(time.numer())
            .ok()?
            .checked_mul(i64::from(self.ticks.get()))?;
        let denom = i64::try_from(time.denom()).ok()?;
        if denom == 1 {
            return Some((scaled, scaled));
        }
        // A time on a power of two, which most spans are, shifts.
        let (floor, rest) = match denom & (denom - 1) == 0 {
            true => (scaled >> denom.trailing_zeros(), scaled & (denom - 1)),
            false => (scaled.div_euclid(denom), scaled.rem_euclid(denom)),
        };
        // A floor under the largest tick has room for one more.
        Some((floor, floor + i64::from(rest != 0)))
    }

    fn cuts(&self) -> &[Cut] {
        self.side.as_ref().map_or(&[], |side| &side.cuts)
    }

    /// The cut of step `index`.
    fn cut(&self, index: usize) -> Option<&Cut> {
        let cuts = self.cuts();
        let at = cuts.partition_point(|cut| (cut.index as usize) < index);
        cuts.get(at).filter(|cut| cut.index as usize == index)
    }

    fn part(&self, index: usize) -> TimeSpan {
        let step = &self.steps[index];
        TimeSpan::new(self.time(step.part_begin), self.time(step.part_end))
    }

    /// The whole of step `index`.
    fn whole(&self, index: usize) -> TimeSpan {
        self.times(index).0
    }

    /// The whole of step `index`, and its part when it is cut.
    fn times(&self, index: usize) -> (TimeSpan, Option<TimeSpan>) {
        let part = self.part(index);
        match self.cut(index) {
            Some(cut) => (
                TimeSpan::new(self.time(cut.begin), self.time(cut.end)),
                Some(part),
            ),
            None => (part, None),
        }
    }

    /// The locations of step `index`.
    #[inline]
    fn locations(&self, index: usize) -> Locations {
        let packed = self.steps[index].loc.0;
        if packed == 0 {
            return Locations::new();
        }
        if packed & PackedLoc::SPARE != 0 {
            let side = self.side.as_ref().expect("a spare location has a side");
            return side.spare[(packed & !PackedLoc::SPARE) as usize].clone();
        }
        let base = self.base & !Self::SMALL;
        let begin = base + (packed >> PackedLoc::LENGTH_BITS);
        // The length takes 11 bits, so it fits 16.
        let length = (packed & PackedLoc::LENGTH_MASK) - 1;
        Locations::one(begin, length as u16)
    }

    /// The places of the steps, for the rare query that leaves the range
    /// of ticks.
    fn places(&self) -> Vec<Place> {
        (0..self.steps.len())
            .map(|index| {
                let (whole, part) = self.times(index);
                Place::at(whole, part, self.steps[index].value, self.locations(index))
            })
            .collect()
    }

    /// Whether the steps are in the order of their haps in a period of
    /// `period` cycles, as [`Layer::is_in_order`] says, compared as ticks.
    /// `None` for a period whose ticks do not fit.
    fn in_order(&self, period: Fraction) -> Option<bool> {
        if period <= Fraction::ZERO {
            return Some(false);
        }
        // A tick is inside the period when `tick / ticks <= n / d`.
        let bound = period.numer().checked_mul(i128::from(self.ticks.get()))?;
        let inside = |tick: u32| Some(i128::from(tick).checked_mul(period.denom())? <= bound);
        let mut cuts = self.cuts().iter().peekable();
        for (index, step) in self.steps.iter().enumerate() {
            let cut = cuts.next_if(|cut| cut.index as usize == index);
            let (begin, end) = cut.map_or((step.part_begin, step.part_end), |c| (c.begin, c.end));
            let shaped =
                begin <= step.part_begin && step.part_begin < step.part_end && step.part_end <= end;
            if !shaped || !inside(end)? {
                return Some(false);
            }
        }
        Some(self.steps.windows(2).all(|pair| {
            pair[0].part_begin <= pair[1].part_begin && pair[0].part_end <= pair[1].part_end
        }))
    }

    /// Whether the steps of both are at the same ticks and cut alike.
    fn same_times(&self, other: &Self) -> bool {
        self.ticks == other.ticks
            && self.cuts() == other.cuts()
            && self.steps.len() == other.steps.len()
            && self
                .steps
                .iter()
                .zip(other.steps.iter())
                .all(|(a, b)| (a.part_begin, a.part_end) == (b.part_begin, b.part_end))
    }
}

thread_local! {
    /// The tables of reduced times that this thread made, by tick size: the
    /// layers of one tick size share one.
    static TICK_GRIDS: std::cell::RefCell<Vec<Arc<TickGrid>>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The table of reduced times for `ticks` in a cycle, when no shift reduces
/// a time and the table is small.
fn tick_grid(ticks: NonZeroU32) -> Option<Arc<TickGrid>> {
    if ticks.is_power_of_two() || i64::from(ticks.get()) > TickGrid::MAX_DENOM {
        return None;
    }
    TICK_GRIDS.with_borrow_mut(|grids| {
        let known = grids
            .iter()
            .find(|grid| grid.denom() == i64::from(ticks.get()));
        known.cloned().or_else(|| {
            let grid = Arc::new(TickGrid::new(i64::from(ticks.get()))?);
            grids.push(Arc::clone(&grid));
            Some(grid)
        })
    })
}

/// The greatest common divisor of two numbers above zero.
fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// A float that orders as `time` does where the floats of two times
/// differ: a search of the steps of a layer compares floats, and compares
/// two times only where their floats are the same. `NAN` for a time that
/// no float orders, which only the time itself orders.
fn time_key(time: Fraction) -> f32 {
    // A float of 32 bits rounds the float of 64 bits, and keeps its order.
    crate::order_key(time) as f32
}

impl Place {
    /// The place of a step with `whole` as its whole, and `part` as its
    /// part where the part is not the whole.
    fn at(whole: TimeSpan, part: Option<TimeSpan>, value: u32, locations: Locations) -> Self {
        Self {
            whole,
            begins: time_key(part.map_or(whole.begin, |part| part.begin)),
            part: part.map(Box::new),
            value,
            locations,
        }
    }

    fn part(&self) -> TimeSpan {
        self.part.as_deref().copied().unwrap_or(self.whole)
    }

    /// Whether the part begins before `time`, whose [key](time_key) is
    /// `key`.
    fn begins_before(&self, time: Fraction, key: f32) -> bool {
        match self.begins.partial_cmp(&key) {
            Some(std::cmp::Ordering::Less) => true,
            Some(std::cmp::Ordering::Greater) => false,
            // The same float, or a time that no float orders.
            _ => self.part().begin < time,
        }
    }

    /// Whether the part begins at `time` or before it. See
    /// [`Self::begins_before`].
    fn begins_by(&self, time: Fraction, key: f32) -> bool {
        match self.begins.partial_cmp(&key) {
            Some(std::cmp::Ordering::Less) => true,
            Some(std::cmp::Ordering::Greater) => false,
            _ => self.part().begin <= time,
        }
    }
}

/// One layer of a sequence node: the places of its steps, and each value
/// of them once. Each hap holds a plain value and its locations: a fold
/// that would give a hap more keeps its own node.
#[derive(Clone)]
pub(crate) struct Layer {
    pub(crate) period: Fraction,
    /// Shared by each layer that a fold makes of the same steps.
    steps: Steps,
    /// The value of each step, each once, before the name and the controls
    /// that the dress gives it.
    values: Arc<[Value]>,
    /// What the folds on the layer give each hap. `None` for a layer that
    /// no fold dressed. See [`Dress`].
    dress: Option<Arc<Dress>>,
    /// The values as the dress makes them, made at the first read.
    values_read: OnceLock<Read>,
    /// Whether each value is data only: a hap of the layer then holds
    /// nothing that a query resolves or binds to a runtime.
    data: bool,
    /// Whether each value is a map of controls.
    maps: bool,
    /// Whether the part of each step lies in one cycle, in every period: a
    /// constant, which is another hap in each cycle, then cuts no step.
    in_cycles: bool,
    /// The locations of each step's hap, as one list, made at the first
    /// query of a layer that has locations of its own.
    contexts: OnceLock<Box<[Locations]>>,
}

/// The values of a layer as its dress makes them.
#[derive(Clone)]
enum Read {
    /// Each value made as a map of its own.
    Made(Arc<[Value]>),
    /// Each value a map that reads the entries of a table the steps share.
    Shared(Arc<Table>),
}

/// The values of a layer as its dress makes them.
#[derive(Clone, Copy)]
enum Values<'a> {
    /// A list of the values.
    Held(&'a [Value]),
    /// The maps that read a table.
    Table(&'a Arc<Table>),
}

/// What makes the haps of the steps of a layer: the list, the values as the
/// dress makes them, and the locations and keys of each hap, all read once
/// for a loop of steps.
struct Maker<'a> {
    listing: &'a Listing,
    values: Values<'a>,
    /// See [`Layer::contexts`].
    contexts: Option<&'a [Locations]>,
    keys: &'a ContextKeys,
}

impl Maker<'_> {
    /// The hap of step `index` at `whole` and `part`.
    fn hap(&self, index: usize, whole: TimeSpan, part: TimeSpan) -> Hap {
        self.make(index, self.listing.value(index), whole, part)
    }

    /// The hap of step `index`, which holds value `value`.
    #[inline]
    fn make(&self, index: usize, value: u32, whole: TimeSpan, part: TimeSpan) -> Hap {
        let value = self
            .values
            .get(value as usize)
            .expect("a step holds a value of its layer");
        let mut hap = Hap::new(Some(whole), part, value);
        hap.context = match self.contexts {
            Some(contexts) => contexts[index].clone(),
            None => self.listing.locations(index),
        };
        hap.keys = self.keys.clone();
        hap
    }
}

impl Values<'_> {
    #[inline]
    fn get(&self, index: usize) -> Option<Value> {
        match self {
            Self::Held(values) => values.get(index).cloned(),
            Self::Table(table) => table.map(index).map(Value::Object),
        }
    }
}
// A layer is cloned by each fold: its steps stay behind a handle.
const _: () = assert!(size_of::<Layer>() <= 144);

/// What the folds on a layer give each hap of it, beyond its step. The
/// layers that a fold dresses alike share one: see [`redressed`].
#[derive(Clone, Default)]
struct Dress {
    /// The names of the control that names each value when it is read: a
    /// value reads as the map that [`crate::controls::named_value`] makes
    /// of it. See [`Layer::named`].
    named: Option<Arc<[Arc<str>]>>,
    /// A control map that each value takes when it is read, after its
    /// name: the union of the constants that a chain of controls set, the
    /// last one last. See [`Layer::set`].
    set: Option<Value>,
    /// The keys of the context of each hap, as a map of the haps set them:
    /// the scale that `scale` names, for one.
    keys: ContextKeys,
    /// The locations of each hap before its step's own: those of the
    /// constants that the steps met, the last first.
    before: Locations,
    /// The locations of each hap after its step's own.
    after: Locations,
}

/// The dress of a layer that no fold dressed.
static BARE: Dress = Dress {
    named: None,
    set: None,
    keys: ContextKeys::NONE,
    before: Locations::new(),
    after: Locations::new(),
};

impl Dress {
    /// Set `control`, a map of controls at `context`, on each value that
    /// this dress makes. See [`Layer::set_here`].
    fn set_here(&mut self, control: &Value, context: Locations) {
        // The value side's locations come first.
        let mut before = context;
        before.append(&self.before);
        self.before = before;
        match &mut self.set {
            Some(set) => compose_data_in_place(ComposeOp::Set, set, control),
            // A handle of the map of the control, which holds its own
            // entries: a map that reads a table would pin that table.
            None => {
                self.set = Some(match control {
                    Value::Object(map) => Value::Object(map.clone().undressed()),
                    other => other.clone(),
                })
            }
        }
    }

    /// Whether the dress gives a hap nothing.
    fn is_bare(&self) -> bool {
        self.named.is_none()
            && self.set.is_none()
            && self.keys.is_empty()
            && self.before.is_empty()
            && self.after.is_empty()
    }

    /// Whether a hap of a step at `locations` holds them alone.
    fn locates_alone(&self) -> bool {
        self.before.is_empty() && self.after.is_empty()
    }

    /// The locations of a hap of a step at `locations`: those before, the
    /// step's own, then those after.
    fn around(&self, locations: &Locations) -> Locations {
        let mut context = self.before.clone();
        context.append(locations);
        context.append(&self.after);
        context
    }

    /// Whether two dresses give each hap the same, with set controls that
    /// `same_value` holds to be the same.
    fn same(&self, other: &Self, same_value: impl Fn(&Value, &Value) -> bool) -> bool {
        let same_set = match (&self.set, &other.set) {
            (Some(a), Some(b)) => same_value(a, b),
            (None, None) => true,
            _ => false,
        };
        same_set
            && self.named == other.named
            && self.keys.same(&other.keys)
            && self.before == other.before
            && self.after == other.after
    }
}

/// One dress for the layers of `layers` that are dressed alike, as a fold
/// that dresses each layer on its own makes them.
fn share_dresses(layers: &mut [Layer]) {
    let mut worn: Vec<Arc<Dress>> = Vec::new();
    for layer in layers {
        let Some(dress) = &layer.dress else {
            continue;
        };
        let same = |other: &Arc<Dress>| {
            Arc::ptr_eq(dress, other) || dress.same(other, |a, b| a.same_data(b))
        };
        match worn.iter().find(|other| same(other)) {
            // A handle of the dress that the layers before wear.
            Some(other) => layer.dress = Some(Arc::clone(other)),
            None => worn.push(Arc::clone(dress)),
        }
    }
}

/// `layers`, each in the dress that `change` makes of its own. Layers that
/// wear one dress before wear one after: a fold dresses the layers of a
/// sequence alike, and they share the new dress.
fn redressed<'a>(
    layers: impl IntoIterator<Item = &'a Layer>,
    change: impl Fn(&mut Dress),
) -> Vec<Layer> {
    let mut last: Option<(*const Dress, Option<Arc<Dress>>)> = None;
    layers
        .into_iter()
        .map(|layer| {
            let from = layer.dress.as_ref().map_or(std::ptr::null(), Arc::as_ptr);
            let dress = match &last {
                Some((seen, dress)) if *seen == from => dress.clone(),
                _ => {
                    let mut dress = layer.dress().clone();
                    change(&mut dress);
                    let dress = (!dress.is_bare()).then(|| Arc::new(dress));
                    last = Some((from, dress.clone()));
                    dress
                }
            };
            layer.with(|layer| layer.dress = dress)
        })
        .collect()
}

impl Layer {
    /// The layer of `layer`, with the times of its steps in cycles. Equal
    /// data values share one place among the values. `None` for a time
    /// outside the native range.
    fn of(layer: SequenceLayer) -> Option<Self> {
        let period = layer.period;
        // Whether each time is in the native range.
        let mut in_range = true;
        // A share of a period of one cycle is its time in cycles. A time
        // outside the native range stays as it is, and the layer is none.
        let mut cycles = |span: TimeSpan| {
            if period == Fraction::ONE {
                return span;
            }
            let times = (span.begin.checked_mul(period)).zip(span.end.checked_mul(period));
            in_range &= times.is_some();
            times.map_or(span, |(begin, end)| TimeSpan::new(begin, end))
        };
        // Each step has a place among the values.
        u32::try_from(layer.steps.len()).ok()?;
        // Room for the values of a short layer, with none to move.
        let mut values: Vec<Value> = Vec::with_capacity(layer.steps.len().min(32));
        // Where each data value is among `values`, by its hash: a table
        // with a slot for each step and as many more, so a value is found
        // in a few steps from the slot of its hash.
        const FREE: u32 = u32::MAX;
        let slots = layer.steps.len().checked_mul(2)?.next_power_of_two();
        let mut known = vec![FREE; slots];
        let place = |step: SequenceStep| {
            // A value that is no data shares no place.
            let slot = step.value.is_data(0).then(|| {
                // The low bits of the hash, which the table is as wide as.
                let mut slot = step.value.data_hash() as usize & (slots - 1);
                while known[slot] != FREE && !values[known[slot] as usize].same_data(&step.value) {
                    slot = (slot + 1) & (slots - 1);
                }
                slot
            });
            let value = match slot.map(|slot| known[slot]) {
                Some(found) if found != FREE => found,
                _ => {
                    let index = values.len() as u32;
                    if let Some(slot) = slot {
                        known[slot] = index;
                    }
                    values.push(step.value);
                    index
                }
            };
            let part = step.part.map(|part| cycles(*part));
            Raw {
                whole: cycles(step.whole),
                part,
                value,
                locations: step.locations,
            }
        };
        // Each step is written one time, where the layer keeps it.
        let raws: Vec<Raw> = layer.steps.into_iter().map(place).collect();
        in_range.then(|| Self::new(period, raws, values.into()))
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
    fn new(period: Fraction, raws: Vec<Raw>, values: Arc<[Value]>) -> Self {
        let in_cycles = lie_in_spans(period, raws.iter().map(Raw::part));
        Self::of_list(period, raws, in_cycles, values)
    }

    /// The layer of `raws`, which it keeps in a list.
    fn of_list(period: Fraction, raws: Vec<Raw>, in_cycles: bool, values: Arc<[Value]>) -> Self {
        let steps = Steps::Listed(Listing::of(raws));
        Self::holding(period, steps, in_cycles, values)
    }

    /// The layer of `layer`, which reads its steps from their source.
    fn of_source(layer: SourcedLayer) -> Self {
        let sourced = Sourced {
            steps: layer.steps,
            read: OnceLock::new(),
        };
        let steps = Steps::Source(Arc::new(sourced));
        Self::holding(layer.period, steps, layer.in_cycles, layer.values)
    }

    /// A layer of `steps` that hold `values`, with no fold on it.
    fn holding(period: Fraction, steps: Steps, in_cycles: bool, values: Arc<[Value]>) -> Self {
        Self {
            in_cycles,
            period,
            steps,
            data: values.iter().all(|value| value.is_data(0)),
            maps: values.iter().all(|value| matches!(value, Value::Object(_))),
            values,
            dress: None,
            values_read: OnceLock::new(),
            contexts: OnceLock::new(),
        }
    }

    /// What the folds on this layer give each hap of it.
    fn dress(&self) -> &Dress {
        self.dress.as_deref().unwrap_or(&BARE)
    }

    /// Change the dress of this layer, which then shares it with no other.
    fn change_dress(&mut self, change: impl FnOnce(&mut Dress)) {
        let mut dress = match self.dress.take() {
            Some(dress) => Arc::unwrap_or_clone(dress),
            None => Dress::default(),
        };
        change(&mut dress);
        self.dress = (!dress.is_bare()).then(|| Arc::new(dress));
    }

    /// The list of the steps. A layer of a source makes it at the first
    /// call: a fold that moves or cuts steps reads it here, and a query
    /// does not.
    fn listing(&self) -> &Listing {
        match &self.steps {
            Steps::Listed(listing) => listing,
            Steps::Source(sourced) => sourced.read.get_or_init(|| {
                let mut raws = Vec::with_capacity(sourced.steps.count());
                let written = sourced
                    .steps
                    .steps(Fraction::ZERO, self.period, &mut |step| {
                        raws.push(Raw {
                            whole: TimeSpan::new(step.begin, step.end),
                            part: None,
                            value: step.value,
                            locations: step.locations,
                        });
                    });
                debug_assert!(written.is_some(), "a source reads its period");
                Listing::of(raws)
            }),
        }
    }

    /// The list of the steps, when it is made. A read of it makes none.
    fn listed(&self) -> Option<&Listing> {
        match &self.steps {
            Steps::Listed(listing) => Some(listing),
            Steps::Source(sourced) => sourced.read.get(),
        }
    }

    /// The source of the steps, when no fold made their list.
    fn source(&self) -> Option<&dyn StepSource> {
        match &self.steps {
            Steps::Source(sourced) if sourced.read.get().is_none() => Some(&*sourced.steps),
            _ => None,
        }
    }

    /// How many steps a period holds.
    fn count(&self) -> usize {
        match self.source() {
            Some(source) => source.count(),
            None => self.listing().len(),
        }
    }

    /// The steps as places, for a test.
    #[cfg(test)]
    fn places(&self) -> Vec<Place> {
        let listing = self.listing();
        (0..listing.len())
            .map(|index| listing.raw(index).place())
            .collect()
    }

    /// The whole of the one step of this layer, when it holds one, whether
    /// that step is cut, and its value. A source of more steps makes no
    /// list to say so.
    fn one_step(&self) -> Option<(TimeSpan, bool, u32)> {
        if self.source().is_some_and(|source| source.count() != 1) {
            return None;
        }
        let listing = self.listing();
        (listing.len() == 1).then(|| {
            let (whole, part) = listing.times(0);
            (whole, part.is_some(), listing.value(0))
        })
    }

    /// This layer with `values` in the place of its own, one for one.
    fn valued(mut self, values: Arc<[Value]>) -> Self {
        self.data = values.iter().all(|value| value.is_data(0));
        self.maps = values.iter().all(|value| matches!(value, Value::Object(_)));
        self.values = values;
        self.change_dress(|dress| {
            dress.named = None;
            dress.set = None;
        });
        self.values_read = OnceLock::new();
        self
    }

    /// Name each value of these layers by the control of `names`, as the
    /// map of the control over its haps gives them. No value is made now:
    /// a chain of controls after the name makes each value one time, at
    /// the first read, and a layer that no query reads makes none.
    ///
    /// A layer that names or sets its values already [takes no
    /// name](Self::takes_name).
    fn named<'a>(
        layers: impl IntoIterator<Item = &'a Layer>,
        names: &Arc<[Arc<str>]>,
    ) -> Vec<Layer> {
        // A handle of the names, which the control holds.
        let mut layers = redressed(layers, |dress| dress.named = Some(Arc::clone(names)));
        for layer in &mut layers {
            layer.maps = true;
        }
        layers
    }

    /// Whether [`Self::named`] names the values of this layer: it holds no
    /// name and no control yet.
    fn takes_name(&self) -> bool {
        self.dress().named.is_none() && self.dress().set.is_none()
    }

    /// This layer where each value takes the keys of the one value of
    /// `values`, a map of controls that fills each cycle, as a `set` of it
    /// gives them. See [`Self::set_here`].
    ///
    /// `None` for any other meeting: a step that the constant cuts, values
    /// that are not all maps of controls, another op, and a meeting that
    /// [`Meeting::in_place`] does not make.
    fn set(&self, values: &Layer, apply: &Combine, flow: LookupFlow) -> Option<Self> {
        if !(values.fills_each_cycle() && values.dress().keys.is_empty()) {
            return None;
        }
        self.set_constant(&values.value(0)?, || values.context(0), apply, flow)
    }

    /// [`Self::set`] for the one value of a constant: `control` is the
    /// value, and `context` gives its locations.
    fn set_constant(
        &self,
        control: &Value,
        context: impl FnOnce() -> Locations,
        apply: &Combine,
        flow: LookupFlow,
    ) -> Option<Self> {
        let Combine::Composed(ComposeOp::Set) = apply else {
            return None;
        };
        if !self.takes_set(control, flow) {
            return None;
        }
        let mut layer = self.with(|_| {});
        layer.set_here(control, context());
        Some(layer)
    }

    /// This layer after a rewrite of pitch, when the control `note` or `n`
    /// names its steps and no control is set on them: each step holds the
    /// note that `note` makes of its own, under the name `note`, and no
    /// map of controls is made. `map`, the rewrite of a hap, reads the
    /// first step for the keys that it gives each hap.
    ///
    /// `None` for any other layer, for a value that is no number and no
    /// text, and where `note` or `map` gives none.
    fn renoted(&self, map: &HapMapFn, note: &NoteFn) -> Option<Self> {
        let names = self
            .dress()
            .named
            .as_ref()
            .filter(|_| self.dress().set.is_none())?;
        let [name] = &names[..] else {
            return None;
        };
        if !matches!(&**name, "note" | "n") {
            return None;
        }
        let renoted = |value: &Value| match value {
            Value::F64(_) | Value::Str(_) => note(value),
            _ => None,
        };
        let notes: Vec<Value> = self.values.iter().map(renoted).collect::<Option<_>>()?;
        // The hap of the first step, as a query reads it.
        let first = crate::controls::named_value(names, self.values.first()?.clone());
        let mut read = cycle_hap(first);
        read.keys = self.dress().keys.clone();
        let keys = map(&read)?.keys;
        let mut layer = self.with(|_| {}).valued(notes.into());
        layer.change_dress(|dress| {
            dress.named = Some(crate::controls::note_names());
            dress.keys = keys;
        });
        layer.maps = true;
        Some(layer)
    }

    /// Whether each value of this layer takes `control` in place by a `set`
    /// of a constant of it. Each value is a map of data, so each meets the
    /// control as a map of data does, and no value is read to know it.
    fn takes_set(&self, control: &Value, flow: LookupFlow) -> bool {
        let set = Combine::Composed(ComposeOp::Set);
        let map = Value::Object(crate::OrderedMap::new());
        self.in_cycles
            && self.maps
            && self.data
            && !self.values.is_empty()
            && matches!(control, Value::Object(_))
            && Meeting::values_meet_in_place(&map, control, &set, flow)
    }

    /// Set `control`, a map of controls at `context` that this layer
    /// [takes](Self::takes_set), on each value. No value is made now: the
    /// map joins those that the layer holds, so a chain of such controls
    /// makes each value one time, at the first read.
    pub(crate) fn set_here(&mut self, control: &Value, context: Locations) {
        self.change_dress(|dress| Dress::set_here(dress, control, context));
        self.values_read = OnceLock::new();
        self.contexts = OnceLock::new();
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
        let holds = |value: u32| (value as usize) < self.values.len();
        self.is_in_order()
            && (self.source().is_some()
                || (0..self.count()).all(|index| holds(self.listing().value(index))))
    }

    /// Whether the period is more than zero and the steps are in the order
    /// of their haps: the part of each lies inside its whole and inside the
    /// period, begins at or after the part before it and ends at or after
    /// it, and no part is empty.
    fn is_in_order(&self) -> bool {
        // A source writes its steps in order.
        if self.source().is_some() {
            return self.period > Fraction::ZERO;
        }
        let listing = self.listing();
        if let Listing::Grid(grid) = listing
            && let Some(in_order) = grid.in_order(self.period)
        {
            return in_order;
        }
        let inside = |index: usize| {
            let (whole, part) = (listing.whole(index), listing.part(index));
            Fraction::ZERO <= whole.begin
                && whole.begin <= part.begin
                && part.begin < part.end
                && part.end <= whole.end
                && whole.end <= self.period
        };
        let ordered = (1..listing.len()).all(|index| {
            let (a, b) = (listing.part(index - 1), listing.part(index));
            a.begin <= b.begin && a.end <= b.end
        });
        self.period > Fraction::ZERO && (0..listing.len()).all(inside) && ordered
    }

    /// The locations of the hap of each step, when the dress adds some to
    /// the step's own: made at the first query. `None` when each hap holds
    /// the locations of its step.
    fn contexts(&self) -> Option<&[Locations]> {
        let dress = self.dress();
        if dress.locates_alone() {
            return None;
        }
        Some(self.contexts.get_or_init(|| {
            (0..self.count())
                .map(|index| dress.around(&self.locations_of(index)))
                .collect()
        }))
    }

    /// The locations of the hap of step `index`: those before, the step's
    /// own, then those after.
    fn context(&self, index: usize) -> Locations {
        match self.contexts() {
            Some(contexts) => contexts[index].clone(),
            None => self.locations_of(index),
        }
    }

    /// The locations of step `index`, as the step holds them.
    fn locations_of(&self, index: usize) -> Locations {
        self.listing().locations(index)
    }

    /// What makes the haps of the steps of this layer, read once for a loop.
    fn maker(&self) -> Maker<'_> {
        Maker {
            listing: self.listing(),
            values: self.reader(),
            contexts: self.contexts(),
            keys: &self.dress().keys,
        }
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
        let maker = self.maker();
        let listing = maker.listing;
        (0..listing.len())
            .map(|index| {
                Some(maker.hap(
                    index,
                    share(listing.whole(index))?,
                    share(listing.part(index))?,
                ))
            })
            .collect()
    }

    /// The values of the steps, each once, with the name of their control
    /// and the controls that a chain of constants set.
    pub(crate) fn value(&self, index: usize) -> Option<Value> {
        self.reader().get(index)
    }

    /// The values as the steps hold them, before the dress. The dress
    /// holds data only, so the purity of the layer is that of these.
    pub(crate) fn held_values(&self) -> &[Value] {
        &self.values
    }

    /// The values as the dress makes them, read once for a loop of steps.
    fn reader(&self) -> Values<'_> {
        let dress = self.dress();
        if dress.named.is_none() && dress.set.is_none() {
            return Values::Held(&self.values);
        }
        match self.values_read.get_or_init(|| self.read_values(dress)) {
            Read::Made(values) => Values::Held(values),
            Read::Shared(table) => Values::Table(table),
        }
    }

    /// [`Self::value`] of each step.
    pub(crate) fn each_value(&self) -> impl Iterator<Item = Value> + '_ {
        let values = self.reader();
        (0..self.values.len()).filter_map(move |index| values.get(index))
    }

    /// The values as `dress` makes them: one table that their maps read
    /// where that gives the same maps, else each value made on its own.
    fn read_values(&self, dress: &Dress) -> Read {
        let set = match &dress.set {
            None => Some(None),
            Some(Value::Object(set)) if !union_drops(set) => Some(Some(set)),
            Some(_) => None,
        };
        let key = match &dress.named {
            Some(names) => self
                .values
                .iter()
                .all(|value| crate::controls::names_as_one_entry(names, value))
                .then(|| Some(crate::controls::key_of(names))),
            None => Some(None),
        };
        if let (Some(set), Some(key)) = (set, key)
            && let Some(table) = Table::new(&self.values, key, set)
        {
            return Read::Shared(Arc::new(table));
        }
        Read::Made(
            self.values
                .iter()
                .map(|value| {
                    // A handle of the value, or a copy of its text.
                    let mut value = match &dress.named {
                        Some(names) => crate::controls::named_value(names, value.clone()),
                        None => value.clone(),
                    };
                    if let Some(set) = &dress.set {
                        compose_data_in_place(ComposeOp::Set, &mut value, set);
                    }
                    value
                })
                .collect(),
        )
    }

    /// Whether this layer and `other` answer the same haps, with values
    /// that `same_value` holds to be the same. The values are read as the
    /// layers hold them: the same values with the same name and the same
    /// controls set are the same, and no value is made to know it.
    pub(crate) fn same(&self, other: &Self, same_value: impl Fn(&Value, &Value) -> bool) -> bool {
        let same_dress = match (&self.dress, &other.dress) {
            (Some(a), Some(b)) if Arc::ptr_eq(a, b) => true,
            _ => self.dress().same(other.dress(), &same_value),
        };
        if !same_dress || self.period != other.period {
            return false;
        }
        let (own, others) = match (&self.steps, &other.steps) {
            (Steps::Listed(own), Steps::Listed(others)) => (own, others),
            (Steps::Source(own), Steps::Source(others)) => {
                // Two layers of one source hold the same steps, whatever was
                // read of it, and are the same when their values are.
                if std::ptr::addr_eq(Arc::as_ptr(&own.steps), Arc::as_ptr(&others.steps)) {
                    return Arc::ptr_eq(&self.values, &other.values)
                        || self.values.len() == other.values.len()
                            && self
                                .values
                                .iter()
                                .zip(&*other.values)
                                .all(|(a, b)| same_value(a, b));
                }
                // Two sources that were made apart are the same when the
                // lists that were read of both are. A read is never made to
                // know it, so a source that nobody read is not the same, and
                // a read only turns a "not the same" into a "same".
                match (own.read.get(), others.read.get()) {
                    (Some(own), Some(others)) => (own, others),
                    _ => return false,
                }
            }
            // A source and a list are not compared: a read would change
            // the answer.
            _ => return false,
        };
        fn value(layer: &Layer, value: u32) -> Option<&Value> {
            layer.values.get(value as usize)
        }
        let same_step = |index: usize| {
            own.locations(index) == others.locations(index)
                && match (
                    value(self, own.value(index)),
                    value(other, others.value(index)),
                ) {
                    (Some(a), Some(b)) => same_value(a, b),
                    _ => false,
                }
        };
        // Steps at the same ticks compare with no time made.
        let same_times = match (own, others) {
            (Listing::Grid(a), Listing::Grid(b)) => a.same_times(b),
            _ => false,
        };
        own.len() == others.len()
            && (0..own.len()).all(|index| {
                (same_times
                    || (own.whole(index), own.part(index))
                        == (others.whole(index), others.part(index)))
                    && same_step(index)
            })
    }

    /// Push the haps of this layer in `turn` of its period, for a query of
    /// `span`.
    fn push(
        &self,
        turn: &Turn,
        span: TimeSpan,
        others: usize,
        out: &mut Vec<Hap>,
    ) -> Result<(), SequenceStop> {
        // A few steps cost less to keep than to walk at every query.
        const KEPT_STEPS: usize = 8;
        if let Some(source) = self.source()
            && source.count() > KEPT_STEPS
        {
            let held = out.len();
            if let Some(pushed) = self.push_written(source, turn, span, others, out) {
                return pushed;
            }
            // The list of the steps answers a span that the source does
            // not read.
            out.truncate(held);
        }
        let owned;
        let places = match self.listing() {
            Listing::Places(listed) => &*listed.places,
            Listing::Grid(grid) => {
                if let Some(pushed) = self.push_grid(grid, turn, span, others, out) {
                    return pushed;
                }
                // A turn past the range of ticks asks the places.
                owned = grid.places();
                &owned[..]
            }
        };
        let instant = span.begin == span.end;
        // The steps are in the order of time, so the first and the last
        // bound them all: a span before or after these bounds meets none.
        let (Some(earliest), Some(latest)) = (places.first(), places.last()) else {
            return Ok(());
        };
        let before = !earliest.begins_by(turn.begin, turn.begin_key)
            && (instant || !earliest.begins_before(turn.end, turn.end_key));
        if before || latest.part().end <= turn.begin {
            return Ok(());
        }
        // The steps that the span meets: those that begin before its end,
        // or at its one time, less those that end at its begin or before.
        // The steps are in the order of their begins and of their ends, so
        // the last ones that begin before the span are those that reach it.
        let begun = places.partition_point(|place| place.begins_before(turn.begin, turn.begin_key));
        let reach = |place: &&Place| place.part().end > turn.begin;
        let first = begun - places[..begun].iter().rev().take_while(reach).count();
        let last = begun
            + places[begun..].partition_point(|place| {
                place.begins_by(turn.begin, turn.begin_key)
                    || (!instant && place.begins_before(turn.end, turn.end_key))
            });
        let met = last - first;
        if met == 0 {
            return Ok(());
        }
        // The list held `others` haps before the sequence wrote.
        if !charge_haps(out.len() - others + met) {
            return Err(SequenceStop::Query);
        }
        out.reserve(met);
        let maker = self.maker();
        for (index, place) in places.iter().enumerate().skip(first).take(met) {
            let whole = TimeSpan::new(turn.moved(place.whole.begin)?, turn.moved(place.whole.end)?);
            let visible = match &place.part {
                None => whole,
                Some(part) => TimeSpan::new(turn.moved(part.begin)?, turn.moved(part.end)?),
            };
            let part = TimeSpan::new(visible.begin.max(span.begin), visible.end.min(span.end));
            out.push(maker.hap(index, whole, part));
        }
        note_accumulated(out.len() - others);
        Ok(())
    }

    /// Push the haps of this layer in `turn`, as [`Self::push`] does, from
    /// the steps on a grid. The search compares ticks, and a time is made
    /// only for a hap. `None` for a turn that leaves the range of ticks.
    fn push_grid(
        &self,
        grid: &Grid,
        turn: &Turn,
        span: TimeSpan,
        others: usize,
        out: &mut Vec<Hap>,
    ) -> Option<Result<(), SequenceStop>> {
        let (begin_low, begin_high) = grid.ticks_at(turn.begin)?;
        let (_, end_high) = grid.ticks_at(turn.end)?;
        let instant = span.begin == span.end;
        let steps = &grid.steps;
        // A step begins before a time when its begin is under the ceiling
        // of the time, and at or before it when its begin is at most the
        // floor.
        let begun = steps.partition_point(|step| i64::from(step.part_begin) < begin_high);
        let reach = |step: &&GridStep| i64::from(step.part_end) > begin_low;
        let first = begun - steps[..begun].iter().rev().take_while(reach).count();
        let last_step = begun
            + steps[begun..].partition_point(|step| {
                let begins = i64::from(step.part_begin);
                begins <= begin_low || (!instant && begins < end_high)
            });
        let met = last_step - first;
        if met == 0 {
            return Some(Ok(()));
        }
        // The list held `others` haps before the sequence wrote.
        if !charge_haps(out.len() - others + met) {
            return Some(Err(SequenceStop::Query));
        }
        out.reserve(met);
        // The cuts are in the order of the steps: one search finds the first.
        let cuts = grid.cuts();
        let mut cuts = &cuts[cuts.partition_point(|cut| (cut.index as usize) < first)..];
        // A turn that begins on a whole cycle moves a tick by whole cycles of
        // ticks, so a time is made one time, reduced, from its tick.
        let offset = turn
            .whole_cycles
            .and_then(|cycles| i64::try_from(cycles).ok())
            .and_then(|cycles| cycles.checked_mul(i64::from(grid.ticks.get())));
        // The tick of the time made last, and the time: the end of a step is
        // the begin of the next, which most steps are.
        let mut last: Option<(u32, Fraction)> = None;
        let mut at = |tick: u32| -> Result<Fraction, SequenceStop> {
            if let Some((made, time)) = last
                && made == tick
            {
                return Ok(time);
            }
            let time = match offset.and_then(|offset| offset.checked_add(i64::from(tick))) {
                Some(ticks) => grid.made_small(ticks),
                None => turn.moved(grid.time(tick))?,
            };
            last = Some((tick, time));
            Ok(time)
        };
        let maker = self.maker();
        let pushed = (first..last_step).try_for_each(|index| {
            let step = &steps[index];
            let visible = TimeSpan::new(at(step.part_begin)?, at(step.part_end)?);
            let whole = match cuts.split_first() {
                Some((cut, rest)) if cut.index as usize == index => {
                    cuts = rest;
                    TimeSpan::new(at(cut.begin)?, at(cut.end)?)
                }
                _ => visible,
            };
            let part = TimeSpan::new(visible.begin.max(span.begin), visible.end.min(span.end));
            out.push(maker.make(index, step.value, whole, part));
            Ok(())
        });
        if pushed.is_ok() {
            note_accumulated(out.len() - others);
        }
        Some(pushed)
    }

    /// Push the haps of the steps that `source` writes in `turn` of the
    /// period, for a query of `span`. `None` for a span that the source
    /// does not read.
    fn push_written(
        &self,
        source: &dyn StepSource,
        turn: &Turn,
        span: TimeSpan,
        others: usize,
        out: &mut Vec<Hap>,
    ) -> Option<Result<(), SequenceStop>> {
        let mut stop = None;
        let values = self.reader();
        let dress = self.dress();
        let written = source.steps(turn.begin, turn.end, &mut |step| {
            let times = turn
                .moved(step.begin)
                .and_then(|begin| Ok((begin, turn.moved(step.end)?)));
            let (begin, end) = match times {
                Ok(times) if stop.is_none() => times,
                Ok(_) => return,
                Err(range) => return stop = Some(range),
            };
            let whole = TimeSpan::new(begin, end);
            let part = TimeSpan::new(begin.max(span.begin), end.min(span.end));
            let value = values
                .get(step.value as usize)
                .expect("a step holds a value of its layer");
            let mut hap = Hap::new(Some(whole), part, value);
            hap.context = match dress.locates_alone() {
                true => step.locations,
                false => dress.around(&step.locations),
            };
            hap.keys = dress.keys.clone();
            out.push(hap);
        });
        if let Some(stop) = stop {
            return Some(Err(stop));
        }
        written?;
        // The budget reads the steps of a turn after they are written.
        if !charge_haps(out.len() - others) {
            return Some(Err(SequenceStop::Query));
        }
        note_accumulated(out.len() - others);
        Some(Ok(()))
    }

    /// Whether this layer is one step that fills each cycle, as a constant
    /// is.
    fn fills_each_cycle(&self) -> bool {
        let cycle = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
        self.period == Fraction::ONE
            && self
                .one_step()
                .is_some_and(|(whole, cut, _)| whole == cycle && !cut)
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
            Side::Functions => ContextKeys::merge(&self.dress().keys, &values.dress().keys),
            Side::Values => ContextKeys::merge(&values.dress().keys, &self.dress().keys),
        };
        // A step that lies in one cycle meets a value that fills the cycle
        // over the whole part: the places stay, and the locations of the
        // value go beside those of each step.
        if self.in_cycles && values.fills_each_cycle() {
            let pairs = (0..u32::try_from(self.values.len()).ok()?)
                .map(|value| (value, 0))
                .collect();
            let met = self.with(|layer| {
                layer.change_dress(|dress| {
                    match side {
                        Side::Functions => {
                            let mut before = values.context(0);
                            before.append(&dress.before);
                            dress.before = before;
                        }
                        Side::Values => dress.after.append(&values.context(0)),
                    }
                    dress.keys = keys;
                });
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
        // Room for a step that meets one value in each turn.
        let room = usize::try_from(turns).ok()?.saturating_mul(self.count());
        let mut raws = Vec::with_capacity(room.min(MAX_FOLDED_STEPS));
        let (own, others) = (self.listing(), values.listing());
        // The parts of `values` are searched and read for each step, so a
        // grid makes their times one time.
        let other_parts: Vec<TimeSpan> = (0..others.len()).map(|at| others.part(at)).collect();
        for turn in 0..turns {
            let start = self.period.checked_mul(Fraction::int(turn))?;
            for index in 0..own.len() {
                let (own_whole, own_part) = own.times(index);
                let whole = moved(own_whole, start)?;
                let part = match own_part {
                    Some(part) => moved(part, start)?,
                    None => whole,
                };
                let (own_value, own_locations) = (own.value(index), own.locations(index));
                // The locations of each hap of this step. Those after stay
                // with the layer of the function side, which they end.
                let mut around = self.dress().before.clone();
                around.append(&own_locations);
                if side == Side::Values {
                    around.append(&self.dress().after);
                }
                // The turns of `values` that the part meets, in order.
                let turns_met = part.begin.checked_div_floor(values.period)?
                    ..part.end.checked_div_ceil(values.period)?;
                for at in turns_met {
                    let value_start = values.period.checked_mul(Fraction::int(at))?;
                    // The part, from the start of the turn, and the steps
                    // of the turn that it meets: they are in the order of
                    // time.
                    let (begin, end) = (
                        part.begin.checked_sub(value_start)?,
                        part.end.checked_sub(value_start)?,
                    );
                    let first = other_parts.partition_point(|part| part.end <= begin);
                    let meeting =
                        (first..other_parts.len()).take_while(|at| other_parts[*at].begin < end);
                    for value_index in meeting {
                        let value_part = moved(other_parts[value_index], value_start)?;
                        let piece = TimeSpan::new(
                            part.begin.max(value_part.begin),
                            part.end.min(value_part.end),
                        );
                        if raws.len() == MAX_FOLDED_STEPS {
                            return None;
                        }
                        // The pairs are few: one for each value that a
                        // value meets.
                        let pair = (own_value, others.value(value_index));
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
                                locations.append(&around);
                                locations
                            }
                            Side::Values => {
                                let mut locations = around.clone();
                                locations.append(&values.context(value_index));
                                locations
                            }
                        };
                        raws.push(Raw {
                            whole,
                            part: (piece != whole).then_some(piece),
                            value,
                            locations,
                        });
                    }
                }
            }
        }
        let mut met = Layer {
            in_cycles: lie_in_spans(period, raws.iter().map(Raw::part)),
            period,
            steps: Steps::Listed(Listing::of(raws)),
            contexts: OnceLock::new(),
            ..self.clone()
        };
        // The locations of the steps are in their places now, but those
        // after the steps of the function side, which end each hap.
        met.change_dress(|dress| {
            dress.before = Locations::new();
            if side == Side::Values {
                dress.after = Locations::new();
            }
            dress.keys = keys;
        });
        met.is_in_order().then_some((met, pairs))
    }
}

/// Whether the part of each of `places` lies in one cycle, in every turn of
/// a period of `period` cycles.
///
/// A period of `p/q` cycles begins its turns at each multiple of `1/q` of
/// a cycle, in some order. So a part lies in one cycle in every turn when
/// it lies in one of those `q` parts of a cycle: `q` times the part lies
/// in one cycle. Whole numbers compare, with no fraction made. `false`
/// also for a time that leaves their range: a cut then settles it.
#[cfg(test)]
fn lie_in_cycles(period: Fraction, places: &[Place]) -> bool {
    lie_in_spans(period, places.iter().map(Place::part))
}

/// [`lie_in_cycles`] for the parts of the steps.
fn lie_in_spans(period: Fraction, parts: impl IntoIterator<Item = TimeSpan>) -> bool {
    let turns = period.denom();
    // The whole cycles in `numer / denom`, with a machine-word division
    // for terms of a machine word.
    let cycles = |numer: i128, denom: i128| match (i64::try_from(numer), i64::try_from(denom)) {
        (Ok(numer), Ok(denom)) => i128::from(numer.div_euclid(denom)),
        _ => numer.div_euclid(denom),
    };
    parts.into_iter().all(|part| {
        let scaled = |time: Fraction| time.numer().checked_mul(turns);
        let (Some(begins), Some(ends)) = (scaled(part.begin), scaled(part.end)) else {
            return false;
        };
        // The end of the cycle that the part begins in, `turns` times.
        let cycle_end = cycles(begins, part.begin.denom()).checked_add(1);
        let bound = cycle_end.and_then(|end| end.checked_mul(part.end.denom()));
        bound.is_some_and(|bound| ends <= bound)
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

/// A [`sequence`] of layers as their maker gives them. `None` as
/// [`sequence`] says, and for a period that is not above zero.
pub fn sequence_of(layers: Vec<GivenLayer>) -> Option<Pattern> {
    let layers = layers.into_iter().map(|layer| match layer {
        GivenLayer::Listed(layer) => Layer::of(layer),
        GivenLayer::Sourced(layer) => Some(Layer::of_source(layer)),
    });
    node(layers.collect::<Option<Vec<_>>>()?)
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
fn node_of_sound(mut layers: Vec<Layer>) -> Pattern {
    layers.retain(|layer| layer.count() > 0);
    share_dresses(&mut layers);
    let layers: Arc<[Layer]> = layers.into();
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
    /// The [keys](time_key) of `begin` and of `end`.
    begin_key: f32,
    end_key: f32,
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

    /// The turn at `index`, kept for the layers that read it next.
    fn turn(&mut self, index: i128) -> Result<&Turn, SequenceStop> {
        if self.read.as_ref().is_some_and(|turn| turn.index == index) {
            return Ok(self.read.as_ref().expect("a turn that was just found"));
        }
        let in_range = |time: Option<Fraction>| time.ok_or(SequenceStop::Range);
        let start = match self.period == Fraction::ONE {
            true => Fraction::int(index),
            false => in_range(self.period.checked_mul(Fraction::int(index)))?,
        };
        let whole_cycles = (start.denom() == 1).then(|| start.numer());
        // A time from a start of whole cycles, with no division.
        let from_start = |time: Fraction| match whole_cycles.and_then(i128::checked_neg) {
            Some(back) => time.checked_add_int(back),
            None => time.checked_sub(start),
        };
        let begin = in_range(from_start(self.span.begin))?;
        let end = in_range(from_start(self.span.end))?;
        let turn = Turn {
            index,
            start,
            whole_cycles,
            begin,
            end,
            begin_key: time_key(begin),
            end_key: time_key(end),
        };
        Ok(self.read.insert(turn))
    }
}

/// Push the haps of `layers`, which share a period, in `span`: each layer
/// in turn, as a stack of them answers.
fn query_layers(
    layers: &[Layer],
    span: TimeSpan,
    from: usize,
    out: &mut Vec<Hap>,
) -> Result<(), SequenceStop> {
    let Some(first) = layers.first() else {
        return Ok(());
    };
    let mut turns = Turns::of(first.period, span)?;
    if turns.indices.end.checked_sub(turns.indices.start) == Some(1) {
        let turn = turns.turn(turns.indices.start)?;
        for layer in layers {
            layer.push(turn, span, from, out)?;
        }
        return Ok(());
    }
    for layer in layers {
        for index in turns.indices.clone() {
            // The query of the node asked before the first turn.
            if index != turns.indices.start && query_stop_requested() {
                return Err(SequenceStop::Query);
            }
            layer.push(turns.turn(index)?, span, from, out)?;
        }
    }
    Ok(())
}

/// The haps of `layers` in `state`'s span, after those of `out`. See
/// [`sequence`].
pub(crate) fn query_sequence_onto(layers: &[Layer], state: &State, out: &mut Vec<Hap>) {
    /// The most room that an answer takes before its first hap.
    const MOST_ROOM: usize = 64;
    // A span of a cycle or more meets each step of most layers. A shorter
    // one meets few, and takes room as its haps come.
    let span = state.span;
    let long = span
        .begin
        .checked_add_int(1)
        .is_some_and(|after_cycle| span.end >= after_cycle);
    if long {
        let steps: usize = layers.iter().map(Layer::count).sum();
        out.reserve(steps.min(MOST_ROOM));
    }
    let from = out.len();
    for layers in layers.chunk_by(|a, b| a.period == b.period) {
        match query_layers(layers, span, from, out) {
            Ok(()) => {}
            Err(stop) => {
                if matches!(stop, SequenceStop::Range) {
                    refuse(QueryLimit::NativeFraction {
                        operation: "sequence",
                    });
                }
                out.truncate(from);
                return;
            }
        }
    }
}

/// Whether moving these layers by `by` leaves every step in place.
pub(crate) fn repeats_at(layers: &[Layer], by: Fraction, span: TimeSpan) -> bool {
    // Both readings must fit, including each whole before the query cuts it.
    let small =
        |time: Fraction| i32::try_from(time.numer()).is_ok() && i32::try_from(time.denom()).is_ok();
    if by.denom() != 1 || !small(by) || !small(span.begin) || !small(span.end) {
        return false;
    }
    layers.iter().all(|layer| {
        let period = layer.period;
        // An integer multiple of the reduced numerator is a whole number of periods.
        layer.listed().is_some_and(Listing::small_denominators)
            && small(period)
            && (period.numer() == 1 || (by.numer() as i32) % (period.numer() as i32) == 0)
    })
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
pub(crate) fn constant_value(layers: &[Layer]) -> Option<Cow<'_, Value>> {
    let cycle = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
    match layers {
        // A reader makes a hap of the value alone, so keys make no constant.
        [layer] if layer.period == Fraction::ONE && layer.dress().keys.is_empty() => {
            match layer.one_step() {
                Some((whole, false, value)) if whole == cycle => layer
                    .value(value as usize)
                    .filter(|value| value.is_data(0))
                    .map(Cow::Owned),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The steps in a cycle of `layers` and their value, when the layers are
/// one data value that fills each of `steps` equal steps and holds nothing
/// more, as the pulse of a `segment` is. Nothing is made for a layer that
/// is not one, so that asking costs no memory.
pub(crate) fn pulse(layers: &[Layer]) -> Option<(i128, &Value)> {
    let [layer] = layers else {
        return None;
    };
    if layer.period.numer() != 1 || !layer.dress().is_bare() {
        return None;
    }
    let first = TimeSpan::new(Fraction::ZERO, layer.period);
    let (whole, cut, value) = layer.one_step()?;
    if whole != first || cut || !layer.locations_of(0).is_empty() {
        return None;
    }
    // A bare dress gives each value as the step holds it.
    let value = layer.values.get(value as usize)?;
    value.is_data(0).then_some((layer.period.denom(), value))
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

/// Whether a test builds each node that a fold stands for now. See
/// [`unfolded`].
pub(crate) fn builds_each_node() -> bool {
    #[cfg(test)]
    return UNFOLDED.get();
    #[cfg(not(test))]
    false
}

/// Whether an application on `pattern` may fold into its steps: it is a
/// sequence or a constant.
pub(crate) fn holds_steps(pattern: &Pattern) -> bool {
    folded(pattern).is_some() || constant(pattern).is_some()
}

/// Whether a constructor may fold `pattern`: its handle is not marked
/// volatile, and the runtime that it binds, if any, changes no answer.
pub(crate) fn folds(pattern: &Pattern) -> bool {
    if builds_each_node() {
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

/// The value that `values` answers in each cycle on one hap that fills the
/// cycle, when the graph says it with no query: a `pure` value, at its
/// locations, or a sequence of one value that fills each cycle.
fn constant_value_of(values: &Pattern) -> Option<Cow<'_, Value>> {
    if !folds(values) {
        return None;
    }
    match values.node_kind() {
        Node::Pure(value) if value.is_data(0) => Some(Cow::Borrowed(value)),
        Node::AddContext(inner, _) => constant_value_of(inner),
        Node::Sequence(layers) => constant_value(layers),
        _ => None,
    }
}

/// The locations of the hap of `values`, a [constant](constant_value_of).
fn constant_locations(values: &Pattern) -> Locations {
    match values.node_kind() {
        Node::AddContext(inner, locations) => {
            let mut context = constant_locations(inner);
            context.extend_from_slice(locations);
            context
        }
        Node::Sequence(layers) => layers[0].context(0),
        _ => Locations::new(),
    }
}

/// The hap that `values` answers in each cycle, as of cycle zero, when the
/// graph says it with no query. See [`constant_value_of`].
pub(crate) fn constant(values: &Pattern) -> Option<Hap> {
    // A handle of the value, for the hap to hold.
    let mut hap = cycle_hap(constant_value_of(values)?.into_owned());
    hap.context = constant_locations(values);
    Some(hap)
}

/// Whether `values` is a constant of a plain data value, by its kinds
/// alone. See [`constant_value_at`].
pub(crate) fn is_constant_value(values: &Pattern) -> bool {
    constant_value_of(values).is_some()
}

/// The layers that `pattern` answers as, when it is a sequence or a
/// constant: a constant is one step that fills each cycle.
fn layers(pattern: &Pattern) -> Option<std::borrow::Cow<'_, [Layer]>> {
    if let Some(layers) = folded(pattern) {
        return Some(std::borrow::Cow::Borrowed(layers));
    }
    let hap = constant(pattern)?;
    let raw = Raw {
        whole: hap.whole?,
        part: None,
        value: 0,
        locations: hap.context,
    };
    Some(std::borrow::Cow::Owned(vec![Layer::new(
        Fraction::ONE,
        vec![raw],
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
                let values: Option<Vec<Value>> =
                    layer.each_value().map(|value| change(&value)).collect();
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
    let layers = redressed(folded(pattern)?, |dress| {
        dress.after.extend_from_slice(locations)
    });
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
        layer.dress().keys.is_empty()
            && (!whole_cycles
                || (whole(layer.period)
                    && (0..layer.count()).all(|index| whole(layer.listing().whole(index).begin))))
    });
    if !plain {
        return None;
    }
    if locations.is_empty() {
        return Some(selector.clone());
    }
    let layers = redressed(layers, |dress| {
        let mut before = locations.clone();
        before.append(&dress.before);
        dress.before = before;
    });
    Some(node_of_sound(layers).with_steps_of(selector))
}

/// Whether `pattern` is a sequence whose haps each hold a value and nothing
/// more: no location and no key of the context.
pub(crate) fn is_bare(pattern: &Pattern) -> bool {
    folded(pattern).is_some_and(|layers| {
        layers.iter().all(|layer| {
            layer.dress().keys.is_empty()
                && layer.dress().locates_alone()
                && match layer.listing() {
                    Listing::Grid(grid) => grid.steps.iter().all(|step| step.loc.0 == 0),
                    listing => (0..listing.len()).all(|index| listing.locations(index).is_empty()),
                }
        })
    })
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
    // A handle of the value, for the caller to hold.
    let value = constant_value_of(values)?.into_owned();
    Some((value, constant_locations(values)))
}

/// `pattern` with each value named by the control of `names`, as
/// [`Pattern::map_control`] names it, when `pattern` is a sequence or a
/// constant that holds no name and no control yet. See [`Layer::named`].
pub(crate) fn named(pattern: &Pattern, names: &Arc<[Arc<str>]>) -> Option<Pattern> {
    let layers = layers(pattern)?;
    if !layers.iter().all(Layer::takes_name) {
        return None;
    }
    // Each layer of a node has steps, so each one stays. The layers are
    // made where the new node holds them.
    let named: Arc<[Layer]> = Layer::named(layers.iter(), names).into();
    Some(Pattern::of(Node::Sequence(named)))
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

/// What a rewrite of pitch makes of a note or a step that a hap holds alone.
pub(crate) type NoteFn = Box<dyn Fn(&Value) -> Option<Value> + Send + Sync>;

/// `pattern` with each hap changed by `map`, a rewrite of pitch, as
/// [`mapped_haps`] gives it, when the control `note` or `n` names the steps
/// of `pattern`: `note` gives the new note of each step, and no map of
/// controls is made. See [`Layer::renoted`].
pub(crate) fn renoted(pattern: &Pattern, map: &HapMapFn, note: &NoteFn) -> Option<Pattern> {
    let layers = folded(pattern)?;
    without_query_signals(|| {
        let renoted = layers.iter().map(|layer| layer.renoted(map, note));
        Some(node_of_sound(renoted.collect::<Option<Vec<_>>>()?))
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
                // One hap for each value in turn: the map reads it and
                // makes its own.
                let mut read = cycle_hap(Value::Undefined);
                read.keys = layer.dress().keys.clone();
                let values = layer
                    .each_value()
                    .map(|value| {
                        // A value of the script is read through its host,
                        // at each query.
                        if !value.is_data(0) {
                            return None;
                        }
                        // A handle of the value, which the layer holds.
                        read.value = value;
                        let hap = map(&read)?;
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
                    layer.change_dress(|dress| dress.keys = keys);
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
        Combine::Composed(_) | Combine::Sets(_) => true,
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
                            &layer.value(function as usize)?,
                            &values.value(value as usize)?,
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

/// Whether each step of the sequence `functions` takes the data value
/// `control` in place by a `set` of a constant of it, as [`applied`] folds
/// that `set`. See [`Layer::takes_set`].
pub(crate) fn takes_set(functions: &Pattern, control: &Value) -> bool {
    folded(functions).is_some_and(|layers| {
        holds_data(layers)
            && control.is_data(0)
            && layers
                .iter()
                .all(|layer| layer.takes_set(control, LookupFlow::Set))
    })
}

/// Set `control` at `context` on each step of `layers`, which
/// [take it](takes_set). The layers are written where no other sequence
/// holds them, and a copy of them otherwise.
pub(crate) fn set_here(layers: &mut Arc<[Layer]>, control: &Value, context: Locations) {
    if Arc::get_mut(layers).is_none() {
        *layers = layers.iter().map(|layer| layer.with(|_| {})).collect();
    }
    let layers = Arc::get_mut(layers).expect("layers that no other sequence holds");
    // Layers that wore one dress wear one after.
    let mut last: Option<(*const Dress, Arc<Dress>)> = None;
    for layer in layers {
        let from = layer.dress.as_ref().map_or(std::ptr::null(), Arc::as_ptr);
        let dress = match &last {
            Some((seen, dress)) if *seen == from => Arc::clone(dress),
            _ => {
                let mut dress = layer.dress().clone();
                // The locations of the control, for each layer.
                dress.set_here(control, context.clone());
                let dress = Arc::new(dress);
                last = Some((from, Arc::clone(&dress)));
                dress
            }
        };
        layer.dress = Some(dress);
        layer.values_read = OnceLock::new();
        layer.contexts = OnceLock::new();
    }
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
                        let hf = cycle_hap(functions.value(function as usize)?);
                        let hv = cycle_hap(layer.value(value as usize)?);
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
                let kept: Vec<bool> = layer.each_value().map(|value| keep(&value)).collect();
                if kept.iter().all(|kept| *kept) {
                    return layer.clone();
                }
                let listing = layer.listing();
                let raws: Vec<Raw> = (0..listing.len())
                    .filter(|index| {
                        kept.get(listing.value(*index) as usize)
                            .copied()
                            .unwrap_or(false)
                    })
                    .map(|index| listing.raw(index))
                    .collect();
                layer.with(|layer| {
                    layer.in_cycles = lie_in_spans(layer.period, raws.iter().map(Raw::part));
                    layer.steps = Steps::Listed(Listing::of(raws));
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
            let listing = layer.listing();
            let raws = (0..listing.len())
                .map(|index| {
                    let (whole, part) = listing.times(index);
                    let part = match part {
                        Some(part) => Some(faster(part)?),
                        None => None,
                    };
                    Some(Raw {
                        whole: faster(whole)?,
                        part,
                        value: listing.value(index),
                        locations: listing.locations(index),
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            Some(layer.with(|layer| {
                layer.in_cycles = lie_in_spans(period, raws.iter().map(Raw::part));
                layer.period = period;
                layer.steps = Steps::Listed(Listing::of(raws));
            }))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(node_of_sound(layers))
}

/// `pattern` with each step `count` times in the time of the step, one
/// after the other: what a `ply` makes of a sequence of whole steps of
/// data, with no join at each query. `None` for any other pattern, for a
/// step that is cut or has no width, for more than [`MAX_FOLDED_STEPS`]
/// steps in a layer, and for a step that begins before the step before it
/// ends: the join answers each of the one step and then each of the
/// other, which is not the order of time that a layer has.
pub(crate) fn plied(pattern: &Pattern, count: usize) -> Option<Pattern> {
    let layers = layers(pattern)?;
    let share = Fraction::int(i128::try_from(count).ok().filter(|count| *count > 0)?);
    let layers = layers
        .iter()
        .map(|layer| {
            let steps = layer.listing();
            let listed = steps.len().checked_mul(count)?;
            if !layer.data || listed > MAX_FOLDED_STEPS {
                return None;
            }
            let mut raws = Vec::with_capacity(listed);
            let mut before = None;
            for index in 0..steps.len() {
                let whole = steps.whole(index);
                let (begin, end) = (whole.begin, whole.end);
                let sounds_on = count > 1 && before.is_some_and(|before| begin < before);
                if steps.is_cut(index) || begin >= end || sounds_on {
                    return None;
                }
                before = Some(end);
                let width = end.checked_sub(begin)?.checked_div(share)?;
                let mut from = begin;
                for at in 1..=count {
                    // The last one ends where the step ends.
                    let to = match at == count {
                        true => end,
                        false => {
                            let at = Fraction::int(i128::try_from(at).ok()?);
                            begin.checked_add(width.checked_mul(at)?)?
                        }
                    };
                    raws.push(Raw {
                        whole: TimeSpan::new(from, to),
                        part: None,
                        value: steps.value(index),
                        locations: steps.locations(index),
                    });
                    from = to;
                }
            }
            Some(layer.with(|layer| {
                layer.in_cycles = lie_in_spans(layer.period, raws.iter().map(Raw::part));
                layer.steps = Steps::Listed(Listing::of(raws));
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

    /// A name, and the pattern that a test builds on the layers of a
    /// sequence.
    type Base<'a> = (&'a str, &'a dyn Fn(&[SequenceLayer]) -> Pattern);

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
                    "{hap:?} {:?} {} {} {:?} {}",
                    hap.live_controls,
                    hap.slider_binding,
                    hap.ui_visuals,
                    hap.keys,
                    hap.pick_lookup.is_some()
                )
            })
            .collect()
    }

    fn assert_same_answers(name: &str, one_node: &Pattern, nodes: &Pattern) {
        assert_eq!(one_node.steps(), nodes.steps(), "the steps of {name}");
        assert_same_haps(name, one_node, nodes);
    }

    /// A walked handle answers from the layers, and any other handle from
    /// their tables.
    fn assert_same_haps(name: &str, one_node: &Pattern, nodes: &Pattern) {
        for (walk, one_node) in [(false, one_node.clone()), (true, one_node.mark_walked())] {
            for (begin, end) in spans() {
                assert_eq!(
                    answers(&one_node, begin, end),
                    answers(nodes, begin, end),
                    "{name} over {begin:?}..{end:?}, walked: {walk}"
                );
            }
        }
    }

    /// Sequences of numbers: steps, a rest, a chord, a slow layer with a
    /// step of three cycles, a layer of one cycle and a half, a value that
    /// fills each cycle, and layers with time before and after their steps.
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
            (
                "layers with time around their steps",
                vec![
                    layer(
                        (1, 1),
                        vec![step((1, 2), (5, 8), 12.0), step((5, 8), (7, 8), 13.0)],
                    ),
                    layer((2, 1), vec![step((1, 8), (1, 4), 14.0)]),
                    layer((2, 1), vec![step((3, 4), (7, 8), 15.0)]),
                ],
            ),
        ]
    }

    /// A step is found by the float of where it begins, and by the time
    /// itself where two floats are the same and where no float orders a
    /// time.
    #[test]
    fn a_place_orders_by_its_time_where_its_float_does_not() {
        let place = |begin: Fraction| {
            let whole = TimeSpan::new(begin, begin + Fraction::ONE);
            Place::at(whole, None, 0, Locations::new())
        };
        let third = Fraction::new(1, 3);
        // The same float of 32 bits as a third, and a time past the
        // numbers that a float holds.
        let near = third + Fraction::new(1, 1 << 40);
        let fine = Fraction::new(1, (1 << 60) + 1);
        assert_eq!(time_key(third), time_key(near));
        assert!(time_key(fine).is_nan());
        let times = [Fraction::ZERO, fine, third, near, Fraction::ONE];
        for begin in times {
            for time in times {
                let (step, key) = (place(begin), time_key(time));
                assert_eq!(
                    step.begins_before(time, key),
                    begin < time,
                    "{begin:?} {time:?}"
                );
                assert_eq!(
                    step.begins_by(time, key),
                    begin <= time,
                    "{begin:?} {time:?}"
                );
            }
        }
    }

    fn built(layers: &[SequenceLayer]) -> Pattern {
        sequence(layers.to_vec()).expect("sound layers")
    }

    /// The layers that a fold dresses alike wear one dress, and layers of
    /// two dresses keep two: a name, a set of a control and locations
    /// make one dress for all the layers of a sequence.
    #[test]
    fn the_layers_that_a_fold_dresses_alike_share_one_dress() {
        let dresses = |pattern: &Pattern| -> usize {
            let Node::Sequence(layers) = pattern.node_kind() else {
                panic!("a sequence");
            };
            let mut seen: Vec<*const Dress> = Vec::new();
            for layer in layers.iter() {
                let at = layer.dress.as_ref().map_or(std::ptr::null(), Arc::as_ptr);
                if !seen.contains(&at) {
                    seen.push(at);
                }
            }
            seen.len()
        };
        let (_, layers) = sequences().swap_remove(5);
        let names: Arc<[Arc<str>]> = Arc::from(vec![Arc::<str>::from("note")]);
        let pattern = named(&built(&layers), &names).expect("a named sequence");
        assert_eq!(dresses(&pattern), 1);
        let control = Value::object([("gain".to_string(), Value::F64(0.5))]);
        let set = compose(&pattern, &pure(control), ComposeOp::Set, Alignment::In);
        assert!(matches!(set.node_kind(), Node::Sequence(_)));
        assert_eq!(dresses(&set), 1);
        let located = located(&set, &[(3, 5)]).expect("a located sequence");
        assert_eq!(dresses(&located), 1);
        // Two sequences of two dresses, one after the other in a stack.
        let other = named(&built(&sequences()[0].1), &names).expect("a named sequence");
        let both = stack(vec![located, other]);
        assert_eq!(dresses(&both), 2);
    }

    /// A ply of a sequence by a whole count is one sequence, which answers
    /// as the join of a pattern for each step. A count that is not whole,
    /// a step that is cut, and more steps than a fold lists keep the join.
    #[test]
    fn a_ply_of_a_sequence_answers_as_the_join_of_each_step() {
        use crate::combinators::ply;
        let is_sequence = |pattern: &Pattern| matches!(pattern.node_kind(), Node::Sequence(_));
        let stepped = |layers: &[SequenceLayer]| built(layers).with_steps(Some(frac((4, 1))));
        for (name, layers) in sequences() {
            // The steps of a chord sound at one time.
            let chord = name == "a chord and fast steps";
            for count in [1, 2, 3, 7] {
                let plied = || ply(&stepped(&layers), frac((count, 1)));
                let folded = plied();
                let name = format!("{name}, each step {count} times");
                assert_eq!(is_sequence(&folded), !chord || count == 1, "{name}");
                assert_same_answers(&name, &folded, &unfolded(plied));
            }
            assert!(!is_sequence(&ply(&built(&layers), frac((3, 2)))), "{name}");
            assert!(
                !is_sequence(&ply(&built(&layers), frac((300, 1)))),
                "{name}"
            );
        }
        let whole = (frac((0, 1)), frac((3, 4)));
        let cut = step((0, 1), (3, 4), 1.0).placed(whole, (frac((1, 4)), frac((1, 2))));
        let cut = built(&[layer((2, 1), vec![cut])]);
        assert!(!is_sequence(&ply(&cut, frac((2, 1)))));
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
        assert_same_haps("a concatenation", &one_node, &nodes);
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
        assert_same_haps("layers", &one_node, &nodes);
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
            let control = |pattern: Pattern| pattern.map_control(&ControlSpec::new(["n"]), false);
            let folded = control(built(&layers));
            assert!(matches!(folded.node_kind(), Node::Sequence(_)), "{name}");
            assert_same_answers(name, &folded, &unfolded(|| control(built(&layers))));
            // Steps that hold a control already, with a constant set on
            // them: the name takes each map with that constant in it.
            let again = |pattern: Pattern| {
                let named = pattern.map_control(&ControlSpec::new(["n"]), true);
                let quiet = ControlSpec::new(["gain"]).apply_to(named, Some(located_value(0.25)));
                quiet.map_control(&ControlSpec::new(["note"]), false)
            };
            assert_same_answers(
                name,
                &again(built(&layers)),
                &unfolded(|| again(built(&layers))),
            );
        }
    }

    #[test]
    fn steps_that_another_control_names_are_another_construction() {
        let (_, layers) = &sequences()[0];
        let named = |name: &str| built(layers).map_control(&ControlSpec::new([name]), false);
        assert!(named("n").same_construction(&named("n")));
        assert!(!named("n").same_construction(&named("note")));
    }

    /// A scale over steps that `note` or `n` names gives each step its
    /// note where it is, and answers as the scale of each hap: steps,
    /// notes, steps with accidentals, a step that is none, and a name that
    /// is no scale.
    #[test]
    fn a_scale_of_named_steps_answers_as_the_scale_of_each_hap() {
        let text = |text: &str| Value::Str(text.into());
        let melodies = [
            (
                "steps",
                vec![
                    Value::F64(0.0),
                    Value::F64(2.0),
                    Value::F64(-3.0),
                    Value::F64(7.5),
                ],
            ),
            (
                "notes",
                vec![text("c3"), text("eb4"), text("f#2"), text("a")],
            ),
            (
                "steps with accidentals",
                vec![text("0"), text("2#"), text("4b"), text("-1")],
            ),
            (
                "a step that is none",
                vec![Value::F64(1.0), text("x"), Value::Bool(true)],
            ),
            // A hap of no note stays as it is, with no tag of the scale.
            ("no note", vec![Value::F64(1.0), Value::Null]),
            (
                "a map of a value",
                vec![Value::object([("value".into(), Value::F64(2.0))])],
            ),
        ];
        for (name, values) in melodies {
            let steps = || {
                let count = values.len() as i128;
                let steps = values.iter().zip(0..).map(|(value, at)| {
                    let (begin, end) = (frac((at, count)), frac((at + 1, count)));
                    SequenceStep::new(begin, end, value.clone(), Locations::new())
                });
                built(&[layer((1, 1), steps.collect())])
            };
            for control in ["note", "n", "s"] {
                for scale in ["C:minor", "F#:major", "no such scale"] {
                    let scaled = || {
                        let named = steps().map_control(&ControlSpec::new([control]), false);
                        crate::combinators::scale(&named, text(scale))
                    };
                    let name = format!("{name} under {control} in {scale}");
                    assert_same_haps(&name, &scaled(), &unfolded(scaled));
                    // A control that is set before the scale stays.
                    let quiet = || {
                        let named = steps().map_control(&ControlSpec::new([control]), false);
                        let gain = ControlSpec::new(["gain"]);
                        let quiet = gain.apply_to(named, Some(located_value(0.25)));
                        crate::combinators::scale(&quiet, text(scale))
                    };
                    assert_same_haps(&name, &quiet(), &unfolded(quiet));
                }
            }
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
                .map_control(&ControlSpec::new([key]), false)
        };
        for (name, layers) in sequences() {
            // Numbers under arithmetic, and control objects that a
            // constant sets, adds to and sets again.
            let chains: [(&str, &dyn Fn(Pattern) -> Pattern); 3] = [
                // A chain of constants that each set a control: a key that
                // is new, and a key that an earlier constant set.
                ("set controls", &|pattern: Pattern| {
                    let notes = pattern.map_control(&ControlSpec::new(["n"]), false);
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
                    let notes = pattern.map_control(&ControlSpec::new(["n"]), false);
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
            |pattern: Pattern, key: &str| pattern.map_control(&ControlSpec::new([key]), false);
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
        // The steps as they are, and with a number that fills each cycle
        // added: a step of several cycles then has a part in each of them.
        let receiver = |layers: &[SequenceLayer], cut: bool| {
            let steps = built(layers);
            if !cut {
                return steps;
            }
            let each_cycle = built(&[layer((1, 1), vec![step((0, 1), (1, 1), 16.0)])]);
            compose(&steps, &each_cycle, ComposeOp::Add, Alignment::In)
        };
        let mut folds = Vec::new();
        for (name, layers) in sequences() {
            for (structure_name, structure) in structures() {
                // `struct` and `mask`, and numbers that the values add to.
                let chains: [(&str, ComposeOp, Alignment); 3] = [
                    ("struct", ComposeOp::KeepIf, Alignment::Out),
                    ("mask", ComposeOp::KeepIf, Alignment::In),
                    ("add out", ComposeOp::Add, Alignment::Out),
                ];
                for ((chain_name, op, how), cut) in chains
                    .into_iter()
                    .flat_map(|chain| [(chain, false), (chain, true)])
                {
                    let cut_name = if cut { " cut at each cycle" } else { "" };
                    let name = format!("{chain_name} of {name}{cut_name} with {structure_name}");
                    let chain = || compose(&receiver(&layers, cut), &built(&structure), op, how);
                    let folded = chain();
                    if matches!(folded.node_kind(), Node::Sequence(_) | Node::Silence) {
                        folds.push(name.clone());
                    }
                    assert_same_answers(&name, &folded, &unfolded(chain));
                }
            }
        }
        // Values of two layers answer each step layer by layer, and so do
        // functions of two layers: any other pair is one node.
        assert!(folds.len() >= 42, "{}: {folds:#?}", folds.len());
    }

    #[test]
    fn a_scale_of_a_sequence_answers_as_the_map_of_each_hap() {
        let scaled = |pattern: Pattern, name: &str| {
            let degrees = pattern.map_control(&ControlSpec::new(["n"]), false);
            crate::combinators::scale(&degrees, Value::Str(name.into()))
        };
        for (name, layers) in sequences() {
            // The scale names each note and its own key of the context,
            // and controls and another scale come after it.
            let chains: [(&str, &dyn Fn(Pattern) -> Pattern); 3] = [
                ("a scale", &|pattern: Pattern| scaled(pattern, "C:minor")),
                ("controls after a scale", &|pattern: Pattern| {
                    let gain =
                        pure(Value::F64(0.5)).map_control(&ControlSpec::new(["gain"]), false);
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

    /// A control of a constant takes the short way to its fold. The fold is
    /// the one that the composition of the control's pattern gives, and it
    /// answers as the nodes it stands for.
    #[test]
    fn a_control_of_a_constant_folds_as_its_composition_folds() {
        let gain = ControlSpec::new(["gain"]);
        let sound = ControlSpec::new(["s"]);
        let constants: [(&str, &dyn Fn() -> Pattern); 3] = [
            ("a number", &|| pure(Value::F64(0.5))),
            ("a number at a place", &|| located_value(0.25)),
            ("one step", &|| {
                built(&[layer((1, 1), vec![step((0, 1), (1, 1), 3.0)])])
            }),
        ];
        for (name, layers) in sequences() {
            let notes = || built(&layers).map_control(&ControlSpec::new(["n"]), false);
            for (constant_name, constant) in constants {
                let name = format!("{constant_name} on {name}");
                let set = gain.apply(&notes(), Some(constant()));
                let composed = compose(
                    &notes(),
                    &gain.pattern(&constant()),
                    ComposeOp::Set,
                    Alignment::In,
                );
                assert_eq!(
                    matches!(set.node_kind(), Node::Sequence(_)),
                    matches!(composed.node_kind(), Node::Sequence(_)),
                    "{name}"
                );
                assert_same_haps(&name, &set, &composed);
                // Each control has a place of its own, so their order shows.
                let chain = |pattern: Pattern| {
                    let quiet = gain.apply(&pattern, Some(constant()));
                    sound.apply(&quiet, Some(located_value(7.0)))
                };
                assert_same_answers(&name, &chain(notes()), &unfolded(|| chain(notes())));
            }
        }
    }

    /// A part lies in one cycle in every turn of a period when each turn,
    /// read on its own, holds the part in one cycle.
    #[test]
    fn a_part_lies_in_cycles_when_each_turn_holds_it_in_one() {
        for period in [
            (1, 1),
            (3, 1),
            (1, 2),
            (3, 2),
            (2, 3),
            (5, 3),
            (7, 4),
            (5, 6),
        ] {
            let period = frac(period);
            let twelfths = (period * frac((12, 1))).floor().numer();
            for (begin, end) in
                (0..twelfths).flat_map(|begin| (begin + 1..=twelfths).map(move |end| (begin, end)))
            {
                let part = TimeSpan::new(frac((begin, 12)), frac((end, 12)));
                let in_each_turn = (0..period.denom()).all(|turn| {
                    let start = period * frac((turn, 1));
                    part.end + start <= (part.begin + start).floor() + Fraction::ONE
                });
                let place = Place::at(part, None, 0, Locations::new());
                assert_eq!(
                    lie_in_cycles(period, &[place]),
                    in_each_turn,
                    "{part:?} of a period of {period:?}"
                );
            }
        }
    }

    /// A step whose time in cycles leaves the native range is no sequence.
    #[test]
    fn a_time_outside_the_native_range_is_no_sequence() {
        let far = layer((i128::MAX, 1), vec![step((2, 3), (1, 1), 0.0)]);
        assert!(sequence(vec![far]).is_none());
    }

    /// The four equal steps of a cycle, at a source that reads no span
    /// of a time finer than a part in a thousand.
    struct Quarters;

    impl StepSource for Quarters {
        fn count(&self) -> usize {
            4
        }

        fn steps(
            &self,
            begin: Fraction,
            end: Fraction,
            each: &mut dyn FnMut(SourceStep),
        ) -> Option<()> {
            for quarter in 0..4 {
                if begin.denom() > 1000 && quarter == 1 {
                    return None;
                }
                let (from, to) = (frac((quarter, 4)), frac((quarter + 1, 4)));
                let holds = from <= begin && begin < to;
                if holds || (begin < from && from < end) {
                    let mut locations = Locations::new();
                    locations.push((quarter as usize, quarter as usize + 1));
                    each(SourceStep {
                        begin: from,
                        end: to,
                        value: quarter as u32,
                        locations,
                    });
                }
            }
            Some(())
        }
    }

    /// A layer that reads its steps from a source answers as a layer that
    /// lists the same steps: for a query that walks it, for a table of it,
    /// and for a fold of it, which both list the steps.
    #[test]
    fn steps_at_a_source_answer_as_the_same_steps_in_a_list() {
        let values = (0..4).map(|value| Value::F64(f64::from(value)));
        let sourced = SourcedLayer {
            period: Fraction::ONE,
            values: values.collect(),
            in_cycles: true,
            steps: Arc::new(Quarters),
        };
        let read = sequence_of(vec![GivenLayer::Sourced(sourced)]).expect("a sound layer");
        let steps = (0..4).map(|at| step((at, 4), (at + 1, 4), at as f64));
        let listed = built(&[layer((1, 1), steps.collect())]);
        let twice = |pattern: &Pattern| faster(pattern, frac((2, 1))).expect("a sequence");
        let instant = (frac((1, 4)), frac((1, 4)));
        // The source reads no span of a time this fine: the layer lists
        // its steps for it.
        let fine = (frac((1, 1024)), frac((3, 4)));
        // Folds that move no step leave the steps at their source: a
        // name, a scale, which tags each hap, and a location.
        let noted = |pattern: &Pattern| {
            let steps = pattern.map_control(&ControlSpec::new(["n"]), false);
            let notes = crate::combinators::scale(&steps, Value::Str("C:minor".into()));
            notes.with_added_context(vec![(7, 8)])
        };
        // A walked handle reads the source at each query, with no table.
        let (walked, noted_walk) = (read.mark_walked(), noted(&read).mark_walked());
        for (begin, end) in spans().into_iter().chain([instant, fine]) {
            assert_eq!(
                answers(&walked, begin, end),
                answers(&listed, begin, end),
                "a walk over {begin:?}..{end:?}"
            );
            assert_eq!(
                answers(&noted_walk, begin, end),
                answers(&noted(&listed), begin, end),
                "a walk of notes over {begin:?}..{end:?}"
            );
        }
        for (begin, end) in spans().into_iter().chain([instant]) {
            assert_eq!(
                answers(&read, begin, end),
                answers(&listed, begin, end),
                "over {begin:?}..{end:?}"
            );
            assert_eq!(
                answers(&twice(&read), begin, end),
                answers(&twice(&listed), begin, end),
                "twice as fast, over {begin:?}..{end:?}"
            );
        }
    }

    #[test]
    fn a_short_source_in_a_long_period_keeps_its_rests_and_locations() {
        let period = Fraction::int(8);
        let read = sequence_of(vec![GivenLayer::Sourced(SourcedLayer {
            period,
            values: (0..4).map(|at| Value::F64(f64::from(at))).collect(),
            in_cycles: true,
            steps: Arc::new(Quarters),
        })])
        .unwrap();
        let steps = (0..4)
            .map(|at| step((at, 32), (at + 1, 32), at as f64))
            .collect();
        let listed = built(&[SequenceLayer { period, steps }]);
        let located = |pattern: Pattern| pattern.with_added_context(vec![(70, 72)]).mark_walked();
        let (read, listed) = (located(read), located(listed));
        for eighth in -67..=67 {
            let begin = Fraction::new(eighth, 8);
            for width in [Fraction::ZERO, frac((1, 7)), frac((17, 2))] {
                let end = begin + width;
                assert_eq!(
                    answers(&read, begin, end),
                    answers(&listed, begin, end),
                    "{begin:?}..{end:?}"
                );
            }
        }
    }

    /// Equal data values of a layer are one value, which its steps share.
    #[test]
    fn equal_values_of_a_layer_are_one_value() {
        let steps = [0.0, 1.0, 0.0, 1.0, 2.0].iter().enumerate();
        let steps = steps.map(|(at, value)| {
            let at = at as i128;
            step((at, 5), (at + 1, 5), *value)
        });
        let pattern = built(&[layer((1, 1), steps.collect())]);
        let Node::Sequence(layers) = pattern.node_kind() else {
            panic!("the steps are no sequence");
        };
        let shared: Vec<u32> = layers[0].places().iter().map(|place| place.value).collect();
        assert_eq!(shared, [0, 1, 0, 1, 2]);
        assert_eq!(layers[0].held_values().len(), 3);
    }

    /// The controls that the last meeting of `pattern` holds apart, when
    /// `pattern` is a run of constants that ends with sets in a row.
    fn sets_of(pattern: &Pattern) -> Option<&[Value]> {
        let Node::Constants(_, run) = pattern.node_kind() else {
            return None;
        };
        match &run.last()?.apply {
            Combine::Sets(controls) => Some(controls),
            _ => None,
        }
    }

    /// Sets of constant controls in a row on a pattern that is no sequence
    /// are one meeting of one node, and it answers as the applications one
    /// by one:
    /// for maps of controls under a control of a signal, for numbers, and
    /// for strings and a function of the script, which no meeting takes in
    /// place. The last control of the chain sets the key of the function,
    /// so the lookup of each hap shows whether the controls met it in turn.
    #[test]
    fn sets_of_constants_in_a_row_are_one_meeting() {
        use crate::controls::{ChainArgument, apply_chain};
        let control = |name: &str| ControlSpec::new([name]);
        // A filter keeps its node, so no control folds into the steps.
        let kept = |layers: &[SequenceLayer]| built(layers).filter_haps(|_| true);
        let function = Value::Function(Box::new(crate::value::FunctionRef::js(7)));
        let bases: [Base<'_>; 4] = [
            ("controls under a signal", &|layers| {
                let notes = built(layers).map_control(&control("n"), false);
                // A map of a signal is no wave, which a control goes under.
                let cutoff = crate::signal::sine().fmap(Value::clone);
                control("lpf").apply(&notes, Some(cutoff))
            }),
            ("numbers", &kept),
            ("strings", &|layers| {
                kept(layers).fmap(|value| Value::Str(value.show().into()))
            }),
            ("a function of the script", &|layers| {
                // A handle of the function for each hap.
                let function = function.clone();
                kept(layers).fmap(move |value| {
                    Value::object([
                        ("n".to_owned(), value.clone()),
                        ("room".to_owned(), function.clone()),
                    ])
                })
            }),
        ];
        // The second gain takes the place of the first, and each control
        // has a place of its own, so their order shows.
        let chain = |pattern: Pattern| {
            ["gain", "pan", "gain", "room"].iter().enumerate().fold(
                pattern,
                |pattern, (index, name)| {
                    control(name).apply(&pattern, Some(located_value(index as f64 + 0.5)))
                },
            )
        };
        let specs = ["gain", "pan", "gain", "room"].map(control);
        let row = |pattern: Pattern| {
            let numbers = (0..).map(|index| ChainArgument::Number(f64::from(index) + 0.5));
            let calls: Vec<_> = specs.iter().zip(numbers).collect();
            apply_chain(pattern, &calls)
        };
        for (name, layers) in sequences() {
            for (base_name, base) in bases {
                let name = format!("{base_name} of {name}");
                let joined = chain(base(&layers));
                let Node::Constants(under, run) = joined.node_kind() else {
                    panic!("{name} is no run of constants");
                };
                assert_eq!(run.len(), 1, "{name}");
                assert_eq!(sets_of(&joined).map(<[Value]>::len), Some(4), "{name}");
                assert!(!matches!(under.node_kind(), Node::Constants(..)), "{name}");
                assert_same_answers(&name, &joined, &unfolded(|| chain(base(&layers))));
                // A row on a stack is one such meeting on the pattern of
                // the stack that takes no control.
                let stacked = || {
                    let notes = built(&layers).map_control(&control("n"), false);
                    crate::stack(vec![notes, base(&layers)])
                };
                let name = format!("a row on a stack of {name}");
                assert_same_answers(&name, &row(stacked()), &unfolded(|| row(stacked())));
            }
        }
    }

    /// A chain of controls answers as its controls applied in turn, with
    /// no fold: for constants in a row, for a pattern between them, for a
    /// control that a union keeps out, and for the name of a wave over the
    /// steps. The steps of a sequence, of a pick and under a wave hold the
    /// constants, and a pattern that keeps its node holds none.
    #[test]
    fn a_chain_of_controls_answers_as_each_control_in_turn() {
        use crate::controls::{ChainArgument, apply_chain};
        let number = |name: &'static str, n: f64| (name, ChainArgument::Number(n));
        let pattern = |name: &'static str, values: Pattern| (name, ChainArgument::Pattern(values));
        let two_steps = || {
            built(&[layer(
                (1, 1),
                vec![step((0, 1), (1, 2), 20.0), step((1, 2), (1, 1), 21.0)],
            )])
        };
        type Chain = Vec<(&'static str, ChainArgument)>;
        let chains: [(&str, &dyn Fn() -> Chain); 4] = [
            ("constants in a row", &|| {
                vec![
                    number("gain", 0.5),
                    pattern("s", located_value(7.0)),
                    number("pan", 0.25),
                    number("gain", 0.75),
                    pattern("room", located_value(3.0)),
                ]
            }),
            ("a pattern between constants", &|| {
                vec![
                    number("gain", 0.5),
                    number("pan", 0.25),
                    pattern("cutoff", two_steps()),
                    pattern("room", located_value(3.0)),
                    number("gain", 1.5),
                ]
            }),
            ("a control named value", &|| {
                vec![
                    number("gain", 0.5),
                    number("value", 2.0),
                    number("pan", 0.25),
                ]
            }),
            ("the name of the wave", &|| {
                vec![
                    number("gain", 0.5),
                    number("lpf", 300.0),
                    number("pan", 0.25),
                ]
            }),
        ];
        let control = |name: &str| ControlSpec::new([name]);
        let notes = |layers: &[SequenceLayer]| built(layers).map_control(&control("n"), false);
        let kept = |layers: &[SequenceLayer]| notes(layers).filter_haps(|_| true);
        let waved = |layers: &[SequenceLayer]| {
            control("lpf").apply(&notes(layers), Some(crate::signal::sine()))
        };
        let picked = |layers: &[SequenceLayer]| {
            let selector = built(&[layer(
                (2, 1),
                vec![step((0, 1), (1, 2), 0.0), step((1, 2), (1, 1), 1.0)],
            )]);
            let lookup = crate::PickLookup::Array {
                enumerable_len: 2,
                length: 2,
                entries: vec![(0, notes(layers)), (1, waved(layers))],
            };
            let (index, join) = (crate::PickIndexMode::Clamp, crate::JoinMode::Restart);
            crate::pick(selector, lookup, index, join)
        };
        // A run of sets holds each control apart, also those of a chain.
        let run = |layers: &[SequenceLayer]| {
            control("gain").apply(&kept(layers), Some(located_value(0.125)))
        };
        // A pattern of the stack that takes no control meets each row as
        // one meeting, which strings meet control by control.
        let stacked = |layers: &[SequenceLayer]| {
            crate::stack(vec![
                notes(layers),
                waved(layers),
                picked(layers),
                kept(layers).fmap(|value| Value::Str(value.show().into())),
            ])
        };
        let segmented =
            |layers: &[SequenceLayer]| crate::combinators::segment(&stacked(layers), frac((4, 1)));
        // Strings meet each control of a run one by one, with the name of
        // the wave over the run at its place.
        let strings = |layers: &[SequenceLayer]| {
            let named = kept(layers).fmap(|value| Value::Str(value.show().into()));
            let quiet = control("gain").apply(&named, Some(located_value(0.125)));
            control("lpf").apply(&quiet, Some(crate::signal::sine()))
        };
        let bases: [Base<'_>; 8] = [
            ("a sequence", &notes),
            ("steps under a wave", &waved),
            ("a pick", &picked),
            ("a stack", &stacked),
            ("a segment of a stack", &segmented),
            ("a pattern that keeps its node", &kept),
            ("a wave over a run of sets on strings", &strings),
            ("a run of sets", &run),
        ];
        for (name, layers) in sequences() {
            let (_, row) = chains[0];
            let specs: Vec<ControlSpec> = row().iter().map(|(name, _)| control(name)).collect();
            let calls: Vec<_> = specs
                .iter()
                .zip(row())
                .map(|(spec, call)| (spec, call.1))
                .collect();
            let set = apply_chain(run(&layers), &calls);
            let held = sets_of(&set).map(<[Value]>::len);
            assert_eq!(held, Some(1 + row().len()), "{name}");
            for (base_name, base) in bases {
                for (chain_name, chain) in chains {
                    let name = format!("{chain_name} on {base_name} of {name}");
                    let specs: Vec<ControlSpec> =
                        chain().iter().map(|(name, _)| control(name)).collect();
                    let calls = || {
                        specs
                            .iter()
                            .zip(chain())
                            .map(|(spec, (_, value))| (spec, value))
                    };
                    let set = apply_chain(base(&layers), &calls().collect::<Vec<_>>());
                    let in_turn = || {
                        calls().fold(base(&layers), |pattern, (spec, value)| match value {
                            ChainArgument::Number(n) => spec.apply_value(pattern, Value::F64(n)),
                            ChainArgument::Pattern(values) => spec.apply_to(pattern, Some(values)),
                        })
                    };
                    assert_same_answers(&name, &set, &unfolded(in_turn));
                }
            }
        }
    }

    /// A receiver that no other handle holds takes a constant control where
    /// it is, and answers as a copy that took it. A query before the control
    /// leaves tables and a reading of the constant on the node, which answer
    /// no query after it. A receiver that another handle holds stays as it
    /// was.
    #[test]
    fn a_receiver_held_alone_takes_a_control_where_it_is() {
        let control = |name: &str| ControlSpec::new([name]);
        let notes = |layers: &[SequenceLayer]| built(layers).map_control(&control("n"), false);
        let bases: [Base<'_>; 3] = [
            ("a sequence", &notes),
            ("one set", &|layers| {
                let swept = control("lpf").apply(&notes(layers), Some(crate::signal::sine()));
                control("gain").apply(&swept, Some(located_value(0.25)))
            }),
            ("a run of sets", &|layers| {
                let swept = control("lpf").apply(&notes(layers), Some(crate::signal::sine()));
                let quiet = control("gain").apply(&swept, Some(located_value(0.25)));
                control("room").apply(&quiet, Some(located_value(0.75)))
            }),
        ];
        // The control goes to the steps of each picked pattern.
        let picked = |layers: &[SequenceLayer], mode| {
            // A restart takes a selector whose steps begin on whole cycles.
            let period = match mode {
                crate::JoinMode::Restart => (2, 1),
                _ => (1, 1),
            };
            let selector = built(&[layer(
                period,
                vec![step((0, 1), (1, 2), 0.0), step((1, 2), (1, 1), 1.0)],
            )]);
            let lookup = crate::PickLookup::Array {
                enumerable_len: 2,
                length: 2,
                entries: vec![
                    (0, notes(layers)),
                    (
                        1,
                        control("gain").apply(&notes(layers), Some(located_value(0.25))),
                    ),
                ],
            };
            crate::pick(selector, lookup, crate::PickIndexMode::Clamp, mode)
        };
        let inner = |layers: &[SequenceLayer]| picked(layers, crate::JoinMode::Inner);
        let restart = |layers: &[SequenceLayer]| picked(layers, crate::JoinMode::Restart);
        // The control goes to each pattern of a stack: steps, steps under a
        // wave, a pick and silence.
        let stacked = |layers: &[SequenceLayer]| {
            let swept = control("lpf").apply(&notes(layers), Some(crate::signal::sine()));
            let quiet = control("gain").apply(&notes(layers), Some(located_value(0.25)));
            crate::stack(vec![quiet, swept, restart(layers), crate::silence()])
        };
        // A pattern of a stack that takes no control meets a constant of
        // it, and each other pattern takes the control.
        let kept = |layers: &[SequenceLayer]| {
            crate::stack(vec![notes(layers), notes(layers).filter_haps(|_| true)])
        };
        // The control goes to the steps under a structure that gives its
        // haps nothing more: the pulses of a segment, in each cycle.
        let segmented = |layers: &[SequenceLayer]| {
            let swept = control("lpf").apply(&notes(layers), Some(crate::signal::sine()));
            crate::combinators::segment(&crate::stack(vec![swept, notes(layers)]), frac((4, 1)))
        };
        // A structure with places in the text keeps the control over it.
        // Pulses that cross the cycles cut each hap of the steps to a part
        // in its cycle, so the control goes to the steps.
        let struck = |layers: &[SequenceLayer]| {
            let structure = built(&[layer(
                (1, 1),
                vec![step((0, 1), (1, 2), 1.0), step((1, 2), (1, 1), 0.0)],
            )]);
            crate::compose::compose(
                &notes(layers),
                &structure,
                crate::compose::ComposeOp::KeepIf,
                crate::compose::Alignment::Out,
            )
        };
        let crossed = |layers: &[SequenceLayer]| {
            let pulse = SequenceStep::new(
                Fraction::ZERO,
                Fraction::ONE,
                Value::Bool(true),
                Locations::new(),
            );
            let structure = built(&[layer((3, 2), vec![pulse])]);
            let swept = control("lpf").apply(&notes(layers), Some(crate::signal::sine()));
            crate::compose::compose(
                &crate::stack(vec![swept, notes(layers)]),
                &structure,
                crate::compose::ComposeOp::KeepIf,
                crate::compose::Alignment::Out,
            )
        };
        let bases = bases.into_iter().chain([
            ("a pick", &inner as _),
            ("a pick that restarts", &restart as _),
            ("a stack", &stacked as _),
            ("a stack that keeps a node", &kept as _),
            ("a segment", &segmented as _),
            ("a structure with places", &struck as _),
            ("pulses that cross cycles", &crossed as _),
        ]);
        let pan = |pattern: Pattern| control("pan").apply_to(pattern, Some(located_value(0.5)));
        let ask = |pattern: &Pattern| {
            for (begin, end) in spans() {
                pattern.query_arc(begin, end);
            }
        };
        let mut written = 0;
        for (name, layers) in sequences() {
            for (base_name, base) in bases.clone() {
                let name = format!("{base_name} of {name}");
                let alone = base(&layers);
                let node = Arc::as_ptr(&alone.node);
                ask(&alone);
                let alone = pan(alone);
                written += usize::from(Arc::as_ptr(&alone.node) == node);

                let held = base(&layers);
                ask(&held);
                // A handle of the receiver, which `held` keeps.
                let copied = pan(held.clone());
                assert_same_answers(&name, &alone, &copied);
                assert_same_answers(&name, &held, &base(&layers));
                assert_same_answers(&name, &alone, &unfolded(|| pan(base(&layers))));
            }
        }
        // Each base of each sequence whose steps lie in one cycle.
        assert!(written >= 7 * 4, "written where it is: {written}");
    }

    /// A control of a number, with no pattern of the number, answers as the
    /// control of a `pure` of it: on a sequence, on a set of a constant, and
    /// on a pattern that takes no control where it is.
    #[test]
    fn a_control_of_a_number_answers_as_the_control_of_its_pure() {
        let control = |name: &str| ControlSpec::new([name]);
        let notes = |layers: &[SequenceLayer]| built(layers).map_control(&control("n"), false);
        let bases: [Base<'_>; 3] = [
            ("a sequence", &notes),
            ("a set", &|layers| {
                let swept = control("lpf").apply(&notes(layers), Some(crate::signal::sine()));
                control("room").apply(&swept, Some(located_value(0.25)))
            }),
            ("a filter", &|layers| notes(layers).filter_haps(|_| true)),
        ];
        for (name, layers) in sequences() {
            for (base_name, base) in bases {
                let name = format!("{base_name} of {name}");
                let number = control("gain").apply_value(base(&layers), Value::F64(0.5));
                let reified = control("gain").apply(&base(&layers), Some(pure(Value::F64(0.5))));
                assert_eq!(
                    std::mem::discriminant(number.node_kind()),
                    std::mem::discriminant(reified.node_kind()),
                    "{name}"
                );
                assert_same_answers(&name, &number, &reified);
            }
        }
    }

    /// A control of a pattern of many steps, on a pattern that is no
    /// sequence, asks the steps one time for all the haps of a query. The
    /// haps that it then makes are those that each hap makes when it asks
    /// the steps over its own part.
    #[test]
    fn a_control_of_many_steps_asks_them_one_time() {
        let control = |name: &str| ControlSpec::new([name]);
        let kept = |layers: &[SequenceLayer]| {
            built(layers)
                .map_control(&control("n"), false)
                .filter_haps(|_| true)
        };
        let steps = |period: (i128, i128), count: i128| {
            let step = |at: i128| step((at, count), (at + 1, count), at as f64);
            built(&[layer(period, (0..count).map(step).collect())])
        };
        let values: [(&str, &dyn Fn() -> Pattern); 3] = [
            ("eight steps a cycle", &|| steps((1, 1), 8)),
            ("five steps in a cycle and a half", &|| steps((3, 2), 5)),
            ("a chord of steps", &|| {
                stack(vec![steps((1, 1), 8), steps((1, 2), 3)]).filter_haps(|_| true)
            }),
        ];
        let said = |haps: &[Hap]| {
            haps.iter()
                .map(|hap| format!("{hap:?}"))
                .collect::<Vec<_>>()
        };
        let mut asked_one_time = 0;
        for (name, layers) in sequences() {
            for (values_name, values) in values {
                let name = format!("{values_name} on {name}");
                let applied = control("gain").apply(&kept(&layers), Some(values()));
                let side = crate::ValueSide::of(applied.node_kind()).expect("an application");
                for (begin, end) in spans() {
                    let state = State::new(TimeSpan::new(begin, end));
                    let functions = side.functions.query(&state);
                    let mut one_query = Vec::new();
                    if !side.met_in_one_query(&mut functions.clone(), &state, &mut one_query) {
                        continue;
                    }
                    asked_one_time += 1;
                    let mut each = Vec::new();
                    let mut meetings = crate::Meetings::default();
                    for hf in functions {
                        assert!(side.meet(hf, &mut meetings, &state, 0, &mut each));
                    }
                    assert_eq!(said(&one_query), said(&each), "{name}: {begin:?}..{end:?}");
                }
            }
        }
        assert!(asked_one_time >= 30, "asked one time: {asked_one_time}");
    }

    /// A union with a map of `value` alone keeps the other side as it is, so
    /// a set of such a map joins no run.
    #[test]
    fn a_set_that_a_union_drops_joins_no_run() {
        let gain = ControlSpec::new(["gain"]);
        let value_alone = ControlSpec::new(["value"]);
        for (name, layers) in sequences() {
            let base = || {
                built(&layers)
                    .map_control(&ControlSpec::new(["n"]), false)
                    .filter_haps(|_| true)
            };
            let chains: [(&str, [&ControlSpec; 2]); 2] = [
                ("dropped first", [&value_alone, &gain]),
                ("dropped last", [&gain, &value_alone]),
            ];
            for (chain_name, controls) in chains {
                let name = format!("{chain_name} on {name}");
                let chain = || {
                    controls.iter().fold(base(), |pattern, control| {
                        control.apply(&pattern, Some(located_value(0.5)))
                    })
                };
                assert!(sets_of(&chain()).is_none(), "{name}");
                assert_same_answers(&name, &chain(), &unfolded(chain));
            }
        }
    }

    /// A row with the name of a wave, on a stack with a pattern under that
    /// wave: the control of that name meets the pattern after the wave, and
    /// the other control goes under the wave, into the steps.
    #[test]
    fn a_row_with_the_name_of_a_wave_sets_the_others_under_it() {
        use crate::controls::{ChainArgument, apply_chain};
        let (gain, lpf) = (ControlSpec::new(["gain"]), ControlSpec::new(["lpf"]));
        for (name, layers) in sequences() {
            let stacked = || {
                let notes = || built(&layers).map_control(&ControlSpec::new(["n"]), false);
                let waved = lpf.apply(&notes(), Some(crate::signal::sine()));
                crate::stack(vec![notes(), waved])
            };
            let row = [
                (&gain, ChainArgument::Number(0.5)),
                (&lpf, ChainArgument::Number(800.0)),
            ];
            let set = apply_chain(stacked(), &row);
            assert_same_answers(name, &set, &unfolded(|| apply_chain(stacked(), &row)));
            // Steps that lie in one cycle take a row where they are.
            if !lies_in_cycles(&built(&layers)) {
                continue;
            }
            let Node::Stack(patterns) = set.node_kind() else {
                panic!("{name} is no stack");
            };
            let Node::Constants(_, run) = patterns[1].node_kind() else {
                panic!("the wave of {name} meets no constant");
            };
            assert_eq!(run.len(), 1, "{name}");
            assert!(sets_of(&patterns[1]).is_none(), "{name}");
        }
    }

    /// A `set` that gives its hap another lookup than a `set` of controls
    /// does joins no run, and no control joins it.
    #[test]
    fn a_set_of_another_lookup_joins_no_run() {
        let gain = ControlSpec::new(["gain"]);
        for (name, layers) in sequences() {
            let chain = || {
                let notes = built(&layers)
                    .map_control(&ControlSpec::new(["n"]), false)
                    .filter_haps(|_| true);
                let quiet = gain.pattern(&located_value(0.5));
                let set = notes.app_left_composed(quiet, ComposeOp::Set, LookupFlow::Left);
                ControlSpec::new(["pan"]).apply(&set, Some(located_value(1.5)))
            };
            let set = chain();
            let Node::Constants(_, run) = set.node_kind() else {
                panic!("{name} is no run of constants");
            };
            assert_eq!(run.len(), 2, "{name}");
            assert!(sets_of(&set).is_none(), "{name}");
            assert_same_answers(name, &set, &unfolded(chain));
        }
    }

    /// A pick whose patterns take no control where they are folds a
    /// constant control into the steps of each, when the steps lie in one
    /// cycle: no run of constants is made over the pick.
    #[test]
    fn a_pick_folds_a_control_that_its_steps_do_not_take() {
        for (name, layers) in sequences() {
            let chain = || {
                let selector = built(&[layer((1, 1), vec![step((0, 1), (1, 1), 0.0)])]);
                let lookup = crate::PickLookup::Array {
                    enumerable_len: 1,
                    length: 1,
                    // Steps of numbers hold no map for a control to go in.
                    entries: vec![(0, built(&layers))],
                };
                let (index, join) = (crate::PickIndexMode::Clamp, crate::JoinMode::Restart);
                let picked = crate::pick(selector, lookup, index, join);
                ControlSpec::new(["gain"]).apply(&picked, Some(located_value(0.5)))
            };
            let folded = matches!(chain().node_kind(), Node::Pick { .. });
            assert_eq!(folded, lies_in_cycles(&built(&layers)), "{name}");
            assert_same_answers(name, &chain(), &unfolded(chain));
        }
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
            .map_control(&ControlSpec::new(["n"]), false)
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

    fn text(value: &str) -> Value {
        Value::Str(value.into())
    }

    fn map_of(entries: &[(&str, Value)]) -> Value {
        Value::object(
            entries
                .iter()
                .map(|(key, value)| (key.to_string(), value.clone())),
        )
    }

    /// A layer of `values` that `names` and `set` dress.
    fn dressed_layer(values: Vec<Value>, names: Option<&[&str]>, set: Option<Value>) -> Layer {
        let raws: Vec<Raw> = (0..values.len())
            .map(|at| {
                let at = at as i128;
                let whole = TimeSpan::new(frac((at, 4)), frac((at + 1, 4)));
                Raw {
                    whole,
                    part: None,
                    value: at as u32,
                    locations: Locations::new(),
                }
            })
            .collect();
        let mut layer = Layer::new(frac((1, 1)), raws, values.into());
        layer.change_dress(|dress| {
            dress.named = names.map(|names| names.iter().map(|name| Arc::from(*name)).collect());
            dress.set = set;
        });
        layer
    }

    /// The value of each step as one map of its own: the way a layer made
    /// each value before the steps shared a table.
    fn made_plain(layer: &Layer) -> Vec<Value> {
        let dress = layer.dress();
        layer
            .values
            .iter()
            .map(|value| {
                let mut value = match &dress.named {
                    Some(names) => crate::controls::named_value(names, value.clone()),
                    None => value.clone(),
                };
                if let Some(set) = &dress.set {
                    compose_data_in_place(ComposeOp::Set, &mut value, set);
                }
                value
            })
            .collect()
    }

    /// Every way to read the two maps agrees.
    fn assert_one_map(shared: &crate::OrderedMap, plain: &crate::OrderedMap, what: &str) {
        assert_eq!(shared, plain, "{what}: ==");
        assert_eq!(format!("{shared:?}"), format!("{plain:?}"), "{what}: Debug");
        assert_eq!(
            format!("{shared:#?}"),
            format!("{plain:#?}"),
            "{what}: Debug#"
        );
        assert_eq!(shared.len(), plain.len(), "{what}: len");
        assert_eq!(shared.is_empty(), plain.is_empty(), "{what}: is_empty");
        assert_eq!(shared.data_hash(), plain.data_hash(), "{what}: data_hash");
        assert!(
            shared.same_data(plain) && plain.same_data(shared),
            "{what}: same_data"
        );
        assert_eq!(
            shared.holds_only_data(),
            plain.holds_only_data(),
            "{what}: data"
        );
        assert_eq!(shared.owns_no_heap(), plain.owns_no_heap(), "{what}: heap");
        assert_eq!(
            shared.js_entries(),
            plain.js_entries(),
            "{what}: js_entries"
        );
        assert_eq!(
            shared.iter().collect::<Vec<_>>(),
            plain.iter().collect::<Vec<_>>(),
            "{what}: iter"
        );
        assert_eq!(
            shared.into_iter().collect::<Vec<_>>(),
            plain.into_iter().collect::<Vec<_>>(),
            "{what}: into_iter"
        );
        for key in ["note", "s", "n", "gain", "release", "value", "new", "none"] {
            assert_eq!(shared.get(key), plain.get(key), "{what}: get {key}");
            assert_eq!(
                shared.contains_key(key),
                plain.contains_key(key),
                "{what}: has {key}"
            );
        }
    }

    #[test]
    fn a_shared_table_reads_as_the_plain_maps() {
        let set = map_of(&[
            ("s", text("gm_piano")),
            ("release", Value::F64(0.28)),
            ("gain", Value::F64(0.78)),
        ]);
        let notes = || vec![text("d#3"), text("a2"), Value::F64(7.0), text("d#3")];
        let raw = || {
            vec![
                map_of(&[("note", text("c3")), ("n", Value::F64(1.0))]),
                map_of(&[("n", Value::F64(2.0))]),
                map_of(&[]),
            ]
        };
        let object = |object: Value| Some(object);
        // The name, the layer's values, its set, and whether the table
        // reads them.
        type Case<'a> = (
            &'a str,
            Vec<Value>,
            Option<&'a [&'a str]>,
            Option<Value>,
            bool,
        );
        let cases: Vec<Case> = vec![
            (
                "one name and a set",
                notes(),
                Some(&["note"]),
                object(set.clone()),
                true,
            ),
            ("one name, no set", notes(), Some(&["note"]), None, true),
            (
                "two names, no list",
                notes(),
                Some(&["note", "s"]),
                object(set.clone()),
                true,
            ),
            (
                "a raw map and a set",
                raw(),
                None,
                object(set.clone()),
                true,
            ),
            (
                "a name that the set holds",
                notes(),
                Some(&["s"]),
                object(set.clone()),
                false,
            ),
            (
                "a raw map with a key that the set holds",
                vec![map_of(&[("gain", Value::F64(1.0)), ("n", Value::F64(2.0))])],
                None,
                object(set.clone()),
                false,
            ),
            (
                "a list under two names",
                vec![Value::List(vec![text("c3"), text("gm_piano")])],
                Some(&["note", "s"]),
                object(set.clone()),
                false,
            ),
            (
                "a list under one name",
                vec![Value::List(vec![text("c3"), text("e3")])],
                Some(&["note"]),
                object(set.clone()),
                true,
            ),
            (
                "an object with a value key",
                vec![map_of(&[("value", text("c3")), ("n", Value::F64(2.0))])],
                Some(&["note"]),
                object(set.clone()),
                false,
            ),
            (
                "a value that is no data",
                vec![Value::Haps(crate::value::HapList::new(Vec::new()))],
                Some(&["note"]),
                None,
                false,
            ),
            (
                "a set that a union drops",
                notes(),
                Some(&["note"]),
                object(map_of(&[("value", Value::F64(1.0))])),
                false,
            ),
            (
                "an object under a name",
                vec![map_of(&[("n", Value::F64(1.0))]), text("x")],
                Some(&["dict"]),
                object(set.clone()),
                true,
            ),
        ];
        for (what, values, names, set, shared) in cases {
            let layer = dressed_layer(values, names, set);
            let plain = made_plain(&layer);
            assert_eq!(layer.each_value().count(), plain.len(), "{what}: count");
            assert!(layer.value(plain.len()).is_none(), "{what}: past the end");
            let read = layer.values_read.get().expect("the layer read its values");
            assert_eq!(matches!(read, Read::Shared(_)), shared, "{what}: the form");
            for (index, plain) in plain.iter().enumerate() {
                let Some(Value::Object(shared)) = layer.value(index) else {
                    panic!("{what}: step {index} is no map");
                };
                let Value::Object(plain) = plain else {
                    panic!("{what}: plain step {index} is no map");
                };
                let name = format!("{what}, step {index}");
                assert_one_map(&shared, plain, &name);
                // A compose reads data only.
                if !Value::Object(plain.clone()).is_data(0) {
                    continue;
                }
                // A write copies the entries into a plain map first.
                for write in 0..5 {
                    let (mut shared, mut plain) = (shared.clone(), plain.clone());
                    let change = |map: &mut crate::OrderedMap| match write {
                        0 => {
                            map.insert("gain".into(), Value::F64(9.0));
                        }
                        1 => {
                            map.insert("new".into(), Value::F64(9.0));
                        }
                        2 => {
                            map.remove("s");
                            map.remove("note");
                        }
                        3 => {
                            if let Some(value) = map.get_mut("release") {
                                *value = Value::F64(9.0);
                            }
                        }
                        _ => {
                            let mut over = Value::Object(map.clone());
                            let by = map_of(&[("n", Value::F64(5.0)), ("fresh", text("f"))]);
                            compose_data_in_place(ComposeOp::Set, &mut over, &by);
                            *map = match over {
                                Value::Object(over) => over,
                                _ => unreachable!("a map composed with a map"),
                            };
                        }
                    };
                    change(&mut shared);
                    change(&mut plain);
                    assert_one_map(&shared, &plain, &format!("{name}, write {write}"));
                }
                // A map that a dressed map is set onto reads the same.
                let (mut onto, mut onto_plain) = (
                    map_of(&[("n", Value::F64(0.0)), ("gain", Value::F64(1.0))]),
                    map_of(&[("n", Value::F64(0.0)), ("gain", Value::F64(1.0))]),
                );
                compose_data_in_place(ComposeOp::Set, &mut onto, &Value::Object(shared.clone()));
                compose_data_in_place(
                    ComposeOp::Set,
                    &mut onto_plain,
                    &Value::Object(plain.clone()),
                );
                let (Value::Object(onto), Value::Object(onto_plain)) = (onto, onto_plain) else {
                    panic!("{what}: a union is no map");
                };
                assert_one_map(&onto, &onto_plain, &format!("{name}, set onto"));
                // An empty map of data set onto it leaves it data.
                let (mut empty, mut empty_plain) =
                    (Value::Object(shared.clone()), Value::Object(plain.clone()));
                compose_data_in_place(ComposeOp::Set, &mut empty, &map_of(&[]));
                compose_data_in_place(ComposeOp::Set, &mut empty_plain, &map_of(&[]));
                let (Value::Object(empty), Value::Object(empty_plain)) = (empty, empty_plain)
                else {
                    panic!("{what}: a union is no map");
                };
                assert_one_map(&empty, &empty_plain, &format!("{name}, empty set onto"));
            }
        }
    }

    /// The order of the entries is the order of the compose of the set
    /// onto the value: the entries of the step, then those of the set.
    #[test]
    fn a_shared_map_holds_the_entries_of_its_step_before_the_set() {
        let set = map_of(&[("s", text("gm_piano")), ("gain", Value::F64(0.5))]);
        let layer = dressed_layer(vec![text("c3")], Some(&["note"]), Some(set));
        let Some(Value::Object(map)) = layer.value(0) else {
            panic!("a step is a map");
        };
        assert_eq!(
            map.iter().map(|(key, _)| key).collect::<Vec<_>>(),
            ["note", "s", "gain"]
        );
    }

    /// A map that reads a table is the raw map of another layer, or the
    /// set of one, and reads the same as the plain map.
    #[test]
    fn a_dressed_map_dresses_another_layer() {
        let inner_set = map_of(&[("s", text("gm_piano")), ("gain", Value::F64(0.5))]);
        let inner = dressed_layer(
            vec![text("c3"), text("e3")],
            Some(&["note"]),
            Some(inner_set),
        );
        let maps: Vec<Value> = inner.each_value().collect();
        let outer_set = map_of(&[("release", Value::F64(0.2))]);
        // The dressed maps as raw values, then as the set.
        for (values, set) in [
            (maps.clone(), outer_set.clone()),
            (vec![text("a2")], maps[0].clone()),
        ] {
            let names: Option<&[&str]> = match values[0] {
                Value::Object(_) => None,
                _ => Some(&["n"]),
            };
            let layer = dressed_layer(values, names, Some(set));
            assert!(matches!(layer.reader(), Values::Table(_)));
            for (shared, plain) in layer.each_value().zip(made_plain(&layer)) {
                let (Value::Object(shared), Value::Object(plain)) = (shared, plain) else {
                    panic!("a step is no map");
                };
                assert_one_map(&shared, &plain, "a dressed map under a dress");
            }
        }
    }

    /// A layer of `cells` steps that fill `[0, 1)` of the period, some cut
    /// by a wider whole, with the locations that `variant` picks.
    fn grid_layer(period: (i128, i128), cells: i128, variant: usize) -> SequenceLayer {
        let steps = (0..cells)
            .map(|cell| {
                let at = |n: i128| frac((n, cells));
                let mut locations = Locations::new();
                let (begin, end) = match (variant + cell as usize) % 6 {
                    0 => (0, 0),
                    1 => (cell as usize * 7, cell as usize * 7 + 3),
                    // A range of no length is not no range.
                    2 => (cell as usize, cell as usize),
                    3 => (cell as usize, cell as usize + 1),
                    4 => ((1 << 22) + cell as usize, (1 << 22) + cell as usize + 5000),
                    _ => (0, 1),
                };
                match (variant + cell as usize) % 6 {
                    0 => {}
                    3 => locations.extend_from_slice(&[(begin, end), (begin + 9, end + 9)]),
                    _ => locations.push((begin, end)),
                }
                let mut step =
                    SequenceStep::new(at(cell), at(cell + 1), Value::F64(cell as f64), locations);
                if cell % 3 == 1 {
                    // The whole reaches into the cells around the part.
                    let whole = (at((cell - 1).max(0)), at((cell + 2).min(cells)));
                    step = step.placed(whole, (at(cell), at(cell + 1)));
                }
                step
            })
            .collect();
        layer(period, steps)
    }

    fn haps_of(layer: &Layer, span: (Fraction, Fraction)) -> Vec<String> {
        let mut out = Vec::new();
        query_sequence_onto(
            std::slice::from_ref(layer),
            &State::new(TimeSpan::new(span.0, span.1)),
            &mut out,
        );
        out.iter()
            .map(|hap| format!("{hap:?} {:?}", hap.context.to_vec()))
            .collect()
    }

    /// The same layer with its steps as places.
    fn as_places(layer: &Layer) -> Layer {
        let places: Arc<[Place]> = layer.places().into();
        layer.with(|layer| {
            layer.steps = Steps::Listed(Listing::Places(Listed {
                places,
                small_denominators: true,
            }));
        })
    }

    fn grid_of(layer: &Layer) -> &Grid {
        match layer.listing() {
            Listing::Grid(grid) => grid,
            Listing::Places(_) => panic!("the steps are not on a grid"),
        }
    }

    /// The grid of a layer answers as its places do, over spans that are
    /// negative, of no width, on a tick or between two, and at the ends of
    /// a period.
    #[test]
    fn a_layer_on_a_grid_answers_as_its_places_do() {
        let mut seen = 0;
        for period in [(1, 1), (3, 2), (2, 1), (1, 3)] {
            for cells in [1, 2, 3, 4, 5, 8, 12] {
                for variant in 0..6 {
                    let grid = Layer::of(grid_layer(period, cells, variant)).unwrap();
                    grid_of(&grid);
                    let places = as_places(&grid);
                    for begin in -40..40i128 {
                        for width in [0, 1, 3, 7, 24] {
                            let begin = frac((begin, 8));
                            let span = (begin, begin.add(frac((width, 8))));
                            assert_eq!(
                                haps_of(&grid, span),
                                haps_of(&places, span),
                                "{period:?} {cells} {variant} {span:?}"
                            );
                        }
                    }
                    // A span between two ticks.
                    for span in [(frac((1, 7)), frac((22, 7))), (frac((-5, 3)), frac((2, 9)))] {
                        assert_eq!(haps_of(&grid, span), haps_of(&places, span));
                    }
                    seen += 1;
                }
            }
        }
        assert_eq!(seen, 168);
    }

    /// A grid gives back each time, cut and location that made it.
    #[test]
    fn a_grid_gives_back_its_steps() {
        for cells in [1, 3, 5, 12] {
            for variant in 0..6 {
                let raws = |layer: SequenceLayer| {
                    layer
                        .steps
                        .iter()
                        .map(|step| (step.whole(), step.part.is_some(), step.locations().clone()))
                        .collect::<Vec<_>>()
                };
                let made = grid_layer((1, 1), cells, variant);
                let layer = Layer::of(made.clone()).unwrap();
                let listing = layer.listing();
                for (index, (whole, part, locations)) in raws(made).into_iter().enumerate() {
                    assert_eq!(listing.whole(index), TimeSpan::new(whole.0, whole.1));
                    assert_eq!(listing.is_cut(index), part);
                    assert_eq!(listing.locations(index), locations);
                }
            }
        }
    }

    /// A tick that is zero is the canonical zero, and the tick size is the
    /// least common multiple of the denominators.
    #[test]
    fn a_grid_is_as_fine_as_its_denominators() {
        let layer = Layer::of(grid_layer((1, 1), 4, 0)).unwrap();
        let grid = grid_of(&layer);
        assert_eq!(grid.ticks.get(), 4);
        assert_eq!(grid.time(0), Fraction::ZERO);
        assert_eq!(grid.time(0).denom(), 1);
        let layer = Layer::of(grid_layer((1, 3), 4, 0)).unwrap();
        assert_eq!(grid_of(&layer).ticks.get(), 12);
    }

    /// A layer whose times do not fit 32 bits of ticks keeps its places,
    /// and a denominator past 32 bits refuses before the tick size is made.
    #[test]
    fn a_layer_off_the_grid_keeps_its_places() {
        let huge = Fraction::new(1, (1 << 40) + 1);
        let none = Locations::new;
        let steps = vec![
            SequenceStep::new(Fraction::ZERO, huge, Value::F64(0.0), none()),
            SequenceStep::new(huge, Fraction::ONE, Value::F64(1.0), none()),
        ];
        let layer = Layer::of(layer((1, 1), steps)).unwrap();
        assert!(matches!(layer.listing(), Listing::Places(_)));
        // The product of the two denominators does not fit 64 bits.
        let wide = Fraction::new((1 << 62) + 1, (1 << 63) + 1);
        let steps = vec![
            SequenceStep::new(Fraction::ZERO, frac((1, 2)), Value::F64(0.0), none()),
            SequenceStep::new(frac((1, 2)), wide, Value::F64(1.0), none()),
        ];
        let layer = Layer::of(self::layer((1, 1), steps)).unwrap();
        assert!(matches!(layer.listing(), Listing::Places(_)));
        // A negative time does not fit unsigned ticks either.
        let steps = vec![SequenceStep::new(
            frac((-1, 2)),
            frac((1, 2)),
            Value::F64(0.0),
            none(),
        )];
        let layer = Layer::of(self::layer((1, 1), steps)).unwrap();
        assert!(matches!(layer.listing(), Listing::Places(_)));
        // The places of a layer off the grid answer.
        assert_eq!(haps_of(&layer, (Fraction::ZERO, Fraction::ONE)).len(), 1);
    }

    /// A range of locations that is too long for a packed one does not push
    /// the short ranges of other steps into the spare list.
    #[test]
    fn a_long_range_leaves_the_short_ones_packed() {
        let mut long = Locations::new();
        long.push((0, 100_000));
        let mut short = Locations::new();
        short.push((5_000_000, 5_000_003));
        let steps = vec![
            SequenceStep::new(Fraction::ZERO, frac((1, 2)), Value::F64(0.0), long),
            SequenceStep::new(frac((1, 2)), Fraction::ONE, Value::F64(1.0), short),
        ];
        let layer = Layer::of(self::layer((1, 1), steps)).unwrap();
        let grid = grid_of(&layer);
        assert_eq!(grid.steps[1].loc.0 & PackedLoc::SPARE, 0);
        assert_eq!(grid.locations(1).to_vec(), [(5_000_000, 5_000_003)]);
        assert_eq!(grid.locations(0).to_vec(), [(0, 100_000)]);
    }

    /// `same` leaves a source that nobody read unread, and answers the same
    /// each time it is asked.
    #[test]
    fn same_reads_no_source() {
        struct Rest;
        impl StepSource for Rest {
            fn count(&self) -> usize {
                1
            }
            fn steps(
                &self,
                _: Fraction,
                end: Fraction,
                each: &mut dyn FnMut(SourceStep),
            ) -> Option<()> {
                each(SourceStep {
                    begin: Fraction::ZERO,
                    end,
                    value: 0,
                    locations: Locations::new(),
                });
                Some(())
            }
        }
        let sourced = |source: &Arc<dyn StepSource>| {
            Layer::of_source(SourcedLayer {
                period: Fraction::ONE,
                values: Arc::new([Value::F64(1.0)]),
                in_cycles: true,
                steps: Arc::clone(source),
            })
        };
        let one: Arc<dyn StepSource> = Arc::new(Rest);
        let (a, b) = (sourced(&one), sourced(&one));
        let other = sourced(&(Arc::new(Rest) as Arc<dyn StepSource>));
        let listed = Layer::of(layer((1, 1), vec![step((0, 1), (1, 1), 1.0)])).unwrap();
        let same = |a: &Layer, b: &Layer| a.same(b, |a, b| a.same_data(b));
        for _ in 0..2 {
            assert!(same(&a, &b));
            assert!(!same(&a, &other));
            assert!(!same(&a, &listed));
            assert!(!same(&listed, &a));
        }
        for layer in [&a, &b, &other] {
            assert!(layer.source().is_some(), "a source was read");
        }
    }

    /// Two layers of the same steps are the same layer, and two grids with
    /// other steps are not.
    #[test]
    fn grids_of_the_same_steps_are_the_same_layer() {
        let first = Layer::of(grid_layer((1, 1), 5, 2)).unwrap();
        let second = Layer::of(grid_layer((1, 1), 5, 2)).unwrap();
        let other = Layer::of(grid_layer((1, 1), 5, 3)).unwrap();
        let same = |a: &Layer, b: &Layer| a.same(b, |a, b| a.same_data(b));
        assert!(same(&first, &second));
        assert!(same(&first, &as_places(&second)));
        assert!(!same(&first, &other));
    }

    /// A grid says a layer is in order as its places do: a step past the
    /// period, out of order, empty, or with a whole inside its part is not.
    #[test]
    fn a_grid_checks_the_order_as_its_places_do() {
        let at = |a: i128, b: i128, whole: (i128, i128)| {
            let step = step((a, 4), (b, 4), 1.0);
            step.placed(
                (frac((whole.0, 4)), frac((whole.1, 4))),
                (frac((a, 4)), frac((b, 4))),
            )
        };
        let cases = [
            vec![at(0, 2, (0, 2)), at(2, 4, (2, 4))],
            vec![at(0, 2, (0, 2)), at(2, 5, (2, 5))],
            vec![at(2, 4, (2, 4)), at(0, 2, (0, 2))],
            vec![at(0, 0, (0, 0))],
            vec![at(1, 3, (0, 4)), at(2, 4, (1, 4))],
            vec![at(1, 3, (2, 4))],
            vec![at(0, 3, (0, 3)), at(1, 2, (1, 2))],
            vec![at(0, 2, (0, 2)), at(1, 4, (1, 4)), at(1, 4, (0, 4))],
        ];
        for period in [(1, 1), (1, 2), (3, 2)] {
            for steps in &cases {
                let grid = Layer::of(layer(period, steps.clone())).unwrap();
                grid_of(&grid);
                assert_eq!(
                    grid.is_in_order(),
                    as_places(&grid).is_in_order(),
                    "{period:?}"
                );
            }
        }
    }

    /// A grid makes the reduced time of a tick as exact division does,
    /// with a table of reductions or with a shift, for a tick of either
    /// sign.
    #[test]
    fn a_grid_makes_the_reduced_time_of_a_tick() {
        for ticks in [1, 3, 6, 12, 64, 100, 1000, 1024, 1025, 4096, 65_537] {
            let step = |end: i128| {
                SequenceStep::new(
                    Fraction::ZERO,
                    Fraction::new(1, end),
                    Value::F64(0.0),
                    Locations::new(),
                )
            };
            let layer = Layer::of(layer((1, 1), vec![step(ticks)])).unwrap();
            let grid = grid_of(&layer);
            assert_eq!(grid.ticks.get() as i128, ticks);
            for tick in (-5000..5000).chain([i128::from(i64::MAX), i128::from(i64::MIN)]) {
                assert_eq!(
                    grid.made(tick),
                    Fraction::new(tick, ticks),
                    "{tick}/{ticks}"
                );
            }
        }
    }

    /// The floor and the ceiling of a time in ticks are those of exact
    /// division, for a time on a tick, between two, and negative.
    #[test]
    fn a_time_has_its_floor_and_ceiling_in_ticks() {
        let layer = Layer::of(grid_layer((1, 1), 12, 0)).unwrap();
        let grid = grid_of(&layer);
        for denom in [1, 2, 3, 5, 7, 8, 12, 16, 24, 1 << 20] {
            for numer in -300..300 {
                let time = Fraction::new(numer, denom);
                let exact = time * Fraction::int(12);
                let (floor, ceil) = grid.ticks_at(time).unwrap();
                assert_eq!(Fraction::int(i128::from(floor)), exact.floor(), "{time:?}");
                assert_eq!(Fraction::int(i128::from(ceil)), exact.ceil(), "{time:?}");
            }
        }
        assert!(grid.ticks_at(Fraction::int(i128::MAX / 2)).is_none());
    }

    /// A source that holds one step in each period.
    struct OneStep;
    impl StepSource for OneStep {
        fn count(&self) -> usize {
            1
        }
        fn steps(
            &self,
            _: Fraction,
            end: Fraction,
            each: &mut dyn FnMut(SourceStep),
        ) -> Option<()> {
            each(SourceStep {
                begin: Fraction::ZERO,
                end,
                value: 0,
                locations: Locations::new(),
            });
            Some(())
        }
    }

    fn one_step_layer(source: &Arc<dyn StepSource>, value: f64) -> Layer {
        Layer::of_source(SourcedLayer {
            period: Fraction::ONE,
            values: Arc::new([Value::F64(value)]),
            in_cycles: true,
            steps: Arc::clone(source),
        })
    }

    /// The answer of `same` for two layers of one source does not change
    /// when a query reads the source of one of them.
    #[test]
    fn same_answers_alike_after_a_source_is_read() {
        let source: Arc<dyn StepSource> = Arc::new(OneStep);
        let (a, b) = (one_step_layer(&source, 1.0), one_step_layer(&source, 1.0));
        let listed = Layer::of(layer((1, 1), vec![step((0, 1), (1, 1), 1.0)])).unwrap();
        let same = |a: &Layer, b: &Layer| a.same(b, |a, b| a.same_data(b));
        let before = (
            same(&a, &b),
            same(&b, &a),
            same(&a, &listed),
            same(&listed, &a),
        );
        assert_eq!(before, (true, true, false, false));
        // A query reads the steps of `a` into a list.
        assert_eq!(a.listing().len(), 1);
        assert!(a.source().is_none(), "the source of a was read");
        let after = (
            same(&a, &b),
            same(&b, &a),
            same(&a, &listed),
            same(&listed, &a),
        );
        assert_eq!(after, before);
    }

    /// Two layers of one source with other values are not the same.
    #[test]
    fn same_tells_two_layers_of_one_source_with_other_values() {
        let source: Arc<dyn StepSource> = Arc::new(OneStep);
        let a = one_step_layer(&source, 1.0);
        let other = a.clone().valued(Arc::new([Value::F64(2.0)]));
        let alike = a.clone().valued(Arc::new([Value::F64(1.0)]));
        let same = |a: &Layer, b: &Layer| a.same(b, |a, b| a.same_data(b));
        assert!(!same(&a, &other));
        assert!(!same(&other, &a));
        assert!(same(&a, &alike));
    }

    /// A raw map with a `value` key under no name reads as the compose of a
    /// set onto it, which keeps every key of the map.
    #[test]
    fn a_raw_map_with_a_value_key_reads_as_the_plain_map() {
        let set = map_of(&[("s", text("piano")), ("gain", Value::F64(0.5))]);
        let raw = vec![map_of(&[("value", text("c3")), ("n", Value::F64(1.0))])];
        let layer = dressed_layer(raw, None, Some(set));
        let plain = made_plain(&layer);
        for (index, plain) in plain.iter().enumerate() {
            let (Some(Value::Object(shared)), Value::Object(plain)) = (layer.value(index), plain)
            else {
                panic!("step {index} is no map");
            };
            assert_one_map(&shared, plain, "a raw map with a value key");
            assert_eq!(shared.get("value"), Some(&text("c3")));
        }
    }
}
