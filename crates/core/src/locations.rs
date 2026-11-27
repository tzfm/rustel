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
//! stays off the heap in 16 bytes: up to [`INLINE`] (4) compact ranges are kept
//! in place as `(begin << 11) | (length + 1)` (`begin < 2^20`, `length <= 2046`),
//! covering scores up to 1 MiB. Every non-zero packed word has its top bit clear,
//! so the final slot's high byte supplies a niche for the inline-vs-shared tag
//! in 100% safe Rust. Longer lists (5..=7 compact ranges) spill into a single
//! 48-byte `Arc<SharedLocs::Packed>`; only 8+ or wide ranges use a boxed slice.

use std::fmt;
use std::sync::Arc;

/// Compact ranges kept in place before the list moves to the heap.
const INLINE: usize = 4;
/// Compact ranges kept in a single-allocation `SharedLocs::Packed`.
const SHARED_PACKED: usize = 7;

const PACKED_LEN_BITS: u32 = 11;
const PACKED_LEN_MASK: u32 = (1 << PACKED_LEN_BITS) - 1;
const PACKED_MAX_LEN: u32 = PACKED_LEN_MASK - 1;
const PACKED_BEGIN_LIMIT: u32 = 1 << (31 - PACKED_LEN_BITS);

macro_rules! define_hi7 {
    ($($name:ident = $val:literal),* $(,)?) => {
        #[derive(Clone, Copy, PartialEq, Eq)]
        #[repr(u8)]
        enum Hi7 {
            $($name = $val,)*
        }

        impl Hi7 {
            #[inline(always)]
            const fn from_u8(b: u8) -> Self {
                match b {
                    $($val => Self::$name,)*
                    _ => panic!("Hi7 value out of 7-bit range"),
                }
            }

            #[inline(always)]
            const fn as_u8(self) -> u8 {
                self as u8
            }
        }
    };
}

define_hi7!(
    V0 = 0,
    V1 = 1,
    V2 = 2,
    V3 = 3,
    V4 = 4,
    V5 = 5,
    V6 = 6,
    V7 = 7,
    V8 = 8,
    V9 = 9,
    V10 = 10,
    V11 = 11,
    V12 = 12,
    V13 = 13,
    V14 = 14,
    V15 = 15,
    V16 = 16,
    V17 = 17,
    V18 = 18,
    V19 = 19,
    V20 = 20,
    V21 = 21,
    V22 = 22,
    V23 = 23,
    V24 = 24,
    V25 = 25,
    V26 = 26,
    V27 = 27,
    V28 = 28,
    V29 = 29,
    V30 = 30,
    V31 = 31,
    V32 = 32,
    V33 = 33,
    V34 = 34,
    V35 = 35,
    V36 = 36,
    V37 = 37,
    V38 = 38,
    V39 = 39,
    V40 = 40,
    V41 = 41,
    V42 = 42,
    V43 = 43,
    V44 = 44,
    V45 = 45,
    V46 = 46,
    V47 = 47,
    V48 = 48,
    V49 = 49,
    V50 = 50,
    V51 = 51,
    V52 = 52,
    V53 = 53,
    V54 = 54,
    V55 = 55,
    V56 = 56,
    V57 = 57,
    V58 = 58,
    V59 = 59,
    V60 = 60,
    V61 = 61,
    V62 = 62,
    V63 = 63,
    V64 = 64,
    V65 = 65,
    V66 = 66,
    V67 = 67,
    V68 = 68,
    V69 = 69,
    V70 = 70,
    V71 = 71,
    V72 = 72,
    V73 = 73,
    V74 = 74,
    V75 = 75,
    V76 = 76,
    V77 = 77,
    V78 = 78,
    V79 = 79,
    V80 = 80,
    V81 = 81,
    V82 = 82,
    V83 = 83,
    V84 = 84,
    V85 = 85,
    V86 = 86,
    V87 = 87,
    V88 = 88,
    V89 = 89,
    V90 = 90,
    V91 = 91,
    V92 = 92,
    V93 = 93,
    V94 = 94,
    V95 = 95,
    V96 = 96,
    V97 = 97,
    V98 = 98,
    V99 = 99,
    V100 = 100,
    V101 = 101,
    V102 = 102,
    V103 = 103,
    V104 = 104,
    V105 = 105,
    V106 = 106,
    V107 = 107,
    V108 = 108,
    V109 = 109,
    V110 = 110,
    V111 = 111,
    V112 = 112,
    V113 = 113,
    V114 = 114,
    V115 = 115,
    V116 = 116,
    V117 = 117,
    V118 = 118,
    V119 = 119,
    V120 = 120,
    V121 = 121,
    V122 = 122,
    V123 = 123,
    V124 = 124,
    V125 = 125,
    V126 = 126,
    V127 = 127,
);

#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<Locations>() == 16);

/// The source locations of a hap. See the module documentation.
#[derive(Clone)]
pub struct Locations(Repr);

enum SharedLocs {
    /// 5..=7 compact ranges in a single 32-byte `Arc` payload (1 allocation).
    Packed {
        len: u8,
        spans: [u32; SHARED_PACKED],
    },
    /// 8+ ranges or any range exceeding 20-bit begin / 2046-byte length.
    Wide(Box<[(usize, usize)]>),
}

impl SharedLocs {
    fn from_ranges(ranges: &[(usize, usize)]) -> Arc<Self> {
        if ranges.len() <= SHARED_PACKED {
            let mut spans = [0u32; SHARED_PACKED];
            let mut fits = true;
            for (slot, &r) in spans[..ranges.len()].iter_mut().zip(ranges) {
                if let Some(p) = pack_range(r) {
                    *slot = p;
                } else {
                    fits = false;
                    break;
                }
            }
            if fits {
                return Arc::new(Self::Packed {
                    len: ranges.len() as u8,
                    spans,
                });
            }
        }
        Arc::new(Self::Wide(Box::from(ranges)))
    }

    #[inline]
    fn len(&self) -> usize {
        match self {
            Self::Packed { len, .. } => usize::from(*len),
            Self::Wide(ranges) => ranges.len(),
        }
    }

    #[inline]
    fn get(&self, index: usize) -> Option<(usize, usize)> {
        match self {
            Self::Packed { len, spans } => {
                (index < usize::from(*len)).then(|| unpack_range(spans[index]))
            }
            Self::Wide(ranges) => ranges.get(index).copied(),
        }
    }
}

#[derive(Clone)]
enum Repr {
    /// Up to 4 compact non-zero ranges (zero-terminated), each packed as
    /// `(begin << 11) | (length + 1)` (`begin < 2^20`, `length <= 2046`).
    /// `s3` is split so `s3_hi: Hi7` provides the enum discriminant niche.
    Packed(PackedLocations),
    /// 5+ ranges, or any range exceeding 20-bit begin / 2046-byte length.
    /// Thin `Arc` pointer keeps `Locations` at 16 bytes.
    Shared(Arc<SharedLocs>),
}

/// Already-split copyable snapshot of `Repr::Packed` so hot loops copy 16 bytes
/// directly into `Locations` with zero bit-unpacking or discriminant checks.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(C, align(8))]
pub(crate) struct PackedLocations {
    s0: u32,
    s1: u32,
    s2: u32,
    s3_lo: [u8; 3],
    s3_hi: Hi7,
}

impl PackedLocations {
    pub(crate) const EMPTY: Self = Self {
        s0: 0,
        s1: 0,
        s2: 0,
        s3_lo: [0; 3],
        s3_hi: Hi7::V0,
    };

    #[inline(always)]
    const fn to_words(self) -> [u32; INLINE] {
        [self.s0, self.s1, self.s2, join_s3(self.s3_lo, self.s3_hi)]
    }

    #[inline(always)]
    pub(crate) const fn from_words(spans: [u32; INLINE]) -> Self {
        let (s3_lo, s3_hi) = split_s3(spans[3]);
        Self {
            s0: spans[0],
            s1: spans[1],
            s2: spans[2],
            s3_lo,
            s3_hi,
        }
    }
}

#[inline(always)]
const fn split_s3(s3: u32) -> ([u8; 3], Hi7) {
    let b = s3.to_le_bytes();
    ([b[0], b[1], b[2]], Hi7::from_u8(b[3]))
}

#[inline(always)]
const fn join_s3(lo: [u8; 3], hi: Hi7) -> u32 {
    u32::from_le_bytes([lo[0], lo[1], lo[2], hi.as_u8()])
}

/// Pack `(begin, end)` into a non-zero 31-bit `u32` when `begin < 2^20` and `length <= 2046`.
#[inline(always)]
fn pack_range((begin, end): (usize, usize)) -> Option<u32> {
    let length = end.checked_sub(begin)?;
    let begin = u32::try_from(begin).ok()?;
    let length = u32::try_from(length).ok()?;
    (begin < PACKED_BEGIN_LIMIT && length <= PACKED_MAX_LEN)
        .then_some((begin << PACKED_LEN_BITS) | (length + 1))
}

#[inline(always)]
fn unpack_range(packed: u32) -> (usize, usize) {
    let begin = (packed >> PACKED_LEN_BITS) as usize;
    let length = ((packed & PACKED_LEN_MASK) - 1) as usize;
    (begin, begin + length)
}

impl Locations {
    /// No ranges.
    pub const fn new() -> Self {
        Self(Repr::Packed(PackedLocations::EMPTY))
    }

    #[inline(always)]
    pub(crate) const fn as_packed_copy(&self) -> Option<PackedLocations> {
        match self.0 {
            Repr::Packed(p) => Some(p),
            Repr::Shared(_) => None,
        }
    }

    #[inline(always)]
    pub(crate) const fn from_packed_copy(packed: PackedLocations) -> Self {
        Self(Repr::Packed(packed))
    }

    /// Pack `(begin, length)` into the canonical non-zero 31-bit format.
    #[inline(always)]
    pub(crate) const fn pack_small(begin: u32, length: u32) -> Option<u32> {
        if begin < PACKED_BEGIN_LIMIT && length <= PACKED_MAX_LEN {
            Some((begin << PACKED_LEN_BITS) | (length + 1))
        } else {
            None
        }
    }

    /// Pack `(begin, end)` into the canonical non-zero 31-bit format.
    #[inline(always)]
    pub(crate) fn pack_range_raw(range: (usize, usize)) -> Option<u32> {
        pack_range(range)
    }

    /// Unpack a non-zero word created by [`Self::pack_range_raw`] or [`Self::pack_small`].
    #[inline(always)]
    pub(crate) fn unpack_range_raw(packed: u32) -> (usize, usize) {
        unpack_range(packed)
    }

    #[inline(always)]
    pub(crate) const fn from_packed(spans: [u32; INLINE]) -> Self {
        Self(Repr::Packed(PackedLocations::from_words(spans)))
    }

    /// The one range that begins at `begin` and is `length` long.
    pub(crate) fn one(begin: u32, length: u16) -> Self {
        if let Some(first) = Self::pack_small(begin, u32::from(length)) {
            return Self(Repr::Packed(PackedLocations {
                s0: first,
                s1: 0,
                s2: 0,
                s3_lo: [0; 3],
                s3_hi: Hi7::V0,
            }));
        }
        let start = begin as usize;
        Self(Repr::Shared(SharedLocs::from_ranges(&[(
            start,
            start + usize::from(length),
        )])))
    }

    /// How many ranges there are.
    #[inline]
    pub fn len(&self) -> usize {
        match &self.0 {
            Repr::Packed(p) => {
                if p.s0 == 0 {
                    0
                } else if p.s1 == 0 {
                    1
                } else if p.s2 == 0 {
                    2
                } else if join_s3(p.s3_lo, p.s3_hi) == 0 {
                    3
                } else {
                    4
                }
            }
            Repr::Shared(shared) => shared.len(),
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        match &self.0 {
            Repr::Packed(p) => p.s0 == 0,
            Repr::Shared(shared) => shared.len() == 0,
        }
    }

    /// Prepend one or two canonical packed ranges (`0` = absent) directly into
    /// a `Packed` list in place. Returns `false` if not `Packed` or full.
    #[inline]
    pub(crate) fn prepend_packed(&mut self, p0: u32, p1: u32) -> bool {
        if p0 == 0 {
            return true;
        }
        let Repr::Packed(p) = &mut self.0 else {
            return false;
        };
        if p1 == 0 {
            if join_s3(p.s3_lo, p.s3_hi) != 0 {
                return false;
            }
            let (lo, hi) = split_s3(p.s2);
            p.s3_lo = lo;
            p.s3_hi = hi;
            p.s2 = p.s1;
            p.s1 = p.s0;
            p.s0 = p0;
        } else {
            if p.s2 != 0 {
                return false;
            }
            let (lo, hi) = split_s3(p.s1);
            p.s3_lo = lo;
            p.s3_hi = hi;
            p.s2 = p.s0;
            p.s1 = p1;
            p.s0 = p0;
        }
        true
    }

    /// Prepend `prefix` before `self` in place when both fit in `Packed`.
    #[inline]
    pub(crate) fn prepend(&mut self, prefix: &Self) {
        if prefix.is_empty() {
            return;
        }
        if let (Some(cur), Some(pre)) = (self.as_packed_copy(), prefix.as_packed_copy()) {
            let cur_len = self.len();
            let pre_len = prefix.len();
            if cur_len + pre_len <= INLINE {
                let cw = cur.to_words();
                let pw = pre.to_words();
                let mut merged = [0u32; INLINE];
                merged[..pre_len].copy_from_slice(&pw[..pre_len]);
                merged[pre_len..pre_len + cur_len].copy_from_slice(&cw[..cur_len]);
                self.0 = Repr::Packed(PackedLocations::from_words(merged));
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
        matches!(self.0, Repr::Packed(_))
    }

    /// Fast `(begin, length)` extraction when stored as a single compact packed range.
    #[inline]
    pub(crate) fn single_packed_raw(&self) -> Option<(u32, u32)> {
        match &self.0 {
            Repr::Packed(p) if p.s0 != 0 && p.s1 == 0 => {
                Some((p.s0 >> PACKED_LEN_BITS, (p.s0 & PACKED_LEN_MASK) - 1))
            }
            _ => None,
        }
    }

    /// The range at `index`, oldest first.
    #[inline]
    pub fn get(&self, index: usize) -> Option<(usize, usize)> {
        match &self.0 {
            Repr::Packed(p) => {
                let word = match index {
                    0 => p.s0,
                    1 => p.s1,
                    2 => p.s2,
                    3 => join_s3(p.s3_lo, p.s3_hi),
                    _ => 0,
                };
                (word != 0).then(|| unpack_range(word))
            }
            Repr::Shared(shared) => shared.get(index),
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
        self.iter().collect()
    }

    /// Pass these locations as a contiguous slice without heap allocation when
    /// stored inline or in `SharedLocs::Packed`/`Wide`.
    pub(crate) fn with_slice<R>(&self, f: impl FnOnce(&[(usize, usize)]) -> R) -> R {
        match &self.0 {
            Repr::Packed(p) => {
                let words = p.to_words();
                let mut buf = [(0usize, 0usize); INLINE];
                let mut count = 0usize;
                for &w in &words {
                    if w == 0 {
                        break;
                    }
                    buf[count] = unpack_range(w);
                    count += 1;
                }
                f(&buf[..count])
            }
            Repr::Shared(shared) => match &**shared {
                SharedLocs::Packed { len, spans } => {
                    let count = usize::from(*len);
                    let mut buf = [(0usize, 0usize); SHARED_PACKED];
                    for (slot, &w) in buf[..count].iter_mut().zip(&spans[..count]) {
                        *slot = unpack_range(w);
                    }
                    f(&buf[..count])
                }
                SharedLocs::Wide(ranges) => f(ranges),
            },
        }
    }

    pub fn clear(&mut self) {
        *self = Self::new();
    }

    /// Add a `(begin, begin + length)` range directly into the packed buffer
    /// when it fits, avoiding `usize` round-trips for grid run decoding.
    #[inline]
    pub(crate) fn push_narrow(&mut self, begin: u32, length: u32) {
        if let Repr::Packed(p) = &mut self.0
            && let Some(packed) = Self::pack_small(begin, length)
        {
            if p.s0 == 0 {
                p.s0 = packed;
                return;
            }
            if p.s1 == 0 {
                p.s1 = packed;
                return;
            }
            if p.s2 == 0 {
                p.s2 = packed;
                return;
            }
            if join_s3(p.s3_lo, p.s3_hi) == 0 {
                let (lo, hi) = split_s3(packed);
                p.s3_lo = lo;
                p.s3_hi = hi;
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
        if let Repr::Packed(p) = &mut self.0
            && let Some(packed) = pack_range(range)
        {
            if p.s0 == 0 {
                p.s0 = packed;
                return;
            }
            if p.s1 == 0 {
                p.s1 = packed;
                return;
            }
            if p.s2 == 0 {
                p.s2 = packed;
                return;
            }
            if join_s3(p.s3_lo, p.s3_hi) == 0 {
                let (lo, hi) = split_s3(packed);
                p.s3_lo = lo;
                p.s3_hi = hi;
                return;
            }
        }
        self.extend_from_slice(&[range]);
    }

    /// Add `more` after the others. Past [`INLINE`] ranges this builds a single
    /// compact `SharedLocs` without an intermediate `Vec` for `<= 7` spans.
    pub fn extend_from_slice(&mut self, more: &[(usize, usize)]) {
        if more.is_empty() {
            return;
        }
        let cur_len = self.len();
        let total = cur_len + more.len();
        if let Some(cur) = self.as_packed_copy() {
            let cw = cur.to_words();
            if total <= INLINE {
                let mut merged = cw;
                let mut fits = true;
                for (slot, &range) in merged[cur_len..total].iter_mut().zip(more) {
                    if let Some(v) = pack_range(range) {
                        *slot = v;
                    } else {
                        fits = false;
                        break;
                    }
                }
                if fits {
                    self.0 = Repr::Packed(PackedLocations::from_words(merged));
                    return;
                }
            } else if total <= SHARED_PACKED {
                let mut spans = [0u32; SHARED_PACKED];
                spans[..cur_len].copy_from_slice(&cw[..cur_len]);
                let mut fits = true;
                for (slot, &range) in spans[cur_len..total].iter_mut().zip(more) {
                    if let Some(v) = pack_range(range) {
                        *slot = v;
                    } else {
                        fits = false;
                        break;
                    }
                }
                if fits {
                    self.0 = Repr::Shared(Arc::new(SharedLocs::Packed {
                        len: total as u8,
                        spans,
                    }));
                    return;
                }
            }
        } else if total <= SHARED_PACKED
            && let Repr::Shared(shared) = &self.0
            && let SharedLocs::Packed { len, spans } = &**shared
        {
            let start = usize::from(*len);
            let mut merged = *spans;
            let mut fits = true;
            for (slot, &range) in merged[start..total].iter_mut().zip(more) {
                if let Some(v) = pack_range(range) {
                    *slot = v;
                } else {
                    fits = false;
                    break;
                }
            }
            if fits {
                self.0 = Repr::Shared(Arc::new(SharedLocs::Packed {
                    len: total as u8,
                    spans: merged,
                }));
                return;
            }
        }
        let all: Vec<(usize, usize)> = self.iter().chain(more.iter().copied()).collect();
        self.0 = Repr::Shared(SharedLocs::from_ranges(&all));
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
        if let (Some(own), Some(add)) = (self.as_packed_copy(), other.as_packed_copy()) {
            let start = self.len();
            let count = other.len();
            let total = start + count;
            let ow = own.to_words();
            let aw = add.to_words();
            if total <= INLINE {
                let mut merged = ow;
                merged[start..total].copy_from_slice(&aw[..count]);
                self.0 = Repr::Packed(PackedLocations::from_words(merged));
                return;
            }
            if total <= SHARED_PACKED {
                let mut spans = [0u32; SHARED_PACKED];
                spans[..start].copy_from_slice(&ow[..start]);
                spans[start..total].copy_from_slice(&aw[..count]);
                self.0 = Repr::Shared(Arc::new(SharedLocs::Packed {
                    len: total as u8,
                    spans,
                }));
                return;
            }
        }
        self.append_slow(other);
    }

    fn append_slow(&mut self, other: &Locations) {
        other.with_slice(|slice| self.extend_from_slice(slice));
    }

    #[inline]
    fn fast_eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Repr::Packed(a), Repr::Packed(b)) => a == b,
            (Repr::Shared(a), Repr::Shared(b)) => Arc::ptr_eq(a, b) || self == other,
            _ => false,
        }
    }
}

/// Stack-resident deduplication cache for batch `Locations` merges (e.g.
/// `pickRestart`, `join`, `AddContext`) when a fixed prefix or suffix pushes
/// repeated haps past the 4-inline limit. Keeps identical spilled location
/// lists sharing one `Arc<SharedLocs>` with zero TLS or retained heap memory.
pub(crate) struct LocMergeCache {
    slots: [(Locations, Locations); 8],
    len: usize,
    next: usize,
}

impl LocMergeCache {
    #[inline]
    pub(crate) const fn new() -> Self {
        Self {
            slots: [const { (Locations::new(), Locations::new()) }; 8],
            len: 0,
            next: 0,
        }
    }

    #[inline]
    pub(crate) fn prepend_into(&mut self, prefix: &Locations, target: &mut Locations) {
        if prefix.is_empty() {
            return;
        }
        if target.is_empty() {
            *target = prefix.clone();
            return;
        }
        if let (Some(cur), Some(pre)) = (target.as_packed_copy(), prefix.as_packed_copy()) {
            let cur_len = target.len();
            let pre_len = prefix.len();
            if cur_len + pre_len <= INLINE {
                let cw = cur.to_words();
                let pw = pre.to_words();
                let mut merged = [0u32; INLINE];
                merged[..pre_len].copy_from_slice(&pw[..pre_len]);
                merged[pre_len..pre_len + cur_len].copy_from_slice(&cw[..cur_len]);
                target.0 = Repr::Packed(PackedLocations::from_words(merged));
                return;
            }
        }
        self.prepend_spilled(prefix, target);
    }

    #[inline(never)]
    fn prepend_spilled(&mut self, prefix: &Locations, target: &mut Locations) {
        for (src, out) in &self.slots[..self.len] {
            if src.fast_eq(target) {
                *target = out.clone();
                return;
            }
        }
        let mut merged = prefix.clone();
        merged.append(target);
        let idx = if self.len < self.slots.len() {
            let i = self.len;
            self.len += 1;
            i
        } else {
            let i = self.next;
            self.next = (self.next + 1) & 7;
            i
        };
        let old = std::mem::replace(target, merged.clone());
        self.slots[idx] = (old, merged);
    }

    #[inline]
    pub(crate) fn append_into(&mut self, target: &mut Locations, suffix: &Locations) {
        if suffix.is_empty() {
            return;
        }
        if target.is_empty() {
            *target = suffix.clone();
            return;
        }
        if let (Some(own), Some(add)) = (target.as_packed_copy(), suffix.as_packed_copy()) {
            let start = target.len();
            let count = suffix.len();
            if start + count <= INLINE {
                let ow = own.to_words();
                let aw = add.to_words();
                let mut merged = ow;
                merged[start..start + count].copy_from_slice(&aw[..count]);
                target.0 = Repr::Packed(PackedLocations::from_words(merged));
                return;
            }
        }
        self.append_spilled(target, suffix);
    }

    #[inline(never)]
    fn append_spilled(&mut self, target: &mut Locations, suffix: &Locations) {
        for (src, out) in &self.slots[..self.len] {
            if src.fast_eq(target) {
                *target = out.clone();
                return;
            }
        }
        let mut merged = target.clone();
        merged.append(suffix);
        let idx = if self.len < self.slots.len() {
            let i = self.len;
            self.len += 1;
            i
        } else {
            let i = self.next;
            self.next = (self.next + 1) & 7;
            i
        };
        let old = std::mem::replace(target, merged.clone());
        self.slots[idx] = (old, merged);
    }

    #[inline]
    pub(crate) fn extend_into(&mut self, target: &mut Locations, more: &[(usize, usize)]) {
        if more.is_empty() {
            return;
        }
        if let Some(cur) = target.as_packed_copy() {
            let cur_len = target.len();
            let total = cur_len + more.len();
            if total <= INLINE {
                let mut merged = cur.to_words();
                let mut fits = true;
                for (slot, &range) in merged[cur_len..total].iter_mut().zip(more) {
                    if let Some(v) = pack_range(range) {
                        *slot = v;
                    } else {
                        fits = false;
                        break;
                    }
                }
                if fits {
                    target.0 = Repr::Packed(PackedLocations::from_words(merged));
                    return;
                }
            }
        }
        self.extend_spilled(target, more);
    }

    #[inline(never)]
    fn extend_spilled(&mut self, target: &mut Locations, more: &[(usize, usize)]) {
        for (src, out) in &self.slots[..self.len] {
            if src.fast_eq(target) {
                *target = out.clone();
                return;
            }
        }
        let mut merged = target.clone();
        merged.extend_from_slice(more);
        let idx = if self.len < self.slots.len() {
            let i = self.len;
            self.len += 1;
            i
        } else {
            let i = self.next;
            self.next = (self.next + 1) & 7;
            i
        };
        let old = std::mem::replace(target, merged.clone());
        self.slots[idx] = (old, merged);
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
            let mut out = Self::from_packed(spans);
            out.push(range);
            for rest in iter {
                out.push(rest);
            }
            return out;
        }
        Self::from_packed(spans)
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
            assert_eq!(
                one.is_inline(),
                begin < PACKED_BEGIN_LIMIT && u32::from(length) <= PACKED_MAX_LEN
            );
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
            assert!(!locations.is_inline());
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
        left.append(&Locations::from(vec![(3, 4), (5, 6), (7, 8)]));
        assert!(left.is_inline());
        assert_eq!(left, vec![(1, 2), (3, 4), (5, 6), (7, 8)]);
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
            vec![(7, 8), (5, 6), (3, 4), (1, 2)]
        );
        assert_eq!(format!("{left:?}"), "[(1, 2), (3, 4), (5, 6), (7, 8)]");
    }
}
