//! Score evaluation, live replacement, and rollback.

use super::*;

impl Session {
    /// Compile mini-notation via the Rust parser and install it.
    pub fn evaluate_mini(&mut self, source: &str) -> Result<(), RuntimeError> {
        self.evaluate_mini_at(source, 0.0, true)
    }

    /// Whether a setup this session has run picks sample variants: sets
    /// `n` anywhere, or maps values through code that could. A helper it
    /// defines can set `n` for any score after it, so every text is then
    /// treated as able to play any variant of every sound it names - see
    /// [`crate::sounds::variant_selection`]. It stays set for the session:
    /// the heap keeps what a setup defined whatever setup runs next.
    pub fn prebake_selects_variants(&self) -> bool {
        self.prebake_selects_variants
    }

    /// Execute setup JavaScript in this Session's existing QuickJS heap.
    ///
    /// This is deliberately separate from [`Session::evaluate`]: setup may
    /// return anything and must not replace the active graph, restart transport, change
    /// the scheduler generation, or become mini fallback input. Deliberate
    /// global/prototype/register mutations survive for later scores, and -
    /// matching one persistent JavaScript realm - mutations completed before a
    /// later throw remain. Sample and preload host effects apply only after the
    /// complete setup turn succeeds.
    pub fn evaluate_prebake(&mut self, source: &str) -> Result<(), RuntimeError> {
        self.evaluate_prebake_inner(source, 0.0, PREBAKE_CPU_BUDGET, None)
    }

    /// Cancellable setup evaluation for product routes with an external stop
    /// signal. Library callers that do not have such a signal use
    /// [`Session::evaluate_prebake`].
    pub fn evaluate_prebake_cancellable(
        &mut self,
        source: &str,
        cancellation: &std::sync::atomic::AtomicBool,
    ) -> Result<(), RuntimeError> {
        self.evaluate_prebake_inner(source, 0.0, PREBAKE_CPU_BUDGET, Some(cancellation))
    }

    /// [`Session::evaluate_prebake_cancellable`] while a score plays: a
    /// recovery resumes the restored score at `now` on the transport clock.
    pub fn evaluate_prebake_cancellable_at(
        &mut self,
        source: &str,
        now: f64,
        cancellation: &std::sync::atomic::AtomicBool,
    ) -> Result<(), RuntimeError> {
        self.evaluate_prebake_inner(source, now, PREBAKE_CPU_BUDGET, Some(cancellation))
    }

    fn evaluate_prebake_inner(
        &mut self,
        source: &str,
        now: f64,
        budget: Duration,
        cancellation: Option<&AtomicBool>,
    ) -> Result<(), RuntimeError> {
        self.with_panic_recovery(now, |session| {
            session.evaluate_prebake_guarded(source, budget, cancellation, None)
        })
    }

    pub(super) fn evaluate_prebake_guarded(
        &mut self,
        source: &str,
        budget: std::time::Duration,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
        expected_error: Option<&str>,
    ) -> Result<(), RuntimeError> {
        #[cfg(any(test, feature = "test-support"))]
        self.panic_if_injected(SessionPanicPoint::Evaluation);
        self.claim_core_settings();
        self.ensure_bindings()?;
        // Before it runs: what a setup defines before a later throw stays
        // defined, helpers that set `n` included.
        if crate::sounds::variant_selection(source).is_some() {
            self.prebake_selects_variants = true;
        }
        // Setup/prebake is the canonical home for voicing dictionaries and
        // MIDI maps. Install those configuration globals before executing it,
        // just as ordinary score evaluation does.
        self.ensure_voicings()?;
        // Setup code is not the active editor document. Locations created
        // here are bare byte offsets and would be indistinguishable from score
        // offsets if a helper returned its own mini-notation pattern later.
        let transpile_options = TranspileOptions {
            emit_mini_locations: false,
            ..TranspileOptions::default()
        };
        let result = match cancellation {
            Some(flag) => self.js.evaluate_prelude_with_effects_cancellable(
                source,
                &transpile_options,
                budget,
                flag,
            ),
            None => self
                .js
                .evaluate_prelude_with_effects(source, &transpile_options, budget),
        };
        let effects = match result {
            Ok((_, effects)) => {
                if expected_error.is_some() {
                    return Err(RuntimeError::Js(
                        "setup replay succeeded after an earlier error".into(),
                    ));
                }
                effects
            }
            Err(rustel_jsruntime::QueryError::Message(message)) => {
                self.retain_recovery_prebake(source, Some(Arc::from(message.as_str())));
                return if expected_error == Some(message.as_str()) {
                    Ok(())
                } else {
                    Err(RuntimeError::Js(message))
                };
            }
            Err(error) => return Err(error.into()),
        };
        // A setup file is the natural home for `preload(...)`: it runs
        // before the score, which is exactly when the files should start
        // arriving. Registrations and preloads enter one ordered loader job
        // so an override is visible before its preload resolves.
        self.apply_sample_effects(effects.samples, effects.preload);
        self.retain_recovery_prebake(source, None);
        Ok(())
    }

    fn retain_recovery_prebake(&mut self, source: &str, error: Option<Arc<str>>) {
        // Keep completed mutations before an ordinary throw. Limits are not replayed.
        let prebakes = Arc::make_mut(&mut self.recovery_prebakes);
        prebakes.retain(|earlier| &*earlier.source != source);
        prebakes.push(recovery::RecoveryPrebake {
            source: Arc::from(source),
            error,
            voicing_identity: self
                .js
                .snapshot_published_runtime_settings()
                .host_voicing_identity(),
        });
    }

    /// Attempt one watched setup evaluation within the audio work this
    /// scheduler can currently afford.
    ///
    /// `Deferred` is deliberately not an error: no JavaScript ran, so the
    /// stable file identity must remain retryable after the producer refills
    /// the old generation's horizon. The two-second ceiling remains a maximum
    /// for unusually large configured horizons, not a replacement for the
    /// remaining-horizon calculation.
    pub(crate) fn evaluate_live_prebake_cancellable(
        &mut self,
        source: &str,
        now: f64,
        continuation_reserve: std::time::Duration,
        cancellation: &std::sync::atomic::AtomicBool,
    ) -> Result<LivePrebakeAttempt, RuntimeError> {
        let Some(budget) = self.live_prebake_budget_at(now, continuation_reserve)? else {
            return Ok(LivePrebakeAttempt::Deferred);
        };
        #[cfg(any(feature = "device-audio", test))]
        let started = Instant::now();
        let result = self.evaluate_prebake_inner(source, now, budget, Some(cancellation));
        #[cfg(any(feature = "device-audio", test))]
        self.record_producer_phase(ProducerPhase::Evaluation, started.elapsed());
        result?;
        Ok(LivePrebakeAttempt::Applied)
    }

    pub(super) fn live_prebake_budget_at(
        &self,
        now: f64,
        continuation_reserve: std::time::Duration,
    ) -> Result<Option<std::time::Duration>, RuntimeError> {
        if !now.is_finite() || now < 0.0 {
            return Err(RuntimeError::Message(format!(
                "live setup clock must be a finite non-negative number, got {now}"
            )));
        }
        if continuation_reserve.is_zero() {
            return Err(RuntimeError::Message(
                "live setup continuation reserve must be greater than zero".into(),
            ));
        }
        Ok(self
            .scheduler
            .affordable_deadline(now, continuation_reserve)
            .filter(|budget| *budget >= MIN_LIVE_PREBAKE_CPU_BUDGET)
            .map(|budget| budget.min(PREBAKE_CPU_BUDGET)))
    }

    #[cfg(any(feature = "device-audio", test))]
    pub(super) fn live_score_budget_at(
        &self,
        now: f64,
        continuation_reserve: Duration,
    ) -> Result<Option<Duration>, RuntimeError> {
        Self::validate_live_score_clock("budget", now)?;
        if continuation_reserve.is_zero() {
            return Err(RuntimeError::Message(
                "live score continuation reserve must be greater than zero".into(),
            ));
        }
        // No sounding score yet (watch started on a broken file): there is
        // no old audible cover to derive a deadline from. Same startup
        // ceiling as the first CLI evaluate, so the next valid save can
        // install instead of deferring forever once the device clock has
        // run past a never-filled horizon.
        if self.last_source.is_none() {
            return Ok(Some(SCORE_CPU_BUDGET));
        }
        // When the horizon cannot afford the 25 ms floor, what matters is
        // whether cover is left. With cover left, defer: the identity stays
        // retryable and the horizon refills within milliseconds. With the
        // horizon spent there is nothing to protect, and a save refused now
        // is refused again on every later attempt, so the set would stay
        // on its last good score. Spend a recovery slice instead: a late
        // install is better than no install.
        let affordable = self
            .scheduler
            .affordable_deadline(now, continuation_reserve)
            .filter(|budget| *budget >= MIN_LIVE_SCORE_CPU_BUDGET);
        Ok(match affordable {
            Some(budget) => Some(budget.min(SCORE_CPU_BUDGET)),
            None if self.scheduler.horizon_remaining(now) > 0.0 => None,
            None => Some(LIVE_SCORE_RECOVERY_BUDGET.min(SCORE_CPU_BUDGET)),
        })
    }

    #[cfg(any(feature = "device-audio", test))]
    pub(super) fn live_query_budget_at(
        &self,
        now: f64,
        continuation_reserve: Duration,
        mode: LiveQueryBudgetMode,
    ) -> Result<Option<Duration>, RuntimeError> {
        if !now.is_finite() || now < 0.0 {
            return Err(RuntimeError::Message(format!(
                "live query clock must be a finite non-negative number, got {now}"
            )));
        }
        if continuation_reserve.is_zero() {
            return Err(RuntimeError::Message(
                "live query continuation reserve must be greater than zero".into(),
            ));
        }

        match mode {
            LiveQueryBudgetMode::InitialPrefill => {
                // The device is already running but no generation has ever
                // been filled, so there is no old audible cover from which to
                // derive a deadline. Retain the fixed startup ceiling; this is
                // bounded startup latency, not a continuity claim.
                return Ok(Some(QUERY_JS_CPU_BUDGET));
            }
            LiveQueryBudgetMode::ReplacementPrefill => {
                // One atomic live query produces at most one cycle. Give it a
                // bounded share of that cycle so viable low-tempo graphs are
                // not judged by the same 25 ms slice as extreme tempos.
                let cps = self.scheduler.cps();
                let sustainable = if cps.is_finite() && cps > 0.0 {
                    Duration::from_secs_f64(
                        (MAX_LIVE_IMPURE_QUERY_SPAN_CYCLES / cps
                            * LIVE_REPLACEMENT_QUERY_COMPUTE_SHARE)
                            .min(LIVE_QUERY_RECOVERY_BUDGET.as_secs_f64()),
                    )
                } else {
                    MIN_LIVE_QUERY_JS_BUDGET
                };
                return Ok(Some(
                    continuation_reserve
                        .max(MIN_LIVE_QUERY_JS_BUDGET)
                        .max(sustainable)
                        .min(QUERY_JS_CPU_BUDGET),
                ));
            }
            LiveQueryBudgetMode::Steady => {}
        }

        Ok(self
            .scheduler
            .affordable_deadline(now, continuation_reserve)
            .filter(|budget| *budget >= MIN_LIVE_QUERY_JS_BUDGET)
            .map(|budget| budget.min(QUERY_JS_CPU_BUDGET)))
    }

    #[cfg(any(feature = "device-audio", test))]
    fn validate_live_score_clock(phase: &str, now: f64) -> Result<(), RuntimeError> {
        if !now.is_finite() || now < 0.0 {
            return Err(RuntimeError::Message(format!(
                "live score {phase} clock must be a finite non-negative number, got {now}"
            )));
        }
        Ok(())
    }

    fn evaluate_mini_at(
        &mut self,
        source: &str,
        now: f64,
        restart_transport: bool,
    ) -> Result<(), RuntimeError> {
        self.with_panic_recovery(now, |session| {
            session.evaluate_mini_guarded(source, now, restart_transport)
        })
    }

    fn evaluate_mini_guarded(
        &mut self,
        source: &str,
        now: f64,
        restart_transport: bool,
    ) -> Result<(), RuntimeError> {
        #[cfg(any(test, feature = "test-support"))]
        self.panic_if_injected(SessionPanicPoint::Evaluation);
        let settings =
            (!self.core_settings_owned).then(|| self.js.snapshot_ambient_runtime_settings());
        let parsed = || rustel_mini::mini(source);
        let pattern = match settings.as_ref() {
            Some(settings) => settings.with(parsed),
            None => self.js.with_runtime_settings(parsed),
        }
        .map_err(|e| RuntimeError::Mini(e.to_string()))?;
        // Same install door as JS so a live mini reload keeps the timeline
        // and publishes a takeover cursor. `set_pattern_at` alone re-anchors
        // at (now, queried_to) and leaves takeover at 0 - a dropped bar.
        self.install_evaluated_score(
            EvaluatedScore::MiniFallback(pattern),
            source,
            now,
            restart_transport,
            settings,
        )
    }

    /// Transpile and evaluate a JavaScript score. Falls back to Rust mini when
    /// the JS host cannot install an active graph for the source. The two-second
    /// limit covers synchronous QuickJS score execution; transpilation and the
    /// Rust mini fallback are explicitly outside that deadline.
    pub fn evaluate(&mut self, source: &str) -> Result<(), RuntimeError> {
        self.evaluate_cancellable(source, &NEVER_CANCELLED)
    }

    /// The product-facing score evaluator with caller-owned cancellation.
    ///
    /// A typed deadline or cancellation is returned directly and can never be
    /// reclassified as a mini parse attempt. Only an ordinary JavaScript
    /// message keeps the compatibility fallback used by [`Session::evaluate`].
    pub fn evaluate_cancellable(
        &mut self,
        source: &str,
        cancellation: &AtomicBool,
    ) -> Result<(), RuntimeError> {
        self.evaluate_at_cancellable(
            source,
            0.0,
            true,
            SCORE_CPU_BUDGET,
            cancellation,
            NoPatternPolicy::InstallSilence,
            true,
        )
    }

    /// Evaluate a score once for validation, refusing the mini-notation
    /// compatibility fallback that [`Session::evaluate_cancellable`] uses to
    /// recover a failed JavaScript evaluation by re-parsing the source as
    /// bare mini-notation.
    ///
    /// Used by `check` and `query` so unresolved names and thrown errors remain
    /// validation failures instead of becoming notes in a mini pattern.
    pub fn evaluate_no_fallback_cancellable(
        &mut self,
        source: &str,
        cancellation: &AtomicBool,
    ) -> Result<(), RuntimeError> {
        self.evaluate_at_cancellable(
            source,
            0.0,
            true,
            SCORE_CPU_BUDGET,
            cancellation,
            NoPatternPolicy::InstallSilence,
            false,
        )
    }

    /// Evaluate an initial product score and require it to produce a Pattern.
    ///
    /// Live reloads deliberately install silence when every pattern is
    /// commented out. A plain file-to-speakers invocation has no future save
    /// to wait for, so accepting the same empty result there would open the
    /// audio device and run forever while playing nothing.
    pub fn evaluate_playable_cancellable(
        &mut self,
        source: &str,
        cancellation: &AtomicBool,
    ) -> Result<(), RuntimeError> {
        if source.trim().is_empty() {
            return Err(RuntimeError::NoPattern);
        }
        self.evaluate_at_cancellable(
            source,
            0.0,
            true,
            SCORE_CPU_BUDGET,
            cancellation,
            NoPatternPolicy::Reject,
            true,
        )
    }

    /// `allow_mini_fallback` is a separate argument, not derived from
    /// `restart_transport`, so [`Session::evaluate_no_fallback_cancellable`]
    /// can refuse the fallback on the same restart-transport path an ordinary
    /// one-shot evaluation uses.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn evaluate_at_cancellable(
        &mut self,
        source: &str,
        now: f64,
        restart_transport: bool,
        budget: Duration,
        cancellation: &AtomicBool,
        no_pattern: NoPatternPolicy,
        allow_mini_fallback: bool,
    ) -> Result<(), RuntimeError> {
        self.with_panic_recovery(now, |session| {
            session.evaluate_at_cancellable_guarded(
                source,
                now,
                restart_transport,
                budget,
                cancellation,
                no_pattern,
                allow_mini_fallback,
            )
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn evaluate_at_cancellable_guarded(
        &mut self,
        source: &str,
        now: f64,
        restart_transport: bool,
        budget: Duration,
        cancellation: &AtomicBool,
        no_pattern: NoPatternPolicy,
        allow_mini_fallback: bool,
    ) -> Result<(), RuntimeError> {
        let evaluated = self.evaluate_score_cancellable(
            source,
            budget,
            cancellation,
            allow_mini_fallback,
            no_pattern,
        )?;
        self.install_evaluated_score(evaluated, source, now, restart_transport, None)
    }

    fn evaluate_score_cancellable(
        &mut self,
        source: &str,
        budget: Duration,
        cancellation: &AtomicBool,
        allow_mini_fallback: bool,
        no_pattern: NoPatternPolicy,
    ) -> Result<EvaluatedScore, RuntimeError> {
        self.evaluate_score_cancellable_seeded(
            source,
            budget,
            cancellation,
            allow_mini_fallback,
            no_pattern,
            None,
        )
    }

    fn evaluate_score_cancellable_seeded(
        &mut self,
        source: &str,
        budget: Duration,
        cancellation: &AtomicBool,
        allow_mini_fallback: bool,
        no_pattern: NoPatternPolicy,
        seed: Option<&RuntimeSettings>,
    ) -> Result<EvaluatedScore, RuntimeError> {
        #[cfg(any(test, feature = "test-support"))]
        self.panic_if_injected(SessionPanicPoint::Evaluation);
        self.claim_core_settings();
        // scale/transpose/scaleTranspose are native registry combinators in
        // rustel-core. The voicings bundle still installs because chord and
        // voicing do not yet have complete native implementations.
        self.ensure_bindings()?;
        self.ensure_voicings()?;
        // A bounded first-install probe may reject the score after JavaScript
        // has published its slider and voicing candidates. Save the empty
        // baseline so that refusal can restore all three active roots.
        if self.generation() == 0 {
            self.js.snapshot_active_as_last_good();
        }
        let options = TranspileOptions {
            widget_methods: rustel_transpiler::VISUAL_WIDGET_METHODS
                .iter()
                .map(|method| (*method).to_owned())
                .collect(),
            ..TranspileOptions::default()
        };
        let evaluated = match seed {
            Some(seed) => self
                .js
                .evaluate_score_candidate_with_effects_cancellable_seeded(
                    source,
                    &options,
                    budget,
                    cancellation,
                    seed,
                ),
            None => self.js.evaluate_score_candidate_with_effects_cancellable(
                source,
                &options,
                budget,
                cancellation,
            ),
        };
        match evaluated {
            // `effects.hydra` is taken below, and only the visuals build has
            // one to take.
            #[cfg_attr(not(feature = "hydra"), allow(unused_mut))]
            Ok((_, mut effects, settings)) => {
                // The recording is taken here, not at the commit seam, because
                // the arms below drop `effects` for any score that names no
                // pattern - and a visuals-only score is exactly that. It is
                // promoted to `pending_hydra` only once something commits, so
                // a candidate the replacement probe rejects never reaches a
                // window. `None` and `Some(empty)` both mean "close": the
                // score either never mentioned Hydra or called `clearHydra()`.
                #[cfg(feature = "hydra")]
                {
                    self.staged_hydra = Some(effects.hydra.take().unwrap_or_default());
                }
                match self.js.active_pattern() {
                    Some(pattern) => Ok(EvaluatedScore::JavaScript {
                        pattern,
                        effects,
                        settings,
                    }),
                    None if no_pattern == NoPatternPolicy::Reject => Err(RuntimeError::NoPattern),
                    None if allow_mini_fallback => {
                        self.evaluate_with_fallback(source, "active pattern missing after eval")
                    }
                    // Commenting out every pattern mutes the score instead of
                    // retaining the previous part.
                    None => Ok(EvaluatedScore::MiniFallback(rustel_core::silence())),
                }
            }
            // Same rule on the live path, which does not allow the fallback:
            // the score evaluated, it simply produced no pattern. A stop is
            // still a stop, so cancellation is checked first.
            Err(rustel_jsruntime::QueryError::Message(ref message))
                if Self::score_holds_no_pattern_message(source, message).is_some()
                    && !cancellation.load(std::sync::atomic::Ordering::Relaxed) =>
            {
                match no_pattern {
                    NoPatternPolicy::Reject => Err(RuntimeError::NoPattern),
                    NoPatternPolicy::InstallSilence => {
                        Ok(EvaluatedScore::MiniFallback(rustel_core::silence()))
                    }
                }
            }
            // Only the final value conversion has this exact message.
            // Exceptions inside the score carry their type and source location.
            Err(rustel_jsruntime::QueryError::Message(message))
                if message
                    == "Error converting from js 'function' into type 'NativePatternWrapper'" =>
            {
                if cancellation.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err(RuntimeError::Cancelled);
                }
                Err(RuntimeError::Js(
                    "the score ends on a function, not a pattern - call it (for example .noise(0.5)) or end on a pattern".into(),
                ))
            }
            Err(rustel_jsruntime::QueryError::Message(message)) if allow_mini_fallback => {
                // Cancellation must stop evaluation without retrying through
                // the native parser, which would delay the caller's stop.
                if cancellation.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err(RuntimeError::Cancelled);
                }
                self.evaluate_with_fallback(source, &message)
            }
            Err(rustel_jsruntime::QueryError::Policy(message)) => Err(RuntimeError::Js(message)),
            Err(rustel_jsruntime::QueryError::Limit(limit)) => Err(limit.into()),
            Err(rustel_jsruntime::QueryError::Message(message)) => Err(RuntimeError::Js(message)),
        }
    }

    fn evaluate_with_fallback(
        &self,
        source: &str,
        js_err: &str,
    ) -> Result<EvaluatedScore, RuntimeError> {
        // A score with no pattern installs silence, allowing a live edit to
        // mute the current part by commenting it out.
        if Self::score_holds_no_pattern_message(source, js_err).is_some() {
            return Ok(EvaluatedScore::MiniFallback(rustel_core::silence()));
        }
        if let Some(mini_src) = extract_mini_fallback(source) {
            match self
                .js
                .with_runtime_settings(|| rustel_mini::mini(&mini_src))
            {
                Ok(pattern) => Ok(EvaluatedScore::MiniFallback(pattern)),
                Err(mini_err) => Err(RuntimeError::Message(format!(
                    "javascript eval failed ({js_err}); mini fallback also failed ({mini_err})"
                ))),
            }
        } else {
            Err(RuntimeError::Js(js_err.to_string()))
        }
    }

    /// A score with nothing to play is the ordinary state of a set that has not
    /// been written yet - `--watch` opens on an empty buffer by design, and a
    /// first save is often just `setCpm(150/4)`. Neither deserves the host's
    /// type-conversion text ("Error converting from js 'undefined' into type
    /// 'NativePatternWrapper'; mini fallback also failed"), which tells the
    /// user nothing about a file they have simply not finished typing.
    pub(super) fn score_holds_no_pattern_message(source: &str, js_err: &str) -> Option<String> {
        if source.trim().is_empty() {
            return Some("no pattern yet: save a pattern to start the set".into());
        }
        // `undefined` SPECIFICALLY, not any failed conversion. A score whose
        // last expression is undefined is one that named no pattern: an empty
        // file, a `setcps` line on its own, or every `$:` commented out --
        // all of which mute rather than fail.
        //
        // A number, object or null reaching here is a different thing: the
        // score produced a value that is not a pattern, which is a mistake
        // worth reporting. Matching the whole conversion family swallowed
        // those into silence too, so `42` installed quietly instead of saying
        // anything.
        if js_err.contains("from js 'undefined' into type 'NativePatternWrapper'") {
            return Some(
                "this save defines no pattern, so there is nothing to play yet".to_string(),
            );
        }
        // register() intentionally returns the function it just published (or
        // an object for an array of aliases). A buffer which currently ends
        // there is the definition-only equivalent of `setCpm(...)`: valid
        // score setup with nothing to play yet. Keep this exact to the final
        // AST expression so an unrelated `() => 42` is still reported as a
        // non-pattern mistake.
        (js_err.contains("into type 'NativePatternWrapper'")
            && rustel_transpiler::registration_hints(source).final_expression_is_registration)
            .then(|| {
                "this save registers a function but defines no pattern to play yet".to_string()
            })
    }

    fn install_evaluated_score(
        &mut self,
        evaluated: EvaluatedScore,
        source: &str,
        now: f64,
        restart_transport: bool,
        candidate_settings: Option<RuntimeSettings>,
    ) -> Result<(), RuntimeError> {
        let probe = self.probe_evaluated_replacement(
            &evaluated,
            restart_transport,
            candidate_settings.as_ref(),
            false,
        );
        #[cfg(feature = "hydra")]
        if probe.is_err() {
            self.staged_hydra = None;
        }
        probe?;
        self.commit_evaluated_score(
            evaluated,
            source,
            now,
            restart_transport,
            candidate_settings,
            Duration::ZERO,
        )
    }

    #[cfg(any(feature = "device-audio", test))]
    fn install_evaluated_score_with_clock<F>(
        &mut self,
        evaluated: EvaluatedScore,
        source: &str,
        earliest_now: Option<f64>,
        restart_transport: bool,
        candidate_settings: Option<RuntimeSettings>,
        mut clock: F,
    ) -> Result<(), RuntimeError>
    where
        F: FnMut() -> f64,
    {
        if let Some(earliest_now) = earliest_now {
            Self::validate_live_score_clock("budget", earliest_now)?;
        }
        let realtime_cps = Self::evaluated_realtime_cps(&evaluated, self.scheduler.cps());
        let probe_started = Instant::now();
        let probe = self.probe_evaluated_replacement(
            &evaluated,
            restart_transport,
            candidate_settings.as_ref(),
            true,
        );
        let probe_elapsed = probe_started.elapsed();
        self.record_producer_phase(ProducerPhase::ReplacementProbe, probe_elapsed);
        #[cfg(feature = "hydra")]
        if probe.is_err() {
            self.staged_hydra = None;
        }
        probe?;
        let sampled_now = clock();
        let now = match earliest_now {
            Some(earliest_now) if !sampled_now.is_finite() || sampled_now < earliest_now => {
                earliest_now
            }
            _ => {
                Self::validate_live_score_clock("install", sampled_now)?;
                sampled_now
            }
        };
        self.commit_evaluated_score(
            evaluated,
            source,
            now,
            restart_transport,
            candidate_settings,
            Self::live_replacement_takeover_headroom(realtime_cps, probe_elapsed),
        )
    }

    fn evaluated_realtime_cps(evaluated: &EvaluatedScore, fallback_cps: f64) -> Option<f64> {
        let cps = match evaluated {
            EvaluatedScore::JavaScript { effects, .. } => effects.cps.unwrap_or(fallback_cps),
            EvaluatedScore::MiniFallback(_) => fallback_cps,
        };
        (cps.is_finite() && cps > 0.0).then_some(cps)
    }

    fn probe_evaluated_replacement(
        &self,
        evaluated: &EvaluatedScore,
        restart_transport: bool,
        candidate_settings: Option<&RuntimeSettings>,
        enforce_realtime_throughput: bool,
    ) -> Result<(), RuntimeError> {
        #[cfg(any(test, feature = "test-support"))]
        self.panic_if_injected(SessionPanicPoint::ReplacementProbe);
        let pattern = match evaluated {
            EvaluatedScore::JavaScript { pattern, .. } | EvaluatedScore::MiniFallback(pattern) => {
                pattern
            }
        };
        // A fresh start normally leaves its first query to the scheduler: an
        // extra query could execute an impure callback twice. A host with a
        // tighter cap can safely preflight a pure graph before installing it,
        // which keeps an event explosion from reaching Studio's live loop.
        let replacing_active = !restart_transport && self.generation() != 0;
        if !replacing_active
            && (self.scheduler.query_hap_budget() == rustel_core::DEFAULT_HAP_BUDGET
                || !pattern.is_pure())
        {
            return Ok(());
        }
        let (settings, uses_js_host) = match evaluated {
            EvaluatedScore::JavaScript { settings, .. } => (Some(settings), true),
            EvaluatedScore::MiniFallback(_) => (candidate_settings, false),
        };
        let realtime_cps = (replacing_active && enforce_realtime_throughput)
            .then(|| Self::evaluated_realtime_cps(evaluated, self.scheduler.cps()))
            .flatten();
        let result = self.probe_live_replacement(
            pattern,
            settings,
            uses_js_host,
            realtime_cps,
            replacing_active,
        );
        if result.is_err() && self.generation() == 0 {
            self.js.clear_active();
        }
        result
    }

    pub(super) fn commit_evaluated_score(
        &mut self,
        evaluated: EvaluatedScore,
        source: &str,
        now: f64,
        restart_transport: bool,
        candidate_settings: Option<RuntimeSettings>,
        replacement_min_headroom: Duration,
    ) -> Result<(), RuntimeError> {
        // Each installed score can report missing inputs and loading samples
        // afresh. Both JavaScript and mini installs pass through this point.
        self.input_warnings_reported.clear();
        self.loading_refusals_reported.clear();
        // Live reloads preserve musical phase at the edit instant.
        // `replace_pattern` alone re-anchors at (now, queried_to), skipping the
        // unplayed horizon. Snapshot the current cycle under the old tempo and
        // restore it after install; tempo changes pivot the mapping at `now`.
        // The replacement queries the overlap through takeover, with unchanged
        // onsets pre-marked and changed or moved ones left to the device's
        // retained copies, so no onset sounds twice.
        // The scheduler, not `last_source`, is the authority for whether an
        // active graph exists. A host may install a direct Rust Pattern, which
        // deliberately has no replayable source but still needs continuous
        // phase and last-good query protection on the next live replacement.
        let replacing_active = !restart_transport && self.generation() != 0;
        // The cycle at `now` uses the old mapping, and the margin is in
        // seconds. A save ends its window on the old tempo (see
        // `replace_pattern_continued`). A quantised launch puts its cursor
        // on the new tempo.
        let margin = self.continuity_margin.unwrap_or(self.schedule_lead);
        // A quantised launch names its takeover time; it can only be later
        // than the margin, never nearer, or frames already rendered would be
        // asked to change.
        let takeover_override = self.takeover_override.take();
        let minimum_margin = if replacing_active {
            margin.max(replacement_min_headroom.as_secs_f64())
        } else {
            margin
        };
        let continuity = (replacing_active && !self.transport.is_stopped())
            .then(|| self.scheduler.cycle_at_time(now));
        let from_zero = std::mem::take(&mut self.next_from_zero);
        // The takeover instant. A quantised rewind takes over exactly on its
        // line: the consumer's pre-armed cut sounds there, and a margin
        // after the line would replay the beat the line already had. An
        // immediate rewind takes over at the edit instant: its cut rides
        // the consumer's flip, so a margin ahead would leave silence
        // between the cut and the first beat. Either can be behind the
        // clock when the producer publishes (always, for the immediate
        // one). The producer then slides cycle zero, the takeover and the
        // cut to its own clock (`slide_pending_rewind_anchor`), and the
        // consumer's restart floor sounds the residue whole. Rendered
        // frames never change: the cut retires what sounds under them. An
        // ordinary replacement, and a launch that does not rewind, keep
        // the margin floor.
        let takeover_time = match (continuity.is_some(), takeover_override) {
            // Only a REWIND may take its line in the rendered past: its cut
            // (pre-armed at the line, or the flip's) retires what sounds, so
            // a late publication is the restart, not a change to rendered
            // frames.
            (true, Some(line)) if line.is_finite() && from_zero => line,
            // A plain quantised launch rings out, like an edit: its takeover
            // keeps the margin floor, or an evaluation that outlasted the
            // head-room published into the past and was refused and rolled
            // back - the launch lost instead of landing late.
            (true, Some(line)) if line.is_finite() => line.max(now + minimum_margin),
            (true, None) if from_zero => now,
            _ => now + minimum_margin,
        };
        // A from-zero reload silences the outgoing rendition. The shape
        // rides the takeover: the consumer cannot tell a rewind's flip from
        // an edit's flip without it. An immediate restart (no named
        // takeover) cuts at the consumer's flip - the first block after
        // publication, ahead of the takeover - because the old rendition's
        // own next beat is already scheduled into the lead between the
        // flip and the takeover, and letting it sound is the restarted
        // loop's first beat twice. A quantised launch names its line; the
        // cut waits for it, so the old score plays its countdown and the
        // restarted loop takes over exactly on the line.
        let takeover_cut = match (from_zero, continuity.is_some(), takeover_override) {
            (true, true, None) => TakeoverCut::AtFlip,
            (true, true, Some(line)) if line.is_finite() => TakeoverCut::AtTakeover,
            // A named takeover that was not finite fell back to the ordinary
            // margin above - the immediate shape.
            (true, true, Some(_)) => TakeoverCut::AtFlip,
            _ => TakeoverCut::None,
        };
        let requery_anchor_time = match takeover_cut {
            TakeoverCut::None => None,
            TakeoverCut::AtFlip | TakeoverCut::AtTakeover => Some(match takeover_override {
                Some(line) if line.is_finite() => line,
                _ => now,
            }),
        };
        let requery_takeover_time = continuity.is_some().then_some(takeover_time);
        // A live device applies the takeover on a frame, not at a time. It
        // drops the outgoing onsets from the edge of that frame, which can
        // be more than a frame before the takeover time.
        let takeover_edge = self.takeover_frame_edge(takeover_time);
        // Where the outgoing score's key pins stop sounding, on the mapping
        // they were placed under: read before the install moves it (a tempo
        // change, a far cursor, a rewind's cycle zero).
        let key_takeover_cycle = self
            .scheduler
            .cycle_at_time(takeover_edge.unwrap_or(takeover_time));
        let continuity_margin = (takeover_time - now).max(0.0);
        // The edit-instant overlap: querying the replacement from `cycle_now`
        // instead of the takeover keeps a hap landing between the two audible
        // - the first hap of a hand-timed save otherwise vanishes, because
        // the outgoing generation is retired at the takeover and the
        // incoming one is queried past it. Only a continuous replacement in
        // the margin window can do this. A quantised launch waits for a
        // boundary beyond the margin, where pre-marking would have to cover
        // every boundary hap the old score still plays (a flam instead of
        // continuity), so it keeps the far cursor. `from_zero` re-anchors
        // the mapping itself and has nothing to continue.
        // From zero: cycle zero goes on the moment the new score takes
        // over, so the first thing heard is its first event rather than
        // whatever happens to lie where the count already stands. Only the
        // mapping moves, and only once the replacement is installed -
        // events the outgoing score has already had scheduled keep the
        // times they were given, so its tail rings out exactly as it
        // would have.
        //
        // Stopped, there is nothing to move: the transport starts at cycle
        // zero anyway, which is what `continuity` being `None` says.
        // `(cycle at the edit, seconds until the device flips generations)`,
        // both as of `now`. `None` keeps the far-cursor contract.
        let overlap_cycle = match (continuity, from_zero, takeover_override) {
            (Some(cycle_now), false, None) if continuity_margin.is_finite() => {
                Some((cycle_now, continuity_margin))
            }
            _ => None,
        };
        let accepted_sample_effects = match evaluated {
            EvaluatedScore::JavaScript {
                pattern,
                mut effects,
                settings,
            } => {
                // The bounded host publishes the active graph only after
                // construction succeeds. The scheduler generation follows
                // after the query probe accepts it at the intended install
                // clock. Native module state and host effects cross that same
                // infallible commit boundary as the graph and generation.
                let expected_generation = self.generation().checked_add(1).ok_or_else(|| {
                    RuntimeError::ResourceLimit("scheduler generation counter exhausted".into())
                })?;
                // Input handles are staged during score construction. Commit
                // their complete bounded set only after the replacement probe
                // accepts the graph, and immediately before the scheduler's
                // infallible generation transition. A thrown/timeout/refused
                // candidate simply drops `effects` and its provisional ports.
                self.js
                    .commit_midi_input_effects(&mut effects, expected_generation)
                    .map_err(RuntimeError::Js)?;
                self.js.adopt_runtime_settings(&settings);
                // Named rather than elided: a field added to `ScoreEffects`
                // and forgotten here compiles clean and simply never happens,
                // which is the quietest bug this seam can produce.
                #[cfg(feature = "hydra")]
                let ScoreEffects {
                    cps,
                    samples,
                    preload,
                    gamepad,
                    hydra: _taken_before_the_no_pattern_arms,
                    ..
                } = effects;
                #[cfg(not(feature = "hydra"))]
                let ScoreEffects {
                    cps,
                    samples,
                    preload,
                    gamepad,
                    ..
                } = effects;
                let cps = cps.filter(|cps| {
                    debug_assert!(
                        cps.is_finite() && *cps > 0.0,
                        "score cps effect must be finite and greater than zero"
                    );
                    cps.is_finite() && *cps > 0.0
                });
                let generation = match (overlap_cycle, cps) {
                    // A continuous replacement queries from the edit instant
                    // and carries the outgoing generation's overlap onsets
                    // across the takeover (see `replace_pattern_continued`).
                    (Some(overlap), cps) => {
                        self.replace_pattern_continued(pattern, now, cps, overlap, takeover_edge)
                    }
                    (None, cps) => self.scheduler.replace_pattern(pattern, now, cps),
                };
                debug_assert_eq!(generation, expected_generation);
                if let Some(cps) = cps {
                    self.config.cps = cps;
                }
                if restart_transport {
                    self.js.midi_input_bus().clear_keys();
                    self.transport.start();
                } else if !self.transport.is_stopped() {
                    // An evaluate over a running transport retires the key
                    // backlog the outgoing graph placed before the takeover.
                    // The ring remembers two seconds of note-ons pinned to
                    // cycle positions by its horizon queries. Carried across,
                    // the incoming graph would render them again for two
                    // seconds after the evaluate. Unlike a restart it is
                    // not a new key epoch: presses that landed after the
                    // outgoing graph's last query (during this evaluation and
                    // its probe) were never placed, never heard, and belong
                    // to the score taking over; the requery the press count
                    // arms claims them at its takeover. A pin at or after
                    // the takeover names audio the device swap drops, so it
                    // is forgotten and claimed again by the incoming graph.
                    self.js
                        .midi_input_bus()
                        .retire_key_backlog(key_takeover_cycle);
                }
                self.last_path = EvaluateSource::JavaScript;
                #[cfg(feature = "hydra")]
                self.promote_staged_hydra();
                if gamepad {
                    rustel_core::gamepad::request();
                }
                Some((samples, preload))
            }
            EvaluatedScore::MiniFallback(pattern) => {
                self.install_pattern_at(
                    pattern,
                    now,
                    restart_transport,
                    overlap_cycle,
                    takeover_edge,
                    key_takeover_cycle,
                )?;
                if let Some(settings) = candidate_settings.as_ref() {
                    self.js.adopt_runtime_settings(settings);
                    self.core_settings_initialized = true;
                }
                self.last_path = EvaluateSource::MiniRust;
                // A score that evaluated and named no pattern is a mute, not a
                // stop - and its visuals are still its visuals. This is the
                // arm the canonical `await initHydra(); osc().out()` file takes.
                #[cfg(feature = "hydra")]
                self.promote_staged_hydra();
                None
            }
        };
        // The replacement is installed: only now does it own the takeover
        // and its cut. If they were set before the fallible install above,
        // a failed commit would leave a rewind's AtFlip/AtTakeover for the
        // next slider or clock-steer requery to publish, and a control move
        // would cut the whole mix.
        #[cfg(any(test, feature = "test-support"))]
        self.panic_if_injected(SessionPanicPoint::Install);
        self.requery_takeover_cut = takeover_cut;
        self.requery_anchor_time = requery_anchor_time;
        self.requery_takeover_time = requery_takeover_time;
        if let Some(cycle_now) = continuity {
            if from_zero {
                // The takeover instant becomes cycle zero, and the query
                // cursor with it: the new generation is queried from the
                // score's first cycle.
                //
                // The anchor is the musical instant (a quantised launch's
                // line, or an immediate restart's edit instant), not a
                // margin ahead of the edit. The consumer cuts the outgoing
                // rendition at the flip (or the pre-armed line), so an
                // anchor a margin late would leave silence between the cut
                // and the first beat.
                // An anchor the clock has passed by the time the producer
                // reaches it (the immediate restart always, a launch whose
                // evaluation outlasted its line, any restart held for a
                // loading sample) is slid to the producer's clock before its
                // first window is queried (`slide_pending_rewind_anchor`),
                // so the downbeat is aimed where it can still sound; the few
                // milliseconds of query and publication after that are the
                // consumer's to absorb, and its restart floor admits those
                // first onsets whole at the flip rather than partway in.
                let anchor = match takeover_override {
                    Some(line) if line.is_finite() => line,
                    _ => now,
                };
                self.scheduler.rebase_start_anchor(anchor);
            } else if overlap_cycle.is_none() {
                // The quantised-launch arm: the re-query cursor sits one
                // continuity margin ahead of the edit instant. Where the
                // edit-instant overlap above handled the gap by carrying the
                // old onsets across, this arm deliberately leaves the gap to
                // the outgoing generation. A live device ends that gap on
                // the takeover frame, so the cursor starts on its edge.
                match takeover_edge {
                    Some(edge) => self.scheduler.rebase_anchor_from(now, cycle_now, edge),
                    None => {
                        let cursor_cycle =
                            cycle_now + continuity_margin.max(0.0) * self.scheduler.cps();
                        self.scheduler
                            .rebase_anchor_cursor(now, cycle_now, cursor_cycle);
                    }
                }
            }
            // `overlap_cycle.is_some()` already re-anchored the mapping at
            // `(now, cycle_now)` inside the install; re-anchoring again would
            // be a no-op at best and a second phase move at worst.
        }
        if let Some((samples, preload)) = accepted_sample_effects {
            self.apply_sample_effects(samples, preload);
        }
        if self.last_source.as_deref() != Some(source) {
            #[cfg(feature = "midi")]
            {
                self.midi_oversized_route_reported = false;
                self.midi_output_limit_reported = false;
            }
            #[cfg(feature = "osc")]
            {
                self.reported_osc_refusals.clear();
                self.osc_output_limit_reported = false;
            }
        }
        self.last_source = Some(Arc::from(source));
        self.recovered_source = None;
        self.js.snapshot_active_as_last_good();
        Ok(())
    }

    /// Fallback wall-clock ceiling for a replacement probe whose live tempo is
    /// unavailable. A live candidate instead receives the same bounded share
    /// of one cycle used to decide whether its first query is sustainable.
    pub(super) const PROBE_DEFAULT_CEILING: Duration = Duration::from_millis(150);

    pub(super) fn live_replacement_probe_budget(realtime_cps: Option<f64>) -> Duration {
        let Some(cps) = realtime_cps.filter(|cps| cps.is_finite() && *cps > 0.0) else {
            return Self::PROBE_DEFAULT_CEILING;
        };
        let sustainable_seconds = (LIVE_REPLACEMENT_QUERY_COMPUTE_SHARE / cps)
            .min(LIVE_QUERY_RECOVERY_BUDGET.as_secs_f64());
        Duration::from_secs_f64(sustainable_seconds)
    }

    /// Leave the consumer on the old generation until a measured-heavy
    /// candidate's first scheduling query has had time to finish. The query
    /// may consume at most 75% of the musical span represented by its probe
    /// budget, leaving the remaining quarter-cycle for conversion, ring
    /// transfer and ordinary scheduling before the takeover frame arrives.
    ///
    /// Cheap probes keep the established 80 ms response. Heavy probes derive
    /// runway from the *bounded* budget rather than an uncapped cycle: at a
    /// very low tempo the probe itself is capped at 250 ms, so an edit never
    /// acquires multi-second artificial latency.
    #[cfg(any(feature = "device-audio", test))]
    pub(super) fn live_replacement_takeover_headroom(
        realtime_cps: Option<f64>,
        probe_elapsed: Duration,
    ) -> Duration {
        // Cheap candidates retain the established immediate-edit latency. A
        // probe that already consumed the compute share of that floor leaves
        // too little time for an equivalent first query plus conversion, so
        // only that measured heavy path receives the complete bounded span.
        if probe_elapsed
            <= LIVE_REPLACEMENT_MIN_HEADROOM.mul_f64(LIVE_REPLACEMENT_QUERY_COMPUTE_SHARE)
        {
            return LIVE_REPLACEMENT_MIN_HEADROOM;
        }
        let query_budget = Self::live_replacement_probe_budget(realtime_cps);
        Duration::from_secs_f64(
            (query_budget.as_secs_f64() / LIVE_REPLACEMENT_QUERY_COMPUTE_SHARE)
                .min(DEFAULT_HORIZON),
        )
        .max(LIVE_REPLACEMENT_MIN_HEADROOM)
    }

    /// Query the first cycle of a candidate. A throw in any lane, contained
    /// by a stack or not, or a persistent resource/effect refusal rejects it
    /// and restores the last good active wrapper when one exists. An impure
    /// replacement that hits the temporary JavaScript CPU ceiling can be
    /// retried under the live producer's scheduling budget.
    fn probe_live_replacement(
        &self,
        pattern: &Pattern,
        candidate_settings: Option<&RuntimeSettings>,
        uses_js_host: bool,
        realtime_cps: Option<f64>,
        replacing_active: bool,
    ) -> Result<(), RuntimeError> {
        let probe_cps = realtime_cps.unwrap_or_else(|| self.scheduler.cps());
        let candidate = if replacing_active {
            "replacement"
        } else {
            "score"
        };
        let disposition = if replacing_active {
            "last-good score kept"
        } else {
            "score was not installed"
        };
        let state = State::new(TimeSpan::new(Fraction::ZERO, Fraction::ONE));
        // A fixed 150 ms probe rejected a candidate at 3.75 CPS even though
        // its measured one-cycle query fit the producer's 200 ms compute
        // share and the identical score ran when installed as the first
        // generation. Keep the probe bounded, but judge it by the same
        // tempo-aware budget as the throughput check below.
        let ceiling = std::time::Instant::now() + Self::live_replacement_probe_budget(realtime_cps);
        let query_started = Instant::now();
        // A RESOURCE refusal on one probed cycle means the score cannot be
        // scheduled live at all: accepting it made the producer spin on an
        // unschedulable pattern and allow the ring to drain with no recovery.
        // Rejecting keeps the last good song sounding, which is the whole
        // never-die contract; a transient budget squeeze is a different
        // thing and never reaches here because the probe has its own
        // ceiling.
        let refuse_heavy = |limit: rustel_core::QueryLimit| -> RuntimeError {
            RuntimeError::ResourceLimit(format!(
                "{candidate} is too heavy to schedule live; {disposition} ({limit})"
            ))
        };
        let outcome = if pattern.is_pure() {
            let query = || {
                rustel_core::with_query_deadline(ceiling, || {
                    let budget = self.scheduler.query_hap_budget();
                    Self::probe_outcome(pattern.query_arc_outcome_with_budget(&state, budget))
                })
            };
            let queried = match candidate_settings {
                Some(settings) => settings.with(query),
                None => self.js.with_runtime_settings(query),
            };
            match queried {
                Ok(outcome) => outcome,
                Err(limit) => {
                    self.js
                        .restore_last_good_active()
                        .map_err(|error| RuntimeError::Js(error.to_string()))?;
                    return Err(refuse_heavy(limit));
                }
            }
        } else {
            if !uses_js_host {
                self.js
                    .restore_last_good_active()
                    .map_err(|error| RuntimeError::Js(error.to_string()))?;
                return Err(RuntimeError::Js(
                    "native replacement unexpectedly requires the JavaScript callback host".into(),
                ));
            }
            // The probe reports a contained lane throw as its refusal, so the
            // callback host does not also log it.
            let query = || {
                self.js.with_contained_query_errors_unlogged(|| {
                    self.js.with_active_scope(|| {
                        rustel_core::with_query_deadline(ceiling, || {
                            let budget = self.scheduler.query_hap_budget();
                            Self::probe_outcome(
                                pattern.query_arc_outcome_with_budget(&state, budget),
                            )
                        })
                    })
                })
            };
            let queried = match candidate_settings {
                Some(settings) => settings.with(query),
                None => query(),
            };
            match queried {
                Ok(Ok(outcome)) => outcome,
                Ok(Err(limit)) => {
                    self.js
                        .restore_last_good_active()
                        .map_err(|error| RuntimeError::Js(error.to_string()))?;
                    return Err(refuse_heavy(limit));
                }
                // Preserve the existing impure-replacement contract only for
                // the temporary JavaScript CPU ceiling: the installed
                // generation is retried by the live producer under its real
                // scheduling budget. Other host limits are structural and
                // would refuse every retry, so keep the last-good graph.
                Err(rustel_jsruntime::QueryError::Limit(
                    rustel_core::QueryLimit::JsCpuDeadline { .. },
                )) => return Ok(()),
                Err(rustel_jsruntime::QueryError::Limit(limit)) => {
                    self.js
                        .restore_last_good_active()
                        .map_err(|error| RuntimeError::Js(error.to_string()))?;
                    return Err(refuse_heavy(limit));
                }
                Err(rustel_jsruntime::QueryError::Policy(message)) => {
                    self.js
                        .restore_last_good_active()
                        .map_err(|error| RuntimeError::Js(error.to_string()))?;
                    return Err(RuntimeError::Js(format!(
                        "{candidate} query refused a host effect; {disposition} ({message})"
                    )));
                }
                // Preserve the established query-throw path below. Ordinary
                // callback failures are carried by `QueryArcOutcome::Thrown`;
                // a host-scope message has historically left probing to that
                // outcome rather than inventing a second error contract.
                Err(rustel_jsruntime::QueryError::Message(_)) => return Ok(()),
            }
        };
        match outcome {
            QueryArcOutcome::Haps(haps) => {
                if let Some(message) = self.invalid_audio_control_in_probe(&haps, probe_cps) {
                    self.js
                        .restore_last_good_active()
                        .map_err(|error| RuntimeError::Js(error.to_string()))?;
                    return Err(RuntimeError::Message(format!(
                        "{candidate} contains an invalid audio control ({message}); {disposition}"
                    )));
                }
                // Throughput describes the one scheduling query above. A
                // separate pure-pattern look-ahead must not make a healthy
                // score appear too slow.
                let elapsed = query_started.elapsed();
                if let Some(candidate_cps) = realtime_cps
                    && candidate_cps.is_finite()
                    && candidate_cps > 0.0
                {
                    let available = LIVE_REPLACEMENT_QUERY_COMPUTE_SHARE / candidate_cps;
                    if elapsed.as_secs_f64() > available {
                        self.js
                            .restore_last_good_active()
                            .map_err(|error| RuntimeError::Js(error.to_string()))?;
                        return Err(RuntimeError::ResourceLimit(format!(
                            "{candidate} cannot keep up at {candidate_cps:.2} CPS: one cycle took {:.1} ms to query, but live scheduling can spend {:.1} ms; {disposition}",
                            elapsed.as_secs_f64() * 1000.0,
                            available * 1000.0,
                        )));
                    }
                }
                Ok(())
            }
            QueryArcOutcome::Thrown(message) => {
                self.js
                    .restore_last_good_active()
                    .map_err(|error| RuntimeError::Js(error.to_string()))?;
                Err(RuntimeError::Js(format!(
                    "{candidate} query failed; {disposition} ({message})"
                )))
            }
        }
    }

    /// The probe's verdict on its cycle. A lane whose throw a stack contained
    /// fails the probe as a query-wide throw does; only playback keeps the
    /// other lanes going.
    fn probe_outcome(
        queried: Result<QueryArcOutcome, rustel_core::QueryLimit>,
    ) -> Result<QueryArcOutcome, rustel_core::QueryLimit> {
        let contained = rustel_core::take_query_contained_throw();
        match (queried, contained) {
            (Ok(QueryArcOutcome::Haps(_)), Some(message)) => Ok(QueryArcOutcome::Thrown(message)),
            (queried, _) => queried,
        }
    }

    /// Validate controls on the same first cycle used by the live query
    /// probe. Asset availability is intentionally ignored here; only a value
    /// that the scalar voice can never interpret rejects the score.
    fn invalid_audio_control_in_probe(&self, haps: &[Hap], cps: f64) -> Option<String> {
        let bundled = rustel_voice::BundledOnly;
        let lookup: &dyn rustel_voice::SampleLookup =
            self.samples.as_deref().map_or(&bundled, |library| library);
        // The window that plays reports its own notices; the probe's are dropped.
        let (invalid, _notices) = rustel_voice::with_diagnostic_policy(false, || {
            haps.iter().filter(|hap| hap.has_onset()).find_map(|hap| {
                let materialized;
                let value = if matches!(hap.value, rustel_core::Value::JsValue(_)) {
                    materialized = rustel_core::materialize_js_value(&hap.value);
                    &materialized
                } else {
                    &hap.value
                };
                // Sessions also host ordinary pattern values for queries,
                // transformations and MIDI. Only control objects describe a
                // scalar audio voice; asking the voice resolver to interpret
                // numbers or strings would reject valid non-audio patterns.
                let object = value.as_object()?;
                let holds_unexpanded_mini = ["n", "s"].into_iter().any(|name| {
                    object
                        .get(name)
                        .and_then(rustel_core::Value::as_str)
                        .is_some_and(|text| {
                            text.chars().any(|ch| {
                                ch.is_whitespace()
                                    || matches!(
                                        ch,
                                        '~' | '*'
                                            | '['
                                            | ']'
                                            | '<'
                                            | '>'
                                            | '{'
                                            | '}'
                                            | '!'
                                            | '?'
                                            | ','
                                            | '|'
                                            | '@'
                                            | '('
                                            | ')'
                                    )
                            })
                        })
                });
                if holds_unexpanded_mini {
                    return None;
                }
                if object
                    .get("n")
                    .and_then(rustel_core::Value::as_f64)
                    .is_some_and(|note| !note.is_finite())
                {
                    return None;
                }
                // JSON.stringify follows JavaScript and turns NaN/Infinity
                // into null. Voice validation happens after that boundary,
                // where null means "control absent" and used to hide a bad
                // expression such as velocity(Math.pow(0.4, pattern)). Keep
                // the control name and reject it before serialization.
                if let Some(message) = non_finite_audio_control(&hap.value) {
                    return Some(message);
                }
                let duration = hap.duration().to_f64();
                let duration_secs = if cps.is_finite() && cps > 0.0 && duration.is_finite() {
                    duration.max(0.0) / cps
                } else {
                    0.25
                };
                let whole_begin = hap.whole_or_part().begin;
                let onset = OnsetEventJson {
                    live_controls: hap.live_controls,
                    onset_id: 0,
                    generation: self.generation().saturating_add(1),
                    whole_begin: whole_begin.show(),
                    duration_secs,
                    // This probe validates controls, not scheduling. A
                    // humanized onset may legitimately fall just before the
                    // queried window, so keep time out of the verdict.
                    target_time: 0.0,
                    value: ValueJson::from_value(&hap.value),
                    value_show: String::new(),
                    ui_visuals: hap.ui_visuals_context(),
                    log_line: None,
                };
                match crate::render::scalar_event_detailed(
                    &onset,
                    self.config.sample_rate,
                    cps,
                    lookup,
                ) {
                    Err(rustel_voice::VoiceError::InvalidControl(message)) => Some(message),
                    _ => None,
                }
            })
        });
        invalid
    }

    /// Test-only source selection for isolated replay-policy cases. Live
    /// callers install only the payload retained for an exact copy receipt.
    #[cfg(all(any(test, feature = "test-support"), feature = "device-audio"))]
    #[doc(hidden)]
    pub fn mark_audible_generation(&mut self, generation: u64) -> Result<(), RuntimeError> {
        let captured = self.capture_rollback_source(generation)?;
        self.install_rollback_source(captured);
        Ok(())
    }

    /// Capture the current replay payload without changing the rollback target.
    /// `None` records a direct Pattern with no textual source to replay.
    #[cfg(all(any(test, feature = "test-support"), feature = "device-audio"))]
    pub(super) fn capture_rollback_source(
        &self,
        generation: u64,
    ) -> Result<Option<AudibleSource>, RuntimeError> {
        if generation != self.generation() {
            return Err(RuntimeError::Message(format!(
                "cannot mark generation {generation} audible while Session generation {} is active",
                self.generation()
            )));
        }
        let Some(source) = self.last_source.clone() else {
            return Ok(None);
        };
        Ok(Some(AudibleSource {
            generation,
            source,
            mini: self.last_path == EvaluateSource::MiniRust,
            settings: self.js.snapshot_published_runtime_settings(),
            cps: self.scheduler.cps(),
            cycle_zero_time: self.scheduler.time_at_cycle(Fraction::ZERO),
        }))
    }

    /// Install a retained payload without consulting the current generation.
    /// `None` clears an older textual target after a direct Pattern cutover.
    #[cfg(feature = "device-audio")]
    pub(super) fn install_rollback_source(&mut self, captured: Option<AudibleSource>) {
        self.audible_source = captured;
    }

    /// Constant-clock convenience for isolated rollback-policy tests.
    #[cfg(all(feature = "device-audio", test))]
    pub(crate) fn rollback_to_previous_source(
        &mut self,
        now: f64,
    ) -> Result<RollbackAttempt, RuntimeError> {
        self.rollback_to_previous_source_with_clock(|| now)
    }

    /// Reinstall the producer-retained rollback source.
    ///
    /// The live watchdog's escape hatch: an installed score that produces no
    /// schedulable events starves the ring, and the set is silent with the
    /// process healthy. Rolling back restores audible cover; the artist sees
    /// the refusal in the log and can edit again.
    /// Replay samples the install clock again after evaluation and probing;
    /// that work must not consume the restored score's takeover headroom.
    #[cfg(feature = "device-audio")]
    pub(crate) fn rollback_to_previous_source_with_clock(
        &mut self,
        clock: impl FnMut() -> f64,
    ) -> Result<RollbackAttempt, RuntimeError> {
        // A callback may finish while the failed candidate is evaluating or
        // converting. Drain here as well as at turn entry: that completion
        // belongs to its retained source, not to the Session now failing.
        if !self.consume_audio_confirmations() {
            return Ok(RollbackAttempt::Deferred);
        }
        let Some(previous) = self.audible_source.clone() else {
            return Ok(RollbackAttempt::Unavailable);
        };
        if previous.generation == self.generation() {
            return Ok(RollbackAttempt::Unavailable);
        }
        // The rollback target already installed cleanly once, so it gets the
        // ordinary startup ceiling rather than a horizon-derived budget: the
        // horizon it would be derived from is exactly what starved.
        // Clone the Arc so the borrow checker sees an owner independent of
        // `self` for the &mut call below.
        let transport = Arc::clone(&self.transport);
        let cancellation = transport.stopped_flag();
        let attempt = self.evaluate_live_score_cancellable_seeded(
            &previous.source,
            previous.mini,
            Duration::from_millis(1),
            cancellation,
            clock,
            Some(&previous.settings),
        )?;
        // A deferred attempt did not run score code, so retain the target for
        // the next recovery attempt.
        match attempt {
            // Retain the replay target while the replacement awaits prefill.
            // Consumer confirmation advances it to the replacement generation;
            // this evaluation does not establish what the device is playing.
            LiveScoreAttempt::Applied(_) => Ok(RollbackAttempt::Applied),
            LiveScoreAttempt::Deferred => Ok(RollbackAttempt::Deferred),
        }
    }

    /// Attempt one watched score evaluation without spending audio work that
    /// is no longer buffered.
    ///
    /// JavaScript samples `clock` before execution to derive its deadline from
    /// the scheduler's *remaining* horizon, then samples it again only after a
    /// successful evaluation so the new generation is anchored at the actual
    /// install time. `Deferred` means no score code ran and the watch layer
    /// must retain the stable file identity. Explicit mini files do not enter
    /// QuickJS and are outside this CPU-bound claim, but still use a clock
    /// sampled after their parse rather than a stale pre-parse value.
    #[cfg(any(feature = "device-audio", test))]
    pub(crate) fn evaluate_live_score_cancellable<F>(
        &mut self,
        source: &str,
        mini: bool,
        continuation_reserve: Duration,
        cancellation: &AtomicBool,
        clock: F,
    ) -> Result<LiveScoreAttempt, RuntimeError>
    where
        F: FnMut() -> f64,
    {
        self.evaluate_live_score_cancellable_seeded(
            source,
            mini,
            continuation_reserve,
            cancellation,
            clock,
            None,
        )
    }

    #[cfg(any(feature = "device-audio", test))]
    fn evaluate_live_score_cancellable_seeded<F>(
        &mut self,
        source: &str,
        mini: bool,
        continuation_reserve: Duration,
        cancellation: &AtomicBool,
        mut clock: F,
        seed: Option<&RuntimeSettings>,
    ) -> Result<LiveScoreAttempt, RuntimeError>
    where
        F: FnMut() -> f64,
    {
        let mut first_clock = Some(clock());
        let now = first_clock.unwrap_or_default();
        self.with_panic_recovery(now, |session| {
            session.evaluate_live_score_cancellable_seeded_guarded(
                source,
                mini,
                continuation_reserve,
                cancellation,
                || first_clock.take().unwrap_or_else(&mut clock),
                seed,
            )
        })
    }

    #[allow(clippy::too_many_arguments)]
    #[cfg(any(feature = "device-audio", test))]
    fn evaluate_live_score_cancellable_seeded_guarded<F>(
        &mut self,
        source: &str,
        mini: bool,
        continuation_reserve: Duration,
        cancellation: &AtomicBool,
        mut clock: F,
        seed: Option<&RuntimeSettings>,
    ) -> Result<LiveScoreAttempt, RuntimeError>
    where
        F: FnMut() -> f64,
    {
        if mini {
            if cancellation.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(RuntimeError::Cancelled);
            }
            let settings = match seed {
                Some(seed) => Some(self.js.seeded_candidate_runtime_settings(seed)),
                None if !self.core_settings_owned => {
                    Some(self.js.snapshot_ambient_runtime_settings())
                }
                None => None,
            };
            let evaluation_started = Instant::now();
            let parsed = || rustel_mini::mini(source);
            let pattern = match settings.as_ref() {
                Some(settings) => settings.with(parsed),
                None => self.js.with_runtime_settings(parsed),
            }
            .map_err(|error| RuntimeError::Mini(error.to_string()));
            self.record_producer_phase(ProducerPhase::Evaluation, evaluation_started.elapsed());
            let pattern = pattern?;
            if cancellation.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(RuntimeError::Cancelled);
            }
            self.install_evaluated_score_with_clock(
                EvaluatedScore::MiniFallback(pattern),
                source,
                None,
                false,
                settings,
                clock,
            )?;
            return Ok(LiveScoreAttempt::Applied(self.generation()));
        }

        let budget_now = clock();
        let Some(budget) = self.live_score_budget_at(budget_now, continuation_reserve)? else {
            return Ok(LiveScoreAttempt::Deferred);
        };

        let evaluation_started = Instant::now();
        let evaluated = self.evaluate_score_cancellable_seeded(
            source,
            budget,
            cancellation,
            false,
            NoPatternPolicy::InstallSilence,
            seed,
        );
        self.record_producer_phase(ProducerPhase::Evaluation, evaluation_started.elapsed());
        let evaluated = evaluated?;
        self.install_evaluated_score_with_clock(
            evaluated,
            source,
            Some(budget_now),
            false,
            None,
            clock,
        )?;
        Ok(LiveScoreAttempt::Applied(self.generation()))
    }

    /// Replace the active source at a live scheduler clock position.
    ///
    /// Unlike [`Session::evaluate`], this does not call `Transport::start`:
    /// a file save racing with Stop must never clear the independent stop path.
    /// The existing active graph and schedule generation survive any read,
    /// parse, transpile or evaluation failure because the scheduler is changed
    /// only after the new graph has evaluated successfully.
    pub fn reload_at(&mut self, source: &str, mini: bool, now: f64) -> Result<u64, RuntimeError> {
        self.reload_at_cancellable(source, mini, now, &NEVER_CANCELLED)
    }

    /// Rebase the running transport so cycle zero is at `now`, without
    /// re-evaluating. Used when the device clock becomes available after score
    /// evaluation.
    pub fn restart_transport_at(&mut self, now: f64) {
        // A held note/key from the preceding transport lifetime must not turn
        // into a stuck gate when the same graph starts again. Continuous live
        // reloads deliberately keep current controller state; an explicit
        // restart is the reset boundary.
        self.js.midi_input_bus().clear_keys();
        self.scheduler.rebase_start_anchor(now);
        // Nothing has sounded on the new timeline, so recovery and rollback
        // must not return to the previous performance's score. The device
        // also holds no latency-compensated event of that score.
        #[cfg(feature = "device-audio")]
        {
            self.audible_source = None;
            self.compensated_onsets.clear();
            self.forget_handed_out();
        }
    }

    /// Finish an already-started transport after its preparation.
    /// Preserve both newly received MIDI keys and the explicit cycle-zero
    /// boundary; an outside-clock `retime` deliberately releases that boundary.
    pub fn finish_transport_start_at(&mut self, now: f64) {
        self.scheduler.rebase_anchor(now, 0.0);
        #[cfg(feature = "device-audio")]
        {
            self.compensated_onsets.clear();
            self.forget_handed_out();
        }
    }

    /// Reload the score, then restart its timeline at cycle zero at `now`.
    pub fn restart_at(&mut self, source: &str, mini: bool, now: f64) -> Result<u64, RuntimeError> {
        let generation = self.reload_at(source, mini, now)?;
        self.restart_transport_at(now);
        Ok(generation)
    }

    /// Reload on the existing timeline with no additional device schedule lead.
    pub fn reload_continuous_at(
        &mut self,
        source: &str,
        mini: bool,
        now: f64,
    ) -> Result<u64, RuntimeError> {
        self.reload_continuous_with_lead(source, mini, now, 0.0)
    }

    /// [`Self::reload_continuous_at`] whose re-query cursor starts
    /// `schedule_lead` seconds past `now`: frames nearer than the device's
    /// playback latency are already rendered, so a cursor at `now` makes the
    /// new generation's first events late by construction (telemetry:
    /// `late_events` on every live update).
    pub fn reload_continuous_with_lead(
        &mut self,
        source: &str,
        mini: bool,
        now: f64,
        schedule_lead: f64,
    ) -> Result<u64, RuntimeError> {
        // The common install path preserves continuity for every live reload.
        self.set_schedule_lead(schedule_lead);
        self.reload_at(source, mini, now)
    }

    /// Cancellable form used by the reusable file watcher.
    ///
    /// The ordinary public reload can replace a graph while Transport is
    /// stopped; a watcher instead passes the live
    /// stop flag so a stop arriving during synchronous score construction can
    /// interrupt QuickJS rather than waiting for the fixed deadline.
    pub fn reload_at_cancellable(
        &mut self,
        source: &str,
        mini: bool,
        now: f64,
        cancellation: &AtomicBool,
    ) -> Result<u64, RuntimeError> {
        if !now.is_finite() || now < 0.0 {
            return Err(RuntimeError::Message(format!(
                "reload clock must be a finite non-negative number, got {now}"
            )));
        }
        let result = if mini {
            self.evaluate_mini_at(source, now, false)
        } else {
            self.evaluate_at_cancellable(
                source,
                now,
                false,
                SCORE_CPU_BUDGET,
                cancellation,
                NoPatternPolicy::InstallSilence,
                false,
            )
        };
        if result.is_err() {
            self.forget_failed_reload_intent();
        }
        result?;
        Ok(self.generation())
    }

    /// A reload that failed before it could install consumed nothing. The
    /// from-zero mark a rewind set for it must not wait for the next
    /// ordinary save: that save would restart from zero and cut what is
    /// sounding. The quantised launch clears its own line on failure;
    /// the immediate rewind has only this.
    fn forget_failed_reload_intent(&mut self) {
        self.next_from_zero = false;
    }

    /// [`Self::reload_at_cancellable`] for a playing device: the score installs
    /// at the `clock` reading taken once it has evaluated.
    #[cfg(feature = "device-audio")]
    pub fn reload_with_clock_cancellable<F>(
        &mut self,
        source: &str,
        mini: bool,
        cancellation: &AtomicBool,
        mut clock: F,
    ) -> Result<u64, RuntimeError>
    where
        F: FnMut() -> f64,
    {
        let mut first_clock = Some(clock());
        let now = first_clock.unwrap_or_default();
        self.with_panic_recovery(now, |session| {
            session.reload_with_clock_cancellable_guarded(source, mini, cancellation, || {
                first_clock.take().unwrap_or_else(&mut clock)
            })
        })
    }

    #[cfg(feature = "device-audio")]
    fn reload_with_clock_cancellable_guarded<F>(
        &mut self,
        source: &str,
        mini: bool,
        cancellation: &AtomicBool,
        mut clock: F,
    ) -> Result<u64, RuntimeError>
    where
        F: FnMut() -> f64,
    {
        let result = (|| {
            if mini {
                let settings = (!self.core_settings_owned)
                    .then(|| self.js.snapshot_ambient_runtime_settings());
                let evaluation_started = Instant::now();
                let parsed = || rustel_mini::mini(source);
                let pattern = match settings.as_ref() {
                    Some(settings) => settings.with(parsed),
                    None => self.js.with_runtime_settings(parsed),
                }
                .map_err(|error| RuntimeError::Mini(error.to_string()));
                self.record_producer_phase(ProducerPhase::Evaluation, evaluation_started.elapsed());
                let pattern = pattern?;
                if cancellation.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err(RuntimeError::Cancelled);
                }
                self.install_evaluated_score_with_clock(
                    EvaluatedScore::MiniFallback(pattern),
                    source,
                    None,
                    false,
                    settings,
                    clock,
                )?;
            } else {
                let started_at = clock();
                Self::validate_live_score_clock("budget", started_at)?;
                let evaluation_started = Instant::now();
                let evaluated = self.evaluate_score_cancellable(
                    source,
                    SCORE_CPU_BUDGET,
                    cancellation,
                    false,
                    NoPatternPolicy::InstallSilence,
                );
                self.record_producer_phase(ProducerPhase::Evaluation, evaluation_started.elapsed());
                let evaluated = evaluated?;
                self.install_evaluated_score_with_clock(
                    evaluated,
                    source,
                    Some(started_at),
                    false,
                    None,
                    clock,
                )?;
            }
            Ok(self.generation())
        })();
        // Capacity only. A syntax error, a throw, a mini-notation
        // mistake: the last good score keeps playing, and that is not
        // the producer refusing work. Counting those as refusals paints
        // the studio's DSP line red for two seconds after a typo.
        if result
            .as_ref()
            .is_err_and(|error| matches!(error, RuntimeError::ResourceLimit(_)))
        {
            self.record_atomic_producer_refusal();
        }
        if result.is_err() {
            self.forget_failed_reload_intent();
        }
        result
    }
}
