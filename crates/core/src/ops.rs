/*
ops.rs - the combinator surface shared by `Pattern` and `PurePattern`
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! One trait, two implementations, and the reason `native_combinator!` works.
//!
//! `native_combinator!` instantiates a single combinator body **twice**: once
//! with the operand typed `PurePattern`, once with it typed `Pattern`. For that
//! to compile, every operation a body uses must exist on both types with the
//! same name and a compatible signature. A constructor (`pure`, `stack`,
//! `fastcat`) is harder to share, because its return type differs between the
//! two views.
//!
//! `PatOps` gives both views one surface:
//!
//! * associated constructors (`pat_pure`, `pat_stack`, …) return `Self`, so a
//!   body like `pat.squeeze_bind(|v| Self::pat_pure(v.clone()).fast(n))` type-checks
//!   under either instantiation;
//! * the bind family takes `Fn(&Value) -> Self`, which is exactly the
//!   purity proof - under the `PurePattern` instantiation the closure *cannot*
//!   return a JS-backed pattern, because it cannot construct a `PurePattern`
//!   around one.
//!
//! Every `PurePattern` implementation goes through `assert_pure`, so if a
//! purity rule inside `rustel-core` were ever broken the panic fires here
//! rather than a false-pure pattern escaping to the tight-lookahead path.

use crate::purity::PurePattern;
use crate::{Hap, Pattern, Value};
use rustel_fraction::Fraction;

/// The purity-parametric combinator surface.
///
/// Implemented by `Pattern` (may contain JavaScript) and `PurePattern`
/// (provably may not). A native combinator body written against this trait is
/// valid for both, which is what makes one body two typed views.
pub trait PatOps: Clone + Sized + Send + Sync + 'static {
    // -- constructors ------------------------------------------------------
    fn pat_pure(value: Value) -> Self;
    fn pat_silence() -> Self;
    fn pat_query_error(message: &'static str) -> Self;
    fn pat_query_limit(limit: crate::QueryLimit) -> Self;
    fn pat_stack(pats: Vec<Self>) -> Self;
    fn pat_slowcat(pats: Vec<Self>) -> Self;
    fn pat_slowcat_prime(pats: Vec<Self>) -> Self;
    fn pat_fastcat(pats: Vec<Self>) -> Self;
    fn pat_randrun(n: usize) -> Self;
    fn pat_irand_segment(n: usize) -> Self;

    // -- metadata ----------------------------------------------------------
    /// `_steps`.
    fn pat_steps(&self) -> Option<Fraction>;
    /// `setSteps`.
    fn set_steps(&self, steps: Option<Fraction>) -> Self;

    // -- time --------------------------------------------------------------
    fn fast(&self, factor: Fraction) -> Self;
    fn slow(&self, factor: Fraction) -> Self;
    fn early(&self, t: Fraction) -> Self;
    fn late(&self, t: Fraction) -> Self;
    fn fast_gap(&self, factor: Fraction) -> Self;
    fn compress(&self, b: Fraction, e: Fraction) -> Self;
    fn focus(&self, b: Fraction, e: Fraction) -> Self;
    fn repeat_cycles(&self, count: Fraction) -> Self;
    fn loop_at(&self, factor: Fraction) -> Self;
    fn cpm(&self, cpm: Fraction) -> Self;
    fn split_queries(&self) -> Self;
    fn with_query_time(&self, f: impl Fn(Fraction) -> Fraction + Send + Sync + 'static) -> Self;
    fn with_hap_time(&self, f: impl Fn(Fraction) -> Fraction + Send + Sync + 'static) -> Self;
    fn with_query_span(
        &self,
        f: impl Fn(&crate::TimeSpan) -> crate::TimeSpan + Send + Sync + 'static,
    ) -> Self;
    fn with_hap_span(
        &self,
        f: impl Fn(&crate::TimeSpan) -> crate::TimeSpan + Send + Sync + 'static,
    ) -> Self;
    fn rev(&self) -> Self;
    fn revv(&self) -> Self;
    fn zoom(&self, s: Fraction, e: Fraction) -> Self;

    // -- values ------------------------------------------------------------
    fn fmap(&self, f: impl Fn(&Value) -> Value + Send + Sync + 'static) -> Self;
    fn map_control(&self, spec: crate::controls::ControlSpec, unnamed_only: bool) -> Self;
    fn filter_values(&self, predicate: impl Fn(&Value) -> bool + Send + Sync + 'static) -> Self;
    fn filter_haps(&self, predicate: impl Fn(&Hap) -> bool + Send + Sync + 'static) -> Self;
    fn filter_haps_function(&self, function: Option<&crate::value::FunctionRef>) -> Self;
    fn filter_when_function(&self, function: Option<&crate::value::FunctionRef>) -> Self;
    /// Native `withHaps` with per-hap drop (tonal `scale`'s shape).
    fn map_haps_native(&self, f: impl Fn(&Hap) -> Option<Hap> + Send + Sync + 'static) -> Self;
    /// Internal pitch-only rewrite: named gain, cutoff and resonance controls must not be transformed.
    #[doc(hidden)]
    fn map_pitch_haps_native(
        &self,
        f: impl Fn(&Hap) -> Option<Hap> + Send + Sync + 'static,
    ) -> Self {
        self.map_haps_native(f)
    }
    fn fit(&self) -> Self;
    /// One hap → N haps sharing its span (`voicing()`'s shape).
    fn expand_haps_native(&self, f: impl Fn(&Hap) -> Vec<Hap> + Send + Sync + 'static) -> Self;
    /// Internal voicing expansion, preserving unchanged named audio controls.
    #[doc(hidden)]
    fn expand_pitch_haps_native(
        &self,
        f: impl Fn(&Hap) -> Vec<Hap> + Send + Sync + 'static,
    ) -> Self {
        self.expand_haps_native(f)
    }
    /// `tag(name)` - mark haps for a later `hasTag` filter.
    fn tag(&self, name: std::sync::Arc<str>) -> Self;
    /// `seed(n)` - constant-form withSeed.
    fn with_rand_seed(&self, seed: f64) -> Self;
    /// Declare that this pattern's answer may change between identical
    /// queries - see [`crate::purity::Purity::volatile`]. What a body that
    /// reads mutable host state at query time calls on its result.
    fn mark_volatile(&self) -> Self;
    fn discrete_only(&self) -> Self;
    fn onsets_only(&self) -> Self;
    fn remove_undefineds(&self) -> Self;
    fn degrade_by_seeded(&self, amount: f64, seed: u32) -> Self;

    // -- application -------------------------------------------------------
    fn app_left_with(
        &self,
        other: Self,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
    ) -> Self;
    fn app_left_with_lookup(
        &self,
        other: Self,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
        lookup_flow: crate::LookupFlow,
    ) -> Self;
    fn app_right_with(
        &self,
        other: Self,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
    ) -> Self;
    fn app_right_with_lookup(
        &self,
        other: Self,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
        lookup_flow: crate::LookupFlow,
    ) -> Self;
    fn app_both_with(
        &self,
        other: Self,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
    ) -> Self;
    fn app_both_with_lookup(
        &self,
        other: Self,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
        lookup_flow: crate::LookupFlow,
    ) -> Self;

    // -- joins -------------------------------------------------------------
    fn inner_bind(&self, f: impl Fn(&Value) -> Self + Send + Sync + 'static) -> Self;
    fn outer_bind(&self, f: impl Fn(&Value) -> Self + Send + Sync + 'static) -> Self;
    fn mix_bind(&self, f: impl Fn(&Value) -> Self + Send + Sync + 'static) -> Self;
    fn squeeze_bind(&self, f: impl Fn(&Value) -> Self + Send + Sync + 'static) -> Self;
    fn reset_bind(&self, f: impl Fn(&Value) -> Self + Send + Sync + 'static) -> Self;
    fn restart_bind(&self, f: impl Fn(&Value) -> Self + Send + Sync + 'static) -> Self;
    fn poly_bind(&self, f: impl Fn(&Value) -> Self + Send + Sync + 'static) -> Self;

    /// Apply a transformer carried as a [`Value`] - `every(4, rev)`.
    ///
    /// Under the `PurePattern` instantiation this REQUIRES a native function.
    /// A JS function reaching it is a classification bug, not a user error, and
    /// panics rather than silently producing a false-pure pattern; see
    /// `Registration::takes_function`, which keeps it unreachable.
    fn apply_fn(&self, f: &crate::value::FunctionRef) -> Self;

    /// Apply one indexed transformer to a complete `echoWith` batch.
    ///
    /// The associated form makes the two-phase JavaScript boundary explicit:
    /// the host receives every delayed pattern before `stack` consumes any
    /// callback result.
    fn apply_fn_indexed_batch(
        patterns: Vec<(Self, i64)>,
        f: &crate::value::FunctionRef,
    ) -> Vec<Self>;

    /// Query with the `sortHapsByPart` ordering the snapshots use.
    fn pat_query_arc(&self, begin: Fraction, end: Fraction) -> Vec<Hap>;

    // -- purity view -------------------------------------------------------
    //
    // A generic body that reaches a bind gets an IMPURE result under the
    // `Pattern` instantiation, because `Pattern`'s bind must use the opaque
    // dynamic constructor. These two let such a body re-enter the pure
    // instantiation when its operands actually are pure, so `compose` and
    // friends do not classify every ordinary pattern impure.

    /// The pure view of this pattern, if it has one.
    fn as_pure_view(&self) -> Option<PurePattern>;
    /// Re-wrap a pure result in this view. Widening is always sound.
    fn from_pure_view(pure: PurePattern) -> Self;
}

impl PatOps for Pattern {
    fn pat_pure(value: Value) -> Self {
        crate::pure(value)
    }
    fn pat_silence() -> Self {
        crate::silence()
    }
    fn pat_query_error(message: &'static str) -> Self {
        crate::query_error_pattern(message)
    }
    fn pat_query_limit(limit: crate::QueryLimit) -> Self {
        crate::query_limit_pattern(limit)
    }
    fn pat_stack(pats: Vec<Self>) -> Self {
        crate::stack(pats)
    }
    fn pat_slowcat(pats: Vec<Self>) -> Self {
        crate::slowcat(pats)
    }
    fn pat_slowcat_prime(pats: Vec<Self>) -> Self {
        crate::slowcat_prime(pats)
    }
    fn pat_fastcat(pats: Vec<Self>) -> Self {
        crate::fastcat(pats)
    }
    fn pat_randrun(n: usize) -> Self {
        crate::signal::randrun(n)
    }
    fn pat_irand_segment(n: usize) -> Self {
        crate::combinators::segment(
            &crate::signal::irand_value(Value::F64(n as f64)),
            Fraction::from(n as i64),
        )
    }

    fn pat_steps(&self) -> Option<Fraction> {
        self.steps
    }
    fn set_steps(&self, steps: Option<Fraction>) -> Self {
        self.clone().with_steps(steps)
    }

    fn fast(&self, factor: Fraction) -> Self {
        Pattern::fast(self, factor)
    }
    fn slow(&self, factor: Fraction) -> Self {
        Pattern::slow(self, factor)
    }
    fn early(&self, t: Fraction) -> Self {
        Pattern::early(self, t)
    }
    fn late(&self, t: Fraction) -> Self {
        Pattern::late(self, t)
    }
    fn fast_gap(&self, factor: Fraction) -> Self {
        Pattern::fast_gap(self, factor)
    }
    fn compress(&self, b: Fraction, e: Fraction) -> Self {
        Pattern::compress(self, b, e)
    }
    fn focus(&self, b: Fraction, e: Fraction) -> Self {
        Pattern::focus(self, b, e)
    }
    fn repeat_cycles(&self, count: Fraction) -> Self {
        Pattern::repeat_cycles(self, count)
    }
    fn loop_at(&self, factor: Fraction) -> Self {
        Pattern::loop_at(self, factor)
    }
    fn cpm(&self, cpm: Fraction) -> Self {
        Pattern::cpm(self, cpm)
    }
    fn split_queries(&self) -> Self {
        Pattern::split_queries(self)
    }
    fn with_query_time(&self, f: impl Fn(Fraction) -> Fraction + Send + Sync + 'static) -> Self {
        Pattern::with_query_time(self, f)
    }
    fn with_hap_time(&self, f: impl Fn(Fraction) -> Fraction + Send + Sync + 'static) -> Self {
        Pattern::with_hap_time(self, f)
    }
    fn with_query_span(
        &self,
        f: impl Fn(&crate::TimeSpan) -> crate::TimeSpan + Send + Sync + 'static,
    ) -> Self {
        Pattern::with_query_span(self, f)
    }
    fn with_hap_span(
        &self,
        f: impl Fn(&crate::TimeSpan) -> crate::TimeSpan + Send + Sync + 'static,
    ) -> Self {
        Pattern::with_hap_span(self, f)
    }
    fn rev(&self) -> Self {
        Pattern::rev(self)
    }
    fn revv(&self) -> Self {
        Pattern::revv(self)
    }
    fn zoom(&self, s: Fraction, e: Fraction) -> Self {
        Pattern::zoom(self, s, e)
    }

    fn fmap(&self, f: impl Fn(&Value) -> Value + Send + Sync + 'static) -> Self {
        Pattern::fmap(self, f)
    }
    fn filter_values(&self, predicate: impl Fn(&Value) -> bool + Send + Sync + 'static) -> Self {
        Pattern::filter_values(self, predicate)
    }
    fn filter_haps(&self, predicate: impl Fn(&Hap) -> bool + Send + Sync + 'static) -> Self {
        Pattern::filter_haps(self, predicate)
    }
    fn filter_haps_function(&self, function: Option<&crate::value::FunctionRef>) -> Self {
        match function.and_then(crate::value::FunctionRef::callback_id) {
            Some(id) => self.filter_haps_js(id),
            None => crate::query_error_pattern("filter predicate is not a function"),
        }
    }
    fn filter_when_function(&self, function: Option<&crate::value::FunctionRef>) -> Self {
        match function.and_then(crate::value::FunctionRef::callback_id) {
            Some(id) => self.filter_when_js(id),
            None => crate::query_error_pattern("filterWhen predicate is not a function"),
        }
    }
    fn map_haps_native(&self, f: impl Fn(&Hap) -> Option<Hap> + Send + Sync + 'static) -> Self {
        Pattern::map_haps_native(self, f)
    }
    fn map_pitch_haps_native(
        &self,
        f: impl Fn(&Hap) -> Option<Hap> + Send + Sync + 'static,
    ) -> Self {
        Pattern::map_pitch_haps_native(self, f)
    }
    fn expand_pitch_haps_native(
        &self,
        f: impl Fn(&Hap) -> Vec<Hap> + Send + Sync + 'static,
    ) -> Self {
        Pattern::expand_pitch_haps_native(self, f)
    }
    fn map_control(&self, spec: crate::controls::ControlSpec, unnamed_only: bool) -> Self {
        Pattern::map_control(self, spec, unnamed_only)
    }
    fn fit(&self) -> Self {
        Pattern::fit(self)
    }
    fn expand_haps_native(&self, f: impl Fn(&Hap) -> Vec<Hap> + Send + Sync + 'static) -> Self {
        Pattern::expand_haps_native(self, f)
    }
    fn tag(&self, name: std::sync::Arc<str>) -> Self {
        Pattern::tag(self, name)
    }
    fn with_rand_seed(&self, seed: f64) -> Self {
        Pattern::with_rand_seed(self, seed)
    }
    fn mark_volatile(&self) -> Self {
        Pattern::mark_volatile(self)
    }
    fn discrete_only(&self) -> Self {
        Pattern::discrete_only(self)
    }
    fn onsets_only(&self) -> Self {
        Pattern::onsets_only(self)
    }
    fn remove_undefineds(&self) -> Self {
        Pattern::remove_undefineds(self)
    }
    fn degrade_by_seeded(&self, amount: f64, seed: u32) -> Self {
        Pattern::degrade_by_seeded(self, amount, seed)
    }

    fn app_left_with(
        &self,
        other: Self,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
    ) -> Self {
        Pattern::app_left_with(self, other, combine)
    }
    fn app_left_with_lookup(
        &self,
        other: Self,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
        lookup_flow: crate::LookupFlow,
    ) -> Self {
        Pattern::app_left_with_lookup(self, other, combine, lookup_flow)
    }
    fn app_right_with(
        &self,
        other: Self,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
    ) -> Self {
        Pattern::app_right_with(self, other, combine)
    }
    fn app_right_with_lookup(
        &self,
        other: Self,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
        lookup_flow: crate::LookupFlow,
    ) -> Self {
        Pattern::app_right_with_lookup(self, other, combine, lookup_flow)
    }
    fn app_both_with(
        &self,
        other: Self,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
    ) -> Self {
        Pattern::app_both_with(self, other, combine)
    }
    fn app_both_with_lookup(
        &self,
        other: Self,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
        lookup_flow: crate::LookupFlow,
    ) -> Self {
        Pattern::app_both_with_lookup(self, other, combine, lookup_flow)
    }

    fn inner_bind(&self, f: impl Fn(&Value) -> Self + Send + Sync + 'static) -> Self {
        self.fmap_to_pattern(f).inner_join()
    }
    fn outer_bind(&self, f: impl Fn(&Value) -> Self + Send + Sync + 'static) -> Self {
        // `outerBind` carries `_steps` through, `innerBind` does not.
        self.fmap_to_pattern(f).outer_join().with_steps(self.steps)
    }
    fn mix_bind(&self, f: impl Fn(&Value) -> Self + Send + Sync + 'static) -> Self {
        self.fmap_to_pattern(f).mix_join()
    }
    fn squeeze_bind(&self, f: impl Fn(&Value) -> Self + Send + Sync + 'static) -> Self {
        self.fmap_to_pattern(f).squeeze_join()
    }
    fn reset_bind(&self, f: impl Fn(&Value) -> Self + Send + Sync + 'static) -> Self {
        self.fmap_to_pattern(f).reset_join()
    }
    fn restart_bind(&self, f: impl Fn(&Value) -> Self + Send + Sync + 'static) -> Self {
        self.fmap_to_pattern(f).restart_join()
    }
    fn poly_bind(&self, f: impl Fn(&Value) -> Self + Send + Sync + 'static) -> Self {
        self.fmap_to_pattern(f).poly_join()
    }

    fn apply_fn(&self, f: &crate::value::FunctionRef) -> Self {
        f.apply(self.clone())
    }

    fn apply_fn_indexed_batch(
        patterns: Vec<(Self, i64)>,
        f: &crate::value::FunctionRef,
    ) -> Vec<Self> {
        f.apply_indexed_batch(patterns)
    }

    fn pat_query_arc(&self, begin: Fraction, end: Fraction) -> Vec<Hap> {
        self.query_arc_sorted(begin, end)
    }

    fn as_pure_view(&self) -> Option<PurePattern> {
        self.as_pure_pattern()
    }
    fn from_pure_view(pure: PurePattern) -> Self {
        pure.pattern().clone()
    }
}

/// Every method funnels through `assert_pure`, so the classification claim is
/// re-checked at each step rather than assumed once.
impl PatOps for PurePattern {
    fn pat_pure(value: Value) -> Self {
        PurePattern::assert_pure(crate::pure(value))
    }
    fn pat_silence() -> Self {
        PurePattern::assert_pure(crate::silence())
    }
    fn pat_query_error(message: &'static str) -> Self {
        PurePattern::assert_pure(crate::query_error_pattern(message))
    }
    fn pat_query_limit(limit: crate::QueryLimit) -> Self {
        PurePattern::assert_pure(crate::query_limit_pattern(limit))
    }
    fn pat_stack(pats: Vec<Self>) -> Self {
        PurePattern::assert_pure(crate::stack(
            pats.into_iter().map(|p| p.pattern().clone()).collect(),
        ))
    }
    fn pat_slowcat(pats: Vec<Self>) -> Self {
        PurePattern::assert_pure(crate::slowcat(
            pats.into_iter().map(|p| p.pattern().clone()).collect(),
        ))
    }
    fn pat_slowcat_prime(pats: Vec<Self>) -> Self {
        PurePattern::assert_pure(crate::slowcat_prime(
            pats.into_iter().map(|p| p.pattern().clone()).collect(),
        ))
    }
    fn pat_fastcat(pats: Vec<Self>) -> Self {
        PurePattern::assert_pure(crate::fastcat(
            pats.into_iter().map(|p| p.pattern().clone()).collect(),
        ))
    }
    fn pat_randrun(n: usize) -> Self {
        PurePattern::assert_pure(crate::signal::randrun(n))
    }
    fn pat_irand_segment(n: usize) -> Self {
        PurePattern::assert_pure(crate::combinators::segment(
            &crate::signal::irand_value(Value::F64(n as f64)),
            Fraction::from(n as i64),
        ))
    }

    fn pat_steps(&self) -> Option<Fraction> {
        self.pattern().steps
    }
    fn set_steps(&self, steps: Option<Fraction>) -> Self {
        PurePattern::with_steps(self, steps)
    }

    fn fast(&self, factor: Fraction) -> Self {
        PurePattern::fast(self, factor)
    }
    fn slow(&self, factor: Fraction) -> Self {
        PurePattern::slow(self, factor)
    }
    fn early(&self, t: Fraction) -> Self {
        PurePattern::early(self, t)
    }
    fn late(&self, t: Fraction) -> Self {
        PurePattern::late(self, t)
    }
    fn fast_gap(&self, factor: Fraction) -> Self {
        PurePattern::fast_gap(self, factor)
    }
    fn compress(&self, b: Fraction, e: Fraction) -> Self {
        PurePattern::compress(self, b, e)
    }
    fn focus(&self, b: Fraction, e: Fraction) -> Self {
        PurePattern::assert_pure(self.pattern().focus(b, e))
    }
    fn repeat_cycles(&self, count: Fraction) -> Self {
        PurePattern::repeat_cycles(self, count)
    }
    fn loop_at(&self, factor: Fraction) -> Self {
        PurePattern::assert_pure(self.pattern().loop_at(factor))
    }
    fn cpm(&self, cpm: Fraction) -> Self {
        PurePattern::assert_pure(self.pattern().cpm(cpm))
    }
    fn split_queries(&self) -> Self {
        PurePattern::split_queries(self)
    }
    fn with_query_time(&self, f: impl Fn(Fraction) -> Fraction + Send + Sync + 'static) -> Self {
        PurePattern::assert_pure(self.pattern().with_query_time(f))
    }
    fn with_hap_time(&self, f: impl Fn(Fraction) -> Fraction + Send + Sync + 'static) -> Self {
        PurePattern::assert_pure(self.pattern().with_hap_time(f))
    }
    fn with_query_span(
        &self,
        f: impl Fn(&crate::TimeSpan) -> crate::TimeSpan + Send + Sync + 'static,
    ) -> Self {
        PurePattern::assert_pure(self.pattern().with_query_span(f))
    }
    fn with_hap_span(
        &self,
        f: impl Fn(&crate::TimeSpan) -> crate::TimeSpan + Send + Sync + 'static,
    ) -> Self {
        PurePattern::assert_pure(self.pattern().with_hap_span(f))
    }
    fn rev(&self) -> Self {
        PurePattern::assert_pure(self.pattern().rev())
    }
    fn revv(&self) -> Self {
        PurePattern::assert_pure(self.pattern().revv())
    }
    fn zoom(&self, s: Fraction, e: Fraction) -> Self {
        PurePattern::assert_pure(self.pattern().zoom(s, e))
    }

    fn fmap(&self, f: impl Fn(&Value) -> Value + Send + Sync + 'static) -> Self {
        PurePattern::fmap(self, f)
    }
    fn filter_values(&self, predicate: impl Fn(&Value) -> bool + Send + Sync + 'static) -> Self {
        PurePattern::assert_pure(self.pattern().filter_values(predicate))
    }
    fn filter_haps(&self, predicate: impl Fn(&Hap) -> bool + Send + Sync + 'static) -> Self {
        PurePattern::assert_pure(self.pattern().filter_haps(predicate))
    }
    fn filter_haps_function(&self, _function: Option<&crate::value::FunctionRef>) -> Self {
        PurePattern::assert_pure(crate::query_error_pattern(
            "filter predicate is not a function",
        ))
    }
    fn filter_when_function(&self, _function: Option<&crate::value::FunctionRef>) -> Self {
        PurePattern::assert_pure(crate::query_error_pattern(
            "filterWhen predicate is not a function",
        ))
    }
    fn map_haps_native(&self, f: impl Fn(&Hap) -> Option<Hap> + Send + Sync + 'static) -> Self {
        PurePattern::assert_pure(self.pattern().map_haps_native(f))
    }
    fn map_pitch_haps_native(
        &self,
        f: impl Fn(&Hap) -> Option<Hap> + Send + Sync + 'static,
    ) -> Self {
        PurePattern::assert_pure(self.pattern().map_pitch_haps_native(f))
    }
    fn expand_pitch_haps_native(
        &self,
        f: impl Fn(&Hap) -> Vec<Hap> + Send + Sync + 'static,
    ) -> Self {
        PurePattern::assert_pure(self.pattern().expand_pitch_haps_native(f))
    }
    fn map_control(&self, spec: crate::controls::ControlSpec, unnamed_only: bool) -> Self {
        PurePattern::assert_pure(self.pattern().map_control(spec, unnamed_only))
    }
    fn fit(&self) -> Self {
        PurePattern::assert_pure(self.pattern().fit())
    }
    fn expand_haps_native(&self, f: impl Fn(&Hap) -> Vec<Hap> + Send + Sync + 'static) -> Self {
        PurePattern::assert_pure(self.pattern().expand_haps_native(f))
    }
    fn tag(&self, name: std::sync::Arc<str>) -> Self {
        PurePattern::assert_pure(self.pattern().tag(name))
    }
    fn with_rand_seed(&self, seed: f64) -> Self {
        PurePattern::assert_pure(self.pattern().with_rand_seed(seed))
    }
    fn mark_volatile(&self) -> Self {
        PurePattern::assert_pure(self.pattern().mark_volatile())
    }
    fn discrete_only(&self) -> Self {
        PurePattern::assert_pure(self.pattern().discrete_only())
    }
    fn onsets_only(&self) -> Self {
        PurePattern::assert_pure(self.pattern().onsets_only())
    }
    fn remove_undefineds(&self) -> Self {
        PurePattern::assert_pure(self.pattern().remove_undefineds())
    }
    fn degrade_by_seeded(&self, amount: f64, seed: u32) -> Self {
        PurePattern::assert_pure(self.pattern().degrade_by_seeded(amount, seed))
    }

    fn app_left_with(
        &self,
        other: Self,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
    ) -> Self {
        PurePattern::assert_pure(
            self.pattern()
                .app_left_with(other.pattern().clone(), combine),
        )
    }
    fn app_left_with_lookup(
        &self,
        other: Self,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
        lookup_flow: crate::LookupFlow,
    ) -> Self {
        PurePattern::assert_pure(self.pattern().app_left_with_lookup(
            other.pattern().clone(),
            combine,
            lookup_flow,
        ))
    }
    fn app_right_with(
        &self,
        other: Self,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
    ) -> Self {
        PurePattern::assert_pure(
            self.pattern()
                .app_right_with(other.pattern().clone(), combine),
        )
    }
    fn app_right_with_lookup(
        &self,
        other: Self,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
        lookup_flow: crate::LookupFlow,
    ) -> Self {
        PurePattern::assert_pure(self.pattern().app_right_with_lookup(
            other.pattern().clone(),
            combine,
            lookup_flow,
        ))
    }
    fn app_both_with(
        &self,
        other: Self,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
    ) -> Self {
        PurePattern::assert_pure(
            self.pattern()
                .app_both_with(other.pattern().clone(), combine),
        )
    }
    fn app_both_with_lookup(
        &self,
        other: Self,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
        lookup_flow: crate::LookupFlow,
    ) -> Self {
        PurePattern::assert_pure(self.pattern().app_both_with_lookup(
            other.pattern().clone(),
            combine,
            lookup_flow,
        ))
    }

    fn inner_bind(&self, f: impl Fn(&Value) -> Self + Send + Sync + 'static) -> Self {
        PurePattern::assert_pure(self.pattern().fmap_to_pure_pattern(f).inner_join())
    }
    fn outer_bind(&self, f: impl Fn(&Value) -> Self + Send + Sync + 'static) -> Self {
        let steps = self.pattern().steps;
        PurePattern::assert_pure(
            self.pattern()
                .fmap_to_pure_pattern(f)
                .outer_join()
                .with_steps(steps),
        )
    }
    fn mix_bind(&self, f: impl Fn(&Value) -> Self + Send + Sync + 'static) -> Self {
        PurePattern::assert_pure(self.pattern().fmap_to_pure_pattern(f).mix_join())
    }
    fn squeeze_bind(&self, f: impl Fn(&Value) -> Self + Send + Sync + 'static) -> Self {
        PurePattern::assert_pure(self.pattern().fmap_to_pure_pattern(f).squeeze_join())
    }
    fn reset_bind(&self, f: impl Fn(&Value) -> Self + Send + Sync + 'static) -> Self {
        PurePattern::assert_pure(self.pattern().fmap_to_pure_pattern(f).reset_join())
    }
    fn restart_bind(&self, f: impl Fn(&Value) -> Self + Send + Sync + 'static) -> Self {
        PurePattern::assert_pure(self.pattern().fmap_to_pure_pattern(f).restart_join())
    }
    fn poly_bind(&self, f: impl Fn(&Value) -> Self + Send + Sync + 'static) -> Self {
        PurePattern::assert_pure(self.pattern().fmap_to_pure_pattern(f).poly_join())
    }

    fn apply_fn(&self, f: &crate::value::FunctionRef) -> Self {
        f.apply_pure(self.clone()).unwrap_or_else(|| {
            // Unreachable: a registration that consumes a function argument
            // declares `takes_function`, which keeps the general path on the
            // dynamic constructor, and the fast path checks the argument
            // values. Kept as the self-check that keeps that claim honest.
            PurePattern::assert_pure(f.apply(self.pattern().clone()))
        })
    }

    fn apply_fn_indexed_batch(
        patterns: Vec<(Self, i64)>,
        f: &crate::value::FunctionRef,
    ) -> Vec<Self> {
        let fallback = patterns.clone();
        f.apply_pure_indexed_batch(patterns).unwrap_or_else(|| {
            // Unreachable for the same representation-level reason as the
            // unary arm above. Keep the assertion as a purity self-check.
            f.apply_indexed_batch(
                fallback
                    .into_iter()
                    .map(|(pattern, index)| (pattern.pattern().clone(), index))
                    .collect(),
            )
            .into_iter()
            .map(PurePattern::assert_pure)
            .collect()
        })
    }

    fn pat_query_arc(&self, begin: Fraction, end: Fraction) -> Vec<Hap> {
        self.pattern().query_arc_sorted(begin, end)
    }

    fn as_pure_view(&self) -> Option<PurePattern> {
        Some(self.clone())
    }
    fn from_pure_view(pure: PurePattern) -> Self {
        pure
    }
}
