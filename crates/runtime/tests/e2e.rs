//! Queries and renders every score under `tests/e2e/scores`, comparing the
//! result against `tests/e2e/golden`.
//!
//! See `tests/e2e/README.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rustel_audio::DspDispatch;
use rustel_fraction::Fraction;
use rustel_runtime::{
    AccelerationPreference, ScoreSampleAccess, Session, SessionConfig, score_sample_cache_dir,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Cycles compared as events, independent of a case's render length.
const QUERY_CYCLES: i128 = 4;

/// Matches `chrome_reference.rs`, so both goldens mean the same thing by
/// "the sound started here".
const AUDIBLE_FLOOR: f32 = 1e-7;

/// Samples are rounded onto this grid before hashing to reduce sensitivity
/// to small differences between hosts' libm implementations.
///
/// Derived rather than chosen. The disagreement between two hosts is about
/// 1.2e-07 - measured, from `seed710`'s peak differing in its eighth decimal
/// (1.735149145 against 1.735149264) while its RMS matched to nine. Only
/// samples that pass through a transcendental differ at all, so call that 1% of
/// the eleven affected scores: roughly 42,000 samples. A rounding boundary
/// flips when a sample lands within one noise step of it, so the expected
/// number of flips is `noise * grid * samples`:
///
///     1/4096  →  20.8 flips     (and 11 scores did fail, on a host whose
///     1/256   →   1.3             libm differs from the one that recorded
///     1/64    →   0.3             these goldens)
///
/// 1/64 leaves a step of 0.0156, about 36 dB below full scale. That is coarse,
/// but still far finer than the changes this gate must catch: when eleven
/// songs' banks stopped resolving, a peak moved from 0.0 to 0.55. Level and
/// loudness are compared separately below at `METRIC_TOLERANCE`, so they
/// catch a gain change too small to move this hash.
const AUDIO_HASH_GRID: f32 = 64.0;

/// Only consulted off the capture platform, where the audio hash cannot match
/// because the platform's libm does not - see `AUDIO_HASH_GRID`. There the
/// question is whether the sound moved, and peak and RMS answer it.
///
/// `chrome_reference.rs` uses 1e-4, which is the right tolerance for two
/// renders of the same engine on the same machine. It is too tight across
/// operating systems: `docs/audio/transient` on Apple silicon has a peak of
/// 0.277811855 against a golden of 0.277771026, a relative delta of 1.47e-4,
/// while its RMS matches to 5.9e-5 and its audible frame range is identical.
/// A level difference of one part in a thousand is 0.009 dB.
///
/// One thousandth still catches the regressions this gate exists for: when
/// eleven songs' sample banks stopped resolving, a peak moved from 0.0 to
/// 0.546612.
const METRIC_TOLERANCE: f64 = 0.001;

/// How far the first and last audible frame may move off the capture
/// platform. One render block: a different target's rounding can cross the
/// audible floor a few samples early or late, while a note that actually
/// moved moves by thousands.
const AUDIBLE_FRAME_TOLERANCE: usize = 128;

/// How much slower the corpus may get before that is a regression, on the one
/// machine the baseline was recorded on.
const SPEED_REGRESSION_LIMIT: f64 = 1.5;

/// Turns the machine-specific timing baseline from a report into a gate.
/// Contributors should not set this: seconds measured on one computer say
/// nothing useful about another computer's performance.
const SPEED_GATE_ENV: &str = "RUSTEL_E2E_SPEED_GATE";

/// Selects engine-owned kernels without changing the committed expectations.
const ACCELERATION_ENV: &str = "RUSTEL_E2E_ACCELERATION";

/// Path to this suite from the repository root, used in copy-pasteable
/// diagnostics regardless of the platform running the test.
const E2E_REPOSITORY_PATH: &str = "crates/runtime/tests/e2e";

fn e2e_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e")
}

fn parse_acceleration(
    value: Result<String, std::env::VarError>,
) -> Result<AccelerationPreference, String> {
    match value {
        Ok(value) => value
            .parse::<AccelerationPreference>()
            .map_err(|error| format!("{ACCELERATION_ENV}: {error}")),
        Err(std::env::VarError::NotPresent) => Ok(AccelerationPreference::Auto),
        Err(std::env::VarError::NotUnicode(_)) => Err(format!(
            "{ACCELERATION_ENV} must be Unicode; expected auto or portable"
        )),
    }
}

fn acceleration_preference() -> AccelerationPreference {
    parse_acceleration(std::env::var(ACCELERATION_ENV)).unwrap_or_else(|error| panic!("{error}"))
}

// ---------------------------------------------------------------- the cases

struct Case {
    /// Path below `scores/`, without the extension.
    name: String,
    source: String,
    duration: f64,
}

impl Case {
    fn set(&self) -> &str {
        self.name.split('/').next().unwrap_or("")
    }
}

/// `@duration` out of the file's own header comment.
fn header_duration(source: &str, name: &str) -> f64 {
    let duration: f64 = source
        .lines()
        .take_while(|line| !line.contains("*/"))
        .find_map(|line| line.trim().strip_prefix("@duration ").map(str::trim))
        .unwrap_or_else(|| panic!("{name} has no @duration in its header"))
        .parse()
        .unwrap_or_else(|e| panic!("{name} has an unreadable @duration: {e}"));
    assert!(
        duration > 0.0 && duration.is_finite(),
        "{name} has an invalid @duration: {duration}"
    );
    duration
}

fn collect(dir: &Path, root: &Path, into: &mut Vec<Case>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|e| e.expect("dir entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect(&path, root, into);
            continue;
        }
        if path.extension().is_none_or(|e| e != "strudel") {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("read score");
        let name = path
            .strip_prefix(root)
            .expect("under root")
            .with_extension("")
            .to_string_lossy()
            .replace('\\', "/");
        let duration = header_duration(&source, &name);
        into.push(Case {
            name,
            source,
            duration,
        });
    }
}

fn cases() -> Vec<Case> {
    let root = e2e_dir().join("scores");
    let mut found = Vec::new();
    collect(&root, &root, &mut found);
    assert!(!found.is_empty(), "no scores under {}", root.display());
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

// ------------------------------------------------------------- measurement

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct Measured {
    case: String,
    /// The requested length.
    dur: f64,
    /// The length actually rendered. `dur` alone cannot catch a renderer that
    /// returned short, long, or empty output.
    frames: usize,
    haps: usize,
    /// Over each event's `show` text: what is played.
    haps_sha256: String,
    /// Source spans carried by those events. A count, not part of the hash: a
    /// location moves when a comment above it does.
    ctx: usize,
    peak: f32,
    rms: f32,
    /// First and last frame above [`AUDIBLE_FLOOR`]; absent for silence.
    audible: Option<(usize, usize)>,
    audio_sha256: String,
}

/// What the goldens were captured on, which decides whether the audio hash is
/// the gate or the fast path. Float results are reproducible on one target
/// and are not promised across targets.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct Environment {
    arch: String,
    os: String,
    sample_rate: u32,
    channels: u16,
    query_cycles: i128,
}

/// Aggregate cost of the whole corpus.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Timing {
    total_secs: f64,
    per_set_secs: BTreeMap<String, f64>,
    slowest: Vec<(String, f64)>,
}

fn hex(digest: impl AsRef<[u8]>) -> String {
    use std::fmt::Write as _;
    digest
        .as_ref()
        .iter()
        .fold(String::with_capacity(64), |mut s, byte| {
            let _ = write!(s, "{byte:02x}");
            s
        })
}

/// A missing sample is kept apart from a sound difference. Offline, sample
/// loading soft-fails to a silent voice, so a cold cache would otherwise look
/// like a regression in every case that uses a bank.
enum Outcome {
    Measured(Box<Measured>, Duration),
    SamplesUnavailable(String),
    Failed(String),
}

/// The policy a musician runs under: `rustel` grants public CORS origins
/// unless asked for `--strict-origins`, and upstream's sample examples select
/// banks by URL.
fn session_config(dispatch: DspDispatch) -> SessionConfig {
    let mut access = ScoreSampleAccess::denied();
    access.permit_public_cors_origins();
    SessionConfig::default()
        .with_score_sample_access(access)
        .with_dsp_dispatch(dispatch)
}

fn measure(case: &Case, dispatch: DspDispatch) -> Outcome {
    let started = Instant::now();
    let config = session_config(dispatch);
    let channels = usize::from(config.channels);
    let mut session = match Session::with_config(config) {
        Ok(session) => session,
        Err(e) => return Outcome::Failed(format!("session: {e}")),
    };
    session.set_direct_diagnostic_logging(false);
    if let Err(e) = session.enable_default_samples() {
        return Outcome::Failed(format!("samples: {e}"));
    }
    if let Err(e) = session.evaluate(&case.source) {
        return Outcome::Failed(format!("evaluate: {e}"));
    }

    let haps = match session.query(Fraction::new(0, 1), Fraction::new(QUERY_CYCLES, 1)) {
        Ok(haps) => haps,
        Err(e) => return Outcome::Failed(format!("query: {e}")),
    };
    let mut hasher = Sha256::new();
    let mut ctx = 0usize;
    for hap in &haps {
        hasher.update(hap.show().as_bytes());
        hasher.update(b"\n");
        ctx += hap.context.len();
    }
    let haps_sha256 = hex(hasher.finalize());

    let pcm = match session.render_pcm(case.duration) {
        Ok(pcm) => pcm,
        Err(e) => return Outcome::Failed(format!("render: {e}")),
    };
    if channels == 0 || pcm.len() % channels != 0 {
        return Outcome::Failed(format!(
            "render returned {} samples for {channels} channels",
            pcm.len()
        ));
    }
    let frames = pcm.len() / channels;

    // After the render: resolution is asynchronous, and a bank that never
    // arrived only reports itself once something asked for it.
    let failures = session.take_sample_failures();
    if !failures.is_empty() {
        let messages = failures
            .into_iter()
            .map(|failure| failure.message)
            .collect::<Vec<_>>();
        return Outcome::SamplesUnavailable(messages.join("; "));
    }

    let mut peak = 0.0f32;
    let mut sum_squares = 0.0f64;
    for &sample in &pcm {
        peak = peak.max(sample.abs());
        sum_squares += f64::from(sample) * f64::from(sample);
    }
    let rms = (sum_squares / pcm.len().max(1) as f64).sqrt() as f32;
    // NaN compares false against every tolerance and would otherwise pass the
    // cross-platform path while also disappearing from the audible scan.
    if !peak.is_finite() || !rms.is_finite() {
        return Outcome::Failed(format!("audio is not finite: peak {peak}, rms {rms}"));
    }
    let mut audible_frames = pcm
        .chunks_exact(channels)
        .enumerate()
        .filter(|(_, frame)| frame.iter().any(|sample| sample.abs() >= AUDIBLE_FLOOR))
        .map(|(index, _)| index);
    let first = audible_frames.next();
    let audible = first.map(|first| (first, audible_frames.next_back().unwrap_or(first)));

    // Quantize samples on the grid documented by AUDIO_HASH_GRID to reduce
    // sensitivity to platform-specific rounding. Peak, RMS and the audible
    // frame range use unquantized samples and are checked when hashes differ.
    let mut hasher = Sha256::new();
    for &sample in &pcm {
        let step = (sample * AUDIO_HASH_GRID).round() as i32;
        hasher.update(step.to_le_bytes());
    }

    Outcome::Measured(
        Box::new(Measured {
            case: case.name.clone(),
            dur: case.duration,
            frames,
            haps: haps.len(),
            haps_sha256,
            ctx,
            peak,
            rms,
            audible,
            audio_sha256: hex(hasher.finalize()),
        }),
        started.elapsed(),
    )
}

/// One `Session` per case, built on the worker thread that uses it.
///
/// `Session` is `!Send`, so it never crosses a thread boundary. A fresh one is
/// required because score-level `register()` and `window` state persist. The
/// parallelism is internal because the gate runs `--test-threads=1`, which
/// serialises test functions and leaves threads inside one of them alone. The
/// 64 MiB stack is what the CLI and the studio give a Session's thread: a
/// pattern query recurses once per graph node.
fn measure_all(cases: &[Case], dispatch: DspDispatch) -> Vec<Outcome> {
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get());
    let next = AtomicUsize::new(0);
    let mut indexed: Vec<(usize, Outcome)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|worker| {
                let next = &next;
                std::thread::Builder::new()
                    .name(format!("e2e-{worker}"))
                    .stack_size(64 * 1024 * 1024)
                    .spawn_scoped(scope, move || {
                        let mut mine = Vec::new();
                        loop {
                            let index = next.fetch_add(1, Ordering::Relaxed);
                            let Some(case) = cases.get(index) else { break };
                            mine.push((index, measure(case, dispatch)));
                        }
                        mine
                    })
                    .expect("spawn a corpus worker")
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("worker panicked"))
            .collect()
    });
    indexed.sort_by_key(|(index, _)| *index);
    indexed.into_iter().map(|(_, outcome)| outcome).collect()
}

// ------------------------------------------------------------- the goldens

fn read_golden(set: &str) -> BTreeMap<String, Measured> {
    let path = e2e_dir().join(format!("golden/{set}.jsonl"));
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut rows = BTreeMap::new();
    for (line_number, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let measured: Measured = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("{}:{}: {e} in {line}", path.display(), line_number + 1));
        let name = measured.case.clone();
        assert!(
            rows.insert(name.clone(), measured).is_none(),
            "{} contains duplicate case {name}",
            path.display()
        );
    }
    rows
}

fn read_json<T: serde::de::DeserializeOwned>(name: &str) -> T {
    let path = e2e_dir().join(format!("golden/{name}"));
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn relative_delta(a: f64, b: f64) -> f64 {
    let scale = a.abs().max(b.abs());
    if scale < 1e-12 {
        0.0
    } else {
        (a - b).abs() / scale
    }
}

/// Events are compared before audio so the diagnostic can distinguish a moved
/// event stream from an unchanged stream whose rendered samples moved.
///
/// `exact` gates on the audio hash itself, and is opt-in: see
/// `RUSTEL_E2E_EXACT` where it is read. It exists because a tolerance cannot
/// catch everything - a one-ULP change in an oscillator moves every sample
/// while moving peak and RMS by nothing - but only the machine that captured
/// the goldens can tell that from its own libm.
fn compare(name: &str, want: &Measured, got: &Measured, exact: bool) -> Option<String> {
    if want.haps != got.haps {
        return Some(format!(
            "{name}: event count changed over {QUERY_CYCLES} cycles: expected {}, actual {}",
            want.haps, got.haps
        ));
    }
    if want.haps_sha256 != got.haps_sha256 {
        return Some(format!(
            "{name}: the {} events changed (same count, different content)\n    \
             expected haps_sha256: {}\n      actual haps_sha256: {}",
            got.haps, want.haps_sha256, got.haps_sha256
        ));
    }
    if want.ctx != got.ctx {
        return Some(format!(
            "{name}: source-location span count changed: expected {}, actual {}",
            want.ctx, got.ctx
        ));
    }
    if (want.dur, want.frames) != (got.dur, got.frames) {
        return Some(format!(
            "{name}: rendered length changed: expected {}s/{} frames, actual {}s/{} frames",
            want.dur, want.frames, got.dur, got.frames
        ));
    }
    if want.audio_sha256 == got.audio_sha256 {
        return None;
    }
    let peak = relative_delta(f64::from(want.peak), f64::from(got.peak));
    let rms = relative_delta(f64::from(want.rms), f64::from(got.rms));
    let detail = format!(
        "peak expected {:.9}, actual {:.9} (relative delta {peak:.6}); \
         rms expected {:.9}, actual {:.9} (relative delta {rms:.6}); \
         audible frames expected {:?}, actual {:?}",
        want.peak, got.peak, want.rms, got.rms, want.audible, got.audible
    );
    let audible_moved = match (want.audible, got.audible) {
        (None, None) => false,
        (Some((wf, wl)), Some((gf, gl))) => {
            wf.abs_diff(gf) > AUDIBLE_FRAME_TOLERANCE || wl.abs_diff(gl) > AUDIBLE_FRAME_TOLERANCE
        }
        // Sound where there was none, or none where there was sound.
        _ => true,
    };
    if exact || peak > METRIC_TOLERANCE || rms > METRIC_TOLERANCE || audible_moved {
        return Some(format!(
            "{name}: audio changed\n    expected audio_sha256: {}\n      actual audio_sha256: {}\n    {detail}",
            want.audio_sha256, got.audio_sha256
        ));
    }
    None
}

/// Write what this engine produced for each set that disagreed, beside the
/// golden it disagreed with. Returns the paths, for the failure message.
fn write_proposed(changed: &BTreeMap<&str, Vec<&Measured>>) -> Vec<String> {
    changed
        .iter()
        .map(|(set, rows)| {
            let path = e2e_dir().join(format!("golden/{set}.jsonl.actual"));
            let mut text = String::new();
            for row in rows {
                text.push_str(&serde_json::to_string(row).expect("serialise"));
                text.push('\n');
            }
            std::fs::write(&path, text).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
            format!("golden/{set}.jsonl.actual")
        })
        .collect()
}

/// Explain where the candidate measurements are and how to compare them.
/// `git diff --no-index` works even though `.actual` files are intentionally
/// ignored, and accepts these forward-slash paths on every supported host.
fn proposed_help(proposed: &[String]) -> String {
    let actual_paths: Vec<_> = proposed
        .iter()
        .map(|path| format!("{E2E_REPOSITORY_PATH}/{path}"))
        .collect();
    let diff_commands: Vec<_> = proposed
        .iter()
        .map(|actual| {
            let expected = actual
                .strip_suffix(".actual")
                .expect("candidate golden must end in .actual");
            format!(
                "git diff --no-index -- {E2E_REPOSITORY_PATH}/{expected} \
                 {E2E_REPOSITORY_PATH}/{actual}"
            )
        })
        .collect();
    format!(
        "What this engine produced:\n  {}\n\n\
         Review the committed expectation -> actual result with:\n  {}\n\n\
         A changed hash proves that behaviour moved; it does not prove the new \
         behaviour is correct. If the change is intentional, replace the \
         matching .jsonl with its reviewed .jsonl.actual and commit that diff.",
        actual_paths.join("\n  "),
        diff_commands.join("\n  ")
    )
}

/// How many files the sample cache holds. A run that had to fetch banks spent
/// its time on the network, and timing that against a warm baseline reports a
/// download as a regression.
fn sample_cache_entries() -> usize {
    fn count(dir: &Path) -> usize {
        std::fs::read_dir(dir).map_or(0, |entries| {
            entries
                .flatten()
                .map(|entry| match entry.file_type() {
                    Ok(kind) if kind.is_dir() => count(&entry.path()),
                    Ok(kind) if kind.is_file() => 1,
                    _ => 0,
                })
                .sum()
        })
    }
    let cache = score_sample_cache_dir();
    count(cache.parent().unwrap_or(&cache))
}

// ------------------------------------------------------------------- tests

#[test]
fn nested_callback_corpus_events_match_in_every_build_profile() {
    // Five corpus scores with deep graphs. The frames beneath the query
    // boundary spend the budget QuickJS measures a callback against, and
    // this checks that an unoptimised build does not lose their events. The
    // test compares events only, so it needs no sample download and no
    // rendered audio, and it can run in an unoptimised profile.
    let witnesses = [
        "corpus/songs/inspire-piano",
        "corpus/generated/seed057",
        "corpus/songs/pumpupthejam",
        "corpus/songs/short-UAVGnDf_1EV1",
        "corpus/songs/swimandsleep",
    ];
    let golden = read_golden("corpus");
    let cases = cases();
    // Match the CLI/studio's native query worker stack, not the test runner's.
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            for name in witnesses {
                let case = cases.iter().find(|case| case.name == name).expect(name);
                let expected = golden.get(name).expect(name);
                let mut session = Session::new().expect("query session");
                session.set_direct_diagnostic_logging(false);
                session.evaluate(&case.source).expect(name);
                let haps = session
                    .query(Fraction::ZERO, Fraction::new(QUERY_CYCLES, 1))
                    .expect(name);
                let mut hasher = Sha256::new();
                for hap in &haps {
                    hasher.update(hap.show().as_bytes());
                    hasher.update(b"\n");
                }
                assert_eq!(haps.len(), expected.haps, "{name}: event count");
                assert_eq!(
                    hex(hasher.finalize()),
                    expected.haps_sha256,
                    "{name}: event content"
                );
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn every_score_matches_the_golden() {
    let acceleration = acceleration_preference();
    let dispatch = acceleration.dispatch();
    // The goldens come from an optimised build, so the audio comparison is a
    // release gate. Events do not depend on the profile, and the test above
    // holds the corpus scores that once proved otherwise to their counts and
    // hashes in both.
    if cfg!(debug_assertions) {
        eprintln!("e2e: not run - run `cargo test --release -p rustel-runtime --test e2e`.");
        return;
    }

    let cases = cases();
    let cached_before = sample_cache_entries();
    eprintln!("e2e acceleration: {}", acceleration.code());
    let started = Instant::now();
    let outcomes = measure_all(&cases, dispatch);
    let elapsed = started.elapsed();
    let fetched = sample_cache_entries().saturating_sub(cached_before);

    let captured_on: Environment = read_json("environment.json");
    let here = Environment {
        arch: std::env::consts::ARCH.to_owned(),
        os: std::env::consts::OS.to_owned(),
        sample_rate: session_config(dispatch).sample_rate,
        channels: session_config(dispatch).channels,
        query_cycles: QUERY_CYCLES,
    };
    assert_eq!(
        (
            captured_on.sample_rate,
            captured_on.channels,
            captured_on.query_cycles
        ),
        (here.sample_rate, here.channels, here.query_cycles),
        "the goldens were captured under a different measurement setup"
    );
    // Bit-identical audio is a property of one machine: glibc selects its libm
    // implementations per CPU, so two x86_64 Linux hosts can differ in the last
    // bits of `sin`/`exp`/`powf`. `AUDIO_HASH_GRID` absorbs that for almost all
    // scores, not all. The hash gate is therefore opt-in, for the machine that
    // captures goldens. Elsewhere the comparison is the metric one, and a hash
    // that moves below tolerance is counted and printed, not failed. The event
    // stream, its hash, the span count, the duration and the frame count are
    // still compared exactly.
    let exact = std::env::var("RUSTEL_E2E_EXACT").is_ok_and(|value| value == "1");

    let goldens: BTreeMap<&str, BTreeMap<String, Measured>> = ["corpus", "docs", "probes"]
        .into_iter()
        .map(|set| (set, read_golden(set)))
        .collect();

    let mut sound = Vec::new();
    let mut unavailable = Vec::new();
    let mut broken = Vec::new();
    let mut ungoverned = Vec::new();
    let mut below_tolerance = 0usize;
    let mut changed_sets: BTreeMap<&str, Vec<&Measured>> = BTreeMap::new();

    for (case, outcome) in cases.iter().zip(&outcomes) {
        match outcome {
            Outcome::Measured(got, _) => {
                let Some(want) = goldens.get(case.set()).and_then(|set| set.get(&case.name)) else {
                    ungoverned.push(case.name.clone());
                    // Produce a complete candidate golden for the set, just as
                    // for an ordinary mismatch. This is the maintainer path
                    // for intentionally adding a case.
                    changed_sets.entry(case.set()).or_default();
                    continue;
                };
                if let Some(message) = compare(&case.name, want, got, exact) {
                    sound.push(message);
                    changed_sets.entry(case.set()).or_default();
                } else if want.audio_sha256 != got.audio_sha256 {
                    below_tolerance += 1;
                }
            }
            Outcome::SamplesUnavailable(why) => unavailable.push(format!("{}: {why}", case.name)),
            Outcome::Failed(why) => broken.push(format!("{}: {why}", case.name)),
        }
    }

    // Every measurement for a set that moved, so what this engine produces can
    // be reviewed against what is committed and, if the change was the point,
    // taken as the new golden.
    for (case, outcome) in cases.iter().zip(&outcomes) {
        if let Outcome::Measured(got, _) = outcome
            && let Some(rows) = changed_sets.get_mut(case.set())
        {
            rows.push(got);
        }
    }
    // Write candidates before any assertion below can stop the test. In
    // particular, a newly added score has no golden yet by definition.
    let proposed = write_proposed(&changed_sets);
    let proposed_help = proposed_help(&proposed);

    let orphaned: Vec<&String> = goldens
        .values()
        .flat_map(|set| set.keys())
        .filter(|name| !cases.iter().any(|case| &case.name == *name))
        .collect();

    eprintln!(
        "e2e: {} scores in {:.1}s ({:.1} ms/score wall); audio compared {}",
        cases.len(),
        elapsed.as_secs_f64(),
        elapsed.as_secs_f64() * 1000.0 / cases.len() as f64,
        if exact {
            // Not bit-exactly, and the wording used to claim it was. Samples
            // are hashed on a grid so that two hosts whose libm disagrees in
            // the last bits still agree; see `AUDIO_HASH_GRID`.
            "exactly, on the quantised audio grid (RUSTEL_E2E_EXACT=1)".to_owned()
        } else {
            format!(
                "by metric tolerance against goldens captured on {}-{}; this is {}-{}. \
                 RUSTEL_E2E_EXACT=1 gates on the audio hash instead",
                captured_on.arch, captured_on.os, here.arch, here.os
            )
        },
    );

    // Printed rather than failed: a last-bit host difference is not a
    // regression, and burying it would hide the day it stops being last-bit.
    if below_tolerance > 0 {
        eprintln!(
            "note: {below_tolerance} score(s) rendered a different audio_sha256 while peak, RMS \
             and audible frames stayed within tolerance - host arithmetic, not a sound change."
        );
    }

    assert!(
        unavailable.is_empty(),
        "{} case(s) could not load their samples - this is NOT a sound \
         difference. Run once with network access to warm the cache, or set \
         RUSTEL_SAMPLE_CACHE to a warm directory.\n  {}",
        unavailable.len(),
        unavailable.join("\n  ")
    );
    assert!(
        broken.is_empty(),
        "{} case(s) failed to run:\n  {}",
        broken.len(),
        broken.join("\n  ")
    );
    assert!(
        ungoverned.is_empty(),
        "{} score(s) have no committed expected result:\n  {}\n\n{}",
        ungoverned.len(),
        ungoverned.join("\n  "),
        proposed_help
    );
    assert!(
        orphaned.is_empty(),
        "{} committed golden row(s) have no score to measure:\n  {}\n\n\
         Restore each score, or intentionally remove its orphaned golden row.",
        orphaned.len(),
        orphaned
            .iter()
            .map(|name| name.as_str())
            .collect::<Vec<_>>()
            .join("\n  ")
    );
    assert!(
        sound.is_empty(),
        "{} score(s) changed:\n  {}\n\n{}",
        sound.len(),
        sound.join("\n  "),
        proposed_help
    );

    if acceleration == AccelerationPreference::Portable {
        eprintln!(
            "e2e speed: not compared - portable kernels do not use the Auto timing baseline."
        );
        return;
    }
    let baseline: Timing = read_json("timing.json");
    let total: f64 = outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            Outcome::Measured(_, took) => Some(took.as_secs_f64()),
            _ => None,
        })
        .sum();
    let ratio = total / baseline.total_secs.max(f64::MIN_POSITIVE);
    eprintln!(
        "e2e speed: {total:.1}s of work against a {:.1}s baseline ({ratio:.2}x)",
        baseline.total_secs
    );
    if std::env::var(SPEED_GATE_ENV).as_deref() != Ok("1") {
        return;
    }
    if fetched > 0 {
        eprintln!("e2e speed: not checked - this run downloaded {fetched} sample file(s).");
        return;
    }
    // Last, because this is the one measurement that depends on what else the
    // machine is doing.
    assert!(
        ratio <= SPEED_REGRESSION_LIMIT,
        "the corpus took {ratio:.2}x its baseline ({total:.1}s against {:.1}s). \
         If the machine was busy, re-run.",
        baseline.total_secs
    );
}

#[test]
fn a_changed_score_is_caught() {
    let dispatch = acceleration_preference().dispatch();
    let case = Case {
        name: "negative-control".into(),
        source: "/*\n  @duration 2\n*/\ns(\"bd sd hh*4\").note(\"c e g\")".into(),
        duration: 2.0,
    };
    let Outcome::Measured(reference, _) = measure(&case, dispatch) else {
        panic!("the control case must render");
    };
    let perturbed = Case {
        source: case.source.replace("c e g", "c e a"),
        ..case
    };
    let Outcome::Measured(changed, _) = measure(&perturbed, dispatch) else {
        panic!("the perturbed case must render");
    };
    let complaint = compare("negative-control", &reference, &changed, true)
        .expect("changing a note must be reported as a difference");
    assert!(
        complaint.contains("events changed"),
        "expected an event difference, got: {complaint}"
    );
    assert!(
        complaint.contains(&format!("expected haps_sha256: {}", reference.haps_sha256)),
        "missing expected hash: {complaint}"
    );
    assert!(
        complaint.contains(&format!("actual haps_sha256: {}", changed.haps_sha256)),
        "missing actual hash: {complaint}"
    );

    let mut changed_audio = (*reference).clone();
    changed_audio.audio_sha256 = "changed-audio-hash".into();
    let complaint = compare("negative-control", &reference, &changed_audio, true)
        .expect("changing PCM must be reported on the capture platform");
    assert!(
        complaint.contains(&format!(
            "expected audio_sha256: {}",
            reference.audio_sha256
        )),
        "missing expected audio hash: {complaint}"
    );
    assert!(
        complaint.contains("actual audio_sha256: changed-audio-hash"),
        "missing actual audio hash: {complaint}"
    );
    assert!(
        complaint.contains("peak expected"),
        "missing metric labels: {complaint}"
    );
    assert!(
        complaint.contains("audible frames expected"),
        "missing audible-range labels: {complaint}"
    );
}

#[test]
fn corpus_acceleration_defaults_to_auto_and_rejects_invalid_values() {
    assert_eq!(
        parse_acceleration(Err(std::env::VarError::NotPresent)),
        Ok(AccelerationPreference::Auto)
    );
    assert_eq!(
        parse_acceleration(Ok("auto".into())),
        Ok(AccelerationPreference::Auto)
    );
    assert_eq!(
        parse_acceleration(Ok("portable".into())),
        Ok(AccelerationPreference::Portable)
    );
    for value in ["", "Portable", "scalar", "avx2", " auto"] {
        let error = parse_acceleration(Ok(value.into())).expect_err("invalid selection");
        assert!(error.contains(ACCELERATION_ENV));
    }
    let error = parse_acceleration(Err(std::env::VarError::NotUnicode(
        std::ffi::OsString::new(),
    )))
    .expect_err("non-Unicode selection");
    assert!(error.contains(ACCELERATION_ENV));
    assert!(error.contains("must be Unicode"));
}

#[test]
fn corpus_session_config_retains_the_selected_dispatch() {
    for acceleration in [
        AccelerationPreference::Auto,
        AccelerationPreference::Portable,
    ] {
        let config = session_config(acceleration.dispatch());
        assert_eq!(
            config.dsp_dispatch.is_forced_portable(),
            acceleration == AccelerationPreference::Portable
        );
    }
}

#[test]
fn golden_help_names_both_files_and_a_copy_pasteable_diff() {
    let help = proposed_help(&["golden/corpus.jsonl.actual".into()]);
    assert!(help.contains("crates/runtime/tests/e2e/golden/corpus.jsonl.actual"));
    assert!(help.contains(
        "git diff --no-index -- crates/runtime/tests/e2e/golden/corpus.jsonl \
         crates/runtime/tests/e2e/golden/corpus.jsonl.actual"
    ));
    assert!(help.contains("does not prove the new behaviour is correct"));
}
