//! Layer meeting (`met`, `met_grids`), integer-tick run packing, and cycle
//! containment checks split out of `sequence.rs` to keep module size bounded.

use super::*;

/// Stack-resident builder that packs multi-location runs into `SmallCopyBuf<u32, 32>`.
/// Small grids (`<= 12` distinct runs) scan inline with zero table init; larger
/// grids lazily initialize a 256-slot open-addressed table for O(1) deduplication.
pub(super) struct PackedRunsBuilder {
    runs: SmallCopyBuf<u32, 32>,
    ht: Option<Box<[u16; 256]>>,
    distinct: usize,
    /// Inline cache of up to 8 distinct 2-word runs `(w0, w1, start_pos)` for
    /// small multi-step meets so repeated note/control location pairs match
    /// without scanning `runs` or allocating `ht`.
    pair_cache: [(u32, u32, u16); 8],
    pair_len: u8,
    prev: ([u32; 4], usize, usize),
}

impl PackedRunsBuilder {
    #[inline]
    pub(super) fn new() -> Self {
        Self {
            runs: SmallCopyBuf::new(0),
            ht: None,
            distinct: 0,
            pair_cache: [(0, 0, 0); 8],
            pair_len: 0,
            prev: ([0; 4], 0, 0),
        }
    }

    #[inline]
    pub(super) fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    #[inline]
    pub(super) fn as_slice(&self) -> &[u32] {
        self.runs.as_slice()
    }

    #[inline]
    pub(super) fn into_boxed_slice(self) -> Box<[u32]> {
        self.runs.into_boxed_slice()
    }

    fn init_ht(&mut self) {
        let mut ht = [u16::MAX; 256];
        let existing = self.runs.as_slice();
        let mut pos = 0usize;
        while pos < existing.len() {
            let start = pos;
            while pos < existing.len() && (existing[pos] & PackedLoc::SPARE) == 0 {
                pos += 1;
            }
            if pos >= existing.len() {
                break;
            }
            if start < usize::from(u16::MAX) {
                let h = (u64::from(existing[start]).wrapping_mul(0x9e37_79b9)
                    ^ u64::from(existing[pos]).wrapping_mul(0x517c_c1b7))
                    >> 8;
                let mut slot = (h as usize) & 255;
                while ht[slot] != u16::MAX {
                    slot = (slot + 1) & 255;
                }
                ht[slot] = start as u16;
            }
            pos += 1;
        }
        self.ht = Some(Box::new(ht));
    }

    #[inline]
    pub(super) fn intern_run(&mut self, needle: &[u32]) -> Option<usize> {
        let n = needle.len();
        debug_assert!(n >= 2);
        if n == 2 {
            let (w0, w1) = (needle[0], needle[1]);
            let plen = usize::from(self.pair_len);
            for &(c0, c1, pos) in &self.pair_cache[..plen] {
                if c0 == w0 && c1 == w1 {
                    return Some(usize::from(pos));
                }
            }
            if plen < self.pair_cache.len() && self.ht.is_none() {
                let start = self.runs.len();
                if start > PackedLoc::INDEX_MASK as usize {
                    return None;
                }
                self.runs.push(w0);
                self.runs.push(w1);
                if start < usize::from(u16::MAX) {
                    self.pair_cache[plen] = (w0, w1, start as u16);
                    self.pair_len += 1;
                }
                self.distinct += 1;
                return Some(start);
            }
        } else if n <= 4 && self.prev.1 == n && self.prev.0[..n] == *needle {
            return Some(self.prev.2);
        }
        let first_word = needle[0];
        if self.ht.is_none() && self.distinct >= 12 {
            self.init_ht();
        }
        let start = if let Some(ht) = self.ht.as_mut()
            && self.distinct < 128
        {
            let existing = self.runs.as_slice();
            let h = (u64::from(first_word).wrapping_mul(0x9e37_79b9)
                ^ u64::from(needle[n - 1]).wrapping_mul(0x517c_c1b7))
                >> 8;
            let mut slot = (h as usize) & 255;
            let found = loop {
                let entry = ht[slot];
                if entry == u16::MAX {
                    break None;
                }
                let pos = usize::from(entry);
                if pos + n <= existing.len()
                    && existing[pos] == first_word
                    && existing[pos + 1..pos + n] == needle[1..]
                {
                    break Some(pos);
                }
                slot = (slot + 1) & 255;
            };
            match found {
                Some(pos) => pos,
                None => {
                    let start = self.runs.len();
                    if start > PackedLoc::INDEX_MASK as usize {
                        return None;
                    }
                    for &word in needle {
                        self.runs.push(word);
                    }
                    if start < usize::from(u16::MAX) {
                        ht[slot] = start as u16;
                    }
                    self.distinct += 1;
                    start
                }
            }
        } else {
            let existing = self.runs.as_slice();
            let mut found = None;
            let mut pos = 0;
            while pos + n <= existing.len() {
                if existing[pos] == first_word && existing[pos + 1..pos + n] == needle[1..] {
                    found = Some(pos);
                    break;
                }
                while pos < existing.len() && (existing[pos] & PackedLoc::SPARE) == 0 {
                    pos += 1;
                }
                pos += 1;
            }
            match found {
                Some(pos) => pos,
                None => {
                    let start = self.runs.len();
                    if start > PackedLoc::INDEX_MASK as usize {
                        return None;
                    }
                    for &word in needle {
                        self.runs.push(word);
                    }
                    self.distinct += 1;
                    start
                }
            }
        };
        if n <= 4 {
            let mut saved = [0u32; 4];
            saved[..n].copy_from_slice(needle);
            self.prev = (saved, n, start);
        }
        Some(start)
    }
}

fn collect_locations_raw(
    locs: &Locations,
    out: &mut SmallCopyBuf<(u32, u16), 8>,
    base: &mut u32,
) -> Option<()> {
    for (begin, end) in locs.iter() {
        let b = u32::try_from(begin).ok()?;
        let len = u16::try_from(end.checked_sub(begin)?).ok()?;
        if u32::from(len) >= PackedLoc::LENGTH_MASK {
            return None;
        }
        *base = (*base).min(b);
        out.push((b, len));
    }
    Some(())
}

fn collect_before_raw(
    dress: &Dress,
    out: &mut SmallCopyBuf<(u32, u16), 8>,
    base: &mut u32,
) -> Option<()> {
    if dress.before_packed[0] != 0 {
        for &p in &dress.before_packed {
            if p == 0 {
                break;
            }
            let (begin, end) = unpack_dress_loc(p);
            let b = u32::try_from(begin).ok()?;
            let len = u16::try_from(end.checked_sub(begin)?).ok()?;
            if u32::from(len) >= PackedLoc::LENGTH_MASK {
                return None;
            }
            *base = (*base).min(b);
            out.push((b, len));
        }
        Some(())
    } else if let Some(tail) = dress.tail.as_deref() {
        collect_locations_raw(&tail.before, out, base)
    } else {
        Some(())
    }
}

impl<'a> GridView<'a> {
    /// Append all compact `(begin, length)` ranges for step `index` into `out`.
    /// Returns `None` if the step uses wide locations or an out-of-range length.
    #[inline]
    fn push_inline_locs_raw<const N: usize>(
        self,
        index: usize,
        out: &mut SmallCopyBuf<(u32, u16), N>,
        base: &mut u32,
    ) -> Option<()> {
        let packed = self.steps.get(index).loc.0;
        if packed == 0 {
            return Some(());
        }
        if packed & PackedLoc::WIDE_MASK == PackedLoc::WIDE_MASK {
            return None;
        }
        let own_base = self.base & !Grid::SMALL;
        if packed & PackedLoc::SPARE != 0 {
            let start = (packed & PackedLoc::INDEX_MASK) as usize;
            match self.packed {
                PackedRunsRef::Small(runs) => {
                    for &word in &runs[start..] {
                        let entry = word & !SMALL_RUN_END;
                        let begin = own_base + u32::from(entry >> SMALL_RUN_LEN_BITS);
                        let length = (entry & SMALL_RUN_LEN_MASK).checked_sub(1)?;
                        *base = (*base).min(begin);
                        out.push((begin, length));
                        if (word & SMALL_RUN_END) != 0 {
                            return Some(());
                        }
                    }
                }
                PackedRunsRef::Wide(runs) => {
                    for &word in &runs[start..] {
                        let entry = word & !PackedLoc::SPARE;
                        let begin = own_base + (entry >> PackedLoc::LENGTH_BITS);
                        let length = (entry & PackedLoc::LENGTH_MASK).checked_sub(1)?;
                        if length >= PackedLoc::LENGTH_MASK {
                            return None;
                        }
                        *base = (*base).min(begin);
                        out.push((begin, length as u16));
                        if word & PackedLoc::SPARE != 0 {
                            return Some(());
                        }
                    }
                }
                PackedRunsRef::None => return None,
            }
            return None;
        }
        let begin = own_base + (packed >> PackedLoc::LENGTH_BITS);
        let length = (packed & PackedLoc::LENGTH_MASK).checked_sub(1)?;
        if length >= PackedLoc::LENGTH_MASK {
            return None;
        }
        *base = (*base).min(begin);
        out.push((begin, length as u16));
        Some(())
    }
}

impl Layer {
    /// This layer with each step cut against the steps of `values` that its
    /// part meets, its whole kept and its contexts joined. `side` tells
    /// which side of the application this layer is: the structure side —
    /// the function side of an `appLeft`, or the value side of an
    /// `appRight`, where `values` holds the functions. With the layer, each
    /// pair of values that meet, this layer's first: a step with the value
    /// `n` holds what the pair `n` makes, which the caller puts in the
    /// place of the pairs.
    ///
    /// `None` for more steps than a fold makes, for a period past the
    /// largest term, and for steps that are then not in the order of time.
    pub(super) fn met(
        &self,
        values: &Layer,
        side: Side,
    ) -> Option<(Layer, SmallCopyBuf<(u32, u32), 16>)> {
        let keys = match side {
            Side::Functions => ContextKeys::merge(self.dress().keys(), values.dress().keys()),
            Side::Values => ContextKeys::merge(values.dress().keys(), self.dress().keys()),
        };
        // A step that lies in one cycle meets a value that fills the cycle
        // over the whole part: the places stay, and the locations of the
        // value go beside those of each step.
        if self.in_cycles() && values.fills_each_cycle() {
            let count = u32::try_from(self.values.len()).ok()?;
            let mut pairs = SmallCopyBuf::<(u32, u32), 16>::new((0, 0));
            for value in 0..count {
                pairs.push((value, 0));
            }
            let met = self.with(|layer| {
                layer.change_dress(|dress| {
                    match side {
                        Side::Functions => dress.prepend_before(values.context(0)),
                        Side::Values => dress.append_after(&values.context(0)),
                    }
                    dress.set_keys(keys);
                });
            });
            return Some((met, pairs));
        }
        if let Some(fast) = self.met_grids(values, side, &keys) {
            return Some(fast);
        }
        // Both layers repeat in the least period that holds a whole count
        // of turns of each.
        let own_period = self.period();
        let values_period = values.period();
        let small = |term: i128| (1..=MAX_PERIOD_TERM).contains(&term);
        let period = own_period.checked_lcm(values_period)?;
        if !(small(period.numer()) && small(period.denom())) {
            return None;
        }
        let turns = period.checked_div(own_period)?.numer();
        if turns <= 0 || turns > MAX_FOLDED_STEPS as i128 {
            return None;
        }
        let moved = |span: TimeSpan, start: Fraction| {
            Some(TimeSpan::new(
                span.begin.checked_add(start)?,
                span.end.checked_add(start)?,
            ))
        };
        thread_local! {
            static SPARE_RAWS: std::cell::RefCell<Vec<Raw>> =
                const { std::cell::RefCell::new(Vec::new()) };
        }
        let mut pairs = SmallCopyBuf::<(u32, u32), 16>::new((0, 0));
        // Room for a step that meets one value in each turn.
        let room = usize::try_from(turns).ok()?.saturating_mul(self.count());
        let mut raws = SPARE_RAWS
            .try_with(|cell| std::mem::take(&mut *cell.borrow_mut()))
            .unwrap_or_default();
        raws.clear();
        raws.reserve(room.min(MAX_FOLDED_STEPS));
        let (own, others) = (self.listing(), values.listing());
        // The parts of `values` are searched and read for each step, so a
        // grid makes their times one time.
        let mut small_parts = [TimeSpan::new(Fraction::ZERO, Fraction::ZERO); 16];
        let heap_parts;
        let other_parts: &[TimeSpan] = if others.len() <= 16 {
            for (at, slot) in small_parts[..others.len()].iter_mut().enumerate() {
                *slot = others.part(at);
            }
            &small_parts[..others.len()]
        } else {
            heap_parts = (0..others.len())
                .map(|at| others.part(at))
                .collect::<Vec<_>>();
            &heap_parts
        };
        let build = (|| -> Option<(Self, SmallCopyBuf<(u32, u32), 16>)> {
            let mut prev_part: Option<TimeSpan> = None;
            for turn in 0..turns {
                let start = own_period.checked_mul(Fraction::int(turn))?;
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
                    let mut around = self.dress().before_locations();
                    around.append(&own_locations);
                    if side == Side::Values {
                        around.append(self.dress().after());
                    }
                    // The turns of `values` that the part meets, in order.
                    let turns_met = part.begin.checked_div_floor(values_period)?
                        ..part.end.checked_div_ceil(values_period)?;
                    for at in turns_met {
                        let value_start = values_period.checked_mul(Fraction::int(at))?;
                        // The part, from the start of the turn, and the steps
                        // of the turn that it meets: they are in the order of
                        // time.
                        let (begin, end) = (
                            part.begin.checked_sub(value_start)?,
                            part.end.checked_sub(value_start)?,
                        );
                        let first = other_parts.partition_point(|part| part.end <= begin);
                        let meeting = (first..other_parts.len())
                            .take_while(|at| other_parts[*at].begin < end);
                        for value_index in meeting {
                            let value_part = moved(other_parts[value_index], value_start)?;
                            let piece = TimeSpan::new(
                                part.begin.max(value_part.begin),
                                part.end.min(value_part.end),
                            );
                            if !(Fraction::ZERO <= whole.begin
                                && whole.begin <= piece.begin
                                && piece.begin < piece.end
                                && piece.end <= whole.end
                                && whole.end <= period)
                            {
                                return None;
                            }
                            if let Some(prev) = prev_part
                                && (prev.begin > piece.begin || prev.end > piece.end)
                            {
                                return None;
                            }
                            prev_part = Some(piece);
                            if raws.len() == MAX_FOLDED_STEPS {
                                return None;
                            }
                            // The pairs are few: one for each value that a
                            // value meets.
                            let pair = (own_value, others.value(value_index));
                            let value =
                                match pairs.as_slice().iter().position(|known| *known == pair) {
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
            let in_cycles = lie_in_spans(period, raws.iter().map(Raw::part));
            let mut met = self.clone();
            met.set_in_cycles(in_cycles);
            met.set_period_and_steps(period, Steps::Listed(Listing::of_vec(&mut raws)));
            // The locations of the steps are in their places now, but those
            // after the steps of the function side, which end each hap.
            met.change_dress(|dress| {
                dress.clear_before();
                if side == Side::Values {
                    dress.clear_after();
                }
                dress.set_keys(keys);
            });
            debug_assert!(met.is_in_order());
            Some((met, pairs))
        })();
        raws.clear();
        if raws.capacity() > 0 && raws.capacity() <= 64 {
            let _ = SPARE_RAWS.try_with(|cell| *cell.borrow_mut() = raws);
        }
        build
    }

    /// Integer-tick fast path for [`Self::met`] when both layers are
    /// [`Grid`]s whose combined period, cuts, and compact locations fit
    /// integer ticks without allocating intermediate [`Fraction`]s or [`Raw`]s.
    fn met_grids(
        &self,
        values: &Layer,
        side: Side,
        keys: &ContextKeys,
    ) -> Option<(Layer, SmallCopyBuf<(u32, u32), 16>)> {
        let own_period = self.period();
        let values_period = values.period();
        let gcd = |mut a: u32, mut b: u32| -> u32 {
            while b != 0 {
                let r = a % b;
                a = b;
                b = r;
            }
            a
        };
        let lcm = |a: u32, b: u32| -> Option<u32> { (a / gcd(a, b)).checked_mul(b) };
        let (period, p_num, p_den, n1, d1, n2, d2, turns) =
            if own_period.denom() == 1 && values_period.denom() == 1 {
                let n1 = u32::try_from(own_period.numer()).ok()?;
                let n2 = u32::try_from(values_period.numer()).ok()?;
                if n1 == 0 || n2 == 0 {
                    return None;
                }
                let p_num = lcm(n1, n2)?;
                if i128::from(p_num) > MAX_PERIOD_TERM {
                    return None;
                }
                let turns = p_num / n1;
                (
                    Fraction::int(i128::from(p_num)),
                    p_num,
                    1u32,
                    n1,
                    1u32,
                    n2,
                    1u32,
                    turns,
                )
            } else {
                let period = own_period.checked_lcm(values_period)?;
                let small = |term: i128| (1..=MAX_PERIOD_TERM).contains(&term);
                if !(small(period.numer()) && small(period.denom())) {
                    return None;
                }
                let p_num = u32::try_from(period.numer()).ok()?;
                let p_den = u32::try_from(period.denom()).ok()?;
                let n1 = u32::try_from(own_period.numer()).ok()?;
                let d1 = u32::try_from(own_period.denom()).ok()?;
                let n2 = u32::try_from(values_period.numer()).ok()?;
                let d2 = u32::try_from(values_period.denom()).ok()?;
                let turns = u32::try_from(period.checked_div(own_period)?.numer()).ok()?;
                (period, p_num, p_den, n1, d1, n2, d2, turns)
            };
        if turns == 0 || turns > MAX_FOLDED_STEPS as u32 {
            return None;
        }
        let has_self_before = self.dress().before_len() != 0;
        let has_self_after = side == Side::Values && !self.dress().after().is_empty();
        let has_other_before = values.dress().before_len() != 0;
        let has_other_after = !values.dress().after().is_empty();
        let mut dress_base = u32::MAX;
        let mut self_before_buf;
        let self_before_sl: &[(u32, u16)] = if has_self_before {
            self_before_buf = SmallCopyBuf::<(u32, u16), 8>::new((0, 0));
            collect_before_raw(self.dress(), &mut self_before_buf, &mut dress_base)?;
            self_before_buf.as_slice()
        } else {
            &[]
        };
        let mut self_after_buf;
        let self_after_sl: &[(u32, u16)] = if has_self_after {
            self_after_buf = SmallCopyBuf::<(u32, u16), 8>::new((0, 0));
            collect_locations_raw(self.dress().after(), &mut self_after_buf, &mut dress_base)?;
            self_after_buf.as_slice()
        } else {
            &[]
        };
        let mut other_before_buf;
        let other_before_sl: &[(u32, u16)] = if has_other_before {
            other_before_buf = SmallCopyBuf::<(u32, u16), 8>::new((0, 0));
            collect_before_raw(values.dress(), &mut other_before_buf, &mut dress_base)?;
            other_before_buf.as_slice()
        } else {
            &[]
        };
        let mut other_after_buf;
        let other_after_sl: &[(u32, u16)] = if has_other_after {
            other_after_buf = SmallCopyBuf::<(u32, u16), 8>::new((0, 0));
            collect_locations_raw(
                values.dress().after(),
                &mut other_after_buf,
                &mut dress_base,
            )?;
            other_after_buf.as_slice()
        } else {
            &[]
        };
        let has_self_dress = has_self_before || has_self_after;
        let has_other_dress = has_other_before || has_other_after;
        self.listing()
            .with_grid_view(|own| {
                values.listing().with_grid_view(|other| {
                    let t1 = own.ticks.get();
                    let t2 = other.ticks.get();
                    let ticks = lcm(lcm(t1, t2)?, lcm(lcm(d1, d2)?, p_den)?)?;
                    let period_ticks = u64::from(p_num) * u64::from(ticks / p_den);
                    if period_ticks > u64::from(u32::MAX) {
                        return None;
                    }
                    let period_ticks_u32 = period_ticks as u32;
                    let scale1 = ticks / t1;
                    let scale2 = ticks / t2;
                    let p1_ticks = n1.checked_mul(ticks / d1)?;
                    let p2_ticks = n2.checked_mul(ticks / d2)?;
                    let other_len = other.steps.len();
                    let own_base = own.base & !Grid::SMALL;
                    let other_base = other.base & !Grid::SMALL;
                    // Precompute the packed-location base in O(1) so steps and
                    // packed location runs build in a single pass with zero
                    // intermediate `MetStep` / `loc_buf` buffers.
                    let mut base = dress_base;
                    if own_base != 0 || !own.steps.has_no_locations() {
                        base = base.min(own_base);
                    }
                    if other_base != 0 || !other.steps.has_no_locations() {
                        base = base.min(other_base);
                    }
                    let base = if base == u32::MAX { 0 } else { base };
                    if base & Grid::SMALL != 0 {
                        return None;
                    }
                    let mut small_steps =
                        SmallCopyBuf::<SmallGridStep, 64>::new(GridSteps::ZERO_SMALL);
                    let mut wide_steps: Vec<GridStep> = Vec::new();
                    let mut packed_runs = PackedRunsBuilder::new();
                    let mut wide_locs_inline: [Option<Locations>; 4] = [None, None, None, None];
                    let mut wide_locs_len = 0usize;
                    let mut wide_locs_spill: Vec<Locations> = Vec::new();
                    let mut loc_pair_cache = [(0u32, 0u32, 0u32); 8];
                    let mut loc_cache_len = 0usize;
                    let mut step_locs = SmallCopyBuf::<(u32, u16), 16>::new((0, 0));
                    let mut tick_cuts = SmallCopyBuf::<Cut, 16>::new(Cut {
                        index: 0,
                        begin: 0,
                        end: 0,
                    });
                    let mut pairs = SmallCopyBuf::<(u32, u32), 16>::new((0, 0));
                    let own_vlen = self.values.len();
                    let other_vlen = values.values.len().max(1);
                    let pair_prod = own_vlen.saturating_mul(other_vlen);
                    let use_pair_map = pair_prod > 12 && pair_prod <= 256;
                    let mut pair_map_storage;
                    let pair_map: &mut [u16] = if use_pair_map {
                        pair_map_storage = [u16::MAX; 256];
                        &mut pair_map_storage
                    } else {
                        &mut []
                    };
                    let mut in_cycles = true;
                    let check_in_cycles = !(self.in_cycles() && p_den == 1);
                    let single_other_turn = p2_ticks == period_ticks_u32;
                    let p2_pow2 = p2_ticks
                        .is_power_of_two()
                        .then(|| p2_ticks.trailing_zeros());
                    let turns_den = u64::from(p_den);
                    let ticks64 = u64::from(ticks);
                    let own_has_cuts = own.rare.is_some();
                    let mut prev_at = u32::MAX;
                    let mut prev_own_b = 0u32;
                    let mut prev_j = 0usize;
                    let mut prev_piece_b = 0u32;
                    let mut prev_piece_e = 0u32;
                    let mut step_count = 0usize;
                    for turn in 0..turns {
                        let start1 = turn * p1_ticks;
                        let mut own_cuts = if own_has_cuts {
                            own.cuts()
                        } else {
                            CutsSlice::EMPTY
                        };
                        for i in 0..own.steps.len() {
                            let own_step = own.steps.get(i);
                            let own_b = start1 + own_step.part_begin * scale1;
                            let own_e = start1 + own_step.part_end * scale1;
                            let (own_whole_b, own_whole_e) = if own_has_cuts {
                                match own_cuts.split_first_at(i) {
                                    Some(cut) => (
                                        start1.checked_add(cut.begin.checked_mul(scale1)?)?,
                                        start1.checked_add(cut.end.checked_mul(scale1)?)?,
                                    ),
                                    None => (own_b, own_e),
                                }
                            } else {
                                (own_b, own_e)
                            };
                            let (first_at, last_at) = if single_other_turn {
                                (0u32, 0u32)
                            } else if let Some(sh) = p2_pow2 {
                                (own_b >> sh, (own_e - 1) >> sh)
                            } else {
                                (own_b / p2_ticks, (own_e - 1) / p2_ticks)
                            };
                            for at in first_at..=last_at {
                                let start2 = at * p2_ticks;
                                let rel_b = own_b.saturating_sub(start2);
                                let rel_e = own_e - start2;
                                let mut j = if at == prev_at && own_b >= prev_own_b {
                                    prev_j
                                } else {
                                    0usize
                                };
                                while j < other_len {
                                    let (_, oe) = other.steps.times_of(j);
                                    if oe * scale2 > rel_b {
                                        break;
                                    }
                                    j += 1;
                                }
                                prev_at = at;
                                prev_own_b = own_b;
                                prev_j = j;
                                while j < other_len {
                                    let other_step = other.steps.get(j);
                                    let jb = other_step.part_begin * scale2;
                                    if jb >= rel_e {
                                        break;
                                    }
                                    let je = other_step.part_end * scale2;
                                    let piece_b = own_b.max(start2 + jb);
                                    let piece_e = own_e.min(start2 + je);
                                    if !(own_whole_b <= piece_b
                                        && piece_b < piece_e
                                        && piece_e <= own_whole_e
                                        && own_whole_e <= period_ticks_u32)
                                        || prev_piece_b > piece_b
                                        || prev_piece_e > piece_e
                                    {
                                        return None;
                                    }
                                    prev_piece_b = piece_b;
                                    prev_piece_e = piece_e;
                                    if step_count >= MAX_FOLDED_STEPS {
                                        return None;
                                    }
                                    let pair = (own_step.value, other_step.value);
                                    let pair_idx = if use_pair_map
                                        && (own_step.value as usize) < own_vlen
                                        && (other_step.value as usize) < other_vlen
                                    {
                                        let flat = (own_step.value as usize) * other_vlen
                                            + (other_step.value as usize);
                                        let cached = pair_map[flat];
                                        if cached != u16::MAX {
                                            usize::from(cached)
                                        } else {
                                            let idx = pairs.len();
                                            pairs.push(pair);
                                            if idx < usize::from(u16::MAX) {
                                                pair_map[flat] = idx as u16;
                                            }
                                            idx
                                        }
                                    } else {
                                        match pairs
                                            .as_slice()
                                            .iter()
                                            .position(|known| *known == pair)
                                        {
                                            Some(idx) => idx,
                                            None => {
                                                pairs.push(pair);
                                                pairs.len() - 1
                                            }
                                        }
                                    };
                                    let value = u32::try_from(pair_idx).ok()?;
                                    let key_a = own_step.loc.0;
                                    let key_b = other_step.loc.0;
                                    let loc = if let Some(&(_, _, cached)) = loc_pair_cache
                                        [..loc_cache_len]
                                        .iter()
                                        .find(|&&(a, b, _)| a == key_a && b == key_b)
                                    {
                                        PackedLoc(cached)
                                    } else {
                                        step_locs.clear();
                                        let mut dummy_base = base;
                                        let push_own =
                                            |dst: &mut SmallCopyBuf<(u32, u16), 16>,
                                             b_ref: &mut u32|
                                             -> Option<()> {
                                                if !has_self_dress
                                                    && (key_a & PackedLoc::SPARE) == 0
                                                {
                                                    if key_a != 0 {
                                                        let b = own_base
                                                            + (key_a >> PackedLoc::LENGTH_BITS);
                                                        let l = ((key_a & PackedLoc::LENGTH_MASK)
                                                            - 1)
                                                            as u16;
                                                        dst.push((b, l));
                                                    }
                                                    return Some(());
                                                }
                                                for &l in self_before_sl {
                                                    dst.push(l);
                                                }
                                                own.push_inline_locs_raw(i, dst, b_ref)?;
                                                for &l in self_after_sl {
                                                    dst.push(l);
                                                }
                                                Some(())
                                            };
                                        let push_other =
                                            |dst: &mut SmallCopyBuf<(u32, u16), 16>,
                                             b_ref: &mut u32|
                                             -> Option<()> {
                                                if !has_other_dress
                                                    && (key_b & PackedLoc::SPARE) == 0
                                                {
                                                    if key_b != 0 {
                                                        let b = other_base
                                                            + (key_b >> PackedLoc::LENGTH_BITS);
                                                        let l = ((key_b & PackedLoc::LENGTH_MASK)
                                                            - 1)
                                                            as u16;
                                                        dst.push((b, l));
                                                    }
                                                    return Some(());
                                                }
                                                for &l in other_before_sl {
                                                    dst.push(l);
                                                }
                                                other.push_inline_locs_raw(j, dst, b_ref)?;
                                                for &l in other_after_sl {
                                                    dst.push(l);
                                                }
                                                Some(())
                                            };
                                        match side {
                                            Side::Functions => {
                                                push_other(&mut step_locs, &mut dummy_base)?;
                                                push_own(&mut step_locs, &mut dummy_base)?;
                                            }
                                            Side::Values => {
                                                push_own(&mut step_locs, &mut dummy_base)?;
                                                push_other(&mut step_locs, &mut dummy_base)?;
                                            }
                                        }
                                        let sl = step_locs.as_slice();
                                        let n = sl.len();
                                        if n > 16 {
                                            return None;
                                        }
                                        let built_loc = pack_met_step_loc(
                                            base,
                                            sl,
                                            &mut packed_runs,
                                            &mut wide_locs_inline,
                                            &mut wide_locs_len,
                                            &mut wide_locs_spill,
                                        )?;
                                        if loc_cache_len < loc_pair_cache.len() {
                                            loc_pair_cache[loc_cache_len] =
                                                (key_a, key_b, built_loc.0);
                                            loc_cache_len += 1;
                                        }
                                        built_loc
                                    };
                                    if check_in_cycles && in_cycles {
                                        let begins = u64::from(piece_b) * turns_den;
                                        let ends = u64::from(piece_e) * turns_den;
                                        if ends > (begins / ticks64 + 1) * ticks64 {
                                            in_cycles = false;
                                        }
                                    }
                                    let step_idx = step_count as u32;
                                    if piece_b != own_whole_b || piece_e != own_whole_e {
                                        tick_cuts.push(Cut {
                                            index: step_idx,
                                            begin: own_whole_b,
                                            end: own_whole_e,
                                        });
                                    }
                                    let grid_step = GridStep {
                                        part_begin: piece_b,
                                        part_end: piece_e,
                                        value,
                                        loc,
                                    };
                                    if wide_steps.is_empty() {
                                        if let Some(small) = SmallGridStep::from_wide(grid_step) {
                                            small_steps.push(small);
                                        } else {
                                            wide_steps.reserve(small_steps.len() + 1);
                                            wide_steps.extend(
                                                small_steps.as_slice().iter().map(|s| s.to_wide()),
                                            );
                                            wide_steps.push(grid_step);
                                        }
                                    } else {
                                        wide_steps.push(grid_step);
                                    }
                                    step_count += 1;
                                    j += 1;
                                }
                            }
                        }
                    }
                    let steps = finish_met_grid_steps(small_steps, wide_steps);
                    let wide_box =
                        finish_wide_locs(&mut wide_locs_inline, wide_locs_len, wide_locs_spill);
                    let (steps, rare) = match steps {
                        GridSteps::Wide(wide_steps) => (
                            GridSteps::Small(Box::from([])),
                            Some(Box::new(SpareRare::from_parts(
                                tick_cuts.as_slice(),
                                wide_box,
                                wide_steps,
                            ))),
                        ),
                        other if !tick_cuts.is_empty() || !wide_box.is_empty() => (
                            other,
                            Some(Box::new(SpareRare::from_parts(
                                tick_cuts.as_slice(),
                                wide_box,
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
                    let listing = Listing::from_parts(
                        NonZeroU32::new(ticks)?,
                        base | Grid::SMALL,
                        steps,
                        packed,
                        rare,
                    );
                    let mut met = self.clone();
                    met.set_in_cycles(in_cycles);
                    met.set_period_and_steps(period, Steps::Listed(listing));
                    if met.dress.is_some() || !keys.is_empty() {
                        met.change_dress(|dress| {
                            dress.clear_before();
                            if side == Side::Values {
                                dress.clear_after();
                            }
                            dress.set_keys(keys.clone());
                        });
                    }
                    debug_assert!(met.is_in_order());
                    Some((met, pairs))
                })
            })
            .flatten()
            .flatten()
    }

    /// Direct single-pass meet of `self` (on the `Side::Functions` side of `appLeft`)
    /// against `rhs_locs.len()` (`2..=8`) equal-width numeric steps (`slowcat == false`:
    /// period `1`, ticks `k`; `slowcat == true`: period `k`, ticks `1`) with zero
    /// temporary RHS `Layer`, `Grid`, or `Pattern` allocation.
    pub(super) fn met_flat_numbers(
        &self,
        rhs_locs: &[(u32, u16)],
        slowcat: bool,
    ) -> Option<(Layer, SmallCopyBuf<(u32, u32), 16>)> {
        let k = rhs_locs.len();
        if !(2..=8).contains(&k) {
            return None;
        }
        let own_period = self.period();
        let n1 = u32::try_from(own_period.numer()).ok()?;
        let d1 = u32::try_from(own_period.denom()).ok()?;
        if n1 == 0 || d1 == 0 {
            return None;
        }
        let n2 = if slowcat { k as u32 } else { 1u32 };
        let gcd = |mut a: u32, mut b: u32| -> u32 {
            while b != 0 {
                let r = a % b;
                a = b;
                b = r;
            }
            a
        };
        let lcm = |a: u32, b: u32| -> Option<u32> { (a / gcd(a, b)).checked_mul(b) };
        // `rhs_period = n2 / 1` is always an integer, so `lcm(n1/d1, n2)` is
        // the integer `lcm(n1, n2)` and `turns = (p_num / n1) * d1`.
        let p_num = lcm(n1, n2)?;
        if i128::from(p_num) > MAX_PERIOD_TERM {
            return None;
        }
        let turns = (p_num / n1).checked_mul(d1)?;
        if turns == 0 || turns > MAX_FOLDED_STEPS as u32 {
            return None;
        }
        let period = Fraction::int(i128::from(p_num));
        let has_self_before = self.dress().before_len() != 0;
        let mut dress_base = u32::MAX;
        let mut self_before_buf;
        let self_before_sl: &[(u32, u16)] = if has_self_before {
            self_before_buf = SmallCopyBuf::<(u32, u16), 8>::new((0, 0));
            collect_before_raw(self.dress(), &mut self_before_buf, &mut dress_base)?;
            self_before_buf.as_slice()
        } else {
            &[]
        };
        let mut rhs_base = u32::MAX;
        for &(b, l) in rhs_locs {
            if u32::from(l) >= PackedLoc::LENGTH_MASK {
                return None;
            }
            rhs_base = rhs_base.min(b);
        }
        self.listing()
            .with_grid_view(|own| {
                let t1 = own.ticks.get();
                let t2 = if slowcat { 1u32 } else { k as u32 };
                let ticks = lcm(lcm(t1, t2)?, d1)?;
                let period_ticks = u64::from(p_num) * u64::from(ticks);
                if period_ticks > u64::from(u32::MAX) {
                    return None;
                }
                let period_ticks_u32 = period_ticks as u32;
                let scale1 = ticks / t1;
                let scale2 = ticks / t2;
                let p1_ticks = n1.checked_mul(ticks / d1)?;
                let p2_ticks = n2.checked_mul(ticks)?;
                let own_base = own.base & !Grid::SMALL;
                let mut base = dress_base.min(rhs_base);
                if own_base != 0 || !own.steps.has_no_locations() {
                    base = base.min(own_base);
                }
                let base = if base == u32::MAX { 0 } else { base };
                if base & Grid::SMALL != 0 {
                    return None;
                }
                let mut small_steps = SmallCopyBuf::<SmallGridStep, 64>::new(GridSteps::ZERO_SMALL);
                let mut wide_steps: Vec<GridStep> = Vec::new();
                let mut packed_runs = PackedRunsBuilder::new();
                let mut wide_locs_inline: [Option<Locations>; 4] = [None, None, None, None];
                let mut wide_locs_len = 0usize;
                let mut wide_locs_spill: Vec<Locations> = Vec::new();
                let mut loc_pair_cache = [(0u32, 0u8, 0u32); 8];
                let mut loc_cache_len = 0usize;
                let mut step_locs = SmallCopyBuf::<(u32, u16), 16>::new((0, 0));
                let mut tick_cuts = SmallCopyBuf::<Cut, 16>::new(Cut {
                    index: 0,
                    begin: 0,
                    end: 0,
                });
                let mut pairs = SmallCopyBuf::<(u32, u32), 16>::new((0, 0));
                let own_vlen = self.values.len();
                let pair_prod = own_vlen.saturating_mul(k);
                let mut pair_map_tiny = [u8::MAX; 16];
                let use_tiny_map = pair_prod > 0 && pair_prod <= 16;
                // O(1) location cache indexed by `pair_idx` (`0..16`). Sentinel
                // `u32::MAX` cannot be a valid `PackedLoc` bit pattern.
                let mut loc_by_pair_key = [u32::MAX; 16];
                let mut loc_by_pair_val = [0u32; 16];
                let mut in_cycles = true;
                let check_in_cycles = !self.in_cycles();
                let single_other_turn = p2_ticks == period_ticks_u32;
                let p2_pow2 = p2_ticks
                    .is_power_of_two()
                    .then(|| p2_ticks.trailing_zeros());
                let scale2_pow2 = scale2.is_power_of_two().then(|| scale2.trailing_zeros());
                let ticks64 = u64::from(ticks);
                let own_has_cuts = own.rare.is_some();
                let mut prev_piece_b = 0u32;
                let mut prev_piece_e = 0u32;
                let mut step_count = 0usize;
                for turn in 0..turns {
                    let start1 = turn * p1_ticks;
                    let mut own_cuts = if own_has_cuts {
                        own.cuts()
                    } else {
                        CutsSlice::EMPTY
                    };
                    for i in 0..own.steps.len() {
                        let own_step = own.steps.get(i);
                        let own_b = start1 + own_step.part_begin * scale1;
                        let own_e = start1 + own_step.part_end * scale1;
                        let (own_whole_b, own_whole_e) = if own_has_cuts {
                            match own_cuts.split_first_at(i) {
                                Some(cut) => (
                                    start1.checked_add(cut.begin.checked_mul(scale1)?)?,
                                    start1.checked_add(cut.end.checked_mul(scale1)?)?,
                                ),
                                None => (own_b, own_e),
                            }
                        } else {
                            (own_b, own_e)
                        };
                        let (first_at, last_at) = if single_other_turn {
                            (0u32, 0u32)
                        } else if let Some(sh) = p2_pow2 {
                            (own_b >> sh, (own_e - 1) >> sh)
                        } else {
                            (own_b / p2_ticks, (own_e - 1) / p2_ticks)
                        };
                        for at in first_at..=last_at {
                            let start2 = at * p2_ticks;
                            let rel_b = own_b.saturating_sub(start2);
                            let rel_e = (own_e - start2).min(p2_ticks);
                            let (j_first, j_last) = if let Some(sh) = scale2_pow2 {
                                ((rel_b >> sh) as usize, ((rel_e - 1) >> sh) as usize)
                            } else {
                                ((rel_b / scale2) as usize, ((rel_e - 1) / scale2) as usize)
                            };
                            for (offset, &rhs_loc) in
                                rhs_locs[j_first..=j_last.min(k - 1)].iter().enumerate()
                            {
                                let j = j_first + offset;
                                let jb = (j as u32) * scale2;
                                let je = jb + scale2;
                                let piece_b = own_b.max(start2 + jb);
                                let piece_e = own_e.min(start2 + je);
                                if !(own_whole_b <= piece_b
                                    && piece_b < piece_e
                                    && piece_e <= own_whole_e
                                    && own_whole_e <= period_ticks_u32)
                                    || prev_piece_b > piece_b
                                    || prev_piece_e > piece_e
                                {
                                    return None;
                                }
                                prev_piece_b = piece_b;
                                prev_piece_e = piece_e;
                                if step_count >= MAX_FOLDED_STEPS {
                                    return None;
                                }
                                let pair = (own_step.value, j as u32);
                                let pair_idx = if use_tiny_map
                                    && (own_step.value as usize) < own_vlen
                                {
                                    let flat = ((own_step.value as usize) * k + j) & 15;
                                    let cached = pair_map_tiny[flat];
                                    if cached != u8::MAX {
                                        usize::from(cached)
                                    } else {
                                        let idx = pairs.len();
                                        pairs.push(pair);
                                        pair_map_tiny[flat] = idx as u8;
                                        idx
                                    }
                                } else {
                                    match pairs.as_slice().iter().position(|known| *known == pair) {
                                        Some(idx) => idx,
                                        None => {
                                            pairs.push(pair);
                                            pairs.len() - 1
                                        }
                                    }
                                };
                                let value = u32::try_from(pair_idx).ok()?;
                                let key_a = own_step.loc.0;
                                let key_b = j as u8;
                                let loc = if pair_idx < 16 && loc_by_pair_key[pair_idx] == key_a {
                                    PackedLoc(loc_by_pair_val[pair_idx])
                                } else if let Some(&(_, _, cached)) = loc_pair_cache
                                    [..loc_cache_len]
                                    .iter()
                                    .find(|&&(a, b, _)| a == key_a && b == key_b)
                                {
                                    PackedLoc(cached)
                                } else {
                                    step_locs.clear();
                                    step_locs.push(rhs_loc);
                                    if !has_self_before && (key_a & PackedLoc::SPARE) == 0 {
                                        if key_a != 0 {
                                            let b = own_base + (key_a >> PackedLoc::LENGTH_BITS);
                                            let l = ((key_a & PackedLoc::LENGTH_MASK) - 1) as u16;
                                            step_locs.push((b, l));
                                        }
                                    } else {
                                        for &l in self_before_sl {
                                            step_locs.push(l);
                                        }
                                        let mut dummy_base = base;
                                        own.push_inline_locs_raw(
                                            i,
                                            &mut step_locs,
                                            &mut dummy_base,
                                        )?;
                                    }
                                    let sl = step_locs.as_slice();
                                    if sl.len() > 16 {
                                        return None;
                                    }
                                    let built_loc = pack_met_step_loc(
                                        base,
                                        sl,
                                        &mut packed_runs,
                                        &mut wide_locs_inline,
                                        &mut wide_locs_len,
                                        &mut wide_locs_spill,
                                    )?;
                                    if pair_idx < 16 && loc_by_pair_key[pair_idx] == u32::MAX {
                                        loc_by_pair_key[pair_idx] = key_a;
                                        loc_by_pair_val[pair_idx] = built_loc.0;
                                    } else if loc_cache_len < loc_pair_cache.len() {
                                        loc_pair_cache[loc_cache_len] = (key_a, key_b, built_loc.0);
                                        loc_cache_len += 1;
                                    }
                                    built_loc
                                };
                                if check_in_cycles && in_cycles {
                                    let begins = u64::from(piece_b);
                                    let ends = u64::from(piece_e);
                                    if ends > (begins / ticks64 + 1) * ticks64 {
                                        in_cycles = false;
                                    }
                                }
                                let step_idx = step_count as u32;
                                if piece_b != own_whole_b || piece_e != own_whole_e {
                                    tick_cuts.push(Cut {
                                        index: step_idx,
                                        begin: own_whole_b,
                                        end: own_whole_e,
                                    });
                                }
                                let grid_step = GridStep {
                                    part_begin: piece_b,
                                    part_end: piece_e,
                                    value,
                                    loc,
                                };
                                if wide_steps.is_empty() {
                                    if let Some(small) = SmallGridStep::from_wide(grid_step) {
                                        small_steps.push(small);
                                    } else {
                                        wide_steps.reserve(small_steps.len() + 1);
                                        wide_steps.extend(
                                            small_steps.as_slice().iter().map(|s| s.to_wide()),
                                        );
                                        wide_steps.push(grid_step);
                                    }
                                } else {
                                    wide_steps.push(grid_step);
                                }
                                step_count += 1;
                            }
                        }
                    }
                }
                let steps = finish_met_grid_steps(small_steps, wide_steps);
                let wide_box =
                    finish_wide_locs(&mut wide_locs_inline, wide_locs_len, wide_locs_spill);
                let (steps, rare) = match steps {
                    GridSteps::Wide(wide_steps) => (
                        GridSteps::Small(Box::from([])),
                        Some(Box::new(SpareRare::from_parts(
                            tick_cuts.as_slice(),
                            wide_box,
                            wide_steps,
                        ))),
                    ),
                    other if !tick_cuts.is_empty() || !wide_box.is_empty() => (
                        other,
                        Some(Box::new(SpareRare::from_parts(
                            tick_cuts.as_slice(),
                            wide_box,
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
                let listing = Listing::from_parts(
                    NonZeroU32::new(ticks)?,
                    base | Grid::SMALL,
                    steps,
                    packed,
                    rare,
                );
                let mut met = self.clone();
                met.set_in_cycles(in_cycles);
                met.set_period_and_steps(period, Steps::Listed(listing));
                if met.dress.is_some() {
                    met.change_dress(|dress| {
                        dress.clear_before();
                    });
                }
                debug_assert!(met.is_in_order());
                Some((met, pairs))
            })
            .flatten()
    }
}

/// Pack a single met step's `(begin, length)` ranges relative to `base` into a
/// [`PackedLoc`], deduplicating 2..=4-range runs into `packed_runs` and 5+
/// ranges into stack-backed `wide_locs_inline` / `wide_locs_spill`.
#[inline]
fn pack_met_step_loc(
    base: u32,
    sl: &[(u32, u16)],
    packed_runs: &mut PackedRunsBuilder,
    wide_locs_inline: &mut [Option<Locations>; 4],
    wide_locs_len: &mut usize,
    wide_locs_spill: &mut Vec<Locations>,
) -> Option<PackedLoc> {
    match sl.len() {
        0 => Some(PackedLoc(0)),
        1 => {
            let (b, l) = sl[0];
            PackedLoc::range_raw(base, b, u32::from(l))
        }
        2..=4 => {
            let n = sl.len();
            let mut run = [0u32; 4];
            for (k, &(b, l)) in sl.iter().enumerate() {
                run[k] = PackedLoc::range_raw(base, b, u32::from(l))?.0;
            }
            run[n - 1] |= PackedLoc::SPARE;
            let run_start = packed_runs.intern_run(&run[..n])?;
            Some(PackedLoc(PackedLoc::SPARE | run_start as u32))
        }
        n => {
            let mut ranges = [(0usize, 0usize); 16];
            for (k, &(b, l)) in sl.iter().enumerate() {
                ranges[k] = (b as usize, b as usize + usize::from(l));
            }
            let wanted = &ranges[..n];
            let at = if wide_locs_spill.is_empty() {
                match wide_locs_inline[..*wide_locs_len].iter().position(|slot| {
                    slot.as_ref()
                        .is_some_and(|known| known.with_slice(|s| s == wanted))
                }) {
                    Some(existing) => existing as u32,
                    None => {
                        let idx = *wide_locs_len as u32;
                        if idx > PackedLoc::INDEX_MASK {
                            return None;
                        }
                        let mut locs = Locations::new();
                        locs.extend_from_slice(wanted);
                        if *wide_locs_len < wide_locs_inline.len() {
                            wide_locs_inline[*wide_locs_len] = Some(locs);
                            *wide_locs_len += 1;
                        } else {
                            wide_locs_spill.reserve(8);
                            for slot in wide_locs_inline.iter_mut() {
                                if let Some(prev) = slot.take() {
                                    wide_locs_spill.push(prev);
                                }
                            }
                            wide_locs_spill.push(locs);
                            *wide_locs_len = wide_locs_spill.len();
                        }
                        idx
                    }
                }
            } else {
                match wide_locs_spill
                    .iter()
                    .position(|known| known.with_slice(|s| s == wanted))
                {
                    Some(existing) => existing as u32,
                    None => {
                        let idx = wide_locs_spill.len() as u32;
                        if idx > PackedLoc::INDEX_MASK {
                            return None;
                        }
                        let mut locs = Locations::new();
                        locs.extend_from_slice(wanted);
                        wide_locs_spill.push(locs);
                        *wide_locs_len = wide_locs_spill.len();
                        idx
                    }
                }
            };
            Some(PackedLoc(PackedLoc::WIDE_MASK | at))
        }
    }
}

#[inline]
fn finish_met_grid_steps(
    small_steps: SmallCopyBuf<SmallGridStep, 64>,
    wide_steps: Vec<GridStep>,
) -> GridSteps {
    if !wide_steps.is_empty() {
        return GridSteps::Wide(wide_steps.into_boxed_slice());
    }
    let len = small_steps.len();
    if len <= MAX_INLINE_GRID_LEN {
        let mut inline = [GridSteps::ZERO_SMALL; MAX_INLINE_GRID_LEN];
        inline[..len].copy_from_slice(small_steps.as_slice());
        GridSteps::Inline(len as u8, inline)
    } else {
        GridSteps::Small(small_steps.into_boxed_slice())
    }
}

#[inline]
fn finish_wide_locs(
    inline: &mut [Option<Locations>; 4],
    len: usize,
    spill: Vec<Locations>,
) -> Box<[Locations]> {
    if !spill.is_empty() {
        return spill.into_boxed_slice();
    }
    if len == 0 {
        return Box::from([]);
    }
    inline[..len]
        .iter_mut()
        .map(|slot| slot.take().expect("wide location populated"))
        .collect()
}

/// Whether the part of each of `places` lies in one cycle, in every turn of
/// a period of `period` cycles.
///
/// A period of `p/q` cycles begins its turns at each multiple of `1/q` of
/// a cycle, in some order. So a part lies in one cycle in every turn when
/// it lies in one of those `q` parts of a cycle: `q` times the part lies
/// in one cycle. Whole numbers compare, with no fraction made. `false`
/// also for a time that leaves their range: a cut then settles it.
pub(super) fn lie_in_spans(period: Fraction, parts: impl IntoIterator<Item = TimeSpan>) -> bool {
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
