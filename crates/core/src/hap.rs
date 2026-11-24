//! `TimeSpan`, `Hap`, `State`, and hap ordering.

use super::*;

// ---------------------------------------------------------------------------
// TimeSpan
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TimeSpan {
    pub begin: Fraction,
    pub end: Fraction,
}

impl TimeSpan {
    pub fn new(begin: Fraction, end: Fraction) -> Self {
        Self { begin, end }
    }

    /// Splits the span at cycle boundaries, passing each piece to `visit`
    /// without heap allocation. Returns `false` if the span exceeds
    /// [`MAX_QUERY_SPAN_CYCLES`] (after publishing the typed refusal).
    pub(crate) fn for_each_cycle(&self, mut visit: impl FnMut(TimeSpan)) -> bool {
        let mut begin = self.begin;
        let end = self.end;
        if begin == end {
            visit(TimeSpan::new(begin, end));
            return true;
        }
        let Some(count) = self
            .cycle_count()
            .filter(|count| *count <= MAX_QUERY_SPAN_CYCLES)
        else {
            refuse(QueryLimit::QuerySpan {
                cycles: MAX_QUERY_SPAN_CYCLES,
            });
            return false;
        };
        let _ = count;
        let end_sam = end.sam();
        while end > begin {
            if begin.sam() == end_sam {
                visit(TimeSpan::new(begin, end));
                break;
            }
            let next_begin = begin.next_sam();
            visit(TimeSpan::new(begin, next_begin));
            begin = next_begin;
        }
        true
    }

    /// Splits the span at cycle boundaries.
    pub fn span_cycles(&self) -> Vec<TimeSpan> {
        let mut begin = self.begin;
        let end = self.end;
        let end_sam = end.sam();

        // Zero-width spans are supported and yield exactly themselves.
        if begin == end {
            return vec![TimeSpan::new(begin, end)];
        }

        // This loop pushes one entry per cycle, so its allocation is driven by
        // user-controlled span width. `fast(1e30)` would otherwise request a
        // vector with roughly 10^30 entries; no observable result exists for
        // that input, so the runtime refuses it.
        //
        // The failure travels on the typed resource-refusal channel so a
        // caller cannot mistake an uncomputed oversized query for silence.
        let Some(count) = self
            .cycle_count()
            .filter(|count| *count <= MAX_QUERY_SPAN_CYCLES)
        else {
            refuse(QueryLimit::QuerySpan {
                cycles: MAX_QUERY_SPAN_CYCLES,
            });
            return Vec::new();
        };

        // The count is at most the limit above, so it is a size.
        let mut spans = Vec::with_capacity(usize::try_from(count).unwrap_or_default());
        while end > begin {
            if begin.sam() == end_sam {
                spans.push(TimeSpan::new(begin, self.end));
                break;
            }
            let next_begin = begin.next_sam();
            spans.push(TimeSpan::new(begin, next_begin));
            begin = next_begin;
        }
        spans
    }

    /// Stack-backed cycle split for spans covering at most 8 cycles.
    /// Returns `None` when the span covers more than 8 cycles or exceeds limits,
    /// in which case callers fall back to [`Self::span_cycles`].
    #[inline]
    pub(crate) fn span_cycles_small(&self) -> Option<([TimeSpan; 8], usize)> {
        let mut begin = self.begin;
        let end = self.end;
        let zero = TimeSpan::new(Fraction::ZERO, Fraction::ZERO);
        let mut buf = [zero; 8];
        if begin == end {
            buf[0] = TimeSpan::new(begin, end);
            return Some((buf, 1));
        }
        let count = self.cycle_count().filter(|count| *count <= 8)?;
        if count <= 0 {
            return Some((buf, 0));
        }
        let end_sam = end.sam();
        let mut len = 0usize;
        while end > begin {
            if begin.sam() == end_sam {
                buf[len] = TimeSpan::new(begin, end);
                len += 1;
                break;
            }
            let next_begin = begin.next_sam();
            buf[len] = TimeSpan::new(begin, next_begin);
            len += 1;
            begin = next_begin;
        }
        Some((buf, len))
    }

    /// How many entries `span_cycles` produces for a span with a width, or
    /// `None` for a count outside the native range.
    ///
    /// Computed from the endpoints, never by iterating - iteration is the
    /// thing being guarded against.
    pub(crate) fn cycle_count(&self) -> Option<i128> {
        if self.end <= self.begin {
            return Some(0);
        }
        // `span_cycles` emits one entry for every integer cycle bucket the
        // half-open span touches. `ceil(end) - floor(begin)` is that count
        // exactly; using floor(width) allowed one extra allocation whenever
        // either edge was fractional.
        let first_cycle = self.begin.floor().numer();
        let after_last_cycle = self.end.ceil().numer();
        after_last_cycle.checked_sub(first_cycle)
    }

    pub fn duration(&self) -> Fraction {
        self.end.sub(self.begin)
    }

    /// Shift to an equal-duration span whose begin is in cycle zero.
    pub fn cycle_arc(&self) -> TimeSpan {
        let begin = self.begin.cycle_pos();
        TimeSpan::new(begin, begin.add(self.duration()))
    }

    pub fn with_time(&self, f: impl Fn(Fraction) -> Fraction) -> TimeSpan {
        TimeSpan::new(f(self.begin), f(self.end))
    }

    pub fn with_end(&self, f: impl Fn(Fraction) -> Fraction) -> TimeSpan {
        TimeSpan::new(self.begin, f(self.end))
    }

    pub fn with_cycle(&self, f: impl Fn(Fraction) -> Fraction) -> TimeSpan {
        let sam = self.begin.sam();
        TimeSpan::new(
            sam.add(f(self.begin.sub(sam))),
            sam.add(f(self.end.sub(sam))),
        )
    }

    /// Returns `None` when the spans do not intersect.
    ///
    /// Zero-width rule (the classic off-by-one source): a point intersection
    /// does NOT count if it sits at the end of a non-zero-width span.
    #[inline]
    pub fn intersection(&self, other: &TimeSpan) -> Option<TimeSpan> {
        if self.begin == other.begin && self.end == other.end {
            return match self.begin.cmp(&self.end) {
                std::cmp::Ordering::Greater => None,
                _ => Some(*self),
            };
        }
        let b = if self.begin >= other.begin {
            self.begin
        } else {
            other.begin
        };
        let e = if self.end <= other.end {
            self.end
        } else {
            other.end
        };
        match b.cmp(&e) {
            std::cmp::Ordering::Less => Some(TimeSpan::new(b, e)),
            std::cmp::Ordering::Greater => None,
            std::cmp::Ordering::Equal => {
                if (b == self.end && self.begin < self.end)
                    || (b == other.end && other.begin < other.end)
                {
                    None
                } else {
                    Some(TimeSpan::new(b, e))
                }
            }
        }
    }

    pub fn midpoint(&self) -> Fraction {
        self.begin.add(self.duration().div(Fraction::int(2)))
    }

    pub fn show(&self) -> String {
        format!("{} → {}", self.begin.show(), self.end.show())
    }
}

/// Largest number of cycle splits `span_cycles` will materialise.
///
/// Real queries span a handful of cycles; the offline renderer's longest
/// documented run is far below this. The bound exists so a hostile or mistaken
/// factor fails fast instead of exhausting memory.
pub const MAX_QUERY_SPAN_CYCLES: i128 = 1_000_000;

/// How deep `Pattern::query` may recurse before refusing.
///
/// The limit is sized for unoptimised stack frames, which are substantially
/// larger than release frames. A value of 512 leaves conservative headroom on
/// the CLI's 64 MiB worker stack. Some combinators create several nodes, so
/// a chain of them reaches the limit in fewer than 512 operations.
pub const MAX_PATTERN_DEPTH: u32 = 512;

/// `sortHapsByPart()`, including its throw on analog haps.
///
/// Shared by every entry point that claims this ordering, so a second path
/// cannot quietly use a more tolerant comparator. See
/// [`Pattern::query_arc_sorted`] for why the throw is preserved.
pub fn sort_haps_by_part(haps: Vec<Hap>) -> Vec<Hap> {
    // One element: `Array.prototype.sort` never calls the comparator, so an
    // analog hap survives.
    if haps.len() < 2 {
        return haps;
    }
    if haps.iter().any(|hap| hap.whole.is_none()) {
        signal_query_error(|| {
            "sortHapsByPart: Cannot read properties of undefined (reading 'begin')".into()
        });
        return Vec::new();
    }
    // Every hap is discrete here, so `query_order` compares part begin, part
    // end, whole begin and whole end.
    stable_sorted(haps)
}

/// Sort host results in place without discarding continuous haps.
pub(crate) fn sort_haps_for_query_in_place(haps: &mut [Hap]) {
    stable_sort_in_place(haps);
}

/// Sort host results without discarding continuous haps that have no whole span.
/// Explicit `sortHapsByPart()` calls keep the comparator above.
pub fn sort_haps_for_query(mut haps: Vec<Hap>) -> Vec<Hap> {
    stable_sort_in_place(&mut haps);
    haps
}

/// Part begin, part end, then the whole. An analog hap's missing whole
/// sorts first.
#[inline(always)]
pub(crate) fn query_order(a: &Hap, b: &Hap) -> std::cmp::Ordering {
    if a.part.begin != b.part.begin {
        return a.part.begin.cmp(&b.part.begin);
    }
    if a.part.end != b.part.end {
        return a.part.end.cmp(&b.part.end);
    }
    match (&a.whole, &b.whole) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(aw), Some(bw)) => {
            if aw.begin != bw.begin {
                aw.begin.cmp(&bw.begin)
            } else if aw.end != bw.end {
                aw.end.cmp(&bw.end)
            } else {
                std::cmp::Ordering::Equal
            }
        }
    }
}

/// The most haps that [`stable_sorted`] sorts where they are: what a short
/// span answers, as playback asks one. The answer of a run takes room for
/// as many.
pub(crate) const FEW_HAPS: usize = 64;

/// A float that orders as `time` does where the floats of two times differ:
/// the float of a time whose parts a float holds exactly, which the one
/// rounding of their division keeps in order. `NAN` for any other time,
/// which only the time itself orders.
#[inline(always)]
pub(crate) fn order_key(time: &Fraction) -> f64 {
    /// The first whole number that a float skips.
    const EXACT: u128 = 1 << 53;
    let n = time.numer();
    let d = time.denom();
    if n.unsigned_abs() < EXACT && d.unsigned_abs() < EXACT {
        (n as i64) as f64 / (d as i64) as f64
    } else {
        f64::NAN
    }
}

/// Precomputed float keys for [`query_order`] across part begin and part end.
#[derive(Clone, Copy)]
struct OrderKey {
    pb: f64,
    pe: f64,
}

impl OrderKey {
    const ZERO: Self = Self { pb: 0.0, pe: 0.0 };

    #[inline(always)]
    pub(crate) fn of(hap: &Hap, prev: Option<(&Hap, Self)>) -> Self {
        let pb = match prev {
            Some((ph, pk)) if ph.part.end == hap.part.begin => pk.pe,
            Some((ph, pk)) if ph.part.begin == hap.part.begin => pk.pb,
            _ => order_key(&hap.part.begin),
        };
        let pe = if hap.part.end == hap.part.begin {
            pb
        } else {
            match prev {
                Some((ph, pk)) if ph.part.end == hap.part.end => pk.pe,
                _ => order_key(&hap.part.end),
            }
        };
        Self { pb, pe }
    }
}

#[inline(always)]
fn cmp_order_keys(ak: OrderKey, bk: OrderKey, a: &Hap, b: &Hap) -> std::cmp::Ordering {
    if ak.pb < bk.pb {
        return std::cmp::Ordering::Less;
    }
    if ak.pb > bk.pb {
        return std::cmp::Ordering::Greater;
    }
    if a.part.begin != b.part.begin {
        return a.part.begin.cmp(&b.part.begin);
    }
    if ak.pe < bk.pe {
        return std::cmp::Ordering::Less;
    }
    if ak.pe > bk.pe {
        return std::cmp::Ordering::Greater;
    }
    if a.part.end != b.part.end {
        return a.part.end.cmp(&b.part.end);
    }
    match (&a.whole, &b.whole) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(aw), Some(bw)) => {
            if aw.begin != bw.begin {
                aw.begin.cmp(&bw.begin)
            } else if aw.end != bw.end {
                aw.end.cmp(&bw.end)
            } else {
                std::cmp::Ordering::Equal
            }
        }
    }
}

#[inline(never)]
fn sort_few_haps_direct<const CAP: usize>(haps: &mut [Hap], first_bad: usize) {
    let n = haps.len();
    let mut indices = [0u8; CAP];
    for (slot, dst) in indices[..n].iter_mut().enumerate() {
        *dst = slot as u8;
    }
    for i in first_bad..n {
        let key = indices[i];
        let key_hap = &haps[key as usize];
        let mut j = i;
        while j > 0 {
            let prev_idx = indices[j - 1] as usize;
            if query_order(&haps[prev_idx], key_hap) != std::cmp::Ordering::Greater {
                break;
            }
            indices[j] = indices[j - 1];
            j -= 1;
        }
        indices[j] = key;
    }
    for i in 0..n {
        let mut dst = i;
        let mut src = indices[dst] as usize;
        if src == dst {
            continue;
        }
        while src != i {
            haps.swap(dst, src);
            indices[dst] = dst as u8;
            dst = src;
            src = indices[dst] as usize;
        }
        indices[dst] = dst as u8;
    }
}

#[inline(never)]
fn sort_few_haps<const CAP: usize>(haps: &mut [Hap], first_bad: usize) {
    let n = haps.len();
    let mut keys = [OrderKey::ZERO; CAP];
    keys[0] = OrderKey::of(&haps[0], None);
    for i in 1..n {
        keys[i] = OrderKey::of(&haps[i], Some((&haps[i - 1], keys[i - 1])));
    }
    let mut indices = [0u8; CAP];
    for (slot, dst) in indices[..n].iter_mut().enumerate() {
        *dst = slot as u8;
    }
    for i in first_bad..n {
        let key = indices[i];
        let key_idx = key as usize;
        let key_k = keys[key_idx];
        let key_hap = &haps[key_idx];
        let mut j = i;
        while j > 0 {
            let prev_idx = indices[j - 1] as usize;
            let greater = cmp_order_keys(keys[prev_idx], key_k, &haps[prev_idx], key_hap)
                == std::cmp::Ordering::Greater;
            if !greater {
                break;
            }
            indices[j] = indices[j - 1];
            j -= 1;
        }
        indices[j] = key;
    }
    // Apply the permutation in place by following disjoint cycles so each
    // hap moves once per cycle step with zero heap allocation.
    for i in 0..n {
        let mut dst = i;
        let mut src = indices[dst] as usize;
        if src == dst {
            continue;
        }
        while src != i {
            haps.swap(dst, src);
            indices[dst] = dst as u8;
            dst = src;
            src = indices[dst] as usize;
        }
        indices[dst] = dst as u8;
    }
}

/// Sort `65..=512` out-of-order haps on the stack without heap allocations or
/// thread-local retained buffers. Exploits natural sorted runs (multi-layer
/// sequences emit one sorted run per layer) with a bottom-up pairwise merge.
#[inline(never)]
fn sort_medium_haps(haps: &mut [Hap]) {
    const CAP: usize = 512;
    const MAX_RUNS: usize = 16;
    let n = haps.len();
    debug_assert!(n <= CAP);
    let mut keys = [OrderKey::ZERO; CAP];
    let mut prev: Option<(&Hap, OrderKey)> = None;
    for (slot, hap) in keys[..n].iter_mut().zip(haps.iter()) {
        let k = OrderKey::of(hap, prev);
        *slot = k;
        prev = Some((hap, k));
    }
    let mut indices = [0u16; CAP];
    for (idx, slot) in indices[..n].iter_mut().enumerate() {
        *slot = idx as u16;
    }
    // Detect natural ascending runs: stacked multi-layer queries yield one
    // sorted run per layer, so bottom-up pairwise merge does O(N log K) work.
    let mut runs = [(0u16, 0u16); MAX_RUNS];
    let mut run_count = 0usize;
    let mut run_start = 0usize;
    let mut too_many_runs = false;
    for i in 1..n {
        if cmp_order_keys(keys[i - 1], keys[i], &haps[i - 1], &haps[i])
            == std::cmp::Ordering::Greater
        {
            if run_count >= MAX_RUNS - 1 {
                too_many_runs = true;
                break;
            }
            runs[run_count] = (run_start as u16, i as u16);
            run_count += 1;
            run_start = i;
        }
    }
    if !too_many_runs {
        runs[run_count] = (run_start as u16, n as u16);
        run_count += 1;
        let mut tmp = [0u16; CAP];
        let mut src_is_indices = true;
        while run_count > 1 {
            let (src, dst) = if src_is_indices {
                (&indices[..n], &mut tmp[..n])
            } else {
                (&tmp[..n], &mut indices[..n])
            };
            let mut next_runs = 0usize;
            let mut r = 0usize;
            while r + 1 < run_count {
                let (a_start, a_end) = (usize::from(runs[r].0), usize::from(runs[r].1));
                let (b_start, b_end) = (usize::from(runs[r + 1].0), usize::from(runs[r + 1].1));
                let mut i = a_start;
                let mut j = b_start;
                let mut out = a_start;
                while i < a_end && j < b_end {
                    let ai = usize::from(src[i]);
                    let bi = usize::from(src[j]);
                    if cmp_order_keys(keys[ai], keys[bi], &haps[ai], &haps[bi])
                        != std::cmp::Ordering::Greater
                    {
                        dst[out] = src[i];
                        i += 1;
                    } else {
                        dst[out] = src[j];
                        j += 1;
                    }
                    out += 1;
                }
                if i < a_end {
                    let rem = a_end - i;
                    dst[out..out + rem].copy_from_slice(&src[i..a_end]);
                } else if j < b_end {
                    let rem = b_end - j;
                    dst[out..out + rem].copy_from_slice(&src[j..b_end]);
                }
                runs[next_runs] = (a_start as u16, b_end as u16);
                next_runs += 1;
                r += 2;
            }
            if r < run_count {
                let (s, e) = (usize::from(runs[r].0), usize::from(runs[r].1));
                dst[s..e].copy_from_slice(&src[s..e]);
                runs[next_runs] = runs[r];
                next_runs += 1;
            }
            run_count = next_runs;
            src_is_indices = !src_is_indices;
        }
        if !src_is_indices {
            indices[..n].copy_from_slice(&tmp[..n]);
        }
    } else {
        indices[..n].sort_unstable_by(|&a, &b| {
            let ai = usize::from(a);
            let bi = usize::from(b);
            cmp_order_keys(keys[ai], keys[bi], &haps[ai], &haps[bi]).then_with(|| a.cmp(&b))
        });
    }
    for i in 0..n {
        let mut dst = i;
        let mut src = usize::from(indices[dst]);
        if src == dst {
            continue;
        }
        while src != i {
            haps.swap(dst, src);
            indices[dst] = dst as u16;
            dst = src;
            src = usize::from(indices[dst]);
        }
        indices[dst] = dst as u16;
    }
}

/// A stable sort of `haps` in place by [`query_order`].
///
/// Already-sorted runs return with zero allocations, runs up to 512 haps sort
/// on the stack, and larger out-of-order runs permute in place by cycle
/// decomposition without allocating a second `Vec<Hap>`.
pub(crate) fn stable_sort_in_place(haps: &mut [Hap]) {
    let n = haps.len();
    if n <= 1 {
        return;
    }
    let Some(first_bad) =
        (1..n).find(|&at| query_order(&haps[at - 1], &haps[at]) == std::cmp::Ordering::Greater)
    else {
        return;
    };
    if n <= 24 {
        sort_few_haps_direct::<24>(haps, first_bad);
        return;
    }
    if n <= FEW_HAPS {
        sort_few_haps::<FEW_HAPS>(haps, first_bad);
        return;
    }
    if n <= 512 {
        sort_medium_haps(haps);
        return;
    }
    let Ok(len) = u32::try_from(n) else {
        haps.sort_by(query_order);
        return;
    };
    let mut keys = Vec::with_capacity(n);
    let mut prev: Option<(&Hap, OrderKey)> = None;
    for hap in haps.iter() {
        let k = OrderKey::of(hap, prev);
        keys.push(k);
        prev = Some((hap, k));
    }
    let mut indices: Vec<u32> = (0..len).collect();
    indices.sort_unstable_by(|&a, &b| {
        let ai = a as usize;
        let bi = b as usize;
        cmp_order_keys(keys[ai], keys[bi], &haps[ai], &haps[bi]).then_with(|| a.cmp(&b))
    });
    for i in 0..n {
        let mut dst = i;
        let mut src = indices[dst] as usize;
        if src == dst {
            continue;
        }
        while src != i {
            haps.swap(dst, src);
            indices[dst] = dst as u32;
            dst = src;
            src = indices[dst] as usize;
        }
        indices[dst] = dst as u32;
    }
}

/// A stable sort of `haps` by [`query_order`].
pub(crate) fn stable_sorted(mut haps: Vec<Hap>) -> Vec<Hap> {
    stable_sort_in_place(&mut haps);
    haps
}

/// The whole cycle containing `t`.
pub fn whole_cycle(t: Fraction) -> TimeSpan {
    TimeSpan::new(t.sam(), t.next_sam())
}

impl TimeSpan {
    /// Whether this span is not empty and ends inside the cycle it begins
    /// in, so [`TimeSpan::span_cycles`] gives it back whole.
    #[inline]
    pub(crate) fn within_one_cycle(&self) -> bool {
        let bd = self.begin.denom();
        if bd == self.end.denom() {
            let bn = self.begin.numer();
            let en = self.end.numer();
            if bn >= en {
                return false;
            }
            if bd == 1 {
                return en.checked_sub(bn) == Some(1);
            }
            if (bd as u128).is_power_of_two() {
                let shift = (bd as u128).trailing_zeros();
                return (bn >> shift) == ((en - 1) >> shift);
            }
        }
        if !(self.begin < self.end) {
            return false;
        }
        let begin_cycle = self.begin.floor_int();
        if self.end.denom() == 1 {
            begin_cycle
                .checked_add(1)
                .is_some_and(|next| self.end.numer() <= next)
        } else {
            // Reduced `end` with `denom > 1` is non-integer, so `end <= begin_cycle + 1`
            // holds iff `end.floor_int() <= begin_cycle`.
            self.end.floor_int() <= begin_cycle
        }
    }
}

// ---------------------------------------------------------------------------
// Hap
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Hap {
    /// `None` means analog/continuous: the value was sampled at the part's
    /// midpoint and has no notional extent.
    pub whole: Option<TimeSpan>,
    pub part: TimeSpan,
    pub value: Value,
    /// Source spans, concatenated on every bind. Drives editor highlighting,
    /// so it must survive every combinator that carries context.
    pub context: Locations,
    /// Optional per-hap metadata (`scale`, `edoSize`, `scaleDefinition`,
    /// `tags`, `logLine`, live slider bindings, visual slots, and JS lookup
    /// shape). Most haps carry none, so this stays a null pointer.
    pub(crate) keys: ContextKeys,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<Hap>() == 192);

/// Read a `tags` key off a JS context object: an array of strings, anything
/// else ignored. A score that somehow writes a non-array there loses the tag,
/// not the hap.
pub(crate) fn tags_from_value(value: &Value) -> Option<Arc<[Arc<str>]>> {
    let Value::List(items) = value else {
        return None;
    };
    let tags: Vec<Arc<str>> = items
        .iter()
        .filter_map(|item| match item {
            // A handle of the text.
            Value::Str(text) => Some(text.to_arc()),
            _ => None,
        })
        .collect();
    (!tags.is_empty()).then(|| Arc::from(tags))
}

impl Hap {
    pub fn new(whole: Option<TimeSpan>, part: TimeSpan, value: Value) -> Self {
        Self {
            whole,
            part,
            value,
            context: Locations::new(),
            keys: ContextKeys::default(),
        }
    }

    #[inline(always)]
    pub(crate) fn of_sequence(
        whole: TimeSpan,
        part: TimeSpan,
        value: Value,
        context: Locations,
        keys: ContextKeys,
    ) -> Self {
        Self {
            whole: Some(whole),
            part,
            value,
            context,
            keys,
        }
    }

    pub(crate) fn with_pick_lookup(mut self, lookup: Arc<PickLookup>) -> Self {
        self.keys.edit().pick_lookup = Some(lookup);
        self
    }

    /// Direct live slider bindings: gain, low-pass cutoff, low-pass resonance,
    /// then the direct FM route's index. Zero is unbound.
    #[inline]
    pub fn live_controls(&self) -> [u64; 4] {
        self.keys.get().live_controls
    }

    #[inline]
    pub(crate) fn slider_binding(&self) -> u64 {
        self.keys.get().slider_binding
    }

    #[inline]
    pub(crate) fn set_live_control(&mut self, slot: usize, binding: u64) {
        if binding == 0 && self.keys.is_empty() {
            return;
        }
        self.keys.edit().live_controls[slot] = binding;
        if binding == 0 {
            self.keys.trim();
        }
    }

    #[inline]
    pub(crate) fn set_live_bindings(&mut self, live_controls: [u64; 4], slider_binding: u64) {
        if live_controls == [0; 4] && slider_binding == 0 {
            if !self.keys.is_empty() {
                let keys = self.keys.get();
                if keys.live_controls != [0; 4] || keys.slider_binding != 0 {
                    let edit = self.keys.edit();
                    edit.live_controls = [0; 4];
                    edit.slider_binding = 0;
                    self.keys.trim();
                }
            }
            return;
        }
        let edit = self.keys.edit();
        edit.live_controls = live_controls;
        edit.slider_binding = slider_binding;
    }

    #[inline]
    pub(crate) fn clear_live_bindings(&mut self) {
        self.set_live_bindings([0; 4], 0);
    }

    #[inline]
    pub(crate) fn pick_lookup_arc(&self) -> Option<Arc<PickLookup>> {
        self.keys.get().pick_lookup.clone()
    }

    /// `hap.context.scale`, exactly as `.scale()` tagged it.
    pub fn scale_context(&self) -> Option<&str> {
        self.keys.get().scale.as_deref()
    }

    /// Set the scale context tag.
    pub fn with_scale_context(mut self, scale: Arc<str>) -> Self {
        self.keys.edit().scale = Some(scale);
        self
    }

    /// `hap.context.edoSize`, exactly as `.xen()` tagged it.
    pub fn edo_size_context(&self) -> Option<f64> {
        self.keys.get().edo_size
    }

    /// Every tag this hap carries, oldest first.
    pub fn tags_context(&self) -> Option<&[Arc<str>]> {
        self.keys.get().tags.as_deref()
    }

    /// The text `.log()` will print when this hap triggers, if any.
    pub fn log_line(&self) -> Option<&str> {
        self.keys.get().log_line.as_deref()
    }

    /// `hap.hasTag(t)` - what a `filter` callback asks.
    pub fn has_tag(&self, tag: &str) -> bool {
        self.keys
            .get()
            .tags
            .as_deref()
            .is_some_and(|tags| tags.iter().any(|held| &**held == tag))
    }

    /// Append a tag - never replaces, so tagging twice keeps both.
    pub fn with_tag_context(mut self, tag: Arc<str>) -> Self {
        let mut tags: Vec<Arc<str>> = self.tags_context().unwrap_or_default().to_vec();
        tags.push(tag);
        self.keys.edit().tags = Some(Arc::from(tags));
        self
    }

    /// `.log()`'s already-formatted text, to be printed when this hap
    /// triggers.
    pub fn with_log_line_context(mut self, line: impl Into<Arc<str>>) -> Self {
        self.keys.edit().log_line = Some(line.into());
        self
    }

    pub fn with_edo_size_context(mut self, edo_size: f64) -> Self {
        self.keys.edit().edo_size = Some(edo_size);
        self
    }

    /// `hap.context.scaleDefinition`, as `edoScale` tagged it. Stored behind
    /// an `Arc`; callback-visible JSON matches the raw argument object.
    pub fn scale_definition_context(&self) -> Option<&Value> {
        self.keys.get().scale_definition.as_deref()
    }

    /// Set the scale-definition context tag.
    pub fn with_scale_definition_context(mut self, definition: Arc<Value>) -> Self {
        self.keys.edit().scale_definition = Some(definition);
        self
    }

    pub fn with_context(mut self, ctx: impl Into<Locations>) -> Self {
        self.context = ctx.into();
        self
    }

    pub fn ui_visuals_context(&self) -> u64 {
        self.keys.get().ui_visuals
    }

    pub fn with_ui_visual_slot(mut self, slot: u8) -> Self {
        self.keys.add_ui_visual_slot(slot);
        self
    }

    pub(crate) fn with_ui_visuals_context(mut self, visuals: u64) -> Self {
        if visuals != 0 || !self.keys.is_empty() {
            self.keys.edit().ui_visuals = visuals;
            if visuals == 0 {
                self.keys.trim();
            }
        }
        self
    }

    pub fn pick_lookup(&self) -> Option<&PickLookup> {
        self.keys.get().pick_lookup.as_deref()
    }

    pub fn whole_or_part(&self) -> TimeSpan {
        self.whole.unwrap_or(self.part)
    }

    /// Effective duration, honoring the `duration` and `clip` controls. A clip
    /// whose product with the duration leaves the native fraction range is
    /// ignored, and the un-clipped duration is returned.
    pub fn duration(&self) -> Fraction {
        let mut duration = self
            .value
            .get("duration")
            .and_then(Value::as_f64)
            .and_then(Fraction::from_f64)
            .or_else(|| self.whole.map(|whole| whole.duration()))
            .unwrap_or_else(|| self.part.duration());
        if let Some(clip) = self
            .value
            .get("clip")
            .and_then(Value::as_f64)
            .and_then(Fraction::from_f64)
        {
            duration = duration.checked_mul(clip).unwrap_or(duration);
        }
        duration
    }

    pub fn end_clipped(&self) -> Fraction {
        self.whole_or_part().begin.add(self.duration())
    }

    pub fn is_active(&self, current: Fraction) -> bool {
        let whole = self.whole_or_part();
        whole.begin <= current && self.end_clipped() >= current
    }

    pub fn is_in_past(&self, current: Fraction) -> bool {
        current > self.end_clipped()
    }

    pub fn is_in_near_past(&self, margin: Fraction, current: Fraction) -> bool {
        current.sub(margin) <= self.end_clipped()
    }

    pub fn is_in_future(&self, current: Fraction) -> bool {
        current < self.whole_or_part().begin
    }

    pub fn is_in_near_future(&self, margin: Fraction, current: Fraction) -> bool {
        let begin = self.whole_or_part().begin;
        current < begin && current > begin.sub(margin)
    }

    pub fn is_within_time(&self, min: Fraction, max: Fraction) -> bool {
        self.whole_or_part().begin <= max && self.end_clipped() >= min
    }

    /// Whether the whole begins exactly where the part does.
    pub fn has_onset(&self) -> bool {
        self.whole
            .map(|w| w.begin == self.part.begin)
            .unwrap_or(false)
    }

    pub fn with_span(&self, f: impl Fn(&TimeSpan) -> TimeSpan) -> Hap {
        Hap {
            whole: self.whole.as_ref().map(&f),
            part: f(&self.part),
            value: self.value.clone(),
            context: self.context.clone(),
            keys: self.keys.clone(),
        }
    }

    /// Changes the spans in place. The caller owns the hap, so no value or
    /// context is cloned.
    pub(crate) fn update_span(&mut self, f: impl Fn(&TimeSpan) -> TimeSpan) {
        if let Some(whole) = self.whole.as_mut() {
            *whole = f(whole);
        }
        self.part = f(&self.part);
    }

    /// Haps cut by the same query share the mapped times of its edges, and
    /// chord/stack haps sharing whole boundaries reuse the last mapped value.
    pub(crate) fn map_times(
        haps: &mut [Self],
        span: TimeSpan,
        edges: Option<(Fraction, Fraction)>,
        map: impl Fn(Fraction) -> Fraction,
    ) {
        let mut begin = edges.map(|(b, _)| b);
        let mut end = edges.map(|(_, e)| e);
        let mut last_wb: Option<(Fraction, Fraction)> = None;
        let mut last_we: Option<(Fraction, Fraction)> = None;
        for hap in haps {
            let part = hap.part;
            hap.part.begin = if part.begin == span.begin {
                *begin.get_or_insert_with(|| map(part.begin))
            } else if let Some((raw, mapped)) = last_wb
                && raw == part.begin
            {
                mapped
            } else if let Some((raw, mapped)) = last_we
                && raw == part.begin
            {
                mapped
            } else {
                let mapped = map(part.begin);
                last_wb = Some((part.begin, mapped));
                mapped
            };
            hap.part.end = if part.end == span.end {
                *end.get_or_insert_with(|| map(part.end))
            } else if let Some((raw, mapped)) = last_we
                && raw == part.end
            {
                mapped
            } else if let Some((raw, mapped)) = last_wb
                && raw == part.end
            {
                mapped
            } else {
                let mapped = map(part.end);
                last_we = Some((part.end, mapped));
                mapped
            };
            if let Some(whole) = &mut hap.whole {
                whole.begin = if whole.begin == part.begin {
                    hap.part.begin
                } else if let Some((raw, mapped)) = last_wb
                    && raw == whole.begin
                {
                    mapped
                } else {
                    let mapped = map(whole.begin);
                    last_wb = Some((whole.begin, mapped));
                    mapped
                };
                whole.end = if whole.end == part.end {
                    hap.part.end
                } else if let Some((raw, mapped)) = last_we
                    && raw == whole.end
                {
                    mapped
                } else {
                    let mapped = map(whole.end);
                    last_we = Some((whole.end, mapped));
                    mapped
                };
            }
        }
    }

    /// Replace only the visible part of a hap, retaining its notional whole.
    ///
    /// Native extension nodes use this for the same intersected-child shape
    /// as JavaScript's `new Hap(whole, part, value, context)` constructor.
    pub fn with_part(mut self, part: TimeSpan) -> Hap {
        self.part = part;
        self
    }

    /// JavaScript `hap.combineContext(other)`, retaining this hap's spans and
    /// value while merging the other hap's contextual metadata.
    pub fn combine_context(&self, other: &Hap) -> Hap {
        let mut context = self.context.clone();
        context.append(&other.context);
        let keys = ContextKeys::merge_with(
            &other.keys,
            &self.keys,
            self.live_controls(),
            self.slider_binding(),
            self.pick_lookup_arc(),
        );
        Hap {
            whole: self.whole,
            part: self.part,
            value: self.value.clone(),
            context,
            keys,
        }
    }

    pub fn with_value(&self, f: impl Fn(&Value) -> Value) -> Hap {
        let value = f(&self.value);
        let keys = if self.keys.is_empty() {
            ContextKeys::NONE
        } else {
            let mut keys = self.keys.clone();
            let k = keys.get();
            let drop_lookup = k.pick_lookup.is_some() && value != self.value;
            let clear_live = k.live_controls != [0; 4] || k.slider_binding != 0;
            if drop_lookup || clear_live {
                let edit = keys.edit();
                if drop_lookup {
                    edit.pick_lookup = None;
                }
                edit.live_controls = [0; 4];
                edit.slider_binding = 0;
                keys.trim();
            }
            keys
        };
        Hap {
            whole: self.whole,
            part: self.part,
            value,
            context: self.context.clone(),
            keys,
        }
    }

    /// Changes the value of an owned hap. The context moves instead of
    /// cloning. The lookup shape survives only when the value is unchanged.
    pub(crate) fn map_value(mut self, f: impl FnOnce(&Value) -> Value) -> Hap {
        self.map_value_in_place(f);
        self
    }

    /// [`Self::map_value`] without moving the hap.
    pub(crate) fn map_value_in_place(&mut self, f: impl FnOnce(&Value) -> Value) {
        let value = f(&self.value);
        if !self.keys.is_empty() {
            let k = self.keys.get();
            let drop_lookup = k.pick_lookup.is_some() && value != self.value;
            let clear_live = k.live_controls != [0; 4] || k.slider_binding != 0;
            if drop_lookup || clear_live {
                let edit = self.keys.edit();
                if drop_lookup {
                    edit.pick_lookup = None;
                }
                edit.live_controls = [0; 4];
                edit.slider_binding = 0;
                self.keys.trim();
            }
        }
        self.value = value;
    }

    /// The owned hap with a new value and the same live control bindings.
    /// Public because a caller that interns an [`OwnedHapMapFn`] for
    /// [`Pattern::fmap_keeping_live_with`] must keep exactly these
    /// semantics.
    pub fn into_value_keeping_live(mut self, value: Value) -> Hap {
        if self.keys.get().pick_lookup.is_some() {
            // The lookup shape was of the value before the change.
            self.keys.edit().pick_lookup = None;
            self.keys.trim();
        }
        self.value = value;
        self
    }

    pub(crate) fn without_live_controls(mut self) -> Self {
        self.clear_live_bindings();
        self
    }

    /// Trusted pitch rewrites can carry named audio controls unchanged.
    /// A raw slider value still loses its identity when its pitch changes.
    pub(crate) fn with_pitch_live_controls(mut self, input: &Hap) -> Self {
        self.clear_live_bindings();
        let input_live = input.live_controls();
        if input_live != [0; 4] {
            let mut kept = [0; 4];
            for (index, key) in ["gain", "cutoff", "resonance", "fmi"]
                .into_iter()
                .enumerate()
            {
                if input_live[index] != 0
                    && let Some(before) = input.value.get(key).and_then(Value::as_f64)
                    && self.value.get(key).and_then(Value::as_f64) == Some(before)
                {
                    kept[index] = input_live[index];
                }
            }
            if kept != [0; 4] {
                self.keys.edit().live_controls = kept;
            }
        }
        self
    }

    pub(crate) fn with_unchanged_live_controls(self, input: &Hap) -> Self {
        let mut mapped = self.with_pitch_live_controls(input);
        let binding = input.slider_binding();
        if binding != 0
            && let Some(before) = input.value.as_f64()
            && mapped.value.as_f64() == Some(before)
        {
            mapped.keys.edit().slider_binding = binding;
        }
        mapped
    }

    /// `Hap.show(true)` - the compact snapshot serialiser.
    ///
    /// Sigils: `⇜` whole starts before part · `(…)` fragment · `⇝` whole ends
    /// after part · `~` analog.
    ///
    pub fn show(&self) -> String {
        self.show_with(true)
    }

    pub fn show_with(&self, compact: bool) -> String {
        let value = match &self.value {
            Value::List(_) | Value::Object(_) => {
                if compact {
                    self.value.compact_json()
                } else {
                    self.value
                        .json_stringify()
                        .unwrap_or_else(|| "undefined".into())
                }
            }
            value => value.show(),
        };
        let spans = match self.whole {
            // Compatibility quirk: strudel.cc stringifies the show method
            // itself for analog haps (a missing `()`), and the exact text is
            // observable - so it is reproduced verbatim.
            None => "~show() {\n    return this.begin.show() + ' → ' + this.end.show();\n  }"
                .to_string(),
            Some(w) => {
                let is_whole = w.begin == self.part.begin && w.end == self.part.end;
                let mut s = String::new();
                if w.begin != self.part.begin {
                    s.push_str(&format!("{} ⇜ ", w.begin.show()));
                }
                if !is_whole {
                    s.push('(');
                }
                s.push_str(&self.part.show());
                if !is_whole {
                    s.push(')');
                }
                if w.end != self.part.end {
                    s.push_str(&format!(" ⇝ {}", w.end.show()));
                }
                s
            }
        };
        format!("[ {spans} | {value} ]")
    }

    pub fn show_whole(&self, compact: bool) -> String {
        let span = self
            .whole
            .map(|whole| whole.show())
            .unwrap_or_else(|| "~".into());
        let value = match &self.value {
            Value::List(_) | Value::Object(_) if compact => self.value.compact_json(),
            Value::List(_) | Value::Object(_) => self
                .value
                .json_stringify()
                .unwrap_or_else(|| "undefined".into()),
            value => value.show(),
        };
        format!("{span}: {value}")
    }
}

impl fmt::Debug for Hap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hap")
            .field("whole", &self.whole)
            .field("part", &self.part)
            .field("value", &self.value)
            .field("context", &self.context)
            .finish_non_exhaustive()
    }
}

/// A value-preserving bind may rebuild `pure(value)` to retime the hap. Carry
/// the exact lookup only when the inner result kept the SAME identity-bearing
/// value; a merely equal user value must not inherit hidden Pattern entries.
/// The hap an inner, outer or mix join makes of an outer hap `outer` and one
/// hap `inner` of the pattern it names, at `whole`. See `Node::Join`.
pub(crate) fn join_hap_in_place(outer: &Hap, inner: &mut Hap, whole: Option<TimeSpan>) {
    inner.whole = whole;
    if !outer.context.is_empty() {
        let mut context = outer.context.clone();
        context.append(&inner.context);
        inner.context = context;
    }
    if !outer.keys.is_none() {
        let pick_lookup = joined_pick_lookup(outer, inner);
        inner.keys = ContextKeys::merge_with(
            &inner.keys,
            &outer.keys,
            inner.live_controls(),
            inner.slider_binding(),
            pick_lookup,
        );
    }
}

pub(crate) fn joined_pick_lookup(outer: &Hap, inner: &Hap) -> Option<Arc<PickLookup>> {
    inner.pick_lookup_arc().or_else(|| {
        (outer.keys.get().pick_lookup.is_some() && inner.value == outer.value)
            .then(|| outer.pick_lookup_arc())
            .flatten()
    })
}

impl fmt::Display for Hap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.show())
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct State {
    pub span: TimeSpan,
    /// Controls carried through the query. `_cps` lives here - `glide`-style
    /// user patterns read it to tell a real trigger from a lookahead query.
    pub controls: OrderedMap,
}

impl State {
    pub fn new(span: TimeSpan) -> Self {
        Self {
            span,
            controls: OrderedMap::new(),
        }
    }
    pub fn set_span(&self, span: TimeSpan) -> Self {
        Self {
            span,
            controls: self.controls.clone(),
        }
    }
    pub fn with_span(&self, f: impl Fn(&TimeSpan) -> TimeSpan) -> Self {
        self.set_span(f(&self.span))
    }

    /// Shallow object spread: existing key order is retained when overwritten,
    /// and newly introduced controls append in source order.
    pub fn set_controls(&self, controls: &OrderedMap) -> Self {
        let mut merged = self.controls.clone();
        for (key, value) in controls {
            merged.insert_key(key.clone(), value.clone());
        }
        Self {
            span: self.span,
            controls: merged,
        }
    }
}
