//! The scan of one plugin bundle in a process of its own.
//!
//! A plugin is native code. A bundle with a fault at its load ends the
//! process that loads the bundle. So the host loads a bundle for its scan
//! in one more process, the scan process, and reads the plugin list from a
//! file:
//!
//! ```text
//!   host process                      scan process
//!   scan thread ── bundle, file ────▶ load the bundle
//!        ◀── plugin list in file ──── write the list, unload, end
//! ```
//!
//! The answer is in a file, so no line a plugin prints is in the answer.
//! The scan process writes one line of progress after each class it reads,
//! and each finished line restarts the wait of the host: the limit is the
//! time of one class, not of the bundle as a whole, so a bundle of many
//! slow classes scans to its end.
//! A scan process that ends with no answer, or gives no answer in the time
//! limit, fails its bundle and no more. A scan that did not run, with no
//! file or no process from the system, fails no bundle.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::cache::Scanned;
use crate::host::{Plugin, WorkerProgram, load_bundle};

/// The scan process reads the path of its answer file from this variable.
const ANSWER_ENV: &str = "RUSTEL_VST3_WORKER";
/// The scan process reads its work limit, in milliseconds, from this
/// variable. The host sets it to the limit it waits itself, so the two
/// sides end a load that shows no progress at the same time.
const LIMIT_ENV: &str = "RUSTEL_VST3_WORK_LIMIT_MS";
/// The time one class of a bundle has in the scan. A load that finishes no
/// class in this time gives no answer; each finished class starts the time
/// again.
pub(crate) const WORK_LIMIT: Duration = Duration::from_secs(120);
/// A scan process with its answer in the file has this long to unload the
/// bundle before the host ends the process.
const QUIT_LIMIT: Duration = Duration::from_secs(2);
/// The watchdog of a scan process looks this often for progress.
const WATCH_PAUSE: Duration = Duration::from_millis(100);

/// A different program, such as a virus scanner, sometimes holds a new file
/// for a moment. A write or a read of the answer file has this many tries.
const FILE_TRIES: u32 = 100;
const FILE_PAUSE: Duration = Duration::from_millis(10);

/// The plugins of a bundle, or the reason the bundle did not load.
type Answer = Result<Vec<Scanned>, String>;

/// What the scan of one bundle gave.
#[derive(Debug, PartialEq)]
pub(crate) enum Scan {
    Read(Answer),
    /// No scan process ran: the system gave no answer file, or the worker
    /// program did not start. This says nothing about the bundle.
    NotRun,
}

/// Ends this process at once. No exit handler runs: a handler waits for a
/// lock that the thread in the load holds.
fn quit(code: std::ffi::c_int) -> ! {
    unsafe extern "C" {
        fn _exit(code: std::ffi::c_int) -> !;
    }
    // SAFETY: the function of the C library takes a number and does not
    // return.
    unsafe { _exit(code) }
}

/// The work limit of this scan process: the number its host set, or the
/// default of [`WORK_LIMIT`] when the host set none.
fn work_limit() -> Duration {
    std::env::var_os(LIMIT_ENV)
        .and_then(|text| text.to_str().and_then(|text| text.parse().ok()))
        .map_or(WORK_LIMIT, Duration::from_millis)
}

/// One finished line at the end of the answer file. True when the whole
/// line is in the file. The handle has write access, not append access
/// only: on Windows a handle for appending cannot cut the file back. A
/// write that failed part way left bytes no newline ends, and they go
/// before this write, so a retry writes its line once, not after the
/// part of a failed try. When the cut fails, the line is not written:
/// the answer must not stand after a part of a line.
fn write_line(file: &Path, line: &[u8]) -> bool {
    use std::io::{Read, Seek, SeekFrom, Write as _};
    let Ok(mut open) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(file)
    else {
        return false;
    };
    let mut bytes = Vec::new();
    if open.read_to_end(&mut bytes).is_err() {
        return false;
    }
    let end = u64::try_from(
        bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |at| at + 1),
    )
    .unwrap_or(0);
    if open.set_len(end).is_err() || open.seek(SeekFrom::Start(end)).is_err() {
        return false;
    }
    open.write_all(line).is_ok()
}

/// The scan process: loads the bundle, writes its plugins to the answer
/// file, and returns the exit code of the process. The worker program calls
/// this with the bundle path the host gave as the last argument. One line
/// of progress goes in the file after each class the load reads.
pub fn serve(bundle: &Path) -> i32 {
    let Some(file) = std::env::var_os(ANSWER_ENV).map(PathBuf::from) else {
        return 2;
    };
    let limit = work_limit();
    let start = Instant::now();
    // The time of the last progress, as nanoseconds from the start, shared
    // with the watchdog. A load with no end must not keep this process
    // after its host; the limit restarts at each class, as the wait of the
    // host does.
    let moved = Arc::new(AtomicU64::new(0));
    let mark = |moved: &AtomicU64| {
        moved.store(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
    };
    {
        let moved = Arc::clone(&moved);
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(WATCH_PAUSE);
                let last = Duration::from_nanos(moved.load(Ordering::Relaxed));
                if start.elapsed() > last + limit + QUIT_LIMIT {
                    quit(3);
                }
            }
        });
    }
    let loaded = load_bundle(bundle, &mut || {
        // The line goes in the file first, so a slow write keeps the
        // watchdog waiting.
        let _ = write_line(&file, b"class\n");
        mark(&moved);
    });
    let answer: Answer = match &loaded {
        Ok(plugins) => Ok(plugins.iter().map(Plugin::scanned).collect()),
        Err(reason) => Err(reason.clone()),
    };
    // The host takes an answer only with its end of line.
    let mut line = serde_json::to_vec(&answer).unwrap_or_default();
    line.push(b'\n');
    let mut written = false;
    for attempt in 0..FILE_TRIES {
        if attempt > 0 {
            std::thread::sleep(FILE_PAUSE);
        }
        written = write_line(&file, &line);
        if written {
            break;
        }
    }
    mark(&moved);
    // The unload runs the code of the plugin: the answer is in the file
    // before.
    drop(loaded);
    if written { 0 } else { 2 }
}

/// The file a scan process writes its lines to: one of progress after each
/// class, and the answer at the end. The file goes with this value.
struct AnswerFile(PathBuf);

/// What one look at the answer file gave.
struct Look {
    /// The lines the scan process finished. A line without its newline,
    /// such as a write a kill cut apart, is no line at all.
    lines: usize,
    /// The answer, when the last finished line of the file is one.
    answer: Option<Answer>,
}

impl AnswerFile {
    fn new() -> std::io::Result<Self> {
        use std::hash::{BuildHasher, Hasher};
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        // A name no other process knows, from the random source of the
        // system.
        let random = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish();
        let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
        let name = format!(
            "rustel-vst3-{}-{serial}-{random:016x}.json",
            std::process::id()
        );
        let path = std::env::temp_dir().join(name);
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(Self(path))
    }

    fn look(&self) -> Look {
        let Ok(bytes) = std::fs::read(&self.0) else {
            return Look {
                lines: 0,
                answer: None,
            };
        };
        // The text after the last newline is a line the writer did not
        // finish. Only the lines a newline ends count, and the answer is
        // the last of them: a line of progress is no answer.
        let Some(cut) = bytes.iter().rposition(|byte| *byte == b'\n') else {
            return Look {
                lines: 0,
                answer: None,
            };
        };
        let finished = &bytes[..cut];
        let start = finished
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |at| at + 1);
        Look {
            lines: 1 + finished.iter().filter(|byte| **byte == b'\n').count(),
            answer: serde_json::from_slice(&finished[start..]).ok(),
        }
    }
}

impl Drop for AnswerFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Loads a bundle in a scan process and gives its plugins. Each class of
/// the bundle has `limit` to finish, and each line of progress restarts
/// the wait. `closed` ends the wait: the host ends.
pub(crate) fn scan(
    program: &WorkerProgram,
    bundle: &Path,
    limit: Duration,
    closed: &AtomicBool,
) -> Scan {
    let Ok(file) = AnswerFile::new() else {
        return Scan::NotRun;
    };
    let mut command = Command::new(&program.program);
    command
        .args(&program.args)
        .arg(bundle)
        .env(ANSWER_ENV, &file.0)
        // The scan process ends its own load at the same limit.
        .env(LIMIT_ENV, limit.as_millis().to_string())
        // A plugin sometimes prints at its load, and a scan loads each
        // bundle of the machine.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // The scan process leads a process group of its own, so Ctrl-C in the
    // terminal goes to the host only.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let Ok(mut child) = command.spawn() else {
        return Scan::NotRun;
    };
    let mut seen = 0;
    let mut deadline = Instant::now() + limit;
    let answer = loop {
        let look = file.look();
        if let Some(answer) = look.answer {
            break answer;
        }
        // Each finished line restarts the wait: the limit is the time of
        // one class, not of the bundle as a whole.
        if look.lines > seen {
            seen = look.lines;
            deadline = Instant::now() + limit;
        }
        match child.try_wait() {
            Ok(None) => {}
            // A process that ended wrote all of its answer, or none. A
            // process that ended well wrote all of it.
            Ok(Some(status)) => {
                let tries = if status.success() { FILE_TRIES } else { 1 };
                let read = (0..tries).find_map(|attempt| {
                    if attempt > 0 {
                        std::thread::sleep(FILE_PAUSE);
                    }
                    file.look().answer
                });
                let ended = || Err(format!("the plugin process ended ({status})"));
                break read.unwrap_or_else(ended);
            }
            Err(error) => break Err(error.to_string()),
        }
        if closed.load(Ordering::Relaxed) || Instant::now() > deadline {
            break Err("the plugin process did not answer".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    // The process unloads the bundle after its answer. A process that does
    // not end in the limit, or gave no answer, ends here.
    let answered = Instant::now();
    while answer.is_ok()
        && matches!(child.try_wait(), Ok(None))
        && answered.elapsed() < QUIT_LIMIT
        && !closed.load(Ordering::Relaxed)
    {
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = child.kill();
    let _ = child.wait();
    Scan::Read(answer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_counts_only_with_its_end_of_line() {
        let file = AnswerFile::new().expect("answer file");
        assert!(file.look().answer.is_none(), "an empty file is no answer");
        // A line of progress is a finished line, and no answer.
        std::fs::write(&file.0, b"class\n").expect("progress");
        assert_eq!((file.look().lines, file.look().answer.is_none()), (1, true));
        let answer: Answer = Ok(vec![Scanned {
            name: "Tone".into(),
            vendor: "rustel".into(),
            categories: "Instrument|Synth".into(),
        }]);
        let mut line = serde_json::to_vec(&answer).expect("answer text");
        std::fs::write(&file.0, &line).expect("part of the answer");
        assert!(
            file.look().answer.is_none(),
            "the scan process still writes"
        );
        line.push(b'\n');
        std::fs::write(&file.0, &line).expect("answer");
        assert_eq!(file.look().answer, Some(answer));
        std::fs::write(&file.0, b"{\"Err\":\"no audio plugin\"}\n").expect("failure");
        assert_eq!(file.look().answer, Some(Err("no audio plugin".into())));
        let path = file.0.clone();
        drop(file);
        assert!(!path.exists(), "the file goes with its owner");
    }

    #[test]
    fn only_the_last_finished_line_is_the_answer() {
        let file = AnswerFile::new().expect("answer file");
        let answer: Answer = Err("no audio plugin".into());
        let mut line = serde_json::to_vec(&answer).expect("answer text");
        line.push(b'\n');
        let mut all = b"class\nclass\n".to_vec();
        all.extend(&line);
        std::fs::write(&file.0, &all).expect("the file of a scan");
        let look = file.look();
        assert_eq!(look.lines, 3);
        assert_eq!(look.answer, Some(answer));
        // A write the end of the process cut apart is no line: the file
        // holds no answer after its finished lines.
        std::fs::write(&file.0, b"class\n{\"Err\":\"cut\"").expect("cut apart");
        let look = file.look();
        assert_eq!(look.lines, 1);
        assert_eq!(look.answer, None);
    }

    #[test]
    fn a_line_replaces_the_part_of_a_failed_write() {
        let file = AnswerFile::new().expect("answer file");
        // A write that failed part way leaves a part of its line.
        std::fs::write(&file.0, b"class\n{\"Err\":\"par").expect("part of a line");
        let answer: Answer = Err("no audio plugin".into());
        let mut line = serde_json::to_vec(&answer).expect("answer text");
        line.push(b'\n');
        assert!(write_line(&file.0, &line));
        let look = file.look();
        assert_eq!(look.lines, 2);
        assert_eq!(look.answer, Some(answer));
    }

    #[test]
    fn a_worker_program_that_does_not_start_is_no_fault_of_the_bundle() {
        let program = WorkerProgram {
            program: std::env::temp_dir().join("rustel-vst3-no-such-program"),
            args: Vec::new(),
        };
        let closed = AtomicBool::new(false);
        let scan = scan(&program, Path::new("some.vst3"), WORK_LIMIT, &closed);
        assert_eq!(scan, Scan::NotRun);
    }

    /// Not a test in its own right: the entry of the scan process the test
    /// below starts. The host's scan adds the bundle path as the last
    /// argument. With the argument `slow=<millis>`, each audio class of
    /// the fixture starts that many milliseconds later. An ordinary run of
    /// the tests has no bundle path and returns at once.
    #[test]
    fn scan_process_of_the_tests() {
        let args: Vec<String> = std::env::args().collect();
        let Some(bundle) = args.last().filter(|arg| arg.ends_with(".vst3")) else {
            return;
        };
        if let Some(slow) = args.iter().find_map(|arg| arg.strip_prefix("slow=")) {
            // SAFETY: this process is the scan process, and no other thread
            // of it reads a variable now.
            unsafe {
                std::env::set_var(rustel_vst3_fixture::SLOW_CLASS_ENV, slow);
            }
        }
        std::process::exit(serve(Path::new(bundle)));
    }

    /// The limit of the scan is the time of one class of a bundle, not of
    /// the bundle as a whole. Two classes that each take less than the limit,
    /// and together more than the limit and the quit limit of the scan
    /// process, still answer. A class that takes more than the limit fails
    /// its bundle.
    #[test]
    fn the_limit_is_for_one_class_not_the_whole_bundle() {
        // Each class takes 4.5 s, less than the limit of 6 s; the two take
        // 9 s, more than the limit and more than the 8 s at which a
        // watchdog that no progress restarts would end the process.
        const LIMIT: Duration = Duration::from_secs(6);
        const SLOW: u64 = 4500;
        let root = std::env::temp_dir().join(format!("rustel-vst3-limit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("limit test folder");
        let bundle = rustel_vst3_fixture::install(&root);
        let run = |slow: u64| {
            let program = WorkerProgram {
                program: std::env::current_exe().expect("test program path"),
                args: vec![
                    // The full name of the test below, so the child binary
                    // runs it alone.
                    "worker::tests::scan_process_of_the_tests".into(),
                    "--exact".into(),
                    format!("slow={slow}").into(),
                ],
            };
            scan(&program, &bundle, LIMIT, &AtomicBool::new(false))
        };
        let names = |scan: Scan| match scan {
            Scan::Read(Ok(plugins)) => plugins
                .into_iter()
                .map(|plugin| plugin.name)
                .collect::<Vec<_>>(),
            other => panic!("the scan gave {other:?}"),
        };
        assert_eq!(
            names(run(SLOW)),
            [rustel_vst3_fixture::NAME, rustel_vst3_fixture::TONE_NAME]
        );
        // One class of 10.5 s takes more than the limit: the scan gives up
        // on the bundle, and the scan of the next bundle goes on.
        match run(10_500) {
            Scan::Read(Err(reason)) => assert!(
                reason.contains("the plugin process did not answer"),
                "{reason}"
            ),
            other => panic!("the scan gave {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
