//! Native unwind containment on producer threads.

use std::panic::{AssertUnwindSafe, catch_unwind};

use super::*;

#[cfg(test)]
mod tests {
    use super::*;

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
            let transport = session.transport();
            let generation = session.generation();
            let cycle = session.cycle_at_time(0.5);
            session.inject_panic_for_test(point);
            let error = session
                .reload_at(CANDIDATE, false, 0.5)
                .expect_err("candidate panics");
            assert_panic(error, &format!("{point:?}"));
            assert_eq!(session.active_source(), Some(GOOD));
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
            let source = format!("{GOOD}.vst('Delay').orbit(1)");
            session.evaluate(&source).expect("plugin source");
            let moved = source.replace("orbit(1)", "orbit(2)");
            session.reload_at(&moved, false, 0.1).expect("move plugin");
            assert_eq!(session.insert_orbits[2], 1);
            moved
        };
        let transport = session.transport();
        let generation = session.generation();
        session.inject_panic_for_test(SessionPanicPoint::Query);
        let error = session
            .schedule_through(0.25, 0.75)
            .expect_err("scheduler query panics");
        assert_panic(error, "Query");
        assert_eq!(session.active_source(), Some(source.as_str()));
        #[cfg(feature = "vst")]
        assert_eq!(session.insert_orbits[2], 1);
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
            .permit_origin("https://example.invalid")
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
}

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
    settings: RuntimeSettings,
    cps: f64,
    cycle: f64,
    query_hap_budget: u64,
    trace_enabled: bool,
    js_memory_limit: usize,
    #[cfg(feature = "vst")]
    insert_orbits: [u8; rustel_audio::MAX_ORBITS],
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
        #[cfg(feature = "vst")]
        let insert_orbits = self.insert_orbits;
        #[cfg(all(feature = "vst", feature = "device-audio"))]
        let insert_orbits = self
            .audible_source
            .as_ref()
            .map_or(insert_orbits, |source| source.insert_orbits);
        RecoveryCheckpoint {
            source,
            #[cfg(feature = "vst")]
            insert_orbits,
            settings,
            prebakes: self.recovery_prebakes.clone(),
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
        #[cfg(feature = "vst")]
        {
            fresh.insert_orbits = checkpoint.insert_orbits;
        }
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
        fresh.direct_diagnostic_logging = self.direct_diagnostic_logging;
        fresh.audio_input_channels = self.audio_input_channels;
        fresh.schedule_lead = self.schedule_lead;
        fresh.continuity_margin = self.continuity_margin;
        // The replayed score takes over on a frame of the same device.
        fresh.live_sample_rate = self.live_sample_rate;
        fresh.stop_when_silent = self.stop_when_silent;
        fresh.export_limiter = self.export_limiter;
        fresh.render_tail_secs = self.render_tail_secs;
        fresh.pending_diagnostics = std::mem::take(&mut self.pending_diagnostics);
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
                    cps: checkpoint.cps,
                    cycle_zero_time: now - checkpoint.cycle / checkpoint.cps,
                    #[cfg(feature = "vst")]
                    insert_orbits: checkpoint.insert_orbits,
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
