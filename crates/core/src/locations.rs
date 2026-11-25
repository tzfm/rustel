/*
locations.rs - the source locations a hap carries
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! The source locations of a hap: `[begin, end)` byte ranges of the score,
//! oldest first. They drive the editor's highlighting.
//!
//! Every query copies them into each hap it returns, so the common case
//! stays off the heap: up to [`INLINE`] ranges are kept in place, each as a
//! begin that fits in `u32` and a length that fits in `u16`, which a word
//! of a score is. Longer lists, and a list with any other range, share one
//! allocation.

use std::fmt;
use std::sync::Arc;

/// Compact ranges kept in place before the list moves to the heap.
const INLINE: usize = 7;
/// Wide `(u32, u16)` ranges kept in place when a range exceeds the 20/12-bit
/// packed layout.
const WIDE_INLINE: usize = 5;

const PACKED_LEN_BITS: u32 = 12;
const PACKED_LEN_MASK: u32 = (1 << PACKED_LEN_BITS) - 1;
const PACKED_BEGIN_LIMIT: u32 = 1 << (32 - PACKED_LEN_BITS);

#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<Locations>() == 32);

/// The source locations of a hap. See the module documentation.
#[derive(Clone)]
pub struct Locations(Repr);

#[derive(Clone)]
enum Repr {
    /// Up to 7 compact ranges, each packed as `(begin << 12) | length`
    /// (`begin < 2^20`, `length < 2^12`), covering scores up to 1 MiB.
    /// Split into `first` plus three 8-byte-aligned `u64` pairs so copying
    /// into `Hap` stays in registers without an overlapping stack bounce.
    Packed {
        len: u8,
        first: u32,
        tail: [u64; 3],
    },
    /// Up to 5 wide `(u32, u16)` ranges for offsets >= 1 MiB or spans >= 4 KiB.
    Inline {
        len: u8,
        /// How long each range is.
        lengths: [u16; WIDE_INLINE],
        /// Where each range begins.
        begins: [u32; WIDE_INLINE],
    },
    Shared(Arc<[(usize, usize)]>),
}

#[inline(always)]
const fn pack_spans(spans: [u32; INLINE]) -> (u32, [u64; 3]) {
    (
        spans[0],
        [
            (spans[1] as u64) | ((spans[2] as u64) << 32),
            (spans[3] as u64) | ((spans[4] as u64) << 32),
            (spans[5] as u64) | ((spans[6] as u64) << 32),
        ],
    )
}

#[inline(always)]
const fn unpack_spans(first: u32, tail: [u64; 3]) -> [u32; INLINE] {
    [
        first,
        tail[0] as u32,
        (tail[0] >> 32) as u32,
        tail[1] as u32,
        (tail[1] >> 32) as u32,
        tail[2] as u32,
        (tail[2] >> 32) as u32,
    ]
}

#[inline(always)]
const fn get_packed_span(first: u32, tail: [u64; 3], idx: usize) -> u32 {
    match idx {
        0 => first,
        1 => tail[0] as u32,
        2 => (tail[0] >> 32) as u32,
        3 => tail[1] as u32,
        4 => (tail[1] >> 32) as u32,
        5 => tail[2] as u32,
        _ => (tail[2] >> 32) as u32,
    }
}

#[inline(always)]
fn set_packed_span(first: &mut u32, tail: &mut [u64; 3], idx: usize, v: u32) {
    match idx {
        0 => *first = v,
        1 => tail[0] = (tail[0] & !0xffff_ffff) | u64::from(v),
        2 => tail[0] = (tail[0] & 0xffff_ffff) | (u64::from(v) << 32),
        3 => tail[1] = (tail[1] & !0xffff_ffff) | u64::from(v),
        4 => tail[1] = (tail[1] & 0xffff_ffff) | (u64::from(v) << 32),
        5 => tail[2] = (tail[2] & !0xffff_ffff) | u64::from(v),
        _ => tail[2] = (tail[2] & 0xffff_ffff) | (u64::from(v) << 32),
    }
}

/// Pack `(begin, end)` into 32 bits when `begin < 2^20` and `length < 2^12`.
#[inline]
fn pack_range((begin, end): (usize, usize)) -> Option<u32> {
    let length = end.checked_sub(begin)?;
    let begin = u32::try_from(begin).ok()?;
    let length = u32::try_from(length).ok()?;
    (begin < PACKED_BEGIN_LIMIT && length <= PACKED_LEN_MASK)
        .then_some((begin << PACKED_LEN_BITS) | length)
}

#[inline]
fn unpack_range(packed: u32) -> (usize, usize) {
    let begin = (packed >> PACKED_LEN_BITS) as usize;
    let length = (packed & PACKED_LEN_MASK) as usize;
    (begin, begin + length)
}

/// `range` as a wide list in place holds it: its begin and its length. `None`
/// for a range that takes more room.
fn narrow((begin, end): (usize, usize)) -> Option<(u32, u16)> {
    let length = end.checked_sub(begin)?;
    Some((u32::try_from(begin).ok()?, u16::try_from(length).ok()?))
}

/// The range that begins at `begin` and is `length` long.
fn wide(begin: u32, length: u16) -> (usize, usize) {
    let begin = begin as usize;
    (begin, begin + usize::from(length))
}

/// Copyable snapshot of a `Locations::Packed` payload for branch-free
/// emission in hot sequence loops.
#[derive(Clone, Copy)]
pub(crate) struct PackedLocations {
    len: u8,
    first: u32,
    tail: [u64; 3],
}

impl PackedLocations {
    #[inline(always)]
    pub(crate) const fn parts(self) -> (u8, u32, u64, u64, u64) {
        (
            self.len,
            self.first,
            self.tail[0],
            self.tail[1],
            self.tail[2],
        )
    }
}

impl Locations {
    /// No ranges.
    pub const fn new() -> Self {
        Self(Repr::Packed {
            len: 0,
            first: 0,
            tail: [0; 3],
        })
    }

    #[inline(always)]
    pub(crate) const fn as_packed_copy(&self) -> Option<PackedLocations> {
        match self.0 {
            Repr::Packed { len, first, tail } => Some(PackedLocations { len, first, tail }),
            _ => None,
        }
    }

    #[inline(always)]
    pub(crate) const fn from_packed_copy(packed: PackedLocations) -> Self {
        Self(Repr::Packed {
            len: packed.len,
            first: packed.first,
            tail: packed.tail,
        })
    }

    #[inline(always)]
    pub(crate) const fn pack_small(begin: u32, length: u32) -> Option<u32> {
        if begin < PACKED_BEGIN_LIMIT && length <= PACKED_LEN_MASK {
            Some((begin << PACKED_LEN_BITS) | length)
        } else {
            None
        }
    }

    #[inline(always)]
    pub(crate) const fn from_packed_parts(len: u8, first: u32, t0: u64, t1: u64, t2: u64) -> Self {
        Self(Repr::Packed {
            len,
            first,
            tail: [t0, t1, t2],
        })
    }

    /// The one range that begins at `begin` and is `length` long, with no
    /// check of the room, which one range always has.
    pub(crate) const fn one(begin: u32, length: u16) -> Self {
        if begin < PACKED_BEGIN_LIMIT && (length as u32) <= PACKED_LEN_MASK {
            let first = (begin << PACKED_LEN_BITS) | (length as u32);
            return Self(Repr::Packed {
                len: 1,
                first,
                tail: [0; 3],
            });
        }
        let mut begins = [0; WIDE_INLINE];
        let mut lengths = [0; WIDE_INLINE];
        begins[0] = begin;
        lengths[0] = length;
        Self(Repr::Inline {
            len: 1,
            begins,
            lengths,
        })
    }

    /// How many ranges there are.
    #[inline]
    pub fn len(&self) -> usize {
        match &self.0 {
            Repr::Packed { len, .. } | Repr::Inline { len, .. } => usize::from(*len),
            Repr::Shared(ranges) => ranges.len(),
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Prepend one or two `Dress`-packed `(begin << 12) | (len + 1)` ranges
    /// directly into a `Packed` list in place. Returns `false` if this list is
    /// not `Packed` or does not have room.
    #[inline]
    pub(crate) fn prepend_packed(&mut self, p0: u32, p1: u32) -> bool {
        let Repr::Packed { len, first, tail } = &mut self.0 else {
            return false;
        };
        let cur = usize::from(*len);
        if cur <= 1 {
            let s0 = p0 - 1;
            if p1 == 0 {
                if cur == 0 {
                    *first = s0;
                    *len = 1;
                } else {
                    tail[0] = u64::from(*first);
                    *first = s0;
                    *len = 2;
                }
            } else {
                let s1 = p1 - 1;
                if cur == 0 {
                    *first = s0;
                    tail[0] = u64::from(s1);
                    *len = 2;
                } else {
                    tail[0] = u64::from(s1) | (u64::from(*first) << 32);
                    *first = s0;
                    *len = 3;
                }
            }
            return true;
        }
        let prefix_len = 1 + usize::from(p1 != 0);
        if cur + prefix_len > INLINE {
            return false;
        }
        let mut spans = unpack_spans(*first, *tail);
        spans.copy_within(0..cur, prefix_len);
        spans[0] = p0 - 1;
        if p1 != 0 {
            spans[1] = p1 - 1;
        }
        let (f, t) = pack_spans(spans);
        *first = f;
        *tail = t;
        *len = (cur + prefix_len) as u8;
        true
    }

    /// Prepend `prefix` before `self` in place when both fit in `Packed`.
    #[inline]
    pub(crate) fn prepend(&mut self, prefix: &Self) {
        if prefix.is_empty() {
            return;
        }
        if let (
            Repr::Packed {
                len: cur_len,
                first: cur_first,
                tail: cur_tail,
            },
            Repr::Packed {
                len: pre_len,
                first: pre_first,
                tail: pre_tail,
            },
        ) = (&mut self.0, &prefix.0)
        {
            let cur = usize::from(*cur_len);
            let pre = usize::from(*pre_len);
            if cur + pre <= INLINE {
                let mut cur_spans = unpack_spans(*cur_first, *cur_tail);
                let pre_spans = unpack_spans(*pre_first, *pre_tail);
                if cur > 0 {
                    cur_spans.copy_within(0..cur, pre);
                }
                cur_spans[..pre].copy_from_slice(&pre_spans[..pre]);
                let (f, t) = pack_spans(cur_spans);
                *cur_first = f;
                *cur_tail = t;
                *cur_len = (cur + pre) as u8;
                return;
            }
        }
        let mut out = prefix.clone();
        out.append(self);
        *self = out;
    }

    /// Whether the ranges are kept in place, owning no heap memory.
    #[cfg(test)]
    pub(crate) fn is_inline(&self) -> bool {
        matches!(self.0, Repr::Packed { .. } | Repr::Inline { .. })
    }

    /// Fast `(begin, length)` extraction when stored as a single compact packed range.
    #[inline]
    pub(crate) fn single_packed_raw(&self) -> Option<(u32, u32)> {
        match &self.0 {
            Repr::Packed { len: 1, first, .. } => {
                let p = *first;
                Some((p >> PACKED_LEN_BITS, p & PACKED_LEN_MASK))
            }
            _ => None,
        }
    }

    /// The range at `index`, oldest first.
    #[inline]
    pub fn get(&self, index: usize) -> Option<(usize, usize)> {
        match &self.0 {
            Repr::Packed { len, first, tail } => (index < usize::from(*len))
                .then(|| unpack_range(get_packed_span(*first, *tail, index))),
            Repr::Inline {
                len,
                begins,
                lengths,
            } => (index < usize::from(*len)).then(|| wide(begins[index], lengths[index])),
            Repr::Shared(ranges) => ranges.get(index).copied(),
        }
    }

    /// The oldest range.
    pub fn first(&self) -> Option<(usize, usize)> {
        self.get(0)
    }

    pub fn iter(&self) -> Iter<'_> {
        Iter {
            locations: self,
            front: 0,
            back: self.len(),
        }
    }

    pub fn to_vec(&self) -> Vec<(usize, usize)> {
        self.iter().collect()
    }

    /// Share or materialize these locations into a single `Arc<[(usize, usize)]>`.
    pub(crate) fn to_arc_slice(&self) -> Arc<[(usize, usize)]> {
        match &self.0 {
            Repr::Shared(shared) => Arc::clone(shared),
            _ => self.iter().collect(),
        }
    }

    /// Pass these locations as a contiguous slice without heap allocation when
    /// stored inline or already shared.
    pub(crate) fn with_slice<R>(&self, f: impl FnOnce(&[(usize, usize)]) -> R) -> R {
        match &self.0 {
            Repr::Packed { len, first, tail } => {
                let spans = unpack_spans(*first, *tail);
                let mut buf = [(0usize, 0usize); INLINE];
                let count = usize::from(*len);
                for (slot, &packed) in buf[..count].iter_mut().zip(&spans[..count]) {
                    *slot = unpack_range(packed);
                }
                f(&buf[..count])
            }
            Repr::Inline {
                len,
                begins,
                lengths,
            } => {
                let mut buf = [(0usize, 0usize); WIDE_INLINE];
                let count = usize::from(*len);
                for ((slot, &begin), &length) in buf[..count]
                    .iter_mut()
                    .zip(&begins[..count])
                    .zip(&lengths[..count])
                {
                    *slot = wide(begin, length);
                }
                f(&buf[..count])
            }
            Repr::Shared(shared) => f(shared),
        }
    }

    pub fn clear(&mut self) {
        *self = Self::new();
    }

    /// Add a `(begin, begin + length)` range directly into the packed buffer
    /// when it fits, avoiding `usize` round-trips for grid run decoding.
    #[inline]
    pub(crate) fn push_narrow(&mut self, begin: u32, length: u32) {
        if let Repr::Packed { len, first, tail } = &mut self.0 {
            let idx = usize::from(*len);
            if idx < INLINE && begin < PACKED_BEGIN_LIMIT && length <= PACKED_LEN_MASK {
                set_packed_span(first, tail, idx, (begin << PACKED_LEN_BITS) | length);
                *len += 1;
                return;
            }
        }
        let start = begin as usize;
        self.extend_from_slice(&[(start, start + length as usize)]);
    }

    /// Add `range` after the others. Past [`INLINE`] ranges this copies the
    /// whole list, so a caller with several ranges extends once instead.
    #[inline]
    pub fn push(&mut self, range: (usize, usize)) {
        if let Repr::Packed { len, first, tail } = &mut self.0 {
            let idx = usize::from(*len);
            if idx < INLINE
                && let Some(packed) = pack_range(range)
            {
                set_packed_span(first, tail, idx, packed);
                *len += 1;
                return;
            }
        }
        self.extend_from_slice(&[range]);
    }

    /// Add `more` after the others. Past [`INLINE`] ranges this copies the
    /// whole list once.
    pub fn extend_from_slice(&mut self, more: &[(usize, usize)]) {
        if more.is_empty() {
            return;
        }
        match &mut self.0 {
            Repr::Packed { len, first, tail } => {
                let start = usize::from(*len);
                let mut spans = unpack_spans(*first, *tail);
                if start + more.len() <= INLINE {
                    let mut packed = [0u32; INLINE];
                    let fits = packed
                        .iter_mut()
                        .zip(more)
                        .all(|(slot, range)| pack_range(*range).map(|v| *slot = v).is_some());
                    if fits {
                        spans[start..start + more.len()].copy_from_slice(&packed[..more.len()]);
                        let (f, t) = pack_spans(spans);
                        *first = f;
                        *tail = t;
                        *len += more.len() as u8;
                        return;
                    }
                }
                if start + more.len() <= WIDE_INLINE {
                    let mut begins = [0u32; WIDE_INLINE];
                    let mut lengths = [0u16; WIDE_INLINE];
                    for (at, &p) in spans[..start].iter().enumerate() {
                        begins[at] = p >> PACKED_LEN_BITS;
                        lengths[at] = (p & PACKED_LEN_MASK) as u16;
                    }
                    let fits = more.iter().enumerate().all(|(at, range)| {
                        narrow(*range)
                            .map(|(b, l)| {
                                begins[start + at] = b;
                                lengths[start + at] = l;
                            })
                            .is_some()
                    });
                    if fits {
                        self.0 = Repr::Inline {
                            len: (start + more.len()) as u8,
                            begins,
                            lengths,
                        };
                        return;
                    }
                }
            }
            Repr::Inline {
                len,
                begins,
                lengths,
            } => {
                let start = usize::from(*len);
                if start + more.len() <= WIDE_INLINE {
                    let mut narrowed = [(0u32, 0u16); WIDE_INLINE];
                    let fits = narrowed
                        .iter_mut()
                        .zip(more)
                        .all(|(slot, range)| narrow(*range).map(|range| *slot = range).is_some());
                    if fits {
                        for (at, (begin, length)) in narrowed[..more.len()].iter().enumerate() {
                            begins[start + at] = *begin;
                            lengths[start + at] = *length;
                        }
                        *len += more.len() as u8;
                        return;
                    }
                }
            }
            Repr::Shared(_) => {}
        }
        let all: Arc<[(usize, usize)]> = self.iter().chain(more.iter().copied()).collect();
        self.0 = Repr::Shared(all);
    }

    /// Append every range of `other`, after this list's own.
    #[inline]
    pub fn append(&mut self, other: &Locations) {
        if other.is_empty() {
            return;
        }
        if self.is_empty() {
            *self = other.clone();
            return;
        }
        if let (
            Repr::Packed {
                len: own,
                first: own_first,
                tail: own_tail,
            },
            Repr::Packed { len, first, tail },
        ) = (&mut self.0, &other.0)
        {
            let start = usize::from(*own);
            let add = usize::from(*len);
            if start + add <= INLINE {
                let mut own_spans = unpack_spans(*own_first, *own_tail);
                let spans = unpack_spans(*first, *tail);
                own_spans[start..start + add].copy_from_slice(&spans[..add]);
                let (f, t) = pack_spans(own_spans);
                *own_first = f;
                *own_tail = t;
                *own += *len;
                return;
            }
        }
        self.append_slow(other);
    }

    fn append_slow(&mut self, other: &Locations) {
        match &other.0 {
            Repr::Shared(ranges) => self.extend_from_slice(ranges),
            Repr::Packed { len, first, tail } => {
                let spans = unpack_spans(*first, *tail);
                let mut buf = [(0, 0); INLINE];
                let count = usize::from(*len);
                for (slot, &p) in buf[..count].iter_mut().zip(&spans[..count]) {
                    *slot = unpack_range(p);
                }
                self.extend_from_slice(&buf[..count]);
            }
            Repr::Inline {
                len,
                begins,
                lengths,
            } => {
                let count = usize::from(*len);
                if let Repr::Inline {
                    len: own,
                    begins: own_begins,
                    lengths: own_lengths,
                } = &mut self.0
                {
                    let start = usize::from(*own);
                    if start + count <= WIDE_INLINE {
                        own_begins[start..start + count].copy_from_slice(&begins[..count]);
                        own_lengths[start..start + count].copy_from_slice(&lengths[..count]);
                        *own += count as u8;
                        return;
                    }
                }
                let mut buf = [(0, 0); WIDE_INLINE];
                for ((slot, &b), &l) in buf[..count]
                    .iter_mut()
                    .zip(&begins[..count])
                    .zip(&lengths[..count])
                {
                    *slot = wide(b, l);
                }
                self.extend_from_slice(&buf[..count]);
            }
        }
    }
}

impl Default for Locations {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Vec<(usize, usize)>> for Locations {
    fn from(ranges: Vec<(usize, usize)>) -> Self {
        let mut locations = Self::new();
        locations.extend_from_slice(&ranges);
        locations
    }
}

impl From<&[(usize, usize)]> for Locations {
    fn from(ranges: &[(usize, usize)]) -> Self {
        let mut locations = Self::new();
        locations.extend_from_slice(ranges);
        locations
    }
}

impl FromIterator<(usize, usize)> for Locations {
    fn from_iter<I: IntoIterator<Item = (usize, usize)>>(iter: I) -> Self {
        let mut iter = iter.into_iter();
        let mut spans = [0_u32; INLINE];
        let mut len = 0_usize;
        while let Some(range) = iter.next() {
            if len < INLINE
                && let Some(packed) = pack_range(range)
            {
                spans[len] = packed;
                len += 1;
                continue;
            }
            let (first, tail) = pack_spans(spans);
            let mut out = Self(Repr::Packed {
                len: len as u8,
                first,
                tail,
            });
            out.push(range);
            for rest in iter {
                out.push(rest);
            }
            return out;
        }
        let (first, tail) = pack_spans(spans);
        Self(Repr::Packed {
            len: len as u8,
            first,
            tail,
        })
    }
}

impl PartialEq for Locations {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

impl Eq for Locations {}

impl PartialEq<Vec<(usize, usize)>> for Locations {
    fn eq(&self, other: &Vec<(usize, usize)>) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter().copied())
    }
}

impl std::hash::Hash for Locations {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.len().hash(state);
        for range in self.iter() {
            range.hash(state);
        }
    }
}

impl fmt::Debug for Locations {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<'a> IntoIterator for &'a Locations {
    type Item = (usize, usize);
    type IntoIter = Iter<'a>;

    fn into_iter(self) -> Iter<'a> {
        self.iter()
    }
}

/// The ranges of a [`Locations`], oldest first.
#[derive(Clone, Debug)]
pub struct Iter<'a> {
    locations: &'a Locations,
    front: usize,
    back: usize,
}

impl Iterator for Iter<'_> {
    type Item = (usize, usize);

    fn next(&mut self) -> Option<(usize, usize)> {
        if self.front == self.back {
            return None;
        }
        let range = self.locations.get(self.front);
        self.front += 1;
        range
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = self.back - self.front;
        (left, Some(left))
    }
}

impl DoubleEndedIterator for Iter<'_> {
    fn next_back(&mut self) -> Option<(usize, usize)> {
        if self.front == self.back {
            return None;
        }
        self.back -= 1;
        self.locations.get(self.back)
    }
}

impl ExactSizeIterator for Iter<'_> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_range_is_the_list_that_a_push_makes() {
        for (begin, length) in [(0u32, 0u16), (7, 3), (u32::MAX, u16::MAX)] {
            let one = Locations::one(begin, length);
            let range = (begin as usize, begin as usize + usize::from(length));
            assert_eq!(one, Locations::from(&[range][..]));
            assert!(one.is_inline());
            assert_eq!(one.get(0), Some(range));
            assert_eq!(one.len(), 1);
        }
    }

    #[test]
    fn a_range_longer_than_a_word_keeps_its_exact_value() {
        let long = usize::from(u16::MAX) + 1;
        for range in [(5, 5 + long), (5, 5 + long - 1), (7, 3)] {
            let mut locations = Locations::from(vec![(1, 2)]);
            locations.push(range);
            assert_eq!(locations.to_vec(), vec![(1, 2), range]);
            assert_eq!(locations.is_inline(), range == (5, 5 + long - 1));
        }
    }

    #[test]
    fn a_range_past_u32_keeps_its_exact_value() {
        let far = usize::try_from(u64::from(u32::MAX) + 7).expect("a 64-bit target");
        let locations = Locations::from(vec![(far, far + 1)]);
        assert_eq!(locations.to_vec(), vec![(far, far + 1)]);
    }

    #[test]
    fn append_keeps_the_order_and_equality_ignores_the_storage() {
        let mut left = Locations::from(vec![(1, 2)]);
        left.append(&Locations::from(vec![(3, 4), (5, 6)]));
        assert_eq!(left, vec![(1, 2), (3, 4), (5, 6)]);
        let many: Vec<(usize, usize)> = (0..=INLINE).map(|index| (index, index + 1)).collect();
        let mut spilled = Locations::from(many.clone());
        spilled.append(&Locations::new());
        assert!(!spilled.is_inline());
        assert_eq!(spilled, Locations::from(many.clone()));
        // Lists in place that are too long together move to the heap.
        let mut joined = Locations::from(many[..INLINE].to_vec());
        joined.append(&Locations::from(many[INLINE..].to_vec()));
        assert_eq!(joined, many);
        assert_eq!(
            left.iter().rev().collect::<Vec<_>>(),
            vec![(5, 6), (3, 4), (1, 2)]
        );
        assert_eq!(format!("{left:?}"), "[(1, 2), (3, 4), (5, 6)]");
    }
}
