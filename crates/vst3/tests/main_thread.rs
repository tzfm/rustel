//! The creation and the release of a plugin on the process's main thread,
//! on macOS: in the scan process and in the host process. The scenario
//! runs the fixture plugin through the host of this library, and this
//! binary is also its own child processes: run again with a probe word as
//! the first argument, its main thread registers as the plugin
//! main-thread service and serves it, as the command line does, while a
//! thread of its own drives the work. The fixture plugin writes the
//! thread of each creation, `terminate` call and final release to a mark
//! file. On the other systems the host puts no step of the plugin
//! lifecycle on the main thread, so the binary does nothing there.

#[cfg(target_os = "macos")]
use rustel_audio::InsertSlot;
#[cfg(target_os = "macos")]
use rustel_vst3::{Host, Resolved, WorkerProgram};
#[cfg(target_os = "macos")]
use rustel_vst3_fixture::{self as fixture, NAME};
#[cfg(target_os = "macos")]
use std::ffi::OsString;
#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::{Command, ExitStatus};
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
fn main() {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let mode = args.first().map(OsString::as_os_str).unwrap_or_default();
    match mode.to_str() {
        Some(mode @ ("host" | "busy")) => {
            let [folder, name] = &args[1..] else {
                std::process::exit(2);
            };
            let (folder, name) = (PathBuf::from(folder), name.to_string_lossy().into_owned());
            std::process::exit(probe(mode == "busy", folder, name));
        }
        Some("worker") => {
            let [bundle] = &args[1..] else {
                std::process::exit(2);
            };
            std::process::exit(worker(PathBuf::from(bundle)));
        }
        _ => scenario(),
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {}

/// The work of `host` and `busy`: the whole plugin path of a program that
/// owns the main thread. Returns 0 when every step succeeded.
#[cfg(target_os = "macos")]
fn probe(busy: bool, folder: PathBuf, name: String) -> i32 {
    serve_the_main_thread(busy, Box::new(move || host_work(folder, name)))
}

/// The work of `worker`: the scan process of `host`.
#[cfg(target_os = "macos")]
fn worker(bundle: PathBuf) -> i32 {
    serve_the_main_thread(false, Box::new(move || rustel_vst3::serve(&bundle)))
}

/// Registers this, the process's main thread, as the plugin main-thread
/// service before the work thread starts, and serves it while the work
/// runs, as the command line does. A busy delay before the first service
/// proves that the steps wait for the main thread, not run elsewhere.
/// Returns the exit code of the work, or 4 when it panicked.
#[cfg(target_os = "macos")]
fn serve_the_main_thread(busy: bool, work: Box<dyn FnOnce() -> i32 + Send>) -> i32 {
    rustel_vst3::install_main_service();
    let work = std::thread::spawn(work);
    if busy {
        std::thread::sleep(Duration::from_millis(300));
    }
    while !work.is_finished() {
        rustel_vst3::pump_main();
        std::thread::sleep(Duration::from_millis(2));
    }
    work.join().unwrap_or(4)
}

/// Scans the folder in a worker process of its own, loads the plugin by
/// name, prepares one copy for a slot and shuts the host down. Returns 0
/// when every step succeeded.
#[cfg(target_os = "macos")]
fn host_work(folder: PathBuf, name: String) -> i32 {
    let host = Host::new();
    host.set_worker(Some(WorkerProgram {
        program: std::env::current_exe().expect("this program"),
        args: vec![OsString::from("worker")],
    }));
    host.scan(std::slice::from_ref(&folder));
    host.wait_idle();
    let start = Instant::now();
    while host.scan_progress().is_some() {
        if start.elapsed() > Duration::from_secs(20) {
            return 5;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    // The load runs in this process, on the plugin thread, with the
    // creation and the release of the plugin on the main thread.
    if !matches!(host.resolve(&name, true), Resolved::Ready(..)) {
        return 6;
    }
    host.prepare(&name, None, false, 48_000, InsertSlot::effect(1, 0));
    host.wait_idle();
    host.shutdown(Duration::from_secs(20));
    0
}

/// Runs both probes of the fixture plugin and reads the marks they wrote.
/// The scan process and the host process alike create the plugin at the
/// factory call, end it at `terminate`, and destroy the object at the
/// final release: all on the process's main thread.
#[cfg(target_os = "macos")]
fn scenario() {
    let root: PathBuf =
        std::env::temp_dir().join(format!("rustel-vst3-main-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("the test folder");
    fixture::install(&root);
    let mark = root.join("marks");
    let folder = root.display().to_string();
    for mode in ["host", "busy"] {
        let status = child(mode, &folder, &mark);
        assert!(status.success(), "the {mode} probe: {status}");
    }
    let text = std::fs::read_to_string(&mark).expect("the mark file");
    let lines: Vec<&str> = text.lines().collect();
    assert!(!lines.is_empty(), "the plugin wrote no marks");
    for line in &lines {
        assert!(
            line.ends_with(" main"),
            "a step ran off the main thread: {line}"
        );
    }
    for step in ["make", "end", "destroy"] {
        let wanted = format!("{step} main");
        assert!(
            lines.contains(&wanted.as_str()),
            "no {step} on the main thread: {text}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// Runs this binary again as one probe, and waits for its end.
#[cfg(target_os = "macos")]
fn child(mode: &str, folder: &str, mark: &Path) -> ExitStatus {
    let mut child = Command::new(std::env::current_exe().expect("this program"))
        .args([mode, folder, NAME])
        .env(fixture::THREAD_MARK_ENV, mark)
        .spawn()
        .expect("the probe process");
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("the probe process") {
            return status;
        }
        if start.elapsed() > Duration::from_secs(60) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the probe is too slow");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
