use super::*;

impl JsRuntime {
    /// Bind `globalThis[name]` to the wrapper in a slot, so tests and user code
    /// can assert on the **actual** pattern handle - its identity across
    /// evaluations, its `instanceof`, and that it stays queryable.
    pub fn bind_global(&self, name: &str, slot: Slot, index: usize) -> Result<(), String> {
        let _settings = self.core_settings.bind();
        with_ctx(&self.ctx, |ctx| -> Result<(), String> {
            let w = Self::wrapper_at(&ctx, slot, index)?;
            ctx.globals().set(name, w).map_err(|e| e.to_string())
        })
    }

    /// Query with scheduler controls attached - notably `_cps`, which
    /// `glide`-style patterns read to tell a real trigger from a lookahead.
    ///
    /// This source-compatible API uses [`DEFAULT_QUERY_JS_BUDGET`] for a
    /// standalone call and preserves its historical string error type. Use
    /// [`Self::query_with_controls_cancellable`] when the typed refusal and a
    /// caller-owned finite policy are required.
    pub fn query_with_controls(
        &self,
        slot: Slot,
        index: usize,
        begin: Fraction,
        end: Fraction,
        controls: &[(&str, f64)],
    ) -> Result<Vec<Hap>, String> {
        let _settings = self.core_settings.bind();
        self.with_legacy_query_turn(|| {
            self.query_with_controls_inner(slot, index, begin, end, controls)
        })
        .map_err(|error| error.to_string())
    }

    /// Query with scheduler controls under one typed, cancellable synchronous
    /// QuickJS boundary.
    ///
    /// JavaScript callbacks, getters and returned-value materialisation share
    /// the supplied elapsed-time deadline. Pure Rust traversal, state
    /// construction and final sorting are not a total-query time bound.
    #[allow(clippy::too_many_arguments)] // Mirrors query_with_controls plus budget/cancellation.
    pub fn query_with_controls_cancellable(
        &self,
        slot: Slot,
        index: usize,
        begin: Fraction,
        end: Fraction,
        controls: &[(&str, f64)],
        budget: std::time::Duration,
        cancellation: &std::sync::atomic::AtomicBool,
    ) -> Result<Vec<Hap>, QueryError> {
        let _settings = self.core_settings.bind();
        self.with_bounded_query_turn(budget, cancellation, true, || {
            self.query_with_controls_inner(slot, index, begin, end, controls)
        })
    }

    pub(super) fn query_with_controls_inner(
        &self,
        slot: Slot,
        index: usize,
        begin: Fraction,
        end: Fraction,
        controls: &[(&str, f64)],
    ) -> Result<Vec<Hap>, QueryError> {
        self.sync_host_voicing_default()?;
        let (pattern, native_active) = with_ctx(&self.ctx, |ctx| -> Result<_, String> {
            if matches!(slot, Slot::Active) {
                let value: rquickjs::Value = host_active(&ctx)
                    .map_err(|error| error.to_string())?
                    .get(0)
                    .map_err(|error| error.to_string())?;
                if let Ok(wrapper) = rquickjs::Class::<NativePatternWrapper>::from_value(&value) {
                    let pattern = wrapper.borrow().pattern.clone();
                    return Ok((pattern, true));
                }
            }
            let wrapper = Self::wrapper_at(&ctx, slot, index)?;
            let pattern = wrapper.borrow().pattern.clone();
            Ok((pattern, false))
        })
        .map_err(QueryError::Message)?;
        let _scope = if native_active {
            QueryScope::push_active_native(self)
        } else {
            QueryScope::push(self, slot, index)
        }
        .map_err(QueryError::Message)?;
        let mut state = State::new(rustel_core::TimeSpan::new(begin, end));
        for (k, v) in controls {
            state.controls.push(((*k).to_string(), Value::F64(*v)));
        }
        // Query-scoped bridge frame: see `query`.
        let frame = BridgeFrame::new(self.ids.clone());
        let _bridge = BridgeScope::push(&frame);
        rustel_core::with_callback_host(self, || {
            // Preserve the queryArc error boundary, then order the returned
            // haps without discarding wholeless signals across cycles.
            let haps = pattern
                .try_query_state_with_budget(&state, self.hap_budget.get())
                .map_err(QueryError::Limit)?;
            Ok(rustel_core::sort_haps_for_query(haps))
        })
    }

    /// Depth of the query stack - 0 when nothing is being queried.
    pub fn query_depth(&self) -> usize {
        let _settings = self.core_settings.bind();
        with_ctx(&self.ctx, |ctx| {
            host_stack(&ctx).map(|stack| stack.len()).unwrap_or(0)
        })
    }

    /// Clear the active slot, making the current graph unreachable.
    pub fn clear_active(&self) {
        let _settings = self.core_settings.bind();
        with_ctx(&self.ctx, |ctx| {
            if let Ok(active) = host_active(&ctx) {
                let _ = active.set(0, rquickjs::Value::new_null(ctx.clone()));
            }
        });
    }

    /// Remember the current active wrapper and its slider and voicing state.
    /// The active wrapper may be absent before the first successful install;
    /// its baseline slider and voicing state still needs to be restorable.
    pub fn snapshot_active_as_last_good(&self) {
        let _settings = self.core_settings.bind();
        with_ctx(&self.ctx, |ctx| {
            *self.last_good_active.borrow_mut() = host_active(&ctx)
                .ok()
                .and_then(|active| active.get::<rquickjs::Value>(0).ok())
                .filter(|value| !value.is_null() && !value.is_undefined())
                .map(|value| rquickjs::Persistent::save(&ctx, value));
            if let Ok(sets) = host_slider_sets(&ctx)
                && let Ok(active_sliders) = sets.get::<rquickjs::Value>(0)
            {
                let _ = sets.set(2, active_sliders);
                if let Ok(bindings) = sets.get::<rquickjs::Value>(3) {
                    let _ = sets.set(5, bindings);
                }
            }
            if let Ok(sets) = host_voicing_sets(&ctx)
                && sets.len() >= 5
                && let Ok(active_registry) = sets.get::<rquickjs::Value>(0)
            {
                let _ = sets.set(2, active_registry);
            }
        });
    }

    /// Restore the last successfully installed active wrapper and its slider
    /// and voicing state. Clear the active wrapper if the snapshot predates
    /// the first successful install.
    pub fn restore_last_good_active(&self) -> Result<(), QueryError> {
        let _settings = self.core_settings.bind();
        with_ctx(&self.ctx, |ctx| -> Result<(), QueryError> {
            let restored = {
                let saved = self.last_good_active.borrow();
                match saved.as_ref() {
                    Some(saved) => saved
                        .clone()
                        .restore(&ctx)
                        .map_err(|error| QueryError::Message(error.to_string()))?,
                    None => rquickjs::Value::new_null(ctx.clone()),
                }
            };
            host_active(&ctx)
                .map_err(|error| QueryError::Message(error.to_string()))?
                .set(0, restored)
                .map_err(|error| QueryError::Message(error.to_string()))?;
            let sets =
                host_slider_sets(&ctx).map_err(|error| QueryError::Message(error.to_string()))?;
            let sliders: rquickjs::Value = sets
                .get(2)
                .map_err(|error| QueryError::Message(error.to_string()))?;
            if !sliders.is_null() && !sliders.is_undefined() {
                sets.set(0, sliders.clone())
                    .map_err(|error| QueryError::Message(error.to_string()))?;
                sets.set(1, sliders)
                    .map_err(|error| QueryError::Message(error.to_string()))?;
                let bindings: rquickjs::Value = sets
                    .get(5)
                    .map_err(|error| QueryError::Message(error.to_string()))?;
                sets.set(3, bindings.clone())
                    .map_err(|error| QueryError::Message(error.to_string()))?;
                sets.set(4, bindings)
                    .map_err(|error| QueryError::Message(error.to_string()))?;
            }
            let voicing_sets =
                host_voicing_sets(&ctx).map_err(|error| QueryError::Message(error.to_string()))?;
            if voicing_sets.len() >= 5 {
                let registry: rquickjs::Value = voicing_sets
                    .get(2)
                    .map_err(|error| QueryError::Message(error.to_string()))?;
                if !registry.is_null() && !registry.is_undefined() {
                    let select: Function = voicing_sets
                        .get(4)
                        .map_err(|error| QueryError::Message(error.to_string()))?;
                    select
                        .call::<_, ()>((registry.clone(),))
                        .map_err(|error| QueryError::Message(describe_js_error(&ctx, error)))?;
                    voicing_sets
                        .set(0, registry.clone())
                        .map_err(|error| QueryError::Message(error.to_string()))?;
                    voicing_sets
                        .set(1, registry)
                        .map_err(|error| QueryError::Message(error.to_string()))?;
                }
            }
            Ok(())
        })
    }

    /// Clone the Rust pattern rooted in the active slot and retain this
    /// runtime's module settings on the exported handle.
    pub fn active_pattern(&self) -> Option<Pattern> {
        self.active_pattern_unwrapped()
            .map(|pattern| pattern.with_runtime_settings(self.core_settings.clone()))
    }

    /// Whether the active slot contains a pattern wrapper, without cloning its
    /// graph.
    pub fn has_active_pattern(&self) -> bool {
        let _settings = self.core_settings.bind();
        with_ctx(&self.ctx, |ctx| {
            let Ok(active) = host_active(&ctx) else {
                return false;
            };
            let Ok(value) = active.get::<rquickjs::Value>(0) else {
                return false;
            };
            if value.is_null() || value.is_undefined() {
                return false;
            }
            rquickjs::Class::<NativePatternWrapper>::from_value(&value).is_ok()
                || rquickjs::Class::<PatternWrapper>::from_value(&value).is_ok()
        })
    }

    pub(super) fn active_pattern_unwrapped(&self) -> Option<Pattern> {
        let _settings = self.core_settings.bind();
        with_ctx(&self.ctx, |ctx| -> Option<Pattern> {
            let value: rquickjs::Value = host_active(&ctx).ok()?.get(0).ok()?;
            if value.is_null() || value.is_undefined() {
                return None;
            }
            if let Ok(wrapper) = rquickjs::Class::<NativePatternWrapper>::from_value(&value) {
                return Some(wrapper.borrow().pattern.clone());
            }
            if let Ok(wrapper) = rquickjs::Class::<PatternWrapper>::from_value(&value) {
                return Some(wrapper.borrow().pattern.clone());
            }
            None
        })
    }

    /// Test-support diagnostic for persistent ownership metadata. Each
    /// durable set is capped independently; retained versions of a growing
    /// lineage can still consume aggregate memory and are an explicit resource
    /// residual of the per-wrapper policy.
    #[doc(hidden)]
    pub fn active_exclusion_count(&self) -> usize {
        let _settings = self.core_settings.bind();
        with_ctx(&self.ctx, |ctx| {
            let Ok(active) = host_active(&ctx) else {
                return 0;
            };
            let Ok(value) = active.get::<rquickjs::Value>(0) else {
                return 0;
            };
            let Ok(wrapper) = rquickjs::Class::<NativePatternWrapper>::from_value(&value) else {
                return 0;
            };
            wrapper.borrow().excluded_frame_ids.len()
        })
    }

    /// Test-support diagnostic for a deliberately saved wrapper that is not
    /// the active publication candidate.
    #[doc(hidden)]
    pub fn wrapper_exclusion_count(&self, name: &str) -> usize {
        let _settings = self.core_settings.bind();
        with_ctx(&self.ctx, |ctx| {
            let Ok(value) = ctx.globals().get::<_, rquickjs::Value>(name) else {
                return 0;
            };
            let Ok(wrapper) = rquickjs::Class::<NativePatternWrapper>::from_value(&value) else {
                return 0;
            };
            wrapper.borrow().excluded_frame_ids.len()
        })
    }

    /// Number of JS objects currently allocated - an independent check on the
    /// per-resource counters.
    pub fn js_object_count(&self) -> usize {
        self.rt.memory_usage().obj_count as usize
    }

    /// A callback created in a live bridge frame but not yet owned by a
    /// wrapper. Searched innermost-first, so a nested query resolves its own
    /// cells before an enclosing one's.
    pub(super) fn pending_callback<'js>(id: CallbackId) -> Option<Function<'js>> {
        let frames = BRIDGE_FRAMES.with(|frames| frames.borrow().clone());
        for ptr in frames.iter().rev() {
            // SAFETY: pushed by `BridgeScope`, which outlives this call.
            let frame = unsafe { &*ptr.cast::<BridgeFrame<'js>>() };
            let hit = frame
                .pending
                .borrow()
                .iter()
                .find(|(candidate, _)| *candidate == id)
                .map(|(_, func)| func.clone());
            if let Some(func) = hit {
                return Some(func);
            }
            let harvested = frame
                .harvested
                .borrow()
                .iter()
                .find(|(candidate, _)| *candidate == id)
                .and_then(|(_, cell)| cell.borrow().as_function().cloned());
            if let Some(func) = harvested {
                return Some(func);
            }
        }
        None
    }

    pub(super) fn pending_js_value<'js>(id: CallbackId) -> Option<rquickjs::Value<'js>> {
        let frames = BRIDGE_FRAMES.with(|frames| frames.borrow().clone());
        for ptr in frames.iter().rev() {
            // SAFETY: pushed by BridgeScope and removed before its frame dies.
            let frame = unsafe { &*ptr.cast::<BridgeFrame<'js>>() };
            if let Some(value) = frame
                .harvested
                .borrow()
                .iter()
                .find(|(candidate, _)| *candidate == id)
                .and_then(|(_, cell)| cell.borrow().as_value().cloned())
            {
                return Some(value);
            }
        }
        None
    }

    pub(crate) fn with_js_value<'js, R>(
        ctx: &Ctx<'js>,
        id: CallbackId,
        f: impl FnOnce(rquickjs::Value<'js>) -> Result<R, String>,
    ) -> Result<R, String> {
        if let Some(value) = Self::pending_js_value(id) {
            return f(value);
        }
        let stack = host_stack(ctx).map_err(|e| e.to_string())?;
        if stack.is_empty() {
            return Err(format!("JavaScript value {id} has no live owner"));
        }
        let top: rquickjs::Value<'js> = stack
            .get(stack.len() - 1)
            .map_err(|error| error.to_string())?;
        let missing = || format!("JavaScript value {id} is not owned by the querying wrapper");
        let object = top.as_object().ok_or_else(missing)?;
        let value = if let Some(wrapper) = rquickjs::Class::<PatternWrapper>::from_object(object) {
            wrapper
                .borrow()
                .cell(id)
                .ok_or_else(missing)?
                .borrow()
                .as_value()
                .cloned()
                .ok_or_else(missing)?
        } else if let Some(wrapper) = rquickjs::Class::<NativePatternWrapper>::from_object(object) {
            wrapper
                .borrow()
                .cell(id)
                .ok_or_else(missing)?
                .borrow()
                .as_value()
                .cloned()
                .ok_or_else(missing)?
        } else {
            return Err(missing());
        };
        f(value)
    }

    /// Run `f` with callback `id`, taken from a live bridge frame or from the
    /// wrapper on top of the query stack.
    ///
    /// The lookup is by global id, not by position: a composed graph holds
    /// cells for ids created by several evaluations, and a positional lookup
    /// would invoke the wrong one.
    pub(crate) fn with_callback<'js, R>(
        ctx: &Ctx<'js>,
        id: CallbackId,
        f: impl FnOnce(Function<'js>) -> Result<R, String>,
    ) -> Result<R, String> {
        // Check the evaluation scratch FIRST: during construction there is no
        // wrapper on the query stack yet, and `register()`'s fast path calls
        // combinator bodies eagerly.
        if let Some(func) = Self::pending_callback(id) {
            return f(func);
        }
        let stack = host_stack(ctx).map_err(|e| e.to_string())?;
        let n = stack.len();
        if n == 0 {
            return Err(format!(
                "callback {id} has no owner and no wrapper is being queried"
            ));
        }
        // Either wrapper family may be on the stack: `PatternWrapper` for
        // graphs built through the builder, `NativePatternWrapper` for graphs
        // built through the host surface. Both own their cells the same way.
        let top: rquickjs::Value<'js> = stack.get(n - 1).map_err(|e| e.to_string())?;
        let missing = || {
            format!(
                "callback {id} has no cell in the querying wrapper; the graph \
                 was composed without importing the wrapper that owns it"
            )
        };
        let object = top.as_object().ok_or_else(missing)?;
        let func = if let Some(w) = rquickjs::Class::<PatternWrapper>::from_object(object) {
            let b = w.borrow();
            b.cell(id)
                .ok_or_else(missing)?
                .borrow()
                .as_function()
                .cloned()
                .ok_or_else(missing)?
        } else if let Some(w) = rquickjs::Class::<NativePatternWrapper>::from_object(object) {
            let b = w.borrow();
            b.cell(id)
                .ok_or_else(missing)?
                .borrow()
                .as_function()
                .cloned()
                .ok_or_else(missing)?
        } else {
            return Err(missing());
        };

        f(func)
    }

    /// Finish one JavaScript host entry while the core query boundary is still
    /// live.
    ///
    /// This ordering is the scheduler-atomicity boundary. If an interrupt were
    /// translated only after `Scheduler::tick`, `queryArc` would first turn the
    /// callback exception into silence and the scheduler would advance its
    /// queried cursor. Publishing the typed refusal here makes the core query
    /// return `Refused` before any such commit.
    pub(super) fn finish_bounded_query_host_call<R>(
        &self,
        out: Result<R, String>,
    ) -> Result<R, String> {
        if self.query_turn_depth.get() == 0 {
            if self.effect_query_depth.get() > 0 && self.effect_policy_pending() {
                rustel_core::refuse_js_pending_jobs();
                return Err(self.effect_policy_message());
            }
            return out;
        }
        if self.bounded_evaluation_interrupted() {
            if self.cancelled.get() {
                // The identical AtomicBool is installed in core's
                // `with_cancellation`, so the core boundary returns its typed
                // Cancelled variant before considering any ordinary error.
                return Err("bounded JavaScript query was cancelled".into());
            }
            let millis = self.query_limit_millis.get().unwrap_or_default();
            rustel_core::refuse_js_cpu_deadline(millis);
            return Err("bounded JavaScript query exceeded its CPU deadline".into());
        }
        if alloc::heap_exhausted() {
            rustel_core::refuse_host_memory();
            return Err("bounded JavaScript query exhausted the host heap".into());
        }
        let jobs_pending = with_ctx(&self.ctx, |ctx| {
            // SAFETY: this live Ctx owns the locked runtime. Query jobs are
            // only observed here; the outer turn guard discards them without
            // invoking their callbacks.
            let runtime = unsafe { rquickjs::qjs::JS_GetRuntime(ctx.as_raw().as_ptr()) };
            unsafe { rquickjs::qjs::JS_IsJobPending(runtime) }
        });
        if jobs_pending {
            rustel_core::refuse_js_pending_jobs();
            return Err("bounded synchronous query left runnable JavaScript jobs".into());
        }
        if self.effect_policy_pending() {
            // Reuse the structural refusal carrier only inside the core query
            // so queryArc and Scheduler cannot accept plausible silence. The
            // owning outer jsruntime boundary replaces this sentinel with the
            // precise `QueryError::Policy` before returning to its caller.
            rustel_core::refuse_js_pending_jobs();
            return Err(self.effect_policy_message());
        }
        if let Err(message) = &out {
            // JavaScript `queryArc` reports ordinary query exceptions through
            // `logger` before returning silence. Native active graphs bypass
            // that JavaScript wrapper, so preserve the same visible contract
            // here instead of making a malformed callback look successful.
            self.record_query_error(message);
        }
        out
    }

    pub(super) fn record_query_error(&self, message: &str) {
        let line = format!("[query] error: {message}");
        let mut logs = self.logs.borrow_mut();
        if logs.last() != Some(&line) && logs.len() < MAX_BUFFERED_LOGS {
            logs.push(line);
        }
    }
}
