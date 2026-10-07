//! Shared fixtures for the studio end-to-end suite: hermetic studios,
//! golden comparison, and screen helpers.
//!
//! Every test starts from [`hermetic`], which pins everything that would
//! otherwise drift between machines: theme, keyboard capabilities, graphics
//! tier, terminal identity, directory layout, and the set folder's content.
//! What a test then asserts is what the studio itself did, not where it ran.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_runtime::session_log::{SaveStatus, SessionMode, SessionRecorder};

use rustel_studio::app::harness::{HarnessOptions, Studio};
use rustel_studio::editor::KeyboardCapabilities;

/// Re-exported so test files can name focus targets without reaching into
/// the runtime's module tree.
pub use rustel_studio::view::PanelKind;

/// See [`global_state`]: the mutex behind it, created on first use.
static GLOBAL_STATE: OnceLock<Mutex<()>> = OnceLock::new();

/// Hold for the duration of a test that drives a studio: the process-global
/// state a studio touches (the graphics tier, the cell-pixel size, the theme
/// directory environment) belongs to one test at a time. The suite is
/// correct under `--test-threads=1` (what CI uses) and under the default
/// thread pool; this lock is what makes that true rather than lucky.
pub fn global_state() -> MutexGuard<'static, ()> {
    let mutex = GLOBAL_STATE.get_or_init(|| Mutex::new(()));
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A hermetic studio over a fresh temporary set, plus the guard that keeps
/// the temporary set and the process-global state alive for the test.
///
/// The studio opens exactly as `rustel studio` would over that folder:
/// starter score `first.strudel` with `$: s("bd")`, no audio device, the log
/// inside the set's own config folder, an in-memory clipboard, the
/// `rustel-dark` theme and `legacy()` keyboard capabilities. The terminal
/// identity claims nothing special, so the graphics tier is cells on every
/// machine, including one with kitty graphics available.
///
/// No test outcome depends on the network. The pinned default manifests load
/// in the background as they do in the product (a cache miss fetches, a
/// failure stays in the failure list), but every bank a score names resolves
/// from local registration.
///
/// One deliberate difference from a fresh folder: the fixture adds a local
/// `bd` bank (a generated WAV under the set directory) and registers it
/// before the first settle. The studio warms a clean score's readiness when
/// its first lint lands, which starts a fetch for each sound the score
/// names. Without a local bank every hermetic studio would use the network,
/// and the header would read `◐ loading 0/1` for as long as the runner's
/// link takes. With the local bank the header reads `✓ ready` once the
/// fixture's readiness wait returns.
pub struct Hermetic {
    pub studio: Studio,
    set_directory_path: std::path::PathBuf,
    config_home_path: std::path::PathBuf,
    // Fields drop in order: the studio closes before the cache variable is
    // put back, and the variable is back before the guard lets another
    // test in.
    sample_cache: Option<OwnSampleCache>,
    _guard: MutexGuard<'static, ()>,
    _home: tempfile::TempDir,
}

impl Hermetic {
    /// The screen the studio paints, as rows joined with newlines.
    pub fn screen(&mut self) -> String {
        self.studio.screen()
    }

    /// The screen's rows, for region assertions that slice rather than
    /// re-read the whole frame.
    pub fn rows(&mut self) -> Vec<String> {
        self.studio.render()
    }

    /// The folder of the set open *now*, read live from the studio - for
    /// tests that write scores, prebakes or tapes the way the product
    /// would find them, and for assertions after File ▸ New set or an
    /// open, when the studio is in a different folder than the fixture was
    /// born over. (The fixture's own folder stays on disk for the whole
    /// test either way.)
    pub fn set_directory(&self) -> std::path::PathBuf {
        self.studio.set_directory()
    }

    /// The folder the studio reads its tapes from: the directory the set
    /// panel's sessions fold (Ctrl+B) lists and a session tape is written
    /// into. Tests write their tapes here rather than guessing at the env
    /// fallback chain.
    pub fn tapes_directory(&self) -> std::path::PathBuf {
        self.studio.sessions_directory()
    }

    /// The dedicated folder recording takes land in. Session tapes stay in
    /// [Self::tapes_directory]; these paths deliberately differ.
    pub fn recordings_directory(&self) -> std::path::PathBuf {
        self.studio.recordings_directory()
    }

    /// The config home the studio reads prefs and user themes from - where
    /// `studio.json` lands when a test flips something the studio keeps.
    pub fn config_home(&self) -> &Path {
        &self.config_home_path
    }

    // -- tapes --------------------------------------------------------------

    /// Write a real tape into [`Self::tapes_directory`], the way the
    /// recorder writes one - its header, one installed save per
    /// `(seconds, source)`, and the log line it always ends on - and return
    /// its path. `timestamp` is the file name's, `2026-09-05T12-09-48`: the
    /// set panel lists tapes newest first by it.
    pub fn write_tape<S: AsRef<str>>(
        &self,
        timestamp: &str,
        saves: impl IntoIterator<Item = (f64, S)>,
    ) -> PathBuf {
        let directory = self.tapes_directory();
        std::fs::create_dir_all(&directory).expect("tape directory");
        let path = directory.join(format!(
            "session-{timestamp}{}",
            rustel_runtime::product::SESSION_FILE_SUFFIX
        ));
        let mut recorder =
            SessionRecorder::create(path.clone(), SessionMode::Normal, None).expect("tape opens");
        for (at, source) in saves {
            recorder.record_save(at, SaveStatus::Installed, source.as_ref(), None);
        }
        drop(recorder);
        path
    }

    /// Open the set panel, open its sessions fold, and put the selection
    /// on the newest tape. The hermetic set has one score, so the panel's
    /// lines are that score, the fold, then the tapes, newest first.
    pub fn select_newest_tape(&mut self) {
        self.studio.chord("ctrl+b");
        assert_eq!(
            self.studio.focus(),
            Some(PanelKind::Set),
            "the set panel has the keys"
        );
        self.studio.press(KeyCode::End, KeyModifiers::NONE);
        self.studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
        self.studio.press(KeyCode::Down, KeyModifiers::NONE);
    }

    /// [`Self::select_newest_tape`], then Enter: the newest tape opens as
    /// the replay view, on its first block.
    pub fn open_newest_tape(&mut self) {
        self.select_newest_tape();
        self.studio.press(KeyCode::Enter, KeyModifiers::NONE);
        assert!(
            self.studio.status().starts_with("replay - "),
            "the newest tape opened as a replay: {}",
            self.studio.status()
        );
    }

    // -- waiting on the loop ------------------------------------------------

    /// Turn the loop until `done` holds, panicking with `what` and the
    /// studio's own words once `budget` has passed. The loop answers when
    /// it answers - the engine, the recorder, the file walks all run on
    /// threads of their own - so every wait is a deadline, never a count
    /// of turns whose length depends on the machine.
    pub fn pump_until(
        &mut self,
        budget: Duration,
        what: &str,
        mut done: impl FnMut(&mut Self) -> bool,
    ) {
        let deadline = Instant::now() + budget;
        loop {
            self.studio.pump();
            if done(self) {
                return;
            }
            if Instant::now() >= deadline {
                panic!(
                    "{what}, within {budget:?} - status {:?}, error {:?}",
                    self.studio.status(),
                    self.studio.errors()
                );
            }
            std::thread::sleep(POLL);
        }
    }

    /// Press until the engine's command queue takes the request. The queue
    /// is two commands deep, and on a loaded machine the loop's own traffic
    /// can hold both slots for a beat; the studio then says so and changes
    /// nothing else, and pressing again is what it asks for.
    pub fn press_until_taken(&mut self, mut press: impl FnMut(&mut Self)) {
        const BUSY: &str = "engine busy - press again";
        let deadline = Instant::now() + ENGINE_QUEUE_BUDGET;
        loop {
            press(self);
            if self.studio.status() != BUSY {
                return;
            }
            if Instant::now() >= deadline {
                panic!("the engine never took the request: {BUSY}");
            }
            self.studio.pump();
            std::thread::sleep(POLL);
        }
    }

    /// Turn the loop until the header's `● REC` chip has landed: the start
    /// reply and the first recording snapshot travel through the worker
    /// like everything else, so the chip is a beat behind the status that
    /// asked for it.
    pub fn wait_for_rec_chip(&mut self) {
        self.pump_until(
            REC_CHIP_BUDGET,
            "the header grew the ● REC chip",
            |studio| studio.rows().iter().any(|row| row.contains("● REC")),
        );
    }

    /// Turn the loop until the take has closed: the status names the saved
    /// take or its failure - anything but the recording and closing states.
    pub fn wait_for_take_close(&mut self) {
        self.pump_until(TAKE_CLOSE_BUDGET, "the take closed", |studio| {
            let status = studio.status();
            !status.contains("recording -") && status != "closing take…"
        });
    }

    /// The WAV files in the recordings folder, in name order: every take
    /// and sample a test has left there. Empty when the folder does not
    /// exist yet.
    pub fn recording_wavs(&self) -> Vec<PathBuf> {
        let directory = self.recordings_directory();
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(error) => panic!("{} reads: {error}", directory.display()),
        };
        let mut wavs: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
            })
            .collect();
        wavs.sort();
        wavs
    }
}

/// How often a wait turns the loop while it waits.
pub const POLL: Duration = Duration::from_millis(10);

/// How long [`Hermetic::press_until_taken`] keeps pressing.
pub const ENGINE_QUEUE_BUDGET: Duration = Duration::from_secs(2);

/// How long a take's start may take to reach the header.
pub const REC_CHIP_BUDGET: Duration = Duration::from_secs(5);

/// How long a take may take to close: the writer is joined and the file
/// judged for silence, which a loaded runner can stretch.
pub const TAKE_CLOSE_BUDGET: Duration = Duration::from_secs(30);

impl std::ops::Deref for Hermetic {
    type Target = Studio;

    fn deref(&self) -> &Self::Target {
        &self.studio
    }
}

impl std::ops::DerefMut for Hermetic {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.studio
    }
}

/// Open a hermetic studio at the standard 150×40.
pub fn hermetic() -> Hermetic {
    hermetic_sized(150, 40)
}

/// A hermetic studio with an explicit keyboard capability set - the keymap
/// suite's knob. Everything else is pinned exactly as [`hermetic`] pins it.
pub fn hermetic_with(capabilities: KeyboardCapabilities) -> Hermetic {
    let mut fixture = hermetic_sized_inner(150, 40, capabilities);
    fixture.studio.settle();
    fixture
}

/// A hermetic studio whose library lists one more local bank, `testkick`:
/// a generated WAV under the set folder, registered the way a host trusts
/// a local folder - no network anywhere. (Every hermetic studio already
/// carries a local `bd` bank; this adds a second, distinct name for the
/// samples browser's tests to find and to count.)
pub fn hermetic_with_local_bank() -> Hermetic {
    let mut fixture = hermetic();
    let bank = fixture
        .set_directory()
        .join("samples-root")
        .join("testkick");
    std::fs::create_dir_all(&bank).expect("bank folder");
    std::fs::write(bank.join("hit.wav"), wav_bytes()).expect("sample fixture");
    fixture
        .register_local_samples("samples-root")
        .expect("local bank registers");
    fixture
}

/// A fresh hermetic studio over the same set an existing fixture holds -
/// the manifest round-trip. The given fixture's studio is closed (its
/// worker joined), but its temporary home - the set directory with it -
/// stays alive, now owned by the fixture this returns. The suite's global
/// lock is held throughout, so no other test can open a studio in between.
pub fn reopen_over(closed: Hermetic) -> Hermetic {
    let Hermetic {
        studio,
        config_home_path: _,
        set_directory_path,
        sample_cache,
        _guard,
        _home,
    } = closed;
    drop(studio);
    let mut reopened = hermetic_over(
        set_directory_path,
        _home,
        _guard,
        150,
        40,
        rustel_studio::editor::KeyboardCapabilities::legacy(),
    );
    reopened.sample_cache = sample_cache;
    reopened
}

/// A minimal but real WAV file: one second of silence at 8 kHz, 16-bit
/// mono - enough for the library to list, load and play the bank. Public
/// for the suites that lay their own audio on disk (a folder to drop, a
/// source to import) rather than going through a fixture.
pub fn wav_bytes() -> Vec<u8> {
    let sample_rate: u32 = 8000;
    let seconds: u32 = 1;
    let samples = sample_rate * seconds;
    let data_len = samples * 2;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&((36 + data_len).to_le_bytes()));
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    bytes.extend_from_slice(&2u16.to_le_bytes()); // block align
    bytes.extend_from_slice(&16u16.to_le_bytes()); // bits
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.extend_from_slice(&vec![0u8; data_len as usize]);
    bytes
}

/// Open a hermetic studio at an explicit size - the layout suite's 80×24
/// case, and anything else that needs a different grid.
pub fn hermetic_sized(width: u16, height: u16) -> Hermetic {
    hermetic_sized_inner(width, height, KeyboardCapabilities::legacy())
}

fn hermetic_sized_inner(
    width: u16,
    height: u16,
    capabilities: rustel_studio::editor::KeyboardCapabilities,
) -> Hermetic {
    hermetic_sized_inner_flags(width, height, capabilities, Opening::default())
}

/// A hermetic studio that opens already writing a session tape: the first
/// evaluate lands in it, and the sessions panel (F4) marks it as being
/// written. Everything else is pinned exactly as [`hermetic`] pins it, and
/// the tape lives in the fixture's own session folder - never a runner's.
pub fn hermetic_recording() -> Hermetic {
    hermetic_sized_inner_flags(
        150,
        40,
        KeyboardCapabilities::legacy(),
        Opening {
            recording: true,
            ..Opening::default()
        },
    )
}

/// A hermetic studio whose sample cache is a folder inside its own
/// temporary home, for a test that empties the cache: the folder
/// `RUSTEL_SAMPLE_CACHE` names for the rest of the suite is never touched.
/// Everything else is pinned exactly as [`hermetic`] pins it.
pub fn hermetic_with_own_sample_cache() -> Hermetic {
    hermetic_sized_inner_flags(
        150,
        40,
        KeyboardCapabilities::legacy(),
        Opening {
            own_sample_cache: true,
            ..Opening::default()
        },
    )
}

/// What a fixture opens with beyond the standard pins.
#[derive(Clone, Copy, Default)]
struct Opening {
    /// The studio opens already writing a session tape.
    recording: bool,
    /// The sample cache is a folder inside the fixture's temporary home
    /// rather than the one the environment names.
    own_sample_cache: bool,
}

/// `RUSTEL_SAMPLE_CACHE` pointed at a fixture's own folder; dropping it
/// puts the previous value back.
struct OwnSampleCache {
    previous: Option<std::ffi::OsString>,
}

impl OwnSampleCache {
    /// Point the variable at `folder`. The caller holds the suite's global
    /// mutex.
    fn point_at(folder: &Path) -> Self {
        std::fs::create_dir_all(folder).expect("own sample cache");
        let previous = std::env::var_os(rustel_runtime::product::SAMPLE_CACHE_ENV);
        // SAFETY: as for the fixture's other variables - mutated only while
        // the suite's global mutex is held.
        unsafe { std::env::set_var(rustel_runtime::product::SAMPLE_CACHE_ENV, folder) };
        Self { previous }
    }
}

impl Drop for OwnSampleCache {
    fn drop(&mut self) {
        // SAFETY: the fixture drops this after its studio and before its
        // guard, so the suite's global mutex is still held.
        unsafe {
            match self.previous.take() {
                Some(previous) => {
                    std::env::set_var(rustel_runtime::product::SAMPLE_CACHE_ENV, previous)
                }
                None => std::env::remove_var(rustel_runtime::product::SAMPLE_CACHE_ENV),
            }
        }
    }
}

/// A hermetic studio with a slider live in the engine and its pill on
/// screen: `$: s("bd").gain(slider(0.8,0,1,0.1))`, typed and played.
pub fn hermetic_with_live_slider() -> Hermetic {
    let mut studio = hermetic();
    studio.chord("ctrl+a");
    studio.type_text("$: s(\"bd\").gain(slider(0.8,0,1,0.1))");
    studio.chord("ctrl+s");
    studio.settle();
    studio
}

fn hermetic_sized_inner_flags(
    width: u16,
    height: u16,
    capabilities: rustel_studio::editor::KeyboardCapabilities,
    opening: Opening,
) -> Hermetic {
    let guard = global_state();
    let home = tempfile::tempdir().expect("temporary home");
    // The header decides between a set's full name and its file name by
    // what fits, and the process counters take what is left - so the
    // set's name length decides the header's layout. A name inherits the
    // length of the temporary home it lives under, which differs between
    // machines (macOS's `/var/folders/...` against Linux's `/tmp`) and
    // between runners (a per-job TMPDIR). The set directory is therefore
    // created with a name padded to a fixed length, not a fixed path:
    // the header's choice has identical inputs everywhere, and a golden
    // frame holds byte-for-byte. The length is the one the first goldens
    // were drawn with, so they stay valid. The directory is removed with
    // the temporary home that contains it.
    const SET_NAME_LENGTH: usize = 47;
    let set_directory_path = home
        .path()
        .join("set".to_string() + &"x".repeat(SET_NAME_LENGTH - "set".len()));
    std::fs::create_dir_all(&set_directory_path).expect("temporary set directory");
    hermetic_over_inner(
        set_directory_path,
        home,
        guard,
        width,
        height,
        capabilities,
        opening,
    )
}

/// Open a hermetic studio over a set directory that already exists. Used by
/// [`reopen_over`]. Everything the standard fixture pins is pinned here too:
/// the environment, the local `bd` bank (created when missing, so a folder
/// that a first fixture prepared is reused), the registration before the
/// first settle, and the readiness wait.
fn hermetic_over(
    set_directory_path: std::path::PathBuf,
    home: tempfile::TempDir,
    guard: MutexGuard<'static, ()>,
    width: u16,
    height: u16,
    capabilities: rustel_studio::editor::KeyboardCapabilities,
) -> Hermetic {
    hermetic_over_inner(
        set_directory_path,
        home,
        guard,
        width,
        height,
        capabilities,
        Opening::default(),
    )
}

fn hermetic_over_inner(
    set_directory_path: std::path::PathBuf,
    home: tempfile::TempDir,
    guard: MutexGuard<'static, ()>,
    width: u16,
    height: u16,
    capabilities: rustel_studio::editor::KeyboardCapabilities,
    opening: Opening,
) -> Hermetic {
    // The local `bd` bank every hermetic studio gets. The starter score
    // names `bd`, and the studio warms a clean score's readiness when its
    // first lint lands. For a bank known only from the default manifests,
    // that warm starts a fetch. A local bank registered before the first
    // lint answers the warm from disk: no network, and the header reads
    // `✓ ready` without a loading state of machine-dependent length. The
    // bank lives inside the set folder, hidden from the set's own file
    // listings.
    let bank = set_directory_path.join(".samples").join("bd");
    std::fs::create_dir_all(&bank).expect("local bd bank");
    std::fs::write(bank.join("hit.wav"), wav_bytes()).expect("bd sample");
    // Prefs, user themes and the platform config folder are read once, at
    // open, from the config home. Point it at the temporary home so a
    // runner's own `studio.json` or theme folder cannot reach a test.
    // `RUSTEL_CONFIG_DIR` is the config resolver's first check on every
    // platform. `XDG_CONFIG_HOME` alone is not enough: the macOS rule is
    // `$HOME/.rustel` and ignores it. The environment is process-global, so
    // every fixture holds the mutex for its whole lifetime.
    let config_home = home.path().join("config-home");
    std::fs::create_dir_all(&config_home).expect("config home");
    // SAFETY: process-global environment, mutated only while `guard` holds
    // the suite's global mutex (and CI runs `--test-threads=1` beneath it).
    // No other thread reads these variables while the lock is held.
    unsafe {
        std::env::set_var("RUSTEL_CONFIG_DIR", &config_home);
        std::env::set_var("XDG_CONFIG_HOME", &config_home);
        // The sessions panel reads its tape folder through the same env
        // fallback chain the CLI does; without this a runner's real
        // `$HOME/.rustel/sessions` would be the honest list, and a test
        // asserting on the panel's rows would hang on whose machine it
        // ran on. Every fixture's tapes are its own.
        std::env::set_var(
            rustel_runtime::product::SESSION_DIRECTORY_ENV,
            home.path().join("sessions"),
        );
        // Reveals are desktop jobs - a browser tab, an Explorer window -
        // and this suite runs on machines whose desktop belongs to whoever
        // is reading the output. The studio itself is told nothing is out
        // there: Alt+O on a sample bank walks `opening …` into `opened …`
        // without a process being started, so the keyboard path a probe
        // exercises is the one under test, and nothing opens on the
        // runner's desktop.
        std::env::set_var(rustel_studio::reveal::HEADLESS_ENV, "1");
        std::env::remove_var("RUSTEL_THEME_DIR");
    }
    let sample_cache = opening
        .own_sample_cache
        .then(|| OwnSampleCache::point_at(&home.path().join("sample-cache")));
    let mut options = HarnessOptions::new(&set_directory_path);
    options.terminal.cell_pixels = None;
    options.capabilities = capabilities;
    if opening.recording {
        // The studio opens already writing a session tape. No explicit
        // path: the tape lands in the set's own sessions folder - the one
        // folder the panel lists and `sessions_directory()` reports - so
        // the tape being written, the panel's list, and a test's own
        // written tapes are all the one directory. The log stays in the
        // session directory the environment pins.
        options.recording = Some(rustel_studio::RecordingOptions {
            mode: rustel_runtime::session_log::SessionMode::Normal,
            file: None,
        });
    }
    let mut studio = Studio::open(options).expect("hermetic studio opens");
    studio.resize(width, height);
    // Before the first settle: the settle is what runs the first lint,
    // the lint is what warms readiness, and the warm must find the local
    // bank rather than reach for the network. Registration itself is a
    // disk walk; it must not sit behind the default manifest fetch the
    // open already queued, or a slow first GitHub round-trip spends the
    // minute budget and this expect dies with "sample manifest deadline
    // exceeded" before the test body runs.
    studio
        .register_local_samples(".samples")
        .expect("local bd bank registers");
    studio.settle();
    studio.wait_for_readiness();
    Hermetic {
        studio,
        config_home_path: config_home,
        set_directory_path,
        sample_cache,
        _guard: guard,
        _home: home,
    }
}

/// A golden comparison: the rendered screen against `goldens/<name>.txt`.
///
/// A mismatch prints a unified diff, writes `<name>.actual` beside the
/// golden (the corpus suite's convention), and fails. `UPDATE_GOLDENS=1`
/// rewrites goldens instead of failing, for the deliberate regeneration
/// a real change earns.
pub fn assert_golden(fixture: &mut Hermetic, name: &str) {
    // Scrub the folder that is open at this moment and the folder the
    // fixture started with. After File ▸ New set or an open they differ,
    // and a status line can still name the earlier folder. An unscrubbed
    // path would put a machine-specific absolute path into a committed
    // golden (on macOS, a `/private/var/...` canonical form).
    let live_directory = fixture.set_directory();
    let screen = fixture.screen();
    let screen = scrub_paths(&screen, &live_directory);
    let screen = scrub_paths(&screen, &fixture.set_directory_path);
    let golden_path = golden_dir().join(format!("{name}.txt"));
    if std::env::var_os("UPDATE_GOLDENS").is_some_and(|value| value == "1") {
        std::fs::create_dir_all(golden_path.parent().expect("golden has a parent"))
            .expect("golden directory");
        std::fs::write(&golden_path, &screen).expect("golden written");
        eprintln!("golden updated: {}", golden_path.display());
        return;
    }
    let golden = match std::fs::read_to_string(&golden_path) {
        Ok(golden) => normalise(&golden),
        // `UPDATE_GOLDENS=1` wrote and returned above. Any other value of
        // the variable tolerates a golden that is not on disk yet: the
        // expectation is empty, the diff below shows the whole screen, and
        // the `.actual` file shows where the golden goes. An existing
        // golden is still compared exactly.
        Err(_) if std::env::var_os("UPDATE_GOLDENS").is_some() => String::new(),
        Err(error) => panic!(
            "golden {} is missing ({}); run UPDATE_GOLDENS=1 cargo test -p rustel-studio-e2e \
             to write it once you have reviewed the screen it should hold",
            golden_path.display(),
            error
        ),
    };
    let actual = normalise(&screen);
    if golden == actual {
        return;
    }
    let actual_path = golden_dir().join(format!("{name}.actual"));
    let _ = std::fs::write(&actual_path, &screen);
    let diff = similar::TextDiff::from_lines(&golden, &actual)
        .unified_diff()
        .context_radius(3)
        .to_string();
    panic!(
        "golden `{name}` does not match (diff saved to {}):\n{}",
        actual_path.display(),
        diff
    );
}

fn normalise(text: &str) -> String {
    // Windows checkouts must not turn a checkout-wide eol setting into a
    // golden failure; `.gitattributes` pins the goldens to LF and this is
    // the belt to its braces.
    text.replace("\r\n", "\n")
}

/// Replaces everything that differs between machines in a rendered frame:
/// the set's absolute path (a fresh temporary directory per run) becomes
/// `<set>`. Everything else on screen is the studio's own doing and is
/// held exactly.
fn scrub_paths(screen: &str, set: &Path) -> String {
    screen.replace(set.to_string_lossy().as_ref(), "<set>")
}

fn golden_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("goldens")
}

/// A captured pty stream, made safe to print.
///
/// The capture is a byte stream, not a screen. On Windows, ConPTY injects a
/// BEL-terminated OSC 0 title sequence when the child spawns. The studio
/// itself sends cursor moves, `ESC[2J`, the alternate screen (`ESC[?1049h`),
/// mouse capture (`ESC[?1003h`) and a hidden cursor (`ESC[?25l`). A panic
/// that prints these bytes verbatim makes the terminal that runs the suite
/// execute them: it beeps, clears the transcript, and stays on the
/// alternate screen with a hidden cursor. `printable` spells each such byte
/// as text (`ESC[2J`, `<BEL>`), so the message stays complete and the
/// terminal executes nothing. Every diagnostic that prints captured pty
/// bytes goes through here.
pub fn printable(raw: &str) -> String {
    let mut safe = String::with_capacity(raw.len());
    let mut characters = raw.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\u{1b}' => match characters.peek() {
                Some('[') => {
                    characters.next();
                    safe.push_str("ESC[");
                    // To the final byte. A CSI's final is any byte in
                    // 0x40..=0x7E - letters, but also `@`, `` ` `` and `~`
                    // (`ESC[6 q` ends on `q` after a space; cursor keys
                    // report as `ESC[1;5A`), and stopping only at letters
                    // would swallow the next sequence into this one's
                    // spelling. Parameter and intermediate bytes before it
                    // are printable and pushed; a stray control byte in a
                    // malformed stream is spelled, never sent.
                    while let Some(&next) = characters.peek() {
                        characters.next();
                        if (0x40..=0x7e).contains(&(next as u8)) {
                            safe.push(next);
                            break;
                        }
                        if next.is_control() {
                            safe.push_str(&format!("\\u{{{:x}}}", next as u32));
                        } else {
                            safe.push(next);
                        }
                    }
                }
                Some(']') => {
                    characters.next();
                    safe.push_str("ESC]");
                    // An OSC's body runs to BEL or ST (`ESC \`). The payload
                    // - a window title, a pointer shape - is printable text
                    // and kept; the terminator is named, never sent.
                    while let Some(&next) = characters.peek() {
                        if next == '\u{7}' {
                            characters.next();
                            safe.push_str("<BEL>");
                            break;
                        }
                        if next == '\u{1b}' {
                            characters.next();
                            if characters.peek() == Some(&'\\') {
                                characters.next();
                                safe.push_str("ESC\\");
                            } else {
                                safe.push_str("ESC");
                            }
                            break;
                        }
                        characters.next();
                        safe.push(next);
                    }
                }
                _ => safe.push_str("ESC"),
            },
            '\u{7}' => safe.push_str("<BEL>"),
            '\u{8}' => safe.push_str("<BS>"),
            '\t' => safe.push_str("<TAB>"),
            '\r' => {
                // A CRLF pair is one line break; a bare CR re-overwrites the
                // line and is spelled so a reader knows the layout moved.
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                safe.push('\n');
            }
            other if other.is_control() => {
                safe.push_str(&format!("\\u{{{:x}}}", other as u32));
            }
            other => safe.push(other),
        }
    }
    safe
}

/// The row of the screen holding a substring, or a panic naming the screen -
/// region assertions start from a row, not a guess.
pub fn row_containing(rows: &[String], needle: &str) -> usize {
    rows.iter()
        .position(|row| row.contains(needle))
        .unwrap_or_else(|| panic!("no row contains {needle:?}:\n{}", rows.join("\n")))
}

/// (column, row) of the first cell drawing `glyph` - a slider pill's knob,
/// say - read off the frame rather than guessed. Columns are cells, so a
/// multi-byte glyph earlier in the row is counted once.
pub fn glyph_column(rows: &[String], glyph: char) -> (u16, u16) {
    let y = rows
        .iter()
        .position(|row| row.contains(glyph))
        .unwrap_or_else(|| panic!("no row draws {glyph:?}:\n{}", rows.join("\n")));
    let byte = rows[y].find(glyph).expect("glyph is in the row named");
    let x = rows[y][..byte].chars().count() as u16;
    (x, y as u16)
}

#[cfg(test)]
mod printable_tests {
    use super::printable;

    /// The safety contract: a diagnostic printed through `printable` gives
    /// the reader's terminal nothing to execute - no BEL to beep, no CSI to
    /// clear or flip screens, no raw control byte of any kind.
    #[test]
    fn nothing_printable_remains_executable() {
        // The exact bytes ConPTY hands the pty suite at spawn, and the
        // controls the studio's own painting leans on.
        let raw = "\u{1b}]0;C:\\path\\rustel.exe\u{7}\u{1b}[2J\u{1b}[?1049h\u{1b}[?25l\
                  \u{1b}[?1003;1006h\u{1b}[6 q\u{1b}]22;hand\u{1b}\\\u{7}\u{8}\t\r\n\u{1b}[H";
        let safe = printable(raw);
        // The one control byte `printable` may leave is the newline it
        // writes for CRLF: formatting, not a command - nothing a terminal
        // executes comes out of a `\n`.
        assert!(
            !safe.chars().any(|c| c.is_control() && c != '\n'),
            "a control byte survived: {safe:?}"
        );
        // The story survives too: the title payload and every sequence are
        // still there, readable.
        assert!(safe.contains("ESC]0;C:\\path\\rustel.exe<BEL>"), "{safe:?}");
        assert!(safe.contains("ESC[2J"), "{safe:?}");
        assert!(safe.contains("ESC[?1049h"), "{safe:?}");
        assert!(safe.contains("ESC]22;handESC\\"), "{safe:?}");
        assert!(safe.contains("<BS>"), "{safe:?}");
        assert!(safe.contains("<TAB>"), "{safe:?}");
        assert_eq!(safe.matches("<BEL>").count(), 2, "{safe:?}");
    }

    /// The escape body consumer must not overrun: a stream whose sequence is
    /// cut off mid-body (a capture always can be) still ends printable.
    #[test]
    fn a_truncated_sequence_still_prints_safe() {
        for fragment in ["\u{1b}[", "\u{1b}[38;2;226", "\u{1b}]0;title", "\u{1b}"] {
            let safe = printable(fragment);
            assert!(
                !safe.chars().any(|c| c.is_control() && c != '\n'),
                "a control byte survived {fragment:?}: {safe:?}"
            );
        }
    }

    /// A diagnostic also has to stay honest: ordinary text passes through
    /// unchanged, so what failed still reads like what failed.
    #[test]
    fn plain_text_is_untouched() {
        let text = "✓ ready  quit again in...";
        assert_eq!(printable(text), text);
    }
}
