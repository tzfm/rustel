use super::*;

impl JsRuntime {
    /// Whether a transformer callback is being invoked EAGERLY - invoked
    /// synchronously by the method the score is calling (`register()`'s fast
    /// path, e.g. numeric `every(2, f)`; `superimpose`/`layer`) - rather than
    /// by a query.
    ///
    /// Three things mark a query: a bounded query turn (every Rust query
    /// entry opens one), a wrapper on the query stack (a score's own
    /// construction-time `queryArc`/`query` call pushes the wrapper it
    /// queries), and a query in progress in the core
    /// ([`rustel_core::query_in_progress`], which also covers the cycle-0
    /// probe `stepJoin`/`stepBind` run while their pattern is constructed).
    /// A callback reached through any of them keeps strudel.cc's
    /// throw-and-silence at that query's boundary; only a callback reached
    /// through none was called synchronously by the method the score is
    /// calling, where upstream's `func(pat)` would throw at that call.
    fn eager_callback_turn(&self, ctx: &Ctx<'_>) -> bool {
        self.query_turn_depth.get() == 0
            && !rustel_core::query_in_progress()
            && host_stack(ctx).is_ok_and(|stack| stack.is_empty())
    }

    /// The preamble of every transformer host call: whether it is EAGER (see
    /// [`Self::eager_callback_turn`]), or the sentinel refusal when it is
    /// eager behind an eager throw still held (see
    /// `refuse_eager_call_behind_a_held_throw`), in which case the callback
    /// must not run at all.
    fn enter_transformer_call(&self, ctx: &Ctx<'_>) -> Result<bool, String> {
        let eager = self.eager_callback_turn(ctx);
        refuse_eager_call_behind_a_held_throw(eager)?;
        Ok(eager)
    }

    /// Complete compatibility path for a pattern-transform callback.
    ///
    /// An EAGER throw (see [`Self::eager_callback_turn`]) is kept for the
    /// registered method that invoked the callback to rethrow, as the indexed
    /// path does: contained into the core's query-error channel it would
    /// have no `queryArc` boundary to take it, and the score would install
    /// with silence where the throwing branch was. Once one is held, a
    /// further eager call does not run its callback at all - see
    /// [`Self::enter_transformer_call`].
    fn call_pattern_compatibility(
        &self,
        id: CallbackId,
        pattern: Pattern,
    ) -> Result<Pattern, String> {
        let out = with_ctx(&self.ctx, |ctx| {
            let eager = self.enter_transformer_call(&ctx)?;
            Self::with_callback(&ctx, id, |f| {
                // The callback may KEEP this wrapper, so it owns the cells
                // its graph reaches - see `callback_argument`.
                let arg = callback_argument(&ctx, pattern.clone())
                    .map_err(|error| callback_host_error(&ctx, error, eager))?;
                let out: rquickjs::Value = f
                    .call((arg,))
                    .map_err(|error| callback_host_error(&ctx, error, eager))?;
                let object = out
                    .as_object()
                    .ok_or_else(|| "pattern transformer did not return a pattern".to_string())?;
                // The returned wrapper may itself reach callbacks - a nested
                // `every(fastcat(2, 3), y => …)` resolves its transformer at
                // QUERY time, so its cell must outlive this call. Unwrapping to
                // a bare `Pattern` and dropping the sidecar loses the only
                // owner, and the id becomes unresolvable on the next query.
                // Both wrapper families can come back.
                let _ = object;
                match unwrap_pattern(&out) {
                    Some((pattern, sidecar)) => {
                        harvest_sidecar(sidecar)
                            .map_err(|error| callback_host_error(&ctx, error, eager))?;
                        Ok(pattern)
                    }
                    None => Err("pattern transformer did not return a pattern".to_string()),
                }
            })
        });
        self.finish_bounded_query_host_call(out)
    }
}

impl CallbackHost for JsRuntime {
    fn log_query_error(&self, message: &str) {
        if !self.contained_query_errors_unlogged.get() {
            self.record_query_error(message);
        }
    }

    fn call_value(&self, id: CallbackId, v: &Value) -> Result<Value, String> {
        let out = with_ctx(&self.ctx, |ctx| {
            Self::with_callback(&ctx, id, |f| {
                let arg = to_js(&ctx, v).map_err(|e| describe_js_error(&ctx, e))?;
                let out: rquickjs::Value = f
                    .call((arg,))
                    .map_err(|error| describe_js_error(&ctx, error))?;
                // `fmap` stores the callback's exact JavaScript return as the
                // hap value.  Keep objects/arrays opaque here so a later JS
                // query observes the same identity; actual semantic consumers
                // and the outer Rust query boundary materialise `JsValue`.
                let (value, sidecars, _) = from_js_bridged_value(&ctx, &out)
                    .map_err(|error| describe_js_error(&ctx, error))?;
                harvest_sidecars(sidecars).map_err(|error| describe_js_error(&ctx, error))?;
                Ok(value)
            })
        });
        self.finish_bounded_query_host_call(out)
    }

    fn call_ref(&self, id: CallbackId) -> Result<Pattern, String> {
        let out = with_ctx(&self.ctx, |ctx| {
            Self::with_callback(&ctx, id, |function| {
                let value: rquickjs::Value = function
                    .call(())
                    .map_err(|error| describe_js_error(&ctx, error))?;
                let reify: Function = ctx
                    .globals()
                    .get("reify")
                    .map_err(|error| describe_js_error(&ctx, error))?;
                let reified: rquickjs::Value = reify
                    .call((value,))
                    .map_err(|error| describe_js_error(&ctx, error))?;
                let Some((pattern, sidecar)) = unwrap_pattern(&reified) else {
                    return Err("ref accessor result did not reify to a pattern".to_string());
                };
                harvest_sidecar(sidecar).map_err(|error| describe_js_error(&ctx, error))?;
                Ok(pattern)
            })
        });
        self.finish_bounded_query_host_call(out)
    }

    fn call_hap_predicate(&self, id: CallbackId, hap: &Hap) -> Result<bool, String> {
        let out = with_ctx(&self.ctx, |ctx| {
            Self::with_callback(&ctx, id, |function| {
                let argument =
                    callback_hap(&ctx, hap).map_err(|error| describe_js_error(&ctx, error))?;
                let value: rquickjs::Value = function
                    .call((argument,))
                    .map_err(|error| describe_js_error(&ctx, error))?;
                rquickjs::Coerced::<bool>::from_js(&ctx, value)
                    .map(|value| value.0)
                    .map_err(|error| describe_js_error(&ctx, error))
            })
        });
        self.finish_bounded_query_host_call(out)
    }

    fn call_time_predicate(&self, id: CallbackId, time: Fraction) -> Result<bool, String> {
        let out = with_ctx(&self.ctx, |ctx| {
            Self::with_callback(&ctx, id, |function| {
                let stack = host_stack(&ctx).map_err(|error| describe_js_error(&ctx, error))?;
                let factory: Function = stack
                    .as_object()
                    .get(FRACTION_FACTORY)
                    .map_err(|error| describe_js_error(&ctx, error))?;
                let argument: rquickjs::Value = factory
                    .call((fraction_seed(&ctx, time)
                        .map_err(|error| describe_js_error(&ctx, error))?,))
                    .map_err(|error| describe_js_error(&ctx, error))?;
                let value: rquickjs::Value = function
                    .call((argument,))
                    .map_err(|error| describe_js_error(&ctx, error))?;
                rquickjs::Coerced::<bool>::from_js(&ctx, value)
                    .map(|value| value.0)
                    .map_err(|error| describe_js_error(&ctx, error))
            })
        });
        self.finish_bounded_query_host_call(out)
    }

    fn call_span_transform(
        &self,
        id: CallbackId,
        span: rustel_core::TimeSpan,
    ) -> Result<rustel_core::TimeSpan, String> {
        let out = with_ctx(&self.ctx, |ctx| {
            Self::with_callback(&ctx, id, |function| {
                let stack = host_stack(&ctx).map_err(|error| describe_js_error(&ctx, error))?;
                let state_factory: Function = stack
                    .as_object()
                    .get(STATE_FACTORY)
                    .map_err(|error| describe_js_error(&ctx, error))?;
                let controls = rquickjs::Object::new(ctx.clone())
                    .map_err(|error| describe_js_error(&ctx, error))?;
                let state: rquickjs::Object = state_factory
                    .call((
                        span_seed(&ctx, span).map_err(|error| describe_js_error(&ctx, error))?,
                        controls,
                    ))
                    .map_err(|error| describe_js_error(&ctx, error))?;
                let argument: rquickjs::Value = state
                    .get("span")
                    .map_err(|error| describe_js_error(&ctx, error))?;
                let value: rquickjs::Value = function
                    .call((argument,))
                    .map_err(|error| describe_js_error(&ctx, error))?;
                let object = value
                    .as_object()
                    .ok_or_else(|| "withQuerySpan callback did not return a TimeSpan".to_owned())?;
                let begin = fraction_property(&ctx, object, "begin")
                    .map_err(|error| describe_js_error(&ctx, error))?;
                let end = fraction_property(&ctx, object, "end")
                    .map_err(|error| describe_js_error(&ctx, error))?;
                Ok(rustel_core::TimeSpan::new(begin, end))
            })
        });
        self.finish_bounded_query_host_call(out)
    }

    fn call_pick_lookup(&self, id: CallbackId) -> Result<rustel_core::PickLookup, String> {
        let out = with_ctx(&self.ctx, |ctx| {
            Self::with_js_value(&ctx, id, |value| {
                let (lookup, sidecars) = native_pick_lookup(&ctx, &value)
                    .map_err(|error| describe_js_error(&ctx, error))?;
                harvest_sidecars(sidecars).map_err(|error| describe_js_error(&ctx, error))?;
                Ok(lookup)
            })
        });
        self.finish_bounded_query_host_call(out)
    }

    fn call_materialize_value(&self, id: CallbackId) -> Result<Value, String> {
        let out = with_ctx(&self.ctx, |ctx| {
            Self::with_js_value(&ctx, id, |value| {
                let (value, sidecars) = materialize_js_value(&ctx, &value)
                    .map_err(|error| describe_js_error(&ctx, error))?;
                harvest_sidecars(sidecars).map_err(|error| describe_js_error(&ctx, error))?;
                Ok(value)
            })
        });
        self.finish_bounded_query_host_call(out)
    }

    /// `every(4, x => x.fast(2))` - apply a user function to a pattern.
    ///
    /// The pattern is handed over as a real `NativePatternWrapper`, so the
    /// callback can use the whole host surface on it, and the returned wrapper
    /// is unwrapped back to a `Pattern`. Anything else coming back is a hard
    /// error rather than a silent `silence`: returning nothing from a
    /// transformer is a user bug worth reporting, not a pattern.
    fn call_pattern(&self, id: CallbackId, pattern: Pattern) -> Result<Pattern, String> {
        self.call_pattern_compatibility(id, pattern)
    }

    fn call_pattern_ir(
        &self,
        id: CallbackId,
        program: &rustel_core::callback_ir::PatternTransformProgram,
        pattern: Pattern,
    ) -> Result<Pattern, String> {
        let mode = self.pattern_transform_ir_mode.get();
        let mut stats = self.pattern_transform_ir_stats.get();
        stats.calls = stats.calls.saturating_add(1);
        match mode {
            PatternTransformIrMode::Auto => {
                stats.native_executions = stats.native_executions.saturating_add(1);
            }
            PatternTransformIrMode::Compatibility => {
                stats.compatibility_executions = stats.compatibility_executions.saturating_add(1);
            }
            PatternTransformIrMode::DualRun => {
                stats.native_executions = stats.native_executions.saturating_add(1);
                stats.compatibility_executions = stats.compatibility_executions.saturating_add(1);
                stats.dual_runs = stats.dual_runs.saturating_add(1);
            }
        }
        // Publish before a compatibility call can re-enter this runtime and
        // update the same counters. Holding a stale snapshot across QuickJS
        // would overwrite nested callback evidence on return.
        self.pattern_transform_ir_stats.set(stats);

        match mode {
            PatternTransformIrMode::Auto => Ok(program.apply(pattern)),
            PatternTransformIrMode::Compatibility => self.call_pattern_compatibility(id, pattern),
            PatternTransformIrMode::DualRun => {
                let candidate = program.apply(pattern.clone());
                match self.call_pattern_compatibility(id, pattern) {
                    Ok(compatibility) if candidate.same_graph_handle(&compatibility) => {
                        Ok(compatibility)
                    }
                    Ok(_) => {
                        let mut stats = self.pattern_transform_ir_stats.get();
                        stats.mismatches = stats.mismatches.saturating_add(1);
                        self.pattern_transform_ir_stats.set(stats);
                        Err(format!(
                            "pattern callback IR {id} disagreed with its JavaScript fallback"
                        ))
                    }
                    Err(error) => Err(error),
                }
            }
        }
    }

    /// `echoWith(times, time, (pattern, index) => result)`.
    ///
    /// `listRange(...).map(callback)` finishes every callback call
    /// before the spread call to `stack` starts reifying results. Keep the raw
    /// QuickJS values rooted in `returned` for phase one, then use the exact
    /// list-constructor reifier in phase two. This order is observable when a
    /// later callback mutates an earlier returned array or replaces the string
    /// parser, and a throw must prevent phase two altogether.
    fn call_pattern_indexed_batch(
        &self,
        id: CallbackId,
        patterns: Vec<(Pattern, i64)>,
    ) -> Result<Vec<Pattern>, String> {
        let out = with_ctx(&self.ctx, |ctx| {
            let eager = self.enter_transformer_call(&ctx)?;
            Self::with_callback(&ctx, id, |function| {
                let mut returned = Vec::new();
                returned
                    .try_reserve_exact(patterns.len())
                    .map_err(|_| "indexed callback result allocation failed".to_string())?;

                // Phase 1: invoke every callback, retaining its exact raw JS
                // result. `?` stops at the first throw, as Array.prototype.map
                // does, and no result has been reified yet.
                for (pattern, index) in patterns {
                    let argument = callback_argument(&ctx, pattern)
                        .map_err(|error| callback_host_error(&ctx, error, eager))?;
                    let result: rquickjs::Value = function
                        .call((argument, index as f64))
                        .map_err(|error| callback_host_error(&ctx, error, eager))?;
                    returned.push(result);
                }

                // Phase 2: `stack(...returned)` applies recursive
                // list-constructor reification in result order.
                let mut reified = Vec::new();
                reified
                    .try_reserve_exact(returned.len())
                    .map_err(|_| "indexed callback reification allocation failed".to_string())?;
                let mut returned_sidecars = Vec::new();
                for result in &returned {
                    let (pattern, sidecars) = reify_list_element_bridged(&ctx, result)
                        .map_err(|error| callback_host_error(&ctx, error, eager))?;
                    returned_sidecars
                        .try_reserve(sidecars.len())
                        .map_err(|_| "indexed callback sidecar allocation failed".to_string())?;
                    returned_sidecars.extend(sidecars);
                    reified.push(pattern);
                }
                // `stack(...returned)` consumes one logical result batch. A
                // later returned wrapper may explicitly own an id excluded by
                // an earlier one, so publish only after all owners are known.
                harvest_sidecars(returned_sidecars)
                    .map_err(|error| callback_host_error(&ctx, error, eager))?;
                Ok(reified)
            })
        });
        // One batch is one synchronous callback-host operation. Deadline,
        // cancellation, heap and pending-job refusals remain typed and cannot
        // be laundered into an ordinary empty pattern.
        self.finish_bounded_query_host_call(out)
    }

    /// `polyBind(x => …)` / `stepBind(x => …)`.
    ///
    /// The argument shape follows the outer hap, exactly as `fmap`'s
    /// does: a transpiled `pure("bd")` carries a pattern, a `pure('bd')`
    /// carries the string.
    ///
    /// A return that is not a pattern is `Ok(None)`, NOT an error - it is
    /// stored as the hap's value and the join is what fails. Only a throw
    /// is `Err`.
    fn call_bind(&self, id: CallbackId, arg: BindArg<'_>) -> Result<BindResult, String> {
        let out = with_ctx(&self.ctx, |ctx| {
            Self::with_callback(&ctx, id, |f| {
                let arg: rquickjs::Value = match arg {
                    BindArg::Value(value) => {
                        to_js(&ctx, value).map_err(|error| describe_js_error(&ctx, error))?
                    }
                    // The callback may KEEP this wrapper - `globalThis.x = p`
                    // outlives the graph it came from - so it owns the cells
                    // its pattern reaches. See `callback_argument`.
                    BindArg::Pattern(pattern) => callback_argument(&ctx, pattern)
                        .map_err(|error| describe_js_error(&ctx, error))?
                        .into_value(),
                };
                let out: rquickjs::Value = f
                    .call((arg,))
                    .map_err(|error| describe_js_error(&ctx, error))?;
                match unwrap_pattern(&out) {
                    Some((pattern, sidecar)) => {
                        // The returned graph may itself reach callbacks; they
                        // must outlive this call.
                        harvest_sidecar(sidecar).map_err(|error| describe_js_error(&ctx, error))?;
                        Ok(BindResult::Pattern(pattern))
                    }
                    // Not a pattern: it becomes the hap's VALUE, and
                    // the join's error message is worded from it.
                    None => {
                        let (value, sidecars) = materialize_js_value(&ctx, &out)
                            .map_err(|error| describe_js_error(&ctx, error))?;
                        harvest_sidecars(sidecars)
                            .map_err(|error| describe_js_error(&ctx, error))?;
                        Ok(BindResult::Value(value))
                    }
                }
            })
        });
        self.finish_bounded_query_host_call(out)
    }

    /// `arpWith(haps => ...)` - pass one congruent chord as actual Hap-shaped
    /// JavaScript objects and preserve the callback's returned pattern.
    ///
    /// A bare Hap (or duck-shaped object) is reified to `pure(value)`; a
    /// returned wrapper stays a pattern, including its timing and callback
    /// sidecar. The latter is harvested into the live query frame because the
    /// core node queries it immediately after this call.
    fn call_haps(&self, id: CallbackId, haps: &[Hap]) -> Result<Pattern, String> {
        let out = with_ctx(&self.ctx, |ctx| {
            Self::with_callback(&ctx, id, |f| {
                let array = rquickjs::Array::new(ctx.clone())
                    .map_err(|error| describe_js_error(&ctx, error))?;
                for (index, hap) in haps.iter().enumerate() {
                    array
                        .set(
                            index,
                            callback_hap(&ctx, hap)
                                .map_err(|error| describe_js_error(&ctx, error))?,
                        )
                        .map_err(|error| describe_js_error(&ctx, error))?;
                }
                let out: rquickjs::Value = f
                    .call((array,))
                    .map_err(|error| describe_js_error(&ctx, error))?;
                // The SAME lexical `reify` applies after the callback.
                // A returned primitive string therefore reaches the mutable
                // string parser at query time, while objects/functions remain
                // ordinary pure values and returned Patterns keep their cells.
                let (pattern, sidecar) =
                    reify_bridged(&ctx, &out).map_err(|error| describe_js_error(&ctx, error))?;
                harvest_sidecar(sidecar).map_err(|error| describe_js_error(&ctx, error))?;
                Ok(pattern)
            })
        });
        self.finish_bounded_query_host_call(out)
    }

    fn call_query(&self, id: CallbackId, state: &State) -> Result<Vec<Hap>, String> {
        // An exhausted heap is a RESOURCE refusal, not a user throw. The
        // signal is STRUCTURAL - the allocator denied a request - so no message
        // can imitate it and no message is needed to detect it. Sticky across
        // nesting: a callback that queries another pattern must not clear its
        // caller's refusal, so this only reports, and the outer boundary
        // clears.
        let out = self.call_query_inner(id, state);
        self.finish_bounded_query_host_call(out)
    }
}
