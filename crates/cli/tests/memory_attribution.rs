/*
rustel - isolated memory attribution for Session and the rustel CLI
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! Measure child-process RSS separately from the Session's live QuickJS heap.
//!
//! The 512 MiB QuickJS ceiling is a limit, not a usage measurement. Child RSS
//! measures the product process; in-process RSS measures the test binary.
//! `js_heap_live` reports bytes still tracked by the QuickJS allocator.
//!
//! The attribution cases measure the cost of requested resources. Leak tests
//! check for continued growth after warmup, including discarded scores,
//! convolvers, worker threads and the Studio timeline.
//!
//! Print the measurements with `-- --nocapture`. Release builds give numbers
//! comparable to the shipped binary:
//!
//! ```sh
//! cargo test --release -p rustel --test memory_attribution -- --nocapture
//! ```

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use std::sync::Mutex;

use rustel_runtime::{ProcessMonitor, Session};

static RUN: Mutex<()> = Mutex::new(());

/// The QuickJS heap ceiling (`DEFAULT_JS_MEMORY_LIMIT`). A live heap this
/// large would mean the bound is doing work; ordinary scores must sit far
/// below it.
const JS_HEAP_CEILING: u64 = 512 * 1024 * 1024;
/// Ordinary Session construction plus a short score must not approach the
/// ceiling. Tens of megabytes of control surface is expected; hundreds are
/// the claim under test.
const ORDINARY_JS_HEAP_LIMIT: u64 = 256 * 1024 * 1024;
/// Re-evaluating a short synth the way Ctrl-Enter does must not retain a
/// new graph per take. Sixteen mebibytes of jitter covers allocator
/// rounding; a linear climb would exceed it.
const REEVAL_HEAP_GROWTH_LIMIT: u64 = 16 * 1024 * 1024;
/// After GC, discarded unique scores may leave the current graph and one
/// blob. Hundreds of kilobytes of leftover blobs are a rooted leak.
const DISCARDED_SCORE_HEAP_LIMIT: u64 = 256 * 1024;
/// Previewing the same sounds a second time runs under the preview RAM
/// ceiling, so whatever the ceiling dropped is decoded again: the second
/// pass costs something, bounded by the ceiling (64 MiB by default) rather
/// than by the size of the fonts. Duplication is what this catches - with
/// no ceiling in force the same pass climbs by about the fonts again, ~95
/// MiB measured, and a per-preview copy would be worse still.
const PREVIEW_SECOND_PASS_RSS_LIMIT: u64 = 64 * 1024 * 1024;
/// A one-note `query` that reached 2 GiB would be a runaway, debug or not.
const CLI_RUNAWAY_RSS: u64 = 2 * 1024 * 1024 * 1024;
/// Worker-restart thread-count slack: sample-loader teardown is not
/// instantaneous, and the test binary has its own threads.
const THREAD_COUNT_SLACK: u64 = 8;

const SYNTH: &str = r#"note("c").s("sawtooth")"#;
const ROOM: &str = r#"note("c3 e3 g3").s("sawtooth").room(0.8).roomsize(2)"#;
const ROOM_LARGE: &str = r#"note("c3").s("sawtooth").room(1).roomsize(6)"#;
const DRUMS: &str = r#"s("bd sd hh")"#;
const GM_PIANO: &str = r#"note("c3").s("gm_piano")"#;
const CALLBACK: &str = r#"s("bd").polyBind(x => pure(x).fast(2))"#;

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

// RSS limits are calibrated for Linux. macOS and Windows retain freed pages,
// so RSS growth there cannot reliably distinguish leaks from allocator reuse.
// Return None on those hosts to skip the guarded RSS assertions and tables.
// Portable leak checks would need resource counters for PCM and shader objects.
fn rss_bytes() -> Option<u64> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let now = Instant::now();
    ProcessMonitor::new(now).sample(now).resident_bytes
}

fn thread_count() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|line| line.starts_with("Threads:"))?;
        line.split_whitespace().nth(1)?.parse().ok()
    }
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("ps")
            .args(["-o", "thcount=", "-p", &std::process::id().to_string()])
            .output()
            .ok()?;
        String::from_utf8_lossy(&output.stdout).trim().parse().ok()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    None
}

fn child_rss_bytes(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
        let pages = statm.split_whitespace().nth(1)?.parse::<u64>().ok()?;
        // SAFETY: `sysconf` reads a process-wide constant and writes nothing.
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let page_size = u64::try_from(page_size).ok()?;
        Some(pages.saturating_mul(page_size))
    }
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("ps")
            .args(["-o", "rss=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        let kb: u64 = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .ok()?;
        Some(kb.saturating_mul(1024))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

fn rustel() -> Command {
    Command::new(env!("CARGO_BIN_EXE_rustel"))
}

/// Peak child RSS while `rustel` runs, then the collected output.
fn peak_child_rss(mut child: Child, timeout: Duration) -> (Option<u64>, std::process::Output) {
    let deadline = Instant::now() + timeout;
    let mut peak = None;
    loop {
        if let Some(rss) = child_rss_bytes(child.id()) {
            peak = Some(peak.unwrap_or(0).max(rss));
        }
        if child.try_wait().expect("poll rustel").is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("rustel child exceeded {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().expect("collect rustel");
    (peak, output)
}

fn run_cli(args: &[&str], timeout: Duration) -> (Option<u64>, std::process::Output) {
    let child = rustel()
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("spawn rustel {args:?}: {error}"));
    peak_child_rss(child, timeout)
}

fn assert_cli_ok(label: &str, args: &[&str], output: &std::process::Output, peak: Option<u64>) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{label} {args:?} failed (code {:?}, peak RSS {:?})\nstderr: {stderr}",
        output.status.code(),
        peak.map(mib)
    );
    if let Some(peak) = peak {
        assert!(
            peak < CLI_RUNAWAY_RSS,
            "{label} peak RSS {:.1} MiB is a runaway",
            mib(peak)
        );
    }
}

struct Stage {
    name: &'static str,
    rss: Option<u64>,
    heap: Option<u64>,
}

fn print_session_table(profile: &str, stages: &[Stage], baseline: Option<u64>) {
    eprintln!("session memory ({profile})");
    eprintln!(
        "{:<28} {:>10} {:>10} {:>12} {:>10}",
        "stage", "rss_mib", "delta_mib", "js_heap_mib", "heap/rss"
    );
    for stage in stages {
        let rss = stage.rss.map(mib);
        let delta = match (stage.rss, baseline) {
            (Some(rss), Some(base)) => Some(mib(rss.saturating_sub(base))),
            _ => None,
        };
        let heap = stage.heap.map(mib);
        let share = match (stage.heap, stage.rss, baseline) {
            (Some(heap), Some(rss), Some(base)) => {
                let grown = rss.saturating_sub(base);
                if grown == 0 {
                    None
                } else {
                    Some(heap as f64 / grown as f64 * 100.0)
                }
            }
            _ => None,
        };
        eprintln!(
            "{:<28} {:>10} {:>10} {:>12} {:>10}",
            stage.name,
            rss.map(|v| format!("{v:.1}")).unwrap_or_else(|| "-".into()),
            delta
                .map(|v| format!("{v:.1}"))
                .unwrap_or_else(|| "-".into()),
            heap.map(|v| format!("{v:.2}"))
                .unwrap_or_else(|| "-".into()),
            share
                .map(|v| format!("{v:.0}%"))
                .unwrap_or_else(|| "-".into()),
        );
    }
    let _ = std::io::stderr().flush();
}

fn print_cli_table(profile: &str, rows: &[(&str, Option<u64>)]) {
    eprintln!("cli child memory ({profile})");
    eprintln!("{:<40} {:>10}", "command", "peak_rss_mib");
    for (label, peak) in rows {
        eprintln!(
            "{:<40} {:>10}",
            label,
            peak.map(mib)
                .map(|v| format!("{v:.1}"))
                .unwrap_or_else(|| "-".into())
        );
    }
    let _ = std::io::stderr().flush();
}

fn profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}

/// A real `rustel` process: query (JS only), play (schedule, no device),
/// render of a dry synth, then room, drums, and a GM piano.
///
/// Differences between rows are the claim. Query vs dry render is the
/// sample-library + scalar backend. Dry vs room is the convolver at peak.
/// `gm_piano` is the soundfont path: tens of decoded zones in the sample
/// bank, which is how a long TUI session climbs - not the 512 MiB JS
/// ceiling.
#[test]
fn cli_child_rss_ladder() {
    let _run = RUN.lock().unwrap_or_else(|poison| poison.into_inner());
    let scratch = tempfile::tempdir().expect("scratch");
    let dry = scratch.path().join("dry.wav");
    let room = scratch.path().join("room.wav");
    let room_large = scratch.path().join("room-large.wav");
    let drums = scratch.path().join("drums.wav");
    let piano = scratch.path().join("piano.wav");

    let cases: [(&str, Vec<String>); 7] = [
        (
            "query synth",
            vec!["query".into(), "-e".into(), SYNTH.into()],
        ),
        (
            "play synth 0.25s",
            vec![
                "play".into(),
                "-e".into(),
                SYNTH.into(),
                "--duration".into(),
                "0.25".into(),
            ],
        ),
        (
            "render synth 0.5s",
            vec![
                "render".into(),
                "-e".into(),
                SYNTH.into(),
                "--duration".into(),
                "0.5".into(),
                "-o".into(),
                dry.to_string_lossy().into_owned(),
            ],
        ),
        (
            "render room(2) 0.5s",
            vec![
                "render".into(),
                "-e".into(),
                ROOM.into(),
                "--duration".into(),
                "0.5".into(),
                "-o".into(),
                room.to_string_lossy().into_owned(),
            ],
        ),
        (
            "render room(6) 0.5s",
            vec![
                "render".into(),
                "-e".into(),
                ROOM_LARGE.into(),
                "--duration".into(),
                "0.5".into(),
                "-o".into(),
                room_large.to_string_lossy().into_owned(),
            ],
        ),
        (
            "render drums 0.5s",
            vec![
                "render".into(),
                "-e".into(),
                DRUMS.into(),
                "--duration".into(),
                "0.5".into(),
                "-o".into(),
                drums.to_string_lossy().into_owned(),
            ],
        ),
        (
            "render gm_piano 0.5s",
            vec![
                "render".into(),
                "-e".into(),
                GM_PIANO.into(),
                "--duration".into(),
                "0.5".into(),
                "-o".into(),
                piano.to_string_lossy().into_owned(),
            ],
        ),
    ];

    let timeout = Duration::from_secs(60);
    let mut rows = Vec::new();
    for (label, args) in &cases {
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let (peak, output) = run_cli(&argv, timeout);
        assert_cli_ok(label, &argv, &output, peak);
        rows.push((*label, peak));
    }
    print_cli_table(profile(), &rows);

    if !cfg!(debug_assertions)
        && let Some(query) = rows[0].1
    {
        // The RSS of a release one-note query must stay well below the 512 MiB
        // JS ceiling. Debug RSS is not comparable to the shipped binary.
        assert!(
            query < ORDINARY_JS_HEAP_LIMIT + 64 * 1024 * 1024,
            "release `query` of one synth note peaked at {:.1} MiB; the 512 MiB JS ceiling is not an explanation unless the live heap is in that band too",
            mib(query)
        );
    }
}

/// In-process attribution: RSS deltas of this test binary plus the live
/// QuickJS heap after each Session stage. The heap column is the one that
/// can confirm or kill the 512 MiB-ceiling story.
#[test]
fn session_stages_do_not_fill_the_js_heap_ceiling() {
    let _run = RUN.lock().unwrap_or_else(|poison| poison.into_inner());
    let mut stages = Vec::new();
    let push = |stages: &mut Vec<Stage>, name: &'static str, session: Option<&Session>| {
        stages.push(Stage {
            name,
            rss: rss_bytes(),
            heap: session.map(|session| session.js_heap_live() as u64),
        });
    };

    push(&mut stages, "baseline", None);
    let baseline = stages[0].rss;

    let mut session = Session::new().expect("session");
    push(&mut stages, "session_new", Some(&session));
    let heap_new = session.js_heap_live() as u64;
    // Construction is a thin QuickJS realm plus native bindings. The
    // "tens of megabytes" Session comment is process RSS, not this
    // counter: a new heap here is tens of kilobytes.
    assert!(heap_new > 0, "Session::new reported an empty QuickJS heap");
    assert!(
        heap_new < ORDINARY_JS_HEAP_LIMIT,
        "Session::new live JS heap {:.1} MiB reaches the ordinary limit; ceiling is {:.0} MiB",
        mib(heap_new),
        mib(JS_HEAP_CEILING)
    );

    session.evaluate(SYNTH).expect("evaluate synth");
    push(&mut stages, "evaluate_synth", Some(&session));
    let heap_after_eval = session.js_heap_live() as u64;

    session.play(0.25).expect("play synth");
    push(&mut stages, "play_synth_0.25s", Some(&session));

    for i in 0..32 {
        session
            .evaluate(SYNTH)
            .unwrap_or_else(|error| panic!("re-eval {i}: {error}"));
    }
    push(&mut stages, "reeval_synth_x32", Some(&session));
    let heap_after_reeval = session.js_heap_live() as u64;
    let growth = heap_after_reeval.saturating_sub(heap_after_eval);

    session.evaluate(CALLBACK).expect("evaluate callback");
    session.play(0.5).expect("play callback");
    push(&mut stages, "play_callback_0.5s", Some(&session));

    match session.enable_default_samples() {
        Ok(()) => {
            session.wait_for_sample_loads(Duration::from_secs(5));
            push(&mut stages, "default_sample_library", Some(&session));
            let files = session.prefetch_sounds(&["bd".into(), "sd".into(), "hh".into()]);
            session.wait_for_sample_loads(Duration::from_secs(8));
            stages.push(Stage {
                name: "prefetch_bd_sd_hh",
                rss: rss_bytes(),
                heap: Some(session.js_heap_live() as u64),
            });
            let _ = files;
            if session.evaluate(DRUMS).is_ok() {
                let _ = session.render_pcm(0.5);
                push(&mut stages, "render_drums_0.5s", Some(&session));
            }
            // Soundfonts are the large RAM path. `render gm_piano` in the
            // CLI ladder often stays small because a 0.5s bounce will not
            // wait for the zones; prefetching them is what the TUI does
            // when the name is in the score.
            let piano = session.prefetch_sounds(&["gm_piano".into()]);
            session.wait_for_sample_loads(Duration::from_secs(15));
            stages.push(Stage {
                name: "prefetch_gm_piano",
                rss: rss_bytes(),
                heap: Some(session.js_heap_live() as u64),
            });
            eprintln!("prefetch_gm_piano requested {piano} file(s)");
        }
        Err(error) => {
            eprintln!("default sample library unavailable: {error}");
            push(&mut stages, "default_sample_library_skip", Some(&session));
        }
    }

    session.evaluate(ROOM).expect("evaluate room");
    let _ = session.render_pcm(0.5).expect("render room");
    push(&mut stages, "render_room(2)_0.5s", Some(&session));

    session.evaluate(ROOM_LARGE).expect("evaluate large room");
    let _ = session.render_pcm(0.5).expect("render large room");
    push(&mut stages, "render_room(6)_0.5s", Some(&session));

    let heap_final = session.js_heap_live() as u64;

    drop(session);
    push(&mut stages, "session_dropped", None);

    print_session_table(profile(), &stages, baseline);

    assert!(
        growth < REEVAL_HEAP_GROWTH_LIMIT,
        "32 re-evals of a one-note synth retained {:.1} MiB of JS heap ({} → {} bytes); that is a live leak, not the 512 MiB ceiling",
        mib(growth),
        heap_after_eval,
        heap_after_reeval
    );
    assert!(
        heap_final < ORDINARY_JS_HEAP_LIMIT,
        "live JS heap after synth, re-eval, drums and room is {:.1} MiB; the 512 MiB ceiling is unused",
        mib(heap_final)
    );
}

#[cfg(feature = "studio")]
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

#[cfg(feature = "studio")]
fn silent_worker() -> rustel_studio::worker::StudioWorker {
    use rustel_studio::engine::StudioConfig;
    rustel_studio::worker::StudioWorker::spawn(
        StudioConfig {
            output: Some("silent".into()),
            poll_interval: Duration::from_millis(2),
            ..Default::default()
        },
        #[cfg(feature = "hydra")]
        rustel_runtime::hydra::HydraBridge::new(),
    )
    .expect("studio worker")
}

#[cfg(feature = "studio")]
fn wait_eval(worker: &rustel_studio::worker::StudioWorker, id: u64) -> u64 {
    use std::sync::mpsc::TryRecvError;

    use rustel_studio::worker::StudioControlEvent;

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match worker.try_recv_control() {
            Ok(StudioControlEvent::Evaluation(outcome)) if outcome.request_id == id => {
                return outcome
                    .result
                    .unwrap_or_else(|error| panic!("evaluate {id}: {error:?}"))
                    .generation;
            }
            Ok(StudioControlEvent::EngineFailure(error)) => {
                panic!("engine failure: {error:?}")
            }
            Err(TryRecvError::Disconnected) => panic!("studio worker disconnected"),
            _ => {}
        }
        assert!(
            Instant::now() < deadline,
            "evaluate {id} was not acknowledged"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Evaluation acknowledges construction, not the first sounding event. A
/// sparse score may not prepare its reverb until the following cycle.
#[cfg(feature = "studio")]
fn wait_playing(
    worker: &rustel_studio::worker::StudioWorker,
    generation: u64,
    expected_reverbs: u64,
) {
    use std::sync::mpsc::TryRecvError;

    use rustel_studio::worker::StudioControlEvent;

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut onset_time = None;
    loop {
        while let Ok(batch) = worker.try_recv_trace() {
            if batch.generation == generation
                && batch.preview_from_cycle.is_none()
                && let Some(onset) = batch.traces.first()
            {
                onset_time.get_or_insert(onset.target_time);
            }
        }
        match worker.try_recv_control() {
            Ok(StudioControlEvent::Snapshot(snapshot)) => {
                if let Some(pressure) = snapshot.pressure {
                    let device = pressure.device;
                    assert_eq!(device.asset_queues.leaked, 0, "asset return queue overflow");
                    if snapshot.audible_generation == Some(generation)
                        && onset_time.is_some_and(|time| snapshot.device_time > time)
                        && device.realtime_pressure.active_orbit_reverbs == expected_reverbs
                        && device.asset_queues.orbit_reverb_installs == 0
                        && device.asset_queues.orbit_reverb_returns == 0
                    {
                        return;
                    }
                }
            }
            Ok(StudioControlEvent::EngineFailure(error)) => {
                panic!("engine failure: {error:?}")
            }
            Err(TryRecvError::Disconnected) => panic!("studio worker disconnected"),
            _ => {}
        }
        assert!(
            Instant::now() < deadline,
            "generation {generation} did not play with {expected_reverbs} installed reverb(s)"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(feature = "studio")]
fn hold(worker: &rustel_studio::worker::StudioWorker, duration: Duration) -> Option<u64> {
    let deadline = Instant::now() + duration;
    let mut peak = rss_bytes();
    while Instant::now() < deadline {
        while worker.try_recv_control().is_ok() {}
        if let Some(rss) = rss_bytes() {
            peak = Some(peak.unwrap_or(0).max(rss));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    peak
}

/// Live studio engine on the silent output: the TUI's worker without ratatui.
/// Orbit reverbs and the sample bank stay alive here, unlike an offline
/// render that frees them when the bounce ends.
#[cfg(feature = "studio")]
#[test]
fn studio_silent_engine_rss_while_playing() {
    use rustel_studio::engine::Launch;

    let _run = RUN.lock().unwrap_or_else(|poison| poison.into_inner());
    let baseline = rss_bytes();
    let mut worker = silent_worker();
    let spawned = rss_bytes();
    worker
        .try_evaluate(1, 1, SYNTH.into(), false, Launch::Now, false)
        .expect("queue synth");
    wait_eval(&worker, 1);
    let after_synth = hold(&worker, Duration::from_millis(400));

    worker
        .try_evaluate(2, 2, ROOM_LARGE.into(), false, Launch::Now, false)
        .expect("queue room");
    wait_eval(&worker, 2);
    let after_room = hold(&worker, Duration::from_millis(800));

    worker
        .try_evaluate(3, 3, DRUMS.into(), false, Launch::Now, false)
        .expect("queue drums");
    wait_eval(&worker, 3);
    let after_drums = hold(&worker, Duration::from_millis(800));

    for revision in 4..20 {
        worker
            .try_evaluate(revision, revision, SYNTH.into(), false, Launch::Now, false)
            .expect("queue re-eval");
        wait_eval(&worker, revision);
    }
    let after_reeval = hold(&worker, Duration::from_millis(400));

    worker.shutdown();

    let rows = [
        ("test_binary_baseline", baseline),
        ("studio_worker_spawned", spawned),
        ("playing_synth", after_synth),
        ("playing_room(6)", after_room),
        ("playing_drums", after_drums),
        ("after_16_reevals", after_reeval),
    ];
    eprintln!("studio silent-engine memory ({})", profile());
    eprintln!("{:<28} {:>10} {:>10}", "stage", "rss_mib", "step_mib");
    let mut previous = None;
    for (name, rss) in rows {
        let step = match (rss, previous) {
            (Some(rss), Some(prev)) => Some((rss as i64 - prev as i64) as f64 / (1024.0 * 1024.0)),
            _ => None,
        };
        previous = rss;
        eprintln!(
            "{:<28} {:>10} {:>10}",
            name,
            rss.map(mib)
                .map(|v| format!("{v:.1}"))
                .unwrap_or_else(|| "-".into()),
            step.map(|v| format!("{v:+.1}"))
                .unwrap_or_else(|| "-".into()),
        );
    }

    // First live playback plus a room(6) convolver is tens of megabytes,
    // not hundreds. The 512 MiB JS story would show up as a climb across
    // the re-eval rows; those stay flat.
    if let (Some(drums), Some(synth)) = (after_drums, after_synth) {
        let room = after_room.unwrap_or(drums);
        let convolver = room.saturating_sub(synth);
        assert!(
            convolver < 64 * 1024 * 1024,
            "live room(6) added {:.1} MiB on top of a sounding synth; expected about 25 MiB of IR/FFT state",
            mib(convolver)
        );
    }
    if let (Some(end), Some(start)) = (after_reeval, after_drums) {
        let climb = end.saturating_sub(start);
        assert!(
            climb < REEVAL_HEAP_GROWTH_LIMIT,
            "16 live re-evals grew RSS by {:.1} MiB",
            mib(climb)
        );
    }
}

/// Discarded scores must not stay rooted. Each take carries a unique blob
/// that dies with the previous graph; after GC only the current one is live.
#[test]
fn session_discarded_unique_scores_are_collectable() {
    let _run = RUN.lock().unwrap_or_else(|poison| poison.into_inner());
    let mut session = Session::new().expect("session");
    let blob = |i: usize| format!("{i:04}{}", "x".repeat(32 * 1024));
    // An IIFE so a second eval cannot hit `const` redeclare in the same
    // realm; the blob is only reachable from this graph.
    let score = |i: usize| {
        format!(
            r#"(function () {{ const blob = "{}"; return note("c").s("sawtooth"); }})()"#,
            blob(i)
        )
    };

    for i in 0..2 {
        session
            .evaluate(&score(i))
            .unwrap_or_else(|error| panic!("warmup {i}: {error}"));
    }
    session.run_js_gc();
    let before = session.js_heap_live() as u64;

    for i in 2..18 {
        session
            .evaluate(&score(i))
            .unwrap_or_else(|error| panic!("unique {i}: {error}"));
    }
    session.run_js_gc();
    let after = session.js_heap_live() as u64;
    let growth = after.saturating_sub(before);
    eprintln!(
        "discarded unique scores: heap {:.2} → {:.2} MiB (growth {:.1} KiB)",
        mib(before),
        mib(after),
        growth as f64 / 1024.0
    );
    assert!(
        growth < DISCARDED_SCORE_HEAP_LIMIT,
        "16 unique discarded scores retained {:.1} KiB of JS heap after GC; the previous graphs are still rooted",
        growth as f64 / 1024.0
    );
}

/// `play` of a callback-bearing score used to be able to root every tick's
/// cells until the call returned. Repeated short plays must not climb.
#[test]
fn session_repeated_callback_play_is_collectable() {
    let _run = RUN.lock().unwrap_or_else(|poison| poison.into_inner());
    let mut session = Session::new().expect("session");
    session.evaluate(CALLBACK).expect("evaluate callback");
    session.play(0.1).expect("warmup play");
    session.run_js_gc();
    let before = session.js_heap_live() as u64;

    for i in 0..12 {
        session
            .play(0.1)
            .unwrap_or_else(|error| panic!("play {i}: {error}"));
    }
    session.run_js_gc();
    let after = session.js_heap_live() as u64;
    let growth = after.saturating_sub(before);
    eprintln!(
        "repeated callback play: heap {:.2} → {:.2} MiB (growth {:.1} KiB)",
        mib(before),
        mib(after),
        growth as f64 / 1024.0
    );
    assert!(
        growth < DISCARDED_SCORE_HEAP_LIMIT,
        "12 short plays of a callback score retained {:.1} KiB after GC; tick cells are leaking across plays",
        growth as f64 / 1024.0
    );
}

/// Each new Session has its own QuickJS heap. Construction must not feed a
/// process-wide table that makes later sessions larger than the first.
#[test]
fn successive_sessions_do_not_grow_the_js_heap() {
    let _run = RUN.lock().unwrap_or_else(|poison| poison.into_inner());
    let mut heaps = Vec::new();
    for i in 0..6 {
        let mut session = Session::new().expect("session");
        session.evaluate(SYNTH).expect("evaluate");
        session.run_js_gc();
        heaps.push(session.js_heap_live() as u64);
        eprintln!("session {i} heap {:.2} MiB", mib(heaps[i]));
    }
    let min = *heaps.iter().min().expect("heaps");
    let max = *heaps.iter().max().expect("heaps");
    assert!(
        max.saturating_sub(min) < 2 * 1024 * 1024,
        "successive Sessions ranged {:.2}-{:.2} MiB; something global is accumulating into each new heap",
        mib(min),
        mib(max)
    );
}

/// Unique live evals are Ctrl-Enter with a changing score. After warmup,
/// RSS must plateau - a new graph per take would climb linearly.
#[cfg(feature = "studio")]
#[test]
fn studio_unique_live_evals_do_not_climb() {
    use rustel_studio::engine::Launch;

    let _run = RUN.lock().unwrap_or_else(|poison| poison.into_inner());
    let mut worker = silent_worker();
    let mut rss_at = Vec::new();
    for i in 0..20u64 {
        let id = i + 1;
        let source = format!(r#"note("c").s("sawtooth").gain({})"#, 0.4 + i as f64 * 0.01);
        worker
            .try_evaluate(id, id, source.into(), false, Launch::Now, false)
            .expect("queue unique");
        wait_eval(&worker, id);
        if i == 7 || i == 19 {
            rss_at.push(hold(&worker, Duration::from_millis(200)));
        }
    }
    worker.shutdown();
    eprintln!(
        "studio unique evals: after 8 {:.1} MiB, after 20 {:.1} MiB",
        rss_at[0].map(mib).unwrap_or(0.0),
        rss_at[1].map(mib).unwrap_or(0.0)
    );
    if let (Some(warm), Some(late)) = (rss_at[0], rss_at[1]) {
        let climb = late.saturating_sub(warm);
        assert!(
            climb < REEVAL_HEAP_GROWTH_LIMIT,
            "unique live evals 8→20 grew RSS by {:.1} MiB; discarded scores are still in the engine",
            mib(climb)
        );
    }
}

/// Switching a live set from a large room to a dry synth and back must not
/// stack convolvers. The second room is allowed to cost about what the
/// first did, not twice that.
#[cfg(feature = "studio")]
#[test]
fn studio_room_then_dry_does_not_stack_convolvers() {
    use rustel_studio::engine::Launch;

    let _run = RUN.lock().unwrap_or_else(|poison| poison.into_inner());
    let mut worker = silent_worker();
    worker
        .try_evaluate(1, 1, SYNTH.into(), false, Launch::Now, false)
        .expect("queue synth");
    let generation = wait_eval(&worker, 1);
    wait_playing(&worker, generation, 0);
    let dry1 = hold(&worker, Duration::from_millis(300));

    worker
        .try_evaluate(2, 2, ROOM_LARGE.into(), false, Launch::Now, false)
        .expect("queue room");
    let generation = wait_eval(&worker, 2);
    wait_playing(&worker, generation, 1);
    let room1 = hold(&worker, Duration::from_millis(600));

    worker
        .try_evaluate(3, 3, SYNTH.into(), false, Launch::Now, false)
        .expect("queue dry");
    let generation = wait_eval(&worker, 3);
    wait_playing(&worker, generation, 1);
    let dry2 = hold(&worker, Duration::from_millis(300));

    worker
        .try_evaluate(4, 4, ROOM_LARGE.into(), false, Launch::Now, false)
        .expect("queue room again");
    let generation = wait_eval(&worker, 4);
    wait_playing(&worker, generation, 1);
    let room2 = hold(&worker, Duration::from_millis(600));
    worker.shutdown();

    eprintln!(
        "studio room cycle: dry1 {:.1}  room1 {:.1}  dry2 {:.1}  room2 {:.1}",
        dry1.map(mib).unwrap_or(0.0),
        room1.map(mib).unwrap_or(0.0),
        dry2.map(mib).unwrap_or(0.0),
        room2.map(mib).unwrap_or(0.0)
    );
    if let (Some(first), Some(second)) = (room1, room2) {
        let extra = second.saturating_sub(first);
        assert!(
            extra < 16 * 1024 * 1024,
            "second live room(6) is {:.1} MiB above the first; convolvers are stacking",
            mib(extra)
        );
    }
}

/// Shutdown must join the engine thread. Three spawn/play/stop cycles that
/// each left a 64 MiB stack around would show up as extra threads.
#[cfg(feature = "studio")]
#[test]
fn studio_worker_restart_does_not_leak_threads() {
    use rustel_studio::engine::Launch;

    let _run = RUN.lock().unwrap_or_else(|poison| poison.into_inner());
    let before = thread_count();
    for cycle in 0..3 {
        let mut worker = silent_worker();
        worker
            .try_evaluate(1, 1, SYNTH.into(), false, Launch::Now, false)
            .unwrap_or_else(|error| panic!("cycle {cycle}: {error}"));
        wait_eval(&worker, 1);
        let _ = hold(&worker, Duration::from_millis(150));
        worker.shutdown();
    }
    // Loader threads may take a moment to notice cancellation.
    std::thread::sleep(Duration::from_millis(200));
    let after = thread_count();
    eprintln!("studio worker restart: threads {:?} → {:?}", before, after);
    if let (Some(before), Some(after)) = (before, after) {
        assert!(
            after <= before.saturating_add(THREAD_COUNT_SLACK),
            "3 studio worker restarts left {after} threads (started at {before}); the engine or sample loaders are outliving shutdown"
        );
    }
}

/// TUI timeline: the painters keep a bounded window of onsets. Flooding
/// unique events must not grow the buffer past that cap - that is how a
/// long set would spike RAM in the chrome, not the engine.
#[cfg(feature = "studio")]
#[test]
fn tui_visual_timeline_stays_bounded() {
    use rustel_runtime::ui_events::{UiEventBatch, UiScheduledEvent, visual_layout};
    use rustel_studio::visuals::VisualState;

    let _run = RUN.lock().unwrap_or_else(|poison| poison.into_inner());
    let source = r#"$: s("bd")._pianoroll()"#;
    let envelope = visual_layout(source, 1).expect("layout");
    let revision = envelope.ui_layout.source_revision.clone();
    let mut visual = VisualState::default();
    visual.install_layout(envelope);
    visual.start();

    const CAP: usize = 2_048;
    let mut onset = 0u64;
    for batch_index in 0..20 {
        let mut events = Vec::new();
        for _ in 0..200 {
            let step = onset;
            onset += 1;
            let begin = format!("{step}/8");
            let end = format!("{}/8", step + 1);
            events.push(UiScheduledEvent {
                onset_id: step,
                generation: 1,
                whole_begin: begin.clone(),
                whole_end: end.clone(),
                part_begin: begin,
                part_end: end,
                target_time: step as f64 * 0.25,
                duration_seconds: 0.24,
                value: Some("bd".into()),
                color: None,
                label: Some("bd".into()),
                active_label: None,
                scale: None,
                frequency_hz: None,
                gain: Some(1.0),
                ui_visuals: 1,
                context: vec![(0, 2)],
            });
        }
        let batch = UiEventBatch::new(
            batch_index as f64,
            batch_index as f64 * 0.5,
            0.5,
            1,
            revision.clone(),
            events,
            0,
        )
        .expect("batch");
        assert!(visual.install_batch(batch), "batch {batch_index} rejected");
    }
    let held = visual.events().count();
    eprintln!("tui visual timeline held {held} events after 4000 unique onsets (cap {CAP})");
    assert!(
        held <= CAP,
        "TUI timeline kept {held} events after 4000 unique onsets; the painters are unbounded"
    );
}

fn print_named_rss(title: &str, rows: &[(String, Option<u64>)]) {
    eprintln!("{title} ({})", profile());
    eprintln!("{:<40} {:>10} {:>10}", "stage", "rss_mib", "step_mib");
    let mut previous = None;
    for (name, rss) in rows {
        let step = match (*rss, previous) {
            (Some(rss), Some(prev)) => Some((rss as i64 - prev as i64) as f64 / (1024.0 * 1024.0)),
            _ => None,
        };
        previous = *rss;
        eprintln!(
            "{:<40} {:>10} {:>10}",
            name,
            rss.map(mib)
                .map(|v| format!("{v:.1}"))
                .unwrap_or_else(|| "-".into()),
            step.map(|v| format!("{v:+.1}"))
                .unwrap_or_else(|| "-".into()),
        );
    }
}

/// Each sample preview decodes into the sample bank and stays there. A
/// second pass over the same names must not allocate a second copy.
#[cfg(feature = "studio")]
#[test]
fn studio_sample_preview_fills_once() {
    use rustel_studio::engine::Launch;

    let _run = RUN.lock().unwrap_or_else(|poison| poison.into_inner());
    let mut worker = silent_worker();
    worker
        .try_evaluate(1, 1, SYNTH.into(), false, Launch::Now, false)
        .expect("start");
    wait_eval(&worker, 1);
    let _ = hold(&worker, Duration::from_millis(300));

    let sounds = [
        "bd",
        "sd",
        "hh",
        "gm_piano",
        "gm_epiano1",
        "gm_acoustic_guitar_nylon",
        "gm_violin",
    ];
    let mut rows = Vec::new();
    rows.push(("playing_synth".into(), rss_bytes()));

    let preview = |worker: &rustel_studio::worker::StudioWorker, sound: &str| {
        if let Some(library) = worker.library() {
            let _ = library.prefetch(sound);
            library.wait_until_idle(Duration::from_secs(8));
        }
        assert!(
            worker.try_audition(sound, 0.8),
            "preview {sound} was not queued"
        );
        let _ = hold(worker, Duration::from_millis(400));
    };

    for sound in sounds {
        preview(&worker, sound);
        rows.push((format!("preview {sound}"), rss_bytes()));
    }
    let after_first = rss_bytes();
    for sound in sounds {
        preview(&worker, sound);
        rows.push((format!("re-preview {sound}"), rss_bytes()));
    }
    let after_second = rss_bytes();
    worker.shutdown();
    print_named_rss("studio sample preview", &rows);

    if let (Some(first), Some(second)) = (after_first, after_second) {
        let extra = second.saturating_sub(first);
        assert!(
            extra < PREVIEW_SECOND_PASS_RSS_LIMIT,
            "re-previewing the same seven sounds grew RSS by {:.1} MiB; previews are duplicating decoded PCM",
            mib(extra)
        );
    }
}

/// A tape run is evaluate-the-next-block, over and over. Synth-only blocks
/// must not climb. Sample-heavy examples grooves fill the bank on the
/// first pass and must plateau when the same tape is played again.
#[cfg(feature = "studio")]
#[test]
fn studio_tape_replay_sequence_plateaus_on_the_second_pass() {
    use rustel_studio::engine::Launch;
    use rustel_studio::examples::SECTIONS;

    let _run = RUN.lock().unwrap_or_else(|poison| poison.into_inner());
    let mut worker = silent_worker();
    let blocks: Vec<&str> = SECTIONS[0]
        .shelves
        .iter()
        .take(2)
        .flat_map(|shelf| shelf.snippets.iter().map(|snippet| snippet.code))
        .take(8)
        .collect();
    assert!(
        blocks.len() >= 4,
        "the examples's first shelves should give a short tape"
    );

    let mut rows = Vec::new();
    let mut id = 1u64;
    let play = |worker: &rustel_studio::worker::StudioWorker,
                id: &mut u64,
                source: &str,
                label: String,
                rows: &mut Vec<(String, Option<u64>)>| {
        worker
            .try_evaluate(*id, *id, source.into(), false, Launch::Now, false)
            .unwrap_or_else(|error| panic!("{label}: {error}"));
        wait_eval(worker, *id);
        *id += 1;
        if let Some(library) = worker.library() {
            library.wait_until_idle(Duration::from_secs(6));
        }
        let _ = hold(worker, Duration::from_millis(350));
        rows.push((label, rss_bytes()));
    };

    for (index, source) in blocks.iter().enumerate() {
        play(
            &worker,
            &mut id,
            source,
            format!("tape1 block {index}"),
            &mut rows,
        );
    }
    let after_first = rss_bytes();
    for (index, source) in blocks.iter().enumerate() {
        play(
            &worker,
            &mut id,
            source,
            format!("tape2 block {index}"),
            &mut rows,
        );
    }
    let after_second = rss_bytes();
    worker.shutdown();
    print_named_rss("studio tape replay", &rows);

    if let (Some(first), Some(second)) = (after_first, after_second) {
        let extra = second.saturating_sub(first);
        assert!(
            extra < REEVAL_HEAP_GROWTH_LIMIT,
            "replaying the same 8 examples blocks grew RSS by {:.1} MiB; the tape is retaining each pass",
            mib(extra)
        );
    }
}

/// Browsing the Hydra examples and flipping Hydra themes compiles a new
/// shader per sketch. The host retires a device after 48 compiles; a
/// second pass over the same catalogue must not keep climbing.
#[cfg(all(feature = "studio", feature = "hydra"))]
#[test]
fn studio_hydra_theme_and_examples_preview_plateaus() {
    use std::sync::mpsc::TryRecvError;

    use rustel_hydra::HydraFrames;
    use rustel_studio::engine::{Launch, StudioConfig, StudioDiagnosticLevel};
    use rustel_studio::examples::{Kind, SECTIONS};
    use rustel_studio::worker::{StudioControlEvent, StudioWorker};

    let _run = RUN.lock().unwrap_or_else(|poison| poison.into_inner());
    let hydra = rustel_runtime::hydra::HydraBridge::new();
    let preview_frames = hydra.preview_frames();
    let theme_frames = hydra.theme_frames();
    let mut worker = StudioWorker::spawn(
        StudioConfig {
            output: Some("silent".into()),
            poll_interval: Duration::from_millis(2),
            ..Default::default()
        },
        hydra,
    )
    .expect("studio worker");
    worker
        .try_evaluate(1, 1, SYNTH.into(), false, Launch::Now, false)
        .expect("start");
    wait_eval(&worker, 1);
    let _ = hold(&worker, Duration::from_millis(200));

    let snippets: Vec<&str> = SECTIONS
        .iter()
        .filter(|section| section.kind == Kind::Hydra)
        .flat_map(|section| {
            section
                .shelves
                .iter()
                .flat_map(|shelf| shelf.snippets.iter().map(|snippet| snippet.code))
        })
        .collect();
    let themes = [
        "osc(3, 0.02, 0.9).color(0.42, 0.55, 0.98).out()",
        "osc(7, 0.025, 1.1).kaleid(9).colorama(0.04).out()",
        "gradient(0.08).kaleid(2).color(1, 0.35, 0.85).out()",
        "noise(3, 0.08).kaleid(6).colorama(0.08).out()",
        "osc(5, 0.03, 1.1).kaleid(6).saturate(1.2).out()",
        "voronoi(6, 0.08, 0.25).posterize(9, 0.55).out()",
    ];
    assert!(
        !snippets.is_empty(),
        "the Hydra examples catalogue is empty"
    );

    let mut rows = Vec::new();
    rows.push(("playing_synth".into(), rss_bytes()));

    // The host can replace queued sketches before it draws. Wait for this
    // sketch's readback so both RSS samples include the complete catalogue.
    let draw = |frames: &HydraFrames, code: &str, enqueue: &dyn Fn() -> bool| {
        let expected = frames.sketch_epoch().expect("preview or theme stream") + 1;
        let deadline = Instant::now() + Duration::from_secs(15);
        let drain = || loop {
            match worker.try_recv_control() {
                Ok(StudioControlEvent::EngineFailure(error)) => {
                    panic!("engine failure while drawing {code:?}: {error:?}")
                }
                Ok(StudioControlEvent::Diagnostic(diagnostic)) => {
                    assert!(
                        diagnostic.kind != "hydra"
                            || !matches!(
                                diagnostic.level,
                                StudioDiagnosticLevel::Warning | StudioDiagnosticLevel::Error
                            ),
                        "Hydra failed while drawing {code:?}: {}",
                        diagnostic.message
                    );
                }
                Err(TryRecvError::Disconnected) => panic!("studio worker disconnected"),
                Err(TryRecvError::Empty) => break,
                _ => {}
            }
        };
        while !enqueue() {
            drain();
            assert!(Instant::now() < deadline, "could not queue {code:?}");
            std::thread::sleep(Duration::from_millis(2));
        }
        loop {
            drain();
            if frames.sketch_drawn(expected) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "sketch {code:?} did not draw epoch {expected}; current epoch: {:?}; memory: {:?}",
                frames.sketch_epoch(),
                frames.memory()
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    let preview = |code: &str| {
        draw(&preview_frames, code, &|| {
            worker.try_preview_snippet(Some(code.to_owned()))
        });
    };
    let theme = |code: &str| {
        draw(&theme_frames, code, &|| {
            worker.try_set_theme_sketch(Some(code.to_owned()), false)
        });
    };

    for (index, code) in snippets.iter().enumerate() {
        preview(code);
        if index == 0 || index + 1 == snippets.len() {
            rows.push((format!("examples snippet {}", index + 1), rss_bytes()));
        }
    }
    for (index, code) in themes.iter().enumerate() {
        theme(code);
        rows.push((format!("theme {}", index + 1), rss_bytes()));
    }
    let after_first = rss_bytes();
    let memory_first = preview_frames.memory();

    for code in &snippets {
        preview(code);
    }
    for code in &themes {
        theme(code);
    }
    let after_second = rss_bytes();
    let memory_second = preview_frames.memory();
    rows.push(("second pass".into(), after_second));
    worker.shutdown();
    print_named_rss("studio hydra examples+theme", &rows);
    eprintln!("Hydra resources: first pass {memory_first:?}; second pass {memory_second:?}");

    if let (Some(first), Some(second)) = (after_first, after_second) {
        let extra = second.saturating_sub(first);
        assert!(
            extra < REEVAL_HEAP_GROWTH_LIMIT,
            "a second pass over Hydra examples snippets and themes grew RSS by {:.1} MiB; shader residue is not being retired",
            mib(extra)
        );
    }
}
