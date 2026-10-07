use super::*;

mod voicing_surface;

impl JsRuntime {
    /// Expose `queryHeld(index, begin, end)` to JavaScript.
    ///
    /// This is what makes reentrancy real rather than theoretical: a callback
    /// invoked *inside* a query can query another pattern, so a second wrapper
    /// is pushed while the first is still live. With a single "currently
    /// querying" slot the inner query would clobber the outer's callback table
    /// and the outer graph would resume against the wrong cells.
    pub fn install_query_binding(&self) -> Result<(), String> {
        let _settings = self.core_settings.bind();
        let me: *const JsRuntime = self;
        with_ctx(&self.ctx, |ctx| -> Result<(), String> {
            let f = Function::new(ctx.clone(), move |index: usize, b: f64, e: f64| {
                // SAFETY: the binding lives on the runtime's own context, so
                // the runtime outlives every call made through it.
                let rt: &JsRuntime = unsafe { &*me };
                let haps = rt
                    .query(
                        Slot::Held,
                        index,
                        Fraction::new((b * 1_000_000.0) as i128, 1_000_000),
                        Fraction::new((e * 1_000_000.0) as i128, 1_000_000),
                    )
                    .unwrap_or_default();
                haps.iter()
                    .map(|h| h.value.show())
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .map_err(|err| err.to_string())?;
            set_host_global(&ctx, "queryHeld", f).map_err(|err| err.to_string())
        })
    }

    pub fn install_voicings_prebake(&self) -> Result<(), String> {
        let _settings = self.core_settings.bind();
        with_ctx(&self.ctx, |ctx| voicing_surface::install(&ctx))
    }
    /// Transpile and execute one user setup/prebake file without replacing the
    /// active graph.
    ///
    /// Each call gets a fresh strict-function lexical scope: declarations do
    /// not leak, while deliberate `globalThis`/`window`, `Pattern.prototype`,
    /// and `register` mutations do. Completed JavaScript-heap mutations are
    /// intentionally not rolled back when a later statement throws, matching
    /// one persistent realm. Host effects are different: sample registration
    /// and preload requests leave the runtime only after the whole setup turn
    /// succeeds.
    ///
    /// The source is invoked as a strict async function on the SAME heap, then
    /// every runnable QuickJS job is drained under the same deadline,
    /// cancellation flag, heap boundary, callback host and bridge frame.
    /// Queue quiescence - not merely root-Promise settlement - is the success
    /// boundary, matching the pinned ordering for detached microtasks.
    ///
    /// This supports queued/microtask setup only. If the root Promise remains
    /// pending after the QuickJS queue empties, progress requires a host event
    /// source (for example network or timer I/O) that this boundary does not
    /// provide, so the setup is refused rather than spun or called complete.
    ///
    /// This compatibility API accepts JavaScript-only setup. A source that
    /// calls `samples(...)` or `preload(...)` is refused after evaluation so
    /// its work cannot be silently discarded; use
    /// [`Self::evaluate_prelude_with_effects`] and apply the returned value.
    pub fn evaluate_prelude(
        &self,
        source: &str,
        options: &rustel_transpiler::TranspileOptions,
        budget: std::time::Duration,
    ) -> Result<rustel_transpiler::TranspileOutput, QueryError> {
        let (output, effects) = self.evaluate_prelude_with_effects(source, options, budget)?;
        if !effects.is_empty() {
            return Err(QueryError::Policy(
                "evaluate_prelude cannot discard sample or preload effects; use evaluate_prelude_with_effects"
                    .into(),
            ));
        }
        Ok(output)
    }

    /// Setup evaluation that returns its sample and preload transaction.
    /// Session applies the value once. [`Self::evaluate_prelude`] is limited to
    /// setup that produces no host effects, so a caller cannot accidentally
    /// report success after discarding sample registration or preload work.
    pub fn evaluate_prelude_with_effects(
        &self,
        source: &str,
        options: &rustel_transpiler::TranspileOptions,
        budget: std::time::Duration,
    ) -> Result<(rustel_transpiler::TranspileOutput, ScoreEffects), QueryError> {
        self.evaluate_prelude_inner(source, options, budget, None)
    }

    /// [`evaluate_prelude`](Self::evaluate_prelude) with a caller-owned
    /// cancellation flag. QuickJS's interrupt handler observes the flag while
    /// JavaScript is executing, so cancellation does not wait for the CPU
    /// deadline or for setup to enter the pattern-query machinery. Effectful
    /// callers must use [`Self::evaluate_prelude_with_effects_cancellable`].
    pub fn evaluate_prelude_cancellable(
        &self,
        source: &str,
        options: &rustel_transpiler::TranspileOptions,
        budget: std::time::Duration,
        cancellation: &std::sync::atomic::AtomicBool,
    ) -> Result<rustel_transpiler::TranspileOutput, QueryError> {
        let (output, effects) =
            self.evaluate_prelude_with_effects_cancellable(source, options, budget, cancellation)?;
        if !effects.is_empty() {
            return Err(QueryError::Policy(
                "evaluate_prelude_cancellable cannot discard sample or preload effects; use evaluate_prelude_with_effects_cancellable"
                    .into(),
            ));
        }
        Ok(output)
    }

    /// Cancellable setup evaluation that returns its owned host effects.
    pub fn evaluate_prelude_with_effects_cancellable(
        &self,
        source: &str,
        options: &rustel_transpiler::TranspileOptions,
        budget: std::time::Duration,
        cancellation: &std::sync::atomic::AtomicBool,
    ) -> Result<(rustel_transpiler::TranspileOutput, ScoreEffects), QueryError> {
        self.evaluate_prelude_inner(source, options, budget, Some(cancellation))
    }

    pub(super) fn evaluate_prelude_inner(
        &self,
        source: &str,
        options: &rustel_transpiler::TranspileOptions,
        budget: std::time::Duration,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<(rustel_transpiler::TranspileOutput, ScoreEffects), QueryError> {
        let _settings = self.core_settings.bind();
        let effect_policy_owner = EffectPolicyOwner::enter(self);
        let effect_transaction = EffectTransactionOwner::enter(self, EffectPolicy::SETUP)?;

        // A job that predates this turn has ambiguous ownership. Discard it,
        // fail BEFORE applying any new synchronous side effect, and let the
        // caller retry. This also makes raw/legacy queue creation recoverable
        // without silently attributing its job to the next setup file.
        let stale_jobs = with_ctx(&self.ctx, |ctx| discard_pending_jobs(&ctx));
        if stale_jobs > 0 {
            return Err(QueryError::Message(format!(
                "discarded {stale_jobs} queued JavaScript job(s) before prebake setup; retry the setup"
            )));
        }
        if budget.is_zero() {
            return Err(QueryError::Message(
                "prebake evaluation budget must be greater than zero".into(),
            ));
        }
        let mut options = options.clone();
        options.add_return = false;
        options.wrap_async = false;
        options.allow_module_syntax = false;
        let output = rustel_transpiler::transpile(source, &options);
        if !output.diagnostics.is_empty() {
            return Err(QueryError::Message(
                output
                    .diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic.message.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ));
        }

        let wrapped = format!(
            "(async function () {{\n\"use strict\";\n{}\n}}).call(globalThis);",
            output.output
        );
        // Two wrapper lines sit above the score's first - the reported
        // line numbers are shifted back down by exactly that.
        const SCORE_WRAPPER_LINES: u32 = 2;
        struct CancellationGuard<'a> {
            slot: std::rc::Rc<Cell<Option<*const std::sync::atomic::AtomicBool>>>,
            previous: Option<*const std::sync::atomic::AtomicBool>,
            _borrow: std::marker::PhantomData<&'a std::sync::atomic::AtomicBool>,
        }
        impl Drop for CancellationGuard<'_> {
            fn drop(&mut self) {
                self.slot.set(self.previous);
            }
        }
        self.cancelled.set(false);
        let previous = self
            .cancel_flag
            .replace(cancellation.map(|flag| flag as *const _));
        let _cancellation_guard = CancellationGuard {
            slot: self.cancel_flag.clone(),
            previous,
            _borrow: std::marker::PhantomData,
        };
        let evaluation = self.with_heap_boundary(|| {
            let alloc = self.ids.clone();
            with_ctx(&self.ctx, |ctx| {
                struct PendingJobGuard {
                    runtime: *mut rquickjs::qjs::JSRuntime,
                }
                impl Drop for PendingJobGuard {
                    fn drop(&mut self) {
                        // SAFETY: this guard is created and dropped entirely
                        // inside the `with_ctx` lock that owns `runtime`.
                        unsafe {
                            js_discard_pending_jobs(self.runtime);
                        }
                    }
                }
                // SAFETY: a live Ctx always has a live owning runtime.
                let runtime = unsafe { rquickjs::qjs::JS_GetRuntime(ctx.as_raw().as_ptr()) };
                let _pending_job_guard = PendingJobGuard { runtime };
                let frame = BridgeFrame::new(alloc);
                let _scope = BridgeScope::push(&frame);
                rustel_core::with_callback_host(self, || {
                    self.with_deadline(budget, || {
                        let deadline_error = || {
                            QueryError::Limit(rustel_core::QueryLimit::JsCpuDeadline {
                                millis: budget.as_millis().min(u128::from(u64::MAX)) as u64,
                            })
                        };
                        let interruption = || {
                            if cancellation.is_some_and(|flag| {
                                flag.load(std::sync::atomic::Ordering::Relaxed)
                            }) || self.cancelled.get()
                            {
                                self.cancelled.set(true);
                                return Some(QueryError::Limit(
                                    rustel_core::QueryLimit::Cancelled,
                                ));
                            }
                            if self.was_interrupted()
                                || self
                                    .deadline
                                    .get()
                                    .is_some_and(|at| std::time::Instant::now() >= at)
                            {
                                self.interrupted.set(true);
                                return Some(deadline_error());
                            }
                            None
                        };

                        let root = match ctx.eval::<rquickjs::Promise, _>(wrapped.as_str()) {
                            Ok(root) => root,
                            Err(error) => {
                                let caught = matches!(error, rquickjs::Error::Exception)
                                    .then(|| ctx.catch());
                                if let Some(error) = interruption() {
                                    return Err(error);
                                }
                                return Err(QueryError::Message(match caught {
                                    Some(value) => remap_reported_position(
                                        describe_caught_js_value(value),
                                        SCORE_WRAPPER_LINES,
                                        &output.line_map,
                                    ),
                                    None => error.to_string(),
                                }));
                            }
                        };

                        let mut jobs = 0usize;
                        loop {
                            if let Some(error) = interruption() {
                                return Err(error);
                            }
                            // Refuse before executing job limit + 1. Exactly
                            // MAX_PREBAKE_JOBS jobs are permitted.
                            if jobs == MAX_PREBAKE_JOBS {
                                if unsafe { rquickjs::qjs::JS_IsJobPending(runtime) } {
                                    return Err(QueryError::Limit(
                                        rustel_core::QueryLimit::JsJobBudget {
                                            budget: MAX_PREBAKE_JOBS,
                                        },
                                    ));
                                }
                                break;
                            }

                            let mut job_ctx = std::ptr::null_mut();
                            // SAFETY: the runtime lock, heap boundary, callback
                            // host and BridgeFrame remain live for this entire
                            // loop. Unlike rquickjs's bool helper, this preserves
                            // the -1 exception result so it can be consumed now.
                            let status = unsafe {
                                rquickjs::qjs::JS_ExecutePendingJob(runtime, &mut job_ctx)
                            };
                            match status {
                                0 => break,
                                1 => jobs += 1,
                                -1 => {
                                    let Some(job_ctx) = std::ptr::NonNull::new(job_ctx) else {
                                        return Err(QueryError::Message(
                                            "QuickJS job failed without an exception context"
                                                .into(),
                                        ));
                                    };
                                    // SAFETY: JS_ExecutePendingJob returned a
                                    // live context belonging to the locked
                                    // runtime. Duplicate it only long enough to
                                    // retrieve/free the pending exception.
                                    let job_ctx = unsafe { Ctx::from_raw(job_ctx) };
                                    let caught = job_ctx.catch();
                                    if job_ctx.as_raw() != ctx.as_raw() {
                                        return Err(QueryError::Message(
                                            "prebake setup queued work on an unexpected QuickJS context"
                                                .into(),
                                        ));
                                    }
                                    if let Some(error) = interruption() {
                                        return Err(error);
                                    }
                                    return Err(QueryError::Message(remap_reported_position(
                                        describe_caught_js_value(caught),
                                        SCORE_WRAPPER_LINES,
                                        &output.line_map,
                                    )));
                                }
                                other => {
                                    return Err(QueryError::Message(format!(
                                        "QuickJS returned unexpected pending-job status {other}"
                                    )));
                                }
                            }
                        }

                        if let Some(error) = interruption() {
                            return Err(error);
                        }
                        match root.state() {
                            rquickjs::promise::PromiseState::Resolved => Ok(()),
                            rquickjs::promise::PromiseState::Rejected => {
                                let error = root
                                    .result::<rquickjs::Value>()
                                    .expect("a rejected Promise has a result")
                                    .expect_err("a rejected Promise result is an exception");
                                Err(QueryError::Message(describe_js_error(&ctx, error)))
                            }
                            rquickjs::promise::PromiseState::Pending => {
                                Err(QueryError::Message(
                                    "prebake setup is awaiting a deferred host capability, but no QuickJS jobs are runnable"
                                        .into(),
                                ))
                            }
                        }
                    })
                })
            })
        });
        let policy = effect_policy_owner.policy();
        match evaluation {
            // Resource ownership stays dominant when more than one refusal
            // occurred during the same setup turn.
            Err(QueryError::Limit(limit)) => Err(QueryError::Limit(limit)),
            _ if policy.is_some() => Err(QueryError::Policy(
                policy.expect("checked host-effect policy refusal"),
            )),
            Err(error) => Err(error),
            Ok(()) => {
                // A prelude declares names for the scores that follow it,
                // not for itself: what it registered is the surface they
                // start from, and no score turn gives it back.
                with_ctx(&self.ctx, |ctx| keep_registration_surface(&ctx))
                    .map_err(|error| QueryError::Message(error.to_string()))?;
                Ok((output, effect_transaction.finish()))
            }
        }
    }

    /// Give each synchronous score its own function scope.
    ///
    /// A function contains `var` declarations as well as `let`, `const`, and
    /// `class`, preventing one score's locals from shadowing globals used by
    /// later scores.
    ///
    /// Invoked plainly rather than through `.call(globalThis)`: a score that
    /// assigns `Function.prototype.call` would otherwise stop every later
    /// evaluation before its body started.
    ///
    /// QuickJS evaluation uses strict mode, so `this` is undefined. The async
    /// wrapper also declares strict mode explicitly.
    ///
    /// The return is `add_final_return_or_undefined`, not `add_final_return`.
    /// `undefined` is how the runtime tells "this score named no pattern" from
    /// "this score produced a value that is not a pattern", so a synthesised
    /// `silence` would make a commented-out score look playable and drop the
    /// message saying it is not.
    fn wrap_score_scope(source: &str) -> String {
        format!(
            "(function () {{\n{}\n}})();",
            rustel_transpiler::add_final_return_or_undefined(source)
        )
    }

    /// Lines [`Self::wrap_score_scope`] holds above the score.
    const SYNC_SCORE_SHIM_LINES: u32 = 1;
    /// Lines [`Self::eval_score_async`]'s wrapper holds above the score.
    const ASYNC_SCORE_SHIM_LINES: u32 = 2;

    /// Read an already-settled score promise without running queued jobs.
    ///
    /// This lets a final `Promise.resolve(s("bd*4"))` return its pattern on the
    /// synchronous path, just as it does through the async score wrapper.
    ///
    /// Only an already-settled promise is read. A PENDING one is left alone so
    /// it still fails the conversion, because resolving it would mean running
    /// jobs, and a score that queues deferred work is refused so that
    /// evaluation stays bounded and atomic.
    fn settle_resolved<'js>(
        ctx: &Ctx<'js>,
        value: rquickjs::Value<'js>,
    ) -> rquickjs::Result<rquickjs::Value<'js>> {
        let Some(object) = value.as_object() else {
            return Ok(value);
        };
        if !object.is_promise() {
            return Ok(value);
        }
        let promise = rquickjs::Promise::from_js(ctx, value.clone())?;
        match promise.state() {
            rquickjs::promise::PromiseState::Resolved => match promise.result() {
                Some(result) => result,
                None => Ok(value),
            },
            // A rejection is rethrown, as the async path does with its own
            // root: the score's error is what a reader needs, not "could not
            // convert a promise".
            rquickjs::promise::PromiseState::Rejected => {
                match promise.result::<rquickjs::Value>() {
                    Some(Err(error)) => Err(error),
                    _ => Ok(value),
                }
            }
            // A PENDING promise keeps its value so the conversion fails as
            // before. Settling it would mean running jobs, and a score that
            // queues deferred work is refused so evaluation stays bounded.
            rquickjs::promise::PromiseState::Pending => Ok(value),
        }
    }

    /// Evaluate a transpiled score as `(async function () {...})()` and pump
    /// the QuickJS job queue until the root promise settles. Failures are
    /// re-thrown onto `ctx` so the caller's exception handling (which expects
    /// a pending exception, as after a plain `ctx.eval`) applies unchanged.
    pub(super) fn eval_score_async<'js>(
        ctx: &Ctx<'js>,
        runtime: *mut rquickjs::qjs::JSRuntime,
        source: &str,
    ) -> rquickjs::Result<rquickjs::Value<'js>> {
        // Plain invocation avoids a score-modified `Function.prototype.call`
        // and keeps `this` undefined, as in the synchronous wrapper. Both
        // wrappers return `undefined` for an empty score so it is consistently
        // reported as naming no pattern.
        let wrapped = format!(
            "(async function () {{\n\"use strict\";\n{}\n}})();",
            rustel_transpiler::add_final_return_or_undefined(source)
        );
        let throw_message = |message: &str| -> rquickjs::Result<rquickjs::Value<'js>> {
            let error = rquickjs::Object::new(ctx.clone())?;
            error.set("message", message)?;
            Err(ctx.throw(error.into_value()))
        };
        let root: rquickjs::Promise = ctx.eval(wrapped.as_str())?;
        let mut jobs = 0usize;
        loop {
            if jobs == MAX_PREBAKE_JOBS {
                // SAFETY: `runtime` owns the locked `ctx` for this whole call.
                if unsafe { rquickjs::qjs::JS_IsJobPending(runtime) } {
                    return throw_message(&format!(
                        "score evaluation exceeded the {MAX_PREBAKE_JOBS}-job budget"
                    ));
                }
                break;
            }
            let mut job_ctx = std::ptr::null_mut();
            // SAFETY: the runtime lock, heap boundary, callback host and
            // BridgeFrame of `execute_score` remain live for this loop.
            // This preserves the -1 exception result so it can be consumed
            // now; rquickjs's bool helper consumes it without reporting it.
            let status = unsafe { rquickjs::qjs::JS_ExecutePendingJob(runtime, &mut job_ctx) };
            match status {
                0 => break,
                1 => jobs += 1,
                -1 => {
                    let Some(job_ctx) = std::ptr::NonNull::new(job_ctx) else {
                        return throw_message("QuickJS job failed without an exception context");
                    };
                    // SAFETY: a live context belonging to the locked runtime;
                    // duplicated only to retrieve the pending exception.
                    let job_ctx = unsafe { Ctx::from_raw(job_ctx) };
                    let caught = job_ctx.catch();
                    if job_ctx.as_raw() != ctx.as_raw() {
                        return throw_message(
                            "score evaluation queued work on an unexpected QuickJS context",
                        );
                    }
                    return Err(ctx.throw(caught));
                }
                other => {
                    return throw_message(&format!(
                        "QuickJS returned unexpected pending-job status {other}"
                    ));
                }
            }
        }
        match root.state() {
            rquickjs::promise::PromiseState::Resolved => match root.result() {
                Some(result) => result,
                None => throw_message("resolved score promise had no result"),
            },
            rquickjs::promise::PromiseState::Rejected => {
                let rejection = root
                    .result::<rquickjs::Value>()
                    .expect("a rejected Promise has a result")
                    .expect_err("a rejected Promise result is an exception");
                match rejection {
                    rquickjs::Error::Exception => Err(rquickjs::Error::Exception),
                    other => Err(other),
                }
            }
            rquickjs::promise::PromiseState::Pending => throw_message(
                "score evaluation is awaiting a deferred host capability, but no QuickJS jobs are runnable",
            ),
        }
    }

    pub(super) fn transpile_score(
        source: &str,
        options: &rustel_transpiler::TranspileOptions,
    ) -> Result<rustel_transpiler::TranspileOutput, String> {
        let mut options = options.clone();
        // QuickJS program evaluation already returns its last expression.
        // A bare top-level `return` is only for the async function wrapper.
        options.add_return = false;
        options.allow_module_syntax = false;
        let output = rustel_transpiler::transpile(source, &options);
        if !output.diagnostics.is_empty() {
            return Err(output
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>()
                .join("\n"));
        }
        Ok(output)
    }

    /// Execute already-transpiled score JavaScript and publish its wrapper as
    /// active only after the complete construction succeeds.
    ///
    /// This is shared by the legacy unbounded API and the product-facing
    /// bounded API below. Keeping the callback host and BridgeFrame here makes
    /// it impossible for the two routes to drift on eager `register()` bodies
    /// or on ownership of callbacks created while the graph is constructed.
    pub(crate) fn execute_score(
        &self,
        source: &str,
        line_map: &rustel_transpiler::LineMap,
        stage_candidate: bool,
    ) -> Result<(), QueryError> {
        // `register()`'s fast path runs combinator bodies EAGERLY, so a user
        // function passed to `every`/`jux`/`sometimesBy` is invoked while the
        // graph is still being constructed. The callback host has to be
        // installed for the whole evaluation, not only for queries, and a
        // bridge frame has to be open so the cell it creates is resolvable
        // before any wrapper owns it.
        //
        // The frame is RAII: it is torn down on success, on a JS exception, on
        // a Rust error and on unwind, so no scratch survives the evaluation.
        //
        // The per-evaluation slots - a recorded ownership refusal and an
        // eager callback's held throw - are cleared on entry and on every
        // exit: one evaluation's leftovers must never be reported by, or
        // thrown into, the next.
        struct EvaluationSlotsGuard;
        impl EvaluationSlotsGuard {
            fn clear() {
                let _ = take_ownership_refusal();
                discard_eager_callback_exception();
            }
        }
        impl Drop for EvaluationSlotsGuard {
            fn drop(&mut self) {
                Self::clear();
            }
        }
        EvaluationSlotsGuard::clear();
        let _evaluation_slots = EvaluationSlotsGuard;
        let awaits = rustel_transpiler::awaits_in_code(source);
        // An error thrown from the score's own code. The reported line is the
        // WRAPPED text's: the shim's lines above the score come off, and the
        // printer's map carries the rest of the way back to the score.
        let shim_lines = if awaits {
            Self::ASYNC_SCORE_SHIM_LINES
        } else {
            Self::SYNC_SCORE_SHIM_LINES
        };
        let score_error = |message: String| {
            QueryError::Message(remap_reported_position(message, shim_lines, line_map))
        };
        let alloc = self.ids.clone();
        with_ctx(&self.ctx, |ctx| -> Result<(), QueryError> {
            // The REPL re-injects these exact objects
            // before every score evaluation. Restore every repairable
            // public destination from private roots without invoking a planted
            // accessor. Irreversible descriptors remain public damage, but do
            // not prevent unrelated later scores from running; canonical
            // function identities stay stable for the runtime's lifetime.
            let canonical = host_repl_tempo_surface(&ctx)
                .map_err(|error| QueryError::Message(error.to_string()))?;
            if canonical.len() >= 6 {
                let scope: rquickjs::Object = canonical
                    .get(0)
                    .map_err(|error| QueryError::Message(error.to_string()))?;
                let repair: Function = canonical
                    .get(5)
                    .map_err(|error| QueryError::Message(error.to_string()))?;
                for (index, name) in ["setCps", "setcps", "setCpm", "setcpm"]
                    .into_iter()
                    .enumerate()
                {
                    let setter: rquickjs::Value = canonical
                        .get(index + 1)
                        .map_err(|error| QueryError::Message(error.to_string()))?;
                    repair
                        .call::<_, bool>((ctx.globals(), name, setter.clone()))
                        .map_err(|error| QueryError::Message(describe_js_error(&ctx, error)))?;
                    repair
                        .call::<_, bool>((scope.clone(), name, setter))
                        .map_err(|error| QueryError::Message(describe_js_error(&ctx, error)))?;
                }
                let cps: rquickjs::Value = canonical
                    .get(1)
                    .map_err(|error| QueryError::Message(error.to_string()))?;
                repair
                    .call::<_, bool>((ctx.globals(), "cps", cps.clone()))
                    .map_err(|error| QueryError::Message(describe_js_error(&ctx, error)))?;
                repair
                    .call::<_, bool>((scope, "cps", cps))
                    .map_err(|error| QueryError::Message(describe_js_error(&ctx, error)))?;
            }
            struct PendingJobGuard {
                runtime: *mut rquickjs::qjs::JSRuntime,
                enabled: bool,
            }
            impl Drop for PendingJobGuard {
                fn drop(&mut self) {
                    if self.enabled {
                        // SAFETY: the guard is created and dropped entirely
                        // inside the `with_ctx` lock owning `runtime`.
                        unsafe {
                            js_discard_pending_jobs(self.runtime);
                        }
                    }
                }
            }
            // SAFETY: a live Ctx always has a live owning runtime.
            let runtime = unsafe { rquickjs::qjs::JS_GetRuntime(ctx.as_raw().as_ptr()) };
            let _pending_job_guard = PendingJobGuard {
                runtime,
                enabled: stage_candidate,
            };
            let frame = BridgeFrame::new(alloc);
            let _scope = BridgeScope::push(&frame);
            rustel_core::with_callback_host(self, || {
                let lane_host =
                    host_stack(&ctx).map_err(|error| QueryError::Message(error.to_string()))?;
                // Named props on the prototype-less host array: not numeric
                // stack slots. `p` and these closures are installed together
                // by `install_semantic_bindings`; skipping when they are
                // absent is the no-bindings path, where `.p` is also missing
                // so `$:` still TypeErrors rather than stacking silently.
                if let Ok(reset) = lane_host.as_object().get::<_, Function>(LANE_RESET) {
                    reset
                        .call::<(), ()>(())
                        .map_err(|error| QueryError::Message(describe_js_error(&ctx, error)))?;
                }
                self.timeline_state.reset();
                // setDefaultJoin is score-scoped: reset before user code.
                rustel_core::compose::set_default_alignment(rustel_core::compose::Alignment::In);
                // Scores containing `await` (the transpiler awaits bare
                // `samples(...)` calls, for Strudel compatibility) cannot be
                // script evaluations: QuickJS rejects top-level await outside
                // the async form. Those run as an async IIFE whose jobs are
                // pumped to completion here; everything else is wrapped in a
                // plain function, which is what scopes its declarations.
                //
                // The two wrappers stay separate because only the async form
                // pumps the job queue: routing every score through it would
                // silently accept one that queues a microtask, which this
                // engine refuses so evaluation stays bounded and atomic.
                //
                // The dispatch asks the parser whether the score awaits. A
                // byte scanner, even one that skips comments and quotes,
                // answers true for `/await/`, `{ await: 1 }` and `x.await`,
                // and sends a score that never awaits to the job-pumping
                // path. It answers false for `` s(`${await p}`) ``, whose
                // interpolation is code inside a quoted literal.
                let evaluated_result: rquickjs::Result<rquickjs::Value> = if awaits {
                    Self::eval_score_async(&ctx, runtime, source)
                } else {
                    ctx.eval(Self::wrap_score_scope(source))
                        .and_then(|value| Self::settle_resolved(&ctx, value))
                };
                let ownership_refusal = take_ownership_refusal();
                let ownership_refused = ownership_refusal.is_some();
                let evaluated: rquickjs::Class<NativePatternWrapper> = match evaluated_result {
                    Ok(_) if ownership_refusal.is_some() => ownership_refusal_wrapper(
                        &ctx,
                        ownership_refusal.expect("checked ownership refusal"),
                    )
                    .map_err(|error| QueryError::Message(error.to_string()))?,
                    Ok(evaluated) => {
                        let finished_value = if let Ok(finish) =
                            lane_host.as_object().get::<_, Function>(LANE_FINISH)
                        {
                            // `all(...)` transforms run here, after the
                            // score, and can throw from the score's own lines
                            // (an eager callback rethrown by a method they
                            // call).
                            finish
                                .call((evaluated,))
                                .map_err(|error| score_error(describe_js_error(&ctx, error)))?
                        } else {
                            evaluated
                        };
                        rquickjs::Class::<NativePatternWrapper>::from_js(&ctx, finished_value)
                            .map_err(|error| QueryError::Message(describe_js_error(&ctx, error)))?
                    }
                    Err(error) => {
                        // A score can enqueue a job and then throw before
                        // returning its Pattern. The job is still unsupported
                        // owned work, so refuse it structurally instead of
                        // laundering it into the ordinary user-error channel.
                        // Cancellation/deadline remains dominant; the outer
                        // bounded boundary translates this sentinel exactly.
                        if stage_candidate && self.bounded_evaluation_interrupted() {
                            return Err(QueryError::Message(
                                "bounded score evaluation was interrupted".into(),
                            ));
                        }
                        if stage_candidate
                            // SAFETY: `runtime` belongs to this live locked context.
                            && unsafe { rquickjs::qjs::JS_IsJobPending(runtime) }
                        {
                            return Err(QueryError::Limit(rustel_core::QueryLimit::JsPendingJobs));
                        }
                        if let Some(limit) = ownership_refusal {
                            ownership_refusal_wrapper(&ctx, limit)
                                .map_err(|error| QueryError::Message(error.to_string()))?
                        } else {
                            return Err(score_error(describe_js_error(&ctx, error)));
                        }
                    }
                };
                if stage_candidate && self.bounded_evaluation_interrupted() {
                    return Err(QueryError::Message(
                        "bounded score evaluation was interrupted".into(),
                    ));
                }
                let evaluated_value = evaluated.clone().into_value();
                let object = evaluated_value.as_object().ok_or_else(|| {
                    QueryError::Message("evaluated Pattern is not an object".into())
                })?;
                let query: rquickjs::Value = object
                    .get("query")
                    .map_err(|error| QueryError::Message(error.to_string()))?;
                let wrapper = match query.as_function() {
                    Some(function) => {
                        let is_own_native = evaluated
                            .borrow()
                            .native_query
                            .as_ref()
                            .is_some_and(|native| same_js_value(&ctx, &query, native));
                        if is_own_native {
                            evaluated
                        } else if let Some(owner) = function
                            .get::<_, rquickjs::Value>(NATIVE_QUERY_MARKER)
                            .ok()
                            .and_then(|value| {
                                let owner = value.as_object().and_then(
                                    rquickjs::Class::<NativePatternWrapper>::from_object,
                                )?;
                                let valid = owner
                                    .borrow()
                                    .native_query
                                    .as_ref()
                                    .is_some_and(|native| same_js_value(&ctx, &query, native));
                                valid.then_some(owner)
                            })
                        {
                            // An ordinary native-to-native own-query copy uses
                            // the copied function's graph, not the wrapper it
                            // was assigned onto. Keep the evaluated wrapper's
                            // step metadata, exactly as `new Pattern(query,
                            // steps)` does, and erase structural `as_pure`
                            // classification with the same identity node used
                            // by native withSteps.
                            let steps = evaluated.borrow().pattern.steps;
                            let (pattern, sidecar) = {
                                let borrowed = owner.borrow();
                                (
                                    borrowed
                                        .pattern
                                        .with_query_span(|span| *span)
                                        .with_steps(steps),
                                    Sidecar::of(&borrowed),
                                )
                            };
                            derive_wrapper(ctx.clone(), pattern, &[sidecar])
                                .map_err(|error| QueryError::Message(error.to_string()))?
                        } else {
                            let steps = evaluated.borrow().pattern.steps;
                            let (id, sidecar) = bridge_callable(&ctx, function.clone())
                                .map_err(|error| QueryError::Message(error.to_string()))?;
                            let owner = Sidecar::of(&evaluated.borrow());
                            derive_wrapper(
                                ctx.clone(),
                                rustel_core::js_query(id).with_steps(steps),
                                &[owner, sidecar],
                            )
                            .map_err(|error| QueryError::Message(error.to_string()))?
                        }
                    }
                    None => {
                        let pattern =
                            rustel_core::query_error_pattern("this.query is not a function");
                        derive_wrapper(ctx.clone(), pattern, &[])
                            .map_err(|error| QueryError::Message(error.to_string()))?
                    }
                };
                if stage_candidate
                    // SAFETY: `runtime` belongs to this live locked context.
                    && unsafe { rquickjs::qjs::JS_IsJobPending(runtime) }
                {
                    return Err(QueryError::Limit(rustel_core::QueryLimit::JsPendingJobs));
                }
                // An eager transformer callback's throw is rethrown by the
                // registered method that invoked it, so the score saw it as an
                // ordinary exception above. One still held here reached the
                // callback through an entry point without that rethrow: the
                // score completed with silence baked in where the throwing
                // branch was, and installing it would pass for success. The
                // typed refusals above keep their type; an ownership refusal
                // already replaced the score with its refusal pattern.
                if !ownership_refused
                    && let Some(message) = take_unthrown_eager_callback_exception(&ctx)
                {
                    return Err(score_error(message));
                }
                // Anything a callback handed back must be owned by the graph
                // that survives, not dropped with the frame.
                let ownership_complete = wrapper.borrow().explicit_ownership_complete;
                let suppressed = frame.suppressed.borrow().clone();
                let harvested: Vec<(CallbackId, rquickjs::Class<CallbackCell>)> =
                    if ownership_complete {
                        Vec::new()
                    } else {
                        frame.harvested.borrow().clone()
                    };
                if !harvested.is_empty() {
                    let mut borrowed = wrapper.borrow_mut();
                    // Reachability-filtered, as in `derive_wrapper`: a cell
                    // belonging to a discarded temporary must not be rooted by
                    // the graph that happens to survive.
                    let opaque = borrowed.pattern.purity().opaque;
                    let reachable_slice = borrowed.pattern.reachable_callbacks();
                    let mut reachable = HashSet::new();
                    reachable
                        .try_reserve(reachable_slice.len())
                        .map_err(|_| QueryError::Limit(rustel_core::QueryLimit::HostMemory))?;
                    reachable.extend(reachable_slice.iter().copied());
                    let mut owned_ids = HashSet::new();
                    owned_ids
                        .try_reserve(borrowed.ids.len().saturating_add(harvested.len()))
                        .map_err(|_| QueryError::Limit(rustel_core::QueryLimit::HostMemory))?;
                    owned_ids.extend(borrowed.ids.iter().copied());
                    for (id, cell) in harvested {
                        let known_reachable = reachable.contains(&id);
                        if (opaque || known_reachable)
                            && (known_reachable
                                || (!borrowed.excluded_frame_ids.contains(&id)
                                    && !suppressed.contains(&id)))
                            && owned_ids.insert(id)
                        {
                            borrowed.ids.push(id);
                            borrowed.cells.push(cell);
                        }
                    }
                }
                // Exclusions remain attached to the durable wrapper. A saved
                // owner for one of these globally unique ids may be published
                // only in a later turn; clearing here would let a subsequent
                // opaque composition resurrect the filtered value. Each set
                // is immutable/Rc-shared and capped at the stepwise boundary.
                if stage_candidate && self.bounded_evaluation_interrupted() {
                    return Err(QueryError::Message(
                        "bounded score evaluation was interrupted".into(),
                    ));
                }
                if let Some(policy) = self.effect_policy_refusal.borrow().clone() {
                    // A nested query can be the first place an out-of-scope
                    // setter is reached, and its public query binding returns
                    // a fallback string. The operation-tree latch still
                    // requires refusal before either the active graph or the
                    // bounded candidate is published.
                    return Err(QueryError::Policy(policy));
                }
                if stage_candidate {
                    host_score_candidate(&ctx)
                        .map_err(|error| QueryError::Message(error.to_string()))?
                        .set(0, wrapper)
                        .map_err(|error| QueryError::Message(error.to_string()))
                } else {
                    host_active(&ctx)
                        .map_err(|error| QueryError::Message(error.to_string()))?
                        .set(0, wrapper)
                        .map_err(|error| QueryError::Message(error.to_string()))
                }
            })
        })
    }

    pub(super) fn clear_staged_score(&self) {
        with_ctx(&self.ctx, |ctx| {
            if let Ok(candidate) = host_score_candidate(&ctx) {
                let _ = candidate.set(0, rquickjs::Value::new_undefined(ctx.clone()));
                let _ = candidate.as_object().set("length", 0);
            }
        });
    }

    /// Publish the exact staged JS wrapper, restoring the exact prior wrapper
    /// if publication itself observes a heap refusal, deadline, cancellation,
    /// or QuickJS error. The prior value remains rooted in this Ctx while the
    /// replacement is attempted, so callback sidecars and object identity are
    /// never reconstructed from a bare Rust Pattern.
    pub(super) fn commit_staged_score(&self, millis: u64) -> Result<(), QueryError> {
        with_ctx(&self.ctx, |ctx| -> Result<(), QueryError> {
            if self.bounded_evaluation_interrupted() {
                return if self.cancelled.get() {
                    Err(QueryError::Limit(rustel_core::QueryLimit::Cancelled))
                } else {
                    Err(QueryError::Limit(rustel_core::QueryLimit::JsCpuDeadline {
                        millis,
                    }))
                };
            }

            let candidate: rquickjs::Value = host_score_candidate(&ctx)
                .map_err(|error| QueryError::Message(error.to_string()))?
                .get(0)
                .map_err(|error| QueryError::Message(error.to_string()))?;
            if candidate.is_undefined() {
                return Err(QueryError::Message(
                    "bounded score evaluation produced no staged wrapper".into(),
                ));
            }
            let active =
                host_active(&ctx).map_err(|error| QueryError::Message(error.to_string()))?;
            let previous: rquickjs::Value = active
                .get(0)
                .map_err(|error| QueryError::Message(error.to_string()))?;

            let _ = alloc::take_heap_exhausted();
            let publication = active.set(0, candidate);
            let interrupted = self.bounded_evaluation_interrupted();
            let heap_exhausted = alloc::take_heap_exhausted();
            if publication.is_ok() && !interrupted && !heap_exhausted {
                return Ok(());
            }

            // `previous` is the exact JS wrapper, still rooted by this local.
            // Restoring a cloned Rust graph would lose callback cells, JS-owned
            // values, writable query identity, and every other wrapper field.
            let _ = alloc::take_heap_exhausted();
            let restored = active.set(0, previous);
            let restore_exhausted = alloc::take_heap_exhausted();
            if restored.is_err() || restore_exhausted {
                return Err(QueryError::Message(
                    "failed to restore the exact active score after publication refusal".into(),
                ));
            }
            if heap_exhausted {
                Err(QueryError::Limit(rustel_core::QueryLimit::HostMemory))
            } else if self.cancelled.get() {
                Err(QueryError::Limit(rustel_core::QueryLimit::Cancelled))
            } else if self.was_interrupted() {
                Err(QueryError::Limit(rustel_core::QueryLimit::JsCpuDeadline {
                    millis,
                }))
            } else {
                Err(QueryError::Message(
                    publication
                        .expect_err("checked failed publication")
                        .to_string(),
                ))
            }
        })
    }

    /// Transpile and evaluate a score under a typed CPU deadline and
    /// caller-owned cancellation flag.
    ///
    /// Transpilation deliberately happens **before** the QuickJS deadline. A
    /// caller may therefore claim bounded synchronous JavaScript occupancy,
    /// but not bounded parse/transpile or total reload time. Execution then
    /// shares one heap boundary, BridgeFrame, callback host, cancellation
    /// scope, and QuickJS deadline. A deadline or cancellation never publishes
    /// a partially-built active wrapper, while ordinary JavaScript side
    /// effects completed before the interruption remain in the one live heap.
    pub fn evaluate_score_cancellable(
        &self,
        source: &str,
        options: &rustel_transpiler::TranspileOptions,
        budget: std::time::Duration,
        cancellation: &std::sync::atomic::AtomicBool,
    ) -> Result<rustel_transpiler::TranspileOutput, QueryError> {
        let (output, _, settings) = self.evaluate_score_cancellable_inner(
            source,
            options,
            budget,
            cancellation,
            false,
            None,
        )?;
        self.core_settings.replace_with(&settings);
        Ok(output)
    }

    /// Bounded score evaluation with atomic host effects.
    ///
    /// Unlike the source-compatible evaluator above, this owns one
    /// sample/preload/tempo transaction. It is returned only after the exact
    /// staged wrapper has published successfully; every error path discards
    /// it. The caller must apply the value with the corresponding scheduler
    /// graph generation.
    pub fn evaluate_score_with_effects_cancellable(
        &self,
        source: &str,
        options: &rustel_transpiler::TranspileOptions,
        budget: std::time::Duration,
        cancellation: &std::sync::atomic::AtomicBool,
    ) -> Result<(rustel_transpiler::TranspileOutput, ScoreEffects), QueryError> {
        let (output, effects, settings) = self.evaluate_score_cancellable_inner(
            source,
            options,
            budget,
            cancellation,
            true,
            None,
        )?;
        self.core_settings.replace_with(&settings);
        Ok((output, effects))
    }

    /// Evaluate and stage a score without publishing its native module
    /// settings.
    ///
    /// The returned snapshot selects the candidate state for probing and can
    /// be published after acceptance. The private active slot already contains
    /// the candidate wrapper and can still be restored on rejection.
    #[doc(hidden)]
    pub fn evaluate_score_candidate_with_effects_cancellable(
        &self,
        source: &str,
        options: &rustel_transpiler::TranspileOptions,
        budget: std::time::Duration,
        cancellation: &std::sync::atomic::AtomicBool,
    ) -> Result<
        (
            rustel_transpiler::TranspileOutput,
            ScoreEffects,
            rustel_core::settings::RuntimeSettings,
        ),
        QueryError,
    > {
        self.evaluate_score_cancellable_inner(source, options, budget, cancellation, true, None)
    }

    /// Stage a candidate from an unpublished settings snapshot without
    /// publishing or mutating the retained seed.
    #[doc(hidden)]
    pub fn evaluate_score_candidate_with_effects_cancellable_seeded(
        &self,
        source: &str,
        options: &rustel_transpiler::TranspileOptions,
        budget: std::time::Duration,
        cancellation: &std::sync::atomic::AtomicBool,
        seed: &rustel_core::settings::RuntimeSettings,
    ) -> Result<
        (
            rustel_transpiler::TranspileOutput,
            ScoreEffects,
            rustel_core::settings::RuntimeSettings,
        ),
        QueryError,
    > {
        self.evaluate_score_cancellable_inner(
            source,
            options,
            budget,
            cancellation,
            true,
            Some(seed),
        )
    }

    pub(super) fn evaluate_score_cancellable_inner(
        &self,
        source: &str,
        options: &rustel_transpiler::TranspileOptions,
        budget: std::time::Duration,
        cancellation: &std::sync::atomic::AtomicBool,
        allow_score_effects: bool,
        seed: Option<&rustel_core::settings::RuntimeSettings>,
    ) -> Result<
        (
            rustel_transpiler::TranspileOutput,
            ScoreEffects,
            rustel_core::settings::RuntimeSettings,
        ),
        QueryError,
    > {
        let candidate_settings = match seed {
            Some(seed) => self.core_settings.detached_from(seed),
            None => self.core_settings.detached_snapshot(),
        };
        let effect_policy_owner = EffectPolicyOwner::enter(self);
        let effect_transaction = EffectTransactionOwner::enter(
            self,
            if allow_score_effects {
                EffectPolicy::SCORE
            } else {
                EffectPolicy::NONE
            },
        )?;

        // A job predating this score has no attributable owner. Discard it and
        // refuse before this score executes any JavaScript, matching the setup
        // turn's fail-closed ownership rule.
        let stale_jobs = with_ctx(&self.ctx, |ctx| discard_pending_jobs(&ctx));

        // `register()` writes onto the shared Pattern prototype and the score
        // scope, and both outlive the evaluation that wrote them. Give the
        // names back before the NEXT score runs, so deleting a `register`
        // line and playing again means what it says: the engine's name
        // answers again, and a name of the score's own goes away with it.
        if allow_score_effects {
            with_ctx(&self.ctx, |ctx| restore_registration_surface(&ctx))
                .map_err(|error| QueryError::Message(error.to_string()))?;
        }

        // The visuals surface borrows six global names - `shape` and `osc`
        // among them - from the moment a score calls `initHydra()`. Give them
        // back before the NEXT score runs, so removing the visuals from a file
        // also removes their hold on the names.
        #[cfg(feature = "hydra")]
        if allow_score_effects {
            let chains = self.hydra_chains.clone();
            with_ctx(&self.ctx, |ctx| {
                crate::install::restore_hydra_surface(&ctx, &chains)
            })
            .map_err(|error| QueryError::Message(error.to_string()))?;
        }
        if budget.is_zero() {
            return Err(QueryError::Message(
                "score evaluation budget must be greater than zero".into(),
            ));
        }
        if cancellation.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(QueryError::Limit(rustel_core::QueryLimit::Cancelled));
        }
        if stale_jobs > 0 {
            return Err(QueryError::Limit(rustel_core::QueryLimit::JsPendingJobs));
        }

        // This is intentionally outside both the deadline and heap boundary:
        // the Rust transpiler neither executes on the QuickJS heap nor has a
        // cancellable/fuelled parser yet. The API's claim stops at synchronous
        // QuickJS execution.
        let output = Self::transpile_score(source, options).map_err(QueryError::Message)?;

        struct CancellationGuard<'a> {
            slot: std::rc::Rc<Cell<Option<*const std::sync::atomic::AtomicBool>>>,
            previous: Option<*const std::sync::atomic::AtomicBool>,
            _borrow: std::marker::PhantomData<&'a std::sync::atomic::AtomicBool>,
        }
        impl Drop for CancellationGuard<'_> {
            fn drop(&mut self) {
                self.slot.set(self.previous);
            }
        }

        self.cancelled.set(false);
        let previous = self
            .cancel_flag
            .replace(Some(cancellation as *const std::sync::atomic::AtomicBool));
        let _cancellation_guard = CancellationGuard {
            slot: self.cancel_flag.clone(),
            previous,
            _borrow: std::marker::PhantomData,
        };
        struct StagedScoreGuard<'a>(&'a JsRuntime);
        impl Drop for StagedScoreGuard<'_> {
            fn drop(&mut self) {
                self.0.clear_staged_score();
            }
        }
        self.clear_staged_score();
        let _staged_score_guard = StagedScoreGuard(self);
        let millis = budget.as_millis().min(u128::from(u64::MAX)) as u64;
        let evaluation = candidate_settings.with(|| {
            self.with_deadline(budget, || {
                let staged = self.with_heap_boundary(|| {
                    // Forking the voicing registry reads enumerable properties
                    // from score-controlled objects. A getter can run arbitrary
                    // JavaScript, so candidate setup shares the score deadline.
                    self.begin_slider_candidate().map_err(QueryError::Message)?;
                    self.begin_voicing_candidate()
                        .map_err(QueryError::Message)?;
                    let _published_owner = self.core_settings.bind();
                    self.execute_score(&output.output, &output.line_map, true)
                });
                if self.cancelled.get() {
                    Err(QueryError::Limit(rustel_core::QueryLimit::Cancelled))
                } else if self.was_interrupted() {
                    Err(QueryError::Limit(rustel_core::QueryLimit::JsCpuDeadline {
                        millis,
                    }))
                } else {
                    match staged {
                        // Resource ownership remains dominant over a policy latch
                        // raised in the same score turn.
                        Err(QueryError::Limit(limit)) => Err(QueryError::Limit(limit)),
                        staged => {
                            if let Some(policy) = effect_policy_owner.policy() {
                                return Err(QueryError::Policy(policy));
                            }
                            staged?;
                            self.commit_staged_score(millis)
                        }
                    }
                }
            })
        });
        if let Err(error) = evaluation {
            self.discard_slider_candidate();
            self.discard_voicing_candidate();
            return Err(error);
        }
        if let Err(error) = self.commit_score_candidates() {
            self.discard_slider_candidate();
            self.discard_voicing_candidate();
            return Err(QueryError::Message(error));
        }
        Ok((output, effect_transaction.finish(), candidate_settings))
    }

    /// Transpile and evaluate a source expression, installing its native
    /// wrapper as the active graph. Evaluation occurs in the existing heap,
    /// so ordinary JavaScript side effects survive a later throw. Native
    /// module settings are candidate state and publish only with the graph.
    ///
    /// This legacy API intentionally preserves its existing unbounded
    /// semantics. Product callers that execute untrusted source should use
    /// [`Self::evaluate_score_cancellable`].
    pub fn evaluate_score(
        &self,
        source: &str,
        options: &rustel_transpiler::TranspileOptions,
    ) -> Result<rustel_transpiler::TranspileOutput, String> {
        let candidate_settings = self.core_settings.detached_snapshot();
        let effect_policy_owner = EffectPolicyOwner::enter(self);
        let _effect_transaction =
            EffectTransactionOwner::enter(self, EffectPolicy::NONE).map_err(|e| e.to_string())?;
        let output = Self::transpile_score(source, options)?;
        self.begin_slider_candidate()?;
        if let Err(error) = self.begin_voicing_candidate() {
            self.discard_slider_candidate();
            self.discard_voicing_candidate();
            return Err(error);
        }
        let evaluated = candidate_settings.with(|| {
            let _published_owner = self.core_settings.bind();
            self.execute_score(&output.output, &output.line_map, false)
        });
        let evaluated = evaluated.map_err(|error| error.to_string());
        let policy = effect_policy_owner.policy();
        match (evaluated, policy) {
            (_, Some(policy)) => {
                self.discard_slider_candidate();
                self.discard_voicing_candidate();
                Err(policy)
            }
            (Err(error), None) => {
                self.discard_slider_candidate();
                self.discard_voicing_candidate();
                Err(error)
            }
            (Ok(()), None) => {
                if let Err(error) = self.commit_score_candidates() {
                    self.discard_slider_candidate();
                    self.discard_voicing_candidate();
                    return Err(error);
                }
                self.core_settings.replace_with(&candidate_settings);
                Ok(output)
            }
        }
    }
}
