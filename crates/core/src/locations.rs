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

// The ranges in place take the room of the list on the heap and its tag.
const _: () = assert!(std::mem::size_of::<Locations>() == 32);

/// The source locations of a hap. See the module documentation.
#[derive(Clone)]
pub struct Locations(Repr);

#[derive(Clone)]
enum Repr {
    /// Up to 7 compact ranges, each packed as `(begin << 12) | length`
    /// (`begin < 2^20`, `length < 2^12`), covering scores up to 1 MiB.
    Packed {
        len: u8,
        spans: [u32; INLINE],
    },
    /// Up to 5 wide `(u32, u16)` ranges for offsets >= 1 MiB or spans >= 4 KiB.
    Inline {
        len: u8,
        /// Where each range begins.
        begins: [u32; WIDE_INLINE],
        /// How long each range is.
        lengths: [u16; WIDE_INLINE],
    },
    Shared(Arc<[(usize, usize)]>),
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

impl Locations {
    /// No ranges.
    pub const fn new() -> Self {
        Self(Repr::Packed {
            len: 0,
            spans: [0; INLINE],
        })
    }

    /// The one range that begins at `begin` and is `length` long, with no
    /// check of the room, which one range always has.
    pub(crate) const fn one(begin: u32, length: u16) -> Self {
        if begin < PACKED_BEGIN_LIMIT && (length as u32) <= PACKED_LEN_MASK {
            let mut spans = [0; INLINE];
            spans[0] = (begin << PACKED_LEN_BITS) | (length as u32);
            return Self(Repr::Packed { len: 1, spans });
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

    /// Whether the ranges are kept in place, owning no heap memory.
    #[cfg(test)]
    pub(crate) fn is_inline(&self) -> bool {
        matches!(self.0, Repr::Packed { .. } | Repr::Inline { .. })
    }

    /// The range at `index`, oldest first.
    #[inline]
    pub fn get(&self, index: usize) -> Option<(usize, usize)> {
        match &self.0 {
            Repr::Packed { len, spans } => {
                (index < usize::from(*len)).then(|| unpack_range(spans[index]))
            }
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
            Repr::Packed { len, spans } => {
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

    /// Add `range` after the others. Past [`INLINE`] ranges this copies the
    /// whole list, so a caller with several ranges extends once instead.
    #[inline]
    pub fn push(&mut self, range: (usize, usize)) {
        if let Repr::Packed { len, spans } = &mut self.0 {
            let idx = usize::from(*len);
            if idx < INLINE
                && let Some(packed) = pack_range(range)
            {
                spans[idx] = packed;
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
            Repr::Packed { len, spans } => {
                let start = usize::from(*len);
                if start + more.len() <= INLINE {
                    let mut packed = [0u32; INLINE];
                    let fits = packed
                        .iter_mut()
                        .zip(more)
                        .all(|(slot, range)| pack_range(*range).map(|v| *slot = v).is_some());
                    if fits {
                        spans[start..start + more.len()].copy_from_slice(&packed[..more.len()]);
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
        let mut inline = [(0, 0); INLINE];
        let all: Arc<[(usize, usize)]> = self
            .as_slice(&mut inline)
            .iter()
            .chain(more)
            .copied()
            .collect();
        self.0 = Repr::Shared(all);
    }

    /// These ranges as one slice, widened into `inline` when they are kept
    /// in place.
    fn as_slice<'a>(&'a self, inline: &'a mut [(usize, usize); INLINE]) -> &'a [(usize, usize)] {
        match &self.0 {
            Repr::Packed { len, spans } => {
                let len = usize::from(*len);
                for (at, slot) in inline[..len].iter_mut().enumerate() {
                    *slot = unpack_range(spans[at]);
                }
                &inline[..len]
            }
            Repr::Inline {
                len,
                begins,
                lengths,
            } => {
                let len = usize::from(*len);
                for (at, slot) in inline[..len].iter_mut().enumerate() {
                    *slot = wide(begins[at], lengths[at]);
                }
                &inline[..len]
            }
            Repr::Shared(ranges) => ranges,
        }
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
                spans: own_spans,
            },
            Repr::Packed { len, spans },
        ) = (&mut self.0, &other.0)
        {
            let start = usize::from(*own);
            let add = usize::from(*len);
            if start + add <= INLINE {
                own_spans[start..start + add].copy_from_slice(&spans[..add]);
                *own += *len;
                return;
            }
        }
        self.append_slow(other);
    }

    fn append_slow(&mut self, other: &Locations) {
        match &other.0 {
            Repr::Shared(ranges) => self.extend_from_slice(ranges),
            Repr::Packed { .. } => {
                let mut inline = [(0, 0); INLINE];
                self.extend_from_slice(other.as_slice(&mut inline));
            }
            Repr::Inline {
                len,
                begins,
                lengths,
            } => {
                let len = usize::from(*len);
                if let Repr::Inline {
                    len: own,
                    begins: own_begins,
                    lengths: own_lengths,
                } = &mut self.0
                {
                    let start = usize::from(*own);
                    if start + len <= WIDE_INLINE {
                        own_begins[start..start + len].copy_from_slice(&begins[..len]);
                        own_lengths[start..start + len].copy_from_slice(&lengths[..len]);
                        *own += len as u8;
                        return;
                    }
                }
                let mut inline = [(0, 0); INLINE];
                self.extend_from_slice(other.as_slice(&mut inline));
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
            let mut out = Self(Repr::Packed {
                len: len as u8,
                spans,
            });
            out.push(range);
            for rest in iter {
                out.push(rest);
            }
            return out;
        }
        Self(Repr::Packed {
            len: len as u8,
            spans,
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
    fn short_lists_stay_in_place_and_long_ones_move_to_the_heap() {
        let mut locations = Locations::new();
        for index in 0..INLINE {
            locations.push((index, index + 1));
            assert!(locations.is_inline());
        }
        locations.push((9, 10));
        assert!(matches!(locations.0, Repr::Shared(_)));
        let mut all: Vec<(usize, usize)> = (0..INLINE).map(|index| (index, index + 1)).collect();
        all.push((9, 10));
        assert_eq!(locations, all);
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
