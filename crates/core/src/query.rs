//! Pattern query execution, wave chains, and value-side reading.

use super::*;

/// How the value side of an `appLeft` is read with no query of its graph for
/// each function hap, found once by the first query that meets the side.
#[derive(Default)]
pub(crate) struct ValueReading {
    found: std::sync::OnceLock<Option<Box<Reading>>>,
}

/// What a value side was found to be, and the settings state it was found
/// under. It answers only while that state is in force, as a table does.
struct Reading {
    settings: settings::SettingsStateId,
    values: ReadValues,
}

enum ReadValues {
    /// The side answers every cycle with one hap that fills the cycle and
    /// that only its span tells from the next cycle's, as a map of a `pure`
    /// gives. Asked for any span, the side answers one hap per cycle piece
    /// of the span, as [`Node::Pure`] does: this hap of cycle zero, moved
    /// to the piece. In a box, so that a signal's reading is not as large.
    Constant(Box<Hap>),
    /// The side is a signal, as `.lpf(sine.range(200, 2000))` gives, which
    /// answers as a table samples it. Stored inline inside `Box<Reading>` to
    /// avoid a second heap allocation and indirection.
    Signal(table::ValueSignal),
}

impl ValueReading {
    /// What `values` was found to be, when the settings state it was found
    /// under is in force.
    ///
    /// A constant is found by a query of cycle zero. A query that already
    /// failed or stopped does not look, and a look that fails or stops is
    /// not kept. Every query so signals what it did before.
    fn of(&self, values: &Pattern, state: &State) -> Option<&ReadValues> {
        let current = |reading: &Reading| settings::is_current_state(&reading.settings);
        if let Some(found) = self.found.get() {
            return found
                .as_deref()
                .filter(|reading| current(reading))
                .map(|reading| &reading.values);
        }
        let unsettled = || query_error_pending() || query_stop_requested();
        if unsettled() {
            return None;
        }
        let settings = settings::current_state_id();
        let found = match values.constant_cycle(state) {
            Some(hap) => Some(ReadValues::Constant(Box::new(hap))),
            None => table::ValueSignal::of(values).map(ReadValues::Signal),
        };
        if unsettled() {
            return None;
        }
        self.found
            .get_or_init(|| found.map(|values| Box::new(Reading { settings, values })))
            .as_deref()
            .filter(|reading| current(reading))
            .map(|reading| &reading.values)
    }

    /// Forget what a query read of the function side, which is another
    /// pattern now. The value side is what it was.
    pub(crate) fn forget_functions(&mut self) {
        self.found = std::sync::OnceLock::new();
    }
}

/// Pack a single `(begin, end)` range into a non-zero `u32` for [`RunApply`]:
/// `begin` in the high 20 bits and `length + 1` in the low 12 bits, leaving
/// `0` as the empty sentinel.
#[inline]
pub(crate) fn pack_run_loc((begin, end): (usize, usize)) -> Option<u32> {
    const LEN_BITS: u32 = 12;
    const MAX_LEN: u32 = (1 << LEN_BITS) - 2;
    const MAX_BEGIN: u32 = (1 << (32 - LEN_BITS)) - 1;
    let length = u32::try_from(end.checked_sub(begin)?).ok()?;
    let begin = u32::try_from(begin).ok()?;
    (begin <= MAX_BEGIN && length <= MAX_LEN).then_some((begin << LEN_BITS) | (length + 1))
}

#[inline]
pub(crate) fn unpack_run_loc(packed: u32) -> (usize, usize) {
    const LEN_BITS: u32 = 12;
    const LEN_MASK: u32 = (1 << LEN_BITS) - 1;
    let begin = (packed >> LEN_BITS) as usize;
    let length = ((packed & LEN_MASK) - 1) as usize;
    (begin, begin + length)
}

#[derive(Clone)]
pub(crate) struct RunSpilled {
    combine: Combine,
    flow: LookupFlow,
    single_key: u16,
    loc0: u32,
    more_context: Option<Locations>,
}

/// Compact 16-byte header for [`RunMeeting`] packing [`Combine`],
/// [`LookupFlow`], an inline single built-in control key, and up to one inline
/// packed source location (`loc0 != 0`). Rare `Combine::With` closures and
/// multi-range locations spill into [`RunSpilled`] so [`RunMeeting`] stays 40
/// bytes.
#[derive(Clone)]
pub(crate) enum RunApply {
    Composed {
        op: compose::ComposeOp,
        flow: LookupFlow,
        merged: bool,
        /// Non-zero packed built-in key when `RunMeeting::value` holds the
        /// single control value inline without allocating a `BuiltinMap<1>`.
        single_key: u16,
        loc0: u32,
    },
    Spilled(Box<RunSpilled>),
}

const _: () = assert!(std::mem::size_of::<RunApply>() <= 16);

impl RunApply {
    fn from_parts(
        apply: Combine,
        flow: LookupFlow,
        single_key: u16,
        loc0: u32,
        more_context: Option<Locations>,
    ) -> Self {
        if more_context.is_none() {
            match apply {
                Combine::Composed(op) => {
                    return Self::Composed {
                        op,
                        flow,
                        merged: false,
                        single_key,
                        loc0,
                    };
                }
                Combine::SetMerged => {
                    return Self::Composed {
                        op: compose::ComposeOp::Set,
                        flow,
                        merged: true,
                        single_key,
                        loc0,
                    };
                }
                Combine::With(_) => {}
            }
        }
        Self::Spilled(Box::new(RunSpilled {
            combine: apply,
            flow,
            single_key,
            loc0,
            more_context,
        }))
    }

    #[inline]
    pub(crate) fn single_key(&self) -> u16 {
        match self {
            Self::Composed { single_key, .. } => *single_key,
            Self::Spilled(s) => s.single_key,
        }
    }

    #[inline]
    fn flow(&self) -> LookupFlow {
        match self {
            Self::Composed { flow, .. } => *flow,
            Self::Spilled(s) => s.flow,
        }
    }

    #[inline]
    fn loc0(&self) -> u32 {
        match self {
            Self::Composed { loc0, .. } => *loc0,
            Self::Spilled(s) => s.loc0,
        }
    }

    #[inline]
    fn more_context(&self) -> Option<&Locations> {
        match self {
            Self::Composed { .. } => None,
            Self::Spilled(s) => s.more_context.as_ref(),
        }
    }

    #[inline]
    pub(crate) fn in_place_op(&self) -> Option<compose::ComposeOp> {
        match self {
            Self::Composed { op, .. } => Some(*op),
            Self::Spilled(s) => match &s.combine {
                Combine::Composed(op) => Some(*op),
                Combine::SetMerged => Some(compose::ComposeOp::Set),
                Combine::With(_) => None,
            },
        }
    }

    fn combine(&self) -> Combine {
        match self {
            Self::Composed { merged: true, .. } => Combine::SetMerged,
            Self::Composed { op, .. } => Combine::Composed(*op),
            Self::Spilled(s) => s.combine.clone(),
        }
    }

    fn same(&self, other: &Self) -> bool {
        if self.flow() != other.flow() || self.single_key() != other.single_key() {
            return false;
        }
        match (self, other) {
            (
                Self::Composed {
                    op: a, merged: ma, ..
                },
                Self::Composed {
                    op: b, merged: mb, ..
                },
            ) => a == b && ma == mb,
            _ => match (self.combine(), other.combine()) {
                (Combine::Composed(a), Combine::Composed(b)) => a == b,
                (Combine::SetMerged, Combine::SetMerged) => true,
                (Combine::With(_), Combine::With(_)) => true,
                _ => false,
            },
        }
    }
}

/// One meeting of a run: the constant of one application, carrying only its
/// value and source locations (cycle spans are synthesized on demand). Sets of
/// maps of controls in a row are one meeting, which the build made of them.
/// See [`Pattern::set_control`].
#[derive(Clone)]
pub(crate) struct RunMeeting {
    pub(crate) value: Value,
    pub(crate) apply: RunApply,
}

const _: () = assert!(std::mem::size_of::<RunMeeting>() <= 40);

impl RunMeeting {
    #[inline]
    pub(crate) fn flow(&self) -> LookupFlow {
        self.apply.flow()
    }

    #[inline]
    pub(crate) fn combine(&self) -> Combine {
        self.apply.combine()
    }

    #[inline]
    pub(crate) fn in_place_op(&self) -> Option<compose::ComposeOp> {
        self.apply.in_place_op()
    }

    #[inline]
    pub(crate) fn values_meet_in_place(
        &self,
        function: &Value,
        value: &Value,
        flow: LookupFlow,
    ) -> bool {
        let Some(op) = self.apply.in_place_op() else {
            return false;
        };
        let compose = Combine::Composed(op);
        Meeting::values_meet_in_place(function, value, &compose, flow)
    }

    #[inline]
    fn context_is_empty(&self) -> bool {
        self.apply.loc0() == 0 && self.apply.more_context().is_none()
    }

    fn context(&self) -> Locations {
        if let Some(more) = self.apply.more_context() {
            more.clone()
        } else if self.apply.loc0() != 0 {
            let mut out = Locations::new();
            out.push(unpack_run_loc(self.apply.loc0()));
            out
        } else {
            Locations::new()
        }
    }

    #[inline]
    pub(crate) fn prepend_context_to(&self, target: &mut Locations) {
        if let Some(more) = self.apply.more_context() {
            target.prepend(more);
        } else if self.apply.loc0() != 0 {
            let loc0 = self.apply.loc0();
            if !target.prepend_packed(loc0, 0) {
                let mut merged = Locations::new();
                merged.push(unpack_run_loc(loc0));
                merged.append(target);
                *target = merged;
            }
        }
    }

    fn same_context(&self, other: &Self) -> bool {
        match (self.apply.more_context(), other.apply.more_context()) {
            (None, None) => self.apply.loc0() == other.apply.loc0(),
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }

    fn pack_context(context: Locations) -> (u32, Option<Locations>) {
        if context.is_empty() {
            (0, None)
        } else if context.len() == 1
            && let Some(range) = context.get(0)
            && let Some(packed) = pack_run_loc(range)
        {
            (packed, None)
        } else {
            (0, Some(context))
        }
    }

    /// Materialize this constant as a cycle-zero [`Hap`] for fallback paths.
    pub(crate) fn as_hap(&self) -> Hap {
        let cycle = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
        let value = match self.apply.single_key() {
            0 => self.value.clone(),
            packed => Value::Object(OrderedMap::from_single_packed(packed, self.value.clone())),
        };
        let mut hap = Hap::new(Some(cycle), cycle, value);
        hap.context = self.context();
        hap
    }

    /// Meet the function hap `hf` with the constant as its application
    /// does for any hap: the constant is one hap in each cycle that the
    /// whole of `hf` covers, or its part. What the meetings make comes
    /// after the haps of `out`. `false` when the query stops.
    fn meet(
        &self,
        thread: &QueryThread,
        hf: Hap,
        mut cache: Option<&mut value::SharedWaveCache>,
        out: &mut Vec<Hap>,
    ) -> bool {
        let met = hf.whole_or_part();
        let Some(overlap) = (match hf.whole {
            None => (hf.part.begin <= hf.part.end).then_some(hf.part),
            Some(whole) if whole == hf.part => (hf.part.begin <= hf.part.end).then_some(hf.part),
            Some(whole) => hf.part.intersection(&whole),
        }) else {
            if met.begin != met.end
                && !met
                    .cycle_count()
                    .is_some_and(|c| c <= MAX_QUERY_SPAN_CYCLES)
            {
                refuse(QueryLimit::QuerySpan {
                    cycles: MAX_QUERY_SPAN_CYCLES,
                });
            }
            return true;
        };
        if overlap.begin < overlap.end {
            if !met
                .cycle_count()
                .is_some_and(|c| c <= MAX_QUERY_SPAN_CYCLES)
            {
                refuse(QueryLimit::QuerySpan {
                    cycles: MAX_QUERY_SPAN_CYCLES,
                });
                return true;
            }
            if overlap.within_one_cycle() {
                let mut hap = hf;
                if !Meeting::in_place_run_meeting(&mut hap, self, overlap, cache) {
                    let constant = self.as_hap();
                    let apply = self.combine();
                    hap = applied_hap(&hap, &constant, hap.whole, overlap, &apply, self.flow());
                }
                return !thread.stop_requested() && thread.push_budgeted(out, 0, hap);
            }
            let mut fallback: Option<(Hap, Combine)> = None;
            let mut stopped = false;
            let mut src = Some(hf);
            overlap.for_each_cycle(|part| {
                if stopped {
                    return;
                }
                let mut hap = if part.end == overlap.end {
                    src.take().expect("last piece moves hap")
                } else {
                    src.as_ref().expect("hap present").clone()
                };
                if !Meeting::in_place_run_meeting(&mut hap, self, part, cache.as_deref_mut()) {
                    let (constant, apply) =
                        fallback.get_or_insert_with(|| (self.as_hap(), self.combine()));
                    hap = applied_hap(&hap, constant, hap.whole, part, apply, self.flow());
                }
                if thread.stop_requested() || !thread.push_budgeted(out, 0, hap) {
                    stopped = true;
                }
            });
            return !stopped;
        }
        let mut fallback: Option<(Hap, Combine)> = None;
        let mut stopped = false;
        let mut single: Option<TimeSpan> = None;
        let mut count = 0usize;
        met.for_each_cycle(|piece| {
            if let Some(part) = hf.part.intersection(&piece) {
                single = Some(part);
                count += 1;
            }
        });
        if count == 0 {
            return true;
        }
        if count == 1 {
            let part = single.expect("count == 1");
            let mut hap = hf;
            if !Meeting::in_place_run_meeting(&mut hap, self, part, cache) {
                let constant = self.as_hap();
                let apply = self.combine();
                hap = applied_hap(&hap, &constant, hap.whole, part, &apply, self.flow());
            }
            return !thread.stop_requested() && thread.push_budgeted(out, 0, hap);
        }
        met.for_each_cycle(|piece| {
            if stopped {
                return;
            }
            let Some(part) = hf.part.intersection(&piece) else {
                return;
            };
            let mut hap = hf.clone();
            if !Meeting::in_place_run_meeting(&mut hap, self, part, cache.as_deref_mut()) {
                let (constant, apply) =
                    fallback.get_or_insert_with(|| (self.as_hap(), self.combine()));
                hap = applied_hap(&hap, constant, hap.whole, part, apply, self.flow());
            }
            if thread.stop_requested() || !thread.push_budgeted(out, 0, hap) {
                stopped = true;
            }
        });
        !stopped
    }

    /// The meeting of the constant `value` at `context` by `apply`, as the
    /// `appLeft` of a `pure` of the value at those places makes it: the
    /// value in every cycle, on a hap that carries nothing more.
    pub(crate) fn of(
        apply: Combine,
        flow: LookupFlow,
        mut value: Value,
        context: Locations,
    ) -> Self {
        let mut single_key = 0u16;
        if let Value::Object(map) = &mut value {
            map.undress_patched();
            if matches!(apply, Combine::Composed(compose::ComposeOp::Set))
                && flow == LookupFlow::Set
                && let Some((packed, inner_val)) = map.single_packed_entry()
            {
                single_key = packed;
                value = inner_val.clone();
            }
        }
        let (loc0, more_context) = Self::pack_context(context);
        Self {
            value,
            apply: RunApply::from_parts(apply, flow, single_key, loc0, more_context),
        }
    }

    /// The op and the number of this meeting, when it is one of a number
    /// by a known op, on a hap that carries nothing more: no place in the
    /// text.
    pub(crate) fn number(&self) -> Option<(compose::ComposeOp, f64)> {
        match (&self.apply, &self.value) {
            (
                RunApply::Composed {
                    op, single_key: 0, ..
                },
                Value::F64(other),
            ) if self.context_is_empty() => Some((*op, *other)),
            _ => None,
        }
    }

    /// Whether the runs `left` and `right` of [`Node::Constants`] meet the
    /// same constants in the same way and in the same order: each value
    /// down to the bits of its numbers, at the same places in the text.
    pub(crate) fn same(left: &[Self], right: &[Self]) -> bool {
        left.len() == right.len()
            && left.iter().zip(right).all(|(a, b)| {
                a.apply.same(&b.apply) && a.value.same_data(&b.value) && a.same_context(b)
            })
    }

    /// Whether this meeting is a `set` of a constant map of controls, which
    /// the `set` of another map after it may join. A union with a map of
    /// `value` alone keeps the other side as it is, so such a constant
    /// takes none.
    pub(crate) fn takes_sets(&self) -> bool {
        let sets = self.apply.in_place_op() == Some(compose::ComposeOp::Set);
        if self.apply.single_key() != 0 {
            return self.flow() == LookupFlow::Set && sets;
        }
        let controls = self.value.as_object();
        self.flow() == LookupFlow::Set
            && sets
            && controls.is_some_and(|controls| !compose::union_drops(controls))
    }

    /// Whether this meeting takes the `set` of the map of controls
    /// `control` after it, as one constant of the union of the two: each
    /// hap then meets the union as it meets the two in turn.
    pub(crate) fn takes_set_after(&self, control: &Value) -> bool {
        let set = Combine::Composed(compose::ComposeOp::Set);
        let map = Value::Object(OrderedMap::new());
        self.takes_sets()
            && matches!(control, Value::Object(_))
            && control.is_data(0)
            && Meeting::values_meet_in_place(&map, control, &set, LookupFlow::Set)
    }

    /// Join the `set` of `control` at `context`, which this meeting
    /// [takes](Self::takes_set_after): the constant is then the union, with
    /// the locations of the later control first.
    pub(crate) fn of_single_control(packed_key: u16, value: Value) -> Self {
        Self::of_single_control_at(packed_key, value, Locations::new())
    }

    pub(crate) fn of_single_control_at(packed_key: u16, value: Value, context: Locations) -> Self {
        let (loc0, more_context) = Self::pack_context(context);
        Self {
            value,
            apply: RunApply::from_parts(
                Combine::Composed(compose::ComposeOp::Set),
                LookupFlow::Set,
                packed_key,
                loc0,
                more_context,
            ),
        }
    }

    pub(crate) fn set_single_after(&mut self, packed_key: u16, key: &Key, value: Value) {
        let prev_key = self.apply.single_key();
        if prev_key != 0 {
            if prev_key == packed_key {
                self.value = value;
                return;
            }
            let prev_val = std::mem::replace(&mut self.value, Value::Undefined);
            self.value = Value::Object(if packed_key != 0 {
                OrderedMap::from_two_packed(prev_key, prev_val, packed_key, value)
            } else {
                let mut map = OrderedMap::from_single_packed(prev_key, prev_val);
                map.set_data(key, value);
                map.mark_made_of_data();
                map
            });
            let flow = self.apply.flow();
            let (loc0, more) = Self::pack_context(self.context());
            self.apply = RunApply::from_parts(Combine::SetMerged, flow, 0, loc0, more);
            return;
        }
        if let Value::Object(map) = &mut self.value {
            map.set_data(key, value);
            map.mark_made_of_data();
        } else {
            let mut single = OrderedMap::of_one(key.clone(), value);
            single.mark_made_of_data();
            compose::compose_data_flat(
                compose::ComposeOp::Set,
                &mut self.value,
                &Value::Object(single),
            );
        }
        let flow = self.apply.flow();
        let (loc0, more) = Self::pack_context(self.context());
        self.apply = RunApply::from_parts(Combine::SetMerged, flow, 0, loc0, more);
    }

    pub(crate) fn set_packed_after(
        &mut self,
        keys: &[u16],
        vals: &[Value],
        mut context: Locations,
        shared: &mut Option<Arc<dyn value::BuiltinEntries>>,
    ) {
        let prev_key = self.apply.single_key();
        let flow = self.apply.flow();
        if prev_key != 0 {
            if keys.len() == 1 && keys[0] == prev_key {
                self.value = vals[0].clone();
                if !context.is_empty() {
                    if !self.context_is_empty() {
                        context.append(&self.context());
                    }
                    let (new_loc0, new_more) = Self::pack_context(context);
                    self.apply = RunApply::from_parts(
                        Combine::Composed(compose::ComposeOp::Set),
                        flow,
                        prev_key,
                        new_loc0,
                        new_more,
                    );
                }
                return;
            }
            let prev_val = std::mem::replace(&mut self.value, Value::Undefined);
            match OrderedMap::from_single_and_packed(prev_key, prev_val, keys, vals) {
                Ok(map) => {
                    self.value = Value::Object(map);
                }
                Err(prev_val) => {
                    self.value = Value::Object(OrderedMap::from_single_packed(prev_key, prev_val));
                    let other_arc =
                        shared.get_or_insert_with(|| value::make_builtin_arc_cloned(keys, vals));
                    let other = Value::Object(OrderedMap::from_made_builtin(Arc::clone(other_arc)));
                    compose::compose_data_flat(compose::ComposeOp::Set, &mut self.value, &other);
                }
            }
        } else if let Value::Object(map) = &mut self.value
            && map.merge_packed_builtin(keys, vals)
        {
        } else {
            let other_arc =
                shared.get_or_insert_with(|| value::make_builtin_arc_cloned(keys, vals));
            let other = Value::Object(OrderedMap::from_made_builtin(Arc::clone(other_arc)));
            compose::compose_data_flat(compose::ComposeOp::Set, &mut self.value, &other);
        }
        if !self.context_is_empty() {
            context.append(&self.context());
        }
        let (new_loc0, new_more) = Self::pack_context(context);
        self.apply = RunApply::from_parts(Combine::SetMerged, flow, 0, new_loc0, new_more);
    }

    pub(crate) fn set_after(&mut self, control: &Value, mut context: Locations) {
        let single_key = self.apply.single_key();
        if single_key != 0 {
            let val = std::mem::replace(&mut self.value, Value::Undefined);
            self.value = Value::Object(OrderedMap::from_single_packed(single_key, val));
        }
        compose::compose_data_flat(compose::ComposeOp::Set, &mut self.value, control);
        if !self.context_is_empty() {
            context.append(&self.context());
        }
        let flow = self.apply.flow();
        let (new_loc0, new_more) = Self::pack_context(context);
        self.apply = RunApply::from_parts(Combine::SetMerged, flow, 0, new_loc0, new_more);
    }
}

/// How one query reads a value side. It is settled at the first function
/// hap, so a query with none reads nothing.
enum Answering<'a> {
    /// The side's constant hap, to move from piece to piece.
    Constant(Hap),
    /// The side's signal, and a copy of the query's state for it to read.
    Signal(&'a table::ValueSignal, State),
    /// A query of the side's graph for each function hap.
    Graph,
}

/// What one query keeps from function hap to function hap while a
/// [`ValueSide`] meets them.
/// Cached per-span step-wave values, cut pieces, and shared patched table so
/// chord haps sharing `hf.part` and a `Heads::Keyed` base table do zero extra
/// allocations or grid time divisions.
struct StepWaveCache {
    part: TimeSpan,
    values: Arc<[Value]>,
    pieces: Vec<TimeSpan>,
    /// `(base, patched_table)` for the last keyed base table or sequence layer.
    table: Option<(value::PatchedBase, Arc<value::Table>)>,
}

#[derive(Default)]
struct StepWaveCacheSet {
    slots: [Option<StepWaveCache>; 4],
    next: u8,
}

impl StepWaveCacheSet {
    #[inline]
    fn find(&mut self, part: TimeSpan) -> Option<&mut StepWaveCache> {
        self.slots
            .iter_mut()
            .flatten()
            .find(|cached| cached.part == part)
    }

    fn insert(&mut self, cache: StepWaveCache) -> &mut StepWaveCache {
        let idx = match self.slots.iter().position(Option::is_none) {
            Some(empty) => empty,
            None => {
                let idx = usize::from(self.next) & 3;
                self.next = self.next.wrapping_add(1);
                idx
            }
        };
        self.slots[idx].insert(cache)
    }

    fn take_victim_pieces(&mut self, count: usize) -> Vec<TimeSpan> {
        if self.slots.iter().all(Option::is_some) {
            let idx = usize::from(self.next) & 3;
            if let Some(mut prev) = self.slots[idx].take() {
                prev.pieces.clear();
                prev.pieces.reserve(count);
                return prev.pieces;
            }
        }
        Vec::with_capacity(count)
    }
}

#[derive(Default)]
pub(crate) struct Meetings<'a> {
    answering: Option<Answering<'a>>,
    /// The haps the value side answers for one function hap.
    values: Vec<Hap>,
    /// The side as the control of a segment of a wave, when it is one.
    steps: Option<&'a fused::StepWave>,
    /// Cached step-wave evaluations and shared overlay tables for up to 4
    /// distinct `hf.part` spans, preventing thrashing across interleaved layers.
    step_cache: StepWaveCacheSet,
    /// Per-(met, part) overlay caches for chord notes meeting a wave signal.
    wave_cache: [Option<(TimeSpan, TimeSpan, value::SharedWaveCache)>; 2],
    wave_cache_next: u8,
}

impl<'a> Meetings<'a> {
    fn wave_overlay_cache(&mut self, met: TimeSpan, part: TimeSpan) -> &mut value::SharedWaveCache {
        if let Some(idx) = self
            .wave_cache
            .iter()
            .position(|slot| matches!(slot, Some((m, p, _)) if *m == met && *p == part))
        {
            return &mut self.wave_cache[idx].as_mut().expect("found slot").2;
        }
        let idx = if let Some(empty) = self.wave_cache.iter().position(Option::is_none) {
            empty
        } else {
            let victim = usize::from(self.wave_cache_next & 1);
            self.wave_cache_next = self.wave_cache_next.wrapping_add(1);
            victim
        };
        self.wave_cache[idx] = Some((met, part, value::SharedWaveCache::default()));
        &mut self.wave_cache[idx].as_mut().expect("inserted slot").2
    }
    /// The gate of `side`, when it is the gate of a degrade that this
    /// query reads: the meeting keeps the function hap as it is. See
    /// [`table::Gate`].
    fn gate(&mut self, side: &ValueSide<'a>, state: &State) -> Option<&'a table::Gate> {
        let keeps = side.flow == LookupFlow::Left
            && matches!(side.apply, Combine::With(combine) if combinators::keeps_left(combine));
        match self.answering.get_or_insert_with(|| side.answering(state)) {
            Answering::Signal(signal, _) if keeps => {
                let signal: &'a table::ValueSignal = signal;
                signal.gate()
            }
            _ => None,
        }
    }

    /// The name and the wave of `side`, when it is a control of a signal
    /// of numbers that this query reads. See [`table::Wave`].
    fn wave(&mut self, side: &ValueSide<'a>, state: &State) -> Option<(&'a Key, &'a table::Wave)> {
        match self.answering.get_or_insert_with(|| side.answering(state)) {
            Answering::Signal(signal, _) => {
                let signal: &'a table::ValueSignal = signal;
                signal.wave()
            }
            Answering::Constant(_) | Answering::Graph => None,
        }
    }

    /// The haps `side` answers over `met` for a function hap with `part`,
    /// and the control that names their values, when they are the values
    /// of a signal under a control.
    fn values(
        &mut self,
        thread: &QueryThread,
        side: &ValueSide<'a>,
        met: TimeSpan,
        part: TimeSpan,
        state: &State,
    ) -> (&[Hap], Option<&'a table::Named>) {
        let Self {
            answering, values, ..
        } = self;
        // The list keeps its room from one function hap to the next.
        let graph = |values: &mut Vec<Hap>| {
            values.clear();
            side.values.answer_child_onto(
                thread,
                &state.set_span(side.values.asked_for(met, part)),
                values,
            );
        };
        match answering.get_or_insert_with(|| side.answering(state)) {
            Answering::Constant(constant) => {
                // One piece, the common case: the constant moved in place,
                // with no allocation.
                if met.within_one_cycle() {
                    constant.move_to_piece(met);
                    (std::slice::from_ref(constant), None)
                } else {
                    values.clear();
                    met.for_each_cycle(|piece| {
                        let mut hap = constant.clone();
                        hap.move_to_piece(piece);
                        values.push(hap);
                    });
                    (values, None)
                }
            }
            // A table samples a signal for a part in one cycle, and so does
            // the walk. Any other part asks the graph, which names each
            // value.
            Answering::Signal(signal, probe) => {
                let signal: &'a table::ValueSignal = signal;
                if part.within_one_cycle() && signal.answer(met, part, probe, values).is_some() {
                    return (values, signal.control());
                }
                graph(values);
                (values, None)
            }
            Answering::Graph => {
                graph(values);
                (values, None)
            }
        }
    }
}

/// One cycle of a concatenation appended onto `out`: the pattern whose turn the
/// cycle of `state` is writes its haps there, each moved to its cycle. The span
/// of `state` lies in one cycle.
fn slowcat_cycle_onto(thread: &QueryThread, pats: &[Pattern], state: &State, out: &mut Vec<Hap>) {
    if pats.is_empty() {
        return;
    }
    let n = pats.len() as i128;
    let cycle = state.span.begin.sam().numer();
    let pat = &pats[cycle.rem_euclid(n) as usize];
    slowcat_cycle_item_onto(thread, n, cycle, pat, state, out);
}

fn slowcat_cycle_pure_onto(
    thread: &QueryThread,
    cells: &[Arc<NodeCell>],
    state: &State,
    out: &mut Vec<Hap>,
) {
    if cells.is_empty() {
        return;
    }
    let n = cells.len() as i128;
    let cycle = state.span.begin.sam().numer();
    let cell = &cells[cycle.rem_euclid(n) as usize];
    if matches!(cell.kind, Node::Silence) {
        return;
    }
    slowcat_cycle_with_onto(n, cycle, state, out, |inner_span, out| {
        Pattern::query_plain_cell_onto(cell, thread, &state.set_span(inner_span), out);
    });
}

fn slowcat_cycle_item_onto(
    thread: &QueryThread,
    n: i128,
    cycle: i128,
    pat: &Pattern,
    state: &State,
    out: &mut Vec<Hap>,
) {
    if pat.is_rest() {
        return;
    }
    slowcat_cycle_with_onto(n, cycle, state, out, |inner_span, out| {
        pat.answer_child_onto(thread, &state.set_span(inner_span), out);
    });
}

enum SlowCatSlice<'a> {
    Pure(&'a [Arc<NodeCell>]),
    Pats(&'a [Pattern]),
}

fn split_slowcat_slice(node: &Node) -> Option<SlowCatSlice<'_>> {
    match node {
        Node::SplitQueries(inner_cell, inner_flags) if (*inner_flags & FLAG_VOLATILE) == 0 => {
            match &inner_cell.kind {
                Node::SlowCatPure(cells) => Some(SlowCatSlice::Pure(cells)),
                Node::SlowCat(pats) => Some(SlowCatSlice::Pats(pats)),
                _ => None,
            }
        }
        Node::SplitQueriesBoxed(inner_pat)
            if !inner_pat.has_runtime_settings()
                && (inner_pat.node_flags() & FLAG_VOLATILE) == 0 =>
        {
            match inner_pat.node_kind() {
                Node::SlowCatPure(cells) => Some(SlowCatSlice::Pure(cells)),
                Node::SlowCat(pats) => Some(SlowCatSlice::Pats(pats)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Fused L1-hot query for `fast(2^p)` over `SplitQueries(SlowCat | SlowCatPure)`
/// (such as `iter(n).fast(2^p)`). Returns `None` when not applicable, or
/// `Some(already_sorted)` when handled (`true` iff every emitted cycle took the
/// sorted direct path so no post-sort scan is needed).
fn try_query_fast_pow2_slowcat_onto(
    thread: &QueryThread,
    inner: &Node,
    factor: Fraction,
    op: &'static str,
    state: &State,
    out: &mut Vec<Hap>,
) -> Option<bool> {
    if factor.denom() != 1 || factor.numer() <= 1 || factor.numer() > 65_536 {
        return None;
    }
    let scale = factor.numer() as u32;
    if !scale.is_power_of_two() {
        return None;
    }
    let slice = split_slowcat_slice(inner)?;
    let shift = scale.trailing_zeros();
    let (Some(ab), Some(ae)) = (
        state.span.begin.checked_mul_pow2(shift),
        state.span.end.checked_mul_pow2(shift),
    ) else {
        return None;
    };
    if ab >= ae {
        return None;
    }
    let asked_span = TimeSpan::new(ab, ae);
    let Some(est_cycles) = asked_span
        .cycle_count()
        .filter(|count| *count <= MAX_QUERY_SPAN_CYCLES)
        .and_then(|count| usize::try_from(count).ok())
    else {
        refuse(QueryLimit::QuerySpan {
            cycles: MAX_QUERY_SPAN_CYCLES,
        });
        return Some(true);
    };
    let len = match slice {
        SlowCatSlice::Pure(cells) => cells.len(),
        SlowCatSlice::Pats(pats) => pats.len(),
    };
    let n = len as i128;
    let from = out.len();
    if ab.denom() == 1 && ae.denom() == 1 && (1..=64).contains(&len) && est_cycles >= len * 2 {
        let mut seq_buf: [Option<&Arc<[sequence::Layer]>>; 64] = [None; 64];
        let mut all_seq = true;
        match slice {
            SlowCatSlice::Pure(cells) => {
                for (dst, cell) in seq_buf[..len].iter_mut().zip(cells.iter()) {
                    if let Node::Sequence(layers) = &cell.kind {
                        *dst = Some(layers);
                    } else {
                        all_seq = false;
                        break;
                    }
                }
            }
            SlowCatSlice::Pats(pats) => {
                for (dst, pat) in seq_buf[..len].iter_mut().zip(pats.iter()) {
                    if !pat.has_runtime_settings()
                        && let Node::Sequence(layers) = pat.node_kind()
                    {
                        *dst = Some(layers);
                    } else {
                        all_seq = false;
                        break;
                    }
                }
            }
        }
        if all_seq {
            let mut refs: [&Arc<[sequence::Layer]>; 64] =
                [seq_buf[0].expect("len >= 1 and all_seq"); 64];
            for (dst, src) in refs[..len].iter_mut().zip(&seq_buf[..len]) {
                *dst = src.expect("all_seq verified");
            }
            if let Some(sorted) = sequence::try_query_fast_pow2_slowcat_static_int(
                thread,
                &refs[..len],
                ab.numer(),
                ae.numer(),
                shift,
                from,
                out,
            ) {
                return Some(sorted);
            }
        }
    }
    let mut cur = ab;
    let mut first_cycle = true;
    let mut all_sorted = true;
    // Pre-reserve exact capacity before cycle 0 when cycle 0 is a single-layer
    // `period == 1` sequence, avoiding an initial throwaway allocation + realloc.
    if est_cycles >= 2 && len > 0 && cur.denom() == 1 {
        let idx0 = cur.numer().rem_euclid(n) as usize;
        let first_steps = match slice {
            SlowCatSlice::Pure(cells) => match &cells[idx0].kind {
                Node::Sequence(layers) => sequence::single_cycle_step_count(layers),
                _ => 0,
            },
            SlowCatSlice::Pats(pats) => {
                let pat = &pats[idx0];
                if !pat.has_runtime_settings()
                    && let Node::Sequence(layers) = pat.node_kind()
                {
                    sequence::single_cycle_step_count(layers)
                } else {
                    0
                }
            }
        };
        if first_steps > 0 {
            let want = first_steps.saturating_mul(est_cycles).min(65_536);
            if thread.within_hap_budget(want) {
                out.reserve_exact(want);
                first_cycle = false;
            }
        }
    }
    while cur < ae {
        let next = cur.next_sam().min(ae);
        let cycle_from = out.len();
        let fast_done = if len > 0
            && cur.denom() == 1
            && next.denom() == 1
            && next.numer() == cur.numer() + 1
        {
            let cycle_num = cur.numer();
            let idx = cycle_num.rem_euclid(n) as usize;
            let direct_seq = match slice {
                SlowCatSlice::Pure(cells) => match &cells[idx].kind {
                    Node::Silence => Ok(None),
                    Node::Sequence(layers) => Ok(Some(layers)),
                    _ => Err(()),
                },
                SlowCatSlice::Pats(pats) => {
                    let pat = &pats[idx];
                    if pat.is_rest() {
                        Ok(None)
                    } else if !pat.has_runtime_settings()
                        && let Node::Sequence(layers) = pat.node_kind()
                    {
                        Ok(Some(layers))
                    } else {
                        Err(())
                    }
                }
            };
            match direct_seq {
                Ok(None) => true,
                Ok(Some(layers)) => match sequence::push_whole_cycle_scaled_pow2(
                    thread, layers, cycle_num, shift, from, out,
                ) {
                    Some(Ok(())) => true,
                    Some(Err(sequence::SequenceStop::Query)) => {
                        out.truncate(from);
                        return Some(true);
                    }
                    Some(Err(sequence::SequenceStop::Range)) => {
                        out.truncate(from);
                        refuse(QueryLimit::NativeFraction { operation: op });
                        return Some(true);
                    }
                    None => false,
                },
                Err(()) => false,
            }
        } else {
            false
        };
        if !fast_done {
            all_sorted = false;
            let sub = TimeSpan::new(cur, next);
            let cycle = state.set_span(sub);
            match slice {
                SlowCatSlice::Pure(cells) => slowcat_cycle_pure_onto(thread, cells, &cycle, out),
                SlowCatSlice::Pats(pats) => slowcat_cycle_onto(thread, pats, &cycle, out),
            }
            let count = out.len() - from;
            if thread.stop_requested() || !thread.charge(count) {
                out.truncate(from);
                return Some(true);
            }
            thread.note_accumulated(count);
            if out.len() > cycle_from {
                Hap::map_times(&mut out[cycle_from..], sub, None, |t| {
                    t.checked_div_pow2(shift)
                        .unwrap_or_else(|| fast_hap_time(t, factor, op))
                });
            }
        } else if thread.stop_requested() {
            out.truncate(from);
            return Some(true);
        }
        if first_cycle {
            first_cycle = false;
            let count = out.len() - from;
            if count > 0 && est_cycles > 2 {
                let want = count.saturating_mul(est_cycles - 1).min(65_536);
                out.reserve(want);
            }
        }
        cur = next;
    }
    Some(all_sorted)
}

fn slowcat_cycle_with_onto(
    n: i128,
    cycle: i128,
    state: &State,
    out: &mut Vec<Hap>,
    query_inner: impl FnOnce(TimeSpan, &mut Vec<Hap>),
) {
    // `cycle - floor(cycle / n)` lies between 0 and `cycle` and is
    // never `i128::MIN`, so it and its negation fit. A count of whole
    // cycles moves a time with no division.
    let offset = cycle - cycle.div_euclid(n);
    let shift = |span: TimeSpan, by: i128| {
        Some(TimeSpan::new(
            span.begin.checked_add_int(by)?,
            span.end.checked_add_int(by)?,
        ))
    };
    let from = out.len();
    let moved = shift(state.span, -offset).and_then(|inner_span| {
        query_inner(inner_span, out);
        out[from..].iter_mut().try_for_each(|hap| {
            let part = shift(hap.part, offset)?;
            // A hap that is not cut reads the times of its part one time.
            if let Some(whole) = hap.whole.as_mut() {
                *whole = match *whole == hap.part {
                    true => part,
                    false => shift(*whole, offset)?,
                };
            }
            hap.part = part;
            Some(())
        })
    });
    if moved.is_none() {
        out.truncate(from);
        refuse(QueryLimit::NativeFraction {
            operation: "slowcat",
        });
    }
}

/// The value side of one `appLeft` node, as a query meets it.
#[derive(Clone, Copy)]
pub(crate) struct ValueSide<'a> {
    pub(crate) functions: &'a Pattern,
    values: &'a Pattern,
    apply: &'a Combine,
    flow: LookupFlow,
    reading: &'a ValueReading,
}

/// Reusable 4-slot stack cache for small (`2..=8` steps) segmented waves so
/// 1/8-cycle chord windows share one [`value::StepPatch`] across all steps and
/// every chord note from the same base.
type StepPatchCache = [Option<(value::PatchedBase, Option<u32>, value::StepPatch)>; 4];

struct SmallStepSlot {
    part: TimeSpan,
    count: u8,
    bounds: [Fraction; 9],
    nums: [f64; 8],
    step_patches: StepPatchCache,
    step_next: u8,
    fallback_caches: Option<Box<[value::SharedWaveCache; 8]>>,
}

impl SmallStepSlot {
    fn new(part: TimeSpan, count: usize, bounds: [Fraction; 9], nums: [f64; 8]) -> Self {
        Self {
            part,
            count: count as u8,
            bounds,
            nums,
            step_patches: [None, None, None, None],
            step_next: 0,
            fallback_caches: None,
        }
    }
}

/// Apply one segmented wave `(key, wave)` to `hf` using inline stack buffers
/// when `count <= 8`, sharing one [`value::StepPatch`] across all steps and
/// across chord haps from the same uniform-key base.
#[allow(clippy::too_many_arguments)]
fn meet_steps_small(
    thread: &QueryThread,
    mut hf: Hap,
    key: &Key,
    wave: &fused::Wave,
    grid: &fused::StepGrid,
    small_cache: &mut [Option<SmallStepSlot>; 4],
    small_next: &mut u8,
    state: &State,
    from: usize,
    out: &mut Vec<Hap>,
) -> Option<bool> {
    let count = grid.len();
    debug_assert!((2..=8).contains(&count));
    let slot_idx = if let Some(idx) = small_cache
        .iter()
        .position(|s| matches!(s, Some(slot) if slot.part == hf.part))
    {
        idx
    } else {
        let mut bounds = [Fraction::ZERO; 9];
        let mut nums = [0.0f64; 8];
        let steps_range = grid.steps();
        let grid_start = steps_range.start;
        let grid_end = steps_range.end;
        let mut begin = grid.time(grid_start)?;
        bounds[0] = if begin < hf.part.begin {
            hf.part.begin
        } else {
            begin
        };
        for (i, k) in steps_range.enumerate() {
            if thread.stop_requested() {
                return Some(false);
            }
            let end = grid.time(k + 1)?;
            bounds[i + 1] = if k + 1 == grid_end && end > hf.part.end {
                hf.part.end
            } else {
                end
            };
            nums[i] = wave.step_value_at(&begin, &state.controls)?;
            begin = end;
        }
        let idx = if let Some(empty) = small_cache.iter().position(Option::is_none) {
            empty
        } else {
            let victim = usize::from(*small_next & 3);
            *small_next = small_next.wrapping_add(1);
            victim
        };
        small_cache[idx] = Some(SmallStepSlot::new(hf.part, count, bounds, nums));
        idx
    };
    let slot = small_cache[slot_idx].as_mut().expect("small slot present");
    let count = usize::from(slot.count);
    let apply = Combine::Composed(compose::ComposeOp::Set);
    out.reserve(count);
    let bulk_ok = thread.charge(out.len() - from + count);
    // Single-allocation StepPatch fast path: one `Arc<StepPatch>` covers all
    // `count` steps AND every chord note sharing the same uniform-key base
    // (or the same `(base, row)` for non-uniform bases).
    if let Value::Object(map) = &hf.value {
        let mut hit: Option<(value::StepPatch, u32, u16)> = None;
        for (b, fixed_row, cached) in slot.step_patches.iter().flatten() {
            if let Some((row, layer_idx)) = map.matches_step_base(b)
                && fixed_row.is_none_or(|r| r == row)
            {
                hit = Some((cached.clone(), row, layer_idx));
                break;
            }
        }
        let resolved = if let Some(found) = hit {
            Some(found)
        } else if let Some((base, row, layer_idx, uniform)) = map.step_base() {
            let fixed_row = (!uniform).then_some(row);
            if let Some(mut built) =
                value::StepPatch::new_for_row(base.clone(), row, layer_idx, &[key], true, count)
            {
                for (i, &num) in slot.nums.iter().enumerate().take(count) {
                    built.set_single_slot(i, num);
                }
                let sp = built.finish();
                let idx = if let Some(empty) = slot.step_patches.iter().position(Option::is_none) {
                    empty
                } else {
                    let v = usize::from(slot.step_next & 3);
                    slot.step_next = slot.step_next.wrapping_add(1);
                    v
                };
                slot.step_patches[idx] = Some((base, fixed_row, sp.clone()));
                Some((sp, row, layer_idx))
            } else {
                None
            }
        } else {
            None
        };
        if let Some((sp, row, layer_idx)) = resolved {
            if !hf.keys.is_empty() {
                let live = controls::live_slot(key.as_str());
                let k = hf.keys.get();
                if k.slider_binding != 0 || live.is_some_and(|s| k.live_controls[s] != 0) {
                    let edit = hf.keys.edit();
                    if let Some(s) = live {
                        edit.live_controls[s] = 0;
                    }
                    edit.slider_binding = 0;
                    hf.keys.trim();
                }
            }
            let whole = hf.whole;
            let last = count - 1;
            for i in 0..count {
                let piece = TimeSpan::new(slot.bounds[i], slot.bounds[i + 1]);
                let (context, keys) = if i == last {
                    (
                        std::mem::take(&mut hf.context),
                        std::mem::take(&mut hf.keys),
                    )
                } else {
                    (hf.context.clone(), hf.keys.clone())
                };
                let hap = Hap {
                    whole,
                    part: piece,
                    value: Value::Object(OrderedMap::from_step_patch(&sp, row, layer_idx, i)),
                    context,
                    keys,
                };
                if bulk_ok {
                    out.push(hap);
                } else if thread.stop_requested() || !thread.push_budgeted(out, from, hap) {
                    return Some(false);
                }
            }
            if bulk_ok {
                thread.note_accumulated(out.len() - from);
            }
            fused::note(fused::Hit::StepMany);
            return Some(true);
        }
    }
    let caches = slot.fallback_caches.get_or_insert_with(|| {
        Box::new(std::array::from_fn(|_| value::SharedWaveCache::default()))
    });
    let mut template = Some(hf);
    for i in 0..count {
        let mut hap = if i + 1 == count {
            template.take().expect("last step moves template")
        } else {
            template.as_ref().expect("template present").clone()
        };
        if !Meeting::set_number_in_place_cached(
            &mut hap,
            key,
            slot.nums[i],
            TimeSpan::new(slot.bounds[i], slot.bounds[i + 1]),
            &apply,
            LookupFlow::Set,
            Some(&mut caches[i]),
        ) {
            return None;
        }
        if bulk_ok {
            out.push(hap);
        } else if thread.stop_requested() || !thread.push_budgeted(out, from, hap) {
            return Some(false);
        }
    }
    if bulk_ok {
        thread.note_accumulated(out.len() - from);
    }
    fused::note(fused::Hit::StepMany);
    Some(true)
}

impl<'a> ValueSide<'a> {
    /// The value side of `node`, when `node` is an `appLeft`.
    pub(crate) fn of(node: &'a Node) -> Option<Self> {
        let Node::AppLeft(pair) = node else {
            return None;
        };
        let AppLeftNode(functions, values, reading, apply, flow) = &**pair;
        Some(Self {
            functions,
            values,
            apply,
            flow: *flow,
            reading,
        })
    }

    /// How a query of `state` reads this side: with no query of its graph
    /// when the side was found to be a constant or a signal, and the signal
    /// may answer as its table does.
    fn answering(&self, state: &State) -> Answering<'a> {
        match self.reading.of(self.values, state) {
            Some(ReadValues::Constant(hap)) => Answering::Constant(Hap::clone(hap)),
            Some(ReadValues::Signal(signal)) if self.values.tables_may_answer() => {
                Answering::Signal(signal, state.clone())
            }
            _ => Answering::Graph,
        }
    }

    /// The haps that the function haps `functions` make with this side,
    /// from one query of the side, when its graph is one of
    /// [discrete](Pattern::is_discrete) haps that no reading answers. Each
    /// function hap asks the side over its part, so one query over all the
    /// parts holds the answer for each: the value haps that meet the part,
    /// cut to it, in the order of the answer.
    ///
    /// The haps come after those of `out`, and `functions` is then empty.
    /// `false` leaves both as they are: for a few function haps, which
    /// their own queries serve, for parts that leave most of the time
    /// between them free, for a part of no width, and for a side that a
    /// reading or its own queries answer.
    pub(crate) fn met_in_one_query_on(
        &self,
        thread: &QueryThread,
        functions: &mut Vec<Hap>,
        state: &State,
        out: &mut Vec<Hap>,
    ) -> bool {
        /// The fewest function haps that one query of the values repays.
        const REPAYS_FROM: usize = 4;
        let width = |span: TimeSpan| span.end.to_f64() - span.begin.to_f64();
        let asked = functions.len() >= REPAYS_FROM
            && self.values.is_discrete()
            && self.reading.of(self.values, state).is_none()
            && functions.iter().all(|hf| hf.part.begin < hf.part.end);
        let all = asked
            .then(|| {
                Some(TimeSpan::new(
                    functions.iter().map(|hf| hf.part.begin).min()?,
                    functions.iter().map(|hf| hf.part.end).max()?,
                ))
            })
            .flatten();
        let Some(all) = all else {
            return false;
        };
        // The values between the parts are made for nothing.
        let filled: f64 = functions.iter().map(|hf| width(hf.part)).sum();
        if filled * 2.0 < width(all) {
            return false;
        }
        with_list(|values| {
            self.values
                .answer_child_onto(thread, &state.set_span(all), values);
            let n_vals = values.len();
            // Stack fast path for <= 32 value haps (all normal window queries):
            // zero heap allocations for `by_begin`, `latest_end`, and `meeting`.
            let mut small_by_begin = [0usize; 32];
            let mut small_latest_end = [Fraction::ZERO; 32];
            let mut small_meeting = [0usize; 32];
            let heap_by_begin;
            let mut heap_latest_end = Vec::new();
            let mut heap_meeting = Vec::new();
            let values_sorted = n_vals <= 1
                || (1..n_vals).all(|i| values[i - 1].part.begin <= values[i].part.begin);
            let (by_begin, latest_end): (&[usize], &[Fraction]) = if n_vals <= 32 {
                for (i, slot) in small_by_begin[..n_vals].iter_mut().enumerate() {
                    *slot = i;
                }
                if !values_sorted {
                    small_by_begin[..n_vals].sort_unstable_by(|a, b| {
                        values[*a]
                            .part
                            .begin
                            .cmp(&values[*b].part.begin)
                            .then_with(|| a.cmp(b))
                    });
                }
                let mut latest = Fraction::ZERO;
                for (i, &index) in small_by_begin[..n_vals].iter().enumerate() {
                    let end = values[index].part.end;
                    latest = if i == 0 { end } else { end.max(latest) };
                    small_latest_end[i] = latest;
                }
                (&small_by_begin[..n_vals], &small_latest_end[..n_vals])
            } else {
                let mut indices: Vec<usize> = (0..n_vals).collect();
                if !values_sorted {
                    indices.sort_unstable_by(|a, b| {
                        values[*a]
                            .part
                            .begin
                            .cmp(&values[*b].part.begin)
                            .then_with(|| a.cmp(b))
                    });
                }
                heap_by_begin = indices;
                heap_latest_end.reserve(n_vals);
                for &index in &heap_by_begin {
                    let end = values[index].part.end;
                    heap_latest_end.push(
                        heap_latest_end
                            .last()
                            .map_or(end, |latest| end.max(*latest)),
                    );
                }
                (&heap_by_begin, &heap_latest_end)
            };
            let from = out.len();
            out.reserve(functions.len());
            // Query-local direct-mapped stack cache for pure ComposeOp pairs
            // whose inputs are graph-backed maps or scalars.
            let cache_pairs = functions.len() >= 8 && matches!(self.apply, Combine::Composed(_));
            type PairEntry = ((usize, u64), (usize, u64), Value);
            let mut pair_cache: [Option<PairEntry>; 32] = std::array::from_fn(|_| None);
            let pair_slot = |fk: (usize, u64), vk: (usize, u64)| -> usize {
                let mut h = (fk.0 as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
                    ^ fk.1.wrapping_mul(0xbf58_476d_1ce4_e5b9);
                h ^= (vk.0 as u64).wrapping_mul(0x94d0_49bb_1331_11eb)
                    ^ vk.1.wrapping_mul(0x517c_c1b7_2722_0a95);
                ((h ^ (h >> 32)).wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 59) as usize
            };
            for mut hf in functions.drain(..) {
                let span = hf.part;
                let first = latest_end.partition_point(|end| *end <= span.begin);
                let after = by_begin.partition_point(|index| values[*index].part.begin < span.end);
                let slice = by_begin.get(first..after).unwrap_or_default();
                let meeting: &[usize] = if n_vals <= 32 {
                    let mut m_len = 0usize;
                    for &index in slice {
                        if values[index].part.end > span.begin {
                            small_meeting[m_len] = index;
                            m_len += 1;
                        }
                    }
                    small_meeting[..m_len].sort_unstable();
                    &small_meeting[..m_len]
                } else {
                    heap_meeting.clear();
                    heap_meeting.extend(
                        slice
                            .iter()
                            .copied()
                            .filter(|index| values[*index].part.end > span.begin),
                    );
                    heap_meeting.sort_unstable();
                    &heap_meeting
                };
                // The part of the value hap, as the query of the part cuts it,
                // then where the two haps meet.
                let met = |hv: &Hap| {
                    let cut =
                        TimeSpan::new(hv.part.begin.max(span.begin), hv.part.end.min(span.end));
                    span.intersection(&cut)
                };
                // One meeting is the hap's own, as in [`Self::meet`].
                if let [index] = meeting
                    && let Some(part) = met(&values[*index])
                {
                    let hv = &values[*index];
                    let cached_hit = if cache_pairs
                        && hf.keys.is_none()
                        && hv.keys.is_none()
                        && let (Some(fk), Some(vk)) =
                            (hf.value.stable_identity(), hv.value.stable_identity())
                    {
                        let slot = pair_slot(fk, vk);
                        let alt = slot ^ 1;
                        let hit = if let Some((cfk, cvk, val)) = &pair_cache[slot]
                            && *cfk == fk
                            && *cvk == vk
                        {
                            Some(val.clone())
                        } else if let Some((cfk, cvk, val)) = &pair_cache[alt]
                            && *cfk == fk
                            && *cvk == vk
                        {
                            Some(val.clone())
                        } else {
                            None
                        };
                        if let Some(val) = hit {
                            let mut context = hv.context.clone();
                            context.append(&hf.context);
                            hf = Hap {
                                whole: hf.whole,
                                part,
                                value: val,
                                context,
                                keys: ContextKeys::NONE,
                            };
                            true
                        } else {
                            if !Meeting::in_place(&mut hf, hv, part, self.apply, self.flow) {
                                hf = applied_hap(&hf, hv, hf.whole, part, self.apply, self.flow);
                            }
                            if !thread.error_pending() && hf.keys.is_none() {
                                let dst = if pair_cache[slot].is_none() {
                                    slot
                                } else if pair_cache[alt].is_none() {
                                    alt
                                } else {
                                    slot
                                };
                                pair_cache[dst] = Some((fk, vk, hf.value.clone()));
                            }
                            true
                        }
                    } else {
                        false
                    };
                    if !cached_hit && !Meeting::in_place(&mut hf, hv, part, self.apply, self.flow) {
                        hf = applied_hap(&hf, hv, hf.whole, part, self.apply, self.flow);
                    }
                    if thread.stop_requested() || !thread.push_budgeted(out, from, hf) {
                        out.truncate(from);
                        return;
                    }
                    continue;
                }
                for hv in meeting.iter().map(|index| &values[*index]) {
                    let Some(part) = met(hv) else {
                        continue;
                    };
                    let hap = if cache_pairs
                        && hf.keys.is_none()
                        && hv.keys.is_none()
                        && let (Some(fk), Some(vk)) =
                            (hf.value.stable_identity(), hv.value.stable_identity())
                    {
                        let slot = pair_slot(fk, vk);
                        let alt = slot ^ 1;
                        let hit = if let Some((cfk, cvk, val)) = &pair_cache[slot]
                            && *cfk == fk
                            && *cvk == vk
                        {
                            Some(val.clone())
                        } else if let Some((cfk, cvk, val)) = &pair_cache[alt]
                            && *cfk == fk
                            && *cvk == vk
                        {
                            Some(val.clone())
                        } else {
                            None
                        };
                        if let Some(val) = hit {
                            let mut context = hv.context.clone();
                            context.append(&hf.context);
                            Hap {
                                whole: hf.whole,
                                part,
                                value: val,
                                context,
                                keys: ContextKeys::NONE,
                            }
                        } else {
                            let hap = applied_hap(&hf, hv, hf.whole, part, self.apply, self.flow);
                            if !thread.error_pending() && hap.keys.is_none() {
                                let dst = if pair_cache[slot].is_none() {
                                    slot
                                } else if pair_cache[alt].is_none() {
                                    alt
                                } else {
                                    slot
                                };
                                pair_cache[dst] = Some((fk, vk, hap.value.clone()));
                            }
                            hap
                        }
                    } else {
                        applied_hap(&hf, hv, hf.whole, part, self.apply, self.flow)
                    };
                    if thread.stop_requested() || !thread.push_budgeted(out, from, hap) {
                        out.truncate(from);
                        return;
                    }
                }
            }
        });
        true
    }

    /// The haps of the function hap `hf` that a wave in steps makes, one
    /// for each step that its part meets, with the number of the step set
    /// under `key`. Whether the query goes on, or `None` for a meeting that
    /// the haps of the steps make, with `out` as it was.
    #[allow(clippy::too_many_arguments)]
    fn meet_steps(
        &self,
        thread: &QueryThread,
        hf: &Hap,
        steps: &fused::StepWave,
        step_cache: &mut StepWaveCacheSet,
        state: &State,
        from: usize,
        out: &mut Vec<Hap>,
    ) -> Option<bool> {
        let set = matches!(self.apply, Combine::Composed(compose::ComposeOp::Set))
            && self.flow == LookupFlow::Set;
        if !set || hf.pick_lookup().is_some() || !hf.value.is_data(0) {
            return None;
        }
        let Value::Object(base_map) = &hf.value else {
            return None;
        };
        let cached = if step_cache.find(hf.part).is_some() {
            step_cache.find(hf.part).expect("checked present")
        } else {
            let grid = steps.grid_over(hf.part)?;
            let count = grid.len();
            if count == 0 {
                return Some(true);
            }
            if thread.stop_requested() {
                return Some(false);
            }
            // Single-step shortcut avoids allocating a shared Table or cache.
            if count == 1 {
                let k = grid.steps().start;
                let begin = grid.time(k)?;
                let end = grid.time(k + 1)?;
                let piece = grid.cut(k, TimeSpan::new(begin, end), hf.part);
                let number = steps.value_at(begin, &state.controls)?;
                let mut hap = hf.clone();
                if !Meeting::set_number_in_place(
                    &mut hap, &steps.key, number, piece, self.apply, self.flow,
                ) {
                    return None;
                }
                if thread.stop_requested() || !thread.push_budgeted(out, from, hap) {
                    return Some(false);
                }
                fused::note(fused::Hit::StepMany);
                return Some(true);
            }
            if count <= 8 {
                let mut pieces = [TimeSpan::new(Fraction::ZERO, Fraction::ZERO); 8];
                let mut nums = [0.0f64; 8];
                let mut begin = grid.time(grid.steps().start)?;
                for (i, k) in grid.steps().enumerate() {
                    if thread.stop_requested() {
                        return Some(false);
                    }
                    let end = grid.time(k + 1)?;
                    pieces[i] = grid.cut(k, TimeSpan::new(begin, end), hf.part);
                    nums[i] = steps.value_at(begin, &state.controls)?;
                    begin = end;
                }
                out.reserve(count);
                let bulk_ok = thread.charge(out.len() - from + count);
                for i in 0..count {
                    let mut hap = hf.clone();
                    if !Meeting::set_number_in_place(
                        &mut hap, &steps.key, nums[i], pieces[i], self.apply, self.flow,
                    ) {
                        return None;
                    }
                    if bulk_ok {
                        out.push(hap);
                    } else if thread.stop_requested() || !thread.push_budgeted(out, from, hap) {
                        return Some(false);
                    }
                }
                if bulk_ok {
                    thread.note_accumulated(out.len() - from);
                }
                fused::note(fused::Hit::StepMany);
                return Some(true);
            }
            let mut pieces = step_cache.take_victim_pieces(count);
            let mut begin = grid.time(grid.steps().start)?;
            let values: Arc<[Value]> = if count <= 32 {
                let mut nums = [0.0f64; 32];
                for (slot, k) in nums[..count].iter_mut().zip(grid.steps()) {
                    if thread.stop_requested() {
                        return Some(false);
                    }
                    let end = grid.time(k + 1)?;
                    pieces.push(grid.cut(k, TimeSpan::new(begin, end), hf.part));
                    *slot = steps.value_at(begin, &state.controls)?;
                    begin = end;
                }
                nums[..count].iter().map(|&n| Value::F64(n)).collect()
            } else {
                let mut vals = Vec::with_capacity(count);
                for k in grid.steps() {
                    if thread.stop_requested() {
                        return Some(false);
                    }
                    let end = grid.time(k + 1)?;
                    pieces.push(grid.cut(k, TimeSpan::new(begin, end), hf.part));
                    vals.push(Value::F64(steps.value_at(begin, &state.controls)?));
                    begin = end;
                }
                Arc::from(vals)
            };
            step_cache.insert(StepWaveCache {
                part: hf.part,
                values,
                pieces,
                table: None,
            })
        };
        let count = cached.pieces.len();
        // Fast path: share one `PatchedTable` across all chord haps backed by
        // the same keyed base table or direct-keyed sequence layer.
        let shared_patched = if let Some((base_source, base_row)) = base_map.dressed_base() {
            if let Some((cached_base, cached_patched)) = &cached.table
                && cached_base.same_source(&base_source)
            {
                Some((Arc::clone(cached_patched), base_row))
            } else if let Some(patched) = value::Table::patched_keyed_table(
                &base_source,
                steps.key.clone(),
                Arc::clone(&cached.values),
            ) {
                let patched = Arc::new(patched);
                cached.table = Some((base_source, Arc::clone(&patched)));
                Some((patched, base_row))
            } else {
                None
            }
        } else {
            None
        };
        let fallback_table = if shared_patched.is_none() {
            Some(Arc::new(value::Table::patched(
                base_map,
                steps.key.clone(),
                Arc::clone(&cached.values),
            )?))
        } else {
            None
        };
        let mut template = hf.clone();
        // Clear value on the template so per-step clones do not bump base_map's Arc.
        template.value = Value::Undefined;
        if !template.keys.is_empty() {
            let slot = controls::live_slot(steps.key.as_str());
            let k = template.keys.get();
            if k.slider_binding != 0 || slot.is_some_and(|s| k.live_controls[s] != 0) {
                let edit = template.keys.edit();
                if let Some(slot) = slot {
                    edit.live_controls[slot] = 0;
                }
                edit.slider_binding = 0;
                template.keys.trim();
            }
        }
        out.reserve(count);
        let bulk_ok = thread.charge(out.len() - from + count);
        for (index, &piece) in cached.pieces.iter().enumerate() {
            let map = match &shared_patched {
                Some((table, base_row)) => table.map_packed(*base_row, index),
                None => fallback_table
                    .as_ref()
                    .expect("fallback table")
                    .map(index)?,
            };
            let mut hap = template.clone();
            hap.value = Value::Object(map);
            hap.part = piece;
            if bulk_ok {
                out.push(hap);
            } else if thread.stop_requested() || !thread.push_budgeted(out, from, hap) {
                return Some(false);
            }
        }
        if bulk_ok {
            thread.note_accumulated(out.len() - from);
        }
        fused::note(fused::Hit::StepMany);
        Some(true)
    }

    /// Meet the function hap `hf` with the value side and push what they
    /// make after the haps of `out`, which held `from` haps of other nodes
    /// before this application wrote. `false` when the query stops.
    fn meet_on(
        &self,
        thread: &QueryThread,
        mut hf: Hap,
        meetings: &mut Meetings<'a>,
        state: &State,
        from: usize,
        out: &mut Vec<Hap>,
    ) -> bool {
        let met = hf.whole_or_part();
        // The control of a segment of a wave takes the number of each step
        // that the part meets. A hap that it cannot set is met as any other.
        if let Some(steps) = meetings.steps.as_ref() {
            let part = hf.part;
            match steps.number(part, &state.controls) {
                Some(number) => {
                    if Meeting::set_number_in_place(
                        &mut hf, &steps.key, number, part, self.apply, self.flow,
                    ) {
                        fused::note(fused::Hit::StepOne);
                        return !thread.stop_requested() && thread.push_budgeted(out, from, hf);
                    }
                }
                None => {
                    if let Some(stopped) = self.meet_steps(
                        thread,
                        &hf,
                        steps,
                        &mut meetings.step_cache,
                        state,
                        from,
                        out,
                    ) {
                        return stopped;
                    }
                }
            }
        }
        // A gate says whether the hap stays as it is, with no hap of its
        // own. A hap with a lookup keeps it only while its value equals
        // itself, which the meeting reads.
        if hf.pick_lookup().is_none()
            && let Some(gate) = meetings.gate(self, state)
            && hf.part.within_one_cycle()
            && let Some(part) = hf.part.intersection(&met)
            && let Some(passes) = gate.passes(met, part, &state.controls)
        {
            hf.part = part;
            return !passes || (!thread.stop_requested() && thread.push_budgeted(out, from, hf));
        }
        // A wave gives its number with no hap of its own.
        if let Some((key, wave)) = meetings.wave(self, state)
            && hf.part.within_one_cycle()
            && let Some(part) = hf.part.intersection(&met)
            && let Some(number) = wave.number(&met, &part, &state.controls)
            && Meeting::set_number_in_place_cached(
                &mut hf,
                key,
                number,
                part,
                self.apply,
                self.flow,
                Some(meetings.wave_overlay_cache(met, part)),
            )
        {
            return !thread.stop_requested() && thread.push_budgeted(out, from, hf);
        }
        let (inner, named) = meetings.values(thread, self, met, hf.part, state);
        let Some(named) = named else {
            return meet_each(thread, hf, inner, self.apply, self.flow, from, out);
        };
        // The one value of a signal is set under the name of its control.
        if let [hv] = inner
            && let Some(part) = hf.part.intersection(&hv.part)
            && Meeting::set_in_place(&mut hf, &named.key, hv, part, self.apply, self.flow)
        {
            return !thread.stop_requested() && thread.push_budgeted(out, from, hf);
        }
        // Any other meeting reads each value as the control names it.
        let inner: Vec<Hap> = inner
            .iter()
            .filter_map(|hv| (named.map)(hv.clone()))
            .collect();
        meet_each(thread, hf, &inner, self.apply, self.flow, from, out)
    }
}

/// Meet the function hap `hf` with the value haps `inner` of an application
/// that combines by `apply`, and push what they make after the haps of
/// `out`, which held `from` haps of other nodes before the application
/// wrote. `false` when the query stops.
fn meet_each(
    thread: &QueryThread,
    mut hf: Hap,
    inner: &[Hap],
    apply: &Combine,
    flow: LookupFlow,
    from: usize,
    out: &mut Vec<Hap>,
) -> bool {
    // One value hap meets this function hap, and that one meeting is
    // the hap's own: the hap is consumed where it is, as a sampled
    // meeting is. Several meetings share the function hap as their
    // base, so each keeps a copy of it.
    if let [hv] = inner
        && let Some(part) = hf.part.intersection(&hv.part)
    {
        // A refused in-place meeting leaves the hap unchanged.
        if !Meeting::in_place(&mut hf, hv, part, apply, flow) {
            hf = applied_hap(&hf, hv, hf.whole, part, apply, flow);
        }
        return !thread.stop_requested() && thread.push_budgeted(out, from, hf);
    }
    for hv in inner {
        let Some(part) = hf.part.intersection(&hv.part) else {
            continue;
        };
        let hap = applied_hap(&hf, hv, hf.whole, part, apply, flow);
        if thread.stop_requested() || !thread.push_budgeted(out, from, hap) {
            return false;
        }
    }
    true
}

/// Answer `state` after the haps of `out`: each hap of `base` meets the
/// constant of each of `meetings` in turn.
///
/// A hap inside one cycle meets each constant over the same part and stays
/// one hap, so each meeting is made where the hap is, or of the hap and the
/// constant. A hap that covers several cycles meets each constant as the
/// node of its application does. A query that stops writes no hap.
fn query_run(
    thread: &QueryThread,
    base: &Pattern,
    meetings: &[RunMeeting],
    state: &State,
    out: &mut Vec<Hap>,
) {
    let from = out.len();
    base.answer_child_onto(thread, state, out);
    if out.len() == from || thread.stop_requested() {
        return;
    }
    // Single-hap fast path: avoid stack-initializing `SharedWaveCache` arrays
    // and running the `retain_from` closure when a voice emits one hap.
    if out.len() - from == 1 {
        let hf = &mut out[from];
        let met = hf.whole_or_part();
        match hf.part.intersection(&met) {
            None => {
                out.truncate(from);
                return;
            }
            Some(part) if part.within_one_cycle() => {
                for meeting in meetings {
                    if Meeting::in_place_run_meeting(hf, meeting, part, None) {
                        continue;
                    }
                    let constant = meeting.as_hap();
                    let apply = meeting.combine();
                    *hf = applied_hap(hf, &constant, hf.whole, part, &apply, meeting.flow());
                    if thread.stop_requested() {
                        out.truncate(from);
                        return;
                    }
                }
                if thread.stop_requested() {
                    out.truncate(from);
                }
                return;
            }
            Some(_) => {}
        }
    }
    // Fast in-place path when every hap's active intersection lies within a
    // single cycle (true for all sub-cycle windows even when `whole` spans
    // multiple cycles): zero scratch buffer, zero Hap moves.
    let single_cycle = out[from..].iter().all(|hf| hf.part.within_one_cycle())
        || out[from..].iter().all(|hf| {
            let met = hf.whole_or_part();
            match hf.part.intersection(&met) {
                Some(part) => part.within_one_cycle(),
                None => true,
            }
        });
    if single_cycle {
        let mut stopped = false;
        if let [meeting] = meetings {
            let mut cache = value::SharedWaveCache::default();
            retain_from(out, from, |hf| {
                if stopped {
                    return false;
                }
                let Some(part) = (match hf.whole {
                    None => (hf.part.begin <= hf.part.end).then_some(hf.part),
                    Some(whole) if whole == hf.part => {
                        (hf.part.begin <= hf.part.end).then_some(hf.part)
                    }
                    Some(whole) => hf.part.intersection(&whole),
                }) else {
                    return false;
                };
                if !Meeting::in_place_run_meeting(hf, meeting, part, Some(&mut cache)) {
                    let constant = meeting.as_hap();
                    let apply = meeting.combine();
                    *hf = applied_hap(hf, &constant, hf.whole, part, &apply, meeting.flow());
                    if thread.stop_requested() {
                        stopped = true;
                        return false;
                    }
                }
                if thread.stop_requested() {
                    stopped = true;
                    return false;
                }
                true
            });
            if stopped {
                out.truncate(from);
            }
            return;
        }
        if meetings.len() == 2 {
            let mut caches: [value::SharedWaveCache; 2] = Default::default();
            retain_from(out, from, |hf| {
                if stopped {
                    return false;
                }
                let Some(part) = (match hf.whole {
                    None => (hf.part.begin <= hf.part.end).then_some(hf.part),
                    Some(whole) if whole == hf.part => {
                        (hf.part.begin <= hf.part.end).then_some(hf.part)
                    }
                    Some(whole) => hf.part.intersection(&whole),
                }) else {
                    return false;
                };
                for (m_idx, meeting) in meetings.iter().enumerate() {
                    if Meeting::in_place_run_meeting(hf, meeting, part, caches.get_mut(m_idx)) {
                        continue;
                    }
                    let constant = meeting.as_hap();
                    let apply = meeting.combine();
                    *hf = applied_hap(hf, &constant, hf.whole, part, &apply, meeting.flow());
                    if thread.stop_requested() {
                        stopped = true;
                        return false;
                    }
                }
                if thread.stop_requested() {
                    stopped = true;
                    return false;
                }
                true
            });
            if stopped {
                out.truncate(from);
            }
            return;
        }
        let mut caches: [value::SharedWaveCache; 4] = Default::default();
        retain_from(out, from, |hf| {
            if stopped {
                return false;
            }
            let Some(part) = (match hf.whole {
                None => (hf.part.begin <= hf.part.end).then_some(hf.part),
                Some(whole) if whole == hf.part => {
                    (hf.part.begin <= hf.part.end).then_some(hf.part)
                }
                Some(whole) => hf.part.intersection(&whole),
            }) else {
                return false;
            };
            for (m_idx, meeting) in meetings.iter().enumerate() {
                if Meeting::in_place_run_meeting(hf, meeting, part, caches.get_mut(m_idx)) {
                    continue;
                }
                let constant = meeting.as_hap();
                let apply = meeting.combine();
                *hf = applied_hap(hf, &constant, hf.whole, part, &apply, meeting.flow());
                if thread.stop_requested() {
                    stopped = true;
                    return false;
                }
            }
            if thread.stop_requested() {
                stopped = true;
                return false;
            }
            true
        });
        if stopped {
            out.truncate(from);
        }
        return;
    }
    // Multi-cycle fallback: move haps into scratch list and expand per cycle.
    if out.len() - from == 1
        && let [meeting] = meetings
    {
        let hf = out.pop().expect("single hap");
        if !meeting.meet(thread, hf, None, out) {
            out.truncate(from);
        }
        return;
    }
    if let [meeting] = meetings {
        let answered = with_list(|haps| {
            haps.extend(out.drain(from..));
            out.reserve(haps.len());
            let mut cache = value::SharedWaveCache::default();
            for mut hf in haps.drain(..) {
                let met = hf.whole_or_part();
                if !met.within_one_cycle() {
                    if !meeting.meet(thread, hf, Some(&mut cache), out) {
                        return None;
                    }
                    continue;
                }
                let Some(part) = hf.part.intersection(&met) else {
                    continue;
                };
                if !Meeting::in_place_run_meeting(&mut hf, meeting, part, Some(&mut cache)) {
                    let constant = meeting.as_hap();
                    let apply = meeting.combine();
                    hf = applied_hap(&hf, &constant, hf.whole, part, &apply, meeting.flow());
                    if thread.stop_requested() {
                        return None;
                    }
                }
                if thread.stop_requested() || !thread.push_budgeted(out, from, hf) {
                    return None;
                }
            }
            Some(())
        });
        if answered.is_none() {
            out.truncate(from);
        }
        return;
    }
    let apply_all_on_piece = |mut hf: Hap,
                              part: TimeSpan,
                              use_cache: bool,
                              caches: &mut [value::SharedWaveCache],
                              out: &mut Vec<Hap>|
     -> bool {
        for (m_idx, meeting) in meetings.iter().enumerate() {
            let cache = if use_cache {
                caches.get_mut(m_idx)
            } else {
                None
            };
            if Meeting::in_place_run_meeting(&mut hf, meeting, part, cache) {
                continue;
            }
            let constant = meeting.as_hap();
            let apply = meeting.combine();
            hf = applied_hap(&hf, &constant, hf.whole, part, &apply, meeting.flow());
            if thread.stop_requested() {
                return false;
            }
        }
        !thread.stop_requested() && thread.push_budgeted(out, from, hf)
    };
    let answered = with_list(|haps| {
        haps.extend(out.drain(from..));
        out.reserve(haps.len());
        let use_cache = haps.len() > 1;
        let mut caches2: [value::SharedWaveCache; 2];
        let mut caches4: [value::SharedWaveCache; 4];
        let caches: &mut [value::SharedWaveCache] = if !use_cache {
            &mut []
        } else if meetings.len() <= 2 {
            caches2 = Default::default();
            &mut caches2
        } else {
            caches4 = Default::default();
            &mut caches4
        };
        for hf in haps.drain(..) {
            let met = hf.whole_or_part();
            let Some(overlap) = (match hf.whole {
                None => (hf.part.begin <= hf.part.end).then_some(hf.part),
                Some(whole) if whole == hf.part => {
                    (hf.part.begin <= hf.part.end).then_some(hf.part)
                }
                Some(whole) => hf.part.intersection(&whole),
            }) else {
                if met.begin != met.end
                    && !met
                        .cycle_count()
                        .is_some_and(|c| c <= MAX_QUERY_SPAN_CYCLES)
                {
                    refuse(QueryLimit::QuerySpan {
                        cycles: MAX_QUERY_SPAN_CYCLES,
                    });
                }
                continue;
            };
            // Every non-empty cycle piece of `overlap` lies strictly within one
            // cycle and inside `met`, so all subsequent meetings apply in place
            // without further cycle splitting.
            if overlap.begin < overlap.end {
                if !met.within_one_cycle()
                    && !met
                        .cycle_count()
                        .is_some_and(|c| c <= MAX_QUERY_SPAN_CYCLES)
                {
                    refuse(QueryLimit::QuerySpan {
                        cycles: MAX_QUERY_SPAN_CYCLES,
                    });
                    continue;
                }
                if overlap.within_one_cycle() {
                    if !apply_all_on_piece(hf, overlap, use_cache, caches, out) {
                        return None;
                    }
                    continue;
                }
                let mut stopped = false;
                let mut src = Some(hf);
                overlap.for_each_cycle(|part| {
                    if stopped {
                        return;
                    }
                    let piece_hap = if part.end == overlap.end {
                        src.take().expect("last piece moves hap")
                    } else {
                        src.as_ref().expect("hap present").clone()
                    };
                    if !apply_all_on_piece(piece_hap, part, use_cache, caches, out) {
                        stopped = true;
                    }
                });
                if stopped {
                    return None;
                }
                continue;
            }
            if met.within_one_cycle() {
                if !apply_all_on_piece(hf, overlap, use_cache, caches, out) {
                    return None;
                }
                continue;
            }
            let ok = with_list(|made| {
                with_list(|next| {
                    made.push(hf);
                    for (m_idx, meeting) in meetings.iter().enumerate() {
                        let mut cache = if use_cache {
                            caches.get_mut(m_idx)
                        } else {
                            None
                        };
                        for hf in made.drain(..) {
                            if !meeting.meet(thread, hf, cache.as_deref_mut(), next) {
                                return None;
                            }
                        }
                        std::mem::swap(made, next);
                    }
                    out.append(made);
                    Some(())
                })
            });
            ok?;
        }
        Some(())
    });
    if answered.is_none() {
        out.truncate(from);
    }
}

impl Pattern {
    /// The haps of this pattern, moved `shift` later, that a restart or a
    /// reset join reads for an outer hap with `part`, appended to `out`: the
    /// haps of the query's span, which the join then cuts to the part.
    ///
    /// A graph of discrete haps is asked over the part alone. The join cuts
    /// every other hap away, so the answer after the cut is the same, and
    /// the haps of the other sections of the span are not made. A query
    /// that fails or is refused only outside the part so answers. A graph
    /// whose joins make patterns at the query is asked over the part too,
    /// in a query that walks: each join then says when a pattern that it
    /// made reads the span, and a table says nothing. Any other graph is
    /// asked over the span, where it reads its values.
    pub(crate) fn query_restarted_onto(
        &self,
        thread: &QueryThread,
        shift: Fraction,
        state: &State,
        part: TimeSpan,
        out: &mut Vec<Hap>,
    ) {
        use purity::SpanRead;
        // What `self.late(shift)` answers, with no node for it.
        let over_part = |out: &mut Vec<Hap>| {
            self.query_moved_onto(thread, shift, "late", &state.set_span(part), out);
        };
        let over_span =
            |out: &mut Vec<Hap>| self.query_moved_onto(thread, shift, "late", state, out);
        let reads = match self.is_cacheable() {
            true => self.span_read(),
            false => SpanRead::Yes,
        };
        if reads == SpanRead::No {
            return over_part(out);
        }
        match self.node_kind() {
            // A stack answers its layers one after the other, so each layer
            // is asked as its own haps allow. A handle that binds a runtime
            // or is marked volatile is asked as it is.
            Node::Stack(layers) if !self.has_runtime_settings() && !self.is_volatile() => {
                let from = out.len();
                for layer in layers.iter().filter(|layer| !layer.is_rest()) {
                    stack_child_onto(thread, out, |out| {
                        layer.query_restarted_onto(thread, shift, state, part, out);
                    });
                    let held = out.len() - from;
                    if !thread.charge(held) {
                        out.truncate(from);
                        return;
                    }
                    thread.note_accumulated(held);
                }
            }
            Node::StackPure(cells) if !self.has_runtime_settings() && !self.is_volatile() => {
                let from = out.len();
                for cell in cells
                    .iter()
                    .filter(|cell| !matches!(cell.kind, Node::Silence))
                {
                    stack_child_onto(thread, out, |out| {
                        if matches!(cell.kind, Node::Sequence(_) | Node::Constants(_)) {
                            let part_state = state.set_span(part);
                            query_moved_with_onto(
                                thread,
                                shift,
                                "late",
                                &part_state,
                                out,
                                |asked, out| {
                                    Pattern::query_plain_cell_onto(cell, thread, asked, out);
                                },
                            );
                        } else {
                            plain_pattern(cell, 0)
                                .query_restarted_onto(thread, shift, state, part, out);
                        }
                    });
                    let held = out.len() - from;
                    if !thread.charge(held) {
                        out.truncate(from);
                        return;
                    }
                    thread.note_accumulated(held);
                }
            }
            _ if reads == SpanRead::Made && part != state.span => {
                let from = out.len();
                if asked_over_part(|| over_part(out)).is_none() {
                    out.truncate(from);
                    over_span(out);
                }
            }
            _ => over_span(out),
        }
    }

    /// The haps of this pattern, `factor` times as fast: what a
    /// [`Node::Fast`] over it answers.
    pub(crate) fn query_faster_onto(
        &self,
        thread: &QueryThread,
        factor: Fraction,
        grid: Option<&TempoGrid>,
        operation: &'static str,
        state: &State,
        out: &mut Vec<Hap>,
    ) {
        query_faster_with_onto(thread, factor, grid, operation, state, out, |asked, out| {
            self.answer_child_onto(thread, asked, out);
        });
    }

    /// The haps of this pattern, `by` later: what a [`Node::Shift`] over
    /// it answers.
    pub(crate) fn query_moved_onto(
        &self,
        thread: &QueryThread,
        by: Fraction,
        operation: &'static str,
        state: &State,
        out: &mut Vec<Hap>,
    ) {
        query_moved_with_onto(thread, by, operation, state, out, |asked, out| {
            self.answer_child_onto(thread, asked, out);
        });
    }

    /// Answer `self.focus_span(focus_span)` directly without constructing
    /// temporary `Late`/`Fast`/`Late` graph nodes on each query.
    pub(crate) fn query_focused_onto(
        &self,
        thread: &QueryThread,
        focus_span: TimeSpan,
        state: &State,
        out: &mut Vec<Hap>,
    ) {
        let (b, e) = (focus_span.begin, focus_span.end);
        if e == b {
            signal_query_error(|| "Division by zero".into());
            return;
        }
        let Some(rate) = e
            .checked_sub(b)
            .and_then(|span| Fraction::ONE.checked_div(span))
        else {
            refuse(QueryLimit::NativeFraction { operation: "focus" });
            return;
        };
        let Some(whole_cycles) = b.sam().checked_neg() else {
            refuse(QueryLimit::NativeFraction { operation: "focus" });
            return;
        };
        if rate <= Fraction::ZERO {
            let focused = self.focus_span(focus_span);
            focused.answer_child_onto(thread, state, out);
            return;
        }
        if b == Fraction::ZERO && rate == Fraction::ONE && whole_cycles == Fraction::ZERO {
            self.answer_child_onto(thread, state, out);
            return;
        }
        let inward = |mut t: Fraction| {
            if b != Fraction::ZERO {
                t = shift_query_time(t, b, "late");
            }
            if rate != Fraction::ONE {
                t = fast_query_time(t, rate, "fast");
            }
            if whole_cycles != Fraction::ZERO {
                t = shift_query_time(t, whole_cycles, "late");
            }
            t
        };
        let asked = state.with_span(|span| span.with_time(inward));
        if thread.stop_requested() {
            return;
        }
        let edges = Some((state.span.begin, state.span.end));
        let from = out.len();
        self.answer_child_onto(thread, &asked, out);
        Hap::map_times(&mut out[from..], asked.span, edges, |mut t| {
            if whole_cycles != Fraction::ZERO {
                t = shift_hap_time(t, whole_cycles, "late");
            }
            if rate != Fraction::ONE {
                t = fast_hap_time(t, rate, "fast");
            }
            if b != Fraction::ZERO {
                t = shift_hap_time(t, b, "late");
            }
            t
        });
    }

    /// Fast in-place path for a chain of 1..=8 wave or single-step `StepWave`
    /// `AppWave`/`AppLeft` nodes (`.lpf(sine...).pan(rand)`): walks the wave
    /// spine, queries the base directly into `out`, caches chord samples, and
    /// applies all numeric patches in a single `OrderedMap` allocation.
    pub(crate) fn try_query_wave_chain_onto(
        &self,
        thread: &QueryThread,
        state: &State,
        out: &mut Vec<Hap>,
    ) -> bool {
        const MAX_WAVES: usize = 8;
        #[derive(Clone, Copy)]
        enum QuickWave<'a> {
            Wave(&'a Key, &'a table::Wave),
            Step(&'a fused::StepWave),
            StepParts(&'a Key, &'a table::Wave, u32),
        }
        impl<'a> QuickWave<'a> {
            #[inline]
            fn key(self) -> &'a Key {
                match self {
                    Self::Wave(key, _) | Self::StepParts(key, _, _) => key,
                    Self::Step(step) => &step.key,
                }
            }
            #[inline]
            fn is_step(self) -> bool {
                matches!(self, Self::Step(_) | Self::StepParts(..))
            }
            #[inline]
            fn sample(self, met: TimeSpan, part: TimeSpan, controls: &OrderedMap) -> Option<f64> {
                match self {
                    Self::Wave(_, wave) => wave.number(&met, &part, controls),
                    Self::Step(step) => step.number(part, controls),
                    Self::StepParts(_, wave, steps) => {
                        fused::StepWave::step_number(wave, steps, &part, controls)
                    }
                }
            }
        }
        let mut waves: [Option<QuickWave<'_>>; MAX_WAVES] = [None; MAX_WAVES];
        let mut count = 0usize;
        let mut base = self;
        loop {
            if count > 0 && (base.has_runtime_settings() || base.is_volatile()) {
                break;
            }
            match base.node_kind() {
                Node::AppWave(app) => {
                    let items = &*app.waves;
                    if count + items.len() > MAX_WAVES {
                        if count == 0 {
                            return false;
                        }
                        break;
                    }
                    let mut ok = true;
                    let saved = count;
                    for item in items.iter().rev() {
                        let Some(key) = controls::builtin_key_ref_by_id(item.key_id) else {
                            ok = false;
                            break;
                        };
                        if item.steps == 0 {
                            waves[count] = Some(QuickWave::Wave(key, &item.wave));
                        } else if fused::StepWave::step_covers_span(item.steps, &state.span) {
                            waves[count] = Some(QuickWave::StepParts(key, &item.wave, item.steps));
                        } else {
                            ok = false;
                            break;
                        }
                        count += 1;
                    }
                    if !ok {
                        count = saved;
                        if count == 0 {
                            return false;
                        }
                        break;
                    }
                    base = &app.inner;
                }
                Node::AppLeft(_) => {
                    let Some(side) = ValueSide::of(base.node_kind()) else {
                        break;
                    };
                    if !matches!(side.apply, Combine::Composed(compose::ComposeOp::Set))
                        || side.flow != LookupFlow::Set
                        || !side.values.tables_may_answer()
                    {
                        break;
                    }
                    let Some(ReadValues::Signal(signal)) = side.reading.of(side.values, state)
                    else {
                        break;
                    };
                    let found = if let Some((key, wave)) = signal.wave() {
                        Some(QuickWave::Wave(key, wave))
                    } else {
                        signal
                            .step_wave()
                            .filter(|step| step.covers_span(state.span))
                            .map(QuickWave::Step)
                    };
                    let Some(found) = found else {
                        break;
                    };
                    if count >= MAX_WAVES {
                        break;
                    }
                    waves[count] = Some(found);
                    count += 1;
                    base = side.functions;
                }
                _ => break,
            }
        }
        if count == 0 {
            return false;
        }
        let first = waves[0].expect("at least one wave");
        if base.is_volatile() {
            return false;
        }
        // Reverse so innermost wave applies first and outermost wins on same key.
        waves[..count].reverse();
        let has_step = waves[..count]
            .iter()
            .any(|w| w.is_some_and(QuickWave::is_step));
        let from = out.len();
        base.answer_child_onto(thread, state, out);
        if thread.stop_requested() {
            out.truncate(from);
            return true;
        }
        let emitted = out.len() - from;
        if emitted == 0 {
            return true;
        }
        let clear_live_keys = |hap: &mut Hap, waves: &[Option<QuickWave<'_>>]| {
            if !hap.keys.is_empty() {
                let k = hap.keys.get();
                let mut clear_slot = k.slider_binding != 0;
                if !clear_slot {
                    for w in waves {
                        if let Some(s) = controls::live_slot(w.expect("wave").key().as_str())
                            && k.live_controls[s] != 0
                        {
                            clear_slot = true;
                            break;
                        }
                    }
                }
                if clear_slot {
                    let edit = hap.keys.edit();
                    for w in waves {
                        if let Some(s) = controls::live_slot(w.expect("wave").key().as_str()) {
                            edit.live_controls[s] = 0;
                        }
                    }
                    edit.slider_binding = 0;
                    hap.keys.trim();
                }
            }
        };
        // Fast single-hap path: skips initializing the 4-slot `SampleSlot`
        // stack cache on the common 1-hap window query.
        if emitted == 1 {
            let hap = &mut out[from];
            let met = hap.whole_or_part();
            let part = hap.part.intersection(&met);
            let ok = hap.pick_lookup().is_none()
                && hap.part.within_one_cycle()
                && hap.value.is_data(0)
                && matches!(hap.value, Value::Object(_))
                && if has_step {
                    hap.part.begin < hap.part.end && part == Some(hap.part)
                } else {
                    part.is_some()
                };
            let Some(part) = part.filter(|_| ok) else {
                out.truncate(from);
                return false;
            };
            let mut patches: [(&Key, f64); MAX_WAVES] = [(first.key(), 0.0); MAX_WAVES];
            for i in 0..count {
                let item = waves[i].expect("wave present");
                let Some(num) = item.sample(met, part, &state.controls) else {
                    out.truncate(from);
                    return false;
                };
                if item.is_step() {
                    fused::note(fused::Hit::StepOne);
                }
                patches[i] = (item.key(), num);
            }
            clear_live_keys(hap, &waves[..count]);
            if let Value::Object(controls) = &mut hap.value {
                if count == 1 {
                    controls.set_data_single_cached(patches[0].0, &Value::F64(patches[0].1), None);
                } else {
                    let mut dummy = value::SharedWaveCache::default();
                    controls.set_data_many_cached(&patches[..count], &mut dummy);
                }
            }
            hap.part = part;
            return true;
        }
        let can_apply_all = out[from..].iter_mut().all(|hap| {
            if hap.pick_lookup().is_some()
                || !hap.part.within_one_cycle()
                || !hap.value.is_data(0)
                || !matches!(hap.value, Value::Object(_))
            {
                return false;
            }
            let part = match hap.whole {
                None => (hap.part.begin <= hap.part.end).then_some(hap.part),
                Some(whole) if whole == hap.part => {
                    (hap.part.begin <= hap.part.end).then_some(hap.part)
                }
                Some(whole) => hap.part.intersection(&whole),
            };
            let Some(part) = part else {
                return false;
            };
            if has_step && (hap.part.begin >= hap.part.end || part != hap.part) {
                return false;
            }
            hap.part = part;
            true
        });
        if !can_apply_all {
            out.truncate(from);
            return false;
        }
        // Fast batched StepPatch path: when 1..=4 unique wave keys apply to
        // multiple haps sharing a base across 2+ distinct timestamps (even when
        // bases are interleaved across voices), pack up to `16 / n_unique`
        // timestamps per base into one `Arc<StepPatch>`.
        //
        // Uses compact `u8` index arrays on the stack (referencing haps in `out`)
        // to avoid zeroing multi-KB `TimeSpan` buffers per base.
        struct SampleSlot {
            met: TimeSpan,
            part: TimeSpan,
            nums: [f64; MAX_WAVES],
            cache: value::SharedWaveCache,
        }
        let apply_fallback_slice = |out: &mut [Hap], mask: Option<u64>| -> bool {
            let mut samples: [Option<SampleSlot>; 4] = [None, None, None, None];
            let mut sample_next = 0u8;
            let mut patches: [(&Key, f64); MAX_WAVES] = [(first.key(), 0.0); MAX_WAVES];
            let mut apply_one = |hap: &mut Hap| -> bool {
                let met = hap.whole_or_part();
                let part = hap.part;
                let slot_idx = if let Some(idx) = samples
                    .iter()
                    .position(|s| matches!(s, Some(slot) if slot.met == met && slot.part == part))
                {
                    idx
                } else {
                    let mut nums = [0.0f64; MAX_WAVES];
                    for i in 0..count {
                        let Some(num) =
                            waves[i]
                                .expect("wave present")
                                .sample(met, part, &state.controls)
                        else {
                            return false;
                        };
                        nums[i] = num;
                    }
                    let idx = if let Some(empty) = samples.iter().position(Option::is_none) {
                        empty
                    } else {
                        let victim = usize::from(sample_next & 3);
                        sample_next = sample_next.wrapping_add(1);
                        victim
                    };
                    samples[idx] = Some(SampleSlot {
                        met,
                        part,
                        nums,
                        cache: value::SharedWaveCache::default(),
                    });
                    idx
                };
                let slot = samples[slot_idx].as_mut().expect("sample slot populated");
                for i in 0..count {
                    let item = waves[i].expect("wave present");
                    if item.is_step() {
                        fused::note(fused::Hit::StepOne);
                    }
                    patches[i] = (item.key(), slot.nums[i]);
                }
                clear_live_keys(hap, &waves[..count]);
                if let Value::Object(controls) = &mut hap.value {
                    controls.set_data_many_cached(&patches[..count], &mut slot.cache);
                }
                true
            };
            match mask {
                Some(mut rem) => {
                    while rem != 0 {
                        let j = rem.trailing_zeros() as usize;
                        rem &= rem - 1;
                        if !apply_one(&mut out[j]) {
                            return false;
                        }
                    }
                }
                None => {
                    for hap in out {
                        if !apply_one(hap) {
                            return false;
                        }
                    }
                }
            }
            true
        };
        if count <= value::StepPatch::MAX_KEYS {
            let mut unique_keys: [&Key; value::StepPatch::MAX_KEYS] =
                [first.key(); value::StepPatch::MAX_KEYS];
            let mut key_slot = [0u8; value::StepPatch::MAX_KEYS];
            let mut n_unique = 0usize;
            for i in 0..count {
                let key = waves[i].expect("wave present").key();
                if let Some(pos) = unique_keys[..n_unique].iter().position(|&k| k == key) {
                    key_slot[i] = pos as u8;
                } else {
                    unique_keys[n_unique] = key;
                    key_slot[i] = n_unique as u8;
                    n_unique += 1;
                }
            }
            let max_slots = value::StepPatch::MAX_VALUES / n_unique;
            if max_slots >= 2 {
                let total = out.len() - from;
                let mut chunk_base = 0usize;
                while chunk_base < total {
                    let chunk_len = (total - chunk_base).min(64);
                    let full_mask = if chunk_len == 64 {
                        u64::MAX
                    } else {
                        (1u64 << chunk_len) - 1
                    };
                    let mut done_mask = 0u64;
                    let mut skip_mask = 0u64;
                    // Reuse sampled wave values across distinct bases in the same chunk.
                    // Each entry stores the representative hap index in this chunk.
                    let mut shared_hap = [0u8; 16];
                    let mut shared_nums = [[0.0f64; value::StepPatch::MAX_KEYS]; 16];
                    let mut n_shared = 0usize;
                    let mut hap_idx = [0u8; 64];
                    let mut hap_rows = [0u32; 64];
                    let mut hap_layers = [0u16; 64];
                    let mut hap_slots = [0u8; 64];
                    let mut slot_hap = [0u8; 16];
                    let mut slot_nums = [[0.0f64; value::StepPatch::MAX_KEYS]; 16];
                    while (done_mask | skip_mask) != full_mask {
                        let start = (!(done_mask | skip_mask)).trailing_zeros() as usize;
                        let Some((base, first_row, first_layer, uniform)) =
                            (match &out[from + chunk_base + start].value {
                                Value::Object(m) => m.step_base(),
                                _ => None,
                            })
                        else {
                            skip_mask |= 1u64 << start;
                            continue;
                        };
                        let mut n_slots = 0usize;
                        let mut m_len = 0usize;
                        let handled = done_mask | skip_mask;
                        for j in start..chunk_len {
                            if handled & (1u64 << j) != 0 {
                                continue;
                            }
                            let hap = &out[from + chunk_base + j];
                            let (row, layer_idx) = if j == start {
                                (first_row, first_layer)
                            } else {
                                let Value::Object(m) = &hap.value else {
                                    continue;
                                };
                                let Some((r, l)) = m.matches_step_base(&base) else {
                                    continue;
                                };
                                if !uniform && r != first_row {
                                    continue;
                                }
                                (r, l)
                            };
                            let met = hap.whole_or_part();
                            let part = hap.part;
                            let s_idx = if let Some(existing) =
                                slot_hap[..n_slots].iter().position(|&rep| {
                                    let rh = &out[from + chunk_base + usize::from(rep)];
                                    rh.whole_or_part() == met && rh.part == part
                                }) {
                                existing
                            } else if n_slots < max_slots {
                                let vals = if let Some(cached) =
                                    shared_hap[..n_shared].iter().position(|&rep| {
                                        let rh = &out[from + chunk_base + usize::from(rep)];
                                        rh.whole_or_part() == met && rh.part == part
                                    }) {
                                    shared_nums[cached]
                                } else {
                                    let mut vals = [0.0f64; value::StepPatch::MAX_KEYS];
                                    for i in 0..count {
                                        let Some(num) = waves[i].expect("wave present").sample(
                                            met,
                                            part,
                                            &state.controls,
                                        ) else {
                                            out.truncate(from);
                                            return false;
                                        };
                                        vals[usize::from(key_slot[i])] = num;
                                    }
                                    if n_shared < 16 {
                                        shared_hap[n_shared] = j as u8;
                                        shared_nums[n_shared] = vals;
                                        n_shared += 1;
                                    }
                                    vals
                                };
                                let idx = n_slots;
                                slot_hap[idx] = j as u8;
                                slot_nums[idx] = vals;
                                n_slots += 1;
                                idx
                            } else {
                                continue;
                            };
                            hap_idx[m_len] = j as u8;
                            hap_rows[m_len] = row;
                            hap_layers[m_len] = layer_idx;
                            hap_slots[m_len] = s_idx as u8;
                            m_len += 1;
                        }
                        if n_slots == 1 {
                            if let Some(patched) = value::Patched::new_single_slot(
                                base,
                                first_row,
                                first_layer,
                                &unique_keys[..n_unique],
                                &slot_nums[0][..n_unique],
                            ) {
                                for k in 0..m_len {
                                    let j = usize::from(hap_idx[k]);
                                    let hap = &mut out[from + chunk_base + j];
                                    for wave in &waves[..count] {
                                        if wave.expect("wave present").is_step() {
                                            fused::note(fused::Hit::StepOne);
                                        }
                                    }
                                    clear_live_keys(hap, &waves[..count]);
                                    if let Value::Object(controls) = &mut hap.value {
                                        controls.set_patched(&patched, hap_rows[k], hap_layers[k]);
                                    }
                                    done_mask |= 1u64 << j;
                                }
                            } else {
                                // `base` was `PatchedBase::Patched` and is now dropped so unique
                                // `Arc::get_mut` succeeds when `m_len == 1`. Apply the already-
                                // sampled values directly without re-sampling waves.
                                let mut patches: [(&Key, f64); value::StepPatch::MAX_KEYS] =
                                    [(first.key(), 0.0); value::StepPatch::MAX_KEYS];
                                for i in 0..n_unique {
                                    patches[i] = (unique_keys[i], slot_nums[0][i]);
                                }
                                let mut local_cache = value::SharedWaveCache::default();
                                for &idx in &hap_idx[..m_len] {
                                    let j = usize::from(idx);
                                    let hap = &mut out[from + chunk_base + j];
                                    for wave in &waves[..count] {
                                        if wave.expect("wave present").is_step() {
                                            fused::note(fused::Hit::StepOne);
                                        }
                                    }
                                    clear_live_keys(hap, &waves[..count]);
                                    if let Value::Object(controls) = &mut hap.value {
                                        controls.set_data_many_cached(
                                            &patches[..n_unique],
                                            &mut local_cache,
                                        );
                                    }
                                    done_mask |= 1u64 << j;
                                }
                            }
                            continue;
                        }
                        if n_slots < 2 {
                            for &j in &hap_idx[..m_len] {
                                skip_mask |= 1u64 << j;
                            }
                            continue;
                        }
                        let Some(mut built) = value::StepPatch::new_for_row(
                            base,
                            first_row,
                            first_layer,
                            &unique_keys[..n_unique],
                            true,
                            n_slots * n_unique,
                        ) else {
                            for &j in &hap_idx[..m_len] {
                                skip_mask |= 1u64 << j;
                            }
                            continue;
                        };
                        for (s, nums) in slot_nums[..n_slots].iter().enumerate() {
                            built.set_slot(s, &nums[..n_unique]);
                        }
                        let sp = built.finish();
                        for k in 0..m_len {
                            let j = usize::from(hap_idx[k]);
                            let hap = &mut out[from + chunk_base + j];
                            for wave in &waves[..count] {
                                if wave.expect("wave present").is_step() {
                                    fused::note(fused::Hit::StepOne);
                                }
                            }
                            clear_live_keys(hap, &waves[..count]);
                            if let Value::Object(controls) = &mut hap.value {
                                controls.set_step_patch(
                                    &sp,
                                    hap_rows[k],
                                    hap_layers[k],
                                    usize::from(hap_slots[k]),
                                );
                            }
                            done_mask |= 1u64 << j;
                        }
                    }
                    if skip_mask != 0
                        && !apply_fallback_slice(
                            &mut out[from + chunk_base..from + chunk_base + chunk_len],
                            Some(skip_mask),
                        )
                    {
                        out.truncate(from);
                        return false;
                    }
                    chunk_base += chunk_len;
                }
                return true;
            }
        }
        if !apply_fallback_slice(&mut out[from..], None) {
            out.truncate(from);
            return false;
        }
        true
    }

    /// Fast single-pass segmented wave application when `2..=4` waves share one
    /// multi-step rate (`shared_steps`) and every hap splits into `1..=8` steps.
    /// Kept out-of-line and non-generic over `haps` so both small-stack and
    /// buffered callers execute one shared compiled body.
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn try_query_app_wave_uniform_steps_onto(
        items: &[AppWaveItem],
        shared_steps: u32,
        has_continuous: bool,
        single_step_mask: u8,
        thread: &QueryThread,
        state: &State,
        from: usize,
        haps: &mut dyn Iterator<Item = Hap>,
        out: &mut Vec<Hap>,
    ) -> Option<bool> {
        let first_key = controls::builtin_key_ref_by_id(items.first()?.key_id)?;
        let n_waves = items.len();
        let mut keys: [&Key; 4] = [first_key; 4];
        let mut unique_keys: [&Key; value::StepPatch::MAX_KEYS] =
            [first_key; value::StepPatch::MAX_KEYS];
        let mut unique_key_ids = [0u16; value::StepPatch::MAX_KEYS];
        let mut key_slot = [0u8; 4];
        let mut n_unique = 0usize;
        let mut hoisted_mask = 0u8;
        let mut wave_live_mask = 0u8;
        for (w, item) in items.iter().enumerate() {
            let key = if w == 0 {
                first_key
            } else {
                controls::builtin_key_ref_by_id(item.key_id)?
            };
            keys[w] = key;
            if let Some(pos) = unique_key_ids[..n_unique]
                .iter()
                .position(|&id| id == item.key_id)
            {
                key_slot[w] = pos as u8;
            } else {
                unique_keys[n_unique] = key;
                unique_key_ids[n_unique] = item.key_id;
                key_slot[w] = n_unique as u8;
                n_unique += 1;
            }
            if item.steps == 0 && item.wave.has_no_cuts() {
                hoisted_mask |= 1u8 << w;
            }
            if let Some(s) = controls::live_slot(key.as_str()) {
                wave_live_mask |= 1u8 << s;
            }
        }
        struct MultiStepSlot {
            active: bool,
            met: TimeSpan,
            part: TimeSpan,
            count: u8,
            bounds: [Fraction; 9],
            nums: [[f64; 4]; 8],
            step_patches: [Option<(value::PatchedBase, Option<u32>, value::StepPatch)>; 4],
            step_next: u8,
            fallback_caches: Option<Box<[value::SharedWaveCache; 8]>>,
        }
        impl MultiStepSlot {
            fn empty(zero: TimeSpan) -> Self {
                Self {
                    active: false,
                    met: zero,
                    part: zero,
                    count: 0,
                    bounds: [Fraction::ZERO; 9],
                    nums: [[0.0f64; 4]; 8],
                    step_patches: [None, None, None, None],
                    step_next: 0,
                    fallback_caches: None,
                }
            }
        }
        let zero_span = TimeSpan::new(Fraction::ZERO, Fraction::ZERO);
        let mut slots = [
            MultiStepSlot::empty(zero_span),
            MultiStepSlot::empty(zero_span),
        ];
        let mut slot_next = 0u8;
        #[cold]
        #[inline(never)]
        fn emit_fallback(
            slot: &mut MultiStepSlot,
            mut template: Hap,
            keys: &[&Key; 4],
            n_waves: usize,
            thread: &QueryThread,
            out: &mut Vec<Hap>,
            state_flags: (usize, bool),
        ) -> bool {
            let (from, bulk_ok) = state_flags;
            let count = usize::from(slot.count);
            let mut patches: [(&Key, f64); 4] = [(keys[0], 0.0); 4];
            let caches = slot.fallback_caches.get_or_insert_with(|| {
                Box::new(std::array::from_fn(|_| value::SharedWaveCache::default()))
            });
            for s_idx in 0..count {
                let mut hap = if s_idx + 1 == count {
                    std::mem::replace(
                        &mut template,
                        Hap::new(
                            None,
                            TimeSpan::new(Fraction::ZERO, Fraction::ZERO),
                            Value::Undefined,
                        ),
                    )
                } else {
                    template.clone()
                };
                for w in 0..n_waves {
                    patches[w] = (keys[w], slot.nums[s_idx][w]);
                }
                if let Value::Object(controls) = &mut hap.value {
                    controls.set_data_many_cached(&patches[..n_waves], &mut caches[s_idx]);
                }
                hap.part = TimeSpan::new(slot.bounds[s_idx], slot.bounds[s_idx + 1]);
                if bulk_ok {
                    out.push(hap);
                } else if thread.stop_requested() || !thread.push_budgeted(out, from, hap) {
                    out.truncate(from);
                    return false;
                }
            }
            if bulk_ok {
                thread.note_accumulated(out.len() - from);
            }
            fused::note(fused::Hit::StepMany);
            true
        }
        for hf in haps {
            let met = if has_continuous {
                hf.whole_or_part()
            } else {
                hf.part
            };
            let slot_idx = if let Some(idx) = slots
                .iter()
                .position(|s| s.active && s.part == hf.part && (!has_continuous || s.met == met))
            {
                idx
            } else {
                let grid = fused::StepWave::step_grid_over(shared_steps, &hf.part)?;
                let count = grid.len();
                let mut hoisted_vals = [0.0f64; 4];
                if hoisted_mask != 0 {
                    for (w, item) in items.iter().enumerate() {
                        if hoisted_mask & (1u8 << w) != 0 {
                            hoisted_vals[w] = item.wave.number(&met, &hf.part, &state.controls)?;
                        }
                    }
                }
                let target_idx = if !slots[0].active {
                    0
                } else if !slots[1].active {
                    1
                } else {
                    let victim = usize::from(slot_next & 1);
                    slot_next = slot_next.wrapping_add(1);
                    victim
                };
                let slot = &mut slots[target_idx];
                let was_active = slot.active;
                slot.active = false;
                let grid_steps = grid.steps();
                let grid_start = grid_steps.start;
                let grid_end = grid_steps.end;
                let mut begin = grid.time(grid_start)?;
                if thread.stop_requested() {
                    out.truncate(from);
                    return Some(true);
                }
                let mut piece_begin = if begin < hf.part.begin {
                    hf.part.begin
                } else {
                    begin
                };
                slot.bounds[0] = piece_begin;
                for (s_idx, k) in grid_steps.enumerate() {
                    let end = grid.time(k + 1)?;
                    let piece_end = if k + 1 == grid_end && end > hf.part.end {
                        hf.part.end
                    } else {
                        end
                    };
                    slot.bounds[s_idx + 1] = piece_end;
                    let piece = TimeSpan::new(piece_begin, piece_end);
                    for (w, item) in items.iter().enumerate() {
                        slot.nums[s_idx][w] = if hoisted_mask & (1u8 << w) != 0 {
                            hoisted_vals[w]
                        } else if item.steps == 0 {
                            item.wave.number(&met, &piece, &state.controls)?
                        } else if single_step_mask & (1u8 << w) != 0 {
                            fused::StepWave::step_number(
                                &item.wave,
                                item.steps,
                                &piece,
                                &state.controls,
                            )?
                        } else {
                            item.wave.step_value_at(&begin, &state.controls)?
                        };
                    }
                    begin = end;
                    piece_begin = end;
                }
                slot.met = met;
                slot.part = hf.part;
                slot.count = count as u8;
                if was_active {
                    slot.step_patches = [None, None, None, None];
                    slot.step_next = 0;
                    slot.fallback_caches = None;
                }
                slot.active = true;
                target_idx
            };
            let slot = &mut slots[slot_idx];
            let count = usize::from(slot.count);
            out.reserve(count);
            let bulk_ok = thread.charge(out.len() - from + count);
            let mut template = hf;
            if !template.keys.is_empty() {
                let k = template.keys.get();
                let mut clear_slot = k.slider_binding != 0;
                if !clear_slot && wave_live_mask != 0 {
                    let mut m = wave_live_mask;
                    while m != 0 {
                        let s = m.trailing_zeros() as usize;
                        m &= m - 1;
                        if k.live_controls[s] != 0 {
                            clear_slot = true;
                            break;
                        }
                    }
                }
                if clear_slot {
                    let edit = template.keys.edit();
                    let mut m = wave_live_mask;
                    while m != 0 {
                        let s = m.trailing_zeros() as usize;
                        m &= m - 1;
                        edit.live_controls[s] = 0;
                    }
                    edit.slider_binding = 0;
                    template.keys.trim();
                }
            }
            // Single-allocation StepPatch fast path across all `count` steps.
            if count * n_unique <= value::StepPatch::MAX_VALUES
                && let Value::Object(map) = &template.value
            {
                let mut hit: Option<(value::StepPatch, u32, u16)> = None;
                for (b, fixed_row, cached) in slot.step_patches.iter().flatten() {
                    if let Some((row, layer_idx)) = map.matches_step_base(b)
                        && fixed_row.is_none_or(|r| r == row)
                    {
                        hit = Some((cached.clone(), row, layer_idx));
                        break;
                    }
                }
                let resolved = if let Some(found) = hit {
                    Some(found)
                } else if let Some((base, row, layer_idx, uniform)) = map.step_base() {
                    let fixed_row = (!uniform).then_some(row);
                    if let Some(mut built) = value::StepPatch::new_for_row_ids(
                        base.clone(),
                        row,
                        layer_idx,
                        &unique_key_ids[..n_unique],
                        &unique_keys[..n_unique],
                        true,
                        count * n_unique,
                    ) {
                        let dst = built.values_mut();
                        for (s_idx, row_nums) in slot.nums.iter().enumerate().take(count) {
                            let offset = s_idx * n_unique;
                            for w in 0..n_waves {
                                dst[offset + usize::from(key_slot[w])] = Value::F64(row_nums[w]);
                            }
                        }
                        let sp = built.finish();
                        let idx = if let Some(empty) =
                            slot.step_patches.iter().position(Option::is_none)
                        {
                            empty
                        } else {
                            let v = usize::from(slot.step_next & 3);
                            slot.step_next = slot.step_next.wrapping_add(1);
                            v
                        };
                        slot.step_patches[idx] = Some((base, fixed_row, sp.clone()));
                        Some((sp, row, layer_idx))
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Some((sp, row, layer_idx)) = resolved {
                    let whole = template.whole;
                    let last = count - 1;
                    for s_idx in 0..count {
                        let piece = TimeSpan::new(slot.bounds[s_idx], slot.bounds[s_idx + 1]);
                        let (context, keys) = if s_idx == last {
                            (
                                std::mem::take(&mut template.context),
                                std::mem::take(&mut template.keys),
                            )
                        } else {
                            (template.context.clone(), template.keys.clone())
                        };
                        let hap = Hap {
                            whole,
                            part: piece,
                            value: Value::Object(OrderedMap::from_step_patch(
                                &sp, row, layer_idx, s_idx,
                            )),
                            context,
                            keys,
                        };
                        if bulk_ok {
                            out.push(hap);
                        } else if thread.stop_requested() || !thread.push_budgeted(out, from, hap) {
                            out.truncate(from);
                            return Some(true);
                        }
                    }
                    if bulk_ok {
                        thread.note_accumulated(out.len() - from);
                    }
                    fused::note(fused::Hit::StepMany);
                    continue;
                }
            }
            if !emit_fallback(slot, template, &keys, n_waves, thread, out, (from, bulk_ok)) {
                return Some(true);
            }
        }
        Some(true)
    }

    /// Direct execution for [`Node::AppWave`] when one or more items are
    /// multi-step segmented waves (`steps > 0`) spanning multiple steps of
    /// `state.span`: avoids rebuilding `AppLeft` + `OnceLock` graphs and uses
    /// stack buffers for `count <= 8` window steps.
    /// Kept out-of-line (and adjacent to `try_query_app_wave_uniform_steps_onto`)
    /// so `answer_onto` stays compact and multi-step wave helpers share warm
    /// code pages.
    #[inline(never)]
    pub(crate) fn try_query_app_wave_steps_onto(
        &self,
        app: &AppWaveNode,
        thread: &QueryThread,
        state: &State,
        out: &mut Vec<Hap>,
    ) -> bool {
        if app.inner.is_volatile() {
            return false;
        }
        let from = out.len();
        let items = &*app.waves;
        app.inner.answer_child_onto(thread, state, out);
        if thread.stop_requested() {
            out.truncate(from);
            return true;
        }
        let inner_len = out.len() - from;
        if inner_len == 0 {
            return true;
        }
        let apply = Combine::Composed(compose::ComposeOp::Set);
        let mut shared_steps = 0u32;
        let mut uniform_steps = items.len() <= 4;
        let mut has_continuous = false;
        let mut single_step_mask = 0u8;
        for (idx, item) in items.iter().enumerate() {
            if item.steps == 0 {
                has_continuous = true;
            } else if fused::StepWave::step_covers_span(item.steps, &state.span) {
                single_step_mask |= 1u8 << idx;
            } else if shared_steps == 0 {
                shared_steps = item.steps;
            } else if shared_steps != item.steps {
                uniform_steps = false;
                break;
            }
        }
        let min_uniform_steps = if items.len() == 1 { 2 } else { 1 };
        let use_uniform = uniform_steps
            && shared_steps > 0
            && !items.is_empty()
            && out[from..].iter().all(|hf| {
                fused::StepWave::takes(hf)
                    && (!has_continuous || (hf.whole.is_some() && hf.part.within_one_cycle()))
                    && fused::StepWave::step_grid_over(shared_steps, &hf.part)
                        .is_some_and(|g| (min_uniform_steps..=8).contains(&g.len()))
            });
        if inner_len <= 4 {
            if use_uniform {
                let mut stack_haps: [Option<Hap>; 4] = [None, None, None, None];
                for (slot, hf) in stack_haps.iter_mut().zip(out.drain(from..)) {
                    *slot = Some(hf);
                }
                let mut iter = stack_haps[..inner_len].iter_mut().filter_map(Option::take);
                let ok = Self::try_query_app_wave_uniform_steps_onto(
                    items,
                    shared_steps,
                    has_continuous,
                    single_step_mask,
                    thread,
                    state,
                    from,
                    &mut iter,
                    out,
                );
                if ok.is_none() {
                    out.truncate(from);
                    return false;
                }
                return true;
            }
            if let [item] = items
                && item.steps > 0
                && let Some(key) = controls::builtin_key_ref_by_id(item.key_id)
                && out[from..].iter().all(|hf| {
                    fused::StepWave::takes(hf)
                        && fused::StepWave::step_grid_over(item.steps, &hf.part)
                            .is_some_and(|g| g.len() <= 8)
                })
            {
                let mut stack_haps: [Option<Hap>; 4] = [None, None, None, None];
                for (slot, hf) in stack_haps.iter_mut().zip(out.drain(from..)) {
                    *slot = Some(hf);
                }
                let mut one_cache = value::SharedWaveCache::default();
                let mut one_last: Option<(TimeSpan, f64)> = None;
                let mut small_cache: [Option<SmallStepSlot>; 4] = [None, None, None, None];
                let mut small_next = 0u8;
                let mut ok = true;
                for mut hf in stack_haps[..inner_len].iter_mut().filter_map(Option::take) {
                    let cached_one = match one_last {
                        Some((lp, ln)) if lp == hf.part => Some(ln),
                        _ => fused::StepWave::step_number(
                            &item.wave,
                            item.steps,
                            &hf.part,
                            &state.controls,
                        )
                        .inspect(|&n| {
                            one_last = Some((hf.part, n));
                            one_cache = value::SharedWaveCache::default();
                        }),
                    };
                    if let Some(num) = cached_one {
                        let part = hf.part;
                        if !Meeting::set_number_in_place_cached(
                            &mut hf,
                            key,
                            num,
                            part,
                            &apply,
                            LookupFlow::Set,
                            Some(&mut one_cache),
                        ) {
                            ok = false;
                            break;
                        }
                        if thread.stop_requested() || !thread.push_budgeted(out, from, hf) {
                            out.truncate(from);
                            return true;
                        }
                        fused::note(fused::Hit::StepOne);
                        continue;
                    }
                    let Some(grid) = fused::StepWave::step_grid_over(item.steps, &hf.part) else {
                        ok = false;
                        break;
                    };
                    let count = grid.len();
                    if count == 0 {
                        continue;
                    }
                    if count == 1 {
                        let k = grid.steps().start;
                        let (Some(begin), Some(end)) = (grid.time(k), grid.time(k + 1)) else {
                            ok = false;
                            break;
                        };
                        let piece = grid.cut(k, TimeSpan::new(begin, end), hf.part);
                        let Some(num) = item.wave.step_value_at(&begin, &state.controls) else {
                            ok = false;
                            break;
                        };
                        if one_last.is_none_or(|(lp, _)| lp != hf.part) {
                            one_last = Some((hf.part, num));
                            one_cache = value::SharedWaveCache::default();
                        }
                        if !Meeting::set_number_in_place_cached(
                            &mut hf,
                            key,
                            num,
                            piece,
                            &apply,
                            LookupFlow::Set,
                            Some(&mut one_cache),
                        ) {
                            ok = false;
                            break;
                        }
                        if thread.stop_requested() || !thread.push_budgeted(out, from, hf) {
                            out.truncate(from);
                            return true;
                        }
                        fused::note(fused::Hit::StepMany);
                        continue;
                    }
                    match meet_steps_small(
                        thread,
                        hf,
                        key,
                        &item.wave,
                        &grid,
                        &mut small_cache,
                        &mut small_next,
                        state,
                        from,
                        out,
                    ) {
                        Some(true) => {}
                        Some(false) => {
                            out.truncate(from);
                            return true;
                        }
                        None => {
                            ok = false;
                            break;
                        }
                    }
                }
                if !ok {
                    out.truncate(from);
                    return false;
                }
                return true;
            }
        }
        let ok = with_list(|buf| {
            buf.extend(out.drain(from..));
            if use_uniform {
                let mut iter = buf.drain(..);
                return Self::try_query_app_wave_uniform_steps_onto(
                    items,
                    shared_steps,
                    has_continuous,
                    single_step_mask,
                    thread,
                    state,
                    from,
                    &mut iter,
                    out,
                );
            }
            for (item_idx, item) in items.iter().enumerate() {
                let key = controls::builtin_key_ref_by_id(item.key_id)?;
                let is_last = item_idx + 1 == items.len();
                if item.steps == 0 {
                    let mut wave_cache = value::SharedWaveCache::default();
                    let mut last_span: Option<(TimeSpan, TimeSpan, f64)> = None;
                    for hap in buf.iter_mut() {
                        if hap.pick_lookup().is_some()
                            || !hap.part.within_one_cycle()
                            || !hap.value.is_data(0)
                            || !matches!(hap.value, Value::Object(_))
                        {
                            return None;
                        }
                        let met = hap.whole_or_part();
                        let part = hap.part.intersection(&met)?;
                        let num = match last_span {
                            Some((lm, lp, ln)) if lm == met && lp == part => ln,
                            _ => {
                                let n = item.wave.number(&met, &part, &state.controls)?;
                                last_span = Some((met, part, n));
                                wave_cache = value::SharedWaveCache::default();
                                n
                            }
                        };
                        if !Meeting::set_number_in_place_cached(
                            hap,
                            key,
                            num,
                            part,
                            &apply,
                            LookupFlow::Set,
                            Some(&mut wave_cache),
                        ) {
                            return None;
                        }
                    }
                    if is_last {
                        out.append(buf);
                    }
                    continue;
                }
                // Segmented step wave (`item.steps > 0`).
                let valid = buf.iter().all(fused::StepWave::takes);
                if !valid {
                    return None;
                }
                let apply_into = |src: &mut Vec<Hap>,
                                  dst: &mut Vec<Hap>,
                                  dst_from: usize|
                 -> Option<bool> {
                    let mut one_cache = value::SharedWaveCache::default();
                    let mut one_last: Option<(TimeSpan, f64)> = None;
                    let mut small_cache: [Option<SmallStepSlot>; 4] = [None, None, None, None];
                    let mut small_next = 0u8;
                    let mut large_cache = StepWaveCacheSet::default();
                    for mut hf in src.drain(..) {
                        let cached_one = match one_last {
                            Some((lp, ln)) if lp == hf.part => Some(ln),
                            _ => fused::StepWave::step_number(
                                &item.wave,
                                item.steps,
                                &hf.part,
                                &state.controls,
                            )
                            .inspect(|&n| {
                                one_last = Some((hf.part, n));
                                one_cache = value::SharedWaveCache::default();
                            }),
                        };
                        if let Some(num) = cached_one {
                            let part = hf.part;
                            if !Meeting::set_number_in_place_cached(
                                &mut hf,
                                key,
                                num,
                                part,
                                &apply,
                                LookupFlow::Set,
                                Some(&mut one_cache),
                            ) {
                                return None;
                            }
                            if thread.stop_requested() || !thread.push_budgeted(dst, dst_from, hf) {
                                return Some(false);
                            }
                            fused::note(fused::Hit::StepOne);
                            continue;
                        }
                        let grid = fused::StepWave::step_grid_over(item.steps, &hf.part)?;
                        let count = grid.len();
                        if count == 0 {
                            continue;
                        }
                        if count == 1 {
                            let k = grid.steps().start;
                            let begin = grid.time(k)?;
                            let end = grid.time(k + 1)?;
                            let piece = grid.cut(k, TimeSpan::new(begin, end), hf.part);
                            let num = item.wave.step_value_at(&begin, &state.controls)?;
                            if one_last.is_none_or(|(lp, _)| lp != hf.part) {
                                one_last = Some((hf.part, num));
                                one_cache = value::SharedWaveCache::default();
                            }
                            if !Meeting::set_number_in_place_cached(
                                &mut hf,
                                key,
                                num,
                                piece,
                                &apply,
                                LookupFlow::Set,
                                Some(&mut one_cache),
                            ) {
                                return None;
                            }
                            if thread.stop_requested() || !thread.push_budgeted(dst, dst_from, hf) {
                                return Some(false);
                            }
                            fused::note(fused::Hit::StepMany);
                            continue;
                        }
                        if count <= 8 {
                            if !meet_steps_small(
                                thread,
                                hf,
                                key,
                                &item.wave,
                                &grid,
                                &mut small_cache,
                                &mut small_next,
                                state,
                                dst_from,
                                dst,
                            )? {
                                return Some(false);
                            }
                            continue;
                        }
                        // Large multi-cycle step count (> 8): share one `Table` across steps.
                        let Value::Object(base_map) = &hf.value else {
                            return None;
                        };
                        let cached = if large_cache.find(hf.part).is_some() {
                            large_cache.find(hf.part).expect("checked present")
                        } else {
                            let mut pieces = large_cache.take_victim_pieces(count);
                            let mut begin = grid.time(grid.steps().start)?;
                            let mut vals = Vec::with_capacity(count);
                            for k in grid.steps() {
                                if thread.stop_requested() {
                                    return Some(false);
                                }
                                let end = grid.time(k + 1)?;
                                pieces.push(grid.cut(k, TimeSpan::new(begin, end), hf.part));
                                vals.push(Value::F64(
                                    item.wave.step_value_at(&begin, &state.controls)?,
                                ));
                                begin = end;
                            }
                            large_cache.insert(StepWaveCache {
                                part: hf.part,
                                values: Arc::from(vals),
                                pieces,
                                table: None,
                            })
                        };
                        let shared_patched =
                            if let Some((base_source, base_row)) = base_map.dressed_base() {
                                if let Some((cached_base, cached_patched)) = &cached.table
                                    && cached_base.same_source(&base_source)
                                {
                                    Some((Arc::clone(cached_patched), base_row))
                                } else if let Some(patched) = value::Table::patched_keyed_table(
                                    &base_source,
                                    key.clone(),
                                    Arc::clone(&cached.values),
                                ) {
                                    let patched = Arc::new(patched);
                                    cached.table = Some((base_source, Arc::clone(&patched)));
                                    Some((patched, base_row))
                                } else {
                                    None
                                }
                            } else {
                                None
                            };
                        let fallback_table = if shared_patched.is_none() {
                            Some(Arc::new(value::Table::patched(
                                base_map,
                                key.clone(),
                                Arc::clone(&cached.values),
                            )?))
                        } else {
                            None
                        };
                        let mut template = hf.clone();
                        template.value = Value::Undefined;
                        if !template.keys.is_empty() {
                            let slot = controls::live_slot(key.as_str());
                            let k = template.keys.get();
                            if k.slider_binding != 0
                                || slot.is_some_and(|s| k.live_controls[s] != 0)
                            {
                                let edit = template.keys.edit();
                                if let Some(slot) = slot {
                                    edit.live_controls[slot] = 0;
                                }
                                edit.slider_binding = 0;
                                template.keys.trim();
                            }
                        }
                        let step_count = cached.pieces.len();
                        dst.reserve(step_count);
                        let bulk_ok = thread.charge(dst.len() - dst_from + step_count);
                        for (index, &piece) in cached.pieces.iter().enumerate() {
                            let map = match &shared_patched {
                                Some((table, base_row)) => table.map_packed(*base_row, index),
                                None => fallback_table
                                    .as_ref()
                                    .expect("fallback table")
                                    .map(index)?,
                            };
                            let mut hap = template.clone();
                            hap.value = Value::Object(map);
                            hap.part = piece;
                            if bulk_ok {
                                dst.push(hap);
                            } else if thread.stop_requested()
                                || !thread.push_budgeted(dst, dst_from, hap)
                            {
                                return Some(false);
                            }
                        }
                        if bulk_ok {
                            thread.note_accumulated(dst.len() - dst_from);
                        }
                        fused::note(fused::Hit::StepMany);
                    }
                    Some(true)
                };
                if is_last {
                    if !apply_into(buf, out, from)? {
                        out.truncate(from);
                        return Some(true);
                    }
                } else {
                    let cont = with_list(|next| {
                        let res = apply_into(buf, next, 0)?;
                        if res {
                            std::mem::swap(buf, next);
                        }
                        Some(res)
                    })?;
                    if !cont {
                        out.truncate(from);
                        return Some(true);
                    }
                }
            }
            Some(true)
        });
        match ok {
            Some(_) => true,
            None => {
                out.truncate(from);
                false
            }
        }
    }

    /// Fast in-place path for a degrade gate `AppLeft` node.
    pub(crate) fn try_query_gate_onto(
        &self,
        thread: &QueryThread,
        side: ValueSide<'_>,
        state: &State,
        out: &mut Vec<Hap>,
    ) -> bool {
        let keeps = side.flow == LookupFlow::Left
            && matches!(side.apply, Combine::With(combine) if combinators::keeps_left(combine));
        if !keeps || !side.values.tables_may_answer() || side.functions.is_volatile() {
            return false;
        }
        let Some(ReadValues::Signal(signal)) = side.reading.of(side.values, state) else {
            return false;
        };
        let Some(gate) = signal.gate() else {
            return false;
        };
        let from = out.len();
        side.functions.answer_child_onto(thread, state, out);
        if thread.stop_requested() {
            out.truncate(from);
            return true;
        }
        if out.len() == from {
            return true;
        }
        if !out[from..]
            .iter()
            .all(|hap| hap.pick_lookup().is_none() && hap.part.within_one_cycle())
        {
            out.truncate(from);
            return false;
        }
        let mut bailed = false;
        retain_from(out, from, |hap| {
            if bailed {
                return false;
            }
            let met = hap.whole_or_part();
            let Some(part) = hap.part.intersection(&met) else {
                return false;
            };
            match gate.passes(met, part, &state.controls) {
                Some(true) => {
                    hap.part = part;
                    true
                }
                Some(false) => false,
                None => {
                    bailed = true;
                    false
                }
            }
        });
        if bailed {
            out.truncate(from);
            return false;
        }
        true
    }

    /// Whether the graph is one of discrete haps: it answers for a part of
    /// a span as for the span, cut and in the same order. One query over a
    /// span then holds the answer for each part of the span.
    pub(crate) fn is_discrete(&self) -> bool {
        self.is_cacheable() && self.span_read() == purity::SpanRead::No
    }

    /// The span to ask this side of an application over, for a hap of the
    /// other side that sounds over `whole` and is cut to `part`: what the
    /// two haps make is cut to `part` too. A graph of
    /// [discrete](Self::is_discrete) haps answers for the part as for the
    /// whole, cut, so it is asked over the part alone. Any other graph is
    /// asked over the whole, and so is each graph for a part of no width.
    fn asked_for(&self, whole: TimeSpan, part: TimeSpan) -> TimeSpan {
        match part != whole && part.begin < part.end && self.is_discrete() {
            true => part,
            false => whole,
        }
    }

    /// Append the haps of this [discrete](Self::is_discrete) pattern for each of
    /// `parts` directly onto `out`, from one query over all of them: the haps of
    /// each part in turn, each cut to its part, as a query for each part answers.
    /// The parts follow one another with no time between them.
    ///
    /// Returns `false` (leaving `out` untouched) for an answer with a hap of no
    /// width, which a part that begins at its time and a part that ends there
    /// read differently.
    pub(crate) fn asked_for_parts_onto(
        &self,
        thread: &QueryThread,
        parts: &[TimeSpan],
        state: &State,
        out: &mut Vec<Hap>,
    ) -> bool {
        thread_local! {
            static SPARE_MET: std::cell::RefCell<Vec<(usize, usize, bool)>> =
                const { std::cell::RefCell::new(Vec::new()) };
            static SPARE_SLOTS: std::cell::RefCell<Vec<Option<Hap>>> =
                const { std::cell::RefCell::new(Vec::new()) };
        }
        let (Some(first_part), Some(last_part)) = (parts.first(), parts.last()) else {
            return true;
        };
        let all = TimeSpan::new(first_part.begin, last_part.end);
        with_list(|haps| {
            let asked = state.set_span(all);
            self.answer_child_onto(thread, &asked, haps);
            if haps.iter().any(|hap| hap.part.begin >= hap.part.end) {
                return false;
            }
            // Count the split pieces first and charge any active hap budget
            // before allocating `met` or reserving `out`.
            let mut total_met = 0usize;
            for (index, hap) in haps.iter().enumerate() {
                if (index & 63) == 0 && thread.stop_requested() {
                    return true;
                }
                let first = parts.partition_point(|part| part.end <= hap.part.begin);
                let after = parts.partition_point(|part| part.begin < hap.part.end);
                let Some(next_total) = total_met.checked_add(after.saturating_sub(first)) else {
                    let _ = thread.charge(usize::MAX);
                    return thread.stop_requested();
                };
                total_met = next_total;
            }
            if !thread.charge(out.len().saturating_add(total_met)) {
                return true;
            }
            // Each hap at each part that it meets, and whether the part is the
            // last one, which takes the hap.
            let mut met = SPARE_MET
                .try_with(|cell| std::mem::take(&mut *cell.borrow_mut()))
                .unwrap_or_default();
            met.clear();
            met.reserve(total_met);
            for (index, hap) in haps.iter().enumerate() {
                if (index & 63) == 0 && thread.stop_requested() {
                    met.clear();
                    if met.capacity() > 0 && met.capacity() <= 128 {
                        let _ = SPARE_MET.try_with(|cell| *cell.borrow_mut() = met);
                    }
                    return true;
                }
                let first = parts.partition_point(|part| part.end <= hap.part.begin);
                let after = parts.partition_point(|part| part.begin < hap.part.end);
                met.extend((first..after).map(|part| (part, index, part + 1 == after)));
            }
            // Haps in the order of the parts, each in one part: the answer.
            if met.len() == haps.len() && met.is_sorted_by_key(|(part, ..)| *part) {
                met.clear();
                if met.capacity() > 0 && met.capacity() <= 128 {
                    let _ = SPARE_MET.try_with(|cell| *cell.borrow_mut() = met);
                }
                if out.capacity() == 0 {
                    *out = std::mem::take(haps);
                } else {
                    out.append(haps);
                }
                return true;
            }
            // The sort keeps the order of the answer in each part.
            met.sort_by_key(|(part, ..)| *part);
            let mut slots = SPARE_SLOTS
                .try_with(|cell| std::mem::take(&mut *cell.borrow_mut()))
                .unwrap_or_default();
            slots.clear();
            slots.extend(haps.drain(..).map(Some));
            let from = out.len();
            out.reserve(met.len());
            let mut ok = true;
            let mut stopped = false;
            for (step, &(part_idx, index, last)) in met.iter().enumerate() {
                if (step & 63) == 0 && thread.stop_requested() {
                    stopped = true;
                    ok = false;
                    break;
                }
                let hap = match last {
                    true => slots[index].take(),
                    false => slots[index].clone(),
                };
                let Some(mut hap) = hap else {
                    ok = false;
                    break;
                };
                let part = parts[part_idx];
                hap.part =
                    TimeSpan::new(hap.part.begin.max(part.begin), hap.part.end.min(part.end));
                out.push(hap);
            }
            if !ok {
                out.truncate(from);
            }
            slots.clear();
            if slots.capacity() > 0 && slots.capacity() <= 64 {
                let _ = SPARE_SLOTS.try_with(|cell| *cell.borrow_mut() = slots);
            }
            met.clear();
            if met.capacity() > 0 && met.capacity() <= 128 {
                let _ = SPARE_MET.try_with(|cell| *cell.borrow_mut() = met);
            }
            ok || stopped
        })
    }

    /// The haps that an `appRight` makes of this pattern of functions and
    /// the value haps `values`, from one query of this pattern, when its
    /// graph is one of [discrete](Self::is_discrete) haps. Each value hap
    /// asks the functions over its whole, so one query over all the wholes
    /// holds the answer for each.
    ///
    /// `None` for a few value haps, which their own queries serve, for
    /// wholes that leave most of the time between them free, for a graph
    /// that may read the span, and for a whole of no width.
    pub(crate) fn met_in_one_query_onto(
        &self,
        values: &[Hap],
        apply: &Combine,
        flow: LookupFlow,
        thread: &QueryThread,
        state: &State,
        out: &mut Vec<Hap>,
    ) -> bool {
        /// The fewest value haps that one query of the functions repays.
        const REPAYS_FROM: usize = 4;
        if values.len() < REPAYS_FROM || !self.is_discrete() {
            return false;
        }
        // This graph answers for the part of a hap as for its whole, cut.
        let asked = |hv: &Hap| hv.part;
        if values.iter().any(|hv| asked(hv).begin >= asked(hv).end) {
            return false;
        }
        let Some(min_begin) = values.iter().map(|hv| asked(hv).begin).min() else {
            return false;
        };
        let Some(max_end) = values.iter().map(|hv| asked(hv).end).max() else {
            return false;
        };
        let all = TimeSpan::new(min_begin, max_end);
        // The functions between the wholes are made for nothing.
        let width = |span: TimeSpan| span.end.to_f64() - span.begin.to_f64();
        let filled: f64 = values.iter().map(|hv| width(asked(hv))).sum();
        if filled * 2.0 < width(all) {
            return false;
        }
        with_list(|functions| {
            self.answer_child_onto(thread, &state.set_span(all), functions);
            let n_funcs = functions.len();
            // Stack fast path for <= 32 function haps: zero heap allocations for
            // `by_begin`, `latest_end`, and `meeting`.
            let mut small_by_begin = [0usize; 32];
            let mut small_latest_end = [Fraction::ZERO; 32];
            let mut small_meeting = [0usize; 32];
            let heap_by_begin;
            let mut heap_latest_end = Vec::new();
            let mut heap_meeting = Vec::new();
            let (by_begin, latest_end): (&[usize], &[Fraction]) = if n_funcs <= 32 {
                for (i, slot) in small_by_begin[..n_funcs].iter_mut().enumerate() {
                    *slot = i;
                }
                small_by_begin[..n_funcs]
                    .sort_by(|a, b| functions[*a].part.begin.cmp(&functions[*b].part.begin));
                let mut latest = Fraction::ZERO;
                for (i, &index) in small_by_begin[..n_funcs].iter().enumerate() {
                    let end = functions[index].part.end;
                    latest = if i == 0 { end } else { end.max(latest) };
                    small_latest_end[i] = latest;
                }
                (&small_by_begin[..n_funcs], &small_latest_end[..n_funcs])
            } else {
                let mut indices: Vec<usize> = (0..n_funcs).collect();
                indices.sort_by(|a, b| functions[*a].part.begin.cmp(&functions[*b].part.begin));
                heap_by_begin = indices;
                heap_latest_end.reserve(n_funcs);
                for &index in &heap_by_begin {
                    let end = functions[index].part.end;
                    heap_latest_end.push(
                        heap_latest_end
                            .last()
                            .map_or(end, |latest| end.max(*latest)),
                    );
                }
                (&heap_by_begin, &heap_latest_end)
            };
            let from = out.len();
            out.reserve(values.len());
            for hv in values {
                let span = asked(hv);
                let first = latest_end.partition_point(|end| *end <= span.begin);
                let after =
                    by_begin.partition_point(|index| functions[*index].part.begin < span.end);
                let range = by_begin.get(first..after).unwrap_or_default();
                let met_indices: &[usize] = if range.len() <= 32 {
                    let mut m_len = 0usize;
                    for &index in range {
                        if functions[index].part.end > span.begin {
                            small_meeting[m_len] = index;
                            m_len += 1;
                        }
                    }
                    small_meeting[..m_len].sort_unstable();
                    &small_meeting[..m_len]
                } else {
                    heap_meeting.clear();
                    heap_meeting.extend(
                        range
                            .iter()
                            .copied()
                            .filter(|index| functions[*index].part.end > span.begin),
                    );
                    heap_meeting.sort_unstable();
                    &heap_meeting
                };
                for &index in met_indices {
                    let hf = &functions[index];
                    // The part of the function hap, as the query of the whole
                    // cuts it, then where the two haps meet.
                    let cut =
                        TimeSpan::new(hf.part.begin.max(span.begin), hf.part.end.min(span.end));
                    let Some(part) = cut.intersection(&hv.part) else {
                        continue;
                    };
                    let hap = applied_hap(hf, hv, hv.whole, part, apply, flow);
                    if thread.stop_requested() || !thread.push_budgeted(out, from, hap) {
                        out.truncate(from);
                        return;
                    }
                }
            }
        });
        true
    }

    /// The query. Everything else is built on this.
    pub fn query(&self, state: &State) -> Vec<Hap> {
        let mut haps = Vec::new();
        self.query_onto(state, &mut haps);
        haps
    }

    /// [`Self::query`], with the haps after those that `out` holds. A walk
    /// writes the haps of its nodes in one list: a node that changes the
    /// haps of the node under it changes them where they are, and a stack
    /// copies none.
    pub(crate) fn query_onto(&self, state: &State, out: &mut Vec<Hap>) {
        QUERY_THREAD.with(|thread| {
            self.answer_onto(thread, state, table::Order::Graph, out);
        });
    }

    /// [`Self::query`] with the haps in `order`, and whether a table gave
    /// the answer, so that it holds data only. The list has room for
    /// `room` haps: it is the list of the query of a caller, which takes
    /// most answers with no move.
    fn answer_with_room(
        &self,
        state: &State,
        order: table::Order,
        room: usize,
    ) -> (Vec<Hap>, bool) {
        let mut haps = Vec::with_capacity(room);
        let data_only =
            QUERY_THREAD.with(|thread| self.answer_onto(thread, state, order, &mut haps));
        (haps, data_only)
    }

    /// [`Self::answer_with_room`] after the haps of `out`, on the counters
    /// of the thread that asks, reached one time for the node.
    pub(crate) fn answer_onto(
        &self,
        thread: &QueryThread,
        state: &State,
        order: table::Order,
        out: &mut Vec<Hap>,
    ) -> bool {
        struct MergeCacheGuard(bool);
        impl Drop for MergeCacheGuard {
            fn drop(&mut self) {
                if self.0 {
                    context_keys::end_query_merge_cache();
                }
            }
        }
        let outermost = thread.depth.get() == 0;
        if outermost {
            context_keys::begin_query_merge_cache();
        }
        let _merge_guard = MergeCacheGuard(outermost);
        if self.is_volatile() {
            note_uncacheable();
        }
        match self.runtime_settings() {
            Some(settings) => {
                if thread.current_runtime.get() == settings.ptr_id() {
                    let owner = outermost.then_some(settings);
                    self.query_with_bound_runtime(thread, state, owner, false, order, out)
                } else {
                    // The child binds a runtime of its own, whose snapshot
                    // can move without the enclosing one changing.
                    note_uncacheable();
                    let _scope = settings.bind();
                    self.query_with_bound_runtime(thread, state, Some(settings), false, order, out)
                }
            }
            None => {
                let attach_current = outermost && thread.current_runtime.get() != 0;
                self.query_with_bound_runtime(thread, state, None, attach_current, order, out)
            }
        }
    }

    /// Recursive child query (always `Order::Graph`, never outermost).
    /// Bypasses `MergeCacheGuard`, `Option<RuntimeSettings>`, `Order::Sorted`,
    /// and (for leaf `Node::Sequence`) `DepthGuard` + `query_node_onto` dispatch.
    /// Kept out-of-line so 50+ interior call sites share one hot icache copy
    /// instead of bloating `query.rs` across multiple code pages.
    #[inline(never)]
    pub(crate) fn answer_child_onto(
        &self,
        thread: &QueryThread,
        state: &State,
        out: &mut Vec<Hap>,
    ) {
        if !self.has_runtime_settings() && !self.is_volatile() {
            self.query_plain_child_onto(thread, state, out);
        } else {
            self.answer_child_slow_onto(thread, state, out);
        }
    }

    #[cold]
    #[inline(never)]
    fn answer_child_slow_onto(&self, thread: &QueryThread, state: &State, out: &mut Vec<Hap>) {
        if self.is_volatile() {
            note_uncacheable();
        }
        match self.runtime_settings() {
            Some(settings) => {
                if thread.current_runtime.get() == settings.ptr_id() {
                    self.query_plain_child_onto(thread, state, out);
                } else {
                    note_uncacheable();
                    let _scope = settings.bind();
                    self.query_with_bound_runtime(
                        thread,
                        state,
                        Some(settings),
                        false,
                        table::Order::Graph,
                        out,
                    );
                }
            }
            None => self.query_plain_child_onto(thread, state, out),
        }
    }

    /// Non-outermost `Order::Graph` query for a child with no `RuntimeSettings`
    /// override and no `metadata_owner`.
    #[inline(never)]
    pub(crate) fn query_plain_child_onto(
        &self,
        thread: &QueryThread,
        state: &State,
        out: &mut Vec<Hap>,
    ) {
        let depth = thread.depth.get().saturating_add(1);
        if depth > MAX_PATTERN_DEPTH {
            refuse(QueryLimit::GraphDepth {
                depth,
                limit: MAX_PATTERN_DEPTH,
            });
            return;
        }
        if thread.stop_requested() {
            return;
        }
        if let Node::Sequence(layers) = self.node_kind() {
            let from = out.len();
            sequence::query_sequence_onto(thread, layers, state, out);
            let made = out.len() - from;
            thread.note_accumulated(made);
            if !thread.charge(made) {
                out.truncate(from);
            }
            return;
        }
        struct DepthGuard<'a>(&'a QueryThread);
        impl Drop for DepthGuard<'_> {
            fn drop(&mut self) {
                self.0.depth.set(self.0.depth.get().saturating_sub(1));
            }
        }
        thread.depth.set(depth);
        let _guard = DepthGuard(thread);
        let from = out.len();
        self.query_node_onto(thread, state, out);
        let made = out.len() - from;
        thread.note_accumulated(made);
        if !thread.charge(made) {
            out.truncate(from);
        }
    }

    /// The haps of this node after those of `out`, and whether they hold
    /// data only. A query that stops writes none.
    pub(crate) fn query_with_bound_runtime(
        &self,
        thread: &QueryThread,
        state: &State,
        metadata_owner: Option<&settings::RuntimeSettings>,
        attach_current_if_needed: bool,
        order: table::Order,
        out: &mut Vec<Hap>,
    ) -> bool {
        // Restores the counter however this frame leaves - including on a
        // panic, so a caught query error cannot leave the thread's depth
        // permanently charged and refuse every later query.
        struct DepthGuard<'a>(&'a QueryThread);
        impl Drop for DepthGuard<'_> {
            fn drop(&mut self) {
                self.0.depth.set(self.0.depth.get().saturating_sub(1));
            }
        }
        let depth = thread.depth.get().saturating_add(1);
        thread.depth.set(depth);
        let _guard = DepthGuard(thread);
        if depth > MAX_PATTERN_DEPTH {
            refuse(QueryLimit::GraphDepth {
                depth,
                limit: MAX_PATTERN_DEPTH,
            });
            return true;
        }
        if thread.stop_requested() {
            return true;
        }
        let from = out.len();
        let handled = if order == table::Order::Sorted {
            match self.node_kind() {
                Node::Sequence(layers) if !layers.is_empty() => {
                    sequence::query_sequence_sorted_onto(thread, layers, state, out)
                }
                Node::Fast(node)
                    if !node.0.has_runtime_settings()
                        && (node.0.node_flags() & FLAG_VOLATILE) == 0 =>
                {
                    if let Some(sorted) = try_query_fast_pow2_slowcat_onto(
                        thread,
                        node.0.node_kind(),
                        node.1,
                        node.2,
                        state,
                        out,
                    ) {
                        if !sorted {
                            crate::hap::sort_haps_for_query_in_place(&mut out[from..]);
                        }
                        true
                    } else {
                        false
                    }
                }
                Node::FastSmall(cell, flags, numer, denom, is_slow)
                    if (*flags & FLAG_VOLATILE) == 0 =>
                {
                    let factor = Fraction::new(i128::from(*numer), i128::from(*denom));
                    let op = if *is_slow { "slow" } else { "fast" };
                    if let Some(sorted) =
                        try_query_fast_pow2_slowcat_onto(thread, &cell.kind, factor, op, state, out)
                    {
                        if !sorted {
                            crate::hap::sort_haps_for_query_in_place(&mut out[from..]);
                        }
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            }
        } else {
            false
        };
        if !handled {
            self.query_node_onto(thread, state, out);
            if order == table::Order::Sorted {
                crate::hap::sort_haps_for_query_in_place(&mut out[from..]);
            }
        }
        let mut data_only = matches!(
            self.node_kind(),
            Node::Sequence(layers) if sequence::holds_data(layers)
        );
        if !data_only && (metadata_owner.is_some() || attach_current_if_needed) {
            let mut all_known_data = true;
            let mut lazy_current: Option<Option<settings::RuntimeSettings>> = None;
            for at in from..out.len() {
                let value = &mut out[at].value;
                if matches!(
                    value,
                    Value::F64(_) | Value::Str(_) | Value::Bool(_) | Value::Null | Value::Undefined
                ) || matches!(value, Value::Object(map) if map.is_known_data())
                {
                    continue;
                }
                all_known_data = false;
                let settings = match metadata_owner {
                    Some(settings) => Some(settings),
                    None => lazy_current
                        .get_or_insert_with(settings::RuntimeSettings::current_requested)
                        .as_ref(),
                };
                if let Some(settings) = settings
                    && !value.attach_runtime_settings(settings, 0)
                {
                    out.truncate(from);
                    return true;
                }
            }
            data_only = all_known_data;
        }
        // Already built by the time we see it - recorded so a producer that
        // grows without an incremental check is visible as the vector it made.
        let made = out.len() - from;
        thread.note_accumulated(made);
        if !thread.charge(made) {
            out.truncate(from);
            return true;
        }
        data_only
    }

    /// Query a plain non-outermost pure [`NodeCell`] without cloning its `Arc`
    /// for the common `Sequence`, `StackPure`, and `Constants` node kinds.
    pub(crate) fn query_plain_cell_onto(
        cell: &Arc<NodeCell>,
        thread: &QueryThread,
        state: &State,
        out: &mut Vec<Hap>,
    ) {
        struct DepthGuard<'a>(&'a QueryThread);
        impl Drop for DepthGuard<'_> {
            fn drop(&mut self) {
                self.0.depth.set(self.0.depth.get().saturating_sub(1));
            }
        }
        match &cell.kind {
            Node::Silence => {}
            Node::Sequence(layers) => {
                let depth = thread.depth.get().saturating_add(1);
                if depth > MAX_PATTERN_DEPTH {
                    refuse(QueryLimit::GraphDepth {
                        depth,
                        limit: MAX_PATTERN_DEPTH,
                    });
                    return;
                }
                if thread.stop_requested() {
                    return;
                }
                let from = out.len();
                sequence::query_sequence_onto(thread, layers, state, out);
                let made = out.len() - from;
                thread.note_accumulated(made);
                if !thread.charge(made) {
                    out.truncate(from);
                }
            }
            Node::StackPure(cells) => {
                let depth = thread.depth.get().saturating_add(1);
                thread.depth.set(depth);
                let _guard = DepthGuard(thread);
                if depth > MAX_PATTERN_DEPTH {
                    refuse(QueryLimit::GraphDepth {
                        depth,
                        limit: MAX_PATTERN_DEPTH,
                    });
                    return;
                }
                if thread.stop_requested() {
                    return;
                }
                let from = out.len();
                query_stack_pure_onto(thread, cells, state, out);
                let made = out.len() - from;
                thread.note_accumulated(made);
                if !thread.charge(made) {
                    out.truncate(from);
                }
            }
            Node::Constants(node) => {
                let depth = thread.depth.get().saturating_add(1);
                thread.depth.set(depth);
                let _guard = DepthGuard(thread);
                if depth > MAX_PATTERN_DEPTH {
                    refuse(QueryLimit::GraphDepth {
                        depth,
                        limit: MAX_PATTERN_DEPTH,
                    });
                    return;
                }
                if thread.stop_requested() {
                    return;
                }
                let from = out.len();
                query_run(thread, &node.0, &node.1, state, out);
                let made = out.len() - from;
                thread.note_accumulated(made);
                if !thread.charge(made) {
                    out.truncate(from);
                }
            }
            _ => {
                plain_pattern(cell, 0).query_plain_child_onto(thread, state, out);
            }
        }
    }

    /// The haps of this `Pure` or `PurePickLookup` node of `value`: one a
    /// cycle, each aligned to its whole cycle.
    pub(crate) fn query_value_onto(
        &self,
        thread: &QueryThread,
        value: &Value,
        state: &State,
        out: &mut Vec<Hap>,
    ) {
        let single_cycle = state.span.begin <= state.span.end && state.span.within_one_cycle();
        if single_cycle {
            if !thread.charge(1) {
                return;
            }
            note_materialised(1);
            if thread.stop_requested() {
                return;
            }
            let lookup = match self.node_kind() {
                Node::PurePickLookup(_) => {
                    let Some(lookup) = self.as_pick_lookup().map(Arc::new) else {
                        return;
                    };
                    Some(lookup)
                }
                _ => None,
            };
            let hap = Hap::new(
                Some(whole_cycle(state.span.begin)),
                state.span,
                value.clone(),
            );
            match lookup {
                Some(lookup) => out.push(hap.with_pick_lookup(lookup)),
                None => out.push(hap),
            }
            return;
        }
        // Charged before the haps are built. This is the one node that
        // can produce a large vector in a single pass (one hap per
        // cycle, up to `MAX_QUERY_SPAN_CYCLES` of them), so charging it
        // afterwards, like the generic wrapper in `query` does, would
        // let a million haps exist first. Everything else grows by
        // combining smaller results, which the wrapper catches.
        let count = if state.span.begin == state.span.end {
            1usize
        } else {
            let Some(c) = state
                .span
                .cycle_count()
                .filter(|c| *c <= MAX_QUERY_SPAN_CYCLES)
            else {
                refuse(QueryLimit::QuerySpan {
                    cycles: MAX_QUERY_SPAN_CYCLES,
                });
                return;
            };
            usize::try_from(c).unwrap_or_default()
        };
        if !thread.charge(count) {
            return;
        }
        note_materialised(count);
        let lookup = match self.node_kind() {
            Node::PurePickLookup(_) => {
                let Some(lookup) = self.as_pick_lookup().map(Arc::new) else {
                    return;
                };
                Some(lookup)
            }
            _ => None,
        };
        let from = out.len();
        out.reserve(count);
        let mut stopped = false;
        state.span.for_each_cycle(|subspan| {
            if stopped {
                return;
            }
            if thread.stop_requested() {
                stopped = true;
                return;
            }
            let hap = Hap::new(Some(whole_cycle(subspan.begin)), subspan, value.clone());
            match &lookup {
                Some(lookup) => out.push(hap.with_pick_lookup(Arc::clone(lookup))),
                None => out.push(hap),
            }
        });
        if stopped {
            out.truncate(from);
        }
    }

    /// The haps of this node after those that `out` holds. A node of a
    /// kind that a walk meets often writes them there. A node of any other
    /// kind makes its list, and the list moves there.
    pub(crate) fn query_node_onto(&self, thread: &QueryThread, state: &State, out: &mut Vec<Hap>) {
        match self.node_kind() {
            Node::Silence => {}

            Node::Pure(value) => {
                self.query_value_onto(thread, value, state, out);
            }
            Node::PureAt(boxed) => {
                let from = out.len();
                self.query_value_onto(thread, &boxed.0, state, out);
                for hap in &mut out[from..] {
                    hap.context.append(&boxed.1);
                }
            }
            Node::PurePickLookup(boxed) => {
                self.query_value_onto(thread, &boxed.0, state, out);
            }

            Node::Sequence(layers) => sequence::query_sequence_onto(thread, layers, state, out),

            Node::Fast(node) => {
                if !node.0.has_runtime_settings()
                    && (node.0.node_flags() & FLAG_VOLATILE) == 0
                    && try_query_fast_pow2_slowcat_onto(
                        thread,
                        node.0.node_kind(),
                        node.1,
                        node.2,
                        state,
                        out,
                    )
                    .is_some()
                {
                    return;
                }
                node.0
                    .query_faster_onto(thread, node.1, None, node.2, state, out);
            }

            Node::FastSmall(cell, flags, numer, denom, is_slow) => {
                let factor = Fraction::new(i128::from(*numer), i128::from(*denom));
                let op = if *is_slow { "slow" } else { "fast" };
                if (*flags & FLAG_VOLATILE) == 0
                    && try_query_fast_pow2_slowcat_onto(thread, &cell.kind, factor, op, state, out)
                        .is_some()
                {
                    return;
                }
                if (*flags & FLAG_VOLATILE) == 0
                    && matches!(
                        cell.kind,
                        Node::Silence | Node::Sequence(_) | Node::StackPure(_) | Node::Constants(_)
                    )
                {
                    query_faster_with_onto(thread, factor, None, op, state, out, |asked, out| {
                        Pattern::query_plain_cell_onto(cell, thread, asked, out);
                    });
                } else {
                    plain_pattern(cell, *flags)
                        .query_faster_onto(thread, factor, None, op, state, out);
                }
            }

            Node::Shift(node) => {
                node.0.query_moved_onto(thread, node.1, node.2, state, out);
            }

            Node::ShiftSmall(cell, flags, numer, denom, is_early) => {
                let offset = Fraction::new(i128::from(*numer), i128::from(*denom));
                let op = if *is_early { "early" } else { "late" };
                if (*flags & FLAG_VOLATILE) == 0
                    && matches!(
                        cell.kind,
                        Node::Silence | Node::Sequence(_) | Node::StackPure(_) | Node::Constants(_)
                    )
                {
                    query_moved_with_onto(thread, offset, op, state, out, |asked, out| {
                        Pattern::query_plain_cell_onto(cell, thread, asked, out);
                    });
                } else {
                    plain_pattern(cell, *flags).query_moved_onto(thread, offset, op, state, out);
                }
            }

            Node::Cpm(boxed) => {
                let (pat, rate) = (&boxed.0, &boxed.1);
                let scheduler_cps = state
                    .controls
                    .get("_cps")
                    .and_then(Value::as_f64)
                    .filter(|cps| cps.is_finite() && *cps > 0.0)
                    .unwrap_or(0.5);
                // What `pat.fast(speed)` answers, with no node for it.
                if let Some((speed, grid)) = rate.speed_at(scheduler_cps)
                    && speed.numer() != 0
                {
                    pat.query_faster_onto(thread, speed, grid, "fast", state, out);
                }
            }

            Node::FilterValues(boxed) => {
                let (pat, predicate) = (&boxed.0, &boxed.1);
                if Pattern::is_keep_defined(predicate)
                    && !pat.has_runtime_settings()
                    && let Node::AppRight(app) = pat.node_kind()
                    && fused::app_right_wave_onto(thread, app, state, out)
                {
                    return;
                }
                let from = out.len();
                pat.answer_child_onto(thread, state, out);
                retain_from(out, from, |hap| predicate(&hap.value));
            }

            Node::FilterHaps(boxed) => {
                let (pat, predicate) = (&boxed.0, &boxed.1);
                let from = out.len();
                pat.answer_child_onto(thread, state, out);
                retain_from(out, from, |hap| predicate(hap));
            }

            Node::Degrade(boxed) => {
                let (pattern, amount, kind) = (&boxed.0, boxed.1, boxed.2);
                let random_seed = state
                    .controls
                    .get("randSeed")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                let from = out.len();
                pattern.answer_child_onto(thread, state, out);
                match kind {
                    DegradeKind::Seeded(seed) => {
                        retain_from(out, from, |hap| {
                            let time =
                                hap.whole_or_part().begin.to_f64() + 0.0003 * f64::from(seed);
                            rng::rand_at_time(time, random_seed) > amount
                        });
                    }
                    DegradeKind::Rand | DegradeKind::RandInverted => {
                        let inverted = kind == DegradeKind::RandInverted;
                        retain_from(out, from, |hap| {
                            let met = hap.whole_or_part();
                            let r = rng::rand_at_time(met.begin.to_f64(), random_seed);
                            let coin = if inverted { 1.0 - r } else { r };
                            if coin.partial_cmp(&amount) != Some(std::cmp::Ordering::Greater) {
                                return false;
                            }
                            let part = match hap.whole {
                                None => (hap.part.begin <= hap.part.end).then_some(hap.part),
                                Some(whole) if whole == hap.part => {
                                    (hap.part.begin <= hap.part.end).then_some(hap.part)
                                }
                                Some(whole) => hap.part.intersection(&whole),
                            };
                            let Some(part) = part else {
                                return false;
                            };
                            hap.part = part;
                            true
                        });
                    }
                }
            }

            Node::Stack(pats) => query_stack_onto(thread, pats, state, out, || {}),
            Node::StackPure(cells) => query_stack_pure_onto(thread, cells, state, out),

            Node::AddContext(boxed) => {
                let (pat, locs) = (&boxed.0, &boxed.1);
                let from = out.len();
                // A located value is two nodes, and one visit answers for
                // both. Any other handle is queried: it may bind a runtime.
                match pat.node_kind() {
                    Node::Pure(value) if !pat.has_runtime_settings() => {
                        pat.query_value_onto(thread, value, state, out);
                    }
                    _ => {
                        pat.answer_child_onto(thread, state, out);
                    }
                }
                for hap in &mut out[from..] {
                    hap.context.extend_from_slice(locs);
                }
            }

            Node::AppWave(app) => {
                if self.try_query_wave_chain_onto(thread, state, out) {
                    return;
                }
                if self.try_query_app_wave_steps_onto(app, thread, state, out) {
                    return;
                }
                // Rare fallback for non-object haps, multi-cycle parts, >48-bit
                // rationals, or cut_mask boundaries: rebuild exact wave chain.
                let mut current = app.inner.clone();
                for item in app.waves.iter() {
                    let values =
                        item.wave
                            .to_control_pattern(item.key_id, item.math_inside, item.steps);
                    current = Pattern::of(Node::AppLeft(Box::new(AppLeftNode(
                        current,
                        values,
                        ValueReading::default(),
                        Combine::Composed(compose::ComposeOp::Set),
                        LookupFlow::Set,
                    ))));
                }
                current.answer_child_onto(thread, state, out);
            }

            // Function haps drive: values are queried over each function
            // hap's wholeOrPart, and the function side's whole wins.
            node @ Node::AppLeft(..) => {
                let side = ValueSide::of(node).expect("an application");
                // Fast in-place path for a chain of 1..=8 wave controls
                // (`.lpf(sine...).pan(rand)`): queries the base directly into
                // `out`, caches chord samples, and applies all patches in one
                // `OrderedMap` allocation with no `with_list` scratch copy.
                if self.try_query_wave_chain_onto(thread, state, out) {
                    return;
                }
                // Fast in-place path for a degrade gate: filters `out` directly.
                if self.try_query_gate_onto(thread, side, state, out) {
                    return;
                }
                with_list(|functions| {
                    side.functions.answer_child_onto(thread, state, functions);
                    if functions.is_empty() {
                        return;
                    }
                    // The control of a segment of a wave takes each number
                    // where the function hap is (cached once in `ValueReading`).
                    let steps = if matches!(side.apply, Combine::Composed(compose::ComposeOp::Set))
                        && side.flow == LookupFlow::Set
                        && side.values.tables_may_answer()
                        && let Some(ReadValues::Signal(signal)) =
                            side.reading.of(side.values, state)
                    {
                        signal
                            .step_wave()
                            .filter(|_| functions.iter().all(fused::StepWave::takes))
                    } else {
                        None
                    };
                    if steps.is_none() && side.met_in_one_query_on(thread, functions, state, out) {
                        return;
                    }
                    let from = out.len();
                    let mut meetings = Meetings {
                        steps,
                        ..Meetings::default()
                    };
                    out.reserve(functions.len());
                    for hf in functions.drain(..) {
                        if !side.meet_on(thread, hf, &mut meetings, state, from, out) {
                            out.truncate(from);
                            return;
                        }
                    }
                });
            }

            // A signal under steps of its own, read in equal steps of a
            // cycle as `segment` gives, answers in one walk of the steps.
            Node::AppRight(node) => {
                if fused::app_right_onto_on(thread, node, state, out) {
                    return;
                }
                let AppNode(pat_func, pat_val, apply, flow) = &**node;
                with_list(|values| {
                    pat_val.answer_child_onto(thread, state, values);
                    if values.is_empty() {
                        return;
                    }
                    if pat_func.met_in_one_query_onto(values, apply, *flow, thread, state, out) {
                        return;
                    }
                    let mut sampler =
                        table::signal_of(pat_func).map(|signal| (signal, state.clone()));
                    let from = out.len();
                    out.reserve(values.len());
                    with_list(|funcs| {
                        for hv in values.drain(..) {
                            let asked = pat_func.asked_for(hv.whole_or_part(), hv.part);
                            let sampled = sampler.as_mut().is_some_and(|(signal, probe)| {
                                signal.answer(asked, hv.part, probe, funcs).is_some()
                            });
                            if !sampled {
                                funcs.clear();
                                pat_func.answer_child_onto(thread, &state.set_span(asked), funcs);
                            }
                            for hf in funcs.iter() {
                                if let Some(part) = hf.part.intersection(&hv.part) {
                                    let hap = applied_hap(hf, &hv, hv.whole, part, apply, *flow);
                                    if thread.stop_requested()
                                        || !thread.push_budgeted(out, from, hap)
                                    {
                                        out.truncate(from);
                                        return;
                                    }
                                }
                            }
                        }
                    });
                });
            }

            Node::Pick(pick) => query_pick(
                thread,
                &pick.selector,
                LookupRef::Compact(&pick.lookup),
                pick.index_mode,
                pick.mode,
                state,
                out,
                &|| {},
            ),

            Node::Constants(node) => query_run(thread, &node.0, &node.1, state, out),

            Node::WithQueryTime(boxed) => {
                let s = state.with_span(|sp| sp.with_time(|t| (boxed.1)(t)));
                boxed.0.answer_child_onto(thread, &s, out);
            }

            Node::WithHapTime(boxed) => {
                let from = out.len();
                boxed.0.answer_child_onto(thread, state, out);
                for hap in &mut out[from..] {
                    hap.update_span(|sp| sp.with_time(|t| (boxed.1)(t)));
                }
            }

            Node::WithQuerySpan(boxed) => {
                boxed
                    .0
                    .answer_child_onto(thread, &state.with_span(|sp| (boxed.1)(sp)), out);
            }

            Node::WithHapSpan(boxed) => {
                let from = out.len();
                boxed.0.answer_child_onto(thread, state, out);
                for hap in &mut out[from..] {
                    hap.update_span(|sp| (boxed.1)(sp));
                }
            }

            Node::Rev(pat) => {
                let cycle = state.span.begin.sam();
                let next_cycle = state.span.begin.next_sam();
                let pivot = cycle
                    .numer()
                    .checked_mul(2)
                    .and_then(|v| v.checked_add(1))
                    .map(Fraction::int);
                let reflect = |sp: &TimeSpan| match pivot {
                    Some(p) => TimeSpan::new(p.sub(sp.end), p.sub(sp.begin)),
                    None => {
                        let b = cycle.add(next_cycle.sub(sp.begin));
                        let e = cycle.add(next_cycle.sub(sp.end));
                        TimeSpan::new(e, b)
                    }
                };
                let from = out.len();
                pat.answer_child_onto(thread, &state.set_span(reflect(&state.span)), out);
                for hap in &mut out[from..] {
                    hap.update_span(reflect);
                }
            }

            Node::FmapNative(boxed) => {
                let from = out.len();
                boxed.0.answer_child_onto(thread, state, out);
                for hap in &mut out[from..] {
                    hap.map_value_in_place(|value| (boxed.1)(value));
                }
            }

            Node::ExpandHapsNative(boxed) => {
                let (pat, f) = (&boxed.0, &boxed.1);
                let from = out.len();
                with_list(|haps| {
                    pat.answer_child_onto(thread, state, haps);
                    for h in haps.drain(..) {
                        f(&h, out);
                        let count = out.len() - from;
                        if thread.stop_requested() || !thread.charge(count) {
                            out.truncate(from);
                            return;
                        }
                        thread.note_accumulated(count);
                    }
                });
            }

            Node::SlowCatPrime(pats) => {
                if pats.is_empty() {
                    return;
                }
                let n = pats.len() as i128;
                let index = state.span.begin.floor().numer().rem_euclid(n);
                pats[index as usize].answer_child_onto(thread, state, out);
            }

            Node::SlowCatPrimePure(cells) => {
                if cells.is_empty() {
                    return;
                }
                let n = cells.len() as i128;
                let index = state.span.begin.floor().numer().rem_euclid(n);
                let cell = &cells[index as usize];
                if !matches!(cell.kind, Node::Silence) {
                    Pattern::query_plain_cell_onto(cell, thread, state, out);
                }
            }

            Node::Signal(signal) => {
                out.push(Hap::new(None, state.span, signal(state)));
            }

            Node::ControlMap(cell, flags, id, unnamed_only) => {
                let from = out.len();
                if (*flags & FLAG_VOLATILE) == 0 {
                    Pattern::query_plain_cell_onto(cell, thread, state, out);
                } else {
                    plain_pattern(cell, *flags).answer_child_onto(thread, state, out);
                }
                if let Some(spec) = controls::builtin_spec_by_id(*id) {
                    map_from(out, from, |hap| {
                        controls::apply_control_hap(spec, *unnamed_only, hap)
                    });
                }
            }

            Node::ControlMapBoxed(boxed) => {
                let (pat, id, unnamed_only) = (&boxed.0, boxed.1, boxed.2);
                let from = out.len();
                pat.answer_child_onto(thread, state, out);
                if let Some(spec) = controls::builtin_spec_by_id(id) {
                    map_from(out, from, |hap| {
                        controls::apply_control_hap(spec, unnamed_only, hap)
                    });
                }
            }

            // A map of each hap, where the hap is. A hap that the map drops
            // leaves the list.
            Node::MapHapsNative(boxed) => {
                let (pat, map) = (&boxed.0, &boxed.1);
                let from = out.len();
                pat.answer_child_onto(thread, state, out);
                let mut kept = from;
                for at in from..out.len() {
                    if let Some(mapped) = map(&out[at]) {
                        out[kept] = mapped;
                        kept += 1;
                    }
                }
                out.truncate(kept);
            }

            Node::MapHapsOwned(boxed) => {
                let (pat, map) = (&boxed.0, &boxed.1);
                let from = out.len();
                pat.answer_child_onto(thread, state, out);
                map_from(out, from, |hap| map(hap));
            }

            Node::UiVisual(node) => {
                let from = out.len();
                node.inner.answer_child_onto(thread, state, out);
                if out.len() > from {
                    ContextKeys::add_ui_visual_slot_to_haps(
                        &mut out[from..],
                        node.slot,
                        &node.cache,
                    );
                }
            }

            // One constituent per cycle. `offset` keeps constituent cycles
            // contiguous: the fourth cycle of a three-pattern slowcat is the
            // SECOND cycle of the first pattern, not the fourth. The wrapping
            // `split_queries` keeps a multi-cycle query from being served by
            // a single constituent. A shifted time outside the native range
            // refuses as `NativeFraction { operation: "slowcat" }`.
            Node::SlowCat(pats) => slowcat_cycle_onto(thread, pats, state, out),
            Node::SlowCatPure(cells) => slowcat_cycle_pure_onto(thread, cells, state, out),

            Node::ChooseCycles(boxed) => {
                let (patterns, seed) = (&boxed.0, &boxed.1);
                if patterns.is_empty() {
                    return;
                }
                let random_seed = state
                    .controls
                    .get("randSeed")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                let small;
                let heap;
                let pieces: &[TimeSpan] = if let Some((buf, len)) = state.span.span_cycles_small() {
                    small = buf;
                    &small[..len]
                } else {
                    heap = state.span.span_cycles();
                    &heap
                };
                let from = out.len();
                for &span in pieces {
                    // `segment(1)` is `struct(pure(true))`, whose appRight
                    // queries `rand` over the pure hap's whole (a full
                    // cycle), so the signal is sampled at the cycle start.
                    // A sample at the sub-span's begin would make the choice
                    // vary within a cycle: `vowel("[a|e|i|o|u]")` would pick
                    // a different vowel per event instead of per cycle.
                    let sample_time = span.begin.sam().to_f64() + 0.0003 * f64::from(*seed);
                    let random = rng::rand_at_time(sample_time, random_seed);
                    let index =
                        ((random * patterns.len() as f64).floor() as usize).min(patterns.len() - 1);
                    let cycle = state.set_span(span);
                    patterns[index].answer_child_onto(thread, &cycle, out);
                    let count = out.len() - from;
                    if thread.stop_requested() || !thread.charge(count) {
                        out.truncate(from);
                        return;
                    }
                    thread.note_accumulated(count);
                }
            }

            // A span in one cycle is one piece, which the pattern answers
            // where the haps of the walk are; multi-cycle spans use a stack
            // split when covering at most 8 cycles and write straight onto `out`.
            Node::SplitQueries(..) | Node::SplitQueriesBoxed(..) => {
                if let Node::SplitQueries(cell, flags) = self.node_kind() {
                    if state.span.within_one_cycle() {
                        match &cell.kind {
                            Node::SlowCat(pats) => slowcat_cycle_onto(thread, pats, state, out),
                            Node::SlowCatPure(cells) => {
                                slowcat_cycle_pure_onto(thread, cells, state, out);
                            }
                            _ if (*flags & FLAG_VOLATILE) == 0 => {
                                Pattern::query_plain_cell_onto(cell, thread, state, out);
                            }
                            _ => {
                                plain_pattern(cell, *flags).answer_child_onto(thread, state, out);
                            }
                        }
                        return;
                    }
                    if let Node::SlowCatPure(cells) = &cell.kind {
                        if state.span.begin < state.span.end {
                            let Some(est_cycles) = state
                                .span
                                .cycle_count()
                                .filter(|count| *count <= MAX_QUERY_SPAN_CYCLES)
                                .and_then(|count| usize::try_from(count).ok())
                            else {
                                refuse(QueryLimit::QuerySpan {
                                    cycles: MAX_QUERY_SPAN_CYCLES,
                                });
                                return;
                            };
                            let from = out.len();
                            let mut cur = state.span.begin;
                            let mut first_cycle = true;
                            while cur < state.span.end {
                                let next = cur.next_sam().min(state.span.end);
                                let cycle = state.set_span(TimeSpan::new(cur, next));
                                slowcat_cycle_pure_onto(thread, cells, &cycle, out);
                                let count = out.len() - from;
                                if thread.stop_requested() || !thread.charge(count) {
                                    out.truncate(from);
                                    return;
                                }
                                thread.note_accumulated(count);
                                if first_cycle {
                                    first_cycle = false;
                                    if count > 0 && est_cycles > 2 {
                                        let want = count.saturating_mul(est_cycles - 1).min(65_536);
                                        out.reserve(want);
                                    }
                                }
                                cur = next;
                            }
                            return;
                        }
                        let small;
                        let heap;
                        let pieces: &[TimeSpan] =
                            if let Some((buf, len)) = state.span.span_cycles_small() {
                                small = buf;
                                &small[..len]
                            } else {
                                heap = state.span.span_cycles();
                                &heap
                            };
                        let from = out.len();
                        for &sub in pieces {
                            let cycle = state.set_span(sub);
                            slowcat_cycle_pure_onto(thread, cells, &cycle, out);
                            let count = out.len() - from;
                            if thread.stop_requested() || !thread.charge(count) {
                                out.truncate(from);
                                return;
                            }
                            thread.note_accumulated(count);
                        }
                        return;
                    }
                }
                let plain_owned;
                let pat = match self.node_kind() {
                    Node::SplitQueries(cell, flags) => {
                        plain_owned = plain_pattern(cell, *flags);
                        &plain_owned
                    }
                    Node::SplitQueriesBoxed(pat) => pat,
                    _ => unreachable!(),
                };
                let concatenated = match pat.node_kind() {
                    Node::SlowCat(pats) if !pat.has_runtime_settings() => Some(Ok(pats.as_ref())),
                    Node::SlowCatPure(cells) if !pat.has_runtime_settings() => {
                        Some(Err(cells.as_ref()))
                    }
                    _ => None,
                };
                if state.span.within_one_cycle() {
                    match concatenated {
                        Some(Ok(pats)) => slowcat_cycle_onto(thread, pats, state, out),
                        Some(Err(cells)) => slowcat_cycle_pure_onto(thread, cells, state, out),
                        None => {
                            pat.answer_child_onto(thread, state, out);
                        }
                    }
                    return;
                }
                let small;
                let heap;
                let pieces: &[TimeSpan] = if let Some((buf, len)) = state.span.span_cycles_small() {
                    small = buf;
                    &small[..len]
                } else {
                    heap = state.span.span_cycles();
                    &heap
                };
                // A graph of discrete haps answers for all the cycles in one query.
                if pieces.len() > 1
                    && pat.is_discrete()
                    && pat.asked_for_parts_onto(thread, pieces, state, out)
                {
                    return;
                }
                let from = out.len();
                for &sub in pieces {
                    let cycle = state.set_span(sub);
                    match concatenated {
                        Some(Ok(pats)) => slowcat_cycle_onto(thread, pats, &cycle, out),
                        Some(Err(cells)) => slowcat_cycle_pure_onto(thread, cells, &cycle, out),
                        None => {
                            pat.answer_child_onto(thread, &cycle, out);
                        }
                    }
                    let count = out.len() - from;
                    if thread.stop_requested() || !thread.charge(count) {
                        out.truncate(from);
                        return;
                    }
                    thread.note_accumulated(count);
                }
            }

            Node::Join(cell, flags, mode) => {
                query_join_cell_onto(cell, *flags, mode, thread, state, out);
            }

            Node::JoinBoxed(boxed) => {
                query_join_onto(&boxed.0, &boxed.1, thread, state, out);
            }

            Node::InnerJoin(boxed) => {
                query_inner_join_onto(&boxed.0, &boxed.1, thread, state, out);
            }

            Node::AppBoth(node) => {
                let AppNode(pat_func, pat_val, apply, lookup_flow) = &**node;
                query_app_both_onto(pat_func, pat_val, apply, *lookup_flow, thread, state, out);
            }

            _ => hand_over(self.query_node(state), out),
        }
    }

    /// The haps of this node, of a kind that makes its list.
    pub(crate) fn query_node(&self, state: &State) -> Vec<Hap> {
        match self.node_kind() {
            // A kind that writes its haps after those of a list.
            Node::Silence
            | Node::Pure(_)
            | Node::PureAt(_)
            | Node::PurePickLookup(..)
            | Node::Sequence(_)
            | Node::Fast(..)
            | Node::FastSmall(..)
            | Node::Shift(..)
            | Node::ShiftSmall(..)
            | Node::Cpm(..)
            | Node::FilterValues(..)
            | Node::FilterHaps(..)
            | Node::Degrade(..)
            | Node::Stack(_)
            | Node::StackPure(_)
            | Node::AddContext(..)
            | Node::AppLeft(..)
            | Node::AppWave(..)
            | Node::AppRight(..)
            | Node::AppBoth(..)
            | Node::Pick(..)
            | Node::SlowCat(_)
            | Node::SlowCatPure(_)
            | Node::SlowCatPrime(_)
            | Node::SlowCatPrimePure(_)
            | Node::WithQueryTime(..)
            | Node::WithHapTime(..)
            | Node::WithQuerySpan(..)
            | Node::WithHapSpan(..)
            | Node::Rev(..)
            | Node::FmapNative(..)
            | Node::ExpandHapsNative(..)
            | Node::Signal(..)
            | Node::ControlMap(..)
            | Node::ControlMapBoxed(..)
            | Node::MapHapsNative(..)
            | Node::MapHapsOwned(..)
            | Node::UiVisual(..)
            | Node::ChooseCycles(..)
            | Node::SplitQueries(..)
            | Node::SplitQueriesBoxed(..)
            | Node::Join(..)
            | Node::JoinBoxed(..)
            | Node::InnerJoin(..)
            | Node::Constants(..) => {
                let mut haps = Vec::new();
                QUERY_THREAD.with(|thread| self.query_node_onto(thread, state, &mut haps));
                haps
            }

            Node::QueryError(message) => {
                signal_query_error(|| message.to_string());
                Vec::new()
            }

            Node::QueryLimit(limit) => {
                refuse((**limit).clone());
                Vec::new()
            }

            Node::WithQuerySpanJs(boxed) => {
                let (pat, id) = (&boxed.0, boxed.1);
                let span = host_call_span_transform(id, state.span);
                if query_error_pending() {
                    Vec::new()
                } else {
                    pat.query(&state.set_span(span))
                }
            }

            Node::FilterValuesJs(boxed) => {
                let (pat, id) = (&boxed.0, boxed.1);
                let mut output = Vec::new();
                for hap in pat.query(state) {
                    // A throwing predicate CONTAINS the failure and keeps the
                    // hap (fail open): one broken filter must not stop the set.
                    let keep = host_call_value_predicate(id, &hap.value);
                    if query_error_pending() {
                        return Vec::new();
                    }
                    if keep {
                        output.push(hap);
                    }
                }
                output
            }

            Node::FilterHapsJs(boxed) => {
                let (pat, id) = (&boxed.0, boxed.1);
                let mut output = Vec::new();
                for hap in pat.query(state) {
                    // A throwing predicate CONTAINS the failure and keeps the
                    // hap (fail open): one broken filter must not stop the set.
                    let keep = host_call_hap_predicate(id, &hap);
                    if query_error_pending() {
                        return Vec::new();
                    }
                    if keep {
                        output.push(hap);
                    }
                }
                output
            }

            Node::FilterWhenJs(boxed) => {
                let (pat, id) = (&boxed.0, boxed.1);
                let mut output = Vec::new();
                for hap in pat.query(state) {
                    let Some(whole) = hap.whole else {
                        signal_query_error(|| {
                            "Cannot read properties of undefined (reading 'begin')".into()
                        });
                        return Vec::new();
                    };
                    let keep = host_call_time_predicate(id, whole.begin);
                    if query_error_pending() {
                        return Vec::new();
                    }
                    if keep {
                        output.push(hap);
                    }
                }
                output
            }

            Node::SortHapsByPart(pat) => sort_haps_by_part(pat.query(state)),

            Node::MapHapsWithState(boxed) => boxed
                .0
                .query(state)
                .into_iter()
                .filter_map(|hap| (boxed.1)(state, &hap))
                .collect(),

            Node::Collect(pat) => collect_congruent(pat.query(state))
                .into_iter()
                .filter_map(|group| {
                    let carrier = group.first()?;
                    let ui_visuals = group
                        .iter()
                        .fold(0_u64, |visuals, hap| visuals | hap.ui_visuals_context());
                    // Every span in the group, not the carrier's alone: a
                    // collected chord is heard as one thing and was typed
                    // as several, and an editor lighting only the first of
                    // them says a note sounded that did not.
                    let mut context: Vec<(usize, usize)> =
                        group.iter().flat_map(|hap| hap.context.iter()).collect();
                    context.sort_unstable();
                    context.dedup();
                    Some(
                        Hap::new(
                            carrier.whole,
                            carrier.part,
                            Value::Haps(value::HapList::new(group)),
                        )
                        .with_ui_visuals_context(ui_visuals)
                        .with_context(context),
                    )
                })
                .collect(),

            Node::CycleOnsetIndex(pat) => query_cycle_onset_index(pat, state),
            Node::CycleOnsetCount(pat) => query_cycle_onset_count(pat, state),

            // Extension implementations may rewrite public Hap values. They
            // have no provenance contract, so only later direct controls bind.
            Node::Extension(extension) => extension
                .query(state)
                .into_iter()
                .map(Hap::without_live_controls)
                .collect(),

            // Out of line: locals in query_node's frame multiply across the
            // recursion (see query_wchoose's note; the 64-layer fast/slow
            // control aborts otherwise).
            Node::WithRandSeed(boxed) => query_with_rand_seed(&boxed.0, boxed.1, state),

            Node::LoopAt(boxed) => {
                let cps = state
                    .controls
                    .get("_cps")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.5);
                combinators::loop_at_cps(&boxed.0, boxed.1, cps).query(state)
            }
            Node::MidiKeys(boxed) => query_midi_keys(&boxed.0, &boxed.1, state),

            Node::WChoose(choice) => query_wchoose(
                &choice.chooser,
                &choice.total,
                &choice.cumulative,
                &choice.values,
                choice.inner,
                state,
            ),

            Node::PickPatternified {
                patterns,
                index_mode,
                mode,
            } => query_patternified_pick(&patterns.0, &patterns.1, *index_mode, *mode, state),

            // See `Pattern::fast_gap`.
            Node::FastGap(boxed) => {
                let (pat, factor) = (&boxed.0, boxed.1);
                let span = state.span;
                let cycle = span.begin.sam();
                let one = Fraction::ONE;
                // Checked on the way IN as well as out: the span scaled by the
                // factor overflows just as readily as the haps scaled back by
                // it, and only fixing the mapping left this side still able to
                // panic.
                let Some(inner_span) = (|| {
                    let bpos = span.begin.checked_sub(cycle)?.checked_mul(factor)?.min(one);
                    let epos = span.end.checked_sub(cycle)?.checked_mul(factor)?.min(one);
                    // `if (bpos >= 1) return undefined` - drops zero-width
                    // queries landing at the start of the next cycle.
                    if bpos >= one {
                        return None;
                    }
                    Some(TimeSpan::new(
                        cycle.checked_add(bpos)?,
                        cycle.checked_add(epos)?,
                    ))
                })() else {
                    // An unrepresentable span and the empty `bpos >= 1` case
                    // both land here. The empty case is ordinary and silent;
                    // an overflow additionally records a refusal below, so the
                    // two stay distinguishable to the caller.
                    if span
                        .begin
                        .checked_sub(cycle)
                        .and_then(|d| d.checked_mul(factor))
                        .is_none()
                    {
                        refuse(QueryLimit::NativeFraction {
                            operation: "fastGap",
                        });
                    }
                    return Vec::new();
                };
                // The division by `factor` can leave the representable range:
                // a very small factor scales a span into numerators no i128
                // can hold. An unchecked overflow would panic, which ends the
                // set, not the query.
                //
                // Refuse as a resource limit instead, as the stepwise family
                // does when its exact arithmetic leaves the native range.
                // Refuse the whole node rather than drop the offending hap:
                // a partial span plays wrong events with no error, while a
                // refusal is visible and the last good score keeps playing.
                let mapped: Option<Vec<Hap>> = pat
                    .query(&state.set_span(inner_span))
                    .into_iter()
                    .map(|hap| Self::fast_gap_unmap_hap(&hap, factor))
                    .collect();
                match mapped {
                    Some(haps) => haps,
                    None => {
                        refuse(QueryLimit::NativeFraction {
                            operation: "fastGap",
                        });
                        Vec::new()
                    }
                }
            }

            Node::RepeatCycles(boxed) => {
                let (pattern, count) = (&boxed.0, boxed.1);
                // Every shift is checked: a span at the far edge of the
                // native range refuses rather than unwinds, as `fastGap`
                // does.
                let refuse_shift = || {
                    refuse(QueryLimit::NativeFraction {
                        operation: "repeatCycles",
                    });
                };
                let cycle = state.span.begin.sam();
                let Some(delta) = cycle
                    .checked_div(count)
                    .and_then(|source| cycle.checked_sub(source.sam()))
                else {
                    refuse_shift();
                    return Vec::new();
                };
                let shifted = state.with_span(|span| {
                    match (span.begin.checked_sub(delta), span.end.checked_sub(delta)) {
                        (Some(begin), Some(end)) => TimeSpan::new(begin, end),
                        _ => {
                            refuse_shift();
                            *span
                        }
                    }
                });
                pattern
                    .query(&shifted)
                    .into_iter()
                    .map(|mut hap| {
                        hap.update_span(|span| {
                            span.with_time(|time| match time.checked_add(delta) {
                                Some(shifted) => shifted,
                                None => {
                                    refuse_shift();
                                    time
                                }
                            })
                        });
                        hap
                    })
                    .collect()
            }

            Node::FmapJs(boxed) => map_js_haps(boxed.0.query(state), boxed.1, false, None),
            Node::FmapJsPreservingControls(boxed) => {
                map_js_haps(boxed.0.query(state), boxed.1, true, boxed.2)
            }

            Node::JsQuery(id) => host_call_query(*id, state),

            Node::Timeline(pair, timelines) => {
                let PatternPair(time_pattern, pattern) = &**pair;
                let scheduler = state.controls.get("cyclist").is_some_and(Value::js_truthy);
                let mut result = Vec::new();
                for time_hap in time_pattern.query(state) {
                    let key = pick_property_key(&time_hap.value);
                    let is_zero = matches!(&time_hap.value, Value::F64(value) if *value == 0.0);
                    let offset = if is_zero {
                        Fraction::ZERO
                    } else if let Some(offset) = timelines.get(&key) {
                        offset
                    } else {
                        let arc = time_hap.whole_or_part();
                        if !scheduler || state.span.begin < arc.midpoint() {
                            arc.begin
                        } else {
                            arc.end
                        }
                    };
                    if scheduler {
                        timelines.insert(key, offset);
                        if !is_zero {
                            let negative = Value::F64(-pick_js_number(&time_hap.value));
                            timelines.remove(&pick_property_key(&negative));
                        }
                    }
                    for hap in pattern.late(offset).query(&state.set_span(time_hap.part)) {
                        let mut context = hap.context.clone();
                        context.append(&time_hap.context);
                        if !push_budgeted(
                            &mut result,
                            Hap {
                                whole: hap.whole,
                                part: hap.part,
                                keys: ContextKeys::merge_with(
                                    &time_hap.keys,
                                    &hap.keys,
                                    hap.live_controls(),
                                    hap.slider_binding(),
                                    hap.pick_lookup_arc(),
                                ),
                                value: hap.value,
                                context,
                            },
                        ) {
                            return Vec::new();
                        }
                    }
                }
                result
            }

            // A `PatternOf*` queried without a join would yield PATTERN-VALUED
            // haps, which nothing downstream can serialise; the node resolves
            // only through a join. Routing the failure through
            // `signal_query_error` matters because the shape is reachable from
            // user-authored source (`pure(pure("bd")).polyJoin()` after
            // transpilation), and a live-coded mistake must not abort the
            // process.
            Node::PatternOfPure(..)
            | Node::PatternOfPureOperand(..)
            | Node::PatternOfDynamic(..)
            | Node::PatternOfHap(..) => {
                signal_query_error(pattern_valued_hap_error);
                Vec::new()
            }

            Node::XenPatterns(boxed) => xen::query_xen_patterns(&boxed.0, &boxed.1, state),

            // The outer pattern is queried over CYCLE ZERO after shifting, so
            // the slice boundaries are always cycle-relative.
            Node::StepJoin(pat) => {
                let sam = state.span.begin.sam();
                let shifted = pat.early(sam);
                let cycle = state.set_span(TimeSpan::new(Fraction::ZERO, Fraction::ONE));
                let resolved = shifted.resolve_for_join(&cycle);
                if budget_exhausted() {
                    return Vec::new();
                }
                // Slicing calls `value.withHap(...)`, so a hap whose value
                // is not a pattern is a `TypeError` here. At query time that
                // reaches `queryArc` and empties the result; at construction
                // time it escapes as a real throw - see `try_step_join`.
                let resolved = match ResolvedHap::all_patterns(resolved) {
                    Ok(resolved) => resolved,
                    Err(value) => {
                        signal_query_error(|| step_join_value_error(&value));
                        return Vec::new();
                    }
                };
                step_slices_pattern(&resolved).query(state)
            }

            // outerJoin semantics: the OUTER hap's whole survives.
            Node::PolyJoin(pat) => {
                let resolved = pat.resolve_for_join(state);
                // A non-pattern value reads a missing step count, which the
                // fraction layer turns into `Division by Zero`; `queryArc`
                // catches it. `polyJoin`'s `fmap` is lazy, so unlike
                // `stepJoin` this is always a query-time failure, never a
                // construction one.
                let resolved = match ResolvedHap::all_patterns(resolved) {
                    Ok(resolved) => resolved,
                    Err(_) => {
                        signal_query_error(|| "Division by Zero".into());
                        return Vec::new();
                    }
                };
                let outer_steps = pat.steps();
                let mut out = Vec::new();
                for (outer, inner) in resolved {
                    let extended = poly_extend(&inner, outer_steps);
                    for b in extended.query(&state.set_span(outer.part)) {
                        let mut ctx = outer.context.clone();
                        ctx.append(&b.context);
                        if !push_budgeted(
                            &mut out,
                            Hap {
                                whole: outer.whole,
                                part: b.part,
                                keys: ContextKeys::merge(&b.keys, &outer.keys),
                                value: b.value.clone(),
                                context: ctx,
                            },
                        ) {
                            return Vec::new();
                        }
                    }
                }
                out
            }

            Node::Arp(pair) => {
                let PatternPair(pat, indices) = &**pair;
                let mut out = Vec::new();
                // `collect()`: congruent haps - equal whole, analog haps
                // grouping together - in first-seen order, which is the order
                // `groupHapsBy` produces and therefore the chord's voice order.
                for group in collect_congruent(pat.query(state)) {
                    // The carrier hap has no context of its own, so
                    // `innerJoin` contributes only the index hap's context.
                    let carrier = &group[0];
                    for index_hap in indices.query(&state.set_span(carrier.part)) {
                        // Negative integers wrap; a fractional index misses
                        // and the following dereference throws.
                        let Some(picked) = js_index(&index_hap.value, group.len()) else {
                            signal_query_error(|| {
                                "Cannot read properties of undefined (reading 'value')".into()
                            });
                            return Vec::new();
                        };
                        let picked = &group[picked];
                        // `withHap`: the joined hap's context first, then the
                        // selected hap's - `h.combineContext(h.value)`.
                        let mut context = index_hap.context.clone();
                        context.append(&picked.context);
                        if !push_budgeted(
                            &mut out,
                            Hap {
                                // `innerJoin`: the INNER (index) hap's whole.
                                whole: index_hap.whole,
                                part: index_hap.part,
                                keys: ContextKeys::merge(&picked.keys, &index_hap.keys),
                                value: picked.value.clone(),
                                context,
                            },
                        ) {
                            return Vec::new();
                        }
                    }
                }
                out
            }

            Node::ArpWith(boxed) => {
                let (pat, id) = (&boxed.0, &boxed.1);
                let mut out = Vec::new();
                for group in collect_congruent(pat.query(state)) {
                    let carrier = &group[0];
                    // `fmap((v) => reify(func(v))).innerJoin()`: the callback
                    // result is a real pattern and is queried over the
                    // collected chord's part. Its haps provide the joined
                    // timing and their VALUES are Hap-shaped objects.
                    let selected = host_call_haps(*id, &group);
                    let Some(_recursion) = CallbackRecursionGuard::enter() else {
                        signal_query_error(|| "Maximum call stack size exceeded".into());
                        return Vec::new();
                    };
                    for inner in selected.query(&state.set_span(carrier.part)) {
                        let materialized = materialize_js_value(&inner.value);
                        let Some(object) = materialized.as_object() else {
                            // `null.value` / `undefined.value` throw at the
                            // first dereference. Other primitives are boxed,
                            // yield `undefined` for `.value`, then fail in
                            // `combineContext` at `b.context.locations`.
                            let message = match materialized {
                                Value::Null => "Cannot read properties of null (reading 'value')",
                                Value::Undefined => {
                                    "Cannot read properties of undefined (reading 'value')"
                                }
                                _ => "Cannot read properties of undefined (reading 'locations')",
                            };
                            signal_query_error(|| message.into());
                            return Vec::new();
                        };
                        // `h.value.value`: a missing `value` property is legal
                        // JavaScript and produces an undefined-valued hap.
                        let value = object.get("value").cloned().unwrap_or(Value::Undefined);
                        // `h.combineContext(h.value)` reads
                        // `b.context.locations`. Missing/undefined/null throws;
                        // other primitives are boxed by JavaScript and simply
                        // contribute no `locations` property.
                        let context = match object.get("context") {
                            None | Some(Value::Undefined) => {
                                signal_query_error(|| {
                                    "Cannot read properties of undefined (reading 'locations')"
                                        .into()
                                });
                                return Vec::new();
                            }
                            Some(Value::Null) => {
                                signal_query_error(|| {
                                    "Cannot read properties of null (reading 'locations')".into()
                                });
                                return Vec::new();
                            }
                            Some(Value::Object(context)) => Some(context),
                            Some(_) => None,
                        };
                        let mut locations = inner.context.clone();
                        if let Some(Value::List(items)) =
                            context.and_then(|context| context.get("locations"))
                        {
                            let mut added = Vec::new();
                            for item in items.iter() {
                                let Some(location) = item.as_object() else {
                                    continue;
                                };
                                let Some(start) = location.get("start").and_then(Value::as_f64)
                                else {
                                    continue;
                                };
                                let Some(end) = location.get("end").and_then(Value::as_f64) else {
                                    continue;
                                };
                                if start.is_finite()
                                    && end.is_finite()
                                    && start >= 0.0
                                    && end >= 0.0
                                    && start.fract() == 0.0
                                    && end.fract() == 0.0
                                    && start <= usize::MAX as f64
                                    && end <= usize::MAX as f64
                                {
                                    added.push((start as usize, end as usize));
                                }
                            }
                            // One extend: past the inline ranges, each call
                            // copies the whole list.
                            locations.extend_from_slice(&added);
                        }
                        if !push_budgeted(
                            &mut out,
                            Hap {
                                whole: inner.whole,
                                part: inner.part,
                                value,
                                context: locations,
                                // `combineContext(h.value)`: each key on the
                                // value-side context wins, and the receiving
                                // hap keeps each key that side lacks. `.log()`
                                // writes its formatted text under `logLine`,
                                // which the runtime prints on the onset.
                                keys: ContextKeys::merge_with(
                                    &ContextKeys::of(context_keys::Keys {
                                        scale: context
                                            .and_then(|context| context.get("scale"))
                                            .and_then(|scale| match scale {
                                                Value::Str(name) => Some(name.to_arc()),
                                                _ => None,
                                            }),
                                        edo_size: context
                                            .and_then(|context| context.get("edoSize"))
                                            .and_then(Value::as_f64),
                                        scale_definition: context
                                            .and_then(|context| context.get("scaleDefinition"))
                                            .cloned()
                                            .map(Arc::new),
                                        tags: context
                                            .and_then(|context| context.get("tags"))
                                            .and_then(tags_from_value),
                                        log_line: context
                                            .and_then(|context| context.get("logLine"))
                                            .and_then(|value| match value {
                                                Value::Str(text) => Some(text.to_arc()),
                                                _ => None,
                                            }),
                                        ..Default::default()
                                    }),
                                    &inner.keys,
                                    [0; 4],
                                    0,
                                    None,
                                ),
                            },
                        ) {
                            return Vec::new();
                        }
                    }
                    if query_error_pending() {
                        return Vec::new();
                    }
                }
                out
            }

            Node::ArpWithPattern(pair) => {
                let PatternPair(pat, callbacks) = &**pair;
                let mut out = Vec::new();
                for callback_hap in callbacks.query(state) {
                    let Value::Function(function) = &callback_hap.value else {
                        signal_query_error(|| "func is not a function".into());
                        return Vec::new();
                    };
                    let Some(id) = function.callback_id() else {
                        signal_query_error(|| "func is not a function".into());
                        return Vec::new();
                    };
                    let selected =
                        arp_with(pat.clone(), id).query(&state.set_span(callback_hap.part));
                    if query_error_pending() {
                        return Vec::new();
                    }
                    for mut hap in selected {
                        let mut context = callback_hap.context.clone();
                        context.append(&hap.context);
                        hap.context = context;
                        if !push_budgeted(&mut out, hap) {
                            return Vec::new();
                        }
                    }
                }
                out
            }

            // Pattern-valued haps, as above. Reachable from user source:
            // `pure(pure("bd")).polyJoin()` transpiles to a THREE-level nest,
            // so the join flattens one level and leaves a `PurePattern` to be
            // queried directly.
            Node::PurePattern(_) | Node::PatternOfJs(..) => {
                signal_query_error(pattern_valued_hap_error);
                Vec::new()
            }
        }
    }

    /// Query a `PatternOf*` (possibly behind time transforms) and resolve each
    /// hap's inner pattern.
    ///
    /// `Join` does this inline, but `stepJoin` needs the inner patterns as
    /// values so it can slice and `stepcat` them. Top-level pattern-of-pattern
    /// nodes retain their exact timing here rather than being flattened.
    ///
    /// `None` means "this node is not a pattern-of-patterns at all", which is
    /// a different thing from a pattern-of-patterns that resolves to nothing.
    /// Callers that just want the haps a join consumes want
    /// [`Pattern::resolve_for_join`] instead.
    fn resolve_pattern_of(&self, state: &State) -> Option<Vec<ResolvedHap>> {
        let resolve = || {
            let mut resolved = self.resolve_pattern_of_inner(state);
            if let (Some(settings), Some(resolved)) = (self.runtime_settings(), &mut resolved) {
                for hap in resolved {
                    if let Some(inner) = &mut hap.inner {
                        inner.attach_runtime_settings(settings.clone());
                    }
                }
            }
            resolved
        };
        match self.runtime_settings() {
            Some(settings) => settings.with(resolve),
            None => resolve(),
        }
    }

    pub(crate) fn resolve_pattern_of_inner(&self, state: &State) -> Option<Vec<ResolvedHap>> {
        match self.node_kind() {
            Node::PatternOfPure(boxed) => {
                let (outer, f, memo) = &**boxed;
                let mut resolved = take_resolved_list();
                let settings = settings::current_state_id();
                let ok = with_list(|outer_haps| {
                    QUERY_THREAD.with(|thread| {
                        outer.answer_child_onto(thread, state, outer_haps);
                    });
                    for hap in outer_haps.drain(..) {
                        let inner = memo
                            .construct(&hap.value, &settings, |value| f(value))
                            .pattern()
                            .clone();
                        if query_error_pending() || budget_exhausted() {
                            return false;
                        }
                        resolved.push(ResolvedHap::of(hap, inner));
                    }
                    true
                });
                if !ok {
                    resolved.clear();
                }
                Some(resolved)
            }
            Node::PatternOfPureOperand(boxed, map) => {
                let OperandPureNode(outer, operand, memo) = &**boxed;
                let mut resolved = take_resolved_list();
                let settings = settings::current_state_id();
                let ok = with_list(|outer_haps| {
                    QUERY_THREAD.with(|thread| {
                        outer.answer_child_onto(thread, state, outer_haps);
                    });
                    for mut hap in outer_haps.drain(..) {
                        let inner = memo
                            .construct(&hap.value, &settings, |value| map.apply(value, operand))
                            .pattern()
                            .clone();
                        if query_error_pending() || budget_exhausted() {
                            return false;
                        }
                        if hap.pick_lookup().is_some() {
                            hap.keys.edit().pick_lookup = None;
                            hap.keys.trim();
                        }
                        resolved.push(ResolvedHap::of(hap, inner));
                    }
                    true
                });
                if !ok {
                    resolved.clear();
                }
                Some(resolved)
            }
            Node::PatternOfDynamic(boxed) => {
                let (outer, f) = (&boxed.0, &boxed.1);
                let mut resolved = take_resolved_list();
                let ok = with_list(|outer_haps| {
                    QUERY_THREAD.with(|thread| {
                        outer.answer_child_onto(thread, state, outer_haps);
                    });
                    for hap in outer_haps.drain(..) {
                        let inner = f(&hap.value);
                        // Iteration unwinds on the first throwing callback, as
                        // Array.map would. A host callback records that throw in the
                        // shared query-error channel and returns silence so Rust
                        // itself stays panic-free; stop HERE or later carrier haps
                        // would still expose user side effects before queryArc
                        // eventually discarded the answer.
                        if query_error_pending() || budget_exhausted() {
                            return false;
                        }
                        resolved.push(ResolvedHap::of(hap, inner));
                    }
                    true
                });
                if !ok {
                    resolved.clear();
                }
                Some(resolved)
            }
            Node::PatternOfHap(bind) => {
                let HapBind(outer, _, f) = &**bind;
                let mut resolved = take_resolved_list();
                let ok = with_list(|outer_haps| {
                    QUERY_THREAD.with(|thread| {
                        outer.answer_child_onto(thread, state, outer_haps);
                    });
                    for hap in outer_haps.drain(..) {
                        let inner = f(&hap);
                        if query_error_pending() || budget_exhausted() {
                            return false;
                        }
                        resolved.push(ResolvedHap::of(hap, inner));
                    }
                    true
                });
                if !ok {
                    resolved.clear();
                }
                Some(resolved)
            }
            // `pure(pattern)`: one hap per cycle, each carrying the same known
            // inner pattern. Mirrors `Node::Pure`'s query exactly, including
            // its pre-charge: if every inner pattern is silent, the join's
            // output stays empty and no later vector exists for the generic
            // check to catch, so the charge belongs here.
            Node::PurePattern(inner) => {
                let spans = state.span.span_cycles();
                if !charge_haps(spans.len()) {
                    return Some(Vec::new());
                }
                note_materialised(spans.len());
                Some(
                    spans
                        .into_iter()
                        .map(|subspan| {
                            ResolvedHap::of(
                                Hap::new(
                                    Some(whole_cycle(subspan.begin)),
                                    subspan,
                                    Value::Undefined,
                                ),
                                (**inner).clone(),
                            )
                        })
                        .collect(),
                )
            }

            // `fmap(func)` with a user callback. `fmap` is representation-
            // agnostic: whatever the outer hap carries is what `func`
            // receives. After transpilation the outer is often itself
            // pattern-valued - `pure("bd")` becomes a pattern-of-patterns
            // while `pure('bd')` stays a string - and the callback must then
            // be handed a PATTERN. Squeezing that through `Value` is
            // impossible by design, so the two cases dispatch on the outer's
            // representation, per hap. The single/double quote distinction
            // is the transpiler's.
            Node::PatternOfJs(boxed) => {
                let (outer, id) = (&boxed.0, &boxed.1);
                let mut out_haps = Vec::new();
                match outer.resolve_pattern_of(state) {
                    Some(resolved) => {
                        for resolved in resolved {
                            let out = match resolved.inner {
                                Some(inner) => host_call_bind(*id, BindArg::Pattern(inner)),
                                // A nested level that is itself not a pattern:
                                // the hap's own value is what the callback gets.
                                None => host_call_bind(*id, BindArg::Value(&resolved.hap.value)),
                            };
                            if query_error_pending() || budget_exhausted() {
                                return Some(Vec::new());
                            }
                            out_haps.push(ResolvedHap::from_bind(resolved.hap, out));
                        }
                    }
                    None => {
                        for hap in outer.query(state) {
                            let out = host_call_bind(*id, BindArg::Value(&hap.value));
                            if query_error_pending() || budget_exhausted() {
                                return Some(Vec::new());
                            }
                            out_haps.push(ResolvedHap::from_bind(hap, out));
                        }
                    }
                }
                Some(out_haps)
            }

            // Unwrap through the ordinary structure/time transformations a
            // pattern-of-patterns may be wrapped in before the join. Matching
            // only the bare node made `polyJoin` return silence for anything
            // that had been `fast`ed, `early`d or split.
            Node::SplitQueries(cell, flags) => {
                plain_pattern(cell, *flags).resolve_pattern_of(state)
            }
            Node::SplitQueriesBoxed(inner) => inner.resolve_pattern_of(state),
            Node::AddContext(boxed) => boxed.0.resolve_pattern_of(state).map(|resolved| {
                let locs = &boxed.1;
                resolved
                    .into_iter()
                    .map(|mut resolved| {
                        resolved.hap.context.extend_from_slice(locs);
                        resolved
                    })
                    .collect()
            }),
            Node::WithQueryTime(boxed) => boxed
                .0
                .resolve_pattern_of(&state.with_span(|sp| sp.with_time(|t| (boxed.1)(t)))),
            // `rev` reflects the query span and then reflects the haps back, so
            // it cannot delegate the way the time transforms above do: the
            // same reflection applies on both sides. Without this arm,
            // `pure("bd sd").rev().polyJoin()` falls through to a plain query
            // of the pattern-of-patterns and produces nothing instead of its
            // two haps.
            Node::Rev(inner) => {
                let cycle = state.span.begin.sam();
                let next_cycle = state.span.begin.next_sam();
                let reflect = |sp: &TimeSpan| {
                    let b = cycle.add(next_cycle.sub(sp.begin));
                    let e = cycle.add(next_cycle.sub(sp.end));
                    TimeSpan::new(e, b)
                };
                inner
                    .resolve_pattern_of(&state.set_span(reflect(&state.span)))
                    .map(|resolved| {
                        resolved
                            .into_iter()
                            .map(|resolved| ResolvedHap {
                                hap: resolved.hap.with_span(reflect),
                                inner: resolved.inner,
                            })
                            .collect()
                    })
            }
            Node::WithHapTime(boxed) => boxed.0.resolve_pattern_of(state).map(|resolved| {
                resolved
                    .into_iter()
                    .map(|resolved| ResolvedHap {
                        hap: resolved.hap.with_span(|sp| sp.with_time(|t| (boxed.1)(t))),
                        inner: resolved.inner,
                    })
                    .collect()
            }),
            Node::Fast(node) => node
                .0
                .resolve_pattern_of(
                    &state.with_span(|sp| sp.with_time(|t| fast_query_time(t, node.1, node.2))),
                )
                .map(|resolved| {
                    resolved
                        .into_iter()
                        .map(|resolved| ResolvedHap {
                            hap: resolved
                                .hap
                                .with_span(|sp| sp.with_time(|t| fast_hap_time(t, node.1, node.2))),
                            inner: resolved.inner,
                        })
                        .collect()
                }),
            Node::FastSmall(cell, flags, numer, denom, is_slow) => {
                let factor = Fraction::new(i128::from(*numer), i128::from(*denom));
                let op = if *is_slow { "slow" } else { "fast" };
                plain_pattern(cell, *flags)
                    .resolve_pattern_of(
                        &state.with_span(|sp| sp.with_time(|t| fast_query_time(t, factor, op))),
                    )
                    .map(|resolved| {
                        resolved
                            .into_iter()
                            .map(|resolved| ResolvedHap {
                                hap: resolved
                                    .hap
                                    .with_span(|sp| sp.with_time(|t| fast_hap_time(t, factor, op))),
                                inner: resolved.inner,
                            })
                            .collect()
                    })
            }
            Node::Shift(node) => node
                .0
                .resolve_pattern_of(
                    &state.with_span(|sp| sp.with_time(|t| shift_query_time(t, node.1, node.2))),
                )
                .map(|resolved| {
                    resolved
                        .into_iter()
                        .map(|resolved| ResolvedHap {
                            hap: resolved.hap.with_span(|sp| {
                                sp.with_time(|t| shift_hap_time(t, node.1, node.2))
                            }),
                            inner: resolved.inner,
                        })
                        .collect()
                }),
            Node::ShiftSmall(cell, flags, numer, denom, is_early) => {
                let offset = Fraction::new(i128::from(*numer), i128::from(*denom));
                let op = if *is_early { "early" } else { "late" };
                plain_pattern(cell, *flags)
                    .resolve_pattern_of(
                        &state.with_span(|sp| sp.with_time(|t| shift_query_time(t, offset, op))),
                    )
                    .map(|resolved| {
                        resolved
                            .into_iter()
                            .map(|resolved| ResolvedHap {
                                hap: resolved.hap.with_span(|sp| {
                                    sp.with_time(|t| shift_hap_time(t, offset, op))
                                }),
                                inner: resolved.inner,
                            })
                            .collect()
                    })
            }
            // `stack` resolves each branch and concatenates, mirroring the
            // query's flatten. `seqPLoop` (and any join over a stacked
            // pattern-of-patterns) otherwise fell through to the plain-query
            // fallback and hit the pattern-valued refusal with no haps at all.
            Node::Stack(children) => Some(
                extend_budgeted(
                    children
                        .iter()
                        .map(|child| query_stack_child(|| child.resolve_for_join(state))),
                )
                // A refused concatenation resolves to silence; the refusal
                // itself travels on the query-wide channel.
                .unwrap_or_default(),
            ),
            Node::StackPure(cells) => Some(
                extend_budgeted(cells.iter().map(|cell| {
                    query_stack_child(|| plain_pattern(cell, 0).resolve_for_join(state))
                }))
                .unwrap_or_default(),
            ),
            // `fastGap` on a pattern-of-patterns: the join must consume the
            // same compressed outer windows the query path produces, or
            // `pure(p).compress(b, e).innerJoin()` - the shape `seqPLoop`
            // builds - falls through to the plain-query fallback and hits the
            // pattern-valued refusal. The in/out scaling mirrors the query
            // arm exactly.
            Node::FastGap(boxed) => {
                let (inner, factor) = (&boxed.0, boxed.1);
                let span = state.span;
                let one = Fraction::ONE;
                let mut resolved_haps = Vec::new();
                for subspan in span.span_cycles() {
                    let cycle = subspan.begin.sam();
                    let Some(inner_span) = (|| {
                        let bpos = subspan
                            .begin
                            .checked_sub(cycle)?
                            .checked_mul(factor)?
                            .min(one);
                        let epos = subspan
                            .end
                            .checked_sub(cycle)?
                            .checked_mul(factor)?
                            .min(one);
                        if bpos >= one {
                            return None;
                        }
                        Some(TimeSpan::new(
                            cycle.checked_add(bpos)?,
                            cycle.checked_add(epos)?,
                        ))
                    })() else {
                        if subspan
                            .begin
                            .checked_sub(cycle)
                            .and_then(|d| d.checked_mul(factor))
                            .is_none()
                        {
                            refuse(QueryLimit::NativeFraction {
                                operation: "fastGap",
                            });
                            return Some(Vec::new());
                        }
                        // The compressed window lies before this cycle; the
                        // query path drops it silently, so skip it here too.
                        continue;
                    };
                    let mapped: Option<Vec<ResolvedHap>> = inner
                        .resolve_pattern_of(&state.set_span(inner_span))?
                        .into_iter()
                        .map(|mut resolved| {
                            resolved.hap = Self::fast_gap_unmap_hap(&resolved.hap, factor)?;
                            Some(resolved)
                        })
                        .collect();
                    let Some(haps) = mapped else {
                        refuse(QueryLimit::NativeFraction {
                            operation: "fastGap",
                        });
                        return Some(Vec::new());
                    };
                    // The per-cycle results concatenate, so each extend is
                    // charged: the branches are individually legal while the
                    // running total is not.
                    if !charge_haps(resolved_haps.len() + haps.len()) {
                        return Some(Vec::new());
                    }
                    resolved_haps.extend(haps);
                    note_accumulated(resolved_haps.len());
                }
                Some(resolved_haps)
            }
            _ => None,
        }
    }

    /// What a join actually consumes: total where [`Pattern::resolve_pattern_of`]
    /// is partial.
    ///
    /// A receiver that is not a pattern-of-patterns still produces haps -
    /// `pure('bd').polyJoin()` queries one hap holding the string `'bd'`.
    /// `fmap` never inspects what it stores, so the failure belongs to
    /// the join that dereferences the value, and each join fails differently
    /// (`polyJoin` reads `._steps`, `stepJoin` calls `.withHap`). Reporting
    /// "not a pattern-of" from here instead would collapse those into one
    /// shape.
    pub(crate) fn resolve_for_join(&self, state: &State) -> Vec<ResolvedHap> {
        self.resolve_pattern_of(state).unwrap_or_else(|| {
            let mut resolved = take_resolved_list();
            with_list(|outer_haps| {
                QUERY_THREAD.with(|thread| {
                    self.answer_child_onto(thread, state, outer_haps);
                });
                for hap in outer_haps.drain(..) {
                    let entry = match &hap.value {
                        // Ordinary `fmap` is representation-agnostic: a
                        // callback may return a Pattern, store it in the hap, and
                        // a following join must consume it. `FmapJs` materialises
                        // that wrapper as `Value::Pattern`; treating every
                        // fallback hap as scalar made
                        // `p.fmap(x => pure(x)).innerJoin()` silently empty.
                        Value::Pattern(value) => ResolvedHap {
                            inner: Some(value.pattern().clone()),
                            hap: hap.with_value(|_| Value::Undefined),
                        },
                        _ => ResolvedHap { hap, inner: None },
                    };
                    resolved.push(entry);
                }
            });
            resolved
        })
    }

    /// The hap mapping `fastGap` applies on the way OUT: within-cycle
    /// positions divided back by the factor, clamped at the cycle boundary.
    /// Shared by the query path and the pattern-of-patterns resolution so a
    /// join consumes exactly the same compressed windows the query produces.
    pub(crate) fn fast_gap_unmap_hap(hap: &Hap, factor: Fraction) -> Option<Hap> {
        let begin = hap.part.begin;
        let end = hap.part.end;
        let cyc = begin.sam();
        let one = Fraction::ONE;
        let begin_pos = begin.checked_sub(cyc)?.checked_div(factor)?.min(one);
        let end_pos = end.checked_sub(cyc)?.checked_div(factor)?.min(one);
        let new_part = TimeSpan::new(cyc.checked_add(begin_pos)?, cyc.checked_add(end_pos)?);
        let new_whole = match hap.whole {
            Some(w) => Some(TimeSpan::new(
                new_part
                    .begin
                    .checked_sub(begin.checked_sub(w.begin)?.checked_div(factor)?)?,
                new_part
                    .end
                    .checked_add(w.end.checked_sub(end)?.checked_div(factor)?)?,
            )),
            None => None,
        };
        Some(Hap {
            whole: new_whole,
            part: new_part,
            value: hap.value.clone(),
            context: hap.context.clone(),
            keys: hap.keys.clone(),
        })
    }

    /// `queryArc(begin, end)` - the public pattern-query boundary.
    ///
    /// Also the boundary at which a mid-query failure becomes an empty
    /// result. Nested `query_arc` calls (a JS callback querying another
    /// pattern) absorb their own errors: the inner boundary catches first.
    pub fn query_arc(&self, begin: Fraction, end: Fraction) -> Vec<Hap> {
        self.query_state(&State::new(TimeSpan::new(begin, end)))
    }

    /// `queryArc`'s error boundary for callers that need to supply a full
    /// [`State`] rather than just a span - the scheduler attaches `_cps`, and
    /// the JS host attaches scheduler controls.
    ///
    /// **Use this, not `query`, at any top-level entry point.** `query` is the
    /// raw recursive step and has no boundary: a pattern whose query fails
    /// mid-way (`note("c").add("x")` reaches `parseNumeral`'s throw) hands
    /// back NaN-valued haps instead of the empty result the boundary
    /// guarantees. On the scheduling path that is not cosmetic - those haps
    /// become onset events, scheduling audio where the score must be silent.
    pub fn query_state(&self, state: &State) -> Vec<Hap> {
        self.try_query_state(state).unwrap_or_default()
    }

    /// `query_state`, distinguishing RESOURCE EXHAUSTION from an ordinary
    /// query throw.
    ///
    /// Both end in no haps, and conflating them is how a limit becomes a lie: a
    /// caller cannot tell "this pattern produces nothing" from "this pattern
    /// was refused", so a truncated render looks like a correct silent one.
    /// Exhaustion therefore travels on its own flag, which the `queryArc`
    /// boundary below deliberately does not clear.
    pub fn try_query_state(&self, state: &State) -> Result<Vec<Hap>, QueryLimit> {
        self.try_query_state_with_budget(state, DEFAULT_HAP_BUDGET)
    }

    /// One outer `queryArc` that keeps the throw distinct from empty haps.
    ///
    /// Steady-state scheduling still uses [`Self::try_query_state`] (silence
    /// for that window, clock keeps running). A live replacement must call
    /// this so a typo that constructs but throws at query time cannot replace
    /// the sounding score with silence.
    pub fn query_arc_outcome(&self, state: &State) -> Result<QueryArcOutcome, QueryLimit> {
        self.query_arc_outcome_with_budget(state, DEFAULT_HAP_BUDGET)
    }

    /// `try_query_state` with an explicit budget.
    ///
    /// Public so embedders and tests can exercise the same resource boundary
    /// with a smaller limit instead of allocating millions of haps.
    ///
    /// Also genuinely useful: an embedder scheduling on a small device can pick
    /// a tighter bound than this build's default.
    pub fn try_query_state_with_budget(
        &self,
        state: &State,
        budget: u64,
    ) -> Result<Vec<Hap>, QueryLimit> {
        match self.query_arc_outcome_with_budget(state, budget)? {
            QueryArcOutcome::Haps(haps) => Ok(haps),
            // The `queryArc` catch: this window is silent. The throw is
            // not a resource refusal and must not become scheduled NaNs.
            QueryArcOutcome::Thrown(_) => Ok(Vec::new()),
        }
    }

    /// [`Self::query_arc_outcome`] with an explicit hap budget.
    pub fn query_arc_outcome_with_budget(
        &self,
        state: &State,
        budget: u64,
    ) -> Result<QueryArcOutcome, QueryLimit> {
        self.query_arc_outcome_in(state, budget, table::Order::Graph)
    }

    /// [`Self::query_arc_outcome_with_budget`] with the haps sorted as
    /// [`sort_haps_for_query`] sorts them. A precomputed answer comes sorted
    /// already, so this is cheaper than sorting the outcome afterwards.
    pub fn query_arc_outcome_sorted_with_budget(
        &self,
        state: &State,
        budget: u64,
    ) -> Result<QueryArcOutcome, QueryLimit> {
        self.query_arc_outcome_in(state, budget, table::Order::Sorted)
    }

    fn query_arc_outcome_in(
        &self,
        state: &State,
        budget: u64,
        order: table::Order,
    ) -> Result<QueryArcOutcome, QueryLimit> {
        // A fresh top-level query clears any stale published failure and
        // establishes a `queryArc` error boundary in a single TLS access.
        let error_boundary = QueryErrorBoundary::enter_top_level();
        let bound_settings = self.runtime_settings();
        let query = || {
            with_hap_budget(budget, || {
                let (is_short, is_narrow) = if state.span.end <= state.span.begin {
                    (true, true)
                } else {
                    let b_cycle = state.span.begin.floor_int();
                    let e_cycle = state.span.end.floor_int();
                    let check_narrow = || {
                        let bn = state.span.begin.numer();
                        let bd = state.span.begin.denom();
                        let en = state.span.end.numer();
                        let ed = state.span.end.denom();
                        en.checked_mul(bd)
                            .zip(bn.checked_mul(ed))
                            .zip(bd.checked_mul(ed))
                            .and_then(|((lhs, rhs), den)| {
                                lhs.checked_sub(rhs)?
                                    .checked_mul(4)
                                    .map(|diff4| diff4 <= den)
                            })
                            .unwrap_or(false)
                    };
                    match e_cycle.checked_sub(b_cycle) {
                        Some(0) => (true, check_narrow()),
                        Some(1) => {
                            let short = state.span.end.cycle_pos() <= state.span.begin.cycle_pos();
                            (short, short && check_narrow())
                        }
                        _ => (false, false),
                    }
                };
                let root_ptr = Arc::as_ptr(&self.node) as usize;
                let default_cap = match self.node_kind() {
                    Node::Sequence(layers) => {
                        if is_narrow && layers.len() <= 8 {
                            let mut bound = 0usize;
                            for layer in layers.iter() {
                                bound = bound.saturating_add(layer.narrow_hap_bound());
                                if bound > 8 {
                                    break;
                                }
                            }
                            if bound <= 8 {
                                bound.clamp(1, 8)
                            } else {
                                16usize
                            }
                        } else {
                            16usize
                        }
                    }
                    _ => 32usize,
                };
                let (raw_query_attaches_metadata, room) = QUERY_THREAD.with(|thread| {
                    let top = thread.depth.get() == 0;
                    let room = if is_short {
                        let (ptr, cap) = thread.short_query_hint.get();
                        if ptr == root_ptr {
                            usize::from(cap)
                        } else {
                            default_cap
                        }
                    } else if match self.node_kind() {
                        Node::Sequence(_) => true,
                        Node::Fast(node) => split_slowcat_slice(node.0.node_kind()).is_some(),
                        Node::FastSmall(cell, ..) => split_slowcat_slice(&cell.kind).is_some(),
                        Node::FilterValues(boxed) => {
                            Pattern::is_keep_defined(&boxed.1)
                                && matches!(boxed.0.node_kind(), Node::AppRight(_))
                        }
                        _ => false,
                    } {
                        // These multi-cycle producers size `out` directly with
                        // `reserve_exact`, so starting empty avoids a throwaway alloc + realloc.
                        0
                    } else {
                        64
                    };
                    (top, room)
                });
                let (mut haps, data_only) = self.answer_with_room(state, order, room);
                if is_short {
                    let len = haps.len();
                    let base = default_cap as u16;
                    let needed = if len <= usize::from(base) {
                        base
                    } else if len <= 8 {
                        8u16
                    } else if len <= 16 {
                        16u16
                    } else if len <= 24 {
                        24u16
                    } else if len <= 32 {
                        32u16
                    } else if len <= 48 {
                        48u16
                    } else {
                        64u16
                    };
                    QUERY_THREAD.with(|thread| {
                        let (ptr, cap) = thread.short_query_hint.get();
                        let next = if ptr == root_ptr {
                            cap.max(needed)
                        } else {
                            needed
                        };
                        thread.short_query_hint.set((root_ptr, next));
                    });
                }
                // Pure graphs with no bound runtime cannot produce JS-owned
                // values, and tables hold data only.
                if data_only {
                    return haps;
                }
                let metadata_owner = bound_settings
                    .cloned()
                    .or_else(settings::RuntimeSettings::current_requested);
                if self.is_pure() && metadata_owner.is_none() {
                    return haps;
                }
                // JS-owned arrays/objects preserve their identity throughout the
                // graph, but the host that can resolve that identity exists only
                // during the outer query. Convert returned values here so every
                // later renderer (CLI JSON, scheduler events, Hap::show) receives
                // an ordinary host-independent Value.
                for hap in &mut haps {
                    let materialized = hap.value.contains_js_value();
                    if materialized {
                        hap.value = materialize_js_value(&hap.value);
                        hap.clear_live_bindings();
                    }
                    if let Some(settings) = &metadata_owner
                        && (materialized || !raw_query_attaches_metadata)
                        && !hap.value.attach_runtime_settings(settings, 0)
                    {
                        return Vec::new();
                    }
                }
                haps
            })
        };
        // The owner governs the complete public query operation, including
        // JS container materialisation after the recursive graph returns.
        let (haps, refusal) = match bound_settings {
            Some(settings) if !settings.is_current_requested() => settings.with(query),
            _ => query(),
        };
        let thrown = error_boundary.finish();
        // Cancellation is checked FIRST: a query stopped part way will usually
        // also look under-budget, and reporting the limit would be misleading.
        if cancellation_requested() {
            return Err(QueryLimit::Cancelled);
        }
        if let Some(limit) = refusal {
            return Err(limit);
        }
        Ok(match thrown {
            Some(message) => QueryArcOutcome::Thrown(message),
            None => QueryArcOutcome::Haps(haps),
        })
    }

    /// Sorted as Strudel's `sortHapsByPart()` does. The order is part of the
    /// serialized query output.
    ///
    /// Four sort keys: part.begin, part.end, **whole.begin, whole.end** -
    /// omitting the whole keys leaves ties in the wrong order whenever two
    /// haps share a part.
    ///
    /// # Analog haps make the comparator throw
    ///
    /// The comparator reads `whole` unconditionally, so any analog hap among
    /// two or more results makes it throw and the enclosing `queryArc` yields
    /// no haps. Substituting `whole_or_part()` would return both haps; the
    /// throw-and-silence behaviour is preserved instead. A single element is
    /// never compared, so exactly one analog hap survives untouched.
    pub fn query_arc_sorted(&self, begin: Fraction, end: Fraction) -> Vec<Hap> {
        // `query_arc` owns the recursive query's error boundary, but sorting
        // is itself observable JavaScript-compatible work: two analog haps
        // make the comparator throw. Keep a second boundary around that final
        // phase so its error becomes this call's empty result and cannot poison
        // the next independent query on the worker thread.
        let error_boundary = QueryErrorBoundary::enter();
        let haps = sort_haps_by_part(self.query_arc(begin, end));
        match error_boundary.finish() {
            Some(_) => Vec::new(),
            None => haps,
        }
    }

    /// `query_arc_sorted`, propagating a resource refusal.
    ///
    /// The untyped form above discards the refusal. That is the right default
    /// for internal call sites that cannot act on one, but not for a
    /// top-level entry point: there, a discarded refusal looks like a pattern
    /// that produced nothing.
    pub fn try_query_arc_sorted(
        &self,
        begin: Fraction,
        end: Fraction,
    ) -> Result<Vec<Hap>, QueryLimit> {
        self.try_query_arc_sorted_with_budget(begin, end, DEFAULT_HAP_BUDGET)
    }

    /// `try_query_arc_sorted` with an explicit budget. See
    /// [`Pattern::try_query_state_with_budget`].
    pub fn try_query_arc_sorted_with_budget(
        &self,
        begin: Fraction,
        end: Fraction,
        budget: u64,
    ) -> Result<Vec<Hap>, QueryLimit> {
        let state = State::new(TimeSpan::new(begin, end));
        let error_boundary = QueryErrorBoundary::enter();
        let haps = sort_haps_by_part(self.try_query_state_with_budget(&state, budget)?);
        Ok(match error_boundary.finish() {
            Some(_) => Vec::new(),
            None => haps,
        })
    }

    /// Query and sort a window while preserving query and comparator errors.
    /// The outcome distinguishes a thrown error from an empty window.
    pub fn try_query_arc_sorted_outcome_with_budget(
        &self,
        begin: Fraction,
        end: Fraction,
        budget: u64,
    ) -> Result<QueryArcOutcome, QueryLimit> {
        let state = State::new(TimeSpan::new(begin, end));
        let error_boundary = QueryErrorBoundary::enter();
        let outcome = self.query_arc_outcome_with_budget(&state, budget)?;
        Ok(match outcome {
            // Keep the boundary active until the comparator has finished.
            QueryArcOutcome::Haps(haps) => {
                let sorted = sort_haps_by_part(haps);
                match error_boundary.finish() {
                    Some(message) => QueryArcOutcome::Thrown(message),
                    None => QueryArcOutcome::Haps(sorted),
                }
            }
            // The inner query boundary already captured this error.
            thrown @ QueryArcOutcome::Thrown(_) => {
                let _ = error_boundary.finish();
                thrown
            }
        })
    }
}

#[inline]
fn query_faster_with_onto(
    thread: &QueryThread,
    factor: Fraction,
    grid: Option<&TempoGrid>,
    operation: &'static str,
    state: &State,
    out: &mut Vec<Hap>,
    query_inner: impl FnOnce(&State, &mut Vec<Hap>),
) {
    if factor == Fraction::ONE {
        query_inner(state, out);
        return;
    }
    let mul_pow2 =
        (factor.denom() == 1 && factor.numer() > 0 && (factor.numer() as u128).is_power_of_two())
            .then(|| (factor.numer() as u128).trailing_zeros());
    let div_pow2 = (factor.numer() == 1 && (factor.denom() as u128).is_power_of_two())
        .then(|| (factor.denom() as u128).trailing_zeros());
    let asked = if let Some(shift) = mul_pow2 {
        state.with_span(|span| {
            span.with_time(|t| {
                t.checked_mul_pow2(shift)
                    .unwrap_or_else(|| fast_query_time(t, factor, operation))
            })
        })
    } else if let Some(shift) = div_pow2 {
        state.with_span(|span| {
            span.with_time(|t| {
                t.checked_div_pow2(shift)
                    .unwrap_or_else(|| fast_query_time(t, factor, operation))
            })
        })
    } else {
        state.with_span(|span| span.with_time(|t| fast_query_time(t, factor, operation)))
    };
    let edges = (!thread.has_stopped_fast() && factor != Fraction::ZERO)
        .then_some((state.span.begin, state.span.end));
    let from = out.len();
    query_inner(&asked, out);
    if out.len() == from {
        return;
    }
    if let Some(shift) = mul_pow2 {
        Hap::map_times(&mut out[from..], asked.span, edges, |t| {
            t.checked_div_pow2(shift)
                .unwrap_or_else(|| fast_hap_time(t, factor, operation))
        });
    } else if let Some(shift) = div_pow2 {
        Hap::map_times(&mut out[from..], asked.span, edges, |t| {
            t.checked_mul_pow2(shift)
                .unwrap_or_else(|| fast_hap_time(t, factor, operation))
        });
    } else if let Some(grid) = grid {
        sequence::with_tick_grid_if(grid.denom(), false, |tick_grid| {
            Hap::map_times(&mut out[from..], asked.span, edges, |t| {
                grid.time_with(t, tick_grid)
                    .unwrap_or_else(|| fast_hap_time(t, factor, operation))
            });
        });
    } else {
        Hap::map_times(&mut out[from..], asked.span, edges, |t| {
            fast_hap_time(t, factor, operation)
        });
    }
}

#[inline]
fn query_moved_with_onto(
    thread: &QueryThread,
    by: Fraction,
    operation: &'static str,
    state: &State,
    out: &mut Vec<Hap>,
    query_inner: impl FnOnce(&State, &mut Vec<Hap>),
) {
    if by == Fraction::ZERO {
        query_inner(state, out);
        return;
    }
    let whole_by = (by.denom() == 1).then(|| by.numer());
    let asked = if let Some(neg) = whole_by.and_then(i128::checked_neg) {
        state.with_span(|span| span.with_time(|t| checked_time(t.checked_add_int(neg), operation)))
    } else {
        state.with_span(|span| span.with_time(|t| shift_query_time(t, by, operation)))
    };
    let edges = (!thread.has_stopped_fast()).then_some((state.span.begin, state.span.end));
    let from = out.len();
    query_inner(&asked, out);
    if out.len() == from {
        return;
    }
    if let Some(whole) = whole_by {
        Hap::map_times(&mut out[from..], asked.span, edges, |t| {
            checked_time(t.checked_add_int(whole), operation)
        });
    } else {
        Hap::map_times(&mut out[from..], asked.span, edges, |t| {
            shift_hap_time(t, by, operation)
        });
    }
}

#[inline(never)]
fn query_join_cell_onto(
    cell: &Arc<NodeCell>,
    flags: u8,
    mode: &JoinMode,
    thread: &QueryThread,
    state: &State,
    out: &mut Vec<Hap>,
) {
    let resolved = match &cell.kind {
        Node::PatternOfPureOperand(boxed, map) => {
            let OperandPureNode(outer, operand, memo) = &**boxed;
            let mut resolved = take_resolved_list();
            let settings = settings::current_state_id();
            let ok = with_list(|outer_haps| {
                outer.answer_child_onto(thread, state, outer_haps);
                for mut hap in outer_haps.drain(..) {
                    let inner = memo
                        .construct(&hap.value, &settings, |value| map.apply(value, operand))
                        .pattern()
                        .clone();
                    if query_error_pending() || thread.stop_requested() {
                        return false;
                    }
                    if hap.pick_lookup().is_some() {
                        hap.keys.edit().pick_lookup = None;
                        hap.keys.trim();
                    }
                    resolved.push(ResolvedHap::of(hap, inner));
                }
                true
            });
            if !ok {
                resolved.clear();
            }
            resolved
        }
        _ => plain_pattern(cell, flags).resolve_for_join(state),
    };
    query_join_resolved_onto(resolved, mode, thread, state, out);
}

#[inline(never)]
fn query_join_onto(
    inner: &Pattern,
    mode: &JoinMode,
    thread: &QueryThread,
    state: &State,
    out: &mut Vec<Hap>,
) {
    let resolved = inner.resolve_for_join(state);
    query_join_resolved_onto(resolved, mode, thread, state, out);
}

fn query_join_resolved_onto(
    mut resolved: Vec<ResolvedHap>,
    mode: &JoinMode,
    thread: &QueryThread,
    state: &State,
    out: &mut Vec<Hap>,
) {
    if resolved.iter().any(|r| r.inner.is_none()) {
        put_resolved_list(resolved);
        signal_query_error(|| "x.value.query is not a function".into());
        return;
    }
    let from = out.len();
    let ok = match mode {
        JoinMode::Inner | JoinMode::Outer | JoinMode::Mix => {
            let part_asked = part_is_asked();
            let mut ok = true;
            for ResolvedHap { hap: a, inner } in resolved.drain(..) {
                let inner_pat = inner.expect("checked all patterns");
                if part_asked && refuses_part(&inner_pat) {
                    ok = false;
                    break;
                }
                let inner_from = out.len();
                let asked = state.set_span(a.part);
                inner_pat.answer_child_onto(thread, &asked, out);
                if thread.stop_requested() {
                    ok = false;
                    break;
                }
                match mode {
                    JoinMode::Inner => {
                        if !a.context.is_empty() || !a.keys.is_none() {
                            for b in &mut out[inner_from..] {
                                hap::join_hap_in_place(&a, b, b.whole);
                            }
                        }
                    }
                    JoinMode::Outer => {
                        if a.context.is_empty() && a.keys.is_none() {
                            for b in &mut out[inner_from..] {
                                b.whole = a.whole;
                            }
                        } else {
                            for b in &mut out[inner_from..] {
                                hap::join_hap_in_place(&a, b, a.whole);
                            }
                        }
                    }
                    JoinMode::Mix => {
                        let mut kept = inner_from;
                        for at in inner_from..out.len() {
                            let whole = match (a.whole, out[at].whole) {
                                (Some(x), Some(y)) => match x.intersection(&y) {
                                    Some(i) => Some(i),
                                    None => {
                                        signal_query_error(|| "TimeSpans do not intersect".into());
                                        continue;
                                    }
                                },
                                _ => None,
                            };
                            hap::join_hap_in_place(&a, &mut out[at], whole);
                            if kept != at {
                                out.swap(kept, at);
                            }
                            kept += 1;
                        }
                        out.truncate(kept);
                    }
                    _ => unreachable!("handled by the outer match"),
                }
                let made = out.len() - from;
                if !thread.charge(made) {
                    ok = false;
                    break;
                }
                thread.note_accumulated(made);
            }
            ok
        }

        JoinMode::Squeeze => {
            let part_asked = part_is_asked();
            let mut ok = true;
            for ResolvedHap {
                hap: outer,
                inner: inner_pat,
            } in resolved.drain(..)
            {
                let inner_pat = inner_pat.expect("checked all patterns");
                let Some(outer_whole) = outer.whole else {
                    continue;
                };
                if part_asked && refuses_part(&inner_pat) {
                    ok = false;
                    break;
                }
                let inner_from = out.len();
                let asked = state.set_span(outer.part);
                inner_pat.query_focused_onto(thread, outer.whole_or_part(), &asked, out);
                if thread.stop_requested() {
                    ok = false;
                    break;
                }
                let mut kept = inner_from;
                for at in inner_from..out.len() {
                    let whole = match out[at].whole {
                        Some(iw) => match iw.intersection(&outer_whole) {
                            Some(w) => Some(w),
                            None => continue,
                        },
                        None => None,
                    };
                    let Some(part) = out[at].part.intersection(&outer.part) else {
                        continue;
                    };
                    let inner = &mut out[at];
                    inner.whole = whole;
                    inner.part = part;
                    if !outer.keys.is_none() {
                        let pick_lookup = joined_pick_lookup(&outer, inner);
                        inner.keys = ContextKeys::merge_with(
                            &outer.keys,
                            &inner.keys,
                            inner.live_controls(),
                            inner.slider_binding(),
                            pick_lookup,
                        );
                    }
                    if !outer.context.is_empty() {
                        inner.context.append(&outer.context);
                    }
                    if kept != at {
                        out.swap(kept, at);
                    }
                    kept += 1;
                }
                out.truncate(kept);
                let made = out.len() - from;
                if !thread.charge(made) {
                    ok = false;
                    break;
                }
                thread.note_accumulated(made);
            }
            ok
        }

        JoinMode::Reset | JoinMode::Restart => {
            let mut ok = true;
            for ResolvedHap {
                hap: outer,
                inner: inner_pat,
            } in resolved.drain(..)
            {
                let inner_pat = inner_pat.expect("checked all patterns");
                let Some(outer_whole) = outer.whole else {
                    continue;
                };
                let shift = if *mode == JoinMode::Restart {
                    outer_whole.begin
                } else {
                    outer_whole.begin.cycle_pos()
                };
                let inner_from = out.len();
                inner_pat.query_restarted_onto(thread, shift, state, outer.part, out);
                if thread.stop_requested() {
                    ok = false;
                    break;
                }
                let mut kept = inner_from;
                for at in inner_from..out.len() {
                    let Some(part) = out[at].part.intersection(&outer.part) else {
                        continue;
                    };
                    let whole = out[at].whole.and_then(|iw| iw.intersection(&outer_whole));
                    let inner = &mut out[at];
                    inner.part = part;
                    hap::join_hap_in_place(&outer, inner, whole);
                    if kept != at {
                        out.swap(kept, at);
                    }
                    kept += 1;
                }
                out.truncate(kept);
                let made = out.len() - from;
                if !thread.charge(made) {
                    ok = false;
                    break;
                }
                thread.note_accumulated(made);
            }
            ok
        }
    };
    if !ok {
        out.truncate(from);
    }
    put_resolved_list(resolved);
}

#[inline(never)]
fn query_inner_join_onto(
    pat: &Pattern,
    f: &ToPatternFn,
    thread: &QueryThread,
    state: &State,
    out: &mut Vec<Hap>,
) {
    let from = out.len();
    let ok = with_list(|outer_haps| {
        pat.answer_child_onto(thread, state, outer_haps);
        for outer in outer_haps.drain(..) {
            let inner_pat = f(&outer.value);
            let inner_from = out.len();
            let asked = state.set_span(outer.part);
            inner_pat.answer_child_onto(thread, &asked, out);
            if thread.stop_requested() {
                return false;
            }
            if !outer.context.is_empty() || !outer.keys.is_none() {
                for inner in &mut out[inner_from..] {
                    hap::join_hap_in_place(&outer, inner, inner.whole);
                }
            }
            let made = out.len() - from;
            if !thread.charge(made) {
                return false;
            }
            thread.note_accumulated(made);
        }
        true
    });
    if !ok {
        out.truncate(from);
    }
}

#[inline(never)]
fn query_app_both_onto(
    pat_func: &Pattern,
    pat_val: &Pattern,
    apply: &Combine,
    lookup_flow: LookupFlow,
    thread: &QueryThread,
    state: &State,
    out: &mut Vec<Hap>,
) {
    let from = out.len();
    let ok = with_list(|funcs| {
        pat_func.answer_child_onto(thread, state, funcs);
        if funcs.is_empty() || thread.stop_requested() {
            return !thread.stop_requested();
        }
        with_list(|vals| {
            pat_val.answer_child_onto(thread, state, vals);
            if vals.is_empty() || thread.stop_requested() {
                return !thread.stop_requested();
            }
            for hf in funcs.iter() {
                for hv in vals.iter() {
                    let Some(part) = hf.part.intersection(&hv.part) else {
                        continue;
                    };
                    let whole = match (hf.whole, hv.whole) {
                        (Some(a), Some(b)) => match a.intersection(&b) {
                            Some(w) => Some(w),
                            None => {
                                signal_query_error(|| "TimeSpans do not intersect".into());
                                continue;
                            }
                        },
                        _ => None,
                    };
                    let hap = applied_hap(hf, hv, whole, part, apply, lookup_flow);
                    if thread.stop_requested() {
                        return false;
                    }
                    if !thread.push_budgeted(out, from, hap) {
                        return false;
                    }
                }
            }
            true
        })
    });
    if !ok {
        out.truncate(from);
    }
}
