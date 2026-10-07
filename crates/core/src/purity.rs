//! Purity classification.
//!
//! > A `PurePattern` cannot contain, reference, invoke, or lazily materialise
//! > any JavaScript-owned identity.
//!
//! Enforced by **representation**, not inspection:
//!
//! * `Purity` is computed at construction and cached on every `Pattern`.
//! * Every constructor **ORs** impurity from all children and arguments, so
//!   impurity is *monotonic* - it can never be lost by wrapping.
//! * Any unknown or dynamic path defaults to impure. False-impure costs
//!   latency; false-pure costs correctness.
//! * `PurePattern` is a distinct type obtainable only via `Pattern::as_pure_pattern`,
//!   and it is the only thing the tight-lookahead path accepts.
//!
//! The reachable-callback set is collected by the same walk, because L2's
//! `gc_mark` needs it and computing it twice would be waste: it is free here,
//! being the same walk that computes purity.

use crate::CallbackId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Purity {
    /// True if any JS-owned identity is reachable from this pattern.
    pub impure: bool,
    /// Every callback reachable from this subtree. Precomputed so `gc_mark`
    /// is O(k) and allocation-free.
    pub reachable: Vec<CallbackId>,
    /// True when the reachable set is **incomplete** - the graph contains an
    /// opaque closure that materialises patterns at query time, so which
    /// callbacks it will touch cannot be enumerated statically.
    ///
    /// L2's `gc_mark` MUST then mark every cell conservatively. Marking only
    /// `reachable` would let a callback be swept while Rust still needs it.
    pub opaque: bool,
    /// True when two identical queries of this graph may answer differently:
    /// a live input port, memory carried from one query to the next, or
    /// host-owned state that no settings snapshot captures. Purity says "no
    /// JavaScript"; this says "not a function of the query state and the
    /// selected settings alone". A result cache keys on exactly that
    /// function, so nothing reached through a volatile graph is ever cached.
    /// Monotonic like `impure`: wrapping never removes it.
    pub volatile: bool,
}

impl Purity {
    pub fn pure() -> Self {
        Purity {
            impure: false,
            reachable: Vec::new(),
            opaque: false,
            volatile: false,
        }
    }

    /// A JavaScript callback may keep state of its own between calls, so it
    /// is volatile as well as impure.
    pub fn with_callback(id: CallbackId) -> Self {
        Purity {
            impure: true,
            reachable: vec![id],
            opaque: false,
            volatile: true,
        }
    }

    /// No JavaScript, but an answer that may change between identical
    /// queries. What a live input, a cross-query memory, or a read of
    /// mutable host state contributes.
    pub fn volatile() -> Self {
        Purity {
            impure: false,
            reachable: Vec::new(),
            opaque: false,
            volatile: true,
        }
    }

    /// An opaque closure that may materialise anything, including JS. Impure
    /// by construction, with an incomplete reachable set.
    ///
    /// This is the "unknown or dynamic path defaults to impure" rule from
    /// Unavoidable: it is the only classification the
    /// dynamic constructor can produce.
    pub fn opaque() -> Self {
        Purity {
            impure: true,
            reachable: Vec::new(),
            opaque: true,
            volatile: true,
        }
    }

    /// Monotonic union. Impurity propagates upward and is never dropped.
    pub fn merge(parts: impl IntoIterator<Item = Purity>) -> Self {
        let mut out = Purity::pure();
        for p in parts {
            out.impure |= p.impure;
            out.opaque |= p.opaque;
            out.volatile |= p.volatile;
            out.reachable.extend(p.reachable);
        }
        out.reachable.sort_unstable();
        out.reachable.dedup();
        out
    }

    pub fn merge_refs<'a>(parts: impl IntoIterator<Item = &'a Purity>) -> Self {
        let mut out = Purity::pure();
        for p in parts {
            out.impure |= p.impure;
            out.opaque |= p.opaque;
            out.volatile |= p.volatile;
            out.reachable.extend_from_slice(&p.reachable);
        }
        out.reachable.sort_unstable();
        out.reachable.dedup();
        out
    }
}

/// A pattern statically known to contain no JavaScript-owned identity.
///
/// Obtainable only from a `Pattern` whose computed `Purity` says so, so it
/// cannot be constructed around an impure graph. The scheduler's tight-lookahead
/// path accepts only this type - which is what makes the real-time guarantee a
/// property of the type system rather than of a cached boolean.
#[derive(Clone)]
pub struct PurePattern(pub(crate) crate::Pattern);

impl PurePattern {
    pub fn pattern(&self) -> &crate::Pattern {
        &self.0
    }

    /// Wrap a pattern that is *already* classified pure.
    ///
    /// Panics if it is not - which cannot happen from inside this module,
    /// because every method below composes pure inputs with wrappers that
    /// introduce no callback. The assertion is the self-check that keeps that
    /// claim honest rather than assumed.
    pub fn assert_pure(p: crate::Pattern) -> Self {
        assert!(
            !p.purity().impure,
            "PurePattern::assert_pure given an impure pattern - a purity rule \
             was broken inside rustel-core"
        );
        PurePattern(p)
    }
}

/// Purity-preserving combinators.
///
/// These are the operations a `PurePattern` may undergo while remaining
/// provably pure: each only *wraps* its operand and introduces no callback, so
/// pure input implies pure output. Anything that could introduce JavaScript is
/// deliberately absent from this API - that absence is the proof.
impl PurePattern {
    pub fn fast(&self, f: rustel_fraction::Fraction) -> Self {
        Self::assert_pure(self.0.fast(f))
    }
    pub fn slow(&self, f: rustel_fraction::Fraction) -> Self {
        Self::assert_pure(self.0.slow(f))
    }
    pub fn late(&self, t: rustel_fraction::Fraction) -> Self {
        Self::assert_pure(self.0.late(t))
    }
    pub fn early(&self, t: rustel_fraction::Fraction) -> Self {
        Self::assert_pure(self.0.early(t))
    }
    pub fn compress(&self, b: rustel_fraction::Fraction, e: rustel_fraction::Fraction) -> Self {
        Self::assert_pure(self.0.compress(b, e))
    }
    pub fn fast_gap(&self, f: rustel_fraction::Fraction) -> Self {
        Self::assert_pure(self.0.fast_gap(f))
    }
    pub fn repeat_cycles(&self, n: rustel_fraction::Fraction) -> Self {
        Self::assert_pure(self.0.repeat_cycles(n))
    }
    pub fn split_queries(&self) -> Self {
        Self::assert_pure(self.0.split_queries())
    }
    /// Native value transform only - the JS variant lives on `Pattern`.
    pub fn fmap(&self, f: impl Fn(&crate::Value) -> crate::Value + Send + Sync + 'static) -> Self {
        Self::assert_pure(self.0.fmap(f))
    }
    pub fn with_steps(&self, steps: Option<rustel_fraction::Fraction>) -> Self {
        Self::assert_pure(self.0.clone().with_steps(steps))
    }

    /// Query without any callback host installed. If the classification were
    /// wrong, the host lookup panics rather than silently entering JS.
    pub fn query_arc(
        &self,
        begin: rustel_fraction::Fraction,
        end: rustel_fraction::Fraction,
    ) -> Vec<crate::Hap> {
        self.0.query_arc(begin, end)
    }
}
