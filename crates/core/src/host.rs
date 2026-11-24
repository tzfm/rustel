//! Callback host boundary, query error/budget state, and thread-local pools.

use super::*;

// ---------------------------------------------------------------------------
// The JS boundary, from L1's side.
//
// L1 never contains a QuickJS value. A node that invokes user code stores an
// opaque `CallbackId`; the host that can resolve it is provided by L2 and
// reached through a thread-local, because a QuickJS runtime is owned by exactly
// one thread and is not `Send`.
//
// Pure patterns never consult the host. Queries of pure patterns with no
// host installed assert this property.
// ---------------------------------------------------------------------------

/// Opaque index into L2's trace-managed callback sidecar.
pub type CallbackId = usize;

/// What a bind callback is handed.
///
/// `fmap(func)` gives `func` whatever the outer hap holds, and after
/// transpilation that is a pattern as often as it is a scalar: `pure("bd")`
/// transpiles to a pattern-of-patterns, `pure('bd')` stays a string.
pub enum BindArg<'a> {
    Value(&'a Value),
    Pattern(Pattern),
}

/// What a bind callback produced.
///
/// `fmap` replaces the hap's value with the callback's return value. A
/// non-pattern return is therefore the new value, and the join words its
/// error from that value. This is why `stepBind(x => undefined)` and
/// `stepBind(x => 42)` throw different `TypeError`s.
pub enum BindResult {
    Pattern(Pattern),
    Value(Value),
}

/// Implemented by L2 (`rustel-jsruntime`). L1 depends only on this trait.
pub trait CallbackHost {
    /// Report an ordinary query error contained by one stacked child.
    fn log_query_error(&self, _message: &str) {}

    /// `fmap`-style: transform one value.
    fn call_value(&self, id: CallbackId, v: &Value) -> Result<Value, String>;
    fn call_ref(&self, id: CallbackId) -> Result<Pattern, String> {
        Err(format!("callback {id} is not a ref accessor"))
    }
    fn call_pick_lookup(&self, id: CallbackId) -> Result<PickLookup, String> {
        Err(format!("JavaScript value {id} is not a pick lookup"))
    }
    fn call_materialize_value(&self, id: CallbackId) -> Result<Value, String> {
        Err(format!("JavaScript value {id} cannot be materialized"))
    }
    /// A user-authored `new Pattern(state => …)`.
    fn call_query(&self, id: CallbackId, state: &State) -> Result<Vec<Hap>, String>;
    /// A bind callback - `polyBind(x => ...)` / `stepBind(x => ...)`.
    ///
    /// Two things separate this from `call_pattern`, which is a pattern
    /// transformer (`every(4, x => x.fast(2))`):
    ///
    /// * the argument may be either a hap value or a whole pattern, because
    ///   `fmap` passes on whatever the outer hap carried and transpiled
    ///   mini-notation makes that a pattern;
    /// * a return value that is not a pattern is not an `Err` - it becomes
    ///   the hap's value and the failure surfaces only in the join. An error
    ///   here would make `stepBind(x => 42)` (throws during construction)
    ///   indistinguishable from `stepBind(x => { throw })` (constructs, then
    ///   empties the query).
    fn call_bind(&self, id: CallbackId, _arg: BindArg<'_>) -> Result<BindResult, String> {
        Err(format!("callback {id} is not a bind callback"))
    }

    /// A pattern transformer - `every(4, x => x.fast(2))`.
    ///
    /// Defaults to an error so an older host that never sees a function-valued
    /// argument does not have to implement it.
    fn call_pattern(&self, id: CallbackId, _pattern: Pattern) -> Result<Pattern, String> {
        Err(format!("callback {id} is not a pattern transformer"))
    }

    /// A pattern transformer with a bounded native candidate and its original
    /// JavaScript callback retained as the compatibility fallback.
    ///
    /// Hosts that do not implement callback IR remain correct by taking the
    /// ordinary callback path. The runtime that owns the callable decides
    /// whether to execute, dual-run, or bypass the candidate.
    fn call_pattern_ir(
        &self,
        id: CallbackId,
        _program: &callback_ir::PatternTransformProgram,
        pattern: Pattern,
    ) -> Result<Pattern, String> {
        self.call_pattern(id, pattern)
    }

    /// An indexed transformer as `echoWith` uses it.
    ///
    /// Deliberately a batch rather than a unary callback with an extra
    /// argument: every `func(pattern, index)` call runs first, and only then
    /// are the raw results reified `stack`-style (scalars, strings and nested
    /// arrays are all acceptable). The phase boundary lives in L2 because a
    /// later callback may mutate an earlier result or replace the string
    /// parser before any result is reified.
    fn call_pattern_indexed_batch(
        &self,
        id: CallbackId,
        _patterns: Vec<(Pattern, i64)>,
    ) -> Result<Vec<Pattern>, String> {
        Err(format!(
            "callback {id} is not an indexed pattern transformer"
        ))
    }

    /// An `arpWith` callback - receives one congruent chord as `Hap[]` and
    /// returns something `reify()` turns into a pattern.
    ///
    /// The result stays a [`Pattern`] rather than being collapsed to a chosen
    /// value here: callbacks may return `fastcat(...haps)` or `stack(...haps)`,
    /// and that returned pattern's timing is part of `arpWith` semantics.
    fn call_haps(&self, id: CallbackId, _haps: &[Hap]) -> Result<Pattern, String> {
        Err(format!("callback {id} is not a hap-array callback"))
    }
    fn call_hap_predicate(&self, id: CallbackId, _hap: &Hap) -> Result<bool, String> {
        Err(format!("callback {id} is not a hap predicate"))
    }
    /// A `filterValues` predicate - receive one value, answer whether to keep
    /// the hap.
    ///
    /// Distinct from [`CallbackHost::call_value`] so the filter path can
    /// contain a throw per hap (see [`crate::signal_callback_failure`])
    /// without changing `fmap`'s abort-and-empty contract.
    fn call_value_predicate(&self, id: CallbackId, v: &Value) -> Result<bool, String> {
        self.call_value(id, v).map(|value| value.js_truthy())
    }
    fn call_time_predicate(&self, id: CallbackId, _time: Fraction) -> Result<bool, String> {
        Err(format!("callback {id} is not a time predicate"))
    }
    fn call_span_transform(&self, id: CallbackId, _span: TimeSpan) -> Result<TimeSpan, String> {
        Err(format!("callback {id} is not a span transform"))
    }
}

thread_local! {
    /// Erased to a thin pointer + vtable pair with a fabricated lifetime; only
    /// ever set by `with_callback_host`, which guarantees the referent outlives
    /// the scope in which it is readable.
    /// `Cell`, not `RefCell`: queries NEST (a JS callback can query another
    /// pattern), so an outstanding `borrow()` during the inner install would
    /// panic with "RefCell already borrowed". `Cell` has no runtime borrow
    /// state to conflict.
    static CURRENT_HOST: std::cell::Cell<Option<*const (dyn CallbackHost + 'static)>> =
        const { std::cell::Cell::new(None) };
    /// An `arpWith` callback may return a pattern that reaches `arpWith` again.
    /// Count that logical re-entry while the returned pattern is queried;
    /// ordinary native pattern composition remains unlimited by this guard.
    static CALLBACK_RECURSION_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Optional producer-side observation of JavaScript callback crossings.
    /// The pointer is installed only around one profiled query and never
    /// escapes that scope. Nested callback re-entry is counted but timed only
    /// at the outermost boundary so elapsed time is not double-counted.
    pub(crate) static CALLBACK_QUERY_METRICS: std::cell::Cell<Option<*mut CallbackQueryMetrics>> =
        const { std::cell::Cell::new(None) };
    pub(crate) static CALLBACK_QUERY_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    #[cfg(feature = "callback-census")]
    static CALLBACK_QUERY_KIND_SAMPLE_SEQUENCE: std::cell::Cell<u64> =
        const { std::cell::Cell::new(0) };
}

/// A callback-host operation counted during one pattern query.
///
/// This is plain producer-thread data. It is neither shared with nor updated
/// by the audio callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CallbackQueryKind {
    Value,
    Reference,
    PickLookup,
    MaterializeValue,
    Pattern,
    PatternIr,
    IndexedPatternBatch,
    Haps,
    HapPredicate,
    TimePredicate,
    SpanTransform,
    Bind,
    Query,
}

impl CallbackQueryKind {
    pub const ALL: [Self; CALLBACK_QUERY_KIND_COUNT] = [
        Self::Value,
        Self::Reference,
        Self::PickLookup,
        Self::MaterializeValue,
        Self::Pattern,
        Self::PatternIr,
        Self::IndexedPatternBatch,
        Self::Haps,
        Self::HapPredicate,
        Self::TimePredicate,
        Self::SpanTransform,
        Self::Bind,
        Self::Query,
    ];

    const fn index(self) -> usize {
        self as usize
    }
}

const CALLBACK_QUERY_KIND_COUNT: usize = 13;

/// Exact callback crossings grouped by host-boundary operation.
///
/// The fixed-size array keeps collection bounded and allocation-free. Timing
/// remains sampled separately by [`CallbackQueryMetrics`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CallbackQueryKindCounts {
    counts: [u64; CALLBACK_QUERY_KIND_COUNT],
}

impl CallbackQueryKindCounts {
    pub fn get(&self, kind: CallbackQueryKind) -> u64 {
        self.counts[kind.index()]
    }

    pub fn total(&self) -> u64 {
        self.counts.iter().copied().fold(0, u64::saturating_add)
    }

    #[cfg(any(feature = "callback-census", test))]
    fn increment(&mut self, kind: CallbackQueryKind) {
        let count = &mut self.counts[kind.index()];
        *count = count.saturating_add(1);
    }
}

/// Aggregate host-boundary work for one pattern query.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CallbackQueryMetrics {
    calls: u64,
    #[cfg(any(feature = "callback-census", test))]
    kinds: CallbackQueryKindCounts,
    #[cfg(any(feature = "callback-census", test))]
    kinds_sampled: bool,
    busy_nanos: u64,
    outer_calls: u64,
    sampled_outer_calls: u64,
    sampled_busy_nanos: u64,
}

impl CallbackQueryMetrics {
    pub fn calls(&self) -> u64 {
        self.calls
    }

    /// Exact kind counts when this query was selected for the bounded census.
    pub fn kind_sample(&self) -> Option<CallbackQueryKindCounts> {
        #[cfg(any(feature = "callback-census", test))]
        {
            self.kinds_sampled.then_some(self.kinds)
        }
        #[cfg(not(any(feature = "callback-census", test)))]
        {
            None
        }
    }

    /// Estimated time inside the callback host boundary.
    ///
    /// Calls are exact. Timing samples one out of every eight outermost
    /// crossings and scales their mean across the query, avoiding a clock
    /// read around every small callback while retaining a useful phase split.
    pub fn busy_nanos(&self) -> u64 {
        self.busy_nanos
    }
}

const CALLBACK_TIMING_SAMPLE_INTERVAL: u64 = 8;
#[cfg(feature = "callback-census")]
const CALLBACK_KIND_SAMPLE_INTERVAL: u64 = 8;

/// Observe JavaScript callback crossings made by `f`.
///
/// Nested installs restore the previous recorder and depth on every exit,
/// including unwinding. The query owner supplies the storage, so recording is
/// bounded and allocation-free.
pub fn with_callback_query_metrics<R>(
    metrics: &mut CallbackQueryMetrics,
    f: impl FnOnce() -> R,
) -> R {
    #[cfg(feature = "callback-census")]
    let sample_kinds = CALLBACK_QUERY_KIND_SAMPLE_SEQUENCE.with(|sequence| {
        let current = sequence.get();
        sequence.set(current.wrapping_add(1));
        current.is_multiple_of(CALLBACK_KIND_SAMPLE_INTERVAL)
    });
    #[cfg(not(feature = "callback-census"))]
    let sample_kinds = false;
    with_callback_query_metrics_policy(metrics, sample_kinds, f)
}

pub(crate) fn with_callback_query_metrics_policy<R>(
    metrics: &mut CallbackQueryMetrics,
    sample_kinds: bool,
    f: impl FnOnce() -> R,
) -> R {
    struct Guard {
        metrics: *mut CallbackQueryMetrics,
        previous: Option<*mut CallbackQueryMetrics>,
        previous_depth: u32,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            // SAFETY: the caller-owned recorder outlives this complete guard
            // scope and no reference to it crosses a callback.
            let metrics = unsafe { &mut *self.metrics };
            if let Some(busy_nanos) = metrics
                .sampled_busy_nanos
                .saturating_mul(metrics.outer_calls)
                .checked_div(metrics.sampled_outer_calls)
            {
                metrics.busy_nanos = busy_nanos;
            }
            CALLBACK_QUERY_METRICS.with(|slot| slot.set(self.previous));
            CALLBACK_QUERY_DEPTH.with(|slot| slot.set(self.previous_depth));
        }
    }

    *metrics = CallbackQueryMetrics::default();
    #[cfg(any(feature = "callback-census", test))]
    {
        metrics.kinds_sampled = sample_kinds;
    }
    #[cfg(not(any(feature = "callback-census", test)))]
    {
        let _ = sample_kinds;
    }
    let previous = CALLBACK_QUERY_METRICS.with(|slot| slot.replace(Some(metrics)));
    let previous_depth = CALLBACK_QUERY_DEPTH.with(|slot| slot.replace(0));
    let _guard = Guard {
        metrics,
        previous,
        previous_depth,
    };
    f()
}

struct CallbackQueryTimer {
    metrics: Option<*mut CallbackQueryMetrics>,
    started: Option<std::time::Instant>,
    previous_depth: u32,
}

impl CallbackQueryTimer {
    pub(crate) fn enter(kind: CallbackQueryKind) -> Self {
        #[cfg(not(any(feature = "callback-census", test)))]
        let _ = kind;
        let metrics = CALLBACK_QUERY_METRICS.with(|slot| slot.get());
        let Some(metrics) = metrics else {
            return Self {
                metrics: None,
                started: None,
                previous_depth: 0,
            };
        };
        // SAFETY: `with_callback_query_metrics` owns this pointer for the
        // complete dynamic scope. No reference is retained across a callback,
        // so nested re-entry cannot alias a live `&mut`.
        unsafe {
            (*metrics).calls = (*metrics).calls.saturating_add(1);
            #[cfg(any(feature = "callback-census", test))]
            if (*metrics).kinds_sampled {
                (*metrics).kinds.increment(kind);
            }
        }
        let previous_depth = CALLBACK_QUERY_DEPTH.with(|depth| {
            let current = depth.get();
            depth.set(current.saturating_add(1));
            current
        });
        let sampled = if previous_depth == 0 {
            // SAFETY: same scoped pointer contract as above.
            let outer_call = unsafe {
                (*metrics).outer_calls = (*metrics).outer_calls.saturating_add(1);
                (*metrics).outer_calls
            };
            outer_call % CALLBACK_TIMING_SAMPLE_INTERVAL == 1
        } else {
            false
        };
        if sampled {
            // SAFETY: same scoped pointer contract as above.
            unsafe {
                (*metrics).sampled_outer_calls = (*metrics).sampled_outer_calls.saturating_add(1);
            }
        }
        Self {
            metrics: Some(metrics),
            started: sampled.then(std::time::Instant::now),
            previous_depth,
        }
    }
}

impl Drop for CallbackQueryTimer {
    fn drop(&mut self) {
        CALLBACK_QUERY_DEPTH.with(|depth| depth.set(self.previous_depth));
        let (Some(metrics), Some(started)) = (self.metrics, self.started) else {
            return;
        };
        let elapsed = started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        // SAFETY: same scoped pointer contract as `enter`; the recorder still
        // outlives this guard and no mutable reference crosses the callback.
        unsafe {
            (*metrics).sampled_busy_nanos = (*metrics).sampled_busy_nanos.saturating_add(elapsed);
        }
    }
}

#[inline]
fn measure_host_callback<R>(kind: CallbackQueryKind, f: impl FnOnce() -> R) -> R {
    let _timer = CallbackQueryTimer::enter(kind);
    f()
}

// Low enough to unwind safely on a 1 MiB process stack. Self-re-entry must
// become a query error before Rust's non-unwinding stack abort.
const MAX_CALLBACK_RECURSION_DEPTH: usize = 16;

pub(crate) struct CallbackRecursionGuard;

impl CallbackRecursionGuard {
    pub(crate) fn enter() -> Option<Self> {
        CALLBACK_RECURSION_DEPTH.with(|depth| {
            let current = depth.get();
            if current >= MAX_CALLBACK_RECURSION_DEPTH {
                None
            } else {
                depth.set(current + 1);
                Some(Self)
            }
        })
    }
}

impl Drop for CallbackRecursionGuard {
    fn drop(&mut self) {
        CALLBACK_RECURSION_DEPTH.with(|depth| {
            let current = depth.get();
            debug_assert!(current > 0, "callback recursion depth underflow");
            depth.set(current.saturating_sub(1));
        });
    }
}

/// Installs `host` for the duration of `f`. L2 wraps every query in this.
///
/// # Safety
/// The raw pointer never escapes this call: it is stored, used only while `f`
/// runs on this thread, and cleared before returning (including on unwind).
pub fn with_callback_host<R>(host: &dyn CallbackHost, f: impl FnOnce() -> R) -> R {
    struct Guard(Option<*const (dyn CallbackHost + 'static)>);
    impl Drop for Guard {
        fn drop(&mut self) {
            CURRENT_HOST.with(|h| h.set(self.0));
        }
    }
    let prev = CURRENT_HOST.with(|h| {
        // SAFETY: the fabricated 'static lifetime never escapes - the guard
        // restores the previous value before this function returns, including
        // on unwind, so the pointer is unreadable after `host` could die.
        let p: *const (dyn CallbackHost + 'static) =
            unsafe { std::mem::transmute::<*const dyn CallbackHost, _>(host) };
        h.replace(Some(p))
    });
    let _guard = Guard(prev);
    f()
}

/// Whether a callback host is installed on this thread.
pub(crate) fn host_installed() -> bool {
    CURRENT_HOST.with(|host| host.get().is_some())
}

pub(crate) fn host_call_value(id: CallbackId, v: &Value) -> Value {
    CURRENT_HOST.with(|h| match h.get() {
        // SAFETY: set only by `with_callback_host`, which outlives this call.
        // As with `host_call_pattern`: a throw inside the user's function is
        // a query error, not a reason to abort the process.
        Some(p) => match measure_host_callback(CallbackQueryKind::Value, || {
            unsafe { &*p }.call_value(id, v)
        }) {
            Ok(value) => value,
            Err(error) => {
                signal_query_error(move || format!("callback {id} failed: {error}"));
                Value::Undefined
            }
        },
        None => panic!(
            "callback {id} invoked with no host installed. A pattern classified \
             PURE must never reach here: purity is what guarantees it needs no host."
        ),
    })
}

pub(crate) fn host_call_ref(id: CallbackId) -> Pattern {
    CURRENT_HOST.with(|host| match host.get() {
        // SAFETY: set only by `with_callback_host`, which outlives this call.
        Some(pointer) => match measure_host_callback(CallbackQueryKind::Reference, || {
            unsafe { &*pointer }.call_ref(id)
        }) {
            Ok(pattern) => pattern,
            Err(error) => {
                signal_query_error(move || format!("ref callback {id} failed: {error}"));
                silence()
            }
        },
        None => panic!(
            "ref callback {id} invoked with no host installed. A pattern \
             classified PURE must never reach here"
        ),
    })
}

pub(crate) fn host_call_pick_lookup(id: CallbackId) -> Option<PickLookup> {
    CURRENT_HOST.with(|host| match host.get() {
        Some(pointer) => {
            match measure_host_callback(CallbackQueryKind::PickLookup, || {
                unsafe { &*pointer }.call_pick_lookup(id)
            }) {
                Ok(lookup) => Some(lookup),
                Err(error) => {
                    signal_query_error(move || error);
                    None
                }
            }
        }
        None => panic!("JavaScript lookup {id} reached with no host installed"),
    })
}

pub fn materialize_js_value(value: &Value) -> Value {
    match value {
        Value::JsValue(reference) => CURRENT_HOST.with(|host| match host.get() {
            Some(pointer) => {
                match measure_host_callback(CallbackQueryKind::MaterializeValue, || {
                    unsafe { &*pointer }.call_materialize_value(reference.id())
                }) {
                    Ok(value) => value,
                    Err(error) => {
                        signal_query_error(move || error);
                        Value::Undefined
                    }
                }
            }
            None => {
                // A top-level query materialises JS-owned containers before
                // its host scope unwinds. Keep this fallback non-fatal for
                // raw-query callers: rendering a hap must never turn a
                // live-coding value into a Rust process abort merely because
                // it crossed the wrong phase boundary.
                signal_query_error(|| {
                    format!(
                        "JavaScript value {} reached with no host installed",
                        reference.id()
                    )
                });
                Value::Undefined
            }
        }),
        // Controls can wrap a JS-owned value before the outer boundary sees
        // it (`s(pure(['bd']))` is `{s: <opaque array>}`). Recurse through the
        // native container or the inner identity would still escape the host.
        Value::List(values) => Value::list(values.iter().map(materialize_js_value).collect()),
        Value::Object(values) => Value::object(
            values
                .iter()
                .map(|(key, value)| (key.to_string(), materialize_js_value(value))),
        ),
        Value::Haps(haps) => Value::Haps(value::HapList::new(
            haps.as_slice()
                .iter()
                .map(|hap| hap.with_value(materialize_js_value))
                .collect(),
        )),
        _ => value.clone(),
    }
}

/// Apply a user JavaScript function to a pattern, through the host.
///
/// `every(4, x => x.fast(2))` puts the closure in a hap; this is where it is
/// called. Like the other host entry points, a pattern classified PURE must
/// never reach here.
pub(crate) fn host_call_pattern(id: CallbackId, pattern: Pattern) -> Pattern {
    CURRENT_HOST.with(|h| match h.get() {
        // SAFETY: as above.
        Some(p) => match measure_host_callback(CallbackQueryKind::Pattern, || {
            unsafe { &*p }.call_pattern(id, pattern)
        }) {
            Ok(result) => result,
            // A throw inside the user's function is a user error. It reaches
            // the `queryArc` boundary, which yields no haps. A panic here
            // would end the process for a mistake in a live-coded expression.
            // A missing host is a false-pure classification, and it panics
            // below.
            Err(error) => {
                signal_query_error(move || format!("pattern callback {id} failed: {error}"));
                silence()
            }
        },
        None => panic!(
            "pattern callback {id} invoked with no host installed. A pattern \
             classified PURE must never reach here: purity is what guarantees it needs no host."
        ),
    })
}

/// Apply a bounded native callback candidate through its owning host.
///
/// The host retains the original JavaScript function and owns selection and
/// differential policy. A missing host remains a false-pure classification,
/// exactly like the compatibility-only pattern callback path.
///
/// Only the identity callback was a candidate before method chains, and its
/// failures keep their wording. A chain stands for an ordinary callback, so a
/// failure of its JavaScript fallback reads as that callback's would.
pub(crate) fn host_call_pattern_ir(
    id: CallbackId,
    program: &callback_ir::PatternTransformProgram,
    pattern: Pattern,
) -> Pattern {
    CURRENT_HOST.with(|host| match host.get() {
        // SAFETY: set only by `with_callback_host`, which outlives this call.
        Some(pointer) => match measure_host_callback(CallbackQueryKind::PatternIr, || {
            unsafe { &*pointer }.call_pattern_ir(id, program, pattern)
        }) {
            Ok(result) => result,
            Err(error) if program.is_empty() => {
                signal_query_error(move || format!("pattern callback IR {id} failed: {error}"));
                silence()
            }
            Err(error) => {
                signal_query_error(move || format!("pattern callback {id} failed: {error}"));
                silence()
            }
        },
        None => panic!(
            "pattern callback IR {id} invoked with no host installed. A pattern \
             classified PURE must never reach here: purity is what guarantees it needs no host."
        ),
    })
}

/// Apply one JavaScript callback to a complete indexed batch.
///
/// `echoWith`'s two phases live behind this one host call: JavaScript invokes
/// every callback first, then performs `stack`-style reification of every raw
/// result. A throw stops the batch and becomes the same query error as the
/// unary transformer path; a missing host remains a hard purity failure.
pub(crate) fn host_call_pattern_indexed_batch(
    id: CallbackId,
    patterns: Vec<(Pattern, i64)>,
) -> Vec<Pattern> {
    CURRENT_HOST.with(|host| match host.get() {
        // SAFETY: set only by `with_callback_host`, which outlives this call.
        Some(pointer) => match measure_host_callback(CallbackQueryKind::IndexedPatternBatch, || {
            unsafe { &*pointer }.call_pattern_indexed_batch(id, patterns)
        }) {
            Ok(result) => result,
            Err(error) => {
                signal_query_error(move || {
                    format!("indexed pattern callback {id} failed: {error}")
                });
                Vec::new()
            }
        },
        None => panic!(
            "indexed pattern callback {id} invoked with no host installed. A pattern \
             classified PURE must never reach here: purity is what guarantees it needs no host."
        ),
    })
}

/// Apply an `arpWith` callback to one congruent chord through the host.
///
/// A throw is a query error; the outer `queryArc` boundary discards the
/// entire partial query. A missing host
/// remains a hard purity failure.
pub(crate) fn host_call_haps(id: CallbackId, haps: &[Hap]) -> Pattern {
    CURRENT_HOST.with(|h| match h.get() {
        // SAFETY: set only by `with_callback_host`, which outlives this call.
        Some(p) => match measure_host_callback(CallbackQueryKind::Haps, || {
            unsafe { &*p }.call_haps(id, haps)
        }) {
            Ok(result) => result,
            Err(error) => {
                signal_query_error(move || format!("hap-array callback {id} failed: {error}"));
                silence()
            }
        },
        None => panic!(
            "hap-array callback {id} invoked with no host installed. A pattern \
             classified PURE must never reach here: purity is what guarantees it needs no host."
        ),
    })
}

/// Apply a user JavaScript `filterHaps` predicate through the host.
///
/// A throw inside the predicate does NOT abort the query: the hap it was
/// asked to judge keeps playing (fail open), the failure is recorded on the
/// contained-callback-failure channel, and the query's caller reports it
/// once. On stage, one broken predicate must not stop the set.
pub(crate) fn host_call_hap_predicate(id: CallbackId, hap: &Hap) -> bool {
    CURRENT_HOST.with(|host| match host.get() {
        Some(pointer) => match measure_host_callback(CallbackQueryKind::HapPredicate, || unsafe {
            &*pointer
        }
        .call_hap_predicate(id, hap)) {
            Ok(result) => result,
            Err(error) => {
                signal_callback_failure(move || format!("hap predicate {id} failed: {error}"));
                true
            }
        },
        None => panic!(
            "hap predicate {id} invoked with no host installed. A pattern classified PURE must never reach here"
        ),
    })
}

/// Apply a user JavaScript `filterValues` predicate through the host, with
/// the same contain-and-fail-open contract as [`host_call_hap_predicate`].
pub(crate) fn host_call_value_predicate(id: CallbackId, v: &Value) -> bool {
    CURRENT_HOST.with(|host| match host.get() {
        Some(pointer) => match measure_host_callback(CallbackQueryKind::Value, || {
            unsafe { &*pointer }.call_value_predicate(id, v)
        }) {
            Ok(keep) => keep,
            Err(error) => {
                signal_callback_failure(move || format!("callback {id} failed: {error}"));
                true
            }
        },
        None => panic!(
            "callback {id} invoked with no host installed. A pattern classified \
             PURE must never reach here: purity is what guarantees it needs no host."
        ),
    })
}

pub(crate) fn host_call_time_predicate(id: CallbackId, time: Fraction) -> bool {
    CURRENT_HOST.with(|host| match host.get() {
        Some(pointer) => match measure_host_callback(CallbackQueryKind::TimePredicate, || unsafe {
            &*pointer
        }
        .call_time_predicate(id, time)) {
            Ok(result) => result,
            Err(error) => {
                signal_query_error(move || format!("time predicate {id} failed: {error}"));
                false
            }
        },
        None => panic!(
            "time predicate {id} invoked with no host installed. A pattern classified PURE must never reach here"
        ),
    })
}

pub(crate) fn host_call_span_transform(id: CallbackId, span: TimeSpan) -> TimeSpan {
    CURRENT_HOST.with(|host| match host.get() {
        Some(pointer) => match measure_host_callback(CallbackQueryKind::SpanTransform, || unsafe {
            &*pointer
        }
        .call_span_transform(id, span)) {
            Ok(result) => result,
            Err(error) => {
                signal_query_error(move || format!("span transform {id} failed: {error}"));
                span
            }
        },
        None => panic!(
            "span transform {id} invoked with no host installed. A pattern classified PURE must never reach here"
        ),
    })
}

/// Apply a user JavaScript bind callback, through the host.
///
/// A non-pattern return becomes the hap's value, and the join fails on it
/// later. Only a throw (or an argument that is not callable) is a query
/// error here: `fmap` never inspects what the callback returns.
pub(crate) fn host_call_bind(id: CallbackId, arg: BindArg<'_>) -> BindResult {
    CURRENT_HOST.with(|h| match h.get() {
        // SAFETY: as above.
        Some(p) => match measure_host_callback(CallbackQueryKind::Bind, || {
            unsafe { &*p }.call_bind(id, arg)
        }) {
            Ok(result) => result,
            // A throw inside the user's function is user error: it unwinds to
            // the `queryArc` boundary, which yields no haps. Reporting a
            // PATTERN rather than a value keeps this distinct from a
            // non-pattern return, which does NOT stop the query here.
            Err(error) => {
                signal_query_error(move || format!("bind callback {id} failed: {error}"));
                BindResult::Pattern(silence())
            }
        },
        None => panic!(
            "bind callback {id} invoked with no host installed. A pattern \
             classified PURE must never reach here: purity is what guarantees it needs no host."
        ),
    })
}

pub(crate) fn host_call_query(id: CallbackId, state: &State) -> Vec<Hap> {
    CURRENT_HOST.with(|h| match h.get() {
        // SAFETY: as above.
        Some(p) => match measure_host_callback(CallbackQueryKind::Query, || {
            unsafe { &*p }.call_query(id, state)
        }) {
            Ok(haps) => haps,
            Err(error) => {
                signal_query_error(move || format!("query callback {id} failed: {error}"));
                Vec::new()
            }
        },
        None => panic!(
            "query callback {id} invoked with no host installed. A pattern \
             classified PURE must never reach here: purity is what guarantees it needs no host."
        ),
    })
}

/// How an `app*` node makes one value of a function value and an argument
/// value.
#[derive(Clone)]
pub(crate) enum Combine {
    /// [`compose::compose_op`] of this op, which a meeting can run on the
    /// structure hap's value where it is. See [`Meeting::in_place`].
    Composed(compose::ComposeOp),
    /// Multiple constant `set` controls merged into one union map.
    SetMerged,
    /// Any other function, boxed so [`Combine`] stays 16 bytes.
    With(Box<CombineFn>),
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<Combine>() == 16);

/// A function that makes one value of a function value and an argument
/// value.
pub(crate) type CombineFn = Arc<dyn Fn(&Value, &Value) -> Value + Send + Sync>;

impl Combine {
    #[inline]
    pub(crate) fn with(combine: CombineFn) -> Self {
        Self::With(Box::new(combine))
    }

    pub(crate) fn apply(&self, function: &Value, value: &Value) -> Value {
        match self {
            Combine::Composed(op) => compose::compose_op(*op, function, value),
            Combine::SetMerged => compose::compose_op(compose::ComposeOp::Set, function, value),
            Combine::With(apply) => apply(function, value),
        }
    }

    /// The compose op that a meeting can run on the structure hap's value
    /// where that value is, when this function is one.
    pub(crate) fn in_place_op(&self) -> Option<compose::ComposeOp> {
        match self {
            Combine::Composed(op) => Some(*op),
            Combine::SetMerged => Some(compose::ComposeOp::Set),
            Combine::With(_) => None,
        }
    }
}

/// Whether a per-hap map looks at the spans of the hap it is given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpanUse {
    /// The map may read or replace the spans.
    Reads,
    /// The map keeps the spans and reads none of them. Its result for a hap
    /// is then the same wherever the hap sits in time, so a precomputed
    /// table may run it once per entry.
    Ignores,
}
/// Maps a value to a whole pattern, for a `PatternOf` awaiting a join.
pub(crate) type ToPatternFn = Arc<dyn Fn(&Value) -> Pattern + Send + Sync>;
pub(crate) type HapToPatternFn = Arc<dyn Fn(&Hap) -> Pattern + Send + Sync>;
/// A closure whose RETURN TYPE proves it cannot materialise JavaScript.
pub(crate) type ToPurePatternFn = Arc<dyn Fn(&Value) -> purity::PurePattern + Send + Sync>;

/// The payload of a [`Node::PatternOfPureOperand`]: a registered body and
/// the chain width it unpacks values to.
///
/// Both come from the process-wide registry table and the source text, so
/// two evaluations of the same call make equal payloads. The pattern the
/// body reads is NOT part of the payload: it rides in the node as a child
/// the adopt walk compares structurally, which is what lets an unchanged
/// operand carry its answers across evaluations and edits. A host that
/// rebuilt its combinator registry per evaluation would mint fresh body
/// objects and lose that carry, never wrong answers; every current host
/// installs the registry once per runtime.
pub struct PatternedPureMap {
    pub(crate) body: register::PureBody,
    pub(crate) chain: usize,
    /// The constant arguments of the call, each at its position, with
    /// `None` at a position that a value of the chain fills. Empty when
    /// every argument rides in the chain.
    pub(crate) constants: Box<[Option<Value>]>,
}

impl PatternedPureMap {
    pub(crate) fn apply(&self, value: &Value, operand: &Pattern) -> purity::PurePattern {
        let pure = operand
            .as_pure_pattern()
            .expect("a PatternOfPureOperand is built from a pure operand");
        if self.chain == 1 {
            if self.constants.is_empty() {
                return (self.body)(std::slice::from_ref(value), pure);
            }
            let values: Vec<Value> = self
                .constants
                .iter()
                .map(|constant| match constant {
                    Some(c) => c.clone(),
                    None => value.clone(),
                })
                .collect();
            return (self.body)(&values, pure);
        }
        let values = register::argument_values(value, self.chain, &self.constants);
        (self.body)(&values, pure)
    }
}

/// The most constructions one `PatternOfPure` node keeps.
///
/// Argument values repeat: a patterned argument such as `"0,1,2,3"` names
/// four values, and every query of every cycle asks the same four again. A
/// bind's carrier repeats even more - `pure(x).inner_bind(f)` asks about
/// `x` once per cycle per query. Keeping the answers is what lets the graph
/// they build (and any cache inside it) live across queries instead of being
/// rebuilt and thrown away each time.
const CONSTRUCTION_MEMO_ENTRIES: usize = 32;

/// The rate of a `cpm`, in cycles each minute, and the speed change that
/// it makes of the tempo of the scheduler.
pub(crate) struct TempoRate {
    pub(crate) cpm: Fraction,
    /// The speed change at the tempo that asked first, with the bits of
    /// that tempo. A float reads as a fraction slowly, and a session seldom
    /// changes its tempo.
    read: std::sync::OnceLock<(u64, Option<Fraction>, Option<TempoGrid>)>,
}

/// A sixteenth of a child cycle as ticks of the scheduler. Borrows its
/// reduction table from the thread-local cache so a `cpm` node holds no
/// `TickGrid` vector on the heap.
#[derive(Clone, Copy)]
pub(crate) struct TempoGrid {
    denom: std::num::NonZeroU32,
    cycle_ticks: i64,
}

impl TempoGrid {
    const STEPS: i64 = 16;

    fn new(speed: Fraction) -> Option<Self> {
        if speed <= Fraction::ZERO || speed == Fraction::ONE {
            return None;
        }
        let tick = Fraction::new(1, i128::from(Self::STEPS)).checked_div(speed)?;
        let denom_i64 = i64::try_from(tick.denom()).ok()?;
        if denom_i64 <= 0 || denom_i64 > rustel_fraction::TickGrid::MAX_DENOM {
            return None;
        }
        let denom = std::num::NonZeroU32::new(u32::try_from(denom_i64).ok()?)?;
        Some(Self {
            denom,
            cycle_ticks: i64::try_from(tick.numer()).ok()?.checked_mul(Self::STEPS)?,
        })
    }

    #[inline(always)]
    pub(crate) fn denom(&self) -> std::num::NonZeroU32 {
        self.denom
    }

    #[inline(always)]
    pub(crate) fn time_with(
        &self,
        time: Fraction,
        tick_grid: Option<&rustel_fraction::TickGrid>,
    ) -> Option<Fraction> {
        let d = time.denom();
        if d > i128::from(Self::STEPS) || !(d as u32).is_power_of_two() {
            return None;
        }
        let n = i64::try_from(time.numer()).ok()?;
        let ticks = n.checked_mul(self.cycle_ticks >> d.trailing_zeros())?;
        Some(match tick_grid {
            Some(grid) => grid.time(ticks),
            None => Fraction::from_ticks(i128::from(ticks), self.denom),
        })
    }
}

impl TempoRate {
    pub(crate) fn new(cpm: Fraction) -> Self {
        Self {
            cpm,
            read: std::sync::OnceLock::new(),
        }
    }

    /// How many times as fast the pattern runs at `cps` cycles each second
    /// of the scheduler. `None` for a speed outside the native range.
    pub(crate) fn speed_at(&self, cps: f64) -> Option<(Fraction, Option<&TempoGrid>)> {
        let read = || Fraction::from_f64(self.cpm.to_f64() / 60.0 / cps);
        let (kept, speed, grid) = self.read.get_or_init(|| {
            let speed = read();
            (cps.to_bits(), speed, speed.and_then(TempoGrid::new))
        });
        match *kept == cps.to_bits() {
            true => Some(((*speed)?, grid.as_ref())),
            false => Some((read()?, None)),
        }
    }
}

/// Patterns a `PatternOfPure` closure has already built, by the value it was
/// asked about and the settings snapshot it was asked under.
///
/// Exact: a native body is a function of its argument values and the
/// selected settings, so the pattern it built for a value IS the pattern it
/// would build again. NaN never equals itself and is never kept; a volatile
/// construction (one that would start fresh memory each time) is not kept
/// either, so sharing cannot change what it answers. Bounded, and drained
/// onto the destruction worklist like any other child.
#[derive(Default)]
pub(crate) struct ConstructionMemo {
    entries: Mutex<Constructions>,
    /// Round-robin victim once full. The key set is a handful of argument
    /// values, so plain replacement is enough and never thrashes on it.
    victim: std::sync::atomic::AtomicUsize,
}

/// What a [`ConstructionMemo`] holds.
#[derive(Default)]
pub(crate) struct Constructions {
    kept: Vec<ConstructionEntry>,
}

pub(crate) struct ConstructionEntry {
    value: Value,
    settings: settings::SettingsStateId,
    /// The standings of the compiled programs the construction applied. See
    /// [`callback_ir::recording_reads`].
    read: Box<[Arc<callback_ir::Standing>]>,
    pattern: purity::PurePattern,
}

/// Whether two memo keys name the same construction.
///
/// Stricter than `Value`'s equality for numbers: `-0.0 == 0.0` there, while a
/// body may well tell them apart (`1 / x`, or how the value prints), so the
/// bits have to agree. Containers compare element by element the same way;
/// everything else as `Value` compares.
pub(crate) fn same_key(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::F64(x), Value::F64(y)) => x.to_bits() == y.to_bits(),
        (Value::List(xs), Value::List(ys)) => {
            xs.len() == ys.len() && xs.iter().zip(ys.iter()).all(|(x, y)| same_key(x, y))
        }
        (Value::Object(xs), Value::Object(ys)) => {
            xs.len() == ys.len()
                && xs
                    .iter()
                    .zip(ys.iter())
                    .all(|((kx, x), (ky, y))| kx == ky && same_key(x, y))
        }
        _ => a == b,
    }
}

/// Whether a value may serve as a memo key: numbers, strings, booleans and
/// nothings, and lists and objects of those. A NaN could never be found
/// again; a pattern, function, hap list or host-owned value can hold a graph,
/// and a key holding a graph is how a cycle would form.
pub(crate) fn plain_key(value: &Value) -> bool {
    match value {
        Value::Undefined | Value::Null | Value::Bool(_) | Value::Str(_) => true,
        Value::F64(number) => !number.is_nan(),
        Value::List(items) => items.iter().all(plain_key),
        Value::Object(map) => map.iter().all(|(_, item)| plain_key(item)),
        Value::Pattern(_) | Value::JsValue(_) | Value::Function(_) | Value::Haps(_) => false,
    }
}

impl ConstructionMemo {
    fn lock(&self) -> std::sync::MutexGuard<'_, Constructions> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The pattern built for `value` under `settings`, building it with
    /// `construct` when nothing kept answers.
    pub(crate) fn construct(
        &self,
        value: &Value,
        settings: &settings::SettingsStateId,
        construct: impl FnOnce(&Value) -> purity::PurePattern,
    ) -> purity::PurePattern {
        {
            let constructions = self.lock();
            if let Some(kept) = constructions.kept.iter().find(|entry| {
                entry.settings == *settings
                    && same_key(&entry.value, value)
                    && callback_ir::reads_hold(&entry.read)
            }) {
                callback_ir::note_read(&kept.read);
                return kept.pattern.clone();
            }
        }
        // Built outside the lock: construction is arbitrary native code.
        let (built, read) = callback_ir::recording_reads(|| construct(value));
        let keep = plain_key(value)
            && !built.pattern().is_volatile()
            && !query_error_pending()
            && !budget_exhausted();
        if keep {
            let entry = ConstructionEntry {
                value: value.clone(),
                settings: settings.clone(),
                read,
                pattern: built.clone(),
            };
            let mut constructions = self.lock();
            if constructions.kept.len() < CONSTRUCTION_MEMO_ENTRIES {
                // Most memos hold a single value; allocate exact capacity on
                // the first insert rather than Rust's default 4-slot growth.
                if constructions.kept.is_empty() {
                    constructions.kept.reserve_exact(1);
                }
                constructions.kept.push(entry);
            } else {
                let victim = self
                    .victim
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    % CONSTRUCTION_MEMO_ENTRIES;
                constructions.kept[victim] = entry;
            }
        }
        built
    }

    /// Hand every kept pattern to the destruction worklist.
    pub(crate) fn drain_into(&self, out: &mut Vec<Pattern>) {
        let mut constructions = self.lock();
        out.extend(constructions.kept.drain(..).map(|entry| entry.pattern.0));
    }
}
/// A native value transform.
pub(crate) type ValueFn = Arc<dyn Fn(&Value) -> Value + Send + Sync>;
/// A native value predicate. Public because a combinator that interns its
/// predicate hands the built object to [`crate::ops::PatOps::filter_values_with`].
pub type ValuePredicate = Arc<dyn Fn(&Value) -> bool + Send + Sync>;
/// A native hap predicate - `filterHaps`, which sees whole/part, not just the
/// value. `discreteOnly`/`onsetsOnly` are the two uses the joins need.
pub(crate) type HapPredicate = Arc<dyn Fn(&Hap) -> bool + Send + Sync>;
pub(crate) type HapMapFn = Arc<dyn Fn(&Hap) -> Option<Hap> + Send + Sync>;
/// A native hap rewrite that owns the hap. Public because the REPL's lane
/// tag interns its map and hands the built object to
/// [`crate::Pattern::fmap_keeping_live_with`].
pub type OwnedHapMapFn = Arc<dyn Fn(Hap) -> Option<Hap> + Send + Sync>;
pub(crate) type StateHapMapFn = Arc<dyn Fn(&State, &Hap) -> Option<Hap> + Send + Sync>;
pub(crate) type HapExpandFn = Arc<dyn Fn(&Hap, &mut Vec<Hap>) + Send + Sync>;
/// A native continuous signal sampled once per query.
pub(crate) type SignalFn = Arc<dyn Fn(&State) -> Value + Send + Sync>;
/// A time transform applied to query or hap spans.
pub(crate) type TimeFn = Arc<dyn Fn(Fraction) -> Fraction + Send + Sync>;
/// A whole-span transform - `withQuerySpan` / `withHapSpan`. Distinct from
/// `TimeFn` because `zoom` and `revv` map begin and end **jointly**, and
/// `withCycle` needs the span's own `sam()`.
pub(crate) type SpanFn = Arc<dyn Fn(&TimeSpan) -> TimeSpan + Send + Sync>;

/// The most closures one intern pool keeps.
pub(crate) const MAX_INTERNED_CLOSURES: usize = 4096;

/// One closure object per key while any live pattern holds it.
///
/// A node holds a transform as an opaque closure object, and two builds of
/// one construction agree on it only while it is the same object. A closure
/// whose behaviour is a pure function of a small deterministic key - a
/// capture-free body, or plain data it captured - is interned weakly here:
/// while any live pattern holds the closure, the same key built again hands
/// out the same `Arc` so unchanged subtrees agree across evaluations and
/// edits. Once no live pattern holds it, the closure frees immediately and
/// dead slots are swept before growing the table.
///
/// Past [`MAX_INTERNED_CLOSURES`] live keys a pool stops taking entries: a
/// score that mints one key a call must not grow it without end, and a
/// fresh closure only loses the carry.
pub fn interned_closure<K, F: ?Sized>(
    pool: &std::sync::Mutex<std::collections::HashMap<K, std::sync::Weak<F>>>,
    key: K,
    make: impl FnOnce() -> Arc<F>,
) -> Arc<F>
where
    K: std::hash::Hash + PartialEq + Eq,
{
    if let Ok(entries) = pool.lock()
        && let Some(found) = entries.get(&key).and_then(std::sync::Weak::upgrade)
    {
        return found;
    }
    let made = make();
    if let Ok(mut entries) = pool.lock() {
        if entries.len() >= 32 {
            entries.retain(|_, weak| weak.strong_count() > 0);
            if entries.capacity() > 128 && entries.len().saturating_mul(4) < entries.capacity() {
                let target = entries.len().saturating_mul(2).max(32);
                entries.shrink_to(target);
            }
        }
        if entries.len() < MAX_INTERNED_CLOSURES {
            entries.insert(key, Arc::downgrade(&made));
        }
    }
    made
}

/// Live `Node` allocations. This cheap always-on counter observes Rust graph
/// lifetime independently of the JavaScript heap; aggregate RSS cannot make
/// that distinction.
pub(crate) static LIVE_NODES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The predicate of each [`Pattern::remove_undefineds`]: the strip a
/// `keepif` makes is the same payload wherever it is built again, so that
/// subtree carries its answers. See [`interned_closure`].
pub(crate) static KEEP_DEFINED: std::sync::OnceLock<ValuePredicate> = std::sync::OnceLock::new();

/// Number of `Node`s currently allocated across all patterns.
pub fn live_node_count() -> u64 {
    LIVE_NODES.load(std::sync::atomic::Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Query errors - the `queryArc` try/catch.
//
// Several operations **throw** mid-query: `parseNumeral` on a value that is
// neither numeric nor a note name, `TimeSpan.intersection_e` on disjoint
// spans. A stack contains the failure to one child; otherwise it reaches
// `queryArc`, which logs it and returns no haps.
//
// Rust has no exception to unwind, and making `query` return a `Result` would
// put a branch on the hottest path in the scheduler for a case only user
// error reaches. A thread-local flag carries the error to the nearest stack
// child boundary or the outer query boundary, which discards its result.
//
// writer                     slot                       reader
// signal_query_error      -> QUERY_ERROR             -> stack child boundary
//                                                       or queryArc boundary
// stack child boundary    -> CALLBACK_FAILURE,       -> queryArc boundary
//                            CONTAINED_THROW
// signal_callback_failure -> CALLBACK_FAILURE        -> queryArc boundary
// queryArc boundary       -> QUERY_CALLBACK_FAILURE, -> top-level caller
// (no query error)           QUERY_CONTAINED_THROW
// ---------------------------------------------------------------------------

/// The error slots of the table above, in one thread-local: a boundary
/// reads and writes several of them at its entry and at its end, and
/// reaches the thread one time for each.
pub(crate) struct QueryErrors {
    /// `QUERY_ERROR`: the pending error of the query.
    error: std::cell::RefCell<Option<String>>,
    /// `CALLBACK_FAILURE`: a failure the query contained rather than
    /// aborted on. The failure is reported once by the query's caller;
    /// first one wins per boundary.
    callback_failure: std::cell::RefCell<Option<String>>,
    /// `QUERY_CALLBACK_FAILURE`: the contained callback failure published
    /// by the last finished `queryArc` boundary, for the top-level caller
    /// to take and report. Cleared at top-level query entry so a stale
    /// failure cannot be attributed to the next independent query.
    published_callback_failure: std::cell::RefCell<Option<String>>,
    /// `CONTAINED_THROW`: a query error a stack contained to one child.
    /// The siblings keep playing, but the caller still reports a throw;
    /// first one wins per boundary.
    contained_throw: std::cell::RefCell<Option<String>>,
    /// `QUERY_CONTAINED_THROW`: the contained throw published by the last
    /// finished `queryArc` boundary, cleared with the published callback
    /// failure.
    published_contained_throw: std::cell::RefCell<Option<String>>,
    /// How many `queryArc`-style boundaries are live on this thread. See
    /// [`query_in_progress`].
    boundaries: std::cell::Cell<u32>,
    /// Fast summary bit: true when `error`, `callback_failure`, or
    /// `contained_throw` may hold `Some(_)`.
    has_inner: std::cell::Cell<bool>,
    /// Fast summary bit: true when `published_callback_failure` or
    /// `published_contained_throw` may hold `Some(_)`.
    has_published: std::cell::Cell<bool>,
}

thread_local! {
    static QUERY_ERRORS: QueryErrors = const {
        QueryErrors {
            error: std::cell::RefCell::new(None),
            callback_failure: std::cell::RefCell::new(None),
            published_callback_failure: std::cell::RefCell::new(None),
            contained_throw: std::cell::RefCell::new(None),
            published_contained_throw: std::cell::RefCell::new(None),
            boundaries: std::cell::Cell::new(0),
            has_inner: std::cell::Cell::new(false),
            has_published: std::cell::Cell::new(false),
        }
    };
}

/// Whether a query is in progress on this thread: a `Pattern::query` frame,
/// or a `queryArc`-style boundary around one - including the cycle-0 probe
/// `stepJoin`/`stepBind` run while their pattern is still being constructed,
/// which resolves a pattern-of-patterns outside any query frame.
///
/// A host uses this to tell a callback the score's own method call invoked
/// synchronously, outside every query, from one a query reached. Only the
/// former throws straight back into the score; the latter belongs to the
/// query's boundary, which answers silence, as `queryArc` does.
pub fn query_in_progress() -> bool {
    QUERY_ERRORS.with(|errors| errors.boundaries.get() > 0)
        || QUERY_DEPTH.with(|depth| depth.get() > 0)
}

/// Record a mid-query failure. The first one wins, as a thrown exception
/// would ensure.
pub fn signal_query_error(message: impl FnOnce() -> String) {
    QUERY_ERRORS.with(|errors| {
        let mut slot = errors.error.borrow_mut();
        if slot.is_none() {
            *slot = Some(message());
            errors.has_inner.set(true);
            set_stop_bit(STOP_ERROR_PENDING, true);
        }
    });
}

/// Take and clear the pending query error, if any.
pub fn take_query_error() -> Option<String> {
    set_stop_bit(STOP_ERROR_PENDING, false);
    QUERY_ERRORS.with(|errors| {
        if !errors.has_inner.get() {
            return None;
        }
        let taken = errors.error.borrow_mut().take();
        if taken.is_some()
            && errors.callback_failure.borrow().is_none()
            && errors.contained_throw.borrow().is_none()
        {
            errors.has_inner.set(false);
        }
        taken
    })
}

/// Record a failure the query contains, reported once by its caller instead
/// of emptying the whole arc. Filters keep the affected hap; stacks discard
/// the failed child's haps and keep the other children playing.
///
/// The flag also prevents result caches from retaining an incomplete answer.
pub fn signal_callback_failure(message: impl FnOnce() -> String) {
    QUERY_ERRORS.with(|errors| {
        let mut slot = errors.callback_failure.borrow_mut();
        if slot.is_none() {
            *slot = Some(message());
            errors.has_inner.set(true);
        }
    });
}

/// Take the contained callback failure published by the last completed
/// `queryArc` boundary, if any.
pub fn take_query_callback_failure() -> Option<String> {
    QUERY_ERRORS.with(|errors| {
        if !errors.has_published.get() {
            return None;
        }
        let taken = errors.published_callback_failure.borrow_mut().take();
        if taken.is_some() && errors.published_contained_throw.borrow().is_none() {
            errors.has_published.set(false);
        }
        taken
    })
}

/// Take the query error a stack contained, published by the last completed
/// `queryArc` boundary, if any. The same failure is also published as a
/// contained callback failure; this channel says that a stack child threw.
pub fn take_query_contained_throw() -> Option<String> {
    QUERY_ERRORS.with(|errors| {
        if !errors.has_published.get() {
            return None;
        }
        let taken = errors.published_contained_throw.borrow_mut().take();
        if taken.is_some() && errors.published_callback_failure.borrow().is_none() {
            errors.has_published.set(false);
        }
        taken
    })
}

fn signal_contained_throw(message: String) {
    QUERY_ERRORS.with(|errors| {
        errors.contained_throw.borrow_mut().get_or_insert(message);
        errors.has_inner.set(true);
    });
}

/// Whether the query in progress can no longer be trusted to have answered
/// completely: a mid-query error, a contained callback failure, a resource
/// refusal, an expired deadline, or a cancellation request.
///
/// A result cache asks this before keeping what a child answered. What an
/// interrupted query produced is what the `queryArc` boundary is about to
/// discard, and keeping it would serve the discarded answer to the next,
/// healthy query. The flags are sticky for the top-level query, so an
/// interruption anywhere earlier in it also refuses the cache - the
/// conservative side.
pub fn query_interrupted() -> bool {
    query_error_pending()
        || QUERY_ERRORS
            .with(|errors| errors.has_inner.get() && errors.callback_failure.borrow().is_some())
        || budget_exhausted()
        || cancellation_observed()
        || cancellation_requested()
}

thread_local! {
    /// How many times a query on this thread has reached something no
    /// result cache may keep: a volatile pattern, a handle bound to a
    /// runtime of its own, or an incomplete raw stack query. Monotonic, so a
    /// cache reads it before and after the evaluation it is about to keep.
    static UNCACHEABLE_TOUCHES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

pub(crate) fn note_uncacheable() {
    UNCACHEABLE_TOUCHES.with(|touches| touches.set(touches.get().wrapping_add(1)));
}

/// How many times a query on this thread has so far reached something a
/// result cache must not keep - see [`purity::Purity::volatile`].
///
/// The static classification is what a node reads when it is built; this is
/// what it reads around the evaluation itself, because a pattern-of-patterns
/// materialises its inner graphs only at query time, and one of those may be
/// volatile although the carrier is not. A cache that sees this move during
/// an evaluation keeps nothing from it.
pub fn uncacheable_touches() -> u64 {
    UNCACHEABLE_TOUCHES.with(|touches| touches.get())
}

impl QueryErrors {
    /// A fresh top-level query starts with no contained failure to report.
    fn clear_published(&self) {
        if self.has_published.get() {
            *self.published_callback_failure.borrow_mut() = None;
            *self.published_contained_throw.borrow_mut() = None;
            self.has_published.set(false);
        }
    }

    /// What the boundary that ends now holds, taken: its error, the
    /// failure it contained and the throw a stack contained in it.
    fn take_inner(&self) -> [Option<String>; 3] {
        if !self.has_inner.get() {
            return [None, None, None];
        }
        let taken = [
            self.error.borrow_mut().take(),
            self.callback_failure.borrow_mut().take(),
            self.contained_throw.borrow_mut().take(),
        ];
        self.has_inner.set(false);
        let _ = QUERY_THREAD.try_with(|thread| thread.set_stop_bit(STOP_ERROR_PENDING, false));
        taken
    }
}

/// One `queryArc`-style error boundary.
///
/// The pending outer exception must survive nested queries, and any exception
/// raised inside this boundary must be discarded on unwind just as JavaScript
/// stack unwinding would discard it. Keeping that restoration in `Drop` avoids
/// poisoning the next query when a native checked-arithmetic panic is caught by
/// an embedding host.
///
/// The contained-callback-failure channel gets the same treatment: stashed on
/// entry, restored on exit, and whatever the inner query contained is
/// published for the outermost caller to take.
pub(crate) struct QueryErrorBoundary {
    outer: Option<String>,
    outer_callback_failure: Option<String>,
    outer_contained_throw: Option<String>,
    has_outer: bool,
    restored: bool,
}

impl QueryErrorBoundary {
    #[inline]
    fn enter_with(clear_published: bool) -> Self {
        QUERY_ERRORS.with(|errors| {
            if clear_published && errors.has_published.get() {
                errors.clear_published();
            }
            errors
                .boundaries
                .set(errors.boundaries.get().saturating_add(1));
            if !errors.has_inner.get() {
                return Self {
                    outer: None,
                    outer_callback_failure: None,
                    outer_contained_throw: None,
                    has_outer: false,
                    restored: false,
                };
            }
            let [outer, outer_callback_failure, outer_contained_throw] = errors.take_inner();
            let has_outer = outer.is_some()
                || outer_callback_failure.is_some()
                || outer_contained_throw.is_some();
            Self {
                outer,
                outer_callback_failure,
                outer_contained_throw,
                has_outer,
                restored: false,
            }
        })
    }

    #[inline]
    pub(crate) fn enter() -> Self {
        Self::enter_with(false)
    }

    #[inline]
    pub(crate) fn enter_top_level() -> Self {
        Self::enter_with(true)
    }

    #[inline]
    pub(crate) fn finish(mut self) -> Option<String> {
        let inner = QUERY_ERRORS.with(|errors| {
            errors
                .boundaries
                .set(errors.boundaries.get().saturating_sub(1));
            if !errors.has_inner.get() && !self.has_outer {
                return None;
            }
            let [inner, inner_callback_failure, inner_contained_throw] = errors.take_inner();
            self.restore_in(errors);
            if inner.is_some() {
                // The queryArc aborted. Discard contained failures with
                // the partial haps, including one that a nested boundary
                // already published. Report only the throw; a "kept
                // playing" warning would be false for a silent window.
                errors.clear_published();
            } else {
                if let Some(failure) = inner_callback_failure {
                    *errors.published_callback_failure.borrow_mut() = Some(failure);
                    errors.has_published.set(true);
                }
                if let Some(thrown) = inner_contained_throw {
                    *errors.published_contained_throw.borrow_mut() = Some(thrown);
                    errors.has_published.set(true);
                }
            }
            inner
        });
        self.restored = true;
        // All `Option<String>` slots are `None` (either `has_outer == false` or
        // `restore_in` took them), and `boundaries` was already decremented.
        std::mem::forget(self);
        inner
    }

    /// Restore the outer error slots and discard any inner error or contained
    /// failure without publishing it. Used by speculative probes such as
    /// [`without_query_signals`].
    pub(crate) fn discard(mut self) -> bool {
        let had_inner = QUERY_ERRORS.with(|errors| {
            errors
                .boundaries
                .set(errors.boundaries.get().saturating_sub(1));
            if !errors.has_inner.get() && !self.has_outer {
                return false;
            }
            let [inner, inner_callback_failure, inner_contained_throw] = errors.take_inner();
            self.restore_in(errors);
            inner.is_some() || inner_callback_failure.is_some() || inner_contained_throw.is_some()
        });
        self.restored = true;
        std::mem::forget(self);
        had_inner
    }

    /// Put back what the query around this boundary held at its entry.
    fn restore_in(&mut self, errors: &QueryErrors) {
        if self.restored {
            return;
        }
        if self.has_outer {
            let outer = self.outer.take();
            let has_error = outer.is_some();
            let outer_cb = self.outer_callback_failure.take();
            let outer_throw = self.outer_contained_throw.take();
            let has_any = has_error || outer_cb.is_some() || outer_throw.is_some();
            *errors.error.borrow_mut() = outer;
            *errors.callback_failure.borrow_mut() = outer_cb;
            *errors.contained_throw.borrow_mut() = outer_throw;
            errors.has_inner.set(has_any);
            let _ =
                QUERY_THREAD.try_with(|thread| thread.set_stop_bit(STOP_ERROR_PENDING, has_error));
        } else if errors.has_inner.get() {
            let _ = errors.take_inner();
        }
        self.restored = true;
    }
}

impl Drop for QueryErrorBoundary {
    fn drop(&mut self) {
        // The slots are gone when the thread ends.
        let _ = QUERY_ERRORS.try_with(|errors| {
            self.restore_in(errors);
            errors
                .boundaries
                .set(errors.boundaries.get().saturating_sub(1));
        });
    }
}

/// Whether a query exception has already been raised.
///
/// Callback-bearing loops use this to stop at the first throw. Continuing to
/// invoke user code would expose side effects an unwinding exception would
/// have prevented, even though `queryArc` discards all partial haps anyway.
#[inline]
pub(crate) fn query_error_pending() -> bool {
    QUERY_THREAD.with(QueryThread::error_pending)
}

/// Contain ordinary child errors at a stack boundary, for both event queries
/// and pattern-of-pattern resolution. Structural refusals remain query-wide.
pub(crate) fn query_stack_child<T>(query: impl FnOnce() -> Vec<T>) -> Vec<T> {
    if query_error_pending() {
        return Vec::new();
    }
    let haps = query();
    match stack_child_failed() {
        true => Vec::new(),
        false => haps,
    }
}

/// The haps of the children `pats` of a stack, after those of `out`. Each
/// child writes after the one before it. A child that fails writes
/// nothing, and its siblings stay. `before` runs before each child is
/// asked.
pub(crate) fn query_stack_onto(
    thread: &QueryThread,
    pats: &[Pattern],
    state: &State,
    out: &mut Vec<Hap>,
    before: impl Fn(),
) {
    let from = out.len();
    for pat in pats.iter().filter(|pat| !pat.is_rest()) {
        before();
        stack_child_onto(thread, out, |out| {
            pat.answer_child_onto(thread, state, out);
        });
        let held = out.len() - from;
        if !thread.charge(held) {
            out.truncate(from);
            return;
        }
        thread.note_accumulated(held);
    }
}

pub(crate) fn query_stack_pure_onto(
    thread: &QueryThread,
    cells: &[Arc<NodeCell>],
    state: &State,
    out: &mut Vec<Hap>,
) {
    let from = out.len();
    for cell in cells
        .iter()
        .filter(|cell| !matches!(cell.kind, Node::Silence))
    {
        stack_child_onto(thread, out, |out| {
            Pattern::query_plain_cell_onto(cell, thread, state, out);
        });
        let held = out.len() - from;
        if !thread.charge(held) {
            out.truncate(from);
            return;
        }
        thread.note_accumulated(held);
    }
}

/// [`query_stack_child`] for a child that writes its haps after those of
/// `out`: the haps of a child that failed go, and those before them stay.
pub(crate) fn stack_child_onto(
    thread: &QueryThread,
    out: &mut Vec<Hap>,
    query: impl FnOnce(&mut Vec<Hap>),
) {
    if thread.error_pending() {
        return;
    }
    let from = out.len();
    query(out);
    if stack_child_failed_on(thread) {
        out.truncate(from);
    }
}

/// Whether the child of a stack that a query just asked gave no answer to
/// keep: the query stops, or the child threw. The stack contains the throw
/// and reports it.
fn stack_child_failed() -> bool {
    QUERY_THREAD.with(stack_child_failed_on)
}

fn stack_child_failed_on(thread: &QueryThread) -> bool {
    if thread.stop_requested() || thread.cancel_observed.get() {
        return true;
    }
    if !thread.error_pending() {
        return false;
    }
    let Some(message) = take_query_error() else {
        return false;
    };
    CURRENT_HOST.with(|host| {
        if let Some(pointer) = host.get() {
            // SAFETY: installed only by `with_callback_host`, which
            // outlives this call, including nested queries.
            unsafe { &*pointer }.log_query_error(&message);
        }
    });
    if QUERY_ERRORS.with(|errors| errors.boundaries.get() > 0) {
        signal_callback_failure(|| message.clone());
        signal_contained_throw(message);
    } else {
        // Raw queries have no boundary to clear a contained failure.
        // Invalidate enclosing caches without poisoning later queries.
        note_uncacheable();
    }
    true
}

// Materialise only what holds a JavaScript-owned identity. On a value that
// holds none, the walk rebuilds every container to hand back the same data,
// so the hap keeps its value as it is. The outer query boundary applies the
// same rule to the haps it returns.
fn materialize_if_js(value: &Value) -> Value {
    if value.contains_js_value() {
        materialize_js_value(value)
    } else {
        value.clone()
    }
}

// Keep callback temporaries out of the recursive query dispatcher. The child
// query finishes before entering this helper, so its frame is not multiplied
// by every node in a pattern graph.
pub(crate) fn map_js_haps(
    haps: Vec<Hap>,
    id: CallbackId,
    preserve_controls: bool,
    predicate: Option<fn(&Value) -> bool>,
) -> Vec<Hap> {
    let mut out = Vec::with_capacity(haps.len());
    for hap in haps {
        // Every iteration can enter QuickJS, and a wide query holds tens of
        // thousands of haps. Check inside the loop so callback and GC work
        // cannot keep the producer occupied after cancellation or a deadline.
        // A pending query error is deliberately NOT a stop: the turn
        // boundary turns a throwing callback into silence after every hap
        // had its turn, which the `and`/`or` lazy-truthy contracts pin.
        if query_stop_requested() {
            return Vec::new();
        }
        let mapped = hap.with_value(|v| {
            if let Some(predicate) = predicate {
                // Final event inspection consumes JS containers once, after
                // all user callbacks have run.
                let value = materialize_if_js(v);
                return if predicate(&value) {
                    materialize_if_js(&host_call_value(id, &value))
                } else {
                    value
                };
            }
            let value = host_call_value(id, v);
            if preserve_controls {
                // Trusted control rewrites consume their result here, so
                // compare native values before retaining bindings. User fmap
                // keeps opaque JS identity.
                materialize_if_js(&value)
            } else {
                value
            }
        });
        out.push(if preserve_controls {
            mapped.with_unchanged_live_controls(&hap)
        } else {
            mapped
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Hap budget.
//
// `MAX_QUERY_SPAN_CYCLES` bounds how wide one query may be. It does not bound
// how dense it is: `s("bd").ply(2000000)` covers a single cycle and still
// asks for two million haps. Nesting multiplies: `ply(1000).ply(1000)` asks
// for a million. The span guard cannot see this, because the expansion
// happens inside one cycle.
//
// The budget is charged as haps are produced, not after. A check on a finished
// `Vec<Hap>`, in the scheduler's queue or at the caller, is too late: the
// allocation it is meant to prevent has already happened.
//
// Exhaustion does not go through `signal_query_error`. That channel means
// "the query threw here", and `queryArc` turns it into an empty result. That
// is correct for a user's throwing callback and wrong for resource
// exhaustion, where an empty result looks like a pattern that produced
// nothing. Exhaustion has its own flag, which the query error boundary does
// not clear.
// ---------------------------------------------------------------------------

/// A query refused for RESOURCE reasons rather than pattern semantics.
///
/// Deliberately not a `String` and deliberately not the query-error channel:
/// this has to be impossible to mistake for "the pattern produced nothing".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryLimit {
    /// More haps than [`DEFAULT_HAP_BUDGET`] were produced.
    HapBudget { budget: u64 },
    /// The query span was wider than [`MAX_QUERY_SPAN_CYCLES`].
    ///
    /// This is a resource refusal and travels on the typed channel. On the
    /// query-error channel, the `queryArc` boundary would turn it into an
    /// empty result, which looks like a pattern that produces nothing.
    QuerySpan { cycles: i128 },
    /// The JavaScript host ran out of heap.
    ///
    /// Distinct from a user throw, which `queryArc` turns into silence. An
    /// exhausted heap is a resource refusal: reporting it as an empty query
    /// would say "this pattern produces nothing" about a pattern that was
    /// never evaluated.
    HostMemory,
    /// One setup turn queued more runnable JavaScript jobs than the host will
    /// execute. This is independent of the CPU deadline: an unbounded chain of
    /// individually cheap microtasks can stay below every per-job timing
    /// sample while still monopolising the session.
    JsJobBudget { budget: usize },
    /// A bounded synchronous score or query turn found runnable JavaScript
    /// jobs.
    ///
    /// Setup evaluation has an explicit async microtask contract; score
    /// construction and query callbacks do not. Treating this host-policy
    /// refusal as an ordinary JavaScript message would let Session's Mini
    /// compatibility fallback install a different score, or let `queryArc`
    /// turn an incomplete query into plausible silence.
    JsPendingJobs,
    /// A bounded JavaScript evaluation occupied the runtime past its CPU
    /// deadline. This is a resource refusal, not a syntax/user exception.
    JsCpuDeadline { millis: u64 },
    /// A query ran past the wall-clock deadline installed around it - the
    /// live probe's ceiling, a scheduler tick's slice - in native work, with
    /// no JavaScript involved: `fill` under `struct` under `fill` spends its
    /// time in pattern recursion. `millis` is how long the deadline allowed
    /// from the moment it was installed.
    QueryDeadline { millis: u64 },
    /// A Euclidean rhythm would allocate an oversized intermediate mask.
    EuclidSteps { steps: u64, limit: u64 },
    /// A mini-notation range would materialise too many pattern nodes.
    MiniRange { elements: u64, limit: u64 },
    /// `echoWith` would allocate and transform too many delayed copies during
    /// pattern construction, before any query hap budget exists.
    EchoCopies { copies: u64, limit: u64 },
    /// `iter`/`chunk` would materialise too many rotated copies during pattern
    /// construction.
    ///
    /// Both build a `Vec` sized directly from the caller's part count, before
    /// any query hap budget exists. An unguarded `chunk(1e9)` would request a
    /// `Vec` of 10^9 patterns, and a failed allocation aborts the process.
    /// strudel.cc throws a catchable `RangeError` here and plays on, so a
    /// refusal is also the compatible behaviour.
    IterParts {
        operation: &'static str,
        parts: u64,
        limit: u64,
    },
    /// A stepwise operation would materialise too many retained or query-split
    /// entries.
    ///
    /// `minimum_entries` is exact for ordinary finite inputs and otherwise a
    /// conservative lower bound discovered by the bounded preflight. The
    /// operation is carried structurally so callers do not have to infer which
    /// constructor was refused from an error string.
    StepwiseExpansion {
        operation: &'static str,
        minimum_entries: u64,
        limit: u64,
    },
    /// A public numeric input or exact intermediate cannot be represented by
    /// the native checked `i128/i128` Fraction layer.
    NativeFraction { operation: &'static str },
    /// A pattern graph nested deeper than the query traversal can safely
    /// recurse through.
    ///
    /// `query_node` descends one native frame per wrapper, so a source that
    /// chains enough combinators (`p = p.fast(2).slow(2)` in a loop) would
    /// overflow the stack. A Rust stack overflow aborts the process: it is
    /// not a panic, `catch_unwind` never sees it, and the session cannot
    /// recover. `Pattern::query` refuses a depth above [`MAX_PATTERN_DEPTH`]
    /// instead.
    GraphDepth { depth: u32, limit: u32 },
    /// The caller asked for the query to stop.
    ///
    /// `Pattern::query` reads the cancellation flag as it recurses, so a
    /// dense query stops between nodes instead of running to completion.
    Cancelled,
}

impl std::fmt::Display for QueryLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GraphDepth { depth, limit } => write!(
                f,
                "pattern graph nests {depth} levels deep, past the {limit} the \
                 query can traverse without overflowing the stack"
            ),
            Self::Cancelled => write!(f, "the query was cancelled"),
            Self::HostMemory => write!(
                f,
                "the JavaScript heap was exhausted and the query was refused. \
                 This is a resource limit, not an empty pattern."
            ),
            Self::JsJobBudget { budget } => write!(
                f,
                "the JavaScript setup queued more than {budget} runnable jobs and was refused"
            ),
            Self::JsPendingJobs => write!(
                f,
                "bounded synchronous JavaScript execution does not support runnable JavaScript jobs"
            ),
            Self::JsCpuDeadline { millis } => write!(
                f,
                "JavaScript evaluation exceeded its {millis} ms CPU deadline and was refused"
            ),
            Self::QueryDeadline { millis } => write!(
                f,
                "the query exceeded its {millis} ms deadline and was refused"
            ),
            Self::EuclidSteps { steps, limit } => write!(
                f,
                "the Euclidean rhythm requested {steps} steps, above the native limit of {limit}, and was refused. This is a resource limit, not an empty pattern."
            ),
            Self::MiniRange { elements, limit } => write!(
                f,
                "the mini range requested {elements} elements, above the native limit of {limit}, and was refused. This is a resource limit, not an empty pattern."
            ),
            Self::EchoCopies { copies, limit } => write!(
                f,
                "echoWith requested {copies} copies, above the native limit of {limit}, and was refused. This is a resource limit, not an empty pattern."
            ),
            Self::IterParts {
                operation,
                parts,
                limit,
            } => write!(
                f,
                "{operation} requested {parts} parts, above the native limit of {limit}, and was refused. This is a resource limit, not an empty pattern."
            ),
            Self::StepwiseExpansion {
                operation,
                minimum_entries,
                limit,
            } => write!(
                f,
                "{operation} would require at least {minimum_entries} stepwise entries, above the native limit of {limit}, and was refused. This is a resource limit, not an empty pattern."
            ),
            Self::NativeFraction { operation } => write!(
                f,
                "{operation} requires fraction arithmetic outside the native checked i128 range and was refused. This is a native representation limit, not an empty pattern."
            ),
            Self::QuerySpan { cycles } => write!(
                f,
                "the query span covers more than {cycles} cycles and was \
                 refused. This is a resource limit, not an empty pattern."
            ),
            Self::HapBudget { budget } => write!(
                f,
                "the query produced more than {budget} haps and was refused. \
                 This is a resource limit, not an empty pattern: the result \
                 would have been truncated, so nothing is returned instead."
            ),
        }
    }
}

impl std::error::Error for QueryLimit {}

/// Outcome of one outer `queryArc` boundary, distinguishing genuine silence
/// from a mid-query throw. The boundary turns a throw into no haps; live
/// reload must not treat that as a successful replacement of a sounding score.
#[derive(Debug, Clone)]
pub enum QueryArcOutcome {
    Haps(Vec<Hap>),
    Thrown(String),
}

/// Haps one top-level query may produce before it is refused.
///
/// Five million is far past any musical query - a 32-per-cycle pattern would
/// need 150,000 cycles to reach it - while bounding one query's haps to a few
/// hundred megabytes rather than all of memory.
pub const DEFAULT_HAP_BUDGET: u64 = 5_000_000;

/// Logical source/expansion high-water one stepwise scalar invocation or
/// top-level query may plan.
///
/// This counts the largest charged construction stage, not the sum of every
/// simultaneously live implementation buffer or its bytes. JavaScript rest
/// arguments, normalized sources, sidecars, and reserved output vectors can
/// overlap while remaining individually bounded by their checked plans.
pub const MAX_STEPWISE_ENTRIES: u64 = 16_384;

/// The counters a query reads on every node and every hap, in one
/// thread-local.
///
/// Where reaching a thread-local is a function call, as on macOS, every
/// separate slot is one more call per node. Repeated reads of one slot in a
/// function share one call. Each slot below is a view of one field and
/// keeps the `with` of the thread-local it replaced.
pub(crate) struct QueryThread {
    hap_budget: std::cell::Cell<Option<u64>>,
    pub(crate) depth: std::cell::Cell<u32>,
    stop: std::cell::Cell<u8>,
    peak: std::cell::Cell<u64>,
    /// Wall-clock deadline for native query work, mirroring the JS CPU
    /// deadline. QuickJS's interrupt handler only fires while JavaScript
    /// runs; a pattern like `chop(32).slow(0.001)` spends its time in
    /// native query recursion (millions of cheap spans, each below every
    /// hap budget). Without this deadline a single save could occupy the
    /// producer for minutes: the live set goes silent while the process
    /// stays alive.
    deadline: std::cell::Cell<Option<std::time::Instant>>,
    /// The time a query is allowed, when its deadline is not made yet:
    /// see [`with_query_budget`].
    budget: std::cell::Cell<Option<std::time::Duration>>,
    /// How long the installed deadline allowed, in milliseconds from its
    /// installation, so a refusal can say what it was measured against
    /// rather than reporting a zero it never had.
    deadline_millis: std::cell::Cell<u64>,
    /// Polling counter: `Instant::now()` is far too expensive per node,
    /// but the sample has to be frequent enough that a deep recursion
    /// notices the deadline in tens of milliseconds, not seconds.
    deadline_tick: std::cell::Cell<u32>,
    /// Sticky for the lifetime of the installed deadline.
    ///
    /// If the first expiry cleared the deadline, the rest of the query
    /// would have no bound: every later `Pattern::query` would read an
    /// empty slot and run at full speed. The refusal flag is the only
    /// other check, and `with_hap_budget` swaps it out at every non-nested
    /// boundary. A JS `queryArc` callback re-entry is such a boundary.
    /// Keeping expiry sticky ensures nested queries cannot escape the
    /// deadline.
    deadline_expired: std::cell::Cell<bool>,
    part_asked: std::cell::Cell<PartAsked>,
    /// Set while a cancellable query runs. Erased to a pointer because
    /// the flag is owned by the caller and shared with whatever thread
    /// will set it.
    cancel: std::cell::Cell<Option<*const std::sync::atomic::AtomicBool>>,
    cancel_observed: std::cell::Cell<bool>,
    /// Haps materialised at leaves on this thread since the last reset.
    materialised: std::cell::Cell<u64>,
    /// Pointer identity of the top `RuntimeSettings` bound on this thread, or 0.
    pub(crate) current_runtime: std::cell::Cell<usize>,
    /// Remaining stepwise construction entries in the current top-level query.
    stepwise_remaining: std::cell::Cell<Option<u64>>,
    /// Per-root capacity hint for short window queries (`(root_ptr, cap)`).
    pub(crate) short_query_hint: std::cell::Cell<(usize, u16)>,
}

impl QueryThread {
    /// Mirror one stop signal into the stop word.
    fn set_stop_bit(&self, bit: u8, on: bool) {
        let word = self.stop.get();
        self.stop.set(if on { word | bit } else { word & !bit });
    }

    /// Whether a query error is currently pending on this thread (mirrored
    /// from `QUERY_ERRORS` so hot loops avoid a second TLS lookup).
    #[inline]
    pub(crate) fn error_pending(&self) -> bool {
        self.stop.get() & STOP_ERROR_PENDING != 0
    }

    /// Whether a refusal or expired deadline has already been recorded on
    /// this thread (single byte load, no clock or tick check).
    #[inline]
    pub(crate) fn has_stopped_fast(&self) -> bool {
        self.stop.get() & (STOP_REFUSED | STOP_DEADLINE_PASSED) != 0
    }

    #[cold]
    #[inline(never)]
    fn rearm_deadline_refusal(&self) {
        refuse(QueryLimit::QueryDeadline {
            millis: self.deadline_millis.get(),
        });
    }

    /// See [`query_stop_requested`].
    #[inline]
    pub(crate) fn stop_requested(&self) -> bool {
        let word = self.stop.get();
        if word == 0 {
            return false;
        }
        if word & (STOP_REFUSED | STOP_DEADLINE_PASSED) != 0 {
            if word & STOP_REFUSED == 0 {
                self.rearm_deadline_refusal();
            }
            return true;
        }
        if word & STOP_CANCEL_ARMED != 0 && self.cancellation_requested() {
            return true;
        }
        if word & STOP_DEADLINE_ARMED != 0 {
            let before = self.deadline_tick.get();
            let after = before.wrapping_add(1);
            self.deadline_tick.set(after);
            if (after >> 8) != (before >> 8) {
                return self.deadline_check_clock();
            }
        }
        false
    }

    #[cold]
    #[inline(never)]
    fn deadline_check_clock(&self) -> bool {
        if self.deadline_expired.get() {
            self.rearm_deadline_refusal();
            return true;
        }
        let (deadline, budget) = (self.deadline.get(), self.budget.get());
        if deadline.is_none() && budget.is_none() {
            return false;
        }
        let now = std::time::Instant::now();
        let Some(deadline) = deadline else {
            self.deadline.set(budget.map(|budget| now + budget));
            return false;
        };
        if now < deadline {
            return false;
        }
        self.deadline_expired.set(true);
        self.set_stop_bit(STOP_DEADLINE_PASSED, true);
        self.rearm_deadline_refusal();
        true
    }

    /// See [`cancellation_requested`].
    fn cancellation_requested(&self) -> bool {
        // The word answers the common case: no flag installed means no other
        // thread can be asking this one to stop.
        if self.stop.get() & STOP_CANCEL_ARMED == 0 {
            return false;
        }
        let requested = match self.cancel.get() {
            None => false,
            // SAFETY: set only by `with_cancellation`, which outlives this read.
            Some(flag) => unsafe { &*flag }.load(std::sync::atomic::Ordering::Relaxed),
        };
        if requested {
            self.cancel_observed.set(true);
        }
        requested
    }

    /// See [`query_deadline_after`].
    #[inline]
    pub(crate) fn deadline_after(&self, checks: u32) -> bool {
        let word = self.stop.get();
        if word & (STOP_DEADLINE_ARMED | STOP_DEADLINE_PASSED) == 0 {
            return false;
        }
        if word & STOP_DEADLINE_PASSED != 0 {
            self.rearm_deadline_refusal();
            return true;
        }
        // The clock is read when the count passes a multiple of 256.
        let before = self.deadline_tick.get();
        let after = before.wrapping_add(checks);
        self.deadline_tick.set(after);
        let ticks = (after >> 8).wrapping_sub(before >> 8);
        if ticks == 0 {
            return false;
        }
        if self.deadline_check_clock() {
            return true;
        }
        ticks >= 2 && self.deadline_check_clock()
    }

    /// See [`charge_haps`].
    #[inline(always)]
    pub(crate) fn charge(&self, n: usize) -> bool {
        match self.hap_budget.get() {
            // No budget installed: not inside a top-level query (a unit test
            // calling `query` directly, say). Unbudgeted rather than zero-budget,
            // so nothing silently starts failing outside the boundary.
            None => true,
            Some(limit) if n as u64 > limit => {
                refuse(QueryLimit::HapBudget { budget: limit });
                false
            }
            Some(_) => true,
        }
    }

    /// Record a vector that has just grown to `len`.
    #[inline(always)]
    pub(crate) fn note_accumulated(&self, len: usize) {
        self.peak.set(self.peak.get().max(len as u64));
    }

    /// Record `n` haps materialised at a leaf without an extra TLS lookup.
    #[inline(always)]
    pub(crate) fn note_materialised(&self, n: usize) {
        self.materialised
            .set(self.materialised.get().saturating_add(n as u64));
    }

    /// [`push_budgeted_from`] on the counters of this thread: a walk that
    /// makes many haps reaches the thread one time.
    #[inline(always)]
    pub(crate) fn push_budgeted(&self, out: &mut Vec<Hap>, from: usize, hap: Hap) -> bool {
        if !self.charge(out.len() - from + 1) {
            return false;
        }
        out.push(hap);
        self.note_accumulated(out.len() - from);
        true
    }
}

pub(crate) fn set_current_runtime_ptr(ptr: usize) {
    let _ = QUERY_THREAD.try_with(|thread| thread.current_runtime.set(ptr));
}

#[inline(always)]
pub(crate) fn current_runtime_ptr() -> usize {
    QUERY_THREAD.with(|thread| thread.current_runtime.get())
}

thread_local! {
    pub(crate) static QUERY_THREAD: QueryThread = const {
        QueryThread {
            hap_budget: std::cell::Cell::new(None),
            depth: std::cell::Cell::new(0),
            stop: std::cell::Cell::new(0),
            peak: std::cell::Cell::new(0),
            deadline: std::cell::Cell::new(None),
            budget: std::cell::Cell::new(None),
            deadline_millis: std::cell::Cell::new(0),
            deadline_tick: std::cell::Cell::new(0),
            deadline_expired: std::cell::Cell::new(false),
            part_asked: std::cell::Cell::new(PartAsked::No),
            cancel: std::cell::Cell::new(None),
            cancel_observed: std::cell::Cell::new(false),
            materialised: std::cell::Cell::new(0),
            current_runtime: std::cell::Cell::new(0),
            stepwise_remaining: std::cell::Cell::new(None),
            short_query_hint: std::cell::Cell::new((0, 20)),
        }
    };
}

/// A view of one field of this thread's [`QueryThread`].
pub(crate) struct QueryCell<T: 'static>(fn(&QueryThread) -> &T);

impl<T: 'static> QueryCell<T> {
    #[inline]
    pub(crate) fn with<R>(&'static self, f: impl FnOnce(&T) -> R) -> R {
        QUERY_THREAD.with(|thread| f((self.0)(thread)))
    }
}

/// Haps still allowed in the current top-level query. `None` outside one.
pub(crate) static HAP_BUDGET: QueryCell<std::cell::Cell<Option<u64>>> =
    QueryCell(|q| &q.hap_budget);
/// How many `Pattern::query` frames are live on this thread.
///
/// Counted at the boundary rather than derived from the graph because
/// `Pattern::of` cannot see a node's children without a per-variant match,
/// and a match that must be extended for every future combinator is a
/// limit that will eventually be forgotten. One counter on the one
/// recursive entry point cannot be.
pub(crate) static QUERY_DEPTH: QueryCell<std::cell::Cell<u32>> = QueryCell(|q| &q.depth);
/// The stop signals as one word — a refusal recorded, a deadline armed
/// or past, a cancellation flag installed — so the checks a query makes
/// per hap read one thread-local instead of walking each slot. The
/// separate slots stay the source of truth; the word only mirrors them.
pub(crate) static QUERY_STOP: QueryCell<std::cell::Cell<u8>> = QueryCell(|q| &q.stop);
pub(crate) static PEAK_CHARGE: QueryCell<std::cell::Cell<u64>> = QueryCell(|q| &q.peak);

/// Whether a query on this thread asks a graph over a part of the span of
/// a restart or a reset join, which cuts the answer to that part. See
/// [`asked_over_part`].
pub(crate) static PART_ASKED: QueryCell<std::cell::Cell<PartAsked>> = QueryCell(|q| &q.part_asked);

/// The state of a query over a part of a span.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PartAsked {
    /// No query asks over a part.
    No,
    /// A query asks, and each pattern that a join made in it so far answers
    /// by its haps.
    Asked,
    /// A join of the query made a pattern that refuses the part.
    Refused,
}

/// Run `ask`, a query of a graph over a part of a span, for a join that
/// cuts the answer to the part. `None` when a join of the graph made a
/// pattern that [refuses the part](refuses_part): the answer for the part
/// is then not known to be the answer for the span, cut.
pub(crate) fn asked_over_part<R>(ask: impl FnOnce() -> R) -> Option<R> {
    /// Puts back the state of the query around this one, also on unwind.
    struct Around(PartAsked);
    impl Drop for Around {
        fn drop(&mut self) {
            PART_ASKED.with(|asked| asked.set(self.0));
        }
    }
    let around = Around(PART_ASKED.with(|asked| asked.replace(PartAsked::Asked)));
    let answer = ask();
    let refused = PART_ASKED.with(std::cell::Cell::get) == PartAsked::Refused;
    let outer = around.0;
    drop(around);
    if !refused {
        return Some(answer);
    }
    // The query that follows asks over the span of this one. A query over
    // a part around this one made that span, so it is refused too.
    if outer != PartAsked::No {
        PART_ASKED.with(|asked| asked.set(PartAsked::Refused));
    }
    None
}

/// Whether a query over a part of a span is in progress on this thread.
pub(crate) fn part_is_asked() -> bool {
    PART_ASKED.with(std::cell::Cell::get) != PartAsked::No
}

/// Whether `made`, a pattern that a join is about to ask in a query over a
/// part, refuses that query: it reads the span, or it is volatile. The join
/// then asks it nothing, so a volatile graph is asked one time, by the
/// query over the span that follows.
pub(crate) fn refuses_part(made: &Pattern) -> bool {
    let refuses = made.span_read() == purity::SpanRead::Yes || made.is_volatile();
    if refuses {
        PART_ASKED.with(|asked| asked.set(PartAsked::Refused));
    }
    refuses
}

/// Alias kept for the shrink/grow boundary.
pub const MAX_STEPWISE_SEGMENTS: u64 = MAX_STEPWISE_ENTRIES;

static STEPWISE_ENTRIES_REMAINING: QueryCell<std::cell::Cell<Option<u64>>> =
    QueryCell(|q| &q.stepwise_remaining);

thread_local! {
    /// Set when a RESOURCE limit refuses the query. Separate from
    /// `QUERY_ERROR` so the `queryArc` boundary cannot turn a refusal into a
    /// plausible silence, and typed so the caller learns which limit fired.
    pub(crate) static REFUSAL: std::cell::RefCell<Option<QueryLimit>> =
        const { std::cell::RefCell::new(None) };
    /// Entries materialised after preflight. Thread-local so parallel tests do
    /// not race.
    static STEPWISE_ENTRIES_MATERIALISED: std::cell::Cell<u64> =
        const { std::cell::Cell::new(0) };
}

/// [`QUERY_STOP`] bits: the refusal slot holds a limit, a deadline is
/// installed, that deadline has passed, a cancellation flag is installed.
pub(crate) const STOP_REFUSED: u8 = 1;
pub(crate) const STOP_DEADLINE_ARMED: u8 = 2;
pub(crate) const STOP_DEADLINE_PASSED: u8 = 4;
pub(crate) const STOP_CANCEL_ARMED: u8 = 8;
pub(crate) const STOP_ERROR_PENDING: u8 = 16;

/// Mirror one stop signal into [`QUERY_STOP`].
pub(crate) fn set_stop_bit(bit: u8, on: bool) {
    QUERY_THREAD.with(|thread| thread.set_stop_bit(bit, on));
}

/// Charge a complete planned stepwise expansion before any Pattern nodes are
/// constructed or raw inputs are reified. A top-level query is cumulative; a
/// scalar body outside one is bounded independently and carries its typed
/// refusal in the returned graph.
pub fn charge_stepwise_entries(operation: &'static str, entries: u64) -> Result<(), QueryLimit> {
    STEPWISE_ENTRIES_REMAINING.with(|remaining| match remaining.get() {
        None if entries <= MAX_STEPWISE_ENTRIES => Ok(()),
        None => Err(QueryLimit::StepwiseExpansion {
            operation,
            minimum_entries: entries,
            limit: MAX_STEPWISE_ENTRIES,
        }),
        Some(left) if entries <= left => {
            remaining.set(Some(left - entries));
            Ok(())
        }
        Some(left) => {
            let used = MAX_STEPWISE_ENTRIES - left;
            let limit = QueryLimit::StepwiseExpansion {
                operation,
                minimum_entries: used.saturating_add(entries),
                limit: MAX_STEPWISE_ENTRIES,
            };
            refuse(limit.clone());
            Err(limit)
        }
    })
}

/// Progressive-segment spelling retained for the core combinators.
pub(crate) fn charge_stepwise_segments(segments: u64) -> Result<(), QueryLimit> {
    charge_stepwise_entries("shrink/grow", segments)
}

/// Publish a non-expansion stepwise refusal immediately when a patterned body
/// is running inside a query, while leaving scalar construction to carry the
/// limit in its returned graph.
pub fn mark_stepwise_refusal(limit: QueryLimit) -> QueryLimit {
    STEPWISE_ENTRIES_REMAINING.with(|remaining| {
        if remaining.get().is_some() {
            refuse(limit.clone());
        }
    });
    limit
}

/// Run `f` apart from the query around it, for work whose result outlives that
/// query. A query error or a resource refusal raised inside makes the result
/// `None`, and the enclosing query sees neither.
pub(crate) fn without_query_signals<T>(f: impl FnOnce() -> Option<T>) -> Option<T> {
    /// Holds the enclosing query's refusal and puts it back, also on unwind.
    struct RefusalScope {
        outer: Option<QueryLimit>,
        finished: bool,
    }
    impl RefusalScope {
        fn enter() -> Self {
            let outer = REFUSAL.with(|slot| slot.borrow_mut().take());
            set_stop_bit(STOP_REFUSED, outer.is_some());
            Self {
                outer,
                finished: false,
            }
        }
        /// Whether a refusal was raised inside.
        fn finish(mut self) -> bool {
            self.finished = true;
            let outer = self.outer.take();
            let outer_refused = outer.is_some();
            let inner = REFUSAL.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), outer));
            set_stop_bit(STOP_REFUSED, outer_refused);
            inner.is_some()
        }
    }
    impl Drop for RefusalScope {
        fn drop(&mut self) {
            if !self.finished {
                let outer = self.outer.take();
                set_stop_bit(STOP_REFUSED, outer.is_some());
                REFUSAL.with(|slot| *slot.borrow_mut() = outer);
            }
        }
    }
    let boundary = QueryErrorBoundary::enter();
    let refusal = RefusalScope::enter();
    let result = f();
    let refused = refusal.finish();
    let failed = boundary.discard();
    if refused || failed {
        return None;
    }
    result
}

/// One time map of [`Node::Fast`] or [`Node::Shift`]. A result outside the
/// native fraction range marks the `NativeFraction` refusal for `operation`
/// and becomes time zero until the query boundary reports the refusal.
pub(crate) fn checked_time(time: Option<Fraction>, operation: &'static str) -> Fraction {
    time.unwrap_or_else(|| {
        mark_stepwise_refusal(QueryLimit::NativeFraction { operation });
        Fraction::ZERO
    })
}

pub(crate) fn fast_query_time(
    time: Fraction,
    factor: Fraction,
    operation: &'static str,
) -> Fraction {
    checked_time(time.checked_mul(factor), operation)
}

pub(crate) fn fast_hap_time(time: Fraction, factor: Fraction, operation: &'static str) -> Fraction {
    checked_time(time.checked_div(factor), operation)
}

pub(crate) fn shift_query_time(time: Fraction, by: Fraction, operation: &'static str) -> Fraction {
    checked_time(time.checked_sub(by), operation)
}

pub(crate) fn shift_hap_time(time: Fraction, by: Fraction, operation: &'static str) -> Fraction {
    checked_time(time.checked_add(by), operation)
}

pub fn note_stepwise_entries_materialised(entries: u64) {
    STEPWISE_ENTRIES_MATERIALISED.with(|count| {
        count.set(count.get().saturating_add(entries));
    });
}

pub(crate) fn note_stepwise_segments_materialised(segments: u64) {
    note_stepwise_entries_materialised(segments);
}

/// Completed expansion entries reported by stepwise constructors on this
/// thread since the last reset. This is a mutation-sensitive construction
/// counter, not an allocator or total-resident-memory measurement.
pub fn stepwise_entries_materialised() -> u64 {
    STEPWISE_ENTRIES_MATERIALISED.with(std::cell::Cell::get)
}

/// Getter kept for the progressive-segment tests.
pub fn stepwise_segments_materialised() -> u64 {
    stepwise_entries_materialised()
}

pub fn reset_stepwise_entries_materialised() {
    STEPWISE_ENTRIES_MATERIALISED.with(|count| count.set(0));
}

/// Reset kept for the progressive-segment tests.
pub fn reset_stepwise_segments_materialised() {
    reset_stepwise_entries_materialised();
}

/// Check `n` haps against the budget, returning `false` if it is exceeded.
///
/// This is a high-water bound (the largest vector any single step may hold),
/// not a running total. `query` charges every node's result, and a hap
/// produced at a leaf passes up through each of its ancestors. A running
/// total would count the same haps once per level: a 64-hap pattern four
/// levels deep would be charged 256. That measures tree depth, not size.
///
/// The high-water bound also matches the purpose: the concern is a single
/// oversized `Vec<Hap>`, and sibling vectors are bounded by depth times the
/// budget.
///
/// Callers must check the result and stop producing; the flag makes the failure
/// visible at the boundary either way.
pub(crate) fn charge_haps(n: usize) -> bool {
    QUERY_THREAD.with(|thread| thread.charge(n))
}

/// The hap budget of the current top-level query, or `None` outside one.
pub(crate) fn hap_budget() -> Option<u64> {
    HAP_BUDGET.with(std::cell::Cell::get)
}

/// Push into an accumulating result, refusing before the budget is exceeded.
///
/// Returns `false` when the caller must STOP producing. The generic charge in
/// `query` sees a node's result only once it is finished, which is too late for
/// an AMPLIFIER: a join's output is the product of its inputs, so two legal
/// operands can multiply into something enormous inside one loop. Charging per
/// push bounds the vector as it grows rather than inspecting it after completion.
pub(crate) fn push_budgeted(out: &mut Vec<Hap>, hap: Hap) -> bool {
    push_budgeted_from(out, 0, hap)
}

/// [`push_budgeted`] into a list that held `from` haps of other nodes
/// before this one wrote: the bound is on the haps of this node, as it is
/// Push `hap` onto `out`, which held `from` haps before this node wrote.
pub(crate) fn push_budgeted_from(out: &mut Vec<Hap>, from: usize, hap: Hap) -> bool {
    QUERY_THREAD.with(|thread| thread.push_budgeted(out, from, hap))
}

/// Keep the haps of `out` from `from` on that `keep` takes, in their order.
/// `keep` may change a hap that it takes.
pub(crate) fn retain_from(out: &mut Vec<Hap>, from: usize, mut keep: impl FnMut(&mut Hap) -> bool) {
    let mut kept = from;
    for at in from..out.len() {
        if keep(&mut out[at]) {
            if kept != at {
                out.swap(kept, at);
            }
            kept += 1;
        }
    }
    out.truncate(kept);
}

thread_local! {
    /// The empty lists that the walks of this thread gave back, each with
    /// the room it grew to.
    static SPARE_LISTS: std::cell::RefCell<Vec<Vec<Hap>>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Empty `ResolvedHap` buffers recycled across join queries on this thread.
    static SPARE_RESOLVED: std::cell::RefCell<Vec<Vec<ResolvedHap>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

pub(crate) fn take_resolved_list() -> Vec<ResolvedHap> {
    SPARE_RESOLVED
        .try_with(|spare| spare.borrow_mut().pop())
        .ok()
        .flatten()
        .unwrap_or_default()
}

pub(crate) fn put_resolved_list(mut list: Vec<ResolvedHap>) {
    const MOST_LISTS: usize = 4;
    const MOST_ROOM: usize = 16;
    list.clear();
    if list.capacity() <= MOST_ROOM {
        let _ = SPARE_RESOLVED.try_with(|spare| {
            let mut spare = spare.borrow_mut();
            if spare.len() < MOST_LISTS {
                spare.push(list);
            }
        });
    }
}

/// Run `work` with an empty list, for the haps that a node reads from the
/// node under it and makes its own haps of. The list goes back to the
/// thread after the work with its room, so the next node takes it and
/// allocates nothing.
pub(crate) fn with_list<R>(work: impl FnOnce(&mut Vec<Hap>) -> R) -> R {
    /// The most lists that a thread keeps, and the most haps that a kept
    /// list has room for.
    const MOST_LISTS: usize = 8;
    const MOST_ROOM: usize = 64;
    // The lists are gone when the thread ends.
    let spare = SPARE_LISTS.try_with(|spare| spare.borrow_mut().pop());
    let mut list = spare
        .ok()
        .flatten()
        .unwrap_or_else(|| Vec::with_capacity(16));
    let result = work(&mut list);
    list.clear();
    if list.capacity() > 0 && list.capacity() <= MOST_ROOM {
        let _ = SPARE_LISTS.try_with(|spare| {
            let mut spare = spare.borrow_mut();
            if spare.len() < MOST_LISTS {
                spare.push(list);
            }
        });
    }
    result
}

/// Each hap of `out` from place `from` on through `map`, which takes the
/// hap: the hap that `map` gives takes a place in the list, in the order of
/// the haps, and a hap that `map` drops leaves the list.
pub(crate) fn map_from(out: &mut Vec<Hap>, from: usize, mut map: impl FnMut(Hap) -> Option<Hap>) {
    let len = out.len();
    if from >= len {
        return;
    }
    // SAFETY: the list forgets the haps from `from` on before any is read,
    // so each is read one time and none is dropped by the list: should
    // `map` unwind, the haps that it did not take leak. A hap is written at
    // or before the place of the hap that was read last, inside the room of
    // the list, and the list then holds the haps written so far.
    unsafe {
        out.set_len(from);
        let base = out.as_mut_ptr();
        let mut kept = from;
        for at in from..len {
            if let Some(mapped) = map(std::ptr::read(base.add(at))) {
                std::ptr::write(base.add(kept), mapped);
                kept += 1;
                out.set_len(kept);
            }
        }
    }
}

/// Put `haps` after those of `out`. An empty list with no room takes the
/// list as it is, with no copy of a hap.
pub(crate) fn hand_over(mut haps: Vec<Hap>, out: &mut Vec<Hap>) {
    match out.capacity() {
        0 => *out = haps,
        _ => out.append(&mut haps),
    }
}

/// Concatenate per-branch results, refusing before the budget is exceeded.
///
/// The `flat_map(...).collect()` producers (`stack`, `splitQueries`,
/// `chooseCycles`, `slowcat`) grow by concatenation, so each branch can be
/// within the budget while the total is not: two 100-hap siblings make 200
/// under a budget of 100. Extending under the bound stops at the first
/// branch that would exceed it.
///
/// Returns `None` once the budget is spent, so callers propagate rather than
/// truncate.
pub(crate) fn extend_budgeted<T>(branches: impl IntoIterator<Item = Vec<T>>) -> Option<Vec<T>> {
    let mut branches = branches.into_iter();
    // The first branch is the answer so far, where it is.
    let mut out: Vec<T> = branches.next().unwrap_or_default();
    if !charge_haps(out.len()) {
        return None;
    }
    note_accumulated(out.len());
    for branch in branches {
        if !charge_haps(out.len() + branch.len()) {
            return None;
        }
        out.extend(branch);
        note_accumulated(out.len());
    }
    Some(out)
}

/// Report that the JavaScript host exhausted its heap.
///
/// Uses the resource channel rather than `signal_query_error`. The latter
/// means "the query threw here", which `queryArc` turns into an empty
/// result, and an out-of-memory callback must not be reported as silence.
///
/// L2 calls this when its budget-aware allocator structurally denies a request;
/// no exception text participates in the classification.
pub fn refuse_host_memory() {
    refuse(QueryLimit::HostMemory);
}

/// Report that synchronous JavaScript crossed its elapsed-time interrupt
/// deadline.
///
/// The JavaScript bridge calls this while a core query boundary is active so
/// callback-driven patterns cannot turn an interrupt exception into ordinary
/// `queryArc` silence. Like every structural refusal, the first limit raised
/// by the query wins. The stable public variant is named `JsCpuDeadline`, but
/// this mechanism is not process CPU-time accounting.
pub fn refuse_js_cpu_deadline(millis: u64) {
    refuse(QueryLimit::JsCpuDeadline { millis });
}

/// Report that a synchronous JavaScript query left runnable jobs behind.
///
/// Query-time JavaScript has no async-job contract. This therefore travels on
/// the typed resource channel rather than the query-error channel, where it
/// would be indistinguishable from a genuinely silent pattern.
pub fn refuse_js_pending_jobs() {
    refuse(QueryLimit::JsPendingJobs);
}

/// Refuse an oversized Euclidean mask through the typed resource channel.
pub fn refuse_euclid_steps(steps: u64, limit: u64) {
    refuse(QueryLimit::EuclidSteps { steps, limit });
}

/// Refuse a native-fraction overflow through the typed resource channel.
/// Extension nodes use this where their span shifts leave the range an
/// `i128` numerator can hold.
pub fn refuse_native_fraction(operation: &'static str) {
    refuse(QueryLimit::NativeFraction { operation });
}

/// Charge a growing collection outside this crate, by its current length.
///
/// `charge_haps` is a high-water bound on a single vector, so the caller
/// must pass the length the vector is about to reach. A constant 1 never
/// exceeds a positive budget and would accept unlimited pushes.
///
/// Returns `false` when the caller must stop.
pub fn charge_hap_at(len_after_push: usize) -> bool {
    charge_haps(len_after_push)
}

/// Whether the current query has already exceeded its budget.
///
/// The expired deadline counts even when no refusal is recorded: every
/// `with_hap_budget` boundary swaps the refusal out, and a JS `queryArc`
/// callback crosses one on every re-entry. Without this, a deep pattern
/// keeps making re-entries after its deadline. Each one is cheap, but tens
/// of thousands of them cost seconds on the producer thread.
pub(crate) fn budget_exhausted() -> bool {
    QUERY_STOP.with(|slot| slot.get()) & (STOP_REFUSED | STOP_DEADLINE_PASSED) != 0
}

/// All three stop signals in one thread-local read: a recorded refusal, a
/// past deadline, an installed cancellation flag. The word cannot answer
/// alone for the armed cases — a deadline must still sample the clock, and
/// a cancellation flag belongs to another thread — so those fall back to
/// their full checks.
pub(crate) fn query_stop_requested() -> bool {
    QUERY_THREAD.with(QueryThread::stop_requested)
}

/// Record a resource refusal. The first one wins, as with query errors.
pub(crate) fn refuse(limit: QueryLimit) {
    REFUSAL.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(limit);
            set_stop_bit(STOP_REFUSED, true);
        }
    });
}

/// The largest hap vector that actually EXISTED since the last reset.
///
/// Distinct from [`haps_materialised`], which counts leaf production, and from
/// the amount charged, which records an intended size rather than a realised
/// one. This is the only observable that separates "refused while
/// concatenating" from "concatenated everything, then refused": both end in the
/// same error, and the leaves are identical in both, so nothing else can tell
/// them apart.
pub fn peak_hap_vector() -> u64 {
    PEAK_CHARGE.with(|c| c.get())
}

/// Record a vector that has just grown to `len`.
pub(crate) fn note_accumulated(len: usize) {
    QUERY_THREAD.with(|thread| thread.note_accumulated(len));
}

/// Run `f` with `flag` as the cancellation signal for any query it performs.
///
/// The flag is read by `query` as it recurses, so a long query stops between
/// nodes rather than at the end. Another thread sets it - a signal watcher, a
/// UI, a scheduler - which is why this takes a shared atomic rather than a
/// thread-local.
///
/// # Safety
/// The pointer never escapes: it is cleared before returning, including on
/// unwind, so it cannot be read after `flag` could die.
pub fn with_cancellation<R>(flag: &std::sync::atomic::AtomicBool, f: impl FnOnce() -> R) -> R {
    /// Puts back the flag and the observation of the scope around this
    /// one, also on unwind.
    struct Around(Option<*const std::sync::atomic::AtomicBool>, bool);
    impl Drop for Around {
        fn drop(&mut self) {
            QUERY_THREAD.with(|thread| {
                thread.set_stop_bit(STOP_CANCEL_ARMED, self.0.is_some());
                thread.cancel.set(self.0);
                thread.cancel_observed.set(self.1);
            });
        }
    }
    let _around = QUERY_THREAD.with(|thread| {
        let previous = thread.cancel.replace(Some(flag as *const _));
        thread.set_stop_bit(STOP_CANCEL_ARMED, true);
        // A fresh scope starts with nothing observed; the enclosing
        // scope's observation is restored when it ends.
        Around(previous, thread.cancel_observed.replace(false))
    });
    f()
}

/// Run `f` with a native query deadline installed (restored afterwards).
pub fn with_query_deadline<T>(deadline: std::time::Instant, f: impl FnOnce() -> T) -> T {
    with_query_deadline_from(std::time::Instant::now(), deadline, f)
}

/// [`with_query_deadline`] for a caller that made `deadline` from a read of
/// the clock at `now`, so the clock is read one time.
pub fn with_query_deadline_from<T>(
    now: std::time::Instant,
    deadline: std::time::Instant,
    f: impl FnOnce() -> T,
) -> T {
    let _around = QUERY_THREAD.with(|thread| {
        // A budget around this deadline starts here at the latest.
        if thread.deadline.get().is_none()
            && let Some(budget) = thread.budget.get()
        {
            thread.deadline.set(Some(now + budget));
        }
        // A NESTED install must never buy more time than the deadline that
        // contains it: the live probe's 150 ms ceiling was being replaced
        // by the 2 s JS budget installed for a callback inside it, so
        // nothing bounded the probe at all.
        let previous = thread.deadline.get();
        let effective = match previous {
            Some(outer) => outer.min(deadline),
            None => deadline,
        };
        thread.deadline.set(Some(effective));
        let allowed = effective
            .saturating_duration_since(now)
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        let millis = thread.deadline_millis.replace(allowed);
        thread.set_stop_bit(STOP_DEADLINE_ARMED, true);
        // A nested scope starts un-expired and re-detects within its own
        // sampling interval; the enclosing scope's verdict is restored
        // when it ends.
        let expired = thread.deadline_expired.replace(false);
        thread.set_stop_bit(STOP_DEADLINE_PASSED, false);
        thread.deadline_tick.set(0);
        DeadlineAround {
            deadline: previous,
            budget: thread.budget.get(),
            millis,
            expired,
        }
    });
    f()
}

/// Puts back the deadline, the budget and the verdict of the scope around
/// a deadline or a budget, also on unwind.
pub(crate) struct DeadlineAround {
    deadline: Option<std::time::Instant>,
    budget: Option<std::time::Duration>,
    millis: u64,
    expired: bool,
}

impl Drop for DeadlineAround {
    fn drop(&mut self) {
        QUERY_THREAD.with(|thread| {
            let armed = self.deadline.is_some() || self.budget.is_some();
            thread.set_stop_bit(STOP_DEADLINE_ARMED, armed);
            thread.deadline.set(self.deadline);
            thread.budget.set(self.budget);
            thread.deadline_millis.set(self.millis);
            thread.set_stop_bit(STOP_DEADLINE_PASSED, self.expired);
            thread.deadline_expired.set(self.expired);
        });
    }
}

/// Run `f` with a native query deadline of `budget` from now, with no read
/// of the clock here. The query reads the clock at its first sample, and
/// its deadline is `budget` from that read, so a query of a few nodes reads
/// no clock at all. Inside another deadline or budget this is
/// [`with_query_deadline`] of `budget` from now.
pub fn with_query_budget<T>(budget: std::time::Duration, f: impl FnOnce() -> T) -> T {
    let around = QUERY_THREAD.with(|thread| {
        if thread.deadline.get().is_some() || thread.budget.get().is_some() {
            return None;
        }
        thread.budget.set(Some(budget));
        let allowed = budget.as_millis().min(u128::from(u64::MAX)) as u64;
        let millis = thread.deadline_millis.replace(allowed);
        thread.set_stop_bit(STOP_DEADLINE_ARMED, true);
        let expired = thread.deadline_expired.replace(false);
        thread.set_stop_bit(STOP_DEADLINE_PASSED, false);
        thread.deadline_tick.set(0);
        Some(DeadlineAround {
            deadline: None,
            budget: None,
            millis,
            expired,
        })
    });
    match around {
        Some(_around) => f(),
        None => {
            let now = std::time::Instant::now();
            with_query_deadline_from(now, now + budget, f)
        }
    }
}

/// Run `f` with both a cancellation flag and a native query deadline budget
/// installed in a single thread-local scope when no outer deadline is active.
pub fn with_cancellation_and_query_budget<T>(
    flag: &std::sync::atomic::AtomicBool,
    budget: std::time::Duration,
    f: impl FnOnce() -> T,
) -> T {
    struct CombinedAround {
        cancel: Option<*const std::sync::atomic::AtomicBool>,
        cancel_observed: bool,
        millis: u64,
        expired: bool,
    }
    impl Drop for CombinedAround {
        fn drop(&mut self) {
            QUERY_THREAD.with(|thread| {
                thread.set_stop_bit(STOP_CANCEL_ARMED, self.cancel.is_some());
                thread.cancel.set(self.cancel);
                thread.cancel_observed.set(self.cancel_observed);
                thread.set_stop_bit(STOP_DEADLINE_ARMED, false);
                thread.deadline.set(None);
                thread.budget.set(None);
                thread.deadline_millis.set(self.millis);
                thread.set_stop_bit(STOP_DEADLINE_PASSED, self.expired);
                thread.deadline_expired.set(self.expired);
            });
        }
    }
    let combined = QUERY_THREAD.with(|thread| {
        if thread.deadline.get().is_some() || thread.budget.get().is_some() {
            return None;
        }
        let previous_cancel = thread.cancel.replace(Some(flag as *const _));
        thread.set_stop_bit(STOP_CANCEL_ARMED, true);
        let cancel_observed = thread.cancel_observed.replace(false);
        thread.budget.set(Some(budget));
        let allowed = budget.as_millis().min(u128::from(u64::MAX)) as u64;
        let millis = thread.deadline_millis.replace(allowed);
        thread.set_stop_bit(STOP_DEADLINE_ARMED, true);
        let expired = thread.deadline_expired.replace(false);
        thread.set_stop_bit(STOP_DEADLINE_PASSED, false);
        thread.deadline_tick.set(0);
        Some(CombinedAround {
            cancel: previous_cancel,
            cancel_observed,
            millis,
            expired,
        })
    });
    match combined {
        Some(_around) => f(),
        None => with_cancellation(flag, || with_query_budget(budget, f)),
    }
}

/// Whether the installed native query deadline has passed. Sampled every
/// 64th call so the clock read stays off the hot path.
pub(crate) fn query_deadline_expired() -> bool {
    query_deadline_after(1)
}

/// [`query_deadline_expired`] for `checks` checks at once. A table answer
/// advances the sampling by the haps it made, as the graph advances it by
/// the leaves it visits, so a deadline is seen after the same amount of
/// work.
pub(crate) fn query_deadline_after(checks: u32) -> bool {
    QUERY_THREAD.with(|thread| thread.deadline_after(checks))
}

/// How long the deadline installed around the current query allowed, in
/// milliseconds from its installation; `None` outside any deadline.
pub fn installed_query_deadline_millis() -> Option<u64> {
    QUERY_THREAD.with(|thread| {
        (thread.deadline.get().is_some() || thread.budget.get().is_some())
            .then(|| thread.deadline_millis.get())
    })
}

/// Whether the current query has been asked to stop.
pub(crate) fn cancellation_requested() -> bool {
    QUERY_THREAD.with(QueryThread::cancellation_requested)
}

/// Whether a cancellation has been observed inside the innermost
/// `with_cancellation` scope. The shared flag belongs to another thread
/// and may be cleared again at any moment, so a result cache asks this
/// rather than the flag: a window cut short by a stop stays cut short.
static CANCELLATION_OBSERVED: QueryCell<std::cell::Cell<bool>> = QueryCell(|q| &q.cancel_observed);

/// Whether a cancellation was observed inside the innermost cancellable
/// scope, whatever the shared flag says now.
pub(crate) fn cancellation_observed() -> bool {
    CANCELLATION_OBSERVED.with(|observed| observed.get())
}

/// Haps materialised on this thread since [`reset_haps_materialised`].
///
/// Tests use this to distinguish "refused" from "refused after building the
/// haps". Both return the same result, so the return value cannot tell them
/// apart. The budget is charged before production so that the allocation
/// does not happen. Without this counter the pre-charge could be deleted
/// and every budget test would still pass.
pub fn haps_materialised() -> u64 {
    QUERY_THREAD.with(|t| t.materialised.get())
}

/// Reset the materialisation counter.
pub fn reset_haps_materialised() {
    QUERY_THREAD.with(|t| {
        t.materialised.set(0);
        t.peak.set(0);
    });
}

pub(crate) fn note_materialised(n: usize) {
    QUERY_THREAD.with(|t| t.note_materialised(n));
}

/// Install a budget for `f`, restoring the previous one afterwards.
///
/// Nested top-level queries (a callback querying another pattern) share the
/// outer budget rather than getting a fresh one. Otherwise a pattern could
/// evade the limit by querying in a loop.
pub(crate) fn with_hap_budget<R>(budget: u64, f: impl FnOnce() -> R) -> (R, Option<QueryLimit>) {
    let (previous, nested, previous_stepwise, had_refusal) = QUERY_THREAD.with(|thread| {
        let previous = thread.hap_budget.get();
        let nested = previous.is_some();
        let previous_stepwise = thread.stepwise_remaining.get();
        let had_refusal = !nested && (thread.stop.get() & STOP_REFUSED != 0);
        if !nested {
            thread.hap_budget.set(Some(budget));
            thread.stepwise_remaining.set(Some(MAX_STEPWISE_ENTRIES));
            if had_refusal {
                thread.set_stop_bit(STOP_REFUSED, false);
            }
        }
        (previous, nested, previous_stepwise, had_refusal)
    });
    let previous_refusal = if had_refusal {
        REFUSAL.with(|f| f.borrow_mut().take())
    } else {
        None
    };
    struct Guard {
        previous_budget: Option<u64>,
        previous_refusal: Option<QueryLimit>,
        previous_stepwise: Option<u64>,
        nested: bool,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            let previous = self.previous_refusal.take();
            let restore_refusal = !self.nested && previous.is_some();
            let mut touch_refusal = restore_refusal;
            QUERY_THREAD.with(|thread| {
                thread.hap_budget.set(self.previous_budget);
                if !self.nested {
                    thread.stepwise_remaining.set(self.previous_stepwise);
                    let had_inner = thread.stop.get() & STOP_REFUSED != 0;
                    if restore_refusal || had_inner {
                        thread.set_stop_bit(STOP_REFUSED, restore_refusal);
                        touch_refusal = true;
                    }
                }
            });
            if touch_refusal {
                REFUSAL.with(|slot| *slot.borrow_mut() = previous);
            }
        }
    }
    let guard = Guard {
        previous_budget: previous,
        previous_refusal,
        previous_stepwise,
        nested,
    };
    let out = f();
    let (had_refusal_out, touch_refusal, previous) = {
        let mut guard = guard;
        let previous = guard.previous_refusal.take();
        let restore_refusal = !guard.nested && previous.is_some();
        let mut had_inner = false;
        QUERY_THREAD.with(|thread| {
            had_inner = thread.stop.get() & STOP_REFUSED != 0;
            thread.hap_budget.set(guard.previous_budget);
            if !guard.nested {
                thread.stepwise_remaining.set(guard.previous_stepwise);
                if restore_refusal || had_inner {
                    thread.set_stop_bit(STOP_REFUSED, restore_refusal);
                }
            }
        });
        // `previous_refusal` was taken and thread state is already restored.
        std::mem::forget(guard);
        (
            had_inner,
            restore_refusal || (!nested && had_inner),
            previous,
        )
    };
    let refusal = if had_refusal_out {
        REFUSAL.with(|f| f.borrow().clone())
    } else {
        None
    };
    if touch_refusal {
        REFUSAL.with(|slot| *slot.borrow_mut() = previous);
    }
    (out, refusal)
}
