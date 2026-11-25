//! Layer meeting (`met`, `met_grids`), integer-tick run packing, and cycle
//! containment checks split out of `sequence.rs` to keep module size bounded.

use super::*;

/// Stack-resident builder that packs multi-location runs into `SmallCopyBuf<u32, 32>`
/// with O(1) deduplication via a 256-slot open-addressed table and previous-run cache.
pub(super) struct PackedRunsBuilder {
    runs: SmallCopyBuf<u32, 32>,
    ht: [u16; 256],
    ht_count: usize,
    prev: ([u32; 4], usize, usize),
}

impl PackedRunsBuilder {
    #[inline]
    pub(super) fn new() -> Self {
        Self {
            runs: SmallCopyBuf::new(0),
            ht: [u16::MAX; 256],
            ht_count: 0,
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

    #[inline]
    pub(super) fn intern_run(&mut self, needle: &[u32]) -> Option<usize> {
        let n = needle.len();
        debug_assert!(n >= 2);
        if n <= 4 && self.prev.1 == n && self.prev.0[..n] == *needle {
            return Some(self.prev.2);
        }
        let first_word = needle[0];
        let start = if self.ht_count < 128 {
            let existing = self.runs.as_slice();
            let h = (u64::from(first_word).wrapping_mul(0x9e37_79b9)
                ^ u64::from(needle[n - 1]).wrapping_mul(0x517c_c1b7))
                >> 8;
            let mut slot = (h as usize) & 255;
            let found = loop {
                let entry = self.ht[slot];
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
                        self.ht[slot] = start as u16;
                        self.ht_count += 1;
                    }
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
        let gcd = |mut a: u32, mut b: u32| -> u32 {
            while b != 0 {
                let r = a % b;
                a = b;
                b = r;
            }
            a
        };
        let lcm = |a: u32, b: u32| -> Option<u32> { (a / gcd(a, b)).checked_mul(b) };
        let turns = u32::try_from(period.checked_div(own_period)?.numer()).ok()?;
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
                    #[derive(Clone, Copy)]
                    struct MetStep {
                        part_begin: u32,
                        part_end: u32,
                        value: u16,
                        loc_start: u16,
                        loc_len: u8,
                    }
                    let mut met_steps = SmallCopyBuf::<MetStep, 32>::new(MetStep {
                        part_begin: 0,
                        part_end: 0,
                        value: 0,
                        loc_start: 0,
                        loc_len: 0,
                    });
                    let mut loc_buf = SmallCopyBuf::<(u32, u16), 128>::new((0, 0));
                    let mut own_step_locs = SmallCopyBuf::<(u32, u16), 8>::new((0, 0));
                    let mut tick_cuts = SmallCopyBuf::<Cut, 16>::new(Cut {
                        index: 0,
                        begin: 0,
                        end: 0,
                    });
                    let mut pairs = SmallCopyBuf::<(u32, u32), 16>::new((0, 0));
                    let own_vlen = self.values.len();
                    let other_vlen = values.values.len().max(1);
                    let use_pair_map = own_vlen > 0 && own_vlen.saturating_mul(other_vlen) <= 256;
                    let mut pair_map_storage;
                    let pair_map: &mut [u16] = if use_pair_map {
                        pair_map_storage = [u16::MAX; 256];
                        &mut pair_map_storage
                    } else {
                        &mut []
                    };
                    let mut in_cycles = true;
                    let turns_den = u64::from(p_den);
                    let ticks64 = u64::from(ticks);
                    let mut base = dress_base;
                    let own_base = own.base & !Grid::SMALL;
                    let other_base = other.base & !Grid::SMALL;
                    let own_has_cuts = own.rare.is_some();
                    let mut prev_at = u32::MAX;
                    let mut prev_own_b = 0u32;
                    let mut prev_j = 0usize;
                    let mut prev_piece_b = 0u32;
                    let mut prev_piece_e = 0u32;
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
                            let mut single_own_loc = [(0u32, 0u16); 1];
                            let around_sl: &[(u32, u16)] =
                                if !has_self_dress && (own_step.loc.0 & PackedLoc::SPARE) == 0 {
                                    let packed = own_step.loc.0;
                                    if packed == 0 {
                                        &[]
                                    } else {
                                        let b = own_base + (packed >> PackedLoc::LENGTH_BITS);
                                        let l = ((packed & PackedLoc::LENGTH_MASK) - 1) as u16;
                                        base = base.min(b);
                                        single_own_loc[0] = (b, l);
                                        &single_own_loc
                                    }
                                } else {
                                    own_step_locs.clear();
                                    for &loc in self_before_sl {
                                        own_step_locs.push(loc);
                                    }
                                    own.push_inline_locs_raw(i, &mut own_step_locs, &mut base)?;
                                    for &loc in self_after_sl {
                                        own_step_locs.push(loc);
                                    }
                                    own_step_locs.as_slice()
                                };
                            let first_at = own_b / p2_ticks;
                            let last_at = (own_e - 1) / p2_ticks;
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
                                    if met_steps.len() >= MAX_FOLDED_STEPS {
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
                                    let value = u16::try_from(pair_idx).ok()?;
                                    let loc_start = u16::try_from(loc_buf.len()).ok()?;
                                    let push_other_loc =
                                        |loc_buf: &mut SmallCopyBuf<(u32, u16), 128>,
                                         base: &mut u32|
                                         -> Option<()> {
                                            if !has_other_dress
                                                && (other_step.loc.0 & PackedLoc::SPARE) == 0
                                            {
                                                let packed = other_step.loc.0;
                                                if packed != 0 {
                                                    let b = other_base
                                                        + (packed >> PackedLoc::LENGTH_BITS);
                                                    let l = ((packed & PackedLoc::LENGTH_MASK) - 1)
                                                        as u16;
                                                    *base = (*base).min(b);
                                                    loc_buf.push((b, l));
                                                }
                                                return Some(());
                                            }
                                            for &loc in other_before_sl {
                                                loc_buf.push(loc);
                                            }
                                            other.push_inline_locs_raw(j, loc_buf, base)?;
                                            for &loc in other_after_sl {
                                                loc_buf.push(loc);
                                            }
                                            Some(())
                                        };
                                    match side {
                                        Side::Functions => {
                                            push_other_loc(&mut loc_buf, &mut base)?;
                                            for &loc in around_sl {
                                                loc_buf.push(loc);
                                            }
                                        }
                                        Side::Values => {
                                            for &loc in around_sl {
                                                loc_buf.push(loc);
                                            }
                                            push_other_loc(&mut loc_buf, &mut base)?;
                                        }
                                    }
                                    let n_locs = loc_buf.len() - usize::from(loc_start);
                                    if n_locs > 16 {
                                        return None;
                                    }
                                    let begins = u64::from(piece_b) * turns_den;
                                    let ends = u64::from(piece_e) * turns_den;
                                    if ends > (begins / ticks64 + 1) * ticks64 {
                                        in_cycles = false;
                                    }
                                    let step_idx = met_steps.len() as u32;
                                    if piece_b != own_whole_b || piece_e != own_whole_e {
                                        tick_cuts.push(Cut {
                                            index: step_idx,
                                            begin: own_whole_b,
                                            end: own_whole_e,
                                        });
                                    }
                                    met_steps.push(MetStep {
                                        part_begin: piece_b,
                                        part_end: piece_e,
                                        value,
                                        loc_start,
                                        loc_len: n_locs as u8,
                                    });
                                    j += 1;
                                }
                            }
                        }
                    }
                    let base = if base == u32::MAX { 0 } else { base };
                    if base & Grid::SMALL != 0 {
                        return None;
                    }
                    let slice = met_steps.as_slice();
                    let locs_all = loc_buf.as_slice();
                    let mut packed_runs = PackedRunsBuilder::new();
                    let steps = GridSteps::build(slice.len(), |index| {
                        let step = slice[index];
                        let n = usize::from(step.loc_len);
                        let loc = match n {
                            0 => PackedLoc(0),
                            1 => {
                                let (b, l) = locs_all[usize::from(step.loc_start)];
                                PackedLoc::range_raw(base, b, u32::from(l))?
                            }
                            _ => {
                                let start = usize::from(step.loc_start);
                                let mut run = [0u32; 16];
                                for (k, &(b, l)) in locs_all[start..start + n].iter().enumerate() {
                                    run[k] = PackedLoc::range_raw(base, b, u32::from(l))?.0;
                                }
                                run[n - 1] |= PackedLoc::SPARE;
                                let run_start = packed_runs.intern_run(&run[..n])?;
                                PackedLoc(PackedLoc::SPARE | run_start as u32)
                            }
                        };
                        Some(GridStep {
                            part_begin: step.part_begin,
                            part_end: step.part_end,
                            value: u32::from(step.value),
                            loc,
                        })
                    })?;
                    let (steps, rare) = match steps {
                        GridSteps::Wide(wide_steps) => (
                            GridSteps::Small(Box::from([])),
                            Some(Box::new(SpareRare::from_parts(
                                tick_cuts.as_slice(),
                                Box::from([]),
                                wide_steps,
                            ))),
                        ),
                        other if !tick_cuts.is_empty() => (
                            other,
                            Some(Box::new(SpareRare::from_parts(
                                tick_cuts.as_slice(),
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
                    met.change_dress(|dress| {
                        dress.clear_before();
                        if side == Side::Values {
                            dress.clear_after();
                        }
                        dress.set_keys(keys.clone());
                    });
                    debug_assert!(met.is_in_order());
                    Some((met, pairs))
                })
            })
            .flatten()
            .flatten()
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
