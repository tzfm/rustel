//! What the studio says after a panic.
//!
//! The studio catches its own panic, restores the terminal, and reports the
//! fault on the screen the performer already looks at, with the facts a bug
//! report needs. The panic message also goes to `studio.log`, but nobody
//! reads the log during a set. Then the studio asks one question: again, or
//! done.
//!
//! This covers a panic, which is the common fault and the one that unwinds.
//! A segfault or an abort from a driver ends the process and leaves nothing
//! here to run. Catching those needs a second process that watches this
//! one.

use std::io::{BufRead, Write};
use std::sync::{Mutex, OnceLock};

/// What the panic hook saw, kept for the report that follows it.
#[derive(Clone, Debug, Default)]
pub struct CrashReport {
    /// The panic's own message.
    pub message: String,
    /// `file:line` where it happened, when the hook was told.
    pub location: Option<String>,
    /// The thread it happened on, so an engine fault is not read as an
    /// editing one.
    pub thread: String,
}

struct RecordedCrash {
    report: CrashReport,
    thread_id: std::thread::ThreadId,
}

fn slot() -> &'static Mutex<Option<RecordedCrash>> {
    static SLOT: OnceLock<Mutex<Option<RecordedCrash>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// Remember panics as they happen, without changing what already happens
/// to them.
///
/// The previous hook still runs, so the message keeps going to stderr and
/// therefore into `studio.log`: the file stays the complete record, and
/// this only adds a copy the studio can show. Installed once; a second
/// call is a no-op, which keeps a restart from stacking hooks.
pub fn watch_for_panics() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let report = CrashReport {
                message: message_of(info),
                location: info
                    .location()
                    .map(|at| format!("{}:{}", at.file(), at.line())),
                thread: std::thread::current()
                    .name()
                    .unwrap_or("unnamed")
                    .to_owned(),
            };
            // A contained score panic is reported as an error, not a crash.
            if !rustel_runtime::panic_is_contained()
                && let Ok(mut slot) = slot().lock()
            {
                // The first panic is the one that explains the rest: a
                // failure while unwinding is a consequence, not a cause.
                slot.get_or_insert(RecordedCrash {
                    report,
                    thread_id: std::thread::current().id(),
                });
            }
            previous(info);
        }));
    });
}

/// The panic the hook last saw, taken so a later one is not confused with
/// it.
pub fn take_report() -> Option<CrashReport> {
    slot()
        .lock()
        .ok()
        .and_then(|mut slot| slot.take().map(|crash| crash.report))
}

/// The same, left in place: the crash file is written before the screen
/// asks, and both want it.
pub fn peek_report() -> Option<CrashReport> {
    slot()
        .lock()
        .ok()
        .and_then(|slot| slot.as_ref().map(|crash| crash.report.clone()))
}

/// Retire a contained panic on this worker without consuming another thread's
/// fatal report. The panic hook has already forwarded its text to the log.
pub(crate) fn forget_recovered_panic() {
    if let Ok(mut slot) = slot().lock()
        && slot
            .as_ref()
            .is_some_and(|crash| crash.thread_id == std::thread::current().id())
    {
        *slot = None;
    }
}

fn message_of(info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        // A panic carrying something else still happened, and saying so
        // beats an empty report.
        "a panic with no message".to_owned()
    }
}

/// One text the studio had open when it fell over.
#[derive(Clone, Debug)]
pub struct CrashSource {
    /// What the strip called it: a scene name, or which prebake.
    pub name: String,
    pub text: String,
    /// The engine was playing this one.
    pub audible: bool,
}

/// A score long enough to bury the rest of the report is trimmed. Nothing
/// anyone writes live comes near this; a generated one might.
const MAX_SOURCE_BYTES: usize = 64 * 1024;

/// How much of the run to keep in the report.
///
/// Enough to show the shape of what led here - the refusals, the updates,
/// the setup that applied - without turning the file into the log, which
/// is beside it anyway.
const CONTEXT_LINES: usize = 30;

/// One thing that happened before the fault.
#[derive(Clone, Debug)]
pub struct CrashMoment {
    /// Seconds since the studio opened, the clock the musician has.
    pub at: f64,
    pub level: &'static str,
    pub kind: String,
    pub text: String,
}

/// The tape, when the set was recording one.
#[derive(Clone, Debug)]
pub struct CrashTape {
    pub path: std::path::PathBuf,
    pub saves: usize,
}

/// Write everything needed to reproduce the fault into one file, and
/// return where it went.
///
/// The scores are the point. A path names a file only the person who
/// crashed can read, and the fault usually lives in what was being
/// evaluated - so the text travels with the report rather than being
/// something they have to remember to attach.
pub fn write_crash_file(
    report: Option<&CrashReport>,
    set: &std::path::Path,
    sources: &[CrashSource],
    before: &[CrashMoment],
    tape: Option<&CrashTape>,
    directory: &std::path::Path,
) -> std::io::Result<std::path::PathBuf> {
    let stamp =
        rustel_runtime::session_log::dashed_timestamp(rustel_runtime::session_log::unix_now());
    let path = directory.join(format!("crash-{stamp}.md"));
    std::fs::create_dir_all(directory)?;
    let mut file = std::fs::File::create(&path)?;
    writeln!(file, "# {} crashed", rustel_runtime::product::NAME)?;
    writeln!(file)?;
    match report {
        Some(report) => {
            writeln!(file, "    {}", report.message)?;
            match &report.location {
                Some(location) => {
                    writeln!(file, "    at {location}, on the {} thread", report.thread)?;
                }
                None => writeln!(file, "    on the {} thread", report.thread)?,
            }
        }
        None => writeln!(file, "    no message was recorded")?,
    }
    writeln!(file)?;
    writeln!(
        file,
        "- engine `{}`",
        rustel_runtime::product::engine_identity()
    )?;
    writeln!(file, "- set `{}`", set.display())?;
    writeln!(file, "- platform `{}`", std::env::consts::OS)?;
    if let Some(tape) = tape {
        // The tape is the whole set, edit by edit, on its original clock.
        // Nothing reproduces a fault as exactly, so say how to play it.
        writeln!(
            file,
            "- tape `{}` - {} save{}, `{} replay` plays the set back",
            tape.path.display(),
            tape.saves,
            if tape.saves == 1 { "" } else { "s" },
            rustel_runtime::product::COMMAND_NAME
        )?;
    }

    if !before.is_empty() {
        writeln!(file)?;
        writeln!(file, "## what happened first")?;
        writeln!(file)?;
        writeln!(file, "```")?;
        for moment in before.iter().rev().take(CONTEXT_LINES).rev() {
            writeln!(
                file,
                "{:>8.1}s {:<5} [{}] {}",
                moment.at,
                moment.level,
                moment.kind,
                moment.text.replace('\n', " ⏎ ")
            )?;
        }
        writeln!(file, "```")?;
    }

    for source in sources {
        writeln!(file)?;
        let playing = if source.audible { " (playing)" } else { "" };
        writeln!(file, "## {}{playing}", source.name)?;
        writeln!(file)?;
        writeln!(file, "```javascript")?;
        let text = source.text.as_str();
        match text.char_indices().nth(MAX_SOURCE_BYTES) {
            Some((cut, _)) => {
                writeln!(file, "{}", &text[..cut])?;
                writeln!(file, "// … trimmed, {} bytes in all", text.len())?;
            }
            None => writeln!(file, "{text}")?,
        }
        writeln!(file, "```")?;
    }
    Ok(path)
}

/// What the performer decides once the screen is back.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AfterCrash {
    /// Open the studio again, from the top.
    Restart,
    /// Leave.
    Quit,
}

/// Print the report and ask. Never called while the alternate screen is
/// up: the terminal is the performer's own by the time this runs.
pub fn report_and_ask(
    report: Option<&CrashReport>,
    set: &std::path::Path,
    log: Option<&std::path::Path>,
    crash_file: Option<&std::path::Path>,
    out: &mut impl Write,
    input: &mut impl BufRead,
) -> AfterCrash {
    write_report(report, set, log, crash_file, out);
    let _ = write!(out, "\n  Enter plays it again · q quits · ");
    let _ = out.flush();
    let mut answer = String::new();
    match input.read_line(&mut answer) {
        // End of input is not an answer: nobody is there to give one, and
        // a studio nobody asked for is worse than none.
        Ok(0) => AfterCrash::Quit,
        Ok(_) => {
            if answer.trim().eq_ignore_ascii_case("q") {
                AfterCrash::Quit
            } else {
                AfterCrash::Restart
            }
        }
        Err(_) => AfterCrash::Quit,
    }
}

/// The report itself, which is also what a bug report wants pasted into it.
pub fn write_report(
    report: Option<&CrashReport>,
    set: &std::path::Path,
    log: Option<&std::path::Path>,
    crash_file: Option<&std::path::Path>,
    out: &mut impl Write,
) {
    let _ = writeln!(out, "\n  the studio stopped: it hit a fault of its own");
    if let Some(report) = report {
        let _ = writeln!(out, "\n  {}", report.message);
        if let Some(location) = &report.location {
            let _ = writeln!(out, "  at {location}, on the {} thread", report.thread);
        } else {
            let _ = writeln!(out, "  on the {} thread", report.thread);
        }
    } else {
        // Caught, but the hook never saw it: still say so rather than
        // print a confident blank.
        let _ = writeln!(out, "\n  no message was recorded");
    }
    let _ = writeln!(out, "\n  set     {}", set.display());
    let _ = writeln!(
        out,
        "  engine  {}",
        rustel_runtime::product::engine_identity()
    );
    if let Some(log) = log {
        let _ = writeln!(out, "  log     {}", log.display());
    }
    let _ = writeln!(
        out,
        "\n  this is a bug in {}.",
        rustel_runtime::product::NAME
    );
    match crash_file {
        // The scores are in there, so the report is one file rather than a
        // thing to remember to attach.
        Some(path) => {
            let _ = writeln!(
                out,
                "  send this and it can be reproduced:\n  {}",
                path.display()
            );
        }
        None => {
            let _ = writeln!(
                out,
                "  the lines above are what a report needs, and the log holds the rest."
            );
        }
    }
}

#[cfg(test)]
static HOOK_TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod recovery_tests {
    use super::*;

    #[test]
    fn a_recovered_worker_panic_does_not_mask_a_later_fatal_report() {
        let _guard = HOOK_TEST_LOCK.lock().unwrap();
        take_report();
        watch_for_panics();
        assert!(std::panic::catch_unwind(|| panic!("recovered score panic")).is_err());
        assert!(peek_report().is_some());

        forget_recovered_panic();
        assert!(peek_report().is_none());

        assert!(std::panic::catch_unwind(|| panic!("later fatal panic")).is_err());
        let report = take_report().expect("new fatal report");
        assert_eq!(report.message, "later fatal panic");
    }

    #[test]
    fn recovery_leaves_another_threads_panic_report_available() {
        let _guard = HOOK_TEST_LOCK.lock().unwrap();
        take_report();
        watch_for_panics();
        std::thread::spawn(|| {
            assert!(std::panic::catch_unwind(|| panic!("other thread fatal panic")).is_err());
        })
        .join()
        .expect("panic stayed inside the other thread's boundary");

        forget_recovered_panic();
        let report = take_report().expect("other thread report was retained");
        assert_eq!(report.message, "other thread fatal panic");
    }

    #[test]
    fn a_contained_score_panic_leaves_no_crash_report() {
        let _guard = HOOK_TEST_LOCK.lock().unwrap();
        take_report();
        watch_for_panics();
        let mut session = rustel_runtime::Session::new().expect("session");
        session
            .with_panic_recovery::<()>(0.0, |_| panic!("contained score panic"))
            .expect_err("the panic is contained");
        assert!(peek_report().is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn report() -> CrashReport {
        CrashReport {
            message: "index out of bounds: the len is 5 but the index is 5".into(),
            location: Some("crates/studio/src/app.rs:513".into()),
            thread: "main".into(),
        }
    }

    #[test]
    fn the_report_says_what_a_bug_report_needs() {
        let mut out = Vec::new();
        write_report(
            Some(&report()),
            Path::new("/sets/tonight"),
            Some(Path::new("/sessions/studio.log")),
            Some(Path::new("/sessions/crash-2026-09-02.md")),
            &mut out,
        );
        let text = String::from_utf8(out).expect("utf-8");
        assert!(text.contains("the len is 5 but the index is 5"), "{text}");
        assert!(text.contains("app.rs:513"), "{text}");
        assert!(text.contains("main thread"), "{text}");
        assert!(text.contains("/sets/tonight"), "{text}");
        assert!(text.contains("/sessions/studio.log"), "{text}");
        assert!(text.contains(rustel_runtime::product::NAME), "{text}");
        assert!(text.contains("crash-2026-09-02.md"), "{text}");
        assert!(text.contains("send this"), "{text}");
    }

    #[test]
    fn a_panic_nobody_recorded_still_reports_the_rest() {
        let mut out = Vec::new();
        write_report(None, Path::new("/sets/tonight"), None, None, &mut out);
        let text = String::from_utf8(out).expect("utf-8");
        assert!(text.contains("no message was recorded"), "{text}");
        assert!(text.contains("/sets/tonight"), "{text}");
    }

    #[test]
    fn enter_plays_it_again_and_q_quits() {
        for (typed, expected) in [
            ("\n", AfterCrash::Restart),
            ("   \n", AfterCrash::Restart),
            ("q\n", AfterCrash::Quit),
            ("Q\n", AfterCrash::Quit),
            // Nobody there to answer.
            ("", AfterCrash::Quit),
        ] {
            let mut out = Vec::new();
            let mut input = typed.as_bytes();
            assert_eq!(
                report_and_ask(
                    Some(&report()),
                    Path::new("/sets/tonight"),
                    None,
                    None,
                    &mut out,
                    &mut input,
                ),
                expected,
                "typing {typed:?}"
            );
        }
    }

    #[test]
    fn the_crash_file_carries_the_scores_that_reproduce_it() {
        let directory = tempfile::tempdir().expect("temp dir");
        let sources = vec![
            CrashSource {
                name: "prebake (local)".into(),
                text: "globalThis.riff = () => note('c')".into(),
                audible: false,
            },
            CrashSource {
                name: "drop".into(),
                text: "$: riff().s(\"piano\")".into(),
                audible: true,
            },
        ];

        let before = vec![
            CrashMoment {
                at: 12.5,
                level: "warn ",
                kind: "check".into(),
                text: "refused - line 16: Unexpected token".into(),
            },
            CrashMoment {
                at: 19.0,
                level: "info ",
                kind: "update".into(),
                text: "generation 2 installed".into(),
            },
        ];
        let tape = CrashTape {
            path: "/sessions/live-2026-09-02.rustel-session".into(),
            saves: 7,
        };

        let path = write_crash_file(
            Some(&report()),
            Path::new("/sets/tonight"),
            &sources,
            &before,
            Some(&tape),
            directory.path(),
        )
        .expect("write");
        let text = std::fs::read_to_string(&path).expect("read");

        assert!(
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("crash-")),
            "{path:?}"
        );
        assert!(text.contains("the len is 5 but the index is 5"), "{text}");
        assert!(text.contains("app.rs:513"), "{text}");
        // Both texts, and which one was sounding.
        assert!(text.contains("globalThis.riff"), "{text}");
        assert!(text.contains("$: riff().s(\"piano\")"), "{text}");
        assert!(text.contains("## drop (playing)"), "{text}");
        assert!(text.contains("## prebake (local)"), "{text}");
        assert!(text.contains("```javascript"), "{text}");
        // The run that led here, and the tape that replays it.
        assert!(text.contains("what happened first"), "{text}");
        assert!(text.contains("Unexpected token"), "{text}");
        assert!(text.contains("generation 2 installed"), "{text}");
        assert!(text.contains("12.5s"), "{text}");
        assert!(text.contains("live-2026-09-02.rustel-session"), "{text}");
        assert!(text.contains("7 saves"), "{text}");
    }

    /// A long run is trimmed to its most recent moments: the ones next to
    /// the fault are the ones that explain it.
    #[test]
    fn only_the_moments_nearest_the_fault_are_kept() {
        let directory = tempfile::tempdir().expect("temp dir");
        let before = (0..CONTEXT_LINES + 40)
            .map(|index| CrashMoment {
                at: index as f64,
                level: "info ",
                kind: "update".into(),
                text: format!("moment {index}"),
            })
            .collect::<Vec<_>>();

        let path = write_crash_file(None, Path::new("/x"), &[], &before, None, directory.path())
            .expect("write");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("moment 69"), "the last moment is missing");
        assert!(!text.contains("moment 0\n"), "an old moment was kept");
        assert!(!text.contains("tape"), "a set with no tape claimed one");
    }

    #[test]
    fn a_score_too_long_to_send_is_trimmed_rather_than_dropped() {
        let directory = tempfile::tempdir().expect("temp dir");
        let sources = vec![CrashSource {
            name: "generated".into(),
            text: "x".repeat(MAX_SOURCE_BYTES * 2),
            audible: false,
        }];

        let path = write_crash_file(
            None,
            Path::new("/sets/x"),
            &sources,
            &[],
            None,
            directory.path(),
        )
        .expect("write");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("trimmed"), "a long score was not trimmed");
        assert!(text.len() < MAX_SOURCE_BYTES * 2, "the trim did not hold");
    }

    #[test]
    fn the_hook_keeps_the_first_panic_and_hands_it_over_once() {
        let _guard = HOOK_TEST_LOCK.lock().unwrap();
        take_report();
        watch_for_panics();
        // A second install is a no-op rather than a second hook.
        watch_for_panics();

        let caught = std::panic::catch_unwind(|| panic!("the first one"));
        assert!(caught.is_err());
        let report = take_report().expect("the hook recorded it");
        assert!(report.message.contains("the first one"), "{report:?}");
        assert!(
            report.location.is_some_and(|at| at.contains("crash.rs")),
            "the location was lost"
        );
        assert!(take_report().is_none(), "the report was handed over twice");
    }
}
