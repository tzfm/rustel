use super::*;

pub fn cells_dropped() -> u64 {
    CELLS_DROPPED.with(|c| c.get())
}

/// Callback cells created so far. `created - dropped` is the live count.
pub fn cells_created() -> u64 {
    CELLS_CREATED.with(|c| c.get())
}

/// Cells currently alive.
pub fn cells_live() -> u64 {
    cells_created().saturating_sub(cells_dropped())
}
pub fn wrappers_dropped() -> u64 {
    WRAPPERS_DROPPED.with(|c| c.get())
}

// ---------------------------------------------------------------------------
// The trace-managed sidecar
// ---------------------------------------------------------------------------

/// One cell per callback, owned by the wrapper that reaches it.
#[derive(Trace, JsLifetime)]
pub(super) enum CellPayload<'js> {
    Function(Function<'js>),
    Value(rquickjs::Value<'js>),
}

#[rquickjs::class]
#[derive(Trace, JsLifetime)]
pub struct CallbackCell<'js> {
    pub(super) payload: CellPayload<'js>,
}

impl<'js> CallbackCell<'js> {
    pub(super) fn function(func: Function<'js>) -> Self {
        Self {
            payload: CellPayload::Function(func),
        }
    }

    pub(super) fn value(value: rquickjs::Value<'js>) -> Self {
        Self {
            payload: CellPayload::Value(value),
        }
    }

    pub(super) fn as_function(&self) -> Option<&Function<'js>> {
        match &self.payload {
            CellPayload::Function(func) => Some(func),
            CellPayload::Value(_) => None,
        }
    }

    pub(super) fn as_value(&self) -> Option<&rquickjs::Value<'js>> {
        match &self.payload {
            CellPayload::Value(value) => Some(value),
            CellPayload::Function(_) => None,
        }
    }
}

impl Drop for CallbackCell<'_> {
    fn drop(&mut self) {
        if matches!(self.payload, CellPayload::Function(_)) {
            CELLS_DROPPED.with(|c| c.set(c.get() + 1));
        }
    }
}

/// The JS-visible pattern handle. Owns its graph and that graph's callbacks.
#[rquickjs::class]
#[derive(JsLifetime)]
pub struct PatternWrapper<'js> {
    #[qjs(skip_trace)]
    pub pattern: Pattern,
    /// Global callback ids, parallel to `cells`. **Not positional indexes** -
    /// see "Callback identity". Kept as two vectors because `JsLifetime` is not
    /// implemented for tuples.
    #[qjs(skip_trace)]
    pub ids: Vec<CallbackId>,
    pub cells: Vec<rquickjs::Class<'js, CallbackCell<'js>>>,
}

impl<'js> PatternWrapper<'js> {
    pub(super) fn cell(&self, id: CallbackId) -> Option<&rquickjs::Class<'js, CallbackCell<'js>>> {
        self.ids
            .iter()
            .position(|k| *k == id)
            .map(|i| &self.cells[i])
    }
}

/// Host object for natively-built pattern graphs.
///
/// It carries the same trace-managed callback sidecar as [`PatternWrapper`],
/// because a graph built through the native surface can now reach user
/// JavaScript: `every(4, x => x.fast(2))` puts a JS function inside a hap.
/// Before that bridge existed this type was deliberately callback-free, and
/// the host refused any function argument outright.
#[rquickjs::class]
#[derive(Clone, JsLifetime, Trace)]
pub struct NativePatternWrapper<'js> {
    #[qjs(skip_trace)]
    pub(super) pattern: Pattern,
    /// The exact native query function currently belonging to this wrapper.
    ///
    /// This is private identity state, not a public truthy marker. A native
    /// query copied from another Pattern still carries that other Pattern's
    /// graph; comparing the visible function with this exact handle
    /// distinguishes an own query from an ordinary `a.query = b.query`
    /// reassignment. `withSteps` deliberately adopts the copied handle on its
    /// fresh wrapper so repeated metadata transforms stay native.
    pub(super) native_query: Option<Function<'js>>,
    /// Global callback ids reachable from `pattern`, parallel to `cells`.
    /// **Not positional indexes** - see "Callback identity".
    #[qjs(skip_trace)]
    pub(super) ids: Vec<CallbackId>,
    /// The trace-managed sidecar. A wrapper owns every cell its graph can
    /// reach, so replacing the active slot makes the graph, its cells and the
    /// JS functions they hold collectable together. There is deliberately no
    /// global registry, which would keep every callback alive.
    pub(super) cells: Vec<rquickjs::Class<'js, CallbackCell<'js>>>,
    /// `true` only when an operation has proved that `ids`/`cells` already
    /// contain every owner the graph can use, even when its purity is opaque.
    /// The evaluation publisher must then not conservatively import unrelated
    /// scratch cells from the surrounding [`BridgeFrame`].
    #[qjs(skip_trace)]
    pub(super) explicit_ownership_complete: bool,
    /// Frame-harvested ids proven to belong only to sources removed from this
    /// graph. Derived opaque wrappers propagate this deny-list so an outer
    /// composition cannot conservatively resurrect a filtered value. The set
    /// is immutable/Rc-shared across metadata-only transforms and capped per
    /// wrapper. Retaining every version of a growing lineage can still consume
    /// aggregate Rust memory; that runtime-total bound is outside this slice.
    #[qjs(skip_trace)]
    pub(super) excluded_frame_ids: Rc<HashSet<CallbackId>>,
}

pub(super) const STEPALT_OWNERSHIP_OPERATION: &str = "stepalt ownership";
pub(super) const MAX_OWNERSHIP_EXCLUSIONS: usize = rustel_core::MAX_STEPWISE_ENTRIES as usize;

#[derive(Debug)]
pub(super) enum OwnershipSetError {
    Limit(rustel_core::QueryLimit),
    Allocation,
}

pub(super) fn ownership_limit() -> rustel_core::QueryLimit {
    rustel_core::QueryLimit::StepwiseExpansion {
        operation: STEPALT_OWNERSHIP_OPERATION,
        minimum_entries: rustel_core::MAX_STEPWISE_ENTRIES.saturating_add(1),
        limit: rustel_core::MAX_STEPWISE_ENTRIES,
    }
}

/// Fallibly build one capped exclusion set after subtracting every id whose
/// ownership/reachability overrides exclusion. The builder never reserves from
/// the raw input cardinality: an adversarial union may be much larger than the
/// public cap even when the final set is small.
pub(super) struct CappedExclusions {
    ids: HashSet<CallbackId>,
}

impl CappedExclusions {
    pub(super) fn new(reserve_hint: usize) -> Result<Self, OwnershipSetError> {
        let mut ids = HashSet::new();
        ids.try_reserve(reserve_hint.min(MAX_OWNERSHIP_EXCLUSIONS))
            .map_err(|_| OwnershipSetError::Allocation)?;
        Ok(Self { ids })
    }

    pub(super) fn insert(
        &mut self,
        id: CallbackId,
        protected: &HashSet<CallbackId>,
    ) -> Result<(), OwnershipSetError> {
        if protected.contains(&id) || self.ids.contains(&id) {
            return Ok(());
        }
        if self.ids.len() == MAX_OWNERSHIP_EXCLUSIONS {
            return Err(OwnershipSetError::Limit(ownership_limit()));
        }
        if self.ids.len() == self.ids.capacity() {
            self.ids
                .try_reserve(1)
                .map_err(|_| OwnershipSetError::Allocation)?;
        }
        self.ids.insert(id);
        Ok(())
    }

    pub(super) fn finish(self) -> HashSet<CallbackId> {
        self.ids
    }
}

pub(super) fn plan_capped_exclusions(
    sets: &[&HashSet<CallbackId>],
    protected: &HashSet<CallbackId>,
) -> Result<HashSet<CallbackId>, OwnershipSetError> {
    let reserve_hint = sets
        .iter()
        .fold(0_usize, |total, set| total.saturating_add(set.len()));
    let mut planned = CappedExclusions::new(reserve_hint)?;
    for set in sets {
        for id in set.iter().copied() {
            planned.insert(id, protected)?;
        }
    }
    Ok(planned.finish())
}

pub(super) fn ownership_refusal_wrapper<'js>(
    ctx: &Ctx<'js>,
    limit: rustel_core::QueryLimit,
) -> rquickjs::Result<rquickjs::Class<'js, NativePatternWrapper<'js>>> {
    let limit = rustel_core::mark_stepwise_refusal(limit);
    new_wrapper(
        ctx,
        NativePatternWrapper::plain(rustel_core::query_limit_pattern(limit)),
    )
}

impl<'js> NativePatternWrapper<'js> {
    pub(super) fn plain(pattern: Pattern) -> Self {
        Self {
            pattern,
            native_query: None,
            ids: Vec::new(),
            cells: Vec::new(),
            explicit_ownership_complete: false,
            excluded_frame_ids: Rc::new(HashSet::new()),
        }
    }

    pub(super) fn cell(&self, id: CallbackId) -> Option<&rquickjs::Class<'js, CallbackCell<'js>>> {
        self.ids
            .iter()
            .position(|k| *k == id)
            .map(|i| &self.cells[i])
    }

    /// A derived wrapper over the SAME callbacks. Every combinator that only
    /// transforms `self` keeps the sidecar, or the derived graph would lose
    /// the cells it still reaches.
    pub(super) fn with_pattern(&self, pattern: Pattern) -> Self {
        Self {
            pattern,
            native_query: self.native_query.clone(),
            ids: self.ids.clone(),
            cells: self.cells.clone(),
            explicit_ownership_complete: self.explicit_ownership_complete,
            excluded_frame_ids: self.excluded_frame_ids.clone(),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum FrameHarvestPolicy {
    /// An opaque graph may depend on any cell produced in the live operation.
    Conservative,
    /// The caller has a complete list of the graph's contributing sidecars.
    /// Opaque reachability remains conservative *within those sidecars*, but
    /// unrelated frame scratch is not ownership of this graph.
    ExplicitSourcesComplete,
}

/// Build a wrapper that owns the union of the contributing wrappers' cells.
///
/// A derived graph may reach callbacks owned by ANY of its operands, and the
/// operand wrappers can die immediately afterwards, so the new wrapper has to
/// keep them alive independently. Dropping this union is the "missing cell"
/// hard error `with_callback` reports rather than a silent wrong-callback
/// invocation.
pub(super) fn derive_wrapper<'js>(
    ctx: Ctx<'js>,
    pattern: Pattern,
    sources: &[Sidecar<'js>],
) -> rquickjs::Result<rquickjs::Class<'js, NativePatternWrapper<'js>>> {
    derive_wrapper_with_policy(ctx, pattern, sources, &[], FrameHarvestPolicy::Conservative)
}

/// Build a wrapper whose ownership is exactly the supplied source sidecars.
///
/// This is intentionally narrower than [`derive_wrapper`]. `stepalt` reifies
/// every source once and knows which source graphs survive its step filter, so
/// those retained sidecars are a complete ownership proof. An opaque retained
/// source still contributes all of *its* cells; it does not make later
/// unrelated values accumulated in the live bridge frame part of the result.
/// A retained opaque wrapper constructed *after* a filtered JS value in the
/// same frame may already own that value conservatively before `stepalt` sees
/// either source. Pruning that pre-polluted sidecar remains outside this narrow
/// proof; retained opaque sources are isolated in an earlier turn by the
/// ownership checks below.
pub(super) fn derive_wrapper_from_explicit_sources<'js>(
    ctx: Ctx<'js>,
    pattern: Pattern,
    sources: &[Sidecar<'js>],
    filtered_sources: &[Sidecar<'js>],
) -> rquickjs::Result<rquickjs::Class<'js, NativePatternWrapper<'js>>> {
    derive_wrapper_with_policy(
        ctx,
        pattern,
        sources,
        filtered_sources,
        FrameHarvestPolicy::ExplicitSourcesComplete,
    )
}

pub(super) fn derive_wrapper_with_policy<'js>(
    ctx: Ctx<'js>,
    pattern: Pattern,
    sources: &[Sidecar<'js>],
    filtered_sources: &[Sidecar<'js>],
    frame_policy: FrameHarvestPolicy,
) -> rquickjs::Result<rquickjs::Class<'js, NativePatternWrapper<'js>>> {
    // Only cells the surviving graph can actually REACH are transferred.
    //
    // Taking every cell in scope roots callbacks belonging to discarded
    // temporaries: `const tmp = …every(fastcat(2,3), f); kept` kept `f` alive
    // for as long as `kept` lived. `reachable_callbacks` is computed by the
    // same walk that classifies purity, so this costs nothing extra.
    //
    // When the reachable set is INCOMPLETE - an opaque closure may materialise
    // callbacks at query time and cannot be enumerated statically - every cell
    // is kept. Under-retaining there would sweep a callback still in use, which
    // is a use-after-free rather than a leak.
    let opaque = pattern.purity().opaque;
    let reachable = pattern.reachable_callbacks();
    let mut reachable_ids = HashSet::new();
    reachable_ids
        .try_reserve(reachable.len())
        .map_err(|_| rquickjs::Error::Allocation)?;
    reachable_ids.extend(reachable.iter().copied());
    let owned_capacity = sources.iter().fold(0_usize, |total, source| {
        total.saturating_add(source.ids.len())
    });
    let mut explicitly_owned = HashSet::new();
    explicitly_owned
        .try_reserve(owned_capacity)
        .map_err(|_| rquickjs::Error::Allocation)?;
    explicitly_owned.extend(sources.iter().flat_map(|source| source.ids.iter().copied()));
    let excluded_hint = sources
        .iter()
        .fold(0_usize, |total, source| {
            total.saturating_add(source.excluded_frame_ids.len())
        })
        .saturating_add(filtered_sources.iter().fold(0_usize, |total, source| {
            total
                .saturating_add(source.ids.len())
                .saturating_add(source.excluded_frame_ids.len())
        }));
    let mut protected = HashSet::new();
    protected
        .try_reserve(reachable_ids.len().saturating_add(explicitly_owned.len()))
        .map_err(|_| rquickjs::Error::Allocation)?;
    protected.extend(reachable_ids.iter().copied());
    protected.extend(explicitly_owned.iter().copied());
    let mut exclusions = CappedExclusions::new(excluded_hint).map_err(|error| match error {
        OwnershipSetError::Allocation => rquickjs::Error::Allocation,
        OwnershipSetError::Limit(_) => unreachable!("an empty exclusion builder cannot exceed cap"),
    })?;
    for source in sources {
        for id in source.excluded_frame_ids.iter().copied() {
            if let Err(error) = exclusions.insert(id, &protected) {
                return match error {
                    OwnershipSetError::Limit(limit) => ownership_refusal_wrapper(&ctx, limit),
                    OwnershipSetError::Allocation => Err(rquickjs::Error::Allocation),
                };
            }
        }
    }
    for source in filtered_sources {
        // Directly filtered ownership is the new proof established by this
        // operation. Persistent inherited exclusions preserve that proof if a
        // saved owner is published only in a later turn.
        for id in source.ids.iter().copied() {
            if let Err(error) = exclusions.insert(id, &protected) {
                return match error {
                    OwnershipSetError::Limit(limit) => ownership_refusal_wrapper(&ctx, limit),
                    OwnershipSetError::Allocation => Err(rquickjs::Error::Allocation),
                };
            }
        }
        for id in source.excluded_frame_ids.iter().copied() {
            if let Err(error) = exclusions.insert(id, &protected) {
                return match error {
                    OwnershipSetError::Limit(limit) => ownership_refusal_wrapper(&ctx, limit),
                    OwnershipSetError::Allocation => Err(rquickjs::Error::Allocation),
                };
            }
        }
    }
    let frame_exclusions = with_bridge_frame(|frame: &BridgeFrame<'js>| {
        let suppressed = frame.suppressed.borrow();
        for id in suppressed.iter().copied() {
            exclusions.insert(id, &protected)?;
        }
        Ok::<_, OwnershipSetError>(())
    });
    if let Some(Err(error)) = frame_exclusions {
        return match error {
            OwnershipSetError::Limit(limit) => ownership_refusal_wrapper(&ctx, limit),
            OwnershipSetError::Allocation => Err(rquickjs::Error::Allocation),
        };
    }
    let excluded_frame_ids = exclusions.finish();
    let reconciled = if matches!(frame_policy, FrameHarvestPolicy::ExplicitSourcesComplete) {
        reconcile_all_bridge_frames(&explicitly_owned, &excluded_frame_ids)
    } else {
        reconcile_all_bridge_frames(&explicitly_owned, &HashSet::new())
    };
    if let Err(error) = reconciled {
        return match error {
            OwnershipSetError::Limit(limit) => ownership_refusal_wrapper(&ctx, limit),
            OwnershipSetError::Allocation => Err(rquickjs::Error::Allocation),
        };
    }
    let wanted = |id: &CallbackId| {
        (opaque || reachable_ids.contains(id)) && !excluded_frame_ids.contains(id)
    };

    let mut ids: Vec<CallbackId> = Vec::new();
    let mut cells: Vec<rquickjs::Class<'js, CallbackCell<'js>>> = Vec::new();
    let owner_capacity = owned_capacity.saturating_add(
        with_bridge_frame(|frame: &BridgeFrame<'js>| frame.harvested.borrow().len()).unwrap_or(0),
    );
    let mut seen_ids = HashSet::new();
    seen_ids
        .try_reserve(owner_capacity)
        .map_err(|_| rquickjs::Error::Allocation)?;
    let mut take = |id: CallbackId, cell: &rquickjs::Class<'js, CallbackCell<'js>>| {
        if wanted(&id) && seen_ids.insert(id) {
            ids.push(id);
            cells.push(cell.clone());
        }
    };
    if matches!(frame_policy, FrameHarvestPolicy::Conservative) {
        with_bridge_frame(|frame: &BridgeFrame<'js>| {
            for (id, cell) in frame.harvested.borrow().iter() {
                take(*id, cell);
            }
        });
    }
    for source in sources {
        for (id, cell) in source.ids.iter().zip(source.cells.iter()) {
            take(*id, cell);
        }
    }
    new_wrapper(
        &ctx,
        NativePatternWrapper {
            pattern,
            native_query: None,
            ids,
            cells,
            explicit_ownership_complete: matches!(
                frame_policy,
                FrameHarvestPolicy::ExplicitSourcesComplete
            ),
            excluded_frame_ids: Rc::new(excluded_frame_ids),
        },
    )
}

/// The callback ownership of one contributing value, detached from the wrapper
/// so the borrow does not outlive the call that reads it.
#[derive(Clone, Default)]
pub(super) struct Sidecar<'js> {
    pub(super) ids: Vec<CallbackId>,
    pub(super) cells: Vec<rquickjs::Class<'js, CallbackCell<'js>>>,
    pub(super) excluded_frame_ids: Rc<HashSet<CallbackId>>,
}

impl<'js> Sidecar<'js> {
    pub(super) fn of(wrapper: &NativePatternWrapper<'js>) -> Self {
        Self {
            ids: wrapper.ids.clone(),
            cells: wrapper.cells.clone(),
            excluded_frame_ids: wrapper.excluded_frame_ids.clone(),
        }
    }

    pub(super) fn one(id: CallbackId, cell: rquickjs::Class<'js, CallbackCell<'js>>) -> Self {
        Self {
            ids: vec![id],
            cells: vec![cell],
            excluded_frame_ids: Rc::new(HashSet::new()),
        }
    }

    pub(super) fn merge_all(
        sidecars: impl IntoIterator<Item = Self>,
    ) -> Result<Self, OwnershipSetError> {
        let iterator = sidecars.into_iter();
        let reserve_hint = iterator.size_hint().1.unwrap_or(iterator.size_hint().0);
        let mut sidecars = Vec::new();
        sidecars
            .try_reserve(reserve_hint)
            .map_err(|_| OwnershipSetError::Allocation)?;
        sidecars.extend(iterator);

        let owner_hint = sidecars.iter().fold(0_usize, |total, sidecar| {
            total.saturating_add(sidecar.ids.len())
        });
        let mut owned = HashSet::new();
        owned
            .try_reserve(owner_hint)
            .map_err(|_| OwnershipSetError::Allocation)?;
        owned.extend(
            sidecars
                .iter()
                .flat_map(|sidecar| sidecar.ids.iter().copied()),
        );
        let exclusion_hint = sidecars.iter().fold(0_usize, |total, sidecar| {
            total.saturating_add(sidecar.excluded_frame_ids.len())
        });
        let mut exclusions = CappedExclusions::new(exclusion_hint)?;
        for sidecar in &sidecars {
            for id in sidecar.excluded_frame_ids.iter().copied() {
                exclusions.insert(id, &owned)?;
            }
        }

        let mut ids = Vec::new();
        let mut cells = Vec::new();
        ids.try_reserve(owned.len())
            .map_err(|_| OwnershipSetError::Allocation)?;
        cells
            .try_reserve(owned.len())
            .map_err(|_| OwnershipSetError::Allocation)?;
        let mut emitted = HashSet::new();
        emitted
            .try_reserve(owned.len())
            .map_err(|_| OwnershipSetError::Allocation)?;
        for sidecar in sidecars {
            for (id, cell) in sidecar.ids.into_iter().zip(sidecar.cells) {
                if emitted.insert(id) {
                    ids.push(id);
                    cells.push(cell);
                }
            }
        }

        Ok(Self {
            ids,
            cells,
            excluded_frame_ids: Rc::new(exclusions.finish()),
        })
    }

    pub(super) fn absorb(&mut self, other: Self) -> Result<(), OwnershipSetError> {
        let merged = Self::merge_all([std::mem::take(self), other])?;
        *self = merged;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The bridge frame - host-private, RAII-scoped.
//
// Two things need a home that user JavaScript cannot reach:
//
//  * the callback id allocator. A JS global is never host-private: user code
//    can read, reset, delete, freeze or replace it. A reset between two graph
//    constructions would give the second callback the first id,
//    `derive_wrapper` would deduplicate it, and the wrong function would run.
//  * the SCRATCH of cells created before any wrapper owns them.
//    `register()`'s fast path calls combinator bodies eagerly, so
//    `every(4, x => x.fast(2))` invokes the user function while the graph is
//    still being built and there is no wrapper on the query stack yet.
//
// Both live in a Rust-owned frame on the stack, registered through a
// thread-local of erased pointers and popped by an RAII guard - so they are
// cleared on success, on a JS exception, on a Rust error, on interrupt and on
// unwind. There is no permanent registry and no `Persistent` root: a frame
// cannot outlive the call that created it.
// ---------------------------------------------------------------------------

/// Callback cells created inside one evaluation or one query.
pub(super) struct BridgeFrame<'js> {
    /// Shared with the runtime; never exposed to JS.
    pub(super) alloc: std::rc::Rc<Cell<CallbackId>>,
    /// Created here and not yet owned by a wrapper, so still resolvable.
    pub(super) pending: RefCell<Vec<(CallbackId, Function<'js>)>>,
    /// Cells taken from wrappers RETURNED by callbacks. A transformer may hand
    /// back a graph that itself reaches a callback; unwrapping it to a
    /// `Pattern` and dropping its sidecar loses the only owner of that cell,
    /// and the id becomes unresolvable on the next query.
    pub(super) harvested: RefCell<Vec<(CallbackId, rquickjs::Class<'js, CallbackCell<'js>>)>>,
    /// Cells proven to belong only to sources filtered from a complete
    /// operation such as `stepalt`. Opaque scratch scans skip these ids unless
    /// the graph statically reaches them or an explicit source owns them.
    pub(super) suppressed: RefCell<HashSet<CallbackId>>,
}

impl<'js> BridgeFrame<'js> {
    pub(super) fn new(alloc: std::rc::Rc<Cell<CallbackId>>) -> Self {
        Self {
            alloc,
            pending: RefCell::new(Vec::new()),
            harvested: RefCell::new(Vec::new()),
            suppressed: RefCell::new(HashSet::new()),
        }
    }

    pub(super) fn next_id(&self) -> CallbackId {
        let id = self.alloc.get();
        self.alloc.set(id + 1);
        id
    }
}

thread_local! {
    /// Stack of live frames, innermost last. Each frame is erased to an
    /// opaque pointer. A raw pointer carries no lifetime, so each read
    /// restores the `'js` parameter with a pointer cast. Only `BridgeScope`
    /// sets the stack, and it removes the pointer before the frame can die.
    pub(super) static BRIDGE_FRAMES: RefCell<Vec<*const ()>> = const { RefCell::new(Vec::new()) };
}

/// RAII registration of a frame. Popping on `Drop` is what makes the scratch
/// lifetime correct under exceptions and unwinding.
pub(super) struct BridgeScope;

impl BridgeScope {
    pub(super) fn push<'js>(frame: &BridgeFrame<'js>) -> Self {
        // SAFETY: the pointer is removed by `Drop` before `frame` can die, and
        // is only dereferenced while this scope is on the stack.
        let erased = std::ptr::from_ref(frame).cast::<()>();
        BRIDGE_FRAMES.with(|frames| frames.borrow_mut().push(erased));
        BridgeScope
    }
}

impl Drop for BridgeScope {
    fn drop(&mut self) {
        BRIDGE_FRAMES.with(|frames| {
            frames.borrow_mut().pop();
        });
    }
}

/// Move a returned wrapper's cells into the live frame, so the durable owner
/// picks them up when the surviving wrapper is built.
pub(super) fn harvest<'js>(
    ids: Vec<CallbackId>,
    cells: Vec<rquickjs::Class<'js, CallbackCell<'js>>>,
) {
    with_bridge_frame(|frame: &BridgeFrame<'js>| {
        let mut harvested = frame.harvested.borrow_mut();
        for (id, cell) in ids.into_iter().zip(cells) {
            if !harvested.iter().any(|(known, _)| *known == id) {
                harvested.push((id, cell));
            }
        }
    });
}

/// Transfer a complete sidecar into bridge scratch without dropping its
/// filtered-id proof. Publishing suppression to every currently open frame is
/// what lets a nested query return before an outer eager combinator derives
/// its durable wrapper.
pub(super) fn harvest_sidecar<'js>(sidecar: Sidecar<'js>) -> rquickjs::Result<()> {
    let mut owned = HashSet::new();
    owned
        .try_reserve(sidecar.ids.len())
        .map_err(|_| rquickjs::Error::Allocation)?;
    owned.extend(sidecar.ids.iter().copied());
    let harvested_reserved = with_bridge_frame(|frame: &BridgeFrame<'js>| {
        frame
            .harvested
            .borrow_mut()
            .try_reserve(sidecar.ids.len())
            .is_ok()
    })
    .unwrap_or(true);
    if !harvested_reserved {
        return Err(rquickjs::Error::Allocation);
    }
    reconcile_all_bridge_frames(&owned, sidecar.excluded_frame_ids.as_ref())
        .map_err(ownership_set_error_to_js)?;
    harvest(sidecar.ids, sidecar.cells);
    Ok(())
}

pub(super) fn harvest_sidecars<'js>(
    sidecars: impl IntoIterator<Item = Sidecar<'js>>,
) -> rquickjs::Result<()> {
    let aggregate = Sidecar::merge_all(sidecars).map_err(ownership_set_error_to_js)?;
    harvest_sidecar(aggregate)
}

/// Make the receiver's and arguments' cells available for the duration of a
/// combinator body.
///
/// `register()`'s fast path applies a transformer EAGERLY - `pat.every(2, f)`
/// calls `f` during construction - and at that moment nothing else knows who
/// owns the receiver's callbacks: no wrapper is on the query stack, and the
/// receiver's cells were bridged in an earlier evaluation whose frame is gone.
/// `callback_argument` would then hand `f` a pattern carrying ids it cannot
/// resolve, and `f` is free to keep it.
///
/// Publishing into `harvested` reuses the machinery already there rather than
/// adding a second ownership channel: `pending_callback` and
/// `callback_argument` both scan it, and `derive_wrapper` transfers from it
/// under the same reachability filter. Nothing new is retained - these are the
/// exact cells the derived wrapper imports from its sidecars anyway.
pub(super) fn publish_owner_cells(sidecars: &[Sidecar<'_>]) -> rquickjs::Result<()> {
    harvest_sidecars(sidecars.iter().cloned())
}

/// Run `f` with the innermost live frame, if any.
pub(super) fn with_bridge_frame<'js, R>(f: impl FnOnce(&BridgeFrame<'js>) -> R) -> Option<R> {
    let ptr = BRIDGE_FRAMES.with(|frames| frames.borrow().last().copied())?;
    // SAFETY: pushed by `BridgeScope`, which outlives this call.
    Some(f(unsafe { &*ptr.cast::<BridgeFrame<'js>>() }))
}

pub(super) fn reconcile_all_bridge_frames(
    owned: &HashSet<CallbackId>,
    excluded: &HashSet<CallbackId>,
) -> Result<(), OwnershipSetError> {
    if owned.is_empty() && excluded.is_empty() {
        return Ok(());
    }
    let frames = BRIDGE_FRAMES.with(|frames| frames.borrow().clone());
    let mut planned = Vec::new();
    planned
        .try_reserve_exact(frames.len())
        .map_err(|_| OwnershipSetError::Allocation)?;
    // Plan every frame before mutating any of them. The cap applies to the
    // final `(old union excluded) - owned` set, so an explicit owner can free
    // a slot and no refusal leaves only some nesting levels changed.
    for ptr in &frames {
        // SAFETY: every pointer remains live while its BridgeScope is stacked.
        let frame = unsafe { &*ptr.cast::<BridgeFrame<'static>>() };
        let suppressed = frame.suppressed.borrow();
        planned.push(plan_capped_exclusions(&[&suppressed, excluded], owned)?);
    }
    for (ptr, candidate) in frames.into_iter().zip(planned) {
        // SAFETY: as above; only lifetime-free integer ids are copied.
        let frame = unsafe { &*ptr.cast::<BridgeFrame<'static>>() };
        *frame.suppressed.borrow_mut() = candidate;
    }
    Ok(())
}

pub(super) fn suppress_in_all_bridge_frames(
    ids: &HashSet<CallbackId>,
) -> Result<(), OwnershipSetError> {
    reconcile_all_bridge_frames(&HashSet::new(), ids)
}

pub(super) fn ownership_set_error_to_js(error: OwnershipSetError) -> rquickjs::Error {
    if let OwnershipSetError::Limit(limit) = error {
        record_ownership_refusal(limit);
    }
    // Every query-time caller observes the typed refusal latch before this
    // transport error. Scalar wrapper-producing paths convert the same limit
    // to a query-limit graph instead.
    rquickjs::Error::Allocation
}

/// Number of cells currently held in bridge scratch. Zero between operations.
pub fn bridge_scratch_len() -> usize {
    BRIDGE_FRAMES
        .with(|frames| {
            frames.borrow().last().copied().map(|ptr| {
                // SAFETY: as above.
                let frame = unsafe { &*ptr.cast::<BridgeFrame<'static>>() };
                frame.pending.borrow().len() + frame.harvested.borrow().len()
            })
        })
        .unwrap_or(0)
}

/// Depth of the live bridge-frame stack. Zero when no evaluation or query is
/// in progress.
pub fn bridge_frame_depth() -> usize {
    BRIDGE_FRAMES.with(|frames| frames.borrow().len())
}
