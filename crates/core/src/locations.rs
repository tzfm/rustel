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
//! stays off the heap: up to [`INLINE`] ranges that fit in `u32` are kept in
//! place. Longer lists share one allocation.

use std::fmt;
use std::sync::Arc;

/// Ranges kept in place before the list moves to the heap.
const INLINE: usize = 3;

/// The source locations of a hap. See the module documentation.
#[derive(Clone)]
pub struct Locations(Repr);

#[derive(Clone)]
enum Repr {
    Inline {
        len: u8,
        ranges: [(u32, u32); INLINE],
    },
    Shared(Arc<[(usize, usize)]>),
}

impl Locations {
    /// No ranges.
    pub const fn new() -> Self {
        Self(Repr::Inline {
            len: 0,
            ranges: [(0, 0); INLINE],
        })
    }

    /// How many ranges there are.
    pub fn len(&self) -> usize {
        match &self.0 {
            Repr::Inline { len, .. } => usize::from(*len),
            Repr::Shared(ranges) => ranges.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether the ranges are kept in place, owning no heap memory.
    pub(crate) fn is_inline(&self) -> bool {
        matches!(self.0, Repr::Inline { .. })
    }

    /// The range at `index`, oldest first.
    pub fn get(&self, index: usize) -> Option<(usize, usize)> {
        match &self.0 {
            Repr::Inline { len, ranges } => (index < usize::from(*len))
                .then(|| (ranges[index].0 as usize, ranges[index].1 as usize)),
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

    pub fn clear(&mut self) {
        *self = Self::new();
    }

    /// Add `range` after the others. Past [`INLINE`] ranges this copies the
    /// whole list, so a caller with several ranges extends once instead.
    pub fn push(&mut self, range: (usize, usize)) {
        self.extend_from_slice(&[range]);
    }

    /// Add `more` after the others. Past [`INLINE`] ranges this copies the
    /// whole list once.
    pub fn extend_from_slice(&mut self, more: &[(usize, usize)]) {
        if more.is_empty() {
            return;
        }
        if let Repr::Inline { len, ranges } = &mut self.0 {
            let start = usize::from(*len);
            if start + more.len() <= INLINE {
                let mut narrow = [(0u32, 0u32); INLINE];
                let fits = narrow.iter_mut().zip(more).all(|(slot, &(begin, end))| {
                    match (u32::try_from(begin), u32::try_from(end)) {
                        (Ok(begin), Ok(end)) => {
                            *slot = (begin, end);
                            true
                        }
                        _ => false,
                    }
                });
                if fits {
                    ranges[start..start + more.len()].copy_from_slice(&narrow[..more.len()]);
                    *len += more.len() as u8;
                    return;
                }
            }
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
            Repr::Inline { len, ranges } => {
                let len = usize::from(*len);
                for (slot, &(begin, end)) in inline.iter_mut().zip(&ranges[..len]) {
                    *slot = (begin as usize, end as usize);
                }
                &inline[..len]
            }
            Repr::Shared(ranges) => ranges,
        }
    }

    /// Append every range of `other`, after this list's own.
    pub fn append(&mut self, other: &Locations) {
        match &other.0 {
            Repr::Shared(ranges) => self.extend_from_slice(ranges),
            Repr::Inline { len, ranges } => {
                let len = usize::from(*len);
                if len == 0 {
                    return;
                }
                if let Repr::Inline {
                    len: own,
                    ranges: own_ranges,
                } = &mut self.0
                {
                    let start = usize::from(*own);
                    if start + len <= INLINE {
                        own_ranges[start..start + len].copy_from_slice(&ranges[..len]);
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
        Self::from(iter.into_iter().collect::<Vec<_>>())
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
    fn short_lists_stay_in_place_and_long_ones_move_to_the_heap() {
        let mut locations = Locations::new();
        for index in 0..INLINE {
            locations.push((index, index + 1));
            assert!(matches!(locations.0, Repr::Inline { .. }));
        }
        locations.push((9, 10));
        assert!(matches!(locations.0, Repr::Shared(_)));
        assert_eq!(locations, vec![(0, 1), (1, 2), (2, 3), (9, 10)]);
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
        let mut spilled = Locations::from(vec![(1, 2), (3, 4), (5, 6), (7, 8)]);
        spilled.append(&Locations::new());
        let shared = Locations::from(vec![(1, 2), (3, 4), (5, 6), (7, 8)]);
        assert_eq!(spilled, shared);
        assert_eq!(
            left.iter().rev().collect::<Vec<_>>(),
            vec![(5, 6), (3, 4), (1, 2)]
        );
        assert_eq!(format!("{left:?}"), "[(1, 2), (3, 4), (5, 6)]");
    }
}
