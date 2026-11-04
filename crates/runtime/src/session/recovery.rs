//! Native unwind containment on producer threads.

use std::panic::{AssertUnwindSafe, catch_unwind};

use super::*;

/// Deterministic producer failures used by integration tests.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionPanicPoint {
    Evaluation,
    ReplacementProbe,
    Install,
    Query,
}

thread_local! {
    static CONTAINING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether this thread is inside [`Session::with_panic_recovery`] or
/// [`catch_score_panic`]. That boundary catches a panic and its caller
/// reports it, so a panic hook can leave the reporting to the caller.
pub fn panic_is_contained() -> bool {
    CONTAINING.with(std::cell::Cell::get)
}

/// Run score work that has no Session to rebuild, such as sampling a
/// pattern the host keeps, and return a panic as its message for the
/// caller to report.
pub fn catch_score_panic<R>(work: impl FnOnce() -> R) -> Result<R, String> {
    let _containing = Containing::enter();
    catch_unwind(AssertUnwindSafe(work)).map_err(|payload| {
        let message = panic_message(&*payload);
        drop_caught(payload);
        message
    })
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "native panic".into())
}

/// Marks this thread as inside a recovery boundary until dropped.
struct Containing(bool);

impl Containing {
    fn enter() -> Self {
        Self(CONTAINING.with(|flag| flag.replace(true)))
    }
}

impl Drop for Containing {
    fn drop(&mut self) {
        CONTAINING.with(|flag| flag.set(self.0));
    }
}

#[derive(Clone)]
pub(super) struct RecoveryPrebake {
    pub(super) source: Arc<str>,
    pub(super) error: Option<Arc<str>>,
    pub(super) voicing_identity: Option<rustel_core::settings::VoicingDictionaryIdentity>,
}

struct RecoveryCheckpoint {
    source: Option<(Arc<str>, bool)>,
    prebakes: Arc<Vec<RecoveryPrebake>>,
    csound: crate::CsoundSnapshot,
    settings: RuntimeSettings,
    cps: f64,
    cycle: f64,
    query_hap_budget: u64,
    trace_enabled: bool,
    js_memory_limit: usize,
    #[cfg(feature = "device-audio")]
    confirmed_generation: Option<u64>,
}

impl Session {
    /// Changes whenever native panic recovery replaces this Session's realm.
    pub fn recovery_epoch(&self) -> u64 {
        self.recovery_epoch
    }

    /// Generation installed by the most recent realm recovery.
    pub fn recovery_generation(&self) -> u64 {
        self.recovery_generation
    }

    /// Contain native unwinds from producer work and rebuild its Session.
    ///
    /// The host's transport and audio callback remain alive. A panic discards
    /// the graph and JavaScript realm, replays retained setup and last-good
    /// source in a fresh realm, and returns [`RuntimeError::Panic`]. Hosts use
    /// this boundary for additional preview or control work that enters JS.
    /// It does not contain aborts, stack overflow, OOM, or blocking native code.
    pub fn with_panic_recovery<T>(
        &mut self,
        now: f64,
        operation: impl FnOnce(&mut Self) -> Result<T, RuntimeError>,
    ) -> Result<T, RuntimeError> {
        if self.panic_guard_active {
            return operation(self);
        }
        if self.panic_poisoned {
            return Err(RuntimeError::Panic(
                "native score work panicked; Session recovery is unavailable".into(),
            ));
        }
        let now = if now.is_finite() && now >= 0.0 {
            now
        } else {
            0.0
        };
        let _containing = Containing::enter();
        let mut checkpoint = self.recovery_checkpoint(now);
        self.panic_is_query.set(false);
        self.panic_guard_active = true;
        let operation_started = Instant::now();
        let result = catch_unwind(AssertUnwindSafe(|| {
            let result = operation(self);
            self.js.raise_caught_native_panic();
            result
        }));
        self.panic_guard_active = false;
        match result {
            Ok(result) => {
                self.panic_is_query.set(false);
                result
            }
            Err(payload) => {
                self.panic_poisoned = true;
                let elapsed = operation_started.elapsed().as_secs_f64();
                let recovery_now = now + elapsed;
                checkpoint.cycle += elapsed * checkpoint.cps;
                if self.panic_is_query.get()
                    && checkpoint
                        .source
                        .as_ref()
                        .is_some_and(|(source, _)| self.recovered_source.as_ref() == Some(source))
                {
                    checkpoint.source = None;
                }
                let detail = panic_message(&*payload);
                // A panic payload is arbitrary host data and can itself have
                // a panicking destructor. It is never reused by the Session.
                drop_caught(payload);
                let recovery = catch_unwind(AssertUnwindSafe(|| {
                    self.rebuild_after_panic(recovery_now, checkpoint)
                }));
                let disposition = match recovery {
                    Ok(Ok(true)) => "last-good source restored",
                    Ok(Ok(false)) => "fresh Session ready for the next score",
                    Ok(Err(_)) => {
                        self.panic_poisoned = true;
                        "Session recovery failed"
                    }
                    Err(payload) => {
                        self.panic_poisoned = true;
                        drop_caught(payload);
                        "Session recovery failed"
                    }
                };
                Err(RuntimeError::Panic(format!(
                    "native score work panicked; {disposition} ({detail})"
                )))
            }
        }
    }

    fn recovery_checkpoint(&self, now: f64) -> RecoveryCheckpoint {
        let cps = self.scheduler.cps();
        let cycle = self.scheduler.cycle_at_time(now);
        let source = self
            .last_source
            .clone()
            .map(|source| (source, self.last_path == EvaluateSource::MiniRust));
        #[cfg(feature = "device-audio")]
        // The last confirmed audio wins over an unconfirmed install; before
        // any window has sounded, the installed score is all there is.
        let source = self
            .audible_source
            .as_ref()
            .map(|source| (source.source.clone(), source.mini))
            .or(source);
        #[cfg(feature = "device-audio")]
        let (cps, cycle) = self.audible_source.as_ref().map_or((cps, cycle), |source| {
            (source.cps, (now - source.cycle_zero_time) * source.cps)
        });
        let settings = self.js.snapshot_published_runtime_settings();
        #[cfg(feature = "device-audio")]
        let settings = self
            .audible_source
            .as_ref()
            .map_or(settings, |source| source.settings.clone());
        let csound = self.csound_snapshot();
        #[cfg(feature = "device-audio")]
        let csound = self
            .audible_source
            .as_ref()
            .map_or(csound, |source| source.csound.clone());
        RecoveryCheckpoint {
            source,
            settings,
            prebakes: self.recovery_prebakes.clone(),
            csound,
            cps,
            cycle,
            query_hap_budget: self.scheduler.query_hap_budget(),
            trace_enabled: self.scheduler.trace_enabled(),
            js_memory_limit: self.js.memory_limit(),
            #[cfg(feature = "device-audio")]
            confirmed_generation: self.audible_source.as_ref().map(|source| source.generation),
        }
    }

    fn rebuild_after_panic(
        &mut self,
        now: f64,
        checkpoint: RecoveryCheckpoint,
    ) -> Result<bool, RuntimeError> {
        // Slider moves live in the realm, not in the score text.
        let sliders = self.js.slider_values().unwrap_or_default();
        let mut config = self.config.clone();
        config.cps = checkpoint.cps;
        let mut fresh = Self::with_config(config)?;
        let voicing_identity = checkpoint.settings.host_voicing_identity();
        let settings = checkpoint.settings.recovery_snapshot();
        fresh.js.adopt_runtime_settings(&settings);
        fresh.core_settings_initialized = true;
        fresh.core_settings_owned = true;
        // Hosts feed the bus they hold, and controllers keep their values.
        fresh.js.adopt_midi_input_bus(self.js.midi_input_bus());
        fresh.recovery_epoch = self.recovery_epoch.saturating_add(1);
        fresh.set_js_memory_limit(checkpoint.js_memory_limit)?;
        fresh.transport = self.transport.clone();
        fresh.scheduler = Scheduler::new(
            fresh.transport.clone(),
            checkpoint.cps,
            fresh.config.horizon,
        );
        fresh.scheduler.rebase_anchor(now, checkpoint.cycle);
        fresh
            .scheduler
            .set_query_hap_budget(checkpoint.query_hap_budget);
        fresh.scheduler.set_trace_enabled(checkpoint.trace_enabled);
        fresh.samples = self.samples.clone();
        fresh.csound = self.csound.clone();
        fresh.direct_diagnostic_logging = self.direct_diagnostic_logging;
        fresh.audio_input_channels = self.audio_input_channels;
        fresh.schedule_lead = self.schedule_lead;
        fresh.continuity_margin = self.continuity_margin;
        // The replayed score takes over on a frame of the same device.
        fresh.live_sample_rate = self.live_sample_rate;
        // The output keeps its plugins and its rate. Its names source and its
        // values source read the chains of this session, so the rebuilt
        // session keeps the same chains, with the flag of their slider links.
        // Each lane keeps its number, so the replayed score has the chains it
        // had. A plugin call of an evaluation before stays known.
        #[cfg(feature = "vst")]
        {
            let live = self.plugins_live.load(std::sync::atomic::Ordering::Relaxed);
            fresh.plugins_live = AtomicBool::new(live);
            fresh.plugin_rate = std::mem::take(&mut self.plugin_rate);
            fresh.ran_plugin_calls = self.ran_plugin_calls;
            fresh.chains = self.chains.clone();
            fresh.slider_links = self.slider_links.clone();
            fresh.lanes = std::mem::take(&mut self.lanes);
        }
        // An edit that waits for its plugin copies waits on, with its plan in
        // the chains: the replay plays the score in play.
        #[cfg(all(feature = "vst", feature = "device-audio"))]
        {
            fresh.plugin_wait = self.plugin_wait;
            fresh.plugin_hold = self.plugin_hold.take();
        }
        fresh.stop_when_silent = self.stop_when_silent;
        fresh.export_limiter = self.export_limiter;
        fresh.render_tail_secs = self.render_tail_secs;
        fresh.pending_diagnostics = std::mem::take(&mut self.pending_diagnostics);
        // The replay runs the imported scripts again, with no new fetch.
        fresh.import_sources = std::mem::take(&mut self.import_sources);
        #[cfg(feature = "device-audio")]
        {
            // Keep output receipts. Replay sources refer to the discarded realm.
            fresh.audio_confirmations = self.audio_confirmations.take();
            if let Some(book) = &mut fresh.audio_confirmations {
                book.discard_replay_sources();
            }
            // The device still holds what it was sent. The replayed score
            // takes over from it like any other generation.
            fresh.compensated_onsets = std::mem::take(&mut self.compensated_onsets);
            // The messages a host handed out stay out too.
            #[cfg(feature = "osc")]
            {
                fresh.osc_handed_out = std::mem::take(&mut self.osc_handed_out);
            }
            #[cfg(feature = "serial")]
            {
                fresh.serial_handed_out = std::mem::take(&mut self.serial_handed_out);
            }
            // The keyboards stay with the bus, and their presses stay placed.
            #[cfg(any(feature = "osc", feature = "serial"))]
            {
                fresh.placed_presses = std::mem::take(&mut self.placed_presses);
            }
        }
        let dirty = std::mem::replace(self, fresh);
        drop_caught(dirty);

        // Reconstruction must not recursively recover the same failing source.
        // A failed replay leaves another empty, clean Session for future edits.
        self.panic_guard_active = true;
        let replay_started = Instant::now();
        let replay = catch_unwind(AssertUnwindSafe(|| -> Result<(), RuntimeError> {
            let mut replayed_voicings = None;
            for prebake in checkpoint.prebakes.iter() {
                self.evaluate_prebake_guarded(
                    &prebake.source,
                    PREBAKE_CPU_BUDGET,
                    None,
                    prebake.error.as_deref(),
                )?;
                if voicing_identity.is_some() && prebake.voicing_identity == voicing_identity {
                    replayed_voicings = Some(self.js.snapshot_published_runtime_settings());
                }
            }
            // Setup may replay older orchestra definitions.
            self.restore_csound_snapshot(&checkpoint.csound);
            // Match the old lease identity, never a private token from another realm.
            if let Some(replayed) = replayed_voicings
                && replayed.host_voicing_identity().is_some()
            {
                settings.adopt_replayed_voicing_default(&replayed);
            }
            // Setup restores globals. The audible score keeps its later settings.
            self.js.adopt_runtime_settings(&settings);
            if let Some((source, mini)) = &checkpoint.source {
                self.reload_at_cancellable(source, *mini, now, &NEVER_CANCELLED)?;
                // Score evaluation resets join. Restore the later host selection.
                let join = settings.with(rustel_core::compose::default_alignment);
                self.set_default_join(join);
                for (id, value) in &sliders {
                    // A slider the replayed score no longer has is skipped.
                    let _ = self.js.set_slider_value(id, *value);
                }
                self.requery_active_at(now + replay_started.elapsed().as_secs_f64())?;
            }
            Ok(())
        }));
        self.panic_guard_active = false;
        if replay.as_ref().is_ok_and(|result| result.is_ok()) {
            self.recovered_source = checkpoint.source.as_ref().map(|(source, _)| source.clone());
            self.recovery_generation = self.generation();
            #[cfg(feature = "device-audio")]
            if let (Some(generation), Some((source, mini))) =
                (checkpoint.confirmed_generation, checkpoint.source.as_ref())
            {
                self.audible_source = Some(AudibleSource {
                    generation,
                    source: source.clone(),
                    mini: *mini,
                    settings: self.js.snapshot_published_runtime_settings(),
                    csound: self.csound_snapshot(),
                    cps: checkpoint.cps,
                    cycle_zero_time: now - checkpoint.cycle / checkpoint.cps,
                    evaluated_cycle: self.installed_cycle,
                });
            }
            return Ok(checkpoint.source.is_some());
        }
        if let Err(payload) = replay {
            drop_caught(payload);
        }
        // Fall back to the setup alone, then to an empty Session. Empty
        // construction does not enter score code, so the fallback ends.
        //
        //   setup + source --fails--> setup alone --fails--> empty Session
        //   Ok(true)                  Ok(false)              Ok(false)
        let mut fallback = checkpoint;
        if fallback.source.take().is_none() {
            fallback.prebakes = Arc::default();
            fallback.csound = crate::CsoundSnapshot::default();
        }
        self.rebuild_after_panic(now, fallback)?;
        Ok(false)
    }

    /// Inject one unwind at the next selected producer phase.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn inject_panic_for_test(&self, point: SessionPanicPoint) {
        self.injected_panic.set(Some(point));
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn panic_if_injected(&self, point: SessionPanicPoint) {
        if self.injected_panic.get() == Some(point) {
            self.injected_panic.set(None);
            panic!("injected native {point:?} panic");
        }
    }
}

fn drop_caught<T>(value: T) {
    if let Err(payload) = catch_unwind(AssertUnwindSafe(|| drop(value))) {
        // A destructor's panic payload may also panic on drop. Leaking that
        // exceptional payload prevents a second unwind from escaping recovery.
        std::mem::forget(payload);
    }
}

#[cfg(test)]
mod tests {

    const GOOD: &str = "note(48).s('sine').fast(8)";
    const CANDIDATE: &str = "note(72).s('triangle').fast(8)";

    fn playing_session() -> Session {
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        session.evaluate(GOOD).expect("initial score");
        session
    }

    fn assert_panic(error: RuntimeError, point: &str) {
        assert!(matches!(error, RuntimeError::Panic(_)), "{error:?}");
        assert_eq!(error.kind(), "panic");
        assert!(error.to_string().contains(point), "{error}");
    }

    #[test]
    fn panicking_score_phases_restore_the_previous_source_and_transport() {
        for point in [
            SessionPanicPoint::Evaluation,
            SessionPanicPoint::ReplacementProbe,
            SessionPanicPoint::Install,
        ] {
            let mut session = playing_session();
            // A score before ran a plugin call. The replay runs none, and
            // the rebuilt session still knows the call.
            #[cfg(feature = "vst")]
            {
                let plugin = format!("{GOOD}.vst('Delay')");
                session.evaluate(&plugin).expect("plugin score");
                session.evaluate(GOOD).expect("initial score");
            }
            let transport = session.transport();
            let generation = session.generation();
            let cycle = session.cycle_at_time(0.5);
            session.inject_panic_for_test(point);
            let error = session
                .reload_at(CANDIDATE, false, 0.5)
                .expect_err("candidate panics");
            assert_panic(error, &format!("{point:?}"));
            assert_eq!(session.active_source(), Some(GOOD));
            #[cfg(feature = "vst")]
            assert!(
                session.ran_plugin_calls(),
                "the rebuilt session lost its plugin call"
            );
            assert!(Arc::ptr_eq(&session.transport(), &transport));
            assert!(!transport.is_stopped());
            assert!(session.generation() > generation);
            assert!((session.cycle_at_time(0.5) - cycle).abs() < 1e-9);
            assert!(
                !session
                    .schedule_through(0.5, 1.0)
                    .expect("restored source schedules")
                    .is_empty()
            );
            session
                .reload_at(CANDIDATE, false, 1.0)
                .expect("the next edit works");
            assert_eq!(session.active_source(), Some(CANDIDATE));
        }
    }

    #[test]
    fn a_scheduler_panic_restores_the_source_and_next_query_works() {
        let mut session = playing_session();
        #[cfg(not(feature = "vst"))]
        let source = GOOD.to_owned();
        #[cfg(feature = "vst")]
        let source = {
            let source = format!("$: {GOOD}.vst('Delay').orbit(1)");
            session.evaluate(&source).expect("plugin source");
            // A new lane above: the plugin lane keeps its number.
            let moved = format!("$: s('hh')\n{}", source.replace("orbit(1)", "orbit(2)"));
            session.reload_at(&moved, false, 0.1).expect("move plugin");
            moved
        };
        #[cfg(feature = "vst")]
        let chain = super::super::tests::plugin_chain(&session);
        // The session plays through the plugins of a live output.
        #[cfg(feature = "vst")]
        session
            .plugins_live
            .store(true, std::sync::atomic::Ordering::Relaxed);
        #[cfg(feature = "vst")]
        session
            .plugin_rate
            .store(44_100, std::sync::atomic::Ordering::Relaxed);
        // The host of the output plays each edit at once.
        #[cfg(all(feature = "vst", feature = "device-audio"))]
        session.set_plugin_wait(false);
        let transport = session.transport();
        let generation = session.generation();
        session.inject_panic_for_test(SessionPanicPoint::Query);
        let error = session
            .schedule_through(0.25, 0.75)
            .expect_err("scheduler query panics");
        assert_panic(error, "Query");
        assert_eq!(session.active_source(), Some(source.as_str()));
        // The rebuilt session keeps the chains its output reads. A new
        // registry gives a new number.
        #[cfg(feature = "vst")]
        assert_eq!(super::super::tests::plugin_chain(&session), chain);
        #[cfg(feature = "vst")]
        assert_eq!(
            super::super::tests::plugin_note(&session)["__rustel_lane"],
            "#0",
            "the rebuilt session lost its lanes"
        );
        #[cfg(feature = "vst")]
        assert!(
            Arc::ptr_eq(
                &session.slider_links,
                &crate::vst::slider_links(&session.chains)
            ),
            "the rebuilt session lost the slider links of its chains"
        );
        #[cfg(feature = "vst")]
        assert!(
            session
                .plugins_live
                .load(std::sync::atomic::Ordering::Relaxed),
            "the rebuilt session lost the plugins of its output"
        );
        #[cfg(feature = "vst")]
        assert_eq!(
            session
                .plugin_rate
                .load(std::sync::atomic::Ordering::Relaxed),
            44_100,
            "the rebuilt session lost the rate of its output"
        );
        #[cfg(all(feature = "vst", feature = "device-audio"))]
        assert!(
            !session.plugin_wait,
            "the rebuilt session lost the wait setting of its host"
        );
        assert!(Arc::ptr_eq(&session.transport(), &transport));
        assert!(session.generation() > generation);
        // Recovery resumes after the time the rebuild took, so a slow machine
        // can start the restored score in a later window.
        let scheduled = (0..16).any(|window| {
            let start = 0.25 + f64::from(window) * 0.5;
            !session
                .schedule_through(start, start + 0.5)
                .expect("fresh graph schedules")
                .is_empty()
        });
        assert!(scheduled, "the restored score never scheduled");
    }

    #[test]
    fn host_boundary_discards_mutated_globals_and_replays_successful_setup() {
        let mut session = Session::new().expect("session");
        session
            .evaluate_prebake("globalThis.recoveryNote = 48;")
            .expect("setup");
        let source = "note(recoveryNote).s('sine').fast(8)";
        session.evaluate(source).expect("source uses setup");
        session
            .js
            .eval("globalThis.oldRealmResidue = 1;")
            .expect("old realm state");
        let transport = session.transport();
        let error = session
            .with_panic_recovery::<()>(0.5, |session| {
                session
                    .js
                    .eval("globalThis.recoveryNote = 72; globalThis.candidateResidue = 1;")
                    .expect("partially mutated score turn");
                panic!("host query panic");
            })
            .expect_err("host panic is caught");
        assert_panic(error, "host query panic");
        assert_eq!(session.active_source(), Some(source));
        assert!(Arc::ptr_eq(&session.transport(), &transport));
        assert_eq!(session.js.get_number("recoveryNote").unwrap(), 48.0);
        session
            .js
            .eval(
                "if ('oldRealmResidue' in globalThis || 'candidateResidue' in globalThis) \
             throw new Error('discarded realm was reused');",
            )
            .expect("neither old nor candidate realm state survives");
        assert!(
            !session
                .schedule_through(0.5, 1.0)
                .expect("replayed setup remains usable")
                .is_empty()
        );
    }

    #[test]
    fn recovery_preserves_tightened_host_limits() {
        let mut session = playing_session();
        let limit = session.js_heap_live() + 16 * 1024 * 1024;
        session
            .set_js_memory_limit(limit)
            .expect("lower heap limit");
        session.set_query_hap_budget(512).expect("lower hap limit");
        session.set_schedule_trace_enabled(true);
        session.inject_panic_for_test(SessionPanicPoint::Query);
        session
            .schedule_through(0.0, 0.5)
            .expect_err("query panics");
        assert_eq!(session.js.memory_limit(), limit);
        assert_eq!(session.scheduler.query_hap_budget(), 512);
        assert!(session.scheduler.trace_enabled());
    }

    #[test]
    fn repeated_query_panics_park_the_source_until_a_new_edit() {
        let mut session = playing_session();
        for _ in 0..2 {
            session.inject_panic_for_test(SessionPanicPoint::Query);
            session
                .schedule_through(0.0, 0.5)
                .expect_err("query panics");
        }
        assert_eq!(session.active_source(), None);
        let epoch = session.recovery_epoch();
        assert!(matches!(
            session.schedule_through(0.0, 0.5),
            Err(RuntimeError::NoPattern)
        ));
        assert_eq!(session.recovery_epoch(), epoch);
        session
            .reload_at(CANDIDATE, false, 0.5)
            .expect("a new edit unblocks playback");
        assert_eq!(session.active_source(), Some(CANDIDATE));
    }

    #[test]
    fn successive_panicking_candidates_do_not_park_the_good_source() {
        for point in [
            SessionPanicPoint::Evaluation,
            SessionPanicPoint::ReplacementProbe,
            SessionPanicPoint::Install,
        ] {
            let mut session = playing_session();
            for candidate in [CANDIDATE, "note(60).s('square')"] {
                session.inject_panic_for_test(point);
                session
                    .reload_at(candidate, false, 0.5)
                    .expect_err("candidate panics");
                assert_eq!(session.active_source(), Some(GOOD));
            }
            assert!(
                !session
                    .schedule_through(0.5, 1.0)
                    .expect("good source still schedules")
                    .is_empty()
            );
        }
        // The commit seam reads the lanes of a candidate that do not play,
        // and the read panics 2 times. The score in play stays.
        #[cfg(all(feature = "vst", feature = "device-audio"))]
        {
            let _plugins = super::super::tests::fixture_plugins();
            let mut session = playing_session();
            let relaxed = std::sync::atomic::Ordering::Relaxed;
            session.plugins_live.store(true, relaxed);
            session.plugin_rate.store(48_000, relaxed);
            let candidate = format!("$: {CANDIDATE}\n_$: {GOOD}.vst('rustel fixture')");
            let stopped = AtomicBool::new(false);
            for now in [0.1, 0.2] {
                session.schedule_through(now, now).expect("cover");
                session.inject_panic_for_test(SessionPanicPoint::Query);
                let reload = session.reload_live(&candidate, false, &stopped, || now + 0.05);
                reload.expect_err("the read of the muted lane panics");
                assert_eq!(session.active_source(), Some(GOOD));
            }
        }
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn successive_candidate_panics_keep_the_last_confirmed_source_until_new_audio() {
        let mut session = playing_session();
        let generation = session.generation();
        let output =
            rustel_audio::device::ManualLiveOutput::new(48_000, generation).expect("manual output");
        session
            .bind_audio_confirmations(output.device().confirmations())
            .expect("bind output confirmations");
        session
            .mark_audible_generation(generation)
            .expect("the original score is confirmed");

        for candidate in [CANDIDATE, "note(60).s('square')"] {
            session.inject_panic_for_test(SessionPanicPoint::Evaluation);
            session
                .reload_at(candidate, false, 0.5)
                .expect_err("candidate panics before restored audio is confirmed");
            assert_eq!(session.active_source(), Some(GOOD));
            assert_eq!(
                session.audible_source.as_ref().unwrap().generation,
                generation
            );
        }
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn an_unconfirmed_candidate_cannot_change_the_restored_tempo_or_timeline() {
        let mut session = playing_session();
        session
            .mark_audible_generation(session.generation())
            .expect("the first source is confirmed");
        let expected_cps = session.cps();
        let expected_cycle = session.cycle_at_time(1.25);
        session
            .reload_at("setcps(2); note(72).s('triangle')", false, 1.0)
            .expect("candidate awaits its first audio window");
        assert_eq!(session.cps(), 2.0);
        session.inject_panic_for_test(SessionPanicPoint::Query);
        session
            .schedule_through(1.25, 1.75)
            .expect_err("candidate query panics before publication");
        assert_eq!(session.active_source(), Some(GOOD));
        assert_eq!(session.cps(), expected_cps);
        assert!((session.cycle_at_time(1.25) - expected_cycle).abs() < 1e-9);
    }

    #[test]
    fn a_parked_source_keeps_the_setup_for_the_next_edit() {
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        session
            .evaluate_prebake("globalThis.recoveryNote = 48;")
            .expect("setup");
        session
            .evaluate("note(recoveryNote).s('sine').fast(8)")
            .expect("source uses setup");
        for _ in 0..2 {
            session.inject_panic_for_test(SessionPanicPoint::Query);
            session
                .schedule_through(0.0, 0.5)
                .expect_err("query panics");
        }
        assert_eq!(session.active_source(), None);
        assert_eq!(session.js.get_number("recoveryNote").unwrap(), 48.0);
        session
            .reload_at("note(recoveryNote + 12).s('sine').fast(8)", false, 0.5)
            .expect("the next edit still sees the setup");
    }

    #[test]
    fn a_setup_run_again_is_replayed_once() {
        let mut session = Session::new().expect("session");
        for _ in 0..3 {
            session
                .evaluate_prebake("globalThis.recoveryNote = 48;")
                .expect("setup");
        }
        assert_eq!(session.recovery_prebakes.len(), 1);
    }

    #[test]
    fn only_work_inside_the_boundary_is_marked_contained() {
        let mut session = playing_session();
        assert!(!panic_is_contained());
        let inside = session
            .with_panic_recovery(0.0, |_| Ok(panic_is_contained()))
            .expect("no panic");
        assert!(inside);
        assert!(!panic_is_contained());
    }

    #[test]
    fn a_moved_slider_keeps_its_value_through_a_rebuild() {
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        session
            .evaluate("note(slider(48, 0, 127)).s('sine').fast(8)")
            .expect("slider score");
        let cells = session.js.slider_values().expect("slider cells");
        let (id, _) = cells.first().expect("one slider").clone();
        assert!(session.set_slider_value(&id, 60.0).expect("slider write"));
        session.inject_panic_for_test(SessionPanicPoint::Query);
        session
            .schedule_through(0.0, 0.5)
            .expect_err("query panics");
        assert_eq!(session.slider_value(&id).expect("slider read"), Some(60.0));
    }

    #[test]
    fn a_score_panic_without_a_session_returns_its_message() {
        let caught = catch_score_panic(|| -> u8 {
            assert!(panic_is_contained());
            panic!("signal panic")
        });
        assert_eq!(caught, Err("signal panic".to_owned()));
        assert!(!panic_is_contained());
        assert_eq!(catch_score_panic(|| 7), Ok(7));
    }

    #[test]
    fn a_query_outside_a_recovery_boundary_reports_its_panic() {
        let session = playing_session();
        session.inject_panic_for_test(SessionPanicPoint::Query);
        let error = session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect_err("query panics");
        assert_panic(error, "Query");
        assert_eq!(session.active_source(), Some(GOOD));
        assert!(
            !session
                .query(Fraction::ZERO, Fraction::ONE)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_rebuild_keeps_the_midi_input_bus_hosts_feed() {
        let mut session = playing_session();
        let bus = session.midi_input_bus();
        session.inject_panic_for_test(SessionPanicPoint::Query);
        session
            .schedule_through(0.0, 0.5)
            .expect_err("query panics");
        assert!(Arc::ptr_eq(&session.midi_input_bus(), &bus));
    }

    #[test]
    fn recovery_preserves_host_settings_after_setup_replay() {
        let mut session = Session::new().expect("session");
        session
            .evaluate_prebake("useRNG('legacy'); setDefaultVoicings('lefthand');")
            .expect("setup defaults");
        session.evaluate(GOOD).expect("score");
        session.set_rng_mode(rustel_core::rng::RngMode::Precise);
        session.set_default_join(rustel_core::compose::Alignment::Out);
        session.set_default_voicings(Some("guidetones"));
        session.with_runtime_settings(|| rustel_core::settings::set_max_polyphony(37));
        assert_restored_settings(&session);
        session.inject_panic_for_test(SessionPanicPoint::Query);
        session
            .schedule_through(0.0, 0.5)
            .expect_err("query panics");
        assert_eq!(session.active_source(), Some(GOOD));
        assert_restored_settings(&session);
    }

    fn assert_restored_settings(session: &Session) {
        session.js.snapshot_published_runtime_settings().with(|| {
            assert_eq!(
                rustel_core::rng::rng_mode(),
                rustel_core::rng::RngMode::Precise
            );
            assert_eq!(
                rustel_core::compose::default_alignment(),
                rustel_core::compose::Alignment::Out
            );
            assert_eq!(
                rustel_core::voicings::selected_default_voicings(),
                "guidetones"
            );
            assert_eq!(rustel_core::settings::max_polyphony(), Some(37));
        });
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn an_unconfirmed_candidate_cannot_change_the_restored_native_settings() {
        let mut session = Session::new().expect("session");
        session.evaluate(GOOD).expect("audible score");
        session.set_rng_mode(rustel_core::rng::RngMode::Precise);
        session.set_default_join(rustel_core::compose::Alignment::Out);
        session.set_default_voicings(Some("guidetones"));
        session.with_runtime_settings(|| rustel_core::settings::set_max_polyphony(37));
        assert_restored_settings(&session);
        session
            .mark_audible_generation(session.generation())
            .expect("confirmed score");
        session.set_rng_mode(rustel_core::rng::RngMode::Legacy);
        session.set_default_join(rustel_core::compose::Alignment::Mix);
        session.set_default_voicings(Some("lefthand"));
        session.with_runtime_settings(|| rustel_core::settings::set_max_polyphony(99));
        session.reload_at(CANDIDATE, false, 0.0).expect("candidate");
        session.inject_panic_for_test(SessionPanicPoint::Query);
        session
            .schedule_through(0.0, 0.5)
            .expect_err("candidate query panics");
        assert_eq!(session.active_source(), Some(GOOD));
        assert_restored_settings(&session);
    }

    #[test]
    fn recovery_replays_setup_mutations_before_an_ordinary_throw() {
        let mut session = Session::new().expect("session");
        let setup = "globalThis.recoveryNote = 48; throw new Error('setup incomplete');";
        assert!(
            session
                .evaluate_prebake(setup)
                .expect_err("partial setup")
                .to_string()
                .contains("setup incomplete")
        );
        let source = "note(recoveryNote).s('sine').fast(8)";
        session
            .evaluate(source)
            .expect("completed setup mutation is usable");
        session.inject_panic_for_test(SessionPanicPoint::Query);
        session
            .schedule_through(0.0, 0.5)
            .expect_err("query panics");
        assert_eq!(session.active_source(), Some(source));
        assert_eq!(session.js.get_number("recoveryNote").unwrap(), 48.0);
        assert_eq!(session.recovery_prebakes.len(), 1);
        assert!(
            !session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("restored helper")
                .is_empty()
        );
        session
            .reload_at("note(recoveryNote + 12).s('sine')", false, 0.5)
            .expect("next edit");
    }

    #[test]
    fn recovery_does_not_replay_a_cancelled_setup() {
        let mut session = playing_session();
        let cancelled = AtomicBool::new(true);
        assert!(matches!(
            session.evaluate_prebake_cancellable("globalThis.cancelledSetup = 1;", &cancelled),
            Err(RuntimeError::Cancelled)
        ));
        assert!(session.recovery_prebakes.is_empty());
        session.inject_panic_for_test(SessionPanicPoint::Query);
        session
            .schedule_through(0.0, 0.5)
            .expect_err("query panics");
        assert_eq!(session.active_source(), Some(GOOD));
        session
            .js
            .eval(
                "if ('cancelledSetup' in globalThis) throw new Error('cancelled setup replayed');",
            )
            .expect("cancelled setup stays absent");
    }

    #[test]
    fn recovery_keeps_a_setup_dictionary_live_for_an_existing_graph() {
        let mut session = Session::new().expect("session");
        session
            .evaluate_prebake(
                "globalThis.liveVoicings = {'7':['0 4 7']}; setDefaultVoicings(liveVoicings);",
            )
            .expect("setup dictionary");
        let source = "pure('C7').voicing()";
        session.evaluate(source).expect("score");
        assert_eq!(
            session.query(Fraction::ZERO, Fraction::ONE).unwrap().len(),
            3
        );
        session.inject_panic_for_test(SessionPanicPoint::Query);
        session
            .schedule_through(0.0, 0.5)
            .expect_err("query panics");
        assert_eq!(session.active_source(), Some(source));
        assert_eq!(
            session.query(Fraction::ZERO, Fraction::ONE).unwrap().len(),
            3
        );
        session
            .js
            .eval("liveVoicings['7'] = ['0 7']")
            .expect("mutate replayed object");
        assert_eq!(
            session.query(Fraction::ZERO, Fraction::ONE).unwrap().len(),
            2
        );
    }

    #[test]
    fn recovery_does_not_bind_an_unrelated_setup_dictionary() {
        let mut session = Session::new().expect("session");
        session
            .evaluate_prebake(
                "globalThis.setupVoicings = {'7':['0 4 7']}; setDefaultVoicings(setupVoicings);",
            )
            .expect("setup dictionary");
        session
            .js
            .eval("setDefaultVoicings({'7':['0 7']})")
            .expect("later selection");
        let source = "pure('C7').voicing()";
        session.evaluate(source).expect("score");
        assert_eq!(
            session.query(Fraction::ZERO, Fraction::ONE).unwrap().len(),
            2
        );
        session.inject_panic_for_test(SessionPanicPoint::Query);
        session
            .schedule_through(0.0, 0.5)
            .expect_err("query panics");
        assert_eq!(session.active_source(), Some(source));
        session
            .js
            .eval("setupVoicings['7'] = ['0']")
            .expect("mutate unrelated setup object");
        assert_eq!(
            session.query(Fraction::ZERO, Fraction::ONE).unwrap().len(),
            2
        );
    }

    #[test]
    fn recovery_rejects_setup_that_unexpectedly_succeeds_during_replay() {
        let mut config = SessionConfig::default();
        config
            .score_sample_access
            .permit_source("https://example.invalid")
            .unwrap();
        let mut session = Session::with_config(config).expect("session");
        let library = Arc::new(crate::samples::SampleLibrary::empty());
        session.samples = Some(library.clone());
        let first = "globalThis.recoveryFlag = true;";
        let failed = "if (globalThis.recoveryFlag) throw new Error('setup incomplete'); \
        globalThis.unapplied = 1; samples({ unapplied: ['https://example.invalid/unapplied.wav'] });";
        session.evaluate_prebake(first).expect("first setup");
        session.evaluate_prebake(failed).expect_err("partial setup");
        // The retained copy of this setup moves after the failed setup.
        session.evaluate_prebake(first).expect("repeat first setup");
        session.evaluate(GOOD).expect("score");
        session.inject_panic_for_test(SessionPanicPoint::Query);
        let error = session
            .schedule_through(0.0, 0.5)
            .expect_err("query panics");
        assert!(error.to_string().contains("fresh Session ready"), "{error}");
        assert_eq!(session.active_source(), None);
        assert!(session.recovery_prebakes.is_empty());
        session
            .js
            .eval("if ('unapplied' in globalThis) throw new Error('unexpected setup effect');")
            .expect("replay globals were discarded");
        library.wait_until_idle(Duration::from_secs(1));
        assert!(
            !library.knows("unapplied"),
            "rejected replay registered a sample"
        );
        session.reload_at(GOOD, false, 0.5).expect("next edit");
    }

    #[test]
    fn recovery_discards_an_orchestra_after_its_permission_is_revoked() {
        let mut config = SessionConfig::default();
        config.score_import_access.permit_csound();
        let mut session = Session::with_config(config).expect("session");
        if let Err(error) = session.evaluate_prebake("loadCSound`instr recoveryVoice\nendin`") {
            if std::env::var_os("RUSTEL_CSOUND_LIBRARY").is_none()
                && error.to_string().contains("Csound 6 is not available")
            {
                return;
            }
            panic!("load orchestra: {error}");
        }
        session
            .evaluate("note('c4').csound('recoveryVoice')")
            .expect("source uses orchestra");
        session.set_score_import_access(crate::ScoreImportAccess::default());
        let error = session
            .with_panic_recovery::<()>(0.5, |_| panic!("host query panic"))
            .expect_err("host panic is caught");
        assert!(error.to_string().contains("fresh Session ready"), "{error}");
        assert_eq!(session.active_source(), None);
        assert!(session.recovery_prebakes.is_empty());
        assert_eq!(session.csound_snapshot(), crate::CsoundSnapshot::default());
        session.reload_at(GOOD, false, 0.5).expect("next edit");
    }

    use super::*;
    use crate::session::tests::*;
    #[cfg(all(feature = "vst", feature = "device-audio"))]
    use crate::test_support::past_the_cover;
    use rustel_core::Value;
    #[cfg(feature = "device-audio")]
    #[test]
    fn rollback_reports_when_no_target_exists() {
        let mut session = Session::new().expect("session");
        assert_eq!(
            session
                .rollback_to_previous_source(0.0)
                .expect("absence is not an evaluation error"),
            RollbackAttempt::Unavailable
        );
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn captured_rollback_source_survives_later_sources_and_settings() {
        use rustel_core::compose::Alignment;
        use rustel_core::rng::RngMode;

        let mut session = Session::new().expect("session");
        session.set_default_voicings(Some("lefthand"));
        session.evaluate_mini("bd").expect("source A");
        // Inject replay policy, not consumer-copy evidence.
        session
            .mark_audible_generation(session.generation())
            .expect("injected replay-policy target");
        let generation_a = session.generation();
        session.set_rng_mode(RngMode::Precise);
        session.set_default_join(Alignment::Out);
        session.set_default_voicings(Some("guidetones"));
        let generation_b = session.reload_at("sd bd", true, 0.1).expect("source B");
        let captured = session
            .capture_rollback_source(generation_b)
            .expect("capture B");
        assert!(captured.is_some());
        assert_eq!(
            session_core_settings(&session),
            (RngMode::Precise, Alignment::Out, 2)
        );
        assert_eq!(
            session.audible_source.as_ref().unwrap().generation,
            generation_a
        );

        session.set_rng_mode(RngMode::Legacy);
        session.set_default_join(Alignment::Mix);
        session.set_default_voicings(Some("lefthand"));
        session.reload_at("hh", true, 0.2).expect("source C");
        let generation_d = session.reload_at("cp", true, 0.3).expect("source D");
        assert_eq!(generation_d, generation_b + 2);
        session.install_rollback_source(captured);

        let retained = session.audible_source.as_ref().expect("retained B");
        assert_eq!(retained.generation, generation_b);
        assert_eq!(retained.source.as_ref(), "sd bd");
        assert!(retained.mini);
        assert_eq!(session.generation(), generation_d);
        assert_eq!(session.last_source.as_deref(), Some("cp"));
        assert_eq!(
            session_core_settings(&session),
            (RngMode::Legacy, Alignment::Mix, 4)
        );
        assert_eq!(
            session.rollback_to_previous_source(0.4).expect("replay B"),
            RollbackAttempt::Applied
        );
        assert_eq!(active_values(&session), ["sd", "bd"]);
        assert_eq!(
            session_core_settings(&session),
            (RngMode::Precise, Alignment::Out, 2)
        );
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn unconfirmed_audible_prefill_does_not_replace_the_rollback_source() {
        // Distinct compatibility fixtures exercise retained score ownership
        // through the same evaluator used for live source replacements.
        let source_a =
            include_str!("../../tests/e2e/scores/corpus/regressions/begingate-pulse3.strudel");
        let source_b =
            include_str!("../../tests/e2e/scores/corpus/regressions/begingate-pulse4.strudel");
        assert_eq!(
            crate::ui_events::source_revision(source_a),
            "87e752905cf56ee74faa82520fc8ec32de83053ad10d417c325224f2a4f669d6"
        );
        assert_eq!(
            crate::ui_events::source_revision(source_b),
            "db6313ce130abc31c00b3a8b93244a7bddf7a9bbee09a255a392feb5b7d50080"
        );
        check_unconfirmed_prefill_rollback(source_a, source_b, false, false);
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn unconfirmed_silent_prefill_does_not_replace_the_rollback_source() {
        check_unconfirmed_prefill_rollback("~", "~ ~", true, false);
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn delayed_audible_confirmation_selects_its_own_rollback_source() {
        check_unconfirmed_prefill_rollback(
            include_str!("../../tests/e2e/scores/corpus/regressions/begingate-pulse3.strudel"),
            include_str!("../../tests/e2e/scores/corpus/regressions/begingate-pulse4.strudel"),
            false,
            true,
        );
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn delayed_silent_confirmation_selects_its_own_rollback_source() {
        check_unconfirmed_prefill_rollback("~", "~ ~", true, true);
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn captured_direct_pattern_clears_a_later_textual_rollback_target() {
        let mut session = Session::new().expect("session");
        #[cfg(not(feature = "vst"))]
        session.evaluate_mini("bd").expect("source A");
        #[cfg(feature = "vst")]
        {
            let source = r#"s("sine").vst("Delay").orbit(1)"#;
            session.evaluate(source).expect("source A");
            let chain = plugin_chain(&session);
            session
                .reload_at(&source.replace("orbit(1)", "orbit(2)"), false, 0.1)
                .expect("move plugin");
            assert_eq!(plugin_chain(&session), chain);
        }
        // Inject replay policy, not consumer-copy evidence.
        session
            .mark_audible_generation(session.generation())
            .expect("injected replay-policy target");
        let generation_a = session.generation();
        session
            .set_pattern(rustel_core::pure(Value::Str("native".into())))
            .expect("direct Pattern B");
        let captured = session
            .capture_rollback_source(session.generation())
            .expect("capture direct Pattern");
        assert!(captured.is_none());
        assert_eq!(
            session.audible_source.as_ref().unwrap().generation,
            generation_a
        );

        let generation_c = session.reload_at("sd", true, 0.2).expect("source C");
        session
            .mark_audible_generation(generation_c)
            .expect("retain C");
        assert_eq!(
            session.audible_source.as_ref().unwrap().generation,
            generation_c
        );
        session.install_rollback_source(captured);
        assert!(session.audible_source.is_none());
        assert_eq!(session.generation(), generation_c);
        assert_eq!(session.last_source.as_deref(), Some("sd"));
        assert_eq!(
            session.rollback_to_previous_source(0.3).expect("no source"),
            RollbackAttempt::Unavailable
        );
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn captured_identical_sources_keep_distinct_generations() {
        let mut session = Session::new().expect("session");
        session.evaluate_mini("bd").expect("first source");
        let first_generation = session.generation();
        let first = session
            .capture_rollback_source(first_generation)
            .expect("capture first")
            .expect("textual source");
        let second_generation = session.reload_at("bd", true, 0.1).expect("same source");
        let second = session
            .capture_rollback_source(second_generation)
            .expect("capture second")
            .expect("textual source");
        assert_eq!(second_generation, first_generation + 1);
        assert_eq!(first.source, second.source);
        assert_eq!(first.generation, first_generation);
        assert_eq!(second.generation, second_generation);

        session.install_rollback_source(Some(first));
        assert_eq!(
            session.audible_source.as_ref().unwrap().generation,
            first_generation
        );
        session.install_rollback_source(Some(second));
        assert_eq!(
            session.audible_source.as_ref().unwrap().generation,
            second_generation
        );
        assert_eq!(session.generation(), second_generation);
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn capturing_a_stale_generation_preserves_the_rollback_target() {
        let mut session = Session::new().expect("session");
        session.evaluate_mini("bd").expect("source A");
        // Inject replay policy, not consumer-copy evidence.
        session
            .mark_audible_generation(session.generation())
            .expect("injected replay-policy target");
        let generation_a = session.generation();
        let generation_b = session.reload_at("sd", true, 0.1).expect("source B");
        assert!(matches!(
            session.capture_rollback_source(generation_a),
            Err(RuntimeError::Message(_))
        ));
        assert!(session.mark_audible_generation(generation_a).is_err());
        let retained = session.audible_source.as_ref().expect("retained A");
        assert_eq!(retained.generation, generation_a);
        assert_eq!(retained.source.as_ref(), "bd");
        assert_eq!(session.generation(), generation_b);
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn rapid_unpublished_supersession_rolls_back_to_the_last_audible_score() {
        let mut session = Session::new().expect("session");
        session.evaluate("s('bd')").expect("audible A");
        // Inject replay policy, not consumer-copy evidence.
        session
            .mark_audible_generation(session.generation())
            .expect("injected replay-policy target");
        let audible_generation = session.generation();
        session
            .reload_at("s('sd')", false, 0.1)
            .expect("unpublished B");
        session
            .reload_at("s('hh')", false, 0.2)
            .expect("unpublished C");

        let audible = session.audible_source.as_ref().expect("audible snapshot");
        assert_eq!(audible.generation, audible_generation);
        assert_eq!(audible.source.as_ref(), "s('bd')");
        assert_eq!(
            session
                .rollback_to_previous_source(0.3)
                .expect("rollback A"),
            RollbackAttempt::Applied
        );
        assert_eq!(
            session.query(Fraction::ZERO, Fraction::ONE).unwrap()[0]
                .value
                .show(),
            "s:bd"
        );

        match rustel_csound::Program::compile(&[]) {
            Ok(_) => {
                let mut config = SessionConfig::default();
                config.score_import_access.permit_csound();
                let mut session = Session::with_config(config).expect("native session");
                session
                    .evaluate_prebake(
                        "loadCSound`instr probe\naOut init .025\nouts aOut,aOut\nendin`",
                    )
                    .expect("audible definition");
                session
                    .evaluate("note('c4').csound('probe')")
                    .expect("audible reference");
                session
                    .mark_audible_generation(session.generation())
                    .expect("retain audible orchestra");
                let audible = session.csound_snapshot();
                session
                    .reload_at(
                        "loadCSound`instr probe\naOut init .1\nouts aOut,aOut\nendin`\nsilence",
                        false,
                        0.1,
                    )
                    .expect("unconfirmed orchestra");
                assert_ne!(session.csound_snapshot(), audible);
                assert_eq!(
                    session
                        .rollback_to_previous_source(0.2)
                        .expect("restore audible orchestra"),
                    RollbackAttempt::Applied
                );
                assert_eq!(session.csound_snapshot(), audible);
            }
            Err(error)
                if std::env::var_os("RUSTEL_CSOUND_LIBRARY").is_none()
                    && error.contains("Csound 6 is not available") => {}
            Err(error) => panic!("configured native Csound failed: {error}"),
        }

        // The session has a live output, so a chain counts the turns of its
        // producer. No turn runs here: a lane that leaves keeps its chain.
        #[cfg(feature = "vst")]
        {
            let _plugins = fixture_plugins();
            let mut session = Session::new().expect("session");
            let relaxed = std::sync::atomic::Ordering::Relaxed;
            session.plugins_live.store(true, relaxed);
            session.plugin_rate.store(48_000, relaxed);
            let source = r#"$: s("sine").vst("Delay").orbit(1)"#;
            session.evaluate(source).expect("audible plugin source");
            session
                .mark_audible_generation(session.generation())
                .expect("retain playing source");
            let chain = plugin_chain(&session);
            let moved = source.replace("orbit(1)", "orbit(2)");
            session.reload_at(&moved, false, 0.1).expect("move to two");
            assert_eq!(plugin_chain(&session), chain);
            // A cancelled text with a different lane. Then an edit of the
            // lane and a new lane above in one evaluation: the lane keeps
            // its number, and so its chain.
            let moved = format!("$: s(\"hh\")\n{}", source.replace("orbit(1)", "orbit(3)"));
            let other = r#"$: note("a1 e2").lpf(400)"#;
            for cancelled in [other, moved.as_str()] {
                assert!(matches!(
                    session.evaluate_cancellable(cancelled, &AtomicBool::new(true)),
                    Err(RuntimeError::Cancelled)
                ));
                assert_eq!(plugin_chain(&session), chain);
            }
            session
                .reload_at(&moved, false, 0.2)
                .expect("move to three");
            assert_eq!(plugin_note(&session)["__rustel_lane"], "#0");
            assert_eq!(plugin_chain(&session), chain);
            // A score with no plugin lane is not audible too. The rollback
            // gives the plugin lane the number it had.
            session
                .reload_at(other, false, 0.25)
                .expect("no plugin lane");
            assert_eq!(
                session
                    .rollback_to_previous_source(0.3)
                    .expect("rollback orbit"),
                RollbackAttempt::Applied
            );
            assert_eq!(session.active_source(), Some(source));
            assert_eq!(plugin_note(&session)["__rustel_lane"], "#0");
            assert_eq!(plugin_chain(&session), chain);

            // A plugin with no copy ready. An edit waits, and a rollback in
            // the same state plays at once. The commit of the rollback reads
            // a clock past the cover, so it comes last.
            let edit = format!("{source}\n$: note(\"c3\").s(\"sine\").vst(\"rustel fixture\")");
            let reserve = Duration::from_millis(1);
            let stopped = AtomicBool::new(false);
            session.schedule_through(0.3, 0.3).expect("cover");
            let clock = past_the_cover(0.35);
            let attempt =
                session.evaluate_live_score_cancellable(&edit, false, reserve, &stopped, clock);
            assert_eq!(attempt.expect("edit"), LiveScoreAttempt::Waiting);
            assert_eq!(session.active_source(), Some(source));
            session.cancel_plugin_hold();
            session.reload_at(&edit, false, 0.4).expect("plugin edit");
            session
                .mark_audible_generation(session.generation())
                .expect("retain the plugin edit");
            session
                .reload_at(other, false, 0.45)
                .expect("no plugin lane");
            session.schedule_through(0.45, 0.45).expect("cover");
            let rollback = session.rollback_to_previous_source_with_clock(past_the_cover(0.5));
            assert_eq!(rollback.expect("rollback"), RollbackAttempt::Applied);
            assert_eq!(session.active_source(), Some(edit.as_str()));
            assert!(!session.holds_edit(&edit));
        }
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn an_injected_intermediate_target_becomes_the_next_replay_source() {
        let mut session = Session::new().expect("session");
        session.evaluate("s('bd')").expect("audible A");
        let generation_b = session
            .reload_at("s('sd')", false, 0.1)
            .expect("candidate B");
        // Inject replay policy, not consumer-copy evidence.
        session
            .mark_audible_generation(generation_b)
            .expect("injected replay-policy target");
        session
            .reload_at("s('hh')", false, 0.2)
            .expect("candidate C");

        assert_eq!(
            session
                .rollback_to_previous_source(0.3)
                .expect("rollback B"),
            RollbackAttempt::Applied
        );
        assert_eq!(
            session.query(Fraction::ZERO, Fraction::ONE).unwrap()[0]
                .value
                .show(),
            "s:sd"
        );
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn arbitrarily_many_unpublished_candidates_keep_one_bounded_audible_snapshot() {
        let mut session = Session::new().expect("session");
        session.evaluate_mini("bd").expect("audible score");
        // Inject replay policy, not consumer-copy evidence.
        session
            .mark_audible_generation(session.generation())
            .expect("injected replay-policy target");
        let audible_generation = session.generation();
        for index in 0..1_000 {
            let source = if index % 2 == 0 { "sd" } else { "hh" };
            session
                .reload_at(source, true, 0.001 * f64::from(index))
                .expect("superseding mini candidate");
        }
        let audible = session.audible_source.as_ref().expect("audible snapshot");
        assert_eq!(audible.generation, audible_generation);
        assert_eq!(audible.source.as_ref(), "bd");
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn an_audible_direct_pattern_clears_textual_rollback_and_remains_last_good() {
        let mut session = Session::new().expect("session");
        session.evaluate("pure('old-js')").expect("old source");
        // Inject replay policy, not consumer-copy evidence.
        session
            .mark_audible_generation(session.generation())
            .expect("injected replay-policy target");
        assert_eq!(
            session.audible_source.as_ref().unwrap().generation,
            session.generation()
        );
        session
            .set_pattern(rustel_core::pure(Value::Str("native".into())))
            .expect("direct pattern");
        let generation = session.generation();
        session
            .mark_audible_generation(generation)
            .expect("native cutover");
        assert!(
            session.audible_source.is_none(),
            "a direct Pattern retained an unrelated textual rollback target"
        );

        let error = session
            .reload_at(
                "pure('candidate').fmap(() => { throw new Error('boom'); })",
                false,
                0.25,
            )
            .expect_err("the query-throwing candidate must be rejected");
        assert!(error.to_string().contains("last-good score kept"));
        assert_eq!(session.generation(), generation);
        let haps = session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("native query");
        assert_eq!(haps.len(), 1);
        assert_eq!(haps[0].value, Value::Str("native".into()));
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn takeover_freshness_rejects_converted_native_events_before_publication() {
        for expired in [false, true] {
            let mut session = Session::new().expect("session");
            session.set_continuity_margin(0.125);
            session.evaluate_mini("~").expect("previous source");
            // Inject replay policy; the live producer test separately proves
            // retention of an actually consumer-confirmed silent score.
            let original = session.generation();
            session
                .mark_audible_generation(original)
                .expect("replay target");
            let voice = rustel_core::pure(Value::object(vec![
                ("s".into(), Value::Str("sine".into())),
                ("note".into(), Value::F64(48.0)),
                ("gain".into(), Value::F64(0.05)),
            ]))
            .fast(8.into());
            session
                .set_pattern_at(voice, 0.0, false)
                .expect("native voice");
            session.requery_active_at(0.0).expect("explicit takeover");
            let candidate = session.generation();
            let mut producer =
                crate::LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
            producer.arm_replacement(original, candidate);
            let mut clocks = [0.0, if expired { 0.25 } else { 0.0 }, 0.5].into_iter();
            let mut published = Vec::new();
            let mut events = Vec::new();
            let result = producer.step_unwatched_with_clock_and_cutover(
                &mut session,
                || clocks.next().unwrap_or(0.5),
                48_000,
                |generation, frame, _cut| published.push((generation, frame)),
                |event| {
                    events.push(event);
                    true
                },
            );
            if expired {
                let error = result.expect_err("expired converted window");
                assert!(error.to_string().contains("takeover"), "{error}");
                assert!(published.is_empty());
                assert!(
                    events.is_empty(),
                    "converted events escaped the refused window"
                );
                assert_eq!(session.active_source(), Some("~"));
            } else {
                result.expect("fresh converted window");
                assert_eq!(published, [(candidate, 6_000)]);
                assert!(!events.is_empty(), "control must convert real voices");
                assert!(
                    events
                        .iter()
                        .all(|event| event.gain > 0.0 && event.freq_hz > 0.0)
                );
            }
        }
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn takeover_freshness_preserves_a_loading_identity_until_progress() {
        let mut session = Session::new().expect("session");
        session.set_continuity_margin(0.125);
        let library = Arc::new(crate::samples::SampleLibrary::with_loading_sample_for_test(
            "held",
        ));
        session.samples = Some(Arc::clone(&library));
        session
            .set_pattern(
                rustel_core::pure(Value::object(vec![("s".into(), Value::Str("held".into()))]))
                    .fast(8.into()),
            )
            .expect("native sample graph");
        let before = session.generation();
        session.requery_active_at(0.0).expect("explicit takeover");
        let candidate = session.generation();
        let mut producer =
            crate::LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        producer.arm_replacement(before, candidate);
        let held = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("Loading must not publish"),
                |_| true,
            )
            .expect("held sample waits as a quiet turn");
        assert_eq!((held.scheduled, held.pushed, held.pending), (0, 0, 0));
        assert_eq!(session.generation(), candidate);
        library.finish_loading_sample_for_test();
        let mut published = Vec::new();
        let mut events = Vec::new();
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 1.0,
                48_000,
                |generation, frame, _cut| published.push((generation, frame)),
                |event| {
                    events.push(event);
                    true
                },
            )
            .expect("Loading keeps its existing later-progress policy");
        assert_eq!(published, [(candidate, 6_000)]);
        assert!(!events.is_empty());
    }

    /// Rollback preserves the language route used by the previous score.
    #[cfg(feature = "device-audio")]
    #[test]
    fn a_rollback_reinstalls_a_mini_score_through_the_mini_door() {
        let mut session = Session::new().expect("session");
        session
            .evaluate_mini("bd sd")
            .expect("install the mini score");
        // Inject replay policy, not consumer-copy evidence.
        session
            .mark_audible_generation(session.generation())
            .expect("injected replay-policy target");
        // The score the watchdog would be rolling back FROM.
        session
            .evaluate(r#"s("hh*4")"#)
            .expect("install a js score");

        assert_eq!(
            session
                .rollback_to_previous_source(0.0)
                .expect("the rollback must not error"),
            RollbackAttempt::Applied
        );

        let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
        let values: Vec<String> = haps.iter().map(|hap| hap.value.show()).collect();
        assert_eq!(
            values,
            vec!["bd".to_string(), "sd".to_string()],
            "the rollback did not restore the mini score"
        );
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn javascript_rollback_restores_the_settings_inherited_by_the_old_score() {
        let mut session = Session::new().expect("session");
        session.set_default_voicings(Some("guidetones"));
        let inherited = "pure('C7').fmap(x => rustelScope.voicing(pure(x))).innerJoin()";
        session
            .evaluate(inherited)
            .expect("install inherited score");
        // Inject replay policy, not consumer-copy evidence.
        session
            .mark_audible_generation(session.generation())
            .expect("injected replay-policy target");
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query inherited score")
                .len(),
            2
        );
        session
            .evaluate(&format!("setDefaultVoicings('lefthand'); {inherited}"))
            .expect("install replacement");
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query replacement")
                .len(),
            4
        );

        assert_eq!(
            session
                .rollback_to_previous_source(0.0)
                .expect("rollback old JavaScript score"),
            RollbackAttempt::Applied
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query rolled-back score")
                .len(),
            2,
            "rollback evaluated the old score under the replacement settings"
        );
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn mini_rollback_republishes_the_settings_inherited_by_the_old_score() {
        let mut session = Session::new().expect("session");
        session.set_default_voicings(Some("guidetones"));
        session.evaluate_mini("bd sd").expect("install mini score");
        // Inject replay policy, not consumer-copy evidence.
        session
            .mark_audible_generation(session.generation())
            .expect("injected replay-policy target");
        session
            .evaluate("setDefaultVoicings('lefthand'); s('hh*4')")
            .expect("install replacement");
        assert_eq!(session_core_settings(&session).2, 4);

        assert_eq!(
            session
                .rollback_to_previous_source(0.0)
                .expect("rollback old mini score"),
            RollbackAttempt::Applied
        );
        assert_eq!(
            session_core_settings(&session).2,
            2,
            "mini rollback left the replacement settings published"
        );
        let values: Vec<String> = session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query rolled-back mini score")
            .into_iter()
            .map(|hap| hap.value.show())
            .collect();
        assert_eq!(values, ["bd", "sd"]);
    }

    /// A deferred rollback retains its target for a later recovery attempt.
    #[cfg(feature = "device-audio")]
    #[test]
    fn a_deferred_rollback_keeps_its_target_for_the_next_attempt() {
        let mut session = Session::new().expect("session");
        // Both scores go through the JavaScript door. An explicit mini file
        // never reaches the budget gate: it does not enter QuickJS, so it has
        // no CPU-bound claim to defer on.
        session
            .evaluate(r#"s("bd sd").gain(0.9)"#)
            .expect("install a js score");
        // Inject replay policy, not consumer-copy evidence.
        session
            .mark_audible_generation(session.generation())
            .expect("injected replay-policy target");
        session
            .evaluate(r#"s("hh*4").gain(0.9)"#)
            .expect("install a second js score");
        assert_eq!(
            session.last_path,
            EvaluateSource::JavaScript,
            "the fixture scores did not go through the JavaScript door"
        );
        session.play(2.0).expect("fill the horizon");

        // Just inside the end of the filled horizon: there is cover left, so
        // the gate defers rather than spending a recovery slice, but far too
        // little of it to afford the evaluation floor.
        let scheduled_to = session.scheduler.horizon_remaining(0.0);
        assert!(scheduled_to > 0.1, "the horizon never filled");
        let starved_now = scheduled_to - 0.005;
        assert_eq!(
            session
                .rollback_to_previous_source(starved_now)
                .expect("a deferred rollback is not an error"),
            RollbackAttempt::Deferred
        );

        // The target survived, so the retry that follows can still rescue the
        // set. Past the horizon there is no cover left to protect and the gate
        // spends a recovery slice instead of deferring.
        assert_eq!(
            session
                .rollback_to_previous_source(scheduled_to + 1.0)
                .expect("the retry must not error"),
            RollbackAttempt::Applied
        );
        let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
        let values: Vec<String> = haps.iter().map(|hap| hap.value.show()).collect();
        assert_eq!(
            values.len(),
            2,
            "the retry did not restore the two-onset score: {values:?}"
        );
    }

    #[test]
    fn a_stopped_session_recovers_after_start() {
        // Stop must not be terminal: the runtime has to schedule again
        // afterwards, or "cancellation" is indistinguishable from breakage.
        let mut session = Session::new().expect("session");
        session.evaluate(r#"s("bd sd")"#).expect("evaluate");
        let handle = session.transport();
        handle.stop();
        handle.start();
        assert!(
            !session.play(2.0).expect("play").onsets.is_empty(),
            "the session did not recover after a stop/start cycle"
        );
    }
}
