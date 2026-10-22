//! Score evaluation, live replacement, and rollback.

use super::*;
use rustel_jsruntime::ImportScope;

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
        self.js.set_cps(self.scheduler.cps());
        self.load_script_imports(source, cancellation, ImportScope::Shared)
            .inspect_err(|_| self.js.show_last_good_imports())?;
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
        // The prebake ran with no score import on the surface, so its names
        // are for every score. The playing score gets its own names back.
        self.js.show_last_good_imports();
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
        let csound = self
            .csound
            .stage(&effects.csound, &self.config.score_import_access)?;
        self.csound.commit(csound);
        self.apply_sample_effects(effects.samples, effects.preload);
        self.retain_recovery_prebake(source, None);
        Ok(())
    }

    /// Run each script that `source` imports with `import('https://...')`
    /// and this realm has not run. A script runs as setup code, before the
    /// text that imports it, wherever the import stands in that text.
    ///
    /// An origin that the host does not trust refuses the evaluation before
    /// any fetch. The first trusted import of a script waits for its fetch.
    ///
    /// A score sees the names of the imports in its own text. Two scripts
    /// with one name in common do not change a score with one of them. A
    /// prebake import is for every score.
    fn load_script_imports(
        &mut self,
        source: &str,
        cancellation: Option<&AtomicBool>,
        scope: ImportScope,
    ) -> Result<(), RuntimeError> {
        use crate::score_import::ImportRefusal;

        // Approve every import first, so a refusal leaves the others unrun.
        let imports = rustel_transpiler::script_imports(source)
            .into_iter()
            .map(|specifier| {
                let url = self
                    .config
                    .score_import_access
                    .approve(&specifier)
                    .map_err(|refusal| match refusal {
                        ImportRefusal::Untrusted { origin } => {
                            RuntimeError::UntrustedImport { origin }
                        }
                        ImportRefusal::Invalid(message) => RuntimeError::Message(message),
                    })?;
                Ok((specifier, url))
            })
            .collect::<Result<Vec<_>, RuntimeError>>()?;
        let named: Vec<String> = match scope {
            ImportScope::Score => imports.iter().map(|(name, _)| name.clone()).collect(),
            // Setup code defines names for every score.
            ImportScope::Shared => Vec::new(),
        };
        for (position, (specifier, url)) in imports.into_iter().enumerate() {
            if self.js.import_loaded(&specifier) {
                if scope == ImportScope::Shared {
                    self.js.share_import(&specifier);
                }
                continue;
            }
            // A new script runs on the names of the imports before it.
            self.js
                .show_imports(named.get(..position).unwrap_or_default());
            if let Some(error) = self.import_errors.get(&specifier) {
                return Err(RuntimeError::Js(error.to_string()));
            }
            let failed = |error: String| RuntimeError::Message(format!("import {url}: {error}"));
            let text = match self.import_sources.get(&specifier) {
                Some(text) => text.clone(),
                None => {
                    let text: Arc<str> = crate::score_import::fetch(&url).map_err(failed)?.into();
                    self.import_sources.insert(specifier.clone(), text.clone());
                    text
                }
            };
            // A script can set `n` in a helper that this text cannot show.
            self.prebake_selects_variants = true;
            let options = TranspileOptions {
                emit_mini_locations: false,
                ..TranspileOptions::default()
            };
            let never = AtomicBool::new(false);
            let ran = self.js.run_import(
                &specifier,
                &text,
                &options,
                PREBAKE_CPU_BUDGET,
                cancellation.unwrap_or(&never),
                scope,
            );
            let effects = match ran.map_err(RuntimeError::from) {
                Ok(effects) => effects,
                Err(RuntimeError::Js(message)) => {
                    // A script can record its own first run. A second run
                    // would then skip the part that failed and report
                    // nothing, so this realm keeps the failure.
                    let message = format!("import {url}: {message}");
                    self.import_errors
                        .insert(specifier, Arc::from(message.as_str()));
                    return Err(RuntimeError::Js(message));
                }
                Err(other) => return Err(other),
            };
            for effects in effects {
                let csound = match self
                    .csound
                    .stage(&effects.csound, &self.config.score_import_access)
                {
                    Ok(csound) => csound,
                    Err(error) => {
                        self.import_errors
                            .insert(specifier.clone(), Arc::from(error.to_string()));
                        return Err(error);
                    }
                };
                self.csound.commit(csound);
                self.apply_sample_effects(effects.samples, effects.preload);
            }
        }
        self.js.show_imports(&named);
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
    pub(super) fn validate_live_score_clock(phase: &str, now: f64) -> Result<(), RuntimeError> {
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

    pub(super) fn evaluate_score_cancellable(
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
        seed: Option<(&RuntimeSettings, &crate::CsoundSnapshot)>,
    ) -> Result<EvaluatedScore, RuntimeError> {
        #[cfg(any(test, feature = "test-support"))]
        self.panic_if_injected(SessionPanicPoint::Evaluation);
        self.claim_core_settings();
        // scale/transpose/scaleTranspose are native registry combinators in
        // rustel-core. The voicings bundle still installs because chord and
        // voicing do not yet have complete native implementations.
        self.ensure_bindings()?;
        self.ensure_voicings()?;
        self.js.set_cps(self.scheduler.cps());
        let evaluated = self
            .load_script_imports(source, Some(cancellation), ImportScope::Score)
            .and_then(|()| {
                self.evaluate_loaded_score(
                    source,
                    budget,
                    cancellation,
                    allow_mini_fallback,
                    no_pattern,
                    seed,
                )
            });
        if evaluated.is_err() {
            // The playing score keeps the names of its own imports.
            self.js.show_last_good_imports();
        }
        evaluated
    }

    /// Evaluate a score after its imports ran.
    fn evaluate_loaded_score(
        &mut self,
        source: &str,
        budget: Duration,
        cancellation: &AtomicBool,
        allow_mini_fallback: bool,
        no_pattern: NoPatternPolicy,
        seed: Option<(&RuntimeSettings, &crate::CsoundSnapshot)>,
    ) -> Result<EvaluatedScore, RuntimeError> {
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
            // Each lane of the text gets its number before the evaluation,
            // so a plugin note names the same lane after an edit.
            #[cfg(feature = "vst")]
            lane_numbers: self.lanes.read(source),
            ..TranspileOptions::default()
        };
        let evaluated = match seed {
            Some((settings, _)) => self
                .js
                .evaluate_score_candidate_with_effects_cancellable_seeded(
                    source,
                    &options,
                    budget,
                    cancellation,
                    settings,
                ),
            None => self.js.evaluate_score_candidate_with_effects_cancellable(
                source,
                &options,
                budget,
                cancellation,
            ),
        };
        // The label rewrite found a different count of labeled statements
        // and wrote places. The notes of this text carry places, and not
        // the lane numbers of this read.
        #[cfg(feature = "vst")]
        if matches!(&evaluated, Ok((output, ..)) if output.lanes_by_place) {
            self.lanes.wrote_places();
        }
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
                // Each door that evaluates a text comes here. The plugins of
                // the calls the score ran load at the next look at the plugin
                // copies, after the copies of the lanes that play.
                #[cfg(feature = "vst")]
                self.keep_ran_plugins(&effects.plugins);
                let csound = match seed {
                    Some((_, snapshot)) => self.csound.stage_from_snapshot(
                        snapshot,
                        &effects.csound,
                        &self.config.score_import_access,
                    ),
                    None => self
                        .csound
                        .stage(&effects.csound, &self.config.score_import_access),
                };
                // The evaluation put this score in the active slot. A refused
                // orchestra puts the score in play there again, so each query
                // of the active slot reads the score in play. The visuals of
                // this score go too.
                let csound = match csound {
                    Ok(csound) => csound,
                    Err(error) => {
                        #[cfg(feature = "hydra")]
                        {
                            self.staged_hydra = None;
                        }
                        self.js
                            .restore_last_good_active()
                            .map_err(|error| RuntimeError::Js(error.to_string()))?;
                        return Err(error);
                    }
                };
                match self.js.active_pattern() {
                    Some(pattern) => Ok(EvaluatedScore::JavaScript {
                        pattern,
                        effects: Box::new(effects),
                        settings,
                        csound,
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

    /// With `hold`, a candidate with a plugin copy that is not ready waits
    /// at the commit seam while the wait is on: see `Session::plugin_seam`.
    /// It then does not go in play, and the session holds its text.
    #[allow(clippy::too_many_arguments)]
    #[cfg(any(feature = "device-audio", test))]
    fn install_evaluated_score_with_clock<F>(
        &mut self,
        evaluated: EvaluatedScore,
        source: &str,
        earliest_now: Option<f64>,
        restart_transport: bool,
        candidate_settings: Option<RuntimeSettings>,
        #[cfg(all(feature = "vst", feature = "device-audio"))] hold: bool,
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
        // The seam plans the plugin chains of the candidate. It runs before
        // the commit reads the clock, so its time takes nothing from the
        // takeover margin.
        #[cfg(all(feature = "vst", feature = "device-audio"))]
        let Some(plan_lines) = self.plugin_seam(
            &evaluated,
            source,
            hold,
            realtime_cps,
            probe_elapsed,
            &mut clock,
        )?
        else {
            return Ok(());
        };
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
        )?;
        // The commit forgot the plugin lines said before. The lines of the
        // plan go out now, so a note of the score with the same line gives
        // no second one.
        #[cfg(all(feature = "vst", feature = "device-audio"))]
        self.report_plugin_lines(plan_lines);
        Ok(())
    }

    pub(super) fn evaluated_realtime_cps(
        evaluated: &EvaluatedScore,
        fallback_cps: f64,
    ) -> Option<f64> {
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
        // Each installed score reports its missing inputs, its loading samples
        // and its plugin lines afresh. Both JavaScript and mini installs pass
        // through this point.
        self.input_warnings_reported.clear();
        self.refusals_reported.clear();
        self.plugin_lines_reported.clear();
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
                csound,
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
                self.csound.commit(csound);
                // Named rather than elided: a field added to `ScoreEffects`
                // and forgotten here compiles clean and simply never happens,
                // which is the quietest bug this seam can produce.
                #[cfg(feature = "hydra")]
                let ScoreEffects {
                    cps,
                    samples,
                    preload,
                    plugins: _kept_at_the_evaluation,
                    gamepad,
                    hydra: _taken_before_the_no_pattern_arms,
                    ..
                } = *effects;
                #[cfg(not(feature = "hydra"))]
                let ScoreEffects {
                    cps,
                    samples,
                    preload,
                    plugins: _kept_at_the_evaluation,
                    gamepad,
                    ..
                } = *effects;
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
                // A query of the new score reads the new tempo from `getCps()`.
                self.js.set_cps(self.scheduler.cps());
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
        self.csound.score_changed();
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
        // The lanes of this text are now the lanes of the score in play, and
        // the chains of this session get them: a chain of a lane that left
        // ends after its time, at once with no live output. A lane that left
        // keeps its number while a chain has it. The looks at the plugin
        // copies of the cycles ahead start again with the new score.
        #[cfg(feature = "vst")]
        {
            self.lanes.install(source);
            let live = self.plugins_live();
            crate::vst::score_changed(&self.chains, &self.lanes, Some(source), live);
            crate::vst::forget_lanes_away(&self.chains, &mut self.lanes);
            #[cfg(feature = "device-audio")]
            self.plugin_ahead.restart();
        }
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
        let lookup = crate::csound::ValidationLookup(lookup);
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
                let event = crate::render::scalar_event_detailed(
                    &onset,
                    self.config.sample_rate,
                    cps,
                    &lookup,
                );
                match event {
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
            csound: self.csound_snapshot(),
            cps: self.scheduler.cps(),
            cycle_zero_time: self.scheduler.time_at_cycle(Fraction::ZERO),
        }))
    }

    /// Install a retained payload without consulting the current generation.
    /// `None` clears an older textual target after a direct Pattern cutover.
    /// The lane table learns when the payload is the score in play.
    #[cfg(feature = "device-audio")]
    pub(super) fn install_rollback_source(&mut self, captured: Option<AudibleSource>) {
        #[cfg(feature = "vst")]
        if captured
            .as_ref()
            .is_some_and(|audible| audible.generation == self.generation())
        {
            // The output plays the score in play. The lane table keeps the
            // lanes of this score for a rollback from the next one.
            self.lanes.mark_audible();
        }
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
            Some((&previous.settings, &previous.csound)),
        )?;
        // A deferred attempt did not run score code, so retain the target for
        // the next recovery attempt.
        match attempt {
            // Retain the replay target while the replacement awaits prefill.
            // Consumer confirmation advances it to the replacement generation;
            // this evaluation does not establish what the device is playing.
            LiveScoreAttempt::Applied(_) => Ok(RollbackAttempt::Applied),
            LiveScoreAttempt::Deferred => Ok(RollbackAttempt::Deferred),
            // A rollback does not wait for its plugin copies: see the seed
            // in `evaluate_live_score_cancellable_seeded_guarded`.
            #[cfg(feature = "vst")]
            LiveScoreAttempt::Waiting => Ok(RollbackAttempt::Deferred),
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
    /// `Waiting` means the edit waits for its plugin copies, as in
    /// [`Self::reload_live`]: the watch layer asks again with the same text.
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
        #[cfg(all(feature = "vst", feature = "device-audio"))]
        if self.waits_for_plugins(source) {
            return Ok(LiveScoreAttempt::Waiting);
        }
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
        seed: Option<(&RuntimeSettings, &crate::CsoundSnapshot)>,
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
        seed: Option<(&RuntimeSettings, &crate::CsoundSnapshot)>,
    ) -> Result<LiveScoreAttempt, RuntimeError>
    where
        F: FnMut() -> f64,
    {
        if mini {
            if cancellation.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(RuntimeError::Cancelled);
            }
            let settings = match seed {
                Some((settings, _)) => Some(self.js.seeded_candidate_runtime_settings(settings)),
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
            let csound = match seed {
                Some((_, snapshot)) => self.csound.stage_from_snapshot(
                    snapshot,
                    &[],
                    &self.config.score_import_access,
                )?,
                None => None,
            };
            self.install_evaluated_score_with_clock(
                EvaluatedScore::MiniFallback(pattern),
                source,
                None,
                false,
                settings,
                #[cfg(all(feature = "vst", feature = "device-audio"))]
                false,
                clock,
            )?;
            self.csound.commit(csound);
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
        let csound = match (&evaluated, seed) {
            (EvaluatedScore::MiniFallback(_), Some((_, snapshot))) => self
                .csound
                .stage_from_snapshot(snapshot, &[], &self.config.score_import_access)?,
            _ => None,
        };
        self.install_evaluated_score_with_clock(
            evaluated,
            source,
            Some(budget_now),
            false,
            None,
            // A rollback, with its seed, does not wait for its plugin copies.
            #[cfg(all(feature = "vst", feature = "device-audio"))]
            seed.is_none(),
            clock,
        )?;
        #[cfg(all(feature = "vst", feature = "device-audio"))]
        if self.holds_edit(source) {
            return Ok(LiveScoreAttempt::Waiting);
        }
        self.csound.commit(csound);
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
    /// at the `clock` reading taken once it has evaluated. The score never
    /// waits for its plugin copies: see `Session::reload_live`.
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
            session.reload_with_clock_cancellable_guarded(
                source,
                mini,
                cancellation,
                #[cfg(feature = "vst")]
                false,
                || first_clock.take().unwrap_or_else(&mut clock),
            )
        })
    }

    /// With `hold`, the score waits for its plugin copies while the wait is
    /// on: see [`Self::reload_live`].
    #[cfg(feature = "device-audio")]
    pub(super) fn reload_with_clock_cancellable_guarded<F>(
        &mut self,
        source: &str,
        mini: bool,
        cancellation: &AtomicBool,
        #[cfg(feature = "vst")] hold: bool,
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
                    #[cfg(feature = "vst")]
                    false,
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
                    #[cfg(feature = "vst")]
                    hold,
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

/// Pull a mini string out of common CLI forms when JS evaluation fails.
fn extract_mini_fallback(source: &str) -> Option<String> {
    let trimmed = source.trim().trim_end_matches(';').trim();
    // s("bd sd") / s('bd sd') / mini("...")
    for name in ["s", "mini", "m"] {
        let prefix = format!("{name}(");
        if let Some(rest) = trimmed.strip_prefix(&prefix)
            && let Some(inner) = strip_quoted_arg(rest)
        {
            return Some(inner);
        }
    }
    // Bare quoted mini: "bd sd"
    if (trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2)
        || (trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2)
    {
        return Some(trimmed[1..trimmed.len() - 1].to_string());
    }
    // Bare unquoted token sequence: `bd sd` - never JavaScript.
    if !trimmed.contains('(') && !trimmed.contains('{') && !trimmed.contains('=') {
        return Some(trimmed.to_string());
    }
    None
}

fn strip_quoted_arg(rest: &str) -> Option<String> {
    let rest = rest.trim();
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let body = rest[1..].strip_suffix(')')?.trim_end();
    let body = body.strip_suffix(quote)?;
    Some(body.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::test_support::session_with_sample_origin;
    use crate::session::tests::*;
    #[cfg(test)]
    mod local_sample_check_tests {
        use super::*;
        use crate::samples::SampleLibrary;
        use crate::score_check::check_score;
        use std::sync::atomic::Ordering;

        fn local_session(granted: bool) -> (tempfile::TempDir, Session, Arc<SampleLibrary>) {
            let folder = tempfile::tempdir().expect("sample folder");
            // Checking the bank names only enumerates files; it does not decode audio.
            std::fs::write(folder.path().join("kick.wav"), b"").expect("sample name");
            let mut access = ScoreSampleAccess::denied();
            if granted {
                access
                    .permit_local_root(folder.path())
                    .expect("local grant");
            }
            let mut session =
                Session::with_config(SessionConfig::default().with_score_sample_access(access))
                    .expect("session");
            let library = Arc::new(SampleLibrary::empty());
            session.samples = Some(Arc::clone(&library));
            (folder, session, library)
        }

        #[test]
        fn check_reports_a_misspelled_sound_after_the_granted_local_import_loads() {
            let (_folder, mut session, library) = local_session(true);
            let check = check_score(
                "await samples('local:'); s(\"kik\")",
                Some(&library),
                &mut session,
                &AtomicBool::new(false),
            );
            assert!(library.knows_samples_source("local:"));
            assert!(library.knows_sound("kick"));
            assert!(check.eval_error.is_none(), "{check:?}");
            assert!(
                check
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.message == "unknown sound \"kik\""),
                "{check:?}"
            );
            assert!(!check.is_ok());
        }

        #[test]
        fn check_accepts_a_sound_in_the_granted_local_folder() {
            let (_folder, mut session, library) = local_session(true);
            let check = check_score(
                "await samples('local:'); s(\"kick\")",
                Some(&library),
                &mut session,
                &AtomicBool::new(false),
            );
            assert!(library.knows_samples_source("local:"));
            assert!(check.is_ok(), "{check:?}");
        }

        #[test]
        fn check_does_not_register_local_banks_without_a_host_grant() {
            let (_folder, mut session, library) = local_session(false);
            check_score(
                "await samples('local:'); s(\"kick\")",
                Some(&library),
                &mut session,
                &AtomicBool::new(false),
            );
            assert!(!library.knows_samples_source("local:"));
            assert!(!library.knows_sound("kick"));
            assert!(
                session
                    .take_diagnostics()
                    .iter()
                    .any(|diagnostic| diagnostic.kind == "samples-failed")
            );
        }

        #[test]
        fn check_without_a_lint_library_leaves_sound_names_unchecked() {
            let (_folder, mut session, _library) = local_session(true);
            let check = check_score(
                "await samples('local:'); s(\"kik\")",
                None,
                &mut session,
                &AtomicBool::new(false),
            );
            assert!(check.is_ok(), "{check:?}");
        }

        #[test]
        fn check_keeps_sound_names_unjudged_while_imports_are_pending() {
            let (_folder, mut session, library) = local_session(true);
            let _hold = library.hold_manifest_worker_for_test();
            let check = check_score(
                "await samples('local:'); samples('https://example.invalid/banks.json'); s(\"kik\")",
                Some(&library),
                &mut session,
                &AtomicBool::new(false),
            );
            assert!(library.manifests_pending() > 0);
            assert!(check.is_ok(), "{check:?}");
        }

        #[test]
        fn check_preserves_an_evaluation_failure_in_a_score_with_a_local_import() {
            let (_folder, mut session, library) = local_session(true);
            let check = check_score(
                "await samples('local:'); throw new Error('score failed');",
                Some(&library),
                &mut session,
                &AtomicBool::new(false),
            );
            assert!(
                check
                    .eval_error
                    .as_ref()
                    .is_some_and(|error| error.message.contains("score failed")),
                "{check:?}"
            );
            assert!(!check.is_ok());
        }

        #[test]
        fn a_cancelled_check_keeps_its_evaluation_error() {
            let (_folder, mut session, library) = local_session(true);
            let check = check_score(
                "await samples('local:'); s(\"kick\")",
                Some(&library),
                &mut session,
                &AtomicBool::new(true),
            );
            assert!(check.eval_error.is_some(), "{check:?}");
            assert!(!check.is_ok());
        }

        #[test]
        fn cancellation_during_the_local_import_wait_stops_the_check() {
            let (_folder, mut session, library) = local_session(true);
            let _hold = library.hold_manifest_worker_for_test();
            let held_jobs = library.manifests_pending();
            let cancellation = AtomicBool::new(false);
            let wait_entered = crate::samples::wait_tests::watch_next_wait();
            let (check, cancelled_at) = std::thread::scope(|scope| {
                let request = scope.spawn({
                    let cancellation = &cancellation;
                    move || {
                        wait_entered
                            .recv_timeout(Duration::from_secs(5))
                            .expect("the evaluated score entered its sample wait");
                        let cancelled_at = Instant::now();
                        cancellation.store(true, Ordering::Relaxed);
                        cancelled_at
                    }
                });
                let check = check_score(
                    "await samples('local:'); s(\"kick\")",
                    Some(&library),
                    &mut session,
                    &cancellation,
                );
                (check, request.join().expect("cancellation request"))
            });
            assert!(
                cancelled_at.elapsed() < Duration::from_secs(2),
                "cancellation waited for the five-second sample deadline"
            );
            assert!(library.manifests_pending() > held_jobs);
            assert_eq!(
                check
                    .eval_error
                    .as_ref()
                    .map(|error| error.message.as_str()),
                Some("cancelled"),
                "{check:?}"
            );
            assert!(!check.is_ok());
        }
    }

    #[test]
    fn rejected_default_join_does_not_reconfigure_the_last_good_callback() {
        let mut session = Session::new().expect("session");
        session
                .evaluate(
                    "setDefaultJoin('restart'); pure(10).fmap(x => fastcat(x,x).add(fastcat(1,2,3))).innerJoin()",
                )
                .expect("callback-bearing last-good score");
        let thirds = vec![
            (Fraction::ZERO, Fraction::new(1, 3)),
            (Fraction::new(1, 3), Fraction::new(2, 3)),
            (Fraction::new(2, 3), Fraction::ONE),
        ];
        assert_eq!(queried_parts(&session), thirds);

        session
            .reload_at(
                "setDefaultJoin('mix'); throw new Error('reject candidate')",
                false,
                0.25,
            )
            .expect_err("candidate must reject");
        assert_eq!(
            queried_parts(&session),
            thirds,
            "rejected JavaScript join state changed the last-good callback"
        );
    }

    #[test]
    fn rejected_default_voicings_do_not_reconfigure_the_last_good_callback() {
        let mut session = Session::new().expect("session");
        session
                .evaluate(
                    "setDefaultVoicings('guidetones'); pure('C7').fmap(x => rustelScope.voicing(pure(x))).innerJoin()",
                )
                .expect("callback-bearing last-good score");
        assert_eq!(
            session.query(Fraction::ZERO, Fraction::ONE).unwrap().len(),
            2
        );

        session
            .reload_at(
                "setDefaultVoicings('lefthand'); throw new Error('reject candidate')",
                false,
                0.25,
            )
            .expect_err("candidate must reject");
        assert_eq!(
            session.query(Fraction::ZERO, Fraction::ONE).unwrap().len(),
            2,
            "rejected JavaScript voicing state changed the last-good callback"
        );
    }

    #[test]
    fn rejected_voicing_candidate_preserves_scope_and_captured_public_routes() {
        let mut session = Session::new().expect("session");
        session
                .evaluate(
                    "setDefaultVoicings('guidetones'); (() => { const acceptedVoicing = voicing; return stack(pure('C7').fmap(x => rustelScope.voicing(pure(x))).innerJoin(), pure('C7').fmap(x => acceptedVoicing(pure(x))).innerJoin()); })()",
                )
                .expect("last-good score through both voicing routes");
        assert_eq!(
            session.query(Fraction::ZERO, Fraction::ONE).unwrap().len(),
            4
        );

        session
            .reload_at(
                "setDefaultVoicings('lefthand'); throw new Error('reject candidate')",
                false,
                0.25,
            )
            .expect_err("candidate must reject");
        assert_eq!(
            session.query(Fraction::ZERO, Fraction::ONE).unwrap().len(),
            4,
            "a rejected default changed a scoped or captured voicing route"
        );
    }

    #[test]
    fn session_default_join_setter_controls_an_existing_deferred_composer() {
        let mut session = Session::new().expect("session");
        session
                .evaluate(
                    "setDefaultJoin('restart'); pure(10).fmap(x => fastcat(x,x).add(fastcat(1,2,3))).innerJoin()",
                )
                .expect("deferred composer score");
        assert_eq!(queried_parts(&session).len(), 3);

        session.set_default_join(rustel_core::compose::Alignment::Mix);
        assert_eq!(
            queried_parts(&session).len(),
            4,
            "the deferred JavaScript composer ignored Session's native setting"
        );
    }

    /// Never-die-live: a JS typo that happens to look like mini (`bd sd`)
    /// must reject on reload, not replace the playing JS song with drums.
    #[test]
    fn live_reload_does_not_mini_fallback_a_js_typo() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(r#"s("bd sd").fast(2)"#)
            .expect("initial js score");
        let generation = session.generation();

        let error = session
            .reload_at("hh sd cp", false, 0.25)
            .expect_err("mini-looking leftover must not replace a JS song");
        assert_eq!(error.kind(), "evaluation");
        assert_eq!(session.generation(), generation);
        let haps = session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("last-good query");
        assert!(
            haps.len() >= 2,
            "mini fallback installed over the JS song: {haps:?}"
        );
    }

    mod hap_budget {
        use super::*;

        const EVENT_EXPLOSION: &str =
            r#"setcpm(138/4); $: s("hh").iter(64).stut(64, 1, 1).echo(64, .001, 1)"#;

        #[test]
        fn initial_live_reload_rejects_a_pure_event_explosion_before_install() {
            let mut session = Session::new().expect("session");
            session.set_query_hap_budget(8).expect("small hap budget");

            let error = session
                .reload_at(EVENT_EXPLOSION, false, 0.0)
                .expect_err("the event-heavy score exceeds the configured hap budget");
            assert!(matches!(&error, RuntimeError::ResourceLimit(_)), "{error}");
            assert_eq!(session.generation(), 0, "the score was not installed");

            session
                .reload_at(r#"s("hh*4")"#, false, 0.0)
                .expect("an ordinary score still installs after the refusal");
            assert_eq!(
                session.query(Fraction::ZERO, Fraction::ONE).unwrap().len(),
                4
            );
        }

        #[test]
        fn live_reload_hap_budget_refusal_keeps_the_last_good_score() {
            let mut session = Session::new().expect("session");
            session.set_query_hap_budget(8).expect("small hap budget");
            let original = r#"s("bd*4")"#;
            session
                .reload_at(original, false, 0.0)
                .expect("initial score");
            let generation = session.generation();
            let before = shown_note(&session);

            let error = session
                .reload_at(EVENT_EXPLOSION, false, 0.25)
                .expect_err("event-heavy replacement must not install");
            assert!(matches!(&error, RuntimeError::ResourceLimit(_)), "{error}");
            assert_eq!(session.generation(), generation);
            assert_eq!(session.active_source(), Some(original));
            assert_eq!(shown_note(&session), before, "the old graph still queries");
        }

        #[test]
        fn initial_live_reload_applies_the_hap_cap_before_install() {
            let mut session = Session::new().expect("session");
            session.set_query_hap_budget(8).expect("small hap budget");

            let error = session
                .reload_at(r#"s("hh*16")"#, false, 0.0)
                .expect_err("the ninth hap must refuse the candidate");
            assert!(matches!(&error, RuntimeError::ResourceLimit(_)), "{error}");
            assert!(error.to_string().contains("8 haps"), "{error}");
            assert_eq!(session.generation(), 0);

            // The lanes that do not play keep the cap.
            #[cfg(feature = "vst")]
            {
                let muted = "$: s(\"bd\")\n_$: s(\"hh*16\").vst(\"fx\")";
                session.reload_at(muted, false, 0.0).expect("one hap plays");
                let deadline = Instant::now() + Duration::from_secs(1);
                let shadow = session.shadow_haps(Fraction::ZERO, Fraction::ONE, deadline);
                assert!(shadow.is_none());
            }
        }
    }

    mod pointer {
        use super::*;

        /// The `n` of every onset a two-second play schedules.
        fn played_ns(session: &mut Session) -> Vec<serde_json::Value> {
            let report = session.play(2.0).expect("play");
            assert!(!report.onsets.is_empty(), "nothing played");
            report
                .onsets
                .iter()
                .map(|onset| match &onset.value {
                    ValueJson::Raw(value) => value["n"].clone(),
                    other => panic!("an onset without controls: {other:?}"),
                })
                .collect()
        }

        /// A Session built with a pointer plays the position the host set, read
        /// when the score is queried rather than when it is evaluated.
        #[test]
        fn a_session_with_a_pointer_plays_the_position_set() {
            let pointer = Pointer::default();
            let mut session =
                Session::with_config(SessionConfig::default().with_pointer(pointer.clone()))
                    .expect("session");
            session
                .evaluate("n(mousex.segment(2)).s('sine')")
                .expect("evaluate");
            for position in [0.25, 0.75] {
                pointer.x.set(position);
                let ns = played_ns(&mut session);
                assert!(ns.iter().all(|n| *n == position), "{position}: {ns:?}");
            }
        }
    }

    #[test]
    fn score_tempo_is_one_session_scheduler_and_report_commit() {
        let mut session = Session::new().expect("session");

        let before = session.generation();
        session
            .evaluate(
                "setcps(2); new Pattern(state => \
                 pure(state.controls._cps).query(state))",
            )
            .expect("score with in-file tempo and scheduler-control probe");
        assert_eq!(
            session.generation(),
            before + 1,
            "tempo and pattern committed as separate generations"
        );
        assert_eq!(session.config().cps, 2.0);
        assert_eq!(session.scheduler.cps(), 2.0);

        let control_probe = session.schedule_at(0.0).expect("schedule cps probe");
        assert_eq!(control_probe.len(), 1);
        assert_eq!(
            control_probe[0].value_show, "2",
            "scheduler query did not receive the committed `_cps`"
        );
        assert_eq!(control_probe[0].duration_secs, 0.5);

        session
            .evaluate("kick: s('bd*4')\ntempochanges: cps(1).gain(0)")
            .expect("a labeled cps lane");
        assert_eq!(session.scheduler.cps(), 1.0);
        assert_eq!(session.config().cps, 1.0);

        let before = session.generation();
        session
            .evaluate(r#"setcps(1); setcpm(120); note("c4 e4")"#)
            .expect("last tempo directive wins");
        assert_eq!(session.generation(), before + 1);
        assert_eq!(session.config().cps, 2.0, "last tempo call did not win");
        assert_eq!(session.scheduler.cps(), 2.0);

        let before = session.generation();
        session
            .evaluate(r#"note("d4 f4")"#)
            .expect("later score without a tempo directive");
        assert_eq!(session.generation(), before + 1);
        assert_eq!(
            session.config().cps,
            2.0,
            "a score without a tempo directive reset Session cps"
        );
        assert_eq!(session.scheduler.cps(), 2.0);

        let play = session.play(0.3).expect("play at committed score tempo");
        assert_eq!(play.cps, 2.0);
        assert!(
            play.onsets.len() >= 2,
            "two-step score produced too few onsets"
        );
        assert!((play.onsets[0].target_time - 0.0).abs() < 1e-9);
        assert!((play.onsets[1].target_time - 0.25).abs() < 1e-9);
        assert!((play.onsets[0].duration_secs - 0.25).abs() < 1e-9);
        assert!((play.onsets[1].duration_secs - 0.25).abs() < 1e-9);

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let output = std::env::temp_dir().join(format!(
            "rustel-in-file-tempo-{}-{unique}.json",
            std::process::id()
        ));
        let render = session
            .render(0.3, &output, RenderFormat::OnsetJson)
            .expect("render at committed score tempo");
        assert_eq!(render.cps, 2.0);
        assert!(render.onset_count >= 2);
        std::fs::remove_file(output).expect("remove owned tempo onset dump");

        // `getCps()` gives the tempo in play, then the tempo the score sets,
        // in the evaluation and in a query.
        session
            .evaluate(
                "const playing = getCps(); setcpm(30); const set = getCps(); \
                 new Pattern(state => pure(`${playing} ${set} ${getCps()}`).query(state))",
            )
            .expect("a score that reads the tempo");
        assert_eq!(active_values(&session), ["2 0.5 0.5"]);
    }

    #[test]
    fn pattern_cpm_matches_the_same_session_cpm_at_a_non_one_default() {
        let mut local = Session::new().expect("local-tempo session");
        local
            .evaluate(r#"note("c4*4").cpm(124.5/4)"#)
            .expect("pattern-local cpm");

        let mut global = Session::new().expect("global-tempo session");
        global
            .evaluate(r#"setcpm(124.5/4); note("c4*4")"#)
            .expect("session cpm");

        let local = local.play(2.0).expect("play local tempo");
        let global = global.play(2.0).expect("play global tempo");
        let local_times = local
            .onsets
            .iter()
            .map(|onset| onset.target_time)
            .collect::<Vec<_>>();
        let global_times = global
            .onsets
            .iter()
            .map(|onset| onset.target_time)
            .collect::<Vec<_>>();
        assert_eq!(local_times.len(), global_times.len());
        for (local, global) in local_times.iter().zip(global_times) {
            assert!((local - global).abs() < 1e-9, "{local} != {global}");
        }
    }

    #[test]
    fn invalid_audio_controls_never_replace_the_last_good_graph() {
        let replace = |session: &mut Session, source: &str| {
            session.evaluate_at_cancellable(
                source,
                0.0,
                false,
                SCORE_CPU_BUDGET,
                &NEVER_CANCELLED,
                NoPatternPolicy::InstallSilence,
                true,
            )
        };
        let mut session = Session::new().expect("session");
        session
            .evaluate(r#"note("c3").s("supersaw").lpenv(1)"#)
            .expect("valid first graph");
        session
            .evaluate(
                r#"s("supersaw").seg(8).note("c4")
                    .sometimesBy(1, x => x.transpose(rand.range(14,24)))"#,
            )
            .expect("a fractional transpose follows the synth's falsy-note fallback");
        let generation = session.generation();
        let before = active_values(&session);

        let error = replace(
            &mut session,
            r#"note("c3").s("supersaw").lpf(1000).lpenv("...")"#,
        )
        .expect_err("invalid lpenv must be rejected before commit");
        assert!(error.to_string().contains("lpenv must be a finite number"));
        assert_eq!(session.generation(), generation);
        assert_eq!(active_values(&session), before);

        let error = replace(
            &mut session,
            r#"s("sd:3")
                    .seg(8)
                    .struct("~ x ~ x")
                    .sometimesBy(1, x => x.plyWith(4, (i, n) =>
                        x.velocity(Math.pow(0.4, i))
                    ))"#,
        )
        .expect_err("a NaN velocity must be reported before JSON turns it into null");
        assert!(
            error
                .to_string()
                .contains("velocity must be a finite number"),
            "the refusal names the control that produced NaN: {error}"
        );
        assert_eq!(session.generation(), generation);
        assert_eq!(active_values(&session), before);
    }

    #[test]
    fn rejected_tempo_effects_are_atomic_and_never_laundered_through_mini() {
        let mut session = Session::new().expect("session");
        session
            .evaluate("setcps(1); note('c4')")
            .expect("initial score tempo");
        let generation = session.generation();
        let source = session.last_source.clone();
        let values = active_values(&session);

        let thrown = session
            .evaluate("setcps(3); throw new Error('score failed')")
            .expect_err("a later throw must reject its staged tempo");
        assert_eq!(thrown.kind(), "evaluation");
        assert_eq!(session.generation(), generation);
        assert_eq!(session.config().cps, 1.0);
        assert_eq!(session.scheduler.cps(), 1.0);
        assert_eq!(session.last_source, source);
        assert_eq!(active_values(&session), values);

        session
            .evaluate_prebake(
                "globalThis.__tempoOriginalS = s; \
                 globalThis.s = (...args) => { \
                   setcps(0); \
                   return __tempoOriginalS(...args); \
                 };",
            )
            .expect("install invalid-tempo score helper");
        let invalid = session
            .evaluate("s('sd')")
            .expect_err("invalid score tempo must not become mini fallback");
        assert_eq!(invalid.kind(), "evaluation");
        assert_eq!(session.generation(), generation);
        assert_eq!(session.config().cps, 1.0);
        assert_eq!(session.scheduler.cps(), 1.0);
        assert_eq!(active_values(&session), values);

        session
            .evaluate_prebake(
                "globalThis.s = (...args) => { \
                   setcps(4); \
                   throw new Error('ordinary score failure'); \
                 };",
            )
            .expect("install ordinary-failure score helper");
        session
            .evaluate("s('sd')")
            .expect("ordinary JavaScript failure may use the mini fallback");
        assert_eq!(session.last_evaluate_source(), EvaluateSource::MiniRust);
        assert_eq!(session.generation(), generation + 1);
        assert_eq!(
            session.config().cps,
            1.0,
            "a failed JavaScript candidate leaked tempo into mini fallback"
        );
        assert_eq!(session.scheduler.cps(), 1.0);
        assert_eq!(active_values(&session), ["sd"]);
    }

    #[test]
    fn a_prebake_helper_may_wrap_a_tempo_setter_for_scores_to_call() {
        // The community prebake shape: a `bpm` helper that wraps the tempo
        // setter is setup defining a function, not calling one. The call
        // runs when a score calls the helper, where a tempo change is
        // allowed.
        let mut session = Session::new().expect("session");
        session
            .evaluate_prebake("window.bpm = b => setcpm(b / 4)")
            .expect("a helper that only wraps a tempo setter is setup");
        session
            .evaluate("bpm(128); note('c4')")
            .expect("the score calls the helper, where tempo may change");
        assert_eq!(session.config().cps, 128.0 / 4.0 / 60.0);
        assert_eq!(session.scheduler.cps(), 128.0 / 4.0 / 60.0);

        // A wrapper the setup itself calls runs in setup, and the engine
        // still refuses it there, in its own words.
        let refused = session
            .evaluate_prebake("window.cpms = c => setcpm(c)\ncpms(60)")
            .expect_err("a wrapper called in setup runs in setup");
        assert!(
            refused.to_string().contains("setCps/setcps"),
            "the refusal names the tempo rule: {refused}"
        );
    }

    #[test]
    fn accepted_host_effects_apply_once_after_graph_and_tempo_commit() {
        let mut session = session_with_sample_origin("http://127.0.0.1:9");
        session.samples = Some(Arc::new(crate::samples::SampleLibrary::empty()));

        session
            .evaluate(
                "samples({ accepted: ['http://127.0.0.1:9/accepted.wav'] }); \
                 preload('accepted'); setCps(1.25); note('c4')",
            )
            .expect("score with host effects");

        assert_eq!(session.scheduler.cps(), 1.25);
        assert_eq!(session.config.cps, 1.25);
        assert_eq!(active_values(&session), ["note:c4"]);

        let library = session
            .samples
            .as_ref()
            .expect("test sample library")
            .clone();
        library.wait_until_idle(LOOPBACK_REFUSAL_DEADLINE);
        assert!(
            library.knows("accepted"),
            "sample registration did not follow graph publication"
        );
        assert_eq!(
            library
                .take_failures()
                .iter()
                .filter(|failure| failure.contains("accepted.wav"))
                .count(),
            1,
            "the preload did not use its same-transaction sample registration exactly once"
        );

        session
            .evaluate("note('d4')")
            .expect("later effect-free score");
        library.wait_until_idle(Duration::from_millis(100));
        assert!(library.take_failures().is_empty());
    }

    #[test]
    fn accepted_setup_effects_apply_once_without_changing_the_score() {
        let mut session = session_with_sample_origin("http://127.0.0.1:9");
        session.samples = Some(Arc::new(crate::samples::SampleLibrary::empty()));
        session
            .evaluate("setCps(1.25); note('c4')")
            .expect("initial score");
        let generation = session.generation();
        let cps = session.scheduler.cps();
        let active = active_values(&session);

        session
            .evaluate_prebake(
                "samples({ setupAccepted: ['http://127.0.0.1:9/setup.wav'] }); \
                 preload('setupAccepted'); globalThis.setupEffectsApplied = 1;",
            )
            .expect("setup with host effects");

        let library = session
            .samples
            .as_ref()
            .expect("test sample library")
            .clone();
        library.wait_until_idle(LOOPBACK_REFUSAL_DEADLINE);
        assert!(library.knows("setupAccepted"));
        assert_eq!(
            library
                .take_failures()
                .iter()
                .filter(|failure| failure.contains("setup.wav"))
                .count(),
            1
        );
        assert_eq!(session.generation(), generation);
        assert_eq!(session.scheduler.cps(), cps);
        assert_eq!(active_values(&session), active);
        assert_eq!(session.js.get_number("setupEffectsApplied"), Some(1.0));

        session
            .evaluate_prebake("globalThis.effectFreeSetup = 1;")
            .expect("later effect-free setup");
        library.wait_until_idle(Duration::from_millis(100));
        assert!(library.take_failures().is_empty());
        assert_eq!(session.generation(), generation);
        assert_eq!(session.scheduler.cps(), cps);
        assert_eq!(active_values(&session), active);
    }

    /// A pattern the setup builds carries the setup's byte offsets. Those
    /// offsets must not reach the score's document, which is a different
    /// text.
    #[test]
    fn prebake_patterns_do_not_impersonate_score_source_locations() {
        let mut session = Session::new().expect("session");
        session
            .evaluate_prebake(
                r#"
                    globalThis.fromSetup = () => stack(
                        note("c4 e4"),
                        note("g4 a4"),
                        pure('setup'),
                    );
                "#,
            )
            .expect("setup helper");
        session.evaluate("$: fromSetup()").expect("score");

        let haps = session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query setup-created pattern");
        assert!(!haps.is_empty());
        assert!(
            haps.iter().all(|hap| hap.context.is_empty()),
            "prebake byte offsets leaked into the score document: {haps:?}"
        );

        // A string that `mini` parses at run time is not in the score either.
        session.evaluate("$: note(mini('c4 e4'))").expect("score");
        let haps = session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query run-time mini");
        assert_eq!(haps.len(), 2);
        assert!(
            haps.iter().all(|hap| hap.context.is_empty()),
            "offsets in the string leaked into the score document: {haps:?}"
        );
    }

    /// A score imports a script only from an origin the host trusts, and the
    /// refusal comes before any fetch.
    #[test]
    fn a_script_import_runs_as_setup_only_from_a_trusted_origin() {
        let url = "https://scripts.example/helpers.js";
        let score = format!("await import('{url}')\n$: fromImport()");
        let mut session = Session::new().expect("session");
        // The text a fetch of `url` gives.
        session.import_sources.insert(
            url.to_owned(),
            Arc::from("globalThis.fromImport = () => stack(note(60), note(64));"),
        );
        let refused = session.evaluate(&score).expect_err("an untrusted origin");
        assert_eq!(refused.kind(), "untrusted-import");
        assert!(
            refused.to_string().starts_with("https://scripts.example "),
            "{refused}"
        );

        let mut access = ScoreImportAccess::denied();
        access.permit_origin("https://scripts.example").unwrap();
        session.set_score_import_access(access);
        // One untrusted import refuses the text before a trusted one runs.
        let mixed = format!("await import('{url}')\nawait import('https://other.example/x.js')");
        let refused = session
            .evaluate(&mixed)
            .expect_err("one origin is untrusted");
        assert_eq!(refused.kind(), "untrusted-import");
        assert!(!session.js.import_loaded(url));
        session.evaluate(&score).expect("the trusted import");
        let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
        assert_eq!(haps.len(), 2);
        // The realm has run the script. A second score loads nothing new.
        session.import_sources.clear();
        session.evaluate(&score).expect("the loaded import");

        // A script that throws keeps its error. The second run of a script
        // with a first-run guard would load a part of it and report nothing.
        let broken = "https://scripts.example/broken.js";
        session.import_sources.insert(
            broken.to_owned(),
            Arc::from(
                "globalThis.ranOnce ??= 0; if (ranOnce++ === 0) throw new Error('first run');",
            ),
        );
        let score = format!("await import('{broken}')\n$: note(60)");
        for _ in 0..2 {
            let error = session.evaluate(&score).expect_err("the script throws");
            assert!(error.to_string().contains("first run"), "{error}");
        }
    }

    /// A score uses a native orchestra only while the host permits one. The
    /// refusal is typed, so the studio can offer its switch, and a permit
    /// the host takes back refuses the next score again. A refused score
    /// leaves the score in play in the active slot.
    #[test]
    fn an_orchestra_loads_only_while_it_is_permitted() {
        let load = "loadCSound`instr voice
endin`";
        let mut session = Session::new().expect("session");
        let refused = session.evaluate_prebake(load).expect_err("no permission");
        assert_eq!(refused.kind(), "csound-permission");
        assert!(
            refused.to_string().starts_with("Csound is not permitted"),
            "{refused}"
        );

        let mut access = ScoreImportAccess::denied();
        access.permit_csound();
        session.set_score_import_access(access);
        // The gate passed. Without the native library the next refusal is
        // the library's own, never the permission's.
        if let Err(error) = session.evaluate_prebake(load) {
            assert!(
                std::env::var_os("RUSTEL_CSOUND_LIBRARY").is_none()
                    && error.to_string().contains("Csound 6 is not available"),
                "{error}"
            );
        }

        session.set_score_import_access(ScoreImportAccess::denied());
        let refused = session
            .evaluate_prebake(load)
            .expect_err("permission taken back");
        assert_eq!(refused.kind(), "csound-permission");

        // The score runs, then the host refuses its orchestra.
        session.evaluate("note(60)").expect("score in play");
        let playing = active_values(&session);
        let refused = session
            .evaluate(&format!("{load}\nnote(62)"))
            .expect_err("no permission");
        assert_eq!(refused.kind(), "csound-permission");
        assert_eq!(active_values(&session), playing);
    }

    /// Two scripts give `sc` two meanings. A score gets the meaning of the
    /// import in its own text, whatever score ran before.
    #[test]
    fn each_score_keeps_the_names_of_its_own_imports() {
        fn play(session: &mut Session, source: &str) -> Result<Vec<String>, RuntimeError> {
            session.evaluate(source).map(|()| active_values(session))
        }
        let url = |name: &str| format!("https://scripts.example/{name}.js");
        let with = |name: &str| format!("await import('{}')\n$: note(0).sc().zz()", url(name));
        let mut session = Session::new().expect("session");
        let mut access = ScoreImportAccess::denied();
        access.permit_origin("https://scripts.example").unwrap();
        session.set_score_import_access(access);
        for (name, octave) in [("low", 24), ("high", 72)] {
            session.import_sources.insert(
                url(name),
                Arc::from(format!(
                    "Pattern.prototype.sc = function () {{ return this.add(note({octave})) }};
                     register('zz', (pat) => pat.add(note({octave})));
                     globalThis.both = {octave}; globalThis.{name}Only = 1;"
                )),
            );
        }
        // `sc` is the engine's. `zz` is a prebake's, and the score before
        // the first import takes `zz` with `register`.
        session
            .evaluate_prebake("Pattern.prototype.zz = function () { return this.add(note(9)) }")
            .expect("prebake");
        let base = play(&mut session, "$: note(0).sc().zz()").expect("no import");
        play(
            &mut session,
            "register('zz', (pat) => pat)\n$: note(0).zz()",
        )
        .expect("register");
        let low = play(&mut session, &with("low")).expect("low");
        let high = play(&mut session, &with("high")).expect("high");
        assert!(low != high && low != base);
        assert_eq!(play(&mut session, &with("low")).expect("low again"), low);

        // A score with no import gets the engine's `sc` and the prebake's
        // `zz` back. A name two scripts made has no owner. A name one script
        // made stays.
        assert_eq!(
            play(&mut session, "$: note(0).sc().zz()").expect("base"),
            base
        );
        play(&mut session, "$: note(both)").expect_err("two meanings, no import");
        play(&mut session, "$: note(lowOnly + highOnly)").expect("one meaning each");

        // A prebake import is for every score. A score import still wins.
        session
            .evaluate_prebake(&format!("await import('{}')", url("high")))
            .expect("prebake");
        let shared = play(&mut session, "$: note(0).sc().zz()").expect("shared");
        assert_eq!(shared, high);
        assert_eq!(play(&mut session, &with("low")).expect("low"), low);
    }

    #[test]
    fn cancellation_after_setup_staging_discards_the_whole_transaction() {
        use std::sync::atomic::Ordering;

        let mut session = Session::new().expect("session");
        session.samples = Some(Arc::new(crate::samples::SampleLibrary::empty()));
        session.evaluate("note('c4')").expect("initial score");
        let generation = session.generation();
        let active = active_values(&session);
        let cancellation = Arc::new(AtomicBool::new(false));
        let request = {
            let cancellation = cancellation.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(40));
                cancellation.store(true, Ordering::Relaxed);
            })
        };

        let error = session
            .evaluate_prebake_cancellable(
                "samples({ cancelledSetup: ['http://127.0.0.1:9/cancelled.wav'] }); \
                 preload('cancelledSetup'); globalThis.cancelledSetupPrefix = 1; \
                 while (true) {}",
                cancellation.as_ref(),
            )
            .expect_err("caller cancellation must stop setup after staging");
        request.join().expect("cancellation requester");
        assert!(matches!(error, RuntimeError::Cancelled));
        assert_eq!(session.js.get_number("cancelledSetupPrefix"), Some(1.0));
        assert_eq!(session.generation(), generation);
        assert_eq!(active_values(&session), active);
        let library = session.samples.as_ref().expect("test sample library");
        assert!(!library.knows("cancelledSetup"));
        assert_eq!(session.preload_requested, 0);

        session
            .evaluate_prebake("globalThis.afterCancelledSetup = 1;")
            .expect("setup recovery after cancellation");
        let library = session.samples.as_ref().expect("test sample library");
        assert!(!library.knows("cancelledSetup"));
        assert_eq!(session.preload_requested, 0);
    }

    #[test]
    fn rejected_effects_do_not_leak_into_a_later_score_or_mini_fallback() {
        let mut session = Session::new().expect("session");
        session.samples = Some(Arc::new(crate::samples::SampleLibrary::empty()));
        session.evaluate("note('c4')").expect("last-good score");
        session
            .evaluate_prebake(
                "globalThis.effectSamples = globalThis.samples; \
                 globalThis.effectPreload = globalThis.preload; \
                 globalThis.s = (...args) => { \
                   effectSamples({ fallbackLeak: ['http://127.0.0.1:9/fallback.wav'] }); \
                   effectPreload('fallbackLeak'); \
                   throw new Error('ordinary score failure'); \
                 };",
            )
            .expect("install failing compatibility helper");

        session
            .evaluate("s('sd')")
            .expect("ordinary JavaScript failure may use Mini fallback");
        assert_eq!(session.last_evaluate_source(), EvaluateSource::MiniRust);
        assert_eq!(active_values(&session), ["sd"]);
        let library = session.samples.as_ref().expect("test sample library");
        assert!(!library.knows("fallbackLeak"));
        assert_eq!(session.preload_requested, 0);

        let rejected = session
            .evaluate(
                "effectSamples({ rejectedLeak: ['http://127.0.0.1:9/rejected.wav'] }); \
                 effectPreload('rejectedLeak'); throw new Error('reject effects')",
            )
            .expect_err("failed score with effects");
        assert!(rejected.to_string().contains("reject effects"));
        session
            .evaluate("note('e4')")
            .expect("effect-free recovery score");
        let library = session.samples.as_ref().expect("test sample library");
        assert!(!library.knows("rejectedLeak"));
        assert_eq!(session.preload_requested, 0);
    }

    #[test]
    fn live_probe_rejects_query_time_effects_and_restores_last_good() {
        let mut session = Session::new().expect("session");
        session.samples = Some(Arc::new(crate::samples::SampleLibrary::empty()));
        session.evaluate("note('c4')").expect("last-good score");
        let generation = session.generation();
        session
            .evaluate_prebake(
                "globalThis.effectSamples = globalThis.samples; \
                 globalThis.effectPreload = globalThis.preload;",
            )
            .expect("save effect bindings");

        let error = session
                .reload_at(
                    "pure('candidate').fmap(value => { \
                   try { effectSamples({ probeLeak: ['http://127.0.0.1:9/probe.wav'] }); } catch (_) {} \
                   try { effectPreload('probeLeak'); } catch (_) {} \
                   return value; \
                 })",
                    false,
                    0.25,
                )
                .expect_err("query-time effect must refuse replacement");
        assert_eq!(error.kind(), "evaluation");
        assert!(error.to_string().contains("host effect"));
        assert_eq!(session.generation(), generation);
        assert_eq!(active_values(&session), ["note:c4"]);
        let library = session.samples.as_ref().expect("test sample library");
        assert!(!library.knows("probeLeak"));
        assert_eq!(session.preload_requested, 0);
    }

    #[test]
    fn live_probe_rejects_policy_plus_pending_jobs_and_discards_effects() {
        let mut session = session_with_sample_origin("http://127.0.0.1:9");
        session.samples = Some(Arc::new(crate::samples::SampleLibrary::empty()));
        session
            .evaluate(
                "(() => { \
                   let calls = 0; \
                   return pure('last-good').fmap(value => `${value}:${++calls}`); \
                 })()",
            )
            .expect("callback-bearing last-good score");
        assert_eq!(active_values(&session), ["last-good:1"]);
        let generation = session.generation();
        session
            .evaluate_prebake(
                "globalThis.probeEffectSamples = samples; \
                 globalThis.probeEffectPreload = preload;",
            )
            .expect("retain effect bindings");

        let error = session
            .reload_at(
                "probeEffectSamples({ stagedLeak: ['http://127.0.0.1:9/staged.wav'] }); \
                 probeEffectPreload('stagedLeak'); \
                 pure('candidate').fmap(value => { \
                   try { \
                     probeEffectSamples({ queryLeak: ['http://127.0.0.1:9/query.wav'] }); \
                   } catch (_) {} \
                   Promise.resolve().then(() => { globalThis.probeJobRan = 1; }); \
                   return value; \
                 })",
                false,
                0.25,
            )
            .expect_err("pending query work must not hide a host-effect refusal");
        assert_eq!(error.kind(), "resource-limit");
        assert!(error.to_string().contains("runnable JavaScript jobs"));
        assert_eq!(session.generation(), generation);
        assert_eq!(active_values(&session), ["last-good:2"]);
        let library = session.samples.as_ref().expect("test sample library");
        assert!(!library.knows("stagedLeak"));
        assert!(!library.knows("queryLeak"));
        assert_eq!(session.preload_requested, 0);
        assert_eq!(session.js.get_number("probeJobRan"), None);
        assert!(!session.js.jobs_pending(), "rejected probe kept its job");

        session
            .reload_at("note('e4')", false, 0.5)
            .expect("recovery after pending-job probe refusal");
        assert_eq!(active_values(&session), ["note:e4"]);
    }

    #[test]
    fn live_probe_rejects_policy_even_when_callback_hits_the_js_deadline() {
        let mut session = session_with_sample_origin("http://127.0.0.1:9");
        session.samples = Some(Arc::new(crate::samples::SampleLibrary::empty()));
        session
            .evaluate(
                "(() => { \
                   let calls = 0; \
                   return pure('last-good').fmap(value => `${value}:${++calls}`); \
                 })()",
            )
            .expect("callback-bearing last-good score");
        assert_eq!(active_values(&session), ["last-good:1"]);
        let generation = session.generation();
        session
            .evaluate_prebake("globalThis.deadlineEffectSamples = samples;")
            .expect("retain sample effect binding");

        let error = session
            .reload_at(
                "pure('candidate').fmap(value => { \
                   try { \
                     deadlineEffectSamples({ deadlineLeak: ['http://127.0.0.1:9/deadline.wav'] }); \
                   } catch (_) {} \
                   while (true) {} \
                   return value; \
                 })",
                false,
                0.25,
            )
            .expect_err("a JavaScript deadline must not hide a host-effect refusal");
        assert_eq!(error.kind(), "evaluation");
        assert!(error.to_string().contains("host effect"));
        assert_eq!(session.generation(), generation);
        assert_eq!(active_values(&session), ["last-good:2"]);
        let library = session.samples.as_ref().expect("test sample library");
        assert!(!library.knows("deadlineLeak"));
        assert!(!session.js.jobs_pending());

        session
            .reload_at("note('f4')", false, 0.5)
            .expect("recovery after combined policy/deadline refusal");
        assert_eq!(active_values(&session), ["note:f4"]);
    }

    #[test]
    fn live_replacement_probe_budget_tracks_the_candidate_tempo() {
        assert_eq!(
            Session::live_replacement_probe_budget(Some(3.75)),
            Duration::from_millis(200),
            "900/4 must receive its sustainable one-cycle compute share"
        );
        assert_eq!(
            Session::live_replacement_probe_budget(Some(50.0)),
            Duration::from_millis(15),
            "very high CPS must not retain the fixed probe ceiling"
        );
        assert_eq!(
            Session::live_replacement_probe_budget(Some(0.5)),
            LIVE_QUERY_RECOVERY_BUDGET,
            "low-tempo candidates remain capped by the bounded recovery slice"
        );
        assert_eq!(
            Session::live_replacement_probe_budget(None),
            Session::PROBE_DEFAULT_CEILING
        );
        assert_eq!(
            Session::live_replacement_probe_budget(Some(f64::NAN)),
            Session::PROBE_DEFAULT_CEILING
        );

        assert_eq!(
            Session::live_replacement_takeover_headroom(Some(3.75), Duration::from_millis(60)),
            LIVE_REPLACEMENT_MIN_HEADROOM,
            "a cheap 900-BPM candidate must retain immediate edit latency"
        );
        let at_900_bpm =
            Session::live_replacement_takeover_headroom(Some(3.75), Duration::from_millis(61));
        assert!(
            (at_900_bpm.as_secs_f64() - 1.0 / 3.75).abs() < 1e-9,
            "a sustainable 900-BPM first query must finish before takeover: {at_900_bpm:?}"
        );
        assert_eq!(
            Session::live_replacement_takeover_headroom(Some(50.0), Duration::from_millis(61)),
            LIVE_REPLACEMENT_MIN_HEADROOM,
            "very short cycles retain the ordinary immediate-edit floor"
        );
        let capped_low_tempo =
            Session::live_replacement_takeover_headroom(Some(0.5), Duration::from_millis(61));
        assert!(
            (capped_low_tempo.as_secs_f64() - 1.0 / 3.0).abs() < 1e-9,
            "the bounded 250 ms probe must not create a multi-second takeover: {capped_low_tempo:?}"
        );
    }

    #[test]
    fn live_score_budget_uses_remaining_horizon_floor_and_ceiling() {
        let reserve = Duration::from_millis(10);
        let mut session = Session::new().expect("session");
        session.evaluate("note('c4')").expect("score");
        session.schedule_at(0.0).expect("fill default horizon");

        let partly_drained = session
            .live_score_budget_at(0.2, reserve)
            .expect("valid clock")
            .expect("partly drained horizon still affords score JavaScript");
        assert!(
            partly_drained.abs_diff(Duration::from_millis(270)) <= Duration::from_micros(1),
            "budget came from the nominal 500ms horizon instead of the 300ms remaining: {partly_drained:?}"
        );
        assert!(
            session
                .live_score_budget_at(0.4719, Duration::from_millis(1))
                .expect("valid just-above-floor clock")
                .is_some(),
            "a score budget above the minimum viable slice was deferred"
        );
        assert_eq!(
            session
                .live_score_budget_at(0.4721, Duration::from_millis(1))
                .expect("valid just-below-floor clock"),
            None,
            "sub-minimum score JavaScript ran instead of retaining its identity"
        );
        assert!(
            session.live_score_budget_at(0.0, Duration::ZERO).is_err(),
            "zero continuation reserve made the deadline equal the whole horizon"
        );

        let mut large = Session::with_config(SessionConfig {
            horizon: 10.0,
            ..SessionConfig::default()
        })
        .expect("large-horizon session");
        large.evaluate("note('c4')").expect("large score");
        large.schedule_at(0.0).expect("fill large horizon");
        assert_eq!(
            large
                .live_score_budget_at(0.0, reserve)
                .expect("large valid clock"),
            Some(SCORE_CPU_BUDGET),
            "a large configured horizon bypassed the score CPU ceiling"
        );
        assert_eq!(SCORE_CPU_BUDGET, Duration::from_secs(2));
    }

    #[test]
    fn live_score_defers_without_running_then_deadlines_transactionally_and_recovers() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(
                "stack( \
                   s('bd').polyBind(value => pure(value).fast(2)), \
                   pure(['old-owned-value']).fast(2) \
                 )",
            )
            .expect("initial callback/JS-owned score");
        session.schedule_at(0.0).expect("fill horizon");
        let generation = session.generation();
        let last_source = session.last_source.clone();
        let last_path = session.last_evaluate_source();
        let before = active_values(&session);
        let cancellation = AtomicBool::new(false);

        let mut deferred_clock_calls = 0;
        let deferred = session
            .evaluate_live_score_cancellable(
                "globalThis.deferredScoreRan = 1; note('d4')",
                false,
                Duration::from_millis(1),
                &cancellation,
                || {
                    deferred_clock_calls += 1;
                    0.49
                },
            )
            .expect("a tiny affordable slice is deferral, not rejection");
        assert_eq!(deferred, LiveScoreAttempt::Deferred);
        assert_eq!(deferred_clock_calls, 1, "deferral sampled an install clock");
        assert_eq!(session.js.get_number("deferredScoreRan"), None);
        assert_eq!(session.config().cps, DEFAULT_CPS);
        assert_eq!(session.scheduler.cps(), DEFAULT_CPS);

        // Make the syntactically recognisable `s('sd')` compatibility form
        // run away in JavaScript. If a typed deadline is mistakenly fed into
        // the mini fallback, this call succeeds as the mini pattern `sd` and
        // the test loses its expected resource-limit failure.
        session
            .evaluate_prebake(
                "globalThis.originalScoreS = s; \
                 globalThis.blockScoreS = true; \
                 globalThis.s = (...args) => { \
                   if (blockScoreS) { \
                     setcps(4); \
                     globalThis.sessionScorePrefix = 17; \
                     while (true) {} \
                   } \
                   return originalScoreS(...args); \
                 };",
            )
            .expect("install hostile score helper");
        let mut deadline_clock_calls = 0;
        let started = Instant::now();
        let error = session
            .evaluate_live_score_cancellable(
                "setcps(2); s('sd')",
                false,
                Duration::from_millis(10),
                &cancellation,
                || {
                    deadline_clock_calls += 1;
                    0.4
                },
            )
            .expect_err("runaway watched score must hit its remaining-horizon deadline");
        assert_eq!(
            error.kind(),
            "resource-limit",
            "typed deadline was lost: {error}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "watched score used the offline two-second ceiling instead of its remaining horizon: {:?}",
            started.elapsed()
        );
        assert_eq!(
            deadline_clock_calls, 1,
            "a failed score sampled an install clock despite making no commit"
        );
        assert_eq!(session.js.get_number("sessionScorePrefix"), Some(17.0));
        assert_eq!(session.generation(), generation);
        assert_eq!(
            session.config().cps,
            DEFAULT_CPS,
            "a timed-out watched score committed its staged tempo"
        );
        assert_eq!(session.scheduler.cps(), DEFAULT_CPS);
        assert_eq!(session.last_source, last_source);
        assert_eq!(session.last_evaluate_source(), last_path);
        session.js.run_gc();
        session.js.run_gc();
        assert_eq!(active_values(&session), before);
        assert_eq!(session.js.query_depth(), 0, "deadline leaked query scope");

        session
            .evaluate_prebake("globalThis.blockScoreS = false;")
            .expect("enable corrected score");
        let mut clocks = [0.4, 0.44].into_iter();
        let applied = session
            .evaluate_live_score_cancellable(
                "setcps(2); s('sd')",
                false,
                Duration::from_millis(10),
                &cancellation,
                || clocks.next().expect("exactly two live score clock samples"),
            )
            .expect("same-heap correction after deadline");
        assert_eq!(applied, LiveScoreAttempt::Applied(generation + 1));
        assert!(
            clocks.next().is_none(),
            "score sampled more than two clocks"
        );
        assert_eq!(session.last_evaluate_source(), EvaluateSource::JavaScript);
        assert_eq!(active_values(&session), ["s:sd"]);
        assert_eq!(session.config().cps, 2.0);
        assert_eq!(session.scheduler.cps(), 2.0);
        // The re-query cursor sits at the edit instant (the overlap up to the
        // takeover is the replacement's own to query, old onsets pre-marked),
        // so the schedule cover reaches exactly to the install clock 0.44.
        // Anchoring at the pre-evaluation clock 0.4 would leave the cover at
        // 0.4 and report zero remaining.
        assert!(
            (session.scheduler.horizon_remaining(0.4) - 0.04).abs() < 1e-9,
            "new generation was anchored at the pre-evaluation clock rather than the fresh install clock"
        );

        // A broken clock provider after successful JS cannot be reported as a
        // score failure: the active graph is already the fully-owned candidate,
        // and rebuilding the old graph from a bare Pattern would discard its
        // wrapper Sidecar roots. Commit at the validated pre-eval sample.
        let corrected = session
            .schedule_through(0.44, 0.94)
            .expect("refill corrected score");
        assert_eq!(corrected.len(), 1);
        // A live reload keeps the cycle↔time mapping fixed, and a tempo change
        // pivots at the install clock. Old mapping (anchor (0,0), cps 0.5) puts the pivot
        // 0.44 s at cycle 0.22; the next sd onset is cycle 1, landing at
        // 0.44 + (1 − 0.22)/2 = 0.83 s.
        assert!((corrected[0].target_time - 0.83).abs() < 1e-9);
        assert!((corrected[0].duration_secs - 0.5).abs() < 1e-9);
        let mut faulty_clocks = [0.44, f64::NAN].into_iter();
        let applied = session
            .evaluate_live_score_cancellable(
                "note('e4')",
                false,
                Duration::from_millis(10),
                &cancellation,
                || faulty_clocks.next().expect("two faulty-clock samples"),
            )
            .expect("successful score must commit despite a faulty post clock");
        assert_eq!(applied, LiveScoreAttempt::Applied(generation + 2));
        assert_eq!(active_values(&session), ["note:e4"]);
        assert_eq!(session.config().cps, 2.0);
        assert_eq!(session.scheduler.cps(), 2.0);
        // The re-query cursor sits at the edit instant, so the cover reaches
        // exactly to the install clock 0.44.
        assert!(session.scheduler.horizon_remaining(0.44).abs() < 1e-9);
    }

    #[test]
    fn score_cancellation_stays_typed_keeps_the_old_graph_and_recovers() {
        use std::sync::atomic::Ordering;

        let mut session = Session::new().expect("session");
        session.samples = Some(Arc::new(crate::samples::SampleLibrary::empty()));
        session.evaluate("note('c4')").expect("initial score");
        let generation = session.generation();
        let before = active_values(&session);
        session
            .evaluate_prebake(
                "globalThis.originalCancelledS = s; \
                 globalThis.effectSamples = samples; \
                 globalThis.effectPreload = preload; \
                 globalThis.blockCancelledS = true; \
                 globalThis.s = (...args) => { \
                   if (blockCancelledS) { \
                     effectSamples({ cancelledLeak: ['http://127.0.0.1:9/cancelled.wav'] }); \
                     effectPreload('cancelledLeak'); \
                     setcps(4); \
                     globalThis.sessionCancelPrefix = 23; \
                     while (true) {} \
                   } \
                   return originalCancelledS(...args); \
                 };",
            )
            .expect("install cancellable hostile helper");

        let cancellation = Arc::new(AtomicBool::new(false));
        let request = {
            let cancellation = cancellation.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(40));
                cancellation.store(true, Ordering::Relaxed);
            })
        };
        let started = Instant::now();
        let error = session
            .evaluate_cancellable("s('sd')", cancellation.as_ref())
            .expect_err("caller cancellation must interrupt score construction");
        request.join().expect("cancellation requester");
        assert!(matches!(error, RuntimeError::Cancelled));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "cancellation waited for the two-second deadline: {:?}",
            started.elapsed()
        );
        assert_eq!(session.js.get_number("sessionCancelPrefix"), Some(23.0));
        assert_eq!(session.generation(), generation);
        assert_eq!(
            session.config().cps,
            DEFAULT_CPS,
            "a cancelled score committed its staged tempo"
        );
        assert_eq!(session.scheduler.cps(), DEFAULT_CPS);
        assert_eq!(active_values(&session), before);
        let library = session.samples.as_ref().expect("test sample library");
        assert!(!library.knows("cancelledLeak"));
        assert_eq!(session.preload_requested, 0);
        assert_eq!(
            session.js.query_depth(),
            0,
            "cancellation leaked query scope"
        );

        cancellation.store(false, Ordering::Relaxed);
        session
            .evaluate_prebake("globalThis.blockCancelledS = false;")
            .expect("enable correction");
        session
            .evaluate_cancellable("s('hh')", cancellation.as_ref())
            .expect("same-heap recovery after cancellation");
        assert_eq!(active_values(&session), ["s:hh"]);
        let library = session.samples.as_ref().expect("test sample library");
        assert!(!library.knows("cancelledLeak"));
        assert_eq!(session.preload_requested, 0);
    }

    #[test]
    fn pending_score_job_policy_cannot_be_laundered_through_mini_fallback() {
        let mut session = Session::new().expect("session");
        session.evaluate("note('c4')").expect("initial score");
        let generation = session.generation();
        let before = active_values(&session);
        let last_source = session.last_source.clone();
        session
            .js
            .eval(
                "Promise.resolve().then(() => { \
                   globalThis.staleScoreJobRan = 1; \
                 });",
            )
            .expect("seed a preexisting runnable job");

        // This shape is deliberately eligible for the compatibility Mini
        // fallback. A string-typed host-policy refusal would therefore install
        // `sd` and hide the fact that this score turn never ran.
        let error = session
            .evaluate("s('sd')")
            .expect_err("a stale score job must be a typed refusal");
        assert_eq!(error.kind(), "resource-limit");
        assert!(error.to_string().contains("runnable JavaScript jobs"));
        assert_eq!(session.generation(), generation);
        assert_eq!(session.last_source, last_source);
        assert_eq!(active_values(&session), before);
        assert_eq!(session.js.get_number("staleScoreJobRan"), None);
        assert!(!session.js.jobs_pending(), "stale job survived refusal");

        session
            .evaluate("s('hh')")
            .expect("same-heap recovery after stale-job refusal");
        assert_eq!(active_values(&session), ["s:hh"]);
    }

    /// A cancelled JavaScript evaluation is not retried through the mini
    /// fallback. `s("bd")` carries an extractable mini pattern, so the
    /// fallback could otherwise return success after a stop.
    #[test]
    fn a_cancelled_evaluation_is_not_laundered_through_the_mini_fallback() {
        let mut session = Session::new().expect("session");
        let cancellation = AtomicBool::new(true);
        let error = session
            .evaluate_cancellable("s(\"bd\")", &cancellation)
            .expect_err("a cancelled evaluation must not succeed via the mini fallback");
        assert!(
            matches!(error, RuntimeError::Cancelled),
            "expected Cancelled, got {error}"
        );
    }

    #[test]
    fn live_mini_is_outside_the_js_budget_but_uses_a_fresh_install_clock() {
        let mut session = Session::new().expect("session");
        session
            .evaluate("setcps(1); note('c4')")
            .expect("initial score");
        let generation = session.generation();
        let cancellation = AtomicBool::new(false);
        let mut clock_calls = 0;
        let applied = session
            .evaluate_live_score_cancellable(
                "e4",
                true,
                Duration::from_millis(1),
                &cancellation,
                || {
                    clock_calls += 1;
                    0.37
                },
            )
            .expect("live mini score");
        assert_eq!(applied, LiveScoreAttempt::Applied(generation + 1));
        assert_eq!(
            clock_calls, 1,
            "mini sampled a JavaScript budget clock instead of only its post-parse install clock"
        );
        assert_eq!(session.last_evaluate_source(), EvaluateSource::MiniRust);
        assert_eq!(
            session.config().cps,
            1.0,
            "an explicit live Mini replacement reset the active tempo"
        );
        assert_eq!(session.scheduler.cps(), 1.0);
        assert_eq!(active_values(&session), ["e4"]);
    }

    #[test]
    #[cfg(feature = "device-audio")]
    fn live_prebake_budget_uses_remaining_horizon_and_keeps_the_two_second_ceiling() {
        let reserve = std::time::Duration::from_millis(10);
        let mut session = Session::new().expect("session");
        session.evaluate("note('c4')").expect("score");
        session.schedule_at(0.0).expect("fill default horizon");

        let partly_drained = session
            .live_prebake_budget_at(0.2, reserve)
            .expect("valid clock")
            .expect("partly drained horizon still affords setup");
        assert!(
            partly_drained.abs_diff(std::time::Duration::from_millis(270))
                <= std::time::Duration::from_micros(1),
            "budget came from the nominal 500ms horizon instead of the 300ms remaining: {partly_drained:?}"
        );
        assert_eq!(
            session
                .live_prebake_budget_at(0.471, reserve)
                .expect("valid exhausted clock"),
            None,
            "evaluation was offered after the continuation reserve consumed the remaining horizon"
        );
        assert!(
            session
                .live_prebake_budget_at(0.4719, std::time::Duration::from_millis(1))
                .expect("valid just-above-floor clock")
                .is_some(),
            "a budget above the minimum viable setup slice was deferred"
        );
        assert_eq!(
            session
                .live_prebake_budget_at(0.4721, std::time::Duration::from_millis(1))
                .expect("valid just-below-floor clock"),
            None,
            "a sub-minimum budget ran setup instead of retaining the identity"
        );

        let mut large = Session::with_config(SessionConfig {
            horizon: 10.0,
            ..SessionConfig::default()
        })
        .expect("large-horizon session");
        large.evaluate("note('c4')").expect("large score");
        large.schedule_at(0.0).expect("fill large horizon");
        assert_eq!(
            large
                .live_prebake_budget_at(0.0, reserve)
                .expect("large valid clock"),
            Some(PREBAKE_CPU_BUDGET),
            "a large configured horizon bypassed the setup CPU ceiling"
        );
        assert!(
            session
                .live_prebake_budget_at(0.0, std::time::Duration::ZERO)
                .is_err(),
            "zero continuation reserve made the deadline equal the whole horizon"
        );
    }

    #[test]
    #[cfg(feature = "device-audio")]
    fn live_prebake_interrupt_uses_the_derived_budget_not_the_offline_constant() {
        let mut session = Session::new().expect("session");
        session.evaluate("note('c4')").expect("score");
        session.schedule_at(0.0).expect("fill horizon");
        let reserve = std::time::Duration::from_millis(1);
        let transport = session.transport();
        let deferred = session
            .evaluate_live_prebake_cancellable(
                "globalThis.mustNotRunUnderTinyBudget = 1;",
                0.49,
                reserve,
                transport.stopped_flag(),
            )
            .expect("a tiny affordable slice is deferral, not rejection");
        assert_eq!(deferred, LivePrebakeAttempt::Deferred);
        assert_eq!(session.js.get_number("mustNotRunUnderTinyBudget"), None);

        let now = 0.47;
        let derived = session
            .live_prebake_budget_at(now, reserve)
            .expect("valid clock")
            .expect("small positive budget");
        let generation = session.generation();
        let error = session
            .evaluate_live_prebake_cancellable(
                "globalThis.liveDeadlinePrefix = 1; while (true) {}",
                now,
                reserve,
                transport.stopped_flag(),
            )
            .expect_err("hostile watched setup must hit its affordable deadline");
        assert!(
            error
                .to_string()
                .contains(&format!("{} ms CPU deadline", derived.as_millis())),
            "watched setup used a fixed/offline deadline instead of {derived:?}: {error}"
        );
        assert_eq!(session.generation(), generation);
        assert_eq!(session.js.get_number("liveDeadlinePrefix"), Some(1.0));
    }

    #[test]
    fn prebake_keeps_the_active_graph_generation_transport_and_score_identity() {
        let mut session = Session::new().expect("session");
        session.evaluate_mini("c4").expect("initial mini score");
        let generation = session.generation();
        let last_source = session.last_source.clone();
        let last_path = session.last_evaluate_source();
        let before = session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("initial active query")
            .into_iter()
            .map(|hap| hap.show())
            .collect::<Vec<_>>();
        session.transport().stop();

        session
            .evaluate_prebake(
                "globalThis.loadCount = (globalThis.loadCount || 0) + 1; \
                 globalThis.fromPrebake = () => note(loadCount === 1 ? 'e4' : 'g4'); \
                 note('g4');",
            )
            .expect("prebake");
        assert_eq!(
            session.generation(),
            generation,
            "prebake replaced the active graph"
        );
        assert_eq!(session.last_source, last_source, "prebake became the score");
        assert_eq!(session.last_evaluate_source(), last_path);
        let after = session
            .js
            .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .expect("active query immediately after successful prebake")
            .into_iter()
            .map(|hap| hap.show())
            .collect::<Vec<_>>();
        assert_eq!(
            after, before,
            "successful prebake replaced the active graph"
        );
        assert!(
            session.transport().is_stopped(),
            "prebake restarted a stopped transport"
        );

        session.transport().start();
        session
            .reload_at("fromPrebake()", false, 1.0)
            .expect("score reload using setup helper");
        session
            .reload_at("fromPrebake().fast(2)", false, 2.0)
            .expect("second score reload");
        assert_eq!(
            session.js.get_number("loadCount"),
            Some(1.0),
            "score reload reran the prebake"
        );
        let haps = session
            .query(Fraction::int(2), Fraction::int(3))
            .expect("query reloaded score");
        assert_eq!(haps.len(), 2);
        assert!(
            haps.iter().all(|hap| hap.value.show().contains("e4")),
            "score reload lost the same-heap helper: {:?}",
            haps.iter().map(|hap| hap.value.show()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn configured_parser_is_consumed_once_before_query_play_and_render() {
        let mut session = Session::new().expect("session");
        session
            .evaluate_prebake(
                "globalThis.productParserCalls = 0; \
                 setStringParser(value => { productParserCalls++; return mini(value); });",
            )
            .expect("install product string parser");
        session
            .evaluate("(() => stack(['c4', 'e4'].join(' ')).note())()")
            .expect("score through configured parser");
        assert_eq!(session.js.get_number("productParserCalls"), Some(1.0));
        let queried: Vec<(String, Fraction, Fraction)> = session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query configured-parser score")
            .into_iter()
            .map(|hap| (hap.value.show(), hap.part.begin, hap.part.end))
            .collect();
        assert_eq!(
            queried,
            [
                ("note:c4".to_owned(), Fraction::ZERO, Fraction::new(1, 2),),
                ("note:e4".to_owned(), Fraction::new(1, 2), Fraction::ONE,),
            ],
            "the product query counted a parser call but ignored its two-step Pattern"
        );
        assert!(
            !session
                .play(1.0)
                .expect("play configured-parser score")
                .onsets
                .is_empty()
        );

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let output = std::env::temp_dir().join(format!(
            "rustel-parser-render-{}-{unique}.json",
            std::process::id()
        ));
        let report = session
            .render(1.0, &output, RenderFormat::OnsetJson)
            .expect("render configured-parser score");
        assert!(report.onset_count > 0, "render produced no onsets");
        std::fs::remove_file(&output).expect("remove owned onset dump");
        assert_eq!(
            session.js.get_number("productParserCalls"),
            Some(1.0),
            "query/play/render reparsed a construction-time string"
        );
    }

    #[test]
    fn failed_prebake_keeps_partial_js_mutations_but_not_scheduler_state() {
        let mut session = Session::new().expect("session");
        session
                .evaluate(
                    "(() => { let calls = 0; return pure('last-good').fmap(value => `${value}:${++calls}`); })()",
                )
                .expect("callback-bearing initial score");
        let generation = session.generation();
        assert_eq!(active_values(&session), ["last-good:1"]);
        let error = session
            .evaluate_prebake(
                "globalThis.__rustel_active = { query: null }; \
                 globalThis.__rustel_active.query = () => []; \
                 globalThis.partialSetup = 17; \
                 Pattern.prototype.afterFailure = function () { return this.fast(2); }; \
                 throw new Error('setup stopped here');",
            )
            .expect_err("throwing prebake must fail");
        assert_eq!(error.kind(), "evaluation");
        assert_eq!(session.js.get_number("partialSetup"), Some(17.0));
        assert_eq!(session.generation(), generation);
        session.js.run_gc();
        session.js.run_gc();
        assert_eq!(
            active_values(&session),
            ["last-good:2"],
            "failed setup replaced the active wrapper or its callback"
        );
        session
            .reload_at("note('e4').afterFailure()", false, 1.0)
            .expect("partial prototype side effect");
    }

    #[test]
    fn editor_visual_calls_keep_the_native_audio_graph_portable() {
        let mut plain = Session::new().expect("plain session");
        plain.evaluate(r#"s("bd sd")"#).expect("plain score");
        let expected = plain
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("plain query")
            .into_iter()
            .map(|hap| hap.show())
            .collect::<Vec<_>>();

        for method in [
            "_scope",
            "_tscope",
            "_pianoroll",
            "_punchcard",
            "_spiral",
            "_pitchwheel",
            "_spectrum",
            "scope",
            "tscope",
            "pianoroll",
            "punchcard",
            "spiral",
            "pitchwheel",
            "spectrum",
            "markcss",
        ] {
            let argument = if method == "markcss" {
                "'color: cyan; text-decoration: underline'"
            } else {
                "{}"
            };
            let mut visual = Session::new().expect("visual session");
            visual
                .evaluate(&format!(r#"s("bd sd").{method}({argument})"#))
                .unwrap_or_else(|error| panic!("{method} did not evaluate: {error}"));
            assert_eq!(
                visual
                    .query(Fraction::ZERO, Fraction::ONE)
                    .unwrap_or_else(|error| panic!("{method} did not query: {error}"))
                    .into_iter()
                    .map(|hap| hap.show())
                    .collect::<Vec<_>>(),
                expected,
                "{method} changed the sounding graph"
            );
        }

        let mut all = Session::new().expect("all visual session");
        all.evaluate(r#"all(pianoroll); s("bd sd")"#)
            .expect("all(pianoroll) score");
        assert_eq!(
            all.query(Fraction::ZERO, Fraction::ONE)
                .expect("all(pianoroll) query")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn removed_wavetable_visual_is_not_installed() {
        let mut session = Session::new().expect("session");
        assert!(
            session.evaluate(r#"s("bd")._wavetable()"#).is_err(),
            "the removed visual must not remain as a silent compatibility call"
        );
    }

    #[test]
    fn extract_s_call() {
        assert_eq!(
            extract_mini_fallback(r#"s("bd sd")"#).as_deref(),
            Some("bd sd")
        );
    }

    #[test]
    fn rust_mini_query_bd_sd() {
        let mut session = Session::new().unwrap();
        session.evaluate_mini("bd sd").unwrap();
        let haps = session.query(Fraction::ZERO, Fraction::ONE).unwrap();
        assert_eq!(haps.len(), 2);
        assert_eq!(haps[0].value.show(), "bd");
        assert_eq!(haps[1].value.show(), "sd");
    }
}
