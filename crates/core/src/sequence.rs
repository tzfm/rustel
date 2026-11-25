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

use crate::compose::{ComposeOp, is_op_closure, union_drops};
use crate::context_keys::ContextKeys;
use crate::value::{LayerCachedValues, LayerReadView, Table};
use crate::{
    Combine, Hap, HapMapFn, Locations, LookupFlow, MAX_QUERY_SPAN_CYCLES, Meeting, Node,
    OwnedHapMapFn, Pattern, QueryLimit, QueryThread, State, TimeSpan, Value, ValuePredicate,
    applied_hap, refuse, silence, without_query_signals,
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

    /// More locations, before the step's own, from an existing [`Locations`].
    pub fn locate_first_locations(&mut self, locations: &Locations) {
        if locations.is_empty() {
            return;
        }
        self.locations.prepend(locations);
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

/// One layer of [`sequence_of`] whose steps are already in cycles with
/// pre-deduplicated value indices.
pub struct IndexedLayer {
    pub period: Fraction,
    pub values: Arc<[Value]>,
    /// Whether each step lies in one cycle, in every period. When `false`,
    /// the layer checks its steps directly.
    pub in_cycles: bool,
    pub steps: Vec<SourceStep>,
}

/// A layer of a sequence, as its maker gives it.
pub enum GivenLayer {
    /// Each step, in a list.
    Listed(SequenceLayer),
    /// Each step, in cycles with pre-deduplicated value indices.
    Indexed(IndexedLayer),
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
    /// Rare fallback when a layer's period does not fit `(i16, 13-bit u16)`.
    WithPeriod(Arc<(Fraction, Steps)>),
    /// Rare lazy cache cell for non-direct layers or spilling dress contexts.
    Cached(Arc<(Steps, OnceLock<Arc<Table>>)>),
}

impl Steps {
    #[inline(always)]
    fn inner(&self) -> &Self {
        let mut cur = self;
        loop {
            match cur {
                Self::WithPeriod(pair) => cur = &pair.1,
                Self::Cached(pair) => cur = &pair.0,
                other => return other,
            }
        }
    }

    #[inline(always)]
    fn strip_cache(&mut self) {
        if let Self::Cached(pair) = self {
            *self = pair.0.clone();
        }
    }

    #[inline(always)]
    fn cache_cell(&self) -> Option<&OnceLock<Arc<Table>>> {
        match self {
            Self::Cached(pair) => Some(&pair.1),
            _ => None,
        }
    }

    fn ensure_fresh_cache(&mut self) {
        if let Self::Cached(pair) = self
            && Arc::get_mut(pair).is_some_and(|(_, cell)| cell.get().is_none())
        {
            return;
        }
        let inner = match &*self {
            Self::Cached(pair) => pair.0.clone(),
            other => other.clone(),
        };
        *self = Self::Cached(Arc::new((inner, OnceLock::new())));
    }
}

/// The source of the steps of a layer, and their list when a fold read
/// them: each layer that a fold makes of the same steps shares it.
struct Sourced {
    steps: Arc<dyn StepSource>,
    read: OnceLock<Listing>,
}

/// The steps of a layer in a list: on a grid when their times fit one,
/// otherwise each at its place.
/// Single-allocation grid storage for `1..=16` compact 8-byte steps with no
/// [`Spare`] sidecar. Uses thin `Arc` pointers so [`Listing`] and [`Steps`]
/// stay 16 bytes via enum niche folding, saving 24 bytes and one allocation
/// per grid.
pub(crate) struct InlineGrid<const N: usize> {
    ticks: NonZeroU32,
    base: u32,
    steps: [SmallGridStep; N],
}

/// Compact 16-bit multi-range location runs stored inline for up to 7 entries
/// (zero heap allocation, smaller total footprint than `Box<[u16]>` plus
/// allocator overhead) and spilled to `Box<[u16]>` when longer.
#[derive(Clone)]
pub(crate) enum SmallPackedRuns {
    Inline(u8, [u16; 7]),
    Heap(Box<[u16]>),
}

impl SmallPackedRuns {
    #[inline]
    fn from_slice(slice: &[u16]) -> Self {
        if slice.len() <= 7 {
            let mut arr = [0u16; 7];
            arr[..slice.len()].copy_from_slice(slice);
            Self::Inline(slice.len() as u8, arr)
        } else {
            Self::Heap(Box::from(slice))
        }
    }

    #[inline(always)]
    fn as_slice(&self) -> &[u16] {
        match self {
            Self::Inline(len, arr) => &arr[..usize::from(*len)],
            Self::Heap(boxed) => boxed,
        }
    }
}

impl std::ops::Deref for SmallPackedRuns {
    type Target = [u16];
    #[inline(always)]
    fn deref(&self) -> &[u16] {
        self.as_slice()
    }
}

/// Compact grid storage for `1..=16` steps carrying deduplicated 16-bit
/// multi-range location runs (`offset < 1024`, `len <= 30`) without `Box<Spare>`
/// or a separate `Box<[SmallGridStep]>`.
pub(crate) struct InlinePackedGrid<const N: usize> {
    ticks: NonZeroU32,
    base: u32,
    packed: SmallPackedRuns,
    steps: [SmallGridStep; N],
}

/// Compact grid storage for `> 16` steps carrying deduplicated 16-bit
/// multi-range location runs without `Box<Spare>`.
pub(crate) struct PackedSliceGrid {
    ticks: NonZeroU32,
    base: u32,
    steps: Box<[SmallGridStep]>,
    packed: SmallPackedRuns,
}

const SMALL_RUN_END: u16 = 1 << 15;
const SMALL_RUN_LEN_BITS: u16 = 5;
const SMALL_RUN_LEN_MASK: u16 = (1 << SMALL_RUN_LEN_BITS) - 1;
const SMALL_RUN_OFF_LIMIT: u32 = 1 << 10;

fn try_pack_runs_u16(runs: &[u32]) -> Option<SmallPackedRuns> {
    if runs.is_empty() {
        return None;
    }
    let mut small = SmallCopyBuf::<u16, 32>::new(0);
    for &word in runs {
        let entry = word & !PackedLoc::SPARE;
        let offset = entry >> PackedLoc::LENGTH_BITS;
        let len_plus_1 = entry & PackedLoc::LENGTH_MASK;
        if offset >= SMALL_RUN_OFF_LIMIT || len_plus_1 > u32::from(SMALL_RUN_LEN_MASK) {
            return None;
        }
        let end_bit = if (word & PackedLoc::SPARE) != 0 {
            SMALL_RUN_END
        } else {
            0
        };
        small.push(end_bit | ((offset as u16) << SMALL_RUN_LEN_BITS) | (len_plus_1 as u16));
    }
    if small.len() <= 7 {
        Some(SmallPackedRuns::from_slice(small.as_slice()))
    } else {
        Some(SmallPackedRuns::Heap(small.into_boxed_slice()))
    }
}

#[derive(Clone)]
pub(crate) enum CompactGrid {
    N1(Arc<InlineGrid<1>>),
    N2(Arc<InlineGrid<2>>),
    N3(Arc<InlineGrid<3>>),
    N4(Arc<InlineGrid<4>>),
    N5(Arc<InlineGrid<5>>),
    N6(Arc<InlineGrid<6>>),
    N7(Arc<InlineGrid<7>>),
    N8(Arc<InlineGrid<8>>),
    N9(Arc<InlineGrid<9>>),
    N10(Arc<InlineGrid<10>>),
    N11(Arc<InlineGrid<11>>),
    N12(Arc<InlineGrid<12>>),
    N13(Arc<InlineGrid<13>>),
    N14(Arc<InlineGrid<14>>),
    N15(Arc<InlineGrid<15>>),
    N16(Arc<InlineGrid<16>>),
    P1(Arc<InlinePackedGrid<1>>),
    P2(Arc<InlinePackedGrid<2>>),
    P3(Arc<InlinePackedGrid<3>>),
    P4(Arc<InlinePackedGrid<4>>),
    P5(Arc<InlinePackedGrid<5>>),
    P6(Arc<InlinePackedGrid<6>>),
    P7(Arc<InlinePackedGrid<7>>),
    P8(Arc<InlinePackedGrid<8>>),
    P9(Arc<InlinePackedGrid<9>>),
    P10(Arc<InlinePackedGrid<10>>),
    P11(Arc<InlinePackedGrid<11>>),
    P12(Arc<InlinePackedGrid<12>>),
    P13(Arc<InlinePackedGrid<13>>),
    P14(Arc<InlinePackedGrid<14>>),
    P15(Arc<InlinePackedGrid<15>>),
    P16(Arc<InlinePackedGrid<16>>),
    PMany(Arc<PackedSliceGrid>),
    /// Inline 1-step uncut grid packed into 12 bytes (0..=3 locations) with
    /// zero heap allocation, living flat inside [`CompactGrid`] so [`Listing`]
    /// and [`Steps`] stay 16 bytes.
    OneStep([u8; 12]),
    /// Inline 1-step uncut grid carrying a non-zero `u16` value index and
    /// 0..=2 locations with zero heap allocation.
    OneStepVal([u8; 12]),
}

const MAX_INLINE_GRID_LEN: usize = 16;

impl CompactGrid {
    fn from_parts(ticks: NonZeroU32, base: u32, steps: &[SmallGridStep]) -> Self {
        macro_rules! build {
            ($($n:literal => $var:ident),+) => {
                match steps.len() {
                    $(
                        $n => {
                            let mut arr = [SmallGridStep {
                                part_begin: 0,
                                part_end: 0,
                                value: 0,
                                loc: 0,
                            }; $n];
                            arr.copy_from_slice(steps);
                            Self::$var(Arc::new(InlineGrid {
                                ticks,
                                base,
                                steps: arr,
                            }))
                        }
                    )+
                    _ => unreachable!("compact grid length out of range"),
                }
            };
        }
        build!(
            1 => N1, 2 => N2, 3 => N3, 4 => N4,
            5 => N5, 6 => N6, 7 => N7, 8 => N8,
            9 => N9, 10 => N10, 11 => N11, 12 => N12,
            13 => N13, 14 => N14, 15 => N15, 16 => N16
        )
    }

    fn from_packed_inline(
        ticks: NonZeroU32,
        base: u32,
        steps: &[SmallGridStep],
        packed: SmallPackedRuns,
    ) -> Self {
        macro_rules! build {
            ($($n:literal => $var:ident),+) => {
                match steps.len() {
                    $(
                        $n => {
                            let mut arr = [SmallGridStep {
                                part_begin: 0,
                                part_end: 0,
                                value: 0,
                                loc: 0,
                            }; $n];
                            arr.copy_from_slice(steps);
                            Self::$var(Arc::new(InlinePackedGrid {
                                ticks,
                                base,
                                packed,
                                steps: arr,
                            }))
                        }
                    )+
                    _ => unreachable!("compact packed grid length out of range"),
                }
            };
        }
        build!(
            1 => P1, 2 => P2, 3 => P3, 4 => P4,
            5 => P5, 6 => P6, 7 => P7, 8 => P8,
            9 => P9, 10 => P10, 11 => P11, 12 => P12,
            13 => P13, 14 => P14, 15 => P15, 16 => P16
        )
    }

    fn from_packed_slice(
        ticks: NonZeroU32,
        base: u32,
        steps: Box<[SmallGridStep]>,
        packed: SmallPackedRuns,
    ) -> Self {
        Self::PMany(Arc::new(PackedSliceGrid {
            ticks,
            base,
            steps,
            packed,
        }))
    }

    #[inline(always)]
    pub(crate) fn parts(&self) -> (NonZeroU32, u32, &[SmallGridStep], Option<&[u16]>) {
        macro_rules! unpack {
            (plain: [$($n:ident),+], packed: [$($p:ident),+]) => {
                match self {
                    $(Self::$n(g) => (g.ticks, g.base, &g.steps[..], None),)+
                    $(Self::$p(g) => (g.ticks, g.base, &g.steps[..], Some(&g.packed[..])),)+
                    Self::PMany(g) => (g.ticks, g.base, &g.steps[..], Some(&g.packed[..])),
                    Self::OneStep(_) | Self::OneStepVal(_) => {
                        unreachable!("inline one-step grid handled before parts()")
                    }
                }
            };
        }
        unpack!(
            plain: [N1, N2, N3, N4, N5, N6, N7, N8, N9, N10, N11, N12, N13, N14, N15, N16],
            packed: [P1, P2, P3, P4, P5, P6, P7, P8, P9, P10, P11, P12, P13, P14, P15, P16]
        )
    }

    #[inline(always)]
    pub(crate) fn ptr_hash(&self, state: &mut impl std::hash::Hasher) {
        use std::hash::Hash;
        macro_rules! hash_ptr {
            ($($var:ident),+) => {
                match self {
                    $(Self::$var(g) => (Arc::as_ptr(g) as usize).hash(state),)+
                    Self::OneStep(buf) => {
                        0xffu8.hash(state);
                        buf.hash(state);
                    }
                    Self::OneStepVal(buf) => {
                        0xfeu8.hash(state);
                        buf.hash(state);
                    }
                }
            };
        }
        hash_ptr!(
            N1, N2, N3, N4, N5, N6, N7, N8, N9, N10, N11, N12, N13, N14, N15, N16, P1, P2, P3, P4,
            P5, P6, P7, P8, P9, P10, P11, P12, P13, P14, P15, P16, PMany
        )
    }

    #[inline(always)]
    pub(crate) fn arc_ptr(&self) -> usize {
        macro_rules! get_ptr {
            ($($var:ident),+) => {
                match self {
                    $(Self::$var(a) => Arc::as_ptr(a) as usize,)+
                    Self::OneStep(_) | Self::OneStepVal(_) => 0,
                }
            };
        }
        get_ptr!(
            N1, N2, N3, N4, N5, N6, N7, N8, N9, N10, N11, N12, N13, N14, N15, N16, P1, P2, P3, P4,
            P5, P6, P7, P8, P9, P10, P11, P12, P13, P14, P15, P16, PMany
        )
    }

    #[inline(always)]
    pub(crate) fn ptr_eq(&self, other: &Self) -> bool {
        macro_rules! same_ptr {
            ($($var:ident),+) => {
                match (self, other) {
                    $((Self::$var(a), Self::$var(b)) => Arc::ptr_eq(a, b),)+
                    (Self::OneStep(a), Self::OneStep(b)) => a == b,
                    (Self::OneStepVal(a), Self::OneStepVal(b)) => a == b,
                    _ => false,
                }
            };
        }
        same_ptr!(
            N1, N2, N3, N4, N5, N6, N7, N8, N9, N10, N11, N12, N13, N14, N15, N16, P1, P2, P3, P4,
            P5, P6, P7, P8, P9, P10, P11, P12, P13, P14, P15, P16, PMany
        )
    }

    fn parts_mut(&mut self) -> Option<(&mut NonZeroU32, &mut u32, &mut [SmallGridStep])> {
        macro_rules! unpack_mut {
            (plain: [$($p:ident),+], packed: [$($k:ident),+]) => {
                match self {
                    $(Self::$p(g) => {
                        let g = Arc::get_mut(g)?;
                        Some((&mut g.ticks, &mut g.base, &mut g.steps[..]))
                    })+
                    $(Self::$k(g) => {
                        let g = Arc::get_mut(g)?;
                        Some((&mut g.ticks, &mut g.base, &mut g.steps[..]))
                    })+
                    Self::PMany(g) => {
                        let g = Arc::get_mut(g)?;
                        Some((&mut g.ticks, &mut g.base, &mut g.steps[..]))
                    }
                    Self::OneStep(_) | Self::OneStepVal(_) => None,
                }
            };
        }
        unpack_mut!(
            plain: [N1, N2, N3, N4, N5, N6, N7, N8, N9, N10, N11, N12, N13, N14, N15, N16],
            packed: [P1, P2, P3, P4, P5, P6, P7, P8, P9, P10, P11, P12, P13, P14, P15, P16]
        )
    }

    /// Rescale this grid by `factor` in place when unshared and every rescaled
    /// tick fits the existing compact layout. Returns `Some(true)` on success,
    /// `None` when the caller should fall back to rebuilding a new listing.
    fn try_faster_in_place(&mut self, factor: Fraction) -> Option<()> {
        let f_num = u32::try_from(factor.numer()).ok()?;
        let f_den = u32::try_from(factor.denom()).ok()?;
        if f_num == 0 || f_den == 0 {
            return None;
        }
        if let Self::OneStep(buf) | Self::OneStepVal(buf) = self {
            let base_ticks = u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
            let span_loc = u32::from_ne_bytes([buf[4], buf[5], buf[6], buf[7]]);
            let old_ticks = base_ticks >> 25;
            let b = span_loc & 0xff;
            let e = (span_loc >> 8) & 0xff;
            let g0 = gcd(u64::from(old_ticks), u64::from(f_den)) as u32;
            let ticks_1 = old_ticks / g0;
            let mul = f_den / g0;
            let g1 = gcd(gcd(u64::from(f_num), u64::from(b)), u64::from(e));
            let new_ticks_u32 =
                u32::try_from(u64::from(ticks_1).checked_mul(u64::from(f_num))? / g1).ok()?;
            if new_ticks_u32 == 0 || new_ticks_u32 > 127 {
                return None;
            }
            let new_b = u8::try_from((u64::from(b) / g1).checked_mul(u64::from(mul))?).ok()?;
            let new_e = u8::try_from((u64::from(e) / g1).checked_mul(u64::from(mul))?).ok()?;
            let new_base_ticks = (base_ticks & 0x00ff_ffff) | (1 << 24) | (new_ticks_u32 << 25);
            let new_span_loc =
                (span_loc & 0xffff_0000) | u32::from(new_b) | (u32::from(new_e) << 8);
            buf[0..4].copy_from_slice(&new_base_ticks.to_ne_bytes());
            buf[4..8].copy_from_slice(&new_span_loc.to_ne_bytes());
            return Some(());
        }
        let (ticks, base, steps) = self.parts_mut()?;
        let old_ticks = ticks.get();
        let g0 = gcd(u64::from(old_ticks), u64::from(f_den)) as u32;
        let ticks_1 = old_ticks / g0;
        let mul = f_den / g0;
        let mut g1 = u64::from(f_num);
        if g1 > 1 {
            for step in steps.iter() {
                g1 = gcd(g1, u64::from(step.part_begin));
                g1 = gcd(g1, u64::from(step.part_end));
                if g1 == 1 {
                    break;
                }
            }
        }
        let ticks_2 = u64::from(ticks_1).checked_mul(u64::from(f_num))? / g1;
        let new_ticks_u32 = u32::try_from(ticks_2).ok()?;
        if i32::try_from(new_ticks_u32).is_err() {
            return None;
        }
        let new_ticks = NonZeroU32::new(new_ticks_u32)?;
        if mul != 1 || g1 != 1 {
            let mul_u64 = u64::from(mul);
            // Validate every rescaled tick fits `u16` before mutating.
            for step in steps.iter() {
                let nb = (u64::from(step.part_begin) / g1).checked_mul(mul_u64)?;
                let ne = (u64::from(step.part_end) / g1).checked_mul(mul_u64)?;
                if u16::try_from(nb).is_err() || u16::try_from(ne).is_err() {
                    return None;
                }
            }
            for step in steps.iter_mut() {
                step.part_begin = ((u64::from(step.part_begin) / g1) * mul_u64) as u16;
                step.part_end = ((u64::from(step.part_end) / g1) * mul_u64) as u16;
            }
        }
        *ticks = new_ticks;
        *base |= Grid::SMALL;
        Some(())
    }
}

/// Borrowed view over packed multi-range location runs.
#[derive(Clone, Copy)]
enum PackedRunsRef<'a> {
    None,
    Small(&'a [u16]),
    Wide(&'a [u32]),
}

/// Owned input for [`Listing::from_parts`] avoiding an intermediate `Box<Spare>`
/// when a grid can stay in [`CompactGrid`].
enum PackedRunsInput {
    None,
    Small(SmallPackedRuns),
    Wide(Box<[u32]>),
}

/// Borrowed view over any grid representation (`OneStep`, `CompactGrid`, or `Grid`).
#[derive(Clone, Copy)]
struct GridView<'a> {
    ticks: NonZeroU32,
    base: u32,
    steps: GridStepsRef<'a>,
    packed: PackedRunsRef<'a>,
    rare: Option<&'a SpareRare>,
}

#[derive(Clone)]
enum Listing {
    Places(Arc<Listed>),
    /// Inline 1-step grid or single-allocation `1..=16` / packed grid.
    CompactGrid(CompactGrid),
    Grid(Arc<Grid>),
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<Listing>() == 16);

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
        Self::of_vec(&mut raws)
    }

    /// Build a [`Listing`] from a single [`Raw`] step without allocating a
    /// `Vec<Raw>`.
    fn of_single(mut raw: Raw) -> Self {
        if let Some(listing) = Grid::build_listing(std::slice::from_mut(&mut raw)) {
            return listing;
        }
        let small = |span: &TimeSpan| {
            i32::try_from(span.begin.denom()).is_ok() && i32::try_from(span.end.denom()).is_ok()
        };
        let small_denominators = small(&raw.whole) && raw.part.as_ref().is_none_or(small);
        Self::Places(Arc::new(Listed {
            places: Box::from([raw.place()]),
            small_denominators,
        }))
    }

    /// Build a [`Listing`] from `raws`, draining `raws` when it falls back to
    /// `Places` so callers can recycle the buffer.
    fn of_vec(raws: &mut Vec<Raw>) -> Self {
        if let Some(listing) = Grid::build_listing(raws) {
            return listing;
        }
        let small = |span: &TimeSpan| {
            i32::try_from(span.begin.denom()).is_ok() && i32::try_from(span.end.denom()).is_ok()
        };
        let small_denominators = raws
            .iter()
            .all(|raw| small(&raw.whole) && raw.part.as_ref().is_none_or(small));
        Self::Places(Arc::new(Listed {
            places: raws.drain(..).map(Raw::place).collect(),
            small_denominators,
        }))
    }

    #[inline]
    fn from_parts(
        ticks: NonZeroU32,
        base: u32,
        steps: GridSteps,
        packed: PackedRunsInput,
        rare: Option<Box<SpareRare>>,
    ) -> Self {
        if rare.is_none() {
            match packed {
                PackedRunsInput::None => {
                    if let GridSteps::Inline(len, ref arr) = steps {
                        let slice = &arr[..usize::from(len)];
                        let raw_base = base & !Grid::SMALL;
                        if slice.len() == 1
                            && (slice[0].loc & SmallGridStep::SPARE) == 0
                            && raw_base < (1 << 24)
                            && ticks.get() <= 127
                            && slice[0].part_begin <= 255
                            && slice[0].part_end <= 255
                        {
                            let small_bit = u32::from(base & Grid::SMALL != 0) << 24;
                            let base_ticks = raw_base | small_bit | (ticks.get() << 25);
                            let span_loc = u32::from(slice[0].part_begin)
                                | (u32::from(slice[0].part_end) << 8)
                                | (u32::from(slice[0].loc) << 16);
                            let mut buf = [0u8; 12];
                            buf[0..4].copy_from_slice(&base_ticks.to_ne_bytes());
                            buf[4..8].copy_from_slice(&span_loc.to_ne_bytes());
                            if slice[0].value == 0 {
                                return Self::CompactGrid(CompactGrid::OneStep(buf));
                            }
                            buf[8..10].copy_from_slice(&slice[0].value.to_ne_bytes());
                            return Self::CompactGrid(CompactGrid::OneStepVal(buf));
                        }
                        if !slice.is_empty() {
                            return Self::CompactGrid(CompactGrid::from_parts(ticks, base, slice));
                        }
                    }
                    let small_steps = match steps {
                        GridSteps::Inline(len, arr) => Box::from(&arr[..usize::from(len)]),
                        GridSteps::Small(small) => small,
                        GridSteps::Wide(_) => Box::from([]),
                    };
                    return Self::Grid(Arc::new(Grid {
                        ticks,
                        base,
                        small_steps,
                        side: None,
                    }));
                }
                PackedRunsInput::Small(packed_small) => {
                    match steps {
                        GridSteps::Inline(len, arr) if len > 0 => {
                            let slice = &arr[..usize::from(len)];
                            let raw_base = base & !Grid::SMALL;
                            if slice.len() == 1
                                && slice[0].loc == SmallGridStep::SPARE
                                && raw_base < (1 << 24)
                                && ticks.get() <= 127
                                && slice[0].part_begin <= 255
                                && slice[0].part_end <= 255
                            {
                                if slice[0].value == 0 && (2..=3).contains(&packed_small.len()) {
                                    let small_bit = u32::from(base & Grid::SMALL != 0) << 24;
                                    let base_ticks = raw_base | small_bit | (ticks.get() << 25);
                                    let span_loc = u32::from(slice[0].part_begin)
                                        | (u32::from(slice[0].part_end) << 8)
                                        | (u32::from(packed_small[0]) << 16);
                                    let loc2 = packed_small.get(2).copied().unwrap_or(0);
                                    let mut buf = [0u8; 12];
                                    buf[0..4].copy_from_slice(&base_ticks.to_ne_bytes());
                                    buf[4..8].copy_from_slice(&span_loc.to_ne_bytes());
                                    buf[8..12].copy_from_slice(
                                        &(u32::from(packed_small[1]) | (u32::from(loc2) << 16))
                                            .to_ne_bytes(),
                                    );
                                    return Self::CompactGrid(CompactGrid::OneStep(buf));
                                }
                                if slice[0].value != 0 && packed_small.len() == 2 {
                                    let small_bit = u32::from(base & Grid::SMALL != 0) << 24;
                                    let base_ticks = raw_base | small_bit | (ticks.get() << 25);
                                    let span_loc = u32::from(slice[0].part_begin)
                                        | (u32::from(slice[0].part_end) << 8)
                                        | (u32::from(packed_small[0]) << 16);
                                    let mut buf = [0u8; 12];
                                    buf[0..4].copy_from_slice(&base_ticks.to_ne_bytes());
                                    buf[4..8].copy_from_slice(&span_loc.to_ne_bytes());
                                    buf[8..10].copy_from_slice(&slice[0].value.to_ne_bytes());
                                    buf[10..12].copy_from_slice(&packed_small[1].to_ne_bytes());
                                    return Self::CompactGrid(CompactGrid::OneStepVal(buf));
                                }
                            }
                            return Self::CompactGrid(CompactGrid::from_packed_inline(
                                ticks,
                                base,
                                slice,
                                packed_small,
                            ));
                        }
                        GridSteps::Small(small) => {
                            return Self::CompactGrid(CompactGrid::from_packed_slice(
                                ticks,
                                base,
                                small,
                                packed_small,
                            ));
                        }
                        GridSteps::Inline(_, _) | GridSteps::Wide(_) => {
                            // Unpack rare empty/wide case below.
                            let wide: Box<[u32]> = packed_small
                                .iter()
                                .map(|&w| {
                                    let entry = w & !SMALL_RUN_END;
                                    let off = u32::from(entry >> SMALL_RUN_LEN_BITS);
                                    let len_p1 = u32::from(entry & SMALL_RUN_LEN_MASK);
                                    let end = if (w & SMALL_RUN_END) != 0 {
                                        PackedLoc::SPARE
                                    } else {
                                        0
                                    };
                                    end | (off << PackedLoc::LENGTH_BITS) | len_p1
                                })
                                .collect();
                            return Self::Grid(Arc::new(Grid {
                                ticks,
                                base,
                                small_steps: Box::from([]),
                                side: Some(Box::new(Spare {
                                    packed: wide,
                                    rare: None,
                                })),
                            }));
                        }
                    }
                }
                PackedRunsInput::Wide(packed_wide) => {
                    let small_steps = match steps {
                        GridSteps::Inline(len, arr) => Box::from(&arr[..usize::from(len)]),
                        GridSteps::Small(small) => small,
                        GridSteps::Wide(_) => Box::from([]),
                    };
                    return Self::Grid(Arc::new(Grid {
                        ticks,
                        base,
                        small_steps,
                        side: Some(Box::new(Spare {
                            packed: packed_wide,
                            rare: None,
                        })),
                    }));
                }
            }
        }
        let packed_wide = match packed {
            PackedRunsInput::None => Box::from([]),
            PackedRunsInput::Wide(wide) => wide,
            PackedRunsInput::Small(small) => small
                .iter()
                .map(|&w| {
                    let entry = w & !SMALL_RUN_END;
                    let off = u32::from(entry >> SMALL_RUN_LEN_BITS);
                    let len_p1 = u32::from(entry & SMALL_RUN_LEN_MASK);
                    let end = if (w & SMALL_RUN_END) != 0 {
                        PackedLoc::SPARE
                    } else {
                        0
                    };
                    end | (off << PackedLoc::LENGTH_BITS) | len_p1
                })
                .collect(),
        };
        let small_steps = match steps {
            GridSteps::Inline(len, arr) => Box::from(&arr[..usize::from(len)]),
            GridSteps::Small(small) => small,
            GridSteps::Wide(_) => Box::from([]),
        };
        Self::Grid(Arc::new(Grid {
            ticks,
            base,
            small_steps,
            side: Some(Box::new(Spare {
                packed: packed_wide,
                rare,
            })),
        }))
    }

    #[inline]
    fn with_grid_view<R>(&self, f: impl FnOnce(GridView<'_>) -> R) -> Option<R> {
        let one_step: [SmallGridStep; 1];
        let packed_buf: [u16; 3];
        let view = match self {
            Self::Places(_) => return None,
            Self::CompactGrid(CompactGrid::OneStep(buf)) => {
                let base_ticks = u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
                let span_loc = u32::from_ne_bytes([buf[4], buf[5], buf[6], buf[7]]);
                let extra = u32::from_ne_bytes([buf[8], buf[9], buf[10], buf[11]]);
                let raw_base = base_ticks & 0x00ff_ffff;
                let small_bit = if (base_ticks & (1 << 24)) != 0 {
                    Grid::SMALL
                } else {
                    0
                };
                let ticks = NonZeroU32::new(base_ticks >> 25).expect("non-zero packed ticks");
                let loc0 = (span_loc >> 16) as u16;
                let (step_loc, packed_len) = if extra != 0 {
                    let loc1 = (extra & 0xffff) as u16;
                    let loc2 = (extra >> 16) as u16;
                    let len = if loc2 != 0 { 3 } else { 2 };
                    packed_buf = [loc0, loc1, loc2];
                    (SmallGridStep::SPARE, len)
                } else {
                    packed_buf = [0, 0, 0];
                    (loc0, 0)
                };
                one_step = [SmallGridStep {
                    part_begin: (span_loc & 0xff) as u16,
                    part_end: ((span_loc >> 8) & 0xff) as u16,
                    value: 0,
                    loc: step_loc,
                }];
                let packed = if packed_len > 0 {
                    PackedRunsRef::Small(&packed_buf[..packed_len])
                } else {
                    PackedRunsRef::None
                };
                GridView {
                    ticks,
                    base: raw_base | small_bit,
                    steps: GridStepsRef::Small(&one_step),
                    packed,
                    rare: None,
                }
            }
            Self::CompactGrid(CompactGrid::OneStepVal(buf)) => {
                let base_ticks = u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
                let span_loc = u32::from_ne_bytes([buf[4], buf[5], buf[6], buf[7]]);
                let value = u16::from_ne_bytes([buf[8], buf[9]]);
                let loc1 = u16::from_ne_bytes([buf[10], buf[11]]);
                let raw_base = base_ticks & 0x00ff_ffff;
                let small_bit = if (base_ticks & (1 << 24)) != 0 {
                    Grid::SMALL
                } else {
                    0
                };
                let ticks = NonZeroU32::new(base_ticks >> 25).expect("non-zero packed ticks");
                let loc0 = (span_loc >> 16) as u16;
                let (step_loc, packed_len) = if loc1 != 0 {
                    packed_buf = [loc0, loc1, 0];
                    (SmallGridStep::SPARE, 2)
                } else {
                    packed_buf = [0, 0, 0];
                    (loc0, 0)
                };
                one_step = [SmallGridStep {
                    part_begin: (span_loc & 0xff) as u16,
                    part_end: ((span_loc >> 8) & 0xff) as u16,
                    value,
                    loc: step_loc,
                }];
                let packed = if packed_len > 0 {
                    PackedRunsRef::Small(&packed_buf[..packed_len])
                } else {
                    PackedRunsRef::None
                };
                GridView {
                    ticks,
                    base: raw_base | small_bit,
                    steps: GridStepsRef::Small(&one_step),
                    packed,
                    rare: None,
                }
            }
            Self::CompactGrid(grid) => {
                let (ticks, base, steps, packed) = grid.parts();
                GridView {
                    ticks,
                    base,
                    steps: GridStepsRef::Small(steps),
                    packed: match packed {
                        Some(small) => PackedRunsRef::Small(small),
                        None => PackedRunsRef::None,
                    },
                    rare: None,
                }
            }
            Self::Grid(grid) => grid.view(),
        };
        Some(f(view))
    }

    fn len(&self) -> usize {
        match self {
            Self::Places(listed) => listed.places.len(),
            Self::CompactGrid(CompactGrid::OneStep(_) | CompactGrid::OneStepVal(_)) => 1,
            Self::CompactGrid(grid) => grid.parts().2.len(),
            Self::Grid(grid) => grid.steps_ref().len(),
        }
    }

    fn part(&self, index: usize) -> TimeSpan {
        match self {
            Self::Places(listed) => listed.places[index].part(),
            _ => self
                .with_grid_view(|view| view.part(index))
                .expect("grid listing"),
        }
    }

    fn whole(&self, index: usize) -> TimeSpan {
        match self {
            Self::Places(listed) => listed.places[index].whole,
            _ => self
                .with_grid_view(|view| view.whole(index))
                .expect("grid listing"),
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
            _ => self
                .with_grid_view(|view| view.times(index))
                .expect("grid listing"),
        }
    }

    /// Whether the step holds a part of its own.
    fn is_cut(&self, index: usize) -> bool {
        match self {
            Self::Places(listed) => listed.places[index].part.is_some(),
            Self::CompactGrid(_) => false,
            Self::Grid(grid) => grid.view().cut(index).is_some(),
        }
    }

    fn value(&self, index: usize) -> u32 {
        match self {
            Self::Places(listed) => listed.places[index].value,
            Self::CompactGrid(CompactGrid::OneStep(_)) => 0,
            Self::CompactGrid(CompactGrid::OneStepVal(buf)) => {
                u32::from(u16::from_ne_bytes([buf[8], buf[9]]))
            }
            Self::CompactGrid(grid) => u32::from(grid.parts().2[index].value),
            Self::Grid(grid) => grid.steps_ref().get(index).value,
        }
    }

    fn locations(&self, index: usize) -> Locations {
        match self {
            Self::Places(listed) => listed.places[index].locations.clone(),
            _ => self
                .with_grid_view(|view| view.locations(index))
                .expect("grid listing"),
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
            Self::CompactGrid(CompactGrid::OneStep(buf) | CompactGrid::OneStepVal(buf)) => {
                u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]) & (1 << 24) != 0
            }
            Self::CompactGrid(grid) => grid.parts().1 & Grid::SMALL != 0,
            Self::Grid(grid) => grid.small_denominators(),
        }
    }
}

/// The places of one layer and whether their times fit a restart at a small offset.
struct Listed {
    places: Box<[Place]>,
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

/// Compact 8-byte step on a [`Grid`] when ticks, value index, and location fit
/// 16 bits. Halves step-array memory for typical layers.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct SmallGridStep {
    part_begin: u16,
    part_end: u16,
    value: u16,
    /// `0` is no location. Bit 15 = 0: inline range `(offset << 6) | (len + 1)`
    /// (`offset < 512`, `len < 63`). Bit 15 = 1, bit 14 = 0: spare index.
    /// Bit 15 = 1, bit 14 = 1: wide index.
    loc: u16,
}

impl SmallGridStep {
    #[inline]
    pub(crate) fn hash_intern(&self, state: &mut impl std::hash::Hasher) {
        use std::hash::Hash;
        let packed = u64::from(self.part_begin)
            | (u64::from(self.part_end) << 16)
            | (u64::from(self.value) << 32)
            | (u64::from(self.loc) << 48);
        packed.hash(state);
    }
    const SPARE: u16 = 1 << 15;
    const WIDE: u16 = 1 << 14;
    const INDEX_MASK: u16 = Self::WIDE - 1;
    const LENGTH_BITS: u32 = 6;
    const LENGTH_MASK: u16 = (1 << Self::LENGTH_BITS) - 1;
    const OFFSET_LIMIT: u32 = 1 << 9;

    #[inline]
    fn from_wide(step: GridStep) -> Option<Self> {
        let part_begin = u16::try_from(step.part_begin).ok()?;
        let part_end = u16::try_from(step.part_end).ok()?;
        let value = u16::try_from(step.value).ok()?;
        let raw = step.loc.0;
        let loc = if raw == 0 {
            0
        } else if raw & PackedLoc::SPARE != 0 {
            let idx = raw & PackedLoc::INDEX_MASK;
            if idx > u32::from(Self::INDEX_MASK) {
                return None;
            }
            let wide_bit = if raw & PackedLoc::WIDE != 0 {
                Self::WIDE
            } else {
                0
            };
            Self::SPARE | wide_bit | (idx as u16)
        } else {
            let offset = raw >> PackedLoc::LENGTH_BITS;
            let len_plus_1 = raw & PackedLoc::LENGTH_MASK;
            if offset >= Self::OFFSET_LIMIT || len_plus_1 > u32::from(Self::LENGTH_MASK) {
                return None;
            }
            ((offset as u16) << Self::LENGTH_BITS) | (len_plus_1 as u16)
        };
        Some(Self {
            part_begin,
            part_end,
            value,
            loc,
        })
    }

    #[inline]
    fn to_wide(self) -> GridStep {
        let loc = if self.loc == 0 {
            PackedLoc(0)
        } else if self.loc & Self::SPARE != 0 {
            let idx = u32::from(self.loc & Self::INDEX_MASK);
            let wide_bit = if self.loc & Self::WIDE != 0 {
                PackedLoc::WIDE
            } else {
                0
            };
            PackedLoc(PackedLoc::SPARE | wide_bit | idx)
        } else {
            let offset = u32::from(self.loc >> Self::LENGTH_BITS);
            let len_plus_1 = u32::from(self.loc & Self::LENGTH_MASK);
            PackedLoc((offset << PackedLoc::LENGTH_BITS) | len_plus_1)
        };
        GridStep {
            part_begin: u32::from(self.part_begin),
            part_end: u32::from(self.part_end),
            value: u32::from(self.value),
            loc,
        }
    }
}

/// Borrowed view of a [`Grid`]'s steps: 8-byte compact steps inline on
/// [`Grid`], or 16-byte wide steps spilled into [`SpareRare`].
#[derive(Clone, Copy)]
enum GridStepsRef<'a> {
    Small(&'a [SmallGridStep]),
    Wide(&'a [GridStep]),
}

impl<'a> GridStepsRef<'a> {
    #[inline]
    fn len(self) -> usize {
        match self {
            Self::Small(s) => s.len(),
            Self::Wide(w) => w.len(),
        }
    }

    #[inline]
    fn get(self, index: usize) -> GridStep {
        match self {
            Self::Small(s) => s[index].to_wide(),
            Self::Wide(w) => w[index],
        }
    }

    #[inline]
    fn times_of(self, index: usize) -> (u32, u32) {
        match self {
            Self::Small(s) => (u32::from(s[index].part_begin), u32::from(s[index].part_end)),
            Self::Wide(w) => (w[index].part_begin, w[index].part_end),
        }
    }

    #[inline]
    fn has_no_locations(self) -> bool {
        match self {
            Self::Small(s) => s.iter().all(|step| step.loc == 0),
            Self::Wide(w) => w.iter().all(|step| step.loc.0 == 0),
        }
    }
}

/// Owned build result before storing small steps directly on [`Listing`] /
/// [`Grid`] and spilling wide steps into [`Spare`].
enum GridSteps {
    /// Up to [`MAX_INLINE_GRID_LEN`] compact steps kept on the stack so
    /// `OneStep` and `CompactGrid` avoid an intermediate `Box` allocation.
    Inline(u8, [SmallGridStep; MAX_INLINE_GRID_LEN]),
    Small(Box<[SmallGridStep]>),
    Wide(Box<[GridStep]>),
}

impl GridSteps {
    const ZERO_SMALL: SmallGridStep = SmallGridStep {
        part_begin: 0,
        part_end: 0,
        value: 0,
        loc: 0,
    };

    /// Collect `len` steps produced by `make`, choosing compact 8-byte storage
    /// when every step fits. Uses a stack buffer for up to 64 steps so the
    /// common case performs zero intermediate heap allocations.
    fn build(len: usize, mut make: impl FnMut(usize) -> Option<GridStep>) -> Option<Self> {
        const STACK: usize = 64;
        if len <= MAX_INLINE_GRID_LEN {
            let mut small = [Self::ZERO_SMALL; MAX_INLINE_GRID_LEN];
            for i in 0..len {
                let step = make(i)?;
                if let Some(s) = SmallGridStep::from_wide(step) {
                    small[i] = s;
                } else {
                    let mut wide = Vec::with_capacity(len);
                    wide.extend(small[..i].iter().map(|s| s.to_wide()));
                    wide.push(step);
                    for j in (i + 1)..len {
                        wide.push(make(j)?);
                    }
                    return Some(Self::Wide(wide.into_boxed_slice()));
                }
            }
            return Some(Self::Inline(len as u8, small));
        }
        if len <= STACK {
            let mut small = [Self::ZERO_SMALL; STACK];
            for i in 0..len {
                let step = make(i)?;
                if let Some(s) = SmallGridStep::from_wide(step) {
                    small[i] = s;
                } else {
                    let mut wide = Vec::with_capacity(len);
                    wide.extend(small[..i].iter().map(|s| s.to_wide()));
                    wide.push(step);
                    for j in (i + 1)..len {
                        wide.push(make(j)?);
                    }
                    return Some(Self::Wide(wide.into_boxed_slice()));
                }
            }
            return Some(Self::Small(Box::from(&small[..len])));
        }
        let mut small = Vec::with_capacity(len);
        for i in 0..len {
            let step = make(i)?;
            if let Some(s) = SmallGridStep::from_wide(step) {
                small.push(s);
            } else {
                let mut wide = Vec::with_capacity(len);
                wide.extend(small.into_iter().map(SmallGridStep::to_wide));
                wide.push(step);
                for j in (i + 1)..len {
                    wide.push(make(j)?);
                }
                return Some(Self::Wide(wide.into_boxed_slice()));
            }
        }
        Some(Self::Small(small.into_boxed_slice()))
    }
}

/// The locations of a step in 4 bytes. `0` is no location. A set top bit
/// is a place in the spare list. Otherwise one range: its begin from the
/// base of the grid in the high 20 bits and its length plus one in the low
/// 11, so a range of no length stays apart from no range.
#[derive(Clone, Copy, PartialEq, Eq)]
struct PackedLoc(u32);

impl PackedLoc {
    const SPARE: u32 = 1 << 31;
    const WIDE: u32 = 1 << 30;
    const WIDE_MASK: u32 = Self::SPARE | Self::WIDE;
    const INDEX_MASK: u32 = Self::WIDE - 1;
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
    #[inline]
    fn range(base: u32, (begin, end): (usize, usize)) -> Option<Self> {
        let offset = u32::try_from(begin).ok()?.checked_sub(base)?;
        let length = u32::try_from(end.checked_sub(begin)?).ok()?;
        (offset < Self::OFFSET_LIMIT && length < Self::LENGTH_MASK)
            .then(|| Self(offset << Self::LENGTH_BITS | (length + 1)))
    }

    #[inline]
    fn range_raw(base: u32, begin: u32, length: u32) -> Option<Self> {
        let offset = begin.checked_sub(base)?;
        (offset < Self::OFFSET_LIMIT && length < Self::LENGTH_MASK)
            .then(|| Self((offset << Self::LENGTH_BITS) | (length + 1)))
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

/// Compact 6-byte cut step when index and whole ticks fit in `u16`.
#[derive(Clone, Copy, PartialEq, Eq)]
struct SmallCut {
    index: u16,
    begin: u16,
    end: u16,
}

impl SmallCut {
    #[inline]
    fn from_wide(cut: Cut) -> Option<Self> {
        Some(Self {
            index: u16::try_from(cut.index).ok()?,
            begin: u16::try_from(cut.begin).ok()?,
            end: u16::try_from(cut.end).ok()?,
        })
    }

    #[inline]
    fn to_wide(self) -> Cut {
        Cut {
            index: u32::from(self.index),
            begin: u32::from(self.begin),
            end: u32::from(self.end),
        }
    }
}

const _: () = assert!(std::mem::size_of::<SmallCut>() == 6);

#[derive(Clone, Copy)]
enum CutsSlice<'a> {
    Small(&'a [SmallCut]),
    Wide(&'a [Cut]),
}

impl<'a> CutsSlice<'a> {
    const EMPTY: Self = Self::Small(&[]);

    #[inline]
    fn len(self) -> usize {
        match self {
            Self::Small(s) => s.len(),
            Self::Wide(w) => w.len(),
        }
    }

    #[inline]
    fn get(self, at: usize) -> Option<Cut> {
        match self {
            Self::Small(s) => s.get(at).copied().map(SmallCut::to_wide),
            Self::Wide(w) => w.get(at).copied(),
        }
    }

    #[inline]
    fn starting_at(self, first: usize) -> Self {
        match self {
            Self::Small(s) => {
                let at = s.partition_point(|cut| usize::from(cut.index) < first);
                Self::Small(&s[at..])
            }
            Self::Wide(w) => {
                let at = w.partition_point(|cut| (cut.index as usize) < first);
                Self::Wide(&w[at..])
            }
        }
    }

    #[inline]
    fn split_first_at(&mut self, index: usize) -> Option<Cut> {
        match self {
            Self::Small(s) => {
                let (&cut, rest) = s.split_first()?;
                if usize::from(cut.index) == index {
                    *s = rest;
                    Some(cut.to_wide())
                } else {
                    None
                }
            }
            Self::Wide(w) => {
                let (&cut, rest) = w.split_first()?;
                if cut.index as usize == index {
                    *w = rest;
                    Some(cut)
                } else {
                    None
                }
            }
        }
    }

    #[inline]
    fn same(self, other: Self) -> bool {
        if self.len() != other.len() {
            return false;
        }
        (0..self.len()).all(|i| self.get(i) == other.get(i))
    }
}

/// The steps of a layer on an integer grid of `1 / ticks` cycles, kept
/// as ticks, when each time of them fits in 32 bits.
pub(crate) struct Grid {
    /// The ticks in a cycle: the least common multiple of the denominators.
    ticks: NonZeroU32,
    /// The begin that the packed ranges count from, below 2^31. The top bit
    /// is [`Listing::small_denominators`].
    base: u32,
    /// Compact 8-byte steps stored inline; empty when wide 16-byte steps live
    /// in `side.rare`.
    small_steps: Box<[SmallGridStep]>,
    /// What most grids have none of: multi-range packed locations, cuts, wide
    /// locations, and wide steps.
    side: Option<Box<Spare>>,
}

const _: () = assert!(std::mem::size_of::<SmallGridStep>() == 8);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<Grid>() == 32);

#[derive(Clone)]
struct SpareRareFull {
    cuts: Box<[Cut]>,
    wide: Box<[Locations]>,
    wide_steps: Box<[GridStep]>,
}

/// Rare cuts, wide locations, and wide steps kept compact behind one 24-byte
/// enum so common power-of-two grids stay 32 bytes.
#[derive(Clone)]
enum SpareRare {
    SmallCuts(Box<[SmallCut]>),
    WideCuts(Box<[Cut]>),
    WideOnly(Box<[Locations]>),
    WideSteps(Box<[GridStep]>),
    Full(Box<SpareRareFull>),
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<SpareRare>() == 24);

/// Stack-backed growable buffer for small `Copy` items during grid construction,
/// avoiding intermediate `Vec` allocations for typical mini patterns.
enum SmallCopyBuf<T: Copy, const N: usize> {
    Inline { len: usize, data: [T; N] },
    Heap(Vec<T>),
}

impl<T: Copy, const N: usize> SmallCopyBuf<T, N> {
    #[inline]
    fn new(zero: T) -> Self {
        Self::Inline {
            len: 0,
            data: [zero; N],
        }
    }

    #[inline]
    fn len(&self) -> usize {
        match self {
            Self::Inline { len, .. } => *len,
            Self::Heap(vec) => vec.len(),
        }
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    fn clear(&mut self) {
        match self {
            Self::Inline { len, .. } => *len = 0,
            Self::Heap(vec) => vec.clear(),
        }
    }

    #[inline]
    fn push(&mut self, value: T) {
        match self {
            Self::Inline { len, data } => {
                if *len < N {
                    data[*len] = value;
                    *len += 1;
                } else {
                    let mut vec = Vec::with_capacity(N * 2);
                    vec.extend_from_slice(&data[..*len]);
                    vec.push(value);
                    *self = Self::Heap(vec);
                }
            }
            Self::Heap(vec) => vec.push(value),
        }
    }

    #[inline]
    fn last_mut(&mut self) -> Option<&mut T> {
        match self {
            Self::Inline { len, data } => (*len > 0).then(|| &mut data[*len - 1]),
            Self::Heap(vec) => vec.last_mut(),
        }
    }

    #[inline]
    fn as_slice(&self) -> &[T] {
        match self {
            Self::Inline { len, data } => &data[..*len],
            Self::Heap(vec) => vec.as_slice(),
        }
    }

    fn into_boxed_slice(self) -> Box<[T]> {
        match self {
            Self::Inline { len, data } => Box::from(&data[..len]),
            Self::Heap(vec) => vec.into_boxed_slice(),
        }
    }
}

impl SpareRare {
    fn from_parts(cuts: &[Cut], wide: Box<[Locations]>, wide_steps: Box<[GridStep]>) -> Self {
        if wide.is_empty() && wide_steps.is_empty() {
            if cuts.len() <= 16 {
                let mut buf = [SmallCut {
                    index: 0,
                    begin: 0,
                    end: 0,
                }; 16];
                let mut all_small = true;
                for (slot, &cut) in buf[..cuts.len()].iter_mut().zip(cuts.iter()) {
                    if let Some(small) = SmallCut::from_wide(cut) {
                        *slot = small;
                    } else {
                        all_small = false;
                        break;
                    }
                }
                if all_small {
                    return Self::SmallCuts(Box::from(&buf[..cuts.len()]));
                }
                return Self::WideCuts(Box::from(cuts));
            }
            let mut small = Vec::with_capacity(cuts.len());
            for &cut in cuts {
                let Some(s) = SmallCut::from_wide(cut) else {
                    return Self::WideCuts(Box::from(cuts));
                };
                small.push(s);
            }
            Self::SmallCuts(small.into_boxed_slice())
        } else if cuts.is_empty() && wide_steps.is_empty() {
            Self::WideOnly(wide)
        } else if cuts.is_empty() && wide.is_empty() {
            Self::WideSteps(wide_steps)
        } else {
            Self::Full(Box::new(SpareRareFull {
                cuts: Box::from(cuts),
                wide,
                wide_steps,
            }))
        }
    }

    #[inline]
    fn cuts(&self) -> CutsSlice<'_> {
        match self {
            Self::SmallCuts(s) => CutsSlice::Small(s),
            Self::WideCuts(w) => CutsSlice::Wide(w),
            Self::WideOnly(_) | Self::WideSteps(_) => CutsSlice::EMPTY,
            Self::Full(full) => CutsSlice::Wide(&full.cuts),
        }
    }

    #[inline]
    fn wide(&self) -> &[Locations] {
        match self {
            Self::SmallCuts(_) | Self::WideCuts(_) | Self::WideSteps(_) => &[],
            Self::WideOnly(w) => w,
            Self::Full(full) => &full.wide,
        }
    }

    #[inline]
    fn wide_steps(&self) -> Option<&[GridStep]> {
        match self {
            Self::WideSteps(steps) => Some(steps),
            Self::Full(full) if !full.wide_steps.is_empty() => Some(&full.wide_steps),
            _ => None,
        }
    }
}

/// Cuts, wide steps, and spare locations of a [`Grid`].
#[derive(Clone)]
struct Spare {
    /// Packed multi-range step locations relative to `Grid::base`. Each word
    /// is a [`PackedLoc`] range, with `PackedLoc::SPARE` set on the last
    /// range of a step.
    packed: Box<[u32]>,
    /// Rare cuts, wide locations, and wide steps, allocated only when present.
    rare: Option<Box<SpareRare>>,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<Spare>() == 24);

impl Grid {
    const SMALL: u32 = 1 << 31;

    /// Pack `locations` relative to `base`: inline for 0 or 1 fitting range,
    /// into `packed_runs` for multi-range steps that all fit, or into `wide`.
    fn pack_locations(
        base: u32,
        locations: &Locations,
        packed_runs: &mut PackedRunsBuilder,
        wide: &mut Vec<usize>,
        index: usize,
    ) -> PackedLoc {
        match locations.len() {
            0 => PackedLoc(0),
            1 => {
                if let Some((begin, length)) = locations.single_packed_raw()
                    && let Some(packed) = PackedLoc::range_raw(base, begin, length)
                {
                    return packed;
                }
                if let Some(range) = locations.get(0)
                    && let Some(packed) = PackedLoc::range(base, range)
                {
                    return packed;
                }
                let at = wide.len() as u32;
                wide.push(index);
                PackedLoc(PackedLoc::WIDE_MASK | at)
            }
            2 => {
                if let Some(r0) = locations.get(0)
                    && let Some(r1) = locations.get(1)
                    && let Some(w0) = PackedLoc::range(base, r0)
                    && let Some(w1) = PackedLoc::range(base, r1)
                    && let Some(start) = packed_runs.intern_run(&[w0.0, w1.0 | PackedLoc::SPARE])
                {
                    return PackedLoc(PackedLoc::SPARE | start as u32);
                }
                let at = wide.len() as u32;
                wide.push(index);
                PackedLoc(PackedLoc::WIDE_MASK | at)
            }
            len => {
                let mut run = SmallCopyBuf::<u32, 8>::new(0);
                let mut all_fit = true;
                for i in 0..len {
                    if let Some(range) = locations.get(i)
                        && let Some(word) = PackedLoc::range(base, range)
                    {
                        run.push(word.0);
                    } else {
                        all_fit = false;
                        break;
                    }
                }
                if all_fit {
                    if let Some(last) = run.last_mut() {
                        *last |= PackedLoc::SPARE;
                    }
                    if let Some(start) = packed_runs.intern_run(run.as_slice()) {
                        return PackedLoc(PackedLoc::SPARE | start as u32);
                    }
                }
                let at = wide.len() as u32;
                wide.push(index);
                PackedLoc(PackedLoc::WIDE_MASK | at)
            }
        }
    }

    #[inline]
    fn note_loc_base(base: &mut Option<u32>, locations: &Locations) {
        if let Some((begin, length)) = locations.single_packed_raw() {
            if length < PackedLoc::LENGTH_MASK && begin < Self::SMALL {
                *base = Some(base.map_or(begin, |least| least.min(begin)));
            }
            return;
        }
        for i in 0..locations.len() {
            if let Some(range) = locations.get(i)
                && PackedLoc::fits(range)
                && let Ok(begin) = u32::try_from(range.0)
                && begin < Self::SMALL
            {
                *base = Some(base.map_or(begin, |b| b.min(begin)));
            }
        }
    }

    /// The steps of `raws` on a grid, or `None` for times that do not fit.
    /// A grid takes the locations that do not pack out of `raws`, and
    /// nothing else.
    fn build(raws: &mut [Raw]) -> Option<Listing> {
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
            Self::note_loc_base(&mut base, &raw.locations);
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
        let mut cuts = SmallCopyBuf::<Cut, 16>::new(Cut {
            index: 0,
            begin: 0,
            end: 0,
        });
        let mut packed_runs = PackedRunsBuilder::new();
        // The places of the steps whose locations do not fit packed encoding.
        let mut wide = Vec::new();
        let steps = GridSteps::build(raws.len(), |index| {
            let raw = &raws[index];
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
            let loc =
                Self::pack_locations(base, &raw.locations, &mut packed_runs, &mut wide, index);
            Some(GridStep {
                part_begin,
                part_end,
                value: raw.value,
                loc,
            })
        })?;
        if wide.len() > PackedLoc::INDEX_MASK as usize {
            return None;
        }
        let (steps, wide_steps) = match steps {
            GridSteps::Wide(wide) => (GridSteps::Small(Box::from([])), wide),
            other => (other, Box::from([])),
        };
        // The grid fits, so any wide locations move with no copy.
        let rare = (!cuts.is_empty() || !wide.is_empty() || !wide_steps.is_empty()).then(|| {
            let wide_locs: Box<[Locations]> = wide
                .iter()
                .map(|index| std::mem::take(&mut raws[*index].locations))
                .collect();
            Box::new(SpareRare::from_parts(
                cuts.as_slice(),
                wide_locs,
                wide_steps,
            ))
        });
        let packed = if packed_runs.is_empty() {
            PackedRunsInput::None
        } else if rare.is_none()
            && let Some(small) = try_pack_runs_u16(packed_runs.as_slice())
        {
            PackedRunsInput::Small(small)
        } else {
            PackedRunsInput::Wide(packed_runs.into_boxed_slice())
        };
        let base = base | if small_denominators { Self::SMALL } else { 0 };
        Some(Listing::from_parts(ticks, base, steps, packed, rare))
    }

    fn build_listing(raws: &mut [Raw]) -> Option<Listing> {
        Self::build(raws)
    }

    fn build_indexed_listing(source_steps: &mut [SourceStep]) -> Option<Listing> {
        Self::build_indexed(source_steps)
    }

    /// Build a [`Listing`] directly from uncut [`SourceStep`]s in cycles without
    /// allocating an intermediate [`Raw`] list.
    fn build_indexed(source_steps: &mut [SourceStep]) -> Option<Listing> {
        u32::try_from(source_steps.len()).ok()?;
        let mut ticks: u64 = 1;
        let mut base: Option<u32> = None;
        let mut last = 1;
        let mut small_denominators = true;
        for step in source_steps.iter() {
            for time in [step.begin, step.end] {
                if time.denom() == last {
                    continue;
                }
                last = time.denom();
                small_denominators &= i32::try_from(last).is_ok();
                let denom = u64::from(u32::try_from(last).ok()?);
                if !ticks.is_multiple_of(denom) {
                    ticks = ticks / gcd(ticks, denom) * denom;
                    u32::try_from(ticks).ok()?;
                }
            }
            Self::note_loc_base(&mut base, &step.locations);
        }
        let base = base.unwrap_or(0);
        let ticks = NonZeroU32::new(u32::try_from(ticks).ok()?)?;
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
        let mut packed_runs = PackedRunsBuilder::new();
        let mut wide = Vec::new();
        let steps = GridSteps::build(source_steps.len(), |index| {
            let src = &source_steps[index];
            let (begin, end) = (on_grid(src.begin)?, on_grid(src.end)?);
            let loc =
                Self::pack_locations(base, &src.locations, &mut packed_runs, &mut wide, index);
            Some(GridStep {
                part_begin: begin,
                part_end: end,
                value: src.value,
                loc,
            })
        })?;
        if wide.len() > PackedLoc::INDEX_MASK as usize {
            return None;
        }
        let (steps, wide_steps) = match steps {
            GridSteps::Wide(wide) => (GridSteps::Small(Box::from([])), wide),
            other => (other, Box::from([])),
        };
        let rare = (!wide.is_empty() || !wide_steps.is_empty()).then(|| {
            let wide_locs: Box<[Locations]> = wide
                .iter()
                .map(|index| std::mem::take(&mut source_steps[*index].locations))
                .collect();
            Box::new(SpareRare::from_parts(&[], wide_locs, wide_steps))
        });
        let packed = if packed_runs.is_empty() {
            PackedRunsInput::None
        } else if rare.is_none()
            && let Some(small) = try_pack_runs_u16(packed_runs.as_slice())
        {
            PackedRunsInput::Small(small)
        } else {
            PackedRunsInput::Wide(packed_runs.into_boxed_slice())
        };
        let base = base | if small_denominators { Self::SMALL } else { 0 };
        Some(Listing::from_parts(ticks, base, steps, packed, rare))
    }

    #[inline(always)]
    fn view(&self) -> GridView<'_> {
        let (packed, rare) = match self.side.as_deref() {
            Some(side) => (
                if side.packed.is_empty() {
                    PackedRunsRef::None
                } else {
                    PackedRunsRef::Wide(&side.packed)
                },
                side.rare.as_deref(),
            ),
            None => (PackedRunsRef::None, None),
        };
        GridView {
            ticks: self.ticks,
            base: self.base,
            steps: self.steps_ref(),
            packed,
            rare,
        }
    }

    #[inline]
    pub(crate) fn can_intern(&self) -> bool {
        self.side.as_deref().is_none_or(|s| s.rare.is_none())
    }

    pub(crate) fn hash_intern(&self, state: &mut impl std::hash::Hasher) {
        use std::hash::Hash;
        self.ticks.get().hash(state);
        self.base.hash(state);
        let len = self.small_steps.len();
        len.hash(state);
        if let Some(first) = self.small_steps.first() {
            first.hash_intern(state);
            if len > 1 {
                self.small_steps[len - 1].hash_intern(state);
            }
        }
        if let Some(spare) = &self.side {
            spare.packed.len().hash(state);
        }
    }

    pub(crate) fn eq_intern(&self, other: &Self) -> bool {
        self.ticks == other.ticks
            && self.base == other.base
            && self.small_steps == other.small_steps
            && match (&self.side, &other.side) {
                (None, None) => true,
                (Some(a), Some(b)) => a.rare.is_none() && b.rare.is_none() && a.packed == b.packed,
                _ => false,
            }
    }

    #[inline(always)]
    fn steps_ref(&self) -> GridStepsRef<'_> {
        if let Some(wide) = self
            .side
            .as_deref()
            .and_then(|side| side.rare.as_deref())
            .and_then(SpareRare::wide_steps)
        {
            GridStepsRef::Wide(wide)
        } else {
            GridStepsRef::Small(&self.small_steps)
        }
    }

    fn small_denominators(&self) -> bool {
        self.base & Self::SMALL != 0
    }
}

impl<'a> GridView<'a> {
    #[inline]
    fn made_small(self, ticks: i64) -> Fraction {
        with_tick_grid(self.ticks, |reductions| {
            self.made_small_with(ticks, reductions)
        })
    }

    #[inline]
    fn made_small_with(self, ticks: i64, reductions: Option<&TickGrid>) -> Fraction {
        match reductions {
            Some(reductions) => reductions.time(ticks),
            None => {
                let d = self.ticks.get();
                if d.is_power_of_two()
                    && let Some(time) = Fraction::dyadic_i64(ticks, d.trailing_zeros())
                {
                    return time;
                }
                Fraction::from_ticks(i128::from(ticks), self.ticks)
            }
        }
    }

    fn time(self, ticks: u32) -> Fraction {
        self.made_small(i64::from(ticks))
    }

    #[inline(always)]
    fn ticks_at(self, time: Fraction) -> Option<(i64, i64)> {
        let ticks = i64::from(self.ticks.get());
        let scaled = i64::try_from(time.numer()).ok()?.checked_mul(ticks)?;
        let denom = i64::try_from(time.denom()).ok()?;
        if denom == 1 {
            return Some((scaled, scaled));
        }
        let (floor, rest) = match denom & (denom - 1) == 0 {
            true => (scaled >> denom.trailing_zeros(), scaled & (denom - 1)),
            false => (scaled.div_euclid(denom), scaled.rem_euclid(denom)),
        };
        Some((floor, floor + i64::from(rest != 0)))
    }

    fn cuts(self) -> CutsSlice<'a> {
        self.rare.map_or(CutsSlice::EMPTY, SpareRare::cuts)
    }

    fn cut(self, index: usize) -> Option<Cut> {
        let mut cuts = self.cuts().starting_at(index);
        cuts.split_first_at(index)
    }

    fn part(self, index: usize) -> TimeSpan {
        let (part_begin, part_end) = self.steps.times_of(index);
        TimeSpan::new(self.time(part_begin), self.time(part_end))
    }

    fn whole(self, index: usize) -> TimeSpan {
        self.times(index).0
    }

    fn times(self, index: usize) -> (TimeSpan, Option<TimeSpan>) {
        let part = self.part(index);
        match self.cut(index) {
            Some(cut) => (
                TimeSpan::new(self.time(cut.begin), self.time(cut.end)),
                Some(part),
            ),
            None => (part, None),
        }
    }

    #[inline]
    fn locations(self, index: usize) -> Locations {
        let packed = self.steps.get(index).loc.0;
        if packed == 0 {
            return Locations::new();
        }
        if packed & PackedLoc::WIDE_MASK == PackedLoc::WIDE_MASK {
            let rare = self.rare.expect("a wide location has a rare side");
            return rare.wide()[(packed & PackedLoc::INDEX_MASK) as usize].clone();
        }
        let base = self.base & !Grid::SMALL;
        if packed & PackedLoc::SPARE != 0 {
            let mut out = Locations::new();
            let start = (packed & PackedLoc::INDEX_MASK) as usize;
            match self.packed {
                PackedRunsRef::Small(runs) => {
                    for &word in &runs[start..] {
                        let entry = word & !SMALL_RUN_END;
                        let begin = base + u32::from(entry >> SMALL_RUN_LEN_BITS);
                        let length = u32::from((entry & SMALL_RUN_LEN_MASK) - 1);
                        out.push_narrow(begin, length);
                        if (word & SMALL_RUN_END) != 0 {
                            break;
                        }
                    }
                }
                PackedRunsRef::Wide(runs) => {
                    for &word in &runs[start..] {
                        let entry = word & !PackedLoc::SPARE;
                        let begin = base + (entry >> PackedLoc::LENGTH_BITS);
                        let length = (entry & PackedLoc::LENGTH_MASK) - 1;
                        out.push_narrow(begin, length);
                        if word & PackedLoc::SPARE != 0 {
                            break;
                        }
                    }
                }
                PackedRunsRef::None => unreachable!("a spare location has packed runs"),
            }
            return out;
        }
        let begin = base + (packed >> PackedLoc::LENGTH_BITS);
        let length = (packed & PackedLoc::LENGTH_MASK) - 1;
        Locations::one(begin, length as u16)
    }

    fn places(self) -> Vec<Place> {
        (0..self.steps.len())
            .map(|index| {
                let (whole, part) = self.times(index);
                Place::at(
                    whole,
                    part,
                    self.steps.get(index).value,
                    self.locations(index),
                )
            })
            .collect()
    }

    fn in_order(self, period: Fraction) -> Option<bool> {
        self.in_order_and_holds(period, None)
    }

    fn in_order_and_holds(self, period: Fraction, values_len: Option<usize>) -> Option<bool> {
        if period <= Fraction::ZERO {
            return Some(false);
        }
        let bound = period.numer().checked_mul(i128::from(self.ticks.get()))?;
        let max_tick_i128 = bound / period.denom();
        let max_tick = u32::try_from(max_tick_i128).unwrap_or(u32::MAX);
        if self.rare.is_none()
            && let GridStepsRef::Small(steps) = self.steps
        {
            let max_u16 = u16::try_from(max_tick).unwrap_or(u16::MAX);
            let mut prev_begin = 0u16;
            let mut prev_end = 0u16;
            match values_len {
                Some(vlen) => {
                    for s in steps {
                        if s.part_begin >= s.part_end
                            || s.part_end > max_u16
                            || prev_begin > s.part_begin
                            || prev_end > s.part_end
                            || usize::from(s.value) >= vlen
                        {
                            return Some(false);
                        }
                        prev_begin = s.part_begin;
                        prev_end = s.part_end;
                    }
                }
                None => {
                    for s in steps {
                        if s.part_begin >= s.part_end
                            || s.part_end > max_u16
                            || prev_begin > s.part_begin
                            || prev_end > s.part_end
                        {
                            return Some(false);
                        }
                        prev_begin = s.part_begin;
                        prev_end = s.part_end;
                    }
                }
            }
            return Some(true);
        }
        let mut cuts = self.cuts();
        let len = self.steps.len();
        let mut prev_begin = 0u32;
        let mut prev_end = 0u32;
        for index in 0..len {
            let (part_begin, part_end) = self.steps.times_of(index);
            let cut = cuts.split_first_at(index);
            let (begin, end) = cut.map_or((part_begin, part_end), |c| (c.begin, c.end));
            if begin > part_begin
                || part_begin >= part_end
                || part_end > end
                || end > max_tick
                || prev_begin > part_begin
                || prev_end > part_end
            {
                return Some(false);
            }
            if let Some(vlen) = values_len
                && (self.steps.get(index).value as usize) >= vlen
            {
                return Some(false);
            }
            prev_begin = part_begin;
            prev_end = part_end;
        }
        Some(true)
    }

    fn same_times(self, other: Self) -> bool {
        self.ticks == other.ticks
            && self.cuts().same(other.cuts())
            && self.steps.len() == other.steps.len()
            && (0..self.steps.len())
                .all(|index| self.steps.times_of(index) == other.steps.times_of(index))
    }

    fn in_cycles_for(self, period: Fraction) -> bool {
        let Ok(turns) = u128::try_from(period.denom()) else {
            return false;
        };
        let ticks = u128::from(self.ticks.get());
        for i in 0..self.steps.len() {
            let (b, e) = self.steps.times_of(i);
            let Some(begins) = u128::from(b).checked_mul(turns) else {
                return false;
            };
            let Some(ends) = u128::from(e).checked_mul(turns) else {
                return false;
            };
            let Some(bound) = (begins / ticks + 1).checked_mul(ticks) else {
                return false;
            };
            if ends > bound {
                return false;
            }
        }
        true
    }

    fn faster(self, factor: Fraction, new_period: Fraction) -> Option<(Listing, bool)> {
        let f_num = u32::try_from(factor.numer()).ok()?;
        let f_den = u32::try_from(factor.denom()).ok()?;
        if f_num == 0 || f_den == 0 {
            return None;
        }
        let old_ticks = self.ticks.get();
        let g0 = gcd(u64::from(old_ticks), u64::from(f_den)) as u32;
        let ticks_1 = old_ticks / g0;
        let mul = f_den / g0;
        let mut g1 = u64::from(f_num);
        if g1 > 1 {
            for i in 0..self.steps.len() {
                let (b, e) = self.steps.times_of(i);
                g1 = gcd(g1, u64::from(b));
                g1 = gcd(g1, u64::from(e));
                if g1 == 1 {
                    break;
                }
            }
            if g1 > 1 {
                let cuts = self.cuts();
                for i in 0..cuts.len() {
                    let cut = cuts.get(i).expect("cut in bounds");
                    g1 = gcd(g1, u64::from(cut.begin));
                    g1 = gcd(g1, u64::from(cut.end));
                    if g1 == 1 {
                        break;
                    }
                }
            }
        }
        let ticks_2 = u64::from(ticks_1).checked_mul(u64::from(f_num))? / g1;
        let new_ticks_u32 = u32::try_from(ticks_2).ok()?;
        if i32::try_from(new_ticks_u32).is_err() {
            return None;
        }
        let new_ticks = NonZeroU32::new(new_ticks_u32)?;
        let clone_packed = || match self.packed {
            PackedRunsRef::None => PackedRunsInput::None,
            PackedRunsRef::Small(runs) => PackedRunsInput::Small(SmallPackedRuns::from_slice(runs)),
            PackedRunsRef::Wide(runs) => PackedRunsInput::Wide(Box::from(runs)),
        };
        let (steps, packed, rare) = if mul == 1 && g1 == 1 {
            let steps = match self.steps {
                GridStepsRef::Small(steps) if steps.len() <= MAX_INLINE_GRID_LEN => {
                    let mut arr = [GridSteps::ZERO_SMALL; MAX_INLINE_GRID_LEN];
                    arr[..steps.len()].copy_from_slice(steps);
                    GridSteps::Inline(steps.len() as u8, arr)
                }
                GridStepsRef::Small(steps) => GridSteps::Small(Box::from(steps)),
                GridStepsRef::Wide(_) => GridSteps::Small(Box::from([])),
            };
            let rare = self.rare.map(|rare| Box::new(rare.clone()));
            (steps, clone_packed(), rare)
        } else {
            let rescale = |t: u32| -> Option<u32> {
                u32::try_from((u64::from(t) / g1).checked_mul(u64::from(mul))?).ok()
            };
            let steps = GridSteps::build(self.steps.len(), |i| {
                let mut step = self.steps.get(i);
                step.part_begin = rescale(step.part_begin)?;
                step.part_end = rescale(step.part_end)?;
                Some(step)
            })?;
            let (steps, wide_steps) = match steps {
                GridSteps::Wide(wide) => (GridSteps::Small(Box::from([])), wide),
                other => (other, Box::from([])),
            };
            let mut cuts = SmallCopyBuf::<Cut, 16>::new(Cut {
                index: 0,
                begin: 0,
                end: 0,
            });
            let mut wide_locs: Box<[Locations]> = Box::from([]);
            if let Some(rare) = self.rare {
                let cuts_view = rare.cuts();
                for i in 0..cuts_view.len() {
                    let mut cut = cuts_view.get(i).expect("cut in bounds");
                    cut.begin = rescale(cut.begin)?;
                    cut.end = rescale(cut.end)?;
                    cuts.push(cut);
                }
                wide_locs = rare.wide().to_vec().into_boxed_slice();
            }
            let rare =
                (!cuts.is_empty() || !wide_locs.is_empty() || !wide_steps.is_empty()).then(|| {
                    Box::new(SpareRare::from_parts(
                        cuts.as_slice(),
                        wide_locs,
                        wide_steps,
                    ))
                });
            (steps, clone_packed(), rare)
        };
        let listing = Listing::from_parts(new_ticks, self.base | Grid::SMALL, steps, packed, rare);
        let in_cycles = listing
            .with_grid_view(|view| view.in_cycles_for(new_period))
            .unwrap_or(false);
        Some((listing, in_cycles))
    }
}

thread_local! {
    /// The tables of reduced times that this thread made, by tick size: the
    /// layers of one tick size share one.
    static TICK_GRIDS: std::cell::RefCell<Vec<TickGrid>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Borrow the table of reduced times for `ticks` without atomic reference
/// counting. Power-of-two grids skip the thread-local completely.
#[inline]
pub(crate) fn with_tick_grid<R>(ticks: NonZeroU32, f: impl FnOnce(Option<&TickGrid>) -> R) -> R {
    with_tick_grid_if(ticks, true, f)
}

/// Borrow a cached [`TickGrid`] for `ticks`, building a new thread-local table
/// for large denominators (`> 64`) only when `build_if_missing` is true.
#[inline]
pub(crate) fn with_tick_grid_if<R>(
    ticks: NonZeroU32,
    build_if_missing: bool,
    f: impl FnOnce(Option<&TickGrid>) -> R,
) -> R {
    let raw = ticks.get();
    if raw.is_power_of_two() || i64::from(raw) > TickGrid::MAX_DENOM {
        return f(None);
    }
    if raw <= 64 {
        static SMALL_GRIDS: [std::sync::OnceLock<TickGrid>; 65] =
            [const { std::sync::OnceLock::new() }; 65];
        let grid = SMALL_GRIDS[raw as usize]
            .get_or_init(|| TickGrid::new(i64::from(raw)).expect("valid small tick grid"));
        return f(Some(grid));
    }
    let denom = i64::from(raw);
    let mut f = Some(f);
    if let Some(result) = TICK_GRIDS.with_borrow(|grids| {
        grids
            .iter()
            .find(|grid| grid.denom() == denom)
            .map(|grid| f.take().expect("closure present")(Some(grid)))
    }) {
        return result;
    }
    let f = f.expect("closure not yet called");
    if !build_if_missing {
        return f(None);
    }
    let Some(built) = TickGrid::new(denom) else {
        return f(None);
    };
    const MAX_TICK_GRIDS: usize = 16;
    let stored = TICK_GRIDS.with_borrow_mut(|grids| {
        if grids.len() >= MAX_TICK_GRIDS {
            return Err(built);
        }
        let idx = grids.len();
        grids.push(built);
        Ok(idx)
    });
    match stored {
        Ok(idx) => TICK_GRIDS.with_borrow(|grids| f(Some(&grids[idx]))),
        Err(built) => f(Some(&built)),
    }
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
    crate::order_key(&time) as f32
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

/// Low 13 bits of [`Layer::period_denom_flags`] hold the small period denominator
/// (`0` signals a rare large period stored inside `Steps::WithPeriod`).
const LAYER_DENOM_MASK: u16 = (1 << 13) - 1;
const LAYER_FLAG_IN_CYCLES: u16 = 1 << 13;
const LAYER_FLAG_DATA: u16 = 1 << 14;
const LAYER_FLAG_MAPS: u16 = 1 << 15;

/// Direct-read flags when a layer answers map reads straight from its inline
/// `named_id`, `values`, and `dress.set_map` without allocating a `Table`.
/// Bits 3..9 store the 6-bit bloom mark bit of `named_id` for zero-lookup
/// bloom filtering in `seq_get` and `seq_position`.
const LAYER_DIRECT_KEYED: u16 = 1 << 0;
const LAYER_DIRECT_SPLIT: u16 = 1 << 1;
const LAYER_DIRECT_MADE: u16 = 1 << 2;
const LAYER_MARK_BIT_SHIFT: u32 = 3;
const LAYER_MARK_BIT_MASK: u16 = 0x3f;
/// Set when `self.values` is known to contain zero `Value::Object(_)` entries,
/// letting post-eval deduplication skip scanning/branching over step values.
const LAYER_VALUES_NO_OBJECTS: u16 = 1 << 9;

/// One layer of a sequence node: the places of its steps, and each value
/// of them once. Each hap holds a plain value and its locations: a fold
/// that would give a hap more keeps its own node.
#[derive(Clone)]
pub(crate) struct Layer {
    /// Shared by each layer that a fold makes of the same steps.
    steps: Steps,
    /// The value of each step, each once, before the name and the controls
    /// that the dress gives it.
    values: Arc<[Value]>,
    /// What the folds on the layer give each hap. `None` for a layer that
    /// no fold dressed (or only carries a built-in `named_id`). See [`Dress`].
    dress: Option<Arc<Dress>>,
    /// Numerator of a small rational period (when `period_denom_flags & LAYER_DENOM_MASK != 0`).
    period_numer: i16,
    /// Low 13 bits: period denominator (`0` means large period in `Steps::WithPeriod`).
    /// Top 3 bits: `in_cycles`, `data`, and `maps` flags.
    period_denom_flags: u16,
    /// Built-in 1-based control id naming each value when read, or `0` when
    /// unnamed or named by a custom control in `dress.tail.custom_named`.
    named_id: u16,
    /// Precomputed direct-keyed read flags (`LAYER_DIRECT_*`).
    direct_flags: u16,
}

/// The values of a layer as its dress makes them.
#[derive(Clone, Copy)]
enum Values<'a> {
    /// A list of the values.
    Held(&'a [Value]),
    /// The maps that read a table.
    Table(&'a Arc<Table>),
    /// Direct maps backed by a direct-keyed layer in `Arc<[Layer]>`, with no `Table`.
    SeqLayer(&'a Arc<[Layer]>, u16),
    /// Single built-in key with no dress tail: emits zero-Arc `StaticInt` maps
    /// for `-128..=127` numeric steps and falls back to `SeqLayer` otherwise.
    SingleBuiltin(&'a Arc<[Layer]>, u16, u16, &'a [Value]),
    /// Direct-keyed layer read without an owning `Arc<[Layer]>` (build folds / tests).
    DirectKeyed(&'a Layer),
}

/// What makes the haps of the steps of a layer: the list, the values as the
/// dress makes them, and the locations and keys of each hap, all read once
/// for a loop of steps.
struct Maker<'a> {
    listing: &'a Listing,
    values: Values<'a>,
    /// Pre-combined step contexts when the dress would spill `Locations::INLINE`.
    contexts: Option<&'a [Locations]>,
    /// The dress when it adds locations before or after each step's own.
    loc_dress: Option<&'a Dress>,
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
        let context = if let Some(contexts) = self.contexts {
            contexts[index].clone()
        } else {
            let step_loc = self.listing.locations(index);
            match self.loc_dress {
                Some(dress) => dress.around_owned(step_loc),
                None => step_loc,
            }
        };
        Hap::of_sequence(whole, part, value, context, self.keys.clone())
    }

    #[cold]
    #[inline(never)]
    fn make_grid_context_slow(&self, grid: GridView<'_>, index: usize) -> Locations {
        let step_loc = grid.locations(index);
        match self.loc_dress {
            Some(dress) => dress.around_owned(step_loc),
            None => step_loc,
        }
    }

    /// Like [`Self::make_grid`], reading a pre-fetched `SmallGridStep::loc`
    /// directly without re-matching `GridStepsRef::Small`.
    #[inline(always)]
    fn make_grid_small(
        &self,
        grid: GridView<'_>,
        index: usize,
        value: u32,
        loc: u16,
        whole: TimeSpan,
        part: TimeSpan,
    ) -> Hap {
        let value = self
            .values
            .get(value as usize)
            .expect("a step holds a value of its layer");
        let context = if let Some(contexts) = self.contexts {
            contexts[index].clone()
        } else if (loc & SmallGridStep::SPARE) == 0
            && self.loc_dress.is_none_or(|d| d.tail.is_none())
        {
            if loc == 0 {
                match self.loc_dress {
                    Some(d) if d.before_packed[0] != 0 => {
                        let p0 = d.before_packed[0] - 1;
                        if d.before_packed[1] != 0 {
                            Locations::from_packed_parts(
                                2,
                                p0,
                                u64::from(d.before_packed[1] - 1),
                                0,
                                0,
                            )
                        } else {
                            Locations::from_packed_parts(1, p0, 0, 0, 0)
                        }
                    }
                    _ => Locations::new(),
                }
            } else {
                let begin =
                    (grid.base & !Grid::SMALL) + u32::from(loc >> SmallGridStep::LENGTH_BITS);
                let length = u32::from((loc & SmallGridStep::LENGTH_MASK) - 1);
                if let Some(step_packed) = Locations::pack_small(begin, length) {
                    match self.loc_dress {
                        Some(d) if d.before_packed[0] != 0 => {
                            let p0 = d.before_packed[0] - 1;
                            if d.before_packed[1] != 0 {
                                Locations::from_packed_parts(
                                    3,
                                    p0,
                                    u64::from(d.before_packed[1] - 1)
                                        | (u64::from(step_packed) << 32),
                                    0,
                                    0,
                                )
                            } else {
                                Locations::from_packed_parts(2, p0, u64::from(step_packed), 0, 0)
                            }
                        }
                        _ => Locations::from_packed_parts(1, step_packed, 0, 0, 0),
                    }
                } else {
                    self.make_grid_context_slow(grid, index)
                }
            }
        } else {
            self.make_grid_context_slow(grid, index)
        };
        Hap::of_sequence(whole, part, value, context, self.keys.clone())
    }

    /// Like [`Self::make`], reading step locations directly from an already
    /// unpacked [`GridView`] to skip per-hap `CompactGrid` dispatch.
    #[inline(always)]
    fn make_grid(
        &self,
        grid: GridView<'_>,
        index: usize,
        value: u32,
        whole: TimeSpan,
        part: TimeSpan,
    ) -> Hap {
        if let GridStepsRef::Small(steps) = grid.steps {
            return self.make_grid_small(grid, index, value, steps[index].loc, whole, part);
        }
        let value = self
            .values
            .get(value as usize)
            .expect("a step holds a value of its layer");
        let context = if let Some(contexts) = self.contexts {
            contexts[index].clone()
        } else {
            self.make_grid_context_slow(grid, index)
        };
        Hap::of_sequence(whole, part, value, context, self.keys.clone())
    }
}

impl Values<'_> {
    #[inline]
    fn get(&self, index: usize) -> Option<Value> {
        match self {
            Self::Held(values) => values.get(index).cloned(),
            Self::Table(table) => table.map(index).map(Value::Object),
            Self::SeqLayer(layers, layer_idx) => {
                let layer = &layers[usize::from(*layer_idx)];
                (index < layer.values.len()).then(|| {
                    Value::Object(crate::OrderedMap::from_seq_layer(
                        layers,
                        *layer_idx,
                        index as u32,
                    ))
                })
            }
            Self::SingleBuiltin(layers, layer_idx, named_id, values) => {
                values.get(index).map(|v| {
                    if let Some(n) = crate::value::static_int_of(v) {
                        Value::Object(crate::OrderedMap::from_static_int(*named_id, n, false))
                    } else {
                        Value::Object(crate::OrderedMap::from_seq_layer(
                            layers,
                            *layer_idx,
                            index as u32,
                        ))
                    }
                })
            }
            Self::DirectKeyed(layer) => (index < layer.values.len()).then(|| {
                Value::Object(crate::OrderedMap::from_layout(
                    layer.seq_layout(index as u32),
                    layer.seq_is_made(),
                ))
            }),
        }
    }
}
// A layer is cloned by each fold: its steps stay behind a handle.
#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<Steps>() == 16);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<Layer>() == 48);

mod dress;
pub(crate) use dress::Dress;
use dress::{BARE, DressSet, unpack_dress_loc};

/// One dress for the layers of `layers` that are dressed alike, as a fold
/// that dresses each layer on its own makes them.
fn share_dresses(layers: &mut [Layer]) {
    if layers.len() <= 1 {
        return;
    }
    for i in 1..layers.len() {
        let Some(dress_i) = layers[i].dress.as_ref() else {
            continue;
        };
        let mut found = None;
        for prev in &layers[..i] {
            if let Some(prev_dress) = prev.dress.as_ref()
                && (Arc::ptr_eq(dress_i, prev_dress)
                    || dress_i.same(prev_dress, |a, b| a.same_data(b)))
            {
                found = Some(Arc::clone(prev_dress));
                break;
            }
        }
        if let Some(shared) = found {
            layers[i].dress = Some(shared);
        }
    }
}

/// `layers`, each in the dress that `change` makes of its own, collected
/// directly into `Arc<[Layer]>` without an intermediate `Vec<Layer>`.
fn redressed_arc(layers: &[Layer], change: impl Fn(&mut Dress)) -> Arc<[Layer]> {
    let mut last: Option<(*const Dress, Option<Arc<Dress>>)> = None;
    layers
        .iter()
        .map(|layer| {
            let from = layer.dress.as_ref().map_or(std::ptr::null(), Arc::as_ptr);
            let dress = match &last {
                Some((seen, dress)) if *seen == from => dress.clone(),
                _ => {
                    let mut dress = layer.dress().clone();
                    change(&mut dress);
                    let dress = (!dress.is_bare()).then(|| dress.into_arc());
                    last = Some((from, dress.clone()));
                    dress
                }
            };
            layer.with(|layer| {
                layer.dress = dress;
                layer.refresh_direct();
            })
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
pub(crate) enum SequenceStop {
    /// A time left the native range.
    Range,
    /// The query stopped, or its budget refused the next hap.
    Query,
}

type BuiltinDedupFn<'a> = dyn FnMut(
        &Arc<dyn crate::value::BuiltinEntries>,
        bool,
    ) -> Option<Arc<dyn crate::value::BuiltinEntries>>
    + 'a;

type KeysDedupFn<'a> =
    dyn FnMut(&Arc<crate::context_keys::Keys>) -> Option<Arc<crate::context_keys::Keys>> + 'a;

/// Post-eval deduplicator hooks called by [`Layer::dedup_in`].
pub(crate) trait LayerDeduper {
    fn dedup_values(&mut self, values: &Arc<[Value]>, may_have_objects: bool) -> Arc<[Value]>;
    fn dedup_values_mut(&mut self, values: &mut Arc<[Value]>, may_have_objects: bool);
    fn dedup_dress(&mut self, dress: &Arc<Dress>) -> Arc<Dress>;
    fn dedup_dress_mut(&mut self, dress: &mut Arc<Dress>);
    fn dedup_compact_grid(&mut self, grid: &CompactGrid) -> CompactGrid;
    fn dedup_compact_grid_mut(&mut self, grid: &mut CompactGrid);
    fn dedup_grid(&mut self, grid: &Arc<Grid>) -> Arc<Grid>;
    fn dedup_grid_mut(&mut self, grid: &mut Arc<Grid>);
}

impl Layer {
    /// Lightweight O(1) upper bound on haps this layer can emit in a narrow
    /// (`<= 1/4` cycle) window, used to size the output `Vec<Hap>` without
    /// over-allocating into cold allocator size classes on sparse patterns.
    #[inline(always)]
    pub(crate) fn narrow_hap_bound(&self) -> usize {
        if self.source().is_some() {
            return usize::MAX;
        }
        let steps = self.count();
        if steps <= 1 {
            return steps;
        }
        let period = self.period();
        // Cycles per turn (floored, >= 1 when period >= 1). For sub-cycle
        // periods (`numer < denom`), scale up by `ceil(denom / numer)`.
        let numer = period.numer();
        let denom = period.denom();
        if numer <= 0 || denom <= 0 {
            return usize::MAX;
        }
        if numer >= denom {
            let whole_cycles = (numer / denom).max(1) as usize;
            let denom4 = whole_cycles.saturating_mul(4);
            (steps.saturating_add(denom4 - 1) / denom4).max(1)
        } else {
            let reps = ((denom + numer - 1) / numer) as usize;
            let total = steps.saturating_mul(reps);
            (total.saturating_add(3) / 4).max(1)
        }
    }

    #[inline]
    pub(crate) fn for_each_unpooled_builtin<'a, S, F>(
        &'a self,
        last_dress: &mut usize,
        last_values: &mut usize,
        seen_ptr: &mut S,
        f: &mut F,
    ) where
        S: FnMut(usize) -> bool,
        F: FnMut(&'a Arc<dyn crate::value::BuiltinEntries>, &'a [u16], &'a [Value]),
    {
        if let Some(dress) = &self.dress {
            let ptr = Arc::as_ptr(dress) as usize;
            if ptr != *last_dress {
                *last_dress = ptr;
                let fresh = Arc::strong_count(dress) == 1 || seen_ptr(ptr);
                if fresh
                    && let DressSet::Builtin(entries, _) = &dress.set
                    && let crate::value::Slice::Builtin(keys, vals) = entries.as_slice()
                    && (2..=crate::value::MAX_BUILTIN_MAP_LEN).contains(&keys.len())
                    && (Arc::strong_count(entries) == 1
                        || seen_ptr(Arc::as_ptr(entries) as *const () as usize))
                {
                    f(entries, keys, vals);
                }
            }
        }
        if !self.values_have_no_objects() {
            let vptr = Arc::as_ptr(&self.values) as *const Value as usize;
            if vptr != *last_values {
                *last_values = vptr;
                let fresh = Arc::strong_count(&self.values) == 1 || seen_ptr(vptr);
                if fresh {
                    for val in self.values.iter() {
                        if let Value::Object(map) = val
                            && let Some(entries) = map.as_builtin()
                            && let crate::value::Slice::Builtin(keys, vals) = entries.as_slice()
                            && (2..=crate::value::MAX_BUILTIN_MAP_LEN).contains(&keys.len())
                            && (Arc::strong_count(entries) == 1
                                || seen_ptr(Arc::as_ptr(entries) as *const () as usize))
                        {
                            f(entries, keys, vals);
                        }
                    }
                }
            }
        }
    }

    /// Whether this layer can emit a hap carrying `vst`/`vsti` plugin controls
    /// or a `JsValue` payload.
    pub(crate) fn may_have_plugin_notes(&self) -> bool {
        if let Some(dress) = &self.dress
            && let Some(tail) = &dress.tail
        {
            if let Some(names) = &tail.custom_named
                && names
                    .iter()
                    .any(|name| name.as_ref() == "vst" || name.as_ref() == "vsti")
            {
                return true;
            }
            if let Some(val) = &tail.set_other
                && crate::value_may_be_plugin_note(val)
            {
                return true;
            }
        }
        self.values.iter().any(crate::value_may_be_plugin_note)
    }

    /// Whether this layer holds no `OnceLock` cache or `StepSource` so it can
    /// be structurally deduplicated by canonical child pointers.
    #[inline]
    pub(crate) fn can_intern(&self) -> bool {
        self.is_data()
            && matches!(
                &self.steps,
                Steps::Listed(Listing::CompactGrid(_) | Listing::Grid(_))
            )
    }

    #[inline]
    pub(crate) fn hash_intern(&self, state: &mut impl std::hash::Hasher) {
        use std::hash::Hash;
        let header = u64::from(self.period_numer as u16)
            | (u64::from(self.period_denom_flags) << 16)
            | (u64::from(self.named_id) << 32)
            | (u64::from(self.direct_flags) << 48);
        header.hash(state);
        (Arc::as_ptr(&self.values) as *const Value as usize).hash(state);
        self.values.len().hash(state);
        self.dress
            .as_ref()
            .map_or(0usize, |d| Arc::as_ptr(d) as usize)
            .hash(state);
        match &self.steps {
            Steps::Listed(Listing::CompactGrid(g)) => {
                1u8.hash(state);
                g.ptr_hash(state);
            }
            Steps::Listed(Listing::Grid(g)) => {
                2u8.hash(state);
                (Arc::as_ptr(g) as usize).hash(state);
            }
            _ => {}
        }
    }

    pub(crate) fn eq_intern(&self, other: &Self) -> bool {
        self.period_numer == other.period_numer
            && self.period_denom_flags == other.period_denom_flags
            && self.named_id == other.named_id
            && self.direct_flags == other.direct_flags
            && Arc::ptr_eq(&self.values, &other.values)
            && match (&self.dress, &other.dress) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            }
            && match (&self.steps, &other.steps) {
                (
                    Steps::Listed(Listing::CompactGrid(a)),
                    Steps::Listed(Listing::CompactGrid(b)),
                ) => a.ptr_eq(b),
                (Steps::Listed(Listing::Grid(a)), Steps::Listed(Listing::Grid(b))) => {
                    Arc::ptr_eq(a, b)
                }
                _ => false,
            }
    }

    #[inline]
    pub(crate) fn can_alias(&self) -> bool {
        Arc::strong_count(&self.values) > 1
            && self
                .dress
                .as_ref()
                .is_none_or(|dress| Arc::strong_count(dress) > 1)
    }

    /// Deduplicate this layer's `values`, `dress`, and `steps` payloads in place
    /// when the enclosing `Arc<[Layer]>` is uniquely owned.
    pub(crate) fn dedup_in_mut(&mut self, deduper: &mut impl LayerDeduper) {
        if self.is_data() {
            let may_have_objects = !self.values_have_no_objects();
            deduper.dedup_values_mut(&mut self.values, may_have_objects);
        }
        if let Some(dress) = &mut self.dress {
            deduper.dedup_dress_mut(dress);
        }
        match &mut self.steps {
            Steps::Listed(Listing::CompactGrid(
                CompactGrid::OneStep(_) | CompactGrid::OneStepVal(_),
            )) => {}
            Steps::Listed(Listing::CompactGrid(g)) => {
                deduper.dedup_compact_grid_mut(g);
            }
            Steps::Listed(Listing::Grid(g)) if g.can_intern() => {
                deduper.dedup_grid_mut(g);
            }
            _ => {}
        }
    }

    /// Deduplicate this layer's `values`, `dress`, and `steps` payloads through
    /// the post-eval deduplicator, returning `Some(new_layer)` only when at
    /// least one handle changed.
    pub(crate) fn dedup_in(&self, deduper: &mut impl LayerDeduper) -> Option<Self> {
        let new_values = if self.is_data() {
            let v = deduper.dedup_values(&self.values, !self.values_have_no_objects());
            (!Arc::ptr_eq(&self.values, &v)).then_some(v)
        } else {
            None
        };
        let new_dress = if let Some(dress) = &self.dress {
            let d = deduper.dedup_dress(dress);
            (!Arc::ptr_eq(dress, &d)).then_some(d)
        } else {
            None
        };
        let new_steps = match &self.steps {
            Steps::Listed(Listing::CompactGrid(
                CompactGrid::OneStep(_) | CompactGrid::OneStepVal(_),
            )) => None,
            Steps::Listed(Listing::CompactGrid(g)) => {
                let c = deduper.dedup_compact_grid(g);
                (!g.ptr_eq(&c)).then_some(Steps::Listed(Listing::CompactGrid(c)))
            }
            Steps::Listed(Listing::Grid(g)) if g.can_intern() => {
                let c = deduper.dedup_grid(g);
                (!Arc::ptr_eq(g, &c)).then_some(Steps::Listed(Listing::Grid(c)))
            }
            _ => None,
        };
        if new_values.is_none() && new_dress.is_none() && new_steps.is_none() {
            return None;
        }
        let mut cloned = self.clone();
        if let Some(v) = new_values {
            cloned.values = v;
        }
        if let Some(d) = new_dress {
            cloned.dress = Some(d);
        }
        if let Some(s) = new_steps {
            cloned.steps = s;
        }
        Some(cloned)
    }

    fn new(period: Fraction, raws: Vec<Raw>, values: Arc<[Value]>) -> Self {
        let in_cycles = lie_in_spans(period, raws.iter().map(Raw::part));
        Self::of_list(period, raws, in_cycles, values)
    }

    fn of_single(period: Fraction, raw: Raw, values: Arc<[Value]>) -> Self {
        let in_cycles = lie_in_spans(period, std::iter::once(raw.part()));
        let steps = Steps::Listed(Listing::of_single(raw));
        Self::holding(period, steps, in_cycles, values)
    }

    /// The layer of `raws`, which it keeps in a list.
    fn of_list(period: Fraction, raws: Vec<Raw>, in_cycles: bool, values: Arc<[Value]>) -> Self {
        let steps = Steps::Listed(Listing::of(raws));
        Self::holding(period, steps, in_cycles, values)
    }

    /// The layer of `layer`, whose steps are already in cycles with
    /// pre-deduplicated value indices.
    fn of_indexed(mut layer: IndexedLayer) -> Self {
        Self::of_indexed_slice(
            layer.period,
            layer.values,
            layer.in_cycles,
            &mut layer.steps,
        )
    }

    /// Build a layer directly from a mutable slice of uncut [`SourceStep`]s in
    /// cycles, allowing callers to reuse their step buffer across layers.
    fn of_indexed_slice(
        period: Fraction,
        values: Arc<[Value]>,
        in_cycles: bool,
        steps: &mut [SourceStep],
    ) -> Self {
        let in_cycles = in_cycles
            || lie_in_spans(
                period,
                steps.iter().map(|step| TimeSpan::new(step.begin, step.end)),
            );
        if let Some(listing) = Grid::build_indexed_listing(steps) {
            return Self::holding(period, Steps::Listed(listing), in_cycles, values);
        }
        let raws: Vec<Raw> = steps
            .iter_mut()
            .map(|step| Raw {
                whole: TimeSpan::new(step.begin, step.end),
                part: None,
                value: step.value,
                locations: std::mem::take(&mut step.locations),
            })
            .collect();
        Self::of_list(period, raws, in_cycles, values)
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

    /// Classify `values` in one pass as `(all_data, all_maps, no_objects)`.
    #[inline]
    fn classify_values(values: &[Value]) -> (bool, bool, bool) {
        let mut data = true;
        let mut maps = true;
        let mut no_objects = true;
        for value in values {
            if data && !value.is_data(0) {
                data = false;
            }
            if matches!(value, Value::Object(_)) {
                no_objects = false;
            } else {
                maps = false;
            }
            if !data && !maps && !no_objects {
                break;
            }
        }
        (data, maps, no_objects)
    }

    /// A layer of `steps` that hold `values`, with no fold on it.
    fn holding(period: Fraction, steps: Steps, in_cycles: bool, values: Arc<[Value]>) -> Self {
        let (data, maps, no_objects) = Self::classify_values(&values);
        Self::holding_classified(period, steps, in_cycles, values, data, maps, no_objects)
    }

    fn holding_classified(
        period: Fraction,
        steps: Steps,
        in_cycles: bool,
        values: Arc<[Value]>,
        data: bool,
        maps: bool,
        no_objects: bool,
    ) -> Self {
        let flags = (if in_cycles { LAYER_FLAG_IN_CYCLES } else { 0 })
            | (if data { LAYER_FLAG_DATA } else { 0 })
            | (if maps { LAYER_FLAG_MAPS } else { 0 });
        let (period_numer, denom, steps) = Self::pack_period(period, steps);
        Self {
            steps,
            values,
            dress: None,
            period_numer,
            period_denom_flags: denom | flags,
            named_id: 0,
            direct_flags: if no_objects && !maps {
                LAYER_VALUES_NO_OBJECTS
            } else {
                0
            },
        }
    }

    #[inline(always)]
    fn values_have_no_objects(&self) -> bool {
        self.direct_flags & LAYER_VALUES_NO_OBJECTS != 0
    }

    #[inline]
    fn pack_period(period: Fraction, steps: Steps) -> (i16, u16, Steps) {
        if let (Ok(n), Ok(d)) = (i16::try_from(period.numer()), u16::try_from(period.denom()))
            && d != 0
            && d <= LAYER_DENOM_MASK
        {
            (n, d, steps)
        } else {
            (0, 0, Steps::WithPeriod(Arc::new((period, steps))))
        }
    }

    #[inline(always)]
    fn is_direct(&self) -> bool {
        self.direct_flags & (LAYER_DIRECT_KEYED | LAYER_DIRECT_SPLIT) != 0
    }

    #[inline(always)]
    fn is_direct_keyed(&self) -> bool {
        self.direct_flags & LAYER_DIRECT_KEYED != 0
    }

    #[inline(always)]
    fn seq_named_mark(&self) -> u64 {
        1u64 << ((self.direct_flags >> LAYER_MARK_BIT_SHIFT) & LAYER_MARK_BIT_MASK)
    }

    #[inline(always)]
    fn seq_packed_key(&self) -> u16 {
        let mark_bit = (self.direct_flags >> LAYER_MARK_BIT_SHIFT) & LAYER_MARK_BIT_MASK;
        self.named_id | (mark_bit << 10)
    }

    #[inline(always)]
    fn has_name(&self) -> bool {
        self.named_id != 0 || self.dress.as_deref().is_some_and(Dress::has_custom_name)
    }

    #[inline]
    fn named_names(&self) -> Option<&[Arc<str>]> {
        if self.named_id != 0 {
            crate::controls::builtin_names_by_id(self.named_id).map(|names| &**names)
        } else {
            self.dress.as_deref().and_then(Dress::custom_named)
        }
    }

    /// Fast variant of [`Self::refresh_direct`] after only `dress.set_map` or
    /// `dress.before` changed while `values` and `named_id` stayed unchanged.
    fn refresh_direct_after_set(&mut self) {
        if self.named_id != 0 && self.is_direct_keyed() {
            let dress = self.dress();
            if !self.spills_contexts(dress)
                && dress
                    .tail
                    .as_deref()
                    .is_none_or(|t| t.set_other.is_none() && t.custom_named.is_none())
            {
                let mark_bit =
                    ((self.direct_flags >> LAYER_MARK_BIT_SHIFT) & LAYER_MARK_BIT_MASK) as u8;
                let key_mark = 1u64 << mark_bit;
                let tail_ok = if !dress.has_set_map() {
                    true
                } else {
                    let keys = dress.set_packed_keys();
                    let tail = dress.set_slice();
                    let drops = keys.len() == 1
                        && crate::controls::builtin_name_str_by_id(keys[0] & 0x03ff)
                            == Some("value")
                        && !matches!(tail.value_at(0), Value::Undefined);
                    let disjoint = dress.set_marks() & key_mark == 0
                        || !keys.iter().any(|&k| (k & 0x03ff) == self.named_id);
                    !drops && disjoint && tail.all_values_data()
                };
                if tail_ok {
                    if dress.has_set_map() {
                        self.direct_flags |= LAYER_DIRECT_MADE;
                    } else {
                        self.direct_flags &= !LAYER_DIRECT_MADE;
                    }
                    self.steps.strip_cache();
                    return;
                }
            }
        }
        self.refresh_direct();
    }

    /// Recompute direct-read flags and attach or strip the rare lazy
    /// `Steps::Cached` cell whenever `steps`, `values`, `named_id`, or `dress`
    /// changes.
    #[inline]
    fn refresh_direct(&mut self) {
        self.refresh_direct_inner(false);
    }

    /// Fast variant of [`Self::refresh_direct`] when only `named_id` changed
    /// while `values` stayed untouched, reusing `LAYER_VALUES_NO_OBJECTS`.
    #[inline]
    fn refresh_direct_after_name(&mut self) {
        let no_objects = self.values_have_no_objects();
        self.refresh_direct_inner(no_objects);
    }

    fn refresh_direct_inner(&mut self, known_no_objects: bool) {
        if self.dress.is_none() {
            if self.named_id == 0 {
                let no_objects = known_no_objects
                    || (!self.maps() && self.values.iter().all(|v| !matches!(v, Value::Object(_))));
                self.direct_flags = if no_objects {
                    LAYER_VALUES_NO_OBJECTS
                } else {
                    0
                };
                self.steps.strip_cache();
                return;
            }
            if !self.values.is_empty()
                && u32::try_from(self.values.len()).is_ok()
                && self.is_data()
                && let Some(names) = crate::controls::builtin_names_by_id(self.named_id)
            {
                let (all_one, no_objects) = if known_no_objects && names.len() == 1 {
                    (true, true)
                } else {
                    let mut all_one = true;
                    let mut no_objects = true;
                    for value in self.values.iter() {
                        if matches!(value, Value::Object(_)) {
                            no_objects = false;
                        }
                        if !crate::controls::names_as_one_entry(names, value) {
                            all_one = false;
                            break;
                        }
                    }
                    (all_one, no_objects)
                };
                if all_one {
                    let mark_bit = crate::controls::builtin_mark_bit_by_id(self.named_id);
                    self.direct_flags = LAYER_DIRECT_KEYED
                        | if no_objects {
                            LAYER_VALUES_NO_OBJECTS
                        } else {
                            0
                        }
                        | (u16::from(mark_bit) << LAYER_MARK_BIT_SHIFT);
                    self.steps.strip_cache();
                    return;
                }
            }
            self.direct_flags = 0;
            self.steps.ensure_fresh_cache();
            return;
        }
        let dress = self.dress();
        let needs_values = self.has_name() || dress.has_set();
        let spills = self.spills_contexts(dress);
        if !needs_values && !spills {
            let no_objects = known_no_objects
                || (!self.maps() && self.values.iter().all(|v| !matches!(v, Value::Object(_))));
            self.direct_flags = if no_objects {
                LAYER_VALUES_NO_OBJECTS
            } else {
                0
            };
            self.steps.strip_cache();
            return;
        }
        if !spills
            && !self.values.is_empty()
            && u32::try_from(self.values.len()).is_ok()
            && self.is_data()
            && dress
                .tail
                .as_deref()
                .is_none_or(|t| t.set_other.is_none() && t.custom_named.is_none())
        {
            if self.named_id != 0 {
                if let Some(names) = crate::controls::builtin_names_by_id(self.named_id) {
                    let (all_one, no_objects) = if known_no_objects && names.len() == 1 {
                        (true, true)
                    } else {
                        let mut all_one = true;
                        let mut no_objects = true;
                        for value in self.values.iter() {
                            if matches!(value, Value::Object(_)) {
                                no_objects = false;
                            }
                            if !crate::controls::names_as_one_entry(names, value) {
                                all_one = false;
                                break;
                            }
                        }
                        (all_one, no_objects)
                    };
                    if all_one {
                        let mark_bit = crate::controls::builtin_mark_bit_by_id(self.named_id);
                        let key_mark = 1u64 << mark_bit;
                        let tail_ok = if !dress.has_set_map() {
                            true
                        } else {
                            let keys = dress.set_packed_keys();
                            let tail = dress.set_slice();
                            let drops = keys.len() == 1
                                && crate::controls::builtin_name_str_by_id(keys[0] & 0x03ff)
                                    == Some("value")
                                && !matches!(tail.value_at(0), Value::Undefined);
                            let disjoint = dress.set_marks() & key_mark == 0
                                || !keys.iter().any(|&k| (k & 0x03ff) == self.named_id);
                            !drops && disjoint && tail.all_values_data()
                        };
                        if tail_ok {
                            self.direct_flags = LAYER_DIRECT_KEYED
                                | if dress.has_set_map() {
                                    LAYER_DIRECT_MADE
                                } else {
                                    0
                                }
                                | if no_objects {
                                    LAYER_VALUES_NO_OBJECTS
                                } else {
                                    0
                                }
                                | (u16::from(mark_bit) << LAYER_MARK_BIT_SHIFT);
                            self.steps.strip_cache();
                            return;
                        }
                    }
                }
            } else if dress.has_set_map() {
                let keys = dress.set_packed_keys();
                let tail = dress.set_slice();
                let tail_marks = dress.set_marks();
                let drops = keys.len() == 1
                    && crate::controls::builtin_name_str_by_id(keys[0] & 0x03ff) == Some("value")
                    && !matches!(tail.value_at(0), Value::Undefined);
                if !drops
                    && tail.all_values_data()
                    && self.values.iter().all(|value| {
                        let Value::Object(map) = value else {
                            return false;
                        };
                        map.as_flat_slice().is_some_and(|head| match head {
                            crate::value::Slice::Plain(pairs) => !pairs.iter().any(|(k, _)| {
                                tail_marks & Table::mark(k.as_str()) != 0
                                    && tail.position_str(k.as_str()).is_some()
                            }),
                            crate::value::Slice::Builtin(hkeys, _)
                            | crate::value::Slice::Pooled(hkeys, _, _) => {
                                !hkeys.iter().any(|&hk| {
                                    let hbit = hk >> 10;
                                    tail_marks & (1u64 << hbit) != 0 && keys.contains(&hk)
                                })
                            }
                        })
                    })
                {
                    self.direct_flags = LAYER_DIRECT_SPLIT | LAYER_DIRECT_MADE;
                    self.steps.strip_cache();
                    return;
                }
            }
        }
        self.direct_flags = 0;
        self.steps.ensure_fresh_cache();
    }

    #[inline(always)]
    pub(crate) fn seq_layout(&self, row: u32) -> crate::value::Layout<'_> {
        let tail = self
            .dress
            .as_deref()
            .map_or(crate::value::Slice::EMPTY, Dress::set_slice);
        if self.is_direct_keyed() {
            let key = crate::controls::builtin_key_ref_by_id(self.named_id)
                .expect("direct-keyed layer has built-in control id");
            return crate::value::Layout::Keyed(key, &self.values[row as usize], tail);
        }
        debug_assert!(self.direct_flags & LAYER_DIRECT_SPLIT != 0);
        let Value::Object(map) = &self.values[row as usize] else {
            unreachable!("direct-split layer holds object values");
        };
        let head = map
            .as_flat_slice()
            .expect("direct-split layer holds flat maps");
        crate::value::Layout::Split(head, tail)
    }

    #[inline(always)]
    pub(crate) fn seq_len(&self, row: u32) -> usize {
        let tail_len = self.dress.as_deref().map_or(0, Dress::set_len);
        if self.is_direct_keyed() {
            return 1 + tail_len;
        }
        debug_assert!(self.direct_flags & LAYER_DIRECT_SPLIT != 0);
        let Value::Object(map) = &self.values[row as usize] else {
            unreachable!("direct-split layer holds object values");
        };
        map.as_flat_slice()
            .expect("direct-split layer holds flat maps")
            .len()
            + tail_len
    }

    #[inline(always)]
    pub(crate) fn seq_at(&self, row: u32, index: usize) -> (&crate::Key, &Value) {
        let tail = self
            .dress
            .as_deref()
            .map_or(crate::value::Slice::EMPTY, Dress::set_slice);
        if self.is_direct_keyed() {
            if index == 0 {
                let key = crate::controls::builtin_key_ref_by_id(self.named_id)
                    .expect("direct-keyed layer has built-in control id");
                return (key, &self.values[row as usize]);
            }
            return tail.at(index - 1);
        }
        debug_assert!(self.direct_flags & LAYER_DIRECT_SPLIT != 0);
        let Value::Object(map) = &self.values[row as usize] else {
            unreachable!("direct-split layer holds object values");
        };
        let head = map
            .as_flat_slice()
            .expect("direct-split layer holds flat maps");
        match index.checked_sub(head.len()) {
            None => head.at(index),
            Some(at) => tail.at(at),
        }
    }

    #[inline(always)]
    pub(crate) fn seq_marks(&self) -> u64 {
        if self.is_direct_keyed() {
            return self.seq_named_mark() | self.dress.as_deref().map_or(0, Dress::set_marks);
        }
        debug_assert!(self.direct_flags & LAYER_DIRECT_SPLIT != 0);
        !0u64 >> 1
    }

    #[inline(always)]
    pub(crate) fn seq_get(&self, row: u32, key: &str) -> Option<&Value> {
        let want_bit = Table::mark_bit(key);
        let want = 1u64 << want_bit;
        if self.is_direct_keyed() {
            let named_mark = self.seq_named_mark();
            let (tail, tail_marks) = match self.dress.as_deref() {
                Some(dress) => (dress.set_slice(), dress.set_marks()),
                None => (crate::value::Slice::EMPTY, 0),
            };
            if (named_mark | tail_marks) & want == 0 {
                return None;
            }
            if named_mark == want
                && crate::controls::builtin_name_str_by_id(self.named_id) == Some(key)
            {
                return Some(&self.values[row as usize]);
            }
            if tail_marks & want != 0 {
                return tail.get_str(key);
            }
            return None;
        }
        debug_assert!(self.direct_flags & LAYER_DIRECT_SPLIT != 0);
        let Value::Object(map) = &self.values[row as usize] else {
            unreachable!("direct-split layer holds object values");
        };
        let head = map
            .as_flat_slice()
            .expect("direct-split layer holds flat maps");
        if let Some(found) = head.get_str(key) {
            return Some(found);
        }
        if let Some(dress) = self.dress.as_deref()
            && dress.set_marks() & want != 0
        {
            return dress.set_slice().get_str(key);
        }
        None
    }

    #[inline(always)]
    pub(crate) fn seq_position(&self, row: u32, key: &str) -> Option<usize> {
        let want_bit = Table::mark_bit(key);
        let want = 1u64 << want_bit;
        if self.is_direct_keyed() {
            let named_mark = self.seq_named_mark();
            let (tail, tail_marks) = match self.dress.as_deref() {
                Some(dress) => (dress.set_slice(), dress.set_marks()),
                None => (crate::value::Slice::EMPTY, 0),
            };
            if (named_mark | tail_marks) & want == 0 {
                return None;
            }
            if named_mark == want
                && crate::controls::builtin_name_str_by_id(self.named_id) == Some(key)
            {
                return Some(0);
            }
            if tail_marks & want != 0 {
                return tail.position_str(key).map(|at| at + 1);
            }
            return None;
        }
        debug_assert!(self.direct_flags & LAYER_DIRECT_SPLIT != 0);
        let Value::Object(map) = &self.values[row as usize] else {
            unreachable!("direct-split layer holds object values");
        };
        let head = map
            .as_flat_slice()
            .expect("direct-split layer holds flat maps");
        if let Some(at) = head.position_str(key) {
            return Some(at);
        }
        if let Some(dress) = self.dress.as_deref()
            && dress.set_marks() & want != 0
        {
            return dress
                .set_slice()
                .position_str(key)
                .map(|at| head.len() + at);
        }
        None
    }

    #[inline(always)]
    pub(crate) fn seq_has_uniform_keys(&self) -> bool {
        self.is_direct_keyed()
    }

    /// Whether two direct-keyed layers share the exact same key layout and
    /// made-ness so sibling chord layers can share one `Patched` or
    /// `StepPatch` overlay while each hap reads values from its own layer.
    #[inline]
    pub(crate) fn seq_same_key_layout(&self, other: &Self) -> bool {
        if !self.is_direct_keyed() || !other.is_direct_keyed() {
            return false;
        }
        if self.named_id != other.named_id || self.seq_is_made() != other.seq_is_made() {
            return false;
        }
        match (self.dress.as_ref(), other.dress.as_ref()) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a.same_set_keys(b),
            (Some(d), None) | (None, Some(d)) => !d.has_set_map(),
        }
    }

    #[inline(always)]
    pub(crate) fn seq_is_made(&self) -> bool {
        self.direct_flags & LAYER_DIRECT_MADE != 0
    }

    #[inline(always)]
    pub(crate) fn seq_keyed_parts(&self) -> Option<crate::value::KeyedParts<'_>> {
        if !self.is_direct_keyed() {
            return None;
        }
        let key = crate::controls::builtin_key_ref_by_id(self.named_id)?;
        let tail = self
            .dress
            .as_deref()
            .map_or(crate::value::Slice::EMPTY, Dress::set_slice);
        Some((key, &self.values, tail))
    }

    #[inline]
    pub(crate) fn seq_tail_arc(&self) -> Option<Arc<[(crate::Key, Value)]>> {
        self.dress.as_deref().and_then(|dress| {
            let slice = dress.set_slice();
            (!slice.is_empty()).then(|| slice.to_shares())
        })
    }

    #[inline(always)]
    fn in_cycles(&self) -> bool {
        self.period_denom_flags & LAYER_FLAG_IN_CYCLES != 0
    }

    #[inline(always)]
    fn set_in_cycles(&mut self, value: bool) {
        if value {
            self.period_denom_flags |= LAYER_FLAG_IN_CYCLES;
        } else {
            self.period_denom_flags &= !LAYER_FLAG_IN_CYCLES;
        }
    }

    #[inline(always)]
    fn maps(&self) -> bool {
        self.period_denom_flags & LAYER_FLAG_MAPS != 0
    }

    #[inline(always)]
    fn set_maps(&mut self, value: bool) {
        if value {
            self.period_denom_flags |= LAYER_FLAG_MAPS;
        } else {
            self.period_denom_flags &= !LAYER_FLAG_MAPS;
        }
    }

    #[inline(always)]
    fn set_data(&mut self, value: bool) {
        if value {
            self.period_denom_flags |= LAYER_FLAG_DATA;
        } else {
            self.period_denom_flags &= !LAYER_FLAG_DATA;
        }
    }

    #[inline]
    fn set_period_and_steps(&mut self, period: Fraction, steps: Steps) {
        let flags = self.period_denom_flags & !LAYER_DENOM_MASK;
        let (numer, denom, steps) = Self::pack_period(period, steps);
        self.period_numer = numer;
        self.period_denom_flags = denom | flags;
        self.steps = steps;
        self.refresh_direct();
    }

    #[inline]
    fn set_steps(&mut self, steps: Steps) {
        if self.period_denom_flags & LAYER_DENOM_MASK != 0 {
            self.steps = steps;
        } else {
            let period = self.period();
            self.steps = Steps::WithPeriod(Arc::new((period, steps)));
        }
        self.refresh_direct();
    }

    #[inline]
    fn period(&self) -> Fraction {
        let denom = self.period_denom_flags & LAYER_DENOM_MASK;
        if denom == 1 {
            Fraction::int(i128::from(self.period_numer))
        } else if denom != 0 {
            Fraction::new(i128::from(self.period_numer), i128::from(denom))
        } else {
            let mut cur = &self.steps;
            loop {
                match cur {
                    Steps::WithPeriod(pair) => return pair.0,
                    Steps::Cached(pair) => cur = &pair.0,
                    _ => unreachable!("zero period denom requires Steps::WithPeriod"),
                }
            }
        }
    }

    #[inline]
    fn same_period(&self, other: &Self) -> bool {
        let a_denom = self.period_denom_flags & LAYER_DENOM_MASK;
        let b_denom = other.period_denom_flags & LAYER_DENOM_MASK;
        if a_denom != b_denom || self.period_numer != other.period_numer {
            return false;
        }
        a_denom != 0 || self.period() == other.period()
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
        self.dress = (!dress.is_bare()).then(|| dress.into_arc());
        self.refresh_direct();
    }

    /// The list of the steps. A layer of a source makes it at the first
    /// call: a fold that moves or cuts steps reads it here, and a query
    /// does not.
    fn listing(&self) -> &Listing {
        match self.steps.inner() {
            Steps::Listed(listing) => listing,
            Steps::Source(sourced) => sourced.read.get_or_init(|| {
                let mut raws = Vec::with_capacity(sourced.steps.count());
                let written = sourced
                    .steps
                    .steps(Fraction::ZERO, self.period(), &mut |step| {
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
            Steps::WithPeriod(_) | Steps::Cached(_) => {
                unreachable!("inner unwraps WithPeriod and Cached")
            }
        }
    }

    /// The list of the steps, when it is made. A read of it makes none.
    fn listed(&self) -> Option<&Listing> {
        match self.steps.inner() {
            Steps::Listed(listing) => Some(listing),
            Steps::Source(sourced) => sourced.read.get(),
            Steps::WithPeriod(_) | Steps::Cached(_) => {
                unreachable!("inner unwraps WithPeriod and Cached")
            }
        }
    }

    /// The source of the steps, when no fold made their list.
    fn source(&self) -> Option<&dyn StepSource> {
        match self.steps.inner() {
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
        let data = values.iter().all(|value| value.is_data(0));
        self.set_data(data);
        self.set_maps(values.iter().all(|value| matches!(value, Value::Object(_))));
        self.values = values;
        self.named_id = 0;
        self.change_dress(|dress| {
            dress.clear_custom_named();
            dress.clear_set();
        });
        self
    }

    /// Name each value of these layers by `spec`, as the
    /// map of the control over its haps gives them. No value is made now:
    /// a chain of controls after the name makes each value one time, at
    /// the first read, and a layer that no query reads makes none.
    ///
    /// A layer that names or sets its values already [takes no
    /// name](Self::takes_name).
    fn named_arc(layers: &[Layer], spec: &crate::controls::ControlSpec) -> Arc<[Layer]> {
        let id = if spec.id != 0 {
            spec.id
        } else {
            crate::controls::builtin_id_of_names(&spec.names)
        };
        if id != 0 {
            return layers
                .iter()
                .map(|layer| {
                    let mut next = layer.clone();
                    next.named_id = id;
                    next.set_maps(true);
                    if next.dress.as_deref().is_some_and(Dress::has_custom_name) {
                        next.change_dress(Dress::clear_custom_named);
                    } else {
                        next.refresh_direct_after_name();
                    }
                    next
                })
                .collect();
        }
        let mut last: Option<(*const Dress, Option<Arc<Dress>>)> = None;
        layers
            .iter()
            .map(|layer| {
                let from = layer.dress.as_ref().map_or(std::ptr::null(), Arc::as_ptr);
                let dress = match &last {
                    Some((seen, dress)) if *seen == from => dress.clone(),
                    _ => {
                        let mut dress = layer.dress().clone();
                        dress.set_custom_named(&spec.names);
                        let dress = (!dress.is_bare()).then(|| dress.into_arc());
                        last = Some((from, dress.clone()));
                        dress
                    }
                };
                layer.with(|layer| {
                    layer.dress = dress;
                    layer.named_id = 0;
                    layer.set_maps(true);
                    layer.refresh_direct();
                })
            })
            .collect()
    }

    /// Name `layers` in place when they are unshared, avoiding `Vec<Layer>`,
    /// `Arc<[Layer]>`, and `Arc<NodeCell>` churn on `s("...")` / `note("...")`.
    fn named_in_place(layers: &mut [Layer], spec: &crate::controls::ControlSpec) {
        let id = if spec.id != 0 {
            spec.id
        } else {
            crate::controls::builtin_id_of_names(&spec.names)
        };
        if id != 0 {
            for layer in layers {
                layer.named_id = id;
                layer.set_maps(true);
                if layer.dress.as_deref().is_some_and(Dress::has_custom_name) {
                    layer.change_dress(Dress::clear_custom_named);
                } else {
                    layer.refresh_direct_after_name();
                }
            }
            return;
        }
        if let [layer] = layers {
            layer.named_id = 0;
            layer.set_maps(true);
            if let Some(dress) = layer.dress.as_mut().and_then(Arc::get_mut) {
                dress.set_custom_named(&spec.names);
                layer.refresh_direct();
            } else {
                layer.change_dress(|dress| dress.set_custom_named(&spec.names));
            }
            return;
        }
        let mut last: Option<(*const Dress, Arc<Dress>)> = None;
        for layer in layers {
            layer.named_id = 0;
            layer.set_maps(true);
            if let Some(dress) = layer.dress.as_mut().and_then(Arc::get_mut) {
                dress.set_custom_named(&spec.names);
                layer.refresh_direct();
                continue;
            }
            let from = layer.dress.as_ref().map_or(std::ptr::null(), Arc::as_ptr);
            let dress = match &last {
                Some((seen, dress)) if *seen == from => Arc::clone(dress),
                _ => {
                    let mut dress = layer.dress().clone();
                    dress.set_custom_named(&spec.names);
                    let dress = dress.into_arc();
                    last = Some((from, Arc::clone(&dress)));
                    dress
                }
            };
            layer.dress = Some(dress);
            layer.refresh_direct();
        }
    }

    /// Whether [`Self::named`] names the values of this layer: it holds no
    /// name and no control yet. When `unnamed_only` is true, also requires
    /// every value to be a plain non-object data value with no slider binding.
    fn takes_name(&self, unnamed_only: bool) -> bool {
        !self.has_name()
            && !self.dress().has_set()
            && (!unnamed_only
                || (self.dress().keys().get().slider_binding == 0
                    && self
                        .values
                        .iter()
                        .all(|v| !matches!(v, Value::Object(_) | Value::JsValue(_)))))
    }

    /// This layer where each value takes the keys of the one value of
    /// `values`, a map of controls that fills each cycle, as a `set` of it
    /// gives them. See [`Self::set_here`].
    ///
    /// `None` for any other meeting: a step that the constant cuts, values
    /// that are not all maps of controls, another op, and a meeting that
    /// [`Meeting::in_place`] does not make.
    fn set(&self, values: &Layer, apply: &Combine, flow: LookupFlow) -> Option<Self> {
        if !(values.fills_each_cycle() && values.dress().keys().is_empty()) {
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
    fn renoted_cached(
        &self,
        map: &HapMapFn,
        note: &NoteFn,
        known_keys: Option<&ContextKeys>,
        dress_cache: &mut RenoteDressCache,
    ) -> Option<Self> {
        let names = self.named_names().filter(|_| !self.dress().has_set())?;
        let [name] = names else {
            return None;
        };
        if !matches!(&**name, "note" | "n") {
            return None;
        }
        let (values, data, no_objects) = if let Some((v, d)) = dress_cache.get_values(&self.values)
        {
            (v, d, false)
        } else {
            let renoted = |value: &Value| match value {
                Value::F64(_) | Value::Str(_) => note(value),
                _ => None,
            };
            let mut data = true;
            let mut no_objects = true;
            let values: Arc<[Value]> = if self.values.len() <= 16 {
                let mut buf: [Value; 16] = [const { Value::Undefined }; 16];
                let len = self.values.len();
                for (slot, val) in buf[..len].iter_mut().zip(self.values.iter()) {
                    let mapped = renoted(val)?;
                    if data && !mapped.is_data(0) {
                        data = false;
                    }
                    if matches!(mapped, Value::Object(_)) {
                        no_objects = false;
                    }
                    *slot = mapped;
                }
                buf[..len]
                    .iter_mut()
                    .map(|slot| std::mem::replace(slot, Value::Undefined))
                    .collect()
            } else {
                let mut notes = Vec::with_capacity(self.values.len());
                for val in self.values.iter() {
                    let mapped = renoted(val)?;
                    if data && !mapped.is_data(0) {
                        data = false;
                    }
                    if matches!(mapped, Value::Object(_)) {
                        no_objects = false;
                    }
                    notes.push(mapped);
                }
                notes.into()
            };
            dress_cache.put_values(
                Arc::as_ptr(&self.values) as *const Value,
                self.values.len(),
                Arc::clone(&values),
                data,
            );
            (values, data, no_objects)
        };
        let note_id = crate::controls::note_control_id();
        let from = self.dress.as_ref().map_or(std::ptr::null(), Arc::as_ptr);
        let dress = match dress_cache.get(from) {
            Some(cached) => cached,
            None => {
                let keys = match known_keys {
                    Some(added) => {
                        if self.dress().keys().is_empty() {
                            added.clone()
                        } else if added.is_empty() {
                            self.dress().keys().clone()
                        } else {
                            ContextKeys::merge(added, self.dress().keys())
                        }
                    }
                    None => {
                        let first =
                            crate::controls::named_value(names, self.values.first()?.clone());
                        let mut read = cycle_hap(first);
                        read.keys = self.dress().keys().clone();
                        map(&read)?.keys
                    }
                };
                let dress = if note_id != 0
                    && self.dress().custom_named().is_none()
                    && !self.dress().has_set()
                    && keys.arc_ptr() == self.dress().keys().arc_ptr()
                {
                    self.dress.clone()
                } else {
                    let mut next = self.dress().clone();
                    if note_id != 0 {
                        next.clear_custom_named();
                    } else {
                        next.set_custom_named(&crate::controls::note_names());
                    }
                    next.clear_set();
                    next.set_keys(keys);
                    (!next.is_bare()).then(|| next.into_arc())
                };
                dress_cache.put(from, dress.clone());
                dress
            }
        };
        let mut layer = self.with(|_| {});
        layer.set_data(data);
        layer.set_maps(true);
        layer.values = values;
        layer.named_id = note_id;
        layer.dress = dress;
        layer.refresh_direct_inner(no_objects);
        Some(layer)
    }

    /// Whether each value of this layer takes `control` in place by a `set`
    /// of a constant of it. Each value is a map of data, so each meets the
    /// control as a map of data does, and no value is read to know it.
    fn takes_set(&self, control: &Value, flow: LookupFlow) -> bool {
        let set = Combine::Composed(ComposeOp::Set);
        let map = Value::Object(crate::OrderedMap::new());
        self.in_cycles()
            && self.maps()
            && self.is_data()
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
    }

    /// Whether each value is data only.
    #[inline(always)]
    pub(crate) fn is_data(&self) -> bool {
        self.period_denom_flags & LAYER_FLAG_DATA != 0
    }

    /// This layer with other places, values or locations.
    fn with(&self, change: impl FnOnce(&mut Self)) -> Self {
        let mut layer = self.clone();
        change(&mut layer);
        layer
    }

    /// Whether the period is more than zero, each step holds one of the
    /// values, and the steps are in the order of their haps. See
    /// [`Self::is_in_order`].
    fn is_sound(&self) -> bool {
        let period = self.period();
        if self.source().is_some() {
            return period > Fraction::ZERO;
        }
        let listing = self.listing();
        if let Some(Some(sound)) =
            listing.with_grid_view(|grid| grid.in_order_and_holds(period, Some(self.values.len())))
        {
            return sound;
        }
        let holds = |value: u32| (value as usize) < self.values.len();
        self.is_in_order() && (0..self.count()).all(|index| holds(listing.value(index)))
    }

    /// Whether the period is more than zero and the steps are in the order
    /// of their haps: the part of each lies inside its whole and inside the
    /// period, begins at or after the part before it and ends at or after
    /// it, and no part is empty.
    fn is_in_order(&self) -> bool {
        let period = self.period();
        // A source writes its steps in order.
        if self.source().is_some() {
            return period > Fraction::ZERO;
        }
        let listing = self.listing();
        if let Some(Some(in_order)) = listing.with_grid_view(|grid| grid.in_order(period)) {
            return in_order;
        }
        let inside = |index: usize| {
            let (whole, part) = (listing.whole(index), listing.part(index));
            Fraction::ZERO <= whole.begin
                && whole.begin <= part.begin
                && part.begin < part.end
                && part.end <= whole.end
                && whole.end <= period
        };
        let ordered = (1..listing.len()).all(|index| {
            let (a, b) = (listing.part(index - 1), listing.part(index));
            a.begin <= b.begin && a.end <= b.end
        });
        period > Fraction::ZERO && (0..listing.len()).all(inside) && ordered
    }

    /// The locations of the hap of step `index`: those before, the step's
    /// own, then those after.
    fn context(&self, index: usize) -> Locations {
        let dress = self.dress();
        let step_loc = self.locations_of(index);
        if dress.locates_alone() {
            step_loc
        } else {
            dress.around(&step_loc)
        }
    }

    /// The locations of step `index`, as the step holds them.
    fn locations_of(&self, index: usize) -> Locations {
        self.listing().locations(index)
    }

    /// Whether combining this layer's dress locations with a step location can
    /// spill `Locations::INLINE` (7), making a cached context table worthwhile.
    fn spills_contexts(&self, dress: &Dress) -> bool {
        let Some(listing) = self.listed() else {
            return false;
        };
        let dress_locs = dress.before_len() + dress.after().len();
        if dress_locs >= 7 {
            return true;
        }
        let max_step_locs = listing
            .with_grid_view(|grid| match grid.packed {
                PackedRunsRef::None => 1usize,
                PackedRunsRef::Small(runs) => {
                    let mut best = 1usize;
                    let mut cur = 0usize;
                    for &w in runs {
                        cur += 1;
                        if (w & (1 << 15)) != 0 {
                            best = best.max(cur);
                            cur = 0;
                        }
                    }
                    best
                }
                PackedRunsRef::Wide(runs) => {
                    let mut best = 1usize;
                    let mut cur = 0usize;
                    for &w in runs {
                        cur += 1;
                        if (w & PackedLoc::SPARE) != 0 {
                            best = best.max(cur);
                            cur = 0;
                        }
                    }
                    best
                }
            })
            .unwrap_or(1);
        dress_locs + max_step_locs > 7
    }

    /// What makes the haps of the steps of this layer, read once for a loop.
    #[inline]
    fn maker<'a>(&'a self, owner: Option<(&'a Arc<[Layer]>, u16)>) -> Maker<'a> {
        let dress = self.dress();
        let (values, contexts) = self.reader_and_contexts(dress, owner);
        Maker {
            listing: self.listing(),
            values,
            contexts,
            loc_dress: (contexts.is_none() && !dress.locates_alone()).then_some(dress),
            keys: dress.keys(),
        }
    }

    /// The values of the steps, each once, with the name of their control
    /// and the controls that a chain of constants set.
    pub(crate) fn value(&self, index: usize) -> Option<Value> {
        self.reader(None).get(index)
    }

    /// The values as the steps hold them, before the dress. The dress
    /// holds data only, so the purity of the layer is that of these.
    pub(crate) fn held_values(&self) -> &[Value] {
        &self.values
    }

    /// The values as the dress makes them, read once for a loop of steps.
    fn reader<'a>(&'a self, owner: Option<(&'a Arc<[Layer]>, u16)>) -> Values<'a> {
        self.reader_and_contexts(self.dress(), owner).0
    }

    fn reader_and_contexts<'a>(
        &'a self,
        dress: &Dress,
        owner: Option<(&'a Arc<[Layer]>, u16)>,
    ) -> (Values<'a>, Option<&'a [Locations]>) {
        if self.is_direct() {
            let values = match owner {
                Some((layers, layer_idx))
                    if self.is_direct_keyed() && self.named_id != 0 && !dress.has_set_map() =>
                {
                    Values::SingleBuiltin(layers, layer_idx, self.named_id, &self.values)
                }
                Some((layers, layer_idx)) => Values::SeqLayer(layers, layer_idx),
                None => Values::DirectKeyed(self),
            };
            return (values, None);
        }
        let Some(cache) = self.steps.cache_cell() else {
            return (Values::Held(&self.values), None);
        };
        let (view, contexts) = cache
            .get_or_init(|| self.read_values(dress))
            .as_layer_read();
        let values = match view {
            LayerReadView::Held => Values::Held(&self.values),
            LayerReadView::Made(values) => Values::Held(values),
            LayerReadView::Shared(table) => Values::Table(table),
        };
        (values, contexts)
    }

    /// [`Self::value`] of each step.
    pub(crate) fn each_value(&self) -> impl Iterator<Item = Value> + '_ {
        let values = self.reader(None);
        (0..self.values.len()).filter_map(move |index| values.get(index))
    }

    /// The values as `dress` makes them: one table that their maps read
    /// where that gives the same maps, else each value made on its own.
    #[cold]
    #[inline(never)]
    fn read_values(&self, dress: &Dress) -> Arc<Table> {
        let needs_values = self.has_name() || dress.has_set();
        let core = if !needs_values {
            LayerCachedValues::Held
        } else {
            let set_owned = dress.set_value();
            let set = match &set_owned {
                None => Some(None),
                Some(Value::Object(set)) if !union_drops(set) => Some(Some(set)),
                Some(_) => None,
            };
            let named = self.named_names();
            let key = match named {
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
                LayerCachedValues::Shared(Arc::new(table))
            } else if let (Some(Some(_)), Some(names)) = (set, named)
                && dress.has_set_map()
                && self.is_data()
                && !names.is_empty()
                && self.values.iter().all(|v| !matches!(v, Value::Object(_)))
                && let set_keys = dress.set_packed_keys()
                && !set_keys.is_empty()
                && let set_slice = dress.set_slice()
                && matches!(
                    set_slice,
                    crate::value::Slice::Builtin(..) | crate::value::Slice::Pooled(..)
                )
                && let max_named = self
                    .values
                    .iter()
                    .map(|v| {
                        if names.len() > 1
                            && let Value::List(list) = v
                        {
                            list.len().min(names.len())
                        } else {
                            1
                        }
                    })
                    .max()
                    .unwrap_or(0)
                && max_named <= 8
                && max_named + set_keys.len() <= crate::value::MAX_BUILTIN_MAP_LEN
                && let Some(packed_names) = {
                    let mut buf = [0u16; 8];
                    let mut ok = true;
                    for (slot, name) in buf[..max_named].iter_mut().zip(names.iter()) {
                        let p = crate::controls::pack_builtin_key_str(name);
                        if p == 0 {
                            ok = false;
                            break;
                        }
                        *slot = p;
                    }
                    ok.then_some(buf)
                }
            {
                LayerCachedValues::Made(
                    self.values
                        .iter()
                        .map(|value| {
                            let mut out_keys = [0u16; crate::value::MAX_BUILTIN_MAP_LEN];
                            let mut out_vals =
                                [const { Value::Undefined }; crate::value::MAX_BUILTIN_MAP_LEN];
                            let mut len = 0usize;
                            if names.len() > 1
                                && let Value::List(list) = value
                            {
                                for (&pk, elem) in packed_names[..max_named].iter().zip(list.iter())
                                {
                                    if let Some(pos) = out_keys[..len].iter().position(|&k| k == pk)
                                    {
                                        out_vals[pos] = elem.clone();
                                    } else {
                                        out_keys[len] = pk;
                                        out_vals[len] = elem.clone();
                                        len += 1;
                                    }
                                }
                            } else {
                                out_keys[0] = packed_names[0];
                                out_vals[0] = value.clone();
                                len = 1;
                            }
                            let named_len = len;
                            for (si, &sk) in set_keys.iter().enumerate() {
                                let sv = set_slice.value_at(si);
                                if let Some(pos) =
                                    out_keys[..named_len].iter().position(|&k| k == sk)
                                {
                                    out_vals[pos] = sv.clone();
                                } else {
                                    out_keys[len] = sk;
                                    out_vals[len] = sv.clone();
                                    len += 1;
                                }
                            }
                            Value::Object(crate::value::OrderedMap::from_made_builtin(
                                crate::value::make_builtin_arc_owned(
                                    &out_keys[..len],
                                    &mut out_vals[..len],
                                ),
                            ))
                        })
                        .collect(),
                )
            } else {
                LayerCachedValues::Made(
                    self.values
                        .iter()
                        .map(|value| {
                            // A handle of the value, or a copy of its text.
                            let mut value = match named {
                                Some(names) => crate::controls::named_value(names, value.clone()),
                                None => value.clone(),
                            };
                            dress.compose_set_into(&mut value);
                            value
                        })
                        .collect(),
                )
            }
        };
        if self.spills_contexts(dress) && self.source().is_none() {
            let listing = self.listing();
            let contexts: Box<[Locations]> = (0..listing.len())
                .map(|index| dress.around(&listing.locations(index)))
                .collect();
            return Arc::new(Table::layer_with_contexts(core, contexts));
        }
        match core {
            LayerCachedValues::Made(values) => Arc::new(Table::layer_made(values)),
            LayerCachedValues::Shared(table) => table,
            LayerCachedValues::Held => {
                unreachable!("LayerCachedValues::Held only built with spilling contexts")
            }
        }
    }

    /// Whether this layer and `other` answer the same haps, with values
    /// that `same_value` holds to be the same. The values are read as the
    /// layers hold them: the same values with the same name and the same
    /// controls set are the same, and no value is made to know it.
    pub(crate) fn same(&self, other: &Self, same_value: impl Fn(&Value, &Value) -> bool) -> bool {
        let same_named = self.named_id == other.named_id
            && (self.named_id != 0 || self.dress().custom_named() == other.dress().custom_named());
        if !same_named {
            return false;
        }
        let same_dress = match (&self.dress, &other.dress) {
            (Some(a), Some(b)) if Arc::ptr_eq(a, b) => true,
            (None, None) => true,
            _ => self.dress().same(other.dress(), &same_value),
        };
        if !same_dress || !self.same_period(other) {
            return false;
        }
        let (own, others) = match (self.steps.inner(), other.steps.inner()) {
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
        let same_times = own
            .with_grid_view(|a| others.with_grid_view(|b| a.same_times(b)).unwrap_or(false))
            .unwrap_or(false);
        own.len() == others.len()
            && (0..own.len()).all(|index| {
                (same_times
                    || (own.whole(index), own.part(index))
                        == (others.whole(index), others.part(index)))
                    && same_step(index)
            })
    }

    /// Push the haps of this layer in `turn` of its period, for a query of
    /// `span`. Outlined so `query_layers`'s multi-branch callers share one
    /// compact machine-code body instead of faulting duplicated copies.
    #[inline(never)]
    fn push(
        &self,
        thread: &QueryThread,
        owner: Option<(&Arc<[Layer]>, u16)>,
        turn: &Turn,
        span: TimeSpan,
        others: usize,
        out: &mut Vec<Hap>,
    ) -> Result<(), SequenceStop> {
        if let Some(source) = self.source() {
            let held = out.len();
            if let Some(pushed) = self.push_written(thread, owner, source, turn, span, others, out)
            {
                return pushed;
            }
            // The list of the steps answers a span that the source does
            // not read.
            out.truncate(held);
        }
        let owned;
        let places = match self.listing() {
            Listing::Places(listed) => &*listed.places,
            listing => {
                match listing
                    .with_grid_view(|grid| {
                        if let Some(pushed) =
                            self.push_grid(thread, owner, grid, turn, span, others, out)
                        {
                            Ok(pushed)
                        } else {
                            Err(grid.places())
                        }
                    })
                    .expect("grid listing")
                {
                    Ok(pushed) => return pushed,
                    Err(places) => {
                        owned = places;
                        &owned[..]
                    }
                }
            }
        };
        self.push_places(thread, owner, places, turn, span, others, out)
    }

    /// Push the haps of this layer in `turn`, as [`Self::push`] does, from
    /// the steps on a grid. The search compares ticks, and a time is made
    /// only for a hap. `None` for a turn that leaves the range of ticks.
    #[allow(clippy::too_many_arguments)]
    fn push_grid(
        &self,
        thread: &QueryThread,
        owner: Option<(&Arc<[Layer]>, u16)>,
        grid: GridView<'_>,
        turn: &Turn,
        span: TimeSpan,
        others: usize,
        out: &mut Vec<Hap>,
    ) -> Option<Result<(), SequenceStop>> {
        // Whole-turn power-of-two fast path (e.g. cycle-by-cycle SlowCatPure /
        // `iter` queries): skips `ticks_at`, binary search, and per-step
        // `dress.around_owned` when `step.loc == 0`.
        if grid.rare.is_none()
            && turn.begin == Fraction::ZERO
            && turn.end == self.period()
            && span.begin < span.end
            && grid.ticks.is_power_of_two()
            && let GridStepsRef::Small(steps) = grid.steps
            && steps.len() >= 2
            && let Some(cycles) = turn.whole_cycles
            && let Ok(cycles_i64) = i64::try_from(cycles)
            && let Some(offset) = cycles_i64.checked_mul(i64::from(grid.ticks.get()))
            && self.period().denom() == 1
            && let Ok(p_num) = i64::try_from(self.period().numer())
            && let Some(period_ticks) = p_num.checked_mul(i64::from(grid.ticks.get()))
            && i64::from(steps[steps.len() - 1].part_end) <= period_ticks
            && offset.checked_add(period_ticks).is_some()
        {
            return Some(
                self.push_grid_whole_turn(thread, owner, grid, steps, offset, others, out),
            );
        }
        let (begin_low, begin_high) = grid.ticks_at(turn.begin)?;
        let (end_low, end_high) = grid.ticks_at(turn.end)?;
        let begin_exact = begin_low == begin_high;
        let end_exact = end_low == end_high;
        let instant = span.begin == span.end;
        let grid_steps = grid.steps;
        let (first, last_step) = match grid_steps {
            GridStepsRef::Small(steps) => {
                if steps.len() == 1 {
                    let s = steps[0];
                    if i64::from(s.part_end) <= begin_low
                        || (instant && i64::from(s.part_begin) > begin_low)
                        || (!instant && i64::from(s.part_begin) >= end_high)
                    {
                        return Some(Ok(()));
                    }
                    (0, 1)
                } else {
                    let (Some(first_step), Some(last_step_ref)) = (steps.first(), steps.last())
                    else {
                        return Some(Ok(()));
                    };
                    if i64::from(last_step_ref.part_end) <= begin_low
                        || (instant && i64::from(first_step.part_begin) > begin_low)
                        || (!instant && i64::from(first_step.part_begin) >= end_high)
                    {
                        return Some(Ok(()));
                    }
                    let begun =
                        steps.partition_point(|step| i64::from(step.part_begin) < begin_high);
                    let first = begun
                        - steps[..begun]
                            .iter()
                            .rev()
                            .take_while(|step| i64::from(step.part_end) > begin_low)
                            .count();
                    let last_step = begun
                        + steps[begun..]
                            .iter()
                            .take_while(|step| {
                                let begins = i64::from(step.part_begin);
                                begins <= begin_low || (!instant && begins < end_high)
                            })
                            .count();
                    (first, last_step)
                }
            }
            GridStepsRef::Wide(steps) => {
                Self::find_wide_grid_steps(steps, begin_low, begin_high, end_high, instant)
            }
        };
        let met = last_step - first;
        if met == 0 {
            return Some(Ok(()));
        }
        // Single-step fast path (dominant case for short playback windows):
        // emit directly without `emit_range` closure, tick cache, or TLS lookup.
        if met == 1 {
            if !thread.charge(out.len() - others + 1) {
                return Some(Err(SequenceStop::Query));
            }
            let (step_begin, step_end, step_value) = match grid_steps {
                GridStepsRef::Small(steps) => {
                    let s = steps[first];
                    (
                        u32::from(s.part_begin),
                        u32::from(s.part_end),
                        u32::from(s.value),
                    )
                }
                GridStepsRef::Wide(steps) => {
                    let s = steps[first];
                    (s.part_begin, s.part_end, s.value)
                }
            };
            let offset = turn
                .whole_cycles
                .and_then(|cycles| i64::try_from(cycles).ok())
                .and_then(|cycles| cycles.checked_mul(i64::from(grid.ticks.get())));
            let ticks_pow2 = grid
                .ticks
                .is_power_of_two()
                .then(|| grid.ticks.trailing_zeros());
            let tick_time = |tick: u32| -> Result<Fraction, SequenceStop> {
                match offset.and_then(|offset| offset.checked_add(i64::from(tick))) {
                    Some(ticks) => {
                        if let Some(shift) = ticks_pow2
                            && let Some(dyadic) = Fraction::dyadic_i64(ticks, shift)
                        {
                            Ok(dyadic)
                        } else {
                            Ok(grid.made_small_with(ticks, None))
                        }
                    }
                    None => turn.moved(grid.made_small_with(i64::from(tick), None)),
                }
            };
            let vis_begin = if begin_exact && i64::from(step_begin) == begin_low {
                span.begin
            } else {
                match tick_time(step_begin) {
                    Ok(t) => t,
                    Err(e) => return Some(Err(e)),
                }
            };
            let vis_end = if end_exact && i64::from(step_end) == end_high {
                span.end
            } else {
                match tick_time(step_end) {
                    Ok(t) => t,
                    Err(e) => return Some(Err(e)),
                }
            };
            let visible = TimeSpan::new(vis_begin, vis_end);
            let whole = if let Some(rare) = grid.rare {
                match rare.cuts().starting_at(first).split_first_at(first) {
                    Some(cut) => {
                        let wb = match tick_time(cut.begin) {
                            Ok(t) => t,
                            Err(e) => return Some(Err(e)),
                        };
                        let we = match tick_time(cut.end) {
                            Ok(t) => t,
                            Err(e) => return Some(Err(e)),
                        };
                        TimeSpan::new(wb, we)
                    }
                    None => visible,
                }
            } else {
                visible
            };
            let part_begin = if i64::from(step_begin) <= begin_low {
                span.begin
            } else {
                visible.begin
            };
            let part_end = if i64::from(step_end) >= end_high {
                span.end
            } else {
                visible.end
            };
            let part = TimeSpan::new(part_begin, part_end);
            let maker = self.maker(owner);
            out.push(maker.make_grid(grid, first, step_value, whole, part));
            thread.note_accumulated(out.len() - others);
            return Some(Ok(()));
        }
        // The list held `others` haps before the sequence wrote.
        if !thread.charge(out.len() - others + met) {
            return Some(Err(SequenceStop::Query));
        }
        out.reserve(met);
        // The cuts are in the order of the steps: one search finds the first.
        let mut cuts = if grid.rare.is_some() {
            grid.cuts().starting_at(first)
        } else {
            CutsSlice::EMPTY
        };
        let has_cuts = cuts.len() != 0;
        // A turn that begins on a whole cycle moves a tick by whole cycles of
        // ticks, so a time is made one time, reduced, from its tick.
        let offset = turn
            .whole_cycles
            .and_then(|cycles| i64::try_from(cycles).ok())
            .and_then(|cycles| cycles.checked_mul(i64::from(grid.ticks.get())));
        let mut emit_range = |reductions: Option<&TickGrid>, cuts: &mut CutsSlice<'_>| {
            // The tick of the time made last, and the time: the end of a step is
            // the begin of the next, which most steps are.
            let mut last: Option<(u32, Fraction)> = None;
            let ticks_pow2 = grid
                .ticks
                .is_power_of_two()
                .then(|| grid.ticks.trailing_zeros());
            let mut at = |tick: u32| -> Result<Fraction, SequenceStop> {
                if let Some((made, time)) = last
                    && made == tick
                {
                    return Ok(time);
                }
                let time = match offset.and_then(|offset| offset.checked_add(i64::from(tick))) {
                    Some(ticks) => {
                        if let Some(shift) = ticks_pow2
                            && let Some(dyadic) = Fraction::dyadic_i64(ticks, shift)
                        {
                            dyadic
                        } else {
                            grid.made_small_with(ticks, reductions)
                        }
                    }
                    None => turn.moved(grid.made_small_with(i64::from(tick), reductions))?,
                };
                last = Some((tick, time));
                Ok(time)
            };
            let maker = self.maker(owner);
            (first..last_step).try_for_each(|index| {
                let (step_begin, step_end, step_value) = match grid_steps {
                    GridStepsRef::Small(steps) => {
                        let s = steps[index];
                        (
                            u32::from(s.part_begin),
                            u32::from(s.part_end),
                            u32::from(s.value),
                        )
                    }
                    GridStepsRef::Wide(steps) => {
                        let s = steps[index];
                        (s.part_begin, s.part_end, s.value)
                    }
                };
                let vis_begin = if begin_exact && i64::from(step_begin) == begin_low {
                    span.begin
                } else {
                    at(step_begin)?
                };
                let vis_end = if end_exact && i64::from(step_end) == end_high {
                    span.end
                } else {
                    at(step_end)?
                };
                let visible = TimeSpan::new(vis_begin, vis_end);
                let whole = if has_cuts {
                    match cuts.split_first_at(index) {
                        Some(cut) => TimeSpan::new(at(cut.begin)?, at(cut.end)?),
                        None => visible,
                    }
                } else {
                    visible
                };
                let part_begin = if i64::from(step_begin) <= begin_low {
                    span.begin
                } else {
                    visible.begin
                };
                let part_end = if i64::from(step_end) >= end_high {
                    span.end
                } else {
                    visible.end
                };
                let part = TimeSpan::new(part_begin, part_end);
                out.push(maker.make_grid(grid, index, step_value, whole, part));
                Ok(())
            })
        };
        let pushed = if grid.ticks.is_power_of_two() || met <= 2 {
            emit_range(None, &mut cuts)
        } else {
            with_tick_grid_if(grid.ticks, met >= 8, |reductions| {
                emit_range(reductions, &mut cuts)
            })
        };
        if pushed.is_ok() {
            thread.note_accumulated(out.len() - others);
        }
        Some(pushed)
    }

    /// Fast path for contiguous interior whole turns of an integer-period
    /// listed grid layer (`<= 64` uncut small steps). Precomputes turn-0 step
    /// spans and locations once, then shifts each turn by `checked_add_int`
    /// with no per-turn binary search, GCD, or location re-decoding.
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn push_whole_int_turns(
        &self,
        thread: &QueryThread,
        owner: Option<(&Arc<[Layer]>, u16)>,
        period_cycles: i128,
        first_turn_index: i128,
        whole_start: i128,
        whole_end: i128,
        others: usize,
        out: &mut Vec<Hap>,
    ) -> Option<Result<(), SequenceStop>> {
        if self.source().is_some() || period_cycles <= 0 || whole_end <= whole_start {
            return None;
        }
        let whole_count = usize::try_from(whole_end.checked_sub(whole_start)?).ok()?;
        if whole_count < 2 {
            return None;
        }
        let listing = self.listing();
        listing.with_grid_view(|grid| {
            if grid.rare.is_some() {
                return None;
            }
            let GridStepsRef::Small(steps) = grid.steps else {
                return None;
            };
            let len = steps.len();
            if len == 0 || len > 64 {
                return None;
            }
            let period_ticks = i64::try_from(period_cycles)
                .ok()?
                .checked_mul(i64::from(grid.ticks.get()))?;
            if i64::from(steps[len - 1].part_end) > period_ticks {
                return None;
            }
            let total_haps = len.checked_mul(whole_count)?;
            if !thread.charge(out.len() - others + total_haps) {
                return Some(Err(SequenceStop::Query));
            }
            out.reserve(total_haps);
            let ticks_pow2 = grid
                .ticks
                .is_power_of_two()
                .then(|| grid.ticks.trailing_zeros());
            let mut last: Option<(u16, Fraction)> = None;
            let mut at0 = |tick: u16| -> Fraction {
                if let Some((made, time)) = last
                    && made == tick
                {
                    return time;
                }
                let time = if let Some(shift) = ticks_pow2
                    && let Some(dyadic) = Fraction::dyadic_i64(i64::from(tick), shift)
                {
                    dyadic
                } else {
                    grid.made_small_with(i64::from(tick), None)
                };
                last = Some((tick, time));
                time
            };
            let maker = self.maker(owner);
            let mut base_spans = [TimeSpan::new(Fraction::ZERO, Fraction::ZERO); 64];
            let mut base_locs: [Locations; 64] = std::array::from_fn(|_| Locations::new());
            let mut step_vals = [0u16; 64];
            for i in 0..len {
                let s = steps[i];
                base_spans[i] = TimeSpan::new(at0(s.part_begin), at0(s.part_end));
                step_vals[i] = s.value;
                base_locs[i] = if let Some(contexts) = maker.contexts {
                    contexts[i].clone()
                } else {
                    let step_loc = grid.locations(i);
                    match maker.loc_dress {
                        Some(dress) => dress.around_owned(step_loc),
                        None => step_loc,
                    }
                };
            }
            for turn_idx in whole_start..whole_end {
                if turn_idx != first_turn_index && thread.stop_requested() {
                    return Some(Err(SequenceStop::Query));
                }
                let Some(shift) = turn_idx.checked_mul(period_cycles) else {
                    return Some(Err(SequenceStop::Range));
                };
                for i in 0..len {
                    let base = base_spans[i];
                    let (Some(b), Some(e)) = (
                        base.begin.checked_add_int(shift),
                        base.end.checked_add_int(shift),
                    ) else {
                        return Some(Err(SequenceStop::Range));
                    };
                    let ts = TimeSpan::new(b, e);
                    let value = maker
                        .values
                        .get(usize::from(step_vals[i]))
                        .expect("a step holds a value of its layer");
                    out.push(Hap::of_sequence(
                        ts,
                        ts,
                        value,
                        base_locs[i].clone(),
                        maker.keys.clone(),
                    ));
                }
            }
            thread.note_accumulated(out.len() - others);
            Some(Ok(()))
        })?
    }

    #[cold]
    #[inline(never)]
    fn find_wide_grid_steps(
        steps: &[GridStep],
        begin_low: i64,
        begin_high: i64,
        end_high: i64,
        instant: bool,
    ) -> (usize, usize) {
        if steps.len() == 1 {
            let s = steps[0];
            if i64::from(s.part_end) <= begin_low
                || (instant && i64::from(s.part_begin) > begin_low)
                || (!instant && i64::from(s.part_begin) >= end_high)
            {
                return (0, 0);
            }
            return (0, 1);
        }
        let (Some(first_step), Some(last_step_ref)) = (steps.first(), steps.last()) else {
            return (0, 0);
        };
        if i64::from(last_step_ref.part_end) <= begin_low
            || (instant && i64::from(first_step.part_begin) > begin_low)
            || (!instant && i64::from(first_step.part_begin) >= end_high)
        {
            return (0, 0);
        }
        let begun = steps.partition_point(|step| i64::from(step.part_begin) < begin_high);
        let first = begun
            - steps[..begun]
                .iter()
                .rev()
                .take_while(|step| i64::from(step.part_end) > begin_low)
                .count();
        let last_step = begun
            + steps[begun..]
                .iter()
                .take_while(|step| {
                    let begins = i64::from(step.part_begin);
                    begins <= begin_low || (!instant && begins < end_high)
                })
                .count();
        (first, last_step)
    }

    #[cold]
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn push_places(
        &self,
        thread: &QueryThread,
        owner: Option<(&Arc<[Layer]>, u16)>,
        places: &[Place],
        turn: &Turn,
        span: TimeSpan,
        others: usize,
        out: &mut Vec<Hap>,
    ) -> Result<(), SequenceStop> {
        let instant = span.begin == span.end;
        let (begin_key, end_key) = turn.keys();
        // The steps are in the order of time, so the first and the last
        // bound them all: a span before or after these bounds meets none.
        let (Some(earliest), Some(latest)) = (places.first(), places.last()) else {
            return Ok(());
        };
        let before = !earliest.begins_by(turn.begin, begin_key)
            && (instant || !earliest.begins_before(turn.end, end_key));
        if before || latest.part().end <= turn.begin {
            return Ok(());
        }
        // The steps that the span meets: those that begin before its end,
        // or at its one time, less those that end at its begin or before.
        // The steps are in the order of their begins and of their ends, so
        // the last ones that begin before the span are those that reach it.
        let begun = places.partition_point(|place| place.begins_before(turn.begin, begin_key));
        let reach = |place: &&Place| place.part().end > turn.begin;
        let first = begun - places[..begun].iter().rev().take_while(reach).count();
        let last = begun
            + places[begun..].partition_point(|place| {
                place.begins_by(turn.begin, begin_key)
                    || (!instant && place.begins_before(turn.end, end_key))
            });
        let met = last - first;
        if met == 0 {
            return Ok(());
        }
        // The list held `others` haps before the sequence wrote.
        if !thread.charge(out.len() - others + met) {
            return Err(SequenceStop::Query);
        }
        out.reserve(met);
        let maker = self.maker(owner);
        for (index, place) in places.iter().enumerate().skip(first).take(met) {
            let whole = TimeSpan::new(turn.moved(place.whole.begin)?, turn.moved(place.whole.end)?);
            let visible = match &place.part {
                None => whole,
                Some(part) => TimeSpan::new(turn.moved(part.begin)?, turn.moved(part.end)?),
            };
            let part = TimeSpan::new(visible.begin.max(span.begin), visible.end.min(span.end));
            out.push(maker.hap(index, whole, part));
        }
        thread.note_accumulated(out.len() - others);
        Ok(())
    }

    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn push_grid_whole_turn(
        &self,
        thread: &QueryThread,
        owner: Option<(&Arc<[Layer]>, u16)>,
        grid: GridView<'_>,
        steps: &[SmallGridStep],
        offset: i64,
        others: usize,
        out: &mut Vec<Hap>,
    ) -> Result<(), SequenceStop> {
        let met = steps.len();
        if !thread.charge(out.len() - others + met) {
            return Err(SequenceStop::Query);
        }
        out.reserve(met);
        let shift = grid.ticks.trailing_zeros();
        let maker = self.maker(owner);
        let zero_loc = maker.contexts.is_none().then(|| {
            maker
                .loc_dress
                .map_or_else(Locations::new, |d| d.around_owned(Locations::new()))
        });
        let mut last_tick = u16::MAX;
        let mut last_time = Fraction::ZERO;
        for (index, &s) in steps.iter().enumerate() {
            let b = if s.part_begin == last_tick {
                last_time
            } else {
                let Some(t) = Fraction::dyadic_i64(offset + i64::from(s.part_begin), shift) else {
                    return Err(SequenceStop::Range);
                };
                t
            };
            let Some(e) = Fraction::dyadic_i64(offset + i64::from(s.part_end), shift) else {
                return Err(SequenceStop::Range);
            };
            last_tick = s.part_end;
            last_time = e;
            let ts = TimeSpan::new(b, e);
            let value = maker
                .values
                .get(usize::from(s.value))
                .expect("a step holds a value of its layer");
            let context = if s.loc == 0
                && let Some(loc) = &zero_loc
            {
                loc.clone()
            } else if let Some(contexts) = maker.contexts {
                contexts[index].clone()
            } else {
                let step_loc = grid.locations(index);
                match maker.loc_dress {
                    Some(dress) => dress.around_owned(step_loc),
                    None => step_loc,
                }
            };
            out.push(Hap::of_sequence(ts, ts, value, context, maker.keys.clone()));
        }
        thread.note_accumulated(out.len() - others);
        Ok(())
    }

    /// Push the haps of the steps that `source` writes in `turn` of the
    /// period, for a query of `span`. `None` for a span that the source
    /// does not read.
    #[allow(clippy::too_many_arguments)]
    fn push_written(
        &self,
        thread: &QueryThread,
        owner: Option<(&Arc<[Layer]>, u16)>,
        source: &dyn StepSource,
        turn: &Turn,
        span: TimeSpan,
        others: usize,
        out: &mut Vec<Hap>,
    ) -> Option<Result<(), SequenceStop>> {
        let mut stop = None;
        let dress = self.dress();
        let mut values: Option<Values<'_>> = None;
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
            let values = values.get_or_insert_with(|| self.reader(owner));
            let value = values
                .get(step.value as usize)
                .expect("a step holds a value of its layer");
            let context = match dress.locates_alone() {
                true => step.locations,
                false => dress.around(&step.locations),
            };
            out.push(Hap::of_sequence(
                whole,
                part,
                value,
                context,
                dress.keys().clone(),
            ));
        });
        if let Some(stop) = stop {
            return Some(Err(stop));
        }
        written?;
        // The budget reads the steps of a turn after they are written.
        if !thread.charge(out.len() - others) {
            return Some(Err(SequenceStop::Query));
        }
        thread.note_accumulated(out.len() - others);
        Some(Ok(()))
    }

    /// Whether this layer is one step that fills each cycle, as a constant
    /// is.
    fn fills_each_cycle(&self) -> bool {
        let cycle = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
        self.period() == Fraction::ONE
            && self
                .one_step()
                .is_some_and(|(whole, cut, _)| whole == cycle && !cut)
    }
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
        GivenLayer::Indexed(layer) => Some(Layer::of_indexed(layer)),
        GivenLayer::Sourced(layer) => Some(Layer::of_source(layer)),
    });
    node(layers.collect::<Option<Vec<_>>>()?)
}

/// A single-layer [`sequence`] built directly from a reusable slice of uncut
/// [`SourceStep`]s in cycles, avoiding intermediate `Vec<GivenLayer>` and
/// `Vec<Layer>` allocations.
/// Build a single-layer sequence directly from integer tick steps
/// `(part_begin, part_end, value_idx, loc_begin, loc_len)` without constructing
/// intermediate `Fraction`s or `SourceStep`s. Returns `None` if any location
/// does not fit inline packing (caller falls back to the general path).
fn layer_of_ticks(
    period: Fraction,
    ticks: NonZeroU32,
    values: Arc<[Value]>,
    in_cycles: bool,
    raw_steps: &[(u16, u16, u16, u32, u16)],
) -> Option<Layer> {
    let (data, maps, no_objects) = Layer::classify_values(&values);
    layer_of_ticks_classified(
        period, ticks, values, in_cycles, raw_steps, data, maps, no_objects,
    )
}

/// Fast O(1) upper bound on grid tick index for `period` and `ticks`.
#[inline(always)]
fn grid_max_tick(period: Fraction, ticks: NonZeroU32) -> Option<u32> {
    if period <= Fraction::ZERO {
        return None;
    }
    let t = i128::from(ticks.get());
    let max_tick_i128 = if period.denom() == 1 {
        period.numer().checked_mul(t)?
    } else {
        period.numer().checked_mul(t)? / period.denom()
    };
    Some(u32::try_from(max_tick_i128).unwrap_or(u32::MAX))
}

/// Build a sequence node of a single layer already verified to be sound and non-empty.
#[inline]
fn node_of_one_sound(layer: Layer) -> Pattern {
    debug_assert!(layer.is_sound() && layer.count() > 0);
    let layers: Arc<[Layer]> = Arc::new([layer]);
    Pattern::of(Node::Sequence(layers))
}

#[allow(clippy::too_many_arguments)]
fn layer_of_ticks_classified(
    period: Fraction,
    ticks: NonZeroU32,
    values: Arc<[Value]>,
    in_cycles: bool,
    raw_steps: &[(u16, u16, u16, u32, u16)],
    data: bool,
    maps: bool,
    no_objects: bool,
) -> Option<Layer> {
    let max_tick = u16::try_from(grid_max_tick(period, ticks)?).unwrap_or(u16::MAX);
    let vlen = values.len();
    let mut base = u32::MAX;
    let mut max_begin = 0u32;
    let mut max_len = 0u16;
    let mut prev_begin = 0u16;
    let mut prev_end = 0u16;
    for &(part_begin, part_end, value, loc_begin, loc_len) in raw_steps {
        if part_begin >= part_end
            || part_end > max_tick
            || prev_begin > part_begin
            || prev_end > part_end
            || usize::from(value) >= vlen
        {
            return None;
        }
        prev_begin = part_begin;
        prev_end = part_end;
        if loc_len > 0 {
            base = base.min(loc_begin);
            max_begin = max_begin.max(loc_begin);
            max_len = max_len.max(loc_len);
        }
    }
    let has_locs = base != u32::MAX;
    let base = if has_locs { base } else { 0 };
    if base & Grid::SMALL != 0 {
        return None;
    }
    let all_small_loc = !has_locs
        || (max_begin - base < SmallGridStep::OFFSET_LIMIT && max_len < SmallGridStep::LENGTH_MASK);
    let (steps, rare) = if all_small_loc && raw_steps.len() <= MAX_INLINE_GRID_LEN {
        let mut small = [GridSteps::ZERO_SMALL; MAX_INLINE_GRID_LEN];
        for (slot, &(part_begin, part_end, value, loc_begin, loc_len)) in
            small.iter_mut().zip(raw_steps.iter())
        {
            let loc = if loc_len == 0 {
                0
            } else {
                (((loc_begin - base) as u16) << SmallGridStep::LENGTH_BITS) | (loc_len + 1)
            };
            *slot = SmallGridStep {
                part_begin,
                part_end,
                value,
                loc,
            };
        }
        (GridSteps::Inline(raw_steps.len() as u8, small), None)
    } else if all_small_loc && raw_steps.len() <= 64 {
        let mut small = [GridSteps::ZERO_SMALL; 64];
        for (slot, &(part_begin, part_end, value, loc_begin, loc_len)) in
            small.iter_mut().zip(raw_steps.iter())
        {
            let loc = if loc_len == 0 {
                0
            } else {
                (((loc_begin - base) as u16) << SmallGridStep::LENGTH_BITS) | (loc_len + 1)
            };
            *slot = SmallGridStep {
                part_begin,
                part_end,
                value,
                loc,
            };
        }
        (GridSteps::Small(Box::from(&small[..raw_steps.len()])), None)
    } else {
        let steps = GridSteps::build(raw_steps.len(), |index| {
            let (part_begin, part_end, value, loc_begin, loc_len) = raw_steps[index];
            let loc = if loc_len == 0 {
                PackedLoc(0)
            } else {
                PackedLoc::range_raw(base, loc_begin, u32::from(loc_len))?
            };
            Some(GridStep {
                part_begin: u32::from(part_begin),
                part_end: u32::from(part_end),
                value: u32::from(value),
                loc,
            })
        })?;
        match steps {
            GridSteps::Wide(wide_steps) => (
                GridSteps::Small(Box::from([])),
                Some(Box::new(SpareRare::WideSteps(wide_steps))),
            ),
            other => (other, None),
        }
    };
    let listing = Listing::from_parts(
        ticks,
        base | Grid::SMALL,
        steps,
        PackedRunsInput::None,
        rare,
    );
    Some(Layer::holding_classified(
        period,
        Steps::Listed(listing),
        in_cycles,
        values,
        data,
        maps,
        no_objects,
    ))
}

/// Compact raw tick step carrying up to two source ranges (`loc1_len == 0`
/// means a single range and `loc0_len == 0` means no location).
#[derive(Clone, Copy)]
pub struct TickStep {
    pub part_begin: u16,
    pub part_end: u16,
    pub value: u16,
    pub loc0_begin: u32,
    pub loc0_len: u16,
    pub loc1_begin: u32,
    pub loc1_len: u16,
}

/// Cut step whose `whole` differs from its `part`, in integer grid ticks.
#[derive(Clone, Copy)]
pub struct TickCut {
    pub index: u16,
    pub whole_begin: u16,
    pub whole_end: u16,
}

fn listing_of_cut_ticks(
    ticks: NonZeroU32,
    raw_steps: &[TickStep],
    cuts: &[TickCut],
) -> Option<Listing> {
    let mut base = u32::MAX;
    for step in raw_steps {
        if step.loc0_len > 0 {
            base = base.min(step.loc0_begin);
        }
        if step.loc1_len > 0 {
            base = base.min(step.loc1_begin);
        }
    }
    let base = if base == u32::MAX { 0 } else { base };
    if base & Grid::SMALL != 0 {
        return None;
    }
    let mut packed_runs = SmallCopyBuf::<u32, 32>::new(0);
    let steps = GridSteps::build(raw_steps.len(), |index| {
        let step = raw_steps[index];
        let loc = if step.loc0_len == 0 && step.loc1_len == 0 {
            PackedLoc(0)
        } else if step.loc1_len == 0 {
            PackedLoc::range_raw(base, step.loc0_begin, u32::from(step.loc0_len))?
        } else {
            let w0 = PackedLoc::range_raw(base, step.loc0_begin, u32::from(step.loc0_len))?.0;
            let w1 = PackedLoc::range_raw(base, step.loc1_begin, u32::from(step.loc1_len))?.0
                | PackedLoc::SPARE;
            let existing = packed_runs.as_slice();
            let mut found = None;
            let mut pos = 0;
            while pos + 2 <= existing.len() {
                if existing[pos] == w0 && existing[pos + 1] == w1 {
                    found = Some(pos);
                    break;
                }
                pos += 2;
            }
            let start = match found {
                Some(pos) => pos,
                None => {
                    let start = packed_runs.len();
                    if start >= PackedLoc::WIDE as usize {
                        return None;
                    }
                    packed_runs.push(w0);
                    packed_runs.push(w1);
                    start
                }
            };
            PackedLoc(PackedLoc::SPARE | start as u32)
        };
        Some(GridStep {
            part_begin: u32::from(step.part_begin),
            part_end: u32::from(step.part_end),
            value: u32::from(step.value),
            loc,
        })
    })?;
    let mut cuts_buf = SmallCopyBuf::<Cut, 16>::new(Cut {
        index: 0,
        begin: 0,
        end: 0,
    });
    for c in cuts {
        cuts_buf.push(Cut {
            index: u32::from(c.index),
            begin: u32::from(c.whole_begin),
            end: u32::from(c.whole_end),
        });
    }
    let (steps, rare) = match steps {
        GridSteps::Wide(wide_steps) => (
            GridSteps::Small(Box::from([])),
            Some(Box::new(SpareRare::from_parts(
                cuts_buf.as_slice(),
                Box::from([]),
                wide_steps,
            ))),
        ),
        other if !cuts_buf.is_empty() => (
            other,
            Some(Box::new(SpareRare::from_parts(
                cuts_buf.as_slice(),
                Box::from([]),
                Box::from([]),
            ))),
        ),
        other => (other, None),
    };
    let packed = if packed_runs.is_empty() {
        PackedRunsInput::None
    } else if rare.is_none()
        && let Some(small) = try_pack_runs_u16(packed_runs.as_slice())
    {
        PackedRunsInput::Small(small)
    } else {
        PackedRunsInput::Wide(packed_runs.into_boxed_slice())
    };
    Some(Listing::from_parts(
        ticks,
        base | Grid::SMALL,
        steps,
        packed,
        rare,
    ))
}

pub fn sequence_of_cut_ticks(
    period: Fraction,
    ticks: NonZeroU32,
    values: Arc<[Value]>,
    in_cycles: bool,
    raw_steps: &[TickStep],
    cuts: &[TickCut],
) -> Option<Pattern> {
    if raw_steps.is_empty() {
        return Some(silence());
    }
    if cuts.is_empty() {
        let max_tick = u16::try_from(grid_max_tick(period, ticks)?).unwrap_or(u16::MAX);
        let vlen = values.len();
        let mut prev_begin = 0u16;
        let mut prev_end = 0u16;
        for step in raw_steps {
            if step.part_begin >= step.part_end
                || step.part_end > max_tick
                || prev_begin > step.part_begin
                || prev_end > step.part_end
                || usize::from(step.value) >= vlen
            {
                return None;
            }
            prev_begin = step.part_begin;
            prev_end = step.part_end;
        }
        let listing = listing_of_cut_ticks(ticks, raw_steps, cuts)?;
        return Some(node_of_one_sound(Layer::holding(
            period,
            Steps::Listed(listing),
            in_cycles,
            values,
        )));
    }
    let listing = listing_of_cut_ticks(ticks, raw_steps, cuts)?;
    node_of_one(Layer::holding(
        period,
        Steps::Listed(listing),
        in_cycles,
        values,
    ))
}

pub fn sequence_of_ticks(
    period: Fraction,
    ticks: NonZeroU32,
    values: Arc<[Value]>,
    in_cycles: bool,
    raw_steps: &[(u16, u16, u16, u32, u16)],
) -> Option<Pattern> {
    if raw_steps.is_empty() {
        return Some(silence());
    }
    Some(node_of_one_sound(layer_of_ticks(
        period, ticks, values, in_cycles, raw_steps,
    )?))
}

/// Fast variant of [`sequence_of_ticks`] when the caller guarantees every
/// value is plain non-object data (e.g. mini-notation `Value::Str`/`Value::F64`).
pub fn sequence_of_ticks_plain(
    period: Fraction,
    ticks: NonZeroU32,
    values: Arc<[Value]>,
    in_cycles: bool,
    raw_steps: &[(u16, u16, u16, u32, u16)],
) -> Option<Pattern> {
    if raw_steps.is_empty() {
        return Some(silence());
    }
    debug_assert!(
        values
            .iter()
            .all(|v| v.is_data(0) && !matches!(v, Value::Object(_)))
    );
    Some(node_of_one_sound(layer_of_ticks_classified(
        period, ticks, values, in_cycles, raw_steps, true, false, true,
    )?))
}

/// Single-layer sequence of `values.len()` equal consecutive steps over
/// `period`, with no per-step locations.
pub fn sequence_of_equal_values(
    period: Fraction,
    values: Arc<[Value]>,
    in_cycles: bool,
) -> Option<Pattern> {
    let count = values.len();
    if count == 0 {
        return Some(silence());
    }
    if period <= Fraction::ZERO || period < Fraction::ONE {
        return None;
    }
    let ticks = NonZeroU32::new(u32::try_from(count).ok()?)?;
    let (data, maps, no_objects) = Layer::classify_values(&values);
    let steps = GridSteps::build(count, |index| {
        Some(GridStep {
            part_begin: index as u32,
            part_end: (index + 1) as u32,
            value: index as u32,
            loc: PackedLoc(0),
        })
    })?;
    let (steps, rare) = match steps {
        GridSteps::Wide(wide_steps) => (
            GridSteps::Small(Box::from([])),
            Some(Box::new(SpareRare::WideSteps(wide_steps))),
        ),
        other => (other, None),
    };
    let listing = Listing::from_parts(ticks, Grid::SMALL, steps, PackedRunsInput::None, rare);
    Some(node_of_one_sound(Layer::holding_classified(
        period,
        Steps::Listed(listing),
        in_cycles,
        values,
        data,
        maps,
        no_objects,
    )))
}

/// Single-layer sequence over `ticks` equal slots in `period`, omitting silent
/// slots. Each item in `slots` is `(tick_index, value)` in ascending tick order.
pub(crate) fn sequence_of_equal_slots(
    period: Fraction,
    ticks: usize,
    values: Arc<[Value]>,
    tick_indices: &[u32],
    in_cycles: bool,
) -> Option<Pattern> {
    if values.is_empty() {
        return Some(silence());
    }
    debug_assert_eq!(values.len(), tick_indices.len());
    let ticks = NonZeroU32::new(u32::try_from(ticks).ok()?)?;
    let (data, maps, no_objects) = Layer::classify_values(&values);
    let steps = GridSteps::build(values.len(), |index| {
        let tick = tick_indices[index];
        Some(GridStep {
            part_begin: tick,
            part_end: tick + 1,
            value: index as u32,
            loc: PackedLoc(0),
        })
    })?;
    let (steps, rare) = match steps {
        GridSteps::Wide(wide_steps) => (
            GridSteps::Small(Box::from([])),
            Some(Box::new(SpareRare::WideSteps(wide_steps))),
        ),
        other => (other, None),
    };
    let listing = Listing::from_parts(ticks, Grid::SMALL, steps, PackedRunsInput::None, rare);
    node_of_one(Layer::holding_classified(
        period,
        Steps::Listed(listing),
        in_cycles,
        values,
        data,
        maps,
        no_objects,
    ))
}

pub fn sequence_of_indexed_slice(
    period: Fraction,
    values: Arc<[Value]>,
    in_cycles: bool,
    steps: &mut [SourceStep],
) -> Option<Pattern> {
    let layer = Layer::of_indexed_slice(period, values, in_cycles, steps);
    node_of_one(layer)
}

/// A single-layer [`sequence`] built directly from one [`SourcedLayer`].
pub fn sequence_of_sourced(layer: SourcedLayer) -> Option<Pattern> {
    node_of_one(Layer::of_source(layer))
}

/// Incremental builder for multi-layer sequences, allowing callers to reuse a
/// single `Vec<SourceStep>` buffer across layers.
pub struct SequenceBuilder {
    inline: [Option<Layer>; 8],
    inline_len: usize,
    overflow: Vec<Layer>,
    /// Cached `(ptr, len, data, maps, no_objects)` for layers sharing one `Arc<[Value]>`.
    last_values: Option<(*const Value, usize, bool, bool, bool)>,
    /// True while every pushed layer has already passed soundness checks.
    all_sound: bool,
}

impl SequenceBuilder {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inline: Default::default(),
            inline_len: 0,
            overflow: if capacity > 8 {
                Vec::with_capacity(capacity)
            } else {
                Vec::new()
            },
            last_values: None,
            all_sound: true,
        }
    }

    #[inline]
    fn classify(&mut self, values: &Arc<[Value]>) -> (bool, bool, bool) {
        let ptr = Arc::as_ptr(values) as *const Value;
        let len = values.len();
        if let Some((last_ptr, last_len, data, maps, no_objects)) = self.last_values
            && last_ptr == ptr
            && last_len == len
        {
            return (data, maps, no_objects);
        }
        let (data, maps, no_objects) = Layer::classify_values(values);
        self.last_values = Some((ptr, len, data, maps, no_objects));
        (data, maps, no_objects)
    }

    fn push_layer(&mut self, layer: Layer) {
        if self.overflow.is_empty() && self.inline_len < self.inline.len() {
            self.inline[self.inline_len] = Some(layer);
            self.inline_len += 1;
        } else {
            if self.overflow.is_empty() {
                self.overflow.reserve(self.inline_len + 4);
                for slot in &mut self.inline[..self.inline_len] {
                    self.overflow.push(slot.take().expect("layer present"));
                }
                self.inline_len = 0;
            }
            self.overflow.push(layer);
        }
    }

    pub fn push_indexed_slice(
        &mut self,
        period: Fraction,
        values: Arc<[Value]>,
        in_cycles: bool,
        steps: &mut [SourceStep],
    ) {
        self.all_sound = false;
        let (data, maps, no_objects) = self.classify(&values);
        let in_cycles = in_cycles
            || lie_in_spans(
                period,
                steps.iter().map(|step| TimeSpan::new(step.begin, step.end)),
            );
        if let Some(listing) = Grid::build_indexed_listing(steps) {
            self.push_layer(Layer::holding_classified(
                period,
                Steps::Listed(listing),
                in_cycles,
                values,
                data,
                maps,
                no_objects,
            ));
            return;
        }
        let raws: Vec<Raw> = steps
            .iter_mut()
            .map(|step| Raw {
                whole: TimeSpan::new(step.begin, step.end),
                part: None,
                value: step.value,
                locations: std::mem::take(&mut step.locations),
            })
            .collect();
        self.push_layer(Layer::of_list(period, raws, in_cycles, values));
    }

    pub fn push_cut_ticks(
        &mut self,
        period: Fraction,
        ticks: NonZeroU32,
        values: Arc<[Value]>,
        in_cycles: bool,
        raw_steps: &[TickStep],
        cuts: &[TickCut],
    ) -> bool {
        if raw_steps.is_empty() {
            return true;
        }
        self.all_sound = false;
        let Some(listing) = listing_of_cut_ticks(ticks, raw_steps, cuts) else {
            return false;
        };
        self.push_layer(Layer::holding(
            period,
            Steps::Listed(listing),
            in_cycles,
            values,
        ));
        true
    }

    pub fn push_ticks(
        &mut self,
        period: Fraction,
        ticks: NonZeroU32,
        values: Arc<[Value]>,
        in_cycles: bool,
        raw_steps: &[(u16, u16, u16, u32, u16)],
    ) -> bool {
        if raw_steps.is_empty() {
            return true;
        }
        let (data, maps, no_objects) = self.classify(&values);
        let Some(layer) = layer_of_ticks_classified(
            period, ticks, values, in_cycles, raw_steps, data, maps, no_objects,
        ) else {
            return false;
        };
        self.push_layer(layer);
        true
    }

    pub fn push_ticks_plain(
        &mut self,
        period: Fraction,
        ticks: NonZeroU32,
        values: Arc<[Value]>,
        in_cycles: bool,
        raw_steps: &[(u16, u16, u16, u32, u16)],
    ) -> bool {
        if raw_steps.is_empty() {
            return true;
        }
        debug_assert!(
            values
                .iter()
                .all(|v| v.is_data(0) && !matches!(v, Value::Object(_)))
        );
        let Some(layer) = layer_of_ticks_classified(
            period, ticks, values, in_cycles, raw_steps, true, false, true,
        ) else {
            return false;
        };
        self.push_layer(layer);
        true
    }

    pub fn push_sourced(&mut self, layer: SourcedLayer) {
        self.all_sound = false;
        self.push_layer(Layer::of_source(layer));
    }

    pub fn finish(mut self) -> Option<Pattern> {
        if self.overflow.is_empty() {
            if self.all_sound
                || self.inline[..self.inline_len]
                    .iter()
                    .all(|l| l.as_ref().is_some_and(Layer::is_sound))
            {
                return Some(node_of_sound_stack(&mut self.inline, self.inline_len));
            }
            return None;
        }
        if self.all_sound {
            Some(node_of_sound(self.overflow))
        } else {
            node(self.overflow)
        }
    }
}

/// The sequence node of a single layer, when it is sound.
fn node_of_one(layer: Layer) -> Option<Pattern> {
    if !layer.is_sound() {
        return None;
    }
    if layer.count() == 0 {
        return Some(silence());
    }
    let layers: Arc<[Layer]> = Arc::new([layer]);
    Some(Pattern::of(Node::Sequence(layers)))
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

/// Build a sequence node directly into `Arc<[Layer]>` from a small stack buffer
/// of non-empty layers without allocating an intermediate `Vec<Layer>`.
fn node_of_sound_stack(layers: &mut [Option<Layer>], len: usize) -> Pattern {
    let mut write = 0usize;
    for read in 0..len {
        if layers[read].as_ref().is_some_and(|l| l.count() > 0) {
            if write != read {
                layers.swap(write, read);
            }
            write += 1;
        }
    }
    if write == 0 {
        return silence();
    }
    if write == 1 {
        let layer = layers[0].take().expect("one layer present");
        return node_of_one_sound(layer);
    }
    if write >= 2 {
        for i in 1..write {
            let Some(dress_i) = layers[i].as_ref().and_then(|l| l.dress.as_ref()) else {
                continue;
            };
            let mut found = None;
            for prev in &layers[..i] {
                if let Some(prev_dress) = prev.as_ref().and_then(|l| l.dress.as_ref())
                    && (Arc::ptr_eq(dress_i, prev_dress)
                        || dress_i.same(prev_dress, |a, b| a.same_data(b)))
                {
                    found = Some(Arc::clone(prev_dress));
                    break;
                }
            }
            if let Some(shared) = found
                && let Some(layer) = layers[i].as_mut()
            {
                layer.dress = Some(shared);
            }
        }
    }
    let arc: Arc<[Layer]> = layers[..write]
        .iter_mut()
        .map(|slot| slot.take().expect("layer present"))
        .collect();
    Pattern::of(Node::Sequence(arc))
}

/// One turn of a period, for a query of a span.
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
    /// The [keys](time_key) of `begin` and of `end`, made at the first read:
    /// a grid compares ticks and reads none.
    keys: std::cell::Cell<Option<(f32, f32)>>,
}

impl Turn {
    /// The [keys](time_key) of `begin` and of `end`.
    fn keys(&self) -> (f32, f32) {
        self.keys.get().unwrap_or_else(|| {
            let keys = (time_key(self.begin), time_key(self.end));
            self.keys.set(Some(keys));
            keys
        })
    }

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
        // Fast path for any positive whole-cycle period over a single turn.
        if period.denom() == 1 && period.numer() > 0 && span.begin < span.end {
            let p = period.numer();
            let first_cycle = span.begin.floor_int();
            let first = if p == 1 {
                first_cycle
            } else if p & (p - 1) == 0 {
                first_cycle >> p.trailing_zeros()
            } else {
                first_cycle.div_euclid(p)
            };
            if let Some(next) = first.checked_add(1)
                && let Some(next_start) = next.checked_mul(p)
            {
                let fits = if span.end.denom() == 1 {
                    span.end.numer() <= next_start
                } else {
                    span.end.floor_int() < next_start
                };
                if fits {
                    return Ok(Self {
                        period,
                        span,
                        indices: first..next,
                        read: None,
                    });
                }
            }
        }
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
        let (start, whole_cycles) = if self.period.denom() == 1 {
            let p = self.period.numer();
            let cycles = if p == 1 {
                index
            } else {
                index.checked_mul(p).ok_or(SequenceStop::Range)?
            };
            (Fraction::int(cycles), Some(cycles))
        } else {
            let start = in_range(self.period.checked_mul(Fraction::int(index)))?;
            let whole = (start.denom() == 1).then(|| start.numer());
            (start, whole)
        };
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
            keys: std::cell::Cell::new(None),
        };
        Ok(self.read.insert(turn))
    }
}

/// Single-turn fast path for non-integer positive rational periods (kept
/// out-of-line so [`query_layers`]'s hot integer-period path stays compact).
#[inline(never)]
fn single_turn_fractional(period: Fraction, span: TimeSpan) -> Option<Turn> {
    if period.numer() == 1 && (period.denom() as u128).is_power_of_two() {
        let shift = (period.denom() as u128).trailing_zeros();
        if let (Some(scaled_begin), Some(scaled_end)) = (
            span.begin.checked_mul_pow2(shift),
            span.end.checked_mul_pow2(shift),
        ) && let turn_idx = scaled_begin.floor_int()
            && let Some(next_turn) = turn_idx.checked_add(1)
            && (if scaled_end.denom() == 1 {
                scaled_end.numer() <= next_turn
            } else {
                scaled_end.floor_int() < next_turn
            })
            && let Some(start) = Fraction::dyadic(turn_idx, shift)
            && let Some(begin) = scaled_begin.cycle_pos().checked_div_pow2(shift)
            && let Some(end) = (if scaled_end.denom() == 1 {
                Fraction::ONE
            } else {
                scaled_end.cycle_pos()
            })
            .checked_div_pow2(shift)
        {
            return Some(Turn {
                index: turn_idx,
                start,
                whole_cycles: (start.denom() == 1).then(|| start.numer()),
                begin,
                end,
                keys: std::cell::Cell::new(None),
            });
        }
        return None;
    }
    if period.numer() > 0 {
        let p_n = period.numer();
        let p_d = period.denom();
        let b_n = span.begin.numer();
        let b_d = span.begin.denom();
        let e_n = span.end.numer();
        let e_d = span.end.denom();
        if let (Some(lhs_b), Some(rhs_b)) = (b_n.checked_mul(p_d), b_d.checked_mul(p_n))
            && let turn_idx = lhs_b.div_euclid(rhs_b)
            && let Some(next_turn) = turn_idx.checked_add(1)
            && let Some(next_num) = next_turn.checked_mul(p_n)
            && let (Some(lhs_e), Some(rhs_e)) = (e_n.checked_mul(p_d), next_num.checked_mul(e_d))
            && lhs_e <= rhs_e
            && let Some(start_num) = turn_idx.checked_mul(p_n)
            && let Some((start, whole_cycles, begin, end)) = if start_num % p_d == 0 {
                let cycles = start_num / p_d;
                let start = Fraction::int(cycles);
                let (begin, end) = if let Some(back) = cycles.checked_neg() {
                    (
                        span.begin.checked_add_int(back),
                        span.end.checked_add_int(back),
                    )
                } else {
                    (span.begin.checked_sub(start), span.end.checked_sub(start))
                };
                begin.zip(end).map(|(b, e)| (start, Some(cycles), b, e))
            } else {
                Fraction::checked_new(start_num, p_d).and_then(|start| {
                    let b = span.begin.checked_sub(start)?;
                    let e = span.end.checked_sub(start)?;
                    Some((start, None, b, e))
                })
            }
        {
            return Some(Turn {
                index: turn_idx,
                start,
                whole_cycles,
                begin,
                end,
                keys: std::cell::Cell::new(None),
            });
        }
    }
    None
}

#[cold]
#[inline(never)]
fn tick_time_moved(grid: GridView<'_>, turn: &Turn, tick: u32) -> Result<Fraction, SequenceStop> {
    turn.moved(grid.made_small_with(i64::from(tick), None))
}

#[inline]
fn resolve_tick_time(
    grid: GridView<'_>,
    turn: &Turn,
    offset: Option<i64>,
    ticks_pow2: Option<u32>,
    tick: u32,
) -> Result<Fraction, SequenceStop> {
    if let Some(ticks) = offset.and_then(|o| o.checked_add(i64::from(tick))) {
        if let Some(shift) = ticks_pow2
            && let Some(dyadic) = Fraction::dyadic_i64(ticks, shift)
        {
            return Ok(dyadic);
        }
        return Ok(Fraction::from_ticks(i128::from(ticks), grid.ticks));
    }
    tick_time_moved(grid, turn, tick)
}

#[cold]
#[inline(never)]
fn resolve_cut_whole(
    grid: GridView<'_>,
    turn: &Turn,
    offset: Option<i64>,
    ticks_pow2: Option<u32>,
    cut: Cut,
) -> Result<TimeSpan, SequenceStop> {
    let wb = resolve_tick_time(grid, turn, offset, ticks_pow2, cut.begin)?;
    let we = resolve_tick_time(grid, turn, offset, ticks_pow2, cut.end)?;
    Ok(TimeSpan::new(wb, we))
}

#[cold]
#[inline(never)]
fn sub_start_cycles_fallback(
    span: TimeSpan,
    start_cycles: i128,
) -> Result<(Fraction, Fraction), SequenceStop> {
    let start_frac = Fraction::int(start_cycles);
    Ok((
        span.begin
            .checked_sub(start_frac)
            .ok_or(SequenceStop::Range)?,
        span.end
            .checked_sub(start_frac)
            .ok_or(SequenceStop::Range)?,
    ))
}

#[inline(never)]
fn query_layers_multi(
    thread: &QueryThread,
    owner: &Arc<[Layer]>,
    offset: usize,
    layers: &[Layer],
    span: TimeSpan,
    from: usize,
    out: &mut Vec<Hap>,
) -> Result<(), SequenceStop> {
    let period = layers[0].period();
    let mut turns = Turns::of(period, span)?;
    if turns.indices.end.checked_sub(turns.indices.start) == Some(1) {
        let turn = turns.turn(turns.indices.start)?;
        for (i, layer) in layers.iter().enumerate() {
            let layer_ref = u16::try_from(offset + i).ok().map(|idx| (owner, idx));
            layer.push(thread, layer_ref, turn, span, from, out)?;
        }
        return Ok(());
    }
    let int_period = (period.denom() == 1 && period.numer() > 0).then(|| period.numer());
    let whole_range = int_period.and_then(|period_cycles| {
        let start_idx = turns.indices.start;
        let end_idx = turns.indices.end;
        let first_start = Fraction::int(start_idx.checked_mul(period_cycles)?);
        let last_end = Fraction::int(end_idx.checked_mul(period_cycles)?);
        let whole_start = if span.begin <= first_start {
            start_idx
        } else {
            start_idx.checked_add(1)?
        };
        let whole_end = if last_end <= span.end {
            end_idx
        } else {
            end_idx.checked_sub(1)?
        };
        (whole_end.checked_sub(whole_start)? >= 2).then_some((
            period_cycles,
            whole_start,
            whole_end,
        ))
    });
    for (i, layer) in layers.iter().enumerate() {
        let layer_ref = u16::try_from(offset + i).ok().map(|idx| (owner, idx));
        if let Some((period_cycles, whole_start, whole_end)) = whole_range {
            if whole_start > turns.indices.start {
                layer.push(
                    thread,
                    layer_ref,
                    turns.turn(turns.indices.start)?,
                    span,
                    from,
                    out,
                )?;
            }
            if let Some(res) = layer.push_whole_int_turns(
                thread,
                layer_ref,
                period_cycles,
                turns.indices.start,
                whole_start,
                whole_end,
                from,
                out,
            ) {
                res?;
            } else {
                for index in whole_start..whole_end {
                    if index != turns.indices.start && thread.stop_requested() {
                        return Err(SequenceStop::Query);
                    }
                    layer.push(thread, layer_ref, turns.turn(index)?, span, from, out)?;
                }
            }
            if whole_end < turns.indices.end {
                let last_idx = turns.indices.end - 1;
                if last_idx != turns.indices.start && thread.stop_requested() {
                    return Err(SequenceStop::Query);
                }
                layer.push(thread, layer_ref, turns.turn(last_idx)?, span, from, out)?;
            }
        } else {
            for index in turns.indices.clone() {
                // The query of the node asked before the first turn.
                if index != turns.indices.start && thread.stop_requested() {
                    return Err(SequenceStop::Query);
                }
                layer.push(thread, layer_ref, turns.turn(index)?, span, from, out)?;
            }
        }
    }
    Ok(())
}

/// Push the haps of `layers`, which share a period, in `span`: each layer
/// in turn, as a stack of them answers.
fn query_layers(
    thread: &QueryThread,
    owner: &Arc<[Layer]>,
    offset: usize,
    layers: &[Layer],
    span: TimeSpan,
    from: usize,
    out: &mut Vec<Hap>,
) -> Result<(), SequenceStop> {
    let Some(first) = layers.first() else {
        return Ok(());
    };
    let period = first.period();
    // Direct single-turn fast paths for whole-cycle and positive rational
    // periods without building `Turns` on the stack.
    let single_turn = if span.begin < span.end {
        if period.denom() == 1 && period.numer() > 0 {
            let p = period.numer();
            let first_cycle = span.begin.floor_int();
            let turn_idx = if p == 1 {
                first_cycle
            } else if p & (p - 1) == 0 {
                first_cycle >> p.trailing_zeros()
            } else {
                first_cycle.div_euclid(p)
            };
            if let Some(next_turn) = turn_idx.checked_add(1)
                && let Some(next_start) = next_turn.checked_mul(p)
                && (if span.end.denom() == 1 {
                    span.end.numer() <= next_start
                } else {
                    span.end.floor_int() < next_start
                })
            {
                let start_cycles = if p == 1 {
                    turn_idx
                } else {
                    turn_idx.checked_mul(p).ok_or(SequenceStop::Range)?
                };
                let (begin, end) = if p == 1 {
                    let b = span.begin.cycle_pos();
                    let e = if span.end.denom() == 1 {
                        Fraction::ONE
                    } else {
                        span.end.cycle_pos()
                    };
                    (b, e)
                } else if let Some(back) = start_cycles.checked_neg() {
                    (
                        span.begin
                            .checked_add_int(back)
                            .ok_or(SequenceStop::Range)?,
                        span.end.checked_add_int(back).ok_or(SequenceStop::Range)?,
                    )
                } else {
                    sub_start_cycles_fallback(span, start_cycles)?
                };
                Some(Turn {
                    index: turn_idx,
                    start: Fraction::int(start_cycles),
                    whole_cycles: Some(start_cycles),
                    begin,
                    end,
                    keys: std::cell::Cell::new(None),
                })
            } else {
                None
            }
        } else {
            single_turn_fractional(period, span)
        }
    } else {
        None
    };
    let Some(turn) = single_turn else {
        return query_layers_multi(thread, owner, offset, layers, span, from, out);
    };
    for (i, layer) in layers.iter().enumerate() {
        let layer_ref = u16::try_from(offset + i).ok().map(|idx| (owner, idx));
        if layer.source().is_none()
            && let Some(done) = layer
                .listing()
                .with_grid_view(|grid| {
                    let GridStepsRef::Small(steps) = grid.steps else {
                        return None;
                    };
                    let (begin_low, begin_high) = grid.ticks_at(turn.begin)?;
                    let (end_low, end_high) = grid.ticks_at(turn.end)?;
                    let begin_exact = begin_low == begin_high;
                    let end_exact = end_low == end_high;
                    // `single_turn` guarantees `span.begin < span.end` (never instant).
                    let (first, last_step) = if steps.len() == 1 {
                        let s = steps[0];
                        if i64::from(s.part_end) <= begin_low || i64::from(s.part_begin) >= end_high
                        {
                            return Some(Ok(()));
                        }
                        (0, 1)
                    } else {
                        let (Some(first_step), Some(last_step_ref)) = (steps.first(), steps.last())
                        else {
                            return Some(Ok(()));
                        };
                        if i64::from(last_step_ref.part_end) <= begin_low
                            || i64::from(first_step.part_begin) >= end_high
                        {
                            return Some(Ok(()));
                        }
                        let begun =
                            steps.partition_point(|step| i64::from(step.part_begin) < begin_high);
                        let first = begun
                            - steps[..begun]
                                .iter()
                                .rev()
                                .take_while(|step| i64::from(step.part_end) > begin_low)
                                .count();
                        let last_step = begun
                            + steps[begun..]
                                .iter()
                                .take_while(|step| i64::from(step.part_begin) < end_high)
                                .count();
                        (first, last_step)
                    };
                    let met = last_step - first;
                    if met == 0 {
                        return Some(Ok(()));
                    }
                    if !thread.charge(out.len() - from + met) {
                        return Some(Err(SequenceStop::Query));
                    }
                    if met > 1 {
                        out.reserve(met);
                    }
                    let mut cuts = if let Some(rare) = grid.rare {
                        rare.cuts().starting_at(first)
                    } else {
                        CutsSlice::EMPTY
                    };
                    let has_cuts = cuts.len() != 0;
                    let offset = turn
                        .whole_cycles
                        .and_then(|cycles| i64::try_from(cycles).ok())
                        .and_then(|cycles| cycles.checked_mul(i64::from(grid.ticks.get())));
                    let ticks_pow2 = grid
                        .ticks
                        .is_power_of_two()
                        .then(|| grid.ticks.trailing_zeros());
                    let maker = layer.maker(layer_ref);
                    let mut last_tick = u16::MAX;
                    let mut last_time = Fraction::ZERO;
                    for (index, &s) in steps[first..last_step].iter().enumerate() {
                        let step_idx = first + index;
                        let vis_begin = if begin_exact && i64::from(s.part_begin) == begin_low {
                            span.begin
                        } else if s.part_begin == last_tick {
                            last_time
                        } else {
                            match resolve_tick_time(
                                grid,
                                &turn,
                                offset,
                                ticks_pow2,
                                u32::from(s.part_begin),
                            ) {
                                Ok(t) => t,
                                Err(e) => return Some(Err(e)),
                            }
                        };
                        let vis_end = if end_exact && i64::from(s.part_end) == end_high {
                            span.end
                        } else {
                            match resolve_tick_time(
                                grid,
                                &turn,
                                offset,
                                ticks_pow2,
                                u32::from(s.part_end),
                            ) {
                                Ok(t) => t,
                                Err(e) => return Some(Err(e)),
                            }
                        };
                        last_tick = s.part_end;
                        last_time = vis_end;
                        let whole = if has_cuts && let Some(cut) = cuts.split_first_at(step_idx) {
                            match resolve_cut_whole(grid, &turn, offset, ticks_pow2, cut) {
                                Ok(w) => w,
                                Err(e) => return Some(Err(e)),
                            }
                        } else {
                            TimeSpan::new(vis_begin, vis_end)
                        };
                        let part = TimeSpan::new(
                            if i64::from(s.part_begin) <= begin_low {
                                span.begin
                            } else {
                                vis_begin
                            },
                            if i64::from(s.part_end) >= end_high {
                                span.end
                            } else {
                                vis_end
                            },
                        );
                        out.push(maker.make_grid_small(
                            grid,
                            step_idx,
                            u32::from(s.value),
                            s.loc,
                            whole,
                            part,
                        ));
                    }
                    thread.note_accumulated(out.len() - from);
                    Some(Ok(()))
                })
                .flatten()
        {
            done?;
            continue;
        }
        layer.push(thread, layer_ref, &turn, span, from, out)?;
    }
    Ok(())
}

/// The haps of `layers` in `state`'s span, after those of `out`. See
/// [`sequence`].
pub(crate) fn query_sequence_onto(
    thread: &QueryThread,
    layers: &Arc<[Layer]>,
    state: &State,
    out: &mut Vec<Hap>,
) {
    /// Cap initial pre-reservation so huge refused queries do not over-allocate.
    const MOST_ROOM: usize = 4096;
    // A span of a cycle or more meets each step of most layers. A shorter
    // one meets few, and takes room as its haps come.
    let span = state.span;
    // A reversed span, which a negative speed asks, meets no step.
    if span.begin > span.end {
        return;
    }
    let long = span
        .begin
        .checked_add_int(1)
        .is_some_and(|after_cycle| span.end >= after_cycle);
    if long {
        let cycles: usize = span
            .end
            .checked_sub(span.begin)
            .and_then(|len| usize::try_from(len.ceil().numer()).ok())
            .unwrap_or(1)
            .clamp(1, 256);
        let est: usize = layers
            .iter()
            .map(|layer| {
                let p = layer.period();
                let turns = if p.denom() == 1 && p.numer() > 1 {
                    let pn = usize::try_from(p.numer()).unwrap_or(1);
                    cycles.div_ceil(pn)
                } else {
                    cycles
                };
                layer.count().saturating_mul(turns)
            })
            .fold(0usize, usize::saturating_add);
        out.reserve(est.min(MOST_ROOM));
    }
    let from = out.len();
    if layers.len() == 1 {
        if let Err(stop) = query_layers(thread, layers, 0, layers, span, from, out) {
            if matches!(stop, SequenceStop::Range) {
                refuse(QueryLimit::NativeFraction {
                    operation: "sequence",
                });
            }
            out.truncate(from);
            return;
        }
    } else {
        let mut offset = 0usize;
        for chunk in layers.chunk_by(Layer::same_period) {
            match query_layers(thread, layers, offset, chunk, span, from, out) {
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
            offset += chunk.len();
        }
    }
    let added = out.len() - from;
    if added > 0 {
        if thread.deadline_after(u32::try_from(added).unwrap_or(u32::MAX)) {
            out.truncate(from);
            return;
        }
        thread.note_materialised(added);
    }
}

mod met;
use met::{PackedRunsBuilder, lie_in_spans};

mod sorted;
pub(crate) use sorted::{
    push_whole_cycle_scaled_pow2, query_sequence_sorted_onto, single_cycle_step_count,
    try_query_fast_pow2_slowcat_static_int,
};

mod folds;
#[allow(unused_imports)]
pub(crate) use folds::*;

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
                    hap.live_controls(),
                    hap.slider_binding(),
                    hap.ui_visuals_context(),
                    hap.keys,
                    hap.pick_lookup().is_some()
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

    fn built(layers: &[SequenceLayer]) -> Pattern {
        sequence(layers.to_vec()).expect("sound layers")
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
            let Node::Constants(set_node) = set.node_kind() else {
                panic!("{name} is no run of constants");
            };
            assert_eq!(set_node.1.len(), 1, "{name}");
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
                            ChainArgument::Constant(value, locations) => {
                                if locations.is_empty() {
                                    spec.apply_value(pattern, value)
                                } else {
                                    let values = crate::pure(value)
                                        .with_added_context(locations.to_vec())
                                        .with_steps(Some(Fraction::ONE));
                                    spec.apply_to(pattern, Some(values))
                                }
                            }
                            ChainArgument::ConstantPair(a, b, locations) => {
                                let value = Value::List(Arc::from([a, b]));
                                if locations.is_empty() {
                                    spec.apply_value(pattern, value)
                                } else {
                                    let values = crate::pure(value)
                                        .with_added_context(locations.to_vec())
                                        .with_steps(Some(Fraction::ONE));
                                    spec.apply_to(pattern, Some(values))
                                }
                            }
                            ChainArgument::Wave(wave, steps, math_inside, op_flags) => spec
                                .apply_to(
                                    pattern,
                                    Some(wave.to_pattern(math_inside, steps, op_flags)),
                                ),
                            ChainArgument::Pattern(values) => spec.apply_to(pattern, Some(values)),
                        })
                    };
                    assert_same_answers(&name, &set, &unfolded(in_turn));
                }
            }
        }
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
                assert_same_answers(&name, &chain(), &unfolded(chain));
            }
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
            let folded = matches!(chain().node_kind(), Node::Pick(..));
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
        assert!(matches!(
            kept.node_kind(),
            Node::Stack(_) | Node::StackPure(_)
        ));
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
}
