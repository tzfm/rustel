use std::fs;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rustel_fraction::Fraction;
use rustel_runtime::TakeoverCut;
use rustel_runtime::{
    FileWatch, MAX_WATCH_SOURCE_BYTES, ReloadStatus, Session, WatchLanguage, WatchPoll, WatchTarget,
};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("rustel-watch-{}-{id}", std::process::id()));
        fs::create_dir(&path).expect("create watch fixture directory");
        Self(path)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write(path: &Path, source: &str) {
    fs::write(path, source).expect("write watch fixture");
}

fn shown(session: &Session) -> String {
    let haps = session
        .query(Fraction::ZERO, Fraction::ONE)
        .expect("query active watch graph");
    assert_eq!(haps.len(), 1, "watch fixture should produce one hap");
    haps[0].value.show()
}

fn installed(poll: WatchPoll) -> u64 {
    let WatchPoll::Event(event) = poll else {
        panic!("expected installed event, got {poll:?}");
    };
    assert_eq!(event.status, ReloadStatus::Installed, "{event:?}");
    assert_eq!(event.generation_after, event.generation_before + 1);
    event.generation_after
}

fn rejected(poll: WatchPoll) {
    let WatchPoll::Event(event) = poll else {
        panic!("expected rejected event, got {poll:?}");
    };
    assert_eq!(event.status, ReloadStatus::Rejected, "{event:?}");
    assert_eq!(event.generation_after, event.generation_before);
    assert!(event.error_kind.is_some(), "rejection has no error kind");
}

#[test]
fn loaded_source_baseline_does_not_swallow_a_save_before_watcher_construction() {
    const DEBOUNCE: Duration = Duration::from_millis(20);
    let temp = TempDir::new();
    let score = temp.join("baseline-race.strudel");
    let loaded = "pure('loaded')";
    let saved = "pure('saved-before-watch')";
    write(&score, loaded);
    let mut session = Session::new().expect("session");
    session.evaluate(loaded).expect("evaluate loaded identity");

    // The save lands after evaluation but before watcher construction.
    write(&score, saved);
    let mut watch = FileWatch::from_loaded_source(
        &score,
        WatchTarget::Score(WatchLanguage::JavaScript),
        loaded,
        DEBOUNCE,
    );
    assert_eq!(
        watch.poll(&mut session, Duration::ZERO),
        WatchPoll::Pending,
        "the constructor reread disk and swallowed the intervening save"
    );
    let WatchPoll::Event(event) = watch.poll(&mut session, DEBOUNCE) else {
        panic!("intervening save did not settle");
    };
    assert_eq!(event.target, WatchTarget::Score(WatchLanguage::JavaScript));
    assert_eq!(event.status, ReloadStatus::Installed);
    assert_eq!(shown(&session), "saved-before-watch");
}

#[test]
fn prebake_watch_mutates_the_heap_without_replacing_the_active_generation() {
    const DEBOUNCE: Duration = Duration::from_millis(20);
    let temp = TempDir::new();
    let prebake = temp.join("prebake.js");
    let initial = "globalThis.watchHelper = () => note('c4');";
    write(&prebake, initial);
    let mut session = Session::new().expect("session");
    session.evaluate_prebake(initial).expect("initial prebake");
    session.evaluate("watchHelper()").expect("initial score");
    let generation = session.generation();
    assert!(shown(&session).contains("c4"));
    let mut watch =
        FileWatch::from_loaded_source(&prebake, WatchTarget::Prebake, initial, DEBOUNCE);

    write(&prebake, "globalThis.watchHelper = () => note('e4');");
    assert_eq!(
        watch.poll(&mut session, Duration::from_millis(1)),
        WatchPoll::Pending
    );
    let WatchPoll::Event(event) = watch.poll(&mut session, Duration::from_millis(21)) else {
        panic!("prebake edit did not settle");
    };
    assert_eq!(event.target, WatchTarget::Prebake);
    assert_eq!(event.status, ReloadStatus::Installed);
    assert_eq!(event.generation_before, generation);
    assert_eq!(event.generation_after, generation);
    assert_eq!(session.generation(), generation);
    assert!(
        shown(&session).contains("c4"),
        "a prebake-only save rebuilt the current graph"
    );
    session
        .reload_at("watchHelper()", false, 0.1)
        .expect("score rebuild after setup edit");
    assert!(shown(&session).contains("e4"));

    let stable_generation = session.generation();
    write(
        &prebake,
        "globalThis.partialPrebake = 9; throw new Error('bad watched setup');",
    );
    assert_eq!(
        watch.poll(&mut session, Duration::from_millis(30)),
        WatchPoll::Pending
    );
    let WatchPoll::Event(rejected) = watch.poll(&mut session, Duration::from_millis(50)) else {
        panic!("failed prebake edit did not settle");
    };
    assert_eq!(rejected.target, WatchTarget::Prebake);
    assert_eq!(rejected.status, ReloadStatus::Rejected);
    assert_eq!(rejected.generation_before, stable_generation);
    assert_eq!(rejected.generation_after, stable_generation);
    assert_eq!(
        watch.poll(&mut session, Duration::from_millis(60)),
        WatchPoll::Unchanged,
        "one failed setup identity was evaluated repeatedly"
    );
    session
        .reload_at("pure(partialPrebake)", false, 0.2)
        .expect("completed side effect after watched throw");
    assert_eq!(shown(&session), "9");
}

#[test]
fn stop_interrupts_a_hostile_watched_prebake_promptly() {
    const DEBOUNCE: Duration = Duration::from_millis(20);
    let temp = TempDir::new();
    let prebake = temp.join("cancel-prebake.js");
    let initial = "globalThis.cancelControl = 1;";
    write(&prebake, initial);
    let mut session = Session::new().expect("session");
    session.evaluate("note('c4')").expect("initial score");
    session.evaluate_prebake(initial).expect("initial prebake");
    let generation = session.generation();
    let mut watch =
        FileWatch::from_loaded_source(&prebake, WatchTarget::Prebake, initial, DEBOUNCE);

    write(&prebake, "while (true) {}");
    assert_eq!(
        watch.poll(&mut session, Duration::from_millis(1)),
        WatchPoll::Pending
    );
    let transport = session.transport();
    let stopper = {
        let transport = transport.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(25));
            transport.stop();
        })
    };
    let started = std::time::Instant::now();
    let WatchPoll::Event(event) = watch.poll(&mut session, Duration::from_millis(21)) else {
        panic!("hostile setup did not return a rejection");
    };
    stopper.join().expect("Stop thread");
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "watched setup waited for the two-second CPU deadline: {:?}",
        started.elapsed()
    );
    assert_eq!(event.target, WatchTarget::Prebake);
    assert_eq!(event.status, ReloadStatus::Rejected);
    assert_eq!(event.error_kind.as_deref(), Some("cancelled"));
    assert_eq!(event.generation_before, generation);
    assert_eq!(event.generation_after, generation);
    assert_eq!(session.generation(), generation);

    transport.start();
    let current = session
        .query(Fraction::ZERO, Fraction::ONE)
        .expect("active query after watched cancellation");
    assert!(
        !current.is_empty(),
        "watched cancellation silenced the active graph"
    );
    assert!(
        current.iter().all(|hap| hap.value.show().contains("c4")),
        "watched cancellation replaced the active graph: {current:?}"
    );
}

#[test]
fn stop_interrupts_a_runaway_watched_score_promptly() {
    const DEBOUNCE: Duration = Duration::from_millis(20);
    let temp = TempDir::new();
    let score = temp.join("cancel-score.strudel");
    let initial = "note('c4')";
    write(&score, initial);
    let mut session = Session::new().expect("session");
    session.evaluate(initial).expect("initial score");
    let generation = session.generation();
    let mut watch = FileWatch::from_loaded_source(
        &score,
        WatchTarget::Score(WatchLanguage::JavaScript),
        initial,
        DEBOUNCE,
    );

    write(&score, "while (true) {}");
    assert_eq!(
        watch.poll(&mut session, Duration::from_millis(1)),
        WatchPoll::Pending
    );
    let transport = session.transport();
    let stopper = {
        let transport = transport.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(25));
            transport.stop();
        })
    };
    let started = std::time::Instant::now();
    let WatchPoll::Event(event) = watch.poll(&mut session, Duration::from_millis(21)) else {
        panic!("runaway score did not return a rejection");
    };
    stopper.join().expect("Stop thread");
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "watched score waited for the two-second CPU deadline: {:?}",
        started.elapsed()
    );
    assert_eq!(event.target, WatchTarget::Score(WatchLanguage::JavaScript));
    assert_eq!(event.status, ReloadStatus::Rejected);
    assert_eq!(event.error_kind.as_deref(), Some("cancelled"));
    assert_eq!(event.generation_before, generation);
    assert_eq!(event.generation_after, generation);
    assert_eq!(session.generation(), generation);

    transport.start();
    assert!(shown(&session).contains("c4"));
}

#[test]
fn stable_saves_install_once_and_failures_keep_last_known_good() {
    const DEBOUNCE: Duration = Duration::from_millis(50);
    let temp = TempDir::new();
    let score = temp.join("song.strudel");
    let initial = r#"
globalThis.reloadCount = (globalThis.reloadCount ?? 0) + 1;
pure(globalThis.reloadCount)
"#;
    write(&score, initial);

    let mut session = Session::new().expect("session");
    session.evaluate(initial).expect("initial evaluation");
    let initial_generation = session.generation();
    assert_eq!(shown(&session), "1");
    let mut watch = FileWatch::new(&score, WatchLanguage::JavaScript, DEBOUNCE);
    assert_eq!(
        watch.poll(&mut session, Duration::ZERO),
        WatchPoll::Unchanged,
        "starting the watcher must not reinstall the loaded score"
    );

    // A stable partial write is diagnosed once, without changing generation or
    // active output. Re-polling the same broken identity does not spin eval.
    write(&score, "pure(");
    assert_eq!(
        watch.poll(&mut session, Duration::from_millis(10)),
        WatchPoll::Pending
    );
    rejected(watch.poll(&mut session, Duration::from_millis(70)));
    assert_eq!(session.generation(), initial_generation);
    assert_eq!(shown(&session), "1");
    assert_eq!(
        watch.poll(&mut session, Duration::from_millis(80)),
        WatchPoll::Unchanged
    );

    let next = r#"
globalThis.reloadCount = (globalThis.reloadCount ?? 0) + 1;
pure(globalThis.reloadCount)
"#;
    write(&score, next);
    assert_eq!(
        watch.poll(&mut session, Duration::from_millis(100)),
        WatchPoll::Pending
    );
    let next_generation = installed(watch.poll(&mut session, Duration::from_millis(160)));
    assert_eq!(next_generation, initial_generation + 1);
    assert_eq!(shown(&session), "2", "the same JS heap must survive");
    assert_eq!(
        watch.poll(&mut session, Duration::from_millis(170)),
        WatchPoll::Unchanged,
        "one stable save installed more than one generation"
    );

    // A stable deletion is a reported read failure, never silence.
    fs::remove_file(&score).expect("remove watched score");
    assert_eq!(
        watch.poll(&mut session, Duration::from_millis(200)),
        WatchPoll::Pending
    );
    rejected(watch.poll(&mut session, Duration::from_millis(260)));
    assert_eq!(session.generation(), next_generation);
    assert_eq!(shown(&session), "2");
}

#[test]
fn atomic_save_burst_is_coalesced_and_stop_is_never_cleared() {
    const DEBOUNCE: Duration = Duration::from_millis(50);
    let temp = TempDir::new();
    let score = temp.join("song.strudel");
    let replacement = temp.join("song.strudel.tmp");
    write(&score, "pure('old')");
    let mut session = Session::new().expect("session");
    session.evaluate("pure('old')").expect("initial");
    let mut watch = FileWatch::new(&score, WatchLanguage::JavaScript, DEBOUNCE);
    let before = session.generation();

    fs::remove_file(&score).expect("atomic-save remove phase");
    assert_eq!(
        watch.poll(&mut session, Duration::from_millis(10)),
        WatchPoll::Pending
    );
    write(&replacement, "pure('new')");
    fs::rename(&replacement, &score).expect("atomic-save rename phase");
    assert_eq!(
        watch.poll(&mut session, Duration::from_millis(20)),
        WatchPoll::Pending,
        "the create/rename identity needs its own stable window"
    );

    session.transport().stop();
    assert_eq!(
        watch.poll(&mut session, Duration::from_millis(80)),
        WatchPoll::Stopped
    );
    assert_eq!(session.generation(), before);
    assert!(session.transport().is_stopped(), "watch cleared Stop");

    session.transport().start();
    let generation = installed(watch.poll(&mut session, Duration::from_millis(81)));
    assert_eq!(generation, before + 1);
    assert_eq!(shown(&session), "new");
    assert_eq!(
        watch.poll(&mut session, Duration::from_millis(90)),
        WatchPoll::Unchanged
    );
}

#[test]
fn live_scheduler_replacement_never_delivers_the_old_generation() {
    let mut session = Session::new().expect("session");
    session
        .evaluate("pure('old').fast(4)")
        .expect("initial evaluation");
    let old_generation = session.generation();
    let old = session.schedule_at(0.0).expect("initial live tick");
    assert!(!old.is_empty(), "initial generation scheduled nothing");
    assert!(old.iter().all(|event| event.generation == old_generation));
    assert!(old.iter().all(|event| event.value_show == "old"));

    let new_generation = session
        .reload_at("pure('new').fast(4)", false, 0.1)
        .expect("reload");
    assert_eq!(new_generation, old_generation + 1);
    // Reloads keep the timeline fixed: the replacement's first event lands on
    // the next fast(4) sub-beat at 0.5 s, not at the reload instant. Queue at
    // the reload clock, then drain at the beat.
    let queued = session.schedule_at(0.1).expect("replacement live tick");
    let mut new = queued;
    new.extend(session.schedule_at(0.5).expect("drain at the next beat"));
    assert!(!new.is_empty(), "replacement generation scheduled nothing");
    assert!(new.iter().all(|event| event.generation == new_generation));
    assert!(new.iter().all(|event| event.value_show == "new"));

    let error = session.reload_at("pure(", false, 0.2).unwrap_err();
    assert_eq!(error.kind(), "evaluation");
    assert_eq!(session.generation(), new_generation);
    // Grid beats land every 0.5 s here; the previous drain consumed 0.5 s,
    // so the next last-known-good event is the 1.0 s beat.
    let after_failure = session.schedule_at(1.0).expect("tick after failure");
    assert!(!after_failure.is_empty(), "last-known-good stopped playing");
    assert!(
        after_failure
            .iter()
            .all(|event| event.generation == new_generation && event.value_show == "new"),
        "failed reload leaked old or partial events: {after_failure:?}"
    );
}

#[test]
fn direct_reload_does_not_restart_a_stopped_transport() {
    let mut session = Session::new().expect("session");
    session.evaluate("pure('old')").expect("initial");
    session.transport().stop();
    session
        .reload_at("pure('new')", false, 0.25)
        .expect("reload while stopped");
    assert!(
        session.transport().is_stopped(),
        "reload_at cleared the independent Stop path"
    );
    assert_eq!(session.schedule_at(0.25).unwrap_err().kind(), "cancelled");
}

#[test]
fn watched_source_size_is_bounded_before_evaluation() {
    const DEBOUNCE: Duration = Duration::from_millis(10);
    let temp = TempDir::new();
    let score = temp.join("song.strudel");
    write(&score, "pure('old')");
    let mut session = Session::new().expect("session");
    session.evaluate("pure('old')").expect("initial");
    let generation = session.generation();
    let mut watch = FileWatch::new(&score, WatchLanguage::JavaScript, DEBOUNCE);

    let file = File::create(&score).expect("replace score with sparse oversized file");
    file.set_len(MAX_WATCH_SOURCE_BYTES + 1)
        .expect("size oversized fixture");
    assert_eq!(
        watch.poll(&mut session, Duration::from_millis(1)),
        WatchPoll::Pending
    );
    let WatchPoll::Event(event) = watch.poll(&mut session, Duration::from_millis(20)) else {
        panic!("oversized stable identity was not reported");
    };
    assert_eq!(event.status, ReloadStatus::Rejected);
    assert_eq!(event.error_kind.as_deref(), Some("resource-limit"));
    assert_eq!(session.generation(), generation);
    assert_eq!(shown(&session), "old");
}

#[test]
fn live_scheduler_installs_the_callback_host_only_when_needed() {
    let mut impure = Session::new().expect("impure session");
    impure
        .evaluate(r#"note("c e g").every(fastcat(2, 3), x => x.fast(2))"#)
        .expect("impure evaluation");
    assert!(impure.active_needs_host());
    assert!(
        !impure
            .schedule_at(0.0)
            .expect("impure live tick")
            .is_empty(),
        "callback-bearing live tick became silence"
    );

    let mut pure = Session::new().expect("pure session");
    pure.evaluate(r#"s("bd sd").fast(2)"#)
        .expect("pure evaluation");
    assert!(!pure.active_needs_host());
    assert!(
        !pure.schedule_at(0.0).expect("pure live tick").is_empty(),
        "pure live tick scheduled nothing"
    );
}

#[test]
fn live_transfer_prefills_only_the_configured_horizon() {
    let mut session = Session::new().expect("session");
    session
        .evaluate("pure('tick').fast(16)")
        .expect("evaluation");
    let events = session
        .schedule_through(0.0, session.config().horizon)
        .expect("lookahead transfer");
    assert!(events.len() > 1, "lookahead did not prefill future events");
    assert!(
        events
            .iter()
            .all(|event| event.target_time <= session.config().horizon),
        "lookahead crossed its deadline: {events:?}"
    );
    assert!(
        session
            .schedule_through(0.0, session.config().horizon + 0.001)
            .is_err(),
        "caller could drain past the configured horizon"
    );
}

#[test]
fn debounce_wall_time_does_not_become_the_live_scheduler_clock() {
    const DEBOUNCE: Duration = Duration::from_millis(50);
    let temp = TempDir::new();
    let score = temp.join("song.strudel");
    write(&score, "pure('old').fast(4)");
    let mut session = Session::new().expect("session");
    session
        .evaluate("pure('old').fast(4)")
        .expect("initial evaluation");
    let mut watch = FileWatch::new(&score, WatchLanguage::JavaScript, DEBOUNCE);

    write(&score, "pure('new').fast(4)");
    assert_eq!(
        watch.poll_at(&mut session, Duration::from_secs(70), 0.25),
        WatchPoll::Pending
    );
    installed(watch.poll_at(&mut session, Duration::from_secs(71), 0.25));

    // Live reloads keep the timeline: the replacement's first event is the
    // next grid beat (0.5 s), so drain up to it after queueing at the reload
    // clock. The assertion below is the test's real point: none of the 70 s
    // wall-clock debounce may leak into scheduler time.
    let mut events = session
        .schedule_at(0.25)
        .expect("schedule replacement on sample clock");
    events.extend(session.schedule_at(0.6).expect("drain the first grid beat"));
    assert!(!events.is_empty());
    assert!(
        events
            .iter()
            .all(|event| event.target_time < 1.0 && event.value_show == "new"),
        "wall-clock debounce leaked into scheduler time: {events:?}"
    );
}

#[test]
#[cfg(feature = "device-audio")]
fn live_scheduler_emits_generation_tagged_pod_audio_through_lookahead() {
    const SAMPLE_RATE: u32 = 48_000;
    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"note("c4 e4").fast(4)"#)
        .expect("initial evaluation");
    let generation = session.generation();
    let events = session
        .schedule_audio_at(0.0, SAMPLE_RATE)
        .expect("schedule POD audio");
    assert!(events.len() > 1, "lookahead produced no useful audio batch");
    assert!(events.iter().all(|event| {
        event.generation == generation
            && event.target_frame <= (session.config().horizon * f64::from(SAMPLE_RATE)) as u64
            && event.freq_hz.is_finite()
            && event.freq_hz > 0.0
    }));

    let next_generation = session
        .reload_at(r#"note("g4").fast(4)"#, false, 0.25)
        .expect("reload audio pattern");
    let replacement = session
        .schedule_audio_at(0.25, SAMPLE_RATE)
        .expect("schedule replacement POD audio");
    assert!(!replacement.is_empty());
    assert!(
        replacement
            .iter()
            .all(|event| event.generation == next_generation),
        "old generation entered replacement batch: {replacement:?}"
    );
}

#[test]
#[cfg(feature = "device-audio")]
fn continuous_unwatched_playback_never_applies_file_changes() {
    use rustel_runtime::LiveFileProducer;

    let temp = TempDir::new();
    let score = temp.join("unwatched.strudel");
    let prebake = temp.join("unwatched-prebake.js");
    let setup = "globalThis.unwatchedHelper = () => note('c4').fast(8);";
    let source = "unwatchedHelper()";
    write(&prebake, setup);
    write(&score, source);
    let mut session = Session::new().expect("session");
    session.evaluate_prebake(setup).expect("initial setup");
    session.evaluate(source).expect("initial score");
    let generation = session.generation();
    let mut producer = LiveFileProducer::from_loaded_sources(
        &score,
        WatchLanguage::JavaScript,
        source,
        Some((prebake.clone(), setup.to_string())),
        Duration::from_millis(1),
        Duration::from_millis(1),
    )
    .expect("continuous producer");
    let mut pushed = Vec::new();
    producer
        .step_unwatched(&mut session, 0.0, 48_000, |event| {
            pushed.push(event);
            true
        })
        .expect("initial continuous step");

    write(&score, r#"note("g4").fast(8)"#);
    write(
        &prebake,
        "globalThis.unwatchedHelper = () => note('e4').fast(8);",
    );
    for now in [1.0, 1.1] {
        producer
            .step_unwatched(&mut session, now, 48_000, |event| {
                pushed.push(event);
                true
            })
            .expect("continuous step after file change");
    }

    assert_eq!(
        session.generation(),
        generation,
        "plain playback silently became watch mode"
    );
    let current = session
        .query(Fraction::ZERO, Fraction::ONE)
        .expect("query fixed live graph");
    assert!(
        current.iter().all(|hap| hap.value.show().contains("c4")),
        "unwatched playback applied the changed file: {current:?}"
    );
    assert!(
        pushed.iter().all(|event| event.generation == generation),
        "unwatched playback emitted a replacement generation"
    );
    session
        .reload_at("unwatchedHelper()", false, 1.2)
        .expect("manual score rebuild after unwatched steps");
    let helper = session
        .query(Fraction::ZERO, Fraction::ONE)
        .expect("query helper after unwatched steps");
    assert!(!helper.is_empty(), "unwatched helper became silence");
    assert!(
        helper.iter().all(|hap| hap.value.show().contains("c4")),
        "unwatched playback evaluated the changed prebake: {helper:?}"
    );
}

#[test]
#[cfg(feature = "device-audio")]
fn coordinated_live_watch_runs_prebake_first_without_false_cutovers() {
    use rustel_runtime::LiveFileProducer;

    const DEBOUNCE: Duration = Duration::from_millis(20);
    const WATCH_POLL: Duration = Duration::from_millis(1);
    const SAMPLE_RATE: u32 = 48_000;
    let temp = TempDir::new();
    let score = temp.join("coordinated.strudel");
    let prebake = temp.join("coordinated-prebake.js");
    let initial_prebake = "globalThis.pbCount = (globalThis.pbCount || 0) + 1; \
                           globalThis.liveHelper = () => note('c4').fast(8);";
    let initial_score = "liveHelper()";
    write(&prebake, initial_prebake);
    write(&score, initial_score);
    let mut session = Session::new().expect("session");
    session
        .evaluate_prebake(initial_prebake)
        .expect("initial setup");
    session.evaluate(initial_score).expect("initial score");
    let initial_generation = session.generation();
    let mut producer = LiveFileProducer::from_loaded_sources(
        &score,
        WatchLanguage::JavaScript,
        initial_score,
        Some((prebake.clone(), initial_prebake.to_string())),
        DEBOUNCE,
        WATCH_POLL,
    )
    .expect("coordinated producer");

    // Backpressure leaves a concrete old-generation batch pending.
    let initial = producer
        .step(
            &mut session,
            Duration::ZERO,
            0.0,
            SAMPLE_RATE,
            |_, _, _| panic!("baseline changed generation"),
            |_| false,
        )
        .expect("baseline step");
    assert_eq!(initial.watch, WatchPoll::Unchanged);
    assert_eq!(initial.prebake_watch, WatchPoll::Unchanged);
    assert!(initial.pending > 0, "backpressure fixture queued nothing");
    // A score-only save must not rerun setup. Expose pbCount through the new
    // graph so a hidden setup replay changes an exact value, not merely an
    // internal counter.
    write(&score, "stack(liveHelper(), pure(pbCount))");
    let pending_score = producer
        .step(
            &mut session,
            Duration::from_millis(2),
            0.002,
            SAMPLE_RATE,
            |_, _, _| panic!("pending score edit changed generation"),
            |_| false,
        )
        .expect("pending score-only step");
    assert_eq!(pending_score.watch, WatchPoll::Pending);
    let mut score_published = Vec::new();
    let mut stable_front = None;
    let stable_score = producer
        .step(
            &mut session,
            Duration::from_millis(22),
            0.022,
            SAMPLE_RATE,
            |generation, _, _| score_published.push(generation),
            |event| {
                stable_front.get_or_insert(event);
                false
            },
        )
        .expect("defer stable score behind old producer backlog");
    assert_eq!(stable_score.prebake_watch, WatchPoll::Unchanged);
    assert_eq!(stable_score.watch, WatchPoll::Pending);
    assert!(score_published.is_empty());
    assert_eq!(session.generation(), initial_generation);
    assert_eq!(stable_score.pending, initial.pending);
    assert!(
        stable_front.is_some(),
        "backlog fixture exposed no queue head"
    );

    // Flush the old producer-side backlog first. Because it was nonempty at
    // entry, this step still observes rather than evaluates the score; the
    // exact stable identity remains retryable on the next poll.
    let flushed_score = producer
        .step(
            &mut session,
            Duration::from_millis(23),
            0.023,
            SAMPLE_RATE,
            |generation, _, _| score_published.push(generation),
            |_| true,
        )
        .expect("flush old score backlog");
    assert_eq!(flushed_score.watch, WatchPoll::Pending);
    assert_eq!(flushed_score.pending, 0);
    assert!(score_published.is_empty());

    let mut score_front = None;
    let score_installed = producer
        .step(
            &mut session,
            Duration::from_millis(24),
            0.024,
            SAMPLE_RATE,
            |generation, _, _| score_published.push(generation),
            |event| {
                score_front.get_or_insert(event);
                false
            },
        )
        .expect("install retained score after backlog handoff");
    assert_eq!(score_installed.prebake_watch, WatchPoll::Unchanged);
    assert!(matches!(
        score_installed.watch,
        WatchPoll::Event(ref event) if event.status == ReloadStatus::Installed
    ));
    assert_eq!(score_published, [initial_generation + 1]);
    let after_score = session
        .query(Fraction::ZERO, Fraction::ONE)
        .expect("query score-only reload");
    assert!(
        after_score.iter().any(|hap| hap.value.show() == "1"),
        "a score-only save reran prebake: {after_score:?}"
    );
    let score_generation = initial_generation + 1;
    let pending_before = score_installed.pending;
    assert!(
        pending_before > 0,
        "score reload left no backpressured batch"
    );

    // A setup-only edit must NOT run while the old queried batch is still
    // waiting to cross the ring. The identity stays retryable, the batch head
    // stays exact, and no generation is published.
    let next_prebake = "globalThis.pbCount++; \
                        globalThis.liveHelper = () => note('e4').fast(8);";
    write(&prebake, next_prebake);
    let mut pending_front = None;
    let pending = producer
        .step(
            &mut session,
            Duration::from_millis(30),
            0.01,
            SAMPLE_RATE,
            |_, _, _| panic!("pending setup edit changed generation"),
            |event| {
                pending_front.get_or_insert(event);
                false
            },
        )
        .expect("pending setup step");
    assert_eq!(pending.prebake_watch, WatchPoll::Pending);
    let mut deferred_front = None;
    let deferred = producer
        .step(
            &mut session,
            Duration::from_millis(50),
            0.03,
            SAMPLE_RATE,
            |_, _, _| panic!("setup-only edit changed generation"),
            |event| {
                deferred_front.get_or_insert(event);
                false
            },
        )
        .expect("defer stable setup behind producer backlog");
    assert_eq!(deferred.prebake_watch, WatchPoll::Pending);
    assert_eq!(deferred.pending, pending_before);
    assert_eq!(deferred_front, score_front, "deferral changed queue head");

    // Flush the exact old-generation batch. Because it was pending at entry,
    // this step still cannot evaluate setup; the following step may.
    let flushed = producer
        .step(
            &mut session,
            Duration::from_millis(51),
            0.031,
            SAMPLE_RATE,
            |_, _, _| panic!("backlog flush changed generation"),
            |_| true,
        )
        .expect("flush old batch before setup");
    assert_eq!(flushed.prebake_watch, WatchPoll::Pending);
    assert_eq!(flushed.pending, 0);

    let installed = producer
        .step(
            &mut session,
            Duration::from_millis(52),
            0.032,
            SAMPLE_RATE,
            |_, _, _| panic!("setup-only edit changed generation"),
            |_| true,
        )
        .expect("install setup after backlog handoff");
    let WatchPoll::Event(ref prebake_event) = installed.prebake_watch else {
        panic!("setup edit did not install after flush: {installed:?}");
    };
    assert_eq!(prebake_event.target, WatchTarget::Prebake);
    assert_eq!(prebake_event.status, ReloadStatus::Installed);
    assert_eq!(session.generation(), score_generation);
    assert_eq!(
        pending_front, score_front,
        "pending setup changed queue head"
    );
    assert!(score_front.is_some(), "queue-identity witness saw no event");
    let after_prebake = session
        .query(Fraction::ZERO, Fraction::ONE)
        .expect("query after setup-only edit");
    assert!(
        after_prebake
            .iter()
            .any(|hap| hap.value.show().contains("c4")),
        "setup rebuilt the score: {after_prebake:?}"
    );
    assert!(
        after_prebake.iter().any(|hap| hap.value.show() == "1"),
        "setup-only edit changed the already-built score: {after_prebake:?}"
    );

    // When both files settle together, setup is evaluated first and the score
    // is built against its new helper in the same step.
    let simultaneous_prebake = "globalThis.pbCount++; \
                                globalThis.parserCalls = 0; \
                                setStringParser(value => { parserCalls++; return mini(value); }); \
                                globalThis.liveHelper = () => note('g4').fast(8);";
    let simultaneous_score = "(() => { \
        const routed = stack(['g4', 'a4'].join(' ')).note().fast(8); \
        if (parserCalls !== 1) throw new Error(`parser calls: ${parserCalls}`); \
        return routed.fast(2); \
    })()";
    write(&prebake, simultaneous_prebake);
    write(&score, simultaneous_score);
    let pending = producer
        .step(
            &mut session,
            Duration::from_millis(60),
            0.04,
            SAMPLE_RATE,
            |_, _, _| panic!("pending simultaneous edit changed generation"),
            |_| false,
        )
        .expect("pending simultaneous step");
    assert_eq!(pending.prebake_watch, WatchPoll::Pending);
    assert_eq!(pending.watch, WatchPoll::Pending);
    let mut published = Vec::new();
    let installed = producer
        .step(
            &mut session,
            Duration::from_millis(80),
            0.06,
            SAMPLE_RATE,
            |generation, _, _| published.push(generation),
            |_| true,
        )
        .expect("installed simultaneous step");
    let WatchPoll::Event(ref prebake_event) = installed.prebake_watch else {
        panic!("simultaneous setup did not install: {installed:?}");
    };
    let WatchPoll::Event(score_event) = installed.watch else {
        panic!("simultaneous score did not install: {installed:?}");
    };
    assert_eq!(prebake_event.target, WatchTarget::Prebake);
    assert_eq!(
        score_event.target,
        WatchTarget::Score(WatchLanguage::JavaScript)
    );
    assert_eq!(published, [score_generation + 1]);
    assert_eq!(session.generation(), score_generation + 1);
    let current = session
        .query(Fraction::ZERO, Fraction::ONE)
        .expect("query simultaneously rebuilt score");
    let current_values = current
        .iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert_eq!(
        current_values.len(),
        32,
        "the configured two-step Mini Pattern did not survive fast(8).fast(2)"
    );
    assert!(
        current_values
            .as_chunks::<2>()
            .0
            .iter()
            .all(|pair| *pair == ["note:g4", "note:a4"]),
        "the watched route counted a parser call but ignored its two-step Pattern: \
         {current_values:?}"
    );

    write(
        &prebake,
        "globalThis.liveHelper = () => note('b4').fast(8);",
    );
    let pending = producer
        .step(
            &mut session,
            Duration::from_millis(90),
            0.09,
            SAMPLE_RATE,
            |_, _, _| panic!("pending final setup changed generation"),
            |_| true,
        )
        .expect("pending setup before Stop");
    assert_eq!(pending.prebake_watch, WatchPoll::Pending);
    session.transport().stop();
    let stopped = producer
        .step(
            &mut session,
            Duration::from_millis(91),
            0.091,
            SAMPLE_RATE,
            |_, _, _| panic!("Stop changed generation"),
            |_| panic!("Stop pushed an event"),
        )
        .expect("stopped coordinated producer");
    assert_eq!(stopped.watch, WatchPoll::Stopped);
    assert_eq!(stopped.prebake_watch, WatchPoll::Stopped);
    assert_eq!(stopped.pending, 0);
    session.transport().start();
    session
        .reload_at("liveHelper()", false, 0.1)
        .expect("manual rebuild after Stop");
    let after_stop = session
        .query(Fraction::ZERO, Fraction::ONE)
        .expect("query helper after Stop");
    assert!(!after_stop.is_empty(), "Stop control became silence");
    assert!(
        after_stop.iter().all(|hap| hap.value.show().contains("g4")),
        "Stop evaluated a pending prebake edit: {after_stop:?}"
    );
}

#[test]
#[cfg(feature = "device-audio")]
fn a_staggered_score_save_waits_for_its_pending_prebake() {
    use rustel_runtime::LiveFileProducer;

    const DEBOUNCE: Duration = Duration::from_millis(20);
    let temp = TempDir::new();
    let score = temp.join("staggered.strudel");
    let prebake = temp.join("staggered-prebake.js");
    let initial_prebake = "globalThis.oldHelper = () => note('c4').fast(8);";
    let initial_score = "oldHelper()";
    write(&prebake, initial_prebake);
    write(&score, initial_score);
    let mut session = Session::new().expect("session");
    session
        .evaluate_prebake(initial_prebake)
        .expect("initial setup");
    session.evaluate(initial_score).expect("initial score");
    let generation = session.generation();
    let mut producer = LiveFileProducer::from_loaded_sources(
        &score,
        WatchLanguage::JavaScript,
        initial_score,
        Some((prebake.clone(), initial_prebake.to_string())),
        DEBOUNCE,
        Duration::from_millis(1),
    )
    .expect("staggered producer");
    producer
        .step(
            &mut session,
            Duration::ZERO,
            0.0,
            48_000,
            |_, _, _| {},
            |_| true,
        )
        .expect("baseline step");

    // The score starts debouncing first and refers to a helper that does not
    // exist yet. Once the setup edit is Pending, the score may continue to be
    // observed but must not be delivered/rejected against the old heap.
    write(&score, "newHelper()");
    let score_pending = producer
        .step(
            &mut session,
            Duration::from_millis(1),
            0.001,
            48_000,
            |_, _, _| panic!("pending score changed generation"),
            |_| true,
        )
        .expect("observe score first");
    assert_eq!(score_pending.watch, WatchPoll::Pending);
    write(&prebake, "globalThis.newHelper = () => note('e4').fast(8);");
    let both_pending = producer
        .step(
            &mut session,
            Duration::from_millis(10),
            0.010,
            48_000,
            |_, _, _| panic!("pending setup changed generation"),
            |_| true,
        )
        .expect("observe setup second");
    assert_eq!(both_pending.prebake_watch, WatchPoll::Pending);
    assert_eq!(both_pending.watch, WatchPoll::Pending);

    // The score is now stable past its own debounce, while the later setup is
    // not. Delivering here reproduces the old false rejection/stranding bug.
    let score_ready_setup_pending = producer
        .step(
            &mut session,
            Duration::from_millis(25),
            0.025,
            48_000,
            |_, _, _| panic!("score installed before its setup"),
            |_| true,
        )
        .expect("defer ready score");
    assert_eq!(score_ready_setup_pending.prebake_watch, WatchPoll::Pending);
    assert_eq!(score_ready_setup_pending.watch, WatchPoll::Pending);
    assert_eq!(session.generation(), generation);

    let mut published = Vec::new();
    let installed = producer
        .step(
            &mut session,
            Duration::from_millis(30),
            0.030,
            48_000,
            |next, _, _| published.push(next),
            |_| true,
        )
        .expect("install setup then deferred score");
    let WatchPoll::Event(ref setup_event) = installed.prebake_watch else {
        panic!("staggered setup did not install: {installed:?}");
    };
    let WatchPoll::Event(ref score_event) = installed.watch else {
        panic!("deferred score did not install: {installed:?}");
    };
    assert_eq!(setup_event.status, ReloadStatus::Installed);
    assert_eq!(score_event.status, ReloadStatus::Installed);
    assert_eq!(published, [generation + 1]);
    let current = session
        .query(Fraction::ZERO, Fraction::ONE)
        .expect("query staggered replacement");
    assert!(!current.is_empty(), "staggered replacement became silence");
    assert!(
        current.iter().all(|hap| hap.value.show().contains("e4")),
        "score was stranded before its setup: {current:?}"
    );
}

#[test]
#[cfg(feature = "device-audio")]
fn failed_prebake_keeps_a_pending_score_retryable_until_setup_recovers() {
    use rustel_runtime::LiveFileProducer;

    const DEBOUNCE: Duration = Duration::from_millis(20);
    let temp = TempDir::new();
    let score = temp.join("failed-setup-score.strudel");
    let prebake = temp.join("failed-setup.js");
    let initial_prebake = "globalThis.recovered = () => note('c4').fast(8);";
    let initial_score = "recovered()";
    write(&prebake, initial_prebake);
    write(&score, initial_score);
    let mut session = Session::new().expect("session");
    session
        .evaluate_prebake(initial_prebake)
        .expect("initial setup");
    session.evaluate(initial_score).expect("initial score");
    let generation = session.generation();
    let mut producer = LiveFileProducer::from_loaded_sources(
        &score,
        WatchLanguage::JavaScript,
        initial_score,
        Some((prebake.clone(), initial_prebake.to_string())),
        DEBOUNCE,
        Duration::from_millis(1),
    )
    .expect("coordinated producer");

    let mut old_front = None;
    let baseline = producer
        .step(
            &mut session,
            Duration::ZERO,
            0.0,
            48_000,
            |_, _, _| panic!("baseline changed generation"),
            |event| {
                old_front.get_or_insert(event);
                false
            },
        )
        .expect("baseline step");
    assert!(baseline.pending > 0, "backpressure fixture queued nothing");
    let old_front = old_front.expect("backpressure fixture exposed no event");

    write(&score, "stack(recovered(), pure(partialMarker))");
    let score_pending = producer
        .step(
            &mut session,
            Duration::from_millis(1),
            0.001,
            48_000,
            |_, _, _| panic!("pending score changed generation"),
            |event| {
                assert_eq!(event, old_front, "pending score changed queue front");
                false
            },
        )
        .expect("observe score first");
    assert_eq!(score_pending.watch, WatchPoll::Pending);

    // This setup completes useful side effects before throwing. If the score
    // is delivered on the Rejected event, it can therefore install against a
    // half-applied setup and become permanently marked as delivered.
    write(
        &prebake,
        "globalThis.partialMarker = (globalThis.partialMarker || 0) + 1; \
         globalThis.recovered = () => note('e4').fast(8); \
         throw new Error('setup broke');",
    );
    for (observed_at, schedule_now, label) in [
        (Duration::from_millis(10), 0.010, "both pending"),
        (Duration::from_millis(25), 0.025, "score ready first"),
    ] {
        let step = producer
            .step(
                &mut session,
                observed_at,
                schedule_now,
                48_000,
                |_, _, _| panic!("{label} changed generation"),
                |event| {
                    assert_eq!(event, old_front, "{label} changed queue front");
                    false
                },
            )
            .expect(label);
        assert_eq!(step.prebake_watch, WatchPoll::Pending, "{label}");
        assert_eq!(step.watch, WatchPoll::Pending, "{label}");
    }

    // Setup may not run while old queried events are still producer-owned.
    // Flush that backlog first; the stable identity remains Pending and is
    // attempted on the next poll.
    let flushed = producer
        .step(
            &mut session,
            Duration::from_millis(30),
            0.030,
            48_000,
            |_, _, _| panic!("backlog flush changed generation"),
            |_| true,
        )
        .expect("flush before setup evaluation");
    assert_eq!(flushed.prebake_watch, WatchPoll::Pending);
    assert_eq!(flushed.watch, WatchPoll::Pending);
    assert_eq!(flushed.pending, 0);

    let rejected_step = producer
        .step(
            &mut session,
            Duration::from_millis(31),
            0.031,
            48_000,
            |_, _, _| panic!("rejected setup changed generation"),
            |_| true,
        )
        .expect("reject setup without delivering score");
    let WatchPoll::Event(ref rejected_setup) = rejected_step.prebake_watch else {
        panic!("setup failure was not delivered: {rejected_step:?}");
    };
    assert_eq!(rejected_setup.status, ReloadStatus::Rejected);
    assert_eq!(rejected_step.watch, WatchPoll::Pending);
    assert_eq!(session.generation(), generation);

    // The rejection latch is sticky. Deferring only on the rejection event is
    // insufficient: the next Unchanged poll would still strand the score.
    let sticky = producer
        .step(
            &mut session,
            Duration::from_millis(32),
            0.032,
            48_000,
            |_, _, _| panic!("unchanged failed setup changed generation"),
            |_| true,
        )
        .expect("retain rejected setup latch");
    assert_eq!(sticky.prebake_watch, WatchPoll::Unchanged);
    assert_eq!(sticky.watch, WatchPoll::Pending);

    write(&prebake, "globalThis.recovered = () => note('g4').fast(8);");
    let correction_pending = producer
        .step(
            &mut session,
            Duration::from_millis(40),
            0.040,
            48_000,
            |_, _, _| panic!("pending correction changed generation"),
            |event| {
                assert_eq!(event, old_front, "pending correction changed queue front");
                false
            },
        )
        .expect("observe corrected setup");
    assert_eq!(correction_pending.prebake_watch, WatchPoll::Pending);
    assert_eq!(correction_pending.watch, WatchPoll::Pending);

    let mut published = Vec::new();
    let recovered = producer
        .step(
            &mut session,
            Duration::from_millis(60),
            0.060,
            48_000,
            |next, _, _| published.push(next),
            |_| true,
        )
        .expect("install corrected setup then retained score");
    let WatchPoll::Event(ref setup_event) = recovered.prebake_watch else {
        panic!("corrected setup did not install: {recovered:?}");
    };
    let WatchPoll::Event(ref score_event) = recovered.watch else {
        panic!("retained score did not install: {recovered:?}");
    };
    assert_eq!(setup_event.status, ReloadStatus::Installed);
    assert_eq!(score_event.status, ReloadStatus::Installed);
    assert_eq!(published, [generation + 1]);
    let current = session
        .query(Fraction::ZERO, Fraction::ONE)
        .expect("query recovered score");
    assert!(
        current.iter().any(|hap| hap.value.show().contains("g4")),
        "score did not use the corrected setup: {current:?}"
    );
    assert!(
        current.iter().any(|hap| hap.value.show() == "1"),
        "completed side effects from the failed setup were lost or replayed: {current:?}"
    );

    let unchanged = producer
        .step(
            &mut session,
            Duration::from_millis(61),
            0.061,
            48_000,
            |_, _, _| panic!("recovered identity published twice"),
            |_| true,
        )
        .expect("settled recovery identity");
    assert_eq!(unchanged.prebake_watch, WatchPoll::Unchanged);
    assert_eq!(unchanged.watch, WatchPoll::Unchanged);
}

#[test]
#[cfg(feature = "device-audio")]
fn retained_score_survives_an_insufficient_successful_setup_correction() {
    use rustel_runtime::LiveFileProducer;

    const DEBOUNCE: Duration = Duration::from_millis(20);
    let temp = TempDir::new();
    let score = temp.join("multi-setup-recovery-score.strudel");
    let prebake = temp.join("multi-setup-recovery.js");
    let initial_prebake = "globalThis.initial = () => note('c4').fast(8);";
    let initial_score = "initial()";
    write(&prebake, initial_prebake);
    write(&score, initial_score);
    let mut session = Session::new().expect("session");
    session
        .evaluate_prebake(initial_prebake)
        .expect("initial setup");
    session.evaluate(initial_score).expect("initial score");
    let generation = session.generation();
    let mut producer = LiveFileProducer::from_loaded_sources(
        &score,
        WatchLanguage::JavaScript,
        initial_score,
        Some((prebake.clone(), initial_prebake.to_string())),
        DEBOUNCE,
        Duration::from_millis(1),
    )
    .expect("coordinated producer");
    producer
        .step(
            &mut session,
            Duration::ZERO,
            0.0,
            48_000,
            |_, _, _| {},
            |_| true,
        )
        .expect("baseline step");

    write(&score, "stack(wanted(), pure(partialMarker))");
    assert_eq!(
        producer
            .step(
                &mut session,
                Duration::from_millis(1),
                0.001,
                48_000,
                |_, _, _| panic!("pending score published"),
                |_| true,
            )
            .expect("observe dependent score")
            .watch,
        WatchPoll::Pending
    );
    write(
        &prebake,
        "globalThis.partialMarker = (globalThis.partialMarker || 0) + 1; \
         throw new Error('first setup failed');",
    );
    producer
        .step(
            &mut session,
            Duration::from_millis(10),
            0.010,
            48_000,
            |_, _, _| panic!("pending setup published"),
            |_| true,
        )
        .expect("observe failing setup");
    let failed = producer
        .step(
            &mut session,
            Duration::from_millis(30),
            0.030,
            48_000,
            |_, _, _| panic!("failed setup published"),
            |_| true,
        )
        .expect("reject first setup");
    assert!(matches!(
        failed.prebake_watch,
        WatchPoll::Event(ref event) if event.status == ReloadStatus::Rejected
    ));
    assert_eq!(failed.watch, WatchPoll::Pending);
    assert_eq!(session.generation(), generation);

    // A successful setup identity is not necessarily sufficient for the
    // retained score. Its rejection must not commit the score identity.
    write(&prebake, "globalThis.wrong = () => note('e4').fast(8);");
    let first_correction_pending = producer
        .step(
            &mut session,
            Duration::from_millis(40),
            0.040,
            48_000,
            |_, _, _| panic!("pending first correction published"),
            |_| true,
        )
        .expect("observe insufficient correction");
    assert_eq!(first_correction_pending.prebake_watch, WatchPoll::Pending);
    assert_eq!(first_correction_pending.watch, WatchPoll::Pending);
    let first_correction = producer
        .step(
            &mut session,
            Duration::from_millis(60),
            0.060,
            48_000,
            |_, _, _| panic!("insufficient correction published"),
            |_| true,
        )
        .expect("apply insufficient correction");
    assert!(matches!(
        first_correction.prebake_watch,
        WatchPoll::Event(ref event) if event.status == ReloadStatus::Installed
    ));
    assert!(matches!(
        first_correction.watch,
        WatchPoll::Event(ref event) if event.status == ReloadStatus::Rejected
    ));
    assert_eq!(session.generation(), generation);

    // The failed retry is attempted once, not flooded on every poll.
    let sticky = producer
        .step(
            &mut session,
            Duration::from_millis(61),
            0.061,
            48_000,
            |_, _, _| panic!("sticky retry published"),
            |_| true,
        )
        .expect("retain score for another setup identity");
    assert_eq!(sticky.prebake_watch, WatchPoll::Unchanged);
    assert_eq!(sticky.watch, WatchPoll::Pending);

    write(&prebake, "globalThis.wanted = () => note('g4').fast(8);");
    let final_pending = producer
        .step(
            &mut session,
            Duration::from_millis(70),
            0.070,
            48_000,
            |_, _, _| panic!("pending final correction published"),
            |_| true,
        )
        .expect("observe sufficient correction");
    assert_eq!(final_pending.prebake_watch, WatchPoll::Pending);
    assert_eq!(final_pending.watch, WatchPoll::Pending);

    let mut published = Vec::new();
    let recovered = producer
        .step(
            &mut session,
            Duration::from_millis(90),
            0.090,
            48_000,
            |next, _, _| published.push(next),
            |_| true,
        )
        .expect("install sufficient correction and retained score");
    assert!(matches!(
        recovered.prebake_watch,
        WatchPoll::Event(ref event) if event.status == ReloadStatus::Installed
    ));
    assert!(matches!(
        recovered.watch,
        WatchPoll::Event(ref event) if event.status == ReloadStatus::Installed
    ));
    assert_eq!(published, [generation + 1]);
    let current = session
        .query(Fraction::ZERO, Fraction::ONE)
        .expect("query multiply recovered score");
    assert!(
        current.iter().any(|hap| hap.value.show().contains("g4")),
        "second correction did not revive the retained score: {current:?}"
    );
    assert!(
        current.iter().any(|hap| hap.value.show() == "1"),
        "failed setup side effect was rolled back or replayed: {current:?}"
    );
}

#[test]
#[cfg(feature = "device-audio")]
fn a_new_score_can_recover_after_a_retained_score_retry_rejects() {
    use rustel_runtime::LiveFileProducer;

    const DEBOUNCE: Duration = Duration::from_millis(20);
    let temp = TempDir::new();
    let score = temp.join("new-score-after-retry.strudel");
    let prebake = temp.join("new-score-after-retry.js");
    let initial_prebake = "globalThis.initial = () => note('c4').fast(8);";
    let initial_score = "initial()";
    write(&prebake, initial_prebake);
    write(&score, initial_score);
    let mut session = Session::new().expect("session");
    session
        .evaluate_prebake(initial_prebake)
        .expect("initial setup");
    session.evaluate(initial_score).expect("initial score");
    let generation = session.generation();
    let mut producer = LiveFileProducer::from_loaded_sources(
        &score,
        WatchLanguage::JavaScript,
        initial_score,
        Some((prebake.clone(), initial_prebake.to_string())),
        DEBOUNCE,
        Duration::from_millis(1),
    )
    .expect("coordinated producer");
    producer
        .step(
            &mut session,
            Duration::ZERO,
            0.0,
            48_000,
            |_, _, _| {},
            |_| true,
        )
        .expect("baseline step");

    write(&score, "missingAfterSetup()");
    producer
        .step(
            &mut session,
            Duration::from_millis(1),
            0.001,
            48_000,
            |_, _, _| panic!("dependent score published early"),
            |_| true,
        )
        .expect("observe dependent score");
    write(&prebake, "throw new Error('setup failed');");
    producer
        .step(
            &mut session,
            Duration::from_millis(10),
            0.010,
            48_000,
            |_, _, _| panic!("pending setup published"),
            |_| true,
        )
        .expect("observe failed setup");
    let failed = producer
        .step(
            &mut session,
            Duration::from_millis(30),
            0.030,
            48_000,
            |_, _, _| panic!("failed setup published"),
            |_| true,
        )
        .expect("reject setup");
    assert!(matches!(
        failed.prebake_watch,
        WatchPoll::Event(ref event) if event.status == ReloadStatus::Rejected
    ));
    assert_eq!(failed.watch, WatchPoll::Pending);

    write(&prebake, "globalThis.unrelatedSetupValue = 1;");
    producer
        .step(
            &mut session,
            Duration::from_millis(40),
            0.040,
            48_000,
            |_, _, _| panic!("pending setup correction published"),
            |_| true,
        )
        .expect("observe successful setup correction");
    let retry = producer
        .step(
            &mut session,
            Duration::from_millis(60),
            0.060,
            48_000,
            |_, _, _| panic!("rejected retained score published"),
            |_| true,
        )
        .expect("retry retained score once");
    assert!(matches!(
        retry.prebake_watch,
        WatchPoll::Event(ref event) if event.status == ReloadStatus::Installed
    ));
    assert!(matches!(
        retry.watch,
        WatchPoll::Event(ref event) if event.status == ReloadStatus::Rejected
    ));
    assert_eq!(session.generation(), generation);

    // A new score identity is independent input. It must not remain
    // behind the suppression for the previous rejected identity merely
    // because the setup file is now unchanged.
    write(&score, "note('a4').fast(8)");
    let new_score_pending = producer
        .step(
            &mut session,
            Duration::from_millis(70),
            0.070,
            48_000,
            |_, _, _| panic!("new score skipped debounce"),
            |_| true,
        )
        .expect("observe independent score");
    assert_eq!(new_score_pending.prebake_watch, WatchPoll::Unchanged);
    assert_eq!(new_score_pending.watch, WatchPoll::Pending);

    let mut published = Vec::new();
    let installed = producer
        .step(
            &mut session,
            Duration::from_millis(90),
            0.090,
            48_000,
            |next, _, _| published.push(next),
            |_| true,
        )
        .expect("install independent score");
    assert_eq!(installed.prebake_watch, WatchPoll::Unchanged);
    assert!(matches!(
        installed.watch,
        WatchPoll::Event(ref event) if event.status == ReloadStatus::Installed
    ));
    assert_eq!(published, [generation + 1]);
    let current = session
        .query(Fraction::ZERO, Fraction::ONE)
        .expect("query independent recovery score");
    assert!(
        !current.is_empty() && current.iter().all(|hap| hap.value.show().contains("a4")),
        "new score stayed blocked after setup recovery: {current:?}"
    );
}

#[test]
#[cfg(feature = "device-audio")]
fn async_prebake_runs_before_the_retained_score_and_recovers_after_rejection() {
    use rustel_runtime::LiveFileProducer;

    const DEBOUNCE: Duration = Duration::from_millis(20);
    let temp = TempDir::new();
    let score = temp.join("async-setup-score.strudel");
    let prebake = temp.join("async-setup.js");
    let initial_prebake = "globalThis.liveHelper = () => note('c4').fast(8);";
    let initial_score = "liveHelper()";
    write(&prebake, initial_prebake);
    write(&score, initial_score);
    let mut session = Session::new().expect("session");
    session
        .evaluate_prebake(initial_prebake)
        .expect("initial setup");
    session.evaluate(initial_score).expect("initial score");
    let generation = session.generation();
    let mut producer = LiveFileProducer::from_loaded_sources(
        &score,
        WatchLanguage::JavaScript,
        initial_score,
        Some((prebake.clone(), initial_prebake.to_string())),
        DEBOUNCE,
        Duration::from_millis(1),
    )
    .expect("coordinated producer");
    producer
        .step(
            &mut session,
            Duration::ZERO,
            0.0,
            48_000,
            |_, _, _| {},
            |_| true,
        )
        .expect("baseline step");

    write(
        &prebake,
        "globalThis.asyncPrefix = 1; \
         await Promise.resolve(); \
         globalThis.liveHelper = () => note('g4').fast(8); \
         queueMicrotask(() => { globalThis.pendingRan = 'ran'; });",
    );
    write(
        &score,
        "stack(liveHelper(), pure(asyncPrefix), pure(pendingRan))",
    );
    let pending = producer
        .step(
            &mut session,
            Duration::from_millis(1),
            0.001,
            48_000,
            |_, _, _| panic!("pending async setup changed generation"),
            |_| true,
        )
        .expect("observe async setup");
    assert_eq!(pending.prebake_watch, WatchPoll::Pending);
    let mut published = Vec::new();
    let installed = producer
        .step(
            &mut session,
            Duration::from_millis(21),
            0.021,
            48_000,
            |next, _, _| published.push(next),
            |_| true,
        )
        .expect("install async setup and retained score");
    let WatchPoll::Event(ref async_event) = installed.prebake_watch else {
        panic!("async setup was not installed: {installed:?}");
    };
    assert_eq!(async_event.status, ReloadStatus::Installed);
    assert!(matches!(
        installed.watch,
        WatchPoll::Event(ref event) if event.status == ReloadStatus::Installed
    ));
    assert_eq!(published, [generation + 1]);
    let current = session
        .query(Fraction::ZERO, Fraction::ONE)
        .expect("query score built after async setup");
    assert!(
        current.iter().any(|hap| hap.value.show().contains("g4")),
        "score was evaluated before the awaited helper existed: {current:?}"
    );
    assert!(
        current.iter().any(|hap| hap.value.show() == "1"),
        "async setup prefix did not survive on the same heap: {current:?}"
    );
    assert!(
        current.iter().any(|hap| hap.value.show() == "ran"),
        "executor stopped at root settlement instead of queue quiescence: {current:?}"
    );

    let active_before_rejection = current.iter().map(|hap| hap.show()).collect::<Vec<_>>();
    write(
        &prebake,
        "globalThis.rejectedPrefix = 1; await Promise.resolve(); \
         throw new Error('queued setup broke');",
    );
    assert_eq!(
        producer
            .step(
                &mut session,
                Duration::from_millis(30),
                0.030,
                48_000,
                |_, _, _| panic!("pending rejection changed generation"),
                |_| true,
            )
            .expect("observe rejecting setup")
            .prebake_watch,
        WatchPoll::Pending
    );
    let rejected = producer
        .step(
            &mut session,
            Duration::from_millis(50),
            0.050,
            48_000,
            |_, _, _| panic!("rejected async setup changed generation"),
            |_| true,
        )
        .expect("report rejecting async setup");
    let WatchPoll::Event(ref rejected_event) = rejected.prebake_watch else {
        panic!("async rejection was not reported: {rejected:?}");
    };
    assert_eq!(rejected_event.status, ReloadStatus::Rejected);
    assert!(
        rejected_event
            .message
            .as_deref()
            .is_some_and(|message| message.contains("queued setup broke")),
        "wrong async rejection: {rejected_event:?}"
    );
    assert_eq!(session.generation(), generation + 1);
    assert_eq!(
        session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query last-good graph after async rejection")
            .iter()
            .map(|hap| hap.show())
            .collect::<Vec<_>>(),
        active_before_rejection,
        "async rejection replaced the last-good graph"
    );

    write(
        &prebake,
        "await Promise.resolve(); globalThis.correctedPrefix = rejectedPrefix + 1;",
    );
    assert_eq!(
        producer
            .step(
                &mut session,
                Duration::from_millis(60),
                0.060,
                48_000,
                |_, _, _| panic!("pending recovery changed generation"),
                |_| true,
            )
            .expect("observe corrected setup")
            .prebake_watch,
        WatchPoll::Pending
    );
    let corrected = producer
        .step(
            &mut session,
            Duration::from_millis(80),
            0.080,
            48_000,
            |_, _, _| panic!("setup-only recovery changed generation"),
            |_| true,
        )
        .expect("install corrected async setup");
    let WatchPoll::Event(ref corrected_event) = corrected.prebake_watch else {
        panic!("corrected setup did not install: {corrected:?}");
    };
    assert_eq!(corrected_event.status, ReloadStatus::Installed);
    assert_eq!(session.generation(), generation + 1);
    session
        .reload_at("pure(correctedPrefix)", false, 0.081)
        .expect("score observes same-heap async recovery state");
    let recovered = session
        .query(Fraction::ZERO, Fraction::ONE)
        .expect("query recovery state");
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].value.show(), "2");
}

#[test]
#[cfg(feature = "device-audio")]
fn watched_setup_defers_without_consuming_the_identity_then_retries_once() {
    use rustel_runtime::LiveFileProducer;

    const DEBOUNCE: Duration = Duration::from_millis(10);
    const POLL: Duration = Duration::from_millis(1);
    const RESERVE: Duration = Duration::from_millis(10);
    let temp = TempDir::new();
    let score = temp.join("deferred-setup-score.strudel");
    let prebake = temp.join("deferred-setup.js");
    let initial_setup = "globalThis.deferredHelper = () => note('c4').fast(8);";
    let initial_score = "deferredHelper()";
    write(&prebake, initial_setup);
    write(&score, initial_score);

    let mut session = Session::new().expect("session");
    session
        .evaluate_prebake(initial_setup)
        .expect("initial setup");
    session.evaluate(initial_score).expect("initial score");
    let generation = session.generation();
    let mut producer = LiveFileProducer::from_loaded_sources_with_prebake_floor(
        &score,
        WatchLanguage::JavaScript,
        initial_score,
        Some((prebake.clone(), initial_setup.to_string())),
        DEBOUNCE,
        POLL,
        RESERVE,
    )
    .expect("producer");
    producer
        .step(
            &mut session,
            Duration::ZERO,
            0.0,
            48_000,
            |_, _, _| {},
            |_| true,
        )
        .expect("fill initial horizon");

    write(
        &prebake,
        "globalThis.deferredRuns = (globalThis.deferredRuns ?? 0) + 1; \
         globalThis.deferredHelper = () => note('g4').fast(8);",
    );
    write(&score, "stack(deferredHelper(), pure(deferredRuns))");
    let pending = producer
        .step(
            &mut session,
            Duration::from_millis(100),
            0.6,
            48_000,
            |_, _, _| panic!("pending identities published a generation"),
            |_| true,
        )
        .expect("observe both edits");
    assert_eq!(pending.prebake_watch, WatchPoll::Pending);
    assert_eq!(pending.watch, WatchPoll::Pending);

    // The preceding step filled through 1.1s. At exactly 1.1 no horizon is
    // left, so setup must remain wholly unapplied while this same step refills
    // the old score for the retry.
    let deferred = producer
        .step(
            &mut session,
            Duration::from_millis(120),
            1.1,
            48_000,
            |_, _, _| panic!("a deferred setup published a generation"),
            |_| true,
        )
        .expect("defer without audio cover");
    assert_eq!(deferred.prebake_watch, WatchPoll::Pending);
    assert_eq!(deferred.watch, WatchPoll::Pending);
    assert_eq!(session.generation(), generation);

    let mut published = Vec::new();
    let installed = producer
        .step(
            &mut session,
            Duration::from_millis(121),
            1.101,
            48_000,
            |next, _, _| published.push(next),
            |_| true,
        )
        .expect("retry deferred identity after refill");
    assert!(matches!(
        installed.prebake_watch,
        WatchPoll::Event(ref event) if event.status == ReloadStatus::Installed
    ));
    assert!(matches!(
        installed.watch,
        WatchPoll::Event(ref event) if event.status == ReloadStatus::Installed
    ));
    assert_eq!(published, [generation + 1]);
    let current = session
        .query(Fraction::ZERO, Fraction::ONE)
        .expect("query retained score after deferred setup");
    assert!(current.iter().any(|hap| hap.value.show().contains("g4")));
    assert!(
        current.iter().any(|hap| hap.value.show() == "1"),
        "deferred setup ran zero or multiple times: {current:?}"
    );

    let unchanged = producer
        .step(
            &mut session,
            Duration::from_millis(122),
            1.102,
            48_000,
            |_, _, _| panic!("deferred identity installed twice"),
            |_| true,
        )
        .expect("settled deferred identity");
    assert_eq!(unchanged.prebake_watch, WatchPoll::Unchanged);
    assert_eq!(unchanged.watch, WatchPoll::Unchanged);
}

#[test]
#[cfg(feature = "device-audio")]
fn first_ready_setup_waits_for_one_real_continuation_sample() {
    use rustel_runtime::LiveFileProducer;

    let temp = TempDir::new();
    let score = temp.join("setup-warmup-score.strudel");
    let prebake = temp.join("setup-warmup.js");
    let initial_setup = "globalThis.warmHelper = () => note('c4').fast(8);";
    let initial_score = "warmHelper()";
    write(&prebake, initial_setup);
    write(&score, initial_score);
    let mut session = Session::new().expect("session");
    session
        .evaluate_prebake(initial_setup)
        .expect("initial setup");
    session.evaluate(initial_score).expect("initial score");
    let mut producer = LiveFileProducer::from_loaded_sources_with_prebake_floor(
        &score,
        WatchLanguage::JavaScript,
        initial_score,
        Some((prebake.clone(), initial_setup.to_string())),
        Duration::ZERO,
        Duration::from_millis(1),
        Duration::from_millis(2),
    )
    .expect("producer");

    // Change the setup before the producer has completed even one normal
    // scheduling continuation. The changed observation is not itself a timing
    // sample, and the first ready poll must be retained for one warmup step.
    write(
        &prebake,
        "globalThis.warmRuns = (globalThis.warmRuns ?? 0) + 1; \
         globalThis.warmHelper = () => note('g4').fast(8);",
    );
    assert_eq!(
        producer
            .step(
                &mut session,
                Duration::ZERO,
                0.0,
                48_000,
                |_, _, _| {},
                |_| true
            )
            .expect("observe pre-warmup edit")
            .prebake_watch,
        WatchPoll::Pending
    );
    assert_eq!(
        producer
            .step(
                &mut session,
                Duration::from_millis(1),
                0.001,
                48_000,
                |_, _, _| panic!("warmup published a generation"),
                |_| true,
            )
            .expect("measure normal continuation before setup")
            .prebake_watch,
        WatchPoll::Pending,
        "the first ready identity ran without a measured continuation"
    );
    let applied = producer
        .step(
            &mut session,
            Duration::from_millis(2),
            0.002,
            48_000,
            |_, _, _| panic!("setup-only warmup edit cut generations"),
            |_| true,
        )
        .expect("apply after continuation warmup");
    assert!(matches!(
        applied.prebake_watch,
        WatchPoll::Event(ref event) if event.status == ReloadStatus::Installed
    ));
    session
        .reload_at("stack(warmHelper(), pure(warmRuns))", false, 0.003)
        .expect("score using warmed setup");
    let current = session
        .query(Fraction::ZERO, Fraction::ONE)
        .expect("query warmed setup");
    assert!(current.iter().any(|hap| hap.value.show().contains("g4")));
    assert!(current.iter().any(|hap| hap.value.show() == "1"));
}

#[test]
#[cfg(feature = "device-audio")]
fn first_ready_score_waits_for_one_real_continuation_sample() {
    use rustel_runtime::LiveFileProducer;

    let temp = TempDir::new();
    let score = temp.join("score-warmup.strudel");
    let initial = "note('c4')";
    write(&score, initial);
    let mut session = Session::new().expect("session");
    session.evaluate(initial).expect("initial score");
    let generation = session.generation();
    let mut producer = LiveFileProducer::from_loaded_sources_with_prebake_floor(
        &score,
        WatchLanguage::JavaScript,
        initial,
        None,
        Duration::ZERO,
        Duration::from_millis(1),
        Duration::from_millis(1),
    )
    .expect("producer");

    // Change the score before the producer has measured even one normal
    // post-evaluation scheduling/ring continuation.
    write(
        &score,
        "globalThis.scoreWarmRuns = (globalThis.scoreWarmRuns ?? 0) + 1; \
         stack(note('g4'), pure(scoreWarmRuns))",
    );
    let changed = producer
        .step(
            &mut session,
            Duration::ZERO,
            0.0,
            48_000,
            |_, _, _| panic!("changed observation installed score"),
            |_| true,
        )
        .expect("observe pre-warmup score edit");
    assert_eq!(changed.watch, WatchPoll::Pending);
    assert_eq!(session.generation(), generation);
    assert!(shown(&session).contains("c4"));

    let ready = producer
        .step(
            &mut session,
            Duration::from_millis(1),
            0.001,
            48_000,
            |_, _, _| panic!("first ready score ran without measured continuation"),
            |_| true,
        )
        .expect("measure one normal continuation");
    assert_eq!(ready.watch, WatchPoll::Pending);
    assert_eq!(session.generation(), generation);
    assert!(shown(&session).contains("c4"));

    let mut published = Vec::new();
    let applied = producer
        .step(
            &mut session,
            Duration::from_millis(2),
            0.002,
            48_000,
            |next, _, _| published.push(next),
            |_| true,
        )
        .expect("apply after score continuation warmup");
    assert!(matches!(
        applied.watch,
        WatchPoll::Event(ref event) if event.status == ReloadStatus::Installed
    ));
    assert_eq!(published, [generation + 1]);
    let current = session
        .query(Fraction::ZERO, Fraction::ONE)
        .expect("query warmed score");
    assert!(current.iter().any(|hap| hap.value.show().contains("g4")));
    assert!(
        current.iter().any(|hap| hap.value.show() == "1"),
        "changed/ready observation executed score early or more than once: {current:?}"
    );
    assert_eq!(
        producer
            .step(
                &mut session,
                Duration::from_millis(3),
                0.003,
                48_000,
                |_, _, _| panic!("warmed score installed twice"),
                |_| true,
            )
            .expect("poll warmed identity")
            .watch,
        WatchPoll::Unchanged
    );
}

#[test]
#[cfg(feature = "device-audio")]
fn watched_setup_budget_includes_observed_product_continuation_high_water() {
    use std::cell::Cell;

    use rustel_runtime::LiveFileProducer;

    let temp = TempDir::new();
    let score = temp.join("setup-high-water-score.strudel");
    let prebake = temp.join("setup-high-water.js");
    let initial_setup = "globalThis.highWaterHelper = () => note('c4').fast(8);";
    let initial_score = "highWaterHelper()";
    write(&prebake, initial_setup);
    write(&score, initial_score);
    let mut session = Session::new().expect("session");
    session
        .evaluate_prebake(initial_setup)
        .expect("initial setup");
    session.evaluate(initial_score).expect("initial score");
    let mut producer = LiveFileProducer::from_loaded_sources_with_prebake_floor(
        &score,
        WatchLanguage::JavaScript,
        initial_score,
        Some((prebake.clone(), initial_setup.to_string())),
        Duration::from_millis(10),
        Duration::from_millis(1),
        Duration::from_millis(2),
    )
    .expect("producer");

    // The delay is inside the real ring-transfer closure, so the producer's
    // measured high-water covers the actual post-setup continuation rather
    // than a test-only number injected into Session.
    let slept = Cell::new(false);
    producer
        .step(
            &mut session,
            Duration::ZERO,
            0.0,
            48_000,
            |_, _, _| {},
            |_| {
                if !slept.replace(true) {
                    std::thread::sleep(Duration::from_millis(30));
                }
                true
            },
        )
        .expect("measure slow product continuation");
    assert!(slept.get(), "fixture did not measure the ring transfer");
    write(
        &prebake,
        "globalThis.highWaterRuns = (globalThis.highWaterRuns ?? 0) + 1; \
         globalThis.highWaterHelper = () => note('g4').fast(8);",
    );
    assert_eq!(
        producer
            .step(
                &mut session,
                Duration::from_millis(10),
                0.0,
                48_000,
                |_, _, _| {},
                |_| true,
            )
            .expect("observe high-water setup")
            .prebake_watch,
        WatchPoll::Pending
    );

    // Seventy milliseconds of horizon remains. The 2ms caller floor alone
    // would allow this setup; floor + the observed >=30ms continuation, under
    // Scheduler's safety factor, must defer it.
    let deferred = producer
        .step(
            &mut session,
            Duration::from_millis(20),
            0.43,
            48_000,
            |_, _, _| panic!("high-water deferral cut generations"),
            |_| true,
        )
        .expect("defer against observed high-water");
    assert_eq!(deferred.prebake_watch, WatchPoll::Pending);
    // Host scheduling can make the measured delay exceed one full horizon.
    // Normal turns must then age that sample out before setup can run.
    let retry_deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut observed_at = Duration::from_millis(20);
    let applied = loop {
        observed_at += Duration::from_millis(1);
        let step = producer
            .step(
                &mut session,
                observed_at,
                0.431,
                48_000,
                |_, _, _| panic!("setup-only high-water edit cut generations"),
                |_| true,
            )
            .expect("retry after high-water deferral refilled the horizon");
        if matches!(step.prebake_watch, WatchPoll::Event(_)) {
            break step;
        }
        assert_eq!(step.prebake_watch, WatchPoll::Pending);
        assert!(
            std::time::Instant::now() < retry_deadline,
            "setup remained pending after normal continuations: {:?}",
            producer.producer_load_snapshot()
        );
    };
    assert!(
        matches!(
            applied.prebake_watch,
            WatchPoll::Event(ref event) if event.status == ReloadStatus::Installed
        ),
        "{applied:?}"
    );
    assert_eq!(
        producer
            .step(
                &mut session,
                observed_at + Duration::from_millis(1),
                0.432,
                48_000,
                |_, _, _| panic!("settled setup edit cut generations"),
                |_| true,
            )
            .expect("poll settled high-water setup")
            .prebake_watch,
        WatchPoll::Unchanged
    );
    session
        .reload_at(
            "stack(highWaterHelper(), pure(highWaterRuns))",
            false,
            0.433,
        )
        .expect("score using high-water setup");
    let current = session
        .query(Fraction::ZERO, Fraction::ONE)
        .expect("query high-water setup");
    assert!(current.iter().any(|hap| hap.value.show().contains("g4")));
    assert!(current.iter().any(|hap| hap.value.show() == "1"));
}

#[test]
#[cfg(feature = "device-audio")]
fn watched_setup_waits_for_producer_backlog_to_cross_the_ring() {
    use rustel_runtime::LiveFileProducer;

    const DEBOUNCE: Duration = Duration::from_millis(10);
    let temp = TempDir::new();
    let score = temp.join("backlogged-setup-score.strudel");
    let prebake = temp.join("backlogged-setup.js");
    let initial_setup = "globalThis.backlogHelper = () => note('c4').fast(32);";
    let initial_score = "backlogHelper()";
    write(&prebake, initial_setup);
    write(&score, initial_score);
    let mut session = Session::new().expect("session");
    session
        .evaluate_prebake(initial_setup)
        .expect("initial setup");
    session.evaluate(initial_score).expect("initial score");
    let mut producer = LiveFileProducer::from_loaded_sources_with_prebake_floor(
        &score,
        WatchLanguage::JavaScript,
        initial_score,
        Some((prebake.clone(), initial_setup.to_string())),
        DEBOUNCE,
        Duration::from_millis(1),
        Duration::from_millis(10),
    )
    .expect("producer");
    let first = producer
        .step(
            &mut session,
            Duration::ZERO,
            0.0,
            48_000,
            |_, _, _| {},
            |_| false,
        )
        .expect("create producer backlog");
    assert!(first.pending > 0, "fixture did not create back-pressure");

    write(
        &prebake,
        "globalThis.backlogRuns = (globalThis.backlogRuns ?? 0) + 1; \
         globalThis.backlogHelper = () => note('g4').fast(32);",
    );
    assert_eq!(
        producer
            .step(
                &mut session,
                Duration::from_millis(10),
                0.01,
                48_000,
                |_, _, _| panic!("pending setup cut generations"),
                |_| false,
            )
            .expect("observe setup while backlogged")
            .prebake_watch,
        WatchPoll::Pending
    );
    let still_blocked = producer
        .step(
            &mut session,
            Duration::from_millis(30),
            0.03,
            48_000,
            |_, _, _| panic!("backlogged setup cut generations"),
            |_| false,
        )
        .expect("stable setup remains behind backlog");
    assert_eq!(still_blocked.prebake_watch, WatchPoll::Pending);
    assert!(still_blocked.pending > 0);

    let flushed = producer
        .step(
            &mut session,
            Duration::from_millis(31),
            0.031,
            48_000,
            |_, _, _| panic!("flush cut generations"),
            |_| true,
        )
        .expect("flush old events");
    assert_eq!(flushed.prebake_watch, WatchPoll::Pending);
    assert_eq!(flushed.pending, 0);

    let applied = producer
        .step(
            &mut session,
            Duration::from_millis(32),
            0.032,
            48_000,
            |_, _, _| panic!("setup-only edit cut generations"),
            |_| true,
        )
        .expect("apply setup after ring handoff");
    assert!(matches!(
        applied.prebake_watch,
        WatchPoll::Event(ref event) if event.status == ReloadStatus::Installed
    ));
    session
        .reload_at("stack(backlogHelper(), pure(backlogRuns))", false, 0.033)
        .expect("score using applied setup");
    let current = session
        .query(Fraction::ZERO, Fraction::ONE)
        .expect("query applied setup");
    assert!(current.iter().any(|hap| hap.value.show().contains("g4")));
    assert!(
        current.iter().any(|hap| hap.value.show() == "1"),
        "setup ran while backlogged and then replayed: {current:?}"
    );
}

#[test]
#[cfg(feature = "device-audio")]
fn watched_score_deferral_retains_the_exact_identity_until_cover_is_refilled() {
    use rustel_runtime::LiveFileProducer;

    const DEBOUNCE: Duration = Duration::from_millis(10);
    let temp = TempDir::new();
    let score = temp.join("deferred-score.strudel");
    let initial = "note('c4')";
    write(&score, initial);
    let mut session = Session::new().expect("session");
    session.evaluate(initial).expect("initial score");
    let generation = session.generation();
    let mut producer = LiveFileProducer::from_loaded_sources_with_prebake_floor(
        &score,
        WatchLanguage::JavaScript,
        initial,
        None,
        DEBOUNCE,
        Duration::from_millis(1),
        Duration::from_millis(1),
    )
    .expect("producer");
    producer
        .step(
            &mut session,
            Duration::ZERO,
            0.0,
            48_000,
            |_, _, _| {},
            |_| true,
        )
        .expect("fill initial horizon and measure continuation");

    write(&score, "note('g4')");
    assert_eq!(
        producer
            .step(
                &mut session,
                Duration::from_millis(10),
                0.0,
                48_000,
                |_, _, _| panic!("an unsettled score cut generations"),
                |_| true,
            )
            .expect("observe changed score")
            .watch,
        WatchPoll::Pending
    );
    let deferred = producer
        .step(
            &mut session,
            Duration::from_millis(20),
            0.49,
            48_000,
            |_, _, _| panic!("a sub-floor score budget cut generations"),
            |_| true,
        )
        .expect("defer score against exhausted cover");
    assert_eq!(deferred.watch, WatchPoll::Pending);
    assert_eq!(session.generation(), generation);
    assert!(shown(&session).contains("c4"));

    let mut published = Vec::new();
    let installed = producer
        .step(
            &mut session,
            Duration::from_millis(21),
            0.491,
            48_000,
            |next, _, _| published.push(next),
            |_| true,
        )
        .expect("retry the same score bytes after refill");
    assert!(matches!(
        installed.watch,
        WatchPoll::Event(ref event) if event.status == ReloadStatus::Installed
    ));
    assert_eq!(published, [generation + 1]);
    assert!(shown(&session).contains("g4"));
    assert_eq!(
        producer
            .step(
                &mut session,
                Duration::from_millis(22),
                0.492,
                48_000,
                |_, _, _| panic!("the retained identity installed twice"),
                |_| true,
            )
            .expect("poll installed identity")
            .watch,
        WatchPoll::Unchanged
    );
}

#[test]
#[cfg(feature = "device-audio")]
fn watched_score_cpu_refusal_keeps_the_old_generation_and_same_heap_recovers() {
    use rustel_runtime::LiveFileProducer;

    const DEBOUNCE: Duration = Duration::from_millis(10);
    let temp = TempDir::new();
    let score = temp.join("bounded-score.strudel");
    let initial = "note('c4')";
    write(&score, initial);
    let mut session = Session::new().expect("session");
    session.evaluate(initial).expect("initial score");
    let generation = session.generation();
    let mut producer = LiveFileProducer::from_loaded_sources_with_prebake_floor(
        &score,
        WatchLanguage::JavaScript,
        initial,
        None,
        DEBOUNCE,
        Duration::from_millis(1),
        Duration::from_millis(1),
    )
    .expect("producer");
    producer
        .step(
            &mut session,
            Duration::ZERO,
            0.0,
            48_000,
            |_, _, _| {},
            |_| true,
        )
        .expect("fill initial horizon and measure continuation");

    write(
        &score,
        "globalThis.scoreDeadlinePrefix = 1; while (true) {}",
    );
    assert_eq!(
        producer
            .step(
                &mut session,
                Duration::from_millis(10),
                0.0,
                48_000,
                |_, _, _| panic!("an unsettled score cut generations"),
                |_| true,
            )
            .expect("observe runaway score")
            .watch,
        WatchPoll::Pending
    );
    let started = std::time::Instant::now();
    let refused = producer
        .step(
            &mut session,
            Duration::from_millis(20),
            0.40,
            48_000,
            |_, _, _| panic!("a refused score cut generations"),
            |_| true,
        )
        .expect("bound runaway score");
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "watched score ignored its remaining-horizon budget: {:?}",
        started.elapsed()
    );
    let event = match refused.watch {
        WatchPoll::Event(event) => event,
        other => panic!("runaway score was not reported: {other:?}"),
    };
    assert_eq!(event.status, ReloadStatus::Rejected);
    assert_eq!(event.error_kind.as_deref(), Some("resource-limit"));
    assert!(
        event
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("CPU deadline")
    );
    assert_eq!(event.generation_before, generation);
    assert_eq!(event.generation_after, generation);
    assert_eq!(session.generation(), generation);
    assert!(shown(&session).contains("c4"));

    write(&score, "stack(note('g4'), pure(scoreDeadlinePrefix))");
    assert_eq!(
        producer
            .step(
                &mut session,
                Duration::from_millis(30),
                0.401,
                48_000,
                |_, _, _| panic!("an unsettled correction cut generations"),
                |_| true,
            )
            .expect("observe corrected score")
            .watch,
        WatchPoll::Pending
    );
    let mut published = Vec::new();
    let corrected = producer
        .step(
            &mut session,
            Duration::from_millis(40),
            0.402,
            48_000,
            |next, _, _| published.push(next),
            |_| true,
        )
        .expect("install same-heap correction");
    assert!(matches!(
        corrected.watch,
        WatchPoll::Event(ref event) if event.status == ReloadStatus::Installed
    ));
    assert_eq!(published, [generation + 1]);
    let current = session
        .query(Fraction::ZERO, Fraction::ONE)
        .expect("query corrected score");
    assert!(current.iter().any(|hap| hap.value.show().contains("g4")));
    assert!(
        current.iter().any(|hap| hap.value.show() == "1"),
        "completed score side effects did not survive refusal: {current:?}"
    );
}

/// A live score save refused for a CPU deadline inside a squeezed slice keeps
/// its identity, so the unchanged bytes install on the next poll.
#[test]
#[cfg(feature = "device-audio")]
fn watched_score_deadline_inside_a_squeezed_slice_retries_the_same_bytes() {
    use rustel_runtime::LiveFileProducer;

    const DEBOUNCE: Duration = Duration::from_millis(10);
    let temp = TempDir::new();
    let score = temp.join("squeezed-score.strudel");
    let initial = "note('c4')";
    write(&score, initial);
    let mut session = Session::new().expect("session");
    session.evaluate(initial).expect("initial score");
    let generation = session.generation();
    let mut producer = LiveFileProducer::from_loaded_sources_with_prebake_floor(
        &score,
        WatchLanguage::JavaScript,
        initial,
        None,
        DEBOUNCE,
        Duration::from_millis(1),
        Duration::from_millis(1),
    )
    .expect("producer");
    producer
        .step(
            &mut session,
            Duration::ZERO,
            0.0,
            48_000,
            |_, _, _| {},
            |_| true,
        )
        .expect("fill initial horizon and measure continuation");

    // Construction needs more than the ~0.1 s slice left at 0.4 s of a 0.5 s
    // horizon and less than the ~0.5 s a refilled horizon grants.
    write(
        &score,
        "const until = Date.now() + 300; while (Date.now() < until) {}; note('g4')",
    );
    assert_eq!(
        producer
            .step(
                &mut session,
                Duration::from_millis(10),
                0.0,
                48_000,
                |_, _, _| panic!("an unsettled score cut generations"),
                |_| true,
            )
            .expect("observe heavy save")
            .watch,
        WatchPoll::Pending
    );
    let refused = producer
        .step(
            &mut session,
            Duration::from_millis(20),
            0.40,
            48_000,
            |_, _, _| panic!("a capacity-refused score cut generations"),
            |_| true,
        )
        .expect("attempt heavy save against the drained horizon");
    let event = match refused.watch {
        WatchPoll::Event(event) => event,
        other => panic!("a squeezed slice did not report its deadline: {other:?}"),
    };
    assert_eq!(event.status, ReloadStatus::Rejected);
    assert_eq!(event.error_kind.as_deref(), Some("resource-limit"));
    assert_eq!(event.generation_before, generation);
    assert_eq!(event.generation_after, generation);
    assert_eq!(session.generation(), generation);
    assert!(shown(&session).contains("c4"));

    // The same bytes install under the horizon the refused step refilled.
    let mut published = Vec::new();
    let installed = producer
        .step(
            &mut session,
            Duration::from_millis(21),
            0.401,
            48_000,
            |next, _, _| published.push(next),
            |_| true,
        )
        .expect("retry the same bytes under the refilled horizon");
    assert!(matches!(
        installed.watch,
        WatchPoll::Event(ref event) if event.status == ReloadStatus::Installed
    ));
    assert_eq!(published, [generation + 1]);
    assert!(shown(&session).contains("g4"));
    assert_eq!(
        producer
            .step(
                &mut session,
                Duration::from_millis(22),
                0.402,
                48_000,
                |_, _, _| panic!("the retried identity installed twice"),
                |_| true,
            )
            .expect("poll installed identity")
            .watch,
        WatchPoll::Unchanged
    );
}

/// A live score that exceeds every slice is refused twice, then its identity
/// is final and later polls do not evaluate it again.
#[test]
#[cfg(feature = "device-audio")]
fn watched_score_capacity_refusal_settles_after_one_transient_retry() {
    use rustel_runtime::LiveFileProducer;

    const DEBOUNCE: Duration = Duration::from_millis(10);
    let temp = TempDir::new();
    let score = temp.join("settling-score.strudel");
    let initial = "note('c4')";
    write(&score, initial);
    let mut session = Session::new().expect("session");
    session.evaluate(initial).expect("initial score");
    let generation = session.generation();
    let mut producer = LiveFileProducer::from_loaded_sources_with_prebake_floor(
        &score,
        WatchLanguage::JavaScript,
        initial,
        None,
        DEBOUNCE,
        Duration::from_millis(1),
        Duration::from_millis(1),
    )
    .expect("producer");
    producer
        .step(
            &mut session,
            Duration::ZERO,
            0.0,
            48_000,
            |_, _, _| {},
            |_| true,
        )
        .expect("fill initial horizon and measure continuation");

    write(&score, "while (true) {}");
    assert_eq!(
        producer
            .step(
                &mut session,
                Duration::from_millis(10),
                0.0,
                48_000,
                |_, _, _| panic!("an unsettled score cut generations"),
                |_| true,
            )
            .expect("observe runaway save")
            .watch,
        WatchPoll::Pending
    );
    for (observed_at, clock, label) in [
        (Duration::from_millis(20), 0.40, "transient"),
        (Duration::from_millis(21), 0.401, "final"),
    ] {
        let refused = producer
            .step(
                &mut session,
                observed_at,
                clock,
                48_000,
                |_, _, _| panic!("a refused score cut generations"),
                |_| true,
            )
            .expect("attempt runaway save");
        let event = match refused.watch {
            WatchPoll::Event(event) => event,
            other => panic!("the {label} refusal was not reported: {other:?}"),
        };
        assert_eq!(event.status, ReloadStatus::Rejected, "{label}");
        assert_eq!(
            event.error_kind.as_deref(),
            Some("resource-limit"),
            "{label}"
        );
        assert_eq!(session.generation(), generation, "{label}");
    }
    assert_eq!(
        producer
            .step(
                &mut session,
                Duration::from_millis(22),
                0.402,
                48_000,
                |_, _, _| panic!("a retired identity cut generations"),
                |_| true,
            )
            .expect("poll after the final refusal")
            .watch,
        WatchPoll::Unchanged,
        "a score too heavy for a full slice was re-evaluated a third time"
    );
    assert!(shown(&session).contains("c4"));
}

/// A live setup save refused for a CPU deadline inside a squeezed slice keeps
/// its identity, so the unchanged bytes install on the next poll.
#[test]
#[cfg(feature = "device-audio")]
fn watched_setup_deadline_inside_a_squeezed_slice_retries_the_same_bytes() {
    use rustel_runtime::LiveFileProducer;

    const DEBOUNCE: Duration = Duration::from_millis(10);
    let temp = TempDir::new();
    let score = temp.join("squeezed-setup-score.strudel");
    let prebake = temp.join("squeezed-setup.js");
    let initial_score = "note('c4')";
    let initial_setup = "globalThis.squeezedSetup = 0;";
    write(&score, initial_score);
    write(&prebake, initial_setup);
    let mut session = Session::new().expect("session");
    session
        .evaluate_prebake(initial_setup)
        .expect("initial setup");
    session.evaluate(initial_score).expect("initial score");
    let generation = session.generation();
    let mut producer = LiveFileProducer::from_loaded_sources_with_prebake_floor(
        &score,
        WatchLanguage::JavaScript,
        initial_score,
        Some((prebake.clone(), initial_setup.to_string())),
        DEBOUNCE,
        Duration::from_millis(1),
        Duration::from_millis(1),
    )
    .expect("producer");
    producer
        .step(
            &mut session,
            Duration::ZERO,
            0.0,
            48_000,
            |_, _, _| {},
            |_| true,
        )
        .expect("fill initial horizon and measure continuation");

    // Setup needs more than the ~0.1 s slice left at 0.4 s of a 0.5 s horizon
    // and less than the ~0.5 s a refilled horizon grants.
    write(
        &prebake,
        "(() => { const until = Date.now() + 300; while (Date.now() < until) {} })(); \
         globalThis.squeezedSetup = 1;",
    );
    assert_eq!(
        producer
            .step(
                &mut session,
                Duration::from_millis(10),
                0.0,
                48_000,
                |_, _, _| panic!("an unsettled setup cut generations"),
                |_| true,
            )
            .expect("observe heavy setup")
            .prebake_watch,
        WatchPoll::Pending
    );
    let refused = producer
        .step(
            &mut session,
            Duration::from_millis(20),
            0.40,
            48_000,
            |_, _, _| panic!("a capacity-refused setup cut generations"),
            |_| true,
        )
        .expect("attempt heavy setup against the drained horizon");
    let event = match refused.prebake_watch {
        WatchPoll::Event(event) => event,
        other => panic!("a squeezed slice did not report its deadline: {other:?}"),
    };
    assert_eq!(event.target, WatchTarget::Prebake);
    assert_eq!(event.status, ReloadStatus::Rejected);
    assert_eq!(event.error_kind.as_deref(), Some("resource-limit"));

    // The same bytes install under the horizon the refused step refilled.
    let installed = producer
        .step(
            &mut session,
            Duration::from_millis(21),
            0.401,
            48_000,
            |_, _, _| panic!("a setup install cut generations"),
            |_| true,
        )
        .expect("retry the same setup under the refilled horizon");
    assert!(
        matches!(
            installed.prebake_watch,
            WatchPoll::Event(ref event) if event.status == ReloadStatus::Installed
        ),
        "the refused setup was not retried: {:?}",
        installed.prebake_watch
    );
    assert_eq!(session.generation(), generation);
    assert_eq!(
        producer
            .step(
                &mut session,
                Duration::from_millis(22),
                0.402,
                48_000,
                |_, _, _| panic!("the retried setup cut generations"),
                |_| true,
            )
            .expect("poll installed setup")
            .prebake_watch,
        WatchPoll::Unchanged
    );
}

/// A changed score refused for capacity in the slice a retried setup leaves
/// keeps its retry, so the unchanged bytes install on the next poll.
#[test]
#[cfg(feature = "device-audio")]
fn watched_score_refused_after_a_retried_setup_retries_the_same_bytes() {
    use std::cell::Cell;

    use rustel_runtime::LiveFileProducer;

    const DEBOUNCE: Duration = Duration::from_millis(10);
    let temp = TempDir::new();
    let score = temp.join("setup-retry-score.strudel");
    let prebake = temp.join("setup-retry.js");
    let initial_score = "note('c4')";
    let initial_setup = "globalThis.setupRetry = 0;";
    write(&score, initial_score);
    write(&prebake, initial_setup);
    let mut session = Session::new().expect("session");
    session
        .evaluate_prebake(initial_setup)
        .expect("initial setup");
    session.evaluate(initial_score).expect("initial score");
    let generation = session.generation();
    let mut producer = LiveFileProducer::from_loaded_sources_with_prebake_floor(
        &score,
        WatchLanguage::JavaScript,
        initial_score,
        Some((prebake.clone(), initial_setup.to_string())),
        DEBOUNCE,
        Duration::from_millis(1),
        Duration::from_millis(1),
    )
    .expect("producer");
    producer
        .step(
            &mut session,
            Duration::ZERO,
            0.0,
            48_000,
            |_, _, _| {},
            |_| true,
        )
        .expect("fill initial horizon and measure continuation");

    // Setup (~300 ms) and score (~250 ms) each fit a refilled ~0.5 s horizon;
    // neither fits the ~0.1 s left at 0.4 s, and the score does not fit the
    // ~0.2 s the setup leaves.
    write(
        &prebake,
        "(() => { const until = Date.now() + 300; while (Date.now() < until) {} })(); \
         globalThis.setupRetry = 1;",
    );
    write(
        &score,
        "const until = Date.now() + 250; while (Date.now() < until) {}; note('g4')",
    );
    producer
        .step(
            &mut session,
            Duration::from_millis(10),
            0.0,
            48_000,
            |_, _, _| panic!("unsettled saves cut generations"),
            |_| true,
        )
        .expect("observe both saves");
    let refused = producer
        .step(
            &mut session,
            Duration::from_millis(20),
            0.40,
            48_000,
            |_, _, _| panic!("a capacity-refused setup cut generations"),
            |_| true,
        )
        .expect("attempt heavy setup against the drained horizon");
    assert!(
        matches!(
            refused.prebake_watch,
            WatchPoll::Event(ref event) if event.error_kind.as_deref() == Some("resource-limit")
        ),
        "the setup was not refused for capacity: {:?}",
        refused.prebake_watch
    );

    // The setup retry installs from 0.401 and leaves the score the clock at
    // 0.70, where the score is refused for capacity in turn.
    let clock_calls = Cell::new(0usize);
    let squeezed = producer
        .step_with_clock(
            &mut session,
            Duration::from_millis(21),
            || {
                let call = clock_calls.get();
                clock_calls.set(call + 1);
                if call == 0 { 0.401 } else { 0.70 }
            },
            48_000,
            |_, _, _| panic!("a capacity-refused score cut generations"),
            |_| true,
        )
        .expect("retry setup and attempt score in the slice it leaves");
    assert!(
        matches!(
            squeezed.prebake_watch,
            WatchPoll::Event(ref event) if event.status == ReloadStatus::Installed
        ),
        "the refused setup was not retried: {:?}",
        squeezed.prebake_watch
    );
    let event = match squeezed.watch {
        WatchPoll::Event(event) => event,
        other => panic!("the squeezed score did not report its deadline: {other:?}"),
    };
    assert_eq!(event.status, ReloadStatus::Rejected);
    assert_eq!(event.error_kind.as_deref(), Some("resource-limit"));
    assert_eq!(session.generation(), generation);

    // The same score bytes install under the horizon the refused step
    // refilled, with no further edit.
    let mut published = Vec::new();
    let installed = producer
        .step(
            &mut session,
            Duration::from_millis(22),
            0.701,
            48_000,
            |next, _, _| published.push(next),
            |_| true,
        )
        .expect("retry the same score under the refilled horizon");
    assert!(
        matches!(
            installed.watch,
            WatchPoll::Event(ref event) if event.status == ReloadStatus::Installed
        ),
        "the capacity-refused score was not retried: {:?}",
        installed.watch
    );
    assert_eq!(published, [generation + 1]);
    assert!(shown(&session).contains("g4"));
}

#[test]
#[cfg(feature = "device-audio")]
fn product_step_resamples_the_audio_clock_after_setup_before_score_cutover() {
    use std::cell::Cell;

    use rustel_runtime::LiveFileProducer;

    const DEBOUNCE: Duration = Duration::from_millis(10);
    const SAMPLE_RATE: u32 = 48_000;
    let temp = TempDir::new();
    let score = temp.join("fresh-clock-score.strudel");
    let prebake = temp.join("fresh-clock-setup.js");
    let initial_setup = "globalThis.clockHelper = () => note('c4').fast(16);";
    let initial_score = "clockHelper()";
    write(&prebake, initial_setup);
    write(&score, initial_score);
    let mut session = Session::new().expect("session");
    session
        .evaluate_prebake(initial_setup)
        .expect("initial setup");
    session.evaluate(initial_score).expect("initial score");
    let old_generation = session.generation();
    let mut producer = LiveFileProducer::from_loaded_sources_with_prebake_floor(
        &score,
        WatchLanguage::JavaScript,
        initial_score,
        Some((prebake.clone(), initial_setup.to_string())),
        DEBOUNCE,
        Duration::from_millis(1),
        Duration::from_millis(10),
    )
    .expect("producer");
    producer
        .step(
            &mut session,
            Duration::ZERO,
            0.0,
            SAMPLE_RATE,
            |_, _, _| {},
            |_| true,
        )
        .expect("fill initial horizon");
    write(
        &prebake,
        "globalThis.clockHelper = () => note('g4').fast(16);",
    );
    write(&score, "clockHelper() // changed with setup");
    producer
        .step(
            &mut session,
            Duration::from_millis(100),
            0.05,
            SAMPLE_RATE,
            |_, _, _| panic!("pending edit cut generations"),
            |_| true,
        )
        .expect("observe simultaneous edit");

    let clock_calls = Cell::new(0usize);
    let mut pushed = Vec::new();
    let step = producer
        .step_with_clock(
            &mut session,
            Duration::from_millis(120),
            || {
                let call = clock_calls.get();
                clock_calls.set(call + 1);
                match call {
                    0 => 0.1,  // setup budget begins
                    1 => 0.3,  // score budget begins after setup
                    2 => 0.35, // score anchors after its own evaluation
                    3 => 0.35, // scheduling samples again
                    4 => 0.35, // prefill is still fresh before publication
                    _ => panic!("unexpected product clock read"),
                }
            },
            SAMPLE_RATE,
            |_, _, _| {},
            |event| {
                pushed.push(event);
                true
            },
        )
        .expect("install with fresh product clock");
    assert!(matches!(
        step.prebake_watch,
        WatchPoll::Event(ref event) if event.status == ReloadStatus::Installed
    ));
    assert!(matches!(
        step.watch,
        WatchPoll::Event(ref event) if event.status == ReloadStatus::Installed
    ));
    assert_eq!(
        clock_calls.get(),
        5,
        "product did not sample setup, score-budget, score-anchor, scheduling and publication boundaries"
    );
    let generation = old_generation + 1;
    let first = pushed
        .iter()
        .filter(|event| event.generation == generation)
        .map(|event| event.target_frame)
        .min()
        .expect("replacement produced no event");
    assert!(
        first >= (0.35 * f64::from(SAMPLE_RATE)) as u64,
        "replacement was anchored before score evaluation completed: frame {first}"
    );
}

#[test]
#[cfg(feature = "device-audio")]
fn live_producer_repeated_edit_soak_keeps_one_heap_and_exact_generations() {
    use std::collections::HashSet;

    use rustel_runtime::LiveFileProducer;

    const EDITS: usize = 64;
    const DEBOUNCE: Duration = Duration::from_millis(2);
    const WATCH_POLL: Duration = Duration::from_millis(1);
    const SAMPLE_RATE: u32 = 48_000;

    fn source(identity: usize, callback: bool) -> String {
        let transform = if callback {
            ".every(fastcat(1, 1), pattern => pattern.fast(2))"
        } else {
            ""
        };
        format!(
            "// soak identity {identity}\n\
             globalThis.liveSoakCount = (globalThis.liveSoakCount ?? 0) + 1;\n\
             note('c4').gain(globalThis.liveSoakCount).fast(8){transform}"
        )
    }

    let temp = TempDir::new();
    let score = temp.join("soak.strudel");
    let initial = source(0, false);
    write(&score, &initial);

    let mut session = Session::new().expect("session");
    session.evaluate(&initial).expect("initial evaluation");
    let generation = AtomicU64::new(session.generation());
    let mut producer =
        LiveFileProducer::new(&score, WatchLanguage::JavaScript, DEBOUNCE, WATCH_POLL)
            .expect("live producer");
    let mut observed = Duration::ZERO;
    let mut schedule_now = 0.0;
    let mut cutovers = Vec::new();
    let mut pushed = Vec::new();

    let mut step = |producer: &mut LiveFileProducer,
                    session: &mut Session,
                    observed_at: Duration,
                    schedule_at: f64| {
        producer
            .step(
                session,
                observed_at,
                schedule_at,
                SAMPLE_RATE,
                |next, _, _| {
                    generation.store(next, Ordering::Release);
                    cutovers.push(next);
                },
                |event| {
                    assert_eq!(
                        event.generation,
                        generation.load(Ordering::Acquire),
                        "replacement crossed before its generation was published"
                    );
                    pushed.push(event);
                    true
                },
            )
            .expect("advance repeated-edit producer")
    };

    assert_eq!(
        step(&mut producer, &mut session, observed, schedule_now).watch,
        WatchPoll::Unchanged,
        "constructing a watcher must treat the loaded score as its baseline"
    );
    let mut expected_generation = session.generation();
    let mut valid_evaluations = 1usize;
    let mut valid_edits = 0usize;
    let mut rejected_edits = 0usize;

    for identity in 1..=EDITS {
        observed += Duration::from_millis(10);
        schedule_now += 0.025;
        let rejected = identity % 5 == 0;
        let callback = identity % 2 == 0;
        if rejected {
            write(
                &score,
                &format!("// invalid soak identity {identity}\nnote("),
            );
        } else {
            write(&score, &source(identity, callback));
        }

        assert_eq!(
            step(&mut producer, &mut session, observed, schedule_now).watch,
            WatchPoll::Pending,
            "edit {identity} skipped the stable-file debounce"
        );
        observed += Duration::from_millis(3);
        let settled = step(&mut producer, &mut session, observed, schedule_now);
        let WatchPoll::Event(event) = settled.watch else {
            panic!("edit {identity} did not settle: {settled:?}");
        };
        assert_eq!(
            producer.pending(),
            0,
            "unbounded producer backlog at edit {identity}"
        );

        if rejected {
            rejected_edits += 1;
            assert_eq!(event.status, ReloadStatus::Rejected);
            assert_eq!(event.generation_before, expected_generation);
            assert_eq!(event.generation_after, expected_generation);
            assert_eq!(session.generation(), expected_generation);
        } else {
            valid_edits += 1;
            valid_evaluations += 1;
            expected_generation += 1;
            assert_eq!(event.status, ReloadStatus::Installed);
            assert_eq!(event.generation_after, expected_generation);
            assert_eq!(session.generation(), expected_generation);
            assert_eq!(session.active_needs_host(), callback);
        }

        observed += Duration::from_millis(2);
        assert_eq!(
            step(&mut producer, &mut session, observed, schedule_now).watch,
            WatchPoll::Unchanged,
            "one stable identity was attempted more than once at edit {identity}"
        );
    }
    assert_eq!(valid_edits, 52);
    assert_eq!(rejected_edits, 12);
    assert_eq!(cutovers.len(), valid_edits);
    assert_eq!(cutovers.last().copied(), Some(expected_generation));
    assert_eq!(generation.load(Ordering::Acquire), expected_generation);
    assert_eq!(session.generation(), expected_generation);
    let expected_gain = format!("gain:{valid_evaluations}");
    let current = session
        .query(Fraction::ZERO, Fraction::ONE)
        .expect("query after repeated edits");
    assert!(
        current
            .iter()
            .all(|hap| hap.value.show().contains(&expected_gain)),
        "the same heap did not retain the exact successful-evaluation count: {current:?}"
    );
    let unique: HashSet<u64> = pushed.iter().map(|event| event.onset_id).collect();
    assert_eq!(
        unique.len(),
        pushed.len(),
        "repeated edits duplicated onset ids"
    );

    session.transport().stop();
    observed += Duration::from_millis(2);
    let stopped = producer
        .step(
            &mut session,
            observed,
            schedule_now,
            SAMPLE_RATE,
            |_, _, _| panic!("Stop installed a generation"),
            |_| panic!("Stop pushed an event"),
        )
        .expect("Stop repeated-edit producer");
    assert_eq!(stopped.watch, WatchPoll::Stopped);
    assert_eq!(stopped.pending, 0);
}

#[cfg(feature = "device-audio")]
#[derive(Default)]
struct LiveProducerLog {
    cutovers: Vec<u64>,
    pushed: Vec<rustel_audio::AudioEvent>,
}

#[cfg(feature = "device-audio")]
fn drive_live_producer(
    producer: &mut rustel_runtime::LiveFileProducer,
    session: &mut Session,
    observed_at: Duration,
    schedule_now: f64,
    generation: &std::sync::atomic::AtomicU64,
    ring: &rustel_audio::Ring,
    log: &mut LiveProducerLog,
) -> rustel_runtime::LiveProducerStep {
    let LiveProducerLog { cutovers, pushed } = log;
    producer
        .step(
            session,
            observed_at,
            schedule_now,
            48_000,
            |next, _, _| {
                generation.store(next, std::sync::atomic::Ordering::Release);
                cutovers.push(next);
            },
            |event| {
                assert_eq!(
                    event.generation,
                    generation.load(std::sync::atomic::Ordering::Acquire),
                    "producer pushed a replacement before publishing its generation"
                );
                assert!(event.confirmation.is_none(), "raw-ring fixture is unbound");
                if ring.push(event.event) {
                    pushed.push(event.event);
                    true
                } else {
                    false
                }
            },
        )
        .expect("drive live producer")
}

#[test]
#[cfg(feature = "device-audio")]
fn assembled_live_route_survives_two_valid_saves_one_invalid_and_backpressure() {
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, AtomicU64};

    use rustel_audio::LiveScalarBackend;
    use rustel_runtime::{LiveFileProducer, WatchPoll};

    const DEBOUNCE: Duration = Duration::from_millis(10);
    const WATCH_POLL: Duration = Duration::from_millis(1);
    const SAMPLE_RATE: u32 = 48_000;

    let source = |label: &str, callback: bool| {
        let transform = if callback {
            ".every(fastcat(1, 1), pattern => pattern.fast(16))"
        } else {
            ".fast(16)"
        };
        format!(
            "// {label}\n\
             globalThis.liveReloads = (globalThis.liveReloads ?? 0) + 1;\n\
             note(globalThis.liveReloads === 1 ? 'c4' : \
                  globalThis.liveReloads === 2 ? 'e4' : 'g4'){transform}"
        )
    };
    let temp = TempDir::new();
    let score = temp.join("assembled.strudel");
    let initial = source("initial", false);
    write(&score, &initial);

    let mut session = Session::new().expect("session");
    session.evaluate(&initial).expect("initial evaluation");
    assert!(
        !session.active_needs_host(),
        "pure initial graph became impure"
    );
    let initial_generation = session.generation();
    let mut producer =
        LiveFileProducer::new(&score, WatchLanguage::JavaScript, DEBOUNCE, WATCH_POLL)
            .expect("live producer");
    // Deliberately tiny: the first horizon cannot fit, so the production
    // producer must retain exactly one bounded batch and retry it.
    let ring = rustel_audio::Ring::new(2);
    let generation = AtomicU64::new(initial_generation);
    let takeover = AtomicU64::new(0);
    // Every flip here is an edit's: the cut word stays zero.
    let takeover_cut = AtomicU64::new(0);
    let stopped = AtomicBool::new(false);
    let line_arm = AtomicU64::new(0);
    let mut backend = LiveScalarBackend::new(SAMPLE_RATE, 64).expect("live backend");
    let mut log = LiveProducerLog::default();

    let first = drive_live_producer(
        &mut producer,
        &mut session,
        Duration::ZERO,
        0.0,
        &generation,
        &ring,
        &mut log,
    );
    assert!(first.scheduled > ring.capacity());
    assert!(first.backpressured);
    assert_eq!(first.pending, producer.pending());
    assert!(first.pending <= first.scheduled);
    assert_eq!(
        first.pushed + first.pending,
        first.scheduled,
        "ring back-pressure lost or invented an onset"
    );

    // The consumer admits ahead: it accepts every ringed onset within the
    // admit horizon. The due onset sounds now. The future onset waits in
    // pending, where the cutover must retire it.
    let mut pcm = [0.0f32; 128 * 2];
    let initial_block = backend.process_block_with(
        &mut pcm,
        128,
        0,
        &ring,
        rustel_audio::LiveFlipAtomics {
            generation: &generation,
            takeover_frame: &takeover,
            takeover_cut: &takeover_cut,
            line_arm: &line_arm,
        },
        &stopped,
    );
    assert_eq!(initial_block.accepted, first.pushed);
    assert!(pcm.iter().any(|sample| sample.abs() > 1e-6));

    // First valid save. The pending observation can keep feeding the old
    // generation. Installation must clear that producer backlog, publish the
    // new generation first, and leave already-ringed old events to filtering.
    let second = source("second", true);
    write(&score, &second);
    assert_eq!(
        drive_live_producer(
            &mut producer,
            &mut session,
            Duration::from_millis(100),
            0.1,
            &generation,
            &ring,
            &mut log,
        )
        .watch,
        WatchPoll::Pending
    );
    let stable_second = drive_live_producer(
        &mut producer,
        &mut session,
        Duration::from_millis(120),
        0.1,
        &generation,
        &ring,
        &mut log,
    );
    assert_eq!(stable_second.watch, WatchPoll::Pending);
    assert!(log.cutovers.is_empty());

    // The score is stable, but the producer still owns old-generation events
    // that have not crossed the fixed ring. Drain and hand those exact events
    // off first. Every step whose ENTRY backlog is nonempty must retain the
    // score identity rather than evaluating it and clearing the backlog.
    let mut observed_ms = 121u64;
    let mut flush_steps = 0usize;
    while producer.pending() > 0 {
        while ring.pop().is_some() {}
        let flushed = drive_live_producer(
            &mut producer,
            &mut session,
            Duration::from_millis(observed_ms),
            0.1,
            &generation,
            &ring,
            &mut log,
        );
        assert_eq!(flushed.watch, WatchPoll::Pending);
        assert!(log.cutovers.is_empty());
        observed_ms += 1;
        flush_steps += 1;
        assert!(
            flush_steps <= first.scheduled + 1,
            "old producer backlog did not make bounded progress"
        );
    }

    // The last flush leaves old-generation events in the ring but none in the
    // producer backlog. The unchanged score identity can now install; those
    // already-ringed events remain the consumer's stale-generation witness.
    let installed_second = drive_live_producer(
        &mut producer,
        &mut session,
        Duration::from_millis(observed_ms),
        0.1,
        &generation,
        &ring,
        &mut log,
    );
    let WatchPoll::Event(second_event) = installed_second.watch else {
        panic!("second save was not installed");
    };
    assert_eq!(second_event.status, ReloadStatus::Installed);
    let second_generation = second_event.generation_after;
    assert!(
        session.active_needs_host(),
        "callback-bearing replacement was classified pure"
    );
    assert_eq!(
        generation.load(std::sync::atomic::Ordering::Acquire),
        second_generation
    );

    let stale = backend.process_block_with(
        &mut pcm,
        128,
        (0.1 * f64::from(SAMPLE_RATE)) as u64,
        &ring,
        rustel_audio::LiveFlipAtomics {
            generation: &generation,
            takeover_frame: &takeover,
            takeover_cut: &takeover_cut,
            line_arm: &line_arm,
        },
        &stopped,
    );
    assert!(stale.stale > 0, "ringed old generation was not filtered");
    let _ = drive_live_producer(
        &mut producer,
        &mut session,
        Duration::from_millis(observed_ms + 1),
        0.1,
        &generation,
        &ring,
        &mut log,
    );
    let replacement_target = log
        .pushed
        .iter()
        .find(|event| event.generation == second_generation)
        .expect("replacement event never crossed the ring")
        .target_frame;
    // Ahead-admission accepts a ringed onset in the first block that sees
    // it (so sidechains arm on time); reaching the scheduled frame is then
    // proven by AUDIBILITY at that frame, not by which block accepted it.
    let mut frame = (0.1 * f64::from(SAMPLE_RATE)) as u64 + 128;
    let mut replacement_accepted = stale.accepted;
    let mut replacement_audible = false;
    while frame <= replacement_target {
        pcm.fill(0.0);
        let report = backend.process_block_with(
            &mut pcm,
            128,
            frame,
            &ring,
            rustel_audio::LiveFlipAtomics {
                generation: &generation,
                takeover_frame: &takeover,
                takeover_cut: &takeover_cut,
                line_arm: &line_arm,
            },
            &stopped,
        );
        replacement_accepted += report.accepted;
        replacement_audible |= pcm.iter().any(|sample| sample.abs() > 1e-6);
        frame += 128;
    }
    assert!(
        replacement_accepted >= 1,
        "replacement was never admitted: pushed={:?}, stale={stale:?}",
        log.pushed
    );
    assert!(
        replacement_audible,
        "replacement never sounded by its scheduled frame"
    );

    // Keep the same producer and callback advancing toward the invalid save so
    // the assertions cover one continuous stream.
    let invalid_frame = (0.2 * f64::from(SAMPLE_RATE)) as u64;
    while frame < invalid_frame {
        let _ = drive_live_producer(
            &mut producer,
            &mut session,
            Duration::from_millis(150),
            frame as f64 / f64::from(SAMPLE_RATE),
            &generation,
            &ring,
            &mut log,
        );
        backend.process_block_with(
            &mut pcm,
            128,
            frame,
            &ring,
            rustel_audio::LiveFlipAtomics {
                generation: &generation,
                takeover_frame: &takeover,
                takeover_cut: &takeover_cut,
                line_arm: &line_arm,
            },
            &stopped,
        );
        frame += 128;
    }

    // A stable malformed save is one reported rejection. It cannot bump the
    // generation, call the cutover hook, or silence the last-known-good graph.
    write(&score, "note(");
    let _ = drive_live_producer(
        &mut producer,
        &mut session,
        Duration::from_millis(200),
        frame as f64 / f64::from(SAMPLE_RATE),
        &generation,
        &ring,
        &mut log,
    );
    let before_rejection_pushes = log.pushed.len();
    let rejected = drive_live_producer(
        &mut producer,
        &mut session,
        Duration::from_millis(220),
        frame as f64 / f64::from(SAMPLE_RATE),
        &generation,
        &ring,
        &mut log,
    );
    // The consumer's ahead-admission usually keeps the ring drained, so the
    // rejection may surface immediately; a still-backpressured producer
    // reports Pending first and must flush its retained backlog with
    // bounded progress before the same single rejection appears.
    let mut reject_observed_ms = 221u64;
    let rejected_event = if rejected.watch == WatchPoll::Pending {
        let mut reject_flush_steps = 0usize;
        while producer.pending() > 0 {
            while ring.pop().is_some() {}
            let flushed = drive_live_producer(
                &mut producer,
                &mut session,
                Duration::from_millis(reject_observed_ms),
                frame as f64 / f64::from(SAMPLE_RATE),
                &generation,
                &ring,
                &mut log,
            );
            assert_eq!(flushed.watch, WatchPoll::Pending);
            assert_eq!(log.cutovers, vec![second_generation]);
            reject_observed_ms += 1;
            reject_flush_steps += 1;
            assert!(
                reject_flush_steps <= first.scheduled + 1,
                "pre-rejection backlog did not make bounded progress"
            );
        }
        let rejected = drive_live_producer(
            &mut producer,
            &mut session,
            Duration::from_millis(reject_observed_ms),
            frame as f64 / f64::from(SAMPLE_RATE),
            &generation,
            &ring,
            &mut log,
        );
        let WatchPoll::Event(rejected_event) = rejected.watch else {
            panic!("malformed save was not reported");
        };
        rejected_event
    } else {
        let WatchPoll::Event(rejected_event) = rejected.watch else {
            panic!("malformed save was neither pending nor reported");
        };
        rejected_event
    };
    assert_eq!(rejected_event.status, ReloadStatus::Rejected);
    assert_eq!(session.generation(), second_generation);
    assert_eq!(log.cutovers, vec![second_generation]);
    assert!(
        log.pushed[before_rejection_pushes..]
            .iter()
            .all(|event| event.generation == second_generation),
        "rejected save emitted a partial generation"
    );
    let current = session
        .query(Fraction::ZERO, Fraction::ONE)
        .expect("last-known-good query after malformed save");
    assert!(
        current.iter().any(|hap| hap.value.show().contains("e4")),
        "malformed save did not retain the E4 graph: {current:?}"
    );

    let third_frame = (0.3 * f64::from(SAMPLE_RATE)) as u64;
    let mut audible_after_rejection = false;
    while frame < third_frame {
        let _ = drive_live_producer(
            &mut producer,
            &mut session,
            Duration::from_millis(250),
            frame as f64 / f64::from(SAMPLE_RATE),
            &generation,
            &ring,
            &mut log,
        );
        pcm.fill(0.0);
        backend.process_block_with(
            &mut pcm,
            128,
            frame,
            &ring,
            rustel_audio::LiveFlipAtomics {
                generation: &generation,
                takeover_frame: &takeover,
                takeover_cut: &takeover_cut,
                line_arm: &line_arm,
            },
            &stopped,
        );
        audible_after_rejection |= pcm.iter().any(|sample| sample.abs() > 1e-6);
        frame += 128;
    }
    assert!(
        audible_after_rejection,
        "rejected save silenced the last-known-good callback graph"
    );

    // Second valid save proves the heap survives (liveReloads reaches 3) and
    // that one stable identity still means exactly one generation.
    let third = source("third", false);
    write(&score, &third);
    let _ = drive_live_producer(
        &mut producer,
        &mut session,
        Duration::from_millis(300),
        frame as f64 / f64::from(SAMPLE_RATE),
        &generation,
        &ring,
        &mut log,
    );
    let stable_third = drive_live_producer(
        &mut producer,
        &mut session,
        Duration::from_millis(320),
        frame as f64 / f64::from(SAMPLE_RATE),
        &generation,
        &ring,
        &mut log,
    );
    let mut third_observed_ms = 321u64;
    let third_step = if matches!(
        stable_third.watch,
        WatchPoll::Event(ref event) if event.status == ReloadStatus::Installed
    ) {
        stable_third
    } else {
        assert_eq!(stable_third.watch, WatchPoll::Pending);
        let mut third_flush_steps = 0usize;
        while producer.pending() > 0 {
            while ring.pop().is_some() {}
            let flushed = drive_live_producer(
                &mut producer,
                &mut session,
                Duration::from_millis(third_observed_ms),
                frame as f64 / f64::from(SAMPLE_RATE),
                &generation,
                &ring,
                &mut log,
            );
            assert_eq!(flushed.watch, WatchPoll::Pending);
            assert_eq!(log.cutovers, vec![second_generation]);
            third_observed_ms += 1;
            third_flush_steps += 1;
            assert!(
                third_flush_steps <= first.scheduled + 1,
                "pre-cutover backlog did not make bounded progress"
            );
        }
        drive_live_producer(
            &mut producer,
            &mut session,
            Duration::from_millis(third_observed_ms),
            frame as f64 / f64::from(SAMPLE_RATE),
            &generation,
            &ring,
            &mut log,
        )
    };
    let WatchPoll::Event(third_event) = third_step.watch else {
        panic!("third save was not installed");
    };
    assert_eq!(third_event.status, ReloadStatus::Installed);
    let third_generation = third_event.generation_after;
    assert!(
        !session.active_needs_host(),
        "pure third replacement retained callback-host ownership"
    );
    assert_eq!(third_generation, initial_generation + 2);
    assert_eq!(log.cutovers, vec![second_generation, third_generation]);

    // Drain ringed generation-2 work, then feed and hear generation 3 through
    // the same backend instance. No restart or offline query substitutes for
    // this output witness.
    pcm.fill(0.0);
    let cutover_block = backend.process_block_with(
        &mut pcm,
        128,
        frame,
        &ring,
        rustel_audio::LiveFlipAtomics {
            generation: &generation,
            takeover_frame: &takeover,
            takeover_cut: &takeover_cut,
            line_arm: &line_arm,
        },
        &stopped,
    );
    let mut third_accepted = cutover_block.accepted;
    let mut third_audible = pcm.iter().any(|sample| sample.abs() > 1e-6);
    frame += 128;
    let _ = drive_live_producer(
        &mut producer,
        &mut session,
        Duration::from_millis(third_observed_ms + 1),
        frame as f64 / f64::from(SAMPLE_RATE),
        &generation,
        &ring,
        &mut log,
    );
    let third_target = log
        .pushed
        .iter()
        .find(|event| event.generation == third_generation)
        .expect("third generation never crossed the ring")
        .target_frame;
    while frame <= third_target {
        pcm.fill(0.0);
        let report = backend.process_block_with(
            &mut pcm,
            128,
            frame,
            &ring,
            rustel_audio::LiveFlipAtomics {
                generation: &generation,
                takeover_frame: &takeover,
                takeover_cut: &takeover_cut,
                line_arm: &line_arm,
            },
            &stopped,
        );
        third_accepted += report.accepted;
        third_audible |= pcm.iter().any(|sample| sample.abs() > 1e-6);
        frame += 128;
    }
    assert!(third_accepted > 0, "third generation was never delivered");
    assert!(third_audible, "third generation produced silent PCM");

    let unchanged = drive_live_producer(
        &mut producer,
        &mut session,
        Duration::from_millis(340),
        frame as f64 / f64::from(SAMPLE_RATE),
        &generation,
        &ring,
        &mut log,
    );
    assert_eq!(unchanged.watch, WatchPoll::Unchanged);
    assert_eq!(log.cutovers.len(), 2, "one save installed more than once");

    let current = session
        .query(Fraction::ZERO, Fraction::ONE)
        .expect("same-heap third query");
    assert!(
        current.iter().any(|hap| hap.value.show().contains("g4")),
        "same heap did not advance liveReloads to G4: {current:?}"
    );
    for (generation, expected_hz, label) in [
        (initial_generation, 261.625_565, "C4"),
        (second_generation, 329.627_557, "E4"),
        (third_generation, 391.995_436, "G4"),
    ] {
        let events: Vec<_> = log
            .pushed
            .iter()
            .filter(|event| event.generation == generation)
            .collect();
        assert!(
            !events.is_empty(),
            "{label} generation {generation} never crossed the product ring"
        );
        assert!(
            events
                .iter()
                .all(|event| (f64::from(event.freq_hz) - expected_hz).abs() < 0.01),
            "{label} generation carried stale/wrong pitch events: {events:?}"
        );
    }
    let unique: HashSet<u64> = log.pushed.iter().map(|event| event.onset_id).collect();
    assert_eq!(
        unique.len(),
        log.pushed.len(),
        "producer duplicated an onset id"
    );

    session.transport().stop();
    let stopped_step = drive_live_producer(
        &mut producer,
        &mut session,
        Duration::from_millis(400),
        frame as f64 / f64::from(SAMPLE_RATE),
        &generation,
        &ring,
        &mut log,
    );
    assert_eq!(stopped_step.watch, WatchPoll::Stopped);
    assert_eq!(stopped_step.pending, 0);
}

#[cfg(feature = "device-audio")]
#[derive(Debug)]
struct BacklogClockOutcome {
    initial_scheduled: usize,
    retained: usize,
    blocked_scheduled: usize,
    blocked_pending: usize,
    flush_scheduled: usize,
    flush_pending: usize,
    blocked_clock_calls: usize,
    flush_clock_calls: usize,
    pushed: Vec<rustel_audio::AudioEvent>,
}

#[cfg(feature = "device-audio")]
fn exercise_post_backlog_clock(watched: bool) -> BacklogClockOutcome {
    use std::cell::Cell;

    use rustel_runtime::LiveFileProducer;

    const SAMPLE_RATE: u32 = 48_000;
    let temp = TempDir::new();
    let score = temp.join(if watched {
        "fresh-backlog-watched.strudel"
    } else {
        "fresh-backlog-unwatched.strudel"
    });
    let source = "note('c4').fast(16)";
    write(&score, source);
    let mut session = Session::new().expect("session");
    session.evaluate(source).expect("initial score");
    let mut producer = LiveFileProducer::from_loaded_sources_with_prebake_floor(
        &score,
        WatchLanguage::JavaScript,
        source,
        None,
        Duration::ZERO,
        Duration::from_millis(10),
        Duration::from_millis(1),
    )
    .expect("producer");

    let initial = if watched {
        producer.step_with_clock(
            &mut session,
            Duration::ZERO,
            || 0.0,
            SAMPLE_RATE,
            |_, _, _| panic!("unchanged initial score published a generation"),
            |_| false,
        )
    } else {
        producer.step_unwatched_with_clock(&mut session, || 0.0, SAMPLE_RATE, |_| false)
    }
    .expect("create producer backlog");
    assert_eq!(initial.pushed, 0);
    assert!(initial.scheduled > 1, "fixture scheduled no useful batch");
    assert_eq!(initial.pending, initial.scheduled);

    // A full ring means no query can make downstream progress. In particular,
    // the producer must not sample a clock that will be stale by the time the
    // retained front event finally crosses.
    let blocked_clock_calls = Cell::new(0usize);
    let blocked = if watched {
        producer.step_with_clock(
            &mut session,
            Duration::from_millis(1),
            || {
                blocked_clock_calls.set(blocked_clock_calls.get() + 1);
                0.1
            },
            SAMPLE_RATE,
            |_, _, _| panic!("unchanged blocked score published a generation"),
            |_| false,
        )
    } else {
        producer.step_unwatched_with_clock(
            &mut session,
            || {
                blocked_clock_calls.set(blocked_clock_calls.get() + 1);
                0.1
            },
            SAMPLE_RATE,
            |_| false,
        )
    }
    .expect("retain blocked batch");

    let audible_clock = Cell::new(0.1);
    let accepted = Cell::new(0usize);
    let flush_clock_calls = Cell::new(0usize);
    let mut pushed = Vec::new();
    let retained = initial.pending;
    let flush = if watched {
        producer.step_with_clock(
            &mut session,
            Duration::from_millis(2),
            || {
                flush_clock_calls.set(flush_clock_calls.get() + 1);
                audible_clock.get()
            },
            SAMPLE_RATE,
            |_, _, _| panic!("unchanged flushed score published a generation"),
            |event| {
                assert!(event.confirmation.is_none(), "backlog fixture is unbound");
                pushed.push(event.event);
                let next = accepted.get() + 1;
                accepted.set(next);
                if next == retained {
                    // The device advances while the retained batch crosses.
                    // Scheduling must sample AFTER this update.
                    audible_clock.set(0.6);
                }
                true
            },
        )
    } else {
        producer.step_unwatched_with_clock(
            &mut session,
            || {
                flush_clock_calls.set(flush_clock_calls.get() + 1);
                audible_clock.get()
            },
            SAMPLE_RATE,
            |event| {
                assert!(event.confirmation.is_none(), "backlog fixture is unbound");
                pushed.push(event.event);
                let next = accepted.get() + 1;
                accepted.set(next);
                if next == retained {
                    audible_clock.set(0.6);
                }
                true
            },
        )
    }
    .expect("flush and refill from fresh clock");

    BacklogClockOutcome {
        initial_scheduled: initial.scheduled,
        retained,
        blocked_scheduled: blocked.scheduled,
        blocked_pending: blocked.pending,
        flush_scheduled: flush.scheduled,
        flush_pending: flush.pending,
        blocked_clock_calls: blocked_clock_calls.get(),
        flush_clock_calls: flush_clock_calls.get(),
        pushed,
    }
}

#[test]
#[cfg(feature = "device-audio")]
fn watched_and_unwatched_refill_from_the_fresh_post_backlog_clock() {
    const SAMPLE_RATE: u32 = 48_000;
    let watched = exercise_post_backlog_clock(true);
    let unwatched = exercise_post_backlog_clock(false);

    for outcome in [&watched, &unwatched] {
        assert_eq!(outcome.blocked_clock_calls, 0);
        assert_eq!(outcome.blocked_scheduled, 0);
        assert_eq!(outcome.blocked_pending, outcome.retained);
        assert_eq!(outcome.flush_clock_calls, 1);
        assert!(
            outcome.flush_scheduled > 1,
            "fresh refill produced no batch"
        );
        assert_eq!(outcome.flush_pending, 0);
        assert!(
            outcome.pushed[outcome.retained..]
                .iter()
                .any(|event| event.target_frame >= 42_000),
            "refill used the stale 0.1s clock instead of the post-flush 0.6s clock: {:?}",
            &outcome.pushed[outcome.retained..]
        );
        assert!(
            outcome
                .pushed
                .iter()
                .all(|event| event.target_frame <= SAMPLE_RATE as u64 * 2),
            "fixture produced an implausible target frame"
        );
    }
    assert_eq!(watched.initial_scheduled, unwatched.initial_scheduled);
    assert_eq!(watched.retained, unwatched.retained);
    assert_eq!(watched.flush_scheduled, unwatched.flush_scheduled);
    assert_eq!(watched.pushed, unwatched.pushed);
}

#[test]
#[cfg(feature = "device-audio")]
fn initial_impure_prefill_works_once_then_steady_state_requires_real_cover() {
    use rustel_runtime::LiveFileProducer;

    let temp = TempDir::new();
    let score = temp.join("initial-query-prefill.strudel");
    let source = r#"
      new Pattern(state => note('c4').fast(8).query(state))
    "#;
    write(&score, source);
    let mut session = Session::new().expect("session");
    session.evaluate(source).expect("initial impure score");
    assert!(session.active_needs_host(), "fixture must enter QuickJS");
    let mut producer = LiveFileProducer::from_loaded_sources_with_prebake_floor(
        &score,
        WatchLanguage::JavaScript,
        source,
        None,
        Duration::ZERO,
        Duration::from_millis(1),
        Duration::from_millis(1),
    )
    .expect("producer");

    // There is no queried horizon at startup, even though the device clock is
    // already nonzero. Initial prefill is the one explicit bootstrap case.
    let first = producer
        .step_unwatched_with_clock(&mut session, || 0.3, 48_000, |_| true)
        .expect("initial impure prefill");
    assert!(first.scheduled > 0);
    assert_eq!(first.pending, 0);

    // At 1.0 s the first horizon is exhausted. The producer must still query:
    // the remaining horizon only shrinks, so a refusal here would be permanent.
    // Steady state fills under the minimum slice on this and every later step.
    let started = std::time::Instant::now();
    let steady = producer
        .step_unwatched_with_clock(&mut session, || 1.0, 48_000, |_| true)
        .expect("exhausted horizon refused the only query that can refill it");
    assert!(
        steady.scheduled > 0,
        "steady fill after an exhausted horizon scheduled nothing: {steady:?}"
    );
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "minimum-slice fill was not bounded: {:?}",
        started.elapsed()
    );

    let later = producer
        .step_unwatched_with_clock(&mut session, || 2.0, 48_000, |_| true)
        .expect("second exhausted-horizon step wedged the producer");
    assert!(
        later.scheduled > 0,
        "producer stopped filling on the step after recovery: {later:?}"
    );
}

#[cfg(feature = "device-audio")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CutoverAction {
    Set(u64),
    Push(u64),
}

#[test]
#[cfg(feature = "device-audio")]
fn watched_tempo_commit_reaches_the_consumer_and_rejection_keeps_that_clock() {
    use std::sync::atomic::{AtomicBool, AtomicU64};

    use rustel_audio::{LiveScalarBackend, Ring};
    use rustel_runtime::LiveFileProducer;

    const SAMPLE_RATE: u32 = 48_000;
    let temp = TempDir::new();
    let score = temp.join("watched-tempo.strudel");
    let initial = "setcps(1); note('c4')";
    write(&score, initial);

    let mut session = Session::new().expect("session");
    session.evaluate(initial).expect("initial score and tempo");
    assert_eq!(session.config().cps, 1.0);
    let initial_generation = session.generation();
    let active_generation = AtomicU64::new(initial_generation);
    let takeover = AtomicU64::new(0);
    // Every flip here is an edit's: the cut word stays zero.
    let takeover_cut = AtomicU64::new(0);
    let stopped = AtomicBool::new(false);
    let line_arm = AtomicU64::new(0);
    let ring = Ring::new(16);
    let mut pushed = Vec::new();
    let mut cutovers = Vec::new();
    let mut producer = LiveFileProducer::from_loaded_sources_with_prebake_floor(
        &score,
        WatchLanguage::JavaScript,
        initial,
        None,
        Duration::ZERO,
        Duration::from_millis(1),
        Duration::from_millis(1),
    )
    .expect("producer");

    producer
        .step_with_clock(
            &mut session,
            Duration::ZERO,
            || 0.0,
            SAMPLE_RATE,
            |_, _, _| panic!("initial prefill cut generations"),
            |event| {
                assert!(event.confirmation.is_none(), "raw-ring fixture is unbound");
                assert!(
                    ring.push(event.event),
                    "fixture ring filled at initial prefill"
                );
                pushed.push(event.event);
                true
            },
        )
        .expect("initial prefill");

    write(&score, r#"setcps(2); note("e4 g4")"#);
    let observed = producer
        .step_with_clock(
            &mut session,
            Duration::from_millis(1),
            || 0.05,
            SAMPLE_RATE,
            |_, _, _| panic!("unsettled tempo edit cut generations"),
            |event| {
                assert!(event.confirmation.is_none(), "raw-ring fixture is unbound");
                assert!(
                    ring.push(event.event),
                    "fixture ring filled while observing edit"
                );
                pushed.push(event.event);
                true
            },
        )
        .expect("observe tempo edit");
    assert_eq!(observed.watch, WatchPoll::Pending);

    let replacement_start = pushed.len();
    let installed = producer
        .step_with_clock(
            &mut session,
            Duration::from_millis(2),
            || 0.1,
            SAMPLE_RATE,
            |generation, _, _| {
                active_generation.store(generation, Ordering::Release);
                cutovers.push(generation);
            },
            |event| {
                assert_eq!(
                    event.generation,
                    active_generation.load(Ordering::Acquire),
                    "replacement crossed before the consumer generation cutover"
                );
                assert!(event.confirmation.is_none(), "raw-ring fixture is unbound");
                assert!(
                    ring.push(event.event),
                    "fixture ring filled at tempo cutover"
                );
                pushed.push(event.event);
                true
            },
        )
        .expect("install watched tempo replacement");
    let WatchPoll::Event(installed_event) = installed.watch else {
        panic!("tempo replacement did not install: {:?}", installed.watch);
    };
    assert_eq!(installed_event.status, ReloadStatus::Installed);
    let tempo_generation = installed_event.generation_after;
    assert_eq!(tempo_generation, initial_generation + 1);
    assert_eq!(session.generation(), tempo_generation);
    assert_eq!(session.config().cps, 2.0);
    assert_eq!(cutovers, [tempo_generation]);
    assert_eq!(active_generation.load(Ordering::Acquire), tempo_generation);

    let replacement_events = &pushed[replacement_start..];
    assert!(
        replacement_events.len() >= 2,
        "tempo replacement produced too few ring events: {replacement_events:?}"
    );
    assert!(
        replacement_events
            .iter()
            .all(|event| event.generation == tempo_generation),
        "tempo replacement mixed generations: {replacement_events:?}"
    );
    let first_replacement = *replacement_events
        .iter()
        .min_by_key(|event| event.target_frame)
        .expect("checked replacement event");
    let last_replacement_frame = replacement_events
        .iter()
        .map(|event| event.target_frame)
        .max()
        .expect("checked replacement event");
    assert!(
        first_replacement.target_frame >= (0.1 * f64::from(SAMPLE_RATE)) as u64,
        "replacement onset preceded its install clock: {first_replacement:?}"
    );
    assert!(
        replacement_events
            .iter()
            .all(|event| (event.duration_secs - 0.25).abs() < 1e-6),
        "two steps per cycle at 2 CPS must last 0.25s: {replacement_events:?}"
    );
    assert!(
        replacement_events.windows(2).all(|events| {
            events[1].target_frame - events[0].target_frame == u64::from(SAMPLE_RATE) / 4
        }),
        "replacement events did not use the 2-CPS quarter-second spacing: {replacement_events:?}"
    );

    let mut backend = LiveScalarBackend::new(SAMPLE_RATE, 16).expect("live scalar backend");
    let mut pcm = [0.0f32; 128 * 2];
    let first_block = backend.process_block_with(
        &mut pcm,
        128,
        first_replacement.target_frame,
        &ring,
        rustel_audio::LiveFlipAtomics {
            generation: &active_generation,
            takeover_frame: &takeover,
            takeover_cut: &takeover_cut,
            line_arm: &line_arm,
        },
        &stopped,
    );
    assert_eq!(
        first_block.accepted,
        replacement_events.len(),
        "ahead-admission must accept every ringed replacement onset"
    );
    assert!(
        first_block.stale > 0,
        "the consumer did not filter the ringed pre-cutover generation"
    );
    assert!(
        pcm.iter().any(|sample| sample.abs() > 1e-6),
        "the committed tempo generation did not reach audible consumer state"
    );

    // This edit stages a valid tempo before failing score construction. The
    // rejection boundary is before Session/Scheduler installation, so the
    // assertions below do not claim rollback after scalar conversion.
    write(&score, "setcps(4); throw new Error('reject watched tempo')");
    let pending_rejection = producer
        .step_with_clock(
            &mut session,
            Duration::from_millis(3),
            || 0.2,
            SAMPLE_RATE,
            |_, _, _| panic!("unsettled rejected tempo cut generations"),
            |event| {
                assert_eq!(event.generation, tempo_generation);
                assert!(event.confirmation.is_none(), "raw-ring fixture is unbound");
                assert!(
                    ring.push(event.event),
                    "fixture ring filled before rejection"
                );
                pushed.push(event.event);
                true
            },
        )
        .expect("observe rejected tempo edit");
    assert_eq!(pending_rejection.watch, WatchPoll::Pending);

    let rejected = producer
        .step_with_clock(
            &mut session,
            Duration::from_millis(4),
            || 0.2,
            SAMPLE_RATE,
            |_, _, _| panic!("rejected tempo edit published a generation"),
            |event| {
                assert_eq!(event.generation, tempo_generation);
                assert!(event.confirmation.is_none(), "raw-ring fixture is unbound");
                assert!(ring.push(event.event), "fixture ring filled at rejection");
                pushed.push(event.event);
                true
            },
        )
        .expect("report rejected tempo edit");
    let WatchPoll::Event(rejected_event) = rejected.watch else {
        panic!("failed tempo edit was not reported: {:?}", rejected.watch);
    };
    assert_eq!(rejected_event.status, ReloadStatus::Rejected);
    assert_eq!(rejected_event.generation_before, tempo_generation);
    assert_eq!(rejected_event.generation_after, tempo_generation);
    assert_eq!(session.generation(), tempo_generation);
    assert_eq!(session.config().cps, 2.0);
    assert_eq!(cutovers, [tempo_generation]);
    assert_eq!(active_generation.load(Ordering::Acquire), tempo_generation);

    // Force a fresh Scheduler fill after the rejection. With the committed
    // 2-CPS clock, the next two-step onset follows by 0.25s and lasts 0.25s. A
    // leaked staged 4-CPS clock moves it earlier and fails the witness.
    let post_rejection_start = pushed.len();
    let steady = producer
        .step_with_clock(
            &mut session,
            Duration::from_millis(5),
            || 0.45,
            SAMPLE_RATE,
            |_, _, _| panic!("unchanged rejected identity published a generation"),
            |event| {
                assert_eq!(event.generation, tempo_generation);
                assert!(event.confirmation.is_none(), "raw-ring fixture is unbound");
                assert!(
                    ring.push(event.event),
                    "fixture ring filled after rejection"
                );
                pushed.push(event.event);
                true
            },
        )
        .expect("refill last-known-good tempo generation");
    assert_eq!(steady.watch, WatchPoll::Unchanged);
    let post_rejection = &pushed[post_rejection_start..];
    assert_eq!(
        post_rejection.len(),
        1,
        "fixture expected one post-rejection onset: {post_rejection:?}"
    );
    let post_rejection = post_rejection[0];
    assert_eq!(post_rejection.generation, tempo_generation);
    assert_eq!(
        post_rejection.target_frame,
        last_replacement_frame + u64::from(SAMPLE_RATE) / 4,
        "Scheduler did not retain the committed 2-CPS clock"
    );
    assert!((post_rejection.duration_secs - 0.25).abs() < 1e-6);

    // Ahead-admission accepts the onset in the first block that sees it;
    // the last-known-good clock is then proven by the event SOUNDING in the
    // block containing its scheduled frame.
    let mut frame = first_replacement.target_frame + 128;
    let mut post_rejection_accepted = false;
    let mut post_rejection_audible = false;
    while frame <= post_rejection.target_frame {
        pcm.fill(0.0);
        let report = backend.process_block_with(
            &mut pcm,
            128,
            frame,
            &ring,
            rustel_audio::LiveFlipAtomics {
                generation: &active_generation,
                takeover_frame: &takeover,
                takeover_cut: &takeover_cut,
                line_arm: &line_arm,
            },
            &stopped,
        );
        post_rejection_accepted |= report.accepted > 0;
        if post_rejection.target_frame < frame + 128 {
            post_rejection_audible = pcm.iter().any(|sample| sample.abs() > 1e-6);
        }
        frame += 128;
    }
    assert!(
        post_rejection_accepted,
        "the last-known-good clock event never reached the consumer"
    );
    assert!(
        post_rejection_audible,
        "the rejected tempo edit silenced the consumer"
    );
}

#[cfg(feature = "device-audio")]
fn copied_initial_watch_output(
    session: &mut Session,
    producer: &mut rustel_runtime::LiveFileProducer,
) -> rustel_audio::device::ManualLiveOutput {
    let mut output = rustel_audio::device::ManualLiveOutput::new(48_000, session.generation())
        .expect("manual output");
    session
        .bind_audio_confirmations(output.device().confirmations())
        .expect("bind manual output confirmations");
    let initial = producer
        .step_with_clock(
            session,
            Duration::ZERO,
            || output.device().clock_seconds(),
            48_000,
            |_, _, _| panic!("initial fill cut generations"),
            |event| {
                assert!(event.confirmation.is_some(), "initial output was not bound");
                output.device().push(event)
            },
        )
        .expect("initial fill");
    assert!(initial.pushed > 0, "initial fixture produced no audio");
    assert_eq!(initial.pending, 0);

    // Establish A through the real DSP/copy boundary, not push acceptance or
    // an injected rollback marker. Copy the whole queried initial window.
    let through = session.time_at_cycle(
        Fraction::from_f64(session.scheduled_to_cycle()).expect("finite queried cycle"),
    );
    assert!(through > 0.0 && through <= 2.0, "bounded initial fixture");
    let mut pcm = [0.0_f32; 256];
    let mut nonzero = false;
    let max_blocks = 2 * 48_000 / (pcm.len() / 2) + 1;
    for _ in 0..max_blocks {
        output.render(&mut pcm);
        assert!(pcm.iter().all(|sample| sample.is_finite()));
        nonzero |= pcm.iter().any(|sample| sample.abs() > 1e-6);
        if output.device().clock_seconds() > through {
            break;
        }
    }
    let report = output.device().report();
    assert!(output.device().clock_seconds() > through);
    assert!(nonzero, "initial fixture did not reach copied PCM");
    assert_eq!(report.accepted_events, initial.pushed as u64);
    assert_eq!(report.refused_voices, 0);
    assert_eq!(report.callback_errors, 0);
    session.consume_audio_confirmations();
    output
}

#[test]
#[cfg(feature = "device-audio")]
fn replacement_query_refusal_rolls_back_and_publishes_before_recovery_push() {
    use std::cell::{Cell, RefCell};

    use rustel_runtime::LiveFileProducer;

    let temp = TempDir::new();
    let score = temp.join("retryable-query-cutover.strudel");
    let initial = "note('c4').fast(8)";
    write(&score, initial);
    let mut session = Session::new().expect("session");
    session.evaluate(initial).expect("initial score");
    let old_generation = session.generation();
    let mut producer = LiveFileProducer::from_loaded_sources_with_prebake_floor(
        &score,
        WatchLanguage::JavaScript,
        initial,
        None,
        Duration::ZERO,
        Duration::from_millis(1),
        Duration::from_millis(1),
    )
    .expect("producer");
    let output = copied_initial_watch_output(&mut session, &mut producer);
    let now = output.device().clock_seconds();

    write(
        &score,
        r#"
          globalThis.__cutoverQuerySpin = true;
          new Pattern(state => {
            if (globalThis.__cutoverQuerySpin) {
              while (true) {}
            }
            return note('g4').fast(8).query(state);
          })
        "#,
    );
    assert_eq!(
        producer
            .step_with_clock(
                &mut session,
                Duration::from_millis(1),
                || now + 0.05,
                48_000,
                |_, _, _| panic!("unsettled score cut generations"),
                |event| output.device().push(event),
            )
            .expect("observe replacement")
            .watch,
        WatchPoll::Pending
    );

    let actions = RefCell::new(Vec::new());
    let error = producer
        .step_with_clock(
            &mut session,
            Duration::from_millis(2),
            || now + 0.1,
            48_000,
            |generation, takeover, _cut| {
                output
                    .device()
                    .set_generation(generation, takeover, TakeoverCut::None);
                actions.borrow_mut().push(CutoverAction::Set(generation));
            },
            |event| {
                actions
                    .borrow_mut()
                    .push(CutoverAction::Push(event.generation));
                output.device().push(event)
            },
        )
        .expect_err("replacement query runaway must refuse prefill");
    assert_eq!(error.kind(), "resource-limit");
    assert!(error.to_string().contains("CPU deadline"));
    let rollback_generation = old_generation + 2;
    assert_eq!(session.generation(), rollback_generation);
    assert_eq!(output.device().generation(), old_generation);
    assert!(actions.borrow().is_empty());

    let recovery = producer
        .step_with_clock(
            &mut session,
            Duration::from_millis(3),
            || now + 0.1,
            48_000,
            |generation, takeover, _cut| {
                output
                    .device()
                    .set_generation(generation, takeover, TakeoverCut::None);
                actions.borrow_mut().push(CutoverAction::Set(generation));
            },
            |event| {
                assert_eq!(
                    output.device().generation(),
                    event.generation,
                    "replacement crossed before its generation was published"
                );
                actions
                    .borrow_mut()
                    .push(CutoverAction::Push(event.generation));
                output.device().push(event)
            },
        )
        .expect("rollback prefill");
    let WatchPoll::Event(event) = recovery.watch else {
        panic!("rollback Installed event was lost: {:?}", recovery.watch);
    };
    assert_eq!(event.status, ReloadStatus::Installed);
    assert_eq!(event.generation_after, rollback_generation);
    assert_eq!(output.device().generation(), rollback_generation);
    let actions = actions.borrow();
    assert_eq!(
        actions.first(),
        Some(&CutoverAction::Set(rollback_generation))
    );
    assert!(
        actions[1..]
            .iter()
            .all(|action| *action == CutoverAction::Push(rollback_generation))
    );
    assert!(actions.len() > 1, "rollback produced no pushed event");
    drop(actions);

    let duplicate_publications = Cell::new(0usize);
    let settled = producer
        .step_with_clock(
            &mut session,
            Duration::from_millis(4),
            || now + 0.11,
            48_000,
            |_, _, _| duplicate_publications.set(duplicate_publications.get() + 1),
            |event| output.device().push(event),
        )
        .expect("settled rollback identity");
    assert_eq!(settled.watch, WatchPoll::Unchanged);
    assert_eq!(
        duplicate_publications.get(),
        0,
        "one rollback Installed event published more than once"
    );
}

#[test]
#[cfg(feature = "device-audio")]
fn silent_impure_replacement_still_publishes_its_generation_once() {
    use std::cell::RefCell;

    use rustel_runtime::LiveFileProducer;

    let temp = TempDir::new();
    let score = temp.join("silent-query-cutover.strudel");
    let initial = "note('c4')";
    write(&score, initial);
    let mut session = Session::new().expect("session");
    session.evaluate(initial).expect("initial score");
    let old_generation = session.generation();
    let mut producer = LiveFileProducer::from_loaded_sources_with_prebake_floor(
        &score,
        WatchLanguage::JavaScript,
        initial,
        None,
        Duration::ZERO,
        Duration::from_millis(1),
        Duration::from_millis(1),
    )
    .expect("producer");
    producer
        .step(
            &mut session,
            Duration::ZERO,
            0.0,
            48_000,
            |_, _, _| {},
            |_| true,
        )
        .expect("initial fill");

    write(&score, "new Pattern(() => [])");
    assert_eq!(
        producer
            .step(
                &mut session,
                Duration::from_millis(1),
                0.05,
                48_000,
                |_, _, _| panic!("unsettled silence published"),
                |_| true,
            )
            .expect("observe silence")
            .watch,
        WatchPoll::Pending
    );
    let actions = RefCell::new(Vec::new());
    let installed = producer
        .step_with_clock(
            &mut session,
            Duration::from_millis(2),
            || 0.1,
            48_000,
            |generation, _, _| actions.borrow_mut().push(CutoverAction::Set(generation)),
            |event| {
                actions
                    .borrow_mut()
                    .push(CutoverAction::Push(event.generation));
                true
            },
        )
        .expect("install silent replacement");
    assert!(session.active_needs_host(), "fixture must query through JS");
    assert!(matches!(
        installed.watch,
        WatchPoll::Event(ref event)
            if event.status == ReloadStatus::Installed
                && event.generation_after == old_generation + 1
    ));
    assert_eq!(installed.scheduled, 0);
    assert_eq!(installed.pushed, 0);
    assert_eq!(installed.pending, 0);
    assert_eq!(
        actions.borrow().as_slice(),
        [CutoverAction::Set(old_generation + 1)]
    );

    let unchanged = producer
        .step_with_clock(
            &mut session,
            Duration::from_millis(3),
            || 0.11,
            48_000,
            |generation, _, _| actions.borrow_mut().push(CutoverAction::Set(generation)),
            |_| true,
        )
        .expect("steady silent generation");
    assert_eq!(unchanged.watch, WatchPoll::Unchanged);
    assert_eq!(
        actions.borrow().as_slice(),
        [CutoverAction::Set(old_generation + 1)],
        "silent generation was published more than once"
    );
}

#[test]
#[cfg(feature = "device-audio")]
fn pure_silence_refills_host_free_even_when_no_js_budget_is_affordable() {
    use std::cell::Cell;

    use rustel_runtime::LiveFileProducer;

    let temp = TempDir::new();
    let score = temp.join("pure-silence-live.strudel");
    write(&score, "silence");
    let mut session = Session::new().expect("session");
    session.evaluate("silence").expect("pure silence");
    assert!(!session.active_needs_host());
    let mut producer = LiveFileProducer::from_loaded_sources_with_prebake_floor(
        &score,
        WatchLanguage::JavaScript,
        "silence",
        None,
        Duration::ZERO,
        Duration::from_millis(1),
        Duration::from_millis(1),
    )
    .expect("producer");
    let initial = producer
        .step_unwatched_with_clock(&mut session, || 0.0, 48_000, |_| true)
        .expect("initial pure silence prefill");
    assert_eq!(initial.scheduled, 0);

    let clock_calls = Cell::new(0usize);
    let steady = producer
        .step_unwatched_with_clock(
            &mut session,
            || {
                clock_calls.set(clock_calls.get() + 1);
                10.0
            },
            48_000,
            |_| true,
        )
        .expect("pure route must bypass the unaffordable JS preflight");
    assert_eq!(clock_calls.get(), 1);
    assert_eq!(steady.scheduled, 0);
    assert_eq!(steady.pushed, 0);
    assert_eq!(steady.pending, 0);
}

#[test]
#[cfg(feature = "device-audio")]
fn failed_replacement_conversion_rolls_back_to_the_audible_score() {
    use std::cell::RefCell;

    use rustel_runtime::LiveFileProducer;

    let temp = TempDir::new();
    let score = temp.join("conversion-latched-cutover.strudel");
    let initial = "note('c4')";
    write(&score, initial);
    let mut session = Session::new().expect("session");
    session.evaluate(initial).expect("initial score");
    let audible_generation = session.generation();
    let actions = RefCell::new(Vec::new());
    let mut producer = LiveFileProducer::from_loaded_sources_with_prebake_floor(
        &score,
        WatchLanguage::JavaScript,
        initial,
        None,
        Duration::ZERO,
        Duration::from_millis(1),
        Duration::from_millis(1),
    )
    .expect("producer");
    let output = copied_initial_watch_output(&mut session, &mut producer);
    let now = output.device().clock_seconds();

    // Keep an onset inside every plausible replacement phase; bare `pure`
    // can legitimately have no onset in this half-cycle and would turn the
    // conversion fixture into accidental silence.
    write(&score, "pure('not-an-audio-note').fast(16)");
    assert_eq!(
        producer
            .step(
                &mut session,
                Duration::from_millis(1),
                now + 0.05,
                48_000,
                |_, _, _| panic!("unsettled invalid audio cut over"),
                |event| output.device().push(event),
            )
            .expect("observe invalid audio replacement")
            .watch,
        WatchPoll::Pending
    );
    // A replacement whose first window refuses every onset is neither
    // published as silence nor left to poison the producer: the last
    // audible score is put back at once, and the turn says so.
    let conversion = producer
        .step_with_clock(
            &mut session,
            Duration::from_millis(2),
            || now + 0.1,
            48_000,
            |generation, takeover, _cut| {
                output
                    .device()
                    .set_generation(generation, takeover, TakeoverCut::None);
                actions.borrow_mut().push(CutoverAction::Set(generation));
            },
            |event| {
                actions
                    .borrow_mut()
                    .push(CutoverAction::Push(event.generation));
                output.device().push(event)
            },
        )
        .expect_err("non-audio replacement unexpectedly converted");
    assert!(conversion.to_string().contains("scalar audio"));
    assert!(conversion.to_string().contains("keeps playing"));
    let rollback_generation = audible_generation + 2;
    assert_eq!(session.generation(), rollback_generation);
    assert_eq!(output.device().generation(), audible_generation);
    assert!(actions.borrow().is_empty());

    // The next turn plays the rollback: the device moves to the restored
    // generation and only its onsets cross - never the refused one's.
    let restored = producer
        .step_with_clock(
            &mut session,
            Duration::from_millis(3),
            || now + 0.1,
            48_000,
            |generation, takeover, _cut| {
                output
                    .device()
                    .set_generation(generation, takeover, TakeoverCut::None);
                actions.borrow_mut().push(CutoverAction::Set(generation));
            },
            |event| {
                assert_eq!(output.device().generation(), event.generation);
                actions
                    .borrow_mut()
                    .push(CutoverAction::Push(event.generation));
                output.device().push(event)
            },
        )
        .expect("the rollback prefill plays");
    assert_ne!(
        restored.watch,
        WatchPoll::Pending,
        "nothing is left pending"
    );
    assert_eq!(output.device().generation(), rollback_generation);
    {
        let actions = actions.borrow();
        assert_eq!(
            actions.first(),
            Some(&CutoverAction::Set(rollback_generation))
        );
        assert!(
            actions[1..]
                .iter()
                .all(|action| *action == CutoverAction::Push(rollback_generation))
        );
    }
    actions.borrow_mut().clear();

    // A new file identity installs over the restored score as usual.
    write(&score, "note('g4').fast(8)");
    assert_eq!(
        producer
            .step_with_clock(
                &mut session,
                Duration::from_millis(4),
                || now + 0.1,
                48_000,
                |_, _, _| panic!("new identity published before settling"),
                |event| output.device().push(event),
            )
            .expect("observe the new identity")
            .watch,
        WatchPoll::Pending
    );
    let recovered = producer
        .step_with_clock(
            &mut session,
            Duration::from_millis(5),
            || now + 0.1,
            48_000,
            |generation, takeover, _cut| {
                output
                    .device()
                    .set_generation(generation, takeover, TakeoverCut::None);
                actions.borrow_mut().push(CutoverAction::Set(generation));
            },
            |event| {
                assert_eq!(output.device().generation(), event.generation);
                actions
                    .borrow_mut()
                    .push(CutoverAction::Push(event.generation));
                output.device().push(event)
            },
        )
        .expect("new valid identity installs");
    let WatchPoll::Event(event) = recovered.watch else {
        panic!("valid recovery lost Installed event: {:?}", recovered.watch);
    };
    assert_eq!(event.status, ReloadStatus::Installed);
    assert_eq!(event.generation_after, rollback_generation + 1);
    assert_eq!(output.device().generation(), rollback_generation + 1);
    let actions = actions.borrow();
    assert_eq!(
        actions.first(),
        Some(&CutoverAction::Set(rollback_generation + 1))
    );
    assert!(
        actions[1..]
            .iter()
            .all(|action| *action == CutoverAction::Push(rollback_generation + 1))
    );
    assert!(actions.len() > 1, "valid recovery produced no pushed event");
}

#[test]
#[cfg(all(feature = "device-audio", feature = "osc"))]
fn watched_external_only_replacement_cuts_over_and_retains_its_intents() {
    use std::cell::Cell;

    use rustel_runtime::LiveFileProducer;

    let temp = TempDir::new();
    let score = temp.join("external-only-cutover.strudel");
    let initial = "note('c4')";
    write(&score, initial);
    let mut session = Session::new().expect("session");
    session.evaluate(initial).expect("initial score");
    let audible_generation = session.generation();
    let device_generation = Cell::new(audible_generation);
    let mut producer = LiveFileProducer::from_loaded_sources_with_prebake_floor(
        &score,
        WatchLanguage::JavaScript,
        initial,
        None,
        Duration::ZERO,
        Duration::from_millis(1),
        Duration::from_millis(1),
    )
    .expect("producer");
    producer
        .step(
            &mut session,
            Duration::ZERO,
            0.0,
            48_000,
            |_, _, _| {},
            |_| true,
        )
        .expect("initial fill");

    write(&score, "s('not-a-native-sample').osc(57120).fast(16)");
    assert_eq!(
        producer
            .step(
                &mut session,
                Duration::from_millis(1),
                0.05,
                48_000,
                |_, _, _| panic!("unsettled external score cut over"),
                |_| true,
            )
            .expect("observe external score")
            .watch,
        WatchPoll::Pending
    );

    let step = producer
        .step_with_clock(
            &mut session,
            Duration::from_millis(2),
            || 0.1,
            48_000,
            |generation, _, _| device_generation.set(generation),
            |_| panic!("external-only fixture unexpectedly produced scalar audio"),
        )
        .expect("install external-only score");
    let installed_generation = installed(step.watch);
    assert_eq!(installed_generation, audible_generation + 1);
    assert_eq!(device_generation.get(), installed_generation);
    assert_eq!(step.scheduled, 0);
    let intents = session.take_pending_osc();
    assert!(!intents.is_empty());
    assert!(
        intents.iter().all(|(_, intent)| {
            intent.generation == installed_generation && intent.port == 57120
        })
    );
}

#[test]
#[cfg(feature = "device-audio")]
fn deferred_score_and_prebake_reports_survive_query_refusal_and_stop_exactly_once() {
    use std::cell::Cell;

    use rustel_runtime::LiveFileProducer;

    let temp = TempDir::new();
    let score = temp.join("deferred-report-score.strudel");
    let prebake = temp.join("deferred-report-prebake.js");
    let initial_setup = "globalThis.__deferredReportSpin = false;";
    let initial_score = r#"
      new Pattern(state => {
        if (globalThis.__deferredReportSpin) {
          while (true) {}
        }
        return note('c4').fast(8).query(state);
      })
    "#;
    write(&score, initial_score);
    write(&prebake, initial_setup);
    let mut session = Session::new().expect("session");
    session
        .evaluate_prebake(initial_setup)
        .expect("initial setup");
    session.evaluate(initial_score).expect("initial score");
    let generation = session.generation();
    let mut producer = LiveFileProducer::from_loaded_sources_with_prebake_floor(
        &score,
        WatchLanguage::JavaScript,
        initial_score,
        Some((prebake.clone(), initial_setup.to_string())),
        Duration::ZERO,
        Duration::from_millis(1),
        Duration::from_millis(1),
    )
    .expect("producer");
    producer
        .step_with_clock(
            &mut session,
            Duration::ZERO,
            || 0.0,
            48_000,
            |_, _, _| panic!("initial fill changed generation"),
            |_| true,
        )
        .expect("initial fill");

    // Setup succeeds and arms the OLD active graph's query-time runaway;
    // score construction then rejects. Both watch identities are committed
    // before the later scheduler query refuses this producer step.
    write(
        &prebake,
        "globalThis.__deferredReportSpin = true; globalThis.reportSetupRan = 1;",
    );
    write(&score, "note(");
    let observed = producer
        .step_with_clock(
            &mut session,
            Duration::from_millis(1),
            || 0.05,
            48_000,
            |_, _, _| panic!("unsettled identities changed generation"),
            |_| true,
        )
        .expect("observe simultaneous identities");
    assert_eq!(observed.watch, WatchPoll::Pending);
    assert_eq!(observed.prebake_watch, WatchPoll::Pending);

    let refused = producer
        .step_with_clock(
            &mut session,
            Duration::from_millis(2),
            || 0.4,
            48_000,
            |_, _, _| panic!("rejected score changed generation"),
            |_| true,
        )
        .expect_err("armed active query must refuse after both watch events");
    assert_eq!(refused.kind(), "resource-limit");
    assert!(refused.to_string().contains("CPU deadline"));
    assert_eq!(session.generation(), generation);

    // Stop is independent and wins over already-retained diagnostics. It must
    // neither sample the clock nor consume the reports.
    session.transport().stop();
    let stopped_clock_calls = Cell::new(0usize);
    let stopped = producer
        .step_with_clock(
            &mut session,
            Duration::from_millis(3),
            || {
                stopped_clock_calls.set(stopped_clock_calls.get() + 1);
                0.4
            },
            48_000,
            |_, _, _| panic!("Stop published a generation"),
            |_| panic!("Stop pushed an event"),
        )
        .expect("Stop preempts retained reports");
    assert_eq!(stopped.watch, WatchPoll::Stopped);
    assert_eq!(stopped.prebake_watch, WatchPoll::Stopped);
    assert_eq!(stopped_clock_calls.get(), 0);

    session.transport().start();
    let report_clock_calls = Cell::new(0usize);
    let reports = producer
        .step_with_clock(
            &mut session,
            Duration::from_millis(4),
            || {
                report_clock_calls.set(report_clock_calls.get() + 1);
                0.4
            },
            48_000,
            |_, _, _| panic!("retained report changed generation"),
            |_| panic!("retained report retried scheduling"),
        )
        .expect("surface retained reports before retry");
    let WatchPoll::Event(score_report) = reports.watch else {
        panic!("rejected score report was lost: {:?}", reports.watch);
    };
    let WatchPoll::Event(prebake_report) = reports.prebake_watch else {
        panic!(
            "successful prebake report was lost: {:?}",
            reports.prebake_watch
        );
    };
    assert_eq!(score_report.status, ReloadStatus::Rejected);
    assert_eq!(score_report.generation_before, generation);
    assert_eq!(score_report.generation_after, generation);
    assert_eq!(prebake_report.status, ReloadStatus::Installed);
    assert_eq!(prebake_report.generation_before, generation);
    assert_eq!(prebake_report.generation_after, generation);
    assert_eq!(report_clock_calls.get(), 0);
    assert_eq!(reports.scheduled, 0);
    assert_eq!(reports.pushed, 0);

    session
        .evaluate_prebake("globalThis.__deferredReportSpin = false;")
        .expect("disarm active query");
    let retried = producer
        .step_with_clock(
            &mut session,
            Duration::from_millis(5),
            || 0.4,
            48_000,
            |_, _, _| panic!("retry changed generation"),
            |_| true,
        )
        .expect("retry after delivering both reports");
    assert_eq!(retried.watch, WatchPoll::Unchanged);
    assert_eq!(retried.prebake_watch, WatchPoll::Unchanged);
}

#[test]
#[cfg(feature = "device-audio")]
fn a_pending_rollback_cutover_rejects_a_generation_mismatch() {
    use std::cell::Cell;

    use rustel_runtime::LiveFileProducer;

    let temp = TempDir::new();
    let score = temp.join("route-owned-cutover.strudel");
    let initial = "note('c4').fast(8)";
    write(&score, initial);
    let mut session = Session::new().expect("session");
    session.evaluate(initial).expect("initial score");
    let initial_generation = session.generation();
    let mut producer = LiveFileProducer::from_loaded_sources_with_prebake_floor(
        &score,
        WatchLanguage::JavaScript,
        initial,
        None,
        Duration::ZERO,
        Duration::from_millis(1),
        Duration::from_millis(1),
    )
    .expect("producer");
    let output = copied_initial_watch_output(&mut session, &mut producer);
    let now = output.device().clock_seconds();

    // A replacement whose first window refuses every onset rolls back to
    // the audible score at once. The rollback is then the pending cutover.
    write(&score, "pure('not-an-audio-note').fast(16)");
    assert_eq!(
        producer
            .step_with_clock(
                &mut session,
                Duration::from_millis(1),
                || now + 0.05,
                48_000,
                |_, _, _| panic!("unsettled replacement cut generations"),
                |event| output.device().push(event),
            )
            .expect("observe replacement")
            .watch,
        WatchPoll::Pending
    );
    let refusal = producer
        .step_with_clock(
            &mut session,
            Duration::from_millis(2),
            || now + 0.1,
            48_000,
            |generation, takeover, cut| output.device().set_generation(generation, takeover, cut),
            |event| output.device().push(event),
        )
        .expect_err("a refused replacement rolls back");
    assert!(refusal.to_string().contains("keeps playing"));
    let pending_generation = initial_generation + 2;
    assert_eq!(session.generation(), pending_generation);
    assert_eq!(output.device().generation(), initial_generation);

    let external_generation = session
        .reload_at("note('a4').fast(8)", false, now + 0.1)
        .expect("external Session replacement");
    assert_eq!(external_generation, pending_generation + 1);
    let mismatch_clock_calls = Cell::new(0usize);
    let mismatch_pushes = Cell::new(0usize);
    let mismatch_publications = Cell::new(0usize);
    let mismatch = producer
        .step_with_clock(
            &mut session,
            Duration::from_millis(3),
            || {
                mismatch_clock_calls.set(mismatch_clock_calls.get() + 1);
                now + 0.1
            },
            48_000,
            |_, _, _| mismatch_publications.set(mismatch_publications.get() + 1),
            |_| {
                mismatch_pushes.set(mismatch_pushes.get() + 1);
                true
            },
        )
        .expect_err("stale stored generation was published after external replacement");
    assert!(
        mismatch
            .to_string()
            .contains("no longer matches Session generation")
    );
    assert_eq!(mismatch_clock_calls.get(), 0);
    assert_eq!(mismatch_pushes.get(), 0);
    assert_eq!(mismatch_publications.get(), 0);
    assert_eq!(output.device().generation(), initial_generation);
    assert_eq!(session.generation(), external_generation);
}

/// Swapping one beat for another at the same tempo must not move the beat.
///
/// The live-performance case: two scores that are both four-to-the-bar at the
/// same cps, swapped mid-set the way a sampler triggers a part. If a
/// replacement anchored the cycle to wherever the install landed, the incoming
/// pattern would sit off the grid, the swap would read as a stumble rather
/// than a change of sound, and repeated swaps would drift.
///
/// `reload_at` with `restart_transport = false` is the live door: it snapshots
/// the cycle under the OLD mapping and rebases onto it. The scheduler API
/// beneath it re-anchors instead and does NOT preserve the grid, so this pins
/// the behaviour at the layer a performer actually reaches.
#[test]
fn swapping_a_beat_at_the_same_tempo_keeps_the_grid() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"s("kick*4")"#)
        .expect("install the first beat");
    let beat = 1.0 / (4.0 * session.config().cps);

    let on_grid = |what: &str, onsets: &[rustel_runtime::OnsetEventJson]| {
        for onset in onsets {
            let steps = onset.target_time / beat;
            assert!(
                (steps - steps.round()).abs() < 1e-6,
                "{what} landed at {}s, off the {beat}s grid by {:.6}s",
                onset.target_time,
                (steps - steps.round()).abs() * beat
            );
        }
    };

    let before = session.schedule_at(0.0).expect("schedule the first beat");
    assert!(!before.is_empty(), "the first beat never scheduled");
    on_grid("the first beat", &before);

    // Swap deliberately BETWEEN beats, as a performer would: not on a
    // boundary, and while the outgoing pattern is still sounding.
    let swap_at = beat * 2.5;
    session
        .reload_at(r#"s("snare*4")"#, false, swap_at)
        .expect("live swap");

    // Walk the clock forward: one call at the swap instant sees nothing,
    // because the horizon it would draw from was already queried.
    let mut after = Vec::new();
    for step in 1..=5 {
        after.extend(
            session
                .schedule_at(swap_at + step as f64 * beat)
                .expect("schedule the swapped-in beat"),
        );
    }
    assert!(!after.is_empty(), "the swapped-in beat never scheduled");
    assert!(
        after.iter().all(|onset| onset.value_show.contains("snare")),
        "the outgoing pattern leaked past the swap: {:?}",
        after.iter().map(|o| &o.value_show).collect::<Vec<_>>()
    );
    on_grid("the swapped-in beat", &after);
}
