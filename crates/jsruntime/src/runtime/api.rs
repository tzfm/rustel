use super::*;

impl JsRuntime {
    pub fn new() -> rquickjs::Result<Self> {
        // The budget is active from CONSTRUCTION, so no allocation is ever
        // unbounded. QuickJS's own `set_memory_limit` is deliberately NOT used:
        // it rejects before calling the allocator, so a refusal would never
        // reach the structural flag and would be indistinguishable from a user
        // throw once it surfaced as an exception.
        let budget = std::rc::Rc::new(alloc::HeapBudget::new(DEFAULT_JS_MEMORY_LIMIT));
        let rt = Runtime::new_with_alloc(alloc::BudgetedAllocator::new(budget.clone()))?;
        // QuickJS anchored its stack budget on the pointer it recorded in the
        // line above and never moves it again, so the size has to be set here,
        // before any query can recurse beneath it. See `MAX_JS_STACK_BYTES`.
        rt.set_max_stack_size(MAX_JS_STACK_BYTES);
        // This runtime never grants module resolution. The explicit resolver
        // is the enforcement boundary for dynamically constructed imports
        // (`eval`/`Function`) that cannot be rejected by source inspection.
        // It also keeps a later QuickJS default change from silently turning
        // score text into filesystem or network access.
        rt.set_loader(RejectModuleResolver, RejectModuleLoader);
        let ctx = Context::full(&rt)?;
        let effect_transaction = Rc::new(RefCell::new(None));
        let effect_policy = Rc::new(Cell::new(EffectPolicy::NONE));
        #[cfg(feature = "hydra")]
        let hydra_chains = Rc::new(RefCell::new(crate::install::HydraChains::default()));
        let effect_policy_refusal = Rc::new(RefCell::new(None));
        let effect_policy_owner_depth = Rc::new(Cell::new(0));
        ctx.with(|ctx| -> rquickjs::Result<()> {
            let held = rquickjs::Array::new(ctx.clone())?;
            held.as_object().set_prototype(None)?;
            let active = rquickjs::Array::new(ctx.clone())?;
            active.as_object().set_prototype(None)?;
            active.set(0, rquickjs::Value::new_null(ctx.clone()))?;
            let stack = rquickjs::Array::new(ctx.clone())?;
            // This is host-only storage, not a JavaScript Array surface. A
            // normal Array prototype lets score code install indexed setters
            // that observe or swallow the temporary owner pushed while a
            // cross-turn `__pure` value is rematerialised. Keep Array's own
            // length/index mechanics while removing that mutable prototype
            // chain before any user code can run.
            stack.as_object().set_prototype(None)?;
            let score_candidate = rquickjs::Array::new(ctx.clone())?;
            score_candidate.as_object().set_prototype(None)?;
            let repl_tempo_surface = rquickjs::Array::new(ctx.clone())?;
            repl_tempo_surface.as_object().set_prototype(None)?;
            let slider_sets = rquickjs::Array::new(ctx.clone())?;
            slider_sets.as_object().set_prototype(None)?;
            let slider_values = rquickjs::Object::new(ctx.clone())?;
            slider_values.set_prototype(None)?;
            slider_sets.set(0, slider_values.clone())?;
            slider_sets.set(1, slider_values)?;
            slider_sets.set(2, rquickjs::Value::new_null(ctx.clone()))?;
            // Parallel host-only binding maps: active, candidate, last-good.
            // Decimal strings retain all 64 token bits without JS Number rounding.
            let bindings = rquickjs::Object::new(ctx.clone())?;
            bindings.set_prototype(None)?;
            slider_sets.set(3, bindings.clone())?;
            slider_sets.set(4, bindings)?;
            slider_sets.set(5, rquickjs::Value::new_null(ctx.clone()))?;
            let voicing_sets = rquickjs::Array::new(ctx.clone())?;
            voicing_sets.as_object().set_prototype(None)?;
            let previous = ctx.store_userdata(HostRoots {
                held,
                active,
                stack,
                score_candidate,
                repl_tempo_surface,
                slider_sets,
                voicing_sets,
                protected_globals: Rc::new(RefCell::new(BTreeSet::new())),
                pattern_transform_ir_cache: Rc::new(RefCell::new(
                    PatternTransformIrCache::default(),
                )),
                effects: EffectBoundaryState {
                    policy: effect_policy.clone(),
                    refusal: effect_policy_refusal.clone(),
                },
                heap: budget.clone(),
            })?;
            debug_assert!(previous.is_none(), "new runtime already had host roots");
            Ok(())
        })?;
        let deadline = std::rc::Rc::new(Cell::new(None::<std::time::Instant>));
        let interrupted = std::rc::Rc::new(Cell::new(false));
        let cancel_flag = std::rc::Rc::new(Cell::new(None::<*const std::sync::atomic::AtomicBool>));
        let cancelled = std::rc::Rc::new(Cell::new(false));

        // One evaluation deadline spans the initial setup call
        // and every runnable QuickJS job it queues. JS_ExecutePendingJob may be
        // interrupted safely only if its borrowed error-context pointer is
        // duplicated before Rust owns it and its pending exception is consumed
        // immediately. rquickjs 0.14 fixes borrowed-context ownership, but
        // its bool helper consumes job exceptions without reporting them.
        // A pending root Promise with an empty job
        // queue still is unsupported: it needs an external host-I/O source,
        // not more polling.
        {
            let d = deadline.clone();
            let i = interrupted.clone();
            let flag = cancel_flag.clone();
            let c = cancelled.clone();
            rt.set_interrupt_handler(Some(Box::new(move || {
                let cancellation_requested = flag.get().is_some_and(|pointer| {
                    // SAFETY: installed only for the duration of the call that
                    // borrowed the AtomicBool, and cleared by an RAII guard
                    // before that borrow can end.
                    unsafe { &*pointer }.load(std::sync::atomic::Ordering::Relaxed)
                });
                if cancellation_requested {
                    c.set(true);
                    return true;
                }
                match d.get() {
                    Some(t) if std::time::Instant::now() >= t => {
                        i.set(true);
                        true // interrupt
                    }
                    _ => false,
                }
            })));
        }
        Ok(Self {
            rt,
            ctx,
            core_settings: rustel_core::settings::RuntimeSettings::default(),
            ids: std::rc::Rc::new(Cell::new(0)),
            logs: std::rc::Rc::new(RefCell::new(Vec::new())),
            contained_query_errors_unlogged: Cell::new(false),
            timeline_state: rustel_core::TimelineState::default(),
            midi_in_bus: std::sync::Arc::new(rustel_core::midi_in::InputBus::new()),
            midi_in_handles: std::rc::Rc::new(RefCell::new(MidiInputHandles::default())),
            #[cfg(feature = "hydra")]
            hydra_chains,
            effect_transaction,
            effect_policy,
            hap_budget: Cell::new(rustel_core::DEFAULT_HAP_BUDGET),
            query_threw: RefCell::new(None),
            pattern_transform_ir_mode: Cell::new(PatternTransformIrMode::Auto),
            pattern_transform_ir_stats: Cell::new(PatternTransformIrStats::default()),
            heap: budget,
            deadline,
            interrupted,
            cancel_flag,
            cancelled,
            query_turn_depth: Cell::new(0),
            effect_query_depth: Cell::new(0),
            heap_boundary_depth: Cell::new(0),
            query_limit_millis: Cell::new(None),
            effect_policy_refusal,
            effect_policy_owner_depth,
            last_good_active: RefCell::new(None),
            pointer: None,
        })
    }

    /// A realm whose `mousex`/`mouseX` and `mousey`/`mouseY` read `pointer`
    /// at query time.
    pub fn with_pointer(pointer: rustel_core::host_value::Pointer) -> rquickjs::Result<Self> {
        let mut runtime = Self::new()?;
        runtime.pointer = Some(pointer);
        Ok(runtime)
    }

    pub fn run_gc(&self) {
        self.rt.run_gc();
    }

    /// Select callback-IR execution policy for tests and same-machine A/B.
    #[doc(hidden)]
    pub fn set_pattern_transform_ir_mode(&self, mode: PatternTransformIrMode) {
        self.pattern_transform_ir_mode.set(mode);
    }

    #[doc(hidden)]
    pub fn pattern_transform_ir_mode(&self) -> PatternTransformIrMode {
        self.pattern_transform_ir_mode.get()
    }

    /// Snapshot bounded producer-side callback-IR counters.
    #[doc(hidden)]
    pub fn pattern_transform_ir_stats(&self) -> PatternTransformIrStats {
        self.pattern_transform_ir_stats.get()
    }

    #[doc(hidden)]
    pub fn reset_pattern_transform_ir_stats(&self) {
        self.pattern_transform_ir_stats
            .set(PatternTransformIrStats::default());
    }

    pub fn take_logs(&self) -> Vec<String> {
        std::mem::take(&mut self.logs.borrow_mut())
    }

    /// Run `f` without logging the query errors a stack contains. A caller
    /// that reports such an error itself keeps it out of the console.
    pub fn with_contained_query_errors_unlogged<R>(&self, f: impl FnOnce() -> R) -> R {
        struct Restore<'a>(&'a Cell<bool>, bool);
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                self.0.set(self.1);
            }
        }
        let flag = &self.contained_query_errors_unlogged;
        let _restore = Restore(flag, flag.replace(true));
        f()
    }

    /// Run native pattern work with this runtime's module settings selected.
    pub fn with_runtime_settings<R>(&self, f: impl FnOnce() -> R) -> R {
        self.core_settings.with(f)
    }

    /// Snapshot the ambient native module settings into this runtime.
    ///
    /// This is the compatibility bridge for a Rust `Pattern` configured with
    /// the public `rustel-core` setters before Session takes ownership of it.
    pub fn inherit_current_runtime_settings(&self) {
        self.core_settings.inherit_current();
    }

    /// Capture ambient settings without changing this runtime.
    #[doc(hidden)]
    pub fn snapshot_ambient_runtime_settings(&self) -> rustel_core::settings::RuntimeSettings {
        rustel_core::settings::RuntimeSettings::snapshot_current()
    }

    /// Publish an accepted native candidate's settings to this runtime.
    #[doc(hidden)]
    pub fn adopt_runtime_settings(&self, settings: &rustel_core::settings::RuntimeSettings) {
        self.core_settings.replace_with(settings);
    }

    /// Snapshot this runtime's published settings without selecting them.
    #[doc(hidden)]
    pub fn snapshot_published_runtime_settings(&self) -> rustel_core::settings::RuntimeSettings {
        self.core_settings.detached_from(&self.core_settings)
    }

    /// Create an unpublished candidate in this runtime from a retained seed.
    #[doc(hidden)]
    pub fn seeded_candidate_runtime_settings(
        &self,
        seed: &rustel_core::settings::RuntimeSettings,
    ) -> rustel_core::settings::RuntimeSettings {
        self.core_settings.detached_from(seed)
    }

    /// Take over `bus` as this realm's MIDI input bus, with its ports and
    /// their controller values. Call it before the semantic bindings
    /// install: they open the score's ports on the bus held then.
    pub fn adopt_midi_input_bus(&mut self, bus: std::sync::Arc<rustel_core::midi_in::InputBus>) {
        self.midi_in_bus = bus;
    }

    /// The MIDI-input tables this runtime's scores have named.
    ///
    /// The host attaches listeners to these; evaluation only ever interns a
    /// name into them, which is why opening a device cannot stall a score.
    pub fn midi_input_bus(&self) -> std::sync::Arc<rustel_core::midi_in::InputBus> {
        std::sync::Arc::clone(&self.midi_in_bus)
    }

    /// Publish a successful score evaluation's complete MIDI-input set.
    ///
    /// Evaluation itself only allocates provisional local handles. The shared
    /// input bus changes here, after Session has accepted the graph and knows
    /// its generation, so a throw, timeout, failed live probe, or rollback
    /// cannot consume input capacity or disconnect the sounding score.
    pub fn commit_midi_input_effects(
        &self,
        effects: &mut ScoreEffects,
        generation: u64,
    ) -> Result<(), String> {
        let bindings = &effects.midi_inputs.bindings;
        let ports: Vec<std::sync::Arc<rustel_core::midi_in::InputPort>> = {
            let handles = self.midi_in_handles.borrow();
            bindings
                .iter()
                .map(|binding| {
                    handles.ports.get(&binding.handle).cloned().ok_or_else(|| {
                        format!(
                            "MIDI input handle {} for \"{}\" was not retained",
                            binding.handle, binding.selector
                        )
                    })
                })
                .collect::<Result<_, _>>()?
        };

        // The bus validates the entire candidate before replacing either of
        // its retained generations.
        self.midi_in_bus
            .commit_generation(generation, ports.clone())?;

        let mut handles = self.midi_in_handles.borrow_mut();
        let active: BTreeMap<String, u64> = bindings
            .iter()
            .map(|binding| (binding.selector.clone(), binding.handle))
            .collect();
        for (binding, port) in bindings.iter().zip(ports) {
            handles.ports.insert(binding.handle, port);
        }
        for handle in &effects.midi_inputs.provisional_handles {
            handles.provisional.remove(handle);
        }
        handles.active = active;
        let active_handles: BTreeSet<u64> = handles.active.values().copied().collect();
        let provisional = handles.provisional.clone();
        handles
            .ports
            .retain(|handle, _| active_handles.contains(handle) || provisional.contains(handle));
        effects.midi_inputs.resolved = true;
        Ok(())
    }

    /// Publish a generation for a score that named no MIDI inputs.
    pub fn commit_empty_midi_input_generation(&self, generation: u64) -> Result<(), String> {
        let mut effects = ScoreEffects::default();
        self.commit_midi_input_effects(&mut effects, generation)
    }

    /// Install `__gc()`, which collects from INSIDE JavaScript.
    ///
    /// Opt-in test support: nothing installs it automatically, so no product
    /// surface exposes `__gc`/`__cellsLive`. It is `pub` rather than
    /// `#[cfg(test)]` because the callers are integration tests in other
    /// crates, which compile against the ordinary library. The ownership
    /// hazard is reachable only mid-query: a bind callback can return a graph
    /// whose cell no JavaScript value references while Rust still holds its
    /// opaque `CallbackId`. Collecting between two callback invocations would
    /// sweep that cell unless the active query roots it.
    ///
    /// `run_gc` from Rust cannot reproduce that window: by the time the query
    /// returns, the dangerous frame has already closed.
    pub fn install_gc_binding(&self) -> Result<(), String> {
        let _settings = self.core_settings.bind();
        with_ctx(&self.ctx, |ctx| -> Result<(), String> {
            // `Ctx::run_gc`, not `Runtime::run_gc`: the latter takes the
            // runtime lock, which is already held while JavaScript is running,
            // and re-entering it aborts.
            let f = Function::new(ctx.clone(), |ctx: Ctx<'_>| ctx.run_gc())
                .map_err(|e| e.to_string())?;
            set_host_global(&ctx, "__gc", f).map_err(|e| e.to_string())?;
            // `__cellsLive()` reports the live callback-cell count from INSIDE
            // JavaScript. Reading it after a scheduling loop has finished says
            // nothing - the frame is torn down by then, so a leak and a
            // correctly-scoped frame look identical. The question "does this
            // grow without bound while playback continues?" can only be asked
            // during the loop, which means asking from a callback.
            let live =
                Function::new(ctx.clone(), || cells_live() as f64).map_err(|e| e.to_string())?;
            set_host_global(&ctx, "__cellsLive", live).map_err(|e| e.to_string())
        })
    }

    /// Run `f` under a CPU deadline enforced by QuickJS's interrupt handler.
    ///
    /// The budget covers one runnable JavaScript turn: synchronous setup plus
    /// every queued microtask driven before the queue becomes quiescent. It is
    /// not elapsed-time support for host-I/O promises. A root Promise that is
    /// still pending when QuickJS has no runnable job is refused explicitly.
    ///
    /// **Partial side effects persist.** There is one JS context and
    /// evaluation mutates live globals in place; if it throws halfway, what
    /// already happened stays. Rolling it back would change the language contract.
    pub fn with_deadline<R>(&self, budget: std::time::Duration, f: impl FnOnce() -> R) -> R {
        let _settings = self.core_settings.bind();
        struct Guard(
            std::rc::Rc<Cell<Option<std::time::Instant>>>,
            Option<std::time::Instant>,
        );
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.set(self.1);
            }
        }
        let prev = self.deadline.get();
        self.interrupted.set(false);
        // A NESTED bounded run must never buy more time than the one that
        // contains it. `chop(32).slow(0.001)` re-enters queryArc ~1000× per
        // cycle from a JS callback; with a fresh budget each time the
        // a fresh budget each time would multiply the allowed work. Inheriting
        // the tighter deadline keeps one save bounded by one budget.
        let deadline = match prev {
            Some(outer) => (std::time::Instant::now() + budget).min(outer),
            None => std::time::Instant::now() + budget,
        };
        self.deadline.set(Some(deadline));
        let _g = Guard(self.deadline.clone(), prev);
        // The same deadline bounds NATIVE query recursion: QuickJS's
        // interrupt handler only fires while JavaScript runs, and a
        // pattern can spend minutes in Rust below every hap budget.
        rustel_core::with_query_deadline(deadline, f)
    }

    /// Whether the last deadline-bounded run was interrupted.
    pub fn was_interrupted(&self) -> bool {
        self.interrupted.get()
    }

    /// Refresh the structural reason for a bounded evaluation to stop even
    /// while Rust is validating the returned wrapper between QuickJS calls.
    /// The engine interrupt handler checks the same two inputs while executing
    /// JavaScript; this exit check makes the deadline cover validation through
    /// the final active-graph publication as well.
    pub(super) fn bounded_evaluation_interrupted(&self) -> bool {
        let cancellation_requested = self.cancel_flag.get().is_some_and(|pointer| {
            // SAFETY: a pointer is installed only while the public bounded
            // evaluator owns the borrowed AtomicBool, under an RAII guard.
            unsafe { &*pointer }.load(std::sync::atomic::Ordering::Relaxed)
        });
        if cancellation_requested {
            self.cancelled.set(true);
            return true;
        }
        if self
            .deadline
            .get()
            .is_some_and(|at| std::time::Instant::now() >= at)
        {
            self.interrupted.set(true);
            return true;
        }
        false
    }

    pub fn jobs_pending(&self) -> bool {
        self.rt.is_job_pending()
    }

    /// Evaluate arbitrary JS source. Used by tests and by prebake.
    pub fn eval(&self, src: &str) -> Result<(), String> {
        let _settings = self.core_settings.bind();
        let effect_policy_owner = EffectPolicyOwner::enter(self);
        let _effect_transaction =
            EffectTransactionOwner::enter(self, EffectPolicy::NONE).map_err(|e| e.to_string())?;
        let result = with_ctx(&self.ctx, |ctx| {
            ctx.eval::<rquickjs::Value, _>(src)
                .map(|_| ())
                .map_err(|error| describe_js_error(&ctx, error))
        });
        let policy = effect_policy_owner.policy();
        match (result, policy) {
            (_, Some(policy)) => Err(policy),
            (result, None) => result,
        }
    }

    /// Read a global as a string - for asserting on descriptors and identity.
    pub fn get_string(&self, name: &str) -> Option<String> {
        let _settings = self.core_settings.bind();
        with_ctx(&self.ctx, |ctx| ctx.globals().get::<_, String>(name).ok())
    }

    /// Read a global as an f64 - for asserting on partial side effects.
    pub fn get_number(&self, name: &str) -> Option<f64> {
        let _settings = self.core_settings.bind();
        with_ctx(&self.ctx, |ctx| ctx.globals().get::<_, f64>(name).ok())
    }

    /// The bounded native MIDI-map registry as deterministic JSON, or `None`
    /// when no map has been registered.
    ///
    /// This is part of the public host API. The native settings registry is
    /// the source of truth, so control keys in this view use their canonical
    /// names and entries use the complete `{ccn,min,max,exp}` shape.
    pub fn midi_maps_json(&self) -> Option<String> {
        let _settings = self.core_settings.bind();
        rustel_core::midimap::midi_maps_json()
    }

    /// Replace one already-registered slider cell without evaluating score
    /// text. The private rooted object cannot be redirected by replacing the
    /// public `sliderValues` compatibility global, and only plain writable
    /// finite-number data properties accept a host write. Anything else - an
    /// unknown id, or an accessor/exotic cell planted by the score - is
    /// reported as absent, so host writes never execute score JavaScript.
    pub fn set_slider_value(&self, id: &str, value: f64) -> Result<bool, String> {
        if id.is_empty()
            || id.len() > 256
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b':' | b'-'))
        {
            return Err("slider id is invalid".into());
        }
        if !value.is_finite() {
            return Err("slider value must be finite".into());
        }
        let _settings = self.core_settings.bind();
        with_ctx(&self.ctx, |ctx| -> Result<bool, String> {
            let sets = host_slider_sets(&ctx).map_err(|error| error.to_string())?;
            let values: rquickjs::Object = sets
                .get(0)
                .map_err(|error| describe_js_error(&ctx, error))?;
            if !slider_cell_is_plain_number(&ctx, &values, id)? {
                return Ok(false);
            }
            values
                .set(id, value)
                .map_err(|error| describe_js_error(&ctx, error))?;
            Ok(true)
        })
    }

    /// Raise a native panic that score JavaScript caught as an exception.
    ///
    /// QuickJS hands such a panic back only when the next exception reaches
    /// the host, which can be any later call. A recovery boundary calls this
    /// before it returns, so the panic surfaces where it can be recovered.
    pub fn raise_caught_native_panic(&self) {
        with_ctx(&self.ctx, |ctx| {
            if ctx.eval::<(), _>("throw 0").is_err() {
                let _ = ctx.catch();
            }
        });
    }

    /// Every active slider cell holding a plain finite number, read without
    /// invoking score code.
    pub fn slider_values(&self) -> Result<Vec<(String, f64)>, String> {
        let _settings = self.core_settings.bind();
        with_ctx(&self.ctx, |ctx| -> Result<Vec<(String, f64)>, String> {
            let sets = host_slider_sets(&ctx).map_err(|error| error.to_string())?;
            let values: rquickjs::Object = sets
                .get(0)
                .map_err(|error| describe_js_error(&ctx, error))?;
            let mut cells = Vec::new();
            for id in values.keys::<String>() {
                let id = id.map_err(|error| describe_js_error(&ctx, error))?;
                if !slider_cell_is_plain_number(&ctx, &values, &id)? {
                    continue;
                }
                let value = values
                    .get::<_, f64>(id.as_str())
                    .map_err(|error| describe_js_error(&ctx, error))?;
                if value.is_finite() {
                    cells.push((id, value));
                }
            }
            Ok(cells)
        })
    }

    /// Read one active query-time slider cell without invoking score code.
    ///
    /// Only plain writable finite-number data properties are read; accessor
    /// cells planted by the score read as `None` instead of executing their
    /// getters.
    pub fn slider_value(&self, id: &str) -> Result<Option<f64>, String> {
        if id.is_empty()
            || id.len() > 256
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b':' | b'-'))
        {
            return Err("slider id is invalid".into());
        }
        let _settings = self.core_settings.bind();
        with_ctx(&self.ctx, |ctx| -> Result<Option<f64>, String> {
            let sets = host_slider_sets(&ctx).map_err(|error| error.to_string())?;
            let values: rquickjs::Object = sets
                .get(0)
                .map_err(|error| describe_js_error(&ctx, error))?;
            if !slider_cell_is_plain_number(&ctx, &values, id)? {
                return Ok(None);
            }
            let value = values
                .get::<_, f64>(id)
                .map_err(|error| describe_js_error(&ctx, error))?;
            value
                .is_finite()
                .then_some(Some(value))
                .ok_or_else(|| "slider cell is not finite".into())
        })
    }

    /// Opaque audio binding for a plain numeric cell in the active score.
    /// Neither this lookup nor its validation can invoke score getters.
    pub fn slider_binding(&self, id: &str) -> Option<u64> {
        if id.is_empty()
            || id.len() > 256
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b':' | b'-'))
        {
            return None;
        }
        with_ctx(&self.ctx, |ctx| {
            let sets = host_slider_sets(&ctx).ok()?;
            let values: rquickjs::Object = sets.get(0).ok()?;
            if !slider_cell_is_plain_number(&ctx, &values, id).ok()? {
                return None;
            }
            let bindings: rquickjs::Object = sets.get(3).ok()?;
            bindings
                .get::<_, String>(id)
                .ok()?
                .parse::<u64>()
                .ok()
                .filter(|id| *id != 0)
        })
    }

    pub(crate) fn begin_slider_candidate(&self) -> Result<(), String> {
        with_ctx(&self.ctx, |ctx| -> Result<(), String> {
            let values = rquickjs::Object::new(ctx.clone()).map_err(|error| error.to_string())?;
            values
                .set_prototype(None)
                .map_err(|error| error.to_string())?;
            let bindings = rquickjs::Object::new(ctx.clone()).map_err(|error| error.to_string())?;
            bindings
                .set_prototype(None)
                .map_err(|error| error.to_string())?;
            let sets = host_slider_sets(&ctx).map_err(|error| error.to_string())?;
            sets.set(4, bindings)
                .map_err(|error| describe_js_error(&ctx, error))?;
            sets.set(1, values)
                .map_err(|error| describe_js_error(&ctx, error))
        })
    }

    pub(super) fn discard_slider_candidate(&self) {
        with_ctx(&self.ctx, |ctx| {
            if let Ok(sets) = host_slider_sets(&ctx)
                && let Ok(active) = sets.get::<rquickjs::Value>(0)
            {
                let _ = sets.set(1, active);
                if let Ok(bindings) = sets.get::<rquickjs::Value>(3) {
                    let _ = sets.set(4, bindings);
                }
            }
        });
    }

    pub(crate) fn begin_voicing_candidate(&self) -> Result<(), String> {
        with_ctx(&self.ctx, |ctx| -> Result<(), String> {
            let sets = host_voicing_sets(&ctx).map_err(|error| error.to_string())?;
            // Direct JsRuntime embedders may evaluate before installing the
            // optional voicings compatibility surface. There is no registry
            // mirror to isolate in that configuration.
            if sets.len() < 5 {
                return Ok(());
            }
            let fork: Function = sets
                .get(3)
                .map_err(|error| describe_js_error(&ctx, error))?;
            let select: Function = sets
                .get(4)
                .map_err(|error| describe_js_error(&ctx, error))?;
            let candidate: rquickjs::Value = fork
                .call(())
                .map_err(|error| describe_js_error(&ctx, error))?;
            sets.set(1, candidate.clone())
                .map_err(|error| describe_js_error(&ctx, error))?;
            if let Err(error) = select.call::<_, ()>((candidate,)) {
                let primary = describe_js_error(&ctx, error);
                let mut failures = vec![primary];
                match sets.get::<rquickjs::Value>(0) {
                    Ok(active) => {
                        if let Err(restore) = select.call::<_, ()>((active.clone(),)) {
                            failures.push(format!(
                                "active-registry reselection failed: {}",
                                describe_js_error(&ctx, restore)
                            ));
                        }
                        if let Err(restore) = sets.set(1, active) {
                            failures.push(format!("candidate-root restore failed: {restore}"));
                        }
                    }
                    Err(restore) => {
                        failures.push(format!("active-registry restore failed: {restore}"));
                    }
                }
                return Err(failures.join("; "));
            }
            Ok(())
        })
    }

    pub(super) fn discard_voicing_candidate(&self) {
        with_ctx(&self.ctx, |ctx| {
            let Ok(sets) = host_voicing_sets(&ctx) else {
                return;
            };
            if sets.len() < 5 {
                return;
            }
            let Ok(active) = sets.get::<rquickjs::Value>(0) else {
                return;
            };
            if let Ok(select) = sets.get::<Function>(4) {
                let _ = select.call::<_, ()>((active.clone(),));
            }
            let _ = sets.set(1, active);
        });
    }

    pub(super) fn commit_score_candidates(&self) -> Result<(), String> {
        self.commit_score_candidates_with_hook(|| Ok(()))
    }

    /// Publish the slider target and `voicings()` registry as one host
    /// transaction. `after_voicing` is a test seam at the exact boundary
    /// between the two root writes; production passes an infallible no-op.
    pub(crate) fn commit_score_candidates_with_hook(
        &self,
        after_voicing: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        with_ctx(&self.ctx, |ctx| -> Result<(), String> {
            let slider_sets = host_slider_sets(&ctx).map_err(|error| error.to_string())?;
            let old_sliders: rquickjs::Value = slider_sets
                .get(0)
                .map_err(|error| describe_js_error(&ctx, error))?;
            let candidate_sliders: rquickjs::Value = slider_sets
                .get(1)
                .map_err(|error| describe_js_error(&ctx, error))?;
            let old_bindings: rquickjs::Value = slider_sets
                .get(3)
                .map_err(|error| describe_js_error(&ctx, error))?;
            let candidate_bindings: rquickjs::Value = slider_sets
                .get(4)
                .map_err(|error| describe_js_error(&ctx, error))?;

            let voicing_sets = host_voicing_sets(&ctx).map_err(|error| error.to_string())?;
            let voicing = if voicing_sets.len() >= 5 {
                Some((
                    voicing_sets
                        .get::<rquickjs::Value>(0)
                        .map_err(|error| describe_js_error(&ctx, error))?,
                    voicing_sets
                        .get::<rquickjs::Value>(1)
                        .map_err(|error| describe_js_error(&ctx, error))?,
                    voicing_sets
                        .get::<Function>(4)
                        .map_err(|error| describe_js_error(&ctx, error))?,
                ))
            } else {
                None
            };

            let rollback = || -> Result<(), String> {
                let mut failures = Vec::new();
                if let Err(error) = slider_sets.set(0, old_sliders.clone()) {
                    failures.push(format!("slider active root: {error}"));
                }
                if let Err(error) = slider_sets.set(1, old_sliders.clone()) {
                    failures.push(format!("slider candidate root: {error}"));
                }
                for slot in [3, 4] {
                    if let Err(error) = slider_sets.set(slot, old_bindings.clone()) {
                        failures.push(format!("slider binding root {slot}: {error}"));
                    }
                }
                if let Some((old_registry, _, select)) = &voicing {
                    if let Err(error) = select.call::<_, ()>((old_registry.clone(),)) {
                        failures.push(format!(
                            "voicing registry selection: {}",
                            describe_js_error(&ctx, error)
                        ));
                    }
                    if let Err(error) = voicing_sets.set(0, old_registry.clone()) {
                        failures.push(format!("voicing active root: {error}"));
                    }
                    if let Err(error) = voicing_sets.set(1, old_registry.clone()) {
                        failures.push(format!("voicing candidate root: {error}"));
                    }
                }
                if failures.is_empty() {
                    Ok(())
                } else {
                    Err(failures.join("; "))
                }
            };
            let refuse = |primary: String| match rollback() {
                Ok(()) => Err(primary),
                Err(rollback) => Err(format!("{primary}; candidate rollback failed: {rollback}")),
            };

            if let Some((_, candidate_registry, _)) = &voicing
                && let Err(error) = voicing_sets.set(0, candidate_registry.clone())
            {
                return refuse(format!("voicing candidate publication failed: {error}"));
            }
            if let Err(error) = after_voicing() {
                return refuse(error);
            }
            if let Err(error) = slider_sets.set(0, candidate_sliders) {
                return refuse(format!("slider candidate publication failed: {error}"));
            }
            if let Err(error) = slider_sets.set(3, candidate_bindings) {
                return refuse(format!("slider binding publication failed: {error}"));
            }
            Ok(())
        })
    }

    pub(super) fn effect_policy_pending(&self) -> bool {
        self.effect_policy_refusal.borrow().is_some()
    }

    pub(super) fn effect_policy_message(&self) -> String {
        self.effect_policy_refusal
            .borrow()
            .clone()
            .unwrap_or_else(|| "query-time host effect was refused".into())
    }

    /// Allocate a globally unique callback id.
    pub fn alloc_id(&self) -> CallbackId {
        // One allocator per runtime, shared by both wrapper families: a
        // separate counter for each would hand the same id to both, which is
        // exactly the wrong-callback invocation the identity rules prevent.
        let id = self.ids.get();
        self.ids.set(id + 1);
        id
    }

    pub fn builder(&self) -> GraphBuilder {
        GraphBuilder::default()
    }

    pub(super) fn install(
        &self,
        slot: Slot,
        b: &GraphBuilder,
        pattern: Pattern,
    ) -> Result<usize, String> {
        let _settings = self.core_settings.bind();
        with_ctx(&self.ctx, |ctx| -> Result<usize, String> {
            // 1. Cells inherited from composed graphs. The new wrapper holds
            //    its own reference, so the source may be released right after.
            let mut ids: Vec<CallbackId> = Vec::new();
            let mut cells: Vec<rquickjs::Class<CallbackCell>> = Vec::new();
            for (slot, index) in &b.imports {
                let src_w = Self::wrapper_at(&ctx, *slot, *index)?;
                let bw = src_w.borrow();
                for (id, cell) in bw.ids.iter().zip(bw.cells.iter()) {
                    if !ids.contains(id) {
                        ids.push(*id);
                        cells.push(cell.clone());
                    }
                }
            }
            // 2. Evaluate this graph's own factories.
            let mut factories = Vec::with_capacity(b.sources.len());
            for (id, src) in &b.sources {
                let f = ctx
                    .eval::<Function, _>(src.as_str())
                    .map_err(|e| e.to_string())?;
                CELLS_CREATED.with(|c| c.set(c.get() + 1));
                let cell =
                    rquickjs::Class::instance(ctx.clone(), CallbackCell::function(f.clone()))
                        .map_err(|e| e.to_string())?;
                ids.push(*id);
                cells.push(cell);
                factories.push(f);
            }
            let own_start = cells.len() - factories.len();

            // 3. Every id the graph can reach must have a cell. A missing one
            //    is a hard error: silently invoking the wrong callback (or
            //    none) is exactly the failure global ids exist to prevent.
            if !pattern.purity().opaque {
                for id in pattern.reachable_callbacks() {
                    if !ids.contains(id) {
                        return Err(format!(
                            "callback {id} is reachable from this graph but no cell \
                             was provided; import the wrapper that owns it"
                        ));
                    }
                }
            }

            let wrapper = rquickjs::Class::instance(
                ctx.clone(),
                PatternWrapper {
                    pattern,
                    ids,
                    cells,
                },
            )
            .map_err(|e| e.to_string())?;

            // 4. Bind each NEW callback to its own wrapper, completing the
            //    cross-heap cycle the collector must reclaim. Imported cells keep their
            //    original binding.
            {
                let own: Vec<_> = wrapper.borrow().cells[own_start..].to_vec();
                for (cell, factory) in own.iter().zip(factories.iter()) {
                    let bound: Function = factory
                        .call((wrapper.clone(),))
                        .map_err(|e| e.to_string())?;
                    cell.borrow_mut().payload = CellPayload::Function(bound);
                }
            }
            match slot {
                Slot::Active => {
                    host_active(&ctx)
                        .map_err(|error| error.to_string())?
                        .set(0, wrapper)
                        .map_err(|e| e.to_string())?;
                    Ok(0)
                }
                Slot::Held => {
                    let arr = host_held(&ctx).map_err(|error| error.to_string())?;
                    let n = arr.len();
                    arr.set(n, wrapper).map_err(|e| e.to_string())?;
                    Ok(n)
                }
            }
        })
    }

    /// Evaluate: build a graph and make it the active one, **replacing**
    /// whatever was active before. The previous graph becomes unreachable.
    pub fn set_active(&self, b: &GraphBuilder, pattern: Pattern) -> Result<(), String> {
        // IDs are allocated by `GraphBuilder::callback` via `alloc_id`.
        // Advancing `next_id` here as well would double-count them, so
        // allocation happens in exactly one place.
        self.install(Slot::Active, b, pattern).map(|_| ())
    }

    /// Hold a graph across evaluations - `globalThis.p = s("bd")`.
    pub fn hold(&self, b: &GraphBuilder, pattern: Pattern) -> Result<usize, String> {
        self.install(Slot::Held, b, pattern)
    }

    /// Reassign a held slot - `globalThis.p = <something else>`.
    ///
    /// The previous wrapper becomes unreachable, so its cells, functions and
    /// Rust nodes are all collectable. Without this the held array is
    /// append-only, i.e. another permanent root.
    pub fn replace_held(
        &self,
        index: usize,
        b: &GraphBuilder,
        pattern: Pattern,
    ) -> Result<(), String> {
        let idx = self.install(Slot::Held, b, pattern)?;
        let _settings = self.core_settings.bind();
        with_ctx(&self.ctx, |ctx| -> Result<(), String> {
            let arr = host_held(&ctx).map_err(|error| error.to_string())?;
            let w: rquickjs::Value = arr.get(idx).map_err(|e| e.to_string())?;
            arr.set(index, w).map_err(|e| e.to_string())?;
            // Drop the temporary tail slot the install appended.
            arr.set(idx, rquickjs::Value::new_null(ctx.clone()))
                .map_err(|e| e.to_string())
        })
    }

    /// `delete globalThis.p` - release a held graph entirely.
    pub fn release_held(&self, index: usize) -> Result<(), String> {
        let _settings = self.core_settings.bind();
        with_ctx(&self.ctx, |ctx| -> Result<(), String> {
            let arr = host_held(&ctx).map_err(|error| error.to_string())?;
            arr.set(index, rquickjs::Value::new_null(ctx.clone()))
                .map_err(|e| e.to_string())
        })
    }

    pub(super) fn wrapper_at<'js>(
        ctx: &Ctx<'js>,
        slot: Slot,
        index: usize,
    ) -> Result<rquickjs::Class<'js, PatternWrapper<'js>>, String> {
        match slot {
            Slot::Active => host_active(ctx)
                .map_err(|error| error.to_string())?
                .get(0)
                .map_err(|_| "active slot is empty".to_string()),
            Slot::Held => {
                let arr = host_held(ctx).map_err(|error| error.to_string())?;
                arr.get(index)
                    .map_err(|_| format!("held slot {index} is empty"))
            }
        }
    }

    /// Run `f` with the active graph's callback host, bridge frame and query
    /// stack installed.
    ///
    /// `query` sets all three up around its own `query_arc_sorted`. The
    /// scheduler instead receives a bare `Pattern` and queries it on its own
    /// clock, so callback-bearing graphs need the same scope here before they
    /// can call `host_call_*`.
    ///
    /// Scope this to ONE `Scheduler::tick` query, not to a whole scheduling
    /// loop. The frame is a GC root for every cell a query-time callback
    /// creates, so a loop-wide frame retains them all for the loop's duration -
    /// linear growth in playback length, and unbounded for a watch process that
    /// never returns. `drain_due` needs none of this: it only filters the event
    /// queue. See `scheduler_callback_cells_do_not_grow_with_playback_length`.
    ///
    /// Standalone legacy calls use [`DEFAULT_QUERY_JS_BUDGET`]. If this is
    /// reached from an already-bounded query or evaluation, it inherits that
    /// exact deadline, cancellation pointer, heap flag and job ownership.
    pub fn with_active_scope<R>(&self, f: impl FnOnce() -> R) -> Result<R, QueryError> {
        let _settings = self.core_settings.bind();
        self.with_legacy_query_turn(|| self.with_active_scope_inner(f))
    }

    pub(super) fn with_active_scope_inner<R>(
        &self,
        f: impl FnOnce() -> R,
    ) -> Result<R, QueryError> {
        self.sync_host_voicing_default()?;
        let _scope = QueryScope::push_active_native(self).map_err(QueryError::Message)?;
        Ok(with_ctx(&self.ctx, |_ctx| {
            let frame = BridgeFrame::new(self.ids.clone());
            let _bridge = BridgeScope::push(&frame);
            rustel_core::with_callback_host(self, f)
        }))
    }

    /// Run one impure scheduler tick under one monotonic elapsed-time QuickJS
    /// interrupt boundary and a caller-owned cancellation flag.
    ///
    /// A callback interruption is published into core's typed refusal channel
    /// before the scheduler can advance its cursor. Query-created jobs are
    /// refused and discarded rather than executed, and reentrant queries
    /// inherit the same absolute deadline.
    pub fn with_active_scope_cancellable<R>(
        &self,
        budget: std::time::Duration,
        cancellation: &std::sync::atomic::AtomicBool,
        f: impl FnOnce() -> R,
    ) -> Result<R, QueryError> {
        let _settings = self.core_settings.bind();
        self.with_bounded_query_turn(budget, cancellation, false, || {
            self.with_active_scope_inner(f)
        })
    }

    /// Whether the active graph needs [`JsRuntime::with_active_scope`] to be
    /// queried - i.e. it is not classified pure.
    pub fn active_needs_host(&self) -> bool {
        let _settings = self.core_settings.bind();
        rustel_core::voicings::selected_default_voicings_is_host_owned()
            || self
                .active_pattern_unwrapped()
                .is_some_and(|pattern| !pattern.is_pure())
    }

    pub(super) fn sync_host_voicing_default(&self) -> Result<(), QueryError> {
        if !rustel_core::voicings::selected_default_voicings_is_host_owned() {
            return Ok(());
        }
        with_ctx(&self.ctx, |ctx| -> Result<(), QueryError> {
            let sets =
                host_voicing_sets(&ctx).map_err(|error| QueryError::Message(error.to_string()))?;
            if sets.len() < 6 {
                return Err(QueryError::Message(
                    "object-valued voicing default has no host sync surface".into(),
                ));
            }
            let sync: Function = sets
                .get(5)
                .map_err(|error| QueryError::Message(error.to_string()))?;
            sync.call::<_, ()>(())
                .map_err(|error| QueryError::Message(describe_js_error(&ctx, error)))
        })
    }

    /// The QuickJS heap ceiling currently in force, in bytes.
    pub fn memory_limit(&self) -> usize {
        self.heap.limit()
    }

    /// Bytes currently live on the QuickJS heap, as this allocator accounts
    /// them.
    pub fn heap_live(&self) -> usize {
        self.heap.live()
    }

    /// Lower the heap ceiling. The ceiling is monotonic: it can only go down.
    ///
    /// Tests use this to cause exhaustion at a small size: a test at 512 MiB
    /// would have to allocate 512 MiB. An embedder on a small device can also
    /// set a tighter bound than this build's default.
    ///
    /// The setter refuses three values:
    ///
    /// * **Zero.** QuickJS reads a limit of zero as unlimited, so accepting it
    ///   would remove the ceiling.
    /// * **Any increase.** Allowing a caller to lower the limit and then raise
    ///   it again would violate the monotonic contract.
    /// * **A value below the live heap.** Every later allocation would be
    ///   denied, and the ceiling cannot be raised again.
    pub fn set_memory_limit(&self, bytes: usize) -> Result<(), String> {
        self.heap.lower_to(bytes)
    }

    /// `query` with an explicit hap-count budget.
    ///
    /// Public so the budget can be exercised through the REAL host at a size
    /// that costs nothing: proving the callback-array check at the 5,000,000
    /// default would mean a JavaScript callback returning five million objects.
    /// This argument does not alter the elapsed-time policy: the call still
    /// uses [`DEFAULT_QUERY_JS_BUDGET`] or inherits an enclosing boundary.
    pub fn query_with_budget(
        &self,
        slot: Slot,
        index: usize,
        begin: Fraction,
        end: Fraction,
        budget: u64,
    ) -> Result<Vec<Hap>, QueryError> {
        let previous = self.hap_budget.replace(budget);
        let out = self.query(slot, index, begin, end);
        self.hap_budget.set(previous);
        out
    }

    /// Run one outer query or scheduler tick, clearing and then translating the
    /// allocator's flag.
    ///
    /// AN OUTER BOUNDARY. Cleared once on the way in, so a refusal left by an
    /// earlier operation cannot poison this one, and translated on the way out
    /// before `queryArc`'s error handling can turn it into silence. Bounded
    /// setup/score evaluation owns this boundary too. A nested legacy query
    /// observes that owner's flag rather than clearing it; a raw public
    /// `with_deadline` still receives the legacy query's ordinary heap owner.
    pub(super) fn with_heap_boundary<R>(
        &self,
        f: impl FnOnce() -> Result<R, QueryError>,
    ) -> Result<R, QueryError> {
        if self.heap_boundary_depth.get() > 0 {
            return f();
        }
        struct HeapBoundaryGuard<'a> {
            depth: &'a Cell<usize>,
            previous: usize,
        }
        impl Drop for HeapBoundaryGuard<'_> {
            fn drop(&mut self) {
                self.depth.set(self.previous);
            }
        }
        let previous = self.heap_boundary_depth.replace(1);
        let _guard = HeapBoundaryGuard {
            depth: &self.heap_boundary_depth,
            previous,
        };
        let _ = alloc::take_heap_exhausted();
        let out = f();
        if alloc::take_heap_exhausted() {
            // Whatever else happened, an allocation was denied: report the
            // refusal rather than an empty result or a user-throw message.
            return Err(QueryError::Limit(rustel_core::QueryLimit::HostMemory));
        }
        out
    }

    /// Give a source-compatible query entry point the default finite policy
    /// without stealing an enclosing resource boundary.
    ///
    /// Three owners can already be active while `queryHeld` reenters here:
    /// another bounded query turn, bounded setup/score evaluation (which owns
    /// the heap flag and often a cancellation pointer), or the public raw
    /// `with_deadline` helper. Only a genuinely standalone call may preflight
    /// jobs and install the default two-second turn. A raw deadline has no heap
    /// owner, so it retains the legacy query's heap boundary without replacing
    /// the earlier absolute deadline.
    pub(super) fn with_legacy_query_turn<R>(
        &self,
        f: impl FnOnce() -> Result<R, QueryError>,
    ) -> Result<R, QueryError> {
        let effect_policy_owner = EffectPolicyOwner::enter(self);
        let _effect_policy = EffectPolicyScope::enter(self, EffectPolicy::NONE);
        let _effect_query = EffectQueryScope::enter(self);
        let out = if self.query_turn_depth.get() > 0
            || self.heap_boundary_depth.get() > 0
            || self.cancel_flag.get().is_some()
        {
            f()
        } else if self.deadline.get().is_some() {
            self.with_heap_boundary(f)
        } else {
            self.with_bounded_query_turn(
                DEFAULT_QUERY_JS_BUDGET,
                &DEFAULT_QUERY_CANCELLATION,
                true,
                f,
            )
        };
        let policy = effect_policy_owner.newly_raised_policy();
        match out {
            Err(QueryError::Limit(
                rustel_core::QueryLimit::JsCpuDeadline { .. }
                | rustel_core::QueryLimit::QueryDeadline { .. },
            )) if policy.is_some() => Err(QueryError::Policy(
                policy.expect("checked host-effect policy refusal"),
            )),
            Err(QueryError::Limit(limit)) => Err(QueryError::Limit(limit)),
            _ if policy.is_some() => Err(QueryError::Policy(
                policy.expect("checked host-effect policy refusal"),
            )),
            out => out,
        }
    }

    /// Own the resource state for one OUTER synchronous query turn.
    ///
    /// `queryHeld` can reenter [`Self::query`] from inside a JavaScript
    /// callback. Such nested queries inherit this boundary: they must not
    /// clear the outer heap flag, replace its cancellation pointer, reset its
    /// deadline, or discard a job the outer callback just queued. Only depth
    /// zero performs preflight and cleanup.
    pub(super) fn with_bounded_query_turn<R>(
        &self,
        budget: std::time::Duration,
        cancellation: &std::sync::atomic::AtomicBool,
        translate_latched: bool,
        f: impl FnOnce() -> Result<R, QueryError>,
    ) -> Result<R, QueryError> {
        if self.query_turn_depth.get() > 0 {
            return f();
        }

        // Join the explicit operation tree. This query clears the latch only
        // when it is genuinely top-level; inside raw/prebake/score evaluation
        // it can report a policy but cannot consume it.
        let effect_policy_owner = EffectPolicyOwner::enter(self);
        let _effect_policy = EffectPolicyScope::enter(self, EffectPolicy::NONE);
        let _effect_query = EffectQueryScope::enter(self);

        // A stale job has no attributable query owner. Always discard it, but
        // preserve caller cancellation as the dominant public reason.
        let stale_jobs = with_ctx(&self.ctx, |ctx| discard_pending_jobs(&ctx));
        if cancellation.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(QueryError::Limit(rustel_core::QueryLimit::Cancelled));
        }
        let millis = budget.as_millis().min(u128::from(u64::MAX)) as u64;
        if budget.is_zero() {
            return Err(QueryError::Limit(rustel_core::QueryLimit::JsCpuDeadline {
                millis,
            }));
        }
        if stale_jobs > 0 {
            return Err(QueryError::Limit(rustel_core::QueryLimit::JsPendingJobs));
        }

        struct QueryTurnGuard<'runtime, 'flag> {
            rt: &'runtime JsRuntime,
            previous_depth: usize,
            previous_millis: Option<u64>,
            previous_cancel: Option<*const std::sync::atomic::AtomicBool>,
            _cancellation: &'flag std::sync::atomic::AtomicBool,
        }
        impl Drop for QueryTurnGuard<'_, '_> {
            fn drop(&mut self) {
                // Cleanup never executes jobs or JavaScript and is therefore
                // safe after a deadline/cancellation interrupt and on unwind.
                with_ctx(&self.rt.ctx, |ctx| {
                    let _ = discard_pending_jobs(&ctx);
                });
                self.rt.cancel_flag.set(self.previous_cancel);
                self.rt.query_limit_millis.set(self.previous_millis);
                self.rt.query_turn_depth.set(self.previous_depth);
            }
        }

        let previous_depth = self.query_turn_depth.replace(1);
        let previous_millis = self.query_limit_millis.replace(Some(millis));
        let previous_cancel = self
            .cancel_flag
            .replace(Some(cancellation as *const std::sync::atomic::AtomicBool));
        self.cancelled.set(false);
        let _turn = QueryTurnGuard {
            rt: self,
            previous_depth,
            previous_millis,
            previous_cancel,
            _cancellation: cancellation,
        };

        // Clear once for the whole turn. Heap classification happens below,
        // after cancellation/deadline latches are known; otherwise the generic
        // heap wrapper can overwrite a scheduler's structurally published
        // Cancelled/JsCpuDeadline refusal with HostMemory.
        let _ = alloc::take_heap_exhausted();
        let out = self.with_deadline(budget, || rustel_core::with_cancellation(cancellation, f));
        let heap_exhausted = alloc::take_heap_exhausted();
        // Every JS host entry publishes these reasons structurally before the
        // core query returns. These outer checks translate the same LATCHED
        // reason for direct callers; they deliberately do not inspect the
        // current clock or cancellation flag after `f`, which could
        // retroactively reject an already-committed scheduler tick.
        let jobs = with_ctx(&self.ctx, |ctx| discard_pending_jobs(&ctx));
        let effect_policy = effect_policy_owner.newly_raised_policy();
        if !translate_latched {
            // Scheduler ticks carry the structural refusal in `TickStatus` and
            // retain their typed reason. Let Session distinguish Stop's
            // established partial-success semantics from resource failure.
            // Direct queries have no such status value and translate below.
            if self.cancelled.get() || self.was_interrupted() {
                return out;
            }
            if heap_exhausted {
                return Err(QueryError::Limit(rustel_core::QueryLimit::HostMemory));
            }
            if effect_policy.is_some() && jobs > 0 {
                return Err(QueryError::Limit(rustel_core::QueryLimit::JsPendingJobs));
            }
            if let Some(policy) = effect_policy {
                return Err(QueryError::Policy(policy));
            }
            return out;
        }
        if self.cancelled.get() {
            Err(QueryError::Limit(rustel_core::QueryLimit::Cancelled))
        } else if self.was_interrupted() {
            Err(QueryError::Limit(rustel_core::QueryLimit::JsCpuDeadline {
                millis,
            }))
        } else if heap_exhausted {
            Err(QueryError::Limit(rustel_core::QueryLimit::HostMemory))
        } else if jobs > 0 {
            Err(QueryError::Limit(rustel_core::QueryLimit::JsPendingJobs))
        } else if let Some(policy) = effect_policy {
            Err(QueryError::Policy(policy))
        } else {
            out
        }
    }

    /// Query a rooted graph through the source-compatible finite policy.
    ///
    /// A standalone call uses [`DEFAULT_QUERY_JS_BUDGET`]. Reentrant calls
    /// inherit an enclosing query/setup/score boundary instead of resetting
    /// its absolute deadline, cancellation pointer, heap latch or job owner.
    pub fn query(
        &self,
        slot: Slot,
        index: usize,
        begin: Fraction,
        end: Fraction,
    ) -> Result<Vec<Hap>, QueryError> {
        let _settings = self.core_settings.bind();
        self.with_legacy_query_turn(|| self.query_inner(slot, index, begin, end))
    }

    /// Query under one monotonic elapsed-time QuickJS interrupt boundary and a
    /// caller-owned cancellation flag.
    ///
    /// This interrupts JavaScript callbacks and JavaScript getters reached
    /// while their returned values are materialised. It is not process CPU-time
    /// accounting or a total-query bound: pure Rust pattern work and arbitrary
    /// Rust-side container traversal are outside that claim.
    pub fn query_cancellable(
        &self,
        slot: Slot,
        index: usize,
        begin: Fraction,
        end: Fraction,
        budget: std::time::Duration,
        cancellation: &std::sync::atomic::AtomicBool,
    ) -> Result<Vec<Hap>, QueryError> {
        let _settings = self.core_settings.bind();
        self.with_bounded_query_turn(budget, cancellation, true, || {
            self.query_inner(slot, index, begin, end)
        })
    }

    /// Cancellable query with a caller-specific hap cap. The previous cap is
    /// restored so unrelated queries retain the runtime default.
    #[allow(clippy::too_many_arguments)] // Mirrors query_cancellable plus the caller's hap cap.
    pub fn query_cancellable_with_hap_budget(
        &self,
        slot: Slot,
        index: usize,
        begin: Fraction,
        end: Fraction,
        budget: std::time::Duration,
        hap_budget: u64,
        cancellation: &std::sync::atomic::AtomicBool,
    ) -> Result<Vec<Hap>, QueryError> {
        let previous = self.hap_budget.replace(hap_budget);
        let out = self.query_cancellable(slot, index, begin, end, budget, cancellation);
        self.hap_budget.set(previous);
        out
    }

    pub(super) fn query_inner(
        &self,
        slot: Slot,
        index: usize,
        begin: Fraction,
        end: Fraction,
    ) -> Result<Vec<Hap>, QueryError> {
        self.sync_host_voicing_default()?;
        if matches!(slot, Slot::Active) {
            let native = with_ctx(&self.ctx, |ctx| -> Option<Pattern> {
                let value: rquickjs::Value = host_active(&ctx).ok()?.get(0).ok()?;
                let wrapper = rquickjs::Class::<NativePatternWrapper>::from_value(&value).ok()?;
                let pattern = wrapper.borrow().pattern.clone();
                Some(pattern)
            });
            if let Some(pattern) = native {
                // The native host surface builds graphs with no callback
                // cells, but not all of them are CLASSIFIED pure: a combinator
                // that consumes a pattern transformer is conservatively impure
                // in `register()`'s general path, because the argument values
                // are only known at query time. Asserting purity here would
                // reject `s("bd").every("<2 3>", rev)`, which is native
                // throughout. Query classified-pure graphs on the fast path,
                // and give the rest a host so a callback that did appear would
                // resolve rather than panic.
                if pattern.is_pure() {
                    // Typed: a refused query must not reach the caller as an
                    // empty result. See `Pattern::try_query_arc_sorted`.
                    // Pure graphs can still report contained semantic errors
                    // from stacked children through the runtime's logger.
                    return rustel_core::with_callback_host(self, || {
                        self.query_sorted_recording_throw(&pattern, begin, end)
                    })
                    .map_err(QueryError::from);
                }
                // Impure: the graph may reach a JS callback, so the wrapper
                // that OWNS the cells must be on the query stack while it runs,
                // and a callback host must be installed. Pushing is what makes
                // reentrancy work - a callback can query another pattern,
                // stacking a second wrapper over this one.
                let _scope = QueryScope::push_active_native(self)?;
                // A patterned transformer resolves at QUERY time and can build
                // new cells then. They belong to this query only, so the frame
                // is torn down with it rather than accumulating across queries.
                return with_ctx(&self.ctx, |_ctx| {
                    let frame = BridgeFrame::new(self.ids.clone());
                    let _bridge = BridgeScope::push(&frame);
                    rustel_core::with_callback_host(self, || {
                        self.query_sorted_recording_throw(&pattern, begin, end)
                    })
                })
                .map_err(QueryError::from);
            }
        }
        let pattern = with_ctx(&self.ctx, |ctx| -> Result<Pattern, String> {
            let w = Self::wrapper_at(&ctx, slot, index)?;
            let p = w.borrow().pattern.clone();
            Ok(p)
        })?;
        let _scope = QueryScope::push(self, slot, index)?;
        let haps = with_ctx(&self.ctx, |_ctx| {
            let frame = BridgeFrame::new(self.ids.clone());
            let _bridge = BridgeScope::push(&frame);
            rustel_core::with_callback_host(self, || {
                self.query_sorted_recording_throw(&pattern, begin, end)
            })
        })
        .map_err(QueryError::from)?;
        Ok(haps)
    }

    /// Preserve a thrown error for the report while returning an empty window.
    /// Store the outcome after the query so an outer query replaces any nested
    /// query's outcome.
    fn query_sorted_recording_throw(
        &self,
        pattern: &Pattern,
        begin: Fraction,
        end: Fraction,
    ) -> Result<Vec<Hap>, rustel_core::QueryLimit> {
        let state = State::new(rustel_core::TimeSpan::new(begin, end));
        let outcome = pattern.query_arc_outcome_with_budget(&state, self.hap_budget.get())?;
        let threw = match &outcome {
            rustel_core::QueryArcOutcome::Thrown(message) => Some(message.clone()),
            rustel_core::QueryArcOutcome::Haps(_) => None,
        };
        *self.query_threw.borrow_mut() = threw;
        match outcome {
            rustel_core::QueryArcOutcome::Haps(haps) => Ok(rustel_core::sort_haps_for_query(haps)),
            rustel_core::QueryArcOutcome::Thrown(_) => Ok(Vec::new()),
        }
    }

    /// Take the last host query's error so the next report cannot reuse it.
    pub fn take_query_throw(&self) -> Option<String> {
        self.query_threw.borrow_mut().take()
    }
}
