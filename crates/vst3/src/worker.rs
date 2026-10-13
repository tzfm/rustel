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
//! A scan process that ends with no answer, or gives no answer in the time
//! limit, fails its bundle and no more. A scan that did not run, with no
//! file or no process from the system, fails no bundle.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::cache::Scanned;
use crate::host::{Plugin, WorkerProgram, load_bundle};

/// The scan process reads the path of its answer file from this variable.
const ANSWER_ENV: &str = "RUSTEL_VST3_WORKER";
/// A load that runs longer than this gives no answer.
const WORK_LIMIT: Duration = Duration::from_secs(120);
/// A scan process with its answer in the file has this long to unload the
/// bundle before the host ends the process.
const QUIT_LIMIT: Duration = Duration::from_secs(2);

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

/// The scan process: loads the bundle, writes its plugins to the answer
/// file, and returns the exit code of the process. The worker program calls
/// this with the bundle path the host gave as the last argument.
pub fn serve(bundle: &Path) -> i32 {
    let Some(file) = std::env::var_os(ANSWER_ENV) else {
        return 2;
    };
    // A load with no end must not keep this process after its host.
    std::thread::spawn(|| {
        std::thread::sleep(WORK_LIMIT + QUIT_LIMIT);
        quit(3);
    });
    let loaded = load_bundle(bundle);
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
        written = std::fs::write(&file, &line).is_ok();
        if written {
            break;
        }
    }
    // The unload runs the code of the plugin: the answer is in the file
    // before.
    drop(loaded);
    if written { 0 } else { 2 }
}

/// The file a scan process writes its answer to. The file goes with this
/// value.
struct AnswerFile(PathBuf);

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

    /// The answer, after the scan process wrote all of it.
    fn read(&self) -> Option<Answer> {
        let bytes = std::fs::read(&self.0).ok()?;
        serde_json::from_slice(bytes.strip_suffix(b"\n")?).ok()
    }
}

impl Drop for AnswerFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Loads a bundle in a scan process and gives its plugins. `closed` ends
/// the wait: the host ends.
pub(crate) fn scan(program: &WorkerProgram, bundle: &Path, closed: &AtomicBool) -> Scan {
    let Ok(file) = AnswerFile::new() else {
        return Scan::NotRun;
    };
    let mut command = Command::new(&program.program);
    command
        .args(&program.args)
        .arg(bundle)
        .env(ANSWER_ENV, &file.0)
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
    let start = Instant::now();
    let answer = loop {
        if let Some(answer) = file.read() {
            break answer;
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
                    file.read()
                });
                let ended = || Err(format!("the plugin process ended ({status})"));
                break read.unwrap_or_else(ended);
            }
            Err(error) => break Err(error.to_string()),
        }
        if closed.load(Ordering::Relaxed) || start.elapsed() > WORK_LIMIT {
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
        assert!(file.read().is_none(), "an empty file is no answer");
        let answer: Answer = Ok(vec![Scanned {
            name: "Tone".into(),
            vendor: "rustel".into(),
            categories: "Instrument|Synth".into(),
        }]);
        let mut line = serde_json::to_vec(&answer).expect("answer text");
        std::fs::write(&file.0, &line).expect("part of the answer");
        assert!(file.read().is_none(), "the scan process still writes");
        line.push(b'\n');
        std::fs::write(&file.0, &line).expect("answer");
        assert_eq!(file.read(), Some(answer));
        std::fs::write(&file.0, b"{\"Err\":\"no audio plugin\"}\n").expect("failure");
        assert_eq!(file.read(), Some(Err("no audio plugin".into())));
        let path = file.0.clone();
        drop(file);
        assert!(!path.exists(), "the file goes with its owner");
    }

    #[test]
    fn a_worker_program_that_does_not_start_is_no_fault_of_the_bundle() {
        let program = WorkerProgram {
            program: std::env::temp_dir().join("rustel-vst3-no-such-program"),
            args: Vec::new(),
        };
        let closed = AtomicBool::new(false);
        let scan = scan(&program, Path::new("some.vst3"), &closed);
        assert_eq!(scan, Scan::NotRun);
    }
}
