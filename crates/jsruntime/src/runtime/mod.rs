use super::*;

// ---------------------------------------------------------------------------
// The runtime
// ---------------------------------------------------------------------------

/// The QuickJS heap ceiling.
///
/// The hap budget bounds what Rust converts. It cannot bound what JavaScript
/// allocated before returning, because the array already exists by then. A
/// callback doing `Array.from({length: 1e9})` takes that memory inside QuickJS
/// with nothing on the Rust side able to intervene.
///
/// The allocator installed at runtime construction enforces this: an allocation
/// past the ceiling fails, and QuickJS raises an ordinary JavaScript error
/// rather than aborting. The refusal is deterministic and catchable, and the
/// process stays alive.
///
/// 512 MiB leaves ample room for ordinary scores while refusing pathological
/// allocations before a desktop begins swapping.
///
/// A browser tab has its own heap ceiling and behavior; this limit applies to
/// the embedded runtime only.
pub const DEFAULT_JS_MEMORY_LIMIT: usize = 512 * 1024 * 1024;

/// The C stack QuickJS may spend before it refuses a call.
///
/// QuickJS records one stack pointer, in `JS_NewRuntime`, and refuses any call
/// made more than this far below it. rquickjs re-records that pointer only
/// under its `parallel` feature, which this build does not enable, so the mark
/// stays where the runtime was constructed and every native `query_node` frame
/// beneath the query boundary is charged against the budget. An unoptimised
/// `query_node` frame is about 49 KiB, so QuickJS's own 1 MiB default refused
/// the callback of a score only five ordinary layers deep with `RangeError:
/// Maximum call stack size exceeded`, and the outer boundary turned that into
/// silence. Nothing had re-entered anything: one callback, called once.
///
/// The size is the graph core already admits. `MAX_PATTERN_DEPTH` of 512 debug
/// frames is roughly 24 MiB, and a callback reached at a depth the graph
/// allows has to be callable, or the two bounds contradict each other.
///
/// This is a budget, not a guarantee: QuickJS refuses a call once it is this
/// far below the anchor, so a thread whose real stack is SMALLER overflows for
/// real before the refusal can fire. Every thread that builds a runtime and
/// then queries must therefore have more stack than this. `rustel-runtime`
/// spawns those threads with `QUERY_WORKER_STACK_BYTES` and asserts the
/// relation at compile time.
pub const MAX_JS_STACK_BYTES: usize = 32 * 1024 * 1024;

pub(super) const MAX_SCORE_SAMPLE_EFFECTS: usize = 64;
pub(super) const MAX_SCORE_SAMPLE_EFFECT_BYTES: usize = 4 * 1024 * 1024;
pub(super) const MAX_PRELOAD_EFFECTS: usize = 256;
pub(super) const MAX_PRELOAD_EFFECT_BYTES: usize = 256 * 1024;
pub(super) const MAX_BUFFERED_LOGS: usize = 1024;

/// Maximum number of QuickJS jobs one setup turn may execute.
///
/// The CPU deadline bounds time spent inside JavaScript, but a program can
/// enqueue an unbounded sequence of individually cheap microtasks. Counting
/// engine jobs gives that queue churn its own deterministic bound. The limit
/// is deliberately about runnable QuickJS work; it does not claim support for
/// promises whose progress depends on host I/O.
pub const MAX_PREBAKE_JOBS: usize = 16_384;

/// Default elapsed-time ceiling for synchronous JavaScript reached through
/// the source-compatible query entry points.
///
/// Callers that need a different finite policy should use the corresponding
/// cancellable API. Pure Rust traversal and sorting are not covered by this
/// QuickJS interrupt deadline.
pub const DEFAULT_QUERY_JS_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

pub(super) static DEFAULT_QUERY_CANCELLATION: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// One score/setup `samples(map, base?)` call: the map as JSON, and the base
/// URL when the source gave one.
pub type SamplesEffects = Vec<(String, Option<String>)>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct EffectPolicy(u8);

impl EffectPolicy {
    pub(super) const NONE: Self = Self(0);
    pub(super) const SAMPLES: u8 = 1 << 0;
    pub(super) const PRELOAD: u8 = 1 << 1;
    pub(super) const TEMPO: u8 = 1 << 2;
    pub(super) const MIDI_INPUT: u8 = 1 << 3;
    /// Opening a visuals window and recording what it draws. Score-only: a
    /// setup file runs before there is anything to look at, and a query-time
    /// callback must not be able to move a window on another screen.
    #[cfg(feature = "hydra")]
    pub(super) const HYDRA: u8 = 1 << 4;
    pub(super) const GAMEPAD: u8 = 1 << 5;
    pub(super) const SETUP: Self = Self(Self::SAMPLES | Self::PRELOAD);
    #[cfg(not(feature = "hydra"))]
    pub(super) const SCORE: Self =
        Self(Self::SAMPLES | Self::PRELOAD | Self::TEMPO | Self::MIDI_INPUT | Self::GAMEPAD);
    #[cfg(feature = "hydra")]
    pub(super) const SCORE: Self = Self(
        Self::SAMPLES
            | Self::PRELOAD
            | Self::TEMPO
            | Self::MIDI_INPUT
            | Self::HYDRA
            | Self::GAMEPAD,
    );

    pub(super) fn allows(self, effect: u8) -> bool {
        self.0 & effect != 0
    }
}

#[derive(Default)]
pub(super) struct MidiInputHandles {
    /// Never reuse a JavaScript-visible handle. Old closures may remain rooted
    /// across an evaluation even after their input is no longer active.
    pub(super) next_handle: u64,
    pub(super) ports: BTreeMap<u64, std::sync::Arc<rustel_core::midi_in::InputPort>>,
    /// Complete selector -> handle set of the last committed score.
    pub(super) active: BTreeMap<String, u64>,
    /// Handles allocated by successful JavaScript evaluation but not yet
    /// committed by Session.
    pub(super) provisional: BTreeSet<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MidiInputBinding {
    pub(super) selector: String,
    pub(super) handle: u64,
}

pub(super) struct MidiInputCandidate {
    pub(super) bindings: Vec<MidiInputBinding>,
    pub(super) provisional_handles: Vec<u64>,
    pub(super) handles: std::rc::Weak<RefCell<MidiInputHandles>>,
    pub(super) resolved: bool,
}

/// Runtime policy for a callback-IR candidate whose original JavaScript
/// callable is still retained as the compatibility fallback.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PatternTransformIrMode {
    /// Execute a proven native candidate.
    #[default]
    Auto,
    /// Force the original QuickJS callback for same-machine comparisons.
    Compatibility,
    /// Execute both paths, require the exact same graph handle, and return the
    /// compatibility result.
    DualRun,
}

/// Producer-side evidence that the callback-IR tier actually ran.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PatternTransformIrStats {
    pub calls: u64,
    pub native_executions: u64,
    pub compatibility_executions: u64,
    pub dual_runs: u64,
    pub mismatches: u64,
}

impl Default for MidiInputCandidate {
    fn default() -> Self {
        Self {
            bindings: Vec::new(),
            provisional_handles: Vec::new(),
            handles: std::rc::Weak::new(),
            resolved: false,
        }
    }
}

impl std::fmt::Debug for MidiInputCandidate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MidiInputCandidate")
            .field("bindings", &self.bindings)
            .field("provisional_handles", &self.provisional_handles)
            .field("resolved", &self.resolved)
            .finish_non_exhaustive()
    }
}

impl PartialEq for MidiInputCandidate {
    fn eq(&self, other: &Self) -> bool {
        self.bindings == other.bindings
            && self.provisional_handles == other.provisional_handles
            && self.resolved == other.resolved
    }
}

impl Drop for MidiInputCandidate {
    fn drop(&mut self) {
        if self.resolved || self.provisional_handles.is_empty() {
            return;
        }
        let Some(handles) = self.handles.upgrade() else {
            return;
        };
        let mut handles = handles.borrow_mut();
        for handle in &self.provisional_handles {
            handles.provisional.remove(handle);
            if !handles.active.values().any(|active| active == handle) {
                handles.ports.remove(handle);
            }
        }
    }
}

pub struct JsRuntime {
    pub(super) rt: Runtime,
    pub(super) ctx: Context,
    /// Native module settings paired with this QuickJS realm.
    pub(super) core_settings: rustel_core::settings::RuntimeSettings,
    /// The single per-runtime callback id allocator, shared by
    /// `GraphBuilder::callback` and the host's bridge.
    ///
    /// An `Rc<Cell<_>>` has no JS-reachable name. User code could reset a JS
    /// global between two graph constructions: the second callback would
    /// reuse the first id, `derive_wrapper` would deduplicate it, and the
    /// wrong function would run.
    pub(super) ids: std::rc::Rc<Cell<CallbackId>>,
    pub(super) logs: std::rc::Rc<RefCell<Vec<String>>>,
    /// Whether a query error a stack contains stays out of `logs`; see
    /// [`JsRuntime::with_contained_query_errors_unlogged`].
    pub(super) contained_query_errors_unlogged: Cell<bool>,
    pub(super) timeline_state: rustel_core::TimelineState,
    /// MIDI inputs this Session's scores have named, and their live values.
    ///
    /// Shared with the host, which attaches the platform listeners; the tables
    /// themselves are dependency-free atomics in core, so nothing here links a
    /// MIDI stack. JavaScript never holds one - `midin()` hands out an integer
    /// handle and nothing else.
    pub(super) midi_in_bus: std::sync::Arc<rustel_core::midi_in::InputBus>,
    /// Stable handle state. The query path reads the handle map only and never
    /// takes the bus's generation lock.
    pub(super) midi_in_handles: std::rc::Rc<RefCell<MidiInputHandles>>,
    /// Effects owned by the one Session evaluation currently in progress.
    /// The transaction is private Rust state: JavaScript callbacks may append
    /// only while the operation policy permits it, and every unsuccessful
    /// exit drops the whole value rather than leaving work for a later score.
    /// Hydra chains under construction by the score currently evaluating, and
    /// which globals the visuals surface has borrowed. Never JavaScript-
    /// reachable: a score that could reset the arena could make one chain's
    /// index name another chain's shader.
    #[cfg(feature = "hydra")]
    pub(super) hydra_chains: std::rc::Rc<RefCell<crate::install::HydraChains>>,
    pub(super) effect_transaction: std::rc::Rc<RefCell<Option<ScoreEffects>>>,
    /// Which host effects the current operation permits. Setup may register
    /// and preload samples; score evaluation may also stage tempo. Queries and
    /// source-compatible raw evaluation temporarily permit none.
    pub(super) effect_policy: std::rc::Rc<Cell<EffectPolicy>>,
    /// Haps one query may produce. Configurable so the limit can be tested
    /// through the real host without provoking it at full scale.
    pub(super) hap_budget: Cell<u64>,
    /// The last host query's thrown error. Queries still return an empty
    /// window; reports consume this message through `take_query_throw`.
    pub(super) query_threw: RefCell<Option<String>>,
    /// Callback-IR selection is resolved at the owning runtime rather than in
    /// a pattern hot loop. `Compatibility` and `DualRun` are test/benchmark
    /// controls; ordinary playback uses `Auto`.
    pub(super) pattern_transform_ir_mode: Cell<PatternTransformIrMode>,
    pub(super) pattern_transform_ir_stats: Cell<PatternTransformIrStats>,
    /// Live bytes and the ceiling, shared with the allocator enforcing them.
    pub(super) heap: std::rc::Rc<alloc::HeapBudget>,
    /// Shared with the interrupt handler. `Some(deadline)` while an evaluation
    /// is running under a CPU budget; `None` when unbounded.
    pub(super) deadline: std::rc::Rc<Cell<Option<std::time::Instant>>>,
    /// Set once the deadline fires, so the caller can tell an interrupted
    /// evaluation from a normal error.
    pub(super) interrupted: std::rc::Rc<Cell<bool>>,
    /// Atomic cancellation flag borrowed only while a bounded evaluation is
    /// running. The interrupt handler reads it without waiting for JavaScript
    /// to reach a query node.
    pub(super) cancel_flag: std::rc::Rc<Cell<Option<*const std::sync::atomic::AtomicBool>>>,
    /// Distinguishes caller cancellation from expiry of the elapsed-time
    /// interrupt deadline.
    pub(super) cancelled: std::rc::Rc<Cell<bool>>,
    /// Depth of the bounded synchronous query turn currently using this
    /// runtime. Only the outer direct query or scheduler tick owns the
    /// deadline, cancellation pointer, heap flag and pending-job cleanup;
    /// reentrant `queryHeld` calls inherit that exact boundary.
    pub(super) query_turn_depth: Cell<usize>,
    /// Depth of any host query scope, including a legacy query inheriting a
    /// caller-installed raw deadline. Effect refusals use this to reach core's
    /// structural channel before a scheduler can commit callback silence;
    /// eager callbacks during graph construction remain outside it.
    pub(super) effect_query_depth: Cell<usize>,
    /// Depth of the generic QuickJS heap-refusal boundary.
    ///
    /// Score/setup evaluation can call the opt-in `queryHeld` binding while it
    /// already owns this flag. A nested legacy query must not clear it and
    /// thereby launder an allocation denial caught by JavaScript.
    pub(super) heap_boundary_depth: Cell<usize>,
    /// Stable public millisecond value reported for a query-turn deadline
    /// refusal. Kept apart from the absolute `Instant`: callbacks must publish
    /// the typed refusal while the core query is still live, before a scheduler
    /// can commit an interrupted query as silence.
    pub(super) query_limit_millis: Cell<Option<u64>>,
    /// Structural host-policy refusal raised by any effect callback.
    ///
    /// JavaScript is allowed to catch the exception, so outer evaluation and
    /// query boundaries also inspect this latch before accepting an otherwise
    /// successful result.
    pub(super) effect_policy_refusal: std::rc::Rc<RefCell<Option<String>>>,
    /// Nesting depth of the outer operation that owns
    /// [`Self::effect_policy_refusal`].
    ///
    /// A raw/prebake/score evaluation can call `queryHeld`, and that query can
    /// itself be where an effect refusal is first raised. The nested query
    /// must report the policy without clearing it; only depth zero may reset
    /// and finally consume the latch.
    pub(super) effect_policy_owner_depth: std::rc::Rc<Cell<usize>>,
    /// Last successfully installed active wrapper. A live replacement that
    /// evaluates but then fails its query probe restores this exact wrapper
    /// so the sounding score keeps its Sidecar and callback roots.
    pub(super) last_good_active: RefCell<Option<rquickjs::Persistent<rquickjs::Value<'static>>>>,
    /// The host's pointer that `mousex` and `mousey` read, if it has one.
    pub(super) pointer: Option<rustel_core::host_value::Pointer>,
}

/// Effects staged by a successful Session-owned JavaScript score or setup.
///
/// They are deliberately separate from the transpiler output: the caller must
/// apply them only after the owning operation succeeds. Score effects cross
/// the same commit boundary as the graph generation; setup effects cross the
/// complete setup-turn boundary.
#[derive(Debug, Default, PartialEq)]
pub struct ScoreEffects {
    pub cps: Option<f64>,
    pub samples: SamplesEffects,
    pub preload: Vec<String>,
    /// Start the host's gamepad poller only if this score is committed.
    pub gamepad: bool,
    pub(super) midi_inputs: MidiInputCandidate,
    /// What this score asked a visuals window to draw.
    ///
    /// `None` means the score never mentioned Hydra; `Some` with no windows
    /// means it called `clearHydra()`. Both close whatever is open - the
    /// distinction is kept because only the second is something the score
    /// said on purpose.
    ///
    /// Boxed because a score with visuals is the exception, and an unboxed
    /// candidate makes every `ScoreEffects` - one per evaluation, on the
    /// producer thread - carry its bulk whether or not it has one.
    #[cfg(feature = "hydra")]
    pub hydra: Option<Box<crate::install::HydraCandidate>>,
}

impl ScoreEffects {
    pub(super) fn is_empty(&self) -> bool {
        #[cfg(feature = "hydra")]
        if self.hydra.is_some() {
            return false;
        }
        self.cps.is_none()
            && self.samples.is_empty()
            && self.preload.is_empty()
            && !self.gamepad
            && self.midi_inputs.bindings.is_empty()
    }
}

/// RAII ownership for one host-effect-policy operation tree.
///
/// The outermost owner clears stale state on entry and exit. Nested owners may
/// inspect the same latch but cannot consume it, so a `queryHeld` result being
/// swallowed by JavaScript cannot launder a policy failure from its enclosing
/// raw/prebake/score evaluation.
pub(super) struct EffectPolicyOwner {
    depth: std::rc::Rc<Cell<usize>>,
    refusal: std::rc::Rc<RefCell<Option<String>>>,
    previous_depth: usize,
    policy_pending_on_entry: bool,
}

impl EffectPolicyOwner {
    pub(super) fn enter(runtime: &JsRuntime) -> Self {
        let previous_depth = runtime.effect_policy_owner_depth.get();
        if previous_depth == 0 {
            runtime.effect_policy_refusal.borrow_mut().take();
        }
        let policy_pending_on_entry = runtime.effect_policy_refusal.borrow().is_some();
        runtime
            .effect_policy_owner_depth
            .set(previous_depth.saturating_add(1));
        Self {
            depth: runtime.effect_policy_owner_depth.clone(),
            refusal: runtime.effect_policy_refusal.clone(),
            previous_depth,
            policy_pending_on_entry,
        }
    }

    pub(super) fn policy(&self) -> Option<String> {
        self.refusal.borrow().clone()
    }

    pub(super) fn newly_raised_policy(&self) -> Option<String> {
        (!self.policy_pending_on_entry)
            .then(|| self.policy())
            .flatten()
    }
}

impl Drop for EffectPolicyOwner {
    fn drop(&mut self) {
        self.depth.set(self.previous_depth);
        if self.previous_depth == 0 {
            self.refusal.borrow_mut().take();
        }
    }
}

/// One fallible evaluation's private effect accumulator.
///
/// Dropping the owner always discards the accumulator. `finish` is the only
/// path that moves it to Session, after evaluation and policy validation have
/// both succeeded.
pub(super) struct EffectTransactionOwner {
    transaction: std::rc::Rc<RefCell<Option<ScoreEffects>>>,
    policy: std::rc::Rc<Cell<EffectPolicy>>,
    previous_policy: EffectPolicy,
}

impl EffectTransactionOwner {
    pub(super) fn enter(runtime: &JsRuntime, policy: EffectPolicy) -> Result<Self, QueryError> {
        if runtime.effect_query_depth.get() > 0 {
            let message = "JavaScript effect transactions cannot begin during a pattern query";
            let mut refusal = runtime.effect_policy_refusal.borrow_mut();
            if refusal.is_none() {
                *refusal = Some(message.to_string());
            }
            return Err(QueryError::Policy(message.to_string()));
        }
        if runtime.effect_transaction.borrow().is_some() {
            let message = "nested JavaScript effect transactions are not supported";
            let mut refusal = runtime.effect_policy_refusal.borrow_mut();
            if refusal.is_none() {
                *refusal = Some(message.to_string());
            }
            return Err(QueryError::Policy(message.to_string()));
        }
        *runtime.effect_transaction.borrow_mut() = Some(ScoreEffects::default());
        let previous_policy = runtime.effect_policy.replace(policy);
        Ok(Self {
            transaction: runtime.effect_transaction.clone(),
            policy: runtime.effect_policy.clone(),
            previous_policy,
        })
    }

    pub(super) fn finish(self) -> ScoreEffects {
        self.transaction
            .borrow_mut()
            .take()
            .expect("effect transaction owner lost its accumulator")
    }
}

impl Drop for EffectTransactionOwner {
    fn drop(&mut self) {
        self.transaction.borrow_mut().take();
        self.policy.set(self.previous_policy);
    }
}

/// Temporarily narrow host effects without changing transaction ownership.
/// Query-time callbacks use this inside score/setup operations so they cannot
/// append work to the evaluation that happened to query them.
pub(super) struct EffectPolicyScope {
    policy: std::rc::Rc<Cell<EffectPolicy>>,
    previous: EffectPolicy,
}

pub(super) struct EffectQueryScope<'a> {
    depth: &'a Cell<usize>,
    previous: usize,
}

impl EffectQueryScope<'_> {
    pub(super) fn enter(runtime: &JsRuntime) -> EffectQueryScope<'_> {
        let previous = runtime.effect_query_depth.get();
        runtime.effect_query_depth.set(previous.saturating_add(1));
        EffectQueryScope {
            depth: &runtime.effect_query_depth,
            previous,
        }
    }
}

impl Drop for EffectQueryScope<'_> {
    fn drop(&mut self) {
        self.depth.set(self.previous);
    }
}

impl EffectPolicyScope {
    pub(super) fn enter(runtime: &JsRuntime, policy: EffectPolicy) -> Self {
        Self::enter_state(
            &EffectBoundaryState {
                policy: runtime.effect_policy.clone(),
                refusal: runtime.effect_policy_refusal.clone(),
            },
            policy,
        )
    }

    pub(super) fn enter_state(state: &EffectBoundaryState, policy: EffectPolicy) -> Self {
        let previous = state.policy.replace(policy);
        Self {
            policy: state.policy.clone(),
            previous,
        }
    }
}

impl Drop for EffectPolicyScope {
    fn drop(&mut self) {
        self.policy.set(self.previous);
    }
}

/// A graph under construction: its pattern plus the callback sources it owns.
///
/// Sources are compiled into cells only when the graph is **installed**, so
/// nothing is rooted before it has an owner.
#[derive(Default)]
pub struct GraphBuilder {
    /// `(global id, factory source)` for callbacks created by this graph.
    sources: Vec<(CallbackId, String)>,
    /// Wrappers whose cells this graph also needs, because it composes
    /// patterns that were built earlier.
    imports: Vec<(Slot, usize)>,
}

impl GraphBuilder {
    /// Import every cell owned by an existing wrapper.
    ///
    /// Required whenever this graph composes a pattern built by an earlier
    /// evaluation: the new wrapper must independently keep those callbacks
    /// alive, because the source wrapper may be released immediately after.
    pub fn import(&mut self, slot: Slot, index: usize) -> &mut Self {
        self.imports.push((slot, index));
        self
    }

    /// Register a callback **factory**, returning its opaque id.
    ///
    /// The source must evaluate to `(self) => <the callback>`, and is invoked
    /// with the graph's own `PatternWrapper` once that exists. This is how a
    /// user closure comes to capture the pattern it belongs to - the shape that
    /// forms the cross-heap cycle:
    ///
    /// ```text
    /// wrapper -> cell -> closure -> wrapper
    /// ```
    ///
    /// Registering a plain function instead would form no cycle, so it would
    /// not exercise cross-heap ownership.
    /// Ids come from the runtime and are globally unique, so a composed graph
    /// can never confuse two callbacks.
    pub fn callback(&mut self, rt: &JsRuntime, factory_src: &str) -> CallbackId {
        let id = rt.alloc_id();
        self.sources.push((id, factory_src.to_string()));
        id
    }
}

/// Why a host query failed.
///
/// Typed rather than a `String`: a resource refusal and an evaluation error
/// need different exit codes and different advice, and deciding between them by
/// matching message text is a contract nothing checks. `Session` used to do
/// exactly that.
#[derive(Debug, Clone)]
pub enum QueryError {
    /// A limit refused the work.
    Limit(rustel_core::QueryLimit),
    /// A native host policy refused JavaScript that is valid score code but
    /// cannot be applied safely in this execution context.
    ///
    /// Kept distinct from `Message`: Session may try its Mini compatibility
    /// fallback after an ordinary JavaScript/source error, but must never turn
    /// a refused side effect into a different successfully-installed score.
    Policy(String),
    /// Anything else - a missing slot, a JS exception, a broken wrapper.
    Message(String),
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Limit(limit) => write!(f, "{limit}"),
            Self::Policy(message) => write!(f, "{message}"),
            Self::Message(message) => write!(f, "{message}"),
        }
    }
}

impl From<String> for QueryError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

impl From<rustel_core::QueryLimit> for QueryError {
    fn from(limit: rustel_core::QueryLimit) -> Self {
        Self::Limit(limit)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Slot {
    Active,
    Held,
}

/// RAII scope for the query stack.
///
/// Pushes the wrapper being queried and pops on drop - including on unwind, so
/// a callback that throws cannot leave a wrapper rooted. Nesting is why this is
/// a stack: a join queries an inner pattern while an outer query is live, and a
/// single slot would let the inner one clobber the outer's callback table.
pub(super) struct QueryScope<'a> {
    rt: &'a JsRuntime,
}

/// Push one concrete wrapper while JavaScript calls its `.query(state)` method.
///
/// The wrapper may be a prebake-held pattern rather than the active graph. Its
/// callback sidecar is therefore the only correct table for the nested native
/// query.
pub(super) struct WrapperQueryScope<'js> {
    stack: rquickjs::Array<'js>,
}

impl<'js> WrapperQueryScope<'js> {
    pub(super) fn push(
        ctx: &Ctx<'js>,
        wrapper: rquickjs::Class<'js, NativePatternWrapper<'js>>,
    ) -> rquickjs::Result<Self> {
        let stack = host_stack(ctx)?;
        stack.set(stack.len(), wrapper)?;
        Ok(Self { stack })
    }
}

impl Drop for WrapperQueryScope<'_> {
    fn drop(&mut self) {
        let len = self.stack.len();
        if len > 0 {
            let _ = self.stack.set(
                len - 1,
                rquickjs::Value::new_undefined(self.stack.ctx().clone()),
            );
            let _ = self.stack.as_object().set("length", len - 1);
        }
    }
}

impl<'a> QueryScope<'a> {
    /// Push the active native wrapper, whose sidecar owns the callbacks a
    /// host-built graph can reach.
    pub(super) fn push_active_native(rt: &'a JsRuntime) -> Result<Self, String> {
        with_ctx(&rt.ctx, |ctx| -> Result<(), String> {
            let active: rquickjs::Value = host_active(&ctx)
                .map_err(|error| error.to_string())?
                .get(0)
                .map_err(|error| error.to_string())?;
            let stack = host_stack(&ctx).map_err(|e| e.to_string())?;
            let n = stack.len();
            stack.set(n, active).map_err(|e| e.to_string())
        })?;
        Ok(QueryScope { rt })
    }

    pub(super) fn push(rt: &'a JsRuntime, slot: Slot, index: usize) -> Result<Self, String> {
        with_ctx(&rt.ctx, |ctx| -> Result<(), String> {
            let w = JsRuntime::wrapper_at(&ctx, slot, index)?;
            let stack = host_stack(&ctx).map_err(|e| e.to_string())?;
            let n = stack.len();
            stack.set(n, w).map_err(|e| e.to_string())
        })?;
        Ok(QueryScope { rt })
    }
}

impl Drop for QueryScope<'_> {
    fn drop(&mut self) {
        with_ctx(&self.rt.ctx, |ctx| {
            if let Ok(stack) = host_stack(&ctx) {
                let n = stack.len();
                if n > 0 {
                    // Pop WITHOUT evaluating JavaScript.
                    //
                    // Calling `(a) => { a.length-- }` through `ctx.eval` is
                    // unsafe here: after the evaluation deadline fires, the
                    // interrupt handler rejects further JavaScript execution
                    // and the stack would not unwind. Cleanup must not require
                    // JavaScript execution.
                    //
                    // Clearing the slot first drops the wrapper reference even
                    // if the length assignment is refused.
                    let _ = stack.set(n - 1, rquickjs::Value::new_undefined(ctx.clone()));
                    let _ = stack.as_object().set("length", n - 1);
                }
            }
        });
    }
}

mod api;
mod callback_host;
mod callback_query;
mod evaluation;
mod inspection;
mod teardown;

#[cfg(test)]
mod caught_panic_tests {
    use super::*;

    fn runtime_with_panicking_native() -> JsRuntime {
        let runtime = JsRuntime::new().expect("runtime");
        with_ctx(&runtime.ctx, |ctx| {
            let boom = rquickjs::Function::new(ctx.clone(), || -> i32 { panic!("native boom") })
                .expect("native function");
            ctx.globals().set("boom", boom).expect("global");
        });
        runtime
    }

    #[test]
    fn a_native_panic_caught_by_score_javascript_is_raised_again() {
        let runtime = runtime_with_panicking_native();
        runtime
            .eval("try { boom(); } catch (error) {}")
            .expect("the score caught the panic as an exception");
        let raised = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.raise_caught_native_panic();
        }))
        .expect_err("the caught panic is raised again");
        assert_eq!(raised.downcast_ref::<&str>(), Some(&"native boom"));
    }

    #[test]
    fn raising_with_no_caught_panic_does_nothing() {
        let runtime = runtime_with_panicking_native();
        runtime.raise_caught_native_panic();
        runtime
            .eval("globalThis.after = 1;")
            .expect("the realm still works");
    }
}
