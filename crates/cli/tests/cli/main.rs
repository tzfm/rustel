/*
rustel - the `rustel` CLI, driven as a subprocess
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! The headless product surface, exercised the way a user reaches it.
//!
//! In-process `Session` tests do not cover argument parsing, exit codes, the
//! JSON written to stdout, or a failure that occurs only in the binary.
//!
//! Each test spawns `CARGO_BIN_EXE_rustel`. A panic is never an acceptable
//! outcome: a mistyped expression in a live-coding session must produce a
//! diagnostic and a non-zero exit, not a crash.

use std::path::PathBuf;
use std::process::{Child, Command, Output};

mod function_score;

fn rustel() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rustel"));
    command.env("RUSTEL_NO_UPDATE_CHECK", "1");
    command
}

/// Run with a hard wall-clock bound.
///
/// A subprocess that never returns is worse than one that crashes: without a
/// timeout the test suite hangs instead of failing, and a supervisor cannot
/// tell it from slow work. `--duration inf` did exactly that.
fn run(args: &[&str]) -> Output {
    let child = rustel()
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn rustel");
    wait_for_output(child, args)
}

fn wait_for_output(mut child: Child, args: &[&str]) -> Output {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        if child.try_wait().expect("wait").is_some() {
            return child.wait_with_output().expect("collect output");
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{args:?} did not terminate within 60s");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[cfg(unix)]
fn run_with_stack_limit(args: &[&str], bytes: libc::rlim_t) -> Option<Output> {
    use std::os::unix::process::CommandExt;

    let mut command = rustel();
    command
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // SAFETY: `setrlimit` is async-signal-safe and the closure only lowers the
    // child process's stack resource before exec. It does not touch parent
    // state or allocate on success.
    unsafe {
        command.pre_exec(move || {
            // Lower only the soft limit. macOS reserves the main thread's
            // stack from the hard limit at exec and answers EINVAL when the
            // hard limit is lowered. The soft limit bounds the child's
            // threads. `getrlimit` is a syscall, so it is async-signal-safe.
            let mut current = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::getrlimit(libc::RLIMIT_STACK, &mut current) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let limit = libc::rlimit {
                rlim_cur: bytes.min(current.rlim_max),
                rlim_max: current.rlim_max,
            };
            if libc::setrlimit(libc::RLIMIT_STACK, &limit) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            // macOS can refuse `setrlimit(RLIMIT_STACK)` with EINVAL. The
            // helper then cannot create a child with a small stack, so the
            // caller skips the bounded run. A missing or unrunnable binary
            // gives NotFound or PermissionDenied from the exec and still fails.
            if error.raw_os_error() == Some(libc::EINVAL) {
                eprintln!("this platform will not bound a child's stack ({error}); skipping");
                return None;
            }
            panic!("spawn stack-bounded rustel: {error}");
        }
    };
    Some(wait_for_output(child, args))
}

/// The whole output, asserting the run succeeded.
fn ok_output(args: &[&str]) -> Output {
    let out = run(args);
    assert!(
        out.status.success(),
        "{args:?} failed with {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// stdout as text, asserting the run succeeded.
fn ok_stdout(args: &[&str]) -> String {
    String::from_utf8_lossy(&ok_output(args).stdout).into_owned()
}

/// A failure must be reported: a non-zero exit, but never a panic.
///
/// 101 is what Rust returns for an unwinding panic, so it is the specific code
/// this surface must never produce - the caller cannot tell a diagnosed error
/// from a crash any other way.
fn assert_reported_failure(args: &[&str], expect_in_stderr: &str) {
    let out = run(args);
    let code = out.status.code();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "{args:?} should have failed but exited 0\nstdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    // `None` means that a signal killed the process: a segfault or an abort.
    // That is a crash, not a reported failure.
    assert!(
        code.is_some(),
        "{args:?} was killed by a signal rather than exiting: that is a crash, \
         not a diagnosed error.\n{stderr}"
    );
    assert_ne!(
        code,
        Some(101),
        "{args:?} PANICKED. User input must never crash the binary.\n{stderr}"
    );
    // A diagnostic is part of the contract: exiting non-zero in silence tells
    // the user nothing.
    assert!(
        !stderr.trim().is_empty(),
        "{args:?} failed with exit {code:?} but printed nothing to stderr"
    );
    assert!(
        stderr
            .to_lowercase()
            .contains(&expect_in_stderr.to_lowercase()),
        "{args:?} failed without naming {expect_in_stderr:?}\nstderr: {stderr}"
    );
}

/// The `data` chunk payload of a RIFF file.
///
/// A chunk can follow the audio, so "everything after byte 44" is not the
/// audio: trailing bytes would make a silent render look audible. Parse the
/// chunk instead.
fn wav_data(bytes: &[u8]) -> &[u8] {
    let at = bytes
        .windows(4)
        .position(|window| window == b"data")
        .expect("data chunk");
    let len =
        u32::from_le_bytes([bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]]) as usize;
    &bytes[at + 8..at + 8 + len]
}

/// Bytes per frame of the stereo PCM16 bounces these tests read.
const PCM_FRAME: usize = 4;

/// Bytes of 48 kHz stereo PCM16 in `secs`.
fn pcm_bytes(secs: f64) -> usize {
    (secs * 48_000.0) as usize * PCM_FRAME
}

/// Whether PCM16 holds only zero samples.
fn silent(pcm: &[u8]) -> bool {
    pcm.as_chunks::<2>()
        .0
        .iter()
        .all(|sample| *sample == [0, 0])
}

/// The PCM body of the WAV at `path`, which must hold exactly `secs` of
/// 48 kHz stereo PCM16.
fn wav_pcm_of_length(path: &std::path::Path, secs: f64) -> Vec<u8> {
    let bytes = std::fs::read(path).expect("bounce");
    let data = wav_data(&bytes);
    assert_eq!(
        data.len(),
        pcm_bytes(secs),
        "{} does not hold {secs} s",
        path.display()
    );
    data.to_vec()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rustel-cli-tests-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir.join(name)
}

/// A score plays through a plugin from a folder in `RUSTEL_VST3_PATH`: with
/// a value by name, with a preset file, and dry with a notice when the name
/// of the value is wrong. The plugin is the fixture gain.
#[cfg(feature = "vst")]
#[test]
fn a_render_goes_through_the_plugin_a_score_names() {
    let directory = tempfile::tempdir().unwrap();
    let plugins = directory.path().join("plugins");
    let config = directory.path().join("config");
    let presets = config.join("vst").join(rustel_vst3_fixture::NAME);
    std::fs::create_dir_all(&plugins).unwrap();
    std::fs::create_dir_all(&presets).unwrap();
    rustel_vst3_fixture::install(&plugins);
    std::fs::write(
        presets.join("Half Level.vstpreset"),
        rustel_vst3_fixture::preset(0.5, 0.0),
    )
    .unwrap();
    let invoke = |args: &[&str]| {
        let child = rustel()
            .args(args)
            .env("RUSTEL_VST3_PATH", &plugins)
            .env("RUSTEL_CONFIG_DIR", &config)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let output = wait_for_output(child, args);
        assert!(output.status.success(), "{output:?}");
        output
    };
    let render_score = |score: &str| {
        let path = directory.path().join("take.wav");
        let output = invoke(&[
            "render",
            "-e",
            score,
            "-o",
            path.to_str().unwrap(),
            "--cycles",
            "1",
        ]);
        let samples: Vec<i16> = wav_pcm_of_length(&path, 2.0)
            .as_chunks::<2>()
            .0
            .iter()
            .map(|sample| i16::from_le_bytes(*sample))
            .collect();
        (samples, String::from_utf8(output.stderr).unwrap())
    };
    let render = |effect: &str| render_score(&format!(r#"note("c3").s("sine"){effect}"#));
    let (dry, _) = render("");
    assert!(dry.iter().any(|sample| sample.abs() > 1_000));
    let scaled = |wet: &[i16], gain: f32| {
        wet.iter()
            .zip(&dry)
            .all(|(wet, dry)| (f32::from(*wet) - f32::from(*dry) * gain).abs() <= 1.0)
    };
    let (quarter, _) = render(r#".vst("rustel fixture", { gain: 0.25 })"#);
    assert!(scaled(&quarter, 0.25));
    let (half, _) = render(r#".vst("rustel fixture", { preset: "Half Level" })"#);
    assert!(scaled(&half, 0.5));
    let (wrong, notice) = render(r#".vst("rustel fixture", { mix: 0.25 })"#);
    assert_eq!(wrong, dry);
    assert!(
        notice.contains("Rustel Fixture has no parameter with the name 'mix'"),
        "{notice}"
    );

    // The second plugin of the bundle is an instrument. With `.vsti()` the
    // plugin makes the sound of the note: a 440 Hz sine with a peak of
    // 0.8 * 0.25, for the length of the note. The sample the score names
    // is not heard and not loaded.
    let tone = |effect: &str| {
        let (samples, notice) = render_score(&format!(
            r#"note("a4 ~").s("nosuchsample").vsti("rustel fixture tone"){effect}"#
        ));
        let left: Vec<i16> = samples.iter().step_by(2).copied().collect();
        (left, notice)
    };
    let (alone, notice) = tone("");
    assert!(notice.is_empty(), "{notice}");
    let (note, rest) = alone.split_at(48_000);
    let rises = note.windows(2).filter(|pair| pair[0] <= 0 && pair[1] > 0);
    assert_eq!(rises.count(), 440);
    let peak = |samples: &[i16]| samples.iter().map(|sample| sample.unsigned_abs()).max();
    let level = f32::from(peak(note).unwrap());
    let wanted = 0.8 * rustel_vst3_fixture::TONE_LEVEL * 32_767.0;
    assert!((level - wanted).abs() < 20.0, "peak {level}");
    assert!(rest.iter().all(|sample| *sample == 0));
    // With `.vst()` on the same note, the instrument goes through the
    // effect. The level of the note is `gain` times `velocity`.
    for effect in [r#".vst("rustel fixture", { gain: 0.5 })"#, ".velocity(0.5)"] {
        let (halved, _) = tone(effect);
        assert!((f32::from(peak(&halved).unwrap()) - wanted * 0.5).abs() < 20.0);
    }
    // The wrong call for a plugin gives the reason, and no sound of the
    // plugin.
    let (silent, notice) = render_score(r#"note("a4").vsti("rustel fixture")"#);
    assert!(silent.iter().all(|sample| *sample == 0));
    assert!(
        notice.contains("Rustel Fixture is an effect: use .vst()"),
        "{notice}"
    );
    let (_, notice) = render_score(r#"note("a4").vst("rustel fixture tone")"#);
    assert!(
        notice.contains("Rustel Fixture Tone is an instrument: use .vsti()"),
        "{notice}"
    );

    let listed = invoke(&["vst"]);
    assert!(String::from_utf8_lossy(&listed.stdout).contains(rustel_vst3_fixture::NAME));
    let one = invoke(&["vst", "fixture", "--json"]);
    let one: serde_json::Value = serde_json::from_slice(&one.stdout).unwrap();
    assert_eq!(one["params"][0]["key"], "gain");
    assert_eq!(one["params"][1]["key"], "beatgate");
    assert_eq!(one["presets"][0], "Half Level");

    // With no scan cache, the list command tests the bundle in a process
    // of its own. The list has the 2 plugins of the bundle by name with no
    // load, and the cache file keeps them for the next start.
    let fresh = directory.path().join("fresh");
    let child = rustel()
        .args(["vst"])
        .env("RUSTEL_VST3_PATH", &plugins)
        .env("RUSTEL_CONFIG_DIR", &fresh)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let scanned = wait_for_output(child, &["vst"]);
    let list = String::from_utf8_lossy(&scanned.stdout);
    assert!(list.contains(rustel_vst3_fixture::TONE_NAME), "{scanned:?}");
    let cache = std::fs::read_to_string(fresh.join("vst").join("scan.json")).unwrap();
    assert!(cache.contains(rustel_vst3_fixture::TONE_NAME), "{cache}");

    // Each bundle runs in a process of its own. A plugin with a fault at
    // its load ends that process and not the command: the command reports
    // the plugin and ends in the ordinary way.
    let faulty = |args: &[&str], fault: &str| {
        let child = rustel()
            .args(args)
            .env("RUSTEL_VST3_PATH", &plugins)
            .env("RUSTEL_CONFIG_DIR", &config)
            .env(rustel_vst3_fixture::ABORT_ENV, fault)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        wait_for_output(child, args)
    };
    let at_load = faulty(&["vst", "fixture"], "load");
    assert_eq!(at_load.status.code(), Some(1), "{at_load:?}");
    let said = String::from_utf8_lossy(&at_load.stderr);
    assert!(said.contains("the plugin process ended"), "{at_load:?}");

    // A fault in an audio block ends the plugin process too. The render
    // goes on with the dry note, and says what happened.
    let path = directory.path().join("fault.wav");
    let score = r#"note("c3").s("sine").vst("rustel fixture", { gain: 0.25 })"#;
    let args = ["render", "-e", score, "-o", path.to_str().unwrap()];
    let in_audio = faulty(
        &[&args[..], &["--cycles", "1"]].concat(),
        rustel_vst3_fixture::ABORT_IN_AUDIO,
    );
    assert!(in_audio.status.success(), "{in_audio:?}");
    let said = String::from_utf8_lossy(&in_audio.stderr);
    assert!(said.contains("the plugin process ended"), "{in_audio:?}");
    let kept: Vec<i16> = wav_pcm_of_length(&path, 2.0)
        .as_chunks::<2>()
        .0
        .iter()
        .map(|sample| i16::from_le_bytes(*sample))
        .collect();
    assert_eq!(kept, dry);
}

#[test]
fn config_get_and_set_use_the_user_directory() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config");
    let invoke = |args: &[&str]| {
        let child = rustel()
            .args(args)
            .env("RUSTEL_CONFIG_DIR", &config)
            .current_dir(directory.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        wait_for_output(child, args)
    };
    let output = invoke(&["config", "get", "check_updates"]);
    assert!(output.status.success());
    assert_eq!(output.stdout, b"true\n");
    assert!(output.stderr.is_empty());
    assert!(!config.exists());

    for value in ["false", "true"] {
        let output = invoke(&["config", "set", "check_updates", value]);
        assert!(output.status.success(), "{output:?}");
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
        let output = invoke(&["config", "get", "check_updates"]);
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("{value}\n")
        );
        let settings: serde_json::Value =
            serde_json::from_slice(&std::fs::read(config.join("rustel.json")).unwrap()).unwrap();
        assert_eq!(settings["check_updates"], value == "true");
    }
    assert!(!directory.path().join(".rustel").exists());
    let before = std::fs::read(config.join("rustel.json")).unwrap();
    for args in [
        vec!["config", "set", "check_updates", "yes"],
        vec!["config", "set", "unknown", "false"],
        vec!["config", "get", "check_updates", "--global"],
    ] {
        let output = invoke(&args);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert_eq!(std::fs::read(config.join("rustel.json")).unwrap(), before);
    }
}

/// A scratch tape recording each `(at, status, source)` save in `mode`. A
/// normal tape drops rejected saves, as the recorder does.
fn recorded_tape(
    name: &str,
    mode: rustel_runtime::session_log::SessionMode,
    saves: &[(f64, rustel_runtime::session_log::SaveStatus, &str)],
) -> PathBuf {
    use rustel_runtime::session_log::SessionRecorder;

    let tape = scratch(name);
    let mut recorder = SessionRecorder::create(tape.clone(), mode, None).expect("recorder");
    for (at, status, source) in saves {
        recorder.record_save(*at, *status, &format!("{source}\n"), None);
    }
    tape
}

/// A scratch tape recording each `(at, source)` save as installed.
fn installed_tape(name: &str, saves: &[(f64, &str)]) -> PathBuf {
    use rustel_runtime::session_log::{SaveStatus, SessionMode};

    let saves: Vec<_> = saves
        .iter()
        .map(|(at, source)| (*at, SaveStatus::Installed, *source))
        .collect();
    recorded_tape(name, SessionMode::Debug, &saves)
}

/// The file is a WAV whose PCM body is not silent.
fn assert_audible(path: &std::path::Path) {
    let bytes = std::fs::read(path).expect("bounce");
    assert_eq!(&bytes[..4], b"RIFF", "{}", path.display());
    assert!(!silent(wav_data(&bytes)), "{} is silent", path.display());
}

/// Structured stderr arrives as JSON lines. A watched live route installs
/// session recording first, so its error line is preceded by a
/// `session_recording` notice; match per line instead of one document.
fn structured_lines(stderr: &[u8]) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(stderr)
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Sources whose graphs need the JavaScript host at query time: callbacks and
/// JS-owned values. Both classes must run without a panic exit.
const HOST_REQUIRED_SOURCES: &[&str] = &[
    r#"note("c e g").every(fastcat(2, 3), x => x.fast(2))"#,
    r#"s("bd sd").stepBind(x => fastcat(x, x))"#,
    r#"note("[c,e,g]").arpWith(haps => fastcat(haps[2], haps[0]))"#,
    r#"wchooseCycles([note("[c,e,g]").arpWith(haps => haps[0]), pure(1).arpWith(haps => haps[0])], [note("d"), 0])"#,
    r#"note("c d").pickF(0, pure([x => x.fast(2)]))"#,
    r#"s("bd").echoWith(fastcat(2, 3), 1/8, function (p, i) {
      if (arguments.length !== 2 || !Number.isInteger(i)) {
        throw new Error('echoWith index missing');
      }
      return p.gain(1 / (i + 1));
    })"#,
    r#"pure(['bd']).fast(2)"#,
];

/// Construction is finite; only Pattern.query enters the runaway loop.
///
/// This distinction is load-bearing for the query-time CPU boundary. Reusing a
/// top-level `while (true)` would keep testing score construction and could
/// stay green if every query route silently lost its own deadline.
const QUERY_TIME_RUNAWAY: &str = r#"
  globalThis.__queryTurnConstructed = true;
  new Pattern(state => {
    globalThis.__queryTurnEntered =
      (globalThis.__queryTurnEntered ?? 0) + 1;
    while (true) {}
  })
"#;

/// A small subprocess batch with one shared hard deadline.
///
/// The five unsignalled query routes each own a two-second deadline. Running
/// them serially would add ten seconds to the ordinary suite; running them as
/// one bounded batch keeps the same product coverage without multiplying wall
/// time. Drop is a supervisor of last resort: every still-live child is killed
/// and reaped if an assertion or timeout fires.
struct CliBatch {
    args: Vec<Vec<String>>,
    children: Vec<Option<Child>>,
}

impl CliBatch {
    fn spawn(cases: &[Vec<String>]) -> Self {
        let children = cases
            .iter()
            .map(|args| {
                Some(
                    rustel()
                        .args(args)
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::piped())
                        .spawn()
                        .unwrap_or_else(|error| panic!("spawn {args:?}: {error}")),
                )
            })
            .collect();
        Self {
            args: cases.to_vec(),
            children,
        }
    }

    #[cfg(unix)]
    fn signal_all(&mut self, signal: i32) {
        for (args, child) in self.args.iter().zip(self.children.iter_mut()) {
            let child = child.as_mut().expect("batch child still owned");
            assert!(
                child.try_wait().expect("poll before signal").is_none(),
                "{args:?} exited before signal {signal}; the fixture never held a running query"
            );
        }
        for (args, child) in self.args.iter().zip(self.children.iter()) {
            let child = child.as_ref().expect("batch child still owned");
            // SAFETY: each pid belongs to a live child this batch owns and has
            // just been checked as unreaped.
            let sent = unsafe { libc::kill(child.id() as libc::pid_t, signal) };
            assert_eq!(
                sent,
                0,
                "failed to deliver signal {signal} to {args:?}: {}",
                std::io::Error::last_os_error()
            );
        }
    }

    fn collect(mut self, timeout: std::time::Duration) -> Vec<Output> {
        let deadline = std::time::Instant::now() + timeout;
        let mut outputs: Vec<Option<Output>> = (0..self.children.len()).map(|_| None).collect();

        while outputs.iter().any(Option::is_none) {
            for (index, child) in self.children.iter_mut().enumerate() {
                let Some(running) = child.as_mut() else {
                    continue;
                };
                if running
                    .try_wait()
                    .expect("poll bounded CLI child")
                    .is_some()
                {
                    let finished = child.take().expect("finished child still owned");
                    outputs[index] = Some(
                        finished
                            .wait_with_output()
                            .expect("collect bounded CLI child"),
                    );
                }
            }

            if outputs.iter().all(Option::is_some) {
                break;
            }
            if std::time::Instant::now() >= deadline {
                let pending = self
                    .children
                    .iter()
                    .enumerate()
                    .filter_map(|(index, child)| child.as_ref().map(|_| &self.args[index]))
                    .collect::<Vec<_>>();
                panic!("bounded CLI batch exceeded {timeout:?}; still running: {pending:?}");
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        outputs
            .into_iter()
            .map(|output| output.expect("every batch child produced output"))
            .collect()
    }
}

impl Drop for CliBatch {
    fn drop(&mut self) {
        for child in &mut self.children {
            let Some(mut child) = child.take() else {
                continue;
            };
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}

/// The five CLI routes a query-time runaway can arrive through. Each one asks
/// for JSON, so its failure is the error envelope and not the human line.
fn query_time_cli_cases(
    score: &std::path::Path,
    render: &std::path::Path,
    export: &std::path::Path,
) -> Vec<Vec<String>> {
    vec![
        vec![
            "query".into(),
            "--json".into(),
            "-e".into(),
            QUERY_TIME_RUNAWAY.into(),
        ],
        vec![
            "trace".into(),
            "--json".into(),
            "-e".into(),
            QUERY_TIME_RUNAWAY.into(),
            "--duration".into(),
            "0.1".into(),
        ],
        vec![
            "render".into(),
            "--json".into(),
            "-e".into(),
            QUERY_TIME_RUNAWAY.into(),
            "--duration".into(),
            "0.1".into(),
            "-o".into(),
            render.to_string_lossy().into_owned(),
            "--format".into(),
            "onset-json".into(),
        ],
        vec![
            "bench".into(),
            "--json".into(),
            "-e".into(),
            QUERY_TIME_RUNAWAY.into(),
            "--iterations".into(),
            "1".into(),
        ],
        vec![
            "export".into(),
            score.to_string_lossy().into_owned(),
            "--json".into(),
            "-o".into(),
            export.to_string_lossy().into_owned(),
            "--duration".into(),
            "0.1".into(),
        ],
    ]
}

#[test]
fn top_level_help_follows_the_central_product_name() {
    let output = rustel().arg("--help").output().expect("read command help");
    assert!(output.status.success(), "--help failed: {output:?}");
    let help = String::from_utf8(output.stdout).expect("help is UTF-8");
    // clap prints the invoked binary name, which ends in `.exe` on Windows.
    // Accept both spellings of the product name and nothing else.
    let name = rustel_runtime::product::COMMAND_NAME;
    let expected = format!("Usage: {name} ");
    let expected_exe = format!("Usage: {name}.exe ");
    assert!(
        help.contains(&expected) || help.contains(&expected_exe),
        "help does not use the central command name {expected:?}:\n{help}"
    );
}

#[test]
fn doctor_emits_the_versioned_capability_schema() {
    let out = ok_stdout(&["doctor", "--json"]);
    let report: rustel_runtime::CapabilityReportV1 = serde_json::from_str(&out)
        .unwrap_or_else(|error| panic!("doctor stdout is not capability JSON: {error}\n{out}"));

    assert_eq!(
        report.schema_version,
        rustel_runtime::CAPABILITY_REPORT_SCHEMA_VERSION
    );
    assert_eq!(report.build.name, rustel_runtime::product::NAME);
    assert_eq!(report.runtime.query_workers, 1);
    let portable = report
        .capabilities
        .iter()
        .find(|status| status.id == "portable_scalar_dsp")
        .expect("portable scalar DSP capability");
    assert!(portable.compiled);
    assert!(portable.detected);
    assert!(portable.selected);
    assert!(
        report
            .capabilities
            .iter()
            .filter(|status| status.selected)
            .all(|status| status.compiled && status.detected)
    );

    if matches!(report.cpu.architecture.as_str(), "x86" | "x86_64") {
        let detected = report
            .cpu
            .features
            .iter()
            .find(|feature| feature.id == "avx2")
            .expect("x86 AVX2 feature")
            .detected;
        for id in ["avx2_convolution", "avx2_supersaw", "avx2_wavetable"] {
            let avx2 = report
                .capabilities
                .iter()
                .find(|status| status.id == id)
                .unwrap_or_else(|| panic!("x86 build contains {id}"));
            assert!(avx2.compiled);
            assert_eq!(avx2.detected, detected);
            assert_eq!(avx2.selected, detected);
        }
    } else {
        for id in ["avx2_convolution", "avx2_supersaw", "avx2_wavetable"] {
            assert!(report.capabilities.iter().all(|status| status.id != id));
        }
    }
}

#[test]
fn doctor_names_the_cargo_features_the_binary_was_built_with() {
    let out = ok_stdout(&["doctor", "--json"]);
    let report: rustel_runtime::CapabilityReportV1 = serde_json::from_str(&out)
        .unwrap_or_else(|error| panic!("doctor stdout is not capability JSON: {error}\n{out}"));
    let features = report.build.features;

    assert!(features.is_sorted(), "{features:?}");
    assert!(
        !features.iter().any(|name| name == "default"),
        "{features:?}"
    );
    for (feature, built) in [
        ("extensions", cfg!(feature = "extensions")),
        ("gamepad", cfg!(feature = "gamepad")),
        ("mp3-export", cfg!(feature = "mp3-export")),
        ("studio", cfg!(feature = "studio")),
    ] {
        assert_eq!(
            features.iter().any(|name| name == feature),
            built,
            "`{feature}` in {features:?}"
        );
    }
}

// -- query ------------------------------------------------------------------

#[test]
fn clear_score_cache_keeps_host_trusted_cache_files() {
    let base = scratch("clear-score-cache");
    let score = base.join("score");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&score).expect("score cache");
    std::fs::write(score.join("entry.wav"), b"score-selected").expect("score cache entry");
    std::fs::write(base.join("trusted.wav"), b"host-trusted").expect("trusted cache entry");

    let output = rustel()
        .args(["clear-score-cache", "--force"])
        .env(rustel_runtime::product::SAMPLE_CACHE_ENV, &base)
        .output()
        .expect("clear-score-cache");
    assert!(
        output.status.success(),
        "clear-score-cache failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("clear result JSON");
    assert_eq!(report["score_sample_cache"]["status"], "cleared");
    assert!(!score.exists(), "score-selected namespace survived clear");
    assert_eq!(
        std::fs::read(base.join("trusted.wav")).expect("trusted entry remains"),
        b"host-trusted"
    );
    assert!(
        base.join(".score-cache-no-legacy").is_file(),
        "clear did not disable legacy migration"
    );
    let _ = std::fs::remove_dir_all(&base);
}

// -- samples -----------------------------------------------------------------

#[test]
fn samples_cache_list_reports_pack_names_without_a_network_fetch_of_files() {
    // --list fetches only the pinned manifests; the file count each row
    // carries proves the pin parsed. No pack row means the pin and the
    // source list drifted apart and the command has nothing to offer.
    let out = ok_stdout(&["samples", "cache", "--list", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("--list stdout is not JSON: {e}\n{out}"));
    let packs = json["packs"].as_array().expect("packs array");
    assert!(!packs.is_empty(), "the pin names no packs: {out}");
    assert!(
        json["sample_cache"]
            .as_str()
            .is_some_and(|path| !path.is_empty()),
        "--list does not say where the cache is: {out}"
    );
    for pack in packs {
        assert!(
            pack["pack"].as_str().is_some_and(|name| !name.is_empty()),
            "a pack row has no name: {pack}"
        );
        assert!(
            pack["files"].as_u64().is_some_and(|files| files > 0),
            "a pack row holds no files: {pack}"
        );
    }
}

#[test]
fn samples_cache_refuses_an_unknown_pack_by_listing_the_packs_there_are() {
    // Output is a diagnosed error, not usage noise: the one thing a person
    // with a mistyped name needs is the list of names that WOULD have
    // matched, in the same words --list shows them.
    let output = run(&["samples", "cache", "no-such-pack"]);
    assert!(!output.status.success(), "an unknown pack must be refused");
    let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
    assert!(
        stderr.contains("no pack matches"),
        "the refusal does not say the name was unknown: {stderr}"
    );
    assert!(
        stderr.contains("piano"),
        "the refusal does not name the packs there are: {stderr}"
    );
}

#[test]
fn samples_clear_reports_the_removed_bytes_as_json() {
    let base = scratch("samples-clear");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("cache dir");
    // The cache's own naming: a 64-hex digest with an extension (the
    // writer takes the first sixteen bytes of a sha256). A file a person
    // left beside it keeps its name and its place.
    std::fs::write(
        base.join("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.wav"),
        b"cached",
    )
    .expect("cache entry");
    std::fs::write(base.join("mine.txt"), b"kept").expect("foreign file");

    let output = rustel()
        .args(["samples", "clear", "--json", "--force"])
        .env(rustel_runtime::product::SAMPLE_CACHE_ENV, &base)
        .output()
        .expect("samples clear");
    assert!(
        output.status.success(),
        "samples clear failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("clear result JSON");
    assert_eq!(report["sample_cache"]["status"], "cleared");
    assert_eq!(
        report["sample_cache"]["bytes"], 6,
        "the removed bytes were not counted"
    );
    assert!(
        !base
            .join("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.wav")
            .exists()
    );
    assert_eq!(
        std::fs::read(base.join("mine.txt")).expect("foreign file kept"),
        b"kept"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn samples_clear_plain_text_says_what_happened() {
    let base = scratch("samples-clear-text");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("cache dir");

    let output = rustel()
        .args(["samples", "clear", "--force"])
        .env(rustel_runtime::product::SAMPLE_CACHE_ENV, &base)
        .output()
        .expect("samples clear");
    assert!(output.status.success(), "samples clear failed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("sample cache cleared"),
        "the text report does not say the cache was cleared: {stdout}"
    );
    assert!(
        !stdout.starts_with('{'),
        "without --json the report is text, not the JSON envelope: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn samples_help_documents_the_group() {
    let out = ok_stdout(&["samples", "--help"]);
    for word in ["cache", "clear"] {
        assert!(
            out.contains(word),
            "`samples --help` does not mention {word}:\n{out}"
        );
    }
}

#[test]
fn query_emits_parseable_json_for_the_documented_shape() {
    let out = ok_stdout(&["query", "--json", "-e", r#"s("bd sd")"#]);
    let json: serde_json::Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("query stdout is not JSON: {e}\n{out}"));

    // Schema, not just "some JSON": these are the fields a consumer reads.
    assert_eq!(json["begin"], "0/1");
    assert_eq!(json["end"], "1/1");
    let haps = json["haps"].as_array().expect("haps array");
    assert_eq!(haps.len(), 2, "{out}");
    for hap in haps {
        assert!(hap["show"].is_string(), "hap has no `show`: {hap}");
        assert!(!hap["value"].is_null(), "hap has no `value`: {hap}");
    }
    assert_eq!(haps[0]["show"], "[ 0/1 → 1/2 | s:bd ]");
    // `s` is a CONTROL, so the value is an object rather than a bare string.
    assert_eq!(haps[0]["value"]["s"], "bd");
    // Nothing threw, so the field stays out of the documented shape.
    assert!(json.get("query_threw").is_none(), "{out}");
}

/// A thrown query prints its report and returns a failed exit status.
#[test]
fn a_query_whose_pattern_threw_fails_the_exit_status() {
    let source = r#"note("c4 e4").fmap(x => { throw new Error("boom-fmap") })"#;
    let out = run(&[
        "query", "--json", "-e", source, "--begin", "0", "--end", "1",
    ]);
    let json: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("query stdout is not JSON: {e}"));
    assert_eq!(json["haps"].as_array().map(Vec::len), Some(0), "{json}");
    assert!(
        json["query_threw"]
            .as_str()
            .is_some_and(|message| message.contains("boom-fmap")),
        "{json}"
    );
    assert_reported_failure(
        &[
            "query", "--json", "-e", source, "--begin", "0", "--end", "1",
        ],
        "boom-fmap",
    );
    // A `$:` lane is a stack child: the stack contains the throw, and the
    // exit status still reports it.
    let lane = format!("$: {source}");
    assert_reported_failure(
        &["query", "--json", "-e", &lane, "--begin", "0", "--end", "1"],
        "boom-fmap",
    );
    assert_reported_failure(&["trace", "-e", source], "boom-fmap");
}

#[test]
fn overflowing_step_combinations_refuse_through_cli_query() {
    for (source, operation) in [
        (r#"stack(s("bd@1e38 sd"), s("cp@17 hh"))"#, "stack"),
        (
            "pure('a').setSteps(1e38).polyBind(() => pure('b').setSteps(0.5))",
            "polyJoin",
        ),
    ] {
        let result = run(&["query", "--json", "-e", source]);
        assert_eq!(
            result.status.code(),
            Some(3),
            "{source}: expected a resource refusal: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(result.stdout.is_empty(), "{source}: refusal emitted stdout");
        let error: serde_json::Value =
            serde_json::from_slice(&result.stderr).expect("JSON resource refusal");
        assert_eq!(error["error"]["kind"], "resource-limit", "{source}");
        let message = error["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains(operation) && message.contains("fraction arithmetic"),
            "{source}: wrong fraction refusal: {error}"
        );
    }
}

#[test]
fn query_score_text_has_no_implicit_module_or_sample_io_access() {
    // The default strudel.cc-parity policy refuses loopback URLs and names the
    // grant that allows them. A one-shot query must fail as well: no later
    // live update can report the missing bank.
    for source in [
        "samples('http://127.0.0.1:9/strudel.json'); s('bd')",
        "samples('http://127.0.0.1:9/file/../../password.txt'); s('bd')",
        "samples('http://127.0.0.1:9/file/%2e%2e/%2e%2e/password.txt'); s('bd')",
    ] {
        let denied = run(&["query", "--json", "-e", source]);
        assert!(!denied.status.success(), "denied query exited 0");
        assert!(
            String::from_utf8_lossy(&denied.stderr)
                .contains("outside the permitted sample origins"),
            "the denied capability was not reported for {source:?}: {}",
            String::from_utf8_lossy(&denied.stderr)
        );
    }

    // --strict-sample-origins removes the parity default entirely: with no
    // grants at all, registration is refused before any URL is considered.
    let strict = run(&[
        "query",
        "--json",
        "-e",
        "samples('https://samples.example/strudel.json'); s('bd')",
        "--strict-sample-origins",
    ]);
    assert!(!strict.status.success(), "strict denial exited 0");
    assert!(
        String::from_utf8_lossy(&strict.stderr).contains("without a host grant"),
        "strict mode must fall back to the inert policy: {}",
        String::from_utf8_lossy(&strict.stderr)
    );

    assert_reported_failure(
        &["query", "--json", "-e", "import('file:///tmp/module.js')"],
        "dynamic import is disabled",
    );
    assert_reported_failure(
        &[
            "query",
            "--json",
            "-e",
            "s('bd')",
            "--allow-sample-origin",
            "file:///tmp",
        ],
        "must be an http or https origin",
    );
}

#[test]
#[cfg(feature = "osc")]
fn query_refuses_an_osc_host_grant_that_is_not_an_ip() {
    assert_reported_failure(
        &[
            "query",
            "--json",
            "-e",
            "s('bd')",
            "--allow-osc-host",
            "evil.example",
        ],
        "not an IP address",
    );
    assert_reported_failure(
        &[
            "query",
            "--json",
            "-e",
            "s('bd')",
            "--allow-osc-host",
            "0.0.0.0",
        ],
        "not a unicast destination",
    );
    let allowed = run(&[
        "query",
        "--json",
        "-e",
        "s('bd')",
        "--allow-osc-host",
        "10.0.0.5",
    ]);
    assert!(
        allowed.status.success(),
        "a unicast grant was refused: {}",
        String::from_utf8_lossy(&allowed.stderr)
    );
}

#[test]
fn query_honours_the_requested_span() {
    let out = ok_stdout(&[
        "query",
        "--json",
        "-e",
        r#"s("bd")"#,
        "--begin",
        "1",
        "--end",
        "3",
    ]);
    let json: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(json["begin"], "1/1");
    assert_eq!(json["end"], "3/1");
    assert_eq!(json["haps"].as_array().expect("haps").len(), 2);
}

#[test]
fn stepwise_query_and_play_keep_exact_window_sensitive_output() {
    const SOURCE: &str = "sequence(0, 1).replicate(slowcat(1, 2))";
    let query = |begin: &str, end: &str| {
        let out = ok_stdout(&[
            "query", "--json", "-e", SOURCE, "--begin", begin, "--end", end,
        ]);
        serde_json::from_str::<serde_json::Value>(&out).expect("stepwise query JSON")
    };
    let shows = |json: &serde_json::Value| {
        json["haps"]
            .as_array()
            .expect("haps")
            .iter()
            .map(|hap| hap["show"].as_str().expect("hap show").to_owned())
            .collect::<Vec<_>>()
    };

    let joined = query("0", "2");
    assert_eq!(joined["begin"], "0/1");
    assert_eq!(joined["end"], "2/1");
    assert_eq!(
        shows(&joined),
        [
            "[ 0/1 → 1/2 | 0 ]",
            "[ 1/2 → 1/1 | 1 ]",
            "[ 1/1 → 3/2 | 0 ]",
            "[ 3/2 → 2/1 | 1 ]",
        ],
        "CLI query stopped using sorted StepJoin output for its whole window"
    );

    let second_cycle = query("1", "2");
    assert_eq!(
        shows(&second_cycle),
        [
            "[ 1/1 → 5/4 | 0 ]",
            "[ 5/4 → 3/2 | 1 ]",
            "[ 3/2 → 7/4 | 0 ]",
            "[ 7/4 → 2/1 | 1 ]",
        ],
        "CLI query did not select factor two when the window begins at cycle one"
    );

    let play = ok_stdout(&["trace", "--json", "-e", SOURCE, "--duration", "4"]);
    let play: serde_json::Value = serde_json::from_str(&play).expect("stepwise play JSON");
    let onsets = play["onsets"]
        .as_array()
        .expect("onsets")
        .iter()
        .map(|onset| {
            (
                onset["whole_begin"].as_str().expect("whole begin"),
                onset["value_show"].as_str().expect("value show"),
                onset["target_time"].as_f64().expect("target time"),
                onset["duration_secs"].as_f64().expect("duration"),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        onsets,
        [
            ("0/1", "0", 0.0, 1.0),
            ("1/2", "1", 1.0, 1.0),
            ("1/1", "0", 2.0, 0.5),
            ("5/4", "1", 2.5, 0.5),
            ("3/2", "0", 3.0, 0.5),
            ("7/4", "1", 3.5, 0.5),
            ("2/1", "0", 4.0, 0.5),
        ],
        "CLI play changed the scheduler's per-window patterned factors"
    );

    let contract = ok_stdout(&[
        "query",
        "--json",
        "-e",
        "stepcat(sequence(0, 1, 2, 3).contract(2), 9)",
    ]);
    let contract: serde_json::Value = serde_json::from_str(&contract).expect("contract query JSON");
    assert_eq!(
        shows(&contract),
        [
            "[ 0/1 → 1/6 | 0 ]",
            "[ 1/6 → 1/3 | 1 ]",
            "[ 1/3 → 1/2 | 2 ]",
            "[ 1/2 → 2/3 | 3 ]",
            "[ 2/3 → 1/1 | 9 ]",
        ],
        "CLI lost contract's divided steps at a real stepcat consumer"
    );
}

#[test]
fn scalar_contract_zero_is_a_safe_explicit_residual_not_a_process_panic() {
    let args = [
        "query",
        "--json",
        "-e",
        "sequence(0, 1).contract(0)",
        "--begin",
        "0",
        "--end",
        "2",
    ];
    let out = run(&args);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "native safe-zero policy stopped being a normal query: {:?}\n{stderr}",
        out.status.code()
    );
    assert!(
        !stderr.to_lowercase().contains("panicked at"),
        "contract(0) unwound the CLI process: {stderr}"
    );
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("safe contract(0) query JSON");
    assert!(
        json["haps"].as_array().expect("haps").is_empty(),
        "defined-step contract(0) must stay safely query-silent: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    // strudel.cc throws `Error: Division by Zero` eagerly. Until the
    // native evaluator can preserve that construction-phase exception, this
    // successful empty result is an intentional divergence. The invariant
    // here is narrower and product-critical: user input must never reach
    // Fraction::div(0), unwind with exit 101, or abort by signal.
}

#[test]
fn canonical_take_drop_scalar_forms_emit_exact_cli_haps() {
    const TAKE_TWO: &[&str] = &["[ 0/1 → 1/2 | 0 ]", "[ 1/2 → 1/1 | 1 ]"];
    const TAKE_NEGATIVE_TWO: &[&str] = &["[ 0/1 → 1/2 | 3 ]", "[ 1/2 → 1/1 | 4 ]"];
    const TAKE_FRACTIONAL: &[&str] = &[
        "[ 0/1 → 2/5 | 0 ]",
        "[ 2/5 → 4/5 | 1 ]",
        "[ (4/5 → 1/1) ⇝ 6/5 | 2 ]",
    ];
    const FULL: &[&str] = &[
        "[ 0/1 → 1/5 | 0 ]",
        "[ 1/5 → 2/5 | 1 ]",
        "[ 2/5 → 3/5 | 2 ]",
        "[ 3/5 → 4/5 | 3 ]",
        "[ 4/5 → 1/1 | 4 ]",
    ];
    const DROP_TWO: &[&str] = &[
        "[ 0/1 → 1/3 | 2 ]",
        "[ 1/3 → 2/3 | 3 ]",
        "[ 2/3 → 1/1 | 4 ]",
    ];
    const DROP_NEGATIVE_TWO: &[&str] = &[
        "[ 0/1 → 1/3 | 0 ]",
        "[ 1/3 → 2/3 | 1 ]",
        "[ 2/3 → 1/1 | 2 ]",
    ];
    const DROP_FRACTIONAL: &[&str] = &[
        "[ -1/5 ⇜ (0/1 → 1/5) | 2 ]",
        "[ 1/5 → 3/5 | 3 ]",
        "[ 3/5 → 1/1 | 4 ]",
    ];

    let cases: &[(&str, &[&str])] = &[
        ("sequence(0, 1, 2, 3, 4).take(2)", TAKE_TWO),
        ("take(2, sequence(0, 1, 2, 3, 4))", TAKE_TWO),
        ("take(2)(sequence(0, 1, 2, 3, 4))", TAKE_TWO),
        ("sequence(0, 1, 2, 3, 4).take(0)", &[]),
        ("sequence(0, 1, 2, 3, 4).take(-2)", TAKE_NEGATIVE_TWO),
        ("sequence(0, 1, 2, 3, 4).take(2.5)", TAKE_FRACTIONAL),
        ("sequence(0, 1, 2, 3, 4).take(6)", FULL),
        ("new Pattern(state => pure('x').query(state)).take(2)", &[]),
        ("sequence(0, 1, 2, 3, 4).drop(2)", DROP_TWO),
        ("drop(2, sequence(0, 1, 2, 3, 4))", DROP_TWO),
        ("drop(2)(sequence(0, 1, 2, 3, 4))", DROP_TWO),
        ("sequence(0, 1, 2, 3, 4).drop(0)", FULL),
        ("sequence(0, 1, 2, 3, 4).drop(-2)", DROP_NEGATIVE_TWO),
        ("sequence(0, 1, 2, 3, 4).drop(2.5)", DROP_FRACTIONAL),
        ("sequence(0, 1, 2, 3, 4).drop(6)", &["[ 0/1 → 1/1 | 0 ]"]),
        ("sequence(0, 1, 2, 3, 4).drop(-6)", &["[ 0/1 → 1/1 | 4 ]"]),
        ("sequence(0, 1, 2, 3, 4).drop(10)", FULL),
        ("new Pattern(state => pure('x').query(state)).drop(2)", &[]),
    ];

    for &(source, expected) in cases {
        let out = ok_stdout(&["query", "--json", "-e", source]);
        let json: serde_json::Value =
            serde_json::from_str(&out).unwrap_or_else(|error| panic!("{source}: {error}"));
        let got = json["haps"]
            .as_array()
            .expect("haps")
            .iter()
            .map(|hap| hap["show"].as_str().expect("hap show").to_owned())
            .collect::<Vec<_>>();
        let expected = expected
            .iter()
            .map(|show| (*show).to_owned())
            .collect::<Vec<_>>();
        assert_eq!(got, expected, "{source}: wrong canonical CLI result");
    }

    let play_view = |source: &str| {
        let out = ok_stdout(&["trace", "--json", "-e", source, "--duration", "2"]);
        let json: serde_json::Value = serde_json::from_str(&out).expect("scalar play JSON");
        json["onsets"]
            .as_array()
            .expect("scalar play onsets")
            .iter()
            .map(|onset| {
                (
                    onset["whole_begin"]
                        .as_str()
                        .expect("whole begin")
                        .to_owned(),
                    onset["value_show"].as_str().expect("value show").to_owned(),
                )
            })
            .collect::<Vec<_>>()
    };
    for (operation, forms) in [
        (
            "shrink",
            [
                "sequence(0, 1, 2, 3).shrink(1)",
                "shrink(1, sequence(0, 1, 2, 3))",
                "shrink(1)(sequence(0, 1, 2, 3))",
            ],
        ),
        (
            "grow",
            [
                "sequence(0, 1, 2, 3).grow(1)",
                "grow(1, sequence(0, 1, 2, 3))",
                "grow(1)(sequence(0, 1, 2, 3))",
            ],
        ),
    ] {
        let method = play_view(forms[0]);
        assert!(!method.is_empty(), "{operation} method scheduled no onsets");
        assert_eq!(
            play_view(forms[1]),
            method,
            "{operation} direct free form diverged in CLI play"
        );
        assert_eq!(
            play_view(forms[2]),
            method,
            "{operation} fully-curried free form diverged in CLI play"
        );
    }
}

#[test]
fn raw_take_drop_reach_cli_query_play_without_stepwise_expansion_charge() {
    const TAKE: &str = r#"(() => {
      if (Object.hasOwn(globalThis, '_take')
          || Object.hasOwn(rustelScope, '_take')
          || typeof globalThis._take !== 'undefined'
          || typeof rustelScope._take !== 'undefined') {
        throw new Error('raw take leaked outside Pattern.prototype');
      }
      return sequence('a','b','c','d','e')._take(-2);
    })()"#;
    const DROP: &str = r#"(() => {
      if (Object.hasOwn(globalThis, '_drop')
          || Object.hasOwn(rustelScope, '_drop')
          || typeof globalThis._drop !== 'undefined'
          || typeof rustelScope._drop !== 'undefined') {
        throw new Error('raw drop leaked outside Pattern.prototype');
      }
      return sequence('a','b','c','d','e')._drop(2);
    })()"#;
    const TAKE_HAPS: &[&str] = &["[ 0/1 → 1/2 | d ]", "[ 1/2 → 1/1 | e ]"];
    const DROP_HAPS: &[&str] = &[
        "[ 0/1 → 1/3 | c ]",
        "[ 1/3 → 2/3 | d ]",
        "[ 2/3 → 1/1 | e ]",
    ];

    for (source, expected_haps, expected_onsets) in [
        (
            TAKE,
            TAKE_HAPS,
            vec![("0/1", "d"), ("1/2", "e"), ("1/1", "d")],
        ),
        (
            DROP,
            DROP_HAPS,
            vec![("0/1", "c"), ("1/3", "d"), ("2/3", "e"), ("1/1", "c")],
        ),
    ] {
        let query = ok_stdout(&["query", "--json", "-e", source]);
        let query: serde_json::Value =
            serde_json::from_str(&query).expect("raw take/drop query JSON");
        assert_eq!(
            query["haps"]
                .as_array()
                .expect("raw take/drop haps")
                .iter()
                .map(|hap| hap["show"].as_str().expect("hap show"))
                .collect::<Vec<_>>(),
            expected_haps,
            "{source}: CLI raw take/drop query timing changed"
        );

        let play = ok_stdout(&["trace", "--json", "-e", source, "--duration", "2"]);
        let play: serde_json::Value = serde_json::from_str(&play).expect("raw take/drop play JSON");
        assert_eq!(
            play["onsets"]
                .as_array()
                .expect("raw take/drop onsets")
                .iter()
                .map(|onset| (
                    onset["whole_begin"].as_str().expect("whole begin"),
                    onset["value_show"].as_str().expect("value show"),
                ))
                .collect::<Vec<_>>(),
            expected_onsets,
            "{source}: CLI raw take/drop scheduler onsets changed"
        );
    }

    // Declared steps above the shared expansion threshold remain O(1) for the
    // raw zoom/take pair.  This protects the product boundary against a false
    // reuse of shrink/grow's materialisation charge.
    let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
    for name in ["_take", "_drop"] {
        let source = format!("pure('x').setSteps({over}).{name}(1)");
        let result = run(&["query", "--json", "-e", &source]);
        assert!(
            result.status.success(),
            "{name}: pool-neutral raw query was refused: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let json: serde_json::Value =
            serde_json::from_slice(&result.stdout).expect("pool-neutral raw query JSON");
        assert_eq!(
            json["haps"].as_array().expect("pool-neutral haps").len(),
            1,
            "{name}: O(1) raw query changed"
        );
    }
}

#[test]
fn raw_extend_replicate_reach_cli_query_play_without_stepwise_expansion_charge() {
    const EXTEND_HAPS: &[&str] = &[
        "[ 0/1 → 1/4 | a ]",
        "[ 1/4 → 1/2 | b ]",
        "[ 1/2 → 3/4 | c ]",
        "[ 3/4 → 1/1 | d ]",
    ];
    const REPLICATE_HAPS: &[&str] = &[
        "[ 0/1 → 1/4 | a ]",
        "[ 1/4 → 1/2 | b ]",
        "[ 1/2 → 3/4 | a ]",
        "[ 3/4 → 1/1 | b ]",
    ];

    for (name, expected_haps, expected_onsets) in [
        (
            "_extend",
            EXTEND_HAPS,
            vec![
                ("0/1", "a"),
                ("1/4", "b"),
                ("1/2", "c"),
                ("3/4", "d"),
                ("1/1", "a"),
            ],
        ),
        (
            "_replicate",
            REPLICATE_HAPS,
            vec![
                ("0/1", "a"),
                ("1/4", "b"),
                ("1/2", "a"),
                ("3/4", "b"),
                ("1/1", "c"),
            ],
        ),
    ] {
        let source = format!(
            r#"(() => {{
              if (Object.hasOwn(globalThis, '{name}')
                  || Object.hasOwn(rustelScope, '{name}')
                  || typeof globalThis.{name} !== 'undefined'
                  || typeof rustelScope.{name} !== 'undefined') {{
                throw new Error('raw stepwise chain leaked outside Pattern.prototype');
              }}
              return slowcat(sequence('a','b'),sequence('c','d')).{name}(2);
            }})()"#
        );
        let query = ok_stdout(&["query", "--json", "-e", &source]);
        let query: serde_json::Value =
            serde_json::from_str(&query).expect("raw extend/replicate query JSON");
        assert_eq!(
            query["haps"]
                .as_array()
                .expect("raw extend/replicate haps")
                .iter()
                .map(|hap| hap["show"].as_str().expect("hap show"))
                .collect::<Vec<_>>(),
            expected_haps,
            "{name}: CLI query timing changed"
        );

        let play = ok_stdout(&["trace", "--json", "-e", &source, "--duration", "2"]);
        let play: serde_json::Value =
            serde_json::from_str(&play).expect("raw extend/replicate play JSON");
        assert_eq!(
            play["onsets"]
                .as_array()
                .expect("raw extend/replicate onsets")
                .iter()
                .map(|onset| (
                    onset["whole_begin"].as_str().expect("whole begin"),
                    onset["value_show"].as_str().expect("value show"),
                ))
                .collect::<Vec<_>>(),
            expected_onsets,
            "{name}: CLI scheduler onsets changed"
        );
    }

    // Raw extend/replicate compose graph transforms and do not materialise a
    // vector from declared step metadata. Crossing the shared threshold with
    // factor one therefore remains a successful one-hap query, with no new
    // resource operation exposed by the CLI.
    let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
    for name in ["_extend", "_replicate"] {
        let source = format!("pure('x').setSteps({over}).{name}(1)");
        let result = run(&["query", "--json", "-e", &source]);
        assert!(
            result.status.success(),
            "{name}: pool-neutral raw chain was refused: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let json: serde_json::Value =
            serde_json::from_slice(&result.stdout).expect("pool-neutral raw chain query JSON");
        assert_eq!(
            json["haps"].as_array().expect("pool-neutral haps").len(),
            1,
            "{name}: O(1) raw chain query changed"
        );
    }
}

#[test]
fn raw_expand_contract_and_with_steps_reach_cli_query_play_without_charge() {
    const EXPANDED_HAPS: &[&str] = &[
        "[ 0/1 → 2/5 | a ]",
        "[ 2/5 → 4/5 | b ]",
        "[ 4/5 → 1/1 | z ]",
    ];
    const CONTRACTED_HAPS: &[&str] = &[
        "[ 0/1 → 1/4 | a ]",
        "[ 1/4 → 1/2 | b ]",
        "[ 1/2 → 1/1 | z ]",
    ];

    for (name, transform, expected_haps, expected_onsets) in [
        (
            "withSteps",
            "sequence('a','b').withSteps(steps => steps.mul(2))",
            EXPANDED_HAPS,
            vec![("0/1", "a"), ("2/5", "b"), ("4/5", "z"), ("1/1", "a")],
        ),
        (
            "_expand",
            "sequence('a','b')._expand(2)",
            EXPANDED_HAPS,
            vec![("0/1", "a"), ("2/5", "b"), ("4/5", "z"), ("1/1", "a")],
        ),
        (
            "_contract",
            "sequence('a','b')._contract(2)",
            CONTRACTED_HAPS,
            vec![("0/1", "a"), ("1/4", "b"), ("1/2", "z"), ("1/1", "a")],
        ),
    ] {
        let source = format!(
            r#"(() => {{
              if (Object.hasOwn(globalThis, '{name}')
                  || Object.hasOwn(rustelScope, '{name}')
                  || typeof globalThis.{name} !== 'undefined'
                  || typeof rustelScope.{name} !== 'undefined') {{
                throw new Error('prototype-only metadata helper leaked');
              }}
              return stepcat({transform}, 'z');
            }})()"#
        );
        let query = ok_stdout(&["query", "--json", "-e", &source]);
        let query: serde_json::Value =
            serde_json::from_str(&query).expect("raw metadata query JSON");
        assert_eq!(
            query["haps"]
                .as_array()
                .expect("raw metadata haps")
                .iter()
                .map(|hap| hap["show"].as_str().expect("hap show"))
                .collect::<Vec<_>>(),
            expected_haps,
            "{name}: CLI query timing changed"
        );

        let play = ok_stdout(&["trace", "--json", "-e", &source, "--duration", "2"]);
        let play: serde_json::Value = serde_json::from_str(&play).expect("raw metadata play JSON");
        assert_eq!(
            play["onsets"]
                .as_array()
                .expect("raw metadata onsets")
                .iter()
                .map(|onset| (
                    onset["whole_begin"].as_str().expect("whole begin"),
                    onset["value_show"].as_str().expect("value show"),
                ))
                .collect::<Vec<_>>(),
            expected_onsets,
            "{name}: CLI scheduler timing changed"
        );
    }

    // Metadata replacement is O(1): none of these paths consumes the shared
    // stepwise materialisation pool or exposes a new resource operation.
    let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
    for (name, source) in [
        (
            "withSteps",
            format!("pure('x').setSteps({over}).withSteps(s => s.mul(2))"),
        ),
        ("_expand", format!("pure('x').setSteps({over})._expand(2)")),
        (
            "_contract",
            format!("pure('x').setSteps({over})._contract(2)"),
        ),
    ] {
        let result = run(&["query", "--json", "-e", &source]);
        assert!(
            result.status.success(),
            "{name}: pool-neutral CLI query was refused: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let json: serde_json::Value =
            serde_json::from_slice(&result.stdout).expect("pool-neutral metadata query JSON");
        assert_eq!(
            json["haps"].as_array().expect("pool-neutral haps").len(),
            1,
            "{name}: metadata-only CLI query changed"
        );
    }
}

#[test]
fn raw_range_pair_reaches_cli_query_play_without_resource_charge() {
    const HAPS: &[&str] = &[
        "[ 0/1 → 1/4 | 10 ]",
        "[ 1/4 → 1/2 | 15 ]",
        "[ 1/2 → 3/4 | 20 ]",
        "[ 3/4 → 1/1 | z ]",
    ];
    const ONSETS: &[(&str, &str)] = &[
        ("0/1", "10"),
        ("1/4", "15"),
        ("1/2", "20"),
        ("3/4", "z"),
        ("1/1", "10"),
    ];

    for (name, transform) in [
        ("_range", "sequence(0,.5,1)._range(10,20)"),
        ("_range2", "sequence(-1,0,1)._range2(10,20)"),
    ] {
        let source = format!(
            r#"(() => {{
              if (Object.hasOwn(globalThis, '{name}')
                  || Object.hasOwn(rustelScope, '{name}')
                  || typeof globalThis.{name} !== 'undefined'
                  || typeof rustelScope.{name} !== 'undefined'
                  || Object.hasOwn(Pattern.prototype, '_rangex')
                  || typeof Pattern.prototype._rangex !== 'undefined') {{
                throw new Error('bounded raw range surface leaked');
              }}
              return stepcat({transform}, 'z');
            }})()"#
        );
        let query = ok_stdout(&["query", "--json", "-e", &source]);
        let query: serde_json::Value = serde_json::from_str(&query).expect("raw range query JSON");
        assert_eq!(
            query["haps"]
                .as_array()
                .expect("raw range haps")
                .iter()
                .map(|hap| hap["show"].as_str().expect("hap show"))
                .collect::<Vec<_>>(),
            HAPS,
            "{name}: CLI raw range query changed"
        );

        let play = ok_stdout(&["trace", "--json", "-e", &source, "--duration", "2"]);
        let play: serde_json::Value = serde_json::from_str(&play).expect("raw range play JSON");
        assert_eq!(
            play["onsets"]
                .as_array()
                .expect("raw range onsets")
                .iter()
                .map(|onset| (
                    onset["whole_begin"].as_str().expect("whole begin"),
                    onset["value_show"].as_str().expect("value show"),
                ))
                .collect::<Vec<_>>(),
            ONSETS,
            "{name}: CLI raw range scheduler onsets changed"
        );
    }

    // The pair composes a fixed number of graph nodes. Declared metadata above
    // the shared stepwise limit remains successful and does not expose a new
    // resource operation through product diagnostics.
    let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
    for (name, source) in [
        ("_range", format!("pure(.5).setSteps({over})._range(10,20)")),
        (
            "_range2",
            format!("pure(0).setSteps({over})._range2(10,20)"),
        ),
    ] {
        let result = run(&["query", "--json", "-e", &source]);
        assert!(
            result.status.success(),
            "{name}: pool-neutral CLI query was refused: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let json: serde_json::Value =
            serde_json::from_slice(&result.stdout).expect("pool-neutral raw range query JSON");
        assert_eq!(
            json["haps"].as_array().expect("pool-neutral haps").len(),
            1,
            "{name}: O(1) raw range query changed"
        );
    }
}

#[test]
fn raw_apply_reaches_cli_query_play_and_preserves_nested_resource_limits() {
    const HAPS: &[&str] = &[
        "[ 0/1 → 1/3 | a ]",
        "[ 1/3 → 2/3 | b ]",
        "[ 2/3 → 1/1 | z ]",
    ];
    const ONSETS: &[(&str, &str)] = &[("0/1", "a"), ("1/3", "b"), ("2/3", "z"), ("1/1", "a")];
    let source = r#"(() => {
      if (Object.hasOwn(globalThis, '_apply')
          || Object.hasOwn(rustelScope, '_apply')
          || typeof globalThis._apply !== 'undefined'
          || typeof rustelScope._apply !== 'undefined'
          || Object.hasOwn(Pattern.prototype, '_swingBy')
          || typeof Pattern.prototype._swingBy !== 'undefined') {
        throw new Error('bounded raw apply surface leaked');
      }
      return stepcat(
        pure('ignored')._apply(value => value, sequence('a', 'b')),
        'z'
      );
    })()"#;

    let query = ok_stdout(&["query", "--json", "-e", source]);
    let query: serde_json::Value = serde_json::from_str(&query).expect("raw apply query JSON");
    assert_eq!(
        query["haps"]
            .as_array()
            .expect("raw apply haps")
            .iter()
            .map(|hap| hap["show"].as_str().expect("hap show"))
            .collect::<Vec<_>>(),
        HAPS,
        "CLI raw apply query changed"
    );

    let play = ok_stdout(&["trace", "--json", "-e", source, "--duration", "2"]);
    let play: serde_json::Value = serde_json::from_str(&play).expect("raw apply play JSON");
    assert_eq!(
        play["onsets"]
            .as_array()
            .expect("raw apply onsets")
            .iter()
            .map(|onset| (
                onset["whole_begin"].as_str().expect("whole begin"),
                onset["value_show"].as_str().expect("value show"),
            ))
            .collect::<Vec<_>>(),
        ONSETS,
        "CLI raw apply scheduler onsets changed"
    );

    // The raw wrapper itself adds no resource operation. Large declared
    // metadata therefore remains one successful hap when the callback merely
    // selects the same terminal.
    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let over = limit + 1;
    let neutral = format!("pure('x').setSteps({over})._apply(value => value)");
    let neutral = ok_stdout(&["query", "--json", "-e", &neutral]);
    let neutral: serde_json::Value =
        serde_json::from_str(&neutral).expect("pool-neutral raw apply query JSON");
    assert_eq!(
        neutral["haps"]
            .as_array()
            .expect("pool-neutral raw apply haps")
            .len(),
        1,
        "raw apply invented a resource refusal"
    );

    // A bounded operation chosen inside the callback retains its own typed
    // product refusal; `_apply` neither removes nor renames that accounting.
    let nested = format!("gap({over})._apply(value => value.shrink(0))");
    let nested = run(&["query", "--json", "-e", &nested]);
    assert_eq!(
        nested.status.code(),
        Some(3),
        "callback-selected shrink was not a typed refusal: {}",
        String::from_utf8_lossy(&nested.stderr)
    );
    assert!(
        nested.stdout.is_empty(),
        "callback-selected refused operation emitted stdout"
    );
    let error: serde_json::Value =
        serde_json::from_slice(&nested.stderr).expect("nested resource-limit envelope");
    assert_eq!(error["error"]["kind"], "resource-limit");
    let message = error["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("shrink/grow")
            && message.contains(&over.to_string())
            && message.contains(&limit.to_string()),
        "wrong callback-selected resource refusal: {error}"
    );
}

#[test]
fn raw_when_reaches_cli_branches_and_preserves_nested_resource_limits() {
    const HAPS: &[&str] = &[
        "[ 0/1 → 1/3 | a ]",
        "[ 1/3 → 2/3 | b ]",
        "[ 2/3 → 1/1 | z ]",
    ];
    const ONSETS: &[(&str, &str)] = &[("0/1", "a"), ("1/3", "b"), ("2/3", "z"), ("1/1", "a")];
    let source = r#"(() => {
      if (Object.hasOwn(globalThis, '_when')
          || Object.hasOwn(rustelScope, '_when')
          || typeof globalThis._when !== 'undefined'
          || typeof rustelScope._when !== 'undefined'
          || typeof Pattern.prototype._when !== 'function'
          || Object.hasOwn(Pattern.prototype, '_swingBy')
          || typeof Pattern.prototype._swingBy !== 'undefined') {
        throw new Error('bounded raw when surface leaked');
      }
      let falseHits = 0;
      let trueHits = 0;
      const falseSelected = pure('outer')._when(
        NaN,
        () => {
          falseHits++;
          throw new Error('false callback ran');
        },
        sequence('a', 'b')
      );
      const guardedTruthy = new Proxy({}, {
        get() { throw new Error('ToBoolean read its object'); }
      });
      const trueSelected = pure('outer')._when(
        guardedTruthy,
        value => {
          trueHits++;
          return value;
        },
        pure('z')
      );
      if (falseHits !== 0 || trueHits !== 1) {
        throw new Error('raw when callback phase changed');
      }
      return stepcat(falseSelected, trueSelected);
    })()"#;

    let query = ok_stdout(&["query", "--json", "-e", source]);
    let query: serde_json::Value = serde_json::from_str(&query).expect("raw when query JSON");
    assert_eq!(
        query["haps"]
            .as_array()
            .expect("raw when haps")
            .iter()
            .map(|hap| hap["show"].as_str().expect("hap show"))
            .collect::<Vec<_>>(),
        HAPS,
        "CLI raw when query changed"
    );

    let play = ok_stdout(&["trace", "--json", "-e", source, "--duration", "2"]);
    let play: serde_json::Value = serde_json::from_str(&play).expect("raw when play JSON");
    assert_eq!(
        play["onsets"]
            .as_array()
            .expect("raw when onsets")
            .iter()
            .map(|onset| (
                onset["whole_begin"].as_str().expect("whole begin"),
                onset["value_show"].as_str().expect("value show"),
            ))
            .collect::<Vec<_>>(),
        ONSETS,
        "CLI raw when scheduler onsets changed"
    );

    // False suppresses the callback body, and the raw wrapper adds no resource
    // operation of its own. The selected large-metadata terminal stays a
    // successful one-hap product.
    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let over = limit + 1;
    let neutral = format!(
        r#"(() => {{
          let hits = 0;
          const result = gap({over})._when(
            false,
            value => {{ hits++; return value.shrink(0); }},
            pure('safe').setSteps({over})
          );
          if (hits !== 0) throw new Error('false branch executed nested work');
          return result;
        }})()"#
    );
    let neutral = ok_stdout(&["query", "--json", "-e", &neutral]);
    let neutral: serde_json::Value =
        serde_json::from_str(&neutral).expect("pool-neutral raw when query JSON");
    assert_eq!(
        neutral["haps"]
            .as_array()
            .expect("pool-neutral raw when haps")
            .len(),
        1,
        "raw when invented a resource refusal"
    );

    // True-branch callback-selected work retains the selected operation's
    // typed product refusal. This is not an arbitrary-callback resource-free
    // or general-bound claim.
    let nested = format!("gap({over})._when(true, value => value.shrink(0))");
    let nested = run(&["query", "--json", "-e", &nested]);
    assert_eq!(
        nested.status.code(),
        Some(3),
        "true-branch shrink was not a typed refusal: {}",
        String::from_utf8_lossy(&nested.stderr)
    );
    assert!(
        nested.stdout.is_empty(),
        "true-branch refused operation emitted stdout"
    );
    let error: serde_json::Value =
        serde_json::from_slice(&nested.stderr).expect("nested resource-limit envelope");
    assert_eq!(error["error"]["kind"], "resource-limit");
    let message = error["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("shrink/grow")
            && message.contains(&over.to_string())
            && message.contains(&limit.to_string()),
        "wrong true-branch resource refusal: {error}"
    );
}

#[test]
fn raw_never_always_reach_cli_and_preserve_selected_resource_limits() {
    const HAPS: &[&str] = &[
        "[ 0/1 → 1/3 | a ]",
        "[ 1/3 → 2/3 | b ]",
        "[ 2/3 → 1/1 | z ]",
    ];
    const ONSETS: &[(&str, &str)] = &[("0/1", "a"), ("1/3", "b"), ("2/3", "z"), ("1/1", "a")];
    let source = r#"(() => {
      const names = ['never', '_never', 'always', '_always'];
      const order = Object.getOwnPropertyNames(Pattern.prototype)
        .filter(name => names.includes(name));
      if (order.join(',') !== names.join(',')
          || Object.hasOwn(globalThis, '_never')
          || Object.hasOwn(globalThis, '_always')
          || Object.hasOwn(rustelScope, '_never')
          || Object.hasOwn(rustelScope, '_always')
          || typeof globalThis._never !== 'undefined'
          || typeof globalThis._always !== 'undefined'
          || typeof rustelScope._never !== 'undefined'
          || typeof rustelScope._always !== 'undefined') {
        throw new Error('bounded raw never/always surface changed');
      }
      let ignoredTouches = 0;
      const ignored = new Proxy(function () {}, {
        apply() { ignoredTouches++; throw new Error('never callback ran'); },
        get() { ignoredTouches++; throw new Error('never callback read'); },
      });
      let alwaysHits = 0;
      const neverSelected = pure('outer')._never(
        ignored, sequence('a', 'b'), { marker: 'ignored extra' }
      );
      const alwaysSelected = pure('outer')._always(
        value => { alwaysHits++; return value; },
        pure('z'),
        { marker: 'ignored extra' }
      );
      if (ignoredTouches !== 0 || alwaysHits !== 1) {
        throw new Error('raw never/always callback phase changed');
      }
      return stepcat(neverSelected, alwaysSelected);
    })()"#;

    let query = ok_stdout(&["query", "--json", "-e", source]);
    let query: serde_json::Value =
        serde_json::from_str(&query).expect("raw never/always query JSON");
    assert_eq!(
        query["haps"]
            .as_array()
            .expect("raw never/always haps")
            .iter()
            .map(|hap| hap["show"].as_str().expect("hap show"))
            .collect::<Vec<_>>(),
        HAPS,
        "CLI raw never/always query changed"
    );

    let play = ok_stdout(&["trace", "--json", "-e", source, "--duration", "2"]);
    let play: serde_json::Value = serde_json::from_str(&play).expect("raw never/always play JSON");
    assert_eq!(
        play["onsets"]
            .as_array()
            .expect("raw never/always onsets")
            .iter()
            .map(|onset| (
                onset["whole_begin"].as_str().expect("whole begin"),
                onset["value_show"].as_str().expect("value show"),
            ))
            .collect::<Vec<_>>(),
        ONSETS,
        "CLI raw never/always scheduler onsets changed"
    );

    // The wrappers add no resource operation of their own. Large metadata on
    // an otherwise bounded selected terminal remains a successful one-hap
    // product for both selection routes.
    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let over = limit + 1;
    for (name, source) in [
        (
            "_never",
            format!("pure('outer')._never(null, pure('safe').setSteps({over}))"),
        ),
        (
            "_always",
            format!("pure('safe').setSteps({over})._always(value => value)"),
        ),
    ] {
        let neutral = ok_stdout(&["query", "--json", "-e", &source]);
        let neutral: serde_json::Value = serde_json::from_str(&neutral)
            .unwrap_or_else(|error| panic!("{name}: pool-neutral query JSON: {error}"));
        assert_eq!(
            neutral["haps"]
                .as_array()
                .expect("pool-neutral raw never/always haps")
                .len(),
            1,
            "{name} invented a resource refusal"
        );
    }

    // Caller-evaluated work selected by `_never`, and callback-selected work
    // returned by `_always`, retain the chosen operation's typed product
    // refusal. This is not an arbitrary-callback resource-free claim.
    for (name, source) in [
        (
            "_never",
            format!("pure('outer')._never(null, gap({over}).shrink(0))"),
        ),
        (
            "_always",
            format!("gap({over})._always(value => value.shrink(0))"),
        ),
    ] {
        let nested = run(&["query", "--json", "-e", &source]);
        assert_eq!(
            nested.status.code(),
            Some(3),
            "{name}: selected shrink was not a typed refusal: {}",
            String::from_utf8_lossy(&nested.stderr)
        );
        assert!(
            nested.stdout.is_empty(),
            "{name}: selected refused operation emitted stdout"
        );
        let error: serde_json::Value = serde_json::from_slice(&nested.stderr)
            .unwrap_or_else(|parse| panic!("{name}: resource envelope: {parse}"));
        assert_eq!(error["error"]["kind"], "resource-limit");
        let message = error["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("shrink/grow")
                && message.contains(&over.to_string())
                && message.contains(&limit.to_string()),
            "{name}: wrong selected resource refusal: {error}"
        );
    }
}

#[test]
fn raw_swing_reaches_cli_query_play_and_preserves_existing_resource_attribution() {
    const HAPS: &[&str] = &[
        "[ (0/1 → 1/8) ⇝ 1/4 | a ]",
        "[ 1/24 ⇜ (1/8 → 1/4) ⇝ 7/24 | a ]",
        "[ (1/4 → 3/8) ⇝ 1/2 | b ]",
        "[ 7/24 ⇜ (3/8 → 1/2) ⇝ 13/24 | b ]",
        "[ (1/2 → 5/8) ⇝ 3/4 | c ]",
        "[ 13/24 ⇜ (5/8 → 3/4) ⇝ 19/24 | c ]",
        "[ (3/4 → 7/8) ⇝ 1/1 | d ]",
        "[ 19/24 ⇜ (7/8 → 1/1) ⇝ 25/24 | d ]",
    ];
    const ONSETS: &[(&str, &str)] = &[
        ("0/1", "a"),
        ("1/4", "b"),
        ("1/2", "c"),
        ("3/4", "d"),
        ("1/1", "a"),
    ];
    let source = r#"(() => {
      const names = ['swing', '_swing'];
      const order = Object.getOwnPropertyNames(Pattern.prototype)
        .filter(name => names.includes(name));
      if (order.join(',') !== names.join(',')
          || Object.hasOwn(globalThis, '_swing')
          || Object.hasOwn(rustelScope, '_swing')
          || typeof globalThis._swing !== 'undefined'
          || typeof rustelScope._swing !== 'undefined'
          || Object.hasOwn(Pattern.prototype, '_swingBy')
          || typeof Pattern.prototype._swingBy !== 'undefined') {
        throw new Error('bounded raw swing surface changed');
      }
      return sequence('a','b','c','d').setSteps(7)._swing(4);
    })()"#;

    let query = ok_stdout(&["query", "--json", "-e", source]);
    let query: serde_json::Value = serde_json::from_str(&query).expect("raw swing query JSON");
    assert_eq!(
        query["haps"]
            .as_array()
            .expect("raw swing haps")
            .iter()
            .map(|hap| hap["show"].as_str().expect("hap show"))
            .collect::<Vec<_>>(),
        HAPS,
        "CLI raw swing query changed"
    );

    let play = ok_stdout(&["trace", "--json", "-e", source, "--duration", "2"]);
    let play: serde_json::Value = serde_json::from_str(&play).expect("raw swing play JSON");
    assert_eq!(
        play["onsets"]
            .as_array()
            .expect("raw swing onsets")
            .iter()
            .map(|onset| (
                onset["whole_begin"].as_str().expect("whole begin"),
                onset["value_show"].as_str().expect("value show"),
            ))
            .collect::<Vec<_>>(),
        ONSETS,
        "CLI raw swing scheduler onsets changed"
    );

    let zero = ok_stdout(&[
        "query",
        "--json",
        "-e",
        "sequence('a','b').setSteps(7)._swing(0)",
    ]);
    let zero: serde_json::Value = serde_json::from_str(&zero).expect("zero raw swing query JSON");
    assert!(
        zero["haps"]
            .as_array()
            .expect("zero raw swing haps")
            .is_empty(),
        "zero raw swing produced CLI haps"
    );

    // Oversized declared metadata alone does not create a `_swing` resource
    // operation. The fixed scalar graph remains successful.
    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let over = limit + 1;
    let neutral = format!("pure('safe').setSteps({over})._swing(4)");
    let neutral = ok_stdout(&["query", "--json", "-e", &neutral]);
    let neutral: serde_json::Value =
        serde_json::from_str(&neutral).expect("pool-neutral raw swing query JSON");
    assert_eq!(
        neutral["haps"]
            .as_array()
            .expect("pool-neutral raw swing haps")
            .len(),
        8,
        "raw swing's fixed scalar graph changed"
    );

    // Nested bounded work remains attributed to that operation after the
    // swing graph wraps it. This is not a general density bound.
    let nested = format!("gap({over}).shrink(0)._swing(4)");
    let nested = run(&["query", "--json", "-e", &nested]);
    assert_eq!(
        nested.status.code(),
        Some(3),
        "nested shrink was not a typed refusal: {}",
        String::from_utf8_lossy(&nested.stderr)
    );
    assert!(
        nested.stdout.is_empty(),
        "nested refused raw swing operation emitted stdout"
    );
    let error: serde_json::Value =
        serde_json::from_slice(&nested.stderr).expect("raw swing resource-limit envelope");
    assert_eq!(error["error"]["kind"], "resource-limit");
    let message = error["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("shrink/grow")
            && message.contains(&over.to_string())
            && message.contains(&limit.to_string()),
        "wrong nested raw swing refusal: {error}"
    );
}

#[test]
fn raw_signal_quartet_reaches_cli_query_play_and_preserves_nested_attribution() {
    const SURFACE: &str = r#"
      const projected = [
        'often', '_often', 'rarely', '_rarely',
        'almostNever', '_almostNever',
        'almostAlways', '_almostAlways'
      ];
      const order = Object.getOwnPropertyNames(Pattern.prototype)
        .filter(name => projected.includes(name));
      if (order.join(',') !== projected.join(',')) {
        throw new Error('raw signal quartet order changed');
      }
      for (const raw of projected.filter(name => name.startsWith('_'))) {
        if (Object.hasOwn(globalThis, raw)
            || Object.hasOwn(rustelScope, raw)
            || typeof globalThis[raw] !== 'undefined'
            || typeof rustelScope[raw] !== 'undefined') {
          throw new Error(`raw signal destination leaked: ${raw}`);
        }
      }
    "#;

    for (raw_name, query_changed, play_changed) in [
        ("_often", [true, true], [true, true, false]),
        ("_rarely", [true, false], [true, false, false]),
        ("_almostNever", [true, false], [true, false, false]),
        ("_almostAlways", [true, true], [true, true, false]),
    ] {
        let source = format!(
            r#"(() => {{
                 {SURFACE}
                 return pure(1).setSteps(7).{raw_name}(
                   value => value.add(10)
                 );
               }})()"#
        );
        let query = ok_stdout(&[
            "query", "--json", "-e", &source, "--begin", "0", "--end", "2",
        ]);
        let query: serde_json::Value = serde_json::from_str(&query)
            .unwrap_or_else(|error| panic!("{raw_name}: CLI query JSON: {error}"));
        assert_eq!(
            query["haps"]
                .as_array()
                .expect("raw signal haps")
                .iter()
                .map(|hap| hap["show"].as_str().expect("hap show").to_owned())
                .collect::<Vec<_>>(),
            query_changed
                .into_iter()
                .enumerate()
                .map(|(cycle, changed)| {
                    let value = if changed { 11 } else { 1 };
                    format!("[ {cycle}/1 → {}/1 | {value} ]", cycle + 1)
                })
                .collect::<Vec<_>>(),
            "{raw_name}: CLI scalar RNG query changed"
        );

        let play = ok_stdout(&["trace", "--json", "-e", &source, "--duration", "4"]);
        let play: serde_json::Value = serde_json::from_str(&play)
            .unwrap_or_else(|error| panic!("{raw_name}: CLI play JSON: {error}"));
        assert_eq!(
            play["onsets"]
                .as_array()
                .expect("raw signal onsets")
                .iter()
                .map(|onset| (
                    onset["whole_begin"]
                        .as_str()
                        .expect("whole begin")
                        .to_owned(),
                    onset["value_show"].as_str().expect("value show").to_owned(),
                ))
                .collect::<Vec<_>>(),
            play_changed
                .into_iter()
                .enumerate()
                .map(|(cycle, changed)| {
                    (
                        format!("{cycle}/1"),
                        if changed { "11" } else { "1" }.to_owned(),
                    )
                })
                .collect::<Vec<_>>(),
            "{raw_name}: CLI scheduler RNG onsets changed"
        );
    }

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let over = limit + 1;
    for raw_name in ["_often", "_rarely", "_almostNever", "_almostAlways"] {
        // A tagged native transformer keeps this path host-free; the raw
        // shorthand itself adds no shared-pool operation or charge.
        let neutral = format!("pure('safe').setSteps({over}).{raw_name}(rev)");
        let neutral = ok_stdout(&["query", "--json", "-e", &neutral]);
        let neutral: serde_json::Value = serde_json::from_str(&neutral)
            .unwrap_or_else(|error| panic!("{raw_name}: neutral query JSON: {error}"));
        assert_eq!(
            neutral["haps"]
                .as_array()
                .expect("pool-neutral raw signal haps")
                .len(),
            1,
            "{raw_name}: invented a raw-specific resource refusal"
        );

        // Ordinary callback-selected work keeps the selected operation's
        // typed accounting. This is not a general callback resource bound.
        let nested = format!("gap({over}).{raw_name}(value => value.shrink(0))");
        let nested = run(&["query", "--json", "-e", &nested]);
        assert_eq!(
            nested.status.code(),
            Some(3),
            "{raw_name}: nested shrink was not a typed refusal: {}",
            String::from_utf8_lossy(&nested.stderr)
        );
        assert!(
            nested.stdout.is_empty(),
            "{raw_name}: refused nested operation emitted stdout"
        );
        let error: serde_json::Value = serde_json::from_slice(&nested.stderr)
            .unwrap_or_else(|parse| panic!("{raw_name}: resource envelope: {parse}"));
        assert_eq!(error["error"]["kind"], "resource-limit");
        let message = error["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("shrink/grow")
                && message.contains(&over.to_string())
                && message.contains(&limit.to_string()),
            "{raw_name}: wrong nested resource refusal: {error}"
        );
    }
}

#[test]
fn raw_set_reaches_cli_query_play_and_preserves_source_resource_attribution() {
    const SOURCE: &str = r#"
      (() => {
        const order = Object.getOwnPropertyNames(Pattern.prototype)
          .filter(name => name === '_set' || name === 'set');
        const keys = Object.keys(Pattern.prototype)
          .filter(name => name === '_set' || name === 'set');
        const raw = Object.getOwnPropertyDescriptor(Pattern.prototype, '_set');
        const publicSet = Object.getOwnPropertyDescriptor(Pattern.prototype, 'set');
        if (order.join(',') !== '_set,set'
            || keys.join(',') !== '_set'
            || raw.writable !== true
            || raw.enumerable !== true
            || raw.configurable !== true
            || raw.value.name !== ''
            || raw.value.length !== 1
            || typeof publicSet.get !== 'function'
            || publicSet.enumerable !== false
            || publicSet.configurable !== true
            || Object.hasOwn(globalThis, '_set')
            || Object.hasOwn(rustelScope, '_set')
            || typeof globalThis._set !== 'undefined'
            || typeof rustelScope._set !== 'undefined') {
          throw new Error('raw set CLI surface changed');
        }
        return pure(1).setSteps(7)._set(9);
      })()
    "#;
    let query = ok_stdout(&[
        "query", "--json", "-e", SOURCE, "--begin", "0", "--end", "2",
    ]);
    let query: serde_json::Value = serde_json::from_str(&query).expect("raw set CLI query JSON");
    assert_eq!(
        query["haps"]
            .as_array()
            .expect("raw set query haps")
            .iter()
            .map(|hap| hap["show"].as_str().expect("hap show"))
            .collect::<Vec<_>>(),
        ["[ 0/1 → 1/1 | 9 ]", "[ 1/1 → 2/1 | 9 ]"],
        "CLI raw set query changed"
    );

    let play = ok_stdout(&["trace", "--json", "-e", SOURCE, "--duration", "4"]);
    let play: serde_json::Value = serde_json::from_str(&play).expect("raw set CLI play JSON");
    assert_eq!(
        play["onsets"]
            .as_array()
            .expect("raw set onsets")
            .iter()
            .map(|onset| (
                onset["whole_begin"].as_str().expect("whole begin"),
                onset["value_show"].as_str().expect("value show"),
            ))
            .collect::<Vec<_>>(),
        [("0/1", "9"), ("1/1", "9"), ("2/1", "9")],
        "CLI raw set scheduler onsets changed"
    );

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let over = limit + 1;
    // `_set` adds no shared-pool operation or charge. Oversized source
    // metadata remains a one-hap map; arbitrary custom fmap/resource behavior
    // is intentionally not generalized from this canonical route.
    let neutral = format!("pure('safe').setSteps({over})._set('changed')");
    let neutral = ok_stdout(&["query", "--json", "-e", &neutral]);
    let neutral: serde_json::Value =
        serde_json::from_str(&neutral).expect("pool-neutral raw set query JSON");
    assert_eq!(
        neutral["haps"]
            .as_array()
            .expect("pool-neutral raw set haps")
            .len(),
        1,
        "raw set invented a raw-specific resource refusal"
    );

    // Work already present in the source graph keeps its operation/type/limit
    // attribution through the one-to-one map.
    let nested = format!("gap({over}).shrink(0)._set('changed')");
    let nested = run(&["query", "--json", "-e", &nested]);
    assert_eq!(
        nested.status.code(),
        Some(3),
        "nested shrink was not a typed refusal: {}",
        String::from_utf8_lossy(&nested.stderr)
    );
    assert!(
        nested.stdout.is_empty(),
        "refused nested raw set operation emitted stdout"
    );
    let error: serde_json::Value =
        serde_json::from_slice(&nested.stderr).expect("raw set resource-limit envelope");
    assert_eq!(error["error"]["kind"], "resource-limit");
    let message = error["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("shrink/grow")
            && message.contains(&over.to_string())
            && message.contains(&limit.to_string()),
        "wrong nested raw set resource refusal: {error}"
    );
}

#[test]
fn raw_keep_reaches_cli_query_play_without_executing_ignored_graphs() {
    const SOURCE: &str = r#"
      (() => {
        const names = ['_set', 'set', '_keep', 'keep'];
        const order = Object.getOwnPropertyNames(Pattern.prototype)
          .filter(name => names.includes(name));
        const keys = Object.keys(Pattern.prototype)
          .filter(name => names.includes(name));
        const raw = Object.getOwnPropertyDescriptor(Pattern.prototype, '_keep');
        const publicKeep = Object.getOwnPropertyDescriptor(Pattern.prototype, 'keep');
        if (order.join(',') !== '_set,set,_keep,keep'
            || keys.join(',') !== '_set,_keep'
            || raw.writable !== true
            || raw.enumerable !== true
            || raw.configurable !== true
            || raw.value.name !== ''
            || raw.value.length !== 1
            || typeof publicKeep.get !== 'function'
            || publicKeep.enumerable !== false
            || publicKeep.configurable !== true
            || Object.hasOwn(globalThis, '_keep')
            || Object.hasOwn(rustelScope, '_keep')
            || typeof globalThis._keep !== 'undefined'
            || typeof rustelScope._keep !== 'undefined') {
          throw new Error('raw keep CLI surface changed');
        }
        const ignored = new Pattern(() => {
          throw new Error('raw keep queried its ignored graph');
        });
        return pure(1).setSteps(7)._keep(ignored);
      })()
    "#;
    let query = ok_stdout(&[
        "query", "--json", "-e", SOURCE, "--begin", "0", "--end", "2",
    ]);
    let query: serde_json::Value = serde_json::from_str(&query).expect("raw keep CLI query JSON");
    assert_eq!(
        query["haps"]
            .as_array()
            .expect("raw keep query haps")
            .iter()
            .map(|hap| hap["show"].as_str().expect("hap show"))
            .collect::<Vec<_>>(),
        ["[ 0/1 → 1/1 | 1 ]", "[ 1/1 → 2/1 | 1 ]"],
        "CLI raw keep query changed or executed its ignored graph"
    );

    let play = ok_stdout(&["trace", "--json", "-e", SOURCE, "--duration", "4"]);
    let play: serde_json::Value = serde_json::from_str(&play).expect("raw keep CLI play JSON");
    assert_eq!(
        play["onsets"]
            .as_array()
            .expect("raw keep onsets")
            .iter()
            .map(|onset| (
                onset["whole_begin"].as_str().expect("whole begin"),
                onset["value_show"].as_str().expect("value show"),
            ))
            .collect::<Vec<_>>(),
        [("0/1", "1"), ("1/1", "1"), ("2/1", "1")],
        "CLI raw keep scheduler onsets changed or executed its ignored graph"
    );

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let over = limit + 1;
    // The captured first argument remains owned, but `_keep` neither queries
    // it nor adds a shared-pool operation/charge. Arbitrary custom-fmap and
    // general ownership/resource bounds remain outside this product proof.
    let neutral = format!(
        "(() => {{ const ignored = new Pattern(() => {{ throw new Error('ignored'); }}); return pure('safe').setSteps({over})._keep(ignored); }})()"
    );
    let neutral = ok_stdout(&["query", "--json", "-e", &neutral]);
    let neutral: serde_json::Value =
        serde_json::from_str(&neutral).expect("pool-neutral raw keep query JSON");
    assert_eq!(
        neutral["haps"]
            .as_array()
            .expect("pool-neutral raw keep haps")
            .iter()
            .map(|hap| hap["value"].as_str().expect("raw keep value"))
            .collect::<Vec<_>>(),
        ["safe"],
        "raw keep invented a refusal or executed the ignored graph"
    );

    // Work already present in the source graph retains its operation/type/
    // limit attribution through the one-to-one identity map.
    let nested = format!("gap({over}).shrink(0)._keep('ignored')");
    let nested = run(&["query", "--json", "-e", &nested]);
    assert_eq!(
        nested.status.code(),
        Some(3),
        "nested shrink was not a typed refusal: {}",
        String::from_utf8_lossy(&nested.stderr)
    );
    assert!(
        nested.stdout.is_empty(),
        "refused nested raw keep operation emitted stdout"
    );
    let error: serde_json::Value =
        serde_json::from_slice(&nested.stderr).expect("raw keep resource-limit envelope");
    assert_eq!(error["error"]["kind"], "resource-limit");
    let message = error["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("shrink/grow")
            && message.contains(&over.to_string())
            && message.contains(&limit.to_string()),
        "wrong nested raw keep resource refusal: {error}"
    );
}

#[test]
fn raw_keepif_reaches_cli_query_play_and_preserves_source_resource_attribution() {
    const FALSE_SOURCE: &str = r#"
      (() => {
        const names = ['_set', 'set', '_keep', 'keep', '_keepif', 'keepif'];
        const order = Object.getOwnPropertyNames(Pattern.prototype)
          .filter(name => names.includes(name));
        const keys = Object.keys(Pattern.prototype)
          .filter(name => names.includes(name));
        const raw = Object.getOwnPropertyDescriptor(Pattern.prototype, '_keepif');
        const publicKeepif = Object.getOwnPropertyDescriptor(
          Pattern.prototype, 'keepif'
        );
        if (order.join(',') !== '_set,set,_keep,keep,_keepif,keepif'
            || keys.join(',') !== '_set,_keep,_keepif'
            || raw.writable !== true
            || raw.enumerable !== true
            || raw.configurable !== true
            || raw.value.name !== ''
            || raw.value.length !== 1
            || typeof publicKeepif.get !== 'function'
            || publicKeepif.enumerable !== false
            || publicKeepif.configurable !== true
            || Object.hasOwn(globalThis, '_keepif')
            || Object.hasOwn(rustelScope, '_keepif')
            || typeof globalThis._keepif !== 'undefined'
            || typeof rustelScope._keepif !== 'undefined') {
          throw new Error('raw keepif CLI surface changed');
        }
        return sequence(1, 2).setSteps(7)._keepif(false);
      })()
    "#;
    let query = ok_stdout(&[
        "query",
        "--json",
        "-e",
        FALSE_SOURCE,
        "--begin",
        "0",
        "--end",
        "2",
    ]);
    let query: serde_json::Value = serde_json::from_str(&query).expect("raw keepif CLI query JSON");
    let haps = query["haps"].as_array().expect("raw keepif query haps");
    assert_eq!(
        haps.iter()
            .map(|hap| hap["show"].as_str().expect("hap show"))
            .collect::<Vec<_>>(),
        [
            "[ 0/1 → 1/2 | undefined ]",
            "[ 1/2 → 1/1 | undefined ]",
            "[ 1/1 → 3/2 | undefined ]",
            "[ 3/2 → 2/1 | undefined ]",
        ],
        "CLI raw keepif false route removed or retimed haps"
    );
    assert!(
        haps.iter().all(|hap| hap["value"].is_null()),
        "CLI raw keepif false values did not serialize as null"
    );

    let play = ok_stdout(&["trace", "--json", "-e", FALSE_SOURCE, "--duration", "4"]);
    let play: serde_json::Value = serde_json::from_str(&play).expect("raw keepif CLI play JSON");
    assert_eq!(
        play["onsets"]
            .as_array()
            .expect("raw keepif onsets")
            .iter()
            .map(|onset| (
                onset["whole_begin"].as_str().expect("whole begin"),
                onset["value_show"].as_str().expect("value show"),
            ))
            .collect::<Vec<_>>(),
        [
            ("0/1", "undefined"),
            ("1/2", "undefined"),
            ("1/1", "undefined"),
            ("3/2", "undefined"),
            ("2/1", "undefined"),
        ],
        "CLI raw keepif scheduler false onsets changed"
    );

    // Pattern-valued controls are truthy and retained, but never queried.
    // Pattern-valued sources are a separate explicit bridge residual.
    const CONTROL_SOURCE: &str = r#"
      (() => {
        const condition = new Pattern(() => {
          throw new Error('raw keepif queried its Pattern control');
        });
        return sequence(1, 2)._keepif(condition);
      })()
    "#;
    let control = ok_stdout(&["query", "--json", "-e", CONTROL_SOURCE]);
    let control: serde_json::Value =
        serde_json::from_str(&control).expect("Pattern-control raw keepif JSON");
    assert_eq!(
        control["haps"]
            .as_array()
            .expect("Pattern-control raw keepif haps")
            .iter()
            .map(|hap| hap["value"].as_f64().expect("numeric value"))
            .collect::<Vec<_>>(),
        [1.0, 2.0],
        "CLI raw keepif queried or changed its truthy Pattern control"
    );

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let over = limit + 1;
    // `_keepif` adds no shared-pool operation or charge. Oversized source
    // metadata remains a one-hap map; no arbitrary custom-fmap/general bound
    // is inferred from this canonical product route.
    let neutral = format!("pure('safe').setSteps({over})._keepif(false)");
    let neutral = ok_stdout(&["query", "--json", "-e", &neutral]);
    let neutral: serde_json::Value =
        serde_json::from_str(&neutral).expect("pool-neutral raw keepif JSON");
    let neutral_haps = neutral["haps"]
        .as_array()
        .expect("pool-neutral raw keepif haps");
    assert_eq!(neutral_haps.len(), 1);
    assert!(neutral_haps[0]["value"].is_null());

    // Work already present in the source graph keeps its operation/type/limit
    // attribution through the truthy one-to-one map.
    let nested = format!("gap({over}).shrink(0)._keepif(true)");
    let nested = run(&["query", "--json", "-e", &nested]);
    assert_eq!(
        nested.status.code(),
        Some(3),
        "nested shrink was not a typed refusal: {}",
        String::from_utf8_lossy(&nested.stderr)
    );
    assert!(
        nested.stdout.is_empty(),
        "refused nested raw keepif operation emitted stdout"
    );
    let error: serde_json::Value =
        serde_json::from_slice(&nested.stderr).expect("raw keepif resource-limit envelope");
    assert_eq!(error["error"]["kind"], "resource-limit");
    let message = error["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("shrink/grow")
            && message.contains(&over.to_string())
            && message.contains(&limit.to_string()),
        "wrong nested raw keepif resource refusal: {error}"
    );
}

#[test]
fn raw_eqt_reaches_cli_query_play_and_preserves_source_resource_attribution() {
    const SOURCE: &str = r#"
      (() => {
        const names = ['eq', '_eqt', 'eqt', 'ne', 'net'];
        const order = Object.getOwnPropertyNames(Pattern.prototype)
          .filter(name => names.includes(name));
        const keys = Object.keys(Pattern.prototype)
          .filter(name => names.includes(name));
        const raw = Object.getOwnPropertyDescriptor(Pattern.prototype, '_eqt');
        const publicEqt = Object.getOwnPropertyDescriptor(Pattern.prototype, 'eqt');
        if (order.join(',') !== 'eq,_eqt,eqt,ne,net'
            || keys.join(',') !== '_eqt'
            || raw.writable !== true
            || raw.enumerable !== true
            || raw.configurable !== true
            || raw.value.name !== ''
            || raw.value.length !== 1
            || typeof publicEqt.get !== 'function'
            || publicEqt.enumerable !== false
            || publicEqt.configurable !== true
            || Object.hasOwn(globalThis, '_eqt')
            || Object.hasOwn(rustelScope, '_eqt')
            || typeof globalThis._eqt !== 'undefined'
            || typeof rustelScope._eqt !== 'undefined') {
          throw new Error('raw eqt CLI surface changed');
        }
        return sequence(1, 2).setSteps(7)._eqt(1);
      })()
    "#;
    let query = ok_stdout(&[
        "query", "--json", "-e", SOURCE, "--begin", "0", "--end", "2",
    ]);
    let query: serde_json::Value = serde_json::from_str(&query).expect("raw eqt CLI query JSON");
    let haps = query["haps"].as_array().expect("raw eqt query haps");
    assert_eq!(
        haps.iter()
            .map(|hap| hap["show"].as_str().expect("hap show"))
            .collect::<Vec<_>>(),
        [
            "[ 0/1 → 1/2 | true ]",
            "[ 1/2 → 1/1 | false ]",
            "[ 1/1 → 3/2 | true ]",
            "[ 3/2 → 2/1 | false ]",
        ],
        "CLI raw eqt boolean rhythm changed"
    );
    assert_eq!(
        haps.iter()
            .map(|hap| hap["value"].as_bool().expect("boolean raw eqt value"))
            .collect::<Vec<_>>(),
        [true, false, true, false],
        "CLI raw eqt values changed"
    );

    let play = ok_stdout(&["trace", "--json", "-e", SOURCE, "--duration", "4"]);
    let play: serde_json::Value = serde_json::from_str(&play).expect("raw eqt CLI play JSON");
    assert_eq!(
        play["onsets"]
            .as_array()
            .expect("raw eqt onsets")
            .iter()
            .map(|onset| (
                onset["whole_begin"].as_str().expect("whole begin"),
                onset["value_show"].as_str().expect("value show"),
            ))
            .collect::<Vec<_>>(),
        [
            ("0/1", "true"),
            ("1/2", "false"),
            ("1/1", "true"),
            ("3/2", "false"),
            ("2/1", "true"),
        ],
        "CLI raw eqt scheduler onsets changed"
    );

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let over = limit + 1;
    // `_eqt` adds no shared-pool operation or charge. Oversized declared
    // metadata remains a two-hap map; this does not assert a general bound on
    // replaced `fmap` implementations.
    let neutral = format!("sequence(1, 2).setSteps({over})._eqt(1)");
    let neutral = ok_stdout(&["query", "--json", "-e", &neutral]);
    let neutral: serde_json::Value =
        serde_json::from_str(&neutral).expect("pool-neutral raw eqt JSON");
    assert_eq!(
        neutral["haps"]
            .as_array()
            .expect("pool-neutral raw eqt haps")
            .iter()
            .map(|hap| hap["value"].as_bool().expect("raw eqt boolean"))
            .collect::<Vec<_>>(),
        [true, false],
        "raw eqt invented a refusal or changed canonical fmap output"
    );

    // Work already present in the source graph retains its operation/type/
    // limit attribution through the one-to-one strict-equality map.
    let nested = format!("gap({over}).shrink(0)._eqt(0)");
    let nested = run(&["query", "--json", "-e", &nested]);
    assert_eq!(
        nested.status.code(),
        Some(3),
        "nested shrink was not a typed refusal: {}",
        String::from_utf8_lossy(&nested.stderr)
    );
    assert!(
        nested.stdout.is_empty(),
        "refused nested raw eqt operation emitted stdout"
    );
    let error: serde_json::Value =
        serde_json::from_slice(&nested.stderr).expect("raw eqt resource-limit envelope");
    assert_eq!(error["error"]["kind"], "resource-limit");
    let message = error["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("shrink/grow")
            && message.contains(&over.to_string())
            && message.contains(&limit.to_string()),
        "wrong nested raw eqt resource refusal: {error}"
    );
}

#[test]
fn raw_net_reaches_cli_query_play_and_preserves_source_resource_attribution() {
    const SOURCE: &str = r#"
      (() => {
        const names = ['ne', '_net', 'net', 'and'];
        const order = Object.getOwnPropertyNames(Pattern.prototype)
          .filter(name => names.includes(name));
        const keys = Object.keys(Pattern.prototype)
          .filter(name => names.includes(name));
        const raw = Object.getOwnPropertyDescriptor(Pattern.prototype, '_net');
        const publicNet = Object.getOwnPropertyDescriptor(Pattern.prototype, 'net');
        if (order.join(',') !== 'ne,_net,net,and'
            || keys.join(',') !== '_net'
            || raw.writable !== true
            || raw.enumerable !== true
            || raw.configurable !== true
            || raw.value.name !== ''
            || raw.value.length !== 1
            || typeof publicNet.get !== 'function'
            || publicNet.enumerable !== false
            || publicNet.configurable !== true
            || Object.hasOwn(globalThis, '_net')
            || Object.hasOwn(rustelScope, '_net')
            || Object.hasOwn(globalThis, 'net')
            || Object.hasOwn(rustelScope, 'net')
            || Object.hasOwn(Pattern.prototype, '_eq')
            || Object.hasOwn(Pattern.prototype, '_ne')) {
          throw new Error('raw net CLI surface changed');
        }
        return sequence(1, 2).setSteps(7)._net(1);
      })()
    "#;
    let query = ok_stdout(&[
        "query", "--json", "-e", SOURCE, "--begin", "0", "--end", "2",
    ]);
    let query: serde_json::Value = serde_json::from_str(&query).expect("raw net CLI query JSON");
    let haps = query["haps"].as_array().expect("raw net query haps");
    assert_eq!(
        haps.iter()
            .map(|hap| hap["show"].as_str().expect("hap show"))
            .collect::<Vec<_>>(),
        [
            "[ 0/1 → 1/2 | false ]",
            "[ 1/2 → 1/1 | true ]",
            "[ 1/1 → 3/2 | false ]",
            "[ 3/2 → 2/1 | true ]",
        ],
        "CLI raw net boolean rhythm changed"
    );
    assert_eq!(
        haps.iter()
            .map(|hap| hap["value"].as_bool().expect("boolean raw net value"))
            .collect::<Vec<_>>(),
        [false, true, false, true],
        "CLI raw net values changed"
    );

    let play = ok_stdout(&["trace", "--json", "-e", SOURCE, "--duration", "4"]);
    let play: serde_json::Value = serde_json::from_str(&play).expect("raw net CLI play JSON");
    assert_eq!(
        play["onsets"]
            .as_array()
            .expect("raw net onsets")
            .iter()
            .map(|onset| (
                onset["whole_begin"].as_str().expect("whole begin"),
                onset["value_show"].as_str().expect("value show"),
            ))
            .collect::<Vec<_>>(),
        [
            ("0/1", "false"),
            ("1/2", "true"),
            ("1/1", "false"),
            ("3/2", "true"),
            ("2/1", "false"),
        ],
        "CLI raw net scheduler onsets changed"
    );

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let over = limit + 1;
    let neutral = format!("sequence(1, 2).setSteps({over})._net(1)");
    let neutral = ok_stdout(&["query", "--json", "-e", &neutral]);
    let neutral: serde_json::Value =
        serde_json::from_str(&neutral).expect("pool-neutral raw net JSON");
    assert_eq!(
        neutral["haps"]
            .as_array()
            .expect("pool-neutral raw net haps")
            .iter()
            .map(|hap| hap["value"].as_bool().expect("raw net boolean"))
            .collect::<Vec<_>>(),
        [false, true],
        "raw net invented a refusal or changed canonical fmap output"
    );

    let nested = format!("gap({over}).shrink(0)._net(0)");
    let nested = run(&["query", "--json", "-e", &nested]);
    assert_eq!(
        nested.status.code(),
        Some(3),
        "nested shrink was not a typed refusal: {}",
        String::from_utf8_lossy(&nested.stderr)
    );
    assert!(
        nested.stdout.is_empty(),
        "refused nested raw net operation emitted stdout"
    );
    let error: serde_json::Value =
        serde_json::from_slice(&nested.stderr).expect("raw net resource-limit envelope");
    assert_eq!(error["error"]["kind"], "resource-limit");
    let message = error["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("shrink/grow")
            && message.contains(&over.to_string())
            && message.contains(&limit.to_string()),
        "wrong nested raw net resource refusal: {error}"
    );
}

#[test]
fn raw_and_reaches_cli_query_play_and_preserves_source_resource_attribution() {
    const SOURCE: &str = r#"
      (() => {
        const names = ['net', '_and', 'and', '_or', 'or'];
        const order = Object.getOwnPropertyNames(Pattern.prototype)
          .filter(name => names.includes(name));
        const keys = Object.keys(Pattern.prototype)
          .filter(name => names.includes(name));
        const raw = Object.getOwnPropertyDescriptor(Pattern.prototype, '_and');
        const publicAnd = Object.getOwnPropertyDescriptor(Pattern.prototype, 'and');
        if (order.join(',') !== 'net,_and,and,_or,or'
            || keys.join(',') !== '_and,_or'
            || raw.writable !== true
            || raw.enumerable !== true
            || raw.configurable !== true
            || raw.value.name !== ''
            || raw.value.length !== 1
            || typeof publicAnd.get !== 'function'
            || publicAnd.enumerable !== false
            || publicAnd.configurable !== true
            || Object.hasOwn(globalThis, '_and')
            || Object.hasOwn(rustelScope, '_and')
            || Object.hasOwn(globalThis, 'and')
            || Object.hasOwn(rustelScope, 'and')
            || !Object.hasOwn(Pattern.prototype, '_or')) {
          throw new Error('raw and CLI surface changed');
        }
        return sequence(0, 1).setSteps(7)._and('right');
      })()
    "#;
    let query = ok_stdout(&[
        "query", "--json", "-e", SOURCE, "--begin", "0", "--end", "2",
    ]);
    let query: serde_json::Value = serde_json::from_str(&query).expect("raw and CLI query JSON");
    let haps = query["haps"].as_array().expect("raw and query haps");
    assert_eq!(
        haps.iter()
            .map(|hap| hap["show"].as_str().expect("hap show"))
            .collect::<Vec<_>>(),
        [
            "[ 0/1 → 1/2 | 0 ]",
            "[ 1/2 → 1/1 | right ]",
            "[ 1/1 → 3/2 | 0 ]",
            "[ 3/2 → 2/1 | right ]",
        ],
        "CLI raw and operand-selection rhythm changed"
    );
    assert_eq!(
        haps.iter()
            .map(|hap| hap["value"].to_string())
            .collect::<Vec<_>>(),
        ["0.0", "\"right\"", "0.0", "\"right\""],
        "CLI raw and values changed"
    );

    let play = ok_stdout(&["trace", "--json", "-e", SOURCE, "--duration", "4"]);
    let play: serde_json::Value = serde_json::from_str(&play).expect("raw and CLI play JSON");
    assert_eq!(
        play["onsets"]
            .as_array()
            .expect("raw and onsets")
            .iter()
            .map(|onset| (
                onset["whole_begin"].as_str().expect("whole begin"),
                onset["value_show"].as_str().expect("value show"),
            ))
            .collect::<Vec<_>>(),
        [
            ("0/1", "0"),
            ("1/2", "right"),
            ("1/1", "0"),
            ("3/2", "right"),
            ("2/1", "0"),
        ],
        "CLI raw and scheduler onsets changed"
    );

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let over = limit + 1;
    let neutral = format!("sequence(0, 1).setSteps({over})._and('right')");
    let neutral = ok_stdout(&["query", "--json", "-e", &neutral]);
    let neutral: serde_json::Value =
        serde_json::from_str(&neutral).expect("pool-neutral raw and JSON");
    assert_eq!(
        neutral["haps"]
            .as_array()
            .expect("pool-neutral raw and haps")
            .iter()
            .map(|hap| hap["value"].to_string())
            .collect::<Vec<_>>(),
        ["0.0", "\"right\""],
        "raw and invented a refusal or changed canonical fmap output"
    );

    let nested = format!("gap({over}).shrink(0)._and('right')");
    let nested = run(&["query", "--json", "-e", &nested]);
    assert_eq!(
        nested.status.code(),
        Some(3),
        "nested shrink was not a typed refusal: {}",
        String::from_utf8_lossy(&nested.stderr)
    );
    assert!(
        nested.stdout.is_empty(),
        "refused nested raw and operation emitted stdout"
    );
    let error: serde_json::Value =
        serde_json::from_slice(&nested.stderr).expect("raw and resource-limit envelope");
    assert_eq!(error["error"]["kind"], "resource-limit");
    let message = error["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("shrink/grow")
            && message.contains(&over.to_string())
            && message.contains(&limit.to_string()),
        "wrong nested raw and resource refusal: {error}"
    );
}

#[test]
fn raw_or_reaches_cli_query_play_and_preserves_source_resource_attribution() {
    const SOURCE: &str = r#"
      (() => {
        const names = ['and', '_or', 'or', '_func', 'func'];
        const order = Object.getOwnPropertyNames(Pattern.prototype)
          .filter(name => names.includes(name));
        const keys = Object.keys(Pattern.prototype)
          .filter(name => names.includes(name));
        const raw = Object.getOwnPropertyDescriptor(Pattern.prototype, '_or');
        const publicOr = Object.getOwnPropertyDescriptor(Pattern.prototype, 'or');
        if (order.join(',') !== 'and,_or,or'
            || keys.join(',') !== '_or'
            || raw.writable !== true
            || raw.enumerable !== true
            || raw.configurable !== true
            || raw.value.name !== ''
            || raw.value.length !== 1
            || typeof publicOr.get !== 'function'
            || publicOr.enumerable !== false
            || publicOr.configurable !== true
            || Object.hasOwn(globalThis, '_or')
            || Object.hasOwn(rustelScope, '_or')
            || Object.hasOwn(globalThis, 'or')
            || Object.hasOwn(rustelScope, 'or')
            || Object.hasOwn(Pattern.prototype, '_func')
            || Object.hasOwn(Pattern.prototype, 'func')) {
          throw new Error('raw or CLI surface changed');
        }
        return sequence(0, 1).setSteps(7)._or('right');
      })()
    "#;
    let query = ok_stdout(&[
        "query", "--json", "-e", SOURCE, "--begin", "0", "--end", "2",
    ]);
    let query: serde_json::Value = serde_json::from_str(&query).expect("raw or CLI query JSON");
    let haps = query["haps"].as_array().expect("raw or query haps");
    assert_eq!(
        haps.iter()
            .map(|hap| hap["show"].as_str().expect("hap show"))
            .collect::<Vec<_>>(),
        [
            "[ 0/1 → 1/2 | right ]",
            "[ 1/2 → 1/1 | 1 ]",
            "[ 1/1 → 3/2 | right ]",
            "[ 3/2 → 2/1 | 1 ]",
        ],
        "CLI raw or operand-selection rhythm changed"
    );
    assert_eq!(
        haps.iter()
            .map(|hap| hap["value"].to_string())
            .collect::<Vec<_>>(),
        ["\"right\"", "1.0", "\"right\"", "1.0"],
        "CLI raw or values changed"
    );

    let play = ok_stdout(&["trace", "--json", "-e", SOURCE, "--duration", "4"]);
    let play: serde_json::Value = serde_json::from_str(&play).expect("raw or CLI play JSON");
    assert_eq!(
        play["onsets"]
            .as_array()
            .expect("raw or onsets")
            .iter()
            .map(|onset| (
                onset["whole_begin"].as_str().expect("whole begin"),
                onset["value_show"].as_str().expect("value show"),
            ))
            .collect::<Vec<_>>(),
        [
            ("0/1", "right"),
            ("1/2", "1"),
            ("1/1", "right"),
            ("3/2", "1"),
            ("2/1", "right"),
        ],
        "CLI raw or scheduler onsets changed"
    );

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let over = limit + 1;
    let neutral = format!("sequence(0, 1).setSteps({over})._or('right')");
    let neutral = ok_stdout(&["query", "--json", "-e", &neutral]);
    let neutral: serde_json::Value =
        serde_json::from_str(&neutral).expect("pool-neutral raw or JSON");
    assert_eq!(
        neutral["haps"]
            .as_array()
            .expect("pool-neutral raw or haps")
            .iter()
            .map(|hap| hap["value"].to_string())
            .collect::<Vec<_>>(),
        ["\"right\"", "1.0"],
        "raw or invented a refusal or changed canonical fmap output"
    );

    let nested = format!("gap({over}).shrink(0)._or('right')");
    let nested = run(&["query", "--json", "-e", &nested]);
    assert_eq!(
        nested.status.code(),
        Some(3),
        "nested shrink was not a typed refusal: {}",
        String::from_utf8_lossy(&nested.stderr)
    );
    assert!(
        nested.stdout.is_empty(),
        "refused nested raw or operation emitted stdout"
    );
    let error: serde_json::Value =
        serde_json::from_slice(&nested.stderr).expect("raw or resource-limit envelope");
    assert_eq!(error["error"]["kind"], "resource-limit");
    let message = error["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("shrink/grow")
            && message.contains(&over.to_string())
            && message.contains(&limit.to_string()),
        "wrong nested raw or resource refusal: {error}"
    );
}

#[test]
fn canonical_take_drop_patterned_windows_reach_cli_query_and_play() {
    let query = |source: &str, begin: &str, end: &str| {
        let out = ok_stdout(&[
            "query", "--json", "-e", source, "--begin", begin, "--end", end,
        ]);
        let json: serde_json::Value =
            serde_json::from_str(&out).unwrap_or_else(|error| panic!("{source}: {error}"));
        json["haps"]
            .as_array()
            .expect("haps")
            .iter()
            .map(|hap| hap["show"].as_str().expect("hap show").to_owned())
            .collect::<Vec<_>>()
    };

    const TAKE: &str = "sequence(0, 1, 2).take(slowcat(1, 2))";
    assert_eq!(
        query(TAKE, "0", "2"),
        ["[ 0/1 → 1/1 | 0 ]", "[ 1/1 → 2/1 | 0 ]"]
    );
    assert_eq!(
        query(TAKE, "1", "2"),
        ["[ 1/1 → 3/2 | 0 ]", "[ 3/2 → 2/1 | 1 ]"]
    );
    let take_play = ok_stdout(&["trace", "--json", "-e", TAKE, "--duration", "4"]);
    let take_play: serde_json::Value = serde_json::from_str(&take_play).expect("take play JSON");
    let take_onsets = take_play["onsets"]
        .as_array()
        .expect("onsets")
        .iter()
        .map(|onset| {
            (
                onset["whole_begin"].as_str().expect("whole begin"),
                onset["value_show"].as_str().expect("value show"),
                onset["target_time"].as_f64().expect("target time"),
                onset["duration_secs"].as_f64().expect("duration"),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        take_onsets,
        [
            ("0/1", "0", 0.0, 2.0),
            ("1/1", "0", 2.0, 1.0),
            ("3/2", "1", 3.0, 1.0),
            ("2/1", "0", 4.0, 1.0),
        ],
        "CLI scheduler changed patterned take's lookahead windows"
    );

    const DROP: &str = "sequence(0, 1, 2).drop(slowcat(1, 2))";
    assert_eq!(
        query(DROP, "0", "2"),
        [
            "[ 0/1 → 1/2 | 1 ]",
            "[ 1/2 → 1/1 | 2 ]",
            "[ 1/1 → 3/2 | 1 ]",
            "[ 3/2 → 2/1 | 2 ]",
        ]
    );
    assert_eq!(query(DROP, "1", "2"), ["[ 1/1 → 2/1 | 2 ]"]);
    let drop_play = ok_stdout(&["trace", "--json", "-e", DROP, "--duration", "4"]);
    let drop_play: serde_json::Value = serde_json::from_str(&drop_play).expect("drop play JSON");
    let drop_onsets = drop_play["onsets"]
        .as_array()
        .expect("onsets")
        .iter()
        .map(|onset| {
            (
                onset["whole_begin"].as_str().expect("whole begin"),
                onset["value_show"].as_str().expect("value show"),
                onset["target_time"].as_f64().expect("target time"),
                onset["duration_secs"].as_f64().expect("duration"),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        drop_onsets,
        [
            ("0/1", "1", 0.0, 1.0),
            ("1/2", "2", 1.0, 1.0),
            ("1/1", "2", 2.0, 2.0),
            ("2/1", "2", 4.0, 2.0),
        ],
        "CLI scheduler changed patterned drop's lookahead windows"
    );
}

#[test]
fn canonical_shrink_grow_scalar_forms_emit_exact_cli_haps() {
    const SHRINK_ONE: &[&str] = &[
        "[ 0/1 → 1/10 | 0 ]",
        "[ 1/10 → 1/5 | 1 ]",
        "[ 1/5 → 3/10 | 2 ]",
        "[ 3/10 → 2/5 | 3 ]",
        "[ 2/5 → 1/2 | 1 ]",
        "[ 1/2 → 3/5 | 2 ]",
        "[ 3/5 → 7/10 | 3 ]",
        "[ 7/10 → 4/5 | 2 ]",
        "[ 4/5 → 9/10 | 3 ]",
        "[ 9/10 → 1/1 | 3 ]",
    ];
    const SHRINK_NEGATIVE: &[&str] = &[
        "[ 0/1 → 1/10 | 0 ]",
        "[ 1/10 → 1/5 | 1 ]",
        "[ 1/5 → 3/10 | 2 ]",
        "[ 3/10 → 2/5 | 3 ]",
        "[ 2/5 → 1/2 | 0 ]",
        "[ 1/2 → 3/5 | 1 ]",
        "[ 3/5 → 7/10 | 2 ]",
        "[ 7/10 → 4/5 | 0 ]",
        "[ 4/5 → 9/10 | 1 ]",
        "[ 9/10 → 1/1 | 0 ]",
    ];
    const GROW_ONE: &[&str] = &[
        "[ 0/1 → 1/10 | 0 ]",
        "[ 1/10 → 1/5 | 0 ]",
        "[ 1/5 → 3/10 | 1 ]",
        "[ 3/10 → 2/5 | 0 ]",
        "[ 2/5 → 1/2 | 1 ]",
        "[ 1/2 → 3/5 | 2 ]",
        "[ 3/5 → 7/10 | 0 ]",
        "[ 7/10 → 4/5 | 1 ]",
        "[ 4/5 → 9/10 | 2 ]",
        "[ 9/10 → 1/1 | 3 ]",
    ];
    const GROW_NEGATIVE: &[&str] = &[
        "[ 0/1 → 1/10 | 3 ]",
        "[ 1/10 → 1/5 | 2 ]",
        "[ 1/5 → 3/10 | 3 ]",
        "[ 3/10 → 2/5 | 1 ]",
        "[ 2/5 → 1/2 | 2 ]",
        "[ 1/2 → 3/5 | 3 ]",
        "[ 3/5 → 7/10 | 0 ]",
        "[ 7/10 → 4/5 | 1 ]",
        "[ 4/5 → 9/10 | 2 ]",
        "[ 9/10 → 1/1 | 3 ]",
    ];
    const ZERO: &[&str] = &[
        "[ 0/1 → 1/16 | 0 ]",
        "[ 1/16 → 1/8 | 1 ]",
        "[ 1/8 → 3/16 | 2 ]",
        "[ 3/16 → 1/4 | 3 ]",
        "[ 1/4 → 5/16 | 0 ]",
        "[ 5/16 → 3/8 | 1 ]",
        "[ 3/8 → 7/16 | 2 ]",
        "[ 7/16 → 1/2 | 3 ]",
        "[ 1/2 → 9/16 | 0 ]",
        "[ 9/16 → 5/8 | 1 ]",
        "[ 5/8 → 11/16 | 2 ]",
        "[ 11/16 → 3/4 | 3 ]",
        "[ 3/4 → 13/16 | 0 ]",
        "[ 13/16 → 7/8 | 1 ]",
        "[ 7/8 → 15/16 | 2 ]",
        "[ 15/16 → 1/1 | 3 ]",
    ];
    const SHRINK_HALF: &[&str] = &[
        "[ 0/1 → 1/13 | 0 ]",
        "[ 1/13 → 2/13 | 1 ]",
        "[ 2/13 → 3/13 | 2 ]",
        "[ 3/13 → 4/13 | 3 ]",
        "[ 7/26 ⇜ (4/13 → 9/26) | 0 ]",
        "[ 9/26 → 11/26 | 1 ]",
        "[ 11/26 → 1/2 | 2 ]",
        "[ 1/2 → 15/26 | 3 ]",
        "[ 15/26 → 17/26 | 1 ]",
        "[ 17/26 → 19/26 | 2 ]",
        "[ 19/26 → 21/26 | 3 ]",
        "[ 10/13 ⇜ (21/26 → 11/13) | 1 ]",
        "[ 11/13 → 12/13 | 2 ]",
        "[ 12/13 → 1/1 | 3 ]",
    ];
    const GROW_NEGATIVE_HALF: &[&str] = &[
        "[ -1/26 ⇜ (0/1 → 1/26) | 1 ]",
        "[ 1/26 → 3/26 | 2 ]",
        "[ 3/26 → 5/26 | 3 ]",
        "[ 5/26 → 7/26 | 1 ]",
        "[ 7/26 → 9/26 | 2 ]",
        "[ 9/26 → 11/26 | 3 ]",
        "[ 5/13 ⇜ (11/26 → 6/13) | 0 ]",
        "[ 6/13 → 7/13 | 1 ]",
        "[ 7/13 → 8/13 | 2 ]",
        "[ 8/13 → 9/13 | 3 ]",
        "[ 9/13 → 10/13 | 0 ]",
        "[ 10/13 → 11/13 | 1 ]",
        "[ 11/13 → 12/13 | 2 ]",
        "[ 12/13 → 1/1 | 3 ]",
    ];
    const SHRINK_TWO: &[&str] = &[
        "[ 0/1 → 1/6 | 0 ]",
        "[ 1/6 → 1/3 | 1 ]",
        "[ 1/3 → 1/2 | 2 ]",
        "[ 1/2 → 2/3 | 3 ]",
        "[ 2/3 → 5/6 | 2 ]",
        "[ 5/6 → 1/1 | 3 ]",
    ];
    const GROW_TWO: &[&str] = &[
        "[ 0/1 → 1/6 | 0 ]",
        "[ 1/6 → 1/3 | 1 ]",
        "[ 1/3 → 1/2 | 0 ]",
        "[ 1/2 → 2/3 | 1 ]",
        "[ 2/3 → 5/6 | 2 ]",
        "[ 5/6 → 1/1 | 3 ]",
    ];
    const FULL: &[&str] = &[
        "[ 0/1 → 1/4 | 0 ]",
        "[ 1/4 → 1/2 | 1 ]",
        "[ 1/2 → 3/4 | 2 ]",
        "[ 3/4 → 1/1 | 3 ]",
    ];

    let cases: &[(&str, &[&str])] = &[
        ("sequence(0, 1, 2, 3).shrink(1)", SHRINK_ONE),
        ("shrink(1, sequence(0, 1, 2, 3))", SHRINK_ONE),
        ("shrink(1)(sequence(0, 1, 2, 3))", SHRINK_ONE),
        ("sequence(0, 1, 2, 3).shrink(-1)", SHRINK_NEGATIVE),
        ("sequence(0, 1, 2, 3).shrink(0)", ZERO),
        ("sequence(0, 1, 2, 3).shrink(0.5)", SHRINK_HALF),
        ("sequence(0, 1, 2, 3).shrink(2)", SHRINK_TWO),
        ("sequence(0, 1, 2, 3).shrink(5)", FULL),
        ("sequence(0, 1, 2, 3).grow(1)", GROW_ONE),
        ("grow(1, sequence(0, 1, 2, 3))", GROW_ONE),
        ("grow(1)(sequence(0, 1, 2, 3))", GROW_ONE),
        ("sequence(0, 1, 2, 3).grow(-1)", GROW_NEGATIVE),
        ("sequence(0, 1, 2, 3).grow(0)", ZERO),
        ("sequence(0, 1, 2, 3).grow(-0.5)", GROW_NEGATIVE_HALF),
        ("sequence(0, 1, 2, 3).grow(2)", GROW_TWO),
        ("sequence(0, 1, 2, 3).grow(5)", FULL),
        (
            "new Pattern(state => pure('x').query(state)).shrink(1)",
            &[],
        ),
        ("new Pattern(state => pure('x').query(state)).grow(1)", &[]),
    ];

    for &(source, expected) in cases {
        let out = ok_stdout(&["query", "--json", "-e", source]);
        let json: serde_json::Value =
            serde_json::from_str(&out).unwrap_or_else(|error| panic!("{source}: {error}"));
        let got = json["haps"]
            .as_array()
            .expect("haps")
            .iter()
            .map(|hap| hap["show"].as_str().expect("hap show").to_owned())
            .collect::<Vec<_>>();
        assert_eq!(got, expected, "{source}: wrong canonical CLI result");
    }
}

#[test]
fn canonical_pair_forms_emit_exact_cli_query_and_play_products() {
    const SHRINK_HAPS: &[&str] = &[
        "[ 0/1 → 1/7 | a ]",
        "[ 1/7 → 2/7 | b ]",
        "[ 2/7 → 3/7 | c ]",
        "[ 3/7 → 4/7 | d ]",
        "[ 4/7 → 5/7 | b ]",
        "[ 5/7 → 6/7 | c ]",
        "[ 6/7 → 1/1 | d ]",
    ];
    const GROW_HAPS: &[&str] = &[
        "[ 0/1 → 1/13 | a ]",
        "[ 1/13 → 2/13 | b ]",
        "[ (2/13 → 5/26) ⇝ 3/13 | c ]",
        "[ 5/26 → 7/26 | a ]",
        "[ 7/26 → 9/26 | b ]",
        "[ 9/26 → 11/26 | c ]",
        "[ 11/26 → 1/2 | a ]",
        "[ 1/2 → 15/26 | b ]",
        "[ 15/26 → 17/26 | c ]",
        "[ (17/26 → 9/13) ⇝ 19/26 | d ]",
        "[ 9/13 → 10/13 | a ]",
        "[ 10/13 → 11/13 | b ]",
        "[ 11/13 → 12/13 | c ]",
        "[ 12/13 → 1/1 | d ]",
    ];

    let query_shows = |source: &str| {
        let out = ok_stdout(&["query", "--json", "-e", source]);
        let json: serde_json::Value =
            serde_json::from_str(&out).unwrap_or_else(|error| panic!("{source}: {error}"));
        json["haps"]
            .as_array()
            .expect("pair haps")
            .iter()
            .map(|hap| hap["show"].as_str().expect("pair hap show").to_owned())
            .collect::<Vec<_>>()
    };

    for source in [
        "sequence('a','b','c','d').shrink([1,2])",
        "shrink([1,2],sequence('a','b','c','d'))",
        "shrink([1,2])(sequence('a','b','c','d'))",
        "sequence('a','b','c','d').s_taper([1,2])",
        "s_taper([1,2],sequence('a','b','c','d'))",
        "s_taper([1,2])(sequence('a','b','c','d'))",
    ] {
        assert_eq!(
            query_shows(source),
            SHRINK_HAPS,
            "{source}: pair timing changed"
        );
    }
    for source in [
        "sequence('a','b','c','d').grow([1,2])",
        "grow([1,2],sequence('a','b','c','d'))",
        "grow([1,2])(sequence('a','b','c','d'))",
    ] {
        assert_eq!(
            query_shows(source),
            GROW_HAPS,
            "{source}: pair timing changed"
        );
    }

    for (source, expected) in [
        (
            "sequence('a','b','c','d').shrink([1,2])",
            vec![
                ("0/1", "a"),
                ("1/7", "b"),
                ("2/7", "c"),
                ("3/7", "d"),
                ("4/7", "b"),
                ("5/7", "c"),
                ("6/7", "d"),
                ("1/1", "a"),
            ],
        ),
        (
            "sequence('a','b','c','d').grow([1,2])",
            vec![
                ("0/1", "a"),
                ("1/13", "b"),
                ("2/13", "c"),
                ("5/26", "a"),
                ("7/26", "b"),
                ("9/26", "c"),
                ("11/26", "a"),
                ("1/2", "b"),
                ("15/26", "c"),
                ("17/26", "d"),
                ("9/13", "a"),
                ("10/13", "b"),
                ("11/13", "c"),
                ("12/13", "d"),
                ("1/1", "a"),
            ],
        ),
    ] {
        let out = ok_stdout(&["trace", "--json", "-e", source, "--duration", "2"]);
        let json: serde_json::Value = serde_json::from_str(&out).expect("pair play JSON");
        assert_eq!(
            json["onsets"]
                .as_array()
                .expect("pair onsets")
                .iter()
                .map(|onset| (
                    onset["whole_begin"].as_str().expect("whole begin"),
                    onset["value_show"].as_str().expect("value show"),
                ))
                .collect::<Vec<_>>(),
            expected,
            "{source}: CLI scheduler changed pair onsets"
        );
    }
}

#[test]
fn raw_shrink_grow_reach_cli_query_play_and_typed_resource_products() {
    const SHRINK_HAPS: &[&str] = &[
        "[ 0/1 → 1/7 | a ]",
        "[ 1/7 → 2/7 | b ]",
        "[ 2/7 → 3/7 | c ]",
        "[ 3/7 → 4/7 | d ]",
        "[ 4/7 → 5/7 | b ]",
        "[ 5/7 → 6/7 | c ]",
        "[ 6/7 → 1/1 | d ]",
    ];
    const GROW_HAPS: &[&str] = &[
        "[ 0/1 → 1/13 | a ]",
        "[ 1/13 → 2/13 | b ]",
        "[ (2/13 → 5/26) ⇝ 3/13 | c ]",
        "[ 5/26 → 7/26 | a ]",
        "[ 7/26 → 9/26 | b ]",
        "[ 9/26 → 11/26 | c ]",
        "[ 11/26 → 1/2 | a ]",
        "[ 1/2 → 15/26 | b ]",
        "[ 15/26 → 17/26 | c ]",
        "[ (17/26 → 9/13) ⇝ 19/26 | d ]",
        "[ 9/13 → 10/13 | a ]",
        "[ 10/13 → 11/13 | b ]",
        "[ 11/13 → 12/13 | c ]",
        "[ 12/13 → 1/1 | d ]",
    ];

    let query_shows = |source: &str| {
        let out = ok_stdout(&["query", "--json", "-e", source]);
        let json: serde_json::Value =
            serde_json::from_str(&out).unwrap_or_else(|error| panic!("{source}: {error}"));
        json["haps"]
            .as_array()
            .expect("raw haps")
            .iter()
            .map(|hap| hap["show"].as_str().expect("raw hap show").to_owned())
            .collect::<Vec<_>>()
    };

    for (source, expected, expected_onsets) in [
        (
            "sequence('a','b','c','d')._shrink([1,2])",
            SHRINK_HAPS,
            vec![
                ("0/1", "a"),
                ("1/7", "b"),
                ("2/7", "c"),
                ("3/7", "d"),
                ("4/7", "b"),
                ("5/7", "c"),
                ("6/7", "d"),
                ("1/1", "a"),
            ],
        ),
        (
            "sequence('a','b','c','d')._grow([1,2])",
            GROW_HAPS,
            vec![
                ("0/1", "a"),
                ("1/13", "b"),
                ("2/13", "c"),
                ("5/26", "a"),
                ("7/26", "b"),
                ("9/26", "c"),
                ("11/26", "a"),
                ("1/2", "b"),
                ("15/26", "c"),
                ("17/26", "d"),
                ("9/13", "a"),
                ("10/13", "b"),
                ("11/13", "c"),
                ("12/13", "d"),
                ("1/1", "a"),
            ],
        ),
    ] {
        assert_eq!(
            query_shows(source),
            expected,
            "{source}: CLI raw query timing changed"
        );
        let out = ok_stdout(&["trace", "--json", "-e", source, "--duration", "2"]);
        let json: serde_json::Value = serde_json::from_str(&out).expect("raw play JSON");
        assert_eq!(
            json["onsets"]
                .as_array()
                .expect("raw onsets")
                .iter()
                .map(|onset| (
                    onset["whole_begin"].as_str().expect("whole begin"),
                    onset["value_show"].as_str().expect("value show"),
                ))
                .collect::<Vec<_>>(),
            expected_onsets,
            "{source}: CLI raw scheduler onsets changed"
        );
    }

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let over = limit + 1;
    for name in ["_shrink", "_grow"] {
        let source = format!("gap({over}).{name}(0)");
        let result = run(&["query", "--json", "-e", &source]);
        assert_eq!(
            result.status.code(),
            Some(3),
            "{name}: raw MAX+1 was not a typed refusal: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(result.stdout.is_empty(), "{name}: refusal emitted stdout");
        let error: serde_json::Value =
            serde_json::from_slice(&result.stderr).expect("raw resource envelope");
        assert_eq!(error["error"]["kind"], "resource-limit");
        let message = error["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("shrink/grow")
                && message.contains(&over.to_string())
                && message.contains(&limit.to_string()),
            "{name}: wrong raw resource refusal: {error}"
        );
    }
}

#[test]
fn patterned_mutable_shrinklist_dispatch_reaches_cli_query_and_play() {
    let source = |name: &str| {
        format!(
            r#"
              (() => {{
                let firstCalls = 0;
                let secondCalls = 0;
                const receiver = sequence('a', 'b', 'c', 'd');
                receiver.shrinklist = function () {{
                  firstCalls++;
                  return [pure(`old${{firstCalls}}`)];
                }};
                const result = receiver.{name}(sequence(1, 2));
                if (firstCalls !== 2 || result._steps.show() !== '2/1') {{
                  throw new Error('wrong eager mutable canonical phase');
                }}
                receiver.shrinklist = function () {{
                  secondCalls++;
                  return [pure(`new${{secondCalls}}`)];
                }};
                return result;
              }})()
            "#
        )
    };

    for name in ["shrink", "grow", "s_taper"] {
        let source = source(name);
        let out = ok_stdout(&["query", "--json", "-e", &source]);
        let json: serde_json::Value =
            serde_json::from_str(&out).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(
            json["haps"]
                .as_array()
                .expect("mutable canonical haps")
                .iter()
                .map(|hap| hap["value"].as_str().expect("mutable value"))
                .collect::<Vec<_>>(),
            ["new1", "new2"],
            "CLI query did not run replacement shrinklist twice for {name}"
        );
    }

    let source = source("shrink");
    let out = ok_stdout(&["trace", "--json", "-e", &source, "--duration", "2"]);
    let json: serde_json::Value = serde_json::from_str(&out).expect("mutable play JSON");
    let onsets = json["onsets"].as_array().expect("mutable play onsets");
    assert!(!onsets.is_empty(), "mutable canonical CLI play was silent");
    assert!(
        onsets.iter().all(|onset| onset["value_show"]
            .as_str()
            .is_some_and(|value| value.starts_with("new"))),
        "CLI play stopped using the post-construction shrinklist override: {out}"
    );
}

#[test]
fn canonical_shrink_grow_patterned_windows_reach_cli_query_and_play() {
    let query = |source: &str, begin: &str, end: &str| {
        let out = ok_stdout(&[
            "query", "--json", "-e", source, "--begin", begin, "--end", end,
        ]);
        let json: serde_json::Value =
            serde_json::from_str(&out).unwrap_or_else(|error| panic!("{source}: {error}"));
        json["haps"]
            .as_array()
            .expect("haps")
            .iter()
            .map(|hap| hap["show"].as_str().expect("hap show").to_owned())
            .collect::<Vec<_>>()
    };
    let onset_view = |source: &str| {
        let out = ok_stdout(&["trace", "--json", "-e", source, "--duration", "4"]);
        let json: serde_json::Value = serde_json::from_str(&out).expect("play JSON");
        json["onsets"]
            .as_array()
            .expect("onsets")
            .iter()
            .map(|onset| {
                (
                    onset["whole_begin"]
                        .as_str()
                        .expect("whole begin")
                        .to_owned(),
                    onset["value_show"].as_str().expect("value show").to_owned(),
                )
            })
            .collect::<Vec<_>>()
    };

    const SHRINK: &str = "sequence(0, 1, 2, 3).shrink(slowcat(1, 2))";
    let shrink_joined = query(SHRINK, "0", "2");
    assert_eq!(shrink_joined.len(), 20);
    assert_eq!(
        query(SHRINK, "1", "2"),
        [
            "[ 1/1 → 7/6 | 0 ]",
            "[ 7/6 → 4/3 | 1 ]",
            "[ 4/3 → 3/2 | 2 ]",
            "[ 3/2 → 5/3 | 3 ]",
            "[ 5/3 → 11/6 | 2 ]",
            "[ 11/6 → 2/1 | 3 ]",
        ]
    );
    assert_eq!(query(SHRINK, "0", "2"), shrink_joined);
    assert_eq!(
        onset_view(SHRINK),
        [
            ("0/1".into(), "0".into()),
            ("1/10".into(), "1".into()),
            ("1/5".into(), "2".into()),
            ("3/10".into(), "3".into()),
            ("2/5".into(), "1".into()),
            ("1/2".into(), "2".into()),
            ("3/5".into(), "3".into()),
            ("7/10".into(), "2".into()),
            ("4/5".into(), "3".into()),
            ("9/10".into(), "3".into()),
            ("1/1".into(), "0".into()),
            ("7/6".into(), "1".into()),
            ("4/3".into(), "2".into()),
            ("3/2".into(), "3".into()),
            ("5/3".into(), "2".into()),
            ("11/6".into(), "3".into()),
            ("2/1".into(), "0".into()),
        ]
    );

    const GROW: &str = "sequence(0, 1, 2, 3).grow(slowcat(1, 2))";
    let grow_joined = query(GROW, "0", "2");
    assert_eq!(grow_joined.len(), 20);
    assert_eq!(
        query(GROW, "1", "2"),
        [
            "[ 1/1 → 7/6 | 0 ]",
            "[ 7/6 → 4/3 | 1 ]",
            "[ 4/3 → 3/2 | 0 ]",
            "[ 3/2 → 5/3 | 1 ]",
            "[ 5/3 → 11/6 | 2 ]",
            "[ 11/6 → 2/1 | 3 ]",
        ]
    );
    assert_eq!(query(GROW, "0", "2"), grow_joined);
    assert_eq!(
        onset_view(GROW),
        [
            ("0/1".into(), "0".into()),
            ("1/10".into(), "0".into()),
            ("1/5".into(), "1".into()),
            ("3/10".into(), "0".into()),
            ("2/5".into(), "1".into()),
            ("1/2".into(), "2".into()),
            ("3/5".into(), "0".into()),
            ("7/10".into(), "1".into()),
            ("4/5".into(), "2".into()),
            ("9/10".into(), "3".into()),
            ("1/1".into(), "0".into()),
            ("7/6".into(), "1".into()),
            ("4/3".into(), "0".into()),
            ("3/2".into(), "1".into()),
            ("5/3".into(), "2".into()),
            ("11/6".into(), "3".into()),
            ("2/1".into(), "0".into()),
        ]
    );
}

#[test]
fn shrink_grow_expansion_boundary_is_typed_and_invalid_infinity_never_expands() {
    let limit = rustel_core::MAX_STEPWISE_SEGMENTS;
    let over = limit + 1;
    let assert_typed_refusal = |operation: &str, label: &str, source: &str| {
        let result = run(&["query", "--json", "-e", source]);
        assert_eq!(
            result.status.code(),
            Some(3),
            "{label} was not a typed resource refusal: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(result.stdout.is_empty(), "{label} emitted stdout");
        let error: serde_json::Value =
            serde_json::from_slice(&result.stderr).expect("resource-limit envelope");
        assert_eq!(error["error"]["kind"], "resource-limit", "{label}: {error}");
        let message = error["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("shrink/grow")
                && message.contains(&over.to_string())
                && message.contains(&limit.to_string()),
            "wrong {operation} {label} refusal: {error}"
        );
    };

    for operation in ["shrink", "grow"] {
        // A silent receiver isolates score construction from query hap volume:
        // accepting the exact bound still has to build the full bounded graph,
        // while stdout remains a small, unambiguously empty query.
        let exact = format!("gap({limit}).{operation}(0)");
        let exact = ok_stdout(&["query", "--json", "-e", &exact]);
        let exact: serde_json::Value =
            serde_json::from_str(&exact).expect("exact expansion query JSON");
        assert!(
            exact["haps"].as_array().expect("exact haps").is_empty(),
            "the exact accepted expansion emitted haps"
        );

        let source = format!("gap({over}).{operation}(0)");
        let result = run(&["query", "--json", "-e", &source]);
        assert_eq!(
            result.status.code(),
            Some(3),
            "{operation} MAX+1 was not a typed resource refusal: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            result.stdout.is_empty(),
            "a refused expansion emitted stdout"
        );
        let error: serde_json::Value =
            serde_json::from_slice(&result.stderr).expect("resource-limit envelope");
        assert_eq!(error["error"]["kind"], "resource-limit");
        let message = error["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("shrink/grow")
                && message.contains(&over.to_string())
                && message.contains(&limit.to_string()),
            "wrong stepwise expansion refusal: {error}"
        );

        // Infinity is invalid numeric input, not zero. The current queryArc
        // boundary may surface it as an empty query, while a construction
        // boundary may reject the command. Either way it must
        // not enter the valid zero-amount expansion loop, panic, or abort.
        let zero_control = format!("sequence(0).{operation}(0)");
        let zero_control = ok_stdout(&["query", "--json", "-e", &zero_control]);
        let zero_control: serde_json::Value =
            serde_json::from_str(&zero_control).expect("zero control query JSON");
        assert_eq!(
            zero_control["haps"].as_array().expect("zero haps").len(),
            1,
            "zero control stopped exercising the valid expansion path"
        );

        let infinity = format!("sequence(0).{operation}(Infinity)");
        let result = run(&["query", "--json", "-e", &infinity]);
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(
            result.status.code().is_some() && result.status.code() != Some(101),
            "{operation}(Infinity) crashed instead of failing safely: {stderr}"
        );
        if result.status.success() {
            let output: serde_json::Value = serde_json::from_slice(&result.stdout)
                .expect("query-boundary Infinity response JSON");
            assert!(
                output["haps"].as_array().expect("Infinity haps").is_empty(),
                "{operation}(Infinity) was reinterpreted as zero"
            );
        } else {
            assert!(
                !stderr.trim().is_empty(),
                "invalid infinity had no diagnostic"
            );
        }
        assert!(
            !stderr.contains("progressive segments"),
            "invalid Infinity was laundered into zero expansion: {stderr}"
        );

        // A modest accepted control proves pair payloads reach the CLI's
        // default-helper handoff without repeating another near-deadline MAX
        // build. MAX+1 still pins the product's typed canonical refusal.
        let pair_accepted = format!("gap(128).{operation}([0,128])");
        let pair_accepted = ok_stdout(&["query", "--json", "-e", &pair_accepted]);
        let pair_accepted: serde_json::Value =
            serde_json::from_str(&pair_accepted).expect("accepted pair query JSON");
        assert!(
            pair_accepted["haps"]
                .as_array()
                .expect("accepted pair haps")
                .is_empty(),
            "accepted pair {operation} emitted from a silent receiver"
        );
        let pair_over = format!("gap({over}).{operation}([0,{over}])");
        assert_typed_refusal(operation, "pair MAX+1", &pair_over);

        // The matching accepted override proves the canonical body charges an
        // ordinary returned Array itself; its MAX+1 sibling below is the
        // exact public boundary.
        let expected_reverse = if operation == "grow" { 1 } else { 0 };
        let custom_accepted = format!(
            r#"
              (() => {{
                let reverseCalls = 0;
                const receiver = gap(1);
                receiver.shrinklist = function () {{
                  const list = Array(128).fill(this);
                  list.reverse = function () {{
                    reverseCalls++;
                    return Array.prototype.reverse.call(this);
                  }};
                  return list;
                }};
                const result = receiver.{operation}(1);
                if (reverseCalls !== {expected_reverse}) {{
                  throw new Error('accepted Array reverse phase changed');
                }}
                return result;
              }})()
            "#
        );
        let custom_accepted = ok_stdout(&["query", "--json", "-e", &custom_accepted]);
        let custom_accepted: serde_json::Value =
            serde_json::from_str(&custom_accepted).expect("accepted override query JSON");
        assert!(
            custom_accepted["haps"]
                .as_array()
                .expect("accepted override haps")
                .is_empty(),
            "accepted override {operation} emitted from a silent receiver"
        );

        // Native safety intentionally rejects oversized ordinary Arrays
        // before grow reverses them or either canonical spreads/reduces them.
        // Proxy/accessor observation order remains a separate residual.
        let custom_over = format!(
            r#"
              (() => {{
                let reverseCalls = 0;
                let iteratorCalls = 0;
                let reduceCalls = 0;
                const receiver = gap(1);
                receiver.shrinklist = function () {{
                  const list = Array({over}).fill(this);
                  list.reverse = function () {{
                    reverseCalls++;
                    return Array.prototype.reverse.call(this);
                  }};
                  list[Symbol.iterator] = function () {{
                    iteratorCalls++;
                    return Array.prototype[Symbol.iterator].call(this);
                  }};
                  list.reduce = function (...args) {{
                    reduceCalls++;
                    return Array.prototype.reduce.apply(this, args);
                  }};
                  return list;
                }};
                const result = receiver.{operation}(1);
                if (reverseCalls !== 0 || iteratorCalls !== 0 || reduceCalls !== 0) {{
                  throw new Error('oversized Array entered a mutating phase');
                }}
                return result;
              }})()
            "#
        );
        assert_typed_refusal(operation, "override MAX+1", &custom_over);
    }
}

#[test]
fn canonical_tour_forms_reach_cli_query_play_and_the_documented_example() {
    const EXPECTED: &[&str] = &[
        "[ 0/1 → 1/9 | a ]",
        "[ 1/9 → 2/9 | b ]",
        "[ 2/9 → 1/3 | x ]",
        "[ 1/3 → 4/9 | a ]",
        "[ 4/9 → 5/9 | x ]",
        "[ 5/9 → 2/3 | b ]",
        "[ 2/3 → 7/9 | x ]",
        "[ 7/9 → 8/9 | a ]",
        "[ 8/9 → 1/1 | b ]",
    ];
    let forms = [
        "pure('x').tour(pure('a'), pure('b'))",
        "tour(pure('x'), pure('a'), pure('b'))",
        "s_tour(pure('x'), pure('a'), pure('b'))",
        "pure('x').s_tour(pure('a'), pure('b'))",
    ];
    for source in forms {
        let out = ok_stdout(&["query", "--json", "-e", source]);
        let json: serde_json::Value =
            serde_json::from_str(&out).unwrap_or_else(|error| panic!("{source}: {error}"));
        assert_eq!(
            json["haps"]
                .as_array()
                .expect("tour haps")
                .iter()
                .map(|hap| hap["show"].as_str().expect("hap show"))
                .collect::<Vec<_>>(),
            EXPECTED,
            "{source}: CLI tour ordering changed"
        );
    }

    let play = ok_stdout(&["trace", "--json", "-e", forms[0], "--duration", "2"]);
    let play: serde_json::Value = serde_json::from_str(&play).expect("tour play JSON");
    assert_eq!(
        play["onsets"]
            .as_array()
            .expect("tour onsets")
            .iter()
            .map(|onset| (
                onset["whole_begin"].as_str().expect("whole begin"),
                onset["value_show"].as_str().expect("value show"),
            ))
            .collect::<Vec<_>>(),
        [
            ("0/1", "a"),
            ("1/9", "b"),
            ("2/9", "x"),
            ("1/3", "a"),
            ("4/9", "x"),
            ("5/9", "b"),
            ("2/3", "x"),
            ("7/9", "a"),
            ("8/9", "b"),
            ("1/1", "a"),
        ],
        "CLI scheduler changed tour's inclusive-boundary onsets"
    );

    const DOCUMENTED: &str = r#""[c g]".tour("e f", "e f g", "g f e c").note()
      .sound("folkharp")
      .pace(8)"#;
    let out = ok_stdout(&["query", "--json", "-e", DOCUMENTED]);
    let json: serde_json::Value = serde_json::from_str(&out).expect("tour example JSON");
    let haps = json["haps"].as_array().expect("tour example haps");
    assert!(
        !haps.is_empty(),
        "the canonical tour documentation example was silent"
    );
    assert!(
        haps.iter().all(|hap| {
            hap["value"]
                .as_object()
                .is_some_and(|value| value.contains_key("note") && value.contains_key("s"))
        }),
        "the tour documentation example lost note/sound controls: {out}"
    );
}

#[test]
fn canonical_stepalt_and_s_alt_reach_cli_query_play_and_the_documented_example() {
    const EXPECTED: &[&str] = &[
        "[ 0/1 → 1/12 | a ]",
        "[ 1/12 → 1/6 | c ]",
        "[ 1/6 → 1/4 | b ]",
        "[ 1/4 → 1/3 | d ]",
        "[ 1/3 → 5/12 | a ]",
        "[ 5/12 → 1/2 | e ]",
        "[ 1/2 → 7/12 | b ]",
        "[ 7/12 → 2/3 | c ]",
        "[ 2/3 → 3/4 | a ]",
        "[ 3/4 → 5/6 | d ]",
        "[ 5/6 → 11/12 | b ]",
        "[ 11/12 → 1/1 | e ]",
    ];
    let forms = [
        "stepalt([pure('a'), pure('b')], [pure('c'), pure('d'), pure('e')])",
        "s_alt([pure('a'), pure('b')], [pure('c'), pure('d'), pure('e')])",
    ];
    for source in forms {
        let out = ok_stdout(&["query", "--json", "-e", source]);
        let json: serde_json::Value =
            serde_json::from_str(&out).unwrap_or_else(|error| panic!("{source}: {error}"));
        assert_eq!(
            json["haps"]
                .as_array()
                .expect("stepalt haps")
                .iter()
                .map(|hap| hap["show"].as_str().expect("hap show"))
                .collect::<Vec<_>>(),
            EXPECTED,
            "{source}: CLI stepalt LCM ordering changed"
        );
    }

    let play = ok_stdout(&["trace", "--json", "-e", forms[0], "--duration", "2"]);
    let play: serde_json::Value = serde_json::from_str(&play).expect("stepalt play JSON");
    assert_eq!(
        play["onsets"]
            .as_array()
            .expect("stepalt onsets")
            .iter()
            .map(|onset| (
                onset["whole_begin"].as_str().expect("whole begin"),
                onset["value_show"].as_str().expect("value show"),
            ))
            .collect::<Vec<_>>(),
        [
            ("0/1", "a"),
            ("1/12", "c"),
            ("1/6", "b"),
            ("1/4", "d"),
            ("1/3", "a"),
            ("5/12", "e"),
            ("1/2", "b"),
            ("7/12", "c"),
            ("2/3", "a"),
            ("3/4", "d"),
            ("5/6", "b"),
            ("11/12", "e"),
            ("1/1", "a"),
        ],
        "CLI scheduler changed stepalt's inclusive-boundary onsets"
    );

    const DOCUMENTED: &str = r#"stepalt(["bd cp", "mt"], "bd").sound()"#;
    let out = ok_stdout(&["query", "--json", "-e", DOCUMENTED]);
    let json: serde_json::Value = serde_json::from_str(&out).expect("stepalt example JSON");
    assert_eq!(
        json["haps"]
            .as_array()
            .expect("stepalt example haps")
            .iter()
            .map(|hap| {
                hap["value"]["s"]
                    .as_str()
                    .expect("stepalt example sound value")
            })
            .collect::<Vec<_>>(),
        ["bd", "cp", "bd", "mt", "bd"],
        "the canonical stepalt documentation example changed: {out}"
    );
}

#[test]
fn canonical_polymeter_aliases_reach_cli_query_play_and_the_documented_example() {
    const MODERN: &[&str] = &[
        "[ 0/1 → 1/6 | a ]",
        "[ 0/1 → 1/6 | x ]",
        "[ 1/6 → 1/3 | b ]",
        "[ 1/6 → 1/3 | y ]",
        "[ 1/3 → 1/2 | a ]",
        "[ 1/3 → 1/2 | z ]",
        "[ 1/2 → 2/3 | b ]",
        "[ 1/2 → 2/3 | x ]",
        "[ 2/3 → 5/6 | a ]",
        "[ 2/3 → 5/6 | y ]",
        "[ 5/6 → 1/1 | b ]",
        "[ 5/6 → 1/1 | z ]",
    ];
    let modern_forms = [
        "polymeter(sequence('a', 'b'), sequence('x', 'y', 'z'))",
        "pm(sequence('a', 'b'), sequence('x', 'y', 'z'))",
    ];
    for source in modern_forms {
        let out = ok_stdout(&["query", "--json", "-e", source]);
        let json: serde_json::Value =
            serde_json::from_str(&out).unwrap_or_else(|error| panic!("{source}: {error}"));
        assert_eq!(
            json["haps"]
                .as_array()
                .expect("modern polymeter haps")
                .iter()
                .map(|hap| hap["show"].as_str().expect("hap show"))
                .collect::<Vec<_>>(),
            MODERN,
            "{source}: CLI modern polymeter timing changed"
        );
    }

    let legacy = ok_stdout(&[
        "query",
        "--json",
        "-e",
        "s_polymeter(['a', 'b'], ['x', 'y', 'z'])",
    ]);
    let legacy: serde_json::Value = serde_json::from_str(&legacy).expect("legacy polymeter JSON");
    assert_eq!(
        legacy["haps"]
            .as_array()
            .expect("legacy polymeter haps")
            .iter()
            .map(|hap| hap["show"].as_str().expect("hap show"))
            .collect::<Vec<_>>(),
        [
            "[ 0/1 → 1/2 | a ]",
            "[ 0/1 → 1/2 | x ]",
            "[ 1/2 → 1/1 | b ]",
            "[ 1/2 → 1/1 | y ]",
        ],
        "CLI legacy polymeter used modern LCM pacing"
    );

    let play = ok_stdout(&["trace", "--json", "-e", modern_forms[0], "--duration", "2"]);
    let play: serde_json::Value = serde_json::from_str(&play).expect("polymeter play JSON");
    assert_eq!(
        play["onsets"]
            .as_array()
            .expect("polymeter onsets")
            .iter()
            .map(|onset| (
                onset["whole_begin"].as_str().expect("whole begin"),
                onset["value_show"].as_str().expect("value show"),
            ))
            .collect::<Vec<_>>(),
        [
            ("0/1", "a"),
            ("0/1", "x"),
            ("1/6", "b"),
            ("1/6", "y"),
            ("1/3", "a"),
            ("1/3", "z"),
            ("1/2", "b"),
            ("1/2", "x"),
            ("2/3", "a"),
            ("2/3", "y"),
            ("5/6", "b"),
            ("5/6", "z"),
            ("1/1", "a"),
            ("1/1", "x"),
        ],
        "CLI scheduler changed polymeter's inclusive-boundary onsets"
    );

    // Keep the executable documentation source byte-for-byte aligned with
    const DOCUMENTED: &str = r#"polymeter("c eb g", "c2 g2").note()"#;
    let out = ok_stdout(&["query", "--json", "-e", DOCUMENTED]);
    let json: serde_json::Value = serde_json::from_str(&out).expect("polymeter example JSON");
    assert_eq!(
        json["haps"]
            .as_array()
            .expect("polymeter example haps")
            .iter()
            .map(|hap| hap["value"]["note"].as_str().expect("example note"))
            .collect::<Vec<_>>(),
        [
            "c", "c2", "eb", "g2", "g", "c2", "c", "g2", "eb", "c2", "g", "g2"
        ],
        "the canonical polymeter documentation example changed: {out}"
    );
}

#[test]
fn canonical_zip_and_s_zip_reach_cli_query_play_and_the_documented_example() {
    const EXPECTED: &[&str] = &[
        "[ 0/1 → 1/6 | a0 ]",
        "[ 1/6 → 1/3 | b0 ]",
        "[ 1/3 → 1/2 | a1 ]",
        "[ 1/2 → 2/3 | b1 ]",
        "[ 2/3 → 5/6 | a0 ]",
        "[ 5/6 → 1/1 | b2 ]",
    ];
    let forms = [
        "zip(sequence('a0', 'a1'), sequence('b0', 'b1', 'b2'))",
        "s_zip(sequence('a0', 'a1'), sequence('b0', 'b1', 'b2'))",
    ];
    for source in forms {
        let out = ok_stdout(&["query", "--json", "-e", source]);
        let json: serde_json::Value =
            serde_json::from_str(&out).unwrap_or_else(|error| panic!("{source}: {error}"));
        assert_eq!(
            json["haps"]
                .as_array()
                .expect("zip haps")
                .iter()
                .map(|hap| hap["show"].as_str().expect("hap show"))
                .collect::<Vec<_>>(),
            EXPECTED,
            "{source}: CLI zip LCM ordering changed"
        );
    }

    let play = ok_stdout(&["trace", "--json", "-e", forms[0], "--duration", "2"]);
    let play: serde_json::Value = serde_json::from_str(&play).expect("zip play JSON");
    assert_eq!(
        play["onsets"]
            .as_array()
            .expect("zip onsets")
            .iter()
            .map(|onset| (
                onset["whole_begin"].as_str().expect("whole begin"),
                onset["value_show"].as_str().expect("value show"),
            ))
            .collect::<Vec<_>>(),
        [
            ("0/1", "a0"),
            ("1/6", "b0"),
            ("1/3", "a1"),
            ("1/2", "b1"),
            ("2/3", "a0"),
            ("5/6", "b2"),
            ("1/1", "a1"),
        ],
        "CLI scheduler changed zip's inclusive-boundary onsets"
    );

    const DOCUMENTED: &str = r#"zip("e f", "e f g", "g [f e] a f4 c").note()
      .sound("folkharp")
      .pace(8)"#;
    let out = ok_stdout(&["query", "--json", "-e", DOCUMENTED]);
    let json: serde_json::Value = serde_json::from_str(&out).expect("zip example JSON");
    let haps = json["haps"].as_array().expect("zip example haps");
    assert!(
        !haps.is_empty(),
        "the canonical zip documentation example was silent"
    );
    assert!(
        haps.iter().all(|hap| {
            hap["value"]
                .as_object()
                .is_some_and(|value| value.contains_key("note") && value.contains_key("s"))
        }),
        "the zip documentation example lost note/sound controls: {out}"
    );
}

#[test]
fn shrinklist_growlist_and_s_taperlist_reach_cli_query_and_play() {
    const SHRINK: &str = "stepcat(...sequence('a','b','c','d').shrinklist([1,3]))";
    const ALIAS: &str = "stepcat(...s_taperlist([1,3],sequence('a','b','c','d')))";
    const GROW: &str = "stepcat(...growlist([1,3],sequence('a','b','c','d')))";
    const SHRINK_HAPS: &[&str] = &[
        "[ 0/1 → 1/9 | a ]",
        "[ 1/9 → 2/9 | b ]",
        "[ 2/9 → 1/3 | c ]",
        "[ 1/3 → 4/9 | d ]",
        "[ 4/9 → 5/9 | b ]",
        "[ 5/9 → 2/3 | c ]",
        "[ 2/3 → 7/9 | d ]",
        "[ 7/9 → 8/9 | c ]",
        "[ 8/9 → 1/1 | d ]",
    ];

    for source in [SHRINK, ALIAS] {
        let out = ok_stdout(&["query", "--json", "-e", source]);
        let json: serde_json::Value =
            serde_json::from_str(&out).unwrap_or_else(|error| panic!("{source}: {error}"));
        assert_eq!(
            json["haps"]
                .as_array()
                .expect("shrinklist haps")
                .iter()
                .map(|hap| hap["show"].as_str().expect("hap show"))
                .collect::<Vec<_>>(),
            SHRINK_HAPS,
            "{source}: CLI shrinklist timing changed"
        );
    }

    let out = ok_stdout(&["query", "--json", "-e", GROW]);
    let json: serde_json::Value = serde_json::from_str(&out).expect("growlist query JSON");
    assert_eq!(
        json["haps"]
            .as_array()
            .expect("growlist haps")
            .iter()
            .map(|hap| hap["value"].as_str().expect("growlist value"))
            .collect::<Vec<_>>(),
        ["c", "d", "b", "c", "d", "a", "b", "c", "d"],
        "CLI growlist did not reverse the same helper Array"
    );

    let play = ok_stdout(&["trace", "--json", "-e", SHRINK, "--duration", "2"]);
    let play: serde_json::Value = serde_json::from_str(&play).expect("shrinklist play JSON");
    assert_eq!(
        play["onsets"]
            .as_array()
            .expect("shrinklist onsets")
            .iter()
            .map(|onset| (
                onset["whole_begin"].as_str().expect("whole begin"),
                onset["value_show"].as_str().expect("value show"),
            ))
            .collect::<Vec<_>>(),
        [
            ("0/1", "a"),
            ("1/9", "b"),
            ("2/9", "c"),
            ("1/3", "d"),
            ("4/9", "b"),
            ("5/9", "c"),
            ("2/3", "d"),
            ("7/9", "c"),
            ("8/9", "d"),
            ("1/1", "a"),
        ],
        "CLI scheduler changed shrinklist inclusive-boundary onsets"
    );

    for helper in ["s_taperlist", "growlist"] {
        let source = format!("stepcat(...{helper}([0, 16385], gap(16385)))");
        let result = run(&["query", "--json", "-e", &source]);
        assert_eq!(
            result.status.code(),
            Some(3),
            "{helper}: MAX+1 helper query was not a typed resource refusal"
        );
        assert!(
            result.stdout.is_empty(),
            "{helper}: refused helper query emitted stdout"
        );
        let error: serde_json::Value =
            serde_json::from_slice(&result.stderr).expect("list-helper resource envelope");
        assert_eq!(error["error"]["kind"], "resource-limit");
        let message = error["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("shrinklist")
                && message.contains("16385")
                && message.contains("16384"),
            "{helper}: wrong list-helper resource refusal: {error}"
        );
    }
}

#[test]
fn tour_expansion_boundary_is_typed_at_cli_query() {
    let exact = "silence.tour(...Array.from({ length: 127 }, () => silence))";
    let exact = ok_stdout(&["query", "--json", "-e", exact]);
    let exact: serde_json::Value =
        serde_json::from_str(&exact).expect("exact-bound tour query JSON");
    assert!(
        exact["haps"]
            .as_array()
            .expect("exact-bound haps")
            .is_empty(),
        "the all-silent exact-bound tour emitted a hap"
    );

    let over = "silence.tour(...Array.from({ length: 128 }, () => silence))";
    let result = run(&["query", "--json", "-e", over]);
    assert_eq!(
        result.status.code(),
        Some(3),
        "oversized tour was not a typed resource refusal: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(result.stdout.is_empty(), "a refused tour emitted stdout");
    let error: serde_json::Value =
        serde_json::from_slice(&result.stderr).expect("tour resource-limit envelope");
    assert_eq!(error["error"]["kind"], "resource-limit");
    let message = error["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("tour") && message.contains("16641") && message.contains("16384"),
        "wrong tour resource refusal: {error}"
    );
}

#[test]
fn stepalt_expansion_boundary_is_typed_at_cli_query() {
    let exact = "stepalt(Array(8192).fill(nothing), [nothing])";
    let exact = ok_stdout(&["query", "--json", "-e", exact]);
    let exact: serde_json::Value =
        serde_json::from_str(&exact).expect("exact-bound stepalt query JSON");
    assert!(
        exact["haps"]
            .as_array()
            .expect("exact-bound stepalt haps")
            .is_empty(),
        "the all-filtered exact-bound stepalt emitted a hap"
    );

    let over = "stepalt(Array(8193).fill(nothing), [nothing])";
    let result = run(&["query", "--json", "-e", over]);
    assert_eq!(
        result.status.code(),
        Some(3),
        "oversized stepalt was not a typed resource refusal: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(result.stdout.is_empty(), "a refused stepalt emitted stdout");
    let error: serde_json::Value =
        serde_json::from_slice(&result.stderr).expect("stepalt resource-limit envelope");
    assert_eq!(error["error"]["kind"], "resource-limit");
    let message = error["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("stepalt") && message.contains("16386") && message.contains("16384"),
        "wrong stepalt resource refusal: {error}"
    );
}

#[test]
fn polymeter_modern_and_legacy_boundaries_are_typed_at_cli_query() {
    for (source, label) in [
        ("polymeter(gap(1), gap(16383))", "modern"),
        ("polymeter(Array(8192).fill(silence), [silence])", "legacy"),
    ] {
        let exact = ok_stdout(&["query", "--json", "-e", source]);
        let exact: serde_json::Value = serde_json::from_str(&exact)
            .unwrap_or_else(|error| panic!("exact {label} polymeter JSON: {error}"));
        assert!(
            exact["haps"]
                .as_array()
                .unwrap_or_else(|| panic!("exact {label} polymeter haps"))
                .is_empty(),
            "all-silent exact {label} polymeter emitted a hap"
        );
    }

    for (source, minimum_entries, label) in [
        ("polymeter(gap(1), gap(16384))", "16385", "modern"),
        (
            "polymeter(Array(8193).fill(silence), [silence])",
            "16386",
            "legacy",
        ),
    ] {
        let result = run(&["query", "--json", "-e", source]);
        assert_eq!(
            result.status.code(),
            Some(3),
            "oversized {label} polymeter was not a typed refusal: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            result.stdout.is_empty(),
            "a refused {label} polymeter emitted stdout"
        );
        let error: serde_json::Value = serde_json::from_slice(&result.stderr)
            .unwrap_or_else(|parse| panic!("{label} resource envelope: {parse}"));
        assert_eq!(error["error"]["kind"], "resource-limit");
        let message = error["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("polymeter")
                && message.contains(minimum_entries)
                && message.contains("16384"),
            "wrong {label} polymeter refusal: {error}"
        );
    }
}

#[test]
fn query_accepts_a_source_file_as_well_as_an_expression() {
    let path = scratch("query-input.js");
    std::fs::write(&path, "s(\"bd sd\")\n").expect("write source");
    let from_file = ok_stdout(&["query", "--json", path.to_str().expect("utf-8 path")]);
    let from_eval = ok_stdout(&["query", "--json", "-e", r#"s("bd sd")"#]);
    let strip = |text: &str| {
        let mut v: serde_json::Value = serde_json::from_str(text).expect("json");
        // The report echoes its own source text; the haps are what must match.
        v["source"] = serde_json::Value::Null;
        v
    };
    assert_eq!(
        strip(&from_file),
        strip(&from_eval),
        "the file and -e paths disagree"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn query_works_for_host_required_sources() {
    for source in HOST_REQUIRED_SOURCES {
        let out = ok_stdout(&["query", "--json", "-e", source]);
        let json: serde_json::Value = serde_json::from_str(&out).expect("json");
        assert!(
            !json["haps"].as_array().expect("haps").is_empty(),
            "{source}: queried nothing"
        );
    }
}

#[test]
fn indexed_echo_aliases_match_exact_callback_and_timing_contracts() {
    let source = r#"
      (() => {
        const aliases = ['echoWith', 'echowith', 'stutWith', 'stutwith'];
        const branches = [];
        for (const name of aliases) {
          for (const form of ['method', 'free']) {
            const label = `${form}:${name}`;
            const callback = function (pattern, index) {
              if (arguments.length !== 2 || typeof index !== 'number'
                  || !Number.isInteger(index)) {
                throw new Error(`bad indexed callback: ${label}:${index}`);
              }
              return pattern.fmap(() => `${label}:${index}`);
            };
            branches.push(
              form === 'method'
                ? pure('x')[name](3, 1/4, callback)
                : globalThis[name](3, 1/4, callback, pure('x'))
            );
          }
        }
        branches.push(
          echoWith(3)(1/4)(function (pattern, index) {
            if (arguments.length !== 2 || typeof index !== 'number'
                || !Number.isInteger(index)) {
              throw new Error(`bad indexed callback: curried:echoWith:${index}`);
            }
            return pattern.fmap(() => `curried:echoWith:${index}`);
          })(pure('x'))
        );
        return stack(...branches);
      })()
    "#;
    let out = ok_stdout(&["query", "--json", "-e", source]);
    let json: serde_json::Value = serde_json::from_str(&out).expect("indexed echo JSON");
    let haps = json["haps"].as_array().expect("indexed echo haps");
    assert_eq!(haps.len(), 45, "an alias/form lost an echo branch: {out}");

    let expected = [
        ("0", "0/1", "1/1", "0/1", "1/1"),
        ("1", "-3/4", "1/4", "0/1", "1/4"),
        ("1", "1/4", "5/4", "1/4", "1/1"),
        ("2", "-1/2", "1/2", "0/1", "1/2"),
        ("2", "1/2", "3/2", "1/2", "1/1"),
    ];
    for alias in ["echoWith", "echowith", "stutWith", "stutwith"] {
        for form in ["method", "free"] {
            let prefix = format!("{form}:{alias}:");
            let mut got = haps
                .iter()
                .filter_map(|hap| {
                    let value = hap["value"].as_str()?;
                    let index = value.strip_prefix(&prefix)?;
                    Some((
                        index.to_owned(),
                        hap["whole"]["begin"].as_str().unwrap().to_owned(),
                        hap["whole"]["end"].as_str().unwrap().to_owned(),
                        hap["part"]["begin"].as_str().unwrap().to_owned(),
                        hap["part"]["end"].as_str().unwrap().to_owned(),
                    ))
                })
                .collect::<Vec<_>>();
            got.sort();
            let mut expected = expected
                .iter()
                .map(|&(index, whole_begin, whole_end, part_begin, part_end)| {
                    (
                        index.to_owned(),
                        whole_begin.to_owned(),
                        whole_end.to_owned(),
                        part_begin.to_owned(),
                        part_end.to_owned(),
                    )
                })
                .collect::<Vec<_>>();
            expected.sort();
            assert_eq!(
                got, expected,
                "{form} {alias} lost index, invocation count, or delayed timing"
            );
        }
    }
    let mut curried = haps
        .iter()
        .filter_map(|hap| {
            let index = hap["value"].as_str()?.strip_prefix("curried:echoWith:")?;
            Some((
                index.to_owned(),
                hap["whole"]["begin"].as_str().unwrap().to_owned(),
                hap["whole"]["end"].as_str().unwrap().to_owned(),
                hap["part"]["begin"].as_str().unwrap().to_owned(),
                hap["part"]["end"].as_str().unwrap().to_owned(),
            ))
        })
        .collect::<Vec<_>>();
    curried.sort();
    let mut expected_curried = expected
        .iter()
        .map(|&(index, whole_begin, whole_end, part_begin, part_end)| {
            (
                index.to_owned(),
                whole_begin.to_owned(),
                whole_end.to_owned(),
                part_begin.to_owned(),
                part_end.to_owned(),
            )
        })
        .collect::<Vec<_>>();
    expected_curried.sort();
    assert_eq!(
        curried, expected_curried,
        "fully split free-function curry lost the indexed callback contract"
    );

    // Upstream feeds raw callback results to stack, which reifies values. A
    // Pattern-only indexed bridge would reject this despite passing every
    // transformer-returning case above.
    let scalar = ok_stdout(&[
        "query",
        "--json",
        "-e",
        "pure('x').echoWith(3, 1/4, (pattern, index) => index)",
    ]);
    let scalar: serde_json::Value =
        serde_json::from_str(&scalar).expect("scalar indexed echo JSON");
    let scalar_haps = scalar["haps"].as_array().expect("scalar indexed haps");
    assert_eq!(scalar_haps.len(), 3);
    let mut values = scalar_haps
        .iter()
        .map(|hap| hap["value"].as_f64().expect("numeric index value"))
        .collect::<Vec<_>>();
    values.sort_by(f64::total_cmp);
    assert_eq!(values, [0.0, 1.0, 2.0]);
    for hap in scalar_haps {
        assert_eq!(hap["whole"]["begin"], "0/1");
        assert_eq!(hap["whole"]["end"], "1/1");
        assert_eq!(hap["part"]["begin"], "0/1");
        assert_eq!(hap["part"]["end"], "1/1");
    }
}

#[test]
fn oversized_indexed_echo_is_a_typed_resource_refusal() {
    let copies = rustel_core::combinators::MAX_ECHO_COPIES + 1;
    let source = format!("pure('x').echoWith({copies}, 1/4, (pattern, index) => pattern)");
    let result = run(&["query", "--json", "-e", &source]);
    assert_eq!(
        result.status.code(),
        Some(3),
        "oversized echoWith was not a resource refusal: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        result.stdout.is_empty(),
        "a refused echoWith emitted a plausible query: {}",
        String::from_utf8_lossy(&result.stdout)
    );
    let error: serde_json::Value =
        serde_json::from_slice(&result.stderr).expect("resource-limit envelope");
    assert_eq!(error["error"]["kind"], "resource-limit");
    let message = error["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("echoWith")
            && message.contains(&copies.to_string())
            && message.contains(&rustel_core::combinators::MAX_ECHO_COPIES.to_string()),
        "wrong indexed echo refusal: {error}"
    );
}

/// `iter` and `chunk` size a `Vec` from the caller's part count. A count such
/// as `1e9` must give a typed refusal, not an allocation that aborts the
/// process. Upstream throws a catchable `RangeError` here.
#[test]
fn oversized_iter_and_chunk_are_typed_resource_refusals() {
    let parts = rustel_core::combinators::MAX_ITER_PARTS + 1;
    let cases = [
        (format!("pure('x').iter({parts})"), "iter"),
        (format!("pure('x').iterback({parts})"), "iterBack"),
        (format!("pure('x').chunk({parts}, p => p)"), "chunk"),
        (format!("pure('x').slowchunk({parts}, p => p)"), "chunk"),
        (format!("pure('x').chunkback({parts}, p => p)"), "chunkBack"),
        (format!("pure('x').fastchunk({parts}, p => p)"), "fastchunk"),
        (format!("pure('x').every({parts}, p => p)"), "firstOf"),
        (format!("pure('x').firstOf({parts}, p => p)"), "firstOf"),
        (format!("pure('x').lastOf({parts}, p => p)"), "lastOf"),
        (format!("pure('x').applyN({parts}, p => p)"), "applyN"),
    ];
    for (source, operation) in cases {
        let result = run(&["query", "--json", "-e", &source]);
        assert_eq!(
            result.status.code(),
            Some(3),
            "{source} was not a resource refusal: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            result.stdout.is_empty(),
            "{source} emitted a plausible query: {}",
            String::from_utf8_lossy(&result.stdout)
        );
        let error: serde_json::Value =
            serde_json::from_slice(&result.stderr).expect("resource-limit envelope");
        assert_eq!(error["error"]["kind"], "resource-limit", "for {source}");
        let message = error["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains(operation)
                && message.contains(&parts.to_string())
                && message.contains(&rustel_core::combinators::MAX_ITER_PARTS.to_string()),
            "wrong refusal for {source}: {error}"
        );
    }
}

/// The guard must not move the boundary for scores that were always fine.
///
/// `chunk` AT the limit is deliberately absent: one cycle of it is ~4.9 MB of
/// haps, which fills the pipe `run` reads from and deadlocks the harness rather
/// than testing anything. Its boundary is covered by a core unit test that
/// needs no subprocess.
#[test]
fn iter_and_chunk_at_the_limit_still_play() {
    let parts = rustel_core::combinators::MAX_ITER_PARTS;
    for source in [
        format!("pure('x').iter({parts})"),
        "pure('x').chunk(64, p => p)".to_string(),
        "pure('x').chunk(4, p => p)".to_string(),
        "pure('x').iter(4)".to_string(),
    ] {
        let result = run(&["query", "--json", "-e", &source]);
        assert_eq!(
            result.status.code(),
            Some(0),
            "{source} was refused at or below the limit: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

// -- play -------------------------------------------------------------------

#[test]
fn play_emits_an_onset_timeline_without_requesting_device_audio() {
    let out = ok_stdout(&["trace", "--json", "-e", r#"s("bd sd")"#, "--duration", "2"]);
    let json: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(
        json["device_audio"], "not-requested",
        "device audio must never be claimed by this build"
    );
    assert_eq!(json["audio_backend"], "none");
    let onsets = json["onsets"].as_array().expect("onsets");
    assert!(!onsets.is_empty());
    for onset in onsets {
        for field in ["onset_id", "generation", "whole_begin", "target_time"] {
            assert!(!onset[field].is_null(), "onset lacks {field}: {onset}");
        }
    }
}

#[test]
#[cfg(not(feature = "device-audio"))]
fn device_audio_request_is_explicit_when_the_default_build_omits_it() {
    let out = run(&[
        "trace",
        "--json",
        "-e",
        r#"note("c4")"#,
        "--duration",
        "0.1",
        "--device-audio",
    ]);
    assert_eq!(out.status.code(), Some(5));
    assert!(out.stdout.is_empty());
    let error: serde_json::Value =
        serde_json::from_slice(&out.stderr).expect("structured audio error");
    assert_eq!(error["error"]["kind"], "audio");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("--features device-audio")
    );
}

#[test]
#[cfg(not(feature = "device-audio"))]
fn live_watch_request_is_explicit_when_device_audio_is_not_compiled() {
    let path = scratch("watch-device-unavailable.js");
    std::fs::write(&path, r#"note("c4")"#).expect("write watch source");
    let out = run(&[
        "trace",
        "--json",
        path.to_str().expect("utf-8 path"),
        "--watch",
        "--device-audio",
    ]);
    assert_eq!(out.status.code(), Some(5));
    assert!(out.stdout.is_empty());
    let error: serde_json::Value =
        serde_json::from_slice(&out.stderr).expect("structured live audio error");
    assert_eq!(error["error"]["kind"], "audio");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("--features device-audio")
    );
}

#[test]
fn live_watch_requires_a_rereadable_file_and_has_no_finite_duration() {
    for args in [
        vec!["trace", "--json", "--watch", "--device-audio"],
        vec!["trace", "--json", "-", "--watch", "--device-audio"],
    ] {
        let out = run(&args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        let error: serde_json::Value =
            serde_json::from_slice(&out.stderr).expect("structured watch input error");
        assert_eq!(error["error"]["kind"], "invalid-argument", "{args:?}");
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("source file"),
            "{args:?}: {error}"
        );
    }

    let path = scratch("watch-duration-conflict.js");
    std::fs::write(&path, r#"note("c4")"#).expect("write watch source");
    let out = run(&[
        "trace",
        "--json",
        path.to_str().expect("utf-8 path"),
        "--watch",
        "--device-audio",
        "--duration",
        "1",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot be used with"));
}

#[test]
fn trace_accepts_export_duration_spellings_without_rounding_seconds() {
    for (length, expected_seconds) in [
        ("3s", 3.0),
        ("0.05m", 3.0),
        ("0:03", 3.0),
        ("1e-3", 0.001),
        ("1e-3s", 0.001),
        ("1E-3 seconds", 0.001),
    ] {
        let report = ok_stdout(&["trace", "--json", "-e", "note('c4')", "--duration", length]);
        let report: serde_json::Value = serde_json::from_str(&report).expect("trace report");
        assert_eq!(report["duration_secs"], expected_seconds, "{length}");
    }

    // Bars use the tempo set by the score.
    let report = ok_stdout(&[
        "trace",
        "--json",
        "-e",
        "setcpm(60); note('c4')",
        "--cps",
        "0.25",
        "--duration",
        "2b",
    ]);
    let report: serde_json::Value = serde_json::from_str(&report).expect("trace bar report");
    assert_eq!(report["duration_secs"], 2.0);
}

#[test]
fn trace_duration_limits_keep_the_resource_error_contract() {
    for length in ["1e12", "1e12s", "1e12m", "1e12b"] {
        let (error, code) =
            error_envelope(&["trace", "--json", "-e", "note('c4')", "--duration", length]);
        assert_eq!(
            error["error"]["kind"], "resource-limit",
            "{length}: {error}"
        );
        assert_eq!(code, Some(3), "{length}: {error}");
    }
}

#[cfg(any(target_os = "windows", not(feature = "midi")))]
#[test]
fn a_virtual_midi_port_is_refused_where_none_can_exist() {
    let score = scratch("virtual-midi-refused.strudel");
    std::fs::write(&score, "note('c4')").expect("write score");
    let result = run(&[
        "play",
        score.to_str().expect("UTF-8 score"),
        "--duration",
        "1",
        "--midi-virtual",
        "rustel-vtest",
    ]);
    assert!(
        !result.status.success(),
        "a virtual MIDI port was silently ignored"
    );
    let error = String::from_utf8_lossy(&result.stderr);
    #[cfg(target_os = "windows")]
    assert!(error.contains("unavailable on Windows"), "{error}");
    #[cfg(not(target_os = "windows"))]
    assert!(
        error.contains("requires a build with the midi feature"),
        "{error}"
    );
    let _ = std::fs::remove_file(score);
}

#[test]
fn play_does_not_panic_for_host_required_sources() {
    // In-process tests cover routing; the subprocess verifies the exit code.
    for source in HOST_REQUIRED_SOURCES {
        let out = run(&["trace", "--json", "-e", source, "--duration", "2"]);
        assert_ne!(
            out.status.code(),
            Some(101),
            "{source}: `play` panicked\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "{source}: `play` failed");
        let json: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("play stdout is JSON");
        assert!(
            !json["onsets"].as_array().expect("onsets").is_empty(),
            "{source}: scheduled nothing"
        );
    }
}

// -- render -----------------------------------------------------------------

#[test]
fn render_writes_both_formats_including_for_host_required_sources() {
    for (i, source) in HOST_REQUIRED_SOURCES
        .iter()
        .chain([&r#"s("bd sd")"#])
        .enumerate()
    {
        let json_out = scratch(&format!("render-{i}.json"));
        let out = run(&[
            "render",
            "-e",
            source,
            "--duration",
            "1",
            "--format",
            "onset-json",
            "-o",
            json_out.to_str().expect("utf-8"),
        ]);
        assert_ne!(out.status.code(), Some(101), "{source}: render panicked");
        assert!(
            out.status.success(),
            "{source}: render failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(json_out.exists(), "{source}: no onset JSON written");
        let text = std::fs::read_to_string(&json_out).expect("read render output");
        serde_json::from_str::<serde_json::Value>(&text).expect("render output is JSON");
        let _ = std::fs::remove_file(&json_out);

        let wav_out = scratch(&format!("render-{i}.wav"));
        let out = run(&[
            "render",
            "-e",
            source,
            "--duration",
            "1",
            "--format",
            "wav",
            "-o",
            wav_out.to_str().expect("utf-8"),
        ]);
        assert!(out.status.success(), "{source}: wav render failed");
        let bytes = std::fs::read(&wav_out).expect("read wav");
        assert_eq!(&bytes[0..4], b"RIFF", "{source}: not a RIFF file");
        assert_eq!(&bytes[8..12], b"WAVE", "{source}: not a WAVE file");
        let _ = std::fs::remove_file(&wav_out);
    }
}

/// The `render` and `--export` success lines name the sample rate the file
/// carries in whole Hz, so a 44100 Hz bounce reads "at 44100 Hz" rather than
/// a truncated kHz figure.
#[test]
fn the_bounce_success_line_names_the_sample_rate_without_truncating_it() {
    let out = scratch("rate-44100.wav");
    let stdout = ok_stdout(&[
        "render",
        "-e",
        r#"s("bd sd")"#,
        "--duration",
        "1",
        "--sample-rate",
        "44100",
        "-o",
        out.to_str().expect("utf-8"),
    ]);
    assert!(
        stdout.contains("at 44100 Hz"),
        "the render line did not name the rate: {stdout}"
    );
    assert!(
        !stdout.contains("kHz"),
        "the render line truncates the rate to kHz: {stdout}"
    );
    let _ = std::fs::remove_file(&out);
}

/// The command line prints recoverable notices on stderr, which a library
/// Session does only when its host opts in.
#[test]
fn render_prints_a_refused_voice_on_stderr() {
    let path = scratch("refused-voice.wav");
    let out = run(&[
        "render",
        "-e",
        r#"s("supersaw").unison(100)"#,
        "--duration",
        "1",
        "--format",
        "scalar-wav",
        "-o",
        path.to_str().expect("UTF-8 path"),
    ]);
    let _ = std::fs::remove_file(&path);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "render failed: {stderr}");
    assert!(
        stderr
            .lines()
            .any(|line| line.starts_with(r#"{"voice_refused":"#) && line.contains("unison 100")),
        "the refused voice was not reported: {stderr}"
    );
}

#[test]
fn scalar_wav_renders_scheduled_notes_as_deterministic_non_silent_pcm() {
    let first = scratch("scalar-first.wav");
    let second = scratch("scalar-second.wav");
    for path in [&first, &second] {
        let out = run(&[
            "render",
            "--json",
            "-e",
            r#"note("c4 e4 g4").gain(0.5)"#,
            "--duration",
            "2",
            "--format",
            "scalar-wav",
            "--output",
            path.to_str().expect("UTF-8 path"),
        ]);
        assert!(
            out.status.success(),
            "scalar render failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let report: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("render report JSON");
        assert_eq!(report["format"], "wav-scalar-pcm");
        assert_eq!(report["audio_backend"], "scalar-rust");
        assert_eq!(report["device_audio"], "not-requested");
    }

    let first_bytes = std::fs::read(&first).expect("first WAV");
    let second_bytes = std::fs::read(&second).expect("second WAV");
    let _ = std::fs::remove_file(first);
    let _ = std::fs::remove_file(second);
    assert_eq!(
        first_bytes, second_bytes,
        "scalar bounce was not deterministic"
    );
    assert_eq!(
        wav_data(&first_bytes).len(),
        2 * 48_000 * 2 * 2,
        "the bounce must end at the requested two-second window"
    );
    assert!(
        wav_data(&first_bytes)
            .as_chunks::<2>()
            .0
            .iter()
            .any(|sample| *sample != [0, 0]),
        "scalar bounce is a false green: its PCM body is silent"
    );
}

#[test]
fn scalar_wav_renders_the_bundled_bd_sample_without_node_or_a_sample_server() {
    let path = scratch("scalar-sample.wav");
    let args = [
        "render",
        "-e",
        r#"s("bd").speed(1.25).begin(0.1).end(0.85).pan(0.3)"#,
        "--duration",
        "1",
        "--format",
        "scalar-wav",
        "--output",
        path.to_str().expect("UTF-8 path"),
    ];
    let child = rustel()
        .args(args)
        .env("PATH", "")
        .env_remove("NODE")
        .env_remove("RUSTEL_NODE")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn sample render");
    let out = wait_for_output(child, &args);
    assert!(
        out.status.success(),
        "sample render failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bytes = std::fs::read(&path).expect("sample WAV");
    let _ = std::fs::remove_file(path);
    assert_eq!(&bytes[..4], b"RIFF");
    assert!(
        wav_data(&bytes)
            .as_chunks::<2>()
            .0
            .iter()
            .any(|sample| *sample != [0, 0]),
        "bundled-sample bounce is a false green: its PCM body is silent"
    );
}

#[test]
fn export_is_a_deterministic_audible_wav_and_cycles_control_length() {
    let score = scratch("musician-export.strudel");
    let first = scratch("musician-export-first.wav");
    let second = scratch("musician-export-second.wav");
    std::fs::write(&score, r#"note("c4 e4 g4").gain(0.2)"#).expect("write score");

    for output in [&first, &second] {
        let args = [
            "export",
            score.to_str().expect("UTF-8 score"),
            "--json",
            "-o",
            output.to_str().expect("UTF-8 output"),
            "--cycles",
            "2",
            "--cps",
            "0.5",
        ];
        let child = rustel()
            .args(args)
            .env("PATH", "")
            .env_remove("NODE")
            .env_remove("RUSTEL_NODE")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn export");
        let result = wait_for_output(child, &args);
        assert!(
            result.status.success(),
            "export failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let report: serde_json::Value =
            serde_json::from_slice(&result.stdout).expect("export report");
        assert_eq!(report["format"], "wav-scalar-pcm");
        assert_eq!(report["duration_secs"], 4.0);
    }

    let first_bytes = std::fs::read(&first).expect("first WAV");
    let second_bytes = std::fs::read(&second).expect("second WAV");
    assert_eq!(&first_bytes[..4], b"RIFF");
    assert_eq!(&first_bytes[8..12], b"WAVE");
    assert_eq!(
        wav_data(&first_bytes).len(),
        4 * 48_000 * 2 * 2,
        "--cycles was parsed but did not control the bounce window"
    );
    assert!(
        wav_data(&first_bytes)
            .as_chunks::<2>()
            .0
            .iter()
            .any(|sample| *sample != [0, 0]),
        "export wrote a silent false green"
    );
    assert_eq!(first_bytes, second_bytes, "export is not stable");

    for path in [score, first, second] {
        let _ = std::fs::remove_file(path);
    }
}

/// A score that throws EAGERLY: a numeric `every` invokes its callback while
/// the score is constructed, so the evaluation itself must fail.
const EAGER_THROW_SCORE: &str =
    r#"note("c4 e4 g4").every(2, x => { throw new Error("boom-at-construction") })"#;
/// A score that throws LAZILY: `fmap`'s callback throws per query, so the
/// bounce is silent where the score is not.
const LAZY_THROW_SCORE: &str = r#"note("c4 e4").fmap(x => { throw new Error("boom-in-query") })"#;

/// Root `--export` must fail its exit status when the score throws. An eager
/// throw fails the evaluation with the callback's message. A lazy throw
/// writes the file and then fails, as `render` does.
#[test]
fn an_export_whose_score_threw_fails_the_exit_status() {
    for (name, score, expected, writes_file) in [
        (
            "export-threw-eager",
            EAGER_THROW_SCORE,
            &["boom-at-construction"][..],
            false,
        ),
        (
            "export-threw-lazy",
            LAZY_THROW_SCORE,
            &["bounce is silent", "boom-in-query"][..],
            true,
        ),
    ] {
        let score_path = scratch(&format!("{name}.strudel"));
        let out = scratch(&format!("{name}.wav"));
        std::fs::write(&score_path, score).expect("write score");
        for expect_in_stderr in expected {
            let _ = std::fs::remove_file(&out);
            assert_reported_failure(
                &[
                    "export",
                    score_path.to_str().expect("UTF-8 score"),
                    "-o",
                    out.to_str().expect("UTF-8 output"),
                    "--cycles",
                    "1",
                ],
                expect_in_stderr,
            );
            assert_eq!(out.exists(), writes_file, "{name}: {}", out.display());
        }
        for path in [score_path, out] {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// `replay --export` must fail when a save throws: a throw in any window's
/// query, or a later save that fails to evaluate on replay. A first save
/// that fails leaves nothing installed, so the bounce is refused before a
/// file exists.
#[test]
fn a_replay_export_whose_save_threw_fails_the_exit_status() {
    let quiet_save = r#"note("c4 e4").gain(0.2)"#;
    for (name, saves, expected, writes_file) in [
        (
            "replay-threw-first",
            &[(0.0, EAGER_THROW_SCORE)][..],
            &["boom-at-construction"][..],
            false,
        ),
        (
            "replay-threw-later",
            &[(0.0, quiet_save), (2.0, EAGER_THROW_SCORE)][..],
            &["failed to evaluate on replay", "boom-at-construction"][..],
            true,
        ),
        (
            "replay-threw-lazy",
            &[(0.0, LAZY_THROW_SCORE)][..],
            &["bounce is silent", "boom-in-query"][..],
            true,
        ),
    ] {
        let tape = installed_tape(&format!("{name}.rustel-session"), saves);
        let out = scratch(&format!("{name}.wav"));
        for expect_in_stderr in expected {
            let _ = std::fs::remove_file(&out);
            assert_reported_failure(
                &[
                    "replay",
                    tape.to_str().expect("UTF-8 tape"),
                    "--export",
                    out.to_str().expect("UTF-8 output"),
                ],
                expect_in_stderr,
            );
            assert_eq!(out.exists(), writes_file, "{name}: {}", out.display());
        }
        for path in [tape, out] {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// A filter predicate that throws fails open: `filterValues(x => log(x))`
/// throws for every hap, and every hap keeps playing. The bounce has all
/// eight hits of two cycles, so each export path exits with status zero.
/// The contained failure is a diagnostic, not a silent window.
#[test]
fn an_export_whose_filter_failed_open_is_complete_and_exits_zero() {
    // A built-in synth, so the audible-output assertion needs no sample
    // library.
    let source = r#"$: s("sine*4").filterValues(x => log(x))"#;
    // The render subcommand and root `--export` print their report on stdout;
    // `replay --export` reports on stderr.
    fn report_onsets(out: &Output) -> u64 {
        let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("bounce report");
        assert!(report.get("query_threw").is_none(), "{report}");
        report["onset_count"].as_u64().expect("onset count")
    }
    fn replay_onsets(out: &Output) -> u64 {
        structured_lines(&out.stderr)
            .iter()
            .find_map(|line| line["replay_export"]["onsets"].as_u64())
            .expect("replay export report")
    }
    type Onsets = fn(&Output) -> u64;

    let score = scratch("filter-open.strudel");
    std::fs::write(&score, source).expect("write score");
    // One save, four seconds of tail: two cycles.
    let tape = installed_tape("filter-open.rustel-session", &[(0.0, source)]);
    let score = score.to_str().expect("UTF-8 score").to_owned();
    let tape_arg = tape.to_str().expect("UTF-8 tape").to_owned();
    let doors: [(&str, Vec<&str>, Onsets); 3] = [
        (
            "filter-open-render.wav",
            vec![
                "render",
                "--json",
                "-e",
                source,
                "--duration",
                "4",
                "--format",
                "scalar-wav",
                "--output",
            ],
            report_onsets,
        ),
        (
            "filter-open-export.wav",
            vec!["export", score.as_str(), "--json", "--cycles", "2", "-o"],
            report_onsets,
        ),
        (
            "filter-open-replay.wav",
            vec!["replay", "--json", tape_arg.as_str(), "--export"],
            replay_onsets,
        ),
    ];
    for (output, mut args, onsets) in doors {
        let output = scratch(output);
        args.push(output.to_str().expect("UTF-8 output"));
        let out = ok_output(&args);
        let onsets = onsets(&out);
        assert!(
            onsets >= 8,
            "{args:?}: the failed-open filter lost hits: {onsets}"
        );
        assert_audible(&output);
        let _ = std::fs::remove_file(output);
    }
    for path in [PathBuf::from(score), tape] {
        let _ = std::fs::remove_file(path);
    }
}

#[test]
fn score_tempo_overrides_cli_baseline_and_controls_cycle_export() {
    let score = scratch("musician-score-tempo.strudel");
    let output = scratch("musician-score-tempo.wav");
    std::fs::write(
        &score,
        r#"
          const names = Object.keys(rustelScope)
            .filter(name => ['setCps', 'setcps', 'setCpm', 'setcpm'].includes(name));
          if (JSON.stringify(names) !== JSON.stringify(['setCps', 'setcps', 'setCpm', 'setcpm'])) {
            throw new Error(`tempo scope order mismatch: ${names}`);
          }
          if (setCps !== setcps || setCpm !== setcpm || setCps === setCpm) {
            throw new Error('tempo aliases mismatch');
          }
          if (setCps.name !== 'setCps' || setCpm.name !== 'setCpm'
              || setCps.length !== 1 || setCpm.length !== 1
              || Object.hasOwn(setCps, 'prototype') || Object.hasOwn(setCpm, 'prototype')) {
            throw new Error('tempo reflection mismatch');
          }
          if (setCps(0.75) !== silence || rustelScope.setcpm(60) !== silence) {
            throw new Error('tempo setters must return the exact silence singleton');
          }
          note("c4 e4").gain(0.2)
        "#,
    )
    .expect("write tempo score");

    let args = [
        "export",
        score.to_str().expect("UTF-8 score"),
        "--json",
        "-o",
        output.to_str().expect("UTF-8 output"),
        "--cycles",
        "2",
        "--cps",
        "0.25",
    ];
    let child = rustel()
        .args(args)
        .env("PATH", "")
        .env_remove("NODE")
        .env_remove("RUSTEL_NODE")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn tempo export");
    let result = wait_for_output(child, &args);
    assert!(
        result.status.success(),
        "tempo export failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&result.stdout).expect("tempo export report");
    assert_eq!(
        report["duration_secs"], 2.0,
        "--cycles used the stale CLI baseline instead of the committed setcpm(60)"
    );
    let bytes = std::fs::read(&output).expect("tempo WAV");
    // The `data` chunk, not the file length: a render's audio is what this
    // asserts, and the file is free to carry anything else.
    let data_at = bytes
        .windows(4)
        .position(|window| window == b"data")
        .expect("data chunk");
    let data_len = u32::from_le_bytes([
        bytes[data_at + 4],
        bytes[data_at + 5],
        bytes[data_at + 6],
        bytes[data_at + 7],
    ]) as usize;
    assert_eq!(data_len, 2 * 48_000 * 2 * 2);
    assert!(
        wav_data(&bytes)
            .as_chunks::<2>()
            .0
            .iter()
            .any(|sample| *sample != [0, 0]),
        "tempo-controlled export was silent"
    );

    for path in [score, output] {
        let _ = std::fs::remove_file(path);
    }
}

#[test]
fn unsafe_in_file_tempo_is_an_actionable_evaluation_error() {
    let out = run(&["query", "--json", "-e", "setcps(0); note('c4')"]);
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--cps"),
        "native finite-positive tempo policy did not name the CLI alternative: {stderr}"
    );
    assert!(
        !stderr.contains("mini fallback also failed"),
        "host tempo policy was laundered through Mini compatibility fallback: {stderr}"
    );
}

#[test]
fn musician_prebake_runs_before_the_score_on_the_same_heap() {
    let prebake = scratch("musician-prebake.js");
    let score = scratch("musician-prebake-score.strudel");
    let output = scratch("musician-prebake.wav");
    std::fs::write(
        &prebake,
        r#"
          await Promise.resolve();
          queueMicrotask(() => {
            const expected = [
              'pace', 'take', 'drop', 'extend', 'replicate', 'expand',
              'contract', 'shrink', 'grow',
              'Fraction', 'Pattern', 'cat', 'fastcat', 'gap', 'growlist', 'noteToMidi',
              'nothing', 'pm', 'polymeter', 'polyrhythm', 'pr', 'pure',
              'register', 'reify',
              's_add', 's_alt', 's_cat', 's_contract', 's_expand', 's_extend',
              's_polymeter', 's_sub', 's_taper', 's_taperlist', 's_tour', 's_zip',
              'seq', 'sequence',
              'setStringParser', 'shrinklist', 'silence',
              'slowcat', 'stack', 'stepalt', 'stepcat', 'steps', 'rustelScope',
              'timeCat', 'timecat', 'tour', 'zip',
              'setCps', 'setcps', 'setCpm', 'setcpm', 'cps',
              'midimaps', 'defaultmidimap',
              'voicings', 'rootNotes', 'voicing',
              'setDefaultVoicings', 'resetVoicings'
            ];
            if (JSON.stringify(Object.keys(rustelScope)) !== JSON.stringify(expected)) {
              throw new Error('foundation scope order mismatch');
            }
            for (const name of expected) {
              if (rustelScope[name] !== globalThis[name]) {
                throw new Error(`foundation scope identity mismatch: ${name}`);
              }
            }
            if (rustelScope.voicing !== globalThis.voicing) {
              throw new Error('native voicing projection lost');
            }
            if (rustelScope.polyrhythm !== rustelScope.pr
                || rustelScope.pr !== rustelScope.stack
                || rustelScope.timecat !== rustelScope.stepcat) {
              throw new Error('foundation list aliases mismatch');
            }
            const methodAliases = [
              ['s_taper', 'shrink'],
              ['s_taperlist', 'shrinklist'],
              ['s_add', 'take'], ['s_sub', 'drop'],
              ['s_expand', 'expand'], ['s_extend', 'extend'],
              ['s_contract', 'contract'], ['steps', 'pace']
            ];
            const freeAliases = [
              ...methodAliases,
              ['s_alt', 'stepalt'],
              ['s_cat', 'stepcat'], ['timeCat', 'stepcat'],
              ['timecat', 'stepcat']
            ];
            const ordinary = (object, name, value) => {
              const descriptor = Object.getOwnPropertyDescriptor(object, name);
              return descriptor && Object.hasOwn(descriptor, 'value')
                && descriptor.value === value && descriptor.writable
                && descriptor.enumerable && descriptor.configurable;
            };
            for (const [alias, canonical] of freeAliases) {
              if (globalThis[alias] !== globalThis[canonical]
                  || rustelScope[alias] !== globalThis[canonical]
                  || !ordinary(globalThis, alias, globalThis[canonical])
                  || !ordinary(rustelScope, alias, globalThis[canonical])) {
                throw new Error(`stepwise free alias mismatch: ${alias}`);
              }
            }
            for (const [alias, canonical] of methodAliases) {
              if (Pattern.prototype[alias] !== Pattern.prototype[canonical]
                  || !ordinary(
                    Pattern.prototype, alias, Pattern.prototype[canonical]
                  )) {
                throw new Error(`stepwise method alias mismatch: ${alias}`);
              }
            }
            for (const name of [
              'stepalt', 's_alt', 'timecat', 'timeCat', 's_cat'
            ]) {
              if (Object.hasOwn(Pattern.prototype, name)) {
                throw new Error(`unexpected stepcat method alias: ${name}`);
              }
            }
            const forbiddenRawAliases = [
              '_s_add', '_s_sub', '_s_taper', '_s_taperlist', '_s_expand', '_s_extend',
              '_s_contract', '_s_alt', '_stepalt', '_s_cat',
              '_timeCat', '_timecat'
            ];
            for (const object of [globalThis, rustelScope, Pattern.prototype]) {
              for (const name of forbiddenRawAliases) {
                if (Object.hasOwn(object, name)) {
                  throw new Error(`unexpected raw stepwise alias: ${name}`);
                }
              }
            }

            globalThis.cliStepwiseAliases = Object.freeze({
              taper: rustelScope.s_taper,
              taperMethod: Pattern.prototype.s_taper,
              alt: rustelScope.s_alt,
              take: globalThis.s_add,
              paceMethod: Pattern.prototype.steps,
              weighted: rustelScope.timeCat
            });
            globalThis.cliAliasGlobalDecoy = function poisonedAliasGlobal() {};
            globalThis.cliAliasScopeDecoy = function poisonedAliasScope() {};
            globalThis.cliAliasPrototypeDecoy =
              function poisonedAliasPrototype() {};
            globalThis.cliTaperCanonicalDecoys = Object.freeze({
              global: function poisonedTaperGlobal() {},
              scope: function poisonedTaperScope() {},
              method: function poisonedTaperMethod() {}
            });
            globalThis.cliAltDecoys = Object.freeze({
              canonicalGlobal: function poisonedStepaltGlobal() {},
              canonicalScope: function poisonedStepaltScope() {},
              aliasGlobal: function poisonedAltGlobal() {},
              aliasScope: function poisonedAltScope() {}
            });
            globalThis.s_add = cliAliasGlobalDecoy;
            rustelScope.s_sub = cliAliasScopeDecoy;
            Pattern.prototype.s_expand = cliAliasPrototypeDecoy;
            globalThis.s_taper = cliAliasGlobalDecoy;
            rustelScope.s_taper = cliAliasScopeDecoy;
            Pattern.prototype.s_taper = cliAliasPrototypeDecoy;
            globalThis.shrink = cliTaperCanonicalDecoys.global;
            rustelScope.shrink = cliTaperCanonicalDecoys.scope;
            Pattern.prototype.shrink = cliTaperCanonicalDecoys.method;
            globalThis.stepalt = cliAltDecoys.canonicalGlobal;
            rustelScope.stepalt = cliAltDecoys.canonicalScope;
            globalThis.s_alt = cliAltDecoys.aliasGlobal;
            rustelScope.s_alt = cliAltDecoys.aliasScope;
            delete globalThis.steps;
            delete rustelScope.timeCat;
            delete Pattern.prototype.s_contract;
            globalThis.assertCliAliasPoison = () => {
              if (globalThis.s_add !== cliAliasGlobalDecoy
                  || rustelScope.s_sub !== cliAliasScopeDecoy
                  || Pattern.prototype.s_expand !== cliAliasPrototypeDecoy
                  || globalThis.s_taper !== cliAliasGlobalDecoy
                  || rustelScope.s_taper !== cliAliasScopeDecoy
                  || Pattern.prototype.s_taper !== cliAliasPrototypeDecoy
                  || globalThis.shrink !== cliTaperCanonicalDecoys.global
                  || rustelScope.shrink !== cliTaperCanonicalDecoys.scope
                  || Pattern.prototype.shrink
                      !== cliTaperCanonicalDecoys.method
                  || globalThis.stepalt !== cliAltDecoys.canonicalGlobal
                  || rustelScope.stepalt !== cliAltDecoys.canonicalScope
                  || globalThis.s_alt !== cliAltDecoys.aliasGlobal
                  || rustelScope.s_alt !== cliAltDecoys.aliasScope
                  || Object.hasOwn(Pattern.prototype, 'stepalt')
                  || Object.hasOwn(Pattern.prototype, 's_alt')
                  || Object.hasOwn(globalThis, 'steps')
                  || Object.hasOwn(rustelScope, 'timeCat')
                  || Object.hasOwn(Pattern.prototype, 's_contract')) {
                throw new Error('static stepwise aliases were reinjected');
              }
            };
            globalThis.prebakeNote = rustelScope.noteToMidi('c4');
            globalThis.setupFunctionValue = fast(2);
            globalThis.setupObjectValue = { marker: 7 };
            const directFunctionPattern = rustelScope.reify(setupFunctionValue);
            globalThis.setupFunctionPattern = rustelScope.seq([[[directFunctionPattern]]]);
            if (setupFunctionPattern !== directFunctionPattern) {
              throw new Error('list singleton identity lost');
            }
            globalThis.setupObjectPattern = rustelScope.reify(setupObjectValue);
            rustelScope.Pattern.prototype.fromSetup = function () { return this.fast(2); };
            globalThis.registeredFromSetup = rustelScope.register(
              'prebakeSlow', (factor, pat) => pat.slow(factor)
            );
            for (const name of [
              'cat', 'fastcat', 'polyrhythm', 'pr', 'seq', 'sequence',
              'slowcat', 'stack', 'stepcat', 'timecat'
            ]) {
              globalThis[name] = function poisonedListGlobal() {
                throw new Error(`mutable list global consulted: ${name}`);
              };
            }
            globalThis.setupWeighted = rustelScope.stepcat(
              [1, note('e4')], [3, note('g4')]
            );
            globalThis.setupPrototypeFast = note('c4').fastcat(note('d4'));
            globalThis.productParserCalls = 0;
            rustelScope.setStringParser(value => {
              productParserCalls++;
              return mini(value);
            });
          });
        "#,
    )
    .expect("write prebake");
    std::fs::write(
        &score,
        r#"
          (() => {
            const state = { span: { begin: 0, end: 1 }, controls: {} };
            assertCliAliasPoison();
            const aliasView = pattern => pattern.query(state).map(hap =>
              `${hap.value}:${hap.part.begin.show()}>${hap.part.end.show()}`
            ).join(',');
            const aliasTaken = cliStepwiseAliases.take(
              2, rustelScope.sequence(10, 11, 12)
            );
            const aliasPaced = cliStepwiseAliases.paceMethod.call(
              rustelScope.sequence(50, 51), 4
            );
            const aliasWeighted = cliStepwiseAliases.weighted(
              [1, pure(30)], [1, pure(31)]
            );
            const aliasAlt = cliStepwiseAliases.alt(
              [pure('a'), pure('b')], [pure('c')]
            );
            const taperBase = () => rustelScope.sequence(60, 61, 62, 63);
            const aliasTapered = cliStepwiseAliases.taper(1, taperBase());
            const aliasTaperedCurried = cliStepwiseAliases.taper(1)(taperBase());
            const aliasTaperedMethod = cliStepwiseAliases.taperMethod.call(
              taperBase(), 1
            );
            const expectedTaper = '60:0/1>1/10,61:1/10>1/5,'
              + '62:1/5>3/10,63:3/10>2/5,61:2/5>1/2,'
              + '62:1/2>3/5,63:3/5>7/10,62:7/10>4/5,'
              + '63:4/5>9/10,63:9/10>1/1';
            if (aliasView(aliasTaken) !== '10:0/1>1/2,11:1/2>1/1'
                || aliasView(aliasPaced)
                    !== '50:0/1>1/4,51:1/4>1/2,50:1/2>3/4,51:3/4>1/1'
                || aliasView(aliasWeighted)
                    !== '30:0/1>1/2,31:1/2>1/1'
                || aliasView(aliasAlt)
                    !== 'a:0/1>1/4,c:1/4>1/2,b:1/2>3/4,c:3/4>1/1'
                || [aliasTapered, aliasTaperedCurried, aliasTaperedMethod]
                    .some(pattern => pattern._steps.show() !== '10/1'
                      || aliasView(pattern) !== expectedTaper)) {
              throw new Error('saved stepwise alias semantics mismatch');
            }
            const functionHaps = setupFunctionPattern.query(state);
            const objectHaps = setupObjectPattern.query(state);
            const transformedHaps = functionHaps[0]?.value(pure('x')).query(state);
            if (functionHaps.length !== 1
                || functionHaps[0].value !== setupFunctionValue
                || transformedHaps.length !== 2
                || transformedHaps.some(hap => hap.value !== 'x')) {
              throw new Error('function identity lost');
            }
            if (objectHaps.length !== 1
                || objectHaps[0].value !== setupObjectValue
                || objectHaps[0].value.marker !== 7) {
              throw new Error('object identity lost');
            }
            if (prebakeNote !== 60) throw new Error('noteToMidi mismatch');
            const singletonHaps = setupFunctionPattern.query(state);
            if (singletonHaps.length !== 1
                || singletonHaps[0].value !== setupFunctionValue) {
              throw new Error('function singleton did not survive setup');
            }
            const weighted = setupWeighted.query(state).map(hap =>
              `${hap.value.note}:${hap.part.begin.show()}>${hap.part.end.show()}`
            );
            if (weighted.join(',') !== 'e4:0/1>1/4,g4:1/4>1/1') {
              throw new Error(`weighted list timing: ${weighted}`);
            }
            const prototypeFast = setupPrototypeFast.query(state).map(hap =>
              `${hap.value.note}:${hap.part.begin.show()}>${hap.part.end.show()}`
            );
            if (prototypeFast.join(',') !== 'c4:0/1>1/2,d4:1/2>1/1') {
              throw new Error(`prototype fastcat timing: ${prototypeFast}`);
            }
            const routed = rustelScope.stack(['a4', 'b4'].join(' ')).note();
            if (productParserCalls !== 1) {
              throw new Error(`configured parser calls: ${productParserCalls}`);
            }
            return rustelScope.stack(
              setupWeighted,
              setupPrototypeFast,
              registeredFromSetup(1, routed).fromSetup()
            );
          })()
        "#,
    )
    .expect("write score");

    let args = [
        "export",
        score.to_str().expect("UTF-8 score"),
        "--json",
        "--prebake",
        prebake.to_str().expect("UTF-8 prebake"),
        "-o",
        output.to_str().expect("UTF-8 output"),
        "--duration",
        "1",
    ];
    let result = run(&args);
    assert!(
        result.status.success(),
        "prebake session path failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&result.stdout).expect("prebaked export report");
    assert_eq!(report["duration_secs"], 1.0);
    let bytes = std::fs::read(&output).expect("prebaked WAV");
    assert_eq!(&bytes[..4], b"RIFF");
    // Parse to the data chunk rather than assuming it is the last thing in
    // the file: reading "everything after the header" as audio counts any
    // trailing bytes as sound, so a silent render would pass this test.
    let data_at = bytes
        .windows(4)
        .position(|window| window == b"data")
        .expect("data chunk");
    let data_len = u32::from_le_bytes([
        bytes[data_at + 4],
        bytes[data_at + 5],
        bytes[data_at + 6],
        bytes[data_at + 7],
    ]) as usize;
    assert_eq!(data_len, 48_000 * 2 * 2);
    let payload = &bytes[data_at + 8..data_at + 8 + data_len];
    assert!(
        payload
            .as_chunks::<2>()
            .0
            .iter()
            .any(|sample| *sample != [0, 0]),
        "same-heap helper score produced a silent false green"
    );

    for path in [prebake, score, output] {
        let _ = std::fs::remove_file(path);
    }
}

#[test]
fn projected_elementals_reach_query_and_same_heap_export() {
    let query_source = r#"
      rustelScope.stack(
        rustelScope.stepcat(
          rustelScope.gap(rustelScope.Fraction('2/6')),
          rustelScope.pure('afterGap')
        ),
        rustelScope.stepcat(
          rustelScope.nothing, rustelScope.pure('fromNothing')
        ),
        rustelScope.stepcat(
          rustelScope.silence, rustelScope.pure('fromSilence')
        )
      )
    "#;
    let out = ok_stdout(&["query", "--json", "-e", query_source]);
    let json: serde_json::Value = serde_json::from_str(&out).expect("elemental query JSON");
    let haps = json["haps"].as_array().expect("elemental query haps");
    assert_eq!(
        haps.len(),
        3,
        "gap/silence/nothing emitted or lost a hap: {out}"
    );
    let by_value = |value: &str| {
        haps.iter()
            .find(|hap| hap["value"] == value)
            .unwrap_or_else(|| panic!("missing {value:?} in {out}"))
    };
    assert_eq!(by_value("fromNothing")["part"]["begin"], "0/1");
    assert_eq!(by_value("fromNothing")["part"]["end"], "1/1");
    assert_eq!(by_value("afterGap")["part"]["begin"], "1/4");
    assert_eq!(by_value("afterGap")["part"]["end"], "1/1");
    assert_eq!(by_value("fromSilence")["part"]["begin"], "1/2");
    assert_eq!(by_value("fromSilence")["part"]["end"], "1/1");

    let prebake = scratch("musician-elemental-prebake.js");
    let score = scratch("musician-elemental-score.strudel");
    let output = scratch("musician-elemental.wav");
    std::fs::write(
        &prebake,
        r#"
          const expected = [
            'pace', 'take', 'drop', 'extend', 'replicate', 'expand',
            'contract', 'shrink', 'grow',
            'Fraction', 'Pattern', 'cat', 'fastcat', 'gap', 'growlist', 'noteToMidi',
            'nothing', 'pm', 'polymeter', 'polyrhythm', 'pr', 'pure',
            'register', 'reify',
            's_add', 's_alt', 's_cat', 's_contract', 's_expand', 's_extend',
            's_polymeter', 's_sub', 's_taper', 's_taperlist', 's_tour', 's_zip',
            'seq', 'sequence',
            'setStringParser', 'shrinklist', 'silence',
            'slowcat', 'stack', 'stepalt', 'stepcat', 'steps', 'rustelScope',
            'timeCat', 'timecat', 'tour', 'zip',
            'setCps', 'setcps', 'setCpm', 'setcpm', 'cps',
            'midimaps', 'defaultmidimap',
            'voicings', 'rootNotes', 'voicing',
            'setDefaultVoicings', 'resetVoicings'
          ];
          if (JSON.stringify(Object.keys(rustelScope)) !== JSON.stringify(expected)) {
            throw new Error('elemental product scope order');
          }
          for (const name of expected) {
            const descriptor = Object.getOwnPropertyDescriptor(rustelScope, name);
            const globalDescriptor = Object.getOwnPropertyDescriptor(globalThis, name);
            if (rustelScope[name] !== globalThis[name]
                || !descriptor?.writable || !descriptor.enumerable
                || !descriptor.configurable || !globalDescriptor?.writable
                || !globalDescriptor.enumerable || !globalDescriptor.configurable) {
              throw new Error(`elemental product scope identity: ${name}`);
            }
          }
          if (rustelScope.voicing !== globalThis.voicing) {
            throw new Error('native voicing projection lost');
          }
          const constructible = value => {
            try { Reflect.construct(value, []); return true; }
            catch (_) { return false; }
          };
          if (Fraction.name !== 'fraction' || Fraction.length !== 1
              || Object.hasOwn(Fraction, 'prototype') || constructible(Fraction)
              || gap.name !== 'gap' || gap.length !== 1
              || Object.hasOwn(gap, 'prototype') || constructible(gap)
              || pure.name !== 'pure' || pure.length !== 1
              || !Object.hasOwn(pure, 'prototype') || !constructible(pure)) {
            throw new Error('elemental product reflection');
          }
          if (Object.getPrototypeOf(Fraction('2/6'))
                  !== Fraction._original.prototype
              || Object.getPrototypeOf(gap('2/6')._steps)
                  !== Fraction._original.prototype) {
            throw new Error('elemental product Fraction identity');
          }
          if (silence === nothing || silence._steps.show() !== '1/1'
              || nothing._steps.show() !== '0/1'
              || gap(1) === silence || gap(0) === nothing
              || Object.hasOwn(silence, '__pure')
              || Object.hasOwn(nothing, '__pure')) {
            throw new Error('elemental product singletons');
          }

          globalThis.cliElementalCanonicals = {
            Fraction: rustelScope.Fraction,
            gap: rustelScope.gap,
            nothing: rustelScope.nothing,
            pure: rustelScope.pure,
            silence: rustelScope.silence,
            stack: rustelScope.stack,
            stepcat: rustelScope.stepcat
          };
          globalThis.cliElementalSilence = cliElementalCanonicals.silence;
          globalThis.cliElementalNothing = cliElementalCanonicals.nothing;
          globalThis.cliElementalObject = { marker: 11 };
          globalThis.cliElementalFunction = suffix => `owned:${suffix}`;
          globalThis.cliElementalPureObject = cliElementalCanonicals.pure(
            cliElementalObject
          );
          globalThis.cliElementalPureFunction = cliElementalCanonicals.pure(
            cliElementalFunction
          );

          globalThis.cliElementalGlobalDecoys = {};
          globalThis.cliElementalScopeDecoys = {};
          for (const name of ['Fraction', 'gap', 'pure']) {
            const globalDecoy = function poisonedElementalGlobal() {
              throw new Error(`mutable global consulted: ${name}`);
            };
            const scopeDecoy = function poisonedElementalScope() {
              throw new Error(`mutable scope consulted: ${name}`);
            };
            cliElementalGlobalDecoys[name] = globalDecoy;
            cliElementalScopeDecoys[name] = scopeDecoy;
            globalThis[name] = globalDecoy;
            rustelScope[name] = scopeDecoy;
          }
          for (const name of ['nothing', 'silence']) {
            delete globalThis[name];
            delete rustelScope[name];
          }
          for (const name of ['Pattern', 'Hap']) {
            const decoy = function poisonedElementalDependency() {
              throw new Error(`mutable dependency consulted: ${name}`);
            };
            cliElementalGlobalDecoys[name] = decoy;
            globalThis[name] = decoy;
          }
          const lexicalFraction = cliElementalCanonicals.Fraction('2/6');
          const lexicalGap = cliElementalCanonicals.gap(lexicalFraction);
          globalThis.cliElementalPostPoisonPure = cliElementalCanonicals.pure(
            cliElementalObject
          );
          if (lexicalFraction.show() !== '1/3'
              || lexicalGap._steps.show() !== '1/3'
              || cliElementalPostPoisonPure.__pure !== cliElementalObject) {
            throw new Error('elemental lexical capture');
          }
          globalThis.cliElementalExport = cliElementalCanonicals.stack(
            cliElementalCanonicals.stepcat(lexicalGap, note('c4')),
            cliElementalCanonicals.stepcat(cliElementalNothing, note('d4')),
            cliElementalCanonicals.stepcat(cliElementalSilence, note('e4'))
          );
        "#,
    )
    .expect("write elemental prebake");
    std::fs::write(
        &score,
        r#"
          (() => {
            const state = {
              span: {
                begin: cliElementalCanonicals.Fraction(0),
                end: cliElementalCanonicals.Fraction(1)
              },
              controls: {}
            };
            const objectHaps = cliElementalPureObject.query(state);
            const functionHaps = cliElementalPureFunction.query(state);
            if (objectHaps.length !== 1
                || objectHaps[0].value !== cliElementalObject
                || functionHaps.length !== 1
                || functionHaps[0].value !== cliElementalFunction
                || cliElementalPostPoisonPure.__pure !== cliElementalObject) {
              throw new Error('elemental same-heap JS identity');
            }
            if (cliElementalCanonicals.silence !== cliElementalSilence
                || cliElementalCanonicals.nothing !== cliElementalNothing
                || cliElementalCanonicals.silence === cliElementalCanonicals.nothing) {
              throw new Error('elemental same-heap singleton identity');
            }
            for (const name of ['Fraction', 'gap', 'pure']) {
              const globalDecoy = cliElementalGlobalDecoys[name];
              const scopeDecoy = cliElementalScopeDecoys[name];
              if (globalThis[name] !== globalDecoy
                  || globalDecoy.name !== 'poisonedElementalGlobal'
                  || rustelScope[name] !== scopeDecoy
                  || scopeDecoy.name !== 'poisonedElementalScope') {
                throw new Error(`static destination was reinjected: ${name}`);
              }
            }
            for (const name of ['nothing', 'silence']) {
              if (Object.hasOwn(globalThis, name)
                  || Object.hasOwn(rustelScope, name)) {
                throw new Error(`deleted static was reinjected: ${name}`);
              }
            }
            for (const name of ['Pattern', 'Hap']) {
              const decoy = cliElementalGlobalDecoys[name];
              if (globalThis[name] !== decoy
                  || decoy.name !== 'poisonedElementalDependency') {
                throw new Error(`lexical dependency was reinjected: ${name}`);
              }
            }
            return cliElementalExport;
          })()
        "#,
    )
    .expect("write elemental score");

    let args = [
        "export",
        score.to_str().expect("UTF-8 score"),
        "--json",
        "--prebake",
        prebake.to_str().expect("UTF-8 prebake"),
        "-o",
        output.to_str().expect("UTF-8 output"),
        "--duration",
        "1.5",
    ];
    let result = run(&args);
    assert!(
        result.status.success(),
        "elemental export failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&result.stdout).expect("elemental export report");
    assert_eq!(report["format"], "wav-scalar-pcm");
    assert_eq!(report["duration_secs"], 1.5);
    assert_eq!(report["onset_count"], 3);
    assert_eq!(report["device_audio"], "not-requested");
    let bytes = std::fs::read(&output).expect("elemental WAV");
    assert_eq!(&bytes[..4], b"RIFF");
    assert_eq!(wav_data(&bytes).len(), 72_000 * 2 * 2);

    for path in [prebake, score, output] {
        let _ = std::fs::remove_file(path);
    }
}

#[test]
fn musician_prebake_failures_are_structured_and_no_file_is_auto_discovered() {
    let directory = scratch("prebake-no-discovery");
    std::fs::create_dir_all(&directory).expect("create prebake directory");
    let score = directory.join("song.strudel");
    let adjacent = directory.join("prebake.js");
    let invalid = directory.join("invalid.js");
    let missing = directory.join("missing.js");
    let output = directory.join("out.wav");
    std::fs::write(&score, "note('c4')").expect("write score");
    std::fs::write(&adjacent, "throw new Error('must not auto-load')").expect("write adjacent");
    std::fs::write(
        &invalid,
        "globalThis.partial = 1; throw new Error('bad setup')",
    )
    .expect("write invalid");

    let score_text = score.to_str().expect("UTF-8 score");
    let output_text = output.to_str().expect("UTF-8 output");
    let missing_result = run(&[
        "export",
        score_text,
        "--json",
        "--prebake",
        missing.to_str().expect("UTF-8 missing"),
        "-o",
        output_text,
    ]);
    assert_eq!(missing_result.status.code(), Some(4));
    let missing_error: serde_json::Value =
        serde_json::from_slice(&missing_result.stderr).expect("missing prebake envelope");
    assert_eq!(missing_error["error"]["kind"], "io");

    let invalid_result = run(&[
        "export",
        score_text,
        "--json",
        "--prebake",
        invalid.to_str().expect("UTF-8 invalid"),
        "-o",
        output_text,
    ]);
    assert_eq!(invalid_result.status.code(), Some(1));
    let invalid_error: serde_json::Value =
        serde_json::from_slice(&invalid_result.stderr).expect("invalid prebake envelope");
    assert_eq!(invalid_error["error"]["kind"], "evaluation");
    assert!(
        invalid_error["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("bad setup")
    );
    assert!(!output.exists(), "failed prebake still rendered a score");

    let no_discovery = run(&["export", score_text, "-o", output_text, "--duration", "0.1"]);
    assert!(
        no_discovery.status.success(),
        "an adjacent prebake.js was auto-loaded: {}",
        String::from_utf8_lossy(&no_discovery.stderr)
    );
    assert!(output.exists());

    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn musician_prebake_cpu_deadline_is_a_typed_resource_refusal() {
    let prebake = scratch("musician-prebake-runaway.js");
    let score = scratch("musician-prebake-runaway.strudel");
    std::fs::write(&prebake, "while (true) {}").expect("write runaway setup");
    std::fs::write(&score, "note('c4')").expect("write score");
    let result = run(&[
        "play",
        score.to_str().expect("UTF-8 score"),
        "-vvv",
        "--prebake",
        prebake.to_str().expect("UTF-8 setup"),
    ]);
    assert_eq!(result.status.code(), Some(3));
    let error: serde_json::Value =
        serde_json::from_slice(&result.stderr).expect("resource-limit envelope");
    assert_eq!(error["error"]["kind"], "resource-limit");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("CPU deadline")
    );
    let _ = std::fs::remove_file(prebake);
    let _ = std::fs::remove_file(score);
}

#[test]
fn synchronous_score_cpu_deadline_is_typed_on_every_initial_cli_route() {
    let score = scratch("initial-score-runaway.strudel");
    let render = scratch("initial-score-runaway.json");
    std::fs::write(&score, "globalThis.initialScorePrefix = 1; while (true) {}")
        .expect("write runaway score");
    let source = "globalThis.initialScorePrefix = 1; while (true) {}";
    let cases = vec![
        vec!["query".into(), "--json".into(), "-e".into(), source.into()],
        vec![
            "trace".into(),
            "--json".into(),
            "-e".into(),
            source.into(),
            "--duration".into(),
            "0.1".into(),
        ],
        vec![
            "render".into(),
            "--json".into(),
            "-e".into(),
            source.into(),
            "--duration".into(),
            "0.1".into(),
            "-o".into(),
            render.to_string_lossy().into_owned(),
            "--format".into(),
            "onset-json".into(),
        ],
        vec![
            "bench".into(),
            "--json".into(),
            "-e".into(),
            source.into(),
            "--iterations".into(),
            "1".into(),
        ],
        vec![
            "play".into(),
            score.to_string_lossy().into_owned(),
            "-vvv".into(),
        ],
    ];
    for case in cases {
        let args = case.iter().map(String::as_str).collect::<Vec<_>>();
        let result = run(&args);
        assert_eq!(
            result.status.code(),
            Some(3),
            "{args:?} did not report the score CPU limit: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let error: serde_json::Value =
            serde_json::from_slice(&result.stderr).expect("resource-limit envelope");
        assert_eq!(error["error"]["kind"], "resource-limit", "{args:?}");
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("2000 ms CPU deadline"),
            "{args:?} returned the wrong refusal: {error}"
        );
    }
    assert!(
        !render.exists(),
        "a refused initial score still rendered output"
    );
    let _ = std::fs::remove_file(score);
}

#[test]
fn query_time_javascript_deadline_is_structured_on_every_cli_route() {
    let score = scratch("query-turn-runaway.strudel");
    let render = scratch("query-turn-runaway-render.json");
    let export = scratch("query-turn-runaway-export.wav");
    std::fs::write(&score, QUERY_TIME_RUNAWAY).expect("write query-time runaway score");
    let _ = std::fs::remove_file(&render);
    let _ = std::fs::remove_file(&export);

    let cases = query_time_cli_cases(&score, &render, &export);
    let outputs = CliBatch::spawn(&cases).collect(std::time::Duration::from_secs(8));
    for (args, output) in cases.iter().zip(outputs) {
        assert_eq!(
            output.status.code(),
            Some(3),
            "{args:?} did not report the query CPU limit\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stdout.is_empty(),
            "{args:?} wrote partial success data before refusing the query: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let error: serde_json::Value = serde_json::from_slice(&output.stderr)
            .unwrap_or_else(|parse| panic!("{args:?} returned no JSON envelope: {parse}"));
        assert_eq!(error["error"]["kind"], "resource-limit", "{args:?}");
        let message = error["error"]["message"]
            .as_str()
            .unwrap_or_else(|| panic!("{args:?} returned no deadline message: {error}"));
        assert_eq!(
            message.matches("2000 ms CPU deadline").count(),
            1,
            "{args:?} used the wrong query-time deadline: {error}"
        );
    }
    assert!(!render.exists(), "a refused render created its output");
    assert!(!export.exists(), "a refused export created its WAV");

    // The error kind comes from the error type, not from its text. A score
    // that throws the deadline wording is an ordinary throw: empty haps, as
    // strudel.cc `queryArc` gives, and never the typed deadline refusal.
    let spoof = run(&[
        "query",
        "--json",
        "-e",
        r#"new Pattern(() => { throw new Error('2000 ms CPU deadline'); })"#,
    ]);
    assert_eq!(
        spoof.status.code(),
        Some(1),
        "ordinary throw text spoofed the typed deadline: {}",
        String::from_utf8_lossy(&spoof.stderr)
    );
    let error: serde_json::Value =
        serde_json::from_slice(&spoof.stderr).expect("spoof control error JSON");
    assert_eq!(error["error"]["kind"], "invalid-argument", "{error}");
    let json: serde_json::Value =
        serde_json::from_slice(&spoof.stdout).expect("spoof control query JSON");
    assert!(
        json["haps"].as_array().is_some_and(Vec::is_empty),
        "ordinary query throw did not remain queryArc silence: {json}"
    );
    assert!(
        json["query_threw"]
            .as_str()
            .is_some_and(|message| message.contains("2000 ms CPU deadline")),
        "the ordinary throw lost its message: {json}"
    );

    let _ = std::fs::remove_file(score);
}

#[cfg(unix)]
#[test]
fn signals_interrupt_query_time_javascript_on_every_cli_route() {
    let score = scratch("signal-query-turn-runaway.strudel");
    let render = scratch("signal-query-turn-runaway-render.json");
    let export = scratch("signal-query-turn-runaway-export.wav");
    std::fs::write(&score, QUERY_TIME_RUNAWAY).expect("write signalled query-time score");

    let cases = query_time_cli_cases(&score, &render, &export);
    for (signal, expected_code) in [(libc::SIGINT, 130), (libc::SIGTERM, 143)] {
        let _ = std::fs::remove_file(&render);
        let _ = std::fs::remove_file(&export);
        let mut batch = CliBatch::spawn(&cases);

        // Score construction is finite. This delay lets every route cross that
        // phase and enter Pattern.query, while remaining far below the fixed
        // two-second query deadline.
        std::thread::sleep(std::time::Duration::from_millis(500));
        batch.signal_all(signal);
        let started = std::time::Instant::now();
        let outputs = batch.collect(std::time::Duration::from_secs(1));
        let elapsed = started.elapsed();

        for (args, output) in cases.iter().zip(outputs) {
            assert_eq!(
                output.status.code(),
                Some(expected_code),
                "{args:?}: signal {signal} produced {:?}, not the reaped shell-convention exit {expected_code}\nstderr: {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "query-time callbacks waited {elapsed:?} after signal {signal}; cancellation must beat the two-second deadline"
        );
        // Stop during scheduling retains play/render's established partial-run
        // behavior; this assertion covers the exit outcome, not transactional
        // deletion of an output that was already opened.
        let _ = std::fs::remove_file(&render);
        let _ = std::fs::remove_file(&export);
    }

    let _ = std::fs::remove_file(score);
}

#[test]
fn musician_prebake_job_budget_is_a_typed_resource_refusal() {
    let prebake = scratch("musician-prebake-job-budget.js");
    let score = scratch("musician-prebake-job-budget.strudel");
    std::fs::write(
        &prebake,
        format!(
            "for (let i = 0; i < {}; i++) queueMicrotask(() => {{}});",
            rustel_jsruntime::MAX_PREBAKE_JOBS + 1
        ),
    )
    .expect("write over-budget setup");
    std::fs::write(&score, "note('c4')").expect("write score");
    let result = run(&[
        "play",
        score.to_str().expect("UTF-8 score"),
        "-vvv",
        "--prebake",
        prebake.to_str().expect("UTF-8 setup"),
    ]);
    assert_eq!(result.status.code(), Some(3));
    let error: serde_json::Value =
        serde_json::from_slice(&result.stderr).expect("resource-limit envelope");
    assert_eq!(error["error"]["kind"], "resource-limit");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("runnable jobs"),
        "wrong job-budget error: {error}"
    );
    let _ = std::fs::remove_file(prebake);
    let _ = std::fs::remove_file(score);
}

#[test]
fn musician_prebake_file_limit_accepts_exactly_four_mib_and_refuses_one_more_byte() {
    const LIMIT: usize = 4 * 1024 * 1024;
    let prebake = scratch("musician-prebake-exact-limit.js");
    let oversized = scratch("musician-prebake-over-limit.js");
    let score = scratch("musician-prebake-limit-score.strudel");
    let output = scratch("musician-prebake-limit.wav");
    std::fs::write(&prebake, vec![b' '; LIMIT]).expect("write exact-limit setup");
    let oversized_file = std::fs::File::create(&oversized).expect("create oversized setup");
    oversized_file
        .set_len((LIMIT + 1) as u64)
        .expect("size oversized setup");
    std::fs::write(&score, "note('c4')").expect("write score");

    let exact = run(&[
        "export",
        score.to_str().expect("UTF-8 score"),
        "--json",
        "--prebake",
        prebake.to_str().expect("UTF-8 setup"),
        "-o",
        output.to_str().expect("UTF-8 output"),
        "--duration",
        "0",
    ]);
    assert!(
        exact.status.success(),
        "the exact byte limit was rejected: {}",
        String::from_utf8_lossy(&exact.stderr)
    );

    let over = run(&[
        "export",
        score.to_str().expect("UTF-8 score"),
        "--json",
        "--prebake",
        oversized.to_str().expect("UTF-8 oversized setup"),
        "-o",
        output.to_str().expect("UTF-8 output"),
    ]);
    assert_eq!(over.status.code(), Some(3));
    let error: serde_json::Value =
        serde_json::from_slice(&over.stderr).expect("resource-limit envelope");
    assert_eq!(error["error"]["kind"], "resource-limit");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("4194304 byte limit")
    );

    // A regular file is rejected by metadata before reading, so it cannot
    // prove the MAX+1 streaming guard. Stdin has no trusted length: exercise
    // both sides through the actual bounded reader so deleting `take(MAX+1)`
    // or the post-read check is a killed mutation.
    let stdin_run = |size: usize| {
        use std::io::Write;
        let args = [
            "export",
            "-",
            "--json",
            "-o",
            output.to_str().expect("UTF-8 output"),
            "--duration",
            "0",
        ];
        let mut child = rustel()
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn stdin musician");
        let mut source = vec![b' '; size];
        source[..10].copy_from_slice(b"note('c4')");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(&source)
            .expect("write bounded source");
        wait_for_output(child, &args)
    };
    let stdin_exact = stdin_run(LIMIT);
    assert!(
        stdin_exact.status.success(),
        "streaming exact limit failed: {}",
        String::from_utf8_lossy(&stdin_exact.stderr)
    );
    let stdin_over = stdin_run(LIMIT + 1);
    assert_eq!(stdin_over.status.code(), Some(3));
    let stdin_error: serde_json::Value =
        serde_json::from_slice(&stdin_over.stderr).expect("stdin resource-limit envelope");
    assert_eq!(stdin_error["error"]["kind"], "resource-limit");

    for path in [prebake, oversized, score, output] {
        let _ = std::fs::remove_file(path);
    }
}

#[test]
#[cfg(not(feature = "device-audio"))]
fn musician_prebake_is_loaded_before_plain_and_watched_device_routing() {
    let prebake = scratch("musician-live-prebake.js");
    let score = scratch("musician-live-prebake.strudel");
    std::fs::write(&prebake, "globalThis.liveHelper = () => note('c4');").expect("write prebake");
    std::fs::write(&score, "liveHelper()").expect("write score");
    for watch in [false, true] {
        let mut args = vec![
            "play",
            score.to_str().expect("UTF-8 score"),
            "--prebake",
            prebake.to_str().expect("UTF-8 prebake"),
        ];
        if watch {
            args.push("--watch");
        }
        let result = run(&args);
        assert_eq!(result.status.code(), Some(5), "{args:?}");
        assert!(
            structured_lines(&result.stderr)
                .iter()
                .any(|line| line["error"]["kind"] == "audio"),
            "{args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let _ = std::fs::remove_file(prebake);
    let _ = std::fs::remove_file(score);
}

#[test]
#[cfg(not(feature = "device-audio"))]
fn musician_plain_and_watch_do_not_auto_discover_adjacent_prebake() {
    let directory = scratch("musician-live-no-prebake-discovery");
    std::fs::create_dir_all(&directory).expect("create isolated score directory");
    let score = directory.join("song.strudel");
    std::fs::write(&score, "note('c4')").expect("write score");
    std::fs::write(
        directory.join("prebake.js"),
        "throw new Error('adjacent setup must not run')",
    )
    .expect("write adjacent setup");
    for watch in [false, true] {
        let mut args = vec!["play", score.to_str().expect("UTF-8 score")];
        if watch {
            args.push("--watch");
        }
        let result = run(&args);
        assert_eq!(result.status.code(), Some(5), "{args:?}");
        assert!(
            structured_lines(&result.stderr)
                .iter()
                .any(|line| line["error"]["kind"] == "audio"),
            "{args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            !String::from_utf8_lossy(&result.stderr).contains("adjacent setup must not run"),
            "{args:?} auto-discovered an adjacent prebake"
        );
    }
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
#[cfg(not(feature = "device-audio"))]
fn musician_play_and_watch_reach_the_live_product_route_without_a_subcommand() {
    let score = scratch("musician-live-unavailable.strudel");
    std::fs::write(&score, r#"note("c4")"#).expect("write score");
    for suffix in [Vec::<&str>::new(), vec!["--watch"]] {
        let mut args = vec!["play", score.to_str().expect("UTF-8 score")];
        args.extend(suffix);
        let output = run(&args);
        assert_eq!(output.status.code(), Some(5), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        let error = structured_lines(&output.stderr)
            .into_iter()
            .find(|line| line["error"]["kind"] == "audio")
            .unwrap_or_else(|| panic!("{args:?}: {}", String::from_utf8_lossy(&output.stderr)));
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("--features device-audio"),
            "{args:?}: {error}"
        );
    }
    let _ = std::fs::remove_file(score);
}

#[test]
fn unwatched_musician_refuses_an_unplayable_score_before_opening_audio() {
    // These cases must terminate before CPAL is touched. With device audio,
    // crossing that boundary would leave a silent process alive until Ctrl-C;
    // without it, the wrong result is the later `audio` error instead.
    for (name, source, extra, expected_kind) in [
        ("empty", "", None, "no-pattern"),
        ("whitespace", "  \n\t", None, "no-pattern"),
        ("undefined", "void 0", None, "no-pattern"),
        ("commented", "// $: s('bd')", None, "no-pattern"),
        ("malformed", "note(", None, "evaluation"),
    ] {
        let score = scratch(&format!("musician-unplayable-{name}.strudel"));
        std::fs::write(&score, source).expect("write unplayable score");
        let path = score.to_str().expect("UTF-8 score");
        // -vvv asks for the structured stderr this test reads.
        let args = match extra {
            Some(flag) => vec!["play", path, "-vvv", flag],
            None => vec!["play", path, "-vvv"],
        };
        let started = std::time::Instant::now();
        let output = run(&args);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "{name} score did not fail promptly"
        );
        assert_eq!(output.status.code(), Some(1), "{name} score");
        assert!(output.stdout.is_empty(), "{name} score emitted stdout");
        let error: serde_json::Value =
            serde_json::from_slice(&output.stderr).expect("structured startup error");
        assert_eq!(
            error["error"]["kind"], expected_kind,
            "{name} score crossed into audio startup: {error}"
        );
        let _ = std::fs::remove_file(score);
    }
}

#[test]
#[cfg(not(feature = "device-audio"))]
fn playable_and_explicitly_watched_scores_still_reach_audio_startup() {
    // Explicit `silence` is a real Pattern, unlike the synthetic silence that
    // represents an undefined score. Explicit watch mode also intentionally
    // waits for the next save when its initial file is empty or malformed.
    for (name, source, extra) in [
        ("notes", r#"note("c4")"#, None),
        ("silence", "silence", None),
        ("watched-empty", "", Some("--watch")),
        ("watched-malformed", "note(", Some("--watch")),
    ] {
        let score = scratch(&format!("musician-audio-control-{name}.strudel"));
        std::fs::write(&score, source).expect("write score");
        let path = score.to_str().expect("UTF-8 score");
        // -vvv asks for the structured stderr this test reads.
        let args = match extra {
            Some(flag) => vec!["play", path, "-vvv", flag],
            None => vec!["play", path, "-vvv"],
        };
        let output = run(&args);
        assert_eq!(
            output.status.code(),
            Some(5),
            "{name} score did not reach audio startup: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            structured_lines(&output.stderr)
                .iter()
                .any(|line| line["error"]["kind"] == "audio"),
            "{name} control failed before audio startup: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let _ = std::fs::remove_file(score);
    }
}

#[test]
fn ambiguous_play_and_export_shapes_are_usage_errors() {
    let score = scratch("musician-conflicts.strudel");
    let output = scratch("musician-conflicts.wav");
    std::fs::write(&score, r#"note("c4")"#).expect("write score");
    let score = score.to_str().expect("UTF-8 score");
    let output = output.to_str().expect("UTF-8 output");
    for args in [
        vec![
            "export",
            score,
            "-o",
            output,
            "--cycles",
            "2",
            "--duration",
            "1",
        ],
        vec!["play", score, "query", "--json", "-e", "pure(1)"],
    ] {
        let result = run(&args);
        assert_eq!(
            result.status.code(),
            Some(2),
            "ambiguous invocation was not a usage error: {args:?}\n{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let _ = std::fs::remove_file(score);
}

#[test]
fn export_preserves_structured_failure_exit_classes() {
    let assert_error_kind = |stderr: &[u8], kind: &str| {
        let lines = structured_lines(stderr);
        let text = String::from_utf8_lossy(stderr);
        assert_eq!(
            lines.len(),
            text.lines().filter(|line| !line.trim().is_empty()).count(),
            "stderr must contain only JSON lines: {text}"
        );
        let errors: Vec<_> = lines.iter().filter_map(|line| line.get("error")).collect();
        assert_eq!(errors.len(), 1, "one terminal error is reported: {text}");
        assert_eq!(errors[0]["kind"], kind, "{text}");
    };
    let missing_output = scratch("missing-parent").join("out.wav");
    let missing_output = missing_output.to_str().expect("UTF-8 output");
    let unused_output = scratch("unused-musician.wav");
    let unused_output_text = unused_output.to_str().expect("UTF-8 output");
    let score = scratch("musician-failures.strudel");
    std::fs::write(&score, "note(").expect("write malformed score");
    let score_text = score.to_str().expect("UTF-8 score");

    let malformed = run(&["export", score_text, "--json", "-o", unused_output_text]);
    assert_eq!(malformed.status.code(), Some(1));
    assert_error_kind(&malformed.stderr, "evaluation");

    std::fs::write(&score, r#"note("c4")"#).expect("write valid score");
    let io = run(&["export", score_text, "--json", "-o", missing_output]);
    assert_eq!(io.status.code(), Some(4));
    assert_error_kind(&io.stderr, "io");

    let limit = run(&[
        "export",
        score_text,
        "--json",
        "-o",
        unused_output_text,
        "--duration",
        "86401",
    ]);
    assert_eq!(limit.status.code(), Some(3));
    assert_error_kind(&limit.stderr, "resource-limit");

    let _ = std::fs::remove_file(score);
    let _ = std::fs::remove_file(unused_output);
}

// -- bench ------------------------------------------------------------------

#[test]
fn bench_emits_metrics_json() {
    let out = ok_stdout(&[
        "bench",
        "--json",
        "-e",
        r#"s("bd sd")"#,
        "--iterations",
        "5",
    ]);
    let json: serde_json::Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("bench stdout is not JSON: {e}\n{out}"));
    assert!(json.is_object(), "bench must emit a metrics object: {json}");
}

#[test]
fn bench_does_not_panic_for_host_required_sources() {
    for source in HOST_REQUIRED_SOURCES {
        let out = run(&["bench", "--json", "-e", source, "--iterations", "3"]);
        assert_ne!(
            out.status.code(),
            Some(101),
            "{source}: `bench` panicked\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

// -- failure modes ----------------------------------------------------------

#[test]
fn malformed_source_is_reported_not_crashed() {
    // A syntax error is the single most common thing a live-coder types.
    assert_reported_failure(&["query", "--json", "-e", "s(\"bd\""], "error");
    assert_reported_failure(&["query", "--json", "-e", "))))"], "error");
    assert_reported_failure(
        &["trace", "--json", "-e", "s(\"bd\"", "--duration", "1"],
        "error",
    );
}

#[test]
fn a_thrown_error_reports_its_own_message_not_the_object_it_became() {
    // Double-quoted strings compile to mini-notation. The report must still
    // show the text passed to `new Error("boom")`, not an object's default
    // text.
    assert_reported_failure(&["query", "-e", "throw new Error(\"boom\")"], "boom");
    let out = run(&["query", "-e", "throw new Error(\"boom\")"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("[object Object]"),
        "the message must name the word thrown, not the object it became: {stderr}"
    );

    // AggregateError carries its message after the errors iterable, and the
    // same words in backticks are an ordinary string: neither shape may
    // fall back to the object's default text either.
    assert_reported_failure(
        &[
            "query",
            "-e",
            "throw new AggregateError([new Error(\"inner\")], \"many\")",
        ],
        "AggregateError: many",
    );
    assert_reported_failure(
        &["query", "-e", "throw new Error(`backticked`)"],
        "backticked",
    );
}

#[test]
fn a_missing_input_file_is_reported_not_crashed() {
    assert_reported_failure(&["query", "--json", "/nonexistent/rustel-source.js"], "");
}

#[test]
fn non_finite_and_negative_durations_and_rates_are_rejected() {
    // `--duration inf` must fail. Without the check it runs forever with no
    // output, and a hang looks the same as slow work.
    let render_output = scratch("non-finite.wav");
    let render_output = render_output.to_str().expect("UTF-8 output");
    for args in [
        vec!["trace", "--json", "-e", r#"s("bd")"#, "--duration", "inf"],
        vec!["trace", "--json", "-e", r#"s("bd")"#, "--duration", "NaN"],
        vec!["trace", "--json", "-e", r#"s("bd")"#, "--duration", "-1"],
        vec!["trace", "--json", "-e", r#"s("bd")"#, "--cps", "0"],
        vec!["trace", "--json", "-e", r#"s("bd")"#, "--cps", "NaN"],
        vec!["trace", "--json", "-e", r#"s("bd")"#, "--cps", "inf"],
        vec![
            "render",
            "-e",
            r#"s("bd")"#,
            "--duration",
            "inf",
            "-o",
            render_output,
        ],
    ] {
        assert_reported_failure(&args, "");
    }
    let _ = std::fs::remove_file(render_output);
}

#[test]
fn an_enormous_but_finite_duration_is_refused_not_run() {
    // The duration and onset bounds cover different inputs: one limits a long
    // window, while the other limits dense event streams within a legal
    // window.
    let render_output = scratch("enormous-duration.json");
    let render_output = render_output.to_str().expect("UTF-8 output");
    for args in [
        vec!["trace", "--json", "-e", r#"s("bd")"#, "--duration", "1e300"],
        vec!["trace", "--json", "-e", r#"s("bd")"#, "--duration", "1e12"],
        vec!["trace", "--json", "-e", r#"s("bd")"#, "--duration", "86401"],
        vec![
            "render",
            "-e",
            r#"s("bd")"#,
            "--duration",
            "1e300",
            "-o",
            render_output,
            "--format",
            "onset-json",
        ],
    ] {
        assert_reported_failure(&args, "duration");
    }
    let _ = std::fs::remove_file(render_output);
}

/// `gamepad-monitor --duration` builds a `Duration` straight from the number
/// it was given, and `from_secs_f64` panics on a finite value too large to
/// hold. The same bound every other window goes through refuses it instead.
///
/// A negative window is not here: clap rejects `-1` as an argument before the
/// handler sees it, which is its own reported failure and not this one.
#[test]
#[cfg(feature = "gamepad")]
fn an_enormous_gamepad_monitor_window_is_refused_not_run() {
    for value in ["1e300", "1e12", "86401"] {
        assert_reported_failure(&["gamepad-monitor", "--duration", value], "duration");
    }
}

/// `midi-monitor --duration` refuses `inf`, NaN, a negative value and a value
/// past the duration bound before it opens a device. `from_secs_f64` panics
/// on a value too large to hold.
#[test]
#[cfg(feature = "midi")]
fn an_enormous_midi_monitor_window_is_refused_not_run() {
    for value in ["inf", "1e300", "1e12", "86401", "NaN"] {
        assert_reported_failure(&["midi-monitor", "--duration", value], "duration");
    }
    assert_reported_failure(&["midi-monitor", "--duration=-1"], "duration");
}

/// Live replay refuses a save delivery wait past the duration bound, from a
/// too-slow `--speed` or an enormous tape `t`, with a diagnostic instead of a
/// panic on its delivery thread. `--export` ignores `--speed` and stays open.
#[test]
fn an_impossibly_slow_replay_is_refused_not_panicked_in_delivery() {
    let quiet = r#"note("c4").gain(0.2)"#;
    // Both speeds pass the finite-and-positive check: the first quotient is
    // past the bound, the second overflows to inf.
    let tape = installed_tape(
        "replay-slow-speed.rustel-session",
        &[(0.0, quiet), (2.0, quiet)],
    );
    for speed in ["1e-300", "1e-320"] {
        assert_reported_failure(
            &[
                "replay",
                tape.to_str().expect("UTF-8 tape"),
                "--speed",
                speed,
            ],
            "--speed",
        );
    }
    // A save 1e20 seconds in, at the default speed.
    let corrupt = installed_tape(
        "replay-huge-t.rustel-session",
        &[(0.0, quiet), (1e20, quiet)],
    );
    assert_reported_failure(
        &["replay", corrupt.to_str().expect("UTF-8 tape")],
        "24 hours",
    );
    let bounce = scratch("replay-huge-t.wav");
    ok_output(&[
        "replay",
        corrupt.to_str().expect("UTF-8 tape"),
        "--speed",
        "1e-300",
        "--export",
        bounce.to_str().expect("UTF-8 bounce"),
        "--duration",
        "0.5",
    ]);
    for path in [tape, corrupt, bounce] {
        let _ = std::fs::remove_file(path);
    }
}

#[test]
fn an_unbounded_onset_count_is_refused_not_accumulated() {
    // The density half. `play` holds the whole timeline before emitting it, so
    // the onset COUNT is the allocation input; a duration cap alone does not
    // bound it.
    assert_reported_failure(
        &[
            "trace",
            "--json",
            "-e",
            r#"s("bd*16 sd*16")"#,
            "--duration",
            "86400",
        ],
        "onsets",
    );
}

#[test]
fn a_throwing_bind_callback_reports_empty_haps_and_a_failed_exit() {
    let out = run(&[
        "query",
        "--json",
        "-e",
        r#"s("bd").polyBind(x => { throw new Error('boom'); })"#,
    ]);
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("query report");
    assert_eq!(json["haps"].as_array().map(Vec::len), Some(0), "{json}");
    assert!(
        json["query_threw"]
            .as_str()
            .is_some_and(|message| message.contains("boom")),
        "{json}"
    );
    let error: serde_json::Value = serde_json::from_slice(&out.stderr).expect("query error");
    assert_eq!(out.status.code(), Some(1), "{error}");
    assert!(
        error["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("boom")),
        "{error}"
    );
}

#[test]
fn invalid_numeric_arguments_are_reported_not_crashed() {
    // Fractions, durations and iteration counts all come straight from the
    // command line, so each is an untrusted parse.
    assert_reported_failure(
        &[
            "query",
            "--json",
            "-e",
            "s(\"bd\")",
            "--begin",
            "not-a-number",
        ],
        "",
    );
    assert_reported_failure(&["query", "--json", "-e", "s(\"bd\")", "--end", "1/0"], "");
    // The i128 edge: no exact fraction at all, and an exact one the first
    // `fast` of the query would overflow. Both used to exit 101.
    assert_reported_failure(
        &[
            "query",
            "--json",
            "-e",
            "note(\"c d\")",
            "--end=1/-170141183460469231731687303715884105728",
        ],
        "out of range",
    );
    assert_reported_failure(
        &[
            "query",
            "--json",
            "-e",
            "note(\"c d\")",
            "--begin=-170141183460469231731687303715884105728/1",
            "--end=-170141183460469231731687303715884105727/1",
        ],
        "out of range",
    );
    assert_reported_failure(
        &["trace", "--json", "-e", "s(\"bd\")", "--duration", "banana"],
        "",
    );
    assert_reported_failure(
        &["bench", "--json", "-e", "s(\"bd\")", "--iterations", "-4"],
        "",
    );
}

#[test]
fn an_unwritable_output_path_is_reported_not_crashed() {
    assert_reported_failure(
        &[
            "render",
            "-e",
            r#"s("bd")"#,
            "--duration",
            "1",
            "-o",
            "/nonexistent-directory/out.wav",
        ],
        "",
    );
}

#[test]
fn no_input_at_all_is_reported_not_crashed() {
    assert_reported_failure(&["query"], "");
}

#[test]
fn a_callback_that_throws_is_reported_or_empty_never_a_crash() {
    // Upstream turns a throwing callback into an empty `queryArc`, so an empty
    // result is correct here. What must not happen is a panic. The `query`
    // form has its own test with an exact exit status.
    let command = [
        "trace",
        "--json",
        "-e",
        r#"s("bd").polyBind(x => { throw new Error('boom'); })"#,
        "--duration",
        "1",
    ];
    let out = run(&command);
    assert_ne!(
        out.status.code(),
        Some(101),
        "{command:?} panicked on a throwing callback\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[cfg(unix)]
#[test]
fn a_self_reentrant_callback_reports_a_query_error_without_aborting() {
    // Callback re-entry must reach the query error boundary before it exhausts
    // the native stack. The CLI reports the error and discards partial haps.
    for source in [
        r#"
          let f;
          f = haps => note("[c,e]").arpWith(f);
          note("[c,e]").arpWith(f)
        "#,
        // Each recursive level also does native pattern work.
        r#"
          let f;
          f = haps => note("[c,e]")
            .every(fastcat(2, 3), p => p.rev())
            .arpWith(f);
          note("[c,e]").arpWith(f)
        "#,
    ] {
        let args = ["query", "--json", "-e", source, "--end", "1/64"];
        // Check the report even when the platform cannot lower the child stack.
        // Where supported, the same checks also cover a 1 MiB stack.
        for out in std::iter::once(run(&args)).chain(run_with_stack_limit(&args, 1024 * 1024)) {
            assert_eq!(
                out.status.code(),
                Some(1),
                "recursive query must report a failed exit without aborting: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let json: serde_json::Value =
                serde_json::from_slice(&out.stdout).expect("recursive-query report");
            assert_eq!(json["haps"].as_array().map(Vec::len), Some(0), "{json}");
            let message = "Maximum call stack size exceeded";
            assert_eq!(json["query_threw"], message, "{json}");
            let error: serde_json::Value =
                serde_json::from_slice(&out.stderr).expect("recursive-query error");
            assert_eq!(error["error"]["kind"], "invalid-argument", "{error}");
            assert_eq!(
                error["error"]["message"],
                format!("the pattern threw while querying: {message}"),
                "{error}"
            );
        }
    }
}

#[test]
fn callback_recursion_bound_does_not_reject_deep_native_graphs() {
    // The guard owns callback RE-ENTRY, not generic Pattern::query depth. A
    // first attempt guarded every native node and made a sufficiently composed
    // pure graph silently disappear even though it never crossed into JS.
    let source = r#"
      let p = s("bd");
      for (let i = 0; i < 64; i++) p = p.fast(2).slow(2);
      p
    "#;
    let out = ok_stdout(&["query", "--json", "-e", source, "--end", "1/64"]);
    let json: serde_json::Value = serde_json::from_str(&out).expect("deep-query JSON");
    assert_eq!(
        json["haps"].as_array().expect("haps").len(),
        1,
        "ordinary native pattern depth must not spend the callback-reentry budget"
    );
}

#[test]
fn weighted_choice_evaluator_does_not_inflate_unrelated_query_frames() {
    // The first WChoose implementation lived directly in query_node's match
    // arm. Its nested-loop locals enlarged EVERY recursive native query frame,
    // and the 64-layer fast/slow control above stack-aborted without containing
    // weighted choice at all. This direct control also pins a musically
    // unreasonable but harmless static weighted nesting at the same depth.
    let source = r#"
      let p = pure('x');
      for (let i = 0; i < 64; i++) p = wchooseCycles([p, 1]);
      p
    "#;
    let out = ok_stdout(&["query", "--json", "-e", source, "--end", "1/64"]);
    let json: serde_json::Value = serde_json::from_str(&out).expect("weighted-depth JSON");
    assert_eq!(
        json["haps"].as_array().expect("haps").len(),
        1,
        "modest static weighted nesting must not abort or silently disappear"
    );
}

#[test]
fn callback_recursion_bound_does_not_reject_static_arp_with_layers() {
    // Receiver composition is not callback RE-ENTRY: each inner arpWith
    // finishes before the outer callback-returned pattern is queried. Keeping
    // the guard over the whole node made the 17th ordinary layer disappear.
    //
    // Three depths, because a layer costs native traversal frames as well as
    // composition, and the two bounds fail differently: 20 is where the old
    // 1 MiB QuickJS budget ran out, 64 is four times the re-entry bound, and
    // 32 sits between them so a regression in either says which one moved.
    for depth in [20, 32, 64] {
        let source = format!(
            r#"
      let p = note("[c,e]");
      for (let i = 0; i < {depth}; i++) p = p.arpWith(haps => haps[0]);
      p
    "#
        );
        let out = ok_stdout(&["query", "--json", "-e", &source, "--end", "1/64"]);
        let json: serde_json::Value = serde_json::from_str(&out).expect("layered-query JSON");
        assert_eq!(
            json["haps"].as_array().expect("haps").len(),
            1,
            "{depth} static arpWith layers must not spend callback-reentry depth"
        );
    }
}

#[test]
fn nested_callback_lane_produces_events_in_every_build_profile() {
    // 44 events at every optimisation level. This is an ordinary written
    // score, not a constructed one. The lane, the nested mini pattern and the
    // two callbacks each add native frames below the query boundary, and
    // QuickJS charges every such frame to its stack budget.
    let source = r#"$: s("mt hh sd [~ lt lt*4 lt cp@2 bd] rim").rarely((x => x.rev())).brak().swing(4).sometimes((x => x.degrade())).attack(0.019)"#;
    let out = ok_stdout(&["query", "--json", "-e", source, "--end", "4"]);
    let json: serde_json::Value = serde_json::from_str(&out).expect("nested-query JSON");
    assert_eq!(json["haps"].as_array().expect("haps").len(), 44);
}

#[test]
fn a_score_that_queries_its_own_pattern_keeps_its_events_at_depth() {
    // The pattern surface's own `query` walks the same graph, but from inside
    // a JavaScript frame rather than beneath the outer query boundary. Its
    // native frames are charged the same way, so the budget has to cover this
    // entry too. The shallow depth fixes the count the deep one must match.
    let source = |depth: usize| {
        format!(
            r#"
      let p = note("[c,e]");
      for (let i = 0; i < {depth}; i++) p = p.arpWith(haps => haps[0]);
      note(String(p.query({{ span: {{ begin: 0, end: 1 }} }}).length))
    "#
        )
    };
    for depth in [4, 32] {
        let out = ok_stdout(&["query", "--json", "-e", &source(depth), "--end", "1/64"]);
        let json: serde_json::Value = serde_json::from_str(&out).expect("self-query JSON");
        let haps = json["haps"].as_array().expect("haps");
        assert_eq!(
            haps.len(),
            1,
            "{depth} layers: the score itself produced no event"
        );
        assert_eq!(
            haps[0]["value"]["note"], "1",
            "{depth} layers: the score's own query returned nothing"
        );
    }
}

#[test]
fn a_callback_under_a_deep_native_graph_is_still_called() {
    // One callback, called once, so it spends no re-entry, and 129 frames is
    // inside the graph bound. The native `query_node` frames below the query
    // boundary must not exhaust the QuickJS stack budget before the callback
    // runs.
    let source = r#"
      let p = note("c").withValue(v => v);
      for (let i = 0; i < 64; i++) p = p.fast(2).slow(2);
      p
    "#;
    let out = ok_stdout(&["query", "--json", "-e", source, "--end", "1/64"]);
    let json: serde_json::Value = serde_json::from_str(&out).expect("deep-callback JSON");
    assert_eq!(
        json["haps"].as_array().expect("haps").len(),
        1,
        "a callback under an ordinary deep graph must still be called"
    );
}

// -- independence -----------------------------------------------------------

#[test]
fn the_binary_needs_no_node_and_no_server() {
    // The product claim is a single self-contained process. Running with an
    // empty PATH removes any chance of shelling out to `node`; if the binary
    // still works, nothing on the query path depends on it.
    //
    // This checks only that the shipped binary has no runtime dependency on an
    // external JavaScript executable.
    let args = ["query", "--json", "-e", r#"s("bd sd")"#];
    let child = rustel()
        .args(args)
        .env("PATH", "")
        .env_remove("NODE")
        .env_remove("RUSTEL_NODE")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn rustel");
    let out = wait_for_output(child, &args);
    assert!(
        out.status.success(),
        "the binary failed with an empty PATH, so it depends on an external \
         tool at runtime:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(json["haps"].as_array().expect("haps").len(), 2);
}

#[test]
fn play_terminates_on_its_own_within_the_requested_duration() {
    // The portable half of cancellation: a bounded `play` must END. A run that
    // never returns would hang CI rather than fail it, so this is asserted with
    // a wall-clock bound well above the 2-cycle workload.
    let started = std::time::Instant::now();
    let out = run(&["trace", "--json", "-e", r#"s("bd sd")"#, "--duration", "2"]);
    assert!(out.status.success());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(60),
        "`play --duration 2` took {:?}; it schedules against a VirtualClock and \
         must not run in real time",
        started.elapsed()
    );
}

#[test]
fn an_unrepresentable_wav_render_is_refused_not_allocated() {
    // `write_silent_wav` computed its sizes with `saturating_mul`, so an
    // impossible request produced `u32::MAX` and then asked for a single ~4 GiB
    // zero buffer. Saturation is the wrong answer to "this number is
    // impossible": it yields a plausible header describing a file nobody asked
    // for, after taking the memory.
    let out = scratch("huge.wav");
    assert_reported_failure(
        &[
            "render",
            "-e",
            r#"s("bd")"#,
            "--duration",
            "86400",
            "--sample-rate",
            "4000000000",
            "-o",
            out.to_str().expect("utf-8"),
            "--format",
            "wav",
        ],
        "",
    );
    let _ = std::fs::remove_file(&out);
}

#[test]
#[cfg(not(feature = "mp3-export"))]
fn an_mp3_render_requires_the_encoder_feature() {
    let out = scratch("unsupported.mp3");
    assert_reported_failure(
        &[
            "render",
            "-e",
            "s('sine')",
            "--duration",
            "0.125",
            "-o",
            out.to_str().expect("utf-8"),
            "--format",
            "mp3",
        ],
        "MP3 export requires the `mp3-export` feature",
    );
    assert!(
        !out.exists(),
        "unsupported MP3 render must not write output"
    );
}

#[test]
#[cfg(feature = "mp3-export")]
fn an_over_budget_mp3_render_is_refused_not_allocated() {
    // An MP3 bounce holds its whole render body in memory, and the encoder
    // copies it again. The WAV writer streams one block at a time. The MP3
    // route must apply a size budget and refuse this request.
    let out = scratch("huge.mp3");
    assert_reported_failure(
        &[
            "render",
            "-e",
            r#"s("bd")"#,
            "--duration",
            "86400",
            "--sample-rate",
            "4294967295",
            "-o",
            out.to_str().expect("utf-8"),
            "--format",
            "mp3",
        ],
        "which an in-memory render cannot hold",
    );
    let _ = std::fs::remove_file(&out);
}

#[test]
fn an_ordinary_wav_render_still_has_a_correct_header() {
    // The complement: the streaming writer must still produce a valid file with
    // the right declared sizes, not merely avoid allocating.
    let out = scratch("ok.wav");
    let _ = ok_stdout(&[
        "render",
        "-e",
        r#"s("bd sd")"#,
        "--duration",
        "2",
        "-o",
        out.to_str().expect("utf-8"),
        "--format",
        "wav",
    ]);
    let bytes = std::fs::read(&out).expect("read wav");
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    // The declared RIFF size must match what was actually written.
    let riff_size = u32::from_le_bytes(bytes[4..8].try_into().expect("4 bytes"));
    assert_eq!(
        riff_size as usize + 8,
        bytes.len(),
        "the RIFF header declares a size the file does not have"
    );
    let data_size = u32::from_le_bytes(bytes[40..44].try_into().expect("4 bytes"));
    assert_eq!(
        data_size as usize,
        bytes.len() - 44,
        "the data chunk size disagrees with the PCM actually written"
    );
    let _ = std::fs::remove_file(&out);
}

#[test]
fn query_reads_as_events_by_default_and_as_json_when_asked() {
    // The JSON is what a machine reads and is unchanged; this is the same
    // report for the person who ran the command, who should not have to count
    // braces to see whether the notes land where they meant them to.
    let readable = ok_stdout(&["query", "-e", r#"s("bd hh").note("c e")"#, "--end", "1/1"]);
    assert!(
        serde_json::from_str::<serde_json::Value>(&readable).is_err(),
        "the default view is for a person to read, not to parse: {readable}"
    );
    let lines = readable.lines().collect::<Vec<_>>();
    assert_eq!(
        lines.first().copied(),
        Some(r#"s("bd hh").note("c e")"#),
        "the source it queried heads the list: {readable}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("0/1 → 1/2") && line.contains("s:bd")),
        "the first event and the span it sounds over: {readable}"
    );
    assert!(
        lines.last().is_some_and(|line| line.contains("2 events")),
        "the count closes the list: {readable}"
    );

    let json = ok_stdout(&[
        "query",
        "--json",
        "-e",
        r#"s("bd hh").note("c e")"#,
        "--end",
        "1/1",
    ]);
    let report: serde_json::Value = serde_json::from_str(&json).expect("query JSON");
    assert_eq!(
        report["haps"].as_array().expect("haps").len(),
        2,
        "--json is the report a script already parses, unchanged"
    );
}

#[cfg(feature = "hydra")]
#[test]
fn query_discloses_unrendered_hydra_visuals_without_changing_its_event_report() {
    let source = "await initHydra()\nosc(10).out()\n$: s(\"bd\")";
    let readable = ok_output(&["query", "-e", source]);
    let stdout = String::from_utf8_lossy(&readable.stdout);
    let stderr = String::from_utf8_lossy(&readable.stderr);
    assert!(stdout.contains("1 event"), "event report: {stdout}");
    assert!(
        stderr.contains("does not render or verify this score's Hydra visuals"),
        "visuals notice: {stderr}"
    );

    let json = ok_output(&["query", "--json", "-e", source]);
    let report: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("query report remains JSON");
    assert_eq!(report["haps"].as_array().expect("haps").len(), 1);
    let notice: serde_json::Value =
        serde_json::from_slice(&json.stderr).expect("visuals notice is JSON on stderr");
    assert_eq!(notice["hydra"]["status"], "not-rendered");

    let quiet = ok_output(&["query", "--quiet", "-e", source]);
    assert!(quiet.stderr.is_empty(), "quiet suppresses notices");

    let audio_only = ok_output(&["query", "-e", "s(\"bd\")"]);
    assert!(
        audio_only.stderr.is_empty(),
        "audio-only query needs no notice"
    );
}

// -- structured errors ------------------------------------------------------

/// Parse the JSON error envelope from stderr, asserting it is well-formed.
fn error_envelope(args: &[&str]) -> (serde_json::Value, Option<i32>) {
    let out = run(args);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let line = stderr
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or_else(|| panic!("{args:?} printed no JSON error envelope:\n{stderr}"));
    let json: serde_json::Value = serde_json::from_str(line)
        .unwrap_or_else(|e| panic!("{args:?} error envelope is not JSON: {e}\n{line}"));
    (json, out.status.code())
}

#[test]
fn every_command_reports_failures_in_the_same_structured_shape() {
    // Every command emits the same error envelope on stderr. Stdout carries
    // only the command's own JSON, so each stream parses independently.
    let render_output = scratch("structured-error.json");
    let render_output = render_output.to_str().expect("UTF-8 output");
    for (args, kind, code) in [
        (vec!["query", "--json", "-e", "s(\"bd\""], "evaluation", 1),
        (
            vec!["trace", "--json", "-e", "s(\"bd\"", "--duration", "1"],
            "evaluation",
            1,
        ),
        (
            vec![
                "render",
                "--json",
                "-e",
                "s(\"bd\"",
                "--duration",
                "1",
                "-o",
                render_output,
                "--format",
                "onset-json",
            ],
            "evaluation",
            1,
        ),
        (
            vec!["bench", "--json", "-e", "s(\"bd\"", "--iterations", "2"],
            "evaluation",
            1,
        ),
        (
            vec!["trace", "--json", "-e", "s(\"bd\")", "--duration", "1e12"],
            "resource-limit",
            3,
        ),
        (vec!["query", "--json", "/nonexistent-source.js"], "io", 4),
    ] {
        let (json, actual) = error_envelope(&args);
        assert_eq!(
            json["error"]["kind"], kind,
            "{args:?} reported the wrong error kind: {json}"
        );
        assert!(
            json["error"]["message"]
                .as_str()
                .is_some_and(|m| !m.is_empty()),
            "{args:?} reported an empty message: {json}"
        );
        assert_eq!(
            actual,
            Some(code),
            "{args:?} exited {actual:?}, not the {code} its kind maps to"
        );
    }
    let _ = std::fs::remove_file(render_output);
}

#[test]
fn the_error_envelope_never_pollutes_stdout() {
    // stdout is the command's data channel. A failure writing there would make
    // a caller's `| jq` see an error object where it expected haps.
    let out = run(&["query", "--json", "-e", "s(\"bd\""]);
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "a failing command wrote to stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn exit_codes_are_a_stable_contract() {
    // Pinned deliberately: these are what a script branches on, so a change
    // here is a breaking change and should have to be made on purpose.
    assert_eq!(
        run(&["query", "--json", "-e", r#"s("bd")"#]).status.code(),
        Some(0)
    );
    assert_eq!(
        run(&["query", "--json", "-e", "s(\"bd\""]).status.code(),
        Some(1)
    );
    // 2 is clap's usage code, left alone rather than reassigned.
    assert_eq!(
        run(&["query", "--json", "--no-such-flag"]).status.code(),
        Some(2)
    );
    assert_eq!(
        run(&["trace", "--json", "-e", r#"s("bd")"#, "--duration", "1e12"])
            .status
            .code(),
        Some(3)
    );
    assert_eq!(
        run(&["query", "--json", "/nonexistent-source.js"])
            .status
            .code(),
        Some(4)
    );
}

// -- signalled cancellation -------------------------------------------------

/// Send `signal` to a long `play` and wait for it, with a hard bound.
///
/// Unix only: the handler is `signal(2)`, and Windows console control handlers
/// are a different mechanism this build does not implement.
#[cfg(unix)]
fn signal_a_running_play(signal: i32) -> (Option<i32>, std::time::Duration) {
    let mut child = rustel()
        // The longest legal window, so the process is certainly still running
        // when the signal lands.
        .args([
            "trace",
            "--json",
            "-e",
            r#"s("bd*8")"#,
            "--duration",
            "86400",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn rustel");

    // Let it get properly underway first.
    std::thread::sleep(std::time::Duration::from_millis(300));
    // SAFETY: `kill` on a pid this test owns and has not yet reaped.
    unsafe {
        libc::kill(child.id() as libc::pid_t, signal);
    }

    let started = std::time::Instant::now();
    let deadline = started + std::time::Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait().expect("wait") {
            return (status.code(), started.elapsed());
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the process ignored signal {signal} and had to be killed");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(unix)]
#[test]
fn sigint_stops_a_running_play_promptly_and_is_reaped() {
    // The handler records the signal, a watcher stops the transport, and the
    // ordinary cancellation path runs.
    let (code, elapsed) = signal_a_running_play(libc::SIGINT);
    assert_eq!(
        code,
        Some(130),
        "SIGINT must exit 128+2 by convention, so a caller can tell an \
         interrupted run from a failed one"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "the process took {elapsed:?} to stop after SIGINT; cancellation must \
         stop in time, not merely eventually"
    );
}

#[cfg(unix)]
#[test]
fn sigterm_stops_a_running_play_promptly_and_is_reaped() {
    let (code, elapsed) = signal_a_running_play(libc::SIGTERM);
    assert_eq!(code, Some(143), "SIGTERM must exit 128+15");
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "the process took {elapsed:?} to stop after SIGTERM"
    );
}

#[cfg(unix)]
#[test]
fn a_signalled_exit_is_an_ordinary_exit_not_a_crash() {
    // `status.code()` is `None` for a process that a signal killed. A handled
    // signal gives an ordinary exit with a code.
    let (code, _) = signal_a_running_play(libc::SIGINT);
    assert!(
        code.is_some(),
        "the process was killed by the signal rather than handling it"
    );
}

#[cfg(unix)]
#[test]
fn sigint_during_musician_prebake_wins_over_the_evaluation_deadline() {
    let prebake = scratch("cancel-musician-prebake.js");
    let score = scratch("cancel-musician-prebake.strudel");
    std::fs::write(&prebake, "while (true) {}").expect("write runaway setup");
    std::fs::write(&score, "note('c4')").expect("write score");
    let args = [
        "play",
        score.to_str().expect("UTF-8 score"),
        "-vvv",
        "--prebake",
        prebake.to_str().expect("UTF-8 setup"),
    ];
    assert_signal_cancels(&args, libc::SIGINT, 130, MAX_STOP);
    let _ = std::fs::remove_file(prebake);
    let _ = std::fs::remove_file(score);
}

#[cfg(unix)]
#[test]
fn signals_cancel_initial_score_evaluation_on_every_cli_route() {
    let score = scratch("cancel-initial-score.strudel");
    let render = scratch("cancel-initial-score.json");
    let source = "while (true) {}";
    std::fs::write(&score, source).expect("write runaway score");
    let commands = vec![
        vec!["query".into(), "--json".into(), "-e".into(), source.into()],
        vec![
            "trace".into(),
            "--json".into(),
            "-e".into(),
            source.into(),
            "--duration".into(),
            "0.1".into(),
        ],
        vec![
            "render".into(),
            "-e".into(),
            source.into(),
            "--duration".into(),
            "0.1".into(),
            "-o".into(),
            render.to_string_lossy().into_owned(),
            "--format".into(),
            "onset-json".into(),
        ],
        vec![
            "bench".into(),
            "--json".into(),
            "-e".into(),
            source.into(),
            "--iterations".into(),
            "1".into(),
        ],
        vec!["play".into(), score.to_string_lossy().into_owned()],
    ];
    for (signal, code) in [(libc::SIGINT, 130), (libc::SIGTERM, 143)] {
        for command in &commands {
            let args = command.iter().map(String::as_str).collect::<Vec<_>>();
            assert_signal_cancels(&args, signal, code, MAX_STOP);
        }
    }
    assert!(
        !render.exists(),
        "a cancelled initial score still rendered output"
    );
    let _ = std::fs::remove_file(score);
}

// The bound includes startup and cold sample downloads: signals arrive 250 ms
// after spawn. These commands request 86400 seconds or run indefinitely, so
// a 60-second bound distinguishes cancellation from normal completion.
// Exit-status assertions separately verify the handled signal.
#[cfg(unix)]
const MAX_STOP: std::time::Duration = std::time::Duration::from_secs(60);

/// Signal a long-running command and require an exact, timely, reaped exit.
///
/// Exact status and elapsed-time bounds distinguish cancellation from ordinary
/// command completion.
#[cfg(unix)]
fn assert_signal_cancels(
    command: &[&str],
    signal: i32,
    expected_code: i32,
    max_stop: std::time::Duration,
) {
    let mut child = rustel()
        .args(command)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn rustel");
    std::thread::sleep(std::time::Duration::from_millis(250));
    // SAFETY: a pid this test owns and has not reaped.
    unsafe {
        libc::kill(child.id() as libc::pid_t, signal);
    }

    let started = std::time::Instant::now();
    // Generously outside the bound the assertion below uses, so the bound is
    // what fails and says why. A tighter deadline would kill the child first
    // and report a timeout instead of a slow cancellation.
    let deadline = started + MAX_STOP + std::time::Duration::from_secs(60);
    loop {
        if let Some(status) = child.try_wait().expect("wait") {
            let elapsed = started.elapsed();
            assert_eq!(
                status.code(),
                Some(expected_code),
                "{command:?}: signal {signal} produced exit {:?}, not \
                 {expected_code}. A code of None means the process was killed BY \
                 the signal instead of handling it.",
                status.code()
            );
            assert!(
                elapsed < max_stop,
                "{command:?}: took {elapsed:?} to stop after signal {signal}. \
                 Cancellation must be timely - a command that merely ran to \
                 completion would also have exited eventually."
            );
            return;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{command:?} ignored signal {signal} and had to be killed");
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Work long enough to be cancelled, for each command.
///
/// `render` is included because cancellation must also cover output-writing
/// commands.
#[cfg(unix)]
fn long_running_commands() -> Vec<Vec<String>> {
    let out = scratch("cancel-render.json");
    vec![
        // This pure workload reaches core's per-node cancellation check rather
        // than a host callback or span refusal.
        vec![
            "query".into(),
            "--json".into(),
            "-e".into(),
            r#"s("bd*2000")"#.into(),
            "--begin".into(),
            "0".into(),
            "--end".into(),
            "500".into(),
        ],
        vec![
            "bench".into(),
            "--json".into(),
            "-e".into(),
            r#"s("bd*500")"#.into(),
            "--iterations".into(),
            "1000000".into(),
        ],
        vec![
            "trace".into(),
            "--json".into(),
            "-e".into(),
            r#"s("bd*8")"#.into(),
            "--duration".into(),
            "86400".into(),
        ],
        vec![
            "render".into(),
            "-e".into(),
            r#"s("bd*8")"#.into(),
            "--duration".into(),
            "86400".into(),
            "-o".into(),
            out.to_string_lossy().into_owned(),
            "--format".into(),
            "onset-json".into(),
        ],
    ]
}

#[cfg(unix)]
#[test]
fn sigint_cancels_every_long_running_command_promptly() {
    for command in long_running_commands() {
        let args: Vec<&str> = command.iter().map(String::as_str).collect();
        assert_signal_cancels(&args, libc::SIGINT, 130, MAX_STOP);
    }
}

#[cfg(unix)]
#[test]
fn sigterm_cancels_every_long_running_command_promptly() {
    for command in long_running_commands() {
        let args: Vec<&str> = command.iter().map(String::as_str).collect();
        assert_signal_cancels(&args, libc::SIGTERM, 143, MAX_STOP);
    }
}

#[cfg(unix)]
#[test]
fn a_signal_arriving_before_the_work_starts_is_not_cleared() {
    // Hold source on stdin until after the signal to test early delivery.
    // transport.start() must not erase the stop request. Transport tests cover
    // the watcher's persistent reassertion of that request.
    use std::io::Write;
    let mut child = rustel()
        .args(["trace", "--json", "-", "--duration", "86400"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn rustel");

    // Blocked on stdin. Nothing has been evaluated, let alone scheduled.
    std::thread::sleep(std::time::Duration::from_millis(200));
    // SAFETY: a pid this test owns and has not reaped.
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGINT);
    }
    // NOW give it the work. A build that cleared the signal would schedule the
    // full 24-hour window.
    {
        let mut stdin = child.stdin.take().expect("stdin");
        stdin.write_all(br#"s("bd*8")"#).expect("write source");
    }

    let started = std::time::Instant::now();
    // Generously outside the bound the assertion below uses, so the bound is
    // what fails and says why. A tighter deadline would kill the child first
    // and report a timeout instead of a slow cancellation.
    let deadline = started + MAX_STOP + std::time::Duration::from_secs(60);
    loop {
        if let Some(status) = child.try_wait().expect("wait") {
            assert_eq!(
                status.code(),
                Some(130),
                "a signal delivered BEFORE the work started must still exit \
                 128+2; `transport.start()` cleared it and the run continued"
            );
            assert!(
                started.elapsed() < std::time::Duration::from_secs(3),
                "the early signal was honoured only after {:?}",
                started.elapsed()
            );
            return;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("an early SIGINT was ignored entirely");
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn a_source_can_be_read_from_stdin() {
    // The feature the test above relies on, asserted in its own right so it
    // cannot quietly stop working and take the determinism with it.
    use std::io::Write;
    let args = ["query", "--json", "-"];
    let mut child = rustel()
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn rustel");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(br#"s("bd sd")"#)
        .expect("write");
    let out = wait_for_output(child, &args);
    assert!(out.status.success(), "reading from stdin failed");
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(json["haps"].as_array().expect("haps").len(), 2);
}

#[test]
fn a_query_span_refusal_exits_three() {
    // A span refusal is a resource refusal and must report as one, not as
    // empty haps with exit 0.
    //
    // With `s("bd*2000")`, 500 cycles is 1,000,000 inner cycles, exactly at
    // MAX_QUERY_SPAN_CYCLES, and 600 is past it. The pair tests the boundary.
    let (json, code) = error_envelope(&[
        "query",
        "--json",
        "-e",
        r#"s("bd*2000")"#,
        "--begin",
        "0",
        "--end",
        "600",
    ]);
    assert_eq!(json["error"]["kind"], "resource-limit", "{json}");
    assert_eq!(code, Some(3), "a span refusal must exit 3");

    // ...and an ordinary span still succeeds, so this is a limit rather than a
    // blanket failure. The exact at-limit boundary is pinned in
    // `hap_budget.rs`, where a million haps cost nothing: through the CLI they
    // would have to be serialised to JSON, which took over a minute.
    let out = ok_stdout(&[
        "query",
        "--json",
        "-e",
        r#"s("bd*4")"#,
        "--begin",
        "0",
        "--end",
        "2",
    ]);
    let ok: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(ok["haps"].as_array().expect("haps").len(), 8);
}

#[test]
fn a_zero_length_window_schedules_nothing() {
    // A zero-length window is legal and schedules nothing, not even the onset
    // at time zero.
    let out = ok_stdout(&["trace", "--json", "-e", r#"s("bd")"#, "--duration", "0"]);
    let json: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(
        json["onsets"].as_array().expect("onsets").len(),
        0,
        "a zero-length window scheduled something: {out}"
    );
}

#[test]
fn every_encoded_wav_field_is_range_checked() {
    // Validating only the PCM size left the other header fields to wrap: a
    // 4,000,000,000 Hz render produced a byte rate of 16e9, truncated to
    // 3,115,171,840 in its u32 field. A header that parses cleanly and
    // describes a file that does not exist is worse than a refusal.
    let out = scratch("wrapped.wav");
    assert_reported_failure(
        &[
            "render",
            "-e",
            r#"s("bd")"#,
            "--duration",
            "0",
            "--sample-rate",
            "4000000000",
            "-o",
            out.to_str().expect("utf-8"),
            "--format",
            "wav",
        ],
        "byte rate",
    );
    let _ = std::fs::remove_file(&out);
}

/// A replay starts from the most recent installed save at the requested time.
#[test]
fn a_recorded_session_selects_the_state_at_the_requested_time() {
    use rustel_runtime::session_log::{SaveStatus, SessionMode, SessionRecorder, SessionScript};

    let session_path = scratch("replay-roundtrip.rustel-session");
    let mut recorder =
        SessionRecorder::create(session_path.clone(), SessionMode::Debug, None).expect("recorder");
    recorder.record_save(
        0.0,
        SaveStatus::Installed,
        "$: s(\"bd*4\").gain(0.6)\n",
        None,
    );
    recorder.record_save(4.0, SaveStatus::Rejected, "$: s(\"bd*4\"\n", Some("syntax"));
    recorder.record_save(
        8.0,
        SaveStatus::Installed,
        "$: s(\"bd*4 sd\").gain(0.6)\n",
        None,
    );
    drop(recorder);

    let script = SessionScript::load(&session_path).expect("load");
    assert_eq!(
        script.saves.len(),
        3,
        "debug tape must keep the rejected save"
    );

    let (index, opening) = script
        .installed_save_at(5.0)
        .expect("installed save before --from");
    assert_eq!(index, 0, "replay selected a rejected save");
    assert_eq!(
        &*opening.source, "$: s(\"bd*4\").gain(0.6)\n",
        "replay opened on the wrong state for --from 5"
    );
}

/// Built-in synth scores whose pitch `crossings_per_sec` tells apart.
const C3_SCORE: &str = r#"$: note("c3*4").s("sawtooth").gain(0.5)"#;
const C5_SCORE: &str = r#"$: note("c5*4").s("sawtooth").gain(0.5)"#;

/// Crossing rate separating the two scores, which measure about 264 (c3) and
/// 1058 (c5) crossings/s. 500 sits roughly a factor of two from each.
const C3_C5_THRESHOLD: f64 = 500.0;

/// The low score from t=0, the high one from t=8.
const LOW_THEN_HIGH: &[(f64, &str)] = &[(0.0, C3_SCORE), (8.0, C5_SCORE)];

/// A score that fails to parse.
const BROKEN_SCORE: &str = r#"$: note("c3*4""#;

/// Zero crossings per second of the left channel between two times of a
/// 48 kHz stereo PCM16 bounce. The window must lie inside the bounce.
fn crossings_per_sec(data: &[u8], from_secs: f64, to_secs: f64) -> f64 {
    let (start, end) = (pcm_bytes(from_secs), pcm_bytes(to_secs));
    assert!(
        end <= data.len(),
        "the window ends at byte {end}, but the bounce holds {} bytes",
        data.len()
    );
    let mut crossings = 0usize;
    let mut last_sign = 0i32;
    for frame in data[start..end].as_chunks::<PCM_FRAME>().0.iter() {
        let sign = i32::from(i16::from_le_bytes([frame[0], frame[1]])).signum();
        if sign != 0 {
            if last_sign != 0 && sign != last_sign {
                crossings += 1;
            }
            last_sign = sign;
        }
    }
    crossings as f64 / (to_secs - from_secs)
}

/// Arguments for `replay <tape> --from <from> --export <out>`, then `more`.
fn replay_export_args<'a>(
    tape: &'a std::path::Path,
    out: &'a std::path::Path,
    from: &'a str,
    more: &[&'a str],
) -> Vec<&'a str> {
    let mut args = vec![
        "replay",
        tape.to_str().expect("utf-8 tape"),
        "--from",
        from,
        "--export",
        out.to_str().expect("utf-8 out"),
    ];
    args.extend_from_slice(more);
    args
}

/// A `--from` export opens on the state sounding at `--from` and chains the
/// next save at its offset.
#[test]
fn a_replay_export_opens_on_the_state_sounding_at_from() {
    let tape = installed_tape("replay-export-opening.rustel-session", LOW_THEN_HIGH);
    let out = scratch("replay-export-opening.wav");

    // From t=5 the t=0 save sounds until the t=8 save lands at offset 3.
    ok_output(&replay_export_args(&tape, &out, "5", &["--duration", "4"]));
    let data = wav_pcm_of_length(&out, 4.0);
    assert!(!silent(&data[..pcm_bytes(2.0)]), "the export opened silent");
    let opening_rate = crossings_per_sec(&data, 0.5, 2.5);
    assert!(
        opening_rate < C3_C5_THRESHOLD,
        "the head is not the opening state: {opening_rate} crossings/s"
    );
    let chained_rate = crossings_per_sec(&data, 3.5, 4.0);
    assert!(
        chained_rate > C3_C5_THRESHOLD,
        "the save at offset 3 did not sound: {chained_rate} crossings/s"
    );

    let _ = std::fs::remove_file(&tape);
    let _ = std::fs::remove_file(&out);
}

/// A `--from` past the last save bounces the closing state for `--duration`.
#[test]
fn a_replay_export_past_the_end_of_the_tape_bounces_the_closing_state() {
    let tape = installed_tape("replay-export-past-end.rustel-session", LOW_THEN_HIGH);
    let out = scratch("replay-export-past-end.wav");

    ok_output(&replay_export_args(
        &tape,
        &out,
        "600",
        &["--duration", "1"],
    ));
    assert_audible(&out);
    let data = wav_pcm_of_length(&out, 1.0);
    let rate = crossings_per_sec(&data, 0.1, 1.0);
    assert!(
        rate > C3_C5_THRESHOLD,
        "the bounce is not the closing state: {rate} crossings/s"
    );

    let _ = std::fs::remove_file(&tape);
    let _ = std::fs::remove_file(&out);
}

/// An export is silent until the first installed save after `--from`, which
/// sounds at its own offset. Normal and debug recordings of one set bounce to
/// the same bytes.
#[test]
fn a_replay_export_is_silent_until_the_first_installed_save_after_from() {
    use rustel_runtime::session_log::{SaveStatus, SessionMode};

    let set = [
        (0.0, SaveStatus::Rejected, BROKEN_SCORE),
        (7.0, SaveStatus::Installed, C3_SCORE),
    ];
    let mut bounces = Vec::new();
    for mode in [SessionMode::Normal, SessionMode::Debug] {
        let name = format!("replay-export-late-{}", mode.as_str());
        let tape = recorded_tape(&format!("{name}.rustel-session"), mode, &set);
        let out = scratch(&format!("{name}.wav"));
        ok_output(&replay_export_args(&tape, &out, "5", &["--duration", "4"]));
        let data = wav_pcm_of_length(&out, 4.0);
        // The save lands at offset 2; 100 ms of margin ahead of it.
        assert!(
            silent(&data[..pcm_bytes(1.9)]),
            "{name}: the bounce sounded before the first installed save"
        );
        assert!(
            !silent(&data[pcm_bytes(2.0)..]),
            "{name}: the first installed save did not sound at its offset"
        );
        bounces.push(data);
        let _ = std::fs::remove_file(&tape);
        let _ = std::fs::remove_file(&out);
    }
    assert!(
        bounces[0] == bounces[1],
        "the normal and debug tapes bounced differently"
    );
}

/// A `--duration` window that closes before the first installed save bounces
/// silence of that length.
#[test]
fn a_replay_export_of_a_window_nothing_sounded_in_is_silent() {
    use rustel_runtime::session_log::{SaveStatus, SessionMode};

    let tape = recorded_tape(
        "replay-export-empty-window.rustel-session",
        SessionMode::Debug,
        &[
            (0.0, SaveStatus::Rejected, BROKEN_SCORE),
            (100.0, SaveStatus::Installed, C3_SCORE),
        ],
    );
    let out = scratch("replay-export-empty-window.wav");

    ok_output(&replay_export_args(&tape, &out, "0", &["--duration", "2"]));
    let data = wav_pcm_of_length(&out, 2.0);
    assert!(silent(&data), "the window is not silent");

    let _ = std::fs::remove_file(&tape);
    let _ = std::fs::remove_file(&out);
}

/// An export of a tape with no installed save fails and writes no file, with
/// or without `--duration`.
#[test]
fn a_replay_export_of_a_tape_where_nothing_installed_is_refused() {
    use rustel_runtime::session_log::{SaveStatus, SessionMode};

    let tape = recorded_tape(
        "replay-export-nothing-installed.rustel-session",
        SessionMode::Debug,
        &[(0.0, SaveStatus::Rejected, BROKEN_SCORE)],
    );
    let out = scratch("replay-export-nothing-installed.wav");
    for more in [&[][..], &["--duration", "2"][..]] {
        let _ = std::fs::remove_file(&out);
        assert_reported_failure(
            &replay_export_args(&tape, &out, "0", more),
            "nothing on it ever sounded",
        );
        assert!(!out.exists(), "{more:?}: a refused bounce wrote a file");
    }

    let _ = std::fs::remove_file(&tape);
}

/// The mp3 export reports two saves, the opening and the chained save, and
/// the opening's onsets, and writes an mp3 stream longer than 100 kB.
#[test]
#[cfg(feature = "mp3-export")]
fn a_replay_export_writes_the_chained_timeline_to_mp3() {
    let tape = installed_tape("replay-export-chained.rustel-session", LOW_THEN_HIGH);
    let out = scratch("replay-export-chained.mp3");

    let result = ok_output(&replay_export_args(
        &tape,
        &out,
        "5",
        &["--duration", "4", "--json"],
    ));
    let lines = structured_lines(&result.stderr);
    let saves = lines
        .iter()
        .find_map(|line| line["replay_export"]["saves"].as_u64())
        .expect("the pre-render report");
    assert_eq!(
        saves, 2,
        "the bounce must hold the opening and chained saves"
    );
    // About ten with the opening; about three without it.
    let onsets = lines
        .iter()
        .find_map(|line| line["replay_export"]["onsets"].as_u64())
        .expect("the render report");
    assert!(
        onsets >= 6,
        "the opening state did not sound in the mp3: {onsets} onsets"
    );
    let bytes = std::fs::read(&out).expect("export mp3");
    // 4 s at 320 kbps is about 160 kB.
    assert!(
        bytes.len() > 100_000,
        "the mp3 is too short for a 4 s bounce: {} bytes",
        bytes.len()
    );
    assert!(
        bytes.starts_with(b"ID3") || (bytes[0] == 0xFF && bytes[1] & 0xE0 == 0xE0),
        "the mp3 route did not write an mp3 stream"
    );

    let _ = std::fs::remove_file(&tape);
    let _ = std::fs::remove_file(&out);
}

/// An unknown recording mode names the real ones rather than failing blankly.
#[test]
fn an_unknown_session_mode_lists_the_real_ones() {
    let output = run(&["play", "song.strudel", "--watch", "--save-session=verbose"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("normal") && stderr.contains("debug"),
        "an unknown session mode must list the real ones: {stderr}"
    );
}

/// Opting out and choosing a mode are mutually exclusive, and saying so beats
/// silently ignoring one of them.
#[test]
fn no_save_session_conflicts_with_asking_for_one() {
    let output = run(&[
        "play",
        "song.strudel",
        "--watch",
        "--no-save-session",
        "--save-session=debug",
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--no-save-session") || stderr.contains("cannot be used"),
        "contradictory recording flags must be reported: {stderr}"
    );
}

/// A deep pattern graph is refused with a resource-limit error. A stack overflow
/// aborts the process and no recovery catches it. A debug frame is larger, so
/// this test runs in debug too. A signal or exit 134 means the abort is back.
#[test]
fn a_deep_pattern_graph_is_refused_instead_of_aborting_the_process() {
    for wrappers in [8_192, 20_000] {
        let source = format!(
            "let p = s('bd'); for (let i = 0; i < {wrappers}; i++) p = p.fast(2).slow(2); p"
        );
        let out = run(&["query", "--json", "-e", &source, "--end", "1/64"]);
        let code = out.status.code();
        assert!(
            code.is_some(),
            "{wrappers} wrappers killed the process by signal - the stack \
             overflow abort is back: {:?}",
            out.status
        );
        assert_ne!(code, Some(134), "{wrappers} wrappers aborted (exit 134)");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            text.contains("resource-limit"),
            "{wrappers} wrappers should be a typed resource refusal, got: {text}"
        );
    }
}

/// The limit is a boundary, so an ordinary graph must still query. A chain of
/// combinators well under the ceiling is what real sources look like.
#[test]
fn a_graph_below_the_depth_limit_still_queries() {
    let source = "let p = s('bd'); for (let i = 0; i < 100; i++) p = p.fast(2).slow(2); p";
    let out = run(&["query", "--json", "-e", source, "--end", "1/4"]);
    assert!(
        out.status.success(),
        "200 chained combinators must still query: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("bd"),
        "the shallow graph produced no haps: {text}"
    );
}

// -- check --------------------------------------------------------------

/// The static lint never runs the score, so a name that only fails to
/// resolve at evaluation time is invisible to it. `check` must still refuse
/// it, the way `strudel.cc`'s `ReferenceError` would.
#[test]
fn check_rejects_an_undefined_name() {
    let out = run(&["check", "-e", "gibberish", "--no-samples"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("gibberish"),
        "check did not name the undefined identifier: {stdout}"
    );
}

/// A score that throws is not a score that plays. The static lint has
/// nothing to say about a `throw`; only running it catches this.
#[test]
fn check_rejects_a_score_that_throws() {
    let out = run(&["check", "-e", r#"throw new Error("boom")"#, "--no-samples"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The evaluation check is additional, not a replacement: the lint's own
/// did-you-mean finding for a misspelled method must still be reported.
#[test]
fn check_keeps_the_lint_did_you_mean_suggestion() {
    let out = run(&["check", "-e", r#"note("c e g").fastt(2)"#, "--no-samples"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("did you mean `fast`"),
        "check dropped the lint's did-you-mean suggestion: {stdout}"
    );
}

/// A score with nothing wrong with it must still pass, in both output
/// formats - the evaluation check is a second gate, not a stricter one.
#[test]
fn check_accepts_a_valid_score_human_and_json() {
    let human = ok_stdout(&["check", "-e", r#"s("bd sd")"#, "--no-samples"]);
    assert!(
        human.contains("no issues found"),
        "a valid score was not reported clean: {human}"
    );
    let json = ok_stdout(&["check", "-e", r#"s("bd sd")"#, "--no-samples", "--json"]);
    let verdict: serde_json::Value = serde_json::from_str(&json).expect("check verdict JSON");
    assert_eq!(verdict["check"]["status"], "ok");
}

#[test]
fn check_and_query_explain_silent_score_mistakes() {
    for signal in ["sine", "sawtooth", "square", "triangle"] {
        let source = format!("note('c e g').{signal}");
        let check = run(&["check", "-e", &source, "--no-samples"]);
        assert!(!check.status.success(), "check accepted {source}");
        assert!(
            String::from_utf8_lossy(&check.stdout).contains(&format!(".s(\"{signal}\")")),
            "check omitted the correction for {source}"
        );
        assert_reported_failure(&["query", "-e", &source], "is not a pattern method");
    }
    assert_reported_failure(&["query", "-e", "chord('am7').voicing()"], "unknown chord");
    let valid = ok_stdout(&["query", "--json", "-e", "note('c e g').s('sine')"]);
    let report: serde_json::Value = serde_json::from_str(&valid).expect("valid query JSON");
    assert!(
        report["haps"]
            .as_array()
            .is_some_and(|haps| !haps.is_empty()),
        "valid query returned no haps: {report}"
    );
}

#[test]
fn one_shot_commands_fail_when_local_samples_lack_a_grant() {
    let source = "samples('local:'); s('bd')";
    let check = run(&["check", "-e", source, "--no-samples"]);
    assert!(!check.status.success(), "check accepted denied samples()");
    assert!(
        String::from_utf8_lossy(&check.stdout).contains("--allow-local-samples"),
        "check gave no grant hint: {}",
        String::from_utf8_lossy(&check.stdout)
    );
    assert_reported_failure(&["query", "-e", source], "--allow-local-samples DIR");
}

#[test]
fn one_shot_commands_fail_when_an_allowed_local_sample_folder_is_missing() {
    let allowed = tempfile::tempdir().expect("local sample root");
    let root = allowed.path().to_str().expect("UTF-8 sample root");
    let source = "samples('local:missing-folder'); s('bd')";
    let check = run(&[
        "check",
        "-e",
        source,
        "--allow-local-samples",
        root,
        "--no-samples",
    ]);
    assert!(!check.status.success(), "check accepted a missing folder");
    assert!(
        String::from_utf8_lossy(&check.stdout).contains("local samples: cannot read"),
        "check omitted the local folder failure: {}",
        String::from_utf8_lossy(&check.stdout)
    );
    assert_reported_failure(
        &["query", "-e", source, "--allow-local-samples", root],
        "local samples: cannot read",
    );
}

#[test]
fn a_refused_samples_import_is_reported_where_it_is_written() {
    let allowed = tempfile::tempdir().expect("local sample root");
    let root = allowed.path().to_str().expect("UTF-8 sample root");
    let source = "samples('local:')\nsamples('http://127.0.0.1:9/strudel.json')\ns('bd')";
    let check = run(&[
        "check",
        "-e",
        source,
        "--allow-local-samples",
        root,
        "--no-samples",
        "--json",
    ]);
    assert!(!check.status.success(), "check accepted a refused import");
    let stdout = String::from_utf8_lossy(&check.stdout);
    let refusal = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|line| {
            line["check"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("outside the permitted sample origins"))
        })
        .unwrap_or_else(|| panic!("no refusal finding: {stdout}"));
    assert_eq!(refusal["check"]["line"], 2, "{refusal}");
    assert!(
        !stdout.contains("--allow-local-samples"),
        "a remote refusal got the local grant hint: {stdout}"
    );
}

// -- query: refusing a score that does not evaluate ---------------------

/// `query` does not fall back to the mini parser when the JavaScript fails.
/// An undefined name is an error, as on strudel.cc: no events, exit 1.
#[test]
fn query_rejects_an_undefined_name() {
    let out = run(&["query", "-e", "gibberish"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "a rejected query printed events anyway: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("gibberish"),
        "query did not name the undefined identifier: {stderr}"
    );
}

/// Three bare, unquoted words are not valid JavaScript. The same
/// compatibility fallback used to reparse them as three mini-notation
/// events; `query` must refuse them instead.
#[test]
fn query_rejects_bare_words_that_are_not_javascript() {
    let out = run(&["query", "-e", "x y z"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "a rejected query printed events anyway: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// The `--json` error envelope must name the same failure, in the same
/// shape every other command's `--json` failure uses.
#[test]
fn query_rejects_an_undefined_name_as_json() {
    let out = run(&["query", "-e", "gibberish", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    let envelope: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("query error envelope JSON");
    assert_eq!(envelope["error"]["kind"], "evaluation");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .expect("error.message is a string")
            .contains("gibberish"),
        "{envelope}"
    );
}

// -- validate ----------------------------------------------------------------

#[test]
fn validate_accepts_a_valid_eval_expression_and_prints_a_verdict() {
    let out = ok_stdout(&["validate", "-e", r#"s("bd sd")"#, "--json"]);
    let verdict: serde_json::Value = serde_json::from_str(&out).expect("validate verdict JSON");
    assert_eq!(verdict["validate"]["status"], "valid");
}

#[test]
fn validate_accepts_a_valid_score_file_from_disk() {
    let score = scratch("validate-good.strudel");
    std::fs::write(&score, "note('c e g').slow(2)").expect("write valid score");
    let out = ok_stdout(&["validate", &score.to_string_lossy(), "--json"]);
    let verdict: serde_json::Value = serde_json::from_str(&out).expect("validate verdict JSON");
    assert_eq!(verdict["validate"]["status"], "valid");
    assert_eq!(
        verdict["validate"]["source"],
        score.to_string_lossy().as_ref()
    );
    let _ = std::fs::remove_file(&score);
}

/// An invalid score must fail with a diagnostic and a non-zero exit, and must
/// not panic. Every other subcommand follows the same contract.
#[test]
fn validate_rejects_an_invalid_score_with_a_diagnostic() {
    // A parse error.
    assert_reported_failure(&["validate", "-e", "note("], "expected");
    // A runtime error thrown while evaluating the score body.
    assert_reported_failure(
        &["validate", "-e", "throw new Error('broken score')"],
        "evaluation",
    );
}

#[test]
fn validate_reports_a_missing_file_as_io_not_success() {
    // Assert rustel's own prefix, not the operating system's wording, which
    // differs between Unix and Windows. An unreadable source is an `io:`
    // failure that names the file.
    assert_reported_failure(
        &["validate", "definitely-missing.strudel"],
        "io: cannot open the source definitely-missing.strudel",
    );
}

/// Validation is a one-shot check. It does not schedule, render or wait on
/// the clock, so a looping pattern returns a verdict immediately.
#[test]
fn validate_returns_immediately_for_a_score_that_would_run_forever() {
    let started = std::time::Instant::now();
    let out = ok_stdout(&["validate", "-e", r#"s("bd sd hh")"#, "--json"]);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "validate did not return promptly"
    );
    let verdict: serde_json::Value = serde_json::from_str(&out).expect("validate verdict JSON");
    assert_eq!(verdict["validate"]["status"], "valid");
}

#[test]
fn the_version_flag_prints_the_package_version() {
    let out = run(&["--version"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        text.trim(),
        format!("rustel {}", env!("CARGO_PKG_VERSION")),
        "{text}"
    );
}

/// `gamepad-monitor` stops when it is interrupted, before its duration ends.
/// The duration is an upper bound.
#[cfg(windows)]
#[test]
fn the_gamepad_monitor_stops_when_it_is_interrupted() {
    use std::os::windows::process::CommandExt;

    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CTRL_BREAK_EVENT: u32 = 1;
    unsafe extern "system" {
        fn GenerateConsoleCtrlEvent(dwCtrlEvent: u32, dwProcessGroupId: u32) -> i32;
    }

    let mut child = rustel()
        .args(["gamepad-monitor", "--duration", "120"])
        .creation_flags(CREATE_NEW_PROCESS_GROUP)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn rustel");
    std::thread::sleep(std::time::Duration::from_millis(1_500));
    if let Some(status) = child.try_wait().expect("wait") {
        panic!("the monitor ended before it was asked to: {status:?}");
    }

    let started = std::time::Instant::now();
    assert_ne!(
        unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, child.id()) },
        0,
        "the console event could not be sent"
    );
    let output = wait_for_output(child, &["gamepad-monitor", "<ctrl-break>"]);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "it sat out its duration instead of stopping: {:?}",
        started.elapsed()
    );
    assert_eq!(
        output.status.code(),
        Some(130),
        "stdout: {}
stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

// A summary proves that Ctrl-C ran graceful shutdown rather than the default
// Windows handler. Give the child its own process group so the signal cannot
// terminate the test runner.
#[cfg(windows)]
#[test]
fn a_console_interrupt_stops_the_run_and_lets_it_say_its_piece() {
    use std::os::windows::process::CommandExt;

    // Ctrl-C is disabled for a new process group; Ctrl-Break is the one that
    // can be aimed, and the handler answers both the same way.
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CTRL_BREAK_EVENT: u32 = 1;
    unsafe extern "system" {
        fn GenerateConsoleCtrlEvent(dwCtrlEvent: u32, dwProcessGroupId: u32) -> i32;
    }

    let mut child = rustel()
        .args(["midi-monitor", "--duration", "60"])
        .creation_flags(CREATE_NEW_PROCESS_GROUP)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn rustel");
    // Long enough for the handler to be installed and the monitor to be in
    // its poll loop.
    std::thread::sleep(std::time::Duration::from_millis(1_500));
    if let Some(status) = child.try_wait().expect("wait") {
        // No MIDI input on this machine, so there is nothing to interrupt.
        // Skipping passes, the way the renderer suites do on a machine with
        // no device: this test is about the signal, not about the hardware.
        eprintln!("skipped: midi-monitor ended on its own ({status:?})");
        return;
    }

    assert_ne!(
        unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, child.id()) },
        0,
        "the console event could not be sent"
    );
    let output = wait_for_output(child, &["midi-monitor", "<ctrl-break>"]);

    // 128 + SIGINT, as on Unix. The default handler would leave 0xC000013A.
    assert_eq!(
        output.status.code(),
        Some(130),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("message(s) in"),
        "the summary is the proof the run ended itself: {stdout:?}"
    );
}

#[test]
fn cache_clear_requires_consent_and_dry_run_changes_nothing() {
    for command in [
        vec!["samples", "clear", "--json"],
        vec!["clear-score-cache"],
    ] {
        let base = scratch(if command.len() == 1 {
            "score-consent"
        } else {
            "sample-consent"
        });
        std::fs::create_dir_all(base.join("score")).unwrap();
        let entry = base.join("score").join("response.wav");
        std::fs::write(&entry, b"keep until confirmed").unwrap();
        for flags in [vec![], vec!["--no-input"], vec!["--quiet"]] {
            let output = rustel()
                .args(&command)
                .args(flags)
                .env("RUSTEL_SAMPLE_CACHE", &base)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(1));
            assert!(output.stdout.is_empty());
            let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
            assert!(
                error["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("--force")
            );
            assert!(entry.exists());
        }
        let output = rustel()
            .args(&command)
            .args(["--dry-run", "--no-input", "--force"])
            .env("RUSTEL_SAMPLE_CACHE", &base)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let report = report.as_object().unwrap().values().next().unwrap();
        assert_eq!(report["status"], "dry_run");
        assert!(report["scope"].as_str().unwrap().contains("kept"));
        assert!(entry.exists());
        assert_eq!(
            std::fs::read_dir(&base).unwrap().count(),
            1,
            "dry run wrote a lock or marker"
        );
        std::fs::remove_dir_all(&base).unwrap();
        let output = rustel()
            .args(&command)
            .arg("-n")
            .env("RUSTEL_SAMPLE_CACHE", &base)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(!base.exists(), "dry run created a cache directory");
    }
}

#[test]
fn bare_invocation_and_display_flags_offer_help() {
    for args in [
        vec![],
        vec!["-h"],
        vec!["--no-color", "--help"],
        vec!["--plain", "--help"],
    ] {
        let output = run(&args);
        assert!(output.status.success(), "{output:?}");
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("Usage:"));
        assert!(help.contains("https://github.com/tzfm/rustel/issues"));
        assert!(!help.contains('\x1b'));
        assert!(output.stderr.is_empty());
    }
    for command in [None, Some("export"), Some("samples")] {
        let mut args = command.into_iter().collect::<Vec<_>>();
        args.push("-h");
        let short = ok_stdout(&args);
        *args.last_mut().unwrap() = "--help";
        assert_eq!(short, ok_stdout(&args));
    }
}

#[test]
fn check_json_findings_end_with_a_json_failure_envelope() {
    let output = run(&[
        "check",
        "-e",
        "note(\"c3\").unknownControl(1)",
        "--json",
        "--quiet",
    ]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(!output.stdout.is_empty());
    for line in String::from_utf8(output.stdout).unwrap().lines() {
        serde_json::from_str::<serde_json::Value>(line).unwrap();
    }
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("check found issues")
    );
}

#[test]
fn watch_code_piped_output_has_no_terminal_controls() {
    use std::io::Write;
    let mut child = rustel()
        .args(["watch-code", "--plain"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let source = "note('c3')\n\u{1b}]52;c;payload\u{7}";
    let event = serde_json::json!({"score_active": {
        "source": rustel_runtime::session_log::encode_base64(source.as_bytes()),
        "index": 1
    }});
    writeln!(child.stdin.take().unwrap(), "{event}").unwrap();
    let output = wait_for_output(child, &["watch-code"]);
    assert!(output.status.success());
    let output = String::from_utf8(output.stdout).unwrap();
    assert!(output.contains("note('c3')"));
    assert!(output.contains("␛]52;c;payload␇"), "{output}");
    assert!(!output.contains('\x1b'));
}

#[test]
fn score_text_in_errors_findings_and_values_cannot_drive_the_terminal() {
    let throws = r"throw new Error('\u001b]52;c;cGF5bG9hZA==\u0007')";
    let check = run(&["check", "-e", throws]);
    let query_error = run(&["query", "-e", throws]);
    let query_value = run(&[
        "query",
        "-e",
        "// \u{1b}]52;c;cGF5bG9hZA==\u{7}\nreify({s: String.fromCharCode(27) + ']52;c;cGF5bG9hZA==' + String.fromCharCode(7)})",
    ]);
    assert!(query_value.status.success(), "{query_value:?}");
    // The source line and the hap's value each carry one sequence.
    for (shown, count) in [
        (check.stdout, 1),
        (query_error.stderr, 1),
        (query_value.stdout, 2),
    ] {
        let shown = String::from_utf8(shown).unwrap();
        assert_eq!(
            shown.matches("␛]52;c;cGF5bG9hZA==␇").count(),
            count,
            "{shown}"
        );
        assert!(
            !shown.contains('\x1b') && !shown.contains('\x07'),
            "{shown:?}"
        );
    }

    // A syntax finding quotes the character the parser stopped at.
    let stray = run(&["check", "-e", "note('c3')\u{1b}"]);
    let stray = String::from_utf8(stray.stdout).unwrap();
    assert!(
        stray
            .lines()
            .any(|line| line.starts_with("syntax") && line.contains('␛')),
        "{stray}"
    );
    assert!(!stray.contains('\x1b'), "{stray:?}");
}

#[cfg(unix)]
#[path = "cli_terminal.rs"]
mod terminal;

#[test]
fn misspelled_commands_with_arguments_offer_a_hint() {
    let output = run(&["sampls", "cache", "--json"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("a similar subcommand exists: 'samples'")
    );

    let output = run(&["samples", "--bogus"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("a similar subcommand exists"));

    // A score in the place of a command is not a misspelling.
    let output = run(&["song.strudel", "--watch"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("run `rustel play song.strudel`"));
}

mod diagnostics;
